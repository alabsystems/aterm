// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! External mutations and deliberately defective History actions are not production steps.

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CapacityHistory",
        action = "RestoreWorkingBeforePublication",
        reason = "External mutation restores the original working inode after the accepted capture; production detects changed ctime and refuses publication."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CapacityHistory",
        action = "RestorePredecessor",
        reason = "External mutation adds or restores a predecessor member after publication; production preserves that unknown member."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CapacityHistory",
        action = "PublishUncovered",
        reason = "Buggy=1 negative control omits the required base FILE and directory cover; no production path may take it."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CapacityHistory",
        action = "RecaptureRestoredBase",
        reason = "Buggy=2 negative control blesses a restored working origin after cover; production retains the original capture."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CapacityHistory",
        action = "RotatePastUnprunedPrefix",
        reason = "Buggy=3 negative control creates a third epoch before predecessor removal; production prunes or refuses first."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CapacityHistory",
        action = "PruneUnknown",
        reason = "Buggy=4 negative control deletes a changed predecessor member; production requires the original held and named stamp."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CapacityHistory",
        action = "ConsumeTerminalReserve",
        reason = "Buggy=5 negative control consumes the final ordinary-write slot; production rotates before working mutation."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CapacityHistory",
        action = "RemoveUnknownPredecessor",
        reason = "Buggy=6 negative control removes an unknown predecessor directory; production requires exact identity and an actually empty directory."
    )
)]
#[doc(hidden)]
pub fn capacity_history_environment() {}
