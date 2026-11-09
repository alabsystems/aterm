// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What aterm paints over a terminal reaches a client as a node of its own, since a terminal's
//! document reads the program's rows and none of aterm's.

use crate::scripted_redraw_fixture::{feed, scripted_app};
use crate::{App, WindowId};
use aterm_a11y::model::{Action, ActionRequest, Node, NodeId, Role, TreeId, TreeUpdate};

const WID: WindowId = WindowId(0);

fn sent(app: &App) -> &TreeUpdate {
    app.windows[&WID].a11y_sent.last().expect("a tree")
}

fn node_of(tree: &TreeUpdate, role: Role) -> Option<(NodeId, &Node)> {
    tree.nodes
        .iter()
        .find(|(_, node)| node.role() == role)
        .map(|(id, node)| (*id, node))
}

fn item_named(tree: &TreeUpdate, label: &str) -> NodeId {
    tree.nodes
        .iter()
        .find(|(_, node)| node.role() == Role::MenuItem && node.label() == Some(label))
        .unwrap_or_else(|| panic!("no row {label:?}"))
        .0
}

fn press(app: &mut App, node: NodeId) {
    app.on_accessibility_action(
        WID,
        ActionRequest {
            context: None,
            action: Action::Click,
            target_tree: TreeId::ROOT,
            target_node: node,
            data: None,
        },
    );
}

/// A window with a tab strip and a client, a frame presented.
fn listened_to() -> App {
    let mut app = scripted_app();
    app.tab_strip_rows = 1;
    app.windows.get_mut(&WID).unwrap().a11y_active = true;
    app.redraw_window(WID);
    app
}

#[test]
fn an_open_tab_menu_is_a_menu_a_client_can_read_and_press() {
    let mut app = listened_to();
    assert!(node_of(sent(&app), Role::Menu).is_none(), "CONTROL");

    assert!(app.open_tab_context_menu(WID, 0, 3, true));
    app.redraw_window(WID);

    let tree = sent(&app).clone();
    let (_, menu) = node_of(&tree, Role::Menu).expect("the menu");
    let entries = app.windows[&WID].tab_menu.as_ref().unwrap().entries.clone();
    let rule = crate::session_chrome::TabMenuEntry::Separator;
    let rows = entries.iter().filter(|entry| **entry != rule).count();
    let visible = crate::tab_menu::visible_entries(app.windows[&WID].tab_menu_rect.unwrap());
    assert_eq!(
        menu.children().len(),
        rows.min(visible),
        "every row but a rule"
    );
    let highlight = app.windows[&WID].tab_menu.as_ref().unwrap().highlight;
    let lit = crate::tab_menu::entry_text(&entries[highlight.expect("a keyboard opening")]);
    assert_eq!(
        tree.focus,
        item_named(&tree, lit),
        "the lit row holds focus"
    );

    let rename = item_named(&tree, "Rename Session\u{2026}");
    press(&mut app, rename);
    assert!(app.windows[&WID].tab_menu.is_none(), "the press acts");
    assert!(app.inline_rename_edit(WID).is_some(), "as the row says");
}

/// The keyboard is in the field, so a client's focus has to be too.
#[test]
fn a_session_field_holds_the_focus_and_reads_what_is_typed() {
    let mut app = listened_to();
    assert!(app.open_tab_context_menu(WID, 0, 3, true));
    app.redraw_window(WID);
    let rename = item_named(sent(&app), "Rename Session\u{2026}");
    press(&mut app, rename);
    let window = app.windows.get_mut(&WID).unwrap();
    let edit = window.rename_edit.as_mut().unwrap();
    edit.text = "build".to_owned();
    edit.cursor = 2;
    app.redraw_window(WID);

    let tree = sent(&app);
    let (field, node) = node_of(tree, Role::TextInput).expect("the field");
    assert_eq!(tree.focus, field);
    assert_eq!(node.label(), Some("Session name"));
    assert_eq!(node.value(), Some("build"));
    let caret = node.text_selection().expect("its caret").focus;
    assert_eq!(caret.character_index, 2);
}

#[test]
fn a_press_on_a_row_of_a_menu_that_changed_does_nothing() {
    let mut app = listened_to();
    assert!(app.open_tab_context_menu(WID, 0, 3, true));
    app.redraw_window(WID);
    let rename = item_named(sent(&app), "Rename Session\u{2026}");
    let window = app.windows.get_mut(&WID).unwrap();
    window.tab_menu.as_mut().unwrap().entries.reverse();

    press(&mut app, rename);

    assert!(app.windows[&WID].tab_menu.is_some(), "still open");
}

#[test]
fn a_row_that_acts_on_nothing_takes_no_press() {
    let mut app = listened_to();
    assert!(app.open_tab_context_menu(WID, 0, 3, true));
    app.redraw_window(WID);
    let menu = app.tab_grid_menu(WID).expect("the menu");
    let item = menu.items.iter().position(|item| item.disabled);
    let item = item.expect("the header says whose tab it is");

    app.choose_tab_menu_item_for_reader(WID, menu.epoch, item);

    assert!(app.windows[&WID].tab_menu.is_some(), "still open");
}

#[test]
fn a_paste_question_holds_the_lines_it_asks_about() {
    let mut app = listened_to();
    let lines: Vec<String> = (0..12).map(|n| format!("command {n}")).collect();
    app.paste_banner = Some(crate::paste_banner::PendingPaste::new(
        WID,
        Some(0),
        lines.join("\n"),
        crate::input::Source::Human,
        crate::input::PasteFraming::AtDrain,
    ));
    app.redraw_window(WID);

    let (_, ask) = node_of(sent(&app), Role::AlertDialog).expect("the question");
    let detail = ask.description().expect("its description");
    assert!(
        detail.ends_with("command 0\ncommand 1\ncommand 2\n+9 more lines"),
        "as the banner previews it: {detail}"
    );
}

/// A paste can be megabytes on one line, which no screen reader could speak.
#[test]
fn a_paste_question_stops_a_long_line() {
    let long = "x".repeat(5_000);
    let detail = crate::paste_banner::announcement_detail(&format!("{long}\nshort"));
    assert!(detail.len() < 1_000, "{} bytes", detail.len());
    assert!(detail.ends_with("\nshort"), "{detail}");
}

/// The paste is painted and answered first, so the close waits behind it: a client told that Enter
/// closes would deliver the paste.
#[test]
fn a_close_question_behind_a_paste_question_waits_its_turn() {
    let mut app = listened_to();
    let prompt = crate::quit_safety::confirm_prompt(false, true, true).expect("a busy close");
    app.close_banner = Some(crate::close_confirm::PendingClose::new(
        WID,
        prompt,
        crate::close_confirm::CloseReplay::Window(WID),
        crate::session_store::ExitActor::Human,
    ));
    app.paste_banner = Some(crate::paste_banner::PendingPaste::new(
        WID,
        Some(0),
        "one\ntwo\n".to_owned(),
        crate::input::Source::Human,
        crate::input::PasteFraming::AtDrain,
    ));
    let asked = |app: &App| -> Vec<String> {
        let nodes = sent(app).nodes.iter();
        nodes
            .filter(|(_, node)| node.role() == Role::AlertDialog)
            .filter_map(|(_, node)| node.description().map(str::to_owned))
            .collect()
    };

    app.redraw_window(WID);
    let heard = asked(&app);
    assert_eq!(heard.len(), 1, "{heard:?}");
    assert!(heard[0].contains("one\ntwo"), "{heard:?}");

    app.paste_banner = None;
    app.redraw_window(WID);
    let heard = asked(&app);
    assert_eq!(heard.len(), 1, "{heard:?}");
    assert!(heard[0].starts_with(prompt.body), "{heard:?}");
}

#[test]
fn a_close_question_says_what_the_answer_ends() {
    let mut app = listened_to();
    let prompt = crate::quit_safety::confirm_prompt(false, true, true).expect("a busy close");
    app.close_banner = Some(crate::close_confirm::PendingClose::new(
        WID,
        prompt,
        crate::close_confirm::CloseReplay::Window(WID),
        crate::session_store::ExitActor::Human,
    ));
    app.redraw_window(WID);

    let (_, ask) = node_of(sent(&app), Role::AlertDialog).expect("the question");
    let detail = ask.description().expect("its description");
    assert!(detail.starts_with(prompt.body), "{detail}");
    assert!(detail.ends_with("Esc cancels"), "{detail}");
}

#[test]
fn a_composition_reads_at_the_caret_of_the_pane_that_holds_it() {
    let mut app = scripted_app();
    feed(&app, b"prompt> ");
    let session = app.front_terminal(WID).unwrap().session;
    let (rows, cols) = (24, 80);
    let read = |app: &App, focused| {
        let document = app.terminal_document_for_test(WID, session, rows, cols, focused);
        document.expect("the document").snap
    };
    assert_eq!(read(&app, true).cursor, Some((0, 8)), "CONTROL");

    app.on_ime_preedit(WID, "\u{65e5}\u{672c}".to_owned(), None);

    let composing = read(&app, true);
    // One character per cell, as every wide character in a document reads.
    assert!(
        composing.value().starts_with("prompt> \u{65e5} \u{672c}"),
        "{}",
        composing.value()
    );
    assert_eq!(composing.cursor, Some((0, 12)), "past two wide characters");
    let elsewhere = read(&app, false);
    assert!(
        elsewhere.value().starts_with("prompt>\n"),
        "{}",
        elsewhere.value()
    );

    app.on_ime_preedit(WID, String::new(), None);
    assert!(
        read(&app, true).value().starts_with("prompt>\n"),
        "a cancelled one leaves none"
    );
}

/// A frame draws a composition's caret whatever the program did with its own.
#[test]
fn a_composition_keeps_its_caret_over_a_hidden_cursor() {
    let mut app = scripted_app();
    feed(&app, b"\x1b[?25l");
    let session = app.front_terminal(WID).unwrap().session;
    let caret = |app: &App| {
        let document = app.terminal_document_for_test(WID, session, 24, 80, true);
        document.expect("the document").snap.cursor
    };
    assert_eq!(caret(&app), None, "CONTROL");

    app.on_ime_preedit(WID, "\u{65e5}".to_owned(), None);

    assert_eq!(caret(&app), Some((0, 2)));
}

/// A frame draws no composition over rows scrolled back, where its caret is not.
#[test]
fn a_view_scrolled_back_reads_no_composition() {
    let mut app = scripted_app();
    for row in 0..30 {
        feed(&app, format!("row {row}\r\n").as_bytes());
    }
    // High enough to stay in the frame once scrolled: a caret below it draws nothing anyway.
    feed(&app, b"\x1b[5;1H");
    let session = app.front_terminal(WID).unwrap().session;
    let term = app.front_terminal(WID).unwrap().term.clone();
    crate::term_lock(&term).scroll_display(2);

    app.on_ime_preedit(WID, "\u{65e5}".to_owned(), None);

    let document = app
        .terminal_document_for_test(WID, session, 24, 80, true)
        .unwrap();
    assert!(
        !document.snap.value().contains('\u{65e5}'),
        "{}",
        document.snap.value()
    );
}

/// A tab with a page beside its terminal has the page's tree, and its menu hangs over that as it
/// does over a terminal alone. (A tab that is only a page has no menu.)
#[test]
fn a_tab_menu_beside_a_page_is_read_with_the_page() {
    let mut app = listened_to();
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    app.split_active_with_stub_terminal(WID, crate::tab_model::SplitAxis::Horizontal);
    app.redraw_window(WID);
    assert!(node_of(sent(&app), Role::Menu).is_none(), "CONTROL");

    let front = app.windows[&WID].tab_set.active_index().unwrap();
    assert!(app.open_tab_context_menu(WID, front, 3, true));
    app.redraw_window(WID);

    assert!(node_of(sent(&app), Role::Menu).is_some(), "the menu");
}

/// The field beside a page takes the keys from the page, so the client's focus goes with them.
#[test]
fn a_session_field_beside_a_page_holds_the_focus() {
    let mut app = listened_to();
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    app.split_active_with_stub_terminal(WID, crate::tab_model::SplitAxis::Horizontal);
    let tab = app.windows[&WID].tab_set.active().unwrap().id;
    assert!(app.begin_session_rename(WID, tab));
    app.redraw_window(WID);

    let tree = sent(&app);
    // By its label: a page wide enough to show its search field (Linux's 9 px cells make this
    // half window that wide) holds a text input of its own, and the page's nodes come ahead of
    // the chrome's.
    let (field, _) = tree
        .nodes
        .iter()
        .find(|(_, node)| node.role() == Role::TextInput && node.label() == Some("Session name"))
        .expect("the field");
    assert_eq!(tree.focus, *field);
}

#[test]
fn a_composition_in_the_find_field_reads_in_the_field() {
    let mut app = scripted_app();
    let mut search = crate::app_search::SearchState::default();
    search.query = "ab".to_owned();
    search.cursor = 1;
    app.windows.get_mut(&WID).unwrap().search = Some(search);

    app.on_ime_preedit(WID, "\u{65e5}\u{672c}".to_owned(), None);

    let find = app.grid_a11y_find(WID).expect("the field");
    assert_eq!(find.query, "a\u{65e5}\u{672c}b");
    assert_eq!(find.caret, 3, "after the composition");
    let session = app.front_terminal(WID).unwrap().session;
    let document = app
        .terminal_document_for_test(WID, session, 24, 80, true)
        .unwrap();
    assert!(
        !document.snap.value().contains('\u{65e5}'),
        "and only there"
    );
}
