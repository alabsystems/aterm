// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CRASH-RESTORE JOURNAL (the crash-restore plan, §2.4, §2.7 and §7.1):
//! one session's shared-memory region across its writer, the window W that
//! journals its engine, and the window W' that restores the session after W
//! dies without a word (a SIGKILL, a jetsam kill, a Force Quit, a crash).
//!
//! W writes three things into the region under single-writer commit words:
//! a KEYFRAME (the engine's checkpoint, two slots written alternately), the
//! raw engine input since that keyframe (the TAIL, a ring bounded by a floor
//! word and a commit word) and a history ring. A process that dies loses no
//! store it already made to shared memory, so the only hazard is a death in
//! the middle of an update; every protocol publishes the data before the
//! commit word that admits it. W' maps the region read-only after the keeper
//! proves W dead, picks the newest valid keyframe, replays the tail past it
//! into a scratch engine, and adopts the result. A restore that dies (a
//! poisoned slice in the tail, or a poisoned keyframe) is retried at most
//! once without the tail and then falls back to the blank META rung: the
//! keeper's `attempts` counter is the brake.
//!
//! Bytes are abstracted to UNITS: one unit is one slice of PTY output, which
//! the gather thread reads (`Gather`), the parse thread processes (`Slice`;
//! `SliceClear` is a slice holding an ED3 or RIS) and then appends to the
//! tail (`Commit`, which moves the floor first when the ring is full: `Cap`
//! units fit).

use aterm_spec::derive::Model;

/// One session's region across its writer W and a restorer, bytes abstracted
/// to units (`aterm-restore`: `JournalWriter` on W's side, `recover` on W''s).
///
/// State. The stream: `g` units gathered, `p` processed by the engine, `t`
/// committed to the tail; `floor` (units below it may be overwritten).
/// The keyframe writer: `seq` (the next keyframe's sequence number),
/// `to_b` (the slot the next keyframe goes to), `writing` (a keyframe copy is
/// in progress). Each slot `a`/`b`: `ok` (its commit word admits it), `full`
/// (its bytes are a complete record), `seq`, `at` (the tail position it was
/// captured at: its `tail_from`), `state` (the units the engine had processed
/// then) and `gen` (the engine's clear generation then). The history:
/// `gen` (the engine's clear generation), `ring_gen` (the identity the history
/// ring was last reset to), `clear_at` (the unit that held the one clear).
/// The death and the restore: `live`, `poison` (some slice of the tail kills
/// a restorer that replays it), `kpoison` (the keyframe itself kills any
/// restorer), `attempts` (the keeper's count of restorers that died),
/// `full_tries`, `restored`, `r` (the units the restored engine reflects),
/// `rgen`, `exact`, `imported`, and the evidence flags `torn` (a restore
/// chose a slot whose bytes were not whole), `wrong` (it replayed tail bytes
/// that had been overwritten) and `resurrected` (it imported history from
/// before a clear the restored engine had seen).
///
/// Invariants. `NeverAhead`: a restore never reflects output the engine had
/// not processed. `ExactIsTrue`: a restore that says it is exact reflects
/// exactly what was gathered. `TornNeverChosen`, `NoWrongReplay`,
/// `ClearedNeverResurrected`, and `PoisonBounded`: a poisoned region costs at
/// most one full try.
///
/// `Buggy`, one defect each: 1 the commit word stored before the copy ·
/// 2 no floor before an overwrite · 3 `exact` ignores the gather counter ·
/// 4 the history join without the identity check · 5 no attempts brake ·
/// 6 a keyframe that does not rebase the tail (its `tail_from` is the other
/// slot's).
///
/// Tier-0 is `tests/derived_crash_restore.rs`; Tier-1
/// (`aterm-restore/src/conformance_region_tests.rs`) drives the real writer over a
/// store that dies at every primitive store and runs the real `recover`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn crash_restore_journal_model() -> Model {
    aterm_spec::ty_model! {
        CrashRestoreJournal {
            const Buggy = 0;
            const MaxP = 3;
            const Cap = 2;
            const MaxKf = 3;
            const MaxTries = 3;
            var g = 0;
            var p = 0;
            var t = 0;
            var floor = 0;
            var seq = 1;
            var to_b = 1;
            var writing = 0;
            var a_ok = 1;
            var a_full = 1;
            var a_seq = 0;
            var a_at = 0;
            var a_state = 0;
            var a_gen = 0;
            var b_ok = 0;
            var b_full = 0;
            var b_seq = 0;
            var b_at = 0;
            var b_state = 0;
            var b_gen = 0;
            var gen = 0;
            var ring_gen = 0;
            var clear_at = 0;
            var live = 1;
            var poison = 0;
            var kpoison = 0;
            var attempts = 0;
            var full_tries = 0;
            var restored = 0;
            var r = 0;
            var rgen = 0;
            var exact = 0;
            var imported = 0;
            var torn = 0;
            var wrong = 0;
            var resurrected = 0;

            action Gather when (live == 1 && MaxP > g) { g = g + 1; }
            action Slice when (live == 1 && g > p && p == t && writing == 0) { p = p + 1; }
            action SliceClear when (live == 1 && g > p && p == t && writing == 0 && clear_at == 0) {
                p = p + 1;
                gen = gen + 1;
                clear_at = p + 1;
            }
            action Commit when (live == 1 && p > t) {
                t = t + 1;
                floor = if Buggy == 2 { floor } else { if t + 1 > floor + Cap { t + 1 - Cap } else { floor } };
            }
            action RingReset when (live == 1 && gen > ring_gen) { ring_gen = gen; }
            action Poison when (live == 1 && poison == 0) { poison = 1; }
            action PoisonKf when (live == 1 && kpoison == 0) { kpoison = 1; }
            action BeginKf when (live == 1 && writing == 0 && p == t && MaxKf + 1 > seq) {
                writing = 1;
                a_ok = if to_b == 0 { if Buggy == 1 { 1 } else { 0 } } else { a_ok };
                a_full = if to_b == 0 { 0 } else { a_full };
                a_seq = if to_b == 0 && Buggy == 1 { seq } else { a_seq };
                b_ok = if to_b == 1 { if Buggy == 1 { 1 } else { 0 } } else { b_ok };
                b_full = if to_b == 1 { 0 } else { b_full };
                b_seq = if to_b == 1 && Buggy == 1 { seq } else { b_seq };
            }
            action FinishKf when (live == 1 && writing == 1) {
                writing = 0;
                seq = seq + 1;
                to_b = 1 - to_b;
                a_ok = if to_b == 0 { 1 } else { a_ok };
                a_full = if to_b == 0 { 1 } else { a_full };
                a_seq = if to_b == 0 { seq } else { a_seq };
                a_at = if to_b == 0 { if Buggy == 6 { b_at } else { t } } else { a_at };
                a_state = if to_b == 0 { t } else { a_state };
                a_gen = if to_b == 0 { gen } else { a_gen };
                b_ok = if to_b == 1 { 1 } else { b_ok };
                b_full = if to_b == 1 { 1 } else { b_full };
                b_seq = if to_b == 1 { seq } else { b_seq };
                b_at = if to_b == 1 { if Buggy == 6 { a_at } else { t } } else { b_at };
                b_state = if to_b == 1 { t } else { b_state };
                b_gen = if to_b == 1 { gen } else { b_gen };
            }
            action Die when (live == 1) { live = 0; }
            action RestoreFullA when (live == 0 && restored == 0 && a_ok == 1 && (b_ok == 0 || a_seq > b_seq)
                                      && (attempts == 0 || (Buggy == 5 && MaxTries > attempts))) {
                full_tries = full_tries + 1;
                attempts = if poison == 1 || kpoison == 1 { attempts + 1 } else { attempts };
                restored = if poison == 1 || kpoison == 1 { 0 } else { 1 };
                r = if a_at + 1 > floor { a_state + t - a_at } else { a_state };
                rgen = if a_at + 1 > floor && clear_at > a_state && t + 1 > clear_at { a_gen + 1 } else { a_gen };
                exact = if a_at + 1 > floor && (t == g || Buggy == 3) { 1 } else { 0 };
                torn = if a_full == 0 { 1 } else { torn };
                wrong = if a_at + 1 > floor && t > a_at + Cap { 1 } else { wrong };
            }
            action RestoreFullB when (live == 0 && restored == 0 && b_ok == 1 && (a_ok == 0 || b_seq > a_seq)
                                      && (attempts == 0 || (Buggy == 5 && MaxTries > attempts))) {
                full_tries = full_tries + 1;
                attempts = if poison == 1 || kpoison == 1 { attempts + 1 } else { attempts };
                restored = if poison == 1 || kpoison == 1 { 0 } else { 1 };
                r = if b_at + 1 > floor { b_state + t - b_at } else { b_state };
                rgen = if b_at + 1 > floor && clear_at > b_state && t + 1 > clear_at { b_gen + 1 } else { b_gen };
                exact = if b_at + 1 > floor && (t == g || Buggy == 3) { 1 } else { 0 };
                torn = if b_full == 0 { 1 } else { torn };
                wrong = if b_at + 1 > floor && t > b_at + Cap { 1 } else { wrong };
            }
            action RestoreScreenA when (live == 0 && restored == 0 && a_ok == 1 && (b_ok == 0 || a_seq > b_seq) && attempts == 1) {
                attempts = attempts + kpoison;
                restored = 1 - kpoison;
                r = a_state;
                rgen = a_gen;
                exact = 0;
                torn = if a_full == 0 { 1 } else { torn };
            }
            action RestoreScreenB when (live == 0 && restored == 0 && b_ok == 1 && (a_ok == 0 || b_seq > a_seq) && attempts == 1) {
                attempts = attempts + kpoison;
                restored = 1 - kpoison;
                r = b_state;
                rgen = b_gen;
                exact = 0;
                torn = if b_full == 0 { 1 } else { torn };
            }
            action RestoreMeta when (live == 0 && restored == 0 && (attempts > 1 || (a_ok == 0 && b_ok == 0))) {
                restored = 1;
                r = 0;
                exact = 0;
                imported = 1;
            }
            action Import when (restored == 1 && imported == 0) {
                imported = 1;
                resurrected = if (ring_gen == rgen || Buggy == 4) && rgen > ring_gen { 1 } else { 0 };
            }

            invariant NeverAhead: restored == 0 || p + 1 > r;
            invariant ExactIsTrue: restored == 0 || exact == 0 || (r == p && g == p);
            invariant TornNeverChosen: torn == 0;
            invariant NoWrongReplay: wrong == 0;
            invariant ClearedNeverResurrected: resurrected == 0;
            invariant PoisonBounded: poison + kpoison == 0 || full_tries <= 1;
        }
    }
}
