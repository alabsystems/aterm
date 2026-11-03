// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Bounded checkpoint replacement and the transfer of terminal evidence.

use aterm_spec::derive::*;

/// A claim keeps at most a current epoch and one finite predecessor. The production
/// window is 32; its final entry is reserved for terminal publication. Two rotations
/// suffice to expose an attempt to start a third physical epoch before pruning the old
/// one. The origin flags record equality with the descriptors accepted before the
/// covering flush, including a same-byte restore that changed the physical stamp.
///
/// Mutants publish without cover, recapture a restored base, start a third epoch,
/// delete an unknown predecessor member, or consume the terminal reserve. No model
/// state is a filesystem capability. Runtime traces must supply these observations.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn capacity_history_model() -> Model {
    aterm_spec::ty_model! {
        CapacityHistory {
            const Buggy = 0;
            const Window = 32;

            var phase = 0;
            var rotations = 0;
            var epochs = 1;
            var current_members = 1;
            var predecessor_members = 0;
            var predecessor_present = 0;
            var predecessor_original = 1;
            var working_original = 1;
            var base_covered = 0;
            var published = 0;
            var publication_original = 1;
            var deleted_unknown = 0;

            action Append when (phase == 0 && working_original == 1
                && Window - 1 > current_members) {
                current_members = current_members + 1;
            }
            action BeginRotation when (phase == 0 && current_members == Window - 1
                && predecessor_members == 0 && predecessor_present == 0
                && epochs == 1 && 2 > rotations
                && working_original == 1) {
                phase = 1;
                epochs = 2;
                base_covered = 0;
                published = 0;
            }
            action RestoreWorkingBeforePublication when ((phase == 1 || phase == 2)
                && working_original == 1) {
                working_original = 0;
            }
            action CoverBase when (phase == 1 && working_original == 1) {
                phase = 2;
                base_covered = 1;
            }
            action Publish when (phase == 2 && base_covered == 1 && working_original == 1) {
                phase = 0;
                published = 1;
                publication_original = working_original;
                predecessor_members = current_members;
                predecessor_present = 1;
                current_members = 1;
                rotations = rotations + 1;
            }
            action RestorePredecessor when (phase == 0 && predecessor_present == 1
                && predecessor_original == 1) {
                predecessor_original = 0;
            }
            action PruneOne when (phase == 0 && predecessor_members > 0
                && predecessor_original == 1 && published == 1 && base_covered == 1) {
                predecessor_members = predecessor_members - 1;
            }
            action RemoveEmptyPredecessor when (phase == 0 && predecessor_present == 1
                && predecessor_members == 0 && predecessor_original == 1
                && published == 1 && base_covered == 1) {
                predecessor_present = 0;
                epochs = 1;
            }

            action PublishUncovered when (Buggy == 1 && phase == 1
                && working_original == 1) {
                phase = 0;
                published = 1;
                predecessor_members = current_members;
                predecessor_present = 1;
                current_members = 1;
            }
            action RecaptureRestoredBase when (Buggy == 2 && phase == 2
                && working_original == 0) {
                phase = 0;
                published = 1;
                publication_original = 0;
                predecessor_members = current_members;
                predecessor_present = 1;
                current_members = 1;
            }
            action RotatePastUnprunedPrefix when (Buggy == 3 && phase == 0
                && predecessor_present == 1 && current_members == Window - 1) {
                epochs = 3;
            }
            action PruneUnknown when (Buggy == 4 && phase == 0
                && predecessor_members > 0 && predecessor_original == 0) {
                predecessor_members = predecessor_members - 1;
                deleted_unknown = 1;
            }
            action ConsumeTerminalReserve when (Buggy == 5 && phase == 0
                && current_members == Window - 1) {
                current_members = Window;
            }
            action RemoveUnknownPredecessor when (Buggy == 6 && phase == 0
                && predecessor_present == 1 && predecessor_members == 0
                && predecessor_original == 0) {
                predecessor_present = 0;
                epochs = 1;
                deleted_unknown = 1;
            }

            invariant PublicationFollowsCover: published == 0 || base_covered == 1;
            invariant PublicationUsesFrozenOriginal: published == 0 || publication_original == 1;
            invariant AtMostTwoPhysicalEpochs: epochs <= 2;
            invariant UnknownPredecessorsArePreserved: deleted_unknown == 0;
            invariant OrdinaryWritesPreserveTerminalRoom: Window > current_members;
            bound HistoryIsFinite: phase <= 2 && rotations <= 2
                && Window > predecessor_members && current_members <= Window
                && predecessor_present <= 1;
        }
    }
}

/// The archive receipt covers the same terminal claim before its directory leaves the
/// admission plane. An original controller may seal a retired generation while holding
/// its own guard; cold conversion instead needs a fresh exclusive guard. Both paths retain
/// the claim mutation gate and prove actual drain before the irreversible terminal commit.
/// A compacted history can disappear only after an exact finite deletion
/// inventory has been covered. The terminal receipts themselves stay present: no owning
/// Host durable-ACK/final-deletion port exists in this prototype. A future port must extend
/// this model and bind its actual journal ACK, rather than treating Closed as deletion
/// authority. Physical reference and slot equality are sampled at a closure decision;
/// subsequent external changes are judged by a new query.
/// The successful query releases its actual gate; the next offered reference starts a
/// new decision without refreshing the physical slot origin or any stored receipt.
///
/// Mutants move before cover, close on a live group, compact an unknown inventory,
/// accept a different closure reference, accept a detached slot, bypass the slot mutation
/// gate, delete final evidence without an owning Host ACK, move while another guard is
/// held, or transfer without the mutation gate.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn capacity_terminal_model() -> Model {
    aterm_spec::ty_model! {
        CapacityTerminal {
            const Buggy = 0;

            var phase = 0;
            var drained = 0;
            var guards_free = 0;
            var owner_terminal = 0;
            var terminal_owner = 0;
            var mutation_held = 0;
            var terminal_covered = 0;
            var archive_covered = 0;
            var moved = 0;
            var move_drained = 0;
            var move_guard_free = 0;
            var move_owner = 0;
            var move_mutation_held = 0;
            var inventory_covered = 0;
            var inventory_original = 1;
            var compacted = 0;
            var deleted_unknown = 0;
            var reference_matches = 1;
            var slot_original = 1;
            var reported_closed = 0;
            var query_reference_matches = 1;
            var query_slot_original = 1;
            var query_mutation_held = 1;
            var receipts_present = 0;

            action ActualDrain when (phase == 0 && drained == 0) { drained = 1; }
            action GuardsClose when (phase == 0 && guards_free == 0) { guards_free = 1; }
            action RetainedOwnerTerminal when (phase == 0 && drained == 1 && guards_free == 0
                && mutation_held == 1 && owner_terminal == 0) {
                owner_terminal = 1;
            }
            action HoldMutation when (mutation_held == 0 && reported_closed == 0) {
                mutation_held = 1;
            }
            action ReleaseMutation when (mutation_held == 1) {
                mutation_held = 0;
            }
            action NextReference when (reported_closed == 1 && mutation_held == 0) {
                reported_closed = 0;
                reference_matches = 1;
            }
            action CoverTerminal when (phase == 0 && drained == 1
                && (guards_free == 1 || owner_terminal == 1)
                && mutation_held == 1) {
                terminal_covered = 1;
                terminal_owner = owner_terminal;
                receipts_present = 1;
                phase = 1;
            }
            action CoverArchive when (phase == 1 && terminal_covered == 1
                && mutation_held == 1) {
                archive_covered = 1;
                phase = 2;
            }
            action Move when (phase == 2 && terminal_covered == 1 && archive_covered == 1
                && drained == 1 && (guards_free == 1 || terminal_owner == 1)
                && mutation_held == 1) {
                moved = 1;
                move_drained = drained;
                move_guard_free = guards_free;
                move_owner = terminal_owner;
                move_mutation_held = mutation_held;
                phase = 3;
            }
            action CoverInventory when (phase == 3 && inventory_original == 1
                && mutation_held == 1) {
                inventory_covered = 1;
                phase = 4;
            }
            action RestoreInventory when ((phase == 3 || phase == 4)
                && inventory_original == 1 && compacted == 0) {
                inventory_original = 0;
            }
            action Compact when (phase == 4 && inventory_covered == 1
                && inventory_original == 1 && mutation_held == 1) {
                compacted = 1;
                phase = 5;
            }
            action OfferDifferentReference when (moved == 1 && reported_closed == 0
                && reference_matches == 1) {
                reference_matches = 0;
            }
            action DetachSlot when (moved == 1 && reported_closed == 0 && slot_original == 1) {
                slot_original = 0;
            }
            action Query when (moved == 1 && reported_closed == 0 && reference_matches == 1
                && slot_original == 1 && mutation_held == 1 && receipts_present == 1) {
                reported_closed = 1;
                query_reference_matches = reference_matches;
                query_slot_original = slot_original;
                query_mutation_held = mutation_held;
            }

            action MoveUncovered when (Buggy == 1 && phase == 1 && mutation_held == 1) {
                moved = 1;
                move_drained = drained;
                move_guard_free = guards_free;
                move_owner = terminal_owner;
                move_mutation_held = mutation_held;
                phase = 3;
            }
            action MoveLiveGroup when (Buggy == 2 && phase == 0 && drained == 0
                && guards_free == 1 && mutation_held == 1) {
                terminal_covered = 1;
                archive_covered = 1;
                receipts_present = 1;
                moved = 1;
                move_drained = 0;
                move_guard_free = guards_free;
                move_owner = terminal_owner;
                move_mutation_held = mutation_held;
                phase = 3;
            }
            action CompactUnknown when (Buggy == 3 && phase == 4 && inventory_original == 0) {
                compacted = 1;
                deleted_unknown = 1;
                phase = 5;
            }
            action QueryDifferentReference when (Buggy == 4 && moved == 1
                && reported_closed == 0 && reference_matches == 0 && mutation_held == 1) {
                reported_closed = 1;
                query_reference_matches = 0;
            }
            action QueryDetachedSlot when (Buggy == 5 && moved == 1
                && reported_closed == 0 && slot_original == 0 && mutation_held == 1) {
                reported_closed = 1;
                query_slot_original = 0;
            }
            action QueryWithoutMutation when (Buggy == 6 && moved == 1
                && reported_closed == 0 && mutation_held == 0) {
                reported_closed = 1;
                query_mutation_held = 0;
            }
            action DeleteFinalEvidence when (Buggy == 7 && compacted == 1
                && receipts_present == 1) {
                receipts_present = 0;
            }
            action MoveHeldGuards when (Buggy == 8 && phase == 0 && drained == 1
                && guards_free == 0 && owner_terminal == 0 && mutation_held == 1) {
                terminal_covered = 1;
                archive_covered = 1;
                receipts_present = 1;
                moved = 1;
                move_drained = drained;
                move_guard_free = 0;
                move_owner = 0;
                move_mutation_held = mutation_held;
                phase = 3;
            }
            action MoveWithoutMutation when (Buggy == 9 && phase == 2 && mutation_held == 0) {
                moved = 1;
                move_drained = drained;
                move_guard_free = guards_free;
                move_owner = terminal_owner;
                move_mutation_held = 0;
                phase = 3;
            }

            invariant TransferFollowsDurableCover: moved == 0
                || (terminal_covered == 1 && archive_covered == 1);
            invariant TransferHasActualDrainAndTerminalCustody: moved == 0
                || (move_drained == 1 && (move_guard_free == 1 || move_owner == 1));
            invariant CompactionPreservesUnknownData: deleted_unknown == 0;
            invariant ClosureNamesExactReference: reported_closed == 0 || query_reference_matches == 1;
            invariant ClosureRetainsNamedSlot: reported_closed == 0 || query_slot_original == 1;
            invariant TransferAndClosureAreSerialized:
                (reported_closed == 0 || query_mutation_held == 1)
                && (moved == 0 || move_mutation_held == 1);
            invariant FinalEvidenceWaitsForHostPort: terminal_covered == 0 || receipts_present == 1;
            bound TransferPhasesAreFinite: phase <= 5;
        }
    }
}
