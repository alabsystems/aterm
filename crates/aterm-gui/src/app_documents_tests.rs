// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::fs;

use super::*;
use crate::input::InputEvent;
use crate::native_editor::Minibuffer;
use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};

const REFUSES_WITHOUT_BLOCKING: std::time::Duration = aterm_spec::wait::MUST_HAPPEN;

#[test]
fn encodes_to_len_agrees_with_encode_for_disk() {
    use crate::native_document_io::{DiskEncoding, LineEnding};
    for text in ["", "one", "one\ntwo\n", "\n\n\n", "a\r\nb\n"] {
        for bom in [false, true] {
            for eol in [LineEnding::Lf, LineEnding::CrLf] {
                let encoding = DiskEncoding { bom, eol };
                let len = crate::native_document_host::encode_for_disk(text, encoding).len() as u64;
                assert!(encodes_to_len(text, encoding, len), "{text:?} {encoding:?}");
                assert!(
                    !encodes_to_len(text, encoding, len + 1),
                    "{text:?} {encoding:?}"
                );
                assert!(
                    len == 0 || !encodes_to_len(text, encoding, len - 1),
                    "{text:?} {encoding:?}"
                );
            }
        }
    }
}

#[test]
fn the_journal_store_opens_on_a_worker_the_boot_does_not_wait_for() {
    let root = std::env::temp_dir().join(format!(
        "aterm-journal-opening-{}-{}",
        std::process::id(),
        aterm_time::since_epoch().unwrap().as_nanos()
    ));
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let opener_root = root.clone();
    let mut slot = JournalSlot::opening(move || {
        release_rx
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .map_err(|_| "the opener was never released".to_string())?;
        DocumentJournalStore::for_test(opener_root)
    });
    assert!(
        !root.join(".journal-key").exists(),
        "nothing is opened before the held opener runs"
    );
    release_tx.send(()).unwrap();
    assert!(
        slot.as_mut().is_some(),
        "the first look settles to the store"
    );
    assert!(
        root.join(".journal-key").exists(),
        "the opener minted the secret"
    );
    assert_eq!(slot.unavailable(), None);
    fs::remove_dir_all(&root).ok();
}

#[test]
fn a_journal_store_that_could_not_open_says_why() {
    let mut slot = JournalSlot::opening(|| Err("no private state directory".to_string()));
    assert_eq!(slot.unavailable(), Some("no private state directory"));
    assert!(slot.as_ref().is_none());
    assert_eq!(
        slot.available_mut().err().as_deref(),
        Some("no private state directory")
    );
}

#[test]
fn native_document_queue_admission_conforms_to_the_derived_model() {
    let model = aterm_models_gui::native_document_queue_model();
    let mut state = model.init_state();
    let mut documents = crate::test_document_store();
    let uri = "file:///queue-retry.md";
    let document = documents.open(uri.to_string(), "draft".to_string());
    let disk = documents.snapshot(document).unwrap();
    let journal_root = std::env::temp_dir().join(format!(
        "aterm-document-queue-retry-{}-{}",
        std::process::id(),
        aterm_time::since_epoch().unwrap().as_nanos()
    ));
    let mut journals = DocumentJournalStore::for_test(journal_root.clone()).unwrap();
    let decision = journals.inspect_open(uri, disk.text.as_bytes()).unwrap();
    journals.initialize(decision, &disk, &disk).unwrap();
    let _ = documents.transact(
        document,
        disk.seq,
        vec![crate::document_store::TextEdit {
            range: disk.text.len()..disk.text.len(),
            insert: " newer".to_string(),
        }],
    );
    let desired = documents.snapshot(document).unwrap();
    journals.observe_commit(&desired).unwrap();
    let JournalEffect::Append {
        plan: first_plan, ..
    } = journals.next_effect(document).unwrap().unwrap()
    else {
        panic!("journal append fixture")
    };
    let mut retries = JournalRetrySet::default();
    let (sender, receiver) = std::sync::mpsc::sync_channel::<u8>(NATIVE_DOCUMENT_QUEUE_CAPACITY);

    try_enqueue_document_job(&sender, 0).expect("first capacity slot");
    assert!(model.fire("SubmitAccepted", &mut state));
    assert_eq!(receiver.recv().unwrap(), 0);
    assert!(matches!(
        native_document_queue_slot_available_wake(),
        crate::Wake::NativeDocumentQueueSlotAvailable
    ));
    assert!(model.fire("WorkerStarts", &mut state));
    assert!(model.fire("ConsumeDrainEdge", &mut state));

    for value in 1..=NATIVE_DOCUMENT_QUEUE_CAPACITY as u8 {
        assert!(model.action_enabled("SubmitAccepted", &state));
        try_enqueue_document_job(&sender, value).expect("capacity slot");
        assert!(model.fire("SubmitAccepted", &mut state));
    }
    assert!(!model.action_enabled("SubmitAccepted", &state));
    assert!(model.action_enabled("SubmitDeferred", &state));
    assert_eq!(
        try_enqueue_document_journal_job(
            &sender,
            u8::MAX,
            document,
            JournalAdmissionEffect::Append(first_plan.generation),
            &mut journals,
            &mut retries,
        )
        .unwrap(),
        JournalDriveOutcome::Deferred
    );
    assert!(retries.contains(document));
    assert!(model.fire("SubmitDeferred", &mut state));
    assert_eq!(state["queued"], 4);
    assert_eq!(state["executing"], 1);
    assert_eq!(state["retry_pending"], 1);
    assert!(model.check_invariant("BoundedFullImageJobs", &state));
    assert!(model.check_invariant("DeferredIntentRetained", &state));

    assert!(model.fire("WorkerCompletes", &mut state));
    assert_eq!(receiver.recv().unwrap(), 1);
    assert!(model.fire("WorkerStarts", &mut state));
    let JournalEffect::Append {
        plan: retry_plan, ..
    } = journals.next_effect(document).unwrap().unwrap()
    else {
        panic!("capacity release must replan without another edit")
    };
    assert_ne!(retry_plan.generation, first_plan.generation);
    assert_eq!(retry_plan.target_seq, desired.seq);
    assert_eq!(
        try_enqueue_document_journal_job(
            &sender,
            42,
            document,
            JournalAdmissionEffect::Append(retry_plan.generation),
            &mut journals,
            &mut retries,
        )
        .unwrap(),
        JournalDriveOutcome::Enqueued
    );
    assert!(!retries.contains(document));
    assert!(model.fire("RetryAccepted", &mut state));
    assert_eq!(state["queued"], 4);
    assert_eq!(state["executing"], 1);
    assert_eq!(state["retry_pending"], 0);
    assert!(model.check_invariant("BoundedFullImageJobs", &state));
    assert!(model.check_invariant("OpenSlotHasRetryEdge", &state));

    let (unbounded, retained) = std::sync::mpsc::channel();
    unbounded.send(0).unwrap();
    assert_eq!(retained.recv().unwrap(), 0, "one image is executing");
    for value in 1..=NATIVE_DOCUMENT_QUEUE_CAPACITY + 1 {
        unbounded
            .send(value)
            .expect("retired channel never refuses");
    }
    assert!(
        !model.action_enabled("SubmitAccepted", &state),
        "the bounded model refuses where the retired channel accepted"
    );
    let _ = fs::remove_dir_all(journal_root);
}

fn fresh_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn open_config(
    name: &str,
    source: &str,
) -> (
    std::path::PathBuf,
    std::path::PathBuf,
    App,
    (crate::tab_model::AppInstanceId, crate::tab_model::ViewId),
) {
    let dir = fresh_dir(name);
    let path = dir.join("aterm.toml");
    crate::test_scratch::write_config(&path, source);
    let mut app = App::headless_for_test();
    app.ensure_and_open_config_editor_path_in_window(WindowId(0), &path)
        .unwrap();
    let views = app.active_native_view(WindowId(0)).unwrap();
    (dir, path, app, views)
}

fn commit(app: &mut App, wid: WindowId, text: &str) {
    app.dispatch_native_event(
        wid,
        AppEvent::TextInput(TextInputEvent::Commit(text.to_string())),
    )
    .unwrap();
}

#[test]
fn file_save_writes_only_the_active_edited_document() {
    let dir = fresh_dir("aterm-file-save-active-document");
    let first = dir.join("first.txt");
    let second = dir.join("second.txt");
    fs::write(&first, "first").unwrap();
    fs::write(&second, "second").unwrap();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.open_document_tab(AppKind::Editor, &file_uri(&first))
        .unwrap();
    commit(&mut app, wid, "draft ");
    let (first_instance, _) = app.active_native_view(wid).unwrap();
    let first_tab = app.windows[&wid].tab_set.active_index().unwrap();
    let first_document = app.native_runtime.document_id(first_instance).unwrap();
    app.open_document_tab(AppKind::Editor, &file_uri(&second))
        .unwrap();
    commit(&mut app, wid, "saved ");
    let (instance, _) = app.active_native_view(wid).unwrap();
    let second_tab = app.windows[&wid].tab_set.active_index().unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();
    let desired = app.document_store.snapshot(document).unwrap().text;
    let palette = app.palette_snapshot(wid);
    assert_eq!(
        palette.action_by_name("Save"),
        Ok(crate::menu::MenuAction::Save)
    );
    assert_eq!(
        palette
            .rows()
            .iter()
            .filter(|row| {
                matches!(
                    &row.action,
                    crate::palette::PaletteTarget::Menu(crate::menu::MenuAction::Save)
                ) || matches!(&row.action, crate::palette::PaletteTarget::Native(target)
                    if target.action.as_str() == "editor/save")
            })
            .count(),
        1,
        "one Save command in the editor palette"
    );
    assert!(app.can_save_document(wid));

    let save = palette
        .rows()
        .iter()
        .find_map(|row| match &row.action {
            crate::palette::PaletteTarget::Native(target)
                if target.action.as_str() == "editor/save" =>
            {
                Some(target.clone())
            }
            _ => None,
        })
        .expect("the menu-labelled Save carries the exact editor target");
    app.switch_tab_in(wid, first_tab);
    assert!(app.activate_native_palette_target(&save).is_err());
    assert_eq!(fs::read_to_string(&first).unwrap(), "first");
    assert_eq!(fs::read_to_string(&second).unwrap(), "second");
    assert_eq!(app.document_store.dirty(first_document), Some(true));
    assert_eq!(app.document_store.dirty(document), Some(true));
    app.switch_tab_in(wid, 0);
    assert!(app.activate_native_palette_target(&save).is_err());
    assert_eq!(fs::read_to_string(&first).unwrap(), "first");
    assert_eq!(fs::read_to_string(&second).unwrap(), "second");

    app.switch_tab_in(wid, second_tab);
    app.palette_enter();
    for c in "Save".chars() {
        app.palette_filter_push(c);
    }
    assert_eq!(
        app.windows[&wid]
            .palette()
            .and_then(crate::palette::PaletteState::selected_target),
        Some(crate::palette::PaletteTarget::Native(save)),
    );
    app.palette_activate();

    assert_eq!(fs::read_to_string(&second).unwrap(), desired.as_ref());
    assert_eq!(fs::read_to_string(&first).unwrap(), "first");
    assert_eq!(app.document_store.dirty(first_document), Some(true));
    assert_eq!(app.document_store.dirty(document), Some(false));
    assert!(!app.can_save_document(wid));
    assert!(app.palette_snapshot(wid).action_by_name("Save").is_err());
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn file_save_waits_for_editor_prompts_without_losing_the_draft() {
    use amacs::input::EditorInput;

    fn assert_save_available(app: &App, wid: WindowId, expected: bool) {
        assert_eq!(app.can_save_document(wid), expected);
        app.publish_menu_live();
        assert_eq!(crate::menu::menu_live().can_save, expected);
        assert_eq!(
            app.palette_snapshot(wid).action_by_name("Save").is_ok(),
            expected
        );
        let (instance, view) = app.active_native_view(wid).unwrap();
        let commands = app.native_runtime.commands(instance, view).unwrap();
        assert_eq!(
            commands
                .iter()
                .find(|command| command.id.as_str() == "editor/save")
                .unwrap()
                .enabled,
            expected
        );
        let compiled = app.compiled_native_ui(wid).unwrap();
        assert_eq!(
            compiled
                .semantic(&crate::native_ui::UiKey::new("editor/save-button"))
                .unwrap()
                .state
                .unwrap()
                .enabled,
            expected
        );
    }

    let scratch = aterm_tempfile::TempDir::new().unwrap();
    let path = scratch.path().join("prompts.txt");
    let source = "first needle\nsecond needle\n";
    fs::write(&path, source).unwrap();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.open_document_tab(AppKind::Editor, &file_uri(&path))
        .unwrap();
    let (instance, view) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();
    assert_save_available(&app, wid, false);
    commit(&mut app, wid, "draft ");
    let draft = app.document_store.snapshot(document).unwrap();
    assert_save_available(&app, wid, true);

    let prompt_commands = [
        ("editor/goto-line-button", "editor/goto-line"),
        ("editor/commands-button", "editor/commands"),
    ];
    let mut capability_failures = Vec::new();
    for command in [
        EditorCommand::GotoLine,
        EditorCommand::ExecuteCommand,
        EditorCommand::SwitchBuffer,
        EditorCommand::KillBuffer,
        EditorCommand::IncrementalSearch,
    ] {
        app.dispatch_native_event(wid, AppEvent::EditorCommand(command))
            .unwrap();
        app.dispatch_native_event(wid, AppEvent::EditorInput(EditorInput::Text("2".into())))
            .unwrap();
        let prompt = editor_buffer(&app, view).minibuffer.clone();
        assert!(matches!(
            (command, &prompt),
            (EditorCommand::GotoLine, Minibuffer::GotoLine { .. })
                | (EditorCommand::ExecuteCommand, Minibuffer::Command { .. })
                | (EditorCommand::SwitchBuffer, Minibuffer::Buffer { .. })
                | (EditorCommand::KillBuffer, Minibuffer::Prompt { .. })
                | (EditorCommand::IncrementalSearch, Minibuffer::Search { .. })
        ));
        assert_save_available(&app, wid, false);
        assert!(app.save_front_document().is_err());
        for (key, action) in prompt_commands {
            let commands = app.native_runtime.commands(instance, view).unwrap();
            let enabled = commands
                .iter()
                .find(|candidate| candidate.id.as_str() == action)
                .unwrap()
                .enabled;
            let compiled = app.compiled_native_ui(wid).unwrap();
            let toolbar_enabled = compiled
                .semantic(&crate::native_ui::UiKey::new(key))
                .unwrap()
                .state
                .unwrap()
                .enabled;
            if enabled || toolbar_enabled {
                capability_failures.push(format!(
                    "{command:?}: {action} enabled in command={enabled}, toolbar={toolbar_enabled}"
                ));
            }
            let before = editor_buffer(&app, view).clone();
            if app
                .act_app(crate::app_control::ActRequest {
                    view,
                    ui_key: key.to_string(),
                    action: action.to_string(),
                    value: None,
                })
                .is_ok()
            {
                capability_failures.push(format!("{command:?}: {action} act did not refuse"));
            }
            assert_eq!(editor_buffer(&app, view), &before);
            if app
                .activate_native_palette_target(&crate::palette::NativeCommandTarget {
                    window: wid,
                    instance,
                    view,
                    generation: app.native_runtime.view_generation(view).unwrap(),
                    action: crate::native_ui::ActionId::new(action),
                })
                .is_ok()
            {
                capability_failures.push(format!("{command:?}: {action} palette did not refuse"));
            }
            assert_eq!(editor_buffer(&app, view), &before);
        }
        assert_eq!(editor_buffer(&app, view).minibuffer, prompt);
        let after_refusal = app.document_store.snapshot(document).unwrap();
        assert_eq!(after_refusal.seq, draft.seq);
        assert_eq!(after_refusal.text, draft.text);
        assert_eq!(fs::read_to_string(&path).unwrap(), source);
        let exit = if command == EditorCommand::GotoLine {
            AppEvent::EditorInput(EditorInput::Key {
                key: Key::Named(NamedKey::Enter),
                mods: Modifiers::empty(),
                text: None,
            })
        } else {
            AppEvent::EditorCommand(EditorCommand::Abort)
        };
        app.dispatch_native_event(wid, exit).unwrap();
        assert!(matches!(
            editor_buffer(&app, view).minibuffer,
            Minibuffer::Inactive
        ));
        assert_save_available(&app, wid, true);
        let commands = app.native_runtime.commands(instance, view).unwrap();
        let compiled = app.compiled_native_ui(wid).unwrap();
        for (key, action) in prompt_commands {
            assert!(
                commands
                    .iter()
                    .find(|candidate| candidate.id.as_str() == action)
                    .unwrap()
                    .enabled
            );
            assert!(
                compiled
                    .semantic(&crate::native_ui::UiKey::new(key))
                    .unwrap()
                    .state
                    .unwrap()
                    .enabled
            );
        }
    }

    app.save_front_document().unwrap();
    assert_save_available(&app, wid, false);
    assert_eq!(app.document_store.dirty(document), Some(false));
    assert_eq!(fs::read_to_string(&path).unwrap(), draft.text.as_ref());
    let saved = app.document_store.snapshot(document).unwrap();
    assert_eq!(saved.seq, draft.seq);
    assert_eq!(saved.text, draft.text);
    assert!(capability_failures.is_empty(), "{capability_failures:#?}");
}

#[test]
fn markdown_reopen_keeps_save_available_and_terminal_has_no_document_save() {
    let dir = fresh_dir("aterm-file-save-non-editor");
    let path = dir.join("reader.md");
    fs::write(&path, "# Original\n").unwrap();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.open_document_tab(AppKind::Editor, &file_uri(&path))
        .unwrap();
    commit(&mut app, wid, "draft ");
    app.open_document_tab(AppKind::Markdown, &file_uri(&path))
        .unwrap();
    assert!(app.can_save_document(wid));
    assert!(app.palette_snapshot(wid).action_by_name("Save").is_ok());
    app.save_front_document().unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "draft # Original\n");

    app.switch_tab_in(wid, 0);
    assert!(app.active_native_view(wid).is_none());
    assert!(!app.can_save_document(wid));
    assert!(app.palette_snapshot(wid).action_by_name("Save").is_err());
    assert!(app.save_front_document().is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), "draft # Original\n");
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn find_navigation_uses_the_active_editor_query_after_enter_and_wraps() {
    let dir = fresh_dir("aterm-editor-find-navigation");
    let path = dir.join("first.txt");
    let source = "α needle β needle γ needle";
    fs::write(&path, source).unwrap();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.search_last_query = "terminal-only".to_string();
    app.open_document_tab(AppKind::Editor, &file_uri(&path))
        .unwrap();
    let (instance, view) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();
    let original = app.document_store.snapshot(document).unwrap();
    let matches: Vec<_> = source
        .match_indices("needle")
        .map(|(at, query)| at..at + query.len())
        .collect();
    assert!(app.search_find_again(true).is_err());
    assert_eq!(editor_buffer(&app, view).primary_selection().range(), 0..0);

    app.find_requested();
    commit(&mut app, wid, "needle");
    assert_eq!(
        editor_buffer(&app, view).primary_selection().range(),
        matches[0]
    );
    for (forward, index) in [(true, 1), (false, 0), (false, 2)] {
        app.search_find_again(forward).unwrap();
        assert_eq!(
            editor_buffer(&app, view).primary_selection().range(),
            matches[index]
        );
    }
    drive_native(
        &mut app,
        key(Key::Named(NamedKey::Enter), Modifiers::empty()),
    );
    assert!(!editor_buffer(&app, view).minibuffer_active());
    for (forward, index) in [(true, 0), (true, 1), (false, 0), (false, 2)] {
        app.search_find_again(forward).unwrap();
        assert_eq!(
            editor_buffer(&app, view).primary_selection().range(),
            matches[index]
        );
    }
    assert!(app.windows[&wid].search.is_none());
    assert_eq!(app.search_last_query, "terminal-only");
    let after = app.document_store.snapshot(document).unwrap();
    assert_eq!(after.seq, original.seq);
    assert_eq!(after.text, original.text);

    let second = dir.join("second.txt");
    fs::write(&second, source).unwrap();
    app.open_document_tab(AppKind::Editor, &file_uri(&second))
        .unwrap();
    let (_, second_view) = app.active_native_view(wid).unwrap();
    assert!(app.search_find_again(false).is_err());
    assert_eq!(
        editor_buffer(&app, second_view).primary_selection().range(),
        0..0
    );
    assert!(app.windows[&wid].search.is_none());
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn find_navigation_without_a_document_query_never_uses_the_terminal_query() {
    let dir = fresh_dir("aterm-native-find-refusal");
    let path = dir.join("reader.md");
    fs::write(&path, "# needle").unwrap();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.search_last_query = "terminal-only".to_string();
    app.open_document_tab(AppKind::Markdown, &file_uri(&path))
        .unwrap();
    for forward in [true, false] {
        assert!(app.search_find_again(forward).is_err());
    }
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    for forward in [true, false] {
        assert!(app.search_find_again(forward).is_err());
    }
    assert!(app.windows[&wid].search.is_none());
    assert_eq!(app.search_last_query, "terminal-only");
    let _ = fs::remove_dir_all(dir);
}

fn file_uri(path: &std::path::Path) -> String {
    crate::native_document_host::path_to_file_uri(path).unwrap()
}

fn key(key: Key, mods: Modifiers) -> InputEvent {
    InputEvent::Key {
        key,
        mods,
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    }
}

fn isearch_key() -> InputEvent {
    let mods = if cfg!(windows) {
        Modifiers::ALT
    } else {
        Modifiers::CTRL
    };
    key(Key::Character('s'), mods)
}

fn drive_native(app: &mut App, event: InputEvent) {
    assert!(app.native_input_event(WindowId(0), &event));
}

fn enter_settings_draft(app: &mut App, wid: WindowId, view: crate::tab_model::ViewId) {
    app.dispatch_native_view_event(
        wid,
        view,
        AppEvent::Action(crate::native_app::ActionInvocation {
            id: crate::native_ui::ActionId::new(format!(
                "settings/set/{}",
                crate::prefs::EDIT_FONT_FAMILY
            )),
            value: None,
        }),
    )
    .unwrap();
    app.dispatch_native_view_event(wid, view, AppEvent::TextInput(TextInputEvent::SelectAll))
        .unwrap();
    app.dispatch_native_view_event(
        wid,
        view,
        AppEvent::TextInput(TextInputEvent::Commit("Async Close Draft Mono".to_string())),
    )
    .unwrap();
}

fn discard_all_settings_drafts(app: &mut App, wid: WindowId, view: crate::tab_model::ViewId) {
    for _ in 0..2 {
        app.dispatch_native_view_event(
            wid,
            view,
            AppEvent::Action(crate::native_app::ActionInvocation {
                id: crate::native_ui::ActionId::new("settings/drafts/discard-all"),
                value: None,
            }),
        )
        .unwrap();
    }
}

fn editor_buffer(
    app: &App,
    view: crate::tab_model::ViewId,
) -> &crate::native_editor::EditorBufferView {
    let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view) else {
        panic!("expected editor view");
    };
    state.buffer.as_ref().expect("attached editor buffer")
}

fn admission_fixture(name: &str, bytes: &[u8]) -> (std::path::PathBuf, String) {
    let dir = fresh_dir(&format!("aterm-document-admission-{name}"));
    let path = dir.join("dropped.md");
    fs::write(&path, bytes).unwrap();
    let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
    (dir, uri)
}

fn admission_key(uri: &str, kind: AppKind) -> DocumentAdmissionKey {
    DocumentAdmissionKey {
        uri: uri.to_string(),
        kind,
        edit: None,
    }
}

fn worker_outcome(
    ticket: DocumentAdmissionTicket,
    wid: WindowId,
    key: &DocumentAdmissionKey,
) -> DocumentAdmissionOutcome {
    DocumentAdmissionOutcome {
        ticket,
        wid,
        key: key.clone(),
        result: App::admit_document_now(&key.uri, false).map(Box::new),
    }
}

#[test]
#[cfg(unix)]
fn directory_entry_admission_follows_links_without_loosening_normal_opens() {
    let dir = aterm_tempfile::TempDir::new().unwrap();
    let path = fs::canonicalize(dir.path()).unwrap();
    fs::write(path.join("target.txt"), "target contents").unwrap();
    fs::create_dir(path.join("target-dir")).unwrap();
    let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
    let wid = WindowId(0);
    let mut app = App::headless_for_test();
    app.open_document_tab_in_window(wid, AppKind::Editor, &uri)
        .unwrap();
    let (_, source) = app.active_native_view(wid).unwrap();
    let source_document = editor_buffer(&app, source).document;
    for (link, target) in [("file-link", "target.txt"), ("dir-link", "target-dir")] {
        std::os::unix::fs::symlink(path.join(target), path.join(link)).unwrap();
        let link_uri = crate::native_document_host::path_to_file_uri(&path.join(link)).unwrap();
        let target_uri = crate::native_document_host::path_to_file_uri(&path.join(target)).unwrap();
        let key = admission_key(&link_uri, AppKind::Editor);
        let options = app.admission_options(&key);
        assert!(
            App::admit_document_request(&key, DocumentAdmissionPurpose::Open, options).is_err()
        );
        let admitted =
            App::admit_document_request(&key, DocumentAdmissionPurpose::DirectoryEntry, options)
                .unwrap();
        assert_eq!(admitted.canonical_uri(), target_uri);
        app.open_document_tab_in_window(wid, AppKind::Editor, &uri)
            .unwrap();
        let point = app
            .document_store
            .directory(source_document)
            .unwrap()
            .point_for(Some(&link_uri));
        let Some(AppViewState::Editor(state)) = app.native_runtime.view_state_mut(source) else {
            panic!("directory view")
        };
        state.buffer.as_mut().unwrap().selections =
            vec![crate::native_editor::Selection::caret(point)];
        let enter = || {
            AppEvent::EditorInput(amacs::input::EditorInput::Key {
                key: aterm_types::keyboard::Key::Named(aterm_types::keyboard::NamedKey::Enter),
                mods: aterm_types::keyboard::Modifiers::empty(),
                text: None,
            })
        };
        app.with_controller_mark(true, |app| {
            app.dispatch_native_view_event(wid, source, enter())
        })
        .unwrap();
        assert!(app.document_store.id_for_uri(&target_uri).is_none());
        app.dispatch_native_view_event(wid, source, enter())
            .unwrap();
        let document = app.document_store.id_for_uri(&target_uri).unwrap();
        let focus = app.active_native_view(wid).unwrap();
        assert_eq!(editor_buffer(&app, focus.1).document, document);
        let ticket = app.mint_document_admission(wid, key.clone());
        app.native_documents
            .admissions
            .get_mut(&(wid, key.clone()))
            .unwrap()
            .purpose = DocumentAdmissionPurpose::DirectoryEntry;
        app.complete_document_admission(DocumentAdmissionOutcome {
            ticket,
            wid,
            key,
            result: Ok(Box::new(admitted)),
        })
        .unwrap();
        assert_eq!(app.active_native_view(wid).unwrap(), focus);
    }
    std::os::unix::fs::symlink(path.join("missing"), path.join("dangling")).unwrap();
    let dangling = crate::native_document_host::path_to_file_uri(&path.join("dangling")).unwrap();
    let focus = app.active_native_view(wid).unwrap();
    assert!(
        app.execute_editor_effect(source, EditorEffect::OpenPath { uri: dangling })
            .is_err()
    );
    assert_eq!(app.active_native_view(wid).unwrap(), focus);
}

#[test]
fn relative_markdown_link_opens_only_its_target_and_reveals_unsaved_headings_on_reuse() {
    let dir = aterm_tempfile::TempDir::new().unwrap();
    let base = fs::canonicalize(dir.path()).unwrap();
    let source = base.join("index.md");
    let target = base.join("target.md");
    let other = base.join("untouched.md");
    fs::write(
        &source,
        "[Kept](target.md#kept)\n\n[Draft](target.md#draft-heading)\n",
    )
    .unwrap();
    let target_text = "# Target\n\n## Kept\n\nBody.\n";
    fs::write(&target, target_text).unwrap();
    fs::write(&other, "# Not granted\n").unwrap();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.open_document_tab(AppKind::Markdown, &file_uri(&source))
        .unwrap();
    let (source_instance, _) = app.active_native_view(wid).unwrap();
    let source_document = app.native_runtime.document_id(source_instance).unwrap();
    let click = |app: &mut App, index| {
        app.dispatch_native_event(
            wid,
            AppEvent::Action(crate::native_app::ActionInvocation {
                id: crate::native_ui::ActionId::new(format!("markdown/link/{index}")),
                value: None,
            }),
        )
        .unwrap()
    };
    click(&mut app, 0);
    let (instance, view) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();
    assert_ne!(document, source_document);
    assert_eq!(
        app.document_store.canonical_uri(document),
        Some(file_uri(&target).as_str())
    );
    assert!(app.document_store.id_for_uri(&file_uri(&other)).is_none());
    let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view) else {
        panic!("editor");
    };
    assert_eq!(
        state.markdown.as_ref().unwrap().mode,
        MarkdownWorkspaceMode::Read
    );
    assert_eq!(
        state.markdown.as_ref().unwrap().reader.source_anchor,
        target_text.find("## Kept").unwrap()
    );
    let tabs = app.windows[&wid].tab_set.len();
    assert!(app.native_runtime.set_editor_markdown_mode(
        instance,
        view,
        MarkdownWorkspaceMode::Edit
    ));
    commit(&mut app, wid, "## Draft heading\n\n");
    let draft = app.document_store.snapshot(document).unwrap();
    let anchor = draft.text.find("## Draft heading").unwrap();
    app.open_document_tab(AppKind::Markdown, &file_uri(&source))
        .unwrap();
    click(&mut app, 1);
    assert_eq!(app.active_native_view(wid), Some((instance, view)));
    assert_eq!(app.windows[&wid].tab_set.len(), tabs);
    let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view) else {
        panic!("editor");
    };
    assert_eq!(
        state.markdown.as_ref().unwrap().mode,
        MarkdownWorkspaceMode::Edit
    );
    assert_eq!(
        state.buffer.as_ref().unwrap().primary_selection().head,
        anchor
    );
    let reused = app.document_store.snapshot(document).unwrap();
    assert_eq!((reused.seq, reused.text), (draft.seq, draft.text));
    assert_eq!(fs::read_to_string(&target).unwrap(), target_text);
    assert!(
        app.open_markdown_link(wid, source_document, "untouched.md?query=1")
            .is_err()
    );
    assert!(
        app.open_markdown_link(wid, source_document, "private.txt")
            .is_err()
    );
    assert!(app.document_store.id_for_uri(&file_uri(&other)).is_none());
    assert!(app.structural_invariants_ok());
}

#[test]
fn relative_markdown_link_fragment_survives_coalesced_async_admission() {
    let dir = aterm_tempfile::TempDir::new().unwrap();
    let base = fs::canonicalize(dir.path()).unwrap();
    let source = base.join("source.md");
    let target = base.join("target.md");
    fs::write(&source, "# Source\n").unwrap();
    let text = "# Target\n\n## First\n\n## Second\n";
    fs::write(&target, text).unwrap();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.open_document_tab(AppKind::Markdown, &file_uri(&source))
        .unwrap();
    let (instance, _) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();
    let key = admission_key(&file_uri(&target), AppKind::Markdown);
    let ticket = app.mint_document_admission(wid, key.clone());
    app.open_markdown_link(wid, document, "target.md#first")
        .unwrap();
    app.open_markdown_link(wid, document, "target.md#second")
        .unwrap();
    assert!(app.document_store.id_for_uri(&key.uri).is_none());
    assert_eq!(app.document_admission_pending(wid, &key), Some(ticket));
    app.complete_document_admission(worker_outcome(ticket, wid, &key))
        .unwrap();
    let (_, view) = app.active_native_view(wid).unwrap();
    let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view) else {
        panic!("editor");
    };
    let workspace = state.markdown.as_ref().unwrap();
    assert_eq!(workspace.mode, MarkdownWorkspaceMode::Read);
    assert_eq!(
        workspace.reader.source_anchor,
        text.find("## Second").unwrap()
    );
    assert!(app.native_documents.reveals.is_empty());
    assert!(app.structural_invariants_ok());
}

#[test]
fn markdown_uri_spellings_share_canonical_controller_and_view_capability() {
    let dir = aterm_tempfile::TempDir::new().unwrap();
    let path = fs::canonicalize(dir.path()).unwrap().join("notes.md");
    fs::write(&path, "# Canonical\n").unwrap();
    let canonical = file_uri(&path);
    let encoded = canonical.strip_suffix(".md").unwrap().to_string() + "%2Emd";
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.open_document_tab(AppKind::Markdown, &encoded).unwrap();
    let (instance, first) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();
    let Some(NativeApp::Editor(editor)) = app.native_runtime.app(instance) else {
        panic!("editor");
    };
    assert_eq!(editor.canonical_uri, canonical);
    assert!(
        editor.markdown.is_some(),
        "language comes from the admitted canonical URI"
    );
    for (intent, mode) in [
        (AppKind::Markdown, MarkdownWorkspaceMode::Read),
        (AppKind::Editor, MarkdownWorkspaceMode::Edit),
    ] {
        app.open_document_tab_in_window_with_config_symlinks(
            wid,
            intent,
            &canonical,
            false,
            Placement::NewTab,
        )
        .unwrap();
        let (current, view) = app.active_native_view(wid).unwrap();
        assert_eq!(current, instance);
        assert_ne!(view, first);
        assert_eq!(app.native_runtime.document_id(current), Some(document));
        let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view) else {
            panic!("editor");
        };
        assert_eq!(state.markdown.as_ref().unwrap().mode, mode);
    }
    assert_eq!(app.document_store.view_count(document), Some(3));
    assert!(app.structural_invariants_ok());
}

#[test]
#[cfg(unix)]
fn directory_alias_suffix_does_not_choose_markdown_capability_or_open_mode() {
    let dir = aterm_tempfile::TempDir::new().unwrap();
    let base = fs::canonicalize(dir.path()).unwrap();
    let wid = WindowId(0);
    for (alias, target, markdown) in [
        ("alias.txt", "target.md", true),
        ("alias.md", "target.txt", false),
    ] {
        let path = base.join(target);
        fs::write(&path, "# Target\n").unwrap();
        std::os::unix::fs::symlink(&path, base.join(alias)).unwrap();
        let mut app = App::headless_for_test();
        app.open_document_tab(AppKind::Editor, &file_uri(&base))
            .unwrap();
        let (_, directory) = app.active_native_view(wid).unwrap();
        app.execute_editor_effect(
            directory,
            EditorEffect::OpenPath {
                uri: file_uri(&base.join(alias)),
            },
        )
        .unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        let Some(NativeApp::Editor(editor)) = app.native_runtime.app(instance) else {
            panic!("editor");
        };
        assert_eq!(editor.canonical_uri, file_uri(&path));
        assert_eq!(editor.markdown.is_some(), markdown);
        let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view) else {
            panic!("editor");
        };
        assert_eq!(state.markdown.is_some(), markdown);
        if let Some(workspace) = &state.markdown {
            assert_eq!(workspace.mode, MarkdownWorkspaceMode::Read);
        } else {
            commit(&mut app, wid, "editable ");
            let document = app.native_runtime.document_id(instance).unwrap();
            assert_eq!(
                app.document_store.snapshot(document).unwrap().text.as_ref(),
                "editable # Target\n"
            );
        }
        assert!(app.structural_invariants_ok());
    }
}

#[test]
fn directory_refresh_preserves_selection_and_does_not_steal_focus_or_reopen_a_closed_view() {
    let dir = aterm_tempfile::TempDir::new().unwrap();
    let path = fs::canonicalize(dir.path()).unwrap();
    fs::write(path.join("b.txt"), "b").unwrap();
    let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
    let target = crate::native_document_host::path_to_file_uri(&path.join("b.txt")).unwrap();
    let wid = WindowId(0);
    let mut app = App::headless_for_test();
    app.open_document_tab_in_window(wid, AppKind::Editor, &uri)
        .unwrap();
    let document = app.document_store.id_for_uri(&uri).unwrap();
    let (_, view) = app.active_native_view(wid).unwrap();
    let point = app
        .document_store
        .directory(document)
        .unwrap()
        .point_for(Some(&target));
    let Some(AppViewState::Editor(state)) = app.native_runtime.view_state_mut(view) else {
        panic!("editor")
    };
    state.buffer.as_mut().unwrap().selections = vec![crate::native_editor::Selection::caret(point)];

    fs::write(path.join("a.txt"), "a").unwrap();
    app.open_document_tab_in_window(wid, AppKind::Editor, &uri)
        .unwrap();
    assert_eq!(app.active_native_view(wid).unwrap().1, view);
    assert!(
        app.document_store
            .snapshot(document)
            .unwrap()
            .text
            .contains("a.txt")
    );
    assert_eq!(
        app.document_store
            .directory(document)
            .unwrap()
            .target_at(editor_buffer(&app, view).primary_selection().head),
        Some(target.as_str())
    );

    fs::write(path.join("c.txt"), "c").unwrap();
    let key = admission_key(&uri, AppKind::Editor);
    let ticket = app.mint_document_admission(wid, key.clone());
    app.native_documents
        .admissions
        .get_mut(&(wid, key.clone()))
        .unwrap()
        .purpose = DocumentAdmissionPurpose::DirectoryRefresh(document);
    let outcome = worker_outcome(ticket, wid, &key);
    let other = crate::native_document_host::path_to_file_uri(&path.join("b.txt")).unwrap();
    app.open_document_tab_in_window(wid, AppKind::Editor, &other)
        .unwrap();
    let focus = app.active_native_view(wid).unwrap();
    app.complete_document_admission(outcome).unwrap();
    assert_eq!(app.active_native_view(wid).unwrap(), focus);
    assert!(
        app.document_store
            .snapshot(document)
            .unwrap()
            .text
            .contains("c.txt")
    );
    assert_eq!(
        app.document_store
            .directory(document)
            .unwrap()
            .target_at(editor_buffer(&app, view).primary_selection().head),
        Some(target.as_str())
    );
    assert_eq!(app.document_store.dirty(document), Some(false));

    let ticket = app.mint_document_admission(wid, key.clone());
    app.native_documents
        .admissions
        .get_mut(&(wid, key.clone()))
        .unwrap()
        .purpose = DocumentAdmissionPurpose::DirectoryRefresh(document);
    let before = app.document_store.snapshot(document).unwrap();
    let failed = DocumentAdmissionOutcome {
        ticket,
        wid,
        key: key.clone(),
        result: Err(DocumentOpenFailure(DocumentHostError::NotAFile)),
    };
    assert!(app.complete_document_admission(failed).is_ok());
    assert_eq!(app.active_native_view(wid).unwrap(), focus);
    assert_eq!(
        app.document_store.snapshot(document).unwrap().seq,
        before.seq
    );
    let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view) else {
        panic!("editor")
    };
    assert!(state.status.is_some());

    let origin = app.document_store.snapshot(document).unwrap().text.len();
    let Some(AppViewState::Editor(state)) = app.native_runtime.view_state_mut(view) else {
        panic!("editor")
    };
    state.buffer.as_mut().unwrap().minibuffer = crate::native_editor::Minibuffer::Search {
        query: "b.txt".into(),
        origin,
    };
    fs::remove_file(path.join("a.txt")).unwrap();
    fs::remove_file(path.join("c.txt")).unwrap();
    app.execute_editor_effect(view, EditorEffect::RefreshDirectory { document })
        .unwrap();
    let crate::native_editor::Minibuffer::Search { origin, .. } =
        &editor_buffer(&app, view).minibuffer
    else {
        panic!("search remains open")
    };
    let refreshed = app.document_store.snapshot(document).unwrap();
    assert!(*origin <= refreshed.text.len());
    assert!(refreshed.text.is_char_boundary(*origin));
    let Some(AppViewState::Editor(state)) = app.native_runtime.view_state_mut(view) else {
        panic!("editor")
    };
    state.buffer.as_mut().unwrap().clear_minibuffer();

    let ticket = app.mint_document_admission(wid, key.clone());
    app.native_documents
        .admissions
        .get_mut(&(wid, key.clone()))
        .unwrap()
        .purpose = DocumentAdmissionPurpose::DirectoryRefresh(document);
    let superseded = worker_outcome(ticket, wid, &key);
    app.request_document_tab_in_window(wid, AppKind::Editor, &uri)
        .unwrap();
    assert!(matches!(
        app.complete_document_admission(superseded),
        Err(DocumentAdmissionRejected::Dropped(_))
    ));
    let ticket = app.mint_document_admission(wid, key.clone());
    app.native_documents
        .admissions
        .get_mut(&(wid, key.clone()))
        .unwrap()
        .purpose = DocumentAdmissionPurpose::DirectoryRefresh(document);
    let outcome = DocumentAdmissionOutcome {
        ticket,
        wid,
        key: key.clone(),
        result: Err(DocumentOpenFailure(DocumentHostError::NotAFile)),
    };
    app.close_active_native_tab(wid).unwrap();
    let focus = app.active_native_view(wid);
    assert!(matches!(
        app.complete_document_admission(outcome),
        Err(DocumentAdmissionRejected::Dropped(_))
    ));
    assert_eq!(app.active_native_view(wid), focus);
}

#[test]
fn reopening_a_file_replaced_by_a_directory_preserves_its_unsaved_buffer() {
    let dir = aterm_tempfile::TempDir::new().unwrap();
    let path = fs::canonicalize(dir.path()).unwrap().join("changing");
    fs::write(&path, "original").unwrap();
    let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
    let mut app = App::headless_for_test();
    app.open_document_tab_in_window(WindowId(0), AppKind::Editor, &uri)
        .unwrap();
    let document = app.document_store.id_for_uri(&uri).unwrap();
    let snapshot = app.document_store.snapshot(document).unwrap();
    assert!(matches!(
        app.document_store
            .replace_text(document, snapshot.seq, "unsaved"),
        crate::document_store::DocumentTxnOutcome::Committed { .. }
    ));
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    assert!(
        app.open_document_tab_in_window(WindowId(0), AppKind::Editor, &uri)
            .is_err()
    );
    assert_eq!(
        app.document_store.snapshot(document).unwrap().text.as_ref(),
        "unsaved"
    );
    assert_eq!(app.document_store.dirty(document), Some(true));
    fs::remove_dir(&path).unwrap();
    fs::write(&path, "original").unwrap();
    let grant_id = app.native_documents.persistence.grant(document).unwrap();
    let restored = app
        .native_documents
        .grants
        .refresh_local(
            grant_id,
            crate::native_document_host::DEFAULT_DOCUMENT_LIMIT,
        )
        .unwrap();
    app.native_documents
        .persistence
        .accept_overwrite(document, restored.observed)
        .unwrap();
    let pending = app
        .native_documents
        .persistence
        .begin(&app.document_store.snapshot(document).unwrap())
        .unwrap();
    let saved = crate::native_document_host::execute_granted_save(
        app.native_documents.grants.get(grant_id).unwrap(),
        &pending.plan,
    );
    assert!(matches!(
        saved,
        crate::native_document_io::AtomicSaveResult::Committed(_)
    ));
    assert_eq!(fs::read_to_string(&path).unwrap(), "unsaved");
}

#[test]
fn document_admission_outcome_round_trips_into_an_installed_tab() {
    let (dir, uri) = admission_fixture("round-trip", b"# dropped\n");
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let key = admission_key(&uri, AppKind::Markdown);
    let ticket = app.mint_document_admission(wid, key.clone());
    assert_eq!(app.document_admission_pending(wid, &key), Some(ticket));

    let outcome = worker_outcome(ticket, wid, &key);
    let line = app
        .complete_document_admission(outcome)
        .expect("a live window installs the admitted document");
    assert!(line.starts_with("app editor file://"), "{line}");
    assert_eq!(app.document_admission_pending(wid, &key), None);
    let (instance, _) = app
        .active_native_view(wid)
        .expect("the Markdown tab is active");
    assert_eq!(
        app.native_runtime.app(instance).map(|app| app.kind()),
        Some(AppKind::Editor)
    );
    let canonical = crate::native_document_host::path_to_file_uri(
        &fs::canonicalize(dir.join("dropped.md")).unwrap(),
    )
    .unwrap();
    assert!(app.document_store.id_for_uri(&canonical).is_some());
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn duplicate_in_flight_admission_is_coalesced_onto_the_first() {
    let (dir, uri) = admission_fixture("coalesce", b"# once\n");
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let key = admission_key(&uri, AppKind::Editor);
    let ticket = app.mint_document_admission(wid, key.clone());
    assert_eq!(
        app.begin_document_admission(wid, key.clone()),
        Ok(DocumentAdmission::Coalesced(ticket)),
        "same uri, same kind: rides on the in-flight read"
    );
    assert_eq!(
        app.begin_document_admission(wid, key.clone()),
        Ok(DocumentAdmission::Coalesced(ticket))
    );
    let other = admission_key(&uri, AppKind::Markdown);
    assert!(
        matches!(
            app.begin_document_admission(wid, other.clone()),
            Ok(DocumentAdmission::Opened(_))
        ),
        "no proxy: the non-coalesced request admits inline"
    );
    assert_eq!(app.document_admission_pending(wid, &other), None);

    let outcome = worker_outcome(ticket, wid, &key);
    app.complete_document_admission(outcome).unwrap();
    assert_eq!(app.document_admission_pending(wid, &key), None);
    let canonical = crate::native_document_host::path_to_file_uri(
        &fs::canonicalize(dir.join("dropped.md")).unwrap(),
    )
    .unwrap();
    let document = app.document_store.id_for_uri(&canonical).unwrap();
    assert_eq!(
        app.document_native_views(document).len(),
        1,
        "Read and Edit intents reuse one canonical document view"
    );
    let replay = worker_outcome(ticket, wid, &key);
    assert!(matches!(
        app.complete_document_admission(replay),
        Err(DocumentAdmissionRejected::Dropped(_))
    ));
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn admission_completion_for_a_closed_window_is_dropped_cleanly() {
    let (dir, uri) = admission_fixture("closed-window", b"# orphan\n");
    let mut app = App::headless_for_test();
    let gone = WindowId(4242);
    assert!(!app.windows.contains_key(&gone));
    let key = admission_key(&uri, AppKind::Markdown);
    let ticket = app.mint_document_admission(gone, key.clone());
    let outcome = worker_outcome(ticket, gone, &key);
    let error = app
        .complete_document_admission(outcome)
        .expect_err("a closed window cannot receive the tab");
    assert!(
        matches!(error, DocumentAdmissionRejected::Dropped(_)),
        "a closed window drops silently, it does not alert: {error:?}"
    );
    assert_eq!(app.document_admission_pending(gone, &key), None);
    let canonical = crate::native_document_host::path_to_file_uri(
        &fs::canonicalize(dir.join("dropped.md")).unwrap(),
    )
    .unwrap();
    assert_eq!(app.document_store.id_for_uri(&canonical), None);
    assert_eq!(app.native_documents.grants.id_for_uri(&canonical), None);
    assert!(matches!(
        app.request_document_tab_in_window(WindowId(0), AppKind::Markdown, &uri),
        Ok(DocumentAdmission::Opened(_))
    ));
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn same_file_from_another_window_is_its_own_admission_not_a_coalesce() {
    let (dir, uri) = admission_fixture("two-windows", b"# twice\n");
    let mut app = App::headless_for_test();
    let live = WindowId(0);
    let gone = WindowId(4243);
    let key = admission_key(&uri, AppKind::Markdown);
    let first = app.mint_document_admission(gone, key.clone());
    let second = app
        .request_document_tab_in_window(live, AppKind::Markdown, &uri)
        .unwrap();
    assert!(
        matches!(second, DocumentAdmission::Opened(_)),
        "a different window is a different admission: {second:?}"
    );
    assert_eq!(
        app.document_admission_pending(gone, &key),
        Some(first),
        "the first window's read is still pending, untouched"
    );
    assert!(app.active_native_view(live).is_some());
    let ticket = app.mint_document_admission(live, key.clone());
    assert_eq!(
        app.begin_document_admission(live, key.clone()),
        Ok(DocumentAdmission::Coalesced(ticket))
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn stale_and_failed_admissions_never_register_a_document() {
    let (dir, uri) = admission_fixture("stale", b"# stale\n");
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let key = admission_key(&uri, AppKind::Markdown);
    let old = app.mint_document_admission(wid, key.clone());
    let fresh = app.mint_document_admission(wid, key.clone());
    assert_ne!(old, fresh);
    let stale = worker_outcome(old, wid, &key);
    let error = app.complete_document_admission(stale).unwrap_err();
    assert!(
        matches!(error, DocumentAdmissionRejected::Dropped(_)),
        "{error:?}"
    );
    assert_eq!(
        app.document_admission_pending(wid, &key),
        Some(fresh),
        "a stale completion leaves the live admission pending"
    );

    let failed = DocumentAdmissionOutcome {
        ticket: fresh,
        wid,
        key: key.clone(),
        result: Err(DocumentOpenFailure(DocumentHostError::Io {
            stage: crate::native_document_io::AtomicSaveStage::Preflight,
            kind: None,
            message:
                "dropped.md is evicted from iCloud Drive and must be downloaded in Finder first"
                    .to_string(),
        })),
    };
    let error = app.complete_document_admission(failed).unwrap_err();
    assert!(
        matches!(&error, DocumentAdmissionRejected::Failed(detail) if detail.contains("evicted from iCloud Drive")),
        "a read failure is the user's to see: {error:?}"
    );
    assert_eq!(app.document_admission_pending(wid, &key), None);
    let canonical = crate::native_document_host::path_to_file_uri(
        &fs::canonicalize(dir.join("dropped.md")).unwrap(),
    )
    .unwrap();
    assert_eq!(app.document_store.id_for_uri(&canonical), None);
    assert!(app.active_native_view(wid).is_none());
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn editor_reducer_overscrolls_to_a_presentable_anchor_and_reverses_immediately() {
    let dir = fresh_dir("aterm-editor-scroll-anchor");
    let path = dir.join("aterm.toml");
    let source = (0..300)
        .map(|line| format!("# config line {line}\n"))
        .collect::<String>();
    crate::test_scratch::write_config(&path, &source);

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.ensure_and_open_config_editor_path_in_window(wid, &path)
        .unwrap();
    let (instance, view) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();
    app.dispatch_native_view_event(
        wid,
        view,
        AppEvent::EditorViewportChanged { visible_lines: 4 },
    )
    .unwrap();
    app.dispatch_native_view_event(wid, view, AppEvent::ScrollLines(i32::MAX))
        .unwrap();

    let snapshot = app.document_store.snapshot(document).unwrap();
    let bottom = crate::native_editor::project_viewport(
        snapshot.text.as_ref(),
        editor_buffer(&app, view),
        editor_buffer(&app, view).viewport_lines(),
        80,
    );
    let visible_lines = editor_buffer(&app, view).viewport_lines();
    assert_eq!(bottom.total_lines, 301);
    assert_eq!(
        bottom.first_line,
        bottom.total_lines.saturating_sub(visible_lines),
        "stored anchor is the last full page for the installed renderer capacity"
    );
    let bottom_anchor = editor_buffer(&app, view).viewport_anchor;

    app.dispatch_native_view_event(wid, view, AppEvent::ScrollLines(-1))
        .unwrap();
    let one_line_up = crate::native_editor::project_viewport(
        snapshot.text.as_ref(),
        editor_buffer(&app, view),
        visible_lines,
        80,
    );
    assert_eq!(one_line_up.first_line + 1, bottom.first_line);
    assert!(
        editor_buffer(&app, view).viewport_anchor < bottom_anchor,
        "one reverse reducer event must move the stored and painted anchor"
    );

    let _ = fs::remove_dir_all(dir);
}

fn clear_exact_config_assist_cache(
    app: &App,
    instance: crate::tab_model::AppInstanceId,
    context: crate::native_config_language::ConfigCompletionContext,
) {
    assert!(
        app.native_runtime
            .config_editor_analysis(instance)
            .is_some(),
        "the exact worker analysis is complete"
    );
    app.native_runtime.clear_config_assist_cache(instance);
    assert_eq!(
        app.native_runtime
            .cached_config_completion_count(instance, context),
        0
    );
    assert!(
        !app.native_runtime
            .cached_config_assist_present(instance, context),
        "no render may pre-populate the input-path fixture"
    );
}

fn begin_inflight_save_for_test(
    app: &mut App,
    document: DocumentId,
) -> crate::native_document_host::PendingDocumentSave {
    let snapshot = app.document_store.snapshot(document).unwrap();
    let pending = app.native_documents.persistence.begin(&snapshot).unwrap();
    assert!(app.native_documents.inflight.insert(document));
    app.native_runtime.set_document_saving(document, true);
    pending
}

fn finish_inflight_save_for_test(
    app: &mut App,
    document: DocumentId,
    source_view: crate::tab_model::ViewId,
    pending: crate::native_document_host::PendingDocumentSave,
) {
    let grant = app
        .native_documents
        .grants
        .cloned_grant(pending.grant)
        .unwrap();
    let config_themes = app
        .native_config_service
        .bound_logical_path()
        .filter(|path| grant.targets_logical_path(path))
        .map(|_| std::sync::Arc::clone(&app.native_config_service.snapshot().assets.themes));
    let result = crate::native_document_host::execute_granted_save(&grant, &pending.plan);
    let config_observation =
        prepare_config_save_observation(&grant, &pending.plan, &result, config_themes);
    app.finish_native_document_save(
        document,
        source_view,
        pending.plan.generation,
        pending.plan.bytes,
        result,
        config_observation,
    )
    .unwrap_or_else(|error| panic!("inflight replay save failed: {error}"));
}

#[cfg(unix)]
#[test]
fn manual_open_refuses_writerless_journal_fifo_without_blocking() {
    use std::os::unix::ffi::OsStrExt as _;

    let dir = fresh_dir("aterm-manual-journal-fifo");
    let config = dir.join("aterm.toml");
    fs::write(&config, "window_theme = \"dark\"\n").unwrap();
    let journal_root = dir.join("private-drafts");
    let mut app = App::headless_for_test();
    app.native_documents.journals = JournalSlot::for_test(journal_root.clone());

    let canonical = crate::native_document_host::path_to_file_uri(
        &fs::canonicalize(&config).expect("canonical config path"),
    )
    .unwrap();
    let journal = app
        .native_documents
        .journals
        .as_ref()
        .unwrap()
        .path_for_uri(&canonical);
    let journal_c =
        std::ffi::CString::new(journal.as_os_str().as_bytes()).expect("journal FIFO path");
    // SAFETY: `journal_c` is a live NUL-terminated pathname and `mkfifo` retains no pointer.
    assert_eq!(unsafe { libc::mkfifo(journal_c.as_ptr(), 0o600) }, 0);

    let started = std::time::Instant::now();
    app.ensure_and_open_config_editor_path_in_window(WindowId(0), &config)
        .expect_err("Manual must refuse a special recovery-journal target");
    assert!(started.elapsed() < REFUSES_WITHOUT_BLOCKING);
    assert!(app.active_native_view(WindowId(0)).is_none());
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn manual_open_reports_busy_held_journal_lock_and_retries_cleanly() {
    let dir = fresh_dir("aterm-manual-journal-held");
    let config = dir.join("aterm.toml");
    crate::test_scratch::write_config(&config, "window_theme = \"dark\"\n");
    let journal_root = dir.join("private-drafts");
    let mut app = App::headless_for_test();
    app.native_documents.journals = JournalSlot::for_test(journal_root.clone());
    let canonical = crate::native_document_host::path_to_file_uri(
        &fs::canonicalize(&config).expect("canonical config path"),
    )
    .unwrap();
    let journal = app
        .native_documents
        .journals
        .as_ref()
        .unwrap()
        .path_for_uri(&canonical);
    let lock_path = journal_root.join(format!(
        ".{}.lock",
        journal.file_name().unwrap().to_string_lossy()
    ));
    let held = crate::native_document_host::HeldAdvisoryLock::lock(
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .unwrap(),
    )
    .unwrap();

    let started = std::time::Instant::now();
    let error = app
        .ensure_and_open_config_editor_path_in_window(WindowId(0), &config)
        .expect_err("held journal lock must report busy");
    assert!(started.elapsed() < REFUSES_WITHOUT_BLOCKING);
    assert!(error.contains("busy"), "{error}");
    assert!(app.active_native_view(WindowId(0)).is_none());

    drop(held);
    app.ensure_and_open_config_editor_path_in_window(WindowId(0), &config)
        .expect("Manual retry succeeds after the lock is released");
    assert!(app.active_native_view(WindowId(0)).is_some());
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn focused_config_completion_enter_and_space_activate_its_semantic_button() {
    for (suffix, named_key) in [("enter", NamedKey::Enter), ("space", NamedKey::Space)] {
        let (dir, _, mut app, (instance, view)) = open_config(
            &format!("aterm-config-editor-focused-completion-{suffix}"),
            "win",
        );
        let wid = WindowId(0);
        let document = app.native_runtime.document_id(instance).unwrap();
        app.dispatch_native_event(wid, AppEvent::EditorCommand(EditorCommand::MoveLineEnd))
            .unwrap();

        assert!(app.prepare_native_input_scratch(wid));
        let focus = app
            .retained_native_leaf_artifact(wid, view, false)
            .expect("compiled native config editor")
            .compiled
            .semantics
            .iter()
            .find(|node| {
                node.key.as_str().starts_with("editor/config-completion/")
                    && node.label.starts_with("Insert window_theme")
            })
            .expect("window theme completion semantic button")
            .key
            .clone();
        let context = app
            .editor_config_completion_context(instance, view)
            .expect("exact rendered completion context");
        let state = app
            .native_runtime
            .view_state_mut(view)
            .expect("config editor view state");
        state.common_mut().last_focus = Some(focus);
        let AppViewState::Editor(state) = state else {
            panic!("config editor view state");
        };
        state.config_completion_interaction = Some(context);

        drive_native(&mut app, key(Key::Named(named_key), Modifiers::empty()));
        assert_eq!(
            app.document_store.snapshot(document).unwrap().text.as_ref(),
            "window_theme = \"auto\"",
            "{suffix}"
        );
        let _ = fs::remove_dir_all(dir);
    }
}

#[test]
fn config_file_creation_is_race_safe_and_never_truncates_existing_bytes() {
    let dir = fresh_dir("aterm-config-editor-ensure");
    let path = dir.join("nested/aterm.toml");

    let recording = aterm_durable::seam::Recording::start();
    let first = ensure_config_file(&path).unwrap();
    #[cfg(unix)]
    let expected_create = [aterm_durable::Flush::Ordered, aterm_durable::Flush::Ordered];
    #[cfg(not(unix))]
    let expected_create = [aterm_durable::Flush::Ordered];
    assert_eq!(
        recording.take(),
        expected_create,
        "the created config file; Unix also flushes its entry"
    );
    assert!(first.is_absolute());
    assert_eq!(fs::read(&path).unwrap(), b"");

    let authored = b"# keep this comment\nfont_px = 15.0\n";
    fs::write(&path, authored).unwrap();
    let second = ensure_config_file(&path).unwrap();
    assert_eq!(
        recording.take(),
        [],
        "an existing config file is not written"
    );
    drop(recording);
    assert_eq!(second, first);
    assert_eq!(fs::read(&path).unwrap(), authored);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn config_file_creation_rejects_relative_authority_before_mutating() {
    let path = std::path::PathBuf::from(format!(
        ".aterm-relative-config-ensure-{}/aterm.toml",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(path.parent().unwrap());

    ensure_config_file(&path).unwrap_err();
    assert!(
        !path.parent().unwrap().exists(),
        "authority validation must precede directory creation"
    );
}

#[cfg(unix)]
#[test]
fn config_file_creation_through_bound_parent_symlink_preserves_the_alias() {
    use std::os::unix::fs::symlink;

    let dir = fresh_dir("aterm-config-editor-ensure-symlink");
    let real_parent = dir.join("real");
    let alias = dir.join("alias");
    fs::create_dir_all(&real_parent).unwrap();
    symlink(&real_parent, &alias).unwrap();
    let path = alias.join("missing/aterm.toml");

    let opened = ensure_config_file(&path).unwrap();

    assert_eq!(opened, path);
    assert_eq!(
        fs::read(real_parent.join("missing/aterm.toml")).unwrap(),
        b""
    );
    assert!(
        fs::symlink_metadata(&alias)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read_link(&alias).unwrap(), real_parent);
    let _ = fs::remove_dir_all(dir);
}

#[cfg(unix)]
#[test]
fn manual_final_config_symlink_opens_saves_target_and_preserves_link() {
    use std::os::unix::fs::symlink;

    let dir = fresh_dir("aterm-config-editor-final-link");
    let config_dir = dir.join("config");
    let dotfiles_dir = dir.join("dotfiles");
    fs::create_dir_all(&config_dir).unwrap();
    fs::create_dir_all(&dotfiles_dir).unwrap();
    let target = dotfiles_dir.join("aterm.toml");
    let logical = config_dir.join("aterm.toml");
    let link_destination = std::path::PathBuf::from("../dotfiles/aterm.toml");
    crate::test_scratch::write_config(&target, "theme = \"Default\"\n");
    symlink(&link_destination, &logical).unwrap();

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let opened = app
        .ensure_and_open_config_editor_path_in_window(wid, &logical)
        .unwrap();
    assert_eq!(opened, file_uri(&logical));
    let (instance, view) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();
    assert_eq!(
        app.document_store.snapshot(document).unwrap().text.as_ref(),
        "theme = \"Default\"\n"
    );
    let grant = app
        .native_documents
        .persistence
        .grant(document)
        .and_then(|grant| app.native_documents.grants.get(grant))
        .unwrap();
    assert_eq!(grant.logical_path(), logical);
    assert_eq!(grant.target().target_path(), target.canonicalize().unwrap());
    assert_eq!(
        app.native_config_service.bound_logical_path(),
        Some(logical.as_path())
    );

    app.dispatch_native_event(wid, AppEvent::TextInput(TextInputEvent::SelectAll))
        .unwrap();
    commit(&mut app, wid, "theme = \"Nord\"\n");
    app.save_document_checkpoint(document, view).unwrap();

    assert!(
        fs::symlink_metadata(&logical)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read_link(&logical).unwrap(), link_destination);
    assert_eq!(fs::read_to_string(&target).unwrap(), "theme = \"Nord\"\n");
    assert_eq!(
        app.native_config_service.snapshot().text.as_ref(),
        "theme = \"Nord\"\n"
    );
    let _ = fs::remove_dir_all(dir);
}

/// Swapping the config's final symlink, or its parent directory's, between
/// open and Save is a conflict: the draft never lands behind the new link.
#[cfg(unix)]
#[test]
fn manual_config_symlink_swap_conflicts_without_redirecting_save() {
    use std::os::unix::fs::symlink;

    for (link, first_dest, second_dest, logical) in [
        ("aterm.toml", "first.toml", "second.toml", "aterm.toml"),
        ("config", "first", "second", "config/aterm.toml"),
    ] {
        let dir = fresh_dir(&format!("aterm-config-editor-link-swap-{link}"));
        let behind = |dest: &str| dir.join(logical.replacen(link, dest, 1));
        let (first, second) = (behind(first_dest), behind(second_dest));
        for (path, text) in [
            (&first, "theme = \"Default\"\n"),
            (&second, "theme = \"Dracula\"\n"),
        ] {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            crate::test_scratch::write_config(path, text);
        }
        let alias = dir.join(link);
        let logical = dir.join(logical);
        symlink(first_dest, &alias).unwrap();

        let mut app = App::headless_for_test();
        app.native_config_service =
            crate::native_config_service::VersionedConfigService::load_path(&logical).unwrap();
        let wid = WindowId(0);
        app.ensure_and_open_config_editor_path_in_window(wid, &logical)
            .unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        app.dispatch_native_event(wid, AppEvent::TextInput(TextInputEvent::SelectAll))
            .unwrap();
        commit(&mut app, wid, "theme = \"Nord\"\n");

        fs::remove_file(&alias).unwrap();
        symlink(second_dest, &alias).unwrap();
        app.save_document_checkpoint(document, view).unwrap_err();

        assert_eq!(fs::read_to_string(&first).unwrap(), "theme = \"Default\"\n");
        assert_eq!(
            fs::read_to_string(&second).unwrap(),
            "theme = \"Dracula\"\n"
        );
        assert_eq!(
            fs::read_link(&alias).unwrap(),
            std::path::PathBuf::from(second_dest)
        );
        assert_eq!(app.document_store.dirty(document), Some(true));
        let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view) else {
            panic!("Manual editor remains installed after {link} conflict");
        };
        assert!(state.status.is_some(), "conflict must be visible");
        assert_eq!(
            app.native_config_service.snapshot().text.as_ref(),
            "theme = \"Default\"\n",
            "a conflicted draft must leave the admitted live config unchanged"
        );
        let _ = fs::remove_dir_all(dir);
    }
}

#[test]
fn config_editor_uses_one_native_document_tab_without_spawning_a_session() {
    let dir = fresh_dir("aterm-config-editor-native");
    let path = dir.join("config/aterm/aterm.toml");
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let sessions = app.pool.sessions.len();

    let first_uri = app
        .ensure_and_open_config_editor_path_in_window(wid, &path)
        .unwrap();
    let first = app.active_native_view(wid).expect("native config editor");
    let document = app
        .native_runtime
        .document_id(first.0)
        .expect("config document");
    let tabs = app.windows[&wid].tab_set.len();
    assert!(app.native_runtime.config_editor_enabled(first.0));
    assert_eq!(
        app.document_store.snapshot(document).unwrap().text.as_ref(),
        ""
    );
    assert_eq!(app.pool.sessions.len(), sessions);

    let second_uri = app
        .ensure_and_open_config_editor_path_in_window(wid, &path)
        .unwrap();
    assert_eq!(second_uri, first_uri);
    assert_eq!(app.active_native_view(wid), Some(first));
    assert_eq!(app.windows[&wid].tab_set.len(), tabs);
    assert_eq!(app.pool.sessions.len(), sessions);
    let _ = fs::remove_dir_all(dir);
}

/// Every `aterm.toml` aterm makes passes the rule every reader of it holds
/// (`aterm_types::dirs::aterm_config_refusal`) under a user-private-group login's umask
/// `002`, Ubuntu's default, where a file made with `0o666` is `664` and every later read of
/// it is refused (the Linux lane found the config editor's first open refused so, unit L1):
/// the config editor's first open (`ensure_config_file`), a Settings write that makes a
/// missing file (`commit_atomic_bytes`) and `--write-config`'s starter. The umask is
/// process-wide, so the body runs alone in a child of this test binary.
/// NEGATIVE CONTROLS (scratch): amacs-files' config commit creating its temporary `0o666`
/// fails the first two; the starter made by a plain `std::fs::write` fails the third.
#[cfg(unix)]
#[test]
fn every_aterm_toml_aterm_makes_under_an_open_umask_passes_the_config_rule() {
    use std::os::unix::fs::PermissionsExt as _;
    if !crate::test_alone::alone() {
        return;
    }
    // SAFETY: `umask(2)` sets this isolated test child's file-creation mask and cannot fail.
    unsafe { libc::umask(0o002) };
    let scratch = aterm_tempfile::tempdir().expect("scratch dir");
    let editor = scratch.path().join("editor/aterm/aterm.toml");
    ensure_config_file(&editor).expect("the config editor's first open makes it");
    let settings = scratch.path().join("settings/aterm/aterm.toml");
    let missing = crate::native_document_host::read_config_atomic_file(
        &settings,
        crate::native_document_host::DEFAULT_DOCUMENT_LIMIT,
        true,
    )
    .expect("a missing config reads as missing");
    assert!(
        matches!(
            crate::native_document_host::commit_atomic_bytes(&missing.baseline, b"font_px = 14\n"),
            crate::native_document_host::AtomicCommitResult::Committed(_)
        ),
        "the Settings write makes it"
    );
    let starter = scratch.path().join("starter/aterm.toml");
    fs::create_dir_all(starter.parent().expect("a folder")).expect("its folder");
    crate::cli::write_starter_config(&starter).expect("the starter");
    let mut wrong = Vec::new();
    for path in [&editor, &settings, &starter] {
        let meta = fs::metadata(path).expect("made");
        if let Some(why) = aterm_types::dirs::aterm_config_file_refusal(&meta) {
            wrong.push(format!(
                "{} {:o}: {why}",
                path.display(),
                meta.permissions().mode() & 0o7777
            ));
        }
        if let Err(error) = crate::native_document_host::read_config_atomic_file(
            path,
            crate::native_document_host::DEFAULT_DOCUMENT_LIMIT,
            false,
        ) {
            wrong.push(format!("{} read: {error:?}", path.display()));
        }
    }
    assert!(wrong.is_empty(), "made under umask 002: {wrong:#?}");
}

#[test]
fn config_editor_blocks_malformed_save_and_keeps_disk_bytes_durable() {
    let dir = fresh_dir("aterm-config-editor-invalid");
    let path = dir.join("aterm.toml");
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.ensure_and_open_config_editor_path_in_window(wid, &path)
        .unwrap();
    let (instance, view) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();

    app.dispatch_native_event(
        wid,
        AppEvent::TextInput(TextInputEvent::Commit(
            "font_px = \"definitely not a number\"\n".to_string(),
        )),
    )
    .unwrap();
    assert!(
        app.native_runtime
            .config_editor_save_error(document)
            .is_some()
    );
    let save = app
        .native_runtime
        .commands(instance, view)
        .unwrap()
        .into_iter()
        .find(|command| command.id.as_str() == "editor/save")
        .expect("editor save command");
    assert!(!save.enabled);

    app.save_document_checkpoint(document, view).unwrap_err();
    assert_eq!(fs::read(&path).unwrap(), b"");
    let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view) else {
        panic!("config editor view must remain installed");
    };
    assert!(state.status.is_some());
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn manual_save_reports_busy_held_file_lock_preserves_draft_and_retries() {
    let (dir, path, mut app, (instance, view)) =
        open_config("aterm-config-editor-held-save-lock", "");
    let wid = WindowId(0);
    let document = app.native_runtime.document_id(instance).unwrap();
    commit(&mut app, wid, "theme = \"Dracula\"\n");
    let draft = app.document_store.snapshot(document).unwrap();
    assert_eq!(app.document_store.failed_checkpoint_seq(document), None);
    assert_eq!(
        app.document_store.phase(document),
        Some(crate::document_store::DocumentPhase::Active)
    );
    assert!(app.native_documents.pending_closes.is_empty());
    assert!(app.native_documents.pending_window_closes.is_empty());
    assert!(app.native_documents.pending_quit.is_none());
    let lock_path = dir.join(".aterm.toml.aterm-write.lock");
    let held = crate::native_document_host::HeldAdvisoryLock::lock(
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .unwrap(),
    )
    .unwrap();

    let pending = app.hold_document_save_for_test(document);
    let grant = app
        .native_documents
        .grants
        .cloned_grant(pending.grant)
        .unwrap();
    let result = crate::native_document_host::execute_granted_save_after(
        &grant,
        &pending.plan,
        std::time::Duration::from_secs(61),
    );
    let error = app
        .finish_native_document_save(
            document,
            view,
            pending.plan.generation,
            pending.plan.bytes,
            result,
            None,
        )
        .unwrap_err();
    assert_eq!(
        error,
        format!(
            "Save failed: {}",
            crate::save_words::words(
                crate::save_words::Speaker::Aterm,
                &path,
                &crate::save_words::Failure::Write {
                    stage: crate::native_document_io::AtomicSaveStage::Preflight,
                    cause: Some(crate::native_document_io::SaveCause {
                        kind: std::io::ErrorKind::WouldBlock,
                        touch: crate::native_document_io::SaveTouch::Lock,
                    }),
                },
            )
        )
    );
    assert_eq!(fs::read(&path).unwrap(), b"");
    assert_eq!(app.document_store.dirty(document), Some(true));
    assert_eq!(
        app.document_store.failed_checkpoint_seq(document),
        Some(draft.seq)
    );
    let mut failures = Vec::new();
    if app.document_store.phase(document) != Some(crate::document_store::DocumentPhase::Active) {
        failures.push("ordinary Save failure froze the document without a close intent");
    }
    assert!(app.native_documents.pending_closes.is_empty());
    assert!(app.native_documents.pending_window_closes.is_empty());
    assert!(app.native_documents.pending_quit.is_none());
    let save = app
        .native_runtime
        .commands(instance, view)
        .unwrap()
        .into_iter()
        .find(|command| command.id.as_str() == "editor/save")
        .expect("Manual Save command");
    assert!(save.enabled, "busy failure remains explicitly retryable");

    commit(&mut app, wid, "# edited after refusal\n");
    let edited = app.document_store.snapshot(document).unwrap();
    let retry_text = format!("{}# edited after refusal\n", draft.text);
    assert_eq!(
        app.document_store.failed_checkpoint_seq(document),
        Some(draft.seq)
    );
    if edited.seq <= draft.seq || edited.text.as_ref() != retry_text {
        failures.push("real text input after Save failure did not update the draft");
    }
    assert_eq!(fs::read(&path).unwrap(), b"");
    assert_eq!(app.document_store.dirty(document), Some(true));

    drop(held);
    app.save_document_checkpoint(document, view)
        .expect("Manual Save retry succeeds after lock release");
    let durable = fs::read_to_string(&path).unwrap();
    if durable != retry_text {
        failures.push("Save retry did not persist the edited draft");
    }
    assert_eq!(
        app.document_store.checkpoint_seq(document),
        Some(edited.seq)
    );
    assert_eq!(app.document_store.dirty(document), Some(false));
    assert_eq!(app.document_store.failed_checkpoint_seq(document), None);
    if app.document_store.phase(document) != Some(crate::document_store::DocumentPhase::Active) {
        failures.push("successful Save retry left the document frozen");
    }
    commit(&mut app, wid, "# edited after retry\n");
    let later = app.document_store.snapshot(document).unwrap();
    let later_text = format!("{retry_text}# edited after retry\n");
    if later.seq <= edited.seq || later.text.as_ref() != later_text {
        failures.push("real text input after successful Save retry did not update the draft");
    }
    if app.document_store.dirty(document) != Some(true) {
        failures.push("the edit after successful Save retry was not marked dirty");
    }
    assert_eq!(app.document_store.failed_checkpoint_seq(document), None);
    assert_eq!(fs::read_to_string(&path).unwrap(), durable);
    assert!(app.native_documents.pending_closes.is_empty());
    assert!(app.native_documents.pending_window_closes.is_empty());
    assert!(app.native_documents.pending_quit.is_none());
    let _ = fs::remove_dir_all(dir);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn malformed_startup_manual_save_binds_and_recovers_the_config_service() {
    let dir = fresh_dir("aterm-config-editor-malformed-startup");
    let path = dir.join("aterm.toml");
    crate::test_scratch::write_config(&path, "theme = [\n");
    let mut app = App::headless_for_test();
    assert!(app.native_config_service.bound_logical_path().is_none());
    let wid = WindowId(0);

    app.ensure_and_open_config_editor_path_in_window(wid, &path)
        .unwrap();

    assert_eq!(
        app.native_config_service.bound_logical_path(),
        Some(path.as_path())
    );
    let (instance, view) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();
    app.dispatch_native_event(wid, AppEvent::TextInput(TextInputEvent::SelectAll))
        .unwrap();
    commit(&mut app, wid, "theme = \"Nord\"\n");
    app.save_document_checkpoint(document, view).unwrap();

    assert_eq!(fs::read_to_string(&path).unwrap(), "theme = \"Nord\"\n");
    assert_eq!(
        app.native_config_service.snapshot().text.as_ref(),
        "theme = \"Nord\"\n",
        "valid Manual recovery is admitted immediately despite an unbound startup service"
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn exact_external_observation_refreshes_clean_manual_and_service_bytes() {
    let dir = fresh_dir("aterm-config-editor-exact-observation");
    let path = dir.join("aterm.toml");
    crate::test_scratch::write_config(&path, "font_px = 12.0\n");
    let mut app = App::headless_for_test();
    app.native_config_service =
        crate::native_config_service::VersionedConfigService::load_path(&path).unwrap();
    let wid = WindowId(0);
    app.ensure_and_open_config_editor_path_in_window(wid, &path)
        .unwrap();
    let document = app.native_runtime.config_editor_document().unwrap();

    let external = "# external comment only\nfont_px = 12.0\n";
    crate::test_scratch::write_config(&path, external);
    let observation =
        crate::native_config_service::VersionedConfigService::observe_path(&path, false).unwrap();
    app.refresh_open_config_editor_observation(&observation)
        .unwrap();
    let snapshot = app
        .sync_native_config_external_observation(observation)
        .unwrap()
        .expect("external observation is admitted outside a write");

    assert_eq!(
        app.document_store.snapshot(document).unwrap().text.as_ref(),
        external
    );
    assert_eq!(snapshot.text.as_ref(), external);
    assert_eq!(snapshot.values().unwrap().get("font_px").unwrap(), "12");
    assert_eq!(app.document_store.dirty(document), Some(false));
    let _ = fs::remove_dir_all(dir);
}

#[cfg(unix)]
#[test]
fn byte_identical_observations_rebind_dirty_manual_without_losing_draft() {
    let dir = fresh_dir("aterm-config-editor-equivalent-generation");
    let path = dir.join("aterm.toml");
    let replacement = dir.join("replacement.toml");
    let baseline_text = "font_px = 12.0\n";
    crate::test_scratch::write_config(&path, baseline_text);
    let mut app = App::headless_for_test();
    app.native_config_service =
        crate::native_config_service::VersionedConfigService::load_path(&path).unwrap();
    let wid = WindowId(0);
    app.ensure_and_open_config_editor_path_in_window(wid, &path)
        .unwrap();
    let (instance, view) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();
    let old_disk = app.native_documents.persistence.observed(document).unwrap();

    commit(&mut app, wid, "# local draft\n");
    let draft = app.document_store.snapshot(document).unwrap();
    assert!(app.document_store.dirty(document).unwrap());

    crate::test_scratch::write_config(&replacement, baseline_text);
    fs::rename(&replacement, &path).unwrap();
    let watch_observation =
        crate::native_config_service::VersionedConfigService::observe_path(&path, false).unwrap();
    assert_eq!(
        crate::native_document_io::detect_version_conflict(
            old_disk,
            watch_observation.baseline.observed
        ),
        Some(crate::native_document_io::VersionConflict::Identity)
    );

    app.refresh_open_config_editor_observation(&watch_observation)
        .unwrap();
    let after_watch = app.document_store.snapshot(document).unwrap();
    assert_eq!(after_watch.seq, draft.seq);
    assert_eq!(
        after_watch.text, draft.text,
        "watch rebind must not touch the dirty Manual draft"
    );
    assert_eq!(
        app.native_documents.persistence.observed(document),
        Some(watch_observation.baseline.observed)
    );
    assert!(!app.native_documents.disk_conflicts.contains_key(&document));

    crate::test_scratch::write_config(&replacement, baseline_text);
    fs::rename(&replacement, &path).unwrap();
    let save_observation =
        crate::native_config_service::VersionedConfigService::observe_path(&path, false).unwrap();
    assert_eq!(
        crate::native_document_io::detect_version_conflict(
            watch_observation.baseline.observed,
            save_observation.baseline.observed
        ),
        Some(crate::native_document_io::VersionConflict::Identity)
    );

    app.save_document_checkpoint(document, view).unwrap_err();

    app.refresh_open_config_editor_observation(&save_observation)
        .unwrap();

    let after = app.document_store.snapshot(document).unwrap();
    assert_eq!(after.seq, draft.seq);
    assert_eq!(
        after.text, draft.text,
        "the dirty Manual draft is untouched"
    );
    assert_eq!(app.document_store.dirty(document), Some(true));
    assert_eq!(
        app.native_documents.persistence.observed(document),
        Some(save_observation.baseline.observed),
        "the save baseline advances to the byte-equivalent disk generation"
    );
    assert!(!app.native_documents.disk_conflicts.contains_key(&document));
    let Some(NativeApp::Editor(editor)) = app.native_runtime.app(instance) else {
        panic!("Manual editor remains installed");
    };
    assert!(!editor.disk_conflict);
    let save = app
        .native_runtime
        .commands(instance, view)
        .unwrap()
        .into_iter()
        .find(|command| command.id.as_str() == "editor/save")
        .expect("Save command");
    assert!(save.enabled, "Manual Save is available after safe rebind");

    app.save_document_checkpoint(document, view).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), draft.text.as_ref());
    let _ = fs::remove_dir_all(dir);
}

#[cfg(unix)]
#[test]
fn byte_identical_symlink_recreation_does_not_rebind_manual_capability() {
    use std::os::unix::fs::symlink;

    let dir = fresh_dir("aterm-config-editor-equivalent-relink");
    let target = dir.join("target.toml");
    let logical = dir.join("aterm.toml");
    let old_link = dir.join("aterm.old-link");
    let baseline_text = "font_px = 12.0\n";
    crate::test_scratch::write_config(&target, baseline_text);
    symlink("target.toml", &logical).unwrap();
    let mut app = App::headless_for_test();
    app.native_config_service =
        crate::native_config_service::VersionedConfigService::load_path(&logical).unwrap();
    let wid = WindowId(0);
    app.ensure_and_open_config_editor_path_in_window(wid, &logical)
        .unwrap();
    let (instance, _) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();
    commit(&mut app, wid, "# local draft\n");
    let draft = app.document_store.snapshot(document).unwrap();

    fs::rename(&logical, &old_link).unwrap();
    symlink("target.toml", &logical).unwrap();
    let observation =
        crate::native_config_service::VersionedConfigService::observe_path(&logical, false)
            .unwrap();
    app.refresh_open_config_editor_observation(&observation)
        .unwrap_err();

    assert_eq!(
        app.document_store.snapshot(document).unwrap().text,
        draft.text
    );
    assert_eq!(app.document_store.dirty(document), Some(true));
    assert!(app.native_documents.disk_conflicts.contains_key(&document));
    let Some(NativeApp::Editor(editor)) = app.native_runtime.app(instance) else {
        panic!("Manual editor remains installed");
    };
    assert!(editor.disk_conflict);
    assert_eq!(fs::read_to_string(&target).unwrap(), baseline_text);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn malformed_toml_observation_reaches_manual_but_not_live_config() {
    let dir = fresh_dir("aterm-config-editor-malformed-observation");
    let path = dir.join("aterm.toml");
    let initial = "font_px = 12.0\n";
    crate::test_scratch::write_config(&path, initial);
    let mut app = App::headless_for_test();
    app.native_config_service =
        crate::native_config_service::VersionedConfigService::load_path(&path).unwrap();
    let before = app.native_config_service.snapshot();
    let wid = WindowId(0);
    app.ensure_and_open_config_editor_path_in_window(wid, &path)
        .unwrap();
    let (instance, view) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.config_editor_document().unwrap();

    let malformed = "font_px = [\n";
    crate::test_scratch::write_config(&path, malformed);
    let observation =
        crate::native_config_service::VersionedConfigService::observe_path(&path, false).unwrap();
    app.note_config_watch_candidate(observation.baseline.clone());
    app.prepare_native_config_external_observation(observation);

    assert_eq!(
        app.document_store.snapshot(document).unwrap().text.as_ref(),
        malformed
    );
    assert!(
        app.native_runtime
            .config_editor_save_error(document)
            .is_some()
    );
    let analysis = app
        .native_runtime
        .config_editor_analysis(instance)
        .expect("exact malformed bytes are analyzed in Manual");
    assert!(analysis.has_errors());
    assert!(
        analysis
            .summary()
            .is_some_and(|summary| summary.contains("Ln 1"))
    );
    app.dispatch_native_event(
        wid,
        AppEvent::EditorConfigDiagnosticNavigate { previous: false },
    )
    .unwrap();
    let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view) else {
        panic!("Manual editor view remains installed");
    };
    let buffer = state
        .buffer
        .as_ref()
        .expect("Manual buffer remains attached");
    assert_eq!(
        buffer.primary_selection().head,
        malformed.len() - 1,
        "F8 targets the authored line end, not the synthetic blank line after its newline"
    );
    assert_eq!(buffer.viewport_anchor, 0);
    let after = app.native_config_service.snapshot();
    assert_eq!(after.revision, before.revision);
    assert_eq!(after.text, before.text);
    assert!(app.native_config_service.reconciliation_required());
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn config_publication_preserves_dirty_manual_drafts() {
    let (dir, path, mut app, (instance, _)) =
        open_config("aterm-config-editor-publication", "font_px = 12.0\n");
    let wid = WindowId(0);
    let document = app.native_runtime.document_id(instance).unwrap();

    commit(&mut app, wid, "# local draft\n");
    let local = app.document_store.snapshot(document).unwrap().text.clone();
    let dirty_external = "# changed externally again\nfont_px = 16.0\n";
    crate::test_scratch::write_config(&path, dirty_external);
    let dirty_observation =
        crate::native_config_service::VersionedConfigService::observe_path(&path, false).unwrap();
    app.refresh_open_config_editor_observation(&dirty_observation)
        .unwrap();
    let dirty_snapshot =
        crate::native_config_service::VersionedConfigService::new(dirty_external.to_string())
            .unwrap()
            .snapshot();
    app.publish_native_config_snapshot(&dirty_snapshot);

    assert_eq!(app.document_store.snapshot(document).unwrap().text, local);
    assert_eq!(app.document_store.dirty(document), Some(true));
    let Some(NativeApp::Editor(editor)) = app.native_runtime.app(instance) else {
        panic!("config editor must remain installed");
    };
    assert_eq!(
        editor.recovery_status.as_deref(),
        Some(crate::native_app::DISK_CONFLICT_RECOVERY)
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn numlock_off_keypad_end_moves_the_editor_caret_to_the_line_end() {
    let (dir, _, mut app, (_, view)) = open_config("aterm-config-editor-keypad-end", "win");
    let wid = WindowId(0);
    assert_eq!(
        editor_buffer(&app, view).primary_selection(),
        &crate::native_editor::Selection { anchor: 0, head: 0 },
        "a freshly opened document starts at the top"
    );

    app.on_key(
        wid,
        aterm_window::event::KeyEvent::synthetic_for_test(
            aterm_window::keyboard::PhysicalKey::Code(aterm_window::keyboard::KeyCode::Numpad1),
            aterm_window::keyboard::Key::Named(aterm_window::keyboard::NamedKey::End),
            None,
            aterm_window::keyboard::KeyLocation::Numpad,
            aterm_window::event::ElementState::Pressed,
            false,
        ),
    );
    assert_eq!(
        editor_buffer(&app, view).primary_selection(),
        &crate::native_editor::Selection { anchor: 3, head: 3 },
        "KP_End is End to the editor: the caret is at the line end"
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn accepted_config_samples_select_the_editable_payload_for_immediate_typing() {
    for (case, prefix, insertion, typed, expected) in [
        (
            "shell",
            "shell",
            "shell = \"\"",
            "/bin/zsh",
            "shell = \"/bin/zsh\"",
        ),
        (
            "color",
            "foreground",
            "foreground = \"#ffffff\"",
            "#112233",
            "foreground = \"#112233\"",
        ),
        (
            "list",
            "shell_args",
            "shell_args = []",
            "\"--login\"",
            "shell_args = [\"--login\"]",
        ),
    ] {
        let (dir, _, mut app, (instance, view)) = open_config(
            &format!("aterm-config-editor-post-completion-{case}"),
            prefix,
        );
        let wid = WindowId(0);
        app.dispatch_native_event(wid, AppEvent::EditorCommand(EditorCommand::MoveLineEnd))
            .unwrap();

        let assist = crate::native_config_language::assist(prefix, prefix.len());
        let (index, completion) = assist
            .completions
            .iter()
            .enumerate()
            .find(|(_, completion)| completion.insertion == insertion)
            .expect("shape-specific key completion");
        let document = app.native_runtime.document_id(instance).unwrap();
        let snapshot = app.document_store.snapshot(document).unwrap();
        let context = crate::native_config_language::ConfigCompletionContext::new(
            document.get(),
            snapshot.seq.0,
            prefix.len(),
        );
        let action =
            crate::native_config_language::config_completion_action(context, index, completion);
        let expected_selection = crate::native_editor::Selection {
            anchor: completion.replacement.start + completion.post_insert_selection.start,
            head: completion.replacement.start + completion.post_insert_selection.end,
        };
        app.dispatch_native_event(
            wid,
            AppEvent::Action(crate::native_app::ActionInvocation {
                id: crate::native_ui::ActionId::new(action),
                value: None,
            }),
        )
        .unwrap();
        assert_eq!(
            editor_buffer(&app, view).primary_selection(),
            &expected_selection
        );

        commit(&mut app, wid, typed);
        let completed = app.document_store.snapshot(document).unwrap();
        assert_eq!(completed.text.as_ref(), expected);
        assert!(
            !crate::native_config_language::analyze(&completed.text).has_errors(),
            "immediate typing after {case} completion must retain valid TOML"
        );
        let _ = fs::remove_dir_all(dir);
    }
}

#[test]
fn presented_config_assist_does_not_steal_enter_or_vertical_motion() {
    let source = "theme = \"Default\"\ncursor_blink = true\n";
    let (dir, _, mut app, (instance, view)) =
        open_config("aterm-config-editor-ordinary-keys", source);
    let wid = WindowId(0);
    let document = app.native_runtime.document_id(instance).unwrap();
    app.dispatch_native_event(wid, AppEvent::EditorCommand(EditorCommand::MoveLineEnd))
        .unwrap();
    assert!(app.prepare_native_input_scratch(wid));

    drive_native(
        &mut app,
        key(Key::Named(NamedKey::ArrowDown), Modifiers::empty()),
    );
    let after_down = editor_buffer(&app, view).primary_selection().head;
    assert!(after_down > source.find('\n').unwrap());
    let AppViewState::Editor(state) = app.native_runtime.view_state(view).unwrap() else {
        panic!("config editor view");
    };
    assert!(state.config_completion_interaction.is_none());

    drive_native(
        &mut app,
        key(Key::Named(NamedKey::Enter), Modifiers::empty()),
    );
    assert_eq!(
        app.document_store
            .snapshot(document)
            .unwrap()
            .text
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count(),
        3,
        "Enter remains a newline even while assistance is presented"
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn exact_analysis_drives_tab_before_any_assist_render() {
    let (dir, _, mut app, (instance, view)) =
        open_config("aterm-config-editor-unrendered-tab", "window_theme = ");
    let wid = WindowId(0);
    let document = app.native_runtime.document_id(instance).unwrap();
    app.dispatch_native_event(wid, AppEvent::EditorCommand(EditorCommand::MoveLineEnd))
        .unwrap();
    let context = app
        .editor_config_completion_context(instance, view)
        .unwrap();
    clear_exact_config_assist_cache(&app, instance, context);
    let before = app.document_store.snapshot(document).unwrap();
    let caret = editor_buffer(&app, view).primary_selection().head;

    drive_native(&mut app, key(Key::Named(NamedKey::Tab), Modifiers::empty()));

    let after = app.document_store.snapshot(document).unwrap();
    assert_eq!((after.seq, &after.text), (before.seq, &before.text));
    assert_eq!(editor_buffer(&app, view).primary_selection().head, caret);
    let AppViewState::Editor(state) = app.native_runtime.view_state(view).unwrap() else {
        panic!("config editor view");
    };
    assert_eq!(state.config_completion_interaction, Some(context));
    assert_eq!(state.config_completion_selected, 0);

    for _ in 0..3 {
        drive_native(
            &mut app,
            InputEvent::Key {
                key: Key::Named(NamedKey::Tab),
                mods: Modifiers::empty(),
                base_layout: None,
                layout_text: None,
                event_type: KeyEventType::Repeat,
            },
        );
    }
    let held = app.document_store.snapshot(document).unwrap();
    assert_eq!((held.seq, &held.text), (before.seq, &before.text));
    assert_eq!(editor_buffer(&app, view).primary_selection().head, caret);
    let AppViewState::Editor(state) = app.native_runtime.view_state(view).unwrap() else {
        panic!("config editor view");
    };
    assert_eq!(state.config_completion_interaction, Some(context));
    assert_eq!(state.config_completion_selected, 0);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn ctrl_space_reopens_dismissed_candidate_and_help_only_contexts() {
    for (suffix, source) in [
        ("candidates", "window_theme = "),
        ("help", "matrix_rain.density = 12"),
    ] {
        let (dir, _, mut app, (instance, view)) =
            open_config(&format!("aterm-config-editor-reopen-{suffix}"), source);
        let wid = WindowId(0);
        let document = app.native_runtime.document_id(instance).unwrap();
        app.dispatch_native_event(wid, AppEvent::EditorCommand(EditorCommand::MoveLineEnd))
            .unwrap();
        let context = app
            .editor_config_completion_context(instance, view)
            .unwrap();
        app.native_runtime.clear_config_assist_cache(instance);
        let before = app.document_store.snapshot(document).unwrap();

        drive_native(
            &mut app,
            key(Key::Named(NamedKey::Escape), Modifiers::empty()),
        );
        let AppViewState::Editor(state) = app.native_runtime.view_state(view).unwrap() else {
            panic!("config editor view");
        };
        assert_eq!(state.config_completion_dismissed, Some(context));
        assert!(app.prepare_native_input_scratch(wid));
        assert!(
            app.retained_native_leaf_artifact(wid, view, false)
                .unwrap()
                .compiled
                .semantics
                .iter()
                .all(|node| !node.key.as_str().starts_with("editor/config-completion/")),
            "{suffix}: a dismissed context renders no completions"
        );
        app.native_runtime.clear_config_assist_cache(instance);

        drive_native(&mut app, key(Key::Named(NamedKey::Space), Modifiers::CTRL));
        let after = app.document_store.snapshot(document).unwrap();
        assert_eq!((after.seq, &after.text), (before.seq, &before.text));
        let AppViewState::Editor(state) = app.native_runtime.view_state(view).unwrap() else {
            panic!("config editor view");
        };
        assert_eq!(state.config_completion_dismissed, None);
        assert_eq!(state.config_completion_interaction, Some(context));
        assert_eq!(state.config_completion_selected, 0);
        let _ = fs::remove_dir_all(dir);
    }
}

#[test]
fn tab_and_arrows_reach_a_nonfirst_config_completion_without_a_pointer() {
    let source = "window_theme = ";
    let assist = crate::native_config_language::assist(source, source.len());
    assert!(assist.completions.len() > 1);
    let second = &assist.completions[1];
    let mut expected = source.to_string();
    expected.replace_range(second.replacement.clone(), &second.insertion);
    let (dir, _, mut app, (_, view)) =
        open_config("aterm-config-editor-keyboard-completion", source);
    let wid = WindowId(0);
    app.dispatch_native_event(wid, AppEvent::EditorCommand(EditorCommand::MoveLineEnd))
        .unwrap();
    assert!(app.prepare_native_input_scratch(wid));

    drive_native(&mut app, key(Key::Named(NamedKey::Tab), Modifiers::SHIFT));
    let AppViewState::Editor(state) = app.native_runtime.view_state(view).unwrap() else {
        panic!("config editor view");
    };
    assert!(state.config_completion_interaction.is_none());
    drive_native(&mut app, key(Key::Named(NamedKey::Tab), Modifiers::empty()));
    drive_native(
        &mut app,
        key(Key::Named(NamedKey::ArrowDown), Modifiers::empty()),
    );
    let AppViewState::Editor(state) = app.native_runtime.view_state(view).unwrap() else {
        panic!("config editor view");
    };
    assert_eq!(state.config_completion_selected, 1);
    assert!(state.config_completion_interaction.is_some());
    drive_native(&mut app, key(Key::Named(NamedKey::Tab), Modifiers::SHIFT));
    let document = app.native_runtime.config_editor_document().unwrap();
    assert_eq!(
        app.document_store.snapshot(document).unwrap().text.as_ref(),
        source,
        "Shift-Tab remains reverse focus traversal while completions are active"
    );
    drive_native(&mut app, key(Key::Named(NamedKey::Tab), Modifiers::empty()));

    assert_eq!(
        app.document_store.snapshot(document).unwrap().text.as_ref(),
        expected
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn caret_only_context_change_resets_stale_completion_selection_for_paint_and_input() {
    let source = "window_theme = ";
    let (dir, _, mut app, (instance, view)) =
        open_config("aterm-config-editor-caret-context-selection", source);
    let wid = WindowId(0);
    app.dispatch_native_event(wid, AppEvent::EditorCommand(EditorCommand::MoveLineEnd))
        .unwrap();
    let stale = app
        .editor_config_completion_context(instance, view)
        .unwrap();
    let snapshot = app
        .document_store
        .snapshot(app.native_runtime.document_id(instance).unwrap())
        .unwrap();
    let Some(AppViewState::Editor(state)) = app.native_runtime.view_state_mut(view) else {
        panic!("config editor view");
    };
    state.config_completion_interaction = Some(stale);
    state.config_completion_selected = 7;
    assert!(
        state
            .buffer
            .as_mut()
            .unwrap()
            .pointer_select(&snapshot.text, 0, false, 4),
        "the fixture changes only the caret"
    );
    let current = app
        .editor_config_completion_context(instance, view)
        .unwrap();
    assert_ne!(current, stale);

    assert!(app.prepare_native_input_scratch(wid));
    let compiled = &app
        .retained_native_leaf_artifact(wid, view, false)
        .unwrap()
        .compiled;
    let selected = compiled
        .semantics
        .iter()
        .filter(|node| {
            node.key.as_str().starts_with("editor/config-completion/")
                && node.state.is_some_and(|state| state.selected)
        })
        .map(|node| node.key.as_str())
        .collect::<Vec<_>>();
    assert!(selected.is_empty(), "{selected:?}");

    drive_native(&mut app, key(Key::Named(NamedKey::Tab), Modifiers::empty()));
    let AppViewState::Editor(state) = app.native_runtime.view_state(view).unwrap() else {
        panic!("config editor view");
    };
    assert_eq!(state.config_completion_interaction, Some(current));
    assert_eq!(state.config_completion_selected, 0);
    assert_eq!(editor_buffer(&app, view).primary_selection().head, 0);
    assert_eq!(
        app.document_store
            .snapshot(snapshot.id)
            .unwrap()
            .text
            .as_ref(),
        source
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn pending_editor_chord_owns_escape_tab_and_ctrl_space_before_config_assist() {
    for (suffix, terminator) in [
        (
            "escape",
            key(Key::Named(NamedKey::Escape), Modifiers::empty()),
        ),
        ("tab", key(Key::Named(NamedKey::Tab), Modifiers::empty())),
        (
            "ctrl-space",
            key(Key::Named(NamedKey::Space), Modifiers::CTRL),
        ),
    ] {
        let source = "win";
        let (dir, _, mut app, (_, view)) = open_config(
            &format!("aterm-config-editor-chord-assist-{suffix}"),
            source,
        );
        let wid = WindowId(0);
        app.dispatch_native_event(wid, AppEvent::EditorCommand(EditorCommand::MoveLineEnd))
            .unwrap();
        assert!(app.prepare_native_input_scratch(wid));
        assert!(
            app.retained_native_leaf_artifact(wid, view, false)
                .unwrap()
                .compiled
                .semantic(&crate::native_ui::UiKey::new("editor/config-assist"))
                .is_some(),
            "the regression starts with visible config assistance"
        );

        drive_native(&mut app, key(Key::Character('x'), Modifiers::CTRL));
        assert!(editor_buffer(&app, view).chord_pending());
        assert!(app.prepare_native_input_scratch(wid));
        assert!(
            app.retained_native_leaf_artifact(wid, view, false)
                .unwrap()
                .compiled
                .semantic(&crate::native_ui::UiKey::new("editor/config-assist"))
                .is_none(),
            "a pending editor chord suppresses config assistance"
        );

        drive_native(&mut app, terminator);
        assert!(
            !editor_buffer(&app, view).chord_pending(),
            "{suffix} is reduced as the second chord stroke"
        );
        let AppViewState::Editor(state) = app.native_runtime.view_state(view).unwrap() else {
            panic!("config editor view");
        };
        assert!(state.config_completion_interaction.is_none());
        assert_eq!(
            app.document_store
                .snapshot(app.native_runtime.config_editor_document().unwrap())
                .unwrap()
                .text
                .as_ref(),
            source
        );

        if suffix == "escape" {
            drive_native(&mut app, isearch_key());
            assert!(matches!(
                &editor_buffer(&app, view).minibuffer,
                Minibuffer::Search { .. }
            ));
        }
        let _ = fs::remove_dir_all(dir);
    }
}

#[test]
fn config_assist_yields_to_search_and_goto_minibuffers() {
    let (dir, _, mut app, (_, view)) = open_config("aterm-config-editor-minibuffer-assist", "win");
    let wid = WindowId(0);
    app.dispatch_native_event(wid, AppEvent::EditorCommand(EditorCommand::MoveLineEnd))
        .unwrap();
    assert!(app.prepare_native_input_scratch(wid));
    assert!(
        app.retained_native_leaf_artifact(wid, view, false)
            .unwrap()
            .compiled
            .semantic(&crate::native_ui::UiKey::new("editor/config-assist"))
            .is_some()
    );

    for command in [EditorCommand::IncrementalSearch, EditorCommand::GotoLine] {
        app.dispatch_native_event(wid, AppEvent::EditorCommand(command))
            .unwrap();
        assert!(app.prepare_native_input_scratch(wid));
        assert!(
            app.retained_native_leaf_artifact(wid, view, false)
                .unwrap()
                .compiled
                .semantic(&crate::native_ui::UiKey::new("editor/config-assist"))
                .is_none()
        );
        drive_native(
            &mut app,
            key(Key::Named(NamedKey::Escape), Modifiers::empty()),
        );
        let AppViewState::Editor(state) = app.native_runtime.view_state(view).unwrap() else {
            panic!("config editor view");
        };
        assert!(
            state
                .buffer
                .as_ref()
                .is_some_and(|buffer| !buffer.minibuffer_active()),
            "one Escape closes the active editor minibuffer"
        );
    }
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn settings_manual_handoff_reveals_an_existing_key_and_adds_an_absent_one() {
    let source = format!("{}shell = \"/bin/zsh\"\n", "# spacer\n".repeat(500));
    let (dir, path, mut app, (_, original_view)) =
        open_config("aterm-config-editor-target-handoff", &source);
    let wid = WindowId(0);

    app.ensure_and_open_config_editor_path_at_in_window(
        wid,
        &path,
        Some(&crate::native_app::ConfigEditorTarget::Key(
            "shell".to_string(),
        )),
    )
    .unwrap();
    let (_, reopened_view) = app.active_native_view(wid).unwrap();
    assert_eq!(reopened_view, original_view, "the canonical tab is reused");
    let value_start = source.find("\"/bin/zsh\"").unwrap();
    let buffer = editor_buffer(&app, reopened_view);
    assert_eq!(
        buffer.primary_selection().range(),
        value_start..value_start + "\"/bin/zsh\"".len()
    );
    assert!(buffer.viewport_anchor > 0, "the distant target is revealed");

    app.ensure_and_open_config_editor_path_at_in_window(
        wid,
        &path,
        Some(&crate::native_app::ConfigEditorTarget::Key(
            "gpu".to_string(),
        )),
    )
    .unwrap();
    let (instance, view) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();
    let text = app.document_store.snapshot(document).unwrap().text;
    let added = crate::native_config_language::config_key_source_range(&text, "gpu")
        .expect("gpu is in the buffer now");
    assert!(text.starts_with(&source), "nothing else moved: {text:?}");
    let AppViewState::Editor(state) = app.native_runtime.view_state(view).unwrap() else {
        panic!("config editor view")
    };
    let buffer = state.buffer.as_ref().unwrap();
    assert_eq!(
        buffer.minibuffer,
        crate::native_editor::Minibuffer::Inactive
    );
    assert_eq!(buffer.primary_selection().range(), added);
    assert!(
        state
            .status
            .as_deref()
            .unwrap()
            .starts_with("Added gpu at its default"),
        "{:?}",
        state.status
    );
    assert_eq!(state.reveal_status, state.status, "the footer says it");
    assert!(app.native_runtime.config_editor_enabled(instance));
    let _ = fs::write(&path, "");
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn opening_a_key_with_no_default_awaits_its_value() {
    let dir = std::env::temp_dir().join(format!(
        "aterm-config-editor-needs-value-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("aterm.toml");
    let source = "agents_auto_prime = false\n";
    crate::test_scratch::write_config(&path, source);
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    for (key, line) in [
        ("shell", "shell = "),
        (
            "sparkle_words.custom.graphic.collection",
            "[[sparkle_words.custom]]\ngraphic.collection = ",
        ),
    ] {
        app.ensure_and_open_config_editor_path_at_in_window(
            wid,
            &path,
            Some(&crate::native_app::ConfigEditorTarget::Key(key.to_string())),
        )
        .unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        let text = app.document_store.snapshot(document).unwrap().text;
        let at = text.find(line).unwrap_or_else(|| panic!("{key}: {text:?}"));
        let AppViewState::Editor(state) = app.native_runtime.view_state(view).unwrap() else {
            panic!("config editor view")
        };
        let buffer = state.buffer.as_ref().unwrap();
        assert_eq!(
            buffer.primary_selection().range(),
            at + line.len()..at + line.len(),
            "{key}: the caret waits at the value"
        );
        let status = state.status.clone().unwrap();
        assert!(
            status.contains(&format!("type a value for {key}"))
                || status.contains(&format!("Type a value for {key}")),
            "{status}"
        );
        assert!(!status.contains("default"), "{status}");
        assert!(
            app.native_runtime
                .config_editor_save_error(document)
                .is_some(),
            "{key}: Save waits for a value"
        );
        app.dispatch_editor_event(
            instance,
            view,
            &AppEvent::EditorCommand(crate::native_editor::EditorCommand::Undo),
        )
        .unwrap();
        assert_eq!(
            &*app.document_store.snapshot(document).unwrap().text,
            source,
            "{key}: Undo takes the line back out"
        );
    }
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn an_absent_key_is_added_where_toml_reads_it() {
    let source = "agents_auto_prime = false\n[update]\nenabled = false\n";
    let apply = |key: &str| {
        let (edit, added_as) = absent_key_edit(source, key).expect("an edit");
        let mut text = source.to_string();
        text.replace_range(edit.replacement.clone(), &edit.insertion);
        (text, added_as)
    };
    let (text, added_as) = apply(crate::prefs::EDIT_SCROLLBACK);
    assert_eq!(added_as, AbsentKeyAdded::AtDefault);
    let key_at = text.find("scrollback").unwrap();
    assert!(
        key_at < text.find("[update]").unwrap(),
        "a root key stays out of [update]: {text}"
    );
    let parsed: aterm_toml::Value = aterm_toml::from_str(&text).unwrap();
    assert!(
        parsed.get(crate::prefs::EDIT_SCROLLBACK).is_some(),
        "{text}"
    );
    let (text, _) = apply("update.auto_apply");
    let parsed: aterm_toml::Value = aterm_toml::from_str(&text).unwrap();
    assert!(parsed["update"]["auto_apply"].as_bool().is_some(), "{text}");
    assert_eq!(parsed["update"]["enabled"].as_bool(), Some(false));
    for key in ["harness", "keybindings"] {
        let (edit, added_as) = absent_key_edit(source, key).expect("an edit");
        assert_eq!(added_as, AbsentKeyAdded::Table, "{key}");
        let mut text = source.to_string();
        text.replace_range(edit.replacement.clone(), &edit.insertion);
        let caret = edit.replacement.start + edit.post_insert_selection.start;
        assert!(
            text[..caret].ends_with(&format!("\n[{key}]\n")),
            "{key}: the caret is under the header: {text:?}"
        );
        let mut typed = text.clone();
        let entry = if key == "harness" {
            "enabled = false\n"
        } else {
            "\"ctrl+t\" = \"new_tab\"\n"
        };
        typed.insert_str(caret, entry);
        let parsed: aterm_toml::Value = aterm_toml::from_str(&typed).unwrap();
        assert!(
            parsed[key].as_table().is_some_and(|t| t.len() == 1),
            "{typed}"
        );
        assert_eq!(
            parsed["update"].as_table().map(|t| t.len()),
            Some(1),
            "{typed}"
        );
        assert!(
            crate::app_config::Config::parse(&text).is_ok(),
            "{key}: an empty table loads: {text}"
        );
    }
    assert!(absent_key_edit(source, "sparkle_words.custom").is_none());
}

#[test]
fn an_absent_number_comes_in_at_its_default_as_settings_writes_it() {
    let source = "agents_auto_prime = false\n";
    let line_height = format!("line_height = {}", crate::app_config::DEFAULT_LINE_HEIGHT);
    for (key, line) in [
        (crate::prefs::EDIT_WINDOW_PADDING, "window_padding = 12"),
        (crate::prefs::EDIT_LINE_HEIGHT, line_height.as_str()),
        ("output_streak.idle_secs", "idle_secs = 10"),
        ("output_streak.intensity", "intensity = 1"),
        (crate::prefs::EDIT_WALLPAPER_DIM, "wallpaper_dim = 0.3"),
    ] {
        let (edit, added_as) = absent_key_edit(source, key).expect("an edit");
        let mut text = source.to_string();
        text.replace_range(edit.replacement.clone(), &edit.insertion);
        assert_eq!(added_as, AbsentKeyAdded::AtDefault, "{key}: {text}");
        assert!(text.lines().any(|at| at == line), "{key}: {text}");
        crate::app_config::Config::parse(&text).unwrap_or_else(|e| panic!("{text}: {e}"));
    }
}

#[test]
fn an_absent_key_is_added_at_its_default_or_awaits_a_value() {
    let source = "agents_auto_prime = false\n[update]\nenabled = false\n";
    let apply = |key: &str| {
        let (edit, added_as) = absent_key_edit(source, key).expect("an edit");
        let mut text = source.to_string();
        text.replace_range(edit.replacement.clone(), &edit.insertion);
        let caret = edit.replacement.start + edit.post_insert_selection.start;
        (text, added_as, caret)
    };
    let (text, added_as, _) = apply("disk.auto_free_gib");
    assert_eq!(added_as, AbsentKeyAdded::AtDefault, "{text}");
    let parsed: aterm_toml::Value = aterm_toml::from_str(&text).unwrap();
    assert_eq!(
        parsed["disk"]["auto_free_gib"].as_integer(),
        i64::try_from(aterm_agent::harness::disk::Config::default().auto_free_gib).ok(),
        "{text}"
    );
    let (text, added_as, _) = apply("disk.apply");
    assert_eq!(added_as, AbsentKeyAdded::AtDefault, "{text}");
    let parsed: aterm_toml::Value = aterm_toml::from_str(&text).unwrap();
    assert_eq!(parsed["disk"]["apply"].as_bool(), Some(false), "{text}");
    let (text, added_as, _) = apply("harness.continue_text");
    assert_eq!(added_as, AbsentKeyAdded::AtDefault, "{text}");
    let parsed: aterm_toml::Value = aterm_toml::from_str(&text).unwrap();
    assert_eq!(
        parsed["harness"]["continue_text"].as_str(),
        Some(
            aterm_agent::supervise::SupervisorConfig::default()
                .continue_text
                .as_str()
        ),
        "{text}"
    );
    for key in [
        crate::prefs::EDIT_GPU,
        crate::prefs::EDIT_SCROLLBACK,
        "matrix_rain.hue",
    ] {
        let (text, added_as, _) = apply(key);
        assert_eq!(added_as, AbsentKeyAdded::AtDefault, "{key}: {text}");
    }
    // The font size's default is the platform's automatic size, which an
    // explicit size draws exactly as.
    let (text, added_as, _) = apply(crate::prefs::EDIT_FONT_PX);
    assert_eq!(added_as, AbsentKeyAdded::AtDefault, "{text}");
    assert!(
        text.lines()
            .any(|line| line == format!("font_px = {}", crate::FONT_PX)),
        "{text}"
    );
    for key in [
        "shell",
        "shell_args",
        crate::prefs::EDIT_CURSOR_NYAN_SPRITE,
        crate::prefs::EDIT_FONT_FAMILY,
        "font_features",
        crate::prefs::EDIT_FALLBACK_FONTS,
        "font_variation",
        "sparkle_words.lexicon",
        crate::prefs::EDIT_WALLPAPER,
        crate::prefs::EDIT_THEME,
    ] {
        let (text, added_as, caret) = apply(key);
        assert_eq!(
            added_as,
            AbsentKeyAdded::NeedsValue { record: None },
            "{key}: {text}"
        );
        let line_start = text[..caret].rfind('\n').map_or(0, |at| at + 1);
        let line_end = text[caret..].find('\n').map_or(text.len(), |at| caret + at);
        assert!(
            text[line_start..caret].ends_with("= ") && caret == line_end,
            "{key}: the caret waits at the value: {text:?}"
        );
        assert!(
            crate::native_config_language::analyze(&text).has_errors(),
            "{key}: the line does not parse until a value is typed, so Save waits"
        );
    }
    for key in [
        "sparkle_words.custom.graphic.collection",
        "sparkle_words.custom.burst.chance",
        "sparkle_words.custom.words",
    ] {
        let root = crate::native_config_language::config_key_record_root(key).unwrap();
        let (text, added_as, caret) = apply(key);
        assert_eq!(
            added_as,
            AbsentKeyAdded::NeedsValue { record: Some(root) },
            "{key}: {text}"
        );
        assert!(text.contains(&format!("[[{root}]]\n")), "{key}: {text}");
        assert!(text[..caret].ends_with(" = "), "{key}: {text:?}");
        let mut typed = text.clone();
        typed.insert_str(caret, "\"x\"");
        let parsed: aterm_toml::Value = aterm_toml::from_str(&typed).unwrap();
        let mut node = &parsed;
        for part in root.split('.') {
            node = &node[part];
        }
        assert!(
            node.as_array().is_some_and(|records| records.len() == 1),
            "{key}: {typed}"
        );
    }
    assert!(
        absent_key_edit(
            "[[sparkle_words.custom]]\nwords = [\"ship\"]\n",
            "sparkle_words.custom.graphic.collection"
        )
        .is_none()
    );
    let findings = |text: &str| {
        crate::native_config_language::analyze(text)
            .diagnostics
            .len()
    };
    for entry in crate::native_config_language::config_schema() {
        let Some((edit, added_as)) = absent_key_edit(source, entry.key) else {
            continue;
        };
        let mut text = source.to_string();
        text.replace_range(edit.replacement.clone(), &edit.insertion);
        match added_as {
            AbsentKeyAdded::AtDefault => {
                assert!(
                    crate::app_config::Config::parse(&text).is_ok(),
                    "{}: loads\n{text}",
                    entry.key
                );
                assert!(
                    findings(&text) <= findings(source),
                    "{}: no new finding\n{text}",
                    entry.key
                );
            }
            AbsentKeyAdded::NeedsValue { .. } => {
                let caret = edit.replacement.start + edit.post_insert_selection.start;
                assert!(
                    text[..caret].ends_with("= "),
                    "{}: no value written\n{text}",
                    entry.key
                );
            }
            AbsentKeyAdded::Table => {
                assert!(
                    crate::app_config::Config::parse(&text).is_ok()
                        && findings(&text) <= findings(source),
                    "{}: an empty table changes nothing\n{text}",
                    entry.key
                );
            }
        }
    }
}

/// A change of how many rows the editor shows (a resize, a footer that
/// wraps) is not a document change: the diagnostic cycle keeps its place,
/// so Previous Problem after it still steps back from the problem F8
/// reached. Published as a document change, it reset the cycle to the
/// first problem, and Previous wrapped to the last.
#[test]
fn a_viewport_change_keeps_the_diagnostic_cycle_in_place() {
    let dir = fresh_dir("aterm-config-editor-cycle-viewport");
    let path = dir.join("aterm.toml");
    crate::test_scratch::write_config(
        &path,
        "future_first_setting = 1\nfuture_second_setting = 2\n",
    );
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.ensure_and_open_config_editor_path_in_window(wid, &path)
        .unwrap();
    let (instance, view) = app.active_native_view(wid).unwrap();
    let analysis = app
        .native_runtime
        .config_editor_analysis(instance)
        .unwrap()
        .clone();
    assert_eq!(analysis.diagnostic_count(), 2);
    let first = analysis.diagnostic_at(0).unwrap().bytes.start;
    let second = analysis.diagnostic_at(1).unwrap().bytes.start;
    drive_native(&mut app, key(Key::Named(NamedKey::F8), Modifiers::empty()));
    assert_eq!(editor_buffer(&app, view).primary_selection().head, second);
    let rows = editor_buffer(&app, view).viewport_lines();
    app.dispatch_native_event(
        wid,
        AppEvent::EditorViewportChanged {
            visible_lines: rows.saturating_sub(2).max(1),
        },
    )
    .unwrap();
    app.dispatch_native_event(
        wid,
        AppEvent::Action(crate::native_app::ActionInvocation {
            id: crate::native_ui::ActionId::new("editor/config-problem-previous"),
            value: None,
        }),
    )
    .unwrap();
    assert_eq!(editor_buffer(&app, view).primary_selection().head, first);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn f8_cycles_offscreen_problems_and_keeps_the_full_semantic_message() {
    let dir = fresh_dir("aterm-config-editor-problem-cycle");
    let path = dir.join("aterm.toml");
    let long_key = format!("future_{}", "diagnostic_".repeat(24));
    let source = format!(
        "future_first_setting = 1\n{}{} = 2\n",
        "# spacer\n".repeat(500),
        long_key
    );
    crate::test_scratch::write_config(&path, &source);
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.ensure_and_open_config_editor_path_in_window(wid, &path)
        .unwrap();
    let (instance, view) = app.active_native_view(wid).unwrap();
    let analysis = app
        .native_runtime
        .config_editor_analysis(instance)
        .unwrap()
        .clone();
    assert_eq!(analysis.diagnostic_count(), 2);
    let first = analysis.diagnostic_at(0).unwrap().bytes.start;
    let second = analysis.diagnostic_at(1).unwrap().bytes.start;

    assert!(app.prepare_native_input_scratch(wid));
    let controls = app
        .retained_native_leaf_artifact(wid, view, false)
        .unwrap()
        .compiled;
    for (key, action) in [
        (
            "editor/config-problem-previous-button",
            "editor/config-problem-previous",
        ),
        (
            "editor/config-problem-next-button",
            "editor/config-problem-next",
        ),
    ] {
        let semantic = controls
            .semantic(&crate::native_ui::UiKey::new(key))
            .expect("visible config problem navigation");
        assert_eq!(
            semantic
                .action
                .as_ref()
                .map(crate::native_ui::ActionId::as_str),
            Some(action)
        );
        assert!(semantic.state.is_some_and(|state| state.enabled));
    }

    drive_native(&mut app, key(Key::Named(NamedKey::F8), Modifiers::empty()));
    assert_eq!(editor_buffer(&app, view).primary_selection().head, second);
    assert!(app.prepare_native_input_scratch(wid));
    let artifact = app.retained_native_leaf_artifact(wid, view, false).unwrap();
    let status = artifact
        .compiled
        .paint
        .iter()
        .find_map(|node| match &node.content {
            crate::native_ui::UiContent::TextViewport(spec) => Some(spec),
            _ => None,
        })
        .unwrap();
    let semantic = status.semantic_status.as_deref().unwrap();
    assert!(semantic.contains(&long_key));
    assert_ne!(status.status.as_deref(), Some(semantic));

    app.dispatch_native_event(
        wid,
        AppEvent::Action(crate::native_app::ActionInvocation {
            id: crate::native_ui::ActionId::new("editor/config-problem-previous"),
            value: None,
        }),
    )
    .unwrap();
    assert_eq!(
        editor_buffer(&app, view).primary_selection().head,
        first,
        "Previous Problem button converges with Shift-F8"
    );

    app.dispatch_native_event(
        wid,
        AppEvent::Action(crate::native_app::ActionInvocation {
            id: crate::native_ui::ActionId::new("editor/config-problem-next"),
            value: None,
        }),
    )
    .unwrap();
    assert_eq!(
        editor_buffer(&app, view).primary_selection().head,
        second,
        "Next Problem button converges with F8"
    );

    drive_native(&mut app, key(Key::Named(NamedKey::F8), Modifiers::SHIFT));
    assert_eq!(editor_buffer(&app, view).primary_selection().head, first);

    let document = app.native_runtime.document_id(instance).unwrap();
    let original = app.document_store.snapshot(document).unwrap();
    let live_config = app.native_config_service.snapshot();
    let navigation = [
        (
            "editor/config-problem-previous-button",
            "editor/config-problem-previous",
        ),
        (
            "editor/config-problem-next-button",
            "editor/config-problem-next",
        ),
    ];
    let position = |app: &App| {
        let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view) else {
            panic!("Manual remains attached");
        };
        let buffer = state.buffer.as_ref().unwrap();
        (
            buffer.minibuffer.clone(),
            buffer.selections.clone(),
            buffer.primary,
            buffer.viewport_anchor,
            state.config_diagnostic_selected,
        )
    };
    let press_f8 = |app: &mut App, previous: bool| {
        app.windows.get_mut(&wid).unwrap().mods = if previous {
            aterm_window::keyboard::ModifiersState::SHIFT
        } else {
            aterm_window::keyboard::ModifiersState::empty()
        };
        for state in [
            aterm_window::event::ElementState::Pressed,
            aterm_window::event::ElementState::Released,
        ] {
            app.on_key(
                wid,
                aterm_window::event::KeyEvent::synthetic_for_test(
                    aterm_window::keyboard::PhysicalKey::Code(aterm_window::keyboard::KeyCode::F8),
                    aterm_window::keyboard::Key::Named(aterm_window::keyboard::NamedKey::F8),
                    None,
                    aterm_window::keyboard::KeyLocation::Standard,
                    state,
                    false,
                ),
            );
        }
    };
    let mut prompt_failures = Vec::new();
    for command in [
        EditorCommand::GotoLine,
        EditorCommand::ExecuteCommand,
        EditorCommand::SwitchBuffer,
        EditorCommand::KillBuffer,
        EditorCommand::IncrementalSearch,
    ] {
        app.dispatch_native_event(wid, AppEvent::EditorSetSelection { anchor: 1, head: 5 })
            .unwrap();
        app.dispatch_native_event(wid, AppEvent::EditorCommand(command))
            .unwrap();
        app.dispatch_native_event(
            wid,
            AppEvent::EditorInput(EditorInput::Text(
                if command == EditorCommand::GotoLine {
                    "2"
                } else {
                    "first"
                }
                .into(),
            )),
        )
        .unwrap();
        assert!(matches!(
            (command, &editor_buffer(&app, view).minibuffer),
            (EditorCommand::GotoLine, Minibuffer::GotoLine { .. })
                | (EditorCommand::ExecuteCommand, Minibuffer::Command { .. })
                | (EditorCommand::SwitchBuffer, Minibuffer::Buffer { .. })
                | (EditorCommand::KillBuffer, Minibuffer::Prompt { .. })
                | (EditorCommand::IncrementalSearch, Minibuffer::Search { .. })
        ));
        let commands = app.native_runtime.commands(instance, view).unwrap();
        let compiled = app.compiled_native_ui(wid).unwrap();
        for (key, action) in navigation {
            if commands
                .iter()
                .find(|item| item.id.as_str() == action)
                .unwrap()
                .enabled
                || compiled
                    .semantic(&crate::native_ui::UiKey::new(key))
                    .unwrap()
                    .state
                    .unwrap()
                    .enabled
            {
                prompt_failures.push(format!("{command:?}: {action} offered during the prompt"));
            }
        }
        // Keep a valid scrolled viewport whose caret is outside it: refusing
        // a document gesture must not reveal that caret behind the prompt.
        let offscreen = source.match_indices("# spacer\n").nth(250).unwrap().0;
        let Some(AppViewState::Editor(state)) = app.native_runtime.view_state_mut(view) else {
            panic!("Manual remains attached");
        };
        state.buffer.as_mut().unwrap().viewport_anchor = offscreen;
        assert!(editor_buffer(&app, view).primary_selection().head < offscreen);
        let retained = position(&app);
        for previous in [false, true] {
            press_f8(&mut app, previous);
            if position(&app) != retained {
                prompt_failures.push(format!(
                    "{command:?}: F8 previous={previous} moved prompt-owned state"
                ));
            }
        }
        for (key, action) in navigation {
            if app
                .act_app(crate::app_control::ActRequest {
                    view,
                    ui_key: key.into(),
                    action: action.into(),
                    value: None,
                })
                .is_ok()
            {
                prompt_failures.push(format!("{command:?}: {action} control did not refuse"));
            }
            if position(&app) != retained {
                prompt_failures.push(format!(
                    "{command:?}: {action} control moved prompt-owned state"
                ));
            }
            let target = crate::palette::NativeCommandTarget {
                window: wid,
                instance,
                view,
                generation: app.native_runtime.view_generation(view).unwrap(),
                action: crate::native_ui::ActionId::new(action),
            };
            if app.activate_native_palette_target(&target).is_ok() {
                prompt_failures.push(format!("{command:?}: {action} palette did not refuse"));
            }
            if position(&app) != retained {
                prompt_failures.push(format!(
                    "{command:?}: {action} palette moved prompt-owned state"
                ));
            }
        }
        for previous in [false, true] {
            app.dispatch_native_event(wid, AppEvent::EditorConfigDiagnosticNavigate { previous })
                .unwrap();
            if position(&app) != retained {
                prompt_failures.push(format!(
                    "{command:?}: direct previous={previous} moved prompt-owned state"
                ));
            }
        }
        let after_prompt = app.document_store.snapshot(document).unwrap();
        assert_eq!(after_prompt.seq, original.seq);
        assert_eq!(after_prompt.text, original.text);
        assert_eq!(app.document_store.dirty(document), Some(false));
        assert_eq!(fs::read_to_string(&path).unwrap(), source);
        let current_config = app.native_config_service.snapshot();
        assert_eq!(current_config.revision, live_config.revision);
        assert_eq!(current_config.text, live_config.text);

        app.dispatch_native_event(wid, AppEvent::EditorCommand(EditorCommand::Abort))
            .unwrap();
        assert!(!editor_buffer(&app, view).minibuffer_active());
        let commands = app.native_runtime.commands(instance, view).unwrap();
        let compiled = app.compiled_native_ui(wid).unwrap();
        for (key, action) in navigation {
            assert!(
                commands
                    .iter()
                    .find(|item| item.id.as_str() == action)
                    .unwrap()
                    .enabled
            );
            assert!(
                compiled
                    .semantic(&crate::native_ui::UiKey::new(key))
                    .unwrap()
                    .state
                    .unwrap()
                    .enabled
            );
        }
        let current = position(&app).4;
        press_f8(&mut app, false);
        assert_eq!(
            editor_buffer(&app, view).primary_selection().head,
            if current == 0 { second } else { first },
            "navigation resumes after cancelling {command:?}"
        );
    }
    assert!(prompt_failures.is_empty(), "{prompt_failures:#?}");
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn ordinary_editor_buffer_enter_still_inserts_a_newline() {
    let dir = fresh_dir("aterm-editor-ordinary-enter");
    let path = dir.join("notes.txt");
    fs::write(&path, "abc").unwrap();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.open_document_tab(AppKind::Editor, &file_uri(&path))
        .unwrap();
    let (instance, view) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();
    app.dispatch_native_event(wid, AppEvent::EditorCommand(EditorCommand::MoveLineEnd))
        .unwrap();
    let state = app.native_runtime.view_state_mut(view).unwrap();
    state.common_mut().last_focus = Some(crate::native_ui::UiKey::new("editor/buffer"));

    drive_native(
        &mut app,
        key(Key::Named(NamedKey::Enter), Modifiers::empty()),
    );

    assert_eq!(
        app.document_store.snapshot(document).unwrap().text.as_ref(),
        "abc\n"
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn editor_set_selection_replaces_the_primary_without_mutating_document() {
    let dir = fresh_dir("aterm-editor-set-selection");
    let path = dir.join("unicode.txt");
    fs::write(&path, "aéz").unwrap();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.open_document_tab(AppKind::Editor, &file_uri(&path))
        .unwrap();
    let (instance, view) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();
    let before = app.document_store.snapshot(document).unwrap();
    let AppViewState::Editor(state) = app.native_runtime.view_state_mut(view).unwrap() else {
        panic!("editor view");
    };
    let buffer = state.buffer.as_mut().unwrap();
    buffer.selections = vec![Selection::caret(0), Selection::caret(before.text.len())];
    buffer.primary = 1;

    app.dispatch_native_event(wid, AppEvent::EditorSetSelection { anchor: 1, head: 3 })
        .unwrap();

    let buffer = editor_buffer(&app, view);
    assert_eq!(buffer.selections, vec![Selection { anchor: 1, head: 3 }]);
    assert_eq!(buffer.primary, 0);
    let selected = app.document_store.snapshot(document).unwrap();
    assert_eq!(selected.seq, before.seq);
    assert_eq!(selected.text, before.text);
    assert_eq!(app.document_store.dirty(document), Some(false));

    app.dispatch_native_event(wid, AppEvent::EditorSetSelection { anchor: 1, head: 2 })
        .unwrap();
    assert_eq!(
        editor_buffer(&app, view).selections,
        vec![Selection { anchor: 1, head: 3 }],
        "a non-UTF-8-boundary accessibility selection is rejected"
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn stale_rendered_config_completion_cannot_retarget_a_new_caret() {
    let (dir, _, mut app, (instance, view)) =
        open_config("aterm-config-editor-stale-completion", "win\nthe");
    let wid = WindowId(0);
    let document = app.native_runtime.document_id(instance).unwrap();
    app.dispatch_native_event(wid, AppEvent::EditorCommand(EditorCommand::MoveLineEnd))
        .unwrap();

    assert!(app.prepare_native_input_scratch(wid));
    let stale_action = app
        .retained_native_leaf_artifact(wid, view, false)
        .expect("compiled native config editor")
        .compiled
        .semantics
        .iter()
        .find(|node| node.label.starts_with("Insert window_theme"))
        .and_then(|node| node.action.clone())
        .expect("rendered window-theme completion action");
    let stale_index = stale_action
        .as_str()
        .strip_prefix(crate::native_config_language::CONFIG_COMPLETION_ACTION_PREFIX)
        .and_then(|suffix| suffix.split('/').next())
        .and_then(|index| index.parse::<usize>().ok())
        .expect("bound completion index");
    let before_move = app.document_store.snapshot(document).unwrap();

    app.dispatch_native_event(wid, AppEvent::EditorCommand(EditorCommand::MoveLineDown))
        .unwrap();
    let moved = app.document_store.snapshot(document).unwrap();
    assert_eq!(moved.seq, before_move.seq, "caret movement does not edit");
    let caret = match app.native_runtime.view_state(view).unwrap() {
        AppViewState::Editor(state) => state.buffer.as_ref().unwrap().primary_selection().head,
        _ => panic!("config editor view"),
    };
    let current = crate::native_config_language::assist(&moved.text, caret);
    assert!(
        current.completions[stale_index]
            .insertion
            .starts_with("theme ="),
        "the same index now denotes a different visible candidate"
    );

    app.dispatch_native_event(
        wid,
        AppEvent::Action(crate::native_app::ActionInvocation {
            id: stale_action,
            value: None,
        }),
    )
    .unwrap();
    let rejected = app.document_store.snapshot(document).unwrap();
    assert_eq!(rejected.seq, moved.seq);
    assert_eq!(rejected.text, moved.text);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn duplicate_basenames_refresh_inactive_tab_titles_and_keep_exact_uri_tooltips() {
    let dir = fresh_dir("aterm-app-document-identities");
    let one_dir = dir.join("one");
    let two_dir = dir.join("two");
    fs::create_dir_all(&one_dir).unwrap();
    fs::create_dir_all(&two_dir).unwrap();
    let one = one_dir.join("README.md");
    let two = two_dir.join("README.md");
    fs::write(&one, "# One\n").unwrap();
    fs::write(&two, "# Two\n").unwrap();
    let one_uri = file_uri(&one);
    let two_uri = file_uri(&two);
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.open_document_tab(AppKind::Markdown, &one_uri).unwrap();
    app.open_document_tab(AppKind::Editor, &one_uri).unwrap();
    app.open_document_tab(AppKind::Markdown, &two_uri).unwrap();

    let documents = app.windows[&wid]
        .tab_set
        .tabs()
        .iter()
        .filter(|tab| tab.presentation.title.starts_with("README.md"))
        .map(|tab| {
            (
                tab.presentation.title.as_str(),
                tab.presentation.tooltip.clone().unwrap(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(documents.len(), 2);
    for ((title, tooltip), (suffix, path)) in documents.iter().zip([("one", &one), ("two", &two)]) {
        let canonical_uri = file_uri(&fs::canonicalize(path).unwrap());
        assert_eq!(*title, format!("README.md — {suffix}"));
        assert!(
            tooltip.ends_with(canonical_uri.as_str()),
            "{tooltip:?} names the admitted canonical file {canonical_uri}"
        );
    }
    app.refresh_window_tabs(wid);
    assert_eq!(
        app.windows[&wid].tab_set.tabs()[1].presentation.title,
        "README.md — one"
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn editor_undo_redo_command_availability_tracks_shipping_history_after_every_reduction() {
    let dir = fresh_dir("aterm-app-editor-history");
    let path = dir.join("history.txt");
    fs::write(&path, "abc").unwrap();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.open_document_tab(AppKind::Editor, &file_uri(&path))
        .unwrap();
    let (instance, view) = app.active_native_view(wid).unwrap();
    let enabled = |app: &App, action: &str| {
        app.native_runtime
            .commands(instance, view)
            .unwrap()
            .into_iter()
            .find(|command| command.id.as_str() == action)
            .expect("editor history command")
            .enabled
    };
    assert!(!enabled(&app, "editor/undo"));
    assert!(!enabled(&app, "editor/redo"));

    commit(&mut app, wid, "x");
    assert!(enabled(&app, "editor/undo"));
    assert!(!enabled(&app, "editor/redo"));

    app.dispatch_native_event(
        wid,
        AppEvent::Action(crate::native_app::ActionInvocation {
            id: crate::native_ui::ActionId::new("editor/undo"),
            value: None,
        }),
    )
    .unwrap();
    assert!(!enabled(&app, "editor/undo"));
    assert!(enabled(&app, "editor/redo"));

    app.dispatch_native_event(
        wid,
        AppEvent::Action(crate::native_app::ActionInvocation {
            id: crate::native_ui::ActionId::new("editor/redo"),
            value: None,
        }),
    )
    .unwrap();
    assert!(enabled(&app, "editor/undo"));
    assert!(!enabled(&app, "editor/redo"));
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn an_undo_back_to_the_saved_text_leaves_the_document_clean() {
    let dir = std::env::temp_dir().join(format!("aterm-app-undo-clean-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("undo.txt");
    fs::write(&path, "abc").unwrap();
    let mtime = || fs::metadata(&path).unwrap().modified().unwrap();
    let before = mtime();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.open_document_tab(AppKind::Editor, &file_uri(&path))
        .unwrap();
    let (instance, view) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();
    let enabled = |app: &App, action: &str| {
        app.native_runtime
            .commands(instance, view)
            .unwrap()
            .into_iter()
            .find(|command| command.id.as_str() == action)
            .expect("editor command")
            .enabled
    };
    let editor_dirty = |app: &App| {
        let Some(NativeApp::Editor(editor)) = app.native_runtime.app(instance) else {
            panic!("editor app");
        };
        editor.dirty
    };
    let type_text = |app: &mut App, text: &str| {
        app.dispatch_native_event(
            wid,
            AppEvent::TextInput(TextInputEvent::Commit(text.to_string())),
        )
        .unwrap();
    };
    let undo = |app: &mut App| {
        app.dispatch_native_event(
            wid,
            AppEvent::Action(crate::native_app::ActionInvocation {
                id: crate::native_ui::ActionId::new("editor/undo"),
                value: None,
            }),
        )
        .unwrap();
    };

    type_text(&mut app, "x");
    assert_eq!(app.document_store.dirty(document), Some(true));
    assert!(enabled(&app, "editor/save") && editor_dirty(&app));
    undo(&mut app);
    assert_eq!(
        app.document_store.snapshot(document).unwrap().text.as_ref(),
        "abc"
    );
    assert_eq!(app.document_store.dirty(document), Some(false));
    assert!(!enabled(&app, "editor/save"), "Save is quiet");
    assert!(!editor_dirty(&app), "the editor reads clean");
    let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view) else {
        panic!("editor view");
    };
    assert_eq!(state.status.as_deref(), Some("No unsaved changes"));
    let views = [crate::document_store::DocumentViewId(view.get())];
    assert!(matches!(
        app.document_store.prepare_close(document, &views),
        Ok(crate::document_store::DocumentCloseReadiness::Ready { .. })
    ));
    app.document_store.abort_close(document).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "abc");
    assert_eq!(mtime(), before, "nothing was written");

    // Two characters at a time: each is its own undo step (one at a
    // time, they would join one run of typing).
    type_text(&mut app, "xx");
    type_text(&mut app, "yy");
    undo(&mut app);
    assert_ne!(
        app.document_store.snapshot(document).unwrap().text.as_ref(),
        "abc"
    );
    assert_eq!(app.document_store.dirty(document), Some(true));
    undo(&mut app);
    assert_eq!(
        app.document_store.snapshot(document).unwrap().text.as_ref(),
        "abc"
    );
    assert_eq!(app.document_store.dirty(document), Some(false));

    type_text(&mut app, "z");
    let saving = begin_inflight_save_for_test(&mut app, document);
    undo(&mut app);
    assert_eq!(
        app.document_store.snapshot(document).unwrap().text.as_ref(),
        "abc"
    );
    assert_eq!(app.document_store.dirty(document), Some(true));
    finish_inflight_save_for_test(&mut app, document, view, saving);
    assert_eq!(fs::read_to_string(&path).unwrap(), "zabc");
    assert_eq!(app.document_store.dirty(document), Some(true));
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn a_content_fingerprint_collision_does_not_mark_an_edit_saved() {
    const SAVED: &str = "file:///Users//alice/Downloads/7ykhvtpwaxxug.txt";
    const EDITED: &str = "file:///Users//alice/Downloads/vwelffjwxnsmb.txt";
    assert_ne!(SAVED, EDITED);
    assert_eq!(SAVED.len(), EDITED.len());
    assert_eq!(
        crate::native_document_io::ContentFingerprint::of(SAVED.as_bytes()),
        crate::native_document_io::ContentFingerprint::of(EDITED.as_bytes())
    );
    let scratch = aterm_tempfile::tempdir().unwrap();
    let path = scratch.path().join("collision.txt");
    fs::write(&path, SAVED).unwrap();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.open_document_tab(AppKind::Editor, &file_uri(&path))
        .unwrap();
    let (instance, view) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();
    let before = app.document_store.snapshot(document).unwrap();
    let outcome = app.document_store.transact(
        document,
        before.seq,
        vec![crate::document_store::TextEdit {
            range: 0..before.text.len(),
            insert: EDITED.into(),
        }],
    );
    assert!(matches!(
        outcome,
        crate::document_store::DocumentTxnOutcome::Committed { .. }
    ));
    app.publish_editor_commit(document, view);
    assert_eq!(
        app.document_store.dirty(document),
        Some(true),
        "different bytes must retain their unsaved checkpoint"
    );
    assert!(
        app.native_runtime
            .commands(instance, view)
            .unwrap()
            .iter()
            .any(|command| command.id.as_str() == "editor/save" && command.enabled)
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), SAVED);
}

#[test]
fn a_saved_image_replaces_the_exact_undo_clean_baseline() {
    const BEFORE: &str = "file:///Users//alice/Downloads/7ykhvtpwaxxug.txt";
    const SAVED: &str = "file:///Users//alice/Downloads/vwelffjwxnsmb.txt";
    let scratch = aterm_tempfile::tempdir().unwrap();
    let path = scratch.path().join("saved-collision.txt");
    fs::write(&path, BEFORE).unwrap();
    let mut app = App::headless_for_test();
    app.open_document_tab(AppKind::Editor, &file_uri(&path))
        .unwrap();
    let (instance, view) = app.active_native_view(WindowId(0)).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();
    let replace = |app: &mut App, text: &str| {
        let head = app.document_store.snapshot(document).unwrap();
        assert!(matches!(
            app.document_store.transact(
                document,
                head.seq,
                vec![crate::document_store::TextEdit {
                    range: 0..head.text.len(),
                    insert: text.into(),
                }]
            ),
            crate::document_store::DocumentTxnOutcome::Committed { .. }
        ));
    };
    replace(&mut app, SAVED);
    app.publish_editor_commit(document, view);
    let saving = begin_inflight_save_for_test(&mut app, document);
    finish_inflight_save_for_test(&mut app, document, view, saving);
    assert_eq!(fs::read_to_string(&path).unwrap(), SAVED);
    assert_eq!(app.document_store.dirty(document), Some(false));
    replace(&mut app, BEFORE);
    app.publish_editor_commit(document, view);
    assert_eq!(
        app.document_store.dirty(document),
        Some(true),
        "an older colliding image is not the newly saved file"
    );
    replace(&mut app, SAVED);
    app.publish_editor_commit(document, view);
    assert_eq!(
        app.document_store.dirty(document),
        Some(false),
        "the exact newly saved image is clean"
    );
}

#[test]
fn stable_observations_do_not_treat_colliding_text_as_identical_bytes() {
    const BEFORE: &str = "file:///Users//alice/Downloads/7ykhvtpwaxxug.txt";
    const OBSERVED: &str = "file:///Users//alice/Downloads/vwelffjwxnsmb.txt";
    for dirty in [false, true] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let path = scratch.path().join("observed-collision.txt");
        fs::write(&path, BEFORE).unwrap();
        let mut app = App::headless_for_test();
        app.open_document_tab(AppKind::Editor, &file_uri(&path))
            .unwrap();
        let (instance, view) = app.active_native_view(WindowId(0)).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        if dirty {
            let head = app.document_store.snapshot(document).unwrap();
            assert!(matches!(
                app.document_store.transact(
                    document,
                    head.seq,
                    vec![crate::document_store::TextEdit {
                        range: 0..head.text.len(),
                        insert: "private draft".into(),
                    }]
                ),
                crate::document_store::DocumentTxnOutcome::Committed { .. }
            ));
            app.publish_editor_commit(document, view);
        }
        fs::write(&path, OBSERVED).unwrap();
        let reduction = app.refresh_native_document(document).unwrap();
        if dirty {
            assert!(
                matches!(
                    reduction,
                    crate::native_document_io::FileWatchReduction::ConflictDirty { .. }
                ),
                "different observed bytes must preserve the draft conflict: {reduction:?}"
            );
            assert_eq!(
                app.document_store.snapshot(document).unwrap().text.as_ref(),
                "private draft"
            );
            assert!(app.native_documents.disk_conflicts.contains_key(&document));
        } else {
            assert!(
                matches!(
                    reduction,
                    crate::native_document_io::FileWatchReduction::ReloadClean { .. }
                ),
                "different observed bytes must reload the clean document: {reduction:?}"
            );
            let head = app.document_store.snapshot(document).unwrap();
            assert_eq!(head.text.as_ref(), OBSERVED);
            assert_eq!(app.document_store.dirty(document), Some(false));
            assert!(matches!(
                app.document_store.transact(
                    document,
                    head.seq,
                    vec![crate::document_store::TextEdit {
                        range: 0..head.text.len(),
                        insert: BEFORE.into(),
                    }]
                ),
                crate::document_store::DocumentTxnOutcome::Committed { .. }
            ));
            app.publish_editor_commit(document, view);
            assert_eq!(
                app.document_store.dirty(document),
                Some(true),
                "the pre-refresh image must not remain the exact clean baseline"
            );
        }
        assert_eq!(fs::read_to_string(&path).unwrap(), OBSERVED);
    }
}

#[cfg(unix)]
#[test]
fn a_fresh_colliding_read_after_a_save_rebind_keeps_the_draft_conflict() {
    const BEFORE: &str = "file:///Users//alice/Downloads/7ykhvtpwaxxug.txt";
    const COLLISION: &str = "file:///Users//alice/Downloads/vwelffjwxnsmb.txt";
    let scratch = aterm_tempfile::tempdir().unwrap();
    let path = scratch.path().join("rebound-collision.txt");
    let replacement = scratch.path().join("replacement.txt");
    fs::write(&path, BEFORE).unwrap();
    let mut app = App::headless_for_test();
    app.open_document_tab(AppKind::Editor, &file_uri(&path))
        .unwrap();
    let (instance, view) = app.active_native_view(WindowId(0)).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();
    let head = app.document_store.snapshot(document).unwrap();
    assert!(matches!(
        app.document_store.transact(
            document,
            head.seq,
            vec![crate::document_store::TextEdit {
                range: 0..head.text.len(),
                insert: "private draft".into(),
            }]
        ),
        crate::document_store::DocumentTxnOutcome::Committed { .. }
    ));
    app.publish_editor_commit(document, view);
    fs::write(&replacement, BEFORE).unwrap();
    fs::rename(&replacement, &path).unwrap();
    let save = app.save_document_checkpoint(document, view).unwrap_err();
    assert!(save.contains("Disk version refreshed"), "{save}");
    assert!(!app.native_documents.disk_conflicts.contains_key(&document));

    fs::write(&path, COLLISION).unwrap();
    let reduction = app.refresh_native_document(document).unwrap();
    assert!(matches!(
        reduction,
        crate::native_document_io::FileWatchReduction::ConflictDirty { .. }
    ));
    assert!(app.native_documents.disk_conflicts.contains_key(&document));
    assert_eq!(app.document_store.dirty(document), Some(true));
    assert_eq!(
        app.document_store.snapshot(document).unwrap().text.as_ref(),
        "private draft"
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), COLLISION);
}

#[test]
fn markdown_mode_change_keeps_the_live_document_after_its_file_disappears() {
    let dir = fresh_dir("aterm-app-edit-handoff");
    let path = dir.join("handoff.md");
    let source = "# Handoff\n\nBody\n";
    fs::write(&path, source).unwrap();
    let uri = file_uri(&path);
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.open_document_tab(AppKind::Markdown, &uri).unwrap();
    let (instance, view) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();
    app.dispatch_native_event(
        wid,
        AppEvent::Action(crate::native_app::ActionInvocation {
            id: crate::native_ui::ActionId::new("markdown/select-all"),
            value: None,
        }),
    )
    .unwrap();
    let before_document = app.document_store.snapshot(document).unwrap();
    let before_tabs = app.windows[&wid].tab_set.tabs().to_vec();
    let before_views = app.view_store.len();
    fs::remove_file(&path).unwrap();

    app.dispatch_native_event(
        wid,
        AppEvent::Action(crate::native_app::ActionInvocation {
            id: crate::native_ui::ActionId::new("editor/markdown/edit"),
            value: None,
        }),
    )
    .unwrap();

    assert_eq!(app.active_native_view(wid), Some((instance, view)));
    assert_eq!(app.windows[&wid].tab_set.tabs(), before_tabs);
    assert_eq!(app.view_store.len(), before_views);
    assert_eq!(app.document_store.view_count(document), Some(1));
    let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view) else {
        panic!("the workspace keeps its editor ownership");
    };
    let workspace = state.markdown.as_ref().unwrap();
    assert_eq!(workspace.mode, MarkdownWorkspaceMode::Edit);
    assert_eq!(workspace.reader.selection, Some(0..source.len()));
    let after = app.document_store.snapshot(document).unwrap();
    assert_eq!(
        (after.seq, after.text),
        (before_document.seq, before_document.text)
    );
    assert_eq!(app.document_store.dirty(document), Some(false));
    assert!(
        !path.exists(),
        "a mode change does not recreate or save the file"
    );
    assert!(app.structural_invariants_ok());
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn editor_ime_input_route_preserves_document_and_minibuffer_marked_carets() {
    let (dir, uri) = admission_fixture("ime-marked-caret", b"original\n");
    let mut app = App::headless_for_test();
    app.open_document_tab(AppKind::Editor, &uri).unwrap();
    let wid = WindowId(0);
    let (instance, view) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();
    let original = app.document_store.snapshot(document).unwrap();
    for minibuffer in [false, true] {
        if minibuffer {
            drive_native(&mut app, isearch_key());
        }
        for (selection, expected) in [
            (Some((3, 6)), Some(6)),
            (Some((1, 3)), Some(3)),
            (Some((2, 3)), None),
            (Some((6, 3)), None),
            (Some((0, 99)), None),
            (None, None),
        ] {
            app.on_ime_preedit(wid, "e\u{301}日本".to_string(), selection);
            let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view) else {
                panic!("editor")
            };
            assert_eq!(state.preedit, "e\u{301}日本");
            assert_eq!(
                state.preedit_caret, expected,
                "minibuffer={minibuffer}, selection={selection:?}"
            );
            assert_eq!(
                state.buffer.as_ref().unwrap().minibuffer_active(),
                minibuffer
            );
            let current = app.document_store.snapshot(document).unwrap();
            assert_eq!(
                (current.seq, current.text.as_ref()),
                (original.seq, original.text.as_ref())
            );
        }
        app.on_ime_preedit(wid, String::new(), None);
        let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view) else {
            panic!("editor")
        };
        assert!(state.preedit.is_empty());
        assert_eq!(state.preedit_caret, None);
    }
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn editor_input_mutates_shared_document_and_save_advances_checkpoint() {
    let dir = fresh_dir("aterm-app-edit");
    let path = dir.join("editable.md");
    fs::write(&path, "shared\n").unwrap();
    let uri = file_uri(&path);
    let mut app = App::headless_for_test();
    app.open_document_tab(AppKind::Markdown, &uri).unwrap();
    app.open_document_tab(AppKind::Editor, &uri).unwrap();
    let wid = WindowId(0);
    let (instance, _) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();

    assert_eq!(
        app.dispatch_native_event(
            wid,
            AppEvent::TextInput(TextInputEvent::Commit("native ".to_string())),
        )
        .unwrap(),
        EventResult::Handled
    );
    let edited = app.document_store.snapshot(document).unwrap();
    assert_eq!(edited.text.as_ref(), "native shared\n");
    assert_eq!(app.document_store.dirty(document), Some(true));

    app.dispatch_native_event(wid, AppEvent::EditorCommand(EditorCommand::Save))
        .unwrap();
    assert_eq!(app.document_store.dirty(document), Some(false));
    assert_eq!(fs::read_to_string(&path).unwrap(), "native shared\n");
    assert_eq!(
        app.document_store.checkpoint_seq(document),
        Some(app.document_store.snapshot(document).unwrap().seq)
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn completion_does_not_report_saved_when_newer_edits_were_not_requested() {
    let dir = fresh_dir("aterm-app-save-newer-modified");
    let path = dir.join("modified.md");
    fs::write(&path, "base\n").unwrap();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.open_document_tab(AppKind::Editor, &file_uri(&path))
        .unwrap();
    let (instance, view) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();

    commit(&mut app, wid, "saved ");
    let first = begin_inflight_save_for_test(&mut app, document);
    commit(&mut app, wid, "newer ");

    finish_inflight_save_for_test(&mut app, document, view, first);

    assert_eq!(app.document_store.dirty(document), Some(true));
    assert_eq!(fs::read_to_string(&path).unwrap(), "saved base\n");
    let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view) else {
        panic!("editor view must remain installed");
    };
    assert_ne!(state.status.as_deref(), Some("Saved"));
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn repeated_save_during_inflight_checkpoint_persists_the_latest_sequence() {
    let dir = fresh_dir("aterm-app-save-latch");
    let path = dir.join("latched.md");
    fs::write(&path, "base\n").unwrap();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.open_document_tab(AppKind::Editor, &file_uri(&path))
        .unwrap();
    let (instance, view) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();

    commit(&mut app, wid, "first ");
    let first = begin_inflight_save_for_test(&mut app, document);

    commit(&mut app, wid, "second ");
    app.save_document_checkpoint(document, view).unwrap();
    commit(&mut app, wid, "third ");
    app.save_document_checkpoint(document, view).unwrap();
    let latest = app.document_store.snapshot(document).unwrap();
    assert_eq!(
        app.native_documents
            .pending_saves
            .get(&document)
            .map(|intent| intent.seq),
        Some(latest.seq)
    );

    finish_inflight_save_for_test(&mut app, document, view, first);

    assert!(!app.native_documents.inflight.contains(&document));
    assert!(!app.native_documents.pending_saves.contains_key(&document));
    assert_eq!(app.document_store.dirty(document), Some(false));
    assert_eq!(
        app.document_store.checkpoint_seq(document),
        Some(latest.seq)
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), latest.text.as_ref());
    let _ = fs::remove_dir_all(dir);
}

fn late_document<R>(
    name: &str,
    arm: impl FnOnce(&mut App, DocumentId, crate::tab_model::ViewId) -> R,
) -> (
    std::path::PathBuf,
    App,
    (DocumentId, crate::tab_model::ViewId),
    (DocumentId, crate::tab_model::ViewId),
    std::path::PathBuf,
    R,
) {
    let dir = fresh_dir(name);
    let planned = dir.join("planned.md");
    let late = dir.join("late.md");
    fs::write(&planned, "planned\n").unwrap();
    fs::write(&late, "late\n").unwrap();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.open_document_tab(AppKind::Editor, &file_uri(&planned))
        .unwrap();
    let (instance, planned_view) = app.active_native_view(wid).unwrap();
    let planned_document = app.native_runtime.document_id(instance).unwrap();
    commit(&mut app, wid, "planned edit ");
    let armed = arm(&mut app, planned_document, planned_view);
    app.open_document_tab(AppKind::Editor, &file_uri(&late))
        .unwrap();
    let (instance, late_view) = app.active_native_view(wid).unwrap();
    let late_document = app.native_runtime.document_id(instance).unwrap();
    commit(&mut app, wid, "late edit ");
    assert_eq!(app.document_store.dirty(late_document), Some(true));
    (
        dir,
        app,
        (planned_document, planned_view),
        (late_document, late_view),
        late,
        armed,
    )
}

#[test]
fn window_close_plan_frozen_before_a_later_document_cannot_orphan_its_dirty_view() {
    let wid = WindowId(0);
    let (dir, mut app, planned, (late_document, late_view), _, ()) =
        late_document("aterm-window-close-late-document", |app, _, _| {
            assert!(
                !app.prepare_window_document_shutdown_inner(wid, false)
                    .unwrap()
            );
        });
    app.save_document_checkpoint(planned.0, planned.1).unwrap();
    assert_eq!(
        app.take_ready_document_shutdowns().unwrap(),
        (false, vec![wid])
    );
    assert_eq!(app.document_store.view_count(planned.0), Some(0));
    assert_eq!(app.close_window_logical(wid), crate::CloseOutcome::Exit);
    assert!(
        !app.view_store.contains(late_view),
        "window teardown left an unreachable native view for a dirty document"
    );
    assert_eq!(
        app.document_store.view_count(late_document),
        Some(0),
        "a retired window must not keep a document reference edge"
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn repeated_window_close_gesture_cannot_commit_a_stale_document_plan() {
    let wid = WindowId(0);
    let (dir, mut app, _, (late_document, late_view), _, ()) =
        late_document("aterm-window-close-repeat-gesture", |app, _, _| {
            assert!(
                !app.prepare_window_document_shutdown_inner(wid, false)
                    .unwrap()
            );
        });
    if app
        .prepare_window_document_shutdown_inner(wid, true)
        .unwrap()
    {
        assert_eq!(
            app.document_store.dirty(late_document),
            Some(false),
            "the window cleared while a document opened after the plan armed \
             was still unsaved"
        );
        assert_eq!(app.document_store.view_count(late_document), Some(0));
        assert_eq!(app.close_window_logical(wid), crate::CloseOutcome::Exit);
        assert!(!app.view_store.contains(late_view));
    } else {
        assert!(app.view_store.contains(late_view));
    }
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn quit_plan_frozen_before_a_later_document_cannot_exit_over_its_edits() {
    let (dir, mut app, planned, (late_document, late_view), late, ()) =
        late_document("aterm-quit-late-document", |app, _, _| {
            assert!(!app.prepare_quit_document_shutdown_inner(false).unwrap());
        });
    app.save_document_checkpoint(planned.0, planned.1).unwrap();
    let mut verdict = app.take_ready_document_shutdowns().unwrap();
    if verdict.0 {
        assert_eq!(
            app.document_store.dirty(late_document),
            Some(false),
            "the Quit was granted over a document opened and edited after it armed"
        );
    } else {
        assert_eq!(verdict, (false, Vec::new()));
        app.save_document_checkpoint(late_document, late_view)
            .unwrap();
        verdict = app.take_ready_document_shutdowns().unwrap();
    }
    assert_eq!(
        verdict,
        (true, Vec::new()),
        "the Quit goes once both are saved"
    );
    assert_eq!(fs::read_to_string(&late).unwrap(), "late edit late\n");
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn repeated_quit_gesture_cannot_exit_over_a_later_document() {
    let (dir, mut app, _, (late_document, late_view), _, ()) =
        late_document("aterm-quit-repeat-gesture", |app, _, _| {
            assert!(!app.prepare_quit_document_shutdown_inner(false).unwrap());
        });
    if app.prepare_quit_document_shutdown_inner(true).unwrap() {
        assert_eq!(
            app.document_store.dirty(late_document),
            Some(false),
            "a repeated Quit was granted over a document opened after it armed"
        );
    } else {
        assert!(app.view_store.contains(late_view));
    }
    let _ = fs::remove_dir_all(dir);
}

/// The same late document, reached through a Quit that was ready when it
/// deferred behind an update handoff (`DeferredHandoffTeardown::CleanQuitReady`).
#[cfg(unix)]
#[test]
fn a_quit_ready_behind_an_update_cannot_exit_over_a_document_edited_meanwhile() {
    let _serial = crate::menu::terminate_test_serial();
    let (dir, mut app, _, (late_document, late_view), late, generation) =
        late_document("aterm-quit-replay-late", |app, document, view| {
            let crate::menu::NativeTerminateDecision::Dispatch(generation) =
                crate::menu::admit_native_terminate(crate::system_quit::Asker::Person)
            else {
                panic!("a first request dispatches");
            };
            assert!(!app.prepare_quit_document_shutdown_inner(false).unwrap());
            app.save_document_checkpoint(document, view).unwrap();
            assert_eq!(
                app.take_ready_document_shutdowns().unwrap(),
                (true, Vec::new())
            );
            generation
        });
    if app.replay_clean_quit_ready() {
        assert_eq!(
            app.document_store.dirty(late_document),
            Some(false),
            "the replayed Quit exited over a document edited while it was deferred"
        );
    } else {
        assert!(
            crate::menu::native_termination_is_current(generation),
            "a Quit left saving stays pending, neither completed nor cancelled"
        );
        app.save_document_checkpoint(late_document, late_view)
            .unwrap();
        assert_eq!(
            app.take_ready_document_shutdowns().unwrap(),
            (true, Vec::new()),
            "the Quit goes once the late document is saved"
        );
    }
    assert_eq!(fs::read_to_string(&late).unwrap(), "late edit late\n");
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn async_window_close_revalidates_settings_drafts_before_document_detach() {
    let dir = fresh_dir("aterm-window-close-native-revalidate");
    let path = dir.join("close.md");
    fs::write(&path, "base\n").unwrap();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.open_document_tab(AppKind::Editor, &file_uri(&path))
        .unwrap();
    let (editor, editor_view) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(editor).unwrap();
    commit(&mut app, wid, "durable ");
    assert!(
        !app.prepare_window_document_shutdown_inner(wid, false)
            .unwrap()
    );

    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let (_, settings_view) = app.active_native_view(wid).unwrap();
    enter_settings_draft(&mut app, wid, settings_view);
    app.save_document_checkpoint(document, editor_view).unwrap();

    assert_eq!(
        app.take_ready_document_shutdowns().unwrap(),
        (false, vec![])
    );
    assert_eq!(app.document_store.view_count(document), Some(1));
    assert!(
        app.native_documents
            .pending_window_closes
            .contains_key(&wid)
    );
    let controls = app.windows[&wid]
        .palette()
        .expect("blocked completion surfaces reducer recovery")
        .controls_lines();
    for action in ["settings/drafts/review", "settings/drafts/discard-all"] {
        assert!(
            controls.iter().any(|line| line.contains(action)),
            "missing {action}: {controls:?}"
        );
    }

    discard_all_settings_drafts(&mut app, wid, settings_view);
    assert_eq!(
        app.take_ready_document_shutdowns().unwrap(),
        (false, vec![wid])
    );
    assert_eq!(app.document_store.view_count(document), Some(0));
    assert!(
        !app.native_documents
            .pending_window_closes
            .contains_key(&wid)
    );
    let _ = fs::remove_dir_all(dir);
}

/// A logout, restart or shutdown (and a Quit & Reopen) asks nothing, but a tab's unsaved work
/// still holds it: the barrier refuses, and AppKit, which waits on the quit's reply, hears NO
/// once, so macOS abandons the logout as it does for any app with unsaved work, and the quit
/// is forgotten.
///
/// NEGATIVE CONTROL: make `menu::answer_cancelled` return before its NO and the refused
/// quit is never answered, so AppKit waits on it. Before 2026-11-06 AppKit had already been
/// answered `NSTerminateCancel` when the quit was taken up, so a logout was abandoned whether
/// or not anything held it.
#[cfg(target_os = "macos")]
#[test]
fn a_refused_quit_barrier_answers_appkit_no_once() {
    use crate::menu::{TerminateReply, ask_native_terminate, offer_terminate};
    use crate::system_quit::Asker;
    for asker in [Asker::System, Asker::Reopen] {
        let _serial = crate::menu::terminate_test_serial();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (_, view) = app.active_native_view(wid).unwrap();
        enter_settings_draft(&mut app, wid, view);
        let TerminateReply::Later(generation) =
            offer_terminate(ask_native_terminate(asker, true), |_| true)
        else {
            panic!("{asker:?}: the window takes the quit up");
        };
        assert!(crate::menu::appkit_awaits_quit_reply());
        assert!(
            !app.native_termination_ready(generation),
            "{asker:?}: the draft holds the quit"
        );
        assert_eq!(
            crate::menu::take_refusals_for_test(),
            [generation],
            "{asker:?}: AppKit hears NO"
        );
        assert!(!crate::menu::appkit_awaits_quit_reply());
        assert!(!crate::menu::native_termination_is_current(generation));
        assert!(
            !crate::system_quit::asked(),
            "{asker:?}: the refused quit is forgotten"
        );
        assert!(
            !app.native_termination_ready(generation),
            "{asker:?}: a replay of the refused quit does nothing"
        );
        assert!(
            crate::menu::take_refusals_for_test().is_empty(),
            "{asker:?}: one reply"
        );
    }
}

/// A quit nobody at a window started (a headless aterm whose launcher is gone, a
/// supervisor's signal) is not held by an unsaved Settings draft nobody can save or discard:
/// it was refused without a word and the aterm lingered to its 30 s backstop.
#[test]
fn an_unattended_quit_is_not_held_by_a_settings_draft() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let (_, view) = app.active_native_view(wid).unwrap();
    enter_settings_draft(&mut app, wid, view);
    assert_eq!(
        app.quit_barriers(),
        crate::app_window::QuitBarrier::Refused,
        "CONTROL: a person's Quit waits on the draft"
    );
    app.quit_unattended = Some("the process that started this headless aterm is gone");
    assert_eq!(app.quit_barriers(), crate::app_window::QuitBarrier::Ready);
}

#[test]
fn async_quit_revalidates_settings_drafts_before_exit_verdict() {
    let dir = fresh_dir("aterm-quit-native-revalidate");
    let path = dir.join("quit.md");
    fs::write(&path, "base\n").unwrap();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.open_document_tab(AppKind::Editor, &file_uri(&path))
        .unwrap();
    let (editor, editor_view) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(editor).unwrap();
    commit(&mut app, wid, "durable ");
    assert!(!app.prepare_quit_document_shutdown_inner(false).unwrap());

    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let (_, settings_view) = app.active_native_view(wid).unwrap();
    enter_settings_draft(&mut app, wid, settings_view);
    app.save_document_checkpoint(document, editor_view).unwrap();

    assert_eq!(
        app.take_ready_document_shutdowns().unwrap(),
        (false, vec![])
    );
    assert!(app.native_documents.pending_quit.is_some());
    assert_eq!(app.document_store.view_count(document), Some(1));
    assert!(app.windows[&wid].palette().is_some());

    discard_all_settings_drafts(&mut app, wid, settings_view);
    assert_eq!(app.take_ready_document_shutdowns().unwrap(), (true, vec![]));
    assert!(app.native_documents.pending_quit.is_none());
    assert_eq!(app.document_store.view_count(document), Some(1));
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn real_save_latch_and_close_completion_refine_the_derived_model() {
    #[derive(Clone, Copy)]
    struct Controller {
        baseline: u64,
        target: i64,
        requested: i64,
        close_seq: i64,
        settled: bool,
    }

    fn relative(seq: aterm_buffer::Seq, baseline: u64) -> i64 {
        i64::try_from(seq.0.saturating_sub(baseline)).unwrap()
    }

    fn project(
        model: &aterm_spec::derive::Model,
        app: &App,
        document: DocumentId,
        controller: Controller,
    ) -> aterm_spec::interp::State {
        let mut state = model.init_state();
        let snapshot = app.document_store.snapshot(document).unwrap();
        let durable = app.document_store.checkpoint_seq(document).unwrap();
        state.insert("head", relative(snapshot.seq, controller.baseline));
        state.insert("durable", relative(durable, controller.baseline));
        state.insert(
            "inflight",
            i64::from(app.native_documents.inflight.contains(&document)),
        );
        state.insert("target", controller.target);
        state.insert("requested", controller.requested);
        state.insert(
            "latched",
            i64::from(app.native_documents.pending_saves.contains_key(&document)),
        );
        state.insert(
            "close_waiting",
            i64::from(
                app.native_documents
                    .pending_window_closes
                    .values()
                    .any(|pending| {
                        pending
                            .plan
                            .documents
                            .iter()
                            .any(|item| item.document == document)
                    }),
            ),
        );
        state.insert("close_seq", controller.close_seq);
        state.insert(
            "closed",
            i64::from(app.document_store.view_count(document) == Some(0)),
        );
        state.insert("settled", i64::from(controller.settled));
        state
    }

    fn assert_action(
        model: &aterm_spec::derive::Model,
        before: &aterm_spec::interp::State,
        after: &aterm_spec::interp::State,
        action: &'static str,
    ) {
        assert_eq!(
            model.successors(action, before).as_slice(),
            std::slice::from_ref(after),
            "shipping document transition must refine {action}"
        );
        assert_eq!(
            aterm_spec::interp::admits(model, before, after),
            Some(action)
        );
        for invariant in &model.invariants {
            assert!(
                model.check_invariant(invariant.name, after),
                "post-state violates {}::{}: {after:?}",
                model.name,
                invariant.name
            );
        }
    }

    let dir = fresh_dir("aterm-app-save-latch-conformance");
    let path = dir.join("close.md");
    fs::write(&path, "base\n").unwrap();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.open_document_tab(AppKind::Editor, &file_uri(&path))
        .unwrap();
    let (instance, view) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();
    let baseline = app.document_store.snapshot(document).unwrap().seq.0;
    let model = aterm_models_native::native_save_intent_latch_model();
    let mut controller = Controller {
        baseline,
        target: 0,
        requested: 0,
        close_seq: 0,
        settled: true,
    };
    let mut state = project(&model, &app, document, controller);
    assert_eq!(state, model.init_state());

    commit(&mut app, wid, "first ");
    controller.settled = false;
    let after = project(&model, &app, document, controller);
    assert_action(&model, &state, &after, "Edit");
    state = after;

    let first = begin_inflight_save_for_test(&mut app, document);
    controller.target = state["head"];
    controller.requested = state["head"];
    let after = project(&model, &app, document, controller);
    assert_action(&model, &state, &after, "BeginSave");
    state = after;

    commit(&mut app, wid, "latest ");
    let after = project(&model, &app, document, controller);
    assert_action(&model, &state, &after, "Edit");
    state = after;

    assert!(
        !app.prepare_window_document_shutdown_inner(wid, true)
            .unwrap()
    );
    controller.requested = state["head"];
    controller.close_seq = state["head"];
    let after = project(&model, &app, document, controller);
    assert_action(&model, &state, &after, "BeginCloseInflight");
    state = after;

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let dropped = buggy.successors("CompleteChain", &state)[0].clone();
    assert_eq!(aterm_spec::interp::admits(&model, &state, &dropped), None);
    assert!(!buggy.check_invariant("SettledCoversLatestRequest", &dropped));
    assert!(!buggy.check_invariant("WaitingCloseHasCompletionPump", &dropped));

    assert_eq!(
        app.take_ready_document_shutdowns().unwrap(),
        (false, Vec::new())
    );
    assert_eq!(project(&model, &app, document, controller), state);
    let early = buggy.successors("CommitClose", &state)[0].clone();
    assert_eq!(early["closed"], 1);
    assert_eq!(aterm_spec::interp::admits(&model, &state, &early), None);
    assert!(!model.check_invariant("ClosedSequenceIsDurable", &early));

    finish_inflight_save_for_test(&mut app, document, view, first);
    controller.target = controller.requested;
    controller.settled = true;
    let after = project(&model, &app, document, controller);
    assert_action(&model, &state, &after, "CompleteChain");
    state = after;

    assert_eq!(
        app.take_ready_document_shutdowns().unwrap(),
        (false, vec![wid])
    );
    let after = project(&model, &app, document, controller);
    assert_action(&model, &state, &after, "CommitClose");
    assert_eq!(fs::read_to_string(&path).unwrap(), "first latest base\n");
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn an_emacs_buffer_close_waits_for_its_inflight_save_then_finishes_once() {
    let dir = fresh_dir("aterm-editor-close-waits");
    let path = dir.join("waits.txt");
    fs::write(&path, "base\n").unwrap();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.open_document_tab(AppKind::Editor, &file_uri(&path))
        .unwrap();
    let (instance, view) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();
    let tabs = |app: &App| app.windows.get(&wid).unwrap().tab_set.len();
    let opened = tabs(&app);
    commit(&mut app, wid, "edit ");
    let save = begin_inflight_save_for_test(&mut app, document);
    let close = |app: &mut App| {
        for event in [
            key(Key::Character('x'), Modifiers::CTRL),
            key(Key::Character('c'), Modifiers::CTRL),
        ] {
            assert!(app.native_input_event(wid, &event));
        }
    };
    close(&mut app);
    assert_eq!(tabs(&app), opened);
    assert_eq!(app.document_store.view_count(document), Some(1));
    assert!(app.document_close_pending_for_test(document));
    assert_eq!(fs::read_to_string(&path).unwrap(), "base\n");
    finish_inflight_save_for_test(&mut app, document, view, save);
    assert_eq!(tabs(&app), opened - 1, "the original key request finishes");
    assert_eq!(app.document_store.view_count(document), Some(0));
    assert!(!app.document_close_pending_for_test(document));
    assert_eq!(fs::read_to_string(&path).unwrap(), "edit base\n");
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn a_stale_save_completion_is_not_painted_as_a_failure() {
    let dir = fresh_dir("aterm-app-stale-save");
    let path = dir.join("late.md");
    fs::write(&path, "base\n").unwrap();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.open_document_tab(AppKind::Editor, &file_uri(&path))
        .unwrap();
    let (instance, view) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();
    commit(&mut app, wid, "edit ");
    let first = begin_inflight_save_for_test(&mut app, document);
    let generation = first.plan.generation;
    let bytes = std::sync::Arc::clone(&first.plan.bytes);
    finish_inflight_save_for_test(&mut app, document, view, first);

    app.finish_native_document_save(
        document,
        view,
        generation,
        std::sync::Arc::clone(&bytes),
        crate::native_document_io::AtomicSaveResult::Cancelled,
        None,
    )
    .unwrap();
    let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view) else {
        panic!("editor view");
    };
    assert_eq!(state.status.as_deref(), Some("Saved"));
    let tab = app.windows[&wid]
        .tab_set
        .tabs()
        .iter()
        .find(|tab| tab.root.contains(view))
        .expect("document tab");
    assert!(!tab.presentation.indicators.attention);

    commit(&mut app, wid, "newer ");
    let held = begin_inflight_save_for_test(&mut app, document);
    assert_ne!(held.plan.generation, generation);
    commit(&mut app, wid, "latest ");
    let latest = app.document_store.snapshot(document).unwrap();
    app.save_document_checkpoint(document, view).unwrap();
    let queued = app.native_documents.pending_saves[&document];
    assert_eq!(queued.seq, latest.seq);
    assert_eq!(queued.source_view, view);
    let checkpoint_before = app.document_store.checkpoint_seq(document);
    let observed_before = app.native_documents.persistence.observed(document);
    let disk_before = fs::read(&path).unwrap();
    assert!(app.native_documents.inflight.contains(&document));
    assert!(app.native_documents.persistence.save_in_flight(document));
    let Some(NativeApp::Editor(editor)) = app.native_runtime.app(instance) else {
        panic!("editor app");
    };
    assert!(editor.checkpoint_pending);
    assert!(!app.can_save_document(wid));
    assert!(!app.prepare_quit_document_shutdown_inner(false).unwrap());
    let quit_before = app
        .native_documents
        .pending_quit
        .clone()
        .expect("Quit is waiting");
    let phase_before = app.document_store.phase(document);
    assert_eq!(
        phase_before,
        Some(crate::document_store::DocumentPhase::Closing {
            requested: latest.seq
        })
    );

    // An obsolete completion at the host boundary cannot take ownership of
    // the newer save or the later explicit Save waiting behind it.
    app.finish_native_document_save(
        document,
        view,
        generation,
        bytes,
        crate::native_document_io::AtomicSaveResult::Cancelled,
        None,
    )
    .unwrap();
    let mut failures = Vec::new();
    if !app.native_documents.inflight.contains(&document) {
        failures.push("obsolete result cleared the newer host inflight latch");
    }
    assert!(
        app.native_documents.persistence.save_in_flight(document),
        "the persistence reducer retains its newer plan"
    );
    let Some(NativeApp::Editor(editor)) = app.native_runtime.app(instance) else {
        panic!("editor app");
    };
    if !editor.checkpoint_pending || app.can_save_document(wid) {
        failures.push("obsolete result cleared the newer editor Saving state");
    }
    if app.native_documents.pending_saves.get(&document) != Some(&queued) {
        failures.push("obsolete result removed the later explicit Save intent");
    }
    assert_eq!(
        app.document_store.checkpoint_seq(document),
        checkpoint_before
    );
    assert_eq!(
        app.native_documents.persistence.observed(document),
        observed_before
    );
    let after = app.document_store.snapshot(document).unwrap();
    assert_eq!(after.seq, latest.seq);
    assert_eq!(after.text, latest.text);
    assert_eq!(app.document_store.dirty(document), Some(true));
    assert_eq!(fs::read(&path).unwrap(), disk_before);
    assert_eq!(
        app.native_documents.pending_quit.as_ref(),
        Some(&quit_before)
    );
    assert_eq!(app.document_store.phase(document), phase_before);
    assert_eq!(
        app.take_ready_document_shutdowns().unwrap(),
        (false, vec![])
    );
    assert_eq!(
        app.native_documents.pending_quit.as_ref(),
        Some(&quit_before)
    );

    // Landing this exact held generation also proves it remained the
    // reducer's current plan, then drains the later Save through the host.
    finish_inflight_save_for_test(&mut app, document, view, held);
    if fs::read_to_string(&path).unwrap() != latest.text.as_ref() {
        failures.push("the later explicitly saved text did not reach disk");
    }
    if app.document_store.checkpoint_seq(document) != Some(latest.seq)
        || app.document_store.dirty(document) != Some(false)
    {
        failures.push("the later Save did not checkpoint the latest draft");
    }
    assert!(!app.native_documents.inflight.contains(&document));
    assert!(!app.native_documents.persistence.save_in_flight(document));
    assert!(!app.native_documents.pending_saves.contains_key(&document));
    let Some(NativeApp::Editor(editor)) = app.native_runtime.app(instance) else {
        panic!("editor app");
    };
    assert!(!editor.checkpoint_pending);
    assert_eq!(app.take_ready_document_shutdowns().unwrap(), (true, vec![]));
    assert!(app.native_documents.pending_quit.is_none());
    let _ = fs::remove_dir_all(dir);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn quit_during_inflight_save_pumps_the_frozen_quit_sequence() {
    let _serial = crate::menu::terminate_test_serial();
    let crate::menu::NativeTerminateDecision::Dispatch(generation) =
        crate::menu::admit_native_terminate(crate::system_quit::Asker::Reopen)
    else {
        panic!("a fresh native quit dispatches");
    };
    let dir = fresh_dir("aterm-app-quit-save-latch");
    let path = dir.join("quit.md");
    fs::write(&path, "base\n").unwrap();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.open_document_tab(AppKind::Editor, &file_uri(&path))
        .unwrap();
    let (instance, view) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();

    commit(&mut app, wid, "first ");
    let first = begin_inflight_save_for_test(&mut app, document);
    commit(&mut app, wid, "latest ");
    let latest = app.document_store.snapshot(document).unwrap();

    assert_eq!(app.quit_barriers(), crate::app_window::QuitBarrier::Saving);
    assert_eq!(
        crate::menu::admit_native_terminate(crate::system_quit::Asker::Person),
        crate::menu::NativeTerminateDecision::DeferExisting(generation),
        "the native quit stays pending while the frozen save catches up"
    );
    assert!(crate::system_quit::reopens());
    assert!(app.native_documents.pending_saves.contains_key(&document));
    finish_inflight_save_for_test(&mut app, document, view, first);

    assert_eq!(app.take_ready_document_shutdowns().unwrap(), (true, vec![]));
    assert!(!app.native_documents.pending_saves.contains_key(&document));
    assert_eq!(
        app.document_store.checkpoint_seq(document),
        Some(latest.seq)
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), latest.text.as_ref());
    assert!(crate::menu::complete_current_native_termination());
    assert_eq!(
        crate::menu::admit_native_terminate(crate::system_quit::Asker::Person),
        crate::menu::NativeTerminateDecision::AllowExit(generation),
        "only the saved native request becomes eligible to exit"
    );
    assert!(crate::system_quit::reopens());
    let _ = fs::remove_dir_all(dir);
}

fn undo(app: &mut App, wid: WindowId) {
    app.dispatch_native_event(
        wid,
        AppEvent::Action(crate::native_app::ActionInvocation {
            id: crate::native_ui::ActionId::new("editor/undo"),
            value: None,
        }),
    )
    .unwrap();
}

fn assert_clean_at(
    app: &App,
    (instance, view): (crate::tab_model::AppInstanceId, crate::tab_model::ViewId),
    text: &str,
    context: &str,
) {
    let document = app.native_runtime.document_id(instance).unwrap();
    assert_eq!(
        app.document_store.snapshot(document).unwrap().text.as_ref(),
        text,
        "{context}"
    );
    assert_eq!(app.document_store.dirty(document), Some(false), "{context}");
    assert!(
        !app.native_documents.disk_conflicts.contains_key(&document),
        "{context}"
    );
    assert!(
        !app.native_documents.recovery_status.contains_key(&document),
        "{context}"
    );
    let Some(NativeApp::Editor(editor)) = app.native_runtime.app(instance) else {
        panic!("{context}: the editor stays installed");
    };
    assert!(!editor.disk_conflict, "{context}");
    assert!(!editor.dirty, "{context}");
    assert_eq!(editor.recovery_status, None, "{context}");
    let save = app
        .native_runtime
        .commands(instance, view)
        .unwrap()
        .into_iter()
        .find(|command| command.id.as_str() == "editor/save")
        .expect("Save command");
    assert!(!save.enabled, "{context}: nothing to save");
}

#[test]
fn an_undo_back_to_the_base_under_a_disk_conflict_adopts_the_disk_text() {
    let wid = WindowId(0);
    let base = "font_px = 12.0\n";
    let external = "cursor_blink = true\nfont_px = 12.0\n";
    let (dir, path, mut app, views) = open_config("aterm-undo-to-base-under-conflict", base);
    let document = app.native_runtime.document_id(views.0).unwrap();
    commit(&mut app, wid, "gpu");
    crate::test_scratch::write_config(&path, external);
    let observation =
        crate::native_config_service::VersionedConfigService::observe_path(&path, false).unwrap();
    app.refresh_open_config_editor_observation(&observation)
        .unwrap();
    assert_eq!(
        app.native_documents.disk_conflicts.get(&document),
        Some(&DiskConflictCause::Version)
    );
    assert_eq!(
        app.native_documents
            .recovery_status
            .get(&document)
            .map(String::as_str),
        Some(crate::native_app::DISK_CONFLICT_RECOVERY)
    );
    undo(&mut app, wid);
    assert_clean_at(&app, views, external, "Manual");
    let _ = fs::remove_dir_all(dir);

    let dir = fresh_dir("aterm-undo-to-base-under-conflict-editor");
    let path = dir.join("notes.md");
    crate::test_scratch::write_config(&path, "disk one\n");
    let mut app = App::headless_for_test();
    app.open_document_tab(AppKind::Editor, &file_uri(&path))
        .unwrap();
    let views = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(views.0).unwrap();
    commit(&mut app, wid, "local ");
    crate::test_scratch::write_config(&path, "disk two\n");
    assert!(matches!(
        app.refresh_native_document(document).unwrap(),
        crate::native_document_io::FileWatchReduction::ConflictDirty { .. }
    ));
    undo(&mut app, wid);
    assert_clean_at(&app, views, "disk two\n", "Editor");
    let _ = fs::remove_dir_all(dir);
}

/// Every reload path (the watch's clean reload, Discard, and an undo back
/// to the base under a conflict) keeps the caret, a mark and the scroll
/// on the text they were on, wherever the disk did not change it.
#[test]
fn a_reload_from_disk_keeps_the_caret_mark_and_scroll_on_their_text() {
    let wid = WindowId(0);
    let base = (1..=60)
        .map(|line| format!("key_{line:02} = {line}\n"))
        .collect::<String>();
    // The external writer deletes line 2 and appends a key at the end.
    let external = format!(
        "{}{}extra = 1\n",
        &base[..base.find("key_02").unwrap()],
        &base[base.find("key_03").unwrap()..]
    );
    let line_start = |text: &str, key: &str| text.find(key).unwrap();
    let place = |app: &mut App, view: crate::tab_model::ViewId| {
        let Some(AppViewState::Editor(state)) = app.native_runtime.view_state_mut(view) else {
            panic!("expected editor view");
        };
        let buffer = state.buffer.as_mut().expect("attached editor buffer");
        let caret = line_start(&base, "key_12") + 4;
        buffer.selections = vec![crate::native_editor::Selection::caret(caret)];
        buffer.primary = 0;
        buffer.viewport_anchor = line_start(&base, "key_10");
        caret
    };
    let check = |app: &App, view: crate::tab_model::ViewId, path: &str| {
        let buffer = editor_buffer(app, view);
        assert_eq!(
            buffer.primary_selection().range(),
            {
                let caret = line_start(&external, "key_12") + 4;
                caret..caret
            },
            "{path}: the caret stays on its text"
        );
        assert_eq!(
            buffer.viewport_anchor,
            line_start(&external, "key_10"),
            "{path}: the scroll stays on its text"
        );
    };
    let reload = |app: &mut App, path: &std::path::Path| {
        crate::test_scratch::write_config(path, &external);
        let observation =
            crate::native_config_service::VersionedConfigService::observe_path(path, false)
                .unwrap();
        app.refresh_open_config_editor_observation(&observation)
            .unwrap();
    };

    // The watch reloads a clean buffer.
    let (dir, path, mut app, views) = open_config("aterm-reload-keeps-caret-clean", &base);
    let document = app.native_runtime.document_id(views.0).unwrap();
    let caret = place(&mut app, views.1);
    app.dispatch_native_event(
        wid,
        AppEvent::EditorCommand(crate::native_editor::EditorCommand::SetMark),
    )
    .unwrap();
    assert_eq!(
        app.editor_workspace
            .global_mark_ring
            .front()
            .map(|mark| mark.offset),
        Some(caret)
    );
    reload(&mut app, &path);
    assert_eq!(
        app.document_store.snapshot(document).unwrap().text.as_ref(),
        external
    );
    check(&app, views.1, "a clean reload");
    assert_eq!(
        app.editor_workspace
            .global_mark_ring
            .front()
            .map(|mark| mark.offset),
        Some(line_start(&external, "key_12") + 4),
        "a clean reload: the mark stays on its text"
    );
    let _ = fs::remove_dir_all(dir);

    // Discard Changes and Reload from Disk.
    let (dir, path, mut app, views) = open_config("aterm-reload-keeps-caret-discard", &base);
    let document = app.native_runtime.document_id(views.0).unwrap();
    place(&mut app, views.1);
    commit(&mut app, wid, "x");
    reload(&mut app, &path);
    assert!(app.native_documents.disk_conflicts.contains_key(&document));
    app.discard_document_changes(document, views.1).unwrap();
    assert_eq!(
        app.document_store.snapshot(document).unwrap().text.as_ref(),
        external
    );
    check(&app, views.1, "Discard");
    let _ = fs::remove_dir_all(dir);

    // An undo back to the base under a conflict adopts the disk text.
    let (dir, path, mut app, views) = open_config("aterm-reload-keeps-caret-undo", &base);
    let document = app.native_runtime.document_id(views.0).unwrap();
    place(&mut app, views.1);
    commit(&mut app, wid, "x");
    reload(&mut app, &path);
    undo(&mut app, wid);
    assert_eq!(
        app.document_store.snapshot(document).unwrap().text.as_ref(),
        external
    );
    check(&app, views.1, "an undo to the base");
    let _ = fs::remove_dir_all(dir);
}

/// A reload from disk is a labelled frame through the editor's one
/// writer: C-/ takes a clean reload back to the text before it, and after
/// Discard brings the person's edits back (dirty, the disk's text one
/// Redo away), each status naming what it undid.
#[test]
fn undo_reaches_a_reload_and_a_discard_as_labelled_frames() {
    let wid = WindowId(0);
    let dir = fresh_dir("aterm-undo-reaches-reloads");
    let path = dir.join("notes.md");
    fs::write(&path, "disk one\n").unwrap();
    let mut app = App::headless_for_test();
    app.open_document_tab(AppKind::Editor, &file_uri(&path))
        .unwrap();
    let views = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(views.0).unwrap();
    let text = |app: &App| app.document_store.snapshot(document).unwrap().text;
    let status = |app: &App| {
        let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(views.1) else {
            panic!("expected editor view");
        };
        state.status.clone().unwrap_or_default()
    };
    let can_undo = |app: &App| {
        let Some(NativeApp::Editor(editor)) = app.native_runtime.app(views.0) else {
            panic!("the editor stays installed");
        };
        editor.can_undo
    };
    assert!(!can_undo(&app), "a fresh file has nothing to undo");

    // The watch's clean reload.
    fs::write(&path, "disk two\n").unwrap();
    assert!(matches!(
        app.refresh_native_document(document).unwrap(),
        crate::native_document_io::FileWatchReduction::ReloadClean { .. }
    ));
    assert_clean_at(&app, views, "disk two\n", "a clean reload");
    assert!(can_undo(&app), "the reload is a frame Undo reaches");
    undo(&mut app, wid);
    assert_eq!(text(&app).as_ref(), "disk one\n");
    assert!(
        status(&app).starts_with("Undid the reload from disk"),
        "{}",
        status(&app)
    );
    let _ = fs::remove_dir_all(dir);

    // Discard Changes and Reload from Disk over the person's edits.
    let dir = fresh_dir("aterm-undo-reaches-discard");
    let path = dir.join("notes.md");
    fs::write(&path, "disk one\n").unwrap();
    let mut app = App::headless_for_test();
    app.open_document_tab(AppKind::Editor, &file_uri(&path))
        .unwrap();
    let views = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(views.0).unwrap();
    commit(&mut app, wid, "local ");
    let mine = text(&app);
    fs::write(&path, "disk two\n").unwrap();
    assert!(matches!(
        app.refresh_native_document(document).unwrap(),
        crate::native_document_io::FileWatchReduction::ConflictDirty { .. }
    ));
    app.discard_document_changes(document, views.1).unwrap();
    assert_clean_at(&app, views, "disk two\n", "Discard");
    undo(&mut app, wid);
    assert_eq!(text(&app), mine, "C-/ brings the person's edits back");
    assert_eq!(app.document_store.dirty(document), Some(true));
    assert!(
        !app.native_documents.disk_conflicts.contains_key(&document),
        "the disk's text is the baseline now: Save overwrites it knowingly"
    );
    assert!(
        status(&app).starts_with("Undid the revert to the file on disk"),
        "{}",
        status(&app)
    );
    app.dispatch_native_event(
        wid,
        AppEvent::Action(crate::native_app::ActionInvocation {
            id: crate::native_ui::ActionId::new("editor/redo"),
            value: None,
        }),
    )
    .unwrap();
    assert_eq!(
        text(&app).as_ref(),
        "disk two\n",
        "Redo takes the disk's text"
    );
    let _ = fs::remove_dir_all(dir);
}

fn editor_action(app: &mut App, wid: WindowId, action: &str) -> Result<(), String> {
    app.dispatch_native_event(
        wid,
        AppEvent::Action(crate::native_app::ActionInvocation {
            id: crate::native_ui::ActionId::new(action),
            value: None,
        }),
    )
    .map(|_| ())
}

fn disk_notice(
    app: &App,
    instance: crate::tab_model::AppInstanceId,
) -> Option<crate::native_app::DiskNotice> {
    let Some(NativeApp::Editor(editor)) = app.native_runtime.app(instance) else {
        panic!("the editor stays installed");
    };
    editor.disk_notice
}

fn editor_status(app: &App, view: crate::tab_model::ViewId) -> String {
    let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view) else {
        panic!("expected editor view");
    };
    state.status.clone().unwrap_or_default()
}

/// The watch's wake over a clean buffer: the file's new text reloads in
/// place as one disk frame (C-/ takes it back), the caret staying on its
/// text. A wake for a file that did not change, or for a key nobody
/// watches, changes nothing; a released document is no longer watched.
#[test]
fn a_watched_file_rewritten_under_a_clean_buffer_reloads_in_place() {
    let wid = WindowId(0);
    let dir = fresh_dir("aterm-watch-clean-reload");
    let path = dir.join("notes.md");
    let base: String = (1..=30).map(|line| format!("line {line:02}\n")).collect();
    fs::write(&path, &base).unwrap();
    let mut app = App::headless_for_test();
    app.open_document_tab(AppKind::Editor, &file_uri(&path))
        .unwrap();
    let views = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(views.0).unwrap();
    let watched = app
        .native_documents
        .watch
        .watched_path(document)
        .map(|watched| fs::canonicalize(watched).unwrap());
    assert_eq!(
        watched,
        Some(fs::canonicalize(&path).unwrap()),
        "opening the file watches it"
    );

    // The negative controls: a wake with no change, and one for a key
    // nobody watches.
    app.native_document_file_changed(document.get());
    app.native_document_file_changed(document.get() + 1000);
    assert_clean_at(&app, views, &base, "an unchanged file");
    assert!(
        !app.editor_workspace.can_undo(document),
        "nothing was reloaded"
    );

    let caret = base.find("line 12").unwrap() + 3;
    {
        let Some(AppViewState::Editor(state)) = app.native_runtime.view_state_mut(views.1) else {
            panic!("expected editor view");
        };
        let buffer = state.buffer.as_mut().expect("attached editor buffer");
        buffer.selections = vec![Selection::caret(caret)];
        buffer.primary = 0;
    }
    // An AI CLI's edit tool deletes line 2.
    let external = base.replacen("line 02\n", "", 1);
    fs::write(&path, &external).unwrap();
    app.native_document_file_changed(document.get());
    assert_clean_at(&app, views, &external, "a clean reload");
    let moved = external.find("line 12").unwrap() + 3;
    assert_eq!(
        editor_buffer(&app, views.1).primary_selection().range(),
        moved..moved,
        "the caret stays on its text"
    );
    assert_eq!(editor_status(&app, views.1), "Reloaded changes from disk");
    undo(&mut app, wid);
    assert_eq!(
        app.document_store.snapshot(document).unwrap().text.as_ref(),
        base,
        "C-/ takes the reload back"
    );
    let _ = fs::remove_dir_all(dir);
}

/// Over unsaved edits the watch never reloads: the header row says the
/// file changed and offers Reload (the disk's text, the edits one undo
/// away) or Keep Mine (the buffer as it is, still unsaved, for Save to
/// write over the file; the file keeps the disk's text until then, and
/// no undo of the buffer's own typing ever reads as saved). Keep Mine
/// with nothing changed on disk keeps nothing (the negative control).
#[test]
fn a_file_changed_under_unsaved_edits_offers_reload_or_keep_and_loses_neither() {
    let wid = WindowId(0);
    let open = |name: &str| {
        let dir = fresh_dir(name);
        let path = dir.join("notes.md");
        fs::write(&path, "disk one\n").unwrap();
        let mut app = App::headless_for_test();
        app.open_document_tab(AppKind::Editor, &file_uri(&path))
            .unwrap();
        let views = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(views.0).unwrap();
        commit(&mut app, wid, "mine ");
        (dir, path, app, views, document)
    };
    let conflicted = |name: &str| {
        let (dir, path, mut app, views, document) = open(name);
        fs::write(&path, "disk two\n").unwrap();
        app.native_document_file_changed(document.get());
        (dir, path, app, views, document)
    };
    let text = |app: &App, document: DocumentId| {
        app.document_store
            .snapshot(document)
            .unwrap()
            .text
            .to_string()
    };

    // Keep Mine.
    let (dir, path, mut app, views, document) = conflicted("aterm-watch-keep");
    assert_eq!(
        text(&app, document),
        "mine disk one\n",
        "the watch never reloads over edits"
    );
    assert_eq!(
        disk_notice(&app, views.0),
        Some(crate::native_app::DiskNotice::Changed)
    );
    assert_eq!(
        app.native_documents.disk_conflicts.get(&document),
        Some(&DiskConflictCause::Version)
    );
    editor_action(&mut app, wid, "editor/keep-local").unwrap();
    assert_eq!(text(&app, document), "mine disk one\n");
    assert_eq!(app.document_store.dirty(document), Some(true));
    assert!(!app.native_documents.disk_conflicts.contains_key(&document));
    assert_eq!(disk_notice(&app, views.0), None);
    assert_eq!(editor_status(&app, views.1), watch::KEPT_STATUS);
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "disk two\n",
        "the file keeps the disk's text until Save"
    );
    undo(&mut app, wid);
    assert_eq!(
        text(&app, document),
        "disk one\n",
        "C-/ undoes the buffer's own typing"
    );
    assert_eq!(
        app.document_store.dirty(document),
        Some(true),
        "the text the buffer opened with is not the file on disk now"
    );
    editor_action(&mut app, wid, "editor/redo").unwrap();
    assert_eq!(text(&app, document), "mine disk one\n");
    editor_action(&mut app, wid, "editor/save").unwrap();
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "mine disk one\n",
        "Save writes over the file knowingly"
    );
    let _ = fs::remove_dir_all(dir);

    // Reload.
    let (dir, _path, mut app, views, document) = conflicted("aterm-watch-reload");
    editor_action(&mut app, wid, "editor/revert").unwrap();
    assert_clean_at(&app, views, "disk two\n", "Reload");
    assert_eq!(disk_notice(&app, views.0), None);
    undo(&mut app, wid);
    assert_eq!(
        text(&app, document),
        "mine disk one\n",
        "C-/ brings the edits back"
    );
    let _ = fs::remove_dir_all(dir);

    // The negative control: nothing changed on disk, so nothing is kept.
    let (dir, _path, mut app, views, document) = open("aterm-watch-keep-nothing");
    let kept = editor_action(&mut app, wid, "editor/keep-local");
    assert!(
        kept.as_ref()
            .is_err_and(|error| error.starts_with("Nothing to keep")),
        "{kept:?}"
    );
    assert_eq!(text(&app, document), "mine disk one\n");
    assert_eq!(disk_notice(&app, views.0), None);
    undo(&mut app, wid);
    assert_eq!(
        text(&app, document),
        "disk one\n",
        "no frame was pushed: C-/ undoes the typing"
    );
    let _ = fs::remove_dir_all(dir);
}

/// A file deleted under its buffer keeps the buffer's text: the header
/// row says so and offers Keep Mine, after which Save writes the file
/// again.
#[test]
fn a_file_deleted_under_its_buffer_keeps_the_text_and_keep_mine_writes_it_again() {
    let wid = WindowId(0);
    let dir = fresh_dir("aterm-watch-deleted");
    let path = dir.join("notes.md");
    fs::write(&path, "disk one\n").unwrap();
    let mut app = App::headless_for_test();
    app.open_document_tab(AppKind::Editor, &file_uri(&path))
        .unwrap();
    let views = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(views.0).unwrap();
    commit(&mut app, wid, "mine ");
    fs::remove_file(&path).unwrap();
    app.native_document_file_changed(document.get());
    assert_eq!(
        app.document_store.snapshot(document).unwrap().text.as_ref(),
        "mine disk one\n",
        "a deleted file never empties its buffer"
    );
    assert_eq!(
        disk_notice(&app, views.0),
        Some(crate::native_app::DiskNotice::Gone)
    );
    assert_eq!(
        app.native_documents
            .recovery_status
            .get(&document)
            .map(String::as_str),
        Some(crate::native_app::DISK_EXISTENCE_RECOVERY)
    );
    editor_action(&mut app, wid, "editor/keep-local").unwrap();
    assert!(!app.native_documents.disk_conflicts.contains_key(&document));
    assert_eq!(app.document_store.dirty(document), Some(true));
    editor_action(&mut app, wid, "editor/save").unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "mine disk one\n");
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn undo_to_base_keeps_local_edits_saves_in_flight_and_binding_conflicts() {
    let wid = WindowId(0);
    let base = "font_px = 12.0\n";
    let external = "cursor_blink = true\nfont_px = 12.0\n";
    let conflicted = |name: &str| {
        let (dir, path, mut app, views) = open_config(name, base);
        commit(&mut app, wid, "gpu");
        commit(&mut app, wid, " = true");
        crate::test_scratch::write_config(&path, external);
        let observation =
            crate::native_config_service::VersionedConfigService::observe_path(&path, false)
                .unwrap();
        app.refresh_open_config_editor_observation(&observation)
            .unwrap();
        let document = app.native_runtime.document_id(views.0).unwrap();
        assert!(app.native_documents.disk_conflicts.contains_key(&document));
        (dir, path, app, views, document)
    };

    let (dir, _, mut app, _, document) = conflicted("aterm-undo-to-base-partial");
    undo(&mut app, wid);
    assert_eq!(
        app.document_store.snapshot(document).unwrap().text.as_ref(),
        "gpufont_px = 12.0\n"
    );
    assert_eq!(app.document_store.dirty(document), Some(true));
    assert!(app.native_documents.disk_conflicts.contains_key(&document));
    assert_eq!(
        app.native_documents
            .recovery_status
            .get(&document)
            .map(String::as_str),
        Some(crate::native_app::DISK_CONFLICT_RECOVERY)
    );
    let _ = fs::remove_dir_all(dir);

    let (dir, path, mut app, views, document) = conflicted("aterm-undo-to-base-saving");
    app.native_documents.inflight.insert(document);
    undo(&mut app, wid);
    undo(&mut app, wid);
    assert_eq!(
        app.document_store.snapshot(document).unwrap().text.as_ref(),
        base
    );
    assert!(app.native_documents.disk_conflicts.contains_key(&document));
    app.native_documents.inflight.remove(&document);
    crate::test_scratch::write_config(&path, base);
    assert!(matches!(
        app.refresh_native_document(document).unwrap(),
        crate::native_document_io::FileWatchReduction::Unchanged
            | crate::native_document_io::FileWatchReduction::RebindEquivalent { .. }
    ));
    assert_clean_at(&app, views, base, "the file back at the base");
    let _ = fs::remove_dir_all(dir);

    let (dir, _, mut app, _, document) = conflicted("aterm-undo-to-base-binding");
    app.set_document_disk_conflict(document, Some(DiskConflictCause::Binding));
    undo(&mut app, wid);
    undo(&mut app, wid);
    assert_eq!(
        app.document_store.snapshot(document).unwrap().text.as_ref(),
        base
    );
    assert_eq!(
        app.native_documents.disk_conflicts.get(&document),
        Some(&DiskConflictCause::Binding)
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn file_refresh_reloads_clean_conflicts_dirty_and_explicit_discard_is_atomic() {
    use amacs::input::EditorInput;

    let dir = fresh_dir("aterm-app-refresh-discard");
    let path = dir.join("watched.md");
    fs::write(&path, "disk one\n").unwrap();
    let mut app = App::headless_for_test();
    app.open_document_tab(AppKind::Editor, &file_uri(&path))
        .unwrap();
    let wid = WindowId(0);
    let (instance, view) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();

    fs::write(&path, "disk two\n").unwrap();
    assert!(matches!(
        app.refresh_native_document(document).unwrap(),
        crate::native_document_io::FileWatchReduction::ReloadClean { .. }
    ));
    assert_eq!(
        app.document_store.snapshot(document).unwrap().text.as_ref(),
        "disk two\n"
    );
    assert_eq!(app.document_store.dirty(document), Some(false));

    commit(&mut app, wid, "local ");
    let local = app.document_store.snapshot(document).unwrap();
    let retained_state = |app: &mut App| {
        assert!(app.prepare_native_input_scratch(wid));
        let artifact = app
            .retained_native_leaf_artifact(wid, view, false)
            .expect("the retained Editor scene matches its current authority");
        let mut save_controls = artifact.compiled.semantics.iter().filter(|node| {
            node.action
                .as_ref()
                .is_some_and(|action| action.as_str() == "editor/save")
        });
        let save = save_controls
            .next()
            .expect("the retained toolbar has Save")
            .state
            .unwrap()
            .enabled;
        assert!(
            save_controls.next().is_none(),
            "the responsive toolbar has exactly one Save control"
        );
        let revert = artifact
            .compiled
            .semantic(&crate::native_ui::UiKey::new("editor/disk-reload"))
            .map(|node| node.state.unwrap().enabled);
        let conflict_recovery = artifact.compiled.paint.iter().any(|node| {
            matches!(&node.content, crate::native_ui::UiContent::TextViewport(spec)
                if spec.semantic_status.as_deref().is_some_and(|status|
                    status.contains(crate::native_app::DISK_CONFLICT_RECOVERY)))
        });
        let stamp = app.windows[&wid].leaf_render_cache[&view]
            .native
            .as_ref()
            .unwrap()
            .stamp;
        (save, revert, conflict_recovery, stamp)
    };
    let warm = retained_state(&mut app);
    assert_eq!(
        (warm.0, warm.1, warm.2),
        (true, None, false),
        "a dirty Editor begins with retained Save and no conflict recovery"
    );
    let tab = app.windows[&wid].tab_set.active_id().unwrap();
    let tab_count = app.windows[&wid].tab_set.tabs().len();
    fs::write(&path, "disk three\n").unwrap();
    app.open_document_tab(AppKind::Editor, &file_uri(&path))
        .unwrap();
    assert_eq!(app.active_native_view(wid), Some((instance, view)));
    assert_eq!(app.native_runtime.document_id(instance), Some(document));
    assert_eq!(app.windows[&wid].tab_set.active_id(), Some(tab));
    assert_eq!(app.windows[&wid].tab_set.tabs().len(), tab_count);
    let preserved = app.document_store.snapshot(document).unwrap();
    assert_eq!(preserved.seq, local.seq);
    assert_eq!(preserved.text, local.text);
    assert_eq!(app.document_store.dirty(document), Some(true));
    assert!(app.native_documents.disk_conflicts.contains_key(&document));
    let Some(NativeApp::Editor(editor)) = app.native_runtime.app(instance) else {
        panic!("conflicted document remains an Editor");
    };
    assert!(editor.disk_conflict);
    let save_enabled = app
        .native_runtime
        .commands(instance, view)
        .unwrap()
        .into_iter()
        .find(|command| command.id.as_str() == "editor/save")
        .expect("Save command")
        .enabled;
    assert!(!save_enabled, "Save UI is disabled during disk conflict");
    let mut retained_failures = Vec::new();
    let conflicted = retained_state(&mut app);
    if (conflicted.0, conflicted.1, conflicted.2) != (false, Some(true), true) {
        retained_failures.push(format!(
            "reopening the dirty document retained stale Save/Revert/recovery: {:?}",
            (conflicted.0, conflicted.1, conflicted.2)
        ));
    }
    if conflicted.3 == warm.3 {
        retained_failures
            .push("conflict state did not retire the old compiled-scene authority".to_string());
    }
    app.save_document_checkpoint(document, view).unwrap_err();
    assert_eq!(fs::read_to_string(&path).unwrap(), "disk three\n");
    let recovery = app
        .native_documents
        .recovery_status
        .get(&document)
        .expect("conflict recovery guidance");
    assert_eq!(recovery, crate::native_app::DISK_CONFLICT_RECOVERY);

    let revert_available = |app: &App| {
        let command_enabled = app
            .native_runtime
            .commands(instance, view)
            .unwrap()
            .into_iter()
            .find(|command| command.id.as_str() == "editor/revert")
            .unwrap()
            .enabled;
        let toolbar_enabled = app
            .compiled_native_ui(wid)
            .unwrap()
            .semantic(&crate::native_ui::UiKey::new("editor/disk-reload"))
            .unwrap()
            .state
            .unwrap()
            .enabled;
        (command_enabled, toolbar_enabled)
    };
    let editor_save_state = |app: &App| {
        let Some(NativeApp::Editor(editor)) = app.native_runtime.app(instance) else {
            panic!("the conflicted editor remains installed");
        };
        (
            editor.dirty,
            editor.disk_conflict,
            editor.checkpoint_pending,
            editor.recovery_status.clone(),
            editor.can_undo,
            editor.can_redo,
            app.native_documents.inflight.contains(&document),
            app.native_documents.persistence.save_in_flight(document),
            app.native_documents.persistence.observed(document),
        )
    };
    assert_eq!(revert_available(&app), (true, true));
    let mut capability_failures = Vec::new();
    for command in [
        EditorCommand::GotoLine,
        EditorCommand::ExecuteCommand,
        EditorCommand::SwitchBuffer,
        EditorCommand::KillBuffer,
        EditorCommand::IncrementalSearch,
    ] {
        app.dispatch_native_event(wid, AppEvent::EditorCommand(command))
            .unwrap();
        app.dispatch_native_event(wid, AppEvent::EditorInput(EditorInput::Text("2".into())))
            .unwrap();
        let before_buffer = editor_buffer(&app, view).clone();
        assert!(matches!(
            (command, &before_buffer.minibuffer),
            (EditorCommand::GotoLine, Minibuffer::GotoLine { .. })
                | (EditorCommand::ExecuteCommand, Minibuffer::Command { .. })
                | (EditorCommand::SwitchBuffer, Minibuffer::Buffer { .. })
                | (EditorCommand::KillBuffer, Minibuffer::Prompt { .. })
                | (EditorCommand::IncrementalSearch, Minibuffer::Search { .. })
        ));
        let query = match &before_buffer.minibuffer {
            Minibuffer::GotoLine { query, .. }
            | Minibuffer::Command { query, .. }
            | Minibuffer::Buffer { query }
            | Minibuffer::Prompt { query, .. }
            | Minibuffer::Search { query, .. } => query,
            Minibuffer::Inactive => unreachable!(),
        };
        assert_eq!(query, "2");
        let available = revert_available(&app);
        if available != (false, false) {
            capability_failures.push(format!(
                "{command:?}: Revert enabled in command/toolbar {available:?}"
            ));
        }
        let before_input = before_buffer.input_lifetime();
        let before_save = editor_save_state(&app);
        let before_conflicts = app.native_documents.disk_conflicts.clone();
        let before_recovery = app.native_documents.recovery_status.clone();
        for palette in [false, true] {
            let result = if palette {
                app.activate_native_palette_target(&crate::palette::NativeCommandTarget {
                    window: wid,
                    instance,
                    view,
                    generation: app.native_runtime.view_generation(view).unwrap(),
                    action: crate::native_ui::ActionId::new("editor/revert"),
                })
            } else {
                app.act_app(crate::app_control::ActRequest {
                    view,
                    ui_key: "editor/disk-reload".to_string(),
                    action: "editor/revert".to_string(),
                    value: None,
                })
                .map(|_| ())
            };
            if result.is_ok() {
                capability_failures.push(format!(
                    "{command:?}: Revert did not refuse with palette={palette}"
                ));
            }
            assert_eq!(editor_buffer(&app, view), &before_buffer);
            assert!(std::sync::Arc::ptr_eq(
                &before_input,
                &editor_buffer(&app, view).input_lifetime()
            ));
            let after = app.document_store.snapshot(document).unwrap();
            assert_eq!(after.seq, local.seq);
            assert_eq!(after.text, local.text);
            assert_eq!(app.document_store.dirty(document), Some(true));
            assert_eq!(fs::read_to_string(&path).unwrap(), "disk three\n");
            assert_eq!(editor_save_state(&app), before_save);
            assert_eq!(app.native_documents.disk_conflicts, before_conflicts);
            assert_eq!(app.native_documents.recovery_status, before_recovery);
        }
        let exit = if command == EditorCommand::GotoLine {
            AppEvent::EditorInput(EditorInput::Key {
                key: Key::Named(NamedKey::Enter),
                mods: Modifiers::empty(),
                text: None,
            })
        } else {
            AppEvent::EditorCommand(EditorCommand::Abort)
        };
        app.dispatch_native_event(wid, exit).unwrap();
        assert!(matches!(
            editor_buffer(&app, view).minibuffer,
            Minibuffer::Inactive
        ));
        assert_eq!(revert_available(&app), (true, true));
    }

    app.dispatch_native_event(
        wid,
        AppEvent::Action(crate::native_app::ActionInvocation {
            id: crate::native_ui::ActionId::new("editor/revert"),
            value: None,
        }),
    )
    .unwrap();
    assert_eq!(
        app.document_store.snapshot(document).unwrap().text.as_ref(),
        "disk three\n"
    );
    assert_eq!(app.document_store.dirty(document), Some(false));
    assert!(!app.native_documents.disk_conflicts.contains_key(&document));
    let Some(NativeApp::Editor(editor)) = app.native_runtime.app(instance) else {
        unreachable!();
    };
    assert!(!editor.disk_conflict);
    assert!(!app.native_documents.recovery_status.contains_key(&document));
    let selection = editor_buffer(&app, app.active_native_view(wid).unwrap().1)
        .primary_selection()
        .range();
    assert!(selection.is_empty());
    assert_eq!(selection.end, "disk three\n".len());
    let discarded = retained_state(&mut app);
    assert_eq!(
        (discarded.0, discarded.1, discarded.2),
        (false, None, false),
        "the retained scene clears conflict controls and recovery after discard"
    );
    assert_ne!(discarded.3, conflicted.3);

    commit(&mut app, wid, "resolved ");
    let resolved = retained_state(&mut app);
    assert_eq!(
        (resolved.0, resolved.1, resolved.2),
        (true, None, false),
        "fresh edits restore retained Save without a conflict action"
    );
    let save_enabled = app
        .native_runtime
        .commands(instance, view)
        .unwrap()
        .into_iter()
        .find(|command| command.id.as_str() == "editor/save")
        .expect("Save command")
        .enabled;
    assert!(
        save_enabled,
        "fresh edits are saveable after explicit reload"
    );
    app.save_document_checkpoint(document, view).unwrap();
    // The submitted Go to Line moved to line 2; Discard keeps that EOF caret.
    assert_eq!(fs::read_to_string(&path).unwrap(), "disk three\nresolved ");
    let _ = fs::remove_dir_all(dir);
    assert!(capability_failures.is_empty(), "{capability_failures:#?}");
    assert!(retained_failures.is_empty(), "{retained_failures:#?}");
}

#[test]
fn shipping_search_and_m_x_own_text_without_document_leakage() {
    let dir = fresh_dir("aterm-app-editor-minibuffer");
    let path = dir.join("modal.txt");
    fs::write(&path, "zero needle tail").unwrap();
    let mut app = App::headless_for_test();
    app.open_document_tab(AppKind::Editor, &file_uri(&path))
        .unwrap();
    let wid = WindowId(0);
    let (instance, view) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();
    let original = app.document_store.snapshot(document).unwrap();

    drive_native(&mut app, isearch_key());
    drive_native(&mut app, InputEvent::Text("needle".to_string()));
    let searched = app.document_store.snapshot(document).unwrap();
    assert_eq!(searched.seq, original.seq);
    assert_eq!(searched.text, original.text);
    assert_eq!(editor_buffer(&app, view).primary_selection().range(), 5..11);

    for event in [
        key(Key::Named(NamedKey::Delete), Modifiers::empty()),
        key(Key::Named(NamedKey::ArrowLeft), Modifiers::empty()),
        key(Key::Named(NamedKey::ArrowDown), Modifiers::empty()),
    ] {
        drive_native(&mut app, event);
    }
    assert_eq!(
        app.document_store.snapshot(document).unwrap().seq,
        original.seq
    );
    drive_native(
        &mut app,
        key(Key::Named(NamedKey::Escape), Modifiers::empty()),
    );
    assert_eq!(
        editor_buffer(&app, view).primary_selection(),
        &Selection::caret(0)
    );

    drive_native(&mut app, key(Key::Character('x'), Modifiers::ALT));
    drive_native(&mut app, InputEvent::Text("forward-char".to_string()));
    assert_eq!(
        app.document_store.snapshot(document).unwrap().seq,
        original.seq
    );
    drive_native(
        &mut app,
        key(Key::Named(NamedKey::Enter), Modifiers::empty()),
    );
    assert_eq!(
        editor_buffer(&app, view).primary_selection(),
        &Selection::caret(1)
    );

    drive_native(&mut app, key(Key::Character('x'), Modifiers::ALT));
    drive_native(
        &mut app,
        InputEvent::Text("definitely-not-a-command".to_string()),
    );
    drive_native(
        &mut app,
        key(Key::Named(NamedKey::Enter), Modifiers::empty()),
    );
    let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view) else {
        panic!("editor remains live");
    };
    assert!(matches!(
        state.buffer.as_ref().map(|buffer| &buffer.minibuffer),
        Some(Minibuffer::Command { query, .. }) if query == "definitely-not-a-command"
    ));
    assert_eq!(
        app.document_store.snapshot(document).unwrap().seq,
        original.seq
    );
    drive_native(&mut app, key(Key::Character('g'), Modifiers::CTRL));
    assert!(matches!(
        &editor_buffer(&app, view).minibuffer,
        Minibuffer::Inactive
    ));
    let _ = fs::remove_dir_all(dir);
}

fn submit_buffer_query(app: &mut App, command: char, query: &str) {
    drive_native(app, key(Key::Character('x'), Modifiers::CTRL));
    drive_native(app, key(Key::Character(command), Modifiers::empty()));
    drive_native(app, InputEvent::Text(query.to_string()));
    drive_native(app, key(Key::Named(NamedKey::Enter), Modifiers::empty()));
}

fn restored_editor_pair(first: &str, second: &str, zoomed: bool) -> App {
    let mut app = App::headless_for_test();
    let mut layout = app.capture_restore_manifest().windows.remove(0);
    layout.tabs.clear();
    layout.native_tabs.clear();
    layout.tab_order.clear();
    layout.active_item = Some(0);
    let leaf = |uri: &str| {
        crate::restore::RestoredSplitTree::leaf(crate::restore::RestoredView::Native(
            crate::restore::NativeLeafRestore::document("editor", uri.to_string()),
        ))
    };
    layout.restored_tabs = vec![crate::restore::RestoredTab {
        root: crate::restore::RestoredSplitTree::Split {
            axis: crate::restore::SplitKind::Horizontal,
            ratio: 0.6,
            first: Box::new(leaf(first)),
            second: Box::new(leaf(second)),
        },
        focused_path: vec![crate::restore::RestoreBranch::Second],
        zoomed,
        parked: false,
        ephemeral: false,
        fleet: false,
    }];
    app.restore_into_window(WindowId(0), layout);
    app
}

#[test]
fn shipping_c_x_b_switches_only_to_an_exact_open_editor_buffer() {
    let dir = fresh_dir("aterm-app-editor-buffer");
    let first_path = dir.join("first.txt");
    let second_path = dir.join("second.txt");
    fs::write(&first_path, "first bytes").unwrap();
    fs::write(&second_path, "second bytes").unwrap();
    let first_uri = file_uri(&first_path);
    let second_uri = file_uri(&second_path);
    let mut app = App::headless_for_test();
    app.open_document_tab(AppKind::Editor, &first_uri).unwrap();
    let first_document = app
        .native_runtime
        .document_id(app.active_native_view(WindowId(0)).unwrap().0)
        .unwrap();
    app.open_document_tab(AppKind::Editor, &second_uri).unwrap();
    let second_document = app
        .native_runtime
        .document_id(app.active_native_view(WindowId(0)).unwrap().0)
        .unwrap();
    let first_before = app.document_store.snapshot(first_document).unwrap();
    let second_before = app.document_store.snapshot(second_document).unwrap();

    drive_native(&mut app, key(Key::Character('x'), Modifiers::CTRL));
    drive_native(&mut app, key(Key::Character('b'), Modifiers::empty()));
    drive_native(&mut app, InputEvent::Text("first.txt".to_string()));
    assert_eq!(
        app.document_store.snapshot(second_document).unwrap().seq,
        second_before.seq
    );
    drive_native(
        &mut app,
        key(Key::Named(NamedKey::Enter), Modifiers::empty()),
    );
    let (active, _) = app.active_native_view(WindowId(0)).unwrap();
    assert_eq!(app.native_runtime.document_id(active), Some(first_document));
    assert_eq!(
        app.document_store.snapshot(first_document).unwrap().text,
        first_before.text
    );
    assert_eq!(
        app.document_store.snapshot(second_document).unwrap().text,
        second_before.text
    );

    app.open_document_tab(AppKind::Editor, &second_uri).unwrap();
    drive_native(&mut app, key(Key::Character('x'), Modifiers::CTRL));
    drive_native(&mut app, key(Key::Character('b'), Modifiers::empty()));
    drive_native(&mut app, InputEvent::Text("missing.txt".to_string()));
    drive_native(
        &mut app,
        key(Key::Named(NamedKey::Enter), Modifiers::empty()),
    );
    let (active, active_view) = app.active_native_view(WindowId(0)).unwrap();
    assert_eq!(
        app.native_runtime.document_id(active),
        Some(second_document)
    );
    assert!(matches!(
        app.native_runtime.view_state(active_view),
        Some(AppViewState::Editor(_))
    ));
    assert_eq!(
        app.document_store.snapshot(second_document).unwrap().seq,
        second_before.seq
    );

    let elsewhere = dir.join("elsewhere.txt");
    fs::write(&elsewhere, "another window").unwrap();
    let elsewhere_uri = file_uri(&elsewhere);
    let remote_session = app.next_session_id;
    let remote = app.insert_logical_window(crate::stub_session(remote_session), 24, 80);
    app.open_document_tab_in_window(remote, AppKind::Editor, &elsewhere_uri)
        .unwrap();
    let (remote_instance, _) = app.active_native_view(remote).unwrap();
    let remote_document = app.native_runtime.document_id(remote_instance).unwrap();
    let elsewhere_query = app
        .document_store
        .canonical_uri(remote_document)
        .unwrap()
        .to_string();
    let remote_before = app.document_store.snapshot(remote_document).unwrap();
    let remote_tabs = app.windows[&remote].tab_set.tabs().to_vec();
    app.frontmost_window = Some(WindowId(0));
    app.sync_active_session();
    let local_tabs = app.windows[&WindowId(0)].tab_set.tabs().to_vec();
    for command in ['b', 'k'] {
        submit_buffer_query(&mut app, command, &elsewhere_query);
        assert_eq!(app.frontmost_window, Some(WindowId(0)));
        assert_eq!(app.windows[&WindowId(0)].tab_set.tabs(), local_tabs);
        assert_eq!(app.windows[&remote].tab_set.tabs(), remote_tabs);
        let after = app.document_store.snapshot(remote_document).unwrap();
        assert_eq!(
            (after.seq, after.text),
            (remote_before.seq, remote_before.text.clone())
        );
        assert_eq!(fs::read_to_string(&elsewhere).unwrap(), "another window");
    }

    let left = dir.join("left");
    let right = dir.join("right");
    fs::create_dir_all(&left).unwrap();
    fs::create_dir_all(&right).unwrap();
    let left_path = left.join("shared.txt");
    let right_path = right.join("shared.txt");
    let left_uri = file_uri(&left_path);
    let right_uri = file_uri(&right_path);
    let wid = WindowId(0);
    let mut failures = Vec::new();
    for terminal_sibling in [false, true] {
        for zoomed in [false, true] {
            for by_uri in [false, true] {
                fs::write(&left_path, "first bytes").unwrap();
                fs::write(&right_path, "second bytes").unwrap();
                let mut app = if terminal_sibling {
                    let mut app = App::headless_for_test();
                    app.open_document_tab(AppKind::Editor, &left_uri).unwrap();
                    app.split_active_with_stub_terminal(
                        wid,
                        crate::tab_model::SplitAxis::Horizontal,
                    );
                    app.windows
                        .get_mut(&wid)
                        .unwrap()
                        .tab_set
                        .active_mut()
                        .unwrap()
                        .zoomed = zoomed;
                    app.open_document_tab(AppKind::Editor, &right_uri).unwrap();
                    app
                } else {
                    restored_editor_pair(&left_uri, &right_uri, zoomed)
                };
                let target = app.windows[&wid]
                    .tab_set
                    .tabs()
                    .iter()
                    .find(|tab| tab.root.len() == 2)
                    .unwrap();
                let target_tab = target.id;
                let target_root = target.root.clone();
                let target_view = target_root.leaves()[0];
                let target_instance = match app.view_store.get(target_view).unwrap() {
                    crate::tab_model::View::Native(native) => native.instance,
                    _ => panic!("first split leaf is the requested editor"),
                };
                let document = app.native_runtime.document_id(target_instance).unwrap();
                let (source_instance, source_view) = app.active_native_view(wid).unwrap();
                let source_document = app.native_runtime.document_id(source_instance).unwrap();
                assert_ne!(source_document, document);
                assert_ne!(target.focus, target_view, "the target starts unfocused");
                let before_source = app.document_store.snapshot(source_document).unwrap();
                let before = app.document_store.snapshot(document).unwrap();
                let count = (app.windows[&wid].tab_set.len(), app.view_store.len());
                let query = if by_uri {
                    app.document_store
                        .canonical_uri(document)
                        .unwrap()
                        .to_string()
                } else {
                    let Some(NativeApp::Editor(editor)) = app.native_runtime.app(target_instance)
                    else {
                        panic!("target Editor");
                    };
                    assert_ne!(
                        editor.title, "shared.txt",
                        "real titles disambiguate distinct files"
                    );
                    editor.title.clone()
                };
                let case = format!(
                    "terminal_sibling={terminal_sibling}, zoomed={zoomed}, by_uri={by_uri}"
                );
                let resolved = app
                    .resolve_open_editor_buffer(source_view, &query)
                    .expect("the admitted unique title or canonical URI resolves before input");
                assert_eq!(
                    (resolved.2, resolved.3),
                    (target_instance, target_view),
                    "{case}"
                );
                submit_buffer_query(&mut app, 'b', &query);
                if app.active_native_view(wid) != Some((target_instance, target_view))
                    || app.windows[&wid].tab_set.active().unwrap().focus != target_view
                {
                    failures.push(format!(
                        "{case}: Switch Buffer did not focus the resolved editor leaf"
                    ));
                }
                if !app.native_input_event(wid, &InputEvent::Text("chosen ".to_string())) {
                    failures.push(format!(
                        "{case}: subsequent editor input had no native owner"
                    ));
                }
                if let Err(why) = app.save_front_document() {
                    failures.push(format!(
                        "{case}: File Save did not reach the chosen editor: {why}"
                    ));
                }
                let after = app.document_store.snapshot(document).unwrap();
                if after.text.as_ref() != "chosen first bytes"
                    || after.seq <= before.seq
                    || app.document_store.checkpoint_seq(document) != Some(after.seq)
                    || app.document_store.dirty(document) != Some(false)
                    || fs::read_to_string(&left_path).unwrap() != "chosen first bytes"
                {
                    failures.push(format!(
                        "{case}: chosen editor did not receive and save the input"
                    ));
                }
                let source_after = app.document_store.snapshot(source_document).unwrap();
                if (source_after.seq, source_after.text)
                    != (before_source.seq, before_source.text.clone())
                    || fs::read_to_string(&right_path).unwrap() != "second bytes"
                {
                    failures.push(format!(
                        "{case}: the prior source editor received the chosen editor's input or Save"
                    ));
                }
                assert!(matches!(
                    &editor_buffer(&app, source_view).minibuffer,
                    Minibuffer::Inactive
                ));
                let target = app.windows[&wid].tab_set.get(target_tab).unwrap();
                assert_eq!(target.root, target_root);
                assert_eq!(target.zoomed, zoomed);
                assert_eq!(
                    (app.windows[&wid].tab_set.len(), app.view_store.len()),
                    count
                );
                assert_eq!(app.frontmost_window, Some(wid));
                assert!(app.structural_invariants_ok());
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn shipping_buffer_queries_refuse_ambiguous_views_before_switch_or_kill() {
    let dir = fresh_dir("aterm-app-editor-buffer-ambiguity");
    let path = dir.join("shared.txt");
    fs::write(&path, "shared bytes").unwrap();
    let uri = file_uri(&path);
    let wid = WindowId(0);
    let mut failures = Vec::new();
    for separate_tabs in [false, true] {
        for zoomed in [false, true] {
            if separate_tabs && zoomed {
                continue;
            }
            for command in ['b', 'k'] {
                for by_uri in [false, true] {
                    let mut app = restored_editor_pair(&uri, &uri, zoomed);
                    if separate_tabs {
                        let mut layout = app.capture_restore_manifest().windows.remove(0);
                        let pair = layout.restored_tabs.remove(0);
                        let crate::restore::RestoredSplitTree::Split { first, second, .. } =
                            pair.root
                        else {
                            panic!("restored pair");
                        };
                        layout.restored_tabs = [*first, *second]
                            .into_iter()
                            .map(|root| crate::restore::RestoredTab {
                                root,
                                focused_path: Vec::new(),
                                zoomed: false,
                                parked: false,
                                ephemeral: false,
                                fleet: false,
                            })
                            .collect();
                        layout.active_item = Some(1);
                        app.restore_into_window(wid, layout);
                    }
                    let (_, source_view) = app.active_native_view(wid).unwrap();
                    let before_tabs = app.windows[&wid].tab_set.tabs().to_vec();
                    let views = before_tabs
                        .iter()
                        .flat_map(|tab| tab.root.leaves())
                        .collect::<Vec<_>>();
                    assert_eq!(views.len(), 2);
                    let before_views = views
                        .iter()
                        .map(|&view| {
                            (
                                view,
                                app.native_runtime.view_generation(view),
                                editor_buffer(&app, view).selections.clone(),
                                editor_buffer(&app, view).viewport_anchor,
                            )
                        })
                        .collect::<Vec<_>>();
                    let document = editor_buffer(&app, source_view).document;
                    assert!(
                        views
                            .iter()
                            .all(|&view| editor_buffer(&app, view).document == document),
                        "both restored views share the admitted canonical document"
                    );
                    let query = if by_uri {
                        app.document_store
                            .canonical_uri(document)
                            .unwrap()
                            .to_string()
                    } else {
                        "shared.txt".to_string()
                    };
                    let before = app.document_store.snapshot(document).unwrap();
                    let checkpoint = app.document_store.checkpoint_seq(document);
                    let count = app.view_store.len();
                    let case = format!(
                        "separate_tabs={separate_tabs}, zoomed={zoomed}, command={command}, query={query}"
                    );
                    if app.resolve_open_editor_buffer(source_view, &query).is_ok() {
                        failures.push(format!(
                            "{case}: an ambiguous view query resolved to one sibling"
                        ));
                    }
                    submit_buffer_query(&mut app, command, &query);
                    if app.windows[&wid].tab_set.tabs() != before_tabs
                        || app.view_store.len() != count
                    {
                        failures.push(format!(
                            "{case}: ambiguous switch/kill changed topology or focus"
                        ));
                    }
                    for (view, generation, selections, viewport) in before_views {
                        if app.native_runtime.view_generation(view) != generation {
                            failures.push(format!(
                                "{case}: ambiguous switch/kill detached view {view:?}"
                            ));
                            continue;
                        }
                        let buffer = editor_buffer(&app, view);
                        if buffer.selections != selections || buffer.viewport_anchor != viewport {
                            failures.push(format!(
                                "{case}: ambiguous switch/kill changed view {view:?}"
                            ));
                        }
                    }
                    let after = app.document_store.snapshot(document).unwrap();
                    assert_eq!((after.seq, after.text), (before.seq, before.text));
                    assert_eq!(app.document_store.checkpoint_seq(document), checkpoint);
                    assert_eq!(app.document_store.dirty(document), Some(false));
                    assert_eq!(fs::read_to_string(&path).unwrap(), "shared bytes");
                    assert_eq!(app.frontmost_window, Some(wid));
                    assert!(app.structural_invariants_ok());
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    let _ = fs::remove_dir_all(dir);
}

/// End `app` as a crash ends its process: its journal locks are gone before anyone reopens.
/// Dropping an App only asks its journal custody worker to release; the worker settles each
/// journal under that journal's lock afterwards, which a reopen's event-loop take must not meet.
fn crash(mut app: App) {
    let finished = app
        .native_documents
        .journals
        .as_mut()
        .and_then(|store| store.custody_finished_for_test());
    drop(app);
    if let Some(finished) = finished {
        finished
            .lock()
            .unwrap()
            .recv_timeout(std::time::Duration::from_secs(60))
            .expect("the crashed App's journal custody worker finished");
    }
}

#[test]
fn production_open_recovers_durable_draft_before_publishing_editor_view() {
    let dir = fresh_dir("aterm-app-crash-recovery");
    let path = dir.join("recover.md");
    let journal_root = dir.join("private-drafts");
    fs::write(&path, "disk\n").unwrap();
    let uri = file_uri(&path);

    {
        let mut crashed = App::headless_for_test();
        crashed.native_documents.journals = JournalSlot::for_test(journal_root.clone());
        crashed.open_document_tab(AppKind::Editor, &uri).unwrap();
        commit(&mut crashed, WindowId(0), "unsaved ");
        let (instance, _) = crashed.active_native_view(WindowId(0)).unwrap();
        let document = crashed.native_runtime.document_id(instance).unwrap();
        assert_eq!(
            crashed
                .document_store
                .snapshot(document)
                .unwrap()
                .text
                .as_ref(),
            "unsaved disk\n"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "disk\n");
        crash(crashed);
    }

    let mut reopened = App::headless_for_test();
    reopened.native_documents.journals = JournalSlot::for_test(journal_root.clone());
    reopened.open_document_tab(AppKind::Editor, &uri).unwrap();
    let (instance, _) = reopened.active_native_view(WindowId(0)).unwrap();
    let document = reopened.native_runtime.document_id(instance).unwrap();
    assert_eq!(
        reopened
            .document_store
            .snapshot(document)
            .unwrap()
            .text
            .as_ref(),
        "unsaved disk\n"
    );
    assert_eq!(reopened.document_store.dirty(document), Some(true));
    let Some(NativeApp::Editor(editor)) = reopened.native_runtime.app(instance) else {
        panic!("recovered view must be Editor")
    };
    assert!(editor.recovery_status.is_some());
    assert_eq!(fs::read_to_string(&path).unwrap(), "disk\n");
    let _ = fs::remove_dir_all(dir);
}

/// A recovered draft is where the reopened buffer's undo starts: C-/
/// stops at it and says how to take the file's text instead, so the
/// draft stays unsaved and journaled, and closing still asks. Typing
/// after it undoes as usual (the negative control: Undo works there).
#[test]
fn undo_stops_at_a_recovered_draft_and_keeps_it_unsaved() {
    let dir = fresh_dir("aterm-app-recovered-floor");
    let path = dir.join("recover.md");
    let journal_root = dir.join("private-drafts");
    fs::write(&path, "disk\n").unwrap();
    let uri = file_uri(&path);
    {
        let mut crashed = App::headless_for_test();
        crashed.native_documents.journals = JournalSlot::for_test(journal_root.clone());
        crashed.open_document_tab(AppKind::Editor, &uri).unwrap();
        commit(&mut crashed, WindowId(0), "unsaved ");
        crash(crashed);
    }
    let mut reopened = App::headless_for_test();
    reopened.native_documents.journals = JournalSlot::for_test(journal_root.clone());
    reopened.open_document_tab(AppKind::Editor, &uri).unwrap();
    let views = reopened.active_native_view(WindowId(0)).unwrap();
    let document = reopened.native_runtime.document_id(views.0).unwrap();
    let text = |app: &App| app.document_store.snapshot(document).unwrap().text;
    assert_eq!(text(&reopened).as_ref(), "unsaved disk\n");
    assert!(
        !reopened.editor_workspace.can_undo(document),
        "Undo is off at the draft"
    );
    undo(&mut reopened, WindowId(0));
    assert_eq!(
        text(&reopened).as_ref(),
        "unsaved disk\n",
        "C-/ kept the draft"
    );
    assert_eq!(reopened.document_store.dirty(document), Some(true));
    assert_eq!(
        editor_status(&reopened, views.1),
        crate::native_editor::RECOVERED_FLOOR
    );

    commit(&mut reopened, WindowId(0), "more ");
    assert!(reopened.editor_workspace.can_undo(document));
    undo(&mut reopened, WindowId(0));
    assert_eq!(text(&reopened).as_ref(), "unsaved disk\n");
    assert_eq!(reopened.document_store.dirty(document), Some(true));
    assert_eq!(fs::read_to_string(&path).unwrap(), "disk\n");
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn production_open_surfaces_disk_conflict_and_preserves_draft() {
    let dir = fresh_dir("aterm-app-recovery-conflict");
    let path = dir.join("conflict.md");
    let journal_root = dir.join("private-drafts");
    fs::write(&path, "base\n").unwrap();
    let uri = file_uri(&path);

    {
        let mut crashed = App::headless_for_test();
        crashed.native_documents.journals = JournalSlot::for_test(journal_root.clone());
        crashed.open_document_tab(AppKind::Editor, &uri).unwrap();
        commit(&mut crashed, WindowId(0), "draft ");
        crash(crashed);
    }
    fs::write(&path, "external\n").unwrap();

    let mut reopened = App::headless_for_test();
    reopened.native_documents.journals = JournalSlot::for_test(journal_root.clone());
    reopened.open_document_tab(AppKind::Editor, &uri).unwrap();
    let (instance, _) = reopened.active_native_view(WindowId(0)).unwrap();
    let document = reopened.native_runtime.document_id(instance).unwrap();
    assert_eq!(
        reopened
            .document_store
            .snapshot(document)
            .unwrap()
            .text
            .as_ref(),
        "external\n",
        "conflicting recovery never silently replaces newer disk bytes"
    );
    let Some(NativeApp::Editor(editor)) = reopened.native_runtime.app(instance) else {
        panic!("conflict view must be Editor")
    };
    assert!(editor.recovery_status.is_some());
    assert!(
        fs::read_dir(&journal_root).unwrap().any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains(".preserved-")),
        "conflicting draft material remains in the private recovery directory"
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn a_dirty_document_leaf_that_closes_inside_a_split_quiet_holds_the_take_back() {
    let dir = fresh_dir("aterm-doc-close-split-hold");
    let path = dir.join("held.md");
    fs::write(&path, "before\n").unwrap();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.open_document_tab(AppKind::Editor, &file_uri(&path))
        .unwrap();
    let (instance, editor_view) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();
    commit(&mut app, wid, "safe ");
    assert_eq!(app.document_store.dirty(document), Some(true));
    // An odd width splits evenly either side of the divider, so the document's close hands
    // the sibling back exactly the grid the split took.
    app.windows.get_mut(&wid).unwrap().cols = 81;
    app.resize_panes(wid);
    let grid = |app: &App, session: u64| {
        let t = crate::term_lock(&app.pool.get(session).expect("live session").term);
        (t.rows(), t.cols())
    };
    let (sibling, _) =
        app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
    let beside = grid(&app, sibling);
    let _ = app.split_active_stub_tab_dir(wid, crate::pane::SplitDir::Vertical);
    let narrowed = grid(&app, sibling);
    assert!(
        narrowed.1 < beside.1 && narrowed.0 == beside.0,
        "the split narrows the sibling at once: {beside:?} -> {narrowed:?}"
    );
    app.split_regrid_holds
        .prolong(std::time::Duration::from_secs(60));

    app.windows
        .get_mut(&wid)
        .unwrap()
        .tab_set
        .active_mut()
        .unwrap()
        .set_focus(editor_view);
    app.sync_active_session();
    app.close_active_tab();

    let tab = app.windows[&wid].tab_set.active().unwrap();
    assert!(
        !tab.root.contains(editor_view),
        "the saved document leaf left the tab"
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), "safe before\n");
    assert_eq!(
        grid(&app, sibling),
        narrowed,
        "the take-back to the grid the split took is withheld inside the quiet"
    );
    let due = app
        .split_regrid_hold_deadline()
        .expect("a withheld take-back owes one wake");
    app.service_split_regrid_holds(due);
    assert_eq!(
        grid(&app, sibling),
        beside,
        "the take-back lands after the quiet"
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn a_document_leaf_whose_save_lands_behind_a_later_tab_leaves_that_tab_in_front() {
    for settings_in_front in [false, true] {
        let at = if settings_in_front { "parked" } else { "front" };
        let dir = fresh_dir(&format!("aterm-doc-close-behind-{at}"));
        let path = dir.join("behind.md");
        fs::write(&path, "before\n").unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.open_document_tab(AppKind::Editor, &file_uri(&path))
            .unwrap();
        let (instance, editor_view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        let mixed = app.windows[&wid].tab_set.active_id().unwrap();
        let (left_behind, _) =
            app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
        app.windows
            .get_mut(&wid)
            .unwrap()
            .tab_set
            .active_mut()
            .unwrap()
            .set_focus(editor_view);
        app.sync_active_session();
        commit(&mut app, wid, "held ");
        let held = app.hold_document_save_for_test(document);
        app.close_active_tab();
        assert!(app.document_close_pending_for_test(document), "{at}");

        // A terminal tab after the mixed one takes the front while the save is in
        // flight, or holds the parked slot behind a Settings tab that took it.
        let front = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(front));
        assert_eq!(app.focused_session_id(wid), Some(front), "{at}");
        if settings_in_front {
            assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));
        }
        app.land_held_document_save_for_test(document, editor_view, held)
            .unwrap();

        assert!(!app.document_close_pending_for_test(document), "{at}");
        assert!(
            !app.windows[&wid]
                .tab_set
                .get(mixed)
                .unwrap()
                .root
                .contains(editor_view),
            "{at}: the saved document leaf left its tab"
        );
        assert_eq!(
            app.structural_invariant_violation(),
            None,
            "{at}: the promoted tab's projection slot lands before the front tab's"
        );
        let ws = &app.windows[&wid];
        assert_eq!(
            ws.layouts
                .get(ws.tabs.active)
                .map(crate::pane::PaneTree::sessions),
            Some(vec![front]),
            "{at}: the active or parked slot still names the later tab"
        );
        if settings_in_front {
            app.close_active_tab();
        }
        assert_eq!(app.focused_session_id(wid), Some(front), "{at}");
        assert_eq!(
            app.active_tree(wid).map(crate::pane::PaneTree::sessions),
            Some(vec![front]),
            "{at}"
        );
        app.close_active_tab();
        assert!(
            app.pool.get(front).is_none(),
            "{at}: Cmd-W closes the tab in front"
        );
        assert!(
            app.windows[&wid].tab_set.get(mixed).is_some() && app.pool.get(left_behind).is_some(),
            "{at}: and not the tab whose save landed behind it"
        );
        let _ = fs::remove_dir_all(dir);
    }
}

#[test]
fn a_dirty_document_tab_closed_from_behind_leaves_the_front_while_it_saves() {
    let dir = fresh_dir("aterm-doc-close-from-behind");
    let path = dir.join("behind.md");
    fs::write(&path, "before\n").unwrap();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.open_document_tab(AppKind::Editor, &file_uri(&path))
        .unwrap();
    let (instance, view) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();
    commit(&mut app, wid, "held ");
    let held = app.hold_document_save_for_test(document);
    app.switch_tab_in(wid, 0);

    assert!(!app.close_tab_at(wid, 1), "the close waits for the save");
    assert_eq!(
        app.windows[&wid].tab_set.len(),
        2,
        "the document stays open"
    );
    assert_eq!(
        app.windows[&wid].tab_set.active_index(),
        Some(0),
        "the refused close leaves the person's front tab in front"
    );
    assert_eq!(app.focused_session_id(wid), Some(0));
    assert_eq!(app.structural_invariant_violation(), None);

    app.land_held_document_save_for_test(document, view, held)
        .unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "held before\n");
    assert_eq!(
        app.windows[&wid].tab_set.len(),
        1,
        "the saved tab closes without a second request"
    );
    assert!(!app.document_close_pending_for_test(document));
    assert_eq!(app.focused_session_id(wid), Some(0));
    assert_eq!(app.structural_invariant_violation(), None);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn a_whole_tab_close_waits_for_every_async_document_and_forgets_its_intents() {
    let dir = fresh_dir("aterm-tab-close-atomic-saves");
    let a = dir.join("a.md");
    let b = dir.join("b.md");
    fs::write(&a, "a\n").unwrap();
    fs::write(&b, "b\n").unwrap();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let mut layout = app.capture_restore_manifest().windows.remove(0);
    layout.tabs.clear();
    layout.native_tabs.clear();
    layout.tab_order.clear();
    layout.active_item = Some(0);
    layout.restored_tabs =
        vec![crate::restore::RestoredTab {
            root: crate::restore::RestoredSplitTree::Split {
                axis: crate::restore::SplitKind::Horizontal,
                ratio: 0.5,
                first: Box::new(crate::restore::RestoredSplitTree::leaf(
                    crate::restore::RestoredView::Native(
                        crate::restore::NativeLeafRestore::document("editor", file_uri(&a)),
                    ),
                )),
                second: Box::new(crate::restore::RestoredSplitTree::leaf(
                    crate::restore::RestoredView::Native(
                        crate::restore::NativeLeafRestore::document("editor", file_uri(&b)),
                    ),
                )),
            },
            focused_path: vec![crate::restore::RestoreBranch::First],
            zoomed: false,
            parked: false,
            ephemeral: false,
            fleet: false,
        }];
    app.restore_into_window(wid, layout);
    let tab = app.windows[&wid].tab_set.active().unwrap();
    let tab_id = tab.id;
    let views = tab.root.leaves();
    let documents = views
        .iter()
        .map(|view| {
            let crate::tab_model::View::Native(native) =
                app.view_store.get(*view).copied().unwrap()
            else {
                panic!("native document")
            };
            app.native_runtime.document_id(native.instance).unwrap()
        })
        .collect::<Vec<_>>();
    commit(&mut app, wid, "one ");
    app.windows
        .get_mut(&wid)
        .unwrap()
        .tab_set
        .active_mut()
        .unwrap()
        .set_focus(views[1]);
    app.sync_active_session();
    commit(&mut app, wid, "two ");
    let first = app.hold_document_save_for_test(documents[0]);
    let second = app.hold_document_save_for_test(documents[1]);
    assert!(app.close_active_native_tab(wid).is_err());
    app.land_held_document_save_for_test(documents[0], views[0], first)
        .unwrap();
    assert!(
        app.windows[&wid].tab_set.get(tab_id).is_some(),
        "one saved member cannot detach the other"
    );
    assert_eq!(app.document_store.view_count(documents[0]), Some(1));
    assert_eq!(app.document_store.view_count(documents[1]), Some(1));
    assert_eq!(fs::read_to_string(&b).unwrap(), "b\n");
    assert!(
        app.native_documents
            .persistence
            .grant(documents[1])
            .is_some()
    );
    app.land_held_document_save_for_test(documents[1], views[1], second)
        .unwrap();
    assert!(app.windows[&wid].tab_set.get(tab_id).is_none());
    for document in documents {
        assert_eq!(app.document_store.view_count(document), Some(0));
        assert!(!app.document_close_pending_for_test(document));
    }
    assert_eq!(fs::read_to_string(&a).unwrap(), "one a\n");
    assert_eq!(fs::read_to_string(&b).unwrap(), "two b\n");
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn final_dirty_document_leaf_is_saved_before_only_that_split_leaf_detaches() {
    let dir = fresh_dir("aterm-app-leaf-close");
    let path = dir.join("leaf-close.md");
    fs::write(&path, "before\n").unwrap();
    let uri = file_uri(&path);
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let mut layout = app.capture_restore_manifest().windows.remove(0);
    layout.tabs.clear();
    layout.native_tabs.clear();
    layout.tab_order.clear();
    layout.active_item = Some(0);
    layout.restored_tabs = vec![crate::restore::RestoredTab {
        root: crate::restore::RestoredSplitTree::Split {
            axis: crate::restore::SplitKind::Horizontal,
            ratio: 0.63,
            first: Box::new(crate::restore::RestoredSplitTree::leaf(
                crate::restore::RestoredView::Native(crate::restore::NativeLeafRestore::document(
                    "editor",
                    uri.clone(),
                )),
            )),
            second: Box::new(crate::restore::RestoredSplitTree::leaf(
                crate::restore::RestoredView::Native(crate::restore::NativeLeafRestore::settings(
                    "/about".to_string(),
                )),
            )),
        },
        focused_path: vec![crate::restore::RestoreBranch::First],
        zoomed: false,
        parked: false,
        ephemeral: false,
        fleet: false,
    }];
    let collapse_layout = layout.clone();
    let mut failures = Vec::new();
    app.restore_into_window(wid, layout);
    let (editor_instance, editor_view) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(editor_instance).unwrap();
    let original = app.windows[&wid].tab_set.active().unwrap();
    let original_tab = original.id;
    let original_root = original.root.clone();
    assert_eq!(original_root.leaves().len(), 2);
    let settings_view = original_root.leaves()[1];
    let settings_generation = app.native_runtime.view_generation(settings_view);
    let settings_state = |app: &App| {
        let Some(AppViewState::Settings(state)) = app.native_runtime.view_state(settings_view)
        else {
            panic!("the Settings sibling stays attached");
        };
        (state.route, state.search.clone(), state.page_scroll)
    };
    let settings_before = settings_state(&app);
    assert_eq!(
        settings_before.0,
        crate::native_settings::SettingsRoute::About
    );
    assert!(app.closed_recovery.views.is_empty());
    assert!(app.closed_recovery.tabs.is_empty());
    commit(&mut app, wid, "safe ");
    let edited = app.document_store.snapshot(document).unwrap();
    assert_eq!(edited.text.as_ref(), "safe before\n");
    assert_eq!(app.document_store.dirty(document), Some(true));

    app.close_active_tab();

    let active = app.windows[&wid].tab_set.active().unwrap();
    assert_eq!(active.id, original_tab);
    assert_eq!(active.root.leaves(), vec![settings_view]);
    assert_eq!(active.focus, settings_view);
    assert_eq!(app.document_store.view_count(document), Some(0));
    assert_eq!(
        app.document_store.checkpoint_seq(document),
        Some(edited.seq)
    );
    assert_eq!(app.document_store.dirty(document), Some(false));
    assert!(app.view_store.get(editor_view).is_none());
    assert!(app.native_runtime.view_state(editor_view).is_none());
    assert!(app.native_documents.pending_closes.is_empty());
    assert_eq!(settings_state(&app), settings_before);
    assert_eq!(
        app.native_runtime.view_generation(settings_view),
        settings_generation
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), "safe before\n");
    assert!(app.structural_invariants_ok());
    let recovery_count = app.closed_recovery.views.len();
    assert!(app.closed_recovery.tabs.is_empty());
    if recovery_count == 1 {
        app.reopen_last_closed_view().unwrap();
        let (reopened_instance, reopened_view) = app.active_native_view(wid).unwrap();
        assert_ne!(reopened_view, editor_view);
        assert_ne!(reopened_instance, editor_instance);
        assert_eq!(
            app.native_runtime.document_id(reopened_instance),
            Some(document)
        );
        let reopened = app.windows[&wid].tab_set.active().unwrap();
        assert_eq!(reopened.id, original_tab);
        assert_eq!(
            reopened.root,
            original_root.map(&mut |view| {
                if *view == editor_view {
                    reopened_view
                } else {
                    *view
                }
            })
        );
        assert_eq!(reopened.focus, reopened_view);
        assert_eq!(app.windows[&wid].tab_set.len(), 1);
        assert_eq!(app.document_store.view_count(document), Some(1));
        let snapshot = app.document_store.snapshot(document).unwrap();
        assert_eq!((snapshot.seq, snapshot.text), (edited.seq, edited.text));
        assert_eq!(app.document_store.dirty(document), Some(false));
        assert_eq!(settings_state(&app), settings_before);
        assert_eq!(
            app.native_runtime.view_generation(settings_view),
            settings_generation
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "safe before\n");
        assert!(app.closed_recovery.views.is_empty());
        assert!(app.closed_recovery.tabs.is_empty());
        assert!(
            app.reopen_last_closed_view().is_err(),
            "the saved close is recorded once"
        );
        assert!(app.structural_invariants_ok());
    }
    if recovery_count != 1 {
        failures.push("durable pane removal did not retain exactly one pane recovery record");
    }
    drop(app);

    // A sibling can close while the document's real save is still in
    // flight, leaving the pending pane close responsible for the whole tab.
    for keep_foreground in [true, false] {
        fs::write(&path, "before\n").unwrap();
        let mut app = App::headless_for_test();
        app.restore_into_window(wid, collapse_layout.clone());
        let (editor_instance, editor_view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(editor_instance).unwrap();
        let tab = app.windows[&wid].tab_set.active().unwrap();
        let original_tab = tab.id;
        let settings_view = tab.root.leaves()[1];
        commit(&mut app, wid, "held ");
        let edited = app.document_store.snapshot(document).unwrap();
        let held = app.hold_document_save_for_test(document);
        app.close_active_tab();
        assert!(app.document_close_pending_for_test(document));
        assert_eq!(app.document_store.view_count(document), Some(1));
        assert_eq!(fs::read_to_string(&path).unwrap(), "before\n");
        assert!(app.closed_recovery.views.is_empty());
        assert!(app.closed_recovery.tabs.is_empty());
        assert!(
            app.windows
                .get_mut(&wid)
                .unwrap()
                .tab_set
                .active_mut()
                .unwrap()
                .set_focus(settings_view)
        );
        app.resync_active_or_window(wid);
        app.close_active_tab();
        let remaining = app.windows[&wid].tab_set.get(original_tab).unwrap();
        assert_eq!(remaining.root.leaves(), vec![editor_view]);
        assert!(app.view_store.get(settings_view).is_none());
        assert_eq!(
            app.closed_recovery.views.len(),
            1,
            "the sibling's own close is recoverable"
        );
        assert!(app.closed_recovery.tabs.is_empty());
        assert!(app.native_documents.inflight.contains(&document));

        let terminal_text = |app: &App, session| {
            let term = crate::term_lock(&app.pool.get(session).unwrap().term);
            (0..usize::from(term.grid().rows()))
                .map(|row| term.row_text(row))
                .collect::<Vec<_>>()
        };
        let foreground = keep_foreground.then(|| {
            let session = app.next_session_id;
            app.push_stub_tab(wid, crate::stub_session(session));
            let tab = app.windows[&wid].tab_set.active().unwrap().clone();
            let layout = app.windows[&wid].layouts[0].clone();
            let term = app.pool.get(session).unwrap().term.clone();
            crate::term_lock(&term).process(b"FOREGROUND STAYS");
            let text = terminal_text(&app, session);
            (session, tab, layout, term, text)
        });
        app.land_held_document_save_for_test(document, editor_view, held)
            .unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "held before\n");
        assert_eq!(
            app.document_store.checkpoint_seq(document),
            Some(edited.seq)
        );
        assert_eq!(app.document_store.dirty(document), Some(false));
        assert_eq!(app.document_store.view_count(document), Some(0));
        assert!(!app.document_close_pending_for_test(document));
        assert_eq!(app.frontmost_window, Some(wid));
        if let Some((session, tab, layout, term, text)) = &foreground {
            assert_eq!(app.windows[&wid].tab_set.active_id(), Some(tab.id));
            assert_eq!(app.windows[&wid].tab_set.get(tab.id), Some(tab));
            assert_eq!(&app.windows[&wid].layouts[0], layout);
            assert_eq!(app.clipboard_session_in(wid), Some(*session));
            assert_eq!(
                app.active_handle.lock().unwrap().as_ref().unwrap().id,
                *session
            );
            assert!(std::sync::Arc::ptr_eq(
                term,
                &app.pool.get(*session).unwrap().term
            ));
            assert_eq!(&terminal_text(&app, *session), text);
            assert!(!app.windows[&wid].pending_close);
        } else {
            assert_eq!(app.clipboard_session_in(wid), None);
            assert!(app.active_handle.lock().unwrap().is_none());
            if !app.windows[&wid].tab_set.is_empty() || !app.windows[&wid].pending_close {
                failures.push(
                    "the final pending document close did not arm the empty window for closure",
                );
            }
        }
        assert_eq!(
            app.closed_recovery.views.len(),
            1,
            "finishing the whole tab adds no obsolete pane placement"
        );
        let closed = app.windows[&wid].tab_set.get(original_tab).is_none();
        if !closed {
            failures.push("the pending close left the saved document as a sole leaf");
        }
        if app.view_store.get(editor_view).is_some()
            || app.native_runtime.view_state(editor_view).is_some()
        {
            failures.push("the pending close left the old document view attached");
        }
        let recovery_count = app.closed_recovery.tabs.len();
        if recovery_count != 1 {
            failures.push(
                "the collapsed pending close did not retain exactly one whole-tab recovery record",
            );
        }
        assert!(app.structural_invariants_ok());
        if let Some((session, foreground_tab, foreground_layout, _, foreground_before)) = foreground
            && closed
            && recovery_count == 1
        {
            app.reopen_last_closed_tab().unwrap();
            let (reopened_instance, reopened_view) = app.active_native_view(wid).unwrap();
            assert_ne!(reopened_instance, editor_instance);
            assert_ne!(reopened_view, editor_view);
            assert_eq!(
                app.native_runtime.document_id(reopened_instance),
                Some(document)
            );
            let reopened = app.windows[&wid].tab_set.active().unwrap();
            assert_ne!(reopened.id, original_tab);
            assert_eq!(reopened.root.leaves(), vec![reopened_view]);
            assert_eq!(app.windows[&wid].tab_set.len(), 2);
            let snapshot = app.document_store.snapshot(document).unwrap();
            assert_eq!((snapshot.seq, snapshot.text), (edited.seq, edited.text));
            assert_eq!(app.document_store.view_count(document), Some(1));
            assert_eq!(app.document_store.dirty(document), Some(false));
            assert_eq!(
                app.windows[&wid].tab_set.get(foreground_tab.id),
                Some(&foreground_tab)
            );
            assert_eq!(app.windows[&wid].layouts[0], foreground_layout);
            assert_eq!(terminal_text(&app, session), foreground_before);
            assert_eq!(fs::read_to_string(&path).unwrap(), "held before\n");
            assert!(app.closed_recovery.tabs.is_empty());
            assert_eq!(app.closed_recovery.views.len(), 1);
            assert!(
                app.reopen_last_closed_tab().is_err(),
                "the saved tab close is recorded once"
            );
            assert!(app.structural_invariants_ok());
        }
    }
    let _ = fs::remove_dir_all(dir);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn nonfinal_document_close_never_forces_a_checkpoint() {
    let dir = fresh_dir("aterm-app-share-close");
    let path = dir.join("shared-close.md");
    fs::write(&path, "before\n").unwrap();
    let uri = file_uri(&path);
    let mut app = App::headless_for_test();
    app.open_document_tab(AppKind::Markdown, &uri).unwrap();
    let wid = WindowId(0);
    app.open_document_tab_in_window_with_config_symlinks(
        wid,
        AppKind::Editor,
        &uri,
        false,
        Placement::NewTab,
    )
    .unwrap();
    let (instance, _) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();
    commit(&mut app, wid, "dirty ");

    app.close_active_native_tab(wid).unwrap();
    assert_eq!(app.document_store.view_count(document), Some(1));
    assert_eq!(app.document_store.dirty(document), Some(true));
    assert_eq!(fs::read_to_string(&path).unwrap(), "before\n");

    let before_tabs = app.windows[&wid].tab_set.len();
    app.close_active_native_tab(wid).unwrap();
    assert_eq!(app.windows[&wid].tab_set.len(), before_tabs - 1);
    assert_eq!(app.document_store.view_count(document), Some(0));
    assert_eq!(fs::read_to_string(&path).unwrap(), "dirty before\n");
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn a_closed_document_leaves_memory_once_no_reopen_can_bring_it_back() {
    let dir = fresh_dir("aterm-app-release");
    let path = dir.join("released.md");
    fs::write(&path, "before\n").unwrap();
    let uri = file_uri(&path);
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.open_document_tab(AppKind::Editor, &uri).unwrap();
    let (instance, _) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();
    let canonical = app
        .document_store
        .canonical_uri(document)
        .unwrap()
        .to_string();
    commit(&mut app, wid, "edited ");
    assert!(app.editor_workspace.can_undo(document));
    let journal = app
        .native_documents
        .journals
        .as_ref()
        .unwrap()
        .path_for_uri(&canonical);
    assert!(journal.exists());
    assert_eq!(
        app.release_unheld_documents(0),
        0,
        "a viewed document stays"
    );

    app.close_active_native_tab(wid).unwrap();
    assert_eq!(app.document_store.view_count(document), Some(0));
    assert_eq!(fs::read_to_string(&path).unwrap(), "edited before\n");
    assert!(
        app.native_documents.release_due,
        "a closed view arms the look"
    );
    assert_eq!(
        app.release_unheld_documents(0),
        0,
        "the closed-tab record can still reopen it"
    );
    assert!(!app.native_documents.release_due);
    app.reopen_last_closed_tab().unwrap();
    let (instance, _) = app.active_native_view(wid).unwrap();
    assert_eq!(app.native_runtime.document_id(instance), Some(document));
    assert!(app.editor_workspace.can_undo(document), "undo survived");

    app.close_active_native_tab(wid).unwrap();
    assert_eq!(
        app.release_unheld_documents(u64::MAX),
        1,
        "past the record's age nothing can bring it back"
    );
    assert_eq!(app.document_store.id_for_uri(&canonical), None);
    assert_eq!(app.native_documents.persistence.grant(document), None);
    assert_eq!(app.native_documents.grants.id_for_uri(&canonical), None);
    assert_eq!(
        app.native_documents
            .journals
            .as_ref()
            .unwrap()
            .durable_seq(document),
        None
    );
    assert!(!app.editor_workspace.can_undo(document));
    assert!(!app.editor_workspace.buffers.contains(&document));
    app.native_documents
        .journals
        .as_ref()
        .unwrap()
        .await_worker_for_test();
    assert!(
        !journal.exists(),
        "the worker retired its clean journal, which no other process holds"
    );

    fs::write(&path, "changed on disk\n").unwrap();
    app.open_document_tab(AppKind::Editor, &uri).unwrap();
    let (instance, _) = app.active_native_view(wid).unwrap();
    let reopened = app.native_runtime.document_id(instance).unwrap();
    assert_ne!(reopened, document);
    assert_eq!(
        app.document_store.snapshot(reopened).unwrap().text.as_ref(),
        "changed on disk\n"
    );
    assert!(app.structural_invariants_ok());
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn a_closed_document_leaves_at_the_first_park_past_its_records_expiry() {
    let dir = fresh_dir("aterm-app-release-expiry");
    let path = dir.join("expiring.md");
    fs::write(&path, "before\n").unwrap();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.open_document_tab(AppKind::Editor, &file_uri(&path))
        .unwrap();
    let (instance, _) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();
    app.close_active_native_tab(wid).unwrap();
    let closed_ms = app.lat_epoch.elapsed().as_millis() as u64;
    assert_eq!(
        app.release_documents_due(closed_ms),
        0,
        "its record is live"
    );
    assert!(!app.native_documents.release_due);
    let deadline = app
        .native_documents
        .release_deadline_ms
        .expect("the record's expiry is the next look");
    assert!(deadline > closed_ms);
    assert_eq!(app.release_documents_due(deadline - 1), 0);
    assert!(app.document_store.phase(document).is_some(), "not yet");
    assert_eq!(app.release_documents_due(deadline), 1);
    assert_eq!(app.document_store.phase(document), None);
    assert_eq!(app.native_documents.release_deadline_ms, None);
    let _ = fs::remove_dir_all(dir);
}

/// A Suspended document whose journal still has work in flight stays, and
/// the landing of that work arms the next look, which lets it go.
#[cfg(unix)]
#[test]
fn a_closed_document_with_a_journal_write_in_flight_waits_for_its_landing() {
    let dir = fresh_dir("aterm-app-release-inflight");
    let path = dir.join("inflight.md");
    fs::write(&path, "before\n").unwrap();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.open_document_tab(AppKind::Editor, &file_uri(&path))
        .unwrap();
    let (instance, _) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();
    app.close_active_native_tab(wid).unwrap();
    app.hold_journal_reseats_for_test();
    app.reseat_document_journals_after_rollback();
    assert_eq!(app.held_journal_reseats_for_test().len(), 1);

    assert_eq!(app.release_unheld_documents(u64::MAX), 0);
    assert!(app.document_store.phase(document).is_some(), "it stays");
    assert!(app.run_held_journal_reseat_for_test());
    assert!(
        app.native_documents.release_due,
        "the landing arms the look"
    );
    assert_eq!(app.release_unheld_documents(u64::MAX), 1);
    assert_eq!(app.document_store.phase(document), None);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn window_close_waits_for_every_document_before_atomic_detach() {
    let dir = fresh_dir("aterm-app-window-batch");
    let first_path = dir.join("first.md");
    let second_path = dir.join("second.md");
    fs::write(&first_path, "first\n").unwrap();
    fs::write(&second_path, "second\n").unwrap();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);

    app.open_document_tab(AppKind::Editor, &file_uri(&first_path))
        .unwrap();
    let (first_instance, first_view) = app.active_native_view(wid).unwrap();
    let first = app.native_runtime.document_id(first_instance).unwrap();
    commit(&mut app, wid, "safe ");

    app.open_document_tab(AppKind::Editor, &file_uri(&second_path))
        .unwrap();
    let (second_instance, second_view) = app.active_native_view(wid).unwrap();
    let second = app.native_runtime.document_id(second_instance).unwrap();
    commit(&mut app, wid, "also ");
    let tabs_before = app.windows[&wid].tab_set.len();

    assert!(
        !app.prepare_window_document_shutdown_inner(wid, false)
            .unwrap()
    );
    assert_eq!(app.document_store.view_count(first), Some(1));
    assert_eq!(app.document_store.view_count(second), Some(1));

    app.save_document_checkpoint(first, first_view).unwrap();
    assert_eq!(
        app.take_ready_document_shutdowns().unwrap(),
        (false, vec![])
    );
    assert!(app.windows.contains_key(&wid));
    assert_eq!(app.windows[&wid].tab_set.len(), tabs_before);
    assert_eq!(app.document_store.view_count(first), Some(1));
    assert_eq!(app.document_store.view_count(second), Some(1));

    app.save_document_checkpoint(second, second_view).unwrap();
    assert_eq!(
        app.take_ready_document_shutdowns().unwrap(),
        (false, vec![wid])
    );
    assert!(app.windows.contains_key(&wid));
    assert_eq!(app.document_store.view_count(first), Some(0));
    assert_eq!(app.document_store.view_count(second), Some(0));
    assert_eq!(fs::read_to_string(&first_path).unwrap(), "safe first\n");
    assert_eq!(fs::read_to_string(&second_path).unwrap(), "also second\n");
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn whole_app_quit_waits_for_all_durable_documents_without_partial_teardown() {
    let dir = fresh_dir("aterm-app-quit-batch");
    let first_path = dir.join("quit-first.md");
    let second_path = dir.join("quit-second.md");
    fs::write(&first_path, "first\n").unwrap();
    fs::write(&second_path, "second\n").unwrap();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);

    app.open_document_tab(AppKind::Editor, &file_uri(&first_path))
        .unwrap();
    let (first_instance, first_view) = app.active_native_view(wid).unwrap();
    let first = app.native_runtime.document_id(first_instance).unwrap();
    commit(&mut app, wid, "safe ");
    app.open_document_tab(AppKind::Editor, &file_uri(&second_path))
        .unwrap();
    let (second_instance, second_view) = app.active_native_view(wid).unwrap();
    let second = app.native_runtime.document_id(second_instance).unwrap();
    commit(&mut app, wid, "also ");

    assert!(!app.prepare_quit_document_shutdown_inner(false).unwrap());
    app.save_document_checkpoint(first, first_view).unwrap();
    assert_eq!(
        app.take_ready_document_shutdowns().unwrap(),
        (false, vec![])
    );
    assert!(app.windows.contains_key(&wid));
    assert_eq!(app.document_store.view_count(first), Some(1));
    assert_eq!(app.document_store.view_count(second), Some(1));

    app.save_document_checkpoint(second, second_view).unwrap();
    assert_eq!(app.take_ready_document_shutdowns().unwrap(), (true, vec![]));
    assert!(app.windows.contains_key(&wid));
    assert_eq!(app.document_store.dirty(first), Some(false));
    assert_eq!(app.document_store.dirty(second), Some(false));
    assert_eq!(app.document_store.view_count(first), Some(1));
    assert_eq!(app.document_store.view_count(second), Some(1));
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn failed_window_save_keeps_the_complete_plan_and_surfaces_status() {
    let dir = fresh_dir("aterm-app-window-save-failure");
    let path = dir.join("conflict.md");
    fs::write(&path, "before\n").unwrap();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.open_document_tab(AppKind::Editor, &file_uri(&path))
        .unwrap();
    let (instance, view) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();
    commit(&mut app, wid, "ours ");
    assert!(
        !app.prepare_window_document_shutdown_inner(wid, false)
            .unwrap()
    );

    fs::write(&path, "theirs\n").unwrap();
    app.save_document_checkpoint(document, view).unwrap_err();
    assert_eq!(
        app.take_ready_document_shutdowns().unwrap(),
        (false, vec![])
    );
    assert!(app.windows.contains_key(&wid));
    assert_eq!(app.document_store.view_count(document), Some(1));
    assert!(matches!(
        app.document_store.phase(document),
        Some(crate::document_store::DocumentPhase::Blocked { .. })
    ));
    let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view) else {
        panic!("editor view must remain installed");
    };
    assert!(state.status.is_some());
    app.dispatch_native_event(
        wid,
        AppEvent::Action(crate::native_app::ActionInvocation {
            id: crate::native_ui::ActionId::new("editor/revert"),
            value: None,
        }),
    )
    .unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "theirs\n");
    assert!(
        app.native_documents.close_due,
        "the discard escalates the close at the next park, not at some later save"
    );
    assert_eq!(
        app.take_ready_document_shutdowns().unwrap(),
        (false, vec![wid]),
        "discard/reload is a complete recovery for a conflict-blocked window close"
    );
    let _ = fs::remove_dir_all(dir);
}

/// Window 0 holds its terminal (session 0) and an Editor whose edits a disk
/// change kept from saving, and the close of window 0 that the failed save
/// left armed (`close_window` arms it through
/// `prepare_window_document_shutdown`). `setup` runs once the Editor holds
/// its edits, before the save; it is handed the document's path.
fn failed_window_close<R>(
    setup: impl FnOnce(&mut App, &std::path::Path) -> R,
) -> (
    aterm_tempfile::TempDir,
    App,
    DocumentId,
    crate::tab_model::ViewId,
    R,
) {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("conflict.md");
    fs::write(&path, "before\n").unwrap();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.open_document_tab_in_window(wid, AppKind::Editor, &file_uri(&path))
        .unwrap();
    let (instance, view) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();
    commit(&mut app, wid, "ours ");
    let set = setup(&mut app, &path);
    fs::write(&path, "theirs\n").unwrap();
    assert!(
        !app.prepare_window_document_shutdown(wid).unwrap(),
        "the save meets the disk change and fails"
    );
    assert!(matches!(
        app.document_store.phase(document),
        Some(crate::document_store::DocumentPhase::Blocked { .. })
    ));
    assert!(
        app.native_documents
            .pending_window_closes
            .contains_key(&wid)
    );
    (dir, app, document, view, set)
}

/// The person discards the blocked edits, which readies the armed close of
/// window 0; after `changed` it must not finish over that window.
fn assert_discard_drops_the_close(
    app: &mut App,
    document: DocumentId,
    view: crate::tab_model::ViewId,
    changed: &str,
) {
    let wid = WindowId(0);
    app.discard_document_changes(document, view).unwrap();
    assert!(app.native_documents.close_due);
    assert_eq!(
        app.take_ready_document_shutdowns().unwrap(),
        (false, vec![]),
        "the close resumed over a window that {changed} since it was asked"
    );
    assert!(
        !app.native_documents
            .pending_window_closes
            .contains_key(&wid),
        "the stale close is dropped, so the person closes the window again"
    );
    assert!(app.windows.contains_key(&wid));
    assert_eq!(app.document_store.view_count(document), Some(1));
    assert_eq!(
        app.document_store.phase(document),
        Some(crate::document_store::DocumentPhase::Active)
    );
}

/// Session 0's shell starts `command` (OSC 133 marks: prompt, command line, run).
fn run_in_shell(app: &App, command: &str) {
    crate::feed_session_for_test(
        app,
        0,
        format!("\x1b]133;A\x07$ \x1b]133;B\x07{command}\n\x1b]133;C\x07").as_bytes(),
    );
    assert!(
        app.session_foreground_busy(0),
        "{command} runs in its shell"
    );
}

#[test]
fn a_failed_window_close_does_not_resume_over_a_job_started_since() {
    let (_dir, mut app, document, view, ()) = failed_window_close(|_, _| ());
    run_in_shell(&app, "make");
    assert_discard_drops_the_close(&mut app, document, view, "started a job");
    assert!(app.pool.get(0).is_some(), "the build's terminal lives");
}

#[test]
fn a_failed_window_close_does_not_resume_over_a_job_that_replaced_the_one_asked_over() {
    let (_dir, mut app, document, view, ()) =
        failed_window_close(|app, _| run_in_shell(app, "tail -f build.log"));
    crate::feed_session_for_test(&app, 0, b"\x1b]133;D;0\x07");
    run_in_shell(&app, "vim notes");
    assert_discard_drops_the_close(
        &mut app,
        document,
        view,
        "ended the job it was asked over and started another",
    );
}

/// Control for the two above: the job the close was asked over, still
/// running, does not hold the close.
#[test]
fn a_failed_window_close_resumes_over_the_job_it_was_asked_over() {
    let wid = WindowId(0);
    let (_dir, mut app, document, view, ()) =
        failed_window_close(|app, _| run_in_shell(app, "tail -f build.log"));
    app.discard_document_changes(document, view).unwrap();
    assert_eq!(
        app.take_ready_document_shutdowns().unwrap(),
        (false, vec![wid])
    );
}

/// A shell with no command marks counts no finished commands, so the job
/// is told apart by its process group: the close asked over a `cat` does
/// not finish over the second `cat` the same command line ran once it ended.
#[cfg(unix)]
#[test]
fn a_failed_window_close_does_not_resume_over_an_unmarked_job_that_replaced_the_one_asked_over() {
    use crate::app_restore::crash_journal_program_tests::{
        kill_all, spawn_leader, type_line, wait_for,
    };
    struct Shell(i32, i32);
    impl Drop for Shell {
        fn drop(&mut self) {
            kill_all(&[self.1], &[self.0]);
        }
    }
    let home = aterm_tempfile::tempdir().unwrap();
    let (master, shell) = spawn_leader(&["/bin/sh", "-i"], home.path());
    let _shell = Shell(master, shell);
    let next_job = move |previous: i32| {
        let mut group = -1;
        wait_for("the next foreground job", || {
            group = crate::quit_safety::foreground_pgrp(master);
            group > 0 && group != shell && group != previous
        });
        group
    };
    let (_dir, mut app, document, view, asked_over) = failed_window_close(|app, _| {
        let mut session = crate::stub_session(app.next_session_id);
        session.master = master;
        session.pid = shell;
        app.push_stub_tab(WindowId(0), session);
        type_line(master, "/bin/cat; /bin/cat\n");
        next_job(-1)
    });
    type_line(master, "\x04");
    let replacement = next_job(asked_over);
    assert_discard_drops_the_close(
        &mut app,
        document,
        view,
        "ended the job it was asked over and started another",
    );
    kill_all(&[replacement], &[]);
}

#[test]
fn a_failed_window_close_does_not_resume_over_a_tab_opened_since() {
    let wid = WindowId(0);
    let (_dir, mut app, document, view, ()) = failed_window_close(|_, _| ());
    let opened = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(opened));
    assert_discard_drops_the_close(&mut app, document, view, "gained a terminal tab");
    assert!(app.window_contains_session(wid, opened));
}

/// A dropped window close takes with it the split-leaf close it deferred;
/// left behind, that close would detach the leaf at some later save.
#[test]
fn a_dropped_window_close_takes_the_leaf_close_it_deferred() {
    let wid = WindowId(0);
    let (_dir, mut app, document, view, ()) = failed_window_close(|app, path| {
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        let _ = app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Vertical);
        let tab = tab_holding(app, wid, view);
        fs::write(path, "theirs\n").unwrap();
        app.prepare_document_view_close(wid, tab, document, view)
            .unwrap_err();
        assert!(
            app.native_documents.pending_closes.contains_key(&document),
            "the leaf close waits on the failed save"
        );
    });
    let opened = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(opened));
    assert_discard_drops_the_close(&mut app, document, view, "gained a terminal tab");
    assert!(app.native_documents.pending_closes.is_empty());
    app.save_document_checkpoint(document, view).unwrap();
    assert!(
        app.windows[&wid]
            .tab_set
            .tabs()
            .iter()
            .any(|tab| tab.root.contains(view)),
        "a later save detached the leaf of a dropped close"
    );
}

/// A clean document whose last checkpoint failed has no text an update could lose, so it holds
/// no update, even where the successor reopens no document tab.
#[test]
fn a_clean_document_whose_save_failed_holds_no_update() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("clean.md");
    fs::write(&path, "on disk\n").unwrap();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.open_document_tab_in_window(wid, AppKind::Editor, &file_uri(&path))
        .unwrap();
    let (instance, _) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();
    app.document_store.checkpoint_fail(document).unwrap();
    assert_eq!(app.document_store.dirty(document), Some(false));
    assert!(app.document_store.failed_checkpoint_seq(document).is_some());
    assert!(!app.successor_restores_document_tabs());
    app.revalidate_native_update_safety()
        .expect("a clean document's failed save holds nothing");
}

/// A view close whose save cannot start leaves no save running: the document stands Blocked
/// with its reason on the tab, not Closing on a checkpoint nothing writes, which held every
/// update after it as aterm's own work.
#[test]
fn a_close_whose_save_cannot_start_is_blocked_not_left_closing() {
    let wid = WindowId(0);
    let (_dir, mut app, document, view, ()) = failed_window_close(|_, _| ());
    let tab = tab_holding(&app, wid, view);
    let message = app
        .prepare_document_view_close(wid, tab, document, view)
        .unwrap_err();
    assert!(
        matches!(
            app.document_store.phase(document),
            Some(crate::document_store::DocumentPhase::Blocked { .. })
        ),
        "{message}: {:?}",
        app.document_store.phase(document)
    );
    assert!(app.document_store.failed_checkpoint_seq(document).is_some());
    let Err(reasons) = app.revalidate_native_update_safety() else {
        panic!("the unsaved text still holds the update");
    };
    assert!(
        !reasons
            .iter()
            .any(|reason| reason.contains("still running")),
        "no checkpoint is running: {reasons:?}"
    );
    assert!(
        App::update_blocker_for_person(&reasons).is_some(),
        "the person's unsaved text is what holds: {reasons:?}"
    );
}

/// A document the armed close never planned (its tab came in after the
/// close was asked), whose leaf close the armed close deferred, is editable
/// again once that close is dropped.
#[test]
fn a_dropped_window_close_reopens_a_document_whose_leaf_close_it_deferred() {
    let wid = WindowId(0);
    let (dir, mut app, document, view, ()) = failed_window_close(|_, _| ());
    let later_path = dir.path().join("later.md");
    fs::write(&later_path, "later\n").unwrap();
    app.open_document_tab_in_window(wid, AppKind::Editor, &file_uri(&later_path))
        .unwrap();
    let (instance, later_view) = app.active_native_view(wid).unwrap();
    let later = app.native_runtime.document_id(instance).unwrap();
    commit(&mut app, wid, "edit ");
    let _ = app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Vertical);
    let tab = tab_holding(&app, wid, later_view);
    assert!(
        !app.prepare_document_view_close(wid, tab, later, later_view)
            .unwrap(),
        "the leaf's save lands, and its close waits on the window close"
    );
    assert_eq!(app.document_store.dirty(later), Some(false));
    assert!(app.native_documents.pending_closes.contains_key(&later));
    assert_discard_drops_the_close(
        &mut app,
        document,
        view,
        "gained a terminal beside the later document",
    );
    assert_eq!(
        app.document_store.phase(later),
        Some(crate::document_store::DocumentPhase::Active),
        "the later document stayed frozen after its deferred leaf close was dropped"
    );
    assert!(
        app.windows[&wid]
            .tab_set
            .tabs()
            .iter()
            .any(|tab| tab.root.contains(later_view))
    );
}

fn tab_holding(
    app: &App,
    wid: WindowId,
    view: crate::tab_model::ViewId,
) -> crate::tab_model::TabId {
    app.windows[&wid]
        .tab_set
        .tabs()
        .iter()
        .find(|tab| tab.root.contains(view))
        .unwrap()
        .id
}

#[test]
fn a_failed_window_close_does_not_resume_once_its_window_is_the_last() {
    let (_dir, mut app, document, view, other) = failed_window_close(|app, _| {
        let session = app.next_session_id;
        let other = app.insert_logical_window(crate::stub_session(session), 24, 80);
        app.frontmost_window = Some(WindowId(0));
        other
    });
    assert_eq!(app.close_window_logical(other), crate::CloseOutcome::Stay);
    assert_discard_drops_the_close(
        &mut app,
        document,
        view,
        "became the last window (closing it quits aterm)",
    );
}

/// Window 0 and a second window each hold an Editor whose edits a disk
/// change kept from saving, so each window's close is armed and waiting,
/// each asked while the other window was open. Answers the second window
/// and each window's document and view, in window order.
fn two_failed_window_closes() -> (
    aterm_tempfile::TempDir,
    App,
    WindowId,
    Vec<(DocumentId, crate::tab_model::ViewId)>,
) {
    let dir = aterm_tempfile::tempdir().unwrap();
    let wid = WindowId(0);
    let mut app = App::headless_for_test();
    let session = app.next_session_id;
    let other = app.insert_logical_window(crate::stub_session(session), 24, 80);
    let mut armed = Vec::new();
    for (window, name) in [(wid, "first.md"), (other, "second.md")] {
        let path = dir.path().join(name);
        fs::write(&path, "before\n").unwrap();
        app.open_document_tab_in_window(window, AppKind::Editor, &file_uri(&path))
            .unwrap();
        let (instance, view) = app.active_native_view(window).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        commit(&mut app, window, "ours ");
        fs::write(&path, "theirs\n").unwrap();
        armed.push((document, view));
    }
    for window in [wid, other] {
        assert!(!app.prepare_window_document_shutdown(window).unwrap());
    }
    (dir, app, other, armed)
}

/// Two closes ready in one pass: the second is weighed with the first
/// already gone, so together they do not quit aterm unasked.
#[test]
fn two_window_closes_ready_together_do_not_quit_without_asking() {
    let wid = WindowId(0);
    let (_dir, mut app, other, armed) = two_failed_window_closes();
    for (document, view) in &armed {
        app.discard_document_changes(*document, *view).unwrap();
    }
    assert_eq!(
        app.take_ready_document_shutdowns().unwrap(),
        (false, vec![wid]),
        "both closes went, which quits aterm with no question"
    );
    assert!(
        !app.native_documents
            .pending_window_closes
            .contains_key(&other)
    );
    assert_eq!(
        app.document_store.phase(armed[1].0),
        Some(crate::document_store::DocumentPhase::Active)
    );
}

/// Window 0 holds its terminal and an Editor, a second window holds a
/// terminal, and the Editor's edits a disk change kept from saving armed a
/// close of window 0 that is still waiting.
fn failed_close_beside_another_window(
    editor_only: bool,
) -> (
    aterm_tempfile::TempDir,
    App,
    WindowId,
    DocumentId,
    crate::tab_model::ViewId,
) {
    let (dir, app, document, view, other) = failed_window_close(|app, _| {
        let session = app.next_session_id;
        let other = app.insert_logical_window(crate::stub_session(session), 24, 80);
        if editor_only {
            // Window 0's terminal moves first, so the Editor is all it holds.
            app.switch_tab_in(WindowId(0), 0);
            app.migrate_active_tab_from_window(WindowId(0)).unwrap();
            assert_eq!(app.windows[&WindowId(0)].tab_set.len(), 1);
        }
        app.frontmost_window = Some(WindowId(0));
        other
    });
    (dir, app, other, document, view)
}

/// The person keeps editing the moved document in `other` and another save
/// escalates: nothing may freeze it, and the escalation must not fail.
fn assert_moved_document_stays_editable(
    app: &mut App,
    other: WindowId,
    document: DocumentId,
    view: crate::tab_model::ViewId,
) {
    app.discard_document_changes(document, view).unwrap();
    commit(app, other, "kept ");
    assert_eq!(app.document_store.dirty(document), Some(true));
    assert_eq!(
        app.take_ready_document_shutdowns().unwrap(),
        (false, vec![]),
        "a close armed for window 0 failed or finished an unrelated escalation"
    );
    assert_eq!(
        app.document_store.phase(document),
        Some(crate::document_store::DocumentPhase::Active),
        "a close armed for window 0 froze a document now open in another window"
    );
    commit(app, other, "still ");
    let text = app.document_store.snapshot(document).unwrap().text;
    assert!(
        text.contains("kept ") && text.contains("still "),
        "every edit applies: {text:?}"
    );
    assert!(
        app.native_documents.pending_window_closes.is_empty(),
        "no close is left to hold every document release"
    );
}

#[test]
fn a_window_emptied_by_a_tab_move_leaves_no_close_to_freeze_its_document() {
    let wid = WindowId(0);
    let (_dir, mut app, other, document, view) = failed_close_beside_another_window(true);
    app.migrate_active_tab_from_window(wid).unwrap();
    assert!(!app.windows.contains_key(&wid), "the emptied window closed");
    assert!(
        !app.native_documents
            .pending_window_closes
            .contains_key(&wid),
        "the closed window's armed close went with it"
    );
    assert_eq!(
        app.document_store.phase(document),
        Some(crate::document_store::DocumentPhase::Active),
        "the document is editable in the window it moved to"
    );
    assert_moved_document_stays_editable(&mut app, other, document, view);
}

#[test]
fn a_document_moved_out_of_a_window_whose_close_failed_stays_editable() {
    let wid = WindowId(0);
    let (_dir, mut app, other, document, view) = failed_close_beside_another_window(false);
    app.migrate_active_tab_from_window(wid).unwrap();
    assert!(
        app.windows.contains_key(&wid),
        "its terminal keeps window 0"
    );
    assert_moved_document_stays_editable(&mut app, other, document, view);
    assert!(
        app.windows.contains_key(&wid),
        "window 0 does not close once the close's document left it"
    );
}

/// A repeated close of a window whose document left it (after a save that
/// failed to write) releases that document; it saves and edits where it went.
#[test]
fn a_repeated_window_close_reopens_a_document_that_left_the_window() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("write-error.md");
    fs::write(&path, "before\n").unwrap();
    let wid = WindowId(0);
    let mut app = App::headless_for_test();
    let session = app.next_session_id;
    let other = app.insert_logical_window(crate::stub_session(session), 24, 80);
    app.open_document_tab_in_window(wid, AppKind::Editor, &file_uri(&path))
        .unwrap();
    let (instance, view) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();
    commit(&mut app, wid, "ours ");
    assert!(
        !app.prepare_window_document_shutdown_inner(wid, false)
            .unwrap()
    );
    let pending = app.hold_document_save_for_test(document);
    app.finish_native_document_save(
        document,
        view,
        pending.plan.generation,
        pending.plan.bytes,
        crate::native_document_io::AtomicSaveResult::Failed {
            stage: crate::native_document_io::AtomicSaveStage::WriteTemporary,
            message: "No space left on device".to_string(),
            cause: None,
        },
        None,
    )
    .unwrap_err();
    assert!(matches!(
        app.document_store.phase(document),
        Some(crate::document_store::DocumentPhase::Blocked { .. })
    ));

    app.migrate_active_tab_from_window(wid).unwrap();
    assert!(
        app.prepare_window_document_shutdown(wid).unwrap(),
        "window 0 holds no document now"
    );
    assert_eq!(
        app.document_store.phase(document),
        Some(crate::document_store::DocumentPhase::Active),
        "the replaced close left the moved document frozen"
    );
    app.save_document_checkpoint(document, view).unwrap();
    commit(&mut app, other, "more ");
    let text = app.document_store.snapshot(document).unwrap().text;
    assert!(text.contains("more "), "the edit applies: {text:?}");
}

/// In window 0, beside its armed close, `later.md` is edited, split with a
/// terminal and its leaf closed: the leaf's save lands and its close waits
/// on the window close. Answers the document, its leaf and its tab.
fn deferred_leaf_close(
    app: &mut App,
    dir: &std::path::Path,
) -> (
    DocumentId,
    crate::tab_model::ViewId,
    crate::tab_model::TabId,
) {
    let wid = WindowId(0);
    let later_path = dir.join("later.md");
    fs::write(&later_path, "later\n").unwrap();
    app.open_document_tab_in_window(wid, AppKind::Editor, &file_uri(&later_path))
        .unwrap();
    let (instance, later_view) = app.active_native_view(wid).unwrap();
    let later = app.native_runtime.document_id(instance).unwrap();
    commit(app, wid, "edit ");
    let _ = app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Vertical);
    let tab = tab_holding(app, wid, later_view);
    assert!(
        !app.prepare_document_view_close(wid, tab, later, later_view)
            .unwrap()
    );
    assert_eq!(app.document_store.dirty(later), Some(false));
    assert!(matches!(
        app.document_store.phase(later),
        Some(crate::document_store::DocumentPhase::Closing { .. })
    ));
    (later, later_view, tab)
}

/// `later`'s leaf closed in `window`, where its tab went: the tab keeps
/// its terminal, and nothing is left frozen or queued.
fn assert_leaf_closed_where_it_went(
    app: &App,
    window: WindowId,
    tab: crate::tab_model::TabId,
    later: DocumentId,
    later_view: crate::tab_model::ViewId,
) {
    let tab = app.windows[&window]
        .tab_set
        .get(tab)
        .expect("the tab stays in the window it moved to");
    assert!(
        !tab.root.contains(later_view),
        "the leaf the person closed is still there"
    );
    assert!(
        !matches!(
            app.document_store.phase(later),
            Some(
                crate::document_store::DocumentPhase::Closing { .. }
                    | crate::document_store::DocumentPhase::Blocked { .. }
            )
        ),
        "the closed leaf's document is left frozen"
    );
    assert!(app.native_documents.pending_closes.is_empty());
}

/// A leaf close the armed window close deferred, whose tab then moved to
/// another window, belongs to that window now: the window close commits
/// without it, and the leaf then closes where its tab went.
#[test]
fn a_committed_window_close_leaves_the_leaf_close_of_a_tab_that_moved_out_to_finish() {
    let wid = WindowId(0);
    let (dir, mut app, document, view, other) = failed_window_close(|app, _| {
        let session = app.next_session_id;
        let other = app.insert_logical_window(crate::stub_session(session), 24, 80);
        app.frontmost_window = Some(WindowId(0));
        other
    });
    let (later, later_view, tab) = deferred_leaf_close(&mut app, dir.path());
    app.migrate_active_tab_from_window(wid).unwrap();
    assert!(app.windows[&other].tab_set.get(tab).is_some());
    app.discard_document_changes(document, view).unwrap();
    assert_eq!(
        app.take_ready_document_shutdowns().unwrap(),
        (false, vec![wid])
    );
    assert_eq!(app.close_window_logical(wid), crate::CloseOutcome::Stay);
    app.release_documents_at_park();
    assert_leaf_closed_where_it_went(&app, other, tab, later, later_view);
}

/// The same, when the person closes window 0 again after moving every
/// document out of it: the new close commits at once, holding nothing.
#[test]
fn a_repeated_window_close_leaves_the_leaf_close_of_a_tab_that_moved_out_to_finish() {
    let wid = WindowId(0);
    let (dir, mut app, document, view, other) = failed_window_close(|app, _| {
        let session = app.next_session_id;
        let other = app.insert_logical_window(crate::stub_session(session), 24, 80);
        app.frontmost_window = Some(WindowId(0));
        other
    });
    let (later, later_view, tab) = deferred_leaf_close(&mut app, dir.path());
    app.migrate_active_tab_from_window(wid).unwrap();
    let notes = app.windows[&wid]
        .tab_set
        .tabs()
        .iter()
        .position(|tab| tab.root.contains(view))
        .unwrap();
    app.switch_tab_in(wid, notes);
    app.migrate_active_tab_from_window(wid).unwrap();
    assert!(
        app.prepare_window_document_shutdown(wid).unwrap(),
        "window 0 holds no document now"
    );
    assert_eq!(
        app.document_store.phase(document),
        Some(crate::document_store::DocumentPhase::Active),
        "the replaced close reopens the document it froze"
    );
    app.release_documents_at_park();
    assert_leaf_closed_where_it_went(&app, other, tab, later, later_view);
}

/// A leaf close whose save failed, and whose tab then moved to another
/// window, finishes there once a later save lands.
#[test]
fn a_leaf_close_whose_tab_moved_finishes_where_it_went_once_its_save_lands() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("later.md");
    fs::write(&path, "later\n").unwrap();
    let wid = WindowId(0);
    let mut app = App::headless_for_test();
    let other = app.insert_logical_window(crate::stub_session(app.next_session_id), 24, 80);
    app.frontmost_window = Some(wid);
    app.open_document_tab_in_window(wid, AppKind::Editor, &file_uri(&path))
        .unwrap();
    let (instance, later_view) = app.active_native_view(wid).unwrap();
    let later = app.native_runtime.document_id(instance).unwrap();
    commit(&mut app, wid, "edit ");
    let _ = app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Vertical);
    let tab = tab_holding(&app, wid, later_view);
    let held = app.hold_document_save_for_test(later);
    assert!(
        !app.prepare_document_view_close(wid, tab, later, later_view)
            .unwrap()
    );
    app.finish_native_document_save(
        later,
        later_view,
        held.plan.generation,
        held.plan.bytes,
        crate::native_document_io::AtomicSaveResult::Failed {
            stage: crate::native_document_io::AtomicSaveStage::WriteTemporary,
            message: "No space left on device".to_string(),
            cause: None,
        },
        None,
    )
    .unwrap_err();
    assert!(matches!(
        app.document_store.phase(later),
        Some(crate::document_store::DocumentPhase::Blocked { .. })
    ));
    assert!(app.native_documents.pending_closes.contains_key(&later));

    app.migrate_active_tab_from_window(wid).unwrap();
    app.save_document_checkpoint(later, later_view).unwrap();
    assert_leaf_closed_where_it_went(&app, other, tab, later, later_view);
}

/// A document open in two windows can leave one of them unsaved, so the
/// close of the first window armed without saving it. Once the second
/// window closed, the first window's close must save it before it goes:
/// its ready edge freezes no document it does not then save.
#[test]
fn a_window_close_saves_a_document_whose_other_window_closed_since() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let wid = WindowId(0);
    let shared_path = dir.path().join("shared.md");
    let conflict_path = dir.path().join("conflict.md");
    fs::write(&shared_path, "shared\n").unwrap();
    fs::write(&conflict_path, "before\n").unwrap();
    let mut app = App::headless_for_test();
    let twin = app.insert_logical_window(crate::stub_session(app.next_session_id), 24, 80);
    let _third = app.insert_logical_window(crate::stub_session(app.next_session_id), 24, 80);
    app.open_document_tab_in_window(wid, AppKind::Editor, &file_uri(&shared_path))
        .unwrap();
    let (instance, _) = app.active_native_view(wid).unwrap();
    let shared = app.native_runtime.document_id(instance).unwrap();
    commit(&mut app, wid, "unsaved ");
    app.open_document_tab_in_window(twin, AppKind::Editor, &file_uri(&shared_path))
        .unwrap();
    let (twin_instance, _) = app.active_native_view(twin).unwrap();
    assert_eq!(app.native_runtime.document_id(twin_instance), Some(shared));
    app.open_document_tab_in_window(wid, AppKind::Editor, &file_uri(&conflict_path))
        .unwrap();
    let (conflict_instance, conflict_view) = app.active_native_view(wid).unwrap();
    let conflict = app.native_runtime.document_id(conflict_instance).unwrap();
    commit(&mut app, wid, "ours ");
    fs::write(&conflict_path, "theirs\n").unwrap();
    assert!(!app.prepare_window_document_shutdown(wid).unwrap());
    assert_eq!(
        app.document_store.phase(shared),
        Some(crate::document_store::DocumentPhase::Active),
        "its view in the twin window kept the shared document out of the save"
    );
    assert!(app.prepare_window_document_shutdown(twin).unwrap());
    assert_eq!(app.close_window_logical(twin), crate::CloseOutcome::Stay);

    app.discard_document_changes(conflict, conflict_view)
        .unwrap();
    assert_eq!(
        app.take_ready_document_shutdowns().unwrap(),
        (false, vec![]),
        "the shared document still owes its save"
    );
    assert_eq!(
        app.document_store.dirty(shared),
        Some(false),
        "the ready edge froze the shared document without saving it"
    );
    assert_eq!(
        app.take_ready_document_shutdowns().unwrap(),
        (false, vec![wid]),
        "the close finishes once the save it started lands"
    );
}

/// A close that comes ready while an update handoff is in flight is the
/// handoff's to replay, with every document leaf still attached for it.
#[cfg(unix)]
#[test]
fn a_window_close_ready_during_an_update_handoff_detaches_nothing_before_its_replay() {
    let wid = WindowId(0);
    let (_dir, mut app, document, view, ()) = failed_window_close(|_, _| ());
    let cancelled = crate::overlap_handoff_tests::arm_outgoing_handoff(&mut app);
    app.discard_document_changes(document, view).unwrap();
    assert_eq!(
        app.take_ready_document_shutdowns().unwrap(),
        (false, vec![])
    );
    assert_eq!(
        app.document_store.view_count(document),
        Some(1),
        "the documents were detached before the handoff deferred the close"
    );
    assert!(
        cancelled.try_recv().is_ok(),
        "the handoff was asked to stop"
    );
    assert!(matches!(
        app.pending_update_handoff.as_ref().map(|pending| &pending.teardown),
        Some(crate::DeferredHandoffTeardown::Mutations(mutations))
            if mutations.contains_key(&crate::DeferredHandoffMutation::CloseWindow(wid))
    ));
    // The handoff rolls back and replays `close_window`.
    app.pending_update_handoff = None;
    assert!(
        app.prepare_window_document_shutdown(wid).unwrap(),
        "the replayed close's document barrier commits"
    );
    assert_eq!(app.document_store.view_count(document), Some(0));
}

/// A close armed for a window that is gone by a path that did not drop it
/// neither freezes its document nor costs another window its close.
#[test]
fn a_close_armed_for_a_vanished_window_costs_no_other_window_its_close() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let first_path = dir.path().join("first.md");
    let second_path = dir.path().join("second.md");
    fs::write(&first_path, "first\n").unwrap();
    fs::write(&second_path, "second\n").unwrap();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let session = app.next_session_id;
    let other = app.insert_logical_window(crate::stub_session(session), 24, 80);
    app.open_document_tab_in_window(wid, AppKind::Editor, &file_uri(&first_path))
        .unwrap();
    let (first_instance, first_view) = app.active_native_view(wid).unwrap();
    let first = app.native_runtime.document_id(first_instance).unwrap();
    commit(&mut app, wid, "safe ");
    app.open_document_tab_in_window(other, AppKind::Editor, &file_uri(&second_path))
        .unwrap();
    let (second_instance, _) = app.active_native_view(other).unwrap();
    let second = app.native_runtime.document_id(second_instance).unwrap();
    commit(&mut app, other, "open ");

    assert!(
        !app.prepare_window_document_shutdown_inner(other, false)
            .unwrap()
    );
    let vanished = WindowId(app.next_window_id + 1);
    let stale = app
        .native_documents
        .pending_window_closes
        .remove(&other)
        .unwrap();
    app.native_documents
        .pending_window_closes
        .insert(vanished, stale);
    assert!(
        !app.prepare_window_document_shutdown_inner(wid, false)
            .unwrap()
    );
    app.save_document_checkpoint(first, first_view).unwrap();
    assert_eq!(
        app.take_ready_document_shutdowns().unwrap(),
        (false, vec![wid])
    );
    assert!(
        !app.native_documents
            .pending_window_closes
            .contains_key(&vanished)
    );
    assert_eq!(
        app.document_store.phase(second),
        Some(crate::document_store::DocumentPhase::Active)
    );
}

/// The window closes a pending update handoff holds to replay.
#[cfg(unix)]
fn closes_behind_the_handoff(app: &App) -> Vec<WindowId> {
    match app
        .pending_update_handoff
        .as_ref()
        .map(|pending| &pending.teardown)
    {
        Some(crate::DeferredHandoffTeardown::Mutations(mutations)) => mutations
            .keys()
            .filter_map(|mutation| match mutation {
                crate::DeferredHandoffMutation::CloseWindow(window) => Some(*window),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// The handoff rolls back: its record is taken, as the completion
/// reducer takes it, and the closes it deferred replay.
#[cfg(unix)]
fn replay_returned_handoff(app: &mut App) -> crate::CloseOutcome {
    let pending = app
        .pending_update_handoff
        .take()
        .expect("a pending handoff");
    match pending.teardown {
        crate::DeferredHandoffTeardown::Mutations(mutations) => {
            app.replay_deferred_handoff_mutations(mutations)
        }
        other => panic!("the handoff holds {other:?}, not the closes"),
    }
}

/// The second window's close was dropped, not handed to the handoff: the
/// window stays, and its document is editable.
#[cfg(unix)]
fn assert_close_dropped_beside_the_handoff(app: &App, window: WindowId, document: DocumentId) {
    assert!(
        !closes_behind_the_handoff(app).contains(&window),
        "the handoff will close window {} too, which quits aterm with no question",
        window.0
    );
    assert!(
        !app.native_documents
            .pending_window_closes
            .contains_key(&window)
    );
    assert_eq!(
        app.document_store.phase(document),
        Some(crate::document_store::DocumentPhase::Active)
    );
}

/// Two closes ready in one pass while an update handoff is in flight: the
/// first is the handoff's to replay and counts as gone, so the second,
/// which would then close the last window, is dropped, and the replay
/// closes only the first.
#[cfg(unix)]
#[test]
fn two_window_closes_ready_during_an_update_handoff_do_not_quit_without_asking() {
    let wid = WindowId(0);
    let (_dir, mut app, other, armed) = two_failed_window_closes();
    let _cancelled = crate::overlap_handoff_tests::arm_outgoing_handoff(&mut app);
    for (document, view) in &armed {
        app.discard_document_changes(*document, *view).unwrap();
    }
    assert_eq!(
        app.take_ready_document_shutdowns().unwrap(),
        (false, vec![])
    );
    assert_eq!(closes_behind_the_handoff(&app), vec![wid]);
    assert_close_dropped_beside_the_handoff(&app, other, armed[1].0);
    assert_eq!(
        replay_returned_handoff(&mut app),
        crate::CloseOutcome::Stay,
        "the replay quit aterm with no question"
    );
    assert!(!app.windows.contains_key(&wid));
    assert!(app.windows.contains_key(&other));
}

/// The same when the two closes come ready in separate passes: the first,
/// still armed behind the handoff, counts as gone when the second is weighed.
#[cfg(unix)]
#[test]
fn a_window_close_waiting_behind_an_update_handoff_counts_as_gone_in_a_later_pass() {
    let wid = WindowId(0);
    let (_dir, mut app, other, armed) = two_failed_window_closes();
    let _cancelled = crate::overlap_handoff_tests::arm_outgoing_handoff(&mut app);
    app.discard_document_changes(armed[0].0, armed[0].1)
        .unwrap();
    assert_eq!(
        app.take_ready_document_shutdowns().unwrap(),
        (false, vec![])
    );
    assert_eq!(closes_behind_the_handoff(&app), vec![wid]);
    app.discard_document_changes(armed[1].0, armed[1].1)
        .unwrap();
    assert_eq!(
        app.take_ready_document_shutdowns().unwrap(),
        (false, vec![])
    );
    assert_close_dropped_beside_the_handoff(&app, other, armed[1].0);
}

/// While window 0's close waits behind an update handoff, closing the
/// other window would close the last one, so its question asks about
/// quitting (here the question a busy window's close queues).
#[cfg(unix)]
#[test]
fn a_close_asked_while_another_waits_behind_an_update_handoff_asks_about_quitting() {
    let (_dir, mut app, document, view, (other, session)) = failed_window_close(|app, _| {
        let session = app.next_session_id;
        let other = app.insert_logical_window(crate::stub_session(session), 24, 80);
        app.frontmost_window = Some(WindowId(0));
        (other, session)
    });
    let _cancelled = crate::overlap_handoff_tests::arm_outgoing_handoff(&mut app);
    app.discard_document_changes(document, view).unwrap();
    assert_eq!(
        app.take_ready_document_shutdowns().unwrap(),
        (false, vec![])
    );
    assert_eq!(closes_behind_the_handoff(&app), vec![WindowId(0)]);
    crate::feed_session_for_test(
        &app,
        session,
        b"\x1b]133;A\x07$ \x1b]133;B\x07make\n\x1b]133;C\x07",
    );
    app.close_banner_queue
        .push(crate::close_confirm::PendingClose::new(
            other,
            crate::quit_safety::busy_view_close_prompt(false),
            crate::close_confirm::CloseReplay::Window(other),
            crate::session_store::ExitActor::Human,
        ));
    app.ask_next_queued_close();
    assert_eq!(
        app.close_banner.as_ref().map(|banner| banner.prompt.title),
        Some(crate::quit_safety::QUIT_TITLE),
        "closing the other window quits aterm once the handoff replays both closes"
    );
}

/// A plain close of a lower window, deferred beside a close that waits
/// behind an update handoff, replays first; the armed close then would
/// close the last window, so it is dropped rather than quit unasked.
#[cfg(unix)]
#[test]
fn a_replayed_window_close_that_became_the_last_is_dropped() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("conflict.md");
    fs::write(&path, "before\n").unwrap();
    let wid = WindowId(0);
    let mut app = App::headless_for_test();
    let session = app.next_session_id;
    let other = app.insert_logical_window(crate::stub_session(session), 24, 80);
    app.open_document_tab_in_window(other, AppKind::Editor, &file_uri(&path))
        .unwrap();
    let (instance, view) = app.active_native_view(other).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();
    commit(&mut app, other, "ours ");
    fs::write(&path, "theirs\n").unwrap();
    assert!(!app.prepare_window_document_shutdown(other).unwrap());
    let _cancelled = crate::overlap_handoff_tests::arm_outgoing_handoff(&mut app);
    app.discard_document_changes(document, view).unwrap();
    assert_eq!(
        app.take_ready_document_shutdowns().unwrap(),
        (false, vec![])
    );
    assert_eq!(app.close_window_logical(wid), crate::CloseOutcome::Stay);
    assert_eq!(closes_behind_the_handoff(&app), vec![wid, other]);
    assert_eq!(
        replay_returned_handoff(&mut app),
        crate::CloseOutcome::Stay,
        "the armed close finished over the last window and quit aterm unasked"
    );
    assert!(!app.windows.contains_key(&wid));
    assert!(app.windows.contains_key(&other));
    assert!(app.native_documents.pending_window_closes.is_empty());
    assert_eq!(
        app.document_store.phase(document),
        Some(crate::document_store::DocumentPhase::Active)
    );
}

/// A Quit that arrives while an update handoff holds a window's close
/// replaces that close (it dominates the teardown). When the handoff
/// returns, the close it no longer holds is disarmed: the window stays,
/// its document is editable, and no later save closes the window.
#[cfg(unix)]
#[test]
fn a_window_close_a_quit_replaced_behind_an_update_handoff_is_disarmed() {
    let (_dir, mut app, document, view, ()) = failed_window_close(|_, _| ());
    let _cancelled = crate::overlap_handoff_tests::arm_outgoing_handoff(&mut app);
    app.discard_document_changes(document, view).unwrap();
    assert_eq!(
        app.take_ready_document_shutdowns().unwrap(),
        (false, vec![])
    );
    assert_eq!(closes_behind_the_handoff(&app), vec![WindowId(0)]);
    assert!(
        app.defer_pending_update_handoff_teardown(crate::DeferredHandoffTeardown::QuitRequested)
    );
    assert!(
        closes_behind_the_handoff(&app).is_empty(),
        "PRECONDITION: the Quit replaced the close"
    );

    let attempt_id = app
        .pending_update_handoff
        .as_ref()
        .expect("a pending handoff")
        .attempt_id;
    let teardown = app.reduce_returned_handoff_completion(crate::UpdateHandoffCompletion {
        attempt_id,
        nonce: None,
        child_pid: None,
        outcome: crate::UpdateHandoffOutcome::ActivityRevoked,
        commit_fd: None,
        reject: None,
        reconcile: None,
        detail: "stood down".to_string(),
        input_drain_spins: 0,
        child_death: crate::ChildDeathEvidence::Unobserved,
        successor_start: crate::app_update_handoff::SuccessorStart::Unlaunched,
    });
    assert_eq!(
        teardown,
        Some(crate::DeferredHandoffTeardown::QuitRequested)
    );
    assert!(
        app.native_documents.pending_window_closes.is_empty(),
        "the close the Quit replaced is disarmed"
    );
    assert!(app.windows.contains_key(&WindowId(0)));
    assert_eq!(
        app.document_store.phase(document),
        Some(crate::document_store::DocumentPhase::Active),
        "its document is editable"
    );
}

#[test]
fn failed_quit_save_is_disarmed_and_cannot_late_exit() {
    let dir = fresh_dir("aterm-app-quit-save-failure");
    let path = dir.join("quit-conflict.md");
    fs::write(&path, "before\n").unwrap();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.open_document_tab(AppKind::Editor, &file_uri(&path))
        .unwrap();
    let (instance, view) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();
    commit(&mut app, wid, "ours ");
    assert!(!app.prepare_quit_document_shutdown_inner(false).unwrap());

    fs::write(&path, "theirs\n").unwrap();
    assert!(app.save_document_checkpoint(document, view).is_err());
    assert!(app.quit_document_shutdown_blocked());
    assert!(app.cancel_failed_quit_document_shutdown());
    assert!(!app.cancel_failed_quit_document_shutdown());
    assert_eq!(
        app.take_ready_document_shutdowns().unwrap(),
        (false, vec![])
    );
    assert!(app.windows.contains_key(&wid));
    assert_eq!(app.document_store.view_count(document), Some(1));
    assert_eq!(
        app.document_store.phase(document),
        Some(crate::document_store::DocumentPhase::Active)
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn dropped_markdown_opens_one_read_workspace_from_either_document_mode() {
    let dir = fresh_dir("aterm-app-document-drop");
    let source = dir.join("source.md");
    let dropped = dir.join("dropped file.md");
    fs::write(&source, "source\n").unwrap();
    fs::write(&dropped, "dropped\n").unwrap();

    for kind in [AppKind::Markdown, AppKind::Editor] {
        let mut app = App::headless_for_test();
        app.open_document_tab(kind, &file_uri(&source)).unwrap();
        let wid = WindowId(0);

        app.drop_file(wid, &dropped);
        let (instance, _) = app.active_native_view(wid).expect("dropped document view");
        assert_eq!(
            app.native_runtime.app(instance).map(NativeApp::kind),
            Some(AppKind::Editor)
        );
        let document = app
            .native_runtime
            .document_id(instance)
            .expect("dropped document identity");
        assert_eq!(
            app.document_store.snapshot(document).unwrap().text.as_ref(),
            "dropped\n"
        );

        let tabs_after_first_drop = app.windows[&wid].tab_set.len();
        app.drop_file(wid, &dropped);
        let (reused_instance, _) = app.active_native_view(wid).expect("reused document view");
        assert_eq!(reused_instance, instance);
        assert_eq!(app.windows[&wid].tab_set.len(), tabs_after_first_drop);
    }
    let _ = fs::remove_dir_all(dir);
}
