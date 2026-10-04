//! Live payload refreshes preserve the viewport and briefly mark semantic JSON changes.

use std::{ops::Range, time::Instant};

use gpui_kit::component::input::{RangeDecoration, RangeDecorationCollection, RangeDecorationStyle};

use super::{ActiveTheme, App, Context, Explorer, Window, payload_diff};
use crate::topics::{FLASH_DURATION, flash_amount};

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
        if ranges.is_empty() || cx.reduce_motion() {
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

    pub(super) fn refresh(&mut self, now: Instant, cx: &mut App) {
        let Some((started, _)) = self.pulse else { return };
        if cx.reduce_motion() || now.saturating_duration_since(started) >= FLASH_DURATION {
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

    #[cfg(test)]
    pub(super) fn ranges(&self, cx: &App) -> Vec<Range<usize>> {
        self.collection.get_ranges(cx)
    }
}

impl Explorer {
    pub(super) fn refresh_details(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let value = self
            .selected
            .as_ref()
            .and_then(|path| self.topics.nodes.get(path))
            .and_then(|node| node.value.as_ref());
        let format = value.map_or("Text", |value| value.format_label());
        let text = value.map(|value| value.display_payload()).unwrap_or_default();
        let same_topic = self.payload_topic == self.selected;
        let previous = self.payload.read(cx).value();
        let changed = previous.as_str() != text;
        let ranges = if same_topic && changed && self.payload_format == "JSON" && format == "JSON" && !cx.reduce_motion() {
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
        } else if cx.reduce_motion() {
            self.payload_highlight.clear(cx);
        }
    }
}
