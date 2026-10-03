//! Connection settings on disk and MQTT passwords in the system credential store.

use std::{
    collections::HashSet,
    fmt, fs,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow};
use directories::{BaseDirs, ProjectDirs};
use keyring::{Entry, Error as KeyringError};
use serde::{Deserialize, Serialize};

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

#[derive(Clone, Serialize, Deserialize)]
#[serde(default, from = "DeserializedConnectionConfig")]
#[non_exhaustive]
pub struct ConnectionConfig {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub topics: Vec<TopicSubscription>,
    pub username: String,
    #[serde(skip)]
    pub password: String,
    pub tls: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConnectionField {
    Name,
    Host,
    Port,
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
            topics: vec![TopicSubscription::default()],
            username: String::new(),
            password: String::new(),
            tls: false,
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
    topics: Option<Vec<TopicSubscription>>,
    base_topic: Option<String>,
    username: String,
    tls: bool,
}

impl Default for DeserializedConnectionConfig {
    fn default() -> Self {
        let config = ConnectionConfig::default();
        Self {
            name: config.name,
            host: config.host,
            port: config.port,
            topics: None,
            base_topic: None,
            username: config.username,
            tls: config.tls,
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
            topics,
            username: config.username,
            password: String::new(),
            tls: config.tls,
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

pub fn config_path() -> Result<PathBuf> {
    let dirs = ProjectDirs::from("", "", "mqtt-ui").context("Could not locate your application configuration directory.")?;
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
    selected: Option<usize>,
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
    for saved in stored.connections {
        let mut config = saved.config;
        config.validate()?;
        if !accounts.insert(credential_account(&config)) {
            return Err(anyhow!("Duplicate saved MQTT connection account."));
        }
        if saved.password_in_keyring {
            config.password = entry(&config)?.get_password()
                .map_err(credential_error)
                .context("Could not retrieve the MQTT password. Unlock your system credential store or enter the password again in Connection settings.")?;
        }
        connections.push(config);
    }
    // A stale index must not prevent access to the other saved connections.
    let selected = stored
        .selected
        .filter(|&index| index < connections.len())
        .or_else(|| (stored.selected.is_some() && !connections.is_empty()).then_some(0));
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
        if old.password_in_keyring && !retained_secrets.contains(&credential_account(&old.config)) {
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
            .map(|config| SavedConnection {
                config: config.clone(),
                password_in_keyring: !config.password.is_empty(),
            })
            .collect(),
        selected: saved.selected,
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

#[derive(Serialize, Deserialize)]
struct SavedConnection {
    #[serde(flatten)]
    config: ConnectionConfig,
    #[serde(default)]
    password_in_keyring: bool,
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
        KeyringError::NoStorageAccess(_) => "The system credential store is locked or access was denied.",
        KeyringError::NoDefaultStore => "The system credential store is unavailable.",
        KeyringError::BadEncoding(_) | KeyringError::BadDataFormat(_, _) => "The stored MQTT password could not be decoded.",
        _ => "The system credential store could not complete the operation.",
    })
}

#[derive(Default, Serialize, Deserialize)]
struct LayoutPreferences {
    topics_width_rem: Option<f32>,
    topics_width_fraction: Option<f32>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct TopicsLayout {
    pub width_rem: Option<f32>,
    pub width_fraction: Option<f32>,
}

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

fn load_topics_layout_from(path: &Path) -> Result<TopicsLayout> {
    let data = match fs::read(path) {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(TopicsLayout::default()),
        Err(error) => return Err(error).context("Could not read the application state."),
    };
    let preferences: LayoutPreferences = toml::from_str(std::str::from_utf8(&data).context("Application state is not valid UTF-8.")?)
        .context("Could not parse the application state.")?;
    Ok(TopicsLayout {
        width_rem: preferences.topics_width_rem.filter(|width| width.is_finite() && *width > 0.),
        width_fraction: preferences
            .topics_width_fraction
            .filter(|fraction| fraction.is_finite() && (0. ..=1.).contains(fraction)),
    })
}

pub(crate) fn save_topics_width(width: f32, fraction: f32) -> Result<()> {
    write_toml(
        &state_path()?,
        &LayoutPreferences {
            topics_width_rem: Some(width),
            topics_width_fraction: Some(fraction),
        },
    )
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
                inner: store.build("mqtt-ui", &credential_account(config), None)?,
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
    fn topics_width_roundtrips_through_toml_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("topq").join("state.toml");
        write_toml(
            &path,
            &LayoutPreferences {
                topics_width_rem: Some(31.5),
                topics_width_fraction: Some(0.4),
            },
        )
        .unwrap();
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
    fn saved_connections_migrate_legacy_filters_and_ignore_plaintext_passwords() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.json");
        fs::write(
            &path,
            r#"{"connections":[{"host":"legacy-broker","base_topic":"home/#","password":"plaintext-secret"},{"host":"default-broker"}],"selected":0}"#,
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
        let bytes = fs::read(&path).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(
            json["connections"]
                .as_array()
                .unwrap()
                .iter()
                .all(|item| item.get("password").is_none() && item.get("base_topic").is_none())
        );
        assert!(!std::str::from_utf8(&bytes).unwrap().contains(&first.password));
        assert!(!std::str::from_utf8(&bytes).unwrap().contains(&second.password));
    }

    #[test]
    fn selection_can_be_cleared_and_stale_index_falls_back_to_first() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connection.json");
        let entries = mock_entries();
        let mut saved = SavedConnections {
            connections: vec![ConnectionConfig::default()],
            selected: None,
        };
        save_connections_to(&path, &saved, &entries).unwrap();
        assert_eq!(load_connections_from(&path, &entries).unwrap().selected, None);
        fs::write(&path, r#"{"connections":[{"host":"localhost"}],"selected":99}"#).unwrap();
        assert_eq!(load_connections_from(&path, &entries).unwrap().selected, Some(0));
        saved.selected = Some(1);
        let before = fs::read(&path).unwrap();
        assert!(save_connections_to(&path, &saved, &entries).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
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
    fn missing_password_in_multi_store_is_an_error() {
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
        entries(&config).unwrap().delete_credential().unwrap();
        assert!(format!("{:#}", load_connections_from(&path, &entries).err().unwrap()).contains("saved MQTT password was not found"));
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
