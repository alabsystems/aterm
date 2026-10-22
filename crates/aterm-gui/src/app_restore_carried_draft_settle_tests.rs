// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use crate::restore::{
    NativeLeafRestore, RestoredSplitTree, RestoredTab, RestoredView, SettingsDraftRestore,
    WindowLayout, WindowShow,
};
use crate::{App, WindowId};

/// A window holding one Settings tab that carries `drafts`, as a seamless
/// successor's restore receives it.
fn settings_window(drafts: &[(&str, &str)]) -> WindowLayout {
    let mut leaf = NativeLeafRestore::settings(
        crate::native_settings::SettingsRoute::TextFonts
            .path()
            .to_string(),
    );
    leaf.settings_drafts = drafts
        .iter()
        .map(|(key, text)| SettingsDraftRestore {
            key: (*key).to_string(),
            text: (*text).to_string(),
        })
        .collect();
    WindowLayout {
        rows: 24,
        cols: 80,
        active_tab: 0,
        outer_x: None,
        outer_y: None,
        maximized: None,
        inner_px: None,
        presence_row: false,
        show: WindowShow::UNKNOWN,
        tabs: Vec::new(),
        native_tabs: Vec::new(),
        tab_order: Vec::new(),
        active_item: Some(0),
        restored_tabs: vec![RestoredTab {
            root: RestoredSplitTree::leaf(RestoredView::Native(leaf)),
            focused_path: Vec::new(),
            zoomed: false,
        }],
    }
}

/// Every live loss row for carried Settings drafts.
fn loss_rows(app: &App) -> Vec<aterm_messages::Message> {
    app.messages
        .live_rows()
        .filter(|row| row.msg.key.as_deref() == Some(crate::update_words::KEY_SETTINGS_DRAFTS))
        .map(|row| row.msg.clone())
        .collect()
}

/// Two Settings views, each losing a draft (a row the new build does not have): one row names
/// both, with their text, rather than the second replacing the first under the update's key.
#[test]
fn every_lost_draft_of_the_update_is_said_in_one_row() {
    let mut app = App::headless_for_test();
    app.restore_into_window(
        WindowId(0),
        settings_window(&[("no_such_row_a", "first words")]),
    );
    let second = app.create_native_restore_window(24, 80);
    app.restore_into_window(
        second,
        settings_window(&[("no_such_row_b", "second words")]),
    );
    assert!(
        loss_rows(&app).is_empty(),
        "nothing is said leaf by leaf: the restore is not done"
    );
    app.settle_carried_settings_drafts();
    let rows = loss_rows(&app);
    assert_eq!(rows.len(), 1, "one row for the update: {rows:?}");
    for said in ["no_such_row_a: first words", "no_such_row_b: second words"] {
        assert!(
            rows[0].detail.iter().any(|line| line == said),
            "{said} is on glass: {:?}",
            rows[0].detail
        );
    }
    app.settle_carried_settings_drafts();
    assert_eq!(loss_rows(&app).len(), 1, "a second settle says nothing new");
}

/// The loss row cannot hold the carried text (each line is cut to the band's width with
/// newlines and tabs gone, and drafts past the 16th are only counted), so every lost draft is
/// saved whole beside messages.log, and the row says where.
#[test]
fn every_lost_draft_is_saved_whole_and_the_row_says_where() {
    // Short: the row's line holds the path whole only under 240 characters,
    // whatever `$TMPDIR` is.
    let scratch = crate::test_scratch::short_dir("the saved drafts");
    let folder = scratch.path().to_path_buf();
    let long = format!("{}\n\tsecond line", "a".repeat(1000));
    let mut drafts = vec![("no_such_row_long".to_string(), long.clone())];
    drafts.extend((1..=17).map(|index| (format!("no_such_row_{index}"), format!("draft {index}"))));
    let borrowed: Vec<(&str, &str)> = drafts
        .iter()
        .map(|(key, text)| (key.as_str(), text.as_str()))
        .collect();
    let mut app = App::headless_for_test();
    app.messages_log =
        crate::messages_store::Writer::spawn(&folder.join(crate::messages_store::FILE_NAME));
    app.restore_into_window(WindowId(0), settings_window(&borrowed));
    app.settle_carried_settings_drafts();

    let saved: Vec<std::path::PathBuf> = std::fs::read_dir(&folder)
        .expect("the folder")
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("settings-drafts-"))
        })
        .collect();
    assert_eq!(
        saved.len(),
        1,
        "one file for the update's losses: {saved:?}"
    );
    let text = std::fs::read_to_string(&saved[0]).expect("the saved drafts");
    for (key, draft) in &drafts {
        assert!(text.contains(key.as_str()), "{key} is named");
        assert!(
            text.contains(draft.as_str()),
            "{key}'s text is whole, newlines kept"
        );
    }
    let rows = loss_rows(&app);
    assert_eq!(rows.len(), 1, "{rows:?}");
    let named = saved[0].display().to_string();
    assert!(
        rows[0].detail.iter().any(|line| line.contains(&named)),
        "the row says where the text is: {:?}",
        rows[0].detail
    );
    drop(app);
}

/// A seamless successor loses nothing before Commit: its restore settles the carried drafts
/// while the parent still holds them, so nothing is written or said until Commit, and then the
/// file and the row come together. Control: a restore with no handoff pending says it at once
/// (the test above).
#[cfg(unix)]
#[test]
fn a_successor_saves_and_says_its_lost_drafts_only_at_commit() {
    // Short: the row's line holds the path whole only under 240 characters,
    // whatever `$TMPDIR` is.
    let scratch = crate::test_scratch::short_dir("the saved drafts");
    let folder = scratch.path().to_path_buf();
    let saved_sets = |folder: &std::path::Path| -> Vec<std::path::PathBuf> {
        std::fs::read_dir(folder)
            .expect("the folder")
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("settings-drafts-"))
            })
            .collect()
    };
    let mut app = App::headless_for_test();
    app.messages_log =
        crate::messages_store::Writer::spawn(&folder.join(crate::messages_store::FILE_NAME));
    app.incoming_handoff_pending = true;
    app.restore_into_window(
        WindowId(0),
        settings_window(&[("no_such_row", "typed before the update")]),
    );
    app.settle_carried_settings_drafts();
    assert!(
        saved_sets(&folder).is_empty(),
        "before Commit the parent still holds the text: nothing is written"
    );
    assert!(loss_rows(&app).is_empty(), "…and nothing is said");

    // The Commit wake: the freeze lifts, and the loss is this process's now.
    app.incoming_handoff_pending = false;
    app.release_successor_commit_freeze();
    let saved = saved_sets(&folder);
    assert_eq!(saved.len(), 1, "saved at Commit: {saved:?}");
    let text = std::fs::read_to_string(&saved[0]).expect("the saved drafts");
    assert!(text.contains("typed before the update"), "{text}");
    let rows = loss_rows(&app);
    assert_eq!(rows.len(), 1, "said at Commit: {rows:?}");
    let named = saved[0].display().to_string();
    assert!(
        rows[0].detail.iter().any(|line| line.contains(&named)),
        "the row says where the text is: {:?}",
        rows[0].detail
    );
    app.release_successor_commit_freeze();
    assert_eq!(saved_sets(&folder).len(), 1, "said once");
    assert_eq!(loss_rows(&app).len(), 1);
    drop(app);
}

/// A draft that landed in a view the restore then closed (the native-only window arm's answer
/// to a window the OS would not attach: `close_window_logical`) is lost with the view and said
/// with its text. Control: the same view left standing holds its draft, and nothing is said.
#[test]
fn a_draft_whose_view_the_restore_closed_is_said_with_its_text() {
    let field = crate::prefs::EDIT_FONT_FAMILY;
    for closed in [false, true] {
        let mut app = App::headless_for_test();
        let torn_off = app.create_native_restore_window(24, 80);
        app.restore_into_window(torn_off, settings_window(&[(field, "Carried Mono")]));
        let (_, view) = app
            .active_native_view(torn_off)
            .expect("PRECONDITION: the Settings leaf reopened");
        assert!(
            matches!(
                app.native_runtime.view_state(view),
                Some(crate::native_app::AppViewState::Settings(state))
                    if state.field_inputs.get(field).map(|input| input.value())
                        == Some("Carried Mono")
            ),
            "PRECONDITION: the draft landed"
        );
        if closed {
            let _ = app.close_window_logical(torn_off);
        }
        app.settle_carried_settings_drafts();
        let rows = loss_rows(&app);
        if closed {
            assert_eq!(rows.len(), 1, "the lost draft is said: {rows:?}");
            assert_eq!(rows[0].detail[0], format!("{field}: Carried Mono"));
        } else {
            assert!(rows.is_empty(), "a draft its view still holds is not lost");
        }
    }
}

/// A draft for a row the new build shows as a slider is said, not taken: the slider can neither
/// show nor edit free text, and the next config snapshot would discard it (`replace_snapshot`
/// keeps only drafts `field_accepts_text_input` admits).
#[test]
fn a_draft_for_a_slider_row_is_said_not_taken() {
    let mut app = App::headless_for_test();
    // A numeric font size: the row is a slider in this build.
    app.native_config_service =
        crate::native_config_service::VersionedConfigService::new("font_px = 14\n".into())
            .expect("the config parses");
    let torn_off = app.create_native_restore_window(24, 80);
    app.restore_into_window(
        torn_off,
        settings_window(&[(crate::prefs::EDIT_FONT_PX, "17 px, maybe")]),
    );
    let (_, view) = app.active_native_view(torn_off).expect("reopened");
    let Some(crate::native_app::AppViewState::Settings(state)) =
        app.native_runtime.view_state(view)
    else {
        panic!("the Settings leaf reopened");
    };
    assert!(
        !state.field_inputs.contains_key(crate::prefs::EDIT_FONT_PX),
        "the slider row holds no text draft"
    );
    app.settle_carried_settings_drafts();
    let rows = loss_rows(&app);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(
        rows[0].detail[0],
        format!("{}: 17 px, maybe", crate::prefs::EDIT_FONT_PX)
    );
}

/// The drafts of a layout the successor could not place ride the ledger it starts with, and
/// are said, never dropped with the layout.
#[test]
fn an_unplaced_layouts_drafts_are_said_with_their_text() {
    let mut app = App::headless_for_test();
    app.carried_settings_drafts = super::CarriedDraftsLedger::unplaced((
        vec![SettingsDraftRestore {
            key: crate::prefs::EDIT_FONT_FAMILY.to_string(),
            text: "Unplaced Mono".to_string(),
        }],
        1,
    ));
    app.settle_carried_settings_drafts();
    let rows = loss_rows(&app);
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].detail[0],
        format!("{}: Unplaced Mono", crate::prefs::EDIT_FONT_FAMILY)
    );
    assert!(
        rows[0]
            .detail
            .get(1)
            .is_some_and(|line| line.starts_with("1 ")),
        "the unreadable draft is counted too: {:?}",
        rows[0].detail
    );
}
