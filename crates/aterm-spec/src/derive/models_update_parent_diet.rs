// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The outgoing process's artifact diet (`docs/DESIGN-warm-successor-2026-09-29.md` §5).
//!
//! On the launched lane the attempt-bound layout sidecar (synced, then its directory) is
//! written before the park, at the attempt's own name, so the freeze the person feels only
//! checks it; the window/tab/pane layout rarely moves while a successor boots.
//!
//! A writer/reader contract (the shape of `NativeUpdateFailedMarkSuppression`). The writer
//! is the outgoing process, before the park (`handoff_diet::speculate_layout`) and at it
//! (`handoff_diet::seal_layout`); the reader is the successor, which reads the sidecar
//! after its grant and hashes it into the adoption proof. Whatever the successor reads is
//! exactly the park's layout, and a speculative sidecar reaches the grant only after it
//! was read back and found byte-equal.

use super::{Liveness, Model, gt, int, var};

/// `NativeUpdateSpeculativeSidecar`: a layout sidecar written before the park is handed
/// to the successor only when it is byte-equal to what the park would write.
///
/// `phase`: 0 launched, 1 held (speculation written or refused, readers live), 2 parked,
/// 3 sealed (the park decided what the file holds), 4 granted (the successor reads the
/// sidecar), 5 retired (rollback or failure). `disk`: what the sidecar's name holds — 0
/// nothing (never written, swept, or consumed), 1 the speculative bytes, 2 bytes written
/// at the park, 3 bytes nobody here wrote. `moved`: the park's layout differs from the
/// speculated one. `verified`: the park read the file back and found it byte-equal.
/// `read`: 0 not read, 1 the successor read the park's bytes, 2 other bytes.
///
/// Environment (none fair): `RefuseWrite` (no room, no permission), `LayoutMoves` (a tab
/// opened, a window moved, a directory changed), `DiskDisturbed` (the file replaced or
/// truncated by someone else).
///
/// Invariants, each broken alone by its own `Buggy = 1` action:
/// * `SpeculationVerifiedBeforeSeal` — a sealed file still holding the speculative bytes
///   was verified; broken by `SealUnverified`.
/// * `SealHoldsTheParksBytes` — broken by `VerifyTheMemoryCopy`, which compares the bytes
///   it meant to write instead of the file, so a disturbed file passes.
/// * `SuccessorReadsTheParksBytes` — the reader half; broken by `GrantBeforeSeal`.
/// * `NothingSpeculativeOutlivesTheAttempt` — a retired attempt leaves no file at the
///   name; broken by `RetireKeepsSpeculation` (no sweep).
/// * `ParkNeverWaitsOnSpeculation` — the park never waits on a refused speculation; broken
///   by `AwaitSpeculation`, also the liveness mutant
///   ([`native_update_speculative_sidecar_liveness`]).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_speculative_sidecar_model() -> Model {
    crate::ty_model! {
        NativeUpdateSpeculativeSidecar {
            const Buggy = 0;
            var phase = 0;
            var fails = 0;
            var spec = 0;
            var moved = 0;
            var disk = 0;
            var verified = 0;
            var read = 0;
            var awaiting = 0;

            // The environment: this attempt's speculative write will be refused.
            action RefuseWrite when (phase == 0 && fails == 0) {
                fails = 1;
            }
            // Before the park, at the launch: the layout sidecar is written at
            // the attempt's name, or the write is refused and nothing is there.
            action Speculate when (phase == 0) {
                spec = if (fails == 1) { 0 } else { 1 };
                disk = if (fails == 1) { 0 } else { 1 };
                phase = 1;
            }
            // The environment, while every reader is live.
            action LayoutMoves when (phase == 1 && moved == 0) {
                moved = 1;
            }
            action DiskDisturbed when (phase == 1 && disk == 1) {
                disk = 3;
            }
            action Park when (phase == 1) {
                phase = 2;
            }
            // The seal: the file read back as the successor will read it, and compared
            // with the park's own bytes.
            action KeepVerified when (phase == 2 && spec == 1 && disk == 1 && moved == 0) {
                verified = 1;
                phase = 3;
            }
            action DiscardAndRewrite when (phase == 2 && spec == 1 && (moved == 1 || disk == 3)) {
                disk = 2;
                phase = 3;
            }
            action WriteAtPark when (phase == 2 && spec == 0 && awaiting == 0) {
                disk = 2;
                phase = 3;
            }
            action Grant when (phase == 3) {
                phase = 4;
            }
            // The reader: the successor's intake of the sidecar after its grant,
            // which consumes the file.
            action SuccessorReads when (phase == 4 && read == 0) {
                read = if (disk == 2 || (disk == 1 && moved == 0)) { 1 } else { 2 };
                disk = 0;
            }
            // Every failure or rollback of the attempt: its artifacts retired
            // by their prefix.
            action Retire when (phase > 0 && phase <= 4) {
                disk = 0;
                phase = 5;
            }

            // Mutants, dead at Buggy = 0.
            action SealUnverified when (Buggy == 1 && phase == 2 && spec == 1 && disk == 1) {
                phase = 3;
            }
            action VerifyTheMemoryCopy when (
                Buggy == 1 && phase == 2 && spec == 1 && moved == 0 && disk == 3
            ) {
                verified = 1;
                phase = 3;
            }
            action GrantBeforeSeal when (Buggy == 1 && phase == 2 && spec == 1) {
                phase = 4;
            }
            action RetireKeepsSpeculation when (
                Buggy == 1 && phase > 0 && phase <= 4 && disk == 1
            ) {
                phase = 5;
            }
            // (liveness) The park waits for a speculation that was refused.
            action AwaitSpeculation when (
                Buggy == 1 && phase == 2 && spec == 0 && awaiting == 0
            ) {
                awaiting = 1;
            }

            invariant SpeculationVerifiedBeforeSeal:
                phase <= 2 || phase > 3 || disk <= 0 || disk > 1 || verified == 1;
            invariant SealHoldsTheParksBytes:
                phase <= 2 || phase > 3 || disk == 2 || (disk == 1 && moved == 0);
            invariant SuccessorReadsTheParksBytes: read <= 1;
            invariant NothingSpeculativeOutlivesTheAttempt: phase <= 4 || disk == 0;
            invariant ParkNeverWaitsOnSpeculation: awaiting == 0;
        }
    }
}

/// [`native_update_speculative_sidecar_model`]'s progress claim: every attempt is granted
/// or retired; the speculation can save the park a write, never hold the grant.
///
/// Fairness, each an assumption someone owns:
/// * `Speculate` — the worker's straight-line write after the launch (a refusal is
///   `RefuseWrite`'s branch, not a wait);
/// * `Park` — the park gate admits at once for an explicit apply, and by the hold cap on
///   the automatic lane, which otherwise stands the successor down (`Retire`);
/// * `KeepVerified`, `DiscardAndRewrite`, `WriteAtPark` — one read and at most one write on
///   the worker, waiting on nothing;
/// * `Grant` — the one `sendmsg` after the artifacts.
///
/// Mutant: `AwaitSpeculation`, a park that waits on a sidecar the launch never wrote.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[crate::liveness(crate::derive::native_update_speculative_sidecar_model)]
pub fn native_update_speculative_sidecar_liveness() -> Liveness {
    Liveness {
        name: "TheAttemptIsGrantedOrRetired",
        goal: gt(var("phase"), int(3)),
        weak: vec![
            "Speculate",
            "Park",
            "KeepVerified",
            "DiscardAndRewrite",
            "WriteAtPark",
            "Grant",
        ],
        strong: vec![],
        mutants: vec!["AwaitSpeculation"],
    }
}
