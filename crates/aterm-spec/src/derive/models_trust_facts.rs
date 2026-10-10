// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The whole-tree Trust lane's facts and the one judge that reads them.

use super::*;

/// FACTS, NOT VERDICTS (2026-09-29, Trust P0): the writer/reader contract of the
/// whole-tree Trust lane's records.
///
/// The WRITER is a whole-tree run (`tools/trust-gate-all.sh`, its end being
/// aterm-release `trust_facts::conclude`): it files what it MEASURED under a key over
/// the tree less its floors (`tools/trust-gate-ratchet.tsv` and the memory record),
/// the prover (the content of its whole toolchain directory) and the lane. The READER
/// is the cutter's assembly (`trust_facts::trust_report`, behind the cut, `ship
/// check` and `ship release`): it opens the record the key of the
/// commit it judges names, and judges it against THAT commit's floors — held, when the
/// facts were measured on another commit, to the merge contract's up-only rule against
/// the floors they were measured with (`trust_facts::floors_lowered`, review
/// 2026-09-29).
///
/// NO FLOOR EXCUSES (2026-09-30, the owner's ruling: no report-only path). The judge
/// holds EVERY floor, whichever prover recorded it (the lane's old
/// BELOW-FLOOR-NEW-PROVER passed a new prover's shortfall), and a shortfall the run's
/// lapsed wall-clock budgets could cover is UNDECIDED, never a pass (its old
/// BELOW-FLOOR-TIMED-OUT passed it). A floor goes down on a reused record only with a
/// new reason in its row's fifth column (`why`): a re-base of another prover's floor
/// onto the measuring one is no exception. What the clock left undecided is resolved
/// by MEASURING AGAIN on the same key (`--remeasure`, `trust_facts::remeasured`): the
/// new facts replace the old in the one record of that key, and never come from
/// another.
///
/// Environment: the tree's other files change (`EditSource`; what a run measures moves
/// with them — the count here IS `src`), the floors move (`RaiseFloor`, `LowerFloor`,
/// a same-prover lowering with no reason), a floor's prover column moves
/// (`RelabelFloor` names another prover; `RebaseFloor` re-bases another prover's floor
/// onto the measuring one, lower, with no reason, and `RebaseFloorWithReason` with
/// one), and the prover changes (`SwapProver`: another build, whose trustc may name the
/// same commit). `Run` files the facts, with the floor row of the commit it ran on;
/// `RunUnderLoad` is a run whose wall-clock budgets lapsed and cost it one proof;
/// `Remeasure` measures the stored record's library again, on its key; `Assemble`
/// judges. The store holds the latest record; `verdict` is the reader's last answer —
/// 1 VERIFIED, 2 refused, 0 not asked since the last change.
///
/// The laws: a VERIFIED answer is judged from facts measured on THIS tree (floors
/// aside) with THIS prover, against THESE floors, every one held; a change to the
/// floors alone finds the facts again (the reuse this exists for: 0.100.0 paid a second
/// four-hour run for one row's total); reused facts never verify floors that went down
/// without a new reason, or were re-labelled away from the measuring prover; and a
/// record's measurement is always of the record's own key.
///
/// `Buggy = 1` adds the readers and writers the contract rules out, each caught by its
/// own law: one keyed without the source (facts carried to code they never measured),
/// one keyed on the trustc commit instead of the prover's content (two builds of one
/// commit read as one prover), one that takes a VERDICT stored at the run's floors (the
/// pre-P0 `trust-tree-<tree>` receipt's `verdict PASS`), one keyed on the whole tree,
/// floors included (the pre-P0 key: a floors-only commit needs a new run), one that
/// judges reused facts against whatever floors the commit holds (P0 before its review),
/// one that does not hold another prover's floor (the lane's BELOW-FLOOR-NEW-PROVER),
/// one that passes a shortfall the lapses cover (BELOW-FLOOR-TIMED-OUT), one that
/// admits a re-base lowering with no reason (P0 after its review), and a re-measure
/// that folds a measurement of another key into the record.
///
/// Tier-1: crates/aterm-release/tests/it/trust_facts_model.rs drives the real key,
/// filing, re-measure and assembly against real git, real prover directories and the
/// real store, checks every transition here, and replays the retired readers as caught
/// negative controls.
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

            // THE WRITER: what the run measured, the floor row it ran with, and (for
            // the stored-verdict reader alone) the verdict the run's own floors gave.
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
            // THE RE-MEASURE: the stored record of THIS key, its library measured
            // again — the record's key and measured floor row stay.
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

            // THE READER: the record this tree's key names, judged against these
            // floors, every one held; they go down only with a new reason and are
            // re-labelled only onto the measuring prover.
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
            // Buggy: reused facts judged against whatever floors the commit holds
            // — the up-only rule never applied (P0 before its review).
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
            // Buggy: another prover's floor stated, not held (the lane's
            // BELOW-FLOOR-NEW-PROVER).
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
            // Buggy: a shortfall the lapses cover passed (BELOW-FLOOR-TIMED-OUT).
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
            // Buggy: a re-base onto the measuring prover admitted lower with no
            // reason (P0 after its review).
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
