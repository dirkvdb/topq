//! Configuration validation, persistence failures, and sanitized diagnostics.

use std::{
    fmt,
    path::{Path, PathBuf},
};

use crate::{config::ConnectionField, secrets::Error as KeyringError};

/// Stable validation reasons; their Display values are translation keys, never UI copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionValidationCode {
    MissingHost,
    HostContainsSchemeOrPath,
    InvalidPort,
    InvalidClientId,
    InvalidTopicFilter,
    MissingTopics,
    DuplicateTopicFilter,
    PasswordWithoutUsername,
}

impl ConnectionValidationCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MissingHost => "config.validation.host_required",
            Self::HostContainsSchemeOrPath => "config.validation.host_scheme_or_path",
            Self::InvalidPort => "config.validation.port_invalid",
            Self::InvalidClientId => "config.validation.client_id_invalid",
            Self::InvalidTopicFilter => "config.validation.topic_filter_invalid",
            Self::MissingTopics => "config.validation.topics_required",
            Self::DuplicateTopicFilter => "config.validation.topic_filter_duplicate",
            Self::PasswordWithoutUsername => "config.validation.username_required",
        }
    }
}

impl fmt::Display for ConnectionValidationCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConnectionValidationError {
    field: ConnectionField,
    code: ConnectionValidationCode,
}

impl ConnectionValidationError {
    pub(crate) fn new(field: ConnectionField, code: ConnectionValidationCode) -> Self {
        Self { field, code }
    }

    /// Returns the translation-independent reason validation failed.
    pub fn code(&self) -> ConnectionValidationCode {
        self.code
    }

    pub(crate) fn field(&self) -> ConnectionField {
        self.field
    }
}

impl fmt::Display for ConnectionValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.code.fmt(f)
    }
}

impl std::error::Error for ConnectionValidationError {}

/// Stable URI failures that can survive Serde's string-only custom-error boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionUriCode {
    FormatFailed,
    UsernameEncodingFailed,
    Invalid,
    InvalidComponents,
    UnsupportedScheme,
    MissingHost,
    UsernameNotUtf8,
}

impl ConnectionUriCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FormatFailed => "config.connection_uri.format_failed",
            Self::UsernameEncodingFailed => "config.connection_uri.username_encode_failed",
            Self::Invalid => "config.connection_uri.invalid",
            Self::InvalidComponents => "config.connection_uri.components_invalid",
            Self::UnsupportedScheme => "config.connection_uri.scheme_invalid",
            Self::MissingHost => "config.connection_uri.host_required",
            Self::UsernameNotUtf8 => "config.connection_uri.username_utf8_invalid",
        }
    }

    fn from_serde_message(message: &str) -> Option<Self> {
        [
            Self::FormatFailed,
            Self::UsernameEncodingFailed,
            Self::Invalid,
            Self::InvalidComponents,
            Self::UnsupportedScheme,
            Self::MissingHost,
            Self::UsernameNotUtf8,
        ]
        .into_iter()
        .find(|code| code.as_str() == message)
    }
}

impl fmt::Display for ConnectionUriCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Parser diagnostics without input values, snippets, or arbitrary serializer messages.
/// Raw Serde errors can echo plaintext credentials, including ignored fields.
#[derive(Clone)]
pub enum ConfigDiagnostic {
    Json {
        category: serde_json::error::Category,
        line: usize,
        column: usize,
    },
    ConnectionUri {
        code: ConnectionUriCode,
        line: usize,
        column: usize,
    },
}

impl From<serde_json::Error> for ConfigDiagnostic {
    fn from(error: serde_json::Error) -> Self {
        let line = error.line();
        let column = error.column();
        if error.classify() == serde_json::error::Category::Data {
            // Serde erases custom error types. Recover only our exact allowlisted codes,
            // optionally followed by Serde's location suffix; never retain its raw message.
            let message = error.to_string();
            let suffix = format!(" at line {line} column {column}");
            let message = message.strip_suffix(&suffix).unwrap_or(&message);
            if let Some(code) = ConnectionUriCode::from_serde_message(message) {
                return Self::ConnectionUri { code, line, column };
            }
        }
        Self::Json {
            category: error.classify(),
            line,
            column,
        }
    }
}

impl fmt::Debug for ConfigDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json { category, line, column } => f
                .debug_struct("Json")
                .field("category", category)
                .field("line", line)
                .field("column", column)
                .finish(),
            Self::ConnectionUri { code, line, column } => f
                .debug_struct("ConnectionUri")
                .field("code", code)
                .field("line", line)
                .field("column", column)
                .finish(),
        }
    }
}

impl fmt::Display for ConfigDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Json { category, .. } => match category {
                serde_json::error::Category::Io => "config.diagnostic.json_io",
                serde_json::error::Category::Syntax => "config.diagnostic.json_syntax",
                serde_json::error::Category::Data => "config.diagnostic.json_data",
                serde_json::error::Category::Eof => "config.diagnostic.json_eof",
            },
            Self::ConnectionUri { code, .. } => code.as_str(),
        })
    }
}

impl std::error::Error for ConfigDiagnostic {}

/// Typed persistence failures. Display is code-only; presentation belongs to the UI.
/// Paths and safe sources retain diagnostic context, but credentials and raw config
/// contents are never stored in errors.
pub enum ConfigError {
    ConfigDirectoryUnavailable,
    HomeDirectoryUnavailable,
    InvalidConfigPath {
        path: PathBuf,
    },
    ReadConfig {
        path: PathBuf,
        source: std::io::Error,
    },
    ParseConfig {
        path: PathBuf,
        source: ConfigDiagnostic,
    },
    ParseConnections {
        path: PathBuf,
        source: ConfigDiagnostic,
    },
    SerializeConfig {
        path: PathBuf,
        source: ConfigDiagnostic,
    },
    ConfigObjectRequired,
    InspectConfig {
        path: PathBuf,
        source: std::io::Error,
    },
    ConfigNotWritable {
        path: PathBuf,
        alternative_path: Option<PathBuf>,
    },
    WriteApplicationConfig {
        path: PathBuf,
        alternative_path: Option<PathBuf>,
        source: Box<ConfigError>,
    },
    CreateConfigDirectory {
        path: PathBuf,
        source: std::io::Error,
    },
    OpenTemporaryConfig {
        path: PathBuf,
        source: std::io::Error,
    },
    WriteConfig {
        path: PathBuf,
        source: std::io::Error,
    },
    FlushConfig {
        path: PathBuf,
        source: std::io::Error,
    },
    SaveConfig {
        path: PathBuf,
        temporary_path: PathBuf,
        source: std::io::Error,
    },
    Validation(ConnectionValidationError),
    SelectedConnectionOutOfRange {
        selected: usize,
        connection_count: usize,
    },
    DuplicateConnectionAccount,
    FormatConnectionUri {
        source: url::ParseError,
    },
    EncodeConnectionUsername,
    InvalidConnectionUri {
        source: url::ParseError,
    },
    ConnectionUriComponents,
    ConnectionUriScheme,
    ConnectionUriHostRequired,
    ConnectionUriUsernameNotUtf8 {
        source: std::str::Utf8Error,
    },
    OpenCredentialStore {
        source: Box<ConfigError>,
    },
    RetrievePassword {
        source: Box<ConfigError>,
    },
    InspectPassword {
        source: Box<ConfigError>,
    },
    UpdatePassword {
        source: Box<ConfigError>,
    },
    CredentialMissing,
    CredentialAccessDenied,
    CredentialStoreUnavailable,
    CredentialNotDecodable,
    CredentialOperationFailed,
    RollbackCredentials {
        failures: Vec<ConfigError>,
    },
    SaveRollbackFailed {
        source: Box<ConfigError>,
        rollback: Box<ConfigError>,
    },
}

impl ConfigError {
    /// The affected filesystem path, or the original failure's path after a failed rollback.
    /// For directory and temporary-file operations this is the directory or temporary file.
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::InvalidConfigPath { path }
            | Self::ReadConfig { path, .. }
            | Self::ParseConfig { path, .. }
            | Self::ParseConnections { path, .. }
            | Self::SerializeConfig { path, .. }
            | Self::InspectConfig { path, .. }
            | Self::ConfigNotWritable { path, .. }
            | Self::WriteApplicationConfig { path, .. }
            | Self::CreateConfigDirectory { path, .. }
            | Self::OpenTemporaryConfig { path, .. }
            | Self::WriteConfig { path, .. }
            | Self::FlushConfig { path, .. }
            | Self::SaveConfig { path, .. } => Some(path),
            Self::SaveRollbackFailed { source, .. } => source.path(),
            _ => None,
        }
    }

    /// The temporary file involved in an open, write, flush, or rename failure.
    pub fn temporary_path(&self) -> Option<&Path> {
        match self {
            Self::SaveConfig { temporary_path, .. } => Some(temporary_path),
            Self::OpenTemporaryConfig { path, .. } | Self::WriteConfig { path, .. } | Self::FlushConfig { path, .. } => Some(path),
            Self::WriteApplicationConfig { source, .. } | Self::SaveRollbackFailed { source, .. } => source.temporary_path(),
            _ => None,
        }
    }

    /// The other configuration destination considered when writing failed.
    pub fn alternative_path(&self) -> Option<&Path> {
        match self {
            Self::ConfigNotWritable { alternative_path, .. } | Self::WriteApplicationConfig { alternative_path, .. } => {
                alternative_path.as_deref()
            }
            Self::SaveRollbackFailed { source, .. } => source.alternative_path(),

            _ => None,
        }
    }

    /// The rejected zero-based selection and the number of saved connections.
    pub fn invalid_selection(&self) -> Option<(usize, usize)> {
        match self {
            Self::SelectedConnectionOutOfRange {
                selected,
                connection_count,
            } => Some((*selected, *connection_count)),
            _ => None,
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::ConfigDirectoryUnavailable => "config.directory_unavailable",
            Self::HomeDirectoryUnavailable => "config.home_directory_unavailable",
            Self::InvalidConfigPath { .. } => "config.path_invalid",
            Self::ReadConfig { .. } => "config.read_failed",
            Self::ParseConfig { .. } => "config.parse_failed",
            Self::ParseConnections { .. } => "config.connections.parse_failed",
            Self::SerializeConfig { .. } => "config.serialize_failed",
            Self::ConfigObjectRequired => "config.json_object_required",
            Self::InspectConfig { .. } => "config.inspect_failed",
            Self::ConfigNotWritable { .. } => "config.not_writable",
            Self::WriteApplicationConfig { .. } => "config.application.write_failed",

            Self::CreateConfigDirectory { .. } => "config.directory_create_failed",
            Self::OpenTemporaryConfig { .. } => "config.temporary_open_failed",
            Self::WriteConfig { .. } => "config.write_failed",
            Self::FlushConfig { .. } => "config.flush_failed",
            Self::SaveConfig { .. } => "config.save_failed",
            Self::Validation(error) => error.code().as_str(),
            Self::SelectedConnectionOutOfRange { .. } => "config.connections.selection_out_of_range",
            Self::DuplicateConnectionAccount => "config.connections.account_duplicate",
            Self::FormatConnectionUri { .. } => ConnectionUriCode::FormatFailed.as_str(),
            Self::EncodeConnectionUsername => ConnectionUriCode::UsernameEncodingFailed.as_str(),
            Self::InvalidConnectionUri { .. } => ConnectionUriCode::Invalid.as_str(),
            Self::ConnectionUriComponents => ConnectionUriCode::InvalidComponents.as_str(),
            Self::ConnectionUriScheme => ConnectionUriCode::UnsupportedScheme.as_str(),
            Self::ConnectionUriHostRequired => ConnectionUriCode::MissingHost.as_str(),
            Self::ConnectionUriUsernameNotUtf8 { .. } => ConnectionUriCode::UsernameNotUtf8.as_str(),
            Self::OpenCredentialStore { .. } => "config.credentials.open_failed",
            Self::RetrievePassword { .. } => "config.credentials.retrieve_failed",
            Self::InspectPassword { .. } => "config.credentials.inspect_failed",
            Self::UpdatePassword { .. } => "config.credentials.update_failed",
            Self::CredentialMissing => "config.credentials.missing",
            Self::CredentialAccessDenied => "config.credentials.access_denied",
            Self::CredentialStoreUnavailable => "config.credentials.unavailable",
            Self::CredentialNotDecodable => "config.credentials.decode_failed",
            Self::CredentialOperationFailed => "config.credentials.operation_failed",
            Self::RollbackCredentials { .. } => "config.credentials.rollback_failed",
            Self::SaveRollbackFailed { .. } => "config.save_rollback_failed",
        }
    }
}

// Debug is the diagnostic surface: unlike Display/localization it deliberately
// includes filesystem context, alternative destinations, selection bounds, safe sources, and every rollback failure.
impl fmt::Debug for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use std::error::Error as _;

        let mut debug = f.debug_struct("ConfigError");
        debug.field("code", &self.code());
        if let Some(path) = self.path() {
            debug.field("path", &path);
        }
        if let Some(path) = self.temporary_path() {
            debug.field("temporary_path", &path);
        }
        if let Some(path) = self.alternative_path() {
            debug.field("alternative_path", &path);
        }
        if let Some((selected, connection_count)) = self.invalid_selection() {
            debug.field("selected", &selected).field("connection_count", &connection_count);
        }
        if let Self::RollbackCredentials { failures } = self {
            debug.field("failures", failures);
        } else if let Some(source) = self.source() {
            debug.field("source", &source);
        }
        if let Self::SaveRollbackFailed { rollback, .. } = self {
            debug.field("rollback", rollback);
        }
        debug.finish()
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ReadConfig { source, .. }
            | Self::InspectConfig { source, .. }
            | Self::CreateConfigDirectory { source, .. }
            | Self::OpenTemporaryConfig { source, .. }
            | Self::WriteConfig { source, .. }
            | Self::FlushConfig { source, .. }
            | Self::SaveConfig { source, .. } => Some(source),
            Self::ParseConfig { source, .. } | Self::ParseConnections { source, .. } | Self::SerializeConfig { source, .. } => Some(source),
            Self::ConnectionUriUsernameNotUtf8 { source } => Some(source),
            Self::Validation(error) => Some(error),
            Self::FormatConnectionUri { source } | Self::InvalidConnectionUri { source } => Some(source),
            Self::OpenCredentialStore { source }
            | Self::RetrievePassword { source }
            | Self::InspectPassword { source }
            | Self::UpdatePassword { source }
            | Self::WriteApplicationConfig { source, .. }
            | Self::SaveRollbackFailed { source, .. } => Some(source.as_ref()),
            Self::RollbackCredentials { failures } => failures.first().map(|error| error as _),
            _ => None,
        }
    }
}

impl From<ConnectionValidationError> for ConfigError {
    fn from(error: ConnectionValidationError) -> Self {
        Self::Validation(error)
    }
}

#[cfg(any(test, target_os = "windows", target_os = "macos"))]
pub(crate) fn credential_error(error: KeyringError) -> ConfigError {
    // Keyring payloads and their sources can contain secrets. Retain only the reason.
    match error {
        KeyringError::NoEntry => ConfigError::CredentialMissing,
        KeyringError::NoStorageAccess(_) => ConfigError::CredentialAccessDenied,
        KeyringError::NoDefaultStore => ConfigError::CredentialStoreUnavailable,
        KeyringError::BadEncoding(_) | KeyringError::BadDataFormat(_, _) => ConfigError::CredentialNotDecodable,
        _ => ConfigError::CredentialOperationFailed,
    }
}

#[cfg(all(target_os = "linux", not(test)))]
pub(crate) fn credential_error(error: KeyringError) -> ConfigError {
    match error {
        KeyringError::NoEntry => ConfigError::CredentialMissing,
        KeyringError::AccessDenied => ConfigError::CredentialAccessDenied,
        KeyringError::Unavailable => ConfigError::CredentialStoreUnavailable,
        KeyringError::NotDecodable => ConfigError::CredentialNotDecodable,
        KeyringError::OperationFailed => ConfigError::CredentialOperationFailed,
    }
}

/// Sanitized TOML diagnostics for log-only layout persistence failures.
#[derive(Clone)]
pub(crate) enum LayoutDiagnostic {
    Toml { span: Option<std::ops::Range<usize>> },
    TomlSerialization,
}

impl fmt::Debug for LayoutDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Toml { span } => f.debug_struct("Toml").field("span", span).finish(),
            Self::TomlSerialization => f.write_str("TomlSerialization"),
        }
    }
}

impl From<toml::de::Error> for LayoutDiagnostic {
    fn from(error: toml::de::Error) -> Self {
        Self::Toml { span: error.span() }
    }
}

impl fmt::Display for LayoutDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Toml { .. } => "config.diagnostic.toml_parse",
            Self::TomlSerialization => "config.diagnostic.toml_serialize",
        })
    }
}

impl std::error::Error for LayoutDiagnostic {}

/// Noncritical layout failures, reported only through logs, never localized UI messages.
pub(crate) enum LayoutError {
    HomeDirectoryUnavailable,
    InvalidStatePath {
        path: PathBuf,
    },
    ReadState {
        path: PathBuf,
        source: std::io::Error,
    },
    StateNotUtf8 {
        path: PathBuf,
        source: std::str::Utf8Error,
    },
    ParseState {
        path: PathBuf,
        source: LayoutDiagnostic,
    },
    SerializeState {
        path: PathBuf,
        source: LayoutDiagnostic,
    },
    CreateStateDirectory {
        path: PathBuf,
        source: std::io::Error,
    },
    OpenTemporaryState {
        path: PathBuf,
        source: std::io::Error,
    },
    WriteState {
        path: PathBuf,
        source: std::io::Error,
    },
    FlushState {
        path: PathBuf,
        source: std::io::Error,
    },
    SaveState {
        path: PathBuf,
        temporary_path: PathBuf,
        source: std::io::Error,
    },
}

impl LayoutError {
    /// The affected state file, directory, or temporary file.
    pub(crate) fn path(&self) -> Option<&Path> {
        match self {
            Self::HomeDirectoryUnavailable => None,
            Self::InvalidStatePath { path }
            | Self::ReadState { path, .. }
            | Self::StateNotUtf8 { path, .. }
            | Self::ParseState { path, .. }
            | Self::SerializeState { path, .. }
            | Self::CreateStateDirectory { path, .. }
            | Self::OpenTemporaryState { path, .. }
            | Self::WriteState { path, .. }
            | Self::FlushState { path, .. }
            | Self::SaveState { path, .. } => Some(path),
        }
    }

    pub(crate) fn temporary_path(&self) -> Option<&Path> {
        match self {
            Self::SaveState { temporary_path, .. } => Some(temporary_path),
            Self::OpenTemporaryState { path, .. } | Self::WriteState { path, .. } | Self::FlushState { path, .. } => Some(path),
            _ => None,
        }
    }

    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::HomeDirectoryUnavailable => "config.home_directory_unavailable",
            Self::InvalidStatePath { .. } => "config.state.path_invalid",
            Self::ReadState { .. } => "config.state.read_failed",
            Self::StateNotUtf8 { .. } => "config.state.utf8_invalid",
            Self::ParseState { .. } => "config.state.parse_failed",
            Self::SerializeState { .. } => "config.state.serialize_failed",
            Self::CreateStateDirectory { .. } => "config.state.directory_create_failed",
            Self::OpenTemporaryState { .. } => "config.state.temporary_open_failed",
            Self::WriteState { .. } => "config.state.write_failed",
            Self::FlushState { .. } => "config.state.flush_failed",
            Self::SaveState { .. } => "config.state.save_failed",
        }
    }
}

impl fmt::Debug for LayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut debug = f.debug_struct("LayoutError");
        debug.field("code", &self.code());
        if let Some(path) = self.path() {
            debug.field("path", &path);
        }
        if let Some(path) = self.temporary_path() {
            debug.field("temporary_path", &path);
        }
        match self {
            Self::ReadState { source, .. }
            | Self::CreateStateDirectory { source, .. }
            | Self::OpenTemporaryState { source, .. }
            | Self::WriteState { source, .. }
            | Self::FlushState { source, .. }
            | Self::SaveState { source, .. } => {
                // Keep the real I/O source for inspection without logging arbitrary custom prose.
                debug.field("io_kind", &source.kind()).field("raw_os_error", &source.raw_os_error());
            }
            Self::StateNotUtf8 { source, .. } => {
                debug.field("source", source);
            }
            Self::ParseState { source, .. } | Self::SerializeState { source, .. } => {
                debug.field("source", source);
            }
            _ => {}
        }
        debug.finish()
    }
}

impl fmt::Display for LayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for LayoutError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ReadState { source, .. }
            | Self::CreateStateDirectory { source, .. }
            | Self::OpenTemporaryState { source, .. }
            | Self::WriteState { source, .. }
            | Self::FlushState { source, .. }
            | Self::SaveState { source, .. } => Some(source),
            Self::StateNotUtf8 { source, .. } => Some(source),
            Self::ParseState { source, .. } | Self::SerializeState { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ConnectionConfig, TopicSubscription};

    #[test]
    fn validation_codes_are_stable_and_preserve_fields() {
        for (code, expected) in [
            (ConnectionValidationCode::MissingHost, "config.validation.host_required"),
            (
                ConnectionValidationCode::HostContainsSchemeOrPath,
                "config.validation.host_scheme_or_path",
            ),
            (ConnectionValidationCode::InvalidPort, "config.validation.port_invalid"),
            (ConnectionValidationCode::InvalidClientId, "config.validation.client_id_invalid"),
            (
                ConnectionValidationCode::InvalidTopicFilter,
                "config.validation.topic_filter_invalid",
            ),
            (ConnectionValidationCode::MissingTopics, "config.validation.topics_required"),
            (
                ConnectionValidationCode::DuplicateTopicFilter,
                "config.validation.topic_filter_duplicate",
            ),
            (
                ConnectionValidationCode::PasswordWithoutUsername,
                "config.validation.username_required",
            ),
        ] {
            assert_eq!(code.to_string(), expected);
        }
        let mut config = ConnectionConfig {
            topics: vec![],
            ..Default::default()
        };
        assert_eq!(config.validate().unwrap_err().code(), ConnectionValidationCode::MissingTopics);
        config.topics = vec![TopicSubscription::default(), TopicSubscription::default()];
        assert_eq!(
            config.validate().unwrap_err().code(),
            ConnectionValidationCode::DuplicateTopicFilter
        );
        config.topics = vec![TopicSubscription::default()];
        config.password = "secret-password".into();
        let error = config.validate().unwrap_err();
        assert_eq!(error.field(), ConnectionField::Username);
        assert_eq!(error.code(), ConnectionValidationCode::PasswordWithoutUsername);
        assert_eq!(config.validate_broker_connection().unwrap_err(), error);
        let error = ConfigError::from(error);
        assert_eq!(error.to_string(), "config.validation.username_required");
        assert!(matches!(error, ConfigError::Validation(_)));
    }

    #[test]
    fn layout_diagnostics_preserve_codes_paths_and_sources_without_arbitrary_prose() {
        use std::error::Error as _;
        let error = LayoutError::HomeDirectoryUnavailable;
        assert_eq!(error.to_string(), "config.home_directory_unavailable");
        assert_eq!(error.path(), None);
        assert_eq!(error.temporary_path(), None);
        assert!(error.source().is_none());

        let diagnostic = LayoutDiagnostic::Toml { span: Some(2..7) };
        assert_eq!(diagnostic.to_string(), "config.diagnostic.toml_parse");
        assert_eq!(format!("{diagnostic:?}"), "Toml { span: Some(2..7) }");
        assert_eq!(LayoutDiagnostic::TomlSerialization.to_string(), "config.diagnostic.toml_serialize");

        let error = LayoutError::SaveState {
            path: "state.toml".into(),
            temporary_path: "state.toml.tmp".into(),
            source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "io-secret"),
        };
        assert_eq!(error.to_string(), "config.state.save_failed");
        assert_eq!(error.path(), Some(Path::new("state.toml")));
        assert_eq!(error.temporary_path(), Some(Path::new("state.toml.tmp")));
        assert_eq!(
            error.source().unwrap().downcast_ref::<std::io::Error>().unwrap().to_string(),
            "io-secret"
        );
        let debug = format!("{error:#?}");
        assert!(debug.contains("config.state.save_failed"));
        assert!(debug.contains("path: \"state.toml\""));
        assert!(debug.contains("temporary_path: \"state.toml.tmp\""));
        assert!(debug.contains("PermissionDenied"));
        assert!(!debug.contains("io-secret"));
        let error = LayoutError::OpenTemporaryState {
            path: "state.toml.tmp".into(),
            source: std::io::ErrorKind::PermissionDenied.into(),
        };
        assert_eq!(error.path(), Some(Path::new("state.toml.tmp")));
        assert_eq!(error.temporary_path(), error.path());
    }

    #[test]
    fn diagnostic_readers_and_debug_preserve_rename_paths_and_selection_bounds() {
        let error = ConfigError::SaveConfig {
            path: "config.json".into(),
            temporary_path: "config.json.tmp".into(),
            source: std::io::ErrorKind::PermissionDenied.into(),
        };
        let debug = format!("{error:?}");
        assert!(debug.contains(&format!("path: {:?}", error.path().unwrap())));
        assert!(debug.contains(&format!("temporary_path: {:?}", error.temporary_path().unwrap())));
        assert!(debug.contains("source:"));
        assert_eq!(error.invalid_selection(), None);
        assert_eq!(error.to_string(), error.code());
        let error = ConfigError::OpenTemporaryConfig {
            path: "config.json.tmp".into(),
            source: std::io::ErrorKind::PermissionDenied.into(),
        };
        assert_eq!(error.path(), Some(Path::new("config.json.tmp")));
        assert_eq!(error.temporary_path(), error.path());
        let error = ConfigError::SelectedConnectionOutOfRange {
            selected: 4,
            connection_count: 2,
        };
        assert_eq!(error.invalid_selection(), Some((4, 2)));
        assert_eq!(error.path(), None);
        assert_eq!(error.temporary_path(), None);
        let debug = format!("{error:?}");
        assert!(debug.contains("selected: 4"));
        assert!(debug.contains("connection_count: 2"));
    }

    #[test]
    fn diagnostic_sanitization_does_not_accept_arbitrary_code_like_messages() {
        for message in [
            "config.connection_uri.invalid: secret-password",
            "config.connection_uri.invalid at line 1 column 2: secret-password",
            "invalid value: string config.connection_uri.invalid",
            "secret-password",
        ] {
            let error: serde_json::Error = serde::de::Error::custom(message);
            let diagnostic = ConfigDiagnostic::from(error);
            assert!(matches!(
                diagnostic,
                ConfigDiagnostic::Json {
                    category: serde_json::error::Category::Data,
                    ..
                }
            ));
            assert_eq!(diagnostic.to_string(), "config.diagnostic.json_data");
            assert!(!format!("{diagnostic:?}").contains("secret-password"));
        }
    }

    #[test]
    fn credential_errors_do_not_expose_secret_bytes() {
        let secret = b"secret-in-error".to_vec();
        for error in [
            KeyringError::BadEncoding(secret.clone()),
            KeyringError::BadDataFormat(secret, std::io::Error::other("secret-in-error").into()),
        ] {
            use std::error::Error as _;
            let error = credential_error(error);
            assert!(matches!(error, ConfigError::CredentialNotDecodable));
            assert_eq!(error.to_string(), "config.credentials.decode_failed");
            assert!(error.source().is_none());
            assert!(!format!("{error:#?}").contains("secret-in-error"));
        }
    }
}
