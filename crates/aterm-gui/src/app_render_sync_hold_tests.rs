// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{recheck_sync_frame_hold, sync_frame_hold, update_sync_frame_holds};
use crate::{App, SyncObservation, WindowId};
use aterm_core::terminal::Terminal;
use std::time::{Duration, Instant};

/// A terminal whose own mode-2026 force-clear cannot fire mid-test: that
/// clear runs on real time (`sync_timeout_ms`, 1 s), while these tests drive
/// the hold on a synthetic clock and a loaded machine can stall past it.
fn terminal_without_force_clear() -> Terminal {
    let mut term = Terminal::new(24, 80);
    term.apply_config(&aterm_core::config::TerminalConfig {
        sync_timeout_ms: 3_600_000,
        ..Default::default()
    });
    term
}

fn observation(active: bool, end_seq: u64, open_dirty: bool) -> SyncObservation {
    pane_observation(11, active, end_seq, open_dirty)
}

fn pane_observation(
    terminal_id: u64,
    active: bool,
    end_seq: u64,
    open_dirty: bool,
) -> SyncObservation {
    SyncObservation {
        terminal_id,
        active,
        end_seq,
        open_dirty,
    }
}

fn terminal_observation(term: &Terminal) -> SyncObservation {
    observation(
        term.modes().synchronized_output(),
        term.sync_end_seq(),
        term.sync_open_dirty(),
    )
}

/// SYNC-1 frame-hold state machine: arm on the rising edge, hold while unexpired,
/// release on `2026l` or timeout, no re-hold within an episode, re-arm on a fresh
/// episode. Covers the full lifecycle the present path drives.
#[test]
fn sync_frame_hold_state_machine() {
    let t0 = Instant::now();
    let timeout = Duration::from_millis(1000);
    let d = |ms| t0 + Duration::from_millis(ms);

    // Inactive throughout: never holds, no deadline armed.
    assert_eq!(
        sync_frame_hold(observation(false, 0, false), false, None, 0, t0, timeout),
        (None, false, 0)
    );

    // Rising edge (false->true): arm at now + timeout and hold, recording the
    // close counter at arm time.
    assert_eq!(
        sync_frame_hold(observation(true, 7, false), false, None, 0, d(0), timeout),
        (Some(d(1000)), true, 7),
        "rising edge arms at now+timeout, holds, records end_seq"
    );

    // Sustained hold (active, not rising, unexpired, counter unmoved): keep the
    // Same deadline, hold.
    assert_eq!(
        sync_frame_hold(
            observation(true, 7, false),
            true,
            Some(d(1000)),
            7,
            d(500),
            timeout,
        ),
        (Some(d(1000)), true, 7),
        "the deadline is not re-armed while holding"
    );

    // `2026l` mid-window (sync inactive before the deadline): release immediately.
    assert_eq!(
        sync_frame_hold(
            observation(false, 8, false),
            true,
            Some(d(1000)),
            7,
            d(500),
            timeout,
        ),
        (None, false, 8),
        "sync end releases the held frame at once"
    );

    // Timeout expiry (active, not rising, now >= deadline): release + disarm so
    // about_to_wait folds no stale past instant (which would busy-spin).
    assert_eq!(
        sync_frame_hold(
            observation(true, 7, false),
            true,
            Some(d(1000)),
            7,
            d(1000),
            timeout,
        ),
        (None, false, 7),
        "an expired hold releases and clears the deadline"
    );

    // After a timeout release, sync still active but not rising and no deadline: it
    // must not re-hold (a stuck-sync app presents normally, not at 1 Hz forever).
    assert_eq!(
        sync_frame_hold(observation(true, 7, false), true, None, 7, d(1500), timeout),
        (None, false, 7),
        "no re-hold within an episode after a timeout"
    );

    // A new episode (sync dropped then set again → rising edge) re-arms.
    assert_eq!(
        sync_frame_hold(
            observation(true, 9, false),
            false,
            None,
            7,
            d(2000),
            timeout
        ),
        (Some(d(3000)), true, 9),
        "a fresh 2026h re-arms the hold"
    );
}

/// An app bracketing every repaint in `2026h…2026l` under sustained flood
/// keeps the sampled level true on every redraw. The close counter turns
/// each closed bracket into a present plus a fresh arm, so glass keeps
/// updating bracket by bracket.
#[test]
fn closed_bracket_presents_even_while_level_stays_true() {
    let t0 = Instant::now();
    let timeout = Duration::from_millis(150);
    let d = |ms| t0 + Duration::from_millis(ms);

    // Frame 0: rising edge — arm and hold (bracket #1 open, counter 0).
    let (armed, hold, seq) =
        sync_frame_hold(observation(true, 0, false), false, None, 0, d(0), timeout);
    assert!(hold && armed == Some(d(150)) && seq == 0);

    // Frame 1 (16 ms later): bracket #1 CLOSED and bracket #2 opened inside one
    // reader batch — the level still samples true, but the counter advanced.
    // The completed frame must PRESENT, with a fresh deadline armed for #2.
    let (armed, hold, seq) = sync_frame_hold(
        observation(true, 1, false),
        true,
        armed,
        seq,
        d(16),
        timeout,
    );
    assert!(
        !hold,
        "a closed bracket presents even while the level reads true"
    );
    assert_eq!(
        armed,
        Some(d(166)),
        "a fresh episode is armed for bracket #2"
    );
    assert_eq!(seq, 1);

    // Frame 2: bracket #2 still open (counter unmoved) — hold under its deadline.
    let (armed2, hold, seq) = sync_frame_hold(
        observation(true, 1, false),
        true,
        armed,
        seq,
        d(32),
        timeout,
    );
    assert!(hold && armed2 == armed && seq == 1);

    // Frame 3: bracket #2 closed, #3 open — present again. Steady state: one
    // present per closed bracket, never a 1 Hz pin.
    let (_, hold, seq) = sync_frame_hold(
        observation(true, 2, false),
        true,
        armed2,
        seq,
        d(48),
        timeout,
    );
    assert!(!hold && seq == 2);
}

/// Tier-1 conformance for `sync_reopen_visibility_model`: drive the real
/// parser-owned episode bit and the shipping GUI hold decision through the
/// close/reopen race, then project each sampled state onto the derived
/// model. The final arm runs the historical close-sequence-only mutant
/// against the same real terminal observation as a non-vacuity control.
#[test]
fn sync_reopen_visibility_model_conforms_to_terminal_and_gui_hold() {
    let model = aterm_spec::derive::sync_reopen_visibility_model();
    let timeout = Duration::from_millis(150);
    let t0 = Instant::now();
    let d = |ms| t0 + Duration::from_millis(ms);
    let mut term = terminal_without_force_clear();
    let mut presented_generation = 0u64;

    // The model begins inside a dirty first episode. Both pieces of this
    // observation come from shipping code: `sync_open_dirty` is set by the
    // real parser action, and `hold` is the real glass decision.
    term.process(b"\x1b[?2026hold-frame");
    let initial = model.init_state();
    let (mut armed, hold, mut armed_end_seq) =
        sync_frame_hold(terminal_observation(&term), false, None, 0, d(0), timeout);
    assert_eq!(
        u64::from(term.modes().synchronized_output()),
        initial["sync_active"] as u64
    );
    assert_eq!(
        u64::from(term.sync_open_dirty()),
        initial["open_dirty"] as u64
    );
    assert_eq!(u64::from(hold), initial["hold"] as u64);
    assert_eq!(term.sync_end_seq(), initial["completed_generation"] as u64);
    assert_eq!(presented_generation, initial["presented_generation"] as u64);
    assert_eq!(
        u64::from(!hold && term.sync_open_dirty()),
        initial["partial_visible"] as u64
    );

    // Sample the close+immediate-reopen exactly as a PTY reader batch can:
    // the level is still true, but the close counter advanced and the new
    // episode is clean. That licenses the completed boundary once.
    term.process(b"\x1b[?2026l\x1b[?2026h");
    let first_closed = model.successors("CloseFirstEpisode", &initial)[0].clone();
    let clean_reopen = model.successors("ReopenClean", &first_closed)[0].clone();
    let (next_armed, clean_hold, next_armed_end_seq) = sync_frame_hold(
        terminal_observation(&term),
        true,
        armed,
        armed_end_seq,
        d(16),
        timeout,
    );
    armed = next_armed;
    armed_end_seq = next_armed_end_seq;
    if !clean_hold {
        presented_generation = term.sync_end_seq();
    }
    assert_eq!(
        u64::from(term.modes().synchronized_output()),
        clean_reopen["sync_active"] as u64
    );
    assert_eq!(
        u64::from(term.sync_open_dirty()),
        clean_reopen["open_dirty"] as u64
    );
    assert_eq!(u64::from(clean_hold), clean_reopen["hold"] as u64);
    assert_eq!(
        term.sync_end_seq(),
        clean_reopen["completed_generation"] as u64
    );
    assert_eq!(
        presented_generation,
        clean_reopen["presented_generation"] as u64
    );
    assert_eq!(
        u64::from(!clean_hold && term.sync_open_dirty()),
        clean_reopen["partial_visible"] as u64
    );
    // Negative control: a level-sampled hold (the mode bit itself) on this
    // terminal reads the close+reopen as one endless episode and presents
    // nothing. Projected field by field, it is the model's `Buggy=1`
    // ReopenClean, which the healthy model refuses.
    let level_hold = term.modes().synchronized_output();
    assert!(level_hold && !clean_hold);
    let mut level_held = clean_reopen.clone();
    level_held.insert("hold", i64::from(level_hold));
    // A held frame leaves glass at the generation presented before it.
    level_held.insert("presented_generation", first_closed["presented_generation"]);
    level_held.insert(
        "partial_visible",
        i64::from(!level_hold && term.sync_open_dirty()),
    );
    // The level hold is the `Buggy=1` trace that picked it (`level_fault`).
    let (mut level_closed, mut level_picked) = (first_closed.clone(), level_held.clone());
    level_closed.insert("level_fault", 1);
    level_picked.insert("level_fault", 1);
    assert_eq!(
        aterm_spec::interp::with_buggy(&model, 1).successors("ReopenClean", &level_closed),
        vec![level_picked]
    );
    assert!(!model.check_invariant("CleanReopenMayPresentCompletedBoundary", &level_held));
    let (level_ok, _) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &first_closed,
        &level_held,
        Some("ReopenClean"),
        "level-sampled sync hold negative control",
    );
    assert!(!level_ok, "the level-sampled hold must not conform");

    // A real printable action dirties the reopened episode. With no new
    // close, the GUI keeps the last completed generation on glass.
    term.process(b"next-frame-prefix");
    let dirty_reopen = model.successors("DirtyReopenedEpisode", &clean_reopen)[0].clone();
    let (next_armed, dirty_hold, next_armed_end_seq) = sync_frame_hold(
        terminal_observation(&term),
        true,
        armed,
        armed_end_seq,
        d(32),
        timeout,
    );
    armed = next_armed;
    armed_end_seq = next_armed_end_seq;
    assert_eq!(
        u64::from(term.modes().synchronized_output()),
        dirty_reopen["sync_active"] as u64
    );
    assert_eq!(
        u64::from(term.sync_open_dirty()),
        dirty_reopen["open_dirty"] as u64
    );
    assert_eq!(u64::from(dirty_hold), dirty_reopen["hold"] as u64);
    assert_eq!(
        term.sync_end_seq(),
        dirty_reopen["completed_generation"] as u64
    );
    assert_eq!(
        presented_generation,
        dirty_reopen["presented_generation"] as u64
    );
    assert_eq!(
        u64::from(!dirty_hold && term.sync_open_dirty()),
        dirty_reopen["partial_visible"] as u64
    );

    term.process(b"\x1b[?2026l");
    let reopened_closed = model.successors("CloseReopenedEpisode", &dirty_reopen)[0].clone();
    let (_, closed_hold, closed_armed_end_seq) = sync_frame_hold(
        terminal_observation(&term),
        true,
        armed,
        armed_end_seq,
        d(48),
        timeout,
    );
    assert!(!closed_hold);
    presented_generation = closed_armed_end_seq;
    assert_eq!(
        u64::from(term.modes().synchronized_output()),
        reopened_closed["sync_active"] as u64
    );
    assert_eq!(
        u64::from(term.sync_open_dirty()),
        reopened_closed["open_dirty"] as u64
    );
    assert_eq!(u64::from(closed_hold), reopened_closed["hold"] as u64);
    assert_eq!(
        term.sync_end_seq(),
        reopened_closed["completed_generation"] as u64
    );
    assert_eq!(
        presented_generation,
        reopened_closed["presented_generation"] as u64
    );

    // Negative control: race close+reopen+write before the next sample. The
    // old close-sequence-only rule released because the counter advanced;
    // the shipping decision also reads the real dirty bit and holds.
    let mut raced = terminal_without_force_clear();
    raced.process(b"\x1b[?2026hold-frame");
    let (raced_armed, raced_initial_hold, raced_armed_end_seq) =
        sync_frame_hold(terminal_observation(&raced), false, None, 0, d(64), timeout);
    assert!(raced_initial_hold);
    raced.process(b"\x1b[?2026l\x1b[?2026hpartial-next-frame");
    let (raced_deadline, correct_hold, raced_seq) = sync_frame_hold(
        terminal_observation(&raced),
        true,
        raced_armed,
        raced_armed_end_seq,
        d(80),
        timeout,
    );
    // The dirty hold arms a FRESH deadline — the freeze valve: a held pane
    // with no deadline never gets its about_to_wait wake — and carries the
    // episode's completed generation forward.
    assert_eq!(
        (raced_deadline, raced_seq),
        (Some(d(80) + timeout), raced.sync_end_seq()),
        "a dirty reopen holds against a fresh deadline"
    );
    let close_sequence_only_mutant_holds =
        !(raced.modes().synchronized_output() && raced.sync_end_seq() != raced_armed_end_seq);
    assert!(raced.sync_open_dirty());
    assert!(
        correct_hold,
        "the real dirty reopened episode must stay off glass"
    );
    assert_ne!(
        correct_hold, close_sequence_only_mutant_holds,
        "negative control: ignoring sync_open_dirty must expose the partial episode"
    );

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let buggy_closed = buggy.successors("CloseFirstEpisode", &buggy.init_state())[0].clone();
    let buggy_clean = buggy.successors("ReopenClean", &buggy_closed)[0].clone();
    let buggy_dirty = buggy.successors("DirtyReopenedEpisode", &buggy_clean)[0].clone();
    assert!(
        !buggy.check_invariant("DirtyReopenHoldsUntilClose", &buggy_dirty),
        "the derived negative control must reject the same stale presentation license"
    );
}

#[test]
fn composed_hold_releases_only_after_every_real_pane_releases() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let t0 = Instant::now();
    let timeout = Duration::from_millis(150);
    let model = aterm_spec::derive::composed_sync_hold_model();

    let held = update_sync_frame_holds(
        app.windows.get_mut(&wid).expect("test window"),
        &[
            pane_observation(11, true, 0, false),
            pane_observation(22, true, 0, false),
        ],
        t0,
        timeout,
    );
    assert!(held, "both rising edges hold the composite");
    let model_held = model.successors("ArmBoth", &model.init_state())[0].clone();

    let held = update_sync_frame_holds(
        app.windows.get_mut(&wid).expect("test window"),
        &[
            pane_observation(11, false, 1, false),
            pane_observation(22, true, 0, false),
        ],
        t0 + Duration::from_millis(1),
        timeout,
    );
    assert!(
        held,
        "pane B's incomplete bracket still owns the glass hold"
    );
    let model_a_closed = model.successors("CloseOnlyA", &model_held)[0].clone();
    assert!(model.check_invariant("NoPartialCompositePresent", &model_a_closed));

    // Negative aggregate control: "any close" would release here, exactly
    // the old scalar-counter bug. The shipping per-pane fold says held.
    let flawed_aggregate_holds = false;
    assert_ne!(held, flawed_aggregate_holds);

    let held = update_sync_frame_holds(
        app.windows.get_mut(&wid).expect("test window"),
        &[
            pane_observation(11, false, 1, false),
            pane_observation(22, false, 1, false),
        ],
        t0 + Duration::from_millis(2),
        timeout,
    );
    assert!(!held, "the composite releases once every pane is complete");
    let model_released = model.successors("CloseRemainingB", &model_a_closed)[0].clone();
    assert_eq!(model_released["presented"], 1);
}

#[test]
fn unchanged_under_lock_sample_does_not_rehold_a_close_reopen_present() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let t0 = Instant::now();
    let timeout = Duration::from_millis(150);
    let window = app.windows.get_mut(&wid).expect("test window");

    assert!(update_sync_frame_holds(
        window,
        &[observation(true, 0, false)],
        t0,
        timeout,
    ));
    let preflight = observation(true, 1, false);
    assert!(
        !update_sync_frame_holds(
            window,
            &[preflight],
            t0 + Duration::from_millis(16),
            timeout,
        ),
        "the completed bracket gets one present while the next is armed"
    );
    assert!(
        !recheck_sync_frame_hold(
            window,
            preflight,
            preflight,
            t0 + Duration::from_millis(16),
            timeout,
        ),
        "the identical extraction-lock sample must preserve that present license"
    );
    let state = window
        .sync_holds
        .iter()
        .find(|state| state.terminal_id == 11)
        .expect("pane hold state");
    assert_eq!(state.armed_end_seq, 1);
    assert_eq!(
        state.hold_until,
        Some(t0 + Duration::from_millis(166)),
        "the next episode remains armed without being consumed"
    );
    assert!(update_sync_frame_holds(
        window,
        &[preflight],
        t0 + Duration::from_millis(32),
        timeout,
    ));
}

#[test]
fn extraction_recheck_revokes_clean_reopen_when_new_episode_became_dirty() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let t0 = Instant::now();
    let timeout = Duration::from_millis(150);
    let window = app.windows.get_mut(&wid).expect("test window");

    assert!(update_sync_frame_holds(
        window,
        &[observation(true, 0, false)],
        t0,
        timeout,
    ));
    let preflight = observation(true, 1, false);
    assert!(
        !update_sync_frame_holds(
            window,
            &[preflight],
            t0 + Duration::from_millis(16),
            timeout,
        ),
        "the clean close boundary receives its one present license"
    );
    assert!(
        recheck_sync_frame_hold(
            window,
            preflight,
            observation(true, 1, true),
            t0 + Duration::from_millis(16),
            timeout,
        ),
        "a write before extraction must revoke the stale clean license"
    );
}

#[test]
fn duplicate_views_transition_one_terminal_hold_only_once() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let t0 = Instant::now();
    let timeout = Duration::from_millis(150);
    let window = app.windows.get_mut(&wid).expect("test window");

    assert!(update_sync_frame_holds(
        window,
        &[observation(true, 0, false), observation(true, 0, false)],
        t0,
        timeout,
    ));
    assert!(
        !update_sync_frame_holds(
            window,
            &[observation(true, 1, false), observation(true, 1, false)],
            t0 + Duration::from_millis(16),
            timeout,
        ),
        "two views of one terminal cannot consume its one close+rearm license twice"
    );
    assert_eq!(window.sync_holds.len(), 1);
}

#[test]
fn one_pane_recheck_cannot_clear_a_sibling_hold_gauge() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let t0 = Instant::now();
    let timeout = Duration::from_millis(150);
    let window = app.windows.get_mut(&wid).expect("test window");
    let observations = [
        pane_observation(11, true, 0, false),
        pane_observation(22, true, 0, false),
    ];

    assert!(update_sync_frame_holds(window, &observations, t0, timeout,));
    assert!(window.sync_holding_gauge.is_held_for_test());
    assert!(
        !recheck_sync_frame_hold(
            window,
            observations[0],
            pane_observation(11, false, 1, false),
            t0 + Duration::from_millis(1),
            timeout,
        ),
        "the first pane itself released"
    );
    assert!(
        window.sync_holding_gauge.is_held_for_test(),
        "the second pane's active hold keeps the per-window gauge leased"
    );
}
