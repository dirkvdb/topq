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
    use std::{sync::Arc, time::Instant};

    use bytes::Bytes;
    use chrono::Local;
    use gpui_kit::component::{Theme, ThemeMode, TitleBar};
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{AppContext, Bounds, TestAppContext, WindowBounds, WindowOptions, px, size};
    use rumqttc::v5::mqttbytes::v5::PublishProperties;

    use super::Explorer;
    use crate::topics::{Message, MessageProperties};

    #[gpui_kit::test]
    fn ansi_payload_display_updates_styles_and_preserves_raw_copy(cx: &mut TestAppContext) {
        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            let (handle, view) = cx.update(|cx| {
                if !cx.has_global::<Theme>() {
                    gpui_kit::init(cx);
                    crate::appearance::register_bundled(cx).unwrap();
                    super::super::init(cx);
                }
                Theme::change(mode, None, cx);
                gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                    cx.new(|cx| {
                        let mut explorer = Explorer::with_settings(None, None, None, window, cx);
                        explorer.show_config = false;
                        explorer.selected = Some("test".into());
                        explorer
                    })
                })
                .unwrap()
            });
            for (raw, displayed, styled) in [
                ("\x1b[1;31mNo clients; rebooting\x1b[0m", "No clients; rebooting", true),
                ("\x1b[32mNo clients; rebooting\x1b[0m", "No clients; rebooting", true),
                ("No clients; rebooting", "No clients; rebooting", false),
                ("\x1b[31mé世界\x1b[0m", "é世界", true),
                ("{\"on\":true}", "{\n  \"on\": true\n}", false),
            ] {
                cx.update_window(handle, |_, window, cx| {
                    view.update(cx, |view, cx| {
                        let mut incoming = message(None);
                        incoming.payload = Bytes::copy_from_slice(raw.as_bytes());
                        view.topics.receive(incoming, Instant::now());
                        view.refresh_details(window, cx);
                        assert_eq!(view.payload.read(cx).value().as_str(), displayed);
                        let expected: Vec<_> = styled.then_some(0..displayed.len()).into_iter().collect();
                        assert_eq!(view.payload_ansi.get_ranges(cx), expected);
                    });
                    window.render_frame(cx);
                    assert!(window.find("payload-editor").visible());
                    window.click("copy-value", cx);
                    assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), raw);
                })
                .unwrap();
                cx.run_until_parked();
            }
            cx.update_window(handle, |_, window, cx| {
                view.update(cx, |view, cx| {
                    let mut incoming = message(None);
                    incoming.payload = Bytes::from_static(b"\x1b[31mA\x1b[32mB");
                    view.topics.receive(incoming, Instant::now());
                    view.refresh_details(window, cx);
                    assert_eq!(view.payload_ansi.get_ranges(cx), vec![0..1, 1..2]);
                });
            })
            .unwrap();
            // Equal theme colors merge adjacent runs, proving that a theme-only
            // update refreshes decorations without needing another MQTT message.
            cx.update(|cx| Theme::update(cx, |theme| theme.danger = theme.success));
            cx.run_until_parked();
            cx.update(|cx| {
                let ranges = view.read(cx).payload_ansi.get_ranges(cx);
                assert_eq!(ranges.len(), 1);
                assert_eq!(ranges[0], 0..2);
            });
        }
    }

    #[gpui_kit::test]
    fn preview_toggle_switches_processing_and_keeps_live_updates_raw(cx: &mut TestAppContext) {
        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            let (handle, view) = cx.update(|cx| {
                if !cx.has_global::<Theme>() {
                    gpui_kit::init(cx);
                    crate::appearance::register_bundled(cx).unwrap();
                    super::super::init(cx);
                }
                Theme::change(mode, None, cx);
                gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                    cx.new(|cx| {
                        let mut explorer = Explorer::with_settings(None, None, None, window, cx);
                        explorer.show_config = false;
                        explorer.selected = Some("test".into());
                        explorer
                    })
                })
                .unwrap()
            });
            for (content_type, raw, processed, language) in [
                (None, "{\"on\":true}", "{\n  \"on\": true\n}", "json"),
                // Formatting does not always change text; the highlighter must still switch.
                (None, "true", "true", "json"),
                (None, "\x1b[1;31mé世界\x1b[0m", "é世界", "plaintext"),
                (
                    Some("image/svg+xml"),
                    "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"1\" height=\"1\"/>",
                    "",
                    "plaintext",
                ),
                (Some("image/png"), "invalid image", "", "plaintext"),
            ] {
                cx.update_window(handle, |_, window, cx| {
                    window.activate_window();
                    view.update(cx, |view, cx| {
                        let mut incoming = message(content_type);
                        incoming.payload = Bytes::copy_from_slice(raw.as_bytes());
                        view.topics.receive(incoming, Instant::now());
                        view.refresh_details(window, cx);
                    });
                    window.render_frame(cx);
                    assert_eq!(window.find("preview-value").label(), Some("Show raw payload"));
                    assert!(view.read(cx).payload_preview);
                    assert_eq!(view.read(cx).payload.read(cx).value().as_str(), processed);
                    assert_eq!(view.read(cx).payload.read(cx).language_name(), language);
                    let preview = window.find("preview-value").bounds();
                    assert!(preview.right() <= window.find("edit-value").bounds().left());

                    window.click("preview-value", cx);
                    assert_eq!(window.find("preview-value").label(), Some("Render payload"));
                    assert!(!view.read(cx).payload_preview);
                    if content_type.is_some() {
                        assert!(window.find("payload-raw").visible());
                        assert!(window.try_find("payload-editor").is_none());
                        let raw_view = view.read(cx).payload_raw.as_ref().unwrap().read(cx);
                        assert_eq!(raw_view.bytes().as_ref(), raw.as_bytes());
                        assert!(view.read(cx).payload.read(cx).value().is_empty());
                    } else {
                        assert!(window.find("payload-editor").visible());
                        assert_eq!(view.read(cx).payload.read(cx).value().as_str(), raw);
                    }
                    assert!(window.try_find("payload-image").is_none());
                    assert!(window.try_find("payload-image-error").is_none());
                    assert!(view.read(cx).payload_image.is_none());
                    assert_eq!(view.read(cx).payload.read(cx).language_name(), "plaintext");
                    assert!(view.read(cx).payload_ansi.get_ranges(cx).is_empty());
                    assert!(view.read(cx).payload_highlight.ranges(cx).is_empty());

                    // Updates retain the selected mode, including on another topic.
                    view.update(cx, |view, cx| {
                        let mut incoming = message(None);
                        incoming.topic = "other".into();
                        incoming.payload = Bytes::from_static(b"\x1b[32m{\"updated\":true}\x1b[0m");
                        view.topics.receive(incoming, Instant::now());
                        view.selected = Some("other".into());
                        view.refresh_details(window, cx);
                    });
                    assert_eq!(view.read(cx).payload.read(cx).value().as_str(), "\x1b[32m{\"updated\":true}\x1b[0m");
                    assert!(view.read(cx).payload_ansi.get_ranges(cx).is_empty());
                    view.update(cx, |view, cx| {
                        view.selected = Some("test".into());
                        view.refresh_details(window, cx);
                    });
                    window.render_frame(cx);
                    // Clicking the toggle focuses it; Space re-enables Preview.
                    window.press("space", cx);
                    assert_eq!(window.find("preview-value").label(), Some("Show raw payload"));
                    assert!(view.read(cx).payload_preview);
                    assert_eq!(view.read(cx).payload.read(cx).value().as_str(), processed);
                    assert_eq!(view.read(cx).payload.read(cx).language_name(), language);
                    assert!(view.read(cx).payload_highlight.ranges(cx).is_empty());
                    if content_type.is_some() {
                        assert!(window.find("payload-image").visible());
                        assert!(window.try_find("payload-editor").is_none());
                    } else {
                        assert!(window.find("payload-editor").visible());
                    }
                    if raw.contains('\x1b') {
                        assert!(!view.read(cx).payload_ansi.get_ranges(cx).is_empty());
                    }
                    window.click("copy-value", cx);
                    assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), raw);
                })
                .unwrap();
                cx.run_until_parked();
            }
            cx.update_window(handle, |_, window, cx| {
                window.click("preview-value", cx);
                view.update(cx, |view, cx| {
                    let mut incoming = message(Some("image/png"));
                    incoming.payload = Bytes::from_static(b"\xff\x00A");
                    view.topics.receive(incoming, Instant::now());
                    view.refresh_details(window, cx);
                });
                window.render_frame(cx);
                assert!(view.read(cx).payload.read(cx).value().is_empty());
                assert!(window.find("payload-raw").visible());
                assert!(window.find(("payload-raw-row", 0usize)).label().unwrap().contains("ff 00 41"));
            })
            .unwrap();
            cx.run_until_parked();
        }
    }

    #[gpui_kit::test]
    fn large_raw_images_only_format_visible_bytes_and_support_navigation(cx: &mut TestAppContext) {
        let (handle, view) = cx.update(|cx| {
            gpui_kit::init(cx);
            crate::appearance::register_bundled(cx).unwrap();
            super::super::init(cx);
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
                        explorer.selected = Some("test".into());
                        explorer.topics.receive(message(Some("image/png")), Instant::now());
                        explorer.refresh_details(window, cx);
                        explorer
                    })
                },
            )
            .unwrap()
        });
        // Include both binary and huge single-line textual image data.
        for (content_type, byte) in [("image/png", 0xff), ("image/svg+xml", b'x')] {
            let payload = Bytes::from(vec![byte; 32 * 1024 * 1024 + 3]);
            let last_offset = (payload.len() - 1) / 16 * 16;
            cx.update_window(handle, |_, window, cx| {
                window.activate_window();
                window.render_frame(cx);
                if view.read(cx).payload_preview {
                    window.click("preview-value", cx);
                }
                view.update(cx, |view, cx| {
                    let mut incoming = message(Some(content_type));
                    incoming.payload = payload.clone();
                    view.topics.receive(incoming, Instant::now());
                    view.refresh_details(window, cx);
                });
                window.render_frame(cx);
                let raw = view.read(cx).payload_raw.as_ref().unwrap().clone();
                assert!(window.find("payload-raw").visible());
                assert!(window.try_find("payload-editor").is_none());
                assert!(view.read(cx).payload.read(cx).value().is_empty());
                assert_eq!(raw.read(cx).byte_count(), payload.len());
                assert_eq!(
                    raw.read(cx).bytes().as_ptr(),
                    payload.as_ptr(),
                    "Raw view must share, not copy, the payload"
                );
                assert!(raw.read(cx).rendered_rows > 0 && raw.read(cx).rendered_rows < 100);
                assert!(window.find(("payload-raw-row", 0usize)).visible());
                assert!(window.try_find(("payload-raw-row", 16_000usize)).is_none());

                window.click("payload-raw", cx);
                window.press("end", cx);
                assert!(window.find(("payload-raw-row", last_offset)).visible());
                assert!(raw.read(cx).rendered_rows < 100);
                window.press("home", cx);
                assert!(window.find(("payload-raw-row", 0usize)).visible());
                window.press("pagedown", cx);
                let offset = raw.read(cx).scroll_handle().0.borrow().base_handle.offset();
                assert!(offset.y < px(0.));
                window.press("down", cx);
                assert!(raw.read(cx).scroll_handle().0.borrow().base_handle.offset().y < offset.y);
                window.press("home", cx);
                window.press("right", cx);
                assert!(raw.read(cx).scroll_handle().0.borrow().base_handle.offset().x < px(0.));

                // Updates preserve the entity/viewport without materializing all bytes.
                window.press("end", cx);
                view.update(cx, |view, cx| view.refresh_details(window, cx));
                assert_eq!(raw, view.read(cx).payload_raw.as_ref().unwrap().clone());
                window.render_frame(cx);
                assert!(window.find(("payload-raw-row", last_offset)).visible());
                assert!(raw.read(cx).rendered_rows < 100);
                // Replacing a large payload with a short one clamps the viewport.
                view.update(cx, |view, cx| {
                    let mut incoming = message(Some(content_type));
                    incoming.payload = Bytes::from_static(b"short image");
                    view.topics.receive(incoming, Instant::now());
                    view.refresh_details(window, cx);
                });
                window.render_frame(cx);
                assert!(window.find(("payload-raw-row", 0usize)).visible());
                assert_eq!(raw.read(cx).byte_count(), 11);
                window.click("preview-value", cx);
                assert!(window.find("payload-image").visible());
                assert!(window.try_find("payload-raw").is_none());
                assert!(view.read(cx).payload_raw.is_none());
            })
            .unwrap();
            cx.run_until_parked();
        }
    }

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
    fn image_payloads_replace_the_editor_and_follow_message_content_type(cx: &mut TestAppContext) {
        const SVG: &[u8] =
            br##"<svg xmlns="http://www.w3.org/2000/svg" width="320" height="160"><rect width="320" height="160" fill="#ff0000"/></svg>"##;
        const SQUARE: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024"><path d="M0 0h1024v1024H0z"/></svg>"#;
        const WIDE: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" width="1536" height="512"><path d="M0 0h1536v512H0z"/></svg>"#;
        const TALL: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" width="512" height="1536"><path d="M0 0h512v1536H0z"/></svg>"#;
        const PNG: &[u8] = &[
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00,
            0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0, 0x1f, 0x00, 0x05, 0x00, 0x01, 0xff, 0x89, 0x99, 0x3d, 0x1d, 0x00, 0x00, 0x00, 0x00, 0x49,
            0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
        ];
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
                            let mut text_message = message(None);
                            text_message.topic = "text".into();
                            explorer.topics.receive(text_message, Instant::now());
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

            for (content_type, payload, expected_error) in [
                (Some(" image/svg+xml ; charset=utf-8"), SVG, None),
                (Some("IMAGE/PNG"), PNG, None),
                (
                    Some("image/svg+xml; charset=utf-8; profile=very-long-profile-name-for-the-original-image-content-type-badge"),
                    SVG,
                    None,
                ),
                (Some("image/svg+xml"), SQUARE, None),
                (Some("image/svg+xml"), WIDE, None),
                (Some("image/svg+xml"), TALL, None),
                (
                    Some("image/png"),
                    b"invalid image".as_slice(),
                    Some("Unable to display image payload"),
                ),
                (Some("image/vnd.example.custom"), SVG, Some("Unsupported image content type")),
                (None, SVG, None),
                (Some("application/json"), br#"{"on":true}"#.as_slice(), None),
                (Some("image/svg+xml"), SVG, None),
            ] {
                let previous_image = cx.update(|cx| view.read(cx).payload_image.clone());
                cx.update_window(handle, |_, window, cx| {
                    view.update(cx, |view, cx| {
                        let mut incoming = message(content_type);
                        incoming.payload = Bytes::from_static(payload);
                        view.topics.receive(incoming, Instant::now());
                        view.refresh_details(window, cx);
                    });
                    window.render_frame(cx);
                })
                .unwrap();
                cx.run_until_parked();
                for (width, height) in [(1200., 850.), (760., 540.)] {
                    cx.simulate_window_resize(handle, size(px(width), px(height)));
                    cx.run_until_parked();
                    cx.update_window(handle, |_, window, cx| {
                        window.render_frame(cx);
                        let explorer = view.read(cx);
                        let is_image =
                            content_type.is_some_and(|content_type| content_type.trim().to_ascii_lowercase().starts_with("image/"));
                        if is_image {
                            assert_eq!(explorer.payload_format, "Image");
                            let badge = window.find("payload-format");
                            assert_eq!(badge.label(), content_type);
                            assert!(badge.visible());
                            assert!(badge.bounds().right() <= window.find("topic-metadata").bounds().right());
                            assert!(explorer.payload.read(cx).value().is_empty());
                            assert!(window.try_find("payload-editor").is_none());
                            if let Some(expected_error) = expected_error {
                                let error = window.find("payload-image-error");
                                assert_eq!(error.label(), Some(expected_error));
                                assert!(error.visible());
                            } else {
                                assert!(
                                    window.try_find("payload-image-error").is_none(),
                                    "Unexpected image decode failure for {content_type:?}"
                                );
                                let image = explorer.payload_image.as_ref().unwrap().clone();
                                let preview = window.find("payload-image");
                                assert_eq!(preview.label(), Some("Latest topic payload image"));
                                assert!(preview.visible());
                                let bounds = preview.bounds();
                                let payload_bounds = window.find("payload").bounds();
                                assert!(bounds.size.width > px(0.) && bounds.size.height > px(0.));
                                assert!(bounds.left() >= payload_bounds.left() && bounds.right() <= payload_bounds.right());
                                assert!(bounds.top() >= payload_bounds.top() && bounds.bottom() <= payload_bounds.bottom());
                                assert_eq!(image.bytes(), payload);
                                let image_bounds = window.find(("payload-image-data", image.id())).bounds();
                                let decoded = image.get_render_image(window, cx).expect("Image payload must decode successfully");
                                assert!(
                                    image_bounds.left() >= bounds.left()
                                        && image_bounds.right() <= bounds.right()
                                        && image_bounds.top() >= bounds.top()
                                        && image_bounds.bottom() <= bounds.bottom(),
                                    "image {image_bounds:?} must fit viewport {bounds:?}"
                                );
                                let original = decoded.size(0).map(|dimension| px(u32::from(dimension) as f32));
                                let fitted = gpui_kit::ObjectFit::ScaleDown.get_bounds(image_bounds, decoded.size(0));
                                let scale = (bounds.size.width / original.width)
                                    .min(bounds.size.height / original.height)
                                    .min(1.);
                                assert!((fitted.size.width - original.width * scale).abs() <= px(0.01));
                                assert!((fitted.size.height - original.height * scale).abs() <= px(0.01));
                                assert!(fitted.size.width <= original.width && fitted.size.height <= original.height);
                                assert!((fitted.center().x - bounds.center().x).abs() <= px(0.01));
                                assert!((fitted.center().y - bounds.center().y).abs() <= px(0.01));
                            }
                        } else {
                            assert!(explorer.payload_image.is_none());
                            assert!(window.find("payload-editor").visible());
                            assert_ne!(explorer.payload_format, "Image");
                            assert_eq!(window.find("payload-format").label(), Some(explorer.payload_format));
                        }
                        if let Some(previous) = &previous_image {
                            assert!(!previous.is_asset_cached(cx), "Replaced images must leave the asset cache");
                        }
                    })
                    .unwrap();
                }
            }

            let image = cx.update(|cx| view.read(cx).payload_image.as_ref().unwrap().clone());
            cx.update_window(handle, |_, window, cx| {
                view.update(cx, |view, cx| view.refresh_details(window, cx));
                assert!(Arc::ptr_eq(&image, view.read(cx).payload_image.as_ref().unwrap()));
                window.render_frame(cx);
                window.press("down", cx);
            })
            .unwrap();
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                assert_eq!(window.find("selected-topic").label(), Some("text"));
                assert!(window.find("payload-editor").visible());
                assert!(view.read(cx).payload_image.is_none());
                assert!(!image.is_asset_cached(cx));
            })
            .unwrap();
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
                    assert_eq!(window.find("payload-format").label(), Some("JSON"));
                    assert_eq!(view.read(cx).payload.read(cx).value().as_str(), "{\n  \"on\": true\n}");
                })
                .unwrap();
            }
        }
    }
}
