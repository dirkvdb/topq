//! Draft composition and publishing below the live topic tree.

use std::{fs::File, io::Read, ops::Range, path::Path, sync::Arc};

use anyhow::{Context as _, ensure};

use super::{CompletePublishTopic, Explorer, GrowPublish, PublishMessage, ShrinkPublish};
use crate::{
    config,
    mqtt::{self, Qos},
};
use gpui_kit::assets::IconName as AssetIconName;
use gpui_kit::base::{
    ElementExt,
    input::{Diagnostic, DiagnosticSeverity},
};
use gpui_kit::component::{
    ActiveTheme, Disableable, IconName, Sizable, StyledExt,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    collapsible::Collapsible,
    input::{Editor, Input, MoveRight, RopeExt},
    resizable::ResizableState,
    select::Select,
};
use gpui_kit::{
    AnyElement, Context, Entity, Image, ImageFormat, IntoElement, ObjectFit, PathPromptOptions, Role, SharedString, StyledImage,
    TestSupportExt, Window, div, img, prelude::*, relative, rems,
};

pub(super) struct PublishFile {
    name: SharedString,
    payload: Vec<u8>,
    content_type: &'static str,
    image: Option<Arc<Image>>,
}

impl PublishFile {
    fn read(path: &Path) -> anyhow::Result<Self> {
        let metadata = std::fs::metadata(path).with_context(|| format!("Could not read {}", path.display()))?;
        ensure!(metadata.is_file(), "Select a regular data file.");
        ensure!(
            metadata.len() <= u64::from(mqtt::MAX_PACKET_SIZE),
            "File exceeds the 16 MiB MQTT packet limit."
        );
        let file = File::open(path).with_context(|| format!("Could not open {}", path.display()))?;
        let mut payload = Vec::new();
        file.take(u64::from(mqtt::MAX_PACKET_SIZE) + 1)
            .read_to_end(&mut payload)
            .with_context(|| format!("Could not read {}", path.display()))?;
        ensure!(
            payload.len() <= mqtt::MAX_PACKET_SIZE as usize,
            "File exceeds the 16 MiB MQTT packet limit."
        );
        let content_type = file_content_type(path);
        let image = ImageFormat::from_mime_type(content_type).map(|format| Arc::new(Image::from_bytes(format, payload.clone())));
        let name = path.file_name().unwrap_or(path.as_os_str()).to_string_lossy().into_owned().into();
        Ok(Self {
            name,
            payload,
            content_type,
            image,
        })
    }
}

fn file_content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" | "jpe" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        "ico" => "image/ico",
        "pbm" | "pgm" | "ppm" | "pnm" => "image/x-portable-anymap",
        "avif" => "image/avif",
        "json" => "application/json",
        "pdf" => "application/pdf",
        "xml" => "application/xml",
        "txt" | "log" => "text/plain",
        "csv" => "text/csv",
        _ => "application/octet-stream",
    }
}

fn file_preview_status(message: &'static str, color: gpui_kit::Hsla) -> AnyElement {
    div()
        .size_full()
        .v_flex()
        .items_center()
        .justify_center()
        .text_xs()
        .text_color(color)
        .child(message)
        .into_any_element()
}

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
    fn set_publish_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.publish_open == open {
            return;
        }
        self.publish_open = open;
        self.restore_publish_height = open;
        if let Err(error) = config::save_publish_open(open) {
            self.error = Some(format!("Could not save the pane layout: {error:#}"));
        }
        cx.notify();
    }

    pub(super) fn edit_topic_value(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(topic) = self.selected.clone() else { return };
        let Some(value) = self.topics.nodes.get(&topic).and_then(|node| node.value.as_ref()) else {
            return;
        };
        let payload = if matches!(self.payload_format, "Text" | "JSON") {
            value.display_payload()
        } else {
            String::new()
        };
        self.publish_file_task = None;
        self.clear_publish_file(cx);
        self.set_publish_open(true, cx);
        self.publish_topic.update(cx, |input, cx| input.set_value(topic, window, cx));
        self.publish_payload.update(cx, |editor, cx| {
            editor.set_value(payload, window, cx);
            let end = editor.text().offset_to_position(editor.text().len());
            editor.set_cursor_position(end, window, cx);
        });
        self.publish_feedback = None;
        self.update_publish_content(cx);
    }

    pub(super) fn update_publish_content(&mut self, cx: &mut Context<Self>) {
        if self.publish_file.is_some() {
            return;
        }
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

    fn clear_publish_file(&mut self, cx: &mut Context<Self>) {
        if let Some(file) = self.publish_file.take()
            && let Some(image) = file.image
        {
            image.remove_asset(cx);
        }
    }

    fn remove_publish_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.publish_pending || self.publish_file_task.is_some() {
            return;
        }
        self.clear_publish_file(cx);
        self.publish_feedback = None;
        self.update_publish_content(cx);
        self.publish_payload.update(cx, |editor, cx| editor.focus(window, cx));
    }

    fn choose_publish_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.publish_pending || self.publish_file_task.is_some() {
            return;
        }
        let selection = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Load payload file".into()),
        });
        let background = cx.background_executor().clone();
        let restore_focus = window.focused(cx);
        self.publish_file_task = Some(cx.spawn_in(window, async move |view, cx| {
            let result: anyhow::Result<Option<PublishFile>> = async {
                let Some(path) = selection.await??.and_then(|paths| paths.into_iter().next()) else {
                    return Ok(None);
                };
                background.spawn(async move { PublishFile::read(&path) }).await.map(Some)
            }
            .await;
            _ = view.update_in(cx, |view, window, cx| {
                view.publish_file_task = None;
                match result {
                    Ok(Some(file)) => {
                        view.clear_publish_file(cx);
                        view.publish_file = Some(file);
                        view.publish_feedback = None;
                        if view.publish_open && !view.show_config {
                            view.publish_topic.update(cx, |input, cx| input.focus(window, cx));
                        }
                    }
                    Ok(None) => {
                        if view.publish_open
                            && !view.show_config
                            && let Some(focus) = restore_focus
                        {
                            focus.focus(window, cx);
                        }
                    }
                    Err(error) => view.publish_feedback = Some(Err(format!("Could not load payload file: {error:#}"))),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn publish_payload_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(file) = &self.publish_file else {
            return Editor::new(&self.publish_payload)
                .h(relative(1.))
                .w_full()
                .text_sm()
                .aria_label("Publish payload")
                .into_any_element();
        };
        let color = cx.theme().muted_foreground;
        div()
            .id("publish-file")
            .test_support()
            .v_flex()
            .size_full()
            .min_w_0()
            .min_h_0()
            .gap_1()
            .child(
                div()
                    .h_flex()
                    .flex_none()
                    .min_w_0()
                    .gap_1()
                    .child(
                        div()
                            .id("publish-file-name")
                            .test_support()
                            .aria_label(file.name.clone())
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .child(file.name.clone()),
                    )
                    .child(
                        div()
                            .id("publish-file-size")
                            .test_support()
                            .aria_label(format!("{} bytes", file.payload.len()))
                            .flex_none()
                            .text_xs()
                            .text_color(color)
                            .child(format!("{} bytes", file.payload.len())),
                    )
                    .child(
                        Button::new("remove-publish-file")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Close)
                            .accessibility_label("Remove file and restore text draft")
                            .tooltip("Remove file")
                            .disabled(self.publish_pending || self.publish_file_task.is_some())
                            .on_click(cx.listener(|view, _, window, cx| view.remove_publish_file(window, cx))),
                    ),
            )
            .child(
                div()
                    .id("publish-file-preview")
                    .test_support()
                    .when(file.image.is_some(), |preview| preview.role(Role::Image))
                    .aria_label("Payload file preview")
                    .relative()
                    .overflow_hidden()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .map(|preview| {
                        if let Some(image) = &file.image {
                            preview.child(
                                // Keep the image's intrinsic aspect ratio out of flex sizing.
                                img(image.clone())
                                    .id(("publish-file-image", image.id()))
                                    .absolute()
                                    .inset_0()
                                    .size_full()
                                    .object_fit(ObjectFit::ScaleDown)
                                    .with_loading(move || file_preview_status("Loading image…", color))
                                    .with_fallback(move || file_preview_status("Image preview unavailable", color))
                                    .test_support(),
                            )
                        } else {
                            preview.child(file_preview_status("File bytes will be published unchanged.", color))
                        }
                    }),
            )
            .into_any_element()
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
        if self.publish_pending || self.publish_file_task.is_some() {
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
                    let topic = self.publish_topic.read(cx).value().to_string();
                    if let Some(file) = &self.publish_file {
                        connection.publish_with_content_type(
                            topic,
                            file.payload.clone(),
                            qos,
                            self.publish_retain,
                            Some(file.content_type.to_owned()),
                        )
                    } else {
                        connection.publish(
                            topic,
                            self.publish_payload.read(cx).value().as_bytes().to_vec(),
                            qos,
                            self.publish_retain,
                        )
                    }
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
        let enabled =
            connected && !self.publish_pending && self.publish_file_task.is_none() && !self.publish_topic.read(cx).value().is_empty();
        let content_type = self
            .publish_file
            .as_ref()
            .map_or(self.publish_content.label(), |file| file.content_type);
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
                                    view.set_publish_open(!view.publish_open, cx);
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
                                        div()
                                            .h_flex()
                                            .flex_none()
                                            .min_w_0()
                                            .gap_2()
                                            .justify_between()
                                            .child("Payload")
                                            .child(
                                                div()
                                                    .id("publish-content-type")
                                                    .test_support()
                                                    .aria_label(content_type)
                                                    .min_w_0()
                                                    .truncate()
                                                    .text_xs()
                                                    .text_color(cx.theme().muted_foreground)
                                                    .child(content_type),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .id("publish-payload")
                                            .test_support()
                                            .flex_1()
                                            .min_h_0()
                                            .min_w_0()
                                            .child(self.publish_payload_view(cx)),
                                    ),
                            )
                            .child(
                                div()
                                    .h_flex()
                                    .flex_none()
                                    .text_sm()
                                    .child(
                                        div()
                                            .h_flex()
                                            .flex_none()
                                            .gap_1()
                                            .child(
                                                div()
                                                    .id("publish-qos-field")
                                                    .test_support()
                                                    .h_flex()
                                                    .flex_none()
                                                    .gap_1()
                                                    .child("QoS")
                                                    .child(
                                                        Select::new(&self.publish_qos)
                                                            .id("publish-qos")
                                                            .small()
                                                            .w(rems(3.5))
                                                            .accessibility_label("Publish QoS"),
                                                    ),
                                            )
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
                                    .child(div().flex_1())
                                    .child(
                                        div()
                                            .h_flex()
                                            .flex_none()
                                            .gap_1()
                                            .child(
                                                Button::new("load-publish-file")
                                                    .small()
                                                    .icon(AssetIconName::Upload)
                                                    .tooltip("Load file")
                                                    .accessibility_label("Load payload from file")
                                                    .disabled(self.publish_pending || self.publish_file_task.is_some())
                                                    .loading(self.publish_file_task.is_some())
                                                    .on_click(cx.listener(|view, _, window, cx| view.choose_publish_file(window, cx))),
                                            )
                                            .child(
                                                Button::new("publish-message")
                                                    .small()
                                                    .label("Publish")
                                                    .disabled(!enabled)
                                                    .loading(self.publish_pending)
                                                    .tooltip("Publish message (Ctrl+Enter)")
                                                    .on_click(cx.listener(|view, _, window, cx| view.publish_message(window, cx))),
                                            ),
                                    ),
                            )
                            .when(!connected, |form| {
                                form.child(
                                    div()
                                        .flex_none()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child("Connect to a broker to publish."),
                                )
                            }),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{PayloadContent, PublishFile, file_content_type, json_error_range};

    #[test]
    fn file_content_type_recognizes_image_extensions_and_binary_fallback() {
        for (name, expected) in [
            ("photo.PNG", "image/png"),
            ("photo.jpeg", "image/jpeg"),
            ("photo.jpg", "image/jpeg"),
            ("animation.GIF", "image/gif"),
            ("photo.webp", "image/webp"),
            ("drawing.svg", "image/svg+xml"),
            ("scan.tiff", "image/tiff"),
            ("icon.ico", "image/ico"),
            ("data.json", "application/json"),
            ("data.csv", "text/csv"),
            ("data.unknown", "application/octet-stream"),
            ("no-extension", "application/octet-stream"),
        ] {
            assert_eq!(file_content_type(Path::new(name)), expected, "{name}");
        }
    }

    #[test]
    fn data_files_preserve_binary_text_and_empty_bytes_without_formatting() {
        let directory = tempfile::tempdir().unwrap();
        for (name, payload) in [
            ("raw.bin", b"\x00\xff\x80\r\n".as_slice()),
            ("data.json", b"{\r\n\"value\":1}\r\n".as_slice()),
            ("empty.txt", b"".as_slice()),
        ] {
            let path = directory.path().join(name);
            std::fs::write(&path, payload).unwrap();
            let file = PublishFile::read(&path).unwrap();
            assert_eq!(file.payload, payload);
            assert_eq!(file.name.as_str(), name);
            assert!(file.image.is_none());
        }
    }

    #[test]
    fn image_file_preview_uses_the_same_unmodified_bytes_as_the_payload() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("image.SVG");
        let payload = br#"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10"/>"#;
        std::fs::write(&path, payload).unwrap();
        let file = PublishFile::read(&path).unwrap();
        assert_eq!(file.content_type, "image/svg+xml");
        assert_eq!(file.image.as_ref().unwrap().bytes(), file.payload);
        assert_eq!(file.payload, payload);
    }

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
