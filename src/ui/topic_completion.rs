//! Fish-style inline topic proposals, scoped to one tree level and separate from the editable value.

use super::Explorer;
use crate::topics::TopicStore;
use gpui_kit::component::{ActiveTheme, input::InputEvent};
use gpui_kit::{
    App, ContentMask, Context, Focusable, IntoElement, SharedString, TestSupportExt, TextAlign, TextRun, Window, canvas, div, point,
    prelude::*,
};

pub(super) struct TopicCompletion {
    prefix: String,
    proposal: String,
}

fn level_completions<'a>(topics: &'a TopicStore, prefix: &'a str) -> impl Iterator<Item = String> + Clone + 'a {
    let level_start = prefix.rfind('/').map_or(0, |index| index + 1);
    topics
        .nodes
        .range(prefix.to_owned()..)
        .take_while(move |(path, _)| path.starts_with(prefix))
        .filter(move |(path, _)| !path[level_start..].contains('/'))
        .filter_map(move |(path, node)| {
            let proposal = if node.children.is_empty() {
                path.clone()
            } else {
                format!("{path}/")
            };
            (proposal.len() > prefix.len()).then_some(proposal)
        })
}

impl Explorer {
    pub(super) fn update_topic_completion(&mut self, event: &InputEvent, cx: &mut Context<Self>) {
        if !matches!(event, InputEvent::Change | InputEvent::Focus | InputEvent::Blur) {
            return;
        }
        let value = self.publish_topic.read(cx).value();
        if matches!(event, InputEvent::Blur)
            || self
                .publish_topic_completion
                .as_ref()
                .is_some_and(|completion| completion.prefix != value.as_str())
        {
            self.publish_topic_completion = None;
        }
        cx.notify();
    }

    fn proposed_topic(&self, cx: &App) -> Option<String> {
        let input = self.publish_topic.read(cx);
        let value = input.value();
        if value.is_empty() || input.selected_range() != (value.len()..value.len()) {
            return None;
        }
        let mut candidates = level_completions(&self.topics, value.as_str());
        if let Some(completion) = self.publish_topic_completion.as_ref()
            && completion.prefix == value.as_str()
            && let Some(proposal) = candidates.clone().find(|proposal| proposal == &completion.proposal)
        {
            return Some(proposal);
        }
        candidates.next()
    }

    fn accept_publish_topic(&mut self, topic: String, window: &mut Window, cx: &mut Context<Self>) {
        self.publish_topic_completion = None;
        self.publish_topic.update(cx, |input, cx| input.replace_all(topic, window, cx));
        cx.notify();
    }

    pub(super) fn accept_topic_proposal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(proposal) = self.proposed_topic(cx) {
            self.accept_publish_topic(proposal, window, cx);
            cx.stop_propagation();
        } else {
            cx.propagate();
        }
    }

    pub(super) fn complete_publish_topic(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(proposal) = self.proposed_topic(cx) else {
            window.focus_next(cx);
            return;
        };
        let value = self.publish_topic.read(cx).value();
        let candidates: Vec<_> = level_completions(&self.topics, value.as_str()).collect();
        if candidates.len() == 1 {
            self.accept_publish_topic(proposal, window, cx);
        } else {
            let current = candidates.iter().position(|path| path == &proposal).unwrap_or(0);
            self.publish_topic_completion = Some(TopicCompletion {
                prefix: value.to_string(),
                proposal: candidates[(current + 1) % candidates.len()].clone(),
            });
            cx.notify();
        }
    }

    pub(super) fn topic_proposal(&self, window: &Window, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        if !self.publish_topic.read(cx).focus_handle(cx).is_focused(window) {
            return None;
        }
        let proposal = self.proposed_topic(cx)?;
        let prefix = self.publish_topic.read(cx).value();
        let suffix = SharedString::from(proposal[prefix.len()..].replace(['\n', '\r'], " "));
        let input = self.publish_topic.clone();
        Some(
            div()
                .id("publish-topic-proposal")
                .test_support()
                .aria_label(suffix.clone())
                .absolute()
                .inset_0()
                .text_sm()
                .child(
                    canvas(
                        |_, _, _| (),
                        move |_, _, window, cx| {
                            let state = input.read(cx);
                            if !state.focus_handle(cx).is_focused(window)
                                || state.value() != prefix
                                || state.selected_range() != (prefix.len()..prefix.len())
                            {
                                return;
                            }
                            let Some((caret, line_height)) = state.cursor_layout() else {
                                return;
                            };
                            let mask = ContentMask {
                                bounds: state.input_bounds(),
                            };
                            let style = window.text_style();
                            let run = TextRun {
                                len: suffix.len(),
                                font: style.font(),
                                color: cx.theme().muted_foreground,
                                background_color: None,
                                underline: None,
                                strikethrough: None,
                            };
                            let line = window
                                .text_system()
                                .shape_line(suffix, style.font_size.to_pixels(window.rem_size()), &[run], None);
                            // Paint after the input so the proposal follows its current caret and scroll.
                            let origin = point(caret.right(), caret.top() - (line_height - caret.size.height) / 2.);
                            window.with_content_mask(Some(mask), |window| {
                                if let Err(error) = line.paint(origin, line_height, TextAlign::Left, None, window, cx) {
                                    tracing::warn!(%error, "Could not paint the topic completion");
                                }
                            });
                        },
                    )
                    .size_full(),
                ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::level_completions;
    use crate::topics::{Message, TopicStore};
    use bytes::Bytes;
    use chrono::Local;
    use std::time::Instant;

    fn store(paths: &[&str]) -> TopicStore {
        let mut topics = TopicStore::default();
        for path in paths {
            topics.receive(
                Message {
                    topic: (*path).to_owned(),
                    payload: Bytes::from_static(b"value"),
                    qos: 0,
                    retained: false,
                    received_at: Local::now(),
                    properties: Default::default(),
                },
                Instant::now(),
            );
        }
        topics
    }

    #[test]
    fn a_unique_branch_is_one_completion_regardless_of_its_descendants() {
        let topics = store(&["home/room/sensor/temperature", "home/room/sensor/humidity", "home/room/status"]);
        assert_eq!(level_completions(&topics, "home/r").collect::<Vec<_>>(), ["home/room/"]);
    }

    #[test]
    fn a_unique_root_completes_only_the_root_and_its_separator() {
        let topics = store(&["home/a/x", "home/b/y", "household/z"]);
        assert_eq!(level_completions(&topics, "hom").collect::<Vec<_>>(), ["home/"]);
    }

    #[test]
    fn sibling_completions_exclude_deeper_levels() {
        let topics = store(&["home/room/sensor/temperature", "home/room/sensor/humidity", "home/status"]);
        assert_eq!(
            level_completions(&topics, "home/").collect::<Vec<_>>(),
            ["home/room/", "home/status"]
        );
    }

    #[test]
    fn unicode_and_empty_levels_are_preserved() {
        let topics = store(&["家//測定値/a", "家//温度"]);
        assert_eq!(level_completions(&topics, "家/").collect::<Vec<_>>(), ["家//"]);
        assert_eq!(level_completions(&topics, "家//").collect::<Vec<_>>(), ["家//温度", "家//測定値/"]);
    }

    #[test]
    fn leading_empty_levels_are_completed_one_at_a_time() {
        let topics = store(&["//a", "/branch/leaf"]);
        assert_eq!(level_completions(&topics, "/").collect::<Vec<_>>(), ["//", "/branch/"]);
    }

    #[test]
    fn an_exact_leaf_has_no_completion() {
        let topics = store(&["home/status"]);
        assert_eq!(level_completions(&topics, "home/status").count(), 0);
    }
}
