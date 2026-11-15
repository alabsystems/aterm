// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The registry roll (design §3.8, step 13): the restatement's merge rule, the covers, and the
//! roll, seal and storage release on scratch labs, a three-Mac split included. Every address is
//! synthetic (`example.test`).

use super::*;
use crate::typed::secret::Store as _;
use crate::{
    alab::registry::{
        ADDRESS, COLLECTIONS, MACHINE_FACTS, MODEL, PHONE, PROVIDER, REGISTRY_2, REGISTRY_3,
    },
    catalog::release::Kept,
    table::tests::{Lab, repair_commons, repair_commons_among_first, repair_commons_first},
    typed::{
        cell::Field,
        owned,
        secret::{self, Fixed, Memory, Secret},
    },
};
use aterm_secrets::commons::roster::Role;

/// The old generation's opener, and one for a table it does not open: one key, two table names.
const OLD: Fixed = Fixed("alab/registry");
const OTHER: Fixed = Fixed("alab/registry.2");

/// The register changes [`restate`] writes for these cells into the third generation, every
/// sealed token opened by [`OLD`], moving into a store nobody reads.
fn restated(
    source: &Cells,
    held: &BTreeSet<Digest>,
    target: &Cells,
    carried: &Carried,
) -> Result<Vec<Vec<Change>>> {
    restate(
        &REGISTRY_3,
        source,
        held,
        target,
        carried,
        (&OLD, None, &Memory::default()),
    )
    .map(|restated| restated.rows)
}

fn op(n: u8) -> Digest {
    [n; 32]
}

fn plain(x: &str) -> Value {
    CellValue::owner(1, json!(x)).encode()
}

fn key() -> (String, String) {
    ("provider".to_owned(), "anthropic.site".to_owned())
}

fn carried_at(coord: &(String, String), origins: impl IntoIterator<Item = Digest>) -> Carried {
    Carried::from([(coord.clone(), origins.into_iter().collect())])
}

/// The candidates `rows` writes into the register `key` names.
fn written_at(rows: &[Vec<Change>], key: &(String, String)) -> Vec<(Digest, Value)> {
    let change = rows
        .iter()
        .flatten()
        .find(|c| (c.collection.clone(), c.key.clone()) == *key)
        .expect("the register is written");
    cell::carried(change.value.as_ref().unwrap()).unwrap()
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
    let none = Carried::new();
    // A first roll: an open conflict stays open.
    let source = Cells::from([(key(), register(&[(op(1), plain("a")), (op(2), plain("b"))]))]);
    let held = BTreeSet::from([op(1), op(2)]);
    let rows = restated(&source, &held, &Cells::new(), &none).unwrap();
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
        restated(&owned, &held, &Cells::new(), &none)
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
        restated(&source, &held, &target, &carried)
            .unwrap()
            .is_empty()
    );

    // A late value whose history holds the carried one: it replaced it.
    let one = Cells::from([(key(), register(&[(op(1), plain("a"))]))]);
    let first = restated(&one, &BTreeSet::from([op(1)]), &Cells::new(), &none).unwrap();
    let target = Cells::from([(
        key(),
        register(&[(op(9), first[0][0].value.clone().unwrap())]),
    )]);
    let carried = carried_at(&key(), [op(1)]);
    let late = Cells::from([(key(), register(&[(op(3), plain("c"))]))]);
    let superseded = restated(&late, &BTreeSet::from([op(1), op(3)]), &target, &carried).unwrap();
    assert_eq!(written(&superseded), vec![(op(3), plain("c"))]);

    // A late value from a Mac that never saw the carried one: both stand.
    let concurrent = restated(&late, &BTreeSet::from([op(3)]), &target, &carried).unwrap();
    assert_eq!(
        written(&concurrent),
        vec![(op(1), plain("a")), (op(3), plain("c"))]
    );

    // An owner's edit in the new generation, which never saw the late value: both stand.
    let edited = Cells::from([(key(), register(&[(op(8), plain("e"))]))]);
    let beside = restated(&late, &BTreeSet::from([op(1), op(3)]), &edited, &carried).unwrap();
    assert_eq!(
        written(&beside),
        vec![(op(3), plain("c")), (op(8), plain("e"))]
    );
}

#[test]
fn carrying_one_cell_never_marks_another_cell_of_the_same_operation_carried() {
    for other in [
        (PROVIDER.name.to_owned(), "openai.site".to_owned()),
        (MODEL.name.to_owned(), key().1),
    ] {
        let source = Cells::from([
            (
                key(),
                register(&[(op(1), plain("https://one.example.test"))]),
            ),
            (
                other.clone(),
                register(&[(op(1), plain("https://two.example.test"))]),
            ),
        ]);
        let held = BTreeSet::from([op(1)]);
        let rows = restated(&source, &held, &Cells::new(), &Carried::new()).unwrap();
        let first = rows
            .iter()
            .flatten()
            .find(|change| (change.collection.clone(), change.key.clone()) == key())
            .unwrap()
            .clone();
        let carried = carried_origins([&Command::Write {
            request: [0; 16],
            changes: vec![first.clone()],
        }]);
        let target = Cells::from([(key(), register(&[(op(9), first.value.unwrap())]))]);
        let resumed = restated(&source, &held, &target, &carried).unwrap();
        assert_eq!(resumed.len(), 1);
        assert_eq!(
            written_at(&resumed, &other),
            vec![(op(1), plain("https://two.example.test"))]
        );
    }
}

#[test]
fn an_already_carried_value_still_cleans_up_a_superseded_late_candidate() {
    let source = Cells::from([(key(), register(&[(op(2), plain("b"))]))]);
    let held = BTreeSet::from([op(1), op(2)]);
    let carried = carried_at(&key(), held.iter().copied());
    let target = Cells::from([(
        key(),
        register(&[(
            op(9),
            cell::carry(&BTreeMap::from([(op(1), plain("a")), (op(2), plain("b"))])).unwrap(),
        )]),
    )]);
    let rows = restated(&source, &held, &target, &carried).unwrap();
    assert_eq!(written(&rows), vec![(op(2), plain("b"))]);

    // The new generation's edit already replaced b; removing the stale a must not revive b.
    let edited = Cells::from([(
        key(),
        register(&[(
            op(9),
            cell::carry(&BTreeMap::from([(op(1), plain("a")), (op(8), plain("e"))])).unwrap(),
        )]),
    )]);
    let rows = restated(&source, &held, &edited, &carried).unwrap();
    assert_eq!(written(&rows), vec![(op(8), plain("e"))]);

    // Nothing in the new generation survives this cleanup. Do not turn its prior removal
    // of b into a resurrection, or encode an empty carried list as a malformed value.
    let stale = Cells::from([(
        key(),
        register(&[(
            op(9),
            cell::carry(&BTreeMap::from([(op(1), plain("a"))])).unwrap(),
        )]),
    )]);
    let rows = restated(&source, &held, &stale, &carried).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].len(), 1);
    assert_eq!(
        (rows[0][0].collection.clone(), rows[0][0].key.clone()),
        key()
    );
    assert_eq!(rows[0][0].value, None);
    assert!(
        restated(&source, &held, &Cells::new(), &carried)
            .unwrap()
            .is_empty()
    );
    let cleared = Cells::from([(key(), register(&[(op(10), Value::Null)]))]);
    assert!(
        restated(&source, &held, &cleared, &carried)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_new_late_candidate_does_not_revive_an_already_carried_sibling() {
    let source = Cells::from([(key(), register(&[(op(2), plain("b")), (op(3), plain("c"))]))]);
    let carried = carried_at(&key(), [op(2)]);
    let held = BTreeSet::from([op(2), op(3)]);
    let target = Cells::from([(key(), register(&[(op(8), plain("e"))]))]);
    let rows = restated(&source, &held, &target, &carried).unwrap();
    assert_eq!(
        written(&rows),
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

#[test]
fn a_roll_cleanup_is_not_suppressed_by_an_earlier_identical_write() {
    let lab = Lab::new(&[Role::Writer]);
    Registry::at(lab.paths[0].clone()).open().unwrap();
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let compiled = &att::REGISTRY;
    let scope = compiled.descriptor(bootstrap.trust()).unwrap().scope;
    let directory = Directory::new(&lab.paths[0], bootstrap.trust());
    let mut store = Store::open(&session, &directory.path(scope).unwrap(), scope).unwrap();
    let changes = vec![Change {
        collection: key().0,
        key: key().1,
        value: Some(
            cell::carry(&BTreeMap::from([(
                op(1),
                plain("https://one.example.test"),
            )]))
            .unwrap(),
        ),
    }];
    let log = Log::read_with(&session, &store, scope, decode).unwrap();
    let first = Command::Write {
        request: typed::request_id(
            APPLICATION,
            &request_role(compiled.table.role, &log),
            &changes,
        )
        .unwrap(),
        changes: changes.clone(),
    };
    let receipt = application::append(&session, &mut store, &first.encode().unwrap()).unwrap();
    let mut late = changes.clone();
    late[0].value = Some(
        cell::carry(&BTreeMap::from([
            (op(1), plain("https://one.example.test")),
            (op(2), plain("https://two.example.test")),
        ]))
        .unwrap(),
    );
    let late = Command::Write {
        request: [240; 16],
        changes: late,
    };
    application::append(&session, &mut store, &late.encode().unwrap()).unwrap();
    let log = Log::read_with(&session, &store, scope, decode).unwrap();
    assert_eq!(
        log.retry(session.actor(), &first).unwrap(),
        Some(receipt.id)
    );
    let view = View::read(compiled.table, &session, &log);
    let source = Cells::from([(
        key(),
        register(&[(op(1), plain("https://one.example.test"))]),
    )]);
    let carried = carried_origins(log.eligible_iter().map(|(_, command)| command));
    let rows = restated(
        &source,
        &BTreeSet::from([op(1), op(2)]),
        &view.cells,
        &carried,
    )
    .unwrap();
    let mut records = pack(rows).unwrap();
    assert_eq!(records.len(), 1);
    let again = records.pop().unwrap();
    assert_eq!(again, changes);
    let cleanup = Command::Write {
        request: typed::request_id(
            APPLICATION,
            &request_role(compiled.table.role, &log),
            &again,
        )
        .unwrap(),
        changes: again,
    };
    if att::retry_or_appended(&log, session.actor(), &cleanup, &BTreeMap::new())
        .unwrap()
        .is_none()
    {
        application::append(&session, &mut store, &cleanup.encode().unwrap()).unwrap();
    }
    let log = Log::read_with(&session, &store, scope, decode).unwrap();
    let cleaned = log
        .retry(session.actor(), &cleanup)
        .unwrap()
        .expect("the cleanup has its own retry receipt");
    assert_ne!(cleaned, receipt.id);
    let providers = View::read(compiled.table, &session, &log).project(&PROVIDER);
    assert!(!providers.rows["anthropic"].fields["site"].conflicted());
    assert_eq!(
        providers.rows["anthropic"].get("site"),
        Some(&json!("https://one.example.test"))
    );
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

/// [`release`] on Mac `n` judging the fleet floor by `store`.
fn release_with(lab: &Lab, n: usize, store: &Memory) -> Released {
    Registry::at(lab.paths[n].clone())
        .release_with(now_ms(), store)
        .unwrap()
}

/// What Mac `n` keeps of the first generation; the second, which has a successor now, keeps
/// itself until it is sealed.
fn first_kept(lab: &Lab, n: usize, first: Digest) -> Vec<(Digest, Kept)> {
    let mut kept = release(lab, n).kept;
    kept.retain(|(id, _)| *id == first);
    kept
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
    assert!(crate::att::REGISTRY_3.successor.is_none());

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
/// every Mac releases it and all three read alike. Each repair names the table that moved, as a
/// push does, in as many rounds as that record needs to reach all three (two where a peer wrote
/// it: m1 in the first, the other peer in the second);
/// `a_roll_restates_everything_and_releases_the_sealed_generation` holds the regular scan across
/// generations. NEGATIVE CONTROLS: with `within` always true, m2 releases right after the heal and
/// its `NotCarried` assertion fails; with `restate` writing the late candidates alone, m1's edit is
/// lost and the conflict assertion fails. It counts who the first roll waits on rather than naming
/// them, writes no facts row on m2, and asserts that m1 released its first generation only at the
/// end, not in the cut; nothing runs the regular scan on three Macs. `lab alab-roll`, whose judge
/// read those, is gone.
#[test]
fn a_late_edit_is_carried_before_its_mac_releases() {
    let lab = Lab::new(&[Role::Writer, Role::Writer, Role::Writer]);
    let macs: Vec<Registry> = lab.paths.iter().cloned().map(Registry::at).collect();
    macs[0].open().unwrap();
    macs[0].seed().unwrap();
    macs[0].write(vec![facts(&lab, 0, "m1")]).unwrap();
    let trust = lab.bootstrap(0).trust();
    let catalog = crate::catalog::scope(trust).digest();
    let g1 = att::REGISTRY.descriptor(trust).unwrap().scope.digest();
    let g2 = att::REGISTRY_2.descriptor(trust).unwrap().scope.digest();
    repair_commons_first(&lab, 1, catalog);
    repair_commons_first(&lab, 1, g1);
    macs[2].write(vec![facts(&lab, 2, "m3")]).unwrap();
    repair_commons_first(&lab, 2, g1);
    let now = [aterm_time::unix_secs().unwrap(); 3];

    // m1 restates; m2 and m3 hear the second generation and declare it on their release pass,
    // as every Mac running this build does on a claims write.
    assert_eq!(macs[0].roll(true, now_ms()).unwrap().waiting.len(), 2);
    repair_commons_first(&lab, 1, catalog);
    repair_commons_first(&lab, 1, g2);
    release(&lab, 1);
    release(&lab, 2);
    repair_commons_first(&lab, 2, catalog);

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
    repair_commons_among_first(&lab, 1, &[1, 2], &now, g1);

    // The heal.
    repair_commons_first(&lab, 1, catalog);
    repair_commons_first(&lab, 1, g2);
    for (n, registry) in macs.iter().enumerate() {
        assert_eq!(registry.generation().generation, 2, "m{}", n + 1);
    }
    let first = first_id(&lab);
    for n in [1, 2] {
        assert_eq!(
            first_kept(&lab, n, first),
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
    repair_commons_first(&lab, 2, g2);
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
/// read-only. Each repair names the table that moved, as a push does;
/// `a_roll_restates_everything_and_releases_the_sealed_generation` holds the regular scan across
/// generations.
/// GAP: this test no longer runs regular scans or idle laps during a roll, so a seal or cover row
/// that arrives only through scan rotation is not seen here.
/// NEGATIVE CONTROL: with `covers` taking every row whatever its signer, m2's pass
/// releases the first generation and its `NotCarried` assertion fails.
#[test]
fn a_cover_written_for_another_mac_counts_for_nothing() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let (a, b) = (
        Registry::at(lab.paths[0].clone()),
        Registry::at(lab.paths[1].clone()),
    );
    let trust = lab.bootstrap(0).trust();
    let catalog = crate::catalog::scope(trust).digest();
    let g1 = att::REGISTRY.descriptor(trust).unwrap().scope.digest();
    let g2 = att::REGISTRY_2.descriptor(trust).unwrap().scope.digest();
    a.open().unwrap();
    a.seed().unwrap();
    repair_commons_first(&lab, 1, catalog);
    repair_commons_first(&lab, 1, g1);
    a.roll(true, now_ms()).unwrap();
    repair_commons_first(&lab, 1, catalog);
    repair_commons_first(&lab, 1, g2);
    release(&lab, 1);
    repair_commons_first(&lab, 1, catalog);
    assert!(a.roll(true, now_ms()).unwrap().sealed);
    // b edits the first generation before it hears the seal.
    b.write(vec![vec![provider(
        "anthropic",
        "site",
        json!("https://late.example.test"),
    )]])
    .unwrap();
    // a released the first generation when it sealed, so g2 is the table the two still share.
    repair_commons_first(&lab, 1, catalog);
    repair_commons_first(&lab, 1, g2);
    let first = first_id(&lab);
    // `NotCarried` is also what a Mac with no cover reports, so a's cover must be here first.
    let cover = keys::roll_pk(1, &keys::uuid32(&lab.machine(0)));
    assert!(
        b.view().unwrap().project(&ROLL).rows.contains_key(&cover),
        "a's cover reached b"
    );
    assert_eq!(first_kept(&lab, 1, first), vec![(first, Kept::NotCarried)]);

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
    repair_commons_first(&lab, 1, g2);
    assert_eq!(first_kept(&lab, 1, first), vec![(first, Kept::NotCarried)]);
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

/// Mac `n` rolls the registry as v0.154.0 did: it declares the second generation and restates
/// every register of the first into it as it was, a sealed cell's token copied (still sealed to
/// the first generation's table), then writes its cover row last. Its seal step is v0.154.0's and
/// waits for every Writer's declaration, so it is not run here: `Directory::undeclared` is what
/// it would wait on.
fn copying_roll(lab: &Lab, n: usize) {
    let bootstrap = lab.bootstrap(n);
    let session = Session::open(&bootstrap).unwrap();
    let trust = bootstrap.trust();
    let directory = Directory::new(&lab.paths[n], trust);
    let (from, to) = (&att::REGISTRY, &att::REGISTRY_2);
    let old_scope = from.descriptor(trust).unwrap().scope;
    let old = directory
        .observe_store_if_exists(&session, old_scope)
        .unwrap()
        .unwrap();
    let old_log = Log::read_with(&session, &old, old_scope, decode).unwrap();
    let old_view = View::read(from.table, &session, &old_log);
    let to_descriptor = to.descriptor(trust).unwrap();
    directory.publish(&session, &to_descriptor).unwrap();
    directory.endorse(&session, &to_descriptor).unwrap();
    let mut store = Store::open(
        &session,
        &directory.path(to_descriptor.scope).unwrap(),
        to_descriptor.scope,
    )
    .unwrap();
    let body: Vec<Vec<Change>> = old_view
        .cells
        .iter()
        .filter(|((collection, _), cell)| {
            !NOT_RESTATED.contains(&collection.as_str())
                && !cell.stopped
                && from
                    .table
                    .collection(collection)
                    .is_some_and(|known| known.kind == typed::Kind::Cells)
        })
        .map(|((collection, key), cell)| {
            let copied: BTreeMap<Digest, Value> = cell::origins(cell).into_iter().collect();
            vec![Change {
                collection: collection.clone(),
                key: key.clone(),
                value: Some(cell::carry(&copied).unwrap()),
            }]
        })
        .collect();
    let uuid = keys::uuid32(&lab.machine(n));
    let held = held_of(&old_log);
    let cover = cover_row(
        &Projection::default(),
        from.generation,
        &uuid,
        &frontier(&held),
        held.len(),
        now_ms(),
    )
    .unwrap();
    let mut records = pack(body).unwrap();
    records.extend(pack(vec![cover]).unwrap());
    for changes in records {
        let request = typed::request_id(APPLICATION, to.table.role, &changes).unwrap();
        let command = Command::Write { request, changes };
        application::append(&session, &mut store, &command.encode().unwrap()).unwrap();
    }
}

/// Whether Mac `n`'s catalog shows Mac `m` has not declared the registry's second generation.
fn undeclared_by(lab: &Lab, n: usize, m: usize) -> bool {
    let bootstrap = lab.bootstrap(n);
    let session = Session::open(&bootstrap).unwrap();
    let directory = Directory::new(&lab.paths[n], bootstrap.trust());
    let descriptor = att::REGISTRY_2.descriptor(bootstrap.trust()).unwrap();
    Directory::undeclared(
        &session,
        &directory.snapshot(&session).unwrap(),
        &descriptor,
    )
    .unwrap()
    .contains(&lab.machine(m))
}

/// `aterm db alab check` on Mac `n`: its findings naming a generation whose tokens do not open.
fn unopened_findings(lab: &Lab, n: usize) -> Vec<Value> {
    let check = crate::alab::command::run(
        &lab.paths[n],
        &["check".to_owned()],
        None,
        &mut |_| panic!("a check opens no review"),
        now_ms(),
    )
    .unwrap();
    check["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["finding"] == json!("generation-unopened"))
        .cloned()
        .collect()
}

/// The text of every register value `rows` writes, for a negative control.
fn written_text(rows: &[Vec<Change>]) -> String {
    rows.iter()
        .flatten()
        .filter_map(|change| change.value.as_ref())
        .map(Value::to_string)
        .collect()
}

/// A typed password candidate holding `token`, as an earlier build wrote it.
fn sealed_at(token: &str) -> Value {
    CellValue::owner(2, json!(token)).encode()
}

/// A password an earlier build sealed rolls, into the third generation, into a reference to a
/// store entry of its coordinates named by the operation that wrote it
/// ([`secret::moved_entry`], so every Mac rolling the same record names the same entry), and the
/// value goes into the store; an equal value of the register shares the first's entry. Planning
/// puts nothing. A re-run once carried writes and puts nothing, and a late token beside an
/// owner's edit in the new generation takes its own operation's entry, never the one that edit
/// names. NEGATIVE CONTROL: no change written holds a value or a token; with `moved` leaving the
/// token in `x`, `` a carried change holds a value or token ``.
#[test]
fn a_sealed_password_rolls_into_a_store_reference() {
    let pk = keys::address_pk("ann@example.test").unwrap();
    let key = (ADDRESS.name.to_owned(), format!("{pk}.password"));
    let at = |n: u8| secret::moved_entry((ADDRESS.name, "password", &pk), &op(n)).unwrap();
    let (entry, second) = (at(1), at(2));
    let (one, two) = ("synthetic-pass-1", "synthetic-pass-2");
    let seal = |value: &str| {
        OLD.sealed(
            ADDRESS.name,
            &pk,
            "password",
            &Secret::new(value.to_owned()),
        )
    };
    let tokens = [seal(one), seal(two), seal(one)];
    let source = Cells::from([(
        key.clone(),
        register(&[
            (op(1), sealed_at(&tokens[0])),
            (op(2), sealed_at(&tokens[1])),
            (op(3), sealed_at(&tokens[2])),
        ]),
    )]);
    let held = BTreeSet::from([op(1), op(2), op(3)]);
    let store = Memory::default();
    let Restated { rows, puts, .. } = restate(
        &REGISTRY_3,
        &source,
        &held,
        &Cells::new(),
        &Carried::new(),
        (&OLD, None, &store),
    )
    .unwrap();
    assert!(store.every().is_empty(), "restating puts nothing");
    for (name, value) in &puts {
        store.put(name, value).unwrap();
    }
    assert_eq!(
        store.names().into_iter().collect::<BTreeSet<_>>(),
        BTreeSet::from([entry.clone(), second.clone()])
    );
    assert_eq!(store.held(&entry).as_deref(), Some(one));
    assert_eq!(store.held(&second).as_deref(), Some(two));
    let reference =
        |name: &str| CellValue::owner(2, secret::reference(Memory::VAULT, name)).encode();
    assert_eq!(
        written_at(&rows, &key),
        vec![
            (op(1), reference(&entry)),
            (op(2), reference(&second)),
            (op(3), reference(&entry)),
        ]
    );
    let text = written_text(&rows);
    for leaked in tokens.iter().map(String::as_str).chain([one, two]) {
        assert!(
            !text.contains(leaked),
            "a carried change holds a value or token"
        );
    }

    // Carried: a re-run writes nothing and puts nothing.
    let change = rows.iter().flatten().next().unwrap().clone();
    let target = Cells::from([(
        key.clone(),
        register(&[(op(9), change.value.clone().unwrap())]),
    )]);
    let done = carried_origins([&Command::Write {
        request: [0; 16],
        changes: vec![change],
    }]);
    let again = restate(
        &REGISTRY_3,
        &source,
        &held,
        &target,
        &done,
        (&OLD, None, &store),
    )
    .unwrap();
    assert!(again.rows.is_empty() && again.puts.is_empty());

    // An owner's edit in the new generation names the first entry; a late token from a Mac that
    // never saw it goes beside it, under its own operation's entry.
    let edited = Cells::from([(key.clone(), register(&[(op(8), reference(&entry))]))]);
    let late = Cells::from([(key.clone(), register(&[(op(4), sealed_at(&seal(two)))]))]);
    let beside = restate(
        &REGISTRY_3,
        &late,
        &BTreeSet::from([op(4)]),
        &edited,
        &Carried::new(),
        (&OLD, None, &store),
    )
    .unwrap();
    let fourth = at(4);
    assert_eq!(
        beside.puts.iter().map(|(name, _)| name).collect::<Vec<_>>(),
        vec![&fourth]
    );
    assert_eq!(
        written_at(&beside.rows, &key),
        vec![(op(4), reference(&fourth)), (op(8), reference(&entry))]
    );

    // An emptied password, typed or a revert's bare `null`, carries as it is.
    for empty in [CellValue::owner(2, Value::Null).encode(), Value::Null] {
        let cleared = Cells::from([(key.clone(), register(&[(op(5), empty.clone())]))]);
        let rows = restated(&cleared, &BTreeSet::from([op(5)]), &Cells::new(), &none()).unwrap();
        assert_eq!(written_at(&rows, &key), vec![(op(5), empty)]);
    }
}

/// A late roll that drops a candidate the new generation holds (the old history shows a late
/// record superseded it) owes the entry that candidate named, so a sweep collects it once no
/// cell names it, and an emptied register owes them all; an entry another kept candidate names
/// is not owed. A roll that would name again an entry the store no longer holds (another Mac's
/// write removed it, unseen here) stops, naming coordinates only, and writes nothing.
/// NEGATIVE CONTROLS: with `drops` never pushed, `` left: [] right: ["alab-address-password-…"] ``;
/// with the store check skipped, the dangling re-carry is written:
/// `` called `Result::unwrap_err()` on an `Ok` value ``.
#[test]
fn a_late_roll_owes_what_it_drops_and_never_names_a_removed_entry_again() {
    let pk = keys::address_pk("ann@example.test").unwrap();
    let key = (ADDRESS.name.to_owned(), format!("{pk}.password"));
    let at = |n: u8| secret::moved_entry((ADDRESS.name, "password", &pk), &op(n)).unwrap();
    let reference =
        |name: &str| CellValue::owner(2, secret::reference(Memory::VAULT, name)).encode();
    let seal = |value: &str| {
        sealed_at(&OLD.sealed(
            ADDRESS.name,
            &pk,
            "password",
            &Secret::new(value.to_owned()),
        ))
    };
    let store = Memory::default();
    store
        .put(&at(1), &Secret::new("synthetic-pass-1".to_owned()))
        .unwrap();
    // A carried O (op 1) into the new generation; the late record L (op 2) superseded it in the
    // old one, so the old register lists only L.
    let target = Cells::from([(key.clone(), register(&[(op(1), reference(&at(1)))]))]);
    let carried = carried_at(&key, [op(1)]);
    let held = BTreeSet::from([op(1), op(2)]);
    let late = Cells::from([(key.clone(), register(&[(op(2), seal("synthetic-pass-2"))]))]);
    let rolled = restate(
        &REGISTRY_3,
        &late,
        &held,
        &target,
        &carried,
        (&OLD, None, &store),
    )
    .unwrap();
    assert_eq!(
        written_at(&rolled.rows, &key),
        vec![(op(2), reference(&at(2)))]
    );
    assert_eq!(rolled.drops, vec![at(1)]);
    // An emptied register owes everything it named.
    let emptied = Cells::from([(key.clone(), register(&[(op(2), Value::Null)]))]);
    let rolled = restate(
        &REGISTRY_3,
        &emptied,
        &held,
        &target,
        &carried,
        (&OLD, None, &store),
    )
    .unwrap();
    assert_eq!(rolled.drops, vec![at(1)]);
    // Equal values share an entry: dropping one candidate keeps the entry the other names.
    let shared = Cells::from([(
        key.clone(),
        register(&[(op(1), reference(&at(1))), (op(3), reference(&at(1)))]),
    )]);
    let rolled = restate(
        &REGISTRY_3,
        &late,
        &BTreeSet::from([op(1), op(2)]),
        &shared,
        &carried,
        (&OLD, None, &store),
    )
    .unwrap();
    assert!(rolled.drops.is_empty(), "{:?}", rolled.drops);

    // Another Mac's write removed the entry O names; this Mac's view still holds O, and the old
    // register holds O beside a late L that never superseded it.
    store.remove(&at(1)).unwrap();
    let beside = Cells::from([(
        key.clone(),
        register(&[
            (op(1), seal("synthetic-pass-1")),
            (op(2), seal("synthetic-pass-2")),
        ]),
    )]);
    let refused = restate(
        &REGISTRY_3,
        &beside,
        &held,
        &target,
        &carried,
        (&OLD, None, &store),
    )
    .err()
    .map(|error| error.to_string());
    let refused = refused.unwrap_or_default();
    assert!(refused.contains("does not hold"), "{refused}");
    assert!(!refused.contains("synthetic-pass"), "{refused}");
}

/// A roll into the second generation, which builds up to 0.199.0 read, reseals each token under
/// that generation's key at the same coordinates, as those builds' own roll does: no reference
/// is written there and nothing goes into the store. NEGATIVE CONTROL: the same roll with no
/// resealer at hand stops (`the new generation's sealer is not at hand`), so no path writes a
/// reference below the third generation.
#[test]
fn a_roll_into_the_second_generation_reseals_and_writes_no_reference() {
    let pk = keys::address_pk("ann@example.test").unwrap();
    let key = (ADDRESS.name.to_owned(), format!("{pk}.password"));
    let token = OLD.sealed(
        ADDRESS.name,
        &pk,
        "password",
        &Secret::new("synthetic-pass-1".to_owned()),
    );
    let source = Cells::from([(key.clone(), register(&[(op(1), sealed_at(&token))]))]);
    let held = BTreeSet::from([op(1)]);
    let store = Memory::default();
    let resealed = restate(
        &REGISTRY_2,
        &source,
        &held,
        &Cells::new(),
        &Carried::new(),
        (&OLD, Some(&OTHER), &store),
    )
    .unwrap();
    assert!(resealed.puts.is_empty(), "nothing goes into the store");
    let written = written_at(&resealed.rows, &key);
    assert_eq!(written.len(), 1);
    let x = CellValue::decode(&written[0].1).unwrap().x;
    let carried = x.as_str().unwrap();
    assert!(
        secret::is_sealed(carried),
        "a sealed token, never a reference"
    );
    assert!(secret::entry_of(&x).is_none());
    assert_ne!(carried, token, "sealed again for the second generation");
    assert_eq!(
        OTHER
            .open(ADDRESS.name, &pk, "password", carried)
            .unwrap()
            .plaintext(),
        "synthetic-pass-1"
    );
    let refused = restate(
        &REGISTRY_2,
        &source,
        &held,
        &Cells::new(),
        &Carried::new(),
        (&OLD, None, &store),
    )
    .err()
    .unwrap();
    assert!(
        refused.to_string().contains("sealer is not at hand"),
        "{refused}"
    );
    assert!(store.every().is_empty());
}

fn none() -> Carried {
    Carried::new()
}

/// A phone number an earlier build sealed rolls into the number itself, the ordinary data a
/// `Phone` cell holds, in its one stored form (its digits, a US number without its leading 1,
/// `typed::cell::phone_digits`); nothing goes into the store. NEGATIVE CONTROL: the
/// token is in no carried change; with `moved` leaving it in `x`, `` the token was carried ``.
#[test]
fn a_sealed_phone_number_rolls_into_the_number() {
    let pk = keys::random_pk(keys::Random::Phone).unwrap();
    let key = (PHONE.name.to_owned(), format!("{pk}.number"));
    let token = OLD.sealed(
        PHONE.name,
        &pk,
        "number",
        &Secret::new("+1 555-010-0123".to_owned()),
    );
    let source = Cells::from([(key.clone(), register(&[(op(1), sealed_at(&token))]))]);
    let store = Memory::default();
    let restated = restate(
        &REGISTRY_3,
        &source,
        &BTreeSet::from([op(1)]),
        &Cells::new(),
        &Carried::new(),
        (&OLD, None, &store),
    )
    .unwrap();
    assert!(restated.puts.is_empty());
    assert_eq!(
        written_at(&restated.rows, &key),
        vec![(op(1), CellValue::owner(2, json!("5550100123")).encode())]
    );
    assert!(
        !written_text(&restated.rows).contains(&token),
        "the token was carried"
    );
}

/// A token that does not open at its coordinates (sealed for another table) stops the roll, and
/// so does one in a column that holds no secret; each error names the coordinates and neither
/// the token nor the value. NEGATIVE CONTROL: with `moved` naming the token in its error,
/// `` an error names a token or a value ``.
#[test]
fn a_token_that_does_not_open_stops_the_roll() {
    let pk = keys::address_pk("ann@example.test").unwrap();
    let value = "synthetic-pass-3";
    let foreign = OTHER.sealed(
        ADDRESS.name,
        &pk,
        "password",
        &Secret::new(value.to_owned()),
    );
    let misplaced = OLD.sealed(ADDRESS.name, &pk, "addr", &Secret::new(value.to_owned()));
    for (column, token) in [("password", &foreign), ("addr", &misplaced)] {
        let source = Cells::from([(
            (ADDRESS.name.to_owned(), format!("{pk}.{column}")),
            register(&[(op(1), sealed_at(token))]),
        )]);
        let store = Memory::default();
        let Err(error) = restate(
            &REGISTRY_3,
            &source,
            &BTreeSet::from([op(1)]),
            &Cells::new(),
            &Carried::new(),
            (&OLD, None, &store),
        ) else {
            panic!("a {column} token that cannot move rolled");
        };
        let error = error.to_string();
        assert!(error.contains(&pk) && error.contains(column), "{error}");
        assert!(
            !error.contains(token.as_str()) && !error.contains(value),
            "an error names a token or a value"
        );
        assert!(store.every().is_empty());
    }
}

/// A register is reported where any candidate's sealed token does not open under the
/// generation's opener: one sealed for another table, alone or beside one that opens; a token
/// that opens, a reference, a typed or bare `null` and a plain value are not. Coordinates only.
/// NEGATIVE CONTROL: with `unopened` taking any token as opening, the report is empty: ``
/// assertion `left == right` failed ``.
#[test]
fn a_token_sealed_for_another_table_is_reported_unopened() {
    let value = Secret::new("synthetic-pass-4".to_owned());
    let at = |addr: &str| keys::address_pk(addr).unwrap();
    let token =
        |opener: &Fixed, pk: &str| sealed_at(&opener.sealed(ADDRESS.name, pk, "password", &value));
    let password = |pk: &str| (ADDRESS.name.to_owned(), format!("{pk}.password"));
    let (copied, opens, moved_out, typed_null, bare_null, beside) = (
        at("ann@example.test"),
        at("bob@example.test"),
        at("cy@example.test"),
        at("di@example.test"),
        at("ed@example.test"),
        at("fy@example.test"),
    );
    let carried_both = cell::carry(&BTreeMap::from([
        (op(5), token(&OTHER, &beside)),
        (op(6), token(&OLD, &beside)),
    ]))
    .unwrap();
    let cells = Cells::from([
        (
            password(&copied),
            register(&[(op(1), token(&OLD, &copied))]),
        ),
        (
            password(&opens),
            register(&[(op(2), token(&OTHER, &opens))]),
        ),
        (
            password(&moved_out),
            register(&[(
                op(3),
                CellValue::owner(3, secret::reference(Memory::VAULT, "alab-x")).encode(),
            )]),
        ),
        (
            password(&typed_null),
            register(&[(op(4), CellValue::owner(2, Value::Null).encode())]),
        ),
        (password(&bare_null), register(&[(op(7), Value::Null)])),
        (password(&beside), register(&[(op(8), carried_both)])),
        (
            (ADDRESS.name.to_owned(), format!("{copied}.addr")),
            register(&[(op(9), plain("ann@example.test"))]),
        ),
    ]);
    let mut found = unopened(&cells, &OTHER);
    found.sort();
    let mut wanted = vec![
        (
            ADDRESS.name.to_owned(),
            copied.clone(),
            "password".to_owned(),
        ),
        (ADDRESS.name.to_owned(), beside, "password".to_owned()),
    ];
    wanted.sort();
    assert_eq!(found, wanted);
    // Under the other table's opener the copied token opens and the other one does not.
    let under_old = unopened(&cells, &OLD);
    assert!(under_old.iter().all(|(_, pk, _)| *pk != copied));
    assert!(under_old.iter().any(|(_, pk, _)| *pk == opens));
}

/// Mac `n` writes `changes` into the registry's first generation as an earlier build did, past
/// this build's cell guards: how a test makes the sealed history a roll migrates.
fn write_first_raw(lab: &Lab, n: usize, changes: Vec<Change>) {
    let bootstrap = lab.bootstrap(n);
    let session = Session::open(&bootstrap).unwrap();
    let directory = Directory::new(&lab.paths[n], bootstrap.trust());
    let scope = att::REGISTRY.descriptor(bootstrap.trust()).unwrap().scope;
    let mut store = Store::open(&session, &directory.path(scope).unwrap(), scope).unwrap();
    let request = typed::request_id(APPLICATION, att::REGISTRY.table.role, &changes).unwrap();
    let command = Command::Write { request, changes };
    application::append(&session, &mut store, &command.encode().unwrap()).unwrap();
}

/// `value` sealed as an earlier build sealed the first generation's cell `collection.pk.column`,
/// under Mac `n`'s audience key.
fn sealed_first(
    lab: &Lab,
    n: usize,
    collection: &str,
    pk: &str,
    column: &str,
    value: &str,
) -> String {
    let bootstrap = lab.bootstrap(n);
    let session = Session::open(&bootstrap).unwrap();
    let scope = att::REGISTRY.descriptor(bootstrap.trust()).unwrap().scope;
    let epoch = session.secret_basis();
    let key = session.secret_key(scope, &epoch).unwrap();
    let table = att::REGISTRY.name();
    Secret::new(value.to_owned())
        .seal(
            &key,
            &epoch,
            secret::Cell {
                table: &table,
                collection,
                row: pk,
                column,
            },
        )
        .unwrap()
}

/// `aterm db emails set <address> password --stdin` on Mac `n`, `value` piped in.
fn set_password(lab: &Lab, n: usize, address: &str, value: &str, store: &Memory) -> Result<Value> {
    let bytes = value.as_bytes().to_vec();
    let mut read = move || Ok(zeroize::Zeroizing::new(bytes.clone()));
    let mut open = |_: &std::path::Path, _: usize| -> Result<()> { panic!("no grid opens") };
    let mut io = crate::alab::command::views::ViewIo {
        open: &mut open,
        read_stdin: &mut read,
        store,
        holding: &|| Ok(usize::from(store.admit().is_err())),
        now_ms: now_ms(),
    };
    crate::alab::command::views::run(
        &lab.paths[n],
        crate::alab::command::views::Noun::Emails,
        &["set", address, "password", "--stdin"].map(str::to_owned),
        &mut io,
    )
}

/// Each record Mac `n` holds of the registry generation `compiled`: its id, and the text of every
/// value it writes.
fn records_of(lab: &Lab, n: usize, compiled: &Compiled) -> Vec<(Digest, String)> {
    let bootstrap = lab.bootstrap(n);
    let session = Session::open(&bootstrap).unwrap();
    let directory = Directory::new(&lab.paths[n], bootstrap.trust());
    let scope = compiled.descriptor(bootstrap.trust()).unwrap().scope;
    let store = directory
        .observe_store_if_exists(&session, scope)
        .unwrap()
        .unwrap();
    let log = Log::read_with(&session, &store, scope, decode).unwrap();
    View::read(att::REGISTRY_2.table, &session, &log)
        .ops
        .iter()
        .map(|op| (op.id, written_text(std::slice::from_ref(&op.changes))))
        .collect()
}

/// A v0.154.0-style roll, which copies a sealed cell's token as it was into the second
/// generation (where, sealed to the first table's name, it never opens), is refused at endorse
/// (one-mesh design §4.8, §11 step A6): the other Mac's release pass leaves it undeclared, so the
/// roll's seal keeps waiting for that Mac, and its `check` names generation 2 by number and
/// table digest, never the value. This build's roll into the second generation, which builds up
/// to 0.199.0 read, reseals the password there and puts nothing into the store, so the other
/// Mac's next pass declares it, the check clears, and the roll seals. Its roll into the third
/// moves the value into the store and carries a reference. While b has not declared the third
/// (a mixed fleet), the second stays current and a secret written there is sealed into its cell,
/// a clear there sweeps nothing, and every moved entry, even one a run cut short left owed,
/// survives; b declares the third once its floor admits; once
/// sealed, the third generation names it and a set keeps it again. NEGATIVE CONTROLS: with
/// `endorse` declaring an `Unopened` generation too, b's first pass declares the copying roll's
/// generation: `` assertion failed: undeclared_by(&lab, 0, 1) ``; no record this build's roll
/// writes holds the token or the value: `` a rolled record holds a value or token ``; with
/// `settle_write` sweeping whatever `sole_generation` says, the clear at the second generation
/// removes the moved entry: `` a moved entry survives a clear below the third generation ``;
/// with `sole_generation` not asking whether a sealed generation is still stored,
/// `` a stored sealed generation stops a sweep `` fails.
#[test]
fn a_copying_rolls_token_is_refused_at_endorse_until_a_roll_moves_it() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let a = Registry::at(lab.paths[0].clone());
    a.open().unwrap();
    a.seed().unwrap();
    let pk = keys::address_pk("ann@example.test").unwrap();
    let value = "synthetic-pass-5";
    a.write(vec![address("ann@example.test")]).unwrap();
    a.write(vec![address("bob@example.test")]).unwrap();
    let token = sealed_first(&lab, 0, ADDRESS.name, &pk, "password", value);
    let key = (ADDRESS.name.to_owned(), format!("{pk}.password"));
    write_first_raw(
        &lab,
        0,
        vec![Change {
            collection: key.0.clone(),
            key: key.1.clone(),
            value: Some(CellValue::owner(ADDRESS.version, json!(token)).encode()),
        }],
    );
    repair_commons(&lab, 6);

    copying_roll(&lab, 0);
    repair_commons(&lab, 6);
    assert!(undeclared_by(&lab, 1, 1));
    // b's release pass (a claims write's housekeeping) judges it and declares nothing.
    release(&lab, 1);
    repair_commons(&lab, 6);
    assert!(undeclared_by(&lab, 0, 1));
    assert!(undeclared_by(&lab, 1, 1));
    let found = unopened_findings(&lab, 1);
    let g2 = aterm_codec::hex::encode_lower(
        &att::REGISTRY_2
            .descriptor(lab.bootstrap(0).trust())
            .unwrap()
            .scope
            .digest(),
    );
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0]["generation"], json!(2));
    assert_eq!(found[0]["table"], json!(g2));
    assert_eq!(found[0]["cells"], json!(1));
    assert_eq!(found[0]["endorsed"], json!(false));
    let line = found[0]["line"].as_str().unwrap();
    assert!(
        line.starts_with(&format!("registry generation 2 ({}…) ", &g2[..8]))
            && line.ends_with("not endorsed"),
        "{line}"
    );
    assert!(!line.contains(&token) && !line.contains(value));
    assert_eq!(a.generation().generation, 1);

    // This build's roll on a: planned, it writes nothing; applied, it reseals the password into
    // the second generation, which builds up to 0.199.0 read, under that generation's key, and
    // puts nothing into the store.
    let store = Memory::default();
    let before: BTreeSet<Digest> = records_of(&lab, 0, &att::REGISTRY_2)
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    let plan = a.roll_with(false, now_ms(), &store).unwrap();
    assert!(plan.registers >= 1 && plan.records == 0, "{plan:?}");
    let staged = a.roll_with(true, now_ms(), &store).unwrap();
    assert!(!staged.sealed && staged.records > 0, "{staged:?}");
    assert_eq!(staged.waiting, vec![lab.machine(1)]);
    assert!(
        store.every().is_empty(),
        "a roll into generation 2 puts nothing"
    );
    for (id, text) in records_of(&lab, 0, &att::REGISTRY_2) {
        if !before.contains(&id) {
            assert!(
                !text.contains(&token) && !text.contains(value) && !text.contains("\"entry\""),
                "a rolled record holds a value, the old token or a reference"
            );
        }
    }
    repair_commons(&lab, 6);
    release(&lab, 1);
    repair_commons(&lab, 6);
    assert!(!undeclared_by(&lab, 0, 1));
    assert!(unopened_findings(&lab, 1).is_empty());
    let rolled = a.roll_with(true, now_ms(), &store).unwrap();
    assert!(rolled.sealed && rolled.waiting.is_empty(), "{rolled:?}");
    assert_eq!(a.generation().generation, 2);
    assert!(store.every().is_empty());

    // The roll into the third generation, which no older build reads, moves the value into the
    // store. A mixed fleet: b has not declared it, so the second stays current, and a secret
    // write there is refused: no run writes a reference, or collects a moved entry, below it.
    let staged = a.roll_with(true, now_ms(), &store).unwrap();
    assert!(!staged.sealed, "{staged:?}");
    assert_eq!(staged.waiting, vec![lab.machine(1)]);
    let names = store.names();
    assert_eq!(names.len(), 1, "{names:?}");
    let entry = names[0].clone();
    assert!(
        secret::bound(ADDRESS.name, "password", &pk, &entry),
        "{entry}"
    );
    assert_eq!(store.held(&entry).as_deref(), Some(value));
    for (_, text) in records_of(&lab, 0, &att::REGISTRY_3) {
        assert!(
            !text.contains(&token) && !text.contains(value),
            "a rolled record holds a value or token"
        );
    }
    store.owe(std::slice::from_ref(&entry)).unwrap();
    store.end_runs();
    // A secret written to the second generation meanwhile is sealed into its cell, the one form
    // builds up to 0.200.0 read: nothing goes into the store, and the moved entry survives.
    let sealed = set_password(&lab, 0, "bob@example.test", "synthetic-pass-6", &store).unwrap();
    assert!(
        !aterm_json::to_string(&sealed)
            .unwrap()
            .contains("synthetic-pass-6")
    );
    assert_eq!(store.every(), vec![value.to_owned()]);
    assert_eq!(
        store.held(&entry).as_deref(),
        Some(value),
        "a moved entry survives an unsealed generation"
    );
    // A clear, which every generation takes, lands and settles: the sweep after it collects
    // nothing below the third generation, whose view names no moved entry.
    let cleared = set_password(&lab, 0, "bob@example.test", "", &store).unwrap();
    assert_eq!(
        store.held(&entry).as_deref(),
        Some(value),
        "a moved entry survives a clear below the third generation: {cleared}"
    );
    assert!(store.owed().contains(&entry));
    assert!(!att::sole_generation(
        &lab.paths[0],
        &att::REGISTRY,
        &att::REGISTRY_2
    ));
    repair_commons(&lab, 6);
    release_with(&lab, 1, &store);
    repair_commons(&lab, 6);
    let rolled = a.roll_with(true, now_ms(), &store).unwrap();
    assert!(rolled.sealed && rolled.waiting.is_empty(), "{rolled:?}");
    assert_eq!(a.generation().generation, 3);
    assert_eq!(
        store.every().iter().filter(|put| *put == value).count(),
        1,
        "a carried value is put once"
    );
    // Sealed: one generation, which names the entry; a sweep pays what is still owed and keeps it.
    store.end_runs();
    let set = set_password(&lab, 0, "bob@example.test", "synthetic-pass-7", &store).unwrap();
    assert_eq!(store.held(&entry).as_deref(), Some(value), "{set}");
    assert!(!store.owed().contains(&entry));
    // One generation is sole only for a handle on it: a write to a sealed one collects nothing.
    assert!(att::sole_generation(
        &lab.paths[0],
        &att::REGISTRY,
        &att::REGISTRY_3
    ));
    assert!(!att::sole_generation(
        &lab.paths[0],
        &att::REGISTRY,
        &att::REGISTRY_2
    ));
    // A sealed generation this Mac still stores (records not yet carried) stops it, too.
    let sealed_store = Directory::new(&lab.paths[0], lab.bootstrap(0).trust())
        .path(
            att::REGISTRY_2
                .descriptor(lab.bootstrap(0).trust())
                .unwrap()
                .scope,
        )
        .unwrap();
    let released = !sealed_store.exists();
    std::fs::create_dir_all(&sealed_store).unwrap();
    assert!(
        !att::sole_generation(&lab.paths[0], &att::REGISTRY, &att::REGISTRY_3),
        "a stored sealed generation stops a sweep"
    );
    if released {
        std::fs::remove_dir(&sealed_store).unwrap();
    }
    // A generation of the family this build does not compile (a newer build's) stops it too.
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let directory = Directory::new(&lab.paths[0], bootstrap.trust());
    let newer = crate::catalog::Descriptor::application(
        bootstrap.trust(),
        att::REGISTRY.table.application,
        "registry.9",
        1,
    )
    .unwrap();
    directory.publish(&session, &newer).unwrap();
    assert!(
        !att::sole_generation(&lab.paths[0], &att::REGISTRY, &att::REGISTRY_3),
        "an unknown later generation stops a sweep"
    );
    let reference = secret::reference(Memory::VAULT, &entry);
    let cells = a.view().unwrap().cells;
    let candidates = cell::origins_refs(&cells[&key]);
    assert!(!candidates.is_empty());
    assert!(
        candidates.iter().all(|(_, value)| value["x"] == reference),
        "the password register holds only the reference"
    );
}

/// A roll never writes over a value the store holds: a token's value goes into its own
/// operation's entry ([`secret::moved_entry`]), never the entry a reference of the register
/// names; where that entry already holds the same value (an earlier pass put it, or another Mac
/// rolling the same record) nothing is put, and where it holds another value the roll stops.
/// NEGATIVE CONTROLS: the owner's value survives the puts; with `moved` skipping the store
/// check, the put writes over `` an unnamed value survives ``.
#[test]
fn a_roll_never_writes_over_a_value_the_store_or_a_reference_holds() {
    let pk = keys::address_pk("ann@example.test").unwrap();
    let key = (ADDRESS.name.to_owned(), format!("{pk}.password"));
    let entry = secret::entry_name(ADDRESS.name, "password", &pk).unwrap();
    let at = |n: u8| secret::moved_entry((ADDRESS.name, "password", &pk), &op(n)).unwrap();
    let (owners, older) = ("synthetic-owner-1", "synthetic-older-0");
    let reference =
        |name: &str| CellValue::owner(2, secret::reference(Memory::VAULT, name)).encode();
    let token = OLD.sealed(
        ADDRESS.name,
        &pk,
        "password",
        &Secret::new(older.to_owned()),
    );
    let store = Memory::default();
    store.put(&entry, &Secret::new(owners.to_owned())).unwrap();
    let source = Cells::from([(
        key.clone(),
        register(&[(op(1), reference(&entry)), (op(2), sealed_at(&token))]),
    )]);
    let held = BTreeSet::from([op(1), op(2)]);
    let Restated { rows, puts, .. } = restate(
        &REGISTRY_3,
        &source,
        &held,
        &Cells::new(),
        &Carried::new(),
        (&OLD, None, &store),
    )
    .unwrap();
    for (name, value) in &puts {
        store.put(name, value).unwrap();
    }
    assert_eq!(
        store.held(&entry).as_deref(),
        Some(owners),
        "the owner's value survives"
    );
    assert_eq!(store.held(&at(2)).as_deref(), Some(older));
    assert_eq!(
        written_at(&rows, &key),
        vec![(op(1), reference(&entry)), (op(2), reference(&at(2)))]
    );

    // The entry already holds the same value: the reference is written, nothing is put.
    let lone = Cells::from([(key.clone(), register(&[(op(3), sealed_at(&token))]))]);
    let store = Memory::default();
    store.put(&at(3), &Secret::new(older.to_owned())).unwrap();
    let again = restate(
        &REGISTRY_3,
        &lone,
        &BTreeSet::from([op(3)]),
        &Cells::new(),
        &Carried::new(),
        (&OLD, None, &store),
    )
    .unwrap();
    assert!(again.puts.is_empty());
    assert_eq!(
        written_at(&again.rows, &key),
        vec![(op(3), reference(&at(3)))]
    );

    // The entry holds another value: the roll stops and the value stays.
    let store = Memory::default();
    store
        .put(&at(3), &Secret::new("unnamed-value-9".into()))
        .unwrap();
    assert!(
        restate(
            &REGISTRY_3,
            &lone,
            &BTreeSet::from([op(3)]),
            &Cells::new(),
            &Carried::new(),
            (&OLD, None, &store),
        )
        .is_err()
    );
    assert_eq!(
        store.held(&at(3)).as_deref(),
        Some("unnamed-value-9"),
        "an unnamed value survives"
    );
}

/// A secret candidate that is neither a sealed token nor a reference to its own cell's entry (a
/// plain value some writer left, or a reference to another entry) stops the roll, and the
/// refusal names no value.
/// NEGATIVE CONTROL: the same register holding the cell's own reference, to an entry the store
/// holds, rolls.
#[test]
fn a_roll_refuses_a_secret_cell_holding_a_plain_value_or_another_entry() {
    let pk = keys::address_pk("ann@example.test").unwrap();
    let key = (ADDRESS.name.to_owned(), format!("{pk}.password"));
    let plain = "Plain-password-77";
    let own = format!("alab-address-password-{pk}");
    let store = Memory::default();
    store
        .put(&own, &Secret::new("held-value-77".to_owned()))
        .unwrap();
    let roll = |x: Value| {
        let source = Cells::from([(
            key.clone(),
            register(&[(op(1), CellValue::owner(2, x).encode())]),
        )]);
        restate(
            &REGISTRY_3,
            &source,
            &BTreeSet::from([op(1)]),
            &Cells::new(),
            &Carried::new(),
            (&OLD, None, &store),
        )
    };
    let refused = roll(json!(plain)).err().unwrap().to_string();
    assert!(!refused.contains(plain), "{refused}");
    assert!(roll(secret::reference(Memory::VAULT, "gh_access_token")).is_err());
    assert!(roll(secret::reference(Memory::VAULT, &own)).is_ok());
}

/// [`copying_roll`] from generation `from` into `to`: the target declared, every register
/// restated as it was and the roller's cover of what it restated written last.
fn copying_roll_between(lab: &Lab, n: usize, from: &att::Compiled, to: &att::Compiled) {
    let bootstrap = lab.bootstrap(n);
    let session = Session::open(&bootstrap).unwrap();
    let trust = bootstrap.trust();
    let directory = Directory::new(&lab.paths[n], trust);
    let old_scope = from.descriptor(trust).unwrap().scope;
    let old = directory
        .observe_store_if_exists(&session, old_scope)
        .unwrap()
        .unwrap();
    let old_log = Log::read_with(&session, &old, old_scope, decode).unwrap();
    let old_view = View::read(from.table, &session, &old_log);
    let to_descriptor = to.descriptor(trust).unwrap();
    directory.publish(&session, &to_descriptor).unwrap();
    directory.endorse(&session, &to_descriptor).unwrap();
    let mut store = Store::open(
        &session,
        &directory.path(to_descriptor.scope).unwrap(),
        to_descriptor.scope,
    )
    .unwrap();
    let body: Vec<Vec<Change>> = old_view
        .cells
        .iter()
        .filter(|((collection, _), cell)| {
            !NOT_RESTATED.contains(&collection.as_str())
                && !cell.stopped
                && from
                    .table
                    .collection(collection)
                    .is_some_and(|known| known.kind == typed::Kind::Cells)
        })
        .map(|((collection, key), cell)| {
            let copied: BTreeMap<Digest, Value> = cell::origins(cell).into_iter().collect();
            vec![Change {
                collection: collection.clone(),
                key: key.clone(),
                value: Some(cell::carry(&copied).unwrap()),
            }]
        })
        .collect();
    let uuid = keys::uuid32(&lab.machine(n));
    let held = held_of(&old_log);
    let cover = cover_row(
        &Projection::default(),
        from.generation,
        &uuid,
        &frontier(&held),
        held.len(),
        now_ms(),
    )
    .unwrap();
    let mut records = pack(body).unwrap();
    records.extend(pack(vec![cover]).unwrap());
    for changes in records {
        let request = typed::request_id(APPLICATION, to.table.role, &changes).unwrap();
        let command = Command::Write { request, changes };
        application::append(&session, &mut store, &command.encode().unwrap()).unwrap();
    }
}

/// Whether Mac `n`'s catalog shows Mac `m` has not declared the registry's third generation.
fn third_undeclared_by(lab: &Lab, n: usize, m: usize) -> bool {
    let bootstrap = lab.bootstrap(n);
    let session = Session::open(&bootstrap).unwrap();
    let directory = Directory::new(&lab.paths[n], bootstrap.trust());
    let descriptor = att::REGISTRY_3.descriptor(bootstrap.trust()).unwrap();
    Directory::undeclared(
        &session,
        &directory.snapshot(&session).unwrap(),
        &descriptor,
    )
    .unwrap()
    .contains(&lab.machine(m))
}

/// A Mac vouches for the generation whose secret cells name store entries only once its fleet
/// floor admits a value: where another Mac rolled into it (here by hand: the generation
/// declared, every register restated and the roller's cover written), this Mac's release pass
/// with the production floor (no vault here to judge) declares nothing of it, though it judges
/// the generation whole and open, so that Mac's seal keeps waiting for this one as it waits for
/// an older build; with a floor that admits, the same pass declares it. While no release is
/// named as the first that writes no secret file, this build publishes no schema of it either.
/// CONTROL: the same pass declares the second generation, rolled from the first, which names no
/// store entry.
/// NEGATIVE CONTROL: with `endorse` not asking the floor for a `names_store` table, b's
/// production pass declares the third generation: `` assertion failed: third_undeclared_by(&lab,
/// 0, 1) ``; with `Handle::publish_schema` not refusing one, the schema is published: `` called
/// `Result::unwrap_err()` on an `Ok` value: 1 ``.
#[test]
fn no_mac_declares_a_store_generation_until_its_floor_admits() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let a = Registry::at(lab.paths[0].clone());
    a.open().unwrap();
    a.seed().unwrap();
    a.write(vec![address("ann@example.test")]).unwrap();
    repair_commons(&lab, 6);
    let staged = a.roll(true, now_ms()).unwrap();
    assert!(!staged.sealed, "{staged:?}");
    repair_commons(&lab, 6);
    release(&lab, 1);
    repair_commons(&lab, 6);
    assert!(
        !undeclared_by(&lab, 0, 1),
        "control: b declares a generation it judges open"
    );
    let rolled = a.roll(true, now_ms()).unwrap();
    assert!(rolled.sealed, "{rolled:?}");
    assert_eq!(a.generation().generation, 2);
    repair_commons(&lab, 6);

    copying_roll_between(&lab, 0, &att::REGISTRY_2, &att::REGISTRY_3);
    repair_commons(&lab, 6);
    assert!(
        !third_undeclared_by(&lab, 1, 0),
        "control: a's declaration of the third generation reached b"
    );
    assert!(third_undeclared_by(&lab, 1, 1));
    for _ in 0..2 {
        release(&lab, 1);
        repair_commons(&lab, 6);
    }
    assert!(third_undeclared_by(&lab, 0, 1));
    assert!(third_undeclared_by(&lab, 1, 1));
    release_with(&lab, 1, &Memory::default());
    repair_commons(&lab, 6);
    assert!(
        !third_undeclared_by(&lab, 0, 1),
        "a Mac its floor admits declares the third generation"
    );

    let alone = Lab::new(&[Role::Writer]);
    let newest = Registry::at(alone.paths[0].clone());
    newest.open_newest(now_ms()).unwrap();
    assert_eq!(newest.generation().generation, 3);
    let refused = newest.publish_schema().unwrap_err().to_string();
    assert!(refused.contains("schema is not published"), "{refused}");
}

/// Whether Mac `n`'s catalog names the registry generation `compiled` at all: a roll declares it
/// before it writes anything into it.
fn catalog_names(lab: &Lab, n: usize, compiled: &Compiled) -> bool {
    let bootstrap = lab.bootstrap(n);
    let session = Session::open(&bootstrap).unwrap();
    let directory = Directory::new(&lab.paths[n], bootstrap.trust());
    let scope = compiled.descriptor(bootstrap.trust()).unwrap().scope;
    directory
        .snapshot(&session)
        .unwrap()
        .entries
        .contains_key(&scope.digest())
}

/// LT-04B's exit test. No registry secret enters the secret store until every Mac reading it
/// writes no vault entry to a file (the fleet floor, `Store::admit`): a roll into the third
/// generation that the floor holds back fails before it declares anything, so the store holds,
/// and owes, nothing, the catalog does not name the third generation and the registry stays at
/// the second, the password sealed in its cell. Once the floor admits, the same roll moves the
/// password into one store entry and the cell carries only its reference. CONTROL: the roll into
/// the second generation, which holds its secrets sealed, never asks the floor.
/// NEGATIVE CONTROL: with `Registry::roll_with` not asking `secrets.admit()` before it declares,
/// the held-back roll declares the third generation and owes the entry before its put is
/// refused: `` assertion failed: store.owed().is_empty() ``.
#[test]
fn no_registry_value_is_written_until_every_mac_reads_it_in_place() {
    let lab = Lab::new(&[Role::Writer]);
    let a = Registry::at(lab.paths[0].clone());
    a.open().unwrap();
    a.seed().unwrap();
    let pk = keys::address_pk("ann@example.test").unwrap();
    let value = "synthetic-pass-7";
    a.write(vec![address("ann@example.test")]).unwrap();
    let token = sealed_first(&lab, 0, ADDRESS.name, &pk, "password", value);
    write_first_raw(
        &lab,
        0,
        vec![Change {
            collection: ADDRESS.name.to_owned(),
            key: format!("{pk}.password"),
            value: Some(CellValue::owner(ADDRESS.version, json!(token)).encode()),
        }],
    );
    let store = Memory::default();
    store.floor(|| {
        Err(Error::new(
            "a Mac reading the vault still writes secret files",
        ))
    });
    let mut rolled = a.roll_with(true, now_ms(), &store).unwrap();
    while a.generation().generation < 2 {
        rolled = a.roll_with(true, now_ms(), &store).unwrap();
    }
    assert!(rolled.sealed, "control: {rolled:?}");
    assert!(
        store.every().is_empty(),
        "control: nothing put below the third"
    );

    let refused = a.roll_with(true, now_ms(), &store).unwrap_err().to_string();
    assert!(refused.contains("still writes secret files"), "{refused}");
    assert!(!refused.contains(value), "{refused}");
    assert!(store.every().is_empty() && store.names().is_empty());
    assert!(store.owed().is_empty());
    assert!(!catalog_names(&lab, 0, &att::REGISTRY_3));
    assert_eq!(a.generation().generation, 2);

    store.floor(|| Ok(()));
    let moved = a.roll_with(true, now_ms(), &store).unwrap();
    assert!(catalog_names(&lab, 0, &att::REGISTRY_3), "{moved:?}");
    assert_eq!(store.every(), vec![value.to_owned()]);
    assert_eq!(store.names().len(), 1);
    let entry = &store.names()[0];
    assert_eq!(store.held(entry).as_deref(), Some(value));
    assert!(store.owed().is_empty(), "the roll paid what it put");
    let text: String = records_of(&lab, 0, &att::REGISTRY_3)
        .into_iter()
        .map(|(_, text)| text)
        .collect();
    assert!(text.contains(entry.as_str()) && !text.contains(value) && !text.contains("sec1:"));
}

/// `aterm db emails` on Mac `n`: the owner's count line on the registry's secrets.
fn secrets_line(lab: &Lab, n: usize, store: &Memory) -> String {
    let mut read = || -> Result<zeroize::Zeroizing<Vec<u8>>> { panic!("a view reads no stdin") };
    let mut open = |_: &std::path::Path, _: usize| -> Result<()> { panic!("no grid opens") };
    let mut io = crate::alab::command::views::ViewIo {
        open: &mut open,
        read_stdin: &mut read,
        store,
        holding: &|| Ok(usize::from(store.admit().is_err())),
        now_ms: now_ms(),
    };
    let value = crate::alab::command::views::run(
        &lab.paths[n],
        crate::alab::command::views::Noun::Emails,
        &[],
        &mut io,
    )
    .unwrap();
    value["secrets"].as_str().unwrap_or_default().to_owned()
}

/// `aterm db emails` carries one owner-readable line with counts only: how many secret cells
/// name a store entry, how many the table still holds sealed, and how many Macs hold the move
/// back. A sealed password at the second generation behind a refusing floor counts as sealed
/// and held; once the roll moves it, as one reference with nothing held. No value shows.
/// NEGATIVE CONTROL: with `secret_cells` counting a sealed candidate as a reference, the line
/// before the roll says `1 cell names`: `` left: "secrets: 1 cell names … ``.
#[test]
fn the_owner_reads_one_line_counting_secret_references_sealed_cells_and_holding_macs() {
    let lab = Lab::new(&[Role::Writer]);
    let a = Registry::at(lab.paths[0].clone());
    a.open().unwrap();
    a.seed().unwrap();
    let pk = keys::address_pk("ann@example.test").unwrap();
    let value = "synthetic-pass-8";
    a.write(vec![address("ann@example.test")]).unwrap();
    let token = sealed_first(&lab, 0, ADDRESS.name, &pk, "password", value);
    write_first_raw(
        &lab,
        0,
        vec![Change {
            collection: ADDRESS.name.to_owned(),
            key: format!("{pk}.password"),
            value: Some(CellValue::owner(ADDRESS.version, json!(token)).encode()),
        }],
    );
    let store = Memory::default();
    store.floor(|| Err(Error::new("held")));
    while a.generation().generation < 2 {
        a.roll_with(true, now_ms(), &store).unwrap();
    }
    let before = secrets_line(&lab, 0, &store);
    assert_eq!(
        before,
        "secrets: 0 cells name a secret-store entry, 1 cell still sealed in the table; \
         1 Mac reading the vault holds the move into the secret store back"
    );
    store.floor(|| Ok(()));
    a.roll_with(true, now_ms(), &store).unwrap();
    let after = secrets_line(&lab, 0, &store);
    assert_eq!(
        after,
        "secrets: 1 cell names a secret-store entry, 0 cells still sealed in the table; \
         no Mac holds the move into the secret store back"
    );
    assert!(!before.contains(value) && !after.contains(value));
}

/// The catalog state Mac `n` gives the registry generation `compiled`, if it names it.
fn catalog_state(lab: &Lab, n: usize, compiled: &Compiled) -> Option<crate::catalog::State> {
    let bootstrap = lab.bootstrap(n);
    let session = Session::open(&bootstrap).unwrap();
    let directory = Directory::new(&lab.paths[n], bootstrap.trust());
    let scope = compiled.descriptor(bootstrap.trust()).unwrap().scope;
    directory
        .snapshot(&session)
        .unwrap()
        .entries
        .get(&scope.digest())
        .map(|entry| entry.state)
}

/// Whether Mac `n` stores the registry generation `compiled`.
fn stores(lab: &Lab, n: usize, compiled: &Compiled) -> bool {
    let trust = lab.bootstrap(n).trust();
    Directory::new(&lab.paths[n], trust)
        .path(compiled.descriptor(trust).unwrap().scope)
        .unwrap()
        .exists()
}

/// The move ends the sealed history's life, behind the same floor: the third generation is a
/// table scope of its own, a fresh audience key no earlier key derives; once the floor admits,
/// the roll that moves the password seals the second generation (its declarations conflict, so
/// no Mac subscribes to it, repairs it or reserves storage for it, and a Mac admitted later never
/// receives one of its sealed tokens) and releases this Mac's storage of it, the only place the
/// token was. While the floor holds the move back, nothing of that happens: the second
/// generation stays subscribed and stored. A Mac that already read the second generation keeps
/// what it read (docs/DESIGN-alab-one-mesh-2026-11-05.md §6, "The honest limit").
/// NEGATIVE CONTROL: with `Registry::roll_with` skipping the seal when it moved a value, the
/// moving roll reports nothing sealed and the second generation stays subscribed and stored:
/// `` assertion failed: moved.sealed && moved.waiting.is_empty() ``.
#[test]
fn the_move_seals_the_sealed_generation_and_releases_its_storage_behind_the_same_floor() {
    let lab = Lab::new(&[Role::Writer]);
    let a = Registry::at(lab.paths[0].clone());
    a.open().unwrap();
    a.seed().unwrap();
    let pk = keys::address_pk("ann@example.test").unwrap();
    let value = "synthetic-pass-9";
    a.write(vec![address("ann@example.test")]).unwrap();
    let token = sealed_first(&lab, 0, ADDRESS.name, &pk, "password", value);
    write_first_raw(
        &lab,
        0,
        vec![Change {
            collection: ADDRESS.name.to_owned(),
            key: format!("{pk}.password"),
            value: Some(CellValue::owner(ADDRESS.version, json!(token)).encode()),
        }],
    );
    let store = Memory::default();
    store.floor(|| Err(Error::new("held")));
    while a.generation().generation < 2 {
        a.roll_with(true, now_ms(), &store).unwrap();
    }
    let trust = lab.bootstrap(0).trust();
    let second = att::REGISTRY_2.descriptor(trust).unwrap().scope;
    let third = att::REGISTRY_3.descriptor(trust).unwrap().scope;
    assert_ne!(second.digest(), third.digest(), "a fresh table scope");

    assert!(a.roll_with(true, now_ms(), &store).is_err());
    release_with(&lab, 0, &store);
    assert_eq!(
        catalog_state(&lab, 0, &att::REGISTRY_2),
        Some(crate::catalog::State::Subscribed)
    );
    assert!(stores(&lab, 0, &att::REGISTRY_2), "held: the token stays");

    store.floor(|| Ok(()));
    let moved = a.roll_with(true, now_ms(), &store).unwrap();
    assert!(moved.sealed && moved.waiting.is_empty(), "{moved:?}");
    assert_eq!(a.generation().generation, 3);
    assert_eq!(
        catalog_state(&lab, 0, &att::REGISTRY_2),
        Some(crate::catalog::State::Conflict)
    );
    assert_eq!(moved.released.released, vec![second.digest()], "{moved:?}");
    assert!(!stores(&lab, 0, &att::REGISTRY_2));
    let text: String = records_of(&lab, 0, &att::REGISTRY_3)
        .into_iter()
        .map(|(_, text)| text)
        .collect();
    assert!(!text.contains(value) && !text.contains("sec1:"));
}

/// A moved value the store took but does not hold yet (a vault write signed on this Mac and not
/// posted) stops the roll before any record names it: the third generation holds no reference,
/// the second stays the registry with the password sealed in it, and the entry stays owed for the
/// sweep. Once the store holds what it takes, the same roll moves it.
/// NEGATIVE CONTROL: with `Registry::roll_with` not reading each moved entry back, the pending
/// roll succeeds: `` assertion failed: refused.is_err() ``.
#[test]
fn no_reference_is_written_before_its_value_reads_back_from_the_store() {
    let lab = Lab::new(&[Role::Writer]);
    let a = Registry::at(lab.paths[0].clone());
    a.open().unwrap();
    a.seed().unwrap();
    let pk = keys::address_pk("ann@example.test").unwrap();
    let value = "synthetic-pass-10";
    a.write(vec![address("ann@example.test")]).unwrap();
    let token = sealed_first(&lab, 0, ADDRESS.name, &pk, "password", value);
    write_first_raw(
        &lab,
        0,
        vec![Change {
            collection: ADDRESS.name.to_owned(),
            key: format!("{pk}.password"),
            value: Some(CellValue::owner(ADDRESS.version, json!(token)).encode()),
        }],
    );
    let pending = Memory::default();
    pending.floor(|| Ok(()));
    pending.pend();
    while a.generation().generation < 2 {
        a.roll_with(true, now_ms(), &pending).unwrap();
    }
    let refused = a.roll_with(true, now_ms(), &pending);
    assert!(refused.is_err());
    let error = refused.unwrap_err().to_string();
    assert!(
        error.contains("does not hold") && !error.contains(value),
        "{error}"
    );
    assert_eq!(a.generation().generation, 2);
    let text: String = records_of(&lab, 0, &att::REGISTRY_3)
        .into_iter()
        .map(|(_, text)| text)
        .collect();
    assert!(
        !text.contains("alab-address-password"),
        "no reference written"
    );
    assert_eq!(pending.owed().len(), 1, "the entry stays owed");
    assert!(stores(&lab, 0, &att::REGISTRY_2));

    let holding = Memory::default();
    holding.floor(|| Ok(()));
    a.roll_with(true, now_ms(), &holding).unwrap();
    assert_eq!(a.generation().generation, 3);
    assert_eq!(holding.every(), vec![value.to_owned()]);
}
