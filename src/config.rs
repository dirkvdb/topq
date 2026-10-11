//! Connection settings on disk and MQTT passwords in the system credential store.

use std::{
    collections::{HashSet, hash_map::RandomState},
    fs,
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

use crate::{
    error::config::{ConfigError, ConnectionValidationCode, ConnectionValidationError, LayoutDiagnostic, LayoutError, credential_error},
    mqtt::Qos,
};

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
            return Err(ConnectionValidationError::new(
                ConnectionField::Topics,
                ConnectionValidationCode::InvalidTopicFilter,
            ));
        }
        Ok(())
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct ConnectionConfig {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub client_id: String,
    pub topics: Vec<TopicSubscription>,
    pub username: String,
    /// Runtime-only password from user input or the credential store; never read from or written to configuration.
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
            return Err(ConnectionValidationError::new(
                ConnectionField::Host,
                ConnectionValidationCode::MissingHost,
            ));
        }
        if self.host.contains("://") || self.host.contains('/') {
            return Err(ConnectionValidationError::new(
                ConnectionField::Host,
                ConnectionValidationCode::HostContainsSchemeOrPath,
            ));
        }
        if self.port == 0 {
            return Err(ConnectionValidationError::new(
                ConnectionField::Port,
                ConnectionValidationCode::InvalidPort,
            ));
        }
        if self.client_id.trim().is_empty() || self.client_id.len() > usize::from(u16::MAX) || self.client_id.contains('\0') {
            return Err(ConnectionValidationError::new(
                ConnectionField::ClientId,
                ConnectionValidationCode::InvalidClientId,
            ));
        }
        Ok(())
    }

    pub(crate) fn validate_broker_connection(&self) -> Result<(), ConnectionValidationError> {
        self.validate_broker_endpoint()?;
        if self.username.is_empty() && !self.password.is_empty() {
            return Err(ConnectionValidationError::new(
                ConnectionField::Username,
                ConnectionValidationCode::PasswordWithoutUsername,
            ));
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<(), ConnectionValidationError> {
        self.validate_broker_endpoint()?;
        if self.topics.is_empty() {
            return Err(ConnectionValidationError::new(
                ConnectionField::Topics,
                ConnectionValidationCode::MissingTopics,
            ));
        }
        let mut filters = HashSet::new();
        for subscription in &self.topics {
            subscription.validate()?;
            if !filters.insert(subscription.topic.as_str()) {
                return Err(ConnectionValidationError::new(
                    ConnectionField::Topics,
                    ConnectionValidationCode::DuplicateTopicFilter,
                ));
            }
        }
        if self.username.is_empty() && !self.password.is_empty() {
            return Err(ConnectionValidationError::new(
                ConnectionField::Username,
                ConnectionValidationCode::PasswordWithoutUsername,
            ));
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
    // Locating the shared state directory can only fail when the home directory is
    // unavailable. Config fallback failures are user-facing; layout failures are log-only.
    let path = state_path().map_err(|_| ConfigError::HomeDirectoryUnavailable)?;
    Ok(path.with_file_name("config.json"))
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
        | ConfigError::CreateConfigDirectory { .. }
        | ConfigError::OpenTemporaryConfig { .. }
        | ConfigError::WriteConfig { .. }
        | ConfigError::FlushConfig { .. }
        | ConfigError::SaveConfig { .. } => true,
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
        let appearance_only = [
            "mode",
            "light_theme",
            "dark_theme",
            "reduce_motion",
            "inline_payload",
            "inline_payload_limit_enabled",
            "inline_payload_max_chars",
        ]
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
        #[serde(default)]
        struct ConnectionSettings {
            name: String,
            client_id: String,
            topics: Vec<TopicSubscription>,
            validate_certificate: bool,
        }

        impl Default for ConnectionSettings {
            fn default() -> Self {
                let config = ConnectionConfig::default();
                Self {
                    name: config.name,
                    client_id: config.client_id,
                    topics: config.topics,
                    validate_certificate: config.validate_certificate,
                }
            }
        }

        #[derive(Deserialize)]
        struct DeserializedSavedConnection {
            connection: String,
            #[serde(flatten)]
            settings: ConnectionSettings,
        }

        let saved = DeserializedSavedConnection::deserialize(deserializer)?;
        let address = parse_connection_uri(&saved.connection).map_err(serde::de::Error::custom)?;
        Ok(Self {
            config: ConnectionConfig {
                name: saved.settings.name,
                host: address.host,
                port: address.port,
                client_id: saved.settings.client_id,
                topics: saved.settings.topics,
                username: address.username,
                password: String::new(),
                tls: address.tls,
                validate_certificate: saved.settings.validate_certificate,
                websocket: address.websocket,
            },
        })
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
    let protocol = config.protocol().trim_end_matches("://");
    // Length prefixes keep host and username delimiters from colliding.
    format!(
        "{}:{}:{}:{}:{}:{}",
        config.host.len(),
        config.host,
        config.port,
        protocol,
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct UiState {
    pub(crate) topics_width_rem: Option<f32>,
    pub(crate) topics_width_fraction: Option<f32>,
    pub(crate) publish_height_rem: Option<f32>,
    pub(crate) publish_height_fraction: Option<f32>,
    pub(crate) publish_open: bool,
}

#[cfg(test)]
pub(crate) fn state_path() -> Result<PathBuf, LayoutError> {
    std::thread_local! {
        static TEST_STATE_DIR: tempfile::TempDir = tempfile::tempdir()
            .expect("Could not create a temporary application state directory.");
    }
    Ok(TEST_STATE_DIR.with(|dir| dir.path().join("topq").join("state.toml")))
}

#[cfg(not(test))]
fn state_path() -> Result<PathBuf, LayoutError> {
    let state_dir = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .map(Ok)
        .unwrap_or_else(|| {
            BaseDirs::new()
                .map(|dirs| dirs.home_dir().join(".local").join("state"))
                .ok_or(LayoutError::HomeDirectoryUnavailable)
        })?;
    Ok(state_dir.join("topq").join("state.toml"))
}

/// Restores the UI state, logging failures and falling back to defaults.
pub(crate) fn load_ui_state() -> UiState {
    state_path().and_then(|path| load_ui_state_from(&path)).unwrap_or_else(|error| {
        tracing::warn!(error_code = error.code(), diagnostic = ?error, "ui_state.load_failed");
        UiState::default()
    })
}

fn load_ui_state_from(path: &Path) -> Result<UiState, LayoutError> {
    let data = match fs::read(path) {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(UiState::default()),
        Err(source) => {
            return Err(LayoutError::ReadState {
                path: path.to_owned(),
                source,
            });
        }
    };
    let text = std::str::from_utf8(&data).map_err(|source| LayoutError::StateNotUtf8 {
        path: path.to_owned(),
        source,
    })?;
    let mut state: UiState = toml::from_str(text).map_err(|error| LayoutError::ParseState {
        path: path.to_owned(),
        source: error.into(),
    })?;
    state.topics_width_rem = state.topics_width_rem.filter(|width| width.is_finite() && *width > 0.);
    state.topics_width_fraction = state
        .topics_width_fraction
        .filter(|fraction| fraction.is_finite() && (0. ..=1.).contains(fraction));
    state.publish_height_rem = state.publish_height_rem.filter(|height| height.is_finite() && *height > 0.);
    state.publish_height_fraction = state
        .publish_height_fraction
        .filter(|fraction| fraction.is_finite() && (0. ..=1.).contains(fraction));
    Ok(state)
}

/// Best-effort save of the entire UI snapshot; failures are logged without replacing unreadable state.
pub(crate) fn save_ui_state(state: &UiState) {
    if let Err(error) = state_path().and_then(|path| save_ui_state_to(&path, state)) {
        tracing::warn!(error_code = error.code(), diagnostic = ?error, "ui_state.save_failed");
    }
}

fn save_ui_state_to(path: &Path, state: &UiState) -> Result<(), LayoutError> {
    // Validate existing state before replacing it, without merging it into the supplied snapshot.
    load_ui_state_from(path)?;
    write_toml(path, state)
}

fn write_toml(path: &Path, value: &impl Serialize) -> Result<(), LayoutError> {
    let parent = path
        .parent()
        .ok_or_else(|| LayoutError::InvalidStatePath { path: path.to_owned() })?;
    fs::create_dir_all(parent).map_err(|source| LayoutError::CreateStateDirectory {
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
    let mut file = options.open(&temp).map_err(|source| LayoutError::OpenTemporaryState {
        path: temp.clone(),
        source,
    })?;
    let data = toml::to_string_pretty(value).map_err(|_| LayoutError::SerializeState {
        path: path.to_owned(),
        source: LayoutDiagnostic::TomlSerialization,
    })?;
    file.write_all(data.as_bytes()).map_err(|source| LayoutError::WriteState {
        path: temp.clone(),
        source,
    })?;
    file.sync_all().map_err(|source| LayoutError::FlushState {
        path: temp.clone(),
        source,
    })?;
    fs::rename(&temp, path).map_err(|source| LayoutError::SaveState {
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
    use crate::error::config::{ConfigDiagnostic, ConnectionUriCode};

    #[derive(Clone, Default)]
    struct CapturedLogs(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for CapturedLogs {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn capture_logs<T>(operation: impl FnOnce() -> T) -> (T, String) {
        let output = CapturedLogs::default();
        let writer = output.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .without_time()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let result = tracing::subscriber::with_default(subscriber, operation);
        let logs = String::from_utf8(output.0.lock().unwrap().clone()).unwrap();
        (result, logs)
    }

    fn assert_layout_failure_logs(logs: &str, path: &Path, error_code: &str) {
        let events = ["ui_state.load_failed", "ui_state.save_failed"];
        assert_eq!(logs.lines().count(), events.len(), "{logs}");
        for (line, event) in logs.lines().zip(events) {
            assert!(line.contains("WARN"), "{line}");
            assert!(line.contains(event), "{line}");
            assert!(line.contains(&format!("error_code=\"{error_code}\"")), "{line}");
            assert!(line.contains("diagnostic=LayoutError"), "{line}");
            assert!(line.contains(&format!("path: {path:?}")), "{line}");
        }
    }

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
    fn topics_default_to_all_topics_at_qos_zero_when_omitted() {
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
    fn saved_connections_require_a_string_connection_uri_before_accessing_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let entries = |_: &ConnectionConfig| -> Result<Entry, ConfigError> { panic!("invalid saved entries must not access credentials") };
        for connection in [None, Some(serde_json::Value::Null), Some(serde_json::json!(42))] {
            let mut saved = serde_json::json!({"name": "Home"});
            if let Some(connection) = connection {
                saved["connection"] = connection;
            }
            let original = serde_json::json!({"connections": [saved], "active_connection": "Home"}).to_string();
            fs::write(&path, &original).unwrap();
            assert!(matches!(
                load_connections_from(&path, &entries),
                Err(ConfigError::ParseConnections { .. })
            ));
            assert!(matches!(
                save_connections_to(&path, &SavedConnections::default(), &entries),
                Err(ConfigError::ParseConnections { .. })
            ));
            assert_eq!(fs::read_to_string(&path).unwrap(), original);
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
    fn omitted_websocket_defaults_to_tcp_without_changing_tls() {
        for (json, expected) in [("{}", "mqtt://"), (r#"{"tls":false}"#, "mqtt://"), (r#"{"tls":true}"#, "mqtts://")] {
            let config: ConnectionConfig = serde_json::from_str(json).unwrap();
            assert!(!config.websocket);
            assert_eq!(config.protocol(), expected);
        }
        assert!(!ConnectionConfig::default().websocket);
    }

    #[test]
    fn certificate_validation_defaults_to_true_when_omitted() {
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
    fn generated_client_ids_are_unique_and_omitted_ids_receive_a_default() {
        let first = ConnectionConfig::default().client_id;
        let second = ConnectionConfig::default().client_id;
        assert!(first.starts_with("topq-"));
        assert_ne!(first, second);
        assert_eq!(first.len(), 21);

        let defaulted: ConnectionConfig = serde_json::from_str(r#"{"host":"broker.example"}"#).unwrap();
        assert!(defaulted.client_id.starts_with("topq-"));
        let configured: ConnectionConfig = serde_json::from_str(r#"{"client_id":"custom-client"}"#).unwrap();
        assert_eq!(configured.client_id, "custom-client");
    }

    #[test]
    fn deserialization_ignores_plaintext_passwords_regardless_of_value_type() {
        for password in [r#""plaintext-secret""#, "null", "true", "42", "[]", "{}"] {
            let json = format!(r#"{{"username":"mqtt-user","password":{password},"topics":[{{"topic":"home/#","qos":0}}]}}"#);
            let config: ConnectionConfig = serde_json::from_str(&json).unwrap();
            assert!(config.password.is_empty());
        }
    }

    #[test]
    fn saved_connection_deserialization_ignores_plaintext_passwords_regardless_of_value_type() {
        for password in [r#""plaintext-secret""#, "null", "true", "42", "[]", "{}"] {
            let json = format!(r#"{{"connection":"mqtt://mqtt-user@localhost:1883","password":{password}}}"#);
            let saved: SavedConnection = serde_json::from_str(&json).unwrap();
            assert!(saved.config.password.is_empty());
            let serialized = serde_json::to_string(&saved).unwrap();
            assert!(!serialized.contains("plaintext-secret"));
            assert!(
                serde_json::from_str::<serde_json::Value>(&serialized)
                    .unwrap()
                    .get("password")
                    .is_none()
            );
        }
    }

    #[test]
    fn topics_require_an_array_when_present() {
        for topics in ["null", "true", "42", r#""home/#""#, "{}"] {
            let json = format!(r#"{{"topics":{topics}}}"#);
            assert!(serde_json::from_str::<ConnectionConfig>(&json).is_err());
            let saved_json = format!(r#"{{"connection":"mqtt://localhost:1883","topics":{topics}}}"#);
            assert!(serde_json::from_str::<SavedConnection>(&saved_json).is_err());
        }
    }

    #[test]
    fn explicit_empty_topics_are_not_replaced_with_defaults() {
        let config: ConnectionConfig = serde_json::from_str(r#"{"topics":[]}"#).unwrap();
        assert!(config.topics.is_empty());
        assert_eq!(config.validate().unwrap_err().code(), ConnectionValidationCode::MissingTopics);
    }

    #[test]
    fn deserialized_invalid_topic_filters_are_not_replaced_with_defaults() {
        for topic in ["", "home/#/value"] {
            let config: ConnectionConfig = serde_json::from_value(serde_json::json!({
                "topics": [{"topic": topic, "qos": 0}]
            }))
            .unwrap();
            assert_eq!(config.topics[0].topic, topic);
            assert_eq!(config.validate().unwrap_err().code(), ConnectionValidationCode::InvalidTopicFilter);
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
    fn connection_config_deserialization_rejects_non_numeric_qos() {
        for qos in [r#""1""#, r#""AtMostOnce""#, "null", "true", "[]", "{}", "1e400"] {
            let json = format!(r#"{{"topics":[{{"topic":"home/#","qos":{qos}}}]}}"#);
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
    fn tcp_and_tls_credential_keys_identify_the_mqtt_protocol() {
        let mut config = authenticated_config();
        assert_eq!(credential_account(&config), "9:localhost:1883:mqtt:9:mqtt-user");
        config.tls = true;
        assert_eq!(credential_account(&config), "9:localhost:1883:mqtts:9:mqtt-user");
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
    fn saved_tcp_and_tls_protocols_restore_passwords_and_roundtrip_uris() {
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
        fs::write(
            &path,
            r#"{"connections":[{"connection":"mqtt://mqtt-user@localhost:1883"},{"connection":"mqtts://mqtt-user@localhost:1883"}]}"#,
        )
        .unwrap();
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
        assert!(json["connections"][0].get("password").is_none());
    }

    #[test]
    fn saved_connections_default_to_validating_certificates_when_omitted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.json");
        let entries = mock_entries();
        fs::write(&path, r#"{"connections":[{"connection":"mqtts://localhost:8883"}]}"#).unwrap();
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
    fn inline_payload_only_config_loads_without_connections() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        fs::write(&path, r#"{"inline_payload":false}"#).unwrap();
        assert!(read_connections(&path).unwrap().unwrap().connections.is_empty());
    }

    #[test]
    fn inline_payload_limit_enabled_only_config_loads_without_connections() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        for enabled in [false, true] {
            fs::write(
                &path,
                serde_json::to_vec(&serde_json::json!({"inline_payload_limit_enabled": enabled})).unwrap(),
            )
            .unwrap();
            let stored = read_connections(&path).unwrap().unwrap();
            assert!(stored.connections.is_empty());
            assert!(stored.active_connection.is_none());
        }
    }

    #[test]
    fn inline_payload_max_chars_only_config_loads_without_connections() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        for limit in [0, 10, 256] {
            fs::write(
                &path,
                serde_json::to_vec(&serde_json::json!({"inline_payload_max_chars": limit})).unwrap(),
            )
            .unwrap();
            let stored = read_connections(&path).unwrap().unwrap();
            assert!(stored.connections.is_empty());
            assert!(stored.active_connection.is_none());
        }
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
            &serde_json::json!({"connections":[{"name":"Home","connection":"mqtt://localhost:1883"}],"active_connection":"Home"}),
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
    fn ui_state_snapshot_roundtrips_through_flat_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("topq").join("state.toml");
        let state = UiState {
            topics_width_rem: Some(31.5),
            topics_width_fraction: Some(0.4),
            publish_height_rem: Some(18.5),
            publish_height_fraction: Some(0.25),
            publish_open: true,
        };
        save_ui_state_to(&path, &state).unwrap();
        assert_eq!(load_ui_state_from(&path).unwrap(), state);
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "topics_width_rem = 31.5\ntopics_width_fraction = 0.4\npublish_height_rem = 18.5\npublish_height_fraction = 0.25\npublish_open = true\n"
        );
        assert!(!path.with_extension("toml.tmp").exists());
    }

    #[test]
    fn ui_state_fraction_boundaries_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.toml");
        for fraction in [0., 0.25, 1.] {
            let state = UiState {
                topics_width_fraction: Some(fraction),
                publish_height_fraction: Some(fraction),
                ..UiState::default()
            };
            save_ui_state_to(&path, &state).unwrap();
            assert_eq!(load_ui_state_from(&path).unwrap(), state);
        }
    }

    #[test]
    fn ui_state_visibility_roundtrips_with_both_pane_sizes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.toml");
        for publish_open in [true, false] {
            let state = UiState {
                topics_width_rem: Some(31.5),
                topics_width_fraction: Some(0.4),
                publish_height_rem: Some(18.5),
                publish_height_fraction: Some(0.25),
                publish_open,
            };
            save_ui_state_to(&path, &state).unwrap();
            assert_eq!(load_ui_state_from(&path).unwrap(), state);
        }
    }

    #[test]
    fn ui_state_saves_replace_the_entire_snapshot_without_merging() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.toml");
        let original = UiState {
            topics_width_rem: Some(31.5),
            topics_width_fraction: Some(0.4),
            publish_height_rem: Some(18.5),
            publish_height_fraction: Some(0.25),
            publish_open: true,
        };
        save_ui_state_to(&path, &original).unwrap();
        let replacement = UiState {
            topics_width_rem: Some(28.),
            topics_width_fraction: Some(0.3),
            ..UiState::default()
        };
        save_ui_state_to(&path, &replacement).unwrap();
        assert_eq!(load_ui_state_from(&path).unwrap(), replacement);
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "topics_width_rem = 28.0\ntopics_width_fraction = 0.3\npublish_open = false\n"
        );
        save_ui_state_to(&path, &UiState::default()).unwrap();
        assert_eq!(load_ui_state_from(&path).unwrap(), UiState::default());
        assert_eq!(fs::read_to_string(&path).unwrap(), "publish_open = false\n");
    }

    #[test]
    fn partial_ui_state_defaults_missing_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.toml");
        for (original, expected) in [
            ("", UiState::default()),
            (
                "topics_width_rem = 31.5\ntopics_width_fraction = 0.4\n",
                UiState {
                    topics_width_rem: Some(31.5),
                    topics_width_fraction: Some(0.4),
                    ..UiState::default()
                },
            ),
            (
                "publish_height_rem = 18.5\npublish_open = true\n",
                UiState {
                    publish_height_rem: Some(18.5),
                    publish_open: true,
                    ..UiState::default()
                },
            ),
        ] {
            fs::write(&path, original).unwrap();
            assert_eq!(load_ui_state_from(&path).unwrap(), expected);
            assert_eq!(fs::read_to_string(&path).unwrap(), original);
        }
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
    fn missing_ui_state_defaults_without_creating_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.toml");
        assert_eq!(load_ui_state_from(&path).unwrap(), UiState::default());
        assert!(!path.exists());
    }

    #[test]
    fn ui_state_facade_defaults_missing_state_without_logging_or_creating_files() {
        let path = state_path().unwrap();
        let (state, logs) = capture_logs(load_ui_state);
        assert_eq!(state, UiState::default());
        assert!(logs.is_empty(), "{logs}");
        assert!(!path.exists());
    }

    #[test]
    fn ui_state_facade_roundtrips_without_logging_and_replaces_external_settings() {
        let path = state_path().unwrap();
        let snapshot = UiState {
            topics_width_rem: Some(31.5),
            topics_width_fraction: Some(0.4),
            publish_height_rem: Some(18.5),
            publish_height_fraction: Some(0.25),
            publish_open: true,
        };
        let (state, logs) = capture_logs(|| {
            save_ui_state(&snapshot);
            assert_eq!(load_ui_state(), snapshot);
            fs::write(&path, "topics_width_rem = 28.0\ntopics_width_fraction = 0.3\npublish_height_rem = 20.0\npublish_height_fraction = 0.5\npublish_open = false\n").unwrap();
            save_ui_state(&snapshot);
            load_ui_state()
        });
        assert_eq!(state, snapshot);
        assert!(logs.is_empty(), "{logs}");
        assert!(!path.with_extension("toml.tmp").exists());
    }

    #[test]
    fn ui_state_facade_logs_safe_diagnostics_defaults_loads_and_preserves_malformed_state() {
        let path = state_path().unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        for (original, code) in [
            (b"topics_width_rem = [ # parser-secret".as_slice(), "config.state.parse_failed"),
            (b"publish_height_rem = \"parser-secret\"".as_slice(), "config.state.parse_failed"),
            (b"publish_open = \"parser-secret\"".as_slice(), "config.state.parse_failed"),
            (b"\xffparser-secret".as_slice(), "config.state.utf8_invalid"),
        ] {
            fs::write(&path, original).unwrap();
            let (state, logs) = capture_logs(|| {
                let state = load_ui_state();
                save_ui_state(&UiState {
                    topics_width_rem: Some(31.5),
                    publish_open: true,
                    ..UiState::default()
                });
                state
            });
            assert_eq!(state, UiState::default());
            assert_layout_failure_logs(&logs, &path, code);
            assert!(!logs.contains("parser-secret"), "{logs}");
            if code == "config.state.parse_failed" {
                assert!(logs.contains("Toml { span: Some("), "{logs}");
            } else {
                assert!(logs.contains("valid_up_to: 0"), "{logs}");
            }
            assert_eq!(fs::read(&path).unwrap(), original);
            assert!(!path.with_extension("toml.tmp").exists());
        }
    }

    #[test]
    fn ui_state_facade_logs_read_errors_and_defaults_without_writing() {
        let path = state_path().unwrap();
        fs::create_dir_all(&path).unwrap();
        let (state, logs) = capture_logs(|| {
            let state = load_ui_state();
            save_ui_state(&UiState::default());
            state
        });
        assert_eq!(state, UiState::default());
        assert_layout_failure_logs(&logs, &path, "config.state.read_failed");
        assert!(logs.contains("io_kind:"), "{logs}");
        assert!(path.is_dir());
        assert!(!path.with_extension("toml.tmp").exists());
    }

    #[test]
    fn ui_state_facade_logs_one_write_failure_without_changing_existing_snapshot() {
        let path = state_path().unwrap();
        let original = UiState {
            topics_width_rem: Some(31.5),
            topics_width_fraction: Some(0.4),
            publish_height_rem: Some(18.5),
            publish_height_fraction: Some(0.25),
            publish_open: true,
        };
        save_ui_state_to(&path, &original).unwrap();
        let before = fs::read(&path).unwrap();
        let temporary_path = path.with_extension("toml.tmp");
        fs::create_dir(&temporary_path).unwrap();
        let ((), logs) = capture_logs(|| save_ui_state(&UiState::default()));
        assert_eq!(logs.lines().count(), 1, "{logs}");
        assert!(logs.contains("WARN"), "{logs}");
        assert!(logs.contains("ui_state.save_failed"), "{logs}");
        assert!(logs.contains("error_code=\"config.state.temporary_open_failed\""), "{logs}");
        assert!(logs.contains(&format!("temporary_path: {temporary_path:?}")), "{logs}");
        assert!(logs.contains("io_kind:"), "{logs}");
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(load_ui_state(), original);
    }

    #[test]
    fn invalid_optional_ui_state_sizes_are_filtered_independently() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.toml");
        let valid = UiState {
            topics_width_rem: Some(31.5),
            topics_width_fraction: Some(0.4),
            publish_height_rem: Some(18.5),
            publish_height_fraction: Some(0.25),
            publish_open: true,
        };
        for invalid in [0., -1., f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            for topics_invalid in [true, false] {
                let mut snapshot = valid;
                let mut expected = valid;
                if topics_invalid {
                    snapshot.topics_width_rem = Some(invalid);
                    expected.topics_width_rem = None;
                } else {
                    snapshot.publish_height_rem = Some(invalid);
                    expected.publish_height_rem = None;
                }
                save_ui_state_to(&path, &snapshot).unwrap();
                assert_eq!(load_ui_state_from(&path).unwrap(), expected, "invalid size: {invalid}");
            }
        }
    }

    #[test]
    fn invalid_optional_ui_state_fractions_are_filtered_independently() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.toml");
        let valid = UiState {
            topics_width_rem: Some(31.5),
            topics_width_fraction: Some(0.4),
            publish_height_rem: Some(18.5),
            publish_height_fraction: Some(0.25),
            publish_open: true,
        };
        for invalid in [-0.1, 1.1, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            for topics_invalid in [true, false] {
                let mut snapshot = valid;
                let mut expected = valid;
                if topics_invalid {
                    snapshot.topics_width_fraction = Some(invalid);
                    expected.topics_width_fraction = None;
                } else {
                    snapshot.publish_height_fraction = Some(invalid);
                    expected.publish_height_fraction = None;
                }
                save_ui_state_to(&path, &snapshot).unwrap();
                assert_eq!(load_ui_state_from(&path).unwrap(), expected, "invalid fraction: {invalid}");
            }
        }
    }

    #[test]
    fn ui_state_saves_leave_malformed_state_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.toml");
        for original in [
            "topics_width_rem = [",
            "publish_height_rem = \"invalid\"",
            "publish_open = \"invalid\"",
        ] {
            fs::write(&path, original).unwrap();
            assert!(matches!(
                save_ui_state_to(&path, &UiState::default()),
                Err(LayoutError::ParseState { .. })
            ));
            assert_eq!(fs::read_to_string(&path).unwrap(), original);
            assert!(!path.with_extension("toml.tmp").exists());
        }
    }

    #[test]
    fn ui_state_saves_leave_non_utf8_state_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.toml");
        let original = b"\xffparser-secret";
        fs::write(&path, original).unwrap();
        assert!(matches!(
            save_ui_state_to(&path, &UiState::default()),
            Err(LayoutError::StateNotUtf8 { .. })
        ));
        assert_eq!(fs::read(&path).unwrap(), original);
        assert!(!path.with_extension("toml.tmp").exists());
    }

    #[test]
    fn ui_state_saves_propagate_read_errors_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.toml");
        fs::create_dir(&path).unwrap();
        assert!(matches!(
            save_ui_state_to(&path, &UiState::default()),
            Err(LayoutError::ReadState { .. })
        ));
        assert!(path.is_dir());
        assert!(!path.with_extension("toml.tmp").exists());
    }

    #[test]
    fn saved_connections_preserve_topics_and_ignore_plaintext_passwords() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.json");
        fs::write(
            &path,
            r#"{"connections":[{"name":"Home","connection":"mqtt://home-broker:1883","topics":[{"topic":"home/#","qos":2}],"password":"plaintext-secret"},{"connection":"mqtt://default-broker:1883"}],"active_connection":"Home"}"#,
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
        assert_eq!(json["active_connection"], "Home");
        assert!(json.get("selected").is_none());
        assert!(
            json["connections"]
                .as_array()
                .unwrap()
                .iter()
                .all(|config| config.get("password").is_none() && config.get("topics").is_some())
        );
        assert!(!fs::read_to_string(&path).unwrap().contains("plaintext-secret"));
        let loaded = load_connections_from(&path, &entries).unwrap();
        assert_eq!(loaded.connections[0].topics, saved.connections[0].topics);
    }

    #[test]
    fn single_connection_shape_is_not_accepted_as_multi_connection_storage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.json");
        let original = br#"{"connection":"mqtt://localhost:1883"}"#;
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
                r#"{{"mode":"dark","connections":[{{"name":"Invalid","connection":"mqtt://broker.example:1883","topics":[{{"topic":"home/#","qos":{qos}}}]}}],"active_connection":"Invalid"}}"#,
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
                r#"{{"mode":"dark","connections":[{{"name":"Recovered","connection":"mqtt://broker.example:1883","topics":[{{"topic":"home/#","qos":{qos}}}]}},{{"name":"Valid","connection":"mqtt://other.example:1883","topics":[{{"topic":"office/#","qos":2}}]}}],"active_connection":"Recovered"}}"#,
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
                .all(|item| item.get("password").is_none())
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
            r#"{"connections":[{"name":"Workshop","connection":"mqtt://workshop:1883"},{"name":"Home","connection":"mqtt://home:1883"}],"active_connection":"Home"}"#,
        )
        .unwrap();
        assert_eq!(load_connections_from(&path, &entries).unwrap().selected, Some(1));
        fs::write(
            &path,
            r#"{"connections":[{"name":"Home","connection":"mqtt://home:1883"},{"name":"Workshop","connection":"mqtt://workshop:1883"}],"active_connection":"Home"}"#,
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
            r#"{"connections":[{"name":"Home","connection":"mqtt://first-broker:1883"},{"name":"Home","connection":"mqtt://second-broker:1883"}],"active_connection":"Home"}"#,
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
            r#"{"connections":[{"name":"Home","connection":"mqtt://first:1883"},{"name":"Home","connection":"mqtt://duplicate:1883"},{"name":"Workshop","connection":"mqtt://third:1883"}],"active_connection":"Workshop"}"#,
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
        fs::write(
            &path,
            r#"{"connections":[{"name":"Home","connection":"mqtt://localhost:1883"}],"active_connection":"Removed"}"#,
        )
        .unwrap();
        assert_eq!(load_connections_from(&path, &entries).unwrap().selected, Some(0));
        fs::write(
            &path,
            r#"{"connections":[{"name":"Home","connection":"mqtt://localhost:1883"}],"active_connection":null}"#,
        )
        .unwrap();
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
    fn a_username_without_a_stored_password_loads_with_an_empty_password() {
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
    fn saved_username_retrieves_only_the_credential_store_password_and_removal_deletes_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connection.json");
        let entries = mock_entries();
        let config = authenticated_config();
        entries(&config).unwrap().set_password(&config.password).unwrap();
        fs::write(
            &path,
            r#"{"connections":[{"connection":"mqtt://mqtt-user@localhost:1883","password":"plaintext-secret"}]}"#,
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
    fn read_errors_preserve_paths_and_io_sources() {
        use std::error::Error as _;
        let dir = tempfile::tempdir().unwrap();
        let error = read_config_value(dir.path()).unwrap_err();
        assert_eq!(error.to_string(), "config.read_failed");
        assert!(error.source().unwrap().downcast_ref::<std::io::Error>().is_some());
        assert!(matches!(error, ConfigError::ReadConfig { path, .. } if path == dir.path()));
        assert!(matches!(load_ui_state_from(dir.path()), Err(LayoutError::ReadState { .. })));
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
            r#"{"connections":[{"connection":"mqtt://broker","validate_certificate":"secret-password"}]}"#,
        )
        .unwrap();
        let error = read_connections(&path).err().unwrap();
        assert!(matches!(error, ConfigError::ParseConnections { .. }));
        assert!(!format!("{error:#?}").contains("secret-password"));
        assert_eq!(error.source().unwrap().to_string(), "config.diagnostic.json_data");
        let path = dir.path().join("state.toml");
        fs::write(&path, "topics_width_rem = \"secret-password\"").unwrap();
        let error = load_ui_state_from(&path).unwrap_err();
        assert!(matches!(error, LayoutError::ParseState { .. }));
        assert!(!format!("{error:#?}").contains("secret-password"));
        assert!(matches!(
            error.source().unwrap().downcast_ref::<LayoutDiagnostic>(),
            Some(LayoutDiagnostic::Toml { span: Some(_) })
        ));
        fs::write(&path, [0xff]).unwrap();
        assert!(matches!(load_ui_state_from(&path), Err(LayoutError::StateNotUtf8 { .. })));
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
        let path = dir.path().join("state.toml");
        let original = "topics_width_rem = 31.5\npublish_open = true\n";
        fs::write(&path, original).unwrap();
        let error = write_toml(&path, &SecretSerializer).unwrap_err();
        assert!(matches!(error, LayoutError::SerializeState { .. }));
        assert_eq!(error.to_string(), "config.state.serialize_failed");
        assert_eq!(error.path(), Some(path.as_path()));
        assert_eq!(error.source().unwrap().to_string(), "config.diagnostic.toml_serialize");
        assert!(!format!("{error:#?}").contains("serializer-secret"));
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
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
}
