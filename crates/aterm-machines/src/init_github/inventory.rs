// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Development repository metadata belongs to the private vault. The signed vault delivers
//! this file beside the development credential; setup never enumerates a public account to
//! infer the repositories a machine should have.

use super::repos;
use crate::model::User;
use crate::port::System;
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// The vault entry the inventory arrives as ([`aterm_secrets::entry_class::GITHUB_REPOSITORIES`]).
pub(super) const ENTRY: &str = aterm_secrets::entry_class::GITHUB_REPOSITORIES;

/// Where the vault writes the inventory under the home `home`.
pub(super) fn file(home: &str) -> PathBuf {
    Path::new(home).join(".secrets").join(ENTRY)
}
pub(super) const MAX_BYTES: usize = 1024 * 1024;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawInventory {
    schema: u32,
    account: String,
    repositories: Vec<aterm_json::Value>,
}

/// An existing administrator-published catalog, including policy the provider does not own.
/// Parsing never invents the account of a missing or malformed catalog.
pub struct RepositoryCatalog {
    raw: RawInventory,
    owner_id: Option<u64>,
}

/// What the provider observation proves, independently of its complete pagination.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepositoryCatalogVisibility {
    /// All records this token can see. Unseen saved records and their policy stay untouched.
    TokenVisible,
    /// Independently proved whole-account visibility permits retiring absent records.
    AccountComplete,
}

impl RepositoryCatalog {
    /// Capture an existing administrator catalog and any numeric account binding in its rows.
    /// An empty catalog retains its declared account for a fresh authenticated observation.
    /// # Errors
    /// The catalog is unsafe, ambiguous, or larger than the delivery budget.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_BYTES {
            return Err("repository catalog exceeds its delivery budget".into());
        }
        let raw: RawInventory =
            aterm_json::from_slice(bytes).map_err(|_| "repository catalog cannot be decoded")?;
        Inventory::try_from(raw.clone())?;
        let mut owner_id = None;
        let mut ids = std::collections::BTreeSet::new();
        for repo in &raw.repositories {
            let id = number(repo, "id")?;
            let owner = repo.get("owner").ok_or("repository has no owner")?;
            let current = number(owner, "id")?;
            if !owner["login"].as_str().is_some_and(super::login)
                || !ids.insert(id)
                || owner_id.is_some_and(|old| old != current)
            {
                return Err("repository catalog has an ambiguous owned-account binding".into());
            }
            owner_id = Some(current);
        }
        Ok(Self { raw, owner_id })
    }

    /// The saved account spelling; a provider observation must also prove its numeric identity.
    #[must_use]
    pub fn account(&self) -> &str {
        &self.raw.account
    }

    /// Numeric owner identity retained in the existing catalog's repository objects.
    #[must_use]
    pub const fn owner_id(&self) -> Option<u64> {
        self.owner_id
    }

    /// Refresh provider metadata under the caller's proved visibility and publication authority.
    /// A complete token-visible snapshot retains unseen saved rows; a proved whole-account
    /// snapshot can retire absent IDs. Policy on each retained ID survives repository renames.
    /// # Errors
    /// Any row is malformed, duplicated, foreign, conflicting, or exceeds the delivery budget.
    pub fn refresh(
        &self,
        account: &str,
        owner_id: u64,
        observed: &[aterm_json::Value],
        visibility: RepositoryCatalogVisibility,
    ) -> Result<Vec<u8>, String> {
        use aterm_json::Value;
        if !super::login(account)
            || owner_id == 0
            || self.owner_id.is_some_and(|old| old != owner_id)
            || (self.owner_id.is_none() && !account.eq_ignore_ascii_case(&self.raw.account))
        {
            return Err("provider account differs from the administrator's catalog binding".into());
        }
        if visibility == RepositoryCatalogVisibility::TokenVisible
            && !account.eq_ignore_ascii_case(&self.raw.account)
        {
            return Err("a partial account rename cannot relabel unseen repository policy".into());
        }
        let old: std::collections::BTreeMap<_, _> = self
            .raw
            .repositories
            .iter()
            .map(|repo| (repo["id"].as_u64().expect("parsed catalog id"), repo))
            .collect();
        let mut ids = std::collections::BTreeSet::new();
        let mut repositories = Vec::new();
        for repo in observed {
            let id = number(repo, "id")?;
            if !ids.insert(id)
                || number(&repo["owner"], "id")? != owner_id
                || !repo["owner"]["login"]
                    .as_str()
                    .is_some_and(|login| login.eq_ignore_ascii_case(account))
            {
                return Err("provider inventory contains duplicate or foreign identities".into());
            }
            for field in ["private", "fork", "archived"] {
                if repo[field].as_bool().is_none() {
                    return Err("provider inventory has an unknown repository disposition".into());
                }
            }
            for field in ["name", "full_name", "clone_url", "default_branch"] {
                if repo[field].as_str().is_none() {
                    return Err("provider inventory lacks required repository metadata".into());
                }
            }
            if repo["size"].as_u64().is_none() {
                return Err("provider inventory lacks a repository size".into());
            }
            // These are the provider-owned fields consumed by setup. Everything else in the
            // saved object belongs to the administrator, including future unknown policy.
            let mut merged = old
                .get(&id)
                .and_then(|repo| repo.as_object())
                .cloned()
                .unwrap_or_default();
            // Owner identity is provider metadata too; saved unknown fields nested beside
            // that identity must survive just like other saved policy on this repository.
            let mut owner = merged
                .get("owner")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            for field in ["id", "login"] {
                owner.insert(field.into(), repo["owner"][field].clone());
            }
            if let Some(kind) = repo["owner"].get("type") {
                owner.insert("type".into(), kind.clone());
            }
            merged.insert("owner".into(), Value::Object(owner));
            for field in [
                "id",
                "name",
                "full_name",
                "private",
                "clone_url",
                "default_branch",
                "size",
                "fork",
                "archived",
            ] {
                merged.insert(field.into(), repo[field].clone());
            }
            repositories.push(Value::Object(merged));
        }
        if visibility == RepositoryCatalogVisibility::TokenVisible {
            repositories.extend(
                self.raw
                    .repositories
                    .iter()
                    .filter(|repo| !ids.contains(&repo["id"].as_u64().expect("parsed id")))
                    .cloned(),
            );
        }
        repositories.sort_by_key(|repo| repo["id"].as_u64());
        let value = aterm_json::json!({
            "schema": 2, "account": account, "repositories": repositories,
        });
        let checked: RawInventory = aterm_json::from_value(value.clone())
            .map_err(|_| "refreshed repository catalog cannot be decoded")?;
        Inventory::try_from(checked)?;
        let bytes =
            aterm_json::to_vec(&value).map_err(|_| "repository catalog cannot be encoded")?;
        if bytes.len() > MAX_BYTES {
            return Err("repository catalog exceeds its delivery budget".into());
        }
        Ok(bytes)
    }
}

fn number(value: &aterm_json::Value, key: &str) -> Result<u64, String> {
    value[key]
        .as_u64()
        .filter(|id| *id != 0)
        .ok_or_else(|| "repository inventory lacks a numeric identity".into())
}

/// Only a complete validated private list can be retained by a captured source.
#[derive(Clone)]
pub(super) struct Inventory(Vec<repos::Repo>);

impl Inventory {
    pub(super) fn read(sys: &dyn System, user: &User) -> Result<Self, String> {
        let bytes = sys
            .read_owned_file(&file(&user.home), user.uid, MAX_BYTES, true)
            .map_err(|_| {
                "repository metadata is not ready in the Secrets vault; waiting for its private github-repositories.json entry".to_owned()
            })?;
        let inventory: RawInventory = aterm_json::from_slice(&bytes)
            .map_err(|_| "the vault's repository metadata is not valid JSON".to_owned())?;
        inventory.try_into()
    }

    pub(super) fn repositories(&self) -> &[repos::Repo] {
        &self.0
    }

    pub(super) fn into_repositories(self) -> Vec<repos::Repo> {
        self.0
    }
}

impl TryFrom<RawInventory> for Inventory {
    type Error = String;

    fn try_from(raw: RawInventory) -> Result<Self, Self::Error> {
        if raw.schema != 2 || !super::login(&raw.account) {
            return Err("the vault's repository metadata has an unsupported schema or invalid development account".into());
        }
        let listed =
            repos::inventory_filtered(&raw.repositories, None, Some(&raw.account), |repo| {
                !repo.fork && !repo.archived
            })?;
        if !listed.notes.is_empty() {
            return Err(format!(
                "the vault's repository metadata has {} conflicting destination(s) among {} intended repositories; fix the private inventory before cloning",
                listed.notes.len(),
                listed.repositories.len() + listed.notes.len()
            ));
        }
        Ok(Self(listed.repositories))
    }
}

#[cfg(test)]
#[path = "inventory_tests.rs"]
mod tests;
