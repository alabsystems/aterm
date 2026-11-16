// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Check view on a scratch registry (`aterm_db::table::fixture::Lab`: an isolated home, no
//! live vault, fleet or Commons). Every value is synthetic.

use crate::App;
use crate::native_tables::native_tables_tests::{
    MC, act, put_in, render, said, settle, two_macs, view_of,
};
use aterm_db::alab::Registry;
use aterm_db::alab::registry::MACHINE;
use aterm_db::att::{Handle, REGISTRY};
use aterm_db::table::fixture::repair_commons;
use aterm_db::typed::cell::{self, CellKey, CellValue};
use aterm_json::json;

/// A second machine row whose aliases name the first's label: `label-ambiguous`.
const OTHER: &str = "mc-000000000000000b";

fn tables(app: &mut App) -> &mut crate::native_tables::TablesApp {
    app.native_runtime
        .tables_mut()
        .expect("the Tables page is open")
}

/// Check lists every finding `aterm db alab check` reports of the registry, each in the words
/// its `--tsv` form says it by (`aterm_db::typed::check::said`) with where it is, a count of them
/// inside the view and none on the rail, and the way to each row it names: Show row opens the
/// row's table with the row selected. Under the findings it says the store's state: whether this
/// Mac can write, whether its history is complete, its records and bytes against the store's
/// caps, this Mac's records against one writer's cap and its automatic-record quota, the refused
/// and inert records, the generation, how many Macs have written it, and the roll line. Nothing
/// of it is worked out until a view opens Check.
/// NEGATIVE CONTROL: with `native_tables_check::list` saying each finding by its word (its
/// press named `found.kind` for `found.said`), the finding reads its word: `` left:
/// "label-ambiguous" right: "A machine's label is another live machine's alias, so it names two
/// Macs" ``. With `TablesApp::check` leaving the reader's flag off, Check is never read: `` the
/// reader works check out once Check is open ``; so too with the reader's skip resting on the
/// registry's mark alone (its `(mark, checking)` made `(mark, false)`), since Check opens on a
/// registry that has not moved and the read it asks for answers unchanged.
#[test]
fn check_lists_each_finding_and_the_stores_state() {
    let lab = two_macs();
    let here = Registry::at(lab.paths[0].clone());
    let there = Registry::at(lab.paths[1].clone());
    // Two Macs name one cell apart without seeing each other: a conflict.
    here.write(vec![vec![put_in(
        &MACHINE,
        MC,
        "nickname",
        json!("Backup"),
    )]])
    .unwrap();
    there
        .write(vec![vec![put_in(&MACHINE, MC, "nickname", json!("Spare"))]])
        .unwrap();
    // Another machine's alias is the first's label: the lab's `label-ambiguous`.
    here.write(vec![vec![
        put_in(&MACHINE, OTHER, "label", json!("m55")),
        put_in(&MACHINE, OTHER, "status", json!("active")),
        put_in(&MACHINE, OTHER, "row", json!("live")),
        cell::change(
            &MACHINE,
            CellKey {
                pk: OTHER,
                col: "aliases",
                elem: Some("m9"),
            },
            &CellValue::owner(MACHINE.version, json!(true)),
        )
        .unwrap(),
    ]])
    .unwrap();
    repair_commons(&lab, 4);

    // What the verb reports, read here as the page reads it.
    let handle = Handle::at(lab.paths[0].clone(), &REGISTRY);
    let (view, usage) = handle.usage().unwrap();
    let found = aterm_db::alab::command::checked(&lab.paths[0], &handle, &view, &usage).unwrap();
    let reported: Vec<aterm_db::att::command::Line> =
        aterm_db::alab::command::lab_json(&found.lab, &found.seen, &found.duplicates)
            .iter()
            .chain(&found.findings)
            .map(aterm_db::att::command::line)
            .collect();
    let kinds: Vec<&str> = reported.iter().map(|line| line.kind.as_str()).collect();
    assert!(
        kinds.contains(&"conflict") && kinds.contains(&"label-ambiguous"),
        "{kinds:?}"
    );

    let mut app = App::headless_for_test();
    app.open_tables_tab_at(Some(lab.paths[0].clone())).unwrap();
    settle(&mut app);
    assert!(
        tables(&mut app).checked_read().is_none(),
        "nothing of Check is worked out before a view opens it"
    );
    // The rail's Check line says what it is and counts nothing.
    let ui = render(&app);
    let line = said(&ui, crate::native_tables_check::OPEN).expect("Check is on the rail");
    assert_eq!(line, "Check: what check finds and the store's state");
    assert!(!line.chars().any(|c| c.is_ascii_digit()), "{line}");

    act(&mut app, crate::native_tables_check::OPEN, None);
    settle(&mut app);
    assert!(
        tables(&mut app).checked_read().is_some(),
        "the reader works check out once Check is open"
    );
    let ui = render(&app);
    assert_eq!(said(&ui, "tables/title").as_deref(), Some("Check"));
    assert_eq!(
        said(&ui, "tables/count"),
        Some(format!("{} findings", reported.len()))
    );
    // Each finding, in the words the verbs' `--tsv` form says it by, and where it is.
    for (n, line) in reported.iter().enumerate() {
        let words =
            said(&ui, &format!("tables/check/finding/{n}")).expect("each finding is listed");
        let (said_words, place) = words.rsplit_once(": ").unwrap();
        assert_eq!(
            said_words,
            aterm_db::typed::check::said(&line.kind),
            "{words}"
        );
        assert_ne!(said_words, line.kind, "{words}");
        for pk in &line.pks {
            assert!(!place.contains(pk.as_str()), "a row by its words: {words}");
        }
    }
    let conflict = reported
        .iter()
        .position(|line| line.kind == "conflict")
        .unwrap();
    assert_eq!(
        said(&ui, &format!("tables/check/finding/{conflict}")).unwrap(),
        format!(
            "{}: Machines \u{b7} m9 \u{b7} nickname",
            aterm_db::typed::check::said("conflict")
        )
    );

    // The store's state, each line from what the store holds here.
    let compiled = handle.compiled();
    let quota_spent = view.quota.spent.get(&usage.actor).copied().unwrap_or(0);
    let store = |key: &str| said(&ui, &format!("tables/check/store/{key}")).unwrap();
    assert_eq!(store("write"), "This machine can write the registry");
    assert_eq!(store("history"), "Its history is complete here");
    assert_eq!(
        store("records"),
        format!("Records: {} of 8,192", usage.records)
    );
    assert!(store("bytes").ends_with(" of 128 MB"), "{}", store("bytes"));
    assert_eq!(
        store("mine"),
        format!(
            "This machine's records: {} of 4,096 one writer may hold",
            usage.mine_records
        )
    );
    assert_eq!(
        store("quota"),
        format!(
            "This machine's automatic records: {quota_spent} of its quota of {}",
            compiled.table.self_quota
        )
    );
    assert_eq!(store("refused"), "Refused records: none");
    assert_eq!(store("inert"), "Inert records: none");
    assert_eq!(
        store("generation"),
        format!("Generation {} ({})", compiled.generation, compiled.name())
    );
    assert_eq!(store("writers"), "2 machines have written it");
    assert_eq!(store("roll"), "Check asks for no roll");

    // A finding selected shows the rows it names, each with the way to it.
    let ambiguous = reported
        .iter()
        .position(|line| line.kind == "label-ambiguous")
        .unwrap();
    act(&mut app, &format!("tables/check/finding/{ambiguous}"), None);
    let ui = render(&app);
    assert_eq!(
        said(&ui, "tables/pane/said").as_deref(),
        Some(aterm_db::typed::check::said("label-ambiguous"))
    );
    let names = &reported[ambiguous].pks;
    assert_eq!(names.len(), 2, "{names:?}");
    let show = format!("tables/check/row/{ambiguous}/1");
    let label = if names[1] == MC { "m9" } else { "m55" };
    assert_eq!(
        said(&ui, &show),
        Some(format!("Show row {label} in Machines"))
    );
    act(&mut app, &show, None);
    let ui = render(&app);
    assert_eq!(said(&ui, "tables/title").as_deref(), Some("Machines"));
    assert!(!view_of(&app).check.open, "Show row leaves Check");
    assert_eq!(said(&ui, "tables/pane/id"), Some(names[1].clone()));
    assert!(
        said(&ui, &format!("tables/cell/{}/label", names[1])).is_some(),
        "the row is listed"
    );
}

/// Check says each row a finding names by the words its row is known by (a machine by its
/// label, as a reference to it reads), in the list and in the selected finding's pane, never by
/// its key; and heads the selected finding with a few words in sentence case, the kind word
/// `check --tsv` prints under it.
/// NEGATIVE CONTROL, observed: with `where_words` joining the finding's keys, the list says them:
/// `` a row by its words: A machine's label is another live machine's alias, so it names two
/// machines: Machines · mc-0000000000000009, mc-000000000000000b ``; with the pane heading the
/// finding by `found.kind`, `` left: Some("label-ambiguous") right: Some("Label names two
/// machines") ``; with no line under the heading saying the finding's word, `` left: None right:
/// Some("label-ambiguous") ``.
#[test]
fn check_names_each_row_by_its_words_and_heads_each_finding() {
    let lab = two_macs();
    let here = Registry::at(lab.paths[0].clone());
    here.write(vec![vec![
        put_in(&MACHINE, OTHER, "label", json!("m55")),
        put_in(&MACHINE, OTHER, "status", json!("active")),
        put_in(&MACHINE, OTHER, "row", json!("live")),
        cell::change(
            &MACHINE,
            CellKey {
                pk: OTHER,
                col: "aliases",
                elem: Some("m9"),
            },
            &CellValue::owner(MACHINE.version, json!(true)),
        )
        .unwrap(),
    ]])
    .unwrap();
    repair_commons(&lab, 4);
    let mut app = App::headless_for_test();
    app.open_tables_tab_at(Some(lab.paths[0].clone())).unwrap();
    settle(&mut app);
    act(&mut app, crate::native_tables_check::OPEN, None);
    settle(&mut app);
    let ui = render(&app);
    let sentence = aterm_db::typed::check::said("label-ambiguous");
    let n = (0..)
        .map_while(|n| said(&ui, &format!("tables/check/finding/{n}")))
        .position(|line| line.starts_with(sentence))
        .expect("the label is found ambiguous");
    let line = said(&ui, &format!("tables/check/finding/{n}")).unwrap();
    let place = line.strip_prefix(&format!("{sentence}: ")).unwrap();
    let mut rows: Vec<&str> = place
        .strip_prefix("Machines \u{b7} ")
        .unwrap()
        .split(", ")
        .collect();
    rows.sort_unstable();
    assert_eq!(rows, ["m55", "m9"], "a row by its words: {line}");

    act(&mut app, &format!("tables/check/finding/{n}"), None);
    let ui = render(&app);
    assert_eq!(
        said(&ui, "tables/pane/title").as_deref(),
        Some("Label names two machines")
    );
    assert_eq!(
        said(&ui, "tables/pane/kind").as_deref(),
        Some("label-ambiguous"),
        "the word `check --tsv` says it by"
    );
    let mut named: Vec<String> = (0..2)
        .map(|m| said(&ui, &format!("tables/pane/row/{m}/key")).unwrap())
        .collect();
    named.sort_unstable();
    assert_eq!(named, ["m55 \u{b7} Machines", "m9 \u{b7} Machines"]);
    // A kind no heading is written for still reads as words.
    assert_eq!(
        aterm_db::typed::check::headed("some-new-kind"),
        "Some new kind"
    );
}

/// Two rows a finding names that read the same (two enrollments of one machine, each known by
/// its machine's label) each say their key beside their words, so the finding never reads as
/// naming one row twice; rows that read apart, or of different tables, keep their words alone.
/// NEGATIVE CONTROL, observed: with `apart` returning `rows` as given, `` left: [(Some
/// ("machine-facts"), "m1"), (Some("machine-facts"), "m1"), …] right: [(Some("machine-facts"),
/// "m1 (0123)"), …] ``.
#[test]
fn two_rows_that_read_the_same_say_their_keys() {
    let pks: Vec<String> = ["0123", "4567", "mc-1", "mc-2"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let rows = vec![
        (Some("machine-facts"), "m1".to_owned()),
        (Some("machine-facts"), "m1".to_owned()),
        (Some("machine"), "m1".to_owned()),
        (None, "mc-2".to_owned()),
    ];
    assert_eq!(
        crate::native_tables_check::apart(rows, &pks),
        [
            (Some("machine-facts"), "m1 (0123)".to_owned()),
            (Some("machine-facts"), "m1 (4567)".to_owned()),
            (Some("machine"), "m1".to_owned()),
            (None, "mc-2".to_owned()),
        ]
    );
}
