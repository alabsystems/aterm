// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Development repository metadata belongs to the private vault. The signed vault delivers
//! this entry beside the development credential, and setup reads it in place from this Mac's
//! store; it never enumerates a public account to infer the repositories a machine should have.

use super::repos;
use crate::model::User;
use crate::port::System;
use serde::Deserialize;
use std::path::Path;

/// The vault entry the inventory arrives as ([`aterm_secrets::entry_class::GITHUB_REPOSITORIES`]).
pub(super) const ENTRY: &str = aterm_secrets::entry_class::GITHUB_REPOSITORIES;

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

/// How a catalog [`RepositoryCatalog::refresh`] made differs from the saved one it was made
/// from, as counts of repositories and two flags: what a publication's log line carries. A
/// repository is in at most one of the counts. One that differs only in a size the saved one
/// stood for, GitHub's being carried by a catalog published for another reason, is in none
/// of them. The two catalogs are equal as JSON exactly where every count is `0` and neither
/// flag is set.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RepositoryCatalogChanges {
    /// In both catalogs, with a field the refreshed one holds, other than `size`, that the
    /// saved one holds at another value or lacks. A key inside `owner` is a field of its own
    /// here and in `pruned`.
    pub other_fields: usize,
    /// In both catalogs, different in `size` alone, where the saved size did not stand: one of
    /// the two is `0` or no count, or they differ by more than a sixteenth of the saved size
    /// and by more than 1 MiB.
    pub size_only: usize,
    /// In the refreshed catalog alone.
    pub added: usize,
    /// In the saved catalog alone.
    pub removed: usize,
    /// In both catalogs, where every field the refreshed one holds, `size` aside, is the saved
    /// one's, and the saved one holds keys the refreshed one lacks: the ones a refresh dropped
    /// because GitHub's row carries them.
    pub pruned: usize,
    /// The account is spelled otherwise than the saved catalog spells it.
    pub account: bool,
    /// The repositories both catalogs hold are in another order: the saved ones were not
    /// sorted by id, as a refresh writes them.
    pub order: bool,
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
    ///
    /// A repository GitHub lists comes out as: the provider-owned fields, from GitHub's row
    /// (`id`, `name`, `full_name`, `private`, `clone_url`, `default_branch`, `size`, `fork`,
    /// `archived`, and the owner's `id`, `login` and, where GitHub sends it, `type`); each key
    /// of [`POLICY`] its saved object holds; each other saved key GitHub's row for that
    /// repository does not carry; and, inside `owner`, each saved key GitHub's owner object
    /// does not carry. A saved key GitHub's row does carry is GitHub's, whatever value either
    /// side holds for it, a null included, and is dropped: no code reads one, GitHub holds its
    /// value, and every publication signs the whole catalog again (the published one was
    /// 298,546 bytes on 2026-11-13, which it would be if its repositories were saved as
    /// GitHub's whole rows; its rows were not read, and setup reads a few hundred bytes of
    /// each). So a policy key takes a name GitHub does not send, and each policy
    /// key code reads joins [`POLICY`] in the change that reads it: any other saved key is
    /// deleted at the first refresh after GitHub's row starts to carry a key of its name,
    /// under either visibility. A later build's [`POLICY`] does not keep its key from a
    /// refresh run by this build. A repository GitHub did not list, kept under
    /// [`RepositoryCatalogVisibility::TokenVisible`], comes out exactly as saved.
    ///
    /// A saved `size` stands while GitHub's is near it: neither is `0`, and they differ by at
    /// most a sixteenth of the saved size, or by at most 1 MiB where that is more. Where the
    /// account and the repositories with those sizes equal the saved ones, the answer holds
    /// them and is, as JSON, the saved catalog, which its publisher does not publish again.
    /// Where anything else differs the catalog is published anyway, so the answer holds
    /// GitHub's size for every observed repository: an administrator on a build that
    /// publishes on any difference then reads the same sizes and finds nothing to publish
    /// in answer.
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
            // The saved object is carried forward without the keys GitHub's row for this
            // repository carries: those are GitHub's. The test is the key, not its value,
            // because GitHub sends several keys as null. What stays is the administrator's:
            // the policy code reads, and any key GitHub does not send, future policy
            // included. The provider-owned fields are then written from GitHub's row.
            // `size` is GitHub's here; where the catalog differs from the saved one only in
            // sizes that stand, the pass below puts those back.
            let mut merged = old
                .get(&id)
                .and_then(|repo| repo.as_object())
                .cloned()
                .unwrap_or_default();
            merged.retain(|key, _| {
                key == "owner" || POLICY.contains(&key.as_str()) || repo.get(key).is_none()
            });
            // Owner identity is provider metadata too. A saved key beside that identity stays
            // on the same rule: unless GitHub's owner object carries a key of its name.
            let mut owner = merged
                .get("owner")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            owner.retain(|key, _| repo["owner"].get(key).is_none());
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
        // GitHub's size for a repository moves as it is pushed to. A catalog that differs
        // from the saved one only in sizes that still stand is the saved one.
        let settled: Vec<Value> = repositories
            .iter()
            .map(|repo| {
                let saved = repo["id"]
                    .as_u64()
                    .and_then(|id| old.get(&id))
                    .and_then(|saved| saved.get("size"));
                let mut repo = repo.clone();
                if let (Some(saved), Some(fields)) = (saved, repo.as_object_mut())
                    && stands(Some(saved), fields.get("size"))
                {
                    fields.insert("size".into(), saved.clone());
                }
                repo
            })
            .collect();
        if account == self.raw.account && settled == self.raw.repositories {
            repositories = settled;
        }
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

    /// How `refreshed`, a catalog [`Self::refresh`] made from this one, differs from it.
    /// # Errors
    /// `refreshed` is not a catalog [`Self::parse`] takes.
    pub fn changes(&self, refreshed: &[u8]) -> Result<RepositoryCatalogChanges, String> {
        fn rows(
            catalog: &RepositoryCatalog,
        ) -> std::collections::BTreeMap<u64, &aterm_json::Value> {
            catalog
                .raw
                .repositories
                .iter()
                .map(|repo| (repo["id"].as_u64().expect("parsed catalog id"), repo))
                .collect()
        }
        fn beside_size(repo: &aterm_json::Value) -> aterm_json::Value {
            let mut repo = repo.clone();
            if let Some(fields) = repo.as_object_mut() {
                fields.remove("size");
            }
            repo
        }
        fn ids<'a>(
            catalog: &'a RepositoryCatalog,
            other: &'a std::collections::BTreeMap<u64, &aterm_json::Value>,
        ) -> impl Iterator<Item = u64> + 'a {
            catalog
                .raw
                .repositories
                .iter()
                .map(|repo| repo["id"].as_u64().expect("parsed catalog id"))
                .filter(|id| other.contains_key(id))
        }
        /// `saved` cut to the keys `repo` holds, at its top and inside its `owner`.
        fn held_by(saved: &aterm_json::Value, repo: &aterm_json::Value) -> aterm_json::Value {
            let mut saved = saved.clone();
            if let Some(fields) = saved.as_object_mut() {
                fields.retain(|key, _| repo.get(key).is_some());
                if let Some(owner) = fields
                    .get_mut("owner")
                    .and_then(aterm_json::Value::as_object_mut)
                {
                    owner.retain(|key, _| repo["owner"].get(key).is_some());
                }
            }
            saved
        }
        let refreshed = Self::parse(refreshed)?;
        let (old, new) = (rows(self), rows(&refreshed));
        let mut changes = RepositoryCatalogChanges {
            removed: old.keys().filter(|id| !new.contains_key(id)).count(),
            account: refreshed.raw.account != self.raw.account,
            order: !ids(self, &new).eq(ids(&refreshed, &old)),
            ..RepositoryCatalogChanges::default()
        };
        for (id, repo) in &new {
            let Some(saved) = old.get(id) else {
                changes.added += 1;
                continue;
            };
            if repo == saved {
                continue;
            }
            let held = held_by(saved, repo);
            if beside_size(repo) != beside_size(&held) {
                changes.other_fields += 1;
            } else if held != **saved {
                changes.pruned += 1;
            } else if !stands(saved.get("size"), repo.get("size")) {
                changes.size_only += 1;
            }
        }
        Ok(changes)
    }
}

/// The keys of a saved repository object that are the administrator's policy and that code
/// reads. [`RepositoryCatalog::refresh`] keeps each whether or not GitHub's row carries a key
/// of that name; any other saved key it keeps only while GitHub's row carries none. A policy
/// key joins this list in the change that first reads it.
const POLICY: [&str; 1] = ["required_submodules"];

/// The share of a saved `size` GitHub's may differ from it by while the saved one stands.
const SIZE_BAND_SHARE: u64 = 16;

/// The least such difference, in KiB (1 MiB), for a repository under 16 MiB.
const SIZE_BAND_LEAST_KIB: u64 = 1024;

/// The `size` (KiB) the catalog holds for a repository it saved at `saved` and GitHub now
/// sizes at `observed`: `saved` while neither is `0` and they differ by at most
/// `max(saved / 16, 1024)`, else `observed`. The band is measured from the saved size, which
/// moves only when the catalog is published, so a look that finds nothing to publish leaves
/// every saved size within its band of the size GitHub gave, however many looks came before.
/// Setup reads a size only to estimate a clone's room at five times it ([`repos::no_room`]).
/// Over 16 MiB a kept size is at least 16/17 of GitHub's, and five times that is 4.7 times
/// GitHub's, the most a measured clone took; up to 16 MiB it is within 1 MiB, 5 MiB of room,
/// beside a floor counted in GiB.
const fn settled_size(saved: u64, observed: u64) -> u64 {
    let band = if saved / SIZE_BAND_SHARE > SIZE_BAND_LEAST_KIB {
        saved / SIZE_BAND_SHARE
    } else {
        SIZE_BAND_LEAST_KIB
    };
    if saved != 0 && observed != 0 && saved.abs_diff(observed) <= band {
        saved
    } else {
        observed
    }
}

/// Whether a saved `size` stands for GitHub's `observed` one: both are counts and
/// [`settled_size`] keeps the saved one (equal sizes included).
fn stands(saved: Option<&aterm_json::Value>, observed: Option<&aterm_json::Value>) -> bool {
    match (
        saved.and_then(aterm_json::Value::as_u64),
        observed.and_then(aterm_json::Value::as_u64),
    ) {
        (Some(saved), Some(observed)) => settled_size(saved, observed) == saved,
        _ => false,
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
            .vault_entry(Path::new(&user.home), ENTRY)
            .ok()
            .flatten()
            .filter(|bytes| bytes.len() <= MAX_BYTES)
            .ok_or_else(|| {
                "repository metadata is not ready in the Secrets vault; waiting for its private github-repositories.json entry".to_owned()
            })?;
        let inventory: RawInventory = aterm_json::from_slice(bytes.as_slice())
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
