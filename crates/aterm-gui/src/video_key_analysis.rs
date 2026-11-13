// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a `video` take's `index.json` says about its frames.

use aterm_gpu::video_tap::CapturedFrame;

/// The side of a `delta` tile, in pixels.
const TILE: usize = 8;

/// `fp` keeps the low 53 bits of the hash, so every value is an integer a JSON
/// reader that parses numbers as doubles (jq, JavaScript) holds exactly.
const FP_MASK: u64 = (1 << 53) - 1;

/// A frame's `fp`: FxHash of its width, height and every RGBA byte, cut to 53 bits.
pub(crate) fn frame_fingerprint(frame: &CapturedFrame) -> u64 {
    use std::hash::Hasher as _;

    let mut hash = aterm_hash::FxHasher::default();
    hash.write_u32(frame.w);
    hash.write_u32(frame.h);
    hash.write(&frame.rgba);
    hash.finish() & FP_MASK
}

/// A frame's `delta`.
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
    /// differs from the frame on screen at the attempt.
    frame_change_us: Option<u64>,
    /// Delay from the attempt to the recorder's next look: the smallest value
    /// `frame_change_us` could take.
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
/// sampling interval, not the requested `fps=`.
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
/// for fewer than two attempts.
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
fn correlate(inputs: &[(u64, String)], frames: &[(u64, u64)]) -> Vec<Correlated> {
    debug_assert!(
        frames.windows(2).all(|w| w[0].0 <= w[1].0),
        "captured frames are stamped in capture order"
    );
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
        let first = frames.partition_point(|&(ft, _)| ft < t);
        let Some(reference) = first.checked_sub(1) else {
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
pub(crate) fn analysis_block(inputs: &[(u64, String)], frames: &[(u64, u64)]) -> String {
    let rows = correlate(inputs, frames);
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
        .filter(|r| r.frame_change_us.is_some())
        .filter_map(|r| r.capture_floor_us.map(us_to_ms))
        .collect();
    floors.sort_by(f64::total_cmp);
    let n = changes.len();
    let p50 = pct(&changes, 1, 2);
    let p90 = pct(&changes, 9, 10);
    let max = changes[n - 1];
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
    let attempt_gap_ms = attempt_gap_p50_us(inputs).map(us_to_ms);
    let gap_field = attempt_gap_ms.map_or_else(|| "null".to_string(), |ms| format!("{ms:.1}"));
    let outpaced = attempt_gap_ms.is_some_and(|gap| gap <= max);

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
#[path = "video_key_analysis_tests.rs"]
mod tests;
