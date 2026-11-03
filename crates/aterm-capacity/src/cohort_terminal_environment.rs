// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! External observations and deliberate negative controls of the terminal model. Its six
//! publication, transfer, compaction and query operations have separate production anchors.

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CapacityTerminal",
        action = "ActualDrain",
        reason = "Kernel environment: the actual bound child group drains; publication checks the resulting lifetime, and physical traces use an actual child and kernel observation"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CapacityTerminal",
        action = "GuardsClose",
        reason = "Descriptor-holder environment: the last other guard closes; cold conversion must still obtain its own fresh exclusive guard rather than infer closure from this observation"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CapacityTerminal",
        action = "RetainedOwnerTerminal",
        reason = "Controller-custody input: an original controller retains its own guard and a committed retired generation; this predicate distinguishes that path from fresh-exclusive cold conversion"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CapacityTerminal",
        action = "HoldMutation",
        reason = "Kernel lock environment: Transaction or Slot acquires a fresh open-description lock; the physical projection samples its actual exclusion rather than a serialized lock flag"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CapacityTerminal",
        action = "ReleaseMutation",
        reason = "Kernel lock environment: dropping the owned Transaction or Slot releases its description; a later operation must acquire another fresh description"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CapacityTerminal",
        action = "NextReference",
        reason = "Caller environment: after a query returns the caller offers another request; this resets the decision without refreshing any retained receipt or physical slot origin"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CapacityTerminal",
        action = "OfferDifferentReference",
        reason = "Caller environment: a different data-only closure reference is offered; decoding that observation supplies no closure or recovery authority"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CapacityTerminal",
        action = "DetachSlot",
        reason = "Filesystem environment: another actor replaces the named slot while an old descriptor remains; query must retain the original parent/name relationship"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CapacityTerminal",
        action = "RestoreInventory",
        reason = "Filesystem environment: an inventory member changes or is restored with a different physical stamp; compaction must preserve the resulting uncertainty"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CapacityTerminal",
        action = "MoveUncovered",
        reason = "Buggy=1 negative control: transfer precedes the archive cover; this is an intentionally faulty model transition, not another publication port"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CapacityTerminal",
        action = "MoveLiveGroup",
        reason = "Buggy=2 negative control: transfer proceeds without actual group drain; this is not an additional production transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CapacityTerminal",
        action = "CompactUnknown",
        reason = "Buggy=3 negative control: compaction deletes an unrecognized inventory; physical unknown-data controls judge the corresponding production boundary separately"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CapacityTerminal",
        action = "QueryDifferentReference",
        reason = "Buggy=4 negative control: a closure decision names another reference; the faulty decision is not a second query implementation"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CapacityTerminal",
        action = "QueryDetachedSlot",
        reason = "Buggy=5 negative control: a closure decision accepts a detached slot; this deliberately violates the named-origin law"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CapacityTerminal",
        action = "QueryWithoutMutation",
        reason = "Buggy=6 negative control: a closure decision bypasses its fresh mutation description; the physical held-gate control tests actual exclusion"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CapacityTerminal",
        action = "DeleteFinalEvidence",
        reason = "Buggy=7 negative control: final receipts disappear without an owning Host ACK; the prototype has no final-evidence deletion port"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CapacityTerminal",
        action = "MoveHeldGuards",
        reason = "Buggy=8 negative control: cold transfer lacks both a fresh exclusive guard and retained original-controller terminal custody"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CapacityTerminal",
        action = "MoveWithoutMutation",
        reason = "Buggy=9 negative control: transfer bypasses its actual mutation gate; this faulty transition is not a production operation"
    )
)]
#[doc(hidden)]
pub fn capacity_terminal_environment() {}
