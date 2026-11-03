// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// One Mac publishing the lab journal (docs/DESIGN-alab-lab-model-2026-11-01.md §3.9, §4.17,
/// §11 step 12; `aterm_db::alab::journal::flush`, `catalog::ring::landing`, `Store::commit_own`):
/// its records in the previous, current and next week's tables (`p`, `c`, `n`), each table
/// holding at most a fixed share of one record of this writer's; the events waiting in its
/// outbox (`pend`, of `noted` events, each a use of one account); and where the lab's tables say
/// that account was last used: `fact` is the newest window table a reader believes it from (0
/// none, 1 the previous week's, 2 the current, 3 the next), through the use's `ev` batch or a
/// `relay` row restating it.
///
/// A flush is due when an event waits or the week has no record of this Mac's yet (`wk`, its
/// `sum` row); the pace line and the six-hour routine only delay it, and are left out. One flush
/// packs one record and lands it in the current week's table (`FlushCurrent`), or in the next
/// week's where this Mac's latest record already is or its current share is full (`FlushNext`);
/// with both shares full it stays local and the outbox keeps its events (`FlushKept`). A record
/// restates the use this Mac once published (`told`): its `ev` batch where the event waits, and
/// otherwise the `relay` row the week's first record carries. A week passing (`Tick`) shifts
/// the tables and deletes the oldest.
///
/// The laws: every share fixed (`FixedShares`), a spill only within the window
/// (`SpillWithinWindow`), this Mac's presence in a window table once it wrote this week
/// (`PresenceInWindow`), the last use it published believed from some window table whenever it
/// wrote this week, though every table that held it was deleted (`RelayKeepsLastUse`), and no
/// event leaving the outbox without landing (`NoEventLost`).
///
/// Mutants: 1 a share sized by the current members (two records with one member), 2 a full
/// window spills past it, 3 a record goes into the previous week's table, 4 a kept record drops
/// its events, 5 the restatement reads only the window's tables, not what this Mac relayed before.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn alab_journal_model() -> Model {
    aterm_spec::ty_model! {
        AlabJournal {
            const Buggy = 0;
            var weeks = 0;
            var p = 0; var c = 0; var n = 0;
            var pend = 0; var noted = 0; var told = 0; var fact = 0; var wk = 0;
            var lost = 0; var beyond = 0;

            action Note when (2 > noted) { noted = noted + 1; pend = pend + 1; }
            action FlushCurrent when ((pend > 0 || wk == 0) && n == 0
                && (if Buggy == 1 {2} else {1}) > c && ((3 > Buggy || Buggy > 3) || 2 > p)) {
                c = if Buggy == 3 {c} else {c + 1};
                p = if Buggy == 3 {p + 1} else {p};
                fact = if pend > 0 || (told == 1 && (fact > 0 || (if Buggy == 5 {0} else {1}) == 1)) {
                    if Buggy == 3 {if fact > 1 {fact} else {1}} else {if fact > 2 {fact} else {2}}
                } else {fact};
                told = if pend > 0 {1} else {told};
                pend = 0; wk = 1;
            }
            action FlushNext when ((pend > 0 || wk == 0) && (n > 0 || (if Buggy == 1 {2} else {1}) <= c)
                && (if Buggy == 1 {2} else {1}) > n) {
                n = n + 1;
                fact = if pend > 0 || (told == 1 && (fact > 0 || (if Buggy == 5 {0} else {1}) == 1)) {3} else {fact};
                told = if pend > 0 {1} else {told};
                pend = 0; wk = 1;
            }
            action FlushKept when (pend > 0 && (if Buggy == 1 {2} else {1}) <= n) {
                lost = if Buggy == 4 {1} else {lost};
                beyond = if Buggy == 2 {1} else {beyond};
                pend = if Buggy == 4 || Buggy == 2 {0} else {pend};
            }
            action Tick when (3 > weeks) {
                weeks = weeks + 1; p = c; c = n; n = 0;
                fact = if fact > 1 {fact - 1} else {0}; wk = 0;
            }

            invariant FixedShares: p <= 1 && c <= 1 && n <= 1;
            invariant SpillWithinWindow: beyond == 0;
            invariant PresenceInWindow: wk == 0 || c + n > 0;
            invariant RelayKeepsLastUse: told == 0 || wk == 0 || fact > 0;
            invariant NoEventLost: lost == 0;
        }
    }
}
