//! Numeric JSON fields and bounded live series owned by the explorer.

use std::collections::{BTreeMap, VecDeque};

use chrono::{DateTime, Local};
use gpui_kit::component::{
    ActiveTheme, IconName, Sizable, StyledExt,
    button::{Button, ButtonVariants, Toggle},
    chart::{AreaChart, LineChart},
    menu::{DropdownMenu, PopupMenuItem},
    plot::{
        AxisLabelSide, AxisText, Curve, IntoPlot, PathCaches, Plot, PlotAxis, TooltipState,
        scale::{Scale, ScaleLinear},
        shape::{Area, Line},
        tooltip::{CrossLine, Dot, Tooltip},
    },
    scroll::ScrollableElement,
};
use gpui_kit::{
    AnyElement, App, Bounds, Context, ElementId, Hsla, IntoElement, Pixels, Point, Render, TestSupportExt, TextAlign, Window, div, point,
    prelude::*, px, rems,
};
use serde::de::{DeserializeOwned, IgnoredAny};
use serde_json::Value;

use super::Explorer;
use crate::topics::Message;

const SAMPLE_LIMIT: usize = 300;
const CHART_COLOR_COUNT: usize = 5;

fn time_label(time: &DateTime<impl chrono::TimeZone>) -> String {
    time.with_timezone(&Local).format("%H:%M:%S").to_string()
}

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

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct FieldKey {
    topic: String,
    pointer: String,
}

impl FieldKey {
    pub(super) fn new(topic: &str, pointer: &str) -> Self {
        Self {
            topic: topic.to_owned(),
            pointer: pointer.to_owned(),
        }
    }

    pub(super) fn topic(&self) -> &str {
        &self.topic
    }
    fn id(&self) -> ElementId {
        ElementId::Name(format!("monitor:{}:{}{}", self.topic.len(), self.topic, self.pointer).into())
    }

    fn label(&self) -> String {
        if self.pointer.is_empty() {
            format!("{} (root)", self.topic)
        } else {
            format!("{}{}", self.topic, self.pointer)
        }
    }

    fn field_name(&self) -> String {
        if self.pointer.is_empty() {
            "(root)".to_owned()
        } else {
            self.pointer
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .replace("~1", "/")
                .replace("~0", "~")
        }
    }
}

impl Render for FieldKey {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .bg(cx.theme().popover)
            .text_color(cx.theme().popover_foreground)
            .border_1()
            .border_color(cx.theme().border)
            .text_sm()
            .child(format!("Add {} to chart", self.label()))
    }
}

struct PlotSeries {
    id: ElementId,
    label: String,
    samples: Vec<Sample>,
    color: Hsla,
}

#[derive(IntoPlot)]
struct GroupPlot {
    id: ElementId,
    left: f32,
    series: Vec<PlotSeries>,
    kind: ChartKind,
    smooth: bool,
}

impl GroupPlot {
    fn domains(&self) -> ((f64, f64), (f64, f64)) {
        let mut time = (f64::INFINITY, f64::NEG_INFINITY);
        let mut value = (f64::INFINITY, f64::NEG_INFINITY);
        for sample in self.series.iter().flat_map(|series| &series.samples) {
            let t = sample.time.timestamp_millis() as f64;
            time = (time.0.min(t), time.1.max(t));
            value = (value.0.min(sample.value), value.1.max(sample.value));
        }
        if time.0 == time.1 {
            time.0 -= 500.;
            time.1 += 500.;
        }
        let padding = (value.0.abs().max(value.1.abs()) * 0.05).max(1.);
        (
            (time.0, time.1),
            ((value.0 - padding).max(-f64::MAX), (value.1 + padding).min(f64::MAX)),
        )
    }

    fn geometry(&self, bounds: Bounds<Pixels>, cx: &App) -> (f32, f32, ScaleLinear<f64>, ScaleLinear<f64>) {
        let left = self.left;
        let bottom = rems(1.5).to_pixels(cx.theme().font_size).as_f32();
        let height = (bounds.size.height.as_f32() - bottom).max(1.);
        let width = bounds.size.width.as_f32().max(left + 1.);
        let (time, value) = self.domains();
        // Normalize before subtracting to keep even opposite f64 extremes finite.
        let magnitude = value.0.abs().max(value.1.abs()).max(1.);
        (
            left,
            height,
            ScaleLinear::new([time.0, time.1], [left, width]),
            ScaleLinear::new([value.0 / magnitude, value.1 / magnitude], [height, 0.]),
        )
    }

    fn magnitude(&self) -> f64 {
        let (_, value) = self.domains();
        value.0.abs().max(value.1.abs()).max(1.)
    }

    fn nearest_samples(&self, time: DateTime<Local>) -> impl Iterator<Item = (&PlotSeries, &Sample)> {
        self.series.iter().filter_map(move |series| {
            let sample = series
                .samples
                .iter()
                .min_by_key(|sample| (sample.time.timestamp_millis() - time.timestamp_millis()).unsigned_abs())?;
            Some((series, sample))
        })
    }

    fn nearest(&self, position: Point<Pixels>, bounds: Bounds<Pixels>, cx: &App) -> Option<(&Sample, usize)> {
        let (left, height, x, _) = self.geometry(bounds, cx);
        if position.x.as_f32() < left || position.y.as_f32() > height {
            return None;
        }
        self.series
            .iter()
            .flat_map(|series| &series.samples)
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                let distance =
                    |sample: &Sample| (x.tick(&(sample.time.timestamp_millis() as f64)).unwrap_or(left) - position.x.as_f32()).abs();
                distance(a).total_cmp(&distance(b))
            })
            .map(|(ix, sample)| (sample, ix))
    }
}

fn grouped_axis_label(value: f64) -> String {
    if value.abs() >= 1_000_000. || (value != 0. && value.abs() < 0.001) {
        format!("{value:.2e}\u{2002}")
    } else {
        axis_label(value)
    }
}

impl Plot for GroupPlot {
    fn prepaint(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) -> Vec<AnyElement> {
        let (_, (min, max)) = self.domains();
        let font = rems(0.75).to_pixels(cx.theme().font_size);
        let gap = rems(0.5).to_pixels(cx.theme().font_size).as_f32();
        self.left = (0..3)
            .map(|ix| {
                let ratio = f64::from(ix) / 2.;
                let label = grouped_axis_label(min * (1. - ratio) + max * ratio).into();
                gpui_kit::component::plot::label::measure_text_width(&label, font, window)
            })
            .fold(0f32, f32::max)
            + gap;
        self.left = self.left.min(bounds.size.width.as_f32() / 2.);
        vec![]
    }

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn paint(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        let (left, height, x, y) = self.geometry(bounds, cx);
        let magnitude = self.magnitude();
        let (time, value) = self.domains();
        let font = rems(0.75).to_pixels(cx.theme().font_size);
        let axis_bounds = Bounds::new(
            bounds.origin + point(px(left), px(0.)),
            gpui_kit::size(bounds.size.width - px(left), bounds.size.height),
        );
        let labels = (0..3).map(|ix| {
            let ratio = f64::from(ix) / 2.;
            let t = time.0 * (1. - ratio) + time.1 * ratio;
            let label = DateTime::from_timestamp_millis(t as i64).map_or_else(String::new, |time| time_label(&time));
            AxisText::new(label, px(x.tick(&t).unwrap_or(left) - left), cx.theme().muted_foreground)
                .font_size(font)
                .align(match ix {
                    0 => TextAlign::Left,
                    2 => TextAlign::Right,
                    _ => TextAlign::Center,
                })
        });
        PlotAxis::new()
            .x(height)
            .x_label(labels)
            .y(0.)
            .y_axis(true)
            .y_label_side(AxisLabelSide::Start)
            .y_label((0..3).map(|ix| {
                let ratio = f64::from(ix) / 2.;
                let v = value.0 * (1. - ratio) + value.1 * ratio;
                AxisText::new(
                    grouped_axis_label(v),
                    px(y.tick(&(v / magnitude)).unwrap_or(height)),
                    cx.theme().muted_foreground,
                )
                .font_size(font)
                .align(TextAlign::Right)
            }))
            .stroke(cx.theme().border)
            .paint(&axis_bounds, window, cx);
        let curve = if self.smooth { Curve::Natural } else { Curve::Linear };
        window.with_content_mask(
            Some(gpui_kit::ContentMask {
                bounds: Bounds::new(axis_bounds.origin, gpui_kit::size(axis_bounds.size.width, px(height))),
            }),
            |window| {
                for series in &self.series {
                    let x = x.clone();
                    let y = y.clone();
                    let points: Vec<_> = series
                        .samples
                        .iter()
                        .filter_map(|sample| {
                            Some((
                                x.tick(&(sample.time.timestamp_millis() as f64))?,
                                y.tick(&(sample.value / magnitude))?,
                            ))
                        })
                        .collect();
                    let caches = PathCaches::for_paint(series.id.clone(), window, cx);
                    caches.update(cx, |caches, _| {
                        if self.kind == ChartKind::Area {
                            let baseline = y.tick(&(0f64.clamp(value.0, value.1) / magnitude)).unwrap_or(height);
                            // Each field has its own native shape; no cross-field segments or stacked values.
                            let area = Area::new()
                                .data(points.iter().copied())
                                .x(|p| Some(p.0))
                                .y1(|p| Some(p.1))
                                .y0(baseline)
                                .curve(curve)
                                .stroke(series.color)
                                .fill(series.color.opacity(0.2));
                            let (fill, line) = caches.slot_pair(0);
                            area.paint_cached(&bounds, fill, line, window);
                        } else {
                            Line::new()
                                .data(points)
                                .x(|p| Some(p.0))
                                .y(|p| Some(p.1))
                                .curve(curve)
                                .stroke(series.color)
                                .stroke_width(2.)
                                .dot()
                                .dot_fill(series.color)
                                .paint_cached(&bounds, caches.slot(0), window);
                        }
                    });
                }
            },
        );
    }

    fn tooltip_state(&self, position: Point<Pixels>, bounds: Bounds<Pixels>, cx: &App) -> Option<TooltipState> {
        let (sample, ix) = self.nearest(position, bounds, cx)?;
        let (_, _, x, y) = self.geometry(bounds, cx);
        let magnitude = self.magnitude();
        let dots = self
            .nearest_samples(sample.time)
            .map(|(_, nearest)| {
                Some(point(
                    px(x.tick(&(nearest.time.timestamp_millis() as f64))?),
                    px(y.tick(&(nearest.value / magnitude))?),
                ))
            })
            .collect::<Option<Vec<_>>>()?;
        Some(TooltipState::new(
            ix,
            point(px(x.tick(&(sample.time.timestamp_millis() as f64))?), position.y),
            dots,
        ))
    }

    fn tooltip(
        &self,
        state: &TooltipState,
        cursor: Point<Pixels>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        let sample = self.series.iter().flat_map(|series| &series.samples).nth(state.index)?;
        let (_, height, _, _) = self.geometry(bounds, cx);
        let mut tooltip = Tooltip::new(cursor, bounds.size)
            .glide(false)
            .cross_line(CrossLine::new(state.cross_line).height(height))
            .dots(self.nearest_samples(sample.time).zip(&state.dots).map(|((series, _), &dot)| {
                // Match Kit's built-in line and area chart hover markers.
                Dot::new(dot)
                    .size(px(8.))
                    .halo(px(20.))
                    .stroke(cx.theme().background)
                    .fill(series.color)
            }))
            .title(time_label(&sample.time));
        for (series, nearest) in self.nearest_samples(sample.time) {
            tooltip = tooltip.row(series.color, series.label.clone(), nearest.value.to_string());
        }
        Some(tooltip.into_any_element())
    }
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

#[derive(Default)]
struct Series {
    samples: VecDeque<Sample>,
    color_ix: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct ChartId(u64);

impl ChartId {
    pub(super) fn element_id(self) -> ElementId {
        ElementId::Name(format!("monitor-chart:{}", self.0).into())
    }
}

struct Chart {
    members: Vec<FieldKey>,
    kind: ChartKind,
    smooth: bool,
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
    // Monotonic chart IDs keep rendering in creation order, independent of field names.
    charts: BTreeMap<ChartId, Chart>,
    next_chart_id: u64,
    next_color_ix: usize,
}

impl Monitoring {
    /// Always opens a new standalone chart, reusing any retained field history.
    pub(super) fn start(&mut self, topic: &str, pointer: &str, payload: &[u8], time: DateTime<Local>) -> bool {
        if self.contains(topic, pointer) {
            return self.open_chart(FieldKey::new(topic, pointer));
        }
        if self.next_chart_id == u64::MAX {
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
        self.open_chart(FieldKey::new(topic, pointer));
        self.next_color_ix = (self.next_color_ix + 1) % CHART_COLOR_COUNT;
        true
    }

    fn open_chart(&mut self, field: FieldKey) -> bool {
        let Some(next) = self.next_chart_id.checked_add(1) else {
            return false;
        };
        let id = ChartId(self.next_chart_id);
        self.next_chart_id = next;
        self.charts.insert(
            id,
            Chart {
                members: vec![field],
                kind: ChartKind::Line,
                smooth: true,
            },
        );
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

    fn series(&self, key: &FieldKey) -> Option<&Series> {
        self.topics.get(&key.topic)?.get(&key.pointer)
    }

    #[cfg(test)]
    pub(super) fn chart_ids(&self) -> impl Iterator<Item = ChartId> + '_ {
        self.charts.keys().copied()
    }

    pub(super) fn members(&self, id: ChartId) -> Option<&[FieldKey]> {
        self.charts.get(&id).map(|chart| chart.members.as_slice())
    }

    pub(super) fn accepts(&self, id: ChartId, field: &FieldKey) -> bool {
        self.members(id).is_some_and(|members| !members.contains(field))
    }

    /// Moves at most one standalone copy; grouped and other duplicate charts stay intact.
    pub(super) fn add(&mut self, id: ChartId, field: &FieldKey, payload: &[u8], time: DateTime<Local>) -> bool {
        if !self.accepts(id, field) {
            return false;
        }
        let Ok(value) = serde_json::from_slice::<Value>(payload) else {
            return false;
        };
        let Some(value) = value.pointer(&field.pointer).and_then(finite_number) else {
            return false;
        };
        if !self.contains(&field.topic, &field.pointer) {
            let mut series = Series {
                color_ix: self.next_color_ix,
                ..Series::default()
            };
            series.push(time, value);
            self.topics
                .entry(field.topic.clone())
                .or_default()
                .insert(field.pointer.clone(), series);
            self.next_color_ix = (self.next_color_ix + 1) % CHART_COLOR_COUNT;
        }
        let source = self.charts.iter().find_map(|(source, chart)| {
            (*source != id && chart.members.len() == 1 && chart.members.first() == Some(field)).then_some(*source)
        });
        if let Some(source) = source {
            self.charts.remove(&source);
        }
        if let Some(chart) = self.charts.get_mut(&id) {
            chart.members.push(field.clone());
        }
        true
    }

    /// Releases datasets only when no remaining chart references them.
    pub(super) fn stop(&mut self, id: ChartId) -> bool {
        let Some(chart) = self.charts.remove(&id) else { return false };
        for member in chart.members {
            if self.charts.values().any(|chart| chart.members.contains(&member)) {
                continue;
            }
            if let Some(fields) = self.topics.get_mut(&member.topic) {
                fields.remove(&member.pointer);
                if fields.is_empty() {
                    self.topics.remove(&member.topic);
                }
            }
        }
        true
    }

    pub(super) fn toggle_chart(&mut self, id: ChartId) -> bool {
        let Some(chart) = self.charts.get_mut(&id) else { return false };
        chart.kind = match chart.kind {
            ChartKind::Line => ChartKind::Area,
            ChartKind::Area => ChartKind::Line,
        };
        true
    }

    pub(super) fn set_smoothing(&mut self, id: ChartId, smooth: bool) -> bool {
        let Some(chart) = self.charts.get_mut(&id) else { return false };
        let changed = chart.smooth != smooth;
        chart.smooth = smooth;
        changed
    }

    pub(super) fn clear(&mut self) {
        self.topics.clear();
        self.charts.clear();
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
        for (&chart_id, options) in &self.charts {
            let members = &options.members;
            let Some(first) = members.first() else { continue };
            let Some(series) = self.series(first) else { continue };
            let id = chart_id.element_id();
            let label = first.label();

            let (min, max) = series.y_domain();
            let color = colors[series.color_ix % colors.len()];
            let (switch_label, switch_icon) = match options.kind {
                ChartKind::Line => ("Show area chart", gpui_kit::assets::IconName::ChartArea),
                ChartKind::Area => ("Show line chart", gpui_kit::assets::IconName::ChartLine),
            };
            let chart = if members.len() > 1 {
                GroupPlot {
                    id: (id.clone(), "group-plot").into(),
                    left: 0.,
                    series: members
                        .iter()
                        .filter_map(|member| {
                            let series = self.series(member)?;
                            Some(PlotSeries {
                                id: member.id(),
                                label: member.label(),
                                samples: series.samples.iter().cloned().collect(),
                                color: colors[series.color_ix % colors.len()],
                            })
                        })
                        .collect(),
                    kind: options.kind,
                    smooth: options.smooth,
                }
                .into_any_element()
            } else {
                match options.kind {
                    ChartKind::Line => LineChart::new(series.samples.iter().cloned())
                        .id((id.clone(), "line-chart"))
                        .x(|sample: &Sample| time_label(&sample.time))
                        .y(|sample: &Sample| sample.value)
                        .name(label.clone())
                        .tooltip_title(|sample| time_label(&sample.time).into())
                        .when(options.smooth, |chart| chart.natural())
                        .when(!options.smooth, |chart| chart.linear())
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
                        .x(|sample: &Sample| time_label(&sample.time))
                        .y(|sample: &Sample| sample.value)
                        .name(label.clone())
                        .tooltip_title(|sample| time_label(&sample.time).into())
                        .when(options.smooth, |chart| chart.natural())
                        .when(!options.smooth, |chart| chart.linear())
                        .y_domain(min, max)
                        .y_axis(true)
                        .y_tick_format(axis_label)
                        .x_tick_count(3)
                        .stroke(color)
                        .fill(color.opacity(0.2))
                        .appear(false)
                        .into_any_element(),
                }
            };

            let owner = cx.weak_entity();
            let feedback_owner = owner.clone();
            content = content.child(
                div()
                    .id(id.clone())
                    .test_support()
                    .border_1()
                    .border_color(cx.theme().border.opacity(0.))
                    .p_1()
                    .drag_over::<FieldKey>(move |style, field, _, cx| {
                        if feedback_owner
                            .upgrade()
                            .is_some_and(|owner| owner.read(cx).monitoring.accepts(chart_id, field))
                        {
                            style.bg(cx.theme().accent).border_color(cx.theme().primary)
                        } else {
                            style
                        }
                    })
                    .on_drop(cx.listener(move |this, field: &FieldKey, _, cx| {
                        this.add_monitor_field(chart_id, field, cx);
                    }))
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
                                div().h_flex().flex_none().justify_end().child(
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
                                                    this.toggle_monitor_chart(chart_id, cx);
                                                })),
                                        )
                                        .child(
                                            Toggle::new((id.clone(), "smooth"))
                                                .small()
                                                .icon(gpui_kit::assets::IconName::ChartSpline)
                                                .checked(options.smooth)
                                                .tooltip(if options.smooth {
                                                    "Disable line smoothing"
                                                } else {
                                                    "Enable line smoothing"
                                                })
                                                .on_click(cx.listener(move |this, smooth, _, cx| {
                                                    this.set_monitor_smoothing(chart_id, *smooth, cx);
                                                })),
                                        )
                                        .child(
                                            Button::new((id.clone(), "add-field"))
                                                .ghost()
                                                .small()
                                                .icon(gpui_kit::assets::IconName::Plus)
                                                .accessibility_label("Add field to chart…")
                                                .tooltip("Add field to chart…")
                                                .dropdown_menu(move |mut menu, _, cx| {
                                                    let Some(view) = owner.upgrade() else { return menu };
                                                    let explorer = view.read(cx);
                                                    let Some(source_topic) = explorer.selected.as_deref() else {
                                                        return menu;
                                                    };
                                                    for field in &explorer.numeric_fields {
                                                        let field = FieldKey::new(source_topic, field.pointer());
                                                        if !explorer.monitoring.accepts(chart_id, &field) {
                                                            continue;
                                                        }
                                                        let owner = owner.clone();
                                                        menu = menu.item(PopupMenuItem::new(field.label()).on_click(move |_, _, cx| {
                                                            let _ =
                                                                owner.update(cx, |this, cx| this.add_monitor_field(chart_id, &field, cx));
                                                        }));
                                                    }
                                                    menu
                                                }),
                                        )
                                        .child(
                                            Button::new((id.clone(), "stop"))
                                                .ghost()
                                                .small()
                                                .icon(IconName::Close)
                                                .accessibility_label(format!("Close chart {label}"))
                                                .tooltip("Close chart")
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.stop_monitoring(chart_id, cx);
                                                })),
                                        ),
                                ),
                            ),
                    )
                    .when(members.len() > 1, |container| {
                        container.child(
                            div()
                                .h_flex()
                                .flex_wrap()
                                .justify_end()
                                .gap_3()
                                .children(members.iter().map(|member| {
                                    let text = member.field_name();
                                    let color = self
                                        .series(member)
                                        .map_or(cx.theme().muted_foreground, |series| colors[series.color_ix % colors.len()]);
                                    div()
                                        .id((member.id(), "legend"))
                                        .test_support()
                                        .aria_label(text.clone())
                                        .h_flex()
                                        .gap_1p5()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(div().size_2().flex_none().rounded(cx.theme().radius_full()).bg(color))
                                        .child(text)
                                })),
                        )
                    })
                    .child(div().id((id.clone(), "plot")).test_support().w_full().h(rems(12.0)).child(chart)),
            );
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
    use chrono::{TimeZone, Timelike};

    #[test]
    fn time_labels_omit_date_fraction_and_offset() {
        let time = Local
            .with_ymd_and_hms(2026, 1, 15, 9, 4, 5)
            .unwrap()
            .with_nanosecond(123_456_789)
            .unwrap();

        assert_eq!(time_label(&time), "09:04:05");
    }

    #[test]
    fn time_labels_convert_to_system_timezone_in_winter_and_summer() {
        for month in [1, 7] {
            for hour in [0, 23] {
                let local = Local.with_ymd_and_hms(2026, month, 15, hour, 4, 5).unwrap();
                let expected = format!("{hour:02}:04:05");
                assert_eq!(time_label(&local.with_timezone(&chrono::Utc)), expected);
                for offset in [-12 * 3600, 14 * 3600] {
                    let time = local.with_timezone(&chrono::FixedOffset::east_opt(offset).unwrap());
                    assert_eq!(time_label(&time), expected);
                }
            }
        }
    }

    #[test]
    fn labels_join_topic_and_pointer_without_extra_space() {
        assert_eq!(FieldKey::new("energy/solar", "/OutputPower").label(), "energy/solar/OutputPower");
        assert_eq!(FieldKey::new("energy/solar", "").label(), "energy/solar (root)");
    }

    #[test]
    fn legend_names_show_only_decoded_json_field_names() {
        for (pointer, expected) in [
            ("/solar/power", "power"),
            ("/a~1b~0c", "a/b~c"),
            ("/items/0", "0"),
            ("", "(root)"),
            ("/", ""),
        ] {
            assert_eq!(FieldKey::new("energy/solar", pointer).field_name(), expected);
        }
    }

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

    fn chart_id(monitoring: &Monitoring, topic: &str, pointer: &str) -> ChartId {
        let field = FieldKey::new(topic, pointer);
        monitoring
            .charts
            .iter()
            .find_map(|(id, chart)| chart.members.contains(&field).then_some(*id))
            .unwrap_or(ChartId(u64::MAX))
    }

    #[test]
    fn grouped_fields_move_one_standalone_history_and_update_once() {
        let mut monitoring = Monitoring::default();
        let seed = message("a", r#"{"a":1,"b":2,"c":3}"#);
        for pointer in ["/a", "/b", "/c"] {
            monitoring.start("a", pointer, &seed.payload, seed.received_at);
        }
        monitoring.receive(&message("a", r#"{"a":4,"b":5,"c":6}"#));
        let b = FieldKey::new("a", "/b");
        let color = monitoring.series(&b).unwrap().color_ix;
        monitoring.toggle_chart(ChartId(0));
        monitoring.set_smoothing(ChartId(0), false);
        assert!(monitoring.add(ChartId(0), &b, &seed.payload, seed.received_at));
        assert_eq!(monitoring.charts.len(), 2);
        assert_eq!(
            monitoring
                .series(&b)
                .unwrap()
                .samples
                .iter()
                .map(|sample| sample.value)
                .collect::<Vec<_>>(),
            [2., 5.]
        );
        assert_eq!(monitoring.series(&b).unwrap().color_ix, color);
        assert_eq!(monitoring.charts[&ChartId(0)].kind, ChartKind::Area);
        assert!(!monitoring.charts[&ChartId(0)].smooth);
        assert!(!monitoring.add(ChartId(0), &b, &seed.payload, seed.received_at));
        assert!(!monitoring.add(ChartId(99), &b, &seed.payload, seed.received_at));
        assert!(!monitoring.add(ChartId(0), &FieldKey::new("a", "/missing"), &seed.payload, seed.received_at));
        assert!(!monitoring.add(ChartId(0), &FieldKey::new("a", "/c"), b"invalid", seed.received_at));
        for _ in 0..305 {
            monitoring.receive(&message("a", r#"{"a":7,"b":8}"#));
        }
        assert_eq!(monitoring.series(&b).unwrap().samples.len(), SAMPLE_LIMIT);
        assert!(monitoring.stop(ChartId(0)));
        assert!(!monitoring.contains("a", "/b"));
        assert!(monitoring.contains("a", "/c"));
    }

    #[test]
    fn grouped_fields_from_different_topics_update_independently_and_reject_stale_seeds() {
        let mut monitoring = Monitoring::default();
        let time = Local::now();
        monitoring.start("a", "", b"1", time);
        let field = FieldKey::new("b", "/a~1b~0c/0");
        assert!(!monitoring.add(ChartId(0), &field, br#"{"a/b~c":["not a number"]}"#, time));
        assert!(monitoring.add(ChartId(0), &field, br#"{"a/b~c":[2]}"#, time));
        assert!(!monitoring.add(ChartId(0), &FieldKey::new("b", "/missing"), b"{}", time));
        assert!(!monitoring.receive(&message("b/child", r#"{"a/b~c":[99]}"#)));
        assert!(monitoring.receive(&message("b", r#"{"a/b~c":[3]}"#)));
        assert_eq!(monitoring.topics["a"][""].samples.len(), 1);
        assert_eq!(monitoring.series(&field).unwrap().samples.len(), 2);
        assert!(monitoring.stop(ChartId(0)));
        assert!(monitoring.is_empty());
    }

    #[test]
    fn clicking_grouped_or_standalone_fields_always_duplicates_without_changing_existing_charts() {
        for grouped in [false, true] {
            for pointer in ["/a", "/b"] {
                let mut monitoring = Monitoring::default();
                let seed = message("topic", r#"{"a":1,"b":2}"#);
                monitoring.start("topic", pointer, &seed.payload, seed.received_at);
                let field = FieldKey::new("topic", pointer);
                if grouped {
                    let other = if pointer == "/a" { "/b" } else { "/a" };
                    monitoring.add(ChartId(0), &FieldKey::new("topic", other), &seed.payload, seed.received_at);
                }
                monitoring.toggle_chart(ChartId(0));
                monitoring.set_smoothing(ChartId(0), false);
                let original = monitoring.members(ChartId(0)).unwrap().to_vec();
                monitoring.receive(&message("topic", r#"{"a":3,"b":4}"#));
                let color = monitoring.series(&field).unwrap().color_ix;
                let next_color = monitoring.next_color_ix;
                for id in [ChartId(1), ChartId(2)] {
                    // An existing dataset can be reused even if the latest payload is invalid.
                    assert!(monitoring.start("topic", pointer, b"invalid", seed.received_at));
                    assert_eq!(monitoring.members(id).unwrap(), std::slice::from_ref(&field));
                    assert_eq!(monitoring.charts[&id].kind, ChartKind::Line);
                    assert!(monitoring.charts[&id].smooth);
                }
                assert_eq!(monitoring.charts.len(), 3);
                assert_eq!(monitoring.members(ChartId(0)).unwrap(), original);
                assert_eq!(monitoring.charts[&ChartId(0)].kind, ChartKind::Area);
                assert!(!monitoring.charts[&ChartId(0)].smooth);
                assert_eq!(monitoring.series(&field).unwrap().samples.len(), 2);
                assert_eq!(monitoring.series(&field).unwrap().color_ix, color);
                assert_eq!(monitoring.next_color_ix, next_color);
                monitoring.receive(&message("topic", r#"{"a":5,"b":6}"#));
                assert_eq!(monitoring.series(&field).unwrap().samples.len(), 3);
            }
        }
    }

    #[test]
    fn duplicate_chart_settings_are_independent_and_closing_retains_referenced_datasets() {
        for grouped in [false, true] {
            for close_original_first in [false, true] {
                let mut monitoring = Monitoring::default();
                let seed = message("a", r#"{"a":1,"b":2}"#);
                monitoring.start("a", "/a", &seed.payload, seed.received_at);
                if grouped {
                    monitoring.add(ChartId(0), &FieldKey::new("a", "/b"), &seed.payload, seed.received_at);
                }
                monitoring.start("a", "/a", &seed.payload, seed.received_at);
                monitoring.start("a", "/a", &seed.payload, seed.received_at);
                monitoring.toggle_chart(ChartId(1));
                monitoring.set_smoothing(ChartId(1), false);
                assert_eq!(monitoring.charts[&ChartId(0)].kind, ChartKind::Line);
                assert!(monitoring.charts[&ChartId(0)].smooth);
                assert_eq!(monitoring.charts[&ChartId(1)].kind, ChartKind::Area);
                assert!(!monitoring.charts[&ChartId(1)].smooth);
                assert_eq!(monitoring.charts[&ChartId(2)].kind, ChartKind::Line);
                assert!(monitoring.charts[&ChartId(2)].smooth);
                let order = if close_original_first { [0, 1, 2] } else { [2, 1, 0] };
                for (ix, id) in order.into_iter().enumerate() {
                    assert!(monitoring.stop(ChartId(id)));
                    assert!(!monitoring.stop(ChartId(id)));
                    assert!(!monitoring.toggle_chart(ChartId(id)));
                    assert!(!monitoring.set_smoothing(ChartId(id), false));
                    assert_eq!(monitoring.contains("a", "/a"), ix < 2);
                    if ix < 2 {
                        assert!(monitoring.receive(&message("a", r#"{"a":3,"b":4}"#)));
                        assert_eq!(monitoring.topics["a"]["/a"].samples.len(), ix + 2);
                    }
                }
                assert!(monitoring.is_empty());
                assert!(monitoring.charts.is_empty());
            }
        }
    }

    #[test]
    fn add_moves_only_one_standalone_copy_and_never_changes_other_groups() {
        let mut monitoring = Monitoring::default();
        let seed = message("a", r#"{"a":1,"b":2,"c":3}"#);
        monitoring.start("a", "/a", &seed.payload, seed.received_at);
        let b = FieldKey::new("a", "/b");
        monitoring.add(ChartId(0), &b, &seed.payload, seed.received_at);
        monitoring.start("a", "/c", &seed.payload, seed.received_at);
        assert!(monitoring.add(ChartId(1), &b, &seed.payload, seed.received_at));
        assert_eq!(monitoring.members(ChartId(0)).unwrap().len(), 2);
        monitoring.start("a", "/b", &seed.payload, seed.received_at);
        monitoring.start("a", "/b", &seed.payload, seed.received_at);
        monitoring.start("a", "/a", &seed.payload, seed.received_at);
        assert!(monitoring.add(ChartId(4), &b, &seed.payload, seed.received_at));
        assert!(monitoring.members(ChartId(2)).is_none());
        assert_eq!(monitoring.members(ChartId(3)).unwrap(), [b]);
        assert_eq!(monitoring.members(ChartId(0)).unwrap().len(), 2);
        assert_eq!(monitoring.members(ChartId(1)).unwrap().len(), 2);
        assert_eq!(monitoring.topics["a"]["/b"].samples.len(), 1);
    }

    #[test]
    fn new_charts_append_in_creation_order_including_duplicates_and_after_removal() {
        let mut monitoring = Monitoring::default();
        let seed = message("topic", r#"{"z":1,"a":2,"m":3}"#);
        for pointer in ["/z", "/a", "/m", "/a"] {
            assert!(monitoring.start("topic", pointer, &seed.payload, seed.received_at));
        }
        let order = |monitoring: &Monitoring| {
            monitoring
                .chart_ids()
                .map(|id| monitoring.members(id).unwrap()[0].pointer.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(order(&monitoring), ["/z", "/a", "/m", "/a"]);
        monitoring.stop(ChartId(1));
        monitoring.start("topic", "/a", &seed.payload, seed.received_at);
        assert_eq!(order(&monitoring), ["/z", "/m", "/a", "/a"]);
        assert_eq!(
            monitoring.chart_ids().collect::<Vec<_>>(),
            [ChartId(0), ChartId(2), ChartId(3), ChartId(4)]
        );
    }

    #[test]
    fn chart_ids_are_not_reused_after_close_or_clear() {
        let mut monitoring = Monitoring::default();
        monitoring.start("a", "", b"1", Local::now());
        monitoring.stop(ChartId(0));
        monitoring.start("a", "", b"1", Local::now());
        assert!(monitoring.members(ChartId(0)).is_none());
        monitoring.clear();
        monitoring.start("a", "", b"1", Local::now());
        assert!(monitoring.members(ChartId(1)).is_none());
        assert!(monitoring.members(ChartId(2)).is_some());
    }

    #[test]
    fn grouped_domains_use_real_time_and_all_values_including_extremes() {
        let time = Local::now();
        let plot = GroupPlot {
            id: "test".into(),
            left: 0.,
            kind: ChartKind::Line,
            smooth: true,
            series: vec![
                PlotSeries {
                    id: "a".into(),
                    label: "a".into(),
                    color: Hsla::default(),
                    samples: vec![Sample { time, value: -f64::MAX }],
                },
                PlotSeries {
                    id: "b".into(),
                    label: "b".into(),
                    color: Hsla::default(),
                    samples: vec![Sample {
                        time: time + chrono::Duration::seconds(10),
                        value: f64::MAX,
                    }],
                },
            ],
        };
        let ((start, end), (min, max)) = plot.domains();
        assert_eq!(end - start, 10_000.);
        assert_eq!((min, max), (-f64::MAX, f64::MAX));
        let magnitude = plot.magnitude();
        let scale = ScaleLinear::new([min / magnitude, max / magnitude], [100., 0.]);
        assert_eq!(scale.tick(&0.), Some(50.));
    }

    #[gpui_kit::test]
    fn grouped_hover_marks_each_fields_nearest_sample(cx: &mut gpui_kit::TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            let time = DateTime::from_timestamp(0, 0).unwrap().with_timezone(&Local);
            let bounds = Bounds::new(point(px(100.), px(50.)), gpui_kit::size(px(260.), px(144.)));
            for offset in [0, 2] {
                for kind in [ChartKind::Line, ChartKind::Area] {
                    for smooth in [false, true] {
                        let series = |id: &'static str, offset, values: [f64; 3], color| PlotSeries {
                            id: id.into(),
                            label: id.to_owned(),
                            color,
                            samples: values
                                .into_iter()
                                .enumerate()
                                .map(|(ix, value)| Sample {
                                    time: time + chrono::Duration::seconds(ix as i64 * 10 + offset),
                                    value,
                                })
                                .collect(),
                        };
                        let plot = GroupPlot {
                            id: "hover-test".into(),
                            left: 40.,
                            kind,
                            smooth,
                            series: vec![
                                series("a", 0, [0., 10., 20.], cx.theme().primary),
                                PlotSeries {
                                    id: "empty".into(),
                                    label: "empty".to_owned(),
                                    samples: vec![],
                                    color: cx.theme().magenta,
                                },
                                series("b", offset, [2., 12., 18.], cx.theme().green),
                            ],
                        };
                        let (_, _, x, y) = plot.geometry(bounds, cx);
                        let position = |sample: &Sample| {
                            point(
                                px(x.tick(&(sample.time.timestamp_millis() as f64)).unwrap()),
                                px(y.tick(&(sample.value / plot.magnitude())).unwrap()),
                            )
                        };
                        for ix in [1, 2] {
                            let a = position(&plot.series[0].samples[ix]);
                            let b = position(&plot.series[2].samples[ix]);
                            // Hover by time, well away from the dots vertically.
                            let cursor = point(a.x - px(1.), px(5.));
                            let state = plot.tooltip_state(cursor, bounds, cx).unwrap();
                            assert_eq!(state.index, ix);
                            assert_eq!(state.cross_line, point(a.x, cursor.y));
                            assert_eq!(state.dots, vec![a, b]);
                        }
                        assert!(plot.tooltip_state(point(px(10.), px(5.)), bounds, cx).is_none());
                        assert!(plot.tooltip_state(point(px(100.), bounds.size.height), bounds, cx).is_none());
                    }
                }
            }
        });
    }

    #[test]
    fn new_charts_cycle_theme_colors_without_recoloring_existing_charts() {
        let mut monitoring = Monitoring::default();
        for ix in 0..7 {
            let topic = format!("topic/{ix}");
            assert!(!monitoring.start(&topic, "", b"invalid", Local::now()));
            assert!(monitoring.start(&topic, "", b"1", Local::now()));
            assert_eq!(monitoring.topics[&topic][""].color_ix, ix % CHART_COLOR_COUNT);
            assert!(monitoring.start(&topic, "", b"2", Local::now()));
        }
        monitoring.stop(chart_id(&monitoring, "topic/0", ""));
        monitoring.toggle_chart(chart_id(&monitoring, "topic/1", ""));
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
        assert!(monitoring.charts[&ChartId(0)].smooth);
        assert!(monitoring.charts[&ChartId(1)].smooth);
        assert!(!monitoring.set_smoothing(chart_id(&monitoring, "missing", ""), false));
        assert!(monitoring.set_smoothing(chart_id(&monitoring, "a", ""), false));
        assert!(!monitoring.set_smoothing(chart_id(&monitoring, "a", ""), false));
        monitoring.toggle_chart(chart_id(&monitoring, "a", ""));
        monitoring.receive(&message("a", "3"));
        assert!(!monitoring.charts[&ChartId(0)].smooth);
        assert!(monitoring.charts[&ChartId(1)].smooth);
        assert!(monitoring.set_smoothing(chart_id(&monitoring, "a", ""), true));
        assert_eq!(monitoring.topics["a"][""].samples.len(), 2);
    }

    #[test]
    fn chart_toggle_is_per_chart_and_preserves_history_and_live_updates() {
        let mut monitoring = Monitoring::default();
        let seed = message("a", r#"{"n":1,"other":2}"#);
        monitoring.start("a", "/n", &seed.payload, seed.received_at);
        monitoring.start("a", "/other", &seed.payload, seed.received_at);
        assert!(!monitoring.toggle_chart(chart_id(&monitoring, "missing", "/n")));
        assert!(monitoring.toggle_chart(chart_id(&monitoring, "a", "/n")));
        assert_eq!(monitoring.charts[&ChartId(0)].kind, ChartKind::Area);
        assert_eq!(monitoring.charts[&ChartId(1)].kind, ChartKind::Line);
        monitoring.receive(&message("a", r#"{"n":3,"other":4}"#));
        assert!(monitoring.toggle_chart(chart_id(&monitoring, "a", "/n")));
        let series = &monitoring.topics["a"]["/n"];
        assert_eq!(monitoring.charts[&ChartId(0)].kind, ChartKind::Line);
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
    fn start_seeds_once_and_duplicates_without_replacing_history() {
        let mut monitoring = Monitoring::default();
        let time = Local::now();
        assert!(monitoring.start("a", "/n", br#"{"n":-2}"#, time));
        assert!(monitoring.start("a", "/n", br#"{"n":99}"#, time));
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
        assert!(monitoring.stop(chart_id(&monitoring, "a", "")));
        assert!(!monitoring.contains("a", ""));
        assert!(!monitoring.stop(chart_id(&monitoring, "a", "")));
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
