//! Draft composition and publishing below the live topic tree.

use std::ops::Range;

use super::{CompletePublishTopic, Explorer, GrowPublish, PublishMessage, ShrinkPublish};
use crate::{config, mqtt::Qos};
use gpui_kit::base::{
    ElementExt,
    input::{Diagnostic, DiagnosticSeverity},
};
use gpui_kit::component::{
    ActiveTheme, Disableable, IconName, Sizable, StyledExt,
    alert::Alert,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    collapsible::Collapsible,
    input::{Editor, Input, MoveRight, RopeExt},
    resizable::ResizableState,
    select::Select,
};
use gpui_kit::{Context, Entity, IntoElement, Role, TestSupportExt, Window, div, prelude::*, relative, rems};

#[derive(Default)]
pub(super) enum PayloadContent {
    #[default]
    Text,
    Json,
}

impl PayloadContent {
    fn detect(text: &str) -> Self {
        if !text.starts_with('{') {
            return Self::Text;
        }
        Self::Json
    }

    fn label(&self) -> &'static str {
        match self {
            Self::Text => "Text",
            Self::Json => "JSON",
        }
    }

    fn language(&self) -> &'static str {
        match self {
            Self::Text => "plaintext",
            Self::Json => "json",
        }
    }
}

fn json_error_range(text: &str, error: &serde_json::Error) -> Range<usize> {
    let line_start = if error.line() <= 1 {
        0
    } else {
        text.match_indices('\n')
            .nth(error.line() - 2)
            .map_or(text.len(), |(offset, _)| offset + 1)
    };
    // Serde reports one-based byte columns; the editor needs character-aligned ranges.
    // At EOF, underline the last visible character instead of an empty range.
    let offset = if error.is_eof() {
        text.len()
    } else {
        (line_start + error.column().saturating_sub(1)).min(text.len())
    };
    let (start, character) = text
        .char_indices()
        .take_while(|(index, _)| *index <= offset)
        .filter(|(_, character)| !character.is_whitespace())
        .last()
        .unwrap_or((0, '{'));
    start..start + character.len_utf8()
}

impl Explorer {
    pub(super) fn update_publish_content(&mut self, cx: &mut Context<Self>) {
        let text = self.publish_payload.read(cx).value();
        self.publish_content = PayloadContent::detect(text.as_str());
        let language = self.publish_content.language();
        let error = if matches!(self.publish_content, PayloadContent::Json) {
            serde_json::from_str::<serde_json::Value>(text.as_str()).err()
        } else {
            None
        };
        self.publish_payload.update(cx, |editor, cx| {
            if editor.language_name() != language {
                editor.set_highlighter(language, cx);
            }
            let diagnostic = error.map(|error| {
                let range = json_error_range(text.as_str(), &error);
                Diagnostic::new(
                    editor.text().offset_to_position(range.start)..editor.text().offset_to_position(range.end),
                    error.to_string(),
                )
                .with_severity(DiagnosticSeverity::Error)
                .with_source("JSON")
            });
            if let Some(diagnostics) = editor.diagnostics_mut() {
                diagnostics.clear();
                diagnostics.extend(diagnostic);
            }
            cx.notify();
        });
        cx.notify();
    }

    fn resize_publish(&mut self, grow: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !self.publish_open || self.show_config {
            return;
        }
        let height = self.publish_panel_height;
        let delta = rems(if grow { 2. } else { -2. }).to_pixels(window.rem_size());
        self.publish_panes.update(cx, |state, cx| {
            let minimum = rems(19.).to_pixels(window.rem_size());
            let maximum = (state.container_size() - rems(4.5).to_pixels(window.rem_size())).max(minimum);
            let height = (height + delta).clamp(minimum, maximum);
            state.resize_panel(0, state.container_size() - height, window, cx);
        });
    }

    pub(super) fn persist_publish_split(&mut self, state: &Entity<ResizableState>, window: &Window, cx: &mut Context<Self>) {
        if !self.publish_open {
            return;
        }
        let state = state.read(cx);
        let Some(height) = state.sizes().get(1) else { return };
        let container_size = state.container_size();
        if container_size.as_f32() <= 0. {
            return;
        }
        let height_rem = *height / window.rem_size();
        let height_fraction = (*height / container_size).clamp(0., 1.);
        self.publish_height_rem = Some(height_rem);
        self.publish_height_fraction = Some(height_fraction);
        if let Err(error) = config::save_publish_height(height_rem, height_fraction) {
            self.error = Some(format!("Could not save the pane layout: {error:#}"));
        }
        cx.notify();
    }

    fn publish_message(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if self.publish_pending {
            return;
        }
        let result = if self.status.is_connected() {
            self.connection
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Connect to a broker before publishing."))
                .and_then(|connection| {
                    let qos = match self.publish_qos.read(cx).selected_value() {
                        Some(&"1") => Qos::AtLeastOnce,
                        Some(&"2") => Qos::ExactlyOnce,
                        _ => Qos::AtMostOnce,
                    };
                    connection.publish(
                        self.publish_topic.read(cx).value().to_string(),
                        self.publish_payload.read(cx).value().as_bytes().to_vec(),
                        qos,
                        self.publish_retain,
                    )
                })
        } else {
            Err(anyhow::anyhow!("Connect to a broker before publishing."))
        };
        match result {
            Ok(()) => {
                self.publish_pending = true;
                self.publish_feedback = None;
            }
            Err(error) => self.publish_feedback = Some(Err(format!("{error:#}"))),
        }
        cx.notify();
    }

    pub(super) fn publish_panel(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let connected = self.status.is_connected() && self.connection.is_some();
        let enabled = connected && !self.publish_pending && !self.publish_topic.read(cx).value().is_empty();
        let view = cx.weak_entity();
        div()
            .id("publish-panel")
            .test_support()
            .key_context("PublishPanel")
            .on_action(cx.listener(|view, _: &PublishMessage, window, cx| view.publish_message(window, cx)))
            .on_action(cx.listener(|view, _: &GrowPublish, window, cx| view.resize_publish(true, window, cx)))
            .on_action(cx.listener(|view, _: &ShrinkPublish, window, cx| view.resize_publish(false, window, cx)))
            .v_flex()
            .flex_none()
            .w_full()
            .min_w_0()
            .on_prepaint(move |bounds, _, cx| {
                if let Some(view) = view.upgrade() {
                    view.update(cx, |view, _| view.publish_panel_height = bounds.size.height);
                }
            })
            .when(self.publish_open, |panel| panel.h_full())
            .child(
                Collapsible::new()
                    .open(self.publish_open)
                    .w_full()
                    .when(self.publish_open, |panel| panel.h_full().min_h_0())
                    .child(
                        div().h_flex().h_10().flex_none().px_4().child(
                            Button::new("toggle-publish")
                                .ghost()
                                .small()
                                .font_medium()
                                .icon(if self.publish_open {
                                    IconName::ChevronDown
                                } else {
                                    IconName::ChevronRight
                                })
                                .label("Publish")
                                .accessibility_label(if self.publish_open { "Collapse publish" } else { "Expand publish" })
                                .tooltip("Drag the top edge to resize (Ctrl+Alt+Up/Down)")
                                .on_click(cx.listener(|view, _, _, cx| {
                                    view.publish_open = !view.publish_open;
                                    view.restore_publish_height = view.publish_open;
                                    if let Err(error) = config::save_publish_open(view.publish_open) {
                                        view.error = Some(format!("Could not save the pane layout: {error:#}"));
                                    }
                                    cx.notify();
                                })),
                        ),
                    )
                    .content(
                        div()
                            .id("publish-form")
                            .test_support()
                            .v_flex()
                            .flex_1()
                            .min_h_0()
                            .min_w_0()
                            .px_4()
                            .pb_4()
                            .gap_3()
                            .child(
                                div()
                                    .key_context("PublishTopic")
                                    .on_action(
                                        cx.listener(|view, _: &CompletePublishTopic, window, cx| view.complete_publish_topic(window, cx)),
                                    )
                                    .capture_action(cx.listener(|view, _: &MoveRight, window, cx| view.accept_topic_proposal(window, cx)))
                                    .v_flex()
                                    .gap_1()
                                    .flex_none()
                                    .child("Topic")
                                    .child(
                                        div()
                                            .relative()
                                            .w_full()
                                            .child(
                                                Input::new(&self.publish_topic)
                                                    .id("publish-topic")
                                                    .small()
                                                    .w_full()
                                                    .aria_label("Publish topic"),
                                            )
                                            .children(self.topic_proposal(window, cx)),
                                    ),
                            )
                            .child(
                                div()
                                    .v_flex()
                                    .gap_1()
                                    .flex_1()
                                    .min_h_0()
                                    .min_w_0()
                                    .child(
                                        div().h_flex().justify_between().child("Payload").child(
                                            div()
                                                .id("publish-content-type")
                                                .test_support()
                                                .aria_label(self.publish_content.label())
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(self.publish_content.label()),
                                        ),
                                    )
                                    .child(
                                        div().id("publish-payload").test_support().flex_1().min_h_0().child(
                                            Editor::new(&self.publish_payload)
                                                .h(relative(1.))
                                                .w_full()
                                                .text_sm()
                                                .aria_label("Publish payload"),
                                        ),
                                    ),
                            )
                            .child(
                                div()
                                    .h_flex()
                                    .flex_none()
                                    .gap_2()
                                    .child("QoS")
                                    .child(
                                        Select::new(&self.publish_qos)
                                            .id("publish-qos")
                                            .small()
                                            .w_16()
                                            .accessibility_label("Publish QoS"),
                                    )
                                    .child(div().flex_1())
                                    .child(
                                        Checkbox::new("publish-retain")
                                            .small()
                                            .label("Retain")
                                            .checked(self.publish_retain)
                                            .on_change(cx.listener(|view, checked, _, cx| {
                                                view.publish_retain = *checked;
                                                cx.notify();
                                            })),
                                    ),
                            )
                            .child(
                                div().h_flex().flex_none().justify_end().child(
                                    Button::new("publish-message")
                                        .small()
                                        .label("Publish")
                                        .disabled(!enabled)
                                        .loading(self.publish_pending)
                                        .tooltip("Publish message (Ctrl+Enter)")
                                        .on_click(cx.listener(|view, _, window, cx| view.publish_message(window, cx))),
                                ),
                            )
                            .when(!connected && self.publish_feedback.is_none(), |form| {
                                form.child(
                                    div()
                                        .flex_none()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child("Connect to a broker to publish."),
                                )
                            })
                            .when_some(self.publish_feedback.as_ref(), |form, feedback| match feedback {
                                Ok(message) => form.child(
                                    div()
                                        .id("publish-feedback")
                                        .test_support()
                                        .role(Role::Status)
                                        .aria_label(message.clone())
                                        .flex_none()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(message.clone()),
                                ),
                                Err(message) => form.child(
                                    div()
                                        .id("publish-feedback")
                                        .test_support()
                                        .child(Alert::error("publish-feedback-content", message.clone()).banner()),
                                ),
                            }),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::{PayloadContent, json_error_range};

    #[test]
    fn only_a_literal_leading_brace_enables_json() {
        for text in ["", "online", "[]", "\"hello\"", "42", "true", "null", " {}", "\n{}"] {
            assert_eq!(PayloadContent::detect(text).language(), "plaintext", "{text:?}");
        }
        for text in ["{}", "{", "{\"a\":1}", "{invalid}"] {
            assert_eq!(PayloadContent::detect(text).language(), "json", "{text:?}");
        }
    }

    #[test]
    fn invalid_json_marks_the_offending_character_on_its_line() {
        let text = "{\n  \"a\": ?\n}";
        let error = serde_json::from_str::<serde_json::Value>(text).unwrap_err();
        let offset = text.find('?').unwrap();
        assert_eq!(json_error_range(text, &error), offset..offset + 1);
    }

    #[test]
    fn invalid_json_ranges_preserve_unicode_character_boundaries() {
        for (text, offending) in [("{\"é😀\": ?}", "?"), ("{\"a\": é}", "é"), ("{\"a\": 😀}", "😀")] {
            let error = serde_json::from_str::<serde_json::Value>(text).unwrap_err();
            let range = json_error_range(text, &error);
            let offset = text.rfind(offending).unwrap();
            assert_eq!(range, offset..offset + offending.len());
        }
    }

    #[test]
    fn incomplete_json_underlines_a_visible_character_even_after_a_newline() {
        for text in ["{", "{\n", "{\"a\":1\n  "] {
            let error = serde_json::from_str::<serde_json::Value>(text).unwrap_err();
            let range = json_error_range(text, &error);
            assert!(!range.is_empty());
            assert!(!text[range].chars().any(char::is_whitespace));
        }
    }
}
