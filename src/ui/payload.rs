//! Image previews and live text refreshes with viewport preservation and semantic JSON change highlights.

use std::{ops::Range, sync::Arc, time::Instant};

use gpui_kit::component::input::{Editor, RangeDecoration, RangeDecorationCollection, RangeDecorationStyle};
use gpui_kit::{AnyElement, Image, ImageFormat, ObjectFit, StyledImage, TestSupportExt, div, img, prelude::*, relative};

use super::{ActiveTheme, App, Context, Explorer, StyledExt, Window, payload_diff};
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
    pub(super) fn payload_view(&self, cx: &Context<Self>) -> AnyElement {
        if self.payload_format == "Image" {
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
        div()
            .id("payload-editor")
            .test_support()
            .size_full()
            .child(
                Editor::new(&self.payload)
                    .readonly(true)
                    .h(relative(1.))
                    .w_full()
                    .text_sm()
                    .aria_label("Latest topic payload"),
            )
            .into_any_element()
    }

    #[hotpath::measure(impl_type = "Explorer")]
    pub(super) fn refresh_details(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let value = self
            .selected
            .as_ref()
            .and_then(|path| self.topics.nodes.get(path))
            .and_then(|node| node.value.as_ref());
        let image_type = value.and_then(|value| image_content_type(value.properties.content_type()));
        let image_format = image_type.and_then(|mime_type| ImageFormat::from_mime_type(&mime_type.to_ascii_lowercase()));
        let image_value = value.zip(image_format);
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
            .map(|value| value.display_payload())
            .unwrap_or_default();
        let same_topic = self.payload_topic == self.selected;
        let previous = self.payload.read(cx).value();
        let changed = previous.as_str() != text;
        let ranges = if same_topic && changed && self.payload_format == "JSON" && format == "JSON" && !Appearance::motion_reduced(cx) {
            payload_diff::changed_ranges(previous.as_str(), &text)
        } else {
            Vec::new()
        };
        self.payload_format = format;
        self.payload_topic = self.selected.clone();
        if !same_topic || changed {
            let language = if format == "JSON" { "json" } else { "plaintext" };
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
        } else if format == "Image" || Appearance::motion_reduced(cx) {
            self.payload_highlight.clear(cx);
        }
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
