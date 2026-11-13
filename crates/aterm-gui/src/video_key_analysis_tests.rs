// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
        40_000, 148_000, 255_000, 361_000, 470_000, 583_000, 690_000, 805_000, 911_000, 1_020_000,
    ];
    let (inputs, frames) = take(33_333, 120_000, &keys, 1_400_000);
    let block = analysis_block(&inputs, &frames);
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
        assert_eq!(legacy_fp(changed), legacy_fp(&base), "{what}");
    }
}

/// `delta` counts whole tiles: the cut right and bottom tiles count like
/// whole ones, a resize changes every tile of the new frame, and the first
/// frame has nothing to differ from.
#[test]
fn delta_counts_cut_tiles_and_a_resize_changes_every_tile() {
    let paint = |x: u32, y: u32| [x as u8, y as u8, 0, 255];
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
