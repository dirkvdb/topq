//! Latest payloads, aggregate topic counts, and tree traversal.

use std::{
    collections::{BTreeMap, BTreeSet, btree_map::Entry},
    time::{Duration, Instant},
};

use bytes::Bytes;
use chrono::{DateTime, Local};

pub const FLASH_DURATION: Duration = Duration::from_millis(450);
const FLASH_FADE_IN: Duration = Duration::from_millis(100);

pub struct Message {
    pub topic: String,
    pub payload: Bytes,
    pub qos: u8,
    pub retained: bool,
    pub received_at: DateTime<Local>,
}

pub struct TopicValue {
    pub payload: Bytes,
    pub qos: u8,
    pub retained: bool,
    pub received_at: DateTime<Local>,
    pub messages: u64,
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
        let elapsed = now.saturating_duration_since(updated);
        let smooth = |progress: f32| {
            let progress = progress.clamp(0., 1.);
            progress * progress * (3. - 2. * progress)
        };
        if elapsed < FLASH_FADE_IN {
            self.flash_from + (1. - self.flash_from) * smooth(elapsed.as_secs_f32() / FLASH_FADE_IN.as_secs_f32())
        } else {
            1. - smooth((elapsed - FLASH_FADE_IN).as_secs_f32() / (FLASH_DURATION - FLASH_FADE_IN).as_secs_f32())
        }
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
    /// Updates the latest value and ancestor counts, returning whether new nodes were added.
    ///
    /// Empty levels are preserved: `a`, `a/`, `/a` and `a//b` are distinct MQTT topics.
    pub fn receive(&mut self, message: Message, now: Instant) -> bool {
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
                });
                break;
            }
        }
        self.topics += usize::from(new_topic);
        self.messages += 1;
        new_nodes
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
    fn displays_json_text_empty_and_binary_payloads() {
        for (payload, expected) in [
            (br#"{"on":true}"#.as_slice(), "{\n  \"on\": true\n}"),
            (b"hello".as_slice(), "hello"),
            (b"".as_slice(), ""),
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
