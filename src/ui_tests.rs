use bytes::Bytes;
use chrono::Local;
use gpui_kit::component::{ActiveTheme, Theme, ThemeMode};
use gpui_kit::test::TestWindowExt;
use gpui_kit::{AnyWindowHandle, AppContext, Bounds, Entity, Styled, TestAppContext, WindowBounds, WindowOptions, px, size};

use super::{ConnectionStatus, Explorer};
use crate::{
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
    }
}

#[gpui_kit::test]
fn status_bar_stays_compact_across_connection_states_and_text_sizes(cx: &mut TestAppContext) {
    for font_size in [16., 20.] {
        let (handle, view) = open(cx, false, 760., 540.);
        cx.update(|cx| Theme::update(cx, |theme| theme.font_size = px(font_size)));
        for status in [
            ConnectionStatus::Disconnected,
            ConnectionStatus::Connecting,
            ConnectionStatus::Connected,
            ConnectionStatus::Failed("Could not connect to broker".into()),
        ] {
            view.update(cx, |view, cx| {
                view.status = status;
                cx.notify();
            });
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                let bar = window.find("status");
                assert!(bar.visible());
                assert_eq!(bar.label(), Some(view.read(cx).status.label()));
                let height = bar.bounds().size.height;
                let compact_height = window.rem_size() * 1.5;
                assert!(
                    height >= compact_height && height <= compact_height + px(1.),
                    "status bar should retain its compact height: {height:?}"
                );
            })
            .unwrap();
        }
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
fn new_connection_topics_default_to_all_topics_with_qos_zero_before_credentials(cx: &mut TestAppContext) {
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
        assert!(section.bounds().top() > window.find("tls").bounds().bottom());
        assert!(section.bounds().bottom() < window.find("connection-field:username").bounds().top());
        assert!(window.find("field:username").bounds().bottom() < window.find("field:password").bounds().top());
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
    for id in ["tls", "toggle-topics", "begin-add-topic"] {
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
        ("tab", "host", Some("broker.example")),
        ("tab", "port", Some("8883")),
        ("tab", "tls", None),
        ("tab", "toggle-topics", None),
        ("tab", "begin-add-topic", None),
        ("tab", "remove-topic:#", None),
        ("tab", "username", Some("another-reader")),
        ("tab", "password", Some("new-password")),
        ("shift-tab", "username", Some("replacement-reader")),
        ("shift-tab", "remove-topic:#", None),
        ("shift-tab", "begin-add-topic", None),
        ("shift-tab", "toggle-topics", None),
        ("shift-tab", "tls", None),
        ("shift-tab", "port", Some("1884")),
        ("shift-tab", "host", Some("other.example")),
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
fn escape_cancels_the_topic_editor_then_connection_edits_and_restores_focus(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 1200., 1600.);
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
        window.click("begin-add-topic", cx);
        window.input("discarded/#", cx);
        window.press("escape", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("settings-dialog").visible());
        assert!(window.try_find("topic-editor").is_none());
        assert_eq!(window.find("port").focused(), Some(true));
        assert_eq!(window.find("port").value(), Some("9999"));
        assert_eq!(view.read(cx).subscription_topics, vec![subscription("#", 0)]);
        assert_eq!(view.read(cx).saved_connections.connections[0].topics, vec![subscription("#", 0)]);
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
        assert!(payload.bounds().bottom() <= window.find("status").bounds().top());
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

#[gpui_kit::test]
fn topic_delete_button_requires_confirmation_and_publishes_empty_retained_messages(cx: &mut TestAppContext) {
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
    let broker = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        assert_eq!(packet(&mut stream).0, 0x10);
        stream.write_all(&[0x20, 2, 0, 0]).unwrap();
        let (header, subscribe) = packet(&mut stream);
        assert_eq!(header, 0x82);
        stream.write_all(&[0x90, 3, subscribe[0], subscribe[1], 2]).unwrap();
        let mut topics = Vec::new();
        for _ in 0..3 {
            let (header, body) = packet(&mut stream);
            assert_eq!(header, 0x31, "must publish with QoS 0 and retain set");
            let length = usize::from(u16::from_be_bytes([body[0], body[1]]));
            assert_eq!(body.len(), length + 2, "deletion payload must be empty");
            topics.push(String::from_utf8(body[2..].to_vec()).unwrap());
        }
        sent.send(topics).unwrap();
        assert_eq!(stream.read(&mut [0]).unwrap(), 0);
    });
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let mut connection = crate::mqtt::connect(ConnectionConfig {
        host: "127.0.0.1".into(),
        port,
        ..Default::default()
    })
    .unwrap();
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match connection.events.recv().await.unwrap() {
                    crate::mqtt::BrokerEvent::Connected => break,
                    crate::mqtt::BrokerEvent::Status(error) => panic!("test broker failed: {error}"),
                    _ => {}
                }
            }
        })
        .await
        .unwrap();
    });
    let (handle, view) = open(cx, false, 1200., 760.);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.connection = Some(connection);
            view.select_topic("home", window, cx);
        });
        window.render_frame(cx);
        window.click("delete-topic", cx);
    })
    .unwrap();
    assert_eq!(cx.pending_prompt().unwrap().0, "Confirm delete");
    assert!(received.try_recv().is_err());
    cx.simulate_prompt_answer("Cancel");
    cx.run_until_parked();
    assert!(received.try_recv().is_err());
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("delete-topic", cx);
    })
    .unwrap();
    cx.simulate_prompt_answer("Delete");
    cx.run_until_parked();
    assert_eq!(received.recv_timeout(Duration::from_secs(5)).unwrap(), ["home", "home/a", "home/b"]);
    view.update(cx, |view, _| view.connection = None);
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
            view.confirm_clear_topic_with_send("home".into(), window, cx, move |_, _| {
                sends.set(sends.get() + 1);
                Ok(())
            });
        });
    })
    .unwrap();
    let (title, detail) = cx.pending_prompt().expect("deletion must require confirmation");
    assert_eq!(title, "Confirm delete");
    assert!(detail.contains("\"home\" and 2 known child topics"));
    assert!(detail.contains("empty payload (QoS 0, retain)"));
    assert!(detail.contains("cannot be undone"));
    assert_eq!(sends.get(), 0);
    cx.simulate_prompt_answer("Cancel");
    cx.run_until_parked();
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(sends.get(), 0);
        assert_eq!(view.topics.topics, 2);
        assert_eq!(view.selected.as_deref(), Some("home"));
        assert!(view.error.is_none());
    });
}

#[gpui_kit::test]
fn confirming_topic_deletion_sends_only_the_scope_shown_in_the_warning(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 1200., 760.);
    let sent = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.select_topic("home", window, cx);
            let sent = sent.clone();
            view.confirm_clear_topic_with_send("home".into(), window, cx, move |_, topics| {
                *sent.borrow_mut() = topics;
                Ok(())
            });
            view.topics.receive(message("home/c"), std::time::Instant::now());
            view.select_topic("home/a", window, cx);
        });
    })
    .unwrap();
    assert!(sent.borrow().is_empty());
    cx.simulate_prompt_answer("Delete");
    cx.run_until_parked();
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
            view.confirm_clear_topic_with_send("home/a".into(), window, cx, |_, _| anyhow::bail!("queue full"));
        });
    })
    .unwrap();
    assert!(cx.pending_prompt().unwrap().1.contains("0 known child topics"));
    cx.simulate_prompt_answer("Delete");
    cx.run_until_parked();
    cx.update(|cx| {
        let view = view.read(cx);
        assert!(view.error.as_ref().unwrap().contains("queue full"));
        assert!(view.status.is_connected());
        assert_eq!(view.topics.topics, 2);
        assert_eq!(view.payload.read(cx).value().as_str(), "42");
    });
}

#[gpui_kit::test]
fn disconnecting_during_topic_deletion_confirmation_prevents_publishing(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 1200., 760.);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.confirm_clear_topic_with_send("home".into(), window, cx, |_, _| panic!("must not publish after disconnect"));
            view.status = ConnectionStatus::Disconnected;
        });
    })
    .unwrap();
    cx.simulate_prompt_answer("Delete");
    cx.run_until_parked();
    cx.update(|cx| {
        let view = view.read(cx);
        assert!(view.error.as_ref().unwrap().contains("disconnected"));
        assert_eq!(view.topics.topics, 2);
    });
}

#[gpui_kit::test]
fn disconnected_topic_deletion_does_not_open_a_prompt(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, false, 1200., 760.);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.select_topic("home", window, cx);
            view.status = ConnectionStatus::Disconnected;
            view.confirm_clear_topic_with_send("home".into(), window, cx, |_, _| panic!("must not publish while disconnected"));
        });
        window.render_frame(cx);
        window.click("delete-topic", cx);
    })
    .unwrap();
    assert!(!cx.has_pending_prompt());
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
