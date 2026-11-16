// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Tables page on a scratch registry (`aterm_db::table::fixture::Lab`: an isolated home, no
//! live vault, fleet or Commons) and on hand-made reads. Every value is synthetic.

use super::*;
use crate::native_app::{ActionInvocation, AppViewState, ViewMotionCx};
use crate::native_ui::{CompiledUi, LogicalRect};
use crate::{App, WindowId};
use aterm_db::alab::Registry;
use aterm_db::alab::registry::{ADDRESS, MACHINE, PHONE};
use aterm_db::att::{Handle, REGISTRY};
use aterm_db::table::fixture::{Lab, repair_commons};
use aterm_db::typed::Collection;
use aterm_db::typed::cell::{self, CellKey, CellValue};
use aterm_db::typed::keys::{self, Random};
use aterm_db::typed::secret::{self, Memory, Secret};
use aterm_json::json;
use aterm_secrets::commons::roster::Role;

pub(crate) const WID: WindowId = WindowId(0);
pub(crate) const MC: &str = "mc-0000000000000009";

fn put(column: &str, value: aterm_json::Value) -> aterm_db::database::Change {
    put_in(&MACHINE, MC, column, value)
}

pub(crate) fn put_in(
    collection: &'static Collection,
    pk: &str,
    column: &str,
    value: aterm_json::Value,
) -> aterm_db::database::Change {
    cell::change(
        collection,
        CellKey {
            pk,
            col: column,
            elem: None,
        },
        &CellValue::owner(collection.version, value),
    )
    .unwrap()
}

pub(crate) const VIEWPORT: LogicalRect = LogicalRect::new(0.0, 0.0, 1400.0, 900.0);

pub(crate) fn tree(app: &App) -> UiTree {
    tree_in(app, VIEWPORT)
}

/// The page as a window of `viewport` lays it out.
fn tree_in(app: &App, viewport: LogicalRect) -> UiTree {
    let (instance, view) = app
        .active_native_view(WID)
        .expect("the Tables tab is in front");
    let cx = ViewCx {
        viewport,
        animation_phase_ms: 0,
        motion: ViewMotionCx::default(),
        terminal_font_px: 16.0,
        terminal_scale: 1.0,
        semantic_font: None,
        document: None,
    };
    app.native_runtime.render(instance, view, &cx).unwrap()
}

pub(crate) fn render(app: &App) -> CompiledUi {
    tree(app).compile(VIEWPORT).unwrap()
}

/// The page compiled for a window `width` wide.
fn render_wide(app: &App, width: f32) -> CompiledUi {
    let viewport = LogicalRect::new(0.0, 0.0, width, VIEWPORT.height);
    tree_in(app, viewport).compile(viewport).unwrap()
}

/// What node `key` says: a text's words, a control's spoken label.
pub(crate) fn said(ui: &CompiledUi, key: &str) -> Option<String> {
    let node = ui.semantic(&UiKey::new(key.to_owned()))?;
    Some(match &node.value {
        SemanticValue::Text(text) if !text.is_empty() => text.clone(),
        _ => node.label.clone(),
    })
}

/// What a grid cell says after its column and row: its text, then `, 2 values` or
/// `, not written` where it is either.
pub(crate) fn cell_says(ui: &CompiledUi, pk: &str, column: &str) -> String {
    let spoken = said(ui, &format!("tables/cell/{pk}/{column}")).expect("the cell is listed");
    spoken
        .split_once(": ")
        .map(|(_, rest)| rest.to_owned())
        .unwrap_or_default()
}

/// Whether a cell or pane value shows `full`, whole or cut to its width with `…`.
fn shows(cell: &str, full: &str) -> bool {
    cell == full
        || cell
            .strip_suffix('…')
            .is_some_and(|start| !start.is_empty() && full.starts_with(start))
}

pub(crate) fn act(app: &mut App, key: &str, value: Option<SemanticInput>) {
    app.dispatch_native_event(
        WID,
        AppEvent::Action(ActionInvocation {
            id: ActionId::new(key.to_owned()),
            value,
        }),
    )
    .expect("the page takes the press");
}

pub(crate) fn key_in(app: &mut App, event: TextInputEvent) {
    app.dispatch_native_event(WID, AppEvent::TextInput(event))
        .expect("the page takes the key");
}

pub(crate) fn settle(app: &mut App) {
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
pub(crate) fn two_macs() -> Lab {
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

/// The page's reader reads the registry again only where it moved: asked again with nothing
/// written it answers unchanged, read as of then, and once a record lands it reads; a page hidden
/// and shown again asks the registry once and reads only where it moved meanwhile.
/// NEGATIVE CONTROL: with the reader's probe removed (`if false && again …`), the second ask
/// reads the whole registry again: `` the registry did not move ``.
#[test]
fn the_tables_reader_reads_again_only_where_the_registry_moved() {
    let lab = two_macs();
    let mut tables = TablesApp::paced(Some(lab.paths[0].clone()), || {}, None).unwrap();
    tables.settle();
    let first = tables.snapshot.as_ref().map(|s| s.read_ms).unwrap();
    let reply = |tables: &mut TablesApp, work: Work| {
        tables.work.send(work).unwrap();
        let reply = tables
            .replies
            .recv_timeout(Duration::from_secs(60))
            .expect("the Tables reader answers within a minute");
        let unchanged = matches!(reply, Reply::Unchanged(_));
        tables.take(reply);
        unchanged
    };
    assert!(reply(&mut tables, Work::Read), "the registry did not move");
    assert!(tables.snapshot.as_ref().unwrap().read_ms >= first);
    Registry::at(lab.paths[0].clone())
        .write(vec![vec![put("location", json!("rack b"))]])
        .unwrap();
    assert!(!reply(&mut tables, Work::Read), "a record landed");
    assert_eq!(
        cell_text(&tables, MC, "location"),
        "rack b",
        "the read shows it"
    );
    tables.set_shown(false);
    tables.set_shown(true);
    let shown = tables
        .replies
        .recv_timeout(Duration::from_secs(60))
        .expect("a page shown again asks the registry");
    assert!(
        matches!(shown, Reply::Unchanged(_)),
        "nothing moved meanwhile"
    );
    tables.set_shown(false);
    Registry::at(lab.paths[0].clone())
        .write(vec![vec![put("location", json!("rack c"))]])
        .unwrap();
    tables.set_shown(true);
    tables.settle();
    assert_eq!(cell_text(&tables, MC, "location"), "rack c");
}

/// The page's reader is told whether a view of it is shown as its view is suspended, shown
/// again and closed.
/// NEGATIVE CONTROL: with `NativeRuntime::set_view_suspended` not syncing the page
/// (`sync_tables_shown` removed there), a suspended view leaves it shown:
/// `` a hidden view hides the page ``.
#[test]
fn the_tables_reader_hears_whether_its_view_is_shown() {
    let lab = two_macs();
    let mut app = App::headless_for_test();
    app.open_tables_tab_at(Some(lab.paths[0].clone())).unwrap();
    settle(&mut app);
    let (_, view) = app
        .active_native_view(WID)
        .expect("the Tables tab is in front");
    let shown = |app: &mut App| app.native_runtime.tables_mut().unwrap().shown;
    assert!(shown(&mut app), "a view opened shown");
    app.native_runtime.set_view_suspended(view, true).unwrap();
    assert!(!shown(&mut app), "a hidden view hides the page");
    app.native_runtime.set_view_suspended(view, false).unwrap();
    assert!(shown(&mut app), "shown again");
    app.native_runtime.remove_view(view).unwrap();
    assert!(!shown(&mut app), "a closed view shows nothing");
}

/// What the read's machine sheet holds at row `pk`, column `column`.
fn cell_text(tables: &TablesApp, pk: &str, column: &str) -> String {
    let snapshot = tables.snapshot.as_ref().expect("a read");
    let sheet = snapshot
        .sheets
        .iter()
        .find(|sheet| sheet.name == MACHINE.name)
        .unwrap();
    let row = sheet.rows.iter().find(|row| row.pk == pk).unwrap();
    row.cells[column].text.clone()
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
    for listed in crate::tables_store::listed(&aterm_db::alab::registry::REGISTRY) {
        let rail =
            said(&ui, &format!("tables/sheet/{}", listed.name)).expect("each table is in the rail");
        assert!(rail.starts_with(&format!("{}: ", listed.title)), "{rail}");
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
    // The cell bar: the value then, and what the cell holds now.
    assert_eq!(bar(&ui, "value").as_deref(), Some("rack a"));
    let state = bar(&ui, "state").expect("what it holds now");
    assert!(state.starts_with("now rack b \u{b7} "), "{state}");
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
/// changed secret cell by the `aterm secrets` entry it names now, as every surface shows it: no
/// table holds a secret.
/// NEGATIVE CONTROL: with the pane saying only `set`, `cleared` or `changed` of a secret column
/// (as when it held sealed values), no entry is named: `` Password: changed ``.
#[test]
fn a_past_table_names_every_writer_since_and_a_secret_by_its_entry() {
    const ENTRY: &str = "address.password.0123456789abcdef";
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
            now: ENTRY.into(),
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
    let password = said(&ui, "tables/pane/now/password/what").unwrap();
    // The line names the entry the cell named then (the row's cell, cut to the pane's width)
    // where a sealed column's said only `changed`.
    assert!(
        password.starts_with("Password: alab-") && !password.ends_with("changed"),
        "{password}"
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

pub(crate) fn author(label: &str, at_ms: u64) -> Author {
    Author {
        kind: AuthorKind::Explicit,
        label: Some(label.into()),
        id: "c05f416f".into(),
        at_ms,
        hlc: (at_ms, 0),
    }
}

/// The store entry the hand-made sheet's password cell names.
const ENTRY: &str = "alab-machine-password-mc-0000000000000009";

pub(crate) fn one_sheet(nickname: Shown) -> Snapshot {
    let column = |name: &str| SheetColumn {
        name: name.into(),
        label: crate::tables_store::label_of(name),
        group: Group::Stored,
        editable: true,
        kind: ColumnKind::Text,
        links: None,
        compiled: None,
        key: false,
    };
    // A secret reference column is read-only on the page (`grid::read_only`).
    let mut password = column("password");
    password.editable = false;
    let mut cells = BTreeMap::new();
    cells.insert("nickname".to_owned(), nickname);
    cells.insert(
        "password".to_owned(),
        Shown {
            text: ENTRY.into(),
            ..Shown::default()
        },
    );
    Snapshot {
        listed: vec![crate::tables_store::Listed {
            name: "machine",
            title: "Machines",
            singular: "Machine",
            face: aterm_db::typed::Face::Owner,
            owned: false,
        }],
        sheets: vec![Sheet {
            name: "machine",
            title: "Machines",
            singular: "Machine",
            owned: false,
            key: "nickname".into(),
            columns: vec![column("nickname"), password],
            rows: vec![SheetRow {
                pk: MC.into(),
                cells,
            }],
            written: Vec::new(),
            since: BTreeMap::new(),
            observed: BTreeMap::new(),
            base: Base {
                table: "alab/registry".into(),
                collection: "machine".into(),
                version: 2,
                header: Vec::new(),
                rows: Vec::new(),
            },
            schema: None,
        }],
        check: None,
        at: None,
        read_ms: 0,
    }
}

pub(crate) fn open_with(snapshot: Snapshot) -> App {
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
        ..Shown::default()
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

/// A secret cell shows the name of its secret-store entry, in the grid and in the pane, and
/// never opens.
/// NEGATIVE CONTROL: with `open_edit` ignoring `editable`, the second press opens an edit field.
#[test]
fn a_secret_cell_names_its_store_entry() {
    let mut app = open_with(one_sheet(Shown {
        text: "m9".into(),
        ..Shown::default()
    }));
    let cell = format!("tables/cell/{MC}/password");
    act(&mut app, &cell, None);
    act(&mut app, &cell, None);
    let ui = render(&app);
    assert!(said(&ui, EDIT).is_none(), "a secret never opens here");
    assert!(shows(&cell_says(&ui, MC, "password"), ENTRY));
    assert_eq!(
        bar_says(&ui),
        ["m9 \u{b7} Password", ENTRY],
        "the cell bar reads the entry's name as the grid does"
    );
    assert!(shows(
        &said(&ui, "tables/pane/field/password/value").unwrap_or_default(),
        ENTRY
    ));
}

/// Every word the page and its read hold: each cell's text and values, each history entry's
/// value, and each rendered node's label and value.
fn every_word(app: &mut App, ui: &CompiledUi) -> Vec<String> {
    let mut words: Vec<String> = ui
        .semantics
        .iter()
        .map(|node| format!("{} {:?}", node.label, node.value))
        .collect();
    let tables = app.native_runtime.tables_mut().unwrap();
    for sheet in &tables.snapshot.as_ref().unwrap().sheets {
        for row in &sheet.rows {
            for shown in row.cells.values() {
                words.push(shown.text.clone());
                words.extend(shown.values.iter().map(|(v, _)| v.clone()));
            }
        }
        for written in &sheet.written {
            words.extend(written.cells.iter().map(|c| c.now.clone()));
        }
        for (_, seen) in &sheet.base.rows {
            words.extend(seen.values().map(|s| s.shown.clone()));
        }
    }
    words
}

/// On a scratch registry an address and a phone number read as stored, and a password held in
/// the secret store reads as its entry's name, never its value.
/// NEGATIVE CONTROL: the sentinel value went into a `Memory` store (`every()` holds it); were the
/// page to show a secret reference's value, or were a value written into the table rather than
/// through `secret::store_cell`, the last assertion would find the sentinel in a word.
#[test]
fn addresses_and_phones_are_plain_and_a_password_names_its_entry() {
    const ADDR: &str = "ann.synthetic@example.com";
    const NUMBER: &str = "5550100123";
    const SENTINEL: &str = "sentinel-not-for-any-page-7f3a";
    let lab = Lab::new(&[Role::Writer]);
    let registry = Registry::at(lab.paths[0].clone());
    // The generation that names store entries and holds plain numbers: the newest, which no
    // roll of this build reaches.
    registry.open_newest(1_800_000_000_000).unwrap();
    repair_commons(&lab, 4);

    let address = keys::address_pk(ADDR).unwrap();
    let phone = keys::random_pk(Random::Phone).unwrap();
    let store = Memory::default();
    let reference = secret::store_cell(
        &store,
        "address",
        "password",
        &address,
        &Secret::new(SENTINEL.into()),
    )
    .unwrap();
    let entry = secret::entry_of(&reference).unwrap().to_owned();
    assert_eq!(
        store.every(),
        vec![SENTINEL.to_owned()],
        "the store holds it"
    );
    registry
        .write(vec![
            vec![
                put_in(&PHONE, &phone, "last4", json!("0123")),
                put_in(&PHONE, &phone, "number", json!(NUMBER)),
                put_in(&PHONE, &phone, "row", json!("live")),
            ],
            vec![
                put_in(&ADDRESS, &address, "addr", json!(ADDR)),
                put_in(&ADDRESS, &address, "domain", json!("example.com")),
                put_in(&ADDRESS, &address, "origin", json!("owner")),
                put_in(&ADDRESS, &address, "phone", json!(phone)),
                put_in(&ADDRESS, &address, "password", reference),
                put_in(&ADDRESS, &address, "row", json!("live")),
            ],
        ])
        .unwrap();

    let mut app = App::headless_for_test();
    app.open_tables_tab_at(Some(lab.paths[0].clone())).unwrap();
    settle(&mut app);
    let mut words = Vec::new();

    act(&mut app, "tables/sheet/address", None);
    act(&mut app, &format!("tables/cell/{address}/password"), None);
    act(&mut app, &format!("tables/cell/{address}/password"), None);
    let ui = render(&app);
    assert!(
        said(&ui, EDIT).is_none(),
        "a secret reference never opens here"
    );
    assert_eq!(cell_says(&ui, &address, "addr"), ADDR);
    assert!(shows(&cell_says(&ui, &address, "password"), &entry));
    assert!(shows(
        &said(&ui, "tables/pane/field/password/value").unwrap_or_default(),
        &entry
    ));
    // The cell bar reads the selected cell as the grid does: the entry's name, whole.
    assert_eq!(bar(&ui, "value").as_deref(), Some(entry.as_str()));
    words.extend(every_word(&mut app, &ui));

    act(&mut app, "tables/sheet/phone", None);
    let ui = render(&app);
    assert_eq!(cell_says(&ui, &phone, "number"), NUMBER);
    words.extend(every_word(&mut app, &ui));

    assert!(
        words.iter().any(|w| w.contains(ADDR)) && words.iter().any(|w| w.contains(NUMBER)),
        "the scan reads what the page shows"
    );
    for value in store.every() {
        assert!(
            !words.iter().any(|w| w.contains(&value)),
            "a secret value reached the page"
        );
    }
}

/// Where the registry holds a phone's number and a password as sealed tokens (the generation a
/// lab starts at, which builds up to 0.200.0 read), the page shows the number as it is, opened
/// with nothing recorded and read-only, and the password as the mask: no word of the page or
/// its read holds the password, and no control opens it.
/// NEGATIVE CONTROL: with `tables_store::read` laying the sheet out without
/// `open_every_ordinary` (`Opened::new()` in its place), the number reads as the mask:
/// `` left: "••••••" right: "5550100123" ``.
#[test]
fn a_sealed_number_shows_as_it_is_and_a_sealed_password_as_the_mask() {
    const ADDR: &str = "ann.synthetic@example.com";
    const NUMBER: &str = "5550100123";
    const SENTINEL: &str = "sentinel-not-for-any-page-91c4";
    let lab = Lab::new(&[Role::Writer]);
    let registry = Registry::at(lab.paths[0].clone());
    registry.open().unwrap();
    assert!(
        registry.generation().successor.is_some(),
        "control: a generation that holds sealed tokens"
    );
    repair_commons(&lab, 4);
    let address = keys::address_pk(ADDR).unwrap();
    let phone = keys::random_pk(Random::Phone).unwrap();
    registry
        .write(vec![
            vec![
                put_in(&PHONE, &phone, "last4", json!("0123")),
                put_in(&PHONE, &phone, "row", json!("live")),
            ],
            vec![
                put_in(&ADDRESS, &address, "addr", json!(ADDR)),
                put_in(&ADDRESS, &address, "domain", json!("example.com")),
                put_in(&ADDRESS, &address, "origin", json!("owner")),
                put_in(&ADDRESS, &address, "row", json!("live")),
            ],
        ])
        .unwrap();
    registry
        .seal_as_older("phone", &phone, "number", NUMBER)
        .unwrap();
    registry
        .seal_as_older("address", &address, "password", SENTINEL)
        .unwrap();

    let mut app = App::headless_for_test();
    app.open_tables_tab_at(Some(lab.paths[0].clone())).unwrap();
    settle(&mut app);
    let mut words = Vec::new();

    act(&mut app, "tables/sheet/address", None);
    let password = format!("tables/cell/{address}/password");
    act(&mut app, &password, None);
    act(&mut app, &password, None);
    let ui = render(&app);
    assert!(
        said(&ui, EDIT).is_none(),
        "a sealed secret never opens here"
    );
    assert_eq!(cell_says(&ui, &address, "password"), secret::MASK);
    // The cell bar reads the selected cell as the grid does: the mask.
    assert_eq!(bar(&ui, "value").as_deref(), Some(secret::MASK));
    words.extend(every_word(&mut app, &ui));

    act(&mut app, "tables/sheet/phone", None);
    let number = format!("tables/cell/{phone}/number");
    act(&mut app, &number, None);
    act(&mut app, &number, None);
    let ui = render(&app);
    assert!(said(&ui, EDIT).is_none(), "a sealed number is read-only");
    assert_eq!(cell_says(&ui, &phone, "number"), NUMBER);
    // The cell bar reads the number as the grid does.
    assert_eq!(bar(&ui, "value").as_deref(), Some(NUMBER));
    words.extend(every_word(&mut app, &ui));

    assert!(
        words.iter().any(|w| w.contains(NUMBER)),
        "the scan reads what the page shows"
    );
    assert!(
        !words.iter().any(|w| w.contains(SENTINEL)),
        "a sealed password reached the page"
    );
}

/// In a past table a sealed number changed since reads, in the cell bar and in the pane, as the
/// grid at Now shows it: the number it is now, beside the number it was then.
/// NEGATIVE CONTROL: with `since_then` wording a sealed number's values now through
/// `shown_value` (no `opened` read), the bar reads `` now •••••• · … `` beside the clear number.
#[test]
fn a_sealed_number_changed_since_reads_as_the_number_it_is_now() {
    const THEN: &str = "5550100123";
    const NOW_IS: &str = "5550100456";
    let lab = Lab::new(&[Role::Writer]);
    let registry = Registry::at(lab.paths[0].clone());
    registry.open().unwrap();
    assert!(
        registry.generation().successor.is_some(),
        "control: a generation that holds sealed tokens"
    );
    repair_commons(&lab, 4);
    let phone = keys::random_pk(Random::Phone).unwrap();
    registry
        .write(vec![vec![
            put_in(&PHONE, &phone, "last4", json!("0123")),
            put_in(&PHONE, &phone, "row", json!("live")),
        ]])
        .unwrap();
    registry
        .seal_as_older("phone", &phone, "number", THEN)
        .unwrap();
    let then = Handle::at(lab.paths[0].clone(), &REGISTRY)
        .history()
        .unwrap()
        .last()
        .unwrap()
        .operation;
    registry
        .seal_as_older("phone", &phone, "number", NOW_IS)
        .unwrap();

    let mut app = App::headless_for_test();
    app.open_tables_tab_at(Some(lab.paths[0].clone())).unwrap();
    settle(&mut app);
    act(&mut app, "tables/sheet/phone", None);
    act(&mut app, &format!("tables/cell/{phone}/number"), None);
    assert_eq!(bar(&render(&app), "value").as_deref(), Some(NOW_IS));
    act(
        &mut app,
        &format!("tables/pane/asof/{}", aterm_codec::hex::encode_lower(&then)),
        None,
    );
    settle(&mut app);
    let ui = render(&app);
    assert_eq!(
        cell_says(&ui, &phone, "number"),
        format!("{THEN}, changed since")
    );
    assert_eq!(bar(&ui, "value").as_deref(), Some(THEN));
    let state = bar(&ui, "state").expect("what it holds now");
    assert!(
        state.starts_with(&format!("now {NOW_IS} \u{b7} ")),
        "{state}"
    );
    let line = said(&ui, "tables/pane/now/number/what").expect("the pane's line");
    assert!(
        line.contains(NOW_IS) && !line.contains(secret::MASK),
        "{line}"
    );
}

/// A sealed number two Macs wrote at once shows each of its numbers as it is, as `aterm db
/// phones` does: the cell is a conflict of two readable values, not of two masks.
/// NEGATIVE CONTROL: with `tables_store::read` reading a conflict's values through
/// `shown_value` whatever opened, the cell reads `` "•••••• / ••••••, 2 values" ``.
#[test]
fn a_conflicted_sealed_number_shows_each_number() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let a = Registry::at(lab.paths[0].clone());
    a.open().unwrap();
    let phone = keys::random_pk(Random::Phone).unwrap();
    a.write(vec![vec![
        put_in(&PHONE, &phone, "last4", json!("0123")),
        put_in(&PHONE, &phone, "row", json!("live")),
    ]])
    .unwrap();
    repair_commons(&lab, 4);
    let b = Registry::at(lab.paths[1].clone());
    a.seal_as_older("phone", &phone, "number", "5550100123")
        .unwrap();
    b.seal_as_older("phone", &phone, "number", "5550100456")
        .unwrap();
    repair_commons(&lab, 4);

    let mut app = App::headless_for_test();
    app.open_tables_tab_at(Some(lab.paths[0].clone())).unwrap();
    settle(&mut app);
    act(&mut app, "tables/sheet/phone", None);
    let ui = render(&app);
    assert_eq!(
        cell_says(&ui, &phone, "number"),
        "5550100123 / 5550100456, 2 values"
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

/// The window's Tables tab: its view, and the icon the tab wears.
fn tables_tab(
    app: &App,
) -> Option<(
    crate::tab_model::ViewId,
    Option<crate::tab_bar::TabIconKind>,
)> {
    app.windows[&WID].tab_set.tabs().iter().find_map(|tab| {
        let mut found = None;
        tab.root.any_leaf(&mut |&view| {
            if matches!(
                app.native_runtime.view_state(view),
                Some(AppViewState::Tables(_))
            ) {
                found = Some((view, tab.presentation.icon));
            }
            found.is_some()
        });
        found
    })
}

/// Registry is one row of the aterm menu, after System State…, and the same command in the
/// palette; the command opens the one Tables tab, which wears its own icon and not the Editor's;
/// and a relaunch that reads the layout a quit wrote opens the tab again, in front as it was.
/// NEGATIVE CONTROL: with the `"tables"` arm of `App::restore_native_leaf` removed, the relaunch
/// builds no Tables tab and the test fails at `the relaunch reopens Tables`.
#[test]
fn tables_opens_from_the_menu_and_palette_and_is_restored() {
    use crate::menu::{MENU_MODEL, MenuAction, MenuEntry, MenuMods};
    use crate::tab_bar::{TabIconKind, tab_icon_primitives};

    // The menu.
    let rows: Vec<_> = MENU_MODEL
        .iter()
        .flat_map(|section| {
            section
                .entries
                .iter()
                .flat_map(MenuEntry::items)
                .map(move |row| (section.title, row))
        })
        .collect();
    let at = rows
        .iter()
        .position(|(_, row)| row.1 == MenuAction::Tables)
        .expect("a Registry row");
    assert_eq!(
        rows[at],
        (
            "aterm",
            ("Registry…", MenuAction::Tables, "", MenuMods::None)
        )
    );
    assert_eq!(rows[at - 1].1.1, MenuAction::SystemState);
    assert_eq!(
        rows.iter()
            .filter(|(_, row)| row.1 == MenuAction::Tables)
            .count(),
        1,
        "no other menu has the row"
    );
    assert!(MenuAction::Tables.offered_on_this_platform());
    let chrome = crate::menu::menu_chrome_lines(None, false);
    assert!(
        chrome.iter().any(|line| line.contains("Registry…")),
        "{chrome:?}"
    );
    assert_eq!(
        MenuAction::from_tag(MenuAction::Tables.tag()),
        Some(MenuAction::Tables)
    );
    assert!(MenuAction::Tables.help().contains("Tables"));

    // The palette, and `invoke`, which reads the palette's rows.
    let mut app = App::headless_for_test();
    let mut palette = crate::palette::PaletteState::new();
    palette.resolve(&app.palette_live());
    assert_eq!(palette.action_by_name("Tables"), Ok(MenuAction::Tables));
    let row = palette
        .rows()
        .iter()
        .find(|row| row.label == "Registry…")
        .expect("a palette row");
    assert!(row.enabled && row.action == MenuAction::Tables && row.section == "aterm");
    assert_eq!(
        MenuAction::from_invoke_name("Tables"),
        Some(MenuAction::Tables)
    );
    assert_eq!(
        MenuAction::Tables.invoke_authority(),
        MenuAction::SystemState.invoke_authority()
    );
    assert!(!MenuAction::Tables.writes_pty_input());
    assert!(!crate::menu::requires_terminal_tab(MenuAction::Tables));

    // The command opens the Tables tab, with its own icon.
    assert_eq!(tables_tab(&app), None);
    let terminal_only = app.windows[&WID].tab_set.len();
    app.open_tables_from_menu();
    assert_eq!(app.pending_action_refusal, None);
    let (view, icon) = tables_tab(&app).expect("the command opens Tables");
    let front = |app: &App| app.active_native_view(WID).map(|(_, view)| view);
    assert_eq!(front(&app), Some(view));
    assert_eq!(icon, Some(TabIconKind::Tables));
    assert_eq!(
        app.live_view_presentation(view).and_then(|tab| tab.icon),
        Some(TabIconKind::Tables),
        "the page's own presentation keeps the icon"
    );
    assert_ne!(
        tab_icon_primitives(TabIconKind::Tables),
        tab_icon_primitives(TabIconKind::Editor)
    );
    assert_eq!(
        (
            TabIconKind::Tables.display_name(),
            TabIconKind::Tables.semantic_name()
        ),
        ("Tables", "tables"),
        "what hover help and `chrome` call the icon"
    );

    // A relaunch: after a quit, and into an update's next build.
    for layout in [app.capture_restore_manifest(), app.capture_handoff_layout()] {
        // Tables is a leaf of the layout only: its strict mirror of native tabs, which a
        // build from before the page cannot read a new kind in, names none.
        assert_eq!(layout.windows[0].native_tabs, []);
        let wrote = layout.to_toml().unwrap();
        let read =
            crate::restore::RestoreManifest::from_toml(&wrote).expect("the layout reads back");
        let mut cold = App::headless_for_test();
        cold.restore_into_window(WID, read.windows.into_iter().next().unwrap());
        let (reopened, icon) = tables_tab(&cold).expect("the relaunch reopens Tables");
        assert_eq!(icon, Some(TabIconKind::Tables));
        assert_eq!(front(&cold), Some(reopened), "in front, as it was");
        assert!(cold.structural_invariants_ok());
    }

    // From another app's tab the command brings the one Tables tab to the front.
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    assert_ne!(front(&app), Some(view));
    app.open_tables_from_menu();
    assert_eq!(app.pending_action_refusal, None);
    assert_eq!(front(&app), Some(view));
    assert_eq!(app.windows[&WID].tab_set.len(), terminal_only + 2);
    assert!(app.structural_invariants_ok());

    // A closed Tables tab is reopened by the same leaf a relaunch reads.
    app.close_active_native_tab(WID).unwrap();
    assert_eq!(tables_tab(&app), None);
    app.reopen_closed_tab_and_wait_for_test().unwrap();
    let (again, icon) = tables_tab(&app).expect("Reopen Closed Tab reopens Tables");
    assert_eq!(icon, Some(TabIconKind::Tables));
    assert_eq!(front(&app), Some(again));
    assert!(app.structural_invariants_ok());

    // With no window in front the command says what it could not open.
    app.frontmost_window = None;
    app.open_tables_from_menu();
    let refusal = app.pending_action_refusal.take().expect("no front window");
    assert!(refusal.contains("Tables"), "{refusal}");
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
        "mc-00000000000000ff, not written, refused: no such machine row",
        "and the cell says it"
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
        ..Shown::default()
    });
    snapshot.sheets[0].rows[0].cells.insert(
        "password".into(),
        Shown {
            text: ENTRY.into(),
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
            now: ENTRY.into(),
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

/// "Not written" is one fixed blue: the staged cell, its value in the cell bar and in the pane,
/// the rail's count, the Write bar and its button wear it whatever the accent (here a green
/// one), and nothing else on the page does.
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
        "tables/cellbar/value/words".into(),
    ]
    .into_iter()
    .collect();
    assert_eq!(wears, staged, "only what is not written wears it");
}

/// A sheet of `rows` rows under `columns` wide columns, `label` its key column: row `n`'s
/// label is `m<n>`.
pub(crate) fn tall_sheet(rows: usize, columns: usize) -> Snapshot {
    let mut snapshot = wide_sheet(columns);
    let sheet = &mut snapshot.sheets[0];
    let template = sheet.rows[0].clone();
    sheet.rows = (0..rows)
        .map(|n| {
            let mut row = template.clone();
            row.pk = pk_of(n);
            row.cells.insert(
                "label".to_owned(),
                Shown {
                    text: format!("m{n}"),
                    ..Shown::default()
                },
            );
            row
        })
        .collect();
    snapshot
}

/// [`tall_sheet`] with one short word in every cell beside the label, so each column is as
/// narrow as a column starts and several show in a small window.
fn narrow_sheet(rows: usize, columns: usize) -> Snapshot {
    let mut snapshot = tall_sheet(rows, columns);
    for row in &mut snapshot.sheets[0].rows {
        for (name, cell) in &mut row.cells {
            if name != "label" {
                cell.text = "v".into();
            }
        }
    }
    snapshot
}

/// Where the grid lays `key` out, whole: what it paints, or its place off the grid's screen.
pub(crate) fn place(ui: &CompiledUi, key: &str) -> LogicalRect {
    ui.paint
        .iter()
        .find(|node| node.key.as_str() == key)
        .map_or_else(|| rect(ui, key), |node| node.rect)
}

/// The key of row `n` of [`tall_sheet`].
pub(crate) fn pk_of(n: usize) -> String {
    format!("mc-{n:016x}")
}

/// The style group `key` is drawn in.
fn group_style_of(tree: &UiTree, key: &str) -> Option<StyleRef> {
    fn find<'a>(node: &'a UiNode, key: &str) -> Option<&'a UiNode> {
        if node.key.as_str() == key {
            return Some(node);
        }
        node.children.iter().find_map(|child| find(child, key))
    }
    match &find(&tree.root, key)?.content {
        UiContent::Group(spec) => Some(spec.style),
        _ => None,
    }
}

/// The grid is a sheet: every cell is ruled and of one height, each row is numbered at its left
/// by where it shows, the header line stays put while the rows scroll under it, and the selected
/// cell wears a ring in the page's primary ink, never the "not written" blue.
#[test]
fn the_grid_rules_every_cell_and_numbers_its_rows() {
    let mut app = open_with(tall_sheet(80, 3));
    let drawn = tree(&app);
    for n in [0, 1, 7] {
        for column in ["label", "c1", "c2"] {
            assert_eq!(
                group_style_of(&drawn, &format!("tables/cell/{}/{column}/seat", pk_of(n))),
                Some(StyleRef::Cell),
                "row {n} {column}"
            );
        }
        assert_eq!(
            group_style_of(&drawn, &format!("tables/num/{}", pk_of(n))),
            Some(StyleRef::CellHead)
        );
    }
    for head in [
        "tables/head/#/seat",
        "tables/head/label/seat",
        "tables/head/c2/seat",
    ] {
        assert_eq!(
            group_style_of(&drawn, head),
            Some(StyleRef::CellHead),
            "{head}"
        );
    }
    let ui = render(&app);
    assert_eq!(said(&ui, "tables/head/#").as_deref(), Some("#"));
    for n in [0, 1, 7] {
        assert_eq!(
            said(&ui, &format!("tables/num/{}/n", pk_of(n))),
            Some((n + 1).to_string()),
            "a row is numbered by where it shows"
        );
    }
    // One height, each row right under the one before, each column right beside the one before.
    let cell = |ui: &CompiledUi, n: usize, column: &str| {
        rect(ui, &format!("tables/cell/{}/{column}/seat", pk_of(n)))
    };
    let (first, second, third) = (cell(&ui, 0, "c1"), cell(&ui, 1, "c1"), cell(&ui, 2, "c1"));
    assert_eq!(first.height, second.height);
    assert_eq!(second.y, first.y + first.height);
    assert_eq!(third.y, second.y + second.height);
    assert_eq!(cell(&ui, 0, "c2").x, first.x + first.width);
    assert_eq!(
        rect(&ui, "tables/head/c1/seat").x,
        first.x,
        "a header over its column"
    );
    assert_eq!(rect(&ui, "tables/head/c1/seat").width, first.width);

    // The rows scroll under the header line, the row numbers with them.
    let head = rect(&ui, "tables/head/c1/seat");
    app.dispatch_native_event(WID, AppEvent::ScrollLines(5))
        .unwrap();
    let ui = render(&app);
    assert_eq!(
        rect(&ui, "tables/head/c1/seat"),
        head,
        "the header stays put"
    );
    assert_eq!(rect(&ui, "tables/head/#/seat").y, head.y);
    assert_eq!(
        cell(&ui, 5, "c1").y,
        first.y,
        "the sixth row is where the first was"
    );
    assert_eq!(
        rect(&ui, &format!("tables/num/{}", pk_of(5))).y,
        first.y,
        "its number with it"
    );

    // The selected cell: a ring in the primary ink over it, and nothing of it blue.
    let key = format!("tables/cell/{}/c1", pk_of(6));
    act(&mut app, &key, None);
    assert_eq!(
        group_style_of(&tree(&app), &format!("{key}/focus")),
        Some(StyleRef::CellSelected)
    );
    let roles = crate::settings::Roles::from_theme(aterm_render::Theme::default());
    let painted = render(&app).painted(roles);
    let ring = painted
        .iter()
        .find(|(painted, _)| painted.as_str() == format!("{key}/focus"))
        .map(|(_, prims)| prims.clone())
        .expect("the ring is drawn");
    assert!(
        ring.iter().any(|prim| matches!(
            prim,
            crate::widget::DrawPrim::Stroke { color, radius, .. }
                if [color[0], color[1], color[2]] == roles.text_primary && *radius == 0.0
        )),
        "{ring:?}"
    );
}

/// The page as the window's last frame on glass compiled it.
pub(crate) fn presented(app: &mut App) -> CompiledUi {
    let (_, view) = app.active_native_view(WID).expect("the Tables tab");
    assert!(app.prepare_native_input_scratch(WID));
    app.windows
        .get_mut(&WID)
        .as_deref_mut()
        .and_then(|window| window.leaf_render_cache.get_mut(&view))
        .and_then(|cache| cache.native.as_mut())
        .expect("the page's frame")
        .presented = true;
    app.retained_native_leaf_artifact(WID, view, true)
        .expect("the page's frame on glass")
        .compiled
        .clone()
}

/// The window pixel of the page's point `(x, y)`.
fn window_px(app: &mut App, x: f32, y: f32) -> (f64, f64) {
    presented(app);
    let (_, view) = app.active_native_view(WID).unwrap();
    let artifact = app.retained_native_leaf_artifact(WID, view, true).unwrap();
    (
        artifact.device_x as f64 + f64::from(x) * artifact.scale,
        artifact.device_y as f64 + f64::from(y) * artifact.scale,
    )
}

/// Put the pointer on the middle of `key` as the window shows it.
fn point_at(app: &mut App, key: &str) {
    let ui = presented(app);
    let at = ui
        .hits
        .iter()
        .find(|hit| hit.key.as_str() == key)
        .map(|hit| hit.rect)
        .unwrap_or_else(|| rect(&ui, key));
    let (x, y) = window_px(app, at.x + at.width / 2.0, at.y + at.height / 2.0);
    app.on_cursor_moved(WID, x, y);
}

/// A sideways wheel over the grid scrolls its columns across beside the row numbers and the key
/// column, their headers with them; a wheel back returns them.
#[test]
fn a_sideways_wheel_scrolls_columns_and_keeps_headers_and_key_column() {
    use aterm_window::event::MouseScrollDelta;
    let mut app = open_with(narrow_sheet(4, 24));
    let label = format!("tables/cell/{}/label/seat", pk_of(1));
    let first = format!("tables/cell/{}/c1/seat", pk_of(1));
    let before = presented(&mut app);
    point_at(&mut app, &format!("tables/cell/{}/label", pk_of(1)));
    // Whichever way a notch of this wheel turns, the other way undoes it.
    let mut moved = None;
    for notch in [1.0, -1.0] {
        app.on_mouse_wheel(WID, MouseScrollDelta::LineDelta(notch, 0.0));
        let after = presented(&mut app);
        if place(&after, &first).x < place(&before, &first).x {
            moved = Some((notch, after));
            break;
        }
    }
    let (notch, after) = moved.expect("a sideways wheel scrolls the columns across");
    for stays in [
        label.as_str(),
        "tables/head/label/seat",
        "tables/head/#/seat",
        &format!("tables/num/{}", pk_of(1)),
    ] {
        assert_eq!(place(&after, stays), place(&before, stays), "{stays} stays");
    }
    assert_eq!(
        place(&after, "tables/head/c1/seat").x,
        place(&after, &first).x,
        "a header scrolls with its column"
    );
    assert_eq!(place(&after, &first).y, place(&before, &first).y);
    // The far end: the last column whole in the grid, and no further.
    for _ in 0..200 {
        app.on_mouse_wheel(WID, MouseScrollDelta::LineDelta(notch, 0.0));
    }
    let end = presented(&mut app);
    let grid = place(&end, "tables/grid");
    let last = place(&end, &format!("tables/cell/{}/c23/seat", pk_of(1)));
    assert!(
        (last.x + last.width - (grid.x + grid.width - 1.0)).abs() <= 1.0,
        "the last column ends at the grid's edge: {last:?} in {grid:?}"
    );
    for _ in 0..200 {
        app.on_mouse_wheel(WID, MouseScrollDelta::LineDelta(-notch, 0.0));
    }
    assert_eq!(
        place(&presented(&mut app), &first),
        place(&before, &first),
        "a wheel back returns them"
    );
    let staged = &app.native_runtime.tables_mut().unwrap().staged;
    assert!(staged.is_empty(), "a scroll stages nothing");
}

/// A column's header edge is held and dragged to size the column: the column follows the pointer
/// while it is held, off the edge's own few points too, keeps the width at the release and
/// across a new read, and the columns after it move over; the key column's edge sizes it too.
#[test]
fn dragging_a_header_edge_resizes_its_column() {
    use aterm_window::event::{ElementState, MouseButton};
    let mut app = open_with(narrow_sheet(4, 6));
    let seat =
        |app: &mut App, column: &str| place(&presented(app), &format!("tables/head/{column}/seat"));
    let scale = {
        presented(&mut app);
        let (_, view) = app.active_native_view(WID).unwrap();
        app.retained_native_leaf_artifact(WID, view, true)
            .unwrap()
            .scale
    };
    for column in ["c1", "label"] {
        let (before, after_before) = (seat(&mut app, column), seat(&mut app, "c2"));
        let edge = format!("tables/edge/{column}");
        assert_eq!(
            said(&presented(&mut app), &edge),
            Some(format!(
                "Fit {} column to its widest value",
                if column == "c1" { "C1" } else { "Label" }
            ))
        );
        point_at(&mut app, &edge);
        let (x, y) = app.windows[&WID].last_cursor_px;
        app.on_mouse_input(WID, ElementState::Pressed, MouseButton::Left);
        // Held, the column follows the pointer: narrower, then wider than it began.
        app.on_cursor_moved(WID, x - 12.0 * scale, y + 40.0 * scale);
        assert_eq!(
            seat(&mut app, column).width,
            before.width - 12.0,
            "{column} follows the pointer while it is held"
        );
        app.on_cursor_moved(WID, x + 24.0 * scale, y);
        assert_eq!(seat(&mut app, column).width, before.width + 24.0);
        app.on_mouse_input(WID, ElementState::Released, MouseButton::Left);
        let after = seat(&mut app, column);
        assert_eq!(after.width, before.width + 24.0, "{column} keeps its width");
        assert_eq!(after.x, before.x);
        assert_eq!(
            seat(&mut app, "c2").x,
            after_before.x + 24.0,
            "the columns after it move over"
        );
        // Released, the pointer's travel sizes nothing more.
        app.on_cursor_moved(WID, x + 90.0 * scale, y);
        assert_eq!(seat(&mut app, column).width, before.width + 24.0);
        // A new read of the registry keeps the width: it is the view's, not the read's.
        app.native_runtime
            .tables_mut()
            .unwrap()
            .show(narrow_sheet(5, 6));
        assert_eq!(seat(&mut app, column).width, before.width + 24.0);
    }
    // A column is never dragged away: it keeps a width a pointer can find again.
    point_at(&mut app, "tables/edge/c1");
    let (x, y) = app.windows[&WID].last_cursor_px;
    app.on_mouse_input(WID, ElementState::Pressed, MouseButton::Left);
    app.on_cursor_moved(WID, x - 5_000.0, y);
    app.on_mouse_input(WID, ElementState::Released, MouseButton::Left);
    assert_eq!(seat(&mut app, "c1").width, NARROWEST * super::scale());
    assert!(
        app.native_runtime.tables_mut().unwrap().staged.is_empty(),
        "a width stages nothing"
    );
}

/// A double press on a column's header edge, or a press of it from the keyboard, fits the column
/// to its header and its widest value in every row, past the bound a column starts within.
#[test]
fn a_double_press_fits_the_column() {
    use aterm_window::event::{ElementState, MouseButton};
    const LONG: &str = "a value wider than a column starts out, in the last of the sheet's rows";
    let mut snapshot = narrow_sheet(300, 3);
    snapshot.sheets[0].rows[299].cells.insert(
        "c1".to_owned(),
        Shown {
            text: LONG.into(),
            ..Shown::default()
        },
    );
    let fits = text_width(LONG, SemanticRole::Text, StyleRef::Plain) + 20.0;
    let mut app = open_with(snapshot);
    let width = |app: &mut App| place(&presented(app), "tables/head/c1/seat").width;
    let began = width(&mut app);
    assert!(began < fits, "the column starts within its bound");
    point_at(&mut app, "tables/edge/c1");
    let at = std::time::Instant::now();
    app.on_mouse_input_at(WID, ElementState::Pressed, MouseButton::Left, at);
    app.on_mouse_input_at(WID, ElementState::Released, MouseButton::Left, at);
    assert_eq!(width(&mut app), began, "one press sizes nothing");
    app.on_mouse_input_at(WID, ElementState::Pressed, MouseButton::Left, at);
    app.on_mouse_input_at(WID, ElementState::Released, MouseButton::Left, at);
    assert_eq!(width(&mut app), fits, "a double press fits the column");

    // From the keyboard: the edge is a control, and its press fits its column.
    let mut app = open_with(tall_sheet(300, 3));
    let began = place(&render(&app), "tables/head/c2/seat").width;
    act(&mut app, "tables/edge/c2", None);
    let fitted = place(&render(&app), "tables/head/c2/seat").width;
    assert!(fitted > began, "{fitted} from {began}");
    assert_eq!(
        fitted,
        text_width(
            "a value wide enough to fill its column to the page's cap",
            SemanticRole::Text,
            StyleRef::Plain
        ) + 20.0
    );
}

/// [`narrow_sheet`] of five rows whose `c1` holds `values` in the stored order.
fn sheet_with(values: [&str; 5], kind: ColumnKind) -> Snapshot {
    let mut snapshot = narrow_sheet(5, 3);
    let sheet = &mut snapshot.sheets[0];
    sheet.columns[1].kind = kind;
    for (row, value) in sheet.rows.iter_mut().zip(values) {
        row.cells.insert(
            "c1".to_owned(),
            Shown {
                text: value.into(),
                ..Shown::default()
            },
        );
    }
    snapshot
}

/// Which stored row shows at each line of the grid, top down, read from the row numbers.
fn order(app: &App) -> Vec<usize> {
    let ui = render(app);
    let mut shown: Vec<(usize, usize)> = (0..5)
        .filter_map(|n| {
            let line = said(&ui, &format!("tables/num/{}/n", pk_of(n)))?;
            Some((line.parse().expect("a row number"), n))
        })
        .collect();
    shown.sort_unstable();
    shown.into_iter().map(|(_, n)| n).collect()
}

/// The sort mark `column`'s header wears: the shape drawn there. The mark is the header's face
/// alone, drawn as bars and never as a glyph of the text face; its state is said in the
/// header's name.
fn mark(app: &App, column: &str) -> Option<StyleRef> {
    let key = format!("tables/head/{column}/mark");
    let style = group_style_of(&tree(app), &key)?;
    let roles = crate::settings::Roles::from_theme(aterm_render::Theme::default());
    let prims = render(app)
        .painted(roles)
        .into_iter()
        .find(|(painted, _)| painted.as_str() == key)
        .map(|(_, prims)| prims)
        .unwrap_or_else(|| panic!("{key} is painted"));
    // Three bars in the primary ink, the narrowest at the point: above the widest for a mark
    // that points up, below it for one that points down.
    let bars: Vec<(f32, f32)> = prims
        .iter()
        .map(|prim| match prim {
            crate::widget::DrawPrim::Panel { y, w, fill, .. } => {
                assert_eq!([fill[0], fill[1], fill[2]], roles.text_primary);
                (*y, *w)
            }
            other => panic!("the mark is drawn of bars: {other:?}"),
        })
        .collect();
    assert_eq!(bars.len(), 3, "{bars:?}");
    let narrowest = bars
        .iter()
        .copied()
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .unwrap();
    let widest = bars
        .iter()
        .copied()
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .unwrap();
    assert_eq!(
        narrowest.0 < widest.0,
        style == StyleRef::SortUp,
        "{style:?} points the way it says: {bars:?}"
    );
    Some(style)
}

pub(crate) fn press_key(app: &mut App, named: aterm_types::keyboard::NamedKey) {
    assert!(app.native_input_event(
        WID,
        &crate::input::InputEvent::Key {
            key: aterm_types::keyboard::Key::Named(named),
            mods: aterm_types::keyboard::Modifiers::empty(),
            base_layout: None,
            layout_text: None,
            event_type: aterm_types::keyboard::KeyEventType::Press,
        }
    ));
}

/// A press on a column's header orders the rows by it from its least value up, a second from
/// its greatest down, a third as the registry holds them; the header says which and wears a
/// mark, an empty cell is last both ways, and rows the column holds equal keep their order. The
/// pointer, the keyboard and an accessibility client's press all sort. The order is the view's:
/// nothing is staged, the sheet read is untouched, and it holds across a new read.
#[test]
fn a_header_press_sorts_ascending_then_descending_then_stored_order() {
    use aterm_types::keyboard::NamedKey;
    use aterm_window::event::{ElementState, MouseButton};
    let read = sheet_with(["pear", "Apple", "", "banana", "pear"], ColumnKind::Text);
    let mut app = open_with(read.clone());
    let head = "tables/head/c1";
    let ui = render(&app);
    assert_eq!(order(&app), [0, 1, 2, 3, 4]);
    assert_eq!(
        said(&ui, head).as_deref(),
        Some("Sort by C1"),
        "a header's name says what its press does"
    );
    let node = ui.semantic(&UiKey::new(head.to_owned())).unwrap();
    assert_eq!(node.role, SemanticRole::Button, "a header is a press");
    assert!(
        ui.focus_order.iter().any(|key| key.as_str() == head),
        "Tab reaches a header"
    );
    assert_eq!(mark(&app, "c1"), None, "no mark yet");

    // The pointer: from the least value up, words without regard to case.
    point_at(&mut app, head);
    app.on_mouse_input(WID, ElementState::Pressed, MouseButton::Left);
    app.on_mouse_input(WID, ElementState::Released, MouseButton::Left);
    assert_eq!(
        order(&app),
        [1, 3, 0, 4, 2],
        "ascending, the empty cell last"
    );
    let ui = render(&app);
    assert_eq!(
        said(&ui, head).as_deref(),
        Some("Sort by C1, sorted ascending")
    );
    assert_eq!(mark(&app, "c1"), Some(StyleRef::SortUp));
    assert_eq!(
        said(&ui, "tables/head/label").as_deref(),
        Some("Sort by Label"),
        "one column is sorted at a time"
    );

    // The keyboard: focus on the header, and Enter presses it.
    app.dispatch_native_event(
        WID,
        AppEvent::FocusChanged(Some(UiKey::new(head.to_owned()))),
    )
    .unwrap();
    press_key(&mut app, NamedKey::Enter);
    assert_eq!(
        order(&app),
        [0, 4, 3, 1, 2],
        "descending, the empty cell last"
    );
    let ui = render(&app);
    assert_eq!(
        said(&ui, head).as_deref(),
        Some("Sort by C1, sorted descending")
    );
    assert_eq!(mark(&app, "c1"), Some(StyleRef::SortDown));

    // A new read keeps the order: it is the view's, not the read's.
    let mut again = read.clone();
    again.sheets[0].rows[2].cells.insert(
        "c1".to_owned(),
        Shown {
            text: "zebra".into(),
            ..Shown::default()
        },
    );
    app.native_runtime.tables_mut().unwrap().show(again.clone());
    assert_eq!(
        order(&app),
        [2, 0, 4, 3, 1],
        "the new value takes its place"
    );
    assert_eq!(
        said(&render(&app), head).as_deref(),
        Some("Sort by C1, sorted descending")
    );

    // An accessibility client's press: the order the registry holds.
    act(&mut app, head, None);
    assert_eq!(order(&app), [0, 1, 2, 3, 4], "the stored order again");
    let ui = render(&app);
    assert_eq!(said(&ui, head).as_deref(), Some("Sort by C1"));
    assert_eq!(mark(&app, "c1"), None);

    // Another column's header takes the order over from the first.
    act(&mut app, head, None);
    act(&mut app, "tables/head/label", None);
    act(&mut app, "tables/head/label", None);
    assert_eq!(order(&app), [4, 3, 2, 1, 0], "m4 down to m0");
    let ui = render(&app);
    assert_eq!(said(&ui, head).as_deref(), Some("Sort by C1"));
    assert_eq!(
        said(&ui, "tables/head/label").as_deref(),
        Some("Sort by Label, sorted descending")
    );

    let tables = app.native_runtime.tables_mut().unwrap();
    assert!(tables.staged.is_empty(), "a sort stages nothing");
    assert_eq!(
        tables.sheet("machine"),
        Some(&again.sheets[0]),
        "the sheet read is as it was read"
    );
}

/// How two cells' words stand in a column of `kind`, as the grid orders them.
fn compare(kind: ColumnKind, a: &str, b: &str) -> std::cmp::Ordering {
    Rank::of(kind, a).cmp(&Rank::of(kind, b))
}

/// How two cells' words stand in a column of words.
fn natural(a: &str, b: &str) -> std::cmp::Ordering {
    compare(ColumnKind::Text, a, b)
}

/// A column is ordered by what it holds: whole numbers and times by their value, not their
/// digits; words by their letters whatever their case, a run of digits in them by its value;
/// an empty cell last both ways. A cell stands by what the registry holds, never by words
/// staged in it: the staged words take their place once they are written and read back.
#[test]
fn numbers_and_times_sort_by_value_and_empty_cells_last() {
    use std::cmp::Ordering;
    let sorted = |values: [&str; 5], kind: ColumnKind, presses: usize| {
        let mut app = open_with(sheet_with(values, kind));
        for _ in 0..presses {
            act(&mut app, "tables/head/c1", None);
        }
        order(&app)
    };
    let numbers = ["10", "9", "", "-3", "100"];
    assert_eq!(sorted(numbers, ColumnKind::Number, 1), [3, 1, 0, 4, 2]);
    assert_eq!(sorted(numbers, ColumnKind::Number, 2), [4, 0, 1, 3, 2]);
    // The column's kind decides: the same cells in a column of words stand by their letters.
    let below = ["-3", "-10", "", "5", "1"];
    assert_eq!(sorted(below, ColumnKind::Number, 1), [1, 0, 4, 3, 2]);
    assert_eq!(sorted(below, ColumnKind::Text, 1), [0, 1, 4, 3, 2]);
    let times = ["1700000000000", "999", "", "1700000000001", "86400000"];
    assert_eq!(sorted(times, ColumnKind::Time, 1), [1, 4, 0, 3, 2]);
    assert_eq!(sorted(times, ColumnKind::Time, 2), [3, 0, 4, 1, 2]);
    let labels = ["m10", "m9", "", "M2", "m55"];
    assert_eq!(sorted(labels, ColumnKind::Text, 1), [3, 1, 0, 4, 2]);
    assert_eq!(sorted(labels, ColumnKind::Text, 2), [4, 0, 1, 3, 2]);

    // A staged cell leaves its row where the value read put it; written and read back, the
    // row stands where the new value puts it.
    let mut app = open_with(sheet_with(numbers, ColumnKind::Number));
    act(&mut app, "tables/head/c1", None);
    let tables = app.native_runtime.tables_mut().unwrap();
    tables.stage("machine", &pk_of(2), "c1", "50".into());
    tables.stage("machine", &pk_of(4), "c1", "0".into());
    assert_eq!(
        order(&app),
        [3, 1, 0, 4, 2],
        "the rows stay as read: -3, 9, 10, 100, empty"
    );
    let tables = app.native_runtime.tables_mut().unwrap();
    tables.show(sheet_with(["10", "9", "50", "-3", "0"], ColumnKind::Number));
    assert_eq!(order(&app), [3, 4, 1, 0, 2], "-3, 0, 9, 10, 50");

    assert_eq!(compare(ColumnKind::Number, "9", "10"), Ordering::Less);
    assert_eq!(compare(ColumnKind::Number, "-3", "2"), Ordering::Less);
    assert_eq!(
        compare(ColumnKind::Number, "7", "not a number"),
        Ordering::Less,
        "a number before what is none"
    );
    assert_eq!(natural("rack 2", "Rack 10"), Ordering::Less);
    assert_eq!(natural("m007", "m7"), Ordering::Less, "equal but for case");
    assert_eq!(natural("a", "ab"), Ordering::Less);
    assert_eq!(natural("b", "A"), Ordering::Greater);
}

/// Four rows of four columns, each cell beside the label holding its own words (`r1c2`).
fn lettered_sheet() -> Snapshot {
    let mut snapshot = narrow_sheet(4, 4);
    for (n, row) in snapshot.sheets[0].rows.iter_mut().enumerate() {
        for (name, cell) in &mut row.cells {
            if name != "label" {
                cell.text = format!("r{n}{name}");
            }
        }
    }
    snapshot
}

pub(crate) fn key_with(
    app: &mut App,
    key: aterm_types::keyboard::Key,
    mods: aterm_types::keyboard::Modifiers,
) {
    let layout_text = match key {
        aterm_types::keyboard::Key::Character(c) => Some(c),
        _ => None,
    };
    assert!(
        app.native_input_event(
            WID,
            &crate::input::InputEvent::Key {
                key: key.clone(),
                mods,
                base_layout: None,
                layout_text,
                event_type: aterm_types::keyboard::KeyEventType::Press,
            }
        ),
        "the page takes {key:?}"
    );
}

/// The page's view: its selected cell (row number in [`lettered_sheet`], column), whether a
/// cell is open, and where its focus is.
fn selected(app: &App) -> (usize, String, bool, String) {
    let (_, view) = app.active_native_view(WID).unwrap();
    let Some(AppViewState::Tables(state)) = app.native_runtime.view_state(view) else {
        panic!("the Tables page is in front");
    };
    let pk = state.row.clone().expect("a row is selected");
    let row = (0..4)
        .find(|n| pk_of(*n) == pk)
        .expect("a row of the sheet");
    (
        row,
        state.column.clone().expect("a column is selected"),
        state.editing.is_some(),
        state
            .common
            .last_focus
            .as_ref()
            .map(|key| key.as_str().to_owned())
            .unwrap_or_default(),
    )
}

/// The keys of a spreadsheet, through the window's key path: the arrows move the selection a
/// cell, Tab and Shift-Tab a cell across and onto the next row, and off the sheet's last cell
/// Tab leaves the grid; Enter opens a cell and, in an open one, keeps what was typed and moves
/// down; Escape leaves the cell as it was; a typed letter opens the cell with it; Delete stages
/// nothing in a cell that takes an edit.
#[test]
fn spreadsheet_keys_move_open_and_empty_cells() {
    use aterm_types::keyboard::{Key, Modifiers, NamedKey};
    let none = Modifiers::empty();
    let mut app = open_with(lettered_sheet());
    let cell = |n: usize, column: &str| format!("tables/cell/{}/{column}", pk_of(n));
    let at = |app: &App| {
        let (row, column, ..) = selected(app);
        (row, column)
    };
    act(&mut app, &cell(0, "c1"), None);
    for (key, to) in [
        (NamedKey::ArrowDown, (1, "c1")),
        (NamedKey::ArrowRight, (1, "c2")),
        (NamedKey::ArrowUp, (0, "c2")),
        (NamedKey::ArrowLeft, (0, "c1")),
        (NamedKey::ArrowUp, (0, "c1")),
    ] {
        key_with(&mut app, Key::Named(key), none);
        assert_eq!(at(&app), (to.0, to.1.to_owned()), "{key:?}");
    }
    assert_eq!(
        selected(&app).3,
        cell(0, "c1"),
        "focus is the selected cell"
    );

    // Tab: across, then the next row's first cell; Shift-Tab back.
    for to in [(0, "c2"), (0, "c3"), (1, "label")] {
        key_with(&mut app, Key::Named(NamedKey::Tab), none);
        assert_eq!(at(&app), (to.0, to.1.to_owned()), "Tab");
    }
    key_with(&mut app, Key::Named(NamedKey::Tab), Modifiers::SHIFT);
    assert_eq!(at(&app), (0, "c3".to_owned()), "Shift-Tab");
    // Off the sheet's last cell Tab goes on to the next control, as from any control.
    act(&mut app, &cell(3, "c3"), None);
    key_with(&mut app, Key::Named(NamedKey::Tab), none);
    let focus = selected(&app).3;
    assert!(
        !focus.starts_with("tables/cell/") && focus != "tables/edit",
        "Tab left the grid: {focus}"
    );

    // Enter opens the cell; what is typed is kept at the next Enter, which moves down.
    act(&mut app, &cell(0, "c1"), None);
    key_with(&mut app, Key::Named(NamedKey::Enter), none);
    assert_eq!(
        selected(&app),
        (0, "c1".to_owned(), true, "tables/edit".to_owned())
    );
    key_in(&mut app, TextInputEvent::Commit("rack c".into()));
    key_with(&mut app, Key::Named(NamedKey::Enter), none);
    assert_eq!(selected(&app), (1, "c1".to_owned(), false, cell(1, "c1")));
    let staged = |app: &mut App, n: usize, column: &str| {
        app.native_runtime
            .tables_mut()
            .unwrap()
            .staged
            .get(&("machine".to_owned(), pk_of(n), column.to_owned()))
            .cloned()
    };
    assert_eq!(staged(&mut app, 0, "c1").as_deref(), Some("rack c"));

    // A typed letter opens the cell with it; Escape leaves the cell as it was.
    key_with(&mut app, Key::Character('q'), none);
    assert_eq!(
        selected(&app),
        (1, "c1".to_owned(), true, "tables/edit".to_owned())
    );
    key_with(&mut app, Key::Named(NamedKey::Escape), none);
    assert_eq!(selected(&app), (1, "c1".to_owned(), false, cell(1, "c1")));
    assert_eq!(staged(&mut app, 1, "c1"), None, "Escape keeps nothing");

    // Tab in an open cell keeps what was typed and moves across.
    key_with(&mut app, Key::Character('q'), none);
    key_with(&mut app, Key::Named(NamedKey::Tab), none);
    assert_eq!(selected(&app), (1, "c2".to_owned(), false, cell(1, "c2")));
    assert_eq!(staged(&mut app, 1, "c1").as_deref(), Some("q"));

    // Delete stages nothing in the cell, which the grid then shows empty and not written.
    key_with(&mut app, Key::Named(NamedKey::Delete), none);
    assert_eq!(staged(&mut app, 1, "c2").as_deref(), Some(""));
    assert_eq!(
        said(&render(&app), &cell(1, "c2")).as_deref(),
        Some(format!("C2 of {}: empty, not written", pk_of(1)).as_str())
    );
    // A column the page does not edit keeps its value.
    let mut fixed = lettered_sheet();
    fixed.sheets[0].columns[2].editable = false;
    let mut app = open_with(fixed);
    act(&mut app, &cell(1, "c2"), None);
    key_with(&mut app, Key::Named(NamedKey::Delete), none);
    key_with(&mut app, Key::Named(NamedKey::Backspace), none);
    assert!(app.native_runtime.tables_mut().unwrap().staged.is_empty());
}

/// Shift and an arrow grow the selection into a range, which a copy takes as tab-separated
/// lines of what the grid shows, a staged cell by its staged words; one cell copies as its
/// words alone; a range's cells are said selected and washed, the selected one ringed; a press
/// on a cell is one cell again. A secret's cell copies the entry name it shows, never a value.
#[test]
fn a_range_of_cells_copies_as_tab_separated_lines() {
    use aterm_types::keyboard::{Key, Modifiers, NamedKey};
    let mut app = open_with(lettered_sheet());
    let cell = |n: usize, column: &str| format!("tables/cell/{}/{column}", pk_of(n));
    act(&mut app, &cell(0, "c1"), None);
    assert_eq!(app.native_selection_text(WID).as_deref(), Some("r0c1"));
    key_with(&mut app, Key::Named(NamedKey::ArrowRight), Modifiers::SHIFT);
    key_with(&mut app, Key::Named(NamedKey::ArrowDown), Modifiers::SHIFT);
    key_with(&mut app, Key::Named(NamedKey::ArrowDown), Modifiers::SHIFT);
    assert_eq!(
        app.native_selection_text(WID).as_deref(),
        Some("r0c1\tr0c2\nr1c1\tr1c2\nr2c1\tr2c2")
    );
    let ui = render(&app);
    assert_eq!(
        said(&ui, &cell(1, "c1")).as_deref(),
        Some(format!("C1 of {}: r1c1, selected", pk_of(1)).as_str())
    );
    assert_eq!(
        said(&ui, &cell(3, "c1")).as_deref(),
        Some(format!("C1 of {}: r3c1", pk_of(3)).as_str()),
        "a cell outside the range"
    );
    let drawn = tree(&app);
    assert_eq!(
        group_style_of(&drawn, &format!("{}/range", cell(1, "c1"))),
        Some(StyleRef::CellRange)
    );
    assert_eq!(
        group_style_of(&drawn, &format!("{}/focus", cell(2, "c2"))),
        Some(StyleRef::CellSelected),
        "the ring is on the cell the arrows moved"
    );
    assert_eq!(
        group_style_of(&drawn, &format!("{}/range", cell(3, "c1"))),
        None
    );
    // The range shrinks back the way it grew, and a staged cell copies as its staged words.
    key_with(&mut app, Key::Named(NamedKey::ArrowUp), Modifiers::SHIFT);
    app.native_runtime
        .tables_mut()
        .unwrap()
        .stage("machine", &pk_of(1), "c2", "a\tb".into());
    assert_eq!(
        app.native_selection_text(WID).as_deref(),
        Some("r0c1\tr0c2\nr1c1\t\"a\tb\""),
        "a tab inside a cell stays in its one field, set between quotes"
    );
    // A press on a cell is one cell again.
    act(&mut app, &cell(3, "c3"), None);
    assert_eq!(app.native_selection_text(WID).as_deref(), Some("r3c3"));
    assert_eq!(
        group_style_of(&tree(&app), &format!("{}/range", cell(1, "c1"))),
        None
    );

    let mut app = open_with(one_sheet(Shown {
        text: "Spare".into(),
        ..Shown::default()
    }));
    act(&mut app, &format!("tables/cell/{MC}/password"), None);
    assert_eq!(app.native_selection_text(WID).as_deref(), Some(ENTRY));
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

/// The page's view state.
pub(crate) fn view_of(app: &App) -> &TablesViewState {
    let (_, view) = app.active_native_view(WID).unwrap();
    let Some(AppViewState::Tables(state)) = app.native_runtime.view_state(view) else {
        panic!("the Tables page is in front");
    };
    state
}

/// The page's view state, to set what a person's drag would.
pub(crate) fn with_view_mut(app: &mut App, set: impl FnOnce(&mut TablesViewState)) {
    let (_, view) = app.active_native_view(WID).unwrap();
    let mut guard = app.native_runtime.view_state_mut(view);
    let Some(AppViewState::Tables(state)) = guard.as_deref_mut() else {
        panic!("the Tables page is in front");
    };
    set(state);
}

/// Every staged cell of the machine sheet: (stored row, column) and its staged words.
fn staged_cells(app: &mut App) -> Vec<((usize, String), String)> {
    app.native_runtime
        .tables_mut()
        .unwrap()
        .staged
        .iter()
        .map(|((_, pk, column), text)| {
            let row = (0..400)
                .find(|n| pk_of(*n) == *pk)
                .expect("a row of the sheet");
            ((row, column.clone()), text.clone())
        })
        .collect()
}

/// Typing into the column the rows are sorted by leaves every row where it is: Return lands on
/// the row that was below and Tab on the cell beside, so the next value typed goes into the
/// cell the person is looking at. The staged words take their place once written and read.
/// NEGATIVE CONTROL: with the order keyed on the staged words and worked out at every call,
/// the edited row jumps between `b` and `d`, Return lands on the row holding `d`, and the
/// second value is staged over it: `assertion `left == right` failed: the rows stay where they
/// are  left: [4, 0, 1, 3, 2]  right: [4, 0, 3, 1, 2]`.
#[test]
fn an_edit_in_the_sorted_column_leaves_every_row_in_place() {
    use aterm_types::keyboard::{Key, Modifiers, NamedKey};
    let none = Modifiers::empty();
    let mut app = open_with(sheet_with(["b", "", "", "d", "a"], ColumnKind::Text));
    act(&mut app, "tables/head/c1", None);
    let sorted = [4, 0, 3, 1, 2];
    assert_eq!(order(&app), sorted, "a, b, d, then the two empty cells");
    let cell = |n: usize, column: &str| format!("tables/cell/{}/{column}", pk_of(n));

    // Fill the first empty cell; Return moves to the second, which takes the next value.
    act(&mut app, &cell(1, "c1"), None);
    key_with(&mut app, Key::Character('c'), none);
    key_with(&mut app, Key::Named(NamedKey::Enter), none);
    assert_eq!(order(&app), sorted, "the rows stay where they are");
    assert_eq!(
        view_of(&app).row.as_deref(),
        Some(pk_of(2).as_str()),
        "Return lands on the row that was below"
    );
    key_with(&mut app, Key::Character('e'), none);
    key_with(&mut app, Key::Named(NamedKey::Enter), none);
    assert_eq!(
        staged_cells(&mut app),
        [
            ((1, "c1".to_owned()), "c".to_owned()),
            ((2, "c1".to_owned()), "e".to_owned())
        ],
        "each value is staged in the cell it was typed into, and `d` is untouched"
    );

    // Tab in an open cell of the sorted column moves across the same row.
    act(&mut app, &cell(4, "c1"), None);
    key_with(&mut app, Key::Character('z'), none);
    key_with(&mut app, Key::Named(NamedKey::Tab), none);
    assert_eq!(order(&app), sorted);
    let view = view_of(&app);
    assert_eq!(
        (view.row.as_deref(), view.column.as_deref()),
        (Some(pk_of(4).as_str()), Some("c2")),
        "Tab lands on the cell beside"
    );
    // Delete in the sorted column: the emptied row stays in view, under the selection.
    act(&mut app, &cell(0, "c1"), None);
    key_with(&mut app, Key::Named(NamedKey::Delete), none);
    assert_eq!(order(&app), sorted);

    // Written and read back, each row stands where its new value puts it.
    app.native_runtime
        .tables_mut()
        .unwrap()
        .show(sheet_with(["", "c", "e", "d", "z"], ColumnKind::Text));
    assert_eq!(
        order(&app),
        [1, 3, 2, 4, 0],
        "c, d, e, z, then the empty cell"
    );
}

/// A cell left open while another Mac writes it is compared, at the Write, against what it
/// held when it was opened: the open field covered the cell, so the person never saw the newer
/// value. It stays, the pane says it was written elsewhere, and the typed one is kept staged.
/// NEGATIVE CONTROL: with `close_edit` passing `stage_over` no `saw` (the cell is then compared
/// against the read it is staged on), the pane says nothing of the other Mac's `rack z`:
/// `assertion `left == right` failed  left: None  right: Some("Written elsewhere since: rack
/// z")`.
#[test]
fn a_cell_changed_elsewhere_while_it_was_open_is_not_written_over() {
    let lab = two_macs();
    let mut app = App::headless_for_test();
    app.open_tables_tab_at(Some(lab.paths[0].clone())).unwrap();
    settle(&mut app);
    let cell = format!("tables/cell/{MC}/location");
    act(&mut app, &cell, None);
    act(&mut app, &cell, None);
    act(&mut app, EDIT, Some(SemanticInput::Text("rack b".into())));

    // The other Mac writes the cell while it is open here, and this page reads that.
    Registry::at(lab.paths[1].clone())
        .write(vec![vec![put("location", json!("rack z"))]])
        .unwrap();
    repair_commons(&lab, 4);
    assert_eq!(location(&lab, 0), Some(json!("rack z")));
    let tables = app.native_runtime.tables_mut().unwrap();
    tables.work.send(Work::Read).unwrap();
    tables.settle();

    key_in(&mut app, TextInputEvent::Submit);
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

/// A range holds the rows that lay between its corners when it was grown. Once the rows shown
/// or their order change (a header's press, a read that moves a row), it is the one selected
/// cell again, so Delete and a copy never reach a row that was not selected; a Delete repeated
/// on a range empties the same cells; a read that moves nothing leaves the range standing.
/// NEGATIVE CONTROL: with `corner` answering the anchor whatever order is in force (no
/// `Corner::order`), the read that moves a row leaves the range standing over other rows:
/// `assertion `left == right` failed: the selected cell's own staged nothing  left:
/// Some("\t\n\t\n\t")  right: Some("")`.
#[test]
fn a_range_is_one_cell_again_once_its_rows_move() {
    use aterm_types::keyboard::{Key, Modifiers, NamedKey};
    let none = Modifiers::empty();
    let cell = |n: usize, column: &str| format!("tables/cell/{}/{column}", pk_of(n));
    let read = sheet_with(["b", "d", "a", "c", "e"], ColumnKind::Text);
    let mut app = open_with(read.clone());
    act(&mut app, &cell(0, "c1"), None);
    key_with(&mut app, Key::Named(NamedKey::ArrowDown), Modifiers::SHIFT);
    assert_eq!(app.native_selection_text(WID).as_deref(), Some("b\nd"));
    // A read that moves no row leaves the range standing.
    app.native_runtime.tables_mut().unwrap().show(read);
    assert_eq!(app.native_selection_text(WID).as_deref(), Some("b\nd"));

    // The header's press puts `c` between `b` and `d`: the range is the selected cell alone.
    act(&mut app, "tables/head/c1", None);
    assert_eq!(order(&app), [2, 0, 3, 1, 4], "a, b, c, d, e");
    assert_eq!(app.native_selection_text(WID).as_deref(), Some("d"));
    let ui = render(&app);
    for n in [0, 3] {
        assert_eq!(
            said(&ui, &cell(n, "c1")),
            Some(format!(
                "C1 of {}: {}",
                pk_of(n),
                if n == 0 { "b" } else { "c" }
            )),
            "no cell is said selected"
        );
    }
    key_with(&mut app, Key::Named(NamedKey::ArrowDown), Modifiers::SHIFT);
    key_with(&mut app, Key::Named(NamedKey::Delete), none);
    assert_eq!(
        staged_cells(&mut app),
        [
            ((1, "c1".to_owned()), String::new()),
            ((4, "c1".to_owned()), String::new())
        ],
        "only the range grown after the sort is emptied"
    );

    // A Delete repeated on a range in the sorted column empties the same cells again.
    let mut app = open_with(sheet_with(["b", "", "c", "", "d"], ColumnKind::Text));
    act(&mut app, "tables/head/c1", None);
    assert_eq!(order(&app), [0, 2, 4, 1, 3]);
    act(&mut app, &cell(0, "c1"), None);
    key_with(&mut app, Key::Named(NamedKey::ArrowRight), Modifiers::SHIFT);
    key_with(&mut app, Key::Named(NamedKey::ArrowDown), Modifiers::SHIFT);
    key_with(&mut app, Key::Named(NamedKey::ArrowDown), Modifiers::SHIFT);
    assert_eq!(
        app.native_selection_text(WID).as_deref(),
        Some("b\tv\nc\tv\nd\tv")
    );
    key_with(&mut app, Key::Named(NamedKey::Delete), none);
    let emptied = staged_cells(&mut app);
    assert_eq!(emptied.len(), 6, "{emptied:?}");
    key_with(&mut app, Key::Named(NamedKey::Delete), none);
    assert_eq!(staged_cells(&mut app), emptied, "the same six cells");

    // A read that moves a row under a standing range: one cell again.
    app.native_runtime
        .tables_mut()
        .unwrap()
        .show(sheet_with(["b", "a", "c", "", "d"], ColumnKind::Text));
    assert_eq!(order(&app), [1, 0, 2, 4, 3]);
    assert_eq!(
        app.native_selection_text(WID).as_deref(),
        Some(""),
        "the selected cell's own staged nothing"
    );
    // Escape on a range leaves the selected cell.
    key_with(&mut app, Key::Named(NamedKey::ArrowUp), Modifiers::SHIFT);
    assert!(view_of(&app).corner().is_some());
    key_with(&mut app, Key::Named(NamedKey::Escape), none);
    assert!(view_of(&app).anchor.is_none());
}

fn paste(app: &mut App, text: &str) {
    assert!(
        app.native_input_event(
            WID,
            &crate::input::InputEvent::Paste(text.to_owned(), crate::input::PasteFraming::AtDrain)
        ),
        "the page takes the paste"
    );
}

/// A paste on a selected cell is a block of cells, not typed text: its lines go down the rows
/// from the selected cell and each line's cells across the columns, each staged in its own
/// cell and none opened; a quoted cell keeps its tab; what a copy took pastes back as it was.
/// One cell pasted on a range fills it. A column the page does not edit and what lies past the
/// sheet's last row are left, and the bar says so. In an open cell a paste is typed text.
/// NEGATIVE CONTROL: with a paste sent as `TextInputEvent::Commit` (typed text), the selected
/// cell opens holding the whole clipboard and nothing is staged: `assertion `left == right`
/// failed: each cell of the block is staged  left: []`.
#[test]
fn a_paste_on_a_cell_stages_a_block_of_cells() {
    use aterm_types::keyboard::{Key, Modifiers, NamedKey};
    let cell = |n: usize, column: &str| format!("tables/cell/{}/{column}", pk_of(n));
    let at = |n: usize, column: &str, text: &str| ((n, column.to_owned()), text.to_owned());
    let mut app = open_with(lettered_sheet());
    act(&mut app, &cell(1, "c1"), None);
    paste(&mut app, "x\ty\nz\t\"q\tr\"\n");
    assert_eq!(
        staged_cells(&mut app),
        [
            at(1, "c1", "x"),
            at(1, "c2", "y"),
            at(2, "c1", "z"),
            at(2, "c2", "q\tr")
        ],
        "each cell of the block is staged"
    );
    assert!(view_of(&app).editing.is_none(), "no cell is opened");
    // The pasted block is the selection, and copies back as it was pasted.
    assert_eq!(
        app.native_selection_text(WID).as_deref(),
        Some("x\ty\nz\t\"q\tr\"")
    );
    assert_eq!(
        selected(&app).3,
        cell(1, "c1"),
        "the keys stay on its first cell"
    );

    // One cell on a range fills the range.
    act(&mut app, &cell(0, "c3"), None);
    key_with(&mut app, Key::Named(NamedKey::ArrowDown), Modifiers::SHIFT);
    paste(&mut app, "k");
    let staged = staged_cells(&mut app);
    assert!(
        staged.contains(&at(0, "c3", "k")) && staged.contains(&at(1, "c3", "k")),
        "{staged:?}"
    );
    assert_eq!(staged.len(), 6);

    // In an open cell a paste is typed text, tabs and all, and stages nothing by itself.
    act(&mut app, &cell(3, "c1"), None);
    act(&mut app, &cell(3, "c1"), None);
    paste(&mut app, "one\ttwo");
    let view = view_of(&app);
    assert_eq!(
        view.editing.as_ref().map(|e| e.input.value()),
        Some("one\ttwo")
    );
    assert_eq!(staged_cells(&mut app).len(), 6);

    // A column the page does not edit and rows past the sheet's last are left, and said.
    let mut fixed = lettered_sheet();
    fixed.sheets[0].columns[2].editable = false;
    let mut app = open_with(fixed);
    act(&mut app, &cell(3, "c2"), None);
    paste(&mut app, "a\tb\nc\td");
    assert_eq!(staged_cells(&mut app), [at(3, "c3", "b")]);
    let note = said(&render(&app), "tables/bar/note").unwrap_or_default();
    assert!(note.starts_with("Pasted 1 of 4 cells"), "{note}");
}

/// Tab-separated text is read as a spreadsheet writes it, and a copy writes it the same way:
/// a cell that begins with a quote or holds a tab or a line end is set between quotes, its own
/// quotes doubled, so it reads back as the one cell it was.
/// NEGATIVE CONTROL: with `tsv_field` leaving a leading quote as it is: `assertion `left ==
/// right` failed  left: "\"spare\" rack"  right: "\"\"\"spare\"\" rack\""`.
#[test]
fn copied_cells_read_back_as_the_cells_they_were() {
    let cells = |rows: &[&[&str]]| -> Vec<Vec<String>> {
        rows.iter()
            .map(|row| row.iter().map(|cell| (*cell).to_owned()).collect())
            .collect()
    };
    assert_eq!(cells_of("a\tb\nc\td"), cells(&[&["a", "b"], &["c", "d"]]));
    assert_eq!(
        cells_of("a\tb\r\nc\td\r\n"),
        cells(&[&["a", "b"], &["c", "d"]]),
        "a line end at the end begins no row"
    );
    assert_eq!(cells_of("a\t\n\tb"), cells(&[&["a", ""], &["", "b"]]));
    assert_eq!(cells_of("a\n\nb"), cells(&[&["a"], &[""], &["b"]]));
    assert_eq!(cells_of(""), Vec::<Vec<String>>::new());
    assert_eq!(
        cells_of("\"two\nlines\"\t\"say \"\"hi\"\"\""),
        cells(&[&["two\nlines", "say \"hi\""]])
    );
    assert_eq!(
        cells_of("\"never closed\tb"),
        cells(&[&["\"never closed", "b"]]),
        "a quote that never closes is the character itself"
    );
    assert_eq!(
        tsv_field("say \"hi\""),
        "say \"hi\"",
        "a quote inside is as it is"
    );
    assert_eq!(tsv_field("\"spare\" rack"), "\"\"\"spare\"\" rack\"");
    for row in [
        ["\"spare\" rack", "next"],
        ["a\tb", "two\nlines"],
        ["", "\""],
        ["plain", "say \"hi\""],
    ] {
        let line = row.map(tsv_field).join("\t");
        assert_eq!(cells_of(&line), cells(&[&row]), "{line:?}");
    }
}

/// A column sized wider than the room beside the key column cannot show whole: the grid stays
/// where it is across as the selection moves down that column.
/// NEGATIVE CONTROL: with `across_for` choosing only between the column's start and its end,
/// the press scrolls to the column's end and each step down flips the grid 180 points:
/// `assertion `left == right` failed: step 1  left: 0.0  right: 180.0`.
#[test]
fn a_column_wider_than_its_room_stays_put_as_the_selection_moves() {
    use aterm_types::keyboard::{Key, Modifiers, NamedKey};
    let mut app = open_with(narrow_sheet(8, 6));
    with_view_mut(&mut app, |state| {
        state
            .widths
            .entry("machine".to_owned())
            .or_default()
            .insert("c1".to_owned(), 900.0);
    });
    render(&app);
    let room = view_of(&app).grid_room.get();
    assert!(room < 900.0, "the column is wider than its room: {room}");
    act(&mut app, &format!("tables/cell/{}/c1", pk_of(0)), None);
    let across = view_of(&app).across;
    for step in 1..=4 {
        render(&app);
        key_with(
            &mut app,
            Key::Named(NamedKey::ArrowDown),
            Modifiers::empty(),
        );
        assert_eq!(view_of(&app).across, across, "step {step}");
    }
    // Where the grid stands: left alone while the wide column fills the room, else at its
    // start; a column that fits is scrolled no further than shows it whole.
    assert_eq!(across_for(100.0, 0.0, 900.0, 720.0), 100.0);
    assert_eq!(across_for(180.0, 0.0, 900.0, 720.0), 180.0);
    assert_eq!(across_for(500.0, 0.0, 900.0, 720.0), 0.0);
    assert_eq!(across_for(0.0, 300.0, 1200.0, 720.0), 300.0);
    assert_eq!(across_for(400.0, 300.0, 500.0, 720.0), 300.0);
    assert_eq!(across_for(0.0, 700.0, 900.0, 720.0), 180.0);
    assert_eq!(across_for(100.0, 200.0, 400.0, 720.0), 100.0);
}

/// After a pointer's press on a header or a drag of a column's edge the keys are still the
/// selected cell's, whose ring is still drawn: a copy takes it and the arrows move it. Pressed
/// from the keyboard, a header keeps the keyboard, so Return presses it again; Left and Right
/// on a column's focused edge size the column, and the edge's name says what its press does.
/// NEGATIVE CONTROL: with `sort_by` always leaving focus on the header, the copy after the
/// pointer's press takes nothing: `assertion `left == right` failed: a copy takes the selected
/// cell  left: None  right: Some("r1c1")`.
#[test]
fn a_pointers_press_on_a_header_leaves_the_keys_on_the_selected_cell() {
    use aterm_types::keyboard::{Key, Modifiers, NamedKey};
    use aterm_window::event::{ElementState, MouseButton};
    let none = Modifiers::empty();
    let cell = |n: usize, column: &str| format!("tables/cell/{}/{column}", pk_of(n));
    let mut app = open_with(lettered_sheet());
    act(&mut app, &cell(1, "c1"), None);
    point_at(&mut app, "tables/head/c2");
    app.on_mouse_input(WID, ElementState::Pressed, MouseButton::Left);
    app.on_mouse_input(WID, ElementState::Released, MouseButton::Left);
    assert_eq!(
        said(&render(&app), "tables/head/c2").as_deref(),
        Some("Sort by C2, sorted ascending"),
        "the press sorted"
    );
    assert_eq!(
        group_style_of(&tree(&app), &format!("{}/focus", cell(1, "c1"))),
        Some(StyleRef::CellSelected),
        "the ring is still on the cell"
    );
    assert_eq!(
        app.native_selection_text(WID).as_deref(),
        Some("r1c1"),
        "a copy takes the selected cell"
    );
    key_with(&mut app, Key::Named(NamedKey::ArrowRight), none);
    assert_eq!(selected(&app), (1, "c2".to_owned(), false, cell(1, "c2")));

    // A drag of a column's edge: at the release the keys are the cell's again.
    let before = place(&presented(&mut app), "tables/head/c3/seat").width;
    point_at(&mut app, "tables/edge/c3");
    let (x, y) = app.windows[&WID].last_cursor_px;
    app.on_mouse_input(WID, ElementState::Pressed, MouseButton::Left);
    app.on_cursor_moved(WID, x + 40.0, y);
    app.on_mouse_input(WID, ElementState::Released, MouseButton::Left);
    assert!(place(&presented(&mut app), "tables/head/c3/seat").width > before);
    assert_eq!(app.native_selection_text(WID).as_deref(), Some("r1c2"));
    key_with(&mut app, Key::Named(NamedKey::Delete), none);
    assert_eq!(
        staged_cells(&mut app),
        [((1, "c2".to_owned()), String::new())]
    );

    // The keyboard: Tab reached the header, so Return presses it and presses it again.
    let (_, view) = app.active_native_view(WID).unwrap();
    let head = "tables/head/c1";
    app.focus_native_control(WID, view, UiKey::new(head.to_owned()), true)
        .unwrap();
    press_key(&mut app, NamedKey::Enter);
    assert_eq!(
        said(&render(&app), head).as_deref(),
        Some("Sort by C1, sorted ascending")
    );
    assert_eq!(selected(&app).3, head, "the keyboard stays on the header");
    press_key(&mut app, NamedKey::Enter);
    assert_eq!(
        said(&render(&app), head).as_deref(),
        Some("Sort by C1, sorted descending")
    );

    // A column's edge from the keyboard: its name says what its press does, and Left and
    // Right size the column.
    let edge = "tables/edge/c1";
    let ui = render(&app);
    assert_eq!(
        said(&ui, edge).as_deref(),
        Some("Fit C1 column to its widest value")
    );
    assert_eq!(
        ui.semantic(&UiKey::new(edge.to_owned()))
            .unwrap()
            .description
            .as_deref(),
        Some("Left and Right make the column narrower and wider")
    );
    let width = |app: &App| place(&render(app), "tables/head/c1/seat").width;
    let began = width(&app);
    app.focus_native_control(WID, view, UiKey::new(edge.to_owned()), true)
        .unwrap();
    key_with(&mut app, Key::Named(NamedKey::ArrowRight), none);
    key_with(&mut app, Key::Named(NamedKey::ArrowRight), none);
    assert_eq!(width(&app), began + 2.0 * WIDTH_STEP * super::scale());
    key_with(&mut app, Key::Named(NamedKey::ArrowLeft), none);
    assert_eq!(width(&app), began + WIDTH_STEP * super::scale());
    assert_eq!(
        selected(&app).1,
        "c2",
        "the selection did not move with the edge's arrows"
    );
}

/// The order of a sheet's rows is worked out once for each read, find and sort, and not again
/// for a redraw, a key or a staged cell: every one of those reads the order already made.
/// NEGATIVE CONTROL: with `put_in_order` keeping nothing (its early return removed), each
/// redraw and each key works the order out again: `assertion `left == right` failed: redraws
/// and keys read the order made  left: 26  right: 2`.
#[test]
fn a_sheets_order_is_worked_out_once_for_each_read_find_and_sort() {
    use aterm_types::keyboard::{Key, Modifiers, NamedKey};
    let none = Modifiers::empty();
    let built = |app: &App| {
        render(app);
        view_of(app).order.borrow().built
    };
    let mut app = open_with(tall_sheet(300, 4));
    act(&mut app, "tables/head/label", None);
    act(&mut app, &format!("tables/cell/{}/c1", pk_of(5)), None);
    let made = built(&app);
    for _ in 0..3 {
        render(&app);
    }
    for key in [NamedKey::ArrowDown, NamedKey::ArrowRight, NamedKey::Tab] {
        key_with(&mut app, Key::Named(key), none);
    }
    key_with(&mut app, Key::Named(NamedKey::ArrowDown), Modifiers::SHIFT);
    assert!(app.native_selection_text(WID).is_some());
    key_with(&mut app, Key::Named(NamedKey::Delete), none);
    key_with(&mut app, Key::Character('q'), none);
    key_with(&mut app, Key::Named(NamedKey::Enter), none);
    assert!(!staged_cells(&mut app).is_empty());
    assert_eq!(built(&app), made, "redraws and keys read the order made");
    // The count in the heading reads the same order.
    assert_eq!(
        said(&render(&app), "tables/count").as_deref(),
        Some("300 rows")
    );

    act(&mut app, "tables/head/label", None);
    assert_eq!(built(&app), made + 1, "a header's press");
    act(&mut app, FIND, Some(SemanticInput::Text("m29".into())));
    assert_eq!(built(&app), made + 2, "the find's words");
    assert!(said(&render(&app), "tables/count").is_some_and(|count| count.ends_with("of 300")));
    app.native_runtime
        .tables_mut()
        .unwrap()
        .show(tall_sheet(300, 4));
    assert_eq!(built(&app), made + 3, "a new read");
}

/// Save's key and File ▸ Save press Write, and the palette lists Write and Discard: the sheet
/// keeps Tab for its cells, so the keyboard reaches Write without leaving them. An open cell's
/// typed words are staged first, as a press on Write stages them.
/// NEGATIVE CONTROL: with the Tables arm of the key path removed, the key writes nothing: `the
/// key pressed Write`.
#[test]
fn saves_key_writes_the_staged_cells() {
    use aterm_types::keyboard::{Key, Modifiers};
    let lab = two_macs();
    let mut app = App::headless_for_test();
    app.open_tables_tab_at(Some(lab.paths[0].clone())).unwrap();
    settle(&mut app);
    let commands = |app: &App| {
        let (instance, view) = app.active_native_view(WID).unwrap();
        app.native_runtime.commands(instance, view).unwrap()
    };
    assert!(!app.can_save_document(WID), "nothing to write yet");
    let listed = commands(&app);
    let write = listed
        .iter()
        .find(|command| command.id.as_str() == WRITE)
        .expect("Write is a command");
    assert_eq!(write.title, "Tables: Write Changes");
    assert!(!write.enabled, "nothing to write yet");
    assert!(
        write
            .shortcut
            .as_deref()
            .is_some_and(|key| key.ends_with("-S"))
    );
    assert!(listed.iter().any(|command| command.id.as_str() == DISCARD));

    // An open cell with new words in it is something to save: the key stages and writes it.
    let cell = format!("tables/cell/{MC}/location");
    act(&mut app, &cell, None);
    act(&mut app, &cell, None);
    act(&mut app, EDIT, Some(SemanticInput::Text("rack b".into())));
    assert!(app.can_save_document(WID));
    key_with(&mut app, Key::Character('s'), Modifiers::SUPER);
    assert!(
        app.native_runtime.tables_mut().unwrap().writing,
        "the key pressed Write"
    );
    settle_write(&mut app);
    assert_eq!(location(&lab, 0), Some(json!("rack b")), "the key wrote it");
    assert!(!app.can_save_document(WID), "written");

    // File ▸ Save, with a cell staged.
    act(&mut app, &cell, None);
    act(&mut app, EDIT, Some(SemanticInput::Text("rack c".into())));
    key_in(&mut app, TextInputEvent::Submit);
    assert!(commands(&app).iter().all(|command| command.enabled));
    app.save_front_document().unwrap();
    assert!(app.native_runtime.tables_mut().unwrap().writing);
    settle_write(&mut app);
    assert_eq!(location(&lab, 0), Some(json!("rack c")));
}

/// Every theme aterm ships, as the native pages read it.
fn shipped_themes() -> Vec<(String, aterm_render::Theme)> {
    let builtins = aterm_types::scheme::builtin_names()
        .into_iter()
        .filter_map(|name| Some((name.to_owned(), aterm_types::scheme::builtin(name)?)));
    let packs = aterm_theme_pack::themes().iter().map(|theme| {
        let scheme = aterm_types::scheme::parse_scheme_str(theme.text).expect("a pack theme");
        (theme.id.to_owned(), scheme)
    });
    builtins
        .chain(packs)
        .map(|(name, scheme)| (name, aterm_render::Theme::from(&scheme)))
        .collect()
}

/// A sheet's words read on the grounds the sheet mixes for itself
/// ([`crate::native_appearance::sheet_ground`]): on every shipped theme a row's number and a
/// header's name hold [`crate::native_appearance::SHEET_WORD_FLOOR`] on the header ground, a
/// cell's words hold it on a range's wash, the sort mark and the bars across hold a mark's 3.0,
/// and both grounds stand off the page the cells lie on.
/// NEGATIVE CONTROL: with a row's number set in `StyleRef::Faint` (the tertiary ink), 52
/// themes fail (`Default: a row's number on the header ground reads 2.6`); with `sheet_ground`
/// mixing its most whatever the inks, 25 themes fail (`Nord: a cell's words on a range's wash
/// reads 4.0`, `Solarized Dark: a header's name on the header ground reads 4.1`).
#[test]
fn a_sheets_words_read_on_its_header_ground_and_its_range_wash() {
    use crate::widget::DrawPrim;
    use aterm_types::keyboard::{Key, Modifiers, NamedKey};
    let reads = crate::native_appearance::contrast_ratio;
    let cell = |n: usize, column: &str| format!("tables/cell/{}/{column}", pk_of(n));
    let mut app = open_with(narrow_sheet(4, 24));
    act(&mut app, "tables/head/c4", None);
    act(&mut app, &cell(0, "c4"), None);
    key_with(&mut app, Key::Named(NamedKey::ArrowDown), Modifiers::SHIFT);
    key_with(&mut app, Key::Named(NamedKey::ArrowDown), Modifiers::SHIFT);
    // Laid out, the grid knows how far it scrolls across; scrolled, columns lie under the
    // key column and its edge wears the bar.
    render(&app);
    app.dispatch_native_event(WID, AppEvent::ScrollAcross(1))
        .unwrap();
    assert!(view_of(&app).across > 0.0);
    let ui = render(&app);
    let themes = shipped_themes();
    assert!(themes.len() > 30, "{} themes swept", themes.len());
    let mut under = Vec::new();
    for (name, theme) in themes {
        let roles = crate::settings::Roles::from_theme(theme);
        let painted = ui.painted(roles);
        let prims = |key: &str| {
            painted
                .iter()
                .find(|(painted, _)| painted.as_str() == key)
                .map(|(_, prims)| prims.clone())
                .unwrap_or_else(|| panic!("{key} is painted"))
        };
        let ground = |key: &str| {
            prims(key)
                .iter()
                .find_map(|prim| match prim {
                    DrawPrim::Panel { fill, .. } => Some([fill[0], fill[1], fill[2]]),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("{key} fills its ground"))
        };
        let ink = |key: &str| {
            prims(key)
                .iter()
                .find_map(|prim| match prim {
                    DrawPrim::Text { color, .. } => Some([color[0], color[1], color[2]]),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("{key} draws words"))
        };
        let head = ground(&format!("tables/num/{}", pk_of(1)));
        let wash = ground(&format!("{}/range", cell(1, "c4")));
        let words = crate::native_appearance::SHEET_WORD_FLOOR;
        for (what, ink, ground, floor) in [
            (
                "a row's number on the header ground",
                ink(&format!("tables/num/{}/n", pk_of(1))),
                head,
                words,
            ),
            (
                "the selected row's number on the header ground",
                ink(&format!("tables/num/{}/n", pk_of(2))),
                head,
                words,
            ),
            (
                "a header's name on the header ground",
                ink("tables/head/c5/word"),
                head,
                words,
            ),
            (
                "a group's word on the header ground",
                ink("tables/groups/0/word"),
                head,
                words,
            ),
            (
                "a cell's words on a range's wash",
                ink(&format!("{}/text", cell(1, "c4"))),
                wash,
                words,
            ),
            (
                "the sort mark on the header ground",
                ground("tables/head/c4/mark"),
                head,
                3.0,
            ),
            (
                "the frozen edge's bar on the header ground",
                ground("tables/grid/across/edge"),
                head,
                3.0,
            ),
            (
                "the thumb across on the page's cells",
                ground("tables/grid/across/thumb"),
                roles.surface,
                3.0,
            ),
            (
                "the thumb across on a range's wash",
                ground("tables/grid/across/thumb"),
                wash,
                3.0,
            ),
        ] {
            let ratio = reads(ink, ground);
            if ratio < floor {
                under.push(format!("{name}: {what} reads {ratio:.1}"));
            }
        }
        for (what, ground) in [("the header ground", head), ("a range's wash", wash)] {
            let ratio = reads(ground, roles.surface);
            if ratio < crate::native_appearance::CARD_CONTRAST_MIN {
                under.push(format!("{name}: {what} stands {ratio:.2} off the page"));
            }
        }
    }
    assert!(under.is_empty(), "{under:#?}");
}

/// Where a sheet has columns off its side, a thumb along its foot says how many and where they
/// stand, and a bar marks the key column's edge while columns lie under it; neither takes a
/// press from the cells under it. A sheet whose columns all show has neither.
/// NEGATIVE CONTROL: with `grid` laying no `tables/grid/across` over the sheet, nothing says
/// there are more columns: `tables/grid/across/thumb is listed` panics.
#[test]
fn a_thumb_says_where_the_columns_stand_across() {
    use aterm_window::event::{ElementState, MouseButton};
    let mut app = open_with(narrow_sheet(4, 24));
    let thumb = |app: &App| place(&render(app), "tables/grid/across/thumb");
    let edge = |app: &App| group_style_of(&tree(app), "tables/grid/across/edge");
    let ui = render(&app);
    let label = place(&ui, &format!("tables/cell/{}/label/seat", pk_of(0)));
    let grid = place(&ui, "tables/grid");
    let began = thumb(&app);
    let track = grid.x + grid.width - 1.0 - (label.x + label.width);
    assert_eq!(began.x, label.x + label.width, "from the key column's edge");
    assert!(
        began.width < track && began.width >= 24.0,
        "a share of the room across: {began:?} of {track}"
    );
    assert_eq!(
        began.y + began.height,
        grid.y + grid.height - 1.0,
        "at the foot"
    );
    assert_eq!(
        edge(&app),
        Some(StyleRef::Plain),
        "nothing lies under the key column"
    );

    app.dispatch_native_event(WID, AppEvent::ScrollAcross(2))
        .unwrap();
    assert!(thumb(&app).x > began.x, "the thumb follows the columns");
    assert_eq!(edge(&app), Some(StyleRef::SheetBar));
    app.dispatch_native_event(WID, AppEvent::ScrollAcross(10_000))
        .unwrap();
    let end = thumb(&app);
    assert!(
        (end.x + end.width - (grid.x + grid.width - 1.0)).abs() <= 0.5,
        "at the far end the thumb ends at the grid's edge: {end:?} in {grid:?}"
    );
    app.dispatch_native_event(WID, AppEvent::ScrollAcross(-10_000))
        .unwrap();

    // The bars are paint alone: a press over the sheet still reaches its cell. The lowest row
    // the window's grid shows whole is the one nearest the thumb (the cell bar stands on the
    // grid and the type line under the header, so a short window's grid shows fewer rows).
    let shown = presented(&mut app);
    let sheet = rect(&shown, "tables/grid");
    let lowest = (0..4)
        .rev()
        .find(|n| {
            let at = place(&shown, &format!("tables/cell/{}/c2/seat", pk_of(*n)));
            at.y >= sheet.y && at.bottom() <= sheet.bottom()
        })
        .expect("the grid shows a row whole");
    let key = format!("tables/cell/{}/c2", pk_of(lowest));
    point_at(&mut app, &key);
    app.on_mouse_input(WID, ElementState::Pressed, MouseButton::Left);
    app.on_mouse_input(WID, ElementState::Released, MouseButton::Left);
    assert_eq!(view_of(&app).column.as_deref(), Some("c2"));
    assert_eq!(view_of(&app).row.as_deref(), Some(pk_of(lowest).as_str()));

    let app = open_with(narrow_sheet(4, 3));
    assert_eq!(
        group_style_of(&tree(&app), "tables/grid/across/thumb"),
        None,
        "every column shows: no thumb"
    );
}

/// The rail heading a listed table sits under: the last heading above its line.
fn rail_heading(ui: &CompiledUi, name: &str) -> &'static str {
    let at = rect(ui, &format!("tables/sheet/{name}")).y;
    ["STORED", "SET BY ATERM", "REFERENCE"]
        .into_iter()
        .filter_map(|word| {
            ui.semantic(&UiKey::new(format!("tables/rail/group/{word}")))
                .map(|node| (word, node.rect.y))
        })
        .filter(|(_, y)| *y < at)
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map_or("", |(word, _)| word)
}

/// A cells collection a table compiles with no row of it written anywhere: reference rows, which
/// a cells collection is not by its kind.
const WIDGET: Collection = Collection {
    name: "widget",
    version: 1,
    kind: aterm_db::typed::Kind::Cells,
    writers: aterm_db::typed::Writers::Owner,
    prefix: "",
    derive: aterm_db::typed::Derive::Word,
    columns: &[
        aterm_db::typed::Column::new("name", aterm_db::typed::Ty::Text(32)),
        aterm_db::typed::Column::new("row", aterm_db::typed::Ty::Enum(&["live"])),
    ],
    unique: &[],
    max_row: 0,
};

/// LT-23. The rail lists every collection of the registry generation in use, read from the
/// handle's compiled table, never from a list the page keeps: a table compiled with one more
/// collection lists it under its title in the group its face names, with no change to the page;
/// and a registry rolled to its second generation lists all of that generation's collections
/// but `archived` (read as the owned rows it carries), `roll` under SET BY ATERM, each with its
/// sheet. An owned sheet (a Mac's facts) is read from the view's owned rows: it sorts and copies,
/// and no cell of it opens, empties or stages.
/// NEGATIVE CONTROLS: with `tables_store::listed` listing only the five collections the page
/// kept before (`machine`, `account`, `address`, `mailbox`, `phone`), `widget is listed: ["machine",
/// "account", "address", "mailbox", "phone"]`; with it facing a collection by its kind alone (the
/// table's faces unread), `widget sits under the group its face names` (`left: "STORED"`); with
/// `TablesApp::takes` not refusing an owned sheet and the facts sheet's columns editable, its
/// cell opens for an edit: `an owned cell never opens`;
/// with the first column of a Mac's own rows headed `MAC`, as the column of the Mac it binds is,
/// `two columns under one heading`; with `Names::words` naming nothing, the facts
/// row's `mac` reads as its key: `a machine reference reads as its label`.
#[test]
fn the_tables_page_lists_every_registry_collection() {
    use aterm_db::alab::registry::{self as registry, FACES};
    use aterm_db::typed::{Face, Faced, Table};
    // A table compiled with one more collection: the page lists it as the table names it.
    let collections: &'static [&'static Collection] = Box::leak(
        registry::COLLECTIONS_2
            .iter()
            .copied()
            .chain([&WIDGET])
            .collect::<Vec<_>>()
            .into_boxed_slice(),
    );
    let faces: &'static [Faced] = Box::leak(
        FACES
            .iter()
            .copied()
            .chain([Faced {
                collection: "widget",
                title: "Widgets",
                singular: "Widget",
                face: Face::Seeded,
                order: &[],
            }])
            .collect::<Vec<_>>()
            .into_boxed_slice(),
    );
    let table = Table {
        collections,
        faces,
        ..registry::REGISTRY_2
    };
    let listed = crate::tables_store::listed(&table);
    let names: Vec<&str> = listed.iter().map(|l| l.name).collect();
    assert!(names.contains(&"widget"), "widget is listed: {names:?}");
    assert!(!names.contains(&"archived"), "archived is read as its rows");
    assert!(
        !names.contains(&"history"),
        "history is read as its entries"
    );
    assert_eq!(names.len(), collections.len() - 2, "{names:?}");
    let mut snapshot = one_sheet(Shown::default());
    snapshot.listed = listed;
    let app = open_with(snapshot);
    let ui = render(&app);
    let widget = said(&ui, "tables/sheet/widget").expect("widget is on the rail");
    assert!(widget.starts_with("Widgets:"), "under its title: {widget}");
    assert_eq!(
        rail_heading(&ui, "widget"),
        "REFERENCE",
        "widget sits under the group its face names"
    );
    for (name, heading) in [
        ("machine", "STORED"),
        ("preference", "STORED"),
        ("machine-facts", "SET BY ATERM"),
        ("roll", "SET BY ATERM"),
        ("source", "SET BY ATERM"),
        ("provider", "REFERENCE"),
        ("model", "REFERENCE"),
    ] {
        assert_eq!(rail_heading(&ui, name), heading, "{name}");
    }

    // A registry rolled to its second generation, one Mac's facts row carried into it.
    let lab = Lab::new(&[Role::Writer]);
    let here = Registry::at(lab.paths[0].clone());
    here.open().unwrap();
    here.write(vec![vec![
        put("label", json!("m9")),
        put("status", json!("active")),
        put("row", json!("live")),
    ]])
    .unwrap();
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
    let rolled = here.roll(true, aterm_time::unix_millis().unwrap()).unwrap();
    assert!(rolled.sealed, "{rolled:?}");
    let handle = Handle::at(lab.paths[0].clone(), &REGISTRY);
    assert_eq!(handle.compiled().table.role, "registry.2");

    let mut app = App::headless_for_test();
    app.open_tables_tab_at(Some(lab.paths[0].clone())).unwrap();
    settle(&mut app);
    let ui = render(&app);
    let listed = crate::tables_store::listed(handle.compiled().table);
    assert_eq!(
        listed.len(),
        17,
        "eighteen collections, archived not listed"
    );
    let read: Vec<&str> = app
        .native_runtime
        .tables_mut()
        .unwrap()
        .snapshot
        .as_ref()
        .unwrap()
        .sheets
        .iter()
        .map(|sheet| sheet.name)
        .collect();
    for shown in &listed {
        let rail = said(&ui, &format!("tables/sheet/{}", shown.name)).expect("listed");
        assert!(rail.starts_with(&format!("{}: ", shown.title)), "{rail}");
        assert!(read.contains(&shown.name), "{} has its sheet", shown.name);
    }
    assert_eq!(rail_heading(&ui, "roll"), "SET BY ATERM");
    assert!(said(&ui, "tables/sheet/archived").is_none());
    assert_eq!(
        said(&ui, "tables/sheet/machine-facts").as_deref(),
        Some("Mac facts: 1 row"),
        "the carried facts row, read as its owned row"
    );

    // The facts sheet: its Mac first, read only.
    act(&mut app, "tables/sheet/machine-facts", None);
    let facts_key = aterm_db::typed::keys::uuid32(&lab.machine(0));
    let cell = format!("tables/cell/{facts_key}/hosts");
    act(&mut app, &cell, None);
    act(&mut app, &cell, None);
    assert!(
        said(&render(&app), EDIT).is_none(),
        "an owned cell never opens"
    );
    key_in(&mut app, TextInputEvent::Delete);
    paste(&mut app, "elsewhere");
    let ui = render(&app);
    assert!(
        said(&ui, "tables/bar/count").is_none(),
        "an owned cell never stages"
    );
    assert_eq!(cell_says(&ui, &facts_key, "hosts"), "mac-one");
    // A machine it names reads as that machine's label, not its key.
    let label = cell_says(&ui, &facts_key, "label");
    assert!(!label.is_empty(), "the Mac states its label");
    assert_eq!(
        cell_says(&ui, &facts_key, "mac"),
        label,
        "a machine reference reads as its label"
    );
    // The column naming the Mac that states the row reads apart from the Mac it binds.
    let stated_by = said(&ui, "tables/head/~mac").expect("the first column is headed");
    assert_ne!(
        Some(stated_by.as_str()),
        said(&ui, "tables/head/mac").as_deref(),
        "two columns under one heading"
    );
    let words = said(&ui, "tables/said/words").expect("the cell says where it came from");
    assert!(words.starts_with("Hosts: seen "), "{words}");
    // It sorts and copies like any sheet.
    act(&mut app, "tables/head/hosts", None);
    assert_eq!(mark(&app, "hosts"), Some(StyleRef::SortUp));
    assert_eq!(app.native_selection_text(WID).as_deref(), Some("mac-one"));
}

/// One machine row whose nickname a person set and whose chip a person stated and its own Mac
/// sees otherwise, and the facts sheet holding that Mac's own row, read `ago` ms before now.
fn sheets_with_provenance(nickname_by: &str, ago: u64) -> Snapshot {
    let now = aterm_time::unix_millis().unwrap();
    let mut snapshot = one_sheet(Shown {
        text: "Spare".into(),
        by: Some(author(nickname_by, now - 3 * 86_400_000)),
        ..Shown::default()
    });
    let mac = Author {
        kind: AuthorKind::Aterm,
        ..author("m22", now - ago)
    };
    let machine = &mut snapshot.sheets[0];
    let mut chip = machine.columns[0].clone();
    chip.name = "chip".into();
    chip.label = "CHIP".into();
    machine.columns.push(chip);
    machine.rows[0].cells.insert(
        "chip".into(),
        Shown {
            text: "M2".into(),
            by: Some(author("m55", now - 3 * 86_400_000)),
            ..Shown::default()
        },
    );
    machine.observed.insert(
        (MC.into(), "chip".into()),
        Observed {
            text: "M4 Pro".into(),
            by: mac.clone(),
            at_ms: now - ago,
            from: "Mac facts",
        },
    );
    let mut facts = machine.clone();
    facts.name = "machine-facts";
    facts.title = "Mac facts";
    facts.owned = true;
    facts.observed.clear();
    facts.columns.truncate(1);
    facts.columns[0].editable = false;
    facts.columns[0].group = Group::SetByAterm;
    facts.rows[0].cells = [(
        "nickname".to_owned(),
        Shown {
            text: "m22".into(),
            by: Some(Author {
                at_ms: now - 2 * 86_400_000,
                ..mac
            }),
            seen_ms: Some(now - ago),
            ..Shown::default()
        },
    )]
    .into_iter()
    .collect();
    snapshot.listed.push(crate::tables_store::Listed {
        name: "machine-facts",
        title: "Mac facts",
        singular: "Mac's facts",
        face: aterm_db::typed::Face::Mac,
        owned: true,
    });
    snapshot.sheets.push(facts);
    snapshot
}

/// The line under the grid says where the selected cell's value came from, in plain words read
/// from what the page's last read holds: a cell a person set says at which Mac and when, set in
/// the plain ink under SET; what a Mac states says when it saw it and which Mac, in the note's
/// ink under SEEN, its time the Mac's own `at`; where a held value and the newest observation
/// disagree it says both values, under DIFFERS in the primary ink, and the grid marks the cell
/// `differs` without a press on it; and a new read changes it, with nothing polled.
/// NEGATIVE CONTROLS: with `provenance` saying only the held words where an observation
/// disagrees (its last `Differs` arm answering `(said, how)`), `both are said` fails; with
/// `Said::style` setting SET in the note's ink, `a person's value reads apart from an
/// observation` fails (`left: Note right: Plain`); with the owned arm reading the writer's claim
/// in place of `seen_ms`, `seen 4 min ago` fails (`seen on …`); with `Said::Differs` set in the
/// note's ink (`style` and `tag_style`), `a disagreement stands out` fails (`left: Note right:
/// Primary`); with the grid's `reported` never found (`.filter(|_| false)`), `marked in the grid`
/// fails; with `content_width` leaving no room for the `differs` mark, `the column is sized for
/// the value and its mark` fails.
#[test]
fn the_line_under_the_grid_says_where_a_value_came_from() {
    const AGO: u64 = 270_000;
    let mut app = open_with(sheets_with_provenance("m55", AGO));
    let ui = render(&app);
    assert_eq!(
        said(&ui, "tables/said/words").as_deref(),
        Some("Select a cell to see where its value came from")
    );
    act(&mut app, &format!("tables/cell/{MC}/nickname"), None);
    let ui = render(&app);
    assert_eq!(said(&ui, "tables/said/tag").as_deref(), Some("SET"));
    let set = said(&ui, "tables/said/words").unwrap();
    assert!(set.starts_with("Nickname: set at m55 on "), "{set}");
    assert_eq!(
        style_of(&tree(&app), "tables/said/words"),
        Some(StyleRef::Plain),
        "a person's value reads apart from an observation"
    );

    // The grid marks the cell whose Mac reports otherwise, before any press on it.
    let ui = render(&app);
    assert_eq!(
        style_of(&tree(&app), &format!("tables/cell/{MC}/chip/tag")),
        Some(StyleRef::Chip),
        "marked in the grid"
    );
    assert_eq!(
        cell_says(&ui, MC, "chip"),
        "M2, m22 reports M4 Pro",
        "the stated value stands, and the Mac's is said beside it"
    );
    let cell = rect(&ui, &format!("tables/cell/{MC}/chip"));
    let mark = text_width("differs", SemanticRole::Text, StyleRef::Chip) + 6.0;
    let room = cell.width - 24.0 - mark;
    assert!(
        room >= text_width("M2", SemanticRole::Text, StyleRef::Primary),
        "the column is sized for the value and its mark: {room} of {}",
        cell.width
    );
    assert_eq!(
        style_of(&tree(&app), &format!("tables/cell/{MC}/nickname/tag")),
        None
    );
    act(&mut app, &format!("tables/cell/{MC}/chip"), None);
    let ui = render(&app);
    assert_eq!(said(&ui, "tables/said/tag").as_deref(), Some("DIFFERS"));
    let both = said(&ui, "tables/said/words").unwrap();
    assert!(
        both.starts_with("Chip: M2 set at m55 on ")
            && both.ends_with(" \u{b7} m22 reports M4 Pro (Mac facts), 4 min ago"),
        "both are said: {both}"
    );
    let tree_now = tree(&app);
    for key in ["tables/said/tag", "tables/said/words"] {
        assert_eq!(
            style_of(&tree_now, key),
            Some(StyleRef::Primary),
            "a disagreement stands out: {key}"
        );
    }

    act(&mut app, "tables/sheet/machine-facts", None);
    act(&mut app, &format!("tables/cell/{MC}/nickname"), None);
    let ui = render(&app);
    assert_eq!(said(&ui, "tables/said/tag").as_deref(), Some("SEEN"));
    assert_eq!(
        said(&ui, "tables/said/words").as_deref(),
        Some("Nickname: seen 4 min ago by m22")
    );
    assert_eq!(
        style_of(&tree(&app), "tables/said/words"),
        Some(StyleRef::Note)
    );

    // A new read names another writer: the line says so at once, with no read of its own.
    act(&mut app, "tables/sheet/machine", None);
    act(&mut app, &format!("tables/cell/{MC}/nickname"), None);
    app.native_runtime
        .tables_mut()
        .unwrap()
        .show(sheets_with_provenance("m73", AGO));
    let words = said(&render(&app), "tables/said/words").unwrap();
    assert!(words.starts_with("Nickname: set at m73 on "), "{words}");
}

/// The provenance line of the machine sheet's `column` once `set` has changed the hand-made
/// read [`sheets_with_provenance`] and the cell is selected: its tag and its words.
fn line_of(column: &str, set: impl FnOnce(&mut Snapshot)) -> (String, String) {
    let mut snapshot = sheets_with_provenance("m55", 270_000);
    set(&mut snapshot);
    let mut app = open_with(snapshot);
    act(&mut app, &format!("tables/cell/{MC}/{column}"), None);
    let ui = render(&app);
    (
        said(&ui, "tables/said/tag").unwrap_or_default(),
        said(&ui, "tables/said/words").unwrap_or_default(),
    )
}

/// The machine sheet's `column` cell of `snapshot`.
fn cell_of<'a>(snapshot: &'a mut Snapshot, column: &str) -> &'a mut Shown {
    snapshot.sheets[0].rows[0]
        .cells
        .entry(column.to_owned())
        .or_default()
}

/// A setting and an observation are told apart in every case the line meets: what aterm wrote
/// for the owner's table is AUTOMATIC (`set by aterm at m55`), never SEEN; a Mac that reports the
/// held value says so; a person's clear that a Mac contradicts keeps who cleared it and is
/// DIFFERS; a cell nothing ever wrote says only what the Mac reports; a conflicted and a staged
/// cell still say what the Mac reports, a conflict DIFFERS where the Mac's value is none of its
/// values; a writer whose clock runs ahead reads at its date, never `just now`; and in the grid a
/// person's value reads in the primary ink, aterm's in the plain one, and only a held value the
/// Mac contradicts wears `differs`.
/// NEGATIVE CONTROLS: with the cells-sheet arm reading an aterm write as SEEN again (`if
/// (sheet.owned || by.kind == AuthorKind::Aterm)`), `aterm's write is a setting` fails (`left:
/// "SEEN"`); with the agreeing branch answering DIFFERS, `the Mac agrees` fails; with the
/// no-writer branch dropped (`let Some(by) = by else` answering `{field}: empty`), `nothing was
/// written` fails; with a clear read as `not set here` (an early return before `set_words`),
/// the clear's tag fails (`left: "SEEN" right: "DIFFERS"`); with `provenance` returning before the observation for a
/// conflict or a staged cell (no `words.push_str`), the conflict's tag (`left: "VALUES"`) and `a
/// staged cell still says` fail; with `on_words` lacking its guard, `ahead of this Mac's clock` fails
/// (`set at m55 just now`); with the grid's `style` always `Plain`, `a person's value` fails.
#[test]
fn the_line_tells_a_setting_from_an_observation_in_every_case() {
    let now = aterm_time::unix_millis().unwrap();
    let aterm = |label: &str| Author {
        kind: AuthorKind::Aterm,
        ..author(label, now - 3 * 86_400_000)
    };

    // What aterm wrote for the owner's table: set, by aterm, at a Mac.
    let (tag, words) = line_of("nickname", |s| {
        cell_of(s, "nickname").by = Some(aterm("m55"));
    });
    assert_eq!(tag, "AUTOMATIC", "aterm's write is a setting: {words}");
    assert!(
        words.starts_with("Nickname: set by aterm at m55 on "),
        "{words}"
    );

    // The Mac reports what the cell holds.
    let (tag, words) = line_of("chip", |s| cell_of(s, "chip").text = "M4 Pro".into());
    assert_eq!(tag, "SET", "the Mac agrees: {words}");
    assert!(
        words.starts_with("Chip: set at m55 on ")
            && words.ends_with(" \u{b7} m22 reports the same (Mac facts), 4 min ago"),
        "{words}"
    );

    // A person cleared the cell; the Mac reports a value.
    let (tag, words) = line_of("chip", |s| cell_of(s, "chip").text.clear());
    assert_eq!(tag, "DIFFERS", "{words}");
    assert!(
        words.starts_with("Chip: cleared at m55 on ")
            && words.ends_with(" \u{b7} m22 reports M4 Pro (Mac facts), 4 min ago"),
        "who cleared it stays: {words}"
    );

    // Nothing was ever written to the cell: the Mac's is the only word.
    let (tag, words) = line_of("chip", |s| {
        s.sheets[0].rows[0].cells.remove("chip");
    });
    assert_eq!(
        (tag.as_str(), words.as_str()),
        (
            "SEEN",
            "Chip: not set here; m22 reports M4 Pro (Mac facts), 4 min ago"
        ),
        "nothing was written"
    );

    // Two values, none chosen, and the Mac reports a third.
    let conflicted = |s: &mut Snapshot, second: &str| {
        let chip = cell_of(s, "chip");
        chip.values = vec![
            ("M2".into(), Some(author("m55", now - 3 * 86_400_000))),
            (second.into(), Some(author("m73", now - 3 * 86_400_000))),
        ];
        chip.text = format!("M2 / {second}");
    };
    let (tag, words) = line_of("chip", |s| conflicted(s, "M3"));
    assert_eq!(tag, "DIFFERS", "{words}");
    assert!(
        words.starts_with("Chip: 2 values, none chosen: M2 set at m55 on ")
            && words.contains("; M3 set at m73 on ")
            && words.ends_with(" \u{b7} m22 reports M4 Pro (Mac facts), 4 min ago"),
        "a conflict still says what the Mac reports: {words}"
    );
    let (tag, words) = line_of("chip", |s| conflicted(s, "M4 Pro"));
    assert_eq!(tag, "VALUES", "one of the values is the Mac's: {words}");
    assert!(words.ends_with("m22 reports the same (Mac facts), 4 min ago"));

    // A staged cell.
    let mut app = open_with(sheets_with_provenance("m55", 270_000));
    app.native_runtime
        .tables_mut()
        .unwrap()
        .stage("machine", MC, "chip", "M5".into());
    act(&mut app, &format!("tables/cell/{MC}/chip"), None);
    let ui = render(&app);
    assert_eq!(said(&ui, "tables/said/tag").as_deref(), Some("NOT WRITTEN"));
    assert_eq!(
        said(&ui, "tables/said/words").as_deref(),
        Some(
            "Chip: M5 set here, not written; the registry holds M2 \u{b7} m22 reports M4 Pro \
             (Mac facts), 4 min ago"
        ),
        "a staged cell still says what the Mac reports"
    );

    // A writer whose clock runs three hours ahead of this Mac's.
    let (_, words) = line_of("nickname", |s| {
        cell_of(s, "nickname").by = Some(author("m55", now + 3 * 3_600_000));
    });
    assert!(
        words.starts_with("Nickname: set at m55 on "),
        "ahead of this Mac's clock: {words}"
    );

    // The grid: a person's value in the primary ink, aterm's in the plain one; an empty cell a
    // Mac reports is no disagreement.
    let mut snapshot = sheets_with_provenance("m55", 270_000);
    cell_of(&mut snapshot, "chip").by = Some(aterm("m22"));
    let app = open_with(snapshot);
    let drawn = tree(&app);
    assert_eq!(
        style_of(&drawn, &format!("tables/cell/{MC}/nickname/text")),
        Some(StyleRef::Primary),
        "a person's value"
    );
    assert_eq!(
        style_of(&drawn, &format!("tables/cell/{MC}/chip/text")),
        Some(StyleRef::Plain),
        "aterm's value"
    );
    assert_eq!(
        cell_says(&render(&app), MC, "chip"),
        "M2, m22 reports M4 Pro, set by aterm"
    );
    let mut snapshot = sheets_with_provenance("m55", 270_000);
    snapshot.sheets[0].rows[0].cells.remove("chip");
    let app = open_with(snapshot);
    assert_eq!(
        style_of(&tree(&app), &format!("tables/cell/{MC}/chip/tag")),
        None
    );
}

/// Where several Macs' rows state one cell, the statement later in the causal order stands, not
/// the one whose own clock claims the later time.
/// NEGATIVE CONTROL: with `Observed::supersedes` comparing the claimed times first (`(self.at_ms,
/// self.by.hlc) > (held.at_ms, held.by.hlc)`), `the causal order decides` fails.
#[test]
fn the_newest_observation_is_the_latest_in_causal_order() {
    let seen = |label: &str, hlc: u64, at_ms: u64| Observed {
        text: label.into(),
        by: Author {
            hlc: (hlc, 0),
            ..author(label, at_ms)
        },
        at_ms,
        from: "Mac facts",
    };
    // m1's clock runs a day ahead; m2 stated after it, having seen its statement.
    let ahead = seen("m1", 1_000, 1_000 + 86_400_000);
    let after = seen("m2", 2_000, 2_000);
    assert!(after.supersedes(&ahead), "the causal order decides");
    assert!(!ahead.supersedes(&after));
    // Two statements no order tells apart: the later claim.
    let tied = seen("m3", 2_000, 3_000);
    assert!(tied.supersedes(&after));
    assert!(
        !after.supersedes(&after),
        "a statement does not stand over itself"
    );
}

/// A staged cell whose row a new read no longer holds is dropped, and the page says so: kept,
/// it would never be written and would hold the page at this point.
/// NEGATIVE CONTROL: with `take` not calling `drop_stranded`, `nothing stays staged` fails
/// (`left: 1`).
#[test]
fn a_staged_cell_whose_row_is_gone_is_dropped_and_said() {
    let mut app = open_with(sheets_with_provenance("m55", 270_000));
    app.native_runtime
        .tables_mut()
        .unwrap()
        .stage("machine", MC, "nickname", "Spare 2".into());
    assert_eq!(app.native_runtime.tables_mut().unwrap().staged.len(), 1);
    let mut gone = sheets_with_provenance("m55", 270_000);
    gone.sheets[0].rows.clear();
    app.native_runtime.tables_mut().unwrap().show(gone);
    let tables = app.native_runtime.tables_mut().unwrap();
    assert_eq!(tables.staged.len(), 0, "nothing stays staged");
    assert!(tables.movable(), "the page may move to another point");
    assert_eq!(
        said(&render(&app), "tables/note").as_deref(),
        Some("1 change not written was dropped: its cell is no longer in the table")
    );
    // A read that still holds the row keeps the cell staged.
    let mut app = open_with(sheets_with_provenance("m55", 270_000));
    app.native_runtime
        .tables_mut()
        .unwrap()
        .stage("machine", MC, "nickname", "Spare 2".into());
    app.native_runtime
        .tables_mut()
        .unwrap()
        .show(sheets_with_provenance("m73", 270_000));
    assert_eq!(app.native_runtime.tables_mut().unwrap().staged.len(), 1);
}

/// What `app` draws in a window `width` by `height`.
pub(crate) fn render_in(app: &App, width: f32, height: f32) -> CompiledUi {
    let viewport = LogicalRect::new(0.0, 0.0, width, height);
    let (instance, view) = app.active_native_view(WID).unwrap();
    let cx = ViewCx {
        viewport,
        animation_phase_ms: 0,
        motion: ViewMotionCx::default(),
        terminal_font_px: 16.0,
        terminal_scale: 1.0,
        semantic_font: None,
        document: None,
    };
    app.native_runtime
        .render(instance, view, &cx)
        .unwrap()
        .compile(viewport)
        .unwrap()
}

/// In a window too short for every table, the rail's list scrolls inside its room: a wheel on
/// it and a press on a table off its foot bring that table into view, and when the tables were
/// read stays at the rail's foot, in the window.
/// NEGATIVE CONTROLS: with the rail's list never scrolled (`.scrolled(0.0)`), and with
/// `ScrollAt` on the rail scrolling the grid instead (its arm's guard `if false`), `a wheel on
/// the rail scrolls it` fails; with `rail_reveal` moving nothing, `in view once chosen` fails.
#[test]
fn the_rail_scrolls_in_a_short_window() {
    const W: f32 = 1200.0;
    const H: f32 = 420.0;
    let mut snapshot = one_sheet(Shown::default());
    snapshot.listed = crate::tables_store::listed(&aterm_db::alab::registry::REGISTRY);
    let mut app = open_with(snapshot);
    let ui = render_in(&app, W, H);
    let read = ui
        .semantic(&UiKey::new("tables/rail/read"))
        .expect("the foot");
    assert!(read.rect.y + read.rect.height <= H, "{:?}", read.rect);
    // The rail's list stands between the window's top and the foot.
    let foot = read.rect.y;
    let inside = |ui: &CompiledUi, key: &str| {
        let node = ui.semantic(&UiKey::new(key.to_owned())).expect(key);
        !node.offscreen && node.rect.y >= 0.0 && node.rect.y + node.rect.height <= foot + 0.5
    };
    let last = "tables/sheet/model";
    assert!(
        !inside(&ui, last),
        "the last table lies off the rail's foot"
    );

    // A wheel on the rail scrolls it, and leaves the grid where it is.
    app.dispatch_native_event(
        WID,
        AppEvent::ScrollAt {
            lines: 40,
            key: UiKey::new("tables/rail/list"),
        },
    )
    .unwrap();
    let ui = render_in(&app, W, H);
    assert!(inside(&ui, last), "a wheel on the rail scrolls it");
    assert!(!inside(&ui, "tables/sheet/machine"));
    assert_eq!(view_of(&app).scroll.get(), 0.0);

    // Back up, then a table off the foot chosen as `aterm ctl act` chooses it: brought in.
    app.dispatch_native_event(
        WID,
        AppEvent::ScrollAt {
            lines: -40,
            key: UiKey::new("tables/rail/list"),
        },
    )
    .unwrap();
    let ui = render_in(&app, W, H);
    assert!(inside(&ui, "tables/sheet/machine"));
    app.dispatch_native_event(WID, AppEvent::RevealControl(UiKey::new(last)))
        .unwrap();
    let ui = render_in(&app, W, H);
    assert!(inside(&ui, last), "in view once chosen");
}

/// Read from a registry: a chip the owner stated that its own Mac reports otherwise is marked in
/// the grid and its line says both (each Mac's newest statement, found by the read itself); what
/// aterm wrote when the Mac registered reads AUTOMATIC, never seen; the reference sheets keyed by
/// words show each row's key in their frozen column and sit under REFERENCE, as the rail puts
/// them; the owner's sheet puts what is set when a row is added under its own word; and a Mac's
/// own rows show their times as dates and their history no `empty → empty`.
/// NEGATIVE CONTROLS: with `Read::observed` finding nothing (`if refs.is_empty() || true`), `the
/// Mac's chip is read beside the stated one` fails; with `key_column` falling back to the first
/// column for word keys (no `prefix.is_empty()` arm), `a CLI's own name` fails (`the cell is
/// listed`); with `group_of` grouping by writer on every sheet, `under the rail's word` fails;
/// with the owned history comparing against `before.get(column)` as it stood (a column never
/// stated before unequal to an empty one), `nothing reads empty to empty` fails; with an owned
/// sheet's times shown as numbers (no `stamp`), `a date` fails. The cells-sheet arm reading an
/// aterm write as SEEN fails it at the status (`left: Some("SEEN")`).
#[test]
fn a_read_names_what_each_mac_reports_and_each_reference_row() {
    let lab = Lab::new(&[Role::Writer]);
    let here = Registry::at(lab.paths[0].clone());
    here.open().unwrap();
    here.seed().unwrap();
    aterm_secrets::client::admin_seed_swarm_roster(&lab.paths[0], &[]).unwrap();
    let probe = aterm_db::alab::register::Probe {
        hw_model: "Mac16,8".into(),
        chip: Some("Apple M4 Pro".into()),
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
    let facts_key = keys::uuid32(&lab.machine(0));
    let machine = view.owned["machine-facts"].rows[&facts_key]
        .value
        .get("mac")
        .and_then(aterm_json::Value::as_str)
        .expect("the facts bind a machine")
        .to_owned();
    here.write(vec![vec![put_in(
        &MACHINE,
        &machine,
        "chip",
        json!("Apple M2"),
    )]])
    .unwrap();

    let mut app = App::headless_for_test();
    app.open_tables_tab_at(Some(lab.paths[0].clone())).unwrap();
    settle(&mut app);
    let ui = render(&app);
    // Registration wrote the label: aterm's, and said so.
    let label = cell_says(&ui, &machine, "label");
    let label = label
        .strip_suffix(", set by aterm")
        .unwrap_or_else(|| panic!("aterm labelled the row: {label}"))
        .to_owned();
    assert_eq!(
        cell_says(&ui, &machine, "chip"),
        format!("Apple M2, {label} reports Apple M4 Pro"),
        "the Mac's chip is read beside the stated one"
    );
    act(&mut app, &format!("tables/cell/{machine}/chip"), None);
    let ui = render(&app);
    assert_eq!(said(&ui, "tables/said/tag").as_deref(), Some("DIFFERS"));
    let words = said(&ui, "tables/said/words").unwrap();
    assert!(
        words.starts_with(&format!("Chip: Apple M2 set at {label} "))
            && words.contains(&format!(
                " \u{b7} {label} reports Apple M4 Pro (Mac facts), "
            )),
        "the Mac's chip is read: {words}"
    );
    act(&mut app, &format!("tables/cell/{machine}/status"), None);
    let ui = render(&app);
    assert_eq!(said(&ui, "tables/said/tag").as_deref(), Some("AUTOMATIC"));
    let words = said(&ui, "tables/said/words").unwrap();
    assert!(
        words.starts_with(&format!("Status: set by aterm at {label} ")),
        "{words}"
    );
    let groups: Vec<String> = (0..60)
        .filter_map(|n| said(&ui, &format!("tables/groups/{n}/word")))
        .collect();
    assert!(
        groups.iter().any(|g| g == "SET WHEN ADDED"),
        "a status set when the row is added: {groups:?}"
    );

    // A CLI is named by its own key, frozen at the left, under the rail's word.
    act(&mut app, "tables/sheet/cli", None);
    let ui = render(&app);
    assert_eq!(cell_says(&ui, "codex", "~key"), "codex", "a CLI's own name");
    assert_eq!(
        said(&ui, "tables/head/~key").as_deref(),
        Some("Sort by Cli")
    );
    let key = rect(&ui, "tables/cell/codex/~key");
    let provider = rect(&ui, "tables/cell/codex/provider");
    assert!(key.x < provider.x, "the key column comes first");
    let groups: Vec<String> = (0..60)
        .filter_map(|n| said(&ui, &format!("tables/groups/{n}/word")))
        .filter(|word| !word.is_empty() && !word.starts_with("LINKED"))
        .collect();
    assert_eq!(groups, ["REFERENCE"], "under the rail's word");
    act(&mut app, "tables/cell/codex/~key", None);
    assert_eq!(
        said(&render(&app), "tables/said/words").as_deref(),
        Some("Cli: codex is this row's key, fixed when the row was added")
    );

    // A Mac's own rows: dates, and a history of what changed.
    act(&mut app, "tables/sheet/machine-facts", None);
    act(&mut app, &format!("tables/cell/{facts_key}/hosts"), None);
    let ui = render(&app);
    let at = cell_says(&ui, &facts_key, "at");
    let seen = cell_says(&ui, &facts_key, "seen");
    let date = |text: &str, len: usize| {
        text.len() == len
            && text.chars().enumerate().all(|(n, c)| match n {
                4 | 7 => c == '-',
                10 => c == ' ',
                13 => c == ':',
                _ => c.is_ascii_digit(),
            })
    };
    assert!(date(&at, 16), "a date: {at}");
    assert!(date(&seen, 10), "a date: {seen}");
    let lines: Vec<String> = (0..4)
        .flat_map(|n| (0..40).map(move |m| format!("tables/pane/written/{n}/{m}")))
        .filter_map(|key| said(&ui, &key))
        .collect();
    assert!(!lines.is_empty(), "the row's history is listed");
    assert!(
        lines
            .iter()
            .all(|line| !line.contains("empty \u{2192} empty")),
        "nothing reads empty to empty: {lines:?}"
    );
}

/// Write is a person's own press: `act` (an agent's control request) is refused it and writes
/// nothing, as are a control client's press and its palette command; the person's press writes.
/// NEGATIVE CONTROL: with `tables/write` not in `app_control::PERSON_ONLY_PREFIXES`, `act`
/// writes: `` act writes nothing of the Tables page ``.
#[test]
fn only_a_persons_press_writes_the_tables() {
    let lab = two_macs();
    let mut app = App::headless_for_test();
    app.open_tables_tab_at(Some(lab.paths[0].clone())).unwrap();
    settle(&mut app);
    let cell = format!("tables/cell/{MC}/location");
    act(&mut app, &cell, None);
    act(&mut app, &cell, None);
    act(&mut app, EDIT, Some(SemanticInput::Text("rack b".into())));
    key_in(&mut app, TextInputEvent::Submit);
    let (_, view) = app.active_native_view(WID).unwrap();
    let refused = app
        .act_app(crate::app_control::ActRequest {
            view,
            ui_key: WRITE.to_owned(),
            action: WRITE.to_owned(),
            value: None,
        })
        .expect_err("act writes nothing of the Tables page");
    assert!(
        refused.contains("the Tables page's Write button"),
        "{refused}"
    );
    let id = ActionId::new(WRITE);
    let refusal = app.with_controller_mark(true, |app| app.person_only_press_refusal(&id));
    assert!(refusal.is_some(), "the list names the button");
    let press = |app: &mut App| {
        app.dispatch_native_control_action(
            WID,
            view,
            UiKey::new(WRITE),
            ActionInvocation {
                id: id.clone(),
                value: None,
            },
            false,
        )
    };
    let command = |app: &mut App| {
        app.dispatch_native_view_event(
            WID,
            view,
            AppEvent::Action(ActionInvocation {
                id: id.clone(),
                value: None,
            }),
        )
    };
    assert_eq!(app.with_controller_mark(true, press).err(), refusal);
    assert_eq!(app.with_controller_mark(true, command).err(), refusal);
    settle(&mut app);
    assert_eq!(location(&lab, 0), Some(json!("rack a")), "nothing written");
    assert_eq!(
        said(&render(&app), "tables/bar/count").as_deref(),
        Some("1 change not written")
    );

    // The person's own press writes.
    assert_eq!(app.person_only_press_refusal(&id), None);
    assert_eq!(press(&mut app), Ok(crate::native_app::EventResult::Handled));
    settle_write(&mut app);
    assert_eq!(location(&lab, 0), Some(json!("rack b")));
}

/// A read that moves the selected row (another Mac's write sorts it elsewhere) scrolls it back
/// into the grid's view, as a move of the selection would.
/// NEGATIVE CONTROL: with `put_in_order` not calling `keep_in_view` on a read that moved the
/// row, the row is left off the grid's screen: `` the selected row is in view ``.
#[test]
fn a_read_that_moves_the_selected_row_keeps_it_in_view() {
    let mut app = open_with(tall_sheet(80, 3));
    let pk = pk_of(2);
    act(&mut app, &format!("tables/cell/{pk}/c1"), None);
    let ui = render(&app);
    let grid = rect(&ui, "tables/grid");
    let seat = format!("tables/cell/{pk}/c1/seat");
    assert!(place(&ui, &seat).bottom() <= grid.bottom());
    assert_eq!(view_of(&app).scroll.get(), 0.0);
    // The next read holds the rows in the other order: the selected row is now near the end.
    let mut moved = tall_sheet(80, 3);
    moved.sheets[0].rows.reverse();
    app.native_runtime.tables_mut().unwrap().show(moved);
    let ui = render(&app);
    let at = place(&ui, &seat);
    assert!(
        at.y >= grid.y && at.bottom() <= grid.bottom(),
        "the selected row is in view: {at:?} in {grid:?}"
    );
    assert!(view_of(&app).scroll.get() > 0.0);
    assert_eq!(view_of(&app).row.as_deref(), Some(pk.as_str()));
}

/// The pane's Keep and Undo buttons are reached from the grid by the arrows: Right past the
/// last column goes to the first Keep, Down and Up move among them, Return presses the one
/// focused, Left (or Up from the first) returns to the selected cell.
/// NEGATIVE CONTROL: with Right on the last column not entering the pane (`enter_pane` not
/// called), the keys stay on the cell: `` Right past the last column focuses the first Keep ``.
#[test]
fn the_panes_keep_and_undo_are_reached_by_the_arrows() {
    use aterm_types::keyboard::{Modifiers, NamedKey};
    let conflict = Shown {
        text: "Backup / Spare".into(),
        by: None,
        values: vec![
            ("Backup".into(), Some(author("m55", 2_000))),
            ("Spare".into(), Some(author("m73", 1_000))),
        ],
        ..Shown::default()
    };
    let mut app = open_with(one_sheet(conflict));
    // Each key lands on the frame the window drew after the one before.
    let key = |app: &mut App, named: NamedKey| {
        presented(app);
        key_with(
            app,
            aterm_types::keyboard::Key::Named(named),
            Modifiers::empty(),
        );
        view_of(app)
            .common
            .last_focus
            .as_ref()
            .map(|key| key.as_str().to_owned())
            .unwrap_or_default()
    };
    let keep = |n: usize| format!("{KEEP}nickname/{n}");
    let password = format!("tables/cell/{MC}/password");
    act(&mut app, &format!("tables/cell/{MC}/nickname"), None);
    // nickname, then password, the sheet's last column; then the pane.
    assert_eq!(key(&mut app, NamedKey::ArrowRight), password);
    assert_eq!(
        key(&mut app, NamedKey::ArrowRight),
        keep(0),
        "Right past the last column focuses the first Keep"
    );
    assert_eq!(key(&mut app, NamedKey::ArrowDown), keep(1));
    assert_eq!(
        key(&mut app, NamedKey::ArrowDown),
        keep(1),
        "the last stays"
    );
    assert_eq!(key(&mut app, NamedKey::ArrowUp), keep(0));
    assert_eq!(key(&mut app, NamedKey::ArrowDown), keep(1));
    key(&mut app, NamedKey::Enter);
    let ui = render(&app);
    assert_eq!(
        said(&ui, "tables/bar/count").as_deref(),
        Some("1 change not written"),
        "Return pressed the focused Keep"
    );
    assert_eq!(cell_says(&ui, MC, "nickname"), "Spare, not written");
    // Left returns to the cell; the Undo the Keep put in the pane, under the Keeps, is reached
    // the same way, and Return undoes.
    assert_eq!(key(&mut app, NamedKey::ArrowLeft), password);
    assert_eq!(key(&mut app, NamedKey::ArrowRight), keep(0));
    key(&mut app, NamedKey::ArrowDown);
    assert_eq!(
        key(&mut app, NamedKey::ArrowDown),
        format!("{UNDO}nickname")
    );
    assert_eq!(
        key(&mut app, NamedKey::Enter),
        password,
        "Undo returns to the cell"
    );
    assert!(
        said(&render(&app), "tables/bar/count").is_none(),
        "Return pressed the focused Undo"
    );
    // Up from the first returns to the selected cell.
    assert_eq!(key(&mut app, NamedKey::ArrowRight), keep(0));
    assert_eq!(key(&mut app, NamedKey::ArrowUp), password);
}

/// What the cell bar's seat `seat` speaks, whole: `address`, `value`, `state`.
fn bar(ui: &CompiledUi, seat: &str) -> Option<String> {
    said(ui, &format!("tables/cellbar/{seat}"))
}

/// The words the cell bar paints at `key`: a seat's `words`, then its `words/1`, `words/2`.
fn bar_paints(ui: &CompiledUi, key: &str) -> Option<String> {
    painted_words(ui, &format!("tables/cellbar/{key}"))
}

/// The cell bar's texts the page's clip cuts through: none where every run is fitted to its
/// seat.
fn bar_clipped(ui: &CompiledUi) -> Vec<String> {
    ui.text_fit_cuts(|_, _, clipped| clipped)
        .into_iter()
        .filter(|(key, _)| key.as_str().starts_with("tables/cellbar/"))
        .map(|(_, line)| line)
        .collect()
}

/// Every seat the cell bar speaks, whole, in the order it says them: together they read as one
/// sentence.
fn bar_says(ui: &CompiledUi) -> Vec<String> {
    ui.semantics
        .iter()
        .filter(|node| {
            node.role == SemanticRole::Text && node.key.as_str().starts_with("tables/cellbar/")
        })
        .map(|node| node.label.clone())
        .collect()
}

/// The words node `key` paints, whether or not the tree lists it.
fn painted_words(ui: &CompiledUi, key: &str) -> Option<String> {
    ui.paint
        .iter()
        .find(|node| node.key.as_str() == key)
        .and_then(|node| match &node.content {
            UiContent::Text(spec) => Some(spec.text.clone()),
            _ => None,
        })
}

/// On a scratch registry the cell bar stands between the tools and the grid, a row high, and
/// says of the selected cell which it is (its row's key, its column) and its whole value where
/// the grid's cell cuts it; who wrote the value and when is the line under the grid's to say,
/// and what its column holds the type line's under the column's name: the bar names none of
/// them. A linked count says which table's rows
/// it counts and how many name this row, in the words the line under the grid says it. The bar
/// is there before a cell is selected, so the grid does not move when one is.
/// NEGATIVE CONTROL: with `linked_words` wording one row as it words several (its `"1"` arm
/// removed), the count reads `` "1 live rows of Machines name this row" ``.
#[test]
fn the_cell_bar_names_the_selected_cell_and_its_whole_value() {
    // Wider than a column starts out.
    const LONG: &str = "rack a, third shelf, behind the panel";
    // A second machine, merged into the first: one row of Machines names `MC`.
    const MERGED: &str = "mc-000000000000000a";
    let lab = two_macs();
    Registry::at(lab.paths[0].clone())
        .write(vec![
            vec![
                put_in(&MACHINE, MERGED, "label", json!("m10")),
                put_in(&MACHINE, MERGED, "status", json!("active")),
                put_in(&MACHINE, MERGED, "merged_into", json!(MC)),
                put_in(&MACHINE, MERGED, "row", json!("live")),
            ],
            vec![put("location", json!(LONG))],
        ])
        .unwrap();

    let mut app = App::headless_for_test();
    app.open_tables_tab_at(Some(lab.paths[0].clone())).unwrap();
    settle(&mut app);
    let ui = render(&app);
    assert_eq!(
        said(&ui, "tables/cellbar").as_deref(),
        Some("Selected cell"),
        "the bar is a group named for what it shows"
    );
    assert_eq!(bar_says(&ui), ["No cell selected"]);
    let (bar_at, grid_at) = (place(&ui, "tables/cellbar"), place(&ui, "tables/grid"));
    let find = place(&ui, "tables/find");
    assert!(
        bar_at.y >= find.y + find.height,
        "under the tools: {bar_at:?}, {find:?}"
    );
    assert_eq!(bar_at.y + bar_at.height, grid_at.y, "on the grid");
    assert_eq!(
        bar_at.height,
        place(&ui, &format!("tables/cell/{MC}/location/seat")).height,
        "a row high"
    );
    assert_eq!((bar_at.x, bar_at.width), (grid_at.x, grid_at.width));

    act(&mut app, &format!("tables/cell/{MC}/location"), None);
    let ui = render(&app);
    assert_eq!(
        (place(&ui, "tables/cellbar"), place(&ui, "tables/grid")),
        (bar_at, grid_at),
        "a selection moves neither"
    );
    assert_eq!(bar(&ui, "address").as_deref(), Some("m9 \u{b7} Location"));
    // The grid's cell cuts the value to its column; the bar says all of it.
    let cut = painted_words(&ui, &format!("tables/cell/{MC}/location/text")).unwrap();
    assert!(cut.ends_with('\u{2026}') && shows(&cut, LONG), "{cut}");
    assert_eq!(bar(&ui, "value").as_deref(), Some(LONG));
    // Who wrote it and when is said once, by the line under the grid: the bar names neither.
    assert_eq!(bar(&ui, "state"), None, "the bar names no writer");
    assert_eq!(
        bar_says(&ui).len(),
        2,
        "the address and the value: the column's type is said under its name"
    );
    let line = said(&ui, "tables/said/words").expect("the line under the grid");
    assert!(line.starts_with("Location: set at "), "{line}");
    // A cell nobody wrote.
    act(&mut app, &format!("tables/cell/{MC}/nickname"), None);
    assert_eq!(bar_says(&render(&app)), ["m9 \u{b7} Nickname", "empty"]);

    // A cell set when its row is added, and the row's own key column, which names the row.
    act(&mut app, &format!("tables/cell/{MC}/status"), None);
    let ui = render(&app);
    assert_eq!(bar(&ui, "address").as_deref(), Some("m9 \u{b7} Status"));
    assert_eq!(bar(&ui, "value").as_deref(), Some("active"));
    let value_at = place(&ui, "tables/cellbar/value").x;
    act(&mut app, &format!("tables/cell/{MC}/label"), None);
    let ui = render(&app);
    assert_eq!(bar(&ui, "address").as_deref(), Some("m9 \u{b7} Label"));
    assert_eq!(
        place(&ui, "tables/cellbar/value").x,
        value_at,
        "a shorter address leaves the value where it was"
    );

    // A linked count: which table's rows it counts, and how many of them name this row.
    act(
        &mut app,
        &format!("tables/cell/{MC}/~machine.merged_into"),
        None,
    );
    let ui = render(&app);
    assert_eq!(
        bar_says(&ui),
        [
            "m9 \u{b7} Machines \u{b7} merged into",
            "1",
            "1 live row of Machines names this row",
        ]
    );
    // The row merged into it is named by none.
    act(
        &mut app,
        &format!("tables/cell/{MERGED}/~machine.merged_into"),
        None,
    );
    assert_eq!(
        bar_says(&render(&app)),
        [
            "m10 \u{b7} Machines \u{b7} merged into",
            "empty",
            "No live row of Machines names this row",
        ]
    );
    // The line under the grid says the count in the same words.
    assert_eq!(
        said(&render(&app), "tables/said/words").as_deref(),
        Some("No live row of Machines names this row")
    );
    assert_eq!(
        linked_words("1", "Machines"),
        "1 live row of Machines names this row"
    );
    assert_eq!(
        linked_words("2", "AI accounts"),
        "2 live rows of AI accounts name this row"
    );
    assert_eq!(
        linked_words("", "Emails"),
        "No live row of Emails names this row",
        "a count of none shows nothing in its cell"
    );
}

/// A staged cell reads its staged words in the cell bar in the "not written" ink, then that it
/// is not written and what the registry holds; taken back, it reads as stored again, with no
/// state: a written cell's writer is the line under the grid's to say.
#[test]
fn a_staged_cell_reads_not_written_in_the_cell_bar() {
    let mut app = open_with(one_sheet(Shown {
        text: "Spare".into(),
        by: Some(author("m73", 1_000)),
        ..Shown::default()
    }));
    let cell = format!("tables/cell/{MC}/nickname");
    act(&mut app, &cell, None);
    let ui = render(&app);
    assert_eq!(bar(&ui, "value").as_deref(), Some("Spare"));
    assert_eq!(
        style_of(&tree(&app), "tables/cellbar/value/words"),
        Some(StyleRef::Plain)
    );
    assert_eq!(bar(&ui, "state"), None, "a written cell has no state");

    app.native_runtime
        .tables_mut()
        .unwrap()
        .stage("machine", MC, "nickname", "Desk".into());
    let ui = render(&app);
    assert_eq!(
        bar_says(&ui),
        [
            "Spare \u{b7} Nickname",
            "Desk",
            "not written \u{b7} holds Spare",
        ],
        "the address is the row's stored key; the value is what is staged"
    );
    let drawn = tree(&app);
    assert_eq!(
        style_of(&drawn, "tables/cellbar/value/words"),
        Some(StyleRef::Unwritten)
    );
    assert_eq!(
        style_of(&drawn, "tables/cellbar/state/words"),
        Some(StyleRef::Note),
        "the words beside it are neutral"
    );

    // Nothing staged in a cell reads `empty`, still not written.
    app.native_runtime
        .tables_mut()
        .unwrap()
        .stage("machine", MC, "nickname", String::new());
    let ui = render(&app);
    assert_eq!(bar(&ui, "value").as_deref(), Some("empty"));
    assert_eq!(
        style_of(&tree(&app), "tables/cellbar/value/words"),
        Some(StyleRef::Unwritten)
    );
    assert_eq!(
        bar(&ui, "state").as_deref(),
        Some("not written \u{b7} holds Spare")
    );

    act(&mut app, "tables/pane/undo/nickname", None);
    let ui = render(&app);
    assert_eq!(bar(&ui, "value").as_deref(), Some("Spare"));
    assert_eq!(bar(&ui, "state"), None);
}

/// A conflicted cell reads every value in the cell bar as the grid shows them, how many there
/// are and that no clock picks one, and names no one writer.
#[test]
fn a_conflict_reads_its_values_in_the_cell_bar() {
    let mut app = open_with(one_sheet(Shown {
        text: "Backup / Spare".into(),
        by: None,
        values: vec![
            ("Backup".into(), Some(author("m55", 2_000))),
            ("Spare".into(), Some(author("m73", 1_000))),
        ],
        ..Shown::default()
    }));
    act(&mut app, &format!("tables/cell/{MC}/nickname"), None);
    let ui = render(&app);
    assert_eq!(
        bar_says(&ui),
        [
            "Backup / Spare \u{b7} Nickname",
            "Backup / Spare",
            "2 values \u{b7} no clock picks one",
        ]
    );
    assert_eq!(
        style_of(&tree(&app), "tables/cellbar/state/words"),
        Some(StyleRef::Note)
    );

    // A value a newer build wrote says so in the same seat.
    let mut app = open_with(one_sheet(Shown {
        text: "{\"shape\":2}".into(),
        by: Some(author("m55", 2_000)),
        raw: true,
        ..Shown::default()
    }));
    act(&mut app, &format!("tables/cell/{MC}/nickname"), None);
    assert_eq!(
        bar(&render(&app), "state").as_deref(),
        Some("written by a newer aterm \u{b7} read only here")
    );
}

/// With no cell selected the cell bar says so, in the note's ink; an empty cell reads `empty`,
/// dimmed, and a row whose key cell is empty is named by its own key.
#[test]
fn an_empty_cell_and_no_selection_read_so_in_the_cell_bar() {
    let mut app = open_with(one_sheet(Shown::default()));
    let ui = render(&app);
    assert_eq!(bar_says(&ui), ["No cell selected"]);
    assert_eq!(
        style_of(&tree(&app), "tables/cellbar/address/words"),
        Some(StyleRef::Note)
    );
    let (bar_at, grid_at) = (place(&ui, "tables/cellbar"), place(&ui, "tables/grid"));

    act(&mut app, &format!("tables/cell/{MC}/nickname"), None);
    let ui = render(&app);
    assert_eq!(
        bar_says(&ui),
        [format!("{MC} \u{b7} Nickname").as_str(), "empty",]
    );
    assert_eq!(
        style_of(&tree(&app), "tables/cellbar/value/words"),
        Some(StyleRef::Faint)
    );
    assert_eq!(
        (place(&ui, "tables/cellbar"), place(&ui, "tables/grid")),
        (bar_at, grid_at),
        "the grid stays where it is"
    );
}

/// Two rows that show one key are told apart in the cell bar by the last of each row's own
/// key, dimmed; a row whose key no other shows carries none.
#[test]
fn rows_that_show_one_key_are_told_apart_in_the_cell_bar() {
    let mut snapshot = tall_sheet(3, 2);
    for n in [0, 2] {
        snapshot.sheets[0].rows[n]
            .cells
            .get_mut("label")
            .unwrap()
            .text = "m34".into();
    }
    let mut app = open_with(snapshot);
    act(&mut app, &format!("tables/cell/{}/c1", pk_of(2)), None);
    let ui = render(&app);
    assert_eq!(
        bar(&ui, "address").as_deref(),
        Some("m34 \u{2026}0002 \u{b7} C1")
    );
    assert_eq!(bar_paints(&ui, "address/words").as_deref(), Some("m34"));
    assert_eq!(
        bar_paints(&ui, "address/words/1").as_deref(),
        Some("\u{2026}0002")
    );
    assert_eq!(
        bar_paints(&ui, "address/words/2").as_deref(),
        Some("\u{b7} C1")
    );
    assert_eq!(
        style_of(&tree(&app), "tables/cellbar/address/words/1"),
        Some(StyleRef::Faint)
    );
    act(&mut app, &format!("tables/cell/{}/c1", pk_of(1)), None);
    let ui = render(&app);
    assert_eq!(bar(&ui, "address").as_deref(), Some("m1 \u{b7} C1"));
    assert_eq!(bar_paints(&ui, "address/words").as_deref(), Some("m1"));
    assert_eq!(
        bar_paints(&ui, "address/words/1").as_deref(),
        Some("\u{b7} C1")
    );
    assert!(bar_paints(&ui, "address/words/2").is_none());
}

/// The rows one Mac states of a collection keyed `<row>.<the Mac's id>` (Accounts in use,
/// Mailbox readings) all show that Mac in their key column and all end in its id: the cell bar
/// tells them apart by the last of what stands before that shared ending, never by the ending
/// itself, which says the same of each. Whatever the keys, no two rows that show the same
/// words read the same mark, and a mark is the last four of the key wherever those tell the
/// rows apart.
/// NEGATIVE CONTROL: with `told_apart` answering the key's last four characters whatever the
/// twins end in, both of the Mac's rows read `` "m1 …505b · C1" ``.
#[test]
fn rows_one_mac_states_are_told_apart_in_the_cell_bar() {
    const M1: &str = "0123456789abcdef0123456789ab505b";
    const M2: &str = "fedcba9876543210fedcba98765477c1";
    let keys = [
        format!("ac-000000000000aaaa.{M1}"),
        format!("ac-000000000000bbbb.{M1}"),
        format!("ac-000000000000aaaa.{M2}"),
    ];
    let mut snapshot = tall_sheet(3, 2);
    for (row, (key, mac)) in snapshot.sheets[0]
        .rows
        .iter_mut()
        .zip(keys.iter().zip(["m1", "m1", "m2"]))
    {
        row.pk = key.clone();
        row.cells.get_mut("label").unwrap().text = mac.into();
    }
    let mut app = open_with(snapshot);
    let mut addresses = Vec::new();
    for key in &keys {
        act(&mut app, &format!("tables/cell/{key}/c1"), None);
        addresses.push(bar(&render(&app), "address").expect("the address"));
    }
    assert_eq!(
        addresses,
        [
            "m1 \u{2026}aaaa\u{2026} \u{b7} C1",
            "m1 \u{2026}bbbb\u{2026} \u{b7} C1",
            "m2 \u{b7} C1",
        ]
    );
    assert_eq!(
        style_of(&tree(&app), "tables/cellbar/address/words"),
        Some(StyleRef::Primary)
    );
    act(&mut app, &format!("tables/cell/{}/c1", keys[0]), None);
    assert_eq!(
        style_of(&tree(&app), "tables/cellbar/address/words/1"),
        Some(StyleRef::Faint)
    );

    // The mark itself, over keys of every shape: each row's differs from every twin's.
    let marks = |keys: &[&str]| -> Vec<String> {
        (0..keys.len())
            .map(|n| {
                let twins: Vec<&str> = keys
                    .iter()
                    .enumerate()
                    .filter(|(at, _)| *at != n)
                    .map(|(_, key)| *key)
                    .collect();
                told_apart(keys[n], &twins)
            })
            .collect()
    };
    let (a1, b1) = (format!("ac-aaaa.{M1}"), format!("ac-bbbb.{M1}"));
    let (a2, b2) = (format!("ac-aaaa.{M2}"), format!("ac-bbbb.{M2}"));
    for (keys, reads) in [
        // The last four tell them apart: nothing else is said.
        (
            vec!["mc-0000000000000000", "mc-0000000000000002"],
            vec!["\u{2026}0000", "\u{2026}0002"],
        ),
        // One Mac's rows, and a Mac that states the same rows under a second id: the part
        // that is the row's own, then the id's ending where another twin ends otherwise.
        (
            vec![a1.as_str(), b1.as_str(), a2.as_str(), b2.as_str()],
            vec![
                "\u{2026}aaaa\u{2026}505b",
                "\u{2026}bbbb\u{2026}505b",
                "\u{2026}aaaa\u{2026}77c1",
                "\u{2026}bbbb\u{2026}77c1",
            ],
        ),
        // Own parts that end alike take one character more than they share; a whole own part
        // has no ellipsis before it.
        (
            vec!["p-1aaaa.5505b", "p-2aaaa.5505b", "q-9bbbb.5505b"],
            vec![
                "\u{2026}1aaaa\u{2026}",
                "\u{2026}2aaaa\u{2026}",
                "\u{2026}bbbb\u{2026}",
            ],
        ),
        (
            vec!["x1.5505b", "y22.5505b"],
            vec!["x1\u{2026}", "y22\u{2026}"],
        ),
        // A key that is the ending of its twin's is said whole.
        (vec!["a505b", "xa505b"], vec!["a505b", "x\u{2026}"]),
        // Characters of more than one byte are counted, never cut.
        (
            vec!["r\u{e9}sum\u{e9}-\u{e9}t\u{e9}s", "r\u{e9}sum\u{e9}-autos"],
            vec!["\u{2026}\u{e9}t\u{e9}s", "\u{2026}utos"],
        ),
    ] {
        let read = marks(&keys);
        assert_eq!(read, reads, "{keys:?}");
        let distinct: std::collections::BTreeSet<&String> = read.iter().collect();
        assert_eq!(distinct.len(), keys.len(), "{keys:?}: {read:?}");
    }
}

/// A range reads in the cell bar as its rows and columns, how many of its cells are filled
/// (a staged cell by its staged words) and, where every column of it holds whole numbers, their
/// sum, least and most; a range over a column of words says no sum.
#[test]
fn a_range_reads_its_cells_and_sums_whole_numbers() {
    use aterm_types::keyboard::{Key, Modifiers, NamedKey};
    let mut app = open_with(sheet_with(["3", "", "10", "x", "-2"], ColumnKind::Number));
    act(&mut app, &format!("tables/cell/{}/c1", pk_of(0)), None);
    for _ in 0..4 {
        key_with(&mut app, Key::Named(NamedKey::ArrowDown), Modifiers::SHIFT);
    }
    let ui = render(&app);
    assert_eq!(
        bar_says(&ui),
        [
            "5 \u{d7} 1 cells",
            "5 cells \u{b7} 4 filled \u{b7} sum 11 \u{b7} least -2 \u{b7} most 10",
        ]
    );
    assert_eq!(
        style_of(&tree(&app), "tables/cellbar/state/words"),
        Some(StyleRef::Note)
    );
    // A staged cell counts as the grid shows it.
    app.native_runtime
        .tables_mut()
        .unwrap()
        .stage("machine", &pk_of(1), "c1", "4".into());
    assert_eq!(
        bar(&render(&app), "state").as_deref(),
        Some("5 cells \u{b7} 5 filled \u{b7} sum 15 \u{b7} least -2 \u{b7} most 10")
    );
    // With a column of words in it the range says only what it holds.
    key_with(&mut app, Key::Named(NamedKey::ArrowRight), Modifiers::SHIFT);
    assert_eq!(
        bar_says(&render(&app)),
        ["5 \u{d7} 2 cells", "10 cells \u{b7} 10 filled"]
    );
    // Escape leaves the one selected cell, which the bar names again.
    key_with(&mut app, Key::Named(NamedKey::Escape), Modifiers::empty());
    assert_eq!(
        bar(&render(&app), "address").as_deref(),
        Some("m4 \u{b7} C2")
    );

    assert_eq!(
        range_words(&["x", ""], true),
        "2 cells \u{b7} 1 filled",
        "no cell reads as a number: no sum"
    );
    let most = i128::MAX.to_string();
    assert_eq!(
        range_words(&[&most, "1"], true),
        "2 cells \u{b7} 2 filled",
        "a sum past what the page counts in is not said"
    );
    assert_eq!(
        range_words(&["7", "8"], false),
        "2 cells \u{b7} 2 filled",
        "a column of words is not summed"
    );
}

/// In a past table a cell changed since reads in the cell bar as the value it held then, what
/// it holds now and who wrote that; the same cell as it is now has no state, and the line under
/// the grid names its writer.
#[test]
fn a_cell_changed_since_reads_then_and_now_in_the_cell_bar() {
    let mut snapshot = one_sheet(Shown {
        text: "Spare".into(),
        by: Some(author("m9", 1_000)),
        ..Shown::default()
    });
    snapshot.at = Some(AsOf {
        operation: [3; 32],
        by: Some(author("m9", 1_000)),
    });
    snapshot.sheets[0].since.insert(
        (MC.into(), "nickname".into()),
        Since {
            now: "Backup / Desk".into(),
            by: vec![author("m55", 2_000), author("m73", 3_000)],
            members: false,
        },
    );
    let mut app = open_with(snapshot.clone());
    act(&mut app, &format!("tables/cell/{MC}/nickname"), None);
    assert_eq!(
        bar_says(&render(&app)),
        [
            "Spare \u{b7} Nickname",
            "Spare",
            "now Backup / Desk \u{b7} m55 / m73",
        ]
    );
    // A cell cleared since holds nothing now; a set says which members came and went.
    let since = snapshot.sheets[0]
        .since
        .get_mut(&(MC.to_owned(), "nickname".to_owned()))
        .unwrap();
    since.now = String::new();
    since.by.truncate(1);
    assert_eq!(
        since_runs(since)
            .iter()
            .map(|run| run.text.as_str())
            .collect::<Vec<_>>(),
        ["now empty", "\u{b7} m55"]
    );
    since.now = "+ m9, \u{2212} m3".into();
    since.members = true;
    assert_eq!(since_runs(since)[0].text, "since + m9, \u{2212} m3");
    // The same sheet as it is now: no state, and the line under the grid names its writer.
    snapshot.at = None;
    let mut app = open_with(snapshot);
    act(&mut app, &format!("tables/cell/{MC}/nickname"), None);
    let ui = render(&app);
    assert_eq!(bar(&ui, "state"), None);
    let line = said(&ui, "tables/said/words").expect("the line under the grid");
    assert!(line.starts_with("Nickname: set at m9 "), "{line}");
}

/// Where the line is too narrow for all of it, the cell's state keeps its seat in what the
/// value leaves, its words cut at their end; the address and the value stay, the value cut
/// only at the bar's end, and each seat still speaks the whole of what the line cuts.
#[test]
fn a_narrow_cell_bar_keeps_the_address_and_the_value() {
    let state = [
        run("now Backup / Desk", StyleRef::Note),
        run("\u{b7} m55 / m73", StyleRef::Note),
    ];
    let (first, last) = (
        text_width(&state[0].text, SemanticRole::Text, StyleRef::Note),
        text_width(&state[1].text, SemanticRole::Text, StyleRef::Note),
    );
    assert_eq!(
        rooms_to_end(&state, runs_width(&state)),
        [first, last],
        "room for all"
    );
    assert_eq!(
        rooms_to_end(&state, first + RUN_GAP + 10.0),
        [first, 10.0],
        "a line cut at its end keeps its first run whole"
    );
    assert_eq!(rooms_to_end(&state, first - 10.0), [first - 10.0, 0.0]);

    // On the page: a cell of a second column that holds `Spare`, with `value` typed into it and
    // not written.
    let open = |value: &str| {
        let mut snapshot = wide_sheet(2);
        snapshot.sheets[0].rows[0].cells.insert(
            "c1".to_owned(),
            Shown {
                text: "Spare".to_owned(),
                by: Some(author("m55", 1_000)),
                ..Shown::default()
            },
        );
        let mut app = open_with(snapshot);
        app.native_runtime
            .tables_mut()
            .unwrap()
            .stage("machine", MC, "c1", value.to_owned());
        act(&mut app, &format!("tables/cell/{MC}/c1"), None);
        render(&app)
    };
    let width = |text: &str, style| text_width(text, SemanticRole::Text, style);
    let address = "m73 \u{b7} C1";
    let state = "not written \u{b7} holds Spare";
    let ui = open("desk");
    assert_eq!(
        bar_says(&ui),
        [address, "desk", state],
        "a short value has room for all of it"
    );
    assert_eq!(bar_paints(&ui, "state/words").as_deref(), Some(state));
    // The room beside the address, as wide as this font sets it; each seat past the value's
    // keeps its gap before it.
    let room = place(&ui, "tables/cellbar").width
        - 22.0
        - 11.0
        - runs_width(&[
            run("m73", StyleRef::Primary),
            run("\u{b7} C1", StyleRef::Primary),
        ]);
    let state_w = width(state, StyleRef::Note) + BAR_GAP;
    let mut value = "desk".to_owned();
    // One word past where the value and its state fit: the state keeps its seat and is cut at
    // its end; it is still spoken whole.
    while width(&value, StyleRef::Unwritten) + state_w <= room {
        value.push_str(" desk");
    }
    assert!(
        width(&value, StyleRef::Unwritten) + BAR_GAP + width("not w\u{2026}", StyleRef::Note)
            <= room,
        "the value leaves room for the start of its state"
    );
    let ui = open(&value);
    assert_eq!(
        bar_says(&ui),
        [address, value.as_str(), state],
        "the state is spoken whole; the value is still whole"
    );
    let painted = bar_paints(&ui, "state/words").expect("the state's painted words");
    assert!(
        painted.ends_with('\u{2026}') && shows(&painted, state),
        "the state is cut at its end: {painted}"
    );
    assert_eq!(bar_clipped(&ui), Vec::<String>::new());

    // A value wider than the bar: only the address and the value show, the value cut at the end.
    let long = "a value that runs on well past the end of the bar, ".repeat(6);
    let mut app = open_with(one_sheet(Shown {
        text: long.trim().to_owned(),
        by: Some(author("m73", 1_000)),
        ..Shown::default()
    }));
    act(&mut app, &format!("tables/cell/{MC}/nickname"), None);
    let ui = render(&app);
    let says = bar_says(&ui);
    assert_eq!(says.len(), 2, "{says:?}");
    assert_eq!(
        says[1],
        long.trim(),
        "the seat speaks the whole value where the line cuts it"
    );
    let painted = bar_paints(&ui, "value/words").expect("the value's painted words");
    assert!(painted.ends_with('\u{2026}') && shows(&painted, long.trim()));
    let cut = painted_words(&ui, &format!("tables/cell/{MC}/nickname/text")).unwrap();
    assert!(
        painted.chars().count() > cut.chars().count(),
        "the bar cuts later than the grid's cell: {} then {}",
        cut.chars().count(),
        painted.chars().count()
    );
    let (bar_at, value) = (
        place(&ui, "tables/cellbar"),
        place(&ui, "tables/cellbar/value"),
    );
    assert!(
        value.x + value.width <= bar_at.x + bar_at.width - 10.0,
        "inside the bar: {value:?} in {bar_at:?}"
    );
}

/// The words the cell bar paints for its address, run by run.
fn address_paints(ui: &CompiledUi) -> Vec<String> {
    ["address/words", "address/words/1", "address/words/2"]
        .iter()
        .filter_map(|key| bar_paints(ui, key))
        .collect()
}

/// A long address keeps all of its words while the seat after the value can give way for it:
/// the state's words go first, and only a value with no room beside it cuts the address, in
/// its row's key and never in its column's words. The seat speaks the whole address either
/// way, and no run is cut by the seat's edge.
/// NEGATIVE CONTROL (run when the seat after the value was the writer's): with the address
/// held to 45% of the bar before any seat gives way (`named` capped at `inner * 0.45`), a
/// window 620 wide paints the key cut (`ann.synthetic.lon…`) while the seats after a short
/// value still show:
/// `` the address is cut only once the seats after the value are gone ``.
#[test]
fn a_long_address_takes_room_from_the_last_seats_before_it_is_cut() {
    const KEY: &str = "ann.synthetic.longname@example.com";
    const COLUMN: &str = "\u{b7} C1";
    // The seat after the value: the cell is typed into and not written.
    const STATE: &str = "not written \u{b7} holds Spare";
    let by = author("m55", 1_000);
    let open = |value: &str| {
        let mut snapshot = wide_sheet(2);
        let cells = &mut snapshot.sheets[0].rows[0].cells;
        cells.get_mut("label").unwrap().text = KEY.into();
        cells.insert(
            "c1".to_owned(),
            Shown {
                text: "Spare".to_owned(),
                by: Some(by.clone()),
                ..Shown::default()
            },
        );
        let mut app = open_with(snapshot);
        app.native_runtime
            .tables_mut()
            .unwrap()
            .stage("machine", MC, "c1", value.to_owned());
        act(&mut app, &format!("tables/cell/{MC}/c1"), None);
        app
    };
    let whole = format!("{KEY} {COLUMN}");
    let natural = runs_width(&[run(KEY, StyleRef::Primary), run(COLUMN, StyleRef::Primary)]);
    // (the address whole past 45% of the bar with the state cut or left out for it, the key
    // cut)
    let mut seen = (false, false);
    let long = "a value that runs on past any room the bar has for it ".repeat(4);
    for value in ["spare", long.trim()] {
        let app = open(value);
        for width in (620..=1400).step_by(20) {
            let ui = render_wide(&app, width as f32);
            let at = format!("{width} wide, {value:?}");
            assert_eq!(bar_clipped(&ui), Vec::<String>::new(), "{at}");
            let says = bar_says(&ui);
            assert_eq!(says[0], whole, "the address is spoken whole: {at}");
            assert_eq!(says[1], value, "the value is spoken whole: {at}");
            let paints = address_paints(&ui);
            let inner = place(&ui, "tables/cellbar").width - 22.0;
            if paints == [KEY, COLUMN] {
                seen.0 |= natural > inner * 0.45
                    && bar_paints(&ui, "state/words").as_deref() != Some(STATE);
                continue;
            }
            // Cut: only the value stands beside it, and the cut is in the row's key.
            assert_eq!(
                says.len(),
                2,
                "the address is cut only once the seats after the value are gone: {at}, {paints:?}"
            );
            assert_eq!(paints.len(), 2, "{at}: {paints:?}");
            assert!(
                paints[0].ends_with('\u{2026}') && shows(&paints[0], KEY),
                "{at}: {paints:?}"
            );
            assert_eq!(paints[1], COLUMN, "the column's words stay: {at}");
            let (bar_at, value_at) = (
                place(&ui, "tables/cellbar"),
                place(&ui, "tables/cellbar/value"),
            );
            assert!(
                (value_at.x + value_at.width - (bar_at.x + bar_at.width - 11.0)).abs() < 0.5,
                "a cut address leaves no spare room beside the value: {at}, {value_at:?}"
            );
            seen.1 = true;
        }
    }
    assert_eq!(
        seen,
        (true, true),
        "the widths cover an address whole past 45% of the bar, and one cut"
    );
}

/// The room each run of a line takes where the line is too narrow: the first run is cut to its
/// first characters, then the last run, and the rooms never pass the line, so no run is cut by
/// its seat's edge. Two rows that show one key keep the part of the key that tells them apart
/// at a narrow window and at a large text size.
/// NEGATIVE CONTROL: with `run_rooms` taking all of the overflow from the first run (the last
/// kept at its own width), the key that tells the rows apart is cut away:
/// `` a key of three characters is not cut  left: 15.0 right: 25.0 ``.
#[test]
fn a_cut_address_ends_in_an_ellipsis_and_never_passes_its_seat() {
    let runs = [
        run("m34", StyleRef::Primary),
        run("\u{2026}09ce", StyleRef::Faint),
        run("\u{b7} Machines \u{b7} merged into", StyleRef::Primary),
    ];
    let natural = runs_width(&runs);
    let fits = |rooms: &[f32]| rooms.iter().sum::<f32>() + RUN_GAP * (rooms.len() - 1) as f32;
    assert_eq!(fits(&run_rooms(&runs, natural)), natural, "room for all");
    let key = text_width("m34", SemanticRole::Text, StyleRef::Primary);
    for short in [10.0, key + 20.0, natural * 0.5] {
        let rooms = run_rooms(&runs, natural - short);
        assert!(
            fits(&rooms) <= natural - short + 0.01,
            "the rooms fit the line: {rooms:?} in {}",
            natural - short
        );
        assert_eq!(rooms[0], key, "a key of three characters is not cut");
    }
    // A long first run gives way first, down to its first characters and an ellipsis.
    let long = [
        run("ann.synthetic@example.com", StyleRef::Primary),
        run("\u{b7} Mailbox hint", StyleRef::Primary),
    ];
    let column = text_width(&long[1].text, SemanticRole::Text, StyleRef::Primary);
    let least = text_width("ann.\u{2026}", SemanticRole::Text, StyleRef::Primary);
    let rooms = run_rooms(&long, runs_width(&long) - 40.0);
    assert_eq!(rooms[1], column, "the last run keeps its words");
    assert!(rooms[0] >= least, "{rooms:?}");
    let rooms = run_rooms(&long, least + RUN_GAP + column - 30.0);
    assert_eq!(rooms[0], least, "the first run keeps its first characters");
    assert_eq!(rooms[1], column - 30.0, "then the last run is cut");
    assert_eq!(run_rooms(&long, 0.0), [0.0, 0.0]);

    // On the page: two rows show `m34`, and a long column's cell of one is selected.
    let mut snapshot = tall_sheet(3, 2);
    let sheet = &mut snapshot.sheets[0];
    for n in [0, 2] {
        sheet.rows[n].cells.get_mut("label").unwrap().text = "m34".into();
    }
    sheet.columns[1].label = "MACHINES \u{b7} MERGED INTO".into();
    let whole = "m34 \u{2026}0002 \u{b7} Machines \u{b7} merged into";
    let mut app = open_with(snapshot);
    act(&mut app, &format!("tables/cell/{}/c1", pk_of(2)), None);
    let mut cut = 0;
    for scale in [1.0, 1.5] {
        crate::native_appearance::install_preferences(
            crate::native_appearance::AppearancePreferences {
                text_scale: scale,
                ..crate::native_appearance::current_preferences()
            },
        );
        for width in (620..=1400).step_by(20) {
            let ui = render_wide(&app, width as f32);
            let at = format!("{width} wide at {scale}");
            assert_eq!(bar_clipped(&ui), Vec::<String>::new(), "{at}");
            assert_eq!(bar(&ui, "address").as_deref(), Some(whole), "{at}");
            let paints = address_paints(&ui);
            assert_eq!(
                (paints[0].as_str(), paints[1].as_str()),
                ("m34", "\u{2026}0002"),
                "what tells the two rows apart stays: {at}"
            );
            let column = paints.get(2).map_or("", String::as_str);
            if column != "\u{b7} Machines \u{b7} merged into" {
                assert!(
                    column.ends_with('\u{2026}')
                        && shows(column, "\u{b7} Machines \u{b7} merged into"),
                    "a cut run ends in an ellipsis: {at}, {paints:?}"
                );
                cut += 1;
            }
        }
    }
    assert!(cut > 0, "some width cuts the column's words");
}

/// A selected cell whose row the find field leaves out is still the selection: the line under
/// the grid goes on saying where its value came from, so the cell bar over the grid names the
/// same cell and says it is not among the rows found. The bar says no value there (the grid
/// shows none, and a copy or a Delete acts on none) and says the value again once the row
/// shows. The bar says no cell is selected exactly where the line says so: before any press,
/// and once a new read no longer holds the selected row.
/// NEGATIVE CONTROL: with `bar_words` naming no cell the grid does not show (its `hidden`
/// never found), the bar reads `` ["No cell selected"] `` over a line that tells of `C1`.
#[test]
fn a_selected_cell_the_find_field_leaves_out_is_named_as_not_found() {
    const NOTHING: &str = "Select a cell to see where its value came from";
    let line = |ui: &CompiledUi| said(ui, "tables/said/words").expect("the line under the grid");
    let mut app = open_with(narrow_sheet(3, 2));
    let ui = render(&app);
    assert_eq!(bar_says(&ui), ["No cell selected"]);
    assert_eq!(line(&ui), NOTHING);

    act(&mut app, &format!("tables/cell/{}/c1", pk_of(1)), None);
    let ui = render(&app);
    assert_eq!(bar(&ui, "address").as_deref(), Some("m1 \u{b7} C1"));
    assert_eq!(bar(&ui, "value").as_deref(), Some("v"));
    let shown = line(&ui);
    assert!(shown.starts_with("C1: "), "{shown}");

    act(&mut app, FIND, Some(SemanticInput::Text("m2".into())));
    let ui = render(&app);
    assert!(
        said(&ui, &format!("tables/cell/{}/c1", pk_of(1))).is_none(),
        "the grid no longer lists the row"
    );
    assert_eq!(
        line(&ui),
        shown,
        "the line still tells of the selected cell"
    );
    assert_eq!(
        bar_says(&ui),
        ["m1 \u{b7} C1", "not among the rows found"],
        "the bar names the cell the line tells of, and no value the grid does not show"
    );
    assert_eq!(bar(&ui, "value"), None);
    assert_eq!(
        style_of(&tree(&app), "tables/cellbar/state/words"),
        Some(StyleRef::Note)
    );
    assert!(bar_clipped(&ui).is_empty(), "{:?}", bar_clipped(&ui));

    act(&mut app, FIND, Some(SemanticInput::Text(String::new())));
    let ui = render(&app);
    assert_eq!(bar(&ui, "address").as_deref(), Some("m1 \u{b7} C1"));
    assert_eq!(bar(&ui, "value").as_deref(), Some("v"));

    // A new read without the selected row: no cell to the line, none to the bar.
    app.native_runtime
        .tables_mut()
        .unwrap()
        .show(narrow_sheet(1, 2));
    let ui = render(&app);
    assert_eq!(line(&ui), NOTHING);
    assert_eq!(bar_says(&ui), ["No cell selected"]);
}

/// The cell bar and the line under the grid never say two things of one cell. The bar's address
/// ends in the field's words the line begins with, its value is the grid's own text for the
/// cell, and where a value came from is said once, by the line: a value a person set (SET) and
/// a Mac's own statement (SEEN) have no state in the bar. A held value its Mac reports
/// otherwise (DIFFERS) reads in the bar as the value the grid's cell shows, never the Mac's,
/// and its state is the grid's mark with the words the grid's cell speaks, which the line says
/// too; a Mac that reports the held value, and a staged cell, wear no such mark in the grid
/// and none in the bar.
#[test]
fn the_cell_bar_and_the_line_under_the_grid_agree() {
    let mut app = open_with(sheets_with_provenance("m55", 270_000));
    for (sheet, column, tag, says) in [
        (
            "machine",
            "nickname",
            "SET",
            vec!["Spare \u{b7} Nickname", "Spare"],
        ),
        (
            "machine",
            "chip",
            "DIFFERS",
            vec![
                "Spare \u{b7} Chip",
                "M2",
                "differs \u{b7} m22 reports M4 Pro",
            ],
        ),
        (
            "machine-facts",
            "nickname",
            "SEEN",
            vec!["m22 \u{b7} Nickname", "m22"],
        ),
    ] {
        act(&mut app, &format!("tables/sheet/{sheet}"), None);
        act(&mut app, &format!("tables/cell/{MC}/{column}"), None);
        let ui = render(&app);
        let at = format!("{sheet}.{column}");
        assert_eq!(said(&ui, "tables/said/tag").as_deref(), Some(tag), "{at}");
        assert_eq!(bar_says(&ui), says, "{at}");
        // The same field words: the bar's address ends in what the line begins with.
        let line = said(&ui, "tables/said/words").expect("the line under the grid");
        let (_, field) = says[0].rsplit_once(" \u{b7} ").expect("key, then column");
        assert!(line.starts_with(&format!("{field}: ")), "{at}: {line}");
        // The same value: the bar reads what the grid's cell shows.
        let cell = cell_says(&ui, MC, column);
        assert!(
            cell == says[1] || cell.starts_with(&format!("{}, ", says[1])),
            "{at}: the grid's cell says {cell}"
        );
        // Who wrote it, where and when: the line's alone.
        for seat in bar_says(&ui) {
            assert!(
                !["set at", "seen ", " ago", " on ", "m55"]
                    .iter()
                    .any(|word| seat.contains(word)),
                "{at}: the bar names no writer and no time: {seat}"
            );
        }
        if tag == "DIFFERS" {
            // What the Mac reports, in the grid's own words, in all three places.
            let reports = says[2].strip_prefix("differs \u{b7} ").unwrap();
            assert_eq!(cell, format!("M2, {reports}"), "{at}");
            assert!(line.contains(reports), "{at}: {line}");
            assert!(line.starts_with("Chip: M2 set at m55 "), "{at}: {line}");
        }
    }

    // A Mac that reports what the cell holds: no mark in the grid, no state in the bar.
    let mut snapshot = sheets_with_provenance("m55", 270_000);
    cell_of(&mut snapshot, "chip").text = "M4 Pro".into();
    let mut app = open_with(snapshot);
    act(&mut app, &format!("tables/cell/{MC}/chip"), None);
    let ui = render(&app);
    assert_eq!(cell_says(&ui, MC, "chip"), "M4 Pro");
    assert_eq!(
        bar_says(&ui),
        ["Spare \u{b7} Chip", "M4 Pro"],
        "the Mac agrees"
    );

    // A staged cell is not written, and the grid marks nothing else on it.
    let mut app = open_with(sheets_with_provenance("m55", 270_000));
    app.native_runtime
        .tables_mut()
        .unwrap()
        .stage("machine", MC, "chip", "M5".into());
    act(&mut app, &format!("tables/cell/{MC}/chip"), None);
    let ui = render(&app);
    assert_eq!(cell_says(&ui, MC, "chip"), "M5, not written");
    assert_eq!(
        bar_says(&ui),
        ["Spare \u{b7} Chip", "M5", "not written \u{b7} holds M2",]
    );
    assert_eq!(said(&ui, "tables/said/tag").as_deref(), Some("NOT WRITTEN"));

    // A find that leaves the selected row out: the line still says where the cell's value came
    // from, and the bar names that cell, with no value the grid does not show.
    let mut app = open_with(sheets_with_provenance("m55", 270_000));
    act(&mut app, &format!("tables/cell/{MC}/chip"), None);
    act(
        &mut app,
        FIND,
        Some(SemanticInput::Text("no row has this".into())),
    );
    let ui = render(&app);
    assert!(said(&ui, &format!("tables/cell/{MC}/chip")).is_none());
    assert_eq!(said(&ui, "tables/said/tag").as_deref(), Some("DIFFERS"));
    let line = said(&ui, "tables/said/words").expect("the line under the grid");
    assert!(line.starts_with("Chip: M2 set at m55 "), "{line}");
    assert_eq!(
        bar_says(&ui),
        ["Spare \u{b7} Chip", "not among the rows found"],
        "one selection, named by both"
    );
}

/// The cell bar stands on the grid of every table the rail lists, read from a registry: the
/// owner's, each Mac's own rows and the reference rows; and Check, which is no table's grid,
/// has no bar. On a read the bar says of a chip its Mac reports otherwise the value the cell
/// holds and the grid's mark, names a reference row by its key, and a Mac's own row by the Mac
/// that states it.
#[test]
fn the_cell_bar_stands_on_every_table_the_rail_lists_and_not_on_check() {
    let lab = Lab::new(&[Role::Writer]);
    let here = Registry::at(lab.paths[0].clone());
    here.open().unwrap();
    here.seed().unwrap();
    aterm_secrets::client::admin_seed_swarm_roster(&lab.paths[0], &[]).unwrap();
    let probe = aterm_db::alab::register::Probe {
        hw_model: "Mac16,8".into(),
        chip: Some("Apple M4 Pro".into()),
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
    let facts_key = keys::uuid32(&lab.machine(0));
    let machine = view.owned["machine-facts"].rows[&facts_key]
        .value
        .get("mac")
        .and_then(aterm_json::Value::as_str)
        .expect("the facts bind a machine")
        .to_owned();
    here.write(vec![vec![put_in(
        &MACHINE,
        &machine,
        "chip",
        json!("Apple M2"),
    )]])
    .unwrap();

    let mut app = App::headless_for_test();
    app.open_tables_tab_at(Some(lab.paths[0].clone())).unwrap();
    settle(&mut app);

    // Every table the rail lists: the bar on its grid.
    let names = app.native_runtime.tables_mut().unwrap().listed_names();
    assert!(
        names.len() > 12,
        "every registry table is listed: {names:?}"
    );
    for name in &names {
        act(&mut app, &format!("tables/sheet/{name}"), None);
        let ui = render(&app);
        assert_eq!(
            said(&ui, "tables/cellbar").as_deref(),
            Some("Selected cell"),
            "{name} has its cell bar"
        );
        let (bar_at, grid_at) = (place(&ui, "tables/cellbar"), place(&ui, "tables/grid"));
        assert_eq!(bar_at.y + bar_at.height, grid_at.y, "{name}: on its grid");
    }

    // The owner's table: a chip its own Mac reports otherwise.
    act(&mut app, "tables/sheet/machine", None);
    act(&mut app, &format!("tables/cell/{machine}/chip"), None);
    let ui = render(&app);
    let label = cell_says(&ui, &machine, "label");
    let label = label.strip_suffix(", set by aterm").unwrap_or(&label);
    assert_eq!(said(&ui, "tables/said/tag").as_deref(), Some("DIFFERS"));
    assert_eq!(
        bar(&ui, "address"),
        Some(format!("{label} \u{b7} Chip")),
        "the row's key, then the column"
    );
    assert_eq!(
        bar(&ui, "value").as_deref(),
        Some("Apple M2"),
        "what the cell holds, as the grid shows it"
    );
    assert_eq!(
        bar(&ui, "state"),
        Some(format!("differs \u{b7} {label} reports Apple M4 Pro")),
        "the grid's mark, and what the grid's cell speaks"
    );

    // A reference table main lists: a CLI's own key, and a column of it.
    act(&mut app, "tables/sheet/cli", None);
    act(&mut app, "tables/cell/codex/~key", None);
    assert_eq!(bar_says(&render(&app)), ["codex \u{b7} Cli", "codex"]);
    act(&mut app, "tables/cell/codex/provider", None);
    let ui = render(&app);
    assert_eq!(
        bar(&ui, "address").as_deref(),
        Some("codex \u{b7} Provider")
    );

    // A Mac's own rows: which Mac states the row, and what it states.
    act(&mut app, "tables/sheet/machine-facts", None);
    act(&mut app, &format!("tables/cell/{facts_key}/hosts"), None);
    let ui = render(&app);
    assert_eq!(said(&ui, "tables/said/tag").as_deref(), Some("SEEN"));
    let mac = cell_says(&ui, &facts_key, "~mac");
    assert_eq!(bar(&ui, "address"), Some(format!("{mac} \u{b7} Hosts")));
    assert_eq!(bar(&ui, "value").as_deref(), Some("mac-one"));
    assert_eq!(
        bar(&ui, "state"),
        None,
        "when it was seen is the line's to say"
    );
    act(&mut app, &format!("tables/cell/{facts_key}/~mac"), None);
    let ui = render(&app);
    assert_eq!(
        bar_says(&ui),
        [format!("{mac} \u{b7} Stated by"), mac.clone()],
        "the Mac that states the row"
    );

    // Check is no table's grid: no bar.
    act(&mut app, crate::native_tables_check::OPEN, None);
    settle(&mut app);
    let ui = render(&app);
    assert!(
        said(&ui, "tables/check/list").is_some()
            || ui.semantic(&UiKey::new("tables/grid")).is_none(),
        "Check is shown"
    );
    assert!(
        ui.semantic(&UiKey::new("tables/cellbar")).is_none(),
        "Check has no cell bar"
    );
    assert!(ui.semantic(&UiKey::new("tables/grid")).is_none());
}
