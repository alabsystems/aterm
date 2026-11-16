// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Rows on the Tables page on a scratch registry (`aterm_db::table::fixture::Lab`: an isolated
//! home, no live vault, fleet or Commons): a row added, retired and restored through the one
//! Write, a retire held back by the rows that name its row, the lifecycle filter, who adds the
//! rows the grid adds none of, a new row's secret named by its verb and entry, a Mac that
//! cannot write, and a reference chosen from its target's rows and stored by key. Every value is
//! synthetic.

use super::*;
use crate::App;
use crate::native_app::{SemanticInput, TextInputEvent};
use crate::native_tables::native_tables_tests::{
    MC, act, cell_says, key_in, put_in, render, said, settle, two_macs,
};
use crate::native_tables::{EDIT, WRITE};
use crate::native_tables_choices::CHOICE;
use crate::native_ui::{CompiledUi, SemanticRole, UiKey};
use aterm_db::alab::Registry;
use aterm_db::alab::registry::{ACCOUNT, ADDRESS, MACHINE, PREFERENCE};
use aterm_db::att::{Handle, REGISTRY};
use aterm_db::table::fixture::{Lab, repair_commons};
use aterm_db::typed::Collection;
use aterm_json::json;
use aterm_secrets::commons::roster::Role;

/// The first new row's key on the page.
const FIRST: &str = "+new-1";

fn settle_write(app: &mut App) {
    app.native_runtime
        .tables_mut()
        .expect("the Tables page is open")
        .settle_write();
}

/// Row `pk`'s `column` of `collection` as Mac `mac` of `lab` reads its registry.
fn held(
    lab: &Lab,
    mac: usize,
    collection: &'static Collection,
    pk: &str,
    column: &str,
) -> Option<aterm_json::Value> {
    Handle::at(lab.paths[mac].clone(), &REGISTRY)
        .view()
        .unwrap()
        .project(collection)
        .rows
        .get(pk)
        .and_then(|row| row.get(column))
        .cloned()
}

/// How many operations the registry holds on Mac `mac`.
fn operations(lab: &Lab, mac: usize) -> usize {
    Handle::at(lab.paths[mac].clone(), &REGISTRY)
        .history()
        .unwrap()
        .len()
}

/// Type `text` into row `pk`'s `column`: a press selects it, a second opens it (one opens the
/// cell already selected), Return stages it.
fn type_in(app: &mut App, pk: &str, column: &str, text: &str) {
    let cell = format!("tables/cell/{pk}/{column}");
    act(app, &cell, None);
    if said(&render(app), EDIT).is_none() {
        act(app, &cell, None);
    }
    assert!(said(&render(app), EDIT).is_some(), "{pk} {column} opened");
    act(app, EDIT, Some(SemanticInput::Text(text.into())));
    key_in(app, TextInputEvent::Submit);
}

/// The page adds a machine where its birth rule lets one be born: Add lays out a new row,
/// worded `new` in its number's seat and live in its lifecycle cell, both not written; its label
/// is typed and Write writes the row under the key the registry derives from it. Retire stages
/// its lifecycle `retired` (worded `retire`), Write writes it and the live grid no longer lists
/// the row; Restore from its pane stages `live` (worded `restore`) and Write writes that. Each
/// is one operation of the registry's one save. A retired or restored row's lifecycle cell is
/// spoken by its value alone, which says what the word in its number's seat says.
/// NEGATIVE CONTROLS: with `TablesApp::write` not carrying a new row's cells as a grid row of
/// their own (its `rows::is_new` branch skipped, `if false && …`), the Write holds nothing of the
/// row and none is born: `` assertion `left == right` failed: the registry holds the new machine
/// left: None right: Some(String("m12")) ``. With the grid speaking the row's word in every
/// lifecycle cell that carries one (not only a new row's), the cell says it twice: `` left:
/// "retired, retire, not written" right: "retired, not written" ``.
#[test]
fn a_row_is_added_retired_and_restored_through_the_one_write() {
    let lab = two_macs();
    let mut app = App::headless_for_test();
    app.open_tables_tab_at(Some(lab.paths[0].clone())).unwrap();
    settle(&mut app);
    act(&mut app, "tables/sheet/machine", None);
    let before = operations(&lab, 0);

    let ui = render(&app);
    assert_eq!(
        said(&ui, ADD).as_deref(),
        Some("Add machine: a new row of Machines")
    );
    act(&mut app, ADD, None);
    let ui = render(&app);
    assert_eq!(
        said(&ui, &format!("tables/num/{FIRST}/n")).as_deref(),
        Some("new")
    );
    assert_eq!(
        cell_says(&ui, FIRST, "row"),
        "live, new, not written",
        "a new row is born live"
    );
    assert_eq!(
        said(&ui, "tables/pane/id").as_deref(),
        Some("Its key is made when it is written")
    );
    type_in(&mut app, FIRST, "label", "m12");
    let ui = render(&app);
    assert_eq!(cell_says(&ui, FIRST, "label"), "m12, not written");
    // The check as it is staged reads the new row as the save will: its status is required.
    assert_eq!(
        said(&ui, "tables/bar/count").as_deref(),
        Some("1 change not written \u{b7} 1 refused")
    );
    assert!(
        cell_says(&ui, FIRST, "status").contains("a new row needs a value here"),
        "{}",
        cell_says(&ui, FIRST, "status")
    );
    type_in(&mut app, FIRST, "status", "active");
    assert_eq!(
        said(&render(&app), "tables/bar/count").as_deref(),
        Some("2 changes not written")
    );

    act(&mut app, WRITE, None);
    settle_write(&mut app);
    let pk = aterm_db::typed::keys::machine_pk("m12");
    assert_eq!(
        held(&lab, 0, &MACHINE, &pk, "label"),
        Some(json!("m12")),
        "the registry holds the new machine"
    );
    assert_eq!(held(&lab, 0, &MACHINE, &pk, "row"), Some(json!("live")));
    assert_eq!(
        operations(&lab, 0),
        before + 1,
        "one Write is one operation"
    );
    let ui = render(&app);
    assert!(
        said(&ui, &format!("tables/cell/{FIRST}/label")).is_none(),
        "the page's key for it is gone"
    );
    assert_eq!(cell_says(&ui, &pk, "label"), "m12");
    assert!(
        said(&ui, "tables/note").is_some_and(|n| n.contains("added 1 row")),
        "{:?}",
        said(&ui, "tables/note")
    );
    assert_eq!(
        said(&ui, "tables/sheet/machine").as_deref(),
        Some("Machines: 2 rows")
    );

    // Retire, from the row's pane.
    act(&mut app, &format!("tables/cell/{pk}/label"), None);
    assert_eq!(
        said(&render(&app), RETIRE).as_deref(),
        Some("Retire this machine")
    );
    act(&mut app, RETIRE, None);
    let ui = render(&app);
    assert_eq!(
        said(&ui, &format!("tables/num/{pk}/n")).as_deref(),
        Some("retire")
    );
    // Its value says the retire; the word in the number's seat is not said again.
    assert_eq!(cell_says(&ui, &pk, "row"), "retired, not written");
    assert_eq!(
        held(&lab, 0, &MACHINE, &pk, "row"),
        Some(json!("live")),
        "staged is not written"
    );
    act(&mut app, WRITE, None);
    settle_write(&mut app);
    assert_eq!(held(&lab, 0, &MACHINE, &pk, "row"), Some(json!("retired")));
    assert_eq!(operations(&lab, 0), before + 2);
    let ui = render(&app);
    assert!(
        said(&ui, &format!("tables/cell/{pk}/label")).is_none(),
        "the live grid leaves a retired row out"
    );
    assert_eq!(
        said(&ui, "tables/sheet/machine").as_deref(),
        Some("Machines: 1 row")
    );

    // Restore, from the pane that still shows it.
    assert_eq!(said(&ui, RESTORE).as_deref(), Some("Restore this machine"));
    act(&mut app, RESTORE, None);
    let ui = render(&app);
    assert_eq!(
        said(&ui, &format!("tables/num/{pk}/n")).as_deref(),
        Some("restore")
    );
    assert_eq!(cell_says(&ui, &pk, "row"), "live, not written");
    act(&mut app, WRITE, None);
    settle_write(&mut app);
    assert_eq!(held(&lab, 0, &MACHINE, &pk, "row"), Some(json!("live")));
    assert_eq!(operations(&lab, 0), before + 3);
    assert_eq!(
        said(&render(&app), "tables/sheet/machine").as_deref(),
        Some("Machines: 2 rows")
    );
}

/// A machine another live machine names (its `merged_into`) is not retired: before anything is
/// staged its pane says how many live rows name it, offers Retire only as a control that takes no
/// press, and lists each of those rows by its words (never its key), which a press shows. The
/// rows are the ones the save's own check refuses such a retire for
/// (`aterm_db::typed::check::referrers`).
/// NEGATIVE CONTROLS: with `TablesApp::stage_life` not asking for the row's referrers
/// (`if false && …`), the retire is staged anyway: `` assertion `left == right` failed: a retire
/// waits for the rows that name it  left: Some("1 change not written") right: None ``. With the
/// pane listing each row by its key (`by.pk`), it reads `` left: Some("Show Machines ·
/// mc-0874bbc304a61ec0") right: Some("Show Machines · m10") ``; with one row's words in the
/// plural, `` left: Some("1 live row names this machine: end or change them before it is
/// retired") ``.
#[test]
fn a_retire_names_the_live_rows_that_still_name_the_row() {
    let lab = two_macs();
    let other = aterm_db::typed::keys::machine_pk("m10");
    Registry::at(lab.paths[0].clone())
        .write(vec![vec![
            put_in(&MACHINE, &other, "label", json!("m10")),
            put_in(&MACHINE, &other, "status", json!("active")),
            put_in(&MACHINE, &other, "merged_into", json!(MC)),
            put_in(&MACHINE, &other, "row", json!("live")),
        ]])
        .unwrap();
    let mut app = App::headless_for_test();
    app.open_tables_tab_at(Some(lab.paths[0].clone())).unwrap();
    settle(&mut app);
    act(&mut app, "tables/sheet/machine", None);
    act(&mut app, &format!("tables/cell/{MC}/label"), None);
    let ui = render(&app);
    assert_eq!(
        said(&ui, RETIRE).as_deref(),
        Some("Retire this machine: 1 live row names it")
    );
    assert_eq!(
        said(&ui, &format!("{RETIRE}/words")).as_deref(),
        Some("1 live row names this machine: end or change that row before it is retired")
    );
    let by = format!("{REFERRER}0");
    assert_eq!(said(&ui, &by).as_deref(), Some("Show Machines \u{b7} m10"));
    act(&mut app, RETIRE, None);
    assert_eq!(
        said(&render(&app), "tables/bar/count"),
        None,
        "a retire waits for the rows that name it"
    );
    act(&mut app, &by, None);
    assert_eq!(said(&render(&app), "tables/pane/id"), Some(other));
}

/// The lifecycle filter steps through the words a row of the table can take: live, the word a
/// removed row takes (`retired` for a machine, `ended` for a link), each other word of the
/// lifecycle column's enum some row holds, conflicted rows where there are any, then all; a word
/// no row of the table can take (a machine's `ended`) is never a step. A retired row shows only
/// under its own word and all, and the rail still counts the live rows. A past point keeps the
/// filter in sight, since it picks that point's rows too.
/// NEGATIVE CONTROLS: with `Filter::next` stepping through every word of the column's enum (as
/// before), a machine's filter steps through words no machine takes: `` left: ["Live rows",
/// "Retired rows", "Ended rows", "Merged rows"] right: ["Live rows", "Retired rows", "All rows",
/// "Live rows"] ``. With the tools line leaving the filter out at a past point (as before), ``
/// the filter stays in sight at a past point left: None right: Some("All rows shown; press for
/// live rows") ``.
#[test]
fn the_lifecycle_filter_steps_through_the_words_a_row_can_take() {
    let lab = two_macs();
    let seeded = Handle::at(lab.paths[0].clone(), &REGISTRY)
        .history()
        .unwrap()
        .last()
        .unwrap()
        .operation;
    let gone = aterm_db::typed::keys::machine_pk("m11");
    Registry::at(lab.paths[0].clone())
        .write(vec![vec![
            put_in(&MACHINE, &gone, "label", json!("m11")),
            put_in(&MACHINE, &gone, "row", json!("retired")),
        ]])
        .unwrap();
    let mut app = App::headless_for_test();
    app.open_tables_tab_at(Some(lab.paths[0].clone())).unwrap();
    settle(&mut app);
    act(&mut app, "tables/sheet/machine", None);
    let listed =
        |app: &App, pk: &str| said(&render(app), &format!("tables/cell/{pk}/label")).is_some();
    let ui = render(&app);
    assert!(listed(&app, MC) && !listed(&app, &gone), "live rows first");
    assert_eq!(
        said(&ui, "tables/sheet/machine").as_deref(),
        Some("Machines: 1 row")
    );
    let mut labels = Vec::new();
    let mut seen = Vec::new();
    for _ in 0..4 {
        let ui = render(&app);
        let spoken = said(&ui, FILTER).unwrap();
        labels.push(spoken.split(" shown").next().unwrap().to_owned());
        seen.push((listed(&app, MC), listed(&app, &gone)));
        act(&mut app, FILTER, None);
    }
    assert_eq!(
        labels,
        ["Live rows", "Retired rows", "All rows", "Live rows"]
    );
    assert_eq!(
        seen,
        [(true, false), (false, true), (true, true), (true, false)]
    );
    assert_eq!(
        said(&render(&app), "tables/sheet/machine").as_deref(),
        Some("Machines: 1 row"),
        "the rail counts live rows"
    );
    // A link ends rather than retires.
    act(&mut app, "tables/sheet/preference", None);
    let filter = |app: &App| said(&render(app), FILTER).unwrap();
    assert_eq!(filter(&app), "Live rows shown; press for ended rows");

    // At a past point the filter still says which rows show (all, one press on from retired).
    act(&mut app, "tables/sheet/machine", None);
    act(&mut app, FILTER, None);
    act(&mut app, &format!("tables/cell/{MC}/label"), None);
    act(
        &mut app,
        &format!(
            "tables/pane/asof/{}",
            aterm_codec::hex::encode_lower(&seeded)
        ),
        None,
    );
    settle(&mut app);
    assert!(said(&render(&app), "tables/asof").is_some(), "a past point");
    assert_eq!(
        said(&render(&app), FILTER).as_deref(),
        Some("All rows shown; press for live rows"),
        "the filter stays in sight at a past point"
    );
}

/// Where the grid adds no row (a provider is a reference row every build compiles), the tools
/// line says who adds one in place of an Add. A new email address's password is never typed:
/// its pane names the verb that sets it, with the row's key once its typed cells make it, and no
/// secret-store entry, since only the verb makes one (a tagged entry in this generation, none
/// in one that seals the password into its cell); its password cell names none either.
/// NEGATIVE CONTROLS: with the read's `Read::life` keeping no column's verb (its `set_by.push`
/// removed), the pane says nothing of the password: `` assertion `left == right` failed: the pane
/// names the verb that sets the password  left: None right: Some("Password is not typed here:
/// once the row is written, aterm db emails set <row> password --stdin sets it") ``. With the
/// pane naming the untagged entry `secret::entry_name` makes (as before), it names an entry the
/// verb never fills: `` left: Some("… password --stdin sets it in
/// alab-address-password-ad-5914422f4413cd85") right: Some("… password --stdin sets it") ``.
/// With `registry::born_elsewhere` quoting the import verb in backticks (as before), they are
/// shown and spoken: `` left: Some("Not added here: `aterm db alab import --apply` adds these
/// rows") ``.
#[test]
fn a_new_rows_secret_is_named_by_its_verb_and_a_table_says_who_adds_its_rows() {
    let lab = Lab::new(&[Role::Writer]);
    // The generation that names store entries: the newest, which no roll of this build reaches.
    Registry::at(lab.paths[0].clone())
        .open_newest(1_800_000_000_000)
        .unwrap();
    repair_commons(&lab, 4);
    let mut app = App::headless_for_test();
    app.open_tables_tab_at(Some(lab.paths[0].clone())).unwrap();
    settle(&mut app);

    act(&mut app, "tables/sheet/provider", None);
    let ui = render(&app);
    assert!(
        said(&ui, ADD).is_none(),
        "no Add where the grid adds no row"
    );
    assert_eq!(
        said(&ui, "tables/rows/elsewhere").as_deref(),
        Some("Not added here: every build writes these from its reference rows")
    );
    act(&mut app, "tables/sheet/source", None);
    assert_eq!(
        said(&render(&app), "tables/rows/elsewhere").as_deref(),
        Some("Not added here: aterm db alab import --apply adds these rows"),
        "a verb is named in plain words, no backticks"
    );

    act(&mut app, "tables/sheet/address", None);
    act(&mut app, ADD, None);
    let set = "tables/pane/row/set/password";
    assert_eq!(
        said(&render(&app), set).as_deref(),
        Some(
            "Password is not typed here: once the row is written, aterm db emails set <row> \
             password --stdin sets it"
        ),
        "the pane names the verb that sets the password"
    );
    const ADDR: &str = "bo.synthetic@example.com";
    type_in(&mut app, FIRST, "addr", ADDR);
    type_in(&mut app, FIRST, "domain", "example.com");
    type_in(&mut app, FIRST, "origin", "owner");
    let key = aterm_db::typed::keys::address_pk(ADDR).unwrap();
    let ui = render(&app);
    assert_eq!(
        said(&ui, set),
        Some(format!(
            "Password is not typed here: once the row is written, aterm db emails set {key} \
             password --stdin sets it"
        ))
    );
    assert_eq!(cell_says(&ui, FIRST, "password"), "empty");
    act(&mut app, &format!("tables/cell/{FIRST}/password"), None);
    act(&mut app, &format!("tables/cell/{FIRST}/password"), None);
    assert!(
        said(&render(&app), EDIT).is_none(),
        "a new row's secret never opens"
    );
}

/// A Mac that is not a Commons writer reads the registry but takes no edit: the Write bar's
/// place says why in one sentence, no Add is offered, and no cell opens.
/// NEGATIVE CONTROL: with `TablesApp::fixed` not asking `cannot_write`, the page offers an Add
/// this Mac could never write: `` no Add ``.
#[test]
fn a_mac_that_cannot_write_says_why_in_the_write_bars_place() {
    let lab = Lab::new(&[Role::Writer, Role::Reader]);
    let here = Registry::at(lab.paths[0].clone());
    here.open().unwrap();
    repair_commons(&lab, 4);
    here.write(vec![vec![
        put_in(&MACHINE, MC, "label", json!("m9")),
        put_in(&MACHINE, MC, "status", json!("active")),
        put_in(&MACHINE, MC, "row", json!("live")),
    ]])
    .unwrap();
    repair_commons(&lab, 4);
    let mut app = App::headless_for_test();
    app.open_tables_tab_at(Some(lab.paths[1].clone())).unwrap();
    settle(&mut app);
    act(&mut app, "tables/sheet/machine", None);
    let ui = render(&app);
    assert_eq!(cell_says(&ui, MC, "label"), "m9", "the reader reads it");
    assert_eq!(
        said(&ui, "tables/bar/cannot").as_deref(),
        Some("This machine cannot write the registry: it is not a Commons writer here")
    );
    assert!(said(&ui, ADD).is_none(), "no Add");
    let cell = format!("tables/cell/{MC}/label");
    act(&mut app, &cell, None);
    act(&mut app, &cell, None);
    let ui = render(&app);
    assert!(
        said(&ui, EDIT).is_none(),
        "a Mac that cannot write opens no cell"
    );
    assert!(said(&ui, RETIRE).is_none(), "nor offers a retire");
}

/// Open row `pk`'s `column`: a press selects it, a second opens it.
fn open(app: &mut App, pk: &str, column: &str) {
    let cell = format!("tables/cell/{pk}/{column}");
    act(app, &cell, None);
    if said(&render(app), EDIT).is_none() {
        act(app, &cell, None);
    }
    assert!(said(&render(app), EDIT).is_some(), "{pk} {column} opened");
}

/// What each choice of the open list says, in its order.
fn choices(ui: &CompiledUi) -> Vec<String> {
    (0..)
        .map_while(|n| said(ui, &format!("{CHOICE}{n}")))
        .collect()
}

/// The role node `key` is spoken with.
fn role(ui: &CompiledUi, key: &str) -> Option<SemanticRole> {
    ui.semantic(&UiKey::new(key.to_owned()))
        .map(|node| node.role)
}

/// A Mac registered here (its row held by its own facts row) and another machine row written
/// under the same label that no Mac holds, as a split seat leaves them (docs/KNOWN_ISSUES.md
/// ATERM-KI-1112-155), with an account to link: the held row's key and label, the split one's
/// key and the label it reads as (the label claim's clash label, `m1-7765`), the held row's
/// status and the account's key.
fn split_seat(lab: &Lab) -> (String, String, String, String, String, String) {
    let here = Registry::at(lab.paths[0].clone());
    here.open().unwrap();
    here.seed().unwrap();
    aterm_secrets::client::admin_seed_swarm_roster(&lab.paths[0], &[]).unwrap();
    let probe = aterm_db::alab::register::Probe {
        hw_model: "Mac16,8".into(),
        hosts: vec!["mac-one".into()],
        node: Some(format!("{:016x}", 1)),
        platform_uuid: "C3396674-FCD3-52E5-A1C1-BD5B9E8F5C40".into(),
        ..aterm_db::alab::register::Probe::default()
    };
    here.register(
        &lab.paths[0].home.join("config.toml"),
        &probe,
        true,
        aterm_time::unix_millis().unwrap(),
    )
    .unwrap();
    let view = Handle::at(lab.paths[0].clone(), &REGISTRY).view().unwrap();
    let held_pk = view.owned["machine-facts"].rows[&aterm_db::typed::keys::uuid32(&lab.machine(0))]
        .value["mac"]
        .as_str()
        .expect("the facts bind a machine")
        .to_owned();
    let row = &view.project(&MACHINE).rows[&held_pk];
    let text = |column: &str| row.get(column).and_then(|v| v.as_str()).unwrap().to_owned();
    let (label, status) = (text("label"), text("status"));
    let split = aterm_db::typed::keys::machine_pk("split-seat");
    let (ad, ac) = ("ad-0000000000000001", "ac-0000000000000001");
    here.write(vec![vec![
        put_in(&MACHINE, &split, "label", json!(label)),
        put_in(&MACHINE, &split, "status", json!("planned")),
        put_in(&MACHINE, &split, "row", json!("live")),
        put_in(&ADDRESS, ad, "addr", json!("owner@example.invalid")),
        put_in(&ADDRESS, ad, "domain", json!("example.invalid")),
        put_in(&ADDRESS, ad, "row", json!("live")),
        put_in(&ACCOUNT, ac, "provider", json!("openai")),
        put_in(&ACCOUNT, ac, "realm", json!("global")),
        put_in(&ACCOUNT, ac, "login", json!(ad)),
        put_in(&ACCOUNT, ac, "status", json!("active")),
        put_in(&ACCOUNT, ac, "row", json!("live")),
    ]])
    .unwrap();
    let reads = Handle::at(lab.paths[0].clone(), &REGISTRY)
        .view()
        .unwrap()
        .project(&MACHINE)
        .rows[&split]
        .get("label")
        .and_then(|v| v.as_str())
        .unwrap()
        .to_owned();
    (held_pk, label, split, reads, status, ac.to_owned())
}

/// A link's machine and account are chosen from their tables' rows, in a list box whose options
/// are spoken by each row's words: two machines under one label each say their key's ending,
/// their status and whether a Mac holds them, and an account says its provider word and address.
/// The chosen row's key is staged and written, never the label; the grid shows the row's words,
/// not its key, and the pane offers the row it names. Words typed that name no row are refused at
/// their cell, and nothing of the link is written until its row is chosen. Its history and its
/// pane's title name the machine by its words and key ending alone, which its status now does
/// not change.
/// NEGATIVE CONTROLS: with `rows::choice` answering `None` (no list for a reference), the cell
/// opens as a text field: `` assertion `left == right` failed: a reference opens on a list box
/// left: None right: Some(ListBox) ``; with `rows::name_references` leaving every cell as read,
/// a login reads as its key: `` left: "ad-0000000000000001" right: "owner@example.invalid" ``;
/// with `rows::targets` telling rows apart by the words they read as (the label claim's) rather
/// than the label they were written under, the two machines say nothing of which is which:
/// `` each row written under one label says which it is left: ["m1", "m1-7765"] right: ["m1 ·
/// …fb03 · planned · a Mac holds it", "m1-7765 · …a4f6 · planned · no Mac holds it"] ``; with
/// `TablesApp::recheck` leaving out `reference_refusals`, words naming no row stand unrefused
/// at their cell: `` left: "owner, not written" ``; with `TablesApp::recheck` keeping a new row's
/// key refusal beside the refused cell it comes of, the row is refused twice, the second in the
/// key's own words: `` left: "live, new, not written, refused: a preference names a machine and
/// an account" right: "live, new, not written" ``; with `rows::name_references` reading a
/// history's reference as it reads now (`said`, not `short`), the line carries the machine's
/// status today: `` ["Row → added", "Machine: empty → m1-7765 · …a4f6 · planned ·…", …] ``.
#[test]
fn a_reference_is_chosen_from_its_rows_and_stored_by_key() {
    let lab = Lab::new(&[Role::Writer]);
    let (held_pk, label, split, split_label, status, ac) = split_seat(&lab);
    let ending = |pk: &str, other: &str| super::ending(pk, &[pk, other]).to_owned();
    let held_says = format!(
        "{label} \u{b7} \u{2026}{} \u{b7} {status} \u{b7} a machine holds it",
        ending(&held_pk, &split)
    );
    let split_says = format!(
        "{split_label} \u{b7} \u{2026}{} \u{b7} planned \u{b7} no machine holds it",
        ending(&split, &held_pk)
    );
    let account_says = "openai \u{b7} owner@example.invalid";
    let mut app = App::headless_for_test();
    app.open_tables_tab_at(Some(lab.paths[0].clone())).unwrap();
    settle(&mut app);

    // The account's login reads as its address, not its key.
    act(&mut app, "tables/sheet/account", None);
    assert_eq!(
        cell_says(&render(&app), &ac, "login"),
        "owner@example.invalid"
    );

    act(&mut app, "tables/sheet/preference", None);
    act(&mut app, ADD, None);
    open(&mut app, FIRST, "machine");
    let ui = render(&app);
    assert_eq!(
        role(&ui, "tables/choices/list"),
        Some(SemanticRole::ListBox),
        "a reference opens on a list box"
    );
    assert_eq!(
        said(&ui, "tables/choices/list").as_deref(),
        Some("Choices for Machine")
    );
    assert_eq!(
        choices(&ui),
        [held_says.clone(), split_says.clone()],
        "each row written under one label says which it is"
    );
    let at = choices(&ui).iter().position(|s| *s == split_says).unwrap();
    let option = format!("{CHOICE}{at}");
    assert_eq!(role(&ui, &option), Some(SemanticRole::ListBoxOption));
    act(&mut app, &option, None);
    let ui = render(&app);
    assert_eq!(
        cell_says(&ui, FIRST, "machine"),
        format!("{split_says}, not written"),
        "the grid shows the chosen row, not its key"
    );

    // Words typed that name no row mark none: they stand as typed and are refused at their
    // cell, as the save refuses a reference to a missing row, and the Write holds it back.
    let before = operations(&lab, 0);
    open(&mut app, FIRST, "account");
    act(&mut app, EDIT, Some(SemanticInput::Text("owner".into())));
    key_in(&mut app, TextInputEvent::Submit);
    type_in(&mut app, FIRST, "why", "owner");
    let ui = render(&app);
    assert_eq!(
        cell_says(&ui, FIRST, "account"),
        "owner, not written, refused: names no row of AI accounts; choose one from its list",
        "no link is keyed by words"
    );
    assert_eq!(cell_says(&ui, FIRST, "row"), "live, new, not written");
    act(&mut app, WRITE, None);
    assert_eq!(operations(&lab, 0), before, "nothing is written");

    open(&mut app, FIRST, "account");
    let ui = render(&app);
    assert_eq!(
        choices(&ui),
        [account_says],
        "an account by provider and address"
    );
    act(&mut app, &format!("{CHOICE}0"), None);
    let ui = render(&app);
    assert_eq!(
        cell_says(&ui, FIRST, "account"),
        format!("{account_says}, not written")
    );
    assert_eq!(
        said(&ui, "tables/bar/count").as_deref(),
        Some("3 changes not written")
    );

    act(&mut app, WRITE, None);
    settle_write(&mut app);
    let pk = aterm_db::typed::keys::preference_pk(&split, &ac).unwrap();
    assert_eq!(
        held(&lab, 0, &PREFERENCE, &pk, "machine"),
        Some(json!(split)),
        "the link names the chosen row by its key"
    );
    assert_eq!(held(&lab, 0, &PREFERENCE, &pk, "account"), Some(json!(ac)));
    let ui = render(&app);
    assert_eq!(cell_says(&ui, &pk, "machine"), split_says);
    assert_eq!(cell_says(&ui, &pk, "account"), account_says);
    // Its history names the machine by what stays of it, not its status now.
    act(&mut app, &format!("tables/cell/{pk}/machine"), None);
    let ui = render(&app);
    let history: Vec<String> = (0..8)
        .filter_map(|m| said(&ui, &format!("tables/pane/written/0/{m}")))
        .collect();
    let split_short = format!("{split_label} \u{b7} \u{2026}{}", ending(&split, &held_pk));
    assert!(
        history.contains(&format!("Machine: empty \u{2192} {split_short}")),
        "{history:?}"
    );
    assert_eq!(
        said(&ui, "tables/pane/title").as_deref(),
        Some(split_short.as_str()),
        "the pane's title names the row by what stays of it"
    );

    // The pane offers the row the reference names, and a press shows it.
    act(&mut app, &format!("tables/cell/{pk}/machine"), None);
    let to = "tables/pane/row/to/machine";
    assert_eq!(
        said(&render(&app), to),
        Some(format!("Show {split_says} in Machines"))
    );
    act(&mut app, to, None);
    let ui = render(&app);
    assert_eq!(said(&ui, "tables/pane/id"), Some(split.clone()));
    assert_eq!(cell_says(&ui, &split, "label"), split_label);
}

/// What is staged in `column` of row `pk` of `sheet`, where something is.
fn staged_of(app: &mut App, sheet: &str, pk: &str, column: &str) -> Option<String> {
    app.native_runtime
        .tables_mut()
        .expect("the Tables page is open")
        .staged
        .get(&(sheet.to_owned(), pk.to_owned(), column.to_owned()))
        .cloned()
}

/// The live rows of any table that name row `pk` of `collection`, as Mac 0 of `lab` reads them
/// (`aterm_db::typed::check::referrers`).
fn live_referrers(lab: &Lab, collection: &'static str, pk: &str) -> Vec<(&'static str, String)> {
    let handle = Handle::at(lab.paths[0].clone(), &REGISTRY);
    let view = handle.view().unwrap();
    aterm_db::typed::check::referrers(handle.compiled().table, &[], &view.cells)
        .remove(&(collection, pk.to_owned()))
        .unwrap_or_default()
}

/// A row a change staged here retires is no row a new reference is chosen from, and its key
/// typed whole is refused at the cell, by the rule the save refuses a live row naming an ended
/// one with (`aterm_db::typed::check::names_not_live`): one Write never retires a machine in one
/// table's save and lands a live link to it in the next. The retire is written and the link is
/// held back, so no live row names the retired machine.
/// NEGATIVE CONTROLS: with `rows::choice` listing a row whose retire is staged
/// (`!app.ends_staged` dropped), the retired machine is still offered: `` the machine staged to
/// retire is not offered: ["m1 · …fb03 · planned · a Mac holds it", "m1-7765 · …a4f6 · planned ·
/// no Mac holds it"] ``; with `TablesApp::recheck` leaving out `reference_refusals`, the typed
/// key stands unrefused: `` m1-7765 · …a4f6 · planned · no Mac holds it, not written ``.
#[test]
fn a_row_staged_to_retire_is_no_row_a_new_reference_names() {
    let lab = Lab::new(&[Role::Writer]);
    let (_held_pk, _label, split, split_label, _status, ac) = split_seat(&lab);
    let mut app = App::headless_for_test();
    app.open_tables_tab_at(Some(lab.paths[0].clone())).unwrap();
    settle(&mut app);
    act(&mut app, "tables/sheet/machine", None);
    act(&mut app, &format!("tables/cell/{split}/label"), None);
    act(&mut app, RETIRE, None);
    assert_eq!(
        cell_says(&render(&app), &split, "row"),
        "retired, not written"
    );

    act(&mut app, "tables/sheet/preference", None);
    act(&mut app, ADD, None);
    open(&mut app, FIRST, "machine");
    let offered = choices(&render(&app));
    assert!(
        !offered.iter().any(|c| c.starts_with(&split_label)),
        "the machine staged to retire is not offered: {offered:?}"
    );
    act(&mut app, EDIT, Some(SemanticInput::Text(split.clone())));
    key_in(&mut app, TextInputEvent::Submit);
    let says = cell_says(&render(&app), FIRST, "machine");
    assert!(
        says.contains(", refused: Machines row ")
            && says.ends_with(" is retired once written; a live row names only live rows"),
        "{says}"
    );
    open(&mut app, FIRST, "account");
    act(&mut app, &format!("{CHOICE}0"), None);
    type_in(&mut app, FIRST, "why", "owner");
    act(&mut app, WRITE, None);
    settle_write(&mut app);
    assert_eq!(
        held(&lab, 0, &MACHINE, &split, "row"),
        Some(json!("retired"))
    );
    let link = aterm_db::typed::keys::preference_pk(&split, &ac).unwrap();
    assert_eq!(
        held(&lab, 0, &PREFERENCE, &link, "row"),
        None,
        "no link is written"
    );
    assert_eq!(live_referrers(&lab, "machine", &split), []);
}

/// Words typed into a reference's cell mark a row only where they are that row's words whole,
/// or its key: two machines written under one label read `m1 · …fb03 · …` and `m1-7765 · …`, so
/// `m1`, or `m`, names neither and is staged as typed, refused at the cell, never the key of the
/// first row whose words start with it. A row's words typed whole stage its key.
/// NEGATIVE CONTROL: with `Choice::follow` marking the first row whose words start with what was
/// typed in a list of rows too (`whole` ignored), `m1` stages the row a Mac holds: `` left:
/// Some("mc-2042463f4ef5fb03") right: Some("m1") ``.
#[test]
fn words_that_start_two_rows_words_name_neither() {
    let lab = Lab::new(&[Role::Writer]);
    let (held_pk, label, split, _split_label, status, _ac) = split_seat(&lab);
    let held_says = format!(
        "{label} \u{b7} \u{2026}{} \u{b7} {status} \u{b7} a machine holds it",
        super::ending(&held_pk, &[&held_pk, &split])
    );
    let mut app = App::headless_for_test();
    app.open_tables_tab_at(Some(lab.paths[0].clone())).unwrap();
    settle(&mut app);
    act(&mut app, "tables/sheet/preference", None);
    act(&mut app, ADD, None);
    for typed in [label.as_str(), "m"] {
        open(&mut app, FIRST, "machine");
        act(&mut app, EDIT, Some(SemanticInput::Text(typed.to_owned())));
        key_in(&mut app, TextInputEvent::Submit);
        assert_eq!(
            staged_of(&mut app, "preference", FIRST, "machine").as_deref(),
            Some(typed)
        );
        assert_eq!(
            cell_says(&render(&app), FIRST, "machine"),
            format!(
                "{typed}, not written, refused: names no row of Machines; choose one from its \
                 list"
            )
        );
    }
    open(&mut app, FIRST, "machine");
    act(&mut app, EDIT, Some(SemanticInput::Text(held_says.clone())));
    key_in(&mut app, TextInputEvent::Submit);
    assert_eq!(
        staged_of(&mut app, "preference", FIRST, "machine"),
        Some(held_pk),
        "a row's words typed whole stage its key"
    );
}

/// A cell of a new row changed while its Write is out is an edit of the row that Write adds,
/// never a row of its own: one Add is one phone, and the next Write sets the changed cell in it.
/// While the Write is out the page renders, its Write button spoken by the words it shows. A new
/// phone's pane names no key, since a phone's is made at random as it is written.
/// NEGATIVE CONTROLS: with `TablesApp::wrote` keeping a new row whose cell is still staged (as
/// before), a second phone is born: `` assertion `left == right` failed: one Add, one phone
/// left: 2 right: 1 ``. With the Write button spoken `Write …` while it shows `Writing…`, the
/// page fails its check under a Write: `` VisibleLabelNotInName(UiKey("tables/write")) ``. With
/// the save's reading keeping a phone's derived key (no `born_at_random` arm), the pane names a
/// key the Write never uses: `` a random key is not named before it is made left: Some("Number
/// is not typed here: once the row is written, aterm db phones set ph-27e3c7a014b8d8b5 number
/// <number> sets it") ``.
#[test]
fn a_new_rows_cell_changed_while_it_is_written_edits_the_row_written() {
    let lab = two_macs();
    let phones = || {
        Handle::at(lab.paths[0].clone(), &REGISTRY)
            .view()
            .unwrap()
            .project(&aterm_db::alab::registry::SEALED_PHONE)
            .rows
    };
    let before = phones().len();
    let mut app = App::headless_for_test();
    app.open_tables_tab_at(Some(lab.paths[0].clone())).unwrap();
    settle(&mut app);
    act(&mut app, "tables/sheet/phone", None);
    act(&mut app, ADD, None);
    type_in(&mut app, FIRST, "last4", "1234");
    assert_eq!(
        said(&render(&app), "tables/pane/row/set/number").as_deref(),
        Some(
            "Number is not typed here: once the row is written, aterm db phones set <row> \
             number <number> sets it"
        ),
        "a random key is not named before it is made"
    );
    act(&mut app, WRITE, None);
    let ui = render(&app);
    assert!(
        said(&ui, WRITE).is_some_and(|w| w.starts_with("Writing\u{2026} ")),
        "{:?}",
        said(&ui, WRITE)
    );
    app.native_runtime
        .tables_mut()
        .unwrap()
        .stage("phone", FIRST, "last4", "5678".to_owned());
    settle_write(&mut app);
    assert_eq!(phones().len(), before + 1, "one Add, one phone");
    assert_eq!(
        said(&render(&app), "tables/bar/count").as_deref(),
        Some("1 change not written")
    );
    act(&mut app, WRITE, None);
    settle_write(&mut app);
    let now = phones();
    assert_eq!(now.len(), before + 1, "one Add, one phone");
    assert!(
        now.values()
            .any(|row| row.get("last4") == Some(&json!("5678"))),
        "the change made during the Write is written to the phone it added"
    );
}

/// A row with no lifecycle written is live, as the registry reads it
/// (`aterm_db::typed::cell::Row::live`): the live grid and the rail count it, a reference is
/// chosen from it, and its pane offers Retire.
/// NEGATIVE CONTROL: with `Filter::admits` asking the lifecycle cell to say `live` (as before),
/// the row shows only under all: `` the live grid lists it ``.
#[test]
fn a_row_with_no_lifecycle_written_is_live() {
    let lab = two_macs();
    let bare = aterm_db::typed::keys::machine_pk("m11");
    Registry::at(lab.paths[0].clone())
        .write(vec![vec![
            put_in(&MACHINE, &bare, "label", json!("m11")),
            put_in(&MACHINE, &bare, "status", json!("active")),
        ]])
        .unwrap();
    let mut app = App::headless_for_test();
    app.open_tables_tab_at(Some(lab.paths[0].clone())).unwrap();
    settle(&mut app);
    act(&mut app, "tables/sheet/machine", None);
    let ui = render(&app);
    assert!(
        said(&ui, &format!("tables/cell/{bare}/label")).is_some(),
        "the live grid lists it"
    );
    assert_eq!(
        said(&ui, "tables/sheet/machine").as_deref(),
        Some("Machines: 2 rows")
    );
    act(&mut app, &format!("tables/cell/{bare}/label"), None);
    assert_eq!(
        said(&render(&app), RETIRE).as_deref(),
        Some("Retire this machine")
    );
    act(&mut app, "tables/sheet/preference", None);
    act(&mut app, ADD, None);
    open(&mut app, FIRST, "machine");
    assert!(
        choices(&render(&app)).contains(&"m11".to_owned()),
        "{:?}",
        choices(&render(&app))
    );
}

/// A row whose lifecycle two Macs wrote apart is settled from its pane: each value offers a Keep,
/// which stages it, and Write writes it, as any conflicted cell is settled.
/// NEGATIVE CONTROL: with `TablesApp::keepable` asking only `takes` (as before), which no
/// lifecycle passes since no grid cell types it, no value offers a Keep: `` the pane offers a
/// Keep for each value ``.
#[test]
fn a_conflicted_lifecycle_is_settled_by_keeping_one_of_its_values() {
    let lab = two_macs();
    Registry::at(lab.paths[0].clone())
        .write(vec![vec![put_in(&MACHINE, MC, "row", json!("retired"))]])
        .unwrap();
    Registry::at(lab.paths[1].clone())
        .write(vec![vec![put_in(&MACHINE, MC, "row", json!("locked"))]])
        .unwrap();
    repair_commons(&lab, 4);
    let mut app = App::headless_for_test();
    app.open_tables_tab_at(Some(lab.paths[0].clone())).unwrap();
    settle(&mut app);
    act(&mut app, "tables/sheet/machine", None);
    for _ in 0..8 {
        if said(&render(&app), FILTER).is_some_and(|f| f.starts_with("Conflicted rows")) {
            break;
        }
        act(&mut app, FILTER, None);
    }
    act(&mut app, &format!("tables/cell/{MC}/label"), None);
    let ui = render(&app);
    assert_eq!(
        said(&ui, "tables/pane/row/state").as_deref(),
        Some("This machine's lifecycle is conflicted: keep one of its values below")
    );
    let keep = said(&ui, "tables/pane/keep/row/0").expect("the pane offers a Keep for each value");
    assert!(
        said(&ui, "tables/pane/keep/row/1").is_some(),
        "the pane offers a Keep for each value"
    );
    let word = keep
        .strip_prefix("Keep ")
        .and_then(|rest| rest.split(" for ").next())
        .unwrap()
        .to_owned();
    act(&mut app, "tables/pane/keep/row/0", None);
    act(&mut app, WRITE, None);
    settle_write(&mut app);
    assert_eq!(held(&lab, 0, &MACHINE, MC, "row"), Some(json!(word)));
}

/// A retired row that still names a retired row is not restored: its pane says why and offers
/// Restore only as a control that takes no press, since the save refuses a live row naming one
/// that has ended (`aterm_db::typed::check::names_not_live`).
/// NEGATIVE CONTROL: with `TablesApp::stage_life` and the pane not asking `named_ended`, Restore
/// is offered as if nothing held it back: `` left: Some("This machine is retired; Restore makes
/// it live again") right: Some("This machine is retired. Not restored: Machines row m9 is
/// retired; a live row names only live rows") ``.
#[test]
fn a_row_naming_a_retired_row_is_not_restored() {
    let lab = two_macs();
    let other = aterm_db::typed::keys::machine_pk("m10");
    let here = Registry::at(lab.paths[0].clone());
    here.write(vec![vec![
        put_in(&MACHINE, &other, "label", json!("m10")),
        put_in(&MACHINE, &other, "status", json!("active")),
        put_in(&MACHINE, &other, "merged_into", json!(MC)),
        put_in(&MACHINE, &other, "row", json!("retired")),
    ]])
    .unwrap();
    here.write(vec![vec![put_in(&MACHINE, MC, "row", json!("retired"))]])
        .unwrap();
    let mut app = App::headless_for_test();
    app.open_tables_tab_at(Some(lab.paths[0].clone())).unwrap();
    settle(&mut app);
    act(&mut app, "tables/sheet/machine", None);
    act(&mut app, FILTER, None);
    act(&mut app, &format!("tables/cell/{other}/label"), None);
    assert_eq!(
        said(&render(&app), &format!("{RESTORE}/words")).as_deref(),
        Some(
            "This machine is retired. Not restored: Machines row m9 is retired; a live row names \
             only live rows"
        )
    );
    act(&mut app, RESTORE, None);
    app.native_runtime.tables_mut().unwrap().stage_life(
        "machine",
        &other,
        aterm_db::typed::grid::LIVE,
    );
    assert_eq!(said(&render(&app), "tables/bar/count"), None);
}

/// A new row's key column left empty is refused at its own cell, once: a link with its machine
/// chosen and its account left empty says so at the account (two refusals, the account and the
/// why), never at the row's lifecycle, which no one types. Its pane says it is born live, as the
/// grid does, rather than listing its lifecycle as empty.
/// NEGATIVE CONTROLS: with `tables_store::placed` saying every key refusal at the lifecycle cell
/// (as before), the account says only the grid's rule, the key's going to the row: `` left:
/// "empty, refused: a new row needs a value here" right: "empty, refused: a new row's key is
/// made from this column; fill it in" ``. With `address_runs` naming a new row by its key
/// (as before), the cell bar shows the page's own key for it: `` left: Some("+new-1 · Machine")
/// right: Some("New link · Machine") ``.
/// With the pane leaving a new row's lifecycle out (no `born_live`), it lists it as empty:
/// `` left: None right: Some("live") ``.
#[test]
fn a_new_rows_key_column_left_empty_is_refused_at_its_own_cell() {
    let lab = Lab::new(&[Role::Writer]);
    split_seat(&lab);
    let mut app = App::headless_for_test();
    app.open_tables_tab_at(Some(lab.paths[0].clone())).unwrap();
    settle(&mut app);
    act(&mut app, "tables/sheet/preference", None);
    act(&mut app, ADD, None);
    assert_eq!(
        said(&render(&app), "tables/pane/field/row/value").as_deref(),
        Some("live")
    );
    open(&mut app, FIRST, "machine");
    act(&mut app, &format!("{CHOICE}0"), None);
    let ui = render(&app);
    assert_eq!(
        said(&ui, "tables/bar/count").as_deref(),
        Some("1 change not written \u{b7} 2 refused")
    );
    assert_eq!(
        cell_says(&ui, FIRST, "account"),
        "empty, refused: a new row's key is made from this column; fill it in"
    );
    assert_eq!(cell_says(&ui, FIRST, "row"), "live, new, not written");
    // The cell bar names the row as its pane does, never by the page's own key for it.
    assert_eq!(
        said(&ui, "tables/cellbar/address").as_deref(),
        Some("New link \u{b7} Machine")
    );
}

/// Ending the link that names a machine frees the machine's Retire in the same Write: the pane
/// lists the link by its words, its End is staged, the machine then offers Retire, and one Write
/// writes both, saving the links before the machines so the retire's save finds the link ended
/// (`aterm_db::typed::check::retired_referenced`).
/// NEGATIVE CONTROLS: with `TablesApp::blocking` not leaving out a row a staged change ends, the
/// Retire still waits: `` left: Some("Retire this machine: 1 live row names it") right:
/// Some("Retire this machine") ``. With `TablesApp::write` not ordering the saves (`rows::order`
/// left out), the machines are saved first and the retire is refused: `` left:
/// Some(String("live")) right: Some(String("retired")) ``.
#[test]
fn ending_the_link_that_names_a_machine_frees_its_retire() {
    let lab = Lab::new(&[Role::Writer]);
    let (_held_pk, _label, split, _split_label, _status, ac) = split_seat(&lab);
    let link = aterm_db::typed::keys::preference_pk(&split, &ac).unwrap();
    Registry::at(lab.paths[0].clone())
        .write(vec![vec![
            put_in(&PREFERENCE, &link, "machine", json!(split)),
            put_in(&PREFERENCE, &link, "account", json!(ac)),
            put_in(&PREFERENCE, &link, "why", json!("owner")),
            put_in(&PREFERENCE, &link, "row", json!("live")),
        ]])
        .unwrap();
    let mut app = App::headless_for_test();
    app.open_tables_tab_at(Some(lab.paths[0].clone())).unwrap();
    settle(&mut app);
    act(&mut app, "tables/sheet/machine", None);
    act(&mut app, &format!("tables/cell/{split}/label"), None);
    let ui = render(&app);
    assert_eq!(
        said(&ui, RETIRE).as_deref(),
        Some("Retire this machine: 1 live row names it")
    );
    let by = format!("{REFERRER}0");
    assert_eq!(
        said(&ui, &by).as_deref(),
        Some("Show Links \u{b7} openai \u{b7} owner@example.invalid"),
        "the link by its words, less the machine it names"
    );
    act(&mut app, &by, None);
    assert_eq!(said(&render(&app), "tables/pane/id"), Some(link.clone()));
    act(&mut app, RETIRE, None);
    assert_eq!(cell_says(&render(&app), &link, "row"), "ended, not written");

    act(&mut app, "tables/sheet/machine", None);
    act(&mut app, &format!("tables/cell/{split}/label"), None);
    assert_eq!(
        said(&render(&app), RETIRE).as_deref(),
        Some("Retire this machine")
    );
    act(&mut app, RETIRE, None);
    act(&mut app, WRITE, None);
    settle_write(&mut app);
    assert_eq!(
        held(&lab, 0, &PREFERENCE, &link, "row"),
        Some(json!("ended"))
    );
    assert_eq!(
        held(&lab, 0, &MACHINE, &split, "row"),
        Some(json!("retired"))
    );
}
