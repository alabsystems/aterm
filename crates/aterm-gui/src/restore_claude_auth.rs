// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_agent::harness::profile::{Context, Selection};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Captured attribution, not a claim that the provider still accepts a credential.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub(crate) struct ClaudeAuthRestore {
    pub stored_login: bool,
    /// The entries cold account admission reads of the environment its shell passed it
    /// ([`aterm_agent::harness::accounts::LaunchEnv`]): directories and selectors, never a
    /// credential. Absent from a record of an older build or of an unread environment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_env: Option<Vec<String>>,
    pub profile: Option<ProfileRestore>,
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
