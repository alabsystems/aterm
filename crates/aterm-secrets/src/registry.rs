// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE STORE'S REGISTRY, VALUE-FREE: each name this Mac's store holds and what it is for (its
//! purpose), as `aterm secrets ls --describe` lists them. The names come from the verified view
//! ([`crate::client::registered_names`]); no value is returned and no file under the home is read:
//! an entry is read in place by its name (`aterm_secrets::read`). The harness reads it before it
//! lets a `NEEDS YOU` line about a credential reach a person
//! (`aterm_agent::supervise::policy::vault_need`).

use crate::need::catalog::{
    AZURE_SIGNING_PRINCIPAL_ENTRY, FLEET_GH_TOKEN_ENTRY, GhProfile, MACHINES_ADMIN_ENTRY,
    NOTARY_ENTRY, PAPER_MASTER_ENTRY, SITE_DEPLOYER_KEY_ENTRY, Want,
};

/// One registry entry, value-free.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The entry's name in the registry: the key a store read takes.
    pub name: String,
    /// What it is for.
    pub purpose: String,
}

/// What entry `name` is for.
#[must_use]
pub fn purpose(name: &str) -> String {
    match name {
        crate::entry_class::DEV_GH_TOKEN => Want::Gh(GhProfile::Dev).title(),
        crate::entry_class::RELEASE_GH_TOKEN => Want::Gh(GhProfile::Release).title(),
        FLEET_GH_TOKEN_ENTRY => Want::Gh(GhProfile::Fleet).title(),
        MACHINES_ADMIN_ENTRY => Want::MachinesAdmin.title(),
        "github-repositories.json" => {
            "Private development account and repository inventory for automatic setup".into()
        }
        NOTARY_ENTRY => Want::Notary.title(),
        PAPER_MASTER_ENTRY => Want::PaperMaster.title(),
        SITE_DEPLOYER_KEY_ENTRY => Want::SiteDeployerKey.title(),
        AZURE_SIGNING_PRINCIPAL_ENTRY => Want::AzureSigningPrincipal.title(),
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
        crate::entry_class::GOOGLE_MAIL_CLIENT => {
            "aterm's Google desktop client registration (`aterm email connect-gmail`)".into()
        }
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

/// Entry `name`, described.
#[must_use]
pub fn entry(name: &str) -> Entry {
    Entry {
        name: name.to_owned(),
        purpose: purpose(name),
    }
}

/// Why the registry could not be read ([`read`]).
pub const NO_REGISTRY: &str = "this Mac has applied no vault registry (no state.toml)";

/// EVERY ENTRY THIS MAC'S STORE HOLDS, described, value-free.
///
/// # Errors
/// Why the registry could not be read: this Mac applied none ([`NO_REGISTRY`]: it joined no vault,
/// or its first round has not applied), or its verified view cannot be read just now. A registry
/// that cannot be read says nothing of what the vault holds, so a caller never takes it as empty.
pub fn read(paths: &crate::client::Paths) -> Result<Vec<Entry>, String> {
    match std::fs::symlink_metadata(paths.state_file()) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(NO_REGISTRY.to_owned()),
        Err(e) => return Err(format!("state.toml cannot be read: {e}")),
    }
    let names = crate::client::registered_names(paths).map_err(|e| e.to_string())?;
    Ok(names.iter().map(|name| entry(name)).collect())
}

/// THE ENTRY THAT HOLDS `want` HERE: the first of its entries ([`Want::entries`]) this Mac's
/// store holds, by name, for a store read (`aterm_secrets::read`). `Ok(None)` where the store
/// holds none of them. No value is read.
///
/// # Errors
/// As [`read`]: the registry cannot be read.
pub fn held(paths: &crate::client::Paths, want: &Want) -> Result<Option<String>, String> {
    let names = want.entries();
    let registry = read(paths)?;
    Ok(names
        .into_iter()
        .find(|name| registry.iter().any(|entry| entry.name == *name)))
}

/// `want`'S VALUE, ON THE ONE PATH: the entry [`held`] names, read in place from this Mac's store
/// and judged by `check` (which answers the value or why it is unusable, naming no part of it);
/// where the registry cannot be read, names none of `want`'s entries, or the value is absent or
/// refused, `want` is filed for `purpose` ([`crate::need::blocked`]).
///
/// # Errors
/// The [`crate::need::blocked`] line: why this Mac holds no usable value and the request filed.
pub fn resolve<T>(
    paths: &crate::client::Paths,
    want: &Want,
    purpose: crate::need::Purpose,
    check: impl FnOnce(&str, crate::SecretBytes) -> Result<T, String>,
) -> Result<T, String> {
    let why = match held(paths, want) {
        Ok(Some(name)) => match crate::client::read_in_place(paths, &name) {
            Ok(Some(value)) => match check(&name, value) {
                Ok(value) => return Ok(value),
                Err(why) => why,
            },
            Ok(None) => format!("the vault holds no {name} here"),
            Err(error) => format!("the vault entry {name} cannot be read here ({error})"),
        },
        Ok(None) => "the vault registry names none".to_owned(),
        Err(why) => why,
    };
    Err(crate::need::blocked(paths, want.clone(), purpose, &why))
}

#[cfg(test)]
#[path = "registry_tests.rs"]
mod tests;
