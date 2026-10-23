// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The whole-tree Trust lane's facts and the one judge that reads them.

use aterm_spec::derive::*;

/// Facts, not verdicts: the writer/reader contract of the whole-tree Trust lane's records.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn trust_facts_reuse_model() -> Model {
    aterm_spec::ty_model! {
        TrustFactsReuse {
            const Buggy = 0;
            const MaxSrc = 2;
            const MaxFloor = 2;
            var src = 0;
            var floor = 0;
            var other = 0;
            var why = 0;
            var prv = 0;
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
            action AssembleSansSource when (Buggy == 1 && asked == 0) {
                asked = 1;
                found = if filed == 1 && f_prv == prv { 1 } else { 0 };
                verdict = if filed == 1 && f_prv == prv && floor <= f_proved { 1 } else { 2 };
                used_src = if filed == 1 && f_prv == prv { m_src } else { 0 };
                used_prv = if filed == 1 && f_prv == prv { m_prv } else { 0 };
                used_proved = if filed == 1 && f_prv == prv { f_proved } else { 0 };
            }
            action AssembleByTrustcCommit when (Buggy == 1 && asked == 0) {
                asked = 1;
                found = if filed == 1 && f_src == src { 1 } else { 0 };
                verdict = if filed == 1 && f_src == src && floor <= f_proved { 1 } else { 2 };
                used_src = if filed == 1 && f_src == src { m_src } else { 0 };
                used_prv = if filed == 1 && f_src == src { m_prv } else { 0 };
                used_proved = if filed == 1 && f_src == src { f_proved } else { 0 };
            }
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
