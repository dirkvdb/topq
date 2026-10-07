//! Latest payloads, aggregate topic counts, and tree traversal.

use std::{
    collections::{BTreeMap, BTreeSet, btree_map::Entry},
    time::{Duration, Instant},
};

use bytes::Bytes;
use chrono::{DateTime, Local};

pub const FLASH_DURATION: Duration = Duration::from_millis(600);
const FLASH_FADE_IN: Duration = Duration::from_millis(100);

/// Semantic MQTT 5 publish metadata, excluding connection-scoped topic aliases.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct MessageProperties {
    content_type: Option<String>,
    payload_format_indicator: Option<u8>,
    message_expiry_interval: Option<u32>,
    response_topic: Option<String>,
    correlation_data: Option<Bytes>,
    user_properties: Vec<(String, String)>,
    subscription_identifiers: Vec<usize>,
}

impl MessageProperties {
    pub(crate) fn from_publish(properties: Option<rumqttc::v5::mqttbytes::v5::PublishProperties>) -> Self {
        let Some(properties) = properties else {
            return Self::default();
        };
        Self {
            content_type: properties.content_type,
            payload_format_indicator: properties.payload_format_indicator,
            message_expiry_interval: properties.message_expiry_interval,
            response_topic: properties.response_topic,
            correlation_data: properties.correlation_data,
            user_properties: properties.user_properties,
            subscription_identifiers: properties.subscription_identifiers,
        }
    }

    pub fn content_type(&self) -> Option<&str> {
        self.content_type.as_deref()
    }

    #[cfg_attr(not(test), expect(dead_code, reason = "Preserved MQTT 5 metadata for future inspector fields"))]
    pub fn payload_format_indicator(&self) -> Option<u8> {
        self.payload_format_indicator
    }

    #[cfg_attr(not(test), expect(dead_code, reason = "Preserved MQTT 5 metadata for future inspector fields"))]
    pub fn message_expiry_interval(&self) -> Option<u32> {
        self.message_expiry_interval
    }

    #[cfg_attr(not(test), expect(dead_code, reason = "Preserved MQTT 5 metadata for future inspector fields"))]
    pub fn response_topic(&self) -> Option<&str> {
        self.response_topic.as_deref()
    }

    #[cfg_attr(not(test), expect(dead_code, reason = "Preserved MQTT 5 metadata for future inspector fields"))]
    pub fn correlation_data(&self) -> Option<&[u8]> {
        self.correlation_data.as_deref()
    }

    #[cfg_attr(not(test), expect(dead_code, reason = "Preserved MQTT 5 metadata for future inspector fields"))]
    pub fn user_properties(&self) -> &[(String, String)] {
        &self.user_properties
    }

    #[cfg_attr(not(test), expect(dead_code, reason = "Preserved MQTT 5 metadata for future inspector fields"))]
    pub fn subscription_identifiers(&self) -> &[usize] {
        &self.subscription_identifiers
    }
}

pub struct Message {
    pub topic: String,
    pub payload: Bytes,
    pub qos: u8,
    pub retained: bool,
    pub received_at: DateTime<Local>,
    pub properties: MessageProperties,
}

pub struct TopicValue {
    pub payload: Bytes,
    pub qos: u8,
    pub retained: bool,
    pub received_at: DateTime<Local>,
    pub messages: u64,
    pub properties: MessageProperties,
}

impl TopicValue {
    pub fn display_payload(&self) -> String {
        match std::str::from_utf8(&self.payload) {
            Ok(text) => serde_json::from_str::<serde_json::Value>(text)
                .ok()
                .and_then(|value| serde_json::to_string_pretty(&value).ok())
                .unwrap_or_else(|| text.to_owned()),
            Err(_) => {
                const HEX: &[u8; 16] = b"0123456789abcdef";
                let mut text = String::with_capacity(self.payload.len().saturating_mul(3));
                for (index, byte) in self.payload.iter().enumerate() {
                    if index > 0 {
                        text.push(if index % 16 == 0 { '\n' } else { ' ' });
                    }
                    text.push(char::from(HEX[usize::from(byte >> 4)]));
                    text.push(char::from(HEX[usize::from(byte & 0x0f)]));
                }
                text
            }
        }
    }

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

#[derive(Default)]
pub struct TopicNode {
    pub children: BTreeSet<String>,
    pub value: Option<TopicValue>,
    pub topics: usize,
    pub messages: u64,
    pub updated: Option<Instant>,
    flash_from: f32,
}

impl TopicNode {
    pub fn flashing(&self, now: Instant) -> bool {
        self.updated.is_some_and(|updated| now.duration_since(updated) < FLASH_DURATION)
    }

    pub fn flash_amount(&self, now: Instant) -> f32 {
        let Some(updated) = self.updated else {
            return 0.;
        };
        flash_amount(now.saturating_duration_since(updated), self.flash_from)
    }
}

pub(crate) fn flash_amount(elapsed: Duration, from: f32) -> f32 {
    let smooth = |progress: f32| {
        let progress = progress.clamp(0., 1.);
        progress * progress * (3. - 2. * progress)
    };
    if elapsed < FLASH_FADE_IN {
        from + (1. - from) * smooth(elapsed.as_secs_f32() / FLASH_FADE_IN.as_secs_f32())
    } else {
        1. - smooth((elapsed - FLASH_FADE_IN).as_secs_f32() / (FLASH_DURATION - FLASH_FADE_IN).as_secs_f32())
    }
}

#[derive(Default)]
pub struct TopicStore {
    pub nodes: BTreeMap<String, TopicNode>,
    roots: BTreeSet<String>,
    pub topics: usize,
    pub messages: u64,
}

impl TopicStore {
    /// Updates the latest value and ancestor counts, returning whether the tree changed.
    ///
    /// Zero-length publishes remove cached values, matching retained-message deletion.
    /// Empty levels are preserved: `a`, `a/`, `/a` and `a//b` are distinct MQTT topics.
    pub fn receive(&mut self, message: Message, now: Instant) -> bool {
        // Brokers normally unset retain when forwarding deletions to existing subscribers.
        if message.payload.is_empty() {
            return self.remove(&message.topic);
        }
        let new_topic = self.nodes.get(&message.topic).is_none_or(|node| node.value.is_none());
        let mut new_nodes = false;
        let root = message.topic.split_once('/').map_or(message.topic.as_str(), |(root, _)| root);
        self.roots.insert(root.to_owned());
        let mut ends = message
            .topic
            .match_indices('/')
            .map(|(index, _)| index)
            .chain(std::iter::once(message.topic.len()))
            .peekable();
        while let Some(end) = ends.next() {
            let entry = self.nodes.entry(message.topic[..end].to_owned());
            new_nodes |= matches!(&entry, Entry::Vacant(_));
            let node = entry.or_default();
            node.messages += 1;
            node.topics += usize::from(new_topic);
            // Resume from the current color when another message interrupts the pulse.
            node.flash_from = node.flash_amount(now);
            node.updated = Some(now);
            if let Some(&child_end) = ends.peek() {
                node.children.insert(message.topic[..child_end].to_owned());
            } else {
                let messages = node.value.as_ref().map_or(1, |value| value.messages + 1);
                node.value = Some(TopicValue {
                    payload: message.payload,
                    qos: message.qos,
                    retained: message.retained,
                    received_at: message.received_at,
                    messages,
                    properties: message.properties,
                });
                break;
            }
        }
        self.topics += usize::from(new_topic);
        self.messages += 1;
        new_nodes
    }

    fn remove(&mut self, path: &str) -> bool {
        let Some(node) = self.nodes.get_mut(path) else {
            return false;
        };
        let Some(value) = node.value.take() else {
            return false;
        };
        self.topics -= 1;
        self.messages -= value.messages;

        let mut current = path;
        while let Some(node) = self.nodes.get_mut(current) {
            node.topics -= 1;
            node.messages -= value.messages;
            let prune = node.value.is_none() && node.children.is_empty();
            if prune {
                self.nodes.remove(current);
            }
            if let Some((parent, _)) = current.rsplit_once('/') {
                if prune && let Some(node) = self.nodes.get_mut(parent) {
                    node.children.remove(current);
                }
                current = parent;
            } else {
                if prune {
                    self.roots.remove(current);
                }
                break;
            }
        }
        true
    }

    /// Exact publishable names in a branch, including intermediate levels but not the empty root.
    pub fn branch_paths(&self, path: &str) -> Vec<String> {
        if !self.nodes.contains_key(path) {
            return Vec::new();
        }
        self.nodes
            .range(path.to_owned()..)
            .take_while(|(candidate, _)| candidate.starts_with(path))
            .filter(|(candidate, _)| {
                !candidate.is_empty()
                    && candidate
                        .strip_prefix(path)
                        .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with('/'))
            })
            .map(|(candidate, _)| candidate.clone())
            .collect()
    }

    pub fn visible_paths(&self, expanded: &BTreeSet<String>) -> Vec<&str> {
        let mut paths = Vec::new();
        let mut stack: Vec<_> = self.roots.iter().rev().collect();
        while let Some(path) = stack.pop() {
            paths.push(path.as_str());
            if expanded.contains(path) {
                stack.extend(self.nodes[path].children.iter().rev());
            }
        }
        paths
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(topic: &str, payload: &[u8]) -> Message {
        Message {
            topic: topic.into(),
            payload: Bytes::copy_from_slice(payload),
            qos: 1,
            retained: true,
            received_at: Local::now(),
            properties: MessageProperties::default(),
        }
    }

    fn publish_properties() -> rumqttc::v5::mqttbytes::v5::PublishProperties {
        rumqttc::v5::mqttbytes::v5::PublishProperties {
            content_type: Some("application/json".into()),
            payload_format_indicator: Some(1),
            message_expiry_interval: Some(30),
            topic_alias: Some(7),
            response_topic: Some("response/topic".into()),
            correlation_data: Some(Bytes::from_static(&[0x00, 0xff])),
            user_properties: vec![("source".into(), "first".into()), ("source".into(), "second".into())],
            subscription_identifiers: vec![3, 9],
        }
    }

    #[test]
    fn receive_stores_all_semantic_publish_properties() {
        let mut store = TopicStore::default();
        let mut incoming = message("home/temperature", b"21");
        incoming.properties = MessageProperties::from_publish(Some(publish_properties()));
        store.receive(incoming, Instant::now());

        let properties = &store.nodes["home/temperature"].value.as_ref().unwrap().properties;
        assert_eq!(properties.content_type(), Some("application/json"));
        assert_eq!(properties.payload_format_indicator(), Some(1));
        assert_eq!(properties.message_expiry_interval(), Some(30));
        assert_eq!(properties.response_topic(), Some("response/topic"));
        assert_eq!(properties.correlation_data(), Some([0x00, 0xff].as_slice()));
        assert_eq!(
            properties.user_properties(),
            [("source".into(), "first".into()), ("source".into(), "second".into())]
        );
        assert_eq!(properties.subscription_identifiers(), [3, 9]);
    }

    #[test]
    fn receive_replaces_properties_instead_of_merging_with_previous_metadata() {
        let mut store = TopicStore::default();
        let mut incoming = message("home/temperature", b"21");
        incoming.properties = MessageProperties::from_publish(Some(publish_properties()));
        store.receive(incoming, Instant::now());

        let replacement = rumqttc::v5::mqttbytes::v5::PublishProperties {
            content_type: Some("text/plain".into()),
            user_properties: vec![("source".into(), "new".into())],
            ..Default::default()
        };
        let expected = MessageProperties::from_publish(Some(replacement.clone()));
        let mut incoming = message("home/temperature", b"22");
        incoming.properties = MessageProperties::from_publish(Some(replacement));
        store.receive(incoming, Instant::now());

        let value = store.nodes["home/temperature"].value.as_ref().unwrap();
        assert_eq!(value.properties, expected);
        assert_eq!(value.payload.as_ref(), b"22");
        assert_eq!(value.messages, 2);
        assert_eq!((store.topics, store.messages), (1, 2));
    }

    #[test]
    fn receive_clears_metadata_when_publish_properties_are_absent_or_empty() {
        for properties in [None, Some(Default::default())] {
            let mut store = TopicStore::default();
            let mut incoming = message("home/temperature", b"21");
            incoming.properties = MessageProperties::from_publish(Some(publish_properties()));
            store.receive(incoming, Instant::now());

            let mut incoming = message("home/temperature", b"22");
            incoming.properties = MessageProperties::from_publish(properties);
            store.receive(incoming, Instant::now());

            assert_eq!(
                store.nodes["home/temperature"].value.as_ref().unwrap().properties,
                MessageProperties::default()
            );
        }
    }

    #[test]
    fn publish_properties_discard_connection_scoped_topic_alias() {
        let properties = rumqttc::v5::mqttbytes::v5::PublishProperties {
            topic_alias: Some(7),
            ..Default::default()
        };
        assert_eq!(MessageProperties::from_publish(Some(properties)), MessageProperties::default());
    }

    #[test]
    fn publish_properties_do_not_override_payload_format_detection_or_display() {
        for (payload, format, display) in [
            (br#"{"on":true}"#.as_slice(), "JSON", "{\n  \"on\": true\n}"),
            (b"hello".as_slice(), "Text", "hello"),
            (&[0xff, 0x00], "Binary · hex", "ff 00"),
        ] {
            let mut store = TopicStore::default();
            let mut incoming = message("test", payload);
            incoming.properties = MessageProperties::from_publish(Some(publish_properties()));
            store.receive(incoming, Instant::now());
            let value = store.nodes["test"].value.as_ref().unwrap();
            assert_eq!(value.format_label(), format);
            assert_eq!(value.display_payload(), display);
        }
    }

    #[test]
    fn branch_paths_include_exact_topic_and_descendants_but_not_similar_prefixes() {
        let mut store = TopicStore::default();
        for topic in ["a", "a/", "a//測定値", "a/b/c", "ab/x", "a-b/x", "z"] {
            store.receive(message(topic, b"value"), Instant::now());
        }
        assert_eq!(store.branch_paths("a"), ["a", "a/", "a//測定値", "a/b", "a/b/c"]);
        assert_eq!(store.branch_paths("a/"), ["a/", "a//測定値"]);
        assert!(store.branch_paths("missing").is_empty());
    }

    #[test]
    fn branch_paths_preserve_leading_empty_levels_without_publishing_an_empty_name() {
        let mut store = TopicStore::default();
        for topic in ["/a", "//b", "a"] {
            store.receive(message(topic, b"value"), Instant::now());
        }
        assert_eq!(store.branch_paths(""), ["/", "//b", "/a"]);
    }

    #[test]
    fn aggregates_updates_without_double_counting_topics() {
        let mut store = TopicStore::default();
        let now = Instant::now();
        store.receive(message("home/temperature", b"20"), now);
        store.receive(message("home/temperature", b"21"), now);
        store.receive(message("home", b"online"), now);
        assert_eq!((store.topics, store.messages), (2, 3));
        assert_eq!((store.nodes["home"].topics, store.nodes["home"].messages), (2, 3));
        let value = store.nodes["home/temperature"].value.as_ref().unwrap();
        assert_eq!(value.payload.as_ref(), b"21");
        assert_eq!(value.messages, 2);
        assert!(store.nodes["home"].flashing(now));
        assert!(!store.nodes["home"].flashing(now + FLASH_DURATION));
    }

    #[test]
    fn preserves_empty_topic_levels_and_sorted_expansion() {
        let mut store = TopicStore::default();
        for topic in ["a/z", "a/", "a//b", "/a", "a", "a/b"] {
            store.receive(message(topic, b"value"), Instant::now());
        }
        assert_eq!(store.topics, 6);
        assert_eq!(store.nodes["a"].topics, 5);
        let expanded = BTreeSet::from(["".into(), "a".into(), "a/".into()]);
        let paths = store.visible_paths(&expanded);
        assert_eq!(paths, ["", "/a", "a", "a/", "a//b", "a/b", "a/z"]);
        assert_eq!(store.visible_paths(&BTreeSet::new()).len(), 2);
    }

    #[test]
    fn empty_publishes_remove_values_and_prune_empty_ancestors_without_requiring_retain() {
        let mut store = TopicStore::default();
        let now = Instant::now();
        for topic in ["home/branch/a", "home/branch/a", "home/branch/b", "home/sibling", "home2/a"] {
            store.receive(message(topic, b"value"), now);
        }
        for topic in ["home", "home/branch", "home/branch/a", "home/branch/b"] {
            let mut deletion = message(topic, b"");
            deletion.retained = false;
            store.receive(deletion, now);
        }
        assert_eq!(store.branch_paths("home"), ["home", "home/sibling"]);
        assert_eq!(store.visible_paths(&BTreeSet::new()), ["home", "home2"]);
        assert_eq!((store.topics, store.messages), (2, 2));
        assert_eq!((store.nodes["home"].topics, store.nodes["home"].messages), (1, 1));
        assert!(!store.receive(message("home/branch/a", b""), now));
        assert!(!store.receive(message("unknown", b""), now));
        assert_eq!((store.topics, store.messages), (2, 2));
    }

    #[test]
    fn deleting_a_parent_value_preserves_its_children_and_deleting_children_preserves_a_parent_value() {
        let mut store = TopicStore::default();
        let now = Instant::now();
        for topic in ["a", "a/b", "a/b/c", "a/b/c"] {
            store.receive(message(topic, b"value"), now);
        }
        assert!(store.receive(message("a/b", b""), now));
        assert!(store.nodes["a/b"].value.is_none());
        assert!(store.nodes["a/b/c"].value.is_some());
        assert_eq!((store.nodes["a"].topics, store.nodes["a"].messages), (2, 3));
        assert!(store.receive(message("a/b/c", b""), now));
        assert_eq!(store.branch_paths("a"), ["a"]);
        assert_eq!((store.topics, store.messages), (1, 1));
        assert!(store.nodes["a"].value.is_some());
        assert!(store.receive(message("a", b""), now));
        assert!(store.nodes.is_empty());
        assert!(store.visible_paths(&BTreeSet::new()).is_empty());
        assert_eq!((store.topics, store.messages), (0, 0));
        assert!(store.receive(message("a/b/c", b"new"), now));
        assert_eq!(store.branch_paths("a"), ["a", "a/b", "a/b/c"]);
        assert_eq!((store.topics, store.messages), (1, 1));
    }

    #[test]
    fn deleting_confirmed_names_preserves_new_descendants_and_empty_unicode_levels() {
        let mut store = TopicStore::default();
        let now = Instant::now();
        for topic in ["/測定値/", "/測定値//温度", "other"] {
            store.receive(message(topic, b"value"), now);
        }
        let confirmed = store.branch_paths("");
        store.receive(message("/測定値//new", b"live"), now);
        for topic in confirmed {
            store.receive(message(&topic, b""), now);
        }
        assert_eq!(store.branch_paths(""), ["/測定値", "/測定値/", "/測定値//new"]);
        assert_eq!((store.topics, store.messages), (2, 2));
        store.receive(message("/測定値//new", b""), now);
        assert!(!store.nodes.contains_key(""));
        assert_eq!(store.visible_paths(&BTreeSet::new()), ["other"]);
    }

    #[test]
    fn displays_json_text_and_binary_payloads() {
        for (payload, expected) in [
            (br#"{"on":true}"#.as_slice(), "{\n  \"on\": true\n}"),
            (b"hello".as_slice(), "hello"),
            (&[0xff, 0x00], "ff 00"),
        ] {
            let mut store = TopicStore::default();
            store.receive(message("test", payload), Instant::now());
            assert_eq!(store.nodes["test"].value.as_ref().unwrap().display_payload(), expected);
        }
    }

    #[test]
    fn display_payload_wraps_binary_bytes_after_sixteen_columns() {
        let mut store = TopicStore::default();
        store.receive(message("test", &[0xff; 17]), Instant::now());
        assert_eq!(
            store.nodes["test"].value.as_ref().unwrap().display_payload(),
            "ff ff ff ff ff ff ff ff ff ff ff ff ff ff ff ff\nff"
        );
    }
}
