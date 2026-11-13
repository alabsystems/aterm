// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded, value-free outcomes for `secrets sync --diagnostic` consumers.

use crate::ErrorKind;

/// The versioned prefix also distinguishes malformed diagnostics from legacy output.
pub const PREFIX: &str = "aterm-secrets-sync-v1";

/// A sync outcome containing only counts and allowlisted failure categories.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncDiagnostic {
    /// A version was applied, possibly with unsuccessful file operations.
    Applied { seq: u64, failed: u64 },
    /// No newer version was available.
    Unchanged(u64),
    /// The local copy was restored to the source; no files were applied locally.
    PutBack(u64),
    /// The source does not admit this machine.
    NotIn,
    /// Sync returned a typed error, without its free-form message.
    Failed(ErrorKind),
}

impl SyncDiagnostic {
    /// Whether this outcome exits successfully.
    pub const fn success(self) -> bool {
        matches!(
            self,
            Self::Applied { failed: 0, .. } | Self::Unchanged(_) | Self::PutBack(_)
        )
    }

    /// One bounded line with no paths, names, URLs or error messages.
    pub fn line(self) -> String {
        match self {
            Self::Applied { seq, failed } => format!("{PREFIX} applied {seq} {failed}"),
            Self::Unchanged(seq) => format!("{PREFIX} unchanged {seq}"),
            Self::PutBack(seq) => format!("{PREFIX} put-back {seq}"),
            Self::NotIn => format!("{PREFIX} not-in"),
            Self::Failed(kind) => format!("{PREFIX} failed {}", failure_code(kind)),
        }
    }

    /// Parse exactly one bounded diagnostic; unknown versions/categories are not evidence.
    pub fn parse(text: &str) -> Option<Self> {
        if text.len() > 128 {
            return None;
        }
        let text = text
            .strip_suffix("\r\n")
            .or_else(|| text.strip_suffix('\n'))
            .unwrap_or(text);
        if text.bytes().any(|byte| byte.is_ascii_control()) {
            return None;
        }
        let uint = |s: &str| {
            (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                .then(|| s.parse().ok())
                .flatten()
        };
        let words: Vec<_> = text.split_ascii_whitespace().collect();
        match words.as_slice() {
            [PREFIX, "applied", seq, failed] => Some(Self::Applied {
                seq: uint(seq)?,
                failed: uint(failed)?,
            }),
            [PREFIX, "unchanged", seq] => Some(Self::Unchanged(uint(seq)?)),
            [PREFIX, "put-back", seq] => Some(Self::PutBack(uint(seq)?)),
            [PREFIX, "not-in"] => Some(Self::NotIn),
            [PREFIX, "failed", code] => Some(Self::Failed(match *code {
                "journal-busy" => ErrorKind::JournalBusy,
                "custody-busy" => ErrorKind::CustodyBusy,
                "vault-busy" => ErrorKind::VaultBusy,
                "vault-changed" => ErrorKind::VaultChanged,
                "membership-capacity" => ErrorKind::MembershipCapacity,
                "membership-history-unavailable" => ErrorKind::MembershipHistoryUnavailable,
                "timed-out" => ErrorKind::TimedOut,
                "join-queue-incomplete" => ErrorKind::JoinQueueIncomplete,
                "not-stored" => ErrorKind::NotStored,
                "refused" => ErrorKind::Refused,
                "member-busy" => ErrorKind::MemberBusy,
                "lab-identity" => ErrorKind::LabIdentity,
                "dropped" => ErrorKind::Dropped,
                "other" => ErrorKind::Other,
                _ => return None,
            })),
            _ => None,
        }
    }
}

/// A safe explanation of what the category establishes, without guessing its underlying cause.
pub const fn failure_reason(kind: ErrorKind) -> &'static str {
    match kind {
        ErrorKind::JournalBusy => "local Commons journal busy",
        ErrorKind::CustodyBusy => "local Commons custody busy",
        ErrorKind::VaultBusy => "local vault state busy",
        ErrorKind::VaultChanged => "local vault observation changed",
        ErrorKind::MembershipCapacity => "membership record capacity exhausted",
        ErrorKind::MembershipHistoryUnavailable => "retained membership history needs repair",
        ErrorKind::TimedOut => "request timed out",
        ErrorKind::JoinQueueIncomplete => "join queues could not all be observed",
        ErrorKind::NotStored => "source did not store the version",
        ErrorKind::Refused => "source refused the version",
        ErrorKind::MemberBusy => "a member's vault pass held its state",
        ErrorKind::LabIdentity => "a lab identity never joins the live ALab",
        ErrorKind::Dropped => "a rotation dropped this Mac from the vault it would join",
        ErrorKind::Other => "unclassified failure",
    }
}

const fn failure_code(kind: ErrorKind) -> &'static str {
    match kind {
        ErrorKind::JournalBusy => "journal-busy",
        ErrorKind::CustodyBusy => "custody-busy",
        ErrorKind::VaultBusy => "vault-busy",
        ErrorKind::VaultChanged => "vault-changed",
        ErrorKind::MembershipCapacity => "membership-capacity",
        ErrorKind::MembershipHistoryUnavailable => "membership-history-unavailable",
        ErrorKind::TimedOut => "timed-out",
        ErrorKind::JoinQueueIncomplete => "join-queue-incomplete",
        ErrorKind::NotStored => "not-stored",
        ErrorKind::Refused => "refused",
        ErrorKind::MemberBusy => "member-busy",
        ErrorKind::LabIdentity => "lab-identity",
        ErrorKind::Dropped => "dropped",
        ErrorKind::Other => "other",
    }
}

#[cfg(test)]
#[path = "sync_diagnostic_tests.rs"]
mod tests;
