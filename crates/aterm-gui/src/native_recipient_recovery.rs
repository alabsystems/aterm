// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Historical completion from complete recipient evidence, never a new grant.

use super::*;
use aterm_agent::harness::native_receipts::RecipientHistory;

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "RecipientClosure",
        action = "Complete",
        reason = "The issuer chooses the complete journal format before admission"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "RecipientClosure",
        action = "Prepare",
        reason = "The authenticated journal admits a prepared record, as modeled by RecipientJournal"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "RecipientClosure",
        action = "RecordF",
        reason = "The issuer records the exact forward recipient before possible delivery, as modeled by RecipientJournal"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "RecipientClosure",
        action = "RecordR",
        reason = "The issuer records the rollback recipient after forward retirement or without a forward grant"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "RecipientClosure",
        action = "SealOpen",
        reason = "The authenticated journal seals an operation with no possible recipient"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "RecipientClosure",
        action = "SealMay",
        reason = "The authenticated journal seals an operation while preserving every possible recipient"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "RecipientClosure",
        action = "NoExec",
        reason = "The authenticated journal records NoExec only when no recipient was recorded"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "RecipientClosure",
        action = "DeliverF",
        reason = "The forward launcher receives Execute only before the original deadline"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "RecipientClosure",
        action = "DeliverR",
        reason = "The rollback launcher receives Execute only before the original deadline"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "RecipientClosure",
        action = "LegacyDeliver",
        reason = "A legacy issuer can deliver without recording a complete recipient set"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "RecipientClosure",
        action = "ExitF",
        reason = "The recorded forward process exits but its group may still act"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "RecipientClosure",
        action = "ExitR",
        reason = "The recorded rollback process exits but its group may still act"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "RecipientClosure",
        action = "ClearF",
        reason = "The forward process group can no longer act"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "RecipientClosure",
        action = "ClearR",
        reason = "The rollback process group can no longer act"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "RecipientClosure",
        action = "Crash",
        reason = "The original issuer epoch ends without erasing its authenticated history"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "RecipientClosure",
        action = "Expire",
        reason = "The original boot monotonic clock reaches the immutable deadline"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "RecipientClosure",
        action = "Reboot",
        reason = "Another boot of the same machine ends the original issuer and all its recipients"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "RecipientClosure",
        action = "Foreign",
        reason = "A different machine has an unrelated monotonic clock"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "RecipientClosure",
        action = "UnknownBoot",
        reason = "The reader cannot establish its current machine and boot"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "RecipientClosure",
        action = "UnknownBirth",
        reason = "A kernel birth observation is unavailable"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "RecipientClosure",
        action = "UnknownGroup",
        reason = "A process-group observation is unavailable"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "RecipientClosure",
        action = "Fresh",
        reason = "The harness creates a fresh plan only after the lapsed answer"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "RecipientClosure",
        action = "Probe",
        project = "aterm_gui::native_act::tests::recipient_recovery::World::probe"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "RecipientClosure",
        action = "Answer",
        project = "aterm_gui::native_act::tests::recipient_recovery::World::probe"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "RecipientClosure",
        action = "AnswerBoot",
        project = "aterm_gui::native_act::tests::recipient_recovery::World::probe"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "RecipientClosure",
        action = "AnswerNoExec",
        project = "aterm_gui::native_act::tests::recipient_recovery::World::probe"
    )
)]
pub(super) fn answer(
    prepare: &Prepare,
    history: &RecipientHistory,
    clock_us: u64,
    current_boot: Option<&BootStamp>,
    kernel: &dyn Kernel,
) -> Option<String> {
    let any = history.forward.is_some() || history.rollback.is_some();
    let consistent = match history.state {
        Recorded::Prepared | Recorded::NoExec | Recorded::Sealed { may_execute: false } => !any,
        Recorded::MayExecute | Recorded::Sealed { may_execute: true } => any,
    };
    if !consistent {
        return None;
    }
    if history.state == Recorded::NoExec {
        return Some(NOEXEC.into());
    }
    let origin = prepare.boot.as_ref()?;
    let current = current_boot?;
    if current.host != origin.host {
        return None;
    }
    let ended = if current.boot != origin.boot {
        true
    } else {
        match history.state {
            Recorded::Sealed { may_execute: false } => true,
            Recorded::Prepared => clock_us >= prepare.deadline_us,
            Recorded::MayExecute | Recorded::Sealed { may_execute: true } => {
                clock_us >= prepare.deadline_us
                    && [history.forward, history.rollback]
                        .into_iter()
                        .flatten()
                        .all(|recipient| {
                            kernel.birth_is_alive(recipient.pid, recipient.birth) == Some(false)
                                && kernel.group_gone(recipient.group) == Some(true)
                        })
            }
            Recorded::NoExec => unreachable!(),
        }
    };
    ended
        .then(|| aterm_agent::harness::native_act::lapsed_reply(&prepare.id))
        .flatten()
}
