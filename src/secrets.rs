//! Synchronous credential operations with platform-native backend selection.

// Existing config tests construct native mock entries directly. Never initialize
// a real credential store through this facade in a test build.
#[cfg(any(test, target_os = "windows", target_os = "macos"))]
pub(crate) use keyring::{Entry, Error};
#[cfg(all(target_os = "linux", not(test)))]
pub(crate) use linux::{Entry, Error};

#[cfg(target_os = "linux")]
mod linux {
    use std::sync::Arc;

    use futures_lite::future::block_on;

    /// Backend payloads are discarded at the boundary: they may contain secrets.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum Error {
        NoEntry,
        AccessDenied,
        Unavailable,
        NotDecodable,
        OperationFailed,
    }

    fn backend_error(error: oo7::Error) -> Error {
        match error {
            oo7::Error::DBus(oo7::dbus::Error::Dismissed)
            | oo7::Error::DBus(oo7::dbus::Error::Service(oo7::dbus::ServiceError::IsLocked(_)))
            | oo7::Error::File(oo7::file::Error::Locked) => Error::AccessDenied,
            oo7::Error::File(oo7::file::Error::Utf8(_)) => Error::NotDecodable,
            _ => Error::OperationFailed,
        }
    }

    pub(crate) struct Entry {
        keyring: Arc<oo7::Keyring>,
        service: String,
        username: String,
    }

    impl Entry {
        #[cfg(not(test))]
        pub(crate) fn new(service: &str, username: &str) -> Result<Self, Error> {
            // Cache only successful initialization so an unavailable service can
            // recover. oo7 alone selects the native or sandbox portal backend.
            static KEYRING: std::sync::Mutex<Option<Arc<oo7::Keyring>>> = std::sync::Mutex::new(None);
            let mut cached = KEYRING.lock().map_err(|_| Error::Unavailable)?;
            if cached.is_none() {
                let keyring = block_on(oo7::Keyring::new()).map_err(|_| Error::Unavailable)?;
                *cached = Some(Arc::new(keyring));
            }
            let keyring = cached.as_ref().ok_or(Error::Unavailable)?;
            Ok(Self::with_keyring(Arc::clone(keyring), service, username))
        }

        fn with_keyring(keyring: Arc<oo7::Keyring>, service: &str, username: &str) -> Self {
            Self {
                keyring,
                service: service.to_owned(),
                username: username.to_owned(),
            }
        }

        fn attributes(&self) -> [(&str, &str); 2] {
            // These are the existing native keyring lookup attributes. No migration
            // or additional application schema is required.
            [("service", &self.service), ("username", &self.username)]
        }

        async fn unlock(&self) -> Result<(), Error> {
            if self.keyring.is_locked().await.map_err(backend_error)? {
                self.keyring.unlock().await.map_err(backend_error)?;
            }
            if self.keyring.is_locked().await.map_err(backend_error)? {
                return Err(Error::AccessDenied);
            }
            Ok(())
        }

        async fn item(&self) -> Result<Option<oo7::Item>, Error> {
            self.unlock().await?;
            let mut items = self.keyring.search_items(&self.attributes()).await.map_err(backend_error)?;
            if items.len() > 1 {
                // Match keyring's ambiguity semantics rather than selecting or
                // deleting an arbitrary credential.
                return Err(Error::OperationFailed);
            }
            let item = items.pop();
            if let Some(item) = &item {
                if item.is_locked().await.map_err(backend_error)? {
                    item.unlock().await.map_err(backend_error)?;
                }
                if item.is_locked().await.map_err(backend_error)? {
                    return Err(Error::AccessDenied);
                }
            }
            Ok(item)
        }

        pub(crate) fn get_password(&self) -> Result<String, Error> {
            block_on(async {
                let item = self.item().await?.ok_or(Error::NoEntry)?;
                let secret = item.secret().await.map_err(backend_error)?;
                std::str::from_utf8(secret.as_bytes())
                    .map(str::to_owned)
                    .map_err(|_| Error::NotDecodable)
            })
        }

        pub(crate) fn set_password(&self, password: &str) -> Result<(), Error> {
            block_on(async {
                match self.item().await? {
                    Some(item) => item.set_secret(password).await.map_err(backend_error),
                    None => self
                        .keyring
                        .create_item("TopQ MQTT password", &self.attributes(), password, true)
                        .await
                        .map_err(backend_error),
                }
            })
        }

        pub(crate) fn delete_credential(&self) -> Result<(), Error> {
            block_on(async {
                let item = self.item().await?.ok_or(Error::NoEntry)?;
                item.delete().await.map_err(backend_error)
            })
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn file_keyring(path: &std::path::Path) -> Arc<oo7::Keyring> {
            // Explicit file backend and a test-only encryption key: no portal,
            // session bus, environment mutation, or user's credential store.
            let file = block_on(oo7::file::UnlockedKeyring::load(path, oo7::Secret::blob([42; 64]))).unwrap();
            Arc::new(oo7::Keyring::File(Arc::new(Some(oo7::file::Keyring::Unlocked(file)).into())))
        }

        #[test]
        fn passwords_are_replaced_persisted_and_deleted() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("secrets.keyring");
            let entry = Entry::with_keyring(file_keyring(&path), "topq", "account");
            assert_eq!(entry.get_password(), Err(Error::NoEntry));
            assert_eq!(entry.delete_credential(), Err(Error::NoEntry));
            entry.set_password("first-secret").unwrap();
            entry.set_password("second-secret-🔒").unwrap();
            assert_eq!(entry.get_password().unwrap(), "second-secret-🔒");
            let bytes = std::fs::read(&path).unwrap();
            assert!(!bytes.windows(b"second-secret".len()).any(|window| window == b"second-secret"));
            let reopened = Entry::with_keyring(file_keyring(&path), "topq", "account");
            assert_eq!(reopened.get_password().unwrap(), "second-secret-🔒");
            reopened.delete_credential().unwrap();
            assert_eq!(reopened.get_password(), Err(Error::NoEntry));
        }

        #[test]
        fn lookup_uses_existing_attributes_and_isolates_accounts_and_services() {
            let dir = tempfile::tempdir().unwrap();
            let keyring = file_keyring(&dir.path().join("secrets.keyring"));
            block_on(keyring.create_item(
                "existing native credential",
                &[("service", "topq"), ("username", "account")],
                "existing",
                true,
            ))
            .unwrap();
            let entry = Entry::with_keyring(Arc::clone(&keyring), "topq", "account");
            assert_eq!(entry.get_password().unwrap(), "existing");
            assert_eq!(
                Entry::with_keyring(Arc::clone(&keyring), "topq", "other").get_password(),
                Err(Error::NoEntry)
            );
            assert_eq!(Entry::with_keyring(keyring, "other", "account").get_password(), Err(Error::NoEntry));
        }

        #[test]
        fn ambiguous_credentials_are_not_read_updated_or_deleted() {
            let dir = tempfile::tempdir().unwrap();
            let keyring = file_keyring(&dir.path().join("secrets.keyring"));
            let attributes = [("service", "topq"), ("username", "account")];
            for password in ["first", "second"] {
                block_on(keyring.create_item("duplicate", &attributes, password, false)).unwrap();
            }
            let entry = Entry::with_keyring(Arc::clone(&keyring), "topq", "account");
            assert_eq!(entry.get_password(), Err(Error::OperationFailed));
            assert_eq!(entry.set_password("replacement"), Err(Error::OperationFailed));
            assert_eq!(entry.delete_credential(), Err(Error::OperationFailed));
            let items = block_on(keyring.search_items(&attributes)).unwrap();
            let passwords: Vec<_> = items.iter().map(|item| block_on(item.secret()).unwrap()).collect();
            assert_eq!(passwords, vec![oo7::Secret::text("first"), oo7::Secret::text("second")]);
        }

        #[test]
        fn invalid_utf8_is_rejected_without_exposing_secret_bytes() {
            let dir = tempfile::tempdir().unwrap();
            let keyring = file_keyring(&dir.path().join("secrets.keyring"));
            block_on(keyring.create_item(
                "invalid",
                &[("service", "topq"), ("username", "account")],
                oo7::Secret::blob(b"secret-\xff"),
                true,
            ))
            .unwrap();
            let entry = Entry::with_keyring(keyring, "topq", "account");
            let error = entry.get_password().unwrap_err();
            assert_eq!(error, Error::NotDecodable);
            assert!(!format!("{error:?}").contains("secret-"));
        }

        #[test]
        fn backend_errors_discard_arbitrary_payloads() {
            assert_eq!(
                backend_error(oo7::Error::File(oo7::file::Error::TargetFileChanged("secret-in-error".into()))),
                Error::OperationFailed
            );
            assert_eq!(
                backend_error(oo7::Error::DBus(oo7::dbus::Error::Service(oo7::dbus::ServiceError::IsLocked(
                    "secret-in-error".into()
                )))),
                Error::AccessDenied
            );
            // Also exercise the initialization failure reason without opening a store.
            assert_eq!(format!("{:?}", Error::Unavailable), "Unavailable");
        }
    }
}
