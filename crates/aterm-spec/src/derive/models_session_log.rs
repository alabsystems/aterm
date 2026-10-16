// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The session log (`docs/DESIGN-session-log-2026-10-15.md` §9, slice 1):
//! the per-session ring with its open/close discipline, and the turn
//! tracker over screen verdict words. Tier-1 bound in aterm-slog
//! (`tests/conformance_ring.rs`, `tests/conformance_turn.rs`), which drive
//! the shipping `Log` and `TurnTracker` and project them onto these
//! variables; no per-method `#[refines]` anchors (aterm-slog links no
//! aterm-spec outside its tests).

use super::*;

/// SESSION LOG RING (slice 1): one session's bounded ring of records, the
/// open path of one run, its turn and the turn's step, a reader's pull
/// cursor, and a seamless update that carries nothing.
///
/// Variables: `seq` the last seq assigned, `lo` the oldest live seq (the
/// ring holds `seq - lo + 1` records, at most `Cap`), `lost` the log's exact
/// eviction count; `run`/`turn`/`step` the open path, and `sorph` whether
/// the open step is an orphan hanging from the run (§2.3: a step asked with
/// no turn open, or under a closed one, attaches to the nearest open
/// ancestor, `orphan=1`; the turn tracker's wall at rest); `tclose` the close
/// records written for the current turn node, `topen` the seq of its open
/// record; `fturn` the fold's view of an open turn (the fold lives outside
/// the ring); `ep` the log's epoch (a handoff starts epoch 1, its first
/// record `gap why=handoff` at seq 1); the reader's epoch `rep`, cursor
/// `cur` (the last seq it holds: `since=` is exclusive), the records it was
/// handed `got` and the losses it was told `rlost`.
///
/// Every append evicts the oldest record when the ring would pass `Cap` and
/// counts it. `CloseTurnCascade` is the two records a turn close writes when
/// its step is open: the step `out=gone`, then the turn. `CloseRunCascade`
/// is a run closed over its open turn and step (an agent leaving
/// mid-turn): `1 + turn + step` records, innermost first. A turn closes
/// without touching an orphan step, which hangs from the run.
///
/// `Buggy = 1` adds one mutant per law, each a dead action at the committed
/// config: an append that does not evict (`Bounded`), an eviction nobody
/// counts (`NoSilentLoss`), a turn closed with its step left open and a
/// step attached to a closed turn (`WellNested`), a second close of a closed turn (`CloseOnce`), a
/// handoff that restarts seqs without marking the new epoch, so the reader's
/// cursor is past the head of the log it now reads (`SeqStrictlyIncreasing`),
/// and a fold recomputed from a ring that evicted the open turn's record
/// (`ProjectionIndependentOfEviction`).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn session_log_ring_model() -> Model {
    crate::ty_model! {
        SessionLogRing {
            const Cap = 2;
            const MaxSeq = 5;
            const Buggy = 0;
            var seq = 0;
            var lo = 1;
            var lost = 0;
            var ep = 0;
            var run = 0;
            var turn = 0;
            var step = 0;
            var sorph = 0;
            var tclose = 0;
            var topen = 0;
            var fturn = 0;
            var rep = 0;
            var cur = 0;
            var got = 0;
            var rlost = 0;

            action OpenRun when (run == 0 && seq <= MaxSeq - 1) {
                run = 1;
                seq = seq + 1;
                lo = if seq + 2 - lo > Cap { lo + 1 } else { lo };
                lost = if seq + 2 - lo > Cap { lost + 1 } else { lost };
            }
            action OpenTurn when (run == 1 && turn == 0 && seq <= MaxSeq - 1) {
                turn = 1;
                tclose = 0;
                topen = seq + 1;
                fturn = 1;
                seq = seq + 1;
                lo = if seq + 2 - lo > Cap { lo + 1 } else { lo };
                lost = if seq + 2 - lo > Cap { lost + 1 } else { lost };
            }
            action OpenStep when (turn == 1 && step == 0 && seq <= MaxSeq - 1) {
                step = 1;
                sorph = 0;
                seq = seq + 1;
                lo = if seq + 2 - lo > Cap { lo + 1 } else { lo };
                lost = if seq + 2 - lo > Cap { lost + 1 } else { lost };
            }
            action OpenOrphanStep when (run == 1 && turn == 0 && step == 0 && seq <= MaxSeq - 1) {
                step = 1;
                sorph = 1;
                seq = seq + 1;
                lo = if seq + 2 - lo > Cap { lo + 1 } else { lo };
                lost = if seq + 2 - lo > Cap { lost + 1 } else { lost };
            }
            action CloseStep when (step == 1 && seq <= MaxSeq - 1) {
                step = 0;
                sorph = 0;
                seq = seq + 1;
                lo = if seq + 2 - lo > Cap { lo + 1 } else { lo };
                lost = if seq + 2 - lo > Cap { lost + 1 } else { lost };
            }
            action CloseTurn when (turn == 1 && (step == 0 || sorph == 1) && seq <= MaxSeq - 1) {
                turn = 0;
                tclose = tclose + 1;
                topen = 0;
                fturn = 0;
                seq = seq + 1;
                lo = if seq + 2 - lo > Cap { lo + 1 } else { lo };
                lost = if seq + 2 - lo > Cap { lost + 1 } else { lost };
            }
            action CloseTurnCascade when (turn == 1 && step == 1 && sorph == 0 && seq <= MaxSeq - 2) {
                step = 0;
                turn = 0;
                tclose = tclose + 1;
                topen = 0;
                fturn = 0;
                seq = seq + 2;
                lo = if seq + 3 - lo > Cap + 1 {
                    lo + 2
                } else if seq + 3 - lo > Cap {
                    lo + 1
                } else {
                    lo
                };
                lost = if seq + 3 - lo > Cap + 1 {
                    lost + 2
                } else if seq + 3 - lo > Cap {
                    lost + 1
                } else {
                    lost
                };
            }
            action CloseRun when (run == 1 && turn == 0 && step == 0 && seq <= MaxSeq - 1) {
                run = 0;
                seq = seq + 1;
                lo = if seq + 2 - lo > Cap { lo + 1 } else { lo };
                lost = if seq + 2 - lo > Cap { lost + 1 } else { lost };
            }
            action CloseRunCascade when (run == 1 && 1 <= turn + step && seq <= MaxSeq - 1 - turn - step) {
                run = 0;
                turn = 0;
                step = 0;
                sorph = 0;
                tclose = if turn == 1 { tclose + 1 } else { tclose };
                topen = 0;
                fturn = 0;
                seq = seq + 1 + turn + step;
                lo = if seq + 2 + turn + step - lo > Cap { seq + 2 + turn + step - Cap } else { lo };
                lost = if seq + 2 + turn + step - lo > Cap {
                    lost + seq + 2 + turn + step - Cap - lo
                } else {
                    lost
                };
            }
            action Mark when (seq <= MaxSeq - 1) {
                seq = seq + 1;
                lo = if seq + 2 - lo > Cap { lo + 1 } else { lo };
                lost = if seq + 2 - lo > Cap { lost + 1 } else { lost };
            }
            action Handoff when (ep == 0 && seq > 0) {
                ep = 1;
                seq = 1;
                lo = 1;
                lost = 0;
                run = 0;
                turn = 0;
                step = 0;
                sorph = 0;
                tclose = 0;
                topen = 0;
                fturn = 0;
            }
            action ReadReset when (ep > rep) {
                rep = ep;
                cur = 0;
                got = 0;
                rlost = 0;
            }
            action ReadGap when (rep == ep && lo > cur + 1) {
                rlost = rlost + lo - 1 - cur;
                cur = lo - 1;
            }
            action ReadOne when (rep == ep && cur + 1 <= seq && lo <= cur + 1) {
                cur = cur + 1;
                got = got + 1;
            }

            action AppendNoEvict when (Buggy == 1 && seq - lo + 1 == Cap && seq <= MaxSeq - 1) {
                seq = seq + 1;
            }
            action EvictUncounted when (Buggy == 1 && seq - lo + 1 == Cap && seq <= MaxSeq - 1) {
                seq = seq + 1;
                lo = lo + 1;
            }
            action CloseParentLeaveChild when (Buggy == 1 && turn == 1 && step == 1 && sorph == 0 && seq <= MaxSeq - 1) {
                turn = 0;
                tclose = tclose + 1;
                topen = 0;
                fturn = 0;
                seq = seq + 1;
                lo = if seq + 2 - lo > Cap { lo + 1 } else { lo };
                lost = if seq + 2 - lo > Cap { lost + 1 } else { lost };
            }
            action StepUnderClosedTurn when (Buggy == 1 && run == 1 && turn == 0 && step == 0 && tclose == 1 && seq <= MaxSeq - 1) {
                step = 1;
                sorph = 0;
                seq = seq + 1;
                lo = if seq + 2 - lo > Cap { lo + 1 } else { lo };
                lost = if seq + 2 - lo > Cap { lost + 1 } else { lost };
            }
            action CloseTwice when (Buggy == 1 && turn == 0 && tclose == 1 && seq <= MaxSeq - 1) {
                tclose = 2;
                seq = seq + 1;
                lo = if seq + 2 - lo > Cap { lo + 1 } else { lo };
                lost = if seq + 2 - lo > Cap { lost + 1 } else { lost };
            }
            action HandoffReuse when (Buggy == 1 && ep == 0 && rep == 0 && cur > 0) {
                seq = 0;
                lo = 1;
                lost = 0;
                run = 0;
                turn = 0;
                step = 0;
                sorph = 0;
                tclose = 0;
                topen = 0;
                fturn = 0;
            }
            action RefoldFromRing when (Buggy == 1 && fturn == 1 && topen > 0 && lo > topen) {
                fturn = 0;
            }

            invariant Bounded: seq - lo + 1 <= Cap;
            invariant NoSilentLoss: lost == lo - 1 && got + rlost == cur;
            invariant WellNested: turn <= run && step <= run && sorph <= step && (sorph == 1 || step <= turn);
            invariant CloseOnce: tclose <= 1;
            invariant SeqStrictlyIncreasing: ep > rep || cur <= seq;
            invariant ProjectionIndependentOfEviction: fturn == turn;
        }
    }
}

/// TURN TRACKER: at most one open turn per agent run, driven by the screen
/// verdict (design §2.4).
///
/// `word` is the current verdict: 0 `unknown`, 1 `busy`, 2 `idle`, 3
/// `prompt` with the question dialog, 4 `prompt` with an approval box, 5
/// `wall:<kind>`, 6 `wall:unresponsive`, 7 `survey`, 8 `question` (asked in
/// prose), 9 `-`. `turn` is the open turn and `extra` a second one under the
/// same run; `step` the screen step held open (0 none, 1 approval, 2
/// question, 3 wall, 4 stall, 5 survey) and `sturn` whether it hangs from
/// the turn (a wall or stall seen with no turn hangs from the run). `pend`
/// is an armed settle and `ticks` its elapsed half-seconds (`Settle` = 1 s);
/// `Tick` is time passing and `Rest` the host's wake after the settle (a
/// wake before it changes nothing; a late one is applied by the next
/// verdict first). `CtlTurn` is a `ctl turn` exchange arriving while the
/// agent rests, before the settle: the exchange it starts is the next turn,
/// so the resting one closes at once (`cw = 11`). `cw` is the word that
/// closed the last turn (10 none yet, 11 a ctl exchange) and `cticks` how
/// long its resting word had held.
///
/// `Buggy = 1` adds one dead mutant per law: a second turn opened without
/// superseding the first (`AtMostOneOpenTurn`), `unknown` read as an end
/// (`UnknownNeverClosesATurn`), `idle` closing the turn before the settle —
/// the 220 ms mid-answer idle Codex shows (`IdleClosesOnlyAfterSettle`), the
/// question dialog closing its turn (`DialogStaysInTurn`), a step opened
/// under a turn that has closed (`StepsNestInTheirTurn`), and the input
/// stall closing the turn (`StallNeverClosesATurn`).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn turn_tracker_model() -> Model {
    crate::ty_model! {
        TurnTracker {
            const Settle = 2;
            const Buggy = 0;
            var turn = 0;
            var extra = 0;
            var step = 0;
            var sturn = 0;
            var word = 0;
            var pend = 0;
            var ticks = 0;
            var cw = 10;
            var cticks = 0;

            action SeeBusy {
                turn = 1;
                step = 0;
                sturn = 0;
                pend = 0;
                ticks = 0;
                word = 1;
            }
            action SeeDialog {
                turn = 1;
                step = 2;
                sturn = 1;
                pend = 0;
                ticks = 0;
                word = 3;
            }
            action SeeBox {
                turn = 1;
                step = 1;
                sturn = 1;
                pend = 0;
                ticks = 0;
                word = 4;
            }
            action SeeWall {
                step = 3;
                sturn = turn;
                pend = turn;
                ticks = 0;
                word = 5;
            }
            action SeeStall {
                step = 4;
                sturn = turn;
                pend = 0;
                ticks = 0;
                word = 6;
            }
            action SeeSurvey {
                step = if turn == 1 { 5 } else { 0 };
                sturn = turn;
                pend = turn;
                ticks = 0;
                word = 7;
            }
            action SeeIdle {
                step = 0;
                sturn = 0;
                pend = turn;
                ticks = 0;
                word = 2;
            }
            action SeeQuestion {
                step = 0;
                sturn = 0;
                pend = turn;
                ticks = 0;
                word = 8;
            }
            action SeeGone {
                cw = if turn == 1 { 9 } else { cw };
                cticks = if turn == 1 { 0 } else { cticks };
                turn = 0;
                step = 0;
                sturn = 0;
                pend = 0;
                ticks = 0;
                word = 9;
            }
            action SeeUnknown {
                pend = 0;
                ticks = 0;
                word = 0;
            }
            action Tick when (pend == 1 && ticks <= Settle - 1) {
                ticks = ticks + 1;
            }
            action Rest when (pend == 1 && Settle <= ticks) {
                turn = 0;
                cw = word;
                cticks = ticks;
                pend = 0;
                ticks = 0;
                step = if word == 5 { 3 } else { 0 };
                sturn = 0;
            }
            action CtlTurn when (pend == 1 && ticks <= Settle - 1) {
                turn = 0;
                cw = 11;
                cticks = ticks;
                pend = 0;
                ticks = 0;
                step = if word == 5 { 3 } else { 0 };
                sturn = 0;
            }

            action SecondTurn when (Buggy == 1 && turn == 1 && extra == 0) {
                extra = 1;
            }
            action UnknownCloses when (Buggy == 1 && turn == 1 && word > 0) {
                turn = 0;
                cw = 0;
                cticks = 0;
                step = 0;
                sturn = 0;
                pend = 0;
                ticks = 0;
                word = 0;
            }
            action EarlyRest when (Buggy == 1 && pend == 1 && word == 2 && Settle > ticks) {
                turn = 0;
                cw = 2;
                cticks = ticks;
                pend = 0;
                ticks = 0;
                step = 0;
                sturn = 0;
            }
            action DialogCloses when (Buggy == 1 && turn == 1 && step == 0) {
                turn = 0;
                cw = 3;
                cticks = 0;
                step = 0;
                sturn = 0;
                pend = 0;
                ticks = 0;
                word = 3;
            }
            action StepAfterClose when (Buggy == 1 && turn == 0 && step == 0 && 10 > cw) {
                step = 1;
                sturn = 1;
            }
            action StallCloses when (Buggy == 1 && turn == 1 && step == 0) {
                turn = 0;
                cw = 6;
                cticks = 0;
                step = 4;
                sturn = 0;
                pend = 0;
                ticks = 0;
                word = 6;
            }

            invariant AtMostOneOpenTurn: turn + extra <= 1;
            invariant UnknownNeverClosesATurn: cw > 0;
            invariant IdleClosesOnlyAfterSettle: 2 > cw || cw > 2 || Settle <= cticks;
            invariant DialogStaysInTurn: 3 > word || word > 3 || turn == 1;
            invariant StepsNestInTheirTurn: sturn <= turn && sturn <= step;
            invariant StallNeverClosesATurn: 6 > cw || cw > 6;
        }
    }
}
