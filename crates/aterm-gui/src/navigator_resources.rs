// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Local-pane readings for the compact one-minute charts in the session panel.

use super::*;
use crate::native_ui::{AuditedCustomNode, LogicalRect};
use crate::settings::Roles;
use crate::widget::{DrawPrim, rgba};
use aterm_vitals::{pane_cpu, pane_resources, pane_tokens};

pub(crate) const AUDIT: &str = "navigator.resource-history";
pub(crate) type Panel = HashMap<u64, Readings>;
type Series = [Option<f64>; pane_cpu::BUCKETS];
const GAP: f32 = 5.0;
const PLOT_HEIGHT: f32 = 8.0;

#[derive(Clone, Copy)]
pub(super) enum Kind {
    Cpu,
    Input,
    Output,
    Memory,
    DiskRead,
    DiskWrite,
    TokenIn,
    TokenOut,
}
use Kind::{Cpu, DiskRead, DiskWrite, Input, Memory, Output, TokenIn, TokenOut};
const METRICS: [Kind; 8] = [
    Cpu, Input, Output, Memory, DiskRead, DiskWrite, TokenIn, TokenOut,
];

#[derive(Clone, Copy)]
enum Axis {
    Cores,
    TerminalBytes,
    Mib,
    DiskBytes,
    Tokens,
}

struct Legend {
    key: &'static str,
    caption: &'static str,
    short: &'static str,
    spoken: &'static str,
    axis: Axis,
}

impl Legend {
    const fn new(
        key: &'static str,
        caption: &'static str,
        short: &'static str,
        spoken: &'static str,
        axis: Axis,
    ) -> Self {
        Self {
            key,
            caption,
            short,
            spoken,
            axis,
        }
    }
}

impl Kind {
    const fn legend(self) -> Legend {
        match self {
            Cpu => Legend::new("CPU", "CPU", "CPU", "CPU", Axis::Cores),
            Input => Legend::new("In", "In", "↓", "Terminal input", Axis::TerminalBytes),
            Output => Legend::new("Out", "Out", "↑", "Terminal output", Axis::TerminalBytes),
            Memory => Legend::new("Mem", "Mem", "Mem", "Memory footprint", Axis::Mib),
            DiskRead => Legend::new("DiskRead", "Disk R", "R", "Disk read", Axis::DiskBytes),
            DiskWrite => Legend::new("DiskWrite", "Disk W", "W", "Disk write", Axis::DiskBytes),
            TokenIn => Legend::new("TokenIn", "Tok in", "Tok↓", "Model input", Axis::Tokens),
            TokenOut => Legend::new("TokenOut", "Tok out", "Tok↑", "Model output", Axis::Tokens),
        }
    }
}

impl Axis {
    const fn unit(self) -> &'static str {
        match self {
            Self::Cores => "reported cores",
            Self::TerminalBytes | Self::DiskBytes => "bytes per second",
            Self::Mib => "MiB",
            Self::Tokens => "tokens per second",
        }
    }

    fn amount(self, v: f64) -> (f64, &'static str) {
        let (factor, unit) = match self {
            Self::Cores => (1., "c"),
            Self::Mib if v >= 1024. => (1024., "GiB"),
            Self::Mib if v > 0. && v < 1. / 1024. => (1. / 1_048_576., "B"),
            Self::Mib if v > 0. && v < 1. => (1. / 1024., "KiB"),
            Self::Mib => (1., "MiB"),
            Self::Tokens if v >= 1e6 => (1e6, "M/s"),
            Self::Tokens if v >= 1e3 => (1e3, "k/s"),
            Self::Tokens => (1., "/s"),
            Self::TerminalBytes | Self::DiskBytes if v >= 1e9 => (1e9, "GB/s"),
            Self::TerminalBytes | Self::DiskBytes if v >= 1e6 => (1e6, "MB/s"),
            Self::TerminalBytes | Self::DiskBytes if v >= 1e3 => (1e3, "kB/s"),
            Self::TerminalBytes | Self::DiskBytes => (1., "B/s"),
        };
        (v / factor, unit)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Trace {
    pub(super) latest: Option<f64>,
    pub(super) values: Series,
}

impl Trace {
    fn sampled<T: Copy>(
        latest: Option<T>,
        buckets: &[Option<T>; pane_cpu::BUCKETS],
        value: impl Fn(T) -> Option<f64>,
    ) -> Self {
        Self {
            latest: latest.and_then(&value),
            values: buckets.map(|r| r.and_then(&value)),
        }
    }

    fn stream(values: Series) -> Self {
        Self {
            latest: values[pane_cpu::BUCKETS - 1],
            values,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Readings {
    pub(super) traces: [Trace; 8],
    pub(crate) cpu_read: Option<pane_cpu::Reading>,
    pub(crate) process_read: Option<pane_resources::Reading>,
    pub(crate) token_read: Option<pane_tokens::Reading>,
    pub(super) memory_floors: [bool; pane_cpu::BUCKETS],
}

fn partial(r: pane_resources::Reading) -> bool {
    r.footprint_unread != 0 || !r.membership_complete
}

impl Readings {
    pub(crate) fn of(
        cpu: Option<&pane_cpu::History>,
        resources: Option<&pane_resources::History>,
        tokens: Option<&pane_tokens::History>,
        at_ns: u64,
        input: Series,
        output: Series,
    ) -> Self {
        let cpu_read = cpu.and_then(|h| h.latest);
        let cpu_buckets = cpu.map(|h| h.buckets(at_ns)).unwrap_or_default();
        let process_read = resources.and_then(|h| h.latest);
        let process_buckets = resources.map(|h| h.buckets(at_ns)).unwrap_or_default();
        let token_read = tokens.and_then(|h| h.latest);
        let current_tokens = tokens.and_then(|h| h.latest_at(at_ns));
        let token_buckets = tokens.map(|h| h.buckets(at_ns)).unwrap_or_default();
        Self {
            traces: [
                Trace::sampled(cpu_read, &cpu_buckets, |r| Some(r.cores)),
                Trace::stream(input),
                Trace::stream(output),
                Trace::sampled(process_read, &process_buckets, |r| {
                    r.footprint.map(|bytes| bytes as f64 / 1_048_576.)
                }),
                Trace::sampled(process_read, &process_buckets, |r| r.read_bps),
                Trace::sampled(process_read, &process_buckets, |r| r.write_bps),
                Trace::sampled(current_tokens, &token_buckets, |r| r.input_per_s),
                Trace::sampled(current_tokens, &token_buckets, |r| r.output_per_s),
            ],
            memory_floors: process_buckets.map(|r| r.is_some_and(partial)),
            cpu_read,
            process_read,
            token_read,
        }
    }

    fn lower_bound(&self, kind: Kind) -> bool {
        matches!(kind, Memory) && self.process_read.is_some_and(partial)
    }

    pub(super) fn description(&self, scales: Scales, at_ns: u64) -> String {
        use std::fmt::Write;
        let mut text =
            String::from("Last minute. Twelve closed five-second buckets, oldest first.");
        for kind in METRICS {
            let trace = &self.traces[kind as usize];
            let unit = kind.legend().axis.unit();
            let latest = trace.latest.map_or_else(
                || "unavailable".into(),
                |value| {
                    let qualifier = if self.lower_bound(kind) {
                        "at least "
                    } else {
                        ""
                    };
                    format!(
                        "{qualifier}{} {unit}",
                        measured_number(value, self.lower_bound(kind))
                    )
                },
            );
            let count = trace.values.iter().flatten().count();
            let _ = write!(
                text,
                " {} {}, scale 0 to {} {}, {count} of {} observed.",
                kind.legend().spoken,
                latest,
                number(scales.get(kind)),
                unit,
                pane_cpu::BUCKETS
            );
            if count == 0 {
                text.push_str(" All gaps.");
            } else if trace.values.iter().all(|value| *value == Some(0.))
                && !(matches!(kind, Memory) && self.memory_floors.iter().any(|v| *v))
            {
                text.push_str(" All zero.");
            } else {
                for (index, value) in trace.values.iter().enumerate() {
                    text.push_str(if index == 0 { " " } else { ", " });
                    if matches!(kind, Memory) && self.memory_floors[index] && value.is_some() {
                        text.push_str("at least ");
                    }
                    match value {
                        Some(v) => text.push_str(&measured_number(
                            *v,
                            matches!(kind, Memory) && self.memory_floors[index],
                        )),
                        None => text.push_str("gap"),
                    }
                }
                text.push('.');
            }
        }
        let age = |end| at_ns.saturating_sub(end) as f64 / 1e9;
        if let Some(r) = self.cpu_read {
            let _ = write!(
                text,
                " CPU observed {:.1} seconds ago over {:.2} seconds.",
                age(r.at_ns),
                r.window_ns as f64 / 1e9
            );
        }
        if let Some(r) = self.process_read {
            let _ = write!(text, " Process sweep {:.1} seconds ago.", age(r.at_ns));
            if let Some(window) = r.window_ns {
                let _ = write!(text, " Disk interval {:.2} seconds.", window as f64 / 1e9);
            }
            if r.footprint_unread != 0 {
                let _ = write!(
                    text,
                    " Footprints unread for {} of {} observed processes.",
                    r.footprint_unread, r.members
                );
            }
            if !r.membership_complete {
                text.push_str(" Process ownership is incomplete.");
            }
        }
        if let Some(r) = self.token_read {
            let _ = write!(
                text,
                " Last token interval ended {:.1} seconds ago and covered {:.2} seconds. Collected {:.1} seconds ago.",
                age(r.at_ns),
                r.window_ns as f64 / 1e9,
                age(r.collected_ns)
            );
            for (kind, rate) in [(TokenIn, r.input_per_s), (TokenOut, r.output_per_s)] {
                if self.traces[kind as usize].latest.is_none()
                    && let Some(rate) = rate
                {
                    let _ = write!(
                        text,
                        " Last observed {} {} tokens per second.",
                        kind.legend().spoken.to_lowercase(),
                        number(rate)
                    );
                }
            }
        }
        text
    }
}

fn number(value: f64) -> String {
    if value == 0. {
        return "0".into();
    }
    if value < 0.01 {
        let scientific = format!("{value:.2e}");
        let (digits, power) = scientific.split_once('e').unwrap_or((&scientific, "0"));
        return format!(
            "{}e{power}",
            digits.trim_end_matches('0').trim_end_matches('.')
        );
    }
    let s = format!("{value:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_owned()
}

/// Round a partial footprint toward zero so its displayed lower bound remains true.
fn measured_number(value: f64, floor: bool) -> String {
    if !floor || value == 0. {
        return number(value);
    }
    let precision = if value < 0.01 {
        10f64.powf(2. - value.log10().floor())
    } else {
        100.
    };
    number((value * precision).floor() / precision)
}

#[derive(Clone, Copy)]
pub(super) struct Scales([f64; 5]);

impl Scales {
    pub(super) fn visible<'a>(rows: impl Iterator<Item = &'a Readings>) -> Self {
        let mut upper = [1.; 5];
        for row in rows {
            for kind in METRICS {
                let i = kind.legend().axis as usize;
                let trace = &row.traces[kind as usize];
                upper[i] = trace
                    .values
                    .iter()
                    .flatten()
                    .copied()
                    .chain(trace.latest)
                    .fold(upper[i], f64::max);
            }
        }
        Self(upper)
    }

    fn get(self, kind: Kind) -> f64 {
        self.0[kind.legend().axis as usize]
    }
}

/// Prefer a full value with units, rounding the compact display only as much as needed.
fn caption(kind: Kind, latest: Option<f64>, lower_bound: bool, width: f32) -> String {
    let Some(value) = latest else {
        return "—".into();
    };
    let (v, unit) = kind.legend().axis.amount(value);
    let prefix = if lower_bound { "≥" } else { "" };
    let fits = |s: &str| text_width(s, SemanticRole::Text, StyleRef::Note) <= width;
    if v > 0. && v < 0.01 {
        let precise = format!("{prefix}{}{unit}", measured_number(v, lower_bound));
        if lower_bound || fits(&precise) {
            return precise;
        }
        return format!("<1{unit}");
    }
    let mut text = String::new();
    for places in [100., 10., 1.] {
        let rounded = if lower_bound {
            (v * places).floor()
        } else {
            (v * places).round()
        } / places;
        // A positive measurement too small for fixed-point keeps its significant digits.
        let n = if rounded == 0. && v != 0. { v } else { rounded };
        text = format!("{prefix}{}{unit}", measured_number(n, lower_bound));
        if fits(&text) {
            break;
        }
    }
    text
}

/// Put the related directions together at every width. A narrow panel grows vertically instead
/// of packing all eight traces into illegible columns.
fn bands(width: f32, line: f32) -> &'static [&'static [Kind]] {
    let unit = line / 18.;
    if width >= 4. * 84. * unit + 3. * GAP {
        &[
            &[Cpu, Memory, Input, Output],
            &[DiskRead, DiskWrite, TokenIn, TokenOut],
        ]
    } else if width >= 3. * 49. * unit + 2. * GAP {
        &[
            &[Cpu, Input, Output],
            &[Memory, DiskRead, DiskWrite],
            &[TokenIn, TokenOut],
        ]
    } else {
        &[
            &[Cpu, Memory],
            &[Input, Output],
            &[DiskRead, DiskWrite],
            &[TokenIn, TokenOut],
        ]
    }
}

pub(super) fn chart_height(width: f32, line: f32, details: bool) -> f32 {
    bands(width, line)
        .iter()
        .map(|band| band_lines(band, details) * caption_line(line))
        .sum()
}

fn band_lines(kinds: &[Kind], details: bool) -> f32 {
    if !details {
        return 2.;
    }
    if kinds.iter().any(|kind| matches!(kind, TokenIn | TokenOut)) {
        5.
    } else {
        4.
    }
}

fn collection_age(kind: Kind, readings: &Readings, at_ns: u64) -> String {
    let end = match kind {
        Cpu => readings.cpu_read.map(|r| r.at_ns),
        Memory | DiskRead | DiskWrite => readings.process_read.map(|r| r.at_ns),
        TokenIn | TokenOut => readings.token_read.map(|r| r.collected_ns),
        Input | Output => readings.traces[kind as usize]
            .latest
            .map(|_| at_ns - at_ns % pane_cpu::BUCKET_NS),
    };
    let Some(age) = end.and_then(|t| at_ns.checked_sub(t)) else {
        return "—".into();
    };
    let seconds = age / 1_000_000_000;
    match seconds {
        0 => "now".into(),
        1..60 => format!("{seconds}s ago"),
        60..3600 => format!("{}m ago", seconds / 60),
        _ => format!("{}h ago", seconds / 3600),
    }
}

fn caption_line(line: f32) -> f32 {
    (line * 0.8).ceil()
}

fn metric_cell(
    key: &str,
    kind: Kind,
    readings: &Readings,
    scales: Scales,
    details_at: Option<u64>,
    width: f32,
    line: f32,
) -> UiNode {
    let legend = kind.legend();
    let label = legend.caption;
    let trace = &readings.traces[kind as usize];
    let base = format!("{key}/{}", legend.key);
    let value = caption(kind, trace.latest, readings.lower_bound(kind), width);
    let encoded = trace
        .values
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let Some(v) = v else {
                return '-';
            };
            let first = if matches!(kind, Memory) && readings.memory_floors[i] {
                b'a'
            } else {
                b'A'
            };
            let level = if *v == 0. {
                0
            } else {
                (v / scales.get(kind) * 24.).round().clamp(1., 24.) as u8
            };
            char::from(first + level)
        })
        .collect();
    let chart = UiNode::new(
        format!("{base}/history"),
        UiContent::Custom(AuditedCustomNode {
            audit_id: AUDIT,
            role: SemanticRole::Group,
            label: legend.spoken.to_owned(),
            paint: Some(encoded),
            action: None,
            focusable: false,
            focused: false,
            focus_visible: false,
            progress: None,
            motion: None,
        }),
    )
    .layout(
        Layout::default()
            .width(Length::Fill)
            .height(Length::Fixed(PLOT_HEIGHT)),
    );
    let measure = |label: &str| text_width(label, SemanticRole::Text, StyleRef::Note);
    let label = if measure(label) + GAP + pane_cpu::BUCKETS as f32 <= width {
        label
    } else {
        legend.short
    };
    let title = words(
        format!("{base}/label"),
        label.into(),
        SemanticRole::Text,
        StyleRef::Note,
        Length::Fixed(measure(label).min(width)),
    );
    let head = plain_group(format!("{base}/head"))
        .layout(Layout::row().gap(GAP).height(Length::Fixed(line)))
        .children(vec![title, chart]);
    let note = |suffix, text| {
        words(
            format!("{base}/{suffix}"),
            text,
            SemanticRole::Text,
            StyleRef::Note,
            Length::Fill,
        )
        .layout(Layout::default().height(Length::Fixed(line)))
    };
    let mut cells = vec![head, note("value", value)];
    if let Some(at_ns) = details_at {
        let prefix = "0–";
        let ceiling = caption(kind, Some(scales.get(kind)), false, width - measure(prefix));
        cells.push(note("range", format!("{prefix}{ceiling}")));
        cells.push(note("collected", collection_age(kind, readings, at_ns)));
        if let Some(r) = readings.token_read {
            let measured = match kind {
                TokenIn => r.input_per_s,
                TokenOut => r.output_per_s,
                _ => None,
            };
            if let Some(rate) = measured {
                let receipt = if trace.latest.is_some() {
                    format!("over {}s", number(r.window_ns as f64 / 1e9))
                } else {
                    format!(
                        "Last {}",
                        caption(kind, Some(rate), false, width - measure("Last "))
                    )
                };
                cells.push(note("interval", receipt));
            }
        }
    }
    plain_group(base)
        .layout(Layout::column().width(Length::Fixed(width)))
        .children(cells)
}

pub(super) fn charts(
    key: &str,
    readings: &Readings,
    scales: Scales,
    details_at: Option<u64>,
    width: f32,
    line: f32,
) -> UiNode {
    let rows = bands(width, line)
        .iter()
        .enumerate()
        .map(|(index, kinds)| {
            let columns = kinds.len() as f32;
            let extent = ((width - (columns - 1.) * GAP) / columns).max(1.);
            let line = caption_line(line);
            let cells = kinds
                .iter()
                .map(|&kind| metric_cell(key, kind, readings, scales, details_at, extent, line))
                .collect();
            plain_group(format!("{key}/metrics/{index}"))
                .layout(Layout::row().gap(GAP).height(Length::Fixed(
                    band_lines(kinds, details_at.is_some()) * line,
                )))
                .children(cells)
        })
        .collect();
    plain_group(format!("{key}/metrics"))
        .layout(Layout::column().height(Length::Fixed(chart_height(
            width,
            line,
            details_at.is_some(),
        ))))
        .children(rows)
}

pub(crate) fn paint(
    prims: &mut Vec<DrawPrim>,
    rect: LogicalRect,
    encoded: Option<&str>,
    roles: &Roles,
) {
    let Some(data) = encoded.filter(|s| s.len() == pane_cpu::BUCKETS) else {
        return;
    };
    let step = rect.width / pane_cpu::BUCKETS as f32;
    let max_height = rect.height.max(1.0);
    for (index, ch) in data.bytes().enumerate() {
        let lower_bound = ch.is_ascii_lowercase();
        let ch = ch.to_ascii_uppercase();
        if !(b'A'..=b'Y').contains(&ch) {
            continue;
        }
        let height = if ch == b'A' {
            1.0
        } else {
            (2.0 + f32::from(ch - b'B') / 23.0 * (max_height - 2.0)).min(max_height)
        };
        prims.push(DrawPrim::Panel {
            x: rect.x + index as f32 * step,
            y: rect.bottom() - height,
            w: if lower_bound {
                (step * 0.65).max(1.).min(step)
            } else {
                step
            },
            h: if lower_bound { 1. } else { height },
            radius: 0.0,
            fill: rgba(roles.text_secondary, 255),
        });
    }
}
