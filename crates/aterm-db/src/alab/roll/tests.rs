// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The registry roll (design §3.8, step 13): the restatement's merge rule, the covers, and the
//! roll, seal and storage release on scratch labs, a three-Mac split included. Every address is
//! synthetic (`example.test`).

use super::*;
use crate::{
    alab::registry::{ADDRESS, COLLECTIONS, MACHINE_FACTS, PROVIDER, REGISTRY},
    catalog::release::Kept,
    table::tests::{Lab, repair_commons, repair_commons_among},
    typed::{cell::Field, owned},
};
use aterm_secrets::commons::roster::Role;

fn op(n: u8) -> Digest {
    [n; 32]
}

fn plain(x: &str) -> Value {
    CellValue::owner(1, json!(x)).encode()
}

fn key() -> (String, String) {
    ("provider".to_owned(), "anthropic.site".to_owned())
}

/// The candidates `rows` writes into the register [`key`] names, as a reader expands them.
fn written(rows: &[Vec<Change>]) -> Vec<(Digest, Value)> {
    let change = rows
        .iter()
        .flatten()
        .find(|c| (c.collection.clone(), c.key.clone()) == key())
        .expect("the register is written");
    cell::carried(change.value.as_ref().unwrap()).unwrap()
}

/// The restatement's merge, register by register: a first roll carries every candidate (a
/// conflict stays one); a re-run writes nothing; a late candidate replaces one the old history
/// shows superseded, stands beside one it never saw, and stands beside an owner's edit in the
/// new generation. NEGATIVE CONTROLS: with `restate` keeping every candidate the new generation
/// holds (no `held` filter), the superseded `o1` comes back beside `o3`; with the new
/// generation's candidates dropped, the owner's edit `e` is lost.
#[test]
fn the_restatement_merges_what_each_side_saw() {
    let none = BTreeSet::new();
    // A first roll: an open conflict stays open.
    let source = Cells::from([(key(), register(&[(op(1), plain("a")), (op(2), plain("b"))]))]);
    let held = BTreeSet::from([op(1), op(2)]);
    let rows = restate(&REGISTRY, &source, &held, &Cells::new(), &none).unwrap();
    assert_eq!(
        written(&rows),
        vec![(op(1), plain("a")), (op(2), plain("b"))]
    );
    // An owned row is no register: it goes as its archived record.
    let owned = Cells::from([(
        (MACHINE_FACTS.name.to_owned(), "0".repeat(32)),
        register(&[(op(1), json!({"n": 1}))]),
    )]);
    assert!(
        restate(&REGISTRY, &owned, &held, &Cells::new(), &none)
            .unwrap()
            .is_empty()
    );

    // A re-run once both are carried writes nothing.
    let target = Cells::from([(
        key(),
        register(&[(op(9), rows[0][0].value.clone().unwrap())]),
    )]);
    let carried = carried_origins([&Command::Write {
        request: [0; 16],
        changes: rows[0].clone(),
    }]);
    assert!(
        restate(&REGISTRY, &source, &held, &target, &carried)
            .unwrap()
            .is_empty()
    );

    // A late value whose history holds the carried one: it replaced it.
    let one = Cells::from([(key(), register(&[(op(1), plain("a"))]))]);
    let first = restate(
        &REGISTRY,
        &one,
        &BTreeSet::from([op(1)]),
        &Cells::new(),
        &none,
    )
    .unwrap();
    let target = Cells::from([(
        key(),
        register(&[(op(9), first[0][0].value.clone().unwrap())]),
    )]);
    let carried = BTreeSet::from([op(1)]);
    let late = Cells::from([(key(), register(&[(op(3), plain("c"))]))]);
    let superseded = restate(
        &REGISTRY,
        &late,
        &BTreeSet::from([op(1), op(3)]),
        &target,
        &carried,
    )
    .unwrap();
    assert_eq!(written(&superseded), vec![(op(3), plain("c"))]);

    // A late value from a Mac that never saw the carried one: both stand.
    let concurrent = restate(
        &REGISTRY,
        &late,
        &BTreeSet::from([op(3)]),
        &target,
        &carried,
    )
    .unwrap();
    assert_eq!(
        written(&concurrent),
        vec![(op(1), plain("a")), (op(3), plain("c"))]
    );

    // An owner's edit in the new generation, which never saw the late value: both stand.
    let edited = Cells::from([(key(), register(&[(op(8), plain("e"))]))]);
    let beside = restate(
        &REGISTRY,
        &late,
        &BTreeSet::from([op(1), op(3)]),
        &edited,
        &carried,
    )
    .unwrap();
    assert_eq!(
        written(&beside),
        vec![(op(3), plain("c")), (op(8), plain("e"))]
    );
}

fn held(actor: u8, sequence: u64, unread: bool) -> Held {
    Held {
        actor: [actor; 48],
        sequence,
        unread,
    }
}

/// A cover carries each writer up to its first record this build cannot read; covers merge by
/// the highest; a Mac is covered only where every writer it holds is. NEGATIVE CONTROL: with
/// `frontier` ignoring unread records, writer 2's cover reaches 5 and the held 4..5 read as
/// carried.
#[test]
fn a_cover_stops_at_the_first_record_this_build_cannot_read() {
    let ops = [
        held(1, 1, false),
        held(1, 2, false),
        held(2, 1, false),
        held(2, 2, false),
        held(2, 4, true),
        held(2, 5, false),
    ];
    let a = keys::actor_key(&[1; 48]);
    let b = keys::actor_key(&[2; 48]);
    assert_eq!(
        frontier(&ops),
        BTreeMap::from([(a.clone(), 2), (b.clone(), 3)])
    );
    let holding = holdings(&ops);
    assert_eq!(holding, BTreeMap::from([(a.clone(), 2), (b.clone(), 5)]));
    assert!(!within(&holding, &frontier(&ops)));
    assert!(within(
        &holding,
        &BTreeMap::from([(a.clone(), 2), (b.clone(), 9)])
    ));
    assert!(!within(&holding, &BTreeMap::from([(b, 9)])));
}

/// An owner's value for `col` of provider `pk`.
fn provider(pk: &str, col: &str, x: Value) -> Change {
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

/// An address row born for a synthetic address.
fn address(addr: &str) -> Vec<Change> {
    let pk = keys::address_pk(addr).unwrap();
    ["addr", "row"]
        .into_iter()
        .zip([json!(addr), json!("live")])
        .map(|(col, x)| {
            cell::change(
                &ADDRESS,
                CellKey {
                    pk: &pk,
                    col,
                    elem: None,
                },
                &CellValue::owner(1, x),
            )
            .unwrap()
        })
        .collect()
}

/// Mac `n`'s own facts row.
fn facts(lab: &Lab, n: usize, label: &str) -> Vec<Change> {
    vec![Change {
        collection: MACHINE_FACTS.name.into(),
        key: keys::uuid32(&crate::alab::tests::machine_of(lab, n)),
        value: Some(json!({"r": 1, "n": 1, "mac": "mc-0000000000000000", "label": label})),
    }]
}

/// Every cells collection's projection and every owned collection's rows of a view.
fn image(view: &View) -> (Vec<Projection>, Vec<owned::OwnedRead>) {
    (
        COLLECTIONS
            .iter()
            .filter(|c| c.kind == typed::Kind::Cells)
            .map(|c| view.project(c))
            .collect(),
        COLLECTIONS
            .iter()
            .filter(|c| c.kind == typed::Kind::Owned)
            .map(|c| view.owned.get(c.name).cloned().unwrap_or_default())
            .collect(),
    )
}

/// One release pass on Mac `n`, as its claims write runs it.
fn release(lab: &Lab, n: usize) -> Released {
    Registry::at(lab.paths[n].clone())
        .release(now_ms())
        .unwrap()
}

/// Whether Mac `n` stores the registry's first generation.
fn stores_first(lab: &Lab, n: usize) -> bool {
    let trust = lab.bootstrap(n).trust();
    Directory::new(&lab.paths[n], trust)
        .path(att::REGISTRY.descriptor(trust).unwrap().scope)
        .unwrap()
        .exists()
}

fn now_ms() -> u64 {
    aterm_time::unix_millis().unwrap()
}

fn first_id(lab: &Lab) -> Digest {
    att::REGISTRY
        .descriptor(lab.bootstrap(0).trust())
        .unwrap()
        .scope
        .digest()
}

/// A roll restates every register (an open conflict included) and carries every owned row as
/// its signed record, so the second generation reads exactly what the first did, signer,
/// operation and rank included, with every quota whole; it seals the first, readers move, and
/// each Mac's storage of the first goes once the cover carries what it holds. A plan writes
/// nothing. The seal waits until the other Writer has declared the second generation, which its
/// release pass does. NEGATIVE CONTROLS: with the seal not waiting on `waiting`, the first
/// apply seals while b has shown no build that reads the second generation and the `staged`
/// assertion fails; with `archived_ops` returning nothing, the owned rows are
/// missing from the second generation and the image comparison fails; with the seal skipped,
/// `generation()` stays 1 and the reader assertion fails; with `covered` always false, the
/// first generation's storage stays on the roller.
#[test]
fn a_roll_restates_everything_and_releases_the_sealed_generation() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let a = Registry::at(lab.paths[0].clone());
    a.open().unwrap();
    a.seed().unwrap();
    a.write(vec![address("ann@example.test"), facts(&lab, 0, "m1")])
        .unwrap();
    repair_commons(&lab, 6);
    let b = Registry::at(lab.paths[1].clone());
    // Two edits of one cell that never saw each other: a conflict.
    a.write(vec![vec![provider(
        "anthropic",
        "site",
        json!("https://a.example.test"),
    )]])
    .unwrap();
    b.write(vec![
        vec![provider(
            "anthropic",
            "site",
            json!("https://b.example.test"),
        )],
        facts(&lab, 1, "m2"),
    ])
    .unwrap();
    repair_commons(&lab, 6);
    let before = a.view().unwrap();
    assert!(before.project(&PROVIDER).rows["anthropic"].fields["site"].conflicted());
    assert_eq!(before.owned[MACHINE_FACTS.name].rows.len(), 2);

    let plan = a.roll(false, now_ms()).unwrap();
    assert_eq!((plan.from, plan.to, plan.records), (1, 2, 0));
    assert!(
        plan.registers > 10 && plan.archived == 2 && plan.cover,
        "{plan:?}"
    );
    assert_eq!(a.generation().generation, 1);
    let b_machine = lab.machine(1);
    assert_eq!(plan.waiting, vec![b_machine]);

    // b has shown no build that reads the second generation: the roll restates but the seal
    // waits, so b still reads and writes the first.
    let staged = a.roll(true, now_ms()).unwrap();
    assert!(!staged.sealed && staged.records > 0, "{staged:?}");
    assert_eq!(staged.waiting, vec![b_machine]);
    assert!(staged.released.released.is_empty());
    assert_eq!(a.generation().generation, 1);
    repair_commons(&lab, 6);
    assert_eq!(b.generation().generation, 1);
    // b's release pass (a claims write's housekeeping) declares the second generation.
    assert!(release(&lab, 1).released.is_empty());
    repair_commons(&lab, 6);

    let rolled = a.roll(true, now_ms()).unwrap();
    assert!(rolled.sealed && rolled.waiting.is_empty(), "{rolled:?}");
    assert_eq!(rolled.released.released, vec![first_id(&lab)]);
    assert!(!stores_first(&lab, 0));
    assert_eq!(a.generation().generation, 2);
    let after = a.view().unwrap();
    assert!(after.complete);
    assert_eq!(image(&after), image(&before));
    let site = &after.project(&PROVIDER).rows["anthropic"].fields["site"];
    assert!(matches!(site, Field::Conflict { candidates, .. } if candidates.len() == 2));
    assert!(after.quota.spent.is_empty(), "{:?}", after.quota.spent);

    // The last generation compiled has nothing after it.
    assert!(a.roll(true, now_ms()).is_err());

    // The other Mac reads the first generation until it hears the seal, then the second.
    assert_eq!(b.generation().generation, 1);
    assert!(stores_first(&lab, 1));
    repair_commons(&lab, 6);
    assert_eq!(b.generation().generation, 2);
    assert_eq!(image(&b.view().unwrap()), image(&before));
    assert_eq!(release(&lab, 1).released, vec![first_id(&lab)]);
    assert!(!stores_first(&lab, 1));
}

/// The three-Mac split (design §11 step 13): m1 rolls alone while m2 and m3, which have not
/// heard the seal, edit the first generation; m1's owner edits the same cell in the second.
/// After the heal neither m2 nor m3 releases the first generation, since m1's cover does not
/// carry m2's late record; m3's roll carries it, merged beside m1's edit as a conflict; then
/// every Mac releases it and all three read alike. NEGATIVE CONTROLS: with `within` always
/// true, m2 releases right after the heal and its `NotCarried` assertion fails; with `restate`
/// writing the late candidates alone, m1's edit is lost and the conflict assertion fails.
#[test]
fn a_late_edit_is_carried_before_its_mac_releases() {
    let lab = Lab::new(&[Role::Writer, Role::Writer, Role::Writer]);
    let macs: Vec<Registry> = lab.paths.iter().cloned().map(Registry::at).collect();
    macs[0].open().unwrap();
    macs[0].seed().unwrap();
    macs[0].write(vec![facts(&lab, 0, "m1")]).unwrap();
    repair_commons(&lab, 6);
    macs[2].write(vec![facts(&lab, 2, "m3")]).unwrap();
    repair_commons(&lab, 6);
    let now = [aterm_time::unix_secs().unwrap(); 3];

    // m1 restates; m2 and m3 hear the second generation and declare it on their release pass,
    // as every Mac running this build does on a claims write.
    assert_eq!(macs[0].roll(true, now_ms()).unwrap().waiting.len(), 2);
    repair_commons(&lab, 6);
    release(&lab, 1);
    release(&lab, 2);
    repair_commons(&lab, 6);

    // The cut: m1 alone seals, then edits the second generation.
    let rolled = macs[0].roll(true, now_ms()).unwrap();
    assert!(rolled.sealed, "{rolled:?}");
    macs[0]
        .write(vec![vec![provider(
            "anthropic",
            "site",
            json!("https://one.example.test"),
        )]])
        .unwrap();
    // m2 and m3 still read the first generation; m2 edits it.
    assert_eq!(macs[1].generation().generation, 1);
    macs[1]
        .write(vec![vec![provider(
            "anthropic",
            "site",
            json!("https://two.example.test"),
        )]])
        .unwrap();
    repair_commons_among(&lab, 6, &[1, 2], &now);

    // The heal.
    repair_commons(&lab, 6);
    for (n, registry) in macs.iter().enumerate() {
        assert_eq!(registry.generation().generation, 2, "m{}", n + 1);
    }
    let first = first_id(&lab);
    for n in [1, 2] {
        assert_eq!(
            release(&lab, n).kept,
            vec![(first, Kept::NotCarried)],
            "m{}",
            n + 1
        );
        assert!(stores_first(&lab, n));
    }

    // m3, which holds m2's late record, rolls it in.
    let late = macs[2].roll(true, now_ms()).unwrap();
    assert_eq!((late.from, late.to, late.sealed), (1, 2, false));
    assert!(late.registers >= 1 && late.cover, "{late:?}");
    assert_eq!(late.released.released, vec![first]);
    repair_commons(&lab, 6);
    assert_eq!(release(&lab, 1).released, vec![first]);
    let views: Vec<View> = macs.iter().map(|r| r.view().unwrap()).collect();
    let site = &views[0].project(&PROVIDER).rows["anthropic"].fields["site"];
    let mut shown: Vec<&Value> = match site {
        Field::Conflict { candidates, .. } => candidates.iter().map(|c| &c.value.x).collect(),
        other => panic!("not a conflict: {other:?}"),
    };
    shown.sort_by_key(|v| v.as_str().map(str::to_owned));
    assert_eq!(
        shown,
        [
            &json!("https://one.example.test"),
            &json!("https://two.example.test")
        ]
    );
    assert_eq!(views[0].owned[MACHINE_FACTS.name].rows.len(), 2);
    for view in &views[1..] {
        assert_eq!(image(view), image(&views[0]));
    }
    for n in 0..3 {
        assert!(!stores_first(&lab, n), "m{}", n + 1);
    }
}

/// A cover counts only as the Mac its key names wrote it: after a split leaves m2 holding a late
/// first-generation record no restatement carried, m1 writing a cover row keyed for m2 that
/// claims all of m2's records does not let m2 release, and the grid shows cover columns
/// read-only. NEGATIVE CONTROL: with `covers` taking every row whatever its signer, m2's pass
/// releases the first generation and its `NotCarried` assertion fails.
#[test]
fn a_cover_written_for_another_mac_counts_for_nothing() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let (a, b) = (
        Registry::at(lab.paths[0].clone()),
        Registry::at(lab.paths[1].clone()),
    );
    a.open().unwrap();
    a.seed().unwrap();
    repair_commons(&lab, 6);
    a.roll(true, now_ms()).unwrap();
    repair_commons(&lab, 6);
    release(&lab, 1);
    repair_commons(&lab, 6);
    assert!(a.roll(true, now_ms()).unwrap().sealed);
    // b edits the first generation before it hears the seal.
    b.write(vec![vec![provider(
        "anthropic",
        "site",
        json!("https://late.example.test"),
    )]])
    .unwrap();
    repair_commons(&lab, 6);
    let first = first_id(&lab);
    assert_eq!(release(&lab, 1).kept, vec![(first, Kept::NotCarried)]);

    let actor = |n: usize| {
        let bootstrap = lab.bootstrap(n);
        keys::actor_key(&Session::open(&bootstrap).unwrap().actor())
    };
    let pk = keys::roll_pk(1, &keys::uuid32(&lab.machine(1)));
    let forged = |col: &str, x: Value| {
        cell::change(
            &ROLL,
            CellKey {
                pk: &pk,
                col,
                elem: None,
            },
            &CellValue::owner(ROLL.version, x),
        )
        .unwrap()
    };
    a.write(vec![vec![
        forged("from", json!(1)),
        forged(
            "covered",
            json!([{"a": actor(0), "s": 999}, {"a": actor(1), "s": 999}]),
        ),
        forged("ops", json!(1)),
        forged("at", json!(now_ms())),
        forged("row", json!("live")),
    ]])
    .unwrap();
    repair_commons(&lab, 6);
    assert_eq!(release(&lab, 1).kept, vec![(first, Kept::NotCarried)]);
    assert!(stores_first(&lab, 1));
    assert!(
        ROLL.columns
            .iter()
            .filter(|c| c.name != "row")
            .all(typed::grid::read_only)
    );
}

/// Mac `n`'s own facts row at counter `counter`.
fn facts_at(lab: &Lab, n: usize, label: &str, counter: u64) -> Vec<Change> {
    let mut row = facts(lab, n, label);
    row[0].value =
        Some(json!({"r": 1, "n": counter, "mac": "mc-0000000000000000", "label": label}));
    row
}

/// The signed record `id` as Mac `n` stores it in the registry's first generation.
fn first_wire(lab: &Lab, n: usize, id: &Digest) -> Vec<u8> {
    let bootstrap = lab.bootstrap(n);
    let session = Session::open(&bootstrap).unwrap();
    let directory = Directory::new(&lab.paths[n], bootstrap.trust());
    let scope = att::REGISTRY.descriptor(bootstrap.trust()).unwrap().scope;
    let store = directory
        .observe_store_if_exists(&session, scope)
        .unwrap()
        .unwrap();
    store.wire(&session, id).unwrap().unwrap().to_vec()
}

/// An archived cell of the row for `operation`, as any Writer may write one.
fn archived_cell(operation: &Digest, col: &str, x: Value) -> Change {
    cell::change(
        &ARCHIVED,
        CellKey {
            pk: &keys::archived_pk(operation),
            col,
            elem: None,
        },
        &CellValue::owner(1, x),
    )
    .unwrap()
}

/// No later write over an archived row takes a carried record away or stands for another: after
/// a roll carries a Mac's latest facts record, a Writer garbling that archived cell and archiving
/// the Mac's older genuine record beside it leaves the latest row as it was, signer and operation
/// included; a record that does not authenticate is no row. NEGATIVE CONTROL: with
/// `archived_ops` honouring only each archived row's latest write, as the cells' current values
/// do, the garbled cell hides the latest record and the older one shows, so the operation
/// assertion fails.
#[test]
fn a_later_write_over_an_archived_row_neither_hides_nor_replaces_it() {
    let lab = Lab::new(&[Role::Writer]);
    let a = Registry::at(lab.paths[0].clone());
    a.open().unwrap();
    let key = keys::uuid32(&crate::alab::tests::machine_of(&lab, 0));
    a.write(vec![facts_at(&lab, 0, "old", 1)]).unwrap();
    let older = a.view().unwrap().owned[MACHINE_FACTS.name].rows[&key].operation;
    a.write(vec![facts_at(&lab, 0, "new", 2)]).unwrap();
    let latest = a.view().unwrap().owned[MACHINE_FACTS.name].rows[&key].operation;
    assert_ne!(older, latest);
    let older_wire = first_wire(&lab, 0, &older);
    let latest_wire = first_wire(&lab, 0, &latest);

    a.roll(true, now_ms()).unwrap();
    assert_eq!(a.generation().generation, 2);
    let row = |view: &View| view.owned[MACHINE_FACTS.name].rows.get(&key).cloned();
    let carried = row(&a.view().unwrap()).expect("the roll carried the facts row");
    assert_eq!(carried.operation, latest);

    let mut garbled = latest_wire.clone();
    let last = garbled.len() - 1;
    garbled[last] ^= 1;
    a.write(vec![
        vec![
            archived_cell(&older, "generation", json!(1)),
            archived_cell(
                &older,
                "wire",
                json!(aterm_secrets::b64::encode_padded(&older_wire)),
            ),
        ],
        vec![archived_cell(
            &latest,
            "wire",
            json!(aterm_secrets::b64::encode_padded(&garbled)),
        )],
    ])
    .unwrap();
    let view = a.view().unwrap();
    let shown = row(&view).expect("a garbled cell does not take the row away");
    assert_eq!(shown.operation, latest);
    assert_eq!(shown.value, carried.value);
    assert_eq!(view.owned[MACHINE_FACTS.name].rows.len(), 1);

    // A record that does not authenticate is no row: the garbled wire under a key of its own.
    let stray = [7; 32];
    a.write(vec![vec![
        archived_cell(&stray, "generation", json!(1)),
        archived_cell(
            &stray,
            "wire",
            json!(aterm_secrets::b64::encode_padded(&garbled)),
        ),
    ]])
    .unwrap();
    let view = a.view().unwrap();
    assert_eq!(view.owned[MACHINE_FACTS.name].rows.len(), 1);
    assert_eq!(row(&view).unwrap().operation, latest);
}

/// `aterm db alab roll`: a dry run names both generations and writes nothing; `--apply` rolls,
/// seals and releases, reporting each by table digest. NEGATIVE CONTROL: with `--dry-run`
/// parsed as `--apply`, the first report says `applied` and the generation moves before the
/// second run.
#[test]
fn the_roll_verb_plans_then_applies() {
    let lab = Lab::new(&[Role::Writer]);
    let a = Registry::at(lab.paths[0].clone());
    a.open().unwrap();
    a.write(vec![facts(&lab, 0, "m1")]).unwrap();
    let run = |words: &[&str]| {
        let words: Vec<String> = words.iter().map(|w| (*w).to_owned()).collect();
        crate::alab::command::run(
            &lab.paths[0],
            &words,
            None,
            &mut |_| panic!("a roll opens no review"),
            now_ms(),
        )
    };
    let plan = run(&["roll", "--dry-run"]).unwrap();
    assert_eq!(plan["applied"], json!(false));
    assert_eq!(plan["from"], json!("alab/registry"));
    assert_eq!(plan["to"], json!("alab/registry.2"));
    assert_eq!(plan["archived"], json!(1));
    assert_eq!(a.generation().generation, 1);
    assert!(run(&["roll", "--apply", "--dry-run"]).is_err());
    let done = run(&["roll", "--apply"]).unwrap();
    assert_eq!(done["sealed"], json!(true));
    assert_eq!(
        done["released"],
        json!([aterm_codec::hex::encode_lower(&first_id(&lab))])
    );
    assert_eq!(a.generation().generation, 2);
}
