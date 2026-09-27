// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The `authorize_pty_to_host` ceremony and its capability token — no production
//! caller (HARDCORE_BACKLOG P4, parked 2026-09-25). See the provenance design §4.4.

use core::marker::PhantomData;

use crate::origin::{Host, Pty};
use crate::provenance::Provenance;

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
