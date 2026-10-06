//! Virtualized hexadecimal and ASCII inspection of the complete image payload.

use std::ops::Range;

use bytes::Bytes;
use gpui_kit::component::{ActiveTheme, scroll::Scrollbar};
use gpui_kit::{
    App, Context, FocusHandle, Focusable, IntoElement, KeyBinding, ListHorizontalSizingBehavior, MouseButton, Pixels, Render,
    ScrollStrategy, SharedString, TestSupportExt, UniformListScrollHandle, Window, div, prelude::*, rems, uniform_list,
};

const BYTES_PER_ROW: usize = 16;
const ROW_HEIGHT_REM: f32 = 1.25;

gpui_kit::actions!(
    payload_raw,
    [
        ScrollHome,
        ScrollEnd,
        ScrollUp,
        ScrollDown,
        ScrollLeft,
        ScrollRight,
        ScrollPageUp,
        ScrollPageDown
    ]
);

pub(super) fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("home", ScrollHome, Some("RawPayload")),
        KeyBinding::new("end", ScrollEnd, Some("RawPayload")),
        KeyBinding::new("up", ScrollUp, Some("RawPayload")),
        KeyBinding::new("down", ScrollDown, Some("RawPayload")),
        KeyBinding::new("left", ScrollLeft, Some("RawPayload")),
        KeyBinding::new("right", ScrollRight, Some("RawPayload")),
        KeyBinding::new("pageup", ScrollPageUp, Some("RawPayload")),
        KeyBinding::new("pagedown", ScrollPageDown, Some("RawPayload")),
    ]);
}

pub(super) struct RawPayload {
    bytes: Bytes,
    scroll_handle: UniformListScrollHandle,
    focus_handle: FocusHandle,
    /// Includes the list's measurement rows, not only painted rows.
    #[cfg(test)]
    pub(super) rendered_rows: usize,
}

impl RawPayload {
    pub(super) fn new(bytes: Bytes, cx: &mut Context<Self>) -> Self {
        Self {
            bytes,
            scroll_handle: UniformListScrollHandle::new(),
            focus_handle: cx.focus_handle().tab_stop(true),
            #[cfg(test)]
            rendered_rows: 0,
        }
    }

    /// Retains the viewport; list layout clamps it if the new payload is shorter.
    /// Create a new entity when switching topics to start at the beginning.
    pub(super) fn set_bytes(&mut self, bytes: Bytes, cx: &mut Context<Self>) {
        self.bytes = bytes;
        cx.notify();
    }

    fn row_count(&self) -> usize {
        self.bytes.len().div_ceil(BYTES_PER_ROW)
    }

    #[cfg(test)]
    pub(super) fn bytes(&self) -> &Bytes {
        &self.bytes
    }

    #[cfg(test)]
    pub(super) fn byte_count(&self) -> usize {
        self.bytes.len()
    }

    #[cfg(test)]
    pub(super) fn scroll_handle(&self) -> &UniformListScrollHandle {
        &self.scroll_handle
    }

    fn render_rows(&mut self, range: Range<usize>, _: &mut Window, _: &mut Context<Self>) -> Vec<gpui_kit::AnyElement> {
        #[cfg(test)]
        {
            self.rendered_rows += range.len();
        }
        range
            .map(|ix| {
                let offset = ix * BYTES_PER_ROW;
                let end = offset.saturating_add(BYTES_PER_ROW).min(self.bytes.len());
                let text = SharedString::from(format_row(offset, &self.bytes[offset..end]));
                div()
                    .id(("payload-raw-row", offset))
                    .test_support()
                    .aria_label(text.clone())
                    .h(rems(ROW_HEIGHT_REM))
                    .line_height(rems(ROW_HEIGHT_REM))
                    .whitespace_nowrap()
                    .px_2()
                    .pr_6()
                    .child(text)
                    .into_any_element()
            })
            .collect()
    }

    fn scroll_home(&mut self, _: &ScrollHome, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_handle.scroll_to_item_strict(0, ScrollStrategy::Top);
        cx.notify();
    }

    fn scroll_end(&mut self, _: &ScrollEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_handle
            .scroll_to_item_strict(self.row_count().saturating_sub(1), ScrollStrategy::Bottom);
        cx.notify();
    }

    fn scroll_by(&mut self, x: Pixels, y: Pixels, cx: &mut Context<Self>) {
        let mut state = self.scroll_handle.0.borrow_mut();
        state.deferred_scroll_to_item = None;
        let handle = &state.base_handle;
        let mut offset = handle.offset();
        let max_offset = handle.max_offset();
        offset.x = (offset.x + x).clamp(-max_offset.x, Pixels::ZERO);
        offset.y = (offset.y + y).clamp(-max_offset.y, Pixels::ZERO);
        handle.set_offset(offset);
        cx.notify();
    }

    fn scroll_up(&mut self, _: &ScrollUp, window: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(Pixels::ZERO, window.rem_size() * ROW_HEIGHT_REM, cx);
    }

    fn scroll_down(&mut self, _: &ScrollDown, window: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(Pixels::ZERO, -window.rem_size() * ROW_HEIGHT_REM, cx);
    }

    fn scroll_left(&mut self, _: &ScrollLeft, window: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(window.rem_size(), Pixels::ZERO, cx);
    }

    fn scroll_right(&mut self, _: &ScrollRight, window: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(-window.rem_size(), Pixels::ZERO, cx);
    }

    fn scroll_page_up(&mut self, _: &ScrollPageUp, _: &mut Window, cx: &mut Context<Self>) {
        let height = self.scroll_handle.0.borrow().base_handle.bounds().size.height;
        self.scroll_by(Pixels::ZERO, height, cx);
    }

    fn scroll_page_down(&mut self, _: &ScrollPageDown, _: &mut Window, cx: &mut Context<Self>) {
        let height = self.scroll_handle.0.borrow().base_handle.bounds().size.height;
        self.scroll_by(Pixels::ZERO, -height, cx);
    }
}

impl Focusable for RawPayload {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for RawPayload {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        {
            self.rendered_rows = 0;
        }
        let theme = cx.theme();
        let ring = theme.ring;
        div()
            .id("payload-raw")
            .test_support()
            .aria_label("Latest topic payload bytes, hexadecimal and ASCII")
            .track_focus(&self.focus_handle)
            .key_context("RawPayload")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| window.focus(&this.focus_handle, cx)),
            )
            .on_action(cx.listener(Self::scroll_home))
            .on_action(cx.listener(Self::scroll_end))
            .on_action(cx.listener(Self::scroll_up))
            .on_action(cx.listener(Self::scroll_down))
            .on_action(cx.listener(Self::scroll_left))
            .on_action(cx.listener(Self::scroll_right))
            .on_action(cx.listener(Self::scroll_page_up))
            .on_action(cx.listener(Self::scroll_page_down))
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .size_full()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .border_1()
            .border_color(theme.input)
            .rounded(theme.radius)
            .bg(theme
                .highlight_theme
                .style
                .editor_background
                .unwrap_or_else(|| theme.input_background()))
            .text_color(theme.foreground)
            .font_family(theme.mono_font_family.clone())
            .text_sm()
            // An inward focused border remains visible even in a clipped inspector pane.
            .focus_visible(move |style| style.border_color(ring))
            .child(
                uniform_list("payload-raw-list", self.row_count(), cx.processor(Self::render_rows))
                    .with_width_from_item(Some(self.row_count().saturating_sub(1)))
                    .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
                    .track_scroll(&self.scroll_handle)
                    .flex_1()
                    .size_full()
                    .min_w_0()
                    .min_h_0(),
            )
            .child(Scrollbar::new(&self.scroll_handle))
    }
}

fn format_row(offset: usize, bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut row = format!("{offset:08x}  ");
    row.reserve(67);
    for ix in 0..BYTES_PER_ROW {
        if ix > 0 {
            row.push(' ');
        }
        if let Some(&byte) = bytes.get(ix) {
            row.push(char::from(HEX[usize::from(byte >> 4)]));
            row.push(char::from(HEX[usize::from(byte & 0x0f)]));
        } else {
            row.push_str("  ");
        }
    }
    row.push_str("  |");
    for ix in 0..BYTES_PER_ROW {
        row.push(match bytes.get(ix) {
            Some(&byte @ 0x20..=0x7e) => char::from(byte),
            Some(_) => '.',
            None => ' ',
        });
    }
    row.push('|');
    row
}

#[cfg(test)]
mod tests {
    use super::format_row;

    #[test]
    fn full_row_has_offset_hex_and_ascii() {
        assert_eq!(
            format_row(0x10, b"0123456789ABCDEF"),
            "00000010  30 31 32 33 34 35 36 37 38 39 41 42 43 44 45 46  |0123456789ABCDEF|"
        );
    }

    #[test]
    fn partial_row_pads_hex_and_ascii_columns() {
        assert_eq!(
            format_row(0x20, b"Hi"),
            format!("00000020  48 69{}  |Hi{}|", " ".repeat(42), " ".repeat(14))
        );
    }

    #[test]
    fn control_and_non_ascii_bytes_are_dots_but_printable_boundaries_are_preserved() {
        assert_eq!(
            format_row(0, &[0, 9, 10, 13, 0x1f, 0x20, 0x7e, 0x7f, 0x80, 0xc3, 0xa9, 0xff]),
            format!(
                "00000000  00 09 0a 0d 1f 20 7e 7f 80 c3 a9 ff{}  |..... ~.....{}|",
                " ".repeat(12),
                " ".repeat(4)
            )
        );
    }

    #[test]
    fn empty_row_has_blank_columns() {
        assert_eq!(format_row(0, &[]), format!("00000000  {}  |{}|", " ".repeat(47), " ".repeat(16)));
    }

    #[test]
    fn offset_uses_at_least_eight_digits_without_truncation() {
        assert!(format_row(usize::MAX, b"x").starts_with(&format!("{:08x}  78", usize::MAX)));
    }
}
