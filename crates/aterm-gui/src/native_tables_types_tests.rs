// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Tables page's typed cells (docs/PLAN.md SF-37) on a scratch registry
//! (`aterm_db::table::fixture::Lab`: an isolated home, no live vault, fleet or Commons): each
//! header says its column's type, an open cell offers what its type holds, and a staged value
//! is checked at once by the save's own reading of the grid. Every value is synthetic.

use super::native_tables_tests::{
    MC, WID, act, cell_says, key_in, one_sheet, open_with, press_key, put_in, render, render_in,
    said, settle, two_macs, view_of,
};
use super::*;
use aterm_db::alab::Registry;
use aterm_db::alab::registry::{ACCOUNT, ADDRESS};
use aterm_db::att::{Handle, REGISTRY};
use aterm_db::table::fixture::{Lab, repair_commons};
use aterm_json::json;
use aterm_types::keyboard::NamedKey;

use crate::App;
use crate::native_ui::CompiledUi;

/// The page opened on the registry at `lab`'s first Mac, read.
fn opened(lab: &Lab) -> App {
    let mut app = App::headless_for_test();
    app.open_tables_tab_at(Some(lab.paths[0].clone())).unwrap();
    settle(&mut app);
    app
}

/// The words the open cell's list shows, top down, as each is said.
fn listed(ui: &CompiledUi) -> Vec<String> {
    (0..)
        .map_while(|n| said(ui, &format!("{CHOICE}{n}")))
        .collect()
}

/// Open the cell at `key`: a press selects it and a second opens it, so one press opens the
/// cell already selected.
fn open(app: &mut App, key: &str) {
    act(app, key, None);
    if view_of(app).editing.is_none() {
        act(app, key, None);
    }
    assert!(view_of(app).editing.is_some(), "{key} opened");
}

/// What the open cell's field holds.
fn field_holds(ui: &CompiledUi) -> Option<String> {
    let node = ui.semantic(&UiKey::new(EDIT.to_owned()))?;
    match &node.value {
        SemanticValue::Text(text) => Some(text.clone()),
        _ => None,
    }
}

/// An enum's cell opens on a list of its compiled words; the arrows move the mark and Return
/// stages the word marked, Escape leaves the cell as it was, and a press on a word stages it. A
/// word that is not one of the column's, typed and staged, is refused at once on its cell by
/// the rule the save reads the grid with; the Write bar counts it, and a Write holds its table
/// back whole (the save would refuse every cell of it), says so, and sends nothing.
/// NEGATIVE CONTROL: with `TablesApp::recheck` returning before it checks anything, the typed
/// word stays staged unrefused until Write: `` left: "server, not written" right: "server, not
/// written, refused: pick one of the column's own words" ``. With
/// `Opens::List` opening as a text field (`opens` answering `Field` for an enum), the cell lists
/// no words: `` the role's list names its six words and empty, the empty cell's marked left:
/// [] ``. With `write` sending a table whatever the check refuses in it (its `held` skip
/// removed), the Write is sent: `` a Write holds the table back ``.
#[test]
fn a_staged_enum_cell_offers_its_words_and_refuses_another() {
    let lab = two_macs();
    let mut app = opened(&lab);
    let ui = render(&app);
    assert_eq!(said(&ui, "tables/type/role").as_deref(), Some("one of 6"));
    let cell = format!("{CELL}{MC}/role");
    open(&mut app, &cell);
    let ui = render(&app);
    assert_eq!(
        listed(&ui),
        [
            "worker",
            "desk",
            "release",
            "vault",
            "lab",
            "spare",
            "empty, marked"
        ],
        "the role's list names its six words and empty, the empty cell's marked"
    );
    // The keys: Down goes round to the first word and puts it in the field; Escape stages
    // nothing.
    press_key(&mut app, NamedKey::ArrowDown);
    let ui = render(&app);
    assert_eq!(
        said(&ui, &format!("{CHOICE}0")).as_deref(),
        Some("worker, marked")
    );
    assert_eq!(field_holds(&ui).as_deref(), Some("worker"));
    press_key(&mut app, NamedKey::Escape);
    let ui = render(&app);
    assert!(listed(&ui).is_empty(), "Escape closes the list");
    assert_eq!(cell_says(&ui, MC, "role"), "empty", "and stages nothing");
    // Down twice and Return stage the second word, and the selection moves down as Return does.
    open(&mut app, &cell);
    press_key(&mut app, NamedKey::ArrowDown);
    press_key(&mut app, NamedKey::ArrowDown);
    press_key(&mut app, NamedKey::Enter);
    let ui = render(&app);
    assert!(listed(&ui).is_empty(), "Return closes the list");
    assert_eq!(cell_says(&ui, MC, "role"), "desk, not written");
    // The list opens on the word the cell holds; a press on a word stages it.
    open(&mut app, &cell);
    assert_eq!(
        said(&render(&app), &format!("{CHOICE}1")).as_deref(),
        Some("desk, marked"),
        "the list opens on the word the cell holds"
    );
    act(&mut app, &format!("{CHOICE}2"), None);
    assert_eq!(cell_says(&render(&app), MC, "role"), "release, not written");
    // A word the column does not know, typed: refused on its cell as it is staged.
    open(&mut app, &cell);
    act(&mut app, EDIT, Some(SemanticInput::Text("server".into())));
    key_in(&mut app, TextInputEvent::Submit);
    let ui = render(&app);
    let refused = "pick one of the column's own words";
    assert_eq!(
        cell_says(&ui, MC, "role"),
        format!("server, not written, refused: {refused}")
    );
    assert_eq!(
        said(&ui, "tables/pane/refused/role").as_deref(),
        Some(refused)
    );
    assert_eq!(
        said(&ui, "tables/bar/count").as_deref(),
        Some("1 change not written \u{b7} 1 refused")
    );
    // A Write holds the table back and says what to do; nothing of it is sent or written.
    act(&mut app, WRITE, None);
    let tables = app.native_runtime.tables_mut().unwrap();
    assert!(!tables.writing, "a Write holds the table back");
    assert!(tables.refused.is_empty(), "no save was asked to refuse it");
    let ui = render(&app);
    assert_eq!(
        said(&ui, "tables/bar/note").as_deref(),
        Some("Machines held back: 1 refused cell")
    );
    assert_eq!(
        cell_says(&ui, MC, "role"),
        format!("server, not written, refused: {refused}")
    );
    let view = Handle::at(lab.paths[0].clone(), &REGISTRY).view().unwrap();
    assert!(
        view.project(&aterm_db::alab::registry::MACHINE).rows[MC]
            .get("role")
            .is_none(),
        "the registry holds no role"
    );
    // A word of the column staged over it is no longer refused.
    open(&mut app, &cell);
    act(&mut app, &format!("{CHOICE}4"), None);
    assert_eq!(cell_says(&render(&app), MC, "role"), "lab, not written");
}

/// Each header says what its column holds under its name, marks a key, and names the table a
/// reference leads to by its rail title; columns stand in the display order compiled beside the
/// collection, the naming column first.
/// NEGATIVE CONTROL: with `cells_sheet` leaving the grid's columns in the order the schema
/// declares them (its sort by `laid_out` removed), the machine's STORED columns read
/// `["label", "aliases", "role", …]`: `` left: ["label", "aliases", …] right: ["label",
/// "nickname", …] ``. With a set's words taking its inner type's singular (`several` without
/// `plural`): `` aliases left: Some("set of Mac label") right: Some("set of Mac labels") ``.
#[test]
fn each_header_says_its_type_and_the_naming_column_leads() {
    let lab = two_macs();
    let mut app = opened(&lab);
    {
        let tables = app.native_runtime.tables_mut().unwrap();
        let names = |name: &str, n: usize| -> Vec<String> {
            ordered(tables.sheet_named(name).unwrap())
                .into_iter()
                .filter(|c| c.group == Group::Stored)
                .map(|c| c.name.clone())
                .take(n)
                .collect()
        };
        assert_eq!(
            names("machine", 5),
            ["label", "nickname", "role", "location", "chip"]
        );
        assert_eq!(
            names("account", 3),
            ["label", "provider", "login"],
            "an account is named by its label"
        );
    }
    let ui = render(&app);
    for (column, says) in [
        ("label", "Mac label"),
        ("aliases", "set of Mac labels"),
        ("nickname", "text"),
        ("status", "one of 4"),
        ("ram_gb", "whole number"),
        ("clis", "\u{2192} CLIs, several"),
        ("model", "\u{2192} Mac models"),
    ] {
        assert_eq!(
            said(&ui, &format!("tables/type/{column}")).as_deref(),
            Some(says),
            "{column}"
        );
    }
    act(&mut app, "tables/sheet/preference", None);
    let ui = render(&app);
    assert_eq!(
        said(&ui, "tables/type/machine").as_deref(),
        Some("key \u{b7} \u{2192} Machines")
    );
    act(&mut app, "tables/sheet/account", None);
    let ui = render(&app);
    let services = format!("any of {}", aterm_db::alab::registry::SERVICES.len());
    for (column, says) in [
        ("services", services.as_str()),
        ("plan_since", "date"),
        ("provider", "\u{2192} Providers"),
        ("label", "text"),
    ] {
        assert_eq!(
            said(&ui, &format!("tables/type/{column}")).as_deref(),
            Some(says),
            "{column}"
        );
    }
    act(&mut app, "tables/sheet/provider", None);
    assert_eq!(
        said(&render(&app), "tables/type/~key").as_deref(),
        Some("key"),
        "a provider is named by its own key"
    );
}

/// An address row and an account that signs in with it, for the typed cells beyond an enum's.
fn with_account(lab: &Lab) -> (String, String) {
    let here = Registry::at(lab.paths[0].clone());
    let (ad, ac) = ("ad-0000000000000001", "ac-0000000000000001");
    here.write(vec![vec![
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
    repair_commons(lab, 4);
    (ad.to_owned(), ac.to_owned())
}

/// A set of words opens on its words, each member ticked: Space or a press puts the marked word
/// in or takes it out, Return stages the set and Escape leaves it. A bool switches to its other
/// value at Return or Space. A day reads, is typed and is staged as its date, in the grid, the
/// pane and the line under the grid alike, and is written as the day's number; a date that is
/// none is refused by the date rule as it is staged.
/// NEGATIVE CONTROL: with `grid::read` taking a day as a whole number only (its `date_day`
/// dropped), the date is refused: `` left: "2026-11-14, not written, refused: this column holds
/// a date, as 2026-11-14" right: "2026-11-14, not written" ``.
#[test]
fn a_set_a_switch_and_a_date_open_by_their_type() {
    let lab = two_macs();
    let (ad, ac) = with_account(&lab);
    let mut app = opened(&lab);
    act(&mut app, "tables/sheet/account", None);
    let services = format!("{CELL}{ac}/services");
    open(&mut app, &services);
    let ui = render(&app);
    let words = listed(&ui);
    assert_eq!(
        words.len(),
        aterm_db::alab::registry::SERVICES.len(),
        "{words:?}"
    );
    assert_eq!(words[0], "claude-code, not in the set, marked");
    // Space puts the marked word in; Down, Down and a press add another; Escape drops them.
    key_in(&mut app, TextInputEvent::Commit(" ".into()));
    press_key(&mut app, NamedKey::ArrowDown);
    press_key(&mut app, NamedKey::ArrowDown);
    act(&mut app, &format!("{CHOICE}2"), None);
    let ui = render(&app);
    assert_eq!(
        said(&ui, &format!("{CHOICE}0")).as_deref(),
        Some("claude-code, in the set")
    );
    assert_eq!(field_holds(&ui).as_deref(), Some("claude-code, codex"));
    press_key(&mut app, NamedKey::Escape);
    assert_eq!(cell_says(&render(&app), ac.as_str(), "services"), "empty");
    open(&mut app, &services);
    key_in(&mut app, TextInputEvent::Commit(" ".into()));
    press_key(&mut app, NamedKey::Enter);
    assert_eq!(
        cell_says(&render(&app), ac.as_str(), "services"),
        "claude-code, not written"
    );
    // A day: opened as its date, staged as its number, read back as its date.
    let since = format!("{CELL}{ac}/plan_since");
    open(&mut app, &since);
    act(
        &mut app,
        EDIT,
        Some(SemanticInput::Text("2026-11-14".into())),
    );
    key_in(&mut app, TextInputEvent::Submit);
    assert_eq!(
        cell_says(&render(&app), ac.as_str(), "plan_since"),
        "2026-11-14, not written"
    );
    let ui = render(&app);
    assert_eq!(
        said(&ui, "tables/pane/field/plan_since/value").as_deref(),
        Some("2026-11-14"),
        "the pane reads the day as its date"
    );
    assert!(
        said(&ui, "tables/said/words")
            .is_some_and(|line| line.starts_with("Plan since: 2026-11-14 set here, not written")),
        "{:?}",
        said(&ui, "tables/said/words")
    );
    open(&mut app, &since);
    assert_eq!(
        field_holds(&render(&app)).as_deref(),
        Some("2026-11-14"),
        "a staged day opens as its date"
    );
    act(
        &mut app,
        EDIT,
        Some(SemanticInput::Text("2026-02-30".into())),
    );
    key_in(&mut app, TextInputEvent::Submit);
    assert_eq!(
        cell_says(&render(&app), ac.as_str(), "plan_since"),
        "2026-02-30, not written, refused: this column holds a date, as 2026-11-14"
    );
    // Written, the registry holds the day's number, which reads back as its date.
    open(&mut app, &since);
    act(
        &mut app,
        EDIT,
        Some(SemanticInput::Text("2026-11-14".into())),
    );
    key_in(&mut app, TextInputEvent::Submit);
    act(&mut app, WRITE, None);
    app.native_runtime.tables_mut().unwrap().settle_write();
    let view = Handle::at(lab.paths[0].clone(), &REGISTRY).view().unwrap();
    assert_eq!(
        view.project(&ACCOUNT).rows[ac.as_str()].get("plan_since"),
        Some(&json!(20_771)),
        "the registry holds a day as its number"
    );
    assert_eq!(
        cell_says(&render(&app), ac.as_str(), "plan_since"),
        "2026-11-14"
    );
    // A bool: Return switches it, and Space back.
    act(&mut app, "tables/sheet/address", None);
    let protected = format!("{CELL}{ad}/protected");
    act(&mut app, &protected, None);
    press_key(&mut app, NamedKey::Enter);
    assert_eq!(
        cell_says(&render(&app), ad.as_str(), "protected"),
        "true, not written"
    );
    assert!(view_of(&app).editing.is_none(), "a switch opens no field");
    key_in(&mut app, TextInputEvent::Commit(" ".into()));
    assert_eq!(
        cell_says(&render(&app), ad.as_str(), "protected"),
        "false, not written"
    );
}

/// A day past what a four-digit year names, written by another build, reads as its number; a
/// year past any calendar, typed into a text cell, is the text, and into a day's is refused by
/// the date rule. None of them overflows the page's arithmetic.
/// NEGATIVE CONTROL: with `grid::day_date` taking any day (its bound at 9999-12-31 dropped), the
/// day reads as a year no calendar holds: `` left: "25252734927768524-07-27" right:
/// "9223372036854775807" ``.
#[test]
fn a_day_past_the_calendar_reads_as_its_number_and_a_huge_year_is_refused() {
    let lab = two_macs();
    let (_, ac) = with_account(&lab);
    Registry::at(lab.paths[0].clone())
        .write(vec![vec![put_in(
            &ACCOUNT,
            &ac,
            "plan_since",
            json!(i64::MAX),
        )]])
        .unwrap();
    let mut app = opened(&lab);
    let huge = "9000000000000000000-01-01";
    let nickname = format!("{CELL}{MC}/nickname");
    open(&mut app, &nickname);
    act(&mut app, EDIT, Some(SemanticInput::Text(huge.into())));
    key_in(&mut app, TextInputEvent::Submit);
    assert!(
        cell_says(&render(&app), MC, "nickname").starts_with(&format!("{huge}, not written")),
        "a text cell stages the text"
    );
    act(&mut app, "tables/sheet/account", None);
    assert_eq!(
        cell_says(&render(&app), &ac, "plan_since"),
        i64::MAX.to_string()
    );
    let since = format!("{CELL}{ac}/plan_since");
    open(&mut app, &since);
    act(&mut app, EDIT, Some(SemanticInput::Text(huge.into())));
    key_in(&mut app, TextInputEvent::Submit);
    assert_eq!(
        cell_says(&render(&app), &ac, "plan_since"),
        format!("{huge}, not written, refused: this column holds a date, as 2026-11-14")
    );
}

/// The column every switch test below runs on: a bool.
static SWITCH: aterm_db::typed::Column =
    aterm_db::typed::Column::new("nickname", aterm_db::typed::Ty::Bool);

/// A conflicted bool holds values the person has not chosen between: Return, Space and a press
/// on it switch nothing, and its values are kept from the pane.
/// NEGATIVE CONTROL: with `open_edit` switching a conflicted bool as any other (its conflict
/// check removed), Return stages `true` over both: `` left: "true, not written" right: "false /
/// true, 2 values" ``.
#[test]
fn a_conflicted_switch_switches_to_nothing_the_person_saw() {
    let conflict = Shown {
        text: "false / true".into(),
        values: vec![("false".into(), None), ("true".into(), None)],
        ..Shown::default()
    };
    let mut snapshot = one_sheet(conflict);
    snapshot.sheets[0].columns[0].compiled = Some(&SWITCH);
    let mut app = open_with(snapshot);
    let cell = format!("{CELL}{MC}/nickname");
    act(&mut app, &cell, None);
    press_key(&mut app, NamedKey::Enter);
    key_in(&mut app, TextInputEvent::Commit(" ".into()));
    act(&mut app, &cell, None);
    assert_eq!(
        cell_says(&render(&app), MC, "nickname"),
        "false / true, 2 values"
    );
    assert!(app.native_runtime.tables_mut().unwrap().staged.is_empty());
}

/// A word of an open list that holds the keys (an assistive technology moved focus to it) keys
/// as the open field does: Escape leaves the cell as it was, and Return stages the word marked.
/// NEGATIVE CONTROL: with the page's text keys finding no arm for a word's key (its mapping to
/// the open field removed), Escape leaves the list open: `` Escape closes the list ``.
#[test]
fn a_focused_word_takes_escape_and_return_as_the_open_field_does() {
    let lab = two_macs();
    let mut app = opened(&lab);
    let cell = format!("{CELL}{MC}/role");
    let focus = |app: &mut App, n: usize| {
        app.dispatch_native_event(
            WID,
            AppEvent::FocusChanged(Some(UiKey::new(format!("{CHOICE}{n}")))),
        )
        .unwrap();
    };
    open(&mut app, &cell);
    press_key(&mut app, NamedKey::ArrowDown);
    focus(&mut app, 0);
    assert!(
        view_of(&app).editing.is_some(),
        "a word's focus keeps it open"
    );
    press_key(&mut app, NamedKey::Escape);
    assert!(view_of(&app).editing.is_none(), "Escape closes the list");
    assert_eq!(cell_says(&render(&app), MC, "role"), "empty");
    open(&mut app, &cell);
    press_key(&mut app, NamedKey::ArrowDown);
    focus(&mut app, 0);
    press_key(&mut app, NamedKey::Enter);
    assert_eq!(cell_says(&render(&app), MC, "role"), "worker, not written");
}

/// Typed into a set's open field, a space is a space: `claude-code, codex` typed key by key is
/// the set of both. Space puts the marked word in or takes it out only while the field holds
/// what the list last wrote.
/// NEGATIVE CONTROL: with Space the list's whatever the field holds (`Choice::spaces` answering
/// `many` alone), the space after the comma takes `claude-code` out: `` left: Some("codex")
/// right: Some("claude-code, codex") ``.
#[test]
fn a_space_typed_into_a_set_is_typed() {
    let lab = two_macs();
    let (_, ac) = with_account(&lab);
    let mut app = opened(&lab);
    act(&mut app, "tables/sheet/account", None);
    open(&mut app, &format!("{CELL}{ac}/services"));
    for key in "claude-code, codex".chars() {
        key_in(&mut app, TextInputEvent::Commit(key.to_string()));
    }
    assert_eq!(
        field_holds(&render(&app)).as_deref(),
        Some("claude-code, codex")
    );
    press_key(&mut app, NamedKey::Enter);
    assert_eq!(
        cell_says(&render(&app), &ac, "services"),
        "claude-code, codex, not written"
    );
}

/// Space on a selected cell that opens a list opens it on what the cell holds, and Return then
/// stages nothing new: a set keeps its members, an enum its word.
/// NEGATIVE CONTROL: with Space and a typed letter on a selected cell typed as any text is (the
/// list's own arms removed), the set's field holds a space, not its members: `` the list opens
/// on the members the cell holds ``.
#[test]
fn space_on_a_selected_list_cell_opens_it_on_what_it_holds() {
    let lab = two_macs();
    let (_, ac) = with_account(&lab);
    let mut app = opened(&lab);
    act(&mut app, "tables/sheet/account", None);
    let services = format!("{CELL}{ac}/services");
    open(&mut app, &services);
    key_in(&mut app, TextInputEvent::Commit(" ".into()));
    press_key(&mut app, NamedKey::Enter);
    // Select it again without opening it: another cell first, then it.
    act(&mut app, &format!("{CELL}{ac}/status"), None);
    act(&mut app, &services, None);
    assert!(view_of(&app).editing.is_none(), "selected, not open");
    key_in(&mut app, TextInputEvent::Commit(" ".into()));
    assert!(
        listed(&render(&app))[0].starts_with("claude-code, in the set"),
        "the list opens on the members the cell holds"
    );
    press_key(&mut app, NamedKey::Enter);
    assert_eq!(
        cell_says(&render(&app), &ac, "services"),
        "claude-code, not written"
    );
    act(&mut app, "tables/sheet/machine", None);
    let role = format!("{CELL}{MC}/role");
    act(&mut app, &role, None);
    key_in(&mut app, TextInputEvent::Commit(" ".into()));
    press_key(&mut app, NamedKey::Enter);
    assert_eq!(cell_says(&render(&app), MC, "role"), "empty");
    // A typed word on a selected set's cell marks its word over the members it holds.
    act(&mut app, "tables/sheet/account", None);
    act(&mut app, &services, None);
    key_in(&mut app, TextInputEvent::Commit("c".into()));
    assert_eq!(field_holds(&render(&app)).as_deref(), Some("claude-code"));
}

/// Leaving an enum's open cell by Tab or by a press elsewhere stages the word its list marked,
/// as Return does: a typed `w` is `worker` however the cell closes.
/// NEGATIVE CONTROL: with `close_edit` staging the field's text over the list's mark (its
/// `Choice::marked` dropped), Tab stages the `w`: `` left: "w, not written, refused: pick one of
/// the column's own words" right: "worker, not written" ``.
#[test]
fn tab_and_a_press_elsewhere_stage_the_marked_word_as_return_does() {
    let lab = two_macs();
    let mut app = opened(&lab);
    let role = format!("{CELL}{MC}/role");
    open(&mut app, &role);
    act(&mut app, EDIT, Some(SemanticInput::Text("w".into())));
    app.dispatch_native_event(WID, AppEvent::StepAcross(1))
        .unwrap();
    assert_eq!(cell_says(&render(&app), MC, "role"), "worker, not written");
    open(&mut app, &role);
    act(&mut app, EDIT, Some(SemanticInput::Text("d".into())));
    act(&mut app, &format!("{CELL}{MC}/nickname"), None);
    assert_eq!(cell_says(&render(&app), MC, "role"), "desk, not written");
}

/// In a window too short for the list under its cell or over it, the list takes the side with
/// more room and shows the words that fit there, the marked one among them, each where a
/// pointer can press it; the wheel moves the mark through the rest.
/// NEGATIVE CONTROL: with `list` below its cell, every word of it, wherever it does not fit above
/// (its fitting to the room removed), only the first word is on the sheet: `` the marked word is
/// shown: [0] ``.
#[test]
fn an_open_list_in_a_short_window_shows_what_fits_where_a_press_finds_it() {
    let lab = two_macs();
    let mut app = opened(&lab);
    open(&mut app, &format!("{CELL}{MC}/role"));
    let (width, height) = (1400.0, 260.0);
    let ui = render_in(&app, width, height);
    let shown = (0..7)
        .filter(|n| said(&ui, &format!("{CHOICE}{n}")).is_some())
        .collect::<Vec<_>>();
    assert!(
        !shown.is_empty() && shown.len() < 7,
        "the window is too short for all seven: {shown:?}"
    );
    for n in &shown {
        let key = format!("{CHOICE}{n}");
        let rect = ui.semantic(&UiKey::new(key.clone())).unwrap().rect;
        let (x, y) = (rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
        assert!(y < height, "{key} stands in the window: {rect:?}");
        assert_eq!(
            ui.hit_test(x, y).map(|hit| hit.key.as_str()),
            Some(key.as_str()),
            "the word `{}` is pressable",
            said(&ui, &key).unwrap_or_default()
        );
    }
    assert!(shown.contains(&6), "the marked word is shown: {shown:?}");
    app.dispatch_native_event(
        WID,
        AppEvent::ScrollAt {
            lines: -1,
            key: UiKey::new(format!("{CHOICE}6")),
        },
    )
    .unwrap();
    assert_eq!(
        said(&render_in(&app, width, height), &format!("{CHOICE}5")).as_deref(),
        Some("spare, marked"),
        "the wheel moves the mark"
    );
}

/// The Write bar counts the cells the check refuses, and a Write holds back each table that
/// has one, whole, and writes the rest.
/// NEGATIVE CONTROL: with `write` sending every table (its `held` skip removed), the Machines
/// save is sent and refused whole, and the note gives the save's rule, not what was held back:
/// `` left: Some("Wrote 1 cell · ROLE: pick one of the column's own…") ``.
#[test]
fn a_write_holds_back_a_table_with_a_refused_cell_and_writes_the_rest() {
    let lab = two_macs();
    let (_, ac) = with_account(&lab);
    let mut app = opened(&lab);
    let role = format!("{CELL}{MC}/role");
    open(&mut app, &role);
    act(&mut app, EDIT, Some(SemanticInput::Text("server".into())));
    key_in(&mut app, TextInputEvent::Submit);
    let nickname = format!("{CELL}{MC}/nickname");
    open(&mut app, &nickname);
    act(&mut app, EDIT, Some(SemanticInput::Text("Spare 2".into())));
    key_in(&mut app, TextInputEvent::Submit);
    act(&mut app, "tables/sheet/account", None);
    open(&mut app, &format!("{CELL}{ac}/services"));
    key_in(&mut app, TextInputEvent::Commit(" ".into()));
    press_key(&mut app, NamedKey::Enter);
    assert_eq!(
        said(&render(&app), "tables/bar/count").as_deref(),
        Some("3 changes not written \u{b7} 1 refused")
    );
    act(&mut app, WRITE, None);
    app.native_runtime.tables_mut().unwrap().settle_write();
    let tables = app.native_runtime.tables_mut().unwrap();
    let staged: Vec<&str> = tables.staged.keys().map(|(_, _, c)| c.as_str()).collect();
    assert_eq!(
        staged,
        ["nickname", "role"],
        "the Machines cells stay staged"
    );
    assert_eq!(
        said(&render(&app), "tables/bar/note").as_deref(),
        Some("Wrote 1 cell \u{b7} Machines held back: 1 refused cell")
    );
    let view = Handle::at(lab.paths[0].clone(), &REGISTRY).view().unwrap();
    assert!(
        view.project(&aterm_db::alab::registry::MACHINE).rows[MC]
            .get("nickname")
            .is_none(),
        "nothing of the Machines table is written"
    );
}
