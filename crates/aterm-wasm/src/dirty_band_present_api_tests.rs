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
