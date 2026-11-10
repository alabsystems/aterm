// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A later reader answers a refused restart prepare `lapsed`, read-only, only where no exec
/// grant was or can be issued under it, or where every process of the boot that could hold one
/// has ended. One prepare id and its one deadline on one machine.
/// The issuer: `journal` is the prepare's record in the custody its stamp names, absent (0),
/// prepared (1), possible execution (2), sealed with none (3), sealed after it (4) or a positive
/// no-grant (5); it only moves forward, and a seal or a no-grant is final. `bstamp` is a prepare
/// stamped with its machine's boot and `cstamp` one stamped with its journal's custody, each set
/// on its own (a machine with no journal record yet stamps no custody), and only an issuer that
/// journals admits either; `legacy` is an older issuer that grants without journaling, which
/// only a prepare with neither stamp can name. `live` is the issuing host epoch; a grant
/// (`granted`, its grantee `child`) needs possible execution recorded (or a legacy issuer) and a
/// deadline the delivery itself reads as ahead. After the forward child exits, a rollback
/// recipient (`rollback`, its delivery `rollback_granted`) can receive Execute under the same
/// journal record. The journal does not distinguish these recipients. `late` is that deadline passed on the boot's monotonic clock, which every process of
/// the boot shares; `boot` is a later boot of the machine, which ends every process of the old
/// one; `moved` is the custody set aside, after which the journal reads absent.
/// The reader reads `clock` (before the deadline 1, past it 2), then the journal (`seen` is the
/// record's state plus one, or 7 for an absence read from a custody that replaced the stamped
/// one; `stood` is a no-grant standing in the stamped custody when it read), and answers: from
/// the journal (`answer` 1), from another boot (2) or NOEXEC (3). `fresh` is the harness's fresh
/// plan on a lapsed answer.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_historical_closure_model() -> Model {
    aterm_spec::ty_model! {
        NativeHistoricalClosure {
            const Buggy = 0;
            var journal = 0; var bstamp = 0; var cstamp = 0; var legacy = 0; var moved = 0;
            var live = 1; var late = 0; var boot = 0; var granted = 0; var child = 0;
            var rollback_granted = 0; var rollback = 0;
            var clock = 0; var seen = 0; var stood = 0; var answer = 0; var fresh = 0;
            // The prepare's shape, fixed before anything records or reads it.
            action StampBoot when (bstamp == 0 && legacy == 0 && journal == 0 && granted == 0
                && clock == 0 && seen == 0) { bstamp = 1; }
            action StampCustody when (cstamp == 0 && legacy == 0 && journal == 0 && granted == 0
                && clock == 0 && seen == 0) { cstamp = 1; }
            action Legacy when (bstamp == 0 && cstamp == 0 && legacy == 0 && journal == 0
                && granted == 0 && clock == 0 && seen == 0) { legacy = 1; }
            action Record when (live == 1 && legacy == 0 && journal == 0) { journal = 1; }
            action May when (live == 1 && journal == 1) { journal = 2; }
            action Deliver when (live == 1 && late == 0 && (journal == 2 || legacy == 1)) {
                granted = 1; child = 1;
            }
            action DeliverRollback when (live == 1 && late == 0 && journal == 2
                && granted == 1 && child == 0 && rollback_granted == 0) {
                rollback_granted = 1; rollback = 1;
            }
            action SealOpen when (live == 1 && journal == 1) { journal = 3; }
            action SealMay when (live == 1 && journal == 2) { journal = 4; }
            action NoExec when (live == 1 && journal == 1) { journal = 5; }
            action Expire when (late == 0) { late = 1; }
            action Crash when (live == 1) { live = 0; }
            action ChildExits when (child == 1) { child = 0; }
            action RollbackExits when (rollback == 1) { rollback = 0; }
            action Reboot when (boot == 0) { boot = 1; live = 0; child = 0; rollback = 0; }
            action Move when (moved == 0) { moved = 1; }
            action ReadClock when (clock == 0) { clock = if late == 1 {2} else {1}; }
            action ReadJournal when (seen == 0 && (1 <= clock || Buggy == 1)) {
                seen = if moved == 1 {7} else {journal + 1};
                stood = if journal == 5 {1} else {0};
            }
            // A seal with no possible execution at any clock; a prepared record, or an absence
            // from the custody the custody stamp names, read after the deadline.
            action AnswerLapsed when (answer == 0 && (seen == 4
                || ((seen == 2 || (Buggy == 2 && seen == 3)) && (clock == 2 || Buggy == 3))
                || (Buggy == 8 && seen == 3 && clock == 2 && child == 0)
                || ((seen == 1 || (Buggy == 5 && seen == 7)) && (cstamp == 1 || Buggy == 4)
                    && clock == 2))) {
                answer = 1;
            }
            // A boot stamp naming another boot: any record but a no-grant, or an absence from
            // the custody the custody stamp names.
            action AnswerBoot when (answer == 0 && bstamp == 1 && (boot == 1 || Buggy == 6)
                && ((2 <= seen && seen <= 5) || (seen == 1 && cstamp == 1)
                    || (Buggy == 7 && seen == 7))) {
                answer = 2;
            }
            action AnswerNoExec when (answer == 0 && seen == 6) { answer = 3; }
            action Fresh when (fresh == 0 && (answer == 1 || answer == 2)) { fresh = 1; }
            invariant NoDoubleExec: fresh == 0 || (child == 0 && rollback == 0);
            invariant LapsedNeverGranted: answer == 0 || 2 <= answer || granted == 0;
            invariant BootOnlyAfterReboot: answer <= 1 || answer == 3 || boot == 1;
            invariant NoRevivalOverNoExec: stood == 0 || answer == 0 || answer == 3;
            bound Finite: 0 <= journal && journal <= 5 && 0 <= bstamp && bstamp <= 1
                && 0 <= cstamp && cstamp <= 1 && 0 <= legacy && legacy <= 1
                && 0 <= moved && moved <= 1 && 0 <= live && live <= 1 && 0 <= late && late <= 1
                && 0 <= boot && boot <= 1 && 0 <= granted && granted <= 1
                && 0 <= child && child <= 1 && 0 <= clock && clock <= 2
                && 0 <= rollback_granted && rollback_granted <= 1
                && 0 <= rollback && rollback <= 1
                && 0 <= seen && seen <= 7 && 0 <= stood && stood <= 1
                && 0 <= answer && answer <= 3 && 0 <= fresh && fresh <= 1;
        }
    }
}
