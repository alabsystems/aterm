// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The lab registry's schemas, seeds and Commons reads. The Commons tests run real labs; every
//! address is synthetic (`example.invalid`).

use super::{
    registry::{
        self, ACCOUNT, ADDRESS, COLLECTIONS, MACHINE_FACTS, MAILBOX, MODEL, PHONE, PROVIDER,
    },
    seeds, *,
};
use crate::{
    database::{Change, MAX_WRITES},
    table::{
        Session,
        tests::{Lab, repair_commons},
    },
    typed::{
        Derive, Kind, SCHEMA, Ty,
        cell::{self, CellKey, CellValue, Cells},
        keys,
        owned::SchemaState,
    },
};
use astream_wire::sync::{Digest, Header, Index};
use aterm_json::{Value, json};
use aterm_secrets::commons::roster::Role;

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
    assert_eq!(
        (MAILBOX.derive, PHONE.derive),
        (Derive::Random, Derive::Random)
    );
    assert!(MAILBOX.column("receives").is_none() && MAILBOX.column("login").is_none());
    assert_eq!(registry::REGISTRY.self_quota, 24);
    assert_eq!(MACHINE_FACTS.kind, Kind::Owned);
}

/// Apply `records` as one writer's chain to `cells`, as the Commons projection does.
fn apply(records: &[Vec<Change>]) -> Cells {
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
fn machine_of(lab: &Lab, n: usize) -> [u8; 16] {
    let bootstrap = lab.bootstrap(n);
    let session = Session::observe(&bootstrap).unwrap();
    *session
        .members()
        .iter()
        .find(|binding| binding.actor() == session.actor())
        .unwrap()
        .machine()
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
/// shows version 2.
/// NEGATIVE CONTROL: with `schema_rows` publishing every collection whatever is stored, the
/// version-1 `machine` row is written beside the newer one: `` left: 2 ``.
#[test]
fn schema_rows_are_published_once_and_never_below_a_visible_version() {
    let lab = Lab::new(&[Role::Writer]);
    let registry = Registry::at(lab.paths[0].clone());
    registry.open().unwrap();
    let newer = json!({"r": 2, "x": {"format": "att/1", "version": 2}});
    registry
        .write(vec![vec![Change {
            collection: SCHEMA.into(),
            key: "machine".into(),
            value: Some(newer.clone()),
        }]])
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
    assert!(matches!(address, SchemaState::One { version: 1, .. }));
}
