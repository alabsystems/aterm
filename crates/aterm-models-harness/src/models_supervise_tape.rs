// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE HARNESS TAPE'S RECOVERY (the replay plan's §6.4 and its model 1): what a
//! reader may make of the bytes a tape's writer left behind — whole, cut short
//! by a crash, or damaged at rest.
//!
//! A harness tape (`aterm-agent/src/supervise/tape/`) is a file of frames,
//! each written by ONE `write(2)` and checked by its own xxh64, which also
//! names the frame's place (frame format 2). The writer never fsyncs between
//! frames (a tape is a diagnostic record, as the journal is), so a crash can
//! leave a frame half on disk, and the disk can damage a byte of a frame long
//! after it was written, or repeat, drop or move whole frames. Recovery is a
//! pure function of the bytes, with astream-engine's store classification: a
//! short or damaged frame with nothing whole after it is the TORN TAIL a crash
//! leaves — dropped, and said — and one with a whole frame after it is a
//! FAULT, which a plain read refuses with its offset rather than truncating
//! away frames that were written whole; a whole frame out of its place is a
//! fault wherever it stands.

use aterm_spec::derive::Model;

/// One tape and its one reading (`aterm-agent/src/supervise/tape/frame.rs`'s
/// `TapeWriter` and `recover.rs`'s `recover`).
///
/// State: `written`, the frames on disk WHOLE AND INTACT (the header's
/// first); `torn`, a frame after them that is not (half-written, or the
/// damaged last one); `rot`, a damaged byte in a frame with a whole, intact
/// frame after it; `magic_rot`, a damaged byte of the file's magic;
/// `misplaced`, whole frames on disk that are not the writer's in its order
/// (one repeated, dropped or moved); `flipped`, the tape already took its one
/// damage; `crashed`, the writer is gone (a crash, or the run's end), so the
/// bytes are at rest; then the reading — `done`, `recovered` (the frames it
/// returned), `refused` (it refused the tape: a fault) and `said_torn` (it
/// reported a torn tail).
///
/// The moves: `Write` (a frame closed by the writer: one whole frame more;
/// the first write is the magic and the header frame together), `Tear` (the
/// writer dies inside a frame's write: a partial frame follows the whole
/// ones), `Crash` (the writer dies, or ends, between frames), `Rot` (a byte of
/// a frame that has a whole frame after it is damaged at rest), `RotLast` (a
/// byte of the LAST whole frame is: no reader can tell that frame from one a
/// crash cut short, so it IS the torn tail — `written` loses it and `torn`
/// holds it, and the reading says torn, never silently clean), `RotMagic` (a
/// byte of the magic is: the reader checks the magic before any frame, and a
/// file that does not start with it cannot be told from a file that is no
/// tape, so it is refused whatever follows — also when the header is the
/// last whole frame), `Misplace` (whole frames are repeated, dropped or moved
/// at rest, every frame still whole: each frame's sum names its place, so the
/// first out of place is refused wherever it stands, the last frame's place
/// included — no crash writes a whole frame at the wrong place; the frames
/// cut short at a boundary are `Crash`, which no frame can tell, and not this
/// move), and `Recover` (the plain read).
///
/// Invariants: `RecoveredIsPrefix` — the reader never returns more than the
/// whole frames (a torn frame is never taken). `NoSilentLoss` — every whole,
/// intact frame comes back unless the tape is refused. `RotRefused` — damage
/// with a whole frame after it, or to the magic, refuses the tape: it is never
/// read through, skipped, or truncated away. `MisplacedRefused` — whole frames
/// out of place refuse the tape: never read back as written. `TornSaid` — a
/// tape that is not refused is said torn exactly when it is.
///
/// One damage per tape: the Tier-1 bind flips one byte, or arranges the whole
/// frames once. Two would let a later damage hide an earlier one's witness
/// (`Rot` in frame 0, then `RotLast` taking the only whole frame after it),
/// which the reader then rightly calls torn.
///
/// The mutants, each a reader that is wrong in one way: `Buggy = 1` ignores
/// the checksum and reads a damaged frame as written (`RotRefused`);
/// `Buggy = 2` takes the torn frame too (`RecoveredIsPrefix`, `NoSilentLoss`);
/// `Buggy = 3` drops a damaged frame and reads on (`NoSilentLoss`,
/// `RotRefused`); `Buggy = 4` reports a torn tail clean (`TornSaid`);
/// `Buggy = 5` never checks the magic and reads the frames after a damaged one
/// as a tape's (`RotRefused`); `Buggy = 6` takes a whole frame at any place —
/// format 1's sums, which name none — and reads frames repeated, dropped or
/// moved as written (`MisplacedRefused`).
///
/// What the model does not hold: the reader's own bounds on bytes no writer
/// leaves (`recover.rs`'s `HELD_MAX`, and its search's), which refuse a tape
/// made by hand to cost more than its size; a tape the bind writes comes
/// nowhere near either.
///
/// Tier-1 (`aterm-agent/src/supervise/tape/conformance_tests.rs`) writes
/// tapes of one, two and three frames with the real writer, then reads, with
/// the real reader, every byte-offset truncation of each, every byte of
/// each flipped under three masks (0x01, 0x80 and 0xff: the low bit, the high
/// bit, every bit), and every arrangement of each one's whole frames at most
/// one frame longer that is neither the tape nor a prefix of it, projecting
/// each reading onto `recovered`, `refused` and `said_torn` beside the model's
/// own run of the same schedule; the negative controls are a reader that
/// ignores xxh64 and one that ignores the place a sum names, which the bind
/// rejects.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervise_tape_recovery_model() -> Model {
    aterm_spec::ty_model! {
        SuperviseTapeRecovery {
            const Buggy = 0;
            const MaxFrames = 3;
            var written = 0;
            var torn = 0;
            var rot = 0;
            var magic_rot = 0;
            var misplaced = 0;
            var flipped = 0;
            var crashed = 0;
            var done = 0;
            var recovered = 0;
            var refused = 0;
            var said_torn = 0;

            action Write when (crashed == 0 && written <= MaxFrames - 1) {
                written = written + 1;
            }
            action Tear when (crashed == 0) {
                torn = 1;
                crashed = 1;
            }
            action Crash when (crashed == 0) {
                crashed = 1;
            }
            action Rot when (crashed == 1 && done == 0 && flipped == 0 && written > 1) {
                rot = 1;
                flipped = 1;
            }
            action RotLast when (crashed == 1 && done == 0 && flipped == 0 && written > 0) {
                written = written - 1;
                torn = 1;
                flipped = 1;
            }
            action RotMagic when (crashed == 1 && done == 0 && flipped == 0 && written > 0) {
                magic_rot = 1;
                flipped = 1;
            }
            action Misplace when (crashed == 1 && done == 0 && flipped == 0 && torn == 0 && written > 0) {
                misplaced = 1;
                flipped = 1;
            }
            action Recover when (crashed == 1 && done == 0) {
                done = 1;
                refused = if magic_rot == 1 && Buggy == 5 {
                    0
                } else if magic_rot == 1 {
                    1
                } else if misplaced == 1 && Buggy == 6 {
                    0
                } else if misplaced == 1 {
                    1
                } else if rot == 1 && (Buggy == 1 || Buggy == 3) {
                    0
                } else {
                    rot
                };
                recovered = if magic_rot == 1 && Buggy == 5 {
                    written
                } else if magic_rot == 1 {
                    0
                } else if misplaced == 1 && Buggy == 6 {
                    written
                } else if misplaced == 1 {
                    0
                } else if rot == 1 && Buggy == 1 {
                    written
                } else if rot == 1 && Buggy == 3 {
                    written - 1
                } else if rot == 1 {
                    0
                } else if Buggy == 2 {
                    written + torn
                } else {
                    written
                };
                said_torn = if magic_rot == 1 && Buggy == 5 {
                    torn
                } else if magic_rot == 1 {
                    0
                } else if misplaced == 1 {
                    0
                } else if Buggy == 4 {
                    0
                } else if rot == 1 && (Buggy == 1 || Buggy == 3) {
                    torn
                } else if rot == 1 {
                    0
                } else {
                    torn
                };
            }

            invariant RecoveredIsPrefix: recovered <= written;
            invariant NoSilentLoss: done == 0 || refused == 1 || recovered == written;
            invariant RotRefused: done == 0 || (rot == 0 && magic_rot == 0) || refused == 1;
            invariant MisplacedRefused: done == 0 || misplaced == 0 || refused == 1;
            invariant TornSaid: done == 0 || refused == 1 || said_torn == torn;
        }
    }
}

/// A REPLAY'S CURSOR (the replay plan's §5.2 and its model 2, both arms):
/// what a replay of a harness tape serves the loop it re-runs, and when it
/// stops serving.
///
/// A replay (`aterm-agent/src/supervise/tape/replay.rs`) holds the tape's
/// records in order and a cursor into them. The replayed loop asks its doors
/// one value at a time. An ACT on the tape — a key, a send, a post, a badge, a
/// signal, a restart, a host's act — is never skipped: a cursor that resynced
/// past one would replay a run in which the act was taken without the loop
/// having taken it. Nothing is ever answered from the world (a replay holds no
/// socket, no clock, no file), nothing is served after the first divergence,
/// and nothing is served from past the act the loop has not yet taken.
///
/// * **The strict arm** (`Acts = 0`, `aterm drive replay FILE`): the record
///   under the cursor answers exactly the request it recorded — anything else
///   is the first divergence, and a request past the tape's end is one too.
/// * **The acts arm** (`Acts = 1`, the scenario: a counterfactual replay,
///   `aterm drive replay FILE --set …|--policy …`, `tape::acts`): the tape is
///   cut into SEGMENTS by its acts. A control-lane read is served from the
///   loop's own segment alone — the first unconsumed record that holds it,
///   the reads skipped on the way dropped, else one the segment already
///   served, repeated — and an act must be the segment's closing act, which
///   moves the cursor to the next segment. Neither ever crosses an act: a
///   control-lane read the segment does not hold is STALE, a divergence
///   like any other.
///
///   What this machine leaves out: two kinds of request the real cursor
///   answers from BEFORE the cursor, across an act already taken — a derived
///   probe (`probe.*`, by astream-effects' keyed rule) and housekeeping (the
///   claim's renewal and release, never compared) — and a wait for what the
///   recorded run waited for only past the segment's act, which it refuses
///   as that act skipped. None moves the cursor or reads past it, so none
///   can break `NoActSkipped` or `NothingFromTheFuture`; the Tier-1 bind
///   drives control-lane reads and acts only, and `tape::acts`' own tests
///   hold those paths (a probe from the segment before, a release past the
///   act never served, a wait past the act refused).
///
/// The tape is four records, `R0 read · R1 act · R2 read · R3 act`
/// (`Records = 4`; record `p` is an act exactly when `p` is 1 or 3), two
/// segments of one read and its act. State: `pos`, the cursor (records
/// consumed); `diverged`, the first divergence latched (a STALE one too);
/// `live`, a value answered from the world; `skipped_act`, an act the cursor
/// passed without the loop asking it; `served_after`, a value served after the
/// divergence; `served_future`, a value served from past the act the loop has
/// not taken.
///
/// The moves, each one request of the loop's: `AskNext` (the very request the
/// record under the cursor holds: served, the cursor steps), `AskOtherRead` (a
/// control-lane read the loop's segment does not hold: one held only past its
/// act, or only in a segment before it), `AskRepeat` (the read the segment already served,
/// asked again with the act next: strict diverges, acts serves it repeated),
/// `AskClosingAct` (the segment's act asked with its read unasked: strict
/// diverges, acts drops the read and takes the act), `AskOtherAct` (an act the
/// segment does not close with), `AskPastEnd` (any request once the tape is
/// spent) and `AskAfterDivergence` (any request once the first divergence is
/// latched).
///
/// Invariants: `NoLiveEffect` — nothing is ever answered from the world;
/// `NoActSkipped` — no act is passed unasked; `NothingServedAfterDivergence`;
/// `NothingFromTheFuture` — no read is answered from past an act not taken.
///
/// The mutants, in both arms: `Buggy = 1` answers past the tape's end from the
/// world (`NoLiveEffect`); `Buggy = 2` resyncs a read it does not hold forward
/// to the next read that matches, across the act between (`NoActSkipped`) —
/// the lenient synthesis the plan rejected (its D1-3), and in the acts arm a
/// resync with no segment bound; `Buggy = 3` keeps serving after the
/// divergence (`NothingServedAfterDivergence`); `Buggy = 4` answers that read
/// from the record past the act that holds it, the cursor standing where it
/// was (`NothingFromTheFuture`) — serving the loop its future without skipping
/// anything.
///
/// Tier-1 (`aterm-agent/src/supervise/tape/conformance_tests.rs`) drives the
/// real replay core through its replaying door over a real four-record tape
/// with schedules drawn from `aterm_spec::rng`, each request one step,
/// projecting the core onto the six variables — the strict core against
/// `Acts = 0`, the acts core against `Acts = 1`; the negative control is a
/// cursor that resyncs across acts, which both binds reject and the
/// `Buggy = 2` machine predicts.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervise_replay_cursor_model() -> Model {
    aterm_spec::ty_model! {
        SuperviseReplayCursor {
            const Buggy = 0;
            const Acts = 0;
            // The counterfactual replay's arm, which the code also runs.
            scenario Acts = 1;
            const Records = 4;
            var pos = 0;
            var diverged = 0;
            var live = 0;
            var skipped_act = 0;
            var served_after = 0;
            var served_future = 0;

            action AskNext when (diverged == 0 && pos <= Records - 1) {
                pos = pos + 1;
            }
            action AskOtherRead when (diverged == 0 && pos <= Records - 1) {
                skipped_act = if Buggy == 2 && pos <= 1 { 1 } else { skipped_act };
                served_future = if Buggy == 4 && pos <= 1 { 1 } else { served_future };
                pos = if Buggy == 2 && pos <= 1 { 3 } else { pos };
                diverged = if (Buggy == 2 || Buggy == 4) && pos <= 1 { 0 } else { 1 };
            }
            action AskRepeat when (diverged == 0 && (pos == 1 || pos == 3)) {
                diverged = if Acts == 1 { 0 } else { 1 };
            }
            action AskClosingAct when (diverged == 0 && (pos == 0 || pos == 2)) {
                pos = if Acts == 1 { pos + 2 } else { pos };
                diverged = if Acts == 1 { 0 } else { 1 };
            }
            action AskOtherAct when (diverged == 0 && pos <= Records - 1) {
                diverged = 1;
            }
            action AskPastEnd when (diverged == 0 && pos == Records && live == 0) {
                live = if Buggy == 1 { live + 1 } else { live };
                diverged = if Buggy == 1 { 0 } else { 1 };
            }
            action AskAfterDivergence when (diverged == 1 && served_after <= 1) {
                served_after = if Buggy == 3 { served_after + 1 } else { served_after };
            }

            invariant NoLiveEffect: live == 0;
            invariant NoActSkipped: skipped_act == 0;
            invariant NothingServedAfterDivergence: served_after == 0;
            invariant NothingFromTheFuture: served_future == 0;
        }
    }
}

/// THE STOP FLAG'S LATCH ON A TAPE (the replay plan's model 3): a flag another
/// thread raises — the hosted loop's stop — recorded so that its replay
/// cannot diverge.
///
/// The loop reads the flag through its door (`Fx::raised`) between the other
/// doors it crosses; another thread raises it at a moment of its own. The tape
/// holds the flag as ONE record, the first read that saw it raised — appended
/// BY THE LOOP'S OWN READ, at the read's place among the loop's records. A
/// replay answers each read false until the read whose place that record
/// holds, true from then on. A record appended by the RAISING thread instead,
/// at the moment it raised, would sit wherever the loop happened to be — in
/// front of a door, after the last read — and the replay would meet it where
/// it asks a door: a divergence no change in the loop made.
///
/// The loop is four operations, `R0 read · D1 door · R2 read · D3 door`
/// (`pos` counts them; an even `pos` is a read). State, recording: `up`, the
/// flag raised; `marked` and `mark`, the flag's record taped and the loop
/// operation it sits before (`4`: after the last); `rec`, the recording
/// ended. Replaying: `rpos`, the replayed loop's operations; `diverged`; and
/// `done`, the replay's end checked.
///
/// The moves: `Raise` (the other thread raises the flag, at any moment of the
/// recording), `LoopOp` (the recorded loop's next operation — a read that sees
/// the flag raised and has no record yet tapes it, in the committed design),
/// `EndRecord`, `ReplayOp` (the replayed loop's next operation: a door that
/// meets the flag's record diverges) and `ReplayEnd` (a record left past the
/// last operation diverges).
///
/// Invariants: `TapedByTheRead` — the flag's record sits only at a read's
/// place; `NoSpuriousDivergence` — the replay of a recording never diverges.
///
/// The mutant: `Buggy = 1` is the setter that appends the record itself, at
/// the moment it raises (both invariants).
///
/// Tier-1 (`aterm-agent/src/supervise/tape/conformance_tests.rs`): a real
/// thread raises the stop at each point of a real recorded four-operation loop
/// (`Fx::raised` between round trips through the recording door), the tape is
/// replayed through the replaying door by the same loop, and each run is
/// projected onto `marked`, `mark` and `diverged`; the negative control is a
/// setter that appends the record itself, which the bind rejects and the
/// `Buggy = 1` machine predicts. A second run has the thread raise the flag at
/// moments of its own, racing the loop, over many seeds: no replay diverges.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_raised_latch_model() -> Model {
    aterm_spec::ty_model! {
        SupervisorRaisedLatch {
            const Buggy = 0;
            const Ops = 4;
            var up = 0;
            var marked = 0;
            var mark = 0;
            var pos = 0;
            var rec = 0;
            var rpos = 0;
            var diverged = 0;
            var done = 0;

            action Raise when (rec == 0 && up == 0) {
                up = 1;
                mark = if Buggy == 1 { pos } else { mark };
                marked = if Buggy == 1 { 1 } else { marked };
            }
            action LoopOp when (rec == 0 && pos <= Ops - 1) {
                mark = if Buggy == 0 && up == 1 && marked == 0 && (pos == 0 || pos == 2) {
                    pos
                } else {
                    mark
                };
                marked = if Buggy == 0 && up == 1 && marked == 0 && (pos == 0 || pos == 2) {
                    1
                } else {
                    marked
                };
                pos = pos + 1;
            }
            action EndRecord when (rec == 0 && pos == Ops) {
                rec = 1;
            }
            action ReplayOp when (rec == 1 && rpos <= Ops - 1 && diverged == 0) {
                diverged = if marked == 1 && mark == rpos && (rpos == 1 || rpos == 3) { 1 } else { 0 };
                rpos = rpos + 1;
            }
            action ReplayEnd when (rec == 1 && rpos == Ops && done == 0) {
                diverged = if marked == 1 && mark == Ops { 1 } else { diverged };
                done = 1;
            }

            invariant TapedByTheRead: marked == 0 || mark == 0 || mark == 2;
            invariant NoSpuriousDivergence: diverged == 0;
        }
    }
}
