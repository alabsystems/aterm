// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A split window renders decorations, ink, peeking cats, novas and the
//! cursor companion in every visible pane; these pin that and the per-pane
//! geometry behind it (owner, 2026-07-28).

use super::{PanePlace, seed_resident_pet_mid_flight_for_test};
use crate::{App, WindowId, term_lock};
use aterm_effects::pane::{
    translate_free_into_pane, translate_ink_into_pane, translate_nova_into_pane,
};
use std::time::{Duration, Instant};

/// Give stub terminals the headless renderer's theme background: an unset
/// default is not a certified blank perch.
fn configure_pane_backgrounds(app: &App, sid: u64) {
    let bg = aterm_render::Theme::default().bg;
    for session in [0, sid] {
        let term = &app.pool.get(session).expect("pane session").term;
        term_lock(term).set_default_background(aterm_core::terminal::Rgb {
            r: (bg >> 16) as u8,
            g: (bg >> 8) as u8,
            b: bg as u8,
        });
    }
}

fn place(col_off: u16, cols: u16) -> PanePlace {
    PanePlace {
        row_off: 0,
        col_off,
        rows: 24,
        cols,
        win_rows: 24,
        win_cols: 80,
        cell_w: 10,
        cell_h: 20,
    }
}

/// A 2-pane vertical split with freshly typed text in both panes.
///
/// The establishing compose spends whatever a never-scanned pane already
/// held (`set_presentable`'s refocus rule), so the words that must summon
/// cats are written after that first frame.
fn split_app(now: Instant) -> (App, WindowId, u64) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let sid = app.split_active_stub_tab(wid);
    configure_pane_backgrounds(&app, sid);
    app.recompute_sparkle();
    {
        let ws = app.windows.get_mut(&wid).expect("window");
        ws.focused = true;
    }
    app.redraw_compose(wid, 24, 80, false, false, None, 0, now);
    for s in [0u64, sid] {
        let term = app.pool.get(s).expect("pane session").term.clone();
        term_lock(&term).process(b"\r\nhello kitty friend\r\n");
    }
    {
        let ws = app.windows.get_mut(&wid).expect("window");
        ws.pending_deco_birth = Some(now);
    }
    (app, wid, sid)
}

/// Everything the split frame emitted, for failure messages.
fn channels(app: &App, wid: WindowId, now: Instant) -> String {
    let ws = app.windows.get(&wid).expect("window");
    format!(
        "cells={:?} sparkle_on={} deco={} ink={} free={} nova={} atlas={} active={}",
        app.win_cell_size(wid),
        app.sparkle.is_some(),
        ws.input_scratch.word_decorations.len(),
        ws.input_scratch.ink.len(),
        ws.input_scratch.free_sprites.len(),
        ws.input_scratch.nova_add.len(),
        ws.input_scratch.free_atlas.is_some(),
        ws.fx.decos().is_active(now),
    )
}

fn compose(app: &mut App, wid: WindowId, now: Instant) -> Option<std::sync::Arc<str>> {
    app.redraw_compose(wid, 24, 80, false, false, None, 0, now)
}

/// A split frame emits peeking cats on both sides of the divider.
#[test]
fn split_compose_emits_cats_in_every_visible_pane() {
    let t0 = Instant::now();
    let (mut app, wid, _) = split_app(t0);
    assert!(
        compose(&mut app, wid, t0).is_some(),
        "the split frame presents"
    );
    // The peek plays over ~600 ms; sample a later frame like the engine
    // batteries do.
    let t1 = t0 + Duration::from_millis(600);
    compose(&mut app, wid, t1);
    let diag = channels(&app, wid, t1);
    let ws = app.windows.get(&wid).expect("window");
    let (cw, _) = app.win_cell_size(wid);
    assert!(
        !ws.input_scratch.free_sprites.is_empty(),
        "a split window draws cats ({diag})"
    );
    assert!(
        ws.input_scratch.free_atlas.is_some(),
        "and ships the atlas they address"
    );
    let divider_x = 40 * cw as i32;
    assert!(
        ws.input_scratch
            .free_sprites
            .iter()
            .any(|s| s.x < divider_x),
        "cats in the LEFT pane"
    );
    assert!(
        ws.input_scratch
            .free_sprites
            .iter()
            .any(|s| s.x >= (41 * cw) as i32),
        "and cats in the RIGHT pane — the fix is not the focused pane only"
    );
}

/// The merged ink stream satisfies `ink_row_slice`'s contract: non-empty and
/// strictly increasing by `(row, col)`, which a pane-major concatenation of
/// a vertical split is not.
#[test]
fn split_compose_ink_is_non_empty_and_sorted() {
    let t0 = Instant::now();
    let (mut app, wid, _) = split_app(t0);
    compose(&mut app, wid, t0);
    compose(&mut app, wid, t0 + Duration::from_millis(600));
    let diag = channels(&app, wid, t0);
    let ws = app.windows.get(&wid).expect("window");
    let ink = &ws.input_scratch.ink;
    assert!(
        !ink.is_empty(),
        "a split window paints animated ink ({diag})"
    );
    assert!(
        ink.windows(2)
            .all(|w| (w[0].row, w[0].col) < (w[1].row, w[1].col)),
        "ink must be sorted (row, col) and unique: {:?}",
        ink.iter().map(|c| (c.row, c.col)).collect::<Vec<_>>()
    );
}

// Not tested: that `deco_fp` alone repaints an animation-only frame. The
// term is needed (an animating cat damages no cell), but with the clock
// advancing other `now`-derived key terms present every frame, so such a
// test would pass vacuously.

/// T4 — without the cadence a correct emitter renders one frame and freezes.
#[test]
fn split_layout_arms_the_effect_frame_cadence() {
    let t0 = Instant::now();
    let (mut app, wid, _) = split_app(t0);
    compose(&mut app, wid, t0);
    let t1 = t0 + Duration::from_millis(100);
    compose(&mut app, wid, t1);
    let diag = channels(&app, wid, t1);
    let ws = app.windows.get(&wid).expect("window");
    assert!(ws.is_split(), "the harness really is split");
    assert!(
        ws.cursor_dependents_need_frame_cadence(t1, true),
        "a split layout keeps the sparkle engine's frame cadence armed ({diag})"
    );
}

/// One companion per window, riding the focused (right) pane's caret and
/// translated into that pane's window coords. Sparkle Words is off, so the
/// only sprites are the companion's, summoned by `on_collect`; an
/// untranslated companion would land in the left pane.
#[test]
fn split_companion_rides_only_the_focused_pane() {
    let t0 = Instant::now();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    // Enable the companion explicitly; its default is off on Windows.
    app.config.cursor_trail = Some(true);
    let sid = app.split_active_stub_tab(wid);
    configure_pane_backgrounds(&app, sid);
    app.recompute_sparkle();
    assert!(
        app.sparkle.take().is_some(),
        "negative control disables a live feed"
    );
    {
        let ws = app.windows.get_mut(&wid).expect("window");
        ws.focused = true;
        ws.fx
            .companions_mut()
            .cursor_cat
            .on_collect(t0, aterm_effects::kitty_registry::KittyLook::default());
    }
    let focus = app
        .active_tree(wid)
        .map(crate::pane::PaneTree::focus)
        .expect("a focused pane");
    assert_eq!(focus, sid, "the fresh split focuses the NEW (right) pane");
    compose(&mut app, wid, t0);
    let t1 = t0 + Duration::from_millis(120);
    compose(&mut app, wid, t1);
    let (cw, _) = app.win_cell_size(wid);
    let diag = channels(&app, wid, t1);
    let ws = app.windows.get(&wid).expect("window");
    let sprites = &ws.input_scratch.free_sprites;
    assert!(
        !sprites.is_empty(),
        "the summoned companion is drawn on a split frame ({diag})"
    );
    // The right pane starts at col 41 (col 40 is the divider).
    let pane_x0 = (41 * cw) as i32;
    assert!(
        sprites.iter().all(|s| s.x >= pane_x0),
        "every companion sprite sits in the FOCUSED pane, not at un-shifted \
         pane-local coords: {:?} (pane starts at x={pane_x0})",
        sprites.iter().map(|s| (s.x, s.y, s.w)).collect::<Vec<_>>()
    );
}

/// A hidden caret is not a coordinate: the resident pet keeps its last body
/// on glass for a bounded fade, but a program relocating the raw cursor in
/// the same DECTCEM-off batch does not turn into a word claim, and the
/// settled ambient kitty keeps peeking.
#[test]
fn split_present_hidden_pet_fade_does_not_claim_program_relocated_word() {
    let t0 = Instant::now();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let focus = app.split_active_stub_tab(wid);
    configure_pane_backgrounds(&app, focus);
    // The resident is the subject, so seat it explicitly: `cursor_trail`'s
    // default is platform-split ([`crate::app_config::DEFAULT_DECORATIVE_EFFECTS`],
    // off on Windows).
    app.config.cursor_trail = Some(true);
    app.recompute_sparkle();
    app.windows.get_mut(&wid).expect("window").focused = true;

    // Establish the per-pane scanner and the resident pet at a bare caret in
    // the focused (right) pane.
    assert!(compose(&mut app, wid, t0).is_some());
    let term = app.pool.get(focus).expect("focused pane").term.clone();
    // Put an ambient word far from the pet, then return the visible caret to
    // a bare cell. It must first settle visibly, proving the negative arm.
    term_lock(&term).process(b"\x1b[11;3Hkitty\x1b[4;31H");
    assert!(compose(&mut app, wid, t0 + Duration::from_millis(16)).is_some());
    assert!(compose(&mut app, wid, t0 + Duration::from_millis(616)).is_some());
    let ambient_before = app.windows[&wid]
        .input_scratch
        .free_sprites
        .iter()
        .filter(|sprite| matches!(sprite.z, aterm_core::render::FreeZ::UnderText))
        .count();
    assert_eq!(
        ambient_before,
        1,
        "negative control: the far word owns one settled ambient kitty ({})",
        channels(&app, wid, t0 + Duration::from_millis(616))
    );

    // Hide and move the program cursor onto that word. The pet still fades
    // at its old body, but no honest visible caret exists at the new cell.
    term_lock(&term).process(b"\x1b[?25l\x1b[11;3H");
    assert!(compose(&mut app, wid, t0 + Duration::from_millis(632)).is_some());
    {
        let term = term_lock(&term);
        assert!(!term.cursor_visible());
        assert_eq!((term.cursor().row, term.cursor().col), (10, 2));
    }
    let ws = &app.windows[&wid];
    let ambient_after = ws
        .input_scratch
        .free_sprites
        .iter()
        .filter(|sprite| matches!(sprite.z, aterm_core::render::FreeZ::UnderText))
        .count();
    let resident_after = ws
        .input_scratch
        .free_sprites
        .iter()
        .filter(|sprite| matches!(sprite.z, aterm_core::render::FreeZ::OverText))
        .count();
    assert!(
        resident_after >= 1 && ws.fx.companion().brain().is_active(),
        "the hidden-caret frame genuinely carries the fading resident body"
    );
    assert_eq!(
        ambient_after, ambient_before,
        "the invisible program-relocated cursor cannot suppress an unrelated word-cat"
    );
}

/// The pet's drawn body, `(x0, x1, y0, y1)` frame px.
type BodyPx = (i32, i32, i32, i32);

/// The resident mid-flight, its caret painted, then ONE present with or
/// without a DECTCEM hide, then the caret shown again: the drawn body
/// `[before, at the hide, back]` and the cell size.
fn split_midflight_pet_through_a_hide(hide: bool) -> ([BodyPx; 3], (i32, i32)) {
    let t0 = Instant::now();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let focus = app.split_active_stub_tab(wid);
    configure_pane_backgrounds(&app, focus);
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("rainbow kitty pet".into());
    app.config.motion = Some("full".into());
    app.recompute_sparkle();
    app.sparkle = None;
    app.windows.get_mut(&wid).expect("window").focused = true;
    assert!(compose(&mut app, wid, t0).is_some());

    let (pane_rows, pane_cols) = {
        let ws = &app.windows[&wid];
        let tree = &ws.layouts[ws.tabs.active];
        let rect = tree
            .compute_layout(ws.rows, ws.cols)
            .into_iter()
            .find(|rect| rect.session == focus)
            .expect("focused pane layout");
        (rect.rows, rect.cols)
    };
    let (cw, ch) = app.win_cell_size(wid);
    let target = (2, 24);
    let (flight_at, flying) = {
        let ws = app.windows.get_mut(&wid).expect("window");
        seed_resident_pet_mid_flight_for_test(
            ws,
            t0 + Duration::from_millis(16),
            pane_rows,
            pane_cols,
            cw as u16,
            ch as u16,
            target,
        )
    };
    let width =
        aterm_effects::kitty_pet::ART_ROWS * ch as f32 * aterm_effects::kitty_pet::ART_ASPECT
            / cw as f32;
    let target_col = aterm_effects::kitty_pet::PetBrain::station(target.1, pane_cols, width);
    assert!(
        (flying.col - target_col).abs() > 10.0,
        "negative control: the fixture is visibly mid-flight"
    );

    let term = app.pool.get(focus).expect("focused pane").term.clone();
    term_lock(&term).process(b"\x1b[3;25H\x1b[?25h");
    let visible_at = flight_at + Duration::from_millis(16);
    assert!(compose(&mut app, wid, visible_at).is_some());
    let before = app.windows[&wid]
        .fx
        .companion()
        .hit_rect()
        .expect("production present draws the mid-flight resident");

    if hide {
        term_lock(&term).process(b"\x1b[?25l");
    }
    let hidden_at = visible_at + Duration::from_millis(16);
    assert!(compose(&mut app, wid, hidden_at).is_some());
    assert_eq!(!term_lock(&term).cursor_visible(), hide);
    let hidden = app.windows[&wid]
        .fx
        .companion()
        .hit_rect()
        .expect("the hidden frame keeps the resident on glass");
    assert!(
        app.windows[&wid]
            .input_scratch
            .free_sprites
            .iter()
            .any(|sprite| matches!(sprite.z, aterm_core::render::FreeZ::OverText)),
        "the inspected frame really emitted the resident body"
    );

    term_lock(&term).process(b"\x1b[?25h");
    let returned_at = hidden_at + Duration::from_millis(16);
    assert!(compose(&mut app, wid, returned_at).is_some());
    let returned = app.windows[&wid]
        .fx
        .companion()
        .hit_rect()
        .expect("quick return keeps the resident on glass");
    ([before, hidden, returned], (cw as i32, ch as i32))
}

/// A DECTCEM hide inside the repaint hold (`HIDDEN_HOLD_MAX`) is no event
/// for a pet in the air: the brain reads the hidden caret as the caret it
/// last saw painted, so the flight goes on to land where it was going,
/// drawn exactly as its painted twin draws it, and the caret's quick
/// return walks on from there. RULE CHANGED 2026-09-29: this used to pin
/// the body STILL on the hidden frame (feet within 1 px), which the old
/// hold got by re-seating the pet from its drawn rect, freezing it in
/// mid-air with the flight's lift baked into its feet.
#[test]
fn split_present_holds_a_hidden_midflight_pet_and_quick_return_walks() {
    let ([before, painted, _], _) = split_midflight_pet_through_a_hide(false);
    let ([before_hidden, hidden, returned], (cw, ch)) = split_midflight_pet_through_a_hide(true);
    assert_eq!(before_hidden, before, "the twins start from one frame");
    assert_ne!(painted, before, "control: the painted twin is still flying");
    assert_eq!(
        hidden, painted,
        "a repaint's hide changed the flight: {before:?} -> {hidden:?}, painted {painted:?}"
    );
    assert!(
        (returned.0 - hidden.0).abs() <= 2 * cw && (returned.3 - hidden.3).abs() <= ch,
        "quick return jumped the body: {hidden:?} -> {returned:?}"
    );
}

/// A SPLIT-PANE FOCUS SWITCH CROSSFADES ACROSS PANES — the composed
/// host's half of `PetSurfaceSeam`'s `Shift` (the owner's half is
/// aterm-effects' `pet_coordinate_seams.rs`). The resident settles in the
/// right pane; the focus moves left through the production verb, and the
/// layout fence runs as the redraw entry runs it. On the very next frame
/// the arriver is drawn in the LEFT pane while the old body is drawn as a
/// ghost in the RIGHT pane, at the window pixels it was drawn at and
/// beneath that pane's text: the host offered the right pane as glass
/// and the ghost reached the window's channel, not the focused pane's
/// staging, which would have cropped it away at the divider. A quarter
/// second later the crossfade is spent: nothing of the pet is left in
/// the right pane. Before this fix that frame drew nothing in the right
/// pane, and the arriver alone faded in from nothing.
#[test]
fn split_focus_switch_fades_the_old_body_in_its_own_pane() {
    let t0 = Instant::now();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let right = app.split_active_stub_tab(wid);
    configure_pane_backgrounds(&app, right);
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("rainbow kitty pet".into());
    app.config.motion = Some("full".into());
    app.recompute_sparkle();
    // Only the companion draws free sprites.
    app.sparkle = None;
    app.windows.get_mut(&wid).expect("window").focused = true;
    let (cw, _) = app.win_cell_size(wid);
    let (left_x1, right_x0) = ((40 * cw) as i32, (41 * cw) as i32);
    let body = |app: &App| {
        app.windows[&wid]
            .input_scratch
            .free_sprites
            .iter()
            .copied()
            .max_by_key(|s| u32::from(s.w) * u32::from(s.h))
    };
    // A frame with nothing new to show early-outs (`None`), keeping the
    // last presented one in `input_scratch`.
    let mut now = t0;
    for _ in 0..200 {
        compose(&mut app, wid, now);
        now += Duration::from_millis(16);
    }
    let before = body(&app).expect("fixture: the resident settled on glass");
    assert!(
        before.x >= right_x0 && app.windows[&wid].fx.companion().alpha() == 255,
        "fixture: the resident stands in the focused right pane: {before:?}"
    );

    assert!(app.focus_pane_in(wid, crate::pane::FocusDir::Left));
    let route = app.active_visible_content_route(wid).expect("route");
    assert!(app.prepare_layout_coordinate_space(wid, route));
    assert!(compose(&mut app, wid, now).is_some());
    let sprites = app.windows[&wid].input_scratch.free_sprites.clone();
    let ghost = sprites
        .iter()
        .find(|s| s.x >= right_x0)
        .unwrap_or_else(|| panic!("the old body fades in its own pane: {sprites:?}"));
    assert_eq!(ghost.z, aterm_core::render::FreeZ::UnderText, "{ghost:?}");
    assert!(
        (ghost.x - before.x).abs() <= 1
            && (ghost.y + i32::from(ghost.h) - before.y - i32::from(before.h)).abs() <= 1,
        "…where the body was drawn: {ghost:?} vs {before:?}"
    );
    assert!(
        sprites.iter().any(|s| s.x + i32::from(s.w) <= left_x1),
        "the arriver is drawn in the newly focused left pane: {sprites:?}"
    );

    for _ in 0..30 {
        now += Duration::from_millis(16);
        compose(&mut app, wid, now);
    }
    let ws = &app.windows[&wid];
    assert!(
        ws.input_scratch
            .free_sprites
            .iter()
            .all(|s| s.x + i32::from(s.w) <= left_x1),
        "the crossfade is spent: {:?}",
        ws.input_scratch.free_sprites
    );
    assert_eq!(ws.fx.companion().alpha(), 255, "the arriver is full");
}

/// T6 — under per-pane state a split must not wipe the untouched pane's
/// episodes (the old blanket `reset()` did exactly that).
#[test]
fn splitting_does_not_reset_the_untouched_panes_episodes() {
    let t0 = Instant::now();
    let (mut app, wid, _) = split_app(t0);
    compose(&mut app, wid, t0);
    let diag = channels(&app, wid, t0);
    assert!(
        app.windows
            .get(&wid)
            .is_some_and(|ws| ws.fx.decos().is_active(t0)),
        "the composed frame established live episodes ({diag})"
    );
    // A pane-space mutation now only EVICTS departed panes.
    app.reset_pane_space_decorations(wid);
    assert!(
        app.windows
            .get(&wid)
            .is_some_and(|ws| ws.fx.decos().is_active(t0)),
        "and both still-visible panes keep theirs"
    );
}

/// A sprite straddling an interior pane edge is cropped in dest and, in
/// lockstep, in its atlas source rect (nearest 1:1); one wholly outside is
/// dropped; a pane at the window edge keeps its overhang; nothing
/// underflows at offset 0.
#[test]
fn translate_free_sprites_into_pane_crops_source_and_dest() {
    let sprite = |x: i32, y: i32, w: u16, h: u16| aterm_core::render::FreeSprite {
        x,
        y,
        w,
        h,
        ax: 100,
        ay: 200,
        aw: w,
        ah: h,
        tint: 0x00FF_FFFF,
        alpha: 255,
        flip_x: false,
        z: aterm_core::render::FreeZ::UnderText,
        sampler: aterm_core::render::FreeSampler::Nearest,
    };
    // Left pane (cols 0..40): a sprite straddling the divider at x = 400.
    let mut free = vec![sprite(380, 40, 40, 20)];
    translate_free_into_pane(&mut free, place(0, 40));
    assert_eq!(free.len(), 1, "the visible half survives");
    assert_eq!(
        (free[0].x, free[0].w),
        (380, 20),
        "dest cropped at the divider"
    );
    assert_eq!(
        (free[0].ax, free[0].aw),
        (100, 20),
        "and the atlas source rect cropped in lockstep"
    );
    assert_eq!(free[0].ay, 200, "an untouched axis keeps its texel origin");

    // Right pane (col_off 41): the shift lands it past the divider, and its
    // left overhang is cropped there.
    let mut free = vec![sprite(-30, 0, 40, 20)];
    translate_free_into_pane(&mut free, place(41, 39));
    assert_eq!(free[0].x, 410, "clipped to the pane's own left edge");
    assert_eq!((free[0].w, free[0].aw), (10, 10), "dest and source agree");
    assert_eq!(free[0].ax, 100 + 30, "the cropped-away texels are skipped");

    // A sprite wholly inside the neighbour is dropped, not wrapped.
    let mut free = vec![sprite(600, 0, 20, 20)];
    translate_free_into_pane(&mut free, place(0, 40));
    assert!(free.is_empty(), "nothing leaks across a divider");

    // Window-edge overhang survives: the leftmost pane owns the left pad,
    // and `col_off == 0` must not underflow the shift.
    let mut free = vec![sprite(-8, -6, 20, 20)];
    translate_free_into_pane(&mut free, place(0, 40));
    assert_eq!(
        (free[0].x, free[0].y, free[0].w, free[0].h),
        (-8, -6, 20, 20),
        "an edge pane keeps its off-grid peek, untouched"
    );
}

/// The cursor companion ships a pose-scaled destination in `w/h` and the
/// natural art size in `aw/ah`, so at an interior pane edge it is cropped
/// proportionally rather than dropped; the crop reduces to texel-for-texel
/// at 1:1.
#[test]
fn a_scaled_companion_is_cropped_at_a_divider_not_deleted() {
    // Dest 40x20 drawn from a 20x10 atlas tile: a 2x upscale.
    let companion = aterm_core::render::FreeSprite {
        x: 380,
        y: 40,
        w: 40,
        h: 20,
        ax: 100,
        ay: 200,
        aw: 20,
        ah: 10,
        tint: 0x00FF_FFFF,
        alpha: 255,
        flip_x: false,
        z: aterm_core::render::FreeZ::OverText,
        sampler: aterm_core::render::FreeSampler::Nearest,
    };
    let mut free = vec![companion];
    translate_free_into_pane(&mut free, place(0, 40));
    assert_eq!(
        free.len(),
        1,
        "the companion SURVIVES the divider — dropping it is the bug"
    );
    // Half the dest is cropped away, so half the SOURCE must be too — 10 of
    // the 20 atlas texels, not the 20 dest pixels a 1:1 crop would take.
    assert_eq!((free[0].x, free[0].w), (380, 20), "dest cropped at 400");
    assert_eq!(
        (free[0].ax, free[0].aw),
        (100, 10),
        "source cropped PROPORTIONALLY (aw/w = 1/2), not texel-for-pixel"
    );
    assert_eq!(
        (free[0].ay, free[0].ah),
        (200, 10),
        "the untouched axis keeps its whole source span"
    );

    // A sliver thinner than the scale factor must still select a texel: an
    // empty source rect is a sprite the backends have nothing to sample.
    let mut sliver = vec![aterm_core::render::FreeSprite {
        x: 399,
        ..companion
    }];
    translate_free_into_pane(&mut sliver, place(0, 40));
    assert_eq!(sliver.len(), 1);
    assert!(
        sliver[0].aw >= 1 && sliver[0].ah >= 1,
        "a cropped source never collapses to zero texels, got {}x{}",
        sliver[0].aw,
        sliver[0].ah
    );
}

/// The ink/nova translators shift into window coords and clip at interior
/// dividers — the cell-addressed and u16-pixel twins of T15.
#[test]
fn translate_ink_and_nova_shift_and_clip_at_the_divider() {
    let mut ink = vec![
        aterm_render::InkCell {
            row: 1,
            col: 3,
            color: [1, 2, 3],
        },
        aterm_render::InkCell {
            row: 1,
            col: 99,
            color: [4, 5, 6],
        },
    ];
    translate_ink_into_pane(&mut ink, place(41, 39));
    assert_eq!(ink.len(), 1, "a cell outside the pane is dropped");
    assert_eq!((ink[0].row, ink[0].col), (1, 44), "and the rest shifts");

    let mut nova = vec![aterm_render::GlowQuad {
        row: 2,
        x: 390,
        y: 40,
        w: 30,
        h: 10,
        color: 0x0011_2233,
        // Additive light (see `GlowQuad::alpha`).
        alpha: 0,
        color2: 0x0011_2233,
        alpha2: 0,
    }];
    translate_nova_into_pane(&mut nova, place(0, 40));
    assert_eq!(
        (nova[0].x, nova[0].w),
        (390, 10),
        "nova light stops at the divider"
    );
    assert_eq!(nova[0].row, 2, "a top pane's row hint is unshifted");
}
