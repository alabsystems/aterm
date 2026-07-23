// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance for `AsymmetricPadLayout`: drive the derived model's
//! executable actions against the shipping `Renderer` layout and CPU cache.

use aterm_core::terminal::Terminal;
use aterm_render::{DamageOutcome, Renderer, Theme, WindowCpu, project_asymmetric_pad_layout};
use aterm_spec::derive::asymmetric_pad_layout_model;

const FONT: &[u8] = include_bytes!("../assets/DejaVuSansMono.ttf");

#[test]
fn asymmetric_pad_layout_model_conforms_to_shipping_cpu_geometry_and_cache() {
    let model = asymmetric_pad_layout_model();
    let picked_states = model.successors("PickLayout", &model.init_state());
    let mut renderer = Renderer::from_bytes(FONT, 16.0, Theme::default()).expect("fixture font");
    renderer.debug_block_on_lazy_fallbacks();

    let (rows, cols) = (2usize, 8usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25lcache");
    let input = term.cell_frame(rows, cols);
    let mut points = 0usize;
    let mut moved_origins = 0usize;
    let mut clamped_requests = 0usize;
    let mut mutant_rejections = 0usize;

    for picked in picked_states {
        let initial = model.successors("ApplyInitialTop", &picked)[0].clone();
        let primed = model.successors("PrimeLayoutCache", &initial)[0].clone();
        let changed = model.successors("ApplyChangedTop", &primed)[0].clone();
        let decided = model.successors("RenderWithLayoutCache", &changed)[0].clone();

        let pad = picked["pad"] as usize;
        let head = picked["head"] as usize;
        let initial_request = picked["initial_request"] as usize;
        let changed_request = picked["changed_request"] as usize;
        renderer.set_pad(pad);
        renderer.set_head(head);
        renderer.set_pad_top(initial_request);
        let mut window = WindowCpu::new();

        let layout = project_asymmetric_pad_layout(&renderer, &window, rows);
        assert_eq!(layout.pad_top, initial["pad_top"] as usize);
        assert_eq!(layout.pad_bottom, initial["pad_bottom"] as usize);
        assert_eq!(layout.grid_top, initial["grid_top"] as usize);
        assert_eq!(layout.pad_top + layout.pad_bottom, 2 * pad);
        assert!(layout.pad_top <= pad);
        assert_eq!(layout.grid_top, head + layout.pad_top);
        if initial_request > pad {
            clamped_requests += 1;
            assert_ne!(
                layout.pad_top, initial_request,
                "unclamped mutant must differ"
            );
        }

        // Stutter conformance: re-applying the SAME base padding is an
        // idempotent environment update, so it must preserve every modeled
        // ApplyInitialTop variable. The former unconditional override clear is
        // the negative mutant here; every point with pad_top != pad catches it.
        renderer.set_pad(pad);
        let after_same_pad = project_asymmetric_pad_layout(&renderer, &window, rows);
        assert_eq!(after_same_pad.pad_top, layout.pad_top);
        assert_eq!(after_same_pad.pad_bottom, layout.pad_bottom);
        assert_eq!(after_same_pad.grid_top, layout.grid_top);
        let frame_size = renderer.frame_size(rows, cols);

        let first = renderer
            .render_input_cached(&mut window, &input)
            .pixels()
            .to_vec();
        assert_eq!(window.last_damage(), DamageOutcome::Full);
        let cache = project_asymmetric_pad_layout(&renderer, &window, rows);
        assert_eq!(
            cache.cached_grid_top,
            Some(primed["cached_grid_top"] as usize)
        );

        // The identical-layout arm is real too: the just-primed cache must gate-hit.
        let steady = renderer
            .render_input_cached(&mut window, &input)
            .pixels()
            .to_vec();
        assert_eq!(steady, first);
        assert_eq!(window.last_damage(), DamageOutcome::GateHit);

        renderer.set_pad_top(changed_request);
        assert_eq!(
            renderer.frame_size(rows, cols),
            frame_size,
            "top-only change must keep dimensions fixed"
        );
        let before_decision = project_asymmetric_pad_layout(&renderer, &window, rows);
        assert_eq!(before_decision.pad_top, changed["pad_top"] as usize);
        assert_eq!(before_decision.pad_bottom, changed["pad_bottom"] as usize);
        assert_eq!(before_decision.grid_top, changed["grid_top"] as usize);
        assert_eq!(
            before_decision.cached_grid_top,
            Some(changed["cached_grid_top"] as usize)
        );
        assert_eq!(
            before_decision.pad_top + before_decision.pad_bottom,
            2 * pad
        );
        if changed_request > pad {
            clamped_requests += 1;
            assert_ne!(
                before_decision.pad_top, changed_request,
                "changed-top unclamped mutant must differ"
            );
        }

        let second = renderer
            .render_input_cached(&mut window, &input)
            .pixels()
            .to_vec();
        let after_decision = project_asymmetric_pad_layout(&renderer, &window, rows);
        assert_eq!(
            usize::from(after_decision.cache_hit),
            decided["cache_hit"] as usize
        );
        assert_eq!(
            usize::from(after_decision.full_repaint),
            decided["full_repaint"] as usize
        );

        if initial["grid_top"] != changed["grid_top"] {
            moved_origins += 1;
            assert_eq!(window.last_damage(), DamageOutcome::Full);
            assert_ne!(second, first, "visible row zero must move with grid_top");

            // NEGATIVE CONTROL: the pre-fix/mutant key used only dimensions and
            // content. Both are unchanged here, so it would falsely hit exactly
            // where the model and shipping cache require a full repaint.
            let dimension_only_mutant_hit = renderer.frame_size(rows, cols) == frame_size;
            assert!(dimension_only_mutant_hit);
            assert_eq!(decided["cache_hit"], 0);
            assert_ne!(
                usize::from(dimension_only_mutant_hit),
                decided["cache_hit"] as usize
            );
            mutant_rejections += 1;
        } else {
            assert_eq!(window.last_damage(), DamageOutcome::GateHit);
            assert_eq!(second, first);
        }
        points += 1;
    }

    assert_eq!(points, 3 * 3 * 5 * 5, "complete bounded PickLayout lattice");
    assert!(moved_origins > 0, "origin-changing cache arm was exercised");
    assert!(
        clamped_requests > 0,
        "oversized top requests were exercised"
    );
    assert_eq!(mutant_rejections, moved_origins);
}
