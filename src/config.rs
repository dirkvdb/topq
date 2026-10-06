//! Connection settings on disk and MQTT passwords in the system credential store.

use std::{
    collections::{HashSet, hash_map::RandomState},
    fmt, fs,
    hash::{BuildHasher, Hash, Hasher},
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow};
#[cfg(not(test))]
use directories::{BaseDirs, ProjectDirs};
use keyring::{Entry, Error as KeyringError};
use serde::{Deserialize, Serialize, ser::SerializeStruct};
use url::Url;

/// An MQTT topic filter and its requested maximum delivery QoS.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TopicSubscription {
    pub topic: String,
    pub qos: u8,
}

impl Default for TopicSubscription {
    fn default() -> Self {
        Self { topic: "#".into(), qos: 0 }
    }
}

impl TopicSubscription {
    /// Rejects invalid MQTT filters, null characters, filters over 65535 bytes, and QoS outside 0..=2.
    pub fn validate(&self) -> Result<(), ConnectionValidationError> {
        if self.topic.len() > usize::from(u16::MAX) || self.topic.contains('\0') || !rumqttc::valid_filter(&self.topic) {
            return Err(ConnectionValidationError {
                field: ConnectionField::Topics,
                message: "Enter a valid MQTT topic filter (e.g. # or home/#), up to 65535 bytes. Use + for one level and # only as the last level.",
            });
        }
        if self.qos > 2 {
            return Err(ConnectionValidationError {
                field: ConnectionField::Topics,
                message: "Choose a topic subscription QoS between 0 and 2.",
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

#[derive(Debug)]
pub struct ConnectionValidationError {
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
                |topic| vec![TopicSubscription { topic, qos: 2 }],
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
        if self.client_id.trim().is_empty() || self.client_id.len() > usize::from(u16::MAX) || self.client_id.contains('\0') {
            return Err(ConnectionValidationError {
                field: ConnectionField::ClientId,
                message: "Enter a non-empty MQTT client ID up to 65535 bytes, without null characters.",
            });
        }
        Ok(())
    }

    pub(crate) fn validate_broker_connection(&self) -> Result<(), ConnectionValidationError> {
        self.validate_broker_endpoint()?;
        if self.username.is_empty() && !self.password.is_empty() {
            return Err(ConnectionValidationError {
                field: ConnectionField::Username,
                message: "Enter a username when using a password.",
            });
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<(), ConnectionValidationError> {
        self.validate_broker_endpoint()?;
        if self.topics.is_empty() {
            return Err(ConnectionValidationError {
                field: ConnectionField::Topics,
                message: "Add at least one topic subscription.",
            });
        }
        let mut filters = HashSet::new();
        for subscription in &self.topics {
            subscription.validate()?;
            if !filters.insert(subscription.topic.as_str()) {
                return Err(ConnectionValidationError {
                    field: ConnectionField::Topics,
                    message: "Each topic filter may only be subscribed to once.",
                });
            }
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
pub fn config_path() -> Result<PathBuf> {
    Ok(TEST_CONFIG_DIR.with(|dir| dir.path().join("topq").join("connections.json")))
}

#[cfg(not(test))]
pub fn config_path() -> Result<PathBuf> {
    let dirs = ProjectDirs::from("", "", "topq").context("Could not locate your application configuration directory.")?;
    Ok(dirs.config_dir().join("connections.json"))
}

/// An absent selection means no connection is active, even when the list is nonempty.
#[derive(Default)]
pub(crate) struct SavedConnections {
    pub connections: Vec<ConnectionConfig>,
    pub selected: Option<usize>,
}

pub(crate) fn load_connections() -> Result<SavedConnections> {
    load_connections_from(&config_path()?, &password_entry)
}

pub(crate) fn save_connections(saved: &SavedConnections) -> Result<()> {
    save_connections_to(&config_path()?, saved, &password_entry)
}

#[derive(Default, Serialize, Deserialize)]
struct StoredConnections {
    connections: Vec<SavedConnection>,
    active_connection: Option<String>,
}

fn read_connections(path: &Path) -> Result<Option<StoredConnections>> {
    let data = match fs::read(path) {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("Could not read the saved connections."),
    };
    let stored = serde_json::from_slice(&data).context("Could not parse the saved connections.")?;
    Ok(Some(stored))
}

fn load_connections_from(path: &Path, entry: &impl Fn(&ConnectionConfig) -> Result<Entry>) -> Result<SavedConnections> {
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
            return Err(anyhow!("Duplicate saved MQTT connection account."));
        }
        if !config.username.is_empty() {
            config.password = match entry(&config)?.get_password() {
                Ok(password) => password,
                Err(KeyringError::NoEntry) => String::new(),
                Err(error) => return Err(credential_error(error))
                    .context("Could not retrieve the MQTT password. Unlock your system credential store or enter the password again in Connection settings."),
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

fn set_credential(config: &ConnectionConfig, password: Option<&str>, entry: &impl Fn(&ConnectionConfig) -> Result<Entry>) -> Result<()> {
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
    entry: &impl Fn(&ConnectionConfig) -> Result<Entry>,
) -> Result<()> {
    let mut failed = false;
    for &index in applied.iter().rev() {
        let change = &changes[index];
        if set_credential(change.config, change.previous.as_deref(), entry).is_err() {
            failed = true;
        }
    }
    if failed {
        return Err(anyhow!("Could not restore all previous MQTT passwords."));
    }
    Ok(())
}

fn save_connections_to(path: &Path, saved: &SavedConnections, entry: &impl Fn(&ConnectionConfig) -> Result<Entry>) -> Result<()> {
    if saved.selected.is_some_and(|index| index >= saved.connections.len()) {
        return Err(anyhow!("Selected connection index is out of range."));
    }
    let mut accounts = HashSet::new();
    for config in &saved.connections {
        config.validate()?;
        if !accounts.insert(credential_account(config)) {
            return Err(anyhow!(
                "Duplicate MQTT connection account; each broker and username must be unique."
            ));
        }
    }
    let previous = read_connections(path)?.unwrap_or_default();
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
            Err(error) => return Err(credential_error(error)).context("Could not inspect a saved MQTT password."),
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
                    .context("Could not update an MQTT password in the system credential store.")?;
                applied.push(index);
            }
        }
        write_json(path, &stored)
    })();
    if let Err(error) = result {
        if rollback_credentials(&changes, &applied, entry).is_err() {
            return Err(error.context("Could not restore all previous MQTT passwords; check the system credential store."));
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

fn connection_uri(config: &ConnectionConfig) -> Result<String> {
    let host = if config.host.contains(':') {
        format!("[{}]", config.host)
    } else {
        config.host.clone()
    };
    let mut url =
        Url::parse(&format!("{}{host}:{}", config.protocol(), config.port)).context("Could not format the broker connection URI.")?;
    url.set_username(&config.username)
        .map_err(|()| anyhow!("Could not encode the MQTT username in the broker connection URI."))?;
    let uri = url.as_str();
    Ok(uri.strip_suffix('/').unwrap_or(uri).to_owned())
}

fn parse_connection_uri(connection: &str) -> Result<ConnectionAddress> {
    let url = Url::parse(connection).context("The saved broker connection URI is invalid.")?;
    if url.password().is_some() || !matches!(url.path(), "" | "/") || url.query().is_some() || url.fragment().is_some() {
        return Err(anyhow!(
            "The broker connection URI must contain only a scheme, host, and optional port."
        ));
    }
    let (tls, websocket, default_port) = match url.scheme() {
        "mqtt" => (false, false, 1883),
        "mqtts" => (true, false, 8883),
        "ws" => (false, true, 80),
        "wss" => (true, true, 443),
        _ => return Err(anyhow!("The broker connection URI scheme must be mqtt, mqtts, ws, or wss.")),
    };
    let host = match url.host().context("The broker connection URI must include a host.")? {
        url::Host::Domain(host) => host.to_owned(),
        url::Host::Ipv4(host) => host.to_string(),
        url::Host::Ipv6(host) => host.to_string(),
    };
    let port = url.port().unwrap_or(default_port);
    let username = percent_encoding::percent_decode_str(url.username())
        .decode_utf8()
        .context("The broker connection URI username is not valid UTF-8.")?
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
fn password_entry(config: &ConnectionConfig) -> Result<Entry> {
    use keyring_core::api::CredentialStoreApi;

    TEST_CREDENTIAL_STORE.with(|store| {
        Ok(Entry {
            inner: store.build("topq", &credential_account(config), None)?,
        })
    })
}

#[cfg(not(test))]
fn password_entry(config: &ConnectionConfig) -> Result<Entry> {
    Entry::new("topq", &credential_account(config))
        .map_err(credential_error)
        .context("Could not open the system credential store.")
}

fn credential_error(error: KeyringError) -> anyhow::Error {
    // Keyring errors may carry secret bytes. Do not propagate their Debug
    // representation or sources to the UI or logs.
    anyhow!(match error {
        KeyringError::NoEntry => "The saved MQTT password was not found.",
        KeyringError::NoStorageAccess(_) => "The system credential store is locked or access was denied.",
        KeyringError::NoDefaultStore => "The system credential store is unavailable.",
        KeyringError::BadEncoding(_) | KeyringError::BadDataFormat(_, _) => "The stored MQTT password could not be decoded.",
        _ => "The system credential store could not complete the operation.",
    })
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
fn state_path() -> Result<PathBuf> {
    std::thread_local! {
        static TEST_STATE_DIR: tempfile::TempDir = tempfile::tempdir()
            .expect("Could not create a temporary application state directory.");
    }
    Ok(TEST_STATE_DIR.with(|dir| dir.path().join("topq").join("state.toml")))
}

#[cfg(not(test))]
fn state_path() -> Result<PathBuf> {
    let state_dir = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .map(Ok)
        .unwrap_or_else(|| {
            BaseDirs::new()
                .map(|dirs| dirs.home_dir().join(".local").join("state"))
                .context("Could not locate your home directory.")
        })?;
    Ok(state_dir.join("topq").join("state.toml"))
}

pub(crate) fn load_topics_layout() -> Result<TopicsLayout> {
    load_topics_layout_from(&state_path()?)
}

fn load_layout_preferences_from(path: &Path) -> Result<LayoutPreferences> {
    let data = match fs::read(path) {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(LayoutPreferences::default()),
        Err(error) => return Err(error).context("Could not read the application state."),
    };
    toml::from_str(std::str::from_utf8(&data).context("Application state is not valid UTF-8.")?)
        .context("Could not parse the application state.")
}

fn load_topics_layout_from(path: &Path) -> Result<TopicsLayout> {
    let preferences = load_layout_preferences_from(path)?;
    Ok(TopicsLayout {
        width_rem: preferences.topics_width_rem.filter(|width| width.is_finite() && *width > 0.),
        width_fraction: preferences
            .topics_width_fraction
            .filter(|fraction| fraction.is_finite() && (0. ..=1.).contains(fraction)),
    })
}

pub(crate) fn load_publish_layout() -> Result<PublishLayout> {
    load_publish_layout_from(&state_path()?)
}

fn load_publish_layout_from(path: &Path) -> Result<PublishLayout> {
    let preferences = load_layout_preferences_from(path)?;
    Ok(PublishLayout {
        open: preferences.publish_open.unwrap_or_default(),
        height_rem: preferences.publish_height_rem.filter(|height| height.is_finite() && *height > 0.),
        height_fraction: preferences
            .publish_height_fraction
            .filter(|fraction| fraction.is_finite() && (0. ..=1.).contains(fraction)),
    })
}

pub(crate) fn save_topics_width(width: f32, fraction: f32) -> Result<()> {
    save_topics_width_to(&state_path()?, width, fraction)
}

fn save_topics_width_to(path: &Path, width: f32, fraction: f32) -> Result<()> {
    let mut preferences = load_layout_preferences_from(path)?;
    preferences.topics_width_rem = Some(width);
    preferences.topics_width_fraction = Some(fraction);
    write_toml(path, &preferences)
}

pub(crate) fn save_publish_height(height_rem: f32, fraction: f32) -> Result<()> {
    save_publish_height_to(&state_path()?, height_rem, fraction)
}

fn save_publish_height_to(path: &Path, height_rem: f32, fraction: f32) -> Result<()> {
    let mut preferences = load_layout_preferences_from(path)?;
    preferences.publish_height_rem = Some(height_rem);
    preferences.publish_height_fraction = Some(fraction);
    write_toml(path, &preferences)
}

pub(crate) fn save_publish_open(open: bool) -> Result<()> {
    save_publish_open_to(&state_path()?, open)
}

fn save_publish_open_to(path: &Path, open: bool) -> Result<()> {
    let mut preferences = load_layout_preferences_from(path)?;
    preferences.publish_open = Some(open);
    write_toml(path, &preferences)
}

fn write_toml(path: &Path, value: &impl Serialize) -> Result<()> {
    fs::create_dir_all(path.parent().context("Invalid state path.")?).context("Could not create the application state directory.")?;
    let temp = path.with_extension("toml.tmp");
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp).context("Could not open the temporary state file.")?;
    file.write_all(toml::to_string_pretty(value)?.as_bytes())
        .context("Could not write the application state.")?;
    file.sync_all().context("Could not flush the application state to disk.")?;
    fs::rename(&temp, path).context("Could not save the application state.")?;
    Ok(())
}

pub(crate) fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    fs::create_dir_all(path.parent().context("Invalid configuration path.")?).context("Could not create the configuration directory.")?;
    let temp = path.with_extension("json.tmp");
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp).context("Could not open the temporary settings file.")?;
    file.write_all(&serde_json::to_vec_pretty(value)?)
        .context("Could not write the settings file.")?;
    file.sync_all().context("Could not flush the settings file to disk.")?;
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
                inner: store.build("topq", &credential_account(config), None)?,
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
        for (host, port) in [("", 1883), ("mqtt://localhost", 1883), ("localhost", 0)] {
            let config = ConnectionConfig {
                host: host.into(),
                port,
                ..Default::default()
            };
            assert!(config.validate().is_err());
        }
        for client_id in [String::new(), "bad\0id".into(), "x".repeat(usize::from(u16::MAX) + 1)] {
            let config = ConnectionConfig {
                client_id,
                ..Default::default()
            };
            assert_eq!(config.validate().unwrap_err().field(), ConnectionField::ClientId);
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
        let expected = vec![TopicSubscription { topic: "#".into(), qos: 0 }];
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
                qos: 2
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
                qos: 1
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
            let subscription = TopicSubscription { topic, qos: 0 };
            assert_eq!(subscription.validate().unwrap_err().field(), ConnectionField::Topics);
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
            for qos in 0..=2 {
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
    fn validate_rejects_invalid_subscription_qos() {
        for qos in [3, u8::MAX] {
            let subscription = TopicSubscription {
                topic: "home/#".into(),
                qos,
            };
            assert_eq!(subscription.validate().unwrap_err().field(), ConnectionField::Topics);
            let config = ConnectionConfig {
                topics: vec![TopicSubscription::default(), subscription],
                ..Default::default()
            };
            assert_eq!(config.validate().unwrap_err().field(), ConnectionField::Topics);
        }
    }

    #[test]
    fn validate_requires_topics_and_rejects_duplicate_filters_regardless_of_qos() {
        for topics in [
            vec![],
            vec![TopicSubscription::default(), TopicSubscription::default()],
            vec![TopicSubscription::default(), TopicSubscription { topic: "#".into(), qos: 2 }],
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
                    qos: 1,
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
    fn multi_connection_file_uses_a_new_path() {
        assert_eq!(config_path().unwrap().file_name().unwrap(), "connections.json");
    }

    #[test]
    fn application_persistence_uses_temporary_configuration_and_mock_credentials() {
        let path = config_path().unwrap();
        TEST_CONFIG_DIR.with(|dir| {
            assert_eq!(path, dir.path().join("topq").join("connections.json"));
            assert!(path.with_file_name("appearance.json").starts_with(dir.path()));
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
                qos: 2
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
        let entries = |_: &ConnectionConfig| -> Result<Entry> { panic!("invalid format must not access keyring") };
        assert!(load_connections_from(&path, &entries).is_err());
        assert!(save_connections_to(&path, &SavedConnections::default(), &entries).is_err());
        assert_eq!(fs::read(path).unwrap(), original);
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
                    qos: 0,
                },
                TopicSubscription {
                    topic: "office/+/temperature".into(),
                    qos: 1,
                },
                TopicSubscription {
                    topic: "$SYS/#".into(),
                    qos: 2,
                },
            ],
            ..authenticated_config()
        };
        let second = ConnectionConfig {
            name: "Workshop".into(),
            topics: vec![TopicSubscription {
                topic: "$SYS/#".into(),
                qos: 1,
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
        assert!(save_connections_to(&path, &saved, &entries).is_err());
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
        let entries = |_: &ConnectionConfig| -> Result<Entry> { panic!("anonymous connections must not access the keychain") };
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
        assert!(format!("{error:#}").contains("Could not retrieve the MQTT password"));
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
