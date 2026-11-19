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
/// repository is in at most one of the counts, the first of `other_fields`, `size_class` and
/// `pruned` that holds for it. The two catalogs mean the same ([`Meaning`]) exactly where
/// `other_fields`, `size_class`, `added` and `removed` are `0` and `account` is unset; a
/// refresh then answers the saved catalog itself, in which every count is `0` and neither flag
/// is set. So `pruned` and `order` are counted only in a catalog published for another reason.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RepositoryCatalogChanges {
    /// In both catalogs, with a field of its meaning other than `size` at another value in the
    /// refreshed one, or held by one of the two alone: a field a refresh writes from GitHub's
    /// row, the owner's `id`, `login` or `type`, or a [`POLICY`] key.
    pub other_fields: usize,
    /// In both catalogs, the same in meaning but for the class of its size
    /// ([`repos::size_class`]).
    pub size_class: usize,
    /// In the refreshed catalog alone.
    pub added: usize,
    /// In the saved catalog alone.
    pub removed: usize,
    /// In both catalogs and the same in meaning, but not as JSON: a refresh makes such a row
    /// only by dropping saved keys GitHub's row carries.
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
    /// A listed repository's `size` is the class of GitHub's ([`repos::size_class`]), except
    /// that a saved size stands while GitHub's is in its class, or under its class but over half
    /// of it, and where GitHub gives none: a row with no `size`, a null or other one that is no
    /// count, or `0`. It stands whether a refresh wrote it as a class or an earlier build wrote
    /// GitHub's own size. So a size moving back and forth across the edge of a class publishes
    /// once, and the class of a size the catalog holds is at or above GitHub's size at the last
    /// look, which setup's clone-room estimate reads ([`repos::need`]).
    ///
    /// Where the catalog this makes means what the saved one means ([`Meaning`]), the answer
    /// is the saved catalog itself, which its publisher does not publish again: a key dropped
    /// and the order by id wait for a change of meaning and are published with it. An
    /// administrator on an earlier build, which compares GitHub's size with the saved one by
    /// its own rule, may publish GitHub's size where this build wrote a class; this build
    /// reads that size inside its class and finds nothing to publish in answer.
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
            // The saved object is carried forward without the keys GitHub's row for this
            // repository carries: those are GitHub's. The test is the key, not its value,
            // because GitHub sends several keys as null. What stays is the administrator's:
            // the policy code reads, and any key GitHub does not send, future policy
            // included. The provider-owned fields are then written from GitHub's row, `size`
            // as its class unless the saved one stands.
            let saved = old.get(&id);
            let size = refreshed_size(
                saved.and_then(|repo| repo["size"].as_u64()).unwrap_or(0),
                repo["size"].as_u64().unwrap_or(0),
            );
            let mut merged = saved
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
            // `id` and `login` were checked above; `type` is written where GitHub sends it.
            for field in OWNER {
                if let Some(value) = repo["owner"].get(field) {
                    owner.insert(field.into(), value.clone());
                }
            }
            merged.insert("owner".into(), Value::Object(owner));
            for field in PROVIDER {
                merged.insert(field.into(), repo[field].clone());
            }
            merged.insert("size".into(), size.into());
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
        let mut account = account;
        if Meaning::of(account, &repositories)
            == Meaning::of(&self.raw.account, &self.raw.repositories)
        {
            account = self.raw.account.as_str();
            repositories.clone_from(&self.raw.repositories);
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
            let (was, now) = (Row::of(saved), Row::of(repo));
            if !was.same_beside_size(&now) {
                changes.other_fields += 1;
            } else if was.size != now.size {
                changes.size_class += 1;
            } else if repo != saved {
                changes.pruned += 1;
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

/// The fields of a repository object [`RepositoryCatalog::refresh`] writes as GitHub's row
/// holds them; it writes `size` as well, as [`refreshed_size`] says.
const PROVIDER: [&str; 8] = [
    "id",
    "name",
    "full_name",
    "private",
    "clone_url",
    "default_branch",
    "fork",
    "archived",
];

/// The keys of a repository's `owner` [`RepositoryCatalog::refresh`] writes from GitHub's.
const OWNER: [&str; 3] = ["id", "login", "type"];

/// The `size` (KiB) a refresh writes for a repository the catalog holds at `saved` that
/// GitHub sizes at `observed`, each `0` where it gives none (a repository new to the catalog, a
/// row GitHub has not sized): the class of GitHub's size ([`repos::size_class`]), except that
/// the saved size stands while GitHub's is in its class, or under its class but over half of
/// it, and where GitHub gives none. Setup estimates a clone's room at five times the class of
/// the size it reads ([`repos::need`]), and the class of what this writes is at or above
/// GitHub's size: under a quarter more where it is GitHub's class or a size in it, under twice
/// it where GitHub's size fell since by under half. So a growing repository is published about
/// four times each time its size doubles, and a size moving back and forth across a class's
/// edge, as pushes and GitHub's maintenance move it, is published once.
fn refreshed_size(saved: u64, observed: u64) -> u64 {
    let Some(class) = repos::size_class(observed) else {
        return saved;
    };
    match repos::size_class(saved) {
        Some(held) if class == held || (class < held && observed > held / 2) => saved,
        _ => class,
    }
}

/// What a catalog means to the Macs that read it, which a refresh publishes only where it
/// moved: its account and, per repository id, the [`PROVIDER`] fields, the class of its `size`
/// ([`repos::size_class`]), the owner's [`OWNER`] keys and the [`POLICY`] keys. The order of
/// the repositories, any other key and a size inside its class are not in it.
#[derive(PartialEq)]
struct Meaning<'a> {
    account: &'a str,
    repositories: std::collections::BTreeMap<u64, Row<'a>>,
}

impl<'a> Meaning<'a> {
    /// The meaning of a catalog of `account` that holds `repositories`, each with an id.
    fn of(account: &'a str, repositories: &'a [aterm_json::Value]) -> Self {
        Self {
            account,
            repositories: repositories
                .iter()
                .map(|repo| (repo["id"].as_u64().expect("checked id"), Row::of(repo)))
                .collect(),
        }
    }
}

/// One repository of a [`Meaning`]; a key the object lacks is `None`, one it holds as null is
/// not.
#[derive(PartialEq)]
struct Row<'a> {
    provider: [Option<&'a aterm_json::Value>; PROVIDER.len()],
    owner: [Option<&'a aterm_json::Value>; OWNER.len()],
    policy: [Option<&'a aterm_json::Value>; POLICY.len()],
    /// The class of its `size`; `None` for `0`, for no count and for no `size`.
    size: Option<u64>,
}

impl<'a> Row<'a> {
    fn of(repo: &'a aterm_json::Value) -> Self {
        Self {
            provider: PROVIDER.map(|key| repo.get(key)),
            owner: OWNER.map(|key| repo.get("owner").and_then(|owner| owner.get(key))),
            policy: POLICY.map(|key| repo.get(key)),
            size: repo
                .get("size")
                .and_then(aterm_json::Value::as_u64)
                .and_then(repos::size_class),
        }
    }

    fn same_beside_size(&self, other: &Self) -> bool {
        (self.provider, self.owner, self.policy) == (other.provider, other.owner, other.policy)
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
