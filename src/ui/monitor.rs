//! Numeric JSON fields and bounded live series owned by the explorer.

use std::collections::{BTreeMap, VecDeque};

use chrono::{DateTime, Local};
use gpui_kit::component::{
    ActiveTheme, IconName, Sizable, StyledExt,
    button::{Button, ButtonVariants, Toggle},
    chart::{AreaChart, LineChart},
    scroll::ScrollableElement,
    tag::Tag,
};
use gpui_kit::{AnyElement, Context, IntoElement, SharedString, TestSupportExt, div, prelude::*, rems};
use serde::de::{DeserializeOwned, IgnoredAny};
use serde_json::Value;

use super::Explorer;
use crate::topics::Message;

const SAMPLE_LIMIT: usize = 300;
const CHART_COLOR_COUNT: usize = 5;

fn axis_label(value: f64) -> String {
    // Kit measures the label to reserve its gutter but exposes no label-gap option.
    // An en space keeps the edge dot clear while preserving Kit's numeric formatting.
    if (value - value.round()).abs() < 0.001 {
        format!("{value:.0}\u{2002}")
    } else {
        format!("{value:.1}\u{2002}")
    }
}

/// A numeric leaf's RFC 6901 pointer and positions in the supplied JSON text.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct NumericField {
    pointer: String,
    line_offset: usize,
    key_offset: Option<usize>,
    value_offset: usize,
}

impl NumericField {
    pub(super) fn pointer(&self) -> &str {
        &self.pointer
    }

    /// The quoted key's start, or the number's start for array elements/root values.
    pub(super) fn offset(&self) -> usize {
        self.key_offset.unwrap_or(self.value_offset)
    }

    /// Start of the line containing `offset()`, including its indentation.
    #[cfg(test)]
    pub(super) fn line_offset(&self) -> usize {
        self.line_offset
    }

    #[cfg(test)]
    pub(super) fn key_offset(&self) -> Option<usize> {
        self.key_offset
    }

    #[cfg(test)]
    pub(super) fn value_offset(&self) -> usize {
        self.value_offset
    }
}

/// Returns numeric leaves in source order; invalid JSON produces no fields.
/// Offsets are UTF-8 byte offsets into `text`, not reserialized JSON positions.
pub(super) fn numeric_fields(text: &str) -> Vec<NumericField> {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return Vec::new();
    };
    let mut walker = NumericWalker {
        text,
        offset: 0,
        fields: Vec::new(),
    };
    if walker.walk(&value, "", None).is_none() {
        return Vec::new();
    }
    walker.skip_whitespace();
    if walker.offset != text.len() {
        return Vec::new();
    }
    walker.fields
}

struct NumericWalker<'a> {
    text: &'a str,
    offset: usize,
    fields: Vec<NumericField>,
}

impl NumericWalker<'_> {
    fn walk(&mut self, value: &Value, pointer: &str, key_offset: Option<usize>) -> Option<()> {
        match value {
            Value::Object(object) => {
                self.consume(b'{')?;
                for ix in 0..object.len() {
                    if ix > 0 {
                        self.consume(b',')?;
                    }
                    self.skip_whitespace();
                    let key_offset = self.offset;
                    let key = self.token::<String>()?;
                    self.consume(b':')?;
                    let escaped = key.replace('~', "~0").replace('/', "~1");
                    self.walk(object.get(&key)?, &format!("{pointer}/{escaped}"), Some(key_offset))?;
                }
                self.consume(b'}')?;
            }
            Value::Array(array) => {
                self.consume(b'[')?;
                for (ix, value) in array.iter().enumerate() {
                    if ix > 0 {
                        self.consume(b',')?;
                    }
                    self.walk(value, &format!("{pointer}/{ix}"), None)?;
                }
                self.consume(b']')?;
            }
            _ => {
                self.skip_whitespace();
                let value_offset = self.offset;
                self.token::<IgnoredAny>()?;
                if finite_number(value).is_some() {
                    let offset = key_offset.unwrap_or(value_offset);
                    let line_offset = self.text[..offset].rfind('\n').map_or(0, |ix| ix + 1);
                    self.fields.push(NumericField {
                        pointer: pointer.to_owned(),
                        line_offset,
                        key_offset,
                        value_offset,
                    });
                }
            }
        }
        Some(())
    }

    fn token<T: DeserializeOwned>(&mut self) -> Option<T> {
        self.skip_whitespace();
        // Consume the original spelling so Unicode and escapes preserve editor positions.
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

fn finite_number(value: &Value) -> Option<f64> {
    value.as_f64().filter(|value| value.is_finite())
}

#[derive(Clone)]
struct Sample {
    time: DateTime<Local>,
    value: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ChartKind {
    #[default]
    Line,
    Area,
}

struct Series {
    samples: VecDeque<Sample>,
    kind: ChartKind,
    color_ix: usize,
    smooth: bool,
}

impl Default for Series {
    fn default() -> Self {
        Self {
            samples: VecDeque::new(),
            kind: ChartKind::default(),
            color_ix: 0,
            smooth: true,
        }
    }
}

impl Series {
    fn push(&mut self, time: DateTime<Local>, value: f64) {
        if self.samples.len() == SAMPLE_LIMIT {
            self.samples.pop_front();
        }
        self.samples.push_back(Sample { time, value });
    }

    fn y_domain(&self) -> (f64, f64) {
        let (min, max) = self.samples.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(min, max), sample| {
            (min.min(sample.value), max.max(sample.value))
        });
        if !min.is_finite() || !max.is_finite() {
            return (-1.0, 1.0);
        }
        let padding = (min.abs().max(max.abs()) * 0.05).max(1.0);
        let lower = (min - padding).max(-f64::MAX);
        let upper = (max + padding).min(f64::MAX);
        (lower, upper)
    }
}

/// Topic-scoped monitoring; the owner notifies after successful mutations.
#[derive(Default)]
pub(super) struct Monitoring {
    topics: BTreeMap<String, BTreeMap<String, Series>>,
    next_color_ix: usize,
}

impl Monitoring {
    /// Adds a series seeded from a finite numeric leaf. Duplicates/invalid seeds return false.
    pub(super) fn start(&mut self, topic: &str, pointer: &str, payload: &[u8], time: DateTime<Local>) -> bool {
        if self.contains(topic, pointer) {
            return false;
        }
        let Ok(payload) = serde_json::from_slice::<Value>(payload) else {
            return false;
        };
        let Some(value) = payload.pointer(pointer).and_then(finite_number) else {
            return false;
        };
        let mut series = Series {
            color_ix: self.next_color_ix,
            ..Series::default()
        };
        series.push(time, value);
        self.topics.entry(topic.to_owned()).or_default().insert(pointer.to_owned(), series);
        self.next_color_ix = (self.next_color_ix + 1) % CHART_COLOR_COUNT;
        true
    }

    /// Call for every message in a batch, before any latest-per-topic coalescing.
    /// Parses once for a monitored topic and returns whether any series received a sample.
    pub(super) fn receive(&mut self, message: &Message) -> bool {
        let Some(series) = self.topics.get_mut(&message.topic) else {
            return false;
        };
        let Ok(payload) = serde_json::from_slice::<Value>(&message.payload) else {
            return false;
        };
        let mut changed = false;
        for (pointer, series) in series {
            if let Some(value) = payload.pointer(pointer).and_then(finite_number) {
                series.push(message.received_at, value);
                changed = true;
            }
        }
        changed
    }

    pub(super) fn contains(&self, topic: &str, pointer: &str) -> bool {
        self.topics.get(topic).is_some_and(|series| series.contains_key(pointer))
    }

    pub(super) fn is_empty(&self) -> bool {
        self.topics.is_empty()
    }

    /// Removes one series and its topic entry when no fields remain.
    pub(super) fn stop(&mut self, topic: &str, pointer: &str) -> bool {
        let Some(series) = self.topics.get_mut(topic) else {
            return false;
        };
        let removed = series.remove(pointer).is_some();
        if series.is_empty() {
            self.topics.remove(topic);
        }
        removed
    }

    pub(super) fn toggle_chart(&mut self, topic: &str, pointer: &str) -> bool {
        let Some(series) = self.topics.get_mut(topic).and_then(|fields| fields.get_mut(pointer)) else {
            return false;
        };
        series.kind = match series.kind {
            ChartKind::Line => ChartKind::Area,
            ChartKind::Area => ChartKind::Line,
        };
        true
    }

    pub(super) fn set_smoothing(&mut self, topic: &str, pointer: &str, smooth: bool) -> bool {
        let Some(series) = self.topics.get_mut(topic).and_then(|fields| fields.get_mut(pointer)) else {
            return false;
        };
        if series.smooth == smooth {
            return false;
        }
        series.smooth = smooth;
        true
    }

    pub(super) fn clear(&mut self) {
        self.topics.clear();
        self.next_color_ix = 0;
    }

    pub(super) fn render(&self, cx: &mut Context<Explorer>) -> AnyElement {
        let theme = cx.theme();
        let colors = [theme.primary, theme.green, theme.magenta, theme.cyan, theme.yellow];
        let mut content = div().flex().flex_wrap().items_start().gap_4().p_3().min_w_0();
        if self.is_empty() {
            content = content.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("Monitor a numeric JSON field to see its values over time."),
            );
        }
        for (topic, fields) in &self.topics {
            for (pointer, series) in fields {
                // Length-prefix the topic to distinguish every topic/pointer pair.
                let id = gpui_kit::ElementId::Name(SharedString::from(format!("monitor:{}:{topic}{pointer}", topic.len())));
                let label = format!("{topic} {}", if pointer.is_empty() { "(root)" } else { pointer });
                let latest = series.samples.back().map_or_else(String::new, |sample| sample.value.to_string());
                let topic = topic.clone();
                let pointer = pointer.clone();
                let (min, max) = series.y_domain();
                let color = colors[series.color_ix];
                let (switch_label, switch_icon) = match series.kind {
                    ChartKind::Line => ("Show area chart", gpui_kit::assets::IconName::ChartArea),
                    ChartKind::Area => ("Show line chart", gpui_kit::assets::IconName::ChartLine),
                };
                let toggle_topic = topic.clone();
                let toggle_pointer = pointer.clone();
                let smoothing_topic = topic.clone();
                let smoothing_pointer = pointer.clone();
                let chart = match series.kind {
                    ChartKind::Line => LineChart::new(series.samples.iter().cloned())
                        .id((id.clone(), "line-chart"))
                        .x(|sample: &Sample| sample.time.format("%H:%M:%S").to_string())
                        .y(|sample: &Sample| sample.value)
                        .name(label.clone())
                        .tooltip_title(|sample| sample.time.format("%Y-%m-%d %H:%M:%S%.3f %:z").to_string().into())
                        .when(series.smooth, |chart| chart.natural())
                        .when(!series.smooth, |chart| chart.linear())
                        .dot()
                        .y_domain(min, max)
                        .y_axis(true)
                        .y_tick_format(axis_label)
                        .x_tick_count(3)
                        .stroke(color)
                        .appear(false)
                        .into_any_element(),
                    ChartKind::Area => AreaChart::new(series.samples.iter().cloned())
                        .id((id.clone(), "area-chart"))
                        .x(|sample: &Sample| sample.time.format("%H:%M:%S").to_string())
                        .y(|sample: &Sample| sample.value)
                        .name(label.clone())
                        .tooltip_title(|sample| sample.time.format("%Y-%m-%d %H:%M:%S%.3f %:z").to_string().into())
                        .when(series.smooth, |chart| chart.natural())
                        .when(!series.smooth, |chart| chart.linear())
                        .y_domain(min, max)
                        .y_axis(true)
                        .y_tick_format(axis_label)
                        .x_tick_count(3)
                        .stroke(color)
                        .fill(color.opacity(0.2))
                        .appear(false)
                        .into_any_element(),
                };
                content = content.child(
                    div()
                        .id(id.clone())
                        .test_support()
                        .v_flex()
                        .flex_1()
                        .flex_basis(rems(24.))
                        .gap_2()
                        .min_w_0()
                        .child(
                            div()
                                .h_flex()
                                .gap_2()
                                .min_w_0()
                                .child(div().flex_1().min_w_0().text_sm().truncate().child(label.clone()))
                                .child(
                                    div()
                                        .id((id.clone(), "latest"))
                                        .test_support()
                                        .aria_label(latest.clone())
                                        .flex_none()
                                        .child(Tag::custom(color, color, color).small().outline().child(latest)),
                                )
                                .child(
                                    div().h_flex().flex_1().min_w_0().justify_end().child(
                                        div()
                                            .h_flex()
                                            .flex_none()
                                            .gap_1()
                                            .child(
                                                Button::new((id.clone(), "toggle-chart"))
                                                    .ghost()
                                                    .small()
                                                    .icon(switch_icon)
                                                    .accessibility_label(switch_label)
                                                    .tooltip(switch_label)
                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                        this.toggle_monitor_chart(&toggle_topic, &toggle_pointer, cx);
                                                    })),
                                            )
                                            .child(
                                                Toggle::new((id.clone(), "smooth"))
                                                    .small()
                                                    .icon(gpui_kit::assets::IconName::ChartSpline)
                                                    .checked(series.smooth)
                                                    .tooltip(if series.smooth {
                                                        "Disable line smoothing"
                                                    } else {
                                                        "Enable line smoothing"
                                                    })
                                                    .on_click(cx.listener(move |this, smooth, _, cx| {
                                                        this.set_monitor_smoothing(&smoothing_topic, &smoothing_pointer, *smooth, cx);
                                                    })),
                                            )
                                            .child(
                                                Button::new((id.clone(), "stop"))
                                                    .ghost()
                                                    .small()
                                                    .icon(IconName::Close)
                                                    .accessibility_label(format!("Stop monitoring {label}"))
                                                    .tooltip("Stop monitoring")
                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                        this.stop_monitoring(&topic, &pointer, cx);
                                                    })),
                                            ),
                                    ),
                                ),
                        )
                        .child(div().w_full().h(rems(12.0)).child(chart)),
                );
            }
        }
        div()
            .id("monitor-panel")
            .test_support()
            .v_flex()
            .size_full()
            .min_w_0()
            .min_h_0()
            .child(div().text_sm().px_3().py_2().child("Monitoring"))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .child(content)
                    .overflow_y_scrollbar()
                    .id("monitor-scroll"),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::topics::MessageProperties;
    use bytes::Bytes;

    fn message(topic: &str, payload: &str) -> Message {
        Message {
            topic: topic.to_owned(),
            payload: Bytes::copy_from_slice(payload.as_bytes()),
            qos: 0,
            retained: false,
            received_at: Local::now(),
            properties: MessageProperties::default(),
        }
    }

    #[test]
    fn new_charts_cycle_theme_colors_without_recoloring_existing_charts() {
        let mut monitoring = Monitoring::default();
        for ix in 0..7 {
            let topic = format!("topic/{ix}");
            assert!(!monitoring.start(&topic, "", b"invalid", Local::now()));
            assert!(monitoring.start(&topic, "", b"1", Local::now()));
            assert_eq!(monitoring.topics[&topic][""].color_ix, ix % CHART_COLOR_COUNT);
            assert!(!monitoring.start(&topic, "", b"2", Local::now()));
        }
        monitoring.stop("topic/0", "");
        monitoring.toggle_chart("topic/1", "");
        monitoring.receive(&message("topic/1", "3"));
        assert_eq!(monitoring.topics["topic/1"][""].color_ix, 1);
        assert!(monitoring.start("next", "", b"4", Local::now()));
        assert_eq!(monitoring.topics["next"][""].color_ix, 2);
        monitoring.clear();
        assert!(monitoring.start("fresh", "", b"5", Local::now()));
        assert_eq!(monitoring.topics["fresh"][""].color_ix, 0);
    }

    #[test]
    fn smoothing_is_per_chart_and_survives_chart_type_and_value_changes() {
        let mut monitoring = Monitoring::default();
        monitoring.start("a", "", b"1", Local::now());
        monitoring.start("b", "", b"2", Local::now());
        assert!(monitoring.topics["a"][""].smooth);
        assert!(monitoring.topics["b"][""].smooth);
        assert!(!monitoring.set_smoothing("missing", "", false));
        assert!(monitoring.set_smoothing("a", "", false));
        assert!(!monitoring.set_smoothing("a", "", false));
        monitoring.toggle_chart("a", "");
        monitoring.receive(&message("a", "3"));
        assert!(!monitoring.topics["a"][""].smooth);
        assert!(monitoring.topics["b"][""].smooth);
        assert!(monitoring.set_smoothing("a", "", true));
        assert_eq!(monitoring.topics["a"][""].samples.len(), 2);
    }

    #[test]
    fn chart_toggle_is_per_field_and_preserves_history_and_live_updates() {
        let mut monitoring = Monitoring::default();
        let seed = message("a", r#"{"n":1,"other":2}"#);
        monitoring.start("a", "/n", &seed.payload, seed.received_at);
        monitoring.start("a", "/other", &seed.payload, seed.received_at);
        assert!(!monitoring.toggle_chart("missing", "/n"));
        assert!(monitoring.toggle_chart("a", "/n"));
        assert_eq!(monitoring.topics["a"]["/n"].kind, ChartKind::Area);
        assert_eq!(monitoring.topics["a"]["/other"].kind, ChartKind::Line);
        monitoring.receive(&message("a", r#"{"n":3,"other":4}"#));
        assert!(monitoring.toggle_chart("a", "/n"));
        let series = &monitoring.topics["a"]["/n"];
        assert_eq!(series.kind, ChartKind::Line);
        assert_eq!(series.samples.iter().map(|sample| sample.value).collect::<Vec<_>>(), vec![1., 3.]);
    }

    #[test]
    fn axis_labels_reserve_dot_clearance_without_changing_number_formatting() {
        for (value, expected) in [(231.1, "231.1"), (0., "0"), (-42., "-42"), (12.345, "12.3")] {
            assert_eq!(axis_label(value), format!("{expected}\u{2002}"));
        }
    }

    #[test]
    fn numeric_fields_track_source_order_duplicate_keys_and_byte_positions() {
        let text = "{\n  \"雪\": true,\n  \"right\": {\n    \"value\": 2\n  },\n  \"left\": {\n    \"value\": -1.25e2\n  }\n}";
        let fields = numeric_fields(text);
        let expected: Vec<_> = [
            ("/right/value", "\"value\": 2", "2"),
            ("/left/value", "\"value\": -1.25e2", "-1.25e2"),
        ]
        .into_iter()
        .map(|(pointer, key, value)| {
            let key_offset = text.find(key).unwrap();
            let value_offset = key_offset + key.find(value).unwrap();
            NumericField {
                pointer: pointer.into(),
                line_offset: text[..key_offset].rfind('\n').unwrap() + 1,
                key_offset: Some(key_offset),
                value_offset,
            }
        })
        .collect();
        assert_eq!(fields, expected);
    }

    #[test]
    fn numeric_fields_escape_decoded_keys_and_walk_nested_arrays() {
        let text = "{\n  \"a/~\\\"\\u96ea\": [\n    [\n      -2,\n      {\"\": 3}\n    ],\n    \"4\"\n  ]\n}";
        let fields = numeric_fields(text);
        let actual: Vec<_> = fields
            .iter()
            .map(|field| {
                (
                    field.pointer(),
                    field.offset(),
                    field.key_offset(),
                    field.value_offset(),
                    field.line_offset(),
                )
            })
            .collect();
        let key = text.find("\"\": 3").unwrap();
        let number = text.find("-2").unwrap();
        assert_eq!(
            actual,
            vec![
                ("/a~1~0\"雪/0/0", number, None, number, text[..number].rfind('\n').unwrap() + 1),
                (
                    "/a~1~0\"雪/0/1/",
                    key,
                    Some(key),
                    text.find('3').unwrap(),
                    text[..key].rfind('\n').unwrap() + 1
                )
            ]
        );
    }

    #[test]
    fn root_numbers_use_empty_pointer_and_original_token_offset() {
        let fields = numeric_fields("\n  -1e-9 \n");
        assert_eq!(
            fields,
            vec![NumericField {
                pointer: String::new(),
                line_offset: 1,
                key_offset: None,
                value_offset: 3
            }]
        );
    }

    #[test]
    fn invalid_json_and_nonnumeric_leaves_produce_no_fields() {
        for text in ["", "not JSON", "{", "[1,]", "1 trailing", "1e999", r#"{"a":"1","b":null,"c":true}"#] {
            assert!(numeric_fields(text).is_empty(), "{text}");
        }
    }

    #[test]
    fn start_seeds_value_and_rejects_duplicates_without_replacing_history() {
        let mut monitoring = Monitoring::default();
        let time = Local::now();
        assert!(monitoring.start("a", "/n", br#"{"n":-2}"#, time));
        assert!(!monitoring.start("a", "/n", br#"{"n":99}"#, time));
        let samples = &monitoring.topics["a"]["/n"].samples;
        assert_eq!((samples.len(), samples[0].value, samples[0].time), (1, -2.0, time));
    }

    #[test]
    fn receive_updates_all_fields_only_on_exact_topic() {
        let mut monitoring = Monitoring::default();
        for (topic, pointer) in [("a", "/n"), ("a", "/array/0"), ("ab", "/n")] {
            assert!(monitoring.start(topic, pointer, br#"{"n":1,"array":[2]}"#, Local::now()));
        }
        assert!(!monitoring.receive(&message("other", "invalid")));
        let received = message("a", r#"{"n":3,"array":[4]}"#);
        assert!(monitoring.receive(&received));
        assert_eq!(
            (
                monitoring.topics["a"]["/n"].samples.back().unwrap().value,
                monitoring.topics["a"]["/array/0"].samples.back().unwrap().value,
                monitoring.topics["ab"]["/n"].samples.len(),
                monitoring.topics["a"]["/n"].samples.back().unwrap().time
            ),
            (3.0, 4.0, 1, received.received_at)
        );
    }

    #[test]
    fn invalid_missing_and_nonnumeric_samples_are_ignored() {
        let mut monitoring = Monitoring::default();
        assert!(monitoring.start("a", "/n", br#"{"n":1}"#, Local::now()));
        for payload in [
            "invalid",
            "{}",
            r#"{"n":"2"}"#,
            r#"{"n":null}"#,
            r#"{"n":true}"#,
            r#"{"n":[]}"#,
            r#"{"n":1e999}"#,
        ] {
            assert!(!monitoring.receive(&message("a", payload)), "{payload}");
            assert!(!monitoring.start("b", "/n", payload.as_bytes(), Local::now()), "{payload}");
        }
        assert_eq!(monitoring.topics["a"]["/n"].samples.len(), 1);
    }

    #[test]
    fn retention_keeps_latest_300_samples_including_every_batched_message() {
        let mut monitoring = Monitoring::default();
        assert!(monitoring.start("a", "", b"0", Local::now()));
        let batch: Vec<_> = (1..=350).map(|value| message("a", &value.to_string())).collect();
        for received in &batch {
            assert!(monitoring.receive(received));
        }
        let samples = &monitoring.topics["a"][""].samples;
        assert_eq!(
            (samples.len(), samples.front().unwrap().value, samples.back().unwrap().value),
            (300, 51.0, 350.0)
        );
    }

    #[test]
    fn stop_prunes_topics_and_clear_removes_every_series() {
        let mut monitoring = Monitoring::default();
        assert!(monitoring.start("a", "", b"1", Local::now()));
        assert!(monitoring.start("b", "", b"2", Local::now()));
        assert!(monitoring.stop("a", ""));
        assert!(!monitoring.contains("a", ""));
        assert!(!monitoring.stop("a", ""));
        monitoring.clear();
        assert!(monitoring.is_empty());
    }

    #[test]
    fn y_domain_encloses_negative_flat_and_extreme_values() {
        for values in [vec![-5.0], vec![0.0, 0.0], vec![-10.0, 5.0], vec![f64::MAX], vec![-f64::MAX]] {
            let mut series = Series::default();
            for value in values {
                series.push(Local::now(), value);
            }
            let (min, max) = series.y_domain();
            assert!(min.is_finite() && max.is_finite() && min < max);
            assert!(series.samples.iter().all(|sample| sample.value >= min && sample.value <= max));
        }
    }
}
