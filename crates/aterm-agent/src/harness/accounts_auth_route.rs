// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! How a Claude Code process authenticates ([`AuthRoute`]) and the vault entry a token route
//! names ([`TokenEntry`]) (`docs/DESIGN-claude-setup-token-2026-11-05.md` §5.2).
//!
//! [`super::ProcessLogin::of_env`] reads the route's shape alone, with no roster. A gate that
//! resolves the account then keeps [`AuthRoute::VaultToken`] only where the marker names that
//! account's entry ([`AuthRoute::for_account`]); a marker naming another entry reads as
//! [`AuthRoute::Outside`] there. Each reader decides the token route by its role:
//!
//! - **attribution** counts it as the row's ([`AuthRoute::on_account`], after
//!   [`AuthRoute::for_account`]);
//! - **launch paths** carry it (a `Token` execution intent);
//! - **login services** (renewal, the sign-in lock and ladder, the login latch, the profile reads,
//!   the `/usage` probe) leave it out ([`AuthRoute::is_stored`]);
//! - **other CLIs** never produce it, and read it as [`AuthRoute::Outside`].

use super::super::journal;
use super::{Cli, Row};
use aterm_secrets::entry_class::CLAUDE_TOKEN_PREFIX;

/// A Claude Code token's vault entry, `aterm-claude-token-ac-<16 hex>`, held as its account's lab
/// key, so it is `Copy` and spells only an entry of the token class (never its `-meta` record).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TokenEntry(journal::Key);

impl TokenEntry {
    /// The entry `name` spells, where it is a token entry (not its record, not a sub-account key).
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::of_key(journal::Key::new(name.strip_prefix(CLAUDE_TOKEN_PREFIX)?)?)
    }

    /// The entry of the account whose lab key is `key`, where it is an account's (`ac-`).
    #[must_use]
    pub fn of_key(key: journal::Key) -> Option<Self> {
        key.as_str().starts_with("ac-").then_some(Self(key))
    }

    /// The entry of a Claude Code roster row's account, where the lab keys it (a journal is
    /// installed) and the row names an email.
    #[must_use]
    pub fn of_row(row: &Row) -> Option<Self> {
        Self::of_key(journal::account_key(Cli::Claude, row)?)
    }

    /// The entry of the Claude Code account whose login email is `email`, where the lab keys it.
    #[must_use]
    pub fn of_email(email: &str) -> Option<Self> {
        Self::of_key(journal::claude_email_key(email)?)
    }

    /// The account's lab key.
    #[must_use]
    pub const fn key(self) -> journal::Key {
        self.0
    }

    /// The entry's name in the vault.
    #[must_use]
    pub fn name(self) -> String {
        format!("{CLAUDE_TOKEN_PREFIX}{}", self.0.as_str())
    }
}

/// The name: it is value-free.
impl std::fmt::Debug for TokenEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.name())
    }
}

/// The name: it is value-free.
impl std::fmt::Display for TokenEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.name())
    }
}

/// Its name, as a record or a same-build wire holds it.
impl serde::Serialize for TokenEntry {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.name())
    }
}

/// Only a name [`TokenEntry::parse`] reads.
impl<'de> serde::Deserialize<'de> for TokenEntry {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = <String as serde::Deserialize>::deserialize(deserializer)?;
        Self::parse(&name).ok_or_else(|| serde::de::Error::custom("not a token entry's name"))
    }
}

/// How a Claude Code process authenticates, read from its exec-time environment (§5.2).
///
/// Its default is [`Self::Outside`], what an unread environment is.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum AuthRoute {
    /// A stored login in the row's login directory.
    Stored,
    /// The vault token aterm's wrapper served it: the descriptor variable, the marker naming
    /// `entry`, the row's login and home, and no other credential, profile or route variable.
    VaultToken {
        /// The token entry the marker names.
        entry: TokenEntry,
    },
    /// Anything else: an environment token, an API key, a provider route, a settings override.
    #[default]
    Outside,
}

impl AuthRoute {
    /// This route, or [`Self::Outside`] where `off` (a finding that moves a process off its
    /// account's own credential). It never grants a route.
    #[must_use]
    pub const fn unless(self, off: bool) -> Self {
        if off { Self::Outside } else { self }
    }

    /// The route at a gate that resolved the process's account to the one whose token entry is
    /// `entry` (`None`: an account with no entry the lab can key): a token route stays one only
    /// where its marker names that entry, and reads as [`Self::Outside`] otherwise.
    #[must_use]
    pub fn for_account(self, entry: Option<TokenEntry>) -> Self {
        match self {
            Self::Stored => Self::Stored,
            Self::VaultToken { entry: named } if Some(named) == entry => self,
            Self::VaultToken { .. } => Self::Outside,
            Self::Outside => Self::Outside,
        }
    }

    /// For attribution, after [`Self::for_account`]: whether the process runs on its account's
    /// own credential, its stored login or its vault token, so its usage, walls and claims count
    /// as the row's.
    #[must_use]
    pub const fn on_account(self) -> bool {
        match self {
            Self::Stored | Self::VaultToken { .. } => true,
            Self::Outside => false,
        }
    }

    /// For a login service (renewal, the sign-in lock and ladder, the login latch, the profile
    /// reads, the `/usage` probe) and for another CLI's readers: whether the process runs on its
    /// stored login. A token route is left out.
    #[must_use]
    pub const fn is_stored(self) -> bool {
        match self {
            Self::Stored => true,
            Self::VaultToken { .. } | Self::Outside => false,
        }
    }

    /// The route as a word, for a record or a report: `stored`, `token` or `outside`.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Stored => "stored",
            Self::VaultToken { .. } => "token",
            Self::Outside => "outside",
        }
    }
}
