// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Scratch probe (HUNT B): the flying head's REAL footprint vs the yield box
//! `WordDecorations::tick` synthesizes for it when no `body_px` is supplied.
use aterm_effects::word_decorations::{EffectGeom, KittyCursorLayout, WordDecorations};

fn main() {
    for (cw, ch) in [(10u16, 20u16), (9, 19), (8, 17), (20, 20)] {
        let geom = EffectGeom {
            cell_w: cw,
            cell_h: ch,
            rows: 24,
            cols: 80,
        };
        let wd = WordDecorations::default();
        let (row, col) = (10u16, 20u16);
        let fp = wd
            .kitty_cursor_footprint(KittyCursorLayout {
                geom,
                cursor: (row, col),
                look: Default::default(),
                bob: 0.0,
            })
            .expect("footprint");
        // The fallback box built in `tick` (word_decorations.rs ~6553).
        let x0 = i32::from(col) * i32::from(cw);
        let y0 = i32::from(row) * i32::from(ch);
        let box_px = (x0, x0 + 2 * i32::from(cw), y0, y0 + i32::from(ch));
        let sprite = (fp.x, fp.x + i32::from(fp.w), fp.y, fp.y + i32::from(fp.h));
        let iw = (sprite.1.min(box_px.1) - sprite.0.max(box_px.0)).max(0);
        let ih = (sprite.3.min(box_px.3) - sprite.2.max(box_px.2)).max(0);
        let sprite_area = i64::from(sprite.1 - sprite.0) * i64::from(sprite.3 - sprite.2);
        let cover = i64::from(iw) * i64::from(ih) * 100 / sprite_area.max(1);
        println!(
            "cell {cw}x{ch}: sprite x[{}..{}] y[{}..{}] ({}x{})  box x[{}..{}] y[{}..{}]  \
             covered {cover}% of the head",
            sprite.0,
            sprite.1,
            sprite.2,
            sprite.3,
            fp.w,
            fp.h,
            box_px.0,
            box_px.1,
            box_px.2,
            box_px.3
        );
    }
}
