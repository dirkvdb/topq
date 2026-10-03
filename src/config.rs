//! Connection settings on disk and MQTT passwords in the system credential store.

use std::{
    fmt, fs,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow};
use directories::ProjectDirs;
use keyring::{Entry, Error as KeyringError};
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct ConnectionConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    #[serde(skip)]
    pub password: String,
    pub tls: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConnectionField {
    Host,
    Port,
    Username,
}

#[derive(Debug)]
pub(crate) struct ConnectionValidationError {
    field: ConnectionField,
    message: &'static str,
}

impl ConnectionValidationError {
    pub(crate) fn field(&self) -> ConnectionField {
        self.field
    }
}

impl fmt::Display for ConnectionValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message)
    }
}

impl std::error::Error for ConnectionValidationError {}

impl Default for ConnectionConfig {
    fn default() -> Self {
        Self {
            host: "localhost".into(),
            port: 1883,
            username: String::new(),
            password: String::new(),
            tls: false,
        }
    }
}

impl ConnectionConfig {
    pub fn validate(&self) -> Result<(), ConnectionValidationError> {
        if self.host.trim().is_empty() {
            return Err(ConnectionValidationError {
                field: ConnectionField::Host,
                message: "Enter a broker hostname or IP address.",
            });
        }
        if self.host.contains("://") || self.host.contains('/') {
            return Err(ConnectionValidationError {
                field: ConnectionField::Host,
                message: "Enter a hostname or IP address without a URL scheme or path.",
            });
        }
        if self.port == 0 {
            return Err(ConnectionValidationError {
                field: ConnectionField::Port,
                message: "Enter a port between 1 and 65535.",
            });
        }
        if self.username.is_empty() && !self.password.is_empty() {
            return Err(ConnectionValidationError {
                field: ConnectionField::Username,
                message: "Enter a username when using a password.",
            });
        }
        Ok(())
    }
}

pub fn config_path() -> Result<PathBuf> {
    let dirs = ProjectDirs::from("", "", "mqtt-ui")
        .context("Could not locate your application configuration directory.")?;
    Ok(dirs.config_dir().join("connection.json"))
}

pub fn load() -> Result<Option<ConnectionConfig>> {
    load_from(&config_path()?, &password_entry)
}

pub fn save(config: &ConnectionConfig) -> Result<()> {
    save_to(&config_path()?, config, &password_entry)
}

#[derive(Serialize, Deserialize)]
struct SavedConnection {
    #[serde(flatten)]
    config: ConnectionConfig,
    #[serde(default)]
    password_in_keyring: bool,
}

fn read_connection(path: &Path) -> Result<Option<SavedConnection>> {
    let data = match fs::read(path) {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("Could not read the saved connection."),
    };
    let saved: SavedConnection =
        serde_json::from_slice(&data).context("Could not parse the saved connection.")?;
    Ok(Some(saved))
}

fn load_from(
    path: &Path,
    entry: &impl Fn(&ConnectionConfig) -> Result<Entry>,
) -> Result<Option<ConnectionConfig>> {
    let Some(saved) = read_connection(path)? else {
        return Ok(None);
    };
    let mut config = saved.config;
    if saved.password_in_keyring {
        config.password = entry(&config)?.get_password()
            .map_err(credential_error)
            .context("Could not retrieve the MQTT password. Unlock your system credential store or enter the password again in Connection settings.")?;
    }
    config.validate()?;
    Ok(Some(config))
}

fn save_to(
    path: &Path,
    config: &ConnectionConfig,
    entry: &impl Fn(&ConnectionConfig) -> Result<Entry>,
) -> Result<()> {
    config.validate()?;
    if !config.password.is_empty() {
        entry(config)?.set_password(&config.password)
            .map_err(credential_error)
            .context("Could not store the MQTT password. Unlock your system credential store and try again.")?;
    } else if read_connection(path)?.is_some_and(|saved| {
        saved.password_in_keyring && credential_account(&saved.config) == credential_account(config)
    }) {
        match entry(config)?.delete_credential() {
            Ok(()) | Err(KeyringError::NoEntry) => {}
            Err(error) => {
                return Err(credential_error(error))
                    .context("Could not remove the saved MQTT password.");
            }
        }
    }
    // Store the secret first: a credential-store failure must not replace the
    // last working connection or fall back to writing a password to disk.
    write_json(
        path,
        &SavedConnection {
            config: config.clone(),
            password_in_keyring: !config.password.is_empty(),
        },
    )
}

fn credential_account(config: &ConnectionConfig) -> String {
    // Length prefixes keep host and username delimiters from colliding.
    format!(
        "{}:{}:{}:{}:{}:{}",
        config.host.len(),
        config.host,
        config.port,
        config.tls,
        config.username.len(),
        config.username
    )
}

fn password_entry(config: &ConnectionConfig) -> Result<Entry> {
    Entry::new("mqtt-ui", &credential_account(config))
        .map_err(credential_error)
        .context("Could not open the system credential store.")
}

fn credential_error(error: KeyringError) -> anyhow::Error {
    // Keyring errors may carry secret bytes. Do not propagate their Debug
    // representation or sources to the UI or logs.
    anyhow!(match error {
        KeyringError::NoEntry => "The saved MQTT password was not found.",
        KeyringError::NoStorageAccess(_) =>
            "The system credential store is locked or access was denied.",
        KeyringError::NoDefaultStore => "The system credential store is unavailable.",
        KeyringError::BadEncoding(_) | KeyringError::BadDataFormat(_, _) =>
            "The stored MQTT password could not be decoded.",
        _ => "The system credential store could not complete the operation.",
    })
}

#[derive(Default, Serialize, Deserialize)]
struct LayoutPreferences {
    topics_width_rem: Option<f32>,
}

pub(crate) fn load_topics_width() -> Result<Option<f32>> {
    let path = config_path()?.with_file_name("layout.json");
    let data = match fs::read(path) {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("Could not read the pane layout."),
    };
    let preferences: LayoutPreferences =
        serde_json::from_slice(&data).context("Could not parse the pane layout.")?;
    Ok(preferences
        .topics_width_rem
        .filter(|width| width.is_finite() && *width > 0.))
}

pub(crate) fn save_topics_width(width: f32) -> Result<()> {
    let path = config_path()?.with_file_name("layout.json");
    write_json(
        &path,
        &LayoutPreferences {
            topics_width_rem: Some(width),
        },
    )
}

pub(crate) fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    fs::create_dir_all(path.parent().context("Invalid configuration path.")?)
        .context("Could not create the configuration directory.")?;
    let temp = path.with_extension("json.tmp");
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temp)
        .context("Could not open the temporary settings file.")?;
    file.write_all(&serde_json::to_vec_pretty(value)?)
        .context("Could not write the settings file.")?;
    file.sync_all()
        .context("Could not flush the settings file to disk.")?;
    fs::rename(&temp, path).context("Could not save the settings file.")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use keyring_core::{api::CredentialStoreApi, mock};

    use super::*;

    fn mock_entries() -> impl Fn(&ConnectionConfig) -> Result<Entry> {
        let store = mock::Store::new().unwrap();
        move |config| {
            Ok(Entry {
                inner: store.build("mqtt-ui", &credential_account(config), None)?,
            })
        }
    }

    fn authenticated_config() -> ConnectionConfig {
        ConnectionConfig {
            username: "mqtt-user".into(),
            password: "test-mqtt-secret".into(),
            ..Default::default()
        }
    }

    #[test]
    fn rejects_invalid_connection_fields() {
        for (host, port) in [("", 1883), ("mqtt://localhost", 1883), ("localhost", 0)] {
            let config = ConnectionConfig {
                host: host.into(),
                port,
                ..Default::default()
            };
            assert!(config.validate().is_err());
        }
    }

    #[test]
    fn validate_accepts_an_ipv6_broker_address() {
        let config = ConnectionConfig {
            host: "::1".into(),
            ..Default::default()
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn saving_keeps_the_password_only_in_the_credential_store() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connection.json");
        let entries = mock_entries();
        let config = authenticated_config();

        save_to(&path, &config, &entries).unwrap();

        let bytes = fs::read(&path).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(json.get("password").is_none());
        assert!(
            !std::str::from_utf8(&bytes)
                .unwrap()
                .contains(&config.password)
        );
        assert_eq!(json["password_in_keyring"], true);
        assert_eq!(
            entries(&config).unwrap().get_password().unwrap(),
            config.password
        );
        let restored = load_from(&path, &entries).unwrap().unwrap();
        assert_eq!(
            (
                restored.host,
                restored.port,
                restored.username,
                restored.password
            ),
            (config.host, config.port, config.username, config.password)
        );
        assert!(!path.with_extension("json.tmp").exists());
    }

    #[test]
    fn serialization_never_includes_the_runtime_password() {
        let json = serde_json::to_value(authenticated_config()).unwrap();
        assert!(json.get("password").is_none());
    }

    #[test]
    fn loading_does_not_import_or_rewrite_a_legacy_plaintext_password() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connection.json");
        let original = br#"{"host":"localhost","port":1883,"username":"mqtt-user","password":"legacy-secret"}"#;
        fs::write(&path, original).unwrap();
        let config = load_from(&path, &|_| {
            panic!("legacy passwords must not access the credential store")
        })
        .unwrap()
        .unwrap();
        assert!(config.password.is_empty());
        assert_eq!(fs::read(path).unwrap(), original);
    }

    #[test]
    fn passwordless_connections_do_not_access_the_credential_store() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connection.json");
        let entries = |_: &ConnectionConfig| -> Result<Entry> {
            panic!("passwordless connections must not access the credential store")
        };
        let config = ConnectionConfig {
            username: "mqtt-user".into(),
            ..Default::default()
        };
        save_to(&path, &config, &entries).unwrap();
        assert!(
            load_from(&path, &entries)
                .unwrap()
                .unwrap()
                .password
                .is_empty()
        );
    }

    #[test]
    fn clearing_a_password_deletes_its_credential() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connection.json");
        let entries = mock_entries();
        let mut config = authenticated_config();
        save_to(&path, &config, &entries).unwrap();
        config.password.clear();
        save_to(&path, &config, &entries).unwrap();
        assert!(matches!(
            entries(&config).unwrap().get_password(),
            Err(KeyringError::NoEntry)
        ));
        assert!(
            load_from(&path, &entries)
                .unwrap()
                .unwrap()
                .password
                .is_empty()
        );
    }

    #[test]
    fn a_credential_write_failure_preserves_the_previous_connection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connection.json");
        let entries = mock_entries();
        let config = authenticated_config();
        save_to(&path, &config, &entries).unwrap();
        let before = fs::read(&path).unwrap();
        let entry = entries(&config).unwrap();
        entry
            .inner
            .as_any()
            .downcast_ref::<mock::Cred>()
            .unwrap()
            .set_error(KeyringError::NoStorageAccess(
                std::io::Error::other("locked").into(),
            ));
        let changed = ConnectionConfig {
            password: "replacement-secret".into(),
            ..config.clone()
        };
        assert!(save_to(&path, &changed, &entries).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(
            load_from(&path, &entries).unwrap().unwrap().password,
            config.password
        );
    }

    #[test]
    fn missing_saved_credentials_are_reported_instead_of_loading_an_empty_password() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connection.json");
        let entries = mock_entries();
        let config = authenticated_config();
        save_to(&path, &config, &entries).unwrap();
        entries(&config).unwrap().delete_credential().unwrap();
        let error = load_from(&path, &entries).err().unwrap();
        assert!(format!("{error:#}").contains("saved MQTT password was not found"));
    }

    #[test]
    fn credentials_are_scoped_to_the_broker_and_username() {
        let config = authenticated_config();
        let key = credential_account(&config);
        for changed in [
            ConnectionConfig {
                host: "other-broker".into(),
                ..config.clone()
            },
            ConnectionConfig {
                port: 8883,
                ..config.clone()
            },
            ConnectionConfig {
                tls: true,
                ..config.clone()
            },
            ConnectionConfig {
                username: "other-user".into(),
                ..config
            },
        ] {
            assert_ne!(credential_account(&changed), key);
        }
    }

    #[test]
    fn credential_errors_do_not_expose_secret_bytes() {
        let secret = b"secret-in-error".to_vec();
        for error in [
            KeyringError::BadEncoding(secret.clone()),
            KeyringError::BadDataFormat(secret, std::io::Error::other("secret-in-error").into()),
        ] {
            let error = credential_error(error);
            assert!(!format!("{error:#?}").contains("secret-in-error"));
        }
    }
}
