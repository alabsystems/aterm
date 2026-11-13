// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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

    // The administrator refreshes provider fields, never the policy attached to a stable
    // repository ID. Full replacement also retires genuinely deleted/transferred IDs.
    let full = |id, name: &str| {
        let mut repo = repository("fixture-owner", name, id);
        let fields = repo.as_object_mut().unwrap();
        fields.insert(
            "owner".into(),
            aterm_json::json!({"login":"fixture-owner","id":7}),
        );
        fields.insert("size".into(), 1.into());
        fields.insert("fork".into(), false.into());
        fields.insert("archived".into(), false.into());
        repo
    };
    let mut policy = full(1, "before-rename");
    policy.as_object_mut().unwrap().insert(
        "required_submodules".into(),
        aterm_json::json!(["vendor/library"]),
    );
    policy.as_object_mut().unwrap().insert(
        "future_policy".into(),
        aterm_json::json!({"keep": ["exact", 7]}),
    );
    policy
        .as_object_mut()
        .unwrap()
        .get_mut("owner")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .insert("future_policy".into(), aterm_json::json!({"keep":true}));
    let before = aterm_json::json!({"schema":2,"account":"fixture-owner",
        "repositories":[policy.clone(),full(2,"removed"),full(3,"archived")]});
    let captured = RepositoryCatalog::parse(&aterm_json::to_vec(&before).unwrap()).unwrap();
    let mut archived = full(3, "archived");
    archived
        .as_object_mut()
        .unwrap()
        .insert("archived".into(), true.into());
    let observed = vec![full(1, "after-rename"), archived, full(4, "new")];
    let complete = RepositoryCatalogVisibility::AccountComplete;
    let visible = RepositoryCatalogVisibility::TokenVisible;
    let bytes = captured
        .refresh("fixture-owner", 7, &observed, complete)
        .unwrap();
    let union: aterm_json::Value = aterm_json::from_slice(
        &captured
            .refresh("fixture-owner", 7, &observed, visible)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(union["repositories"].as_array().unwrap().len(), 4);
    assert_eq!(
        union["repositories"][1], before["repositories"][1],
        "a restricted token cannot retire or rewrite an unseen saved record"
    );
    assert_eq!(
        union["repositories"][0]["future_policy"],
        policy["future_policy"]
    );
    let after: aterm_json::Value = aterm_json::from_slice(&bytes).unwrap();
    let repos = after["repositories"].as_array().unwrap();
    assert_eq!(
        repos
            .iter()
            .map(|r| r["id"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        [1, 3, 4]
    );
    assert_eq!(repos[0]["name"].as_str(), Some("after-rename"));
    for field in ["required_submodules", "future_policy"] {
        assert_eq!(repos[0][field], policy[field]);
    }
    assert_eq!(
        repos[0]["owner"]["future_policy"],
        policy["owner"]["future_policy"]
    );
    assert!(repos[2].get("required_submodules").is_none());
    assert_eq!(
        Inventory::try_from(aterm_json::from_slice::<RawInventory>(&bytes).unwrap())
            .unwrap()
            .repositories()
            .len(),
        2
    );
    // A renamed account retains its numeric owner identity too.
    let mut renamed_owner = full(1, "after-rename");
    let renamed_fields = renamed_owner.as_object_mut().unwrap();
    renamed_fields.insert(
        "owner".into(),
        aterm_json::json!({"login":"renamed-owner","id":7}),
    );
    renamed_fields.insert("full_name".into(), "renamed-owner/after-rename".into());
    renamed_fields.insert(
        "clone_url".into(),
        "https://github.com/renamed-owner/after-rename.git".into(),
    );
    assert!(
        captured
            .refresh("renamed-owner", 7, &[renamed_owner], complete)
            .is_ok()
    );
    let empty = captured.refresh("fixture-owner", 7, &[], complete).unwrap();
    let restarted = RepositoryCatalog::parse(&empty).unwrap();
    assert_eq!(restarted.owner_id(), None);
    assert!(
        restarted
            .refresh("fixture-owner", 7, &[full(4, "new")], visible)
            .is_ok()
    );
    assert!(restarted.refresh("someone-else", 7, &[], complete).is_err());
    assert!(captured.refresh("fixture-owner", 8, &[], complete).is_err());
    assert!(
        captured
            .refresh("fixture-owner", 7, &[full(1, "a"), full(1, "b")], visible)
            .is_err()
    );
    let mut foreign = full(1, "foreign");
    foreign.as_object_mut().unwrap().insert(
        "owner".into(),
        aterm_json::json!({"login":"fixture-owner","id":8}),
    );
    assert!(
        captured
            .refresh("fixture-owner", 7, &[foreign], visible)
            .is_err()
    );
    let mut unknown = full(1, "unknown");
    unknown.as_object_mut().unwrap().remove("private");
    assert!(
        captured
            .refresh("fixture-owner", 7, &[unknown], visible)
            .is_err()
    );
    let mut too_large = before.clone();
    too_large
        .as_object_mut()
        .unwrap()
        .get_mut("repositories")
        .unwrap()
        .as_array_mut()
        .unwrap()[0]
        .as_object_mut()
        .unwrap()
        .insert("future_policy".into(), "x".repeat(MAX_BYTES).into());
    assert!(RepositoryCatalog::parse(&aterm_json::to_vec(&too_large).unwrap()).is_err());
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
