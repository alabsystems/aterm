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
        registry::{ADDRESS, CLI, MACHINE, MACHINE_FACTS},
    },
    table::tests::{Lab, repair_commons_first},
    typed::{
        grid::{
            self as sheet,
            tests::{MC, add_row, elem, machine, put, record, type_in},
        },
        keys,
    },
};
use aterm_codec::hex;
use aterm_json::json;
use aterm_secrets::commons::roster::Role;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

fn never_open(_: &Path, _: usize) -> Result<()> {
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

/// `command` signed as the session's next record with an explicit clock claim, on the installation
/// chain and the complete observed frontier, and not yet ingested.
fn seal_history_at(
    session: &crate::table::Session<'_>,
    store: &crate::table::store::Store,
    scope: crate::table::Scope,
    command: &Command,
    authored_ms: u64,
) -> Result<Vec<u8>> {
    let evidence = store.metadata_evidence(session)?;
    let headers: Vec<_> = evidence
        .iter()
        .filter(|op| op.catalog_eligible)
        .map(|op| &op.header)
        .collect();
    let previous = headers
        .iter()
        .filter(|header| header.actor == session.actor())
        .max_by_key(|header| header.sequence);
    let mut parents: BTreeSet<_> = headers.iter().map(|header| header.id).collect();
    for header in &headers {
        for parent in header.parents.iter().chain(header.previous.iter()) {
            parents.remove(parent);
        }
    }
    if let Some(previous) = previous {
        parents.insert(previous.id);
    }
    let parents: Vec<_> = parents.into_iter().collect();
    crate::table::fixture::seal_at(
        session,
        scope,
        crate::crypto::Causal {
            sequence: previous.map_or(1, |header| header.sequence + 1),
            previous: previous.map(|header| header.id),
            parents: &parents,
        },
        &command.encode()?,
        authored_ms,
    )
}

/// Append an authenticated operation with an explicit signed clock claim, preserving the
/// installation chain and the complete observed frontier.
fn append_history_at(handle: &Handle, command: Command, authored_ms: u64) -> Digest {
    application::with_store(
        &handle.paths,
        false,
        handle.descriptor(),
        |session, store, scope| {
            let wire = seal_history_at(session, store, scope, &command, authored_ms)?;
            Ok(store.ingest(session, &wire)?.id)
        },
    )
    .unwrap()
}

/// Signed records survive repair with causal order intact despite a backward clock. Hidden
/// merges and refused payloads still advance descendants; concurrent equal clocks use digest.
/// Restoring the authored-time sort makes the parent follow its observed child (negative control).
#[test]
fn history_follows_causality_when_a_writer_clock_runs_behind() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    Registry::at(lab.paths[0].clone()).open().unwrap();
    let handle = Handle::at(lab.paths[0].clone(), &REGISTRY);
    let parent = append_history_at(
        &handle,
        Command::Write {
            request: [71; 16],
            changes: machine(MC),
        },
        u64::MAX,
    );
    repair_new_registry(&lab);
    let peer = Handle::at(lab.paths[1].clone(), &REGISTRY);
    let concurrent = append_history_at(
        &peer,
        Command::Write {
            request: [72; 16],
            changes: vec![put(&MACHINE, MC, "status", json!("parked"))],
        },
        0,
    );
    let sibling = append_history_at(
        &handle,
        Command::Write {
            request: [75; 16],
            changes: vec![put(&MACHINE, MC, "status", json!("active"))],
        },
        0,
    );
    let merge = append_history_at(&handle, Command::Merge { request: [73; 16] }, 0);
    let refused = append_history_at(
        &handle,
        Command::Write {
            request: [76; 16],
            changes: vec![crate::database::Change {
                collection: MACHINE.name.into(),
                key: format!("{MC}.note"),
                value: None,
            }],
        },
        0,
    );
    let child = append_history_at(
        &handle,
        Command::Write {
            request: [74; 16],
            changes: vec![put(&MACHINE, MC, "location", json!("rack b"))],
        },
        0,
    );
    repair_registry(&lab, 2);
    let history = handle.history().unwrap();
    let peer_history = peer.history().unwrap();
    assert_eq!(
        history
            .iter()
            .map(|entry| (entry.operation, entry.hlc))
            .collect::<Vec<_>>(),
        peer_history
            .iter()
            .map(|entry| (entry.operation, entry.hlc))
            .collect::<Vec<_>>()
    );
    let position = |id| {
        history
            .iter()
            .position(|entry| entry.operation == id)
            .unwrap()
    };
    assert!(position(parent) < position(concurrent));
    assert!(position(parent) < position(child));
    assert!(!history.iter().any(|entry| entry.operation == merge));
    assert!(!history.iter().any(|entry| entry.operation == refused));
    for (id, counter, authored_ms) in [
        (parent, 0, u64::MAX),
        (concurrent, 1, 0),
        (sibling, 1, 0),
        (child, 4, 0),
    ] {
        let entry = &history[position(id)];
        assert_eq!(
            entry.hlc,
            Hlc {
                ms: u64::MAX,
                counter
            }
        );
        assert_eq!(entry.authored_ms, authored_ms);
    }
    assert!(
        history
            .windows(2)
            .all(|pair| (pair[0].hlc, pair[0].operation) < (pair[1].hlc, pair[1].operation))
    );
    let Output::Json(output) = run(&lab.paths[0], &["history", "alab/registry", "machine"]) else {
        panic!("history must return JSON");
    };
    let operations = output["operations"].as_array().unwrap();
    let shown = operations
        .iter()
        .find(|op| op["operation"] == json!(hex::encode_lower(&child)))
        .unwrap();
    assert_eq!(shown["hlc"], json!({"ms": u64::MAX, "counter": 4}));
    assert_eq!(shown["authored_ms"], json!(0));
}

/// A clear of a typed cell (`x: null`), as the grid writes an emptied cell.
fn clear(pk: &str, column: &str) -> crate::database::Change {
    put(&MACHINE, pk, column, Value::Null)
}

/// A set member taken out (`x: false`).
fn take_out(pk: &str, column: &str, member: &str) -> crate::database::Change {
    crate::typed::cell::change(
        &MACHINE,
        crate::typed::cell::CellKey {
            pk,
            col: column,
            elem: Some(member),
        },
        &crate::typed::cell::CellValue::owner(1, Value::Bool(false)),
    )
    .unwrap()
}

/// A table reads as of the causal cut an operation names, through the one read every verb uses,
/// and two points compare forward with each difference named by the operation, writer and
/// machine that made it. Mac 0 writes a row (`born`); Mac 1 then sets its status (`parked`) while
/// Mac 0, concurrently and with a clock claim older than Mac 1's, sets another status and takes a
/// set member out (`lost`); after both meet, Mac 0 sets a nickname and clears the location
/// (`after`). As of `parked` the table holds only `born` and `parked`: `lost`, which `parked`
/// never observed, is left out though its clock claims an earlier time. From `born` to the head
/// the nickname is added, the location and the member retired, and the status conflict-changed
/// by both concurrent writes; from `born` to `parked` the status is changed, by Mac 1 alone.
/// NEGATIVE CONTROL: with `Log::as_of` keeping every operation (its cut ignored), `get --at`
/// reads the head: `` assertion `left == right` failed: as of born left: Null right:
/// String("rack b") ``. With the cut taken by authored time (every operation whose clock claims
/// no later time than the point's), `lost`, which claims an earlier time than `born`, joins
/// `born`'s cut: `` left: String("lost") right: String("active") ``.
#[test]
fn a_table_reads_as_of_an_operation_and_diffs_two_points() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    Registry::at(lab.paths[0].clone()).open().unwrap();
    let here = Handle::at(lab.paths[0].clone(), &REGISTRY);
    let mut born_row = machine(MC);
    born_row[2] = put(&MACHINE, MC, "location", json!("rack b"));
    let born = append_history_at(
        &here,
        Command::Write {
            request: [81; 16],
            changes: born_row,
        },
        2_000,
    );
    repair_new_registry(&lab);
    let there = Handle::at(lab.paths[1].clone(), &REGISTRY);
    let parked = append_history_at(
        &there,
        Command::Write {
            request: [82; 16],
            changes: vec![put(&MACHINE, MC, "status", json!("parked"))],
        },
        3_000,
    );
    let lost = append_history_at(
        &here,
        Command::Write {
            request: [83; 16],
            changes: vec![
                put(&MACHINE, MC, "status", json!("lost")),
                take_out(MC, "clis", "codex"),
            ],
        },
        1_000,
    );
    repair_registry(&lab, 2);
    let after = append_history_at(
        &here,
        Command::Write {
            request: [84; 16],
            changes: vec![
                put(&MACHINE, MC, "nickname", json!("spare")),
                clear(MC, "location"),
            ],
        },
        4_000,
    );
    repair_registry(&lab, 2);
    let hex = |id: &Digest| hex::encode_lower(id);
    let get_at = |at: Option<&Digest>| {
        let mut words = vec!["get", "alab/registry", "machine", MC];
        let at = at.map(hex);
        if let Some(at) = &at {
            words.extend(["--at", at.as_str()]);
        }
        let Output::Json(row) = run(&lab.paths[0], &words) else {
            panic!("get prints JSON");
        };
        row["row"].clone()
    };

    // As of `born`: the row as Mac 0 first wrote it.
    let as_born = get_at(Some(&born));
    assert_eq!(as_born["location"], json!("rack b"), "as of born");
    assert_eq!(as_born["status"], json!("active"));
    assert_eq!(as_born["clis"], json!(["claude", "codex"]));
    assert_eq!(as_born["nickname"], Value::Null);
    // As of `parked`: Mac 1's status, without `lost`, whose clock claims an earlier time.
    let as_parked = get_at(Some(&parked));
    assert_eq!(as_parked["status"], json!("parked"), "as of parked");
    assert_eq!(as_parked["clis"], json!(["claude", "codex"]));
    // The head: both statuses, the nickname, no location.
    let head = get_at(None);
    let mut statuses: Vec<String> = head["status"]["conflict"]
        .as_array()
        .expect("the head's status is a conflict")
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    statuses.sort();
    assert_eq!(statuses, ["lost", "parked"]);
    assert_eq!(head["nickname"], json!("spare"));
    assert_eq!(head["location"], Value::Null);
    assert_eq!(head["clis"], json!(["claude"]));
    // scan and export read the same cut.
    let Output::Json(scanned) = run(
        &lab.paths[0],
        &["scan", "alab/registry", "machine", "--at", &hex(&born)],
    ) else {
        panic!("scan prints JSON");
    };
    assert_eq!(scanned["rows"][0]["location"], json!("rack b"));
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
            "--at",
            &hex(&parked),
        ],
    );
    let exported = std::fs::read_to_string(&file).unwrap();
    assert!(exported.contains("\tparked\t"), "{exported}");
    assert!(!exported.contains("lost"), "{exported}");
    // A point this table does not hold is refused, never read as the head.
    let unknown = command::run(
        &lab.paths[0],
        &[
            "get",
            "alab/registry",
            "machine",
            MC,
            "--at",
            &hex(&[9; 32]),
        ]
        .map(str::to_owned),
        &mut never_open,
        &mut |_| {},
    );
    assert!(unknown.is_err());

    // Each operation's machine, as history resolves it.
    let history = here.history().unwrap();
    let machine_of = |id: &Digest| {
        history
            .iter()
            .find(|entry| entry.operation == *id)
            .unwrap()
            .machine
    };
    assert_ne!(machine_of(&parked), machine_of(&lost), "two Macs wrote");
    let made = |diff: &Diff, key: &str| {
        let cell = diff
            .cells
            .iter()
            .find(|cell| cell.collection == MACHINE.name && cell.key == key)
            .unwrap_or_else(|| panic!("{key} differs"));
        (
            cell.delta,
            cell.by
                .iter()
                .map(|by| (by.operation, by.machine))
                .collect::<BTreeSet<_>>(),
        )
    };

    // From `born` to the head: every kind of difference, each with its makers.
    let to_head = here.diff(born, Point::Head).unwrap();
    assert_eq!(
        made(&to_head, &format!("{MC}.nickname")),
        (Delta::Added, BTreeSet::from([(after, machine_of(&after))]))
    );
    assert_eq!(
        made(&to_head, &format!("{MC}.location")),
        (
            Delta::Retired,
            BTreeSet::from([(after, machine_of(&after))])
        )
    );
    assert_eq!(
        made(&to_head, &format!("{MC}.clis.codex")),
        (Delta::Retired, BTreeSet::from([(lost, machine_of(&lost))]))
    );
    assert_eq!(
        made(&to_head, &format!("{MC}.status")),
        (
            Delta::ConflictChanged,
            BTreeSet::from([(parked, machine_of(&parked)), (lost, machine_of(&lost))])
        )
    );
    assert!(
        !to_head.cells.iter().any(
            |cell| cell.key == format!("{MC}.label") || cell.key == format!("{MC}.clis.claude")
        ),
        "an unchanged cell is not a difference"
    );
    // From `born` to `parked`: the status changed, by Mac 1 alone; nothing else.
    let to_parked = here.diff(born, Point::At(parked)).unwrap();
    assert_eq!(
        to_parked
            .cells
            .iter()
            .map(|cell| (cell.key.as_str(), cell.delta))
            .collect::<Vec<_>>(),
        vec![(format!("{MC}.status").as_str(), Delta::Changed)]
    );
    assert_eq!(
        made(&to_parked, &format!("{MC}.status")).1,
        BTreeSet::from([(parked, machine_of(&parked))])
    );
    // `lost` never observed `parked`: no forward comparison runs between them.
    assert!(here.diff(parked, Point::At(lost)).is_err());
    // A peer that holds the same operations answers the same comparison.
    assert_eq!(there.diff(born, Point::Head).unwrap(), to_head);

    // The verb: one cell per difference, with its operation, author and machine.
    let Output::Json(shown) = run(
        &lab.paths[0],
        &["diff", "alab/registry", "machine", &hex(&born), "head"],
    ) else {
        panic!("diff prints JSON");
    };
    assert_eq!(shown["from"], json!(hex(&born)));
    assert_eq!(shown["to"], json!("head"));
    let status = shown["cells"]
        .as_array()
        .unwrap()
        .iter()
        .find(|cell| cell["key"] == json!(format!("{MC}.status")))
        .unwrap();
    assert_eq!(status["change"], json!("conflict-changed"));
    assert_eq!(status["before"], json!(["active"]));
    let by: BTreeSet<String> = status["by"]
        .as_array()
        .unwrap()
        .iter()
        .map(|by| {
            assert_eq!(by["author"].as_str().unwrap().len(), 96);
            assert!(by["machine"].as_str().is_some());
            by["operation"].as_str().unwrap().to_owned()
        })
        .collect();
    assert_eq!(by, BTreeSet::from([hex(&parked), hex(&lost)]));
    let Output::Json(forward) = run(
        &lab.paths[0],
        &["diff", "alab/registry", &hex(&born), &hex(&parked)],
    ) else {
        panic!("diff prints JSON");
    };
    assert_eq!(forward["cells"].as_array().unwrap().len(), 1);
    // A mistyped digest is refused as a digest, never taken for a collection.
    let mistyped = command::run(
        &lab.paths[0],
        &["diff", "alab/registry", &hex(&born).to_uppercase()].map(str::to_owned),
        &mut never_open,
        &mut |_| {},
    )
    .unwrap_err();
    assert!(mistyped.to_string().contains("hex"), "{mistyped}");
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
        let mut open = |path: &Path, line: usize| -> Result<()> {
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
        edit::edit(&handle, &MACHINE, &[], &mut open, &mut |line| {
            reports.push(line.to_owned());
        })
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
        &mut |path: &Path, _| {
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

/// An address is ordinary data: scan as JSON and as TSV, get, history and an exported file all
/// show it as stored, and `--show-addresses` is no option of any verb.
/// NEGATIVE CONTROL: with `grid::show` abbreviating an `Email` (`an…@…`), the TSV scan lacks the
/// address and the check fails naming the verb: `` scan tsv: addr domain … =pk an…@… … ``.
#[test]
fn every_table_verb_shows_an_address_as_stored() {
    const ADDR: &str = "ann.lee@example.invalid";
    let lab = Lab::new(&[Role::Writer]);
    let owner = Registry::at(lab.paths[0].clone());
    owner.open().unwrap();
    let pk = keys::address_pk(ADDR).unwrap();
    owner
        .write(vec![vec![
            put(&ADDRESS, &pk, "addr", json!(ADDR)),
            put(&ADDRESS, &pk, "domain", json!("example.invalid")),
            put(&ADDRESS, &pk, "origin", json!("owner")),
            put(&ADDRESS, &pk, "row", json!("live")),
        ]])
        .unwrap();
    let scratch = aterm_tempfile::tempdir().unwrap();
    let file = scratch.path().join("address.tsv");
    let paths = &lab.paths[0];
    run(
        paths,
        &[
            "export",
            "alab/registry",
            "address",
            "--tsv",
            file.to_str().unwrap(),
        ],
    );
    let seen = [
        (
            "scan json",
            text(&run(paths, &["scan", "alab/registry", "address", "--json"])),
        ),
        (
            "scan tsv",
            text(&run(paths, &["scan", "alab/registry", "address", "--tsv"])),
        ),
        (
            "get",
            text(&run(paths, &["get", "alab/registry", "address", &pk])),
        ),
        (
            "history",
            text(&run(paths, &["history", "alab/registry", "address"])),
        ),
        ("file", std::fs::read_to_string(&file).unwrap()),
        (
            "baseline",
            std::fs::read_to_string(command::baseline(&file)).unwrap(),
        ),
    ];
    for (verb, shown) in &seen {
        assert!(shown.contains(ADDR), "{verb}: {shown}");
        assert!(!shown.contains("an…@…"), "{verb}: {shown}");
    }
    for verb in [
        vec!["scan", "alab/registry", "address", "--show-addresses"],
        vec!["get", "alab/registry", "address", &pk, "--show-addresses"],
        vec!["history", "alab/registry", "--show-addresses"],
    ] {
        let words: Vec<String> = verb.into_iter().map(str::to_owned).collect();
        let why = command::run(paths, &words, &mut never_open, &mut |_| {})
            .unwrap_err()
            .0;
        assert!(why.contains("unknown option"), "{why}");
    }
}

/// A value another writer's build stored though this one refuses it: a note holding a token
/// reads `REFUSED` on every verb, history included, while a note holding a phone number or an
/// address, ordinary data, shows as stored.
/// NEGATIVE CONTROLS: with `grid::show` skipping its `admissible` check, history prints the
/// stored token: `` history: {"operations":[{"authored_ms":…`` (the token assertion); with
/// `raw_shown` never consulting `guard::withheld`, scan prints it: `` scan json:
/// {"collection":"machine",…"note":"ghp_cccc…"…``; with `raw_shown` refusing
/// `guard::phone_shaped` text again, the phone note reads `REFUSED` and the assertion that it
/// shows as stored fails.
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
    let words = |verb: &[&str]| text(&run(paths, verb));
    words(&[
        "export",
        "alab/registry",
        "machine",
        "--tsv",
        file.to_str().unwrap(),
    ]);
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
        ("file", std::fs::read_to_string(&file).unwrap()),
    ];
    for (verb, shown) in &seen {
        assert!(!shown.contains(TOKEN), "{verb}: {shown}");
        // `get` reads the one row whose note holds the address.
        if *verb != "get" {
            assert!(shown.contains("555 0101"), "{verb}: {shown}");
        }
        assert!(shown.contains("ann.lee@example.invalid"), "{verb}: {shown}");
    }
    assert!(seen[0].1.contains(sheet::REFUSED), "{}", seen[0].1);
    assert!(seen[3].1.contains(sheet::REFUSED), "{}", seen[3].1);
    let get = words(&["get", "alab/registry", "machine", token]);
    assert!(
        get.contains(sheet::REFUSED) && !get.contains(TOKEN),
        "{get}"
    );
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

/// An explicit undo of a withdrawn machine number has the same claim semantics whether
/// the exact beforeimage is a native value or a candidate carried by a registry roll.
#[test]
fn typed_claim_undo_keeps_its_semantics_across_a_registry_roll() {
    use crate::table::tests::repair_commons;
    let pks = ["mc-0000000000000001", "mc-0000000000000002"];
    for rolled in [false, true] {
        let lab = Lab::new(&[Role::Writer, Role::Writer]);
        let macs: Vec<_> = lab.paths.iter().cloned().map(Registry::at).collect();
        for registry in &macs {
            registry.open().unwrap();
        }
        repair_commons(&lab, 6);
        for (registry, pk) in macs.iter().zip(pks) {
            let mut row = machine(pk);
            row[0] = put(&MACHINE, pk, "label", json!("m7"));
            registry.write(vec![row]).unwrap();
        }
        repair_commons(&lab, 6);
        let original = macs[0].view().unwrap().project(&MACHINE);
        let winner = pks
            .iter()
            .position(|pk| original.rows[*pk].get("label") == Some(&json!("m7")))
            .unwrap();
        let now = || aterm_time::unix_millis().unwrap();
        if rolled {
            macs[0].roll(true, now()).unwrap();
            repair_commons(&lab, 6);
            macs[1].release(now()).unwrap();
            repair_commons(&lab, 6);
            assert!(macs[0].roll(true, now()).unwrap().sealed);
            repair_commons(&lab, 6);
            assert_eq!(macs[winner].generation().generation, 2);
            assert_eq!(
                macs[winner].view().unwrap().project(&MACHINE).rows[pks[winner]].get("label"),
                Some(&json!("m7")),
            );
        }
        macs[winner]
            .write(vec![vec![put(&MACHINE, pks[winner], "label", json!("m8"))]])
            .unwrap();
        let handle = Handle::at(lab.paths[winner].clone(), macs[winner].generation());
        let coordinate = (MACHINE.name.to_owned(), format!("{}.label", pks[winner]));
        let target = handle.view().unwrap().cells[&coordinate].candidates[0].operation;
        let undone = handle.revert([81; 16], target).unwrap();
        let read = handle.view().unwrap();
        assert_eq!(read.cells[&coordinate].candidates[0].operation, undone);
        let signed = read
            .ops
            .iter()
            .find(|operation| operation.id == undone)
            .unwrap();
        assert_eq!(
            crate::typed::cell::carried_refs(signed.changes[0].value.as_ref().unwrap()).is_some(),
            rolled,
            "the signed compensation still contains its exact beforeimage"
        );
        assert!(
            crate::typed::cell::CellValue::decode(&read.cells[&coordinate].candidates[0].value)
                .is_ok()
        );
        let restored = read.project(&MACHINE);
        assert_eq!(
            restored.rows[pks[1 - winner]].get("label"),
            Some(&json!("m7")),
            "a restored claim follows the incumbent after its withdrawal, rolled={rolled}"
        );
    }
}

/// A lifecycle reactivation reasserts the observed claim at its signed event, including
/// when the registry carries that assertion into a sealed generation.
#[test]
fn a_revived_machine_keeps_the_replacement_holders_label_across_a_roll() {
    use crate::table::tests::repair_commons;
    let pks = ["mc-0000000000000001", "mc-0000000000000002"];
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let macs: Vec<_> = lab.paths.iter().cloned().map(Registry::at).collect();
    for registry in &macs {
        registry.open().unwrap();
    }
    repair_commons(&lab, 6);
    let mut first = machine(pks[0]);
    first[0] = put(&MACHINE, pks[0], "label", json!("m7"));
    macs[0].write(vec![first]).unwrap();
    macs[0]
        .write(vec![vec![put(&MACHINE, pks[0], "row", json!("retired"))]])
        .unwrap();
    repair_commons(&lab, 6);
    let mut second = machine(pks[1]);
    second[0] = put(&MACHINE, pks[1], "label", json!("m7"));
    macs[1].write(vec![second]).unwrap();
    repair_commons(&lab, 6);
    macs[0]
        .write(vec![vec![put(&MACHINE, pks[0], "row", json!("live"))]])
        .unwrap();
    repair_commons(&lab, 6);
    for registry in &macs {
        let view = registry.view().unwrap();
        assert_eq!(
            view.project(&MACHINE).rows[pks[1]].get("label"),
            Some(&json!("m7"))
        );
        assert_ne!(
            view.project(&MACHINE).rows[pks[0]].get("label"),
            Some(&json!("m7"))
        );
    }
    let now = || aterm_time::unix_millis().unwrap();
    macs[0].roll(true, now()).unwrap();
    repair_commons(&lab, 6);
    macs[1].release(now()).unwrap();
    repair_commons(&lab, 6);
    assert!(macs[0].roll(true, now()).unwrap().sealed);
    repair_commons(&lab, 6);
    for registry in &macs {
        assert_eq!(registry.generation().generation, 2);
        let view = registry.view().unwrap();
        assert_eq!(
            view.project(&MACHINE).rows[pks[1]].get("label"),
            Some(&json!("m7"))
        );
        assert_ne!(
            view.project(&MACHINE).rows[pks[0]].get("label"),
            Some(&json!("m7"))
        );
    }
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
        &mut |path: &Path, line: usize| {
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
    let words = |verb: &[&str]| text(&run(paths, verb));
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

/// A machine row `label` keyed `pk`, live, as a write lands it.
fn other_machine(pk: &str, label: &str) -> Vec<Change> {
    vec![
        put(&MACHINE, pk, "label", json!(label)),
        put(&MACHINE, pk, "status", json!("active")),
        put(&MACHINE, pk, "row", json!("live")),
    ]
}

/// The machine key no row of these tests holds.
const MISSING: &str = "mc-00000000000000ff";
/// A machine row these tests make.
const OTHER: &str = "mc-0000000000000008";

/// The problems a save comes back with, or a panic naming what it wrote instead.
fn problems(saved: Saved) -> Vec<grid::Problem> {
    match saved {
        Saved::Problems(problems) => problems,
        Saved::Written { applied, .. } => panic!("the save was written: {applied:?}"),
    }
}

/// A grid save whose `Ref` cell, or whose `Set(Ref)` element, names a row its target
/// collection does not hold is refused at that cell, as the registry's own rules refuse a
/// write, and nothing of it is written, not even the row's other edited cell. A reference to a
/// row the table holds is written, and a column already dangling (an unguarded write made it
/// so, as a concurrent one can) holds back no save that leaves it alone, while `check` still
/// reports it.
/// NEGATIVE CONTROL: with `Compiled::rules` answering the table's guard alone
/// (`typed::check::missing_row` not called), the save naming the missing machine is written:
/// `` the save was written: Applied { records: 1, cells: 2, .. } ``.
#[test]
fn a_grid_save_naming_a_missing_row_is_refused_and_writes_nothing() {
    let lab = Lab::new(&[Role::Writer]);
    let owner = Registry::at(lab.paths[0].clone());
    owner.open().unwrap();
    owner
        .write(vec![
            machine(MC),
            other_machine(OTHER, "m8"),
            vec![put(&CLI, "agy", "row", json!("live"))],
        ])
        .unwrap();
    let handle = Handle::at(lab.paths[0].clone(), &REGISTRY);
    let written = handle.history().unwrap().len();
    let exported = export_machines(&lab.paths[0]);
    let row = record(&exported.text, MC);
    let moved = type_in(&exported.text, row, "location", "rack b");

    let named = type_in(&moved, row, "merged_into", MISSING);
    let found = problems(handle.save(&MACHINE, &exported.base, &named).unwrap());
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].rule, "machine.merged_into: no such machine row");
    assert_eq!(
        (found[0].record, found[0].column),
        (row, sheet::tests::column(&named, "merged_into"))
    );
    assert!(!found[0].rule.contains(MISSING), "a refusal quotes no key");

    let added = type_in(&moved, row, "clis", "agy, claude, codex, nonesuch");
    let found = problems(handle.save(&MACHINE, &exported.base, &added).unwrap());
    assert_eq!(found[0].rule, "machine.clis: no such cli row");
    assert_eq!(found[0].column, sheet::tests::column(&added, "clis"));

    assert_eq!(handle.history().unwrap().len(), written, "nothing written");
    let rows = handle.view().unwrap().project(&MACHINE).rows;
    assert_eq!(rows[MC].get("location"), Some(&json!("rack a")));
    assert_eq!(rows[MC].get("merged_into"), None);

    // Rows the table holds are named, and both edits land.
    let held = type_in(
        &type_in(&moved, row, "merged_into", OTHER),
        row,
        "clis",
        "agy, claude, codex",
    );
    let Saved::Written { applied, .. } = handle.save(&MACHINE, &exported.base, &held).unwrap()
    else {
        panic!("references to held rows are written");
    };
    assert_eq!(applied.records, 1);
    let rows = handle.view().unwrap().project(&MACHINE).rows;
    assert_eq!(rows[MC].get("merged_into"), Some(&json!(OTHER)));
    assert!(rows[MC].fields["clis"].has_member("agy"));

    // A reference already dangling, in a column the save leaves alone, holds nothing back.
    owner
        .write(vec![vec![put(
            &MACHINE,
            OTHER,
            "merged_into",
            json!(MISSING),
        )]])
        .unwrap();
    let exported = export_machines(&lab.paths[0]);
    let saved = type_in(
        &exported.text,
        record(&exported.text, OTHER),
        "location",
        "rack c",
    );
    let Saved::Written { .. } = handle.save(&MACHINE, &exported.base, &saved).unwrap() else {
        panic!("a save that leaves a dangling column alone is written");
    };
    let Output::Json(checked) = run(&lab.paths[0], &["check", "alab/registry"]) else {
        panic!("check prints JSON");
    };
    let dangling: Vec<_> = checked["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["finding"] == json!("dangling") && f["column"] == json!("merged_into"))
        .collect();
    assert_eq!(dangling.len(), 1, "{checked}");
    assert_eq!(dangling[0]["pk"], json!(OTHER));
}

/// An owner verb's write and a revert pass the same rule as a grid save: a write naming a
/// missing row is refused and appends nothing, one that makes the row it names in the same
/// write is written, and a revert whose compensation would restore a reference to a missing row
/// is refused.
/// NEGATIVE CONTROL: with `Compiled::rules` answering the table's guard alone, the owner verb
/// naming the missing machine is written: `` an owner write naming a missing row is refused ``.
#[test]
fn an_owner_write_and_a_revert_naming_a_missing_row_are_refused() {
    let lab = Lab::new(&[Role::Writer]);
    let owner = Registry::at(lab.paths[0].clone());
    owner.open().unwrap();
    owner.write(vec![machine(MC)]).unwrap();
    let handle = Handle::at(lab.paths[0].clone(), &REGISTRY);
    let written = handle.history().unwrap().len();

    let refused = handle
        .commit_owner(|_| {
            Ok((
                (),
                vec![vec![put(&MACHINE, MC, "merged_into", json!(MISSING))]],
            ))
        })
        .expect_err("an owner write naming a missing row is refused");
    assert!(
        refused
            .to_string()
            .contains("machine.merged_into: row mc-0000000000000009: no such machine row"),
        "{refused}"
    );
    assert_eq!(handle.history().unwrap().len(), written, "nothing written");

    let mut born = other_machine(OTHER, "m8");
    born.push(put(&MACHINE, MC, "merged_into", json!(OTHER)));
    handle.commit_owner(|_| Ok(((), vec![born]))).unwrap();

    // An unguarded write leaves MC naming a missing row; the owner then names a held one, and
    // undoing that would name the missing row again.
    owner
        .write(vec![vec![put(&MACHINE, MC, "merged_into", json!(MISSING))]])
        .unwrap();
    let ((), applied) = handle
        .commit_owner(|_| {
            Ok((
                (),
                vec![vec![put(&MACHINE, MC, "merged_into", json!(OTHER))]],
            ))
        })
        .unwrap();
    let written = handle.history().unwrap().len();
    let refused = handle
        .revert([7; 16], applied.operations[0])
        .expect_err("a revert restoring a missing row's reference is refused");
    assert!(
        refused
            .to_string()
            .contains("machine.merged_into: row mc-0000000000000009: no such machine row"),
        "{refused}"
    );
    assert_eq!(handle.history().unwrap().len(), written, "nothing written");
    assert_eq!(
        handle.view().unwrap().project(&MACHINE).rows[MC].get("merged_into"),
        Some(&json!(OTHER))
    );
}

/// A row every build of the table compiles (a registry seed: a CLI, provider or plan) may be
/// named before any seed pass has written it here, as the next pass writes it: a grid save adding
/// a compiled CLI to a machine on a registry no pass seeded is written, `check` reports the
/// reference until a pass writes the row, and then reports nothing. A key no build compiles is
/// still refused.
/// NEGATIVE CONTROL: with `Compiled::rules` passing `|_, _| false` for `Compiled::seeded`, the
/// save naming the compiled CLI is refused: `` panicked … a compiled seed row may be named ``.
#[test]
fn a_write_may_name_a_seed_row_no_pass_has_written_here() {
    let lab = Lab::new(&[Role::Writer]);
    let owner = Registry::at(lab.paths[0].clone());
    owner.open().unwrap();
    owner.write(vec![other_machine(OTHER, "m8")]).unwrap();
    let handle = Handle::at(lab.paths[0].clone(), &REGISTRY);
    assert!(
        !handle
            .view()
            .unwrap()
            .project(&CLI)
            .rows
            .contains_key("grok"),
        "no seed pass wrote the CLI rows here"
    );
    let dangling = |paths: &Paths| {
        let Output::Json(checked) = run(paths, &["check", "alab/registry"]) else {
            panic!("check prints JSON");
        };
        checked["findings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|f| {
                f["finding"] == json!("dangling")
                    && f["pk"] == json!(OTHER)
                    && f["column"] == json!("clis")
            })
            .count()
    };

    let exported = export_machines(&lab.paths[0]);
    let row = record(&exported.text, OTHER);
    let unknown = type_in(&exported.text, row, "clis", "nonesuch");
    let found = problems(handle.save(&MACHINE, &exported.base, &unknown).unwrap());
    assert_eq!(found[0].rule, "machine.clis: no such cli row");

    let compiled = type_in(&exported.text, row, "clis", "grok");
    let Saved::Written { applied, .. } = handle.save(&MACHINE, &exported.base, &compiled).unwrap()
    else {
        panic!("a compiled seed row may be named");
    };
    assert_eq!(applied.cells, 1);
    assert_eq!(dangling(&lab.paths[0]), 1, "check reports it unseeded");
    owner.seed().unwrap();
    assert_eq!(
        dangling(&lab.paths[0]),
        0,
        "a seed pass writes every named row"
    );
}

/// The resolution's digest separates two resolutions that bind one (roster, writer) apart: the
/// writer seated as another machine, or as none, digests otherwise, so the shared files' key
/// moves with any binding (`file::materialize`'s `key`). Two Writers write the registry, and
/// Mac 0's resolution of it binds each to its machine.
///
/// NEGATIVE CONTROL (2026-11-14): with `Resolved::digest` passing over its bindings (the loop
/// that hashes them removed), each rebound resolution digested as the first:
/// `assertion `left != right` failed: another machine`.
#[test]
fn the_resolved_digest_separates_any_binding() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    Registry::at(lab.paths[0].clone()).open().unwrap();
    let here = Handle::at(lab.paths[0].clone(), &REGISTRY);
    append_history_at(
        &here,
        Command::Write {
            request: [91; 16],
            changes: machine(MC),
        },
        2_000,
    );
    repair_new_registry(&lab);
    append_history_at(
        &Handle::at(lab.paths[1].clone(), &REGISTRY),
        Command::Write {
            request: [92; 16],
            changes: vec![put(&MACHINE, MC, "status", json!("parked"))],
        },
        3_000,
    );
    repair_registry(&lab, 6);
    let table = here.compiled.table;
    let resolved =
        application::observe_store(&here.paths, here.descriptor(), |session, store, scope| {
            let log = Log::read_with(session, store, scope, decode)?;
            Resolved::read(table, session, &log)
        })
        .unwrap();
    let writers: Vec<(Digest, Actor)> = resolved.writers().copied().collect();
    assert_eq!(
        writers
            .iter()
            .map(|(_, writer)| *writer)
            .collect::<BTreeSet<_>>()
            .len(),
        2,
        "CONTROL: both Writers' operations are eligible here: {writers:?}"
    );
    let first = resolved.digest();
    assert_eq!(first, resolved.clone().digest(), "a digest is a function");
    for (roster, writer) in writers {
        for (machine, why) in [(Some([0xee; 16]), "another machine"), (None, "no seat")] {
            let rebound = resolved.clone().rebound(roster, writer, machine);
            assert_ne!(rebound.digest(), first, "{why}");
        }
    }
}

/// A table's history reads in pages. Paging with a cursor serves every entry once, in history's
/// order; the filters keep a page to one collection, row, column, writer or kind; each entry
/// carries the kind aterm-db decides (a facts-only record is aterm's, a mixed record the first
/// kind that holds, a revert explicit whatever it restores) and says when it projects nothing
/// (an owned row at a key not its signer's, an automatic record past its writer's quota). The
/// revision probe answers the page's revision until a record arrives; a peer's late record
/// sorts inside the range already served, so the next page asked at the old revision says it
/// moved and starts again from the top, where a page asked without one would skip that record.
/// NEGATIVE CONTROL: with `history::page` never setting `moved` (`let moved = false;`), the page
/// after the cursor resumes past the late record: `` assertion failed: next.moved ``.
#[test]
fn history_reads_one_page_after_a_cursor_and_reports_a_moved_revision() {
    use crate::{
        alab::registry::{MACHINE_FACTS, SELF_QUOTA},
        att::history::{Ask, EntryKind, Filter, Inert, Writer},
        typed::cell::{CellKey, CellValue},
    };
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let registry = Registry::at(lab.paths[0].clone());
    registry.open().unwrap();
    let handle = Handle::at(lab.paths[0].clone(), &REGISTRY);
    let facts = |machine: [u8; 16]| Change {
        collection: MACHINE_FACTS.name.into(),
        key: keys::uuid32(&machine),
        value: Some(json!({"r": 1, "n": 1, "mac": MC, "label": "m9"})),
    };
    let marked = |col: &str, value: CellValue| {
        cell::change(
            &MACHINE,
            CellKey {
                pk: MC,
                col,
                elem: None,
            },
            &value,
        )
        .unwrap()
    };
    let source = "sr-0123456789abcdef";
    let own = vec![facts(lab.machine(0))];
    let foreign = vec![facts(lab.machine(1))];
    let imported = vec![marked(
        "location",
        CellValue::imported(1, json!("rack c"), source),
    )];
    // The page's own closure let the last flagged cell win: import here.
    let mixed = vec![
        marked("note", CellValue::automatic(1, json!("seen"))),
        marked("status", CellValue::imported(1, json!("parked"), source)),
    ];
    let explicit = vec![put(&MACHINE, MC, "location", json!("rack d"))];
    // No cell's source: a value that does not read as a cell, and a collection this build does
    // not compile.
    let not_a_source = vec![Change {
        collection: MACHINE.name.into(),
        key: format!("{MC}.note"),
        value: Some(json!({"r": 1, "x": "seen", "s": "not-a-source"})),
    }];
    let uncompiled = vec![Change {
        collection: "later".into(),
        key: "k".into(),
        value: Some(json!({"s": source})),
    }];
    registry
        .write(vec![
            not_a_source.clone(),
            uncompiled.clone(),
            machine(MC),
            own.clone(),
            imported.clone(),
            mixed.clone(),
            foreign.clone(),
            explicit.clone(),
        ])
        .unwrap();
    let written = |history: &[Entry], changes: &[Change]| {
        history
            .iter()
            .find(|entry| entry.changes == changes)
            .unwrap()
            .clone()
    };
    let target = written(&handle.history().unwrap(), &explicit).operation;
    // The revert restores the imported `rack c`, a value that carries a source.
    handle.revert([91; 16], target).unwrap();
    // Enough automatic records to pass this Mac's quota.
    registry
        .write(
            (0..SELF_QUOTA)
                .map(|n| {
                    vec![marked(
                        "note",
                        CellValue::automatic(1, json!(format!("seen {n}"))),
                    )]
                })
                .collect(),
        )
        .unwrap();

    let history = handle.history().unwrap();
    let kind = |changes: &[Change]| written(&history, changes).kind;
    assert_eq!(kind(&machine(MC)), EntryKind::Explicit);
    assert_eq!(
        kind(&own),
        EntryKind::Aterm,
        "a facts-only record is aterm's"
    );
    assert_eq!(kind(&imported), EntryKind::Import);
    assert_eq!(kind(&mixed), EntryKind::Aterm, "the first kind that holds");
    assert_eq!(kind(&explicit), EntryKind::Explicit);
    assert_eq!(kind(&not_a_source), EntryKind::Explicit, "no cell's source");
    assert_eq!(kind(&uncompiled), EntryKind::Explicit, "no cell's source");
    let revert = history
        .iter()
        .find(|entry| entry.reverts == Some(target))
        .unwrap();
    assert!(
        revert
            .changes
            .iter()
            .any(|change| change.value.as_ref().is_some_and(|v| v.get("s").is_some())),
        "the revert restores an imported value"
    );
    assert_eq!(revert.kind, EntryKind::Explicit, "a revert is explicit");
    assert_eq!(written(&history, &own).inert, None);
    assert_eq!(written(&history, &foreign).inert, Some(Inert::Rows));
    let view = handle.view().unwrap();
    assert!(!view.quota.over.is_empty());
    for entry in &history {
        assert_eq!(
            entry.inert == Some(Inert::Quota),
            view.quota.over.contains(&entry.operation),
            "an entry is inert by quota exactly where the view's quota says so"
        );
    }

    // Pages of two, each after the last one's cursor, serve the whole history once, in order.
    let first = Ask::first(Filter::default(), 2);
    let mut ask = Some(first.clone());
    let mut served = Vec::new();
    let mut revision = None;
    while let Some(asked) = ask {
        let page = handle.history_page(&asked).unwrap();
        assert!(!page.moved);
        assert!(page.entries.len() <= 2);
        assert_eq!(*revision.get_or_insert(page.revision), page.revision);
        if page.next.is_some() {
            assert_eq!(page.next, page.entries.last().map(Entry::cursor));
        }
        served.extend(page.entries.iter().cloned());
        ask = asked.after(&page);
    }
    assert_eq!(served, history);
    let revision = revision.unwrap();
    assert_eq!(handle.revision().unwrap(), revision);

    // The filters, applied before a page is cut.
    let only = |filter: Filter| {
        handle
            .history_page(&Ask::first(filter, usize::MAX))
            .unwrap()
            .entries
    };
    let aterm = only(Filter {
        kind: Some(EntryKind::Aterm),
        ..Filter::default()
    });
    assert_eq!(
        aterm,
        history
            .iter()
            .filter(|entry| entry.kind == EntryKind::Aterm)
            .cloned()
            .collect::<Vec<_>>()
    );
    let facts_rows = only(Filter {
        collection: Some(MACHINE_FACTS.name.into()),
        ..Filter::default()
    });
    assert_eq!(
        facts_rows
            .iter()
            .map(|e| e.changes.clone())
            .collect::<Vec<_>>(),
        [own.clone(), foreign.clone()]
    );
    let locations = only(Filter {
        collection: Some(MACHINE.name.into()),
        row: Some(MC.into()),
        column: Some("location".into()),
        ..Filter::default()
    });
    assert_eq!(
        locations.len(),
        4,
        "the machine's birth, the import, the edit, the revert"
    );
    assert_eq!(
        only(Filter {
            writer: Some(Writer::Machine(lab.machine(1))),
            ..Filter::default()
        }),
        []
    );
    assert_eq!(
        only(Filter {
            writer: Some(Writer::Machine(lab.machine(0))),
            ..Filter::default()
        }),
        history
    );
    let one = handle
        .history_page(&Ask::first(
            Filter {
                column: Some("location".into()),
                ..Filter::default()
            },
            1,
        ))
        .unwrap();
    assert_eq!(one.entries, locations[..1]);
    assert_eq!(one.next, Some(locations[0].cursor()));

    // A peer's record that sorts inside what was served: it observed only the records the peer
    // held, and its writer's clock runs behind, so it lands before the cursor.
    repair_new_registry(&lab);
    let peer = Handle::at(lab.paths[1].clone(), &REGISTRY);
    let last_before = append_history_at(
        &handle,
        Command::Write {
            request: [92; 16],
            changes: vec![put(&MACHINE, MC, "label", json!("m10"))],
        },
        u64::MAX / 2,
    );
    append_history_at(
        &handle,
        Command::Write {
            request: [93; 16],
            changes: vec![put(&MACHINE, MC, "label", json!("m11"))],
        },
        u64::MAX / 2,
    );
    let late = append_history_at(
        &peer,
        Command::Write {
            request: [94; 16],
            changes: vec![put(&MACHINE, MC, "location", json!("rack e"))],
        },
        0,
    );
    let before = handle.history().unwrap();
    let at = before
        .iter()
        .position(|entry| entry.operation == last_before)
        .unwrap();
    let served = handle
        .history_page(&Ask::first(Filter::default(), at + 1))
        .unwrap();
    assert_eq!(served.entries, before[..=at]);
    let old = served.revision;
    assert_ne!(old, revision, "two records arrived");
    assert_eq!(handle.revision().unwrap(), old);
    repair_registry(&lab, 2);
    let moved = handle.revision().unwrap();
    assert_ne!(moved, old, "the peer's record arrived");
    let after = handle.history().unwrap();
    let landed = after
        .iter()
        .position(|entry| entry.operation == late)
        .unwrap();
    assert!(
        landed <= at,
        "the late record sorts inside the range served"
    );
    let next = handle
        .history_page(&Ask::first(Filter::default(), 3).after(&served).unwrap())
        .unwrap();
    assert!(next.moved);
    assert_eq!(next.revision, moved);
    assert_eq!(next.entries, after[..3], "a moved page starts from the top");
    // A reader that does not say its revision is served after the cursor, past the late record.
    let blind = handle
        .history_page(&Ask {
            revision: None,
            ..Ask::first(Filter::default(), 3).after(&served).unwrap()
        })
        .unwrap();
    assert!(!blind.moved);
    assert!(!blind.entries.iter().any(|entry| entry.operation == late));
}

/// Seal, on Mac `from`, a typed write whose causal parent never arrives anywhere, and give it to
/// Mac `to`: the record a slow or partitioned peer leaves waiting (`Log::awaited`).
fn withhold_on(lab: &Lab, from: usize, to: usize, changes: Vec<crate::database::Change>) {
    let command = Command::Write {
        request: crate::crypto::random().unwrap(),
        changes,
    };
    let missing = [91; 32];
    let wire = application::with_store(
        &lab.paths[from],
        false,
        Handle::at(lab.paths[from].clone(), &REGISTRY).descriptor(),
        |session, _, scope| {
            crate::table::tests::seal(
                session,
                scope,
                crate::crypto::Causal {
                    sequence: 1_000,
                    previous: Some(missing),
                    parents: &[missing],
                },
                &command.encode()?,
            )
        },
    )
    .unwrap();
    application::with_store(
        &lab.paths[to],
        false,
        Handle::at(lab.paths[to].clone(), &REGISTRY).descriptor(),
        |session, store, _| store.ingest(session, &wire),
    )
    .unwrap();
}

/// A record still waiting on a causal parent holds back only a write of a row it writes (TK-08):
/// Mac 0 holds Mac 1's withheld edit of one machine row, so a grid save of that row waits and
/// says how many records are withheld, while a save of another row is written at once and the
/// withheld record, when its parent arrives, is concurrent with it rather than hidden by it.
/// NEGATIVE CONTROL: with `apply` refusing on `!view.complete` (as before), the save of the other
/// row fails too: `` called `Result::unwrap()` on an `Err` value: Error("this table's history
/// here is incomplete; a grid save waits for a complete scan") ``.
#[test]
fn a_withheld_record_holds_back_only_the_rows_it_touches() {
    const OTHER: &str = "mc-000000000000000a";
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let owner = Registry::at(lab.paths[0].clone());
    owner.open().unwrap();
    let mut other = machine(OTHER);
    other[0] = put(&MACHINE, OTHER, "label", json!("m10"));
    owner.write(vec![machine(MC), other]).unwrap();
    repair_new_registry(&lab);
    withhold_on(
        &lab,
        1,
        0,
        vec![put(&MACHINE, MC, "location", json!("rack w"))],
    );
    let handle = Handle::at(lab.paths[0].clone(), &REGISTRY);
    let view = handle.view().unwrap();
    assert!(!view.complete);
    assert_eq!(view.awaiting.records, 1);
    assert_eq!(
        view.awaiting.rows,
        Some(BTreeSet::from([(MACHINE.name.to_owned(), MC.to_owned())]))
    );

    // Another row: written now.
    let exported = export_machines(&lab.paths[0]);
    let saved = type_in(
        &exported.text,
        record(&exported.text, OTHER),
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
    let applied = handle.apply(&MACHINE, &plan).unwrap();
    assert_eq!((applied.records, applied.cells), (1, 1));
    let after = handle.view().unwrap();
    assert_eq!(
        after.awaiting.records, 1,
        "the save names only records it read as parents, so it waits on nothing"
    );
    let rows = after.project(&MACHINE).rows;
    assert_eq!(rows[OTHER].get("location"), Some(&json!("rack b")));

    // The withheld record's row: waits, naming the count and the row, and writes nothing.
    let exported = export_machines(&lab.paths[0]);
    let saved = type_in(
        &exported.text,
        record(&exported.text, MC),
        "location",
        "rack c",
    );
    let plan = sheet::plan(
        &registry::REGISTRY,
        &MACHINE,
        &exported.base,
        &saved,
        &no_birth,
    )
    .unwrap();
    let error = handle.apply(&MACHINE, &plan).unwrap_err().to_string();
    assert!(error.contains("history incomplete (1 withheld)"), "{error}");
    assert!(error.contains(&format!("machine row {MC}")), "{error}");
    let rows = handle.view().unwrap().project(&MACHINE).rows;
    assert_eq!(rows[MC].get("location"), Some(&json!("rack a")));
}

/// A withheld record this Mac cannot read (no command: its rows are unknown) holds back every
/// write, and a later generation's restated history still arriving holds back every write too;
/// with nothing withheld, nothing is held back.
#[test]
fn an_unreadable_or_unrestated_wait_holds_back_every_row() {
    let rows = vec![vec![put(&MACHINE, MC, "location", json!("rack b"))]];
    let mut view = View::empty_for_test();
    assert_eq!(view.holds_back(&rows), None);
    view.complete = false;
    view.awaiting = crate::typed::Awaiting {
        records: 2,
        rows: None,
        restated: true,
    };
    let why = view.holds_back(&rows).unwrap();
    assert!(why.starts_with("history incomplete (2 withheld)"), "{why}");
    view.awaiting = crate::typed::Awaiting {
        records: 0,
        rows: Some(BTreeSet::new()),
        restated: false,
    };
    let why = view.holds_back(&rows).unwrap();
    assert!(why.contains("restated history is still arriving"), "{why}");
    view.awaiting = crate::typed::Awaiting {
        records: 1,
        rows: Some(BTreeSet::from([(
            MACHINE.name.to_owned(),
            "mc-000000000000000b".to_owned(),
        )])),
        restated: true,
    };
    assert_eq!(view.holds_back(&rows), None);
}

/// `aterm db alab check` reports every finding `aterm db table check` reports of the registry,
/// so the Tables page's Check view, which lists alab's, misses none: two Macs that state one
/// version of the machine table's columns apart from each other are a `schema-split` in both,
/// and alab's `--tsv` says it in words.
/// NEGATIVE CONTROL: with `alab::command::checked` not extending its findings by
/// `command::schema_findings`, alab names no split: `` alab check reports what table check
/// reports ``.
#[test]
fn alab_check_reports_every_finding_table_check_reports() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let owner = Registry::at(lab.paths[0].clone());
    owner.open().unwrap();
    owner.write(vec![machine(MC)]).unwrap();
    repair_new_registry(&lab);
    // Each Mac states the machine table's columns at one version, each its own way, before
    // either sees the other's.
    for (n, note) in ["here", "there"].into_iter().enumerate() {
        let mut row = MACHINE.schema_row();
        row.as_object_mut()
            .and_then(|row| row.get_mut("x"))
            .and_then(Value::as_object_mut)
            .expect("a schema row's columns")
            .insert("note".to_owned(), json!(note));
        Registry::at(lab.paths[n].clone())
            .write(vec![vec![crate::database::Change {
                collection: crate::typed::SCHEMA.to_owned(),
                key: MACHINE.name.to_owned(),
                value: Some(row),
            }]])
            .unwrap();
    }
    repair_registry(&lab, 2);
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
    let split = json!({"finding": "schema-split", "collection": MACHINE.name,
        "version": MACHINE.version});
    let table = table["findings"].as_array().unwrap();
    assert!(table.contains(&split), "{table:?}");
    let reported = alab["findings"].as_array().unwrap();
    for found in table {
        assert!(
            reported.contains(found),
            "alab check reports what table check reports: {found} in {reported:?}"
        );
    }
    let Value::String(tsv) = crate::alab::command::run(
        &lab.paths[0],
        &["check".to_owned(), "--tsv".to_owned()],
        None,
        &mut |_| panic!("check opens no review"),
        0,
    )
    .unwrap() else {
        panic!("check --tsv prints lines");
    };
    let line = format!(
        "schema-split\tmachine\t\t\t{}\tversion {}",
        crate::typed::check::said("schema-split"),
        MACHINE.version
    );
    assert!(tsv.lines().any(|l| l == line), "{tsv}");
    // `aterm db table check --tsv` says it in the same words.
    let Output::Text(table_tsv) = run(&lab.paths[0], &["check", "alab/registry", "--tsv"]) else {
        panic!("check --tsv prints lines");
    };
    assert!(table_tsv.lines().any(|l| l == line), "{table_tsv}");
}

/// A typed table's checkpoint pass (tracker LT-B10B): fenced while a member reports a build that
/// cannot read `ATCKP1`; past the fence, Mac 0 proposes a checkpoint of the records at least 14
/// days old and Mac 1, which recomputes the same body from its own replica, seconds it, so both
/// seats sign one body and it is final; a pass with nothing new signs nothing; and the table
/// reads exactly as before, with the checkpoint records neither projected nor refused.
/// NEGATIVE CONTROL: with `MetadataEvidence::is_table_control` recognizing only cutoffs, the
/// document log decodes each checkpoint as a command and refuses it: `a checkpoint is never
/// refused` with two record ids on the left.
#[test]
fn a_registry_checkpoint_waits_on_the_fence_then_two_seats_sign_one_body() {
    use super::checkpoint::{AGE_MS, Signed};
    use aterm_secrets::commons::{
        finality,
        hearing::{self, Ledger, Said},
    };
    const DAY: u64 = 24 * 60 * 60 * 1000;
    // Records written 20 days ago: old enough to go below a cut, recent enough that their
    // writers are current.
    let now = aterm_time::unix_millis().unwrap();
    let then = now - 20 * DAY;
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    Registry::at(lab.paths[0].clone()).open().unwrap();
    let here = Handle::at(lab.paths[0].clone(), &REGISTRY);
    append_history_at(
        &here,
        Command::Write {
            request: [91; 16],
            changes: machine(MC),
        },
        then + 1_000,
    );
    repair_new_registry(&lab);
    let there = Handle::at(lab.paths[1].clone(), &REGISTRY);
    append_history_at(
        &there,
        Command::Write {
            request: [92; 16],
            changes: vec![put(&MACHINE, MC, "status", json!("parked"))],
        },
        then + 2_000,
    );
    repair_registry(&lab, 2);
    append_history_at(
        &here,
        Command::Write {
            request: [93; 16],
            changes: vec![put(&MACHINE, MC, "nickname", json!("spare"))],
        },
        then + 3_000,
    );
    repair_registry(&lab, 2);
    let before = here.view().unwrap();

    assert!(then + 3_000 + AGE_MS < now);
    let bootstrap = lab.bootstrap(0);
    let access = bootstrap.access().unwrap().unwrap();
    let seats: std::collections::BTreeSet<Actor> = access
        .members()
        .iter()
        .map(|member| member.binding().actor())
        .collect();
    let heard_at = |version| {
        let mut ledger = Ledger::new(bootstrap.trust());
        for seat in &seats {
            ledger.record(*seat, now, Said::Version(version));
        }
        ledger
    };
    let older = heard_at(hearing::Version(0, 202, 0));
    assert_eq!(
        here.checkpoint_every(&older, now, 1).unwrap(),
        Signed::Fenced
    );
    let ready = heard_at(hearing::READS_CHECKPOINTS);
    assert_eq!(
        here.checkpoint_every(&ready, now, 1).unwrap(),
        Signed::Proposed
    );
    assert_eq!(
        here.checkpoint_every(&ready, now, 1).unwrap(),
        Signed::Nothing
    );
    repair_registry(&lab, 2);
    assert_eq!(
        there.checkpoint_every(&ready, now, 1).unwrap(),
        Signed::Seconded
    );
    assert_eq!(
        there.checkpoint_every(&ready, now, 1).unwrap(),
        Signed::Nothing
    );
    repair_registry(&lab, 2);

    let shares = application::observe_store(&lab.paths[0], here.descriptor(), |_, store, _| {
        Ok(store
            .checkpoints()
            .map(|(signer, body)| (signer, body.clone()))
            .collect::<Vec<_>>())
    })
    .unwrap();
    assert_eq!(shares.len(), 2, "one share from each seat");
    assert_eq!(shares[0].1, shares[1].1, "one body");
    assert!(shares[0].1.below > 0);
    let signed = shares.iter().map(|(signer, _)| *signer).collect();
    assert!(finality::is_final(&seats, &signed));

    let after = here.view().unwrap();
    assert_eq!(after.cells, before.cells);
    assert_eq!(after.owned, before.owned);
    assert_eq!(
        after.refused, before.refused,
        "a checkpoint is never refused"
    );
}

/// The names `names` gives the operations of `held`, in order; `?` for one it does not name.
fn named(names: &[(&'static str, Digest)], held: &[Held]) -> Vec<&'static str> {
    held.iter()
        .map(|held| {
            names
                .iter()
                .find(|(_, id)| *id == held.operation)
                .map_or("?", |(name, _)| *name)
        })
        .collect()
}

/// What `operation` wrote at `key` of `collection`, what that replaced and what it had not
/// seen, each as [`named`] names it.
fn wrote_at(
    handle: &Handle,
    names: &[(&'static str, Digest)],
    operation: Digest,
    collection: &str,
    key: &str,
) -> [Vec<&'static str>; 3] {
    let wrote = handle.wrote(operation).unwrap();
    let cell = wrote
        .iter()
        .find(|cell| cell.collection == collection && cell.key == key)
        .unwrap_or_else(|| panic!("the operation wrote {key}"));
    [
        named(names, &cell.wrote),
        named(names, &cell.replaced),
        named(names, &cell.unseen),
    ]
}

/// A sealed secret token of an earlier build, which no store here opens.
fn sealed_token() -> String {
    let token = format!(
        "sec1:{}:{}",
        "ab".repeat(32),
        aterm_secrets::b64::encode_padded(&[7; 40])
    );
    assert!(crate::typed::secret::is_sealed(&token));
    token
}

/// `changes` signed by `handle`'s Mac as its next record of the handle's generation and never
/// ingested there: the operation and its wire, which only a roll's archived row then carries.
fn sealed_record(
    handle: &Handle,
    request: u8,
    changes: Vec<Change>,
    authored_ms: u64,
) -> (Digest, Vec<u8>) {
    application::with_store(
        &handle.paths,
        false,
        handle.descriptor(),
        |session, store, scope| {
            let command = Command::Write {
                request: [request; 16],
                changes,
            };
            let wire = seal_history_at(session, store, scope, &command, authored_ms)?;
            Ok((session.authenticate(scope, &wire)?.header().id, wire))
        },
    )
    .unwrap()
}

/// `handle`'s Mac archives `record`, the first generation's operation whose wire is `wire`, as a
/// roll does: the operation that archived it, authored at `request` seconds.
fn archive(handle: &Handle, request: u8, record: Digest, wire: &[u8]) -> Digest {
    let pk = keys::archived_pk(&record);
    let archived = |col: &str, x: Value| {
        cell::change(
            &registry::ARCHIVED,
            cell::CellKey {
                pk: &pk,
                col,
                elem: None,
            },
            &cell::CellValue::owner(registry::ARCHIVED.version, x),
        )
        .unwrap()
    };
    append_history_at(
        handle,
        Command::Write {
            request: [request; 16],
            changes: vec![
                archived("generation", json!(1)),
                archived("wire", json!(aterm_secrets::b64::encode_padded(wire))),
                archived("row", json!("live")),
            ],
        },
        u64::from(request) * 1_000,
    )
}

/// The cover cell that makes the Mac `by` a roller of the first generation: the word a carried
/// history stands on (`alab::roll::rollers`), which any Writer may write for itself.
fn cover(by: &str) -> Change {
    cell::change(
        &registry::ROLL,
        cell::CellKey {
            pk: &keys::roll_pk(1, by),
            col: "from",
            elem: None,
        },
        &cell::CellValue::owner(registry::ROLL.version, json!(1)),
    )
    .unwrap()
}

/// One history row in which the Mac `by` states that `operation` of the first generation, by
/// `actor` on `machine` at the clock `ms`, made `changes`, each spelled as a carried entry's
/// body spells it (`alab::history`): any statement, true or not, as any roller may write one.
fn stated(
    by: &str,
    operation: Digest,
    (actor, machine): (&[u8], &[u8; 16]),
    ms: u64,
    trimmed: bool,
    changes: Value,
) -> Change {
    let body = json!({
        "v": 1, "by": by, "o": hex::encode_lower(&operation), "g": 1,
        "a": hex::encode_lower(actor), "m": hex::encode_lower(machine),
        "t": ms, "h": [ms, 0], "q": null, "r": null, "z": trimmed, "c": changes,
    });
    cell::change(
        &registry::HISTORY,
        cell::CellKey {
            pk: &keys::history_pk(by, &operation),
            col: "entry",
            elem: None,
        },
        &cell::CellValue::owner(
            registry::HISTORY.version,
            json!(aterm_secrets::b64::encode_padded(
                &aterm_json::to_vec(&body).unwrap()
            )),
        ),
    )
    .unwrap()
}

/// A cell lists every value it held with the write that put each there, and a write lists what
/// it replaced and the writes it had not seen. Mac 0 writes a row (`born`); Mac 1 then sets its
/// status (`parked`) while Mac 0, concurrently and with a clock claim older than Mac 1's, sets
/// another status and takes a set member out (`lost`). The status then lists the three writes in
/// history order, each with its writer, machine, clocks and stored value as history shows them,
/// and the two concurrent ones stand. Neither of those is "earlier" (their order is the digest's
/// where their clocks tie), so each names the other as unseen and `born` as what it replaced;
/// `born` replaced and missed nothing. After both meet, Mac 0 sets the status again (`later`):
/// it alone stands, it replaced both and missed neither, and the two still name each other. A
/// peer that holds the same operations answers the same.
/// NEGATIVE CONTROL: with `Reader::wrote` calling unseen only a write earlier in history order
/// (`(other.hlc, other.id) < (write.hlc, write.id)` in place of `!observes(other, operation)`),
/// only the later of the two concurrent writes names the other; here `lost` was the earlier:
/// `` left: [["lost"], ["born"], []] right: [["lost"], ["born"], ["parked"]] ``. With every
/// observed write called replaced (`Reader::tops`' test against the ones already found
/// removed), `later` replaced `born` too: `` left: [["later"], ["born", "lost", "parked"], []] ``.
#[test]
fn a_cell_lists_every_value_it_held_and_a_write_lists_what_it_had_not_seen() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    Registry::at(lab.paths[0].clone()).open().unwrap();
    let here = Handle::at(lab.paths[0].clone(), &REGISTRY);
    let born = append_history_at(
        &here,
        Command::Write {
            request: [81; 16],
            changes: machine(MC),
        },
        2_000,
    );
    repair_new_registry(&lab);
    let there = Handle::at(lab.paths[1].clone(), &REGISTRY);
    let parked = append_history_at(
        &there,
        Command::Write {
            request: [82; 16],
            changes: vec![put(&MACHINE, MC, "status", json!("parked"))],
        },
        3_000,
    );
    let lost = append_history_at(
        &here,
        Command::Write {
            request: [83; 16],
            changes: vec![
                put(&MACHINE, MC, "status", json!("lost")),
                take_out(MC, "clis", "codex"),
            ],
        },
        1_000,
    );
    repair_registry(&lab, 2);
    let mut names = vec![("born", born), ("parked", parked), ("lost", lost)];
    let status = format!("{MC}.status");
    let codex = format!("{MC}.clis.codex");
    let stored = |word: &str| put(&MACHINE, MC, "status", json!(word)).value;
    let standing = |held: &[Held]| {
        held.iter()
            .filter(|held| held.stands)
            .map(|held| held.operation)
            .collect::<BTreeSet<_>>()
    };

    // The three writes, in history's order, each as history shows it.
    let history = here.history().unwrap();
    let order: Vec<Digest> = history
        .iter()
        .map(|entry| entry.operation)
        .filter(|id| [born, parked, lost].contains(id))
        .collect();
    assert_eq!(order[0], born, "both observed born");
    let values = here.cell_values(MACHINE.name, &status).unwrap();
    assert_eq!(
        values.iter().map(|held| held.operation).collect::<Vec<_>>(),
        order
    );
    for held in &values {
        let entry = history
            .iter()
            .find(|entry| entry.operation == held.operation)
            .unwrap();
        assert_eq!(
            (held.source, held.actor, held.machine, held.hlc),
            (Source::Log, entry.actor, entry.machine, entry.hlc)
        );
        assert_eq!(
            (held.reverts, held.withheld, held.inert),
            (None, false, None)
        );
        let (claimed, word) = match held.operation {
            id if id == born => (2_000, "active"),
            id if id == parked => (3_000, "parked"),
            _ => (1_000, "lost"),
        };
        assert_eq!((held.authored_ms, &held.value), (claimed, &stored(word)));
    }
    let machine_of = |id: Digest| {
        values
            .iter()
            .find(|held| held.operation == id)
            .unwrap()
            .machine
    };
    assert!(machine_of(parked).is_some());
    assert_ne!(machine_of(parked), machine_of(lost), "two Macs wrote");
    assert_eq!(standing(&values), BTreeSet::from([parked, lost]));

    // Each concurrent write replaced born and had not seen the other.
    assert_eq!(
        wrote_at(&here, &names, lost, MACHINE.name, &status),
        [vec!["lost"], vec!["born"], vec!["parked"]]
    );
    assert_eq!(
        wrote_at(&here, &names, parked, MACHINE.name, &status),
        [vec!["parked"], vec!["born"], vec!["lost"]]
    );
    // A set's member is its own register: lost took it out, and nothing stood beside that.
    assert_eq!(
        wrote_at(&here, &names, lost, MACHINE.name, &codex),
        [vec!["lost"], vec!["born"], vec![]]
    );
    assert_eq!(
        here.wrote(parked).unwrap().len(),
        1,
        "parked wrote one cell"
    );
    // born wrote the row's six cells, replaced nothing and missed nothing.
    let first = here.wrote(born).unwrap();
    assert_eq!(first.len(), 6);
    assert!(first.iter().all(|cell| {
        named(&names, &cell.wrote) == ["born"] && cell.replaced.is_empty() && cell.unseen.is_empty()
    }));

    // A later write that saw both: it alone stands, and it replaced both.
    let later = append_history_at(
        &here,
        Command::Write {
            request: [84; 16],
            changes: vec![put(&MACHINE, MC, "status", json!("active"))],
        },
        4_000,
    );
    repair_registry(&lab, 2);
    names.push(("later", later));
    let values = here.cell_values(MACHINE.name, &status).unwrap();
    assert_eq!(
        values.iter().map(|held| held.operation).collect::<Vec<_>>(),
        [order.as_slice(), &[later]].concat()
    );
    assert_eq!(standing(&values), BTreeSet::from([later]));
    let concurrent: Vec<&str> = order[1..]
        .iter()
        .map(|id| if *id == lost { "lost" } else { "parked" })
        .collect();
    assert_eq!(
        wrote_at(&here, &names, later, MACHINE.name, &status),
        [vec!["later"], concurrent, vec![]]
    );
    assert_eq!(
        wrote_at(&here, &names, parked, MACHINE.name, &status),
        [vec!["parked"], vec!["born"], vec!["lost"]]
    );
    // A peer answers the same, a cell nothing wrote lists nothing, and an operation this table
    // does not hold is refused.
    assert_eq!(there.cell_values(MACHINE.name, &status).unwrap(), values);
    assert_eq!(there.wrote(lost).unwrap(), here.wrote(lost).unwrap());
    assert!(
        here.cell_values(MACHINE.name, &format!("{MC}.nickname"))
            .unwrap()
            .is_empty()
    );
    assert!(here.wrote([9; 32]).is_err());
}

/// A secret never leaves a cell's values: a write to a secret column is listed with no value
/// whatever it holds, and so is a write to any column whose value holds a sealed token, in the
/// cell's values and in what a later write replaced.
/// NEGATIVE CONTROL: with `shown` asking no secret rule (`alab::history::secret_change` not
/// asked), the stored values are listed: `a secret left the read`.
#[test]
fn a_cells_values_never_carry_a_secret() {
    const PLAIN: &str = "plain-words-in-a-secret-column";
    let token = sealed_token();
    let lab = Lab::new(&[Role::Writer]);
    Registry::at(lab.paths[0].clone()).open().unwrap();
    let handle = Handle::at(lab.paths[0].clone(), &REGISTRY);
    let password = "ad-0000000000000001.password".to_owned();
    let secret = |x: &str| Change {
        collection: ADDRESS.name.to_owned(),
        key: password.clone(),
        value: Some(cell::CellValue::owner(2, json!(x)).encode()),
    };
    let write = |request: u8, changes: Vec<Change>| {
        append_history_at(
            &handle,
            Command::Write {
                request: [request; 16],
                changes,
            },
            u64::from(request) * 1_000,
        )
    };
    let born = write(1, machine(MC));
    let sealed = write(2, vec![secret(&token)]);
    let plain = write(3, vec![secret(PLAIN)]);
    // Any writer may store any value: a token in a column that is no secret's.
    let location = format!("{MC}.location");
    let stray = write(
        4,
        vec![Change {
            collection: MACHINE.name.to_owned(),
            key: location.clone(),
            value: Some(cell::CellValue::owner(1, json!(token)).encode()),
        }],
    );
    let moved = write(5, vec![put(&MACHINE, MC, "location", json!("rack c"))]);

    let passwords = handle.cell_values(ADDRESS.name, &password).unwrap();
    let locations = handle.cell_values(MACHINE.name, &location).unwrap();
    let replaced = handle.wrote(moved).unwrap();
    let shown = format!(
        "{passwords:?}{locations:?}{replaced:?}{:?}{:?}{:?}",
        handle.wrote(sealed).unwrap(),
        handle.wrote(plain).unwrap(),
        handle.wrote(stray).unwrap()
    );
    assert!(
        !shown.contains(&token) && !shown.contains(PLAIN),
        "a secret left the read"
    );
    let listed = |held: &[Held]| {
        held.iter()
            .map(|held| {
                (
                    held.operation,
                    held.value.clone(),
                    held.withheld,
                    held.stands,
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        listed(&passwords),
        [(sealed, None, true, false), (plain, None, true, true)]
    );
    let rack = |x: &str| put(&MACHINE, MC, "location", json!(x)).value;
    assert_eq!(
        listed(&locations),
        [
            (born, rack("rack a"), false, false),
            (stray, None, true, false),
            (moved, rack("rack c"), false, true)
        ]
    );
    assert_eq!(listed(&replaced[0].replaced), [(stray, None, true, false)]);
}

/// An owned row's values are whole rows at its writer's key: Mac 0 writes its facts row twice,
/// and Mac 1 writes a row at Mac 0's key. The key lists Mac 0's two rows, the later standing,
/// and the second replaced the first; Mac 1's row, which no reader keeps, is not listed, is no
/// write to that key, and stays in history marked inert.
/// NEGATIVE CONTROL: with `Reader::records` keeping every change (`owned::kept` not asked), the
/// row Mac 1 forged is listed at Mac 0's key: `` left: [("first", 1, false), ("forged", 9, false),
/// ("second", 2, true)] right: [("first", 1, false), ("second", 2, true)] ``.
#[test]
fn an_owned_rows_values_are_its_own_macs_whole_rows() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    Registry::at(lab.paths[0].clone()).open().unwrap();
    let here = Handle::at(lab.paths[0].clone(), &REGISTRY);
    let me = keys::uuid32(&lab.machine(0));
    let row = |n: u64, label: &str| Change {
        collection: MACHINE_FACTS.name.into(),
        key: me.clone(),
        value: Some(json!({"r": 1, "n": n, "mac": "mc-0000000000000000", "label": label})),
    };
    let write = |handle: &Handle, request: u8, change: Change| {
        append_history_at(
            handle,
            Command::Write {
                request: [request; 16],
                changes: vec![change],
            },
            u64::from(request) * 1_000,
        )
    };
    let first = write(&here, 1, row(1, "m9"));
    repair_new_registry(&lab);
    let there = Handle::at(lab.paths[1].clone(), &REGISTRY);
    let forged = write(&there, 2, row(9, "m1"));
    repair_registry(&lab, 2);
    let second = write(&here, 3, row(2, "m10"));
    repair_registry(&lab, 2);
    let names = [("first", first), ("forged", forged), ("second", second)];

    for handle in [&here, &there] {
        let values = handle.cell_values(MACHINE_FACTS.name, &me).unwrap();
        let listed: Vec<_> = named(&names, &values)
            .into_iter()
            .zip(&values)
            .map(|(name, held)| {
                let n = held.value.as_ref().and_then(|row| row["n"].as_u64());
                (name, n.unwrap(), held.stands)
            })
            .collect();
        assert_eq!(listed, [("first", 1, false), ("second", 2, true)]);
        assert_eq!(values[1].value, row(2, "m10").value, "the whole row");
        assert!(
            values
                .iter()
                .all(|held| held.machine == Some(lab.machine(0)) && held.source == Source::Log)
        );
    }
    let wrote = here.wrote(second).unwrap();
    assert_eq!(wrote.len(), 1);
    assert_eq!(
        (wrote[0].collection.as_str(), wrote[0].key.as_str()),
        (MACHINE_FACTS.name, me.as_str())
    );
    assert_eq!(
        wrote_at(&here, &names, second, MACHINE_FACTS.name, &me),
        [vec!["second"], vec!["first"], vec![]]
    );
    assert!(
        here.wrote(forged).unwrap().is_empty(),
        "a row no reader keeps is no write to that key"
    );
    let history = here.history().unwrap();
    let inert = history.iter().find(|entry| entry.operation == forged);
    assert_eq!(inert.unwrap().inert, Some(Inert::Rows));
}

/// A revert is listed as a write with the operation it reverted and the value it restored, a
/// revert back to absence with no value, and each stands; a revert this log refuses (its keys
/// are not its target's) is no write and is not listed.
/// NEGATIVE CONTROL: with `Reader::held` leaving `reverts` out (`None` for every write), the
/// revert reads as a plain write: `` left: [("born", None, false), ("moved", None, false),
/// ("back", None, true)] right: [("born", None, false), ("moved", None, false), ("back",
/// Some("moved"), true)] ``.
#[test]
fn a_revert_is_listed_with_its_target_and_a_refused_one_is_not() {
    let lab = Lab::new(&[Role::Writer]);
    Registry::at(lab.paths[0].clone()).open().unwrap();
    let handle = Handle::at(lab.paths[0].clone(), &REGISTRY);
    let write = |request: u8, changes: Vec<Change>| {
        append_history_at(
            &handle,
            Command::Write {
                request: [request; 16],
                changes,
            },
            u64::from(request) * 1_000,
        )
    };
    let born = write(1, machine(MC));
    let moved = write(2, vec![put(&MACHINE, MC, "location", json!("rack b"))]);
    let called = write(3, vec![put(&MACHINE, MC, "nickname", json!("spare"))]);
    let back = handle.revert([91; 16], moved).unwrap();
    let uncalled = handle.revert([92; 16], called).unwrap();
    let names = [
        ("born", born),
        ("moved", moved),
        ("called", called),
        ("back", back),
        ("uncalled", uncalled),
    ];
    let name = |id: Digest| names.iter().find(|(_, held)| *held == id).unwrap().0;
    let listed = |key: &str| {
        let values = handle.cell_values(MACHINE.name, key).unwrap();
        let rows: Vec<_> = named(&names, &values)
            .into_iter()
            .zip(&values)
            .map(|(name_of, held)| (name_of, held.reverts.map(name), held.stands))
            .collect();
        (rows, values)
    };
    let location = format!("{MC}.location");
    let nickname = format!("{MC}.nickname");
    let check = || {
        let (rows, values) = listed(&location);
        assert_eq!(
            rows,
            [
                ("born", None, false),
                ("moved", None, false),
                ("back", Some("moved"), true)
            ]
        );
        assert_eq!(values[2].value, values[0].value, "the value it restored");
        assert!(values[2].value.is_some());
        let (rows, values) = listed(&nickname);
        assert_eq!(
            rows,
            [("called", None, false), ("uncalled", Some("called"), true)]
        );
        assert_eq!(
            (&values[1].value, values[1].withheld),
            (&None, false),
            "back to absence"
        );
    };
    check();
    assert_eq!(
        wrote_at(&handle, &names, back, MACHINE.name, &location),
        [vec!["back"], vec!["moved"], vec![]]
    );
    // A revert whose keys are not its target's is refused by every reader: no write.
    let refused = append_history_at(
        &handle,
        Command::Revert {
            request: [93; 16],
            operation: back,
            changes: vec![
                put(&MACHINE, MC, "location", json!("rack z")),
                put(&MACHINE, MC, "nickname", json!("junk")),
            ],
        },
        9_000,
    );
    check();
    assert!(handle.wrote(refused).is_err());
}

/// A set's member is its own register. Mac 0 writes a row with two members; then Mac 0 takes
/// one out while Mac 1, concurrently, puts it in again. That member lists the three writes, the
/// two concurrent ones standing and each unseen by the other; the other member lists only the
/// row's first write.
/// NEGATIVE CONTROL: with `Reader::records` matching a change by its row and column, the member
/// dropped from both keys, the member nothing else wrote lists the other member's writes too:
/// `` assertion `left == right` failed: the other member left: 3 right: 1 ``.
#[test]
fn a_set_members_values_are_its_own_register() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    Registry::at(lab.paths[0].clone()).open().unwrap();
    let here = Handle::at(lab.paths[0].clone(), &REGISTRY);
    let write = |handle: &Handle, request: u8, changes: Vec<Change>| {
        append_history_at(
            handle,
            Command::Write {
                request: [request; 16],
                changes,
            },
            u64::from(request) * 1_000,
        )
    };
    let born = write(&here, 1, machine(MC));
    repair_new_registry(&lab);
    let there = Handle::at(lab.paths[1].clone(), &REGISTRY);
    let out = write(&here, 2, vec![take_out(MC, "clis", "codex")]);
    let again = write(&there, 3, vec![elem(&MACHINE, MC, "clis", "codex")]);
    repair_registry(&lab, 2);
    let names = [("born", born), ("out", out), ("again", again)];

    let claude = here
        .cell_values(MACHINE.name, &format!("{MC}.clis.claude"))
        .unwrap();
    assert_eq!(claude.len(), 1, "the other member");
    assert_eq!((claude[0].operation, claude[0].stands), (born, true));
    let codex = format!("{MC}.clis.codex");
    let values = here.cell_values(MACHINE.name, &codex).unwrap();
    let mut listed: Vec<_> = named(&names, &values)
        .into_iter()
        .zip(&values)
        .map(|(name, held)| {
            let x = cell::CellValue::decode(held.value.as_ref().unwrap())
                .unwrap()
                .x;
            (name, x, held.stands)
        })
        .collect();
    assert_eq!(listed[0], ("born", json!(true), false));
    listed.sort_by_key(|(name, ..)| *name);
    assert_eq!(
        listed,
        [
            ("again", json!(true), true),
            ("born", json!(true), false),
            ("out", json!(false), true)
        ]
    );
    assert_eq!(
        wrote_at(&here, &names, out, MACHINE.name, &codex),
        [vec!["out"], vec!["born"], vec!["again"]]
    );
    assert_eq!(
        wrote_at(&here, &names, again, MACHINE.name, &codex),
        [vec!["again"], vec!["born"], vec!["out"]]
    );
}

/// An automatic record past its writer's quota never projects, so it never stands, replaced
/// nothing and is in no write's replaced or unseen list: it is listed in its cell's values,
/// marked. Mac 0 writes a row; Mac 1 spends its quota and then writes the row's location
/// automatically, past it; Mac 0, having seen that, sets the location. Mac 0's write replaced the
/// row's first value, not the one that never counted.
/// NEGATIVE CONTROL: with `Record::counts` answering `true` for every record, the owner's write
/// replaced the past-quota one: `` left: [["target"], ["excess"], []] right: [["target"],
/// ["born"], []] ``.
#[test]
fn a_write_past_its_quota_is_listed_and_replaced_nothing() {
    let (_lab, handle, target, excess) = quota_revert_pair(false);
    let view = handle.view().unwrap();
    let born = view
        .ops
        .iter()
        .find(|op| op.changes == machine(MC))
        .unwrap();
    let names = [("born", born.id), ("target", target), ("excess", excess)];
    let location = format!("{MC}.location");
    let values = handle.cell_values(MACHINE.name, &location).unwrap();
    assert_eq!(
        named(&names, &values)
            .into_iter()
            .zip(&values)
            .map(|(name, held)| (name, held.inert, held.stands))
            .collect::<Vec<_>>(),
        [
            ("born", None, false),
            ("excess", Some(Inert::Quota), false),
            ("target", None, true)
        ]
    );
    assert_eq!(
        wrote_at(&handle, &names, target, MACHINE.name, &location),
        [vec!["target"], vec!["born"], vec![]]
    );
    assert_eq!(
        wrote_at(&handle, &names, excess, MACHINE.name, &location),
        [vec!["excess"], vec![], vec![]]
    );
}

/// A cell's values read through a registry roll. The roll restates a register as one carried
/// value and carries the old log's entries as history rows; in the new generation the cell lists
/// the old generation's writes under their own operations, with the writers and clocks that log
/// gave them, the last standing, a write that held a sealed token still with no value, and a
/// write of the new generation replaced the one that stood. A revert of that write restores the
/// carried register: it is listed with its target and the one value the register holds, and it
/// stands, not the write the register names. A carried candidate no history row names is listed
/// under its operation with the writer and clocks of the record that restated it, with no value
/// where it holds a sealed token; a revert that restores a register of two candidates is listed
/// as that register. An owned row archived as its signed record, which no history
/// row names, is listed with the clocks of the record that archived it, and so is an earlier
/// row of that key archived after it, which does not stand; the key's first row of the new
/// generation replaced the one that stood, not the one archived last. A row retired and made
/// live again in the new generation re-asserts its label: the carried write stands.
/// NEGATIVE CONTROL: with `Reader::held` listing a restatement as its own record's write (the
/// carried register not read), the roll's record is listed after the carried entries: `` left:
/// ["born", "stray", "moved", "?"] right: ["born", "stray", "moved"] ``. With `Reader::cell`
/// leaving out the archived rows, the owned key lists nothing: `` left: [] right: ["facts"] ``.
/// With `Reader::archived` keeping only the archived row that stands, the earlier row is not
/// listed: `` left: [("facts", Restated { by: … }, 6000, true)] right: [("facts", Restated { by:
/// … }, 6000, true), ("older", Restated { by: … }, 8000, false)] ``. With `Reader::standing`
/// reading a revert's restored register as a restatement's (its `!reverted` test removed), the
/// write the register names stands and not the revert: `` left: [false, false, true, false,
/// false] right: [false, false, false, false, true] ``. With `restored` answering the register
/// itself, the revert's value is `{"k": [{"o": …, "v": …}]}`: `the value it restored`. With
/// `Reader::held` listing a restatement's candidate as stored (`shown` not asked), the token is
/// listed: `` left: (Some(Object {"r": Number(1), "x": String("sec1:…")}), false) right: (None,
/// true) ``. With `shown` reading a restored register of several candidates as a value of no
/// known type (`raw`), the operations it names read as credentials and the revert as refused:
/// `` left: (…, Some(…), true, true) right: (…, Some(…), false, true) ``. With `Reader::wrote` leaving
/// out the archived rows, the new row replaced nothing: `` left: [["fresh"], [], []] right:
/// [["fresh"], ["facts"], []] ``. With `Reader::reassert` marking nothing, the carried label
/// does not stand: `` left: [("born", Carried { generation: 1, trimmed: false }, false)] ``.
#[test]
fn a_cells_values_read_through_a_registry_roll() {
    use crate::table::tests::repair_commons;
    let pk = "mc-0000000000000001";
    let token = sealed_token();
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let macs: Vec<_> = lab.paths.iter().cloned().map(Registry::at).collect();
    for registry in &macs {
        registry.open().unwrap();
    }
    repair_commons(&lab, 6);
    let first = Handle::at(lab.paths[0].clone(), &REGISTRY);
    let write = |handle: &Handle, request: u8, changes: Vec<Change>| {
        append_history_at(
            handle,
            Command::Write {
                request: [request; 16],
                changes,
            },
            u64::from(request) * 1_000,
        )
    };
    let stored = |column: &str, x: &str| Change {
        collection: MACHINE.name.to_owned(),
        key: format!("{pk}.{column}"),
        value: Some(cell::CellValue::owner(1, json!(x)).encode()),
    };
    let born = write(&first, 1, machine(pk));
    // Any writer may store any value: a sealed token in a column that is no secret's.
    let stray = append_history_at(
        &first,
        Command::Write {
            request: [21; 16],
            changes: vec![stored("location", &token)],
        },
        1_500,
    );
    let moved = write(
        &first,
        2,
        vec![put(&MACHINE, pk, "location", json!("rack b"))],
    );
    // Two facts rows of Mac 0 signed for the first generation and never ingested there: no
    // roll carries their history.
    let me = keys::uuid32(&lab.machine(0));
    let facts_row = |n: u64, label: &str| Change {
        collection: MACHINE_FACTS.name.into(),
        key: me.clone(),
        value: Some(json!({"r": 1, "n": n, "mac": pk, "label": label})),
    };
    let (facts, wire) = sealed_record(&first, 3, vec![facts_row(2, "m9")], 3_000);
    let (older, older_wire) = sealed_record(&first, 13, vec![facts_row(1, "m8")], 3_000);
    repair_commons(&lab, 6);
    let now = || aterm_time::unix_millis().unwrap();
    macs[0].roll(true, now()).unwrap();
    repair_commons(&lab, 6);
    macs[1].release(now()).unwrap();
    repair_commons(&lab, 6);
    assert!(macs[0].roll(true, now()).unwrap().sealed);
    repair_commons(&lab, 6);
    assert_eq!(macs[0].generation().generation, 2);
    let rolled = Handle::at(lab.paths[0].clone(), macs[0].generation());

    let location = format!("{pk}.location");
    let rack = |x: &str| put(&MACHINE, pk, "location", json!(x)).value;
    let listed = |names: &[(&'static str, Digest)]| {
        let values = rolled.cell_values(MACHINE.name, &location).unwrap();
        let rows: Vec<_> = values
            .iter()
            .map(|held| {
                (
                    held.source,
                    held.authored_ms,
                    held.value.clone(),
                    held.stands,
                )
            })
            .collect();
        (named(names, &values), rows, values)
    };
    let carried = Source::Carried {
        generation: 1,
        trimmed: false,
    };
    let mut names = vec![
        ("born", born),
        ("stray", stray),
        ("moved", moved),
        ("facts", facts),
        ("older", older),
    ];
    let (order, rows, values) = listed(&names);
    assert_eq!(order, ["born", "stray", "moved"]);
    assert_eq!(
        rows,
        [
            (carried, 1_000, rack("rack a"), false),
            (carried, 1_500, None, false),
            (carried, 2_000, rack("rack b"), true)
        ]
    );
    assert!(
        values
            .iter()
            .all(|held| held.machine == Some(lab.machine(0)))
    );
    assert_eq!(
        values.iter().map(|held| held.withheld).collect::<Vec<_>>(),
        [false, true, false]
    );
    let mut shown = format!("{values:?}");

    // A write of the new generation replaced the carried candidate.
    let later = write(
        &rolled,
        4,
        vec![put(&MACHINE, pk, "location", json!("rack c"))],
    );
    names.push(("later", later));
    let (order, rows, _) = listed(&names);
    assert_eq!(order, ["born", "stray", "moved", "later"]);
    assert_eq!(
        rows.iter().map(|row| row.3).collect::<Vec<_>>(),
        [false, false, false, true]
    );
    assert_eq!((rows[3].0, &rows[3].2), (Source::Log, &rack("rack c")));
    assert_eq!(
        wrote_at(&rolled, &names, later, MACHINE.name, &location),
        [vec!["later"], vec!["moved"], vec![]]
    );

    // A revert of it restores the carried register: the revert stands, with the value restored.
    let undone = rolled.revert([94; 16], later).unwrap();
    names.push(("undone", undone));
    let (order, rows, values) = listed(&names);
    assert_eq!(order, ["born", "stray", "moved", "later", "undone"]);
    assert_eq!(
        rows.iter().map(|row| row.3).collect::<Vec<_>>(),
        [false, false, false, false, true]
    );
    assert_eq!(
        (values[4].source, values[4].reverts),
        (Source::Log, Some(later))
    );
    assert_eq!(values[4].value, rack("rack b"), "the value it restored");
    assert_eq!(
        wrote_at(&rolled, &names, undone, MACHINE.name, &location),
        [vec!["undone"], vec!["later"], vec![]]
    );
    shown.push_str(&format!("{values:?}{:?}", rolled.wrote(undone).unwrap()));

    // A carried candidate no history row names: the restating record's writer and clocks.
    let restate = |request: u8, column: &str, origin: u8, value: &Value| {
        let restating = write(
            &rolled,
            request,
            vec![Change {
                collection: MACHINE.name.to_owned(),
                key: format!("{pk}.{column}"),
                value: Some(json!({"k": [{"o": hex::encode_lower(&[origin; 32]), "v": value}]})),
            }],
        );
        let values = rolled
            .cell_values(MACHINE.name, &format!("{pk}.{column}"))
            .unwrap();
        let last = values.last().unwrap().clone();
        assert_eq!(
            (last.operation, last.source, last.authored_ms, last.stands),
            (
                [origin; 32],
                Source::Restated { by: restating },
                u64::from(request) * 1_000,
                true
            )
        );
        (
            last,
            format!("{values:?}{:?}", rolled.wrote(restating).unwrap()),
        )
    };
    let spare = put(&MACHINE, pk, "nickname", json!("spare")).value.unwrap();
    let (nickname, _) = restate(5, "nickname", 7, &spare);
    assert_eq!(
        (nickname.value.as_ref(), nickname.withheld),
        (Some(&spare), false)
    );
    // One that holds a sealed token is listed with no value.
    let (status, text) = restate(7, "status", 8, &stored("status", &token).value.unwrap());
    assert_eq!((status.value, status.withheld), (None, true));
    shown.push_str(&text);
    assert!(!shown.contains(&token), "a sealed token left the read");
    // A revert that restores a register of several candidates is listed as that register: each
    // candidate as stored, the operations it names as they are.
    let other = put(&MACHINE, pk, "nickname", json!("other")).value.unwrap();
    let register = json!({"k": [
        {"o": hex::encode_lower(&[7; 32]), "v": spare},
        {"o": hex::encode_lower(&[9; 32]), "v": other},
    ]});
    let nicknames = format!("{pk}.nickname");
    write(
        &rolled,
        14,
        vec![Change {
            collection: MACHINE.name.to_owned(),
            key: nicknames.clone(),
            value: Some(register.clone()),
        }],
    );
    let solo = write(
        &rolled,
        15,
        vec![put(&MACHINE, pk, "nickname", json!("solo"))],
    );
    let both = rolled.revert([96; 16], solo).unwrap();
    let values = rolled.cell_values(MACHINE.name, &nicknames).unwrap();
    let last = values.last().unwrap();
    assert_eq!(
        (last.operation, last.reverts, last.refused, last.stands),
        (both, Some(solo), false, true)
    );
    assert_eq!(last.value, Some(register), "the register it restored");

    // An owned row that stands as an archived record no history row names.
    assert!(
        rolled
            .cell_values(MACHINE_FACTS.name, &me)
            .unwrap()
            .is_empty()
    );
    let facts_listed = |names: &[(&'static str, Digest)]| {
        let values = rolled.cell_values(MACHINE_FACTS.name, &me).unwrap();
        let rows: Vec<_> = named(names, &values)
            .into_iter()
            .zip(&values)
            .map(|(name, held)| (name, held.source, held.authored_ms, held.stands))
            .collect();
        (rows, values)
    };
    let archiving = archive(&rolled, 6, facts, &wire);
    let (rows, values) = facts_listed(&names);
    assert_eq!(named(&names, &values), ["facts"]);
    assert_eq!(
        rows,
        [("facts", Source::Restated { by: archiving }, 6_000, true)]
    );
    assert_eq!(values[0].value, facts_row(2, "m9").value);

    // An earlier row of the same key, archived after it: listed, and it does not stand.
    let again = archive(&rolled, 8, older, &older_wire);
    let (rows, values) = facts_listed(&names);
    assert_eq!(
        rows,
        [
            ("facts", Source::Restated { by: archiving }, 6_000, true),
            ("older", Source::Restated { by: again }, 8_000, false)
        ]
    );
    assert_eq!(values[1].value, facts_row(1, "m8").value);

    // The key's first row of this generation replaced the archived row that stood, which is
    // not the one archived last, and missed nothing.
    let fresh = write(&rolled, 9, vec![facts_row(3, "m10")]);
    names.push(("fresh", fresh));
    assert_eq!(
        wrote_at(&rolled, &names, fresh, MACHINE_FACTS.name, &me),
        [vec!["fresh"], vec!["facts"], vec![]]
    );
    let (rows, _) = facts_listed(&names);
    assert_eq!(
        rows.iter()
            .map(|(name, _, _, stands)| (*name, *stands))
            .collect::<Vec<_>>(),
        [("facts", false), ("older", false), ("fresh", true)]
    );

    // A row retired and made live again re-asserts its label: the carried write stands.
    let label = (MACHINE.name.to_owned(), format!("{pk}.label"));
    write(
        &rolled,
        10,
        vec![put(&MACHINE, pk, "row", json!("retired"))],
    );
    let revived = write(&rolled, 11, vec![put(&MACHINE, pk, "row", json!("live"))]);
    assert_eq!(
        rolled.view().unwrap().cells[&label]
            .candidates
            .iter()
            .map(|candidate| candidate.operation)
            .collect::<Vec<_>>(),
        [revived],
        "the label stands under the re-entry"
    );
    let values = rolled.cell_values(&label.0, &label.1).unwrap();
    assert_eq!(
        named(&names, &values)
            .into_iter()
            .zip(&values)
            .map(|(name, held)| (name, held.source, held.stands))
            .collect::<Vec<_>>(),
        [("born", carried, true)]
    );
}

/// A claim's re-entry is no write to the claim's cell: the writes it re-asserts stand. Mac 0
/// writes a machine row with a label (`born`), retires the row and makes it live again
/// (`revived`), each a write of the `row` cell alone. The view then holds the label under
/// `revived`, which wrote nothing there; the label lists its one write, `born`, standing, and
/// `revived` wrote only the `row` cell. The same holds where the row comes back by a revert of
/// its retirement. A later write of the label replaced `born` and alone stands.
/// NEGATIVE CONTROL: with `Reader::reassert` marking nothing (returning at once), no listed
/// value of the label stands though the cell shows it: `` left: [("born", false)] right:
/// [("born", true)] ``.
#[test]
fn a_claims_reentry_stands_as_the_writes_it_reasserts() {
    let lab = Lab::new(&[Role::Writer]);
    Registry::at(lab.paths[0].clone()).open().unwrap();
    let handle = Handle::at(lab.paths[0].clone(), &REGISTRY);
    let write = |request: u8, changes: Vec<Change>| {
        append_history_at(
            &handle,
            Command::Write {
                request: [request; 16],
                changes,
            },
            u64::from(request) * 1_000,
        )
    };
    let row = |state: &str| vec![put(&MACHINE, MC, "row", json!(state))];
    let mut first = machine(MC);
    first[0] = put(&MACHINE, MC, "label", json!("m7"));
    let born = write(1, first);
    write(2, row("retired"));
    let revived = write(3, row("live"));
    let mut names = vec![("born", born), ("revived", revived)];
    let label = (MACHINE.name.to_owned(), format!("{MC}.label"));
    let candidates = || {
        handle.view().unwrap().cells[&label]
            .candidates
            .iter()
            .map(|candidate| candidate.operation)
            .collect::<Vec<_>>()
    };
    let listed = |names: &[(&'static str, Digest)]| {
        let values = handle.cell_values(&label.0, &label.1).unwrap();
        named(names, &values)
            .into_iter()
            .zip(&values)
            .map(|(name, held)| (name, held.stands))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        candidates(),
        [revived],
        "the label stands under the re-entry"
    );
    assert_eq!(listed(&names), [("born", true)]);
    let wrote = handle.wrote(revived).unwrap();
    assert_eq!(
        wrote
            .iter()
            .map(|cell| cell.key.as_str())
            .collect::<Vec<_>>(),
        [format!("{MC}.row")],
        "the re-entry wrote the row cell alone"
    );

    // The row comes back by a revert of its retirement.
    let retired = write(4, row("retired"));
    let back = handle.revert([95; 16], retired).unwrap();
    assert_eq!(candidates(), [back], "the label stands under the revert");
    assert_eq!(listed(&names), [("born", true)]);

    // A later write of the label replaced the write that stood.
    let renamed = write(6, vec![put(&MACHINE, MC, "label", json!("m8"))]);
    names.push(("renamed", renamed));
    assert_eq!(listed(&names), [("born", false), ("renamed", true)]);
    assert_eq!(
        wrote_at(&handle, &names, renamed, &label.0, &label.1),
        [vec!["renamed"], vec!["born"], vec![]]
    );
}

/// No string another surface refuses to show leaves a cell's values. A credential-shaped string
/// that another build stored in a text column, in a cell's import source, in a column this
/// build does not compile and in an owned row's field is listed as the refusal mark, the rest
/// of the value as stored and the write marked; a value of a column this build does not compile
/// whose name is a secret's is listed with no value; and a set member whose key is
/// credential-shaped is named by the mark.
/// NEGATIVE CONTROL: with `shown` listing every value the secret rule passes as stored
/// (`grid::raw_shown` not asked), the string is listed where the mark belongs: `` left: […
/// (stray, Some(Object({"r": Number(1), "x": String("ghp_…")})), false, false) …] ``. With an
/// unknown column of a secret's name read as any other (`typed::secret_named` not asked), its
/// value is listed: `` left: (Some(Object({"r": Number(3), "x": Object({"entry": String("(refused
/// value: not shown)"), "vault": String("alab")})})), false, true) right: (None, true, false)
/// ``. With `Reader::wrote` naming a key as stored (`keys::safe_key` not asked), no cell's key
/// is the mark: `` the member's key is the mark left: [] ``.
#[test]
fn a_cells_values_never_show_a_refused_string() {
    // In halves, so the export guard never reads a token here.
    const CREDENTIAL: &str = concat!("ghp", "_0123456789abcdefghijklmnopqrstuvwxyzAB");
    const MEMBER: &str = concat!("sk", "-0123456789abcdef");
    const ENTRY: &str = "alab-address-app_password-ad-0000000000000001.0123456789abcdef";
    assert!(crate::typed::guard::credential_shaped(CREDENTIAL));
    assert!(crate::typed::guard::credential_shaped(MEMBER));
    let lab = Lab::new(&[Role::Writer]);
    Registry::at(lab.paths[0].clone()).open().unwrap();
    let handle = Handle::at(lab.paths[0].clone(), &REGISTRY);
    let me = keys::uuid32(&lab.machine(0));
    let stored = |collection: &str, key: String, value: Value| Change {
        collection: collection.to_owned(),
        key,
        value: Some(value),
    };
    let write = |request: u8, changes: Vec<Change>| {
        append_history_at(
            &handle,
            Command::Write {
                request: [request; 16],
                changes,
            },
            u64::from(request) * 1_000,
        )
    };
    let location = format!("{MC}.location");
    let nickname = format!("{MC}.nickname");
    let gadget = format!("{MC}.gadget");
    let password = "ad-0000000000000001.app_password".to_owned();
    let born = write(1, machine(MC));
    // Any writer may store any value: what an older or modified build's guards let through.
    let stray = write(
        2,
        vec![
            stored(
                MACHINE.name,
                location.clone(),
                cell::CellValue::owner(1, json!(CREDENTIAL)).encode(),
            ),
            stored(
                MACHINE.name,
                nickname.clone(),
                cell::CellValue::imported(1, json!("spare"), CREDENTIAL).encode(),
            ),
            stored(
                MACHINE.name,
                gadget.clone(),
                json!({"r": 9, "x": {"serial": CREDENTIAL, "slot": 2}}),
            ),
            stored(
                ADDRESS.name,
                password.clone(),
                json!({"r": 3, "x": {"vault": "alab", "entry": ENTRY}}),
            ),
            stored(
                MACHINE.name,
                format!("{MC}.clis.{MEMBER}"),
                cell::CellValue::owner(1, json!(true)).encode(),
            ),
            stored(
                MACHINE_FACTS.name,
                me.clone(),
                json!({"r": 1, "n": 1, "mac": MC, "label": "m9", "note": CREDENTIAL}),
            ),
        ],
    );
    let moved = write(3, vec![put(&MACHINE, MC, "location", json!("rack c"))]);
    assert!(handle.view().unwrap().refused.is_empty());

    let one = |collection: &str, key: &str| {
        let mut values = handle.cell_values(collection, key).unwrap();
        let last = values.pop().unwrap();
        assert_eq!(last.operation, stray, "{key}");
        last
    };
    let refused = json!(sheet::REFUSED);
    let locations = handle.cell_values(MACHINE.name, &location).unwrap();
    assert_eq!(
        locations
            .iter()
            .map(|held| (
                held.operation,
                held.value.clone(),
                held.withheld,
                held.refused
            ))
            .collect::<Vec<_>>(),
        [
            (
                born,
                put(&MACHINE, MC, "location", json!("rack a")).value,
                false,
                false
            ),
            (stray, Some(json!({"r": 1, "x": refused})), false, true),
            (
                moved,
                put(&MACHINE, MC, "location", json!("rack c")).value,
                false,
                false
            ),
        ]
    );
    let source = one(MACHINE.name, &nickname);
    assert_eq!(
        (source.value, source.withheld, source.refused),
        (
            Some(json!({"r": 1, "x": "spare", "s": refused})),
            false,
            true
        )
    );
    let unknown = one(MACHINE.name, &gadget);
    assert_eq!(
        (unknown.value, unknown.withheld, unknown.refused),
        (
            Some(json!({"r": 9, "x": {"serial": refused, "slot": 2}})),
            false,
            true
        )
    );
    let named_secret = one(ADDRESS.name, &password);
    assert_eq!(
        (
            named_secret.value,
            named_secret.withheld,
            named_secret.refused
        ),
        (None, true, false)
    );
    let row = one(MACHINE_FACTS.name, &me);
    assert_eq!(
        (row.value, row.withheld, row.refused),
        (
            Some(json!({"r": 1, "n": 1, "mac": MC, "label": "m9", "note": refused})),
            false,
            true
        )
    );
    let wrote = handle.wrote(stray).unwrap();
    assert_eq!(wrote.len(), 6);
    assert_eq!(
        wrote
            .iter()
            .filter(|cell| cell.key == sheet::REFUSED)
            .map(|cell| (cell.collection.as_str(), cell.wrote[0].value.clone()))
            .collect::<Vec<_>>(),
        [(
            MACHINE.name,
            Some(cell::CellValue::owner(1, json!(true)).encode())
        )],
        "the member's key is the mark"
    );
    let replaced = handle.wrote(moved).unwrap();
    assert_eq!(replaced[0].replaced, [locations[1].clone()]);
    let shown = format!("{locations:?}{wrote:?}{replaced:?}");
    assert!(
        ![CREDENTIAL, MEMBER, ENTRY]
            .iter()
            .any(|refused| shown.contains(refused)),
        "a refused string left the read"
    );
}

/// An owned row's values read through a registry roll: a carried history entry is its roller's
/// word, and never stands in for a signed record. In the first generation Mac 0 writes its
/// facts row twice (`early`, which holds a sealed token, then `seen`) and Mac 1 writes a row at
/// Mac 0's key (`forged`). After the roll the key lists `early` as the history states it, with
/// no value and never standing, and `seen` once, standing: the record the roll archived, with
/// the clocks its history entry states; `forged` is not listed. Mac 1 then makes itself a roller
/// and states three entries at Mac 0's key in Mac 0's name: a row nothing signed (`claimed`), a
/// trimmed entry (`trimmed`) and another row under the digest of a record Mac 0 signed
/// (`facts`). Each is listed apart and does not stand. Once `facts` is archived, its value,
/// writer and machine are the signed record's, and it stands; an archived row no entry names
/// (`sealed`, which holds a token) is listed with no value, as Mac 0's and at the record of Mac 1
/// that archived it. Mac 0's first row of the new generation replaced `facts`. A typed cell's
/// entry stated with a sealed token is listed with no value.
/// NEGATIVE CONTROL: with `Reader::cell` listing the carried entries before the archived rows,
/// the statement stands in for the signed record: `` left: [("early", "Stated", false),
/// ("seen", "Stated", false)] right: [("early", "Stated", false), ("seen", "Carried", true)] ``.
/// With `Reader::cell` not passing over an entry already listed (`seen` not asked in its last
/// loop), the archived row is listed again as stated: `` left: [("early", "Stated", false),
/// ("seen", "Carried", true), ("seen", "Stated", false)] ``. With `Reader::stated` not asking
/// `owned::kept`, the forged row is listed: `` left: [("early", "Stated", false), ("seen",
/// "Carried", true), ("forged", "Stated", false)] ``. With `Reader::stated` judging an entry
/// stated with no value as a row with none (`keyed` not given), the secret's row is not listed:
/// `` left: [("seen", "Carried", true)] right: [("early", "Stated", false), ("seen", "Carried",
/// true)] ``. With `Reader::restated` giving an archived row the machine of the record that
/// archived it, Mac 1 reads as the writer of Mac 0's row: `an archived row is its signer's`.
/// With `Reader::restated` listing the row as stored (`shown` not asked), the token is listed:
/// `` left: (Some(Object({… "serial": String("sec1:…")})), false, Restated { by: … }) right:
/// (None, true, Restated { by: … }) ``; with `Reader::stated` doing so, the typed cell's:
/// `` left: (Some(Object({"r": Number(1), "x": String("sec1:…")})), false) right: (None, true)
/// ``. With `Reader::wrote` leaving out the archived rows, the new row replaced nothing:
/// `` left: [["fresh"], [], []] right: [["fresh"], ["facts"], []] ``.
#[test]
fn an_owned_rows_values_read_through_a_registry_roll() {
    use crate::table::tests::repair_commons;
    let pk = "mc-0000000000000001";
    let token = sealed_token();
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let macs: Vec<_> = lab.paths.iter().cloned().map(Registry::at).collect();
    for registry in &macs {
        registry.open().unwrap();
    }
    repair_commons(&lab, 6);
    let first = Handle::at(lab.paths[0].clone(), &REGISTRY);
    let write = |handle: &Handle, request: u8, changes: Vec<Change>| {
        append_history_at(
            handle,
            Command::Write {
                request: [request; 16],
                changes,
            },
            u64::from(request) * 1_000,
        )
    };
    let me = keys::uuid32(&lab.machine(0));
    let at_me = |row: Value| Change {
        collection: MACHINE_FACTS.name.into(),
        key: me.clone(),
        value: Some(row),
    };
    let facts_row = |n: u64, label: &str| json!({"r": 1, "n": n, "mac": pk, "label": label});
    let secret_row =
        |n: u64| json!({"r": 1, "n": n, "mac": pk, "label": "m8", "serial": token.as_str()});
    let born = write(&first, 1, machine(pk));
    let early = write(&first, 2, vec![at_me(secret_row(1))]);
    let seen = write(&first, 3, vec![at_me(facts_row(2, "m9"))]);
    repair_commons(&lab, 6);
    let forged = write(
        &Handle::at(lab.paths[1].clone(), &REGISTRY),
        4,
        vec![at_me(facts_row(9, "m1"))],
    );
    repair_commons(&lab, 6);
    // Two more rows of Mac 0, signed for the first generation and never ingested there.
    let (facts, wire) = sealed_record(&first, 5, vec![at_me(facts_row(5, "m10"))], 5_000);
    let (sealed, sealed_wire) = sealed_record(&first, 6, vec![at_me(secret_row(4))], 6_000);
    let author = first
        .view()
        .unwrap()
        .ops
        .iter()
        .find(|op| op.id == born)
        .unwrap()
        .actor;
    let mut names = vec![
        ("early", early),
        ("seen", seen),
        ("forged", forged),
        ("facts", facts),
        ("sealed", sealed),
    ];
    let kind = |source: Source| match source {
        Source::Log => "Log",
        Source::Carried { generation: 1, .. } => "Carried",
        Source::Stated { generation: 1, .. } => "Stated",
        Source::Restated { .. } => "Restated",
        _ => "?",
    };
    let listed = |handle: &Handle, names: &[(&'static str, Digest)]| {
        let values = handle.cell_values(MACHINE_FACTS.name, &me).unwrap();
        let rows: Vec<_> = named(names, &values)
            .into_iter()
            .zip(&values)
            .map(|(name, held)| (name, kind(held.source), held.stands))
            .collect();
        (rows, values)
    };
    let (rows, values) = listed(&first, &names);
    assert_eq!(rows, [("early", "Log", false), ("seen", "Log", true)]);
    assert_eq!((&values[0].value, values[0].withheld), (&None, true));
    let mut shown = format!("{values:?}");

    let now = || aterm_time::unix_millis().unwrap();
    macs[0].roll(true, now()).unwrap();
    repair_commons(&lab, 6);
    macs[1].release(now()).unwrap();
    repair_commons(&lab, 6);
    assert!(macs[0].roll(true, now()).unwrap().sealed);
    repair_commons(&lab, 6);
    assert_eq!(macs[0].generation().generation, 2);
    let rolled = Handle::at(lab.paths[0].clone(), macs[0].generation());
    let there = Handle::at(lab.paths[1].clone(), macs[1].generation());

    // The roll's own history and archive: the signed row once, the secret's row with no value.
    let (rows, values) = listed(&rolled, &names);
    assert_eq!(
        rows,
        [("early", "Stated", false), ("seen", "Carried", true)]
    );
    assert_eq!(
        (&values[0].value, values[0].withheld, values[0].source),
        (
            &None,
            true,
            Source::Stated {
                generation: 1,
                trimmed: false
            }
        )
    );
    assert_eq!(
        (values[1].value.as_ref(), values[1].authored_ms),
        (Some(&facts_row(2, "m9")), 3_000)
    );
    assert!(
        values
            .iter()
            .all(|held| held.machine == Some(lab.machine(0)) && held.actor == author)
    );
    shown.push_str(&format!("{values:?}"));

    // Mac 1 makes itself a roller and states history at Mac 0's key, in Mac 0's name.
    let whom = (author.as_slice(), &lab.machine(0));
    let by = keys::uuid32(&lab.machine(1));
    let (claimed, trimmed, typed) = ([5; 32], [6; 32], [7; 32]);
    let at_key = |row: Value| json!([{"c": MACHINE_FACTS.name, "k": me.as_str(), "x": row}]);
    write(
        &there,
        31,
        vec![
            cover(&by),
            stated(&by, claimed, whom, 1, false, at_key(facts_row(99, "m2"))),
            stated(
                &by,
                trimmed,
                whom,
                2,
                true,
                json!([{"c": MACHINE_FACTS.name, "k": me.as_str()}]),
            ),
            stated(&by, facts, whom, 3, false, at_key(facts_row(77, "m3"))),
            stated(
                &by,
                typed,
                whom,
                4,
                false,
                json!([{"c": MACHINE.name, "k": format!("{pk}.location"),
                    "x": {"r": 1, "x": token.as_str()}}]),
            ),
        ],
    );
    repair_commons(&lab, 6);
    names.extend([("claimed", claimed), ("trimmed", trimmed), ("typed", typed)]);
    let (rows, values) = listed(&rolled, &names);
    assert_eq!(
        rows,
        [
            ("claimed", "Stated", false),
            ("trimmed", "Stated", false),
            ("facts", "Stated", false),
            ("early", "Stated", false),
            ("seen", "Carried", true)
        ]
    );
    assert_eq!(
        (&values[1].value, values[1].withheld, values[1].source),
        (
            &None,
            false,
            Source::Stated {
                generation: 1,
                trimmed: true
            }
        )
    );
    assert_eq!(values[2].value, Some(facts_row(77, "m3")));

    // Mac 1 archives the two signed records. The one a statement names is the record's own row,
    // writer and machine, and it stands; the other is Mac 0's too, at the record that archived
    // it, with no value.
    archive(&there, 32, facts, &wire);
    let archiving = archive(&there, 33, sealed, &sealed_wire);
    repair_commons(&lab, 6);
    for handle in [&rolled, &there] {
        let (rows, values) = listed(handle, &names);
        assert_eq!(
            rows,
            [
                ("claimed", "Stated", false),
                ("trimmed", "Stated", false),
                ("facts", "Carried", true),
                ("early", "Stated", false),
                ("seen", "Carried", false),
                ("sealed", "Restated", false)
            ]
        );
        assert_eq!(values[2].value, Some(facts_row(5, "m10")));
        assert_eq!(
            (&values[5].value, values[5].withheld, values[5].source),
            (&None, true, Source::Restated { by: archiving })
        );
        assert!(
            [&values[2], &values[5]]
                .iter()
                .all(|held| held.machine == Some(lab.machine(0)) && held.actor == author),
            "an archived row is its signer's"
        );
        shown.push_str(&format!("{values:?}"));
    }

    // Mac 0's first row of the new generation replaced the archived row that stood.
    let fresh = write(&rolled, 34, vec![at_me(facts_row(6, "m11"))]);
    names.push(("fresh", fresh));
    assert_eq!(
        wrote_at(&rolled, &names, fresh, MACHINE_FACTS.name, &me),
        [vec!["fresh"], vec!["facts"], vec![]]
    );
    let (rows, _) = listed(&rolled, &names);
    assert_eq!(
        (rows[2], rows.last().unwrap()),
        (("facts", "Carried", false), &("fresh", "Log", true))
    );

    // A typed cell's entry stated with a sealed token is listed with no value.
    let locations = rolled
        .cell_values(MACHINE.name, &format!("{pk}.location"))
        .unwrap();
    let stated_token = locations
        .iter()
        .find(|held| held.operation == typed)
        .unwrap();
    assert_eq!((&stated_token.value, stated_token.withheld), (&None, true));
    shown.push_str(&format!("{locations:?}{:?}", rolled.wrote(fresh).unwrap()));
    assert!(!shown.contains(&token), "a sealed token left the read");
}
