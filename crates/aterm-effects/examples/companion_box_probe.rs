// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! HUNT A probe: the flying head's DRAWN rect vs the rect the word-cat
//! pixel-yield (`CatTick::companion_px`) is handed for it.
use aterm_effects::word_decorations::{EffectGeom, KittyCursorLayout, WordDecorations};
use aterm_effects::kitty_registry::KittyLook;

fn main() {
    for (cw, ch) in [(15u16, 28u16), (10, 20), (8, 16)] {
        let geom = EffectGeom { cell_w: cw, cell_h: ch, rows: 24, cols: 80 };
        let wd = WordDecorations::default();
        for (crow, ccol) in [(3u16, 0u16), (3, 10), (10, 40)] {
            let fp = wd.kitty_cursor_footprint(KittyCursorLayout {
                geom,
                cursor: (crow, ccol),
                look: KittyLook::default(),
                bob: 0.0,
            });
            let Some(f) = fp else { println!("no footprint"); continue };
            let hx0 = f.x;
            let hx1 = hx0 + i32::from(f.w);
            let hy0 = f.y;
            let hy1 = hy0 + i32::from(f.h);
            // The model the host hands the yield for a flying head.
            let mx0 = i32::from(ccol) * i32::from(cw);
            let mx1 = mx0 + 2 * i32::from(cw);
            let my0 = i32::from(crow) * i32::from(ch);
            let my1 = my0 + i32::from(ch);
            let iw = (hx1.min(mx1) - hx0.max(mx0)).max(0);
            let ih = (hy1.min(my1) - hy0.max(my0)).max(0);
            let head_area = (hx1 - hx0) * (hy1 - hy0);
            println!(
                "cell {cw}x{ch} caret r{crow}c{ccol}: HEAD x[{hx0},{hx1}) y[{hy0},{hy1}) {}x{} | MODEL x[{mx0},{mx1}) y[{my0},{my1}) | overlap {}x{} = {:.1}% of the head",
                f.w, f.h, iw, ih,
                100.0 * f64::from(iw * ih) / f64::from(head_area.max(1)),
            );
        }
    }
}