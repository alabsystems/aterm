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
        let words: Vec<_> = text.split_ascii_whitespace().collect();
        match words.as_slice() {
            [PREFIX, "applied", seq, failed] => Some(Self::Applied {
                seq: seq.parse().ok()?,
                failed: failed.parse().ok()?,
            }),
            [PREFIX, "unchanged", seq] => Some(Self::Unchanged(seq.parse().ok()?)),
            [PREFIX, "put-back", seq] => Some(Self::PutBack(seq.parse().ok()?)),
            [PREFIX, "not-in"] => Some(Self::NotIn),
            [PREFIX, "failed", code] => Some(Self::Failed(match *code {
                "journal-busy" => ErrorKind::JournalBusy,
                "custody-busy" => ErrorKind::CustodyBusy,
                "local-network-pending" => ErrorKind::LocalNetworkPending,
                "timed-out" => ErrorKind::TimedOut,
                "not-stored" => ErrorKind::NotStored,
                "refused" => ErrorKind::Refused,
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
        ErrorKind::LocalNetworkPending => "local network unavailable or permission pending",
        ErrorKind::TimedOut => "request timed out",
        ErrorKind::NotStored => "source did not store the version",
        ErrorKind::Refused => "source refused the version",
        ErrorKind::Other => "unclassified failure",
    }
}

const fn failure_code(kind: ErrorKind) -> &'static str {
    match kind {
        ErrorKind::JournalBusy => "journal-busy",
        ErrorKind::CustodyBusy => "custody-busy",
        ErrorKind::LocalNetworkPending => "local-network-pending",
        ErrorKind::TimedOut => "timed-out",
        ErrorKind::NotStored => "not-stored",
        ErrorKind::Refused => "refused",
        ErrorKind::Other => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcomes_round_trip_without_free_form_messages() {
        for outcome in [
            SyncDiagnostic::Applied {
                seq: u64::MAX,
                failed: u64::MAX,
            },
            SyncDiagnostic::Unchanged(2),
            SyncDiagnostic::PutBack(3),
            SyncDiagnostic::NotIn,
            SyncDiagnostic::Failed(ErrorKind::JournalBusy),
            SyncDiagnostic::Failed(ErrorKind::CustodyBusy),
            SyncDiagnostic::Failed(ErrorKind::LocalNetworkPending),
            SyncDiagnostic::Failed(ErrorKind::TimedOut),
            SyncDiagnostic::Failed(ErrorKind::NotStored),
            SyncDiagnostic::Failed(ErrorKind::Refused),
            SyncDiagnostic::Failed(ErrorKind::Other),
        ] {
            for ending in ["", "\n", "\r\n"] {
                assert_eq!(
                    SyncDiagnostic::parse(&format!("{}{ending}", outcome.line())),
                    Some(outcome)
                );
            }
        }
    }

    #[test]
    fn unknown_or_extra_data_is_not_a_diagnostic() {
        for text in [
            "aterm-secrets-sync-v2 unchanged 4",
            "aterm-secrets-sync-v1 failed password=private",
            "aterm-secrets-sync-v1 unchanged -1",
            "aterm-secrets-sync-v1 applied 2",
            "aterm-secrets-sync-v1 unchanged 3\napplied seq 99",
            "aterm-secrets-sync-v1\nunchanged 3",
        ] {
            assert_eq!(SyncDiagnostic::parse(text), None);
        }
    }
}
