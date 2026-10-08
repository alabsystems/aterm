// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{App, Backend, RepaintKey, SelectionFingerprint, WindowId};
use crate::app_render::{
    project_asymmetric_pad_layout_key, should_repaint, should_repaint_or_recover,
};
use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_core::terminal::{CursorStyle, Terminal};
use aterm_render::{DamageOutcome, RenderInput};

/// A selection with anchors `(r0,c0)`..`(r1,c1)`, completed or not.
fn selection(
    r0: u16,
    c0: u16,
    r1: u16,
    c1: u16,
    done: bool,
) -> aterm_core::selection::TextSelection {
    let mut sel = aterm_core::selection::TextSelection::default();
    sel.start_selection(
        i32::from(r0),
        c0,
        SelectionSide::Left,
        SelectionType::Simple,
    );
    sel.update_selection(i32::from(r1), c1, SelectionSide::Right);
    if done {
        sel.complete_selection();
    }
    sel
}

/// The pane selection fold: no selection contributes nothing (a settled window folds to
/// 0); any change of a pane's selection, its state, or which pane holds it changes the
/// fold.
#[test]
fn the_pane_selection_fold_changes_with_any_panes_selection() {
    let none = aterm_core::selection::TextSelection::default();
    assert_eq!(super::fold_pane_selection_fp(0, 0, &none), 0);
    assert_eq!(
        super::fold_pane_selection_fp(super::fold_pane_selection_fp(0, 0, &none), 1, &none),
        0,
        "a settled split folds to the idle 0"
    );
    let a = selection(0, 0, 0, 3, true);
    let fold = |sels: &[&aterm_core::selection::TextSelection]| {
        sels.iter()
            .enumerate()
            .fold(0, |acc, (i, s)| super::fold_pane_selection_fp(acc, i, s))
    };
    let only_first = fold(&[&a, &none]);
    let only_second = fold(&[&none, &a]);
    assert_ne!(only_first, 0);
    assert_ne!(only_first, only_second, "the pane holding it matters");
    assert_ne!(
        fold(&[&a, &none]),
        fold(&[&selection(0, 0, 0, 4, true), &none]),
        "a moved end"
    );
    assert_ne!(
        fold(&[&a, &none]),
        fold(&[&selection(0, 0, 0, 3, false), &none]),
        "its state"
    );
    assert_eq!(
        fold(&[&a, &none]),
        fold(&[&a.clone(), &none]),
        "deterministic"
    );
}

/// Build the `RepaintKey` for the current frame exactly as `redraw()` does:
/// observe the damage epoch, the selection, and the supplied visual-only
/// state. Returns the key; the caller decides whether to "present" (which in
/// `redraw()` consumes the damage via `take_damage`).
pub(super) fn frame_key(
    term: &mut Terminal,
    blink_phase: bool,
    invert: bool,
    cursor_override: Option<CursorStyle>,
) -> RepaintKey {
    RepaintKey {
        damage_epoch: term.damage_epoch(),
        grid_top: 0,
        blink_phase,
        invert,
        // These unit frames never exercise the drag-hover highlight.
        drag_hover: false,
        cursor_override,
        cursor_row: term.cursor().row as usize,
        cursor_col: term.cursor().col as usize,
        cursor_visible: term.cursor_visible(),
        cursor_style: term.cursor_style(),
        // No cursor aurora in these unit frames (the wake is a host-driven
        // animation); the cursor-position terms above carry the move.
        glow_fp: 0,
        // No cadence-comet trail in these unit frames (host-driven animation).
        trail_fp: 0,
        // No sparkle-word decorations in these unit frames (host-driven).
        deco_fp: 0,
        // No matrix rain in these unit frames (default-off feature) — the
        // constant-0 sentinel keeps the key byte-identical to the pre-rain path.
        rain_fp: 0,
        streak_fp: 0,
        selection: SelectionFingerprint::of(term.text_selection()),
        pane_selection_fp: super::fold_pane_selection_fp(0, 0, term.text_selection()),
        // These unit frames are single-pane, which marks no pane — the same
        // 0 sentinel the shipping single-pane key carries.
        active_pane_mark: 0,
        // No tab strip in these unit frames (the disabled-strip sentinel), so the
        // key matches the byte-identical no-strip path.
        tab_strip: 0,
        // No tab context menu in these unit frames — the closed sentinel keeps
        // the key byte-identical to the pre-menu path.
        tab_menu_fp: 0,
        // No Settings overlay in these unit frames — the closed sentinel keeps the
        // key byte-identical to the no-overlay path.
        settings_fp: 0,
        // No ⌘F find bar in these unit frames — the not-searching sentinel keeps the
        // key byte-identical to the pre-find path.
        find_fp: 0,
        // No scroll pill in these unit frames (host-driven fade) — the invisible
        // sentinel keeps the key byte-identical to the pre-pill path.
        pill_fp: 0,
        // No composition in these unit frames — the 0 sentinel keeps the
        // key byte-identical to the pre-preedit shape.
        preedit_fp: 0,
        // Whole-row in these unit frames (no sub-row scroll) — the frac-0 sentinel
        // keeps the key byte-identical to the pre-M1b path.
        scroll_frac_px: 0,
        // No update-ready nudge in these unit frames (the no-nudge sentinel).
        relaunch_fp: 0,
        // No build badge in these unit frames (the hidden sentinel).
        badge_fp: 0,
        // No transient notice in these unit frames (the no-notice sentinel).
        bubble_fp: 0,
        // No upgrade rim in these unit frames (the no-rim sentinel).
        level_up_fp: 0,
        // No message band in these unit frames — the hidden sentinel keeps the
        // key byte-identical to the no-band path.
        band_fp: 0,
        // No presence in these unit frames (the quiet sentinel).
        presence_fp: 0,
        // No connection drag in these unit frames (the no-drag sentinel).
        conn_wire_fp: 0,
        // Fixed OS appearance in these unit frames (a flip is exercised by
        // `os_appearance_flip_repaints`).
        system_dark: false,
        link_caption: None,
        // No Claude Code footer in these unit frames (the no-facts sentinel).
        claude_footer_fp: 0,
    }
}

/// `selection_inactive = true` is host state, so a focus flip must invalidate both GUI
/// cache layers: prime the real CPU cache, drive `App::on_focus`, and with an explicit
/// inactive-background override check the unfocused band appears and focus restores it.
#[test]
fn explicit_inactive_selection_focus_flip_invalidates_and_repaints() {
    const INACTIVE_BG: u32 = 0x0001_7AC4;

    fn render_cached(app: &mut App, input: &RenderInput, inactive: bool) -> Vec<u32> {
        app.backend.set_selection_inactive(inactive);
        let App {
            backend, windows, ..
        } = app;
        let Backend::Cpu(renderer) = backend.ready_mut() else {
            panic!("headless test backend is CPU")
        };
        let ws = windows
            .get_mut(&WindowId(0))
            .expect("headless fixture window");
        let _ = renderer.render_input_cached(&mut ws.cpu_cache, input);
        ws.cpu_cache.frame_pixels().to_vec()
    }

    fn count_colour(pixels: &[u32], rgb: u32) -> usize {
        pixels
            .iter()
            .filter(|pixel| **pixel & 0x00ff_ffff == rgb)
            .count()
    }

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.render_knobs.selection_inactive = true;
    let Backend::Cpu(renderer) = app.backend.ready_mut() else {
        panic!("headless test backend is CPU")
    };
    renderer.set_selection_inactive_bg(Some(INACTIVE_BG));

    let mut term = Terminal::new(2, 3);
    term.process(b"\x1b[?25lABC");
    term.text_selection_mut()
        .start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
    term.text_selection_mut()
        .update_selection(0, 0, SelectionSide::Right);
    term.text_selection_mut().complete_selection();
    let input = term.cell_frame(2, 3);
    assert!(
        input.selection.contains(0, 0),
        "fixture must select a visible cell"
    );

    let focused_pixels = render_cached(&mut app, &input, false);
    assert_eq!(
        count_colour(&focused_pixels, INACTIVE_BG),
        0,
        "the explicit inactive override is ignored while focused"
    );
    let _ = render_cached(&mut app, &input, false);
    assert_eq!(
        app.windows[&wid].cpu_cache.last_damage(),
        DamageOutcome::GateHit,
        "negative control: an identical focused frame really is cached"
    );
    let focused_key = frame_key(&mut term, true, false, None);
    app.windows
        .get_mut(&wid)
        .expect("headless fixture window")
        .stamp_present_decision(focused_key);

    app.on_focus(wid, false);
    assert!(
        app.windows[&wid].last_present.is_none(),
        "focus loss invalidates the GUI repaint gate"
    );
    assert_eq!(
        app.windows[&wid].cpu_cache.last_damage(),
        DamageOutcome::Full,
        "focus loss invalidates the per-window raster cache"
    );
    let inactive = app.render_knobs.selection_inactive && !app.windows[&wid].focused;
    assert!(
        inactive,
        "configured unfocused state reaches the present fold"
    );
    let inactive_pixels = render_cached(&mut app, &input, inactive);
    assert_ne!(
        inactive_pixels, focused_pixels,
        "focus loss visibly recolours the selected cell"
    );
    assert!(
        count_colour(&inactive_pixels, INACTIVE_BG) > 0,
        "the next raster uses the explicit inactive-selection background"
    );

    let _ = render_cached(&mut app, &input, true);
    assert_eq!(
        app.windows[&wid].cpu_cache.last_damage(),
        DamageOutcome::GateHit,
        "the unfocused frame is cached before the reverse focus edge"
    );
    let unfocused_key = frame_key(&mut term, true, false, Some(CursorStyle::HollowBlock));
    app.windows
        .get_mut(&wid)
        .expect("headless fixture window")
        .stamp_present_decision(unfocused_key);

    app.on_focus(wid, true);
    assert!(app.windows[&wid].last_present.is_none());
    assert_eq!(
        app.windows[&wid].cpu_cache.last_damage(),
        DamageOutcome::Full
    );
    let inactive = app.render_knobs.selection_inactive && !app.windows[&wid].focused;
    assert!(!inactive);
    let refocused_pixels = render_cached(&mut app, &input, inactive);
    assert_eq!(
        refocused_pixels, focused_pixels,
        "focus gain restores the active-selection raster exactly"
    );
}

/// Drive the ty-checked `AsymmetricPadLayout` cache actions against the real
/// GUI `RepaintKey` comparison. Run by the cross-crate xref gate (its only
/// caller), so the gate runs the behavior as well as collecting its
/// `#[refines]` anchors.
pub(super) fn run_asymmetric_pad_layout_conformance() {
    let model = aterm_spec::derive::asymmetric_pad_layout_model();
    let picked = model
        .successors("PickLayout", &model.init_state())
        .into_iter()
        .find(|state| {
            state["pad"] == 2
                && state["head"] == 1
                && state["initial_request"] == 2
                && state["changed_request"] == 0
        })
        .expect("bounded GUI layout fixture");
    let initial = model.successors("ApplyInitialTop", &picked)[0].clone();
    let primed = model.successors("PrimeLayoutCache", &initial)[0].clone();
    let changed = model.successors("ApplyChangedTop", &primed)[0].clone();
    let decided = model.successors("RenderWithLayoutCache", &changed)[0].clone();

    let mut term = Terminal::new(2, 8);
    term.process(b"\x1b[?25lcache");
    let mut cached = frame_key(&mut term, true, false, None);
    cached.grid_top = initial["grid_top"] as usize;

    // Prime the real shipping cache seam, rather than manufacturing the
    // `Option<RepaintKey>` consumed below. This binds PrimeLayoutCache to
    // WindowState::stamp_present_decision and then projects the private
    // stored key back onto the model's cached_grid_top variable.
    let mut app = App::headless_for_test();
    app.backend.set_pad(picked["pad"] as usize);
    app.backend.set_head(picked["head"] as usize);
    app.backend.set_pad_top(picked["initial_request"] as usize);
    app.backend.set_pad(picked["pad"] as usize);
    assert_eq!(
        app.backend.pad_top(),
        initial["pad_top"] as usize,
        "the GUI backend delegate must not snap a same-value pad to symmetry"
    );
    let cached = {
        let ws = app
            .windows
            .get_mut(&WindowId(0))
            .expect("headless fixture window");
        ws.stamp_present_decision(cached);
        ws.last_present.expect("prime stores the repaint decision")
    };
    assert_eq!(
        project_asymmetric_pad_layout_key(&cached),
        primed["cached_grid_top"] as usize
    );
    assert!(
        !should_repaint(Some(cached), cached),
        "identical layout may reuse the GUI present cache"
    );

    // Same terminal content and every same-sized-frame key term; only the
    // effective grid origin changes.
    let mut current = cached;
    current.grid_top = changed["grid_top"] as usize;
    assert_ne!(current.grid_top, cached.grid_top);
    let repaint = should_repaint(Some(cached), current);
    assert_eq!(usize::from(!repaint), decided["cache_hit"] as usize);
    assert_eq!(usize::from(repaint), decided["full_repaint"] as usize);

    // Negative control: without the `grid_top` term the gate falsely hits, the decision
    // `LayoutChangeForcesFullRepaint` rejects.
    let mut mutant_without_grid_top = current;
    mutant_without_grid_top.grid_top = cached.grid_top;
    assert!(!should_repaint(Some(cached), mutant_without_grid_top));
    assert_ne!(
        usize::from(!should_repaint(Some(cached), mutant_without_grid_top)),
        decided["cache_hit"] as usize
    );
}

/// CPU softbuffer acquisition/commit errors and GPU drawable drops share
/// one recovery transition. The retained-stamp negative control reproduces
/// the black-window wedge; the real host helper must make the identical key
/// repaintable immediately.
#[test]
fn dropped_surface_attempt_unstamps_same_key_for_retry() {
    let mut term = Terminal::new(2, 8);
    term.process(b"surface");
    let key = frame_key(&mut term, true, false, None);
    let wid = WindowId(0);
    let mut app = App::headless_for_test();
    {
        let ws = app.windows.get_mut(&wid).expect("fixture window");
        ws.stamp_present_decision(key);
        assert!(
            !should_repaint(ws.last_present, key),
            "negative control: retaining the optimistic stamp wedges the retry"
        );
    }

    app.rearm_dropped_present(wid, crate::metrics::PresentDropReason::CpuAcquire);
    let stamp = app.windows[&wid].last_present;
    assert!(
        stamp.is_none(),
        "a dropped surface transaction is never success"
    );
    assert!(
        should_repaint(stamp, key),
        "the same frame must be eligible on the very next redraw"
    );
}

/// A surface-recovery redraw is not a content change. Both render paths use
/// this exact cache seam so identical terminal cells cannot swallow the
/// frame that fills newly exposed raw-size bands or acknowledges CPU fallback.
#[test]
fn outstanding_recovery_bypasses_identical_repaint_key() {
    let mut term = Terminal::new(4, 10);
    let key = frame_key(&mut term, true, false, None);
    assert!(!should_repaint(Some(key), key));
    assert!(!should_repaint_or_recover(Some(key), key, false));
    assert!(should_repaint_or_recover(Some(key), key, true));
}

/// What buys the frame after a torn caret: the caret move is itself part of the
/// `RepaintKey`, so the next redraw cannot early-out, and `Wake::Output` clears its latch
/// before its work so a chunk landing mid-redraw re-arms. This pins the half here.
#[test]
fn a_bare_caret_move_moves_the_repaint_key() {
    let mut term = Terminal::new(4, 10);
    let settled = frame_key(&mut term, true, false, None);
    // CUP: relocate the caret, write nothing.
    term.process(b"\x1b[3;7H");
    let moved = frame_key(&mut term, true, false, None);
    assert_eq!(
        settled.damage_epoch, moved.damage_epoch,
        "a pure caret move marks no grid damage — the premise of the latch"
    );
    assert!(
        should_repaint(Some(settled), moved),
        "the cursor terms carry the frame a torn caret owes, with no latch"
    );
}

/// Exercise the COMPLETE split compose call site, not only the pure gate:
/// after one unchanged frame early-outs, the same key must compose when a
/// recovery redraw is outstanding. Removing the recovery-aware predicate
/// from `redraw_compose` makes the final assertion fail.
#[test]
fn split_compose_recovery_bypasses_identical_key_at_shipping_callsite() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    // Split through the App so `tab_set` (which `redraw_compose` plans from) and
    // `ws.layouts` agree.
    app.split_active_stub_tab_dir(wid, crate::pane::SplitDir::Vertical);
    let (rows, cols) = {
        let ws = app.windows.get(&wid).expect("fixture window");
        (usize::from(ws.rows), usize::from(ws.cols))
    };
    let now = std::time::Instant::now();
    let args = (wid, rows, cols, false, false, None, 0, now);

    assert!(
        app.redraw_compose(
            args.0, args.1, args.2, args.3, args.4, args.5, args.6, args.7
        )
        .is_some(),
        "first split composition stamps the exact shipping repaint key"
    );
    assert!(
        app.redraw_compose(
            args.0, args.1, args.2, args.3, args.4, args.5, args.6, args.7
        )
        .is_none(),
        "an identical settled split frame takes the real early-out"
    );

    app.windows
        .get_mut(&wid)
        .unwrap()
        .present_retry
        .on_recovery_redraw_requested();
    assert!(
        app.redraw_compose(
            args.0, args.1, args.2, args.3, args.4, args.5, args.6, args.7
        )
        .is_some(),
        "the shipping split early-out must not swallow recovery"
    );
}

/// A zoomed split has one visible pane, so its live implicit background (OSC 11/DECSCNM)
/// feeds both the presented frame (padding included) and the `image` capture.
#[test]
fn zoomed_split_live_and_capture_use_the_visible_panes_effective_background() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.split_active_stub_tab(wid);
    app.toggle_pane_zoom();
    app.backend.set_pad(7);

    let terminal = app.front_terminal(wid).expect("focused pane").term.clone();
    {
        let mut term = crate::term_lock(&terminal);
        term.process(
            b"\x1b]21;cursor=\x07\x1b]10;rgb:a1/b2/c3\x07\x1b]11;rgb:12/34/56\x07\x1b[?5h",
        );
        assert_eq!(
            term.cursor_color(),
            None,
            "the fixture selects OSC 21 dynamic cursor behavior"
        );
    }
    let (rows, cols) = {
        let ws = &app.windows[&wid];
        (usize::from(ws.rows), usize::from(ws.cols))
    };
    assert!(
        app.redraw_compose(
            wid,
            rows,
            cols,
            false,
            false,
            None,
            0,
            std::time::Instant::now(),
        )
        .is_some(),
        "the zoomed compositor presents the live-color change"
    );
    assert_eq!(
        app.windows[&wid].input_scratch.default_bg, 0x00A1_B2C3,
        "DECSCNM makes OSC 10 the presented implicit/padding background"
    );
    assert_eq!(
        app.windows[&wid].input_scratch.cursor_color, 0x00A1_B2C3,
        "the split compositor resolves a dynamic cursor from live OSC 10"
    );
    assert_eq!(
        app.windows[&wid].input_scratch.cursor_trail_color, 0x00A1_B2C3,
        "the focused split wake follows the same effective cursor color"
    );

    let capture = app
        .prepare_terminal_capture_grid(wid)
        .expect("zoomed capture");
    assert!(capture.composed);
    assert_eq!(capture.leaves.len(), 1, "zoom exposes one split leaf");
    assert_eq!(
        app.windows[&wid].input_scratch.default_bg, 0x00A1_B2C3,
        "image capture uses the same visible pane ground"
    );
    assert_eq!(
        app.windows[&wid].input_scratch.cursor_color, 0x00A1_B2C3,
        "zoomed capture preserves the focused pane's dynamic cursor color"
    );
}

/// The acceptance behavior: after a present with NO subsequent terminal
/// mutation and unchanged blink/flash/selection/focus, the next redraw
/// decision is "skip" — i.e. `redraw()` would not re-rasterize.
#[test]
fn steady_screen_skips_rerasterize_after_present() {
    let mut term = Terminal::new(4, 10);
    term.process(b"steady");

    // Frame 1: present (record the key, consume the damage like redraw does).
    let k1 = frame_key(&mut term, true, false, None);
    assert!(should_repaint(None, k1), "first present");
    term.take_damage();
    let last_present = Some(k1);

    // Frame 2: nothing changed (no write, same blink/flash/selection/focus).
    let k2 = frame_key(&mut term, true, false, None);
    assert!(
        !should_repaint(last_present, k2),
        "an unchanged steady screen must skip the extract + rasterize + present"
    );
}

/// An OS appearance flip with a plain theme moves only `system_dark`, which must defeat
/// the early-out so the preview's `window_theme=auto` mock re-renders. The term comes
/// from `repaint_system_dark` (every key site's source) after the real
/// `sync_app_theme_to_appearance`. The all-window nudge is pinned by
/// `app_colorscheme::tests::appearance_flip_nudges_every_window`.
#[test]
fn os_appearance_flip_repaints() {
    let mut term = Terminal::new(4, 10);
    term.process(b"idle");
    let mut k1 = frame_key(&mut term, true, false, None);
    term.take_damage();
    let mut k2 = frame_key(&mut term, true, false, None);
    assert!(!should_repaint(Some(k1), k2), "idle screen skips");

    // The population half: a plain theme (no dark:/light: split) makes the
    // flip a theme-unchanged no-op everywhere EXCEPT the tracked appearance,
    // which must still move the key term through `repaint_system_dark`.
    let mut app = crate::App::headless_for_test();
    app.config = aterm_toml::from_str(r#"theme = "Dracula""#).expect("valid toml");
    app.os_appearance = aterm_types::Appearance::Light;
    app.theme = app.config.theme_for(aterm_types::Appearance::Light);
    k1.system_dark = crate::app_render::repaint_system_dark(app.os_appearance);
    assert!(!k1.system_dark, "light OS appearance populates false");
    app.sync_app_theme_to_appearance(aterm_types::Appearance::Dark);
    k2.system_dark = crate::app_render::repaint_system_dark(app.os_appearance);
    assert!(k2.system_dark, "the tracked flip must move the key term");
    assert!(
        should_repaint(Some(k1), k2),
        "an appearance flip must repaint"
    );
}

/// A selection change still repaints: it mutates the selection, not the grid
/// damage tracker, so only the key's selection fingerprint can carry it.
#[test]
fn a_selection_change_still_repaints() {
    let mut term = Terminal::new(4, 10);
    for _ in 0..8 {
        term.process(b"row\r\n");
    }
    let base = frame_key(&mut term, true, false, None);
    term.take_damage();
    term.text_selection_mut().start_selection(
        0,
        0,
        aterm_core::selection::SelectionSide::Left,
        aterm_core::selection::SelectionType::Simple,
    );
    let sel = frame_key(&mut term, true, false, None);
    assert!(
        should_repaint(Some(base), sel),
        "a selection change must repaint"
    );
}
