// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::time::Instant;

use super::{
    classify_press, committed_char_cells, committed_text_cells, committed_text_moves_cursor,
};
use crate::input::{InputEvent, Source};
use crate::{App, WindowId, term_lock};
use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};

fn key(event_type: KeyEventType) -> InputEvent {
    InputEvent::Key {
        key: Key::Character('a'),
        mods: Modifiers::empty(),
        base_layout: None,
        layout_text: None,
        event_type,
    }
}

/// A bare modifier / lock key press at the seam.
fn modifier_press(key: Key) -> InputEvent {
    InputEvent::Key {
        key,
        mods: Modifiers::empty(),
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    }
}
#[test]
fn fx_celebration_enter_uses_the_committed_input_path() {
    use aterm_effects::kitty_sing::{CelebrateOn, song_signature};
    let enter = || InputEvent::Key {
        key: Key::Named(NamedKey::Enter),
        mods: Modifiers::empty(),
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    };
    for (event, fires) in [
        (enter(), true),
        (InputEvent::KeySequence(b"echo done\r".to_vec()), true),
        (InputEvent::KeySequence(b"echo done\n".to_vec()), true),
        (InputEvent::KeySequence(b"echo done".to_vec()), false),
        (InputEvent::Text("done".to_string()), false),
        (
            InputEvent::Paste("done\n".to_string(), crate::input::PasteFraming::AtDrain),
            false,
        ),
    ] {
        let mut app = App::headless_for_test();
        app.config =
            aterm_toml::from_str("cursor_trail = true\ncursor_trail_style = \"rainbow kitty pet\"")
                .expect("valid fixture");
        let wid = WindowId(0);
        app.fx_control(crate::FxCtlOp::Celebrate {
            sig: song_signature('C'),
            bars: 2,
            on: CelebrateOn::Enter,
        })
        .expect("positive fixture arms");
        assert!(!app.windows[&wid].fx.companions().kitty_sing.external_live());
        let _ = app.input(wid, event, Source::Human);
        let sing = &app.windows[&wid].fx.companions().kitty_sing;
        assert_eq!(
            sing.external_live(),
            fires,
            "only accepted Enter-shaped input fires"
        );
        assert_eq!(
            sing.armed_external().is_none(),
            fires,
            "the edge consumes exactly its arm"
        );
    }
}

#[test]
fn typing_press_snaps_and_deselects_inert_and_repeat_and_release_do_not() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    {
        let mut t = term_lock(&term);
        // Enough output to build scrollback, then scroll into history and select.
        t.process("x\r\n".repeat(64).as_bytes());
        t.scroll_to_top();
        assert_ne!(t.grid().display_offset(), 0, "viewport is in history");
        t.text_selection_mut()
            .start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
        t.text_selection_mut()
            .update_selection(0, 5, SelectionSide::Left);
        t.text_selection_mut().complete_selection();
        assert!(t.text_selection().has_selection());
    }
    // Release first: no snap, no deselect (a legacy release also encodes
    // nothing, so the event is a no-op at the PTY).
    let _ = app.input(wid, key(KeyEventType::Release), Source::Human);
    {
        let t = term_lock(&term);
        assert_ne!(t.grid().display_offset(), 0, "release must not snap");
        assert!(
            t.text_selection().has_selection(),
            "release must not deselect"
        );
    }
    // Repeat (the same held press continuing): neither.
    let _ = app.input(wid, key(KeyEventType::Repeat), Source::Human);
    {
        let t = term_lock(&term);
        assert_ne!(t.grid().display_offset(), 0, "repeat must not snap");
        assert!(
            t.text_selection().has_selection(),
            "repeat must not deselect"
        );
    }
    // Inert: every bare modifier and lock key, each on its own. None may
    // move the viewport or drop the highlight (⌘-C must have something to
    // copy).
    for k in [
        Key::Named(NamedKey::SuperLeft),
        Key::Named(NamedKey::SuperRight),
        Key::Named(NamedKey::ShiftLeft),
        Key::Named(NamedKey::ControlLeft),
        Key::Named(NamedKey::AltLeft),
        Key::Named(NamedKey::MetaLeft),
        Key::Named(NamedKey::HyperLeft),
        Key::Named(NamedKey::CapsLock),
        Key::Named(NamedKey::NumLock),
        Key::Named(NamedKey::ScrollLock),
    ] {
        let _ = app.input(wid, modifier_press(k.clone()), Source::Human);
        let t = term_lock(&term);
        assert_ne!(
            t.grid().display_offset(),
            0,
            "bare {k:?} must not snap the viewport"
        );
        assert!(
            t.text_selection().has_selection(),
            "bare {k:?} must not clear the selection"
        );
    }
    // Press: snaps to the live bottom and clears the selection in one pass.
    let _ = app.input(wid, key(KeyEventType::Press), Source::Human);
    let t = term_lock(&term);
    assert_eq!(t.grid().display_offset(), 0, "press snaps to bottom");
    assert!(!t.text_selection().has_selection(), "press deselects");
}

#[test]
fn zero_width_commits_and_stationary_kills_arm_no_cursor_move() {
    use std::time::Duration;

    use crate::cursor_glow::{CursorGlow, Geom, GlowStyle};
    use crate::cursor_trail::TrailConfig;

    let combining = InputEvent::Text("\u{0301}\u{fe0f}\u{200d}".to_string());
    assert!(!committed_text_moves_cursor("\u{0301}\u{fe0f}\u{200d}"));
    assert_eq!(classify_press(&combining).typed_forward, None);
    assert!(committed_text_moves_cursor("中🙂"));
    for (text, cells) in [
        ("⚠️", 2),
        ("🇺🇸", 2),
        ("👨‍👩‍👧‍👦", 2),
        ("中", 2),
        ("中🙂", 4),
        // Spacing vowel signs take a cell of their own; a ZWJ folds into its base.
        ("\u{915}\u{93f}\u{924}\u{93e}\u{92c}", 5),
        ("\u{2764}\u{200d}\u{1f525}", 1),
    ] {
        assert_eq!(
            committed_text_cells(text, false),
            cells,
            "Text/IME credit must match terminal grapheme materialization for {text:?}"
        );
    }
    assert_eq!(
        committed_text_cells("\u{0301}\u{fe0f}\u{200d}", false),
        0,
        "combining/VS/joiner-only commits own no cells"
    );
    assert_eq!(
        committed_text_cells("°", false),
        1,
        "single-width mode prices an ambiguous grapheme as one terminal cell"
    );
    assert_eq!(
        committed_text_cells("°", true),
        2,
        "CJK ambiguous-double mode prices the same grapheme as two terminal cells"
    );
    assert_eq!(committed_char_cells('°', false), 1);
    assert_eq!(
        committed_char_cells('°', true),
        2,
        "direct Character input follows the same terminal CJK-width mode"
    );
    assert_eq!(
        classify_press(&InputEvent::Text("中🙂".to_string())).typed_forward,
        Some(true),
        "CJK and emoji bases retain typed-cell provenance"
    );

    let ctrl_k = InputEvent::Key {
        key: Key::Character('k'),
        mods: Modifiers::CTRL,
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    };
    let class = classify_press(&ctrl_k);
    assert!(class.kill_key && !class.kill_moves);

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
    let trail_cfg = TrailConfig {
        enabled: true,
        duration: Duration::from_millis(300),
        max_len: 24,
        color: 0x0050_FA7B,
        intensity: 0.0,
        warmth: 0.0,
    };
    for arm_glow in [
        CursorGlow::note_user_gesture as fn(&mut CursorGlow, Instant),
        CursorGlow::note_return,
        CursorGlow::note_reflow,
    ] {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.config.cursor_trail = Some(true);
        app.config.cursor_trail_style = Some("rainbow kitty".to_string());
        let glow_cfg = app.glow_config();
        assert!(matches!(glow_cfg.style, GlowStyle::RainbowKitty));
        let seed = Instant::now();
        let mut glow_out = Vec::new();
        let mut trail_out = Vec::new();
        {
            let ws = app.windows.get_mut(&wid).unwrap();
            ws.fx
                .cursor_mut()
                .glow
                .tick(Some((0, 0)), seed, &glow_cfg, geom, &mut glow_out);
            ws.fx
                .cursor_mut()
                .trail
                .tick(Some((0, 0)), seed, &trail_cfg, &mut trail_out);
            // Simulate swallowed older movement gestures. Ctrl-K is newer,
            // stationary input and must supersede them without replacing
            // them with another move licence.
            arm_glow(&mut ws.fx.cursor_mut().glow, seed);
            ws.fx.cursor_mut().trail.note_user_gesture(seed);
        }
        // The headless PTY peer may already be closed; the supersession under
        // test is a pre-egress side effect.
        let _ = app.input(wid, ctrl_k.clone(), Source::Human);
        let moved = Instant::now();
        let ws = app.windows.get_mut(&wid).unwrap();
        let glow_fp =
            ws.fx
                .cursor_mut()
                .glow
                .tick(Some((0, 1)), moved, &glow_cfg, geom, &mut glow_out);
        let trail_fp =
            ws.fx
                .cursor_mut()
                .trail
                .tick(Some((0, 1)), moved, &trail_cfg, &mut trail_out);
        assert_eq!(glow_fp, 0, "stationary kill clears prior glow licences");
        assert_eq!(trail_fp, 0, "stationary kill clears prior trail licences");
        assert!(glow_out.is_empty() && trail_out.is_empty());
    }

    for moving_kill in [
        InputEvent::Key {
            key: Key::Character('u'),
            mods: Modifiers::CTRL,
            base_layout: None,
            layout_text: None,
            event_type: KeyEventType::Press,
        },
        InputEvent::Key {
            key: Key::Character('w'),
            mods: Modifiers::CTRL,
            base_layout: None,
            layout_text: None,
            event_type: KeyEventType::Press,
        },
        InputEvent::Key {
            key: Key::Named(NamedKey::Backspace),
            mods: Modifiers::ALT,
            base_layout: None,
            layout_text: None,
            event_type: KeyEventType::Press,
        },
    ] {
        let class = classify_press(&moving_kill);
        assert!(class.kill_key && class.kill_moves);
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.config.cursor_trail = Some(true);
        app.config.cursor_trail_style = Some("comet".to_string());
        let cfg = app.trail_config();
        let mut out = Vec::new();
        app.windows
            .get_mut(&wid)
            .unwrap()
            .fx
            .cursor_mut()
            .trail
            .tick(Some((0, 20)), Instant::now(), &cfg, &mut out);
        let _ = app.input(wid, moving_kill, Source::Human);
        let fp = app
            .windows
            .get_mut(&wid)
            .unwrap()
            .fx
            .cursor_mut()
            .trail
            .tick(Some((0, 2)), Instant::now(), &cfg, &mut out);
        assert_eq!(fp, 0, "moving span kills are navigation-class");
        assert!(out.is_empty());
    }
}

#[test]
fn failed_inline_key_write_revokes_every_new_movement_licence() {
    use std::time::Duration;

    use crate::cursor_glow::Geom;
    use crate::cursor_trail::TrailConfig;

    let events = [
        InputEvent::Key {
            key: Key::Character('a'),
            mods: Modifiers::empty(),
            base_layout: None,
            layout_text: None,
            event_type: KeyEventType::Press,
        },
        InputEvent::Key {
            key: Key::Named(NamedKey::Enter),
            mods: Modifiers::empty(),
            base_layout: None,
            layout_text: None,
            event_type: KeyEventType::Press,
        },
        InputEvent::Key {
            key: Key::Named(NamedKey::Tab),
            mods: Modifiers::empty(),
            base_layout: None,
            layout_text: None,
            event_type: KeyEventType::Press,
        },
    ];
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
    let trail_cfg = TrailConfig {
        enabled: true,
        duration: Duration::from_millis(300),
        max_len: 24,
        color: 0x0050_FA7B,
        intensity: 0.0,
        warmth: 0.0,
    };

    for event in events {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.config.cursor_trail = Some(true);
        app.config.cursor_trail_style = Some("rainbow kitty".to_string());
        let glow_cfg = app.glow_config();
        let now = Instant::now();
        let mut glow_out = Vec::new();
        let mut trail_out = Vec::new();
        {
            let ws = app.windows.get_mut(&wid).unwrap();
            ws.fx
                .cursor_mut()
                .glow
                .tick(Some((0, 0)), now, &glow_cfg, geom, &mut glow_out);
            ws.fx
                .cursor_mut()
                .trail
                .tick(Some((0, 0)), now, &trail_cfg, &mut trail_out);
        }
        assert_eq!(
            app.input(wid, event, Source::Human),
            crate::input::InputOutcome::WriteFailed,
            "the synthetic headless sink proves no bytes landed"
        );
        let ws = app.windows.get_mut(&wid).unwrap();
        let moved = now + Duration::from_millis(4);
        assert_eq!(
            ws.fx
                .cursor_mut()
                .glow
                .tick(Some((0, 1)), moved, &glow_cfg, geom, &mut glow_out),
            0,
            "failed input cannot fund a later glow move"
        );
        assert_eq!(
            ws.fx
                .cursor_mut()
                .trail
                .tick(Some((0, 1)), moved, &trail_cfg, &mut trail_out),
            0,
            "failed input cannot fund a later classic comet move"
        );
        assert!(glow_out.is_empty() && trail_out.is_empty());
    }
    // A key that never reached the PTY clears the movement licence
    // (`SwallowedKeyClearsLicense`, proved by aterm-spec's registry sweep).
}

/// A key QUEUED to the session's ordered writer (every key on Windows, where that
/// writer is the ConPTY writer; on unix a key behind a draining paste) whose write
/// then FAILS. Its enqueue kept the press credit, since a queued key's bytes will
/// reach the wire; these never will, so the writer publishes a FAILED receipt and
/// the render prelude takes the credit back (`revoke_failed_delivery`), as a failed
/// inline write's dispatch does at once
/// ([`failed_inline_key_write_revokes_every_new_movement_licence`]).
///
/// Driven on the ConPTY route on this host (`force_conpty_route_for_test`), against
/// the headless stub's invalid sink. CONTROL: with the prelude never run, the typed
/// glyph's credit, which the enqueue kept, funds the program's own later `+1` as the
/// key's echo, which is what every such key did before this edge existed. Enter and
/// Tab bank no credit a one-cell move could spend; for them the test pins that the
/// failed receipt is read and nothing funds the move.
#[test]
fn failed_queued_key_write_revokes_its_press_credit() {
    use std::time::Duration;

    use crate::app_render::CursorFxInputs;
    use crate::cursor_glow::Geom;
    use crate::cursor_trail::TrailConfig;

    let events = [
        InputEvent::Key {
            key: Key::Character('a'),
            mods: Modifiers::empty(),
            base_layout: None,
            layout_text: None,
            event_type: KeyEventType::Press,
        },
        InputEvent::Key {
            key: Key::Named(NamedKey::Enter),
            mods: Modifiers::empty(),
            base_layout: None,
            layout_text: None,
            event_type: KeyEventType::Press,
        },
        InputEvent::Key {
            key: Key::Named(NamedKey::Tab),
            mods: Modifiers::empty(),
            base_layout: None,
            layout_text: None,
            event_type: KeyEventType::Press,
        },
    ];
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
    let trail_cfg = TrailConfig {
        enabled: true,
        duration: Duration::from_millis(300),
        max_len: 24,
        color: 0x0050_FA7B,
        intensity: 0.0,
        warmth: 0.0,
    };

    for (event, banks_credit) in events.into_iter().zip([true, false, false]) {
        for prelude in [true, false] {
            let mut app = App::headless_for_test();
            let wid = WindowId(0);
            app.config.cursor_trail = Some(true);
            app.config.cursor_trail_style = Some("rainbow kitty".to_string());
            let glow_cfg = app.glow_config();
            let session = app.front_terminal(wid).unwrap().session;
            // The seed frame: the caret at (0,0), and the window's delivery
            // baseline, without which the prelude applies nothing.
            app.cursor_frame(wid, CursorFxInputs::sample_for_test(Instant::now()))
                .expect("seed");
            let _route = super::paste_order::force_conpty_route_for_test();
            assert_eq!(
                app.input(wid, event.clone(), Source::Human),
                crate::input::InputOutcome::Ok,
                "FIFO admission: handed to the session's ordered writer ({event:?})"
            );
            // The key's dispatch is no later than this; every instant below is
            // placed after it, however long the writer thread takes.
            let dispatched = Instant::now();
            let sink = app.pool.get(session).unwrap().ctx.sink.clone();
            super::paste_order::await_drain_for_test(&sink);
            assert!(
                !super::paste_order::is_ordering(&sink),
                "the writer finished the job"
            );
            {
                let ctx = &app.pool.get(session).unwrap().ctx;
                let batch = ctx
                    .output_echo
                    .deliveries_after(&ctx.sink, Some(0), dispatched);
                assert!(
                    batch.items.iter().all(Option::is_none),
                    "a queued write that failed publishes no delivery ({event:?})"
                );
                assert_eq!(
                    batch
                        .failed
                        .iter()
                        .flatten()
                        .map(|t| t.window)
                        .collect::<Vec<_>>(),
                    [Some(wid)],
                    "…but one failed receipt, for the window that dispatched it ({event:?})"
                );
            }
            if prelude {
                let mut frame =
                    CursorFxInputs::sample_for_test(dispatched + Duration::from_millis(2));
                frame.cur = Some((0, 0));
                app.cursor_frame(wid, frame).expect("the prelude");
                let latest = {
                    let ctx = &app.pool.get(session).unwrap().ctx;
                    ctx.output_echo
                        .deliveries_after(&ctx.sink, None, dispatched)
                        .latest
                };
                assert_eq!(
                    app.windows[&wid].delivery_seen,
                    Some((session, latest)),
                    "the prelude read the failed receipt"
                );
            }
            let ws = app.windows.get_mut(&wid).unwrap();
            let moved = dispatched + Duration::from_millis(4);
            let mut glow_out = Vec::new();
            let mut trail_out = Vec::new();
            let glow =
                ws.fx
                    .cursor_mut()
                    .glow
                    .tick(Some((0, 1)), moved, &glow_cfg, geom, &mut glow_out);
            if prelude {
                assert_eq!(
                    glow, 0,
                    "a failed queued write cannot fund a later glow move ({event:?})"
                );
                assert_eq!(
                    ws.fx
                        .cursor_mut()
                        .trail
                        .tick(Some((0, 1)), moved, &trail_cfg, &mut trail_out),
                    0,
                    "nor a later classic comet move ({event:?})"
                );
                assert!(glow_out.is_empty() && trail_out.is_empty());
            } else if banks_credit {
                assert_ne!(
                    glow, 0,
                    "CONTROL: unrevoked, the credit the enqueue kept funds the move ({event:?})"
                );
            }
        }
    }
}
