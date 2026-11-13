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
        registry::{ADDRESS, CLI, MACHINE},
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
use aterm_codec::hex;
use aterm_json::json;
use aterm_secrets::commons::roster::Role;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

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

/// Append an authenticated operation with an explicit signed clock claim, preserving the
/// installation chain and the complete observed frontier.
fn append_history_at(handle: &Handle, command: Command, authored_ms: u64) -> Digest {
    application::with_store(
        &handle.paths,
        false,
        handle.descriptor(),
        |session, store, scope| {
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
            let wire = crate::table::fixture::seal_at(
                session,
                scope,
                crate::crypto::Causal {
                    sequence: previous.map_or(1, |header| header.sequence + 1),
                    previous: previous.map(|header| header.id),
                    parents: &parents,
                },
                &command.encode()?,
                authored_ms,
            )?;
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
