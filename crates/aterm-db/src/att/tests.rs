// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The generic table verbs and the grid round trip on real Commons labs: isolated scratch homes,
//! no live vault, fleet or Commons. Grid edits are made exactly as amacs's grid commits them
//! (`typed::grid::tests::type_in`), and the editor is a scripted stand-in, so no window opens.
//! Every address is synthetic (`example.invalid`).

use super::{
    command::{self, Output},
    *,
};
use crate::{
    alab::{
        Registry,
        registry::{ADDRESS, MACHINE},
    },
    table::tests::{Lab, repair_commons_first},
    typed::{
        grid::{
            self as sheet,
            tests::{MC, add_row, machine, put, record, type_in},
        },
        keys,
    },
};
use aterm_json::json;
use aterm_secrets::commons::roster::Role;
use std::path::{Path, PathBuf};

fn never_open(_: &Path, _: usize, _: &[String]) -> Result<()> {
    panic!("this verb opens no editor")
}

fn run(paths: &Paths, words: &[&str]) -> Output {
    let words: Vec<String> = words.iter().map(|w| (*w).to_owned()).collect();
    command::run(paths, &words, &mut never_open, &mut |_| {}).unwrap()
}

fn text(output: &Output) -> String {
    match output {
        Output::Json(value) => aterm_json::to_string(value).unwrap(),
        Output::Text(text) => text.clone(),
    }
}

/// The registry's `machine` export on `paths`.
fn export_machines(paths: &Paths) -> sheet::Export {
    let handle = Handle::at(paths.clone(), &REGISTRY);
    sheet::export(
        &registry::REGISTRY,
        &MACHINE,
        &handle.view().unwrap().cells,
        &[],
        false,
    )
    .unwrap()
}

fn no_birth(_: &Collection, _: &BTreeMap<String, Value>) -> Result<String> {
    Err(Error::new("no births here"))
}

/// What a push does when Mac 0 first writes the registry: the catalog, which tells Mac 1 of the
/// table, then the table, each by name.
fn repair_new_registry(lab: &Lab) {
    let catalog = crate::catalog::scope(lab.bootstrap(0).trust()).digest();
    repair_commons_first(lab, 1, catalog);
    repair_registry(lab, 1);
}

/// `rounds` rounds of repair naming the registry's table, as a push names the table that moved. The
/// tests here assert what that one table holds after a repair, not how the repair reached it, and
/// a push moves records with the same exchange and admissions as a scan's lap.
/// GAP: these tests see no regular-scan table choice and no idle laps, so a defect only when the
/// registry is reached by the scan's rotation is not seen here;
/// `alab::roll::tests::a_roll_restates_everything_and_releases_the_sealed_generation` and
/// `replica::tests`' table scan keep those.
fn repair_registry(lab: &Lab, rounds: usize) {
    let table = REGISTRY.descriptor(lab.bootstrap(0).trust()).unwrap().scope;
    repair_commons_first(lab, rounds, table.digest());
}

/// A grid save on one Mac never overwrites what another Mac wrote after the export: of the
/// owner's two edits, the one to a cell a peer changed meanwhile is held back as stale and the
/// other is written, in one owner record that spends none of the per-Mac quota.
/// NEGATIVE CONTROL: with `settle` writing every edit whatever its register holds now (the
/// candidate comparison removed), the save writes both cells, the owner's `status` over the
/// peer's: `` left: (1, 2) right: (1, 1) ``.
#[test]
fn a_grid_save_never_overwrites_a_peer_edit_made_after_the_export() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let owner = Registry::at(lab.paths[0].clone());
    owner.open().unwrap();
    owner.write(vec![machine(MC)]).unwrap();
    repair_new_registry(&lab);
    let exported = export_machines(&lab.paths[0]);
    Registry::at(lab.paths[1].clone())
        .write(vec![vec![put(&MACHINE, MC, "status", json!("parked"))]])
        .unwrap();
    repair_registry(&lab, 1);
    let row = record(&exported.text, MC);
    let saved = type_in(
        &type_in(&exported.text, row, "status", "lost"),
        row,
        "location",
        "rack b",
    );
    let plan = sheet::plan(
        &registry::REGISTRY,
        &MACHINE,
        &exported.base,
        &saved,
        &no_birth,
    )
    .unwrap();
    let handle = Handle::at(lab.paths[0].clone(), &REGISTRY);
    let applied = handle.apply(&MACHINE, &plan).unwrap();
    assert_eq!((applied.records, applied.cells), (1, 1));
    assert_eq!(applied.stale.len(), 1);
    assert_eq!(applied.stale[0].column, "status");
    let view = handle.view().unwrap();
    let rows = view.project(&MACHINE).rows;
    assert_eq!(rows[MC].get("status"), Some(&json!("parked")));
    assert_eq!(rows[MC].get("location"), Some(&json!("rack b")));
    assert!(view.quota.over.is_empty());
    assert!(view.quota.spent.is_empty(), "{:?}", view.quota.spent);
}

/// A peer's record this build refuses (a typed write that stops a cell) never holds a grid save
/// back: it can never project here, so the history still reads complete, `check` lists it by
/// operation, and the owner's save is written.
/// NEGATIVE CONTROL: with `View::read` taking `complete` from `Snapshot::history_complete` (as
/// before), every save on every Mac refuses for the table's life: `` called `Result::unwrap()`
/// on an `Err` value: Error("this table's history here is incomplete; a grid save waits for a
/// complete scan") ``.
#[test]
fn a_peers_refused_record_never_holds_a_grid_save_back() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let owner = Registry::at(lab.paths[0].clone());
    owner.open().unwrap();
    owner.write(vec![machine(MC)]).unwrap();
    repair_new_registry(&lab);
    // A write that stops a cell: `att::decode` refuses it on every Mac.
    Registry::at(lab.paths[1].clone())
        .write(vec![vec![crate::database::Change {
            collection: MACHINE.name.to_owned(),
            key: format!("{MC}.note"),
            value: None,
        }]])
        .unwrap();
    repair_registry(&lab, 1);
    let handle = Handle::at(lab.paths[0].clone(), &REGISTRY);
    let view = handle.view().unwrap();
    assert_eq!(view.refused.len(), 1);
    let checked = text(&run(&lab.paths[0], &["check", "alab/registry"]));
    assert!(
        checked.contains(&aterm_codec::hex::encode_lower(&view.refused[0])),
        "{checked}"
    );
    let exported = export_machines(&lab.paths[0]);
    let row = record(&exported.text, MC);
    let saved = type_in(&exported.text, row, "location", "rack b");
    let plan = sheet::plan(
        &registry::REGISTRY,
        &MACHINE,
        &exported.base,
        &saved,
        &no_birth,
    )
    .unwrap();
    let applied = handle.apply(&MACHINE, &plan).unwrap();
    assert_eq!((applied.records, applied.cells), (1, 1));
    let rows = handle.view().unwrap().project(&MACHINE).rows;
    assert_eq!(rows[MC].get("location"), Some(&json!("rack b")));
}

/// An edit session writes the collection to a private scratch file, reopens it on the first bad
/// cell with nothing written and the problem named by row and column (never the value), writes
/// the corrected save, and removes its scratch folder when it ends; a reopened file closed
/// unchanged ends the session with nothing of it written.
/// NEGATIVE CONTROL: with the session keeping its scratch folder (`scratch.keep()` in place of
/// `drop(scratch)`), the folder outlives it: `` assertion failed: !folder.exists() ``.
#[test]
fn an_edit_session_reopens_on_a_bad_cell_then_writes_and_leaves_no_scratch() {
    let lab = Lab::new(&[Role::Writer]);
    let owner = Registry::at(lab.paths[0].clone());
    owner.open().unwrap();
    owner.write(vec![machine(MC)]).unwrap();
    let handle = Handle::at(lab.paths[0].clone(), &REGISTRY);
    let mut opened: Vec<(PathBuf, usize)> = Vec::new();
    let mut reports = Vec::new();
    let edited = {
        let mut open = |path: &Path, line: usize, _: &[String]| -> Result<()> {
            opened.push((path.to_owned(), line));
            let held = std::fs::read_to_string(path).unwrap();
            let row = record(&held, MC);
            let value = if opened.len() == 1 {
                "sideways"
            } else {
                "parked"
            };
            std::fs::write(path, type_in(&held, row, "status", value)).unwrap();
            Ok(())
        };
        edit::edit(
            &handle,
            &MACHINE,
            &[],
            false,
            None,
            &mut open,
            &mut |line| {
                reports.push(line.to_owned());
            },
        )
        .unwrap()
    };
    assert_eq!(edited.rounds, 2);
    assert_eq!(opened[0].1, 1);
    assert_eq!(opened[1].1, 2);
    assert!(opened[0].0.ends_with("machine.tsv"));
    assert_eq!(reports.len(), 1);
    assert!(
        reports[0].starts_with("row 1, column status: machine.status"),
        "{reports:?}"
    );
    assert!(!reports[0].contains("sideways"));
    assert_eq!(edited.applied.len(), 1);
    assert_eq!(edited.applied[0].cells, 1);
    let folder = opened[0].0.parent().unwrap().to_owned();
    assert!(!folder.exists());
    assert_eq!(
        handle.view().unwrap().project(&MACHINE).rows[MC].get("status"),
        Some(&json!("parked"))
    );
    // A bad save closed unchanged on its reopen is abandoned: nothing written.
    let mut rounds = 0;
    let abandoned = edit::edit(
        &handle,
        &MACHINE,
        &[],
        false,
        None,
        &mut |path: &Path, _, _: &[String]| {
            rounds += 1;
            if rounds == 1 {
                let held = std::fs::read_to_string(path).unwrap();
                let row = record(&held, MC);
                std::fs::write(path, type_in(&held, row, "ram_gb", "lots")).unwrap();
            }
            Ok(())
        },
        &mut |_| {},
    )
    .unwrap();
    assert!(abandoned.abandoned && abandoned.applied.is_empty());
    assert_eq!(
        handle.view().unwrap().project(&MACHINE).rows[MC].get("ram_gb"),
        None
    );
}

/// No table verb shows an address unless asked (design §3.3, §8): scan as JSON and as TSV, get,
/// history, check, ls, schema and an exported file with its baseline all mask it, and
/// `--show-addresses` shows it.
/// NEGATIVE CONTROL: with history rendering values unmasked (`change_json` passing `true`), the
/// history output holds the address and the check fails naming it: `` history:
/// {"operations":[…"value":"ann.lee@example.invalid"…`` (a synthetic address).
#[test]
fn no_table_verb_shows_an_address_unless_asked() {
    let lab = Lab::new(&[Role::Writer]);
    let owner = Registry::at(lab.paths[0].clone());
    owner.open().unwrap();
    let pk = keys::address_pk("ann.lee@example.invalid").unwrap();
    owner
        .write(vec![vec![
            put(&ADDRESS, &pk, "addr", json!("ann.lee@example.invalid")),
            put(&ADDRESS, &pk, "domain", json!("example.invalid")),
            put(&ADDRESS, &pk, "origin", json!("owner")),
            put(&ADDRESS, &pk, "row", json!("live")),
        ]])
        .unwrap();
    let scratch = aterm_tempfile::tempdir().unwrap();
    let file = scratch.path().join("address.tsv");
    let file_word = file.to_str().unwrap();
    let paths = &lab.paths[0];
    let outputs = [
        (
            "scan json",
            run(paths, &["scan", "alab/registry", "address", "--json"]),
        ),
        (
            "scan tsv",
            run(paths, &["scan", "alab/registry", "address", "--tsv"]),
        ),
        ("get", run(paths, &["get", "alab/registry", "address", &pk])),
        (
            "history",
            run(paths, &["history", "alab/registry", "address"]),
        ),
        ("check", run(paths, &["check", "alab/registry"])),
        ("ls", run(paths, &["ls"])),
        ("schema", run(paths, &["schema", "alab/registry/address"])),
        (
            "export",
            run(
                paths,
                &["export", "alab/registry", "address", "--tsv", file_word],
            ),
        ),
    ];
    let mut seen = outputs
        .map(|(verb, output)| (verb.to_owned(), text(&output)))
        .to_vec();
    seen.push(("file".into(), std::fs::read_to_string(&file).unwrap()));
    seen.push((
        "baseline".into(),
        std::fs::read_to_string(command::baseline(&file)).unwrap(),
    ));
    for (verb, shown) in &seen {
        assert!(!shown.contains("ann.lee"), "{verb}: {shown}");
        assert!(!shown.contains("@example.invalid"), "{verb}");
    }
    assert!(seen[0].1.contains("an…@…"));
    assert!(seen[3].1.contains("an…@…"), "{}", seen[3].1);
    let shown = text(&run(
        paths,
        &[
            "scan",
            "alab/registry",
            "address",
            "--json",
            "--show-addresses",
        ],
    ));
    assert!(shown.contains("ann.lee@example.invalid"));
}

/// A value another writer's build stored though this one refuses it never shows as stored: a
/// note holding a token or a phone number reads `REFUSED` on every verb, even with
/// `--show-addresses`, and a note holding an address is masked unless asked, history included.
/// NEGATIVE CONTROLS: with `grid::show` skipping its `admissible` check, history prints the
/// stored token: `` history: {"operations":[{"authored_ms":…`` (the token assertion); with
/// `raw_masked` never consulting `refused`, scan prints it: `` scan json:
/// {"collection":"machine",…"note":"ghp_cccc…"…``.
#[test]
fn no_table_verb_shows_a_refused_value_another_build_stored() {
    // In halves, so the export guard never reads a token here.
    const TOKEN: &str = concat!("ghp", "_cccccccccccccccccccccccc");
    let lab = Lab::new(&[Role::Writer]);
    let owner = Registry::at(lab.paths[0].clone());
    owner.open().unwrap();
    // Stored as an older or modified build would: no guard ran.
    let unchecked = |pk: &str, text: &str| crate::database::Change {
        collection: MACHINE.name.to_owned(),
        key: format!("{pk}.note"),
        value: Some(crate::typed::cell::CellValue::owner(1, json!(text)).encode()),
    };
    let (token, phone, address) = (
        "mc-0000000000000001",
        "mc-0000000000000002",
        "mc-0000000000000003",
    );
    let mut rows = vec![machine(token), machine(phone), machine(address)];
    rows[0].push(unchecked(token, TOKEN));
    rows[1].push(unchecked(phone, "codes to +1 415 555 0101"));
    rows[2].push(unchecked(address, "reach ann.lee@example.invalid"));
    owner.write(rows).unwrap();
    let scratch = aterm_tempfile::tempdir().unwrap();
    let file = scratch.path().join("machine.tsv");
    let paths = &lab.paths[0];
    for reveal in [false, true] {
        let words = |verb: &[&str]| {
            let mut list = verb.to_vec();
            if reveal {
                list.push("--show-addresses");
            }
            text(&run(paths, &list))
        };
        let seen = [
            (
                "scan json",
                words(&["scan", "alab/registry", "machine", "--json"]),
            ),
            (
                "scan tsv",
                words(&["scan", "alab/registry", "machine", "--tsv"]),
            ),
            ("get", words(&["get", "alab/registry", "machine", address])),
            ("history", words(&["history", "alab/registry", "machine"])),
            (
                "export",
                words(&[
                    "export",
                    "alab/registry",
                    "machine",
                    "--tsv",
                    file.to_str().unwrap(),
                ]),
            ),
            ("file", std::fs::read_to_string(&file).unwrap()),
        ];
        for (verb, shown) in &seen {
            assert!(!shown.contains(TOKEN), "{verb}: {shown}");
            assert!(!shown.contains("555 0101"), "{verb}: {shown}");
            // `export` reports only where it wrote; `file` is what it wrote.
            if *verb != "export" {
                assert_eq!(shown.contains("ann.lee"), reveal, "{verb}: {shown}");
            }
        }
        assert!(seen[0].1.contains(sheet::REFUSED), "{}", seen[0].1);
        assert!(seen[3].1.contains(sheet::REFUSED), "{}", seen[3].1);
        let get = words(&["get", "alab/registry", "machine", token]);
        assert!(
            get.contains(sheet::REFUSED) && !get.contains(TOKEN),
            "{get}"
        );
    }
}

/// `export` and `apply` round trip a file: the owner's edit is written, a cell a peer changed
/// meanwhile is reported and the file rewritten from the table as it is now with that cell
/// showing `⚠ mine ‖ theirs`, so applying the rewritten file unchanged writes nothing more.
/// NEGATIVE CONTROL: with `apply` rewriting the file from the fresh export without
/// `grid::reopen`, the stale cell loses the owner's value: `` assertion failed:
/// rewritten.contains("⚠ lost ‖ parked") ``.
#[test]
fn export_and_apply_round_trip_a_file_and_show_a_stale_cell() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let owner = Registry::at(lab.paths[0].clone());
    owner.open().unwrap();
    owner.write(vec![machine(MC)]).unwrap();
    repair_new_registry(&lab);
    let scratch = aterm_tempfile::tempdir().unwrap();
    let file = scratch.path().join("machine.tsv");
    let word = file.to_str().unwrap();
    run(
        &lab.paths[0],
        &["export", "alab/registry", "machine", "--tsv", word],
    );
    Registry::at(lab.paths[1].clone())
        .write(vec![vec![put(&MACHINE, MC, "status", json!("parked"))]])
        .unwrap();
    repair_registry(&lab, 1);
    let held = std::fs::read_to_string(&file).unwrap();
    let row = record(&held, MC);
    let saved = type_in(
        &type_in(&held, row, "status", "lost"),
        row,
        "location",
        "rack b",
    );
    std::fs::write(&file, &saved).unwrap();
    let Output::Json(applied) = run(&lab.paths[0], &["apply", word]) else {
        panic!("apply prints JSON");
    };
    assert_eq!(applied["written"], json!(1));
    assert_eq!(applied["stale"], json!([{"pk": MC, "column": "status"}]));
    let rewritten = std::fs::read_to_string(&file).unwrap();
    assert!(rewritten.contains("⚠ lost ‖ parked"));
    assert!(rewritten.contains("\track b\t"));
    let Output::Json(again) = run(&lab.paths[0], &["apply", word]) else {
        panic!("apply prints JSON");
    };
    assert_eq!(again["written"], json!(0));
}

/// `revert` appends the exact compensation of one grid save, restoring the cell it changed.
/// NEGATIVE CONTROL: with `Handle::revert` returning the target without appending its
/// compensation, the cell keeps the save's value: `` left: Some(String("rack b")) right:
/// Some(String("rack a")) ``.
#[test]
fn revert_restores_what_one_save_changed() {
    let lab = Lab::new(&[Role::Writer]);
    let owner = Registry::at(lab.paths[0].clone());
    owner.open().unwrap();
    owner.write(vec![machine(MC)]).unwrap();
    let exported = export_machines(&lab.paths[0]);
    let saved = type_in(
        &exported.text,
        record(&exported.text, MC),
        "location",
        "rack b",
    );
    let plan = sheet::plan(
        &registry::REGISTRY,
        &MACHINE,
        &exported.base,
        &saved,
        &no_birth,
    )
    .unwrap();
    let handle = Handle::at(lab.paths[0].clone(), &REGISTRY);
    let applied = handle.apply(&MACHINE, &plan).unwrap();
    let operation = aterm_codec::hex::encode_lower(&applied.operations[0]);
    let Output::Json(reverted) = run(&lab.paths[0], &["revert", "alab/registry", &operation])
    else {
        panic!("revert prints JSON");
    };
    assert_eq!(
        handle.view().unwrap().project(&MACHINE).rows[MC].get("location"),
        Some(&json!("rack a"))
    );
    assert_ne!(reverted["operation"], json!(operation));
}

/// Two replicas hold an owner edit and one excess automatic edit of the same cell. The
/// excess edit arrives either before or after the owner edit, but is inert in both views.
fn quota_revert_pair(owner_first: bool) -> (Lab, Handle, Digest, Digest) {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let owner = Registry::at(lab.paths[0].clone());
    owner.open().unwrap();
    owner.write(vec![machine(MC)]).unwrap();
    let change = vec![put(&MACHINE, MC, "location", json!("rack b"))];
    if owner_first {
        owner.write(vec![change.clone()]).unwrap();
    }
    repair_new_registry(&lab);
    let automatic = |column: &str, value: Value| {
        cell::change(
            &MACHINE,
            cell::CellKey {
                pk: MC,
                col: column,
                elem: None,
            },
            &cell::CellValue::automatic(1, value),
        )
        .unwrap()
    };
    let mut records: Vec<_> = (0..registry::SELF_QUOTA)
        .map(|n| vec![automatic("note", json!(format!("observation {n}")))])
        .collect();
    records.push(vec![automatic("location", json!("over quota"))]);
    Registry::at(lab.paths[1].clone()).write(records).unwrap();
    // `SELF_QUOTA + 1` records at `BATCH` a round.
    repair_registry(&lab, 2);
    if !owner_first {
        owner.write(vec![change.clone()]).unwrap();
        repair_registry(&lab, 1);
    }
    let handle = Handle::at(lab.paths[0].clone(), &REGISTRY);
    let view = handle.view().unwrap();
    assert!(view.complete);
    assert_eq!(view.quota.over.len(), 1);
    assert_eq!(
        view.project(&MACHINE).rows[MC].get("location"),
        Some(&json!("rack b"))
    );
    let target = view.ops.iter().find(|op| op.changes == change).unwrap().id;
    let excess = *view.quota.over.first().unwrap();
    (lab, handle, target, excess)
}

fn assert_quota_revert(owner_first: bool) {
    let (lab, handle, target, excess) = quota_revert_pair(owner_first);
    let before = handle.view().unwrap().ops.len();
    let undo = handle.revert([81; 16], target).unwrap();
    assert_eq!(handle.revert([81; 16], target).unwrap(), undo);
    let view = handle.view().unwrap();
    assert_eq!(view.ops.len(), before + 1);
    assert_eq!(
        view.project(&MACHINE).rows[MC].get("location"),
        Some(&json!("rack a"))
    );
    repair_registry(&lab, 1);
    let peer = Handle::at(lab.paths[1].clone(), &REGISTRY).view().unwrap();
    assert_eq!(view.cells, peer.cells);
    assert!(peer.quota.over.contains(&excess));
    assert!(peer.ops.iter().any(|op| op.id == excess));
}

/// A later automatic record past the peer's quota cannot block undoing the owner edit
/// which both replicas still show as the cell's only candidate.
/// NEGATIVE CONTROL: raw compensation refuses it as an affected proposal that changed.
#[test]
fn typed_revert_ignores_later_over_quota_edits() {
    assert_quota_revert(true);
}

/// Compensation restores the same preimage the typed reader saw, not an inert automatic
/// value retained before the owner edit. Both replicas retain that excess record as history.
/// NEGATIVE CONTROL: raw compensation restores `over quota` instead of `rack a`.
#[test]
fn typed_revert_restores_the_preimage_without_over_quota_edits() {
    assert_quota_revert(false);
}

/// An automatic record which changed no visible cell is not an eligible revert target.
/// NEGATIVE CONTROL: raw compensation accepts the excess operation and appends a record.
#[test]
fn typed_revert_refuses_an_over_quota_target_without_appending() {
    let (lab, handle, _, excess) = quota_revert_pair(true);
    let before = handle.view().unwrap();
    assert!(handle.revert([82; 16], excess).is_err());
    let after = handle.view().unwrap();
    assert_eq!(before.ops, after.ops);
    assert_eq!(before.cells, after.cells);
    let peer = Handle::at(lab.paths[1].clone(), &REGISTRY).view().unwrap();
    assert_eq!(after.cells, peer.cells);
}

/// A revert is the owner's verb, never an automatic record, even where it restores a value an
/// automatic path wrote: once this Mac has spent its whole quota, reverting an owner edit that
/// sat over a birth value still restores the birth value.
/// NEGATIVE CONTROL: with `owned::automatic` ignoring `Op::revert`, the revert copies the
/// birth's `"a": true`, lands past the quota as this Mac's 25th automatic record and never
/// projects while `revert` reports success: `` left: Some(String("rack b")) right:
/// Some(String("rack a")) ``.
#[test]
fn a_revert_over_an_automatic_value_spends_no_quota() {
    let lab = Lab::new(&[Role::Writer]);
    let owner = Registry::at(lab.paths[0].clone());
    owner.open().unwrap();
    let born = |pk: &str, col: &str, x: Value| {
        cell::change(
            &MACHINE,
            cell::CellKey {
                pk,
                col,
                elem: None,
            },
            &cell::CellValue::automatic(1, x),
        )
        .unwrap()
    };
    // One birth record, then the rest of this Mac's quota spent on other births.
    let mut records = vec![vec![
        born(MC, "label", json!("m9")),
        born(MC, "status", json!("active")),
        born(MC, "location", json!("rack a")),
        born(MC, "row", json!("live")),
    ]];
    for n in 1..registry::SELF_QUOTA {
        let pk = format!("mc-1{n:015}");
        records.push(vec![born(&pk, "label", json!(format!("m{}", 100 + n)))]);
    }
    owner.write(records).unwrap();
    let exported = export_machines(&lab.paths[0]);
    let saved = type_in(
        &exported.text,
        record(&exported.text, MC),
        "location",
        "rack b",
    );
    let plan = sheet::plan(
        &registry::REGISTRY,
        &MACHINE,
        &exported.base,
        &saved,
        &no_birth,
    )
    .unwrap();
    let handle = Handle::at(lab.paths[0].clone(), &REGISTRY);
    let applied = handle.apply(&MACHINE, &plan).unwrap();
    let view = handle.view().unwrap();
    let me = view.ops.last().unwrap().actor;
    assert_eq!(view.quota.spent.get(&me), Some(&registry::SELF_QUOTA));
    assert_eq!(
        view.project(&MACHINE).rows[MC].get("location"),
        Some(&json!("rack b"))
    );
    let operation = aterm_codec::hex::encode_lower(&applied.operations[0]);
    run(&lab.paths[0], &["revert", "alab/registry", &operation]);
    let view = handle.view().unwrap();
    assert_eq!(
        view.project(&MACHINE).rows[MC].get("location"),
        Some(&json!("rack a"))
    );
    assert!(view.quota.over.is_empty());
}

/// A new grid row whose derived key the table already holds is refused at its pk cell, even
/// when a `--where` export left that row out of the file, and nothing is written into the row
/// the owner never saw.
/// NEGATIVE CONTROL: with `Compiled::born` passing a held key through, the save reaches the
/// store and only `settle`'s all-or-nothing rule keeps the hidden row whole, reporting its
/// cells stale instead of refusing the new row: `` assertion failed: applied["problems"]
/// .is_array() ``.
#[test]
fn a_new_row_that_is_a_hidden_row_is_refused() {
    let lab = Lab::new(&[Role::Writer]);
    let owner = Registry::at(lab.paths[0].clone());
    owner.open().unwrap();
    let pk = keys::machine_pk("m7");
    owner
        .write(vec![vec![
            put(&MACHINE, &pk, "label", json!("m7")),
            put(&MACHINE, &pk, "status", json!("active")),
            put(&MACHINE, &pk, "row", json!("live")),
        ]])
        .unwrap();
    let scratch = aterm_tempfile::tempdir().unwrap();
    let file = scratch.path().join("machine.tsv");
    let word = file.to_str().unwrap();
    run(
        &lab.paths[0],
        &[
            "export",
            "alab/registry",
            "machine",
            "--tsv",
            word,
            "--where",
            "status=parked",
        ],
    );
    let held = std::fs::read_to_string(&file).unwrap();
    let mut text = add_row(&held, 0);
    for (name, value) in [
        ("label", "m7"),
        ("status", "parked"),
        ("location", "rack z"),
    ] {
        text = type_in(&text, 1, name, value);
    }
    std::fs::write(&file, &text).unwrap();
    let Output::Json(applied) = run(&lab.paths[0], &["apply", word]) else {
        panic!("apply prints JSON");
    };
    assert!(applied["problems"].is_array());
    assert_eq!(applied["written"], json!(0));
    let handle = Handle::at(lab.paths[0].clone(), &REGISTRY);
    let row = handle.view().unwrap().project(&MACHINE).rows[&pk].clone();
    assert_eq!(row.get("status"), Some(&json!("active")));
    assert_eq!(row.get("location"), None);
}

/// `check` names a writer whose automatic quota is spent and asks for a roll: its later
/// facts, observations and births are inert, and automatic writes alone never reach the 75 %
/// records trigger. One record short of the quota asks for nothing.
/// NEGATIVE CONTROL: with `capacity` never reporting a spent quota, neither finding appears
/// at the quota: `` left: 0 right: 1 `` (the message `[]`).
#[test]
fn check_names_a_spent_quota_and_asks_for_a_roll() {
    let lab = Lab::new(&[Role::Writer]);
    let owner = Registry::at(lab.paths[0].clone());
    owner.open().unwrap();
    let born = |n: usize| {
        let pk = format!("mc-1{n:015}");
        vec![
            cell::change(
                &MACHINE,
                cell::CellKey {
                    pk: &pk,
                    col: "label",
                    elem: None,
                },
                &cell::CellValue::automatic(1, json!(format!("m{}", 100 + n))),
            )
            .unwrap(),
        ]
    };
    // `aterm db table check alab/registry` and `aterm db alab check`, the capacity findings.
    let capacity = || -> [Vec<Value>; 2] {
        let Output::Json(table) = run(&lab.paths[0], &["check", "alab/registry"]) else {
            panic!("check prints JSON");
        };
        let alab = crate::alab::command::run(
            &lab.paths[0],
            &["check".to_owned()],
            None,
            &mut |_| panic!("check opens no review"),
            0,
        )
        .unwrap();
        [table, alab].map(|report| {
            report["findings"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|f| matches!(f["finding"].as_str(), Some("quota-spent" | "roll")))
                .cloned()
                .collect()
        })
    };
    owner
        .write((1..registry::SELF_QUOTA).map(born).collect())
        .unwrap();
    assert_eq!(capacity(), [Vec::<Value>::new(), Vec::new()]);
    owner.write(vec![born(registry::SELF_QUOTA)]).unwrap();
    for found in capacity() {
        let spent: Vec<&Value> = found
            .iter()
            .filter(|f| f["finding"] == json!("quota-spent"))
            .collect();
        assert_eq!(spent.len(), 1, "{found:?}");
        assert_eq!(spent[0]["mine"], json!(true));
        assert_eq!(spent[0]["spent"], json!(registry::SELF_QUOTA));
        assert!(found.contains(
            &json!({"finding": "roll", "reason": "quota", "verb": "aterm db alab roll --apply"})
        ));
    }
}

/// A filtered `edit --where` session never drops a stale row on reopen when a peer's concurrent
/// edit moves that row outside the filter: the reopened file still shows `⚠ mine ‖ theirs` so
/// the owner's edit is preserved and can be resolved in the second round.
/// NEGATIVE CONTROL: with `edit::edit` re-exporting through `filters` alone on reopen, `MC`
/// (now `parked` in the store) is excluded from `fresh.base` and `grid::reopen` silently drops
/// `⚠ lost ‖ parked`: `` assertion failed: reopened.contains("⚠ lost ‖ parked") ``.
#[test]
fn filtered_edit_reopen_keeps_a_stale_row_that_left_the_filter() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let owner = Registry::at(lab.paths[0].clone());
    owner.open().unwrap();
    owner.write(vec![machine(MC)]).unwrap();
    repair_new_registry(&lab);
    let handle = Handle::at(lab.paths[0].clone(), &REGISTRY);
    let filters = [sheet::Where::parse("status=active").unwrap()];
    let mut reopened = String::new();
    let mut rounds = 0;
    let edited = edit::edit(
        &handle,
        &MACHINE,
        &filters,
        false,
        None,
        &mut |path: &Path, line: usize, _: &[String]| {
            rounds += 1;
            let held = std::fs::read_to_string(path).unwrap();
            if rounds == 1 {
                Registry::at(lab.paths[1].clone())
                    .write(vec![vec![put(&MACHINE, MC, "status", json!("parked"))]])
                    .unwrap();
                repair_registry(&lab, 1);
                let row = record(&held, MC);
                std::fs::write(path, type_in(&held, row, "status", "lost")).unwrap();
            } else {
                assert_eq!(line, 2);
                reopened = held.clone();
                let row = record(&held, MC);
                std::fs::write(path, type_in(&held, row, "status", "lost")).unwrap();
            }
            Ok(())
        },
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(edited.rounds, 2);
    assert!(reopened.contains("⚠ lost ‖ parked"), "{reopened}");
    assert_eq!(
        handle.view().unwrap().project(&MACHINE).rows[MC].get("status"),
        Some(&json!("lost"))
    );
}

/// Unchecked set elements, non-boolean 3-part cell payloads and newer-build set columns stay
/// redacted on every verb (unknown non-secret set words stay visible and read-only), and CLI
/// refusals never quote an address or credential passed in the wrong argument slot.
/// NEGATIVE CONTROLS: with `change_json` returning raw `cell.x` whenever `key.elem.is_some()`,
/// history prints `TOKEN`; with `get` or `parse` formatting `{pk}` or `{flag}`, the refusal
/// quotes `ann.lee@example.invalid` or `TOKEN`.
#[test]
fn unchecked_set_cells_and_cli_refusals_never_leak_tokens_or_addresses() {
    const TOKEN: &str = concat!("ghp", "_cccccccccccccccccccccc");
    let lab = Lab::new(&[Role::Writer]);
    let owner = Registry::at(lab.paths[0].clone());
    owner.open().unwrap();
    let mut row = machine(MC);
    row.push(crate::database::Change {
        collection: MACHINE.name.to_owned(),
        key: format!("{MC}.clis.{TOKEN}"),
        value: Some(crate::typed::cell::CellValue::owner(1, json!(true)).encode()),
    });
    row.push(crate::database::Change {
        collection: MACHINE.name.to_owned(),
        key: format!("{MC}.clis.muse"),
        value: Some(crate::typed::cell::CellValue::owner(1, json!(TOKEN)).encode()),
    });
    row.push(crate::database::Change {
        collection: MACHINE.name.to_owned(),
        key: format!("{MC}.tags.future-tag"),
        value: Some(crate::typed::cell::CellValue::owner(2, json!(true)).encode()),
    });
    row.sort_by(|a, b| (&a.collection, &a.key).cmp(&(&b.collection, &b.key)));
    owner.write(vec![row]).unwrap();
    let scratch = aterm_tempfile::tempdir().unwrap();
    let file = scratch.path().join("machine.tsv");
    let paths = &lab.paths[0];
    for reveal in [false, true] {
        let words = |verb: &[&str]| {
            let mut list = verb.to_vec();
            if reveal {
                list.push("--show-addresses");
            }
            text(&run(paths, &list))
        };
        let seen = [
            (
                "scan json",
                words(&["scan", "alab/registry", "machine", "--json"]),
            ),
            (
                "scan tsv",
                words(&["scan", "alab/registry", "machine", "--tsv"]),
            ),
            ("get", words(&["get", "alab/registry", "machine", MC])),
            ("history", words(&["history", "alab/registry", "machine"])),
            ("check", words(&["check", "alab/registry"])),
            (
                "export",
                words(&[
                    "export",
                    "alab/registry",
                    "machine",
                    "--tsv",
                    file.to_str().unwrap(),
                ]),
            ),
            ("file", std::fs::read_to_string(&file).unwrap()),
        ];
        for (verb, shown) in &seen {
            assert!(!shown.contains(TOKEN), "{verb}: {shown}");
        }
        assert!(seen[0].1.contains(sheet::REFUSED), "{}", seen[0].1);
        assert!(seen[1].1.contains("future-tag"), "{}", seen[1].1);
        assert!(seen[3].1.contains(sheet::REFUSED), "{}", seen[3].1);
    }

    let addr = "ann.lee@example.invalid";
    let opt = format!("--token={TOKEN}");
    let bad_filter = format!("{addr}=1");
    for bad in [
        vec![addr],
        vec![TOKEN, "--bogus"],
        vec!["scan", "alab/registry", "machine", &opt],
        vec!["get", "alab/registry", addr, MC],
        vec!["get", "alab/registry", "address", addr],
        vec!["scan", "alab/registry", "machine", "--where", &bad_filter],
    ] {
        let words: Vec<String> = bad.into_iter().map(str::to_owned).collect();
        let why = command::run(paths, &words, &mut never_open, &mut |_| {})
            .unwrap_err()
            .0;
        assert!(!why.contains("ann.lee") && !why.contains(TOKEN), "{why}");
    }
    for bad in [
        vec![addr, "--apply"],
        vec!["check", &opt],
        vec!["check", addr],
    ] {
        let words: Vec<String> = bad.into_iter().map(str::to_owned).collect();
        let why = crate::alab::command::run(paths, &words, None, &mut |_| Ok(()), 0)
            .unwrap_err()
            .0;
        assert!(!why.contains("ann.lee") && !why.contains(TOKEN), "{why}");
    }

    for mismatched in [
        vec!["scan", "alab/registry", "machine", "--tsv", "--json"],
        vec!["ls", "--publish"],
        vec!["check", "alab/registry", "--where", "status=active"],
        vec![
            "export",
            "alab/registry",
            "machine",
            "--tsv",
            "one.tsv",
            "--tsv",
            "two.tsv",
        ],
    ] {
        let words: Vec<String> = mismatched.into_iter().map(str::to_owned).collect();
        assert!(command::run(paths, &words, &mut never_open, &mut |_| {}).is_err());
    }

    let target = scratch.path().join("real.tsv");
    std::fs::write(&target, "label\t=pk\n").unwrap();
    std::fs::write(command::baseline(&target), "{}").unwrap();
    let link = scratch.path().join("linked.tsv");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    std::fs::write(
        command::baseline(&link),
        export_machines(paths).base.to_json().to_string(),
    )
    .unwrap();
    let apply_link = vec!["apply".to_owned(), link.to_str().unwrap().to_owned()];
    assert!(command::run(paths, &apply_link, &mut never_open, &mut |_| {}).is_err());
}

/// `scan` refuses `--where` on `Kind::Owned` collections instead of silently returning every
/// owned row unfiltered, `read_regular_utf8` refuses symlinks and directories without following
/// them, and `Handle::apply` omits a stale row deletion from `retired`.
/// NEGATIVE CONTROLS: without the `Kind::Owned` filter check in `scan`, `--where pk=none`
/// returns `Ok`; with `std::fs::metadata` + `std::fs::read` in `read_regular_utf8`, reading a
/// symlink returns `Ok("target\n")`; with `apply` echoing `plan.retired`, a stale row deletion
/// still reports `[MC]` in `retired`.
#[test]
fn scan_refuses_owned_filters_and_grid_reads_refuse_symlinks_and_stale_retirements() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let owner = Registry::at(lab.paths[0].clone());
    owner.open().unwrap();
    owner.write(vec![machine(MC)]).unwrap();
    repair_new_registry(&lab);

    let filter_words: Vec<String> = [
        "scan",
        "alab/registry",
        "machine-facts",
        "--where",
        "pk=none",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let why = command::run(&lab.paths[0], &filter_words, &mut never_open, &mut |_| {})
        .unwrap_err()
        .0;
    assert!(
        why.contains("--where filters cells collections only"),
        "{why}"
    );

    let scratch = aterm_tempfile::tempdir().unwrap();
    let target = scratch.path().join("target.tsv");
    std::fs::write(&target, "target\n").unwrap();
    let link = scratch.path().join("link.tsv");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert!(read_regular_utf8(&link, 64, "unreadable", "oversize", "utf8").is_err());
    assert!(read_regular_utf8(scratch.path(), 64, "unreadable", "oversize", "utf8").is_err());
    assert_eq!(
        read_regular_utf8(&target, 64, "unreadable", "oversize", "utf8").unwrap(),
        "target\n"
    );
    assert_eq!(
        read_regular_utf8(&target, 3, "unreadable", "oversize", "utf8")
            .unwrap_err()
            .0,
        "oversize"
    );

    let file = scratch.path().join("machine.tsv");
    let word = file.to_str().unwrap();
    run(
        &lab.paths[0],
        &["export", "alab/registry", "machine", "--tsv", word],
    );
    // Peer concurrently changes the row's lifecycle before the owner deletes the row in the TSV.
    Registry::at(lab.paths[1].clone())
        .write(vec![vec![put(&MACHINE, MC, "row", json!("retired"))]])
        .unwrap();
    repair_registry(&lab, 1);
    let header_only = format!(
        "{}\n",
        std::fs::read_to_string(&file)
            .unwrap()
            .lines()
            .next()
            .unwrap()
    );
    std::fs::write(&file, header_only).unwrap();
    // When a peer already retired the row, deleting it in the TSV writes nothing and reports
    // `retired: []`.
    let Output::Json(same_retire) = run(&lab.paths[0], &["apply", word]) else {
        panic!("apply prints JSON");
    };
    assert_eq!(same_retire["written"], json!(0));
    assert_eq!(same_retire["retired"], json!([]));

    // When a peer writes `"live"` concurrently, the deletion is held back as stale and `retired`
    // stays empty.
    Registry::at(lab.paths[0].clone())
        .write(vec![vec![put(&MACHINE, MC, "row", json!("live"))]])
        .unwrap();
    repair_registry(&lab, 1);
    run(
        &lab.paths[0],
        &["export", "alab/registry", "machine", "--tsv", word],
    );
    Registry::at(lab.paths[1].clone())
        .write(vec![vec![put(&MACHINE, MC, "row", json!("live"))]])
        .unwrap();
    repair_registry(&lab, 1);
    let header_only = format!(
        "{}\n",
        std::fs::read_to_string(&file)
            .unwrap()
            .lines()
            .next()
            .unwrap()
    );
    std::fs::write(&file, header_only).unwrap();
    let Output::Json(stale_retire) = run(&lab.paths[0], &["apply", word]) else {
        panic!("apply prints JSON");
    };
    assert_eq!(stale_retire["written"], json!(0));
    assert_eq!(stale_retire["retired"], json!([]));
    assert_eq!(stale_retire["stale"], json!([{"pk": MC, "column": "row"}]));
}

/// E1 (`docs/measured/e2e-emails-table-2026-11-05.md`): on three real Macs a digest read that a
/// repair round raced failed with "table witness advanced during observation; reopen and verify".
/// A read now reopens while the observation answers busy, until its deadline; any other answer,
/// and a busy one past the deadline, comes back at once.
/// NEGATIVE CONTROL: with `reopened` calling `observe` once, the first assertion fails (left a
/// busy error, right `Ok(3)`); observed.
#[test]
fn a_read_that_a_writer_kept_busy_reopens_until_it_reads() {
    let later = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut attempts = 0;
    let read = reopened(later, || {
        attempts += 1;
        if attempts < 3 {
            Err(Error::snapshot_changed())
        } else {
            Ok(attempts)
        }
    });
    assert_eq!(read.map_err(|e| e.to_string()), Ok(3));

    let mut attempts = 0;
    let read: Result<()> = reopened(later, || {
        attempts += 1;
        Err(Error::new("application table is not initialized"))
    });
    assert!(read.is_err_and(|e| !e.is_busy()));
    assert_eq!(attempts, 1, "a refusal that is not busy is not read again");

    let mut attempts = 0;
    let read: Result<()> = reopened(std::time::Instant::now(), || {
        attempts += 1;
        Err(Error::snapshot_changed())
    });
    assert!(read.is_err_and(|e| e.is_busy()));
    assert_eq!(attempts, 1, "past the deadline a busy read is the answer");
}
