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

/// The saved catalog the size laws refresh: a repository under 16 MiB (its band is 1 MiB), one
/// over it (its band is a sixteenth of it, rounded down: 16 does not divide this one), one
/// GitHub has not sized and one of 512 KiB.
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

/// What a refresh of `catalog` answers, with its counts. Every refresh the three size laws
/// and `refreshed_row` make passes through here, so each of those is held to this: the
/// counts are all 0 and the flags unset
/// exactly where the answer is, as JSON, the saved catalog, which is where its publisher
/// signs nothing. A publication's log line therefore never reads all zeros.
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
    assert_eq!(
        changes == NONE,
        after == saved,
        "{changes:?} for a refresh that {} the saved catalog",
        if after == saved { "is" } else { "is not" }
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
    size_only: 0,
    added: 0,
    removed: 0,
    pruned: 0,
    account: false,
    order: false,
};

/// A saved size stands while GitHub's is within a sixteenth of it, or within 1 MiB where that
/// is more, and neither is 0: a refresh whose only differences are such sizes is, as JSON, the
/// saved catalog. One step past either edge, a size that was or becomes 0, and a repository
/// new to the catalog each take GitHub's size.
#[test]
fn a_saved_size_stands_while_githubs_is_near_it() {
    let (saved, catalog) = saved_catalog();
    let complete = RepositoryCatalogVisibility::AccountComplete;
    let visible = RepositoryCatalogVisibility::TokenVisible;
    let [small, large, _, tiny] = SAVED_SIZES;
    assert_eq!((small / 16).max(1024), 1024);
    assert_eq!((large / 16).max(1024), 100_000);
    assert_eq!(
        large.div_ceil(16),
        100_001,
        "the band's share is rounded down"
    );

    // Inside the band, both edges included: nothing to publish, under either visibility.
    for near in [
        [small + 1024, large + 100_000, 0, tiny + 1024],
        [small - 1024, large - 100_000, 0, 1],
        [small + 1000, large + 1025, 0, tiny],
        [small - 1, large - 1, 0, tiny + 1],
        SAVED_SIZES,
    ] {
        for visibility in [complete, visible] {
            let (after, changes) = refreshed(&catalog, &rows(&near), visibility);
            assert_eq!(
                after, saved,
                "sizes {near:?} are near the saved ones, so the refresh is the saved catalog"
            );
            assert_eq!(changes, NONE);
        }
    }
    // A token that sees only two of them keeps the others' rows, and their sizes, as saved.
    let (after, changes) = refreshed(&catalog, &rows(&[small + 7, large - 7]), visible);
    assert_eq!((after, changes), (saved.clone(), NONE));

    // One KiB past an edge takes GitHub's size, and the catalog, published for that, carries
    // GitHub's size for every repository it observed.
    let moved = RepositoryCatalogChanges {
        size_only: 1,
        ..NONE
    };
    for far in [
        [small + 1025, large + 9, 0, tiny + 9],
        [small - 1025, large + 9, 0, tiny + 9],
        [small + 9, large + 100_001, 0, tiny + 9],
        [small + 9, large - 100_001, 0, tiny + 9],
        // A size GitHub had not given, and one it no longer gives, are never near.
        [small + 9, large + 9, 700, tiny + 9],
        [small + 9, large + 9, 0, 0],
    ] {
        let (after, changes) = refreshed(&catalog, &rows(&far), complete);
        assert_eq!(sizes(&after), far, "a size outside the band takes GitHub's");
        assert_eq!(changes, moved, "sizes {far:?}");
    }

    // A repository new to the catalog takes GitHub's size, whatever it is.
    let mut observed = rows(&SAVED_SIZES);
    observed.push(listed(5, "new", 3));
    let (after, changes) = refreshed(&catalog, &observed, complete);
    assert_eq!(sizes(&after), [small, large, 0, tiny, 3]);
    assert_eq!(changes, RepositoryCatalogChanges { added: 1, ..NONE });
}

/// A catalog that differs from the saved one in anything but near sizes is published anyway:
/// it holds every field a refresh takes as GitHub gave it, the near sizes included, and its
/// counts say why.
#[test]
fn a_catalog_published_for_another_field_carries_githubs_sizes() {
    let (saved, catalog) = saved_catalog();
    let complete = RepositoryCatalogVisibility::AccountComplete;
    let near = [2_009, 1_600_009, 0, 521];
    let changed = |field: &str, value: aterm_json::Value| {
        let mut observed = rows(&near);
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
        assert_eq!(sizes(&after), near, "{field}");
        assert_eq!(changes, other, "{field}");
        // Policy the administrator attached stays, as before.
        assert_eq!(
            after["repositories"][0]["required_submodules"],
            saved["repositories"][0]["required_submodules"]
        );
    }

    // A rename and a size past the band on one repository count once, as another field; a
    // size past the band on a second counts as size alone; a retired one as removed.
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
    assert_eq!(sizes(&after), [9_000, 1_600_009, 4_000]);
    assert_eq!(
        changes,
        RepositoryCatalogChanges {
            other_fields: 1,
            size_only: 1,
            removed: 1,
            ..NONE
        }
    );
}

/// A saved catalog whose account is spelled otherwise than GitHub spells it, or whose
/// repositories are not sorted by id, is published once although every repository in it is
/// as saved and every size near: the answer holds GitHub's sizes, its flag says why, and a
/// refresh of that answer is that answer.
#[test]
fn a_catalog_published_for_the_account_or_the_order_alone_says_so() {
    let complete = RepositoryCatalogVisibility::AccountComplete;
    let visible = RepositoryCatalogVisibility::TokenVisible;
    let near = [2_009, 1_600_024, 0, 521];
    let ids = |catalog: &aterm_json::Value| -> Vec<u64> {
        catalog["repositories"]
            .as_array()
            .unwrap()
            .iter()
            .map(|repo| repo["id"].as_u64().unwrap())
            .collect()
    };
    let settles = |published: &aterm_json::Value, visibility| {
        let again = RepositoryCatalog::parse(&aterm_json::to_vec(published).unwrap()).unwrap();
        let (after, changes) = refreshed(&again, &rows(&near), visibility);
        assert_eq!((&after, changes), (published, NONE));
    };

    // The account's letter case alone.
    let (saved, catalog) = catalog_of("Fixture-Owner", saved_rows());
    for visibility in [complete, visible] {
        let (after, changes) = refreshed(&catalog, &rows(&near), visibility);
        assert_eq!(after["account"].as_str(), Some("fixture-owner"));
        assert_eq!(
            sizes(&after),
            near,
            "a catalog published for its account's spelling carries GitHub's sizes"
        );
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
        settles(&after, visibility);
    }

    // The saved order alone: ids 4, 3, 1, 2.
    let mut unsorted = saved_rows();
    unsorted.rotate_right(2);
    unsorted.swap(0, 1);
    let (saved, catalog) = catalog_of("fixture-owner", unsorted);
    assert_eq!(ids(&saved), [4, 3, 1, 2]);
    for visibility in [complete, visible] {
        let (after, changes) = refreshed(&catalog, &rows(&near), visibility);
        assert_eq!(ids(&after), [1, 2, 3, 4]);
        assert_eq!(sizes(&after), near);
        assert_eq!(
            changes,
            RepositoryCatalogChanges {
                order: true,
                ..NONE
            }
        );
        settles(&after, visibility);
    }
    // A token that sees two of the four still writes all four sorted; the two it did not
    // see keep their saved rows.
    let (after, changes) = refreshed(&catalog, &rows(&near[..2]), visible);
    assert_eq!(ids(&after), [1, 2, 3, 4]);
    assert_eq!(
        sizes(&after),
        [near[0], near[1], SAVED_SIZES[2], SAVED_SIZES[3]]
    );
    assert_eq!(
        changes,
        RepositoryCatalogChanges {
            order: true,
            ..NONE
        }
    );
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

/// The first repository of what `saved` refreshes to when GitHub lists `observed` alone.
fn refreshed_row(saved: &[aterm_json::Value], observed: &aterm_json::Value) -> aterm_json::Value {
    let (_, catalog) = seeded_catalog(saved);
    let (after, _) = refreshed(
        &catalog,
        std::slice::from_ref(observed),
        RepositoryCatalogVisibility::TokenVisible,
    );
    after["repositories"][0].clone()
}

/// A saved object carried forward for a repository GitHub lists loses each key GitHub's row
/// for it carries, at its top and inside `owner`, and keeps the policy code reads and every
/// key GitHub does not send. What comes out is a fixed point of the same observation, and a
/// repository GitHub did not list stays exactly as saved.
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
    let (_, catalog) = seeded_catalog(&saved);
    // GitHub's row: the same size, and the same provider-shaped keys at other values.
    let github = whole(listed(1, "kept", 2_000), "github");
    assert_ne!(github["html_url"], kept["html_url"]);

    for visibility in [complete, visible] {
        let bytes = catalog
            .refresh(
                "fixture-owner",
                7,
                std::slice::from_ref(&github),
                visibility,
            )
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
            .refresh(
                "fixture-owner",
                7,
                std::slice::from_ref(&github),
                visibility,
            )
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
            assert_eq!(rows.len(), 2);
            assert_eq!(
                rows[1], unseen,
                "a repository GitHub did not list is not as it was saved"
            );
            assert_eq!(changes, RepositoryCatalogChanges { pruned: 1, ..NONE });
        } else {
            assert_eq!(rows.len(), 1);
            let retired = RepositoryCatalogChanges {
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
    for observed in [[bare.clone(), carrying.clone()], [carrying, bare]] {
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
        assert_eq!(changes, RepositoryCatalogChanges { pruned: 1, ..NONE });
    }
}

/// A repository in both catalogs is in at most one count, taken in the order `other_fields`,
/// `pruned`, `size_only`: one saved whole and observed renamed is another field's alone, and
/// one saved whole whose size left its band is a pruned one alone. A key the refreshed object
/// keeps as null is a key it holds, not a dropped one.
#[test]
fn a_repository_saved_whole_is_in_one_count() {
    let complete = RepositoryCatalogVisibility::AccountComplete;
    let counts = |saved: aterm_json::Value, observed: aterm_json::Value| {
        let (_, catalog) = seeded_catalog(std::slice::from_ref(&saved));
        refreshed(&catalog, std::slice::from_ref(&observed), complete).1
    };
    let fat = || whole(listed(1, "kept", 2_000), "saved");
    let github = |name: &str, size: u64| whole(listed(1, name, size), "github");
    let other = RepositoryCatalogChanges {
        other_fields: 1,
        ..NONE
    };
    let pruned = RepositoryCatalogChanges { pruned: 1, ..NONE };

    // The dropped keys alone, the size inside its band (1 MiB for this one).
    assert_eq!(counts(fat(), github("kept", 2_000 + 1024)), pruned);
    assert_eq!(
        counts(fat(), github("renamed", 2_000 + 1024)),
        other,
        "a repository saved whole and renamed is counted for its name, once"
    );
    assert_eq!(
        counts(fat(), github("kept", 2_000 + 1025)),
        pruned,
        "a repository saved whole whose size left its band is counted as pruned, once"
    );
    assert_eq!(
        counts(fat(), github("renamed", 2_000 + 1025)),
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
    let moved = RepositoryCatalogChanges {
        size_only: 1,
        ..NONE
    };
    assert_eq!(
        counts(
            policy(listed(1, "kept", 2_000)),
            listed(1, "kept", 2_000 + 1025)
        ),
        moved,
        "a kept key held as null was counted as another field"
    );
}

/// With the size band: a saved catalog of whole GitHub rows, observed equal in every field a
/// refresh takes but with sizes moved inside the band, comes out lean and holds GitHub's
/// sizes; a second look over that, with sizes moved again inside the band, is the same bytes.
#[test]
fn a_whole_saved_catalog_comes_out_lean_with_githubs_sizes() {
    let (lean, _) = saved_catalog();
    let fat: Vec<_> = lean["repositories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|repo| whole(repo.clone(), "saved"))
        .collect();
    let (seeded, catalog) = seeded_catalog(&fat);
    let github = |sizes: &[u64]| -> Vec<_> {
        rows(sizes)
            .into_iter()
            .map(|repo| whole(repo, "github"))
            .collect()
    };
    for visibility in [
        RepositoryCatalogVisibility::AccountComplete,
        RepositoryCatalogVisibility::TokenVisible,
    ] {
        let near = [2_009, 1_600_009, 0, 521];
        let bytes = catalog
            .refresh("fixture-owner", 7, &github(&near), visibility)
            .unwrap();
        let after: aterm_json::Value = aterm_json::from_slice(&bytes).unwrap();
        for row in after["repositories"].as_array().unwrap() {
            assert!(
                PROVIDER_SHAPED.iter().all(|key| row.get(key).is_none())
                    && row["owner"].get("avatar_url").is_none(),
                "a look that found only sizes inside the band left a saved row whole: {row:?}"
            );
        }
        assert_eq!(
            sizes(&after),
            near,
            "the publication that makes the catalog lean carries GitHub's sizes"
        );
        assert_eq!(
            after["repositories"][0]["required_submodules"],
            lean["repositories"][0]["required_submodules"]
        );
        assert_eq!(
            catalog.changes(&bytes).unwrap(),
            RepositoryCatalogChanges { pruned: 4, ..NONE }
        );
        assert!(bytes.len() < seeded.len());

        // The band is measured from the sizes that publication holds.
        let published = RepositoryCatalog::parse(&bytes).unwrap();
        let again = published
            .refresh(
                "fixture-owner",
                7,
                &github(&[2_009 + 1024, 1_600_009 - 100_000, 0, 1]),
                visibility,
            )
            .unwrap();
        assert_eq!(
            String::from_utf8(again.clone()).unwrap(),
            String::from_utf8(bytes).unwrap(),
            "a second look with sizes inside the band changed the lean catalog"
        );
        assert_eq!(published.changes(&again).unwrap(), NONE);
    }
}
