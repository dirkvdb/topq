use super::*;

use std::{
    fs,
    time::{Duration, Instant},
};

use bytes::Bytes;
use chrono::Local;
use gpui_kit::component::Theme;
use gpui_kit::{AnyWindowHandle, Bounds, Entity, TestAppContext, WindowBounds, WindowOptions, px, size};
use serde_json::json;
use tokio::sync::mpsc;

use crate::{mqtt::BrokerEvent, topics::Message};

// Construct without a selection so the fixture cannot start MQTT or load credentials.
// A live event receiver makes unintended session restarts observable without a broker.
fn open(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<Explorer>, mpsc::Sender<BrokerEvent>) {
    // Native filesystem callbacks wake the executor from an OS-owned thread.
    cx.executor().allow_parking();
    let (sender, events) = mpsc::channel(4);
    let (handle, view) = cx.update(|cx| {
        if !cx.has_global::<Theme>() {
            gpui_kit::init(cx);
            appearance::register_bundled(cx).unwrap();
            super::super::init(cx);
        }
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(Default::default(), size(px(1000.), px(700.))))),
                ..gpui_kit::component::TitleBar::window_options()
            },
            cx,
            |window, cx| {
                cx.new(|cx| {
                    let mut view = Explorer::with_connections(SavedConnections::default(), None, None, None, window, cx);
                    let home = ConnectionConfig {
                        name: "Home".into(),
                        host: "home.invalid".into(),
                        client_id: "home-client".into(),
                        ..Default::default()
                    };
                    let work = ConnectionConfig {
                        name: "Work".into(),
                        host: "work.invalid".into(),
                        client_id: "work-client".into(),
                        ..Default::default()
                    };
                    view.saved_connections = SavedConnections {
                        connections: vec![home.clone(), work],
                        selected: Some(0),
                    };
                    view.active_config = Some(home);
                    view.status = ConnectionStatus::Connected;
                    view.show_config = false;
                    view.connection_form_open = false;
                    view.editing = Some(0);
                    view.topics.receive(
                        Message {
                            topic: "home/value".into(),
                            payload: Bytes::from_static(b"42"),
                            qos: 0,
                            retained: true,
                            received_at: Local::now(),
                            properties: Default::default(),
                        },
                        Instant::now(),
                    );
                    view.sync_tree(cx);
                    view.select_topic("home/value", window, cx);
                    view.listen_for_events(events, window, cx);
                    view
                })
            },
        )
        .unwrap()
    });
    cx.run_until_parked();
    (handle, view, sender)
}

fn snapshot(saved: &SavedConnections) -> Value {
    let connections: Vec<_> = saved
        .connections
        .iter()
        .map(|config| {
            let username = if config.username.is_empty() {
                String::new()
            } else {
                format!("{}@", config.username)
            };
            json!({
                "name": config.name,
                "connection": format!("{}{}{}:{}", config.protocol(), username, config.host, config.port),
                "client_id": config.client_id,
                "topics": config.topics,
                "validate_certificate": config.validate_certificate,
            })
        })
        .collect();
    json!({
        "connections": connections,
        "active_connection": saved.selected.map(|ix| &saved.connections[ix].name),
    })
}

fn assert_session(view: &Explorer, cx: &gpui_kit::App, sender: &mpsc::Sender<BrokerEvent>) {
    assert!(view.status.is_connected());
    assert!(view.active_config.is_some());
    assert!(view.event_task.is_some());
    assert!(!sender.is_closed(), "reload canceled the existing MQTT event listener");
    assert_eq!(view.selected.as_deref(), Some("home/value"));
    assert_eq!(view.topics.nodes["home/value"].value.as_ref().unwrap().payload.as_ref(), b"42");
    assert_eq!(view.payload.read(cx).value().as_str(), "42");
}

// Native callbacks run outside GPUI's deterministic executor. Only the test waits;
// the application remains parked on its subscription, with no clock advancement.
fn wait_for_config(cx: &mut TestAppContext, description: &str, mut condition: impl FnMut(&gpui_kit::App) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        cx.run_until_parked();
        if cx.update(|cx| condition(cx)) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {description}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn assert_during_native_callbacks(cx: &mut TestAppContext, mut check: impl FnMut(&gpui_kit::App)) {
    let deadline = Instant::now() + Duration::from_millis(100);
    loop {
        cx.run_until_parked();
        cx.update(|cx| check(cx));
        if Instant::now() >= deadline {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[gpui_kit::test]
fn valid_appearance_limits_sync_the_input_without_writing_files(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.json");
    let (handle, view, sender) = open(cx);
    fs::write(&path, b"{\"untouched\":true}\n").unwrap();
    let original = fs::read(&path).unwrap();
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            for limit in [0, 256] {
                let mut value = snapshot(&view.saved_connections);
                value["inline_payload_max_chars"] = json!(limit);
                value["inline_payload_limit_enabled"] = json!(true);
                view.apply_config_value(&path, value, window, cx);
                assert_eq!(Appearance::inline_payload_max_chars(cx), limit);
                assert!(Appearance::inline_payload_limit_enabled(cx));
                assert_eq!(view.inline_payload_max_chars.read(cx).value().as_str(), limit.to_string());
                assert!(view.error.is_none());
                assert_session(view, cx, &sender);
                assert_eq!(fs::read(&path).unwrap(), original);
            }
        });
    })
    .unwrap();
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[gpui_kit::test]
fn invalid_appearance_retains_both_sections_until_a_valid_correction(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.json");
    let (handle, view, sender) = open(cx);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            let previous = view.saved_connections.clone();
            let generation = view.settings_generation;
            let appearance = serde_json::to_value(cx.global::<Appearance>()).unwrap();
            let mut value = snapshot(&previous);
            value["connections"][1]["name"] = json!("Renamed work");
            value["inline_payload_max_chars"] = json!(-1);
            view.apply_config_value(&path, value.clone(), window, cx);
            assert!(view.saved_connections == previous);
            assert_eq!(serde_json::to_value(cx.global::<Appearance>()).unwrap(), appearance);
            assert_eq!(view.settings_generation, generation);
            assert!(view.error.is_some());
            assert_eq!(view.error, view.config_reload_error);
            assert_session(view, cx, &sender);

            value["inline_payload_max_chars"] = json!(64);
            view.apply_config_value(&path, value, window, cx);
            assert_eq!(Appearance::inline_payload_max_chars(cx), 64);
            assert_eq!(view.saved_connections.connections[1].name, "Renamed work");
            assert!(view.error.is_none());
            assert!(view.config_reload_error.is_none());
            assert_session(view, cx, &sender);
        });
    })
    .unwrap();
    assert!(!path.exists());
}

#[gpui_kit::test]
fn invalid_inactive_connection_rejects_the_entire_revision_and_correction_clears_error(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.json");
    let (handle, view, sender) = open(cx);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            let previous = view.saved_connections.clone();
            let generation = view.settings_generation;
            let appearance = serde_json::to_value(cx.global::<Appearance>()).unwrap();
            let mut value = snapshot(&previous);
            value["inline_payload_max_chars"] = json!(96);
            value["connections"][0]["name"] = json!("Renamed home");
            value["active_connection"] = json!("Renamed home");
            value["connections"][1]["topics"] = json!([{"topic": "bad/#/filter", "qos": 0}]);
            view.apply_config_value(&path, value.clone(), window, cx);
            assert!(view.saved_connections == previous);
            assert_eq!(serde_json::to_value(cx.global::<Appearance>()).unwrap(), appearance);
            assert_eq!(view.settings_generation, generation);
            assert!(view.error.is_some());
            assert_eq!(view.error, view.config_reload_error);
            assert_session(view, cx, &sender);

            value["connections"][1]["topics"] = json!([{"topic": "work/#", "qos": 1}]);
            view.apply_config_value(&path, value, window, cx);
            assert_eq!(Appearance::inline_payload_max_chars(cx), 96);
            assert_eq!(view.active_config.as_ref().unwrap().name, "Renamed home");
            assert!(view.error.is_none());
            assert!(view.config_reload_error.is_none());
            assert_session(view, cx, &sender);
        });
    })
    .unwrap();
    assert!(!path.exists());
}

#[gpui_kit::test]
fn appearance_edits_and_reloading_our_own_save_preserve_selection_and_topic_data(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.json");
    let (handle, view, sender) = open(cx);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            let previous = view.saved_connections.clone();
            let mut value = snapshot(&previous);
            // Like a user's appearance-only edit, retain all connection entries and
            // omit runtime client IDs to exercise their identity-preserving load path.
            for connection in value["connections"].as_array_mut().unwrap() {
                connection.as_object_mut().unwrap().remove("client_id");
            }
            value["inline_payload_max_chars"] = json!(128);
            view.apply_config_value(&path, value, window, cx);
            assert!(view.saved_connections == previous);
            assert_session(view, cx, &sender);

            // config_path is explicitly redirected to a thread-local tempdir in tests.
            config::save_connections(&view.saved_connections).unwrap();
            let saved_path = config::config_path().unwrap();
            let bytes = fs::read(&saved_path).unwrap();
            view.apply_config_value(&saved_path, serde_json::from_slice(&bytes).unwrap(), window, cx);
            assert!(view.saved_connections == previous);
            assert_session(view, cx, &sender);
            assert_eq!(fs::read(&saved_path).unwrap(), bytes);
            let generation = view.settings_generation;
            view.apply_config_value(&saved_path, serde_json::from_slice(&bytes).unwrap(), window, cx);
            assert_eq!(view.settings_generation, generation, "identical own-save reload must be a no-op");
        });
    })
    .unwrap();
}

#[gpui_kit::test]
fn reordering_and_renaming_the_selected_connection_does_not_restart_mqtt(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.json");
    let (handle, view, sender) = open(cx);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            let mut value = snapshot(&view.saved_connections);
            value["connections"].as_array_mut().unwrap().swap(0, 1);
            value["connections"][1]["name"] = json!("Renamed home");
            value["active_connection"] = json!("Renamed home");
            view.apply_config_value(&path, value, window, cx);
            assert_eq!(view.saved_connections.selected, Some(1));
            assert_eq!(view.editing, Some(1));
            assert_eq!(view.active_config.as_ref().unwrap().name, "Renamed home");
            assert_session(view, cx, &sender);
        });
    })
    .unwrap();
    sender.try_send(BrokerEvent::Connected).unwrap();
    cx.run_until_parked();
    cx.update(|cx| assert_session(view.read(cx), cx, &sender));
}

#[gpui_kit::test]
fn selected_broker_setting_change_reconnects_through_the_missing_password_branch(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.json");
    let (handle, view, sender) = open(cx);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            // Keep the credential account unchanged, including its cached empty
            // password: neither a keyring lookup nor a network connection is needed.
            view.saved_connections.connections[0].username = "reader".into();
            view.active_config.as_mut().unwrap().username = "reader".into();
            view.dismissed_missing_password_warning = true;
            let mut value = snapshot(&view.saved_connections);
            value["connections"][0]["client_id"] = json!("replacement-client");
            view.apply_config_value(&path, value, window, cx);
            assert_eq!(view.saved_connections.selected, Some(0));
            assert_eq!(view.saved_connections.connections[0].client_id, "replacement-client");
            assert!(view.saved_connections.connections[0].missing_password());
            assert!(view.active_config.is_none());
            assert!(view.connection.is_none());
            assert!(view.event_task.is_none());
            assert!(matches!(view.status, ConnectionStatus::Disconnected));
            assert!(view.topics.nodes.is_empty());
            assert!(view.selected.is_none());
            assert!(!view.dismissed_missing_password_warning);
            assert!(view.error.is_none());
        });
    })
    .unwrap();
    cx.run_until_parked();
    assert!(sender.is_closed());
}

#[gpui_kit::test]
fn null_selection_or_removing_the_active_connection_disconnects(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.json");
    for remove_active in [false, true] {
        let (handle, view, sender) = open(cx);
        cx.update_window(handle, |_, window, cx| {
            view.update(cx, |view, cx| {
                let mut value = snapshot(&view.saved_connections);
                if remove_active {
                    value["connections"] = json!([]);
                    // Removing the last connection also clears a stale active name.
                } else {
                    value["active_connection"] = Value::Null;
                }
                view.apply_config_value(&path, value, window, cx);
                assert_eq!(view.saved_connections.selected, None);
                assert!(view.active_config.is_none());
                assert!(view.connection.is_none());
                assert!(view.event_task.is_none());
                assert!(matches!(view.status, ConnectionStatus::Disconnected));
                assert!(view.topics.nodes.is_empty());
                assert!(view.selected.is_none());
                assert!(view.error.is_none());
            });
        })
        .unwrap();
        cx.run_until_parked();
        assert!(sender.is_closed());
    }
}

#[gpui_kit::test]
fn monitoring_pauses_in_settings_and_resumes_with_the_latest_file_after_closing(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.json");
    let (handle, view, sender) = open(cx);
    let mut value = cx.update(|cx| snapshot(&view.read(cx).saved_connections));
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    let watch = ConfigWatch::new(path.clone(), directory.path().join("state.json"));
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.watch_config(watch, window, cx);
        });
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            assert!(view.config_watch_task.is_some());
            assert_session(view, cx, &sender);
            view.open_connection(window, cx);
            assert!(view.config_watch_task.is_none());
            view.edit_connection(1, window, cx);
            view.host.update(cx, |input, cx| input.set_value("unsaved-draft", window, cx));
        });
    })
    .unwrap();
    cx.run_until_parked();
    let generation = cx.update(|cx| view.read(cx).settings_generation);
    fs::write(&path, b"{").unwrap();
    assert_during_native_callbacks(cx, |cx| {
        let view = view.read(cx);
        assert!(view.config_watch_task.is_none());
        assert!(view.error.is_none());
        assert!(view.config_reload_error.is_none());
        assert_eq!(Appearance::inline_payload_max_chars(cx), 10);
        assert_eq!(view.host.read(cx).value().as_str(), "unsaved-draft");
        assert_eq!(view.settings_generation, generation);
        assert_session(view, cx, &sender);
    });
    value["inline_payload_max_chars"] = json!(73);
    value["connections"][1]["connection"] = json!("mqtts://updated.invalid:8883");
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    assert_during_native_callbacks(cx, |cx| {
        let view = view.read(cx);
        assert!(view.config_watch_task.is_none());
        assert!(view.error.is_none());
        assert!(view.config_reload_error.is_none());
        assert_eq!(Appearance::inline_payload_max_chars(cx), 10);
        assert_eq!(view.saved_connections.connections[1].host, "work.invalid");
        assert_eq!(view.host.read(cx).value().as_str(), "unsaved-draft");
        assert_eq!(view.inline_payload_max_chars.read(cx).value().as_str(), "10");
        assert_eq!(view.settings_generation, generation);
    });
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            assert_eq!(Appearance::inline_payload_max_chars(cx), 10);
            assert_eq!(view.saved_connections.connections[1].host, "work.invalid");
            assert_eq!(view.host.read(cx).value().as_str(), "unsaved-draft");
            assert_eq!(view.editing, Some(1));
            assert!(view.show_config);
            assert_session(view, cx, &sender);
            view.cancel_connection(window, cx);
            assert!(view.config_watch_task.is_some());
        });
    })
    .unwrap();
    // Resume replays the latest snapshot immediately, without a new file event.
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            assert!(!view.show_config);
            assert_eq!(Appearance::inline_payload_max_chars(cx), 73);
            assert_eq!(view.saved_connections.connections[1].host, "updated.invalid");
            assert!(view.error.is_none());
            assert!(view.config_reload_error.is_none());
            assert_session(view, cx, &sender);
            view.open_connection(window, cx);
            assert!(view.config_watch_task.is_none());
            view.edit_connection(1, window, cx);
            assert_eq!(view.host.read(cx).value().as_str(), "updated.invalid");
            assert_eq!(view.port.read(cx).value().as_str(), "8883");
        });
    })
    .unwrap();
}

#[gpui_kit::test]
fn pending_reads_and_errors_are_discarded_if_settings_open_before_delivery(cx: &mut TestAppContext) {
    for malformed in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        let (handle, view, sender) = open(cx);
        let mut value = cx.update(|cx| snapshot(&view.read(cx).saved_connections));
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        let mut watch = ConfigWatch::new(path.clone(), directory.path().join("state.json"));
        value["inline_payload_max_chars"] = json!(73);
        fs::write(
            &path,
            if malformed {
                b"{".to_vec()
            } else {
                serde_json::to_vec(&value).unwrap()
            },
        )
        .unwrap();
        let pending = watch.check().unwrap();
        cx.update_window(handle, |_, window, cx| {
            view.update(cx, |view, cx| {
                let original = Appearance::inline_payload_max_chars(cx);
                let revision = config::application_config_revision();
                view.open_connection(window, cx);
                let focused = window.focused(cx);
                assert!(!view.apply_config_change(pending, revision, window, cx));
                assert_eq!(Appearance::inline_payload_max_chars(cx), original);
                assert!(view.show_config);
                assert_eq!(window.focused(cx), focused);
                assert!(view.config_reload_error.is_none());
                assert!(view.error.is_none());
                assert_session(view, cx, &sender);
                value["inline_payload_max_chars"] = json!(78);
                fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
                view.cancel_connection(window, cx);
                watch.invalidate();
                assert!(view.apply_config_change(watch.check().unwrap(), revision, window, cx));
                assert_eq!(Appearance::inline_payload_max_chars(cx), 78);
                assert_session(view, cx, &sender);
            });
        })
        .unwrap();
    }
}

#[gpui_kit::test]
fn watcher_replays_a_baseline_newer_than_the_initial_appearance_load(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.json");
    let (handle, view, sender) = open(cx);
    let mut value = cx.update(|cx| snapshot(&view.read(cx).saved_connections));
    value["inline_payload_max_chars"] = json!(31);
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    let watch = ConfigWatch::new(path, directory.path().join("state.json"));
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| view.watch_config(watch, window, cx));
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        assert_eq!(Appearance::inline_payload_max_chars(cx), 31);
        assert!(view.read(cx).config_watch_task.is_some());
        assert_session(view.read(cx), cx, &sender);
    });
}

#[gpui_kit::test]
fn queued_reload_cannot_undo_a_newer_in_app_save(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.json");
    let (handle, view, sender) = open(cx);
    let value = cx.update(|cx| snapshot(&view.read(cx).saved_connections));
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    let mut watch = ConfigWatch::new(path.clone(), directory.path().join("state.json"));
    watch.invalidate();
    let pending = watch.check().unwrap();
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            let revision = config::application_config_revision();
            config::update_application_config(&path, &json!({"inline_payload_max_chars": 27})).unwrap();
            let current = config::read_config_value(&path).unwrap().unwrap();
            view.apply_config_value(&path, current, window, cx);
            assert_eq!(Appearance::inline_payload_max_chars(cx), 27);
            assert!(!view.apply_config_change(pending, revision, window, cx));
            assert_eq!(Appearance::inline_payload_max_chars(cx), 27);
            assert_session(view, cx, &sender);
            watch.invalidate();
            assert!(view.apply_config_change(watch.check().unwrap(), config::application_config_revision(), window, cx));
            assert_eq!(Appearance::inline_payload_max_chars(cx), 27);
            assert_session(view, cx, &sender);
        });
    })
    .unwrap();
}

#[gpui_kit::test]
fn watcher_task_applies_native_file_events_and_recovers_from_bad_json(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.json");
    let state = directory.path().join("state.json");
    let (handle, view, sender) = open(cx);
    let mut value = cx.update(|cx| snapshot(&view.read(cx).saved_connections));
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    let watch = ConfigWatch::new(path.clone(), state);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| view.watch_config(watch, window, cx));
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        assert_eq!(Appearance::inline_payload_max_chars(cx), 10);
        assert_session(view.read(cx), cx, &sender);
    });
    value["inline_payload_max_chars"] = json!(73);
    let bytes = serde_json::to_vec(&value).unwrap();
    fs::write(&path, &bytes).unwrap();
    wait_for_config(cx, "native config edit", |cx| Appearance::inline_payload_max_chars(cx) == 73);
    cx.update(|cx| {
        assert_eq!(view.read(cx).inline_payload_max_chars.read(cx).value().as_str(), "73");
        assert_session(view.read(cx), cx, &sender);
    });

    fs::write(&path, b"{").unwrap();
    wait_for_config(cx, "malformed config notification", |cx| {
        view.read(cx).config_reload_error.is_some()
    });
    cx.update(|cx| {
        assert_eq!(view.read(cx).error, view.read(cx).config_reload_error);
        assert_eq!(Appearance::inline_payload_max_chars(cx), 73);
        assert_session(view.read(cx), cx, &sender);
    });
    fs::write(&path, &bytes).unwrap();
    wait_for_config(cx, "config recovery notification", |cx| view.read(cx).config_reload_error.is_none());
    cx.update(|cx| {
        assert!(view.read(cx).error.is_none());
        assert_eq!(Appearance::inline_payload_max_chars(cx), 73);
        assert_session(view.read(cx), cx, &sender);
    });
    assert_eq!(fs::read(&path).unwrap(), bytes);
    view.update(cx, |view, _| view.pause_config_watch());
    cx.run_until_parked();
}

#[gpui_kit::test]
fn closing_settings_reselects_state_priority_and_falls_back_after_state_deletion(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join(".config/topq/config.json");
    let state = directory.path().join(".local/state/topq/config.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::create_dir_all(state.parent().unwrap()).unwrap();
    let (handle, view, sender) = open(cx);
    let mut normal = cx.update(|cx| snapshot(&view.read(cx).saved_connections));
    normal["inline_payload_max_chars"] = json!(31);
    fs::write(&path, serde_json::to_vec(&normal).unwrap()).unwrap();
    let watch = ConfigWatch::new(path.clone(), state.clone());
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| view.watch_config(watch, window, cx));
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            assert_eq!(Appearance::inline_payload_max_chars(cx), 31);
            assert_eq!(view.config_watch_paths, Some((path.clone(), state.clone())));
            assert!(view.config_watch_task.is_some());
            view.open_connection(window, cx);
            assert!(view.config_watch_task.is_none());
        });
    })
    .unwrap();
    cx.run_until_parked();

    let mut preferred = normal.clone();
    preferred["inline_payload_max_chars"] = json!(73);
    preferred["connections"][1]["name"] = json!("State work");
    fs::write(&state, serde_json::to_vec(&preferred).unwrap()).unwrap();
    normal["inline_payload_max_chars"] = json!(41);
    fs::write(&path, serde_json::to_vec(&normal).unwrap()).unwrap();
    assert_during_native_callbacks(cx, |cx| {
        let view = view.read(cx);
        assert!(view.show_config);
        assert!(view.config_watch_task.is_none());
        assert_eq!(Appearance::inline_payload_max_chars(cx), 31);
        assert_eq!(view.saved_connections.connections[1].name, "Work");
        assert!(view.error.is_none());
        assert_session(view, cx, &sender);
    });
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.cancel_connection(window, cx);
            assert!(view.config_watch_task.is_some());
        });
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        assert_eq!(Appearance::inline_payload_max_chars(cx), 73);
        assert_eq!(view.read(cx).saved_connections.connections[1].name, "State work");
        assert_session(view.read(cx), cx, &sender);
    });

    // The resumed subscription must still monitor both candidates, not freeze the
    // previously selected normal path or treat its edits as effective revisions.
    normal["inline_payload_max_chars"] = json!(42);
    fs::write(&path, serde_json::to_vec(&normal).unwrap()).unwrap();
    assert_during_native_callbacks(cx, |cx| {
        assert_eq!(Appearance::inline_payload_max_chars(cx), 73);
        assert_eq!(view.read(cx).saved_connections.connections[1].name, "State work");
        assert!(view.read(cx).config_reload_error.is_none());
        assert_session(view.read(cx), cx, &sender);
    });
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.open_connection(window, cx);
            assert!(view.config_watch_task.is_none());
        });
    })
    .unwrap();
    cx.run_until_parked();
    fs::remove_file(&state).unwrap();
    normal["inline_payload_max_chars"] = json!(43);
    fs::write(&path, serde_json::to_vec(&normal).unwrap()).unwrap();
    assert_during_native_callbacks(cx, |cx| {
        assert!(view.read(cx).show_config);
        assert!(view.read(cx).config_watch_task.is_none());
        assert_eq!(Appearance::inline_payload_max_chars(cx), 73);
    });
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| view.cancel_connection(window, cx));
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        let view = view.read(cx);
        assert!(!view.show_config);
        assert!(view.config_watch_task.is_some());
        assert_eq!(view.config_watch_paths, Some((path.clone(), state.clone())));
        assert_eq!(Appearance::inline_payload_max_chars(cx), 43);
        assert_eq!(view.saved_connections.connections[1].name, "Work");
        assert_session(view, cx, &sender);
    });
    normal["inline_payload_max_chars"] = json!(44);
    fs::write(&path, serde_json::to_vec(&normal).unwrap()).unwrap();
    wait_for_config(cx, "normal edit after settings fallback", |cx| {
        Appearance::inline_payload_max_chars(cx) == 44
    });
    cx.update(|cx| assert_session(view.read(cx), cx, &sender));
    view.update(cx, |view, _| view.pause_config_watch());
    cx.run_until_parked();
}

#[gpui_kit::test]
fn native_state_creation_and_deletion_switch_priority_while_settings_are_closed(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join(".config/topq/config.json");
    let state = directory.path().join(".local/state/topq/config.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::create_dir_all(state.parent().unwrap()).unwrap();
    let (handle, view, sender) = open(cx);
    let mut normal = cx.update(|cx| snapshot(&view.read(cx).saved_connections));
    normal["inline_payload_max_chars"] = json!(31);
    fs::write(&path, serde_json::to_vec(&normal).unwrap()).unwrap();
    let watch = ConfigWatch::new(path.clone(), state.clone());
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| view.watch_config(watch, window, cx));
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| assert_eq!(Appearance::inline_payload_max_chars(cx), 31));

    let mut preferred = normal.clone();
    preferred["inline_payload_max_chars"] = json!(73);
    preferred["connections"][1]["name"] = json!("State work");
    fs::write(&state, serde_json::to_vec(&preferred).unwrap()).unwrap();
    wait_for_config(cx, "native state creation", |cx| Appearance::inline_payload_max_chars(cx) == 73);
    cx.update(|cx| {
        assert_eq!(view.read(cx).saved_connections.connections[1].name, "State work");
        assert_session(view.read(cx), cx, &sender);
    });

    normal["inline_payload_max_chars"] = json!(41);
    fs::write(&path, serde_json::to_vec(&normal).unwrap()).unwrap();
    assert_during_native_callbacks(cx, |cx| {
        let view = view.read(cx);
        assert!(!view.show_config);
        assert!(view.config_watch_task.is_some());
        assert_eq!(Appearance::inline_payload_max_chars(cx), 73);
        assert_eq!(view.saved_connections.connections[1].name, "State work");
        assert!(view.config_reload_error.is_none());
        assert_session(view, cx, &sender);
    });
    preferred["inline_payload_max_chars"] = json!(74);
    fs::write(&state, serde_json::to_vec(&preferred).unwrap()).unwrap();
    wait_for_config(cx, "native preferred state edit", |cx| {
        Appearance::inline_payload_max_chars(cx) == 74
    });
    fs::remove_file(&state).unwrap();
    wait_for_config(cx, "native state deletion and normal fallback", |cx| {
        Appearance::inline_payload_max_chars(cx) == 41
    });
    cx.update(|cx| {
        assert_eq!(view.read(cx).saved_connections.connections[1].name, "Work");
        assert_session(view.read(cx), cx, &sender);
    });
    normal["inline_payload_max_chars"] = json!(42);
    fs::write(&path, serde_json::to_vec(&normal).unwrap()).unwrap();
    wait_for_config(cx, "native normal edit after fallback", |cx| {
        Appearance::inline_payload_max_chars(cx) == 42
    });
    cx.update(|cx| {
        assert!(!view.read(cx).show_config);
        assert!(view.read(cx).error.is_none());
        assert_session(view.read(cx), cx, &sender);
    });
    view.update(cx, |view, _| view.pause_config_watch());
    cx.run_until_parked();
}

#[gpui_kit::test]
fn save_and_connect_missing_password_resumes_native_monitoring(cx: &mut TestAppContext) {
    // Both application paths are thread-local temporary directories under cfg(test).
    // Saving an empty password cannot access the real credential store or MQTT.
    let (path, state) = config::application_config_paths().unwrap();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let (handle, view, sender) = open(cx);
    let mut value = cx.update(|cx| {
        view.update(cx, |view, _| {
            view.saved_connections.connections[0].username = "reader".into();
            view.active_config.as_mut().unwrap().username = "reader".into();
            snapshot(&view.saved_connections)
        })
    });
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    let watch = ConfigWatch::new(path.clone(), state);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| view.watch_config(watch, window, cx));
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            assert_session(view, cx, &sender);
            view.open_connection(window, cx);
            view.edit_connection(0, window, cx);
            assert!(view.config_watch_task.is_none());
            assert_eq!(view.username.read(cx).value().as_str(), "reader");
            assert!(view.password.read(cx).value().is_empty());
        });
    })
    .unwrap();
    cx.run_until_parked();
    value["inline_payload_max_chars"] = json!(73);
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.connect_from_form(window, cx);
            assert!(!view.show_config);
            assert!(view.config_watch_task.is_some());
            assert!(view.saved_connections.connections[0].missing_password());
            assert!(view.active_config.is_none());
            assert!(view.connection.is_none());
            assert!(view.event_task.is_none());
            assert!(matches!(view.status, ConnectionStatus::Disconnected));
            assert!(view.error.is_none());
        });
    })
    .unwrap();
    cx.run_until_parked();
    assert!(sender.is_closed());
    cx.update(|cx| assert_eq!(Appearance::inline_payload_max_chars(cx), 73));
    value["inline_payload_max_chars"] = json!(82);
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    wait_for_config(cx, "native edit after Save & Connect closes settings", |cx| {
        Appearance::inline_payload_max_chars(cx) == 82
    });
    cx.update(|cx| {
        let view = view.read(cx);
        assert!(view.error.is_none());
        assert!(view.config_reload_error.is_none());
        assert!(view.active_config.is_none());
        assert!(view.connection.is_none());
        assert!(view.event_task.is_none());
        assert!(matches!(view.status, ConnectionStatus::Disconnected));
    });
    view.update(cx, |view, _| view.pause_config_watch());
    cx.run_until_parked();
}
