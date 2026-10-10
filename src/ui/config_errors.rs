//! English fallback copy for typed configuration errors, kept outside persistence logic.

use crate::{
    error::config::{ConfigDiagnostic, ConfigError, ConnectionUriCode, ConnectionValidationCode, ConnectionValidationError},
    ui::messages::LocalizedError,
};

impl LocalizedError for ConnectionValidationCode {
    fn localized_message(&self) -> String {
        match self {
            Self::MissingHost => "Enter a broker hostname or IP address.",
            Self::HostContainsSchemeOrPath => "Enter a hostname or IP address without a URL scheme or path.",
            Self::InvalidPort => "Enter a port between 1 and 65535.",
            Self::InvalidClientId => "Enter a non-empty MQTT client ID up to 65535 bytes, without null characters.",
            Self::InvalidTopicFilter => {
                "Enter a valid MQTT topic filter (e.g. # or home/#), up to 65535 bytes. Use + for one level and # only as the last level."
            }

            Self::MissingTopics => "Add at least one topic subscription.",
            Self::DuplicateTopicFilter => "Each topic filter may only be subscribed to once.",
            Self::PasswordWithoutUsername => "Enter a username when using a password.",
        }
        .into()
    }
}

impl LocalizedError for ConnectionValidationError {
    fn localized_message(&self) -> String {
        self.code().localized_message()
    }
}

impl LocalizedError for ConnectionUriCode {
    fn localized_message(&self) -> String {
        match self {
            Self::FormatFailed => "Could not format the broker connection URI.",
            Self::UsernameEncodingFailed => "Could not encode the MQTT username in the broker connection URI.",
            Self::Invalid => "The saved broker connection URI is invalid.",
            Self::InvalidComponents => "The broker connection URI must contain only a scheme, host, and optional username and port. Passwords, paths, queries, and fragments are not allowed.",
            Self::UnsupportedScheme => "The broker connection URI scheme must be mqtt, mqtts, ws, or wss.",
            Self::MissingHost => "The broker connection URI must include a host.",
            Self::UsernameNotUtf8 => "The broker connection URI username is not valid UTF-8.",
        }.into()
    }
}

impl LocalizedError for ConfigError {
    fn localized_message(&self) -> String {
        match self {
            Self::ConfigDirectoryUnavailable => "Could not locate your application configuration directory.",
            Self::HomeDirectoryUnavailable => "Could not locate your home directory.",
            Self::InvalidConfigPath { .. } => "Invalid configuration path.",
            Self::ReadConfig { .. } => "Could not read the configuration file.",
            Self::ParseConfig { .. } => "Could not parse the configuration file.",
            Self::ParseConnections { .. } => "Could not parse the saved connections.",
            Self::SerializeConfig { .. } => "Could not serialize the configuration settings.",
            Self::ConfigObjectRequired => "Configuration settings must be JSON objects.",
            Self::InspectConfig { .. } => "Could not inspect the configuration file.",
            Self::ConfigNotWritable { .. } => "The application configuration cannot be written. Check your directory permissions and try again.",
            Self::WriteApplicationConfig { .. } => "Could not save the application configuration. Check your directory permissions and try again.",

            Self::CreateConfigDirectory { .. } => "Could not create the configuration directory.",
            Self::OpenTemporaryConfig { .. } => "Could not open the temporary settings file.",
            Self::WriteConfig { .. } => "Could not write the settings file.",
            Self::FlushConfig { .. } => "Could not flush the settings file to disk.",
            Self::SaveConfig { .. } => "Could not save the settings file.",
            Self::Validation(error) => return error.localized_message(),
            Self::SelectedConnectionOutOfRange { .. } => "Selected connection index is out of range.",
            Self::DuplicateConnectionAccount => "Duplicate MQTT connection account; each broker and username must be unique.",
            Self::FormatConnectionUri { .. } => return ConnectionUriCode::FormatFailed.localized_message(),
            Self::EncodeConnectionUsername => return ConnectionUriCode::UsernameEncodingFailed.localized_message(),
            Self::InvalidConnectionUri { .. } => return ConnectionUriCode::Invalid.localized_message(),
            Self::ConnectionUriComponents => return ConnectionUriCode::InvalidComponents.localized_message(),
            Self::ConnectionUriScheme => return ConnectionUriCode::UnsupportedScheme.localized_message(),
            Self::ConnectionUriHostRequired => return ConnectionUriCode::MissingHost.localized_message(),
            Self::ConnectionUriUsernameNotUtf8 { .. } => return ConnectionUriCode::UsernameNotUtf8.localized_message(),
            Self::OpenCredentialStore { .. } => "Could not open the system credential store.",
            Self::RetrievePassword { .. } => "Could not retrieve the MQTT password. Unlock your system credential store or enter the password again in Connection settings.",
            Self::InspectPassword { .. } => "Could not inspect a saved MQTT password.",
            Self::UpdatePassword { .. } => "Could not update an MQTT password in the system credential store.",
            Self::CredentialMissing => "The saved MQTT password was not found.",
            Self::CredentialAccessDenied => "The system credential store is locked or access was denied.",
            Self::CredentialStoreUnavailable => "The system credential store is unavailable.",
            Self::CredentialNotDecodable => "The stored MQTT password could not be decoded.",
            Self::CredentialOperationFailed => "The system credential store could not complete the operation.",
            Self::RollbackCredentials { .. } => "Could not restore all previous MQTT passwords.",
            Self::SaveRollbackFailed { .. } => "Could not restore all previous MQTT passwords; check the system credential store.",
        }
        .into()
    }
}

impl LocalizedError for ConfigDiagnostic {
    fn localized_message(&self) -> String {
        match self {
            Self::Json { category, line, column } => match category {
                serde_json::error::Category::Io => format!("Could not process JSON at line {line}, column {column}."),
                serde_json::error::Category::Syntax => format!("Invalid JSON syntax at line {line}, column {column}."),
                serde_json::error::Category::Data => format!("Invalid JSON value at line {line}, column {column}."),
                serde_json::error::Category::Eof => format!("Incomplete JSON at line {line}, column {column}."),
            },
            Self::ConnectionUri { code, .. } => code.localized_message(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_errors_do_not_implement_localized_error() {
        trait AmbiguousIfLocalized<A> {
            fn assert_not_localized() {}
        }
        impl<T: ?Sized> AmbiguousIfLocalized<()> for T {}
        struct IsLocalized;
        impl<T: ?Sized + LocalizedError> AmbiguousIfLocalized<IsLocalized> for T {}

        // Type inference becomes ambiguous if either log-only type gains localization.
        let _ = <crate::error::config::LayoutError as AmbiguousIfLocalized<_>>::assert_not_localized;
        let _ = <crate::error::config::LayoutDiagnostic as AmbiguousIfLocalized<_>>::assert_not_localized;
    }

    #[test]
    fn uri_diagnostic_fallback_preserves_the_specific_reason() {
        let diagnostic = ConfigDiagnostic::ConnectionUri {
            code: ConnectionUriCode::UnsupportedScheme,
            line: 0,
            column: 0,
        };
        assert_eq!(diagnostic.to_string(), "config.connection_uri.scheme_invalid");
        assert_eq!(diagnostic.localized_message(), ConfigError::ConnectionUriScheme.localized_message());
    }

    #[test]
    fn validation_fallback_is_separate_from_machine_code() {
        let config = crate::config::ConnectionConfig {
            host: String::new(),
            ..Default::default()
        };
        let error = config.validate().unwrap_err();
        assert_eq!(error.to_string(), "config.validation.host_required");
        assert_eq!(error.localized_message(), "Enter a broker hostname or IP address.");
        let error = ConfigError::from(error);
        assert_eq!(error.localized_message(), "Enter a broker hostname or IP address.");
    }

    #[test]
    fn persistence_fallback_does_not_display_paths_or_sources() {
        let error = ConfigError::ReadConfig {
            path: "/private/secret-path".into(),
            source: std::io::Error::other("secret-source"),
        };
        assert_eq!(error.localized_message(), "Could not read the configuration file.");
        assert_eq!(error.to_string(), "config.read_failed");
    }

    #[test]
    fn migration_fallbacks_do_not_display_paths_or_sources() {
        for (error, code, message) in [
            (
                ConfigError::InspectConfig {
                    path: "/private/secret-path".into(),
                    source: std::io::Error::other("secret-source"),
                },
                "config.inspect_failed",
                "Could not inspect the configuration file.",
            ),
            (
                ConfigError::ConfigNotWritable {
                    path: "/private/secret-path".into(),
                    alternative_path: Some("/private/fallback-path".into()),
                },
                "config.not_writable",
                "The application configuration cannot be written. Check your directory permissions and try again.",
            ),
            (
                ConfigError::WriteApplicationConfig {
                    path: "/private/secret-path".into(),
                    alternative_path: Some("/private/fallback-path".into()),
                    source: Box::new(ConfigError::WriteConfig {
                        path: "/private/temporary-path".into(),
                        source: std::io::Error::other("secret-source"),
                    }),
                },
                "config.application.write_failed",
                "Could not save the application configuration. Check your directory permissions and try again.",
            ),
        ] {
            assert_eq!(error.to_string(), code);
            assert_eq!(error.localized_message(), message);
        }
    }

    #[test]
    fn credential_context_has_a_complete_fallback_without_concatenating_sources() {
        let error = ConfigError::RetrievePassword {
            source: Box::new(ConfigError::CredentialAccessDenied),
        };
        assert_eq!(
            error.localized_message(),
            "Could not retrieve the MQTT password. Unlock your system credential store or enter the password again in Connection settings."
        );
        assert_eq!(error.to_string(), "config.credentials.retrieve_failed");
    }
}
