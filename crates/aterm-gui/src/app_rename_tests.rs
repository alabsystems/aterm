// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::session_timeline::MetaWriteError;

use crate::session_pin_for_test as pin;

fn meta_events(app: &App, session: u64) -> Vec<String> {
    let ctx = app.pool.get(session).expect("session").ctx.clone();
    let tl = ctx.timeline.lock().unwrap();
    tl.since(None)
        .filter(|e| e.kind == "meta-change")
        .map(|e| e.payload.to_string())
        .collect()
}

#[test]
fn a_commit_pins_the_title_and_closes_the_editor() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    assert!(app.begin_active_session_rename(wid));
    assert_eq!(app.rename_edit_session(wid), Some(0));

    app.commit_session_rename(wid, 0, "build agent");
    assert_eq!(pin(&app, 0).as_deref(), Some("build agent"));
    assert_eq!(app.tab_titles(wid)[0], "build agent");
    assert_eq!(app.rename_edit_session(wid), None, "the editor closed");
    assert_eq!(
        meta_events(&app, 0),
        vec!["field=title value=build%20agent"]
    );
}

#[test]
fn opening_another_rename_commits_the_one_it_replaces() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    let first = app.windows[&wid].tab_set.tabs()[0].id;
    assert!(app.begin_session_rename(wid, first));
    app.rename_field_edit(wid, crate::app_search::SearchEdit::Insert("x".to_string()));

    app.push_stub_tab(wid, crate::stub_session(1));
    let second = app.windows[&wid].tab_set.tabs()[1].id;
    assert!(app.begin_session_rename(wid, second));
    assert_eq!(
        pin(&app, 0).as_deref(),
        Some("x"),
        "the replaced edit was committed, not dropped"
    );
    assert_eq!(
        app.rename_edit_session(wid),
        Some(1),
        "the new edit is live on the second tab"
    );
}

#[test]
fn a_cancel_writes_nothing_and_leaves_an_existing_pin_alone() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    assert!(app.begin_active_session_rename(wid));
    app.commit_session_rename(wid, 0, "first");
    assert_eq!(pin(&app, 0).as_deref(), Some("first"));

    assert!(app.begin_active_session_rename(wid));
    app.cancel_session_rename(wid, 0);
    assert_eq!(
        pin(&app, 0).as_deref(),
        Some("first"),
        "cancel wrote nothing"
    );
    assert_eq!(app.rename_edit_session(wid), None);
    assert_eq!(meta_events(&app, 0).len(), 1, "cancel recorded nothing");
}

#[test]
fn an_empty_commit_clears_the_pin_and_the_ladder_falls_back() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    crate::term_lock(&app.pool.get(0).expect("session 0").term)
        .process(b"\x1b]2;vim src/main.rs\x07");
    assert!(app.begin_active_session_rename(wid));
    app.commit_session_rename(wid, 0, "build agent");
    assert_eq!(app.tab_titles(wid)[0], "build agent");

    assert!(app.begin_active_session_rename(wid));
    app.commit_session_rename(wid, 0, "   ");
    assert_eq!(pin(&app, 0), None, "whitespace-only commits CLEAR the pin");
    assert_eq!(
        app.tab_titles(wid)[0],
        "vim src/main.rs",
        "the ladder falls back to the OSC title"
    );
    assert_eq!(
        meta_events(&app, 0),
        vec!["field=title value=build%20agent", "field=title value=-"],
    );
}

#[test]
fn a_close_during_the_edit_drops_the_commit_instead_of_retargeting() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    let tab = app.windows[&wid].tab_set.active_id().expect("a tab");
    assert!(app.begin_session_rename(wid, tab));

    app.cancel_session_rename(wid, 0);
    app.commit_session_rename(wid, 0, "ghost");
    assert_eq!(
        pin(&app, 0),
        None,
        "a commit with no live edit writes nothing"
    );
    assert!(meta_events(&app, 0).is_empty());

    app.windows.get_mut(&wid).unwrap().rename_edit = None;
    let gone = crate::tab_model::TabId::from_stored(u64::MAX);
    assert!(!app.begin_session_rename(wid, gone));
    assert_eq!(app.rename_edit_session(wid), None);
}

#[test]
fn a_menu_command_ends_the_edit_before_it_runs_and_rename_stays_idempotent() {
    use crate::menu::MenuAction;
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    assert!(app.begin_active_session_rename(wid));

    assert!(
        !app.divert_menu_action_around_rename(MenuAction::RenameSession),
        "the rename command is never diverted away from itself"
    );
    assert_eq!(
        app.rename_edit_session(wid),
        Some(0),
        "re-issuing Rename Session leaves the open editor alone"
    );

    assert!(
        !app.divert_menu_action_around_rename(MenuAction::CloseTab),
        "a structural command still runs — after the edit is settled"
    );
    assert_eq!(
        app.rename_edit_session(wid),
        None,
        "the editor is gone before Close Tab acts on the tab it was over"
    );
}

#[test]
fn an_over_cap_pin_is_refused_and_stores_nothing() {
    let mut app = App::headless_for_test();
    let over = "x".repeat(crate::session_timeline::META_TITLE_MAX + 1);
    assert_eq!(
        app.write_session_title_pin(0, &over),
        Err(MetaWriteError::TooLong {
            cap: crate::session_timeline::META_TITLE_MAX
        })
    );
    assert_eq!(pin(&app, 0), None);
    assert!(meta_events(&app, 0).is_empty());
}

#[test]
fn a_surface_less_window_refuses_the_edit_instead_of_storing_a_phantom() {
    use crate::menu::MenuAction;
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert_eq!(app.tab_strip_rows, 0, "negative control: no strip");
    assert!(
        !app.can_rename_session(wid),
        "the menu/palette capability agrees with the write path"
    );

    assert!(
        !app.begin_active_session_rename(wid),
        "nothing can present this edit, so it is refused"
    );
    assert_eq!(app.rename_edit_session(wid), None, "and nothing is stored");

    assert!(!app.divert_menu_action_around_rename(MenuAction::CloseTab));

    app.tab_strip_rows = 1;
    assert!(app.can_rename_session(wid), "a visible strip is a surface");
    assert!(app.begin_active_session_rename(wid));
    assert_eq!(app.rename_edit_session(wid), Some(0));
    assert!(
        app.inline_rename_edit(wid).is_some(),
        "the in-grid strip is what presents it"
    );
}

#[test]
fn the_input_seam_drives_the_in_grid_field_and_commits_it() {
    use crate::input::InputEvent;
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    assert!(app.begin_active_session_rename(wid));

    assert!(app.rename_input_event(wid, &InputEvent::Text("build agent".into())));
    assert_eq!(
        app.inline_rename_edit(wid).map(|edit| edit.text.as_str()),
        Some("build agent")
    );
    let key = |named: NamedKey| InputEvent::Key {
        key: Key::Named(named),
        mods: Modifiers::empty(),
        event_type: KeyEventType::Press,
        base_layout: None,
        layout_text: None,
    };
    assert!(app.rename_input_event(wid, &key(NamedKey::ArrowLeft)));
    assert!(app.rename_input_event(wid, &key(NamedKey::Backspace)));
    assert_eq!(
        app.inline_rename_edit(wid).map(|edit| edit.text.as_str()),
        Some("build aget")
    );

    assert!(app.rename_input_event(wid, &key(NamedKey::Enter)));
    assert_eq!(pin(&app, 0).as_deref(), Some("build aget"));
    assert_eq!(app.rename_edit_session(wid), None, "the editor closed");
}

#[test]
fn the_input_seam_cancels_on_escape_and_writes_nothing() {
    use crate::input::InputEvent;
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    assert!(app.begin_active_session_rename(wid));
    assert!(app.rename_input_event(wid, &InputEvent::Text("discard me".into())));
    assert!(app.rename_input_event(
        wid,
        &InputEvent::Key {
            key: Key::Named(NamedKey::Escape),
            mods: Modifiers::empty(),
            event_type: KeyEventType::Press,
            base_layout: None,
            layout_text: None,
        }
    ));
    assert_eq!(app.rename_edit_session(wid), None);
    assert_eq!(pin(&app, 0), None);
    assert!(meta_events(&app, 0).is_empty());
}

#[test]
fn the_in_grid_field_holds_only_storable_values() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    assert!(app.begin_active_session_rename(wid));

    app.rename_field_edit(wid, SearchEdit::Insert("build ".into()));
    assert_eq!(
        app.inline_rename_edit(wid).map(|edit| edit.text.as_str()),
        Some("build "),
        "a trailing space mid-typing is kept — it is about to become a word"
    );
    app.rename_field_edit(
        wid,
        SearchEdit::Insert("x".repeat(crate::session_timeline::META_TITLE_MAX + 10)),
    );
    let text = app
        .inline_rename_edit(wid)
        .map(|edit| edit.text.clone())
        .expect("still editing");
    assert_eq!(text.len(), crate::session_timeline::META_TITLE_MAX);

    let session = app.rename_edit_session(wid).expect("editing");
    app.commit_session_rename(wid, session, &text);
    assert_eq!(
        pin(&app, 0).as_deref(),
        Some(text.as_str()),
        "the field never held a value the store would refuse"
    );
}

#[test]
fn a_keystroke_in_the_field_moves_the_strip_fingerprint() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    let fingerprint = |app: &mut App| {
        let titles = app.tab_titles(wid);
        let metadata = app.tab_strip_metadata(wid);
        app.tab_strip_fingerprint_from_parts(wid, &titles, &metadata)
    };
    let idle = fingerprint(&mut app);
    assert!(app.begin_active_session_rename(wid));
    let opened = fingerprint(&mut app);
    assert_ne!(idle, opened, "opening the editor repaints the strip");

    app.rename_field_edit(wid, SearchEdit::Insert("a".into()));
    let typed = fingerprint(&mut app);
    assert_ne!(opened, typed, "a typed character repaints the strip");

    app.rename_field_edit(wid, SearchEdit::MoveStart);
    assert_ne!(typed, fingerprint(&mut app), "the caret is painted state");
}
