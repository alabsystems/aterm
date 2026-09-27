// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The rainbow ribbon's follow pass: the twin bit the witness WRITES when it
//! arms a record, and the follow verdict that READS it.

use super::Model;

/// **THE TWIN CONTRACT OF THE FOLLOW PASS** (2026-09-23 — the owner: *"the
/// existing line rainbow should beautifully flow and drift and fade away,
/// not simply abruptly vanish"*). `aterm-effects`' content witness arms a
/// record per typed glyph and, when the row one up already holds the same
/// glyph at the same column, WRITES a twin bit (`Witness::walk`'s arm,
/// `Seen::twins`); the follow pass later READS it to decide whether the
/// run's text moved one row up (`Witness::follow_runs` /
/// `Witness::follow_verdict`). The model states both halves as one
/// contract over a run of `N` armed records, scanned in column order. Each
/// record is one of three classes at the target row — ARRIVED (its glyph
/// there, no twin: it came), STANDING (its glyph there and a twin was: it
/// stood there before the record was armed), ABSENT (its glyph not there,
/// twin or not) — and GONE or not from its own row.
///
/// The TRUTH (`arr`, `stand`, `gone`, `first_arr`, `last_arr`, `split`,
/// `lead_run`, `tail_run`) is stated over the classes alone; the READER
/// (`found`, `neutral`, `after`, `broken`, `rlead`, `lo`, `hi`) is
/// `follow_verdict`'s record-by-record scan; `Decide` gives the verdict
/// (`named`, `missed`) with `follow_runs`' gates — two or more gone and no
/// fewer than half, two or more found and no fewer than half of the
/// records not neutral, the found records one block. Columns are 1-based
/// here (`0` is none).
///
/// * `AllStandingNeverFollows` — no ARRIVED record, no follow: an erase
///   under an identical line (every record a twin) names nothing
///   (2026-09-22 review).
/// * `AnArrivedBlockFollows` — two or more ARRIVED, no ABSENT between the
///   first and the last, `2·arr ≥ N − stand`, and the gone gate held: the
///   follow IS named, over exactly the ARRIVED block plus the STANDING
///   records flush against its edges (the riders, whatever their own row
///   reads — the review of the first cut, 2026-09-23).
/// * `OnlyAnArrivedBlockFollows` — and nothing else is.
/// * `MissedIsAnArrivalNotNamed` — `ribbon_follow_missed=` counts the gone
///   records of a run whose arrivals were enough but not one block, and
///   nothing else.
///
/// Out of the model: which offset (only `dr = −1`), the RELAID suffix of a
/// composer's wrap (`Witness::relaid_suffix` — a denominator discount read
/// off other columns of the run's own row, pinned by the witness units and
/// `tests/composer_growth_follows.rs`), wide glyphs and released records.
/// The Tier-1 conformance (`aterm-effects`' `tests/ribbon_follow_conformance.rs`)
/// drives the REAL arm and follow over every 6-record configuration of the
/// classes (the twin-or-not ABSENT both ways) and gone bits, where no
/// suffix can be relaid, and compares the verdict, extent and missed count
/// with this model at `N = 6`. `Buggy = 1` replays the 2026-09-22 veto the
/// owner's vanish came from: the reader takes a STANDING record for an
/// ABSENT one, so one twin inside an arrived block breaks it and the row
/// melts — the counterexample `AnArrivedBlockFollows` must find.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn ribbon_follow_twin_model() -> Model {
    crate::ty_model! {
        RibbonFollowTwin {
            const Buggy = 0;
            const N = 4;
            var i = 0;
            var done = 0;
            var arr = 0;
            var stand = 0;
            var gone = 0;
            var first_arr = 0;
            var last_arr = 0;
            var gap = 0;
            var split = 0;
            var run = 0;
            var lead_run = 0;
            var tail_run = 0;
            var tail_open = 0;
            var found = 0;
            var neutral = 0;
            var after = 0;
            var broken = 0;
            var rlead = 0;
            var lo = 0;
            var hi = 0;
            var named = 0;
            var missed = 0;

            action ScanArrived when (N > i) {
                i = i + 1;
                arr = arr + 1;
                first_arr = if first_arr == 0 { i + 1 } else { first_arr };
                last_arr = i + 1;
                split = if gap == 1 { 1 } else { split };
                lead_run = if arr == 0 { run } else { lead_run };
                run = 0;
                tail_run = 0;
                tail_open = 1;
                found = found + 1;
                broken = if after == 1 { 1 } else { broken };
                lo = if after == 0 && lo == 0 {
                    if rlead == 0 { i + 1 } else { rlead }
                } else { lo };
                hi = if after == 0 { i + 1 } else { hi };
                rlead = if after == 0 { 0 } else { rlead };
            }
            action ScanArrivedGone when (N > i) {
                i = i + 1;
                gone = gone + 1;
                arr = arr + 1;
                first_arr = if first_arr == 0 { i + 1 } else { first_arr };
                last_arr = i + 1;
                split = if gap == 1 { 1 } else { split };
                lead_run = if arr == 0 { run } else { lead_run };
                run = 0;
                tail_run = 0;
                tail_open = 1;
                found = found + 1;
                broken = if after == 1 { 1 } else { broken };
                lo = if after == 0 && lo == 0 {
                    if rlead == 0 { i + 1 } else { rlead }
                } else { lo };
                hi = if after == 0 { i + 1 } else { hi };
                rlead = if after == 0 { 0 } else { rlead };
            }
            action ScanStanding when (N > i) {
                i = i + 1;
                stand = stand + 1;
                run = run + 1;
                tail_run = if tail_open == 1 { tail_run + 1 } else { tail_run };
                neutral = if Buggy == 1 { neutral } else { neutral + 1 };
                after = if Buggy == 1 && found > 0 { 1 } else { after };
                rlead = if found == 0 {
                    if Buggy == 1 { 0 } else { if rlead == 0 { i + 1 } else { rlead } }
                } else { rlead };
                hi = if Buggy == 0 && found > 0 && after == 0 { i + 1 } else { hi };
            }
            action ScanStandingGone when (N > i) {
                i = i + 1;
                gone = gone + 1;
                stand = stand + 1;
                run = run + 1;
                tail_run = if tail_open == 1 { tail_run + 1 } else { tail_run };
                neutral = if Buggy == 1 { neutral } else { neutral + 1 };
                after = if Buggy == 1 && found > 0 { 1 } else { after };
                rlead = if found == 0 {
                    if Buggy == 1 { 0 } else { if rlead == 0 { i + 1 } else { rlead } }
                } else { rlead };
                hi = if Buggy == 0 && found > 0 && after == 0 { i + 1 } else { hi };
            }
            action ScanAbsent when (N > i) {
                i = i + 1;
                gap = if arr > 0 { 1 } else { gap };
                run = 0;
                tail_open = 0;
                after = if found > 0 { 1 } else { after };
                rlead = if found == 0 { 0 } else { rlead };
            }
            action ScanAbsentGone when (N > i) {
                i = i + 1;
                gone = gone + 1;
                gap = if arr > 0 { 1 } else { gap };
                run = 0;
                tail_open = 0;
                after = if found > 0 { 1 } else { after };
                rlead = if found == 0 { 0 } else { rlead };
            }
            action Decide when (i == N && done == 0) {
                done = 1;
                named = if broken == 0 && found > 1 && N <= found + found + neutral
                    && gone > 1 && N <= gone + gone { 1 } else { 0 };
                missed = if broken == 1 && found > 1 && N <= found + found + neutral
                    && gone > 1 && N <= gone + gone { gone } else { 0 };
            }

            invariant AllStandingNeverFollows:
                if done == 1 && arr == 0 { named == 0 } else { named <= 1 };
            invariant AnArrivedBlockFollows:
                if done == 1 && split == 0 && arr > 1 && N <= arr + arr + stand
                    && gone > 1 && N <= gone + gone {
                    named == 1 && lo + lead_run == first_arr && hi == last_arr + tail_run
                } else { named <= 1 };
            invariant OnlyAnArrivedBlockFollows:
                if named == 1 {
                    done == 1 && split == 0 && arr > 1 && N <= arr + arr + stand
                        && gone > 1 && N <= gone + gone
                } else { named == 0 };
            invariant MissedIsAnArrivalNotNamed:
                if done == 1 && split == 1 && arr > 1 && N <= arr + arr + stand
                    && gone > 1 && N <= gone + gone {
                    missed == gone
                } else { missed == 0 };
            invariant StateBounded:
                i <= N && done <= 1 && arr + stand <= i && gone <= i
                    && found <= i && neutral <= i && lo <= N && hi <= N
                    && rlead <= N && named <= 1 && missed <= N;
        }
    }
}

/// **THE ARRIVAL EVIDENCE THE WITNESS WRITES** (2026-09-25/26 — the writer
/// half of [`ribbon_follow_twin_model`]'s reader). The follow verdict reads,
/// per armed record and per follow offset, whether a glyph found there
/// ARRIVED (`Seen::arrivals`: `clear && !twin`) or is NEUTRAL; this model
/// states what the witness must have written by then, over ONE record and
/// ONE offset through the frames of its life. Each frame the row at that
/// offset is sampled holding the record's own glyph (SAME), something else
/// (DIFF), or not sampled (UNSAMPLED), and wall time advances one or two
/// units — frames are 8-16 ms apart while a key's light is live and 110-130
/// ms apart once the window idles, so the unit is time, not frames
/// (`TwinMin = 2` units is `TWIN_MIN`, 40 ms, at 20 ms a unit).
///
/// The TRUTH (`first`, `streak`, `stood`) is stated over what the row was
/// seen holding; the WRITER (`clear`, `twin`, `pending`, `pend`) is
/// `Seen::observe` / `Witness::insert`:
///
/// * `CopyThereFirstIsNoArrival` — a copy standing at the offset when the
///   record was armed is never an arrival: `$ cd ..` typed under `$ cd ..`
///   then Ctrl-U lights nothing (2026-09-22 review).
/// * `ACopyThatStoodIsNoArrival` — a copy that APPEARED beside the standing
///   line and was seen there, with nothing else seen there between, for
///   `TwinMin` is never an arrival either: fzf's list landing under the
///   query, a completion popup, a job notice's re-echo (2026-09-25).
/// * `AnythingElseIsAnArrival` — and nothing else is refused: a row never
///   sampled is no evidence against a move, and a copy seen for less than
///   `TwinMin` is a TORN REPAINT of the move itself (a program without a
///   synchronized-update bracket drawing the new row before erasing the
///   old), which must follow (`tests/moved_again_without_a_key.rs`).
///
/// The bug selector replays the three historical writers, each caught by
/// its own invariant: `Buggy = 1` is the 2026-09-23 NEUTRAL-at-arm writer
/// (a twin only when the copy was there at arm) — the fzf false follow;
/// `Buggy = 2` is the fail-closed writer of the first review round (an
/// offset counts as clear only once seen holding something else) — the
/// missed follows; `Buggy = 3` is the writer before 2026-09-22 (no twin at
/// all) — the Ctrl-U that lit the copy. Out of the model: band moves
/// (`band_keeps`), the scroll's dating of rows it brings in (`last_walk`),
/// the deferral. The Tier-1 bind (`aterm-effects`'
/// `tests/ribbon_arrival_conformance.rs`) drives the REAL arm, walks and
/// follow pass over every trace of this model up to its bound and checks
/// the follow is named exactly where the model's writer says ARRIVED.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn ribbon_arrival_evidence_model() -> Model {
    crate::ty_model! {
        RibbonArrivalEvidence {
            const Buggy = 0;
            const TwinMin = 2;
            const T = 5;
            var armed = 0;
            var t = 0;
            var first = 0;
            var streak = 0;
            var stood = 0;
            var clear = 0;
            var twin = 0;
            var pending = 0;
            var pend = 0;

            action ArmSame when (armed == 0) {
                armed = 1;
                first = 1;
                clear = if Buggy == 2 { 0 } else { 1 };
                twin = if Buggy == 3 { 0 } else { 1 };
            }
            action ArmDiff when (armed == 0) {
                armed = 1;
                clear = 1;
            }
            action ArmUnsampled when (armed == 0) {
                armed = 1;
                clear = if Buggy == 2 { 0 } else { 1 };
            }
            action SameAfterOne when (armed == 1 && t + 1 <= T) {
                t = t + 1;
                streak = if streak == 0 { t + 1 } else { streak };
                stood = if streak > 0 && TwinMin <= t + 1 - streak { 1 } else { stood };
                pend = if pending == 0 { t + 1 } else { pend };
                twin = if Buggy == 1 || Buggy == 3 { twin } else {
                    if pending == 1 && TwinMin <= t + 1 - pend { 1 } else { twin }
                };
                pending = 1;
            }
            action SameAfterTwo when (armed == 1 && t + 2 <= T) {
                t = t + 2;
                streak = if streak == 0 { t + 2 } else { streak };
                stood = if streak > 0 && TwinMin <= t + 2 - streak { 1 } else { stood };
                pend = if pending == 0 { t + 2 } else { pend };
                twin = if Buggy == 1 || Buggy == 3 { twin } else {
                    if pending == 1 && TwinMin <= t + 2 - pend { 1 } else { twin }
                };
                pending = 1;
            }
            action DiffAfterOne when (armed == 1 && t + 1 <= T) {
                t = t + 1;
                streak = 0;
                clear = 1;
                pending = 0;
                pend = 0;
            }
            action DiffAfterTwo when (armed == 1 && t + 2 <= T) {
                t = t + 2;
                streak = 0;
                clear = 1;
                pending = 0;
                pend = 0;
            }
            action UnsampledAfterOne when (armed == 1 && t + 1 <= T) {
                t = t + 1;
            }
            action UnsampledAfterTwo when (armed == 1 && t + 2 <= T) {
                t = t + 2;
            }

            invariant CopyThereFirstIsNoArrival:
                if first == 1 { twin == 1 } else { twin <= 1 };
            invariant ACopyThatStoodIsNoArrival:
                if stood == 1 { twin == 1 } else { twin <= 1 };
            invariant AnythingElseIsAnArrival:
                if armed == 1 && first == 0 && stood == 0 {
                    clear == 1 && twin == 0
                } else { clear <= 1 };
            invariant StateBounded:
                armed <= 1 && t <= T && first <= 1 && stood <= 1 && clear <= 1
                    && twin <= 1 && pending <= 1 && pend <= T + 1 && streak <= T + 1;
        }
    }
}
