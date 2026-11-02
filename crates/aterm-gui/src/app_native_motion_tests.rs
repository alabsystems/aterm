// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native-page motion (`docs/DESIGN-init-on-by-default-2026-10-25.md` §5.1, §5.5, §5.6): moving
//! nodes, the page's motion policy, and the host's motion tick, on the System State page's
//! working spinner.

use std::time::{Duration, Instant};

use aterm_messages::motion::Cadence;

use super::*;
use crate::app_native::with_native_preview_instant_for_test;
use crate::native_app::{NativeAppModel, ViewCx, ViewMotionCx};
use crate::native_ui::{
    AuditedCustomNode, CompiledUi, GroupSpec, Layout, Length, LogicalRect, NodeMotion,
    SYSTEM_STATE_MARK_AUDIT, SemanticRole, UiContent, UiKey, UiNode, UiTree,
};
use crate::widget::DrawPrim;

fn spinner(key: &str, motion: Option<NodeMotion>) -> UiNode {
    UiNode::new(
        key,
        UiContent::Custom(AuditedCustomNode {
            audit_id: SYSTEM_STATE_MARK_AUDIT,
            role: SemanticRole::Group,
            label: String::new(),
            paint: Some(crate::system_state::working_spinner()),
            action: None,
            focusable: false,
            focused: false,
            focus_visible: false,
            progress: None,
            motion,
        }),
    )
    .layout(
        Layout::default()
            .width(Length::Fixed(16.0))
            .height(Length::Fixed(16.0)),
    )
    .paint_only()
}

fn page_of(children: Vec<UiNode>) -> CompiledUi {
    UiTree::new(
        UiNode::new(
            "page",
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
        )
        .layout(Layout::column().gap(4.0))
        .children(children),
    )
    .compile(LogicalRect::new(0.0, 0.0, 200.0, 100.0))
    .expect("the page compiles")
}

/// The prims a tray draws for the node at paint index `index`, at `t_ms`: what follows its
/// clip, up to the clip's pop.
fn node_prims(compiled: &CompiledUi, index: usize, t_ms: u64) -> Vec<String> {
    let tray = compiled.tray_with_chrome(
        aterm_render::Theme::default(),
        aterm_render::Theme::default(),
        13.0,
        t_ms,
    );
    let mut depth = 0usize;
    let mut node = 0usize;
    let mut out = Vec::new();
    for prim in &tray.prims {
        match prim {
            DrawPrim::ClipPush { .. } => depth += 1,
            DrawPrim::ClipPop => {
                depth -= 1;
                if depth == 0 {
                    node += 1;
                }
            }
            other if node == index => out.push(format!("{other:?}")),
            _ => {}
        }
    }
    out
}

/// A MOVING NODE IS LISTED AND DRAWN AT THE MOTION CLOCK: the compiled tree lists the one node
/// that moves (and not the still one beside it), its painter draws a new frame when the clock
/// crosses a 12 Hz step and the same frame within one, and its prims are clipped to its own
/// rect, so a frame of it repaints those pixels alone. NEGATIVE CONTROL: the still spinner draws
/// the same glyph at every time.
#[test]
fn a_moving_node_is_listed_and_drawn_at_the_motion_clock() {
    let compiled = page_of(vec![
        spinner("still", None),
        spinner("moving", Some(NodeMotion::endless(0, Cadence::SPINNER))),
    ]);
    let moving = compiled
        .paint
        .iter()
        .position(|node| node.key == UiKey::new("moving"))
        .expect("the moving node paints");
    let still = compiled
        .paint
        .iter()
        .position(|node| node.key == UiKey::new("still"))
        .expect("the still node paints");
    assert_eq!(compiled.motion, [moving], "only the moving node is listed");
    // One frame lasts 1000/12 ms: 10 ms and 80 ms share frame 0, 90 ms is frame 1.
    assert_eq!(
        node_prims(&compiled, moving, 10),
        node_prims(&compiled, moving, 80)
    );
    assert_ne!(
        node_prims(&compiled, moving, 10),
        node_prims(&compiled, moving, 90)
    );
    assert_eq!(
        node_prims(&compiled, still, 10),
        node_prims(&compiled, still, 90)
    );
    let tray = compiled.tray_with_chrome(
        aterm_render::Theme::default(),
        aterm_render::Theme::default(),
        13.0,
        0,
    );
    let node = &compiled.paint[moving];
    let rect = node.motion_rect().expect("the node is on the page");
    assert!(
        tray.prims.iter().any(|prim| matches!(prim,
            DrawPrim::ClipPush { x, y, w, h }
                if (*x, *y, *w, *h) == (rect.x, rect.y, rect.width, rect.height))),
        "the moving node's prims are clipped to its rect"
    );
}

/// A MOTION WITH AN END HOLDS ITS LAST FRAME AND OWES NO WAKE AFTER IT, and an endless one always
/// owes its next frame after the one painted.
#[test]
fn a_motion_with_an_end_holds_its_last_frame_and_owes_no_wake_after_it() {
    let ends = NodeMotion {
        start_ms: 1_000,
        until_ms: Some(1_250),
        cadence: Cadence::hz(10),
    };
    assert_eq!(ends.frame_at(0), 0, "frame 0 before its start");
    assert_eq!(ends.frame_at(1_100), 1);
    assert_eq!(ends.frame_at(1_250), 2);
    assert_eq!(ends.frame_at(9_000), 2, "the last frame holds");
    assert_eq!(ends.next_frame_after(1_100), Some(1_200));
    // Frame 2 shows at 1200 ms; nothing after it changes, so no wake is owed.
    assert_eq!(ends.next_frame_after(1_200), None);
    assert_eq!(ends.next_frame_after(5_000), None);
    let endless = NodeMotion::endless(0, Cadence::SPINNER);
    assert_eq!(endless.next_frame_after(0), Some(84));
    assert_eq!(endless.next_frame_after(1_000_000), Some(1_000_084));
}

fn running_page(motion: ViewMotionCx) -> CompiledUi {
    let mut app =
        crate::system_state::SystemStateApp::new(aterm_types::macos_setup::Plan::default());
    app.publish(crate::system_state::fixture_status("running").expect("a scenario"));
    let view = crate::system_state::SystemStateViewState::default();
    let viewport = LogicalRect::new(0.0, 0.0, 999.0, 1400.0);
    let cx = ViewCx {
        viewport,
        animation_phase_ms: 0,
        motion,
        terminal_font_px: 13.0,
        terminal_scale: 2.0,
        semantic_font: None,
        document: None,
    };
    app.view(&view, &cx)
        .compile(viewport)
        .expect("the page compiles")
}

fn spinners(page: &CompiledUi) -> Vec<(UiKey, Option<NodeMotion>)> {
    page.paint
        .iter()
        .filter(|node| {
            matches!(&node.content, UiContent::Custom(custom)
                if custom.audit_id == SYSTEM_STATE_MARK_AUDIT
                    && custom.paint.as_deref().is_some_and(|mark| mark.starts_with("working,")))
        })
        .map(|node| (node.key.clone(), node.motion()))
        .collect()
}

/// THE WORKING SPINNER STEPS WHERE THE PAGE MAY MOVE IN PLACE AND STANDS STILL OTHERWISE (§5.5): a
/// running row's spinner is a moving node at 12 Hz in full motion and under Reduce Motion
/// (rotation in place is not travel), and a still glyph under Serious Mode, the shed, Low Power
/// or in an unfocused window. The person's `motion = full` overrides the shed and Low Power as
/// it does everywhere. It is paint only, so a screen reader never meets it, moving or not.
#[test]
fn the_working_spinner_steps_where_the_page_may_move_in_place_and_stands_still_otherwise() {
    let full = ViewMotionCx::default();
    let moving = spinners(&running_page(full));
    assert!(!moving.is_empty(), "the running Dock row shows a spinner");
    assert!(
        moving
            .iter()
            .all(|(_, motion)| *motion == Some(NodeMotion::endless(0, Cadence::SPINNER))),
        "{moving:?}"
    );
    let reduced = ViewMotionCx {
        system_reduced: true,
        ..full
    };
    assert_eq!(
        spinners(&running_page(reduced)),
        moving,
        "Reduce Motion keeps it"
    );
    for (why, quiet) in [
        (
            "serious",
            ViewMotionCx {
                serious: true,
                ..full
            },
        ),
        ("shed", ViewMotionCx { shed: true, ..full }),
        (
            "low power",
            ViewMotionCx {
                low_power: true,
                ..full
            },
        ),
        (
            "unfocused",
            ViewMotionCx {
                window_focused: false,
                ..full
            },
        ),
    ] {
        let still = spinners(&running_page(quiet));
        assert_eq!(still.len(), moving.len(), "{why}: the glyph stays");
        assert!(
            still.iter().all(|(_, motion)| motion.is_none()),
            "{why}: {still:?}"
        );
    }
    let insisted = ViewMotionCx {
        mode: crate::motion::MotionMode::Full,
        shed: true,
        low_power: true,
        ..full
    };
    assert_eq!(spinners(&running_page(insisted)), moving, "motion = full");
    let page = running_page(full);
    for (key, _) in &moving {
        assert!(page.semantic(key).is_none(), "{key:?} is never spoken");
    }
}

/// An app showing the running page in a focused window with glass, its first frame painted at
/// `t0`.
fn app_with_a_running_page(t0: Instant) -> (App, WindowId, crate::tab_model::ViewId) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    {
        let window = app.windows.get_mut(&wid).unwrap();
        window.rows = 90;
        window.cols = 160;
        window.os_window_for_test = true;
        window.focused = true;
    }
    assert!(app.open_system_state_tab());
    let (_, view) = app.active_native_view(wid).expect("the page is in front");
    app.receive_system_state_status(
        Some(app.system_state_host.status_generation_for_test()),
        crate::system_state::fixture_status("running").expect("a scenario"),
    );
    assert!(with_native_preview_instant_for_test(t0, || {
        app.prepare_native_input_scratch(wid)
    }));
    (app, wid, view)
}

fn raster(app: &App, wid: WindowId, view: crate::tab_model::ViewId) -> &crate::NativeLeafRaster {
    app.windows[&wid].leaf_render_cache[&view]
        .native
        .as_ref()
        .expect("a retained raster")
}

fn motion_ms(app: &App, at: Instant) -> u64 {
    u64::try_from(at.saturating_duration_since(app.lat_epoch).as_millis()).unwrap()
}

/// An OS policy notification forces a fresh sample even while the page is quiet and its
/// fallback sample is still recent. The headless runtime's inert probe says unconstrained;
/// clearing the cached restriction recompiles the still glyph into a moving spinner.
#[cfg(any(target_os = "macos", windows))]
#[test]
fn a_policy_notification_resumes_a_quieted_page_without_a_motion_wake() {
    let t0 = Instant::now() + Duration::from_secs(1);
    let (mut app, wid, view) = app_with_a_running_page(t0);
    app.low_power = true;
    app.low_power_sampled_at = Some(t0);
    assert!(with_native_preview_instant_for_test(t0, || {
        app.prepare_native_input_scratch(wid)
    }));
    assert!(raster(&app, wid, view).compiled.motion.is_empty());
    assert_eq!(app.next_native_motion_tick(wid), None);

    // A motion-tick sample is throttled and cannot be the recovery mechanism.
    assert!(!app.resample_low_power(t0, false));
    assert!(app.low_power);
    app.resample_os_preferences(crate::ResampleCause::SettingsBurst);
    assert!(
        !app.low_power,
        "the notification bypasses the fallback interval"
    );
    assert!(with_native_preview_instant_for_test(t0, || {
        app.prepare_native_input_scratch(wid)
    }));
    assert!(!raster(&app, wid, view).compiled.motion.is_empty());
    assert!(app.next_native_motion_tick(wid).is_some());
}

/// A SEEN RUNNING PAGE WAKES FOR EACH SPINNER STEP AND REPAINTS THE SPINNER ALONE, PAINT-ONLY; A
/// SETTLED PAGE OWES NO WAKE (§5.1, §5.6, the positive control and its zero). The window's motion
/// tick is the spinner's next 12 Hz step after the frame painted; the frame that follows it is a
/// motion frame: no compile, no content raster, the spinners' rects alone rastered at the
/// clock's time, and the window's card patched in place (a new serial, the rows it changed named
/// to the GPU arm, the same pixels a fresh copy of the raster holds), with no change to what the
/// page says. Once the run ends the page shows no spinner and the window owes no motion wake.
/// NEGATIVE CONTROL: a tick served within the frame already painted owes nothing.
#[test]
fn a_seen_running_page_wakes_for_each_spinner_step_and_a_settled_page_owes_none() {
    let t0 = Instant::now() + Duration::from_secs(1);
    let (mut app, wid, view) = app_with_a_running_page(t0);
    let painted = raster(&app, wid, view).motion_ms;
    assert_eq!(painted, motion_ms(&app, t0));
    let moving = raster(&app, wid, view).compiled.motion.clone();
    assert!(
        !moving.is_empty(),
        "the running page shows a moving spinner"
    );
    let due = app
        .next_native_motion_tick(wid)
        .expect("a moving spinner owes its next step");
    let step = NodeMotion::endless(0, Cadence::SPINNER)
        .next_frame_after(painted)
        .unwrap();
    assert_eq!(motion_ms(&app, due), step, "the next 12 Hz step");
    assert!(due > t0 && due <= t0 + Duration::from_millis(84));

    // Within the painted frame, nothing is owed.
    assert!(!with_native_preview_instant_for_test(t0, || {
        app.native_motion_due(wid, t0)
    }));
    let semantics_before = raster(&app, wid, view).compiled.semantics.clone();
    assert!(with_native_preview_instant_for_test(due, || {
        app.native_motion_due(wid, due)
    }));
    let compiled = raster(&app, wid, view).compiled.clone();
    let spinner_rect = moving
        .iter()
        .filter_map(|&index| compiled.paint[index].motion_rect())
        .reduce(|a, b| {
            let x = a.x.min(b.x);
            let y = a.y.min(b.y);
            LogicalRect::new(
                x,
                y,
                a.right().max(b.right()) - x,
                a.bottom().max(b.bottom()) - y,
            )
        })
        .unwrap();
    assert_eq!(
        app.windows[&wid].leaf_render_cache[&view].native_damage, None,
        "a motion tick marks no content damage"
    );
    let card_before = app.windows[&wid].settings_card.as_ref().unwrap().serial;
    let (full_before, regional_before) = {
        let before = raster(&app, wid, view);
        (before.full_rasters, before.regional_rasters)
    };
    let compiles = crate::app_native::native_ui_compile_count();
    assert!(with_native_preview_instant_for_test(due, || {
        app.prepare_native_input_scratch(wid)
    }));
    assert_eq!(
        crate::app_native::native_ui_compile_count(),
        compiles,
        "a motion frame compiles nothing"
    );
    let after = raster(&app, wid, view);
    assert_eq!(after.motion_ms, motion_ms(&app, due));
    assert_eq!(
        (after.full_rasters, after.regional_rasters),
        (full_before, regional_before),
        "nor rasters the page's content"
    );
    let crate::NativeRasterWork::Motion { rect, pixels } = after.last_work else {
        panic!("a paint-only frame, not {:?}", after.last_work);
    };
    let scale = app.windows[&wid].scale as f32;
    assert!(
        rect.x as f32 <= spinner_rect.x * scale
            && rect.y as f32 <= spinner_rect.y * scale
            && (rect.x + rect.width) as f32 >= spinner_rect.right() * scale
            && (rect.y + rect.height) as f32 >= spinner_rect.bottom() * scale
            && pixels < u64::from(after.width) * u64::from(after.height) / 100,
        "{rect:?} is the spinners' rect {spinner_rect:?}, rounded out"
    );
    assert_eq!(
        after.compiled.semantics, semantics_before,
        "nothing new is said"
    );
    let card = app.windows[&wid].settings_card.as_ref().unwrap();
    assert_ne!(card.serial, card_before, "the card's pixels changed");
    assert_eq!(
        card.delta,
        Some(aterm_gpu::TrayDelta {
            since: card_before,
            rows: (rect.y, rect.y + rect.height),
        }),
        "the card names the rows it patched"
    );
    assert!(
        card.rgba == after.rgba,
        "the patched card is the raster's copy"
    );
    let next = app.next_native_motion_tick(wid).expect("it steps on");
    assert!(next > due);

    // The run ends: no spinner, no wake.
    let mut settled = crate::system_state::fixture_status("running").unwrap();
    settled.busy = false;
    settled.running.clear();
    app.receive_system_state_status(
        Some(app.system_state_host.status_generation_for_test()),
        settled,
    );
    assert!(with_native_preview_instant_for_test(next, || {
        app.prepare_native_input_scratch(wid)
    }));
    assert!(raster(&app, wid, view).compiled.motion.is_empty());
    assert_eq!(
        app.next_native_motion_tick(wid),
        None,
        "a settled page owes no wake"
    );
}

/// A SPLIT'S SPINNER STEPS IN THE REUSED CARD (§5.1): with the running page beside a terminal,
/// a motion frame paints the page's spinner alone (no compile, no content raster) and blends
/// its rect into the window's card in place, naming those rows to the GPU arm, and that card
/// holds the same pixels a full blit of every leaf makes. NEGATIVE CONTROL: the frame before the
/// step, at the same clock, leaves the card as it was.
#[test]
fn a_splits_spinner_steps_in_the_reused_card_as_a_full_blit_draws_it() {
    let t0 = Instant::now() + Duration::from_secs(1);
    let (mut app, wid, view) = app_with_a_running_page(t0);
    let _ = app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
    assert!(
        with_native_preview_instant_for_test(t0, || app.prepare_heterogeneous_input_scratch(wid))
            .is_some()
    );
    let serial = app.windows[&wid].settings_card.as_ref().unwrap().serial;
    assert!(
        with_native_preview_instant_for_test(t0, || app.prepare_heterogeneous_input_scratch(wid))
            .is_some()
    );
    assert_eq!(
        app.windows[&wid].settings_card.as_ref().unwrap().serial,
        serial,
        "a frame with nothing new keeps the card"
    );
    let due = app
        .next_native_motion_tick(wid)
        .expect("the split's spinner steps");
    assert!(with_native_preview_instant_for_test(due, || {
        app.native_motion_due(wid, due)
    }));
    let compiles = crate::app_native::native_ui_compile_count();
    assert!(
        with_native_preview_instant_for_test(due, || app.prepare_heterogeneous_input_scratch(wid))
            .is_some()
    );
    assert_eq!(crate::app_native::native_ui_compile_count(), compiles);
    let crate::NativeRasterWork::Motion { rect, .. } = raster(&app, wid, view).last_work else {
        panic!("a paint-only frame");
    };
    let patched = app.windows[&wid].settings_card.as_ref().unwrap();
    assert_ne!(patched.serial, serial);
    let Some(delta) = patched.delta else {
        panic!("the card names the rows it patched");
    };
    assert_eq!(delta.since, serial);
    assert_eq!(delta.rows.1 - delta.rows.0, rect.height);
    let patched = patched.rgba.clone();
    app.windows.get_mut(&wid).unwrap().settings_card = None;
    assert!(
        with_native_preview_instant_for_test(due, || app.prepare_heterogeneous_input_scratch(wid))
            .is_some()
    );
    assert!(
        app.windows[&wid].settings_card.as_ref().unwrap().rgba == patched,
        "the patched card is the card a full blit draws"
    );
}

/// TYPING HOLDS THE SPINNER UNTIL THE GAP CLOSES, AND AN UNSEEN PAGE OWES NO WAKE (§5.6): a key
/// typed in a focused window moves the motion wake to the end of the typing gap and a tick that
/// meets the typing owes no frame; a window that loses its focus, or whose page an overlay
/// covers or the clock is pinned, owes none at all.
#[test]
fn typing_holds_the_spinner_until_the_gap_closes_and_an_unseen_page_owes_no_wake() {
    let t0 = Instant::now() + Duration::from_secs(1);
    let (mut app, wid, _) = app_with_a_running_page(t0);
    let free = app.next_native_motion_tick(wid).expect("it moves");
    app.last_keystroke_at = Some(t0);
    let held = app
        .next_native_motion_tick(wid)
        .expect("it waits for the gap");
    assert_eq!(held, t0 + crate::AUTOMATIC_UPDATE_KEYSTROKE_GAP);
    assert!(held > free);
    assert!(!with_native_preview_instant_for_test(free, || {
        app.native_motion_due(wid, free)
    }));
    app.last_keystroke_at = None;
    assert_eq!(app.next_native_motion_tick(wid), Some(free));

    app.windows.get_mut(&wid).unwrap().focused = false;
    assert_eq!(
        app.next_native_motion_tick(wid),
        None,
        "an unfocused window"
    );
    app.windows.get_mut(&wid).unwrap().focused = true;
    app.windows.get_mut(&wid).unwrap().occluded = true;
    assert_eq!(app.next_native_motion_tick(wid), None, "an occluded window");
    app.windows.get_mut(&wid).unwrap().occluded = false;
    assert_eq!(app.next_native_motion_tick(wid), Some(free));

    app.pin_native_motion(Some(1_234));
    assert_eq!(app.next_native_motion_tick(wid), None, "a pinned clock");
    assert!(app.prepare_native_input_scratch(wid));
    assert_eq!(
        app.windows[&wid]
            .leaf_render_cache
            .values()
            .find_map(|cache| cache.native.as_ref())
            .unwrap()
            .motion_ms,
        1_234,
        "the page paints at the pinned time"
    );
}

/// AN INJECTED KEY IS TYPING, as a pressed one is: a chord holds the motion; the text and raw
/// bytes an agent writes, and the mouse, do not.
#[test]
fn an_injected_key_is_typing_and_written_text_is_not() {
    use crate::input::InputEvent;
    let key = InputEvent::Key {
        key: aterm_types::keyboard::Key::Character('a'),
        mods: aterm_types::keyboard::Modifiers::empty(),
        base_layout: None,
        layout_text: Some('a'),
        event_type: aterm_types::keyboard::KeyEventType::Press,
    };
    assert!(input_is_typing(&key));
    assert!(!input_is_typing(&InputEvent::Text("hello".to_owned())));
    assert!(!input_is_typing(&InputEvent::KeySequence(
        b"\x1b[A".to_vec()
    )));
}
