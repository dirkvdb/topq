//! Image previews and live text refreshes with viewport preservation and semantic JSON change highlights.

use std::{ops::Range, sync::Arc, time::Instant};

use gpui_kit::component::input::{Editor, RangeDecoration, RangeDecorationCollection, RangeDecorationStyle};
use gpui_kit::{AnyElement, Image, ImageFormat, ObjectFit, StyledImage, TestSupportExt, div, img, prelude::*, relative};

use super::{
    ActiveTheme, App, AssetIconName, Button, ButtonVariants, Context, DropdownMenu, Explorer, PopupMenuItem, Sizable, StyledExt, Window,
    monitor, payload_diff,
};
use crate::{
    appearance::Appearance,
    topics::{FLASH_DURATION, flash_amount},
};

pub(super) struct PayloadHighlight {
    collection: RangeDecorationCollection,
    ranges: Vec<Range<usize>>,
    pulse: Option<(Instant, f32)>,
}

impl PayloadHighlight {
    pub(super) fn new(collection: RangeDecorationCollection) -> Self {
        Self {
            collection,
            ranges: Vec::new(),
            pulse: None,
        }
    }

    fn amount(&self, now: Instant) -> f32 {
        self.pulse
            .map_or(0., |(started, from)| flash_amount(now.saturating_duration_since(started), from))
    }

    fn set(&mut self, ranges: Vec<Range<usize>>, now: Instant, cx: &mut App) {
        if ranges.is_empty() || Appearance::motion_reduced(cx) {
            self.clear(cx);
            return;
        }
        let from = self.amount(now);
        self.ranges = ranges;
        self.pulse = Some((now, from));
        self.refresh(now, cx);
    }

    fn clear(&mut self, cx: &mut App) {
        self.ranges.clear();
        self.pulse = None;
        self.collection.clear(cx);
    }

    #[hotpath::measure(impl_type = "PayloadHighlight")]
    pub(super) fn refresh(&mut self, now: Instant, cx: &mut App) {
        let Some((started, _)) = self.pulse else { return };
        if Appearance::motion_reduced(cx) || now.saturating_duration_since(started) >= FLASH_DURATION {
            self.clear(cx);
            return;
        }
        let color = cx.theme().warning.opacity(0.25 * self.amount(now));
        self.collection.set(
            self.ranges
                .iter()
                .map(|range| {
                    RangeDecoration::new(range.clone())
                        .with_style(RangeDecorationStyle::Fill)
                        .with_color(color)
                })
                .collect(),
            cx,
        );
    }

    pub(super) fn is_active(&self) -> bool {
        self.pulse.is_some()
    }

    #[cfg(test)]
    pub(super) fn ranges(&self, cx: &App) -> Vec<Range<usize>> {
        self.collection.get_ranges(cx)
    }
}

fn image_content_type(content_type: Option<&str>) -> Option<&str> {
    let mime_type = content_type?.split(';').next()?.trim();
    let (kind, subtype) = mime_type.split_once('/')?;
    (kind.eq_ignore_ascii_case("image") && !subtype.is_empty()).then_some(mime_type)
}

fn image_status(id: &'static str, message: &'static str, color: gpui_kit::Hsla) -> AnyElement {
    div()
        .id(id)
        .test_support()
        .aria_label(message)
        .v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .text_sm()
        .text_color(color)
        .child(message)
        .into_any_element()
}

impl Explorer {
    pub(super) fn start_monitoring(&mut self, pointer: &str, cx: &mut Context<Self>) {
        let Some(topic) = self.selected.as_ref() else { return };
        let Some(value) = self.topics.nodes.get(topic).and_then(|node| node.value.as_ref()) else {
            return;
        };
        if self.monitoring.start(topic, pointer, &value.payload, value.received_at) {
            self.details_view.update(cx, |_, cx| cx.notify());
        }
    }

    pub(super) fn toggle_monitor_chart(&mut self, topic: &str, pointer: &str, cx: &mut Context<Self>) {
        if self.monitoring.toggle_chart(topic, pointer) {
            self.details_view.update(cx, |_, cx| cx.notify());
        }
    }

    pub(super) fn set_monitor_smoothing(&mut self, topic: &str, pointer: &str, smooth: bool, cx: &mut Context<Self>) {
        if self.monitoring.set_smoothing(topic, pointer, smooth) {
            self.details_view.update(cx, |_, cx| cx.notify());
        }
    }

    pub(super) fn stop_monitoring(&mut self, topic: &str, pointer: &str, cx: &mut Context<Self>) {
        if self.monitoring.stop(topic, pointer) {
            self.details_view.update(cx, |_, cx| cx.notify());
        }
    }

    pub(super) fn monitor_field_menu(&self, cx: &Context<Self>) -> impl IntoElement {
        let view = cx.weak_entity();
        Button::new("monitor-field-menu")
            .ghost()
            .small()
            .label("Monitor field")
            .dropdown_caret(true)
            .dropdown_menu(move |mut menu, _, cx| {
                let Some(view) = view.upgrade() else { return menu };
                let explorer = view.read(cx);
                for field in &explorer.numeric_fields {
                    let pointer = field.pointer().to_owned();
                    let monitored = explorer
                        .selected
                        .as_ref()
                        .is_some_and(|topic| explorer.monitoring.contains(topic, &pointer));
                    let owner = view.downgrade();
                    menu = menu.item(
                        PopupMenuItem::new(if pointer.is_empty() { "(root)".to_owned() } else { pointer.clone() })
                            .checked(monitored)
                            .on_click(move |_, _, cx| {
                                let _ = owner.update(cx, |view, cx| view.start_monitoring(&pointer, cx));
                            }),
                    );
                }
                menu
            })
    }

    pub(super) fn payload_view(&self, cx: &Context<Self>) -> AnyElement {
        if let Some(raw) = &self.payload_raw {
            return raw.clone().into_any_element();
        }
        if self.payload_preview && self.payload_format == "Image" {
            let color = cx.theme().muted_foreground;
            let Some(image) = &self.payload_image else {
                return image_status("payload-image-error", "Unsupported image content type", color);
            };
            return div()
                .id("payload-image")
                .role(gpui_kit::Role::Image)
                .test_support()
                .aria_label("Latest topic payload image")
                .relative()
                .overflow_hidden()
                .size_full()
                .min_w_0()
                .min_h_0()
                .child(
                    img(image.clone())
                        .id(("payload-image-data", image.id()))
                        .absolute()
                        .inset_0()
                        .size_full()
                        .object_fit(ObjectFit::ScaleDown)
                        .with_loading(move || image_status("payload-image-loading", "Loading image…", color))
                        .with_fallback(move || image_status("payload-image-error", "Unable to display image payload", color))
                        .test_support(),
                )
                .into_any_element();
        }
        let numeric = !self.numeric_fields.is_empty();
        let hovered = self
            .hovered_field
            .as_ref()
            .and_then(|pointer| self.numeric_fields.iter().find(|field| field.pointer() == pointer));
        let button = hovered.and_then(|field| {
            let editor = self.payload.read(cx);
            let bounds = editor.range_to_bounds(&(field.offset()..field.offset() + 1))?;
            let pointer = field.pointer().to_owned();
            let field_name = if pointer.is_empty() {
                "(root)"
            } else {
                pointer.strip_prefix('/').unwrap_or(&pointer)
            };
            let monitored = self
                .selected
                .as_ref()
                .is_some_and(|topic| self.monitoring.contains(topic, &pointer));
            // Anchor in the same window coordinates as the text, not the editor's inset content origin.
            // The slot follows indentation and shares the measured row height for exact centering.
            let position = gpui_kit::point(bounds.left() - gpui_kit::rems(1.5).to_pixels(cx.theme().font_size), bounds.top());
            Some(
                gpui_kit::anchored().position(position).child(
                    div().h_flex().justify_center().w_5().h(bounds.size.height).child(
                        Button::new("monitor-hovered-field")
                            .ghost()
                            .xsmall()
                            .icon(AssetIconName::ChartLine)
                            .accessibility_label(format!("Monitor JSON field {field_name}"))
                            .tooltip(if monitored { "Already monitored" } else { "Monitor field" })
                            .on_click(cx.listener(move |view, _, _, cx| view.start_monitoring(&pointer, cx))),
                    ),
                ),
            )
        });
        let owner = cx.weak_entity();
        div()
            .id("payload-editor")
            .test_support()
            .relative()
            .size_full()
            .when(numeric, |container| container.pl_6())
            .child(
                Editor::new(&self.payload)
                    .readonly(true)
                    .h(relative(1.))
                    .w_full()
                    .text_sm()
                    .aria_label("Latest topic payload"),
            )
            .when(numeric, |container| {
                container.child(
                    // Observe capture without a hitbox: the editor owns text selection and may stop bubbling.
                    gpui_kit::canvas(
                        |_, _, _| (),
                        move |viewport, _, window, _| {
                            let scroll_owner = owner.clone();
                            window.on_mouse_event(move |event: &gpui_kit::ScrollWheelEvent, phase, _, cx| {
                                if !phase.bubble() && viewport.contains(&event.position) {
                                    let _ = scroll_owner.update(cx, |view, cx| {
                                        if view.hovered_field.take().is_some() {
                                            view.details_view.update(cx, |_, cx| cx.notify());
                                        }
                                    });
                                }
                            });
                            window.on_mouse_event(move |event: &gpui_kit::MouseMoveEvent, phase, _, cx| {
                                if phase.bubble() {
                                    return;
                                }
                                let _ = owner.update(cx, |view, cx| {
                                    let editor = view.payload.read(cx);
                                    let hovered = viewport
                                        .contains(&event.position)
                                        .then(|| {
                                            view.numeric_fields.iter().find_map(|field| {
                                                let bounds = editor.range_to_bounds(&(field.offset()..field.offset() + 1))?;
                                                (event.position.y >= bounds.origin.y && event.position.y < bounds.bottom())
                                                    .then(|| field.pointer().to_owned())
                                            })
                                        })
                                        .flatten();
                                    if hovered != view.hovered_field {
                                        view.hovered_field = hovered;
                                        view.details_view.update(cx, |_, cx| cx.notify());
                                    }
                                });
                            });
                        },
                    )
                    .absolute()
                    .inset_0(),
                )
            })
            .when_some(button, |container, button| container.child(button))
            .into_any_element()
    }

    pub(super) fn clear_payload_highlights(&mut self, cx: &mut App) {
        self.payload_highlight.clear(cx);
    }

    #[hotpath::measure(impl_type = "Explorer")]
    pub(super) fn refresh_details(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let value = self
            .selected
            .as_ref()
            .and_then(|path| self.topics.nodes.get(path))
            .and_then(|node| node.value.as_ref());
        let same_topic = self.payload_topic == self.selected;
        let image_type = value.and_then(|value| image_content_type(value.properties.content_type()));
        // Images stay as shared bytes in raw mode. Never decode the complete payload
        // into a text editor: binary data can produce enormous logical lines.
        let raw_bytes = value.filter(|_| !self.payload_preview && image_type.is_some());
        if let Some(value) = raw_bytes {
            if same_topic && let Some(raw) = &self.payload_raw {
                raw.update(cx, |raw, cx| raw.set_bytes(value.payload.clone(), cx));
            } else {
                self.payload_raw = Some(cx.new(|cx| super::payload_raw::RawPayload::new(value.payload.clone(), cx)));
            }
        } else {
            self.payload_raw = None;
        }
        let image_format = image_type.and_then(|mime_type| ImageFormat::from_mime_type(&mime_type.to_ascii_lowercase()));
        let image_value = value.zip(image_format).filter(|_| self.payload_preview);
        let same_image = match (&self.payload_image, image_value) {
            (Some(image), Some((value, format))) => image.format() == format && image.bytes() == value.payload.as_ref(),
            (None, None) => true,
            _ => false,
        };
        if !same_image {
            if let Some(previous) = self.payload_image.take() {
                previous.remove_asset(cx);
            }
            self.payload_image = image_value.map(|(value, format)| Arc::new(Image::from_bytes(format, value.payload.to_vec())));
        }
        let format = if image_type.is_some() {
            "Image"
        } else {
            value.map_or("Text", |value| value.format_label())
        };
        let text = value
            .filter(|_| image_type.is_none())
            .map(|value| {
                if self.payload_preview {
                    value.display_payload()
                } else {
                    String::from_utf8_lossy(&value.payload).into_owned()
                }
            })
            .unwrap_or_default();
        let (text, ansi_styles) = if self.payload_preview && format == "Text" && text.contains('\x1b') {
            super::payload_ansi::parse(&text, cx.theme())
        } else {
            (text, Vec::new())
        };
        let previous = self.payload.read(cx).value();
        let changed = previous.as_str() != text;
        let ranges = if self.payload_preview
            && same_topic
            && changed
            && self.payload_format == "JSON"
            && self.payload.read(cx).language_name() == "json"
            && format == "JSON"
            && !Appearance::motion_reduced(cx)
        {
            payload_diff::changed_ranges(previous.as_str(), &text)
        } else {
            Vec::new()
        };
        if changed
            || !same_topic
            || self.payload.read(cx).language_name()
                != if self.payload_preview && format == "JSON" {
                    "json"
                } else {
                    "plaintext"
                }
        {
            self.numeric_fields = if self.payload_preview && format == "JSON" {
                monitor::numeric_fields(&text)
            } else {
                Vec::new()
            };
            // Live value changes do not end the pointer interaction with this field.
            if !same_topic
                || self
                    .hovered_field
                    .as_ref()
                    .is_some_and(|pointer| !self.numeric_fields.iter().any(|field| field.pointer() == pointer))
            {
                self.hovered_field = None;
            }
        }
        self.payload_format = format;
        self.payload_topic = self.selected.clone();
        let language = if self.payload_preview && format == "JSON" {
            "json"
        } else {
            "plaintext"
        };
        if !same_topic || changed || self.payload.read(cx).language_name() != language {
            let offset = if same_topic {
                self.payload_pending_scroll.unwrap_or_else(|| self.payload.read(cx).scroll_offset())
            } else {
                Default::default()
            };
            if self.payload_pending_scroll.is_none() {
                let view = cx.weak_entity();
                window.on_next_frame(move |_, cx| {
                    let _ = view.update(cx, |view, _| view.payload_pending_scroll = None);
                });
            }
            // set_value resets the live offset immediately; keep the original until layout
            // consumes the restoration, including when several updates precede that frame.
            self.payload_pending_scroll = Some(offset);
            self.payload.update(cx, |editor, cx| {
                if editor.language_name() != language {
                    editor.set_highlighter(language, cx);
                }
                editor.set_value(text, window, cx);
                // Override caret-following and any pending restoration from the previous topic.
                editor.set_scroll_offset(offset, cx);
            });
            self.payload_highlight.set(ranges, Instant::now(), cx);
        } else if !self.payload_preview || format == "Image" || Appearance::motion_reduced(cx) {
            self.payload_highlight.clear(cx);
        }
        self.payload_ansi.set(
            ansi_styles
                .into_iter()
                .map(|(range, style)| gpui_kit::base::input::TextDecoration::new(range, style))
                .collect(),
            cx,
        );
        self.schedule_received_age(window, cx);
        self.schedule_animation(window, cx);
        self.details_view.update(cx, |_, cx| cx.notify());
    }
}

#[cfg(test)]
mod tests {
    use super::image_content_type;

    #[test]
    fn image_content_types_ignore_case_whitespace_and_parameters() {
        for (content_type, expected) in [
            ("image/png", "image/png"),
            ("IMAGE/JPEG", "IMAGE/JPEG"),
            (" image/svg+xml ; charset=utf-8", "image/svg+xml"),
            ("image/vnd.example.custom", "image/vnd.example.custom"),
        ] {
            assert_eq!(image_content_type(Some(content_type)), Some(expected));
        }
    }

    #[test]
    fn non_image_or_missing_content_types_do_not_select_the_image_view() {
        for content_type in [
            None,
            Some("text/plain"),
            Some("application/image"),
            Some("image"),
            Some("image/"),
            Some("images/png"),
        ] {
            assert_eq!(image_content_type(content_type), None);
        }
    }
}
