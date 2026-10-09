//! Connection settings on disk and MQTT passwords in the system credential store.

use std::{
    collections::{HashSet, hash_map::RandomState},
    fmt, fs,
    hash::{BuildHasher, Hash, Hasher},
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(not(test))]
use directories::{BaseDirs, ProjectDirs};
use keyring::{Entry, Error as KeyringError};
use serde::{Deserialize, Serialize, ser::SerializeStruct};
use url::Url;

use crate::mqtt::Qos;

/// An MQTT topic filter and its requested maximum delivery QoS.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TopicSubscription {
    pub topic: String,
    pub qos: Qos,
}

impl Default for TopicSubscription {
    fn default() -> Self {
        Self {
            topic: "#".into(),
            qos: Qos::AtMostOnce,
        }
    }
}

impl TopicSubscription {
    /// Rejects invalid MQTT filters, null characters, and filters over 65535 bytes.
    /// QoS validity is enforced by its type; unknown numeric levels load as QoS 0.
    pub fn validate(&self) -> Result<(), ConnectionValidationError> {
        if self.topic.len() > usize::from(u16::MAX) || self.topic.contains('\0') || !rumqttc::valid_filter(&self.topic) {
            return Err(ConnectionValidationError {
                field: ConnectionField::Topics,
                code: ConnectionValidationCode::InvalidTopicFilter,
            });
        }
        Ok(())
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, from = "DeserializedConnectionConfig")]
#[non_exhaustive]
pub struct ConnectionConfig {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub client_id: String,
    pub topics: Vec<TopicSubscription>,
    pub username: String,
    #[serde(skip)]
    pub password: String,
    pub tls: bool,
    /// Validate the server certificate chain and hostname for TLS connections; defaults to true.
    pub validate_certificate: bool,
    /// Use MQTT over WebSocket, secured with TLS when `tls` is true.
    #[serde(default)]
    pub websocket: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConnectionField {
    Name,
    Host,
    Port,
    ClientId,
    Topics,
    Username,
}

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
/// Raw Serde/TOML errors can echo plaintext credentials, including ignored legacy fields.
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
    Toml {
        span: Option<std::ops::Range<usize>>,
    },
    TomlSerialization,
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

impl From<toml::de::Error> for ConfigDiagnostic {
    fn from(error: toml::de::Error) -> Self {
        Self::Toml { span: error.span() }
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
            Self::Toml { span } => f.debug_struct("Toml").field("span", span).finish(),
            Self::TomlSerialization => f.write_str("TomlSerialization"),
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
            Self::Toml { .. } => "config.diagnostic.toml_parse",
            Self::TomlSerialization => "config.diagnostic.toml_serialize",
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
    InvalidStatePath {
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
        source: ConfigDiagnostic,
    },
    SerializeState {
        path: PathBuf,
        source: ConfigDiagnostic,
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
            | Self::InvalidStatePath { path }
            | Self::ReadConfig { path, .. }
            | Self::ParseConfig { path, .. }
            | Self::ParseConnections { path, .. }
            | Self::SerializeConfig { path, .. }
            | Self::InspectConfig { path, .. }
            | Self::ConfigNotWritable { path, .. }
            | Self::WriteApplicationConfig { path, .. }
            | Self::ReadState { path, .. }
            | Self::StateNotUtf8 { path, .. }
            | Self::ParseState { path, .. }
            | Self::SerializeState { path, .. }
            | Self::CreateConfigDirectory { path, .. }
            | Self::OpenTemporaryConfig { path, .. }
            | Self::WriteConfig { path, .. }
            | Self::FlushConfig { path, .. }
            | Self::SaveConfig { path, .. }
            | Self::CreateStateDirectory { path, .. }
            | Self::OpenTemporaryState { path, .. }
            | Self::WriteState { path, .. }
            | Self::FlushState { path, .. }
            | Self::SaveState { path, .. } => Some(path),
            Self::SaveRollbackFailed { source, .. } => source.path(),
            _ => None,
        }
    }

    /// The temporary file involved in an open, write, flush, or rename failure.
    pub fn temporary_path(&self) -> Option<&Path> {
        match self {
            Self::SaveConfig { temporary_path, .. } | Self::SaveState { temporary_path, .. } => Some(temporary_path),
            Self::OpenTemporaryConfig { path, .. }
            | Self::WriteConfig { path, .. }
            | Self::FlushConfig { path, .. }
            | Self::OpenTemporaryState { path, .. }
            | Self::WriteState { path, .. }
            | Self::FlushState { path, .. } => Some(path),
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
            Self::InvalidStatePath { .. } => "config.state.path_invalid",
            Self::ReadConfig { .. } => "config.read_failed",
            Self::ParseConfig { .. } => "config.parse_failed",
            Self::ParseConnections { .. } => "config.connections.parse_failed",
            Self::SerializeConfig { .. } => "config.serialize_failed",
            Self::ConfigObjectRequired => "config.json_object_required",
            Self::InspectConfig { .. } => "config.inspect_failed",
            Self::ConfigNotWritable { .. } => "config.not_writable",
            Self::WriteApplicationConfig { .. } => "config.application.write_failed",
            Self::ReadState { .. } => "config.state.read_failed",
            Self::StateNotUtf8 { .. } => "config.state.utf8_invalid",
            Self::ParseState { .. } => "config.state.parse_failed",
            Self::SerializeState { .. } => "config.state.serialize_failed",
            Self::CreateConfigDirectory { .. } => "config.directory_create_failed",
            Self::OpenTemporaryConfig { .. } => "config.temporary_open_failed",
            Self::WriteConfig { .. } => "config.write_failed",
            Self::FlushConfig { .. } => "config.flush_failed",
            Self::SaveConfig { .. } => "config.save_failed",
            Self::CreateStateDirectory { .. } => "config.state.directory_create_failed",
            Self::OpenTemporaryState { .. } => "config.state.temporary_open_failed",
            Self::WriteState { .. } => "config.state.write_failed",
            Self::FlushState { .. } => "config.state.flush_failed",
            Self::SaveState { .. } => "config.state.save_failed",
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
            | Self::ReadState { source, .. }
            | Self::CreateConfigDirectory { source, .. }
            | Self::OpenTemporaryConfig { source, .. }
            | Self::WriteConfig { source, .. }
            | Self::FlushConfig { source, .. }
            | Self::SaveConfig { source, .. }
            | Self::CreateStateDirectory { source, .. }
            | Self::OpenTemporaryState { source, .. }
            | Self::WriteState { source, .. }
            | Self::FlushState { source, .. }
            | Self::SaveState { source, .. } => Some(source),
            Self::ParseConfig { source, .. }
            | Self::ParseConnections { source, .. }
            | Self::SerializeConfig { source, .. }
            | Self::ParseState { source, .. }
            | Self::SerializeState { source, .. } => Some(source),
            Self::StateNotUtf8 { source, .. } | Self::ConnectionUriUsernameNotUtf8 { source } => Some(source),
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

impl Default for ConnectionConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            host: "localhost".into(),
            port: 1883,
            client_id: default_client_id(),
            topics: vec![TopicSubscription::default()],
            username: String::new(),
            password: String::new(),
            tls: false,
            validate_certificate: true,
            websocket: false,
        }
    }
}

// Keep legacy fields confined to deserialization, and never accept a plaintext password.
#[derive(Deserialize)]
#[serde(default)]
struct DeserializedConnectionConfig {
    name: String,
    host: String,
    port: u16,
    client_id: String,
    topics: Option<Vec<TopicSubscription>>,
    base_topic: Option<String>,
    username: String,
    tls: bool,
    validate_certificate: bool,
    websocket: bool,
}

impl Default for DeserializedConnectionConfig {
    fn default() -> Self {
        let config = ConnectionConfig::default();
        Self {
            name: config.name,
            host: config.host,
            port: config.port,
            client_id: config.client_id,
            topics: None,
            base_topic: None,
            username: config.username,
            tls: config.tls,
            validate_certificate: config.validate_certificate,
            websocket: config.websocket,
        }
    }
}

impl From<DeserializedConnectionConfig> for ConnectionConfig {
    fn from(config: DeserializedConnectionConfig) -> Self {
        let topics = config.topics.unwrap_or_else(|| {
            config.base_topic.map_or_else(
                || vec![TopicSubscription::default()],
                |topic| {
                    vec![TopicSubscription {
                        topic,
                        qos: Qos::ExactlyOnce,
                    }]
                },
            )
        });
        Self {
            name: config.name,
            host: config.host,
            port: config.port,
            client_id: config.client_id,
            topics,
            username: config.username,
            password: String::new(),
            tls: config.tls,
            validate_certificate: config.validate_certificate,
            websocket: config.websocket,
        }
    }
}

fn default_client_id() -> String {
    let mut hasher = RandomState::new().build_hasher();
    std::process::id().hash(&mut hasher);
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .hash(&mut hasher);
    format!("topq-{:016x}", hasher.finish())
}

impl ConnectionConfig {
    pub(crate) fn missing_password(&self) -> bool {
        !self.username.is_empty() && self.password.is_empty()
    }

    /// Returns the broker protocol prefix for the transport and TLS settings.
    pub fn protocol(&self) -> &'static str {
        match (self.websocket, self.tls) {
            (false, false) => "mqtt://",
            (false, true) => "mqtts://",
            (true, false) => "ws://",
            (true, true) => "wss://",
        }
    }

    fn validate_broker_endpoint(&self) -> Result<(), ConnectionValidationError> {
        if self.host.trim().is_empty() {
            return Err(ConnectionValidationError {
                field: ConnectionField::Host,
                code: ConnectionValidationCode::MissingHost,
            });
        }
        if self.host.contains("://") || self.host.contains('/') {
            return Err(ConnectionValidationError {
                field: ConnectionField::Host,
                code: ConnectionValidationCode::HostContainsSchemeOrPath,
            });
        }
        if self.port == 0 {
            return Err(ConnectionValidationError {
                field: ConnectionField::Port,
                code: ConnectionValidationCode::InvalidPort,
            });
        }
        if self.client_id.trim().is_empty() || self.client_id.len() > usize::from(u16::MAX) || self.client_id.contains('\0') {
            return Err(ConnectionValidationError {
                field: ConnectionField::ClientId,
                code: ConnectionValidationCode::InvalidClientId,
            });
        }
        Ok(())
    }

    pub(crate) fn validate_broker_connection(&self) -> Result<(), ConnectionValidationError> {
        self.validate_broker_endpoint()?;
        if self.username.is_empty() && !self.password.is_empty() {
            return Err(ConnectionValidationError {
                field: ConnectionField::Username,
                code: ConnectionValidationCode::PasswordWithoutUsername,
            });
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<(), ConnectionValidationError> {
        self.validate_broker_endpoint()?;
        if self.topics.is_empty() {
            return Err(ConnectionValidationError {
                field: ConnectionField::Topics,
                code: ConnectionValidationCode::MissingTopics,
            });
        }
        let mut filters = HashSet::new();
        for subscription in &self.topics {
            subscription.validate()?;
            if !filters.insert(subscription.topic.as_str()) {
                return Err(ConnectionValidationError {
                    field: ConnectionField::Topics,
                    code: ConnectionValidationCode::DuplicateTopicFilter,
                });
            }
        }
        if self.username.is_empty() && !self.password.is_empty() {
            return Err(ConnectionValidationError {
                field: ConnectionField::Username,
                code: ConnectionValidationCode::PasswordWithoutUsername,
            });
        }
        Ok(())
    }
}

// Tests must never resolve user configuration paths or open the system credential store.
// Thread-local storage also keeps concurrently running test fixtures independent.
#[cfg(test)]
std::thread_local! {
    static TEST_CONFIG_DIR: tempfile::TempDir = tempfile::tempdir()
        .expect("Could not create a temporary application configuration directory.");
    static TEST_CREDENTIAL_STORE: std::sync::Arc<keyring_core::mock::Store> = keyring_core::mock::Store::new()
        .expect("Could not create an in-memory test credential store.");
}

#[cfg(test)]
pub fn config_path() -> Result<PathBuf, ConfigError> {
    Ok(TEST_CONFIG_DIR.with(|dir| dir.path().join("topq").join("config.json")))
}

#[cfg(not(test))]
pub fn config_path() -> Result<PathBuf, ConfigError> {
    let dirs = ProjectDirs::from("", "", "topq").ok_or(ConfigError::ConfigDirectoryUnavailable)?;
    Ok(dirs.config_dir().join("config.json"))
}

/// An absent selection means no connection is active, even when the list is nonempty.
#[derive(Default)]
pub(crate) struct SavedConnections {
    pub connections: Vec<ConnectionConfig>,
    pub selected: Option<usize>,
}

pub(crate) fn load_connections() -> Result<SavedConnections, ConfigError> {
    load_connections_from(&application_config_read_path()?, &password_entry)
}

pub(crate) fn save_connections(saved: &SavedConnections) -> Result<(), ConfigError> {
    save_connections_with_entry(saved, &password_entry)
}

fn save_connections_with_entry(
    saved: &SavedConnections,
    entry: &impl Fn(&ConnectionConfig) -> Result<Entry, ConfigError>,
) -> Result<(), ConfigError> {
    let config_path = config_path()?;
    let state_path = state_config_path()?;
    let source_path = prioritized_config_path(&state_path, &config_path)?;
    let destination_path = writable_config_path(&config_path, &state_path)?;
    let result = save_connections_to_with_source(&destination_path, &source_path, saved, entry);
    if result.as_ref().is_err_and(is_io_error) && destination_path != state_path && config_path_is_writable(&state_path) {
        return save_connections_to_with_source(&state_path, &source_path, saved, entry).map_err(|source| {
            if is_io_error(&source) {
                ConfigError::WriteApplicationConfig {
                    path: state_path,
                    alternative_path: None,
                    source: Box::new(source),
                }
            } else {
                source
            }
        });
    }
    match result {
        Err(source) if is_io_error(&source) => {
            let alternative_path = (destination_path != state_path).then_some(state_path);
            Err(ConfigError::WriteApplicationConfig {
                path: destination_path,
                alternative_path,
                source: Box::new(source),
            })
        }
        result => result,
    }
}

pub(crate) fn application_config_read_path() -> Result<PathBuf, ConfigError> {
    prioritized_config_path(&state_config_path()?, &config_path()?)
}

fn state_config_path() -> Result<PathBuf, ConfigError> {
    Ok(state_path()?.with_file_name("config.json"))
}

fn prioritized_config_path(state_path: &Path, config_path: &Path) -> Result<PathBuf, ConfigError> {
    match fs::symlink_metadata(state_path) {
        Ok(_) => Ok(state_path.to_path_buf()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(config_path.to_path_buf()),
        Err(source) => Err(ConfigError::InspectConfig {
            path: state_path.to_owned(),
            source,
        }),
    }
}

fn writable_config_path(config_path: &Path, state_path: &Path) -> Result<PathBuf, ConfigError> {
    let state_exists = match fs::symlink_metadata(state_path) {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(source) => {
            return Err(ConfigError::InspectConfig {
                path: state_path.to_owned(),
                source,
            });
        }
    };
    if state_exists {
        return if config_path_is_writable(state_path) {
            Ok(state_path.to_path_buf())
        } else {
            Err(ConfigError::ConfigNotWritable {
                path: state_path.to_owned(),
                alternative_path: None,
            })
        };
    }
    if config_path_is_writable(config_path) {
        return Ok(config_path.to_path_buf());
    }
    if config_path_is_writable(state_path) {
        return Ok(state_path.to_path_buf());
    }
    Err(ConfigError::ConfigNotWritable {
        path: config_path.to_owned(),
        alternative_path: Some(state_path.to_owned()),
    })
}

fn config_path_is_writable(path: &Path) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    if !nearest_existing_directory_is_writable(parent) {
        return false;
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            fs::metadata(path).is_ok_and(|target| target.is_file() && permissions_allow_write(&target))
        }
        Ok(metadata) => metadata.is_file() && permissions_allow_write(&metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(_) => false,
    }
}

fn nearest_existing_directory_is_writable(path: &Path) -> bool {
    let mut candidate = path;
    loop {
        match fs::metadata(candidate) {
            Ok(metadata) => return metadata.is_dir() && permissions_allow_directory_write(&metadata),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let Some(parent) = candidate.parent() else {
                    return false;
                };
                candidate = parent;
            }
            Err(_) => return false,
        }
    }
}

#[cfg(unix)]
fn permissions_allow_write(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o222 != 0
}

#[cfg(not(unix))]
fn permissions_allow_write(metadata: &fs::Metadata) -> bool {
    !metadata.permissions().readonly()
}

#[cfg(unix)]
fn permissions_allow_directory_write(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o222 != 0 && metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn permissions_allow_directory_write(metadata: &fs::Metadata) -> bool {
    !metadata.permissions().readonly()
}

fn is_io_error(error: &ConfigError) -> bool {
    match error {
        ConfigError::ReadConfig { .. }
        | ConfigError::InspectConfig { .. }
        | ConfigError::ReadState { .. }
        | ConfigError::CreateConfigDirectory { .. }
        | ConfigError::OpenTemporaryConfig { .. }
        | ConfigError::WriteConfig { .. }
        | ConfigError::FlushConfig { .. }
        | ConfigError::SaveConfig { .. }
        | ConfigError::CreateStateDirectory { .. }
        | ConfigError::OpenTemporaryState { .. }
        | ConfigError::WriteState { .. }
        | ConfigError::FlushState { .. }
        | ConfigError::SaveState { .. } => true,
        ConfigError::WriteApplicationConfig { source, .. } => is_io_error(source),
        // A failed credential rollback must not be hidden by retrying a disk write elsewhere.
        _ => false,
    }
}

#[derive(Default, Serialize, Deserialize)]
struct StoredConnections {
    connections: Vec<SavedConnection>,
    active_connection: Option<String>,
}

fn read_connections(path: &Path) -> Result<Option<StoredConnections>, ConfigError> {
    let Some(value) = read_config_value(path)? else {
        return Ok(None);
    };
    if path.file_name().is_some_and(|name| name == "config.json") && value.get("connections").is_none() {
        let appearance_only = ["mode", "light_theme", "dark_theme", "reduce_motion", "theme"]
            .iter()
            .any(|key| value.get(key).is_some());
        if appearance_only || value.as_object().is_some_and(serde_json::Map::is_empty) {
            return Ok(Some(StoredConnections::default()));
        }
    }
    let stored = serde_json::from_value(value).map_err(|error| ConfigError::ParseConnections {
        path: path.to_owned(),
        source: error.into(),
    })?;
    Ok(Some(stored))
}

fn load_connections_from(
    path: &Path,
    entry: &impl Fn(&ConnectionConfig) -> Result<Entry, ConfigError>,
) -> Result<SavedConnections, ConfigError> {
    let Some(stored) = read_connections(path)? else {
        return Ok(SavedConnections::default());
    };
    let mut connections = Vec::with_capacity(stored.connections.len());
    let mut accounts = HashSet::new();
    let mut names = HashSet::new();
    for saved in stored.connections {
        let mut config = saved.config;
        if !config.name.is_empty() && !names.insert(config.name.clone()) {
            continue;
        }
        config.validate()?;
        if !accounts.insert(credential_account(&config)) {
            return Err(ConfigError::DuplicateConnectionAccount);
        }
        if !config.username.is_empty() {
            config.password = match entry(&config)?.get_password() {
                Ok(password) => password,
                Err(KeyringError::NoEntry) => String::new(),
                Err(error) => {
                    return Err(ConfigError::RetrievePassword {
                        source: Box::new(credential_error(error)),
                    });
                }
            };
        }
        connections.push(config);
    }
    // An unknown name must not prevent access to the saved connections.
    let selected = stored.active_connection.and_then(|name| {
        connections
            .iter()
            .position(|config| config.name == name)
            .or_else(|| (!connections.is_empty()).then_some(0))
    });
    Ok(SavedConnections { connections, selected })
}

struct CredentialChange<'a> {
    config: &'a ConnectionConfig,
    previous: Option<String>,
    desired: Option<&'a str>,
}

fn set_credential(
    config: &ConnectionConfig,
    password: Option<&str>,
    entry: &impl Fn(&ConnectionConfig) -> Result<Entry, ConfigError>,
) -> Result<(), ConfigError> {
    let credential = entry(config)?;
    match password {
        Some(password) => credential.set_password(password).map_err(credential_error),
        None => match credential.delete_credential() {
            Ok(()) | Err(KeyringError::NoEntry) => Ok(()),
            Err(error) => Err(credential_error(error)),
        },
    }
}

fn rollback_credentials(
    changes: &[CredentialChange<'_>],
    applied: &[usize],
    entry: &impl Fn(&ConnectionConfig) -> Result<Entry, ConfigError>,
) -> Result<(), ConfigError> {
    let mut failures = Vec::new();
    for &index in applied.iter().rev() {
        let change = &changes[index];
        if let Err(error) = set_credential(change.config, change.previous.as_deref(), entry) {
            failures.push(error);
        }
    }
    if !failures.is_empty() {
        return Err(ConfigError::RollbackCredentials { failures });
    }
    Ok(())
}

#[cfg(test)]
fn save_connections_to(
    path: &Path,
    saved: &SavedConnections,
    entry: &impl Fn(&ConnectionConfig) -> Result<Entry, ConfigError>,
) -> Result<(), ConfigError> {
    save_connections_to_with_source(path, path, saved, entry)
}

fn save_connections_to_with_source(
    path: &Path,
    source_path: &Path,
    saved: &SavedConnections,
    entry: &impl Fn(&ConnectionConfig) -> Result<Entry, ConfigError>,
) -> Result<(), ConfigError> {
    if let Some(selected) = saved.selected.filter(|&index| index >= saved.connections.len()) {
        return Err(ConfigError::SelectedConnectionOutOfRange {
            selected,
            connection_count: saved.connections.len(),
        });
    }
    let mut accounts = HashSet::new();
    for config in &saved.connections {
        config.validate()?;
        if !accounts.insert(credential_account(config)) {
            return Err(ConfigError::DuplicateConnectionAccount);
        }
    }
    let previous = read_connections(source_path)?.unwrap_or_default();
    let retained_secrets: HashSet<_> = saved
        .connections
        .iter()
        .filter(|config| !config.password.is_empty())
        .map(credential_account)
        .collect();
    let mut changes = Vec::new();
    for config in &saved.connections {
        if !config.password.is_empty() {
            changes.push(CredentialChange {
                config,
                previous: None,
                desired: Some(&config.password),
            });
        }
    }
    for old in &previous.connections {
        if !old.config.username.is_empty() && !retained_secrets.contains(&credential_account(&old.config)) {
            changes.push(CredentialChange {
                config: &old.config,
                previous: None,
                desired: None,
            });
        }
    }
    // Snapshot every affected credential before changing any of them. This lets a
    // keyring or disk failure restore the previously usable saved connections.
    for change in &mut changes {
        change.previous = match entry(change.config)?.get_password() {
            Ok(password) => Some(password),
            Err(KeyringError::NoEntry) => None,
            Err(error) => {
                return Err(ConfigError::InspectPassword {
                    source: Box::new(credential_error(error)),
                });
            }
        };
    }
    let stored = StoredConnections {
        connections: saved
            .connections
            .iter()
            .map(|config| SavedConnection { config: config.clone() })
            .collect(),
        active_connection: saved.selected.map(|index| saved.connections[index].name.clone()),
    };
    let mut applied = Vec::new();
    let result = (|| {
        for (index, change) in changes.iter().enumerate() {
            if change.previous.as_deref() != change.desired {
                set_credential(change.config, change.desired, entry)
                    .map_err(|source| ConfigError::UpdatePassword { source: Box::new(source) })?;
                applied.push(index);
            }
        }
        update_json_with_fallback(path, (path != source_path).then_some(source_path), &stored)
    })();
    if let Err(error) = result {
        if let Err(rollback) = rollback_credentials(&changes, &applied, entry) {
            return Err(ConfigError::SaveRollbackFailed {
                source: Box::new(error),
                rollback: Box::new(rollback),
            });
        }
        return Err(error);
    }
    Ok(())
}

struct SavedConnection {
    config: ConnectionConfig,
}

impl Serialize for SavedConnection {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let config = &self.config;
        let mut state = serializer.serialize_struct("SavedConnection", 5)?;
        state.serialize_field("name", &config.name)?;
        let connection = connection_uri(config).map_err(serde::ser::Error::custom)?;
        state.serialize_field("connection", &connection)?;
        state.serialize_field("client_id", &config.client_id)?;
        state.serialize_field("topics", &config.topics)?;
        state.serialize_field("validate_certificate", &config.validate_certificate)?;
        state.end()
    }
}

impl<'de> Deserialize<'de> for SavedConnection {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct DeserializedSavedConnection {
            #[serde(default)]
            connection: Option<String>,
            #[serde(flatten)]
            config: ConnectionConfig,
        }

        let mut saved = DeserializedSavedConnection::deserialize(deserializer)?;
        if let Some(connection) = saved.connection {
            let address = parse_connection_uri(&connection).map_err(serde::de::Error::custom)?;
            saved.config.host = address.host;
            saved.config.port = address.port;
            saved.config.tls = address.tls;
            saved.config.websocket = address.websocket;
            saved.config.username = address.username;
        }
        Ok(Self { config: saved.config })
    }
}

struct ConnectionAddress {
    host: String,
    port: u16,
    tls: bool,
    websocket: bool,
    username: String,
}

fn connection_uri(config: &ConnectionConfig) -> Result<String, ConfigError> {
    let host = if config.host.contains(':') {
        format!("[{}]", config.host)
    } else {
        config.host.clone()
    };
    let mut url = Url::parse(&format!("{}{host}:{}", config.protocol(), config.port))
        .map_err(|source| ConfigError::FormatConnectionUri { source })?;
    url.set_username(&config.username)
        .map_err(|()| ConfigError::EncodeConnectionUsername)?;
    let uri = url.as_str();
    Ok(uri.strip_suffix('/').unwrap_or(uri).to_owned())
}

fn parse_connection_uri(connection: &str) -> Result<ConnectionAddress, ConfigError> {
    let url = Url::parse(connection).map_err(|source| ConfigError::InvalidConnectionUri { source })?;
    if url.password().is_some() || !matches!(url.path(), "" | "/") || url.query().is_some() || url.fragment().is_some() {
        return Err(ConfigError::ConnectionUriComponents);
    }
    let (tls, websocket, default_port) = match url.scheme() {
        "mqtt" => (false, false, 1883),
        "mqtts" => (true, false, 8883),
        "ws" => (false, true, 80),
        "wss" => (true, true, 443),
        _ => return Err(ConfigError::ConnectionUriScheme),
    };
    let host = match url.host().ok_or(ConfigError::ConnectionUriHostRequired)? {
        url::Host::Domain(host) => host.to_owned(),
        url::Host::Ipv4(host) => host.to_string(),
        url::Host::Ipv6(host) => host.to_string(),
    };
    let port = url.port().unwrap_or(default_port);
    let username = percent_encoding::percent_decode_str(url.username())
        .decode_utf8()
        .map_err(|source| ConfigError::ConnectionUriUsernameNotUtf8 { source })?
        .into_owned();
    Ok(ConnectionAddress {
        host,
        port,
        tls,
        websocket,
        username,
    })
}

fn credential_account(config: &ConnectionConfig) -> String {
    // Preserve legacy TCP/TLS keys; WebSocket uses a separate protocol scope.
    let transport = if config.websocket {
        if config.tls { "wss" } else { "ws" }
    } else if config.tls {
        "true"
    } else {
        "false"
    };
    // Length prefixes keep host and username delimiters from colliding.
    format!(
        "{}:{}:{}:{}:{}:{}",
        config.host.len(),
        config.host,
        config.port,
        transport,
        config.username.len(),
        config.username
    )
}

#[cfg(test)]
fn password_entry(config: &ConnectionConfig) -> Result<Entry, ConfigError> {
    use keyring_core::api::CredentialStoreApi;

    TEST_CREDENTIAL_STORE.with(|store| {
        Ok(Entry {
            inner: store
                .build("topq", &credential_account(config), None)
                .map_err(|error| ConfigError::OpenCredentialStore {
                    source: Box::new(credential_error(error)),
                })?,
        })
    })
}

#[cfg(not(test))]
fn password_entry(config: &ConnectionConfig) -> Result<Entry, ConfigError> {
    Entry::new("topq", &credential_account(config)).map_err(|error| ConfigError::OpenCredentialStore {
        source: Box::new(credential_error(error)),
    })
}

fn credential_error(error: KeyringError) -> ConfigError {
    // Keyring payloads and their sources can contain secrets. Retain only the reason.
    match error {
        KeyringError::NoEntry => ConfigError::CredentialMissing,
        KeyringError::NoStorageAccess(_) => ConfigError::CredentialAccessDenied,
        KeyringError::NoDefaultStore => ConfigError::CredentialStoreUnavailable,
        KeyringError::BadEncoding(_) | KeyringError::BadDataFormat(_, _) => ConfigError::CredentialNotDecodable,
        _ => ConfigError::CredentialOperationFailed,
    }
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct LayoutPreferences {
    topics_width_rem: Option<f32>,
    topics_width_fraction: Option<f32>,
    publish_height_rem: Option<f32>,
    publish_height_fraction: Option<f32>,
    publish_open: Option<bool>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct TopicsLayout {
    pub width_rem: Option<f32>,
    pub width_fraction: Option<f32>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct PublishLayout {
    pub open: bool,
    pub height_rem: Option<f32>,
    pub height_fraction: Option<f32>,
}

#[cfg(test)]
fn state_path() -> Result<PathBuf, ConfigError> {
    std::thread_local! {
        static TEST_STATE_DIR: tempfile::TempDir = tempfile::tempdir()
            .expect("Could not create a temporary application state directory.");
    }
    Ok(TEST_STATE_DIR.with(|dir| dir.path().join("topq").join("state.toml")))
}

#[cfg(not(test))]
fn state_path() -> Result<PathBuf, ConfigError> {
    let state_dir = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .map(Ok)
        .unwrap_or_else(|| {
            BaseDirs::new()
                .map(|dirs| dirs.home_dir().join(".local").join("state"))
                .ok_or(ConfigError::HomeDirectoryUnavailable)
        })?;
    Ok(state_dir.join("topq").join("state.toml"))
}

pub(crate) fn load_topics_layout() -> Result<TopicsLayout, ConfigError> {
    load_topics_layout_from(&state_path()?)
}

fn load_layout_preferences_from(path: &Path) -> Result<LayoutPreferences, ConfigError> {
    let data = match fs::read(path) {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(LayoutPreferences::default()),
        Err(source) => {
            return Err(ConfigError::ReadState {
                path: path.to_owned(),
                source,
            });
        }
    };
    let text = std::str::from_utf8(&data).map_err(|source| ConfigError::StateNotUtf8 {
        path: path.to_owned(),
        source,
    })?;
    toml::from_str(text).map_err(|error| ConfigError::ParseState {
        path: path.to_owned(),
        source: error.into(),
    })
}

fn load_topics_layout_from(path: &Path) -> Result<TopicsLayout, ConfigError> {
    let preferences = load_layout_preferences_from(path)?;
    Ok(TopicsLayout {
        width_rem: preferences.topics_width_rem.filter(|width| width.is_finite() && *width > 0.),
        width_fraction: preferences
            .topics_width_fraction
            .filter(|fraction| fraction.is_finite() && (0. ..=1.).contains(fraction)),
    })
}

pub(crate) fn load_publish_layout() -> Result<PublishLayout, ConfigError> {
    load_publish_layout_from(&state_path()?)
}

fn load_publish_layout_from(path: &Path) -> Result<PublishLayout, ConfigError> {
    let preferences = load_layout_preferences_from(path)?;
    Ok(PublishLayout {
        open: preferences.publish_open.unwrap_or_default(),
        height_rem: preferences.publish_height_rem.filter(|height| height.is_finite() && *height > 0.),
        height_fraction: preferences
            .publish_height_fraction
            .filter(|fraction| fraction.is_finite() && (0. ..=1.).contains(fraction)),
    })
}

pub(crate) fn save_topics_width(width: f32, fraction: f32) -> Result<(), ConfigError> {
    save_topics_width_to(&state_path()?, width, fraction)
}

fn save_topics_width_to(path: &Path, width: f32, fraction: f32) -> Result<(), ConfigError> {
    let mut preferences = load_layout_preferences_from(path)?;
    preferences.topics_width_rem = Some(width);
    preferences.topics_width_fraction = Some(fraction);
    write_toml(path, &preferences)
}

pub(crate) fn save_publish_height(height_rem: f32, fraction: f32) -> Result<(), ConfigError> {
    save_publish_height_to(&state_path()?, height_rem, fraction)
}

fn save_publish_height_to(path: &Path, height_rem: f32, fraction: f32) -> Result<(), ConfigError> {
    let mut preferences = load_layout_preferences_from(path)?;
    preferences.publish_height_rem = Some(height_rem);
    preferences.publish_height_fraction = Some(fraction);
    write_toml(path, &preferences)
}

pub(crate) fn save_publish_open(open: bool) -> Result<(), ConfigError> {
    save_publish_open_to(&state_path()?, open)
}

fn save_publish_open_to(path: &Path, open: bool) -> Result<(), ConfigError> {
    let mut preferences = load_layout_preferences_from(path)?;
    preferences.publish_open = Some(open);
    write_toml(path, &preferences)
}

fn write_toml(path: &Path, value: &impl Serialize) -> Result<(), ConfigError> {
    let parent = path
        .parent()
        .ok_or_else(|| ConfigError::InvalidStatePath { path: path.to_owned() })?;
    fs::create_dir_all(parent).map_err(|source| ConfigError::CreateStateDirectory {
        path: parent.to_owned(),
        source,
    })?;
    let temp = path.with_extension("toml.tmp");
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp).map_err(|source| ConfigError::OpenTemporaryState {
        path: temp.clone(),
        source,
    })?;
    let data = toml::to_string_pretty(value).map_err(|_| ConfigError::SerializeState {
        path: path.to_owned(),
        source: ConfigDiagnostic::TomlSerialization,
    })?;
    file.write_all(data.as_bytes()).map_err(|source| ConfigError::WriteState {
        path: temp.clone(),
        source,
    })?;
    file.sync_all().map_err(|source| ConfigError::FlushState {
        path: temp.clone(),
        source,
    })?;
    fs::rename(&temp, path).map_err(|source| ConfigError::SaveState {
        path: path.to_owned(),
        temporary_path: temp.clone(),
        source,
    })?;
    Ok(())
}

pub(crate) fn update_json(path: &Path, value: &impl Serialize) -> Result<(), ConfigError> {
    update_json_with_fallback(path, None, value)
}

fn update_json_with_fallback(path: &Path, fallback_path: Option<&Path>, value: &impl Serialize) -> Result<(), ConfigError> {
    if path.file_name().is_none_or(|name| name != "config.json") {
        return write_json(path, value);
    }
    let mut current = match read_config_value(path)? {
        Some(value) => value,
        None => match fallback_path {
            Some(fallback_path) => read_config_value(fallback_path)?.unwrap_or_else(empty_config_value),
            None => empty_config_value(),
        },
    };
    let updates = serde_json::to_value(value).map_err(|error| ConfigError::SerializeConfig {
        path: path.to_owned(),
        source: error.into(),
    })?;
    let (Some(current), Some(updates)) = (current.as_object_mut(), updates.as_object()) else {
        return Err(ConfigError::ConfigObjectRequired);
    };
    current.extend(updates.clone());
    write_json(path, &current)
}

fn empty_config_value() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

pub(crate) fn update_application_config(preferred_path: &Path, value: &impl Serialize) -> Result<PathBuf, ConfigError> {
    let config_path = config_path()?;
    let state_path = state_config_path()?;
    if preferred_path != config_path && preferred_path != state_path {
        update_json(preferred_path, value)?;
        return Ok(preferred_path.to_path_buf());
    }

    let destination_path = writable_config_path(&config_path, &state_path)?;
    let source_path = prioritized_config_path(&state_path, &config_path)?;
    let fallback_path = (destination_path != source_path).then_some(source_path.as_path());
    match update_json_with_fallback(&destination_path, fallback_path, value) {
        Ok(()) => Ok(destination_path),
        Err(error) if destination_path != state_path && is_io_error(&error) && config_path_is_writable(&state_path) => {
            let fallback_path = prioritized_config_path(&state_path, &config_path)?;
            update_json_with_fallback(&state_path, Some(&fallback_path), value).map_err(|source| {
                if is_io_error(&source) {
                    ConfigError::WriteApplicationConfig {
                        path: state_path.clone(),
                        alternative_path: None,
                        source: Box::new(source),
                    }
                } else {
                    source
                }
            })?;
            Ok(state_path)
        }
        Err(source) if is_io_error(&source) => {
            let alternative_path = (destination_path != state_path).then_some(state_path);
            Err(ConfigError::WriteApplicationConfig {
                path: destination_path,
                alternative_path,
                source: Box::new(source),
            })
        }
        Err(source) => Err(source),
    }
}

pub(crate) fn read_config_value(path: &Path) -> Result<Option<serde_json::Value>, ConfigError> {
    match fs::read(path) {
        Ok(data) => serde_json::from_slice(&data).map(Some).map_err(|error| ConfigError::ParseConfig {
            path: path.to_owned(),
            source: error.into(),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(ConfigError::ReadConfig {
            path: path.to_owned(),
            source,
        }),
    }
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), ConfigError> {
    let parent = path
        .parent()
        .ok_or_else(|| ConfigError::InvalidConfigPath { path: path.to_owned() })?;
    fs::create_dir_all(parent).map_err(|source| ConfigError::CreateConfigDirectory {
        path: parent.to_owned(),
        source,
    })?;
    let temp = path.with_extension("json.tmp");
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp).map_err(|source| ConfigError::OpenTemporaryConfig {
        path: temp.clone(),
        source,
    })?;
    let data = serde_json::to_vec_pretty(value).map_err(|error| ConfigError::SerializeConfig {
        path: path.to_owned(),
        source: error.into(),
    })?;
    file.write_all(&data).map_err(|source| ConfigError::WriteConfig {
        path: temp.clone(),
        source,
    })?;
    file.sync_all().map_err(|source| ConfigError::FlushConfig {
        path: temp.clone(),
        source,
    })?;
    fs::rename(&temp, path).map_err(|source| ConfigError::SaveConfig {
        path: path.to_owned(),
        temporary_path: temp.clone(),
        source,
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use keyring_core::{api::CredentialStoreApi, mock};

    use super::*;

    fn mock_entries() -> impl Fn(&ConnectionConfig) -> Result<Entry, ConfigError> {
        let store = mock::Store::new().unwrap();
        move |config| {
            Ok(Entry {
                inner: store
                    .build("topq", &credential_account(config), None)
                    .map_err(|error| ConfigError::OpenCredentialStore {
                        source: Box::new(credential_error(error)),
                    })?,
            })
        }
    }

    fn authenticated_config() -> ConnectionConfig {
        ConnectionConfig {
            name: "Home".into(),
            username: "mqtt-user".into(),
            password: "test-mqtt-secret".into(),
            ..Default::default()
        }
    }

    #[test]
    fn rejects_invalid_connection_fields() {
        for (host, port, field, code) in [
            ("", 1883, ConnectionField::Host, ConnectionValidationCode::MissingHost),
            (
                "mqtt://localhost",
                1883,
                ConnectionField::Host,
                ConnectionValidationCode::HostContainsSchemeOrPath,
            ),
            ("localhost", 0, ConnectionField::Port, ConnectionValidationCode::InvalidPort),
        ] {
            let config = ConnectionConfig {
                host: host.into(),
                port,
                ..Default::default()
            };
            let error = config.validate().unwrap_err();
            assert_eq!(error.field(), field);
            assert_eq!(error.code(), code);
            assert_eq!(error.to_string(), code.as_str());
        }
        for client_id in [String::new(), "bad\0id".into(), "x".repeat(usize::from(u16::MAX) + 1)] {
            let config = ConnectionConfig {
                client_id,
                ..Default::default()
            };
            let error = config.validate().unwrap_err();
            assert_eq!(error.field(), ConnectionField::ClientId);
            assert_eq!(error.code(), ConnectionValidationCode::InvalidClientId);
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
    fn topics_default_to_all_topics_at_qos_zero_for_new_and_existing_connections() {
        let expected = vec![TopicSubscription {
            topic: "#".into(),
            qos: Qos::AtMostOnce,
        }];
        assert_eq!(ConnectionConfig::default().topics, expected);
        let config: ConnectionConfig = serde_json::from_str(r#"{"host":"broker.example","port":1883}"#).unwrap();
        assert_eq!(config.topics, expected);
        let config: ConnectionConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(config.topics, expected);
        assert_eq!(config.name, "");
        assert_eq!(config.host, "localhost");
        assert_eq!(config.port, 1883);
        assert_eq!(config.username, "");
        assert_eq!(config.password, "");
        assert!(!config.tls);
    }

    #[test]
    fn protocol_selects_tcp_or_websocket_with_optional_tls() {
        for (tls, websocket, expected) in [
            (false, false, "mqtt://"),
            (true, false, "mqtts://"),
            (false, true, "ws://"),
            (true, true, "wss://"),
        ] {
            let config = ConnectionConfig {
                tls,
                websocket,
                ..Default::default()
            };
            assert_eq!(config.protocol(), expected);
        }
    }

    #[test]
    fn saved_connection_uris_restore_transport_host_and_port() {
        for (connection, expected_protocol, expected_host, expected_port) in [
            ("mqtt://broker.example:1884", "mqtt://", "broker.example", 1884),
            ("mqtts://broker.example:8883", "mqtts://", "broker.example", 8883),
            ("ws://broker.example:8080", "ws://", "broker.example", 8080),
            ("wss://[::1]:9443", "wss://", "::1", 9443),
        ] {
            let saved: SavedConnection = serde_json::from_str(&format!(r#"{{"connection":"{connection}"}}"#)).unwrap();
            assert_eq!(saved.config.protocol(), expected_protocol);
            assert_eq!(saved.config.host, expected_host);
            assert_eq!(saved.config.port, expected_port);
        }
    }

    #[test]
    fn saved_connection_uris_restore_and_encode_usernames() {
        let saved: SavedConnection = serde_json::from_str(r#"{"connection":"mqtts://alice%40home%3Aname@broker.example:8883"}"#).unwrap();
        assert_eq!(saved.config.username, "alice@home:name");
        assert_eq!(
            connection_uri(&saved.config).unwrap(),
            "mqtts://alice%40home%3Aname@broker.example:8883"
        );
    }

    #[test]
    fn saved_connection_uris_apply_scheme_default_ports() {
        for (connection, expected_port) in [
            ("mqtt://broker.example", 1883),
            ("mqtts://broker.example", 8883),
            ("ws://broker.example", 80),
            ("wss://broker.example", 443),
        ] {
            let saved: SavedConnection = serde_json::from_str(&format!(r#"{{"connection":"{connection}"}}"#)).unwrap();
            assert_eq!(saved.config.port, expected_port);
        }
    }

    #[test]
    fn saved_connection_uris_reject_passwords_paths_and_unknown_schemes() {
        for connection in [
            "mqtt://user:password@broker.example:1883",
            "mqtt://broker.example:1883/path",
            "ftp://broker.example:21",
        ] {
            let result = serde_json::from_str::<SavedConnection>(&format!(r#"{{"connection":"{connection}"}}"#));
            assert!(result.is_err(), "accepted unexpected URI: {connection}");
        }
    }

    #[test]
    fn legacy_configs_default_to_non_websocket_without_changing_tls() {
        for (json, expected) in [("{}", "mqtt://"), (r#"{"tls":false}"#, "mqtt://"), (r#"{"tls":true}"#, "mqtts://")] {
            let config: ConnectionConfig = serde_json::from_str(json).unwrap();
            assert!(!config.websocket);
            assert_eq!(config.protocol(), expected);
        }
        assert!(!ConnectionConfig::default().websocket);
    }

    #[test]
    fn certificate_validation_defaults_to_true_for_new_and_legacy_connections() {
        assert!(ConnectionConfig::default().validate_certificate);
        for json in ["{}", r#"{"tls":false}"#, r#"{"tls":true}"#, r#"{"websocket":true}"#] {
            let config: ConnectionConfig = serde_json::from_str(json).unwrap();
            assert!(config.validate_certificate);
        }
    }

    #[test]
    fn disabled_certificate_validation_roundtrips_through_json() {
        let config = ConnectionConfig {
            tls: true,
            validate_certificate: false,
            ..Default::default()
        };
        let json = serde_json::to_value(&config).unwrap();
        assert_eq!(json["validate_certificate"], false);
        let loaded: ConnectionConfig = serde_json::from_value(json).unwrap();
        assert!(!loaded.validate_certificate);
        assert!(loaded.tls);
    }

    #[test]
    fn secure_websocket_roundtrips_with_certificate_validation_settings() {
        for validate_certificate in [true, false] {
            let config = ConnectionConfig {
                tls: true,
                websocket: true,
                port: 9002,
                validate_certificate,
                ..Default::default()
            };
            let json = serde_json::to_value(&config).unwrap();
            let loaded: ConnectionConfig = serde_json::from_value(json).unwrap();
            assert_eq!(loaded.protocol(), "wss://");
            assert_eq!(loaded.port, 9002);
            assert_eq!(loaded.validate_certificate, validate_certificate);
        }
    }

    #[test]
    fn generated_client_ids_are_unique_and_legacy_configs_receive_a_default() {
        let first = ConnectionConfig::default().client_id;
        let second = ConnectionConfig::default().client_id;
        assert!(first.starts_with("topq-"));
        assert_ne!(first, second);
        assert_eq!(first.len(), 21);

        let legacy: ConnectionConfig = serde_json::from_str(r#"{"host":"broker.example"}"#).unwrap();
        assert!(legacy.client_id.starts_with("topq-"));
        let configured: ConnectionConfig = serde_json::from_str(r#"{"client_id":"custom-client"}"#).unwrap();
        assert_eq!(configured.client_id, "custom-client");
    }

    #[test]
    fn legacy_base_topic_migrates_to_a_single_qos_two_subscription() {
        let config: ConnectionConfig = serde_json::from_str(r#"{"base_topic":"home/#"}"#).unwrap();
        assert_eq!(
            config.topics,
            vec![TopicSubscription {
                topic: "home/#".into(),
                qos: Qos::ExactlyOnce
            }]
        );
        assert!(config.validate().is_ok());
        let json = serde_json::to_value(config).unwrap();
        assert!(json.get("base_topic").is_none());
        assert_eq!(json["topics"], serde_json::json!([{"topic":"home/#","qos":2}]));
    }

    #[test]
    fn explicit_topics_take_precedence_over_legacy_base_topic_including_an_empty_list() {
        let config: ConnectionConfig = serde_json::from_str(r#"{"base_topic":"old/#","topics":[{"topic":"new/#","qos":1}]}"#).unwrap();
        assert_eq!(
            config.topics,
            vec![TopicSubscription {
                topic: "new/#".into(),
                qos: Qos::AtLeastOnce
            }]
        );
        let config: ConnectionConfig = serde_json::from_str(r#"{"base_topic":"old/#","topics":[]}"#).unwrap();
        assert!(config.topics.is_empty());
        assert_eq!(config.validate().unwrap_err().field(), ConnectionField::Topics);
    }

    #[test]
    fn invalid_legacy_filters_are_not_silently_replaced_with_defaults() {
        for json in [r#"{"base_topic":""}"#, r#"{"base_topic":"home/#/value"}"#] {
            let config: ConnectionConfig = serde_json::from_str(json).unwrap();
            assert_eq!(config.validate().unwrap_err().field(), ConnectionField::Topics);
        }
    }

    #[test]
    fn deserialization_ignores_plaintext_passwords_in_both_configuration_shapes() {
        for json in [
            r#"{"username":"mqtt-user","password":"plaintext-secret","base_topic":"home/#"}"#,
            r##"{"username":"mqtt-user","password":"plaintext-secret","topics":[{"topic":"#","qos":0}]}"##,
        ] {
            let config: ConnectionConfig = serde_json::from_str(json).unwrap();
            assert!(config.password.is_empty());
        }
    }

    #[test]
    fn validate_rejects_invalid_topic_filters() {
        for topic in ["", "home#", "home/#/value", "home+", "home/++", "home/\0"]
            .map(String::from)
            .into_iter()
            .chain(["a".repeat(usize::from(u16::MAX) + 1), "é".repeat(32768)])
        {
            let subscription = TopicSubscription {
                topic,
                qos: Qos::AtMostOnce,
            };
            let error = subscription.validate().unwrap_err();
            assert_eq!(error.field(), ConnectionField::Topics);
            assert_eq!(error.code(), ConnectionValidationCode::InvalidTopicFilter);
            let config = ConnectionConfig {
                topics: vec![subscription],
                ..Default::default()
            };
            assert_eq!(config.validate().unwrap_err().field(), ConnectionField::Topics);
        }
    }

    #[test]
    fn validate_accepts_exact_topics_and_wildcard_filters() {
        for topic in ["#", "home/#", "home/+/temperature", "$SYS/#", "/", "home/", " home/value "]
            .map(String::from)
            .into_iter()
            .chain(["a".repeat(usize::from(u16::MAX))])
        {
            for qos in [Qos::AtMostOnce, Qos::AtLeastOnce, Qos::ExactlyOnce] {
                let subscription = TopicSubscription { topic: topic.clone(), qos };
                assert!(subscription.validate().is_ok());
                let config = ConnectionConfig {
                    topics: vec![subscription],
                    ..Default::default()
                };
                assert!(config.validate().is_ok());
            }
        }
    }

    #[test]
    fn deserialization_rejects_non_numeric_subscription_qos() {
        for qos in [r#""1""#, r#""AtMostOnce""#, "null", "true", "[]", "{}", "1e400"] {
            let json = format!(r#"{{"topic":"home/#","qos":{qos}}}"#);
            assert!(serde_json::from_str::<TopicSubscription>(&json).is_err(), "accepted QoS {qos}");
        }
    }

    #[test]
    fn invalid_numeric_subscription_qos_defaults_to_zero() {
        for qos in [
            "3",
            "255",
            "256",
            "-1",
            "-256",
            "1.5",
            "18446744073709551616",
            "-18446744073709551616",
            "1e100",
        ] {
            let json = format!(r#"{{"topic":"home/#","qos":{qos}}}"#);
            let subscription: TopicSubscription = serde_json::from_str(&json).unwrap();
            assert_eq!(subscription.qos, Qos::AtMostOnce, "QoS {qos}");
            assert_eq!(serde_json::to_value(&subscription).unwrap()["qos"], 0);
        }
    }

    #[test]
    fn integral_float_subscription_qos_preserves_valid_levels() {
        for (number, expected) in [("0.0", Qos::AtMostOnce), ("1.0", Qos::AtLeastOnce), ("2e0", Qos::ExactlyOnce)] {
            let json = format!(r#"{{"topic":"home/#","qos":{number}}}"#);
            let subscription: TopicSubscription = serde_json::from_str(&json).unwrap();
            assert_eq!(subscription.qos, expected);
        }
    }

    #[test]
    fn all_subscription_qos_levels_roundtrip_as_numeric_json() {
        for (qos, numeric) in [(Qos::AtMostOnce, 0), (Qos::AtLeastOnce, 1), (Qos::ExactlyOnce, 2)] {
            let subscription = TopicSubscription {
                topic: "home/#".into(),
                qos,
            };
            let json = serde_json::to_value(&subscription).unwrap();
            assert_eq!(json, serde_json::json!({"topic":"home/#","qos":numeric}));
            assert_eq!(serde_json::from_value::<TopicSubscription>(json).unwrap(), subscription);
        }
    }

    #[test]
    fn connection_config_deserialization_rejects_non_numeric_qos_without_using_legacy_defaults() {
        for qos in [r#""1""#, r#""AtMostOnce""#, "null", "true", "[]", "{}", "1e400"] {
            let json = format!(r#"{{"base_topic":"legacy/#","topics":[{{"topic":"home/#","qos":{qos}}}]}}"#);
            assert!(serde_json::from_str::<ConnectionConfig>(&json).is_err(), "accepted QoS {qos}");
        }
    }

    #[test]
    fn connection_config_qos_roundtrip_preserves_numeric_topics_format() {
        let config = ConnectionConfig {
            topics: vec![
                TopicSubscription {
                    topic: "home/#".into(),
                    qos: Qos::AtMostOnce,
                },
                TopicSubscription {
                    topic: "office/#".into(),
                    qos: Qos::AtLeastOnce,
                },
                TopicSubscription {
                    topic: "$SYS/#".into(),
                    qos: Qos::ExactlyOnce,
                },
            ],
            ..Default::default()
        };
        let json = serde_json::to_value(&config).unwrap();
        assert_eq!(
            json["topics"],
            serde_json::json!([
                {"topic":"home/#","qos":0},
                {"topic":"office/#","qos":1},
                {"topic":"$SYS/#","qos":2},
            ])
        );
        assert!(json.get("base_topic").is_none());
        assert!(serde_json::from_value::<ConnectionConfig>(json).unwrap() == config);
    }

    #[test]
    fn validate_requires_topics_and_rejects_duplicate_filters_regardless_of_qos() {
        for topics in [
            vec![],
            vec![TopicSubscription::default(), TopicSubscription::default()],
            vec![
                TopicSubscription::default(),
                TopicSubscription {
                    topic: "#".into(),
                    qos: Qos::ExactlyOnce,
                },
            ],
        ] {
            let config = ConnectionConfig {
                topics,
                ..Default::default()
            };
            assert_eq!(config.validate().unwrap_err().field(), ConnectionField::Topics);
        }
        let config = ConnectionConfig {
            topics: vec![
                TopicSubscription::default(),
                TopicSubscription {
                    topic: "home/#".into(),
                    qos: Qos::AtLeastOnce,
                },
            ],
            ..Default::default()
        };
        assert!(config.validate().is_ok(), "overlapping but distinct filters are allowed");
    }

    #[test]
    fn serialization_never_includes_the_runtime_password() {
        let json = serde_json::to_value(authenticated_config()).unwrap();
        assert!(json.get("password").is_none());
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
    fn tcp_and_tls_credential_keys_keep_the_legacy_format() {
        let mut config = authenticated_config();
        assert_eq!(credential_account(&config), "9:localhost:1883:false:9:mqtt-user");
        config.tls = true;
        assert_eq!(credential_account(&config), "9:localhost:1883:true:9:mqtt-user");
    }

    #[test]
    fn websocket_credentials_are_separate_for_each_protocol() {
        let mut config = authenticated_config();
        let tcp = credential_account(&config);
        config.tls = true;
        let tls = credential_account(&config);
        config.websocket = true;
        let websocket = credential_account(&config);
        assert_eq!(websocket, "9:localhost:1883:wss:9:mqtt-user");
        assert_ne!(websocket, tcp);
        assert_ne!(websocket, tls);
        config.tls = false;
        let unencrypted_websocket = credential_account(&config);
        assert_eq!(unencrypted_websocket, "9:localhost:1883:ws:9:mqtt-user");
        assert_ne!(unencrypted_websocket, websocket);
        assert_ne!(unencrypted_websocket, tcp);
        assert_ne!(unencrypted_websocket, tls);
    }

    #[test]
    fn legacy_saved_protocols_restore_passwords_and_persist_websocket_false() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.json");
        let entries = mock_entries();
        let tcp = authenticated_config();
        let tls = ConnectionConfig {
            tls: true,
            password: "tls-secret".into(),
            ..tcp.clone()
        };
        entries(&tcp).unwrap().set_password(&tcp.password).unwrap();
        entries(&tls).unwrap().set_password(&tls.password).unwrap();
        fs::write(&path, r#"{"connections":[{"host":"localhost","username":"mqtt-user","password_in_keyring":true},{"host":"localhost","username":"mqtt-user","tls":true,"password_in_keyring":true}]}"#).unwrap();
        let saved = load_connections_from(&path, &entries).unwrap();
        assert_eq!(saved.connections[0].password, tcp.password);
        assert_eq!(saved.connections[1].password, tls.password);
        assert_eq!(saved.connections[0].protocol(), "mqtt://");
        assert_eq!(saved.connections[1].protocol(), "mqtts://");
        save_connections_to(&path, &saved, &entries).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert!(
            json["connections"]
                .as_array()
                .unwrap()
                .iter()
                .all(|config| config.get("host").is_none()
                    && config.get("port").is_none()
                    && config.get("tls").is_none()
                    && config.get("websocket").is_none())
        );
        assert_eq!(json["connections"][0]["connection"], "mqtt://mqtt-user@localhost:1883");
        assert_eq!(json["connections"][1]["connection"], "mqtts://mqtt-user@localhost:1883");
        assert!(json["connections"][0].get("username").is_none());
        assert!(json["connections"][0].get("password_in_keyring").is_none());
    }

    #[test]
    fn legacy_saved_connections_default_to_validating_certificates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.json");
        let entries = mock_entries();
        fs::write(&path, r#"{"connections":[{"tls":true}]}"#).unwrap();
        let saved = load_connections_from(&path, &entries).unwrap();
        assert!(saved.connections[0].validate_certificate);
        save_connections_to(&path, &saved, &entries).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(json["connections"][0]["validate_certificate"], true);
    }

    #[test]
    fn disabled_certificate_validation_persists_without_changing_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.json");
        let entries = mock_entries();
        let mut config = ConnectionConfig {
            tls: true,
            ..authenticated_config()
        };
        let account = credential_account(&config);
        save_connections_to(
            &path,
            &SavedConnections {
                connections: vec![config.clone()],
                selected: Some(0),
            },
            &entries,
        )
        .unwrap();
        config.validate_certificate = false;
        assert_eq!(credential_account(&config), account);
        save_connections_to(
            &path,
            &SavedConnections {
                connections: vec![config.clone()],
                selected: Some(0),
            },
            &entries,
        )
        .unwrap();
        let loaded = load_connections_from(&path, &entries).unwrap();
        assert!(!loaded.connections[0].validate_certificate);
        assert_eq!(loaded.connections[0].password, config.password);
        assert_eq!(loaded.selected, Some(0));
        let json: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(json["connections"][0]["validate_certificate"], false);
        assert!(json["connections"][0].get("password").is_none());
    }

    #[test]
    fn websocket_roundtrip_and_removal_leave_tcp_and_tls_credentials_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.json");
        let entries = mock_entries();
        let tcp = authenticated_config();
        let tls = ConnectionConfig {
            name: "Home TLS".into(),
            tls: true,
            password: "tls-secret".into(),
            ..tcp.clone()
        };
        let websocket = ConnectionConfig {
            name: "Home WebSocket".into(),
            websocket: true,
            password: "ws-secret".into(),
            ..tcp.clone()
        };
        let saved = SavedConnections {
            connections: vec![tcp.clone(), tls.clone(), websocket.clone()],
            selected: Some(2),
        };
        save_connections_to(&path, &saved, &entries).unwrap();
        let mut loaded = load_connections_from(&path, &entries).unwrap();
        assert_eq!(loaded.selected, Some(2));
        assert!(loaded.connections[2].websocket);
        assert!(!loaded.connections[2].tls);
        assert_eq!(loaded.connections[2].password, websocket.password);
        let json: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(json["connections"][2]["connection"], "ws://mqtt-user@localhost:1883");
        assert!(
            json["connections"]
                .as_array()
                .unwrap()
                .iter()
                .all(|config| config.get("password").is_none())
        );
        loaded.connections.pop();
        loaded.selected = Some(0);
        save_connections_to(&path, &loaded, &entries).unwrap();
        assert!(matches!(entries(&websocket).unwrap().get_password(), Err(KeyringError::NoEntry)));
        assert_eq!(entries(&tcp).unwrap().get_password().unwrap(), tcp.password);
        assert_eq!(entries(&tls).unwrap().get_password().unwrap(), tls.password);
    }

    #[test]
    fn connection_name_does_not_change_the_credential_account() {
        let config = authenticated_config();
        let renamed = ConnectionConfig {
            name: "Renamed".into(),
            ..config.clone()
        };
        assert_eq!(credential_account(&renamed), credential_account(&config));
    }

    #[test]
    fn empty_store_has_no_connections_or_selection() {
        let dir = tempfile::tempdir().unwrap();
        let saved = load_connections_from(&dir.path().join("connection.json"), &mock_entries()).unwrap();
        assert!(saved.connections.is_empty());
        assert_eq!(saved.selected, None);
    }

    #[test]
    fn application_configuration_uses_a_single_config_file() {
        assert_eq!(config_path().unwrap().file_name().unwrap(), "config.json");
    }

    #[test]
    fn state_configuration_path_takes_precedence_over_config_directory() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join(".config/topq/config.json");
        let state = directory.path().join(".local/state/topq/config.json");
        fs::create_dir_all(config.parent().unwrap()).unwrap();
        fs::create_dir_all(state.parent().unwrap()).unwrap();
        fs::write(&config, r#"{"mode":"dark"}"#).unwrap();
        fs::write(&state, r#"{"mode":"light"}"#).unwrap();

        assert_eq!(prioritized_config_path(&state, &config).unwrap(), state);
    }

    #[cfg(unix)]
    #[test]
    fn read_only_config_file_uses_the_state_directory() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join(".config/topq/config.json");
        let state = directory.path().join(".local/state/topq/config.json");
        fs::create_dir_all(config.parent().unwrap()).unwrap();
        fs::write(&config, "{}").unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o444)).unwrap();

        assert_eq!(writable_config_path(&config, &state).unwrap(), state);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_to_read_only_config_uses_the_state_directory() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join(".config/topq/config.json");
        let target = directory.path().join("store/config.json");
        let state = directory.path().join(".local/state/topq/config.json");
        fs::create_dir_all(config.parent().unwrap()).unwrap();
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, "{}").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o444)).unwrap();
        symlink(&target, &config).unwrap();

        assert_eq!(writable_config_path(&config, &state).unwrap(), state);
    }

    #[cfg(unix)]
    #[test]
    fn reports_an_error_when_neither_configuration_path_is_writable() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join(".config/topq/config.json");
        let state = directory.path().join(".local/state/topq/config.json");
        fs::create_dir_all(config.parent().unwrap()).unwrap();
        fs::create_dir_all(state.parent().unwrap()).unwrap();
        fs::write(&config, "{}").unwrap();
        fs::write(&state, "{}").unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o444)).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o444)).unwrap();

        let error = writable_config_path(&config, &state).unwrap_err();
        assert!(matches!(error, ConfigError::ConfigNotWritable { .. }));
        assert_eq!(error.to_string(), "config.not_writable");
        assert_eq!(error.path(), Some(state.as_path()));
        assert_eq!(error.alternative_path(), None);
    }

    #[cfg(unix)]
    #[test]
    fn unwritable_config_and_state_directories_retain_both_paths() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("config/config.json");
        let state = directory.path().join("state/config.json");
        for path in [&config, &state] {
            let parent = path.parent().unwrap();
            fs::create_dir_all(parent).unwrap();
            fs::set_permissions(parent, fs::Permissions::from_mode(0o555)).unwrap();
        }
        let error = writable_config_path(&config, &state).unwrap_err();
        for path in [&config, &state] {
            fs::set_permissions(path.parent().unwrap(), fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert_eq!(error.code(), "config.not_writable");
        assert_eq!(error.path(), Some(config.as_path()));
        assert_eq!(error.alternative_path(), Some(state.as_path()));
        assert!(format!("{error:?}").contains("alternative_path:"));
    }

    #[test]
    fn config_inspection_failure_retains_path_and_io_source() {
        use std::error::Error as _;

        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("config.json");
        let state = directory.path().join("not-a-directory/config.json");
        fs::write(state.parent().unwrap(), "").unwrap();
        for error in [
            prioritized_config_path(&state, &config).unwrap_err(),
            writable_config_path(&config, &state).unwrap_err(),
        ] {
            assert!(matches!(error, ConfigError::InspectConfig { .. }));
            assert_eq!(error.to_string(), "config.inspect_failed");
            assert_eq!(error.path(), Some(state.as_path()));
            assert!(error.source().unwrap().downcast_ref::<std::io::Error>().is_some());
        }
    }

    #[test]
    fn application_write_failure_preserves_safe_diagnostics_and_retry_rules() {
        use std::error::Error as _;

        let error = ConfigError::WriteApplicationConfig {
            path: "config.json".into(),
            alternative_path: Some("state/config.json".into()),
            source: Box::new(ConfigError::SaveConfig {
                path: "config.json".into(),
                temporary_path: "config.json.tmp".into(),
                source: std::io::ErrorKind::PermissionDenied.into(),
            }),
        };
        assert_eq!(error.to_string(), "config.application.write_failed");
        assert_eq!(error.path(), Some(Path::new("config.json")));
        assert_eq!(error.alternative_path(), Some(Path::new("state/config.json")));
        assert_eq!(error.temporary_path(), Some(Path::new("config.json.tmp")));
        assert_eq!(error.source().unwrap().to_string(), "config.save_failed");
        assert!(is_io_error(&error));

        let error = ConfigError::SaveRollbackFailed {
            source: Box::new(error),
            rollback: Box::new(ConfigError::RollbackCredentials {
                failures: vec![ConfigError::CredentialAccessDenied],
            }),
        };
        assert!(!is_io_error(&error), "failed credential rollback must not trigger a write retry");
        assert_eq!(error.alternative_path(), Some(Path::new("state/config.json")));
        let debug = format!("{error:#?}");
        assert!(debug.contains("config.save_failed"));
        assert!(debug.contains("config.credentials.access_denied"));
    }

    #[test]
    fn migrating_config_to_state_preserves_existing_settings() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join(".config/topq/config.json");
        let state = directory.path().join(".local/state/topq/config.json");
        fs::create_dir_all(config.parent().unwrap()).unwrap();
        fs::write(&config, r#"{"mode":"dark","connections":[]}"#).unwrap();

        update_json_with_fallback(&state, Some(&config), &serde_json::json!({"light_theme":"Ayu Light"})).unwrap();

        let saved = read_config_value(&state).unwrap().unwrap();
        assert_eq!(saved["mode"], "dark");
        assert_eq!(saved["connections"], serde_json::json!([]));
        assert_eq!(saved["light_theme"], "Ayu Light");
    }

    #[test]
    fn connection_save_to_state_preserves_config_preferences() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join(".config/topq/config.json");
        let state = directory.path().join(".local/state/topq/config.json");
        fs::create_dir_all(config.parent().unwrap()).unwrap();
        fs::write(&config, r#"{"mode":"dark","connections":[]}"#).unwrap();
        let saved = SavedConnections {
            connections: vec![ConnectionConfig {
                name: "State connection".into(),
                ..Default::default()
            }],
            selected: Some(0),
        };

        save_connections_to_with_source(&state, &config, &saved, &mock_entries()).unwrap();

        let value = read_config_value(&state).unwrap().unwrap();
        assert_eq!(value["mode"], "dark");
        assert_eq!(value["connections"][0]["name"], "State connection");
    }

    #[test]
    fn connection_migration_removes_credentials_from_the_source_configuration() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("config/config.json");
        let state = directory.path().join("state/config.json");
        let entries = mock_entries();
        let original = authenticated_config();
        save_connections_to(
            &config,
            &SavedConnections {
                connections: vec![original.clone()],
                selected: Some(0),
            },
            &entries,
        )
        .unwrap();
        let before = fs::read(&config).unwrap();
        save_connections_to_with_source(&state, &config, &SavedConnections::default(), &entries).unwrap();
        assert!(matches!(entries(&original).unwrap().get_password(), Err(KeyringError::NoEntry)));
        assert_eq!(fs::read(&config).unwrap(), before);
        assert!(load_connections_from(&state, &entries).unwrap().connections.is_empty());
    }

    #[test]
    fn application_config_parse_failure_preserves_its_variant_and_existing_file() {
        let config = config_path().unwrap();
        let state = state_config_path().unwrap();
        fs::create_dir_all(config.parent().unwrap()).unwrap();
        let malformed = "{\n\"password\": \"parser-secret\",\n";
        fs::write(&config, malformed).unwrap();

        let error = update_application_config(&config, &serde_json::json!({"mode":"dark"})).unwrap_err();
        assert!(matches!(error, ConfigError::ParseConfig { .. }));
        assert_eq!(error.code(), "config.parse_failed");
        assert_eq!(error.path(), Some(config.as_path()));
        assert!(!format!("{error:#?}").contains("parser-secret"));
        assert_eq!(fs::read_to_string(&config).unwrap(), malformed);
        assert!(!state.exists());
    }

    #[test]
    fn application_config_serialization_failure_preserves_its_variant_and_existing_file() {
        use std::error::Error as _;

        struct SecretSerializer;
        impl Serialize for SecretSerializer {
            fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("serializer-secret"))
            }
        }
        let config = config_path().unwrap();
        let state = state_config_path().unwrap();
        fs::create_dir_all(config.parent().unwrap()).unwrap();
        let before = r#"{"mode":"dark","connections":[]}"#;
        fs::write(&config, before).unwrap();

        let error = update_application_config(&config, &SecretSerializer).unwrap_err();
        assert!(matches!(error, ConfigError::SerializeConfig { .. }));
        assert_eq!(error.code(), "config.serialize_failed");
        assert_eq!(error.path(), Some(config.as_path()));
        assert!(!format!("{error:#?}").contains("serializer-secret"));
        assert_eq!(error.source().unwrap().to_string(), "config.diagnostic.json_data");
        assert_eq!(fs::read_to_string(&config).unwrap(), before);
        assert!(!state.exists());
    }

    #[test]
    fn application_config_fallback_serialization_failure_preserves_its_variant() {
        struct FailsOnRetry(std::cell::Cell<bool>);
        impl Serialize for FailsOnRetry {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                if self.0.replace(true) {
                    return Err(serde::ser::Error::custom("retry-serializer-secret"));
                }
                serde_json::json!({"mode":"light"}).serialize(serializer)
            }
        }
        let config = config_path().unwrap();
        let state = state_config_path().unwrap();
        fs::create_dir_all(config.parent().unwrap()).unwrap();
        let before = r#"{"mode":"dark","connections":[]}"#;
        fs::write(&config, before).unwrap();
        fs::create_dir(config.with_extension("json.tmp")).unwrap();

        let error = update_application_config(&config, &FailsOnRetry(std::cell::Cell::new(false))).unwrap_err();
        assert!(matches!(error, ConfigError::SerializeConfig { .. }));
        assert_eq!(error.path(), Some(state.as_path()));
        assert!(!format!("{error:#?}").contains("retry-serializer-secret"));
        assert_eq!(fs::read_to_string(&config).unwrap(), before);
        assert!(!state.exists());
    }

    #[test]
    fn connection_fallback_rollback_failure_preserves_its_top_level_variant() {
        let config = config_path().unwrap();
        let state = state_config_path().unwrap();
        let entries = mock_entries();
        let original = authenticated_config();
        save_connections_to(
            &config,
            &SavedConnections {
                connections: vec![original.clone()],
                selected: Some(0),
            },
            &entries,
        )
        .unwrap();
        let before = fs::read(&config).unwrap();
        fs::create_dir(config.with_extension("json.tmp")).unwrap();
        fs::create_dir_all(state.with_extension("json.tmp")).unwrap();
        let calls = std::cell::Cell::new(0);
        let failing_entries = |config: &ConnectionConfig| {
            calls.set(calls.get() + 1);
            // Each attempt snapshots, updates, then restores the credential.
            if calls.get() == 6 {
                Err(ConfigError::OpenCredentialStore {
                    source: Box::new(ConfigError::CredentialAccessDenied),
                })
            } else {
                entries(config)
            }
        };
        let changed = ConnectionConfig {
            password: "changed-secret".into(),
            ..original
        };

        let error = save_connections_with_entry(
            &SavedConnections {
                connections: vec![changed],
                selected: Some(0),
            },
            &failing_entries,
        )
        .unwrap_err();
        assert_eq!(error.code(), "config.save_rollback_failed");
        assert_eq!(error.path(), Some(state.with_extension("json.tmp").as_path()));
        assert!(!format!("{error:#?}").contains("changed-secret"));
        let ConfigError::SaveRollbackFailed { source, rollback } = error else {
            panic!("expected a top-level fallback rollback failure")
        };
        assert!(matches!(*source, ConfigError::OpenTemporaryConfig { .. }));
        assert!(matches!(*rollback, ConfigError::RollbackCredentials { .. }));
        assert_eq!(calls.get(), 6);
        assert_eq!(fs::read(&config).unwrap(), before);
        assert!(!state.exists());
    }

    #[test]
    fn application_writes_retry_in_state_and_preserve_existing_settings() {
        let config = config_path().unwrap();
        let state = state_config_path().unwrap();
        fs::create_dir_all(config.parent().unwrap()).unwrap();
        fs::write(&config, r#"{"mode":"dark","connections":[]}"#).unwrap();
        fs::create_dir(config.with_extension("json.tmp")).unwrap();

        let destination = update_application_config(&config, &serde_json::json!({"reduce_motion":"on"})).unwrap();
        assert_eq!(destination, state);
        let value = read_config_value(&state).unwrap().unwrap();
        assert_eq!(value["mode"], "dark");
        assert_eq!(value["connections"], serde_json::json!([]));
        assert_eq!(value["reduce_motion"], "on");
        assert_eq!(application_config_read_path().unwrap(), state);
    }

    #[test]
    fn connection_writes_retry_in_state_after_restoring_credentials() {
        let config = config_path().unwrap();
        let state = state_config_path().unwrap();
        let original = authenticated_config();
        save_connections(&SavedConnections {
            connections: vec![original.clone()],
            selected: Some(0),
        })
        .unwrap();
        update_json(&config, &serde_json::json!({"mode":"dark"})).unwrap();
        let before = fs::read(&config).unwrap();
        fs::create_dir(config.with_extension("json.tmp")).unwrap();

        let changed = ConnectionConfig {
            password: "changed-secret".into(),
            ..original
        };
        save_connections(&SavedConnections {
            connections: vec![changed.clone()],
            selected: Some(0),
        })
        .unwrap();
        assert_eq!(application_config_read_path().unwrap(), state);
        assert_eq!(read_config_value(&state).unwrap().unwrap()["mode"], "dark");
        assert_eq!(load_connections().unwrap().connections[0].password, changed.password);
        assert_eq!(fs::read(&config).unwrap(), before);
    }

    #[test]
    fn unified_config_updates_preserve_both_settings_sections() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        fs::write(
            &path,
            r#"{"mode":"dark","reduce_motion":"on","connections":[],"active_connection":null}"#,
        )
        .unwrap();

        update_json(&path, &serde_json::json!({"light_theme":"Ayu Light"})).unwrap();
        update_json(
            &path,
            &serde_json::json!({"connections":[{"name":"Home"}],"active_connection":"Home"}),
        )
        .unwrap();
        let saved = read_config_value(&path).unwrap().unwrap();
        assert_eq!(saved["mode"], "dark");
        assert_eq!(saved["reduce_motion"], "on");
        assert_eq!(saved["light_theme"], "Ayu Light");
        assert_eq!(saved["connections"][0]["name"], "Home");
        assert_eq!(saved["active_connection"], "Home");
    }

    #[test]
    fn application_persistence_uses_temporary_configuration_and_mock_credentials() {
        let path = config_path().unwrap();
        TEST_CONFIG_DIR.with(|dir| {
            assert_eq!(path, dir.path().join("topq").join("config.json"));
            assert!(path.starts_with(dir.path()));
        });
        let config = authenticated_config();
        let entry = password_entry(&config).unwrap();
        assert!(entry.inner.as_any().is::<mock::Cred>(), "tests must not use a system credential");
        assert!(matches!(entry.get_password(), Err(KeyringError::NoEntry)));
        save_connections(&SavedConnections {
            connections: vec![config.clone()],
            selected: Some(0),
        })
        .unwrap();
        assert!(path.is_file());
        assert_eq!(load_connections().unwrap().connections[0].password, config.password);
        assert_eq!(entry.get_password().unwrap(), config.password);
        save_connections(&SavedConnections::default()).unwrap();
        assert!(load_connections().unwrap().connections.is_empty());
        assert!(matches!(entry.get_password(), Err(KeyringError::NoEntry)));
    }

    #[test]
    fn application_test_storage_is_thread_local_and_temporary_files_are_cleaned_up() {
        let config = authenticated_config();
        let path = config_path().unwrap();
        TEST_CONFIG_DIR.with(|dir| assert!(path.starts_with(dir.path())));
        let entry = password_entry(&config).unwrap();
        assert!(entry.inner.as_any().is::<mock::Cred>());
        entry.set_password("parent-thread-secret").unwrap();
        let other_path = std::thread::spawn(move || {
            let other_path = config_path().unwrap();
            TEST_CONFIG_DIR.with(|dir| assert!(other_path.starts_with(dir.path())));
            let other_entry = password_entry(&config).unwrap();
            assert!(other_entry.inner.as_any().is::<mock::Cred>());
            assert!(matches!(other_entry.get_password(), Err(KeyringError::NoEntry)));
            assert!(load_connections().unwrap().connections.is_empty());
            save_connections(&SavedConnections {
                connections: vec![config.clone()],
                selected: Some(0),
            })
            .unwrap();
            assert_eq!(load_connections().unwrap().connections[0].password, config.password);
            assert!(other_path.is_file());
            other_path
        })
        .join()
        .unwrap();
        assert_ne!(other_path, path);
        assert!(
            !other_path.parent().unwrap().exists(),
            "temporary configuration must be cleaned up on thread exit"
        );
        assert_eq!(entry.get_password().unwrap(), "parent-thread-secret");
        assert!(load_connections().unwrap().connections.is_empty());
    }

    #[test]
    fn topics_width_roundtrips_through_toml_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("topq").join("state.toml");
        save_topics_width_to(&path, 31.5, 0.4).unwrap();
        assert_eq!(
            load_topics_layout_from(&path).unwrap(),
            TopicsLayout {
                width_rem: Some(31.5),
                width_fraction: Some(0.4),
            }
        );
        assert_eq!(
            fs::read_to_string(path).unwrap(),
            "topics_width_rem = 31.5\ntopics_width_fraction = 0.4\n"
        );
    }

    #[test]
    fn publish_height_roundtrips_through_toml_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("topq").join("state.toml");
        for fraction in [0., 0.25, 1.] {
            save_publish_height_to(&path, 18.5, fraction).unwrap();
            assert_eq!(
                load_publish_layout_from(&path).unwrap(),
                PublishLayout {
                    height_rem: Some(18.5),
                    height_fraction: Some(fraction),
                    ..PublishLayout::default()
                }
            );
        }
        let state = fs::read_to_string(path).unwrap();
        assert_eq!(state, "publish_height_rem = 18.5\npublish_height_fraction = 1.0\n");
    }

    #[test]
    fn publish_open_roundtrips_without_losing_pane_sizes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("topq").join("state.toml");
        save_topics_width_to(&path, 31.5, 0.4).unwrap();
        save_publish_height_to(&path, 18.5, 0.25).unwrap();
        assert!(!load_publish_layout_from(&path).unwrap().open);
        for open in [true, false] {
            save_publish_open_to(&path, open).unwrap();
            assert_eq!(
                load_publish_layout_from(&path).unwrap(),
                PublishLayout {
                    open,
                    height_rem: Some(18.5),
                    height_fraction: Some(0.25),
                }
            );
            assert_eq!(
                load_topics_layout_from(&path).unwrap(),
                TopicsLayout {
                    width_rem: Some(31.5),
                    width_fraction: Some(0.4),
                }
            );
            save_topics_width_to(&path, 31.5, 0.4).unwrap();
            save_publish_height_to(&path, 18.5, 0.25).unwrap();
            assert_eq!(load_publish_layout_from(&path).unwrap().open, open);
        }
    }

    #[test]
    fn interleaved_layout_saves_preserve_both_panes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.toml");
        save_topics_width_to(&path, 31.5, 0.4).unwrap();
        save_publish_height_to(&path, 18.5, 0.25).unwrap();
        assert_eq!(
            load_topics_layout_from(&path).unwrap(),
            TopicsLayout {
                width_rem: Some(31.5),
                width_fraction: Some(0.4),
            }
        );
        save_topics_width_to(&path, 28., 0.3).unwrap();
        assert_eq!(
            load_publish_layout_from(&path).unwrap(),
            PublishLayout {
                height_rem: Some(18.5),
                height_fraction: Some(0.25),
                ..PublishLayout::default()
            }
        );
        save_publish_height_to(&path, 20., 0.5).unwrap();
        assert_eq!(
            load_topics_layout_from(&path).unwrap(),
            TopicsLayout {
                width_rem: Some(28.),
                width_fraction: Some(0.3),
            }
        );
        assert_eq!(
            load_publish_layout_from(&path).unwrap(),
            PublishLayout {
                height_rem: Some(20.),
                height_fraction: Some(0.5),
                ..PublishLayout::default()
            }
        );
    }

    #[test]
    fn legacy_topics_only_state_defaults_publish_layout() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.toml");
        fs::write(&path, "topics_width_rem = 31.5\ntopics_width_fraction = 0.4\n").unwrap();
        assert_eq!(load_publish_layout_from(&path).unwrap(), PublishLayout::default());
        assert_eq!(
            load_topics_layout_from(&path).unwrap(),
            TopicsLayout {
                width_rem: Some(31.5),
                width_fraction: Some(0.4),
            }
        );
    }

    #[test]
    fn state_path_is_stable_within_a_thread_and_isolated_between_threads() {
        let path = state_path().unwrap();
        assert_eq!(state_path().unwrap(), path);
        let other_path = std::thread::spawn(|| {
            let path = state_path().unwrap();
            assert_eq!(state_path().unwrap(), path);
            path
        })
        .join()
        .unwrap();
        assert_ne!(path, other_path);
    }

    #[test]
    fn missing_layout_state_defaults_both_panes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.toml");
        assert_eq!(load_topics_layout_from(&path).unwrap(), TopicsLayout::default());
        assert_eq!(load_publish_layout_from(&path).unwrap(), PublishLayout::default());
        assert!(!path.exists());
    }

    #[test]
    fn invalid_optional_layout_sizes_are_filtered_independently() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.toml");
        for invalid in [0., -1., f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            write_toml(
                &path,
                &LayoutPreferences {
                    topics_width_rem: Some(invalid),
                    topics_width_fraction: Some(0.4),
                    publish_height_rem: Some(invalid),
                    publish_height_fraction: Some(0.25),
                    ..LayoutPreferences::default()
                },
            )
            .unwrap();
            assert_eq!(
                load_topics_layout_from(&path).unwrap(),
                TopicsLayout {
                    width_rem: None,
                    width_fraction: Some(0.4),
                }
            );
            assert_eq!(
                load_publish_layout_from(&path).unwrap(),
                PublishLayout {
                    height_rem: None,
                    height_fraction: Some(0.25),
                    ..PublishLayout::default()
                }
            );
        }
    }

    #[test]
    fn invalid_optional_layout_fractions_are_filtered_independently() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.toml");
        for invalid in [-0.1, 1.1, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            write_toml(
                &path,
                &LayoutPreferences {
                    topics_width_rem: Some(31.5),
                    topics_width_fraction: Some(invalid),
                    publish_height_rem: Some(18.5),
                    publish_height_fraction: Some(invalid),
                    ..LayoutPreferences::default()
                },
            )
            .unwrap();
            assert_eq!(
                load_topics_layout_from(&path).unwrap(),
                TopicsLayout {
                    width_rem: Some(31.5),
                    width_fraction: None,
                }
            );
            assert_eq!(
                load_publish_layout_from(&path).unwrap(),
                PublishLayout {
                    height_rem: Some(18.5),
                    height_fraction: None,
                    ..PublishLayout::default()
                }
            );
        }
    }

    #[test]
    fn layout_saves_leave_malformed_state_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.toml");
        for original in ["topics_width_rem = [", "publish_height_rem = \"invalid\""] {
            fs::write(&path, original).unwrap();
            assert!(save_topics_width_to(&path, 31.5, 0.4).is_err());
            assert_eq!(fs::read_to_string(&path).unwrap(), original);
            assert!(save_publish_height_to(&path, 18.5, 0.25).is_err());
            assert_eq!(fs::read_to_string(&path).unwrap(), original);
            assert!(save_publish_open_to(&path, true).is_err());
            assert_eq!(fs::read_to_string(&path).unwrap(), original);
            assert!(!path.with_extension("toml.tmp").exists());
        }
    }

    #[test]
    fn layout_saves_propagate_read_errors_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.toml");
        fs::create_dir(&path).unwrap();
        assert!(save_topics_width_to(&path, 31.5, 0.4).is_err());
        assert!(save_publish_height_to(&path, 18.5, 0.25).is_err());
        assert!(save_publish_open_to(&path, true).is_err());
        assert!(path.is_dir());
        assert!(!path.with_extension("toml.tmp").exists());
    }

    #[test]
    fn saved_connections_migrate_legacy_filters_and_ignore_plaintext_passwords() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.json");
        fs::write(
            &path,
            r#"{"connections":[{"name":"Legacy","host":"legacy-broker","base_topic":"home/#","password":"plaintext-secret"},{"host":"default-broker"}],"active_connection":"Legacy"}"#,
        ).unwrap();
        let entries = mock_entries();
        let saved = load_connections_from(&path, &entries).unwrap();
        assert_eq!(saved.selected, Some(0));
        assert_eq!(
            saved.connections[0].topics,
            vec![TopicSubscription {
                topic: "home/#".into(),
                qos: Qos::ExactlyOnce
            }]
        );
        assert_eq!(saved.connections[1].topics, vec![TopicSubscription::default()]);
        assert!(saved.connections.iter().all(|config| config.password.is_empty()));
        save_connections_to(&path, &saved, &entries).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(json["active_connection"], "Legacy");
        assert!(json.get("selected").is_none());
        assert!(
            json["connections"]
                .as_array()
                .unwrap()
                .iter()
                .all(|config| { config.get("base_topic").is_none() && config.get("password").is_none() && config.get("topics").is_some() })
        );
        let loaded = load_connections_from(&path, &entries).unwrap();
        assert_eq!(loaded.connections[0].topics, saved.connections[0].topics);
    }

    #[test]
    fn single_connection_shape_is_not_accepted_as_multi_connection_storage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.json");
        let original = br#"{"host":"localhost","port":1883,"password_in_keyring":true}"#;
        fs::write(&path, original).unwrap();
        let entries = |_: &ConnectionConfig| -> Result<Entry, ConfigError> { panic!("invalid format must not access keyring") };
        assert!(load_connections_from(&path, &entries).is_err());
        assert!(save_connections_to(&path, &SavedConnections::default(), &entries).is_err());
        assert_eq!(fs::read(path).unwrap(), original);
    }

    #[test]
    fn non_numeric_saved_qos_is_rejected_without_changing_config_or_accessing_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let entries = |_: &ConnectionConfig| -> Result<Entry, ConfigError> { panic!("invalid QoS must not access keyring") };
        for qos in [r#""1""#, "null", "true", "[]", "{}"] {
            let original = format!(
                r#"{{"mode":"dark","connections":[{{"name":"Invalid","host":"broker.example","topics":[{{"topic":"home/#","qos":{qos}}}]}}],"active_connection":"Invalid"}}"#,
            );
            fs::write(&path, &original).unwrap();
            assert!(matches!(
                load_connections_from(&path, &entries),
                Err(ConfigError::ParseConnections { .. })
            ));
            assert!(matches!(
                save_connections_to(&path, &SavedConnections::default(), &entries),
                Err(ConfigError::ParseConnections { .. })
            ));
            assert_eq!(fs::read(&path).unwrap(), original.as_bytes());
        }
    }

    #[test]
    fn invalid_numeric_saved_qos_loads_all_connections_and_normalizes_on_save() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let entries = mock_entries();
        for qos in ["3", "255", "256", "-1", "1.5", "18446744073709551616"] {
            let original = format!(
                r#"{{"mode":"dark","connections":[{{"name":"Recovered","host":"broker.example","topics":[{{"topic":"home/#","qos":{qos}}}]}},{{"name":"Valid","host":"other.example","topics":[{{"topic":"office/#","qos":2}}]}}],"active_connection":"Recovered"}}"#,
            );
            fs::write(&path, &original).unwrap();
            let saved = load_connections_from(&path, &entries).unwrap();
            assert_eq!(saved.connections.len(), 2);
            assert_eq!(saved.selected, Some(0));
            assert_eq!(saved.connections[0].topics[0].qos, Qos::AtMostOnce);
            assert_eq!(saved.connections[1].topics[0].qos, Qos::ExactlyOnce);
            assert_eq!(fs::read(&path).unwrap(), original.as_bytes());

            save_connections_to(&path, &saved, &entries).unwrap();
            let json: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            assert_eq!(json["connections"][0]["topics"][0]["qos"], 0);
            assert_eq!(json["connections"][1]["topics"][0]["qos"], 2);
            assert_eq!(json["active_connection"], "Recovered");
            assert_eq!(json["mode"], "dark");
        }
    }

    #[test]
    fn multi_connection_roundtrip_keeps_passwords_out_of_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connection.json");
        let entries = mock_entries();
        let first = ConnectionConfig {
            topics: vec![
                TopicSubscription {
                    topic: "home/#".into(),
                    qos: Qos::AtMostOnce,
                },
                TopicSubscription {
                    topic: "office/+/temperature".into(),
                    qos: Qos::AtLeastOnce,
                },
                TopicSubscription {
                    topic: "$SYS/#".into(),
                    qos: Qos::ExactlyOnce,
                },
            ],
            ..authenticated_config()
        };
        let second = ConnectionConfig {
            name: "Workshop".into(),
            topics: vec![TopicSubscription {
                topic: "$SYS/#".into(),
                qos: Qos::AtLeastOnce,
            }],
            host: "another-broker".into(),
            password: "another-secret".into(),
            ..first.clone()
        };
        save_connections_to(
            &path,
            &SavedConnections {
                connections: vec![first.clone(), second.clone()],
                selected: Some(1),
            },
            &entries,
        )
        .unwrap();
        let loaded = load_connections_from(&path, &entries).unwrap();
        assert_eq!(loaded.selected, Some(1));
        assert_eq!(loaded.connections.len(), 2);
        assert_eq!(loaded.connections[0].password, first.password);
        assert_eq!(loaded.connections[1].password, second.password);
        assert_eq!(loaded.connections[0].name, first.name);
        assert_eq!(loaded.connections[1].name, second.name);
        assert_eq!(loaded.connections[0].topics, first.topics);
        assert_eq!(loaded.connections[1].topics, second.topics);
        assert_eq!(loaded.connections[0].client_id, first.client_id);
        assert_eq!(loaded.connections[1].client_id, second.client_id);
        let bytes = fs::read(&path).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["active_connection"], "Workshop");
        assert!(json.get("selected").is_none());
        assert!(
            json["connections"]
                .as_array()
                .unwrap()
                .iter()
                .all(|item| item.get("password").is_none()
                    && item.get("password_in_keyring").is_none()
                    && item.get("base_topic").is_none())
        );
        assert!(!std::str::from_utf8(&bytes).unwrap().contains(&first.password));
        assert!(!std::str::from_utf8(&bytes).unwrap().contains(&second.password));
    }

    #[test]
    fn selection_can_be_cleared_and_invalid_index_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connection.json");
        let entries = mock_entries();
        let mut saved = SavedConnections {
            connections: vec![ConnectionConfig::default()],
            selected: None,
        };
        save_connections_to(&path, &saved, &entries).unwrap();
        assert_eq!(load_connections_from(&path, &entries).unwrap().selected, None);
        let json: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert!(json["active_connection"].is_null());
        assert!(json.get("selected").is_none());
        saved.selected = Some(1);
        let before = fs::read(&path).unwrap();
        assert!(matches!(
            save_connections_to(&path, &saved, &entries),
            Err(ConfigError::SelectedConnectionOutOfRange {
                selected: 1,
                connection_count: 1
            })
        ));
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    #[test]
    fn active_connection_resolves_by_name_when_connections_are_reordered() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.json");
        let entries = mock_entries();
        fs::write(
            &path,
            r#"{"connections":[{"name":"Workshop","host":"workshop"},{"name":"Home","host":"home"}],"active_connection":"Home"}"#,
        )
        .unwrap();
        assert_eq!(load_connections_from(&path, &entries).unwrap().selected, Some(1));
        fs::write(
            &path,
            r#"{"connections":[{"name":"Home","host":"home"},{"name":"Workshop","host":"workshop"}],"active_connection":"Home"}"#,
        )
        .unwrap();
        assert_eq!(load_connections_from(&path, &entries).unwrap().selected, Some(0));
    }

    #[test]
    fn loading_connections_with_duplicate_names_keeps_only_the_first_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.json");
        fs::write(
            &path,
            r#"{"connections":[{"name":"Home","host":"first-broker"},{"name":"Home","host":"second-broker"}],"active_connection":"Home"}"#,
        )
        .unwrap();

        let saved = load_connections_from(&path, &mock_entries()).unwrap();
        assert_eq!(saved.connections.len(), 1);
        assert_eq!(saved.connections[0].host, "first-broker");
        assert_eq!(saved.selected, Some(0));
    }

    #[test]
    fn active_connection_tracks_remaining_entries_after_duplicate_names_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.json");
        fs::write(
            &path,
            r#"{"connections":[{"name":"Home","host":"first"},{"name":"Home","host":"duplicate"},{"name":"Workshop","host":"third"}],"active_connection":"Workshop"}"#,
        )
        .unwrap();

        let saved = load_connections_from(&path, &mock_entries()).unwrap();
        assert_eq!(saved.connections.len(), 2);
        assert_eq!(saved.connections[1].host, "third");
        assert_eq!(saved.selected, Some(1));
    }

    #[test]
    fn unknown_active_connection_falls_back_to_first_and_null_clears_selection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.json");
        let entries = mock_entries();
        fs::write(&path, r#"{"connections":[{"name":"Home"}],"active_connection":"Removed"}"#).unwrap();
        assert_eq!(load_connections_from(&path, &entries).unwrap().selected, Some(0));
        fs::write(&path, r#"{"connections":[{"name":"Home"}],"active_connection":null}"#).unwrap();
        assert_eq!(load_connections_from(&path, &entries).unwrap().selected, None);
    }

    #[test]
    fn removing_connection_deletes_only_its_credential() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connection.json");
        let entries = mock_entries();
        let first = authenticated_config();
        let second = ConnectionConfig {
            host: "another-broker".into(),
            ..first.clone()
        };
        save_connections_to(
            &path,
            &SavedConnections {
                connections: vec![first.clone(), second.clone()],
                selected: Some(1),
            },
            &entries,
        )
        .unwrap();
        save_connections_to(
            &path,
            &SavedConnections {
                connections: vec![first.clone()],
                selected: Some(0),
            },
            &entries,
        )
        .unwrap();
        assert!(matches!(entries(&second).unwrap().get_password(), Err(KeyringError::NoEntry)));
        assert_eq!(entries(&first).unwrap().get_password().unwrap(), first.password);
        assert_eq!(load_connections_from(&path, &entries).unwrap().connections.len(), 1);
    }

    #[test]
    fn clearing_password_deletes_credential_without_affecting_other_connections() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connection.json");
        let entries = mock_entries();
        let first = authenticated_config();
        let second = ConnectionConfig {
            host: "another-broker".into(),
            ..first.clone()
        };
        save_connections_to(
            &path,
            &SavedConnections {
                connections: vec![first.clone(), second.clone()],
                selected: Some(0),
            },
            &entries,
        )
        .unwrap();
        let mut cleared = first.clone();
        cleared.password.clear();
        save_connections_to(
            &path,
            &SavedConnections {
                connections: vec![cleared, second.clone()],
                selected: Some(0),
            },
            &entries,
        )
        .unwrap();
        assert!(matches!(entries(&first).unwrap().get_password(), Err(KeyringError::NoEntry)));
        assert_eq!(entries(&second).unwrap().get_password().unwrap(), second.password);
    }

    #[test]
    fn deleting_last_connection_clears_selection_and_keyring() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connection.json");
        let entries = mock_entries();
        let config = authenticated_config();
        save_connections_to(
            &path,
            &SavedConnections {
                connections: vec![config.clone()],
                selected: Some(0),
            },
            &entries,
        )
        .unwrap();
        save_connections_to(&path, &SavedConnections::default(), &entries).unwrap();
        let loaded = load_connections_from(&path, &entries).unwrap();
        assert!(loaded.connections.is_empty());
        assert_eq!(loaded.selected, None);
        assert!(matches!(entries(&config).unwrap().get_password(), Err(KeyringError::NoEntry)));
    }

    #[test]
    fn changing_broker_removes_old_account_without_touching_other_accounts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connection.json");
        let entries = mock_entries();
        let config = authenticated_config();
        let renamed = ConnectionConfig {
            host: "new-broker".into(),
            ..config.clone()
        };
        save_connections_to(
            &path,
            &SavedConnections {
                connections: vec![config.clone()],
                selected: Some(0),
            },
            &entries,
        )
        .unwrap();
        save_connections_to(
            &path,
            &SavedConnections {
                connections: vec![renamed.clone()],
                selected: Some(0),
            },
            &entries,
        )
        .unwrap();
        assert!(matches!(entries(&config).unwrap().get_password(), Err(KeyringError::NoEntry)));
        assert_eq!(entries(&renamed).unwrap().get_password().unwrap(), renamed.password);
    }

    #[test]
    fn duplicate_accounts_are_rejected_without_modifying_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connection.json");
        let config = authenticated_config();
        let entries = mock_entries();
        save_connections_to(
            &path,
            &SavedConnections {
                connections: vec![config.clone()],
                selected: Some(0),
            },
            &entries,
        )
        .unwrap();
        let before = fs::read(&path).unwrap();
        assert!(
            save_connections_to(
                &path,
                &SavedConnections {
                    connections: vec![config.clone(), config],
                    selected: Some(1),
                },
                &entries
            )
            .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    #[test]
    fn missing_password_does_not_prevent_loading_saved_connections() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connection.json");
        let entries = mock_entries();
        let missing = authenticated_config();
        let available = ConnectionConfig {
            name: "Another".into(),
            host: "another-broker".into(),
            ..missing.clone()
        };
        save_connections_to(
            &path,
            &SavedConnections {
                connections: vec![missing.clone(), available.clone()],
                selected: Some(0),
            },
            &entries,
        )
        .unwrap();
        entries(&missing).unwrap().delete_credential().unwrap();
        let loaded = load_connections_from(&path, &entries).unwrap();
        assert!(loaded.connections[0].password.is_empty());
        assert_eq!(loaded.connections[1].password, available.password);
        assert_eq!(loaded.selected, Some(0));
    }

    #[test]
    fn a_username_without_a_stored_password_loads_without_a_flag() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connection.json");
        fs::write(&path, r#"{"connections":[{"connection":"mqtt://mqtt-user@localhost:1883"}]}"#).unwrap();
        let loaded = load_connections_from(&path, &mock_entries()).unwrap();
        assert!(loaded.connections[0].password.is_empty());
    }

    #[test]
    fn anonymous_connection_does_not_need_a_credential_store_to_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connection.json");
        fs::write(&path, r#"{"connections":[{"connection":"mqtt://localhost:1883"}]}"#).unwrap();
        let entries = |_: &ConnectionConfig| -> Result<Entry, ConfigError> { panic!("anonymous connections must not access the keychain") };
        let loaded = load_connections_from(&path, &entries).unwrap();
        assert!(loaded.connections[0].password.is_empty());
    }

    #[test]
    fn legacy_password_flag_does_not_control_keychain_lookup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connection.json");
        let entries = mock_entries();
        let config = authenticated_config();
        entries(&config).unwrap().set_password(&config.password).unwrap();
        fs::write(
            &path,
            r#"{"connections":[{"connection":"mqtt://mqtt-user@localhost:1883","password_in_keyring":false}]}"#,
        )
        .unwrap();
        let loaded = load_connections_from(&path, &entries).unwrap();
        assert_eq!(loaded.connections[0].password, config.password);
        save_connections_to(&path, &SavedConnections::default(), &entries).unwrap();
        assert!(matches!(entries(&config).unwrap().get_password(), Err(KeyringError::NoEntry)));
    }

    #[test]
    fn keychain_failure_other_than_missing_entry_still_prevents_loading() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connection.json");
        let entries = mock_entries();
        let config = authenticated_config();
        fs::write(&path, r#"{"connections":[{"connection":"mqtt://mqtt-user@localhost:1883"}]}"#).unwrap();
        entries(&config)
            .unwrap()
            .inner
            .as_any()
            .downcast_ref::<mock::Cred>()
            .unwrap()
            .set_error(KeyringError::NoStorageAccess(std::io::Error::other("locked").into()));
        let error = load_connections_from(&path, &entries).err().unwrap();
        assert_eq!(error.code(), "config.credentials.retrieve_failed");
        assert!(matches!(error, ConfigError::RetrievePassword { source } if matches!(*source, ConfigError::CredentialAccessDenied)));
    }

    #[test]
    fn disk_failure_restores_keyring_and_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connection.json");
        let entries = mock_entries();
        let original = authenticated_config();
        save_connections_to(
            &path,
            &SavedConnections {
                connections: vec![original.clone()],
                selected: Some(0),
            },
            &entries,
        )
        .unwrap();
        let before = fs::read(&path).unwrap();
        fs::create_dir(path.with_extension("json.tmp")).unwrap();
        let changed = ConnectionConfig {
            password: "new-secret".into(),
            ..original.clone()
        };
        assert!(
            save_connections_to(
                &path,
                &SavedConnections {
                    connections: vec![changed],
                    selected: Some(0),
                },
                &entries
            )
            .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(entries(&original).unwrap().get_password().unwrap(), original.password);
    }

    #[test]
    fn disk_failure_restores_a_deleted_credential() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connection.json");
        let entries = mock_entries();
        let config = authenticated_config();
        save_connections_to(
            &path,
            &SavedConnections {
                connections: vec![config.clone()],
                selected: Some(0),
            },
            &entries,
        )
        .unwrap();
        let before = fs::read(&path).unwrap();
        fs::create_dir(path.with_extension("json.tmp")).unwrap();
        assert!(save_connections_to(&path, &SavedConnections::default(), &entries).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(entries(&config).unwrap().get_password().unwrap(), config.password);
    }

    #[test]
    fn keyring_delete_failure_preserves_previous_multi_store() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connection.json");
        let entries = mock_entries();
        let config = authenticated_config();
        save_connections_to(
            &path,
            &SavedConnections {
                connections: vec![config.clone()],
                selected: Some(0),
            },
            &entries,
        )
        .unwrap();
        let before = fs::read(&path).unwrap();
        entries(&config)
            .unwrap()
            .inner
            .as_any()
            .downcast_ref::<mock::Cred>()
            .unwrap()
            .set_error(KeyringError::NoStorageAccess(std::io::Error::other("locked").into()));
        assert!(save_connections_to(&path, &SavedConnections::default(), &entries).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(entries(&config).unwrap().get_password().unwrap(), config.password);
    }

    #[test]
    fn keyring_failure_preserves_previous_multi_store() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connection.json");
        let entries = mock_entries();
        let config = authenticated_config();
        save_connections_to(
            &path,
            &SavedConnections {
                connections: vec![config.clone()],
                selected: Some(0),
            },
            &entries,
        )
        .unwrap();
        let before = fs::read(&path).unwrap();
        let other = ConnectionConfig {
            host: "another-broker".into(),
            ..config.clone()
        };
        entries(&other)
            .unwrap()
            .inner
            .as_any()
            .downcast_ref::<mock::Cred>()
            .unwrap()
            .set_error(KeyringError::NoStorageAccess(std::io::Error::other("locked").into()));
        let result = save_connections_to(
            &path,
            &SavedConnections {
                connections: vec![config.clone(), other],
                selected: Some(1),
            },
            &entries,
        );
        assert!(result.is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(entries(&config).unwrap().get_password().unwrap(), config.password);
    }

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
    fn connection_uri_errors_are_typed_and_serde_uses_codes() {
        for (uri, code) in [
            ("mqtt://user:secret-password@broker", "config.connection_uri.components_invalid"),
            ("mqtt://broker/path", "config.connection_uri.components_invalid"),
            ("mqtt://broker?query", "config.connection_uri.components_invalid"),
            ("mqtt://broker#fragment", "config.connection_uri.components_invalid"),
            ("ftp://broker", "config.connection_uri.scheme_invalid"),
            ("mqtt://%FF@broker", "config.connection_uri.username_utf8_invalid"),
            ("not a URI", "config.connection_uri.invalid"),
        ] {
            let error = parse_connection_uri(uri).err().unwrap();
            assert_eq!(error.code(), code);
            assert_eq!(format!("{error:#}"), code);
            assert!(!format!("{error:?}").contains("secret-password"));
            let error = serde_json::from_value::<SavedConnection>(serde_json::json!({"connection": uri}))
                .err()
                .unwrap();
            assert_eq!(error.to_string(), code);
        }
        let saved = SavedConnection {
            config: ConnectionConfig {
                host: "invalid host".into(),
                ..Default::default()
            },
        };
        assert_eq!(
            serde_json::to_value(saved).unwrap_err().to_string(),
            "config.connection_uri.format_failed"
        );
    }

    #[test]
    fn diagnostic_readers_and_debug_preserve_rename_paths_and_selection_bounds() {
        for error in [
            ConfigError::SaveConfig {
                path: "config.json".into(),
                temporary_path: "config.json.tmp".into(),
                source: std::io::ErrorKind::PermissionDenied.into(),
            },
            ConfigError::SaveState {
                path: "state.toml".into(),
                temporary_path: "state.toml.tmp".into(),
                source: std::io::ErrorKind::PermissionDenied.into(),
            },
        ] {
            let debug = format!("{error:?}");
            assert!(debug.contains(&format!("path: {:?}", error.path().unwrap())));
            assert!(debug.contains(&format!("temporary_path: {:?}", error.temporary_path().unwrap())));
            assert!(debug.contains("source:"));
            assert_eq!(error.invalid_selection(), None);
            assert_eq!(error.to_string(), error.code());
        }
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
        let diagnostic = ConfigDiagnostic::Toml { span: Some(2..7) };
        assert_eq!(format!("{diagnostic:?}"), "Toml { span: Some(2..7) }");
    }

    #[test]
    fn uri_codes_survive_json_sanitization_and_connection_loading() {
        use std::error::Error as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let entries = |_: &ConnectionConfig| -> Result<Entry, ConfigError> { panic!("invalid URIs must not access credentials") };
        for (uri, expected) in [
            ("mqtt://user:secret-password@broker", ConnectionUriCode::InvalidComponents),
            ("ftp://broker", ConnectionUriCode::UnsupportedScheme),
            ("mqtt://%FF@broker", ConnectionUriCode::UsernameNotUtf8),
            ("not a URI", ConnectionUriCode::Invalid),
        ] {
            let json = serde_json::json!({"connections": [{"connection": uri}]}).to_string();
            fs::write(&path, &json).unwrap();
            let error = load_connections_from(&path, &entries).err().unwrap();
            assert_eq!(error.code(), "config.connections.parse_failed");
            assert_eq!(error.path(), Some(path.as_path()));
            let source = error.source().unwrap();
            assert_eq!(source.to_string(), expected.as_str());
            assert!(
                matches!(source.downcast_ref::<ConfigDiagnostic>(), Some(ConfigDiagnostic::ConnectionUri { code, .. }) if *code == expected)
            );
            assert!(!format!("{error:#?}").contains("secret-password"));
            let error = serde_json::from_str::<SavedConnection>(&serde_json::json!({"connection": uri}).to_string())
                .err()
                .unwrap();
            let location = (error.line(), error.column());
            let diagnostic = ConfigDiagnostic::from(error);
            assert_eq!(diagnostic.to_string(), expected.as_str());
            assert!(matches!(diagnostic, ConfigDiagnostic::ConnectionUri { line, column, .. } if (line, column) == location));
        }
    }

    #[test]
    fn uri_serialization_failures_preserve_codes_and_existing_files() {
        use std::error::Error as _;
        let dir = tempfile::tempdir().unwrap();
        let entries = |_: &ConnectionConfig| -> Result<Entry, ConfigError> { panic!("anonymous connections must not access credentials") };
        for name in ["config.json", "connections.json"] {
            let path = dir.path().join(name);
            let mut saved = SavedConnections {
                connections: vec![ConnectionConfig::default()],
                selected: Some(0),
            };
            save_connections_to(&path, &saved, &entries).unwrap();
            let before = fs::read(&path).unwrap();
            saved.connections[0].host = "invalid host".into();
            let error = save_connections_to(&path, &saved, &entries).unwrap_err();
            assert_eq!(error.code(), "config.serialize_failed");
            assert_eq!(error.source().unwrap().to_string(), "config.connection_uri.format_failed");
            assert!(matches!(
                error.source().unwrap().downcast_ref::<ConfigDiagnostic>(),
                Some(ConfigDiagnostic::ConnectionUri {
                    code: ConnectionUriCode::FormatFailed,
                    ..
                })
            ));
            assert_eq!(fs::read(&path).unwrap(), before);
        }
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
    fn read_errors_preserve_paths_and_io_sources() {
        use std::error::Error as _;
        let dir = tempfile::tempdir().unwrap();
        let error = read_config_value(dir.path()).unwrap_err();
        assert_eq!(error.to_string(), "config.read_failed");
        assert!(error.source().unwrap().downcast_ref::<std::io::Error>().is_some());
        assert!(matches!(error, ConfigError::ReadConfig { path, .. } if path == dir.path()));
        assert!(matches!(
            load_layout_preferences_from(dir.path()),
            Err(ConfigError::ReadState { .. })
        ));
    }

    #[test]
    fn parser_diagnostics_keep_locations_without_echoing_config_values() {
        use std::error::Error as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(&path, "{\n\"password\": \"secret-password\",\n").unwrap();
        let error = read_config_value(&path).unwrap_err();
        assert_eq!(error.code(), "config.parse_failed");
        assert!(!format!("{error:#?}").contains("secret-password"));
        let source = error.source().unwrap();
        assert_eq!(source.to_string(), "config.diagnostic.json_eof");
        assert!(matches!(
            source.downcast_ref::<ConfigDiagnostic>(),
            Some(ConfigDiagnostic::Json { line: 3, .. })
        ));
        fs::write(
            &path,
            r#"{"connections":[{"connection":"mqtt://broker","port":"secret-password"}]}"#,
        )
        .unwrap();
        let error = read_connections(&path).err().unwrap();
        assert!(matches!(error, ConfigError::ParseConnections { .. }));
        assert!(!format!("{error:#?}").contains("secret-password"));
        assert_eq!(error.source().unwrap().to_string(), "config.diagnostic.json_data");
        let path = dir.path().join("state.toml");
        fs::write(&path, "topics_width_rem = \"secret-password\"").unwrap();
        let error = load_layout_preferences_from(&path).err().unwrap();
        assert!(matches!(error, ConfigError::ParseState { .. }));
        assert!(!format!("{error:#?}").contains("secret-password"));
        assert!(matches!(
            error.source().unwrap().downcast_ref::<ConfigDiagnostic>(),
            Some(ConfigDiagnostic::Toml { span: Some(_) })
        ));
        fs::write(&path, [0xff]).unwrap();
        assert!(matches!(load_layout_preferences_from(&path), Err(ConfigError::StateNotUtf8 { .. })));
    }

    #[test]
    fn serialization_errors_do_not_expose_custom_messages() {
        use std::error::Error as _;
        struct SecretSerializer;
        impl Serialize for SecretSerializer {
            fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("serializer-secret"))
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let error = update_json(&path, &SecretSerializer).unwrap_err();
        assert!(matches!(error, ConfigError::SerializeConfig { .. }));
        assert_eq!(error.to_string(), "config.serialize_failed");
        assert!(!format!("{error:#?}").contains("serializer-secret"));
        assert!(!error.source().unwrap().to_string().contains("serializer-secret"));
        let error = write_json(&path, &SecretSerializer).unwrap_err();
        assert!(matches!(error, ConfigError::SerializeConfig { .. }));
        let error = write_toml(&dir.path().join("state.toml"), &SecretSerializer).unwrap_err();
        assert!(matches!(error, ConfigError::SerializeState { .. }));
        assert_eq!(error.source().unwrap().to_string(), "config.diagnostic.toml_serialize");
        assert!(!format!("{error:#?}").contains("serializer-secret"));
    }

    #[test]
    fn rollback_failure_retains_the_original_failure_and_all_restore_failures() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.json");
        let entries = mock_entries();
        let config = authenticated_config();
        save_connections_to(
            &path,
            &SavedConnections {
                connections: vec![config.clone()],
                selected: Some(0),
            },
            &entries,
        )
        .unwrap();
        let before = fs::read(&path).unwrap();
        fs::create_dir(path.with_extension("json.tmp")).unwrap();
        let calls = std::cell::Cell::new(0);
        let failing_entries = |config: &ConnectionConfig| {
            calls.set(calls.get() + 1);
            if calls.get() == 3 {
                Err(ConfigError::OpenCredentialStore {
                    source: Box::new(ConfigError::CredentialAccessDenied),
                })
            } else {
                entries(config)
            }
        };
        let changed = ConnectionConfig {
            password: "changed-secret".into(),
            ..config
        };
        let error = save_connections_to(
            &path,
            &SavedConnections {
                connections: vec![changed],
                selected: Some(0),
            },
            &failing_entries,
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "config.save_rollback_failed");
        assert_eq!(error.path(), Some(path.with_extension("json.tmp").as_path()));
        assert_eq!(error.temporary_path(), error.path());
        let debug = format!("{error:#?}");
        assert!(!debug.contains("changed-secret"));
        assert!(debug.contains("rollback:"));
        assert!(debug.contains("failures:"));
        assert!(debug.contains("config.credentials.access_denied"));
        let ConfigError::SaveRollbackFailed { source, rollback } = error else {
            panic!("expected rollback failure")
        };
        assert!(matches!(*source, ConfigError::OpenTemporaryConfig { .. }));
        let ConfigError::RollbackCredentials { failures } = *rollback else {
            panic!("expected restore failures")
        };
        assert_eq!(failures.len(), 1);
        assert!(matches!(failures[0], ConfigError::OpenCredentialStore { .. }));
        assert_eq!(fs::read(&path).unwrap(), before);
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
