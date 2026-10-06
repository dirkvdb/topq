//! Retained render boundaries keep animated panes from rebuilding their siblings.

use gpui_kit::{Context, Entity, IntoElement, Render, Subscription, WeakEntity, Window, div};

use super::Explorer;

#[derive(Clone, Copy)]
pub(super) enum ExplorerPaneKind {
    Header,
    Topics,
    Details,
    Publish,
}

pub(super) struct ExplorerPane {
    explorer: WeakEntity<Explorer>,
    kind: ExplorerPaneKind,
    _subscription: Subscription,
    #[cfg(test)]
    pub(super) render_count: usize,
}

impl ExplorerPane {
    pub(super) fn new(explorer: &Entity<Explorer>, kind: ExplorerPaneKind, cx: &mut Context<Self>) -> Self {
        // Explicit shell/state changes invalidate all panes; child notifications
        // (animation, editor decorations, and clock ticks) do not emit this signal.
        let subscription = cx.observe(explorer, |_, _, cx| cx.notify());
        Self {
            explorer: explorer.downgrade(),
            kind,
            _subscription: subscription,
            #[cfg(test)]
            render_count: 0,
        }
    }
}

impl Render for ExplorerPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        {
            self.render_count += 1;
        }
        let Some(explorer) = self.explorer.upgrade() else {
            return div().into_any_element();
        };
        explorer.update(cx, |explorer, cx| match self.kind {
            ExplorerPaneKind::Header => explorer.header(cx).into_any_element(),
            ExplorerPaneKind::Topics => explorer.topic_list(cx).into_any_element(),
            ExplorerPaneKind::Details => explorer.details(cx).into_any_element(),
            ExplorerPaneKind::Publish => explorer.publish_panel(window, cx).into_any_element(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use bytes::Bytes;
    use chrono::Local;
    use gpui_kit::component::{Theme, ThemeMode, TitleBar};
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{AppContext, Bounds, TestAppContext, WindowBounds, WindowOptions, px, size};
    use rumqttc::v5::mqttbytes::v5::PublishProperties;

    use super::Explorer;
    use crate::topics::{Message, MessageProperties};

    fn message(content_type: Option<&str>) -> Message {
        Message {
            topic: "test".into(),
            payload: Bytes::from_static(br#"{"on":true}"#),
            qos: 0,
            retained: false,
            received_at: Local::now(),
            properties: MessageProperties::from_publish(content_type.map(|content_type| PublishProperties {
                content_type: Some(content_type.to_owned()),
                ..Default::default()
            })),
        }
    }

    #[gpui_kit::test]
    fn content_type_details_follow_latest_message_without_changing_payload_format(cx: &mut TestAppContext) {
        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            let (handle, view) = cx.update(|cx| {
                if !cx.has_global::<Theme>() {
                    gpui_kit::init(cx);
                    crate::appearance::register_bundled(cx).unwrap();
                    super::super::init(cx);
                }
                Theme::change(mode, None, cx);
                Theme::update(cx, |theme| theme.font_size = px(20.));
                gpui_kit::open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(Bounds::new(Default::default(), size(px(760.), px(540.))))),
                        ..TitleBar::window_options()
                    },
                    cx,
                    |window, cx| {
                        cx.new(|cx| {
                            let mut explorer = Explorer::with_settings(None, None, None, window, cx);
                            explorer.show_config = false;
                            explorer.topics.receive(message(None), Instant::now());
                            explorer.sync_tree(cx);
                            explorer.focus_topics(window, cx);
                            explorer
                        })
                    },
                )
                .unwrap()
            });

            cx.update_window(handle, |_, window, cx| {
                window.activate_window();
                window.render_frame(cx);
                window.press("home", cx);
            })
            .unwrap();
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                assert_eq!(window.find("selected-topic").label(), Some("test"));
                assert!(window.try_find("message-content-type").is_none());
            })
            .unwrap();

            for content_type in [
                Some("application/json"),
                Some("application/vnd.example.long-content-type+json; charset=utf-8; profile=very-long-profile-name"),
                Some("text/plain"),
                None,
            ] {
                cx.update_window(handle, |_, window, cx| {
                    view.update(cx, |view, cx| {
                        view.topics.receive(message(content_type), Instant::now());
                        view.refresh_details(window, cx);
                    });
                })
                .unwrap();
                cx.run_until_parked();
                cx.update_window(handle, |_, window, cx| {
                    window.render_frame(cx);
                    if let Some(content_type) = content_type {
                        let row = window.find("message-content-type");
                        let expected = format!("Content type: {content_type}");
                        assert_eq!(row.label(), Some(expected.as_str()));
                        assert!(row.visible());
                        assert!(row.bounds().size.height > px(0.));
                        let details = window.find("details-pane");
                        assert!(row.bounds().left() >= details.bounds().left());
                        assert!(row.bounds().right() <= details.bounds().right());
                        assert!(window.find("topic-metadata").bounds().bottom() <= row.bounds().top());
                        assert!(window.find("received-age").bounds().bottom() <= row.bounds().top());
                        assert!(row.bounds().bottom() <= window.find("payload").bounds().top());
                        let value = window.find("message-content-type-value");
                        assert_eq!(value.label(), Some(content_type));
                        assert!(value.bounds().right() <= row.bounds().right());
                    } else {
                        assert!(window.try_find("message-content-type").is_none());
                    }
                    assert!(window.find("payload").bounds().size.height > px(0.));
                    assert_eq!(view.read(cx).payload_format, "JSON");
                    assert_eq!(view.read(cx).payload.read(cx).value().as_str(), "{\n  \"on\": true\n}");
                })
                .unwrap();
            }
        }
    }
}
