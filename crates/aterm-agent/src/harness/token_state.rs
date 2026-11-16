// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A Claude token's state on this Mac as plain words (`docs/DESIGN-claude-setup-token-2026-11-05.md`
//! §5.1): the type every surface that shows a token names. The reads that compute a state are
//! Unix-only and live in `accounts_tokens`; the state itself is declared here so a surface on any
//! platform compiles against it.

use aterm_secrets::entry_class::{MintedBy, Mismatch};

/// Why a token is not verified yet (§5.6, §5.1's `unverified (<why>)`), as this Mac's own
/// comparison stands. It serves this Mac meanwhile where its route serves before verification
/// ([`TokenState::in_use`]), unless its windows differ here or, while this account's stored login
/// is usable here, it awaits a basis (the route's hold, `token_route::standing_judged`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unverified {
    /// This Mac's row recorded no basis that names a week still running: no session on the
    /// account's stored login here has reported its windows since the token was made, or the
    /// last one's week has ended. A reading of the token now would be compared with nothing, so
    /// where the stored login is usable here sessions stay on it until one reports.
    AwaitingBasis,
    /// The row holds a current basis, and no served reading of the token carrying rate limits
    /// was compared with it yet.
    NoReadingYet,
    /// The token session's windows were not the basis this Mac's row recorded: this Mac starts no
    /// session on the token, and journaled `token-differ`. Another Mac's comparison is its own,
    /// and the token stays in use there.
    WindowsDiffer,
    /// They were, and another account's current windows on this Mac were too, so the comparison
    /// could not tell the two accounts apart and no verification follows that week (a basis of a
    /// later week reopens it): the token serves where its route puts it in use, and one a
    /// person's terminal made (or whose meta names no route) serves only its minting Mac, whose
    /// surfaces offer its replacement.
    WindowsShared,
    /// They were, and no other account's were: this Mac's `verified` stamp is on its way to the
    /// vault, whichever Mac minted the token.
    WindowsEqual,
    /// No roster row names the account, so no stored-login reading under a row compares.
    NoRosterRow,
}

impl Unverified {
    /// The reason as §5.1 words it.
    #[must_use]
    pub const fn words(self) -> &'static str {
        match self {
            Self::AwaitingBasis => "awaiting a stored-login reading",
            Self::NoReadingYet => "no reading yet",
            Self::WindowsDiffer => "windows differ",
            Self::WindowsShared => "windows shared",
            Self::WindowsEqual => "windows equal",
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
    Rejected {
        /// The vault's records put it in use on another Mac
        /// ([`aterm_secrets::claude_token::Standing::serves_elsewhere`]), so a new token in its
        /// place replaces theirs too.
        elsewhere: bool,
    },
    /// Not verified yet. A token aterm's window or headless route minted proved its account at
    /// its mint, so every member Mac starts sessions on it; one minted at a person's terminal, or
    /// whose meta names no route, only the Mac that minted it. Not a Mac whose own comparison
    /// found its windows different, nor one that awaits a basis while this account's stored
    /// login is usable there.
    Unverified {
        /// This Mac minted it.
        minted_here: bool,
        /// The route that minted it, as its meta names it (`None`: no route this build knows).
        minted_by: Option<MintedBy>,
        /// Why verification has not passed yet, as this Mac's comparison stands.
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
            Self::Rejected { .. } => "rejected".to_owned(),
            Self::Unverified {
                minted_here,
                minted_by,
                why,
                minted_on,
                exposed,
            } => {
                let mut words = if *minted_here {
                    format!("unverified ({})", why.words())
                } else if MintedBy::serves_unverified(*minted_by, false) {
                    format!("unverified (made on {minted_on}; {})", why.words())
                } else {
                    format!("unverified (made on {minted_on}; awaits verification there)")
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
            Self::Rejected { .. } => "rejected",
            Self::Unverified { .. } => "unverified",
            Self::Exposed(_) => "exposed",
            Self::Soon => "soon",
            Self::Active => "active",
        }
    }

    /// Whether sessions on this Mac may start on the token (§5.1's *in use*): `active`, `soon` or
    /// `exposed` on every Mac; `unverified` on every Mac where aterm's window or headless route
    /// minted it (that mint proved the account), and on its minting Mac only where a person's
    /// terminal minted it or its meta names no route ([`MintedBy::serves_unverified`]); and
    /// there only while this Mac's comparison has not found its windows different from its
    /// row's basis (§5.6): such a token may be another account's, so this Mac starts no session
    /// on it, and a session running on it here moves back where this Mac holds the account's
    /// stored login. A token that awaits a basis ([`Unverified::AwaitingBasis`]) is in use here
    /// by this state; the route's judgment holds it while this account's stored login is usable
    /// here (`token_route::standing_judged`), which this state does not know.
    #[must_use]
    pub const fn in_use(&self) -> bool {
        match self {
            Self::Active | Self::Soon | Self::Exposed(_) => true,
            Self::Unverified {
                minted_here,
                minted_by,
                why,
                ..
            } => {
                MintedBy::serves_unverified(*minted_by, *minted_here)
                    && !matches!(why, Unverified::WindowsDiffer)
            }
            Self::Unknown
            | Self::None
            | Self::Unreadable
            | Self::Mismatched(_)
            | Self::Ended
            | Self::Rejected { .. } => false,
        }
    }
}
