// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `video … keys` self-evaluation: correlate pre-routing input attempts against the
//! frames this recording captured.
//!
//! It is a sampling instrument: a key's effect is seen at the next captured frame, so every
//! reading is bounded below by the recorder's sampling grid, published beside it
//! (`capture_floor_ms`). Two limits are disclosed in band; neither filters anything (every
//! correlated attempt stays in `n` and the percentiles):
//!
//! 1. **The capture floor.** At `N` fps an attempt waits up to `1000/N` ms for the next
//!    look, so a 1 ms and a 15 ms echo read the same at 30 fps. Field names say
//!    `frame_change` or `capture_`, never latency; `at_capture_floor_n` counts readings that
//!    are a bound rather than a measurement.
//! 2. **Cross-attribution during sustained typing.** The observed change is the first
//!    fingerprint move after the attempt, often the previous key's echo, so readings come
//!    out faster than reality. A fingerprint cannot say which key moved it, but the cadence
//!    is observable: when `attempt_gap_p50_ms` is no slower than `frame_change_max_ms`,
//!    `attempts_outpace_readings` flags the take as ambiguous.
//!
//! Key→photon latency is out of reach here. The nearest instrument is `ctl metrics
//! percentiles` (`input_p50/p95/p99_ms`), sampled per keystroke, with its bounds documented
//! on `crate::control::control_query::cmd_metrics`.

/// One correlated attempt, before it is rendered to JSON.
struct Correlated {
    /// Attempt stamp, `metrics::now_us` epoch.
    t_us: u64,
    /// Delay from the attempt to the first captured frame whose fingerprint
    /// moved. `None` when no captured frame after it moved.
    frame_change_us: Option<u64>,
    /// Delay from the attempt to the recorder's next look: the smallest value
    /// `frame_change_us` could take. `None` when no frame was captured at or
    /// after the attempt.
    capture_floor_us: Option<u64>,
    /// Why the row is null, when it is.
    unobserved: Option<&'static str>,
}

/// Sampled-fingerprint move that counts as "the screen changed". The
/// fingerprint is a `step_by(64)` byte sum, so a single glyph moves it by far
/// more than this.
const FP_CHANGE: u64 = 200;

fn us_to_ms(us: u64) -> f64 {
    us as f64 / 1000.0
}

/// The `numer/denom` quantile of a sorted slice (`1, 2` is p50).
fn pct(sorted: &[f64], numer: usize, denom: usize) -> f64 {
    sorted[(sorted.len() * numer / denom).min(sorted.len() - 1)]
}

/// Median gap between consecutive captured frames: the recorder's achieved
/// sampling interval, not the requested `fps=`. `None` for fewer than two
/// frames.
fn capture_interval_us(frames: &[(u64, u64)]) -> Option<u64> {
    let mut gaps: Vec<u64> = frames
        .windows(2)
        .map(|w| w[1].0.saturating_sub(w[0].0))
        .collect();
    if gaps.is_empty() {
        return None;
    }
    gaps.sort_unstable();
    Some(gaps[gaps.len() / 2])
}

/// Median gap between consecutive input attempts (the typing cadence). `None`
/// for fewer than two attempts. Compared against the readings to detect
/// cross-attribution (module docs, limit 2).
fn attempt_gap_p50_us(inputs: &[(u64, String)]) -> Option<u64> {
    let mut gaps: Vec<u64> = inputs
        .windows(2)
        .map(|w| w[1].0.saturating_sub(w[0].0))
        .collect();
    if gaps.is_empty() {
        return None;
    }
    gaps.sort_unstable();
    Some(gaps[gaps.len() / 2])
}

/// Correlate every attempt against the captured frame series.
///
/// `frames` is `(t_us, fingerprint)` in capture order; `inputs` is
/// `(t_us, …)` in attempt order.
fn correlate(inputs: &[(u64, String)], frames: &[(u64, u64)]) -> Vec<Correlated> {
    let mut out = Vec::with_capacity(inputs.len());
    for (t, _) in inputs {
        // The reference is the last frame captured strictly before the attempt:
        // what was on screen when the key was pressed.
        let Some((_, ref_fp)) = frames.iter().rev().find(|(ft, _)| ft < t).copied() else {
            // The attempt predates the first captured frame: keep its row so
            // `key_response` stays as long as `inputs[]`.
            out.push(Correlated {
                t_us: *t,
                frame_change_us: None,
                capture_floor_us: None,
                unobserved: Some("no captured frame precedes this attempt"),
            });
            continue;
        };
        let mut after = frames.iter().filter(|(ft, _)| ft >= t).copied();
        let Some((first_after_us, first_after_fp)) = after.next() else {
            out.push(Correlated {
                t_us: *t,
                frame_change_us: None,
                capture_floor_us: None,
                unobserved: Some("no captured frame follows this attempt"),
            });
            continue;
        };
        let capture_floor_us = Some(first_after_us.saturating_sub(*t));
        let hit_us = if first_after_fp.abs_diff(ref_fp) > FP_CHANGE {
            Some(first_after_us)
        } else {
            after
                .find(|(_, fp)| fp.abs_diff(ref_fp) > FP_CHANGE)
                .map(|(ft, _)| ft)
        };
        match hit_us {
            Some(ft) => out.push(Correlated {
                t_us: *t,
                frame_change_us: Some(ft.saturating_sub(*t)),
                capture_floor_us,
                unobserved: None,
            }),
            None => out.push(Correlated {
                t_us: *t,
                frame_change_us: None,
                capture_floor_us,
                unobserved: Some("no captured frame after this attempt changed"),
            }),
        }
    }
    out
}

/// The sentence that travels with every reading, in band, so grepping a single
/// line still gets it.
const SEMANTICS: &str = "frame_change_ms is the delay from a pre-routing input ATTEMPT to the first \
     frame THIS RECORDING CAPTURED whose fingerprint moved. It is the recorder's observation, NOT \
     key->photon latency: it cannot go below capture_floor_ms (the wait until this take's next \
     captured frame), it cannot tell which attempt caused the change it saw (see \
     attempts_outpace_readings), and it observes neither compositor pickup nor scanout. For \
     key->photon latency use `ctl metrics percentiles` (input_p50/p95/p99_ms), which samples per \
     keystroke instead of per captured frame.";

/// Build the `"analysis"` object for a take that logged at least one attempt.
///
/// `inputs` is `(t_us, json_field)` where `json_field` is the already-escaped
/// `"ch":"a"` / `"key":"ArrowUp"` fragment. `frames` is `(t_us, fingerprint)`
/// in capture order. Returns the complete `  "analysis": { … },\n` text,
/// two-space indented to sit inside `index.json`.
pub(crate) fn analysis_block(inputs: &[(u64, String)], frames: &[(u64, u64)]) -> String {
    let rows = correlate(inputs, frames);
    // One row per attempt: `key_response` and `inputs[]` must be the same
    // length, or a dropped attempt is indistinguishable from an idle one.
    debug_assert_eq!(rows.len(), inputs.len());
    let mut row_lines = String::new();
    for (row, (_, field)) in rows.iter().zip(inputs.iter()) {
        if !row_lines.is_empty() {
            row_lines.push_str(",\n");
        }
        let t = row.t_us;
        row_lines.push_str(&format!("      {{{field},\"t_us\":{t},"));
        match row.frame_change_us {
            Some(us) => row_lines.push_str(&format!("\"frame_change_ms\":{:.1},", us_to_ms(us))),
            None => row_lines.push_str("\"frame_change_ms\":null,"),
        }
        match row.capture_floor_us {
            Some(us) => row_lines.push_str(&format!("\"capture_floor_ms\":{:.1},", us_to_ms(us))),
            None => row_lines.push_str("\"capture_floor_ms\":null,"),
        }
        let at_floor = matches!(
            (row.frame_change_us, row.capture_floor_us),
            (Some(change), Some(floor)) if change == floor
        );
        row_lines.push_str(&format!("\"at_capture_floor\":{at_floor}"));
        if let Some(why) = row.unobserved {
            row_lines.push_str(&format!(",\"unobserved\":\"{why}\""));
        }
        row_lines.push('}');
    }

    let mut changes: Vec<f64> = rows
        .iter()
        .filter_map(|r| r.frame_change_us.map(us_to_ms))
        .collect();
    if changes.is_empty() {
        return format!(
            "  \"analysis\": {{\n    \"key_response\": [\n{row_lines}\n    ],\n    \
             \"n\": 0,\n    \
             \"note\": \"input attempts logged but no later captured frame changed; inputs are \
             not delivery receipts, and a take that captured no frame after an attempt cannot \
             answer for it\",\n    \
             \"semantics\": \"{SEMANTICS}\"\n  }},\n"
        );
    }
    changes.sort_by(f64::total_cmp);
    let mut floors: Vec<f64> = rows
        .iter()
        // Only attempts that produced a reading contribute to the floor
        // summary, so the floor is comparable to the percentiles above it.
        .filter(|r| r.frame_change_us.is_some())
        .filter_map(|r| r.capture_floor_us.map(us_to_ms))
        .collect();
    floors.sort_by(f64::total_cmp);
    let n = changes.len();
    let p50 = pct(&changes, 1, 2);
    let p90 = pct(&changes, 9, 10);
    let max = changes[n - 1];
    // A reading implies a first-after frame, so `floors` and `changes` have the
    // same population.
    debug_assert_eq!(floors.len(), changes.len());
    let floor_p50 = pct(&floors, 1, 2);
    let floor_max = floors[floors.len() - 1];
    let at_floor_n = rows
        .iter()
        .filter(|r| matches!((r.frame_change_us, r.capture_floor_us), (Some(c), Some(f)) if c == f))
        .count();
    let interval_field = capture_interval_us(frames)
        .map(us_to_ms)
        .map_or_else(|| "null".to_string(), |ms| format!("{ms:.1}"));
    // Ambiguity is stated for the take, not per row: keys arriving no slower than the
    // readings mean a key was pressed while an earlier one's change was outstanding.
    let attempt_gap_ms = attempt_gap_p50_us(inputs).map(us_to_ms);
    let gap_field = attempt_gap_ms.map_or_else(|| "null".to_string(), |ms| format!("{ms:.1}"));
    let outpaced = attempt_gap_ms.is_some_and(|gap| gap <= max);

    // The headline says what the recording resolved relative to its own floor
    // and typing cadence; it never grades the terminal's echo latency.
    let headline = if at_floor_n == n {
        format!(
            "AT CAPTURE FLOOR: all {n} readings landed on the recorder's FIRST captured frame \
             after the attempt, so this take resolved nothing below its own ~{floor_p50:.1} ms \
             sampling floor. It bounds the terminal's echo at <= that; it does not measure it."
        )
    } else if outpaced {
        format!(
            "AMBIGUOUS: attempts arrived ~{gap_field} ms apart, no slower than the \
             {max:.1} ms slowest reading, so at least one key was pressed while an earlier key's \
             change was still outstanding and these readings may be crediting the earlier key — \
             i.e. they can read FASTER than the terminal is. Retype with gaps well above \
             {max:.1} ms, or measure with `ctl metrics percentiles`."
        )
    } else if p50 <= floor_p50 * 2.0 {
        format!(
            "NEAR CAPTURE FLOOR: the ~{floor_p50:.1} ms sampling floor is most of the \
             {p50:.1} ms median reading ({at_floor_n} of {n} readings are floor-limited), so \
             little of this figure belongs to the terminal."
        )
    } else {
        format!(
            "ABOVE CAPTURE FLOOR: the {p50:.1} ms median reading is well clear of the \
             ~{floor_p50:.1} ms sampling floor and of the ~{gap_field} ms typing cadence, so the \
             delay is in the terminal, the shell or the load rather than in this recorder — \
             still measured in captured frames, not photons."
        )
    };

    format!(
        "  \"analysis\": {{\n    \"key_response\": [\n{row_lines}\n    ],\n    \
         \"n\": {n},\n    \
         \"frame_change_p50_ms\": {p50:.1}, \"frame_change_p90_ms\": {p90:.1}, \
         \"frame_change_max_ms\": {max:.1},\n    \
         \"capture_floor_p50_ms\": {floor_p50:.1}, \"capture_floor_max_ms\": {floor_max:.1},\n    \
         \"capture_interval_p50_ms\": {interval_field},\n    \
         \"at_capture_floor_n\": {at_floor_n},\n    \
         \"attempt_gap_p50_ms\": {gap_field}, \"attempts_outpace_readings\": {outpaced},\n    \
         \"capture_verdict\": \"{headline}\",\n    \
         \"semantics\": \"{SEMANTICS}\"\n  }},\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `(t_us, serialized input row)` — the shape `inputs[]` rows take here.
    type Inputs = Vec<(u64, String)>;
    /// `(t_us, fingerprint)` captured frames.
    type Frames = Vec<(u64, u64)>;

    /// A take from an explicit world model: the terminal echoes `echo_us` after
    /// each attempt and the recorder looks every `interval_us`, so the gap
    /// between what the terminal did and what the block reports is exact.
    fn take(interval_us: u64, echo_us: u64, keys: &[u64], span_us: u64) -> (Inputs, Frames) {
        let inputs = keys
            .iter()
            .map(|t| (*t, "\"ch\":\"x\"".to_string()))
            .collect();
        let mut frames = Vec::new();
        let mut ft = 0;
        while ft <= span_us {
            let echoed = keys.iter().filter(|k| *k + echo_us <= ft).count() as u64;
            frames.push((ft, 1_000 * echoed));
            ft += interval_us;
        }
        (inputs, frames)
    }

    fn field(block: &str, key: &str) -> String {
        let at = block
            .find(&format!("\"{key}\":"))
            .unwrap_or_else(|| panic!("{key} missing from:\n{block}"));
        let rest = block[at + key.len() + 3..].trim_start();
        if let Some(open) = rest.strip_prefix('"') {
            // A string value: take it whole rather than to the first comma.
            return open[..open.find('"').expect("closed string")].to_string();
        }
        rest.split([',', '\n', '}'])
            .next()
            .unwrap()
            .trim()
            .to_string()
    }

    /// A terminal echoing in 1 ms, recorded at 30 fps, is not reported as a
    /// ~19 ms terminal: the reading is the sampling grid, and the block says so.
    #[test]
    fn a_floor_limited_take_says_it_measured_the_recorder() {
        let (inputs, frames) = take(33_333, 1_000, &[40_000, 148_000, 255_000, 361_000], 500_000);
        let block = analysis_block(&inputs, &frames);
        assert_eq!(field(&block, "at_capture_floor_n"), field(&block, "n"));
        assert!(
            field(&block, "capture_verdict").starts_with("AT CAPTURE FLOOR"),
            "{block}"
        );
        // Every reading equals its own floor, and the floor is published.
        assert_eq!(
            field(&block, "frame_change_p50_ms"),
            field(&block, "capture_floor_p50_ms"),
            "{block}"
        );
    }

    /// A 120 ms echo recorded at 30 fps with keys ~107 ms apart reads ~30 ms at
    /// p50 (each key is credited with an earlier key's echo); the take is
    /// declared ambiguous and the cadence that makes it so is published.
    #[test]
    fn a_slow_terminal_typed_fast_is_declared_ambiguous_not_good() {
        let keys = [
            40_000, 148_000, 255_000, 361_000, 470_000, 583_000, 690_000, 805_000, 911_000,
            1_020_000,
        ];
        let (inputs, frames) = take(33_333, 120_000, &keys, 1_400_000);
        let block = analysis_block(&inputs, &frames);
        // The misleading median is kept (no silent filter) but framed by the
        // cadence that produced it.
        assert_eq!(field(&block, "frame_change_p50_ms"), "30.0", "{block}");
        assert_eq!(
            field(&block, "attempts_outpace_readings"),
            "true",
            "{block}"
        );
        assert_eq!(field(&block, "attempt_gap_p50_ms"), "108.0", "{block}");
        assert!(
            field(&block, "capture_verdict").starts_with("AMBIGUOUS"),
            "{block}"
        );
        // The one attempt that raced nothing carries the true number.
        assert!(block.contains("\"frame_change_ms\":126.7,"), "{block}");
    }

    /// The same 120 ms terminal typed slowly enough to settle between keys:
    /// no ambiguity, nothing at the floor, and a headline that puts the delay
    /// in the terminal rather than the recorder.
    #[test]
    fn a_settled_slow_take_reads_above_the_floor() {
        let (inputs, frames) = take(
            33_333,
            200_000,
            &[40_000, 500_000, 960_000, 1_420_000],
            2_000_000,
        );
        let block = analysis_block(&inputs, &frames);
        assert_eq!(
            field(&block, "attempts_outpace_readings"),
            "false",
            "{block}"
        );
        assert_eq!(field(&block, "at_capture_floor_n"), "0", "{block}");
        assert!(
            field(&block, "capture_verdict").starts_with("ABOVE CAPTURE FLOOR"),
            "{block}"
        );
        // 200 ms echo on a 33.3 ms grid: every reading rounds up to the next
        // captured frame, so the p50 is the true echo plus < one interval.
        let p50: f64 = field(&block, "frame_change_p50_ms").parse().unwrap();
        assert!((200.0..233.4).contains(&p50), "{block}");
    }

    /// An attempt the recorder cannot answer for keeps its row and says why.
    #[test]
    fn an_unanswerable_attempt_keeps_its_row_and_names_the_reason() {
        let frames = vec![(500_000u64, 0u64), (533_333, 0)];
        let inputs = vec![
            (100_000u64, "\"ch\":\"a\"".to_string()),
            (600_000, "\"key\":\"ArrowUp\"".to_string()),
        ];
        let block = analysis_block(&inputs, &frames);
        assert_eq!(block.matches("\"t_us\":").count(), 2, "{block}");
        assert!(
            block.contains("no captured frame precedes this attempt"),
            "{block}"
        );
        assert!(
            block.contains("no captured frame follows this attempt"),
            "{block}"
        );
        assert!(field(&block, "n") == "0", "{block}");
    }

    /// The achieved cadence is measured from the frame stamps, not taken from
    /// the requested `fps=`: a take that fell behind publishes its real floor.
    #[test]
    fn the_capture_interval_is_measured_not_requested() {
        let (inputs, frames) = take(50_000, 1_000, &[40_000, 148_000, 255_000], 400_000);
        let block = analysis_block(&inputs, &frames);
        assert_eq!(field(&block, "capture_interval_p50_ms"), "50.0", "{block}");
    }
}
