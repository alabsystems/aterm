// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[cfg(unix)]
use super::DeliveredClass;
use crate::input::{InputEvent, Source};
use crate::{App, WindowId};
use aterm_models_effects::cursor_cat_model;
use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};
use std::time::{Duration, Instant};

fn key(character: char) -> InputEvent {
    InputEvent::Key {
        key: Key::Character(character),
        mods: Modifiers::empty(),
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    }
}

fn named(named: NamedKey) -> InputEvent {
    InputEvent::Key {
        key: Key::Named(named),
        mods: Modifiers::empty(),
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    }
}

fn type_word(app: &mut App, wid: WindowId, word: &str) {
    for c in word.chars() {
        app.input(wid, key(c), Source::Human);
    }
}

/// Drive enough echo-correlated pulses to cross every real earn threshold.
fn drive_cursor_cat_momentum(app: &mut App, wid: WindowId, enabled: bool) {
    let base = Instant::now();
    for i in 0..160 {
        crate::app_render::forward_kitty_cursor_motion(
            enabled,
            false,
            crate::cursor_glow::GlowStyle::RainbowKitty,
            Some(aterm_effects::cursor_glow::CursorCatMotionPulse {
                at: base + Duration::from_millis(i * 40),
                kind: aterm_effects::cursor_glow::CursorCatMotionKind::Advance,
            }),
            &mut app
                .windows
                .get_mut(&wid)
                .unwrap()
                .fx
                .companions_mut()
                .cursor_cat,
        );
    }
}

/// Host binding for the derived cursor-cat owner gate: with `cursor_trail = false` the
/// echo-correlated momentum seam withholds the decisive pulse and the glass/capture predicate
/// does not draw the ordinary branch; enabling the master proves the fixture can arm.
#[test]
fn cursor_trail_master_owns_ordinary_kitty_but_not_the_collection_hello() {
    let model = cursor_cat_model();
    let wid = WindowId(0);

    let mut off_app = App::headless_for_test();
    off_app.config.cursor_trail = Some(false);
    off_app.config.cursor_trail_style = Some("rainbow kitty".into());
    off_app.kitty_cursor_enabled_cache = None;
    off_app.recompute_sparkle();
    assert!(
        off_app.sparkle.is_some(),
        "Sparkle is on for this gate probe"
    );
    drive_cursor_cat_momentum(&mut off_app, wid, false);
    assert!(
        !off_app.windows[&wid].fx.companions().cursor_cat.is_active(),
        "trail master off must withhold the decisive ordinary momentum key"
    );
    assert!(
        !crate::app_render::cursor_cat_presentation_enabled(
            true,
            false,
            crate::cursor_glow::GlowStyle::RainbowKitty,
            false,
        ),
        "trail master off must also close the ordinary render branch"
    );

    let mut off_spec = model.init_state();
    assert!(model.fire("TypeWhileTrailOff", &mut off_spec));
    assert_eq!(
        off_spec[&"ordinary_armed"],
        i64::from(off_app.windows[&wid].fx.companions().cursor_cat.is_active())
    );
    assert_eq!(off_spec[&"ordinary_visible"], 0);

    let mut buggy = cursor_cat_model();
    for cst in &mut buggy.consts {
        if cst.0 == "Buggy" {
            cst.1 = 1;
        }
    }
    let mut leaked = buggy.init_state();
    assert!(buggy.fire("TypeWhileTrailOff", &mut leaked));
    assert_eq!(leaked[&"ordinary_armed"], 1);
    assert_eq!(leaked[&"ordinary_visible"], 1);
    assert!(!buggy.check_invariant("TrailMasterOwnsOrdinary", &leaked));

    let mut on_app = App::headless_for_test();
    on_app.config.cursor_trail = Some(true);
    on_app.config.cursor_trail_style = Some("rainbow kitty".into());
    on_app.kitty_cursor_enabled_cache = None;
    on_app.recompute_sparkle();
    drive_cursor_cat_momentum(&mut on_app, wid, true);
    assert!(
        on_app.windows[&wid].fx.companions().cursor_cat.is_active(),
        "enabled Nyan owner forwards the decisive momentum key"
    );
    assert!(crate::app_render::cursor_cat_presentation_enabled(
        true,
        true,
        crate::cursor_glow::GlowStyle::RainbowKitty,
        false,
    ));
    let mut on_spec = model.init_state();
    assert!(model.fire("EnableTrail", &mut on_spec));
    assert!(model.fire("TypeOrdinary", &mut on_spec));
    assert_eq!(on_spec[&"ordinary_armed"], 1);
    assert_eq!(on_spec[&"ordinary_visible"], 1);

    let hello_at = Instant::now();
    {
        let look = aterm_effects::kitty_registry::KittyLook::for_launch(App::TEST_LAUNCH_SEED);
        let ws = off_app.windows.get_mut(&wid).expect("window 0");
        ws.fx.companions_mut().cursor_cat.on_collect(hello_at, look);
        ws.fx
            .companions_mut()
            .cursor_cat
            .set_collection_presentable(hello_at, true);
    }
    let hello_visible = {
        let ws = off_app.windows.get_mut(&wid).expect("window 0");
        let f = ws
            .fx
            .companions_mut()
            .cursor_cat
            .frame(hello_at + Duration::from_millis(200));
        assert!(f.collection_hello, "PRECONDITION: this frame IS the hello");
        f.alpha > 0
    };
    assert!(hello_visible, "the hello shows with the trail master OFF");
    assert!(model.fire("Collect", &mut off_spec));
    assert_eq!(off_spec[&"trail_master"], 0);
    assert_eq!(off_spec[&"visible"], i64::from(hello_visible));
    assert!(model.check_invariant("HelloIndependentOfTrailMaster", &off_spec));
}

/// Typing `dog` through the REAL input path summons the dog cameo — but
/// only after the window has typed a lot ([`aterm_effects::kitty_summon`]'s
/// `DOG_SUMMON_KEYS`), and nothing lands in the Kitty Log (dogs are
/// visitors).
#[test]
fn typing_dog_after_the_gate_pops_the_dog_cameo() {
    let mut app = App::headless_for_test();
    app.recompute_sparkle();
    assert!(app.sparkle.is_some(), "sparkle resolves ON by default");
    let wid = WindowId(0);

    type_word(&mut app, wid, "dog ");
    assert!(
        !app.windows[&wid]
            .fx
            .companions()
            .dog_cameo
            .active(std::time::Instant::now()),
        "no dog before the typed-a-lot gate opens"
    );

    for _ in 0..30 {
        type_word(&mut app, wid, "qwzx qwzx qwzx qwzx ");
    }
    type_word(&mut app, wid, "dog");
    let now = std::time::Instant::now();
    assert!(
        app.windows[&wid].fx.companions().dog_cameo.active(now),
        "the earned dog arrives on the completing letter"
    );
    assert_eq!(
        app.kitty_log.log().sightings,
        0,
        "dogs are visitors — the Kitty Log records nothing"
    );
}

/// The word engine's "a human typed" witness is wired. A spent cat re-arms only on a committed
/// edit keystroke, a signal only this input path produces, so a dropped seam would silently stop
/// every retype replaying its cat.
#[test]
fn a_committed_edit_key_arms_the_word_engine_typing_witness() {
    use aterm_types::keyboard::Key;

    let now = std::time::Instant::now();

    let mut app = App::headless_for_test();
    app.recompute_sparkle();
    let wid = WindowId(0);
    assert!(
        !app.windows[&wid].fx.decos().typed_edit_witness(now),
        "PRECONDITION: a window nobody has typed in carries no witness"
    );
    type_word(&mut app, wid, "k");
    assert!(
        app.windows[&wid]
            .fx
            .decos()
            .typed_edit_witness(std::time::Instant::now()),
        "a committed printable key is what the re-arm may trust"
    );

    let mut nav = App::headless_for_test();
    nav.recompute_sparkle();
    for _ in 0..4 {
        nav.input(
            wid,
            InputEvent::Key {
                key: Key::Named(aterm_types::keyboard::NamedKey::ArrowLeft),
                mods: Modifiers::empty(),
                base_layout: None,
                layout_text: None,
                event_type: KeyEventType::Press,
            },
            Source::Human,
        );
    }
    assert!(
        !nav.windows[&wid]
            .fx
            .decos()
            .typed_edit_witness(std::time::Instant::now()),
        "walking the caret over a finished kitty is not a retype of it"
    );
}

/// An app whose session sink writes to a test-owned pipe. The private fd matters: the ordering
/// FIFO is a process-global registry keyed by PTY master, and every plain `headless_for_test`
/// shares master `-1`, so pinning that key would divert concurrent tests' keystrokes.
#[cfg(unix)]
pub(super) fn app_with_private_pty() -> (
    App,
    std::sync::Arc<aterm_session::sink::SinkWriter>,
    [i32; 2],
) {
    use aterm_session::sink::SinkWriter;
    use std::sync::Arc;

    let mut pipe = [0; 2];
    assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
    let flags = unsafe { libc::fcntl(pipe[0], libc::F_GETFL) };
    assert!(flags >= 0);
    assert_eq!(
        unsafe { libc::fcntl(pipe[0], libc::F_SETFL, flags | libc::O_NONBLOCK) },
        0
    );
    let sink = Arc::new(SinkWriter::new(pipe[1]));
    let mut app = App::headless_for_test_with_sink(sink.clone());
    app.recompute_sparkle();
    (app, sink, pipe)
}

#[cfg(unix)]
pub(super) fn drain(pipe: [i32; 2]) -> Vec<u8> {
    let mut bytes = [0u8; 64];
    let read = unsafe { libc::read(pipe[0], bytes.as_mut_ptr().cast(), bytes.len()) };
    if read <= 0 {
        return Vec::new();
    }
    bytes[..read as usize].to_vec()
}

/// A frame observing a key is presented at most one refresh after the key's edge (its dispatch
/// inline, the writer's completed write when queued).
#[cfg(unix)]
pub(super) const OBSERVING_FRAME: Duration = Duration::from_millis(16);

/// The clock of the `nth` (1-based) frame after the key edge `edge`
/// ([`OBSERVING_FRAME`]): never before `edge` (an edge the caller has
/// already observed), monotone in `nth` and in real time.
#[cfg(unix)]
pub(super) fn observing_frame(edge: Instant, nth: u32) -> Instant {
    Instant::now().min(edge + OBSERVING_FRAME * nth)
}

/// The delivery edge of a queued write: the newest of the `receipts`
/// receipts `wid`'s session published past the window's baseline
/// (`delivery_seen`), placed as the render prelude places it.
#[cfg(unix)]
pub(super) fn delivery_edge(app: &App, wid: WindowId, receipts: u64) -> Instant {
    let session = app.front_terminal(wid).expect("a terminal").session;
    let after = app.windows[&wid]
        .delivery_seen
        .map_or(0, |(_, serial)| serial);
    let ctx = &app.pool.get(session).expect("the session").ctx;
    let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while std::time::Instant::now() < deadline {
        let batch = ctx
            .output_echo
            .deliveries_after(&ctx.sink, Some(after), Instant::now());
        if batch.latest >= after + receipts {
            let (at, _) = batch
                .items
                .iter()
                .flatten()
                .next_back()
                .expect("a resolved receipt");
            return *at;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("no {receipts} delivery receipt(s) past serial {after}");
}

/// A control fence (`signal`'s, say) recorded on a session is not an empty
/// synthetic key: the next look of every window presenting the session clears
/// only the two movement candidates (no PTY bytes, typing heat, rain
/// scheduling or latency-hot stamp).
#[cfg(unix)]
#[test]
fn a_session_fence_clears_only_the_licence_in_every_window_presenting_it() {
    use crate::cursor_glow::{Geom, GlowStyle};
    use crate::cursor_trail::TrailConfig;

    let (mut app, sink, pipe) = app_with_private_pty();
    let wid = WindowId(0);
    let session = app.front_terminal(wid).unwrap().session;
    let second = app
        .open_active_session_in_new_window_logical()
        .expect("share the active session through the canonical view-count seam");
    assert_eq!(app.frontmost_window, Some(second));
    assert_eq!(app.pool.views(session), Some(2));
    app.reconcile_licence(wid);
    app.reconcile_licence(second);
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("lumen".to_string());
    let glow_cfg = app.glow_config();
    assert!(matches!(glow_cfg.style, GlowStyle::Lumen));
    let trail_cfg = TrailConfig {
        enabled: true,
        duration: Duration::from_millis(300),
        max_len: 24,
        color: 0x0050_FA7B,
        intensity: 0.5,
        warmth: 0.0,
    };
    let geom = Geom {
        cw: 8,
        ch: 16,
        rows: 6,
        cols: 40,
        origin_x: 0,
        origin_y: 0,
        win_w: 320,
        win_h: 96,
        head: 0,
    };
    let now = Instant::now();
    let mut glow_out = Vec::new();
    let mut trail_out = Vec::new();
    for candidate in [wid, second] {
        let ws = app.windows.get_mut(&candidate).unwrap();
        ws.fx
            .cursor_mut()
            .glow
            .tick(Some((0, 0)), now, &glow_cfg, geom, &mut glow_out);
        ws.fx
            .cursor_mut()
            .trail
            .tick(Some((0, 0)), now, &trail_cfg, &mut trail_out);
        ws.fx.cursor_mut().glow.note_synthetic_move(now);
        ws.fx.cursor_mut().trail.note_synthetic_move(now);
        assert!(ws.fx.cursor_mut().glow.move_licensed(now));
        assert!(ws.fx.cursor_mut().trail.move_licensed(now));
    }
    let (cadence_before, rain_deadline_before, input_hot_before) = {
        let ws = app.windows.get_mut(&wid).unwrap();
        ws.fx.cursor_mut().cadence.on_keystroke(now);
        (
            ws.fx.cursor_mut().cadence.sample(now),
            ws.next_rain_tick,
            ws.input_hot_until,
        )
    };

    for candidate in [wid, second] {
        app.reconcile_licence(candidate);
        let ws = &app.windows[&candidate];
        assert!(ws.fx.cursor().glow.move_licensed(now), "no fence, no clear");
        assert!(
            ws.fx.cursor().trail.move_licensed(now),
            "no fence, no clear"
        );
    }
    app.pool
        .get(session)
        .expect("the shared session")
        .ctx
        .licence
        .fence(false);
    for candidate in [wid, second] {
        app.reconcile_licence(candidate);
        let ws = &app.windows[&candidate];
        assert!(!ws.fx.cursor().glow.move_licensed(now));
        assert!(!ws.fx.cursor().trail.move_licensed(now));
    }
    let ws = &app.windows[&wid];
    assert_eq!(ws.fx.cursor().cadence.sample(now), cadence_before);
    assert_eq!(ws.next_rain_tick, rain_deadline_before);
    assert_eq!(ws.input_hot_until, input_hot_before);
    assert!(
        drain(pipe).is_empty(),
        "candidate fence writes no PTY bytes"
    );
    drop(app);
    drop(sink);
    unsafe { libc::close(pipe[0]) };
}

#[test]
fn ime_and_external_local_boundaries_retire_older_candidates_before_consumers() {
    use crate::cursor_glow::Geom;
    use crate::cursor_trail::TrailConfig;

    fn assert_boundary_dark(mut app: App, boundary: impl FnOnce(&mut App, WindowId)) {
        let wid = WindowId(0);
        app.config.cursor_trail = Some(true);
        app.config.cursor_trail_style = Some("lumen".to_string());
        let glow_cfg = app.glow_config();
        let trail_cfg = TrailConfig {
            enabled: true,
            duration: Duration::from_millis(300),
            max_len: 24,
            color: 0x0050_FA7B,
            intensity: 0.5,
            warmth: 0.0,
        };
        let geom = Geom {
            cw: 8,
            ch: 16,
            rows: 6,
            cols: 40,
            origin_x: 0,
            origin_y: 0,
            win_w: 320,
            win_h: 96,
            head: 0,
        };
        let now = Instant::now();
        let mut glow = Vec::new();
        let mut trail = Vec::new();
        {
            let ws = app.windows.get_mut(&wid).unwrap();
            ws.fx
                .cursor_mut()
                .glow
                .tick(Some((0, 0)), now, &glow_cfg, geom, &mut glow);
            ws.fx
                .cursor_mut()
                .trail
                .tick(Some((0, 0)), now, &trail_cfg, &mut trail);
            ws.fx.cursor_mut().glow.note_synthetic_move(now);
            ws.fx.cursor_mut().trail.note_synthetic_move(now);
        }

        boundary(&mut app, wid);
        let ws = app.windows.get_mut(&wid).unwrap();
        assert!(!ws.fx.cursor_mut().glow.move_licensed(now));
        assert!(!ws.fx.cursor_mut().trail.move_licensed(now));
        let moved = now + Duration::from_millis(1);
        assert_eq!(
            ws.fx
                .cursor_mut()
                .glow
                .tick(Some((0, 1)), moved, &glow_cfg, geom, &mut glow),
            0
        );
        assert_eq!(
            ws.fx
                .cursor_mut()
                .trail
                .tick(Some((0, 1)), moved, &trail_cfg, &mut trail),
            0
        );
        assert!(glow.is_empty() && trail.is_empty());
    }

    assert_boundary_dark(App::headless_for_test(), |app, wid| {
        app.on_ime_preedit(wid, "compose".to_string(), None);
    });
    assert_boundary_dark(App::headless_for_test(), |app, wid| {
        app.on_ime_commit(wid, String::new());
    });

    let mut search = App::headless_for_test();
    search.search_enter();
    assert_boundary_dark(search, |app, wid| {
        app.on_ime_commit(wid, "é".to_string());
    });

    let mut native = App::headless_for_test();
    assert!(native.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    assert_boundary_dark(native, |app, wid| {
        app.on_ime_commit(wid, "é".to_string());
    });

    for action in [
        crate::menu::MenuAction::Copy,
        crate::menu::MenuAction::Paste,
    ] {
        assert_boundary_dark(App::headless_for_test(), |app, _wid| {
            app.fence_menu_action(action);
        });
    }
    assert_boundary_dark(App::headless_for_test(), |app, _wid| {
        app.clear_front_move_license();
    });
    assert_boundary_dark(App::headless_for_test(), |app, wid| {
        app.clear_tab_surface_move_license(Some(wid));
    });
    assert_boundary_dark(App::headless_for_test(), |app, _wid| {
        app.clear_tab_surface_move_license(None);
    });
    assert_boundary_dark(App::headless_for_test(), |app, wid| {
        app.on_pill_wake(wid, 0, crate::presence::PillHit::Go);
    });
}

/// The typing witness's clock starts when the key reaches the terminal, not at event-loop
/// arrival.
#[cfg(unix)]
#[test]
fn a_key_queued_behind_a_paste_still_witnesses_its_own_echo() {
    let (mut app, sink, pipe) = app_with_private_pty();
    let wid = WindowId(0);
    let session = app.front_terminal(wid).unwrap().session;

    assert!(
        !super::paste_order::is_ordering(&sink),
        "PRECONDITION: a fresh session has no paste draining"
    );
    let inline_at = Instant::now();
    type_word(&mut app, wid, "k");
    assert!(
        app.windows[&wid]
            .fx
            .decos()
            .typed_edit_witness(inline_at + Duration::from_millis(300)),
        "an inline key witnesses the echo that follows it"
    );
    assert!(
        !app.windows[&wid]
            .fx
            .decos()
            .typed_edit_witness(inline_at + Duration::from_millis(1_100)),
        "and is granted no head start it did not spend in a queue"
    );

    let pin = super::paste_order::pin_ordering_for_test(&sink);
    assert!(
        super::paste_order::is_ordering(&sink),
        "PRECONDITION: the key path must see this session as ordering"
    );
    let queued_at = Instant::now();
    type_word(&mut app, wid, "k");
    assert!(
        app.windows[&wid].fx.decos().typed_edit_witness(queued_at),
        "a queued key witnesses immediately — the FIFO may drain at once"
    );
    assert!(
        app.windows[&wid]
            .fx
            .decos()
            .typed_edit_witness(queued_at + Duration::from_millis(1_400)),
        "and must still witness the echo that only arrives after the queue \
         drains — the arrival-stamped witness expired here"
    );
    assert!(
        !app.windows[&wid]
            .fx
            .decos()
            .typed_edit_witness(queued_at + Duration::from_millis(2_400)),
        "the queue's head start is a bounded grace, not an open licence"
    );

    app.windows
        .get_mut(&wid)
        .unwrap()
        .fx
        .decos_mut()
        .bind_pane(session.wrapping_add(0xBEEF), (0, 0));
    let elsewhere_at = Instant::now();
    type_word(&mut app, wid, "k");
    assert!(
        !app.windows[&wid]
            .fx
            .decos()
            .typed_edit_witness(elsewhere_at),
        "another pane's key may not license a re-arm on this scan"
    );
    assert!(
        !app.windows[&wid]
            .fx
            .decos()
            .typed_edit_witness(elsewhere_at + Duration::from_millis(1_400)),
        "and the queue grace must not smuggle it in later either"
    );

    drop(pin);
    super::paste_order::await_drain_for_test(&sink);
    assert!(
        !super::paste_order::is_ordering(&sink),
        "the ordered egress must drain once the pin is released"
    );
    unsafe {
        libc::close(pipe[0]);
        libc::close(pipe[1]);
    }
}

/// A key queued behind a draining paste keeps its licence. A typed key's
/// arrival stamp is revoked at enqueue and the prelude re-stamps it at its
/// delivery receipt (one stamp, one echo, as for an inline key: without the
/// re-stamp the hop reads as program output, `no-fresh-hint`).
#[cfg(unix)]
#[test]
fn a_queued_one_shot_key_keeps_its_licence_at_delivery() {
    use crate::app_render::CursorFxInputs;
    let chord = |key: Key, mods: Modifiers| InputEvent::Key {
        key,
        mods,
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    };
    let cases: [(&str, InputEvent, DeliveredClass, bool); 7] = [
        ("k", key('k'), DeliveredClass::Typed, false),
        (
            "Enter",
            named(NamedKey::Enter),
            DeliveredClass::Return,
            false,
        ),
        (
            "Shift+Enter",
            chord(Key::Named(NamedKey::Enter), Modifiers::SHIFT),
            DeliveredClass::Newline,
            true,
        ),
        (
            "ArrowLeft",
            named(NamedKey::ArrowLeft),
            DeliveredClass::Nav,
            false,
        ),
        (
            "Backspace",
            named(NamedKey::Backspace),
            DeliveredClass::Erase,
            false,
        ),
        (
            "Ctrl+W",
            chord(Key::Character('w'), Modifiers::CTRL),
            DeliveredClass::Kill {
                moves_cursor: true,
                word: true,
            },
            false,
        ),
        ("Tab", named(NamedKey::Tab), DeliveredClass::Gesture, false),
    ];
    for style in ["rainbow kitty", "comet"] {
        for (label, ev, class, alt_screen) in cases.clone() {
            for queued in [false, true] {
                let (mut app, sink, pipe) = app_with_private_pty();
                let wid = WindowId(0);
                app.config.motion = Some("full".into());
                app.config.cursor_trail = Some(true);
                app.config.cursor_trail_style = Some(style.to_string());
                app.config.trail_sounds = Some(false);
                app.windows.get_mut(&wid).unwrap().focused = true;
                app.frontmost_window = Some(wid);
                let session = app.front_terminal(wid).unwrap().session;
                if alt_screen {
                    let term = app.front_terminal(wid).expect("terminal").term.clone();
                    crate::term_lock(&term).process(b"\x1b[?1049h");
                }
                let mut seed = CursorFxInputs::sample_for_test(Instant::now());
                seed.cur = Some((2, 2));
                app.cursor_frame(wid, seed).expect("seed");
                let pin = queued.then(|| super::paste_order::pin_ordering_for_test(&sink));
                if queued {
                    assert!(super::paste_order::is_ordering(&sink));
                }
                let pressed = Instant::now();
                assert_eq!(
                    app.input(wid, ev.clone(), Source::Human),
                    crate::input::InputOutcome::Ok,
                    "{label}"
                );
                drop(pin);
                super::paste_order::await_drain_for_test(&sink);
                assert!(!super::paste_order::is_ordering(&sink));
                let mut edge = pressed;
                if queued {
                    let ctx = &app.pool.get(session).unwrap().ctx;
                    let mut batch =
                        ctx.output_echo
                            .deliveries_after(&ctx.sink, Some(0), Instant::now());
                    let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
                    while std::time::Instant::now() < deadline {
                        if batch.items.iter().flatten().next().is_some() {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(5));
                        batch =
                            ctx.output_echo
                                .deliveries_after(&ctx.sink, Some(0), Instant::now());
                    }
                    let classes: Vec<_> =
                        batch.items.iter().flatten().map(|(_, t)| t.key).collect();
                    assert_eq!(
                        classes,
                        vec![Some(class)],
                        "{label}: the queued key's receipt carries its class (latest={}, evicted={}, spill_drained={})",
                        batch.latest,
                        batch.evicted,
                        ctx.sink.egress_drained_to_kernel()
                    );
                    edge = batch.items.iter().flatten().next_back().unwrap().0;
                }
                let now = observing_frame(edge, 1);
                let mut live = CursorFxInputs::sample_for_test(now);
                live.cur = Some((2, 2));
                app.cursor_frame(wid, live).expect("the prelude");
                assert!(
                    app.windows[&wid].fx.cursor().glow.move_licensed(now),
                    "{style} {label} queued={queued}: the key's one-shot licence stands after the prelude"
                );
                drop(app);
                drop(sink);
                unsafe {
                    libc::close(pipe[0]);
                    libc::close(pipe[1]);
                }
            }
        }
    }
}

/// A Tab's dispatch arm claims no hop refused before the key. A bare Tab / ⌃V is written inline
/// and armed at dispatch (`note_insert_armed`) without the late-receipt claim, whose backward
/// window (`INSERT_RETRO_FRESH`) is for a spill-held receipt and would light a refused prompt
/// print as `insert`.
#[cfg(unix)]
#[test]
fn a_tabs_dispatch_arm_claims_no_hop_refused_before_the_key() {
    use crate::cursor_glow::Geom;
    use aterm_types::keyboard::NamedKey;

    let (mut app, sink, pipe) = app_with_private_pty();
    let wid = WindowId(0);
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("rainbow kitty".to_string());
    let glow_cfg = app.glow_config();
    let geom = Geom {
        cw: 8,
        ch: 16,
        rows: 24,
        cols: 80,
        origin_x: 0,
        origin_y: 0,
        win_w: 640,
        win_h: 384,
        head: 0,
    };
    let mut out = Vec::new();
    let t0 = Instant::now();
    {
        let glow = &mut app.windows.get_mut(&wid).unwrap().fx.cursor_mut().glow;
        glow.tick(Some((2, 2)), t0, &glow_cfg, geom, &mut out);
        glow.tick(
            Some((2, 4)),
            t0 + Duration::from_millis(20),
            &glow_cfg,
            geom,
            &mut out,
        );
        assert_eq!(glow.spawns(), 0, "the keyless hop is refused");
    }
    assert_eq!(
        app.input(wid, named(NamedKey::Tab), Source::Human),
        crate::input::InputOutcome::Ok
    );
    {
        let glow = &app.windows[&wid].fx.cursor().glow;
        assert_eq!(
            glow.spawns(),
            0,
            "a Tab whose byte just went out has no echo to light yet: {:?}",
            glow.admission_log()
                .last()
                .map(|r| (r.licence, r.origin, r.target))
        );
        assert_eq!(glow.insert_tally().lit, 0);
        assert_eq!(glow.insert_tally().delivered, 1, "the licence is armed");
    }
    let glow = &mut app.windows.get_mut(&wid).unwrap().fx.cursor_mut().glow;
    glow.tick(
        Some((2, 12)),
        t0 + Duration::from_millis(60),
        &glow_cfg,
        geom,
        &mut out,
    );
    assert_eq!(
        glow.admission_log()
            .last()
            .map(|r| (r.licence, r.reason, r.origin, r.target)),
        Some(("insert", "licensed", (2, 4), (2, 12))),
        "the real completion is the insert's"
    );
    assert_eq!(glow.insert_tally().lit, 1);
    drop(app);
    drop(sink);
    unsafe {
        libc::close(pipe[0]);
        libc::close(pipe[1]);
    }
}

/// A key release queued behind a draining paste carries no ticket. Under Kitty flags 2|8
/// (REPORT_EVENT_TYPES + REPORT_ALL_KEYS_AS_ESC) a release (`CSI n;1:3 u`) is written like a
/// press but banks nothing at dispatch; a ticket would re-stamp the key-up as a typed press or
/// arm a second insert licence.
#[cfg(unix)]
#[test]
fn a_queued_key_release_publishes_no_receipt() {
    let (mut app, sink, pipe) = app_with_private_pty();
    let wid = WindowId(0);
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("rainbow kitty".to_string());
    let session = app.front_terminal(wid).unwrap().session;
    let term = app.pool.get(session).unwrap().term.clone();
    crate::term_lock(&term).process(b"\x1b[>10u");
    let release = |key: Key| InputEvent::Key {
        key,
        mods: Modifiers::empty(),
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Release,
    };
    let pin = super::paste_order::pin_ordering_for_test(&sink);
    assert!(super::paste_order::is_ordering(&sink));
    assert_eq!(
        app.input(wid, release(Key::Character('a')), Source::Human),
        crate::input::InputOutcome::Ok
    );
    assert_eq!(
        app.input(wid, release(Key::Named(NamedKey::Tab)), Source::Human),
        crate::input::InputOutcome::Ok
    );
    drop(pin);
    super::paste_order::await_drain_for_test(&sink);
    assert!(!super::paste_order::is_ordering(&sink));
    let now = Instant::now();
    let ctx = &app.pool.get(session).unwrap().ctx;
    let batch = ctx.output_echo.deliveries_after(&ctx.sink, Some(0), now);
    let tickets: Vec<_> = batch.items.iter().flatten().map(|(_, t)| *t).collect();
    assert!(
        tickets.is_empty(),
        "a key-up is not typing and not an insert: {tickets:?}"
    );
    assert_eq!(batch.latest, 0, "no receipt at all");
    let bytes = drain(pipe);
    assert_eq!(bytes, b"\x1b[97;1:3u\x1b[9;1:3u");
    assert!(
        !app.windows[&wid].fx.cursor().glow.move_licensed(now),
        "a key-up arms no typed licence"
    );
    unsafe {
        libc::close(pipe[0]);
        libc::close(pipe[1]);
    }
}

/// A queued ticket carries its dispatch instant. The insert's late-receipt
/// claim reaches back `INSERT_RETRO_FRESH` from the writer's completion,
/// which for a Tab queued behind a short paste reaches past the Tab's own
/// press.
#[cfg(unix)]
#[test]
fn a_queued_ticket_carries_its_dispatch_instant() {
    use aterm_effects::cursor_glow::InsertWidth;

    let (mut app, sink, pipe) = app_with_private_pty();
    let wid = WindowId(0);
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("rainbow kitty".to_string());
    let session = app.front_terminal(wid).unwrap().session;
    let pin = super::paste_order::pin_ordering_for_test(&sink);
    assert!(super::paste_order::is_ordering(&sink));
    let before = Instant::now();
    assert_eq!(
        app.input(wid, named(NamedKey::Tab), Source::Human),
        crate::input::InputOutcome::Ok
    );
    let tab_dispatched = Instant::now();
    assert_eq!(
        app.input(
            wid,
            InputEvent::Paste("yz".to_string(), crate::input::PasteFraming::AtDrain),
            Source::Human
        ),
        crate::input::InputOutcome::Ok
    );
    let paste_dispatched = Instant::now();
    drop(pin);
    super::paste_order::await_drain_for_test(&sink);
    assert!(!super::paste_order::is_ordering(&sink));
    let now = Instant::now();
    let ctx = &app.pool.get(session).unwrap().ctx;
    let batch = ctx.output_echo.deliveries_after(&ctx.sink, Some(0), now);
    let receipts: Vec<_> = batch.items.iter().flatten().copied().collect();
    assert_eq!(receipts.len(), 2, "the Tab's and the paste's: {receipts:?}");
    let (tab_at, tab) = receipts[0];
    let (paste_at, paste) = receipts[1];
    assert_eq!(tab.insert, Some(InsertWidth::Unknown));
    assert_eq!(paste.insert, Some(InsertWidth::Cells(2)));
    let tab_dispatch = tab.dispatched_at;
    let paste_dispatch = paste.dispatched_at;
    assert!(
        before <= tab_dispatch && tab_dispatch <= tab_dispatched,
        "the Tab's dispatch instant is the press's"
    );
    assert!(
        tab_dispatched <= paste_dispatch && paste_dispatch <= paste_dispatched,
        "the paste's dispatch instant is its own"
    );
    assert!(
        tab_dispatch <= tab_at && paste_dispatch <= paste_at,
        "a receipt is never before its dispatch: tab {:?} paste {:?}",
        tab_at.saturating_duration_since(tab_dispatch),
        paste_at.saturating_duration_since(paste_dispatch)
    );
    let _ = drain(pipe);
    unsafe {
        libc::close(pipe[0]);
        libc::close(pipe[1]);
    }
}

/// The prelude hands a queued Tab's dispatch instant to the engine
/// (`note_insert_delivered_from`): the seam between
/// [`a_queued_ticket_carries_its_dispatch_instant`] and the engine's
/// `a_late_receipt_claims_no_hop_refused_before_its_dispatch`.
#[cfg(unix)]
#[test]
fn the_prelude_bounds_a_queued_tabs_late_claim_at_its_dispatch() {
    use crate::app_render::CursorFxInputs;
    use aterm_types::keyboard::NamedKey;

    let (mut app, sink, pipe) = app_with_private_pty();
    let wid = WindowId(0);
    app.config.motion = Some("full".into());
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("rainbow kitty".to_string());
    app.config.trail_sounds = Some(false);
    app.windows.get_mut(&wid).unwrap().focused = true;
    app.frontmost_window = Some(wid);
    let t0 = Instant::now();
    let mut seed = CursorFxInputs::sample_for_test(t0);
    seed.cur = Some((2, 2));
    app.cursor_frame(wid, seed).expect("seed");
    std::thread::sleep(Duration::from_millis(5));
    let hop_at = Instant::now();
    let mut hop = CursorFxInputs::sample_for_test(hop_at);
    hop.cur = Some((2, 4));
    app.cursor_frame(wid, hop).expect("the keyless hop");
    std::thread::sleep(Duration::from_millis(30));
    {
        let glow = &app.windows[&wid].fx.cursor().glow;
        assert_eq!(glow.spawns(), 0, "the keyless hop is refused");
        assert_eq!(glow.insert_tally().lit, 0);
    }
    let pin = super::paste_order::pin_ordering_for_test(&sink);
    assert!(super::paste_order::is_ordering(&sink));
    assert_eq!(
        app.input(wid, named(NamedKey::Tab), Source::Human),
        crate::input::InputOutcome::Ok
    );
    drop(pin);
    super::paste_order::await_drain_for_test(&sink);
    assert!(!super::paste_order::is_ordering(&sink));
    let now = Instant::now();
    let mut live = CursorFxInputs::sample_for_test(now);
    live.cur = Some((2, 4));
    app.cursor_frame(wid, live).expect("the prelude");
    let glow = &app.windows[&wid].fx.cursor().glow;
    let last = glow
        .admission_log()
        .last()
        .map(|r| (r.licence, r.reason, r.origin, r.target));
    assert_eq!(
        glow.insert_tally().delivered,
        1,
        "the queued Tab's receipt re-armed the licence: {last:?}"
    );
    assert_eq!(
        glow.spawns(),
        0,
        "a hop refused before the Tab's dispatch is program output: {last:?}"
    );
    assert_eq!(glow.insert_tally().lit, 0, "{last:?}");
    let _ = drain(pipe);
    drop(app);
    drop(sink);
    unsafe {
        libc::close(pipe[0]);
        libc::close(pipe[1]);
    }
}

/// A real PTY pair whose slave this test puts into `lflag` mode, with no
/// child: the master reflects the slave's termios directly (as aterm-pty's
/// real-spawn test proves for IUTF8 + B230400), so the session sink
/// reads what a `read -s` / raw TUI would set.
#[cfg(unix)]
pub(super) fn pty_pair_with_lflag(edit: impl FnOnce(&mut libc::tcflag_t)) -> (i32, i32) {
    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    // SAFETY: `libc::termios` is plain integer fields plus a byte array; zeroed is valid.
    let mut t: libc::termios = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { libc::tcgetattr(slave, &mut t) },
        0,
        "tcgetattr(slave)"
    );
    edit(&mut t.c_lflag);
    // SAFETY: `t` is a valid termios for the open slave.
    assert_eq!(
        unsafe { libc::tcsetattr(slave, libc::TCSANOW, &t) },
        0,
        "tcsetattr(slave)"
    );
    (master, slave)
}

/// A headless app on a real pty pair whose slave is in `lflag` mode, with
/// the rainbow kitty trail on.
#[cfg(unix)]
fn pty_app(
    edit: impl FnOnce(&mut libc::tcflag_t),
) -> (
    App,
    std::sync::Arc<aterm_session::sink::SinkWriter>,
    i32,
    i32,
) {
    let (master, slave) = pty_pair_with_lflag(edit);
    let sink = std::sync::Arc::new(aterm_session::sink::SinkWriter::new(master));
    let mut app = App::headless_for_test_with_sink(sink.clone());
    app.recompute_sparkle();
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("rainbow kitty".to_string());
    (app, sink, master, slave)
}

#[cfg(unix)]
fn close_pty(
    app: App,
    sink: std::sync::Arc<aterm_session::sink::SinkWriter>,
    master: i32,
    slave: i32,
) {
    drop(app);
    drop(sink);
    // SAFETY: both fds are the caller's and open; the borrowed-fd sink did not close master.
    unsafe {
        libc::close(master);
        libc::close(slave);
    }
}

/// The three tty modes a press can meet: cooked (a shell's baseline),
/// canonical no-echo (`read -s`, `sudo`, an `ssh` passphrase) and raw
/// (Claude Code's `setRawMode`, vim, ZLE at rest: the program echoes for
/// itself), each with whether the tty echoes what is typed.
#[cfg(unix)]
type SetMode = fn(&mut libc::tcflag_t);
#[cfg(unix)]
const TTY_MODES: [(&str, SetMode, bool); 3] = [
    ("cooked (echo+icanon)", |_| {}, true),
    ("canonical no-echo (read -s)", |l| *l &= !libc::ECHO, false),
    (
        "raw (a TUI echoes for itself)",
        |l| *l &= !(libc::ECHO | libc::ICANON),
        true,
    ),
];

/// A press the tty will not echo banks no credit. The host reads the pty's
/// ECHO/ICANON off the session sink at the key (iTerm2's password-mode
/// rule); a press into canonical no-echo mode arms no typed licence and no
/// press credit on either engine, so a program write on that row has
/// nothing to spend.
#[cfg(unix)]
#[test]
fn a_press_the_tty_will_not_echo_banks_no_credit() {
    for (label, edit, banks) in TTY_MODES {
        let (mut app, sink, master, slave) = pty_app(edit);
        let echo = sink.tty_echo().expect("a pty master answers");
        assert_eq!(echo.swallows_input(), !banks, "{label}: the sink's verdict");
        let wid = WindowId(0);
        type_word(&mut app, wid, "abc");
        let now = Instant::now();
        let session = app.front_terminal(wid).unwrap().session;
        let ctx = &app.pool.get(session).unwrap().ctx;
        assert!(
            ctx.output_echo
                .sample(&ctx.sink, now)
                .last_accepted_at
                .is_some(),
            "{label}: the bytes reached the PTY regardless"
        );
        let ws = app.windows.get(&wid).unwrap();
        assert_eq!(ws.fx.cursor().glow.move_licensed(now), banks, "{label}");
        assert_eq!(
            ws.fx.cursor().trail.move_licensed(now),
            banks,
            "{label}: the classic trail's typed licence, in lockstep"
        );
        assert_eq!(
            ws.fx.cursor().glow.in_flight_tally().swallowed_no_echo,
            if banks { 0 } else { 3 },
            "{label}: the `swallowed_no_echo=` tally"
        );
        close_pty(app, sink, master, slave);
    }
}

/// A password prompt clicks one tone and rings no ting. Real pty pairs and the audio host's
/// capture tap; `a`, `A` (Shift), `1`, `.`, a space and a bare Shift go into each.
#[cfg(unix)]
#[test]
fn a_password_prompt_clicks_one_tone_and_rings_no_ting() {
    use aterm_effects::trail_sound::{SoundGesture, SoundKind, glyph_class};
    use aterm_session::sink::SinkWriter;
    use std::sync::Arc;

    type Edit = fn(&mut libc::tcflag_t);
    let cases: [(&str, Edit, bool); 2] = [
        ("cooked (echo+icanon)", |_| {}, false),
        ("canonical no-echo (read -s)", |l| *l &= !libc::ECHO, true),
    ];
    let press = |key: Key, mods: Modifiers| InputEvent::Key {
        key,
        mods,
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    };
    for (label, edit, secret) in cases {
        let (master, slave) = pty_pair_with_lflag(edit);
        let sink = Arc::new(SinkWriter::new(master));
        assert_eq!(
            sink.tty_swallows_input(),
            secret,
            "{label}: the sink's verdict"
        );
        let mut app = App::headless_for_test_with_sink(sink.clone());
        app.config.cursor_trail = Some(true);
        app.config.cursor_trail_style = Some("rainbow kitty".to_string());
        app.trail_audio = crate::trail_audio::TrailAudio::capturing_for_test();
        let wid = WindowId(0);
        app.cursor_frame(
            wid,
            crate::app_render::CursorFxInputs::sample_for_test(Instant::now()),
        )
        .expect("the fixture window ticks");
        let _ = app.trail_audio.take_captured_with_meta_for_test();
        let keys = [
            ('a', press(Key::Character('a'), Modifiers::empty())),
            ('A', press(Key::Character('a'), Modifiers::SHIFT)),
            ('1', press(Key::Character('1'), Modifiers::empty())),
            ('.', press(Key::Character('.'), Modifiers::empty())),
            (' ', press(Key::Named(NamedKey::Space), Modifiers::empty())),
        ];
        for (ch, ev) in keys {
            let _ = app.input(wid, ev, Source::Human);
            let spoken = app.trail_audio.take_captured_with_meta_for_test();
            let clicks: Vec<_> = spoken
                .iter()
                .filter(|(ev, _)| {
                    matches!(
                        ev.kind,
                        SoundGesture::Trail(SoundKind::Typed | SoundKind::Space)
                    )
                })
                .collect();
            assert_eq!(clicks.len(), 1, "{label}: `{ch}` clicks once: {spoken:?}");
            let (ev, meta) = clicks[0];
            let got = (ev.kind, ev.shifted, meta.glyph_class, meta.rank);
            if secret {
                assert_eq!(
                    got,
                    (
                        SoundGesture::Trail(SoundKind::Typed),
                        false,
                        glyph_class::SECRET,
                        0
                    ),
                    "{label}: `{ch}` told the synth which key it was"
                );
            } else {
                let want_kind = if ch == ' ' {
                    SoundKind::Space
                } else {
                    SoundKind::Typed
                };
                assert_eq!(
                    got,
                    (
                        SoundGesture::Trail(want_kind),
                        ch == 'A',
                        super::typed_glyph_class(Some(ch)),
                        super::typed_glyph_rank(Some(ch)),
                    ),
                    "{label}: `{ch}` is not its own click"
                );
            }
        }
        let _ = app.input(
            wid,
            press(Key::Named(NamedKey::ShiftLeft), Modifiers::empty()),
            Source::Human,
        );
        let lifted = app
            .trail_audio
            .take_captured_with_meta_for_test()
            .iter()
            .any(|(ev, _)| matches!(ev.kind, SoundGesture::Trail(SoundKind::Shift)));
        assert_eq!(lifted, !secret, "{label}: the bare Shift's ting");
        let ws = app.windows.get(&wid).unwrap();
        assert_eq!(
            ws.fx.cursor().glow.in_flight_tally().swallowed_no_echo,
            if secret { 5 } else { 0 },
            "{label}: the `swallowed_no_echo=` tally"
        );
        drop(app);
        drop(sink);
        // SAFETY: both fds are ours and open; the borrowed-fd sink did not close master.
        unsafe {
            libc::close(master);
            libc::close(slave);
        }
    }
}

/// The glow's in-flight press credits, read at a tick on the origin.
#[cfg(unix)]
fn erase_credits(app: &mut App, wid: WindowId) -> usize {
    let glow_cfg = app.glow_config();
    let geom = crate::cursor_glow::Geom {
        cw: 8,
        ch: 16,
        rows: 24,
        cols: 80,
        origin_x: 0,
        origin_y: 0,
        win_w: 640,
        win_h: 384,
        head: 0,
    };
    let glow = &mut app.windows.get_mut(&wid).unwrap().fx.cursor_mut().glow;
    glow.tick(
        Some((0, 0)),
        Instant::now(),
        &glow_cfg,
        geom,
        &mut Vec::new(),
    );
    glow.in_flight_tally().credits
}

/// A human paste, waited for until the ordering FIFO has drained it.
#[cfg(unix)]
fn paste_drained(
    app: &mut App,
    sink: &std::sync::Arc<aterm_session::sink::SinkWriter>,
    wid: WindowId,
    text: &str,
) {
    assert_eq!(
        app.input(
            wid,
            InputEvent::Paste(text.to_string(), crate::input::PasteFraming::AtDrain),
            Source::Human
        ),
        crate::input::InputOutcome::Ok
    );
    super::paste_order::await_drain_for_test(sink);
    assert!(!super::paste_order::is_ordering(sink), "the paste drained");
}

/// A Backspace into a committed IME run gives back one glyph's credit. The
/// host banks an IME commit as one press priced at its summed width, and
/// remembers the last typed dispatch's per-grapheme widths to price each
/// Backspace: `日本語` (6) ⌫ → 4 ⌫ → 2 ⌫ → 0.
#[cfg(unix)]
#[test]
fn a_backspace_into_an_ime_run_gives_back_one_glyphs_credit() {
    let (mut app, sink, master, slave) = pty_app(|_| {});
    let wid = WindowId(0);
    assert_eq!(erase_credits(&mut app, wid), 0);
    let ok = crate::input::InputOutcome::Ok;
    let backspace = |app: &mut App| {
        assert_eq!(
            app.input(wid, named(NamedKey::Backspace), Source::Human),
            ok
        );
    };
    assert_eq!(
        app.input(wid, InputEvent::Text("日本語".to_string()), Source::Human),
        ok
    );
    assert_eq!(erase_credits(&mut app, wid), 6, "one press, three glyphs");
    for want in [4, 2, 0] {
        backspace(&mut app);
        assert_eq!(erase_credits(&mut app, wid), want, "one glyph given back");
    }
    let wide = "w".repeat(super::ErasePriceMemory::CAP + 2);
    paste_drained(&mut app, &sink, wid, &wide);
    paste_drained(&mut app, &sink, wid, &wide);
    assert_eq!(
        app.windows[&wid].erase_prices.widths().len(),
        super::ErasePriceMemory::CAP,
        "the memory is bounded as a whole"
    );
    type_word(&mut app, wid, "de");
    assert_eq!(erase_credits(&mut app, wid), 2);
    paste_drained(&mut app, &sink, wid, "x\ny");
    for _ in 0..3 {
        backspace(&mut app);
        assert_eq!(
            erase_credits(&mut app, wid),
            2,
            "the paste's, newline included"
        );
    }
    backspace(&mut app);
    assert_eq!(erase_credits(&mut app, wid), 1, "past the paste: `e`");
    close_pty(app, sink, master, slave);
}

/// A Backspace erasing a zero-width glyph gives back a priced nothing. `\u{0301}日本` (a
/// base-less combining mark, then two CJK glyphs) prices 0, 2, 2 and banks one press of four
/// cells.
#[cfg(unix)]
#[test]
fn a_backspace_erasing_a_zero_width_glyph_gives_back_a_priced_nothing() {
    let mut widths = Vec::new();
    super::committed_grapheme_cells_into("\u{0301}日本", false, &mut widths);
    assert_eq!(
        widths,
        [0, 2, 2],
        "the mark is a priced zero inside the run"
    );
    assert_eq!(super::committed_text_cells("\u{0301}日本", false), 4);

    let (mut app, sink, master, slave) = pty_app(|_| {});
    let wid = WindowId(0);
    assert_eq!(erase_credits(&mut app, wid), 0, "seeded");
    let ok = crate::input::InputOutcome::Ok;
    type_word(&mut app, wid, "x");
    assert_eq!(erase_credits(&mut app, wid), 1);
    assert_eq!(
        app.input(
            wid,
            InputEvent::Text("\u{0301}日本".to_string()),
            Source::Human
        ),
        ok
    );
    assert_eq!(erase_credits(&mut app, wid), 5, "one key, then four cells");
    for (want, why) in [
        (3, "本 given back"),
        (1, "日 given back: the run is paid out"),
        (1, "the mark: a priced zero, and `x` untouched"),
        (0, "unpriced: the whole newest press — `x` — as ever"),
    ] {
        assert_eq!(
            app.input(wid, named(NamedKey::Backspace), Source::Human),
            ok
        );
        assert_eq!(erase_credits(&mut app, wid), want, "{why}");
    }
    close_pty(app, sink, master, slave);
}

/// A key typed after a paste keeps the paste's price zeros. A typed press
/// keeps the memory and appends its own widths behind the zeros, so the
/// memory is the row's erasable tail in order: under a stall `abc`, paste
/// `yz`, `d`, then Backspaces retire `d`, nothing for `z`, nothing for `y`,
/// then `c`.
#[cfg(unix)]
#[test]
fn a_key_typed_after_a_paste_keeps_the_pastes_price_zeros() {
    let (mut app, sink, master, slave) = pty_app(|_| {});
    let wid = WindowId(0);
    assert_eq!(erase_credits(&mut app, wid), 0, "seeded");
    let ok = crate::input::InputOutcome::Ok;

    type_word(&mut app, wid, "abc");
    assert_eq!(erase_credits(&mut app, wid), 3);
    paste_drained(&mut app, &sink, wid, "yz");
    type_word(&mut app, wid, "d");
    assert_eq!(
        erase_credits(&mut app, wid),
        4,
        "four presses, two pasted glyphs"
    );
    assert_eq!(
        app.windows[&wid].erase_prices.widths(),
        [1, 1, 1, 0, 0, 1],
        "the row's erasable tail, in order"
    );
    for (want, why) in [
        (3, "`d`: its own press"),
        (3, "`z`: the paste's, nothing of the ring"),
        (3, "`y`: the paste's, nothing of the ring"),
        (2, "`c`: past the paste, the run's again"),
    ] {
        assert_eq!(
            app.input(wid, named(NamedKey::Backspace), Source::Human),
            ok
        );
        assert_eq!(erase_credits(&mut app, wid), want, "{why}");
    }
    paste_drained(&mut app, &sink, wid, "q");
    assert_eq!(
        app.input(wid, InputEvent::Text("日本".to_string()), Source::Human),
        ok
    );
    assert_eq!(
        app.windows[&wid].erase_prices.widths(),
        [1, 1, 0, 2, 2],
        "the commit's graphemes behind the paste's zero"
    );
    assert_eq!(
        app.input(wid, named(NamedKey::ArrowLeft), Source::Human),
        ok
    );
    assert!(
        app.windows[&wid].erase_prices.widths().is_empty(),
        "an arrow moves the erase point: the memory is closed"
    );
    close_pty(app, sink, master, slave);
}

/// A paste the tty will not echo carries no delivery ticket, like the press
/// path: a paste into `read -s` / `sudo` / an `ssh` passphrase must not arm
/// a same-row insert licence that a program print on that row could spend
/// as a phantom.
#[cfg(unix)]
#[test]
fn a_paste_the_tty_will_not_echo_publishes_no_insert_receipt() {
    use aterm_effects::cursor_glow::InsertWidth;

    for (label, edit, ticketed) in TTY_MODES {
        let (mut app, sink, master, slave) = pty_app(edit);
        let wid = WindowId(0);
        paste_drained(&mut app, &sink, wid, "foo bar ");
        let now = Instant::now();
        let session = app.front_terminal(wid).unwrap().session;
        let ctx = &app.pool.get(session).unwrap().ctx;
        assert!(
            ctx.output_echo
                .sample(&ctx.sink, now)
                .last_accepted_at
                .is_some(),
            "{label}: the bytes reached the PTY regardless"
        );
        let batch = ctx.output_echo.deliveries_after(&ctx.sink, Some(0), now);
        let insert = batch.items.iter().flatten().find_map(|(_, t)| t.insert);
        assert_eq!(
            insert,
            ticketed.then_some(InsertWidth::Cells(8)),
            "{label}: the insert receipt"
        );
        assert_eq!(
            batch.latest,
            u64::from(ticketed),
            "{label}: no receipt at all"
        );
        let ws = &app.windows[&wid];
        assert!(
            !ws.fx.cursor().glow.move_licensed(now) && !ws.fx.cursor().trail.move_licensed(now),
            "{label}: a paste's gesture stamp is revoked at enqueue whatever the tty"
        );
        close_pty(app, sink, master, slave);
    }
}

/// A bare Tab the tty will not echo arms no insert and no gesture. A bare
/// Tab / ⌃V has `typed_forward == None`, so the tty's verdict is read
/// separately: a press the tty will not echo is neither a licence nor a
/// credit in the classes that read the tty.
#[cfg(unix)]
#[test]
fn a_bare_tab_the_tty_will_not_echo_arms_no_insert() {
    use aterm_effects::cursor_glow::InsertWidth;

    for (label, edit, arms) in TTY_MODES {
        let (mut app, sink, master, slave) = pty_app(edit);
        let wid = WindowId(0);
        assert_eq!(
            app.input(wid, named(NamedKey::Tab), Source::Human),
            crate::input::InputOutcome::Ok
        );
        {
            let ws = app.windows.get(&wid).unwrap();
            let now = Instant::now();
            assert_eq!(
                ws.fx.cursor().glow.insert_tally().delivered,
                u64::from(arms),
                "{label}: the inline Tab's insert arm"
            );
            assert_eq!(ws.fx.cursor().glow.move_licensed(now), arms, "{label}");
            assert_eq!(ws.fx.cursor().trail.move_licensed(now), arms, "{label}");
        }
        let pin = super::paste_order::pin_ordering_for_test(&sink);
        assert!(super::paste_order::is_ordering(&sink));
        assert_eq!(
            app.input(wid, named(NamedKey::Tab), Source::Human),
            crate::input::InputOutcome::Ok
        );
        drop(pin);
        super::paste_order::await_drain_for_test(&sink);
        assert!(!super::paste_order::is_ordering(&sink), "{label}: drained");
        let now = Instant::now();
        let session = app.front_terminal(wid).unwrap().session;
        let ctx = &app.pool.get(session).unwrap().ctx;
        let batch = ctx.output_echo.deliveries_after(&ctx.sink, Some(0), now);
        let insert = batch.items.iter().flatten().find_map(|(_, t)| t.insert);
        assert_eq!(
            insert,
            arms.then_some(InsertWidth::Unknown),
            "{label}: the queued Tab's receipt"
        );
        let classes: Vec<_> = batch.items.iter().flatten().map(|(_, t)| t.key).collect();
        assert_eq!(
            classes,
            if arms {
                vec![Some(DeliveredClass::Gesture)]
            } else {
                vec![]
            },
            "{label}: the gesture class rides the receipt only where the tty echoes"
        );
        assert_eq!(
            batch.latest,
            u64::from(arms),
            "{label}: one receipt or none"
        );
        close_pty(app, sink, master, slave);
    }
}

/// Typing `kitty` never wakes an animal on the input path: the screen scanner answers the echoed
/// word with the ambient word-cat, centred on it.
#[test]
fn typing_kitty_logs_one_episode_and_wakes_no_companion() {
    let mut app = App::headless_for_test();
    app.recompute_sparkle();
    assert!(
        app.sparkle.is_some(),
        "the default config resolves sparkle words ON"
    );
    let wid = WindowId(0);
    let now = std::time::Instant::now();
    assert!(!app.windows[&wid].fx.companions().cursor_cat.is_active());

    type_word(&mut app, wid, "kitty");
    assert!(
        !app.windows[&wid].fx.companions().cursor_cat.is_active(),
        "the typed word must NOT wake the CURSOR kitty"
    );
    assert!(
        app.windows[&wid]
            .fx
            .companions()
            .cursor_cat
            .static_deadline(now)
            .is_none(),
        "the companion owns no erase wake either — it was never presented"
    );
    assert_eq!(
        app.kitty_log.log().sightings,
        1,
        "one episode through the ordinary Kitty Log rules"
    );
    let entry = &app.kitty_log.log().entries[0];
    assert_eq!(
        entry.kitty_type, "head_peek",
        "no schema churn: the summon logs as the pinned type it renders"
    );
    assert_eq!(entry.magic, "none");

    type_word(&mut app, wid, "y");
    assert_eq!(
        app.kitty_log.log().sightings,
        1,
        "the completion was consumed — its tail re-triggers nothing"
    );
}

/// Keys alone build no cat momentum: the metric builds only from a real
/// keystroke correlated with its forward echo (the glow's
/// `take_momentum_pulse`, fed in `cursor_frame`).
#[test]
fn key_only_input_builds_no_cat_momentum() {
    let mut app = App::headless_for_test();
    app.recompute_sparkle();
    let wid = WindowId(0);
    app.config.cursor_trail_style = Some("nyan".to_string());
    let now = std::time::Instant::now();
    for _ in 0..40 {
        app.input(wid, key('j'), Source::Human);
    }
    assert_eq!(
        app.windows[&wid].fx.companions().cursor_cat.momentum(now),
        0.0,
        "keys with no correlated forward echo build zero cat momentum"
    );
    assert!(
        !app.windows[&wid].fx.companions().cursor_cat.is_active(),
        "…so a key-only stream never summons the cat over a dark ribbon"
    );
}

/// A word-breaking key (plain Enter) clears the run on the real path:
/// `kit` ⏎ `ty` was never the typed word.
#[test]
fn enter_breaks_the_typed_run() {
    let mut app = App::headless_for_test();
    app.recompute_sparkle();
    let wid = WindowId(0);
    type_word(&mut app, wid, "kit");
    app.input(wid, named(NamedKey::Enter), Source::Human);
    type_word(&mut app, wid, "ty");
    assert!(!app.windows[&wid].fx.companions().cursor_cat.is_active());
    assert_eq!(app.kitty_log.log().sightings, 0);
    type_word(&mut app, wid, " kitty");
    assert_eq!(
        app.kitty_log.log().sightings,
        1,
        "a contiguous word still summons"
    );
}

/// With sparkle words off, or the feline family off while other families
/// keep the master on (`[sparkle_words.feline] enabled = false`), nothing
/// can draw a cat, so the typed summon is inert: no companion, no log
/// entry (a `head_peek` row would land in a category the config can never
/// produce ambiently).
#[test]
fn summon_is_inert_with_sparkle_words_off() {
    use crate::app_config::{SparkleFelineConfig, SparkleWordsConfig};
    for (label, words) in [
        (
            "sparkle words off",
            SparkleWordsConfig {
                enabled: Some(false),
                ..Default::default()
            },
        ),
        (
            "feline family off",
            SparkleWordsConfig {
                feline: Some(SparkleFelineConfig {
                    enabled: Some(false),
                    ..Default::default()
                }),
                ..Default::default()
            },
        ),
    ] {
        let mut app = App::headless_for_test();
        app.config.sparkle_words = Some(words);
        app.prepared_sparkle = app.config.prepare_sparkle_runtime();
        app.sparkle_dirty = true;
        app.recompute_sparkle();
        assert!(
            app.sparkle.as_ref().is_none_or(|rs| !rs.cfg.feline),
            "{label}: resolved OFF, not merely unread"
        );
        let wid = WindowId(0);
        type_word(&mut app, wid, "kitty");
        assert!(
            !app.windows[&wid].fx.companions().cursor_cat.is_active(),
            "{label}"
        );
        assert_eq!(app.kitty_log.log().sightings, 0, "{label}");
    }
}

/// Unresolved is not off: with the vocabulary on, the first keystroke
/// summons before any frame has filled the cache, which is the state at
/// launch and for a headless instance driven over the control socket.
#[test]
fn a_typed_summon_before_the_first_frame_is_not_taken_for_effects_off() {
    let mut app = App::headless_for_test();
    assert!(
        app.sparkle.is_none() && app.sparkle_dirty,
        "fixture: the cache is unread, exactly as it is at launch"
    );
    let wid = WindowId(0);
    type_word(&mut app, wid, "kitty");
    assert!(
        app.kitty_log.log().sightings > 0,
        "a config with cats ON must summon on the first keystroke, drawn \
         frame or not"
    );
}

/// Every granted summon is a fresh `(session, ident)` episode, so the
/// dedupe ring (which absorbs recounts of one episode for its TTL) does not
/// eat a later summon of the same session.
#[test]
fn each_granted_summon_logs_a_fresh_episode() {
    let mut app = App::headless_for_test();
    app.recompute_sparkle();
    let wid = WindowId(0);
    let session = app.front_terminal(wid).unwrap().session;
    let now = std::time::Instant::now();
    app.record_typed_kitty(wid, session, now);
    assert_eq!(app.kitty_log.log().sightings, 1);
    app.record_typed_kitty(wid, session, now);
    assert_eq!(
        app.kitty_log.log().sightings,
        2,
        "a reused ident would have been absorbed by the ring for RING_TTL"
    );
}

/// The typed summon logs the cat on glass: its roster row wears the
/// companion verdict's look (the launch kitty, or the pinned favourite),
/// never `KittyLook::default()`.
#[test]
fn a_typed_summon_records_the_verdicts_look() {
    use aterm_effects::kitty_registry::{KittyLook, glyph_key};
    let mut app = App::headless_for_test();
    app.recompute_sparkle();
    let wid = WindowId(0);
    let session = app.front_terminal(wid).unwrap().session;
    let now = std::time::Instant::now();
    let launch = KittyLook::for_launch(App::TEST_LAUNCH_SEED);
    assert_eq!(
        app.companion_verdict(wid, session, now),
        launch,
        "precondition: nothing pinned"
    );

    app.record_typed_kitty(wid, session, now);
    let row = app
        .kitty_log
        .log()
        .collectibles
        .iter()
        .find(|item| item.key == glyph_key(launch.variant))
        .expect("the summon minted the launch kitty's head as a roster row")
        .clone();
    assert_eq!(
        (row.coat, row.iris),
        (launch.coat, launch.iris),
        "…in the launch coat"
    );

    let pinned = KittyLook {
        variant: aterm_effects::cat_glyphs_gen::CatGlyphId::S101,
        coat: (launch.coat + 3) % 16,
        ..KittyLook::default()
    }
    .normalized();
    assert_ne!(pinned.variant, launch.variant, "fixture: a different head");
    app.kitty_log.favourite(
        &aterm_effects::kitty_registry::KittySighting {
            kitty_type: aterm_effects::kitty_registry::KittyType::HeadPeek,
            magic: aterm_effects::kitty_registry::KittyMagic::None,
            shown_as: aterm_effects::kitty_registry::KittyShownAs::Cat,
            langs: aterm_lexicon::LangSet::EMPTY,
            traits: 0,
            look: pinned,
            ident: 0xF00D,
        },
        aterm_lexicon::Lexicon::builtin(),
        now,
        true,
    );
    assert_eq!(app.companion_verdict(wid, session, now), pinned);
    app.record_typed_kitty(wid, session, now);
    let row = app
        .kitty_log
        .log()
        .collectibles
        .iter()
        .find(|item| item.key == glyph_key(pinned.variant))
        .expect("the summon logged the pinned head")
        .clone();
    assert_eq!(
        (row.coat, row.iris),
        (pinned.coat, pinned.iris),
        "…in the pinned coat"
    );
}
