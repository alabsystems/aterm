// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{contrast_ratio, linear_luminance};

// The complete color-adjustment implementation before extracting its target.
#[inline]
fn original_floor(fg: u32, bg: u32, min: f32) -> u32 {
    if contrast_ratio(fg, bg) >= min {
        return fg;
    }
    let l = linear_luminance(bg);
    let target: u32 = if (l + 0.05) * (l + 0.05) >= 0.05 * 1.05 {
        0x0000_0000
    } else {
        0x00ff_ffff
    };
    let mix = |fg: u32, shift: u32, t: f32| -> u32 {
        let f = ((fg >> shift) & 0xff) as f32;
        let g = ((target >> shift) & 0xff) as f32;
        ((f + (g - f) * t).round() as u32) & 0xff
    };
    let mut step = 1u32;
    while step <= 10 {
        let t = step as f32 / 10.0;
        let cand = (mix(fg, 16, t) << 16) | (mix(fg, 8, t) << 8) | mix(fg, 0, t);
        if contrast_ratio(cand, bg) >= min {
            return cand;
        }
        step += 1;
    }
    target
}

#[test]
fn boolean_and_refactored_floor_match_the_original() {
    let edges = [
        0, 1, 0x101013, 0x141414, 0x777777, 0x808080, 0xfefefe, 0xffffff, 0xff000000, 0xffffffff,
    ];
    let mut pairs: Vec<(u32, u32)> = edges
        .iter()
        .flat_map(|&fg| edges.iter().map(move |&bg| (fg, bg)))
        .collect();
    let mut state = 0x9e37_79b9u32;
    for i in 0..8192 {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        let fg = state;
        let bg = match i % 4 {
            0 => fg,
            1 => 0,
            2 => 0xffffff,
            _ => state.rotate_left(11),
        };
        pairs.push((fg, bg));
    }
    let thresholds: Vec<f32> = (0..=60)
        .map(|n| n as f32 / 2.0)
        .chain([-1.0, 1.01, f32::NEG_INFINITY, f32::INFINITY, f32::NAN])
        .collect();
    for (fg, bg) in pairs {
        for &min in &thresholds {
            let expected = original_floor(fg, bg, min);
            assert_eq!(
                super::floor_fg_contrast(fg, bg, min),
                expected,
                "color fg={fg:x} bg={bg:x} min={min}"
            );
            assert_eq!(
                super::fg_contrast_would_change(fg, bg, min),
                expected != fg,
                "boolean fg={fg:x} bg={bg:x} min={min}"
            );
        }
    }
}

#[test]
fn unattainable_and_nan_floors_are_not_just_a_ratio_comparison() {
    assert!(!super::fg_contrast_would_change(0xffffff, 0, f32::INFINITY));
    assert!(!super::fg_contrast_would_change(0, 0xffffff, 30.0));
    assert!(super::fg_contrast_would_change(0x777777, 0, f32::NAN));
    assert!(super::fg_contrast_would_change(
        0xffffffff,
        0,
        f32::INFINITY
    ));
}

#[cfg(target_os = "macos")]
fn instructions() -> Option<u64> {
    let mut info = std::mem::MaybeUninit::<libc::rusage_info_v4>::uninit();
    // SAFETY: the output has the size/alignment of RUSAGE_INFO_V4; read only on success.
    let rc = unsafe {
        libc::proc_pid_rusage(
            std::process::id() as i32,
            libc::RUSAGE_INFO_V4,
            info.as_mut_ptr().cast(),
        )
    };
    // SAFETY: a successful proc_pid_rusage initialized the output.
    (rc == 0).then(|| unsafe { info.assume_init() }.ri_instructions)
}
#[cfg(not(target_os = "macos"))]
fn instructions() -> Option<u64> {
    None
}

fn original_change(fg: u32, bg: u32, min: f32) -> bool {
    original_floor(fg, bg, min) != fg
}

#[test]
#[ignore = "manual paired contrast boolean instruction measurement"]
fn contrast_boolean_cost() {
    use std::hint::black_box;
    type Probe = fn(u32, u32, f32) -> bool;
    for (name, fg, bg, min) in [
        ("adequate", 0xc0c0c0, 0, 3.0),
        ("faint", 0x141414, 0x101013, 3.0),
        ("equal", 0x101013, 0x101013, 3.0),
        ("optimal-unattainable", 0xffffff, 0, 30.0),
        ("unattainable", 0x777777, 0, 30.0),
        ("nan", 0x777777, 0, f32::NAN),
        ("infinite", 0x777777, 0, f32::INFINITY),
        ("packed-high", 0xffffffff, 0, 30.0),
        ("light-bg", 0xe0e0e0, 0xffffff, 4.5),
        ("zero-floor", 0x777777, 0x777777, 0.0),
    ] {
        assert_eq!(
            original_change(fg, bg, min),
            super::fg_contrast_would_change(fg, bg, min)
        );
        let mut probes: [(&str, Probe); 2] = [
            ("reference", original_change),
            ("candidate", super::fg_contrast_would_change),
        ];
        for (_, probe) in probes {
            for _ in 0..100 {
                black_box(probe(fg, bg, min));
            }
        }
        for sample in 0..5 {
            if sample != 0 {
                probes.swap(0, 1);
            }
            for (variant, probe) in probes {
                let before = instructions();
                for _ in 0..20_000 {
                    black_box(probe(black_box(fg), black_box(bg), black_box(min)));
                }
                let count = before.zip(instructions()).map(|(a, b)| b.saturating_sub(a));
                println!(
                    "contrast-boolean case={name} variant={variant} sample={sample} iterations=20000 instructions={count:?}"
                );
            }
        }
    }
}
