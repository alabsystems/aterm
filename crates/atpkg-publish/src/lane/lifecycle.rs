// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The lab's lifecycle authority, as a lane reads it: `registry/repos.toml` on the publication
//! engine's main. A lane builds and publishes a repository only while that registry says it is
//! registered, active and has an open public tier.

use std::collections::BTreeMap;
use std::path::Path;

use crate::ctx::Ctx;
use crate::host::Slug;

/// The registry's path in the engine's repository.
pub const REGISTRY: &str = "registry/repos.toml";

/// The lifecycle state of a repository that is worked on.
pub const ACTIVE: &str = "active";

/// What the registry says of one repository.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Standing {
    /// Registered, active, public tier open: a lane may build and publish it.
    Active,
    /// `[lifecycle.<repo>] state` is anything but `active` (on-hold, unmaintained, retired,
    /// consolidating).
    Inactive {
        /// The state, as written.
        state: String,
        /// The registry's reason, when it gives one.
        reason: Option<String>,
    },
    /// `[repos.<repo>] public_release = false`: its public tier is closed.
    PublicClosed,
    /// No `[repos.<repo>]` row.
    Unregistered,
}

impl Standing {
    /// Why a lane leaves `repo` alone, or `None` when it may work on it.
    #[must_use]
    pub fn refusal(&self, repo: &str) -> Option<String> {
        match self {
            Self::Active => None,
            Self::Inactive { state, reason } => Some(format!(
                "{repo} is `{state}` in the lifecycle registry{} — excluded from builds and \
                 releases until the owner changes that",
                reason
                    .as_deref()
                    .map(|r| format!(" ({r})"))
                    .unwrap_or_default()
            )),
            Self::PublicClosed => Some(format!(
                "{repo}'s public tier is closed (public_release = false in the registry) — \
                 never promoted, pinned or published"
            )),
            Self::Unregistered => Some(format!("{repo} is not a repository the registry manages")),
        }
    }
}

/// The registry's lifecycle and public-tier facts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Registry {
    /// Registered repository → whether its public tier is open.
    repos: BTreeMap<String, bool>,
    /// Repository → its recorded state and reason.
    lifecycle: BTreeMap<String, (String, Option<String>)>,
}

impl Registry {
    /// Read `registry/repos.toml`'s text.
    /// # Errors
    /// Why it is not a registry this lane can decide by: text that is not TOML, no `[repos]`
    /// table, a `public_release` that is not a boolean, or a lifecycle entry with no `state`
    /// string. A fact that cannot be read is never read as "active" or "open".
    pub fn parse(text: &str) -> std::result::Result<Self, String> {
        let doc: aterm_toml::Value =
            aterm_toml::from_str(text).map_err(|e| format!("the registry is not TOML: {e}"))?;
        let repos = doc
            .get("repos")
            .and_then(aterm_toml::Value::as_table)
            .ok_or_else(|| "the registry carries no [repos] table".to_string())?;
        let mut out = Self::default();
        for (name, row) in repos {
            let open = match row.get("public_release") {
                None => true,
                Some(v) => v
                    .as_bool()
                    .ok_or_else(|| format!("[repos.{name}] public_release is not a boolean"))?,
            };
            out.repos.insert(name.clone(), open);
        }
        if let Some(entries) = doc.get("lifecycle") {
            let entries = entries
                .as_table()
                .ok_or_else(|| "the registry's [lifecycle] is not a table".to_string())?;
            for (name, row) in entries {
                let state = row
                    .get("state")
                    .and_then(aterm_toml::Value::as_str)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| format!("[lifecycle.{name}] names no state"))?;
                let reason = row
                    .get("reason")
                    .and_then(aterm_toml::Value::as_str)
                    .map(str::to_string);
                out.lifecycle
                    .insert(name.clone(), (state.to_string(), reason));
            }
        }
        Ok(out)
    }

    /// What the registry says of `repo`. A recorded lifecycle state decides first (the registry
    /// records one for projects it does not otherwise manage), then registration, then the
    /// public tier.
    #[must_use]
    pub fn standing(&self, repo: &str) -> Standing {
        if let Some((state, reason)) = self.lifecycle.get(repo)
            && state != ACTIVE
        {
            return Standing::Inactive {
                state: state.clone(),
                reason: reason.clone(),
            };
        }
        match self.repos.get(repo) {
            None => Standing::Unregistered,
            Some(false) => Standing::PublicClosed,
            Some(true) => Standing::Active,
        }
    }
}

/// The registry on `engine`'s main, read through the staging host into `work` (as the Apple
/// lane reads the engine's ledger), and the main it was read from.
/// # Errors
/// Why the engine's main could not be read, or what makes its registry undecidable.
pub fn read(
    ctx: &Ctx,
    engine: &Slug,
    work: &Path,
) -> std::result::Result<(String, Registry), String> {
    let (at, text) = super::alab::engine_main_file(ctx, engine, work, REGISTRY)?;
    let registry = Registry::parse(&text).map_err(|why| format!("{at}: {why}"))?;
    Ok((at, registry))
}
