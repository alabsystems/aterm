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

/// A row GitHub lists for the fixture account, with every field a refresh requires.
fn listed(id: u64, name: &str, size: u64) -> aterm_json::Value {
    let mut repo = repository("fixture-owner", name, id);
    let fields = repo.as_object_mut().unwrap();
    fields.insert(
        "owner".into(),
        aterm_json::json!({"login":"fixture-owner","id":7}),
    );
    fields.insert("size".into(), size.into());
    fields.insert("fork".into(), false.into());
    fields.insert("archived".into(), false.into());
    repo
}

/// The saved catalog the size laws refresh, its sizes GitHub's own as an earlier build wrote
/// them: one in the class (1,792, 2,048] KiB, one in (1,572,864, 1,835,008] KiB, one GitHub has
/// not sized and one in the least class, at most 1 MiB.
const SAVED_SIZES: [u64; 4] = [2_000, 1_600_015, 0, 512];
const NAMES: [&str; 4] = ["small", "large", "unsized", "tiny"];

fn rows(sizes: &[u64]) -> Vec<aterm_json::Value> {
    sizes
        .iter()
        .zip(NAMES)
        .zip(1..)
        .map(|((size, name), id)| listed(id, name, *size))
        .collect()
}

/// The saved rows, the first with policy the administrator attached.
fn saved_rows() -> Vec<aterm_json::Value> {
    let mut repositories = rows(&SAVED_SIZES);
    repositories[0].as_object_mut().unwrap().insert(
        "required_submodules".into(),
        aterm_json::json!(["vendor/library"]),
    );
    repositories
}

/// A saved catalog that spells its account `account` and holds `repositories` in that order.
fn catalog_of(
    account: &str,
    repositories: Vec<aterm_json::Value>,
) -> (aterm_json::Value, RepositoryCatalog) {
    let saved = aterm_json::json!({"schema":2,"account":account,
        "repositories":repositories});
    let catalog = RepositoryCatalog::parse(&aterm_json::to_vec(&saved).unwrap()).unwrap();
    (saved, catalog)
}

fn saved_catalog() -> (aterm_json::Value, RepositoryCatalog) {
    catalog_of("fixture-owner", saved_rows())
}

/// Whether `changes` says the meaning moved: a count other than `pruned` above 0, or the
/// account's spelling.
const fn moved(changes: RepositoryCatalogChanges) -> bool {
    changes.other_fields + changes.size_class + changes.added + changes.removed > 0
        || changes.account
}

/// What a refresh of `catalog` answers, with its counts. Every refresh the laws below make
/// passes through here, so each of those is held to this: the answer is, as JSON, the saved
/// catalog, which is where its publisher signs nothing, exactly where the two mean the same
/// ([`Meaning`]); and exactly there the counts are all 0 and the flags unset. So a
/// publication's log line always has a count of a change of meaning above 0, or `account`.
fn refreshed(
    catalog: &RepositoryCatalog,
    observed: &[aterm_json::Value],
    visibility: RepositoryCatalogVisibility,
) -> (aterm_json::Value, RepositoryCatalogChanges) {
    let bytes = catalog
        .refresh("fixture-owner", 7, observed, visibility)
        .unwrap();
    let after: aterm_json::Value = aterm_json::from_slice(&bytes).unwrap();
    let changes = catalog.changes(&bytes).unwrap();
    let saved = aterm_json::json!({"schema":catalog.raw.schema,"account":catalog.raw.account,
        "repositories":catalog.raw.repositories});
    let same = Meaning::of(
        after["account"].as_str().unwrap(),
        after["repositories"].as_array().unwrap(),
    ) == Meaning::of(&catalog.raw.account, &catalog.raw.repositories);
    let unsigned = after == saved;
    assert_eq!(
        (same, changes == NONE, !moved(changes)),
        (unsigned, unsigned, unsigned),
        "{changes:?} for a refresh that {} the saved catalog",
        if unsigned { "is" } else { "is not" }
    );
    (after, changes)
}

fn sizes(catalog: &aterm_json::Value) -> Vec<u64> {
    catalog["repositories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|repo| repo["size"].as_u64().unwrap())
        .collect()
}

const NONE: RepositoryCatalogChanges = RepositoryCatalogChanges {
    other_fields: 0,
    size_class: 0,
    added: 0,
    removed: 0,
    pruned: 0,
    account: false,
    order: false,
};

/// GitHub's size for a repository moves as it is pushed to. A size anywhere in the class of the
/// saved one ([`repos::size_class`]), the class's edges included, and a size of `0` change
/// nothing: the refresh is, as JSON, the saved catalog, under either visibility. The saved sizes
/// are GitHub's own, as an earlier build wrote them; their classes are (1,792, 2,048] KiB,
/// (1,572,864, 1,835,008] KiB, none and at most 1 MiB.
#[test]
fn a_size_inside_its_class_changes_nothing() {
    assert_eq!(repos::size_class(0), None, "GitHub's 0 is no size");
    for (kib, class) in [
        (1, 1_024),
        (1_024, 1_024),
        (1_025, 1_280),
        (1_280, 1_280),
        (1_281, 1_536),
        (1_792, 1_792),
        (1_793, 2_048),
        (2_048, 2_048),
        (2_049, 2_560),
        (1_600_015, 1_835_008),
        (1_835_009, 2_097_152),
        (2_097_153, 2_621_440),
        (1 << 63, 1 << 63),
        (7 << 61, 7 << 61),
        ((7 << 61) + 1, u64::MAX),
        (u64::MAX, u64::MAX),
    ] {
        assert_eq!(
            repos::size_class(kib),
            Some(class),
            "the class of {kib} KiB"
        );
        assert_eq!(
            repos::size_class(class),
            Some(class),
            "a class is its own class"
        );
    }
    let (saved, catalog) = saved_catalog();
    for inside in [
        [1_793, 1_572_865, 0, 1],
        [2_048, 1_835_008, 0, 1_024],
        [1_900, 1_700_000, 0, 700],
        [1_999, 1_600_000, 0, 513],
        [0, 0, 0, 0],
        SAVED_SIZES,
    ] {
        for visibility in [
            RepositoryCatalogVisibility::AccountComplete,
            RepositoryCatalogVisibility::TokenVisible,
        ] {
            let (after, changes) = refreshed(&catalog, &rows(&inside), visibility);
            assert_eq!(
                (&after, changes),
                (&saved, NONE),
                "sizes {inside:?} are in the classes of the saved ones"
            );
        }
    }
    // A token that sees only two of them keeps the others' rows, and their sizes, as saved.
    let (after, changes) = refreshed(
        &catalog,
        &rows(&[1_800, 1_600_100]),
        RepositoryCatalogVisibility::TokenVisible,
    );
    assert_eq!((after, changes), (saved, NONE));
}

/// One KiB past a saved size's class, a fall to half of it, and a size GitHub gives a
/// repository it had not sized, are a change: the catalog, published for it, holds GitHub's
/// class for that repository and the saved size of every other whose size stayed in its class.
/// A repository new to the catalog takes its class; one GitHub has not sized takes `0`.
#[test]
fn a_size_past_its_class_is_published_as_githubs_class() {
    let (saved, catalog) = saved_catalog();
    let complete = RepositoryCatalogVisibility::AccountComplete;
    let moved = RepositoryCatalogChanges {
        size_class: 1,
        ..NONE
    };
    for (github, published) in [
        ([2_049, 1_700_000, 0, 700], [2_560, 1_600_015, 0, 512]),
        ([1_024, 1_700_000, 0, 700], [1_024, 1_600_015, 0, 512]),
        ([1_900, 1_835_009, 0, 700], [2_000, 2_097_152, 0, 512]),
        ([1_900, 917_504, 0, 700], [2_000, 917_504, 0, 512]),
        ([1_900, 1_700_000, 700, 700], [2_000, 1_600_015, 1_024, 512]),
        ([1_900, 1_700_000, 0, 1_025], [2_000, 1_600_015, 0, 1_280]),
    ] {
        let (after, changes) = refreshed(&catalog, &rows(&github), complete);
        assert_eq!(sizes(&after), published, "GitHub's sizes {github:?}");
        assert_eq!(changes, moved, "GitHub's sizes {github:?}");
        assert_eq!(
            after["repositories"][0]["required_submodules"],
            saved["repositories"][0]["required_submodules"]
        );
        // The published catalog is a fixed point of the same look.
        let published = RepositoryCatalog::parse(&aterm_json::to_vec(&after).unwrap()).unwrap();
        assert_eq!(
            refreshed(&published, &rows(&github), complete),
            (after, NONE)
        );
    }

    // A repository new to the catalog takes its class, or `0` where GitHub has not sized it.
    let mut observed = rows(&SAVED_SIZES);
    observed.extend([listed(5, "new", 3), listed(6, "empty", 0)]);
    let (after, changes) = refreshed(&catalog, &observed, complete);
    assert_eq!(sizes(&after), [2_000, 1_600_015, 0, 512, 1_024, 0]);
    assert_eq!(changes, RepositoryCatalogChanges { added: 2, ..NONE });
}

/// What a build before this one (0.202.0 to 0.205.0) did with sizes, as a model: it kept a saved
/// size while GitHub's was the same or, neither being 0, within a sixteenth of the saved one or
/// 1 MiB where that is more, and otherwise published GitHub's size for every repository it
/// observed. The answer is the sizes it published, or `None` where it published nothing.
fn earlier_build_publishes(saved: &[u64], github: &[u64]) -> Option<Vec<u64>> {
    let keeps = |saved: u64, github: u64| {
        saved == github
            || (saved != 0 && github != 0 && saved.abs_diff(github) <= (saved / 16).max(1_024))
    };
    (!saved
        .iter()
        .zip(github)
        .all(|(saved, github)| keeps(*saved, *github)))
    .then(|| github.to_vec())
}

/// The two admin seats may run two builds: an earlier one that writes GitHub's own size, and
/// this one, which writes a class and keeps a saved size while GitHub's is at most its class. They spell one catalog
/// differently and do not undo each other: this build never answers what the earlier one
/// wrote while GitHub's sizes stay in their classes, and the earlier one answers a class this
/// build published once, with GitHub's sizes, which this build then keeps. Only a size past its
/// class makes this build sign.
#[test]
fn two_builds_that_spell_one_catalog_differently_do_not_undo_each_other() {
    /// The sizes the vault's catalog holds.
    fn held(vault: &[aterm_json::Value]) -> Vec<u64> {
        vault
            .iter()
            .map(|repo| repo["size"].as_u64().unwrap())
            .collect()
    }
    /// A look by this build at GitHub's sizes `github`: whether it signs, and the vault after.
    fn this_build(vault: &mut Vec<aterm_json::Value>, github: [u64; 4]) -> bool {
        let (_, catalog) = catalog_of("fixture-owner", vault.clone());
        let (after, changes) = refreshed(
            &catalog,
            &rows(&github),
            RepositoryCatalogVisibility::AccountComplete,
        );
        vault.clone_from(after["repositories"].as_array().unwrap());
        moved(changes)
    }
    /// A look by the earlier build ([`earlier_build_publishes`]): whether it signs, and the
    /// vault after.
    fn earlier_build(vault: &mut [aterm_json::Value], github: [u64; 4]) -> bool {
        let Some(published) = earlier_build_publishes(&held(vault), &github) else {
            return false;
        };
        for (repo, size) in vault.iter_mut().zip(published) {
            repo.as_object_mut()
                .unwrap()
                .insert("size".into(), size.into());
        }
        true
    }
    // The vault's catalog, as the earlier build published it.
    let mut vault = rows(&SAVED_SIZES);

    // GitHub's sizes inside the classes of what the earlier build wrote: this build signs
    // nothing; the earlier one, with the large repository past its own band, signs.
    let first = [2_040, 1_800_000, 0, 1_000];
    assert!(
        !this_build(&mut vault, first),
        "this build answered the earlier one"
    );
    assert_eq!(held(&vault), SAVED_SIZES);
    assert!(earlier_build(&mut vault, first));
    assert_eq!(held(&vault), first);
    // GitHub's sizes move again, past the earlier build's band, in their classes or under them
    // by less than half: this build does not answer what the earlier one wrote.
    let second = [1_025, 1_600_000, 0, 3];
    assert!(
        !this_build(&mut vault, second),
        "this build answered the earlier one"
    );
    assert_eq!(held(&vault), first);

    // The large repository passes its class: this build publishes its class, once, and keeps
    // every other size the earlier one wrote.
    let third = [1_025, 1_835_009, 0, 3];
    assert!(this_build(&mut vault, third));
    assert_eq!(held(&vault), [2_040, 2_097_152, 0, 1_000]);
    assert!(
        !this_build(&mut vault, third),
        "this build signed its own class again"
    );
    // The earlier build answers the class with GitHub's sizes, once; this build keeps them, and
    // neither signs again while GitHub's sizes stand.
    assert!(earlier_build(&mut vault, third));
    assert_eq!(held(&vault), third);
    for _ in 0..4 {
        assert!(
            !this_build(&mut vault, third),
            "this build answered the earlier one"
        );
        assert!(
            !earlier_build(&mut vault, third),
            "the earlier build answered this one"
        );
    }
    assert_eq!(held(&vault), third);
}

/// A catalog that differs from the saved one in meaning beside its sizes is published: it holds
/// every field a refresh takes as GitHub gave it and the saved size of each repository whose
/// size stayed in its class, and its counts say why.
#[test]
fn a_catalog_published_for_another_field_keeps_each_size_inside_its_class() {
    let (saved, catalog) = saved_catalog();
    let complete = RepositoryCatalogVisibility::AccountComplete;
    let inside = [1_900, 1_700_000, 0, 700];
    let changed = |field: &str, value: aterm_json::Value| {
        let mut observed = rows(&inside);
        observed[1]
            .as_object_mut()
            .unwrap()
            .insert(field.into(), value);
        observed
    };
    let other = RepositoryCatalogChanges {
        other_fields: 1,
        ..NONE
    };
    for (field, value) in [
        ("default_branch", aterm_json::Value::from("trunk")),
        ("archived", true.into()),
        ("fork", true.into()),
        ("private", false.into()),
    ] {
        let (after, changes) = refreshed(&catalog, &changed(field, value.clone()), complete);
        assert_eq!(after["repositories"][1][field], value, "{field}");
        assert_eq!(sizes(&after), SAVED_SIZES, "{field}");
        assert_eq!(changes, other, "{field}");
        // Policy the administrator attached stays, as before.
        assert_eq!(
            after["repositories"][0]["required_submodules"],
            saved["repositories"][0]["required_submodules"]
        );
    }
    // The owner's type, which GitHub sends and the saved catalog lacks, is a field too.
    let typed: Vec<_> = rows(&inside)
        .into_iter()
        .map(|repo| with_owner(repo, "type", "User"))
        .collect();
    let (after, changes) = refreshed(&catalog, &typed, complete);
    assert_eq!(
        after["repositories"][2]["owner"]["type"].as_str(),
        Some("User")
    );
    assert_eq!(
        changes,
        RepositoryCatalogChanges {
            other_fields: 4,
            ..NONE
        }
    );

    // A rename and a size past its class on one repository count once, as another field; a
    // size GitHub gives one it had not sized counts as its class; a retired one as removed.
    let mut observed = rows(&[2_009, 1_600_009, 4_000]);
    let renamed = observed[0].as_object_mut().unwrap();
    renamed.insert("name".into(), "renamed".into());
    renamed.insert("full_name".into(), "fixture-owner/renamed".into());
    renamed.insert(
        "clone_url".into(),
        "https://github.com/fixture-owner/renamed.git".into(),
    );
    renamed.insert("size".into(), 9_000.into());
    let (after, changes) = refreshed(&catalog, &observed, complete);
    assert_eq!(sizes(&after), [10_240, 1_600_015, 4_096]);
    assert_eq!(
        changes,
        RepositoryCatalogChanges {
            other_fields: 1,
            size_class: 1,
            removed: 1,
            ..NONE
        }
    );
}

/// A saved catalog whose account is spelled otherwise than GitHub spells it is published once
/// although every repository in it means what it meant: the answer holds GitHub's spelling and
/// each saved size inside its class, its flag says why, and a refresh of that answer is that
/// answer.
#[test]
fn a_catalog_published_for_the_account_alone_says_so() {
    let complete = RepositoryCatalogVisibility::AccountComplete;
    let visible = RepositoryCatalogVisibility::TokenVisible;
    let inside = [2_009, 1_600_024, 0, 521];
    let (saved, catalog) = catalog_of("Fixture-Owner", saved_rows());
    for visibility in [complete, visible] {
        let (after, changes) = refreshed(&catalog, &rows(&inside), visibility);
        assert_eq!(after["account"].as_str(), Some("fixture-owner"));
        assert_eq!(sizes(&after), SAVED_SIZES);
        assert_eq!(
            changes,
            RepositoryCatalogChanges {
                account: true,
                ..NONE
            }
        );
        assert_eq!(
            after["repositories"][0]["required_submodules"],
            saved["repositories"][0]["required_submodules"]
        );
        let again = RepositoryCatalog::parse(&aterm_json::to_vec(&after).unwrap()).unwrap();
        assert_eq!(refreshed(&again, &rows(&inside), visibility), (after, NONE));
    }
}

/// A saved catalog whose repositories are not sorted by id means what the sorted one means: a
/// refresh that finds nothing else answers it as saved, under either visibility. The next
/// change of meaning publishes it sorted, and its flag says the order was published beside it.
#[test]
fn a_catalog_in_another_order_is_sorted_only_beside_a_change() {
    let complete = RepositoryCatalogVisibility::AccountComplete;
    let visible = RepositoryCatalogVisibility::TokenVisible;
    let inside = [2_009, 1_600_024, 0, 521];
    let ids = |catalog: &aterm_json::Value| -> Vec<u64> {
        catalog["repositories"]
            .as_array()
            .unwrap()
            .iter()
            .map(|repo| repo["id"].as_u64().unwrap())
            .collect()
    };
    // The saved order: ids 4, 3, 1, 2.
    let mut unsorted = saved_rows();
    unsorted.rotate_right(2);
    unsorted.swap(0, 1);
    let (saved, catalog) = catalog_of("fixture-owner", unsorted);
    assert_eq!(ids(&saved), [4, 3, 1, 2]);
    for visibility in [complete, visible] {
        assert_eq!(
            refreshed(&catalog, &rows(&inside), visibility),
            (saved.clone(), NONE),
            "the order alone was published"
        );
    }
    // A token that sees two of the four finds nothing else either.
    assert_eq!(
        refreshed(&catalog, &rows(&inside[..2]), visible),
        (saved.clone(), NONE)
    );
    // A repository new to the catalog: all five are published sorted, each saved size inside
    // its class standing.
    let mut observed = rows(&inside);
    observed.push(listed(5, "new", 3));
    for visibility in [complete, visible] {
        let (after, changes) = refreshed(&catalog, &observed, visibility);
        assert_eq!(ids(&after), [1, 2, 3, 4, 5]);
        assert_eq!(sizes(&after), [2_000, 1_600_015, 0, 512, 1_024]);
        assert_eq!(
            changes,
            RepositoryCatalogChanges {
                added: 1,
                order: true,
                ..NONE
            }
        );
    }
    // A retired repository does not make the order differ: the three kept are in id order.
    let (_, catalog) = saved_catalog();
    let (_, changes) = refreshed(&catalog, &rows(&SAVED_SIZES[..3]), complete);
    assert_eq!(changes, RepositoryCatalogChanges { removed: 1, ..NONE });
}

/// The keys of an object a refresh makes that it writes from GitHub's row, in the catalog's
/// (sorted) order.
const PROVIDER_OWNED: [&str; 10] = [
    "archived",
    "clone_url",
    "default_branch",
    "fork",
    "full_name",
    "id",
    "name",
    "owner",
    "private",
    "size",
];

/// Keys GitHub's REST row carries that no code names, as [`whole`] adds them.
const PROVIDER_SHAPED: [&str; 3] = ["html_url", "permissions", "pushed_at"];

/// `repo` with part of what GitHub's whole REST row carries beside the fields a refresh takes
/// ([`PROVIDER_SHAPED`], and `avatar_url` in its owner), at values that differ with `mark`.
fn whole(mut repo: aterm_json::Value, mark: &str) -> aterm_json::Value {
    let fields = repo.as_object_mut().unwrap();
    fields.insert(
        "html_url".into(),
        format!("https://github.invalid/{mark}").into(),
    );
    fields.insert(
        "permissions".into(),
        aterm_json::json!({"admin": true, "pull": true, "push": mark == "saved"}),
    );
    fields.insert("pushed_at".into(), format!("{mark}-pushed").into());
    with_owner(
        repo,
        "avatar_url",
        format!("https://avatars.invalid/{mark}"),
    )
}

fn with(
    mut repo: aterm_json::Value,
    key: &str,
    value: impl Into<aterm_json::Value>,
) -> aterm_json::Value {
    repo.as_object_mut()
        .unwrap()
        .insert(key.into(), value.into());
    repo
}

fn with_owner(
    mut repo: aterm_json::Value,
    key: &str,
    value: impl Into<aterm_json::Value>,
) -> aterm_json::Value {
    repo.as_object_mut()
        .unwrap()
        .get_mut("owner")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .insert(key.into(), value.into());
    repo
}

fn keys(object: &aterm_json::Value) -> Vec<&str> {
    object
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect()
}

fn seeded_catalog(repositories: &[aterm_json::Value]) -> (Vec<u8>, RepositoryCatalog) {
    let bytes = aterm_json::to_vec(&aterm_json::json!({"schema":2,"account":"fixture-owner",
        "repositories":repositories}))
    .unwrap();
    let catalog = RepositoryCatalog::parse(&bytes).unwrap();
    (bytes, catalog)
}

/// A repository new to every catalog here, which GitHub lists beside a look's other rows so
/// that the catalog is published and what a refresh drops can be read.
fn newcomer() -> aterm_json::Value {
    listed(9, "newcomer", 1)
}

/// The first repository of what `saved` refreshes to when GitHub lists `observed` and
/// [`newcomer`].
fn refreshed_row(saved: &[aterm_json::Value], observed: &aterm_json::Value) -> aterm_json::Value {
    let (_, catalog) = seeded_catalog(saved);
    let (after, _) = refreshed(
        &catalog,
        &[observed.clone(), newcomer()],
        RepositoryCatalogVisibility::TokenVisible,
    );
    after["repositories"][0].clone()
}

/// A saved object carried forward for a repository GitHub lists, in a catalog published for a
/// change of meaning, loses each key GitHub's row for it carries, at its top and inside
/// `owner`, and keeps the policy code reads and every key GitHub does not send. What comes out
/// is a fixed point of the same observation, and a repository GitHub did not list stays exactly
/// as saved. Where nothing else moved, the saved object stays whole: the keys GitHub's row
/// carries are no part of the catalog's meaning.
#[test]
fn a_refresh_drops_the_saved_keys_githubs_row_carries() {
    let complete = RepositoryCatalogVisibility::AccountComplete;
    let visible = RepositoryCatalogVisibility::TokenVisible;
    let submodules = aterm_json::json!(["vendor/library"]);
    let future = aterm_json::json!({"keep": ["exact", 7]});
    let kept = with_owner(
        with(
            with(
                whole(listed(1, "kept", 2_000), "saved"),
                "required_submodules",
                submodules.clone(),
            ),
            "future_policy",
            future.clone(),
        ),
        "future_policy",
        aterm_json::json!({"keep": true}),
    );
    let unseen = with(
        whole(listed(2, "unseen", 512), "saved"),
        "future_policy",
        future.clone(),
    );
    let saved = [kept.clone(), unseen.clone()];
    let (seeded, catalog) = seeded_catalog(&saved);
    // GitHub's row: the same size, and the same provider-shaped keys at other values.
    let github = whole(listed(1, "kept", 2_000), "github");
    assert_ne!(github["html_url"], kept["html_url"]);

    // Nothing else moved: the saved catalog, whole, is the answer.
    let (after, changes) = refreshed(&catalog, std::slice::from_ref(&github), visible);
    assert_eq!(
        (aterm_json::to_vec(&after).unwrap(), changes),
        (seeded, NONE),
        "the dropped keys alone were published"
    );

    let observed = [github.clone(), newcomer()];
    for visibility in [complete, visible] {
        let bytes = catalog
            .refresh("fixture-owner", 7, &observed, visibility)
            .unwrap();
        let after: aterm_json::Value = aterm_json::from_slice(&bytes).unwrap();
        let row = &after["repositories"][0];
        for key in PROVIDER_SHAPED {
            assert!(
                row.get(key).is_none(),
                "the refreshed object still holds GitHub's {key}"
            );
        }
        assert!(
            row["owner"].get("avatar_url").is_none(),
            "the refreshed owner still holds GitHub's avatar_url"
        );
        let mut expected = PROVIDER_OWNED.to_vec();
        expected.extend(["future_policy", "required_submodules"]);
        expected.sort_unstable();
        assert_eq!(
            keys(row),
            expected,
            "what GitHub owns and the saved keys it does not send, and nothing else"
        );
        assert_eq!(keys(&row["owner"]), ["future_policy", "id", "login"]);
        assert_eq!(row["required_submodules"], submodules);
        assert_eq!(row["future_policy"], future);
        assert_eq!(
            row["owner"]["future_policy"],
            kept["owner"]["future_policy"]
        );

        // A fixed point: the catalog that came out, refreshed by the same observation, is
        // the same bytes, which its publisher reads as nothing to publish.
        let lean = RepositoryCatalog::parse(&bytes).unwrap();
        let again = lean
            .refresh("fixture-owner", 7, &observed, visibility)
            .unwrap();
        assert_eq!(
            String::from_utf8(again).unwrap(),
            String::from_utf8(bytes.clone()).unwrap(),
            "a second refresh by the same observation changed the catalog"
        );
        assert_eq!(lean.changes(&bytes).unwrap(), NONE);

        let rows = after["repositories"].as_array().unwrap();
        let changes = catalog.changes(&bytes).unwrap();
        if visibility == visible {
            assert_eq!(rows.len(), 3);
            assert_eq!(
                rows[1], unseen,
                "a repository GitHub did not list is not as it was saved"
            );
            assert_eq!(
                changes,
                RepositoryCatalogChanges {
                    added: 1,
                    pruned: 1,
                    ..NONE
                }
            );
        } else {
            assert_eq!(rows.len(), 2);
            let retired = RepositoryCatalogChanges {
                added: 1,
                pruned: 1,
                removed: 1,
                ..NONE
            };
            assert_eq!(changes, retired);
        }
    }

    // The policy code reads stays where GitHub's row carries a key of its name.
    let named = with(github.clone(), "required_submodules", "github-sends-this");
    assert_eq!(
        refreshed_row(&saved, &named)["required_submodules"],
        submodules,
        "GitHub's row carrying `required_submodules` replaced or dropped the saved policy"
    );

    // No list of GitHub's names: a key of any name that both hold is GitHub's.
    let invented = "zz_no_such_github_key";
    let both = [with_owner(
        with(kept.clone(), invented, "saved"),
        invented,
        "saved",
    )];
    let row = refreshed_row(
        &both,
        &with_owner(with(github.clone(), invented, "github"), invented, "github"),
    );
    assert!(
        row.get(invented).is_none() && row["owner"].get(invented).is_none(),
        "a saved key GitHub's row carries under a name no list holds stayed: {row:?}"
    );

    // Presence, not value: GitHub sends some keys as null, and they are its keys all the same.
    let null = aterm_json::Value::Null;
    let valued = [with_owner(
        with(
            with(kept.clone(), "homepage", "https://saved.invalid"),
            "license",
            aterm_json::json!({"key": "saved"}),
        ),
        "email",
        "saved@example.invalid",
    )];
    let row = refreshed_row(
        &valued,
        &with_owner(
            with(
                with(github.clone(), "homepage", null.clone()),
                "license",
                null.clone(),
            ),
            "email",
            null,
        ),
    );
    assert!(
        row.get("homepage").is_none()
            && row.get("license").is_none()
            && row["owner"].get("email").is_none(),
        "a saved key GitHub's row carries as null stayed: {row:?}"
    );

    // The prune drops no more than GitHub names: a row with fewer keys (a token GitHub
    // shows less to) leaves the saved ones it lacks.
    let mut fewer = github.clone();
    fewer.as_object_mut().unwrap().remove("pushed_at");
    let row = refreshed_row(&saved, &fewer);
    assert_eq!(row["pushed_at"], kept["pushed_at"]);
    assert!(row.get("html_url").is_none());

    // A saved key is tested against GitHub's row for its own repository: what GitHub's row
    // for another one carries drops nothing here, in either order of the listing.
    let bare = listed(1, "kept", 2_000);
    let carrying = whole(listed(2, "unseen", 512), "github");
    for observed in [
        [bare.clone(), carrying.clone(), newcomer()],
        [carrying, bare, newcomer()],
    ] {
        let (after, changes) = refreshed(&catalog, &observed, complete);
        let rows = after["repositories"].as_array().unwrap();
        assert_eq!(
            rows[0], kept,
            "a saved key that GitHub's row for another repository carries was dropped"
        );
        assert!(
            PROVIDER_SHAPED.iter().all(|key| rows[1].get(key).is_none())
                && rows[1]["owner"].get("avatar_url").is_none(),
            "a saved key that GitHub's row for its own repository carries stayed: {:?}",
            rows[1]
        );
        assert_eq!(rows[1]["future_policy"], future);
        assert_eq!(
            changes,
            RepositoryCatalogChanges {
                added: 1,
                pruned: 1,
                ..NONE
            }
        );
    }
}

/// A repository in both catalogs is in at most one count, taken in the order `other_fields`,
/// `size_class`, `pruned`: one saved whole and observed renamed is another field's alone, and
/// one saved whole whose size left its class is a size's alone. A key the refreshed object
/// keeps as null is a key it holds, not a dropped one. Each look lists [`newcomer`] beside it,
/// so that the catalog is published.
#[test]
fn a_repository_saved_whole_is_in_one_count() {
    let complete = RepositoryCatalogVisibility::AccountComplete;
    let counts = |saved: aterm_json::Value, observed: aterm_json::Value| {
        let (_, catalog) = seeded_catalog(std::slice::from_ref(&saved));
        let (_, changes) = refreshed(&catalog, &[observed, newcomer()], complete);
        assert_eq!(changes.added, 1, "the newcomer");
        RepositoryCatalogChanges {
            added: 0,
            ..changes
        }
    };
    let fat = || whole(listed(1, "kept", 2_000), "saved");
    let github = |name: &str, size: u64| whole(listed(1, name, size), "github");
    let other = RepositoryCatalogChanges {
        other_fields: 1,
        ..NONE
    };
    let pruned = RepositoryCatalogChanges { pruned: 1, ..NONE };
    let moved = RepositoryCatalogChanges {
        size_class: 1,
        ..NONE
    };

    // The dropped keys alone, the size inside its class ((1,792, 2,048] KiB for this one).
    assert_eq!(counts(fat(), github("kept", 2_048)), pruned);
    assert_eq!(
        counts(fat(), github("renamed", 2_048)),
        other,
        "a repository saved whole and renamed is counted for its name, once"
    );
    assert_eq!(
        counts(fat(), github("kept", 2_049)),
        moved,
        "a repository saved whole whose size left its class is counted for its size, once"
    );
    assert_eq!(
        counts(fat(), github("renamed", 2_049)),
        other,
        "a repository saved whole, renamed and resized is counted for its name, once"
    );

    // Policy held as null, at the top and inside `owner`, is no key GitHub sends: the
    // refreshed object holds it, so it is neither a dropped key nor another field.
    let null = aterm_json::Value::Null;
    let policy = |repo| {
        with_owner(
            with(repo, "future_policy", null.clone()),
            "future_policy",
            null.clone(),
        )
    };
    assert_eq!(
        counts(policy(fat()), github("kept", 2_000)),
        pruned,
        "a kept key held as null was counted as another field"
    );
    assert_eq!(
        counts(policy(listed(1, "kept", 2_000)), listed(1, "kept", 2_049)),
        moved,
        "a kept key held as null was counted as another field"
    );
}

/// A saved catalog of whole GitHub rows, observed equal in every field a refresh takes but with
/// sizes moved inside their classes, is the saved catalog. The next change of meaning publishes
/// it lean, with each saved size that stayed in its class; a look over that, with sizes moved
/// inside their classes again, is the same bytes.
#[test]
fn a_whole_saved_catalog_stays_whole_until_its_meaning_moves() {
    let (lean, _) = saved_catalog();
    let fat: Vec<_> = lean["repositories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|repo| whole(repo.clone(), "saved"))
        .collect();
    let (seeded, catalog) = seeded_catalog(&fat);
    let github = |sizes: &[u64], newcomer_too: bool| -> Vec<_> {
        let mut rows: Vec<_> = rows(sizes)
            .into_iter()
            .map(|repo| whole(repo, "github"))
            .collect();
        if newcomer_too {
            rows.push(newcomer());
        }
        rows
    };
    let inside = [1_900, 1_700_000, 0, 700];
    for visibility in [
        RepositoryCatalogVisibility::AccountComplete,
        RepositoryCatalogVisibility::TokenVisible,
    ] {
        let (after, changes) = refreshed(&catalog, &github(&inside, false), visibility);
        assert_eq!(
            (aterm_json::to_vec(&after).unwrap(), changes),
            (seeded.clone(), NONE),
            "a look that found only sizes inside their classes published the whole rows lean"
        );

        let bytes = catalog
            .refresh("fixture-owner", 7, &github(&inside, true), visibility)
            .unwrap();
        let after: aterm_json::Value = aterm_json::from_slice(&bytes).unwrap();
        for row in after["repositories"].as_array().unwrap() {
            assert!(
                PROVIDER_SHAPED.iter().all(|key| row.get(key).is_none())
                    && row["owner"].get("avatar_url").is_none(),
                "a catalog published for a newcomer left a saved row whole: {row:?}"
            );
        }
        assert_eq!(sizes(&after), [2_000, 1_600_015, 0, 512, 1_024]);
        assert_eq!(
            after["repositories"][0]["required_submodules"],
            lean["repositories"][0]["required_submodules"]
        );
        assert_eq!(
            catalog.changes(&bytes).unwrap(),
            RepositoryCatalogChanges {
                added: 1,
                pruned: 4,
                ..NONE
            }
        );
        assert!(bytes.len() < seeded.len());

        // The classes are those of the sizes that publication holds.
        let published = RepositoryCatalog::parse(&bytes).unwrap();
        let again = published
            .refresh(
                "fixture-owner",
                7,
                &github(&[2_048, 1_835_008, 0, 1], true),
                visibility,
            )
            .unwrap();
        assert_eq!(
            String::from_utf8(again.clone()).unwrap(),
            String::from_utf8(bytes).unwrap(),
            "a second look with sizes inside their classes changed the lean catalog"
        );
        assert_eq!(published.changes(&again).unwrap(), NONE);
    }
}

/// The room setup estimates for each repository of `catalog` ([`repos::need`]), in bytes, read
/// as setup reads the vault's entry ([`Inventory`]).
fn room(catalog: &aterm_json::Value) -> Vec<u64> {
    let raw: RawInventory = aterm_json::from_value(catalog.clone()).unwrap();
    Inventory::try_from(raw)
        .unwrap()
        .repositories()
        .iter()
        .map(repos::need)
        .collect()
}

/// GitHub's sizes from about a quarter of `kib` to four times it, about 64 to each doubling,
/// the edges of `kib`'s neighbourhood included.
fn around(kib: u64) -> Vec<u64> {
    let mut sizes = Vec::new();
    let mut github = (kib / 4).max(1);
    while github <= kib.saturating_mul(4) {
        sizes.push(github);
        github += (github / 64).max(1);
    }
    sizes.extend([
        kib.saturating_sub(1).max(1),
        kib,
        kib + 1,
        kib * 2,
        kib * 2 + 1,
    ]);
    sizes
}

/// Saved sizes the room laws refresh: classes, and GitHub's own sizes as 0.202.0 to 0.205.0
/// wrote them, the one a review found under-counted (2 GiB and 1 KiB) among them.
const ROOM_SAVED: [u64; 10] = [
    1, 512, 1_024, 1_025, 2_000, 1_600_015, 2_097_152, 2_097_153, 3_000_000, 6_000_000,
];

/// Setup estimates a clone's room from the size the catalog holds ([`repos::need`]), and the
/// estimate is never under five times GitHub's size at the look that left it there: a size a
/// refresh keeps, a class it wrote or GitHub's own as an earlier build wrote it, is read as its
/// class, which is at or above every size of GitHub's that the refresh keeps it for. The
/// clone of 2026-10-25 that took 4.7 times its size fits in any estimate this law admits.
#[test]
fn setup_never_estimates_a_clone_under_five_times_githubs_size() {
    let complete = RepositoryCatalogVisibility::AccountComplete;
    for saved in ROOM_SAVED {
        let (_, catalog) = catalog_of("fixture-owner", vec![listed(1, "code", saved)]);
        for github in around(saved) {
            let (after, _) = refreshed(&catalog, &[listed(1, "code", github)], complete);
            let need = room(&after)[0];
            assert!(
                need >= 5 * github * 1024,
                "saved {saved} KiB, GitHub's {github} KiB: the catalog holds {:?} KiB, which \
                 setup reads as {need} bytes, under five times GitHub's",
                sizes(&after)
            );
        }
    }
    // The review's case: a raw 2 GiB and 1 KiB, GitHub's size grown to 4 GiB.
    let (_, catalog) = catalog_of("fixture-owner", vec![listed(1, "code", 2_097_153)]);
    let (after, _) = refreshed(&catalog, &[listed(1, "code", 4_194_304)], complete);
    assert!(room(&after)[0] >= 20 * aterm_capacity::floor::GIB);
}

/// A size a refresh writes, for a repository new to the catalog or one whose size left what
/// the catalog held, is read by setup ([`repos::need`]) at under a quarter more than five times
/// GitHub's size: 5 MiB for a size of at most 1 MiB. So a Mac being set up leaves a repository
/// uncloned for want of disk space only where five times GitHub's size leaves less than a
/// quarter of it to spare.
#[test]
fn setup_estimates_a_size_the_catalog_writes_at_under_a_quarter_more_than_five_times_githubs() {
    let complete = RepositoryCatalogVisibility::AccountComplete;
    let bound = |github: u64, need: u64, what: &str| {
        let five = 5 * github * 1024;
        if github <= 1024 {
            assert_eq!(need, 5 * 1024 * 1024, "{what}");
        } else {
            assert!(
                five <= need && 4 * need < 5 * five,
                "{what}: setup reads {need} bytes for GitHub's {github} KiB"
            );
        }
    };
    let (_, empty) = catalog_of("fixture-owner", Vec::new());
    let mut github = 1;
    while github < 1 << 40 {
        for kib in [
            github,
            github + 1,
            github * 5 / 4,
            github * 3 / 2,
            github * 7 / 4,
        ] {
            let (after, _) = refreshed(&empty, &[listed(1, "code", kib)], complete);
            bound(kib, room(&after)[0], "a repository new to the catalog");
        }
        github *= 2;
    }
    for saved in ROOM_SAVED {
        let (_, catalog) = catalog_of("fixture-owner", vec![listed(1, "code", saved)]);
        for github in around(saved) {
            let (after, _) = refreshed(&catalog, &[listed(1, "code", github)], complete);
            if sizes(&after) != [saved] {
                bound(github, room(&after)[0], &format!("saved {saved} KiB"));
            }
        }
    }
    // The review's case: GitHub sizes a new repository at 2 GiB and 1 KiB. Five times that is
    // 10 GiB; a Mac with 15 GiB above its floor clones it.
    let (after, _) = refreshed(&empty, &[listed(1, "code", 2_097_153)], complete);
    assert!(room(&after)[0] <= 15 * aterm_capacity::floor::GIB);
}

/// GitHub's size can move back and forth across the edge of a class, as pushes grow a
/// repository and GitHub's maintenance shrinks it. A look that finds it past the class the
/// catalog holds publishes GitHub's class once; the catalog then holds that class while
/// GitHub's size is over half of it, so a day of looks at such a wobble publishes at most once,
/// at any size. A fall to half the class held publishes GitHub's class.
#[test]
fn a_size_that_moves_back_and_forth_across_a_class_edge_is_published_once() {
    let complete = RepositoryCatalogVisibility::AccountComplete;
    for (saved, high, low) in [
        (1_024, 1_030, 1_020),
        (16_384, 16_390, 16_380),
        (20_480, 20_490, 20_470),
        (1_048_576, 1_048_626, 1_048_526),
        (4_194_304, 4_194_354, 4_194_254),
        (5_242_880, 5_242_930, 5_242_830),
    ] {
        let mut vault = vec![listed(1, "code", saved)];
        let mut signed = 0;
        for look in 0..96 {
            let github = if look % 2 == 0 { high } else { low };
            let (_, catalog) = catalog_of("fixture-owner", vault.clone());
            let (after, changes) = refreshed(&catalog, &[listed(1, "code", github)], complete);
            if moved(changes) {
                signed += 1;
                vault.clone_from(after["repositories"].as_array().unwrap());
            }
        }
        assert!(
            signed <= 1,
            "saved {saved} KiB, GitHub's size moving between {low} and {high} KiB: {signed} of \
             96 looks published"
        );
    }

    // A class held, GitHub's size fallen to just over half of it and then to half.
    let (saved, catalog) = catalog_of("fixture-owner", vec![listed(1, "code", 20_480)]);
    assert_eq!(
        refreshed(&catalog, &[listed(1, "code", 10_241)], complete),
        (saved, NONE)
    );
    let (after, changes) = refreshed(&catalog, &[listed(1, "code", 10_240)], complete);
    assert_eq!(
        (sizes(&after), changes),
        (
            vec![10_240],
            RepositoryCatalogChanges {
                size_class: 1,
                ..NONE
            }
        )
    );
}

/// GitHub's row for a repository may carry no `size`, or a null one. Such a look keeps the size
/// the catalog holds, as GitHub's `0` does, and a repository new to the catalog takes `0`,
/// which setup reads as not sized; the rest of the look is published as any other.
#[test]
fn a_row_without_a_size_keeps_the_saved_size() {
    let complete = RepositoryCatalogVisibility::AccountComplete;
    let (saved, catalog) = saved_catalog();
    let sizeless = |mut repo: aterm_json::Value, null: bool| {
        let fields = repo.as_object_mut().unwrap();
        if null {
            fields.insert("size".into(), aterm_json::Value::Null);
        } else {
            fields.remove("size");
        }
        repo
    };
    for null in [false, true] {
        let observed: Vec<_> = rows(&SAVED_SIZES)
            .into_iter()
            .map(|repo| sizeless(repo, null))
            .collect();
        assert_eq!(
            refreshed(&catalog, &observed, complete),
            (saved.clone(), NONE),
            "null: {null}"
        );
        let mut beside = observed.clone();
        beside.push(sizeless(listed(5, "new", 3), null));
        let (after, changes) = refreshed(&catalog, &beside, complete);
        assert_eq!(sizes(&after), [2_000, 1_600_015, 0, 512, 0], "null: {null}");
        assert_eq!(
            changes,
            RepositoryCatalogChanges { added: 1, ..NONE },
            "null: {null}"
        );
        // One row without a size beside rows with one: those are refreshed as ever.
        let mut mixed = rows(&[2_049, 1_600_000, 0, 700]);
        mixed[1] = sizeless(mixed[1].clone(), null);
        let (after, _) = refreshed(&catalog, &mixed, complete);
        assert_eq!(sizes(&after)[1..], [1_600_015, 0, 512], "null: {null}");
        assert_ne!(sizes(&after)[0], 2_000, "null: {null}");
    }
}
