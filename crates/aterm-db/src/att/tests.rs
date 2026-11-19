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

/// The session's chain head in `store`: the sequence of its last record (0 before its first), that
/// record, and the next record's parents, the complete observed frontier with that record.
fn chain_head(
    session: &crate::table::Session<'_>,
    store: &crate::table::store::Store,
) -> Result<(u64, Option<Digest>, Vec<Digest>)> {
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
    Ok((
        previous.map_or(0, |header| header.sequence),
        previous.map(|header| header.id),
        parents.into_iter().collect(),
    ))
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
    let (sequence, previous, parents) = chain_head(session, store)?;
    crate::table::fixture::seal_at(
        session,
        scope,
        crate::crypto::Causal {
            sequence: sequence + 1,
            previous,
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

/// The batches of [`aterm_journal::MAX_APPEND_RECORDS`] each open of [`append_bulk_at`] writes at
/// most, 1,024 records: a 1,000-record bulk takes one open, as each step of [`copy_registry`] takes
/// one open for all it copies into a Mac's store.
const BULK_OPEN_BATCHES: usize = 4;

/// When an open of this file's bulk fixtures ([`append_bulk_at`], [`copy_registry`]) stops: an hour
/// from now. The ordinary admission's minute ([`crate::admission::deadline`]) bounds the session's
/// generation checks too, so it would time the fixture's sealing and replay, which a loaded Mac
/// stretches without bound: a niced release run spent over a minute on an open that takes 9 s at a
/// load of 40. No other process holds these homes, so this is a hang detector and no budget.
fn bulk_open_deadline() -> std::time::Instant {
    std::time::Instant::now() + std::time::Duration::from_secs(60 * 60)
}

/// Write up to `count` records to `handle`'s table as this Mac's writer, each authored at
/// `authored_ms`, one chain in batches of [`aterm_journal::MAX_APPEND_RECORDS`]: how many it
/// wrote, and the error that stopped it short, if one did. A refused batch is retried a record at
/// a time, so the count is exact. An open writes at most [`BULK_OPEN_BATCHES`] batches, under
/// [`bulk_open_deadline`]. Within an open, each batch after the first is admitted against the
/// writer counts that the open's earlier batch commits left in memory, as a repair's batches
/// through a kept store are (`replica`'s `keep`).
/// Each open reads the chain head ([`chain_head`]) after `Store::open`, which seals held writes
/// first, and its batches carry the chain on from there.
/// NEGATIVE CONTROL: with a batch commit counting none of its records toward their writer in
/// memory (`Store::ingest_batch`, each record's count taken back after `hold`),
/// `a_writer_past_its_quota_writes_on_once_a_checkpoint_is_final` fails: a later open is refused,
/// `table writer evidence quota exhausted`, for the records earlier batches wrote past the
/// writer's quota. With one batch an open, it passes.
fn append_bulk_at(handle: &Handle, count: usize, authored_ms: u64) -> (usize, Option<String>) {
    let mut written = 0;
    loop {
        let (wrote, refused) = application::with_store_until(
            &handle.paths,
            false,
            handle.descriptor(),
            bulk_open_deadline(),
            |session, store, scope| {
                let (mut sequence, mut last, mut parents) = chain_head(session, store)?;
                let mut wrote = 0;
                for _ in 0..BULK_OPEN_BATCHES {
                    let mut wires = Vec::new();
                    while wires.len() < aterm_journal::MAX_APPEND_RECORDS
                        && written + wrote + wires.len() < count
                    {
                        let n = sequence + 1;
                        let mut request = [0; 16];
                        request[..8].copy_from_slice(&n.to_le_bytes());
                        let command = Command::Write {
                            request,
                            changes: vec![put(&MACHINE, MC, "nickname", json!(format!("n{n}")))],
                            said: None,
                        };
                        let wire = crate::table::fixture::seal_at(
                            session,
                            scope,
                            crate::crypto::Causal {
                                sequence: n,
                                previous: last,
                                parents: &parents,
                            },
                            &command.encode()?,
                            authored_ms,
                        )?;
                        let id = session.authenticate(scope, &wire)?.header().id;
                        sequence = n;
                        last = Some(id);
                        parents = vec![id];
                        wires.push(wire);
                    }
                    if wires.is_empty() {
                        break;
                    }
                    let refs: Vec<&[u8]> = wires.iter().map(Vec::as_slice).collect();
                    if store.ingest_batch(session, &refs).is_ok() {
                        wrote += wires.len();
                        continue;
                    }
                    for wire in &wires {
                        if let Err(error) = store.ingest(session, wire) {
                            return Ok((wrote, Some(error.to_string())));
                        }
                        wrote += 1;
                    }
                }
                Ok((wrote, None))
            },
        )
        .unwrap();
        written += wrote;
        if refused.is_some() || written == count {
            return (written, refused);
        }
    }
}

/// Copy every record each of `lab`'s Macs holds into each other's registry store, parents first,
/// in batches through each store's own admission and quotas: what a repair moves, without the
/// mesh, whose per-batch store opens are tracker LT-B5R's. The records go twice around the Macs:
/// each Mac's store takes what it lacks of all the Mac before it holds, then hands on all it holds
/// itself, so the last Mac holds every record after the first lap and each other Mac after the
/// second, with one open of each store a step.
fn copy_registry(lab: &Lab) {
    let macs = lab.paths.len();
    let handle = |n: usize| Handle::at(lab.paths[n].clone(), &REGISTRY);
    let mut carried = application::observe_store_until(
        &lab.paths[0],
        handle(0).descriptor(),
        bulk_open_deadline(),
        |session, store, _| held_wires(session, store),
    )
    .unwrap();
    let steps = 2 * macs - 2;
    for step in 1..=steps {
        let to = step % macs;
        carried = application::with_store_until(
            &lab.paths[to],
            false,
            handle(to).descriptor(),
            bulk_open_deadline(),
            |session, store, _| {
                let have: BTreeSet<Digest> = store.inventory(session)?.copied().collect();
                let lacking: Vec<&[u8]> = carried
                    .iter()
                    .filter(|(id, _)| !have.contains(id))
                    .map(|(_, wire)| wire.as_slice())
                    .collect();
                for batch in lacking.chunks(aterm_journal::MAX_APPEND_RECORDS) {
                    store.ingest_batch(session, batch)?;
                }
                if step == steps {
                    return Ok(Vec::new());
                }
                held_wires(session, store)
            },
        )
        .unwrap();
    }
}

/// Every record `store` holds, parents first, with its wire.
fn held_wires(
    session: &crate::table::Session<'_>,
    store: &crate::table::store::Store,
) -> Result<Vec<(Digest, Vec<u8>)>> {
    let ids: Vec<Digest> = store.inventory(session)?.copied().collect();
    let ordered = store.causal_order_of(&ids);
    let wires = store.replication_wires_unchecked(&ordered);
    Ok(ordered.into_iter().zip(wires).collect())
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
            said: None,
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
            said: None,
        },
        0,
    );
    let sibling = append_history_at(
        &handle,
        Command::Write {
            request: [75; 16],
            changes: vec![put(&MACHINE, MC, "status", json!("active"))],
            said: None,
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
            said: None,
        },
        0,
    );
    let child = append_history_at(
        &handle,
        Command::Write {
            request: [74; 16],
            changes: vec![put(&MACHINE, MC, "location", json!("rack b"))],
            said: None,
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
            said: None,
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
            said: None,
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
            said: None,
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
            said: None,
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
            said: None,
        },
        2_000,
    );
    repair_new_registry(&lab);
    append_history_at(
        &Handle::at(lab.paths[1].clone(), &REGISTRY),
        Command::Write {
            request: [92; 16],
            changes: vec![put(&MACHINE, MC, "status", json!("parked"))],
            said: None,
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
/// order, and newest first runs back from history's end the same way; the filters keep a page to one collection, row, column, writer or kind; each entry
/// carries the kind aterm-db decides (a facts-only record is aterm's, a mixed record the first
/// kind that holds, a revert explicit whatever it restores) and says when it projects nothing
/// (an owned row at a key not its signer's, an automatic record past its writer's quota). The
/// revision probe answers the page's revision until a record arrives; a peer's late record
/// sorts inside the range already served, so the next page asked at the old revision says it
/// moved and starts again from the top, where a page asked without one would skip that record.
/// NEGATIVE CONTROL: with `history::page` never setting `moved` (`let moved = false;`), the page
/// after the cursor resumes past the late record: `` assertion failed: next.moved ``. With a page
/// read newest first served from the front (`order[..end].iter()`, no `.rev()`), the backward
/// pages hold the oldest entries first: `` newest first, every entry once ``.
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
    // Newest first, pages of two, each before the last one's cursor, serve it backwards, once.
    let mut ask = Some(Ask::newest(Filter::default(), 2));
    let mut backwards = Vec::new();
    while let Some(asked) = ask {
        let page = handle.history_page(&asked).unwrap();
        assert!(!page.moved);
        assert!(page.entries.len() <= 2);
        backwards.extend(page.entries.iter().cloned());
        ask = asked.after(&page);
    }
    assert_eq!(
        backwards,
        history.iter().rev().cloned().collect::<Vec<_>>(),
        "newest first, every entry once"
    );

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
    let newest = handle
        .history_page(&Ask::newest(
            Filter {
                column: Some("location".into()),
                ..Filter::default()
            },
            1,
        ))
        .unwrap();
    assert_eq!(newest.entries, locations[3..], "the filter's newest entry");

    // A peer's record that sorts inside what was served: it observed only the records the peer
    // held, and its writer's clock runs behind, so it lands before the cursor.
    repair_new_registry(&lab);
    let peer = Handle::at(lab.paths[1].clone(), &REGISTRY);
    let last_before = append_history_at(
        &handle,
        Command::Write {
            request: [92; 16],
            changes: vec![put(&MACHINE, MC, "label", json!("m10"))],
            said: None,
        },
        u64::MAX / 2,
    );
    append_history_at(
        &handle,
        Command::Write {
            request: [93; 16],
            changes: vec![put(&MACHINE, MC, "label", json!("m11"))],
            said: None,
        },
        u64::MAX / 2,
    );
    let late = append_history_at(
        &peer,
        Command::Write {
            request: [94; 16],
            changes: vec![put(&MACHINE, MC, "location", json!("rack e"))],
            said: None,
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

/// A registry history of every shape a page filters on, as Mac 0 holds it: two Macs' writers,
/// each kind, two machine rows and their facts (one inert), a revert, and a peer's records whose
/// clock runs behind, so they sort among Mac 0's. Answers the lab and Mac 0's handle.
fn paged_history() -> (Lab, Handle) {
    use crate::{
        alab::registry::MACHINE_FACTS,
        typed::cell::{CellKey, CellValue},
    };
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let registry = Registry::at(lab.paths[0].clone());
    registry.open().unwrap();
    let handle = Handle::at(lab.paths[0].clone(), &REGISTRY);
    let facts = |n: usize| Change {
        collection: MACHINE_FACTS.name.into(),
        key: keys::uuid32(&lab.machine(n)),
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
    let explicit = vec![put(&MACHINE, MC, "location", json!("rack d"))];
    registry
        .write(vec![
            machine(MC),
            other_machine(OTHER, "m8"),
            vec![facts(0)],
            vec![marked(
                "location",
                CellValue::imported(1, json!("rack c"), "sr-0123456789abcdef"),
            )],
            vec![marked("note", CellValue::automatic(1, json!("seen")))],
            vec![facts(1)],
            explicit.clone(),
            vec![put(&MACHINE, OTHER, "location", json!("rack b"))],
        ])
        .unwrap();
    let target = handle
        .history()
        .unwrap()
        .into_iter()
        .find(|entry| entry.changes == explicit)
        .unwrap()
        .operation;
    handle.revert([91; 16], target).unwrap();
    repair_new_registry(&lab);
    let peer = Handle::at(lab.paths[1].clone(), &REGISTRY);
    for (n, (pk, column, value)) in [
        (MC, "location", "rack p"),
        (OTHER, "location", "rack q"),
        (OTHER, "label", "m7"),
    ]
    .into_iter()
    .enumerate()
    {
        append_history_at(
            &peer,
            Command::Write {
                request: [100 + n as u8; 16],
                changes: vec![put(&MACHINE, pk, column, json!(value))],
                said: None,
            },
            0,
        );
    }
    registry
        .write(vec![vec![put(&MACHINE, MC, "label", json!("m10"))]])
        .unwrap();
    repair_registry(&lab, 2);
    (lab, handle)
}

#[test]
fn view_with_history_decodes_the_head_once() {
    let (_lab, handle) = paged_history();
    let history = handle.history().unwrap();
    let before = super::DECODED.get();
    let (head, expected_usage) = handle.usage().unwrap();
    let one_read = super::DECODED.get() - before;
    assert!(one_read > 0);

    let machine_judgments = || crate::typed::claim::projection_count(MACHINE.name);
    let before = super::DECODED.get();
    let before_judgments = machine_judgments();
    let read = handle.view_with_history().unwrap();
    assert_eq!(super::DECODED.get() - before, one_read);
    assert_eq!(machine_judgments() - before_judgments, 1);
    assert_eq!(read.usage, expected_usage);
    assert_eq!(read.entries, history);
    assert_eq!(read.machines, head.project_settled(&MACHINE));
    assert_eq!(read.macs, Macs::of(&head));
    assert_eq!(read.view.awaiting, head.awaiting);
    assert_eq!(read.view.refusals, head.refusals);
}

/// The page `ask` names of a table at `revision` whose every entry is `all`, in history's order,
/// worked out from what a page is: from the top where it was asked at another revision, else
/// the entries after its cursor (before it, newest first, run backwards) that its filter keeps
/// (kind, writer, and a change to the place it names), at most `limit` of them, and the last
/// one's cursor where one more follows.
fn page_by_definition(all: &[Entry], revision: Revision, ask: &Ask) -> Page {
    use crate::att::history::Writer;
    let moved = ask.revision.is_some_and(|asked| asked != revision);
    let after = ask.after.filter(|_| !moved);
    let filter = &ask.filter;
    let mut served: Vec<&Entry> = all
        .iter()
        .filter(|entry| {
            after.is_none_or(|after| {
                if ask.newest_first {
                    entry.cursor() < after
                } else {
                    entry.cursor() > after
                }
            })
        })
        .filter(|entry| filter.kind.is_none_or(|kind| entry.kind == kind))
        .filter(|entry| match filter.writer {
            None => true,
            Some(Writer::Actor(actor)) => entry.actor == actor,
            Some(Writer::Machine(machine)) => entry.machine == Some(machine),
        })
        .filter(|entry| {
            filter.collection.is_none() && filter.row.is_none() && filter.column.is_none()
                || entry.changes.iter().any(|change| filter.wants(change))
        })
        .collect();
    if ask.newest_first {
        served.reverse();
    }
    let more = served.len() > ask.limit;
    let entries: Vec<Entry> = served.into_iter().take(ask.limit).cloned().collect();
    Page {
        revision,
        moved,
        next: more.then(|| entries.last().map(Entry::cursor)).flatten(),
        entries,
        // The fixture holds no record its reader refuses.
        refused: Vec::new(),
    }
}

/// Every page a reader of `all` (a table's every entry, in history's order) can ask at
/// `revision`: each filter on its own (each kind, each writer and machine, each place) and
/// combined, each way, at each of several lengths, from the top and after each entry's cursor,
/// and at the revision, at none and at another.
fn every_ask(all: &[Entry], revision: Revision) -> Vec<Ask> {
    use crate::{
        alab::registry::MACHINE_FACTS,
        att::history::{Filter, Writer},
    };
    let actors: BTreeSet<Actor> = all.iter().map(|entry| entry.actor).collect();
    let machines: BTreeSet<[u8; 16]> = all.iter().filter_map(|entry| entry.machine).collect();
    let mut filters = vec![Filter::default()];
    for kind in [EntryKind::Aterm, EntryKind::Import, EntryKind::Explicit] {
        filters.push(Filter {
            kind: Some(kind),
            ..Filter::default()
        });
    }
    for writer in actors
        .iter()
        .map(|actor| Writer::Actor(*actor))
        .chain(machines.iter().map(|machine| Writer::Machine(*machine)))
        .chain([Writer::Actor([0xee; 48]), Writer::Machine([0xee; 16])])
    {
        filters.push(Filter {
            writer: Some(writer),
            ..Filter::default()
        });
    }
    for collection in [MACHINE.name, MACHINE_FACTS.name] {
        filters.push(Filter {
            collection: Some(collection.into()),
            ..Filter::default()
        });
    }
    for row in [MC, OTHER] {
        filters.push(Filter {
            row: Some(row.into()),
            ..Filter::default()
        });
    }
    filters.push(Filter {
        column: Some("location".into()),
        ..Filter::default()
    });
    filters.push(Filter {
        collection: Some(MACHINE.name.into()),
        row: Some(MC.into()),
        column: Some("location".into()),
        kind: Some(EntryKind::Explicit),
        writer: actors.first().copied().map(Writer::Actor),
    });
    let elsewhere = Revision([0xee; 32]);
    let mut asks = Vec::new();
    for filter in &filters {
        for newest_first in [false, true] {
            for limit in [1, 2, 5, usize::MAX] {
                for after in std::iter::once(None).chain(all.iter().map(|e| Some(e.cursor()))) {
                    for asked in [Some(revision), None, Some(elsewhere)] {
                        asks.push(Ask {
                            filter: filter.clone(),
                            after,
                            limit,
                            revision: asked,
                            newest_first,
                        });
                    }
                }
            }
        }
    }
    asks
}

/// A page at a held revision reads no record again (tracker LT-25B): a reader that keeps its
/// pages' order ([`Kept`]) reads the table once, and every page it asks at that revision after
/// the first, of every filter, length, direction and revision asked, from the top and after a
/// cursor, decodes no record ([`super::DECODED`]) and is the page its definition names
/// ([`page_by_definition`]) and the page read with nothing kept. The cut of the order a kept page
/// is copied from is the defined page for every cursor too ([`every_ask`]): the served pages
/// read from the top and after the middle entry, since a page reaches the kept order the same
/// way whatever it asks. Once a record is written, the next page reads the new revision,
/// says the table moved where it was asked at the old one, and holds the new record. A handle on
/// the same table made at another place (another copy of its `const`) is served from the same
/// kept order, and a page read with nothing kept, which takes its entries out of the order it
/// read rather than copying them, is the kept page for every ask.
/// NEGATIVE CONTROL: with the kept order never matched (`Order::of` answering `false`), every
/// page reads the store again and the sweep fails at its first ask: `a page at the held
/// revision decoded 13 records: Ask { filter: Filter { collection: None, .. }, after: None,
/// limit: 1, .. }`. With the order matched whatever the revision (`Order::of` comparing only the
/// home and the table), the page after the write is cut from the old order: `the new revision
/// is read: every record and the new one`. With the table compared by address
/// (`std::ptr::eq`), the handle made elsewhere reads it again: `a handle made at another place
/// read the table again left: 13 right: 0`. With a page taken newest first left oldest first
/// (`Cut::taken` turning nothing back), the sweep of cuts fails at its first newest-first page
/// of two: `Ask { .., limit: 2, .., newest_first: true }` (the entries of the left page in the
/// other order). With the page asking after no sighting (tracker LT-25C), every page at the held
/// revision opens the registry's store again: `a page at the held revision opened the
/// registry's store left: 768 right: 0`.
#[test]
fn a_history_page_at_a_held_revision_reads_no_record_again() {
    use super::history::{Ask, Cut, Filter};
    // The registry as another place names it: a static is an address of its own.
    static ELSEWHERE: Compiled = REGISTRY;
    let (_lab, handle) = paged_history();
    let all = handle.history().unwrap();
    for kind in [EntryKind::Aterm, EntryKind::Import, EntryKind::Explicit] {
        assert!(all.iter().any(|entry| entry.kind == kind), "{kind:?}");
    }
    let writers: BTreeSet<Actor> = all.iter().map(|entry| entry.actor).collect();
    assert_eq!(writers.len(), 2, "both Macs wrote");
    assert!(all.iter().any(|entry| entry.inert.is_some()));
    assert!(all.iter().any(|entry| entry.reverts.is_some()));
    let mut kept = Kept::default();
    assert_eq!(kept.revision(), None);
    let decoded = super::DECODED.get();
    let first = handle
        .history_page_kept(&Ask::newest(Filter::default(), 3), &mut kept)
        .unwrap();
    assert!(
        super::DECODED.get() - decoded >= all.len(),
        "the first page reads every record"
    );
    let revision = first.revision;
    assert_eq!(kept.revision(), Some(revision));
    assert_eq!(handle.revision().unwrap(), revision);
    let asks = every_ask(&all, revision);
    // Every ask's page is cut from the order as its definition names it, and taken out of the
    // order (a page read with nothing kept) or copied from it (a kept one), a page is one page.
    for ask in &asks {
        let cut = || Cut::of(&all, revision, ask);
        assert_eq!(
            cut().copied(&all),
            page_by_definition(&all, revision, ask),
            "{ask:?}"
        );
        assert_eq!(cut().taken(all.clone()), cut().copied(&all), "{ask:?}");
    }
    // Served at the held revision, the asks of every filter, direction, length and revision, from
    // the top and after the middle entry: a page reaches the kept order the same way whatever it
    // asks, and only the cut above reads the ask's cursor.
    let middle = all[all.len() / 2].cursor();
    let served: Vec<&Ask> = asks
        .iter()
        .filter(|ask| ask.after.is_none_or(|after| after == middle))
        .collect();
    // The registry's data folder, whose opens a page at the held revision makes none of.
    let folder =
        application::observe_store(&handle.paths, handle.descriptor(), |session, _, scope| {
            crate::catalog::Directory::new(session.paths(), session.trust()).path(scope)
        })
        .unwrap();
    let opens = std::rc::Rc::new(std::cell::Cell::new(0));
    let counted = std::rc::Rc::clone(&opens);
    let sightings = crate::table::store::test_sightings();
    crate::table::store::with_open_hook(
        &folder,
        move || {
            counted.set(counted.get() + 1);
            Ok(())
        },
        || {
            for &ask in &served {
                let decoded = super::DECODED.get();
                let page = handle.history_page_kept(ask, &mut kept).unwrap();
                assert_eq!(
                    super::DECODED.get() - decoded,
                    0,
                    "a page at the held revision decoded {} records: {ask:?}",
                    super::DECODED.get() - decoded
                );
                assert_eq!(page, page_by_definition(&all, revision, ask), "{ask:?}");
            }
        },
    );
    // Each opened no store, and learned the revision from the kept order's sighting (LT-25C).
    assert_eq!(
        opens.get(),
        0,
        "a page at the held revision opened the registry's store"
    );
    let now = crate::table::store::test_sightings();
    assert_eq!(
        (now.0 - sightings.0, now.1 - sightings.1),
        (served.len(), served.len()),
        "each page at the held revision stood on the kept order's sighting"
    );
    // The kept order is the order a read with nothing kept serves, page for page.
    for ask in asks.iter().step_by(97) {
        assert_eq!(
            handle.history_page_kept(ask, &mut kept).unwrap(),
            handle.history_page(ask).unwrap(),
            "{ask:?}"
        );
    }
    assert_eq!(kept.revision(), Some(revision));
    assert_eq!(
        kept.reads(),
        1,
        "one read of the table, at its one revision"
    );
    // A handle on the table made at another place is served from the kept order.
    let elsewhere = Handle::at(handle.paths.clone(), &ELSEWHERE);
    assert!(!std::ptr::eq(elsewhere.compiled, handle.compiled));
    let decoded = super::DECODED.get();
    assert_eq!(
        elsewhere
            .history_page_kept(&Ask::newest(Filter::default(), 3), &mut kept)
            .unwrap(),
        first
    );
    assert_eq!(
        super::DECODED.get() - decoded,
        0,
        "a handle made at another place read the table again"
    );
    assert_eq!(kept.reads(), 1);

    // A record written: the next page reads the new revision, and from the top where it was
    // asked at the old one.
    Registry::at(handle.paths.clone())
        .write(vec![vec![put(&MACHINE, OTHER, "label", json!("m11"))]])
        .unwrap();
    let decoded = super::DECODED.get();
    let older = Ask {
        revision: Some(revision),
        ..Ask::newest(Filter::default(), 3).after(&first).unwrap()
    };
    let moved = handle.history_page_kept(&older, &mut kept).unwrap();
    assert!(
        super::DECODED.get() - decoded > all.len(),
        "the new revision is read: every record and the new one"
    );
    assert!(moved.moved);
    assert_ne!(moved.revision, revision);
    assert_eq!(kept.revision(), Some(moved.revision));
    assert_eq!(kept.reads(), 2);
    let now = handle.history().unwrap();
    assert_eq!(now.len(), all.len() + 1);
    assert_eq!(moved, page_by_definition(&now, moved.revision, &older));
    assert_eq!(
        moved.entries[0].changes,
        [put(&MACHINE, OTHER, "label", json!("m11"))],
        "the new record heads the page"
    );
    // And that revision is held in its turn.
    let decoded = super::DECODED.get();
    let again = handle
        .history_page_kept(&Ask::newest(Filter::default(), 3), &mut kept)
        .unwrap();
    assert_eq!(super::DECODED.get(), decoded);
    assert_eq!(again.entries, moved.entries);
}

/// A kept order is of the access it was read under (tracker LT-25B): a membership change moves
/// the table's revision with no record written (Mac 1 made a Reader, whose records then count
/// for nothing here), so the next page reads the table again and is the page read with nothing
/// kept, never one cut from the order the change left behind.
/// NEGATIVE CONTROL: with the order matched whatever the revision (`Order::of` comparing only the
/// home and the table), the page after the change is cut from the old order: `the membership
/// change is read`.
#[test]
fn a_kept_history_order_is_read_again_after_a_membership_change() {
    use super::history::{Ask, Filter};
    let (lab, handle) = paged_history();
    let mut kept = Kept::default();
    let ask = Ask::newest(Filter::default(), usize::MAX);
    let before = handle.history_page_kept(&ask, &mut kept).unwrap();
    assert_eq!(kept.reads(), 1);

    lab.admit(&[Role::Writer, Role::Reader]);
    let decoded = super::DECODED.get();
    let after = handle.history_page_kept(&ask, &mut kept).unwrap();
    assert!(
        super::DECODED.get() > decoded,
        "the membership change is read"
    );
    assert_ne!(
        after.revision, before.revision,
        "membership moves the revision"
    );
    assert_eq!(kept.revision(), Some(after.revision));
    assert_eq!(kept.reads(), 2);
    assert_eq!(after, handle.history_page(&ask).unwrap());
    assert_ne!(
        after.entries, before.entries,
        "the change is one a page shows"
    );
}

/// A recovery replica enrolled for the registry beside a kept order's sighting, the primary store
/// untouched (tracker LT-25C): the primary still stands as the sighting read it, but a read of
/// the table now reads the combined view, so the next page is read again at that view's revision
/// and is the page read with nothing kept, never one cut from the primary's order.
/// NEGATIVE CONTROL: with the recovery enrollment left out of `Directory::observe_table_sighted`,
/// the page is cut from the primary's order: `a page after the enrollment holds the primary's
/// revision` (`left` and `right` the same revision).
#[test]
fn a_kept_history_order_is_read_again_once_a_recovery_replica_is_enrolled() {
    use super::history::{Ask, Filter};
    use crate::table::store::test_sightings;
    let (lab, handle) = paged_history();
    let mut kept = Kept::default();
    let ask = Ask::newest(Filter::default(), usize::MAX);
    let before = handle.history_page_kept(&ask, &mut kept).unwrap();
    assert_eq!(handle.history_page_kept(&ask, &mut kept).unwrap(), before);
    assert_eq!(kept.reads(), 1, "the held page stood on its sighting");
    {
        let bootstrap = lab.bootstrap(0);
        let session = crate::table::Session::open(&bootstrap).unwrap();
        let directory = crate::catalog::Directory::new(&lab.paths[0], bootstrap.trust());
        let scope = REGISTRY.descriptor(bootstrap.trust()).unwrap().scope;
        drop(
            crate::table::store::Store::join_recovery(
                &session,
                &directory.recovery().path(scope).unwrap(),
                scope,
            )
            .unwrap(),
        );
    }
    let sightings = test_sightings();
    let after = handle.history_page_kept(&ask, &mut kept).unwrap();
    let now = test_sightings();
    assert_eq!(
        (now.0 - sightings.0, now.1 - sightings.1),
        (1, 1),
        "the primary store, untouched, still stands as the sighting read it"
    );
    assert_ne!(
        after.revision, before.revision,
        "a page after the enrollment holds the primary's revision"
    );
    assert_eq!(kept.reads(), 2);
    assert_eq!(kept.revision(), Some(after.revision));
    assert_eq!(after, handle.history_page(&ask).unwrap());
    assert_eq!(after.revision, handle.revision().unwrap());
}

/// A page whose first attempt read another revision, let the kept order go and was then refused
/// as busy (the read is made again: `att::reopened`), while the store went back to the bytes the
/// order's sighting read: the next attempt asks after no sighting, since the order it names is no
/// longer held, and reads the store again; the page is the page read with nothing kept.
/// Here a group permit past the witness stands beside the registry's claim, so the sighting
/// fails and an open reads another revision; the permit that stood is put back between the
/// attempts.
/// NEGATIVE CONTROL: with the attempt asking after the sighting whatever `kept` holds, the second
/// attempt's sighting stands and the page is refused: `a history sighting stood with no order
/// kept`.
#[test]
fn a_history_page_made_again_after_a_busy_attempt_asks_after_no_order_it_let_go() {
    use super::history::{Ask, Filter, with_order_let_go};
    let (_lab, handle) = paged_history();
    let mut kept = Kept::default();
    let ask = Ask::newest(Filter::default(), usize::MAX);
    let before = handle.history_page_kept(&ask, &mut kept).unwrap();
    let (claims, scope) =
        application::observe_store(&handle.paths, handle.descriptor(), |session, _, scope| {
            Ok((crate::table::store::claim_dir(session)?, scope))
        })
        .unwrap();
    let put_back = crate::table::store::permit_group(&claims, scope);
    let page = with_order_let_go(
        move || {
            put_back();
            Err(Error::busy())
        },
        || handle.history_page_kept(&ask, &mut kept),
    )
    .unwrap();
    assert_eq!(page, before, "the store went back to the bytes it held");
    assert_eq!(kept.revision(), Some(before.revision));
    assert_eq!(kept.reads(), 2, "the page read the store again");
}

/// A cold file beside a registry that stands on no floor, which no read takes, refuses no read
/// of History and moves no revision (tracker LT-25C), whatever it is: here a file other users
/// can write, which a read that took it would refuse. The revision, a page with nothing kept
/// and a kept page are what they were, and the kept page stands on its sighting. Once a recovery
/// replica is enrolled, the combined view read in the table's place takes no cold payloads
/// either, its primary's neither, and is read the same way.
/// NEGATIVE CONTROLS: with `Store::revision` hashing the cold file whether or not a read takes
/// it, as it did, the revision refuses: `cold: mode 666 lets other users write it; run chmod 600
/// on it`. With a combined view's revision hashing its primary's cold file (`Store::files_into`
/// handed `true` for the primary), the revision read after the enrollment refuses the same way.
#[test]
fn a_cold_file_no_read_takes_refuses_no_history_read() {
    use super::history::{Ask, Filter};
    use std::{
        io::Write as _,
        os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _},
    };
    let (lab, handle) = paged_history();
    let mut kept = Kept::default();
    let ask = Ask::newest(Filter::default(), usize::MAX);
    let before = handle.history_page_kept(&ask, &mut kept).unwrap();
    let folder =
        application::observe_store(&handle.paths, handle.descriptor(), |session, _, scope| {
            crate::catalog::Directory::new(session.paths(), session.trust()).path(scope)
        })
        .unwrap();
    let cold = folder.join("cold");
    assert!(!cold.exists(), "the registry keeps no cold payloads here");
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&cold)
        .unwrap()
        .write_all(b"not the registry's cold payloads")
        .unwrap();
    std::fs::set_permissions(&cold, std::fs::Permissions::from_mode(0o666)).unwrap();
    assert_eq!(handle.revision().unwrap(), before.revision);
    assert_eq!(handle.history_page(&ask).unwrap(), before);
    let sightings = crate::table::store::test_sightings();
    assert_eq!(handle.history_page_kept(&ask, &mut kept).unwrap(), before);
    let now = crate::table::store::test_sightings();
    assert_eq!(
        (now.0 - sightings.0, now.1 - sightings.1),
        (1, 1),
        "the kept page stood on its sighting"
    );
    assert_eq!(kept.reads(), 1);

    {
        let bootstrap = lab.bootstrap(0);
        let session = crate::table::Session::open(&bootstrap).unwrap();
        let directory = crate::catalog::Directory::new(&lab.paths[0], bootstrap.trust());
        let scope = REGISTRY.descriptor(bootstrap.trust()).unwrap().scope;
        drop(
            crate::table::store::Store::join_recovery(
                &session,
                &directory.recovery().path(scope).unwrap(),
                scope,
            )
            .unwrap(),
        );
    }
    let combined = handle.revision().unwrap();
    assert_ne!(combined, before.revision, "CONTROL: the combined view");
    let after = handle.history_page_kept(&ask, &mut kept).unwrap();
    assert_eq!(after.revision, combined);
    assert_eq!(after, handle.history_page(&ask).unwrap());
}

/// A page cut from a kept order says the records its read held and does not project
/// ([`super::history::Page::refused`]) as the page read with nothing kept says them, a page that
/// stands on the order's sighting (tracker LT-25C) and reads no record too: the registry holds a
/// version 2 record, which its generation does not take.
/// NEGATIVE CONTROL: with the page that stands on a sighting cut without the order's refused
/// records: `a page that stood on its sighting says other refused records`.
#[test]
fn a_history_page_at_a_held_revision_says_the_records_its_read_refused() {
    use super::history::{Ask, Filter};
    use crate::{said::Said, typed::values::Principal};
    let (lab, handle) = paged_history();
    let stray = Command::Write {
        request: [9; 16],
        changes: vec![put(&MACHINE, MC, "note", json!("unseen"))],
        said: Some(Box::new(Said::by(Principal {
            program: Some("claude".to_owned()),
            ..Principal::default()
        }))),
    };
    let signed = super::said_tests::sign_raw(&lab.paths[0], &REGISTRY, &stray.encode().unwrap());
    let ask = Ask::newest(Filter::default(), 3);
    let unkept = handle.history_page(&ask).unwrap();
    assert_eq!(
        unkept
            .refused
            .iter()
            .map(|unread| unread.operation)
            .collect::<Vec<_>>(),
        [signed],
        "CONTROL: the read refuses the version 2 record"
    );
    let mut kept = Kept::default();
    assert_eq!(handle.history_page_kept(&ask, &mut kept).unwrap(), unkept);
    let sightings = crate::table::store::test_sightings();
    assert_eq!(
        handle.history_page_kept(&ask, &mut kept).unwrap(),
        unkept,
        "a page that stood on its sighting says other refused records"
    );
    let now = crate::table::store::test_sightings();
    assert_eq!((now.0 - sightings.0, now.1 - sightings.1), (1, 1));
    assert_eq!(kept.reads(), 1);
}

/// A catalog's table and a recovery directory's table are never answered from a sighting
/// (tracker LT-25C): `Directory::observe_table_sighted` handed one asks after no sighting and
/// reads the table. A sighting is made of the catalog's primary store, but the catalog is read
/// by no reader that keeps one; a recovery directory reads its replica, which no sighting is of.
/// NEGATIVE CONTROLS: with the catalog's scope not compared: `the catalog was asked after its
/// sighting left: 1 right: 0`. With the recovery directory not compared: `a recovery directory
/// was asked after the primary's sighting left: 1 right: 0`.
#[test]
fn a_catalog_or_recovery_table_is_never_answered_from_a_sighting() {
    use crate::{catalog::Sighted, table::store::test_sightings};
    let (_lab, handle) = paged_history();
    application::observe_store(&handle.paths, handle.descriptor(), |session, _, scope| {
        let directory = crate::catalog::Directory::new(session.paths(), session.trust());
        let sighting_of = |scope| match directory.observe_table_sighted(session, scope, None) {
            Ok(Sighted::Opened(store)) => store.sighted_revision(session).map(|(_, sighting)| {
                sighting.expect("an observed primary store holding no conflict is sighted")
            }),
            Ok(Sighted::Unchanged) => panic!("a table with no sighting was answered unchanged"),
            Err(error) => Err(error),
        };
        let registry = sighting_of(scope)?;
        let catalog = directory.catalog_scope();
        let of_catalog = sighting_of(catalog)?;
        // The registry's own sighting stands: what each read below would otherwise answer.
        let asked = test_sightings().0;
        assert!(matches!(
            directory.observe_table_sighted(session, scope, Some(&registry))?,
            Sighted::Unchanged
        ));
        assert_eq!(test_sightings().0 - asked, 1);

        let asked = test_sightings().0;
        let read = directory.observe_table_sighted(session, catalog, Some(&of_catalog));
        assert_eq!(
            test_sightings().0 - asked,
            0,
            "the catalog was asked after its sighting"
        );
        assert!(matches!(read, Ok(Sighted::Opened(_))));
        let asked = test_sightings().0;
        let read = directory
            .recovery()
            .observe_table_sighted(session, scope, Some(&registry));
        assert_eq!(
            test_sightings().0 - asked,
            0,
            "a recovery directory was asked after the primary's sighting"
        );
        assert!(!matches!(read, Ok(Sighted::Unchanged)));
        Ok(())
    })
    .unwrap();
}

/// The data folder of `handle`'s table on its Mac: the folder whose opens [`counted`] counts.
fn data_folder(handle: &Handle) -> PathBuf {
    application::observe_store(&handle.paths, handle.descriptor(), |session, _, scope| {
        crate::catalog::Directory::new(session.paths(), session.trust()).path(scope)
    })
    .unwrap()
}

/// What `read` answers, with how many opens of the store in data folder `folder` it made past
/// the store's claim (`table::store::with_open_hook`: a sighting asked to stand makes none), and
/// how many sightings it asked to stand and how many of them stood.
fn counted<T>(folder: &Path, read: impl FnOnce() -> T) -> (T, usize, (usize, usize)) {
    let opens = std::rc::Rc::new(std::cell::Cell::new(0));
    let count = std::rc::Rc::clone(&opens);
    let before = crate::table::store::test_sightings();
    let read = crate::table::store::with_open_hook(
        folder,
        move || {
            count.set(count.get() + 1);
            Ok(())
        },
        read,
    );
    let now = crate::table::store::test_sightings();
    (read, opens.get(), (now.0 - before.0, now.1 - before.1))
}

/// A revision asked again by a reader that keeps a sighting opens no store while the store
/// stands (tracker LT-25D, the Tables page's change check): the first ask, nothing kept, opens
/// the registry's store once, as the plain read does each time, and keeps that open's sighting
/// and no order; each ask after it asks the sighting, which stands, opens no store, decodes no
/// record and answers the revision the plain read (`Handle::revision`, a full open) answers.
/// NEGATIVE CONTROL: with `Handle::revision_kept` asking after no sighting (`None` handed to
/// `application::observe_store_sighted`), each ask opens the store: `a revision asked again
/// opened the registry's store left: 1 right: 0`.
#[test]
fn a_revision_asked_again_opens_no_store_while_it_stands() {
    let (_lab, handle) = paged_history();
    let folder = data_folder(&handle);
    let plain = handle.revision().unwrap();
    let mut kept = Kept::default();
    assert_eq!((kept.sighted(), kept.revision()), (None, None));
    let (first, opens, sightings) = counted(&folder, || handle.revision_kept(&mut kept));
    assert_eq!(first.unwrap(), plain);
    assert_eq!(
        (opens, sightings),
        (1, (0, 0)),
        "with nothing kept the store is opened once and no sighting asked"
    );
    assert_eq!(kept.sighted(), Some(plain), "the open's sighting is kept");
    assert_eq!(
        (kept.revision(), kept.reads()),
        (None, 0),
        "a revision read builds no order"
    );
    // CONTROL: the plain read opens the store each time it is asked.
    let (again, opens, _) = counted(&folder, || handle.revision());
    assert_eq!((again.unwrap(), opens), (plain, 1));
    for _ in 0..3 {
        let decoded = super::DECODED.get();
        let (again, opens, sightings) = counted(&folder, || handle.revision_kept(&mut kept));
        assert_eq!(
            again.unwrap(),
            plain,
            "a revision asked again is the one an open reads"
        );
        assert_eq!(
            opens, 0,
            "a revision asked again opened the registry's store"
        );
        assert_eq!(
            sightings,
            (1, 1),
            "a revision asked again stood on the kept sighting"
        );
        assert_eq!(super::DECODED.get(), decoded);
    }
    assert_eq!(handle.revision().unwrap(), plain);
    assert_eq!(kept.sighted(), Some(plain));
    assert_eq!((kept.revision(), kept.reads()), (None, 0));
}

/// A revision asked after the table moved is the new one, never the held one (tracker LT-25D):
/// after a record is written, after a membership change that writes no record (Mac 1 made a
/// Reader), and after a group permit is written beside the claim and again once it is put back,
/// the kept sighting is asked, fails, the store is opened once and the answer is what the plain
/// read answers; that open's sighting is kept in its turn and the next ask stands on it.
/// NEGATIVE CONTROL: with `Handle::revision_kept` answering the kept sighting's revision without
/// asking it: `a revision asked after a record was written is the held one` (`left` and
/// `right` the same revision).
#[test]
fn a_revision_asked_after_the_table_moved_is_the_new_one() {
    let (lab, handle) = paged_history();
    let folder = data_folder(&handle);
    let mut kept = Kept::default();
    let mut held = handle.revision_kept(&mut kept).unwrap();
    let (claims, scope) =
        application::observe_store(&handle.paths, handle.descriptor(), |session, _, scope| {
            Ok((crate::table::store::claim_dir(session)?, scope))
        })
        .unwrap();
    let mut put_back = None;
    for what in [
        "a record was written",
        "a membership change",
        "a group permit was written",
        "the group permit was put back",
    ] {
        match what {
            "a record was written" => Registry::at(handle.paths.clone())
                .write(vec![vec![put(&MACHINE, OTHER, "label", json!("m11"))]])
                .unwrap(),
            "a membership change" => lab.admit(&[Role::Writer, Role::Reader]),
            "a group permit was written" => {
                put_back = Some(crate::table::store::permit_group(&claims, scope));
            }
            _ => put_back.take().unwrap()(),
        }
        let (moved, opens, sightings) = counted(&folder, || handle.revision_kept(&mut kept));
        let moved = moved.unwrap();
        assert_ne!(moved, held, "a revision asked after {what} is the held one");
        assert_eq!(moved, handle.revision().unwrap(), "after {what}");
        assert_eq!(
            (opens, sightings),
            (1, (1, 0)),
            "after {what} the kept sighting failed and the store was opened once"
        );
        assert_eq!(kept.sighted(), Some(moved), "after {what}");
        let (again, opens, sightings) = counted(&folder, || handle.revision_kept(&mut kept));
        assert_eq!(again.unwrap(), moved, "after {what}");
        assert_eq!((opens, sightings), (0, (1, 1)), "after {what}");
        held = moved;
    }
}

/// A revision read and History's pages keep one sighting between them (tracker LT-25D). A
/// reader whose first read is a revision read keeps that open's sighting and no order; the
/// History page after it must read the records, so it opens the store once, asking after no
/// sighting (it holds no order to cut from), and the two are of one revision: with a sighting
/// kept, a revision read and then the first page open the store once in total. From then a
/// revision read and a page, in any order, open no store, each standing on the one kept
/// sighting. A reader whose first read is a page then reads the revision without an open. Once a
/// record is written, the revision read opens the store, reads the new revision and lets the
/// order of the old one go; the page after it reads the new revision's records, says the table
/// moved and heads with the new record, never a page of the old order.
/// NEGATIVE CONTROLS: with a page that reads the store keeping no sighting of its open
/// (`Served::Read` leaving `kept.seen` empty): `the page that read the store keeps its order
/// and its open's sighting left: (Some(..), None, 1) right: (Some(..), Some(..), 1)`. With a
/// page asking after the kept sighting whatever order it holds, the first page after a revision
/// read is refused: `a history sighting stood with no order kept`. With the revision read
/// leaving the old order kept: `the revision read let the order of the revision the table left
/// go left: (Some(..), Some(..)) right: (None, Some(..))`, the page after it still read from the
/// store. With the page asking after the sighting of any order of its home and table (in its
/// ask and where the sighting stood), the test passes: the revision read let that order go.
/// With both, the page after the write is cut from the old order while the sighting of the new
/// revision stands: `the page after a moved revision read is of the new revision` (`left` the
/// revision the table left).
#[test]
fn a_revision_read_and_a_history_page_share_one_kept_sighting() {
    use super::history::{Ask, Filter};
    let (_lab, handle) = paged_history();
    let folder = data_folder(&handle);
    let ask = Ask::newest(Filter::default(), 3);
    let mut kept = Kept::default();
    let revision = handle.revision_kept(&mut kept).unwrap();
    // A sighting kept and no order: the revision read stands on it, and the page reads the
    // records in the one open the two make.
    let ((again, page), opens, sightings) = counted(&folder, || {
        let again = handle.revision_kept(&mut kept).unwrap();
        (again, handle.history_page_kept(&ask, &mut kept).unwrap())
    });
    assert_eq!(again, revision);
    assert_eq!(
        (opens, sightings),
        (1, (1, 1)),
        "a revision read and the first page after it opened the store once in total"
    );
    assert_eq!(page.revision, revision);
    assert_eq!(page, handle.history_page(&ask).unwrap());
    assert_eq!(
        (kept.revision(), kept.sighted(), kept.reads()),
        (Some(revision), Some(revision), 1),
        "the page that read the store keeps its order and its open's sighting"
    );
    // From then neither opens the store, in either order.
    let (served, opens, sightings) = counted(&folder, || {
        [
            handle.revision_kept(&mut kept).unwrap() == revision,
            handle.history_page_kept(&ask, &mut kept).unwrap() == page,
            handle.history_page_kept(&ask, &mut kept).unwrap() == page,
            handle.revision_kept(&mut kept).unwrap() == revision,
        ]
    });
    assert_eq!(served, [true; 4]);
    assert_eq!(
        (opens, sightings),
        (0, (4, 4)),
        "a revision read and a page at the held revision each stood on the one kept sighting"
    );
    assert_eq!(kept.reads(), 1);

    // A page first: the revision read after it stands on the page's sighting.
    let mut paged = Kept::default();
    assert_eq!(handle.history_page_kept(&ask, &mut paged).unwrap(), page);
    let (after, opens, sightings) = counted(&folder, || handle.revision_kept(&mut paged));
    assert_eq!(after.unwrap(), revision);
    assert_eq!(opens, 0, "a revision read after a page opened the store");
    assert_eq!(sightings, (1, 1));

    // A record written: the revision read opens the store and lets the old order go, and the
    // page after it is of the new revision.
    Registry::at(handle.paths.clone())
        .write(vec![vec![put(&MACHINE, OTHER, "label", json!("m11"))]])
        .unwrap();
    let (moved, opens, sightings) = counted(&folder, || handle.revision_kept(&mut kept));
    let moved = moved.unwrap();
    assert_ne!(moved, revision);
    assert_eq!(moved, handle.revision().unwrap());
    assert_eq!((opens, sightings), (1, (1, 0)));
    let let_go = (kept.revision(), kept.sighted());
    let older = Ask {
        revision: Some(revision),
        ..ask.clone()
    };
    let (after, opens, sightings) =
        counted(&folder, || handle.history_page_kept(&older, &mut kept));
    let after = after.unwrap();
    assert_eq!(
        after.revision, moved,
        "the page after a moved revision read is of the new revision"
    );
    assert!(after.moved);
    assert_eq!(
        after.entries[0].changes,
        [put(&MACHINE, OTHER, "label", json!("m11"))],
        "the new record heads the page"
    );
    assert_eq!(after, handle.history_page(&older).unwrap());
    assert_eq!((opens, sightings), (1, (0, 0)));
    assert_eq!(
        (kept.revision(), kept.sighted(), kept.reads()),
        (Some(moved), Some(moved), 2)
    );
    assert_eq!(
        let_go,
        (None, Some(moved)),
        "the revision read let the order of the revision the table left go"
    );
    // The reader that read a page first: its page reads the new revision, and its revision read
    // after that opens nothing.
    let (after, opens, sightings) =
        counted(&folder, || handle.history_page_kept(&older, &mut paged));
    assert_eq!(after.unwrap().revision, moved);
    assert_eq!((opens, sightings), (1, (1, 0)));
    let (after, opens, sightings) = counted(&folder, || handle.revision_kept(&mut paged));
    assert_eq!(after.unwrap(), moved);
    assert_eq!((opens, sightings), (0, (1, 1)));
}

/// A kept sighting is asked only of the home and the table it was made of (tracker LT-25D): a
/// revision asked with a sighting of another Mac's registry, or of another table at the same
/// home (the tracker), asks after no sighting, opens that table's store once and answers what
/// the plain read answers there; the sighting kept is then that open's.
/// NEGATIVE CONTROL: with a kept sighting taken to be of any home and table (`Seen::is_of`
/// answering `true`), the sighting is asked of a store it was not made of, whose claim refuses
/// it: `a sighting of another home was asked left: 1 right: 0`.
#[test]
fn a_revision_asked_with_a_sighting_of_another_home_or_table_opens_the_store() {
    let (lab, handle) = paged_history();
    let mut kept = Kept::default();
    let here = handle.revision_kept(&mut kept).unwrap();
    assert_eq!(kept.sighted(), Some(here));

    let peer = Handle::at(lab.paths[1].clone(), &REGISTRY);
    let (there, opens, sightings) = counted(&data_folder(&peer), || peer.revision_kept(&mut kept));
    let there = there.unwrap();
    assert_eq!(sightings.0, 0, "a sighting of another home was asked");
    assert_eq!(opens, 1, "the other home's store is opened");
    assert_eq!(there, peer.revision().unwrap());
    assert_ne!(there, here, "CONTROL: two homes, two revisions");
    assert_eq!(kept.sighted(), Some(there));
    // And back: the peer's sighting is not asked of this home either.
    let (back, opens, sightings) =
        counted(&data_folder(&handle), || handle.revision_kept(&mut kept));
    assert_eq!(back.unwrap(), here);
    assert_eq!((opens, sightings.0), (1, 0));

    crate::file::tests::declare(&lab.paths[0], &TRACKER);
    let tracker = Handle::at(lab.paths[0].clone(), &TRACKER);
    let (other, opens, sightings) =
        counted(&data_folder(&tracker), || tracker.revision_kept(&mut kept));
    let other = other.unwrap();
    assert_eq!(sightings.0, 0, "a sighting of another table was asked");
    assert_eq!(opens, 1, "the other table's store is opened");
    assert_eq!(other, tracker.revision().unwrap());
    assert_ne!(other, here, "CONTROL: two tables, two revisions");
    assert_eq!(kept.sighted(), Some(other));
    // Each stands on a sighting of its own.
    let mut registry = Kept::default();
    assert_eq!(handle.revision_kept(&mut registry).unwrap(), here);
    let (again, opens, sightings) =
        counted(&data_folder(&tracker), || tracker.revision_kept(&mut kept));
    assert_eq!((again.unwrap(), opens, sightings), (other, 0, (1, 1)));
    let (again, opens, sightings) = counted(&data_folder(&handle), || {
        handle.revision_kept(&mut registry)
    });
    assert_eq!((again.unwrap(), opens, sightings), (here, 0, (1, 1)));
}

/// A kept sighting is of the home whose store was opened, never of the path its handle was made
/// at (tracker LT-25D). Once this Mac's writer is succeeded the same handle reads the successor
/// home's store, in another data folder at another revision. The sighting a revision read kept,
/// and the one a page kept with its order, are of the original home's store: neither read asks
/// it of the successor's. Each opens the successor's store once, asking after no sighting, and
/// answers what the plain read answers there; the sighting kept is then of the successor home,
/// and the next revision read and page stand on it and open nothing.
/// NEGATIVE CONTROL: with `application::observe_store_sighted` handing the reader the path the
/// read was asked at in place of the home it selected, the sighting of the original home's
/// store is asked of the successor's, whose claim refuses it: `a revision read asked the
/// successor home's store after a sighting of the original home's left: (1, (1, 0)) right: (1,
/// (0, 0))`. With a page asking the kept sighting wherever it is of the handle's path
/// (`Seen::is_of` handed `self.paths` in `read_history_page`): `a page asked the successor
/// home's store after a sighting of the original home's left: (1, (1, 0)) right: (1, (0, 0))`.
#[test]
fn a_sighting_kept_before_a_succession_is_not_asked_of_the_successor_home() {
    use super::history::{Ask, Filter};
    let mut lab = Lab::new(&[Role::Writer, Role::Writer]);
    let registry = Registry::at(lab.paths[0].clone());
    registry.open().unwrap();
    registry
        .write(vec![machine(MC), other_machine(OTHER, "m8")])
        .unwrap();
    let handle = Handle::at(lab.paths[0].clone(), &REGISTRY);
    let ask = Ask::newest(Filter::default(), 3);
    let mut marked = Kept::default();
    let held = handle.revision_kept(&mut marked).unwrap();
    let mut paged = Kept::default();
    assert_eq!(
        handle.history_page_kept(&ask, &mut paged).unwrap().revision,
        held
    );
    let before = data_folder(&handle);
    // CONTROL: before the succession each stands on its sighting.
    let (stood, opens, sightings) = counted(&before, || {
        (
            handle.revision_kept(&mut marked).unwrap(),
            handle.history_page_kept(&ask, &mut paged).unwrap().revision,
        )
    });
    assert_eq!((stood, opens, sightings), ((held, held), 0, (2, 2)));

    crate::generation::tests::succeed(&mut lab);
    let after = data_folder(&handle);
    assert_ne!(
        after, before,
        "CONTROL: the same handle now reads another home's store"
    );
    let plain = handle.revision().unwrap();
    assert_ne!(plain, held, "CONTROL: two homes, two revisions");

    let (mark, opens, sightings) = counted(&after, || handle.revision_kept(&mut marked));
    assert_eq!(mark.unwrap(), plain, "the revision is the successor home's");
    assert_eq!(
        (opens, sightings),
        (1, (0, 0)),
        "a revision read asked the successor home's store after a sighting of the original \
         home's"
    );
    assert_eq!(marked.sighted(), Some(plain));
    let (page, opens, sightings) = counted(&after, || handle.history_page_kept(&ask, &mut paged));
    assert_eq!(page.unwrap(), handle.history_page(&ask).unwrap());
    assert_eq!(
        (opens, sightings),
        (1, (0, 0)),
        "a page asked the successor home's store after a sighting of the original home's"
    );
    assert_eq!(
        (paged.revision(), paged.sighted()),
        (Some(plain), Some(plain))
    );
    // The sightings kept now are of the successor home: each read stands on its own.
    let (stood, opens, sightings) = counted(&after, || {
        (
            handle.revision_kept(&mut marked).unwrap(),
            handle.history_page_kept(&ask, &mut paged).unwrap().revision,
            handle.revision_kept(&mut paged).unwrap(),
        )
    });
    assert_eq!(
        (stood, opens, sightings),
        ((plain, plain, plain), 0, (3, 3)),
        "a sighting made of the successor home's store stands for it"
    );
}

/// A refused read leaves the kept sighting as it was, never one of a store it did not read
/// (tracker LT-25D). A group permit written beside the registry's claim fails the kept sighting,
/// and the open the read falls back to refuses: the revision read, and a page, are refused with
/// that open's refusal, and the sighting and the order kept are the ones held before. With the
/// permit put back, the store standing byte for byte as the sighting read it, the next revision
/// read and page stand on that sighting and open nothing. Where the store did move (a record
/// written) and the open refuses, the read is refused and never answered the held revision; the
/// next read that opens the store answers the new one.
/// NEGATIVE CONTROLS: with `Handle::revision_kept` taking the sighting out of `kept` before its
/// read and putting none back when refused: `a refused revision read leaves the kept sighting
/// as it was left: None right: Some(Revision(..))`. With a refused page putting no sighting
/// back: `a refused page leaves the kept sighting as it was left: None right: Some(..)`.
#[test]
fn a_refused_revision_read_leaves_the_kept_sighting_as_it_was() {
    use super::history::{Ask, Filter};
    let (_lab, handle) = paged_history();
    let folder = data_folder(&handle);
    let ask = Ask::newest(Filter::default(), 3);
    let mut kept = Kept::default();
    let page = handle.history_page_kept(&ask, &mut kept).unwrap();
    let held = handle.revision_kept(&mut kept).unwrap();
    assert_eq!(page.revision, held);
    let (claims, scope) =
        application::observe_store(&handle.paths, handle.descriptor(), |session, _, scope| {
            Ok((crate::table::store::claim_dir(session)?, scope))
        })
        .unwrap();
    // `read`, every open of the registry's store refusing; with the sightings it asked and
    // those that stood.
    fn refusing<T>(folder: &Path, read: impl FnOnce() -> Result<T>) -> (String, (usize, usize)) {
        let before = crate::table::store::test_sightings();
        let refusal = crate::table::store::with_open_hook(
            folder,
            || Err(Error::new("the open refuses")),
            read,
        )
        .err()
        .expect("the read is refused")
        .to_string();
        let now = crate::table::store::test_sightings();
        (refusal, (now.0 - before.0, now.1 - before.1))
    }

    let put_back = crate::table::store::permit_group(&claims, scope);
    let (refusal, sightings) = refusing(&folder, || handle.revision_kept(&mut kept));
    assert!(refusal.contains("the open refuses"), "{refusal}");
    assert_eq!(sightings, (1, 0), "the sighting failed before the open");
    assert_eq!(
        kept.sighted(),
        Some(held),
        "a refused revision read leaves the kept sighting as it was"
    );
    let (refusal, sightings) = refusing(&folder, || handle.history_page_kept(&ask, &mut kept));
    assert!(refusal.contains("the open refuses"), "{refusal}");
    assert_eq!(sightings, (1, 0));
    assert_eq!(
        kept.sighted(),
        Some(held),
        "a refused page leaves the kept sighting as it was"
    );
    assert_eq!((kept.revision(), kept.reads()), (Some(held), 1));
    put_back();
    let (served, opens, sightings) = counted(&folder, || {
        (
            handle.revision_kept(&mut kept).unwrap(),
            handle.history_page_kept(&ask, &mut kept).unwrap(),
        )
    });
    assert_eq!(served, (held, page));
    assert_eq!(
        (opens, sightings),
        (0, (2, 2)),
        "the sighting a refused read left stands for the store it read"
    );

    // The store moved and the open refuses: no answer, and none of the held revision.
    Registry::at(handle.paths.clone())
        .write(vec![vec![put(&MACHINE, OTHER, "label", json!("m11"))]])
        .unwrap();
    let (refusal, sightings) = refusing(&folder, || handle.revision_kept(&mut kept));
    assert!(refusal.contains("the open refuses"), "{refusal}");
    assert_eq!(sightings, (1, 0));
    assert_eq!(kept.sighted(), Some(held));
    let (moved, opens, sightings) = counted(&folder, || handle.revision_kept(&mut kept));
    let moved = moved.unwrap();
    assert_ne!(
        moved, held,
        "a changed store is never answered the held revision"
    );
    assert_eq!(moved, handle.revision().unwrap());
    assert_eq!((opens, sightings), (1, (1, 0)));
    assert_eq!((kept.sighted(), kept.revision()), (Some(moved), None));
}

/// Seal, on Mac `from`, a typed write whose causal parent never arrives anywhere, and give it to
/// Mac `to`: the record a slow or partitioned peer leaves waiting (`Log::awaited`).
fn withhold_on(lab: &Lab, from: usize, to: usize, changes: Vec<crate::database::Change>) {
    let command = Command::Write {
        request: crate::crypto::random().unwrap(),
        changes,
        said: None,
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
            said: None,
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
            said: None,
        },
        then + 2_000,
    );
    repair_registry(&lab, 2);
    append_history_at(
        &here,
        Command::Write {
            request: [93; 16],
            changes: vec![put(&MACHINE, MC, "nickname", json!("spare"))],
            said: None,
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
    assert!(matches!(
        here.checkpoint_every(&older, now, 1).unwrap(),
        Signed::Fenced
    ));
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

/// A history entry names its writer by the machine row that writer's own `machine-facts` row
/// binds (`merged_into`, else `mac`), with the row's label as the grid shows it (a live row's
/// settled claim) and, where another row shares that label (a merged row keeps the label it
/// had), its key's ending; a writer no facts row binds by its Commons machine id; a row whose
/// label is conflicted by its key; and a predecessor (writer succession) by what its successor
/// binds. `aterm db table history` prints the same in its JSON, with the actor, and as one TSV
/// line per changed cell.
/// NEGATIVE CONTROL: with `Macs::from_rows` binding each facts row's `mac` before its
/// `merged_into`, Mac 1 is named by the row it was merged away from: `assertion `left == right`
/// failed: Mac 1, by the row its merged_into names  left: Some("mc-000000000000000b")  right:
/// Some("mc-000000000000000c")`.
#[test]
fn a_history_entry_names_its_writers_machine_row() {
    use crate::{
        alab::registry::MACHINE_FACTS,
        att::history::{Mac, Macs},
        typed::owned::{OwnedRead, OwnedRow},
    };
    let (b, c) = ("mc-000000000000000b", "mc-000000000000000c");
    let lab = Lab::new(&[Role::Writer, Role::Writer, Role::Writer]);
    let here = Registry::at(lab.paths[0].clone());
    here.open().unwrap();
    let facts = |n: usize, value: Value| {
        vec![Change {
            collection: MACHINE_FACTS.name.into(),
            key: keys::uuid32(&lab.machine(n)),
            value: Some(value),
        }]
    };
    // Mac 1's first row was merged into another: the merged row keeps the label the live one
    // holds.
    let mut merged = other_machine(b, "m55");
    merged.pop();
    merged.push(put(&MACHINE, b, "row", json!("merged")));
    merged.push(put(&MACHINE, b, "merged_into", json!(c)));
    here.write(vec![
        other_machine(MC, "m73"),
        other_machine(c, "m55"),
        merged,
        facts(0, json!({"r": 1, "n": 1, "mac": MC, "label": "m0"})),
        // Not its signer's own row: inert.
        facts(1, json!({"r": 1, "n": 1, "mac": MC, "label": "m0"})),
    ])
    .unwrap();
    repair_new_registry(&lab);
    Registry::at(lab.paths[1].clone())
        .write(vec![
            facts(
                1,
                json!({"r": 1, "n": 1, "mac": b, "merged_into": c, "label": "m1"}),
            ),
            vec![put(&MACHINE, MC, "location", json!("rack z"))],
        ])
        .unwrap();
    Registry::at(lab.paths[2].clone())
        .write(vec![vec![put(&MACHINE, MC, "status", json!("parked"))]])
        .unwrap();
    repair_registry(&lab, 2);

    let handle = Handle::at(lab.paths[0].clone(), &REGISTRY);
    let history = handle.history().unwrap();
    let by = |n: usize| {
        let machine = lab.machine(n);
        let macs: BTreeSet<_> = history
            .iter()
            .filter(|entry| entry.machine == Some(machine))
            .map(|entry| entry.mac.clone())
            .collect();
        assert_eq!(
            macs.len(),
            1,
            "one name for every entry of Mac {n}: {macs:?}"
        );
        macs.into_iter().next().unwrap()
    };
    let (zero, one, two) = (by(0), by(1), by(2));
    assert_eq!(zero.row(), Some(MC), "Mac 0, by the row its mac names");
    assert_eq!(zero.name(), "m73", "a label no other row holds, alone");
    assert_eq!(
        one.row(),
        Some(c),
        "Mac 1, by the row its merged_into names"
    );
    assert_eq!(
        one.name(),
        "m55 (\u{2026}000c)",
        "a shared label, with the key's ending"
    );
    assert_eq!(
        two,
        Mac::Machine(lab.machine(2)),
        "no facts row binds Mac 2"
    );
    assert_eq!(two.name(), keys::uuid32(&lab.machine(2)));

    // The command prints the actor and the Mac, and a line per changed cell.
    let page = run(&lab.paths[0], &["history", "alab/registry", "machine", MC]);
    let Output::Json(page) = page else {
        panic!("history answers JSON")
    };
    let written = page["operations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|op| {
            op["changes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|change| change["value"] == json!("rack z"))
        })
        .unwrap();
    assert_eq!(written["actor"].as_str().map(str::len), Some(96));
    assert_eq!(
        written["mac"],
        json!({"name": "m55 (\u{2026}000c)", "row": c})
    );
    let tsv = text(&run(
        &lab.paths[0],
        &["history", "alab/registry", "machine", MC, "--tsv"],
    ));
    let mut lines = tsv.lines();
    assert_eq!(
        lines.next(),
        Some(
            "time\tmac\tkind\tinert\tcollection\trow\tcolumn\tvalue\toperation\tby\treason\tevidence"
        )
    );
    let line = lines
        .find(|line| line.contains("rack z"))
        .unwrap_or_else(|| panic!("a line for Mac 1's write: {tsv}"));
    let fields: Vec<&str> = line.split('\t').collect();
    assert_eq!(
        fields[1..8],
        [
            "m55 (\u{2026}000c)",
            "explicit",
            "",
            "machine",
            MC,
            "location",
            "rack z"
        ]
    );
    assert_eq!(fields[0].len(), "2026-11-15 00:00:00Z".len());
    assert!(fields[0].ends_with('Z'), "{line}");
    let every = text(&run(&lab.paths[0], &["history", "alab/registry", "--tsv"]));
    assert!(
        every
            .lines()
            .any(|line| line.contains("\tm73\taterm\t\tmachine-facts\t")),
        "Mac 0's facts row, as aterm's: {every}"
    );
    assert!(
        every
            .lines()
            .any(|line| line.contains("\tm73\taterm\trows\tmachine-facts\t")),
        "Mac 0's write of Mac 1's facts row, which never takes effect: {every}"
    );
    let paged = text(&run(
        &lab.paths[0],
        &["history", "alab/registry", "--limit", "1", "--tsv"],
    ));
    assert!(
        paged
            .lines()
            .last()
            .is_some_and(|line| line.starts_with("# more: --after ")),
        "{paged}"
    );

    // Two Macs relabel one row apart: its label names no one, and the row's key does.
    here.write(vec![vec![put(&MACHINE, MC, "label", json!("m74"))]])
        .unwrap();
    Registry::at(lab.paths[1].clone())
        .write(vec![vec![put(&MACHINE, MC, "label", json!("m75"))]])
        .unwrap();
    repair_registry(&lab, 2);
    let view = handle.view().unwrap();
    assert!(view.project(&MACHINE).rows[MC].fields["label"].conflicted());
    let history = handle.history().unwrap();
    let first = history
        .iter()
        .find(|entry| entry.machine == Some(lab.machine(0)))
        .unwrap();
    assert_eq!(first.mac.name(), MC, "a conflicted label names no one");

    // A predecessor whose own facts row binds nothing (6), or that has none (7), binds what its
    // successor binds, however far back; one whose own row binds a machine row keeps it (5).
    let row = |machine: [u8; 16], value: Value| {
        (
            keys::uuid32(&machine),
            OwnedRow {
                machine,
                actor: [0; 48],
                value,
                rank: (0, 0, 0),
                operation: [0; 32],
            },
        )
    };
    let succeeded = OwnedRead {
        rows: [
            row(
                [5; 16],
                json!({"mac": b, "predecessor": keys::uuid32(&[6; 16])}),
            ),
            row([6; 16], json!({"predecessor": keys::uuid32(&[7; 16])})),
            row(
                [8; 16],
                json!({"mac": c, "predecessor": keys::uuid32(&[5; 16])}),
            ),
        ]
        .into_iter()
        .collect(),
        inert: 0,
    };
    let macs = Macs::from_rows(&view.project(&MACHINE), &succeeded);
    let of = |n: u8| macs.name(&[0; 48], Some(&[n; 16])).row().map(str::to_owned);
    assert_eq!(of(6).as_deref(), Some(b), "a predecessor");
    assert_eq!(of(7).as_deref(), Some(b), "a predecessor's predecessor");
    assert_eq!(of(5).as_deref(), Some(b), "a predecessor's own row");
    assert_eq!(of(9), None);
    assert_eq!(macs.name(&[3; 48], None), Mac::Actor([3; 48]));
}

/// A table's floor (tracker LT-B10C1): a writer refused at its quota is refused again while the
/// checkpoint pass is fenced; once the pass proposes a checkpoint of its records, final at once
/// since it is the only member that wrote the table in the 30 days before the cut, the pass
/// records it as the table's floor, and the writer writes on, a quota's worth more, through
/// fresh opens that read the floor back. The table reads every record.
/// NEGATIVE CONTROL: with `Floor::covers` answering `false`, the floor holds nothing cold and the
/// writer is refused at once: `the writer writes on past its quota: 0`.
#[test]
fn a_writer_past_its_quota_writes_on_once_a_checkpoint_is_final() {
    use super::checkpoint::Signed;
    use aterm_secrets::commons::hearing::{self, Ledger, Said};
    const DAY: u64 = 24 * 60 * 60 * 1000;
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
            said: None,
        },
        then,
    );
    let (first, refused) = append_bulk_at(&here, crate::table::store::WRITER_RECORDS * 2, then);
    let refused = refused.expect("a writer is refused at its quota");
    assert!(refused.contains("writer evidence quota"), "{refused}");
    assert!(first > 4000, "{first}");
    let held = || {
        application::observe_store(&lab.paths[0], here.descriptor(), |session, store, _| {
            store.retained_by(session, session.actor())
        })
        .unwrap()
    };
    let at_quota = held();

    let bootstrap = lab.bootstrap(0);
    let access = bootstrap.access().unwrap().unwrap();
    let heard_at = |version| {
        let mut ledger = Ledger::new(bootstrap.trust());
        for member in access.members() {
            ledger.record(member.binding().actor(), now, Said::Version(version));
        }
        ledger
    };
    let older = heard_at(hearing::Version(0, 202, 0));
    assert!(matches!(
        here.checkpoint_every(&older, now, 1).unwrap(),
        Signed::Fenced
    ));
    assert_eq!(
        append_bulk_at(&here, 1, then).0,
        0,
        "no floor, no room: the writer is still refused"
    );

    let ready = heard_at(hearing::READS_CHECKPOINTS);
    assert_eq!(
        here.checkpoint_every(&ready, now, 1).unwrap(),
        Signed::Proposed
    );
    let floor = application::observe_store(&lab.paths[0], here.descriptor(), |_, store, _| {
        Ok(store.floor().cloned())
    })
    .unwrap()
    .expect("the pass records the final checkpoint as the floor");
    assert!(floor.below > first as u64, "{} below", floor.below);

    let (second, refused) = append_bulk_at(&here, crate::table::store::WRITER_RECORDS * 2, now);
    assert!(
        second > 4000,
        "the writer writes on past its quota: {second}"
    );
    let refused = refused.expect("and is refused at its quota again");
    assert!(refused.contains("writer evidence quota"), "{refused}");

    let rows = here.view().unwrap();
    assert!(rows.refused.is_empty(), "{:?}", rows.refused);
    assert_eq!(
        held(),
        at_quota + 1 + second,
        "every record is held, the checkpoint's too"
    );
}

/// Every file named `name` under `root`.
fn files_named(root: &Path, name: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                dirs.push(path);
            } else if kind.is_file() && entry.file_name() == name {
                found.push(path);
            }
        }
    }
    found
}

/// A read after a final checkpoint opens no record at or below it (tracker LT-B10C3): once the
/// pass records a final checkpoint of 501 records as the floor, having kept each one's payload
/// sealed to this Mac (`table::store::cold`), and the writer writes 20 more, a read opens the
/// payload of each record above the floor and the cold file, and none below; it reads exactly
/// what a read that opens every record reads (the cold file moved aside): its cells, operations,
/// owned rows, quota and refusals.
/// NEGATIVE CONTROL: with `Store::cold_payloads` answering none, the read opens every record, the
/// 501 below the floor included: `left: 0, right: 500`.
#[test]
fn a_read_after_a_final_checkpoint_opens_no_body_below_it() {
    use super::checkpoint::Signed;
    use crate::crypto::test_cipher_constructions;
    use aterm_secrets::commons::hearing::{self, Ledger, Said};
    const DAY: u64 = 24 * 60 * 60 * 1000;
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
            said: None,
        },
        then,
    );
    assert_eq!(append_bulk_at(&here, 500, then), (500, None));
    let bootstrap = lab.bootstrap(0);
    let access = bootstrap.access().unwrap().unwrap();
    let mut ready = Ledger::new(bootstrap.trust());
    for member in access.members() {
        ready.record(
            member.binding().actor(),
            now,
            Said::Version(hearing::READS_CHECKPOINTS),
        );
    }
    assert_eq!(
        here.checkpoint_every(&ready, now, 1).unwrap(),
        Signed::Proposed
    );
    let floor = application::observe_store(&lab.paths[0], here.descriptor(), |_, store, _| {
        Ok(store.floor().cloned())
    })
    .unwrap()
    .expect("the pass records the final checkpoint as the floor");
    assert_eq!(floor.below, 501);
    assert_eq!(append_bulk_at(&here, 20, now), (20, None));
    let held = application::observe_store(&lab.paths[0], here.descriptor(), |session, store, _| {
        Ok(store.inventory(session)?.len())
    })
    .unwrap();
    // The 501 below, the checkpoint record, the 20 above.
    assert_eq!(held, 522);

    let read = || {
        let before = test_cipher_constructions();
        let view = here.view().unwrap();
        (view, test_cipher_constructions() - before)
    };
    let (floored, opened) = read();
    let cold = files_named(lab._home.path(), "cold");
    assert_eq!(cold.len(), 1, "{cold:?}");
    let aside = cold[0].with_file_name("cold.aside");
    std::fs::rename(&cold[0], &aside).unwrap();
    let (full, opened_all) = read();
    std::fs::rename(&aside, &cold[0]).unwrap();
    assert!(
        opened_all >= held,
        "a read without the cold file opens every record: {opened_all}"
    );
    assert_eq!(
        opened_all - opened,
        501 - 1,
        "the read opened {opened} payloads, {opened_all} without the cold file: none of the 501 \
         below the floor, and the cold file once"
    );
    assert_eq!(floored.cells, full.cells);
    assert_eq!(
        floored.ops.iter().map(|op| op.id).collect::<Vec<_>>(),
        full.ops.iter().map(|op| op.id).collect::<Vec<_>>()
    );
    assert_eq!(floored.owned, full.owned);
    assert_eq!(floored.quota, full.quota);
    assert_eq!(floored.refused, full.refused);
    assert_eq!(floored.complete, full.complete);
}

/// Three Macs write 21,000 operations under seven final checkpoints, as
/// [`table_outlives_operation_cap_via_checkpoint_writes`] describes; answers the lab with how
/// many operations were written, and each Mac's handle. The writes run once a process, whichever
/// test asks first, every check of [`operations_under_checkpoints`] with them, and the Macs they
/// leave are kept for the run: each caller holds them alone until it drops the guard, and puts
/// back what it moves. They are not copied per caller, since a table's custody holds its folder's
/// inode and refuses a copy as a moved table.
fn twenty_thousand_operations_under_checkpoints()
-> (std::sync::MutexGuard<'static, (Lab, usize)>, Vec<Handle>) {
    static BUILT: std::sync::OnceLock<std::sync::Mutex<(Lab, usize)>> = std::sync::OnceLock::new();
    let built = BUILT
        .get_or_init(|| {
            let (lab, _, written) = operations_under_checkpoints(7, 1_000);
            aterm_spec::fs_lane::remove_at_exit(lab._home.path());
            std::sync::Mutex::new((lab, written))
        })
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    aterm_spec::fs_lane::join();
    let macs = built
        .0
        .paths
        .iter()
        .map(|paths| Handle::at(paths.clone(), &REGISTRY))
        .collect();
    (built, macs)
}

/// Three Macs write `each` operations apiece in each of `rounds` rounds, each round under a final
/// checkpoint, as [`table_outlives_operation_cap_via_checkpoint_writes`] describes: a round's
/// `3 * each` records, its acknowledgements and its checkpoint's signatures stay within the
/// table's 8,192 operations above the floor and each writer's 4,096. Answers the lab, each Mac's
/// handle and how many operations were written.
fn operations_under_checkpoints(rounds: u64, each: usize) -> (Lab, Vec<Handle>, usize) {
    use super::checkpoint::Signed;
    use aterm_secrets::commons::hearing::{self, Ledger, Said};
    const DAY: u64 = 24 * 60 * 60 * 1000;
    let start = aterm_time::unix_millis().unwrap();
    let lab = Lab::new(&[Role::Writer, Role::Writer, Role::Writer]);
    Registry::at(lab.paths[0].clone()).open().unwrap();
    let macs: Vec<Handle> = (0..3)
        .map(|n| Handle::at(lab.paths[n].clone(), &REGISTRY))
        .collect();
    append_history_at(
        &macs[0],
        Command::Write {
            request: [91; 16],
            changes: machine(MC),
            said: None,
        },
        start - 20 * DAY,
    );
    repair_new_registry(&lab);
    repair_registry(&lab, 2);

    let bootstrap = lab.bootstrap(0);
    let access = bootstrap.access().unwrap().unwrap();
    let ready = |now| {
        let mut ledger = Ledger::new(bootstrap.trust());
        for member in access.members() {
            ledger.record(
                member.binding().actor(),
                now,
                Said::Version(hearing::READS_CHECKPOINTS),
            );
        }
        ledger
    };
    let mut written = 1;
    for round in 0..rounds {
        // Round 0's records are 20 days old; round r's sit just inside the cut of a pass whose
        // clock is 15 days a round past the start.
        let now = start + 15 * DAY * round;
        let authored = if round == 0 {
            start - 20 * DAY
        } else {
            now - 15 * DAY + 1_000
        };
        for mac in &macs {
            let (count, refused) = append_bulk_at(mac, each, authored);
            assert_eq!(refused, None, "write refused in round {round}");
            assert_eq!(count, each);
            written += count;
        }
        copy_registry(&lab);
        let round_written = written;
        for mac in &macs {
            assert_eq!(
                append_bulk_at(mac, 1, authored),
                (1, None),
                "round {round}'s ack"
            );
            written += 1;
        }
        copy_registry(&lab);
        let ledger = ready(now);
        assert_eq!(
            macs[0].checkpoint_every(&ledger, now, 1).unwrap(),
            Signed::Proposed,
            "round {round}"
        );
        copy_registry(&lab);
        for mac in &macs[1..] {
            assert_eq!(
                mac.checkpoint_every(&ledger, now, 1).unwrap(),
                Signed::Seconded,
                "round {round}"
            );
        }
        copy_registry(&lab);
        macs[0].checkpoint_every(&ledger, now, 1).unwrap();
        for (n, mac) in macs.iter().enumerate() {
            let floor =
                application::observe_store(&lab.paths[n], mac.descriptor(), |_, store, _| {
                    Ok(store.floor().map(|body| body.below))
                })
                .unwrap();
            assert!(
                floor.is_some_and(|below| below as usize >= round_written),
                "Mac {n}'s floor after round {round}: {floor:?} for {round_written} written"
            );
        }
    }
    (lab, macs, written)
}

/// A table outlives its fixed operation counts (tracker LT-B10C2): three writers write 21,000
/// operations, 1,000 each a round for seven rounds, where the table's quota is 8,192 operations
/// and each writer's 4,096. After each round every writer acknowledges the others' records with
/// one of its own, since a cut holds only what every current writer has seen; then Mac 0 proposes
/// a checkpoint of that round, Macs 1 and 2 second it,
/// it is final at two of three, and every Mac's pass records it as the floor; no write and no
/// copy between the Macs' stores is refused, and every Mac holds every record. The copies go
/// through each store's own admission and quotas, as a repair's batches do, without the mesh:
/// syncing thousands of records at a 16-record batch that reopens the store each time is tracker
/// LT-B5R's. Each round's records are authored inside
/// that round, and the pass's clock moves 15 days a round, so each cut reaches past the round
/// before, its checkpoint records included.
/// NEGATIVE CONTROL: with the journal's own record count restored (`records: operations * 2 +
/// conflicts` in `Capacity::journal`), the store refuses the copies once it holds about 16,400
/// records: `table custody witness is not contiguous`. Without the passes, LT-B10C1's control:
/// the writers are refused at their quota.
#[test]
fn table_outlives_operation_cap_via_checkpoint_writes() {
    let (built, macs) = twenty_thousand_operations_under_checkpoints();
    let (lab, written) = (&built.0, built.1);
    assert!(written > 20_000, "{written}");
    let held: Vec<usize> = macs
        .iter()
        .enumerate()
        .map(|(n, mac)| {
            application::observe_store(&lab.paths[n], mac.descriptor(), |session, store, _| {
                Ok(store.metadata_evidence(session)?.len())
            })
            .unwrap()
        })
        .collect();
    assert!(
        held.iter()
            .all(|&count| count == held[0] && count >= written),
        "every Mac holds every record: {held:?} for {written} written"
    );
}

/// What a page of History costs on a registry of 100,000 records (tracker LT-25B): three Macs
/// write 2,500 operations apiece in each of 14 rounds under final checkpoints, and Mac 0 serves
/// the pages the Tables page's History drawer asks: its first page (the newest 100) with nothing
/// kept, then, keeping their order ([`Kept`]), the first page again, Read older three times, a
/// filter change (the kind, then a table) and a Find's bounded scan of 2,048, each at the
/// revision the first page was read at, and the first page after one more record. Each kept
/// page is the page read with nothing kept. It serves the Tables page's change check too
/// (tracker LT-25D): the revision read by a full open, by a reader that keeps a sighting (its
/// first ask, which opens the store, then asked again), asked of the sighting a page kept, and
/// asked after one more record. Prints each one's median of three wall times in this build's
/// profile; asserts nothing about time. Every time is taken on this test's own thread, in the
/// process that wrote the fixture and has opened the store before: none is a newly started
/// process's first open, and none is taken on the Tables page's reader thread, which runs at
/// background priority. `revision asked, nothing kept` is one sample, not a median.
/// Run it alone: `targo --unverified test -p aterm-db --lib -- --ignored --nocapture
/// att::tests::history_pages_at_100000_records_cost`.
#[test]
#[ignore = "manual measurement of History pages at 100,000 records; no latency budget"]
fn history_pages_at_100000_records_cost() {
    use super::history::{Ask, Filter, Page};
    use std::time::{Duration, Instant};
    let built = Instant::now();
    let (_lab, macs, written) = operations_under_checkpoints(14, 2_500);
    eprintln!(
        "fixture: {written} operations written in {:?}",
        built.elapsed()
    );
    let here = &macs[0];
    let median = |mut times: Vec<Duration>| {
        times.sort_unstable();
        times[times.len() / 2]
    };
    let timed = |what: &str, read: &mut dyn FnMut() -> Page| -> Page {
        let mut times = Vec::new();
        let mut served = None;
        for _ in 0..3 {
            let at = Instant::now();
            served = Some(read());
            times.push(at.elapsed());
        }
        let served = served.unwrap();
        eprintln!(
            "{what}: {:?} for {} entries (of 3: {times:?})",
            median(times.clone()),
            served.entries.len()
        );
        served
    };
    let revision = median(
        (0..3)
            .map(|_| {
                let at = Instant::now();
                here.revision().unwrap();
                at.elapsed()
            })
            .collect(),
    );
    eprintln!("revision read: {revision:?}");
    // The change check of a reader that reads no History: it keeps a sighting and no order.
    let mut marked = Kept::default();
    let at = Instant::now();
    let opened = here.revision_kept(&mut marked).unwrap();
    eprintln!("revision asked, nothing kept: {:?}", at.elapsed());
    assert_eq!(opened, here.revision().unwrap());
    let asked_again = |what: &str, kept: &mut Kept| {
        let times: Vec<Duration> = (0..3)
            .map(|_| {
                let at = Instant::now();
                assert_eq!(here.revision_kept(kept).unwrap(), opened);
                at.elapsed()
            })
            .collect();
        eprintln!("{what}: {:?} (of 3: {times:?})", median(times.clone()));
    };
    asked_again("revision asked again, sighting kept", &mut marked);
    assert_eq!((marked.revision(), marked.sighted()), (None, Some(opened)));
    let first_ask = Ask::newest(Filter::default(), 100);
    let first = timed("first page, nothing kept", &mut || {
        here.history_page(&first_ask).unwrap()
    });
    assert_eq!(first.entries.len(), 100);
    let mut kept = Kept::default();
    let at = Instant::now();
    let filled = here.history_page_kept(&first_ask, &mut kept).unwrap();
    eprintln!("first page, filling kept: {:?}", at.elapsed());
    assert_eq!(filled, first);
    asked_again("revision asked after a page, its sighting kept", &mut kept);
    let mut measure = |what: &str, ask: &Ask| -> Page {
        let served = timed(what, &mut || {
            here.history_page_kept(ask, &mut kept).unwrap()
        });
        assert_eq!(served, here.history_page(ask).unwrap(), "{what}");
        served
    };
    measure("first page again", &first_ask);
    let mut older = first_ask.after(&first).unwrap();
    for n in 1..=3 {
        let served = measure(&format!("read older {n}"), &older);
        assert!(!served.moved);
        older = older.after(&served).unwrap();
    }
    let at_first = |filter: Filter, limit: usize| Ask {
        revision: Some(first.revision),
        ..Ask::newest(filter, limit)
    };
    measure(
        "filter: kind",
        &at_first(
            Filter {
                kind: Some(EntryKind::Explicit),
                ..Filter::default()
            },
            100,
        ),
    );
    measure(
        "filter: table",
        &at_first(
            Filter {
                collection: Some(MACHINE.name.into()),
                ..Filter::default()
            },
            100,
        ),
    );
    measure("find scan", &at_first(Filter::default(), 2_048));
    let now = aterm_time::unix_millis().unwrap();
    assert_eq!(append_bulk_at(here, 1, now), (1, None));
    let at = Instant::now();
    let moved = here.history_page_kept(&first_ask, &mut kept).unwrap();
    eprintln!("first page after a write: {:?}", at.elapsed());
    assert_ne!(moved.revision, first.revision);
    assert_eq!(moved, here.history_page(&first_ask).unwrap());
    let at = Instant::now();
    let after = here.revision_kept(&mut marked).unwrap();
    eprintln!("revision asked after a write: {:?}", at.elapsed());
    assert_eq!(after, moved.revision);
    eprintln!("history holds {} entries", here.history().unwrap().len());
}

/// A read from a final checkpoint applies only the records above it and equals the full read, at
/// 20,000 operations (tracker LT-B10C): after three Macs wrote 21,000 operations under seven final
/// checkpoints, Mac 0's floor stands above 20,000 of them. A read there opens no payload below
/// the floor (it opens the cold file once in their place) and reads exactly what a read that
/// opens every record reads (the cold file moved aside): its cells, operations, owned rows,
/// quota and refusals.
/// NEGATIVE CONTROL: with `Store::cold_payloads` answering none, both reads open every record,
/// 21,065 payloads each: `left: 0, right: 21036`.
#[test]
fn a_read_from_a_final_checkpoint_applies_only_the_records_above_it_and_equals_the_full_read_at_20000_operations()
 {
    use crate::crypto::test_cipher_constructions;
    let (built, macs) = twenty_thousand_operations_under_checkpoints();
    let (lab, written) = (&built.0, built.1);
    assert!(written > 20_000, "{written}");
    let here = &macs[0];
    let below = application::observe_store(&lab.paths[0], here.descriptor(), |_, store, _| {
        Ok(store.floor().map(|body| body.below))
    })
    .unwrap()
    .expect("the seventh pass recorded its final checkpoint as the floor") as usize;
    assert!(below > 20_000, "the floor covers {below} of {written}");
    let read = || {
        let before = test_cipher_constructions();
        let view = here.view().unwrap();
        (view, test_cipher_constructions() - before)
    };
    // One read first: this process's first read of the table also sets up its keys, which
    // neither measured read should pay.
    read();
    let (floored, opened) = read();
    let cold = files_named(&lab._home.path().join("m1"), "cold");
    assert_eq!(cold.len(), 1, "{cold:?}");
    // Put back for the next test of these Macs, whether or not the read below panics.
    struct Aside([PathBuf; 2]);
    impl Drop for Aside {
        fn drop(&mut self) {
            let [cold, aside] = &self.0;
            let back = std::fs::rename(aside, cold);
            if !std::thread::panicking() {
                back.unwrap();
            }
        }
    }
    let aside = Aside([cold[0].clone(), cold[0].with_file_name("cold.aside")]);
    std::fs::rename(&aside.0[0], &aside.0[1]).unwrap();
    let (full, opened_all) = read();
    drop(aside);
    assert_eq!(
        opened_all - opened,
        below - 1,
        "the read opened {opened} payloads, {opened_all} without the cold file: none of the \
         {below} below the floor, and the cold file once"
    );
    assert_eq!(floored.cells, full.cells);
    assert_eq!(
        floored.ops.iter().map(|op| op.id).collect::<Vec<_>>(),
        full.ops.iter().map(|op| op.id).collect::<Vec<_>>()
    );
    assert_eq!(floored.owned, full.owned);
    assert_eq!(floored.quota, full.quota);
    assert_eq!(floored.refused, full.refused);
    assert_eq!(floored.complete, full.complete);
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
                said: None,
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
            said: None,
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
            said: None,
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
            said: None,
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
            said: None,
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
            said: None,
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

/// A write lists what has written its cells since (tracker LT-25: History shows a selected
/// record so): the writes to each cell that observed it, in history order, standing or not, and
/// never one it had not seen. Mac 0 writes a row (`born`); Mac 1 sets its status (`parked`) while
/// Mac 0, concurrently, sets another and takes a set member out (`lost`); then Mac 0, having seen
/// both, sets the status again (`later`). Since `born` the status was written by all three; since
/// either concurrent write only by `later`, the other being unseen; the member `lost` took out by
/// nothing. [`Handle::wrote_named`] answers the same with each writer's Mac, and a peer the same.
/// NEGATIVE CONTROL: with `Reader::beside` asking the observation the wrong way round
/// (`observes(operation, &other.id)` in place of `observes(&other.id, operation)`), what the
/// born write saw is called written since and what saw it is not: `` left: [] right: ["lost",
/// "parked", "later"] ``.
#[test]
fn a_write_lists_what_has_written_its_cells_since() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    Registry::at(lab.paths[0].clone()).open().unwrap();
    let here = Handle::at(lab.paths[0].clone(), &REGISTRY);
    let born = append_history_at(
        &here,
        Command::Write {
            request: [85; 16],
            changes: machine(MC),
            said: None,
        },
        2_000,
    );
    repair_new_registry(&lab);
    let there = Handle::at(lab.paths[1].clone(), &REGISTRY);
    let parked = append_history_at(
        &there,
        Command::Write {
            request: [86; 16],
            changes: vec![put(&MACHINE, MC, "status", json!("parked"))],
            said: None,
        },
        3_000,
    );
    let lost = append_history_at(
        &here,
        Command::Write {
            request: [87; 16],
            changes: vec![
                put(&MACHINE, MC, "status", json!("lost")),
                take_out(MC, "clis", "codex"),
            ],
            said: None,
        },
        1_000,
    );
    repair_registry(&lab, 2);
    let later = append_history_at(
        &here,
        Command::Write {
            request: [88; 16],
            changes: vec![put(&MACHINE, MC, "status", json!("active"))],
            said: None,
        },
        4_000,
    );
    repair_registry(&lab, 2);
    let names = [
        ("born", born),
        ("parked", parked),
        ("lost", lost),
        ("later", later),
    ];
    let status = format!("{MC}.status");
    let codex = format!("{MC}.clis.codex");
    let since = |handle: &Handle, operation: Digest, key: &str| -> Vec<&'static str> {
        let wrote = handle.wrote(operation).unwrap();
        let cell = wrote
            .iter()
            .find(|cell| cell.collection == MACHINE.name && cell.key == key)
            .unwrap_or_else(|| panic!("the operation wrote {key}"));
        named(&names, &cell.since)
    };
    let mut concurrent: Vec<&str> = here
        .history()
        .unwrap()
        .iter()
        .filter_map(|entry| match entry.operation {
            id if id == parked => Some("parked"),
            id if id == lost => Some("lost"),
            _ => None,
        })
        .collect();
    concurrent.push("later");
    assert_eq!(since(&here, born, &status), concurrent, "in history order");
    assert_eq!(since(&here, lost, &status), ["later"]);
    assert_eq!(since(&here, parked, &status), ["later"]);
    assert!(since(&here, later, &status).is_empty());
    assert!(
        since(&here, lost, &codex).is_empty(),
        "nothing wrote the member since"
    );
    assert_eq!(
        since(&there, born, &status),
        concurrent,
        "a peer answers the same"
    );
    // Named: the same cells, and the join that names each writer's Mac.
    let (wrote, macs) = here.wrote_named(parked).unwrap();
    assert_eq!(wrote, here.wrote(parked).unwrap());
    let writer = &wrote[0].wrote[0];
    assert_eq!(macs, Macs::of(&here.view().unwrap()));
    assert_eq!(
        macs.name(&writer.actor, writer.machine.as_ref()),
        Mac::Machine(writer.machine.unwrap()),
        "no facts row binds a scratch Mac's machine"
    );
}

/// A row's writes to the cells asked for are read at once ([`Handle::writes`], tracker FL-258):
/// each operation once, in the order history gives, its writer named and kinded as history
/// names it, and at each cell what [`Handle::wrote`] says it wrote, replaced and had not seen,
/// in the order the cells were asked for. Mac 0 writes a row (`born`, two of the cells asked
/// for); Mac 1 then sets its status (`parked`) while Mac 0, concurrently, sets another status
/// and takes a set member out (`lost`). A peer holding the same operations answers the same; a
/// cell nothing wrote lists nothing, and a collection of rows is no typed cell's.
/// NEGATIVE CONTROL: with `Reader::writes` listing a write once per cell (one `Written` for each
/// cell it wrote), `born` and `lost`, which each wrote two of the cells, are each listed twice,
/// five operations for three: `` left: [a, a, b, b, c] right: [a, b, c] `` (as digests).
#[test]
fn a_rows_writes_are_read_once_in_history_order_with_their_macs() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    Registry::at(lab.paths[0].clone()).open().unwrap();
    let here = Handle::at(lab.paths[0].clone(), &REGISTRY);
    let born = append_history_at(
        &here,
        Command::Write {
            request: [91; 16],
            changes: machine(MC),
            said: None,
        },
        2_000,
    );
    repair_new_registry(&lab);
    let there = Handle::at(lab.paths[1].clone(), &REGISTRY);
    let parked = append_history_at(
        &there,
        Command::Write {
            request: [92; 16],
            changes: vec![put(&MACHINE, MC, "status", json!("parked"))],
            said: None,
        },
        3_000,
    );
    let lost = append_history_at(
        &here,
        Command::Write {
            request: [93; 16],
            changes: vec![
                put(&MACHINE, MC, "status", json!("lost")),
                take_out(MC, "clis", "codex"),
            ],
            said: None,
        },
        1_000,
    );
    repair_registry(&lab, 2);
    let status = format!("{MC}.status");
    let codex = format!("{MC}.clis.codex");
    let asked = [status.as_str(), codex.as_str()];
    let read_by = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let writes = here.writes(MACHINE.name, &asked, read_by).unwrap();

    let history = here.history().unwrap();
    let order: Vec<Digest> = history
        .iter()
        .map(|entry| entry.operation)
        .filter(|id| [born, parked, lost].contains(id))
        .collect();
    assert_eq!(
        writes
            .iter()
            .map(|written| written.operation)
            .collect::<Vec<_>>(),
        order
    );
    for written in &writes {
        let entry = history
            .iter()
            .find(|entry| entry.operation == written.operation)
            .unwrap();
        assert_eq!(
            (
                written.source,
                written.actor,
                written.machine,
                &written.mac,
                written.authored_ms,
                written.hlc,
                written.reverts,
                written.kind
            ),
            (
                Source::Log,
                entry.actor,
                entry.machine,
                &entry.mac,
                entry.authored_ms,
                entry.hlc,
                entry.reverts,
                entry.kind
            )
        );
        let mut cells: Vec<Wrote> = here
            .wrote(written.operation)
            .unwrap()
            .into_iter()
            .filter(|cell| asked.contains(&cell.key.as_str()))
            .collect();
        cells.sort_by_key(|cell| cell.key != status);
        assert_eq!(written.cells, cells);
    }
    let cells = |id: Digest| {
        writes
            .iter()
            .find(|written| written.operation == id)
            .map_or(0, |written| written.cells.len())
    };
    assert_eq!((cells(born), cells(parked), cells(lost)), (2, 1, 2));
    assert_eq!(there.writes(MACHINE.name, &asked, read_by).unwrap(), writes);
    let nickname = format!("{MC}.nickname");
    assert!(
        here.writes(MACHINE.name, &[nickname.as_str()], read_by)
            .unwrap()
            .is_empty()
    );
    let facts = keys::uuid32(&lab.machine(0));
    assert!(
        here.writes(MACHINE_FACTS.name, &[facts.as_str()], read_by)
            .is_err()
    );
}

/// [`Handle::writes`] answers by its bound: a spent one refuses at once, and so does a read of a
/// store another program holds past it; given time, it reads.
/// NEGATIVE CONTROL: with `Handle::writes` observing through `reading` (the admission deadline,
/// with no `read_by`), the spent read reads: `` a spent bound read: Ok([Written { .. }]) ``.
#[test]
fn a_rows_writes_answer_by_their_bound() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    Registry::at(lab.paths[0].clone()).open().unwrap();
    let here = Handle::at(lab.paths[0].clone(), &REGISTRY);
    append_history_at(
        &here,
        Command::Write {
            request: [94; 16],
            changes: machine(MC),
            said: None,
        },
        2_000,
    );
    let status = format!("{MC}.status");
    let spent = here.writes(MACHINE.name, &[status.as_str()], std::time::Instant::now());
    assert!(spent.is_err(), "a spent bound read: {spent:?}");
    let held = aterm_secrets::client::test_hold_state_lock(&lab.paths[0]).unwrap();
    let busy = here.writes(
        MACHINE.name,
        &[status.as_str()],
        std::time::Instant::now() + std::time::Duration::from_millis(300),
    );
    assert!(busy.is_err(), "a read of a held store: {busy:?}");
    drop(held);
    let read_by = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    assert_eq!(
        here.writes(MACHINE.name, &[status.as_str()], read_by)
            .unwrap()
            .len(),
        1
    );
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
                said: None,
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
                said: None,
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
                said: None,
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
            said: None,
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
                said: None,
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
                said: None,
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
            said: None,
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

/// [`Handle::writes`] through a registry roll: each write of the earlier generation is listed
/// once, as carried, with the clocks its log gave it, its kind and value and no replaced or
/// unseen (the writes it saw are of another log); the first write after the roll replaced the
/// carried value that stood. Each is what [`Handle::cell_values`] lists for the cell, and the new
/// write's cell is what [`Handle::wrote`] says. A carried candidate no history row names is
/// listed under its own operation, at the record that restated it.
/// NEGATIVE CONTROL: with `Reader::writes` leaving out a roll's restatement (its `!logged` arm
/// skipped), the candidate no history row names is not listed: `` left: [] right: [([7, 7, …],
/// Restated { by: … })] ``; the carried writes a history row names are listed still.
#[test]
fn a_rolled_rows_writes_are_carried() {
    use crate::table::tests::repair_commons;
    let pk = "mc-0000000000000001";
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
                said: None,
            },
            u64::from(request) * 1_000,
        )
    };
    let born = write(&first, 1, machine(pk));
    let moved = write(
        &first,
        2,
        vec![put(&MACHINE, pk, "location", json!("rack b"))],
    );
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
    let fresh = write(
        &rolled,
        5,
        vec![put(&MACHINE, pk, "location", json!("rack c"))],
    );

    let location = format!("{pk}.location");
    let read_by = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let writes = rolled
        .writes(MACHINE.name, &[location.as_str()], read_by)
        .unwrap();
    let names = [("born", born), ("moved", moved), ("fresh", fresh)];
    let name = |id: Digest| {
        names
            .iter()
            .find(|(_, named)| *named == id)
            .map_or("?", |(name, _)| *name)
    };
    assert_eq!(
        writes
            .iter()
            .map(|written| name(written.operation))
            .collect::<Vec<_>>(),
        ["born", "moved", "fresh"]
    );
    let carried = Source::Carried {
        generation: 1,
        trimmed: false,
    };
    for written in &writes[..2] {
        assert_eq!(
            (written.source, written.kind, written.cells.len()),
            (carried, EntryKind::Explicit, 1)
        );
        let cell = &written.cells[0];
        assert!(
            cell.replaced.is_empty() && cell.unseen.is_empty(),
            "{cell:?}"
        );
    }
    let values = rolled.cell_values(MACHINE.name, &location).unwrap();
    assert_eq!(
        writes
            .iter()
            .flat_map(|written| written.cells[0].wrote.clone())
            .collect::<Vec<_>>(),
        values
    );
    let new = &writes[2];
    assert_eq!((new.source, new.kind), (Source::Log, EntryKind::Explicit));
    assert_eq!(
        named(&names, &new.cells[0].replaced),
        ["moved"],
        "the first write after the roll replaced the value that stood"
    );
    let wrote = rolled.wrote(fresh).unwrap();
    assert_eq!(new.cells, wrote);

    let nickname = format!("{pk}.nickname");
    let spare = put(&MACHINE, pk, "nickname", json!("spare")).value.unwrap();
    let restating = write(
        &rolled,
        6,
        vec![Change {
            collection: MACHINE.name.to_owned(),
            key: nickname.clone(),
            value: Some(json!({"k": [{"o": hex::encode_lower(&[7; 32]), "v": spare}]})),
        }],
    );
    let restated = rolled
        .writes(MACHINE.name, &[nickname.as_str()], read_by)
        .unwrap();
    assert_eq!(
        restated
            .iter()
            .map(|written| (written.operation, written.source, written.kind))
            .collect::<Vec<_>>(),
        [(
            [7; 32],
            Source::Restated { by: restating },
            EntryKind::Explicit
        )]
    );
    assert_eq!(
        restated[0].cells[0].wrote,
        rolled.cell_values(MACHINE.name, &nickname).unwrap()
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
                said: None,
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
                said: None,
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
                said: None,
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

/// A ROLL OF A FAMILY WITH NO SECRET COLUMN KEEPS EVERY SIGNED RECORD (tracker TK-31; decisions
/// TK-D2, TK-D17 and TK-D31, docs/DESIGN-lab-tracker-2026-11-14.md §8). The family is
/// `kept_tests::KEPT`: two generations, pages of prose, no secret column, version 2 records.
/// Three Macs: the roller, an author (another Writer), and a reader that is not in the lab's
/// exchanges until the roll is done.
///
/// The author signs 84 records of about 40 KiB each, each saying who made it, why and on what
/// evidence: more than the 3 MiB a restated history holds (`alab::history::MAX_HISTORY_BYTES`),
/// past which a registry roll leaves a horizon. The roller rolls the family. Then:
///
/// * the roll waited for the author to declare the second generation, sealed the first, and
///   every reader moved to the second, whose registers read as the first's did;
/// * the first generation is kept (`catalog::State::Kept`): both Macs still store it, and a
///   release pass a year later releases nothing;
/// * the second generation holds no restated history and no horizon;
/// * history through the roll lists every record the author signed, as a record of generation
///   1, signed by the author's Mac, with its principal, reason and evidence; and the roll's own
///   records as generation 2's, signed by the roller, saying who rolled;
/// * as of a record of the sealed generation, the table reads as that generation held it then;
/// * a diff from a record of the sealed generation to the head names the author's later record
///   in the sealed log, and its statement, as what changed the cell, shows no cell the roll only
///   restated, and after a write in the second generation names that write;
/// * a record the author signs into the first generation after the seal and before it hears of
///   it reaches the roller by repair, is listed, and a roll run again carries it;
/// * the reader, which joins after all that, fetches the sealed generation by repair and reads
///   the same history, every record under its own signer.
///
/// NEGATIVE CONTROL, observed 2026-11-17: with `Handle::earlier` answering no generation (a
/// history that reads the current generation alone, as every family read before this), the test
/// fails with `Mac 0: history lists every record the author signed, across the roll`.
#[test]
fn a_roll_of_a_family_with_no_secret_column_keeps_every_record_under_its_own_signature_and_writes_no_horizon()
 {
    use super::kept_tests::{KEPT, KEPT_2, PAGE, declare, page, said, state, storage, title};
    use crate::{
        catalog::State,
        table::fixture::{repair_commons, repair_commons_among},
    };
    const ROLLER: usize = 0;
    const AUTHOR: usize = 1;
    const READER: usize = 2;
    const PAGES: usize = 84;
    const PAGE_BYTES: usize = 40_000;
    register_for_test(&KEPT);
    assert!(keeps_records(&KEPT));
    assert!(!keeps_records(&REGISTRY), "the registry declares secrets");

    let lab = Lab::new(&[Role::Writer, Role::Writer, Role::Reader]);
    let now_ms = aterm_time::unix_millis().unwrap();
    let clocks = [now_ms / 1000; 3];
    // The two Writers exchange; the reader joins at the end.
    let writers = |rounds| repair_commons_among(&lab, rounds, &[ROLLER, AUTHOR], &clocks);
    let at = |n: usize| Handle::at(lab.paths[n].clone(), &KEPT);
    let author_said = said("s-author", "TK-31: pages written before the roll");
    let roller_said = said("s-roller", "TK-31: roll lab/kept to its second generation");
    let author = |rows: Vec<Vec<Change>>| {
        at(AUTHOR)
            .saying(author_said.clone())
            .commit_owner(|_| Ok(((), rows)))
            .unwrap()
            .1
            .operations
    };
    let title_of = |view: &View, pk: &str| {
        view.project(&PAGE)
            .rows
            .get(pk)
            .and_then(|row| row.get("title").cloned())
    };

    // The author signs more than 3 MiB of records into the first generation, then two titles,
    // the second over the first.
    declare(&lab.paths[AUTHOR], &KEPT);
    let pages: Vec<_> = (0..PAGES)
        .map(|n| page(&format!("p{n:02}"), PAGE_BYTES))
        .collect();
    // A few commits, each inside one write's admission deadline in a debug build.
    let mut signed = Vec::new();
    for chunk in pages.chunks(14) {
        signed.extend(author(chunk.to_vec()));
    }
    assert_eq!(signed.len(), PAGES, "one record a page");
    let first_title = author(vec![title("p00", "first")])[0];
    let second_title = author(vec![title("p00", "second")])[0];
    signed.extend([first_title, second_title]);
    writers(6);
    let before = at(ROLLER).view().unwrap();
    assert_eq!(before.project(&PAGE).rows.len(), PAGES);

    // A plan writes nothing; the first apply restates and waits for the author's declaration of
    // the second generation; the author declares it; the next apply seals.
    let roller = || at(ROLLER).saying(roller_said.clone());
    let plan = roller().roll(false, now_ms).unwrap();
    assert_eq!((plan.from, plan.to, plan.records), (1, 2, 0));
    // Each page's lifecycle and body, and the one title.
    assert_eq!(plan.registers, 2 * PAGES + 1, "{plan:?}");
    assert_eq!(plan.waiting, vec![lab.machine(AUTHOR)]);
    assert_eq!(
        state(&lab.paths[ROLLER], &KEPT_2),
        None,
        "a plan declares nothing"
    );
    let staged = roller().roll(true, now_ms).unwrap();
    assert!(!staged.sealed && staged.records > PAGES, "{staged:?}");
    assert_eq!(staged.registers, plan.registers);
    assert_eq!(staged.waiting, vec![lab.machine(AUTHOR)]);
    assert_eq!(at(ROLLER).compiled.generation, 1, "unsealed, readers stay");
    writers(6);
    assert!(at(AUTHOR).endorse().unwrap());
    writers(6);
    let rolled = roller().roll(true, now_ms).unwrap();
    assert!(rolled.sealed && rolled.waiting.is_empty(), "{rolled:?}");
    assert_eq!(rolled.records, 0, "nothing is restated twice: {rolled:?}");
    // The author has not heard the seal, and signs one more record into the first generation.
    assert_eq!(at(AUTHOR).compiled.generation, 1);
    signed.extend(author(vec![page("late", 64)]));
    writers(6);

    // Readers moved, and the second generation reads as the first did.
    for n in [ROLLER, AUTHOR] {
        let handle = at(n);
        assert_eq!(handle.compiled.generation, 2, "Mac {n} reads the second");
        let view = handle.view().unwrap();
        assert!(view.complete, "Mac {n}: the restatement is whole");
        assert_eq!(view.project(&PAGE), before.project(&PAGE), "Mac {n}");
        // Kept: still stored, and no release pass takes it, however late.
        assert_eq!(state(&lab.paths[n], &KEPT), Some(State::Kept), "Mac {n}");
        let released = Registry::at(lab.paths[n].clone())
            .release(now_ms + 365 * 86_400_000)
            .unwrap();
        assert!(
            released.released.is_empty() && released.kept.is_empty(),
            "Mac {n}: no release pass names a kept generation: {released:?}"
        );
        assert!(
            storage(&lab, n, &KEPT).exists(),
            "Mac {n} stores the sealed log"
        );
        // No restated history and no horizon in the second generation.
        let carried = handle.carried_history().unwrap();
        assert!(carried.entries.is_empty() && carried.horizons.is_empty());
        assert!(
            view.cells
                .keys()
                .all(|(collection, _)| collection == PAGE.name
                    || collection == crate::alab::registry::ROLL.name),
            "Mac {n}: the roll wrote pages and its cover, nothing else"
        );
    }

    // History across the roll: every record the author signed, under the author's signature,
    // with what it says; more than a restated history's bound of them.
    let author_machine = Some(lab.machine(AUTHOR));
    let check_history = |n: usize, signed: &[Digest]| {
        let history = at(n).history().unwrap();
        let mut bytes = 0;
        for id in signed {
            let entry = history
                .iter()
                .find(|entry| entry.operation == *id)
                .unwrap_or_else(|| {
                    panic!("Mac {n}: history lists every record the author signed, across the roll")
                });
            assert_eq!(entry.generation, 1, "Mac {n}: a record of the sealed log");
            assert_eq!(entry.machine, author_machine, "Mac {n}: its own signer");
            assert_eq!(entry.said.as_ref(), Some(&author_said), "Mac {n}");
            assert!(entry.inert.is_none(), "Mac {n}: {:?}", entry.inert);
            bytes += aterm_json::to_vec(&entry.changes).unwrap().len();
        }
        assert!(
            bytes > crate::alab::history::MAX_HISTORY_BYTES,
            "Mac {n}: {bytes} bytes of the author's records, past a restated history's bound"
        );
        let cursors: Vec<_> = history.iter().map(history::Entry::cursor).collect();
        assert!(cursors.is_sorted(), "Mac {n}: one order across generations");
        history
    };
    let history = check_history(ROLLER, &signed);
    let restated: Vec<_> = history.iter().filter(|e| e.generation == 2).collect();
    assert_eq!(restated.len(), staged.records);
    assert!(restated.iter().all(|entry| {
        entry.machine == Some(lab.machine(ROLLER)) && entry.said.as_ref() == Some(&roller_said)
    }));
    assert_eq!(history.len(), signed.len() + staged.records);

    // As of a record of the sealed generation.
    let then = at(ROLLER).view_at(Point::At(first_title)).unwrap();
    assert_eq!(title_of(&then, "p00"), Some(json!("first")));
    assert_eq!(then.project(&PAGE).rows.len(), PAGES);

    // A diff across the roll: the one cell that changed, by the author's record in the sealed
    // log; no cell the roll only restated.
    let changed = |diff: &Diff| -> Vec<(String, Vec<Maker>)> {
        diff.cells
            .iter()
            .filter(|cell| cell.collection == PAGE.name)
            .map(|cell| (cell.key.clone(), cell.by.clone()))
            .collect()
    };
    let by_author = vec![(
        "p00.title".to_owned(),
        vec![Maker {
            operation: second_title,
            actor: history
                .iter()
                .find(|entry| entry.operation == second_title)
                .unwrap()
                .actor,
            machine: author_machine,
            said: Some(author_said.clone()),
        }],
    )];
    let across = at(ROLLER).diff(first_title, Point::Head).unwrap();
    assert_eq!(changed(&across), by_author);
    let within = at(ROLLER)
        .diff(first_title, Point::At(second_title))
        .unwrap();
    assert_eq!(changed(&within), by_author);
    // A write in the second generation is then what changed it.
    let third_title = roller()
        .commit_owner(|_| Ok(((), vec![title("p00", "third")])))
        .unwrap()
        .1
        .operations[0];
    let across = at(ROLLER).diff(first_title, Point::Head).unwrap();
    let makers = changed(&across);
    assert_eq!(makers.len(), 1, "{makers:?}");
    assert_eq!(makers[0].0, "p00.title");
    assert_eq!(makers[0].1.len(), 1);
    assert_eq!(makers[0].1[0].operation, third_title);
    assert_eq!(makers[0].1[0].machine, Some(lab.machine(ROLLER)));
    assert_eq!(makers[0].1[0].said.as_ref(), Some(&roller_said));

    // The record the author signed after the seal reached the roller by repair, as any record
    // of a kept generation does, and is listed above; a roll run again carries it, and nothing
    // it carried before.
    let late = |n: usize| {
        at(n)
            .view()
            .unwrap()
            .project(&PAGE)
            .rows
            .contains_key("late")
    };
    assert!(!late(ROLLER));
    let again = roller().roll(true, now_ms).unwrap();
    assert_eq!(
        (again.from, again.to, again.registers),
        (1, 2, 2),
        "{again:?}"
    );
    assert!(!again.sealed, "sealed already: {again:?}");
    assert!(late(ROLLER));
    assert_eq!(
        title_of(&at(ROLLER).view().unwrap(), "p00"),
        Some(json!("third"))
    );

    // The reader joins: it fetches the sealed generation and reads the same history, each
    // record under its own signer's signature.
    assert!(!storage(&lab, READER, &KEPT).exists());
    repair_commons(&lab, 6);
    assert_eq!(state(&lab.paths[READER], &KEPT), Some(State::Kept));
    assert!(storage(&lab, READER, &KEPT).exists(), "fetched by repair");
    let read = check_history(READER, &signed);
    let whole = at(ROLLER).history().unwrap();
    assert_eq!(read, whole, "the reader's history is the roller's");
    assert_eq!(
        title_of(&at(READER).view_at(Point::At(first_title)).unwrap(), "p00"),
        Some(json!("first"))
    );
}

/// A KEPT ROLL OF A GENERATION WITH OWNED ROWS AND LOG ENTRIES READS EACH FROM THE SEALED LOG,
/// UNDER ITS OWN SIGNER (tracker TK-40). The family is `kept_tests::KEPT`: two generations, each
/// with pages (cells), a seat a Mac (an owned row at its own machine uuid) and remarks (a log,
/// `<uuid32>.<n>`). Two Macs, both Writers: the roller and an author.
///
/// Into the first generation the roller signs its seat and a remark, and the author, having
/// heard those, a page, a remark and its seat twice, the second over the first. The roller
/// rolls the family: the plan restates the page's two registers and nothing else, and the roll
/// is not refused. Then, on both Macs:
///
/// * the second generation's own log holds no change to a seat or a remark (none was restated
///   or archived), and its view holds each seat and each remark exactly as the first
///   generation's view read it before the roll: the same row at the same key, by the same
///   operation, whose signer is the Mac the key names;
/// * history lists each of those records as a record of generation 1, signed by its own Mac,
///   with its principal, reason and evidence;
/// * as of the author's first seat, the seat reads as it did then, beside the rows that record
///   had observed;
/// * a diff from that record to the head, across the roll, shows the author's seat changed by
///   the author's second record in the sealed log, and no other seat or remark.
///
/// The author then signs into the second generation its seat, with a counter below its sealed
/// row's, and a remark. On both Macs: the author's seat is the second generation's (a later
/// generation's row over an earlier one's, whatever their counters) while the roller's is still
/// read from the sealed log; the new remark's counter goes on past the sealed one's, so all
/// three remarks stand at three keys; as of the new seat, a point of the second generation, the
/// sealed rows are held; and a diff to the head from a sealed record, and one from a record of
/// the second generation signed before the new seat, each show the new seat as a change of the
/// sealed one and the new remark as added, and nothing else. An append through a handle asked
/// for the second generation by name goes on past the sealed counter too.
///
/// NEGATIVE CONTROL, observed 2026-11-18: with `kept::overlay` laying nothing under a view's own
/// reads (a view that reads its own log alone, as every view read before this), the test fails
/// with `Mac 0: the second generation's view holds each seat its signer wrote into the first`.
#[test]
fn a_kept_roll_of_a_generation_with_owned_rows_and_log_entries_reads_each_from_the_sealed_log_under_its_own_signer()
 {
    use super::kept_tests::{KEPT, KEPT_2, REMARK, SEAT, declare, page, said, state};
    use crate::{catalog::State, table::fixture::repair_commons_among, typed::owned};
    const ROLLER: usize = 0;
    const AUTHOR: usize = 1;
    register_for_test(&KEPT);
    assert!(keeps_records(&KEPT));

    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let now_ms = aterm_time::unix_millis().unwrap();
    let clocks = [now_ms / 1000; 2];
    let exchange = |rounds| repair_commons_among(&lab, rounds, &[ROLLER, AUTHOR], &clocks);
    let at = |n: usize| Handle::at(lab.paths[n].clone(), &KEPT);
    let saids = [
        said("s-roller", "TK-40: the roller's own rows, and the roll"),
        said("s-author", "TK-40: the author's own rows"),
    ];
    let seat_key = |n: usize| keys::uuid32(&lab.machine(n));
    // Mac `n` states its seat with `counter`: one record, named by its operation.
    let seat = |n: usize, counter: u64, label: &str| {
        let row = Change {
            collection: SEAT.name.to_owned(),
            key: seat_key(n),
            value: Some(json!({"r": 1, "n": counter, "label": label})),
        };
        at(n)
            .saying(saids[n].clone())
            .commit_owner(|_| Ok(((), vec![vec![row]])))
            .unwrap()
            .1
            .operations[0]
    };
    // Mac `n` appends a remark: its key.
    let remark = |n: usize, text: &str| {
        let row = json!({"text": text}).as_object().unwrap().clone();
        at(n)
            .saying(saids[n].clone())
            .append_log(&REMARK, &[row])
            .unwrap()
            .remove(0)
    };
    let entry_key = |entry: &owned::LogEntry| owned::log_entry_key(&entry.machine, entry.n);
    let remark_keys = |view: &View| -> BTreeSet<String> {
        view.logs[REMARK.name]
            .entries
            .iter()
            .map(entry_key)
            .collect()
    };
    let remark_op = |view: &View, key: &str| {
        view.logs[REMARK.name]
            .entries
            .iter()
            .find(|entry| entry_key(entry) == key)
            .unwrap_or_else(|| panic!("the view holds the remark {key}"))
            .operation
    };
    // Each seat and remark that differs, with the operations that made the difference.
    let own_rows = |diff: &Diff| -> Vec<(String, String, Delta, Vec<Digest>)> {
        diff.cells
            .iter()
            .filter(|cell| cell.collection == SEAT.name || cell.collection == REMARK.name)
            .map(|cell| {
                (
                    cell.collection.clone(),
                    cell.key.clone(),
                    cell.delta,
                    cell.by.iter().map(|maker| maker.operation).collect(),
                )
            })
            .collect()
    };

    // The first generation: the roller's seat and remark, which the author hears before it
    // signs a page, a remark and its seat twice.
    declare(&lab.paths[ROLLER], &KEPT);
    declare(&lab.paths[AUTHOR], &KEPT);
    exchange(6);
    let roller_seat = seat(ROLLER, 1, "roller");
    let roller_remark = remark(ROLLER, "the roller's");
    exchange(6);
    at(AUTHOR)
        .saying(saids[AUTHOR].clone())
        .commit_owner(|_| Ok(((), vec![page("p0", 64)])))
        .unwrap();
    let author_remark = remark(AUTHOR, "the author's");
    let first = seat(AUTHOR, 1, "first");
    let second = seat(AUTHOR, 2, "second");
    exchange(6);
    let before = at(ROLLER).view().unwrap();
    assert_eq!(before.owned[SEAT.name].rows.len(), 2, "a seat a Mac");
    assert_eq!(
        before.owned[SEAT.name].rows[&seat_key(AUTHOR)].operation,
        second
    );
    assert_eq!(
        remark_keys(&before),
        BTreeSet::from([roller_remark.clone(), author_remark.clone()])
    );
    // Every record a Mac signed of its own rows, and which Mac.
    let signed = [
        (roller_seat, ROLLER),
        (remark_op(&before, &roller_remark), ROLLER),
        (remark_op(&before, &author_remark), AUTHOR),
        (first, AUTHOR),
        (second, AUTHOR),
    ];

    // The roll is not refused, restates the page's lifecycle and body, and carries no seat and
    // no remark.
    let roller = || at(ROLLER).saying(saids[ROLLER].clone());
    let plan = roller().roll(false, now_ms).unwrap();
    assert_eq!((plan.from, plan.to, plan.registers), (1, 2, 2), "{plan:?}");
    let staged = roller().roll(true, now_ms).unwrap();
    assert!(!staged.sealed, "{staged:?}");
    exchange(6);
    assert!(at(AUTHOR).endorse().unwrap());
    exchange(6);
    let rolled = roller().roll(true, now_ms).unwrap();
    assert!(rolled.sealed && rolled.waiting.is_empty(), "{rolled:?}");
    exchange(6);

    for n in [ROLLER, AUTHOR] {
        let handle = at(n);
        assert_eq!(handle.compiled.generation, 2, "Mac {n} reads the second");
        assert_eq!(state(&lab.paths[n], &KEPT), Some(State::Kept), "Mac {n}");
        let view = handle.view().unwrap();
        assert!(view.complete, "Mac {n}: the restatement is whole");
        // No row was restated or archived: the second generation's own log holds none.
        assert!(
            view.ops
                .iter()
                .flat_map(|op| &op.changes)
                .all(|change| change.collection != SEAT.name && change.collection != REMARK.name),
            "Mac {n}: the roll wrote no seat and no remark"
        );
        assert!(handle.carried_history().unwrap().entries.is_empty());
        // Each is read from the sealed log: the same row, at the same key, by the same record.
        assert_eq!(
            view.owned[SEAT.name], before.owned[SEAT.name],
            "Mac {n}: the second generation's view holds each seat its signer wrote into the first"
        );
        assert_eq!(
            view.logs[REMARK.name], before.logs[REMARK.name],
            "Mac {n}: the second generation's view holds each remark its signer appended to the \
             first"
        );
        // Each under its own signer, at its own key.
        for who in [ROLLER, AUTHOR] {
            let row = &view.owned[SEAT.name].rows[&seat_key(who)];
            assert_eq!(row.machine, lab.machine(who), "Mac {n}: {who}'s seat");
        }
        let seats = &view.owned[SEAT.name].rows;
        assert_eq!(seats[&seat_key(ROLLER)].operation, roller_seat, "Mac {n}");
        assert_eq!(seats[&seat_key(AUTHOR)].operation, second, "Mac {n}");
        assert_eq!(seats[&seat_key(AUTHOR)].value["label"], json!("second"));
        for (key, who) in [(&roller_remark, ROLLER), (&author_remark, AUTHOR)] {
            assert_eq!(
                owned::log_key(key).map(|(machine, _)| machine),
                Some(lab.machine(who)),
                "Mac {n}: a remark at its signer's key"
            );
        }

        // History: each record under its own signer's signature, as a record of generation 1.
        let history = handle.history().unwrap();
        for (id, who) in signed {
            let entry = history
                .iter()
                .find(|entry| entry.operation == id)
                .unwrap_or_else(|| {
                    panic!("Mac {n}: history lists every record of a Mac's own rows")
                });
            assert_eq!(entry.generation, 1, "Mac {n}: a record of the sealed log");
            assert_eq!(entry.machine, Some(lab.machine(who)), "Mac {n}");
            assert_eq!(entry.said.as_ref(), Some(&saids[who]), "Mac {n}");
            assert!(entry.inert.is_none(), "Mac {n}: {:?}", entry.inert);
        }

        // As of the author's first seat, a point of the sealed generation.
        let then = handle.view_at(Point::At(first)).unwrap();
        let seats = &then.owned[SEAT.name].rows;
        assert_eq!(seats[&seat_key(AUTHOR)].operation, first, "Mac {n}");
        assert_eq!(seats[&seat_key(AUTHOR)].value["label"], json!("first"));
        assert_eq!(seats[&seat_key(ROLLER)].operation, roller_seat, "Mac {n}");
        assert_eq!(remark_keys(&then), remark_keys(&before), "Mac {n}");

        // A diff across the roll: the author's seat, changed by the author's second record in
        // the sealed log; no seat or remark that only stands where it stood.
        let across = handle.diff(first, Point::Head).unwrap();
        assert_eq!(
            own_rows(&across),
            vec![(
                SEAT.name.to_owned(),
                seat_key(AUTHOR),
                Delta::Changed,
                vec![second]
            )],
            "Mac {n}"
        );
        let maker = &across
            .cells
            .iter()
            .find(|cell| cell.collection == SEAT.name)
            .unwrap()
            .by[0];
        assert_eq!(maker.machine, Some(lab.machine(AUTHOR)), "Mac {n}");
        assert_eq!(maker.said.as_ref(), Some(&saids[AUTHOR]), "Mac {n}");
    }

    // The second generation. The author states its seat again, with a counter below its sealed
    // row's, and appends a remark.
    let earlier = at(AUTHOR)
        .saying(saids[AUTHOR].clone())
        .commit_owner(|_| Ok(((), vec![page("p1", 64)])))
        .unwrap()
        .1
        .operations[0];
    let third = seat(AUTHOR, 1, "third");
    let later_remark = remark(AUTHOR, "the author's, after the roll");
    assert_eq!(
        owned::log_key(&later_remark),
        Some((lab.machine(AUTHOR), 2)),
        "the counter goes on past the sealed generation's"
    );
    exchange(6);
    for n in [ROLLER, AUTHOR] {
        let handle = at(n);
        let view = handle.view().unwrap();
        let seats = &view.owned[SEAT.name].rows;
        assert_eq!(
            seats[&seat_key(AUTHOR)].operation,
            third,
            "Mac {n}: a later generation's row over an earlier one's"
        );
        assert_eq!(seats[&seat_key(AUTHOR)].value["label"], json!("third"));
        assert_eq!(
            seats[&seat_key(ROLLER)],
            before.owned[SEAT.name].rows[&seat_key(ROLLER)],
            "Mac {n}: the roller's seat is still read from the sealed log"
        );
        assert_eq!(
            remark_keys(&view),
            BTreeSet::from([
                roller_remark.clone(),
                author_remark.clone(),
                later_remark.clone()
            ]),
            "Mac {n}: every remark of both generations, each at its own key"
        );
        assert_eq!(
            view.logs[REMARK.name].inert, 0,
            "Mac {n}: no key used twice"
        );
        let later = remark_op(&view, &later_remark);
        let history = handle.history().unwrap();
        for id in [third, later] {
            let entry = history.iter().find(|entry| entry.operation == id).unwrap();
            assert_eq!(entry.generation, 2, "Mac {n}");
            assert_eq!(entry.machine, Some(lab.machine(AUTHOR)), "Mac {n}");
        }

        // As of the new seat, a point of the second generation: the sealed rows are held.
        let then = handle.view_at(Point::At(third)).unwrap();
        let seats = &then.owned[SEAT.name].rows;
        assert_eq!(seats[&seat_key(AUTHOR)].operation, third, "Mac {n}");
        assert_eq!(seats[&seat_key(ROLLER)].operation, roller_seat, "Mac {n}");
        assert_eq!(remark_keys(&then), remark_keys(&before), "Mac {n}");

        // A diff from a sealed record to the head, and one within the second generation.
        let added = (
            REMARK.name.to_owned(),
            later_remark.clone(),
            Delta::Added,
            vec![later],
        );
        let across = handle.diff(second, Point::Head).unwrap();
        assert_eq!(
            own_rows(&across),
            vec![
                added.clone(),
                (
                    SEAT.name.to_owned(),
                    seat_key(AUTHOR),
                    Delta::Changed,
                    vec![third]
                ),
            ],
            "Mac {n}"
        );
        let seat_diff = across
            .cells
            .iter()
            .find(|cell| cell.collection == SEAT.name)
            .unwrap();
        assert_eq!(seat_diff.before[0].operation, second, "Mac {n}");
        assert_eq!(seat_diff.by[0].machine, Some(lab.machine(AUTHOR)));
        assert_eq!(seat_diff.by[0].said.as_ref(), Some(&saids[AUTHOR]));
        // From a record of the second generation signed before the new seat: the seat is a
        // change of the sealed row, never a row added, and no sealed row differs.
        let within = handle.diff(earlier, Point::Head).unwrap();
        assert_eq!(
            own_rows(&within),
            own_rows(&across),
            "Mac {n}: a diff within the second generation holds the sealed rows on both sides"
        );
        let seat_diff = within
            .cells
            .iter()
            .find(|cell| cell.collection == SEAT.name)
            .unwrap();
        assert_eq!(seat_diff.before[0].operation, second, "Mac {n}");
        let within = handle.diff(third, Point::Head).unwrap();
        assert_eq!(own_rows(&within), vec![added], "Mac {n}");
    }

    // A handle asked for the second generation by name reads from it on, and an append
    // through it still goes on past what its Mac spent in the sealed generation.
    let named = Handle::at(lab.paths[ROLLER].clone(), &KEPT_2)
        .saying(saids[ROLLER].clone())
        .append_log(
            &REMARK,
            &[json!({"text": "by name"}).as_object().unwrap().clone()],
        )
        .unwrap()
        .remove(0);
    assert_eq!(owned::log_key(&named), Some((lab.machine(ROLLER), 2)));
}

/// A final checkpoint is final (tracker LT-B10D1, design §5, "Nothing above S changes what S's
/// state holds"): Mac 0 writes a machine row and names it 20 days ago, and its pass signs and
/// records a checkpoint of both, final at once since it is the only member whose frontier tip is
/// recent. Both Macs' reads name that checkpoint as the settled floor S, Mac 1's though it signed
/// nothing. Then, above S, Mac 0 writes a record reusing the naming's request id with another
/// command, a revert of the naming, a revert of that revert and one more edit:
///
/// - the reused request id leaves both records standing, the naming below S and the reuse;
/// - the revert of an edit below S is refused, with its own refusal, as is the revert of that
///   revert, and no revert of the edit is written;
/// - the cells are the cells at S with the two edits above applied, the state at S recomputed
///   now is the state the checkpoint signed, and the view at S is the view read before;
/// - the reverts' writer is not held to recovery for them, and the history reads as complete;
/// - an unavailable (quarantined) record above S holds back its writer's records above S and
///   leaves those below eligible, and a quarantined record below S makes no writer opaque (it is
///   still awaited: the read folds records, and its write is not in the cells).
///
/// NEGATIVE CONTROLS, each against `documents::log`:
/// - request identities judged on one side (`Log::judge` grouping without the
///   `partition(|id| self.is_settled(id))`): `the record below S stands: Some("request identity
///   reused with different commands")`;
/// - the settled-revert rule removed (its branch in `Log::judge` never taken): `the revert of an
///   edit below S is refused: None`;
/// - a revert of a settled-refused revert judged as an ordinary revert (the `revert_refusals.get
///   (target)` arm dropped from `Log::judge`): `a revert of that refused revert reaches below S
///   through it`, left `Some("revert does not name a structurally valid observed edit with the
///   same keys")`;
/// - every refused revert counted against its writer (`Log::faulty_reverts` unfiltered): `Err(…
///   "local proposal writer has withheld, unavailable or refused history; recovery required")`;
/// - opaque authors judged without the floor (`!self.is_settled(id)` dropped from both filters in
///   `Log::judge`): `an unavailable record above S leaves those below eligible: [false, false]`.
#[test]
fn a_final_checkpoint_is_final_for_requests_reverts_and_opaque_authors() {
    use super::checkpoint::Signed;
    use crate::documents::log::{Log, SETTLED_REVERT_REFUSAL};
    const OPAQUE: &str = "author has unavailable retained request history";
    use aterm_secrets::commons::hearing::{self, Ledger, Said};
    const DAY: u64 = 24 * 60 * 60 * 1000;
    let now = aterm_time::unix_millis().unwrap();
    let then = now - 20 * DAY;
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    Registry::at(lab.paths[0].clone()).open().unwrap();
    let here = Handle::at(lab.paths[0].clone(), &REGISTRY);
    let born = append_history_at(
        &here,
        Command::Write {
            request: [91; 16],
            changes: machine(MC),
            said: None,
        },
        then,
    );
    let nickname = put(&MACHINE, MC, "nickname", json!("settled"));
    let named = append_history_at(
        &here,
        Command::Write {
            request: [92; 16],
            changes: vec![nickname.clone()],
            said: None,
        },
        then + 1_000,
    );
    let bootstrap = lab.bootstrap(0);
    let access = bootstrap.access().unwrap().unwrap();
    let mut ready = Ledger::new(bootstrap.trust());
    for member in access.members() {
        ready.record(
            member.binding().actor(),
            now,
            Said::Version(hearing::READS_CHECKPOINTS),
        );
    }
    assert_eq!(
        here.checkpoint_every(&ready, now, 1).unwrap(),
        Signed::Proposed
    );
    let floor = application::observe_store(&lab.paths[0], here.descriptor(), |_, store, _| {
        Ok(store.floor().cloned())
    })
    .unwrap()
    .expect("the pass records the final checkpoint as the floor");
    let at_floor = here.view().unwrap();

    let reused = append_history_at(
        &here,
        Command::Write {
            request: [92; 16],
            changes: vec![put(&MACHINE, MC, "status", json!("parked"))],
            said: None,
        },
        now,
    );
    let undo = append_history_at(
        &here,
        Command::Revert {
            request: [93; 16],
            operation: named,
            changes: vec![crate::database::Change {
                value: None,
                ..nickname.clone()
            }],
            said: None,
        },
        now + 1,
    );
    let redo = append_history_at(
        &here,
        Command::Revert {
            request: [96; 16],
            operation: undo,
            changes: vec![nickname.clone()],
            said: None,
        },
        now + 2,
    );
    let later = append_history_at(
        &here,
        Command::Write {
            request: [94; 16],
            changes: vec![put(&MACHINE, MC, "location", json!("rack b"))],
            said: None,
        },
        now + 3,
    );
    repair_new_registry(&lab);
    repair_registry(&lab, 2);

    // Every replica names the same floor: the pass's, on the Mac that signed it and on one that
    // signed nothing.
    for paths in &lab.paths {
        let named_floor =
            application::observe_store(paths, here.descriptor(), |session, store, scope| {
                let log = Log::read_with(session, store, scope, super::decode)?;
                Ok(log.settled_frontier().cloned())
            })
            .unwrap();
        assert_eq!(
            named_floor.as_ref(),
            Some(&floor.frontier),
            "the settled floor"
        );
    }

    application::observe_store(&lab.paths[0], here.descriptor(), |session, store, scope| {
        let log = Log::read_with(session, store, scope, super::decode)?;
        let snapshot = log.snapshot();
        let op = |id: &Digest| {
            snapshot
                .operations
                .iter()
                .find(|op| op.header.id == *id)
                .unwrap()
                .clone()
        };
        // The request rule.
        assert_eq!(op(&named).refusal, None, "the record below S stands");
        assert!(op(&named).eligible && op(&named).duplicate_of.is_none());
        assert_eq!(
            op(&reused).refusal,
            None,
            "the reuse above S is a new write"
        );
        assert!(op(&reused).eligible && op(&reused).duplicate_of.is_none());
        // The revert rule, read and written.
        assert_eq!(
            op(&undo).refusal.as_deref(),
            Some(SETTLED_REVERT_REFUSAL),
            "the revert of an edit below S is refused"
        );
        assert!(!op(&undo).eligible);
        assert_eq!(
            op(&redo).refusal.as_deref(),
            Some(SETTLED_REVERT_REFUSAL),
            "a revert of that refused revert reaches below S through it"
        );
        let written = log.compensate(
            [95; 16],
            named,
            &BTreeSet::new(),
            &crate::documents::Unwritten::new(),
        );
        assert!(
            written
                .as_ref()
                .is_err_and(|error| error.to_string().contains(SETTLED_REVERT_REFUSAL)),
            "no revert of an edit below S is written: {written:?}"
        );
        // The cells: those at S with the records above applied.
        let tips: BTreeSet<Digest> = floor.frontier.values().map(|tip| tip.digest).collect();
        let mut cut = tips.clone();
        for tip in &tips {
            cut.extend(log.causal_index().ancestors(tip));
        }
        let mut folded = log.as_of_cut(&cut).cells_without(
            None,
            &BTreeSet::new(),
            &crate::documents::Unwritten::new(),
        );
        for id in [reused, later] {
            crate::documents::apply(
                &|later, earlier| log.observes(later, earlier),
                &mut folded,
                id,
                log.record_changes(&id).unwrap(),
                true,
            );
        }
        assert_eq!(
            log.cells_without(None, &BTreeSet::new(), &crate::documents::Unwritten::new()),
            folded,
            "the fold from S"
        );
        let view = View::read(REGISTRY.table, session, &log)?;
        let key = (nickname.collection.clone(), nickname.key.clone());
        assert_eq!(
            view.cells.get(&key),
            at_floor.cells.get(&key),
            "the naming stands"
        );
        assert!(view.refused.contains(&undo) && !view.refused.contains(&named));
        assert_eq!(
            super::checkpoint::state(REGISTRY.table, session, &log, &cut)?,
            floor.state,
            "the state at S is the state the checkpoint signed"
        );
        let at_s = View::read(REGISTRY.table, session, &log.as_of_cut(&cut))?;
        assert_eq!(at_s.cells, at_floor.cells, "the cells at S");
        assert_eq!(at_s.owned, at_floor.owned, "the owned rows at S");

        // A revert refused for reaching below S is no fault of its writer's: it writes on.
        let me = session.actor();
        assert!(log.writable(me).is_ok(), "{:?}", log.writable(me));
        assert!(
            log.history_complete(),
            "a settled refusal is judged history"
        );
        assert!(log.snapshot().history_complete(), "in the snapshot too");

        // The opaque rule: the writer's newest record above S quarantined here holds back its
        // earlier records above S, and none below.
        let dark = log.with_quarantined(&later);
        let snapshot = dark.snapshot();
        let standing = |id: &Digest| {
            snapshot
                .operations
                .iter()
                .find(|op| op.header.id == *id)
                .is_some_and(|op| op.eligible && op.refusal.is_none())
        };
        assert_eq!(
            [standing(&born), standing(&named)],
            [true, true],
            "an unavailable record above S leaves those below eligible"
        );
        assert!(
            !standing(&reused),
            "and holds back its writer's records above S"
        );
        assert_eq!(
            dark.cells_without(None, &BTreeSet::new(), &crate::documents::Unwritten::new()),
            log.as_of_cut(&cut).cells_without(
                None,
                &BTreeSet::new(),
                &crate::documents::Unwritten::new()
            ),
            "with everything above S held back, the cells are those at S"
        );
        // A record below S quarantined here makes no writer opaque. It is still awaited, as is
        // what the store's quarantine withholds above S: a read folds the records, and its write
        // is missing from the cells until a read takes them from S's state (tracker LT-B10D2).
        let settled_dark = log.with_quarantined(&named);
        let snapshot = settled_dark.snapshot();
        assert!(
            snapshot
                .operations
                .iter()
                .all(|op| op.refusal.as_deref() != Some(OPAQUE)),
            "an unavailable record below S makes no writer opaque"
        );
        let awaited = settled_dark.awaited(me);
        assert!(
            awaited.contains(&named) && awaited.contains(&later),
            "{awaited:?}"
        );
        Ok(())
    })
    .unwrap();
}

/// `table`'s state as `log` holds it, read from `base`, the state at `log`'s settled floor, and
/// the records above that floor ([`crate::typed::state::State::fold`]); `resolved` is the whole
/// log's resolution.
fn state_above(
    table: &'static Table,
    log: &Log,
    resolved: &Resolved,
    base: &crate::typed::state::State,
) -> crate::typed::state::State {
    let frontier = log.settled_frontier().expect("the log has a settled floor");
    let (eligible, merges) = log.eligible_and_merges();
    let ops: Vec<Op> = eligible
        .into_iter()
        .filter(|(operation, _)| !log.is_settled(&operation.header.id))
        .filter_map(|(operation, command)| Op::from_eligible(0, operation, command))
        .collect();
    let merges: Vec<(Actor, u64)> = merges
        .into_iter()
        .filter(|(actor, sequence)| {
            frontier
                .get(actor)
                .is_none_or(|tip| *sequence > tip.sequence)
        })
        .collect();
    crate::typed::state::State::fold(
        table,
        Some(base),
        &ops,
        &merges,
        resolved.carried_records(),
        log.unwritable(|id| !log.is_settled(id)),
        |roster: &Digest, actor: &Actor| {
            resolved.bindings.get(&(*roster, *actor)).copied().flatten()
        },
    )
    .0
}

/// A read from the state at the floor equals the full read (tracker LT-B10D2, design §5, "The
/// state at F serves every registry reader"). Mac 0 writes the second registry generation 20 days
/// ago: 20 automatic records (machine facts and account observations), owner records, a schema
/// row and an archived row carrying a record of the first generation. Its pass signs a checkpoint
/// of all of it, final at once since Mac 0 is the only member whose frontier tip is recent. Above
/// the floor it writes 8 more automatic records (the last 4 past its quota of 24), owner records,
/// schema rows that raise one collection's version and split another's, a request id reused with
/// another command, the same carried record archived again and a second one archived. Then:
///
/// - the state at the floor, read from exactly the cut, digests to the state the checkpoint
///   signed (its cells and every part below), and does so still with a record above
///   the floor unavailable here;
/// - that state, with the records above the floor folded in, is the state the full read folds
///   from every record: the owned rows of this generation and the carried rows apart, the quota
///   counters, the schema rows and the facts (first writes, records touching a collection, the
///   writers with refused history);
/// - the full read's quota, schema, carried rows and facts are the ones the records say: the
///   quota continues across the floor, the first machine-facts write is below it.
///
/// NEGATIVE CONTROL, run once: `State::fold` charging each writer from a fresh counter in place
/// of its base's (`owned::charge(table, &mut Counter::default(), …)`) fails `the fold from the
/// state at the floor`: the folded account row is `n` 37, a record past the quota, where the full
/// read's is `n` 33. And `Log::as_of_cut` keeping every opaque writer of the whole log (its
/// `opaque_authors` cloned) fails `the state at the floor is the cut's alone`: the writer of the
/// unavailable record above the floor is a refused writer of the cut.
#[test]
fn a_view_from_the_state_at_the_floor_equals_the_full_read() {
    use super::checkpoint::Signed;
    use crate::{alab::registry::ACCOUNT_OBS, typed::SCHEMA};
    use aterm_secrets::commons::hearing::{self, Ledger, Said};
    const DAY: u64 = 24 * 60 * 60 * 1000;
    let now = aterm_time::unix_millis().unwrap();
    let then = now - 20 * DAY;
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    Registry::at(lab.paths[0].clone()).open().unwrap();
    let first = Handle::at(lab.paths[0].clone(), &REGISTRY);
    let here = Handle::at(lab.paths[0].clone(), &REGISTRY_2);
    application::with_store(&lab.paths[0], true, here.descriptor(), |_, _, _| Ok(())).unwrap();
    let me = keys::uuid32(&lab.machine(0));
    let pk = "mc-0000000000000001";
    let facts = |n: u64| Change {
        collection: MACHINE_FACTS.name.into(),
        key: me.clone(),
        value: Some(json!({"r": 1, "n": n, "mac": pk, "label": format!("m{n}")})),
    };
    let observed = |account: &str, n: u64| Change {
        collection: ACCOUNT_OBS.name.into(),
        key: format!("{account}.{me}"),
        value: Some(json!({"r": 1, "n": n})),
    };
    let schema = |collection: &str, version: u64, note: &str| Change {
        collection: SCHEMA.into(),
        key: collection.into(),
        value: Some(json!({"r": version, "note": note})),
    };
    let write = |request: [u8; 16], changes: Vec<Change>, at: u64| {
        append_history_at(
            &here,
            Command::Write {
                request,
                changes,
                said: None,
            },
            at,
        )
    };
    let numbered = |n: u64| {
        let mut request = [0; 16];
        request[..8].copy_from_slice(&n.to_le_bytes());
        request
    };
    // Two first-generation records of Mac 0, never ingested: only archived rows carry them.
    let (kept, kept_wire) = sealed_record(&first, 7, vec![observed("ac-carried1", 1)], 7_000);
    let (later, later_wire) = sealed_record(&first, 8, vec![observed("ac-carried2", 1)], 8_000);

    // At or below the floor.
    let first_facts = write(numbered(1), vec![facts(1)], then);
    for n in 2..=20 {
        let change = if n % 2 == 0 {
            facts(n)
        } else {
            observed("ac-0000000000000001", n)
        };
        write(numbered(n), vec![change], then + n);
    }
    write(
        numbered(21),
        vec![put(&MACHINE, MC, "nickname", json!("one"))],
        then + 21,
    );
    write(numbered(22), vec![schema("machine", 1, "below")], then + 22);
    archive(&here, 30, kept, &kept_wire);
    write(
        numbered(23),
        vec![put(&MACHINE, MC, "location", json!("x"))],
        then + 25,
    );

    let bootstrap = lab.bootstrap(0);
    let access = bootstrap.access().unwrap().unwrap();
    let mut ready = Ledger::new(bootstrap.trust());
    for member in access.members() {
        ready.record(
            member.binding().actor(),
            now,
            Said::Version(hearing::READS_CHECKPOINTS),
        );
    }
    assert_eq!(
        here.checkpoint_every(&ready, now, 1).unwrap(),
        Signed::Proposed
    );
    let floor = application::observe_store(&lab.paths[0], here.descriptor(), |_, store, _| {
        Ok(store.floor().cloned())
    })
    .unwrap()
    .expect("the pass records the final checkpoint as the floor");

    // Above it.
    for n in 31..=38 {
        let change = if n % 2 == 0 {
            facts(n)
        } else {
            observed("ac-0000000000000001", n)
        };
        write(numbered(n), vec![change], now + n);
    }
    let renamed = write(
        numbered(39),
        vec![put(&MACHINE, MC, "nickname", json!("two"))],
        now + 39,
    );
    write(numbered(40), vec![schema("machine", 2, "above")], now + 40);
    write(numbered(41), vec![schema("provider", 1, "one")], now + 41);
    write(numbered(42), vec![schema("provider", 1, "two")], now + 42);
    write(
        [202; 16],
        vec![put(&MACHINE, MC, "nickname", json!("c"))],
        now + 43,
    );
    write(
        [202; 16],
        vec![put(&MACHINE, MC, "nickname", json!("d"))],
        now + 44,
    );
    archive(&here, 50, kept, &kept_wire);
    archive(&here, 51, later, &later_wire);

    application::observe_store(&lab.paths[0], here.descriptor(), |session, store, scope| {
        let table = REGISTRY_2.table;
        let log = Log::read_with(session, store, scope, REGISTRY_2.decoder())?;
        assert_eq!(
            log.settled_frontier(),
            Some(&floor.frontier),
            "the settled floor"
        );
        let resolved = Resolved::read(table, session, &log)?;
        let full = View::of(table, &log, &resolved);
        let tips: BTreeSet<Digest> = floor.frontier.values().map(|tip| tip.digest).collect();
        let mut cut = tips.clone();
        for tip in &tips {
            cut.extend(log.causal_index().ancestors(tip));
        }

        // The state at the floor is the state the checkpoint signed.
        let (at_floor, claims) = super::checkpoint::at_cut(table, session, &log, &cut)?;
        let base = at_floor.state.clone().settled(table);
        let index = log.causal_index();
        let cells = crate::checkpoint::cells_digest(&at_floor.cells, |id| {
            index.get(id).map(|header| (header.actor, header.sequence))
        })?;
        assert_eq!(
            super::checkpoint::digest(cells, &base, &claims)?,
            floor.state,
            "the state digest is what the checkpoint signs"
        );

        // A record above the floor this Mac cannot read leaves the state at it as it was.
        assert_eq!(
            super::checkpoint::state(table, session, &log.with_quarantined(&renamed), &cut)?,
            floor.state,
            "the state at the floor is the cut's alone"
        );

        // The fold from it is the full read's.
        let folded = state_above(table, &log, &resolved, &base);
        assert_eq!(folded, full.state, "the fold from the state at the floor");

        // And the full read's parts are what the records say.
        let actor = session.actor();
        let counter = full.state.quota[&actor];
        assert_eq!(
            (counter.spent, full.quota.over.len()),
            (Some(28), 4),
            "the quota continues across the floor"
        );
        assert_eq!(
            (base.quota[&actor].spent, base.quota[&actor].merges_after),
            (Some(20), 0)
        );
        assert_eq!(
            full.state.first[&(actor, MACHINE_FACTS.name.to_owned(), me.clone())].operation,
            first_facts,
            "the first machine-facts write is below the floor"
        );
        assert_eq!(
            full.state.touched[&(actor, ACCOUNT_OBS.name.to_owned())],
            9 + 4
        );
        assert!(
            base.refused.is_empty() && full.state.refused.contains(&actor),
            "the reused request id above the floor refuses its writer"
        );
        assert_eq!(
            (full.schema("machine"), full.schema("provider")),
            (
                crate::typed::owned::SchemaState::One {
                    version: 2,
                    row: json!({"r": 2, "note": "above"})
                },
                crate::typed::owned::SchemaState::Split { version: 1 }
            )
        );
        assert_eq!(base.carried_ids, BTreeSet::from([kept]));
        assert_eq!(full.state.carried_ids, BTreeSet::from([kept, later]));
        let accounts = &full.owned[ACCOUNT_OBS.name].rows;
        assert!(
            accounts.contains_key(&format!("ac-carried1.{me}"))
                && accounts.contains_key(&format!("ac-carried2.{me}")),
            "the carried rows: {:?}",
            accounts.keys().collect::<Vec<_>>()
        );
        Ok(())
    })
    .unwrap();
}

/// A claim held across a final checkpoint reads from the state at the floor as from the full
/// history (tracker LT-B10D3, design §5, "The state at F serves every registry reader"). Mac 0
/// writes the registry's machines 20 days ago: A takes label `m7`, B contends for it, A releases
/// it, B releases it by retiring, A retakes it, B is revived (which reasserts its `m7` after A's
/// retake), A releases and retakes it again (after B's revival), and B writes `m7` again, so B's
/// claim at the floor is a tenure that began at its revival and continues through a later write.
/// D is born with `m5`. Its pass signs a checkpoint of all of it, final at once. Above the floor
/// A writes `m7` again (its tenure from its last retake goes on), C is born with `m7`, B writes
/// its row live again (it stays in the period its revival began), and D is retired and revived.
/// Then:
///
/// - the claims at the floor (each claimed coordinate's latest writes with their tenures' starts
///   and the rows' lifecycle there) are in the state the checkpoint signed;
/// - a read of the records above the floor that takes every claim and lifecycle write at or
///   below it from those claims projects the machines, claims and settlements included, as the
///   read of every record does;
/// - and that read is what the records say: B holds `m7` from its revival, before A's last
///   retake and C's birth, and D holds `m5` again.
///
/// NEGATIVE CONTROL, run once: `Floor::at` keeping each latest write with `starts` of
/// `Some(BTreeSet::from([id]))` (every kept write begins its tenure) fails `the claims from the
/// floor`: B's tenure begins at its write after A's last retake, so A reads `m7`. And
/// `Activity::read_inner` seeding no kept lifecycle write (`seed.kept.rows` unread) fails it too:
/// B's live write above the floor then begins from the initial period, which its revival's claim
/// is not in, so B's claim is withheld and A reads `m7`.
#[test]
fn a_claim_held_across_a_final_checkpoint_reads_from_the_state_at_the_floor() {
    use super::checkpoint::Signed;
    use aterm_secrets::commons::hearing::{self, Ledger, Said};
    const DAY: u64 = 24 * 60 * 60 * 1000;
    let now = aterm_time::unix_millis().unwrap();
    let then = now - 20 * DAY;
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    Registry::at(lab.paths[0].clone()).open().unwrap();
    let here = Handle::at(lab.paths[0].clone(), &REGISTRY_2);
    application::with_store(&lab.paths[0], true, here.descriptor(), |_, _, _| Ok(())).unwrap();
    let [a, b, c, d] = [
        "mc-000000000000000a",
        "mc-000000000000000b",
        "mc-000000000000000c",
        "mc-000000000000000d",
    ];
    let born = |pk: &str, label: &str| {
        let mut row = machine(pk);
        row[0] = put(&MACHINE, pk, "label", json!(label));
        row
    };
    let label = |pk: &str, label: &str| vec![put(&MACHINE, pk, "label", json!(label))];
    let lifecycle = |pk: &str, row: &str| vec![put(&MACHINE, pk, "row", json!(row))];
    let mut written = 0_u64;
    let mut write = |changes: Vec<Change>, at: u64| {
        written += 1;
        let mut request = [0; 16];
        request[..8].copy_from_slice(&written.to_le_bytes());
        append_history_at(
            &here,
            Command::Write {
                request,
                changes,
                said: None,
            },
            at,
        )
    };

    // At or below the floor.
    write(born(a, "m7"), then + 1);
    write(born(b, "m7"), then + 2);
    write(label(a, "m8"), then + 3);
    write(lifecycle(b, "retired"), then + 4);
    write(label(a, "m7"), then + 5);
    write(lifecycle(b, "live"), then + 6);
    write(label(a, "m8"), then + 7);
    write(label(a, "m7"), then + 8);
    write(label(b, "m7"), then + 9);
    write(born(d, "m5"), then + 10);

    let bootstrap = lab.bootstrap(0);
    let access = bootstrap.access().unwrap().unwrap();
    let mut ready = Ledger::new(bootstrap.trust());
    for member in access.members() {
        ready.record(
            member.binding().actor(),
            now,
            Said::Version(hearing::READS_CHECKPOINTS),
        );
    }
    assert_eq!(
        here.checkpoint_every(&ready, now, 1).unwrap(),
        Signed::Proposed
    );
    let floor = application::observe_store(&lab.paths[0], here.descriptor(), |_, store, _| {
        Ok(store.floor().cloned())
    })
    .unwrap()
    .expect("the pass records the final checkpoint as the floor");

    // Above it.
    write(label(a, "m7"), now + 11);
    write(born(c, "m7"), now + 12);
    write(lifecycle(b, "live"), now + 13);
    write(lifecycle(d, "retired"), now + 14);
    write(lifecycle(d, "live"), now + 15);

    application::observe_store(&lab.paths[0], here.descriptor(), |session, store, scope| {
        let table = REGISTRY_2.table;
        let log = Log::read_with(session, store, scope, REGISTRY_2.decoder())?;
        assert_eq!(
            log.settled_frontier(),
            Some(&floor.frontier),
            "the settled floor"
        );
        let full = View::read(table, session, &log)?;
        let tips: BTreeSet<Digest> = floor.frontier.values().map(|tip| tip.digest).collect();
        let mut cut = tips.clone();
        for tip in &tips {
            cut.extend(log.causal_index().ancestors(tip));
        }

        // The claims at the floor are in the state the checkpoint signed.
        let (at_floor, claims) = super::checkpoint::at_cut(table, session, &log, &cut)?;
        let index = log.causal_index();
        let cells = crate::checkpoint::cells_digest(&at_floor.cells, |id| {
            index.get(id).map(|header| (header.actor, header.sequence))
        })?;
        assert_eq!(
            super::checkpoint::digest(cells, &at_floor.state.clone().settled(table), &claims)?,
            floor.state,
            "the state digest, claims included, is what the checkpoint signs"
        );

        // A read of the records above the floor, every claim and lifecycle write at or below it
        // taken from the claims there.
        let mut above = full.clone();
        above.ops.retain(|op| !log.is_settled(&op.id));
        assert_eq!(
            (above.ops.len(), full.ops.len() - above.ops.len()),
            (5, 10),
            "the records above the floor, and those at or below it"
        );
        above.claims_floor = Some(std::sync::Arc::new(claims));
        let (read, settled) = above.project_settled(&MACHINE);
        let (whole, whole_settled) = full.project_settled(&MACHINE);
        assert_eq!(read, whole, "the claims from the floor");
        assert_eq!(
            settled, whole_settled,
            "and how each claimed column settled"
        );
        assert_eq!(above.project(&MACHINE), whole);

        // And what the records say.
        let label = |pk: &str| whole.rows[pk].get("label").cloned();
        assert_eq!(
            label(b),
            Some(json!("m7")),
            "B holds m7 from its revival, before A's last retake"
        );
        assert!(
            [label(a), label(c)]
                .iter()
                .all(|held| held.is_some() && *held != Some(json!("m7"))),
            "A and C read clash labels: {:?}",
            [label(a), label(c)]
        );
        assert_eq!(label(d), Some(json!("m5")), "D is revived with its label");
        Ok(())
    })
    .unwrap();
}

/// A lab for the checkpoint fence's read (tracker LT-C1): five seats on one roster and their
/// registry as Mac 0 holds it at `now`. Seats 0, 1 and 4 signed a record of it in the last 30
/// days (20, 20 and 5 days ago), seat 2 signed one 40 days ago, and seat 3 signed none. Answers
/// the lab, Mac 0's handle, each Mac's seat (its Commons writer) and `now`.
fn fence_lab() -> (Lab, Handle, Vec<Actor>, u64) {
    const DAY: u64 = 24 * 60 * 60 * 1000;
    let now = aterm_time::unix_millis().unwrap();
    let lab = Lab::new(&[Role::Writer; 5]);
    Registry::at(lab.paths[0].clone()).open().unwrap();
    let mac = |n: usize| Handle::at(lab.paths[n].clone(), &REGISTRY);
    let here = mac(0);
    append_history_at(
        &here,
        Command::Write {
            request: [90; 16],
            changes: machine(MC),
            said: None,
        },
        now - 40 * DAY,
    );
    repair_new_registry(&lab);
    // Each later record is written by a Mac that holds every earlier one, in the order of their
    // signed times, so the newest record of each recent seat has seen the whole table.
    let writes: [(usize, &str, u64); 4] = [
        (2, "two", now - 40 * DAY + 1_000),
        (1, "one", now - 20 * DAY),
        (0, "zero", now - 20 * DAY + 1_000),
        (4, "four", now - 5 * DAY),
    ];
    for (n, nickname, authored_ms) in writes {
        append_history_at(
            &mac(n),
            Command::Write {
                request: [91 + n as u8; 16],
                changes: vec![put(&MACHINE, MC, "nickname", json!(nickname))],
                said: None,
            },
            authored_ms,
        );
        repair_registry(&lab, 2);
    }
    let seats = (0..5)
        .map(|n| {
            let access = lab.bootstrap(n).access().unwrap().unwrap();
            access.local().binding().actor()
        })
        .collect();
    (lab, here, seats, now)
}

/// The ledger of [`fence_lab`]'s Mac 0 in which four seats hold the checkpoint fence, one for
/// each reason a seat can: seat 1 reported 0.202.0 yesterday, seat 2 was heard two days ago
/// with no report, seat 3 was never heard, and seat 4 reported a build that reads checkpoints
/// ten days ago, which is not in the last 7 days. Seat 0 reported one that reads them today.
fn fence_held(lab: &Lab, seats: &[Actor], now: u64) -> aterm_secrets::commons::hearing::Ledger {
    use aterm_secrets::commons::hearing::{self, Ledger, Said};
    const DAY: u64 = 24 * 60 * 60 * 1000;
    let mut ledger = Ledger::new(lab.bootstrap(0).trust());
    ledger.record(seats[0], now, Said::Version(hearing::READS_CHECKPOINTS));
    ledger.record(
        seats[1],
        now - DAY,
        Said::Version(hearing::Version(0, 202, 0)),
    );
    ledger.record(seats[2], now - 2 * DAY, Said::Nothing);
    ledger.record(
        seats[4],
        now - 10 * DAY,
        Said::Version(hearing::READS_CHECKPOINTS),
    );
    ledger
}

/// The ledger of [`fence_lab`]'s Mac 0 in which no seat holds the fence: every seat reported a
/// build that reads checkpoints, today.
fn fence_ready(lab: &Lab, seats: &[Actor], now: u64) -> aterm_secrets::commons::hearing::Ledger {
    use aterm_secrets::commons::hearing::{self, Ledger, Said};
    let mut ledger = Ledger::new(lab.bootstrap(0).trust());
    for seat in seats {
        ledger.record(*seat, now, Said::Version(hearing::READS_CHECKPOINTS));
    }
    ledger
}

/// How many checkpoint records `handle`'s store holds.
fn checkpoints_held(lab: &Lab, handle: &Handle) -> usize {
    application::observe_store(&lab.paths[0], handle.descriptor(), |_, store, _| {
        Ok(store.checkpoints().count())
    })
    .unwrap()
}

/// A checkpoint that is not signed says who holds it back and why (tracker LT-C1). With four of
/// five seats holding the fence, one for each reason, the pass signs nothing and the read on
/// demand names, on the roster, each of the four with its reason (an older build with its
/// release, no report, unheard) and, among the three seats that signed a record in the last 30
/// days, the two of them that hold it. Once every seat reports
/// a build that reads checkpoints, the read names the same seats in both sets and no holder in
/// either, and the pass signs.
/// NEGATIVE CONTROL: with `checkpoint::fence` keeping every roster holder as a recent one (the
/// `recent.contains(actor)` filter dropped), the seats that signed nothing in 30 days are named
/// among those that did: `the recent seats that hold the fence` with four holders on the left.
#[test]
fn a_fenced_checkpoint_says_which_seats_hold_it_and_why_and_an_open_fence_says_none() {
    use super::checkpoint::Signed;
    use aterm_secrets::commons::hearing::{self, Hold};
    let (lab, here, seats, now) = fence_lab();
    let sorted = |mut holders: Vec<(Actor, Hold)>| {
        holders.sort_by_key(|(seat, _)| *seat);
        holders
    };
    let roster: BTreeSet<Actor> = seats.iter().copied().collect();
    let recent: BTreeSet<Actor> = [seats[0], seats[1], seats[4]].into_iter().collect();

    let held = fence_held(&lab, &seats, now);
    assert_eq!(
        here.checkpoint_every(&held, now, 1).unwrap(),
        Signed::Fenced,
        "a seat holds the fence, so the pass signs nothing"
    );
    assert_eq!(checkpoints_held(&lab, &here), 0, "nothing is signed");
    let (_, _, fence) = here.usage_fenced_by(&held, now).unwrap();
    assert!(!fence.open());
    assert_eq!(fence.roster.seats, roster, "the seats on the roster");
    assert_eq!(
        fence.roster.holders,
        sorted(vec![
            (seats[1], Hold::Older(Some(hearing::Version(0, 202, 0)))),
            (seats[2], Hold::Older(None)),
            (seats[3], Hold::Unheard),
            (seats[4], Hold::Unheard),
        ]),
        "the roster's seats that hold the fence"
    );
    assert_eq!(
        fence.recent.seats, recent,
        "the seats that signed a record in the last 30 days"
    );
    assert_eq!(
        fence.recent.holders,
        sorted(vec![
            (seats[1], Hold::Older(Some(hearing::Version(0, 202, 0)))),
            (seats[4], Hold::Unheard),
        ]),
        "the recent seats that hold the fence"
    );

    let ready = fence_ready(&lab, &seats, now);
    let (_, _, open) = here.usage_fenced_by(&ready, now).unwrap();
    assert!(open.open());
    assert_eq!(open.roster.seats, roster);
    assert_eq!(open.roster.holders, vec![], "an open fence names no holder");
    assert_eq!(open.recent.seats, recent);
    assert_eq!(open.recent.holders, vec![]);
    assert_eq!(
        here.checkpoint_every(&ready, now, 1).unwrap(),
        Signed::Proposed,
        "past the fence the pass signs"
    );
    assert_eq!(checkpoints_held(&lab, &here), 1);
}

/// The pass signs exactly where the fence's function says it is open (tracker LT-C1): for each
/// ledger, the read's verdict at that ledger and time and the pass's answer agree, a fenced pass
/// carries the read's very answer and adds no checkpoint, and an open one signs. The fence is
/// the whole roster's: one seat that never signed a record of the table, unheard, holds it
/// while no recent seat does. So do a seat on an older build, a seat heard with no report, and
/// a seat heard today whose last report is older than 7 days.
/// NEGATIVE CONTROL: with `checkpoint::held` answering false (the pass opening every table), the
/// pass goes on to sign over a seat that signed nothing and is unheard, which the store
/// refuses: `seat 3 unheard: a table checkpoint waits until every member heard in the last 7
/// days runs aterm 0.203.0 or later: 1 do not yet`.
#[test]
fn the_checkpoint_pass_signs_exactly_where_the_fence_says_open() {
    use super::checkpoint::Signed;
    use aterm_secrets::commons::hearing::{self, Said};
    const DAY: u64 = 24 * 60 * 60 * 1000;
    let (lab, here, seats, now) = fence_lab();
    let reads = Said::Version(hearing::READS_CHECKPOINTS);
    // Every seat reads checkpoints, but for seat `n`, heard as `said` at `at_ms` (or never).
    let but = |n: usize, heard: &[(u64, Said)]| {
        let mut ledger = aterm_secrets::commons::hearing::Ledger::new(lab.bootstrap(0).trust());
        for (m, seat) in seats.iter().enumerate() {
            if m != n {
                ledger.record(*seat, now, reads);
            }
        }
        for (at_ms, said) in heard {
            ledger.record(seats[n], *at_ms, *said);
        }
        ledger
    };
    let cases = [
        ("seat 3 unheard", but(3, &[]), false),
        (
            "seat 1 on an older build",
            but(1, &[(now, Said::Version(hearing::Version(0, 202, 0)))]),
            false,
        ),
        (
            "seat 2 with no report",
            but(2, &[(now, Said::Nothing)]),
            false,
        ),
        (
            "seat 0's report is 8 days old",
            but(0, &[(now - 8 * DAY, reads), (now, Said::Unknown)]),
            false,
        ),
        (
            "every seat reads checkpoints",
            but(3, &[(now, reads)]),
            true,
        ),
        ("seat 3 unheard again", but(3, &[]), false),
    ];
    let mut signed = 0;
    for (what, ledger, open) in cases {
        let (_, _, verdict) = here.usage_fenced_by(&ledger, now).unwrap();
        assert_eq!(verdict.open(), open, "{what}");
        let passed = here
            .checkpoint_every(&ledger, now, 1)
            .unwrap_or_else(|error| panic!("{what}: {error}"));
        match passed {
            Signed::Fenced => {
                assert!(!open, "{what}: the pass is fenced where the fence is open");
            }
            Signed::Proposed => {
                assert!(open, "{what}: the pass signed over a held fence");
                signed += 1;
            }
            other => panic!("{what}: {other:?}"),
        }
        assert_eq!(checkpoints_held(&lab, &here), signed, "{what}");
    }
    assert_eq!(signed, 1);
}

/// A fenced pass takes no writer (tracker LT-C1): while another local writer holds the control
/// journal, the pass over a held fence still answers and signs nothing. The roster is judged
/// before anything is opened, so a Mac whose fence stays held (every one with a seat unheard
/// for 7 days) never reads a table's log, queues its hearing tick behind a writer, seals a held
/// write or repairs a tail to learn it signs nothing.
/// NEGATIVE CONTROL: with `Handle::checkpoint_every` opening the table before it judges (its
/// `held` branch removed), the pass waits out the admission on the journal's writer and is
/// refused: `a fenced pass under another writer: Err(Error("journal control writer admission:
/// another local journal writer is active"))`.
#[test]
fn a_fenced_checkpoint_pass_takes_no_writer() {
    use super::checkpoint::Signed;
    use aterm_secrets::commons::bootstrap::Bootstrap;
    let (lab, here, seats, now) = fence_lab();
    let held = fence_held(&lab, &seats, now);
    let (_custody, writer) = Bootstrap::cached_with_journal_until(
        &lab.paths[0],
        std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap()
    .expect("CONTROL: the control journal's writer is taken");
    let passed = here.checkpoint_every(&held, now, 1);
    drop(writer);
    assert!(
        matches!(passed, Ok(Signed::Fenced)),
        "a fenced pass under another writer: {passed:?}"
    );
    assert_eq!(checkpoints_held(&lab, &here), 0, "nothing is signed");
}

/// The fence's read never takes a busy vault for a Mac with no seat (tracker LT-C1): a vault
/// writer holds this Mac's state when the read starts and lets go as the read turns to the
/// table, and the read names the roster's five seats and their holders, as it does with no
/// writer. Its seats are the members the read's own observation validated, which waits a
/// writer out; a look taken before it without waiting reads nothing under a writer, which is
/// right for the pass (it signs nothing that time) and would have the read say no seat.
/// NEGATIVE CONTROL: with `Handle::usage_fenced` taking its roster from `seated` before the
/// read (no seat where that reads none), the fence has no seat: `the seats on the roster` with
/// an empty set on the left.
#[test]
fn the_fence_read_under_a_vault_writer_names_every_seat() {
    let (lab, here, seats, now) = fence_lab();
    fence_held(&lab, &seats, now)
        .write(&lab.bootstrap(0))
        .unwrap();
    let writer = aterm_secrets::client::test_hold_state_lock(&lab.paths[0]).unwrap();
    let let_go = std::rc::Rc::new(std::cell::Cell::new(false));
    crate::file::seam::toward_custody({
        let let_go = let_go.clone();
        move || {
            drop(writer);
            let_go.set(true);
        }
    });
    let read = here.usage_fenced(now);
    assert!(
        let_go.get(),
        "CONTROL: the writer held the state until the read turned to the table"
    );
    let (_, _, fence) = read.unwrap();
    assert_eq!(
        fence.roster.seats,
        seats.iter().copied().collect::<BTreeSet<Actor>>(),
        "the seats on the roster"
    );
    assert!(!fence.open());
    assert_eq!(fence.recent.seats.len(), 3);
}

/// What `aterm db table check` says of a table's checkpoint fence (tracker LT-C1). In words:
/// three sentences, for whether this machine may sign a checkpoint, the enrolled machines and
/// those that wrote a record in the last 30 days, each with how many hold checkpoints back for
/// each reason, the release a checkpoint needs beside the older ones, and no machine's id. In
/// JSON: the same sentences, each set's counts and seats by id, and each holder once with why,
/// the release it reported and whether it is a recent seat, every list sorted. `--fence` prints
/// the sentences alone and `--tsv` none of them. The verb reads the ledger this Mac keeps, in
/// which this Mac's own seat is judged by its running build.
/// NEGATIVE CONTROL: with `command::why` naming a seat heard with no report `unheard`, the
/// roster's sentence counts three machines not heard from: `the roster's counts by reason`.
#[test]
fn check_says_who_holds_the_checkpoint_fence_in_words_and_names_the_seats_in_json() {
    use aterm_secrets::commons::hearing::{self, Ledger};
    let (lab, here, seats, now) = fence_lab();
    let id = |n: usize| hex::encode_lower(&seats[n]);
    let ids = |of: &[usize]| {
        let mut ids: Vec<String> = of.iter().map(|n| id(*n)).collect();
        ids.sort();
        ids
    };
    let needs = hearing::READS_CHECKPOINTS;

    let held = fence_held(&lab, &seats, now);
    let (_, _, fence) = here.usage_fenced_by(&held, now).unwrap();
    let lines = command::fence_lines(&fence);
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert!(lines[0].contains("signs no new one"), "{lines:?}");
    let older = format!("1 on aterm older than {needs} (0.202.0)");
    assert!(
        lines[1].starts_with("Of 5 enrolled machines: ")
            && lines[1].contains(&older)
            && lines[1].contains("1 with no version report in 7 days")
            && lines[1].contains("2 not heard from in 7 days"),
        "the roster's counts by reason: {lines:?}"
    );
    assert!(
        lines[2].starts_with("Of the 3 that wrote a record in the last 30 days: ")
            && lines[2].contains(&older)
            && lines[2].contains("1 not heard from in 7 days")
            && !lines[2].contains("version report"),
        "the recent machines' counts by reason: {lines:?}"
    );
    for n in 0..5 {
        assert!(
            lines.iter().all(|line| !line.contains(&id(n)[..8])),
            "the words name no seat: {lines:?}"
        );
    }
    let holder = |n: usize, why: &str, version: Option<&str>, recent: bool| json!({"seat": id(n), "why": why, "version": version, "recent": recent});
    let mut holders = vec![
        holder(1, "older", Some("0.202.0"), true),
        holder(2, "no_report", None, false),
        holder(3, "unheard", None, false),
        holder(4, "unheard", None, true),
    ];
    holders.sort_by_key(|holder| holder["seat"].as_str().map(str::to_owned));
    assert_eq!(
        command::fence_json(&fence),
        json!({
            "open": false,
            "said": lines,
            "limit": 255,
            "roster": {
                "count": 5, "holding": 4, "older": 1, "no_report": 1, "unheard": 2,
                "seats": ids(&[0, 1, 2, 3, 4]),
            },
            "recent": {
                "count": 3, "holding": 2, "older": 1, "no_report": 0, "unheard": 1,
                "seats": ids(&[0, 1, 4]),
            },
            "holders": holders,
        })
    );

    let ready = fence_ready(&lab, &seats, now);
    let (_, _, fence) = here.usage_fenced_by(&ready, now).unwrap();
    let lines = command::fence_lines(&fence);
    assert!(lines[0].contains("may sign one"), "{lines:?}");
    assert!(
        lines[1..]
            .iter()
            .all(|line| line.ends_with(": none holds checkpoints back")),
        "{lines:?}"
    );
    let open = command::fence_json(&fence);
    assert_eq!(open["open"], json!(true));
    assert_eq!(open["holders"], json!([]));
    for set in ["roster", "recent"] {
        for count in ["holding", "older", "no_report", "unheard"] {
            assert_eq!(open[set][count], json!(0), "{set} {count}");
        }
    }
    assert_eq!(open["roster"]["seats"], json!(ids(&[0, 1, 2, 3, 4])));
    assert_eq!(open["recent"]["seats"], json!(ids(&[0, 1, 4])));

    // The verb judges by the ledger this Mac keeps beside its journal, which knows this Mac's
    // own seat by the build that runs: here a test binary's, older than any that reads
    // checkpoints, so seat 0 holds the fence too.
    let bootstrap = lab.bootstrap(0);
    held.write(&bootstrap).unwrap();
    let Output::Json(checked) = run(&lab.paths[0], &["check", "alab/registry"]) else {
        panic!("check prints JSON");
    };
    let (_, _, kept) = here
        .usage_fenced_by(&Ledger::read(&bootstrap), now)
        .unwrap();
    assert_eq!(checked["fence"], command::fence_json(&kept));
    let own = hearing::Version::own();
    assert!(own < needs, "{own}");
    assert_eq!(checked["fence"]["roster"]["holding"], json!(5));
    assert_eq!(checked["fence"]["roster"]["older"], json!(2));
    assert!(
        checked["fence"]["holders"]
            .as_array()
            .unwrap()
            .contains(&holder(0, "older", Some(&own.to_string()), true)),
        "{}",
        checked["fence"]
    );
    let said = command::fence_lines(&kept);
    assert!(
        said[1].contains(&format!("2 on aterm older than {needs} ({own} to 0.202.0)")),
        "{said:?}"
    );
    // In words at the command line: the sentences alone, a line each.
    let Output::Text(words) = run(&lab.paths[0], &["check", "alab/registry", "--fence"]) else {
        panic!("check --fence prints lines");
    };
    assert_eq!(words, format!("{}\n", said.join("\n")));
    // The findings' lines are as they were: the fence is no finding.
    let Output::Text(tsv) = run(&lab.paths[0], &["check", "alab/registry", "--tsv"]) else {
        panic!("check --tsv prints lines");
    };
    assert!(
        said.iter().all(|line| !tsv.contains(line.as_str())),
        "{tsv}"
    );
}

/// The fence's words and JSON over fences no lab roster makes (tracker LT-C1). A roster past
/// the JSON's bound lists that many seats and holders and counts them all. One enrolled machine
/// is said in the singular, and a table nobody wrote in 30 days says so. A seat this Mac did
/// not watch for 7 days is one not heard from in 7 days, counted and named with those.
/// NEGATIVE CONTROL: with `command::fence_json` listing every holder (its `take` removed), 299
/// holders are listed past the bound of 255: `the holders listed`.
#[test]
fn the_fence_json_is_bounded_and_its_words_cover_one_machine_and_no_recent_writer() {
    use super::checkpoint::{Fence, Seats};
    use aterm_secrets::commons::hearing::Hold;
    let seat = |n: u16| {
        let mut seat: Actor = [0; 48];
        seat[..2].copy_from_slice(&n.to_be_bytes());
        seat
    };
    let all: BTreeSet<Actor> = (0..300).map(seat).collect();
    let wide = Fence {
        roster: Seats {
            holders: (1..300).map(|n| (seat(n), Hold::Unheard)).collect(),
            seats: all,
        },
        recent: Seats {
            seats: BTreeSet::new(),
            holders: Vec::new(),
        },
    };
    let json = command::fence_json(&wide);
    assert_eq!(json["limit"], json!(command::FENCE_SEATS));
    assert_eq!(json["roster"]["count"], json!(300));
    assert_eq!(json["roster"]["holding"], json!(299));
    assert_eq!(json["roster"]["unheard"], json!(299));
    assert_eq!(
        json["roster"]["seats"].as_array().unwrap().len(),
        command::FENCE_SEATS,
        "the seats listed"
    );
    assert_eq!(
        json["holders"].as_array().unwrap().len(),
        command::FENCE_SEATS,
        "the holders listed"
    );
    let lines = command::fence_lines(&wide);
    assert!(
        lines[1].contains("299 not heard from in 7 days"),
        "{lines:?}"
    );
    assert_eq!(lines[2], "No machine wrote a record in the last 30 days");

    let one = Fence {
        roster: Seats {
            seats: [seat(7)].into_iter().collect(),
            holders: vec![(seat(7), Hold::Unwatched)],
        },
        recent: Seats {
            seats: [seat(7)].into_iter().collect(),
            holders: vec![(seat(7), Hold::Unwatched)],
        },
    };
    let lines = command::fence_lines(&one);
    assert!(
        lines[1].starts_with("Of 1 enrolled machine: 1 not heard from in 7 days"),
        "{lines:?}"
    );
    let json = command::fence_json(&one);
    assert_eq!(json["roster"]["unheard"], json!(1));
    assert_eq!(json["holders"][0]["why"], json!("unheard"));
    assert_eq!(json["holders"][0]["recent"], json!(true));
}
