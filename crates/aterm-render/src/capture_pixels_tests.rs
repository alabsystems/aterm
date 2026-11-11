// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::srgb_to_linear;

/// The ulps on each side of a threshold that are walked one by one.
const THRESHOLD_WINDOW_ULPS: u32 = 4096;

/// Where `thresholds` departs from the encode's definition near its own entries, if it does.
fn threshold_table_error(thresholds: &[f32; 255]) -> Option<String> {
    for (index, &threshold) in thresholds.iter().enumerate() {
        let byte = index as u32 + 1;
        let bits = threshold.to_bits();
        let below = f32::from_bits(bits - 1);
        if srgb8_from_linear_powf(below) != byte - 1 || srgb8_from_linear_powf(threshold) != byte {
            return Some(format!(
                "threshold {byte} at {threshold:e} is not the first f32 of its byte"
            ));
        }
        let mut previous = 0;
        for pattern in bits - THRESHOLD_WINDOW_ULPS..=bits + THRESHOLD_WINDOW_ULPS {
            let linear = f32::from_bits(pattern);
            let exact = srgb8_from_linear_powf(linear);
            if exact < previous {
                return Some(format!("the encode decreases at {linear:e}"));
            }
            let read = srgb8_by_thresholds(thresholds, linear);
            if read != exact {
                return Some(format!(
                    "the table reads {read} at {linear:e}, the encode {exact}"
                ));
            }
            previous = exact;
        }
    }
    None
}

/// `srgb8_from_linear_powf` before its floor, in both `f32` and `f64` over the same `f32` constants.
fn srgb8_unrounded(linear: f32) -> (f64, f64) {
    let clamped = linear.clamp(0.0, 1.0);
    let in_f32 = f64::from(linear_to_srgb(clamped) * 255.0 + 0.5);
    let x = f64::from(clamped);
    let encoded = if clamped <= 0.003_130_8 {
        f64::from(12.92_f32) * x
    } else {
        f64::from(1.055_f32) * x.powf(f64::from(1.0_f32 / 2.4)) - f64::from(0.055_f32)
    };
    (in_f32, encoded * 255.0 + 0.5)
}

/// The table matches the definition for every `f32` across each threshold window.
#[test]
fn encode_thresholds_are_exact_and_locally_monotone() {
    let thresholds = &*SRGB8_ENCODE_THRESHOLDS;
    assert_eq!(threshold_table_error(thresholds), None);
    assert!(
        thresholds.windows(2).all(|pair| pair[0] < pair[1]),
        "the thresholds ascend"
    );
    for (index, &threshold) in thresholds.iter().enumerate() {
        let byte = f64::from(index as u32 + 1);
        let bits = threshold.to_bits();
        let edge = |pattern: u32| srgb8_unrounded(f32::from_bits(pattern)).1;
        let margin = (byte - edge(bits - THRESHOLD_WINDOW_ULPS))
            .min(edge(bits + THRESHOLD_WINDOW_ULPS) - byte);
        let stray = (bits - THRESHOLD_WINDOW_ULPS..=bits + THRESHOLD_WINDOW_ULPS)
            .map(|pattern| {
                let (in_f32, exact) = srgb8_unrounded(f32::from_bits(pattern));
                (in_f32 - exact).abs()
            })
            .fold(0.0, f64::max);
        assert!(
            margin > 16.0 * stray,
            "byte {byte}: margin {margin:e} against an f32 stray of {stray:e}"
        );
    }
    assert_eq!(
        srgb8_from_linear(f32::NAN),
        srgb8_from_linear_powf(f32::NAN)
    );
    for linear in [f32::NEG_INFINITY, -1.0, -0.0, 0.0, 1.0, 2.0, f32::INFINITY] {
        assert_eq!(
            srgb8_from_linear(linear),
            srgb8_from_linear_powf(linear),
            "{linear}"
        );
    }

    let mut late = *thresholds;
    late[127] = f32::from_bits(late[127].to_bits() + 1);
    assert!(
        threshold_table_error(&late).is_some(),
        "negative control: an entry one ulp late is caught"
    );
    let mut early = *thresholds;
    early[0] = f32::from_bits(early[0].to_bits() - 1);
    assert!(
        threshold_table_error(&early).is_some(),
        "negative control: an entry one ulp early is caught"
    );
}

/// The decode table holds `srgb_to_linear` bit for bit and every P3 triple encodes alike by table and `powf`.
#[test]
fn display_p3_lut_matches_powf_over_every_input() {
    let decoded: [f32; 256] = std::array::from_fn(|byte| {
        let by_powf = srgb_to_linear(byte as f32 / 255.0);
        assert_eq!(
            srgb8_to_linear(byte as u8).to_bits(),
            by_powf.to_bits(),
            "decode of {byte}"
        );
        by_powf
    });
    let decoded = &decoded;
    std::thread::scope(|scope| {
        for eighth in 0..8_u8 {
            scope.spawn(move || {
                for r in eighth * 32..=eighth * 32 + 31 {
                    for g in 0..=u8::MAX {
                        for b in 0..=u8::MAX {
                            let p3 = [r, g, b];
                            let linear = display_p3_linear_to_srgb_linear(
                                p3.map(|channel| decoded[usize::from(channel)]),
                            );
                            let by_powf =
                                linear.map(|channel| srgb8_from_linear_powf(channel) as u8);
                            assert_eq!(display_p3_to_srgb8(p3), by_powf, "{p3:?}");
                        }
                    }
                }
            });
        }
    });
}

#[test]
fn f16_lut_matches_powf() {
    let agree = |linear: [f32; 3]| {
        for channel in linear {
            assert_eq!(
                srgb8_from_linear(channel),
                srgb8_from_linear_powf(channel),
                "{channel:e}"
            );
        }
    };
    for bits in 0..=u16::MAX {
        agree(tone_map_scrgb_to_sdr([f16_to_f32(bits); 3], 1.0));
    }
    let mut state = 0x5eed_u64;
    for _ in 0..1_000_000 {
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut word = state;
        word = (word ^ (word >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        word = (word ^ (word >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        word ^= word >> 31;
        let scrgb = [0, 16, 32].map(|shift| f16_to_f32((word >> shift) as u16));
        let sdr_white_scale = 1.0 + f32::from((word >> 48) as u16) / 8192.0;
        agree(tone_map_scrgb_to_sdr(scrgb, sdr_white_scale));
    }
    let inf_mapped = tone_map_scrgb_to_sdr([f32::INFINITY, 1.0, 0.0], 0.5);
    agree(inf_mapped);
    assert_eq!(inf_mapped.map(srgb8_from_linear), [255, 0, 0]);
    assert_eq!(tone_map_scrgb_to_sdr([1.0, 1.0, 1.0], 0.0), [0.0; 3]);
}
