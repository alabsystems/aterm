// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What predictive echo did on glass, one stderr line per event.

use std::fmt::Write as _;
use std::sync::OnceLock;
use std::time::Instant;

use aterm_core::render::RenderInput;

use crate::predict::{PredictMode, Predictor};

/// Whether the trace is on (`$ATERM_TRACE_PREDICT` set, in a build that reads seams).
pub(crate) fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| aterm_types::dev_seam!("ATERM_TRACE_PREDICT").is_some())
}

/// Microseconds since the trace's epoch (its first stamp).
fn micros(at: Instant) -> u128 {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    at.saturating_duration_since(*EPOCH.get_or_init(Instant::now))
        .as_micros()
}

/// `text` percent-encoded: `%`, space and every byte outside printable ASCII.
fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_graphic() && b != b'%' {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// Row `row` of `scratch` as its glyphs, trailing blanks trimmed.
fn row_text(scratch: &RenderInput, row: usize) -> String {
    let text: String = scratch
        .cells
        .get(row)
        .map(|cells| cells.iter().map(|c| c.ch).collect())
        .unwrap_or_default();
    text.trim_end_matches([' ', '\0']).to_string()
}

const fn mode_name(mode: PredictMode) -> &'static str {
    match mode {
        PredictMode::Off => "off",
        PredictMode::Adaptive => "adaptive",
        PredictMode::Always => "always",
    }
}

/// A press's line ([`press`]): `armed` is the op it armed
/// (`Predictor::armed_since`).
fn press_line(predictor: &Predictor, key: &str, armed: Option<u64>, at: Instant) -> String {
    format!(
        "aterm-predict press t={} key={} armed={} pending={}",
        micros(at),
        encode(key),
        armed.map_or_else(|| "-".to_string(), |s| s.to_string()),
        predictor.pending().len()
    )
}

/// Trace a press the predictor was just handed ([`press_line`]).
pub(crate) fn press(predictor: &Predictor, key: &str, armed: Option<u64>, at: Instant) {
    if on() {
        crate::logging::stderr_line!("{}", press_line(predictor, key, armed, at));
    }
}

/// The rows a paint may touch and how the engine had them, read before it.
pub(crate) struct Under(Vec<(usize, String)>);

impl Under {
    /// The rows of `cells` and of the chain's cursor, as `scratch` holds them before the paint.
    pub(crate) fn read(
        scratch: &RenderInput,
        cells: &[crate::predict::Prediction],
        chain_cursor: Option<(u16, u16)>,
    ) -> Under {
        if !on() {
            return Under(Vec::new());
        }
        Under(Under::rows(scratch, cells, chain_cursor))
    }

    fn rows(
        scratch: &RenderInput,
        cells: &[crate::predict::Prediction],
        chain_cursor: Option<(u16, u16)>,
    ) -> Vec<(usize, String)> {
        let mut rows: Vec<usize> = cells
            .iter()
            .map(|p| usize::from(p.row))
            .chain(chain_cursor.map(|(r, _)| usize::from(r)))
            .collect();
        rows.sort_unstable();
        rows.dedup();
        rows.into_iter()
            .map(|r| (r, row_text(scratch, r)))
            .collect()
    }
}

/// A painted frame's line ([`frame`]): `cursor` the engine's, `shown` the
/// cursor drawn.
fn frame_line(
    predictor: &Predictor,
    under: &Under,
    scratch: &RenderInput,
    cursor: (usize, usize),
    shown: (u16, u16),
    at: Instant,
) -> String {
    let visible = predictor.visible_ops_for_test();
    let newest = predictor.shown_through();
    let mut line = format!(
        "aterm-predict frame t={} mode={} engaged={} visible={visible} newest={} cursor={},{} shown={},{}",
        micros(at),
        mode_name(predictor.mode()),
        u8::from(predictor.engaged_for_test()),
        newest.map_or_else(|| "-".to_string(), |s| s.to_string()),
        cursor.0,
        cursor.1,
        shown.0,
        shown.1,
    );
    for (row, text) in &under.0 {
        let _ = write!(
            line,
            " row={row} under={} over={}",
            encode(text),
            encode(&row_text(scratch, *row))
        );
    }
    line
}

/// Trace the frame that ERASED the prediction the frame before it painted.
pub(crate) fn clear(at: Instant) {
    if on() {
        crate::logging::stderr_line!("aterm-predict clear t={}", micros(at));
    }
}

/// Trace a frame that painted a prediction, after the paint and before the caller
/// moves the drawn cursor: `scratch`'s own cursor is still the engine's, and `shown` the one the
/// caller is about to draw.
pub(crate) fn frame(
    predictor: &Predictor,
    under: &Under,
    scratch: &RenderInput,
    shown: (u16, u16),
    at: Instant,
) {
    if on() {
        let cursor = (scratch.cursor_row, scratch.cursor_col);
        crate::logging::stderr_line!(
            "{}",
            frame_line(predictor, under, scratch, cursor, shown, at)
        );
    }
}

#[cfg(test)]
#[path = "predict_trace_tests.rs"]
mod tests;
