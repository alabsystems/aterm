// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::Arc;
use std::time::Instant;

use aterm_effects::cursor_glow::InsertWidth;

use aterm_core::terminal::Terminal;
use aterm_session::sink::SinkWriter;

use crate::cursor_glow::{Geom, GlowStyle};
use crate::input::{InputEvent, Source};
use crate::{App, WindowId};

/// Drive the genuine asynchronous input seam, then present a cold one-cell
/// program cursor delta before delivery is proven. A private sink keeps the
/// process-global paste-order registry isolated from other parallel tests.
fn program_move_after_paste(text: &str, queue_behind_existing: bool) -> bool {
    let mut pipe = [0; 2];
    assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
    let sink = Arc::new(SinkWriter::new(pipe[1]));
    let mut app = App::headless_for_test_with_sink(sink.clone());
    let wid = WindowId(0);
    let ordering_pin =
        queue_behind_existing.then(|| super::paste_order::pin_ordering_for_test(&sink));
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("rainbow kitty".to_string());
    let cfg = app.glow_config();
    assert!(cfg.enabled && matches!(cfg.style, GlowStyle::RainbowKitty));
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
    let mut out = Vec::new();
    let seed = Instant::now();
    assert_eq!(
        app.windows
            .get_mut(&wid)
            .unwrap()
            .fx
            .cursor_mut()
            .glow
            .tick(Some((2, 2)), seed, &cfg, geom, &mut out),
        0
    );
    assert_eq!(
        app.input(
            wid,
            InputEvent::Paste(text.to_string(), crate::input::PasteFraming::AtDrain),
            Source::Human
        ),
        crate::input::InputOutcome::Ok
    );

    let now = Instant::now();
    let glow = &mut app.windows.get_mut(&wid).unwrap().fx.cursor_mut().glow;
    let fingerprint = glow.tick(Some((2, 3)), now, &cfg, geom, &mut out);
    let admitted = fingerprint != 0
        || !out.is_empty()
        || !glow.halos().is_empty()
        || !glow.under_quads().is_empty();

    drop(ordering_pin);
    // Let the tiny ordered write release its registry key before closing
    // this fixture's descriptors.
    super::paste_order::await_drain_for_test(&sink);
    assert!(!super::paste_order::is_ordering(&sink));
    unsafe {
        libc::close(pipe[0]);
        libc::close(pipe[1]);
    }
    admitted
}

#[test]
fn empty_or_sanitizer_empty_paste_cannot_fund_a_program_trail() {
    // An input with no possible cursor movement arms no licence, and a
    // movement-capable paste is swallowed at its asynchronous enqueue
    // (enqueue is not delivery). The model side (`PressArmsLicense`,
    // `SwallowedKeyClearsLicense` and their mutants) is proved by aterm-spec's
    // registry sweep.
    for text in ["", "\x1b\x03\u{009b}\x7f", "\u{0301}\u{fe0f}\u{200d}"] {
        assert!(
            !program_move_after_paste(text, false),
            "paste with no sanitizer-surviving payload funded later program light"
        );
    }
    assert!(
        Terminal::paste_has_payload("中🙂"),
        "CJK/emoji bases remain movement-eligible"
    );
    assert!(
        !program_move_after_paste("中🙂", false),
        "even the first FIFO paste is asynchronous and cannot fund concurrent program motion"
    );
    assert!(
        !program_move_after_paste("中🙂", true),
        "a paste queued behind existing egress cannot fund concurrent program motion"
    );
}

/// Drive a real paste through `App::input`, let the FIFO writer land it,
/// read the session's delivery register as the render prelude does, arm
/// the engine with it, and present the echo hop `(2, 2) → (2, 2 + hop)`.
/// Returns `(spawns, the ring's last licence token, the ticket's width)`.
fn delivered_paste_then_hop(
    text: &str,
    hop: u16,
    apply: bool,
) -> (u64, &'static str, Option<InsertWidth>) {
    let mut pipe = [0; 2];
    assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
    let sink = Arc::new(SinkWriter::new(pipe[1]));
    let mut app = App::headless_for_test_with_sink(sink.clone());
    let wid = WindowId(0);
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("rainbow kitty".to_string());
    let cfg = app.glow_config();
    let geom = Geom {
        cw: 8,
        ch: 16,
        rows: 6,
        cols: 80,
        origin_x: 0,
        origin_y: 0,
        win_w: 640,
        win_h: 96,
        head: 0,
    };
    let mut out = Vec::new();
    let seed = Instant::now();
    app.windows
        .get_mut(&wid)
        .unwrap()
        .fx
        .cursor_mut()
        .glow
        .tick(Some((2, 2)), seed, &cfg, geom, &mut out);
    assert_eq!(
        app.input(
            wid,
            InputEvent::Paste(text.to_string(), crate::input::PasteFraming::AtDrain),
            Source::Human
        ),
        crate::input::InputOutcome::Ok
    );
    // The FIFO drains: the writer thread's completed write is the
    // delivery edge.
    super::paste_order::await_drain_for_test(&sink);
    assert!(!super::paste_order::is_ordering(&sink));
    // The frame that observes the paste's delivery (see
    // `typed_kitty_summon_tests::observing_frame`).
    let now = super::typed_kitty_summon_tests::observing_frame(
        super::typed_kitty_summon_tests::delivery_edge(&app, wid, 1),
        1,
    );
    let session = app.front_terminal(wid).unwrap().session;
    let ctx = &app.pool.get(session).unwrap().ctx;
    let batch = ctx.output_echo.deliveries_after(&ctx.sink, Some(0), now);
    let ticket = batch.items[0].map(|(_, t)| t);
    let width = ticket.and_then(|t| t.insert);
    let glow = &mut app.windows.get_mut(&wid).unwrap().fx.cursor_mut().glow;
    if apply
        && let Some((at, ticket)) = batch.items[0]
        && let Some(width) = ticket.insert
    {
        glow.note_insert_delivered_from(at, at, width);
    }
    glow.tick(Some((2, 2 + hop)), now, &cfg, geom, &mut out);
    let licence = glow.admission_log().last().map_or("no-row", |r| r.licence);
    let spawns = glow.spawns();
    unsafe {
        libc::close(pipe[0]);
        libc::close(pipe[1]);
    }
    (spawns, licence, width)
}

/// A body whose width the text cannot say (a line break, a Tab) is an
/// insert of unknown width bounded at the engine's gesture cap, so a
/// same-row placeholder echo (Claude Code's `[Pasted text #1 +N lines] `)
/// no wider than that is licensed, and a wider one is not.
#[cfg(unix)]
#[test]
fn a_multiline_or_tabbed_paste_is_an_insert_of_unknown_width() {
    let cap = crate::cursor_glow::CursorGlow::INSERT_GESTURE_CELLS;
    let (spawns, licence, width) = delivered_paste_then_hop("one\ntwo\nthree", 27, true);
    assert_eq!(
        width,
        Some(InsertWidth::Unknown),
        "a multi-line body is the unknown class, bounded at the gesture cap"
    );
    assert_eq!(
        (spawns, licence),
        (1, "insert"),
        "a placeholder-wide echo is licensed"
    );
    let (spawns, _, width) = delivered_paste_then_hop("a\tb", cap + 1, true);
    assert_eq!(width, Some(InsertWidth::Unknown));
    assert_eq!(spawns, 0, "…and an echo past the cap is not");
}

/// A queued key's receipt re-stamps only the window that typed it: the dispatch
/// banks the press credit on the dispatching window's engine alone, so the
/// ticket's typed half names that window and is applied there only; window B,
/// which banked nothing, stays unlicensed. The insert half is session-wide (a
/// paste needs no per-window credit), so B still arms a delivered paste's
/// licence. Also run under `comet`, reading A's classic-trail typed twin.
#[cfg(unix)]
#[test]
fn a_queued_keys_receipt_stamps_only_the_window_that_typed_it() {
    use super::typed_kitty_summon_tests::{delivery_edge, observing_frame};
    use crate::app_render::CursorFxInputs;
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers};
    for style in ["rainbow kitty", "comet"] {
        for queued in [false, true] {
            let mut pipe = [0; 2];
            assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
            let sink = Arc::new(SinkWriter::new(pipe[1]));
            let mut app = App::headless_for_test_with_sink(sink.clone());
            app.recompute_sparkle();
            app.config.motion = Some("full".into());
            app.config.cursor_trail = Some(true);
            app.config.cursor_trail_style = Some(style.to_string());
            app.config.trail_sounds = Some(false);
            let wid_a = WindowId(0);
            app.windows.get_mut(&wid_a).unwrap().focused = true;
            app.frontmost_window = Some(wid_a);
            let wid_b = app
                .open_active_session_in_new_window_logical()
                .expect("a second window on the session");
            app.windows.get_mut(&wid_b).unwrap().focused = true;
            // Both windows baseline on the register.
            for w in [wid_a, wid_b] {
                let mut seed = CursorFxInputs::sample_for_test(Instant::now());
                seed.cur = Some((2, 2));
                app.cursor_frame(w, seed).expect("seed");
            }
            let pin = queued.then(|| super::paste_order::pin_ordering_for_test(&sink));
            app.frontmost_window = Some(wid_a);
            let pressed = Instant::now();
            assert_eq!(
                app.input(
                    wid_a,
                    InputEvent::Key {
                        key: Key::Character('a'),
                        mods: Modifiers::empty(),
                        base_layout: None,
                        layout_text: None,
                        event_type: KeyEventType::Press,
                    },
                    Source::Human
                ),
                crate::input::InputOutcome::Ok
            );
            drop(pin);
            super::paste_order::await_drain_for_test(&sink);
            assert!(!super::paste_order::is_ordering(&sink));
            // The frames that observe the key: its dispatch inline, its
            // receipt queued ([`observing_frame`]).
            let edge = if queued {
                delivery_edge(&app, wid_a, 1)
            } else {
                pressed
            };
            let now = observing_frame(edge, 1);
            let mut live = CursorFxInputs::sample_for_test(now);
            live.cur = Some((2, 2));
            app.cursor_frame(wid_b, live).expect("B's prelude");
            let b = &app.windows[&wid_b];
            assert!(
                !b.fx.cursor().glow.move_licensed(now),
                "{style} queued={queued}: window B typed nothing and banked nothing, yet holds a typed licence"
            );
            assert!(
                !b.fx.cursor().trail.move_licensed(now),
                "{style} queued={queued}: the classic trail too"
            );
            assert_eq!(b.fx.cursor().glow.in_flight_tally().credits, 0);
            // …while A, which typed it, is licensed either way.
            let mut live = CursorFxInputs::sample_for_test(now);
            live.cur = Some((2, 2));
            app.cursor_frame(wid_a, live).expect("A's prelude");
            assert!(
                app.windows[&wid_a].fx.cursor().glow.move_licensed(now),
                "{style} queued={queued}: window A holds the key's licence"
            );
            if style == "comet" {
                // The trail's typed twin, re-stamped at delivery on the
                // dispatching window (its tick is disabled under rainbow
                // kitty, where it clears its hints every frame).
                assert!(
                    app.windows[&wid_a].fx.cursor().trail.move_licensed(now),
                    "{style} queued={queued}: window A's classic trail holds the key's typed twin"
                );
            }
            // The one-shot classes are the dispatching window's too: an
            // Enter typed in A behind a paste re-licenses A, never B.
            let pin = queued.then(|| super::paste_order::pin_ordering_for_test(&sink));
            let pressed = Instant::now();
            assert_eq!(
                app.input(
                    wid_a,
                    InputEvent::Key {
                        key: Key::Named(aterm_types::keyboard::NamedKey::Enter),
                        mods: Modifiers::empty(),
                        base_layout: None,
                        layout_text: None,
                        event_type: KeyEventType::Press,
                    },
                    Source::Human
                ),
                crate::input::InputOutcome::Ok
            );
            drop(pin);
            super::paste_order::await_drain_for_test(&sink);
            let edge = if queued {
                delivery_edge(&app, wid_a, 1)
            } else {
                pressed
            };
            let now = observing_frame(edge, 1);
            let mut live = CursorFxInputs::sample_for_test(now);
            live.cur = Some((2, 2));
            app.cursor_frame(wid_b, live).expect("B's prelude");
            assert!(
                !app.windows[&wid_b].fx.cursor().glow.move_licensed(now),
                "{style} queued={queued}: window B did not press Enter, yet holds its return licence"
            );
            let mut live = CursorFxInputs::sample_for_test(now);
            live.cur = Some((2, 2));
            app.cursor_frame(wid_a, live).expect("A's prelude");
            assert!(
                app.windows[&wid_a].fx.cursor().glow.move_licensed(now),
                "{style} queued={queued}: window A holds the Enter's licence"
            );
            // The insert half stays session-wide: a paste delivered on the
            // session arms B's insert licence (Rainbow Kitty only; under
            // `comet` it arms nothing).
            assert_eq!(
                app.input(
                    wid_a,
                    InputEvent::Paste("foo bar ".to_string(), crate::input::PasteFraming::AtDrain),
                    Source::Human
                ),
                crate::input::InputOutcome::Ok
            );
            super::paste_order::await_drain_for_test(&sink);
            let now = Instant::now();
            let mut live = CursorFxInputs::sample_for_test(now);
            live.cur = Some((2, 2));
            app.cursor_frame(wid_b, live).expect("B's prelude");
            assert_eq!(
                app.windows[&wid_b]
                    .fx
                    .cursor()
                    .glow
                    .insert_tally()
                    .delivered,
                u64::from(style == "rainbow kitty"),
                "{style} queued={queued}: the delivered paste arms B too"
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

/// A real paste through `App::input` and the FIFO writer, then one
/// [`App::cursor_frame`] prelude tick presenting the echo hop
/// `(2, 2) → (2, 2 + hop)`. Without `baselined` the window's `delivery_seen` is
/// forgotten before the paste (a session switch, a first read). Returns
/// `(spawns, the ring's last licence token, delivery_seen)`.
#[cfg(unix)]
fn prelude_paste_then_hop(
    text: &str,
    hop: u16,
    baselined: bool,
) -> (u64, &'static str, Option<(u64, u64)>) {
    use crate::app_render::CursorFxInputs;

    let mut pipe = [0; 2];
    assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
    let sink = Arc::new(SinkWriter::new(pipe[1]));
    let mut app = App::headless_for_test_with_sink(sink.clone());
    let wid = WindowId(0);
    app.config.motion = Some("full".into());
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("rainbow kitty".to_string());
    app.config.trail_sounds = Some(false);
    app.windows.get_mut(&wid).unwrap().focused = true;
    app.frontmost_window = Some(wid);
    let session = app.front_terminal(wid).unwrap().session;
    // The seed tick: the engines learn the caret and the prelude reads the
    // register for the first time (nothing to apply; the window baselines
    // at the tracker's newest serial, 0).
    let mut seed = CursorFxInputs::sample_for_test(Instant::now());
    seed.cur = Some((2, 2));
    app.cursor_frame(wid, seed).expect("seed tick");
    assert_eq!(
        app.windows[&wid].delivery_seen,
        Some((session, 0)),
        "the first read baselines at the tracker's newest serial"
    );
    if !baselined {
        app.windows.get_mut(&wid).unwrap().delivery_seen = None;
    }
    assert_eq!(
        app.input(
            wid,
            InputEvent::Paste(text.to_string(), crate::input::PasteFraming::AtDrain),
            Source::Human
        ),
        crate::input::InputOutcome::Ok
    );
    super::paste_order::await_drain_for_test(&sink);
    assert!(!super::paste_order::is_ordering(&sink));
    // The frame that first observes the echo: the prelude reads the
    // receipt, arms the engine, and the tick judges the hop, presented on
    // the delivery's clock (`typed_kitty_summon_tests::observing_frame`).
    let now = super::typed_kitty_summon_tests::observing_frame(
        super::typed_kitty_summon_tests::delivery_edge(&app, wid, 1),
        1,
    );
    let mut live = CursorFxInputs::sample_for_test(now);
    live.cur = Some((2, 2 + hop));
    app.cursor_frame(wid, live).expect("live tick");
    let ws = &app.windows[&wid];
    let licence = ws
        .fx
        .cursor()
        .glow
        .admission_log()
        .last()
        .map_or("no-row", |r| r.licence);
    let out = (ws.fx.cursor().glow.spawns(), licence, ws.delivery_seen);
    drop(app);
    drop(sink);
    unsafe {
        libc::close(pipe[0]);
        libc::close(pipe[1]);
    }
    out
}

/// The prelude in `cursor_frame` applies a real paste's receipt on the frame
/// observing the echo (the hop of exactly its width is `licence=insert`) and
/// records the serial as seen. A window with no baseline baselines at the
/// newest serial and applies nothing. Fails under an inverted freshness gate, a
/// baseline that applies on the first read, or a `latest` that does not advance.
#[cfg(unix)]
#[test]
fn the_render_prelude_applies_a_delivered_paste_and_baselines_a_fresh_window() {
    let (spawns, licence, seen) = prelude_paste_then_hop("foo bar ", 8, true);
    assert_eq!(licence, "insert", "the prelude armed the engine");
    assert_eq!(spawns, 1, "the delivered paste's echo is licensed");
    let session = seen.map(|(session, _)| session).expect("the window read");
    assert_eq!(seen, Some((session, 1)), "the receipt's serial is seen");

    let (spawns, licence, seen) = prelude_paste_then_hop("foo bar ", 8, false);
    assert_ne!(
        licence, "insert",
        "a window with no baseline applies nothing on its first read"
    );
    assert_eq!(spawns, 0, "the hop is program output to it");
    assert_eq!(
        seen,
        Some((session, 1)),
        "…and it baselines at the newest serial all the same"
    );
}

/// A paste followed by a frame's worth of queued keys (the glow engine holds up
/// to 128 unpaid) keeps its oldest receipt through the real writer and render
/// prelude, and the paste's insert receipt owns its own first visible echo.
#[cfg(unix)]
#[test]
fn a_paste_before_33_to_128_queued_keys_keeps_its_first_visible_echo() {
    use crate::app_render::CursorFxInputs;
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers};

    for queued_keys in [33, aterm_effects::cursor_glow::TYPED_STAMP_DEPTH] {
        let mut pipe = [0; 2];
        assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
        let sink = Arc::new(SinkWriter::new(pipe[1]));
        let mut app = App::headless_for_test_with_sink(sink.clone());
        let wid = WindowId(0);
        app.config.motion = Some("full".into());
        app.config.cursor_trail = Some(true);
        app.config.cursor_trail_style = Some("rainbow kitty".to_string());
        app.config.trail_sounds = Some(false);
        app.windows.get_mut(&wid).unwrap().focused = true;
        app.frontmost_window = Some(wid);
        let session = app.front_terminal(wid).unwrap().session;
        let mut seed = CursorFxInputs::sample_for_test(Instant::now());
        seed.cur = Some((2, 2));
        app.cursor_frame(wid, seed).expect("baseline and seed");
        assert_eq!(app.windows[&wid].delivery_seen, Some((session, 0)));

        let pin = super::paste_order::pin_ordering_for_test(&sink);
        assert_eq!(
            app.input(
                wid,
                InputEvent::Paste("foo bar ".to_string(), crate::input::PasteFraming::AtDrain),
                Source::Human,
            ),
            crate::input::InputOutcome::Ok,
        );
        for _ in 0..queued_keys {
            assert_eq!(
                app.input(
                    wid,
                    InputEvent::Key {
                        key: Key::Character('k'),
                        mods: Modifiers::empty(),
                        base_layout: None,
                        layout_text: None,
                        event_type: KeyEventType::Press,
                    },
                    Source::Human,
                ),
                crate::input::InputOutcome::Ok,
            );
        }
        drop(pin);
        super::paste_order::await_drain_for_test(&sink);
        assert!(!super::paste_order::is_ordering(&sink));
        // The frame that observes the batch's delivery (see
        // `typed_kitty_summon_tests::observing_frame`).
        let now = super::typed_kitty_summon_tests::observing_frame(
            super::typed_kitty_summon_tests::delivery_edge(&app, wid, queued_keys as u64 + 1),
            1,
        );
        let ctx = &app.pool.get(session).unwrap().ctx;
        let batch = ctx.output_echo.deliveries_after(&ctx.sink, Some(0), now);
        assert_eq!(
            (batch.latest, batch.evicted),
            (queued_keys as u64 + 1, 0),
            "{queued_keys} queued keys may not evict the preceding paste"
        );
        assert_eq!(
            batch.items[0].and_then(|(_, t)| t.insert),
            Some(InsertWidth::Cells(8)),
            "the oldest receipt is the paste"
        );

        let mut live = CursorFxInputs::sample_for_test(now);
        live.cur = Some((2, 10));
        app.cursor_frame(wid, live).expect("first echoed frame");
        let ws = &app.windows[&wid];
        assert_eq!(
            ws.fx.cursor().glow.insert_tally().delivered,
            1,
            "{queued_keys} keys: the prelude must apply the paste receipt"
        );
        assert_eq!(
            ws.fx.cursor().glow.insert_tally().lit,
            1,
            "{queued_keys} later keys cannot steal the paste's own echo"
        );
        assert_eq!(ws.fx.cursor().glow.spawns(), 1);
        assert!(
            !ws.fx.cursor().glow_scratch.is_empty(),
            "{queued_keys} keys: the licensed sweep must emit visible light"
        );
        assert_eq!(
            ws.delivery_seen,
            Some((session, queued_keys as u64 + 1)),
            "the prelude consumed the whole ordered batch"
        );
        drop(app);
        drop(sink);
        unsafe {
            libc::close(pipe[0]);
            libc::close(pipe[1]);
        }
    }
}

/// The FIFO can finish a paste and its queued keys before the child
/// repaints, and the child may then show the paste in one output frame and
/// the keys in a second. Replaying those two writes must not spend the
/// later keys' press credits on the paste's cells, or the second 33-cell
/// echo falls below the typed share floor and the rainbow has a dark gap.
#[cfg(unix)]
#[test]
fn a_delayed_paste_frame_leaves_later_queued_keys_for_their_own_echo() {
    use crate::app_render::CursorFxInputs;
    use crate::cursor_glow::ProbeTrust;
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers};

    let mut pipe = [0; 2];
    assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
    let sink = Arc::new(SinkWriter::new(pipe[1]));
    let mut app = App::headless_for_test_with_sink(sink.clone());
    let wid = WindowId(0);
    app.config.motion = Some("full".into());
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("rainbow kitty".to_string());
    app.config.trail_sounds = Some(false);
    app.windows.get_mut(&wid).unwrap().focused = true;
    app.frontmost_window = Some(wid);
    let mut seed = CursorFxInputs::sample_for_test(Instant::now());
    seed.cur = Some((2, 2));
    app.cursor_frame(wid, seed).expect("baseline and seed");

    let pin = super::paste_order::pin_ordering_for_test(&sink);
    assert_eq!(
        app.input(
            wid,
            InputEvent::Paste("p".repeat(33), crate::input::PasteFraming::AtDrain),
            Source::Human,
        ),
        crate::input::InputOutcome::Ok,
    );
    for _ in 0..33 {
        assert_eq!(
            app.input(
                wid,
                InputEvent::Key {
                    key: Key::Character('k'),
                    mods: Modifiers::empty(),
                    base_layout: None,
                    layout_text: None,
                    event_type: KeyEventType::Press,
                },
                Source::Human,
            ),
            crate::input::InputOutcome::Ok,
        );
    }
    drop(pin);
    super::paste_order::await_drain_for_test(&sink);
    assert!(!super::paste_order::is_ordering(&sink));
    // The paste's and the 33 keys' receipts: the output frames below are
    // the first and second to observe that delivery, presented on its
    // clock (`typed_kitty_summon_tests::observing_frame`).
    let edge = super::typed_kitty_summon_tests::delivery_edge(&app, wid, 34);

    let present_output = |app: &mut App, bytes: &[u8], nth: u32| {
        let mut row = Vec::new();
        let (cur, print_anchor) = {
            let front = app.front_terminal(wid).unwrap();
            let mut term = crate::term_lock(&front.term);
            term.process(bytes);
            let cursor = term.cursor();
            term.row_cols_into(usize::from(cursor.row), &mut row);
            ((cursor.row, cursor.col), term.print_anchor())
        };
        let now = super::typed_kitty_summon_tests::observing_frame(edge, nth);
        app.windows.get_mut(&wid).unwrap().poof_row_buf = row;
        let mut frame = CursorFxInputs::sample_for_test(now);
        frame.cur = Some(cur);
        frame.row_probe = Some((cur.0, cur.1, ProbeTrust::Full));
        frame.print_anchor = print_anchor;
        app.cursor_frame(wid, frame).expect("output frame");
        cur
    };

    let paste_output = [b"\x1b[3;3H".as_slice(), b"p".repeat(33).as_slice()].concat();
    assert_eq!(present_output(&mut app, &paste_output, 1), (2, 35));
    let ws = &app.windows[&wid];
    assert_eq!(ws.fx.cursor().glow.insert_tally().delivered, 1);
    assert_eq!(
        ws.fx.cursor().glow.insert_tally().lit,
        1,
        "the paste's first frame must use its own delivered insert licence"
    );
    assert_eq!(
        ws.fx.cursor().glow.in_flight_tally().credits,
        33,
        "later queued keys have not echoed yet"
    );

    // One model credit stands for this 33-key batch: project the engine's
    // first-frame decision onto the derived licence transition; the
    // typed-class verdict (insert still armed, keys spent) is refused.
    let model = aterm_models_effects::cursor_hint_license_model();
    let mut before = model.init_state();
    for action in [
        "PasteEnqueues",
        "PressBehindQueuedInsert",
        "WriteCompletesArmsInsertLicence",
    ] {
        assert!(model.fire(action, &mut before), "{action}");
    }
    let key_credit = i64::from(ws.fx.cursor().glow.in_flight_tally().credits == 33);
    let insert_lit = i64::try_from(ws.fx.cursor().glow.insert_tally().lit).unwrap();
    let licensed = i64::try_from(ws.fx.cursor().glow.admission_tally().licensed).unwrap();
    let spawns = i64::try_from(ws.fx.cursor().glow.spawns()).unwrap();
    let mut observed = before.clone();
    observed.insert("insert_hint", 1 - insert_lit);
    observed.insert("hint", key_credit);
    observed.insert("later_key_credit", key_credit);
    observed.insert("spent", 1 - key_credit);
    observed.insert("consumed", licensed);
    observed.insert("admissions", licensed);
    observed.insert("spawns", spawns);
    observed.insert("births", licensed);
    observed.insert("resident", i64::from(ws.fx.cursor().glow.is_active()));
    observed.insert("licensed_tally", licensed);
    let (ok, why) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &before,
        &observed,
        Some("DelayedInsertEchoPrecedesLaterKey"),
        "real delayed paste frame before queued keys",
    );
    assert!(ok, "real cursor classifier rejected by model: {why}");
    let mut stolen = observed.clone();
    stolen.insert("insert_hint", 1);
    stolen.insert("hint", 0);
    stolen.insert("later_key_credit", 0);
    stolen.insert("spent", 1);
    let (ok, _) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &before,
        &stolen,
        Some("DelayedInsertEchoPrecedesLaterKey"),
        "paste frame steals later keys negative control",
    );
    assert!(!ok, "the old typed-class verdict must be refused");

    assert_eq!(present_output(&mut app, &b"k".repeat(33), 2), (2, 68));
    let ws = &app.windows[&wid];
    assert_eq!(ws.fx.cursor().glow.in_flight_tally().credits, 0);
    let ribbon = ws
        .fx
        .cursor()
        .glow
        .v2_ribbon()
        .expect("rainbow kitty engaged");
    for col in 35..68 {
        assert!(
            ribbon
                .cells()
                .iter()
                .any(|cell| cell.row == 2 && cell.col == col && !cell.leaving()),
            "queued key at col {col} left a gap in the live ribbon"
        );
    }
    drop(app);
    drop(sink);
    unsafe {
        libc::close(pipe[0]);
        libc::close(pipe[1]);
    }
}
