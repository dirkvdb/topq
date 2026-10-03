use bytes::Bytes;
use chrono::Local;
use gpui_kit::component::{ActiveTheme, Size, Theme, ThemeMode};
use gpui_kit::test::TestWindowExt;
use gpui_kit::{AnyWindowHandle, AppContext, Bounds, Entity, Styled, TestAppContext, WindowBounds, WindowOptions, px, size};

use super::{ConnectionStatus, Explorer};
use crate::{config::ConnectionConfig, topics::Message};

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
fn theme_selector_stays_compact_in_appearance_settings(cx: &mut TestAppContext) {
    let (handle, _) = open(cx, true, 760., 540.);
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("0-1", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let theme_width = window.find("theme").bounds().size.width;
        let settings_width = window.find("settings-dialog").bounds().size.width;
        assert!(
            theme_width < settings_width / 2.,
            "theme selector should not stretch across the settings page"
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn theme_menu_supports_keyboard_selection_dismissal_and_saved_preferences(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("appearance.json");
    let (handle, _) = open(cx, true, 760., 540.);
    cx.update(|cx| cx.set_global(crate::appearance::load(&path).unwrap()));
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("settings-dialog").visible());
        window.click("0-1", cx);
    })
    .unwrap();
    cx.run_until_parked();
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
        assert!(window.find("settings-dialog").visible());
        window.click("theme", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        // Navigate the theme menu by keyboard; headings are skipped.
        window.press("down", cx);
        window.press("down", cx);
        window.press("down", cx);
        window.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        assert_eq!(cx.theme().theme_name().as_str(), "Gruvbox Light");
        crate::appearance::sync_system(window, cx);
        assert_eq!(cx.theme().theme_name().as_str(), "Gruvbox Light");
        window.render_frame(cx);
        assert!(window.try_find("popup-menu").is_none());
        assert!(window.find("settings-dialog").visible());
        assert_eq!(window.find("theme").label(), Some("Gruvbox Light"));
    })
    .unwrap();
    cx.update(|cx| {
        cx.set_global(crate::appearance::load(&path).unwrap());
        assert_eq!(crate::appearance::Appearance::selected(cx), Some("Gruvbox Light"));
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("theme", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.scroll("popup-menu", gpui_kit::ScrollDelta::Lines(gpui_kit::point(0., -20.)), cx);
        window.render_frame(cx);
        let mut menu = window.within("popup-menu");
        assert_eq!(menu.find(21usize).label(), Some("Tokyo Night"));
        menu.click(21usize, cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(cx.theme().theme_name().as_str(), "Tokyo Night");
        assert_eq!(cx.theme().highlight_theme.name, "Tokyo Night");
        assert_eq!(window.find("theme").label(), Some("Tokyo Night"));
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
        assert!(matches!(cx.theme().theme_name().as_str(), "Ayu Light" | "Ayu Dark"));
        cx.set_global(crate::appearance::load(&path).unwrap());
        assert_eq!(crate::appearance::Appearance::selected(cx), None);
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
        assert!(window.try_find("theme").is_none());
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
        assert!(window.try_find("theme").is_none());
        assert_eq!(view.read(cx).saved_connections.connections.len(), 2);
        assert_eq!(view.read(cx).saved_connections.selected, Some(0));
        window.click("0-1", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("settings-dialog").visible());
        assert!(window.find("theme").visible());
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
        assert!(window.find("host").bounds().size.width > px(250.));
        window.click("back-to-connections", cx);
        window.render_frame(cx);
        assert!(window.find("connection-card:0").visible());
        window.click("new-connection", cx);
        window.render_frame(cx);
        assert_eq!(window.find("name").value(), Some(""));
        assert_eq!(window.find("host").value(), Some("localhost"));
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
            view.flash_until = Some(std::time::Instant::now());
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
        assert!(view.flash_until.is_none());
        assert!(view.tree_state.read(cx).index_of(&"home".into()).is_none());
        assert!(view.tree_state.read(cx).selected_item().is_none());
        assert_eq!(view.topic_name.read(cx).value().as_str(), "");
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
fn connection_inputs_leave_room_for_their_focus_rings(cx: &mut TestAppContext) {
    for font_size in [14., 20.] {
        let (handle, _) = open(cx, true, 1200., 1200.);
        cx.update(|cx| Theme::update(cx, |theme| theme.font_size = px(font_size)));
        cx.update_window(handle, |_, window, cx| {
            for id in ["name", "host", "port", "username", "password"] {
                window.render_frame(cx);
                window.click(id, cx);
                window.render_frame(cx);
                let field = window.find(format!("connection-field:{id}")).bounds();
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
fn tls_checkbox_has_room_for_its_focus_ring_and_toggles_from_the_form(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, true, 760., 540.);
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.scroll("name", gpui_kit::ScrollDelta::Lines(gpui_kit::point(0., -6.)), cx);
        let input_left = window.find("port").bounds().left();
        let checkbox_left = window.find("tls").bounds().left();
        assert!(
            checkbox_left >= input_left + px(3.),
            "TLS focus ring touches the clipped edge: {checkbox_left:?} vs {input_left:?}"
        );
        window.click("tls", cx);
        window.render_frame(cx);
        assert!(view.read(cx).tls);
        assert_eq!(window.find("port").value(), Some("8883"));
    })
    .unwrap();
}

#[gpui_kit::test]
fn reopening_settings_restores_the_selected_connection(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 1200., 760.);
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
        assert_eq!(view.read(cx).editing, Some(0));
    })
    .unwrap();
}

#[gpui_kit::test]
fn tabbing_between_connection_inputs_selects_values_for_replacement(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, true, 1200., 900.);
    cx.update_window(handle, |_, window, _| window.activate_window()).unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            for (input, value) in [
                (&view.name, "Home"),
                (&view.host, "localhost"),
                (&view.port, "1883"),
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
    for (key, count, id, replacement) in [
        ("tab", 1, "host", "broker.example"),
        ("tab", 1, "port", "8883"),
        ("tab", 2, "username", "another-reader"),
        ("tab", 1, "password", "new-password"),
        ("shift-tab", 1, "username", "replacement-reader"),
        ("shift-tab", 2, "port", "1884"),
        ("shift-tab", 1, "host", "other.example"),
        ("shift-tab", 1, "name", "Workshop"),
    ] {
        // TLS is a separate Tab stop between the port and username.
        for _ in 0..count {
            cx.update_window(handle, |_, window, cx| window.press(key, cx)).unwrap();
            cx.run_until_parked();
        }
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find(id).focused(), Some(true), "expected {key} to focus {id}");
            window.input(replacement, cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let view = view.read(cx);
            let input = match id {
                "name" => &view.name,
                "host" => &view.host,
                "port" => &view.port,
                "username" => &view.username,
                "password" => &view.password,
                _ => unreachable!(),
            };
            assert_eq!(input.read(cx).value().as_str(), replacement);
        })
        .unwrap();
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
        assert_eq!(window.find("selected-topic").value(), Some("home/a"));
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
        assert_eq!(window.find("selected-topic").value(), Some("home/a"));
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
        assert!(window.find("settings-dialog").visible());
        window.click("edit-connection:0", cx);
        window.render_frame(cx);
        window.click("port", cx);
        window.press("secondary-a", cx);
        window.input("9999", cx);
        window.press("escape", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("settings-dialog").is_none());
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
        view.topics
            .receive(message("home/a"), std::time::Instant::now() - std::time::Duration::from_millis(100));
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
            view.topics.receive(message("energy/solar"), std::time::Instant::now());
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
            assert!(
                bounds.top() >= frame.top() + inset && bounds.bottom() <= frame.bottom() - inset,
                "topic text is clipped at base font {font_size}: frame {frame:?}, text {bounds:?}, line height {line_height:?}"
            );
            window.click("selected-topic", cx);
            window.press("secondary-a", cx);
            window.press("secondary-c", cx);
            assert_eq!(cx.read_from_clipboard().and_then(|item| item.text()), Some("energy/solar".into()));
        })
        .unwrap();
    }
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
