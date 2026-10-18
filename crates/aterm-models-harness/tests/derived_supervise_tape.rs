// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 of the harness tape's models (the replay plan's §9):
//! `SuperviseTapeRecovery` (model 1, §6.4), `SuperviseReplayCursor` (model 2,
//! the strict arm of §5.2) and `SupervisorRaisedLatch` (model 3) — each proved
//! at `Buggy = 0` and caught at `Buggy = 1`, each invariant broken by the
//! mutant written to break it, and walked the way a tape meets its ends, a
//! replay its tape and a raised flag its loop. Tier-1 is
//! `aterm-agent/src/supervise/tape/conformance_tests.rs`.

use aterm_models_harness::supervise_tape_recovery_model;
use aterm_spec::walk::{only, walk};
use aterm_spec::{interp, verify};

#[test]
fn supervise_tape_recovery_proves_and_catches() {
    let model = supervise_tape_recovery_model();
    let committed = interp::fired_actions(&model);
    for action in [
        "Write", "Tear", "Crash", "Rot", "RotLast", "RotMagic", "Recover",
    ] {
        assert!(
            committed.contains(action),
            "{action} fires at the committed configuration: {committed:?}"
        );
    }
    verify::prove_and_catch_scalar(
        &model,
        "harness tape: a whole-frame prefix, every whole frame back, damage refused, a tear said",
    );
}

/// Each wrong reader breaks the law it was written to break, alone.
#[test]
fn each_wrong_reader_breaks_its_own_law() {
    let model = supervise_tape_recovery_model();
    for (dial, law) in [
        (1, "RotRefused"),
        (2, "RecoveredIsPrefix"),
        (3, "NoSilentLoss"),
        (4, "TornSaid"),
        (5, "RotRefused"),
    ] {
        let wrong = interp::with_buggy(&only(&model, law), dial);
        assert!(
            interp::violated_invariants(&wrong).contains(&law),
            "Buggy = {dial} breaks {law}"
        );
        let right = interp::with_buggy(&only(&model, law), 0);
        assert!(
            interp::violated_invariants(&right).is_empty(),
            "the committed reader keeps {law}"
        );
    }
}

/// The ends a tape meets: a clean end comes back whole; a crash inside a frame
/// loses only that frame and says so; damage before a whole frame is refused;
/// damage to the last frame is a torn tail.
#[test]
fn supervise_tape_recovery_walks_the_ends_a_tape_meets() {
    let m = supervise_tape_recovery_model();
    let read = |s: &interp::State| (s["recovered"], s["refused"], s["said_torn"]);

    let clean = walk(&m, &["Write", "Write", "Write", "Crash", "Recover"]);
    assert_eq!(read(&clean), (3, 0, 0), "a clean end, whole: {clean:?}");

    let torn = walk(&m, &["Write", "Write", "Tear", "Recover"]);
    assert_eq!(
        read(&torn),
        (2, 0, 1),
        "a crash mid-frame keeps the whole frames: {torn:?}"
    );

    let header_torn = walk(&m, &["Tear", "Recover"]);
    assert_eq!(
        read(&header_torn),
        (0, 0, 1),
        "a crash inside the first write leaves nothing, said torn: {header_torn:?}"
    );

    let rotten = walk(&m, &["Write", "Write", "Write", "Crash", "Rot", "Recover"]);
    assert_eq!(
        read(&rotten),
        (0, 1, 0),
        "damage with a whole frame after it is refused: {rotten:?}"
    );

    let last = walk(
        &m,
        &["Write", "Write", "Write", "Crash", "RotLast", "Recover"],
    );
    assert_eq!(
        read(&last),
        (2, 0, 1),
        "a damaged last frame is the torn tail: {last:?}"
    );

    // The magic is read before any frame: damaged, the file is refused, even
    // when the header is the only whole frame and no frame follows it.
    for schedule in [
        ["Write", "Crash", "RotMagic", "Recover"].as_slice(),
        &["Write", "Write", "Write", "Crash", "RotMagic", "Recover"],
    ] {
        let magic = walk(&m, schedule);
        assert_eq!(
            read(&magic),
            (0, 1, 0),
            "a damaged magic is refused: {magic:?}"
        );
    }

    let one = walk(&m, &["Write", "Crash"]);
    assert!(
        !m.action_enabled("Rot", &one),
        "with one frame there is no frame after a damaged one"
    );
    let damaged = walk(&m, &["Write", "Write", "Crash", "Rot"]);
    assert!(
        !m.action_enabled("RotLast", &damaged),
        "one damage per tape: {damaged:?}"
    );
}

// -- model 2: a strict replay's cursor ---------------------------------------

/// Tier-0 of `SuperviseReplayCursor` (model 2, the strict arm): proved at
/// `Buggy = 0`, caught at `Buggy = 1`, every move firing at the committed
/// configuration.
#[test]
fn supervise_replay_cursor_proves_and_catches() {
    use aterm_models_harness::supervise_replay_cursor_model;
    let model = supervise_replay_cursor_model();
    let committed = interp::fired_actions(&model);
    for action in [
        "AskNext",
        "AskOtherRead",
        "AskOtherAct",
        "AskPastEnd",
        "AskAfterDivergence",
    ] {
        assert!(
            committed.contains(action),
            "{action} fires at the committed configuration: {committed:?}"
        );
    }
    verify::prove_and_catch_scalar(
        &model,
        "strict replay: nothing from the world, no act skipped, nothing after the divergence",
    );
}

/// Each wrong cursor breaks the law it was written to break, alone.
#[test]
fn each_wrong_cursor_breaks_its_own_law() {
    use aterm_models_harness::supervise_replay_cursor_model;
    let model = supervise_replay_cursor_model();
    for (dial, law) in [
        (1, "NoLiveEffect"),
        (2, "NoActSkipped"),
        (3, "NothingServedAfterDivergence"),
    ] {
        let wrong = interp::with_buggy(&only(&model, law), dial);
        assert!(
            interp::violated_invariants(&wrong).contains(&law),
            "Buggy = {dial} breaks {law}"
        );
        let right = interp::with_buggy(&only(&model, law), 0);
        assert!(
            interp::violated_invariants(&right).is_empty(),
            "the committed cursor keeps {law}"
        );
    }
}

/// The cursor the way a replay meets its tape: every record asked in order is
/// served to the end, and the end is a divergence, never a question to the
/// world; a read the tape does not hold diverges where it is asked, before the
/// act after it — and the lenient cursor resyncs across that act.
#[test]
fn supervise_replay_cursor_walks_a_replay() {
    use aterm_models_harness::supervise_replay_cursor_model;
    let m = supervise_replay_cursor_model();
    let read = |s: &interp::State| (s["pos"], s["diverged"], s["live"], s["skipped_act"]);
    let whole = walk(&m, &["AskNext", "AskNext", "AskNext", "AskNext"]);
    assert_eq!(
        read(&whole),
        (4, 0, 0, 0),
        "the tape served whole: {whole:?}"
    );
    let past = walk(
        &m,
        &["AskNext", "AskNext", "AskNext", "AskNext", "AskPastEnd"],
    );
    assert_eq!(read(&past), (4, 1, 0, 0), "past the end diverges: {past:?}");
    let other = walk(&m, &["AskOtherRead"]);
    assert_eq!(
        read(&other),
        (0, 1, 0, 0),
        "diverged where asked: {other:?}"
    );
    assert!(
        !m.action_enabled("AskNext", &other),
        "nothing served after it"
    );
    let lenient = interp::with_buggy(&m, 2);
    let resynced = walk(&lenient, &["AskOtherRead"]);
    assert_eq!(
        read(&resynced),
        (3, 0, 0, 1),
        "the lenient cursor passes the act: {resynced:?}"
    );
}

// -- model 3: the stop flag's latch ------------------------------------------

/// Tier-0 of `SupervisorRaisedLatch` (model 3): proved at `Buggy = 0`, caught
/// at `Buggy = 1`, every move firing at the committed configuration.
#[test]
fn supervisor_raised_latch_proves_and_catches() {
    use aterm_models_harness::supervisor_raised_latch_model;
    let model = supervisor_raised_latch_model();
    let committed = interp::fired_actions(&model);
    for action in ["Raise", "LoopOp", "EndRecord", "ReplayOp", "ReplayEnd"] {
        assert!(
            committed.contains(action),
            "{action} fires at the committed configuration: {committed:?}"
        );
    }
    verify::prove_and_catch_scalar(
        &model,
        "a raised flag is taped by the loop's own read, and its replay never diverges",
    );
}

/// The setter that tapes the flag itself breaks both laws, each alone.
#[test]
fn a_setter_that_tapes_the_flag_breaks_both_laws() {
    use aterm_models_harness::supervisor_raised_latch_model;
    let model = supervisor_raised_latch_model();
    for law in ["TapedByTheRead", "NoSpuriousDivergence"] {
        let wrong = interp::with_buggy(&only(&model, law), 1);
        assert!(
            interp::violated_invariants(&wrong).contains(&law),
            "Buggy = 1 breaks {law}"
        );
        let right = interp::with_buggy(&only(&model, law), 0);
        assert!(
            interp::violated_invariants(&right).is_empty(),
            "the committed latch keeps {law}"
        );
    }
}

/// The flag raised between a read and the door after it: the loop's next read
/// tapes it and the replay meets it there; the setter's own record sits in
/// front of the door, and the replay diverges at the door.
#[test]
fn supervisor_raised_latch_walks_a_raise_between_read_and_door() {
    use aterm_models_harness::supervisor_raised_latch_model;
    let schedule = [
        "LoopOp",
        "Raise",
        "LoopOp",
        "LoopOp",
        "LoopOp",
        "EndRecord",
        "ReplayOp",
        "ReplayOp",
        "ReplayOp",
        "ReplayOp",
        "ReplayEnd",
    ];
    let m = supervisor_raised_latch_model();
    let ok = walk(&m, &schedule);
    assert_eq!(
        (ok["marked"], ok["mark"], ok["diverged"]),
        (1, 2, 0),
        "taped by the read after the door, replayed: {ok:?}"
    );
    let setter = interp::with_buggy(&m, 1);
    let stops = walk(&setter, &schedule[..8]);
    assert_eq!(
        (stops["marked"], stops["mark"], stops["diverged"]),
        (1, 1, 1),
        "taped in front of the door, diverged there: {stops:?}"
    );
}
