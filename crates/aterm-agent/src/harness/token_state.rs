// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A Claude token's state on this Mac as plain words (`docs/DESIGN-claude-setup-token-2026-11-05.md`
//! §5.1): the type every surface that shows a token names. The reads that compute a state are
//! Unix-only and live in `accounts_tokens`; the state itself is declared here so a surface on any
//! platform compiles against it.

use aterm_secrets::entry_class::Mismatch;

/// Why a token this Mac minted is not verified yet (§5.6, §5.1's `unverified (<why>)`); it
/// serves this Mac meanwhile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unverified {
    /// No served reading carrying rate limits was compared yet, or the row had no stored-login
    /// reading in a current window to compare it with.
    NoReadingYet,
    /// The token session's windows were not the row's stored login's.
    WindowsDiffer,
    /// They were, and another account's current windows on this Mac were too.
    WindowsShared,
    /// No roster row names the account, so no stored-login reading under a row compares.
    NoRosterRow,
}

impl Unverified {
    /// The reason as §5.1 words it.
    #[must_use]
    pub const fn words(self) -> &'static str {
        match self {
            Self::NoReadingYet => "no reading yet",
            Self::WindowsDiffer => "windows differ",
            Self::WindowsShared => "windows shared",
            Self::NoRosterRow => "no roster row",
        }
    }
}

/// A token's state on this Mac (§5.1), in the order its checks run: the first that applies is
/// the state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TokenState {
    /// The vault could not be read just now (busy, unsigned, failed or mid-apply).
    Unknown,
    /// No entry for the account.
    None,
    /// This Mac opens no epoch the inner seal names.
    Unreadable,
    /// A check failed.
    Mismatched(Mismatch),
    /// Its end has passed.
    Ended,
    /// This Mac saw Claude Code refuse it.
    Rejected,
    /// Not verified yet: it serves only the Mac that minted it.
    Unverified {
        /// This Mac minted it.
        minted_here: bool,
        /// On the minting Mac, why verification has not passed yet.
        why: Unverified,
        /// The Mac that did, as its meta names it.
        minted_on: String,
        /// The removed Macs it was handed to, beside it.
        exposed: Vec<String>,
    },
    /// A Mac the log now removes was handed it.
    Exposed(Vec<String>),
    /// 30 days or fewer until its end.
    Soon,
    /// In use, with more than 30 days left.
    Active,
}

impl TokenState {
    /// The state as a person reads it, value-free.
    #[must_use]
    pub fn words(&self) -> String {
        match self {
            Self::Unknown => "unknown (the vault could not be read just now)".to_owned(),
            Self::None => "none".to_owned(),
            Self::Unreadable => "unreadable here".to_owned(),
            Self::Mismatched(Mismatch::Draft) => "mismatched (draft)".to_owned(),
            Self::Mismatched(Mismatch::Value) => "mismatched (value)".to_owned(),
            Self::Ended => "ended".to_owned(),
            Self::Rejected => "rejected".to_owned(),
            Self::Unverified {
                minted_here,
                why,
                minted_on,
                exposed,
            } => {
                let mut words = if *minted_here {
                    format!("unverified ({})", why.words())
                } else {
                    format!("unverified (made on {minted_on}; serves that Mac until verified)")
                };
                if !exposed.is_empty() {
                    words.push_str(&format!(", exposed (removed {})", exposed.join(", ")));
                }
                words
            }
            Self::Exposed(macs) => format!("exposed (removed {})", macs.join(", ")),
            Self::Soon => "soon".to_owned(),
            Self::Active => "active".to_owned(),
        }
    }

    /// The state as one word with no blank, as a survey row's `token=` and a status line's
    /// `token=` carry it, and as the offer's pair names it (§5.9, §5.10).
    #[must_use]
    pub const fn word(&self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::None => "none",
            Self::Unreadable => "unreadable",
            Self::Mismatched(_) => "mismatched",
            Self::Ended => "ended",
            Self::Rejected => "rejected",
            Self::Unverified { .. } => "unverified",
            Self::Exposed(_) => "exposed",
            Self::Soon => "soon",
            Self::Active => "active",
        }
    }

    /// Whether sessions on this Mac start on the token (§5.1's *in use*): `active`, `soon` or
    /// `exposed` on every Mac, `unverified` on its minting Mac only.
    #[must_use]
    pub const fn in_use(&self) -> bool {
        matches!(
            self,
            Self::Active
                | Self::Soon
                | Self::Exposed(_)
                | Self::Unverified {
                    minted_here: true,
                    ..
                }
        )
    }
}
