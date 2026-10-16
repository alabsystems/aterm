// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The whole-tree Trust lane's facts and the one judge that reads them.

use super::*;

/// Facts, not verdicts: the writer/reader contract of the whole-tree Trust lane's records.
///
/// The writer is a whole-tree run (`ship trust run --whole-tree`, ending in aterm-release
/// `trust_facts::conclude`): it files what it measured under a key over the tree less its
/// floors (aterm-release's `trust/floors.tsv` and memory record), the prover (the content
/// of its whole toolchain directory) and the lane. The reader is the cutter's assembly
/// (`trust_facts::trust_report`, behind the cut, `ship check` and `ship release`): it opens
/// the record its commit's key names and judges it against that commit's floors; facts
/// measured on another commit are also held to the floors' up-only rule against the floors
/// they were measured with (`trust_facts::floors_lowered`).
///
/// The judge holds every floor, whichever prover recorded it, and a shortfall the run's
/// lapsed wall-clock budgets could cover is undecided, never a pass. A floor goes down on
/// a reused record only with a new reason in its row's fifth column (`why`), re-bases
/// included. What the clock left undecided is resolved by measuring again on the same key
/// (`--remeasure`, `trust_facts::remeasured`); the new facts replace the old in that key's
/// record and never come from another.
///
/// Environment: `EditSource` (`src` is what a run measures), `RaiseFloor`, `LowerFloor` (no
/// reason), `RelabelFloor` (the prover column names another prover), `RebaseFloor` /
/// `RebaseFloorWithReason` (another prover's floor re-based lower onto the measuring one),
/// and `SwapProver` (another build, whose trustc may name the same commit). `Run` files the
/// facts; `RunUnderLoad` lapsed and is one proof short; `Remeasure` measures the stored
/// record again on its key; `Assemble` judges. `verdict` is the reader's last answer:
/// 1 VERIFIED, 2 refused, 0 not asked since the last change.
///
/// Laws: VERIFIED is judged from facts measured on this tree (floors aside) with this
/// prover, against these floors, every one held; a floors-only change finds the facts
/// again (no second multi-hour run); reused facts never verify floors lowered without a
/// new reason or re-labelled away from the measuring prover; a record's measurement is of
/// its own key.
///
/// `Buggy = 1` adds the readers and writers the contract rules out, each caught by its own
/// law: keyed without the source; keyed on the trustc commit instead of the prover's
/// content; taking a verdict stored at the run's floors; keyed on the whole tree, floors
/// included; judging reused facts against whatever floors the commit holds; not holding
/// another prover's floor; passing a shortfall the lapses cover; admitting a re-base
/// lowering with no reason; and a re-measure folded into another key's record.
///
/// Tier-1: `crates/aterm-release/tests/it/trust_facts_model.rs` drives the real key,
/// filing, re-measure and assembly against real git, prover directories and store, with
/// these readers as negative controls.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn trust_facts_reuse_model() -> Model {
    crate::ty_model! {
        TrustFactsReuse {
            const Buggy = 0;
            const MaxSrc = 2;
            const MaxFloor = 2;
            // the tree less its floors, the floor, its prover column (1: another
            // prover's), whether the row gives a reason, the prover
            var src = 0;
            var floor = 0;
            var other = 0;
            var why = 0;
            var prv = 0;
            // the store's record: its key, the floor row of the commit it was run
            // on, what it measured (and whether lapses cost it a proof), the key that
            // measurement was taken under, and the verdict the run's own floors gave
            var filed = 0;
            var f_src = 0;
            var f_prv = 0;
            var f_floor = 0;
            var f_other = 0;
            var f_why = 0;
            var f_proved = 0;
            var f_lapsed = 0;
            var m_src = 0;
            var m_prv = 0;
            var f_pass = 0;
            // the reader's last answer, and the facts it was judged from
            var asked = 0;
            var found = 0;
            var verdict = 0;
            var used_src = 0;
            var used_prv = 0;
            var used_proved = 0;

            action EditSource when (MaxSrc > src) {
                src = src + 1;
                asked = 0;
                found = 0;
                verdict = 0;
                used_src = 0;
                used_prv = 0;
                used_proved = 0;
            }
            action RaiseFloor when (MaxFloor > floor) {
                floor = floor + 1;
                why = 0;
                asked = 0;
                found = 0;
                verdict = 0;
                used_src = 0;
                used_prv = 0;
                used_proved = 0;
            }
            // A floor lowered by hand under the same prover, with no new reason.
            action LowerFloor when (floor > 0) {
                floor = floor - 1;
                why = 0;
                asked = 0;
                found = 0;
                verdict = 0;
                used_src = 0;
                used_prv = 0;
                used_proved = 0;
            }
            // The prover column rewritten to name another prover.
            action RelabelFloor when (other == 0) {
                other = 1;
                why = 0;
                asked = 0;
                found = 0;
                verdict = 0;
                used_src = 0;
                used_prv = 0;
                used_proved = 0;
            }
            // Another prover's floor re-based onto the measuring prover, lower, with
            // no reason.
            action RebaseFloor when (other == 1 && floor > 0) {
                other = 0;
                floor = floor - 1;
                why = 0;
                asked = 0;
                found = 0;
                verdict = 0;
                used_src = 0;
                used_prv = 0;
                used_proved = 0;
            }
            // …and with its reason in the fifth column (`ship trust judge
            // --update-ratchet --reason`).
            action RebaseFloorWithReason when (other == 1 && floor > 0) {
                other = 0;
                floor = floor - 1;
                why = 1;
                asked = 0;
                found = 0;
                verdict = 0;
                used_src = 0;
                used_prv = 0;
                used_proved = 0;
            }
            action SwapProver {
                prv = 1 - prv;
                asked = 0;
                found = 0;
                verdict = 0;
                used_src = 0;
                used_prv = 0;
                used_proved = 0;
            }

            // The writer: what the run measured, the floor row it ran with, and (for the
            // stored-verdict reader alone) the verdict the run's own floors gave.
            action Run {
                filed = 1;
                f_src = src;
                f_prv = prv;
                f_floor = floor;
                f_other = other;
                f_why = why;
                f_proved = src;
                f_lapsed = 0;
                m_src = src;
                m_prv = prv;
                f_pass = if floor <= src { 1 } else { 0 };
                asked = 0;
                found = 0;
                verdict = 0;
                used_src = 0;
                used_prv = 0;
                used_proved = 0;
            }
            // A run whose wall-clock budgets lapsed: one proof short, and the lapse
            // on record.
            action RunUnderLoad when (src > 0) {
                filed = 1;
                f_src = src;
                f_prv = prv;
                f_floor = floor;
                f_other = other;
                f_why = why;
                f_proved = src - 1;
                f_lapsed = 1;
                m_src = src;
                m_prv = prv;
                f_pass = if floor + 1 <= src { 1 } else { 0 };
                asked = 0;
                found = 0;
                verdict = 0;
                used_src = 0;
                used_prv = 0;
                used_proved = 0;
            }
            // The re-measure: the stored record of this key, measured again; its key and
            // floor row stay.
            action Remeasure when (filed == 1 && f_src == src && f_prv == prv) {
                f_proved = src;
                f_lapsed = 0;
                m_src = src;
                m_prv = prv;
                asked = 0;
                found = 0;
                verdict = 0;
                used_src = 0;
                used_prv = 0;
                used_proved = 0;
            }
            // Buggy: a re-measure folded into whatever record the store holds.
            action RemeasureAcrossKeys when (Buggy == 1 && filed == 1) {
                f_proved = src;
                f_lapsed = 0;
                m_src = src;
                m_prv = prv;
                asked = 0;
                found = 0;
                verdict = 0;
                used_src = 0;
                used_prv = 0;
                used_proved = 0;
            }

            // The reader: the record this tree's key names, judged against these floors,
            // every one held.
            action Assemble when (asked == 0) {
                asked = 1;
                found = if filed == 1 && f_src == src && f_prv == prv { 1 } else { 0 };
                verdict = if filed == 1 && f_src == src && f_prv == prv
                    && floor <= f_proved
                    && (other == 0 || f_other == 1)
                    && (f_floor <= floor || (why == 1 && f_why == 0))
                {
                    1
                } else {
                    2
                };
                used_src = if filed == 1 && f_src == src && f_prv == prv { m_src } else { 0 };
                used_prv = if filed == 1 && f_src == src && f_prv == prv { m_prv } else { 0 };
                used_proved = if filed == 1 && f_src == src && f_prv == prv { f_proved } else { 0 };
            }
            // Buggy: keyed without the source.
            action AssembleSansSource when (Buggy == 1 && asked == 0) {
                asked = 1;
                found = if filed == 1 && f_prv == prv { 1 } else { 0 };
                verdict = if filed == 1 && f_prv == prv && floor <= f_proved { 1 } else { 2 };
                used_src = if filed == 1 && f_prv == prv { m_src } else { 0 };
                used_prv = if filed == 1 && f_prv == prv { m_prv } else { 0 };
                used_proved = if filed == 1 && f_prv == prv { f_proved } else { 0 };
            }
            // Buggy: keyed on the trustc commit, which both builds name.
            action AssembleByTrustcCommit when (Buggy == 1 && asked == 0) {
                asked = 1;
                found = if filed == 1 && f_src == src { 1 } else { 0 };
                verdict = if filed == 1 && f_src == src && floor <= f_proved { 1 } else { 2 };
                used_src = if filed == 1 && f_src == src { m_src } else { 0 };
                used_prv = if filed == 1 && f_src == src { m_prv } else { 0 };
                used_proved = if filed == 1 && f_src == src { f_proved } else { 0 };
            }
            // Buggy: the verdict stored at the run's floors, taken as it stands.
            action AssembleStoredVerdict when (Buggy == 1 && asked == 0) {
                asked = 1;
                found = if filed == 1 && f_src == src && f_prv == prv { 1 } else { 0 };
                verdict = if filed == 1 && f_src == src && f_prv == prv && f_pass == 1 {
                    1
                } else {
                    2
                };
                used_src = if filed == 1 && f_src == src && f_prv == prv { m_src } else { 0 };
                used_prv = if filed == 1 && f_src == src && f_prv == prv { m_prv } else { 0 };
                used_proved = if filed == 1 && f_src == src && f_prv == prv { f_proved } else { 0 };
            }
            // Buggy: keyed on the whole tree, the floors included.
            action AssembleWholeTree when (Buggy == 1 && asked == 0) {
                asked = 1;
                found = if filed == 1 && f_src == src && f_prv == prv && f_floor == floor
                    && f_other == other && f_why == why
                {
                    1
                } else {
                    0
                };
                verdict = if filed == 1 && f_src == src && f_prv == prv && f_floor == floor
                    && f_other == other && f_why == why && floor <= f_proved
                {
                    1
                } else {
                    2
                };
                used_src = if filed == 1 && f_src == src && f_prv == prv && f_floor == floor
                    && f_other == other && f_why == why
                {
                    m_src
                } else {
                    0
                };
                used_prv = if filed == 1 && f_src == src && f_prv == prv && f_floor == floor
                    && f_other == other && f_why == why
                {
                    m_prv
                } else {
                    0
                };
                used_proved = if filed == 1 && f_src == src && f_prv == prv && f_floor == floor
                    && f_other == other && f_why == why
                {
                    f_proved
                } else {
                    0
                };
            }
            // Buggy: reused facts judged against whatever floors the commit holds (no
            // up-only rule).
            action AssembleUncheckedReuse when (Buggy == 1 && asked == 0) {
                asked = 1;
                found = if filed == 1 && f_src == src && f_prv == prv { 1 } else { 0 };
                verdict = if filed == 1 && f_src == src && f_prv == prv && floor <= f_proved {
                    1
                } else {
                    2
                };
                used_src = if filed == 1 && f_src == src && f_prv == prv { m_src } else { 0 };
                used_prv = if filed == 1 && f_src == src && f_prv == prv { m_prv } else { 0 };
                used_proved = if filed == 1 && f_src == src && f_prv == prv { f_proved } else { 0 };
            }
            // Buggy: another prover's floor stated, not held.
            action AssembleNewProverExcused when (Buggy == 1 && asked == 0) {
                asked = 1;
                found = if filed == 1 && f_src == src && f_prv == prv { 1 } else { 0 };
                verdict = if filed == 1 && f_src == src && f_prv == prv
                    && (floor <= f_proved || other == 1)
                    && (other == 0 || f_other == 1)
                    && (f_floor <= floor || (why == 1 && f_why == 0))
                {
                    1
                } else {
                    2
                };
                used_src = if filed == 1 && f_src == src && f_prv == prv { m_src } else { 0 };
                used_prv = if filed == 1 && f_src == src && f_prv == prv { m_prv } else { 0 };
                used_proved = if filed == 1 && f_src == src && f_prv == prv { f_proved } else { 0 };
            }
            // Buggy: a shortfall the lapses cover passed.
            action AssembleLapseExcused when (Buggy == 1 && asked == 0) {
                asked = 1;
                found = if filed == 1 && f_src == src && f_prv == prv { 1 } else { 0 };
                verdict = if filed == 1 && f_src == src && f_prv == prv
                    && floor <= f_proved + f_lapsed
                    && (other == 0 || f_other == 1)
                    && (f_floor <= floor || (why == 1 && f_why == 0))
                {
                    1
                } else {
                    2
                };
                used_src = if filed == 1 && f_src == src && f_prv == prv { m_src } else { 0 };
                used_prv = if filed == 1 && f_src == src && f_prv == prv { m_prv } else { 0 };
                used_proved = if filed == 1 && f_src == src && f_prv == prv { f_proved } else { 0 };
            }
            // Buggy: a re-base onto the measuring prover admitted lower with no reason.
            action AssembleRebaseUnreasoned when (Buggy == 1 && asked == 0) {
                asked = 1;
                found = if filed == 1 && f_src == src && f_prv == prv { 1 } else { 0 };
                verdict = if filed == 1 && f_src == src && f_prv == prv
                    && floor <= f_proved
                    && (other == 0 || f_other == 1)
                    && (f_floor <= floor || (f_other == 1 && other == 0) || (why == 1 && f_why == 0))
                {
                    1
                } else {
                    2
                };
                used_src = if filed == 1 && f_src == src && f_prv == prv { m_src } else { 0 };
                used_prv = if filed == 1 && f_src == src && f_prv == prv { m_prv } else { 0 };
                used_proved = if filed == 1 && f_src == src && f_prv == prv { f_proved } else { 0 };
            }

            invariant VerifiedFromThisTree: verdict == 0 || verdict == 2 || used_src == src;
            invariant VerifiedFromThisProver: verdict == 0 || verdict == 2 || used_prv == prv;
            invariant VerifiedAgainstTheseFloors:
                verdict == 0 || verdict == 2 || floor <= used_proved;
            invariant FloorsAloneReuseTheFacts:
                asked == 0 || filed == 0 || found == 1 ||
                src > f_src || f_src > src || prv > f_prv || f_prv > prv;
            invariant NoFloorLoweredOnReuse:
                verdict == 0 || verdict == 2 || f_floor <= floor || (why == 1 && f_why == 0);
            invariant NoFloorRelabelledOnReuse:
                verdict == 0 || verdict == 2 || other == 0 || f_other == 1;
            invariant MeasuredUnderItsOwnKey:
                filed == 0 || (m_src == f_src && m_prv == f_prv);
        }
    }
}
