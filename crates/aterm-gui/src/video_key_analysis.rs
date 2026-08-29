// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `video … keys` self-evaluation: correlate pre-routing input attempts
//! against the frames THIS RECORDING ACTUALLY CAPTURED.
//!
//! # What this instrument can and cannot say
//!
//! It is a SAMPLING instrument. It sees the terminal only on the frames the
//! recorder captured, so the earliest moment it can notice a key's effect is
//! the next captured frame after that key — never the moment the pixels
//! changed. Every number here is therefore bounded below by the recorder's own
//! sampling grid, and that bound is published beside the number
//! (`capture_floor_ms`) instead of being left for the reader to remember.
//!
//! Two failure modes have already misled readers of this codebase, and both are
//! now disclosed in band rather than documented somewhere else:
//!
//! 1. **The capture floor.** At a capture cadence of `N` fps, an attempt lands a
//!    uniformly-distributed wait of up to `1000/N` ms before the recorder's next
//!    look. A terminal that echoes in 1 ms and one that echoes in 15 ms produce
//!    the SAME reading at 30 fps. The old block reported that reading as
//!    `p50_ms` with a `verdict` of `INSTANT`/`GOOD`/`SLOW`, which reads as a
//!    statement about the terminal; it was a statement about the recorder.
//!    Every field name here says `frame_change` or `capture_`, and
//!    `at_capture_floor_n` counts the attempts whose reading is a BOUND rather
//!    than a measurement.
//!
//! 2. **Cross-attribution during sustained typing.** The observed change is the
//!    first WHOLE-frame fingerprint move after the attempt — and while typing,
//!    the move that arrives next is very often the PREVIOUS key's echo, not
//!    this one's. That does not merely add noise: it reports FASTER than
//!    reality. A modelled terminal echoing in 120 ms, recorded at 30 fps while
//!    ten keys are typed ~107 ms apart, reports `p50_ms: 30.0` and a `GOOD`
//!    verdict under the old block, because the echo each key is credited with
//!    belongs to an earlier key.
//!
//!    This one is NOT detectable per attempt. A whole-frame fingerprint cannot
//!    say which key caused a move, so a per-row `confounded: false` would be a
//!    fresh lie in the rows it failed to catch — and it fails to catch the
//!    common case, where the misattribution chain has already shifted by one.
//!    What IS exactly observable is the cadence: `attempt_gap_p50_ms` against
//!    `frame_change_max_ms`. When keys arrive no slower than the readings, the
//!    readings are ambiguous as a set, and `attempts_outpace_readings` says so
//!    for the whole take rather than pretending to grade individual rows.
//!
//! Neither disclosure filters anything. Every correlated attempt stays in `n`
//! and in the percentiles — a silent filter is the failure mode this file
//! exists to prevent, not a fix for it.
//!
//! This instrument cannot supply key→photon latency at all. The nearest thing
//! that can is `ctl metrics percentiles` — `input_p50/p95/p99_ms`, the
//! key-arrival→content-present-return histogram, which samples per KEYSTROKE
//! rather than per captured frame and so has no sampling floor. It stops at
//! application-present return, and it has its own honesty bounds; see the
//! `WHAT THESE SLICES INCLUDE` block on `crate::control::control_query::cmd_metrics`.

/// One correlated attempt, before it is rendered to JSON.
struct Correlated {
    /// Attempt stamp, `metrics::now_us` epoch.
    t_us: u64,
    /// Delay from the attempt to the first CAPTURED frame whose fingerprint
    /// moved. `None` when no captured frame after it ever moved.
    frame_change_us: Option<u64>,
    /// Delay from the attempt to the recorder's next look — the smallest value
    /// `frame_change_us` could possibly have taken for this attempt. `None`
    /// when the take captured no frame at or after the attempt.
    capture_floor_us: Option<u64>,
    /// Why the row is null, when it is.
    unobserved: Option<&'static str>,
}

/// Sampled-fingerprint move that counts as "the screen changed". Unchanged
/// from the original block: the fingerprint is a `step_by(64)` byte sum, so a
/// single glyph moves it by far more than this.
const FP_CHANGE: u64 = 200;

fn us_to_ms(us: u64) -> f64 {
    us as f64 / 1000.0
}

/// `p` as a permille index into a sorted slice, using the block's original
/// convention (`len/2` for p50) so existing readings stay comparable.
fn pct(sorted: &[f64], numer: usize, denom: usize) -> f64 {
    sorted[(sorted.len() * numer / denom).min(sorted.len() - 1)]
}

/// Median gap between consecutive captured frames — the recorder's achieved
/// sampling interval, measured rather than assumed from the requested `fps=`.
/// `None` for a take with fewer than two frames.
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

/// Median gap between consecutive input ATTEMPTS — the typing cadence this
/// take actually drove. `None` for fewer than two attempts. Compared against
/// the readings to decide whether the correlation could be crediting the wrong
/// key; see the module header's failure mode 2.
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
        // The reference is the last frame captured strictly BEFORE the attempt:
        // whatever was on screen when the key was pressed.
        let Some((_, ref_fp)) = frames.iter().rev().find(|(ft, _)| ft < t).copied() else {
            // The attempt predates the recording's first captured frame. The
            // old block dropped these rows silently, so `key_response` could be
            // shorter than `inputs[]` with nothing saying why.
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

/// The one sentence that must travel with every reading, emitted in band so a
/// reader who greps a single line still gets it.
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
    // One row per attempt, always: `key_response` and `inputs[]` must be the
    // same length or a reader cannot tell a dropped attempt from an idle one.
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
    // same population by construction — the floor summary is directly
    // comparable to the percentiles above it.
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
    // AMBIGUITY, STATED FOR THE TAKE AND NOT FAKED PER ROW. Keys arriving no
    // slower than the readings themselves means at least one attempt was
    // pressed while a previous one's change was still outstanding, and from
    // there the whole set may be shifted by a key. `None` for a single attempt,
    // which cannot outpace anything.
    let attempt_gap_ms = attempt_gap_p50_us(inputs).map(us_to_ms);
    let gap_field = attempt_gap_ms.map_or_else(|| "null".to_string(), |ms| format!("{ms:.1}"));
    let outpaced = attempt_gap_ms.is_some_and(|gap| gap <= max);

    // THE VERDICT IS ABOUT THE RECORDER. It never grades the terminal, because
    // this instrument cannot: the three old grades (INSTANT/GOOD/SLOW) were
    // read as statements about aterm's echo latency and were statements about
    // the capture cadence. Each arm below names what the RECORDING resolved.
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

    /// Build a take from an explicit model of the world: the terminal echoed
    /// `echo_us` after each attempt, and the recorder looked every
    /// `interval_us`. The gap between what the terminal DID and what the block
    /// REPORTS is then exact rather than asserted.
    /// `(t_us, serialized input row)` — the shape `inputs[]` rows take here.
    type Inputs = Vec<(u64, String)>;
    /// `(t_us, seq)` — captured frame stamps.
    type Frames = Vec<(u64, u64)>;

    fn take(
        interval_us: u64,
        echo_us: u64,
        keys: &[u64],
        span_us: u64,
    ) -> (Inputs, Frames) {
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

    /// THE REGRESSION THIS MODULE EXISTS FOR. A terminal echoing in 1 ms,
    /// recorded at 30 fps, must NOT be reported as a ~19 ms terminal: the
    /// reading is the sampling grid, and the block has to say so.
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
        // The names a reader greps can no longer be mistaken for photons.
        for gone in ["\"p50_ms\"", "\"p90_ms\"", "\"max_ms\"", "\"verdict\""] {
            assert!(!block.contains(gone), "{gone} still emitted:\n{block}");
        }
        // The disclaimer travels in band, on the same line a reader greps.
        assert!(block.contains("NOT key->photon latency"), "{block}");
    }

    /// THE SECOND LIE. A terminal echoing in 120 ms, recorded at 30 fps while
    /// keys are typed ~107 ms apart, used to report `p50_ms: 30.0` with
    /// `verdict: GOOD` — because each key was credited with an earlier key's
    /// echo. The p50 reading is still ~30 ms (the instrument genuinely cannot
    /// separate overlapping echoes), so it must no longer pass as healthy: the
    /// take is declared AMBIGUOUS and the cadence that makes it so is published.
    #[test]
    fn a_slow_terminal_typed_fast_is_declared_ambiguous_not_good() {
        let keys = [
            40_000, 148_000, 255_000, 361_000, 470_000, 583_000, 690_000, 805_000, 911_000,
            1_020_000,
        ];
        let (inputs, frames) = take(33_333, 120_000, &keys, 1_400_000);
        let block = analysis_block(&inputs, &frames);
        // The misleading median survives — hiding it would be a silent filter —
        // but it is now framed by the cadence that produced it.
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
        // The one attempt that raced nothing carries the honest number.
        assert!(block.contains("\"frame_change_ms\":126.7,"), "{block}");
    }

    /// The SAME 120 ms terminal, typed slowly enough for the screen to settle
    /// between keys, reads honestly: no ambiguity, nothing at the floor, and a
    /// verdict that puts the delay in the terminal rather than the recorder.
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

    /// An attempt the recorder cannot answer for keeps its row and says why —
    /// the old block dropped pre-first-frame attempts silently, so
    /// `key_response` could be shorter than `inputs[]` with no explanation.
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

    /// The achieved cadence is MEASURED from the frame stamps, never taken
    /// from the requested `fps=` — a take that fell behind its request must
    /// publish the floor it actually had.
    #[test]
    fn the_capture_interval_is_measured_not_requested() {
        let (inputs, frames) = take(50_000, 1_000, &[40_000, 148_000, 255_000], 400_000);
        let block = analysis_block(&inputs, &frames);
        assert_eq!(field(&block, "capture_interval_p50_ms"), "50.0", "{block}");
    }
}
