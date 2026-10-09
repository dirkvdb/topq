//! Presentation boundary for errors. Backends expose codes and data, never translated copy.

/// Renders the current English fallback without exposing diagnostic sources.
/// Locale selection and translation catalogs can be introduced here without changing services.
pub(super) trait LocalizedError {
    fn localized_message(&self) -> String;
}

impl crate::topics::TopicValue {
    pub fn preview(&self) -> String {
        match std::str::from_utf8(&self.payload) {
            Ok(text) => text.chars().take(100).map(|ch| if ch.is_control() { ' ' } else { ch }).collect(),
            Err(_) => format!("<{} binary bytes>", self.payload.len()),
        }
    }

    pub fn format_label(&self) -> &'static str {
        match std::str::from_utf8(&self.payload) {
            Ok(text) if serde_json::from_str::<serde_json::Value>(text).is_ok() => "JSON",
            Ok(_) => "Text",
            Err(_) => "Binary · hex",
        }
    }
}

#[cfg(test)]
mod tests {

    use crate::topics::{Message, MessageProperties, TopicStore};

    #[test]
    fn payload_labels_and_binary_preview_are_ui_copy() {
        for (payload, label, preview) in [
            (br#"{"on":true}"#.as_slice(), "JSON", "{\"on\":true}"),
            (b"hello\n".as_slice(), "Text", "hello "),
            (&[0xff, 0x00], "Binary · hex", "<2 binary bytes>"),
        ] {
            let mut store = TopicStore::default();
            store.receive(
                Message {
                    topic: "test".into(),
                    payload: bytes::Bytes::copy_from_slice(payload),
                    qos: 0,
                    retained: false,
                    received_at: chrono::Local::now(),
                    properties: MessageProperties::default(),
                },
                std::time::Instant::now(),
            );
            let value = store.nodes["test"].value.as_ref().unwrap();
            assert_eq!(value.format_label(), label);
            assert_eq!(value.preview(), preview);
        }
    }
}
