// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Fx layers in a composed split, and the residency-hint seam.

use std::sync::Arc;

use aterm_core::render::{
    FxArt, FxBlend, FxContent, FxInk, FxItem, FxLayer, FxRect, FxSlot, FxSpace, FxTexels, PxRect,
};
use aterm_render::Frame;

use super::*;
use crate::pane::PaneRect;

fn pane_layer(id: u64, (x, y, w, h): (i32, i32, u16, u16), space: FxSpace) -> FxLayer {
    FxLayer::new(
        id,
        Arc::new(FxContent::new(
            vec![FxItem::Rect(FxRect { x, y, w, h, ink: 0 })],
            vec![FxInk::over(0x00C0_3070, 0xA0); 2],
        )),
        FxSlot::Over,
        space,
        FxBlend::Exact,
    )
}

fn split_app() -> (App, WindowId, Vec<PaneRect>) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    let sid = app.split_active_stub_tab(wid);
    app.windows.get_mut(&wid).expect("window").focused = true;
    for s in [0, sid] {
        let term = app.pool.get(s).expect("pane session").term.clone();
        crate::term_lock(&term).process(b"\x1b[?25l");
    }
    let ws = &app.windows[&wid];
    let rects = ws.layouts[ws.tabs.active].compute_layout(ws.rows, ws.cols);
    assert_eq!(rects.len(), 2, "the fixture is a two-leaf split: {rects:?}");
    assert!(
        rects.iter().any(|r| r.col_off > 0),
        "one pane sits right of the window's corner: {rects:?}"
    );
    (app, wid, rects)
}

fn frame_with(
    app: &mut App,
    wid: WindowId,
    base: &RenderInput,
    pane: Option<&PaneRect>,
    install: impl FnOnce(&mut RenderInput, Option<ComposedCursorEffectClip>),
) -> Frame {
    let (cell_w, cell_h) = app.win_cell_size(wid);
    let clip = pane.and_then(|p| {
        app.composed_cursor_effect_clip(
            wid,
            ComposedCursorEffectPane {
                row: usize::from(p.row_off),
                col: usize::from(p.col_off),
                pane_rows: usize::from(p.rows),
                pane_cols: usize::from(p.cols),
                cell_w,
                cell_h,
            },
        )
    });
    {
        let ws = app.windows.get_mut(&wid).expect("window");
        ws.input_scratch.clone_from(base);
        install(&mut ws.input_scratch, clip);
    }
    app.splice_tab_strip(wid);
    let (pad, pad_top, head) = (app.win_pad(wid), app.win_pad_top(wid), app.win_head(wid));
    let input = app.windows[&wid].input_scratch.clone();
    let Backend::Cpu(r) = app.backend.ready_mut() else {
        panic!("the headless backend is the CPU renderer");
    };
    r.set_pad(pad);
    r.set_pad_top(pad_top);
    r.set_head(head);
    r.render_input(&input)
}

fn project(dst: &mut RenderInput, clip: Option<ComposedCursorEffectClip>, layers: &[FxLayer]) {
    project_focused_composed_cursor_effects(
        dst,
        FocusedComposedCursorEffects {
            glow_add: &[],
            glow_halo: &[],
            fire_patch: &[],
            glow_under: &[],
            char_fg: &[],
            fire_halo: &[],
            trail: &[],
            trail_color: 0,
            active_cursor_fill: None,
            fx_layers: layers,
        },
        clip,
        None,
        None,
    );
}

fn changed(a: &Frame, b: &Frame) -> Vec<(usize, usize)> {
    assert_eq!((a.width, a.height), (b.width, b.height));
    (0..a.pixels.len())
        .filter(|&i| a.pixels[i] != b.pixels[i])
        .map(|i| (i % a.width, i / a.width))
        .collect()
}

#[test]
fn a_panes_fx_layers_land_at_its_corner_and_stay_in_its_box() {
    let (mut app, wid, panes) = split_app();
    let now = Instant::now();
    let (rows, cols) = {
        let ws = &app.windows[&wid];
        (usize::from(ws.rows), usize::from(ws.cols))
    };
    assert!(
        app.redraw_compose(wid, rows, cols, false, false, None, 0, now)
            .is_some(),
        "the split composes"
    );
    let base = app.windows[&wid].input_scratch.clone();
    let strip = usize::from(app.chrome_rows(wid));
    assert!(
        strip >= 1,
        "the strip is up, so the prepend moves the layers"
    );
    let (cw, ch) = app.win_cell_size(wid);
    let (pad, grid_top) = (app.win_pad(wid), app.win_pad_top(wid) + app.win_head(wid));
    let bare = frame_with(&mut app, wid, &base, None, |dst, clip| {
        project(dst, clip, &[]);
    });
    assert_eq!(
        app.windows[&wid].input_scratch.cells.len(),
        rows + strip,
        "the strip was prepended"
    );
    let (cwi, chi) = (i32::try_from(cw).unwrap(), i32::try_from(ch).unwrap());
    for (i, pane) in panes.iter().enumerate() {
        let sibling = &panes[1 - i];
        let cell_box = |p: &PaneRect| {
            let x0 = pad + usize::from(p.col_off) * cw;
            let y0 = grid_top + (strip + usize::from(p.row_off)) * ch;
            (
                x0..x0 + usize::from(p.cols) * cw,
                y0..y0 + usize::from(p.rows) * ch,
            )
        };
        let (bx, by) = cell_box(pane);
        let (sx, sy) = cell_box(sibling);
        let wide = (
            -3 * cwi,
            -3 * chi,
            u16::try_from((usize::from(pane.cols) + 6) * cw).unwrap(),
            u16::try_from((usize::from(pane.rows) + 6) * ch).unwrap(),
        );
        let window = pane_layer(10, wide, FxSpace::Window);
        let grid = pane_layer(11, wide, FxSpace::Grid);

        let lit = frame_with(&mut app, wid, &base, Some(pane), |dst, clip| {
            project(dst, clip, std::slice::from_ref(&window));
        });
        let px = changed(&lit, &bare);
        assert!(
            px.iter().all(|(x, y)| bx.contains(x) && by.contains(y)),
            "pane {i}: the light stays in its box {bx:?}x{by:?}"
        );
        for corner in [
            (bx.start, by.start),
            (bx.end - 1, by.start),
            (bx.start, by.end - 1),
            (bx.end - 1, by.end - 1),
        ] {
            assert!(
                px.contains(&corner),
                "pane {i}: the light reaches {corner:?}"
            );
        }
        let lit_grid = frame_with(&mut app, wid, &base, Some(pane), |dst, clip| {
            project(dst, clip, std::slice::from_ref(&grid));
        });
        assert!(
            lit_grid.pixels == lit.pixels,
            "pane {i}: the Grid-space layer registers with the Window-space one"
        );
        let leak = frame_with(&mut app, wid, &base, Some(pane), |dst, clip| {
            let mut placed = clip.expect("on grid").fx.place(&window);
            placed.clip = PxRect::EVERYWHERE;
            dst.fx_layers.push(placed);
        });
        assert!(
            changed(&leak, &bare)
                .iter()
                .any(|(x, y)| sx.contains(x) && sy.contains(y)),
            "CONTROL pane {i}: unclipped, the light reaches the sibling pane"
        );

        let probe = pane_layer(12, (0, 0, cw as u16, ch as u16), FxSpace::Window);
        let corner_cell = |(x, y): &(usize, usize)| {
            (bx.start..bx.start + cw).contains(x) && (by.start..by.start + ch).contains(y)
        };
        let placed = frame_with(&mut app, wid, &base, Some(pane), |dst, clip| {
            project(dst, clip, std::slice::from_ref(&probe));
        });
        let px = changed(&placed, &bare);
        assert!(
            px.len() == cw * ch && px.iter().all(corner_cell),
            "pane {i}: the probe lights exactly the pane's top-left cell"
        );
        let unplaced = frame_with(&mut app, wid, &base, Some(pane), |dst, _| {
            dst.fx_layers.push(probe.clone());
        });
        assert!(
            !changed(&unplaced, &bare).iter().any(corner_cell),
            "CONTROL pane {i}: unplaced, the probe lights another cell"
        );

        let twice = frame_with(&mut app, wid, &base, Some(pane), |dst, clip| {
            project(dst, clip, std::slice::from_ref(&window));
            project(dst, clip, std::slice::from_ref(&window));
            assert_eq!(dst.fx_layers.len(), 1, "replaced by id, not doubled");
        });
        assert!(twice.pixels == lit.pixels, "pane {i}: twice is once");
        let doubled = frame_with(&mut app, wid, &base, Some(pane), |dst, clip| {
            let placement = clip.expect("on grid").fx;
            dst.fx_layers.push(placement.place(&window));
            dst.fx_layers.push(placement.place(&window));
        });
        assert!(
            doubled.pixels != lit.pixels,
            "CONTROL pane {i}: a doubled translucent sheet draws other pixels"
        );
    }

    let mut dst = base.clone();
    let window = pane_layer(10, (0, 0, 8, 8), FxSpace::Window);
    let clip = app.composed_cursor_effect_clip(
        wid,
        ComposedCursorEffectPane {
            row: usize::from(panes[0].row_off),
            col: usize::from(panes[0].col_off),
            pane_rows: usize::from(panes[0].rows),
            pane_cols: usize::from(panes[0].cols),
            cell_w: cw,
            cell_h: ch,
        },
    );
    project(&mut dst, clip, std::slice::from_ref(&window));
    assert_eq!(dst.fx_layers.len(), 1);
    project(&mut dst, None, std::slice::from_ref(&window));
    assert!(dst.fx_layers.is_empty(), "no pane, no light");
}

#[test]
fn the_placement_moves_offset_and_clip_by_the_pane_origin() {
    let placement = PaneFxPlacement {
        window_origin: (40, 70),
        grid_origin: (30, 34),
        window_box: PxRect::new(40, 70, 140, 190),
    };
    let mut w = pane_layer(1, (0, 0, 4, 4), FxSpace::Window);
    w.offset = (3, -2);
    w.clip = PxRect::new(-10, 5, 50, 500);
    let placed = placement.place(&w);
    assert_eq!(placed.offset, (43, 68));
    assert_eq!(placed.clip, PxRect::new(40, 75, 90, 190));
    let mut g = w.clone();
    g.space = FxSpace::Grid;
    let placed = placement.place(&g);
    assert_eq!(placed.offset, (33, 32));
    assert_eq!(placed.clip, PxRect::new(30, 39, 80, 154));
    let open = placement.place(&pane_layer(2, (0, 0, 4, 4), FxSpace::Window));
    assert_eq!(
        open.clip, placement.window_box,
        "an open clip becomes the box"
    );
}

fn art(byte: u8) -> Arc<FxArt> {
    Arc::new(FxArt::new(1, 1, FxTexels::Index8, vec![byte]).expect("a 1x1 art"))
}

fn same(a: &[Arc<FxArt>], b: &[&Arc<FxArt>]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| Arc::ptr_eq(x, y))
}

#[test]
fn a_hint_reaches_one_present_that_reaches_the_renderer() {
    let (a, b, c) = (art(1), art(2), art(3));
    let mut pending = vec![a.clone(), b.clone()];
    let mut input = RenderInput::empty();
    input.fx_prefetch = vec![c.clone()];
    hand_fx_prefetch(&mut pending, &mut input);
    assert!(same(&input.fx_prefetch, &[&a, &b]) && pending.is_empty());
    pending.push(c.clone());
    settle_fx_prefetch(&mut pending, &mut input, false);
    assert!(
        same(&pending, &[&a, &b, &c]) && input.fx_prefetch.is_empty(),
        "a dropped present queues its hints again, first"
    );
    hand_fx_prefetch(&mut pending, &mut input);
    assert!(same(&input.fx_prefetch, &[&a, &b, &c]));
    settle_fx_prefetch(&mut pending, &mut input, true);
    assert!(
        pending.is_empty() && input.fx_prefetch.is_empty(),
        "a taken present consumes them"
    );
}

#[test]
fn queued_hints_ride_the_next_present_through_a_dropped_one() {
    use crate::present::scripted::ScriptedOutcome;
    use crate::scripted_redraw_fixture::{feed, scripted_app, surface};
    let wid = WindowId(0);
    let mut app = scripted_app();
    app.redraw_window(wid);
    let proved = |app: &App| app.windows[&wid].redraws_proved_unchanged;
    let before = proved(&app);
    app.redraw_window(wid);
    assert_eq!(
        proved(&app),
        before + 1,
        "CONTROL: a repeat frame with nothing queued is proved unchanged"
    );
    let (a, b, c) = (art(1), art(2), art(3));
    app.queue_fx_prefetch(wid, &[a.clone(), b.clone(), a.clone()]);
    assert!(same(&app.windows[&wid].fx_prefetch_pending, &[&a, &b]));
    let before = proved(&app);
    app.redraw_window(wid);
    assert_eq!(proved(&app), before, "a hint-only frame is not gated");
    assert!(
        app.windows[&wid].fx_prefetch_pending.is_empty()
            && app.windows[&wid].input_scratch.fx_prefetch.is_empty(),
        "the present consumed the hints"
    );

    feed(&app, b"x");
    app.queue_fx_prefetch(wid, std::slice::from_ref(&c));
    let commits = surface(&mut app).commits();
    surface(&mut app).fail_next(ScriptedOutcome::FailAcquire);
    app.redraw_window(wid);
    assert_eq!(surface(&mut app).commits(), commits, "the present failed");
    assert!(
        same(&app.windows[&wid].fx_prefetch_pending, &[&c]),
        "a failed present keeps its hints"
    );
}

#[test]
fn queued_hints_ride_a_split_windows_next_present() {
    use crate::scripted_redraw_fixture::scripted_app;
    let wid = WindowId(0);
    let mut app = scripted_app();
    app.split_active_stub_tab(wid);
    assert!(app.windows[&wid].is_split(), "the window composes");
    app.redraw_window(wid);
    app.redraw_window(wid);
    let proved = |app: &App| app.windows[&wid].redraws_proved_unchanged;
    let before = proved(&app);
    app.redraw_window(wid);
    assert_eq!(
        proved(&app),
        before + 1,
        "CONTROL: a repeat composed frame with nothing queued is proved unchanged"
    );
    let (a, b) = (art(1), art(2));
    app.queue_fx_prefetch(wid, &[a.clone(), b.clone()]);
    let before = proved(&app);
    app.redraw_window(wid);
    assert_eq!(
        proved(&app),
        before,
        "a hint-only composed frame is not gated"
    );
    assert!(
        app.windows[&wid].fx_prefetch_pending.is_empty()
            && app.windows[&wid].input_scratch.fx_prefetch.is_empty(),
        "the composed present consumed the hints"
    );
}
