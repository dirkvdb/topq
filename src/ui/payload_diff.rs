//! Semantic JSON changes expressed as byte ranges in the current payload.

use std::ops::Range;

use serde::de::{DeserializeOwned, IgnoredAny};
use serde_json::Value;

/// Returns sorted, non-overlapping byte ranges in `current` for added or modified JSON.
///
/// `current` is expected to be formatted with `serde_json::to_string_pretty(Value)`.
/// Objects are compared by key, recursively: new members include the quoted key,
/// colon, and entire value, while existing members highlight only changed values.
/// Arrays are compared by index, not by identity or an edit-distance algorithm;
/// inserting or removing an element can therefore highlight shifted elements.
/// Added array elements and type changes highlight the entire new value.
/// Removals have no current-text range. Invalid input (including an empty initial
/// `previous` payload) produces no ranges.
pub(super) fn changed_ranges(previous: &str, current: &str) -> Vec<Range<usize>> {
    let Ok(previous) = serde_json::from_str::<Value>(previous) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<Value>(current) else {
        return Vec::new();
    };
    if previous == value {
        return Vec::new();
    }

    let mut walker = DiffWalker {
        text: current,
        offset: 0,
        ranges: Vec::new(),
    };
    if walker.walk(&previous, &value).is_none() {
        return Vec::new();
    }
    walker.skip_whitespace();
    if walker.offset != current.len() {
        return Vec::new();
    }
    walker.ranges
}

struct DiffWalker<'a> {
    text: &'a str,
    offset: usize,
    ranges: Vec<Range<usize>>,
}

impl DiffWalker<'_> {
    fn walk(&mut self, previous: &Value, current: &Value) -> Option<()> {
        if previous == current {
            self.token::<IgnoredAny>()?;
            return Some(());
        }

        match (previous, current) {
            (Value::Object(previous), Value::Object(current)) => {
                self.consume(b'{')?;
                for ix in 0..current.len() {
                    if ix > 0 {
                        self.consume(b',')?;
                    }
                    self.skip_whitespace();
                    let start = self.offset;
                    let key = self.token::<String>()?;
                    self.consume(b':')?;
                    let value = current.get(&key)?;
                    if let Some(previous) = previous.get(&key) {
                        self.walk(previous, value)?;
                    } else {
                        self.token::<IgnoredAny>()?;
                        self.ranges.push(start..self.offset);
                    }
                }
                self.consume(b'}')?;
            }
            (Value::Array(previous), Value::Array(current)) => {
                self.consume(b'[')?;
                for (ix, value) in current.iter().enumerate() {
                    if ix > 0 {
                        self.consume(b',')?;
                    }
                    if let Some(previous) = previous.get(ix) {
                        self.walk(previous, value)?;
                    } else {
                        self.highlight_value()?;
                    }
                }
                self.consume(b']')?;
            }
            _ => self.highlight_value()?,
        }
        Some(())
    }

    fn highlight_value(&mut self) -> Option<()> {
        self.skip_whitespace();
        let start = self.offset;
        self.token::<IgnoredAny>()?;
        self.ranges.push(start..self.offset);
        Some(())
    }

    fn token<T: DeserializeOwned>(&mut self) -> Option<T> {
        self.skip_whitespace();
        // Read lengths from the actual text, rather than reserializing decoded
        // strings or numbers: escapes and floating-point spellings affect offsets.
        let mut stream = serde_json::Deserializer::from_str(self.text.get(self.offset..)?).into_iter::<T>();
        let value = stream.next()?.ok()?;
        self.offset += stream.byte_offset();
        Some(value)
    }

    fn consume(&mut self, byte: u8) -> Option<()> {
        self.skip_whitespace();
        if self.text.as_bytes().get(self.offset) != Some(&byte) {
            return None;
        }
        self.offset += 1;
        Some(())
    }

    fn skip_whitespace(&mut self) {
        while self.text.as_bytes().get(self.offset).is_some_and(u8::is_ascii_whitespace) {
            self.offset += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::changed_ranges;
    use serde_json::{Value, json};

    fn assert_diff(previous: &str, current: Value, highlighted: &[&str]) {
        let current = serde_json::to_string_pretty(&current).expect("test JSON should serialize");
        let mut expected: Vec<_> = highlighted
            .iter()
            .map(|text| {
                let start = current.find(text).expect("expected highlight should occur in current JSON");
                start..start + text.len()
            })
            .collect();
        expected.sort_by_key(|range| range.start);
        assert_eq!(changed_ranges(previous, &current), expected, "current JSON: {current}");
    }

    #[test]
    fn unchanged_payload_has_no_ranges() {
        assert_diff(
            r#"{"nested":{"array":[true,null,"é",{},[]]},"number":12}"#,
            json!({"nested": {"array": [true, null, "é", {}, []]}, "number": 12}),
            &[],
        );
    }

    #[test]
    fn object_order_and_equivalent_string_escapes_are_unchanged() {
        assert_diff(r#"{"z":"\u00e9","a":true}"#, json!({"a": true, "z": "é"}), &[]);
    }

    #[test]
    fn nested_changes_highlight_only_modified_values() {
        assert_diff(
            r#"{"device":{"reading":12,"status":"old","stable":true},"other":null}"#,
            json!({"device": {"reading": 13, "status": "new", "stable": true}, "other": null}),
            &["13", "\"new\""],
        );
    }

    #[test]
    fn inserted_sibling_highlights_key_and_value_but_not_existing_members() {
        assert_diff(
            r#"{"middle":true,"z":false}"#,
            json!({"a": "added", "middle": true, "z": false}),
            &["\"a\": \"added\""],
        );
    }

    #[test]
    fn inserted_subtree_is_one_range_including_its_key() {
        assert_diff(
            r#"{"stable":0}"#,
            json!({"added": {"items": [1, {}]}, "stable": 0}),
            &["\"added\": {\n    \"items\": [\n      1,\n      {}\n    ]\n  }"],
        );
    }

    #[test]
    fn removed_siblings_do_not_highlight_surviving_members() {
        assert_diff(r#"{"a":1,"middle":true,"z":[1,2]}"#, json!({"middle": true}), &[]);
    }

    #[test]
    fn insertion_and_removal_do_not_hide_a_modified_sibling() {
        assert_diff(
            r#"{"deleted":0,"modified":1,"stable":"same"}"#,
            json!({"added": false, "modified": 2, "stable": "same"}),
            &["\"added\": false", "2"],
        );
    }

    #[test]
    fn nested_insertions_and_removals_preserve_unchanged_members() {
        assert_diff(
            r#"{"nested":{"gone":1,"stable":2},"other":[]}"#,
            json!({"nested": {"new": {}, "stable": 2}, "other": []}),
            &["\"new\": {}"],
        );
    }

    #[test]
    fn arrays_recurse_by_index_and_highlight_appended_subtrees() {
        assert_diff(
            r#"[{"value":1,"stable":true},[2,3]]"#,
            json!([{"value": 4, "stable": true}, [2, 5], {"new": []}]),
            &["4", "5", "{\n    \"new\": []\n  }"],
        );
    }

    #[test]
    fn array_insertion_highlights_shifted_values_by_index() {
        assert_diff(r#"["a","b"]"#, json!(["a", "inserted", "b"]), &["\"inserted\"", "\"b\""]);
    }

    #[test]
    fn array_removal_highlights_shifted_values_by_index() {
        assert_diff(r#"["a","b","c"]"#, json!(["a", "c"]), &["\"c\""]);
    }

    #[test]
    fn removing_array_tail_has_no_ranges() {
        assert_diff(r#"[1,2,{"last":3}]"#, json!([1, 2]), &[]);
    }

    #[test]
    fn escaped_unicode_keys_and_strings_use_current_byte_offsets() {
        assert_diff(
            r#"{"aé":"unchanged🦀","quote\"slash\\\n雪":"old","z":0}"#,
            json!({"aé": "unchanged🦀", "quote\"slash\\\n雪": "new\"\\\n🦀", "z": 42}),
            &["\"new\\\"\\\\\\n🦀\"", "42"],
        );
    }

    #[test]
    fn added_escaped_unicode_key_includes_its_entire_value() {
        assert_diff(
            r#"{"aé":true}"#,
            json!({"aé": true, "雪\"\\\t": "🦀"}),
            &["\"雪\\\"\\\\\\t\": \"🦀\""],
        );
    }

    #[test]
    fn unicode_escaped_previous_key_matches_decoded_current_key() {
        assert_diff(r#"{"\u96ea":1}"#, json!({"雪": 2}), &["2"]);
    }

    #[test]
    fn empty_key_and_control_character_escapes_do_not_shift_following_ranges() {
        assert_diff(
            r#"{"":"\u0000\b\f\n\r\t","z":1}"#,
            json!({"": "\0\u{0008}\u{000c}\n\r\t", "z": 123}),
            &["123"],
        );
    }

    #[test]
    fn empty_containers_can_gain_members() {
        assert_diff(
            r#"{"array":[],"object":{}}"#,
            json!({"array": [null], "object": {"": []}}),
            &["null", "\"\": []"],
        );
    }

    #[test]
    fn containers_becoming_empty_have_no_ranges() {
        assert_diff(r#"{"array":[1],"object":{"gone":2}}"#, json!({"array": [], "object": {}}), &[]);
    }

    #[test]
    fn changed_root_scalars_highlight_the_entire_current_token() {
        for (previous, current) in [
            ("1", json!(123)),
            ("false", json!(true)),
            (r#""old""#, json!("é\"\\\n🦀")),
            ("0.5", json!(-1.25e30)),
        ] {
            let current = serde_json::to_string_pretty(&current).unwrap();
            assert_eq!(
                changed_ranges(previous, &current),
                vec![0..current.len()],
                "previous: {previous}, current: {current}"
            );
        }
    }

    #[test]
    fn type_changes_highlight_the_entire_new_value() {
        for (previous, current) in [
            ("null", json!({"items": [1, {}]})),
            ("{}", json!([{"nested": true}])),
            ("[]", json!({})),
            ("{}", json!([])),
            ("true", json!(null)),
            (r#"{"old":1}"#, json!("new")),
        ] {
            let current = serde_json::to_string_pretty(&current).unwrap();
            assert_eq!(
                changed_ranges(previous, &current),
                vec![0..current.len()],
                "previous: {previous}, current: {current}"
            );
        }
    }

    #[test]
    fn nested_type_change_excludes_the_existing_key_and_siblings() {
        assert_diff(
            r#"{"nested":false,"stable":{}}"#,
            json!({"nested": {"items": []}, "stable": {}}),
            &["{\n    \"items\": []\n  }"],
        );
    }

    #[test]
    fn numeric_spellings_do_not_shift_following_ranges() {
        assert_diff(
            r#"{"a":18446744073709551615,"b":1e-9,"c":-1.25e30,"z":0}"#,
            json!({"a": u64::MAX, "b": 1e-9, "c": -1.25e30, "z": 987}),
            &["987"],
        );
    }

    #[test]
    fn invalid_or_initial_previous_payload_has_no_ranges() {
        for previous in ["", "not JSON", "{", "{} trailing", r#"{"broken":tru}"#] {
            assert!(
                changed_ranges(previous, "{\n  \"value\": 1\n}").is_empty(),
                "previous: {previous:?}"
            );
        }
    }

    #[test]
    fn invalid_current_payload_has_no_ranges() {
        for current in ["", "not JSON", "[", "{} trailing", r#"{"value":tru}"#] {
            assert!(changed_ranges("{}", current).is_empty(), "current: {current:?}");
        }
    }
}
