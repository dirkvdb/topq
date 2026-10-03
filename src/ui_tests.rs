use bytes::Bytes;
use chrono::Local;
use gpui_kit::component::{ActiveTheme, Size, Theme, ThemeMode};
use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    AnyWindowHandle, AppContext, Bounds, Entity, Styled, TestAppContext, WindowBounds,
    WindowOptions, px, size,
};

use super::{ConnectionStatus, Explorer};
use crate::{config::ConnectionConfig, topics::Message};

fn open(
    cx: &mut TestAppContext,
    form: bool,
    width: f32,
    height: f32,
) -> (AnyWindowHandle, Entity<Explorer>) {
    cx.update(|cx| {
        if !cx.has_global::<Theme>() {
            gpui_kit::init(cx);
            crate::appearance::register_bundled(cx).unwrap();
            super::init(cx);
        }
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    Default::default(),
                    size(px(width), px(height)),
                ))),
                ..Default::default()
            },
            cx,
            |window, cx| {
                cx.new(|cx| {
                    let mut view = Explorer::with_settings(None, None, None, window, cx);
                    if !form {
                        view.show_config = false;
                        view.active_config = Some(ConnectionConfig::default());
                        view.status = ConnectionStatus::Connected;
                        for topic in ["home/a", "home/b"] {
                            view.topics
                                .receive(message(topic), std::time::Instant::now());
                        }
                        view.sync_tree(cx);
                        view.focus_topics(window, cx);
                    }
                    view
                })
            },
        )
        .unwrap()
    })
}

fn message(topic: &str) -> Message {
    Message {
        topic: topic.into(),
        payload: Bytes::from_static(b"42"),
        qos: 0,
        retained: true,
        received_at: Local::now(),
    }
}

#[gpui_kit::test]
fn theme_menu_supports_keyboard_selection_dismissal_and_saved_preferences(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("appearance.json");
    let (handle, _) = open(cx, true, 760., 540.);
    cx.update(|cx| cx.set_global(crate::appearance::load(&path).unwrap()));
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("theme", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("popup-menu").visible());
        window.press("escape", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("popup-menu").is_none());
        assert_eq!(window.find("host").focused(), Some(true));
        window.press("shift-tab", cx);
        assert_eq!(window.find("theme").focused(), Some(true));
        window.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        // Follow system, Default Light, then Ayu Light; headings are skipped.
        window.press("down", cx);
        window.press("down", cx);
        window.press("down", cx);
        window.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        assert_eq!(cx.theme().theme_name().as_str(), "Ayu Light");
        crate::appearance::sync_system(window, cx);
        assert_eq!(cx.theme().theme_name().as_str(), "Ayu Light");
        window.render_frame(cx);
        assert!(window.try_find("popup-menu").is_none());
        assert_eq!(window.find("theme").focused(), Some(true));
    })
    .unwrap();
    cx.update(|cx| {
        cx.set_global(crate::appearance::load(&path).unwrap());
        assert_eq!(
            crate::appearance::Appearance::selected(cx),
            Some("Ayu Light")
        );
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("theme", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.press("up", cx);
        let mut menu = window.within("popup-menu");
        assert_eq!(menu.find(20usize).label(), Some("Tokyo Night"));
        menu.click(20usize, cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(cx.theme().theme_name().as_str(), "Tokyo Night");
        assert_eq!(cx.theme().highlight_theme.name, "Tokyo Night");
        crate::appearance::sync_system(window, cx);
        assert_eq!(cx.theme().theme_name().as_str(), "Tokyo Night");
        window.click("theme", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.within("popup-menu").click(0usize, cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        assert!(matches!(
            cx.theme().theme_name().as_str(),
            "Ayu Light" | "Ayu Dark"
        ));
        cx.set_global(crate::appearance::load(&path).unwrap());
        assert_eq!(crate::appearance::Appearance::selected(cx), None);
    });
}

#[gpui_kit::test]
fn enter_keeps_invalid_port_next_to_its_field_and_returns_focus(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, true, 760., 540.);
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("port", cx);
        window.press("secondary-a", cx);
        window.input("invalid", cx);
        window.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let error = window.find("port-error");
        assert_eq!(error.label(), Some("Enter a port between 1 and 65535."));
        assert_eq!(window.find("port").focused(), Some(true));
        assert!(error.bounds().top() >= window.find("port").bounds().bottom());
        assert!(view.read(cx).connection.is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
fn pending_connection_prevents_duplicate_submission(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, true, 1200., 760.);
    view.update(cx, |view, cx| {
        view.status = ConnectionStatus::Connecting;
        cx.notify();
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("connect", cx);
        window.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| assert!(view.read(cx).connection.is_none()));
}

#[gpui_kit::test]
fn tree_keyboard_selection_survives_insertions_before_the_selected_topic(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 1200., 760.);
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(view.read(cx).selected, None);
        assert!(window.try_find("selected-topic").is_none());
        assert!(window.find("empty-topic-selection").visible());
        assert!(window.find("details-pane").visible());
    })
    .unwrap();
    cx.update_window(handle, |_, window, cx| {
        window.press("home", cx);
        window.press("right", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.press("down", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("selected-topic").value(), Some("home/a"));
        assert_eq!(view.read(cx).payload.read(cx).value().as_str(), "42");
        window.click("copy-value", cx);
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("42".into())
        );
    })
    .unwrap();
    view.update(cx, |view, cx| {
        view.topics
            .receive(message("home/0"), std::time::Instant::now());
        view.sync_tree(cx);
        cx.notify();
    });
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("selected-topic").value(), Some("home/a"));
        window.press("secondary-1", cx);
        window.press("end", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| assert_eq!(view.read(cx).selected.as_deref(), Some("home/b")));
}

#[gpui_kit::test]
fn topic_rows_use_compact_spacing_and_remain_comfortable_at_larger_text_sizes(
    cx: &mut TestAppContext,
) {
    for font_size in [16., 20.] {
        let (handle, _) = open(cx, false, 1200., 760.);
        cx.update(|cx| Theme::update(cx, |theme| theme.font_size = px(font_size)));
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let row = window.find("topic:home");
            let height = row.bounds().size.height;
            assert!(height >= px(24.), "topic row is too small: {height:?}");
            assert!(
                height < window.rem_size() * 2.,
                "topic row did not use compact spacing: {height:?}"
            );
        })
        .unwrap();
    }
}

#[gpui_kit::test]
fn escape_cancels_connection_edits_and_restores_the_trigger_focus(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 1200., 760.);
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.press("shift-tab", cx);
        assert_eq!(window.find("settings").focused(), Some(true));
        window.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("connection-form").visible());
        window.click("port", cx);
        window.press("secondary-a", cx);
        window.input("9999", cx);
        window.press("escape", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(!view.read(cx).show_config);
        assert_eq!(window.find("settings").focused(), Some(true));
        window.click("settings", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("port").value(), Some("1883"));
    })
    .unwrap();
}

#[gpui_kit::test]
fn pane_headings_align_in_both_themes_at_minimum_size_and_larger_text(cx: &mut TestAppContext) {
    for mode in [ThemeMode::Light, ThemeMode::Dark] {
        for font_size in [16., 20.] {
            let (handle, _) = open(cx, false, 760., 540.);
            cx.update(|cx| {
                Theme::change(mode, None, cx);
                Theme::update(cx, |theme| theme.font_size = px(font_size));
            });
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.press("home", cx);
            })
            .unwrap();
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                let topics = window.find("topics-heading").bounds();
                let details = window.find("details-heading").bounds();
                assert_eq!(topics.top(), details.top());
                assert_eq!(topics.size.height, details.size.height);
                assert!(window.find("copy-topic").visible());
                assert!(window.find("status").visible());
            })
            .unwrap();
        }
    }
}

#[gpui_kit::test]
fn reduced_motion_hides_update_flashes_without_losing_selection(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 1200., 760.);
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.press("home", cx);
    })
    .unwrap();
    cx.run_until_parked();
    view.update(cx, |view, cx| {
        view.topics.receive(
            message("home/a"),
            std::time::Instant::now() - std::time::Duration::from_millis(100),
        );
        cx.notify();
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("topic:home").visible());
        assert!(window.try_find("update:home").is_none());
        let explorer = view.read(cx);
        let entry = explorer.tree_state.read(cx).entry(0).unwrap();
        let mut row = explorer.topic_row(entry, cx);
        assert_ne!(row.style().text.color, Some(cx.theme().foreground));
    })
    .unwrap();
    cx.update(|cx| cx.set_reduce_motion(true));
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let explorer = view.read(cx);
        let entry = explorer.tree_state.read(cx).entry(0).unwrap();
        let mut row = explorer.topic_row(entry, cx);
        assert_eq!(row.style().text.color, Some(cx.theme().foreground));
        assert_eq!(window.find("selected-topic").value(), Some("home"));
    })
    .unwrap();
}

#[gpui_kit::test]
fn long_topic_and_payload_remain_usable_at_minimum_size_with_large_text(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 760., 540.);
    let topic = format!("home/{}", "temperature_測定値_".repeat(20));
    cx.update(|cx| Theme::update(cx, |theme| theme.font_size = px(20.)));
    view.update(cx, |view, cx| {
        view.topics
            .receive(message(&topic), std::time::Instant::now());
        view.sync_tree(cx);
        cx.notify();
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.press("home", cx);
        window.press("right", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.press("end", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("selected-topic").value(), Some(topic.as_str()));
        let payload = window.find("payload");
        assert!(payload.visible());
        assert!(payload.bounds().size.height > px(0.));
        assert!(payload.bounds().bottom() <= window.find("status").bounds().top());
        assert_eq!(window.find("copy-topic").label(), Some("Copy topic name"));
        assert_eq!(window.find("copy-value").label(), Some("Copy latest value"));
    })
    .unwrap();
}

#[gpui_kit::test]
fn selected_topic_has_room_for_the_full_text_line_at_different_font_sizes(cx: &mut TestAppContext) {
    for font_size in [14., 16., 20., 24.] {
        let (handle, view) = open(cx, false, 760., 540.);
        cx.update(|cx| Theme::update(cx, |theme| theme.font_size = px(font_size)));
        view.update(cx, |view, cx| {
            view.topics
                .receive(message("energy/solar"), std::time::Instant::now());
            view.sync_tree(cx);
            cx.notify();
        });
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.press("home", cx);
            window.press("right", cx);
            window.press("down", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("selected-topic").value(), Some("energy/solar"));
            let state = view.read(cx).topic_name.read(cx);
            let bounds = state.input_bounds();
            let line_height = state.line_height().unwrap();
            let frame = window.find("selected-topic").bounds();
            let inset = Size::Medium.input_py() + px(1.);
            assert!(bounds.top() >= frame.top() + inset && bounds.bottom() <= frame.bottom() - inset,
                "topic text is clipped at base font {font_size}: frame {frame:?}, text {bounds:?}, line height {line_height:?}");
            window.click("selected-topic", cx);
            window.press("secondary-a", cx);
            window.press("secondary-c", cx);
            assert_eq!(cx.read_from_clipboard().and_then(|item| item.text()), Some("energy/solar".into()));
        }).unwrap();
    }
}

#[gpui_kit::test]
fn payload_language_follows_selection_and_json_remains_readonly_and_copyable(
    cx: &mut TestAppContext,
) {
    let (handle, view) = open(cx, false, 1200., 760.);
    let json = br#"{"mode":"heat","temperature":23.3,"enabled":true,"error":null}"#;
    view.update(cx, |view, cx| {
        for (topic, payload) in [
            ("home/a", json.as_slice()),
            ("home/b", b"online".as_slice()),
            ("home/c", &[0xff, 0x00]),
        ] {
            let mut message = message(topic);
            message.payload = Bytes::copy_from_slice(payload);
            view.topics.receive(message, std::time::Instant::now());
        }
        view.sync_tree(cx);
        cx.notify();
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.press("home", cx);
        window.press("right", cx);
        window.press("down", cx);
    })
    .unwrap();
    cx.run_until_parked();
    let formatted =
        serde_json::to_string_pretty(&serde_json::from_slice::<serde_json::Value>(json).unwrap())
            .unwrap();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            view.read(cx).payload.read(cx).language_name().as_str(),
            "json"
        );
        assert_eq!(view.read(cx).payload.read(cx).value().as_str(), formatted);
        window.click("payload", cx);
        window.press("secondary-a", cx);
        window.input("must not replace the payload", cx);
        window.press("secondary-c", cx);
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some(formatted.clone())
        );
        assert_eq!(view.read(cx).payload.read(cx).value().as_str(), formatted);
        window.click("copy-value", cx);
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some(std::str::from_utf8(json).unwrap().to_owned())
        );
        window.press("secondary-1", cx);
        window.press("down", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(view.read(cx).payload_format, "Text");
        assert_eq!(
            view.read(cx).payload.read(cx).language_name().as_str(),
            "plaintext"
        );
        assert_eq!(view.read(cx).payload.read(cx).value().as_str(), "online");
        window.press("down", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(view.read(cx).payload_format, "Binary · hex");
        assert_eq!(
            view.read(cx).payload.read(cx).language_name().as_str(),
            "plaintext"
        );
        assert_eq!(view.read(cx).payload.read(cx).value().as_str(), "ff 00");
    })
    .unwrap();
}

#[gpui_kit::test]
fn metadata_tags_leave_room_for_json_at_minimum_size_in_both_themes(cx: &mut TestAppContext) {
    for mode in [ThemeMode::Light, ThemeMode::Dark] {
        let (handle, view) = open(cx, false, 760., 540.);
        cx.update(|cx| {
            Theme::change(mode, None, cx);
            Theme::update(cx, |theme| theme.font_size = px(20.));
        });
        view.update(cx, |view, _| {
            let mut message = message("home/a");
            message.payload = Bytes::from_static(br#"{"mode":"heat","temperature":23.3}"#);
            view.topics.receive(message, std::time::Instant::now());
        });
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.press("home", cx);
            window.press("right", cx);
            window.press("down", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let metadata = window.find("topic-metadata");
            let payload = window.find("payload");
            assert!(metadata.visible());
            assert!(metadata.bounds().bottom() <= payload.bounds().top());
            assert!(payload.visible());
            assert!(payload.bounds().size.height > px(0.));
            assert!(payload.bounds().bottom() <= window.find("status").bounds().top());
        })
        .unwrap();
    }
}
