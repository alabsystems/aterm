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

fn render(app: &App) -> CompiledUi {
    let (instance, view) = app
        .active_native_view(WID)
        .expect("the Tables tab is in front");
    let cx = ViewCx {
        viewport: LogicalRect::new(0.0, 0.0, 1400.0, 900.0),
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
        .compile(cx.viewport)
        .unwrap()
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
    let who = said(&ui, "tables/pane/written/0/who").unwrap();
    assert!(who.starts_with(&format!("{by} \u{b7} ")), "{who}");
    assert!(who.contains(" \u{b7} claimed "), "{who}");
}

fn author(machine: &str, at_ms: u64) -> Author {
    Author {
        kind: AuthorKind::Person,
        machine: Some(machine.into()),
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
            columns: vec![column("nickname"), password],
            rows: vec![SheetRow {
                pk: MC.into(),
                cells,
            }],
            written: Vec::new(),
            base: Base {
                table: "alab/registry".into(),
                collection: "machine".into(),
                version: 2,
                reveal: false,
                header: Vec::new(),
                rows: Vec::new(),
            },
        }],
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
