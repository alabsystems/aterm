// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Tables page on a scratch registry (`aterm_db::table::fixture::Lab`: an isolated home, no
//! live vault, fleet or Commons) and on hand-made reads. Every value is synthetic.

use super::*;
use crate::native_app::{ActionInvocation, AppViewState, ViewMotionCx};
use crate::native_ui::{CompiledUi, LogicalRect};
use crate::{App, WindowId};
use aterm_db::alab::{Registry, registry::MACHINE};
use aterm_db::att::{Handle, REGISTRY};
use aterm_db::table::fixture::{Lab, repair_commons};
use aterm_db::typed::cell::{self, CellKey, CellValue};
use aterm_json::json;
use aterm_secrets::commons::roster::Role;

const WID: WindowId = WindowId(0);
const MC: &str = "mc-0000000000000009";

fn put(column: &str, value: aterm_json::Value) -> aterm_db::database::Change {
    cell::change(
        &MACHINE,
        CellKey {
            pk: MC,
            col: column,
            elem: None,
        },
        &CellValue::owner(MACHINE.version, value),
    )
    .unwrap()
}

const VIEWPORT: LogicalRect = LogicalRect::new(0.0, 0.0, 1400.0, 900.0);

fn tree(app: &App) -> UiTree {
    let (instance, view) = app
        .active_native_view(WID)
        .expect("the Tables tab is in front");
    let cx = ViewCx {
        viewport: VIEWPORT,
        animation_phase_ms: 0,
        motion: ViewMotionCx::default(),
        terminal_font_px: 16.0,
        terminal_scale: 1.0,
        semantic_font: None,
        document: None,
    };
    app.native_runtime.render(instance, view, &cx).unwrap()
}

fn render(app: &App) -> CompiledUi {
    tree(app).compile(VIEWPORT).unwrap()
}

/// What node `key` says: a text's words, a control's spoken label.
fn said(ui: &CompiledUi, key: &str) -> Option<String> {
    let node = ui.semantic(&UiKey::new(key.to_owned()))?;
    Some(match &node.value {
        SemanticValue::Text(text) if !text.is_empty() => text.clone(),
        _ => node.label.clone(),
    })
}

/// What a grid cell says after its column and row: its text, then `, 2 values` or
/// `, not written` where it is either.
fn cell_says(ui: &CompiledUi, pk: &str, column: &str) -> String {
    let spoken = said(ui, &format!("tables/cell/{pk}/{column}")).expect("the cell is listed");
    spoken
        .split_once(": ")
        .map(|(_, rest)| rest.to_owned())
        .unwrap_or_default()
}

fn act(app: &mut App, key: &str, value: Option<SemanticInput>) {
    app.dispatch_native_event(
        WID,
        AppEvent::Action(ActionInvocation {
            id: ActionId::new(key.to_owned()),
            value,
        }),
    )
    .expect("the page takes the press");
}

fn key_in(app: &mut App, event: TextInputEvent) {
    app.dispatch_native_event(WID, AppEvent::TextInput(event))
        .expect("the page takes the key");
}

fn settle(app: &mut App) {
    app.native_runtime
        .tables_mut()
        .expect("the Tables page is open")
        .settle();
}

fn settle_write(app: &mut App) {
    app.native_runtime
        .tables_mut()
        .expect("the Tables page is open")
        .settle_write();
}

fn location(lab: &Lab, mac: usize) -> Option<aterm_json::Value> {
    Handle::at(lab.paths[mac].clone(), &REGISTRY)
        .view()
        .unwrap()
        .project(&MACHINE)
        .rows[MC]
        .get("location")
        .cloned()
}

/// Two writing Macs, the row written by the second and carried to the first.
fn two_macs() -> Lab {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let here = Registry::at(lab.paths[0].clone());
    here.open().unwrap();
    let there = Registry::at(lab.paths[1].clone());
    there.open().unwrap();
    repair_commons(&lab, 4);
    there
        .write(vec![vec![
            put("label", json!("m9")),
            put("status", json!("active")),
            put("location", json!("rack a")),
            put("row", json!("live")),
        ]])
        .unwrap();
    repair_commons(&lab, 4);
    assert_eq!(
        location(&lab, 0),
        Some(json!("rack a")),
        "the row reached the first Mac"
    );
    lab
}

/// The page lists the registry's stored tables with their rows, and a cell edited in the grid
/// is written by one Write through the registry's save, after which the row's history names the
/// operation that wrote it and the Mac it came from.
/// NEGATIVE CONTROL: with `TablesApp::write` sending nothing to its reader (its send removed),
/// the staged cell is never written and the registry keeps the old value:
/// `assertion `left == right` failed: the registry holds the edit  left: Some(String("rack a"))
/// right: Some(String("rack b"))`.
#[test]
fn the_tables_page_lists_collections_and_writes_an_edited_cell() {
    let lab = two_macs();
    let handle = Handle::at(lab.paths[0].clone(), &REGISTRY);
    let before = handle.history().unwrap();
    let seeded_by = before.last().unwrap().machine;

    let mut app = App::headless_for_test();
    app.open_tables_tab_at(Some(lab.paths[0].clone())).unwrap();
    settle(&mut app);

    let ui = render(&app);
    for (name, title, _) in crate::tables_store::SHOWN {
        let rail = said(&ui, &format!("tables/sheet/{name}")).expect("each table is in the rail");
        assert!(rail.starts_with(&format!("{title}: ")), "{rail}");
    }
    assert_eq!(
        said(&ui, "tables/sheet/machine").as_deref(),
        Some("Machines: 1 row")
    );
    assert_eq!(said(&ui, "tables/count").as_deref(), Some("1 row"));
    let cell = format!("tables/cell/{MC}/location");
    assert_eq!(cell_says(&ui, MC, "location"), "rack a");
    assert!(
        said(&ui, "tables/bar/count").is_none(),
        "nothing staged yet"
    );

    // A press selects the cell, a second opens it; Return stages what was typed.
    act(&mut app, &cell, None);
    act(&mut app, &cell, None);
    assert!(said(&render(&app), EDIT).is_some(), "the cell opened");
    act(&mut app, EDIT, Some(SemanticInput::Text("rack b".into())));
    key_in(&mut app, TextInputEvent::Submit);
    let ui = render(&app);
    assert_eq!(
        said(&ui, "tables/bar/count").as_deref(),
        Some("1 change not written")
    );
    assert_eq!(cell_says(&ui, MC, "location"), "rack b, not written");
    assert_eq!(
        location(&lab, 0),
        Some(json!("rack a")),
        "staged is not written"
    );

    act(&mut app, WRITE, None);
    settle_write(&mut app);
    assert_eq!(
        location(&lab, 0),
        Some(json!("rack b")),
        "the registry holds the edit"
    );
    let history = handle.history().unwrap();
    assert_eq!(
        history.len(),
        before.len() + 1,
        "one Write is one operation"
    );
    let last = history.last().unwrap();
    assert_ne!(
        last.machine, seeded_by,
        "this Mac wrote it, not the seeding one"
    );
    assert!(
        last.changes
            .iter()
            .any(|c| c.key == format!("{MC}.location")),
        "one operation wrote the cell"
    );

    let ui = render(&app);
    assert_eq!(cell_says(&ui, MC, "location"), "rack b");
    assert_eq!(said(&ui, "tables/note").as_deref(), Some("Wrote 1 cell"));
    // The pane: the field's writer, and the Write as the newest history entry.
    let by = said(&ui, "tables/pane/field/location/by").expect("the writer of the cell");
    let machine = aterm_db::typed::keys::uuid32(last.machine.as_ref().unwrap());
    assert!(machine.starts_with(&by), "{by} names {machine}");
    assert_eq!(
        said(&ui, "tables/pane/written/0/0").as_deref(),
        Some("Location: rack a \u{2192} rack b")
    );
    // The entry names its writer, its place in the causal order and its writer's own clock.
    assert_eq!(said(&ui, "tables/pane/written/0/who"), Some(by));
    let when = said(&ui, "tables/pane/written/0/when").unwrap();
    assert!(when.starts_with("\u{b7} "), "{when}");
    assert!(when.contains(" \u{b7} claimed "), "{when}");
}

/// The page reads the table as of a past operation through the registry's own as-of read: a
/// history entry's As of shows the row as that operation's causal past holds it, read only, with
/// each cell changed since marked and listed in the pane with what it holds now and who wrote it;
/// Now returns to the table as it is.
/// NEGATIVE CONTROL: with `tables_store::read` reading the head whatever point it is given
/// (`view_at(Point::Head)`), the past table shows the head's value:
/// `` left: "rack b, changed since" right: "rack a, changed since" ``. With `TablesApp::fixed`
/// reading only the point shown (not a move in flight), the cell opens for an edit on the way to
/// the past point and the grid no longer lists it: `` the cell is listed ``.
#[test]
fn the_tables_page_reads_as_of_a_point_and_compares_it_with_now() {
    let lab = two_macs();
    let handle = Handle::at(lab.paths[0].clone(), &REGISTRY);
    let seeded = handle.history().unwrap().last().unwrap().operation;
    Registry::at(lab.paths[0].clone())
        .write(vec![vec![put("location", json!("rack b"))]])
        .unwrap();

    let mut app = App::headless_for_test();
    app.open_tables_tab_at(Some(lab.paths[0].clone())).unwrap();
    settle(&mut app);
    let cell = format!("tables/cell/{MC}/location");
    act(&mut app, &cell, None);
    let ui = render(&app);
    assert_eq!(cell_says(&ui, MC, "location"), "rack b");
    assert!(said(&ui, "tables/asof").is_none(), "the table as it is");
    let as_of = format!(
        "tables/pane/asof/{}",
        aterm_codec::hex::encode_lower(&seeded)
    );
    assert!(
        said(&ui, &as_of).is_some(),
        "the seeding entry offers its point"
    );

    act(&mut app, &as_of, None);
    // While the move is read, the page takes no edit: a past table could not write it.
    act(&mut app, &cell, None);
    act(&mut app, &cell, None);
    act(&mut app, EDIT, Some(SemanticInput::Text("rack z".into())));
    key_in(&mut app, TextInputEvent::Submit);
    assert!(
        said(&render(&app), "tables/bar/count").is_none(),
        "nothing staged on the way to a past point"
    );
    settle(&mut app);
    let ui = render(&app);
    assert_eq!(cell_says(&ui, MC, "location"), "rack a, changed since");
    assert_eq!(cell_says(&ui, MC, "status"), "active");
    let words = said(&ui, "tables/asof").expect("the past point is named");
    assert!(words.starts_with("As of "), "{words}");
    assert!(words.ends_with(" \u{b7} 1 cell changed since"), "{words}");
    assert_eq!(
        said(&ui, "tables/pane/now/location/what").as_deref(),
        Some("Location: rack a \u{2192} rack b")
    );
    let newest = handle.history().unwrap().last().unwrap().machine.unwrap();
    let who = said(&ui, "tables/pane/now/location/who").expect("who wrote it since");
    assert!(
        aterm_db::typed::keys::uuid32(&newest).starts_with(&who),
        "{who} wrote rack b"
    );
    // A past table is read only: a second press opens nothing.
    act(&mut app, &cell, None);
    assert!(
        said(&render(&app), EDIT).is_none(),
        "a past cell never opens"
    );

    act(&mut app, NOW, None);
    settle(&mut app);
    let ui = render(&app);
    assert_eq!(cell_says(&ui, MC, "location"), "rack b");
    assert!(
        said(&ui, "tables/asof").is_none(),
        "back to the table as it is"
    );
}

/// In a past table the pane names every writer of a cell's concurrent values since, and a
/// changed secret says only that it changed.
/// NEGATIVE CONTROL: with the pane's secret branch removed, the line says what the mask says:
/// `` left: Some("Password: set → set") right: Some("Password: changed") ``.
#[test]
fn a_past_table_names_every_writer_since_and_never_a_secret() {
    let mut snapshot = one_sheet(Shown {
        text: "Spare".into(),
        ..Shown::default()
    });
    snapshot.at = Some(AsOf {
        operation: [3; 32],
        by: Some(author("m9", 1_000)),
    });
    let sheet = &mut snapshot.sheets[0];
    sheet.since.insert(
        (MC.into(), "password".into()),
        Since {
            now: aterm_db::typed::secret::MASK.into(),
            by: vec![author("m55", 2_000)],
            members: false,
        },
    );
    sheet.since.insert(
        (MC.into(), "nickname".into()),
        Since {
            now: "Backup / Spare".into(),
            by: vec![author("m55", 2_000), author("m73", 3_000)],
            members: false,
        },
    );
    let mut app = open_with(snapshot);
    act(&mut app, &format!("tables/cell/{MC}/nickname"), None);
    let ui = render(&app);
    assert_eq!(
        said(&ui, "tables/pane/now/password/what").as_deref(),
        Some("Password: changed")
    );
    assert_eq!(
        said(&ui, "tables/pane/now/nickname/who").as_deref(),
        Some("m55 / m73")
    );
    assert!(
        said(&ui, "tables/asof")
            .unwrap()
            .ends_with("2 cells changed since")
    );
    // A past table takes no edit, and Keep is not offered.
    act(&mut app, &format!("tables/cell/{MC}/nickname"), None);
    assert!(said(&render(&app), EDIT).is_none());
}

/// Two moves asked for in a row keep the page from taking an edit until both have answered: the
/// first answer may show the table as it is, but the second may land it on a past table, which
/// could never write an edit staged in between.
/// NEGATIVE CONTROL: with the moves in flight held as one flag that the first answer clears
/// (the first answer setting it to none), the edit staged between the answers is kept:
/// `` the second move has not answered ``.
#[test]
fn an_edit_waits_for_every_move_asked_for() {
    let head = one_sheet(Shown {
        text: "Spare".into(),
        ..Shown::default()
    });
    let mut app = open_with(head.clone());
    let tables = app.native_runtime.tables_mut().unwrap();
    // Now, then As of another point, before either answers.
    tables.go(None);
    tables.go(Some([3; 32]));
    tables.take(Reply::Read {
        read: Ok(head),
        moved: true,
    });
    tables.stage("machine", MC, "nickname", "Backup".into());
    assert!(tables.staged.is_empty(), "the second move has not answered");
    let mut past = one_sheet(Shown {
        text: "Spare".into(),
        ..Shown::default()
    });
    past.at = Some(AsOf {
        operation: [3; 32],
        by: None,
    });
    tables.take(Reply::Read {
        read: Ok(past),
        moved: true,
    });
    tables.stage("machine", MC, "nickname", "Backup".into());
    assert!(tables.staged.is_empty(), "a past table takes no edit");
    // Back at the head with every move answered, an edit stages again.
    tables.go(None);
    tables.take(Reply::Read {
        read: Ok(one_sheet(Shown {
            text: "Spare".into(),
            ..Shown::default()
        })),
        moved: true,
    });
    tables.stage("machine", MC, "nickname", "Backup".into());
    assert_eq!(tables.staged.len(), 1);
}

fn author(label: &str, at_ms: u64) -> Author {
    Author {
        kind: AuthorKind::Person,
        label: Some(label.into()),
        id: "c05f416f".into(),
        at_ms,
        hlc: (at_ms, 0),
    }
}

fn one_sheet(nickname: Shown) -> Snapshot {
    let column = |name: &str| SheetColumn {
        name: name.into(),
        label: crate::tables_store::label_of(name),
        group: Group::Stored,
        editable: true,
        numeric: false,
        secret: false,
        masked: false,
        links: None,
    };
    let mut password = column("password");
    password.secret = true;
    password.editable = false;
    let mut cells = BTreeMap::new();
    cells.insert("nickname".to_owned(), nickname);
    cells.insert(
        "password".to_owned(),
        Shown {
            text: aterm_db::typed::secret::MASK.into(),
            ..Shown::default()
        },
    );
    Snapshot {
        sheets: vec![Sheet {
            name: "machine",
            title: "Machines",
            singular: "Machine",
            key: "nickname".into(),
            columns: vec![column("nickname"), password],
            rows: vec![SheetRow {
                pk: MC.into(),
                cells,
            }],
            written: Vec::new(),
            since: BTreeMap::new(),
            base: Base {
                table: "alab/registry".into(),
                collection: "machine".into(),
                version: 2,
                reveal: false,
                header: Vec::new(),
                rows: Vec::new(),
            },
        }],
        at: None,
        read_ms: 0,
    }
}

fn open_with(snapshot: Snapshot) -> App {
    let mut app = App::headless_for_test();
    app.open_tables_tab_at(None).unwrap();
    // The reader of a test binary reads nothing; its answer is taken before the hand-made one.
    settle(&mut app);
    app.native_runtime.tables_mut().unwrap().show(snapshot);
    app
}

/// A conflicted cell shows every value, none chosen, with how many there are; the pane lists
/// each with its writer, and Keep stages the one kept.
/// NEGATIVE CONTROL: with the grid drawing `Shown::text` of only the first value, the cell reads
/// `Backup` and the first assertion fails.
#[test]
fn a_conflict_shows_every_value_and_keep_stages_one() {
    let conflict = Shown {
        text: "Backup / Spare".into(),
        by: None,
        values: vec![
            ("Backup".into(), Some(author("m55", 2_000))),
            ("Spare".into(), Some(author("m73", 1_000))),
        ],
        raw: false,
    };
    let mut app = open_with(one_sheet(conflict));
    let cell = format!("tables/cell/{MC}/nickname");
    act(&mut app, &cell, None);
    let ui = render(&app);
    assert_eq!(cell_says(&ui, MC, "nickname"), "Backup / Spare, 2 values");
    assert_eq!(
        said(&ui, "tables/pane/conflict/nickname/head").as_deref(),
        Some("NICKNAME \u{b7} 2 VALUES")
    );
    assert!(
        said(&ui, "tables/pane/conflict/nickname/0/who")
            .unwrap()
            .starts_with("m55")
    );
    // Opening the cell and leaving it untouched stages nothing.
    act(&mut app, &cell, None);
    key_in(&mut app, TextInputEvent::Submit);
    assert!(said(&render(&app), "tables/bar/count").is_none());

    act(&mut app, "tables/pane/keep/nickname/1", None);
    let ui = render(&app);
    assert_eq!(
        said(&ui, "tables/bar/count").as_deref(),
        Some("1 change not written")
    );
    assert_eq!(cell_says(&ui, MC, "nickname"), "Spare, not written");
    act(&mut app, "tables/pane/undo/nickname", None);
    assert!(said(&render(&app), "tables/bar/count").is_none());
}

/// A secret cell shows only the mask, says only `set` in the pane, and never opens.
/// NEGATIVE CONTROL: with `open_edit` ignoring `editable`, the second press opens an edit field.
#[test]
fn a_secret_cell_shows_only_that_it_holds_one() {
    let mut app = open_with(one_sheet(Shown {
        text: "m9".into(),
        ..Shown::default()
    }));
    let cell = format!("tables/cell/{MC}/password");
    act(&mut app, &cell, None);
    act(&mut app, &cell, None);
    let ui = render(&app);
    assert!(said(&ui, EDIT).is_none(), "a secret never opens here");
    assert_eq!(
        cell_says(&ui, MC, "password"),
        aterm_db::typed::secret::MASK
    );
    assert_eq!(
        said(&ui, "tables/pane/field/password/value").as_deref(),
        Some("set")
    );
}

/// No word on the page names a state the owner ruled out.
#[test]
fn the_page_has_no_ruled_out_state_words() {
    let mut app = open_with(one_sheet(Shown {
        text: "m9".into(),
        ..Shown::default()
    }));
    act(&mut app, &format!("tables/cell/{MC}/nickname"), None);
    let ui = render(&app);
    for node in &ui.semantics {
        let text = format!("{} {:?}", node.label, node.value).to_lowercase();
        for word in ["needs you", "open task", "waiting", "blocked", "error"] {
            assert!(!text.contains(word), "{word} in {text}");
        }
    }
}

#[test]
fn a_tables_tab_is_one_per_app() {
    let mut app = App::headless_for_test();
    app.open_tables_tab_at(None).unwrap();
    app.open_tables_tab().unwrap();
    let tabs = app.windows[&WID]
        .tab_set
        .tabs()
        .iter()
        .filter(|tab| {
            let mut found = false;
            tab.root.any_leaf(&mut |&view| {
                found = matches!(
                    app.native_runtime.view_state(view),
                    Some(AppViewState::Tables(_))
                );
                found
            });
            found
        })
        .count();
    assert_eq!(tabs, 1);
}

/// A cell staged on one read and written after a later read that holds another Mac's newer value
/// is compared against what it held when it was staged: the newer value stays, and the staged
/// one is kept on the page rather than written over it.
/// NEGATIVE CONTROL: with `TablesApp::write` comparing against the latest read (its
/// `cell.clone_from(then)` removed), the Write overwrites the other Mac's `rack z` with `rack b`
/// and the first assertion fails.
#[test]
fn a_cell_changed_elsewhere_after_staging_is_not_written_over() {
    let lab = two_macs();
    let mut app = App::headless_for_test();
    app.open_tables_tab_at(Some(lab.paths[0].clone())).unwrap();
    settle(&mut app);
    let cell = format!("tables/cell/{MC}/location");
    act(&mut app, &cell, None);
    act(&mut app, &cell, None);
    act(&mut app, EDIT, Some(SemanticInput::Text("rack b".into())));
    key_in(&mut app, TextInputEvent::Submit);

    // The other Mac writes the cell; this page reads it before the Write.
    Registry::at(lab.paths[1].clone())
        .write(vec![vec![put("location", json!("rack z"))]])
        .unwrap();
    repair_commons(&lab, 4);
    assert_eq!(location(&lab, 0), Some(json!("rack z")));
    let tables = app.native_runtime.tables_mut().unwrap();
    tables.work.send(Work::Read).unwrap();
    tables.settle();
    assert_eq!(
        said(&render(&app), "tables/pane/since/location").as_deref(),
        Some("Written elsewhere since: rack z")
    );

    act(&mut app, WRITE, None);
    settle_write(&mut app);
    assert_eq!(
        location(&lab, 0),
        Some(json!("rack z")),
        "the newer value stays"
    );
    let ui = render(&app);
    assert_eq!(cell_says(&ui, MC, "location"), "rack b, not written");
    assert!(
        said(&ui, "tables/bar/note")
            .unwrap_or_default()
            .contains("changed elsewhere since the read, kept here"),
        "{:?}",
        said(&ui, "tables/bar/note")
    );
}

/// A cell whose Write names a row its table does not hold (`merged_into` naming a machine that
/// is not there) is refused by the registry's save: the page shows the rule on that cell's
/// field, keeps the cell staged, and the registry holds nothing of it.
/// NEGATIVE CONTROL: with `Compiled::rules` answering the table's guard alone
/// (`typed::check::missing_row` not called), the Write lands and no refusal is shown: `` the
/// refusal is on the cell: None ``.
#[test]
fn a_write_naming_a_missing_row_shows_the_refusal_on_the_cell() {
    let lab = two_macs();
    let mut app = App::headless_for_test();
    app.open_tables_tab_at(Some(lab.paths[0].clone())).unwrap();
    settle(&mut app);
    let cell = format!("tables/cell/{MC}/merged_into");
    act(&mut app, &cell, None);
    act(&mut app, &cell, None);
    act(
        &mut app,
        EDIT,
        Some(SemanticInput::Text("mc-00000000000000ff".into())),
    );
    key_in(&mut app, TextInputEvent::Submit);
    act(&mut app, WRITE, None);
    settle_write(&mut app);

    let ui = render(&app);
    let refused = said(&ui, "tables/pane/refused/merged_into");
    assert_eq!(
        refused.as_deref(),
        Some("no such machine row"),
        "the refusal is on the cell: {refused:?}"
    );
    assert_eq!(
        cell_says(&ui, MC, "merged_into"),
        "mc-00000000000000ff, not written"
    );
    let merged = Handle::at(lab.paths[0].clone(), &REGISTRY)
        .view()
        .unwrap()
        .project(&MACHINE)
        .rows[MC]
        .get("merged_into")
        .cloned();
    assert_eq!(merged, None, "the registry holds nothing of it");
}

fn machine_at(pk: &str, column: &str, value: aterm_json::Value) -> aterm_db::database::Change {
    cell::change(
        &MACHINE,
        CellKey {
            pk,
            col: column,
            elem: None,
        },
        &CellValue::owner(MACHINE.version, value),
    )
    .unwrap()
}

/// A Mac's facts row as its own Commons machine `n` states it.
fn facts_row(n: u8, value: aterm_json::Value) -> (String, aterm_db::typed::owned::OwnedRow) {
    (
        aterm_db::typed::keys::uuid32(&[n; 16]),
        aterm_db::typed::owned::OwnedRow {
            machine: [n; 16],
            actor: [n; 48],
            value,
            rank: (0, 0, 0),
            operation: [0; 32],
        },
    )
}

/// Each writer is named from what aterm holds, never made up: the registry's label of the
/// machine its facts row binds (unless that label is conflicted), else the label its facts row
/// states, else its seat's; a predecessor machine (writer succession) takes its successor's
/// label, however far back; an actor whose machine is unknown takes its seat's; and a writer
/// nothing names has no label, only the start of its machine id, else of its actor.
/// NEGATIVE CONTROL: with `Writers::new`'s predecessor pass removed, machine 6 has no label:
/// `assertion `left == right` failed: a predecessor  left: None  right: Some("m73")`.
#[test]
fn a_writer_is_named_through_its_seat_machine_and_label() {
    use crate::tables_store::{Seat, Writers, short_id};
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let here = Registry::at(lab.paths[0].clone());
    here.open().unwrap();
    let there = Registry::at(lab.paths[1].clone());
    there.open().unwrap();
    repair_commons(&lab, 4);
    let (a, b) = ("mc-000000000000000a", "mc-000000000000000b");
    here.write(vec![
        vec![
            machine_at(a, "label", json!("m73")),
            machine_at(a, "row", json!("live")),
        ],
        vec![
            machine_at(b, "label", json!("m55")),
            machine_at(b, "row", json!("live")),
        ],
    ])
    .unwrap();
    repair_commons(&lab, 4);
    // Two Macs relabel one machine apart: its label is conflicted, and names no writer.
    here.write(vec![vec![machine_at(b, "label", json!("m56"))]])
        .unwrap();
    there
        .write(vec![vec![machine_at(b, "label", json!("m57"))]])
        .unwrap();
    repair_commons(&lab, 4);
    let machines = Handle::at(lab.paths[0].clone(), &REGISTRY)
        .view()
        .unwrap()
        .project(&MACHINE);
    assert!(machines.rows[b].fields["label"].conflicted());

    let facts = aterm_db::typed::owned::OwnedRead {
        rows: [
            facts_row(1, json!({"mac": a, "label": "m1"})),
            facts_row(2, json!({"mac": b, "label": "m2"})),
            facts_row(3, json!({"mac": "mc-00000000000000ff", "label": "m3"})),
            facts_row(
                5,
                json!({"mac": a, "label": "m5", "predecessor": aterm_db::typed::keys::uuid32(&[6; 16])}),
            ),
            facts_row(
                6,
                json!({"predecessor": aterm_db::typed::keys::uuid32(&[7; 16])}),
            ),
        ]
        .into_iter()
        .collect(),
        inert: 0,
    };
    let seats = [
        Seat {
            actor: [8; 48],
            machine: [8; 16],
            label: "m8".into(),
        },
        Seat {
            actor: [9; 48],
            machine: [1; 16],
            label: "m99".into(),
        },
    ];
    let writers = Writers::new(&machines, &facts, &seats);
    let of = |n: u8| writers.label(&[0; 48], Some(&[n; 16]));
    assert_eq!(of(1).as_deref(), Some("m73"), "the registry's label first");
    assert_eq!(
        of(2).as_deref(),
        Some("m2"),
        "a conflicted label names no one"
    );
    assert_eq!(of(3).as_deref(), Some("m3"), "the facts row's own label");
    assert_eq!(of(6).as_deref(), Some("m73"), "a predecessor");
    assert_eq!(of(7).as_deref(), Some("m73"), "a predecessor's predecessor");
    assert_eq!(of(8).as_deref(), Some("m8"), "a seat's machine");
    assert_eq!(
        writers.label(&[8; 48], None).as_deref(),
        Some("m8"),
        "an actor's seat"
    );
    assert_eq!(writers.label(&[10; 48], Some(&[10; 16])), None);
    assert_eq!(writers.label(&[10; 48], None), None);
    assert_eq!(short_id(&[10; 48], Some(&[10; 16])), "0a0a0a0a");
    assert_eq!(short_id(&[0xab; 48], None), "abababab");
}

/// Where aterm holds a Mac's label, the page names the writer by it: here a registration names
/// each Mac in the registry, and the cell another Mac wrote reads as written by that Mac's
/// label, in WRITTEN BY and in the history.
/// NEGATIVE CONTROL: with `tables_store::read` naming writers by `short_id` alone (its
/// `label: writers.label(..)` set to `None`), WRITTEN BY reads the machine id: `assertion
/// `left == right` failed: the writer of the cell is the other Mac, by its label  left:
/// Some("28a48a48")  right: Some("m2")`.
#[test]
fn the_page_names_a_writer_by_its_mac_label() {
    let lab = two_macs();
    aterm_secrets::client::admin_seed_swarm_roster(&lab.paths[0], &[]).unwrap();
    aterm_secrets::client::sync(&lab.paths[1], None).unwrap();
    for (n, host) in ["mac-one", "mac-two"].into_iter().enumerate() {
        let probe = aterm_db::alab::register::Probe {
            hw_model: "Mac16,8".into(),
            hosts: vec![host.into()],
            node: Some(format!("{:016x}", n + 1)),
            platform_uuid: format!("C3396674-FCD3-52E5-A1C1-BD5B9E8F5C4{n}"),
            ..aterm_db::alab::register::Probe::default()
        };
        Registry::at(lab.paths[n].clone())
            .register(
                &lab.paths[n].home.join("config.toml"),
                &probe,
                true,
                aterm_time::unix_millis().unwrap(),
            )
            .unwrap();
        repair_commons(&lab, 4);
    }
    let view = Handle::at(lab.paths[0].clone(), &REGISTRY).view().unwrap();
    let mac = view.owned["machine-facts"].rows[&aterm_db::typed::keys::uuid32(&lab.machine(1))]
        .value["mac"]
        .as_str()
        .unwrap()
        .to_owned();
    let label = view.project(&MACHINE).rows[&mac]
        .get("label")
        .and_then(aterm_json::Value::as_str)
        .unwrap()
        .to_owned();

    let mut app = App::headless_for_test();
    app.open_tables_tab_at(Some(lab.paths[0].clone())).unwrap();
    settle(&mut app);
    act(&mut app, &format!("tables/cell/{MC}/location"), None);
    let ui = render(&app);
    assert_eq!(
        said(&ui, "tables/pane/field/location/by").as_deref(),
        Some(label.as_str()),
        "the writer of the cell is the other Mac, by its label"
    );
    let tree = tree(&app);
    assert_eq!(
        style_of(&tree, "tables/pane/field/location/by"),
        Some(StyleRef::Note)
    );
    let history: Vec<String> = (0..8)
        .filter_map(|n| said(&ui, &format!("tables/pane/written/{n}/who")))
        .collect();
    assert!(history.contains(&label), "{history:?} names {label}");
}

/// A writer aterm knows no label of shows the start of its machine id, dimmed, in every place the
/// page names a writer; a known one shows its label in the page's usual ink.
/// NEGATIVE CONTROL: with `Author::style` returning `known` always, WRITTEN BY is set as a Note:
/// `assertion `left == right` failed: tables/pane/field/password/by  left: Some(Note)  right:
/// Some(FaintNote)`.
#[test]
fn an_unknown_writer_shows_its_short_id_dimmed() {
    let unknown = Author {
        label: None,
        ..author("unused", 2_000)
    };
    let mut snapshot = one_sheet(Shown {
        text: "Backup / Spare".into(),
        by: None,
        values: vec![
            ("Backup".into(), Some(unknown.clone())),
            ("Spare".into(), Some(author("m73", 1_000))),
        ],
        raw: false,
    });
    snapshot.sheets[0].rows[0].cells.insert(
        "password".into(),
        Shown {
            text: aterm_db::typed::secret::MASK.into(),
            by: Some(unknown.clone()),
            ..Shown::default()
        },
    );
    snapshot.sheets[0].written.push(Written {
        operation: [7; 32],
        by: unknown,
        cells: vec![WrittenCell {
            pk: MC.into(),
            column: "password".into(),
            secret: true,
            now: "set".into(),
        }],
    });
    let mut app = open_with(snapshot);
    act(&mut app, &format!("tables/cell/{MC}/nickname"), None);
    let ui = render(&app);
    let tree = tree(&app);
    assert_eq!(
        said(&ui, "tables/pane/field/password/by").as_deref(),
        Some("c05f416f")
    );
    // Dimmed at the size of what is beside it: the history's line is Plain, the rest notes.
    for (key, dim) in [
        ("tables/pane/field/password/by", StyleRef::FaintNote),
        ("tables/pane/written/0/who", StyleRef::Faint),
        ("tables/pane/conflict/nickname/0/who", StyleRef::FaintNote),
    ] {
        assert_eq!(style_of(&tree, key), Some(dim), "{key}");
    }
    assert_eq!(
        style_of(&tree, "tables/pane/written/0/when"),
        Some(StyleRef::Plain)
    );
    assert_eq!(
        said(&ui, "tables/pane/conflict/nickname/1/who").as_deref(),
        Some("m73")
    );
    assert_eq!(
        style_of(&tree, "tables/pane/conflict/nickname/1/who"),
        Some(StyleRef::Note)
    );
}

/// A sheet of `n` wide columns, `label` its key column, one row.
fn wide_sheet(n: usize) -> Snapshot {
    let mut snapshot = one_sheet(Shown {
        text: "Spare".into(),
        ..Shown::default()
    });
    let sheet = &mut snapshot.sheets[0];
    let template = sheet.columns[0].clone();
    sheet.columns.clear();
    sheet.key = "label".into();
    let names: Vec<String> = std::iter::once("label".to_owned())
        .chain((1..n).map(|i| format!("c{i}")))
        .collect();
    for name in &names {
        sheet.columns.push(SheetColumn {
            name: name.clone(),
            label: crate::tables_store::label_of(name),
            ..template.clone()
        });
        sheet.rows[0].cells.insert(
            name.clone(),
            Shown {
                text: if name == "label" {
                    "m73".into()
                } else {
                    "a value wide enough to fill its column to the page's cap".into()
                },
                ..Shown::default()
            },
        );
    }
    snapshot
}

fn rect(ui: &CompiledUi, key: &str) -> LogicalRect {
    ui.semantic(&UiKey::new(key.to_owned()))
        .unwrap_or_else(|| panic!("{key} is listed"))
        .rect
}

/// The key column stays at the grid's left while the rest scroll across: selecting the last
/// column moves every other column left, never the row's label or its header.
/// NEGATIVE CONTROL: with `grid`'s `lane` scrolling every child (no `split_off`), the label
/// scrolls with the rest: `assertion `left == right` failed: the label stays at the left  left:
/// -1885.0  right: 205.0`.
#[test]
fn the_key_column_stays_at_the_left_while_the_grid_scrolls_across() {
    let mut app = open_with(wide_sheet(12));
    let label = format!("tables/cell/{MC}/label");
    let first = format!("tables/cell/{MC}/c1");
    let last = format!("tables/cell/{MC}/c11");
    act(&mut app, &label, None);
    let before = render(&app);
    act(&mut app, &last, None);
    let after = render(&app);
    assert_eq!(
        rect(&after, &label).x,
        rect(&before, &label).x,
        "the label stays at the left"
    );
    assert_eq!(
        rect(&after, "tables/head/label").x,
        rect(&before, "tables/head/label").x,
        "its header too"
    );
    assert!(
        rect(&after, &first).x < rect(&before, &first).x,
        "the rest scrolled across: {:?} then {:?}",
        rect(&before, &first),
        rect(&after, &first)
    );
    let shown = rect(&after, &last);
    let pinned = rect(&after, &label);
    assert!(
        shown.x >= pinned.x + pinned.width - 0.5,
        "the selected column shows beside the label: {shown:?}, label {pinned:?}"
    );
    // Stepping back to the key column scrolls nothing: it is always in view.
    key_in(&mut app, TextInputEvent::Left { extend: false });
    for _ in 0..12 {
        key_in(&mut app, TextInputEvent::Left { extend: false });
    }
    assert_eq!(rect(&render(&app), &label).x, pinned.x);
    assert_eq!(
        said(&render(&app), "tables/pane/title").as_deref(),
        Some("m73"),
        "the pane names the row by its key"
    );
}

/// "Not written" is one fixed blue: the staged cell, the rail's count, the Write bar and its
/// button wear it whatever the accent (here a green one), and nothing else on the page does.
/// NEGATIVE CONTROL: with `NativeRoles::unwritten` the accent, the accent moves it (the default
/// theme's own accent is already green, from its cursor): `assertion `left == right` failed: the
/// accent does not move it  left: [100, 202, 115]  right: [109, 213, 128]`. With the rail's count
/// set as `StyleRef::Accent`, `tables/sheet/machine/staged` is missing from what wears it and the
/// last assertion fails.
#[test]
fn not_written_is_one_fixed_blue_whatever_the_accent() {
    use crate::native_appearance::{SystemAccent, install_system_accent};
    let theme = aterm_render::Theme::default();
    let blue = crate::settings::Roles::from_theme(theme);
    let green = SystemAccent {
        on_dark: [0x30, 0xD1, 0x58],
        on_light: [0x28, 0xA7, 0x45],
    };
    install_system_accent(Some(green));
    let roles = crate::settings::Roles::from_theme(theme);
    assert_eq!(
        roles.unwritten, blue.unwritten,
        "the accent does not move it"
    );
    assert_ne!(roles.accent, blue.accent, "the accent moved");
    let dark = !aterm_messages::ink::bg_is_light(roles.surface);
    let seed = if dark {
        crate::native_appearance::NOT_WRITTEN.on_dark
    } else {
        crate::native_appearance::NOT_WRITTEN.on_light
    };
    assert_eq!(roles.unwritten, seed, "the design's blue, as it is");

    let mut app = open_with(one_sheet(Shown {
        text: "Spare".into(),
        ..Shown::default()
    }));
    let cell = format!("tables/cell/{MC}/nickname");
    act(&mut app, &cell, None);
    act(&mut app, &cell, None);
    act(&mut app, EDIT, Some(SemanticInput::Text("Desk".into())));
    key_in(&mut app, TextInputEvent::Submit);
    let painted = render(&app).painted(roles);
    install_system_accent(None);
    let wash = crate::native_appearance::mix_srgb(
        roles.elevated,
        roles.unwritten,
        crate::native_appearance::UNWRITTEN_WASH,
    );
    let blues = [roles.unwritten, roles.unwritten_ink, wash];
    let wears: std::collections::BTreeSet<String> = painted
        .iter()
        .filter(|(_, prims)| {
            prims.iter().any(|prim| {
                use crate::widget::DrawPrim;
                let color = match prim {
                    DrawPrim::Panel { fill, .. } => *fill,
                    DrawPrim::Stroke { color, .. }
                    | DrawPrim::Line { color, .. }
                    | DrawPrim::Dot { color, .. }
                    | DrawPrim::Text { color, .. } => *color,
                    _ => return false,
                };
                blues.contains(&[color[0], color[1], color[2]])
            })
        })
        .map(|(key, _)| key.as_str().to_owned())
        .collect();
    let staged: std::collections::BTreeSet<String> = [
        format!("{cell}/staged"),
        "tables/sheet/machine/staged".into(),
        "tables/bar".into(),
        "tables/bar/count".into(),
        WRITE.into(),
        "tables/pane/field/nickname/value".into(),
    ]
    .into_iter()
    .collect();
    assert_eq!(wears, staged, "only what is not written wears it");
}

/// The style node `key`'s words are set in.
fn style_of(tree: &UiTree, key: &str) -> Option<StyleRef> {
    fn find<'a>(node: &'a UiNode, key: &str) -> Option<&'a UiNode> {
        if node.key.as_str() == key {
            return Some(node);
        }
        node.children.iter().find_map(|child| find(child, key))
    }
    match &find(&tree.root, key)?.content {
        UiContent::Text(spec) => Some(spec.style),
        _ => None,
    }
}
