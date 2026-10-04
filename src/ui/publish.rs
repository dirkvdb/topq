//! Draft composition and publishing below the live topic tree.

use super::{Explorer, PublishMessage};
use gpui_kit::component::{
    ActiveTheme, Disableable, IconName, Sizable, StyledExt,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    collapsible::Collapsible,
    input::{Editor, Input},
    select::Select,
};
use gpui_kit::{Context, IntoElement, Role, TestSupportExt, Window, div, prelude::*, relative, rems};

impl Explorer {
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
                        Some(&"1") => 1,
                        Some(&"2") => 2,
                        _ => 0,
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

    pub(super) fn publish_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let connected = self.status.is_connected() && self.connection.is_some();
        let enabled = connected && !self.publish_pending && !self.publish_topic.read(cx).value().is_empty();
        div()
            .id("publish-panel")
            .test_support()
            .key_context("PublishPanel")
            .on_action(cx.listener(|view, _: &PublishMessage, window, cx| view.publish_message(window, cx)))
            .v_flex()
            .flex_none()
            .min_w_0()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(
                Collapsible::new()
                    .open(self.publish_open)
                    .w_full()
                    .child(
                        div().h_flex().h_10().px_4().child(
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
                                .on_click(cx.listener(|view, _, _, cx| {
                                    view.publish_open = !view.publish_open;
                                    cx.notify();
                                })),
                        ),
                    )
                    .content(
                        div()
                            .id("publish-form")
                            .test_support()
                            .v_flex()
                            .h(rems(16.))
                            .min_h_0()
                            .min_w_0()
                            .px_4()
                            .pb_4()
                            .gap_3()
                            .child(
                                div().v_flex().gap_1().flex_none().child("Topic").child(
                                    Input::new(&self.publish_topic)
                                        .id("publish-topic")
                                        .small()
                                        .w_full()
                                        .aria_label("Publish topic"),
                                ),
                            )
                            .child(
                                div().v_flex().gap_1().flex_1().min_h_0().min_w_0().child("Payload").child(
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
                            .when_some(self.publish_feedback.as_ref(), |form, feedback| {
                                let (message, color) = match feedback {
                                    Ok(message) => (message, cx.theme().muted_foreground),
                                    Err(message) => (message, cx.theme().danger),
                                };
                                form.child(
                                    div()
                                        .id("publish-feedback")
                                        .test_support()
                                        .role(Role::Status)
                                        .aria_label(message.clone())
                                        .flex_none()
                                        .text_xs()
                                        .text_color(color)
                                        .child(message.clone()),
                                )
                            }),
                    ),
            )
    }
}
