// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The two `authorize_*` ceremonies (lattice edges) and their capability
//! tokens. See `designs/2026-04-19-provenance-framework.md` §4.4.

use core::marker::PhantomData;

use crate::metrics::Subsystem;
use crate::origin::{Host, NetworkUntrusted, Pty};
use crate::provenance::{DynProvenance, Provenance, UnliftableTop};

/// Capability token authorizing a `Pty` → `Host` lift.
///
/// Non-`Clone`, non-`Copy`, lifetime-bound. Consumers mint these from
/// subsystem capabilities (`ConductorActivationToken`, `ResponseCapability`,
/// etc.) by calling `.as_host_auth_token(&self) -> HostAuthorizationToken<'_>`
/// on the subsystem capability. See §6 migration table.
///
/// The token is intentionally zero-sized and carries no data: its role is
/// purely to make the lift auditable (`grep -rn 'fn authorize_'`).
#[derive(Debug)]
pub struct HostAuthorizationToken<'a> {
    _lifetime: PhantomData<&'a ()>,
}

impl HostAuthorizationToken<'_> {
    /// Mint a token on behalf of a capability-bearing subsystem.
    ///
    /// # Capability seal (#8013)
    ///
    /// This constructor is gated behind the `internal-mint` feature (or the
    /// in-crate `test` cfg). Only an explicit allow-list of capability-bearing
    /// crates activates the feature — see the design note in
    /// `designs/2026-04-19-provenance-framework.md` §4.4 and the CI enforcer at
    /// `aterm audit policy --seals`. Without the feature this symbol
    /// does not exist in compiled builds, so no downstream workspace crate can
    /// forge a [`HostAuthorizationToken`] by accident or by name.
    ///
    /// The constructor is `#[doc(hidden)]` and deliberately verbosely named so
    /// that `cargo doc` does not surface it as an inviting public API and so
    /// that grep-audits flag any non-allow-listed caller. Every capability
    /// module that calls it does so through `as_host_auth_token(&self)`; the
    /// `check-provenance-ceremony.sh` script (run in CI) is a secondary
    /// belt-and-braces grep audit that each call site is inside a recognized
    /// capability module.
    ///
    /// The constructor takes no argument and produces a bare ZST — the audit
    /// relies on *where* the call occurs (a capability method) plus the
    /// feature gate (only an allow-listed crate can even compile this symbol),
    /// not on a runtime witness. (ATERM_DESIGN §5.4 supersedes this with a
    /// sealed, by-reference, reachability-proven mint; this is the interim form.)
    #[cfg(any(test, feature = "internal-mint"))]
    #[doc(hidden)]
    #[must_use]
    pub fn __new_for_capability_only() -> Self {
        Self {
            _lifetime: PhantomData,
        }
    }
}

/// Capability token authorizing a `NetworkUntrusted` → `Host` lift.
///
/// Separate from [`HostAuthorizationToken`] because network-origin lifts are
/// approved by a different subsystem (the host app's network-config module)
/// than PTY-origin lifts (parser-side modal auth). Having distinct token
/// types keeps the two code paths independently auditable.
#[derive(Debug)]
pub struct NetworkAuthorizationToken<'a> {
    _lifetime: PhantomData<&'a ()>,
}

impl NetworkAuthorizationToken<'_> {
    /// Mint a token on behalf of a capability-bearing network-config
    /// subsystem. See [`HostAuthorizationToken::__new_for_capability_only`]
    /// for the capability-seal rationale (#8001, #8013).
    ///
    /// Gated behind the `internal-mint` feature (or `test` cfg) for the same
    /// reason as `HostAuthorizationToken::__new_for_capability_only`. CI
    /// enforcer: `aterm audit policy --seals`.
    #[cfg(any(test, feature = "internal-mint"))]
    #[doc(hidden)]
    #[must_use]
    pub fn __new_for_capability_only() -> Self {
        Self {
            _lifetime: PhantomData,
        }
    }
}

/// Lift a `Provenance<T, Pty>` to `Provenance<T, Host>`. The single canonical
/// way up the bottom edge of the lattice.
///
/// Consuming the [`HostAuthorizationToken`] records a lift site in the audit
/// surface; the token was minted elsewhere (by a capability-bearing module)
/// and handed to us. This is the generalized pattern of the Terminal-class RCE
/// fix (#7875).
#[must_use]
pub fn authorize_pty_to_host<T>(
    x: Provenance<T, Pty>,
    _cap: HostAuthorizationToken<'_>,
) -> Provenance<T, Host> {
    Provenance::from_host(x.value)
}

/// Lift a `Provenance<T, NetworkUntrusted>` to `Provenance<T, Host>`.
///
/// Separate ceremony from [`authorize_pty_to_host`] because the authorizing
/// capability is different (the host app's network-config module vs. the
/// parser-side modal auth).
#[must_use]
pub fn authorize_network_to_host<T>(
    x: Provenance<T, NetworkUntrusted>,
    _cap: NetworkAuthorizationToken<'_>,
) -> Provenance<T, Host> {
    Provenance::from_host(x.value)
}

// ---------------------------------------------------------------------------
// DynProvenance authorize entry points (§7.2 drop-on-Top enforcement)
// ---------------------------------------------------------------------------

/// Lift a `DynProvenance<T>` whose runtime tag is `Pty` to `Provenance<T, Host>`.
///
/// This is the dynamic-tagged analogue of [`authorize_pty_to_host`] and is
/// the §7.2 enforcement entry point for subsystems that handle `DynProvenance`
/// at an FFI / storage boundary.
///
/// The input is consumed regardless of outcome.
///
/// # Errors
///
/// Returns [`UnliftableTop::Top`] when:
/// * The carrier is the synthetic `Top` element. The per-subsystem drop
///   counter is incremented before the error is returned.
/// * The carrier's tag is not `Pty` (fail-closed — a dynamic ceremony that
///   expected PTY bytes will not silently lift other origins). No counter
///   is incremented in that case; this failure mode is a programmer error
///   at the caller, not a Top drop.
///
/// Tag-mismatch callers that need to distinguish "wrong origin" from "Top"
/// should call [`DynProvenance::try_as`] first.
pub fn try_authorize_pty_to_host_dyn<T>(
    x: DynProvenance<T>,
    cap: HostAuthorizationToken<'_>,
    subsystem: Subsystem,
) -> Result<Provenance<T, Host>, UnliftableTop> {
    match x.drop_if_top(subsystem) {
        None => Err(UnliftableTop::Top),
        Some(dp) => match dp.try_as::<Pty>() {
            Ok(p) => Ok(authorize_pty_to_host(p, cap)),
            Err(_) => Err(UnliftableTop::Top),
        },
    }
}

/// Lift a `DynProvenance<T>` whose runtime tag is `NetworkUntrusted` to
/// `Provenance<T, Host>`.
///
/// See [`try_authorize_pty_to_host_dyn`] for the `Top` drop semantics. Both
/// ceremonies increment the same per-subsystem counter since §7.3 tracks
/// drops by consuming subsystem, not by the incoming origin.
///
/// # Errors
///
/// * [`UnliftableTop::Top`] when the carrier is `Top`. The metric is incremented
///   in that case.
pub fn try_authorize_network_to_host_dyn<T>(
    x: DynProvenance<T>,
    cap: NetworkAuthorizationToken<'_>,
    subsystem: Subsystem,
) -> Result<Provenance<T, Host>, UnliftableTop> {
    match x.drop_if_top(subsystem) {
        None => Err(UnliftableTop::Top),
        Some(dp) => match dp.try_as::<NetworkUntrusted>() {
            Ok(p) => Ok(authorize_network_to_host(p, cap)),
            Err(_) => Err(UnliftableTop::Top),
        },
    }
}
