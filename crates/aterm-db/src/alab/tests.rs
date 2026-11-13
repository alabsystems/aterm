// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The lab registry's schemas, seeds and Commons reads. The Commons tests run real labs; every
//! address is synthetic (`example.invalid`).

use super::{
    registry::{
        self, ACCOUNT, ADDRESS, COLLECTIONS, MACHINE, MACHINE_FACTS, MAILBOX, MODEL, PHONE,
        PROVIDER,
    },
    seeds, *,
};
use crate::{
    database::{Change, MAX_WRITES},
    table::{
        Session,
        tests::{
            Lab,
            frontier::{first_records, receive, wide_lab, wires},
            repair_commons,
        },
    },
    typed::{
        Derive, SCHEMA, Ty,
        cell::{self, CellKey, CellValue, Cells},
        keys,
        owned::SchemaState,
    },
};
use astream_wire::sync::{Digest, Header, Index, MAX_PARENTS};
use aterm_json::{Value, json};
use aterm_secrets::commons::roster::Role;

#[test]
fn a_registry_read_keeps_vault_contention_distinct_from_absence() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let registry = Registry::at(paths.clone());
    registry.open().unwrap();
    let config = paths.home.join("aterm.toml");
    assert!(matches!(
        registry.lab(&config, None),
        Ok(Some(LabRead::Projected(..)))
    ));
    let writer = aterm_secrets::client::test_hold_state_lock(paths).unwrap();
    let read = registry.lab(&config, None);
    let table = registry.table();
    let imports = crate::import::commons::Imports::at(paths.clone()).snapshot_if_declared();
    let proposals = crate::accounts::proposals::Proposals::at(paths.clone()).snapshot_if_declared();
    drop(writer);
    assert!(
        read.expect_err("a busy vault is not an absent registry")
            .is_busy()
    );
    assert!(
        table
            .expect_err("a busy vault is not an absent table")
            .is_busy()
    );
    assert!(
        imports
            .expect_err("busy imports must not look absent")
            .is_busy()
    );
    assert!(
        proposals
            .expect_err("busy proposals must not look absent")
            .is_busy()
    );
    assert!(matches!(
        registry.lab(&config, None),
        Ok(Some(LabRead::Projected(..)))
    ));
}

#[test]
fn a_signed_vault_without_commons_is_a_known_absent_source() {
    use aterm_secrets::commons::{
        Unavailable,
        bootstrap::{Availability, Bootstrap},
    };
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = aterm_secrets::client::Paths::for_home(&root.path().join("home"));
    let vault = root.path().join("team.vault");
    aterm_secrets::client::test_create_unbound(&paths, vault.to_str().unwrap(), None, Some("m1"))
        .unwrap();
    assert!(matches!(
        Bootstrap::observe(&paths).unwrap(),
        Availability::Unavailable(Unavailable::ControlNotVisible)
    ));
    let registry = Registry::at(paths.clone());
    assert!(registry.table().unwrap().is_none());
    assert!(
        registry
            .lab(&paths.home.join("aterm.toml"), None)
            .unwrap()
            .is_none()
    );
    assert!(
        crate::import::commons::Imports::at(paths.clone())
            .snapshot_if_declared()
            .unwrap()
            .is_none()
    );
    assert!(
        crate::accounts::proposals::Proposals::at(paths)
            .snapshot_if_declared()
            .unwrap()
            .is_none()
    );
}

/// Every collection a type refers to.
fn refs(ty: &Ty, out: &mut Vec<&'static str>) {
    match ty {
        Ty::Ref(collection) => out.push(collection),
        Ty::Set(inner) | Ty::List(inner, _) => refs(inner, out),
        Ty::Struct(members) => members.iter().for_each(|m| refs(&m.ty, out)),
        _ => {}
    }
}

/// The registry's schemas keep secrets and addresses where the design puts them: every
/// reference names a registry collection, every unique rule a column; the one column that can
/// hold an address is `address.addr`, so no mailbox login, `receives` pattern or other address
/// rides any other column; mailboxes and phones have random keys; no address kind names a
/// mailbox login; and each schema row fits one record.
/// NEGATIVE CONTROL: with a `login` column typed `email` added to `MAILBOX`, the pii assertion
/// fails: `` left: ["address.addr", "mailbox.login"] ``.
#[test]
fn registry_schemas_keep_secrets_and_addresses_out() {
    let names: Vec<&str> = COLLECTIONS.iter().map(|c| c.name).collect();
    let mut pii = Vec::new();
    for collection in COLLECTIONS {
        assert_eq!(collection.check(), Ok(()), "{}", collection.name);
        for column in collection.columns {
            let mut named = Vec::new();
            refs(&column.ty, &mut named);
            for target in named {
                assert!(
                    names.contains(&target),
                    "{}.{}",
                    collection.name,
                    column.name
                );
            }
            if column.ty.pii() {
                pii.push(format!("{}.{}", collection.name, column.name));
            }
            if let Ty::Enum(words) = column.ty {
                assert!(!words.contains(&"mailbox-login"), "{}", collection.name);
            }
        }
        for rule in collection.unique {
            for column in *rule {
                assert!(collection.column(column).is_some(), "{}", collection.name);
            }
        }
        let row = vec![Change {
            collection: SCHEMA.into(),
            key: collection.name.into(),
            value: Some(collection.schema_row()),
        }];
        assert!(
            crate::database::Command::Write {
                request: [0; 16],
                changes: row
            }
            .encode()
            .unwrap()
            .len()
                <= crate::crypto::MAX_BODY
        );
    }
    assert_eq!(pii, ["address.addr"]);
    // A mailbox's key derives from its login's address key (a member-readable row); a phone's
    // stays random, since its number is a low-entropy secret.
    assert_eq!(
        (MAILBOX.derive, PHONE.derive),
        (Derive::Composite, Derive::Random)
    );
    // A mailbox's patterns name address rows; a domain pattern's dots are no key, so they list.
    assert!(matches!(
        MAILBOX.column("receives").map(|c| &c.ty),
        Some(Ty::Set(inner)) if matches!(**inner, Ty::Ref("address"))
    ));
    assert!(matches!(
        MAILBOX.column("receives_domains").map(|c| &c.ty),
        Some(Ty::List(..))
    ));
    assert!(matches!(
        MAILBOX.column("login").map(|c| &c.ty),
        Some(Ty::Ref("address"))
    ));
    for (collection, column) in [
        (&ADDRESS, "password"),
        (&MAILBOX, "mailbox_password"),
        (&MAILBOX, "totp"),
        (&PHONE, "number"),
    ] {
        assert!(
            matches!(collection.column(column).map(|c| &c.ty), Some(Ty::Secret)),
            "{}.{column}",
            collection.name
        );
    }
}

/// Apply `records` as one writer's chain to `cells`, as the Commons projection does.
pub(super) fn apply(records: &[Vec<Change>]) -> Cells {
    let mut index = Index::new(1024);
    let mut cells = Cells::new();
    let mut previous = None;
    for (n, changes) in records.iter().enumerate() {
        let mut id: Digest = [0; 32];
        id[0] = u8::try_from(n + 1).unwrap();
        index
            .insert(Header {
                actor: [7; 48],
                sequence: n as u64 + 1,
                previous,
                parents: previous.into_iter().collect(),
                id,
            })
            .unwrap();
        crate::documents::apply(&index, &mut cells, id, changes, false);
        previous = Some(id);
    }
    cells
}

/// The compiled seeds pass their columns' types, name one provider word per provider (the
/// importer's and the account key's), give each CLI a seeded provider and a canonical realm, key
/// each plan by its provider, write automatic cells only, and once applied call for nothing more.
/// NEGATIVE CONTROL: with Kimi's seed keyed `moonshot`, the provider words differ from
/// `keys::PROVIDERS`: `` left: ["anthropic", "openai", "google", "meta", "moonshot", "xai"] ``.
#[test]
fn seeds_fit_their_schemas_and_share_the_provider_words() {
    let words: Vec<&str> = seeds::PROVIDERS.iter().map(|p| p.word).collect();
    assert_eq!(words, keys::PROVIDERS);
    assert_eq!(
        crate::import::provider_words().collect::<Vec<_>>(),
        keys::PROVIDERS
    );
    for provider in &seeds::PROVIDERS {
        for realm in provider.realms {
            assert_eq!(keys::canonical_realm(realm), Some(*realm));
            assert_eq!(keys::provider_of_realm(realm), Some(provider.word));
        }
    }
    for cli in &seeds::CLIS {
        assert!(words.contains(&cli.provider), "{}", cli.id);
        if cli.pool_realm != "none" {
            assert_eq!(keys::provider_of_realm(cli.pool_realm), Some(cli.provider));
        }
    }
    for plan in &seeds::PLANS {
        assert!(
            plan.pk.starts_with(&format!("{}-", plan.provider)),
            "{}",
            plan.pk
        );
        assert!(crate::typed::pk_grammar(plan.pk));
    }
    let rows = seeds::rows_for(&Cells::new()).unwrap();
    assert_eq!(rows.len(), 6 + 7 + 7);
    for change in rows.iter().flatten() {
        assert_eq!(change.value.as_ref().unwrap()["a"], Value::Bool(true));
    }
    assert!(rows.iter().all(|row| row.len() <= MAX_WRITES));
    let cells = apply(&rows);
    let kimi = cell::project(&PROVIDER, &cells).rows["kimi"].clone();
    assert_eq!(
        kimi.get("realms"),
        Some(&json!(["kimi:global", "kimi:mainland-cn"]))
    );
    assert!(seeds::rows_for(&cells).unwrap().is_empty());
}

/// An owner's value for `col` of provider `pk`.
fn owner_value(pk: &str, col: &str, x: Value) -> Change {
    cell::change(
        &PROVIDER,
        CellKey {
            pk,
            col,
            elem: None,
        },
        &CellValue::owner(1, x),
    )
    .unwrap()
}

/// A seed pass writes the compiled rows into the cells a complete scan finds absent, and nothing
/// once they are held: a re-run, a run after the owner changed a seeded cell (which it leaves as
/// the owner set it), and a second Mac's run after repair all write no record.
/// NEGATIVE CONTROL: with `seeds::held` always false (every seeded cell written whatever the
/// scan found), the run after the owner's edit rebuilds the first pass's records and finds them
/// as retries: `` left: Seeded { records: 0, retried: 2 } ``.
#[test]
fn seeds_go_only_into_cells_a_complete_scan_found_absent() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let first = Registry::at(lab.paths[0].clone());
    first.open().unwrap();
    let seeded = first.seed().unwrap();
    assert!(seeded.records >= 1 && seeded.retried == 0, "{seeded:?}");
    let view = first.view().unwrap();
    assert!(view.complete);
    let providers = view.project(&PROVIDER);
    assert_eq!(providers.rows.len(), 6);
    assert_eq!(
        providers.rows["anthropic"].get("status"),
        Some(&json!("active"))
    );
    let clis = view.project(&registry::CLI);
    assert_eq!(
        clis.rows["claude"].fields["readings"].members(),
        ["cli-output", "statusline"].into_iter().collect()
    );
    first
        .write(vec![vec![owner_value(
            "anthropic",
            "status",
            json!("watch"),
        )]])
        .unwrap();
    assert_eq!(first.seed().unwrap(), Seeded::default());
    assert_eq!(
        first.view().unwrap().project(&PROVIDER).rows["anthropic"].get("status"),
        Some(&json!("watch"))
    );
    repair_commons(&lab, 6);
    let second = Registry::at(lab.paths[1].clone());
    assert_eq!(second.seed().unwrap(), Seeded::default());
    assert_eq!(second.view().unwrap().project(&PROVIDER).rows.len(), 6);
}

/// This Mac's Commons machine uuid.
pub(super) fn machine_of(lab: &Lab, n: usize) -> [u8; 16] {
    lab.machine(n)
}

/// A Mac model's birth: one automatic cell.
fn birth(model: &str) -> Vec<Change> {
    vec![
        cell::change(
            &MODEL,
            CellKey {
                pk: model,
                col: "row",
                elem: None,
            },
            &CellValue::automatic(1, json!("live")),
        )
        .unwrap(),
    ]
}

/// One remaining automatic slot cannot hold a causal join and a seed record. Refusal
/// appends neither; another writer with room still seeds the registry and shares its rows.
/// Macs 1 and the last, past `LEGACY_MAX_MEMBERS`, write through the product; the others seal what
/// a first `Registry::write` appends, since a seed's reservation depends on the frontier's width
/// and not on how its peers wrote.
/// NEGATIVE CONTROLS: counting only the seed grows 87 retained records to 89 on refusal;
/// reserving new joins but ignoring pending ones grows 89 to 90 on the interrupted retry.
#[test]
fn seeding_reserves_causal_joins_within_the_remaining_automatic_quota() {
    let lab = wide_lab(MAX_PARENTS + 1);
    let first = Registry::at(lab.paths[0].clone());
    let scope = first.open().unwrap();
    first
        .write(
            (0..registry::SELF_QUOTA - 1)
                .map(|n| birth(&format!("vm-first-{n}")))
                .collect(),
        )
        .unwrap();
    let last = lab.paths.len() - 1;
    for n in [1, last] {
        Registry::at(lab.paths[n].clone())
            .write(vec![birth(&format!("vm-peer-{n}"))])
            .unwrap();
    }
    let mut originals: Vec<_> = [0, 1, last]
        .into_iter()
        .flat_map(|n| wires(&lab, n, scope))
        .collect();
    originals.extend(first_records(&lab, 2..last, scope, |n, _| {
        crate::database::Command::Write {
            request: [u8::try_from(n).unwrap(); 16],
            changes: birth(&format!("vm-peer-{n}")),
        }
        .encode()
        .unwrap()
    }));
    for n in 0..2 {
        receive(&lab, n, scope, &originals);
    }
    let actor = {
        let bootstrap = lab.bootstrap(0);
        let session = Session::observe(&bootstrap).unwrap();
        let directory = crate::catalog::Directory::new(&lab.paths[0], bootstrap.trust());
        let store =
            crate::table::store::Store::observe(&session, &directory.path(scope).unwrap(), scope)
                .unwrap();
        let (_, needed) = store.metadata_with_append_bound(&session).unwrap();
        assert!(needed > 1, "this seed needs a causal join as well as data");
        session.actor()
    };
    assert_eq!(
        first.view().unwrap().quota.spent[&actor],
        registry::SELF_QUOTA - 1
    );
    let before = wires(&lab, 0, scope);
    assert!(first.seed().is_err());
    let after = wires(&lab, 0, scope);
    assert_eq!(after.len(), before.len(), "a refused seed appends nothing");
    assert!(after == before, "all retained records stay unchanged");

    // An interrupted append can leave joins with no following business record. They
    // will be charged to the next automatic record even though they are retained already.
    {
        let bootstrap = lab.bootstrap(0);
        let session = Session::open(&bootstrap).unwrap();
        let directory = crate::catalog::Directory::new(&lab.paths[0], bootstrap.trust());
        let mut store =
            crate::table::store::Store::open(&session, &directory.path(scope).unwrap(), scope)
                .unwrap();
        let merge = crate::database::Command::Merge { request: [83; 16] };
        crate::documents::application::append(&session, &mut store, &merge.encode().unwrap())
            .unwrap();
        assert_eq!(store.metadata_with_append_bound(&session).unwrap().1, 1);
    }
    assert_eq!(
        first.view().unwrap().quota.spent[&actor],
        registry::SELF_QUOTA - 1
    );
    let interrupted = wires(&lab, 0, scope);
    assert!(interrupted.len() > before.len());
    assert!(first.seed().is_err());
    let retried = wires(&lab, 0, scope);
    assert_eq!(
        retried.len(),
        interrupted.len(),
        "a retry reserves prior joins"
    );
    assert!(retried == interrupted, "the retry appends no seed");

    let second = Registry::at(lab.paths[1].clone());
    assert!(second.seed().unwrap().records > 0);
    let view = second.view().unwrap();
    assert!(view.quota.over.is_empty());
    assert_eq!(view.project(&PROVIDER).rows.len(), seeds::PROVIDERS.len());
    assert_eq!(second.seed().unwrap(), Seeded::default());
    receive(&lab, 0, scope, &wires(&lab, 1, scope));
    let recovered = first.view().unwrap();
    assert_eq!(recovered.cells, view.cells);
    assert!(recovered.quota.over.is_empty());
    assert_eq!(first.seed().unwrap(), Seeded::default());
}

/// A writer's automatic records past its quota never project: of 26 automatic records (24 model
/// births, then a facts row at its own key and a 25th birth), the first 24 project and the last
/// two do not, while the same Mac's owner record still does; its next seed pass is refused.
/// NEGATIVE CONTROL: with `View::read` taking the cells of every eligible operation
/// (`log.snapshot().cells` in place of `cells_without`), the 26th birth projects:
/// `` assertion failed: !models.rows.contains_key("vm-25") ``.
#[test]
fn automatic_records_past_the_quota_never_project() {
    let lab = Lab::new(&[Role::Writer]);
    let registry = Registry::at(lab.paths[0].clone());
    registry.open().unwrap();
    let me = keys::uuid32(&machine_of(&lab, 0));
    let facts = Change {
        collection: MACHINE_FACTS.name.into(),
        key: me.clone(),
        value: Some(json!({"r": 1, "n": 1, "mac": "mc-0000000000000000", "label": "m9"})),
    };
    let mut records: Vec<Vec<Change>> = (1..=24).map(|n| birth(&format!("vm-{n}"))).collect();
    records.push(vec![facts]);
    records.push(birth("vm-25"));
    records.push(vec![owner_value("xai", "status", json!("watch"))]);
    registry.write(records).unwrap();
    let view = registry.view().unwrap();
    let models = view.project(&MODEL);
    assert!(models.rows.contains_key("vm-24"));
    assert!(!models.rows.contains_key("vm-25"));
    assert!(view.owned[MACHINE_FACTS.name].rows.is_empty());
    assert_eq!(view.quota.over.len(), 2);
    assert_eq!(view.quota.spent.values().copied().collect::<Vec<_>>(), [26]);
    assert_eq!(
        view.project(&PROVIDER).rows["xai"].get("status"),
        Some(&json!("watch"))
    );
    assert!(registry.seed().is_err());
}

/// A Mac's facts row counts only at its own machine key: at another key it is inert.
/// NEGATIVE CONTROL: with `View::read` reading no owned collection, the own-key row is missing:
/// `` called `Option::unwrap()` on a `None` value ``.
#[test]
fn a_facts_row_counts_only_at_its_writers_own_key() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let registry = Registry::at(lab.paths[0].clone());
    registry.open().unwrap();
    let row = |key: String| Change {
        collection: MACHINE_FACTS.name.into(),
        key,
        value: Some(json!({"r": 1, "n": 1, "mac": "mc-0000000000000000", "label": "m9"})),
    };
    let me = keys::uuid32(&machine_of(&lab, 0));
    let peer = keys::uuid32(&machine_of(&lab, 1));
    registry
        .write(vec![vec![row(me.clone())], vec![row(peer)]])
        .unwrap();
    let view = registry.view().unwrap();
    let facts = &view.owned[MACHINE_FACTS.name];
    assert_eq!(facts.rows.get(&me).unwrap().value["label"], json!("m9"));
    assert_eq!((facts.rows.len(), facts.inert), (1, 1));
}

/// Schema rows go in once, by the owner verb, and never below a version already visible: a
/// newer build's `machine` row at version 2 keeps this build's version-1 row out, and the reader
/// shows version 2. The source's earlier free-text coordinates upgrade once to version 3's keys.
/// NEGATIVE CONTROL: with `schema_rows` publishing every collection whatever is stored, the
/// version-1 `machine` row is written beside the newer one: `` left: 2 ``.
#[test]
fn schema_rows_are_published_once_and_never_below_a_visible_version() {
    let lab = Lab::new(&[Role::Writer]);
    let registry = Registry::at(lab.paths[0].clone());
    registry.open().unwrap();
    let newer = json!({"r": 2, "x": {"format": "att/1", "version": 2}});
    let mut earlier_source = registry::SOURCE.schema_row();
    let object = earlier_source.as_object_mut().unwrap();
    object.insert("r".into(), json!(2));
    let schema = object.get_mut("x").unwrap().as_object_mut().unwrap();
    schema.insert("version".into(), json!(2));
    let differs = schema
        .get_mut("columns")
        .unwrap()
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|column| column["name"] == json!("differs"))
        .unwrap()
        .as_object_mut()
        .unwrap()
        .get_mut("type")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .get_mut("list")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .get_mut("struct")
        .unwrap()
        .as_array_mut()
        .unwrap();
    for member in differs {
        let max = match member["name"].as_str().unwrap() {
            "pk" => 32,
            "elem" => 64,
            _ => continue,
        };
        member
            .as_object_mut()
            .unwrap()
            .insert("type".into(), json!({"text": max}));
    }
    registry
        .write(vec![vec![
            Change {
                collection: SCHEMA.into(),
                key: "machine".into(),
                value: Some(newer.clone()),
            },
            Change {
                collection: SCHEMA.into(),
                key: registry::SOURCE.name.into(),
                value: Some(earlier_source),
            },
        ]])
        .unwrap();
    assert_eq!(registry.publish_schema().unwrap(), 1);
    assert_eq!(registry.publish_schema().unwrap(), 0);
    let view = registry.view().unwrap();
    let machine_rows = view
        .ops
        .iter()
        .flat_map(|op| &op.changes)
        .filter(|c| c.collection == SCHEMA && c.key == "machine")
        .count();
    assert_eq!(machine_rows, 1);
    assert_eq!(
        view.schema("machine"),
        SchemaState::One {
            version: 2,
            row: newer
        }
    );
    assert_eq!(
        view.schema("account"),
        SchemaState::One {
            version: 1,
            row: ACCOUNT.schema_row()
        }
    );
    let address = view.schema(ADDRESS.name);
    assert!(matches!(address, SchemaState::One { version: 2, .. }));
    assert_eq!(
        view.schema(registry::SOURCE.name),
        SchemaState::One {
            version: 3,
            row: registry::SOURCE.schema_row()
        }
    );
    assert_eq!(
        view.ops
            .iter()
            .flat_map(|op| &op.changes)
            .filter(|c| c.collection == SCHEMA && c.key == registry::SOURCE.name)
            .count(),
        2
    );
}

/// A new grid row of a preference is born under the key of its machine and account, whichever
/// way round the columns come, and refuses without both; the registry names no machine slot; and
/// an account reads as its label, else its default, which names no machine.
/// NEGATIVE CONTROL: with the `preference` arm removed from `registry::birth`, the grid refuses
/// the row: `` called `Result::unwrap()` on an `Err` value: Error("preference: the grid adds no
/// rows here; a lab verb or the importer keys them") ``.
#[test]
fn a_preference_is_born_from_its_pair_and_an_account_reads_its_label() {
    let machine = keys::machine_pk("m8");
    let account = keys::account_pk("claude", "ann@example.test").unwrap();
    let values = std::collections::BTreeMap::from([
        ("machine".to_owned(), json!(machine)),
        ("account".to_owned(), json!(account)),
    ]);
    assert_eq!(
        registry::birth(&registry::PREFERENCE, &values, &Cells::new()).unwrap(),
        keys::preference_pk(&machine, &account).unwrap()
    );
    let partial = std::collections::BTreeMap::from([("machine".to_owned(), json!(machine))]);
    assert!(registry::birth(&registry::PREFERENCE, &partial, &Cells::new()).is_err());
    assert_eq!(registry::REGISTRY.ends, ["preference"]);
    assert!(MACHINE.column("max_seats").is_none());
    assert!(ACCOUNT.column("class").is_none());
    let change = |col: &str, x: Value| {
        cell::change(
            &ACCOUNT,
            CellKey {
                pk: &account,
                col,
                elem: None,
            },
            &CellValue::owner(1, x),
        )
        .unwrap()
    };
    let unnamed = apply(&[vec![change("realm", json!("claude"))]]);
    let row = cell::project(&ACCOUNT, &unnamed).rows[&account].clone();
    assert_eq!(
        registry::account_label(&account, &row),
        Some(keys::default_account_label("claude", &account).unwrap())
    );
    let named = apply(&[vec![change("label", json!("research"))]]);
    let row = cell::project(&ACCOUNT, &named).rows[&account].clone();
    assert_eq!(
        registry::account_label(&account, &row).as_deref(),
        Some("research")
    );
}

/// What one read of the registry gives the harness: a partial history before anything else is
/// read, nothing new where the inventory is the one the caller holds, else the projection and
/// the inventory it was made from. `Registry::lab`'s own answer on a real partial history is
/// [`a_registry_lab_read_is_partial_while_an_operation_awaits_its_parent`]'s.
/// NEGATIVE CONTROL: with the `!complete` check after `inventory()`, `unread` panics.
#[test]
fn a_registry_read_is_partial_before_anything_else_and_projects_only_a_moved_inventory() {
    let unread = || -> Inventory { panic!("a partial history reads no inventory") };
    let unprojected =
        || -> crate::Result<view::LabView> { panic!("nothing is projected where nothing moved") };
    let held = Inventory([3; 32]);
    assert!(
        matches!(
            store::lab_read(false, unread, Some(held), unprojected),
            Ok(LabRead::Partial)
        ),
        "a partial history"
    );
    assert!(
        matches!(
            store::lab_read(true, || held, Some(held), unprojected),
            Ok(LabRead::Unchanged)
        ),
        "the held inventory"
    );
    let moved = Inventory([4; 32]);
    let mut named = view::LabView::default();
    named.me = Some("pk-me".into());
    match store::lab_read(true, || moved, Some(held), || Ok(named.clone())).unwrap() {
        LabRead::Projected(view, inventory) => {
            assert_eq!(*view, named, "the projection");
            assert_eq!(inventory, moved, "the inventory it was made from");
        }
        other => panic!("a moved inventory projects: {other:?}"),
    }
    assert!(
        matches!(
            store::lab_read(true, || held, None, || Ok(view::LabView::default())),
            Ok(LabRead::Projected(_, inventory)) if inventory == held
        ),
        "a first read projects"
    );
}

/// On a real lab, a Mac that holds an operation of the registry but not its predecessor reads a
/// partial history (`LabRead::Partial`), never no lab, and projects once the predecessor arrives.
/// NEGATIVE CONTROL: with `Registry::lab` reading a partial history as no lab
/// (`if !view.complete { return Ok(None) }` before `lab_read`, as before), `` a partial
/// history `` fails, and the harness drops its lab where it should go pending.
#[test]
fn a_registry_lab_read_is_partial_while_an_operation_awaits_its_parent() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let first = Registry::at(lab.paths[0].clone());
    let scope = first.open().unwrap();
    first.write(vec![birth("vm-held")]).unwrap();
    repair_commons(&lab, 6);
    let second = Registry::at(lab.paths[1].clone());
    let config = lab.paths[1].home.join("config.toml");
    let vault = aterm_secrets::client::config(&lab.paths[1])
        .unwrap()
        .unwrap();
    let projects = |what: &str| {
        assert!(
            matches!(second.lab(&config, None), Ok(Some(LabRead::Projected(..)))),
            "{what}"
        );
    };
    projects("a complete history projects");

    let new_since = |before: &[Vec<u8>]| -> Vec<Vec<u8>> {
        wires(&lab, 0, scope)
            .into_iter()
            .filter(|wire| !before.contains(wire))
            .collect()
    };
    let held = wires(&lab, 0, scope);
    first.write(vec![birth("vm-parent")]).unwrap();
    let parent = new_since(&held);
    first.write(vec![birth("vm-child")]).unwrap();
    let child: Vec<_> = new_since(&held)
        .into_iter()
        .filter(|wire| !parent.contains(wire))
        .collect();
    assert!(!parent.is_empty() && !child.is_empty());
    receive(&lab, 1, scope, &child);
    assert!(
        !second.view().unwrap().complete,
        "the child waits on its parent"
    );
    assert!(
        matches!(second.lab(&config, None), Ok(Some(LabRead::Partial))),
        "a partial history"
    );
    assert!(
        second
            .with_admitted_projection::<()>(&config, &lab.paths[1], &vault, None, |_, _| panic!(
                "partial source admitted"
            ))
            .is_err()
    );
    assert!(second.observe_projection(&config).is_err());
    receive(&lab, 1, scope, &parent);
    assert!(second.view().unwrap().complete, "the parent arrived");
    projects("a history made whole projects");
    second.observe_projection(&config).unwrap();
    second
        .with_admitted_projection(&config, &lab.paths[1], &vault, None, |_, _| Ok(()))
        .unwrap();
}
