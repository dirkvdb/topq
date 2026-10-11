//! Presentation boundary for errors. Backends expose codes and data, never translated copy.

/// Renders the current English fallback without exposing diagnostic sources.
/// Locale selection and translation catalogs can be introduced here without changing services.
pub(super) trait LocalizedError {
    fn localized_message(&self) -> String;
}

impl crate::topics::TopicValue {
    pub(crate) fn preview(&self, max_chars: Option<usize>) -> Option<String> {
        match std::str::from_utf8(&self.payload) {
            Ok(text) => {
                if max_chars.is_some_and(|limit| text.chars().nth(limit).is_some()) {
                    return None;
                }
                Some(
                    text.chars()
                        .take(max_chars.unwrap_or(100))
                        .map(|ch| if ch.is_control() { ' ' } else { ch })
                        .collect(),
                )
            }
            Err(_) if max_chars.is_some_and(|limit| self.payload.len() > limit) => None,
            Err(_) => Some(format!("<{} binary bytes>", self.payload.len())),
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
    fn inline_payload_length_limit_is_inclusive_and_counts_unicode_characters() {
        for (payload, limit, expected) in [
            ("123456789", 10, Some("123456789")),
            ("1234567890", 10, Some("1234567890")),
            ("12345678901", 10, None),
            (r#"{"v":true}"#, 10, Some(r#"{"v":true}"#)),
            (r#"{"v":false}"#, 10, None),
            ("éééééééééé", 10, Some("éééééééééé")),
            ("ééééééééééé", 10, None),
            ("hello\n", 6, Some("hello ")),
            ("hello\n", 5, None),
            ("", 0, Some("")),
            ("a", 0, None),
        ] {
            let value = crate::topics::TopicValue {
                payload: bytes::Bytes::copy_from_slice(payload.as_bytes()),
                qos: 0,
                retained: false,
                received_at: chrono::Local::now(),
                messages: 1,
                properties: MessageProperties::default(),
            };
            assert_eq!(
                value.preview(Some(limit)).as_deref(),
                expected,
                "payload={payload:?}, limit={limit}"
            );
        }
    }

    #[test]
    fn inline_payload_length_limit_does_not_truncate_eligible_text_to_the_old_preview_cap() {
        let payload = "a".repeat(101);
        let value = crate::topics::TopicValue {
            payload: bytes::Bytes::copy_from_slice(payload.as_bytes()),
            qos: 0,
            retained: false,
            received_at: chrono::Local::now(),
            messages: 1,
            properties: MessageProperties::default(),
        };
        assert_eq!(value.preview(Some(101)).as_deref(), Some(payload.as_str()));
        assert!(value.preview(Some(100)).is_none());
        assert_eq!(value.preview(None).unwrap().len(), 100);
    }

    #[test]
    fn inline_payload_binary_length_limit_uses_payload_bytes() {
        let value = crate::topics::TopicValue {
            payload: bytes::Bytes::from_static(&[0xff, 0x00]),
            qos: 0,
            retained: false,
            received_at: chrono::Local::now(),
            messages: 1,
            properties: MessageProperties::default(),
        };
        assert_eq!(value.preview(Some(2)).as_deref(), Some("<2 binary bytes>"));
        assert!(value.preview(Some(1)).is_none());
        assert!(value.preview(Some(0)).is_none());
        assert_eq!(value.preview(None).as_deref(), Some("<2 binary bytes>"));
    }

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
            assert_eq!(value.preview(None).as_deref(), Some(preview));
        }
    }
}
