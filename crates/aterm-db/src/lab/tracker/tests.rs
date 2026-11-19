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
fn agent() -> Principal {
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

/// A package that needs a mapped key (its id holds a dot), and a decision that does (37 bytes).
const DOTTED: &str = "TI-W0.1";
const LONG: &str = "S8-ONE-AUTHORITATIVE-SID-GRAMMAR-TEST";

/// A clean fixture: a row of every cells table, one write each, with a row of each kind the
/// key grammar does not take as it is named (a dotted package and metric, a long decision, a
/// project of two words, a release with a slash), every reference naming a row here.
fn fixture() -> Vec<Vec<Change>> {
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
/// * The family is its sixteen cells tables and its two logs, each with a face.
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

    /// The tracker's packages as an owner's shared file: a test's, since this build names no
    /// shared file of the tracker's cells tables (TK-12).
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

    // The family: sixteen cells tables and two logs, registered for no production build.
    let kinds = |kind| {
        TRACKER
            .collections
            .iter()
            .filter(|collection| collection.kind == kind)
            .count()
    };
    assert_eq!((kinds(Kind::Cells), kinds(Kind::Log)), (16, 2));
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
