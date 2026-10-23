// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{App, Backend, RepaintKey, SelectionFingerprint, WindowId};
use crate::app_render::{project_asymmetric_pad_layout_key, should_repaint};
use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_core::terminal::{CursorStyle, Terminal};
use aterm_render::{DamageOutcome, RenderInput};

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

/// The `RepaintKey` `redraw()` builds for the current frame; presenting (`take_damage`) is the
/// caller's.
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
        drag_hover: false,
        cursor_override,
        cursor_row: term.cursor().row as usize,
        cursor_col: term.cursor().col as usize,
        cursor_visible: term.cursor_visible(),
        cursor_style: term.cursor_style(),
        glow_fp: 0,
        trail_fp: 0,
        deco_fp: 0,
        rain_fp: 0,
        streak_fp: 0,
        selection: SelectionFingerprint::of(term.text_selection()),
        pane_selection_fp: super::fold_pane_selection_fp(0, 0, term.text_selection()),
        active_pane_mark: 0,
        tab_strip: 0,
        tab_menu_fp: 0,
        settings_fp: 0,
        find_fp: 0,
        pill_fp: 0,
        preedit_fp: 0,
        scroll_frac_px: 0,
        relaunch_fp: 0,
        badge_fp: 0,
        deck_fp: 0,
        level_up_fp: 0,
        band_fp: 0,
        presence_fp: 0,
        question: (0, 0),
        system_dark: false,
        link_caption: None,
        claude_footer_fp: 0,
    }
}

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

#[test]
fn the_real_asymmetric_pad_layout_conforms() {
    crate::conformance_support::on_conformance_stack(
        "AsymmetricPadLayout",
        run_asymmetric_pad_layout_conformance,
    );
}

fn run_asymmetric_pad_layout_conformance() {
    let model = aterm_models_render::asymmetric_pad_layout_model();
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

    let mut current = cached;
    current.grid_top = changed["grid_top"] as usize;
    assert_ne!(current.grid_top, cached.grid_top);
    let repaint = should_repaint(Some(cached), current);
    assert_eq!(usize::from(!repaint), decided["cache_hit"] as usize);
    assert_eq!(usize::from(repaint), decided["full_repaint"] as usize);

    let mut mutant_without_grid_top = current;
    mutant_without_grid_top.grid_top = cached.grid_top;
    assert!(!should_repaint(Some(cached), mutant_without_grid_top));
    assert_ne!(
        usize::from(!should_repaint(Some(cached), mutant_without_grid_top)),
        decided["cache_hit"] as usize
    );
}

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

#[test]
fn a_bare_caret_move_moves_the_repaint_key() {
    let mut term = Terminal::new(4, 10);
    let settled = frame_key(&mut term, true, false, None);
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

#[test]
fn split_compose_recovery_bypasses_identical_key_at_shipping_callsite() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
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

#[test]
fn steady_screen_skips_rerasterize_after_present() {
    let mut term = Terminal::new(4, 10);
    term.process(b"steady");

    let k1 = frame_key(&mut term, true, false, None);
    assert!(should_repaint(None, k1), "first present");
    term.take_damage();
    let last_present = Some(k1);

    let k2 = frame_key(&mut term, true, false, None);
    assert!(
        !should_repaint(last_present, k2),
        "an unchanged steady screen must skip the extract + rasterize + present"
    );
}

#[test]
fn os_appearance_flip_repaints() {
    let mut term = Terminal::new(4, 10);
    term.process(b"idle");
    let mut k1 = frame_key(&mut term, true, false, None);
    term.take_damage();
    let mut k2 = frame_key(&mut term, true, false, None);
    assert!(!should_repaint(Some(k1), k2), "idle screen skips");

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

#[test]
fn a_question_posted_or_answered_alone_reaches_the_glass() {
    use super::scripted_redraw_fixture::{scripted_app, surface};
    let wid = WindowId(0);
    let mut app = scripted_app();
    app.redraw_window(wid);
    let presents = |app: &mut App, what: &str, expect: usize| {
        let before = surface(app).commits();
        app.redraw_window(wid);
        assert_eq!(surface(app).commits(), before + expect, "{what}");
    };
    presents(&mut app, "CONTROL: nothing changed, nothing presents", 0);
    let paste = |text: &str| {
        crate::paste_banner::PendingPaste::new(
            wid,
            Some(0),
            text.to_string(),
            crate::input::Source::Human,
            crate::input::PasteFraming::AtDrain,
        )
    };
    app.paste_banner = Some(paste("one\ntwo\n"));
    presents(&mut app, "the paste question appears", 1);
    presents(&mut app, "and stands", 0);
    app.paste_banner = Some(paste("three\nfour\n"));
    presents(&mut app, "a new question replaces it", 1);
    app.paste_banner = None;
    presents(&mut app, "the answer takes it away", 1);
    let prompt =
        crate::quit_safety::confirm_prompt(false, true, true).expect("a busy close prompts");
    app.close_banner = Some(crate::close_confirm::PendingClose::new(
        wid,
        prompt,
        crate::close_confirm::CloseReplay::Window(wid),
        crate::session_store::ExitActor::Human,
    ));
    presents(&mut app, "the close question appears", 1);
    app.close_banner = None;
    presents(&mut app, "and its answer takes it away", 1);
}

#[test]
fn a_question_alone_reaches_a_split_windows_glass() {
    use super::scripted_redraw_fixture::{scripted_app, surface};
    let wid = WindowId(0);
    let mut app = scripted_app();
    app.split_active_stub_tab(wid);
    assert!(app.windows[&wid].is_split(), "the window composes");
    app.redraw_window(wid);
    app.redraw_window(wid);
    let presents = |app: &mut App, what: &str, expect: usize| {
        let before = surface(app).commits();
        app.redraw_window(wid);
        assert_eq!(surface(app).commits(), before + expect, "{what}");
    };
    presents(&mut app, "CONTROL: nothing changed, nothing presents", 0);
    app.paste_banner = Some(crate::paste_banner::PendingPaste::new(
        wid,
        Some(0),
        "one\ntwo\n".to_string(),
        crate::input::Source::Human,
        crate::input::PasteFraming::AtDrain,
    ));
    presents(&mut app, "the paste question appears", 1);
    presents(&mut app, "and stands", 0);
    app.paste_banner = None;
    presents(&mut app, "the answer takes it away", 1);
    let prompt =
        crate::quit_safety::confirm_prompt(false, true, true).expect("a busy close prompts");
    app.close_banner = Some(crate::close_confirm::PendingClose::new(
        wid,
        prompt,
        crate::close_confirm::CloseReplay::Window(wid),
        crate::session_store::ExitActor::Human,
    ));
    presents(&mut app, "the close question appears", 1);
    app.close_banner = None;
    presents(&mut app, "and its answer takes it away", 1);
}
