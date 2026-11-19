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
/// NEGATIVE CONTROL: with a `login` column typed `email` added to `MAILBOX`, the address
/// assertion fails: `` left: ["address.addr", "mailbox.login"] ``.
#[test]
fn registry_schemas_keep_secrets_and_addresses_out() {
    let names: Vec<&str> = COLLECTIONS.iter().map(|c| c.name).collect();
    let mut addresses = Vec::new();
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
            if column.ty.holds_address() {
                addresses.push(format!("{}.{}", collection.name, column.name));
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
                changes: row,
                said: None
            }
            .encode()
            .unwrap()
            .len()
                <= crate::crypto::MAX_BODY
        );
    }
    assert_eq!(addresses, ["address.addr"]);
    // A mailbox's key derives from its login's address key (a member-readable row); a phone's
    // stays random, since its number is a low-entropy secret.
    assert_eq!(
        (MAILBOX.derive, PHONE.derive),
        (Derive::Composite, Derive::Random)
    );
    // A mailbox's patterns name address rows; its domains are a text set from the fourth
    // generation, each domain an element keyed by its digest, and one list before it.
    assert!(matches!(
        MAILBOX.column("receives").map(|c| &c.ty),
        Some(Ty::Set(inner)) if matches!(**inner, Ty::Ref("address"))
    ));
    assert_eq!(
        MAILBOX
            .column("receives_domains")
            .and_then(|c| c.ty.text_set()),
        Some(&Ty::Text(253))
    );
    for older in [&registry::MAILBOX_3, &registry::SEALED_MAILBOX] {
        assert!(matches!(
            older.column("receives_domains").map(|c| &c.ty),
            Some(Ty::List(..))
        ));
    }
    assert!(matches!(
        MAILBOX.column("login").map(|c| &c.ty),
        Some(Ty::Ref("address"))
    ));
    for (collection, column) in [
        (&ADDRESS, "password"),
        (&MAILBOX, "mailbox_password"),
        (&MAILBOX, "totp"),
    ] {
        assert!(
            matches!(
                collection.column(column).map(|c| &c.ty),
                Some(Ty::SecretRef)
            ),
            "{}.{column}",
            collection.name
        );
    }
    assert!(matches!(
        PHONE.column("number").map(|c| &c.ty),
        Some(Ty::Phone)
    ));
}

/// LT-04's exit test. Every secret column of the registry's third generation names an
/// `aterm secrets` entry and the value lives only in the store: a password goes into the store
/// (`secret::store_cell`, under a fresh tagged entry of its cell), the table holds
/// `{vault, entry}`, and the value reaches no cell, no history entry, no export and no grid
/// save. An address and a phone number are ordinary data, shown as they are. The generations
/// builds up to 0.199.0 read keep sealed tokens and hold no reference.
/// NEGATIVE CONTROLS: the value itself is no `SecretRef` (`cell::change` refuses it), a grid save
/// that types the value over the reference is refused as read-only, and the value is absent from
/// every table surface though the store holds it; with `grid::show` showing a reference's JSON
/// rather than its entry name, the export assertion fails.
#[test]
fn registry_secret_cells_name_store_entries_and_contacts_are_plain() {
    use crate::typed::{grid, secret};
    // The third generation's secret-named columns are references, exactly these three; the
    // generations builds up to 0.199.0 read hold sealed tokens there and no reference anywhere.
    let secret_columns = |table: &crate::typed::Table, ty: &Ty| {
        let mut found: Vec<String> = table
            .collections
            .iter()
            .flat_map(|collection| {
                collection
                    .columns
                    .iter()
                    .filter(|column| &column.ty == ty && column.name != "vault_ref")
                    .map(|column| format!("{}.{}", collection.name, column.name))
            })
            .collect();
        found.sort();
        found
    };
    let three = [
        "address.password",
        "mailbox.mailbox_password",
        "mailbox.totp",
    ];
    for table in &registry::GENERATIONS[2..] {
        assert_eq!(
            secret_columns(table, &Ty::SecretRef),
            three,
            "{}",
            table.role
        );
    }
    for table in &registry::GENERATIONS[..2] {
        assert!(
            secret_columns(table, &Ty::SecretRef).is_empty(),
            "{}",
            table.role
        );
        let sealed = secret_columns(table, &Ty::Secret);
        assert!(
            three.iter().all(|c| sealed.contains(&(*c).to_owned())),
            "{sealed:?}"
        );
    }

    let lab = Lab::new(&[Role::Writer]);
    newest(&lab.paths[0], 1_800_000_000_000);
    let registry = Registry::at(lab.paths[0].clone());
    assert_eq!(registry.generation().generation, 4);
    let store = secret::Memory::default();
    let address = "lt04.fixture@example.invalid";
    let number = "2025550149";
    let value = "Synthetic-password-LT04-not-for-tables";
    let ad = keys::address_pk(address).unwrap();
    let ph = keys::random_pk(keys::Random::Phone).unwrap();
    let put = |collection: &crate::typed::Collection, pk: &str, col: &str, x: Value| {
        cell::change(
            collection,
            CellKey {
                pk,
                col,
                elem: None,
            },
            &CellValue::owner(collection.version, x),
        )
        .unwrap()
    };
    let reference = secret::store_cell(
        &store,
        ADDRESS.name,
        "password",
        &ad,
        &secret::Secret::new(value.to_owned()),
    )
    .unwrap();
    let entry = secret::entry_of(&reference).unwrap().to_owned();
    assert!(
        secret::bound(ADDRESS.name, "password", &ad, &entry),
        "{entry}"
    );
    assert_eq!(store.held(&entry).as_deref(), Some(value));
    // NEGATIVE CONTROL: the value itself can never be a password cell.
    assert!(
        cell::change(
            &ADDRESS,
            CellKey {
                pk: &ad,
                col: "password",
                elem: None
            },
            &CellValue::owner(ADDRESS.version, json!(value)),
        )
        .is_err()
    );
    registry
        .write(vec![
            vec![
                put(&ADDRESS, &ad, "addr", json!(address)),
                put(&ADDRESS, &ad, "domain", json!("example.invalid")),
                put(&ADDRESS, &ad, "origin", json!("owner")),
                put(&ADDRESS, &ad, "row", json!("live")),
                put(&ADDRESS, &ad, "password", reference.clone()),
            ],
            vec![
                put(&PHONE, &ph, "last4", json!("0149")),
                put(&PHONE, &ph, "number", json!(number)),
                put(&PHONE, &ph, "row", json!("live")),
            ],
        ])
        .unwrap();

    let handle = crate::att::Handle::at(lab.paths[0].clone(), &crate::att::REGISTRY);
    let view = handle.view().unwrap();
    let held = view.project(&ADDRESS).rows[&ad].clone();
    assert_eq!(held.get("password"), Some(&reference));
    let table = handle.compiled().table;
    let emails = grid::export(table, &ADDRESS, &view.cells, &[]).unwrap();
    let phones = grid::export(table, &PHONE, &view.cells, &[]).unwrap();
    // The page and the grid show the entry's name, the address and the number as they are.
    assert!(
        emails.text.contains(&entry),
        "the grid names the store entry"
    );
    assert!(
        emails.text.contains(address),
        "an address is shown as it is"
    );
    assert!(
        phones.text.contains(number),
        "a phone number is shown as it is"
    );
    let history = handle.history().unwrap();
    let history_json =
        aterm_json::to_string(&history.iter().map(|e| &e.changes).collect::<Vec<_>>()).unwrap();
    assert!(history_json.contains(&entry));
    for surface in [
        emails.text.as_str(),
        phones.text.as_str(),
        &history_json,
        &aterm_json::to_string(&emails.base.to_json()).unwrap(),
    ] {
        assert!(
            !surface.contains(value),
            "a secret value reached a table surface"
        );
        assert!(
            !surface.contains(secret::MASK),
            "nothing is masked any more"
        );
    }
    // NEGATIVE CONTROL: a grid save typing the value over the reference is refused, and writes
    // nothing.
    let typed_over = emails.text.replace(&entry, value);
    assert_ne!(typed_over, emails.text);
    match handle.save(&ADDRESS, &emails.base, &typed_over).unwrap() {
        crate::att::Saved::Problems(problems) => assert!(!problems.is_empty()),
        crate::att::Saved::Written { .. } => panic!("a value was typed into a secret cell"),
    }
    assert_eq!(handle.history().unwrap().len(), history.len());
    assert_eq!(store.every(), [value]);
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
        crate::documents::apply(
            &|later, earlier| index.observes(later, earlier),
            &mut cells,
            id,
            changes,
            false,
        );
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
    assert_eq!(
        rows.len(),
        seeds::PROVIDERS.len() + seeds::CLIS.len() + seeds::PLANS.len()
    );
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
/// as retries: `` left: Seeded { records: 0, retried: 3 } ``.
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

/// A provider the owner retired is named by no seed the pass writes: a seeded CLI or plan whose
/// `provider` is that provider is written without it, since a live row naming a retired one
/// would have the rules every write passes refuse the pass (`typed::check::names_not_live`), and
/// every later seed pass. The pass passes those rules, and once the provider is live again the
/// next pass fills the cells it left out.
/// NEGATIVE CONTROL: with `names_ended_provider` always false, the Kimi Code CLI's seed names the
/// retired provider and the rules refuse it: `` assertion `left == right` failed: the seeds pass
/// the rules every write passes left: Some(Refused { collection: "cli", pk: "kimi", column:
/// "provider", rule: "names provider row kimi, which is not live; point it at a live row, leave
/// it empty, or make that row live first" }) right: None ``.
#[test]
fn a_seed_names_no_provider_the_owner_retired() {
    let retired = apply(&[vec![owner_value("kimi", "row", json!("retired"))]]);
    let rows = seeds::rows_for(&retired).unwrap();
    let names_kimi = |rows: &[Vec<Change>]| {
        rows.iter().flatten().any(|change| {
            change.key.ends_with(".provider")
                && change.value.as_ref().unwrap()["x"] == json!(keys::KIMI)
        })
    };
    let mut view = crate::typed::View::empty_for_test();
    view.cells = retired.clone();
    assert_eq!(
        crate::typed::check::refused(&registry::REGISTRY, &rows, &view, seeds::compiled),
        None,
        "the seeds pass the rules every write passes"
    );
    assert!(!names_kimi(&rows), "{rows:?}");
    let live = apply(&[
        vec![owner_value("kimi", "row", json!("retired"))],
        rows.concat(),
        vec![owner_value("kimi", "row", json!("live"))],
    ]);
    assert!(names_kimi(&seeds::rows_for(&live).unwrap()));
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
            said: None,
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

/// One account cell of `pk`, as `value` says who wrote it.
fn account_cell(pk: &str, col: &str, value: &CellValue) -> Change {
    cell::change(
        &ACCOUNT,
        CellKey {
            pk,
            col,
            elem: None,
        },
        value,
    )
    .unwrap()
}

/// SF-07's exit (docs/DESIGN-shared-files-2026-11-11.md §5, §10). One address is three
/// accounts: Claude, Codex and Google accounts born over it as `add account` births them (an
/// owner record each, at account version 2) have three keys, each the canonical key of its realm
/// and the address, and three slugs, `claude-fb-alpha`, `oai-fb-alpha` and `google-fb-alpha`,
/// with the provider word and the address as their names. The slug, the key and the provider
/// word with the address in either spelling name one account; the address alone names all three;
/// a slug's prefix or tail names none.
///
/// An automatic write to an account is inert, one record for each rule, and leaves the account
/// as its owner wrote it:
/// - rule 2: an automatic `browser`, an Owner column;
/// - rule 3a: an automatic `status=active` after the birth, a Birth column: it stays `pending`;
/// - rule 3b: the second Mac's observer births the Claude account (`status=active`) before it
///   hears of the owner's birth: after the exchange `status` reads `pending`, no conflict;
/// - rule 4: an owner's `realm=codex` on the Claude row, a fixed column after its birth: the
///   realm stays `claude`;
/// - the verb rule: an automatic `r=1` label `claude-<6 hex>`, as an observer before version 2
///   stored it, and an automatic `r=2` label `stolen`, on the Verb column `label`.
///
/// `view.inert` counts those six records for `account`, all on the Claude row, and its slug is
/// still `claude-fb-alpha`. The owner's label `m2-work`, a machine's word in it, makes it
/// `claude-m2-work`, and `claude-fb-alpha` then names no account.
/// NEGATIVE CONTROL: with `authority::read` projecting every record as written (its opt-in test
/// made `true`), the owner's realm write and the automatic label both stand: `` left:
/// "oai-stolen" right: "claude-fb-alpha" ``. Each rule's own control is in
/// `typed/authority_tests.rs`.
#[test]
fn one_address_is_three_accounts_slugged_claude_oai_and_google_and_an_automatic_owner_write_is_inert()
 {
    use super::account_names;
    use aterm_file::account::{Found, named};
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let first = Registry::at(lab.paths[0].clone());
    let scope = first.open().unwrap();
    let second = Registry::at(lab.paths[1].clone());
    let ada = "ada.fb@alpha.example.invalid";
    let address = keys::address_pk(ada).unwrap();
    let owner = |x: Value| CellValue::owner(ACCOUNT.version, x);
    let automatic = |x: Value| CellValue::automatic(ACCOUNT.version, x);
    let mut born = vec![vec![
        cell::change(
            &ADDRESS,
            CellKey {
                pk: &address,
                col: "addr",
                elem: None,
            },
            &CellValue::owner(ADDRESS.version, json!(ada)),
        )
        .unwrap(),
        cell::change(
            &ADDRESS,
            CellKey {
                pk: &address,
                col: "row",
                elem: None,
            },
            &CellValue::owner(ADDRESS.version, json!("live")),
        )
        .unwrap(),
    ]];
    let mut accounts = Vec::new();
    for realm in ["claude", "codex", "google"] {
        let pk = keys::account_pk(realm, &keys::claims_email(None, Some(ada)).unwrap()).unwrap();
        let provider = keys::provider_of_realm(realm).unwrap();
        born.push(vec![
            account_cell(&pk, "provider", &owner(json!(provider))),
            account_cell(&pk, "realm", &owner(json!(realm))),
            account_cell(&pk, "login", &owner(json!(address))),
            account_cell(&pk, "status", &owner(json!("pending"))),
            account_cell(&pk, "row", &owner(json!("live"))),
        ]);
        accounts.push(pk);
    }
    let [claude, codex, google] = [&accounts[0], &accounts[1], &accounts[2]];
    first.write(born).unwrap();
    // The second Mac's observer births the Claude account before it hears of the owner's birth.
    second
        .write(vec![vec![
            account_cell(claude, "provider", &automatic(json!(keys::ANTHROPIC))),
            account_cell(claude, "realm", &automatic(json!("claude"))),
            account_cell(claude, "login", &automatic(json!(address))),
            account_cell(claude, "status", &automatic(json!("active"))),
            account_cell(claude, "row", &automatic(json!("live"))),
        ]])
        .unwrap();
    receive(&lab, 0, scope, &wires(&lab, 1, scope));
    let old_label = format!("claude-{}", &claude[3..9]);
    first
        .write(vec![
            vec![account_cell(
                claude,
                "browser",
                &automatic(json!("default")),
            )],
            vec![account_cell(claude, "status", &automatic(json!("active")))],
            vec![account_cell(claude, "realm", &owner(json!("codex")))],
            vec![account_cell(
                claude,
                "label",
                &CellValue::automatic(1, json!(old_label)),
            )],
            vec![account_cell(claude, "label", &automatic(json!("stolen")))],
        ])
        .unwrap();

    let view = first.view().unwrap();
    assert!(view.complete);
    let names = account_names::of(&view);
    assert_eq!(names[claude].slug, "claude-fb-alpha");
    let shown: Vec<(&str, Option<&str>)> = [claude, codex, google]
        .iter()
        .map(|pk| (names[*pk].slug.as_str(), names[*pk].name.as_deref()))
        .collect();
    assert_eq!(
        shown,
        [
            ("claude-fb-alpha", Some(&*format!("claude {ada}"))),
            ("oai-fb-alpha", Some(&*format!("oai {ada}"))),
            ("google-fb-alpha", Some(&*format!("google {ada}"))),
        ]
    );
    for word in ["claude-fb-alpha", claude.as_str()] {
        assert_eq!(named(&names, word), Found::One(claude.as_str()), "{word}");
    }
    for word in [format!("oai {ada}"), format!("oai:{ada}")] {
        assert_eq!(named(&names, &word), Found::One(codex.as_str()), "{word}");
    }
    let Found::Many(mut all) = named(&names, ada) else {
        panic!("the address names every account it signs in");
    };
    all.sort_unstable();
    let mut keys_of = vec![claude.as_str(), codex.as_str(), google.as_str()];
    keys_of.sort_unstable();
    assert_eq!(all, keys_of);
    for word in ["claude-fb", "fb-alpha"] {
        assert_eq!(named(&names, word), Found::None, "{word}");
    }

    let rows = view.project(&ACCOUNT);
    let row = &rows.rows[claude];
    assert_eq!(row.get("browser"), None, "rule 2");
    assert_eq!(
        row.get("status"),
        Some(&json!("pending")),
        "rules 3a and 3b"
    );
    assert!(!row.fields["status"].conflicted(), "rule 3b");
    assert_eq!(row.get("realm"), Some(&json!("claude")), "rule 4");
    assert_eq!(row.get("label"), None, "the verb rule");
    assert_eq!(view.inert.records.get(ACCOUNT.name), Some(&6));
    assert_eq!(
        view.inert.rows(ACCOUNT.name),
        std::collections::BTreeSet::from([claude.as_str()])
    );

    first
        .write(vec![vec![account_cell(
            claude,
            "label",
            &owner(json!("m2-work")),
        )]])
        .unwrap();
    let names = account_names::of(&first.view().unwrap());
    assert_eq!(names[claude].slug, "claude-m2-work");
    assert_eq!(named(&names, "claude-m2-work"), Found::One(claude.as_str()));
    assert_eq!(named(&names, "claude-fb-alpha"), Found::None);
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
    let future_version = registry::MACHINE.version + 1;
    let newer = json!({"r": future_version, "x": {"format": "att/1", "version": future_version}});
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
            version: future_version,
            row: newer
        }
    );
    assert_eq!(
        view.schema("account"),
        SchemaState::One {
            version: ACCOUNT.version,
            row: ACCOUNT.schema_row()
        }
    );
    // The first generation's `address`, as builds up to 0.199.0 read it: a sealed password.
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
/// way round the columns come, and refuses without both; the registry names no machine slot and
/// no account class.
/// NEGATIVE CONTROL: with the `preference` arm removed from `registry::birth`, the grid refuses
/// the row: `` called `Result::unwrap()` on an `Err` value: Error("preference: the grid adds no
/// rows here; a lab verb or the importer keys them") ``.
#[test]
fn a_preference_is_born_from_its_pair() {
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

/// Every surface shows a secret column's value only as its own entry's name: a plain value some
/// writer left, and a reference to an entry not the cell's own (which may be named after a
/// credential), show as the mask in the grid's cell, the pane and history ([`grid::show_at`]).
/// NEGATIVE CONTROL: the cell's own reference shows as its entry name, not the mask.
#[test]
fn a_secret_column_shows_only_its_own_entrys_name() {
    use crate::typed::{grid, secret};
    let ad = keys::address_pk("lt04.show@example.invalid").unwrap();
    let column = ADDRESS.column("password").unwrap();
    let own = secret::entry_name(ADDRESS.name, "password", &ad).unwrap();
    assert_eq!(
        grid::show_at(ADDRESS.name, column, &ad, &secret::reference("v", &own)),
        own
    );
    for stray in [
        json!("Plain-password-77"),
        secret::reference("v", "gh_access_token"),
        secret::reference(
            "v",
            concat!("alab-address-password-", "AKIA", "IOSFODNN7EXAMPLE"),
        ),
    ] {
        assert_eq!(
            grid::show_at(ADDRESS.name, column, &ad, &stray),
            secret::MASK
        );
    }
    assert_eq!(
        grid::show(&column.ty, &json!("Plain-password-77")),
        secret::MASK
    );
}

/// A `vault_ref` points at an entry it does not own, so it never names a secret cell's entry: a
/// name of that shape, which the credential guard waives for a cell's own bound entry, is
/// refused there when written and shown as the mask, so no key-shaped run reaches a table.
/// NEGATIVE CONTROL: a `vault_ref` naming an ordinary vault entry is written and shown as is.
#[test]
fn a_vault_ref_never_names_a_secret_cells_entry() {
    use crate::typed::{grid, secret};
    let mailbox = &registry::MAILBOX;
    let mb = keys::mailbox_pk(&keys::address_pk("lt04.vault@example.invalid").unwrap()).unwrap();
    let column = mailbox.column("vault_ref").unwrap();
    let key = CellKey {
        pk: &mb,
        col: "vault_ref",
        elem: None,
    };
    let at = |entry: &str| CellValue::owner(mailbox.version, secret::reference("v", entry));
    let shaped = concat!("alab-mailbox-totp-", "JBSWY3DPEHPK", "3PXPJBSWY3DP");
    for refused in [shaped, "alab-mailbox-totp-mb-0000000000000000"] {
        assert!(
            cell::change(mailbox, key, &at(refused)).is_err(),
            "{refused}"
        );
        assert_eq!(
            grid::show_at(mailbox.name, column, &mb, &secret::reference("v", refused)),
            secret::MASK
        );
    }
    assert!(cell::change(mailbox, key, &at("ai-mailbox.toml")).is_ok());
    assert_eq!(
        grid::show_at(
            mailbox.name,
            column,
            &mb,
            &secret::reference("v", "ai-mailbox.toml")
        ),
        "ai-mailbox.toml"
    );
}

/// Start this lab's registry at the newest generation this build compiles, the one whose secret
/// cells name `aterm secrets` entries and whose phone column takes a number
/// ([`Registry::open_newest`]): earlier generations are the ones builds up to 0.200.0 read,
/// which keep sealed tokens, and no roll of this build reaches the newest one. It runs before
/// anything opens an earlier generation.
pub(crate) fn newest(paths: &aterm_secrets::client::Paths, now_ms: u64) {
    let registry = Registry::at(paths.clone());
    registry.open_newest(now_ms).unwrap();
    assert!(
        registry.generation().successor.is_none(),
        "the lab reads its newest generation"
    );
}

/// Each registry generation keeps the shape the builds reading it know ([`crate::att::conform`],
/// which every write path runs): a store reference or a plain phone number is refused in the
/// generations builds up to 0.199.0 read, whose secret columns hold sealed tokens, and the
/// refusal names no value; the third generation takes them and refuses a token.
/// NEGATIVE CONTROLS: the same reference and number pass into the third generation, and a
/// sealed token passes into the second; with `conform` skipping `Ty::Secret` columns, the
/// second generation takes the reference: `` a generation older builds read took a reference ``.
#[test]
fn a_reference_or_a_number_never_enters_a_generation_older_builds_read() {
    use crate::typed::secret::{self, Fixed, Memory, Sealer as _, Secret};
    let ad = keys::address_pk("ann@example.invalid").unwrap();
    let ph = keys::random_pk(keys::Random::Phone).unwrap();
    let entry = secret::test_entry((ADDRESS.name, "password", &ad), 1);
    let change = |collection: &crate::typed::Collection, pk: &str, col: &str, x: Value| {
        crate::database::Change {
            collection: collection.name.to_owned(),
            key: CellKey {
                pk,
                col,
                elem: None,
            }
            .encode()
            .unwrap(),
            value: Some(CellValue::owner(collection.version, x).encode()),
        }
    };
    let reference = change(
        &ADDRESS,
        &ad,
        "password",
        secret::reference(Memory::VAULT, &entry),
    );
    let number = change(&PHONE, &ph, "number", json!("2025550149"));
    let token = Fixed("alab/registry.2")
        .seal(
            ADDRESS.name,
            &ad,
            "password",
            &Secret::new("synthetic-pass-9".into()),
        )
        .unwrap();
    let sealed = change(&ADDRESS, &ad, "password", json!(token));
    for table in &registry::GENERATIONS[..2] {
        for refused in [&reference, &number] {
            let error = crate::att::conform(table, &mut [refused.clone()])
                .err()
                .unwrap_or_else(|| {
                    panic!(
                        "a generation older builds read took a reference ({})",
                        table.role
                    )
                })
                .to_string();
            assert!(
                error.contains("as the builds that read it know it"),
                "{error}"
            );
            assert!(
                !error.contains(&entry) && !error.contains("2025550149"),
                "{error}"
            );
        }
        crate::att::conform(table, &mut [sealed.clone()]).unwrap();
    }
    let mut rows = [reference.clone(), number.clone()];
    crate::att::conform(&registry::REGISTRY_3, &mut rows).unwrap();
    // The third generation stores each value as its own collection types it.
    let held = |change: &crate::database::Change| {
        CellValue::decode(change.value.as_ref().unwrap()).unwrap().x
    };
    assert_eq!(held(&rows[0]), secret::reference(Memory::VAULT, &entry));
    assert_eq!(held(&rows[1]), json!("2025550149"));
    assert!(crate::att::conform(&registry::REGISTRY_3, &mut [sealed]).is_err());
}

/// Each generation takes a Mac's hosts, its nickname aliases and a mailbox's domains only in the
/// form the builds reading it know (LT-15B, [`crate::att::conform`]): the first three take the
/// whole list and refuse an element, the fourth takes the element and refuses the list, and the
/// refusal names no member. A set every generation holds alike (a Mac's aliases) is carried as it
/// is, even an element this build's guards would refuse.
/// NEGATIVE CONTROLS (run 2026-11-15): with `conform` re-typing no `Ty::Set` column, the fourth
/// generation takes the whole list: `` registry.4 took a list ``; with it re-typing every list
/// and set column (`set_apart` always true), the stray alias is refused: `` a set every
/// generation holds alike is carried ``.
#[test]
fn each_generation_takes_a_free_text_set_only_in_its_own_form() {
    let mc = keys::machine_pk("m70");
    let mb = keys::mailbox_pk(&keys::address_pk("ann@example.invalid").unwrap()).unwrap();
    let list = |collection: &crate::typed::Collection, pk: &str, col: &str, x: Value| {
        crate::database::Change {
            collection: collection.name.to_owned(),
            key: CellKey {
                pk,
                col,
                elem: None,
            }
            .encode()
            .unwrap(),
            value: Some(CellValue::owner(1, x).encode()),
        }
    };
    let member = |collection: &crate::typed::Collection, pk: &str, col: &str, text: &str| {
        cell::member_change(collection, pk, col, text, true, |x| CellValue::owner(1, x)).unwrap()
    };
    let lists = [
        list(
            &registry::LIST_MACHINE,
            &mc,
            "hosts",
            json!(["studio.local"]),
        ),
        list(
            &registry::LIST_MACHINE,
            &mc,
            "nickname_aliases",
            json!(["Red Balloon"]),
        ),
        list(
            &registry::MAILBOX_3,
            &mb,
            "receives_domains",
            json!(["pm.example"]),
        ),
    ];
    let members = [
        member(&MACHINE, &mc, "hosts", "studio.local"),
        member(&MACHINE, &mc, "nickname_aliases", "Red Balloon"),
        member(&MAILBOX, &mb, "receives_domains", "pm.example"),
    ];
    let stray = crate::database::Change {
        collection: MACHINE.name.to_owned(),
        key: CellKey {
            pk: &mc,
            col: "aliases",
            elem: Some("not-a-label"),
        }
        .encode()
        .unwrap(),
        value: Some(CellValue::owner(1, json!(true)).encode()),
    };
    for table in registry::GENERATIONS {
        assert!(
            crate::att::conform(table, &mut [stray.clone()]).is_ok(),
            "a set every generation holds alike is carried ({})",
            table.role
        );
    }
    let (older, newest) = registry::GENERATIONS.split_at(3);
    let newest = newest[0];
    for (word, form, taken_by, refused_by) in [
        ("list", &lists, older, newest),
        ("member", &members, &[newest][..], older[2]),
    ] {
        for change in form {
            for table in taken_by {
                crate::att::conform(table, &mut [change.clone()])
                    .unwrap_or_else(|error| panic!("{} refused its own form: {error}", table.role));
            }
            let error = crate::att::conform(refused_by, &mut [change.clone()])
                .err()
                .unwrap_or_else(|| panic!("{} took a {word}", refused_by.role))
                .to_string();
            assert!(
                error.contains("as the builds that read it know it"),
                "{error}"
            );
            for text in ["studio", "Balloon", "pm.example"] {
                assert!(!error.contains(text), "{error}");
            }
        }
    }
}
