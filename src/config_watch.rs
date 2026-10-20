//! Native change notifications and content snapshots for application configuration.
//! The caller owns snapshot application; this module has no executor or GPUI state.

use std::{
    collections::BTreeSet,
    fmt, fs, io,
    path::{Path, PathBuf},
};

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::{
    config::{parse_config_bytes, prioritized_config_path},
    error::config::ConfigError,
};

/// One parsed file revision. Debug output deliberately excludes configuration data.
pub(crate) struct ConfigSnapshot {
    path: PathBuf,
    value: Value,
}

impl ConfigSnapshot {
    #[cfg(test)]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    #[cfg(test)]
    pub(crate) fn value(&self) -> &Value {
        &self.value
    }

    pub(crate) fn into_parts(self) -> (PathBuf, Value) {
        (self.path, self.value)
    }
}

impl fmt::Debug for ConfigSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConfigSnapshot")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

/// Compares explicit, previously resolved paths. Run filesystem work on the
/// background executor, checking at startup/resume and after native notifications.
/// No method calls application path locators or accesses the credential store.
pub(crate) struct ConfigWatch {
    config_path: PathBuf,
    state_path: PathBuf,
    observation: Observation,
    value: Option<Value>,
}

impl ConfigWatch {
    /// Observes the initial file without emitting a snapshot or baseline error.
    pub(crate) fn new(config_path: PathBuf, state_path: PathBuf) -> Self {
        let (observation, _) = observe(&config_path, &state_path);
        let value = match &observation {
            Observation::Content { path, bytes, .. } => parse_config_bytes(bytes, path).ok(),
            _ => None,
        };
        Self {
            config_path,
            state_path,
            observation,
            value,
        }
    }

    /// The explicit paths used to reconstruct the watch on resume.
    pub(crate) fn paths(&self) -> (PathBuf, PathBuf) {
        (self.config_path.clone(), self.state_path.clone())
    }

    /// Install native watches before the initial snapshot check.
    pub(crate) fn subscribe(&self) -> Result<ConfigEvents, ConfigError> {
        let (sender, receiver) = mpsc::channel(32);
        let absolute = |path: &Path| {
            std::path::absolute(path).map_err(|source| ConfigError::WatchConfig {
                path: path.to_owned(),
                source: notify::Error::io(source),
            })
        };
        let paths = (absolute(&self.config_path)?, absolute(&self.state_path)?);
        let watcher = native_watcher(&paths, sender.clone())?;
        Ok(ConfigEvents {
            watcher,
            paths,
            sender,
            receiver,
        })
    }

    /// Forces the next check to replay current valid content or an error, even
    /// when unchanged. Missing files still produce no snapshot. Call on initial
    /// watch start and after discarding a check made stale by a facade write.
    pub(crate) fn invalidate(&mut self) {
        self.observation = Observation::Unobserved;
        self.value = None;
    }

    /// Returns changed content or one error per changed observation. Missing files
    /// never produce a default configuration. Recovery and recreation are observed
    /// even if the recovered bytes match the last successful snapshot.
    /// Formatting-only changes are suppressed, but effective path changes are not.
    pub(crate) fn check(&mut self) -> Option<Result<ConfigSnapshot, ConfigError>> {
        let (observation, error) = observe(&self.config_path, &self.state_path);
        if observation == self.observation {
            return None;
        }
        let same_path = match (&self.observation, &observation) {
            (
                Observation::Content {
                    path: old_path,
                    target: old_target,
                    ..
                },
                Observation::Content { path, target, .. },
            ) => old_path == path && old_target == target,
            _ => false,
        };
        self.observation = observation;
        if let Some(error) = error {
            self.value = None;
            return Some(Err(error));
        }
        let Observation::Content { path, bytes, .. } = &self.observation else {
            self.value = None;
            return None;
        };
        match parse_config_bytes(bytes, path) {
            Ok(value) => {
                if same_path && self.value.as_ref() == Some(&value) {
                    return None;
                }
                self.value = Some(value.clone());
                Some(Ok(ConfigSnapshot { path: path.clone(), value }))
            }
            Err(error) => {
                self.value = None;
                Some(Err(error))
            }
        }
    }
}

/// Owns the native backend for the lifetime of the caller's async task.
/// Drop this value to unsubscribe (for example, while settings are open).
pub(crate) struct ConfigEvents {
    watcher: RecommendedWatcher,
    paths: (PathBuf, PathBuf),
    sender: mpsc::Sender<Result<(), ConfigError>>,
    receiver: mpsc::Receiver<Result<(), ConfigError>>,
}

impl ConfigEvents {
    /// Wait for an invalidation, not a snapshot. Refresh on the background
    /// executor before checking content, including after rescan notifications.
    pub(crate) async fn changed(&mut self) -> Option<Result<(), ConfigError>> {
        self.receiver.recv().await
    }

    /// Re-resolve ancestry and symlinks after an event. Install the replacement
    /// backend before dropping the old one, including watches invalidated by removal.
    pub(crate) fn refresh(&mut self) -> Result<(), ConfigError> {
        self.watcher = native_watcher(&self.paths, self.sender.clone())?;
        Ok(())
    }
}

fn native_watcher(paths: &(PathBuf, PathBuf), sender: mpsc::Sender<Result<(), ConfigError>>) -> Result<RecommendedWatcher, ConfigError> {
    // notify aliases RecommendedWatcher to its polling fallback on unsupported
    // platforms. Fail explicitly rather than silently reintroducing polling.
    if RecommendedWatcher::kind() == notify::WatcherKind::PollWatcher {
        return Err(ConfigError::WatchConfig {
            path: paths.0.clone(),
            source: notify::Error::generic("native filesystem notifications are unavailable"),
        });
    }
    let mut targets = BTreeSet::new();
    for path in [&paths.0, &paths.1] {
        resolve_watch_targets(path, &mut targets, 0)?;
    }
    let mut directories = BTreeSet::new();
    for target in &targets {
        // Only the nearest existing directory is needed. If it disappears its
        // own removal event prompts refresh to move to the next existing ancestor.
        for ancestor in target.ancestors().skip(1) {
            if ancestor.is_dir() {
                directories.insert(ancestor.to_owned());
                break;
            }
        }
    }
    let error_path = paths.0.clone();
    let callback_path = error_path.clone();
    let mut watcher = notify::recommended_watcher(move |result: notify::Result<Event>| {
        let message = match result {
            Ok(event) if relevant_event(&event, &targets) => Ok(()),
            Ok(_) => return,
            Err(source) => Err(ConfigError::WatchConfig {
                path: callback_path.clone(),
                source,
            }),
        };
        // Bounded invalidations coalesce under load: an already queued wakeup
        // always causes full watch refresh and snapshot comparison, never a delta.
        let _ = sender.try_send(message);
    })
    .map_err(|source| ConfigError::WatchConfig { path: error_path, source })?;
    for path in directories {
        watcher
            .watch(&path, RecursiveMode::NonRecursive)
            .map_err(|source| ConfigError::WatchConfig { path, source })?;
    }
    Ok(watcher)
}

fn relevant_event(event: &Event, targets: &BTreeSet<PathBuf>) -> bool {
    if event.need_rescan() {
        return true;
    }
    if matches!(event.kind, EventKind::Access(_)) {
        return false;
    }
    event.paths.is_empty() || event.paths.iter().any(|path| targets.iter().any(|target| target.starts_with(path)))
}

fn resolve_watch_targets(path: &Path, targets: &mut BTreeSet<PathBuf>, depth: usize) -> Result<(), ConfigError> {
    targets.insert(path.to_owned());
    // Inspect each component so directory symlinks and dangling file symlinks
    // retain both their lexical replacement watch and external target watch.
    for prefix in path.ancestors() {
        match fs::symlink_metadata(prefix) {
            Ok(metadata) if metadata.is_symlink() => {
                let link = fs::read_link(prefix).map_err(|source| ConfigError::WatchConfig {
                    path: prefix.to_owned(),
                    source: notify::Error::io(source),
                })?;
                targets.insert(prefix.to_owned());
                if depth >= 40 {
                    return Err(ConfigError::WatchConfig {
                        path: path.to_owned(),
                        source: notify::Error::generic("configuration symlink depth exceeded"),
                    });
                }
                let target = if link.is_absolute() {
                    link
                } else {
                    prefix.parent().unwrap_or(Path::new(".")).join(link)
                };
                let suffix = path.strip_prefix(prefix).unwrap_or(Path::new(""));
                resolve_watch_targets(&normalize_path(&target.join(suffix)), targets, depth + 1)?;
                // Keep inspecting lexical parents: the file itself may be a
                // symlink inside a separately replaceable directory symlink.
            }
            Ok(_) => {}
            Err(source)
                if matches!(
                    source.kind(),
                    io::ErrorKind::InvalidInput | io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) => {}
            Err(source) => {
                return Err(ConfigError::WatchConfig {
                    path: prefix.to_owned(),
                    source: notify::Error::io(source),
                });
            }
        }
    }
    Ok(())
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            std::path::Component::CurDir => {}
            _ => normalized.push(component),
        }
    }
    normalized
}

// Keep raw bytes, not timestamps or hashes: equal-length writes within a single
// filesystem clock tick and atomic replacements must be observable without collisions.
#[derive(PartialEq, Eq)]
enum Observation {
    Unobserved,
    Content {
        path: PathBuf,
        target: PathBuf,
        bytes: Vec<u8>,
    },
    Missing {
        path: PathBuf,
    },
    Failed {
        path: PathBuf,
        operation: &'static str,
        kind: io::ErrorKind,
        os_code: Option<i32>,
    },
}

fn observe(config_path: &Path, state_path: &Path) -> (Observation, Option<ConfigError>) {
    let path = match prioritized_config_path(state_path, config_path) {
        Ok(path) => path,
        Err(error) => {
            let observation = match &error {
                ConfigError::InspectConfig { path, source } => failed(path, "inspect", source),
                _ => Observation::Failed {
                    path: state_path.to_owned(),
                    operation: "inspect",
                    kind: io::ErrorKind::Other,
                    os_code: None,
                },
            };
            return (observation, Some(error));
        }
    };
    match fs::read(&path) {
        Ok(bytes) => match fs::canonicalize(&path) {
            Ok(target) => (Observation::Content { path, target, bytes }, None),
            Err(source) if source.kind() == io::ErrorKind::NotFound => (Observation::Missing { path }, None),
            Err(source) => (failed(&path, "inspect", &source), Some(ConfigError::InspectConfig { path, source })),
        },
        Err(source) if source.kind() == io::ErrorKind::NotFound => (Observation::Missing { path }, None),
        Err(source) => (failed(&path, "read", &source), Some(ConfigError::ReadConfig { path, source })),
    }
}

fn failed(path: &Path, operation: &'static str, source: &io::Error) -> Observation {
    Observation::Failed {
        path: path.to_owned(),
        operation,
        kind: source.kind(),
        os_code: source.raw_os_error(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(dir: &tempfile::TempDir) -> (PathBuf, PathBuf) {
        (dir.path().join("config.json"), dir.path().join("state.json"))
    }

    async fn notification(events: &mut ConfigEvents) {
        tokio::time::timeout(std::time::Duration::from_secs(5), events.changed())
            .await
            .expect("native notification timed out")
            .unwrap()
            .unwrap();
        events.refresh().unwrap();
    }

    async fn drain(events: &mut ConfigEvents) {
        while let Ok(message) = tokio::time::timeout(std::time::Duration::from_millis(100), events.changed()).await {
            message.unwrap().unwrap();
            events.refresh().unwrap();
        }
    }

    #[tokio::test]
    async fn native_writes_replacements_deletion_recreation_and_state_priority() {
        let dir = tempfile::tempdir().unwrap();
        let (config, state) = paths(&dir);
        fs::write(&config, b"{}").unwrap();
        let mut watch = ConfigWatch::new(config.clone(), state.clone());
        assert_eq!(watch.paths(), (config.clone(), state.clone()));
        let mut events = watch.subscribe().unwrap();
        for mode in ["write", "replace", "state", "remove_state", "delete", "recreate"] {
            drain(&mut events).await;
            match mode {
                "write" => fs::write(&config, br#"{"mode":"dark"}"#).unwrap(),
                "replace" => {
                    let replacement = dir.path().join("temporary");
                    fs::write(&replacement, b"{}").unwrap();
                    fs::rename(replacement, &config).unwrap();
                }
                "state" => fs::write(&state, b"{}").unwrap(),
                "remove_state" => fs::remove_file(&state).unwrap(),
                "delete" => fs::remove_file(&config).unwrap(),
                _ => fs::write(&config, b"{}").unwrap(),
            }
            notification(&mut events).await;
            let snapshot = watch.check();
            if mode == "delete" {
                assert!(snapshot.is_none());
            } else {
                assert_eq!(snapshot.unwrap().unwrap().path(), if mode == "state" { &state } else { &config });
            }
        }
    }

    #[tokio::test]
    async fn native_absent_parents_and_directory_recreation() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("missing/nested/config.json");
        let state = dir.path().join("state/config.json");
        let mut watch = ConfigWatch::new(config.clone(), state);
        let mut events = watch.subscribe().unwrap();
        fs::create_dir_all(config.parent().unwrap()).unwrap();
        fs::write(&config, b"{}").unwrap();
        notification(&mut events).await;
        assert!(watch.check().unwrap().is_ok());
        drain(&mut events).await;
        fs::remove_dir_all(dir.path().join("missing")).unwrap();
        notification(&mut events).await;
        assert!(watch.check().is_none());
        drain(&mut events).await;
        fs::create_dir_all(config.parent().unwrap()).unwrap();
        fs::write(&config, b"{}").unwrap();
        notification(&mut events).await;
        assert!(watch.check().unwrap().is_ok());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn native_external_symlink_target_writes_replacement_and_retarget() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        let (config, state) = paths(&dir);
        let first = external.path().join("first");
        let second = external.path().join("second");
        fs::write(&first, b"{}").unwrap();
        fs::write(&second, b"{}").unwrap();
        symlink(&first, &config).unwrap();
        let mut watch = ConfigWatch::new(config.clone(), state);
        let mut events = watch.subscribe().unwrap();
        fs::write(&first, br#"{"mode":"dark"}"#).unwrap();
        notification(&mut events).await;
        assert_eq!(watch.check().unwrap().unwrap().value()["mode"], "dark");
        drain(&mut events).await;
        fs::remove_file(&config).unwrap();
        symlink(&second, &config).unwrap();
        notification(&mut events).await;
        assert!(watch.check().unwrap().is_ok());
        drain(&mut events).await;
        fs::rename(&first, &second).unwrap();
        notification(&mut events).await;
        assert_eq!(watch.check().unwrap().unwrap().value()["mode"], "dark");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn native_directory_symlink_swap_and_dangling_external_state_target() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        let first = external.path().join("first");
        let second = external.path().join("second");
        fs::create_dir(&first).unwrap();
        fs::create_dir(&second).unwrap();
        let first_file = external.path().join("first.json");
        let second_file = external.path().join("second.json");
        fs::write(&first_file, b"{}").unwrap();
        fs::write(&second_file, b"{}").unwrap();
        symlink(&first_file, first.join("config.json")).unwrap();
        symlink(&second_file, second.join("config.json")).unwrap();
        let link = dir.path().join("config-dir");
        symlink(&first, &link).unwrap();
        let config = link.join("config.json");
        let state = dir.path().join("state.json");
        let mut watch = ConfigWatch::new(config.clone(), state.clone());
        let mut events = watch.subscribe().unwrap();
        fs::remove_file(&link).unwrap();
        symlink(&second, &link).unwrap();
        notification(&mut events).await;
        assert!(watch.check().unwrap().is_ok());
        drain(&mut events).await;
        fs::write(second.join("config.json"), br#"{"mode":"dark"}"#).unwrap();
        notification(&mut events).await;
        assert_eq!(watch.check().unwrap().unwrap().value()["mode"], "dark");
        drain(&mut events).await;
        let target = external.path().join("absent/state.json");
        symlink(&target, &state).unwrap();
        notification(&mut events).await;
        assert!(watch.check().is_none());
        drain(&mut events).await;
        fs::create_dir(target.parent().unwrap()).unwrap();
        fs::write(&target, b"{}").unwrap();
        notification(&mut events).await;
        assert_eq!(watch.check().unwrap().unwrap().path(), state);
    }

    #[tokio::test]
    async fn native_idle_reads_and_unrelated_paths_do_not_wake() {
        let dir = tempfile::tempdir().unwrap();
        let (config, state) = paths(&dir);
        fs::write(&config, b"{}").unwrap();
        let watch = ConfigWatch::new(config.clone(), state);
        let mut events = watch.subscribe().unwrap();
        fs::read(&config).unwrap();
        fs::write(dir.path().join("unrelated"), b"{}").unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(250), events.changed())
                .await
                .is_err()
        );
    }

    #[test]
    fn rescan_bypasses_path_filter_and_access_does_not() {
        let targets = BTreeSet::from([PathBuf::from("/config/settings.json")]);
        let event = Event::new(EventKind::Other).set_flag(notify::event::Flag::Rescan);
        assert!(relevant_event(&event, &targets));
        let event = Event::new(EventKind::Access(notify::event::AccessKind::Any)).add_path(PathBuf::from("/config/settings.json"));
        assert!(!relevant_event(&event, &targets));
    }

    #[test]
    fn invalidate_replays_identical_valid_baseline_once_per_invalidation() {
        let dir = tempfile::tempdir().unwrap();
        let (config, state) = paths(&dir);
        fs::write(&config, br#"{"mode":"dark"}"#).unwrap();
        let mut watch = ConfigWatch::new(config.clone(), state);
        assert!(watch.check().is_none());
        for _ in 0..2 {
            watch.invalidate();
            let snapshot = watch.check().unwrap().unwrap();
            assert_eq!(snapshot.path(), config);
            assert_eq!(snapshot.value()["mode"], "dark");
            assert!(watch.check().is_none());
        }
    }

    #[test]
    fn invalidate_replays_identical_malformed_baseline_once() {
        let dir = tempfile::tempdir().unwrap();
        let (config, state) = paths(&dir);
        fs::write(&config, b"{").unwrap();
        let mut watch = ConfigWatch::new(config, state);
        assert!(watch.check().is_none());
        for _ in 0..2 {
            watch.invalidate();
            assert!(matches!(watch.check(), Some(Err(ConfigError::ParseConfig { .. }))));
            assert!(watch.check().is_none());
        }
    }

    #[test]
    fn invalidate_replays_identical_io_failure_once() {
        let dir = tempfile::tempdir().unwrap();
        let (config, state) = paths(&dir);
        fs::create_dir(&config).unwrap();
        let mut watch = ConfigWatch::new(config, state);
        assert!(watch.check().is_none());
        for _ in 0..2 {
            watch.invalidate();
            assert!(matches!(watch.check(), Some(Err(ConfigError::ReadConfig { .. }))));
            assert!(watch.check().is_none());
        }
    }

    #[test]
    fn invalidate_missing_baseline_emits_nothing_and_still_detects_creation() {
        let dir = tempfile::tempdir().unwrap();
        let (config, state) = paths(&dir);
        let mut watch = ConfigWatch::new(config.clone(), state);
        assert!(watch.check().is_none());
        watch.invalidate();
        assert!(watch.check().is_none());
        assert!(watch.check().is_none());
        fs::write(&config, b"{}").unwrap();
        assert!(watch.check().unwrap().is_ok());
        fs::remove_file(&config).unwrap();
        watch.invalidate();
        assert!(watch.check().is_none());
        fs::write(&config, b"{}").unwrap();
        assert!(watch.check().unwrap().is_ok());
    }

    #[test]
    fn in_place_changes_are_detected_even_with_equal_timestamps_and_lengths() {
        let dir = tempfile::tempdir().unwrap();
        let (config, state) = paths(&dir);
        fs::write(&config, br#"{"mode":"light"}"#).unwrap();
        let modified = fs::metadata(&config).unwrap().modified().unwrap();
        let mut watch = ConfigWatch::new(config.clone(), state);
        assert!(watch.check().is_none());
        fs::write(&config, br#"{"mode":"night"}"#).unwrap();
        fs::File::options()
            .write(true)
            .open(&config)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(modified))
            .unwrap();
        let snapshot = watch.check().unwrap().unwrap();
        assert_eq!(snapshot.path(), config);
        assert_eq!(snapshot.value()["mode"], "night");
        assert!(watch.check().is_none());
    }

    #[test]
    fn atomic_replacements_are_detected() {
        let dir = tempfile::tempdir().unwrap();
        let (config, state) = paths(&dir);
        fs::write(&config, b"{}").unwrap();
        let mut watch = ConfigWatch::new(config.clone(), state);
        let temporary = dir.path().join("replacement.json");
        fs::write(&temporary, br#"{"mode":"dark"}"#).unwrap();
        fs::rename(temporary, &config).unwrap();
        assert_eq!(watch.check().unwrap().unwrap().value()["mode"], "dark");
    }

    #[test]
    fn initial_absence_creation_deletion_and_recreation_are_observed() {
        let dir = tempfile::tempdir().unwrap();
        let (config, state) = paths(&dir);
        let mut watch = ConfigWatch::new(config.clone(), state);
        assert!(watch.check().is_none());
        fs::write(&config, b"{}").unwrap();
        assert!(watch.check().unwrap().is_ok());
        fs::remove_file(&config).unwrap();
        assert!(watch.check().is_none());
        assert!(watch.check().is_none());
        fs::write(&config, b"{}").unwrap();
        assert!(watch.check().unwrap().is_ok());
    }

    #[test]
    fn malformed_revisions_are_reported_once_and_recovery_is_emitted() {
        let dir = tempfile::tempdir().unwrap();
        let (config, state) = paths(&dir);
        fs::write(&config, b"{}").unwrap();
        let mut watch = ConfigWatch::new(config.clone(), state);
        fs::write(&config, b"{").unwrap();
        assert!(matches!(watch.check(), Some(Err(ConfigError::ParseConfig { .. }))));
        assert!(watch.check().is_none());
        fs::write(&config, b"[").unwrap();
        assert!(matches!(watch.check(), Some(Err(ConfigError::ParseConfig { .. }))));
        fs::write(&config, b"{}").unwrap();
        assert!(watch.check().unwrap().is_ok());
    }

    #[test]
    fn malformed_baseline_is_suppressed_but_changed_malformed_bytes_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        let (config, state) = paths(&dir);
        fs::write(&config, b"{").unwrap();
        let mut watch = ConfigWatch::new(config.clone(), state);
        assert!(watch.check().is_none());
        fs::write(&config, b"[").unwrap();
        assert!(watch.check().unwrap().is_err());
    }

    #[test]
    fn io_failures_are_reported_once_and_can_recover_to_identical_content() {
        let dir = tempfile::tempdir().unwrap();
        let (config, state) = paths(&dir);
        fs::write(&config, b"{}").unwrap();
        let mut watch = ConfigWatch::new(config.clone(), state);
        fs::remove_file(&config).unwrap();
        fs::create_dir(&config).unwrap();
        assert!(matches!(watch.check(), Some(Err(ConfigError::ReadConfig { .. }))));
        assert!(watch.check().is_none());
        fs::remove_dir(&config).unwrap();
        fs::write(&config, b"{}").unwrap();
        assert!(watch.check().unwrap().is_ok());
    }

    #[test]
    fn state_creation_overrides_normal_config_and_removal_restores_it() {
        let dir = tempfile::tempdir().unwrap();
        let (config, state) = paths(&dir);
        fs::write(&config, b"{}").unwrap();
        let mut watch = ConfigWatch::new(config.clone(), state.clone());
        fs::write(&state, b"{}").unwrap();
        assert_eq!(watch.check().unwrap().unwrap().path(), state);
        fs::write(&config, br#"{"mode":"dark"}"#).unwrap();
        assert!(watch.check().is_none());
        fs::remove_file(&state).unwrap();
        let (path, value) = watch.check().unwrap().unwrap().into_parts();
        assert_eq!(path, config);
        assert_eq!(value["mode"], "dark");
    }

    #[test]
    fn malformed_state_does_not_fall_back_to_normal_config() {
        let dir = tempfile::tempdir().unwrap();
        let (config, state) = paths(&dir);
        fs::write(&config, b"{}").unwrap();
        let mut watch = ConfigWatch::new(config, state.clone());
        fs::write(&state, b"{").unwrap();
        assert!(watch.check().unwrap().is_err());
        assert!(watch.check().is_none());
    }

    #[test]
    fn formatting_only_changes_are_suppressed() {
        let dir = tempfile::tempdir().unwrap();
        let (config, state) = paths(&dir);
        fs::write(&config, br#"{"mode":"dark","reduce_motion":true}"#).unwrap();
        let mut watch = ConfigWatch::new(config.clone(), state);
        fs::write(&config, b"{\n  \"reduce_motion\": true, \"mode\": \"dark\"\n}").unwrap();
        assert!(watch.check().is_none());
        assert!(watch.check().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_retargeting_is_detected_even_with_identical_contents() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let (config, state) = paths(&dir);
        let first = dir.path().join("first.json");
        let second = dir.path().join("second.json");
        fs::write(&first, b"{}").unwrap();
        fs::write(&second, b"{}").unwrap();
        symlink(&first, &config).unwrap();
        let mut watch = ConfigWatch::new(config.clone(), state);
        fs::remove_file(&config).unwrap();
        symlink(&second, &config).unwrap();
        assert_eq!(watch.check().unwrap().unwrap().path(), config);
    }

    #[cfg(unix)]
    #[test]
    fn dangling_state_symlink_retains_precedence_until_target_is_created() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let (config, state) = paths(&dir);
        fs::write(&config, b"{}").unwrap();
        let target = dir.path().join("target.json");
        symlink(&target, &state).unwrap();
        let mut watch = ConfigWatch::new(config, state.clone());
        assert!(watch.check().is_none());
        fs::write(&target, b"{}").unwrap();
        assert_eq!(watch.check().unwrap().unwrap().path(), state);
    }

    #[test]
    fn snapshot_debug_does_not_include_contents() {
        let snapshot = ConfigSnapshot {
            path: PathBuf::from("config.json"),
            value: serde_json::json!({"password": "do-not-log"}),
        };
        assert!(!format!("{snapshot:?}").contains("do-not-log"));
    }
}
