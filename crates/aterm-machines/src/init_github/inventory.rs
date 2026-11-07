// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Development repository metadata belongs to the private vault. The signed vault delivers
//! this file beside the development credential; setup never enumerates a public account to
//! infer the repositories a machine should have.

use super::repos;
use crate::model::User;
use crate::port::System;
use serde::Deserialize;
use std::path::Path;

pub(super) const FILE: &str = ".secrets/github-repositories.json";
pub(super) const MAX_BYTES: usize = 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawInventory {
    schema: u32,
    account: String,
    repositories: Vec<aterm_json::Value>,
}

/// Only a complete validated private list can be retained by a captured source.
#[derive(Clone)]
pub(super) struct Inventory(Vec<repos::Repo>);

impl Inventory {
    pub(super) fn read(sys: &dyn System, user: &User) -> Result<Self, String> {
        let bytes = sys
            .read_owned_file(&Path::new(&user.home).join(FILE), user.uid, MAX_BYTES, true)
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
mod tests {
    use super::*;

    fn repository(owner: &str, name: &str, id: u64) -> aterm_json::Value {
        aterm_json::json!({"id":id,"name":name,"owner":{"login":owner},
            "full_name":format!("{owner}/{name}"),"private":true,
            "clone_url":format!("https://github.com/{owner}/{name}.git"),
            "default_branch":"main"})
    }

    #[test]
    fn private_metadata_is_the_complete_inventory_and_account_source() {
        let repo = repository("fixture-owner", "private-code", 1);
        let inventory = Inventory::try_from(RawInventory {
            schema: 2,
            account: "fixture-owner".into(),
            repositories: vec![repo.clone()],
        })
        .unwrap();
        let listed = inventory.repositories();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].owner, "fixture-owner");
        assert!(
            Inventory::try_from(RawInventory {
                schema: 1,
                account: "fixture-owner".into(),
                repositories: vec![repo.clone()],
            })
            .is_err()
        );
        assert!(
            Inventory::try_from(RawInventory {
                schema: 2,
                account: "fixture-owner".into(),
                repositories: vec![repo.clone(), repo],
            })
            .is_err()
        );
    }

    #[test]
    fn excluded_repositories_cannot_block_an_active_destination() {
        for flag in ["fork", "archived"] {
            for excluded_first in [false, true] {
                let active = repository("team", "shared", 1);
                let mut excluded = repository("alice", "Shared", 2);
                excluded
                    .as_object_mut()
                    .unwrap()
                    .insert(flag.into(), aterm_json::Value::Bool(true));
                let repositories = if excluded_first {
                    vec![excluded, active]
                } else {
                    vec![active, excluded]
                };
                let inventory = Inventory::try_from(RawInventory {
                    schema: 2,
                    account: "alice".into(),
                    repositories,
                })
                .unwrap();
                let listed = inventory.repositories();
                assert_eq!(listed.len(), 1);
                assert_eq!(listed[0].owner, "team");
                assert_eq!(listed[0].name, "shared");
            }
        }
    }

    #[test]
    fn excluded_metadata_still_requires_valid_unique_identities() {
        let active = repository("alice", "code", 1);
        let mut excluded = repository("team", "other", 2);
        excluded
            .as_object_mut()
            .unwrap()
            .insert("archived".into(), aterm_json::Value::Bool(true));
        excluded.as_object_mut().unwrap().insert(
            "clone_url".into(),
            aterm_json::Value::String("https://elsewhere.invalid/code.git".into()),
        );
        let inventory = RawInventory {
            schema: 2,
            account: "alice".into(),
            repositories: vec![active.clone(), excluded],
        };
        assert!(Inventory::try_from(inventory).is_err());
        let mut excluded = repository("team", "other", 1);
        excluded
            .as_object_mut()
            .unwrap()
            .insert("fork".into(), aterm_json::Value::Bool(true));
        let inventory = RawInventory {
            schema: 2,
            account: "alice".into(),
            repositories: vec![active, excluded],
        };
        assert!(Inventory::try_from(inventory).is_err());
    }
}
