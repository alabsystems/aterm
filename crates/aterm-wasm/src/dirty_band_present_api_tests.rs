// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

use super::*;

fn term() -> AtermTerminal {
    crate::system_font_terminal(8, 40, 14.0)
}

/// What a from-scratch full expansion of the current frame produces.
fn full_expansion(t: &AtermTerminal) -> Vec<u8> {
    let mut rgba = Vec::new();
    let mut bands = Vec::new();
    expand_full(
        t.win.frame_pixels(),
        &mut rgba,
        &mut bands,
        t.width,
        t.height,
    );
    rgba
}

#[test]
fn band_merge_maps_rows_to_pixel_runs() {
    let mut out = Vec::new();
    dirty_rows_to_bands(&[false, true, true, false, true], 3, 10, 80, 100, &mut out);
    assert_eq!(out, vec![0, 13, 80, 20, 0, 43, 80, 10]);
    dirty_rows_to_bands(&[true], 95, 10, 80, 100, &mut out);
    assert_eq!(out, vec![0, 95, 80, 5]);
    dirty_rows_to_bands(&[true], 100, 10, 80, 100, &mut out);
    assert!(out.is_empty(), "fully out-of-frame band drops");
    dirty_rows_to_bands(&[false, true], usize::MAX, usize::MAX, 80, 100, &mut out);
    assert!(out.is_empty());
    let mut short = [1u8, 2, 3, 4];
    expand_rgba_band(&[0x00112233, 0x00445566], &mut short, 0, 2);
    assert_eq!(short, [0x11, 0x22, 0x33, 0xff]);
    shift_rgba_band(&mut short, 1, 0, 2, 1);
    assert_eq!(short, [0x11, 0x22, 0x33, 0xff]);
}

#[test]
fn first_frame_is_one_full_band_and_gate_hit_is_zero() {
    let mut t = term();
    t.process(b"hello band world\r\n");
    t.render().expect("renderable frame");
    assert_eq!(
        t.present_bands,
        vec![0, 0, t.width as i32, t.height as i32],
        "first frame presents one full band"
    );
    let full_rgba = t.rgba.clone();
    t.render().expect("renderable frame");
    assert_eq!(t.present_band_count(), 0, "no change → zero bands");
    assert_eq!(t.rgba, full_rgba, "gate-hit leaves the buffer intact");
}

#[test]
fn keystroke_damage_exports_a_sub_frame_band_and_stays_byte_exact() {
    let mut t = term();
    t.process(b"line one\r\nline two\r\nline three");
    t.render().expect("renderable frame");
    t.process_str("x"); // one-row edit on the cursor row
    t.render().expect("renderable frame");
    assert!(
        t.present_band_count() >= 1,
        "damage must surface as at least one band"
    );
    let banded_px: i32 = t
        .present_bands
        .as_chunks::<4>()
        .0
        .iter()
        .map(|q| q[2] * q[3])
        .sum();
    assert!(
        (banded_px as usize) < t.width * t.height,
        "a keystroke never pays a full-frame band"
    );
    assert_eq!(t.rgba, full_expansion(&t), "partial update is byte-exact");
}

#[test]
fn scroll_and_resize_take_the_full_band_path() {
    let mut t = term();
    for i in 0..50 {
        t.process(format!("scroll line {i}\r\n").as_bytes());
    }
    t.render().expect("renderable frame");
    t.scroll_lines(3);
    t.render().expect("renderable frame");
    assert_eq!(t.present_bands[..2], [0, 0]);
    assert_eq!(t.present_bands[2..4], [t.width as i32, t.height as i32]);
    assert_eq!(t.rgba, full_expansion(&t));
    t.resize(10, 44).expect("admitted geometry");
    t.render().expect("renderable frame");
    assert_eq!(t.present_band_count(), 1);
    assert_eq!(t.rgba.len(), t.width * t.height * 4);
    assert_eq!(t.rgba, full_expansion(&t));
}

#[test]
fn fractional_scroll_frames_present_the_full_band() {
    let mut t = term();
    for i in 0..50 {
        t.process(format!("frac line {i}\r\n").as_bytes());
    }
    t.render().expect("renderable frame");
    let half_cell = t.cell_height() as f64 / 2.0;
    t.scroll_px(half_cell); // sub-row residual banks, no whole-row flip
    t.render().expect("renderable frame");
    assert_eq!(
        t.present_bands,
        vec![0, 0, t.width as i32, t.height as i32],
        "a translated band frame must not export partial bands"
    );
    t.scroll_px(-half_cell);
    t.render().expect("renderable frame");
    assert_eq!(
        t.present_bands,
        vec![0, 0, t.width as i32, t.height as i32],
        "the residual-clearing frame re-presents the whole band"
    );
    assert_eq!(t.rgba, full_expansion(&t));
}

#[test]
fn scroll_blit_is_taken_and_stays_byte_exact() {
    use aterm_render::DamageOutcome;
    let mut t = term();
    t.set_scrollback_limit(4000);
    for i in 0..2000 {
        t.process(format!("scroll blit line {i}\r\n").as_bytes());
    }
    t.scroll_to_bottom();
    t.render().expect("renderable frame");
    let mut blit_frames = 0usize;
    for _ in 0..60 {
        t.scroll_lines(3);
        t.render().expect("renderable frame");
        if matches!(t.win.last_damage(), DamageOutcome::Scroll { .. }) {
            blit_frames += 1;
        }
        assert_eq!(
            t.rgba,
            full_expansion(&t),
            "scroll-blit present must be byte-identical to a full repaint"
        );
        assert_eq!(
            t.present_bands,
            vec![0, 0, t.width as i32, t.height as i32],
            "a scroll-blit frame exports one full band"
        );
    }
    assert!(
        blit_frames >= 1,
        "the scroll-blit path must be exercised ({blit_frames}/60 frames)"
    );
}

#[test]
fn shade_scroll_blit_present_stays_byte_exact() {
    use aterm_render::DamageOutcome;
    let mut t = term();
    t.set_scrollback_limit(4000);
    for i in 0..1500 {
        let line = format!("\u{2593}\u{2593}\u{2592}\u{2591} \u{c9}\u{c1}\u{d1} r{i} \u{2588}\r\n");
        t.process(line.as_bytes());
    }
    t.scroll_to_bottom();
    t.render().expect("renderable frame");
    let mut blit_frames = 0usize;
    for _ in 0..40 {
        t.scroll_lines(1); // an odd pixel shift when cell_h is odd
        t.render().expect("renderable frame");
        if matches!(t.win.last_damage(), DamageOutcome::Scroll { .. }) {
            blit_frames += 1;
        }
        assert_eq!(
            t.rgba,
            full_expansion(&t),
            "shade scroll-blit present must equal a full repaint"
        );
    }
    assert!(
        blit_frames >= 1,
        "the shade scroll-blit path must be exercised ({blit_frames}/40 frames)"
    );
}

#[test]
fn selection_change_bands_only_the_selection_rows() {
    let mut t = term();
    t.process(b"alpha beta gamma\r\ndelta epsilon\r\nzeta eta");
    t.render().expect("renderable frame");
    t.selection_start(1, 0);
    t.selection_extend(1, 5);
    t.render().expect("renderable frame");
    assert!(t.present_band_count() >= 1);
    let banded_px: i32 = t
        .present_bands
        .as_chunks::<4>()
        .0
        .iter()
        .map(|q| q[2] * q[3])
        .sum();
    assert!(
        (banded_px as usize) < t.width * t.height,
        "a one-row selection drag repaints a sub-frame band"
    );
    assert_eq!(t.rgba, full_expansion(&t));
}

#[test]
fn rgba_shift_matches_snapshot_coordinates_and_preserves_unmoved_bytes() {
    for w in [0usize, 1, 2, 7] {
        for h in [0usize, 1, 2, 5, 9] {
            let original: Vec<u8> = (0..w * h * 4 + 3).map(|n| (n % 251) as u8).collect();
            for y0 in 0..h + 2 {
                for y1 in 0..h + 2 {
                    for delta in [i64::MIN, -10, -5, -2, -1, 0, 1, 2, 5, 10, i64::MAX] {
                        let mut expected = original.clone();
                        if w != 0 && y0 < y1 && y1 <= h {
                            for row in y0..y1 {
                                let source = row as i128 + i128::from(delta);
                                if source >= y0 as i128 && source < y1 as i128 {
                                    let stride = w * 4;
                                    let source = source as usize;
                                    expected[row * stride..(row + 1) * stride].copy_from_slice(
                                        &original[source * stride..(source + 1) * stride],
                                    );
                                }
                            }
                        }
                        let mut actual = original.clone();
                        shift_rgba_band(&mut actual, w, y0, y1, delta);
                        assert_eq!(
                            actual, expected,
                            "w={w} h={h} y0={y0} y1={y1} delta={delta}"
                        );
                    }
                }
            }
        }
    }
    for w in [usize::MAX, usize::MAX / 4 + 1] {
        let mut bytes = [1, 2, 3, 4];
        shift_rgba_band(&mut bytes, w, 0, 1, 1);
        assert_eq!(bytes, [1, 2, 3, 4]);
    }
}

#[test]
#[ignore = "manual before/after pixel-band shift measurement; run alone"]
fn rgba_shift_cost() {
    use std::hint::black_box;
    use std::time::Instant;
    let case: usize = std::env::var("ATERM_RGBA_SHIFT_CASE")
        .unwrap()
        .parse()
        .unwrap();
    let (w, h, delta) = [
        (64usize, 32usize, 1i64),
        (640, 480, 1),
        (1600, 900, 14),
        (1600, 900, -14),
    ][case];
    let stride = w * 4;
    let original: Vec<u8> = (0..stride * h).map(|n| (n % 251) as u8).collect();
    let mut expected = original.clone();
    let mag = delta.unsigned_abs() as usize;
    for row in 0..h {
        let source = if delta > 0 {
            row + ((h - 1 - row) / mag) * mag
        } else {
            row % mag
        };
        expected[row * stride..(row + 1) * stride]
            .copy_from_slice(&original[source * stride..(source + 1) * stride]);
    }
    let mut rgba = original;
    eprintln!("RGBA shift case={case} width={w} height={h} delta={delta} batches=20 shifts_per_batch=128 bytes={}", rgba.len());
    for batch in 0..20 {
        let start = Instant::now();
        for _ in 0..128 {
            shift_rgba_band(
                black_box(&mut rgba),
                black_box(w),
                black_box(0),
                black_box(h),
                black_box(delta),
            );
        }
        eprintln!(
            "RGBA shift batch={batch} elapsed_ns={}",
            start.elapsed().as_nanos()
        );
    }
    assert_eq!(black_box(rgba), expected);
}
