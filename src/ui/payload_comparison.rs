//! Compares the two latest topic payloads in a retained, virtualized diff surface.

use std::{borrow::Cow, time::Duration};

use bytes::Bytes;
use gpui_kit::component::{
    ActiveTheme, Size, StyleSized, StyledExt,
    diff::{Diff, DiffFile, DiffHunkSeparator, DiffState},
};
use gpui_kit::{App, Context, Entity, IntoElement, Render, Task, TestSupportExt, Window, div, prelude::*};
use similar::TextDiff;

use crate::topics::TopicValue;

fn editor_background(cx: &App) -> gpui_kit::Hsla {
    cx.theme()
        .highlight_theme
        .style
        .editor_background
        .unwrap_or_else(|| cx.theme().input_background())
}

pub(super) fn is_text_payload(value: &TopicValue) -> bool {
    super::payload::payload_image_content_type(value).is_none()
        && value.inferred_content_type().is_none_or(|mime| mime.starts_with("text/"))
        && std::str::from_utf8(&value.payload).is_ok()
}

#[derive(Clone, PartialEq, Eq)]
struct ComparisonKey {
    topic: String,
    current: Bytes,
    previous: Option<Bytes>,
    previous_is_text: bool,
    preview: bool,
}

pub(super) struct PayloadComparison {
    diff: Entity<DiffState>,
    key: Option<ComparisonKey>,
    status: Option<&'static str>,
    task: Option<Task<()>>,
}

impl PayloadComparison {
    pub(super) fn new(cx: &mut Context<Self>) -> Self {
        Self {
            diff: cx.new(|cx| DiffState::new([], cx).with_context_lines(None)),
            key: None,
            status: None,
            task: None,
        }
    }

    #[cfg(test)]
    pub(super) fn diff(&self) -> &Entity<DiffState> {
        &self.diff
    }

    fn diff_view(&self, cx: &App) -> Diff {
        Diff::new(&self.diff)
            .line_number(false)
            .header_visible(false)
            // Full context is supplied, so no unavailable-line information is shown.
            .hunk_separator(DiffHunkSeparator::LineInfo)
            .size_full()
            .input_py(Size::Medium)
            .bg(editor_background(cx))
            .border_1()
            .border_color(cx.theme().input)
            .rounded(cx.theme().radius)
            .overflow_hidden()
    }

    pub(super) fn clear(&mut self, cx: &mut Context<Self>) {
        if self.key.take().is_some() {
            self.task = None;
            self.status = None;
            self.diff.update(cx, |diff, cx| diff.set_files([], cx));
            cx.notify();
        }
    }

    pub(super) fn refresh(
        &mut self,
        topic: &str,
        current: &TopicValue,
        previous: Option<&TopicValue>,
        preview: bool,
        text: &str,
        cx: &mut Context<Self>,
    ) {
        let key = ComparisonKey {
            topic: topic.to_owned(),
            current: current.payload.clone(),
            previous: previous.map(|value| value.payload.clone()),
            previous_is_text: previous.is_some_and(is_text_payload),
            preview,
        };
        if self.key.as_ref() == Some(&key) {
            return;
        }
        let retain_diff = self
            .key
            .as_ref()
            .is_some_and(|previous| previous.topic == key.topic && previous.preview == preview)
            && self.status.is_none()
            && !self.diff.read(cx).files().is_empty();
        self.key = Some(key.clone());
        self.task = None;
        let Some(previous) = previous else {
            self.diff.update(cx, |diff, cx| diff.set_files([], cx));
            self.status = Some("No previous payload. Waiting for the next message.");
            cx.notify();
            return;
        };
        if !key.previous_is_text {
            self.diff.update(cx, |diff, cx| diff.set_files([], cx));
            self.status = Some("The previous payload is binary and cannot be compared as text.");
            cx.notify();
            return;
        }
        let previous_text = if preview {
            let text = previous.display_payload();
            if previous.format_label() == "Text" && text.contains('\x1b') {
                super::payload_ansi::parse(&text, cx.theme()).0
            } else {
                text
            }
        } else {
            String::from_utf8_lossy(&previous.payload).into_owned()
        };
        let language = if preview && current.format_label() == "JSON" {
            "json"
        } else {
            "plaintext"
        };
        self.status = None;
        if previous_text == text || !retain_diff {
            // Keep this surface populated even before its first comparison is ready.
            self.diff
                .update(cx, |diff, cx| diff.set_files([unchanged_file(text, language)], cx));
        }
        if previous_text == text {
            cx.notify();
            return;
        }
        let text = text.to_owned();
        self.task = Some(cx.spawn(async move |view, cx| {
            let result = cx
                .background_spawn(async move { comparison_files(&previous_text, &text, language) })
                .await;
            let _ = view.update(cx, |view, cx| {
                // A topic change or newer message invalidates the old source coordinates.
                if view.key.as_ref() != Some(&key) {
                    return;
                }
                match result {
                    Ok(files) => {
                        view.diff.update(cx, |diff, cx| diff.set_files(files, cx));
                        view.status = None;
                    }
                    Err(error) => {
                        tracing::warn!(%error, "Could not prepare payload diff");
                        view.diff.update(cx, |diff, cx| diff.set_files([], cx));
                        view.status = Some("Unable to compare payloads. Toggle Diff to try again.");
                    }
                }
                view.task = None;
                cx.notify();
            });
        }));
        cx.notify();
    }
}

fn unchanged_file(text: &str, language: &str) -> DiffFile {
    // Match the patch transport's final newline without changing the retained payload.
    let text = if text.is_empty() || text.ends_with('\n') {
        Cow::Borrowed(text)
    } else {
        Cow::Owned(format!("{text}\n"))
    };
    DiffFile::unchanged("current/payload", &text).with_language(language)
}

fn comparison_files(previous: &str, current: &str, language: &str) -> Result<Vec<DiffFile>, gpui_kit::component::diff::DiffParseError> {
    // Supply the complete source: payload diffs never fold unchanged lines.
    let context = previous.lines().count().max(current.lines().count());
    let patch = TextDiff::configure()
        .timeout(Duration::from_millis(100))
        .diff_lines(previous, current)
        .unified_diff()
        // File-ending notices do not belong in the payload UI. Only the display patch
        // gains a transport newline; retained bytes and Copy latest value stay intact.
        .missing_newline_hint(false)
        .context_radius(context)
        .header("previous/payload", "current/payload")
        .to_string();
    DiffFile::parse(&patch).map(|files| files.into_iter().map(|file| file.with_language(language)).collect())
}

impl Render for PayloadComparison {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let container = div().id("payload-diff").test_support().v_flex().size_full().min_w_0().min_h_0();
        if let Some(status) = self.status {
            container
                .bg(editor_background(cx))
                .border_1()
                .border_color(cx.theme().input)
                .rounded(cx.theme().radius)
                .overflow_hidden()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .id("payload-diff-status")
                        .test_support()
                        .aria_label(status)
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(status),
                )
        } else {
            container.child(self.diff_view(cx))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{PayloadComparison, comparison_files};
    use gpui_kit::component::{ActiveTheme, Theme, ThemeMode, diff::DiffFileStatus};
    use gpui_kit::{AppContext, Overflow, Styled, TestAppContext};

    #[gpui_kit::test]
    fn diff_surface_uses_editor_theme_tokens_and_clips_rounded_corners(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            cx.update(|cx| {
                Theme::change(mode, None, cx);
                // Make the syntax theme background differ from the input fallback.
                Theme::update(cx, |theme| {
                    std::sync::Arc::make_mut(&mut theme.highlight_theme).style.editor_background = Some(theme.popover);
                });
                let comparison = cx.new(PayloadComparison::new);
                let mut diff = comparison.read(cx).diff_view(cx);
                let style = diff.style();
                assert_eq!(style.background, Some(cx.theme().popover.into()));
                assert_eq!(style.padding.top, Some(gpui_kit::component::Size::Medium.input_py().into()));
                assert_eq!(style.padding.bottom, Some(gpui_kit::component::Size::Medium.input_py().into()));
                assert_eq!(style.border_color, Some(cx.theme().input));
                assert_eq!(style.corner_radii.top_left, Some(cx.theme().radius.into()));
                assert_eq!(style.corner_radii.top_right, Some(cx.theme().radius.into()));
                assert_eq!(style.corner_radii.bottom_left, Some(cx.theme().radius.into()));
                assert_eq!(style.corner_radii.bottom_right, Some(cx.theme().radius.into()));
                assert_eq!(style.overflow.x, Some(Overflow::Hidden));
                assert_eq!(style.overflow.y, Some(Overflow::Hidden));
            });
        }
    }

    #[test]
    fn comparison_counts_only_changed_lines() {
        let files = comparison_files("same\nold\nend\n", "same\nnew\nend\n", "plaintext").unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].status(), DiffFileStatus::Modified);
        assert_eq!((files[0].additions(), files[0].deletions()), (1, 1));
    }

    #[test]
    fn comparison_handles_unicode_patch_like_lines_and_missing_newline() {
        for (previous, current) in [("é\n--- before", "雪\n+++ after"), ("same\n", "same"), ("", "new"), ("old", "")] {
            let files = comparison_files(previous, current, "plaintext").unwrap();
            assert_eq!(files.len(), 1, "{previous:?} -> {current:?}");
            assert!(files[0].additions() + files[0].deletions() > 0);
        }
    }
}
