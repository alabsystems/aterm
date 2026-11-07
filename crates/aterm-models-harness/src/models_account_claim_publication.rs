// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A retained account candidate survives failed/lost writes and retirement.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_claim_publication_model() -> Model {
    aterm_spec::ty_model! {
        AccountClaimPublication {
            const Buggy = 0;
            var chosen = 0; var pending = 0; var disk = 0; var committed = 0;
            var retired = 0; var acknowledged = 0; var admitted = 0; var live = 0;
            var retired_unwritten = 0;
            var terminal = 0;
            var rival = 0; var other = 0;
            action ReserveA when (chosen == 0) {
                chosen = 1; pending = 1;
                committed = if Buggy == 1 {1} else {0};
            }
            action ReserveB when (chosen == 0) {
                chosen = 2; pending = 2;
                committed = if Buggy == 1 {2} else {0};
            }
            action Persist when (pending > 0 && disk == 0 && retired == 0) {
                disk = pending;
                pending = if Buggy == 2 {0} else {pending};
            }
            action CloseOnDisk when (pending > 0 && disk > 0 && terminal == 0) { terminal = 1; }
            action Retire when (pending > 0 && retired == 0) {
                retired = 1;
                retired_unwritten = if disk == 0 {1} else {0};
                pending = if Buggy == 4 {0} else {pending};
            }
            action Rival when (chosen > 0 && rival == 0) {
                rival = 1; other = if Buggy == 3 {1} else {0};
            }
            action Confirm when (pending > 0) {
                disk = if retired == 0 || disk > 0 || Buggy == 10 {pending} else {disk};
                acknowledged = if retired == 0 || disk > 0 || Buggy == 10 {1} else {0};
                committed = if retired == 1 && disk == 0 && Buggy <= 9 || Buggy == 5 && retired == 1 {0} else {pending};
                pending = if retired == 1 && disk == 0 && Buggy <= 9 || Buggy == 5 && retired == 1 {pending} else {0};
                admitted = if terminal == 1 {if Buggy == 9 {1} else {0}} else {
                    if retired == 0 {if Buggy == 6 {0} else {1}} else {if Buggy == 7 {1} else {0}}
                };
                live = if retired == 0 {1} else {if Buggy == 8 {1} else {0}};
                terminal = if retired == 1 && (disk > 0 || Buggy == 10) {1} else {terminal};
            }
            invariant NoEarlyClaim: committed == 0 || committed == disk;
            invariant RetainedCandidate: chosen == 0 || pending == chosen || committed == chosen;
            invariant NoRival: other == 0;
            invariant AcknowledgedClaim: acknowledged == 0 || committed == chosen;
            invariant ValidAdmission: acknowledged == 0 || retired == 1 || terminal == 1 || admitted == 1;
            invariant NoTerminalAdmission: terminal == 0 || admitted == 0;
            invariant NoRetiredAdmission: retired == 0 || admitted == 0;
            invariant NoRetiredAuthority: retired == 0 || live == 0;
            invariant NoFreshRetiredClaim: retired_unwritten == 0 || disk == 0;
            bound Finite: 0 <= chosen && chosen <= 2 && 0 <= pending && pending <= 2
                && 0 <= disk && disk <= 2 && 0 <= committed && committed <= 2
                && 0 <= retired && retired <= 1 && 0 <= acknowledged && acknowledged <= 1
                && 0 <= admitted && admitted <= 1 && 0 <= live && live <= 1
                && 0 <= terminal && terminal <= 1
                && 0 <= retired_unwritten && retired_unwritten <= 1
                && 0 <= rival && rival <= 1 && 0 <= other && other <= 1;
        }
    }
}

/// An account child closes without an effect only with live cohort evidence.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_noeffect_closure_model() -> Model {
    aterm_spec::ty_model! {
        AccountNoEffectClosure {
            const Buggy = 0;
            var quiet = 0; var checked = 0; var closed = 0;
            action CohortGone when (quiet == 0) { quiet = 1; checked = 0; }
            action Close when (closed == 0) {
                checked = 1;
                closed = if Buggy == 1 {1} else {if Buggy == 2 {0} else {quiet}};
            }
            invariant NoUnprovenClosure: closed == 0 || quiet == 1;
            invariant ReadyCloses: checked == 0 || quiet == 0 || closed == 1;
            bound Finite: 0 <= quiet && quiet <= 1 && 0 <= checked && checked <= 1 && 0 <= closed && closed <= 1;
        }
    }
}
