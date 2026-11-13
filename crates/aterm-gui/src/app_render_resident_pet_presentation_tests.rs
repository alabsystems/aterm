// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_effects::host::CaptureMode;
use aterm_effects::kitty_pet::PetSense;
use std::time::{Duration, Instant};

#[test]
fn a_reduced_motion_resident_keeps_full_body_cursor_home_and_no_frame_debt() {
    let mut app = crate::App::headless_for_test();
    let geom = aterm_effects::word_decorations::EffectGeom {
        cell_w: 10,
        cell_h: 20,
        rows: 30,
        cols: 80,
    };
    let ws = app.windows.get_mut(&crate::WindowId(0)).unwrap();
    let start = Instant::now();
    let mut tick = 0;
    for caret_col in [12_u16, 60] {
        let caret = (4, caret_col);
        let mut previous = None;
        for _ in 0..3 {
            let now = start + Duration::from_millis(tick * 16);
            tick += 1;
            let pet = crate::app_render::pet_owner_tick_for_test(
                ws,
                CaptureMode::Present,
                PetSense {
                    caret_drawn: true,
                    now,
                    caret: Some(caret),
                    rows: geom.rows,
                    cols: geom.cols,
                    cell_w: geom.cell_w,
                    cell_h: geom.cell_h,
                    reduced_motion: true,
                    output_burst: false,
                    pointer: None,
                    wrapped: false,
                },
            );
            assert!(pet.on_glass);
            let body = pet.body_px.expect("the static resident is drawn");
            let cursor_x = i32::from(caret.1) * i32::from(geom.cell_w);
            assert!(
                (body.0 - cursor_x).max(cursor_x - body.1).max(0) <= 10 * i32::from(geom.cell_w),
                "the stationary posture follows the caret"
            );
            assert!(
                !ws.fx.companion_mut().needs_frames(),
                "a retained static body owes no idle animation"
            );
            if let Some(previous) = previous {
                assert_eq!(pet.fp, previous, "idle frames remain byte-stable");
            }
            previous = Some(pet.fp);
            ws.fx.open_words();
            let head = ws.fx.companions_mut().cursor_cat.static_frame(now);
            let fp = super::emit_single_cursor_companion(
                ws,
                geom,
                Some(caret),
                &pet,
                false,
                head,
                false,
                0,
                now,
                true,
                0x0010_1010,
                0x00FF_FFFF,
                0x0050_FA7B,
            );
            assert_ne!(fp, 0);
            assert_eq!(
                ws.fx.free_sprites().len(),
                1,
                "the static posture emits only its full body"
            );
            assert!(ws.fx.free_sprites()[0].w >= 5 * geom.cell_w);
        }
    }
}
