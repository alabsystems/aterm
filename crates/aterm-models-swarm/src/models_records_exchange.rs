// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Formal model of the exchange that makes a records file kept whole a layout where the file
//! system has no atomic exchange of two names (NTFS: `aterm-secrets`' `records_storage` on
//! Windows, ATERM-KI-1116-52817): three renames that never replace, through a swap name, and the
//! next writer carrying a cut-short exchange on from where the names stand.

use aterm_spec::derive::*;

/// `SwarmRecordsExchange`: one records entry past its preparation (`records_storage`'s
/// `Prepared`): the intent names the original records file, which stands whole at the entry, and
/// the prepared layout folder (its data a copy of the original, its header the intent) stands at
/// the preserved name `.aterm-records-<nonce>`. On Unix one exchange swaps the two names; here
/// it is three renames, none of which replaces an entry: `Park` (the folder takes the swap name,
/// `.aterm-records-<nonce>.swap`), `Move` (the original takes the preserved name, where it is
/// kept as the layout's predecessor) and `Place` (the folder takes the entry); `Pin` publishes the
/// pin beside the entry, the one point at which readers select the layout's data.
///
/// The names: `e` what stands at the entry, `f` at the preserved name, `t` at the swap name (0
/// nothing, 1 the original records file, 2 the prepared folder); `pin` 1 once the pin stands.
/// `run` is the writer under the records lock: 0 none, 1 the writer that prepared, 2 one started
/// after a cut (each takes the lock and carries on what it finds: `records_storage`'s
/// `recover_pending`); `Cut` ends a writer as a crash would, at any step, and `Restart` starts
/// another, at most twice. A writer's next step is a function of the names alone, so a writer
/// started after a cut takes the same step the cut writer would have. `Stuck` is a writer that
/// finds no step to take and refuses, leaving the entry unfinished: every records write is then
/// refused. `Read` is a reader with no lock (`records_storage::data_path`): `rd` what it got: 1
/// the original whole, 2 the layout's data under its pin, 3 a refusal, 4 no records at all.
///
/// The laws: the original stands at exactly one name and the folder at exactly one name at every
/// step (`CopiesKept`); a reader reads the original whole, the layout's data under its pin, or
/// is refused, and never takes an entry left empty mid-exchange for one that holds no records
/// (`ReadsWhole`); a pin stands only over the folder at the entry with the original kept at the
/// preserved name and nothing parked (`PinnedOverLayout`); and no writer refuses an exchange
/// that a cut left anywhere (`NeverStuck`), so the next writer always finishes it.
///
/// Mutants: 1 a writer started after a cut knows no swap name: it prepares a second folder at
/// the preserved name the parked folder left empty (`Reprepare`, which the next rename then
/// refuses), and refuses an entry whose original was moved; 2 a reader takes a missing entry for
/// no records whatever the intent names; 3 the pin is published once the original is kept, with
/// the folder still parked.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn records_exchange_model() -> Model {
    aterm_spec::ty_model! {
        SwarmRecordsExchange {
            const Buggy = 0;

            var e = 1;
            var f = 2;
            var t = 0;
            var pin = 0;
            var run = 1;
            var restarts = 0;
            var stuck = 0;
            var rd = 0;

            action Park when (run > 0 && pin == 0 && e == 1 && f == 2 && t == 0) {
                f = 0;
                t = 2;
            }
            // Mutant 1: a writer started after a cut carries on no parked exchange.
            action Move when (run > 0 && pin == 0 && e == 1 && f == 0 && t == 2 &&
                (run == 1 || Buggy == 0 || Buggy == 2 || Buggy == 3)) {
                e = 0;
                f = 1;
            }
            action Reprepare when (Buggy == 1 && run == 2 && pin == 0 && e == 1 && f == 0 &&
                t == 2) {
                f = 2;
            }
            action Place when (run > 0 && pin == 0 && e == 0 && f == 1 && t == 2 &&
                (run == 1 || Buggy == 0 || Buggy == 2 || Buggy == 3)) {
                e = 2;
                t = 0;
            }
            // Mutant 3 pins once the original is kept, the folder still parked.
            action Pin when (run > 0 && pin == 0 && ((e == 2 && f == 1 && t == 0) ||
                (Buggy == 3 && e == 0 && f == 1 && t == 2))) {
                pin = 1;
                run = 0;
            }
            // A writer with no step to take: none of the guards above holds.
            action Stuck when (run > 0 && pin == 0 && (if (e == 1 && f == 2 && t == 0) ||
                (e == 1 && f == 0 && t == 2 &&
                    (run == 1 || Buggy == 0 || Buggy == 2 || Buggy == 3)) ||
                (Buggy == 1 && run == 2 && e == 1 && f == 0 && t == 2) ||
                (e == 0 && f == 1 && t == 2 &&
                    (run == 1 || Buggy == 0 || Buggy == 2 || Buggy == 3)) ||
                (e == 2 && f == 1 && t == 0) ||
                (Buggy == 3 && e == 0 && f == 1 && t == 2) { 0 } else { 1 }) == 1) {
                stuck = 1;
                run = 0;
            }
            action Cut when (run > 0) {
                run = 0;
            }
            action Restart when (run == 0 && pin == 0 && stuck == 0 && restarts <= 1) {
                run = 2;
                restarts = restarts + 1;
            }
            // Mutant 2 reads a missing entry as no records.
            action Read {
                rd = if pin == 1 {
                    if e == 2 { 2 } else { 3 }
                } else {
                    if e == 1 { 1 } else { if e == 0 && Buggy == 2 { 4 } else { 3 } }
                };
            }

            invariant CopiesKept:
                (if e == 1 { 1 } else { 0 }) + (if f == 1 { 1 } else { 0 }) == 1 &&
                (if e == 2 { 1 } else { 0 }) + (if f == 2 { 1 } else { 0 }) +
                    (if t == 2 { 1 } else { 0 }) == 1;
            invariant ReadsWhole: rd <= 3;
            invariant PinnedOverLayout: pin == 0 || (e == 2 && f == 1 && t == 0);
            invariant NeverStuck: stuck == 0;
            bound RecordsExchangeBox:
                e <= 2 && f <= 2 && t <= 2 && pin <= 1 && run <= 2 && restarts <= 2 &&
                stuck <= 1 && rd <= 4;
        }
    }
}
