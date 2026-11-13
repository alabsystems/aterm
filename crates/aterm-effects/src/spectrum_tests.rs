// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// Arc samples for "holds everywhere" checks: about eight per LUT interval.
const WALK: usize = 4096;

/// The committed tables as the generator writes them, ready to paste.
fn regenerated_tables() -> String {
    let table = generate_spectrum_table();
    let (lut, anchor_at) = (table.lut, table.anchor_at);
    let mut out =
        format!("pub const SPECTRUM_ANCHOR_AT: [usize; SPECTRUM_STOPS] = {anchor_at:?};\n");
    for row in lut.chunks(6) {
        let cells: Vec<String> = row
            .iter()
            .map(|c| format!("0x{:04X}_{:04X},", c >> 16, c & 0xffff))
            .collect();
        out.push_str(&format!("    {}\n", cells.join(" ")));
    }
    out
}

/// **THE COMMITTED TABLE IS THE LAW'S OWN OUTPUT.** Byte-for-byte, no
/// tolerance: the moment an anchor or interpolation rule moves, this fails and
/// the table has to be regenerated rather than nudged — the failure prints the
/// regenerated tables to paste.
#[test]
fn spectrum_lut_is_byte_reproducible_from_its_generator() {
    let fresh = generate_spectrum_lut();
    assert!(
        fresh == SPECTRUM_LUT && generate_spectrum_table().anchor_at == SPECTRUM_ANCHOR_AT,
        "SPECTRUM_LUT / SPECTRUM_ANCHOR_AT drifted from the generator; paste:\n{}",
        regenerated_tables()
    );
}

/// The anchors are stored bit for bit at their generated indices.
#[test]
fn spectrum_reproduces_its_anchors_exactly() {
    let anchor_at = generate_spectrum_table().anchor_at;
    assert_eq!(
        anchor_at, SPECTRUM_ANCHOR_AT,
        "SPECTRUM_ANCHOR_AT drifted from the pace that places the anchors"
    );
    assert_eq!(SPECTRUM_ANCHOR_AT[0], 0, "red is the arc's own start");
    assert_eq!(
        SPECTRUM_ANCHOR_AT[SPECTRUM_STOPS - 1],
        SPECTRUM_LUT_LEN - 1,
        "violet is the arc's own end"
    );
    assert!(
        SPECTRUM_ANCHOR_AT.windows(2).all(|w| w[0] < w[1]),
        "the seven names must land in ROYGBIV order: {SPECTRUM_ANCHOR_AT:?}"
    );
    for (i, &anchor) in SPECTRUM_ANCHORS.iter().enumerate() {
        assert_eq!(
            SPECTRUM_LUT[SPECTRUM_ANCHOR_AT[i]], anchor,
            "table index {} is not anchor {i}",
            SPECTRUM_ANCHOR_AT[i]
        );
        assert_eq!(spectrum_stop(i), anchor, "stop {i}");
        assert_eq!(
            spectrum(spectrum_stop_position(i)),
            anchor,
            "the open arc misses anchor {i} at its own position"
        );
    }
}

/// HSV hue climbs from red to violet and never reverses (a reversal shows a
/// colour twice on one mark).
#[test]
fn spectrum_hue_climbs_from_red_to_violet_without_reversing() {
    let mut prev = f64::NEG_INFINITY;
    let mut worst_drop = 0.0f64;
    for (i, &entry) in SPECTRUM_LUT.iter().enumerate() {
        let hue = spectrum_hsv(entry).0;
        let chan = |sh: u32| f64::from((entry >> sh) & 0xff);
        let spread = chan(16).max(chan(8)).max(chan(0)) - chan(16).min(chan(8)).min(chan(0));
        let rotation = if spread > 0.0 { 60.0 / spread } else { 360.0 };
        if hue < prev {
            worst_drop = worst_drop.max(prev - hue);
        }
        assert!(
            hue >= prev - rotation,
            "hue reverses at entry {i}: {prev:.3}° -> {hue:.3}° \
             (byte rotation here is {rotation:.3}°)"
        );
        prev = hue;
    }
    assert!(
        worst_drop <= 90.0 / 255.0,
        "the table's worst hue reversal is {worst_drop:.3}°, past the \
         {:.3}° a rounded byte can account for",
        90.0 / 255.0
    );
    for i in 1..SPECTRUM_STOPS {
        assert!(
            spectrum_hsv(spectrum_stop(i)).0 > spectrum_hsv(spectrum_stop(i - 1)).0,
            "anchor {i} does not climb"
        );
    }
    assert_eq!(spectrum_hsv(SPECTRUM_LUT[0]).0, 0.0, "red is hue 0");
    assert_eq!(
        SPECTRUM_LUT[0], SPECTRUM_ANCHORS[0],
        "the table starts on the authored red"
    );
    assert_eq!(
        SPECTRUM_LUT[SPECTRUM_LUT_LEN - 1],
        SPECTRUM_ANCHORS[SPECTRUM_STOPS - 1],
        "the table ends on the authored violet"
    );
}

/// The arc is vivid.
#[test]
fn spectrum_stays_saturated_across_the_whole_arc() {
    let mut min_sat = 1.0f64;
    let mut sum_sat = 0.0f64;
    let mut min_val = 1.0f64;
    for &rgb in SPECTRUM_LUT.iter() {
        let (_, s, v) = spectrum_hsv(rgb);
        min_sat = min_sat.min(s);
        sum_sat += s;
        min_val = min_val.min(v);
    }
    let mean_sat = sum_sat / SPECTRUM_LUT_LEN as f64;

    for (i, &rgb) in SPECTRUM_LUT.iter().enumerate() {
        let (_, s, _) = spectrum_hsv(rgb);
        assert!(
            s >= 0.999,
            "entry {i} desaturates to S {s:.4} — the palette lost its \
             shared zero channel, or a chroma taper came back"
        );
    }
    assert!(min_sat >= 0.999, "the arc's floor fell to S {min_sat:.4}");
    assert!(mean_sat >= 0.999, "mean saturation is only {mean_sat:.4}");

    let anchor_min_v = SPECTRUM_ANCHORS
        .iter()
        .map(|&c| spectrum_hsv(c).2)
        .fold(f64::MAX, f64::min);
    assert!(
        (min_val - anchor_min_v).abs() < 1e-9,
        "the arc's darkest entry is V {min_val:.4}, but the darkest AUTHORED \
         anchor is V {anchor_min_v:.4} — the arc is darker between its stops \
         than at any of them"
    );

    let chroma = |c: u32| {
        let (r, g, b) = ((c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff);
        r.max(g).max(b) - r.min(g).min(b)
    };
    let min_chroma = SPECTRUM_LUT.iter().copied().map(chroma).min().unwrap();
    assert!(
        min_chroma >= 24,
        "a neutral hole reopened in the table: min chroma {min_chroma}"
    );
}

/// Adjacent entries in [`SPECTRUM_LUT`] stay close enough that the LUT prints
/// no visible staircase and [`spectrum`] stays continuous between them.
#[test]
fn spectrum_lut_has_bounded_adjacent_steps() {
    const CEILING: i32 = 5;
    const {
        let mut i = 0;
        while i < SPECTRUM_LUT_LEN - 1 {
            let mut shift = 0u32;
            while shift <= 16 {
                let a = ((SPECTRUM_LUT[i] >> shift) & 0xff) as i32;
                let b = ((SPECTRUM_LUT[i + 1] >> shift) & 0xff) as i32;
                assert!(
                    (a - b).abs() <= CEILING,
                    "SPECTRUM_LUT adjacent step exceeds CEILING"
                );
                shift += 8;
            }
            i += 1;
        }
    }
    let mut worst = 0i32;
    for i in 0..SPECTRUM_LUT_LEN - 1 {
        let step = [16u32, 8, 0]
            .into_iter()
            .map(|shift| {
                let a = ((SPECTRUM_LUT[i] >> shift) & 0xff) as i32;
                let b = ((SPECTRUM_LUT[i + 1] >> shift) & 0xff) as i32;
                (a - b).abs()
            })
            .max()
            .unwrap_or(0);
        worst = worst.max(step);
    }
    assert!(worst > 0, "a table with no structure would pass vacuously");
    for i in 0..=WALK {
        let t = i as f32 / WALK as f32;
        let a = spectrum(t);
        let b = spectrum((t + 0.5 / (SPECTRUM_LUT_LEN - 1) as f32).min(1.0));
        for shift in [16u32, 8, 0] {
            let d = ((a >> shift) & 0xff).abs_diff((b >> shift) & 0xff) as i32;
            assert!(d <= worst, "the read steps {d} across half a table entry");
        }
    }
}

/// Point-marks snap to a name (§2.3.3): [`spectrum_snap`] resolves to one of
/// exactly seven colours.
#[test]
fn spectrum_snap_resolves_to_one_of_the_seven_names() {
    let named: Vec<u32> = (0..SPECTRUM_STOPS).map(spectrum_stop).collect();
    assert_eq!(named, SPECTRUM_ANCHORS.to_vec());
    for i in 0..=WALK {
        let snapped = spectrum_snap(i as f32 / WALK as f32);
        assert!(
            named.contains(&snapped),
            "snap produced #{snapped:06X}, which is not a named stop"
        );
    }
    for (i, &stop) in named.iter().enumerate() {
        assert_eq!(
            spectrum_snap(spectrum_stop_position(i)),
            stop,
            "a stop does not snap to itself"
        );
        assert!(
            (0..=WALK).any(|k| spectrum_snap(k as f32 / WALK as f32) == stop),
            "stop {i} is unreachable by snapping"
        );
    }
}

/// Both ends clamp, NaN included: nothing walks violet back into red.
#[test]
fn spectrum_endpoints_are_the_named_red_and_violet() {
    assert_eq!(spectrum(0.0), SPECTRUM_ANCHORS[0]);
    assert_eq!(spectrum(1.0), SPECTRUM_ANCHORS[SPECTRUM_STOPS - 1]);
    for t in [-1.0f32, -0.001, f32::NEG_INFINITY] {
        assert_eq!(spectrum(t), SPECTRUM_ANCHORS[0], "below the arc at {t}");
    }
    for t in [1.001f32, 4.0, f32::INFINITY] {
        assert_eq!(
            spectrum(t),
            SPECTRUM_ANCHORS[SPECTRUM_STOPS - 1],
            "above the arc at {t}"
        );
    }
    assert_eq!(spectrum(f32::NAN), SPECTRUM_ANCHORS[0], "NaN draws red");
    assert!(spectrum_max_byte_rate() > 0.0);
}
