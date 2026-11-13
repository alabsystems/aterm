// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE APPLIED REGISTRY, VALUE-FREE: each name this Mac's vault registry holds, where its file
//! lands (its pointer) and what it is for (its purpose), as `aterm secrets ls --describe` lists
//! them. Reading it opens `state.toml` alone: never the vault identity, a materialized file or a
//! value. The harness reads it before it lets a `NEEDS YOU` line about a credential reach a person
//! (`aterm_agent::supervise::policy::vault_need`).

use crate::need::catalog::{GhProfile, NOTARY_ENTRY, SITE_DEPLOYER_KEY_ENTRY, Want};

/// One registry entry, value-free.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The entry's name in the registry.
    pub name: String,
    /// Where its file lands (`~/…`), `None` for an entry with no file.
    pub pointer: Option<String>,
    /// What it is for.
    pub purpose: String,
}

/// What entry `name` is for.
#[must_use]
pub fn purpose(name: &str) -> String {
    match name {
        "gh_access_token" => Want::Gh(GhProfile::Dev).title(),
        "gh_access_token_alabsystems" => Want::Gh(GhProfile::Release).title(),
        "github-repositories.json" => {
            "Private development account and repository inventory for automatic setup".into()
        }
        NOTARY_ENTRY => Want::Notary.title(),
        SITE_DEPLOYER_KEY_ENTRY => Want::SiteDeployerKey.title(),
        "~/.aterm/release-ring/release-credentials.toml" => {
            "Release signing profile; use with --release-credentials".into()
        }
        "~/.aterm/release-ring/machine.key" => "Machine release signing key".into(),
        "~/.aterm/release-ring/machine.toml" => "Machine release identity".into(),
        _ if crate::entry_class::LEGACY_RELEASE_RING.contains(&name)
            || crate::entry_class::LEGACY_ROSTER.contains(&name) =>
        {
            "0.145 duplicate of its ~/.aterm/release-ring/ twin, read by nothing; recorded for \
             retirement (ATERM-KI-1112-980)"
                .into()
        }
        "ai-accounts.csv" => {
            "AI account sheet (the lab importer's input; read directly only on a Mac the lab does \
             not name)"
                .into()
        }
        "ai-mailbox.toml" => "AI account mailbox configuration".into(),
        "~/.config/gh/hosts.yml" => "GitHub CLI authentication".into(),
        "~/.config/git/credentials" => "Git HTTPS credentials".into(),
        "~/.config/git/config" => "Git configuration and credential routing".into(),
        _ if name.ends_with(".cer") && name.starts_with("~/.aterm/apple/") => {
            "Apple Developer ID certificate".into()
        }
        _ if name.ends_with(".key") && name.starts_with("~/.aterm/apple/") => {
            "Apple Developer ID private key".into()
        }
        _ if name.starts_with("~/.aterm/release-ring/roster/") => {
            "Release machine roster or its signature".into()
        }
        _ => Want::Entry(name.to_owned()).title(),
    }
}

/// Where entry `name`'s file lands, as `~/…`; `None` for an entry the materializer writes no file
/// for (a reserved entry, a login file it refuses).
#[must_use]
pub fn pointer(name: &str) -> Option<String> {
    // Reuse the materializer's eligibility rules, including reserved entries and login
    // files it deliberately refuses. The synthetic home is never opened.
    crate::materialize::target_path(std::path::Path::new("~"), name)
        .ok()
        .map(|path| path.to_string_lossy().into_owned())
}

/// Entry `name`, described.
#[must_use]
pub fn entry(name: &str) -> Entry {
    Entry {
        name: name.to_owned(),
        pointer: pointer(name),
        purpose: purpose(name),
    }
}

/// Why the registry could not be read ([`read`]).
pub const NO_REGISTRY: &str = "this Mac has applied no vault registry (no state.toml)";

/// EVERY ENTRY THIS MAC'S APPLIED REGISTRY HOLDS, described, read from `state.toml` alone.
///
/// # Errors
/// Why the registry cannot be read: this Mac applied none ([`NO_REGISTRY`]: it joined no vault,
/// or its first round has not applied), or `state.toml` cannot be read or parsed. A registry that
/// cannot be read says nothing of what the vault holds, so a caller never takes it as empty.
pub fn read(paths: &crate::client::Paths) -> Result<Vec<Entry>, String> {
    match std::fs::symlink_metadata(paths.state_file()) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(NO_REGISTRY.to_owned()),
        Err(e) => return Err(format!("state.toml cannot be read: {e}")),
    }
    let names = crate::client::registered_names(paths).map_err(|e| e.to_string())?;
    Ok(names.iter().map(|name| entry(name)).collect())
}

#[cfg(test)]
#[path = "registry_tests.rs"]
mod tests;
