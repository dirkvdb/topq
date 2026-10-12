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
        }
    }
}

impl Render for ExplorerPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
    use gpui_kit::component::{
        Theme, ThemeMode, TitleBar,
        diff::{DiffFileStatus, DiffLineRange, DiffSide},
    };
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{App, AppContext, Bounds, Context, Entity, TestAppContext, Window, WindowBounds, WindowOptions, px, size};
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

    fn receive_comparison_payload(
        view: &mut Explorer,
        topic: &str,
        payload: &[u8],
        content_type: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Explorer>,
    ) {
        let mut incoming = message(content_type);
        incoming.topic = topic.into();
        incoming.payload = Bytes::copy_from_slice(payload);
        view.topics.receive(incoming, Instant::now());
        view.sync_tree(cx);
        view.refresh_details(window, cx);
    }

    fn assert_loaded_comparison(view: &Entity<Explorer>, previous: &str, current: &str, language: &str, window: &mut Window, cx: &mut App) {
        window.render_frame(cx);
        assert!(window.find("payload-diff").visible());
        assert!(window.try_find("payload-diff-status").is_none());
        assert!(window.try_find("payload-editor").is_none());
        assert!(view.read(cx).payload_diff_enabled);
        assert_eq!(window.find("diff-value").checked(), Some(true));
        let diff = view.read(cx).payload_comparison.read(cx).diff().clone();
        let files = diff.read(cx).files();
        assert_eq!(files.len(), 1);
        assert_eq!(
            files[0].status(),
            if previous == current {
                DiffFileStatus::Unchanged
            } else {
                DiffFileStatus::Modified
            }
        );
        assert!(!files[0].is_binary());
        assert_eq!(files[0].has_changes(), previous != current);
        assert_eq!(files[0].language().as_str(), language);
        let path = files[0].path().clone();
        // Read both complete sources through the public selection API, not private line storage.
        for (side, expected) in [(DiffSide::Original, previous), (DiffSide::Modified, current)] {
            // Reference files provide only the current side when both payloads are identical.
            if previous == current && side == DiffSide::Original {
                continue;
            }
            diff.update(cx, |diff, cx| {
                diff.set_selected_lines(Some(DiffLineRange::new(path.clone(), side, 1, usize::MAX)), cx);
                // Display patches omit file-ending notices and terminate source rows.
                // The raw payload and toolbar Copy assertions below remain unchanged.
                let expected = if expected.ends_with('\n') {
                    expected.to_owned()
                } else {
                    format!("{expected}\n")
                };
                assert_eq!(diff.selected_text(cx), expected, "{side:?} display source");
            });
        }
        diff.update(cx, |diff, cx| diff.set_selected_lines(None, cx));
    }

    fn assert_comparison_status(view: &Entity<Explorer>, expected: &str, window: &mut Window, cx: &mut App) {
        window.render_frame(cx);
        assert!(window.find("payload-diff").visible());
        let status = window.find("payload-diff-status");
        assert!(status.visible());
        assert_eq!(status.label(), Some(expected));
        assert!(window.try_find("payload-editor").is_none());
        assert!(view.read(cx).payload_comparison.read(cx).diff().read(cx).files().is_empty());
    }

    #[gpui_kit::test]
    fn payload_diff_keeps_source_visible_while_comparisons_are_pending(cx: &mut TestAppContext) {
        let (handle, view) = cx.update(|cx| {
            gpui_kit::init(cx);
            crate::appearance::register_bundled(cx).unwrap();
            super::super::init(cx);
            gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                cx.new(|cx| {
                    let mut explorer = Explorer::with_settings(None, None, None, window, cx);
                    explorer.show_config = false;
                    explorer.selected = Some("test".into());
                    for (topic, payload) in [
                        ("test", "first\n"),
                        ("test", "second\n"),
                        ("other", "other first\n"),
                        ("other", "other second\n"),
                    ] {
                        receive_comparison_payload(&mut explorer, topic, payload.as_bytes(), None, window, cx);
                    }
                    explorer
                })
            })
            .unwrap()
        });
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.activate_window();
            window.render_frame(cx);
            window.click("diff-value", cx);
            // Before background work runs, first activation already shows the current source.
            assert_loaded_comparison(&view, "second\n", "second\n", "plaintext", window, cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            assert_loaded_comparison(&view, "first\n", "second\n", "plaintext", window, cx);
            // Keep the previous comparison visible through multiple superseding requests.
            for current in ["third\n", "fourth\n"] {
                view.update(cx, |view, cx| {
                    receive_comparison_payload(view, "test", current.as_bytes(), None, window, cx)
                });
                assert_loaded_comparison(&view, "first\n", "second\n", "plaintext", window, cx);
                window.click("copy-value", cx);
                assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), current);
            }
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            assert_loaded_comparison(&view, "third\n", "fourth\n", "plaintext", window, cx);
            view.update(cx, |view, cx| {
                receive_comparison_payload(view, "test", b"fourth\n", None, window, cx)
            });
            assert_loaded_comparison(&view, "fourth\n", "fourth\n", "plaintext", window, cx);
            view.update(cx, |view, cx| view.select_topic("other", window, cx));
            // A topic switch shows its own current source, never the previous topic's diff.
            assert_loaded_comparison(&view, "other second\n", "other second\n", "plaintext", window, cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            assert_loaded_comparison(&view, "other first\n", "other second\n", "plaintext", window, cx);
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn payload_diff_shows_all_unchanged_lines_after_each_live_update(cx: &mut TestAppContext) {
        let versions = [
            "old\none\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nold end\n",
            "new\none\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nnew end\n",
            "latest\none\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nlatest end\n",
        ];
        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            let (handle, view) = cx.update(|cx| {
                if !cx.has_global::<Theme>() {
                    gpui_kit::init(cx);
                    crate::appearance::register_bundled(cx).unwrap();
                    super::super::init(cx);
                }
                Theme::change(mode, None, cx);
                gpui_kit::open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(Bounds::new(Default::default(), size(px(900.), px(900.))))),
                        ..WindowOptions::default()
                    },
                    cx,
                    |window, cx| {
                        cx.new(|cx| {
                            let mut explorer = Explorer::with_settings(None, None, None, window, cx);
                            explorer.show_config = false;
                            explorer.selected = Some("test".into());
                            receive_comparison_payload(&mut explorer, "test", versions[0].as_bytes(), None, window, cx);
                            explorer
                        })
                    },
                )
                .unwrap()
            });
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| {
                window.activate_window();
                window.render_frame(cx);
                window.click("diff-value", cx);
            })
            .unwrap();
            for pair in versions.windows(2) {
                cx.update_window(handle, |_, window, cx| {
                    view.update(cx, |view, cx| {
                        receive_comparison_payload(view, "test", pair[1].as_bytes(), None, window, cx)
                    });
                })
                .unwrap();
                cx.run_until_parked();
                cx.update_window(handle, |_, window, cx| {
                    assert_loaded_comparison(&view, pair[0], pair[1], "plaintext", window, cx);
                    let snapshots = gpui_kit::base::test_support::snapshots(window);
                    let sources: Vec<_> = snapshots
                        .iter()
                        .filter(|snapshot| snapshot.path().last() == Some(&gpui_kit::ElementId::from("source")))
                        .collect();
                    assert_eq!(
                        sources.len(),
                        13,
                        "every source row, including both changed versions, must be displayed"
                    );
                    for text in ["one", "two", "three", "four", "five", "six", "seven", "eight", "nine"] {
                        assert!(
                            sources.iter().any(|row| row.label() == Some(text) && row.visible()),
                            "missing unchanged line {text}"
                        );
                    }
                    assert!(
                        !snapshots
                            .iter()
                            .any(|snapshot| snapshot.label().is_some_and(|label| label.contains("unchanged lines")))
                    );
                })
                .unwrap();
            }
        }
    }

    #[gpui_kit::test]
    fn payload_diff_font_metrics_and_top_inset_match_editor_across_themes_and_zoom(cx: &mut TestAppContext) {
        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            for (font_size, mono_font_size) in [(14., 12.), (18., 16.)] {
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
                            receive_comparison_payload(&mut explorer, "test", br#"{"reading":1}"#, None, window, cx);
                            receive_comparison_payload(&mut explorer, "test", br#"{"reading":2}"#, None, window, cx);
                            explorer
                        })
                    })
                    .inspect(|_| {
                        Theme::update(cx, |theme| {
                            theme.font_size = px(font_size);
                            theme.mono_font_size = px(mono_font_size);
                        });
                    })
                    .unwrap()
                });
                cx.run_until_parked();
                let (editor_inset, editor_character_size) = cx
                    .update_window(handle, |_, window, cx| {
                        window.activate_window();
                        window.render_frame(cx);
                        let first_row = view.read(cx).payload.read(cx).range_to_bounds(&(0..1)).unwrap();
                        let inset = first_row.top() - window.find("payload-editor").bounds().top();
                        window.click("diff-value", cx);
                        (inset, first_row.size)
                    })
                    .unwrap();
                cx.run_until_parked();
                cx.update_window(handle, |_, window, cx| {
                    window.render_frame(cx);
                    let first_row = gpui_kit::base::test_support::snapshots(window)
                        .into_iter()
                        .find(|snapshot| {
                            snapshot.label() == Some("{") && snapshot.path().last() == Some(&gpui_kit::ElementId::from("source"))
                        })
                        .expect("the diff's first source row should be rendered");
                    let diff_inset = first_row.bounds().top() - window.find("payload-diff").bounds().top();
                    assert_eq!(diff_inset, editor_inset, "first-row inset in {mode:?} at {font_size}px");
                    // The source label rounds its layout width up; editor range geometry stays fractional.
                    assert_eq!(
                        first_row.bounds().size.width,
                        px(f32::from(editor_character_size.width).ceil()),
                        "character width in {mode:?} at {font_size}px"
                    );
                    assert_eq!(
                        first_row.bounds().size.height,
                        editor_character_size.height,
                        "line height in {mode:?} at {font_size}px"
                    );
                })
                .unwrap();
            }
        }
    }

    #[gpui_kit::test]
    fn payload_diff_toggle_supports_pointer_and_space_and_explains_the_first_message(cx: &mut TestAppContext) {
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
                        receive_comparison_payload(&mut explorer, "test", b"first payload", None, window, cx);
                        explorer
                    })
                })
                .unwrap()
            });
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| {
                window.activate_window();
                window.render_frame(cx);
                assert!(!view.read(cx).payload_diff_enabled);
                assert!(view.read(cx).topics.nodes["test"].previous_value().is_none());
                let toggle = window.find("diff-value");
                assert!(toggle.visible());
                assert_eq!(toggle.checked(), Some(false));
                assert_eq!(toggle.label(), Some("Compare with previous payload"));
                let preview = window.find("preview-value").bounds();
                let diff = toggle.bounds();
                let edit = window.find("edit-value").bounds();
                assert!(preview.right() <= diff.left());
                assert!(diff.right() <= edit.left());
                assert_eq!(preview.center().y, diff.center().y);
                assert_eq!(diff.center().y, edit.center().y);
                assert_eq!(diff.size.width, preview.size.width, "Diff must remain an icon-only toolbar toggle");
                assert_eq!(diff.left() - preview.right(), edit.left() - diff.right());
                assert!(window.find("payload-editor").visible());
                assert!(window.try_find("payload-diff").is_none());

                window.click("diff-value", cx);
                assert!(view.read(cx).payload_diff_enabled);
                assert_eq!(window.find("diff-value").checked(), Some(true));
                assert_eq!(window.find("diff-value").label(), Some("Show latest payload"));
                assert_eq!(window.find("diff-value").focused(), Some(true));
            })
            .unwrap();
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| {
                assert_comparison_status(&view, "No previous payload. Waiting for the next message.", window, cx);
                window.press("space", cx);
                assert!(!view.read(cx).payload_diff_enabled);
                assert_eq!(window.find("diff-value").checked(), Some(false));
                assert_eq!(window.find("diff-value").label(), Some("Compare with previous payload"));
                assert!(window.find("payload-editor").visible());
                assert_eq!(view.read(cx).payload.read(cx).value().as_str(), "first payload");
                assert!(window.try_find("payload-diff").is_none());
                window.press("space", cx);
                assert!(view.read(cx).payload_diff_enabled);
                assert_eq!(window.find("diff-value").checked(), Some(true));
                assert_eq!(window.find("diff-value").label(), Some("Show latest payload"));
            })
            .unwrap();
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| {
                assert_comparison_status(&view, "No previous payload. Waiting for the next message.", window, cx);
            })
            .unwrap();
        }
    }

    #[gpui_kit::test]
    fn payload_diff_rolls_over_live_and_retains_history_while_another_topic_is_selected(cx: &mut TestAppContext) {
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
                        receive_comparison_payload(&mut explorer, "test", b"first\n", None, window, cx);
                        receive_comparison_payload(&mut explorer, "other", b"other first\n", None, window, cx);
                        receive_comparison_payload(&mut explorer, "other", b"other second\n", None, window, cx);
                        explorer
                    })
                })
                .unwrap()
            });
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| {
                window.activate_window();
                window.render_frame(cx);
                window.click("diff-value", cx);
            })
            .unwrap();
            cx.run_until_parked();
            for (previous, current) in [("first\n", "second\n"), ("second\n", "third\n")] {
                cx.update_window(handle, |_, window, cx| {
                    view.update(cx, |view, cx| {
                        receive_comparison_payload(view, "test", current.as_bytes(), None, window, cx);
                    });
                })
                .unwrap();
                cx.run_until_parked();
                cx.update_window(handle, |_, window, cx| {
                    assert_loaded_comparison(&view, previous, current, "plaintext", window, cx);
                    assert_eq!(
                        view.read(cx).topics.nodes["test"].previous_value().unwrap().payload.as_ref(),
                        previous.as_bytes()
                    );
                })
                .unwrap();
            }
            cx.update_window(handle, |_, window, cx| {
                view.update(cx, |view, cx| {
                    receive_comparison_payload(view, "test", b"third\n", None, window, cx)
                });
            })
            .unwrap();
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| {
                assert_loaded_comparison(&view, "third\n", "third\n", "plaintext", window, cx);
                window.click("topic:other", cx);
            })
            .unwrap();
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| {
                assert_eq!(view.read(cx).selected.as_deref(), Some("other"));
                assert_loaded_comparison(&view, "other first\n", "other second\n", "plaintext", window, cx);
            })
            .unwrap();
            for current in [b"fourth\n".as_slice(), b"fifth\n".as_slice()] {
                cx.update_window(handle, |_, window, cx| {
                    view.update(cx, |view, cx| receive_comparison_payload(view, "test", current, None, window, cx));
                })
                .unwrap();
                cx.run_until_parked();
                cx.update_window(handle, |_, window, cx| {
                    assert_eq!(view.read(cx).selected.as_deref(), Some("other"));
                    assert_loaded_comparison(&view, "other first\n", "other second\n", "plaintext", window, cx);
                    assert_eq!(view.read(cx).payload.read(cx).value().as_str(), "other second\n");
                })
                .unwrap();
            }
            cx.update_window(handle, |_, window, cx| {
                view.update(cx, |view, cx| {
                    receive_comparison_payload(view, "other", b"other third\n", None, window, cx)
                });
            })
            .unwrap();
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| {
                assert_loaded_comparison(&view, "other second\n", "other third\n", "plaintext", window, cx);
                window.click("topic:test", cx);
            })
            .unwrap();
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| {
                assert_eq!(view.read(cx).selected.as_deref(), Some("test"));
                assert_loaded_comparison(&view, "fourth\n", "fifth\n", "plaintext", window, cx);
                assert_eq!(
                    view.read(cx).topics.nodes["test"].previous_value().unwrap().payload.as_ref(),
                    b"fourth\n"
                );
                assert_eq!(
                    view.read(cx).topics.nodes["other"].previous_value().unwrap().payload.as_ref(),
                    b"other second\n"
                );
            })
            .unwrap();
            cx.run_until_parked();
        }
    }

    #[gpui_kit::test]
    fn payload_diff_follows_preview_formatting_and_always_copies_the_latest_raw_payload(cx: &mut TestAppContext) {
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
                        explorer
                    })
                })
                .unwrap()
            });
            for (topic, previous_raw, current_raw, previous_preview, current_preview, language) in [
                (
                    "json",
                    "{\"on\":false}",
                    "{\"on\":true}",
                    "{\n  \"on\": false\n}",
                    "{\n  \"on\": true\n}",
                    "json",
                ),
                (
                    "ansi",
                    "\x1b[31mold é\x1b[0m",
                    "\x1b[32mnew 世界\x1b[0m",
                    "old é",
                    "new 世界",
                    "plaintext",
                ),
                (
                    "whitespace",
                    "{\"on\":true}",
                    "{ \"on\": true }",
                    "{\n  \"on\": true\n}",
                    "{\n  \"on\": true\n}",
                    "json",
                ),
            ] {
                cx.update_window(handle, |_, window, cx| {
                    window.activate_window();
                    view.update(cx, |view, cx| {
                        receive_comparison_payload(view, topic, previous_raw.as_bytes(), None, window, cx);
                        receive_comparison_payload(view, topic, current_raw.as_bytes(), None, window, cx);
                        view.select_topic(topic, window, cx);
                    });
                    window.render_frame(cx);
                    if !view.read(cx).payload_diff_enabled {
                        window.click("diff-value", cx);
                    }
                })
                .unwrap();
                cx.run_until_parked();
                for preview in [true, false, true] {
                    cx.update_window(handle, |_, window, cx| {
                        window.render_frame(cx);
                        if view.read(cx).payload_preview != preview {
                            window.click("preview-value", cx);
                        }
                    })
                    .unwrap();
                    cx.run_until_parked();
                    cx.update_window(handle, |_, window, cx| {
                        assert_eq!(view.read(cx).payload_preview, preview);
                        assert_eq!(window.find("preview-value").checked(), Some(preview));
                        let (previous, current, language) = if preview {
                            (previous_preview, current_preview, language)
                        } else {
                            (previous_raw, current_raw, "plaintext")
                        };
                        assert_loaded_comparison(&view, previous, current, language, window, cx);
                        assert!(view.read(cx).payload_diff_enabled);
                        assert_eq!(window.find("diff-value").checked(), Some(true));
                        assert_eq!(view.read(cx).payload.read(cx).value().as_str(), current);
                        window.click("copy-value", cx);
                        assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), current_raw);
                    })
                    .unwrap();
                    cx.run_until_parked();
                }
            }
        }
    }

    #[gpui_kit::test]
    fn payload_diff_is_suppressed_for_binary_and_images_and_explains_a_previous_binary_payload(cx: &mut TestAppContext) {
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
                        receive_comparison_payload(&mut explorer, "test", b"initial text", None, window, cx);
                        explorer
                    })
                })
                .unwrap()
            });
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| {
                window.activate_window();
                window.render_frame(cx);
                window.click("diff-value", cx);
            })
            .unwrap();
            cx.run_until_parked();
            for (content_type, payload, inferred) in [
                (None, b"\xff\x00A".as_slice(), None),
                (None, b"%PDF-1.7\n".as_slice(), Some("application/pdf")),
                (None, b"GIF89a".as_slice(), Some("image/gif")),
                (
                    Some("image/svg+xml"),
                    b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"1\" height=\"1\"/>".as_slice(),
                    None,
                ),
                (Some("image/png"), b"invalid image".as_slice(), None),
            ] {
                cx.update_window(handle, |_, window, cx| {
                    view.update(cx, |view, cx| {
                        receive_comparison_payload(view, "test", payload, content_type, window, cx)
                    });
                })
                .unwrap();
                cx.run_until_parked();
                for preview in [true, false] {
                    cx.update_window(handle, |_, window, cx| {
                        window.render_frame(cx);
                        if view.read(cx).payload_preview != preview {
                            window.click("preview-value", cx);
                        }
                    })
                    .unwrap();
                    cx.run_until_parked();
                    cx.update_window(handle, |_, window, cx| {
                        window.render_frame(cx);
                        assert_eq!(
                            view.read(cx).topics.nodes["test"].value.as_ref().unwrap().inferred_content_type(),
                            inferred
                        );
                        assert!(view.read(cx).payload_diff_enabled);
                        assert!(window.try_find("diff-value").is_none());
                        assert!(window.try_find("payload-diff").is_none());
                        assert!(window.try_find("payload-diff-status").is_none());
                        assert!(view.read(cx).payload_comparison.read(cx).diff().read(cx).files().is_empty());
                    })
                    .unwrap();
                }
                cx.update_window(handle, |_, window, cx| {
                    view.update(cx, |view, cx| {
                        receive_comparison_payload(view, "test", b"now text", None, window, cx)
                    });
                })
                .unwrap();
                cx.run_until_parked();
                cx.update_window(handle, |_, window, cx| {
                    assert_comparison_status(&view, "The previous payload is binary and cannot be compared as text.", window, cx);
                    assert_eq!(window.find("diff-value").checked(), Some(true));
                    assert_eq!(window.find("diff-value").label(), Some("Show latest payload"));
                    assert_eq!(
                        view.read(cx).topics.nodes["test"].previous_value().unwrap().payload.as_ref(),
                        payload
                    );
                    window.click("copy-value", cx);
                    assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), "now text");
                    view.update(cx, |view, cx| {
                        receive_comparison_payload(view, "test", b"next text", None, window, cx)
                    });
                })
                .unwrap();
                cx.run_until_parked();
                cx.update_window(handle, |_, window, cx| {
                    assert_loaded_comparison(&view, "now text", "next text", "plaintext", window, cx);
                })
                .unwrap();
                cx.run_until_parked();
            }
        }
    }

    #[gpui_kit::test]
    fn payload_diff_rebuilds_when_a_topic_is_deleted_and_recreated_before_refresh(cx: &mut TestAppContext) {
        let (handle, view) = cx.update(|cx| {
            gpui_kit::init(cx);
            crate::appearance::register_bundled(cx).unwrap();
            super::super::init(cx);
            gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                cx.new(|cx| {
                    let mut explorer = Explorer::with_settings(None, None, None, window, cx);
                    explorer.show_config = false;
                    explorer.selected = Some("test".into());
                    receive_comparison_payload(&mut explorer, "test", b"old first\n", None, window, cx);
                    receive_comparison_payload(&mut explorer, "test", b"old second\n", None, window, cx);
                    explorer
                })
            })
            .unwrap()
        });
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.activate_window();
            window.render_frame(cx);
            window.click("diff-value", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            assert_loaded_comparison(&view, "old first\n", "old second\n", "plaintext", window, cx);
            view.update(cx, |view, cx| {
                // Several queued messages can be applied before the next detail refresh.
                for payload in [b"".as_slice(), b"new first\n", b"new second\n"] {
                    let mut incoming = message(None);
                    incoming.payload = Bytes::copy_from_slice(payload);
                    view.topics.receive(incoming, Instant::now());
                }
                assert_eq!(view.topics.nodes["test"].value.as_ref().unwrap().messages, 2);
                view.refresh_details(window, cx);
            });
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            assert_loaded_comparison(&view, "new first\n", "new second\n", "plaintext", window, cx);
        })
        .unwrap();
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
        // One grayscale pixel, with single-symbol DC/AC Huffman tables.
        const JPEG: &[u8] = b"\xff\xd8\xff\xe0\x00\x10JFIF\x00\x01\x01\x00\x00\x01\x00\x01\x00\x00\xff\xdb\x00\x43\x00\
            \x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\
            \x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\
            \x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\
            \x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\
            \xff\xc0\x00\x0b\x08\x00\x01\x00\x01\x01\x01\x11\x00\xff\xc4\x00\x26\x00\x01\
            \x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x10\x01\
            \x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\
            \xff\xda\x00\x08\x01\x01\x00\x00\x3f\x00\x3f\xff\xd9";
        const CORRUPT_PNG: &[u8] = b"\x89PNG\r\n\x1a\n";
        const PSD: &[u8] = b"8BPS\x00\x01\x00\x00\x00\x00\x00\x00\x00\x01\x00\x00\x00\x01\x00\x00\x00\x01\x00\x08\x00\x01";
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

            // Expected badges and editor text are fixture data, independent of MIME inference.
            for (content_type, payload, expected_badge, expected_error, expected_text) in [
                (
                    Some(" image/svg+xml ; charset=utf-8"),
                    SVG,
                    " image/svg+xml ; charset=utf-8",
                    None,
                    None,
                ),
                (Some("IMAGE/PNG"), PNG, "IMAGE/PNG", None, None),
                (None, PNG, "image/png", None, None),
                (Some("application/octet-stream"), PNG, "image/png", None, None),
                (Some("text/plain"), PNG, "image/png", None, None),
                (None, JPEG, "image/jpeg", None, None),
                (
                    Some("image/svg+xml; charset=utf-8; profile=very-long-profile-name-for-the-original-image-content-type-badge"),
                    SVG,
                    "image/svg+xml; charset=utf-8; profile=very-long-profile-name-for-the-original-image-content-type-badge",
                    None,
                    None,
                ),
                (Some("image/svg+xml"), SQUARE, "image/svg+xml", None, None),
                (Some("image/svg+xml"), WIDE, "image/svg+xml", None, None),
                (Some("image/svg+xml"), TALL, "image/svg+xml", None, None),
                (
                    Some("image/png"),
                    b"invalid image".as_slice(),
                    "image/png",
                    Some("Unable to display image payload"),
                    None,
                ),
                (None, CORRUPT_PNG, "image/png", Some("Unable to display image payload"), None),
                (
                    Some("application/octet-stream"),
                    CORRUPT_PNG,
                    "image/png",
                    Some("Unable to display image payload"),
                    None,
                ),
                (Some("image/jpeg"), PNG, "image/jpeg", Some("Unable to display image payload"), None),
                (
                    Some("image/vnd.example.custom"),
                    PNG,
                    "image/vnd.example.custom",
                    Some("Unsupported image content type"),
                    None,
                ),
                (None, PSD, "image/vnd.adobe.photoshop", Some("Unsupported image content type"), None),
                (None, b"plain payload".as_slice(), "Text", None, Some("plain payload")),
                (None, b"\xff\x00A".as_slice(), "Binary · hex", None, Some("ff 00 41")),
                (
                    Some("application/octet-stream"),
                    b"\xff\x00A".as_slice(),
                    "Binary · hex",
                    None,
                    Some("ff 00 41"),
                ),
                (None, SVG, "Text", None, Some(std::str::from_utf8(SVG).unwrap())),
                (
                    Some("application/json"),
                    br#"{"on":true}"#.as_slice(),
                    "JSON",
                    None,
                    Some("{\n  \"on\": true\n}"),
                ),
                (None, b"%PDF-1.7\n".as_slice(), "Text", None, Some("%PDF-1.7\n")),
                (
                    None,
                    b"%PDF-1.7\n\xff".as_slice(),
                    "Binary · hex",
                    None,
                    Some("25 50 44 46 2d 31 2e 37 0a ff"),
                ),
                (
                    Some(" APPLICATION/OCTET-STREAM ; charset=binary"),
                    b"%PDF-1.7\n\xff".as_slice(),
                    "Binary · hex",
                    None,
                    Some("25 50 44 46 2d 31 2e 37 0a ff"),
                ),
                (Some("image/svg+xml"), SVG, "image/svg+xml", None, None),
            ] {
                let payload = Bytes::copy_from_slice(payload);
                let previous_image = cx.update(|cx| view.read(cx).payload_image.clone());
                cx.update_window(handle, |_, window, cx| {
                    view.update(cx, |view, cx| {
                        let mut incoming = message(content_type);
                        incoming.payload = payload.clone();
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
                        if expected_text.is_none() {
                            assert_eq!(explorer.payload_format, "Image");
                            assert!(window.try_find("message-content-type").is_none());
                            let badge = window.find("payload-format");
                            assert_eq!(badge.label(), Some(expected_badge));
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
                                assert_eq!(image.bytes(), payload.as_ref());
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
                        } else if let Some(expected_text) = expected_text {
                            assert!(explorer.payload_image.is_none());
                            assert!(window.find("payload-editor").visible());
                            assert_eq!(explorer.payload_format, expected_badge);
                            assert_eq!(window.find("payload-format").label(), Some(expected_badge));
                            assert_eq!(explorer.payload.read(cx).value().as_str(), expected_text);
                            assert!(window.try_find("payload-image").is_none());
                            assert!(window.try_find("payload-image-error").is_none());
                            if payload.starts_with(b"%PDF-") {
                                assert_eq!(window.find("message-content-type").label(), Some("Content type: application/pdf"));
                                assert_eq!(window.find("message-content-type-value").label(), Some("application/pdf"));
                                assert!(window.find("message-content-type").visible());
                                assert_eq!(
                                    explorer.topics.nodes["test"].value.as_ref().unwrap().properties.content_type(),
                                    content_type
                                );
                            }
                        }
                        let explorer = view.read(cx);
                        assert!(explorer.payload_raw.is_none());
                        if let Some(previous) = &previous_image
                            && explorer
                                .payload_image
                                .as_ref()
                                .is_none_or(|current| !Arc::ptr_eq(previous, current))
                        {
                            assert!(!previous.is_asset_cached(cx), "Replaced images must leave the asset cache");
                        }
                    })
                    .unwrap();
                }
                if expected_text.is_none() {
                    cx.update_window(handle, |_, window, cx| {
                        window.click("preview-value", cx);
                        let explorer = view.read(cx);
                        assert!(!explorer.payload_preview);
                        assert_eq!(window.find("preview-value").label(), Some("Render payload"));
                        assert_eq!(window.find("payload-format").label(), Some(expected_badge));
                        assert!(window.find("payload-raw").visible());
                        assert!(window.try_find("payload-editor").is_none());
                        assert!(window.try_find("payload-image").is_none());
                        assert!(window.try_find("payload-image-error").is_none());
                        assert!(explorer.payload_image.is_none());
                        assert!(explorer.payload.read(cx).value().is_empty());
                        let raw = explorer.payload_raw.as_ref().unwrap().read(cx);
                        assert_eq!(raw.bytes(), &payload);
                        assert_eq!(raw.bytes().as_ptr(), payload.as_ptr(), "Raw view must share inferred image bytes");
                        if let Some(previous) = &previous_image {
                            assert!(!previous.is_asset_cached(cx));
                        }
                        window.click("preview-value", cx);
                        assert!(view.read(cx).payload_preview);
                        assert_eq!(window.find("preview-value").label(), Some("Show raw payload"));
                        assert!(window.try_find("payload-raw").is_none());
                        assert!(view.read(cx).payload_raw.is_none());
                    })
                    .unwrap();
                    cx.run_until_parked();
                    cx.update_window(handle, |_, window, cx| {
                        window.render_frame(cx);
                        if let Some(error) = expected_error {
                            assert_eq!(window.find("payload-image-error").label(), Some(error));
                        } else {
                            assert!(window.find("payload-image").visible());
                            assert!(window.try_find("payload-image-error").is_none());
                            assert!(
                                view.read(cx)
                                    .payload_image
                                    .as_ref()
                                    .unwrap()
                                    .clone()
                                    .get_render_image(window, cx)
                                    .is_some()
                            );
                        }
                    })
                    .unwrap();
                }
            }

            let image = cx.update(|cx| view.read(cx).payload_image.as_ref().unwrap().clone());
            cx.update_window(handle, |_, window, cx| {
                view.update(cx, |view, cx| {
                    view.refresh_details(window, cx);
                    view.focus_topics(window, cx);
                });
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
