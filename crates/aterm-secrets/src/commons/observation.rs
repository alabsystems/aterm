// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Reasons an offline vault observation cannot supply Commons control. None of
//! these is evidence that a network is unused or may safely be initialized.

use super::Snapshot;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unavailable {
    Busy,
    Unconfigured,
    UnsignedSource,
    NotAdmitted,
    Revoked,
    IncompleteApply,
    SourceChanged,
    UntrustedPin,
    MissingIdentity,
    MissingStateLock,
    MissingCache,
    PendingApply,
    ControlNotVisible,
}

impl Unavailable {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Busy => "vault-busy",
            Self::Unconfigured => "vault-unconfigured",
            Self::UnsignedSource => "vault-unsigned",
            Self::NotAdmitted => "vault-not-admitted",
            Self::Revoked => "vault-revoked",
            Self::IncompleteApply => "vault-apply-incomplete",
            Self::SourceChanged => "vault-source-changed",
            Self::UntrustedPin => "vault-pin-untrusted",
            Self::MissingIdentity => "vault-identity-missing",
            Self::MissingStateLock => "vault-state-lock-missing",
            Self::MissingCache => "vault-cache-missing",
            Self::PendingApply => "vault-apply-pending",
            Self::ControlNotVisible => "control-not-visible",
        }
    }

    pub const fn explanation(self) -> &'static str {
        match self {
            Self::Busy => "vault state is busy; retry after its current operation",
            Self::Unconfigured => "no secrets vault is configured for this installation",
            Self::UnsignedSource => "the configured secrets source has no trusted signing pin",
            Self::NotAdmitted => "this installation has not been admitted to its secrets vault",
            Self::Revoked => "this installation's secrets vault membership is revoked",
            Self::IncompleteApply => {
                "the local vault apply is incomplete; sync the configured vault"
            }
            Self::SourceChanged => {
                "the applied vault state does not match its configured source; sync the vault"
            }
            Self::UntrustedPin => "the configured vault signing pin is not admitted",
            Self::MissingIdentity => {
                "the enrolled vault identity is missing; restore its custody before using Commons"
            }
            Self::MissingStateLock => {
                "the vault observation lock is missing; sync the configured vault before using Commons"
            }
            Self::MissingCache => {
                "the authenticated vault cache is missing; sync the configured vault"
            }
            Self::PendingApply => {
                "the cached vault version is not fully applied; sync the configured vault"
            }
            Self::ControlNotVisible => {
                "no Commons control entry is visible to this installation; it may be unconfigured or addressed to other recipients"
            }
        }
    }
}

/// One observation under the vault lock. A visible snapshot still requires the
/// Commons schema, immutable pins, device custody and current membership checks.
#[derive(Clone, Debug)]
pub enum Observation {
    Unavailable(Unavailable),
    Visible(Snapshot),
}

impl Observation {
    pub fn into_snapshot(self) -> Option<Snapshot> {
        match self {
            Self::Visible(snapshot) => Some(snapshot),
            Self::Unavailable(_) => None,
        }
    }
}
