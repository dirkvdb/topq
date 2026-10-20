//! Applying external configuration revisions without persisting them back to disk.

use std::path::Path;

use gpui_kit::{AppContext, Context, Window};
use serde_json::Value;

use super::{ConnectionStatus, Explorer, messages::LocalizedError};
use crate::{
    appearance::{self, Appearance},
    config::{self, ConnectionConfig, SavedConnections},
    config_watch::{ConfigSnapshot, ConfigWatch},
    error::config::ConfigError,
};

impl Explorer {
    pub(super) fn watch_config(&mut self, mut watch: ConfigWatch, window: &mut Window, cx: &mut Context<Self>) {
        self.pause_config_watch();
        self.config_watch_paths = Some(watch.paths());
        if self.show_config {
            return;
        }
        // Subscribe before reading so startup/resume cannot miss a concurrent save.
        watch.invalidate();
        self.config_watch_task = Some(cx.spawn_in(window, async move |view, cx| {
            let (mut watch, subscription) = cx
                .background_spawn(async move {
                    let subscription = watch.subscribe();
                    (watch, subscription)
                })
                .await;
            let mut events = match subscription {
                Ok(events) => events,
                Err(error) => {
                    let _ = view.update_in(cx, |view, _, cx| view.config_reload_failed(error, cx));
                    return;
                }
            };
            loop {
                let Ok(Some(revision)) = view.update_in(cx, |view, _, _| (!view.show_config).then(config::application_config_revision))
                else {
                    break;
                };
                let (next_watch, next_events, change) = cx
                    .background_spawn(async move {
                        let change = match events.refresh() {
                            Ok(()) => watch.check(),
                            Err(error) => Some(Err(error)),
                        };
                        (watch, events, change)
                    })
                    .await;
                watch = next_watch;
                events = next_events;
                if let Some(change) = change {
                    match view.update_in(cx, |view, window, cx| view.apply_config_change(change, revision, window, cx)) {
                        Ok(true) => {}
                        Ok(false) => {
                            watch.invalidate();
                            continue;
                        }
                        Err(_) => break,
                    }
                }
                match events.changed().await {
                    Some(Ok(())) => {}
                    Some(Err(error)) => {
                        let _ = view.update_in(cx, |view, _, cx| view.config_reload_failed(error, cx));
                        break;
                    }
                    None => break,
                }
            }
        }));
    }

    pub(super) fn pause_config_watch(&mut self) {
        // Canceling the task drops its native subscription, not just its snapshots.
        self.config_watch_task = None;
    }

    pub(super) fn resume_config_watch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.show_config
            && self.config_watch_task.is_none()
            && let Some((config, state)) = self.config_watch_paths.clone()
        {
            self.watch_config(ConfigWatch::new(config, state), window, cx);
        }
    }

    fn apply_config_change(
        &mut self,
        change: Result<ConfigSnapshot, ConfigError>,
        revision: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        // Settings may have opened during the read. Retry after closing instead of
        // changing the dialog's state or acknowledging this observation. Also reject
        // reads predating a save (the title-bar connection picker can save settings).
        if self.show_config || revision != config::application_config_revision() {
            return false;
        }
        match change {
            Ok(snapshot) => {
                let (path, value) = snapshot.into_parts();
                self.apply_config_value(&path, value, window, cx);
            }
            Err(error) => self.config_reload_failed(error, cx),
        }
        true
    }

    pub(super) fn config_reload_failed(&mut self, error: ConfigError, cx: &mut Context<Self>) {
        tracing::warn!(error_code = error.code(), diagnostic = ?error, "config.reload_failed");
        self.show_config_reload_error(error.localized_message(), cx);
    }

    fn show_config_reload_error(&mut self, message: String, cx: &mut Context<Self>) {
        self.error = Some(message.clone());
        self.config_reload_error = Some(message);
        cx.notify();
    }

    fn apply_config_value(&mut self, path: &Path, value: Value, window: &mut Window, cx: &mut Context<Self>) {
        // Validate the entire revision before changing either live settings section.
        let preferences = match appearance::from_config(value.clone(), path, cx) {
            Ok(preferences) => preferences,
            Err(error) => {
                tracing::warn!(error_code = error.code(), diagnostic = ?error, "config.reload_failed");
                self.show_config_reload_error(error.localized_message(), cx);
                return;
            }
        };
        let connections = match config::load_connections_from_value(value, path, &self.saved_connections) {
            Ok(connections) => connections,
            Err(error) => {
                self.config_reload_failed(error, cx);
                return;
            }
        };
        let appearance_changed = match appearance::apply_loaded(preferences, window, cx) {
            Ok(changed) => changed,
            Err(error) => {
                tracing::warn!(error_code = error.code(), diagnostic = ?error, "config.reload_failed");
                self.show_config_reload_error(error.localized_message(), cx);
                return;
            }
        };
        let cleared_error = self.config_reload_error.take().is_some_and(|message| {
            if self.error.as_ref() == Some(&message) {
                self.error = None;
                true
            } else {
                false
            }
        });
        let connections_changed = connections != self.saved_connections;
        if appearance_changed {
            self.inline_payload_max_chars.update(cx, |input, cx| {
                input.set_value(Appearance::inline_payload_max_chars(cx).to_string(), window, cx);
            });
            self.refresh_animation(window, cx);
        }
        if connections_changed {
            self.apply_config_connections(connections, window, cx);
        }
        if appearance_changed || connections_changed {
            self.settings_generation = self.settings_generation.wrapping_add(1);
        }
        if appearance_changed || connections_changed || cleared_error {
            cx.notify();
        }
    }

    fn apply_config_connections(&mut self, connections: SavedConnections, window: &mut Window, cx: &mut Context<Self>) {
        let previous_selected = self
            .saved_connections
            .selected
            .and_then(|ix| self.saved_connections.connections.get(ix));
        let selected = connections.selected.and_then(|ix| connections.connections.get(ix)).cloned();
        let reconnect = match (previous_selected, selected.as_ref()) {
            (Some(previous), Some(selected)) => !same_broker_settings(previous, selected),
            (None, None) => false,
            _ => true,
        };
        self.editing = connections.selected;
        self.saved_connections = connections;

        // Settings are closed during reload; their inputs are populated on opening.
        let focused = window.focused(cx);
        if reconnect {
            self.active_config = None;
            if let Some(config) = selected {
                self.start_connection(config, window, cx);
            } else {
                self.event_task = None;
                self.connection = None;
                self.status = ConnectionStatus::Disconnected;
                self.dismissed_missing_password_warning = false;
                self.clear_topics(window, cx);
            }
        } else if let (Some(active), Some(selected)) = (&mut self.active_config, selected) {
            // Renaming/reordering a connection does not require restarting its session.
            active.name = selected.name;
        }
        if let Some(focused) = focused {
            focused.focus(window, cx);
        }
    }
}

#[cfg(test)]
#[path = "config_reload_tests.rs"]
mod tests;

fn same_broker_settings(left: &ConnectionConfig, right: &ConnectionConfig) -> bool {
    left.host == right.host
        && left.port == right.port
        && left.client_id == right.client_id
        && left.topics == right.topics
        && left.username == right.username
        && left.password == right.password
        && left.tls == right.tls
        && left.validate_certificate == right.validate_certificate
        && left.websocket == right.websocket
}
