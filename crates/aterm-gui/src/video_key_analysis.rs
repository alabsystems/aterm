// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a `video` take's `index.json` says about its frames: each frame's `fp` and
//! `delta`, and the `video … keys` self-evaluation that correlates pre-routing input
//! attempts against them.
//!
//! `fp` ([`frame_fingerprint`]) is a 53-bit hash of a frame's whole content, so two
//! frames of one take whose pixels differ anywhere have different `fp` but for a 2⁻⁵³
//! collision. `delta` ([`changed_tiles`]) is how many 8×8 tiles differ from the
//! previous captured frame: 0 for the first frame, every tile of the new frame across
//! a resize.
//!
//! The self-evaluation is a sampling instrument: a key's effect is seen at the next
//! captured frame, so every reading is bounded below by the recorder's sampling grid,
//! published beside it (`capture_floor_ms`). Three limits are disclosed in band; none
//! filters anything (every correlated attempt stays in `n` and the percentiles):
//!
//! 1. **The capture floor.** At `N` fps an attempt waits up to `1000/N` ms for the next
//!    look, so a 1 ms and a 15 ms echo read the same at 30 fps. Field names say
//!    `frame_change` or `capture_`, never latency; `at_capture_floor_n` counts readings that
//!    are a bound rather than a measurement.
//! 2. **Cross-attribution during sustained typing.** The observed change is the first
//!    content change after the attempt, often the previous key's echo, so readings come
//!    out faster than reality. A content hash cannot say which key changed it, but the
//!    cadence is observable: when `attempt_gap_p50_ms` is no slower than
//!    `frame_change_max_ms`, `attempts_outpace_readings` flags the take as ambiguous.
//! 3. **Any change counts.** The change is any pixel that differs from the frame on
//!    screen at the attempt, so a cursor blink, a spinner or a clock that changes before
//!    the echo is read as the response.
//!
//! Key→photon latency is out of reach here. The nearest instrument is `ctl metrics
//! percentiles` (`input_p50/p95/p99_ms`), sampled per keystroke, with its bounds documented
//! on `crate::control::control_query::cmd_metrics`.

use aterm_gpu::video_tap::CapturedFrame;

/// The side of a `delta` tile, in pixels.
const TILE: usize = 8;

/// `fp` keeps the low 53 bits of the hash, so every value is an integer a JSON
/// reader that parses numbers as doubles (jq, JavaScript) holds exactly.
const FP_MASK: u64 = (1 << 53) - 1;

/// A frame's `fp`: FxHash of its width, height and every RGBA byte, cut to 53 bits.
/// It is compared only within one take, which one process wrote.
pub(crate) fn frame_fingerprint(frame: &CapturedFrame) -> u64 {
    use std::hash::Hasher as _;

    let mut hash = aterm_hash::FxHasher::default();
    hash.write_u32(frame.w);
    hash.write_u32(frame.h);
    hash.write(&frame.rgba);
    hash.finish() & FP_MASK
}

/// A frame's `delta`: the number of 8×8 tiles (the right and bottom ones cut to the
/// frame) whose bytes differ from `prev`, the previous captured frame. 0 without a
/// previous frame; every tile of `frame` when the dimensions changed.
///
/// Rows are compared in memory order, a whole row first, so an unchanged row costs
/// one `memcmp`; a changed row compares only the tiles of its band not yet known to
/// differ, and a band's remaining rows are skipped once every tile in it differs.
pub(crate) fn changed_tiles(prev: Option<&CapturedFrame>, frame: &CapturedFrame) -> u64 {
    let Some(prev) = prev else {
        return 0;
    };
    let (w, h) = (frame.w as usize, frame.h as usize);
    let columns = w.div_ceil(TILE);
    let stride = w * 4;
    let comparable = prev.w == frame.w
        && prev.h == frame.h
        && frame.rgba.len() == stride * h
        && prev.rgba.len() == frame.rgba.len();
    if !comparable {
        return (columns * h.div_ceil(TILE)) as u64;
    }
    if stride == 0 {
        return 0;
    }
    let mut changed = vec![false; columns];
    let mut total = 0;
    for (band_prev, band) in prev
        .rgba
        .chunks(stride * TILE)
        .zip(frame.rgba.chunks(stride * TILE))
    {
        changed.fill(false);
        let mut found = 0;
        for (row_prev, row) in band_prev.chunks(stride).zip(band.chunks(stride)) {
            if row_prev == row {
                continue;
            }
            let tiles = row_prev.chunks(TILE * 4).zip(row.chunks(TILE * 4));
            for (seen, (tile_prev, tile)) in changed.iter_mut().zip(tiles) {
                if !*seen && tile_prev != tile {
                    *seen = true;
                    found += 1;
                }
            }
            if found == columns {
                break;
            }
        }
        total += found as u64;
    }
    total
}

/// One correlated attempt, before it is rendered to JSON.
#[derive(Debug, PartialEq, Eq)]
struct Correlated {
    /// Attempt stamp, `metrics::now_us` epoch.
    t_us: u64,
    /// Delay from the attempt to the first captured frame whose content
    /// differs from the frame on screen at the attempt. `None` when no captured
    /// frame after it differs.
    frame_change_us: Option<u64>,
    /// Delay from the attempt to the recorder's next look: the smallest value
    /// `frame_change_us` could take. `None` when no frame was captured at or
    /// after the attempt.
    capture_floor_us: Option<u64>,
    /// Why the row is null, when it is.
    unobserved: Option<&'static str>,
}

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

/// Median gap between attempts adjacent in time (the typing cadence). `None`
/// for fewer than two attempts. The ledger is not in time order (a hardware
/// key is backdated to its arrival), so the stamps are sorted first. Compared
/// against the readings to detect cross-attribution (module docs, limit 2).
fn attempt_gap_p50_us(inputs: &[(u64, String)]) -> Option<u64> {
    let mut stamps: Vec<u64> = inputs.iter().map(|(t, _)| *t).collect();
    stamps.sort_unstable();
    let mut gaps: Vec<u64> = stamps
        .windows(2)
        .map(|w| w[1].saturating_sub(w[0]))
        .collect();
    if gaps.is_empty() {
        return None;
    }
    gaps.sort_unstable();
    Some(gaps[gaps.len() / 2])
}

/// Correlate every attempt against the captured frame series.
///
/// `frames` is `(t_us, fingerprint)` in capture order, which is stamp order;
/// `inputs` is `(t_us, …)` in ledger order, which is not (a hardware key is
/// backdated to its arrival), and row `i` answers `inputs[i]`. Each attempt
/// finds its frames by binary search, and the first change after its first
/// frame is a lookup in `next_change`, so the whole correlation is
/// O(frames + inputs · log frames).
fn correlate(inputs: &[(u64, String)], frames: &[(u64, u64)]) -> Vec<Correlated> {
    debug_assert!(
        frames.windows(2).all(|w| w[0].0 <= w[1].0),
        "captured frames are stamped in capture order"
    );
    // `next_change[j]`: the first frame after `j` whose fingerprint differs
    // from frame `j`'s.
    let mut next_change = vec![None; frames.len()];
    for j in (0..frames.len().saturating_sub(1)).rev() {
        next_change[j] = if frames[j + 1].1 == frames[j].1 {
            next_change[j + 1]
        } else {
            Some(j + 1)
        };
    }
    let mut out = Vec::with_capacity(inputs.len());
    for &(t, _) in inputs {
        // The first frame captured at or after the attempt; the one before it
        // is the reference, what was on screen when the key was pressed.
        let first = frames.partition_point(|&(ft, _)| ft < t);
        let Some(reference) = first.checked_sub(1) else {
            // The attempt predates the first captured frame: keep its row so
            // `key_response` stays as long as `inputs[]`.
            out.push(Correlated {
                t_us: t,
                frame_change_us: None,
                capture_floor_us: None,
                unobserved: Some("no captured frame precedes this attempt"),
            });
            continue;
        };
        let Some(&(first_after_us, first_after_fp)) = frames.get(first) else {
            out.push(Correlated {
                t_us: t,
                frame_change_us: None,
                capture_floor_us: None,
                unobserved: Some("no captured frame follows this attempt"),
            });
            continue;
        };
        let ref_fp = frames[reference].1;
        // The first frame is the change, or else it shows the reference's
        // content and the first frame differing from it is its next change.
        let hit = if first_after_fp == ref_fp {
            next_change[first]
        } else {
            Some(first)
        };
        let capture_floor_us = Some(first_after_us.saturating_sub(t));
        out.push(match hit {
            Some(j) => Correlated {
                t_us: t,
                frame_change_us: Some(frames[j].0.saturating_sub(t)),
                capture_floor_us,
                unobserved: None,
            },
            None => Correlated {
                t_us: t,
                frame_change_us: None,
                capture_floor_us,
                unobserved: Some("no captured frame after this attempt changed"),
            },
        });
    }
    out
}

/// The sentence that travels with every reading, in band, so grepping a single
/// line still gets it.
const SEMANTICS: &str = "frame_change_ms is the delay from a pre-routing input ATTEMPT to the first \
     frame THIS RECORDING CAPTURED whose content hash differs from the frame on screen at the \
     attempt. Any pixel change counts, a cursor blink included, so a blink before the echo is read \
     as the response. It is the recorder's observation, NOT \
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

    /// The typing cadence is measured between attempts adjacent in time,
    /// whatever order the ledger holds them in.
    #[test]
    fn the_typing_cadence_is_measured_in_time_order() {
        let ledger: Inputs = [300_000u64, 0, 200_000, 100_000]
            .iter()
            .map(|t| (*t, String::new()))
            .collect();
        assert_eq!(attempt_gap_p50_us(&ledger), Some(100_000));
        // Negative control: gaps taken in ledger order read this cadence as 0.
        let mut naive: Vec<u64> = ledger
            .windows(2)
            .map(|w| w[1].0.saturating_sub(w[0].0))
            .collect();
        naive.sort_unstable();
        assert_eq!(naive[naive.len() / 2], 0);
    }

    /// The quadratic correlation the binary search replaced: per attempt, scan
    /// every frame for the reference, the first frame and the first change.
    fn reference_correlate(inputs: &[(u64, String)], frames: &[(u64, u64)]) -> Vec<Correlated> {
        let row = |t_us, frame_change_us, capture_floor_us, unobserved| Correlated {
            t_us,
            frame_change_us,
            capture_floor_us,
            unobserved,
        };
        inputs
            .iter()
            .map(|&(t, _)| {
                let Some(&(_, ref_fp)) = frames.iter().rev().find(|(ft, _)| *ft < t) else {
                    return row(
                        t,
                        None,
                        None,
                        Some("no captured frame precedes this attempt"),
                    );
                };
                let mut after = frames.iter().filter(|(ft, _)| *ft >= t);
                let Some(&(first_us, _)) = after.clone().next() else {
                    return row(
                        t,
                        None,
                        None,
                        Some("no captured frame follows this attempt"),
                    );
                };
                match after.find(|(_, fp)| *fp != ref_fp) {
                    Some(&(ft, _)) => row(t, Some(ft - t), Some(first_us - t), None),
                    None => row(
                        t,
                        None,
                        Some(first_us - t),
                        Some("no captured frame after this attempt changed"),
                    ),
                }
            })
            .collect()
    }

    /// Seeded takes with repeated fingerprints, equal stamps, attempts before
    /// the first frame and after the last, and ledgers out of time order: the
    /// binary-search correlation gives the quadratic reference's rows, in
    /// ledger order.
    #[test]
    fn correlate_matches_the_reference_on_shuffled_inputs() {
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        let mut next = move |bound: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % bound
        };
        let mut out_of_order = 0;
        for _ in 0..200 {
            let mut ft = next(50);
            let frames: Frames = (0..next(40))
                .map(|_| {
                    ft += next(3) * 10;
                    (ft, next(3))
                })
                .collect();
            let inputs: Inputs = (0..next(30))
                .map(|_| (next(ft + 100), String::new()))
                .collect();
            out_of_order += usize::from(inputs.windows(2).any(|w| w[0].0 > w[1].0));
            assert_eq!(
                correlate(&inputs, &frames),
                reference_correlate(&inputs, &frames),
                "inputs {inputs:?}\nframes {frames:?}"
            );
        }
        // Negative control: most ledgers really were out of time order.
        assert!(out_of_order > 100, "{out_of_order}");
    }

    /// A `w`×`h` frame whose pixel at `(x, y)` is `paint(x, y)`.
    fn frame(w: u32, h: u32, paint: impl Fn(u32, u32) -> [u8; 4]) -> CapturedFrame {
        let mut rgba = Vec::with_capacity(w as usize * h as usize * 4);
        for y in 0..h {
            for x in 0..w {
                rgba.extend_from_slice(&paint(x, y));
            }
        }
        CapturedFrame {
            seq: 0,
            t_us: 0,
            w,
            h,
            rgba,
        }
    }

    /// The strided byte sum `fp` was before it hashed the whole frame, kept to
    /// show what each fixture below would have hidden.
    fn legacy_fp(frame: &CapturedFrame) -> u64 {
        frame.rgba.iter().step_by(64).map(|&b| u64::from(b)).sum()
    }

    /// Every channel of every pixel counts, wherever it is: a green-only and a
    /// blue-only change, a pixel off the 16-pixel grid and a one-row scroll each
    /// change `fp` and `delta`, while identical frames keep both.
    #[test]
    fn fingerprint_sees_every_channel_column_and_scroll() {
        const W: u32 = 32;
        const H: u32 = 16;
        let paint = |x: u32, y: u32| {
            [
                (x * 7 + y * 13) as u8,
                (x * 3 + y * 5) as u8,
                (x * 11 + y * 17) as u8,
                255,
            ]
        };
        let base = frame(W, H, paint);
        let same = frame(W, H, paint);
        assert_eq!(frame_fingerprint(&same), frame_fingerprint(&base));
        assert_eq!(changed_tiles(Some(&base), &same), 0);
        let touched = |at: (u32, u32), edit: fn([u8; 4]) -> [u8; 4]| {
            frame(W, H, move |x, y| {
                let pixel = paint(x, y);
                if (x, y) == at { edit(pixel) } else { pixel }
            })
        };
        let cases = [
            (
                "green only",
                touched((16, 3), |[r, g, b, a]| [r, g ^ 1, b, a]),
                1,
            ),
            (
                "blue only",
                touched((0, 9), |[r, g, b, a]| [r, g, b ^ 0x80, a]),
                1,
            ),
            (
                "column 5",
                touched((5, 0), |[r, g, b, a]| [r ^ 1, g, b, a]),
                1,
            ),
            ("scroll", frame(W, H, |x, y| paint(x, (y + 1) % H)), 8),
        ];
        for (what, changed, tiles) in &cases {
            assert_ne!(
                frame_fingerprint(changed),
                frame_fingerprint(&base),
                "{what}"
            );
            assert!(frame_fingerprint(changed) <= FP_MASK, "{what}");
            assert_eq!(changed_tiles(Some(&base), changed), *tiles, "{what}");
            // Negative control: the strided sum reads this change as none.
            assert_eq!(legacy_fp(changed), legacy_fp(&base), "{what}");
        }
    }

    /// `delta` counts whole tiles: the cut right and bottom tiles count like
    /// whole ones, a resize changes every tile of the new frame, and the first
    /// frame has nothing to differ from.
    #[test]
    fn delta_counts_cut_tiles_and_a_resize_changes_every_tile() {
        let paint = |x: u32, y: u32| [x as u8, y as u8, 0, 255];
        // 3 × 2 tiles; the last column and the last band are cut.
        let small = frame(20, 12, paint);
        assert_eq!(changed_tiles(None, &small), 0);
        let corner = frame(20, 12, |x, y| {
            if (x, y) == (19, 11) {
                [0; 4]
            } else {
                paint(x, y)
            }
        });
        assert_eq!(changed_tiles(Some(&small), &corner), 1);
        assert_eq!(changed_tiles(Some(&small), &frame(40, 12, paint)), 10);
    }
}
