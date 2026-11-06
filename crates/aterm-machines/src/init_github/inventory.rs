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

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Inventory {
    schema: u32,
    pub(super) account: String,
    repositories: Vec<aterm_json::Value>,
}

impl Inventory {
    pub(super) fn read(sys: &dyn System, user: &User) -> Result<Self, String> {
        let bytes = sys
            .read_owned_file(&Path::new(&user.home).join(FILE), user.uid, MAX_BYTES, true)
            .map_err(|_| {
                "repository metadata is not ready in the Secrets vault; waiting for its private github-repositories.json entry".to_owned()
            })?;
        let inventory: Self = aterm_json::from_slice(&bytes)
            .map_err(|_| "the vault's repository metadata is not valid JSON".to_owned())?;
        inventory.validate()?;
        Ok(inventory)
    }

    fn validate(&self) -> Result<(), String> {
        if self.schema != 2 || !super::login(&self.account) {
            return Err("the vault's repository metadata has an unsupported schema or invalid development account".into());
        }
        self.repositories().map(|_| ())
    }

    pub(super) fn repositories(&self) -> Result<repos::Inventory, String> {
        let listed =
            repos::inventory_filtered(&self.repositories, None, Some(&self.account), |repo| {
                !repo.fork && !repo.archived
            })?;
        if !listed.notes.is_empty() {
            return Err(format!(
                "the vault's repository metadata has {} conflicting destination(s) among {} intended repositories; fix the private inventory before cloning",
                listed.notes.len(),
                listed.repositories.len() + listed.notes.len()
            ));
        }
        Ok(listed)
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
        let inventory = Inventory {
            schema: 2,
            account: "fixture-owner".into(),
            repositories: vec![repository("fixture-owner", "private-code", 1)],
        };
        inventory.validate().unwrap();
        let listed = inventory.repositories().unwrap();
        assert_eq!(listed.repositories.len(), 1);
        assert_eq!(listed.repositories[0].owner, "fixture-owner");
        let mut invalid = inventory.clone();
        invalid.schema = 1;
        assert!(invalid.validate().is_err());
        let mut duplicate = inventory;
        duplicate
            .repositories
            .push(duplicate.repositories[0].clone());
        assert!(duplicate.validate().is_err());
    }

    #[test]
    fn private_inventory_never_drops_an_intended_repository_to_resolve_a_folder_collision() {
        let inventory = Inventory {
            schema: 2,
            account: "alice".into(),
            repositories: vec![
                repository("alice", "shared", 1),
                repository("team", "Shared", 2),
            ],
        };
        let why = inventory.repositories().unwrap_err();
        assert!(
            why.contains("1 conflicting destination(s) among 2 intended repositories"),
            "{why}"
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
                let inventory = Inventory {
                    schema: 2,
                    account: "alice".into(),
                    repositories,
                };
                let listed = inventory.repositories().unwrap();
                assert_eq!(listed.repositories.len(), 1);
                assert_eq!(listed.repositories[0].owner, "team");
                assert_eq!(listed.repositories[0].name, "shared");
                assert!(listed.notes.is_empty());
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
        let inventory = Inventory {
            schema: 2,
            account: "alice".into(),
            repositories: vec![active.clone(), excluded],
        };
        assert!(inventory.repositories().is_err());
        let mut excluded = repository("team", "other", 1);
        excluded
            .as_object_mut()
            .unwrap()
            .insert("fork".into(), aterm_json::Value::Bool(true));
        let inventory = Inventory {
            repositories: vec![active, excluded],
            ..inventory
        };
        assert!(inventory.repositories().is_err());
    }
}
