// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `lab/tracker`'s schema and rules (package TK-10, docs/DESIGN-lab-tracker-2026-11-14.md §3,
//! §7 findings 4, 7, 9 and 10): the family on test replicas, a clean fixture of every table,
//! and each thing the lab tracker's data holds that the tracker maps or reports.

use super::{
    TRACKER, checks, mapped,
    tables::{
        CHANGE, COMPETITION, DECISION, GOAL, GROUP, LANE, METRIC, PACKAGE, PLAN, PROJECT, RELEASE,
        REPO, REQUEST, RULING, STOP, VIEW,
    },
};
use crate::database::{Candidate, Cell, Change};
use crate::typed::{
    Collection, Kind, Ty,
    cell::{self, CellKey, CellValue, Cells},
    check, guard, pk_grammar,
    values::Principal,
};
use aterm_json::{Value, json};
use std::collections::BTreeSet;

/// The owner's write of the row `pk` of `collection`: each of `cells`, a set column's members as
/// an array, and the lifecycle a born row takes.
fn row(collection: &Collection, pk: &str, cells: &[(&str, Value)]) -> Vec<Change> {
    let mut changes = Vec::new();
    for (column, value) in cells {
        let ty = &collection
            .column(column)
            .unwrap_or_else(|| panic!("{}.{column}", collection.name))
            .ty;
        if let (Ty::Set(_), Value::Array(members)) = (ty, value) {
            for member in members {
                changes.push(
                    cell::member_change(
                        collection,
                        pk,
                        column,
                        member.as_str().expect("a set's member is text"),
                        true,
                        |x| CellValue::owner(collection.version, x),
                    )
                    .unwrap_or_else(|e| panic!("{}.{column}: {e:?}", collection.name)),
                );
            }
        } else {
            changes.push(put(collection, pk, column, value.clone()));
        }
    }
    changes.push(put(collection, pk, "row", json!("live")));
    changes
}

/// The owner's write of one cell.
fn put(collection: &Collection, pk: &str, column: &str, value: Value) -> Change {
    cell::change(
        collection,
        CellKey {
            pk,
            col: column,
            elem: None,
        },
        &CellValue::owner(collection.version, value),
    )
    .unwrap_or_else(|e| panic!("{}.{column}: {e:?}", collection.name))
}

/// The table's cells once `rows` have landed on nothing, each cell one candidate: what a Mac
/// that holds every write reads.
fn landed(rows: &[Vec<Change>]) -> Cells {
    let mut cells = Cells::new();
    for (n, change) in rows.iter().flatten().enumerate() {
        let mut operation = [0; 32];
        operation[..8].copy_from_slice(&(n as u64 + 1).to_be_bytes());
        cells.insert(
            (change.collection.clone(), change.key.clone()),
            Cell {
                stopped: false,
                candidates: vec![Candidate {
                    operation,
                    value: change.value.clone().expect("a write"),
                }],
            },
        );
    }
    cells
}

/// A principal aterm stated: an agent's session.
pub(crate) fn agent() -> Principal {
    Principal {
        session: Some("s-47051b9a207955a92b14".to_owned()),
        program: Some("claude".to_owned()),
        machine: Some("0123456789abcdef0123456789abcdef".to_owned()),
        ..Principal::default()
    }
}

/// This package's own exit command: a test's path longer than the run of token characters the
/// free-text guard reads as a key.
const EXIT_COMMAND: &str = "targo --unverified test -p aterm-db --lib \
     lab::tracker::tests::the_schema_compiles_and_a_clean_fixture_checks_clean";

/// A package's exit test, typed.
fn exit() -> Value {
    json!({
        "repos": ["aterm"],
        "in": "aterm",
        "command": EXIT_COMMAND,
        "needs": ["macos"],
        "timeout_s": 3600,
        "passes": "exit-0",
    })
}

/// The rows of `lab/packages` a shared-file test needs, and only what they name: one ready
/// package, its lane, which names no repo and no goal, and the repo its exit runs in.
pub(crate) fn packages() -> Vec<Vec<Change>> {
    vec![
        row(&REPO, "aterm", &[("id", json!("aterm"))]),
        row(
            &LANE,
            "tables",
            &[
                ("id", json!("tables")),
                ("name", json!("Tables")),
                ("prefix", json!("TK")),
            ],
        ),
        row(
            &PACKAGE,
            "TK-05",
            &[
                ("id", json!("TK-05")),
                ("lane", json!("tables")),
                ("title", json!("The tracker's logs as shared files")),
                ("exit", exit()),
                ("state", json!("ready")),
            ],
        ),
    ]
}

/// A package that needs a mapped key (its id holds a dot), and a decision that does (37 bytes).
const DOTTED: &str = "TI-W0.1";
const LONG: &str = "S8-ONE-AUTHORITATIVE-SID-GRAMMAR-TEST";

/// A clean fixture: a row of every cells table, one write each, with a row of each kind the
/// key grammar does not take as it is named (a dotted package and metric, a long decision, a
/// project of two words, a release with a slash), every reference naming a row here.
pub(crate) fn fixture() -> Vec<Vec<Change>> {
    let metric = mapped::key("A10.lab_objects");
    let dotted = mapped::key(DOTTED);
    let long = mapped::key(LONG);
    let release = mapped::key("aterm/0.204");
    let after = exit();
    let quote = "we want verification on and for it to work.";
    let item = "no second tracker beside this one";
    vec![
        row(
            &REPO,
            "aterm",
            &[
                ("id", json!("aterm")),
                ("remote", json!("alab/aterm")),
                ("path", json!("aterm")),
            ],
        ),
        row(
            &GROUP,
            "A",
            &[
                ("id", json!("A")),
                ("horizon", json!("short")),
                ("name", json!("Product")),
                ("scoreboard", json!("goals met of goals set")),
            ],
        ),
        row(
            &GOAL,
            "A10",
            &[
                ("id", json!("A10")),
                ("group", json!("A")),
                ("title", json!("The lab plans from its own tables")),
                ("metric", json!(metric)),
                ("baseline", json!("0")),
                ("target", json!("1121")),
                ("target_by", json!("owner")),
            ],
        ),
        row(
            &METRIC,
            &metric,
            &[
                ("id", json!("A10.lab_objects")),
                ("goal", json!("A10")),
                ("quantity", json!("count")),
                ("unit", json!("rows")),
                ("direction", json!("up")),
                ("comparable_on", json!("any")),
                ("source", json!("python3 goals/tools/goals.py measure A10")),
            ],
        ),
        row(
            &LANE,
            "tables",
            &[
                ("id", json!("tables")),
                ("name", json!("Tables")),
                ("prefix", json!("TK")),
                ("repos", json!(["aterm"])),
                ("goals", json!(["A10"])),
                ("rank", json!("3")),
                ("coordinator", agent().to_value()),
            ],
        ),
        row(
            &PACKAGE,
            &dotted,
            &[
                ("id", json!(DOTTED)),
                ("lane", json!("tables")),
                ("title", json!("The first wave, part one")),
                ("state", json!("done")),
            ],
        ),
        row(
            &PACKAGE,
            "TK-10",
            &[
                ("id", json!("TK-10")),
                ("lane", json!("tables")),
                ("goal", json!("A10")),
                ("rank", json!("10")),
                ("title", json!("The lab/tracker tables")),
                (
                    "outcome",
                    json!("The family and its rules on test replicas"),
                ),
                ("repos", json!(["aterm"])),
                ("owns", json!(["crates/aterm-db/src/lab/tracker"])),
                ("depends", json!([dotted])),
                (
                    "blocked_on",
                    json!([{"decision": long}, {"release": release}]),
                ),
                ("request", json!("RQ-41")),
                ("release", json!(release)),
                ("exit", exit()),
                ("size", json!("L")),
                ("state", json!("running")),
                (
                    "claim",
                    json!({"by": agent().to_value(), "on": 20_774, "at": 1_794_873_600_000_u64}),
                ),
                ("split_from", json!(dotted)),
                ("design", json!("docs/DESIGN-lab-tracker-2026-11-14.md")),
            ],
        ),
        row(
            &CHANGE,
            &mapped::change_key("TK-10", "exit", &after),
            &[
                ("package", json!("TK-10")),
                ("column", json!("exit")),
                ("exit", after.clone()),
                ("reason", json!("the test moved under lab::tracker")),
                ("approved_by", agent().to_value()),
            ],
        ),
        row(
            &DECISION,
            &long,
            &[
                ("id", json!(LONG)),
                ("lane", json!("tables")),
                ("decision", json!("One grammar reads a session id")),
                (
                    "reason",
                    json!("two readers disagreed on s-47051b9a207955a92b14@m72"),
                ),
                ("decided_by", agent().to_value()),
                ("decided_on", json!(20_774)),
                ("state", json!("decided")),
            ],
        ),
        row(
            &DECISION,
            "TK-D14",
            &[
                ("id", json!("TK-D14")),
                (
                    "decision",
                    json!("No build registers lab/tracker before the cutover"),
                ),
                ("decided_by", agent().to_value()),
                ("state", json!("standing")),
                ("supersedes", json!(long)),
            ],
        ),
        row(
            &RULING,
            &mapped::ruling_key(20_766, quote),
            &[
                ("date", json!(20_766)),
                ("about", json!("verification")),
                ("quote", json!(quote)),
                ("repos", json!(["aterm"])),
                ("source", json!("docs/OWNER_RULINGS.md")),
                ("grade", json!("carried")),
            ],
        ),
        row(
            &STOP,
            &mapped::stop_key("tables", item),
            &[("lane", json!("tables")), ("item", json!(item))],
        ),
        row(
            &COMPETITION,
            "C7",
            &[
                ("id", json!("C7")),
                ("name", json!("A solver contest")),
                ("kind", json!("ranked")),
                ("catalog", json!(12)),
                ("state", json!("measured")),
                ("url", json!("https://example.org/c7")),
            ],
        ),
        row(
            &PROJECT,
            &mapped::key("agent harnesses"),
            &[
                ("id", json!("agent harnesses")),
                ("size", json!("3 repositories")),
                ("score", json!("2 of 5")),
            ],
        ),
        row(
            &PLAN,
            "north_star",
            &[
                ("id", json!("north_star")),
                ("value", json!("The lab runs itself, and shows its work")),
            ],
        ),
        row(
            &RELEASE,
            &release,
            &[
                ("id", json!("aterm/0.204")),
                ("repo", json!("aterm")),
                ("version", json!("0.204.0")),
                ("state", json!("planned")),
                ("target", json!(20_800)),
            ],
        ),
        row(
            &REQUEST,
            "RQ-41",
            &[
                ("id", json!("RQ-41")),
                ("kind", json!("feature")),
                ("title", json!("Plan the lab from its own tables")),
                ("requested_by", agent().to_value()),
                ("lane", json!("tables")),
                ("goal", json!("A10")),
                ("priority", json!("next")),
                ("state", json!("planned")),
                ("packages", json!(["TK-10"])),
                ("release", json!(release)),
                ("triaged_by", agent().to_value()),
            ],
        ),
        row(
            &VIEW,
            "ready-now",
            &[
                ("id", json!("ready-now")),
                ("of", json!("packages")),
                ("where", json!("state = ready")),
                ("sort", json!("rank")),
            ],
        ),
    ]
}

/// The kinds of the tracker's own findings over `cells`, and the table layer's.
fn found(cells: &Cells) -> (Vec<(&'static str, Vec<String>)>, Vec<check::Finding>) {
    let (own, generic) = checks::tracker(&TRACKER, cells);
    (
        own.into_iter()
            .map(|finding| (finding.kind, finding.pks))
            .collect(),
        generic,
    )
}

/// THE SCHEMA COMPILES AND A CLEAN FIXTURE CHECKS CLEAN (TK-10's exit test). `lab/tracker` is
/// declared on two test replicas and nowhere else: no build registers it for production before
/// the cutover (`att::FAMILIES`, TK-32).
///
/// * The family is its sixteen cells tables and its three logs, each with a face.
/// * Mac 0 writes a row of every cells table ([`fixture`]), each row a record of its own that
///   says who made it and why (design §7, finding 10), and Mac 1 reads them by repair: the
///   tracker's checks ([`checks::tracker`]) find nothing, of their own rules or the table
///   layer's, on either Mac.
/// * A sample of a metric names the machine it was taken on by a link to its registry row
///   (§4.6): its machine uuid, never a label.
/// * The outbox publish runs the table's full rules, the missing-row rule included (§7,
///   finding 4): an intent born with the typed exit of this package, whose lane the table holds,
///   is published and its row checks clean; one that names a lane the table does not hold is
///   held and writes nothing; and one that writes two rows is held, since they would share one
///   record and one reason.
///
/// NEGATIVE CONTROL, observed: with `Compiled::rules` returning after the owner's rules,
/// without `typed::check::refused` (the rules every table keeps, the missing-row rule among
/// them), the intent that names no lane is published and the test fails at `an intent naming
/// a lane the table does not hold is held`. With `att::TRACKER` packing its rows together
/// (`rows_apart: false`), it fails at `each row is a record of its own`: 2 records, not 18.
#[cfg(unix)]
#[test]
fn the_schema_compiles_and_a_clean_fixture_checks_clean() {
    use crate::{
        att::{self, Handle},
        file::{
            self, Class, Computed, Store, Type,
            publish::{self, Answer},
            seam,
        },
        said::Said,
        table::fixture::{Lab, repair_commons_among_first},
        typed::grid,
    };
    use aterm_file::{Draft, Edit, File};
    use aterm_secrets::commons::roster::Role;
    use std::time::Instant;

    /// The tracker's packages as an owner's shared file: a test's own, beside the build's
    /// `lab/packages` (`crate::file::PACKAGES`).
    static FILES: [Type; 1] = [Type {
        name: "test/packages",
        about: "the tracker's packages, on a test replica",
        class: Class::Owner,
        store: Store::Table {
            family: &att::TRACKER,
            collection: &PACKAGE,
        },
        computed: Computed::None,
        handles: &["id"],
    }];
    const NOW: u64 = 1_800_000_000_000;

    // The family: sixteen cells tables and three logs, registered for no production build.
    let kinds = |kind| {
        TRACKER
            .collections
            .iter()
            .filter(|collection| collection.kind == kind)
            .count()
    };
    assert_eq!((kinds(Kind::Cells), kinds(Kind::Log)), (16, 3));
    assert!(
        att::generations().all(|compiled| compiled.table.application != super::APPLICATION),
        "no build registers lab/tracker before the cutover"
    );

    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let trust = lab.bootstrap(0).trust();
    let clocks = [NOW / 1000; 2];
    let repair = || {
        repair_commons_among_first(
            &lab,
            1,
            &[0, 1],
            &clocks,
            crate::catalog::scope(trust).digest(),
        );
        repair_commons_among_first(
            &lab,
            2,
            &[0, 1],
            &clocks,
            att::TRACKER.descriptor(trust).unwrap().scope.digest(),
        );
    };

    // Mac 0 writes the fixture: each row a record of its own, each saying who and why.
    file::fixture::declare(&lab.paths[0], &att::TRACKER);
    let said = Said {
        principal: agent(),
        reason: Some("TK-10: a clean fixture".to_owned()),
        evidence: Vec::new(),
    };
    let rows = fixture();
    let handle = Handle::at(lab.paths[0].clone(), &att::TRACKER).saying(said.clone());
    let ((), applied) = handle.commit_owner(|_| Ok(((), fixture()))).unwrap();
    assert_eq!(
        (applied.records, applied.operations.len()),
        (rows.len(), rows.len()),
        "each row is a record of its own"
    );
    let history = handle.history().unwrap();
    for entry in &history {
        let pks: BTreeSet<&str> = entry
            .changes
            .iter()
            .map(|change| CellKey::parse(&change.key).unwrap().pk)
            .collect();
        assert_eq!(pks.len(), 1, "a record holds one row's changes");
        assert_eq!(entry.said.as_ref(), Some(&said), "and says who and why");
    }

    // A sample names its machine by a link: the machine's uuid, never a label.
    let machine = crate::typed::keys::uuid32(&lab.machine(0));
    let sample = |machine: &str| {
        json!({
            "metric": mapped::key("A10.lab_objects"),
            "value": "1121",
            "at": NOW,
            "machine": machine,
            "class": "m5-max",
        })
        .as_object()
        .unwrap()
        .clone()
    };
    handle
        .append_log(&super::MEASURE, &[sample(&machine)])
        .unwrap();
    assert!(
        handle
            .append_log(&super::MEASURE, &[sample("m72")])
            .is_err(),
        "a label is no link to a machine"
    );

    // Both Macs read every row, and the checks find nothing.
    repair();
    for at in [0, 1] {
        let view = Handle::at(lab.paths[at].clone(), &att::TRACKER)
            .view()
            .unwrap();
        assert!(view.complete && view.refused.is_empty(), "Mac {at}");
        for collection in TRACKER.collections {
            if collection.kind == Kind::Cells {
                assert!(
                    !view.project(collection).rows.is_empty(),
                    "Mac {at} reads a {} row",
                    collection.name
                );
            }
        }
        assert_eq!(found(&view.cells), (Vec::new(), Vec::new()), "Mac {at}");
        let [measured] = &view.logs[super::MEASURE.name].entries[..] else {
            panic!("Mac {at} reads one sample")
        };
        assert_eq!(measured.value["machine"], json!(machine), "Mac {at}");
    }

    // The outbox publish runs the table's full rules.
    let born = |id: &str, lane: &str| Draft {
        born: true,
        revert: None,
        edits: [
            ("id", json!(id)),
            ("lane", json!(lane)),
            ("title", json!("The next package")),
            ("state", json!("ready")),
            ("exit", exit()),
            ("row", json!("live")),
        ]
        .into_iter()
        .map(|(column, value)| {
            let ty = &PACKAGE.column(column).unwrap().ty;
            Edit {
                key: String::new(),
                column: column.to_owned(),
                elem: None,
                text: grid::show(ty, &value),
                value,
                expected: Vec::new(),
                // As a write verb stamps it: the exit's record holds a command, whose long test
                // path the free-text guard would refuse.
                judged: seam::judged(ty),
            }
        })
        .collect(),
    };
    let note = |pk: &str| Edit {
        key: pk.to_owned(),
        column: "note".to_owned(),
        elem: None,
        value: json!("seen"),
        text: "seen".to_owned(),
        expected: Vec::new(),
        judged: seam::judged(&PACKAGE.column("note").unwrap().ty),
    };
    assert_eq!(
        seam::judged(&PACKAGE.column("exit").unwrap().ty),
        aterm_file::outbox::Judged::Secret,
        "a record that holds a command is held to the secret rule, as a command is"
    );
    let outbox = File::at(&file::root(&lab.paths[0]), &FILES[0].file());
    let queue = |draft| {
        outbox
            .write(&seam::owner().saying(said.to_value()).unwrap(), draft, NOW)
            .unwrap()
            .id
            .clone()
    };
    let good = queue(born("TK-11", "tables"));
    let missing = queue(born("TK-12", "no-such-lane"));
    let two = queue(Draft {
        born: false,
        revert: None,
        edits: vec![note("TK-10"), note(&mapped::key(DOTTED))],
    });
    let outcomes = publish::pass_over(
        &lab.paths[0],
        &lab._home.path().join("aterm.toml"),
        &FILES,
        &publish::Pass {
            by: Instant::now() + aterm_spec::wait::MUST_HAPPEN,
            now_ms: NOW,
            journal: None,
        },
    );
    let answer = |id: &str| {
        outcomes
            .iter()
            .find_map(|outcome| outcome.answer(id).cloned())
            .unwrap_or_else(|| panic!("no pass answered {id}: {outcomes:?}"))
    };
    let Answer::Published { op: Some(_) } = answer(&good) else {
        panic!("an intent whose lane the table holds is published: {outcomes:?}")
    };
    let Answer::Held { why } = answer(&missing) else {
        panic!("an intent naming a lane the table does not hold is held: {outcomes:?}")
    };
    assert!(why.contains("no such lane row"), "{why}");
    let Answer::Held { why } = answer(&two) else {
        panic!("an intent that writes two rows is held: {outcomes:?}")
    };
    assert!(why.contains("more than one row"), "{why}");

    // What was published is there on both Macs, and still checks clean; what was held is not.
    repair();
    for at in [0, 1] {
        let view = Handle::at(lab.paths[at].clone(), &att::TRACKER)
            .view()
            .unwrap();
        let packages = view.project(&PACKAGE).rows;
        assert_eq!(packages["TK-11"].get("exit"), Some(&exit()), "Mac {at}");
        assert!(!packages.contains_key("TK-12"), "Mac {at}");
        assert_eq!(packages["TK-10"].get("note"), None, "Mac {at}");
        assert_eq!(found(&view.cells), (Vec::new(), Vec::new()), "Mac {at}");
    }
}

/// CLAIM, NOTE, DONE AND MEASURE ROUND-TRIP ON TWO REPLICAS (TK-12's exit test): the four
/// commands of `aterm goals` ([`super::command`]), each read from the words an agent types, run
/// on two test replicas of `lab/tracker` by two agents, each record saying who made it, why
/// and on what evidence on both.
///
/// * `claim` and `done` are one write of the package's row through the outbox of `lab/packages`.
///   A claim made where no publisher runs answers `queued (not claimed yet)` and the table still
///   says ready; the machine's later pass publishes it, and then both replicas read the package
///   running and held by the agent that claimed it, the record stating that agent and its reason.
/// * A claim of a package the machine reads as running is refused and queues nothing; so is a
///   `done` with no evidence and a `done` of a package nobody claimed; and the write both make
///   (`file::write::set_seen`) refuses a row read at a snapshot revision the machine no longer
///   holds.
/// * `note` and `measure` append this machine's own log entries, at `<its uuid>.<n>`, the note
///   with the evidence it cites and the sample with the machine it was taken on; one about a
///   row the tracker does not hold is refused and appends nothing.
/// * `done` by the other agent, on the other replica, is `published`, and both replicas read the
///   package done on that agent's record and its evidence.
/// * Two agents claim one package at once: the second replica's claim is published, and reaches
///   the first replica after its own claim read the package ready and before it was written.
///   That claim is held, the table keeps the first claim, and nothing is overwritten.
/// * The tracker's checks find nothing on either replica afterwards.
///
/// NEGATIVE CONTROL, observed: with `command::set` passing the write verb no statement
/// (`said: None`), the test fails at `the claim's record says who claimed it and why`. With
/// `command::run` skipping its refusal of a package `lease::Leases::free` does not take, the second agent's claim of the
/// running package is published over the first and the test fails at `a claim of a package
/// another agent holds is refused`.
#[cfg(unix)]
#[test]
fn claim_note_done_measure_round_trip_on_two_replicas() {
    use super::command::{self, Ask, Env};
    use super::frozen;
    use crate::{
        att::{self, Handle},
        file::{
            self, materialize,
            publish::{self, Answer},
            write::Refusal,
        },
        said::Said,
        table::fixture::{Lab, repair_commons_among_first},
        typed::values::Evidence,
    };
    use aterm_secrets::commons::roster::Role;
    use std::{
        rc::Rc,
        time::{Duration, Instant},
    };

    const NOW: u64 = 1_800_000_000_000;
    const COMMIT: &str = "commit:aterm@9c37b7a100f";
    const WAIT: Duration = aterm_spec::wait::MUST_HAPPEN;

    /// Both replicas exchange the catalog and the tracker.
    fn repair(lab: &Lab) {
        let trust = lab.bootstrap(0).trust();
        let clocks = [NOW / 1000; 2];
        for (rounds, table) in [
            (1, crate::catalog::scope(trust).digest()),
            (2, att::TRACKER.descriptor(trust).unwrap().scope.digest()),
        ] {
            repair_commons_among_first(lab, rounds, &[0, 1], &clocks, table);
        }
    }

    assert!(
        !command::live(),
        "no build registers lab/tracker before the cutover, so the front door writes nothing"
    );
    let lab = Rc::new(Lab::new(&[Role::Writer, Role::Writer]));
    let config = lab._home.path().join("aterm.toml");
    let packages = file::PACKAGES.file();

    // Mac 0 writes the fixture and two ready packages; both Macs hold the family.
    let ready = |id: &str| {
        row(
            &PACKAGE,
            id,
            &[
                ("id", json!(id)),
                ("lane", json!("tables")),
                ("title", json!("The commands, through the outbox")),
                ("exit", exit()),
                ("state", json!("ready")),
            ],
        )
    };
    file::fixture::declare(&lab.paths[0], &att::TRACKER);
    repair(&lab);
    file::fixture::declare(&lab.paths[1], &att::TRACKER);
    Handle::at(lab.paths[0].clone(), &att::TRACKER)
        .saying(Said::by(agent()))
        .commit_owner(|_| {
            let mut rows = fixture();
            rows.extend([ready("TK-12"), ready("TK-26")]);
            Ok(((), rows))
        })
        .unwrap();
    repair(&lab);

    // Two agents, as aterm reads each from its process's lineage.
    let who = |session: &str, program: &str| Principal {
        session: Some(session.to_owned()),
        program: Some(program.to_owned()),
        ..Principal::default()
    };
    let (first, second) = (who("ta92es4hh3q", "claude"), who("t1k7ct7dh0b", "codex"));
    // An agent as its claim names it: with the machine its session runs on (TK-14).
    let on = |by: &Principal, mac: usize| Principal {
        machine: Some(crate::typed::keys::uuid32(&lab.machine(mac))),
        ..by.clone()
    };
    let goals = |mac: usize, by: &Principal, attempt: Duration, words: &[&str]| {
        let words: Vec<String> = words.iter().map(|word| (*word).to_owned()).collect();
        let ask = Ask::parse(&words)?;
        command::run(
            &lab.paths[mac],
            &config,
            &ask,
            &Env {
                principal: by,
                by: "program",
                session: by.session.as_deref(),
                now_ms: NOW,
                attempt,
                journal: None,
            },
        )
    };
    let view = |mac: usize| {
        Handle::at(lab.paths[mac].clone(), &att::TRACKER)
            .view()
            .unwrap()
    };
    let package = |mac: usize, pk: &str, column: &str| {
        view(mac).project(&PACKAGE).rows[pk].get(column).cloned()
    };
    // The statement of the record `op` (lower-case hex), as Mac `mac` holds it.
    let said_of = |mac: usize, op: &str| {
        Handle::at(lab.paths[mac].clone(), &att::TRACKER)
            .history()
            .unwrap()
            .into_iter()
            .find(|entry| aterm_codec::hex::encode_lower(&entry.operation) == op)
            .unwrap_or_else(|| panic!("Mac {mac} holds the record {op}"))
            .said
    };
    // The word after `prefix` in an answer: the operation of `published <op>: …`, the intent of
    // `queued (…) <id>: …`, the key of `published <key>: …`.
    let after = |text: &str, prefix: &str| -> String {
        text.strip_prefix(prefix)
            .and_then(|rest| rest.split(':').next())
            .unwrap_or_else(|| panic!("an answer that starts `{prefix}`: {text}"))
            .to_owned()
    };

    // QUEUED (NOT CLAIMED YET). The machine's pass has materialized `lab/packages`; a claim made
    // where no publisher runs waits in the outbox, and the table still says ready.
    let _ = materialize::pass(
        &lab.paths[0],
        &config,
        Some(&packages),
        &materialize::Pass {
            now_ms: NOW,
            ..materialize::Pass::now()
        },
    );
    let claim = ["claim", "TK-12", "--reason", "TK-12: the commands"];
    let (text, code) = goals(0, &first, Duration::ZERO, &claim).unwrap();
    let queued = after(&text, "queued (not claimed yet) ");
    assert_eq!(
        (text.as_str(), code),
        (
            format!("queued (not claimed yet) {queued}: a later pass publishes it\n").as_str(),
            0
        )
    );
    assert_eq!(
        (package(0, "TK-12", "state"), package(0, "TK-12", "claim")),
        (Some(json!("ready")), None),
        "a queued claim has claimed nothing"
    );

    // PUBLISHED. The machine's later pass signs it: the package is running and held by the
    // agent that claimed it, on both Macs, and the record says who and why.
    let outcomes = publish::pass(
        &lab.paths[0],
        &config,
        Some(&packages),
        &publish::Pass {
            by: Instant::now() + WAIT,
            now_ms: NOW,
            journal: None,
        },
    );
    let Some(Answer::Published { op: Some(op) }) = outcomes
        .iter()
        .find_map(|outcome| outcome.answer(&queued).cloned())
    else {
        panic!("the later pass publishes the queued claim: {outcomes:?}")
    };
    repair(&lab);
    let claimed = Said {
        principal: first.clone(),
        reason: Some("TK-12: the commands".to_owned()),
        evidence: Vec::new(),
    };
    for mac in [0, 1] {
        assert_eq!(
            package(mac, "TK-12", "state"),
            Some(json!("running")),
            "Mac {mac}"
        );
        assert_eq!(
            package(mac, "TK-12", "claim"),
            Some(json!({
                "by": on(&first, 0).to_value(),
                "on": NOW / 86_400_000,
                "at": NOW,
                "exit": frozen::digest(&exit()),
            })),
            "Mac {mac}: the claim names its holder, the machine its session runs on and the \
             digest of the exit it claimed"
        );
        assert_eq!(
            said_of(mac, &op),
            Some(claimed.clone()),
            "Mac {mac}: the claim's record says who claimed it and why"
        );
    }

    // A claim of a package another agent holds is refused on the machine that reads it running,
    // and queues nothing.
    let refused = goals(1, &second, WAIT, &["claim", "TK-12"]);
    assert_eq!(
        refused,
        Err(Refusal::Problem(
            "TK-12 is running, held by claude ta92es4hh3q: a claim takes a ready package"
                .to_owned()
        )),
        "a claim of a package another agent holds is refused"
    );
    let waiting = |mac: usize| {
        let outbox = aterm_file::File::at(&file::root(&lab.paths[mac]), &packages).outbox();
        (outbox.queued.len(), outbox.held.len())
    };
    assert_eq!(waiting(1), (0, 0));
    // Nor is a write queued against a snapshot that moved after its command read the row: the
    // command decided on a row the write would no longer be compared with.
    let seen = aterm_file::File::at(&file::root(&lab.paths[1]), &packages)
        .read()
        .status
        .rev();
    let stale: Vec<String> = ["set", "lab/packages", "TK-26", "state=running"]
        .map(str::to_owned)
        .into();
    let env = file::write::Env {
        by: "program",
        session: None,
        now_ms: NOW,
        attempt: WAIT,
        journal: None,
        json: false,
        stdin: None,
        said: None,
        editor: &|_, _, _| Err("no editor".to_owned()),
        say: &|_| {},
    };
    #[expect(
        clippy::disallowed_methods,
        reason = "the test of the refusal `aterm goals` relies on"
    )]
    let moved = file::write::set_seen(&lab.paths[1], &config, &stale, seen + 1, &env);
    assert!(
        matches!(&moved, Err(Refusal::Problem(why)) if why.contains("snapshot moved")),
        "a write read at another revision is refused"
    );
    assert_eq!(waiting(1), (0, 0));

    // NOTE AND MEASURE: Mac 1's own log entries, each at its own key, saying who and on what.
    let evidence = vec![Evidence::parse(COMMIT).unwrap()];
    let uuid = crate::typed::keys::uuid32(&lab.machine(1));
    let saying = "--: the exit test passes on both replicas";
    let noted = goals(
        1,
        &second,
        WAIT,
        &["note", "TK-12", "--evidence", COMMIT, "--", saying],
    );
    assert_eq!(
        noted,
        Ok((format!("published {uuid}.1: a note on package TK-12\n"), 0))
    );
    assert_eq!(
        goals(1, &second, WAIT, &["note", "TK-99", "--", saying]),
        Err(Refusal::Problem("no package TK-99".to_owned()))
    );
    let source = "aterm db table ls";
    let measured = goals(
        1,
        &second,
        WAIT,
        &[
            "measure",
            "A10.lab_objects",
            "1122",
            "--source",
            source,
            "--commit",
            "9c37b7a100f",
        ],
    );
    assert_eq!(
        measured,
        Ok((
            format!("published {uuid}.1: a sample of A10.lab_objects\n"),
            0
        ))
    );
    assert_eq!(
        goals(1, &second, WAIT, &["measure", "A10.no_such", "1"]),
        Err(Refusal::Problem("no metric A10.no_such".to_owned()))
    );

    // DONE cites its evidence and ends a running package: the other agent's, on the other Mac.
    let Err(Refusal::Usage(why)) = goals(1, &second, WAIT, &["done", "TK-12"]) else {
        panic!("a done with no evidence is refused")
    };
    assert!(why.contains("done cites what shows it"), "{why}");
    assert_eq!(
        goals(1, &second, WAIT, &["done", "TK-26", "--evidence", COMMIT]),
        Err(Refusal::Problem(
            "TK-26 is ready: done ends a running package".to_owned()
        ))
    );
    let done = [
        "done",
        "TK-12",
        "--evidence",
        COMMIT,
        "--reason",
        "it passes",
    ];
    let (text, code) = goals(1, &second, WAIT, &done).unwrap();
    let finished = after(&text, "published ");
    assert_eq!(
        (text.as_str(), code),
        (format!("published {finished}: TK-12 is done\n").as_str(), 0)
    );

    // HELD. Mac 1's agent claims TK-26; its record reaches Mac 0 after Mac 0's own claim read
    // the package ready and before that claim is written. Nothing is overwritten: both wrote
    // the state `running`, and the holder the table names is no longer the one Mac 0 read.
    let (text, _) = goals(1, &second, WAIT, &["claim", "TK-26"]).unwrap();
    let theirs = after(&text, "published ");
    assert_eq!(
        text,
        format!("published {theirs}: TK-26 is running, held by codex t1k7ct7dh0b\n")
    );
    // Mac 0's claim is read and queued (the claim reads this machine's snapshot and its store,
    // so a record that arrived before it read would refuse it); the record arrives after that
    // read and before Mac 0's publisher compares the claim with the table.
    let (text, code) = goals(0, &first, Duration::ZERO, &["claim", "TK-26"]).unwrap();
    let held = after(&text, "queued (not claimed yet) ");
    assert_eq!(code, 0, "{text}");
    repair(&lab);
    let outcomes = publish::pass(
        &lab.paths[0],
        &config,
        Some(&packages),
        &publish::Pass {
            by: Instant::now() + WAIT,
            now_ms: NOW,
            journal: None,
        },
    );
    assert_eq!(
        outcomes
            .iter()
            .find_map(|outcome| outcome.answer(&held).cloned()),
        Some(Answer::Held {
            why: format!("{}: TK-26.claim", publish::MOVED)
        }),
        "a claim that lost is held"
    );
    assert_eq!(
        waiting(0),
        (0, 1),
        "the held claim is kept, and signed nothing"
    );

    // Both Macs read the same tracker: every record says who made it and on what evidence, and
    // the checks find nothing.
    repair(&lab);
    let finishing = Said {
        principal: second.clone(),
        reason: Some("it passes".to_owned()),
        evidence: evidence.clone(),
    };
    for mac in [0, 1] {
        let view = view(mac);
        assert!(view.complete && view.refused.is_empty(), "Mac {mac}");
        let rows = view.project(&PACKAGE).rows;
        assert_eq!(
            rows["TK-12"].get("state"),
            Some(&json!("done")),
            "Mac {mac}"
        );
        assert_eq!(
            said_of(mac, &finished),
            Some(finishing.clone()),
            "Mac {mac}: the done's record says who finished it, why and on what evidence"
        );
        assert_eq!(
            rows["TK-26"].get("claim").map(|claim| &claim["by"]),
            Some(&on(&second, 1).to_value()),
            "Mac {mac}: the first claim stands"
        );
        let [note] = &view.logs[super::NOTE.name].entries[..] else {
            panic!("Mac {mac} reads one note")
        };
        assert_eq!(
            (note.machine, &note.value),
            (
                lab.machine(1),
                &json!({
                    "n": 1,
                    "r": 1,
                    "on": "package",
                    "about": "TK-12",
                    "text": saying,
                    "evidence": [evidence[0].to_value()],
                    "at": NOW,
                })
            ),
            "Mac {mac}"
        );
        let [sample] = &view.logs[super::MEASURE.name].entries[..] else {
            panic!("Mac {mac} reads one sample")
        };
        assert_eq!(
            sample.value,
            json!({
                "n": 1,
                "r": 1,
                "metric": mapped::key("A10.lab_objects"),
                "value": "1122",
                "at": NOW,
                "source": source,
                "commit": "9c37b7a100f",
                "machine": uuid,
            }),
            "Mac {mac}"
        );
        for (entry, evidence) in [(note, evidence.clone()), (sample, Vec::new())] {
            assert_eq!(
                said_of(mac, &aterm_codec::hex::encode_lower(&entry.operation)),
                Some(Said {
                    principal: second.clone(),
                    reason: None,
                    evidence,
                }),
                "Mac {mac}: a log entry's record says who appended it"
            );
        }
        assert_eq!(found(&view.cells), (Vec::new(), Vec::new()), "Mac {mac}");
    }
}

/// A ROW IS KEYED BY ITS NAME, OR BY ITS NAME'S MAPPED FORM (design §7, finding 7). A word in
/// the key grammar is its own key; any other (a dot, more than 26 bytes, a space, a slash) is
/// keyed by a head of the word and a digest of all of it: a key every Mac makes alike, in the
/// grammar, that no other word shares. Every id of publication 384b3a9f's tracker is so keyed,
/// 37 packages, 9 decisions and 1 project by their mapped form. A row whose key is not the one
/// its name makes is a finding, and the owner's write of one is refused.
///
/// The corpus is withheld from the public snapshot with `publish/`, so a tree with neither
/// checks the named ids alone; a checkout that has the manifest and not the corpus fails.
///
/// NEGATIVE CONTROL, observed: with `mapped::key` leaving the digest off (the head and `_`
/// alone), `two ids that share a head keep their own rows` fails.
#[test]
fn a_row_is_keyed_by_its_name_or_its_mapped_form() {
    for plain in ["TK-10", "tables", "A11", "north_star", "C7"] {
        assert_eq!(mapped::key(plain), plain, "a key is its own");
    }
    let mapped = |id: &str| {
        let key = mapped::key(id);
        assert!(pk_grammar(&key), "{id}: {key}");
        assert!(guard::word(&key).is_ok(), "{id}: {key} reads as a secret");
        assert_eq!(
            key,
            mapped::key(id),
            "{id}: one key however often it is made"
        );
        key
    };
    for odd in [
        DOTTED,
        "TI-D.con_param",
        LONG,
        "agent harnesses",
        "aterm/0.204",
        "A10.lab_objects",
    ] {
        assert_ne!(mapped(odd), odd);
    }
    assert!(mapped(DOTTED).starts_with("TI-W0-1_"), "{}", mapped(DOTTED));
    assert_ne!(
        mapped("S8-ONE-AUTHORITATIVE-SID-GRAMMAR-TEST"),
        mapped("S8-ONE-AUTHORITATIVE-SID-GRAMMAR-RULE"),
        "two ids that share a head keep their own rows"
    );
    assert_ne!(mapped("TI-W0.1"), mapped("TI-W0-1"));
    // A word the key guard reads as a credential is no key as it stands, nor is a head of one.
    for token in [
        "sk-1",
        "sk-learn bench",
        "xai-grok.4 integration",
        "ghp_tokens.rotation",
    ] {
        assert_ne!(mapped(token), token);
    }
    assert!(mapped("sk-learn bench").starts_with("id_"));

    let root = aterm_tempfile::manifest_dir();
    let dir = root.join("testdata/publication-384b3a9f");
    if dir.exists() {
        for (file, by_mapped_form) in [
            ("packages", 37),
            ("decisions", 9),
            ("projects", 1),
            ("goals", 0),
            ("lanes", 0),
            ("groups", 0),
            ("competitions", 0),
            ("plan", 0),
        ] {
            let text = std::fs::read_to_string(dir.join(format!("{file}.tsv")))
                .unwrap_or_else(|e| panic!("{file}: the corpus is unreadable: {e}"));
            let ids: Vec<&str> = text
                .lines()
                .skip(1)
                .filter_map(|line| line.split('\t').next())
                .collect();
            let keys: BTreeSet<String> = ids.iter().map(|id| mapped(id)).collect();
            assert_eq!(keys.len(), ids.len(), "{file}: each id its own key");
            assert_eq!(
                ids.iter().filter(|id| mapped::key(id) != **id).count(),
                by_mapped_form,
                "{file}"
            );
        }
    } else {
        assert!(
            !root.join("../../publish/manifest.txt").exists(),
            "a checkout with publish/ holds the corpus; only the export withholds it"
        );
    }

    // A row under another key than its name makes: a finding, and refused where it is written.
    let stray = vec![
        row(
            &LANE,
            "tables",
            &[
                ("id", json!("tables")),
                ("name", json!("Tables")),
                ("prefix", json!("TK")),
            ],
        ),
        row(
            &PACKAGE,
            "TI-W0-1",
            &[
                ("id", json!(DOTTED)),
                ("lane", json!("tables")),
                ("title", json!("Keyed by hand")),
                ("state", json!("ready")),
            ],
        ),
    ];
    assert_eq!(
        found(&landed(&stray)).0,
        [("key-mismatch", vec!["TI-W0-1".to_owned()])]
    );
    let refused = checks::guard(&mut stray.clone(), &Cells::new())
        .unwrap()
        .expect("refused");
    assert_eq!(
        (
            refused.collection,
            refused.pk.as_str(),
            refused.column.as_str()
        ),
        ("package", "TI-W0-1", "id")
    );
    // A birth makes the key itself, from what the row says.
    let values = [("id".to_owned(), json!(DOTTED))].into_iter().collect();
    assert_eq!(
        mapped::birth(&PACKAGE, &values, &Cells::new()).unwrap(),
        mapped::key(DOTTED)
    );
    assert!(mapped::birth(&PACKAGE, &[].into_iter().collect(), &Cells::new()).is_err());
}

/// EVERY STATE THE DATA HOLDS IS MAPPED, WITH A FINDING EACH (design §7, finding 7). The lab
/// tracker's `waiting` is no package state, its `active` no decision state, and its
/// `decided_by` free text: each maps to what the tracker's types hold, the row keeps what was
/// said, and the checks report the row until someone settles it. The tracker's own words map
/// to themselves and are no finding.
///
/// NEGATIVE CONTROL, observed: with `checks::tracker` not reporting `state-mapped`, the
/// findings lack the package and the decision that hold `state_said`.
#[test]
fn every_state_the_data_holds_is_mapped_with_a_finding_each() {
    for word in ["ready", "running", "blocked", "done", "dropped"] {
        let state = mapped::package_state(word).expect(word);
        assert_eq!((state.state, state.said), (word, None));
    }
    for word in ["open", "decided", "standing", "superseded"] {
        let state = mapped::decision_state(word).expect(word);
        assert_eq!((state.state, state.said), (word, None));
    }
    let waiting = mapped::package_state("waiting").unwrap();
    assert_eq!((waiting.state, waiting.said), ("ready", Some("waiting")));
    let active = mapped::decision_state("active").unwrap();
    assert_eq!((active.state, active.said), ("decided", Some("active")));
    assert_eq!(mapped::package_state("active"), None);
    assert_eq!(mapped::decision_state("in force"), None);
    let by = mapped::decided_by("aterm-perf-s8");
    assert_eq!(by, Principal::said("aterm-perf-s8"));

    let rows = vec![
        row(
            &LANE,
            "tables",
            &[
                ("id", json!("tables")),
                ("name", json!("Tables")),
                ("prefix", json!("TK")),
            ],
        ),
        row(
            &PACKAGE,
            "TK-90",
            &[
                ("id", json!("TK-90")),
                ("lane", json!("tables")),
                ("title", json!("Held back by its holder")),
                ("state", json!(waiting.state)),
                ("state_said", json!(waiting.said)),
            ],
        ),
        row(
            &PACKAGE,
            "TK-91",
            &[
                ("id", json!("TK-91")),
                ("lane", json!("tables")),
                ("title", json!("Ready as the tracker says it")),
                ("state", json!("ready")),
            ],
        ),
        row(
            &DECISION,
            "TK-D1",
            &[
                ("id", json!("TK-D1")),
                ("decision", json!("In force, in the lab tracker's word")),
                ("decided_by", agent().to_value()),
                ("state", json!(active.state)),
                ("state_said", json!(active.said)),
            ],
        ),
        row(
            &DECISION,
            "TK-D2",
            &[
                ("id", json!("TK-D2")),
                ("decision", json!("Decided by a name in words")),
                ("decided_by", by.to_value()),
                ("state", json!("decided")),
            ],
        ),
    ];
    let (own, generic) = found(&landed(&rows));
    assert_eq!(generic, Vec::new());
    assert_eq!(
        own,
        [
            ("decided-by-said", vec!["TK-D2".to_owned()]),
            ("state-mapped", vec!["TK-D1".to_owned()]),
            ("state-mapped", vec!["TK-90".to_owned()]),
        ]
    );
    for (kind, _) in &own {
        assert_ne!(check::said(kind), *kind, "{kind} is said in a sentence");
        assert_eq!(
            check::headed(kind),
            checks::heading(kind).expect("a tracker finding has a heading"),
            "{kind} has a heading"
        );
    }
}

/// A LANE PREFIX IS HELD BY ONE ROW, AND A DEPENDENCY CYCLE IS A FINDING (design §3, §7
/// finding 9). Two live lanes that state one prefix are reported by the table layer's unique
/// rule, which the lane table declares. Packages that depend on one another in a circle are
/// reported together, one finding a circle, and the owner's write that would close a circle on
/// its own view is refused; a package that depends on a done one, or a chain with no way back,
/// is no finding.
///
/// NEGATIVE CONTROL, observed: with `checks::circles` reporting no component, `a circle of
/// three is one finding` fails; with `LANE.unique` naming only `id`, the two lanes are no
/// finding and it fails at `two lanes that state one prefix`.
#[test]
fn a_lane_prefix_is_held_by_one_row_and_a_dependency_cycle_is_a_finding() {
    let lane = |id: &str, prefix: &str| {
        row(
            &LANE,
            id,
            &[
                ("id", json!(id)),
                ("name", json!(id)),
                ("prefix", json!(prefix)),
            ],
        )
    };
    let package = |id: &str, depends: &[&str]| {
        row(
            &PACKAGE,
            id,
            &[
                ("id", json!(id)),
                ("lane", json!("tables")),
                ("title", json!("A package")),
                ("state", json!("ready")),
                ("depends", json!(depends)),
            ],
        )
    };

    // One prefix, two lanes.
    let (own, generic) = found(&landed(&[lane("tables", "TK"), lane("tracker", "TK")]));
    assert_eq!(own, Vec::new());
    let [
        check::Finding::Unique {
            collection, pks, ..
        },
    ] = &generic[..]
    else {
        panic!("two lanes that state one prefix: {generic:?}")
    };
    assert_eq!(
        (collection.as_str(), &pks[..]),
        ("lane", &["tables".to_owned(), "tracker".to_owned()][..])
    );
    assert_eq!(
        found(&landed(&[lane("tables", "TK"), lane("tracker", "TR")])),
        (Vec::new(), Vec::new())
    );

    // A chain is no circle; a circle of three is one finding, and so is a package on itself.
    let chain = vec![
        lane("tables", "TK"),
        package("TK-1", &[]),
        package("TK-2", &["TK-1"]),
        package("TK-3", &["TK-2", "TK-1"]),
    ];
    assert_eq!(found(&landed(&chain)), (Vec::new(), Vec::new()));
    let mut circle = chain.clone();
    circle.push(vec![
        cell::member_change(&PACKAGE, "TK-1", "depends", "TK-3", true, |x| {
            CellValue::owner(1, x)
        })
        .unwrap(),
    ]);
    circle.push(package("TK-4", &["TK-4"]));
    circle.push(package("TK-5", &["TK-3"]));
    assert_eq!(
        found(&landed(&circle)).0,
        [
            (
                "depends-cycle",
                vec!["TK-1".to_owned(), "TK-2".to_owned(), "TK-3".to_owned()]
            ),
            ("depends-cycle", vec!["TK-4".to_owned()]),
        ],
        "a circle of three is one finding"
    );

    // The write that would close the circle is refused on the writer's own view; one that adds
    // an edge with no way back is not.
    let held = landed(&chain);
    let closing = |from: &str, to: &str| {
        vec![vec![
            cell::member_change(&PACKAGE, from, "depends", to, true, |x| {
                CellValue::owner(1, x)
            })
            .unwrap(),
        ]]
    };
    let refused = checks::guard(&mut closing("TK-1", "TK-3"), &held)
        .unwrap()
        .expect("a write that closes a circle is refused");
    assert_eq!(
        (
            refused.collection,
            refused.pk.as_str(),
            refused.column.as_str()
        ),
        ("package", "TK-1", "depends")
    );
    assert!(
        checks::guard(&mut closing("TK-2", "TK-2"), &held)
            .unwrap()
            .is_some(),
        "a package does not depend on itself"
    );
    assert_eq!(
        checks::guard(&mut closing("TK-3", "TK-1"), &held).unwrap(),
        None
    );
    // Nor can a circle be closed in two writes, the edge while its package is retired and then
    // the revival: a package made live adds every dependency it holds.
    let mut asleep = chain.clone();
    asleep.push(vec![put(&PACKAGE, "TK-1", "row", json!("retired"))]);
    asleep.extend(closing("TK-1", "TK-3"));
    let asleep = landed(&asleep);
    assert_eq!(
        found(&asleep).0,
        Vec::new(),
        "a retired package closes no circle"
    );
    let revive = |pk: &str| vec![vec![put(&PACKAGE, pk, "row", json!("live"))]];
    let refused = checks::guard(&mut revive("TK-1"), &asleep)
        .unwrap()
        .expect("a revival that closes a circle is refused");
    assert_eq!(
        (refused.pk.as_str(), refused.column.as_str()),
        ("TK-1", "depends")
    );
    assert_eq!(checks::guard(&mut revive("TK-2"), &asleep).unwrap(), None);
    assert_eq!(
        checks::guard(&mut chain.clone(), &Cells::new()).unwrap(),
        None
    );
}

/// CONCURRENT DONE AND BLOCKED HEAL TO DONE WITH A FINDING (TK-13's exit test; design §4.1, §7
/// finding 4). Two Macs hold three packages and each changes them without hearing the other;
/// once they have, both read every package's state alike by the state rule ([`state`]), which
/// judges each change by the state it saw and reads no clock.
///
/// * `TK-13` is running, held by one agent. Another finishes it on Mac 0 with a run as its
///   evidence while a third blocks it on Mac 1 with a reason: each change is one the running
///   state allows, and each Mac reads its own until they meet. Then both read `done`; the
///   block is shown as what lost, with who made it, and is a finding (`state-outranked`) in
///   place of the table layer's bare `conflict`; and the done is credited to the principal of
///   the record that wrote it, not to the holder of the claim.
/// * `TK-14` is ready. Mac 0 says `done` of it, which ready does not allow, while Mac 1 claims
///   it. The cell read alone shows `done` first; the rule reads `running`, by the claim, and
///   the done is a finding (`state-not-allowed`): two changes made at once leave no state
///   neither Mac could have reached.
/// * `TK-15` is running and Mac 0 says `done` of it citing nothing: it stays running, with the
///   same finding, though its cell holds the one word `done`.
/// * Mac 1, having heard all of it, says `done` of `TK-13` again: the finding is settled, and
///   the credit stays with the record that wrote done first.
/// * Then, on one Mac, each change the rule names by what its record carries: a block that
///   gives no reason does not count; a release to ready does, and settles the finding before
///   it; a `running` that writes no claim does not count; and no write of a state leaves
///   `done`, whatever its reason.
///
/// NEGATIVE CONTROL, observed: with `state::Write::allowed` allowing every change, `TK-14`
/// reads `done` and the test fails at `Mac 0: a done that saw ready does not count`; with
/// `state::standing` settling by the last word of the order, not the first, it fails at `Mac 0:
/// concurrent done and blocked heal to done`.
#[cfg(unix)]
#[test]
fn concurrent_done_and_blocked_heal_to_done_with_a_finding() {
    use super::state::{self, Settled};
    use crate::{
        att::{self, Handle},
        file,
        said::Said,
        table::fixture::{Lab, repair_commons_among_first},
        typed::values::Evidence,
    };
    use astream_wire::sync::Digest;
    use aterm_secrets::commons::roster::Role;

    const NOW: u64 = 1_800_000_000_000;
    const EXIT: &str = "targo --unverified test -p aterm-db --lib \
         lab::tracker::tests::concurrent_done_and_blocked_heal_to_done_with_a_finding";

    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let trust = lab.bootstrap(0).trust();
    let clocks = [NOW / 1000; 2];
    let repair = || {
        repair_commons_among_first(
            &lab,
            1,
            &[0, 1],
            &clocks,
            crate::catalog::scope(trust).digest(),
        );
        repair_commons_among_first(
            &lab,
            2,
            &[0, 1],
            &clocks,
            att::TRACKER.descriptor(trust).unwrap().scope.digest(),
        );
    };
    let at = |mac: usize| Handle::at(lab.paths[mac].clone(), &att::TRACKER);
    // One record on `mac`, saying `said`: its operation.
    let write = |mac: usize, said: &Said, changes: Vec<Change>| -> Digest {
        let ((), applied) = at(mac)
            .saying(said.clone())
            .commit_owner(|_| Ok(((), vec![changes])))
            .unwrap();
        assert_eq!(applied.refused, None, "Mac {mac}");
        applied.operations[0]
    };
    let who = |session: &str| Principal {
        session: Some(session.to_owned()),
        program: Some("claude".to_owned()),
        ..Principal::default()
    };
    let saying = |by: &Principal, reason: Option<&str>, evidence: Vec<Evidence>| Said {
        principal: by.clone(),
        reason: reason.map(str::to_owned),
        evidence,
    };
    let (holder, finisher, blocker) = (agent(), who("s-finisher"), who("s-blocker"));
    let ran = Evidence::Run {
        command: EXIT.to_owned(),
        exit: 0,
        output: Some("test result: ok. 1 passed".to_owned()),
        repo: Some("aterm".to_owned()),
        commit: None,
    };
    let claim = |by: &Principal| json!({"by": by.to_value(), "on": 20_774, "at": NOW});
    let state_of = |pk: &str, state: &str| put(&PACKAGE, pk, "state", json!(state));

    // Mac 0 writes the packages, and Mac 1 hears them.
    file::fixture::declare(&lab.paths[0], &att::TRACKER);
    file::fixture::declare(&lab.paths[1], &att::TRACKER);
    let born = saying(&holder, Some("TK-13: three packages"), Vec::new());
    write(
        0,
        &born,
        row(
            &LANE,
            "tables",
            &[
                ("id", json!("tables")),
                ("name", json!("Tables")),
                ("prefix", json!("TK")),
            ],
        ),
    );
    for (pk, state, held) in [
        ("TK-13", "running", true),
        ("TK-14", "ready", false),
        ("TK-15", "running", true),
    ] {
        let mut cells = vec![
            ("id", json!(pk)),
            ("lane", json!("tables")),
            ("title", json!("A package")),
            ("state", json!(state)),
        ];
        if held {
            cells.push(("claim", claim(&holder)));
        }
        write(0, &born, row(&PACKAGE, pk, &cells));
    }
    repair();

    // Each Mac changes them without hearing the other.
    let done = write(
        0,
        &saying(&finisher, None, vec![ran.clone()]),
        vec![state_of("TK-13", "done")],
    );
    let blocked = write(
        1,
        &saying(&blocker, Some("waits on the version 2 write"), Vec::new()),
        vec![state_of("TK-13", "blocked")],
    );
    let early = write(
        0,
        &saying(&finisher, None, vec![ran]),
        vec![state_of("TK-14", "done")],
    );
    let claimed = write(
        1,
        &saying(&blocker, None, Vec::new()),
        vec![
            state_of("TK-14", "running"),
            put(&PACKAGE, "TK-14", "claim", claim(&blocker)),
        ],
    );
    let unproved = write(
        0,
        &saying(&finisher, Some("it is finished"), Vec::new()),
        vec![state_of("TK-15", "done")],
    );
    let read = |mac: usize| state::read(&at(mac)).unwrap();
    let operations = |writes: &[state::Write]| -> Vec<Digest> {
        writes.iter().map(|write| write.operation).collect()
    };
    // Until they meet, each reads its own change, which the state it saw allows.
    for (mac, own) in [(0, "done"), (1, "blocked")] {
        let (_, states) = read(mac);
        let Settled {
            state,
            lost,
            refused,
            ..
        } = &states.packages["TK-13"];
        assert_eq!(*state, Some(own), "Mac {mac} reads its own change");
        assert!(lost.is_empty() && refused.is_empty(), "Mac {mac}");
    }

    repair();
    for mac in [0, 1] {
        let (view, states) = read(mac);
        assert!(view.complete && view.refused.is_empty(), "Mac {mac}");

        // Done and blocked heal to done; the block is shown; the done is its writer's.
        let healed = &states.packages["TK-13"];
        assert_eq!(
            healed.state,
            Some("done"),
            "Mac {mac}: concurrent done and blocked heal to done"
        );
        assert_eq!(operations(&healed.by), [done], "Mac {mac}");
        assert_eq!(operations(&healed.lost), [blocked], "Mac {mac}");
        assert_eq!(
            (
                healed.lost[0].state,
                healed.lost[0].said.as_ref().map(|said| &said.principal)
            ),
            (Some("blocked"), Some(&blocker)),
            "Mac {mac}: the one that lost is shown, with who made it"
        );
        assert!(healed.refused.is_empty(), "Mac {mac}");
        assert_eq!(
            healed.credit(),
            [&finisher],
            "Mac {mac}: credit goes to the record that wrote done"
        );
        let mut packages = view.project(&PACKAGE);
        assert_eq!(
            packages.rows["TK-13"].get("claim"),
            Some(&claim(&holder)),
            "Mac {mac}: the claim is still its holder's, who is not credited"
        );

        // A done that saw ready does not count, whatever it cites: the claim made at once
        // stands, though the cell read alone shows done first.
        let raced = &states.packages["TK-14"];
        assert_eq!(
            packages.rows["TK-14"].get("state"),
            Some(&json!("done")),
            "Mac {mac}: the cell alone"
        );
        assert_eq!(
            raced.state,
            Some("running"),
            "Mac {mac}: a done that saw ready does not count"
        );
        assert_eq!(
            (
                operations(&raced.by),
                operations(&raced.refused),
                raced.lost.len()
            ),
            (vec![claimed], vec![early], 0),
            "Mac {mac}"
        );
        assert!(raced.credit().is_empty(), "Mac {mac}: no done, no credit");

        // A done that cites nothing does not count: the package reads as it did before.
        let bare = &states.packages["TK-15"];
        assert_eq!(
            (bare.state, operations(&bare.refused), bare.lost.len()),
            (Some("running"), vec![unproved], 0),
            "Mac {mac}: a done with no run and no evidence does not count"
        );

        // Every reader of the tracker's rows takes the settled state.
        states.over(&mut packages);
        for (pk, settled) in [
            ("TK-13", "done"),
            ("TK-14", "running"),
            ("TK-15", "running"),
        ] {
            assert_eq!(
                packages.rows[pk].get("state"),
                Some(&json!(settled)),
                "Mac {mac}: {pk}"
            );
        }

        // Each is a finding of the rule's own, in place of the table layer's bare conflict.
        assert_eq!(
            found(&view.cells)
                .1
                .iter()
                .map(check::Finding::kind)
                .collect::<Vec<_>>(),
            ["conflict", "conflict"],
            "Mac {mac}: the cells alone say only that two state cells are conflicted"
        );
        let (own, generic) = state::findings(&TRACKER, &view.cells, &states);
        assert_eq!(generic, Vec::new(), "Mac {mac}");
        let own: Vec<(&str, Vec<String>)> = own
            .into_iter()
            .map(|finding| (finding.kind, finding.pks))
            .collect();
        assert_eq!(
            own,
            [
                ("state-not-allowed", vec!["TK-14".to_owned()]),
                ("state-not-allowed", vec!["TK-15".to_owned()]),
                ("state-outranked", vec!["TK-13".to_owned()]),
            ],
            "Mac {mac}: with a finding"
        );
        for (kind, _) in &own {
            assert_ne!(check::said(kind), *kind, "{kind} is said in a sentence");
            assert_eq!(
                check::headed(kind),
                checks::heading(kind).expect("a state finding has a heading"),
                "{kind} has a heading"
            );
        }
    }

    // One who has heard both says done again: the finding is settled, and the credit stays
    // with the record that wrote done.
    write(
        1,
        &saying(&blocker, Some("seen: done stands"), Vec::new()),
        vec![state_of("TK-13", "done")],
    );
    repair();
    for mac in [0, 1] {
        let (view, states) = read(mac);
        let healed = &states.packages["TK-13"];
        assert_eq!(
            (healed.state, operations(&healed.by), healed.lost.len()),
            (Some("done"), vec![done], 0),
            "Mac {mac}: said again, settled"
        );
        assert_eq!(healed.credit(), [&finisher], "Mac {mac}");
        let (own, generic) = state::findings(&TRACKER, &view.cells, &states);
        assert_eq!(generic, Vec::new(), "Mac {mac}");
        assert!(
            own.iter()
                .all(|finding| finding.pks != ["TK-13".to_owned()]),
            "Mac {mac}: {own:?}"
        );
    }

    // What a change's record carries decides whether it counts.
    let settled = |pk: &str| read(0).1.packages[pk].clone();
    let bare = saying(&finisher, None, Vec::new());
    let unreasoned = write(0, &bare, vec![state_of("TK-15", "blocked")]);
    let now = settled("TK-15");
    assert_eq!(
        (now.state, operations(&now.refused)),
        (Some("running"), vec![unreasoned]),
        "a block that gives no reason does not count"
    );
    write(0, &bare, vec![state_of("TK-15", "ready")]);
    let now = settled("TK-15");
    assert_eq!(
        (now.state, now.refused.len(), now.lost.len()),
        (Some("ready"), 0, 0),
        "a release to ready counts, and settles what did not count before it"
    );
    let unclaimed = write(0, &bare, vec![state_of("TK-15", "running")]);
    let now = settled("TK-15");
    assert_eq!(
        (now.state, operations(&now.refused)),
        (Some("ready"), vec![unclaimed]),
        "a running that writes no claim does not count"
    );
    let withdrawn = write(
        0,
        &saying(&blocker, Some("it was not finished"), Vec::new()),
        vec![state_of("TK-13", "blocked")],
    );
    let now = settled("TK-13");
    assert_eq!(
        (now.state, operations(&now.refused), now.credit()),
        (Some("done"), vec![withdrawn], vec![&finisher]),
        "no write of a state leaves done"
    );
}

/// TWO HUNDRED AGENTS CLAIM NEXT ON FIFTY PACKAGES WITH FIFTY DISTINCT HOLDERS (TK-14's exit
/// test; design §4.2, decisions TK-D4 and TK-D23). Four Macs hold fifty ready packages, and
/// fifty agents on each ask `aterm goals claim --next` ([`super::command`]), answered by the
/// leases every Mac reads alike from the same records ([`lease`]).
///
/// * **Spread.** In each round the next two agents of each Mac ask, the Macs hearing one
///   another only between rounds, so eight agents ask at once. Two Macs' claims of one package
///   stand together as `contested`, and the claim that does not keep it is released to the one
///   that does by its agent's next `claim --next`, which it asks first. Each agent picks among
///   the first eight free packages by a hash of itself, so a round places several packages
///   instead of the two each Mac's first two would, and all fifty are held within sixteen
///   rounds.
/// * **Fifty distinct holders.** Then every package is held by one claim, uncontested, each by
///   a different agent; the other hundred and fifty agents were each told that no ready package
///   is free, and a holder asking again is told the package it holds. Every Mac reads the same.
/// * **Renewal and lapse.** Three Macs renew their leases ([`lease::renew`]); the fourth stops.
///   A period later every Mac reads the fourth Mac's claims lapsed and the others' standing,
///   and agents of the three take the lapsed packages over with `claim --next`: still fifty
///   holders, none of them on the fourth Mac.
/// * **Contested, kept by work.** A holder releases its package (`release`), and two agents on
///   two Macs claim it at once. The one whose claim has the larger operation notes the package
///   before they meet; once they do, both claims stand, contested, and the one that worked on
///   the package keeps it. The other's `claim --next` releases it to the keeper and finds no
///   package free.
/// * **Done ends the lease.** Once the keeper says done, no one holds the package, and the
///   keeper's next `claim --next` is not answered with it.
///
/// NEGATIVE CONTROL, observed: with `lease::Leases::next` taking the first free package (the
/// hash index 0), every Mac's two agents take the same two packages in a round, and the test
/// fails at `round 1 claimed 2 packages`; with that check removed, at `all fifty packages are
/// held within 16 rounds: 32 held after 16`. With `lease::Leases::of` ordering the
/// contested claims by operation alone (dropping `worked`), it fails at `the agent that worked
/// on the package keeps it`. With `lease::Leases::of` leasing a claim whatever its package's
/// state, it fails at `a done package is held by no one`.
#[cfg(unix)]
#[test]
fn two_hundred_agents_claim_next_on_fifty_packages_with_fifty_distinct_holders() {
    use super::{
        command::{self, Ask, Env},
        lease::{self, Leases, Renewed},
    };
    use crate::{
        att::{self, Handle},
        file::{self, write::Refusal},
        said::Said,
        table::fixture::{Lab, repair_commons_among_first},
    };
    use aterm_secrets::commons::roster::Role;
    use std::{
        collections::{BTreeMap, VecDeque},
        time::Duration,
    };

    const NOW: u64 = 1_800_000_000_000;
    const MACS: usize = 4;
    const AGENTS: usize = 200;
    const PACKAGES: usize = 50;
    const ROUNDS: usize = 16;
    const WAIT: Duration = aterm_spec::wait::MUST_HAPPEN;

    let lab = Lab::new(&[Role::Writer; MACS]);
    let config = lab._home.path().join("aterm.toml");
    let trust = lab.bootstrap(0).trust();
    let everyone: Vec<usize> = (0..MACS).collect();
    let repair = || {
        let clocks = [NOW / 1000; MACS];
        for (rounds, table) in [
            (1, crate::catalog::scope(trust).digest()),
            (2, att::TRACKER.descriptor(trust).unwrap().scope.digest()),
        ] {
            repair_commons_among_first(&lab, rounds, &everyone, &clocks, table);
        }
    };
    let at = |mac: usize| Handle::at(lab.paths[mac].clone(), &att::TRACKER);
    let leases = |mac: usize, now: u64| lease::read(&at(mac), now).unwrap().2;
    let uuid = |mac: usize| crate::typed::keys::uuid32(&lab.machine(mac));

    // Mac 0 writes a lane and fifty ready packages, ranked; every Mac holds them.
    for mac in 0..MACS {
        file::fixture::declare(&lab.paths[mac], &att::TRACKER);
    }
    let id = |n: usize| format!("LS-{n:02}");
    at(0)
        .saying(Said::by(agent()))
        .commit_owner(|_| {
            let mut rows = vec![row(
                &LANE,
                "leases",
                &[
                    ("id", json!("leases")),
                    ("name", json!("Leases")),
                    ("prefix", json!("LS")),
                ],
            )];
            rows.extend((1..=PACKAGES).map(|n| {
                row(
                    &PACKAGE,
                    &id(n),
                    &[
                        ("id", json!(id(n))),
                        ("lane", json!("leases")),
                        ("rank", json!(n.to_string())),
                        ("title", json!("A package to lease")),
                        ("state", json!("ready")),
                    ],
                )
            }));
            Ok(((), rows))
        })
        .unwrap();
    repair();

    // Two hundred agents, fifty on each Mac.
    let agents: Vec<Principal> = (0..AGENTS)
        .map(|n| Principal {
            session: Some(format!("s-agent-{n:03}")),
            program: Some("claude".to_owned()),
            ..Principal::default()
        })
        .collect();
    let mac_of = |n: usize| n % MACS;
    let goals = |n: usize, now: u64, attempt: Duration, words: &[&str]| {
        let words: Vec<String> = words.iter().map(|word| (*word).to_owned()).collect();
        let ask = Ask::parse(&words)?;
        command::run(
            &lab.paths[mac_of(n)],
            &config,
            &ask,
            &Env {
                principal: &agents[n],
                by: "program",
                session: agents[n].session.as_deref(),
                now_ms: now,
                attempt,
                journal: None,
            },
        )
    };
    // Each package's one holder, as a Mac reads its leases: (package, agent).
    let holders = |leases: &Leases| -> BTreeMap<String, Principal> {
        leases
            .packages
            .iter()
            .filter_map(|(pk, lease)| Some((pk.clone(), lease.holder()?.by.clone())))
            .collect()
    };
    let agent_of = |by: &Principal| {
        agents
            .iter()
            .position(|agent| lease::same_agent(agent, by))
            .expect("a holder is one of the agents")
    };
    let nothing_free = |why: &str| why.ends_with("no ready package that nobody holds");

    // SPREAD: rounds in which each Mac's next two agents ask at once with the other Macs', each
    // Mac hearing the others only between rounds; an agent whose claim did not keep its package
    // asks again first, until all fifty packages are held, uncontested.
    let mut waiting: Vec<VecDeque<usize>> = (0..MACS)
        .map(|mac| (0..AGENTS).filter(|n| mac_of(*n) == mac).collect())
        .collect();
    let mut told_none: Vec<usize> = Vec::new();
    let (mut rounds, mut released) = (0, 0);
    let mut ask = |n: usize, now: u64, told_none: &mut Vec<usize>| -> Option<String> {
        match goals(n, now, WAIT, &["claim", "--next"]) {
            Ok((text, 0)) if text.starts_with("holds ") => None,
            Ok((text, code)) => {
                // Each line a write published: releases first, then the claim where one was made.
                let wrote = text
                    .lines()
                    .filter(|line| line.starts_with("published "))
                    .count();
                assert!(
                    wrote > 0 && wrote == text.lines().count() - usize::from(code != 0),
                    "agent {n}: every write of its ask is published: {text}"
                );
                released += wrote - usize::from(code == 0);
                if code != 0 {
                    assert!(nothing_free(text.trim_end()), "agent {n}: {text}");
                    told_none.push(n);
                }
                Some(text)
            }
            Err(Refusal::Problem(why)) if nothing_free(&why) => {
                told_none.push(n);
                None
            }
            other => panic!("agent {n}: {other:?}"),
        }
    };
    loop {
        let now = NOW + rounds as u64;
        let read = leases(0, now);
        if read.contested().is_empty() && holders(&read).len() == PACKAGES {
            break;
        }
        rounds += 1;
        assert!(
            rounds <= ROUNDS,
            "all fifty packages are held within {ROUNDS} rounds: {} held after {}",
            holders(&read).len(),
            rounds - 1
        );
        let mut asked = Vec::new();
        for queue in &mut waiting {
            for _ in 0..2 {
                if let Some(n) = queue.pop_front() {
                    ask(n, now, &mut told_none);
                    asked.push(n);
                }
            }
        }
        repair();
        // An agent whose claim does not keep its package asks again first.
        let read = leases(0, now);
        let keeping: Vec<usize> = holders(&read).values().map(&agent_of).collect();
        for n in asked.into_iter().rev() {
            if !keeping.contains(&n) && !told_none.contains(&n) {
                waiting[mac_of(n)].push_front(n);
            }
        }
        if rounds == 1 {
            // The first round's eight agents spread over more packages than one per Mac pair.
            assert!(
                read.packages.len() > 2,
                "round 1 claimed {} packages",
                read.packages.len()
            );
        }
    }
    let settled = NOW + rounds as u64;
    // Every other agent asks, and is told no ready package is free.
    for n in waiting.into_iter().flatten() {
        assert_eq!(
            ask(n, settled, &mut told_none),
            None,
            "agent {n} finds nothing free"
        );
    }
    assert!(released > 0, "a contested claim was released to the keeper");

    // FIFTY DISTINCT HOLDERS, read alike on every Mac.
    let fifty = |now: u64| {
        let first = leases(0, now);
        for mac in 1..MACS {
            assert_eq!(leases(mac, now), first, "Mac {mac} reads Mac 0's leases");
        }
        assert_eq!(
            first.contested(),
            Vec::<&str>::new(),
            "no claim is contested"
        );
        let holders = holders(&first);
        assert_eq!(holders.len(), PACKAGES, "every package is held");
        let distinct: BTreeSet<usize> = holders.values().map(&agent_of).collect();
        assert_eq!(
            distinct.len(),
            PACKAGES,
            "fifty packages, fifty distinct holders"
        );
        first
    };
    let read = fifty(settled);
    assert_eq!(
        told_none.len(),
        AGENTS - PACKAGES,
        "every other agent was told no ready package is free"
    );
    let (pk, holder) = holders(&read).into_iter().next().unwrap();
    let n = agent_of(&holder);
    assert_eq!(
        goals(n, settled, WAIT, &["claim", "--next"]),
        Ok((
            format!("holds {pk}: it is running, held by claude s-agent-{n:03}\n"),
            0
        )),
        "a holder asking again is told the package it holds"
    );

    // RENEWAL AND LAPSE: Macs 0 to 2 renew; Mac 3 stops.
    let renewed = NOW + lease::RENEW_EVERY_MS;
    for mac in 0..MACS - 1 {
        let wrote = lease::renew(&at(mac), &uuid(mac), renewed, |_| true).unwrap();
        assert!(matches!(wrote, Renewed::Wrote(_)), "Mac {mac}: {wrote:?}");
        assert_eq!(
            lease::renew(&at(mac), &uuid(mac), renewed + 1, |_| true).unwrap(),
            Renewed::Fresh,
            "Mac {mac} renews once a period"
        );
    }
    assert_eq!(
        lease::renew(&at(0), &uuid(0), renewed, |_| false).unwrap(),
        Renewed::Nothing,
        "no live session holds a lease: nothing to renew"
    );
    repair();
    let lapsed = settled + lease::PERIOD_MS;
    let on_mac = |claim: &lease::Claim, mac: usize| claim.machine.as_deref() == Some(&*uuid(mac));
    let read = leases(0, lapsed);
    let gone: Vec<String> = read
        .packages
        .iter()
        .filter(|(_, lease)| lease.held.is_empty())
        .map(|(pk, _)| pk.clone())
        .collect();
    assert!(!gone.is_empty(), "Mac 3 held some packages");
    for (pk, lease) in &read.packages {
        let claims: Vec<&lease::Claim> = lease.held.iter().chain(&lease.lapsed).collect();
        let [claim] = claims[..] else {
            panic!("{pk}: one claim")
        };
        assert_eq!(
            lease.held.is_empty(),
            on_mac(claim, MACS - 1),
            "{pk}: a lease lapses only where its Mac stopped renewing"
        );
    }
    for mac in 1..MACS {
        assert_eq!(leases(mac, lapsed), read, "Mac {mac} reads the same lapses");
    }
    // Agents of the three Macs that were told none was free take the lapsed packages over.
    let mut takers = told_none.iter().copied().filter(|n| mac_of(*n) != MACS - 1);
    let mut taken = Vec::new();
    for _ in &gone {
        let n = takers.next().unwrap();
        let (text, code) = goals(n, lapsed, WAIT, &["claim", "--next"]).unwrap();
        let (_, said) = text
            .strip_prefix("published ")
            .and_then(|rest| rest.split_once(": "))
            .unwrap_or_else(|| panic!("agent {n} takes over a lapsed package: {text}"));
        let (pk, holder) = said.split_once(" is running, held by ").unwrap();
        assert_eq!(
            (holder, code),
            (format!("claude s-agent-{n:03}\n").as_str(), 0),
            "agent {n} takes over a lapsed package"
        );
        taken.push(pk.to_owned());
        repair();
    }
    taken.sort();
    assert_eq!(taken, gone, "the lapsed packages, each taken over once");
    let read = fifty(lapsed);
    assert!(
        read.packages
            .values()
            .all(|lease| !on_mac(lease.holder().unwrap(), MACS - 1)),
        "no holder is on the Mac that stopped renewing"
    );

    // CONTESTED, KEPT BY WORK: a holder of Mac 0 releases its package, and agents of Macs 1
    // and 2 claim it at once.
    let (pk, holder) = holders(&read)
        .into_iter()
        .find(|(_, by)| mac_of(agent_of(by)) == 0)
        .unwrap();
    let (text, code) = goals(agent_of(&holder), lapsed, WAIT, &["release", &pk]).unwrap();
    assert!(
        text.ends_with(&format!("{pk} is ready\n")) && code == 0,
        "a release: {text}"
    );
    repair();
    let free: Vec<usize> = told_none
        .iter()
        .copied()
        .filter(|n| read.held_by(&agents[*n]).is_empty())
        .collect();
    let a = *free.iter().find(|n| mac_of(**n) == 1).unwrap();
    let b = *free.iter().find(|n| mac_of(**n) == 2).unwrap();
    let mut ops = BTreeMap::new();
    for n in [a, b] {
        let (text, code) = goals(n, lapsed, WAIT, &["claim", &pk]).unwrap();
        assert_eq!(code, 0, "{text}");
        let op = text
            .strip_prefix("published ")
            .and_then(|rest| rest.split(':').next())
            .unwrap()
            .to_owned();
        ops.insert(n, op);
    }
    // The agent whose claim has the larger operation works on the package before they meet.
    let (worker, idle) = if ops[&a] > ops[&b] { (a, b) } else { (b, a) };
    goals(
        worker,
        lapsed,
        WAIT,
        &["note", &pk, "--", "started on the lease"],
    )
    .unwrap();
    repair();
    for mac in 0..MACS {
        let read = leases(mac, lapsed);
        let lease = read.of_package(&pk).unwrap();
        assert!(lease.contested(), "Mac {mac}: both claims stand");
        assert_eq!(
            agent_of(&lease.holder().unwrap().by),
            worker,
            "Mac {mac}: the agent that worked on the package keeps it"
        );
    }
    let answer = goals(idle, lapsed, WAIT, &["claim", "--next"]).unwrap();
    assert!(
        answer.0.contains(&format!(
            "{pk} is running, held by claude s-agent-{worker:03}\n"
        )) && answer.0.ends_with("no ready package that nobody holds\n")
            && answer.1 != 0,
        "the other releases it to the keeper and finds nothing free: {answer:?}"
    );
    repair();
    let read = fifty(lapsed);
    assert_eq!(
        agent_of(&read.of_package(&pk).unwrap().holder().unwrap().by),
        worker
    );

    // DONE ENDS THE LEASE: once the keeper finishes the package, no one holds it, and its
    // agent's next ask is not answered with it.
    goals(
        worker,
        lapsed,
        WAIT,
        &["done", &pk, "--evidence", "note:the exit test passes"],
    )
    .unwrap();
    repair();
    assert!(
        !leases(0, lapsed).held(&pk),
        "a done package is held by no one"
    );
    assert_eq!(
        goals(worker, lapsed, WAIT, &["claim", "--next"]),
        Err(Refusal::Problem(
            "no ready package that nobody holds".to_owned()
        )),
        "the agent that finished it is not told it holds it"
    );
}

/// A CLAIMANT'S EXIT EDIT IS REFUSED AND AN APPROVED CHANGE LANDS (TK-15's exit test; design
/// §4.3, §7 finding 21). Once a package is claimed, its `exit` and `outcome` change only by a
/// `change` row someone other than its claimants approved, and a wrong done is withdrawn only
/// that way ([`super::frozen`]). Every Mac reads it alike, from the records.
///
/// * `TK-16` is ready on two Macs. Mac 0 claims it while Mac 1, not having heard, edits its
///   exit: each reads its own until they meet, then both read the exit the claim saw, and the
///   edit made at the same time as the claim is a finding (`frozen-edit`).
/// * `TK-15` is claimed, and its claimant edits its exit and outcome: neither counts, the
///   package reads its first exit though its cell holds the edit, and it is a finding. The
///   claimant states the first exit again, which changes nothing and settles the finding.
/// * The claimant asks for the edit as a change and approves it themselves: it does not
///   count. The lane's coordinator approves it: the claimant's edit lands, with no finding.
/// * An approval is not used twice: once another approved change has put the first exit back,
///   the first change's approval, which never saw that value, lets nothing through.
/// * The claimant finishes it, then says it is blocked: no write of a state leaves done. An
///   approved change of its state to `running` withdraws the done, and its credit with it.
/// * `TK-17` is imported running and claimed, its exit in the same record: that counts. An
///   outcome it never had, given by its claimant, is refused and does not show; a change whose
///   `before` names another value lets nothing through, nor does one approved by someone who
///   wrote its claim alone; stating its absence again settles the finding, and the change
///   approved by the coordinator lands.
///
/// NEGATIVE CONTROL, observed: with `frozen::Frozen::settle` taking no edit as frozen, the test
/// fails at `Mac 0: an edit made at the same time as a claim does not count`; with
/// `frozen::Approval::approves` not reading the claimants, it fails at `a claimant's own
/// approval does not count`.
#[cfg(unix)]
#[test]
fn a_claimants_exit_edit_is_refused_and_an_approved_change_lands() {
    use super::frozen::{self, Column};
    use super::state;
    use crate::{
        att::{self, Handle},
        file,
        said::Said,
        table::fixture::{Lab, repair_commons_among_first},
        typed::values::Evidence,
    };
    use astream_wire::sync::Digest;
    use aterm_secrets::commons::roster::Role;

    const NOW: u64 = 1_800_000_000_000;
    const WEAKER: &str = "targo --unverified test -p aterm-db --lib lab::tracker::tests";

    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let trust = lab.bootstrap(0).trust();
    let clocks = [NOW / 1000; 2];
    let repair = || {
        repair_commons_among_first(
            &lab,
            1,
            &[0, 1],
            &clocks,
            crate::catalog::scope(trust).digest(),
        );
        repair_commons_among_first(
            &lab,
            2,
            &[0, 1],
            &clocks,
            att::TRACKER.descriptor(trust).unwrap().scope.digest(),
        );
    };
    let at = |mac: usize| Handle::at(lab.paths[mac].clone(), &att::TRACKER);
    // One record on `mac` by `by`, giving `reason`: its operation.
    let write = |mac: usize, by: &Principal, reason: &str, changes: Vec<Change>| -> Digest {
        let said = Said {
            principal: by.clone(),
            reason: Some(reason.to_owned()),
            evidence: Vec::new(),
        };
        let ((), applied) = at(mac)
            .saying(said)
            .commit_owner(|_| Ok(((), vec![changes])))
            .unwrap();
        assert_eq!(applied.refused, None, "Mac {mac}: {reason}");
        assert_eq!(applied.records, 1, "Mac {mac}: {reason}");
        applied.operations[0]
    };
    let who = |session: &str| Principal {
        session: Some(session.to_owned()),
        program: Some("claude".to_owned()),
        ..Principal::default()
    };
    let (planner, claimant, coordinator) = (who("s-planner"), agent(), who("s-coordinator"));
    let first = exit();
    let weaker = json!({
        "repos": ["aterm"],
        "in": "aterm",
        "command": WEAKER,
        "needs": ["macos"],
        "timeout_s": 3600,
        "passes": "exit-0",
    });
    let claim = |exit: &Value| json!({"by": claimant.to_value(), "on": 20_774, "at": NOW, "exit": frozen::digest(exit)});
    let read = |mac: usize| state::read(&at(mac)).unwrap();
    let column = |mac: usize, pk: &str, column: &str| -> Column {
        read(mac).1.frozen.column(pk, column).cloned().unwrap()
    };
    let shown = |mac: usize, pk: &str, column: &str| -> (Value, Value) {
        let (view, states) = read(mac);
        let mut packages = view.project(&PACKAGE);
        let alone = packages.rows[pk].get(column).cloned().unwrap();
        states.over(&mut packages);
        (alone, packages.rows[pk].get(column).cloned().unwrap())
    };
    let findings = |mac: usize| -> Vec<(&'static str, Vec<String>)> {
        let (view, states) = read(mac);
        let (own, generic) = state::findings(&TRACKER, &view.cells, &states);
        assert_eq!(generic, Vec::new(), "Mac {mac}");
        own.into_iter()
            .map(|finding| (finding.kind, finding.pks))
            .collect()
    };
    let operations =
        |edits: &[frozen::Edit]| -> Vec<Digest> { edits.iter().map(|e| e.operation).collect() };
    let edit = |pk: &str, column: &str, value: &Value| put(&PACKAGE, pk, column, value.clone());
    // A change of `pk`'s `column` to `value` from `before`, approved by `approver` if any: its
    // key and its row.
    let change =
        |pk: &str, column: &str, value: &Value, before: &Value, approver: Option<&Principal>| {
            let key = mapped::change_key(pk, column, value);
            let mut cells = vec![
                ("package", json!(pk)),
                ("column", json!(column)),
                (column, value.clone()),
                ("before", json!(frozen::digest(before))),
                (
                    "reason",
                    json!("the exit names a test that cannot run here"),
                ),
            ];
            if let Some(by) = approver {
                cells.push(("approved_by", by.to_value()));
            }
            let row = row(&CHANGE, &key, &cells);
            (key, row)
        };

    // Two ready packages, on both Macs.
    file::fixture::declare(&lab.paths[0], &att::TRACKER);
    file::fixture::declare(&lab.paths[1], &att::TRACKER);
    write(
        0,
        &planner,
        "the lane",
        row(
            &LANE,
            "tables",
            &[
                ("id", json!("tables")),
                ("name", json!("Tables")),
                ("prefix", json!("TK")),
            ],
        ),
    );
    write(
        0,
        &planner,
        "the repository",
        row(
            &REPO,
            "aterm",
            &[
                ("id", json!("aterm")),
                ("remote", json!("alab/aterm")),
                ("path", json!("aterm")),
            ],
        ),
    );
    for pk in ["TK-15", "TK-16"] {
        write(
            0,
            &planner,
            "a package",
            row(
                &PACKAGE,
                pk,
                &[
                    ("id", json!(pk)),
                    ("lane", json!("tables")),
                    ("title", json!("Frozen exit")),
                    ("outcome", json!("What done means")),
                    ("exit", first.clone()),
                    ("state", json!("ready")),
                ],
            ),
        );
    }
    repair();

    // Mac 0 claims TK-16 while Mac 1 edits its exit.
    write(
        0,
        &claimant,
        "claim",
        vec![
            put(&PACKAGE, "TK-16", "state", json!("running")),
            put(&PACKAGE, "TK-16", "claim", claim(&first)),
        ],
    );
    let racing = write(
        1,
        &planner,
        "a narrower test",
        vec![edit("TK-16", "exit", &weaker)],
    );
    assert_eq!(
        column(1, "TK-16", "exit").value,
        Some(weaker.clone()),
        "Mac 1: an edit of a package no claim has seen counts"
    );
    repair();
    for mac in [0, 1] {
        let settled = column(mac, "TK-16", "exit");
        assert_eq!(
            (settled.value, operations(&settled.refused)),
            (Some(first.clone()), vec![racing]),
            "Mac {mac}: an edit made at the same time as a claim does not count"
        );
        assert_eq!(
            findings(mac),
            [("frozen-edit", vec!["TK-16".to_owned()])],
            "Mac {mac}"
        );
    }

    // The claimant of TK-15 edits its exit and outcome: neither counts.
    write(
        0,
        &claimant,
        "claim",
        vec![
            put(&PACKAGE, "TK-15", "state", json!("running")),
            put(&PACKAGE, "TK-15", "claim", claim(&first)),
        ],
    );
    let weakened = write(
        0,
        &claimant,
        "the test is too strict",
        vec![
            edit("TK-15", "exit", &weaker),
            edit("TK-15", "outcome", &json!("Less")),
        ],
    );
    for (name, was) in [("exit", &first), ("outcome", &json!("What done means"))] {
        let settled = column(0, "TK-15", name);
        assert_eq!(
            (settled.value.as_ref(), operations(&settled.refused)),
            (Some(was), vec![weakened]),
            "a claimant's {name} edit is refused"
        );
    }
    assert_eq!(
        shown(0, "TK-15", "exit"),
        (weaker.clone(), first.clone()),
        "the cell holds the edit; every reader takes the exit that stands"
    );
    assert!(
        findings(0).contains(&("frozen-edit", vec!["TK-15".to_owned()])),
        "a refused edit is a finding"
    );
    assert_ne!(
        check::said("frozen-edit"),
        "frozen-edit",
        "a frozen edit is said in a sentence"
    );
    assert_eq!(
        check::headed("frozen-edit"),
        checks::heading("frozen-edit").expect("a frozen finding has a heading"),
    );
    // Stating what stands again changes nothing and settles the finding.
    let restate = || {
        write(
            0,
            &claimant,
            "as it was",
            vec![
                edit("TK-15", "exit", &first),
                edit("TK-15", "outcome", &json!("What done means")),
            ],
        )
    };
    restate();
    assert_eq!(
        findings(0),
        [("frozen-edit", vec!["TK-16".to_owned()])],
        "stated again, settled"
    );

    // Asked as a change and approved by the claimant: it does not count.
    let (asked, own) = change("TK-15", "exit", &weaker, &first, Some(&claimant));
    write(0, &claimant, "asked", own);
    let self_approved = write(
        0,
        &claimant,
        "approved",
        vec![edit("TK-15", "exit", &weaker)],
    );
    let settled = column(0, "TK-15", "exit");
    assert_eq!(
        (settled.value, operations(&settled.refused), settled.landed),
        (Some(first.clone()), vec![self_approved], vec![]),
        "a claimant's own approval does not count"
    );

    // Approved by the lane's coordinator, the claimant's edit lands.
    restate();
    write(
        0,
        &coordinator,
        "approved: the narrower test proves the same",
        vec![put(&CHANGE, &asked, "approved_by", coordinator.to_value())],
    );
    let landed = write(
        0,
        &claimant,
        "as approved",
        vec![edit("TK-15", "exit", &weaker)],
    );
    let settled = column(0, "TK-15", "exit");
    assert_eq!(
        (
            settled.value.as_ref(),
            settled.refused.len(),
            settled.landed.clone()
        ),
        (Some(&weaker), 0, vec![(landed, asked.clone())]),
        "an approved change lands"
    );
    assert_eq!(
        findings(0),
        [("frozen-edit", vec!["TK-16".to_owned()])],
        "with no finding"
    );

    // Another approved change puts the first exit back; the first change's approval, which
    // never saw it, lets nothing through again.
    let (back, approved) = change("TK-15", "exit", &first, &weaker, Some(&coordinator));
    write(0, &coordinator, "asked and approved: put it back", approved);
    let restored = write(
        0,
        &claimant,
        "as approved",
        vec![edit("TK-15", "exit", &first)],
    );
    let reused = write(
        0,
        &claimant,
        "approved once",
        vec![edit("TK-15", "exit", &weaker)],
    );
    let settled = column(0, "TK-15", "exit");
    assert_eq!(
        (
            settled.value,
            operations(&settled.refused),
            settled.landed.last().cloned()
        ),
        (Some(first.clone()), vec![reused], Some((restored, back))),
        "an approval is not used twice"
    );

    // A done is left only by an approved change.
    let settled = |mac: usize| read(mac).1.packages["TK-15"].clone();
    let finished = Said {
        principal: claimant.clone(),
        reason: None,
        evidence: vec![Evidence::Run {
            command: EXIT_COMMAND.to_owned(),
            exit: 0,
            output: Some("test result: ok. 1 passed".to_owned()),
            repo: Some("aterm".to_owned()),
            commit: None,
        }],
    };
    let ((), _) = at(0)
        .saying(finished)
        .commit_owner(|_| {
            Ok((
                (),
                vec![vec![put(&PACKAGE, "TK-15", "state", json!("done"))]],
            ))
        })
        .unwrap();
    assert_eq!(settled(0).credit(), [&claimant]);
    let blocked = write(
        0,
        &claimant,
        "it was not finished",
        vec![put(&PACKAGE, "TK-15", "state", json!("blocked"))],
    );
    assert_eq!(
        (settled(0).state, operations_of(&settled(0).refused)),
        (Some("done"), vec![blocked]),
        "no write of a state leaves done"
    );
    let (_, withdrawal) = change(
        "TK-15",
        "state",
        &json!("running"),
        &json!("done"),
        Some(&coordinator),
    );
    write(
        0,
        &coordinator,
        "approved: its run was on another tree",
        withdrawal,
    );
    write(
        0,
        &claimant,
        "withdrawn as approved",
        vec![put(&PACKAGE, "TK-15", "state", json!("running"))],
    );

    // TK-17 is imported already claimed, with its exit and no outcome, in one record: what
    // that record states counts.
    write(
        0,
        &planner,
        "imported",
        row(
            &PACKAGE,
            "TK-17",
            &[
                ("id", json!("TK-17")),
                ("lane", json!("tables")),
                ("title", json!("Imported running")),
                ("exit", first.clone()),
                ("state", json!("running")),
                ("claim", claim(&first)),
            ],
        ),
    );
    assert_eq!(
        column(0, "TK-17", "exit").value,
        Some(first.clone()),
        "what a record stating a package's first state states counts"
    );
    assert!(
        !findings(0)
            .iter()
            .any(|(_, pks)| pks == &["TK-17".to_owned()]),
        "an import is no frozen edit"
    );
    // Its claimant gives it an outcome: refused, and none shows.
    let outcome = |text: &str| vec![edit("TK-17", "outcome", &json!(text))];
    let shows = || {
        let (view, states) = read(0);
        let mut packages = view.project(&PACKAGE);
        states.over(&mut packages);
        packages.rows["TK-17"].get("outcome").cloned()
    };
    let unasked = write(0, &claimant, "an outcome", outcome("Less"));
    assert_eq!(
        (shows(), operations(&column(0, "TK-17", "outcome").refused)),
        (None, vec![unasked]),
        "an edit of a column that held none is refused, and never shows"
    );
    // An approved change whose `before` is not what the package held lets nothing through.
    let (_, misread) = change(
        "TK-17",
        "outcome",
        &json!("Narrower"),
        &json!("Less"),
        Some(&coordinator),
    );
    write(0, &coordinator, "approved", misread);
    let misread = write(0, &claimant, "as approved", outcome("Narrower"));
    assert_eq!(
        operations(&column(0, "TK-17", "outcome").refused),
        [misread],
        "a change lands only on the value it names in `before`"
    );
    // One who wrote the package's claim alone approves nothing of it.
    let taker = who("s-taker");
    write(
        0,
        &taker,
        "taking it over",
        vec![put(
            &PACKAGE,
            "TK-17",
            "claim",
            json!({"by": taker.to_value(), "on": 20_774}),
        )],
    );
    let (asked, own) = change(
        "TK-17",
        "outcome",
        &json!("What done means"),
        &Value::Null,
        Some(&taker),
    );
    write(0, &taker, "asked and approved", own);
    let took = write(0, &taker, "as approved", outcome("What done means"));
    assert_eq!(
        operations(&column(0, "TK-17", "outcome").refused),
        [took],
        "one who wrote a claim alone does not approve"
    );
    // Stating its absence again changes nothing; then, approved by the coordinator, it lands.
    write(
        0,
        &taker,
        "as it was",
        vec![edit("TK-17", "outcome", &Value::Null)],
    );
    assert!(
        !findings(0)
            .iter()
            .any(|(_, pks)| pks == &["TK-17".to_owned()]),
        "stating absence again settles the finding"
    );
    write(
        0,
        &coordinator,
        "approved",
        vec![put(&CHANGE, &asked, "approved_by", coordinator.to_value())],
    );
    let landed = write(0, &taker, "as approved", outcome("What done means"));
    assert_eq!(
        (shows(), column(0, "TK-17", "outcome").landed),
        (Some(json!("What done means")), vec![(landed, asked)]),
        "a change from no value lands"
    );
    repair();
    for mac in [0, 1] {
        let now = settled(mac);
        assert_eq!(
            (now.state, now.refused.len(), now.credit()),
            (Some("running"), 0, vec![]),
            "Mac {mac}: an approved change withdraws a wrong done, and its credit"
        );
        assert_eq!(
            column(mac, "TK-15", "exit").value,
            Some(first.clone()),
            "Mac {mac}"
        );
    }
}

/// The records of `writes`.
#[cfg(unix)]
fn operations_of(writes: &[super::state::Write]) -> Vec<astream_wire::sync::Digest> {
    writes.iter().map(|write| write.operation).collect()
}

/// THE 384B3A9F CORPUS IMPORTS WITH NO REFUSED KEY AND EVERY FINDING CLASSIFIED (TK-11's exit
/// test): publication's `goals/data` at 384b3a9f, committed at
/// `crates/aterm-db/testdata/publication-384b3a9f`, read by [`super::import`] into the tracker's
/// rows, samples and notes, and nothing lost on the way.
///
/// * Every TSV line is a row of its table, or for `metrics.tsv` a sample of the `measure` log
///   or a note quoting it; repositories, which have no file, are rows of the names that name
///   them. Every non-empty cell has a place ([`import::Place`]) or a finding by file, line,
///   column and rule, and each place holds what it says: the row's column has a value, the
///   sample its column, the note quotes the cell.
/// * No key is refused: each row is keyed as its name makes it, in the key grammar and past
///   the key guard, no two alike; the whole import, one write of a table that holds nothing,
///   passes every rule a write passes (`att::Compiled::rules`), each record holding one row's
///   changes and saying who imported it, why and the commit (a row of more changes than a
///   record takes is several); and every sample and note is an entry its log takes.
/// * Every finding is classified: none is [`import::Rule::Refused`], each rule says itself in a
///   sentence and a heading, none keeps its words nowhere, and a finding's words are where it
///   says they are: the row's column holds them, or a note quotes them. Read back
///   through the state rule ([`state::findings`]), the rows hold no finding the import did not
///   name: each `state-mapped` and `decided-by-said` is a row the import named by that rule,
///   and the table layer finds nothing.
///
/// The counts the design asked to be printed from the data (§7, finding 18) are printed.
///
/// The same corpus with one lane's prefix too long for its column: that lane is no row, and
/// the import is still one write every rule takes, every cell placed or named.
///
/// NEGATIVE CONTROL, observed: with `import::Importer::named` keying a row by its name as it
/// stands (`name.to_owned()` for `mapped::key(name)`), the test fails at the first dotted
/// package's key (`package: TI-W0.1`), which no key takes; with `import::state` keeping a mapped
/// word in `state_said` without naming it (no `making.kept("state", rule)`), it fails at a
/// decision the state rule reads as `state-mapped` that the import did not name; with
/// `import::import` returning its first pass, the broken lane's packages still name it and the
/// test fails at `a row not made is named by no other row`.
#[cfg(unix)]
#[test]
fn the_384b3a9f_corpus_imports_with_no_refused_key_and_every_finding_classified() {
    use super::import::{self, Kept, Place, Rule};
    use super::{MEASURE, NOTE, state};
    use crate::att::{self, Author};
    use crate::typed::{owned, values::Evidence, view::View};
    use astream_wire::sync::Digest;
    use std::collections::BTreeMap;

    const COMMIT: &str = "384b3a9fe3212e58c8556619a6311ac80ad80bb7";
    const AT: u64 = 1_795_000_000_000;

    let root = aterm_tempfile::manifest_dir();
    let dir = root.join("testdata/publication-384b3a9f");
    if !dir.exists() {
        assert!(
            !root.join("../../publish/manifest.txt").exists(),
            "a checkout with publish/ holds the corpus; only the export withholds it"
        );
        return;
    }
    let files = import::read_dir(&dir).expect("the corpus is readable");
    let imported = import::import(COMMIT, AT, &files).expect("a commit");
    let collection = |name: &str| TRACKER.collection(name).unwrap_or_else(|| panic!("{name}"));

    // Every line is a row, a sample or a note.
    type Lines<'a> = Vec<(usize, BTreeMap<&'a str, &'a str>)>;
    let tsv: BTreeMap<&str, Lines<'_>> = files
        .iter()
        .map(|(name, text)| {
            let mut lines = text
                .lines()
                .enumerate()
                .filter(|(_, line)| !line.is_empty());
            let head: Vec<&str> = lines.next().unwrap().1.split('\t').collect();
            let lines = lines
                .map(|(n, line)| (n + 1, head.iter().copied().zip(line.split('\t')).collect()))
                .collect();
            (name.as_str(), lines)
        })
        .collect();
    let made: BTreeSet<(&str, usize)> = imported
        .rows
        .iter()
        .filter(|row| row.collection.name != REPO.name)
        .map(|row| (row.file, row.line))
        .collect();
    let mut lines = 0;
    for (file, rows) in &tsv {
        assert_eq!(imported.read[*file], rows.len(), "{file}");
        for (n, _) in rows {
            lines += 1;
            if *file == "metrics.tsv" {
                assert!(
                    imported
                        .placed
                        .contains_key(&((*file).to_owned(), *n, "date".to_owned())),
                    "{file} line {n} is a sample or a note quoting one"
                );
            } else {
                assert!(made.contains(&(*file, *n)), "{file} line {n} is a row");
            }
        }
    }

    // Every non-empty cell has a place, or a finding names it.
    let named: BTreeSet<(&str, usize, &str)> = imported
        .findings
        .iter()
        .map(|f| (f.file.as_str(), f.line, f.column.as_str()))
        .collect();
    let mut cells = 0;
    for (file, rows) in &tsv {
        for (n, row) in rows {
            for (column, text) in row.iter().filter(|(_, text)| !text.is_empty()) {
                cells += 1;
                assert!(
                    imported
                        .placed
                        .contains_key(&((*file).to_owned(), *n, (*column).to_owned()))
                        || named.contains(&(*file, *n, *column)),
                    "{file} line {n} {column}: {text:?} has no place and no finding"
                );
            }
        }
    }

    // No key is refused, and the whole import is one write every rule takes.
    let mut keys = BTreeSet::new();
    for row in &imported.rows {
        assert!(pk_grammar(&row.key), "{}: {}", row.collection.name, row.key);
        assert!(
            guard::word(&row.key).is_ok(),
            "{}: {}",
            row.collection.name,
            row.key
        );
        assert!(
            keys.insert((row.collection.name, row.key.clone())),
            "{} {} once",
            row.collection.name,
            row.key
        );
        assert_eq!(
            mapped::key_of(row.collection, |column| row.values.get(column).cloned()),
            Some(row.key.clone()),
            "{} {}: its key is the one its name makes",
            row.collection.name,
            row.key
        );
    }
    let mut changes = imported
        .changes()
        .expect("every value the import made is a cell");
    assert_eq!(
        att::TRACKER
            .rules(&mut changes, &View::empty_for_test(), Author::Owner)
            .expect("typed cells"),
        None,
        "the whole import passes every rule a write passes"
    );
    let said = imported.said(agent());
    assert_eq!(
        said.evidence,
        [Evidence::Commit {
            repo: "publication".to_owned(),
            commit: COMMIT.to_owned()
        }]
    );
    let records = att::TRACKER
        .packed(changes.clone(), Some(&said))
        .expect("each record the import writes fits a record");
    assert_eq!(records.len(), changes.len(), "each one a record of its own");
    let mut spread: BTreeMap<(&str, &str), usize> = BTreeMap::new();
    for record in &records {
        let rows: BTreeSet<(&str, &str)> = record
            .iter()
            .map(|change| {
                (
                    change.collection.as_str(),
                    CellKey::parse(&change.key).unwrap().pk,
                )
            })
            .collect();
        let [row] = rows.into_iter().collect::<Vec<_>>()[..] else {
            panic!("a record holds one row's changes")
        };
        *spread.entry(row).or_default() += 1;
    }
    assert_eq!(spread.len(), imported.rows.len(), "every row is written");
    let wide: Vec<_> = spread.iter().filter(|(_, records)| **records > 1).collect();
    for (log, entries) in [(&MEASURE, &imported.measures), (&NOTE, &imported.notes)] {
        for entry in entries {
            owned::log_row(log, entry, 1).unwrap_or_else(|e| panic!("{}: {e:?}", log.name));
        }
    }

    // Each place holds what it says.
    let cells_landed = landed(&changes);
    let projected: BTreeMap<&str, _> = TRACKER
        .collections
        .iter()
        .filter(|c| c.kind == Kind::Cells)
        .map(|c| (c.name, cell::project(c, &cells_landed)))
        .collect();
    for ((file, n, column), places) in &imported.placed {
        for place in places {
            match place {
                Place::Row {
                    collection: name,
                    key,
                    column: to,
                } => {
                    let row = &projected[name].rows[key];
                    let has = row.get(to).is_some_and(|x| !x.is_null())
                        || !cell::members(
                            collection(name).column(to).unwrap(),
                            row.fields.get(*to),
                        )
                        .is_empty();
                    assert!(
                        has,
                        "{file} line {n} {column}: {name} {key} {to} holds nothing"
                    );
                }
                Place::Measure { index, column: to } => {
                    let entry = &imported.measures[*index];
                    let text = tsv[file.as_str()]
                        .iter()
                        .find(|(line, _)| line == n)
                        .and_then(|(_, row)| row.get(column.as_str()))
                        .unwrap();
                    let holds = match *to {
                        "date" => {
                            crate::typed::grid::day_date(entry["at"].as_u64().unwrap() / 86_400_000)
                                .as_deref()
                                == Some(*text)
                        }
                        "goal" | "metric" => entry.contains_key("metric"),
                        other => entry[other].as_str() == Some(*text),
                    };
                    assert!(
                        holds,
                        "{file} line {n} {column}: sample {index} holds {text:?}"
                    );
                }
                Place::Note { index } => {
                    let note = &imported.notes[*index];
                    let text = tsv[file.as_str()]
                        .iter()
                        .find(|(line, _)| line == n)
                        .and_then(|(_, row)| row.get(column.as_str()))
                        .unwrap();
                    let quoted = Evidence::note(&format!("{column}: {text}")).to_value();
                    assert!(
                        note["evidence"]
                            .as_array()
                            .is_some_and(|evidence| evidence.contains(&quoted)),
                        "{file} line {n} {column}: note {index} quotes it"
                    );
                    assert!(
                        projected[note["on"].as_str().unwrap()]
                            .rows
                            .contains_key(note["about"].as_str().unwrap()),
                        "note {index} is about a row the import made"
                    );
                }
            }
        }
    }

    // Every finding is classified, and its words are where it says.
    for rule in Rule::ALL {
        assert!(
            !rule.word().is_empty() && !rule.sentence().is_empty() && !rule.heading().is_empty()
        );
    }
    for finding in &imported.findings {
        assert_ne!(
            finding.rule,
            Rule::Refused,
            "the column's type refused {finding:?} by a rule the import does not name"
        );
        match finding.kept {
            Kept::Note => assert!(
                imported
                    .placed
                    .get(&(finding.file.clone(), finding.line, finding.column.clone()))
                    .is_some_and(|places| places.iter().any(|p| matches!(p, Place::Note { .. }))),
                "{finding:?} is quoted in a note"
            ),
            Kept::Column => {
                let text = tsv[finding.file.as_str()]
                    .iter()
                    .find(|(line, _)| *line == finding.line)
                    .and_then(|(_, row)| row.get(finding.column.as_str()))
                    .unwrap();
                let pk = finding.row.as_deref().expect("a row's");
                let table = if finding.file == "decisions.tsv" {
                    "decision"
                } else {
                    "package"
                };
                let row = &projected[table].rows[pk];
                let kept = match finding.column.as_str() {
                    "state" => row.get("state_said"),
                    "decided_by" => row.get("decided_by").and_then(|by| by.get("said")),
                    "claimed_by" => row
                        .get("claim")
                        .and_then(|claim| claim.get("by"))
                        .and_then(|by| by.get("said")),
                    other => panic!("{other}: a column keeps words only as said"),
                };
                assert_eq!(kept.and_then(Value::as_str), Some(*text), "{finding:?}");
            }
            Kept::Not => panic!("{finding:?} keeps its words nowhere"),
        }
    }

    // Read back through the state rule: no finding the import did not name.
    let operation = |n: usize| -> Digest {
        let mut digest = [0; 32];
        digest[..8].copy_from_slice(&(n as u64 + 1).to_be_bytes());
        digest
    };
    let records: Vec<_> = records
        .iter()
        .enumerate()
        .map(|(n, record)| (operation(n), record.as_slice(), Some(&said)))
        .collect();
    let found = state::writes(records.iter().copied(), |_, _| false);
    let edits = super::frozen::Edits::of(records.iter().copied(), &found, |_, _| false);
    let states = state::settle(found, &edits, &cells_landed);
    let (own, generic) = state::findings(&TRACKER, &cells_landed, &states);
    assert_eq!(generic, Vec::new(), "the table layer finds nothing");
    let by_rule = |rule: Rule, column: &str| -> BTreeSet<String> {
        imported
            .findings
            .iter()
            .filter(|f| f.rule == rule && f.column == column)
            .filter_map(|f| f.row.clone())
            .collect()
    };
    let mapped_states: BTreeSet<String> = by_rule(Rule::StateMapped, "state")
        .union(&by_rule(Rule::NotAWord, "state"))
        .cloned()
        .collect();
    for finding in &own {
        let [pk] = &finding.pks[..] else {
            panic!("{finding:?} names one row")
        };
        let expected = match finding.kind {
            "state-mapped" => &mapped_states,
            "decided-by-said" => &by_rule(Rule::SaidInWords, "decided_by"),
            kind => panic!("{kind} on {pk}: a finding the import did not name"),
        };
        assert!(expected.contains(pk), "{finding:?}: the import named it");
    }

    // What the data holds, counted from it.
    let mut by: BTreeMap<(&str, &str, &str), (usize, usize)> = BTreeMap::new();
    for finding in &imported.findings {
        let count = by
            .entry((
                finding.rule.word(),
                finding.file.as_str(),
                finding.column.as_str(),
            ))
            .or_default();
        count.0 += 1;
        count.1 += finding.members;
    }
    let mut tables: BTreeMap<&str, usize> = BTreeMap::new();
    for row in &imported.rows {
        *tables.entry(row.collection.name).or_default() += 1;
    }
    println!("rows of more cells than one record takes: {wide:?}");
    println!(
        "publication {COMMIT}: {lines} lines, {cells} cells; {} rows {tables:?}, {} samples, {} notes, {} findings",
        imported.rows.len(),
        imported.measures.len(),
        imported.notes.len(),
        imported.findings.len()
    );
    for ((rule, file, column), (cells, members)) in &by {
        println!("  {rule} {file} {column}: {cells} cells, {members} members");
    }
    assert_eq!(tables["package"], 1121, "every package");
    assert_eq!(
        by[&("names-no-row", "packages.tsv", "depends")].1,
        111,
        "the words in depends that name no package (§7, finding 18)"
    );

    // A line the tracker cannot make a row of is a row nothing names: a lane whose prefix its
    // column refuses is no lane, its packages name no lane and are not made, and what named
    // them names no row; the import is still one write every rule takes, every cell placed or
    // named.
    let mut broken = files.clone();
    let lanes = broken.get_mut("lanes.tsv").unwrap();
    assert_eq!(
        lanes.matches("\tCU\t").count(),
        1,
        "the truth lane's prefix"
    );
    *lanes = lanes.replacen("\tCU\t", "\tCLEANUPXX\t", 1);
    let again = import::import(COMMIT, AT, &broken).expect("a commit");
    assert!(
        !again
            .rows
            .iter()
            .any(|row| row.collection.name == LANE.name && row.key == "truth"),
        "a lane its prefix's column refuses is not made"
    );
    let mut changes = again.changes().expect("typed cells");
    assert_eq!(
        att::TRACKER
            .rules(&mut changes, &View::empty_for_test(), Author::Owner)
            .expect("typed cells"),
        None,
        "a row not made is named by no other row"
    );
    let named: BTreeSet<(&str, usize, &str)> = again
        .findings
        .iter()
        .map(|f| (f.file.as_str(), f.line, f.column.as_str()))
        .collect();
    for (file, rows) in &tsv {
        for (n, row) in rows {
            for (column, _) in row.iter().filter(|(_, text)| !text.is_empty()) {
                assert!(
                    again
                        .placed
                        .contains_key(&((*file).to_owned(), *n, (*column).to_owned()))
                        || named.contains(&(*file, *n, *column)),
                    "{file} line {n} {column}: placed or named"
                );
            }
        }
    }
    assert!(
        again
            .findings
            .iter()
            .any(|f| f.rule == Rule::RowNotMade && f.file == "packages.tsv"),
        "a package of the lane that is not made is named cell by cell"
    );
}
