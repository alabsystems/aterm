// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_agent::harness::accounts::{AuthRoute, TokenEntry};
use aterm_agent::harness::profile::{Context, Selection};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Captured attribution, not a claim that the provider still accepts a credential.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub(crate) struct ClaudeAuthRestore {
    /// How the session authenticated ([`Self::route`]): `"stored"`, `"outside"`, or a token
    /// route with its entry (`docs/DESIGN-claude-setup-token-2026-11-05.md` §5.4). Absent from a
    /// record of v0.157.0 and older, which carried [`Self::stored_login`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    route: Option<RouteRestore>,
    /// The route as v0.157.0 and older wrote it: `true` a stored login, `false` outside. Read
    /// where [`Self::route`] is absent. [`Self::new`] never sets it, so only an older record
    /// read back carries it again (the handoff generator's `NO_HANDOFF_WRITES` retires it; the
    /// shipped desks keep the reader to it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stored_login: Option<bool>,
    /// The entries cold account admission reads of the environment its shell passed it
    /// ([`aterm_agent::harness::accounts::LaunchEnv`]): directories and selectors, never a
    /// credential. Absent from a record of an older build or of an unread environment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_env: Option<Vec<String>>,
    pub profile: Option<ProfileRestore>,
}

/// A route as the record holds it, apart from [`AuthRoute`] so the record's words stay its own.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
enum RouteRestore {
    /// [`AuthRoute::Stored`].
    Stored,
    /// [`AuthRoute::Outside`].
    Outside,
    /// [`AuthRoute::VaultToken`]: a session the token wrapper served, written since the token
    /// route went on in every build (S13).
    Token {
        /// The token entry the marker named.
        entry: TokenEntry,
    },
}

impl ClaudeAuthRestore {
    /// The record of a session that authenticated by `route`.
    pub(crate) fn new(
        route: AuthRoute,
        launch_env: Option<Vec<String>>,
        profile: Option<ProfileRestore>,
    ) -> Self {
        Self {
            route: Some(match route {
                AuthRoute::Stored => RouteRestore::Stored,
                AuthRoute::VaultToken { entry } => RouteRestore::Token { entry },
                AuthRoute::Outside => RouteRestore::Outside,
            }),
            stored_login: None,
            launch_env,
            profile,
        }
    }

    /// How the session authenticated: its `route`, else an older build's `stored_login` (`true`
    /// stored, `false` outside), else outside.
    pub(crate) fn route(&self) -> AuthRoute {
        match (self.route, self.stored_login) {
            (Some(RouteRestore::Stored), _) | (None, Some(true)) => AuthRoute::Stored,
            (Some(RouteRestore::Token { entry }), _) => AuthRoute::VaultToken { entry },
            (Some(RouteRestore::Outside), _) | (None, Some(false) | None) => AuthRoute::Outside,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub(crate) struct ProfileRestore {
    plain: PathBuf,
    records: PathBuf,
    selection: Filename,
    home: Option<String>,
    config: Option<String>,
    login: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
enum Filename {
    Unknown,
    Standard,
    CustomOauth,
}

#[cfg(test)]
#[path = "restore_claude_auth_tests.rs"]
mod tests;

impl From<Context> for ProfileRestore {
    fn from(profile: Context) -> Self {
        Self {
            plain: profile.plain,
            records: profile.records,
            selection: match profile.selection {
                Selection::Unknown => Filename::Unknown,
                Selection::Standard => Filename::Standard,
                Selection::CustomOauth => Filename::CustomOauth,
            },
            home: profile.home,
            config: profile.config,
            login: profile.login,
        }
    }
}

impl From<ProfileRestore> for Context {
    fn from(profile: ProfileRestore) -> Self {
        Self {
            plain: profile.plain,
            records: profile.records,
            selection: match profile.selection {
                Filename::Unknown => Selection::Unknown,
                Filename::Standard => Selection::Standard,
                Filename::CustomOauth => Selection::CustomOauth,
            },
            home: profile.home,
            config: profile.config,
            login: profile.login,
        }
    }
}
