// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A STAGE'S ROSTER AUTHORIZATION GOES STALE (round eight of the update audit,
//! #503). A build staged under a machine roster is withdrawn by a LATER roster
//! that revokes its signer — and a roster reaches this Mac only through a check.
//! The launch swaps before any check runs, so a stage signed by a machine revoked
//! while the app was quit was installed at the next launch, the revocation unread.
//!
//! A WRITER/READER pair (the shape of `NativeUpdateFailedMarkSuppression`). The
//! writer is the check that admits a roster: it retires a stage the roster revokes
//! and stamps one it does not (`aterm_update`'s `observe_floor_and_withdraw_at`
//! and `confirm_roster_authorization`; the staging check stamps the stage it
//! publishes). The readers are every lane that swaps — the launch's gates
//! (`stage_admission_gates`, gate 4d) and the in-session lane's pre-park checks —
//! which admit a stage only while its stamp is younger than the window.
//!
//! The guarantee is a bound, not an absence: a revocation published inside the
//! window before a swap is the residual (docs/RELEASE-KEYS.md); one published at
//! least a window before it is never missed.

use super::Model;

/// `NativeUpdateRosterFreshness`: one stage, the owner's revocation of its signer,
/// the checks that read the roster, and the swap.
///
/// Time is in ticks of half a window (twelve hours): `F` ticks is the window
/// (`aterm_update`'s `ROSTER_FRESHNESS_SECS`, a day) and `T` the horizon. How
/// often a running client re-reads the roster (`ROSTER_RECONFIRM_AFTER_SECS`, an
/// hour) is the environment's: `Check` may fire at any tick. `now` is the clock;
/// `conf` the tick the stage's roster authorization was last confirmed (it was
/// confirmed when it was staged, at 0); `revoked` and `rev_at` whether and when
/// the owner published a roster revoking its signer; `staged` whether the stage is
/// on disk; `swapped` whether a lane installed it; `late` whether that swap came at
/// least a window after the revocation.
///
/// Actions. `Tick` and `Revoke` are the environment (time, and the owner). `Check`
/// is a check that admits the channel head's current roster: it RETIRES a stage the
/// roster revokes and CONFIRMS one it does not. A check that admits no roster (the
/// unchanged pointer's one HEAD, a fetch that failed) changes nothing and is not an
/// action. `Swap` is any lane's install: the stage on disk, its confirmation
/// younger than the window.
///
/// Invariant `NoSwapPastTheWindow` — a swap never installs a stage whose signer's
/// revocation was published a window or more before it. Broken, each alone, by
/// the `Buggy = 1` actions:
///
/// * `SwapOnTheFloorAlone` — the swap asks only the floor (what a check has seen),
///   as the launch did before #503: a stage confirmed days ago is installed the
///   moment the app starts.
/// * `ConfirmWithoutTheRoster` — a check stamps the stage with a time it did not
///   read the roster at: an unchanged head's pointer read taken for a
///   confirmation, a stamp written before the withdrawal, or a stamp dated at the
///   WRITE when the roster was read earlier — before the owner's revocation, then
///   a download of hours before the publish (#503's review). `Check` stamps the
///   tick it reads the roster at, which is what the shipping check records.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_roster_freshness_model() -> Model {
    crate::ty_model! {
        NativeUpdateRosterFreshness {
            const Buggy = 0;
            const F = 2;
            const T = 5;
            var now = 0;
            var conf = 0;
            var revoked = 0;
            var rev_at = 0;
            var staged = 1;
            var swapped = 0;
            var late = 0;

            // The environment: time passes; the owner revokes the stage's signer.
            action Tick when (now <= T - 1) {
                now = now + 1;
            }
            action Revoke when (revoked == 0) {
                revoked = 1;
                rev_at = now;
            }
            // A check admits the current roster: retire, or confirm.
            action Check when (staged == 1 && swapped == 0) {
                staged = if revoked == 1 { 0 } else { 1 };
                conf = if revoked == 1 { conf } else { now };
            }
            // Any lane's swap: the stage confirmed inside the window.
            action Swap when (staged == 1 && swapped == 0 && now - conf <= F - 1) {
                swapped = 1;
                late = if revoked == 1 && F <= now - rev_at { 1 } else { 0 };
            }

            // Mutants, dead at Buggy = 0.
            action SwapOnTheFloorAlone when (Buggy == 1 && staged == 1 && swapped == 0) {
                swapped = 1;
                late = if revoked == 1 && F <= now - rev_at { 1 } else { 0 };
            }
            action ConfirmWithoutTheRoster when (Buggy == 1 && staged == 1 && swapped == 0) {
                conf = now;
            }

            invariant NoSwapPastTheWindow: late == 0;
        }
    }
}
