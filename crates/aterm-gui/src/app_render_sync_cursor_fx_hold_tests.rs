// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::trail_audio::TrailAudio;
use std::time::Duration;

#[test]
fn synchronized_hold_preserves_license_and_anchors_until_glass_release() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("rainbow kitty".into());
    app.config.motion = Some("full".into());
    app.config.trail_sounds = Some(false);
    app.windows.get_mut(&wid).expect("window").focused = true;

    let t0 = Instant::now();
    let origin = (2, 3);
    let target = (2, 4);
    let mut seed = CursorFxInputs::sample_for_test(t0);
    seed.cur = Some(origin);
    assert!(
        app.cursor_frame_for_present(wid, false, seed).is_some(),
        "the baseline is a real presented tick"
    );

    let key_at = t0 + Duration::from_millis(1);
    let (spawns, tally, anchors) = {
        let ws = app.windows.get_mut(&wid).expect("window");
        ws.fx.cursor_mut().glow.note_typed_cells(key_at, 1);
        ws.fx.cursor_mut().trail.note_typed(key_at);
        (
            ws.fx.cursor_mut().glow.spawns(),
            ws.fx.cursor_mut().glow.admission_tally(),
            (
                ws.fx.cursor_mut().glow.cursor_anchor(),
                ws.fx.cursor_mut().trail.cursor_anchor(),
            ),
        )
    };

    for delay in [
        Duration::from_millis(8),
        SYNC_HOLD_CAP / 2,
        SYNC_HOLD_CAP.saturating_sub(Duration::from_millis(1)),
    ] {
        let mut held = CursorFxInputs::sample_for_test(key_at + delay);
        held.cur = Some(target);
        assert!(
            app.cursor_frame_for_present(wid, true, held).is_none(),
            "a held snapshot is not presentable"
        );
        let ws = app.windows.get(&wid).expect("window");
        assert_eq!(ws.fx.cursor().glow.spawns(), spawns);
        assert_eq!(ws.fx.cursor().glow.admission_tally(), tally);
        assert_eq!(
            (
                ws.fx.cursor().glow.cursor_anchor(),
                ws.fx.cursor().trail.cursor_anchor(),
            ),
            anchors,
            "off-glass frames advance neither engine anchor"
        );
    }

    let mut release = CursorFxInputs::sample_for_test(key_at + SYNC_HOLD_CAP);
    release.cur = Some(target);
    assert!(
        app.cursor_frame_for_present(wid, false, release).is_some(),
        "the release presents the coherent endpoint once"
    );
    let ws = app.windows.get(&wid).expect("window");
    assert_eq!(ws.fx.cursor().glow.spawns(), spawns + 1);
    assert_eq!(
        ws.fx.cursor().glow.admission_tally().licensed,
        tally.licensed + 1
    );
    assert_eq!(ws.fx.cursor().glow.cursor_anchor(), Some(target));
    assert_eq!(ws.fx.cursor().trail.cursor_anchor(), Some(target));
    assert!(ws.fx.cursor().glow.ribbon_segments() > 0);
    assert!(
        !ws.fx.cursor().trail.is_active(),
        "RainbowKitty owns the glow engine; the classic comet remains disabled"
    );
}

/// A torn present's hidden cursor reaches no motion engine: with a typed key
/// in flight, one torn present parks the hidden cursor eight rows up and
/// neither engine's anchor moves. The key's licence survives until the
/// shown caret returns and lays its cell. Negative control: the same hidden
/// cursor on a frame whose bracket has closed is taken as the anchor.
#[test]
fn a_torn_presents_hidden_cursor_moves_no_engine_and_spends_no_key() {
    let run = |hidden_withheld: bool| {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.config.cursor_trail = Some(true);
        app.config.cursor_trail_style = Some("rainbow kitty".into());
        app.config.motion = Some("full".into());
        app.config.trail_sounds = Some(false);
        app.windows.get_mut(&wid).expect("window").focused = true;

        let t0 = Instant::now();
        let origin = (10, 3);
        let write_cursor = (2, 40);
        let mut seed = CursorFxInputs::sample_for_test(t0);
        seed.cur = Some(origin);
        assert!(app.cursor_frame_for_present(wid, false, seed).is_some());

        let key_at = t0 + Duration::from_millis(16);
        {
            let ws = app.windows.get_mut(&wid).expect("window");
            ws.fx.cursor_mut().glow.note_typed_cells(key_at, 1);
            ws.fx.cursor_mut().trail.note_typed(key_at);
        }
        let before = {
            let ws = &app.windows[&wid];
            (
                ws.fx.cursor().glow.spawns(),
                ws.fx.cursor().glow.admission_tally(),
            )
        };

        let mut torn = CursorFxInputs::sample_for_test(key_at + Duration::from_millis(16));
        torn.cur = Some(write_cursor);
        torn.cursor_visible = false;
        torn.hidden_withheld = hidden_withheld;
        assert!(
            app.cursor_frame_for_present(wid, false, torn).is_some(),
            "past the hold the frame presents"
        );
        let ws = &app.windows[&wid];
        let anchors = (
            ws.fx.cursor().glow.cursor_anchor(),
            ws.fx.cursor().trail.cursor_anchor(),
        );
        let after = (
            ws.fx.cursor().glow.spawns(),
            ws.fx.cursor().glow.admission_tally(),
        );
        (
            app,
            wid,
            key_at,
            origin,
            write_cursor,
            before,
            after,
            anchors,
        )
    };

    // The torn present.
    let (mut app, wid, key_at, origin, write_cursor, before, after, anchors) = run(true);
    assert_eq!(
        anchors.0,
        Some(origin),
        "the glow keeps the last caret it saw as its anchor"
    );
    assert_ne!(
        anchors.1,
        Some(write_cursor),
        "the classic trail does not take the write cursor either"
    );
    assert_eq!(after, before, "nothing judged, nothing spent");
    let mut shown = CursorFxInputs::sample_for_test(key_at + Duration::from_millis(48));
    shown.cur = Some((origin.0, origin.1 + 1));
    assert!(app.cursor_frame_for_present(wid, false, shown).is_some());
    let ws = &app.windows[&wid];
    assert_eq!(
        ws.fx.cursor().glow.spawns(),
        before.0 + 1,
        "the key's licence survives to lay its own cell"
    );
    assert_eq!(
        ws.fx.cursor().glow.admission_tally().licensed,
        before.1.licensed + 1
    );
    assert_eq!(
        ws.fx.cursor().glow.cursor_anchor(),
        Some((origin.0, origin.1 + 1))
    );
    assert!(ws.fx.cursor().glow.ribbon_segments() > 0);

    // Negative control: the bracket closed, the hidden cursor is handed over.
    let (_app, _wid, _key_at, _origin, write_cursor_b, _before, _after, anchors) = run(false);
    assert_eq!(write_cursor_b, write_cursor);
    assert_eq!(
        anchors,
        (Some(write_cursor), Some(write_cursor)),
        "a finished frame's hidden caret still reaches the engines"
    );
}

#[test]
fn sync_opening_between_preflight_and_exact_capture_advances_nothing_off_glass() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("rainbow kitty".into());
    app.config.motion = Some("full".into());
    app.config.trail_sounds = Some(true);
    app.trail_audio = TrailAudio::capturing_for_test();
    app.windows.get_mut(&wid).expect("window").focused = true;

    let t0 = Instant::now();
    let origin = (2, 3);
    let target = (2, 4);
    let mut seed = CursorFxInputs::sample_for_test(t0);
    seed.cur = Some(origin);
    assert!(
        app.cursor_frame_for_present(wid, false, seed).is_some(),
        "fixture first establishes the presented engine/sound clock"
    );
    let _ = app.trail_audio.take_captured_for_test();

    let candidate_at = t0 + Duration::from_millis(16);
    {
        let window = app.windows.get_mut(&wid).expect("window");
        window
            .fx
            .cursor_mut()
            .glow
            .note_typed_cells(candidate_at, 1);
        window.fx.cursor_mut().trail.note_typed(candidate_at);
    }
    let terminal = app.pool.get(0).expect("front session").term.clone();
    let preflight = {
        let terminal = term_lock(&terminal);
        SyncObservation {
            terminal_id: terminal.render_identity(),
            active: terminal.modes().synchronized_output(),
            end_seq: terminal.sync_end_seq(),
            open_dirty: terminal.sync_open_dirty(),
        }
    };
    assert!(
        !preflight.active,
        "the preflight deliberately samples sync false"
    );
    assert!(!update_sync_frame_hold(
        app.windows.get_mut(&wid).expect("window"),
        preflight,
        candidate_at,
        SYNC_HOLD_CAP,
    ));

    let before = {
        let window = &app.windows[&wid];
        (
            window.fx.cursor().glow.spawns(),
            window.fx.cursor().glow.admission_tally(),
            window.fx.cursor().glow.cursor_anchor(),
            window.fx.cursor().trail.cursor_anchor(),
            window
                .fx
                .companions()
                .cursor_cat
                .momentum(candidate_at)
                .to_bits(),
        )
    };

    // A synchronized episode opens after the preflight but before the exact
    // snapshot authorizes effects.
    {
        let mut terminal = term_lock(&terminal);
        terminal.process(b"\x1b[?2026hpartial");
    }
    let current = {
        let terminal = term_lock(&terminal);
        SyncObservation {
            terminal_id: terminal.render_identity(),
            active: terminal.modes().synchronized_output(),
            end_seq: terminal.sync_end_seq(),
            open_dirty: terminal.sync_open_dirty(),
        }
    };
    assert!(current.active && current.open_dirty);
    let late_hold = recheck_sync_frame_hold(
        app.windows.get_mut(&wid).expect("window"),
        preflight,
        current,
        candidate_at,
        SYNC_HOLD_CAP,
    );
    assert!(
        late_hold,
        "the exact capture must revoke the candidate before effects run"
    );
    let mut candidate = CursorFxInputs::sample_for_test(candidate_at);
    candidate.cur = Some(target);
    let tick = if late_hold {
        None
    } else {
        app.cursor_frame_for_present(wid, false, candidate)
    };
    assert!(tick.is_none());

    let after = {
        let window = &app.windows[&wid];
        (
            window.fx.cursor().glow.spawns(),
            window.fx.cursor().glow.admission_tally(),
            window.fx.cursor().glow.cursor_anchor(),
            window.fx.cursor().trail.cursor_anchor(),
            window
                .fx
                .companions()
                .cursor_cat
                .momentum(candidate_at)
                .to_bits(),
        )
    };
    assert_eq!(
        after, before,
        "licenses, anchors, and cat momentum stay frozen"
    );
    assert!(
        app.trail_audio.take_captured_for_test().is_empty(),
        "the rejected off-glass candidate cannot reach the sound host"
    );

    // Non-vacuity: once the episode closes the same prepared move advances
    // and speaks.
    {
        let mut terminal = term_lock(&terminal);
        terminal.process(b"\x1b[?2026l");
    }
    let released = {
        let terminal = term_lock(&terminal);
        SyncObservation {
            terminal_id: terminal.render_identity(),
            active: terminal.modes().synchronized_output(),
            end_seq: terminal.sync_end_seq(),
            open_dirty: terminal.sync_open_dirty(),
        }
    };
    assert!(!recheck_sync_frame_hold(
        app.windows.get_mut(&wid).expect("window"),
        current,
        released,
        candidate_at,
        SYNC_HOLD_CAP,
    ));
    let mut release = CursorFxInputs::sample_for_test(candidate_at);
    release.cur = Some(target);
    assert!(app.cursor_frame_for_present(wid, false, release).is_some());
    let window = &app.windows[&wid];
    assert!(window.fx.cursor().glow.spawns() > before.0);
    assert_ne!(
        window
            .fx
            .companions()
            .cursor_cat
            .momentum(candidate_at)
            .to_bits(),
        before.4,
        "the admitted move is capable of advancing the cat clock"
    );
    assert!(
        !app.trail_audio.take_captured_for_test().is_empty(),
        "the admitted move is capable of producing trail sound"
    );
}
