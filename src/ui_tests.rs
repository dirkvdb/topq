use bytes::Bytes;
use chrono::Local;
use gpui_kit::component::{ActiveTheme, Theme, ThemeMode, WindowExt};
use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    AnyWindowHandle, App, AppContext, Bounds, Entity, Focusable, Styled, TestAppContext, Window, WindowBounds, WindowOptions, px, size,
};

use super::{ConnectionStatus, Explorer};
use crate::{
    appearance::{Appearance, AppearanceMode},
    config::{ConnectionConfig, ConnectionField, TopicSubscription},
    topics::Message,
};

fn open(cx: &mut TestAppContext, form: bool, width: f32, height: f32) -> (AnyWindowHandle, Entity<Explorer>) {
    cx.update(|cx| {
        if !cx.has_global::<Theme>() {
            gpui_kit::init(cx);
            crate::appearance::register_bundled(cx).unwrap();
            super::init(cx);
        }
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(Default::default(), size(px(width), px(height))))),
                ..gpui_kit::component::TitleBar::window_options()
            },
            cx,
            |window, cx| {
                cx.new(|cx| {
                    let mut view = Explorer::with_settings(None, None, None, window, cx);
                    if !form {
                        view.show_config = false;
                        view.editing = Some(0);
                        let config = ConnectionConfig {
                            name: "Home".into(),
                            ..Default::default()
                        };
                        view.active_config = Some(config.clone());
                        view.saved_connections.connections.push(config);
                        view.saved_connections.selected = Some(0);
                        view.status = ConnectionStatus::Connected;
                        for topic in ["home/a", "home/b"] {
                            view.topics.receive(message(topic), std::time::Instant::now());
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

#[gpui_kit::test]
fn publish_topic_tab_cycles_inline_proposals_without_changing_typed_text(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 1200., 760.);
    cx.update_window(handle, |_, window, _| window.activate_window()).unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("toggle-publish", cx);
        window.click("publish-topic", cx);
        window.input("home/", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("publish-topic-proposal").label(), Some("a"));
        assert!(window.try_find("publish-topic-suggestions").is_none());
    })
    .unwrap();
    for expected in ["b", "a", "b"] {
        cx.update_window(handle, |_, window, cx| window.press("tab", cx)).unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("publish-topic").value(), Some("home/"));
            assert_eq!(window.find("publish-topic-proposal").label(), Some(expected));
            assert_eq!(window.find("publish-topic").focused(), Some(true));
            assert_eq!(view.read(cx).publish_topic.read(cx).selected_range(), 5..5);
        })
        .unwrap();
    }
    cx.update_window(handle, |_, window, cx| window.press("right", cx)).unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("publish-topic").value(), Some("home/b"));
        assert!(window.try_find("publish-topic-proposal").is_none());
        assert_eq!(window.find("publish-topic").focused(), Some(true));
        window.press("shift-tab", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("toggle-publish").focused(), Some(true));
    })
    .unwrap();
}

#[gpui_kit::test]
fn publish_topic_completion_accepts_unique_branches_and_cycles_only_siblings(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 1200., 760.);
    cx.update_window(handle, |_, window, cx| {
        window.activate_window();
        view.update(cx, |view, cx| {
            for topic in ["home/room/sensor/temperature", "home/room/sensor/humidity", "home/room/status"] {
                view.topics.receive(message(topic), std::time::Instant::now());
            }
            view.sync_tree(cx);
        });
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("toggle-publish", cx);
        window.click("publish-topic", cx);
        window.input("home/r", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("publish-topic-proposal").label(), Some("oom/"));
        window.press("tab", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("publish-topic").value(), Some("home/room/"));
        assert_eq!(window.find("publish-topic-proposal").label(), Some("sensor/"));
    })
    .unwrap();
    for expected in ["status", "sensor/"] {
        cx.update_window(handle, |_, window, cx| window.press("tab", cx)).unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("publish-topic").value(), Some("home/room/"));
            assert_eq!(window.find("publish-topic-proposal").label(), Some(expected));
        })
        .unwrap();
    }
    cx.update_window(handle, |_, window, cx| window.press("right", cx)).unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("publish-topic").value(), Some("home/room/sensor/"));
        assert_eq!(window.find("publish-topic-proposal").label(), Some("humidity"));
        assert_eq!(window.find("publish-topic").focused(), Some(true));
        window.press("secondary-a", cx);
        window.input("h", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("publish-topic-proposal").label(), Some("ome/"));
        window.press("tab", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("publish-topic").value(), Some("home/"));
        assert_eq!(window.find("publish-topic-proposal").label(), Some("a"));
    })
    .unwrap();
}

#[gpui_kit::test]
fn publish_topic_right_accepts_the_first_proposal_and_undo_restores_typed_text(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 1200., 760.);
    cx.update_window(handle, |_, window, _| window.activate_window()).unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("toggle-publish", cx);
        window.click("publish-topic", cx);
        window.input("home/", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("publish-topic").value(), Some("home/"));
        assert_eq!(window.find("publish-topic-proposal").label(), Some("a"));
        window.press("right", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("publish-topic").value(), Some("home/a"));
        assert_eq!(view.read(cx).publish_topic.read(cx).selected_range(), 6..6);
        assert!(window.try_find("publish-topic-proposal").is_none());
        window.press("secondary-z", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("publish-topic").value(), Some("home/"));
        assert_eq!(window.find("publish-topic-proposal").label(), Some("a"));
    })
    .unwrap();
}

#[gpui_kit::test]
fn publish_topic_proposals_follow_edits_focus_and_cursor_without_interfering_with_navigation(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 1200., 760.);
    cx.update_window(handle, |_, window, _| window.activate_window()).unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("toggle-publish", cx);
        window.click("publish-topic", cx);
        assert!(window.try_find("publish-topic-proposal").is_none());
        window.input("home/", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.press("tab", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("publish-topic-proposal").label(), Some("b"));
        window.press("left", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("publish-topic-proposal").is_none());
        window.press("right", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("publish-topic").value(), Some("home/"));
        assert_eq!(view.read(cx).publish_topic.read(cx).selected_range(), 5..5);
        assert_eq!(window.find("publish-topic-proposal").label(), Some("b"));
        window.press("backspace", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("publish-topic").value(), Some("home"));
        assert_eq!(window.find("publish-topic-proposal").label(), Some("/"));
        window.click("publish-payload", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("publish-topic-proposal").is_none());
        window.click("publish-topic", cx);
        window.press("secondary-a", cx);
        window.input("unknown", cx);
        window.press("left", cx);
        window.press("right", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("publish-topic").value(), Some("unknown"));
        assert_eq!(view.read(cx).publish_topic.read(cx).selected_range(), 7..7);
        assert!(window.try_find("publish-topic-proposal").is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
fn publish_topic_tab_uses_edited_prefix_and_preserves_normal_navigation(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 1200., 760.);
    cx.update_window(handle, |_, window, _| window.activate_window()).unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("toggle-publish", cx);
        window.click("publish-topic", cx);
        window.input("home/", cx);
        window.press("tab", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.press("secondary-a", cx);
        window.input("home/b", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| window.press("tab", cx)).unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("publish-topic").value(), Some("home/b"));
        assert!(view.read(cx).publish_payload.read(cx).focus_handle(cx).is_focused(window));
        window.click("publish-topic", cx);
        window.press("secondary-a", cx);
        window.input("unknown/", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| window.press("tab", cx)).unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("publish-topic").value(), Some("unknown/"));
        assert!(view.read(cx).publish_payload.read(cx).focus_handle(cx).is_focused(window));
    })
    .unwrap();
}

#[gpui_kit::test]
fn publish_topic_tab_preserves_unicode_empty_levels_and_undo(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 1200., 760.);
    cx.update_window(handle, |_, window, cx| {
        window.activate_window();
        view.update(cx, |view, cx| {
            view.topics.receive(message("家//測定値"), std::time::Instant::now());
            view.sync_tree(cx);
        });
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("toggle-publish", cx);
        window.click("publish-topic", cx);
        window.input("家//測", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| window.press("tab", cx)).unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("publish-topic").value(), Some("家//測定値"));
        assert_eq!(window.find("publish-topic").focused(), Some(true));
        window.press("secondary-z", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("publish-topic").value(), Some("家//測"));
    })
    .unwrap();
}

#[gpui_kit::test]
fn publish_disclosure_restores_saved_state_in_new_explorers(cx: &mut TestAppContext) {
    let (handle, _) = open(cx, false, 900., 640.);
    cx.update_window(handle, |_, window, _| window.activate_window()).unwrap();
    cx.run_until_parked();
    for expected_open in [true, false] {
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click("toggle-publish", cx);
            let restored = cx.new(|cx| Explorer::new(window, cx));
            assert_eq!(restored.read(cx).publish_open, expected_open);
            assert_eq!(crate::config::load_publish_layout().unwrap().open, expected_open);
        })
        .unwrap();
        cx.run_until_parked();
    }
}

#[gpui_kit::test]
fn publish_disclosure_preserves_draft_and_options_across_keyboard_and_pointer_toggles(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 900., 640.);
    cx.update_window(handle, |_, window, _| window.activate_window()).unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("publish-form").is_none());
        window.click("toggle-publish", cx);
        assert!(window.find("publish-form").visible());
        window.click("publish-topic", cx);
        window.input("home/command", cx);
        window.click("publish-payload", cx);
        window.input("{\"enabled\":true}", cx);
        window.click("publish-retain", cx);
        window.click("toggle-publish", cx);
        assert!(window.try_find("publish-form").is_none());
        window.click("toggle-publish", cx);
        window.click("publish-topic", cx);
        window.press("shift-tab", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("toggle-publish").focused(), Some(true));
        window.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("publish-form").is_none());
        window.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("publish-form").visible());
        assert_eq!(window.find("publish-topic").value(), Some("home/command"));
        assert_eq!(view.read(cx).publish_payload.read(cx).value().as_str(), "{\"enabled\":true}");
        assert!(view.read(cx).publish_retain);
        window.click("publish-qos", cx);
    })
    .unwrap();
    cx.run_until_parked();
    for key in ["home", "down"] {
        cx.update_window(handle, |_, window, cx| window.press(key, cx)).unwrap();
        cx.run_until_parked();
    }
    cx.update_window(handle, |_, window, cx| window.press("enter", cx)).unwrap();
    cx.run_until_parked();
    cx.update(|cx| assert_eq!(view.read(cx).publish_qos.read(cx).selected_value(), Some(&"1")));
}

fn assert_publish_payload_detection(
    window: &mut Window,
    cx: &mut App,
    view: &Entity<Explorer>,
    raw: &str,
    error: Option<&str>,
    cursor: usize,
) {
    window.render_frame(cx);
    let json = raw.starts_with('{');
    let view = view.read(cx);
    let editor = view.publish_payload.read(cx);
    assert_eq!(editor.value().as_str(), raw);
    assert_eq!(editor.selected_range(), cursor..cursor);
    assert!(editor.focus_handle(cx).is_focused(window));
    assert_eq!(editor.language_name().as_str(), if json { "json" } else { "plaintext" });
    let diagnostics = editor.diagnostics().expect("publish payload editor must expose diagnostics");
    match error {
        Some(message) => assert!(
            diagnostics.iter().any(|diagnostic| {
                diagnostic.severity == gpui_kit::base::input::DiagnosticSeverity::Error
                    && diagnostic.message.as_str() == message
                    && !diagnostic.range.is_empty()
                    && raw.is_char_boundary(diagnostic.range.start)
                    && raw.is_char_boundary(diagnostic.range.end)
            }),
            "expected an error diagnostic with message {message:?}"
        ),
        None => assert!(diagnostics.is_empty()),
    }
    let content_type = window.find("publish-content-type");
    assert!(content_type.visible());
    assert_eq!(content_type.label(), Some(if json { "JSON" } else { "Text" }));
    assert!(window.try_find("publish-validation").is_none());
    assert!(view.publish_feedback.is_none());
    assert!(!view.publish_pending);
}

#[gpui_kit::test]
fn publish_payload_native_input_detects_json_and_errors_then_returns_to_plaintext_without_losing_focus(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 1200., 760.);
    let plain = "online é";
    let json = "{ \"label\":\"é\",\"count\":1 }";
    let malformed = json.strip_suffix('}').unwrap();
    let error = serde_json::from_str::<serde_json::Value>(malformed).unwrap_err().to_string();
    cx.update_window(handle, |_, window, _| window.activate_window()).unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("toggle-publish", cx);
        window.click("publish-payload", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        assert_publish_payload_detection(window, cx, &view, "", None, 0);
        window.input(plain, cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        assert_publish_payload_detection(window, cx, &view, plain, None, plain.len());
        window.press("secondary-a", cx);
        window.input(json, cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        assert_publish_payload_detection(window, cx, &view, json, None, json.len());
        window.press("backspace", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        assert_publish_payload_detection(window, cx, &view, malformed, Some(&error), malformed.len());
        window.input("}", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        assert_publish_payload_detection(window, cx, &view, json, None, json.len());
        window.press("secondary-a", cx);
        window.press("backspace", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        assert_publish_payload_detection(window, cx, &view, "", None, 0);
        window.input(plain, cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        assert_publish_payload_detection(window, cx, &view, plain, None, plain.len());
    })
    .unwrap();
    cx.run_until_parked();
}

#[gpui_kit::test]
fn publish_payload_native_input_does_not_validate_content_without_a_literal_leading_brace(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 1200., 760.);
    cx.update_window(handle, |_, window, _| window.activate_window()).unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("toggle-publish", cx);
        window.click("publish-topic", cx);
        window.input("home/command", cx);
        window.click("publish-payload", cx);
        window.input("{\"a\":1}", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        assert_publish_payload_detection(window, cx, &view, "{\"a\":1}", None, 7);
    })
    .unwrap();
    cx.run_until_parked();
    for raw in [
        "42",
        "-1.25e+2",
        "true",
        "false",
        "null",
        "\"hello é\"",
        "[1,false,null]",
        " {\"a\":1}",
        "  {\"a\":1}  ",
        " {\"a\":",
        "[1,",
        "\"unterminated",
        "  [1,",
        "  \"unterminated",
        "nullish",
        "42 bottles",
        "   ",
        "",
    ] {
        cx.update_window(handle, |_, window, cx| {
            window.press("secondary-a", cx);
            window.press("backspace", cx);
            window.input(raw, cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            assert_publish_payload_detection(window, cx, &view, raw, None, raw.len());
            assert_eq!(window.find("publish-topic").value(), Some("home/command"));
        })
        .unwrap();
        cx.run_until_parked();
    }
}

#[gpui_kit::test]
fn publish_payload_keyboard_editing_undo_and_redo_preserve_detection_and_cursor(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 1200., 760.);
    cx.update_window(handle, |_, window, _| window.activate_window()).unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("toggle-publish", cx);
        window.click("publish-payload", cx);
        window.input("{}", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        assert_publish_payload_detection(window, cx, &view, "{}", None, 2);
        window.press("home", cx);
        window.input(" ", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        assert_publish_payload_detection(window, cx, &view, " {}", None, 1);
    })
    .unwrap();
    cx.run_until_parked();
    let redo = if cfg!(target_os = "macos") {
        "secondary-shift-z"
    } else {
        "secondary-y"
    };
    let error = serde_json::from_str::<serde_json::Value>("{").unwrap_err().to_string();
    for (key, raw, diagnostic_error, cursor) in [
        ("secondary-z", "{}", None, 0),
        (redo, " {}", None, 1),
        ("backspace", "{}", None, 0),
        ("end", "{}", None, 2),
        ("backspace", "{", Some(error.as_str()), 1),
        ("secondary-z", "{}", None, 2),
        (redo, "{", Some(error.as_str()), 1),
        ("secondary-z", "{}", None, 2),
    ] {
        cx.update_window(handle, |_, window, cx| window.press(key, cx)).unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            assert_publish_payload_detection(window, cx, &view, raw, diagnostic_error, cursor);
        })
        .unwrap();
        cx.run_until_parked();
    }
}

const PUBLISH_FILE_DRAFT: &str = "draft é\nkeep me";
const PUBLISH_FILE_SVG: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" width="2" height="2"><path d="M0 0h2v2H0z"/></svg>"#;

fn open_publish_file_draft(cx: &mut TestAppContext, width: f32, height: f32) -> (AnyWindowHandle, Entity<Explorer>) {
    let (handle, view) = open(cx, false, width, height);
    cx.update_window(handle, |_, window, cx| {
        window.activate_window();
        view.update(cx, |view, cx| {
            view.status = ConnectionStatus::Disconnected;
            cx.notify();
        });
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("toggle-publish", cx);
        window.click("publish-topic", cx);
        window.input("home/command", cx);
        window.click("publish-payload", cx);
        window.input(PUBLISH_FILE_DRAFT, cx);
        window.press("left", cx);
    })
    .unwrap();
    cx.run_until_parked();
    (handle, view)
}

fn respond_to_publish_file_prompt(cx: &mut TestAppContext, path: Option<&std::path::Path>) {
    assert!(cx.did_prompt_for_paths());
    cx.simulate_path_prompt_response(|options| {
        assert!(options.files);
        assert!(!options.directories);
        assert!(!options.multiple);
        path.map(|path| vec![path.to_path_buf()])
    });
    cx.run_until_parked();
    assert!(!cx.did_prompt_for_paths());
}

#[track_caller]
fn assert_publish_file(window: &mut Window, cx: &mut App, name: &str, bytes: usize, content_type: &str) {
    window.render_frame(cx);
    let file = window.within("publish-payload").find("publish-file");
    assert!(
        file.visible(),
        "{name}: file {:?}, payload {:?}, feedback {:?}",
        file.bounds(),
        window.find("publish-payload").bounds(),
        window.try_find("publish-feedback").map(|feedback| feedback.bounds())
    );
    assert_eq!(window.find("publish-file-name").label(), Some(name));
    assert_eq!(window.find("publish-file-size").label(), Some(format!("{bytes} bytes").as_str()));
    assert_eq!(window.find("publish-content-type").label(), Some(content_type));
    assert!(window.find("publish-file-preview").visible());
}

#[gpui_kit::test]
fn publish_file_cancel_and_read_errors_preserve_the_text_draft_and_previous_file(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    let binary = directory.path().join("payload.unknown");
    std::fs::write(&binary, [0, 0xff, 0x80, b'\n']).unwrap();
    let missing = directory.path().join("missing.bin");
    let oversized = directory.path().join("oversized.bin");
    std::fs::File::create(&oversized).unwrap().set_len(16 * 1024 * 1024 + 1).unwrap();
    let (handle, view) = open_publish_file_draft(cx, 1000., 760.);
    let draft = cx.update(|cx| view.read(cx).publish_payload.clone());

    for loaded in [false, true] {
        if loaded {
            cx.update_window(handle, |_, window, cx| window.click("load-publish-file", cx))
                .unwrap();
            respond_to_publish_file_prompt(cx, Some(&binary));
        }
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click("load-publish-file", cx);
            assert!(view.read(cx).publish_file_task.is_some());
            window.click("load-publish-file", cx);
            if loaded {
                window.click("remove-publish-file", cx);
                assert!(view.read(cx).publish_file.is_some());
            }
            window.click("publish-message", cx);
            window.click("publish-topic", cx);
            window.press("ctrl-enter", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update(|cx| {
            assert!(!view.read(cx).publish_pending);
            assert!(
                view.read(cx).publish_feedback.is_none(),
                "Ctrl+Enter must be suppressed during file selection"
            );
        });
        respond_to_publish_file_prompt(cx, None);

        for (path, error) in [
            (&missing, "Could not read"),
            (&oversized, "16 MiB MQTT packet limit"),
            (&directory.path().to_path_buf(), "regular data file"),
        ] {
            cx.update_window(handle, |_, window, cx| window.click("load-publish-file", cx))
                .unwrap();
            respond_to_publish_file_prompt(cx, Some(path));
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                assert!(window.find("publish-error").visible());
                assert!(window.try_find("publish-feedback").is_none());
                let state = view.read(cx);
                let feedback = state.publish_feedback.as_ref().unwrap().as_ref().unwrap_err();
                assert!(feedback.starts_with("Could not load payload file:"), "{feedback}");
                assert!(feedback.contains(error), "{feedback}");
                assert!(state.publish_file_task.is_none());
                assert_eq!(state.publish_file.is_some(), loaded);
                assert_eq!(state.publish_payload.entity_id(), draft.entity_id());
                assert_eq!(draft.read(cx).value().as_str(), PUBLISH_FILE_DRAFT);
                assert_eq!(
                    draft.read(cx).selected_range(),
                    PUBLISH_FILE_DRAFT.len() - 1..PUBLISH_FILE_DRAFT.len() - 1
                );
                if loaded {
                    assert_publish_file(window, cx, "payload.unknown", 4, "application/octet-stream");
                    assert!(window.try_find(("input", draft.entity_id())).is_none());
                } else {
                    assert!(window.try_find("publish-file").is_none());
                    assert!(window.find(("input", draft.entity_id())).visible());
                }
            })
            .unwrap();
        }
        // Canceling a replacement must retain the error and existing attachment too.
        let feedback = cx.update(|cx| view.read(cx).publish_feedback.clone());
        cx.update_window(handle, |_, window, cx| window.click("load-publish-file", cx))
            .unwrap();
        respond_to_publish_file_prompt(cx, None);
        cx.update(|cx| {
            assert_eq!(view.read(cx).publish_feedback, feedback);
            assert_eq!(view.read(cx).publish_file.is_some(), loaded);
            assert_eq!(draft.read(cx).value().as_str(), PUBLISH_FILE_DRAFT);
        });
    }
}

#[gpui_kit::test]
fn publish_file_replace_collapse_and_remove_restore_the_editor_across_themes_and_zoom(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    let binary = directory.path().join("binary.data");
    std::fs::write(&binary, [0, 0xff, 0x80]).unwrap();
    let image = directory.path().join("preview-é.SVG");
    std::fs::write(&image, PUBLISH_FILE_SVG).unwrap();
    for mode in [ThemeMode::Light, ThemeMode::Dark] {
        for font_size in [16., 20.] {
            crate::config::save_publish_open(false).unwrap();
            let (handle, view) = open_publish_file_draft(cx, 760., 540.);
            cx.update(|cx| {
                Theme::change(mode, None, cx);
                Theme::update(cx, |theme| theme.font_size = px(font_size));
            });
            let draft = cx.update(|cx| view.read(cx).publish_payload.clone());
            for (path, name, bytes, content_type) in [
                (&binary, "binary.data", 3, "application/octet-stream"),
                (&image, "preview-é.SVG", PUBLISH_FILE_SVG.len(), "image/svg+xml"),
            ] {
                cx.update_window(handle, |_, window, cx| {
                    window.render_frame(cx);
                    window.click("load-publish-file", cx);
                })
                .unwrap();
                respond_to_publish_file_prompt(cx, Some(path));
                cx.update_window(handle, |_, window, cx| {
                    assert_publish_file(window, cx, name, bytes, content_type);
                    assert!(
                        window.try_find(("input", draft.entity_id())).is_none(),
                        "the Editor must be replaced, not merely covered"
                    );
                    window.click("publish-message", cx);
                    assert!(
                        view.read(cx).publish_feedback.is_none(),
                        "the offline Publish button must ignore clicks"
                    );
                    assert!(!view.read(cx).publish_pending);
                    let state = view.read(cx);
                    assert!(state.publish_file.is_some());
                    assert!(state.publish_file_task.is_none());
                    assert_eq!(state.publish_payload.entity_id(), draft.entity_id());
                    assert_eq!(draft.read(cx).value().as_str(), PUBLISH_FILE_DRAFT);
                    assert_eq!(
                        draft.read(cx).selected_range(),
                        PUBLISH_FILE_DRAFT.len() - 1..PUBLISH_FILE_DRAFT.len() - 1
                    );
                    assert_eq!(window.find("publish-topic").focused(), Some(true));
                    assert_publish_resize_layout(window, cx);
                    let payload = window.find("publish-payload").bounds();
                    let panel = window.find("publish-panel").bounds();
                    for id in [
                        "publish-file-name",
                        "publish-file-size",
                        "publish-file-preview",
                        "remove-publish-file",
                    ] {
                        let element = window.find(id);
                        let bounds = element.bounds();
                        assert!(element.visible(), "{id}, {mode:?}, font {font_size}");
                        assert!(
                            bounds.left() >= payload.left() && bounds.right() <= payload.right(),
                            "{id}: {bounds:?}, payload {payload:?}"
                        );
                        assert!(
                            bounds.top() >= payload.top() && bounds.bottom() <= payload.bottom(),
                            "{id}: {bounds:?}, payload {payload:?}"
                        );
                    }
                    let load = window.find("load-publish-file").bounds();
                    let publish = window.find("publish-message").bounds();
                    assert!(load.left() >= panel.left() && load.bottom() <= panel.bottom());
                    assert!(load.right() <= publish.left(), "file and publish actions must not overlap");
                    assert_eq!(load.top(), publish.top());
                    let retain = window.find("publish-retain").bounds();
                    let qos = window.find("publish-qos").bounds();
                    assert!(qos.right() < retain.left() && retain.right() < load.left());
                    for bounds in [retain, qos] {
                        assert!(
                            (bounds.center().y - publish.center().y).abs() <= px(1.),
                            "controls must share one row"
                        );
                    }
                })
                .unwrap();
            }
            cx.update_window(handle, |_, window, cx| {
                window.click("toggle-publish", cx);
                assert!(window.try_find("publish-file").is_none());
                assert!(view.read(cx).publish_file.is_some());
                window.click("toggle-publish", cx);
                assert_publish_file(window, cx, "preview-é.SVG", PUBLISH_FILE_SVG.len(), "image/svg+xml");
                window.click("remove-publish-file", cx);
            })
            .unwrap();
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                assert!(view.read(cx).publish_file.is_none());
                assert!(window.try_find("publish-file").is_none());
                assert!(window.find(("input", draft.entity_id())).visible());
                assert_eq!(window.find(("input", draft.entity_id())).focused(), Some(true));
                assert_eq!(window.find("publish-content-type").label(), Some("Text"));
                assert_eq!(draft.read(cx).value().as_str(), PUBLISH_FILE_DRAFT);
                window.input("!", cx);
                assert_eq!(draft.read(cx).value().as_str(), "draft é\nkeep m!e");
                window.press("secondary-z", cx);
            })
            .unwrap();
            cx.run_until_parked();
            cx.update(|cx| assert_eq!(draft.read(cx).value().as_str(), PUBLISH_FILE_DRAFT));
        }
    }
}

#[gpui_kit::test]
fn publish_file_images_fit_without_upscaling_or_overlapping_controls(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("large.svg");
    for mode in [ThemeMode::Light, ThemeMode::Dark] {
        crate::config::save_publish_open(false).unwrap();
        let (handle, _) = open_publish_file_draft(cx, 760., 540.);
        cx.update(|cx| {
            Theme::change(mode, None, cx);
            Theme::update(cx, |theme| theme.font_size = px(20.));
        });
        for (width, height) in [(1024, 1024), (1536, 512), (512, 1536), (1, 1), (16, 8)] {
            let svg = format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}"><rect width="{width}" height="{height}"/></svg>"#,
            );
            std::fs::write(&path, &svg).unwrap();
            let image = std::sync::Arc::new(gpui_kit::Image::from_bytes(gpui_kit::ImageFormat::Svg, svg.into_bytes()));
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.click("load-publish-file", cx);
            })
            .unwrap();
            respond_to_publish_file_prompt(cx, Some(&path));
            // Start the real image decoder before checking layout after it finishes.
            cx.update_window(handle, |_, window, cx| window.render_frame(cx)).unwrap();
            cx.run_until_parked();
            for resize in [None, Some("ctrl-alt-up"), Some("ctrl-alt-down")] {
                cx.update_window(handle, |_, window, cx| {
                    if let Some(key) = resize {
                        window.press(key, cx);
                    }
                    window.render_frame(cx);
                    let preview = window.find("publish-file-preview").bounds();
                    let element = window.find(("publish-file-image", image.id()));
                    let bounds = element.bounds();
                    assert!(element.visible());
                    assert!(preview.size.width > px(0.) && preview.size.height > px(0.));
                    assert!(
                        bounds.left() >= preview.left() && bounds.right() <= preview.right(),
                        "image {bounds:?}, preview {preview:?}"
                    );
                    assert!(
                        bounds.top() >= preview.top() && bounds.bottom() <= preview.bottom(),
                        "image {bounds:?}, preview {preview:?}"
                    );
                    let decoded = image.clone().get_render_image(window, cx).expect("SVG must decode successfully");
                    let original = decoded.size(0).map(|dimension| px(u32::from(dimension) as f32));
                    let fitted = gpui_kit::ObjectFit::ScaleDown.get_bounds(bounds, decoded.size(0));
                    let scale = (preview.size.width / original.width)
                        .min(preview.size.height / original.height)
                        .min(1.);
                    assert!((fitted.size.width - original.width * scale).abs() <= px(0.01));
                    assert!((fitted.size.height - original.height * scale).abs() <= px(0.01));
                    assert!(fitted.size.width <= original.width && fitted.size.height <= original.height);
                    assert!(fitted.left() >= preview.left() && fitted.right() <= preview.right());
                    assert!(fitted.top() >= preview.top() && fitted.bottom() <= preview.bottom());
                    assert!((fitted.size.width / fitted.size.height - width as f32 / height as f32).abs() < 0.001);
                    for id in ["publish-qos", "publish-retain", "load-publish-file", "publish-message"] {
                        let control = window.find(id);
                        assert!(control.visible(), "{id}");
                        assert!(fitted.bottom() <= control.bounds().top(), "image must not cover {id}");
                    }
                })
                .unwrap();
                cx.run_until_parked();
            }
        }
    }
}

#[gpui_kit::test]
fn publish_file_edit_value_cancels_the_load_and_replaces_the_attachment_with_topic_text(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    let image = directory.path().join("preview.svg");
    std::fs::write(&image, PUBLISH_FILE_SVG).unwrap();
    let (handle, view) = open_publish_file_draft(cx, 1000., 760.);
    cx.update_window(handle, |_, window, cx| window.click("load-publish-file", cx))
        .unwrap();
    respond_to_publish_file_prompt(cx, Some(&image));
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            receive_payload(view, "home/a", "topic value", window, cx);
            view.select_topic("home/a", window, cx);
        });
        window.render_frame(cx);
        window.click("load-publish-file", cx);
        assert!(view.read(cx).publish_file_task.is_some());
    })
    .unwrap();
    assert!(cx.did_prompt_for_paths());
    cx.simulate_path_prompt_response(|_| Some(vec![image]));
    // The picker has replied, but its async task has not run; Edit value must win.
    cx.update_window(handle, |_, window, cx| window.click("edit-value", cx)).unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let state = view.read(cx);
        assert!(state.publish_file_task.is_none());
        assert!(state.publish_file.is_none());
        assert!(window.try_find("publish-file").is_none());
        assert_eq!(window.find("publish-topic").value(), Some("home/a"));
        assert_publish_payload_detection(window, cx, &view, "topic value", None, "topic value".len());
    })
    .unwrap();
}

#[gpui_kit::test]
fn publish_is_disabled_offline_and_shortcut_reports_error_without_losing_draft(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 900., 640.);
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("toggle-publish", cx);
        window.click("publish-topic", cx);
        window.input("home/command", cx);

        window.click("publish-message", cx);
        assert!(view.read(cx).publish_feedback.is_none());
        window.press("ctrl-enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("publish-error").visible());
        assert!(window.try_find("publish-feedback").is_none());
        assert_eq!(
            view.read(cx).publish_feedback.as_ref().unwrap().as_deref().map_err(String::as_str),
            Err("Connect to a broker before publishing.")
        );
        assert_eq!(window.find("publish-topic").value(), Some("home/command"));
        assert!(!view.read(cx).publish_pending);
    })
    .unwrap();
}

fn drag_publish_divider(window: &mut Window, delta: gpui_kit::Pixels, cx: &mut App) {
    window.render_frame(cx);
    let panel = window.find("publish-panel").bounds();
    let from = gpui_kit::point(panel.center().x, panel.top());
    window.drag(from, from + gpui_kit::point(px(0.), delta), cx);
}

fn assert_publish_resize_layout(window: &mut Window, cx: &mut App) {
    window.render_frame(cx);
    let pane = window.find("topics-pane").bounds();
    let panel = window.within("topics-pane").within("publish-panes").find("publish-panel");
    assert!(panel.visible());
    let panel = panel.bounds();
    let heading = window.within("topics-pane").within("publish-panes").find("topics-heading");
    assert!(heading.visible());
    assert!(heading.bounds().top() >= pane.top());
    assert!(heading.bounds().bottom() < panel.top());
    let row = window.find("topic:home");
    assert!(row.visible(), "the topic tree must remain visible above the publish panel");
    assert!(row.bounds().top() >= heading.bounds().bottom());
    assert!(row.bounds().bottom() <= panel.top());
    assert!(panel.left() >= pane.left());
    assert!(panel.right() <= pane.right(), "panel {panel:?}, pane {pane:?}");
    assert!(
        panel.bottom() <= pane.bottom(),
        "panel {panel:?}, pane {pane:?}, rem {:?}",
        window.rem_size()
    );
    for id in [
        "toggle-publish",
        "publish-topic",
        "publish-payload",
        "publish-qos",
        "publish-retain",
        "publish-message",
    ] {
        let control = window.find(id);
        let bounds = control.bounds();
        assert!(control.visible(), "{id} is not visible");
        assert!(bounds.left() >= panel.left(), "{id}: {bounds:?}, panel: {panel:?}");
        assert!(bounds.right() <= panel.right(), "{id}: {bounds:?}, panel: {panel:?}");
        assert!(bounds.top() >= panel.top(), "{id}: {bounds:?}, panel: {panel:?}");
        assert!(bounds.bottom() <= panel.bottom(), "{id}: {bounds:?}, panel: {panel:?}");
    }
    assert!(window.find("publish-payload").bounds().size.height > px(0.));
}

#[gpui_kit::test]
fn publish_divider_drag_grows_and_shrinks_the_editor_and_reopening_preserves_size_and_draft(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 1200., 850.);
    cx.update_window(handle, |_, window, _| window.activate_window()).unwrap();
    cx.run_until_parked();
    let publish_panes = cx.update(|cx| view.read(cx).publish_panes.clone());
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("publish-form").is_none());
        window.click("toggle-publish", cx);
    })
    .unwrap();
    cx.run_until_parked();
    settle_publish_resize(handle, cx);
    let (initial_height, initial_payload_height, horizontal_sizes) = cx
        .update_window(handle, |_, window, cx| {
            assert_publish_resize_layout(window, cx);
            let panel = window.find("publish-panel").bounds();
            assert!((panel.size.height - window.rem_size() * 19.).abs() <= px(1.));
            assert!(panel.top() - window.find("topics-pane").bounds().top() >= window.rem_size() * 4.5);
            let initial = (
                panel.size.height,
                window.find("publish-payload").bounds().size.height,
                view.read(cx).panes.read(cx).sizes().clone(),
            );
            window.click("publish-topic", cx);
            window.input("home/command/é", cx);
            window.click("publish-payload", cx);
            window.input("{\"enabled\":true}", cx);
            window.click("publish-retain", cx);
            window.click("publish-qos", cx);
            initial
        })
        .unwrap();
    cx.run_until_parked();
    for key in ["home", "down", "enter"] {
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.press(key, cx);
        })
        .unwrap();
        cx.run_until_parked();
    }
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(view.read(cx).publish_qos.read(cx).selected_value(), Some(&"1"));
        drag_publish_divider(window, px(-160.), cx);
    })
    .unwrap();
    cx.run_until_parked();
    let (grown_height, grown_payload_height) = cx
        .update_window(handle, |_, window, cx| {
            assert_publish_resize_layout(window, cx);
            let height = window.find("publish-panel").bounds().size.height;
            let payload_height = window.find("publish-payload").bounds().size.height;
            assert!(height > initial_height + px(80.), "dragging up must enlarge the bottom panel");
            assert!(
                payload_height > initial_payload_height + px(80.),
                "the editor must use the added height"
            );
            assert_eq!(view.read(cx).panes.read(cx).sizes(), &horizontal_sizes);
            drag_publish_divider(window, px(64.), cx);
            (height, payload_height)
        })
        .unwrap();
    cx.run_until_parked();
    let (saved_height, saved_payload_height, saved_layout) = cx
        .update_window(handle, |_, window, cx| {
            assert_publish_resize_layout(window, cx);
            let height = window.find("publish-panel").bounds().size.height;
            let payload_height = window.find("publish-payload").bounds().size.height;
            assert!(height < grown_height && height > initial_height);
            assert!(payload_height < grown_payload_height && payload_height > initial_payload_height);
            let saved_layout = (view.read(cx).publish_height_rem, view.read(cx).publish_height_fraction);
            assert!(saved_layout.0.is_some() && saved_layout.1.is_some());
            assert_eq!(publish_panes.read(cx).sizes().len(), 2);
            assert_eq!(view.read(cx).publish_panes.entity_id(), publish_panes.entity_id());
            window.click("toggle-publish", cx);
            (height, payload_height, saved_layout)
        })
        .unwrap();
    cx.run_until_parked();
    settle_publish_resize(handle, cx);
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("publish-form").is_none());
        for id in ["topics-heading", "publish-panel"] {
            let element = window.within("topics-pane").within("publish-panes").find(id);
            assert!(element.visible());
        }
        assert!((window.find("publish-panel").bounds().size.height - window.rem_size() * 2.5).abs() <= px(1.));
        assert_eq!(view.read(cx).publish_panes.entity_id(), publish_panes.entity_id());
        assert_eq!(view.read(cx).publish_height_rem, saved_layout.0);
        assert_eq!(view.read(cx).publish_height_fraction, saved_layout.1);
        window.click("toggle-publish", cx);
    })
    .unwrap();
    cx.run_until_parked();
    settle_publish_resize(handle, cx);
    cx.update_window(handle, |_, window, cx| {
        assert_publish_resize_layout(window, cx);
        assert!((window.find("publish-panel").bounds().size.height - saved_height).abs() <= px(1.));
        assert!((window.find("publish-payload").bounds().size.height - saved_payload_height).abs() <= px(1.));
        assert_eq!(view.read(cx).publish_panes.entity_id(), publish_panes.entity_id());
        assert_eq!(view.read(cx).panes.read(cx).sizes(), &horizontal_sizes);
        assert_eq!(window.find("publish-topic").value(), Some("home/command/é"));
        assert_eq!(view.read(cx).publish_payload.read(cx).value().as_str(), "{\"enabled\":true}");
        assert_eq!(view.read(cx).publish_qos.read(cx).selected_value(), Some(&"1"));
        assert!(view.read(cx).publish_retain);
        assert_eq!(window.find("publish-retain").checked(), Some(true));
    })
    .unwrap();
}

#[gpui_kit::test]
fn publish_divider_drag_limits_keep_topics_and_controls_on_screen_at_minimum_size_across_themes_and_zoom(cx: &mut TestAppContext) {
    for mode in [ThemeMode::Light, ThemeMode::Dark] {
        for font_size in [16., 20.] {
            let (handle, _) = open(cx, false, 760., 540.);
            cx.update(|cx| {
                Theme::change(mode, None, cx);
                Theme::update(cx, |theme| theme.font_size = px(font_size));
            });
            cx.update_window(handle, |_, window, cx| {
                window.activate_window();
                window.render_frame(cx);
                window.click("toggle-publish", cx);
            })
            .unwrap();
            cx.run_until_parked();
            for delta in [-1000., 1000., 1000.] {
                cx.update_window(handle, |_, window, cx| {
                    assert_publish_resize_layout(window, cx);
                    drag_publish_divider(window, px(delta), cx);
                })
                .unwrap();
                cx.run_until_parked();
                cx.update_window(handle, |_, window, cx| {
                    assert_publish_resize_layout(window, cx);
                    if delta > 0. {
                        let height = window.find("publish-panel").bounds().size.height;
                        assert!(
                            (height - window.rem_size() * 19.).abs() <= px(1.),
                            "downward drag must clamp at the minimum: height {height:?}, font {font_size}, theme {mode:?}"
                        );
                    }
                })
                .unwrap();
                cx.run_until_parked();
            }
        }
    }
}

#[gpui_kit::test]
fn publish_keyboard_resize_changes_height_by_two_rem_in_topic_and_payload_focus_and_clamps_at_minimum(cx: &mut TestAppContext) {
    for font_size in [16., 20.] {
        let (handle, view) = open(cx, false, 1200., 850.);
        cx.update(|cx| Theme::update(cx, |theme| theme.font_size = px(font_size)));
        cx.update_window(handle, |_, window, cx| {
            window.activate_window();
            window.render_frame(cx);
            window.click("toggle-publish", cx);
        })
        .unwrap();
        cx.run_until_parked();
        for id in ["publish-topic", "publish-payload"] {
            let (height, payload_height) = cx
                .update_window(handle, |_, window, cx| {
                    assert_publish_resize_layout(window, cx);
                    window.click(id, cx);
                    let before = (
                        window.find("publish-panel").bounds().size.height,
                        window.find("publish-payload").bounds().size.height,
                    );
                    window.press("ctrl-alt-up", cx);
                    before
                })
                .unwrap();
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| {
                assert_publish_resize_layout(window, cx);
                let step = window.rem_size() * 2.;
                assert!(
                    (window.find("publish-panel").bounds().size.height - height - step).abs() <= px(1.),
                    "height {:?}, before {height:?}, step {step:?}, sizes {:?}",
                    window.find("publish-panel").bounds().size.height,
                    view.read(cx).publish_panes.read(cx).sizes()
                );
                assert!((window.find("publish-payload").bounds().size.height - payload_height - step).abs() <= px(1.));
                if id == "publish-topic" {
                    assert_eq!(window.find(id).focused(), Some(true));
                } else {
                    assert!(view.read(cx).publish_payload.read(cx).focus_handle(cx).is_focused(window));
                }
                window.press("ctrl-alt-down", cx);
            })
            .unwrap();
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| {
                assert_publish_resize_layout(window, cx);
                assert!((window.find("publish-panel").bounds().size.height - height).abs() <= px(1.));
                assert!((window.find("publish-payload").bounds().size.height - payload_height).abs() <= px(1.));
                window.press("ctrl-alt-down", cx);
            })
            .unwrap();
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| {
                assert_publish_resize_layout(window, cx);
                assert!((window.find("publish-panel").bounds().size.height - window.rem_size() * 19.).abs() <= px(1.));
            })
            .unwrap();
            cx.run_until_parked();
        }
        let sizes = cx.update(|cx| view.read(cx).publish_panes.read(cx).sizes().clone());
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click("topic-search", cx);
            window.press("ctrl-alt-up", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            assert_publish_resize_layout(window, cx);
            assert_eq!(view.read(cx).publish_panes.read(cx).sizes(), &sizes);
            assert_eq!(window.find("topic-search").focused(), Some(true));
        })
        .unwrap();
    }
}

#[gpui_kit::test]
fn expanded_publish_panel_fits_after_the_same_window_shrinks_across_themes_and_zoom(cx: &mut TestAppContext) {
    for mode in [ThemeMode::Light, ThemeMode::Dark] {
        for font_size in [16., 20.] {
            let (handle, view) = open(cx, false, 1200., 850.);
            cx.update(|cx| {
                Theme::change(mode, None, cx);
                Theme::update(cx, |theme| theme.font_size = px(font_size));
            });
            cx.update_window(handle, |_, window, cx| {
                window.activate_window();
                window.render_frame(cx);
                window.click("toggle-publish", cx);
            })
            .unwrap();
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| {
                assert_publish_resize_layout(window, cx);
                drag_publish_divider(window, px(-1000.), cx);
            })
            .unwrap();
            cx.run_until_parked();
            let (expanded_height, state_id) = cx
                .update_window(handle, |_, window, cx| {
                    assert_publish_resize_layout(window, cx);
                    let pane = window.find("topics-pane").bounds();
                    let panel = window.find("publish-panel").bounds();
                    assert!((panel.top() - pane.top() - window.rem_size() * 4.5).abs() <= px(1.));
                    assert!(panel.size.height > window.rem_size() * 19.);
                    (panel.size.height, view.read(cx).publish_panes.entity_id())
                })
                .unwrap();
            cx.simulate_window_resize(handle, size(px(760.), px(540.)));
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| window.render_frame(cx)).unwrap();
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| {
                assert_publish_resize_layout(window, cx);
                assert!(window.find("publish-panel").bounds().size.height < expanded_height);
                assert_eq!(view.read(cx).publish_panes.entity_id(), state_id);
                drag_publish_divider(window, px(1000.), cx);
            })
            .unwrap();
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| {
                assert_publish_resize_layout(window, cx);
                assert!((window.find("publish-panel").bounds().size.height - window.rem_size() * 19.).abs() <= px(1.));
            })
            .unwrap();
            cx.run_until_parked();
        }
    }
}

fn settle_publish_resize(handle: AnyWindowHandle, cx: &mut TestAppContext) {
    for _ in 0..3 {
        cx.update_window(handle, |_, window, cx| window.render_frame(cx)).unwrap();
        cx.run_until_parked();
    }
}

fn assert_cached_publish_height(window: &mut Window, cx: &mut App, view: &Entity<Explorer>) -> (f32, f32) {
    assert_publish_resize_layout(window, cx);
    let view = view.read(cx);
    let state = view.publish_panes.read(cx);
    let height = state.sizes()[1];
    let height_rem = view.publish_height_rem.expect("resizing must cache the expanded height in rem");
    let height_fraction = view
        .publish_height_fraction
        .expect("resizing must cache the expanded height fraction");
    assert!((height - window.find("publish-panel").bounds().size.height).abs() <= px(1.));
    assert!((window.rem_size() * height_rem - height).abs() <= px(1.));
    assert!((height_fraction - height / state.container_size()).abs() < 0.001);
    (height_rem, height_fraction)
}

#[gpui_kit::test]
fn saved_publish_height_restores_fraction_in_preference_to_rem_when_first_expanded(cx: &mut TestAppContext) {
    for font_size in [16., 20.] {
        let (handle, view) = open(cx, false, 1200., 850.);
        cx.update(|cx| Theme::update(cx, |theme| theme.font_size = px(font_size)));
        view.update(cx, |view, cx| {
            view.publish_height_rem = Some(19.);
            view.publish_height_fraction = Some(0.65);
            view.restore_publish_height = true;
            cx.notify();
        });
        settle_publish_resize(handle, cx);
        cx.update_window(handle, |_, window, cx| {
            window.activate_window();
            window.render_frame(cx);
            assert!(window.try_find("publish-form").is_none());
            assert!(
                view.read(cx).restore_publish_height,
                "restoration must wait until the disclosure opens"
            );
            assert_eq!(view.read(cx).publish_height_rem, Some(19.));
            assert_eq!(view.read(cx).publish_height_fraction, Some(0.65));
            window.click("toggle-publish", cx);
        })
        .unwrap();
        cx.run_until_parked();
        settle_publish_resize(handle, cx);
        cx.update_window(handle, |_, window, cx| {
            assert_publish_resize_layout(window, cx);
            let height = window.find("publish-panel").bounds().size.height;
            let expected = view.read(cx).publish_panes.read(cx).container_size() * 0.65;
            assert!(
                (height - expected).abs() <= px(1.),
                "saved fraction must determine the initial height: height {height:?}, expected {expected:?}, sizes {:?}",
                view.read(cx).publish_panes.read(cx).sizes()
            );
            assert!(height > window.rem_size() * 19. + window.rem_size() * 2.);
            assert!(!view.read(cx).restore_publish_height);
        })
        .unwrap();
        cx.run_until_parked();
    }
}

#[gpui_kit::test]
fn saved_publish_height_restores_rem_when_no_fraction_is_cached(cx: &mut TestAppContext) {
    for font_size in [16., 20.] {
        let (handle, view) = open(cx, false, 1200., 850.);
        cx.update(|cx| Theme::update(cx, |theme| theme.font_size = px(font_size)));
        view.update(cx, |view, cx| {
            view.publish_height_rem = Some(26.);
            view.publish_height_fraction = None;
            view.restore_publish_height = true;
            cx.notify();
        });
        settle_publish_resize(handle, cx);
        cx.update_window(handle, |_, window, cx| {
            window.activate_window();
            window.render_frame(cx);
            assert!(view.read(cx).restore_publish_height);
            assert_eq!(view.read(cx).publish_height_fraction, None);
            window.click("toggle-publish", cx);
        })
        .unwrap();
        cx.run_until_parked();
        settle_publish_resize(handle, cx);
        cx.update_window(handle, |_, window, cx| {
            assert_publish_resize_layout(window, cx);
            let height = window.find("publish-panel").bounds().size.height;
            assert!(
                (height - window.rem_size() * 26.).abs() <= px(1.),
                "legacy rem-only height must use the current zoom: height {height:?}, rem {:?}, sizes {:?}",
                window.rem_size(),
                view.read(cx).publish_panes.read(cx).sizes()
            );
            assert!(!view.read(cx).restore_publish_height);
        })
        .unwrap();
        cx.run_until_parked();
    }
}

#[gpui_kit::test]
fn publish_height_cache_tracks_native_resizing_and_survives_collapse_and_a_simulated_restart(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 1200., 850.);
    let publish_panes_id = cx.update(|cx| view.read(cx).publish_panes.entity_id());
    cx.update(|cx| Theme::update(cx, |theme| theme.font_size = px(16.)));
    cx.update_window(handle, |_, window, cx| {
        window.activate_window();
        window.render_frame(cx);
        window.click("toggle-publish", cx);
    })
    .unwrap();
    cx.run_until_parked();
    settle_publish_resize(handle, cx);
    for delta in [-80., 32.] {
        let previous_height = cx
            .update_window(handle, |_, window, cx| {
                assert_publish_resize_layout(window, cx);
                let height = window.find("publish-panel").bounds().size.height;
                drag_publish_divider(window, px(delta), cx);
                height
            })
            .unwrap();
        cx.run_until_parked();
        settle_publish_resize(handle, cx);
        cx.update_window(handle, |_, window, cx| {
            let (height_rem, _) = assert_cached_publish_height(window, cx, &view);
            assert!((window.rem_size() * height_rem - previous_height + px(delta)).abs() <= px(1.));
        })
        .unwrap();
        cx.run_until_parked();
    }
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("publish-payload", cx);
    })
    .unwrap();
    cx.run_until_parked();
    for (key, delta_rem) in [("ctrl-alt-up", 2.), ("ctrl-alt-down", -2.)] {
        let previous = cx
            .update_window(handle, |_, window, cx| {
                let cached = assert_cached_publish_height(window, cx, &view);
                window.press(key, cx);
                cached
            })
            .unwrap();
        cx.run_until_parked();
        settle_publish_resize(handle, cx);
        cx.update_window(handle, |_, window, cx| {
            let cached = assert_cached_publish_height(window, cx, &view);
            assert!((window.rem_size() * (cached.0 - previous.0 - delta_rem)).abs() <= px(1.));
            assert!(
                (cached.1 - previous.1) * delta_rem > 0.,
                "keyboard resizing must update the saved fraction too"
            );
            assert!(view.read(cx).publish_payload.read(cx).focus_handle(cx).is_focused(window));
        })
        .unwrap();
        cx.run_until_parked();
    }
    let saved = cx
        .update_window(handle, |_, window, cx| {
            let cached = assert_cached_publish_height(window, cx, &view);
            window.click("toggle-publish", cx);
            cached
        })
        .unwrap();
    cx.run_until_parked();
    settle_publish_resize(handle, cx);
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("publish-form").is_none());
        let panel = window.within("topics-pane").within("publish-panes").find("publish-panel");
        assert!(panel.visible());
        assert!((panel.bounds().size.height - window.rem_size() * 2.5).abs() <= px(1.));
        assert_eq!(view.read(cx).publish_panes.entity_id(), publish_panes_id);
        assert_eq!(
            view.read(cx).publish_height_rem,
            Some(saved.0),
            "collapse must not cache the disclosure's height"
        );
        assert_eq!(view.read(cx).publish_height_fraction, Some(saved.1));
    })
    .unwrap();
    cx.run_until_parked();

    // Seed a fresh view like Explorer::new, without accessing the state file or environment.
    let (restarted_handle, restarted_view) = open(cx, false, 1200., 1000.);
    restarted_view.update(cx, |view, cx| {
        view.publish_height_rem = Some(saved.0);
        view.publish_height_fraction = Some(saved.1);
        view.restore_publish_height = true;
        cx.notify();
    });
    settle_publish_resize(restarted_handle, cx);
    cx.update_window(restarted_handle, |_, window, cx| {
        window.activate_window();
        window.render_frame(cx);
        assert!(window.try_find("publish-form").is_none());
        assert!(restarted_view.read(cx).restore_publish_height);
        window.click("toggle-publish", cx);
    })
    .unwrap();
    cx.run_until_parked();
    settle_publish_resize(restarted_handle, cx);
    cx.update_window(restarted_handle, |_, window, cx| {
        assert_publish_resize_layout(window, cx);
        let height = window.find("publish-panel").bounds().size.height;
        let expected = restarted_view.read(cx).publish_panes.read(cx).container_size() * saved.1;
        assert!((height - expected).abs() <= px(1.));
        assert!(
            height > window.rem_size() * saved.0 + window.rem_size(),
            "restart must preserve the split proportion"
        );
        assert!(!restarted_view.read(cx).restore_publish_height);
    })
    .unwrap();
}

#[gpui_kit::test]
fn oversized_saved_publish_fraction_clamps_on_initial_restoration_at_minimum_window_size(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 760., 540.);
    cx.update(|cx| Theme::update(cx, |theme| theme.font_size = px(16.)));
    view.update(cx, |view, cx| {
        view.publish_height_rem = Some(80.);
        view.publish_height_fraction = Some(0.99);
        view.restore_publish_height = true;
        cx.notify();
    });
    settle_publish_resize(handle, cx);
    cx.update_window(handle, |_, window, cx| {
        window.activate_window();
        window.render_frame(cx);
        assert!(view.read(cx).restore_publish_height);
        window.click("toggle-publish", cx);
    })
    .unwrap();
    cx.run_until_parked();
    settle_publish_resize(handle, cx);
    cx.update_window(handle, |_, window, cx| {
        assert_publish_resize_layout(window, cx);
        let pane = window.find("topics-pane").bounds();
        let panel = window.find("publish-panel").bounds();
        let top_minimum = window.rem_size() * 4.5;
        assert!((panel.top() - pane.top() - top_minimum).abs() <= px(1.));
        assert!((panel.size.height - (pane.size.height - top_minimum)).abs() <= px(1.));
        assert!(panel.size.height >= window.rem_size() * 19. - px(1.));
        assert!(panel.size.height < view.read(cx).publish_panes.read(cx).container_size() * 0.99);
        assert!(!view.read(cx).restore_publish_height);
    })
    .unwrap();
}

#[gpui_kit::test]
fn publish_panel_keeps_topic_tree_and_controls_visible_at_minimum_size_across_themes_and_zoom(cx: &mut TestAppContext) {
    for mode in [ThemeMode::Light, ThemeMode::Dark] {
        for font_size in [16., 20.] {
            let (handle, _) = open(cx, false, 760., 540.);
            cx.update(|cx| {
                Theme::change(mode, None, cx);
                Theme::update(cx, |theme| theme.font_size = px(font_size));
            });
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.click("toggle-publish", cx);
                let panel = window.find("publish-panel").bounds();
                let heading = window.find("topics-heading").bounds();
                assert!(
                    panel.top() > heading.bottom() + window.rem_size(),
                    "panel {panel:?}, heading {heading:?}, font {font_size}"
                );
                for id in [
                    "publish-topic",
                    "publish-payload",
                    "publish-retain",
                    "publish-qos",
                    "load-publish-file",
                    "publish-message",
                ] {
                    let control = window.find(id);
                    assert!(control.visible(), "{id} is not visible");
                    assert!(control.bounds().left() >= panel.left());
                    assert!(
                        control.bounds().right() <= panel.right(),
                        "{id}: {:?}, panel {panel:?}, font {font_size}",
                        control.bounds()
                    );
                    assert!(control.bounds().bottom() <= panel.bottom());
                }
                let topic = window.find("publish-topic").bounds();
                let payload = window.find("publish-payload").bounds();
                let action = window.find("publish-message").bounds();
                assert_eq!(topic.left(), payload.left());
                assert_eq!(topic.right(), action.right());
                let retain = window.find("publish-retain").bounds();
                let qos = window.find("publish-qos").bounds();
                let load = window.find("load-publish-file").bounds();
                assert!(
                    qos.size.width >= window.rem_size() * 3.5,
                    "QoS needs room for the selected digit and caret"
                );
                assert_eq!(window.find("publish-qos-field").bounds().left(), topic.left());
                assert!(qos.right() < retain.left());
                assert!(retain.right() < load.left());
                assert!(load.right() < action.left());
                for bounds in [retain, qos, load] {
                    assert!(
                        (bounds.center().y - action.center().y).abs() <= px(1.),
                        "controls must share one row"
                    );
                }
                assert_eq!(window.find("load-publish-file").label(), Some("Load payload from file"));
                assert!(load.size.width <= load.size.height, "file action must remain icon-only");
                assert!(
                    payload.size.height > window.rem_size() * 2.,
                    "payload {payload:?}, panel {panel:?}, rem {:?}",
                    window.rem_size()
                );
            })
            .unwrap();
        }
    }
}

#[gpui_kit::test]
fn publish_panel_does_not_intercept_slashes_as_topic_filter_shortcuts(cx: &mut TestAppContext) {
    let (handle, _) = open(cx, false, 900., 640.);
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("toggle-publish", cx);
        window.click("publish-topic", cx);
        window.input("home/command", cx);
        assert_eq!(window.find("publish-topic").value(), Some("home/command"));
        assert_eq!(window.find("publish-topic").focused(), Some(true));
    })
    .unwrap();
}

fn subscription(topic: &str, qos: u8) -> TopicSubscription {
    TopicSubscription { topic: topic.into(), qos }
}

fn message(topic: &str) -> Message {
    Message {
        topic: topic.into(),
        payload: Bytes::from_static(b"42"),
        qos: 0,
        retained: true,
        received_at: Local::now(),
        properties: Default::default(),
    }
}

#[gpui_kit::test]
fn broker_events_update_connection_topics_and_selected_payload_through_async_delivery(cx: &mut TestAppContext) {
    use crate::mqtt::BrokerEvent;

    let (handle, view) = open(cx, false, 1200., 760.);
    let (sender, events) = tokio::sync::mpsc::channel(4);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.status = ConnectionStatus::Connecting;
            view.select_topic("home/a", window, cx);
            view.listen_for_events(events, window, cx);
        });
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        assert!(view.read(cx).status.is_connecting());
        assert_eq!(view.read(cx).payload.read(cx).value().as_str(), "42");
        assert!(!view.read(cx).topics.nodes.contains_key("home/pushed"));
    });

    let mut update = message("home/a");
    update.payload = Bytes::from_static(b"delivered asynchronously");
    assert!(sender.try_send(BrokerEvent::Connected).is_ok());
    assert!(sender.try_send(BrokerEvent::Message(update)).is_ok());
    assert!(sender.try_send(BrokerEvent::Message(message("home/pushed"))).is_ok());
    cx.run_until_parked();

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let view = view.read(cx);
        assert!(view.status.is_connected());
        assert_eq!(window.find("connection-indicator").label(), Some("Connected"));
        assert_eq!(
            view.topics.nodes["home/a"].value.as_ref().unwrap().payload.as_ref(),
            b"delivered asynchronously"
        );
        assert_eq!(view.payload.read(cx).value().as_str(), "delivered asynchronously");
        assert!(view.topics.nodes.contains_key("home/pushed"));
        assert!(view.tree_state.read(cx).index_of(&"home/pushed".into()).is_some());
    })
    .unwrap();
}

#[gpui_kit::test]
fn topic_animation_reuses_the_details_publish_and_header_panes(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 1200., 760.);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| view.select_topic("home/a", window, cx));
        window.render_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| window.draw(cx).clear(cx)).unwrap();
    cx.run_until_parked();
    // Let initial native title-bar and focus callbacks settle before measuring.
    for _ in 0..2 {
        cx.update_window(handle, |_, window, cx| window.simulate_next_frame(cx)).unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| window.draw(cx).clear(cx)).unwrap();
        cx.run_until_parked();
    }

    let counts = |cx: &App| {
        let view = view.read(cx);
        [
            view.topics_view.read(cx).render_count,
            view.details_view.read(cx).render_count,
            view.publish_view.read(cx).render_count,
            view.header_view.read(cx).render_count,
        ]
    };
    let before = cx.update(|cx| counts(cx));
    for _ in 0..3 {
        cx.update_window(handle, |_, window, cx| {
            assert!(window.simulate_next_frame(cx) > 0);
        })
        .unwrap();
        cx.run_until_parked();
        // render_frame forces a whole-window refresh and intentionally bypasses
        // caches; use the normal draw path to test animation invalidation.
        cx.update_window(handle, |_, window, cx| window.draw(cx).clear(cx)).unwrap();
        cx.run_until_parked();
    }
    let after = cx.update(|cx| counts(cx));
    assert!(after[0] > before[0], "the animated topic pane must repaint");
    assert_eq!(after[1..], before[1..], "tree animation must not rebuild sibling panes");
}

#[gpui_kit::test]
fn topic_animation_rebuilds_only_the_updated_row_and_its_ancestors(cx: &mut TestAppContext) {
    use crate::mqtt::BrokerEvent;
    use std::time::{Duration, Instant};

    let (handle, view) = open(cx, false, 1200., 760.);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            for node in view.topics.nodes.values_mut() {
                node.updated = Some(Instant::now() - Duration::from_secs(1));
            }
            view.select_topic("home/a", window, cx);
        });
        window.render_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
    for _ in 0..3 {
        cx.update_window(handle, |_, window, cx| window.simulate_next_frame(cx)).unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| window.draw(cx).clear(cx)).unwrap();
        cx.run_until_parked();
    }

    let counts = |cx: &App| {
        let explorer = view.read(cx);
        let rows = explorer.topic_rows.borrow();
        ["home", "home/a", "home/b"].map(|path| rows.get(path).unwrap().read(cx).render_count)
    };
    let before = cx.update(|cx| counts(cx));
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.apply_broker_events([BrokerEvent::Message(message("home/a"))], window, cx)
        });
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| window.draw(cx).clear(cx)).unwrap();
    cx.run_until_parked();
    for _ in 0..3 {
        cx.update_window(handle, |_, window, cx| assert!(window.simulate_next_frame(cx) > 0))
            .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| window.draw(cx).clear(cx)).unwrap();
        cx.run_until_parked();
    }
    let after = cx.update(|cx| counts(cx));
    assert!(after[0] > before[0] + 2, "the updated ancestor must animate");
    assert!(after[1] > before[1] + 2, "the updated leaf must animate");
    assert_eq!(
        after[2], before[2],
        "an unchanged sibling must reuse its row layout and paint cache"
    );

    // Backdate the model rather than sleeping: the next requested frame must
    // render the final color and stop requesting frames after the flash expires.
    view.update(cx, |view, _| {
        for node in view.topics.nodes.values_mut() {
            node.updated = Some(Instant::now() - Duration::from_secs(1));
        }
    });
    cx.update_window(handle, |_, window, cx| window.simulate_next_frame(cx)).unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| window.draw(cx).clear(cx)).unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| window.simulate_next_frame(cx)).unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| window.draw(cx).clear(cx)).unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| assert_eq!(window.simulate_next_frame(cx), 0))
        .unwrap();
}

#[gpui_kit::test]
fn broker_update_rebuilds_the_first_cached_row_without_animation(cx: &mut TestAppContext) {
    use crate::mqtt::BrokerEvent;

    cx.update(|cx| cx.set_reduce_motion(true));
    let (handle, view) = open(cx, false, 1200., 760.);
    cx.update_window(handle, |_, window, cx| window.render_frame(cx)).unwrap();
    cx.run_until_parked();
    for _ in 0..3 {
        cx.update_window(handle, |_, window, cx| window.simulate_next_frame(cx)).unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| window.draw(cx).clear(cx)).unwrap();
        cx.run_until_parked();
    }
    let count = |cx: &App| view.read(cx).topic_rows.borrow().get("home").unwrap().read(cx).render_count;
    let before = cx.update(|cx| count(cx));
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.apply_broker_events([BrokerEvent::Message(message("home"))], window, cx)
        });
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| window.draw(cx).clear(cx)).unwrap();
    cx.run_until_parked();
    // Cached rows have a definite size, so list measurement does not invoke
    // Render. This must be the visible row's rebuild on the first draw, not a
    // discarded measurement or a later corrective animation frame.
    assert_eq!(cx.update(|cx| count(cx)), before + 1);
    cx.update_window(handle, |_, window, cx| assert_eq!(window.simulate_next_frame(cx), 0))
        .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| window.draw(cx).clear(cx)).unwrap();
    cx.run_until_parked();
    assert_eq!(
        cx.update(|cx| count(cx)),
        before + 1,
        "the row must settle without a render-notify loop"
    );
}

#[gpui_kit::test]
fn topic_row_views_are_virtualized_and_pruned_when_filtered_out(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 1200., 760.);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            for index in 0..400 {
                view.topics
                    .receive(message(&format!("home/sensor-{index:03}")), std::time::Instant::now());
            }
            view.select_topic("home/a", window, cx);
        });
        window.render_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        let explorer = view.read(cx);
        assert!(
            explorer.topic_rows.borrow().len() < explorer.topics.nodes.len() / 4,
            "only visited rows should allocate a view"
        );
    });
    cx.update_window(handle, |_, window, cx| {
        window.click("topic-search", cx);
        window.input("no matching topics", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| window.draw(cx).clear(cx)).unwrap();
    cx.run_until_parked();
    cx.update(|cx| assert!(view.read(cx).topic_rows.borrow().is_empty()));
    cx.update_window(handle, |_, window, cx| window.simulate_next_frame(cx)).unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| window.draw(cx).clear(cx)).unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        assert_eq!(
            window.simulate_next_frame(cx),
            0,
            "hidden rows must not keep the frame loop running"
        )
    })
    .unwrap();
}

#[gpui_kit::test]
fn broker_updates_invalidate_only_the_affected_cached_panes(cx: &mut TestAppContext) {
    use crate::mqtt::BrokerEvent;

    let (handle, view) = open(cx, false, 1200., 760.);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| view.select_topic("home/a", window, cx));
        window.render_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| window.draw(cx).clear(cx)).unwrap();
    cx.run_until_parked();

    let counts = |cx: &App| {
        let view = view.read(cx);
        [
            view.topics_view.read(cx).render_count,
            view.details_view.read(cx).render_count,
            view.publish_view.read(cx).render_count,
            view.header_view.read(cx).render_count,
        ]
    };
    let before = cx.update(|cx| counts(cx));
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.apply_broker_events([BrokerEvent::Message(message("home/b"))], window, cx)
        });
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| window.draw(cx).clear(cx)).unwrap();
    cx.run_until_parked();
    let after_unselected = cx.update(|cx| counts(cx));
    assert!(after_unselected[0] > before[0]);
    assert_eq!(after_unselected[1..], before[1..]);

    cx.update_window(handle, |_, window, cx| {
        let mut update = message("home/a");
        update.payload = Bytes::from_static(b"updated selected payload");
        view.update(cx, |view, cx| view.apply_broker_events([BrokerEvent::Message(update)], window, cx));
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| window.draw(cx).clear(cx)).unwrap();
    cx.run_until_parked();
    let after_selected = cx.update(|cx| counts(cx));
    assert!(after_selected[1] > after_unselected[1]);
    assert_eq!(after_selected[2..], before[2..]);
    cx.update(|cx| assert_eq!(view.read(cx).payload.read(cx).value().as_str(), "updated selected payload"));
}

#[gpui_kit::test]
fn broker_event_channel_closure_fails_pending_publish_and_retains_connection_status_context(cx: &mut TestAppContext) {
    use crate::mqtt::BrokerEvent;

    let (handle, view) = open(cx, false, 1200., 760.);
    let (sender, events) = tokio::sync::mpsc::channel(1);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| view.listen_for_events(events, window, cx));
    })
    .unwrap();
    assert!(sender.try_send(BrokerEvent::Status("Broker unavailable".into())).is_ok());
    cx.run_until_parked();
    view.update(cx, |view, _| {
        assert_eq!(view.status.label(), "Broker unavailable");
        view.publish_pending = true;
        view.publish_feedback = None;
    });

    drop(sender);
    cx.run_until_parked();

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let view = view.read(cx);
        assert!(view.connection.is_none());
        assert!(!view.publish_pending);
        assert_eq!(
            view.publish_feedback.as_ref().unwrap().as_deref().map_err(String::as_str),
            Err("Connection stopped before the message could be queued. Reconnect and try again.")
        );
        assert!(matches!(&view.status, ConnectionStatus::Failed(status) if status == "Connection stopped · Broker unavailable"));
        assert_eq!(
            window.find("connection-indicator").label(),
            Some("Connection stopped · Broker unavailable")
        );
        assert!(window.find("connection-error").visible());
    })
    .unwrap();
}

#[gpui_kit::test]
fn replacing_broker_event_listener_cancels_old_channel_and_ignores_queued_events(cx: &mut TestAppContext) {
    use crate::mqtt::BrokerEvent;

    let (handle, view) = open(cx, false, 1200., 760.);
    let (old_sender, old_events) = tokio::sync::mpsc::channel(2);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| view.listen_for_events(old_events, window, cx));
    })
    .unwrap();
    cx.run_until_parked();
    assert!(old_sender.try_send(BrokerEvent::Status("Stale connection failure".into())).is_ok());
    assert!(old_sender.try_send(BrokerEvent::Message(message("home/stale"))).is_ok());

    let (sender, events) = tokio::sync::mpsc::channel(2);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.status = ConnectionStatus::Connecting;
            view.listen_for_events(events, window, cx);
        });
    })
    .unwrap();
    assert!(sender.try_send(BrokerEvent::Connected).is_ok());
    assert!(sender.try_send(BrokerEvent::Message(message("home/current"))).is_ok());
    cx.run_until_parked();
    assert!(old_sender.is_closed());
    drop(old_sender);
    cx.run_until_parked();

    cx.update(|cx| {
        let view = view.read(cx);
        assert!(view.status.is_connected());
        assert!(!view.topics.nodes.contains_key("home/stale"));
        assert!(view.topics.nodes.contains_key("home/current"));
        assert!(view.error.is_none());
    });
}

#[gpui_kit::test]
fn connection_failures_use_alerts_and_the_status_bar_is_removed(cx: &mut TestAppContext) {
    for font_size in [16., 20.] {
        let (handle, view) = open(cx, false, 760., 540.);
        cx.update(|cx| Theme::update(cx, |theme| theme.font_size = px(font_size)));
        for status in [
            ConnectionStatus::Disconnected,
            ConnectionStatus::Connecting,
            ConnectionStatus::Connected,
            ConnectionStatus::Failed("Could not connect to broker".into()),
        ] {
            let failed = matches!(status, ConnectionStatus::Failed(_));
            view.update(cx, |view, cx| {
                view.status = status;
                cx.notify();
            });
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                assert!(window.try_find("status").is_none());
                assert_eq!(window.try_find("connection-error").is_some(), failed);
                let picker = window.find("connection-picker").bounds();
                let indicator = window.find("connection-indicator");
                let settings = window.find("settings").bounds();
                assert!(indicator.visible());
                assert_eq!(indicator.label(), Some(view.read(cx).status.label()));
                assert!(indicator.bounds().left() >= picker.right());
                assert!(indicator.bounds().right() <= settings.left());
            })
            .unwrap();
        }
    }
}

#[gpui_kit::test]
fn application_errors_are_shown_in_alerts(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 900., 640.);
    view.update(cx, |view, cx| {
        view.error = Some("Could not save the application state.".into());
        cx.notify();
    });
    cx.update_window(handle, |_, window, cx| {
        assert!(view.read(cx).error.is_some());
        window.render_frame(cx);
        let alert = window.find("application-error");
        assert!(alert.visible());
        let alert_bounds = alert.bounds();
        assert_eq!(view.read(cx).error.as_deref(), Some("Could not save the application state."));
        // Alert's built-in close control has no test-support registration.
        window.click_at("application-error", gpui_kit::point(alert_bounds.size.width - px(26.), px(20.)), cx);
        window.render_frame(cx);
        assert!(view.read(cx).error.is_none());
        assert!(window.try_find("application-error").is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
fn publish_errors_use_dismissible_alerts_without_changing_the_draft_layout(cx: &mut TestAppContext) {
    for mode in [ThemeMode::Light, ThemeMode::Dark] {
        for font_size in [16., 20.] {
            let (handle, view) = open_publish_file_draft(cx, 760., 540.);
            cx.update(|cx| {
                Theme::change(mode, None, cx);
                Theme::update(cx, |theme| theme.font_size = px(font_size));
            });
            let payload_bounds = cx
                .update_window(handle, |_, window, cx| {
                    window.render_frame(cx);
                    let bounds = window.find("publish-payload").bounds();
                    view.update(cx, |view, cx| {
                        view.publish_pending = true;
                        view.apply_broker_events(
                            [crate::mqtt::BrokerEvent::PublishError(
                                "Broker rejected publish: NotAuthorized".into(),
                            )],
                            window,
                            cx,
                        );
                    });
                    bounds
                })
                .unwrap();
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                let alert = window.find("publish-error");
                assert!(alert.visible());
                assert_eq!(alert.label(), Some("Broker rejected publish: NotAuthorized"));
                assert!(window.try_find("publish-feedback").is_none());
                assert_eq!(window.find("publish-payload").bounds(), payload_bounds);
                assert!(!view.read(cx).publish_pending);
                assert_eq!(view.read(cx).publish_payload.read(cx).value().as_str(), PUBLISH_FILE_DRAFT);
                let bounds = alert.bounds();
                window.click_at("publish-error", gpui_kit::point(bounds.size.width - px(26.), px(20.)), cx);
                window.render_frame(cx);
                assert!(window.try_find("publish-error").is_none());
                assert!(view.read(cx).publish_feedback.is_none());
                assert_eq!(window.find("publish-payload").bounds(), payload_bounds);
                assert_eq!(view.read(cx).publish_payload.read(cx).value().as_str(), PUBLISH_FILE_DRAFT);
            })
            .unwrap();
        }
    }
}

#[gpui_kit::test]
fn settings_block_dragging_the_underlying_pane_divider(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 760., 540.);
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("settings", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("settings-dialog").visible());
        let pane = window.find("topics-pane").bounds();
        let sizes = view.read(cx).panes.read(cx).sizes().clone();
        // Cover both the settings surface and the backdrop below it.
        for y in [pane.center().y, pane.bottom() - px(1.)] {
            let from = gpui_kit::point(pane.right(), y);
            window.drag(from, from + gpui_kit::point(px(80.), px(0.)), cx);
            assert_eq!(view.read(cx).panes.read(cx).sizes(), &sizes);
        }
        window.click("close-settings", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("settings-dialog").is_none());
        let pane = window.find("topics-pane").bounds();
        let width = view.read(cx).panes.read(cx).sizes()[0];
        let from = gpui_kit::point(pane.right(), pane.center().y);
        window.drag(from, from + gpui_kit::point(px(80.), px(0.)), cx);
        assert!(view.read(cx).panes.read(cx).sizes()[0] > width);
    })
    .unwrap();
}

#[gpui_kit::test]
fn appearance_selectors_stay_compact_and_aligned(cx: &mut TestAppContext) {
    for theme in [ThemeMode::Light, ThemeMode::Dark] {
        for (width, font_size) in [(760., 16.), (760., 20.), (1200., 16.), (1200., 20.)] {
            let (handle, _) = open(cx, true, width, 540.);
            cx.update(|cx| {
                Theme::change(theme, None, cx);
                Theme::update(cx, |theme| theme.font_size = px(font_size));
            });
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.click("0-1", cx);
            })
            .unwrap();
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                let settings = window.find("settings-dialog").bounds();
                let mode = window.find("appearance-mode").bounds();
                let light = window.find("light-theme").bounds();
                let dark = window.find("dark-theme").bounds();
                for selector in [mode, light, dark] {
                    assert!(
                        selector.size.width < settings.size.width / 2.,
                        "appearance selectors should remain compact: selector={selector:?}, settings={settings:?}"
                    );
                    assert!(selector.left() >= settings.left() && selector.right() <= settings.right());
                    assert_eq!(selector.left(), mode.left(), "selector leading edges should align");
                    assert_eq!(selector.right(), mode.right(), "selector trailing edges should align");
                    assert_eq!(selector.size, mode.size, "appearance selectors should share one control size");
                }
                assert!(mode.bottom() < light.top());
                assert!(light.bottom() < dark.top());
                let mode_label = window.find("appearance-mode-label").bounds();
                let mut previous_bottom = None;
                for (id, selector) in [("appearance-mode", mode), ("light-theme", light), ("dark-theme", dark)] {
                    let label = window.find(format!("{id}-label")).bounds();
                    let description = window.find(format!("{id}-description")).bounds();
                    assert_eq!(label.left(), mode_label.left(), "field labels should share a leading edge");
                    assert_eq!(description.left(), label.left(), "help text should align with its label");
                    assert_eq!(description.top() - label.bottom(), window.rem_size() * 0.25);
                    if selector.left() == label.left() {
                        assert_eq!(selector.top() - description.bottom(), window.rem_size() * 0.5);
                    } else {
                        assert_eq!(selector.center().y, label.center().y, "align controls to labels, not descriptions");
                        assert_eq!(selector.left() - label.right(), window.rem_size() * 1.5);
                    }
                    if let Some(bottom) = previous_bottom {
                        assert_eq!(label.top() - bottom, window.rem_size(), "setting rows should share one gap");
                    }
                    previous_bottom = Some(description.bottom().max(selector.bottom()));
                }
            })
            .unwrap();
        }
    }
}

#[gpui_kit::test]
fn appearance_menus_support_keyboard_selection_dismissal_and_saved_preferences(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("appearance.json");
    let (handle, _) = open(cx, true, 760., 540.);
    cx.update(|cx| cx.set_global(crate::appearance::load(&path, cx).unwrap()));
    cx.update_window(handle, |_, window, cx| {
        crate::appearance::select_mode(AppearanceMode::Dark, window, cx).unwrap();
        window.render_frame(cx);
        window.click("0-1", cx);
    })
    .unwrap();
    cx.run_until_parked();
    let previous_focus = cx
        .update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("appearance-mode").label(), Some("Mode: Dark"));
            assert_eq!(window.find("light-theme").label(), Some("Light theme: Ayu Light"));
            assert_eq!(window.find("dark-theme").label(), Some("Dark theme: Charcoal Grove"));
            let focus = window.focused(cx);
            window.click("light-theme", cx);
            focus
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
        assert!(window.find("settings-dialog").visible());
        assert_eq!(window.focused(cx), previous_focus);
        window.click("light-theme", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let themes: Vec<_> = gpui_kit::component::ThemeRegistry::global(cx)
            .sorted_themes()
            .into_iter()
            .filter(|theme| theme.mode == ThemeMode::Light)
            .collect();
        let menu = window.within("popup-menu");
        for (ix, theme) in themes.iter().enumerate() {
            assert_eq!(menu.find(ix).label(), Some(theme.name.as_str()));
        }
        let target = themes.iter().position(|theme| theme.name == "Gruvbox Light").unwrap();
        for _ in 0..themes.len() {
            window.press("down", cx);
            if window.within("popup-menu").find(target).selected() == Some(true) {
                break;
            }
        }
        assert_eq!(window.within("popup-menu").find(target).selected(), Some(true));
        window.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(Appearance::mode(cx), AppearanceMode::Dark);
        assert_eq!(cx.theme().theme_name().as_str(), "Charcoal Grove");
        assert_eq!(window.find("light-theme").label(), Some("Light theme: Gruvbox Light"));
        window.click("appearance-mode", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let menu = window.within("popup-menu");
        for (ix, label) in ["System", "Light", "Dark"].into_iter().enumerate() {
            assert_eq!(menu.find(ix).label(), Some(label));
        }
        for _ in 0..3 {
            window.press("down", cx);
            if window.within("popup-menu").find(1usize).selected() == Some(true) {
                break;
            }
        }
        assert_eq!(window.within("popup-menu").find(1usize).selected(), Some(true));
        window.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(Appearance::mode(cx), AppearanceMode::Light);
        assert_eq!(cx.theme().theme_name().as_str(), "Gruvbox Light");
        crate::appearance::sync_system(window, cx);
        assert_eq!(cx.theme().theme_name().as_str(), "Gruvbox Light");
        assert_eq!(window.find("appearance-mode").label(), Some("Mode: Light"));
        window.click("dark-theme", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let themes: Vec<_> = gpui_kit::component::ThemeRegistry::global(cx)
            .sorted_themes()
            .into_iter()
            .filter(|theme| theme.mode == ThemeMode::Dark)
            .collect();
        let target = themes.iter().position(|theme| theme.name == "Tokyo Night").unwrap();
        let menu = window.within("popup-menu");
        for (ix, theme) in themes.iter().enumerate() {
            assert_eq!(menu.find(ix).label(), Some(theme.name.as_str()));
        }
        window.scroll("popup-menu", gpui_kit::ScrollDelta::Lines(gpui_kit::point(0., -20.)), cx);
        window.render_frame(cx);
        window.within("popup-menu").click(target, cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(cx.theme().theme_name().as_str(), "Gruvbox Light");
        assert_eq!(window.find("dark-theme").label(), Some("Dark theme: Tokyo Night"));
        window.click("appearance-mode", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.within("popup-menu").click(2usize, cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(Appearance::mode(cx), AppearanceMode::Dark);
        assert_eq!(cx.theme().theme_name().as_str(), "Tokyo Night");
        assert_eq!(cx.theme().highlight_theme.name, "Tokyo Night");
        window.click("appearance-mode", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.within("popup-menu").click(0usize, cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(Appearance::mode(cx), AppearanceMode::System);
        let expected = if ThemeMode::from(window.appearance()).is_dark() {
            "Tokyo Night"
        } else {
            "Gruvbox Light"
        };
        assert_eq!(cx.theme().theme_name().as_str(), expected);
        assert_eq!(window.find("appearance-mode").label(), Some("Mode: System"));
        assert!(window.try_find("popup-menu").is_none());
        assert!(window.find("settings-dialog").visible());
    })
    .unwrap();
    cx.update(|cx| {
        cx.set_global(crate::appearance::load(&path, cx).unwrap());
        assert_eq!(Appearance::mode(cx), AppearanceMode::System);
        assert_eq!(Appearance::selected_theme(ThemeMode::Light, cx), "Gruvbox Light");
        assert_eq!(Appearance::selected_theme(ThemeMode::Dark, cx), "Tokyo Night");
    });
}

#[gpui_kit::test]
fn title_bar_picker_lists_saved_servers_and_settings_hold_theme(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 1200., 760.);
    view.update(cx, |view, cx| {
        view.saved_connections.connections.push(ConnectionConfig {
            name: "Workshop".into(),
            host: "second.example".into(),
            username: "reader".into(),
            tls: true,
            ..Default::default()
        });
        cx.notify();
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("toolbar").visible());
        assert!(window.find("connection-picker").visible());
        assert!(window.try_find("appearance-mode").is_none());
        assert!(window.try_find("disconnect").is_none());
        window.click("connection-picker", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("connection-picker").label(), Some("Home"));
        let menu = window.within("popup-menu");
        assert_eq!(menu.find(0usize).label(), Some("Home"));
        assert_eq!(menu.find(1usize).label(), Some("Workshop"));
        window.press("escape", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("settings", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("settings-dialog").visible());
        assert!(window.find("connections-overview").visible());
        assert!(window.find("connection-card:0").visible());
        assert!(window.find("connection-card:1").visible());
        assert!(window.try_find("name").is_none());
        assert!(window.try_find("appearance-mode").is_none());
        assert_eq!(view.read(cx).saved_connections.connections.len(), 2);
        assert_eq!(view.read(cx).saved_connections.selected, Some(0));
        window.click("0-1", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("settings-dialog").visible());
        assert!(window.find("appearance-mode").visible());
        assert!(window.find("light-theme").visible());
        assert!(window.find("dark-theme").visible());
        assert_eq!(window.find("close-settings").label(), Some("Close settings"));
        window.click("close-settings", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("settings-dialog").is_none());
        assert!(window.find("connection-picker").visible());
    })
    .unwrap();
}

#[gpui_kit::test]
fn connection_card_labels_align_with_the_icon_square(cx: &mut TestAppContext) {
    for font_size in [14., 20.] {
        for width in [760., 1200.] {
            let (handle, _) = open(cx, false, width, 760.);
            cx.update(|cx| Theme::update(cx, |theme| theme.font_size = px(font_size)));
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.click("settings", cx);
                window.render_frame(cx);
                let icon = window.find("connection-icon:0").bounds();
                let name = window.find("connection-name:0").bounds();
                let details = window.find("connection-details:0").bounds();
                let card = window.find("connection-card:0").bounds();
                assert_eq!(name.top(), icon.top(), "connection name should align with the top of the icon");
                assert_eq!(
                    details.bottom(),
                    icon.bottom(),
                    "connection details should align with the bottom of the icon"
                );
                assert!(name.bottom() <= details.top(), "connection labels should not overlap");
                assert!(
                    card.size.height <= window.rem_size() * 3.5 + px(2.),
                    "connection card should use compact vertical padding"
                );
            })
            .unwrap();
        }
    }
}

#[gpui_kit::test]
fn add_connection_aligns_with_the_right_edge_of_the_connections_header(cx: &mut TestAppContext) {
    for font_size in [14., 20.] {
        for width in [760., 1200.] {
            let (handle, _) = open(cx, false, width, 760.);
            cx.update(|cx| Theme::update(cx, |theme| theme.font_size = px(font_size)));
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.click("settings", cx);
                window.render_frame(cx);
                let actions = window.find("connections-header-actions").bounds();
                assert_eq!(window.find("new-connection").label(), Some("Add connection"));
                let button = window.find("new-connection").bounds();
                assert_eq!(button.size.width, button.size.height, "add action should be an icon-only button");
                let card = window.find("connection-card:0").bounds();
                assert_eq!(
                    button.right(),
                    actions.right(),
                    "header action should sit at the suffix's right edge"
                );
                assert_eq!(button.right(), card.right(), "header action should align with the connection cards");
                assert!(button.left() >= card.left(), "header action should remain inside the page");
                assert!(button.bottom() <= card.top(), "header action should stay above the cards");
            })
            .unwrap();
        }
    }
}

#[gpui_kit::test]
fn connection_cards_open_a_full_width_form_and_return_to_the_overview(cx: &mut TestAppContext) {
    let (handle, _) = open(cx, false, 760., 540.);
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("settings", cx);
        window.render_frame(cx);
        assert!(window.find("connection-card:0").bounds().size.width > px(300.));
        assert!(window.try_find("name").is_none());
        let edit = window.find("edit-connection:0");
        let delete = window.find("delete-connection:0");
        assert_eq!(edit.label(), Some("Edit connection"));
        assert_eq!(delete.label(), Some("Delete connection"));
        assert!(delete.visible());
        assert!(delete.bounds().left() >= edit.bounds().right());
        window.click("edit-connection:0", cx);
        window.render_frame(cx);
        assert!(window.try_find("connections-overview").is_none());
        assert!(window.find("name").bounds().size.width > px(250.));
        let broker = window.find("broker-settings").bounds();
        assert!(broker.size.width > px(250.));
        assert!(window.find("host").bounds().size.width > window.find("port").bounds().size.width);
        window.click("back-to-connections", cx);
        window.render_frame(cx);
        assert!(window.find("connection-card:0").visible());
        window.click("new-connection", cx);
        window.render_frame(cx);
        assert_eq!(window.find("name").value(), Some(""));
        assert_eq!(window.find("host").value(), Some("localhost"));
        assert!(window.find("client-id").value().unwrap().starts_with("topq-"));
    })
    .unwrap();
}

#[gpui_kit::test]
fn canceling_connection_deletion_preserves_the_connection_and_topics(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 1200., 760.);
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("settings", cx);
    })
    .unwrap();
    for button in ["delete-connection:0", "remove-connection"] {
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            if button == "remove-connection" {
                window.click("edit-connection:0", cx);
                window.render_frame(cx);
                window.scroll("name", gpui_kit::ScrollDelta::Lines(gpui_kit::point(0., -100.)), cx);
            }
            window.click(button, cx);
        })
        .unwrap();
        assert!(cx.has_pending_prompt());
        assert_eq!(cx.pending_prompt().unwrap().0, "Delete connection \"Home\"?");
        cx.update(|cx| {
            assert_eq!(view.read(cx).saved_connections.connections.len(), 1);
            assert_eq!(view.read(cx).topics.topics, 2);
        });
        cx.simulate_prompt_answer("Cancel");
        cx.run_until_parked();
        cx.update(|cx| {
            let view = view.read(cx);
            assert_eq!(view.saved_connections.connections.len(), 1);
            assert!(view.status.is_connected());
            assert!(view.active_config.is_some());
            assert_eq!(view.topics.topics, 2);
        });
    }
}

#[gpui_kit::test]
fn deleting_the_active_connection_clears_the_topic_tree_and_details(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 1200., 760.);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.expanded.insert("home".into());
            view.selected = Some("home/a".into());

            view.sync_tree(cx);
            view.refresh_details(window, cx);
            assert_eq!(view.payload.read(cx).value().as_str(), "42");
            view.remove_connection_with_save(0, window, cx, |saved| {
                assert!(saved.connections.is_empty());
                assert_eq!(saved.selected, None);
                Ok(())
            });
        });
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        let view = view.read(cx);
        assert!(view.saved_connections.connections.is_empty());
        assert!(view.connection.is_none());
        assert!(view.active_config.is_none());
        assert!(matches!(view.status, ConnectionStatus::Disconnected));
        assert!(view.topics.nodes.is_empty());
        assert_eq!(view.topics.topics, 0);
        assert_eq!(view.topics.messages, 0);
        assert!(view.expanded.is_empty());
        assert!(view.selected.is_none());

        assert!(view.tree_state.read(cx).index_of(&"home".into()).is_none());
        assert!(view.tree_state.read(cx).selected_item().is_none());
        assert_eq!(view.payload.read(cx).value().as_str(), "");
    });
}

#[gpui_kit::test]
fn deleting_an_inactive_connection_preserves_the_active_connections_topics(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 1200., 760.);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.saved_connections.connections.insert(
                0,
                ConnectionConfig {
                    name: "Workshop".into(),
                    host: "second.example".into(),
                    ..Default::default()
                },
            );
            view.saved_connections.selected = Some(1);
            view.remove_connection_with_save(0, window, cx, |saved| {
                assert_eq!(saved.connections.len(), 1);
                assert_eq!(saved.connections[0].name, "Home");
                assert_eq!(saved.selected, Some(0));
                Ok(())
            });
        });
    })
    .unwrap();
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.saved_connections.selected, Some(0));
        assert!(view.status.is_connected());
        assert_eq!(view.active_config.as_ref().unwrap().name, "Home");
        assert_eq!(view.topics.topics, 2);
        assert_eq!(view.topics.messages, 2);
        assert!(view.topics.nodes.contains_key("home/a"));
        assert!(view.tree_state.read(cx).index_of(&"home".into()).is_some());
    });
}

#[gpui_kit::test]
fn confirming_connection_deletion_preserves_topics_when_saving_fails(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 1200., 760.);
    view.update(cx, |view, cx| {
        // Validation fails before any settings or credentials are written.
        view.saved_connections.connections.push(ConnectionConfig {
            name: "Invalid".into(),
            host: String::new(),
            ..Default::default()
        });
        cx.notify();
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("settings", cx);
        window.render_frame(cx);
        window.click("delete-connection:0", cx);
    })
    .unwrap();
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Delete");
    cx.run_until_parked();
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.saved_connections.connections.len(), 2);
        assert_eq!(view.saved_connections.selected, Some(0));
        assert!(view.status.is_connected());
        assert!(view.active_config.is_some());
        assert_eq!(view.topics.topics, 2);
        assert!(view.error.as_ref().unwrap().starts_with("Could not remove connection:"));
    });
}

#[gpui_kit::test]
fn new_connection_topics_default_to_all_topics_with_qos_zero_after_credentials(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, true, 1200., 1200.);
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let section = window.find("subscription-topics");
        assert!(section.visible());
        assert_eq!(section.label(), Some("Topics"));
        assert_eq!(section.expanded(), Some(true));
        assert!(window.find("subscription:#").visible());
        assert_eq!(window.find("topic-filter:#").label(), Some("#"));
        assert_eq!(window.find("topic-qos:#").label(), Some("QoS 0"));
        assert_eq!(window.find("remove-topic:#").label(), Some("Remove topic #"));
        assert_eq!(window.find("begin-add-topic").label(), Some("Add topic"));
        assert!(window.try_find("topic-editor").is_none());
        assert!(window.find("connection-field:client-id").bounds().top() > window.find("field:port").bounds().bottom());
        assert!(window.find("connection-field:username").bounds().top() > window.find("broker-settings").bounds().bottom());
        assert!(window.try_find("tls").is_none());
        assert!(window.find("field:username").bounds().bottom() < window.find("field:password").bounds().top());
        assert!(section.bounds().top() > window.find("connection-field:password").bounds().bottom());
        assert_eq!(view.read(cx).subscription_topics, vec![subscription("#", 0)]);
    })
    .unwrap();
}

#[gpui_kit::test]
fn editing_connections_resets_the_exact_saved_topics_and_new_connection_defaults(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 1200., 1600.);
    let saved = vec![subscription(" home/+/temperature ", 1), subscription("家//status", 2)];
    let other = vec![subscription("workshop/#", 0)];
    view.update(cx, |view, cx| {
        view.saved_connections.connections[0].topics = saved.clone();
        view.saved_connections.connections.push(ConnectionConfig {
            name: "Workshop".into(),
            topics: other.clone(),
            ..Default::default()
        });
        cx.notify();
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("settings", cx);
        window.click("edit-connection:0", cx);
        assert_eq!(view.read(cx).subscription_topics, saved);
        assert_eq!(
            window.find("topic-filter: home/+/temperature ").label(),
            Some(" home/+/temperature ")
        );
        assert_eq!(window.find("topic-qos: home/+/temperature ").label(), Some("QoS 1"));
        assert_eq!(window.find("topic-qos:家//status").label(), Some("QoS 2"));
        window.click("remove-topic: home/+/temperature ", cx);
        window.click("begin-add-topic", cx);
        window.input("unsaved/#", cx);
        window.click("cancel-topic", cx);
        assert!(window.try_find("topic-editor").is_none());
        assert_eq!(view.read(cx).saved_connections.connections[0].topics, saved);
        window.click("back-to-connections", cx);
        window.click("edit-connection:0", cx);
        assert_eq!(view.read(cx).subscription_topics, saved);
        assert!(window.find("subscription: home/+/temperature ").visible());
        window.click("begin-add-topic", cx);
        window.input("discarded/+", cx);
        window.click("back-to-connections", cx);
        window.click("edit-connection:1", cx);
        assert_eq!(view.read(cx).subscription_topics, other);
        assert_eq!(window.find("topic-filter:workshop/#").label(), Some("workshop/#"));
        assert!(window.try_find("subscription:家//status").is_none());
        assert!(window.try_find("topic-editor").is_none());
        assert_eq!(view.read(cx).topic_input.read(cx).value().as_str(), "");
        assert_eq!(view.read(cx).topic_qos.read(cx).selected_value(), Some(&"0"));
        window.click("back-to-connections", cx);
        window.click("new-connection", cx);
        assert_eq!(view.read(cx).subscription_topics, vec![subscription("#", 0)]);
        assert_eq!(window.find("topic-qos:#").label(), Some("QoS 0"));
        assert_eq!(window.find("subscription-topics").expanded(), Some(true));
        assert!(window.try_find("topic-editor").is_none());
        assert!(window.try_find("subscription:workshop/#").is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
fn collapsing_topics_preserves_rows_and_adding_from_collapsed_expands_the_editor(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, true, 1200., 1600.);
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("begin-add-topic", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.input("home/+/temperature", cx);
        window.click("add-topic", cx);
        assert!(window.try_find("topic-editor").is_none());
        assert_eq!(window.find("topic-qos:home/+/temperature").label(), Some("QoS 0"));
        window.click("toggle-topics", cx);
        assert_eq!(window.find("subscription-topics").expanded(), Some(false));
        assert_eq!(window.find("toggle-topics").label(), Some("Expand topics"));
        assert!(window.try_find("subscription:#").is_none_or(|row| !row.visible()));
        assert!(window.try_find("subscription:home/+/temperature").is_none_or(|row| !row.visible()));
        assert_eq!(
            view.read(cx).subscription_topics,
            vec![subscription("#", 0), subscription("home/+/temperature", 0)]
        );
        window.click("toggle-topics", cx);
        assert_eq!(window.find("subscription-topics").expanded(), Some(true));
        assert!(window.find("subscription:#").visible());
        assert!(window.find("subscription:home/+/temperature").visible());
        window.click("toggle-topics", cx);
        window.click("begin-add-topic", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("subscription-topics").expanded(), Some(true));
        assert!(window.find("topic-editor").visible());
        assert_eq!(window.find("topic-filter").value(), Some(""));
        assert_eq!(window.find("topic-qos").value(), Some("0"));
        assert_eq!(window.find("topic-filter").focused(), Some(true));
        window.input("discarded/#", cx);
        window.click("cancel-topic", cx);
        assert!(window.try_find("topic-editor").is_none());
        assert_eq!(
            view.read(cx).subscription_topics,
            vec![subscription("#", 0), subscription("home/+/temperature", 0)]
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn adding_topic_filters_with_keyboard_qos_selection_restores_focus(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, true, 1200., 1600.);
    cx.update_window(handle, |_, window, _| window.activate_window()).unwrap();
    cx.run_until_parked();
    for (filter, qos) in [("home/+/temperature", 1), ("家//status/#", 2)] {
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click("name", cx);
            window.click("begin-add-topic", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("topic-filter").focused(), Some(true));
            assert_eq!(window.find("topic-qos").value(), Some("0"));
            window.input(filter, cx);
            window.press("tab", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("topic-qos").focused(), Some(true));
            window.press("enter", cx);
        })
        .unwrap();
        cx.run_until_parked();
        for _ in 0..qos {
            cx.update_window(handle, |_, window, cx| window.press("down", cx)).unwrap();
            cx.run_until_parked();
        }
        cx.update_window(handle, |_, window, cx| window.press("enter", cx)).unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("topic-qos").value(), Some(qos.to_string().as_str()));
            assert_eq!(window.find("topic-qos").expanded(), Some(false));
            assert_eq!(
                view.read(cx).topic_qos.read(cx).selected_value().copied(),
                Some(qos.to_string().as_str())
            );
            window.press("shift-tab", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("topic-filter").focused(), Some(true));
            window.press("enter", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("topic-editor").is_none());
            assert_eq!(window.find(format!("topic-filter:{filter}")).label(), Some(filter));
            assert_eq!(
                window.find(format!("topic-qos:{filter}")).label(),
                Some(format!("QoS {qos}").as_str())
            );
            assert_eq!(window.find("name").focused(), Some(true));
            assert!(view.read(cx).saved_connections.connections.is_empty());
            assert!(view.read(cx).connection.is_none());
        })
        .unwrap();
    }
    cx.update(|cx| {
        assert_eq!(
            view.read(cx).subscription_topics,
            vec![
                subscription("#", 0),
                subscription("home/+/temperature", 1),
                subscription("家//status/#", 2)
            ]
        );
    });
}

#[gpui_kit::test]
fn topic_editor_buttons_support_keyboard_activation_and_cancel_restores_the_trigger(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, true, 1200., 1600.);
    cx.update_window(handle, |_, window, _| window.activate_window()).unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("port", cx);
    })
    .unwrap();
    for id in [
        "client-id",
        "username",
        "password",
        "toggle-password",
        "toggle-topics",
        "begin-add-topic",
    ] {
        cx.update_window(handle, |_, window, cx| window.press("tab", cx)).unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find(id).focused(), Some(true));
        })
        .unwrap();
    }
    cx.update_window(handle, |_, window, cx| window.press("enter", cx)).unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("topic-filter").focused(), Some(true));
        window.input("discarded/#", cx);
    })
    .unwrap();
    for id in ["topic-qos", "cancel-topic"] {
        cx.update_window(handle, |_, window, cx| window.press("tab", cx)).unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find(id).focused(), Some(true));
        })
        .unwrap();
    }
    cx.update_window(handle, |_, window, cx| window.press("enter", cx)).unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("settings-dialog").visible());
        assert!(window.try_find("topic-editor").is_none());
        assert_eq!(window.find("begin-add-topic").focused(), Some(true));
        assert_eq!(view.read(cx).subscription_topics, vec![subscription("#", 0)]);
        assert!(view.read(cx).saved_connections.connections.is_empty());
    })
    .unwrap();
}

#[gpui_kit::test]
fn removing_subscriptions_changes_only_the_draft_and_preserves_domain_ids(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 1200., 1600.);
    let saved = vec![subscription("#", 0), subscription("home/+", 1), subscription("workshop/#", 2)];
    view.update(cx, |view, cx| {
        view.saved_connections.connections[0].topics = saved.clone();
        view.active_config.as_mut().unwrap().topics = saved.clone();
        cx.notify();
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("settings", cx);
        window.click("edit-connection:0", cx);
        let old_top = window.find("subscription:home/+").bounds().top();
        window.click("remove-topic:#", cx);
        assert!(window.try_find("subscription:#").is_none());
        assert!(window.find("subscription:home/+").bounds().top() < old_top);
        assert_eq!(window.find("topic-filter:home/+").label(), Some("home/+"));
        assert_eq!(window.find("topic-qos:home/+").label(), Some("QoS 1"));
        assert_eq!(window.find("topic-qos:workshop/#").label(), Some("QoS 2"));
        assert_eq!(
            view.read(cx).subscription_topics,
            vec![subscription("home/+", 1), subscription("workshop/#", 2)]
        );
        window.click("remove-topic:home/+", cx);
        assert!(window.try_find("subscription:home/+").is_none());
        assert!(window.find("subscription:workshop/#").visible());
        assert_eq!(window.find("remove-topic:workshop/#").label(), Some("Remove topic workshop/#"));
        let view = view.read(cx);
        assert_eq!(view.subscription_topics, vec![subscription("workshop/#", 2)]);
        assert_eq!(view.saved_connections.connections[0].topics, saved);
        assert_eq!(view.active_config.as_ref().unwrap().topics, saved);
        assert!(view.status.is_connected());
        assert_eq!(view.topics.topics, 2);
    })
    .unwrap();
}

#[gpui_kit::test]
fn adding_a_duplicate_topic_filter_with_different_qos_keeps_the_editor_and_draft_unchanged(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 1200., 1600.);
    view.update(cx, |view, cx| {
        view.saved_connections.connections[0].topics = vec![subscription("#", 2)];
        cx.notify();
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("settings", cx);
        window.click("edit-connection:0", cx);
        window.click("begin-add-topic", cx);
        assert_eq!(window.find("topic-qos").value(), Some("0"));
        window.input("#", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| window.click("add-topic", cx)).unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find("topics-error").label(),
            Some("This topic filter is already in the list.")
        );
        assert_eq!(window.find("topic-filter").value(), Some("#"));
        assert_eq!(window.find("topic-filter").focused(), Some(true));
        assert!(window.find("topic-editor").visible());
        assert_eq!(window.find("topic-qos:#").label(), Some("QoS 2"));
        assert_eq!(view.read(cx).subscription_topics, vec![subscription("#", 2)]);
        assert_eq!(view.read(cx).saved_connections.connections[0].topics, vec![subscription("#", 2)]);
        window.click("cancel-topic", cx);
        assert!(window.try_find("topics-error").is_none());
        assert!(window.try_find("topic-editor").is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
fn saving_an_empty_subscription_list_expands_inline_validation_without_saving(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, true, 1200., 1600.);
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("name", cx);
        window.input("Home", cx);
        window.click("remove-topic:#", cx);
        assert!(view.read(cx).subscription_topics.is_empty());
        window.click("toggle-topics", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| window.click("save-connection", cx))
        .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("settings-dialog").visible());
        assert_eq!(window.find("subscription-topics").expanded(), Some(true));
        assert!(window.find("topic-editor").visible());
        assert_eq!(window.find("topics-error").label(), Some("Add at least one topic subscription."));
        assert_eq!(window.find("topic-filter").focused(), Some(true));
        let view = view.read(cx);
        assert!(matches!(view.field_error, Some((ConnectionField::Topics, _))));
        assert!(view.saved_connections.connections.is_empty());
        assert!(view.connection.is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
fn saving_with_a_pending_topic_editor_preserves_input_and_reports_inline_validation(cx: &mut TestAppContext) {
    for filter in ["", "pending/+/temperature"] {
        let (handle, view) = open(cx, false, 1200., 1600.);
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click("settings", cx);
            window.click("edit-connection:0", cx);
            window.click("begin-add-topic", cx);
            window.input(filter, cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| window.click("save-connection", cx))
            .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find("settings-dialog").visible());
            assert!(window.find("topic-editor").visible());
            let error = window.find("topics-error");
            assert_eq!(error.label(), Some("Add or cancel the topic before saving."));
            assert!(error.bounds().top() >= window.find("field:topic-filter").bounds().bottom());
            assert_eq!(window.find("topic-filter").value(), Some(filter));
            assert_eq!(window.find("topic-filter").focused(), Some(true));
            let view = view.read(cx);
            assert!(view.connection_form_open);
            assert_eq!(view.subscription_topics, vec![subscription("#", 0)]);
            assert_eq!(view.saved_connections.connections.len(), 1);
            assert_eq!(view.saved_connections.connections[0].topics, vec![subscription("#", 0)]);
            assert_eq!(view.active_config.as_ref().unwrap().topics, vec![subscription("#", 0)]);
        })
        .unwrap();
    }
}

#[gpui_kit::test]
fn topics_disclosure_stays_left_aligned_above_headerless_rows_with_qos_tags(cx: &mut TestAppContext) {
    for mode in [ThemeMode::Light, ThemeMode::Dark] {
        for font_size in [14., 20.] {
            for width in [760., 1200.] {
                let (handle, view) = open(cx, true, width, 1200.);
                cx.update(|cx| {
                    Theme::change(mode, None, cx);
                    Theme::update(cx, |theme| theme.font_size = px(font_size));
                });
                view.update(cx, |view, cx| {
                    view.subscription_topics = vec![subscription("#", 0), subscription("home/#", 1), subscription("$SYS/#", 2)];
                    cx.notify();
                });
                cx.update_window(handle, |_, window, cx| {
                    window.render_frame(cx);
                    for _ in 0..40 {
                        if window.find("remove-topic:$SYS/#").visible() {
                            break;
                        }
                        window.scroll("settings-dialog", gpui_kit::ScrollDelta::Lines(gpui_kit::point(0., -2.)), cx);
                    }
                    window.render_frame(cx);
                    let section = window.find("subscription-topics").bounds();
                    let trigger = window.find("toggle-topics").bounds();
                    let first_row = window.find("subscription:#").bounds();
                    let rem = window.rem_size();
                    assert!(
                        trigger.left() - section.left() <= rem,
                        "Topics disclosure must stay at the leading edge"
                    );
                    assert!(
                        trigger.right() < section.left() + section.size.width / 2.,
                        "Topics disclosure must not stretch across the header"
                    );
                    assert!(
                        first_row.top() - trigger.bottom() <= rem,
                        "Rows must follow the disclosure without a column-header band"
                    );
                    for (topic, qos) in [("#", 0), ("home/#", 1), ("$SYS/#", 2)] {
                        let tag = window.find(format!("topic-qos:{topic}"));
                        let filter = window.find(format!("topic-filter:{topic}"));
                        let remove = window.find(format!("remove-topic:{topic}"));
                        assert_eq!(tag.label(), Some(format!("QoS {qos}").as_str()));
                        assert!(tag.visible());
                        assert!(filter.bounds().right() < tag.bounds().left());
                        assert!(tag.bounds().right() < remove.bounds().left());
                    }
                    window.click("toggle-topics", cx);
                    assert_eq!(window.find("subscription-topics").expanded(), Some(false));
                    assert_eq!(window.find("toggle-topics").bounds().left(), trigger.left());
                    window.click("toggle-topics", cx);
                    assert_eq!(window.find("subscription-topics").expanded(), Some(true));
                    assert_eq!(window.find("toggle-topics").bounds().left(), trigger.left());
                })
                .unwrap();
            }
        }
    }
}

#[gpui_kit::test]
fn subscription_rows_and_editor_fit_minimum_size_in_both_themes_with_larger_text(cx: &mut TestAppContext) {
    for mode in [ThemeMode::Light, ThemeMode::Dark] {
        for font_size in [14., 20.] {
            let (handle, _) = open(cx, true, 760., 540.);
            cx.update(|cx| {
                Theme::change(mode, None, cx);
                Theme::update(cx, |theme| theme.font_size = px(font_size));
            });
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                for _ in 0..20 {
                    if window.find("remove-topic:#").visible() && window.find("begin-add-topic").visible() {
                        break;
                    }
                    window.scroll("settings-dialog", gpui_kit::ScrollDelta::Lines(gpui_kit::point(0., -2.)), cx);
                }
                let row = window.find("subscription:#").bounds();
                let section = window.find("subscription-topics").bounds();
                let filter = window.find("topic-filter:#").bounds();
                let qos = window.find("topic-qos:#").bounds();
                let remove = window.find("remove-topic:#");
                assert!(remove.visible());
                assert!(filter.left() >= section.left());
                assert!(filter.right() <= qos.left());
                assert!(qos.right() <= remove.bounds().left());
                assert!(remove.bounds().right() <= row.right());
                window.click("begin-add-topic", cx);
                for _ in 0..20 {
                    if window.find("topic-filter").visible() && window.find("add-topic").visible() {
                        break;
                    }
                    window.scroll("settings-dialog", gpui_kit::ScrollDelta::Lines(gpui_kit::point(0., -1.)), cx);
                }
                let editor = window.find("topic-editor").bounds();
                let input = window.find("field:topic-filter").bounds();
                let select = window.find("topic-qos");
                let cancel = window.find("cancel-topic");
                let add = window.find("add-topic");
                let dialog = window.find("settings-dialog").bounds();
                let ring = px(3.);
                assert!(window.find("topic-filter").visible());
                assert!(select.visible());
                assert!(cancel.visible());
                assert!(add.visible());
                assert!(input.left() - ring >= editor.left());
                assert!(input.right() + ring <= select.bounds().left());
                assert!(select.bounds().right() + ring <= editor.right());
                assert!(cancel.bounds().top() >= input.bottom());
                assert!(cancel.bounds().right() <= add.bounds().left());
                assert!(add.bounds().right() <= editor.right());
                assert!(editor.left() >= dialog.left());
                assert!(editor.right() <= dialog.right());
            })
            .unwrap();
        }
    }
}

#[gpui_kit::test]
fn connection_inputs_leave_room_for_their_focus_rings(cx: &mut TestAppContext) {
    for font_size in [14., 20.] {
        let (handle, _) = open(cx, true, 1200., 1200.);
        cx.update(|cx| Theme::update(cx, |theme| theme.font_size = px(font_size)));
        cx.update_window(handle, |_, window, cx| {
            for id in ["name", "host", "port", "client-id", "username", "password"] {
                window.render_frame(cx);
                window.click(id, cx);
                window.render_frame(cx);
                let field = if matches!(id, "host" | "port") {
                    window.find("broker-settings").bounds()
                } else {
                    window.find(format!("connection-field:{id}")).bounds()
                };
                let input = window.find(format!("field:{id}")).bounds();
                let ring = px(3.);
                assert!(input.left() - ring >= field.left(), "{id} focus ring is clipped on the left");
                assert!(input.right() + ring <= field.right(), "{id} focus ring is clipped on the right");
                assert!(input.top() - ring >= field.top(), "{id} focus ring is clipped on top");
                assert!(input.bottom() + ring <= field.bottom(), "{id} focus ring is clipped on the bottom");
                assert_eq!(window.find(id).focused(), Some(true));
            }
        })
        .unwrap();
    }
}

#[gpui_kit::test]
fn connection_form_actions_are_below_the_fields_and_aligned_right(cx: &mut TestAppContext) {
    for (width, height) in [(760., 540.), (1200., 1200.)] {
        let (handle, _) = open(cx, true, width, height);
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.scroll("name", gpui_kit::ScrollDelta::Lines(gpui_kit::point(0., -100.)), cx);
            let password = window.find("field:password").bounds();
            let save = window.find("save-connection");
            let connect = window.find("connect");
            assert!(save.visible());
            assert!(connect.visible());
            assert!(save.bounds().top() > password.bottom());
            assert_eq!(save.bounds().top(), connect.bounds().top());
            assert!(save.bounds().right() < connect.bounds().left());
            assert!((connect.bounds().right() - password.right()).abs() < px(1.));
        })
        .unwrap();
    }
}

#[gpui_kit::test]
fn broker_settings_align_protocol_host_and_port_across_themes_and_text_sizes(cx: &mut TestAppContext) {
    for mode in [ThemeMode::Light, ThemeMode::Dark] {
        for font_size in [16., 20.] {
            let (handle, _) = open(cx, true, 760., 800.);
            cx.update(|cx| {
                Theme::change(mode, None, cx);
                Theme::update(cx, |theme| theme.font_size = px(font_size));
            });
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                assert_eq!(window.find("broker-settings").label(), Some("Broker settings"));
                assert!(window.try_find("tls").is_none());
                let protocol = window.find("broker-protocol");
                let host = window.find("field:host").bounds();
                let port = window.find("field:port").bounds();
                assert_eq!(protocol.label(), Some("Protocol"));
                assert_eq!(protocol.value(), Some("mqtt://"));
                assert_eq!(protocol.bounds().top(), host.top());
                assert_eq!(host.top(), port.top());
                assert_eq!(protocol.bounds().size.height, host.size.height);
                assert_eq!(host.size.height, port.size.height);
                assert!(protocol.bounds().right() < host.left());
                assert!(host.right() < port.left());
                assert!((host.left() - protocol.bounds().right() - (port.left() - host.right())).abs() < px(1.));
                assert_eq!(protocol.bounds().left(), window.find("field:name").bounds().left());
                assert_eq!(port.right(), window.find("field:name").bounds().right());
                assert!(host.size.width > px(0.));
            })
            .unwrap();
        }
    }
}

#[gpui_kit::test]
fn editing_connections_restores_broker_protocol_and_preserves_custom_ports(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 1200., 1200.);
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("settings", cx);
    })
    .unwrap();
    cx.run_until_parked();
    for (tls, websocket, expected) in [(false, false, "mqtt://"), (true, false, "mqtts://"), (false, true, "ws://")] {
        view.update(cx, |view, cx| {
            let config = &mut view.saved_connections.connections[0];
            config.tls = tls;
            config.websocket = websocket;
            config.port = 9001;
            cx.notify();
        });
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click("edit-connection:0", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("broker-protocol").value(), Some(expected));
            assert_eq!(view.read(cx).protocol.read(cx).selected_value(), Some(&expected));
            assert_eq!(window.find("port").value(), Some("9001"));
            window.click("back-to-connections", cx);
        })
        .unwrap();
        cx.run_until_parked();
    }
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("new-connection", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("broker-protocol").value(), Some("mqtt://"));
        assert_eq!(window.find("port").value(), Some("1883"));
    })
    .unwrap();
}

#[gpui_kit::test]
fn broker_protocol_supports_keyboard_selection_default_ports_and_tab_order(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, true, 1200., 1200.);
    cx.update_window(handle, |_, window, _| window.activate_window()).unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("name", cx);
        window.press("tab", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("broker-protocol").focused(), Some(true));
        window.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| window.press("down", cx)).unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| window.press("enter", cx)).unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("broker-protocol").value(), Some("mqtts://"));
        assert_eq!(window.find("broker-protocol").expanded(), Some(false));
        assert_eq!(view.read(cx).protocol.read(cx).selected_value(), Some(&"mqtts://"));
        assert_eq!(window.find("port").value(), Some("8883"));
        window.press("tab", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("host").focused(), Some(true));
        window.press("tab", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("port").focused(), Some(true));
        window.input("9001", cx);
        window.click("broker-protocol", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| window.press("down", cx)).unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| window.press("enter", cx)).unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("broker-protocol").value(), Some("ws://"));
        assert_eq!(view.read(cx).protocol.read(cx).selected_value(), Some(&"ws://"));
        assert_eq!(window.find("port").value(), Some("9001"));
        window.click("broker-protocol", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| window.press("escape", cx)).unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("broker-protocol").focused(), Some(true));
        assert_eq!(window.find("broker-protocol").expanded(), Some(false));
        assert!(window.find("settings-dialog").visible());
        window.press("escape", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("settings-dialog").is_none());
        assert!(!view.read(cx).show_config);
    })
    .unwrap();
}

#[gpui_kit::test]
fn reopening_settings_restores_the_selected_connection(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 1200., 1600.);
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("settings", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("settings-dialog").visible());
        window.click("edit-connection:0", cx);
        window.render_frame(cx);
        window.click("port", cx);
        window.press("secondary-a", cx);
        window.input("9999", cx);
        window.click("remove-topic:#", cx);
        window.click("begin-add-topic", cx);
        window.input("unsaved/#", cx);
        window.click("add-topic", cx);
        assert_eq!(view.read(cx).subscription_topics, vec![subscription("unsaved/#", 0)]);
        window.click("close-settings", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("settings-dialog").is_none());
        window.click("settings", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("settings-dialog").visible());
        assert!(window.find("connections-overview").visible());
        window.click("edit-connection:0", cx);
        window.render_frame(cx);
        assert_eq!(window.find("name").value(), Some("Home"));
        assert_eq!(window.find("port").value(), Some("1883"));
        assert_eq!(view.read(cx).subscription_topics, vec![subscription("#", 0)]);
        assert_eq!(window.find("topic-qos:#").label(), Some("QoS 0"));
        assert!(window.try_find("subscription:unsaved/#").is_none());
        assert!(window.try_find("topic-editor").is_none());
        assert_eq!(view.read(cx).editing, Some(0));
    })
    .unwrap();
}

#[gpui_kit::test]
fn password_inspection_toggles_visibility_without_changing_the_value(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, true, 1200., 1600.);
    let password = " secret 密碼 ";
    cx.update_window(handle, |_, window, _| window.activate_window()).unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("password", cx);
        window.input(password, cx);
        assert_eq!(window.find("password").value(), None);
        assert_eq!(window.find("toggle-password").label(), Some("Show password"));
        let field = window.find("field:password").bounds();
        let toggle = window.find("toggle-password").bounds();
        assert!(toggle.left() >= field.left() && toggle.right() <= field.right());
        assert!(toggle.top() >= field.top() && toggle.bottom() <= field.bottom());

        window.click("toggle-password", cx);
        assert_eq!(window.find("password").value(), Some(password));
        assert_eq!(window.find("toggle-password").label(), Some("Hide password"));
        window.click("toggle-password", cx);
        assert_eq!(window.find("password").value(), None);
        assert_eq!(view.read(cx).password.read(cx).value().as_str(), password);

        window.click("password", cx);
        window.press("tab", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("toggle-password").focused(), Some(true));
        window.press("space", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("password").value(), Some(password));
        assert_eq!(window.find("toggle-password").label(), Some("Hide password"));
        assert_eq!(view.read(cx).password.read(cx).value().as_str(), password);
    })
    .unwrap();
}

#[gpui_kit::test]
fn opening_connection_forms_masks_previously_inspected_passwords(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 1200., 1600.);
    view.update(cx, |view, _| {
        view.saved_connections.connections[0].password = "home-secret".into();
        view.saved_connections.connections.push(ConnectionConfig {
            name: "Workshop".into(),
            password: "workshop-secret".into(),
            ..Default::default()
        });
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("settings", cx);
        window.click("edit-connection:0", cx);
        assert_eq!(window.find("password").value(), None);
        window.click("toggle-password", cx);
        assert_eq!(window.find("password").value(), Some("home-secret"));

        window.click("back-to-connections", cx);
        window.click("edit-connection:1", cx);
        assert_eq!(window.find("password").value(), None);
        assert_eq!(window.find("toggle-password").label(), Some("Show password"));
        window.click("toggle-password", cx);
        assert_eq!(window.find("password").value(), Some("workshop-secret"));

        window.click("close-settings", cx);
        window.click("settings", cx);
        window.click("edit-connection:1", cx);
        assert_eq!(window.find("password").value(), None);
        window.click("toggle-password", cx);
        window.click("back-to-connections", cx);
        window.click("new-connection", cx);
        assert_eq!(window.find("password").value(), None);
        assert_eq!(window.find("toggle-password").label(), Some("Show password"));
        assert_eq!(view.read(cx).password.read(cx).value().as_str(), "");
        assert_eq!(view.read(cx).saved_connections.connections[0].password, "home-secret");
        assert_eq!(view.read(cx).saved_connections.connections[1].password, "workshop-secret");
    })
    .unwrap();
}

#[gpui_kit::test]
fn tabbing_between_connection_inputs_selects_values_and_visits_subscription_controls(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, true, 1200., 1600.);
    cx.update_window(handle, |_, window, _| window.activate_window()).unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            for (input, value) in [
                (&view.name, "Home"),
                (&view.host, "localhost"),
                (&view.port, "1883"),
                (&view.client_id, "topq-test-client"),
                (&view.username, "reader"),
                (&view.password, "secret"),
            ] {
                input.update(cx, |input, cx| input.set_value(value, window, cx));
            }
        });
        window.render_frame(cx);
        window.click("name", cx);
    })
    .unwrap();
    for (key, id, replacement) in [
        ("tab", "broker-protocol", None),
        ("tab", "host", Some("broker.example")),
        ("tab", "port", Some("8883")),
        ("tab", "client-id", Some("topq-next-client")),
        ("tab", "username", Some("another-reader")),
        ("tab", "password", Some("new-password")),
        ("tab", "toggle-password", None),
        ("tab", "toggle-topics", None),
        ("tab", "begin-add-topic", None),
        ("tab", "remove-topic:#", None),
        ("shift-tab", "begin-add-topic", None),
        ("shift-tab", "toggle-topics", None),
        ("shift-tab", "toggle-password", None),
        ("shift-tab", "password", Some("replacement-password")),
        ("shift-tab", "username", Some("replacement-reader")),
        ("shift-tab", "client-id", Some("topq-previous-client")),
        ("shift-tab", "port", Some("1884")),
        ("shift-tab", "host", Some("other.example")),
        ("shift-tab", "broker-protocol", None),
        ("shift-tab", "name", Some("Workshop")),
    ] {
        cx.update_window(handle, |_, window, cx| window.press(key, cx)).unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find(id).focused(), Some(true), "expected {key} to focus {id}");
            if let Some(replacement) = replacement {
                window.input(replacement, cx);
            }
        })
        .unwrap();
        cx.run_until_parked();
        if let Some(replacement) = replacement {
            cx.update(|cx| {
                let view = view.read(cx);
                let input = match id {
                    "name" => &view.name,
                    "host" => &view.host,
                    "port" => &view.port,
                    "client-id" => &view.client_id,
                    "username" => &view.username,
                    "password" => &view.password,
                    _ => unreachable!(),
                };
                assert_eq!(input.read(cx).value().as_str(), replacement);
            });
        }
    }
}

#[gpui_kit::test]
fn clicking_a_connection_input_does_not_select_all(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, true, 1200., 900.);
    cx.update_window(handle, |_, window, _| window.activate_window()).unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.host.update(cx, |input, cx| input.set_value("broker.example", window, cx));
        });
        window.render_frame(cx);
        window.click("host", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| assert!(view.read(cx).host.read(cx).selected_range().is_empty()));
}

#[gpui_kit::test]
fn saving_requires_a_connection_name(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, true, 760., 540.);
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.scroll("name", gpui_kit::ScrollDelta::Lines(gpui_kit::point(0., -100.)), cx);
        window.click("connect", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("settings-dialog").visible());
        assert_eq!(window.find("name-error").label(), Some("Enter a connection name."));
        assert_eq!(window.find("name").focused(), Some(true));
        assert!(view.read(cx).saved_connections.connections.is_empty());
    })
    .unwrap();
}

#[gpui_kit::test]
fn enter_keeps_invalid_port_next_to_its_field_and_returns_focus(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, true, 760., 540.);
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("name", cx);
        window.input("Home", cx);
        window.press("tab", cx);
        window.press("tab", cx);
        window.press("tab", cx);
        assert_eq!(window.find("port").focused(), Some(true));
        window.press("secondary-a", cx);
        window.input("invalid", cx);
        window.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("settings-dialog").visible());
        let error = window.find("port-error");
        assert_eq!(error.label(), Some("Enter a port between 1 and 65535."));
        assert_eq!(window.find("port").focused(), Some(true));
        assert!(view.read(cx).connection.is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
fn enter_rejects_invalid_topic_filters_inline_and_returns_focus_without_adding(cx: &mut TestAppContext) {
    // Trimming the spaces would silently turn the last filter into a different, valid subscription.
    for filter in ["", "home/#/temperature", "home/tem+perature", " home/# "] {
        let expected_error = subscription(filter, 0).validate().unwrap_err();
        assert_eq!(expected_error.field(), ConnectionField::Topics);
        let (handle, view) = open(cx, true, 1200., 1600.);
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click("begin-add-topic", cx);
            window.input(filter, cx);
            window.press("enter", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find("settings-dialog").visible());
            assert!(window.find("topic-editor").visible());
            let error = window.find("topics-error");
            assert_eq!(error.label(), Some(expected_error.to_string().as_str()));
            assert_eq!(window.find("topic-filter").focused(), Some(true));
            assert_eq!(window.find("topic-filter").value(), Some(filter));
            assert!(error.bounds().top() >= window.find("field:topic-filter").bounds().bottom());
            assert_eq!(view.read(cx).subscription_topics, vec![subscription("#", 0)]);
            assert!(view.read(cx).connection.is_none());
            assert!(view.read(cx).saved_connections.connections.is_empty());
            window.press("secondary-a", cx);
            window.input("home/+/temperature", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("topics-error").is_none());
            assert!(view.read(cx).field_error.is_none());
            window.press("enter", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("topic-editor").is_none());
            assert_eq!(window.find("topic-filter:home/+/temperature").label(), Some("home/+/temperature"));
            assert_eq!(
                view.read(cx).subscription_topics,
                vec![subscription("#", 0), subscription("home/+/temperature", 0)]
            );
        })
        .unwrap();
    }
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
        window.click("name", cx);
        window.input("Home", cx);
        window.scroll("name", gpui_kit::ScrollDelta::Lines(gpui_kit::point(0., -100.)), cx);
        window.click("connect", cx);
        window.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        assert!(view.read(cx).connection.is_none());
        assert!(view.read(cx).show_config);
        assert!(view.read(cx).field_error.is_none());
    });
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
        assert_eq!(window.find("selected-topic").label(), Some("home/a"));
        assert_eq!(view.read(cx).payload.read(cx).value().as_str(), "42");
        window.click("copy-value", cx);
        assert_eq!(cx.read_from_clipboard().and_then(|item| item.text()), Some("42".into()));
    })
    .unwrap();
    view.update(cx, |view, cx| {
        view.topics.receive(message("home/0"), std::time::Instant::now());
        view.sync_tree(cx);
        cx.notify();
    });
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("selected-topic").label(), Some("home/a"));
        window.press("secondary-1", cx);
        window.press("end", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| assert_eq!(view.read(cx).selected.as_deref(), Some("home/b")));
}

#[gpui_kit::test]
fn topic_rows_use_compact_spacing_and_remain_comfortable_at_larger_text_sizes(cx: &mut TestAppContext) {
    for font_size in [16., 20.] {
        let (handle, _) = open(cx, false, 1200., 760.);
        cx.update(|cx| Theme::update(cx, |theme| theme.font_size = px(font_size)));
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let row = window.find("topic:home");
            let height = row.bounds().size.height;
            assert!(height >= px(24.), "topic row is too small: {height:?}");
            assert_eq!(height, window.rem_size() * 1.5, "topic row did not use compact spacing");
        })
        .unwrap();
    }
}

#[gpui_kit::test]
fn escape_closes_settings_from_the_overview_appearance_and_connection_controls(cx: &mut TestAppContext) {
    for target in [None, Some("appearance"), Some("name"), Some("password"), Some("toggle-password")] {
        let (handle, view) = open(cx, false, 1200., 1600.);
        let mut previous_focus = None;
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            previous_focus = window.focused(cx);
            window.click("settings", cx);
            match target {
                Some("appearance") => window.click("0-1", cx),
                Some(id) => {
                    window.click("edit-connection:0", cx);
                    window.click(id, cx);
                }
                None => {}
            }
            window.press("escape", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(
                window.try_find("settings-dialog").is_none(),
                "Escape should close settings from {target:?}"
            );
            assert!(!view.read(cx).show_config);
            assert!(previous_focus.as_ref().unwrap().is_focused(window));
        })
        .unwrap();
    }
}

#[gpui_kit::test]
fn escape_closes_settings_with_a_pending_topic_editor_and_restores_focus(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 1200., 1600.);
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.press("shift-tab", cx);
        assert_eq!(window.find("topic-search").focused(), Some(true));
        window.press("shift-tab", cx);
        assert_eq!(window.find("settings").focused(), Some(true));
        window.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("settings-dialog").visible());
        window.click("edit-connection:0", cx);
        window.render_frame(cx);
        window.click("port", cx);
        window.press("secondary-a", cx);
        window.input("9999", cx);
        window.click("begin-add-topic", cx);
        window.input("discarded/#", cx);
        window.press("escape", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("settings-dialog").is_none());
        assert!(window.try_find("topic-editor").is_none());
        assert!(!view.read(cx).topic_editor_open);
        assert_eq!(view.read(cx).subscription_topics, vec![subscription("#", 0)]);
        assert_eq!(view.read(cx).saved_connections.connections[0].topics, vec![subscription("#", 0)]);
        assert_eq!(view.read(cx).saved_connections.connections[0].port, 1883);
        assert!(!view.read(cx).show_config);
        assert_eq!(window.find("settings").focused(), Some(true));
        window.click("settings", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("settings-dialog").visible());
        window.click("edit-connection:0", cx);
        window.render_frame(cx);
        assert_eq!(window.find("port").value(), Some("1883"));
        assert_eq!(view.read(cx).subscription_topics, vec![subscription("#", 0)]);
        assert!(window.try_find("subscription:discarded/#").is_none());
        assert!(window.try_find("topic-editor").is_none());
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
                assert!(window.try_find("status").is_none());
            })
            .unwrap();
        }
    }
}

fn receive_payload(view: &mut Explorer, topic: &str, payload: &str, window: &mut Window, cx: &mut gpui_kit::Context<Explorer>) {
    let mut update = message(topic);
    update.payload = Bytes::copy_from_slice(payload.as_bytes());
    view.topics.receive(update, std::time::Instant::now());
    view.refresh_details(window, cx);
    cx.notify();
}

#[gpui_kit::test]
fn edit_value_prefills_publish_draft_and_focuses_the_payload_end(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 900., 640.);
    cx.update_window(handle, |_, window, _| window.activate_window()).unwrap();
    for (open, raw, expected) in [
        (false, "first line\n温度 🌡", "first line\n温度 🌡"),
        (true, "{\"value\":\"é\"}", "{\n  \"value\": \"é\"\n}"),
    ] {
        cx.update_window(handle, |_, window, cx| {
            view.update(cx, |view, cx| {
                receive_payload(view, "home/a", raw, window, cx);
                view.select_topic("home/a", window, cx);
                view.publish_open = open;
                view.publish_topic.update(cx, |input, cx| input.set_value("old/topic", window, cx));
                view.publish_payload
                    .update(cx, |editor, cx| editor.set_value("old draft", window, cx));
                view.publish_retain = true;
                view.publish_qos
                    .update(cx, |select, cx| select.set_selected_value(&"1", window, cx));
                view.publish_feedback = Some(Ok("Previous publish".into()));
                cx.notify();
            });
            window.render_frame(cx);
            assert_eq!(window.find("edit-value").label(), Some("Publish new value"));
            window.click("edit-value", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find("publish-form").visible());
            assert_eq!(window.find("publish-topic").value(), Some("home/a"));
            assert_publish_payload_detection(window, cx, &view, expected, None, expected.len());
            assert!(view.read(cx).publish_retain);
            assert_eq!(view.read(cx).publish_qos.read(cx).selected_value(), Some(&"1"));
            assert!(crate::config::load_publish_layout().unwrap().open);
            window.input("!", cx);
            let editor = view.read(cx).publish_payload.read(cx);
            assert_eq!(editor.value().as_str(), format!("{expected}!"));
            assert_eq!(editor.selected_range(), expected.len() + 1..expected.len() + 1);
            assert_eq!(
                view.read(cx).topics.nodes["home/a"].value.as_ref().unwrap().payload.as_ref(),
                raw.as_bytes()
            );
        })
        .unwrap();
        cx.run_until_parked();
    }
}

#[gpui_kit::test]
fn edit_value_leaves_the_payload_empty_for_images_and_binary_values(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 900., 640.);
    cx.update_window(handle, |_, window, _| window.activate_window()).unwrap();
    for (payload, content_type, format) in [
        (&[0xff, 0x00][..], None, "Binary · hex"),
        (&[0x89, b'P', b'N', b'G'][..], Some("image/png"), "Image"),
        (PUBLISH_FILE_SVG, Some("image/svg+xml"), "Image"),
        (b"unsupported image".as_slice(), Some("image/vnd.example.custom"), "Image"),
    ] {
        cx.update_window(handle, |_, window, cx| {
            view.update(cx, |view, cx| {
                let mut update = message("home/a");
                update.payload = Bytes::copy_from_slice(payload);
                update.properties = crate::topics::MessageProperties::from_publish(content_type.map(|content_type| {
                    rumqttc::v5::mqttbytes::v5::PublishProperties {
                        content_type: Some(content_type.to_owned()),
                        ..Default::default()
                    }
                }));
                view.topics.receive(update, std::time::Instant::now());
                view.select_topic("home/a", window, cx);
                view.publish_payload
                    .update(cx, |editor, cx| editor.set_value("old draft", window, cx));
                cx.notify();
            });
            window.render_frame(cx);
            assert_eq!(view.read(cx).payload_format, format);
            window.click("edit-value", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find("publish-form").visible());
            assert_eq!(window.find("publish-topic").value(), Some("home/a"));
            assert_publish_payload_detection(window, cx, &view, "", None, 0);
            assert_eq!(
                view.read(cx).topics.nodes["home/a"].value.as_ref().unwrap().payload.as_ref(),
                payload
            );
        })
        .unwrap();
    }
}

#[gpui_kit::test]
fn edit_value_supports_keyboard_activation_and_is_absent_without_a_topic_value(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 900., 640.);
    cx.update_window(handle, |_, window, cx| {
        window.activate_window();
        window.render_frame(cx);
        assert!(window.try_find("edit-value").is_none());
        view.update(cx, |view, cx| view.select_topic("home", window, cx));
        window.render_frame(cx);
        assert!(window.try_find("edit-value").is_none());
        view.update(cx, |view, cx| view.select_topic("home/b", window, cx));
        window.render_frame(cx);
        window.click("payload", cx);
    })
    .unwrap();
    cx.run_until_parked();
    for _ in 0..2 {
        cx.update_window(handle, |_, window, cx| window.press("shift-tab", cx)).unwrap();
        cx.run_until_parked();
    }
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("edit-value").focused(), Some(true));
        window.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("publish-topic").value(), Some("home/b"));
        let raw = std::str::from_utf8(&view.read(cx).topics.nodes["home/b"].value.as_ref().unwrap().payload)
            .unwrap()
            .to_owned();
        assert_publish_payload_detection(window, cx, &view, &raw, None, raw.len());
    })
    .unwrap();
}

#[gpui_kit::test]
fn live_payload_updates_preserve_scrolled_viewport_and_focus(cx: &mut TestAppContext) {
    for mode in [ThemeMode::Light, ThemeMode::Dark] {
        let (handle, view) = open(cx, false, 1200., 760.);
        let mut json = serde_json::json!({"items": (0..200).collect::<Vec<_>>(), "reading": 1});
        cx.update(|cx| Theme::change(mode, None, cx));
        cx.update_window(handle, |_, window, cx| {
            view.update(cx, |view, cx| {
                receive_payload(view, "home/a", &json.to_string(), window, cx);
                view.select_topic("home/a", window, cx);
            });
            window.render_frame(cx);
            window.simulate_next_frame(cx);
            window.click("payload", cx);
            window.scroll("payload", gpui_kit::ScrollDelta::Lines(gpui_kit::point(0., -30.)), cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let offset = view.read(cx).payload.read(cx).scroll_offset();
            assert!(offset.y < px(0.), "wheel input must scroll the large payload");
            let rows = view.read(cx).payload.read(cx).visible_row_range();
            json["reading"] = 2.into();
            view.update(cx, |view, cx| receive_payload(view, "home/a", &json.to_string(), window, cx));
            json["reading"] = 3.into();
            view.update(cx, |view, cx| receive_payload(view, "home/a", &json.to_string(), window, cx));
            window.render_frame(cx);
            let editor = view.read(cx).payload.read(cx);
            assert_eq!(editor.scroll_offset(), offset);
            assert_eq!(editor.visible_row_range(), rows);
            assert!(editor.focus_handle(cx).is_focused(window));
            assert!(editor.value().as_str().ends_with("\"reading\": 3\n}"));
            window.simulate_next_frame(cx);
            window.scroll("payload", gpui_kit::ScrollDelta::Lines(gpui_kit::point(0., -10.)), cx);
            let new_offset = view.read(cx).payload.read(cx).scroll_offset();
            assert!(new_offset.y < offset.y);
            json["reading"] = 4.into();
            view.update(cx, |view, cx| receive_payload(view, "home/a", &json.to_string(), window, cx));
            window.render_frame(cx);
            assert_eq!(view.read(cx).payload.read(cx).scroll_offset(), new_offset);
        })
        .unwrap();
    }
}

#[gpui_kit::test]
fn identical_payload_updates_do_not_reset_selection_or_restart_highlights(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 1200., 760.);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            receive_payload(view, "home/a", r#"{"reading":1}"#, window, cx);
            view.select_topic("home/a", window, cx);
            receive_payload(view, "home/a", r#"{"reading":2}"#, window, cx);
            view.payload_highlight
                .refresh(std::time::Instant::now() + crate::topics::FLASH_DURATION, cx);
        });
        window.render_frame(cx);
        window.click("payload", cx);
        window.press("secondary-a", cx);
        let selection = view.read(cx).payload.read(cx).selected_range();
        assert!(!selection.is_empty());
        view.update(cx, |view, cx| receive_payload(view, "home/a", r#"{ "reading" : 2 }"#, window, cx));
        window.render_frame(cx);
        assert_eq!(view.read(cx).payload.read(cx).selected_range(), selection);
        assert!(view.read(cx).payload_highlight.ranges(cx).is_empty());
    })
    .unwrap();
}

#[gpui_kit::test]
fn json_payload_highlights_only_changed_and_added_content_then_expires(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 1200., 760.);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            receive_payload(view, "home/a", r#"{"device":{"reading":1,"stable":true},"removed":0}"#, window, cx);
            view.select_topic("home/a", window, cx);
        });
        assert!(
            view.read(cx).payload_highlight.ranges(cx).is_empty(),
            "selecting a topic is not an update"
        );
        view.update(cx, |view, cx| {
            receive_payload(view, "home/a", r#"{"added":"é","device":{"reading":2,"stable":true}}"#, window, cx);
        });
        window.render_frame(cx);
        let text = view.read(cx).payload.read(cx).value();
        let ranges = view.read(cx).payload_highlight.ranges(cx);
        let highlighted: Vec<_> = ranges.iter().map(|range| &text[range.clone()]).collect();
        assert_eq!(highlighted, ["\"added\": \"é\"", "2"]);
        view.update(cx, |view, cx| {
            view.payload_highlight
                .refresh(std::time::Instant::now() + crate::topics::FLASH_DURATION, cx);
        });
        window.render_frame(cx);
        assert!(view.read(cx).payload_highlight.ranges(cx).is_empty());
    })
    .unwrap();
}

#[gpui_kit::test]
fn switching_topics_resets_payload_scroll_and_clears_previous_highlights(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 1200., 760.);
    let json = serde_json::json!({"items": (0..200).collect::<Vec<_>>(), "reading": 1});
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            receive_payload(view, "home/a", &json.to_string(), window, cx);
            view.select_topic("home/a", window, cx);
        });
        window.render_frame(cx);
        window.simulate_next_frame(cx);
        window.scroll("payload", gpui_kit::ScrollDelta::Lines(gpui_kit::point(0., -30.)), cx);
        assert!(view.read(cx).payload.read(cx).scroll_offset().y < px(0.));
        let mut updated = json.clone();
        updated["reading"] = 2.into();
        view.update(cx, |view, cx| receive_payload(view, "home/a", &updated.to_string(), window, cx));
        assert!(!view.read(cx).payload_highlight.ranges(cx).is_empty());
        view.update(cx, |view, cx| {
            receive_payload(view, "home/b", &updated.to_string(), window, cx);
            view.select_topic("home/b", window, cx);
        });
        window.render_frame(cx);
        assert_eq!(view.read(cx).payload.read(cx).scroll_offset(), gpui_kit::point(px(0.), px(0.)));
        assert!(view.read(cx).payload_highlight.ranges(cx).is_empty());
    })
    .unwrap();
}

#[gpui_kit::test]
fn shorter_payload_clamps_scroll_and_plaintext_updates_keep_the_viewport(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 1200., 760.);
    let text = "line of plaintext\n".repeat(200);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            receive_payload(view, "home/a", &text, window, cx);
            view.select_topic("home/a", window, cx);
        });
        window.render_frame(cx);
        window.simulate_next_frame(cx);
        window.scroll("payload", gpui_kit::ScrollDelta::Lines(gpui_kit::point(0., -30.)), cx);
        let offset = view.read(cx).payload.read(cx).scroll_offset();
        assert!(offset.y < px(0.));
        view.update(cx, |view, cx| {
            receive_payload(view, "home/a", &format!("{text}new line"), window, cx)
        });
        window.render_frame(cx);
        assert_eq!(view.read(cx).payload.read(cx).scroll_offset(), offset);
        assert!(view.read(cx).payload_highlight.ranges(cx).is_empty());
        view.update(cx, |view, cx| receive_payload(view, "home/a", "short", window, cx));
        window.render_frame(cx);
        assert_eq!(view.read(cx).payload.read(cx).scroll_offset().y, px(0.));
    })
    .unwrap();
}

#[gpui_kit::test]
fn reduced_motion_clears_payload_highlights_and_keeps_updates_visible(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 1200., 760.);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            receive_payload(view, "home/a", r#"{"reading":1}"#, window, cx);
            view.select_topic("home/a", window, cx);
            receive_payload(view, "home/a", r#"{"reading":2}"#, window, cx);
        });
        assert!(!view.read(cx).payload_highlight.ranges(cx).is_empty());
        cx.set_reduce_motion(true);
        view.update(cx, |view, cx| view.refresh_animation(window, cx));
        assert!(view.read(cx).payload_highlight.ranges(cx).is_empty());
        view.update(cx, |view, cx| receive_payload(view, "home/a", r#"{"reading":3}"#, window, cx));
        window.render_frame(cx);
        assert!(view.read(cx).payload_highlight.ranges(cx).is_empty());
        assert_eq!(view.read(cx).payload.read(cx).value().as_str(), "{\n  \"reading\": 3\n}");
    })
    .unwrap();
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
        view.topics
            .receive(message("home/a"), std::time::Instant::now() - std::time::Duration::from_millis(100));
        cx.notify();
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("topic:home").visible());
        assert!(window.try_find("update:home").is_none());
        let explorer = view.read(cx);
        let rows = explorer.topic_rows.borrow();
        let mut row = rows.get("home").unwrap().read(cx).item(explorer, cx);
        assert_ne!(row.style().text.color, Some(cx.theme().foreground));
    })
    .unwrap();
    cx.update(|cx| cx.set_reduce_motion(true));
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let explorer = view.read(cx);
        let rows = explorer.topic_rows.borrow();
        let mut row = rows.get("home").unwrap().read(cx).item(explorer, cx);
        assert_eq!(row.style().text.color, Some(cx.theme().foreground));
        assert_eq!(window.find("selected-topic").label(), Some("home"));
    })
    .unwrap();
}

#[gpui_kit::test]
fn long_topic_and_payload_remain_usable_at_minimum_size_with_large_text(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 760., 540.);
    let topic = format!("home/{}", "temperature_測定値_".repeat(20));
    cx.update(|cx| Theme::update(cx, |theme| theme.font_size = px(20.)));
    view.update(cx, |view, cx| {
        view.topics.receive(message(&topic), std::time::Instant::now());
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
        assert_eq!(window.find("selected-topic").label(), Some(topic.as_str()));
        let payload = window.find("payload");
        assert!(payload.visible());
        assert!(payload.bounds().size.height > px(0.));
        assert!(payload.bounds().bottom() <= window.find("details-pane").bounds().bottom());
        assert_eq!(window.find("copy-topic").label(), Some("Copy topic name"));
        assert_eq!(window.find("copy-value").label(), Some("Copy latest value"));
    })
    .unwrap();
}

#[gpui_kit::test]
fn selected_topic_breadcrumbs_fit_in_the_header_at_different_font_sizes(cx: &mut TestAppContext) {
    for font_size in [14., 16., 20., 24.] {
        // The panes require at least 35rem combined, plus the window border.
        let width = 760_f32.max(font_size * 35. + 4.);
        let (handle, view) = open(cx, false, width, 540.);
        cx.update(|cx| Theme::update(cx, |theme| theme.font_size = px(font_size)));
        cx.update_window(handle, |_, window, cx| {
            view.update(cx, |view, cx| {
                view.topics.receive(message("energy/solar/result"), std::time::Instant::now());
                view.select_topic("energy/solar/result", window, cx);
            });
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let header = window.find("details-heading").bounds();
            let breadcrumb = window.find("selected-topic");
            assert_eq!(breadcrumb.label(), Some("energy/solar/result"));
            assert_eq!(breadcrumb.value(), None);
            for index in 0..3 {
                let segment = window.find(format!("topic-segment:{index}"));
                assert!(segment.visible());
                assert!(segment.bounds().top() >= header.top());
                assert!(segment.bounds().bottom() <= header.bottom());
            }
            let delete = window.find("delete-topic");
            let copy = window.find("copy-topic");
            assert!(delete.visible());
            assert_eq!(delete.label(), Some("Delete topic and subtopics"));
            assert!(delete.bounds().right() <= copy.bounds().left());
            assert!(
                copy.visible(),
                "copy button is clipped at font {font_size}: header {header:?}, breadcrumbs {:?}, copy {:?}",
                breadcrumb.bounds(),
                copy.bounds()
            );
            assert!(breadcrumb.bounds().right() <= delete.bounds().left());
            window.click("copy-topic", cx);
            assert_eq!(
                cx.read_from_clipboard().and_then(|item| item.text()),
                Some("energy/solar/result".into())
            );
        })
        .unwrap();
    }
}

#[gpui_kit::test]
fn topic_breadcrumbs_navigate_to_their_full_prefix_and_update_the_tree_and_payload(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 1200., 760.);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.topics.receive(message("energy/solar/result"), std::time::Instant::now());
            view.select_topic("energy/solar/result", window, cx);
        });
    })
    .unwrap();
    cx.run_until_parked();
    for (index, path, payload) in [(2, "energy/solar/result", "42"), (1, "energy/solar", ""), (0, "energy", "")] {
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(
                window.find(format!("topic-segment:{index}")).label(),
                Some(format!("Navigate to topic {path}").as_str())
            );
            window.click(format!("topic-segment:{index}"), cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let view = view.read(cx);
            assert_eq!(view.selected.as_deref(), Some(path));
            assert_eq!(view.tree_state.read(cx).selected_item().unwrap().id.as_str(), path);
            assert_eq!(view.payload.read(cx).value().as_str(), payload);
            assert_eq!(window.find("selected-topic").label(), Some(path));
            window.click("copy-topic", cx);
            assert_eq!(cx.read_from_clipboard().and_then(|item| item.text()), Some(path.to_owned()));
        })
        .unwrap();
    }
}

#[gpui_kit::test]
fn topic_breadcrumbs_preserve_empty_levels_and_unicode_prefixes(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 1200., 760.);
    let topic = "/energy//測定値/";
    for (index, prefix) in ["", "/energy", "/energy/", "/energy//測定値", topic].into_iter().enumerate() {
        cx.update_window(handle, |_, window, cx| {
            view.update(cx, |view, cx| {
                view.topics.receive(message(topic), std::time::Instant::now());
                view.select_topic(topic, window, cx);
            });
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click(format!("topic-segment:{index}"), cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update(|cx| {
            let view = view.read(cx);
            assert_eq!(view.selected.as_deref(), Some(prefix));
            assert_eq!(view.tree_state.read(cx).selected_item().unwrap().id.as_str(), prefix);
        });
    }
}

fn click_topic_deletion_button(cx: &mut TestAppContext, handle: AnyWindowHandle, button: &'static str) {
    cx.update(|cx| cx.set_reduce_motion(true));
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.within("dialog").click(button, cx);
    })
    .unwrap();
    cx.run_until_parked();
}

#[gpui_kit::test]
fn topic_deletion_requires_confirmation_and_broker_updates_remove_it_in_both_clients(cx: &mut TestAppContext) {
    // Broker events wake the GPUI executor from the real MQTT worker thread.
    cx.executor().allow_parking();

    use std::{
        io::{Read, Write},
        net::{TcpListener, TcpStream},
        time::Duration,
    };

    fn packet(stream: &mut TcpStream) -> (u8, Vec<u8>) {
        let mut header = [0; 2];
        stream.read_exact(&mut header).unwrap();
        assert!(header[1] < 128, "test broker expects short packets");
        let mut body = vec![0; usize::from(header[1])];
        stream.read_exact(&mut body).unwrap();
        (header[0], body)
    }

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (sent, received) = std::sync::mpsc::channel();
    let (recreate, recreation_requested) = std::sync::mpsc::channel();
    let broker = std::thread::spawn(move || {
        let handshake = || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let (header, connect) = packet(&mut stream);
            assert_eq!(header, 0x10);
            assert_eq!(&connect[..7], b"\x00\x04MQTT\x05", "must connect using MQTT 5");
            stream.write_all(&[0x20, 3, 0, 0, 0]).unwrap();
            let (header, subscribe) = packet(&mut stream);
            assert_eq!(header, 0x82);
            assert_ne!(u16::from_be_bytes([subscribe[0], subscribe[1]]), 0);
            assert_eq!(subscribe[2], 0, "SUBSCRIBE properties must be empty");
            assert_eq!(&subscribe[3..], b"\x00\x01#\x00");
            stream.write_all(&[0x90, 4, subscribe[0], subscribe[1], 0, 0]).unwrap();
            stream
        };
        let mut stream = handshake();
        let mut observer = handshake();
        let mut topics = Vec::new();
        for _ in 0..3 {
            let (header, body) = packet(&mut stream);
            assert_eq!(header, 0x31, "must publish with QoS 0 and retain set");
            let length = usize::from(u16::from_be_bytes([body[0], body[1]]));
            assert_eq!(body[2 + length], 0, "PUBLISH properties must be empty");
            assert_eq!(body.len(), length + 3, "deletion payload must be empty");
            topics.push(String::from_utf8(body[2..2 + length].to_vec()).unwrap());
            // Existing subscriptions receive the empty publish with retain unset.
            for subscriber in [&mut stream, &mut observer] {
                subscriber.write_all(&[0x30, body.len() as u8]).unwrap();
                subscriber.write_all(&body).unwrap();
            }
        }
        sent.send(topics).unwrap();
        recreation_requested.recv_timeout(Duration::from_secs(5)).unwrap();
        let body = b"\x00\x06home/a\x00live";
        for subscriber in [&mut stream, &mut observer] {
            subscriber.write_all(&[0x30, body.len() as u8]).unwrap();
            subscriber.write_all(body).unwrap();
        }
        assert_eq!(stream.read(&mut [0]).unwrap(), 0);
        assert_eq!(observer.read(&mut [0]).unwrap(), 0);
    });
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let mut connections = Vec::new();
    for _ in 0..2 {
        let mut connection = crate::mqtt::connect(ConnectionConfig {
            host: "127.0.0.1".into(),
            port,
            ..Default::default()
        })
        .unwrap();
        let mut events = connection.take_events().unwrap();
        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    match events.recv().await.unwrap() {
                        crate::mqtt::BrokerEvent::Connected => break,
                        crate::mqtt::BrokerEvent::Status(error) => panic!("test broker failed: {error}"),
                        _ => {}
                    }
                }
            })
            .await
            .unwrap();
        });
        connections.push((connection, events));
    }
    let (connection, events) = connections.pop().unwrap();
    let (observer_handle, observer) = open(cx, false, 1200., 760.);
    cx.update_window(observer_handle, |_, window, cx| {
        observer.update(cx, |view, cx| {
            view.connection = Some(connection);
            view.listen_for_events(events, window, cx);
            view.topics.receive(message("outside/a"), std::time::Instant::now());
            view.expanded.insert("outside".into());
            view.select_topic("home/a", window, cx);
        });
    })
    .unwrap();
    let (connection, events) = connections.pop().unwrap();
    let (handle, view) = open(cx, false, 1200., 760.);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.connection = Some(connection);
            view.listen_for_events(events, window, cx);
            view.topics.receive(message("outside/a"), std::time::Instant::now());
            view.expanded.insert("outside".into());
            view.select_topic("home", window, cx);
        });
        window.render_frame(cx);
        window.click("delete-topic", cx);
    })
    .unwrap();
    assert!(
        !cx.has_pending_prompt(),
        "confirmation should use a GPUI Kit dialog, not a platform prompt"
    );
    assert!(received.try_recv().is_err());
    click_topic_deletion_button(cx, handle, "cancel");
    assert!(received.try_recv().is_err());
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("delete-topic", cx);
    })
    .unwrap();
    click_topic_deletion_button(cx, handle, "ok");
    assert_eq!(received.recv_timeout(Duration::from_secs(5)).unwrap(), ["home", "home/a", "home/b"]);
    let clients = [(handle, view.clone()), (observer_handle, observer.clone())];
    for recreated in [false, true] {
        if recreated {
            recreate.send(()).unwrap();
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            cx.run_until_parked();
            let mut ready = true;
            for (handle, view) in &clients {
                ready &= cx
                    .update_window(*handle, |_, window, cx| {
                        window.render_frame(cx);
                        view.read(cx).topics.nodes.contains_key("home") == recreated
                    })
                    .unwrap();
            }
            if ready {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "both clients must process the broker update");
            std::thread::sleep(Duration::from_millis(1));
        }
        cx.update(|cx| {
            for (_, view) in &clients {
                let view = view.read(cx);
                assert_eq!(view.topics.topics, if recreated { 2 } else { 1 });
                assert_eq!(view.topics.messages, if recreated { 2 } else { 1 });
                assert_eq!(view.tree_state.read(cx).index_of(&"home".into()).is_some(), recreated);
                assert!(view.selected.is_none());
                assert!(view.payload.read(cx).value().is_empty());
                assert!(view.expanded.contains("outside"));
                assert!(!view.expanded.contains("home"));
                assert!(view.topics.nodes["outside/a"].value.is_some());
                assert!(view.error.is_none());
                if recreated {
                    assert_eq!(view.topics.nodes["home/a"].value.as_ref().unwrap().payload.as_ref(), b"live");
                }
            }
        });
    }
    view.update(cx, |view, _| view.connection = None);
    observer.update(cx, |view, _| view.connection = None);
    broker.join().unwrap();
}

#[gpui_kit::test]
fn canceling_topic_deletion_preserves_topics_and_does_not_publish(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 1200., 760.);
    let sends = std::rc::Rc::new(std::cell::Cell::new(0));
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.select_topic("home", window, cx);
            let sends = sends.clone();
            view.show_confirm_topic_deletion_dialog("home".into(), window, cx, move |_, _| {
                sends.set(sends.get() + 1);
                Ok(())
            });
        });
    })
    .unwrap();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.has_active_dialog(cx));
        assert_eq!(
            window.find("delete-topic-scope").label(),
            Some("Delete home and 2 known child topics.")
        );
        let warning = window.find("delete-topic-warning");
        assert_eq!(
            warning.label(),
            Some("Deletes retained messages on the broker. This cannot be undone and may affect other subscribers.")
        );
        assert!(window.try_find("delete-topic-note").is_none());
        assert_eq!(sends.get(), 0);
    })
    .unwrap();
    click_topic_deletion_button(cx, handle, "cancel");
    cx.update_window(handle, |_, window, cx| assert!(!window.has_active_dialog(cx)))
        .unwrap();
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(sends.get(), 0);
        assert_eq!(view.topics.topics, 2);
        assert_eq!(view.selected.as_deref(), Some("home"));
        assert!(view.error.is_none());
    });
}

#[gpui_kit::test]
fn topic_deletion_dialog_escape_cancels_and_restores_focus(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 760., 540.);
    let mut previous_focus = None;
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.select_topic("home", window, cx);
            previous_focus = window.focused(cx);
            view.show_confirm_topic_deletion_dialog("home".into(), window, cx, |_, _| panic!("Escape must not publish"));
        });
        window.render_frame(cx);
        assert!(window.has_active_dialog(cx));
        assert!(!previous_focus.as_ref().unwrap().is_focused(window));
        window.press("escape", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(!window.has_active_dialog(cx));
        assert!(previous_focus.as_ref().unwrap().is_focused(window));
        assert!(view.read(cx).error.is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
fn topic_deletion_dialog_enter_confirms_only_once(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 760., 540.);
    let sends = std::rc::Rc::new(std::cell::Cell::new(0));
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.select_topic("home", window, cx);
            let sends = sends.clone();
            view.show_confirm_topic_deletion_dialog("home".into(), window, cx, move |_, _| {
                sends.set(sends.get() + 1);
                Ok(())
            });
        });
        window.render_frame(cx);
        window.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(!window.has_active_dialog(cx));
        window.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(sends.get(), 1);
}

#[gpui_kit::test]
fn topic_deletion_dialog_wraps_content_and_keeps_actions_visible_across_themes_and_zoom(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_reduce_motion(true));
    for mode in [ThemeMode::Light, ThemeMode::Dark] {
        for (width, height, font_size) in [(760., 540., 16.), (760., 540., 20.), (480., 400., 20.)] {
            let (handle, view) = open(cx, false, width, height);
            cx.update(|cx| {
                Theme::change(mode, None, cx);
                Theme::update(cx, |theme| theme.font_size = px(font_size));
            });
            cx.update_window(handle, |_, window, cx| {
                view.update(cx, |view, cx| {
                    let path = "homeassistant/binary_sensor/very_long_device_identifier/living_room/occupancy/state";
                    view.topics.receive(message(path), std::time::Instant::now());
                    view.select_topic(path, window, cx);
                    view.show_confirm_topic_deletion_dialog(path.into(), window, cx, |_, _| panic!("layout test must not publish"));
                });
                window.render_frame(cx);
                let surface = window.within("dialog").find(0usize).bounds();
                let scope = window.find("delete-topic-scope");
                let marker = window.find("delete-topic-marker");
                let warning = window.find("delete-topic-warning");

                let cancel = window.within("dialog").find("cancel");
                let delete = window.within("dialog").find("ok");
                assert!(scope.visible());
                assert!(marker.visible());
                assert!(warning.visible());

                assert_eq!(scope.bounds().left(), marker.bounds().left(), "marker should align under the title");
                assert!(warning.bounds().size.height > window.rem_size() * 2., "warning should wrap");
                assert!(warning.bounds().left() > marker.bounds().left());
                assert!(warning.bounds().right() < marker.bounds().right());
                assert!(warning.bounds().top() > marker.bounds().top());
                assert!(warning.bounds().bottom() < marker.bounds().bottom());
                assert!(marker.bounds().bottom() <= cancel.bounds().top());
                assert!(marker.bounds().right() <= surface.right());
                for content in [scope.bounds(), cancel.bounds(), delete.bounds()] {
                    assert!(content.left() >= surface.left());
                    assert!(content.right() <= surface.right());
                    assert!(content.bottom() <= surface.bottom());
                }
                assert!(surface.left() >= px(0.) && surface.right() <= px(width));
                assert!(surface.top() >= px(0.) && surface.bottom() <= px(height));
                assert!(cancel.visible() && delete.visible());
                assert_eq!(cancel.label(), Some("Cancel"));
                assert_eq!(delete.label(), Some("Delete"));
                assert_eq!(cancel.bounds().top(), delete.bounds().top());
                assert!(cancel.bounds().right() <= delete.bounds().left());

                window.press("escape", cx);
            })
            .unwrap();
            cx.run_until_parked();
        }
    }
}

#[gpui_kit::test]
fn topic_deletion_scope_renders_unicode_and_empty_topic_levels(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_reduce_motion(true));
    for (path, message_topic, expected) in [
        ("home/測定値", "home/測定値", "Delete home/測定値 and 0 known child topics."),
        ("", "/測定値", "Delete the 1 known topics under the empty topic level."),
    ] {
        let (handle, view) = open(cx, false, 760., 540.);
        cx.update_window(handle, |_, window, cx| {
            view.update(cx, |view, cx| {
                view.topics.receive(message(message_topic), std::time::Instant::now());
                view.select_topic(path, window, cx);
                view.show_confirm_topic_deletion_dialog(path.into(), window, cx, |_, _| panic!("rendering must not publish"));
            });
            window.render_frame(cx);
            let scope = window.find("delete-topic-scope");
            assert!(scope.visible());
            assert_eq!(scope.label(), Some(expected));
            window.press("escape", cx);
        })
        .unwrap();
        cx.run_until_parked();
    }
}

#[gpui_kit::test]
fn confirming_topic_deletion_sends_only_the_scope_shown_in_the_warning(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 1200., 760.);
    let sent = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.select_topic("home", window, cx);
            let sent = sent.clone();
            view.show_confirm_topic_deletion_dialog("home".into(), window, cx, move |_, topics| {
                *sent.borrow_mut() = topics;
                Ok(())
            });
            view.topics.receive(message("home/c"), std::time::Instant::now());
            view.select_topic("home/a", window, cx);
        });
    })
    .unwrap();
    assert!(sent.borrow().is_empty());
    click_topic_deletion_button(cx, handle, "ok");
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(*sent.borrow(), ["home", "home/a", "home/b"]);
        assert_eq!(view.selected.as_deref(), Some("home/a"));
        // QoS 0 has no acknowledgement; cached values must not imply broker deletion succeeded.
        assert_eq!(view.payload.read(cx).value().as_str(), "42");
        assert_eq!(view.topics.topics, 3);
        assert!(view.error.is_none());
    });
}

#[gpui_kit::test]
fn failed_topic_deletion_reports_the_error_without_losing_topic_data(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 1200., 760.);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.select_topic("home/a", window, cx);
            view.show_confirm_topic_deletion_dialog("home/a".into(), window, cx, |_, _| anyhow::bail!("queue full"));
        });
    })
    .unwrap();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("delete-topic-scope").label().unwrap().contains("0 known child topics"));
    })
    .unwrap();
    click_topic_deletion_button(cx, handle, "ok");
    cx.update(|cx| {
        let view = view.read(cx);
        assert!(view.error.as_ref().unwrap().contains("queue full"));
        assert!(view.status.is_connected());
        assert_eq!(view.topics.topics, 2);
        assert_eq!(view.payload.read(cx).value().as_str(), "42");
    });
}

#[gpui_kit::test]
fn disconnecting_during_topic_deletion_confirmation_still_queues_the_confirmed_request(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 1200., 760.);
    let sent = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            let sent = sent.clone();
            view.show_confirm_topic_deletion_dialog("home".into(), window, cx, move |_, topics| {
                *sent.borrow_mut() = topics;
                Ok(())
            });
            view.status = ConnectionStatus::Disconnected;
        });
    })
    .unwrap();
    click_topic_deletion_button(cx, handle, "ok");
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(*sent.borrow(), ["home", "home/a", "home/b"]);
        assert!(view.error.is_none());
        assert_eq!(view.topics.topics, 2);
    });
}

#[gpui_kit::test]
fn disconnected_topic_deletion_does_not_open_a_dialog(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 1200., 760.);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.select_topic("home", window, cx);
            view.status = ConnectionStatus::Disconnected;
            view.show_confirm_topic_deletion_dialog("home".into(), window, cx, |_, _| panic!("must not publish while disconnected"));
        });
        window.render_frame(cx);
        window.click("delete-topic", cx);
    })
    .unwrap();
    cx.update_window(handle, |_, window, cx| assert!(!window.has_active_dialog(cx)))
        .unwrap();
}

#[gpui_kit::test]
fn payload_language_follows_selection_and_json_remains_readonly_and_copyable(cx: &mut TestAppContext) {
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
    let formatted = serde_json::to_string_pretty(&serde_json::from_slice::<serde_json::Value>(json).unwrap()).unwrap();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(view.read(cx).payload.read(cx).language_name().as_str(), "json");
        assert_eq!(view.read(cx).payload.read(cx).value().as_str(), formatted);
        window.click("payload", cx);
        window.press("secondary-a", cx);
        window.input("must not replace the payload", cx);
        window.press("secondary-c", cx);
        assert_eq!(cx.read_from_clipboard().and_then(|item| item.text()), Some(formatted.clone()));
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
        assert_eq!(view.read(cx).payload.read(cx).language_name().as_str(), "plaintext");
        assert_eq!(view.read(cx).payload.read(cx).value().as_str(), "online");
        window.press("down", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(view.read(cx).payload_format, "Binary · hex");
        assert_eq!(view.read(cx).payload.read(cx).language_name().as_str(), "plaintext");
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
            let received_age = window.find("received-age");
            let payload = window.find("payload");
            assert!(metadata.visible());
            assert!(metadata.bounds().bottom() <= payload.bounds().top());
            assert!(received_age.visible());
            assert!(received_age.bounds().size.width > px(0.));
            assert!(received_age.bounds().right() <= window.find("details-pane").bounds().right());
            assert!(received_age.bounds().bottom() <= payload.bounds().top());
            assert!(payload.visible());
            assert!(payload.bounds().size.height > px(0.));
            assert!(payload.bounds().bottom() <= window.find("details-pane").bounds().bottom());
        })
        .unwrap();
    }
}

#[gpui_kit::test]
fn topic_filter_shortcuts_work_from_the_application_shell_payload_and_toolbar(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 900., 640.);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| view.select_topic("home/a", window, cx));
    })
    .unwrap();
    cx.run_until_parked();

    for shortcut in ["/", "ctrl-f"] {
        for source in ["application", "payload", "toolbar"] {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                match source {
                    "application" => view.update(cx, |view, cx| view.focus.focus(window, cx)),
                    "payload" => window.click("payload", cx),
                    "toolbar" => {
                        view.update(cx, |view, cx| view.focus_topics(window, cx));
                        window.press("shift-tab", cx);
                        window.press("shift-tab", cx);
                        assert_eq!(window.find("settings").focused(), Some(true));
                    }
                    _ => unreachable!(),
                }
                window.render_frame(cx);
                assert_eq!(window.find("topic-search").focused(), Some(false));
                window.press(shortcut, cx);
                assert_eq!(window.find("topic-search").focused(), Some(true), "{shortcut} from {source}");
            })
            .unwrap();
        }
    }

    cx.update_window(handle, |_, window, cx| {
        window.input("home/a", cx);
        assert_eq!(view.read(cx).topic_filter.read(cx).value().as_str(), "home/a");
        window.press("ctrl-f", cx);
        assert_eq!(window.find("topic-search").focused(), Some(true));
        assert_eq!(view.read(cx).topic_filter.read(cx).value().as_str(), "home/a");
        view.update(cx, |view, cx| view.clear_topics(window, cx));
        for shortcut in ["/", "ctrl-f"] {
            view.update(cx, |view, cx| view.focus_topics(window, cx));
            window.press(shortcut, cx);
            assert_eq!(window.find("topic-search").focused(), Some(true));
        }
    })
    .unwrap();
}

#[gpui_kit::test]
fn topic_filter_shortcuts_preserve_modal_settings_focus_and_text_entry(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, true, 900., 640.);
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("host", cx);
        window.press("secondary-a", cx);
        window.input("broker/path", cx);
        assert_eq!(view.read(cx).host.read(cx).value().as_str(), "broker/path");
        window.press("ctrl-f", cx);
        assert_eq!(window.find("host").focused(), Some(true));
        assert_eq!(window.find("topic-search").focused(), Some(false));
        assert!(window.find("settings-dialog").visible());
    })
    .unwrap();
}

#[gpui_kit::test]
fn topic_filter_shortcuts_focus_it_and_clear_button_resets_it(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 900., 640.);

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("topic-search").focused(), Some(false));
        window.press("/", cx);
        window.render_frame(cx);
        assert_eq!(window.find("topic-search").focused(), Some(true));
        window.input("home", cx);
    })
    .unwrap();

    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(view.read(cx).topic_filter.read(cx).value().as_str(), "home");
        assert!(window.find("clean").visible());
        window.click("clean", cx);
    })
    .unwrap();

    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(view.read(cx).topic_filter.read(cx).value().as_str(), "");
        assert_eq!(window.find("topic-search").focused(), Some(true));
        assert!(window.try_find("clean").is_none());
        window.press("escape", cx);
        window.press("ctrl-f", cx);
        window.render_frame(cx);
        assert_eq!(window.find("topic-search").focused(), Some(true));
    })
    .unwrap();
}

#[gpui_kit::test]
fn topic_filter_shows_matching_paths_and_ancestors_then_restores_the_tree(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 900., 640.);
    view.update(cx, |view, cx| {
        view.expanded.insert("home".into());
        view.sync_tree(cx);
        cx.notify();
    });

    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| view.select_topic("home/b", window, cx));
        window.render_frame(cx);
        assert!(window.find("topic-search").bounds().size.width >= window.rem_size() * 10.);
        window.click("topic-search", cx);
        assert_eq!(window.find("topic-search").focused(), Some(true));
        window.input("HOME/A", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(view.read(cx).topic_filter.read(cx).value().as_str(), "HOME/A");
        assert_eq!(view.read(cx).selected.as_deref(), Some("home/b"));
        assert!(window.find("topic:home").visible());
        assert!(window.find("topic:home/a").visible());
        assert!(window.try_find("topic:home/b").is_none());
        window.press("secondary-a", cx);
        window.input("missing", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("topics-filter-empty").visible());
        window.press("escape", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(view.read(cx).topic_filter.read(cx).value().as_str(), "missing");
        assert_eq!(window.find("topic-search").focused(), Some(false));
        assert!(window.find("topics-filter-empty").visible());
        assert!(window.try_find("topic:home/a").is_none());
        assert!(window.try_find("topic:home/b").is_none());
        for shortcut in ["/", "ctrl-f"] {
            window.press(shortcut, cx);
            assert_eq!(window.find("topic-search").focused(), Some(true));
            assert_eq!(view.read(cx).topic_filter.read(cx).value().as_str(), "missing");
            window.press("escape", cx);
            assert_eq!(window.find("topic-search").focused(), Some(false));
        }
        window.click("clean", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(view.read(cx).topic_filter.read(cx).value().as_str(), "");
        assert!(window.try_find("topics-filter-empty").is_none());
        assert!(window.find("topic:home/a").visible());
        assert!(window.find("topic:home/b").visible());
    })
    .unwrap();
}

#[gpui_kit::test]
fn publish_panel_queues_one_retained_qos_one_message_without_claiming_broker_acknowledgement(cx: &mut TestAppContext) {
    // Broker events wake the GPUI executor from the real MQTT worker thread.
    cx.executor().allow_parking();

    use std::{
        io::{ErrorKind, Read, Write},
        net::{TcpListener, TcpStream},
        time::{Duration, Instant},
    };

    fn packet(stream: &mut TcpStream) -> (u8, Vec<u8>) {
        let mut header = [0; 2];
        stream.read_exact(&mut header).unwrap();
        assert!(header[1] < 128, "test broker expects short packets");
        let mut body = vec![0; usize::from(header[1])];
        stream.read_exact(&mut body).unwrap();
        (header[0], body)
    }

    let topic = "home/command/é";
    let payload = "{\"enabled\":true}";
    let feedback = "Queued for sending · delivery not confirmed";
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (sent, received) = std::sync::mpsc::channel();
    let (check_duplicates, duplicate_check_requested) = std::sync::mpsc::channel();
    let (checked, duplicate_check_completed) = std::sync::mpsc::channel();
    let broker = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let (header, connect) = packet(&mut stream);
        assert_eq!(header, 0x10);
        assert_eq!(&connect[..7], b"\x00\x04MQTT\x05", "must connect using MQTT 5");
        stream.write_all(&[0x20, 3, 0, 0, 0]).unwrap();
        let (header, subscribe) = packet(&mut stream);
        assert_eq!(header, 0x82);
        assert_ne!(u16::from_be_bytes([subscribe[0], subscribe[1]]), 0);
        assert_eq!(subscribe[2], 0, "SUBSCRIBE properties must be empty");
        assert_eq!(&subscribe[3..], b"\x00\x01#\x00");
        stream.write_all(&[0x90, 4, subscribe[0], subscribe[1], 0, 0]).unwrap();
        sent.send(packet(&mut stream)).unwrap();

        // Withhold PUBACK: queue feedback must not depend on broker acknowledgement.
        duplicate_check_requested.recv_timeout(Duration::from_secs(5)).unwrap();
        stream.set_read_timeout(Some(Duration::from_millis(250))).unwrap();
        let error = stream.read(&mut [0]).expect_err("pending clicks must not send another packet");
        assert!(matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut));
        checked.send(()).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        assert_eq!(stream.read(&mut [0]).unwrap(), 0);
    });

    let mut connection = crate::mqtt::connect(ConnectionConfig {
        host: "127.0.0.1".into(),
        port,
        ..Default::default()
    })
    .unwrap();
    let events = connection.take_events().unwrap();
    let (handle, view) = open(cx, false, 1200., 760.);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.connection = Some(connection);
            view.status = ConnectionStatus::Connecting;
            view.listen_for_events(events, window, cx);
            cx.notify();
        });
    })
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        cx.run_until_parked();
        let connected = cx
            .update_window(handle, |_, _, cx| {
                assert!(
                    !matches!(view.read(cx).status, ConnectionStatus::Failed(_)),
                    "test broker connection failed"
                );
                view.read(cx).status.is_connected()
            })
            .unwrap();
        if connected {
            break;
        }
        assert!(Instant::now() < deadline, "view must process the broker connection");
        std::thread::sleep(Duration::from_millis(1));
    }
    cx.update_window(handle, |_, window, _| window.activate_window()).unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("toggle-publish", cx);
        assert_eq!(window.find("publish-topic").value(), Some(""));
        window.click("publish-topic", cx);
        window.input(topic, cx);
        window.click("publish-payload", cx);
        window.input(payload, cx);
        window.click("publish-retain", cx);
        view.update(cx, |view, cx| {
            view.publish_qos.update(cx, |state, cx| state.set_selected_value(&"1", window, cx));
        });
    })
    .unwrap();
    cx.run_until_parked();
    let payload_bounds = cx
        .update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.find("publish-payload").bounds()
        })
        .unwrap();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("publish-topic").value(), Some(topic));
        assert_eq!(view.read(cx).publish_payload.read(cx).value().as_str(), payload);
        assert_eq!(view.read(cx).publish_qos.read(cx).selected_value(), Some(&"1"));
        assert_eq!(window.find("publish-retain").checked(), Some(true));
        assert!(view.read(cx).publish_feedback.is_none());
        assert!(!view.read(cx).publish_pending);

        window.click("publish-message", cx);
        assert!(view.read(cx).publish_pending);
        assert!(view.read(cx).publish_feedback.is_none());
        window.render_frame(cx);
        window.click("publish-message", cx);
        assert!(view.read(cx).publish_pending);
        assert!(view.read(cx).publish_feedback.is_none());
    })
    .unwrap();

    let (header, body) = received.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(header, 0x33, "must publish with QoS 1, retain set, and DUP unset");
    let topic_length = usize::from(u16::from_be_bytes([body[0], body[1]]));
    assert_eq!(&body[2..2 + topic_length], topic.as_bytes());
    let packet_id = u16::from_be_bytes([body[2 + topic_length], body[3 + topic_length]]);
    assert_ne!(packet_id, 0, "QoS 1 requires a nonzero packet identifier");
    assert_eq!(body[4 + topic_length], 0, "PUBLISH properties must be empty");
    assert_eq!(&body[5 + topic_length..], payload.as_bytes());

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        cx.run_until_parked();
        let queued = cx
            .update_window(handle, |_, _, cx| view.read(cx).publish_feedback.is_some())
            .unwrap();
        if queued {
            break;
        }
        assert!(Instant::now() < deadline, "view must process PublishQueued without PUBACK");
        std::thread::sleep(Duration::from_millis(1));
    }
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let view = view.read(cx);
        assert!(!view.publish_pending);
        assert_eq!(view.publish_feedback.as_ref().unwrap().as_deref(), Ok(feedback));
        assert!(window.try_find("publish-feedback").is_none());
        assert_eq!(window.find("publish-payload").bounds(), payload_bounds);
        assert_ne!(window.find("publish-message").disabled(), Some(true));
        assert_eq!(window.find("publish-topic").value(), Some(topic));
        assert_eq!(view.publish_payload.read(cx).value().as_str(), payload);
        assert_eq!(view.publish_qos.read(cx).selected_value(), Some(&"1"));
        assert!(view.publish_retain);
        assert!(view.status.is_connected());
    })
    .unwrap();

    check_duplicates.send(()).unwrap();
    duplicate_check_completed.recv_timeout(Duration::from_secs(5)).unwrap();
    view.update(cx, |view, _| view.connection = None);
    broker.join().unwrap();
}

#[gpui_kit::test]
fn publish_file_panel_sends_exact_binary_and_image_bytes_with_content_type_to_the_broker(cx: &mut TestAppContext) {
    use std::{
        io::{ErrorKind, Read, Write},
        net::{TcpListener, TcpStream},
        time::{Duration, Instant},
    };

    fn packet(stream: &mut TcpStream) -> (u8, Vec<u8>) {
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        let header = byte[0];
        let mut length = 0;
        for shift in [0, 7, 14, 21] {
            stream.read_exact(&mut byte).unwrap();
            length |= usize::from(byte[0] & 0x7f) << shift;
            if byte[0] & 0x80 == 0 {
                let mut body = vec![0; length];
                stream.read_exact(&mut body).unwrap();
                return (header, body);
            }
        }
        panic!("invalid MQTT remaining length");
    }

    fn wait_for_state(cx: &mut TestAppContext, view: &Entity<Explorer>, condition: impl Fn(&Explorer) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            cx.run_until_parked();
            if cx.update(|cx| {
                let state = view.read(cx);
                assert!(
                    !matches!(state.status, ConnectionStatus::Failed(_)),
                    "test broker connection failed"
                );
                condition(state)
            }) {
                return;
            }
            assert!(Instant::now() < deadline, "UI must process the broker event");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    cx.executor().allow_parking();
    let directory = tempfile::tempdir().unwrap();
    let binary = directory.path().join("command.bin");
    let binary_bytes = [0, 0xff, 0x80, b'\r', b'\n', 0, b'{', b'}'];
    std::fs::write(&binary, binary_bytes).unwrap();
    let image = directory.path().join("command.svg");
    std::fs::write(&image, PUBLISH_FILE_SVG).unwrap();
    let topic = "home/command/é";
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (sent, received) = std::sync::mpsc::channel();
    let (check_duplicates, duplicate_check_requested) = std::sync::mpsc::channel();
    let (checked, duplicate_check_completed) = std::sync::mpsc::channel();
    let broker = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let (header, connect) = packet(&mut stream);
        assert_eq!(header, 0x10);
        assert_eq!(&connect[..7], b"\x00\x04MQTT\x05");
        stream.write_all(&[0x20, 3, 0, 0, 0]).unwrap();
        let (header, subscribe) = packet(&mut stream);
        assert_eq!(header, 0x82);
        assert_eq!(&subscribe[2..], b"\x00\x00\x01#\x00");
        stream.write_all(&[0x90, 4, subscribe[0], subscribe[1], 0, 0]).unwrap();
        for _ in 0..2 {
            sent.send(packet(&mut stream)).unwrap();
        }
        // Queue completion is independent of PUBACK, just as for text publishing.
        duplicate_check_requested.recv_timeout(Duration::from_secs(5)).unwrap();
        stream.set_read_timeout(Some(Duration::from_millis(250))).unwrap();
        let error = stream
            .read(&mut [0])
            .expect_err("busy clicks and shortcuts must not send duplicate files");
        assert!(matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut));
        checked.send(()).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        assert_eq!(stream.read(&mut [0]).unwrap(), 0);
    });

    let mut connection = crate::mqtt::connect(ConnectionConfig {
        host: "127.0.0.1".into(),
        port,
        ..Default::default()
    })
    .unwrap();
    let events = connection.take_events().unwrap();
    let (handle, view) = open_publish_file_draft(cx, 1200., 760.);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.connection = Some(connection);
            view.status = ConnectionStatus::Connecting;
            view.listen_for_events(events, window, cx);
            cx.notify();
        });
    })
    .unwrap();
    wait_for_state(cx, &view, |view| view.status.is_connected());
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("publish-topic", cx);
        window.press("secondary-a", cx);
        window.input(topic, cx);
        window.click("publish-retain", cx);
        window.click("publish-qos", cx);
    })
    .unwrap();
    cx.run_until_parked();
    for key in ["home", "down", "enter"] {
        cx.update_window(handle, |_, window, cx| window.press(key, cx)).unwrap();
        cx.run_until_parked();
    }
    cx.update(|cx| assert_eq!(view.read(cx).publish_qos.read(cx).selected_value(), Some(&"1")));

    for (path, bytes, content_type, shortcut) in [
        (&binary, binary_bytes.as_slice(), "application/octet-stream", false),
        (&image, PUBLISH_FILE_SVG, "image/svg+xml", true),
    ] {
        cx.update_window(handle, |_, window, cx| {
            window.click("load-publish-file", cx);
            window.click("publish-message", cx);
            window.click("publish-topic", cx);
            window.press("ctrl-enter", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update(|cx| {
            assert!(view.read(cx).publish_file_task.is_some());
            assert!(
                !view.read(cx).publish_pending,
                "clicks and Ctrl+Enter must not publish while a file load is in progress"
            );
        });
        respond_to_publish_file_prompt(cx, Some(path));
        let payload_bounds = cx
            .update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                let payload_bounds = window.find("publish-payload").bounds();
                assert_eq!(window.find("publish-content-type").label(), Some(content_type));
                assert!(view.read(cx).publish_file_task.is_none());
                if shortcut {
                    window.click("publish-topic", cx);
                    window.press("ctrl-enter", cx);
                } else {
                    window.click("publish-message", cx);
                }
                payload_bounds
            })
            .unwrap();
        if !shortcut {
            cx.update_window(handle, |_, window, cx| {
                assert!(view.read(cx).publish_pending);
                window.render_frame(cx);
                for id in ["publish-message", "load-publish-file", "remove-publish-file"] {
                    window.click(id, cx);
                }
                window.click("publish-topic", cx);
                window.press("ctrl-enter", cx);
                assert!(view.read(cx).publish_pending);
                assert!(view.read(cx).publish_file.is_some());
            })
            .unwrap();
            assert!(!cx.did_prompt_for_paths());
        }
        // Keyboard actions are deferred; let the UI queue the command before waiting on the worker.
        cx.run_until_parked();
        let (header, body) = received.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(header, 0x33, "QoS 1 and retain must survive switching from text to file");
        let topic_length = usize::from(u16::from_be_bytes([body[0], body[1]]));
        assert_eq!(&body[2..2 + topic_length], topic.as_bytes());
        let packet_id = u16::from_be_bytes([body[2 + topic_length], body[3 + topic_length]]);
        assert_ne!(packet_id, 0);
        let properties_start = 5 + topic_length;
        let properties_length = usize::from(body[4 + topic_length]);
        let mut properties = vec![0x03]; // MQTT 5 Content Type, without Payload Format Indicator.
        properties.extend_from_slice(&(content_type.len() as u16).to_be_bytes());
        properties.extend_from_slice(content_type.as_bytes());
        assert_eq!(&body[properties_start..properties_start + properties_length], properties);
        assert_eq!(&body[properties_start + properties_length..], bytes);
        wait_for_state(cx, &view, |view| view.publish_feedback.is_some());
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let state = view.read(cx);
            assert!(!state.publish_pending);
            assert_eq!(
                state.publish_feedback.as_ref().unwrap().as_deref(),
                Ok("Queued for sending · delivery not confirmed")
            );
            assert!(state.publish_file.is_some());
            assert_eq!(state.publish_payload.read(cx).value().as_str(), PUBLISH_FILE_DRAFT);
            assert_eq!(state.publish_qos.read(cx).selected_value(), Some(&"1"));
            assert_eq!(window.find("publish-retain").checked(), Some(true));
            assert!(window.find("load-publish-file").visible());
            assert!(window.try_find("publish-feedback").is_none());
            assert_eq!(window.find("publish-payload").bounds(), payload_bounds);
        })
        .unwrap();
    }
    check_duplicates.send(()).unwrap();
    duplicate_check_completed.recv_timeout(Duration::from_secs(5)).unwrap();
    view.update(cx, |view, _| view.connection = None);
    broker.join().unwrap();
}
