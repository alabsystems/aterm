// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{Theme, WindowCpu};
use aterm_core::terminal::Terminal;

fn renderer() -> Renderer {
    let bytes = std::fs::read(aterm_tempfile::manifest_dir().join("assets/DejaVuSansMono.ttf"))
        .expect("bundled DejaVu asset");
    let mut r = Renderer::from_bytes(&bytes, 18.0, Theme::default()).expect("fixture parses");
    r.set_runtime_font_discovery(false);
    r
}

fn scrolled_input() -> RenderInput {
    let mut t = Terminal::new(8, 20);
    t.process(b"\x1b[?25l");
    for i in 0..40 {
        t.process(format!("LINE {i:02}\r\n").as_bytes());
    }
    t.scroll_display(2);
    let input = t.cell_frame(8, 20);
    assert!(input.apron_row.present && input.apron_row.cursor_col.is_none());
    input
}

#[test]
fn the_apron_raster_follows_the_font_epoch_and_every_invalidation() {
    let mut r = renderer();
    let input = scrolled_input();
    let mut sc = ApronScratch::default();
    assert!(r.rasterize_apron_row(&input, &mut sc));
    let g0 = sc.generation();
    assert!(r.rasterize_apron_row(&input, &mut sc));
    assert_eq!(
        sc.generation(),
        g0,
        "an unchanged frame is a hit: nothing rastered"
    );
    r.cursor_blink_phase = !r.cursor_blink_phase;
    assert!(r.rasterize_apron_row(&input, &mut sc));
    assert_eq!(
        sc.generation(),
        g0,
        "no caret on the row: the blink phase is not its input"
    );
    r.font_epoch += 1;
    assert!(r.rasterize_apron_row(&input, &mut sc));
    assert_eq!(
        sc.generation(),
        g0 + 1,
        "a landed face re-rasters the strip"
    );
    sc.invalidate();
    assert!(r.rasterize_apron_row(&input, &mut sc));
    assert_eq!(
        sc.generation(),
        g0 + 2,
        "an invalidation re-rasters the strip"
    );
    let mut wc = WindowCpu::default();
    assert!(r.rasterize_apron_row(&input, &mut wc.apron));
    let w0 = wc.apron.generation();
    wc.invalidate();
    assert!(r.rasterize_apron_row(&input, &mut wc.apron));
    assert_eq!(
        wc.apron.generation(),
        w0 + 1,
        "the window's invalidation reaches the strip"
    );
}
