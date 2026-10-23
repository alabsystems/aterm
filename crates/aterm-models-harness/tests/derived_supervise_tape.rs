// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 of the harness tape's models (the replay plan's §9).

use aterm_models_harness::supervise_tape_recovery_model;
use aterm_spec::walk::{only, walk};
use aterm_spec::{interp, verify};

#[test]
fn supervise_tape_recovery_fires_every_committed_move() {
    let model = supervise_tape_recovery_model();
    let committed = interp::fired_actions(&model);
    for action in [
        "Write", "Tear", "Crash", "Rot", "RotLast", "RotMagic", "Misplace", "Recover",
    ] {
        assert!(
            committed.contains(action),
            "{action} fires at the committed configuration: {committed:?}"
        );
    }
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
        (6, "MisplacedRefused"),
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
/// damage to the last frame is a torn tail; whole frames out of place are
/// refused.
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

    for schedule in [
        ["Write", "Crash", "Misplace", "Recover"].as_slice(),
        &["Write", "Write", "Write", "Crash", "Misplace", "Recover"],
    ] {
        let moved = walk(&m, schedule);
        assert_eq!(
            read(&moved),
            (0, 1, 0),
            "frames out of place are refused: {moved:?}"
        );
    }
    let torn = walk(&m, &["Write", "Write", "Tear"]);
    assert!(!m.action_enabled("Misplace", &torn), "{torn:?}");
    assert!(
        !m.action_enabled("Misplace", &damaged),
        "one damage per tape: {damaged:?}"
    );
}

/// Every move of model 2, each of which fires in both arms.
const CURSOR_MOVES: [&str; 7] = [
    "AskNext",
    "AskOtherRead",
    "AskRepeat",
    "AskClosingAct",
    "AskOtherAct",
    "AskPastEnd",
    "AskAfterDivergence",
];

/// Model 2 in the arm `acts` (0: strict, 1: the acts scenario).
fn cursor_arm(acts: i64) -> aterm_spec::derive::Model {
    interp::with_consts(
        &aterm_models_harness::supervise_replay_cursor_model(),
        &[("Acts", acts)],
    )
}

/// Tier-0 of `SuperviseReplayCursor` (model 2): proved at `Buggy = 0` and
/// caught at `Buggy = 1` in both arms — the strict one the model commits to
/// and the acts scenario — every move firing in each.
#[test]
fn supervise_replay_cursor_proves_and_catches() {
    for acts in [0, 1] {
        let model = cursor_arm(acts);
        let committed = interp::fired_actions(&model);
        for action in CURSOR_MOVES {
            assert!(
                committed.contains(action),
                "Acts = {acts}: {action} fires at the committed configuration: {committed:?}"
            );
        }
        verify::prove_and_catch_scalar(
            &model,
            "a replay: nothing from the world, no act skipped, nothing after the divergence, \
             nothing from the future",
        );
    }
}

/// Each wrong cursor breaks the law it was written to break, alone, in both
/// arms.
#[test]
fn each_wrong_cursor_breaks_its_own_law() {
    for acts in [0, 1] {
        let model = cursor_arm(acts);
        for (dial, law) in [
            (1, "NoLiveEffect"),
            (2, "NoActSkipped"),
            (3, "NothingServedAfterDivergence"),
            (4, "NothingFromTheFuture"),
        ] {
            let wrong = interp::with_buggy(&only(&model, law), dial);
            assert!(
                interp::violated_invariants(&wrong).contains(&law),
                "Acts = {acts}: Buggy = {dial} breaks {law}"
            );
            let right = interp::with_buggy(&only(&model, law), 0);
            assert!(
                interp::violated_invariants(&right).is_empty(),
                "Acts = {acts}: the committed cursor keeps {law}"
            );
        }
    }
}

/// The strict cursor the way a replay meets its tape: every record asked in
/// order is served to the end, and the end is a divergence, never a question
/// to the world; a read the tape does not hold diverges where it is asked,
/// before the act after it — and the lenient cursor resyncs across that act.
/// A read asked again, or an act asked before its read, diverges too.
#[test]
fn supervise_replay_cursor_walks_a_replay() {
    let m = cursor_arm(0);
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
    for strict_only in [&["AskNext", "AskRepeat"][..], &["AskClosingAct"]] {
        let s = walk(&m, strict_only);
        assert_eq!(s["diverged"], 1, "strict: {strict_only:?} diverges: {s:?}");
    }
    let lenient = interp::with_buggy(&m, 2);
    let resynced = walk(&lenient, &["AskOtherRead"]);
    assert_eq!(
        read(&resynced),
        (3, 0, 0, 1),
        "the lenient cursor passes the act: {resynced:?}"
    );
}

/// The acts cursor the way a counterfactual meets its tape: a read the
/// segment served is served again, the closing act asked with the read
/// before it unasked drops the read and moves to the next segment — no act
/// skipped — and a read held only past the act is STALE, never resynced to
/// nor answered from there; the mutant that answers it from there stands
/// still and serves the future.
#[test]
fn supervise_replay_cursor_walks_a_counterfactual() {
    let m = cursor_arm(1);
    let read = |s: &interp::State| {
        (
            s["pos"],
            s["diverged"],
            s["skipped_act"],
            s["served_future"],
        )
    };
    let repeated = walk(&m, &["AskNext", "AskRepeat", "AskRepeat", "AskNext"]);
    assert_eq!(
        read(&repeated),
        (2, 0, 0, 0),
        "a read served again: {repeated:?}"
    );
    let dropped = walk(&m, &["AskClosingAct", "AskClosingAct"]);
    assert_eq!(
        read(&dropped),
        (4, 0, 0, 0),
        "each read dropped, each act taken: {dropped:?}"
    );
    let stale = walk(&m, &["AskOtherRead"]);
    assert_eq!(read(&stale), (0, 1, 0, 0), "STALE where asked: {stale:?}");
    let past = walk(&m, &["AskClosingAct", "AskClosingAct", "AskPastEnd"]);
    assert_eq!(past["diverged"], 1, "nothing past the last act: {past:?}");
    let future = interp::with_buggy(&m, 4);
    let served = walk(&future, &["AskOtherRead"]);
    assert_eq!(
        read(&served),
        (0, 0, 0, 1),
        "the future served, the cursor still: {served:?}"
    );
}

/// Tier-0 of `SupervisorRaisedLatch` (model 3): every move fires at the committed
/// configuration (`aterm-models`' `non_vacuity` proves and catches it).
#[test]
fn supervisor_raised_latch_fires_every_committed_move() {
    use aterm_models_harness::supervisor_raised_latch_model;
    let model = supervisor_raised_latch_model();
    let committed = interp::fired_actions(&model);
    for action in ["Raise", "LoopOp", "EndRecord", "ReplayOp", "ReplayEnd"] {
        assert!(
            committed.contains(action),
            "{action} fires at the committed configuration: {committed:?}"
        );
    }
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
