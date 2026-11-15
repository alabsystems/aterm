// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A document open that fails shows why in the window that asked for it, on one Recovery tab per
//! file, whose Retry opens the file again as the person first asked; and a failed open leaves no
//! grant it minted behind.

use std::fs;

use super::*;
use crate::native_app::{RecoveryApp, RecoveryCapability, RecoveryViewState};

/// A scratch folder holding `names`, each a small Markdown file, and their canonical URIs.
fn files(name: &str, names: &[&str]) -> (aterm_tempfile::TempDir, Vec<String>) {
    let dir = aterm_tempfile::Builder::new()
        .prefix(&format!("aterm-failed-open-{name}-"))
        .tempdir()
        .unwrap();
    let base = fs::canonicalize(dir.path()).unwrap();
    let uris = names
        .iter()
        .map(|file| {
            let path = base.join(file);
            fs::write(&path, format!("# {file}\n")).unwrap();
            crate::native_document_host::path_to_file_uri(&path).unwrap()
        })
        .collect();
    (dir, uris)
}

/// The file `uri` names, as a gesture hands it to [`App::open_path_for_person`].
fn path(uri: &str) -> std::path::PathBuf {
    crate::native_document_host::file_uri_path(uri).unwrap()
}

/// The front view of `wid`, when it is a Recovery tab: its app and view state.
fn front_recovery(app: &App, wid: WindowId) -> Option<(&RecoveryApp, &RecoveryViewState)> {
    let (instance, view) = app.active_native_view(wid)?;
    match (
        app.native_runtime.app(instance)?,
        app.native_runtime.view_state(view)?,
    ) {
        (NativeApp::Recovery(recovery), AppViewState::Recovery(state)) => Some((recovery, state)),
        _ => None,
    }
}

/// How many Recovery tabs `wid` holds.
fn recovery_tabs(app: &App, wid: WindowId) -> usize {
    app.windows[&wid]
        .tab_set
        .tabs()
        .iter()
        .filter(|tab| {
            let mut found = false;
            tab.root.any_leaf(&mut |&view| {
                found = matches!(
                    app.view_store.get(view).copied(),
                    Some(crate::tab_model::View::Native(native))
                        if matches!(app.native_runtime.app(native.instance), Some(NativeApp::Recovery(_)))
                );
                found
            });
            found
        })
        .count()
}

/// The canonical URI of the document `wid` shows in front, when it shows one.
fn front_document(app: &App, wid: WindowId) -> Option<String> {
    let (instance, _) = app.active_native_view(wid)?;
    let document = app.native_runtime.document_id(instance)?;
    app.document_store
        .canonical_uri(document)
        .map(str::to_string)
}

fn evicted(
    ticket: DocumentAdmissionTicket,
    wid: WindowId,
    key: &DocumentAdmissionKey,
) -> DocumentAdmissionOutcome {
    DocumentAdmissionOutcome {
        ticket,
        wid,
        key: key.clone(),
        result: Err(DocumentOpenFailure(DocumentHostError::Io {
            stage: crate::native_document_io::AtomicSaveStage::Preflight,
            kind: None,
            message: "the file is evicted from iCloud Drive and must be downloaded in Finder first"
                .to_string(),
        })),
    }
}

/// A file that fails again keeps its one Recovery tab, which says so and comes back to the
/// front. NEGATIVE CONTROL: a second file's failure is its own tab.
#[test]
fn a_second_failure_of_the_same_file_reuses_its_tab() {
    let (_dir, uris) = files("again", &["again.md", "other.md", "elsewhere.md"]);
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let key = DocumentAdmissionKey {
        uri: uris[0].clone(),
        kind: AppKind::Markdown,
        edit: None,
        config: false,
    };
    let first = app.mint_document_admission(wid, key.clone());
    app.complete_document_admission(evicted(first, wid, &key))
        .unwrap_err();
    let failure = app
        .active_native_view(wid)
        .expect("the failure is in front");
    let (_, state) = front_recovery(&app, wid).unwrap();
    assert_eq!(state.notice, None);
    let tabs = app.windows[&wid].tab_set.len();

    app.open_document_tab(AppKind::Editor, &uris[2]).unwrap();
    assert_eq!(front_document(&app, wid).as_deref(), Some(uris[2].as_str()));
    let second = app.mint_document_admission(wid, key.clone());
    app.complete_document_admission(evicted(second, wid, &key))
        .unwrap_err();
    assert_eq!(
        app.windows[&wid].tab_set.len(),
        tabs + 1,
        "only the document's tab is new"
    );
    assert_eq!(recovery_tabs(&app, wid), 1, "one tab for the one file");
    assert_eq!(
        app.active_native_view(wid),
        Some(failure),
        "it comes back to the front"
    );
    let (recovery, state) = front_recovery(&app, wid).unwrap();
    assert_eq!(state.notice.as_deref(), Some(STILL_COULD_NOT_OPEN));
    assert!(recovery.reason.contains("evicted from iCloud Drive"));

    let other = DocumentAdmissionKey {
        uri: uris[1].clone(),
        kind: AppKind::Markdown,
        edit: None,
        config: false,
    };
    let ticket = app.mint_document_admission(wid, other.clone());
    app.complete_document_admission(evicted(ticket, wid, &other))
        .unwrap_err();
    assert_eq!(
        recovery_tabs(&app, wid),
        2,
        "another file's failure is its own tab"
    );
    let (recovery, state) = front_recovery(&app, wid).unwrap();
    assert_eq!(state.notice, None);
    assert_eq!(
        recovery.capability,
        Some(RecoveryCapability::Document {
            kind: AppKind::Markdown,
            uri: uris[1].clone(),
            config_editor: false,
        })
    );
}

/// A busy draft says so in plain words: another aterm process is writing it, nothing changed. No
/// word on the page, its diagnostics, its tab or the opener's line names Manual or the drafts
/// folder; nor does any other draft failure's, whose own words say that folder as
/// [`crate::native_document_journal::DRAFTS_FOLDER_WORD`]. NEGATIVE CONTROLS: another file's held
/// draft does not make this open busy; (scratch) a lock failure said in its words whole fails the
/// drafts-folder check.
#[test]
fn busy_words_name_neither_manual_nor_the_drafts_path() {
    let (_dir, uris) = files("busy-words", &["mine.md", "theirs.md", "unlockable.md"]);
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let drafts = app
        .native_documents
        .journals
        .as_ref()
        .unwrap()
        .path_for_uri(&uris[0]);
    let drafts_root = drafts.parent().unwrap().to_string_lossy().into_owned();

    let theirs = app.hold_draft_lock_for_test(&uris[1]);
    app.open_path_for_person(wid, AppKind::Editor, &path(&uris[0]), None)
        .unwrap();
    assert_eq!(front_document(&app, wid).as_deref(), Some(uris[0].as_str()));
    assert_eq!(recovery_tabs(&app, wid), 0);
    drop(theirs);

    let held = app.hold_draft_lock_for_test(&uris[1]);
    let line = app
        .open_path_for_person(wid, AppKind::Editor, &path(&uris[1]), None)
        .unwrap_err();
    let (recovery, _) = front_recovery(&app, wid).expect("the busy open shows in its window");
    assert_eq!(recovery.reason, DRAFTS_BUSY_PROBLEM);
    assert_eq!(recovery.metadata, InstallFailure::DRAFTS_BUSY);
    let tab = app.windows[&wid].tab_set.active().unwrap();
    for words in [
        line.as_str(),
        recovery.reason.as_str(),
        recovery.metadata.as_str(),
        tab.presentation.title.as_str(),
        tab.presentation.tooltip.as_deref().unwrap_or_default(),
    ] {
        assert!(!words.contains("Manual"), "{words}");
        assert!(!words.contains(&drafts_root), "{words}");
        assert!(!words.contains(".lock"), "{words}");
    }
    assert!(app.document_store.id_for_uri(&uris[1]).is_none());
    drop(held);

    // A folder where the draft's lock file goes: no process can take that lock.
    let journal = app
        .native_documents
        .journals
        .as_ref()
        .unwrap()
        .path_for_uri(&uris[2]);
    let lock = journal.with_file_name(format!(
        ".{}.lock",
        journal.file_name().unwrap().to_str().unwrap()
    ));
    fs::create_dir_all(&lock).unwrap();
    let line = app
        .open_path_for_person(wid, AppKind::Editor, &path(&uris[2]), None)
        .unwrap_err();
    let (recovery, _) = front_recovery(&app, wid).expect("the unlockable draft shows");
    assert_ne!(
        recovery.reason, DRAFTS_BUSY_PROBLEM,
        "a lock no one holds is not busy"
    );
    assert!(
        recovery
            .metadata
            .contains(crate::native_document_journal::DRAFTS_FOLDER_WORD),
        "{}",
        recovery.metadata
    );
    let tab = app.windows[&wid].tab_set.active().unwrap();
    for words in [
        line.as_str(),
        recovery.reason.as_str(),
        recovery.metadata.as_str(),
        tab.presentation.title.as_str(),
        tab.presentation.tooltip.as_deref().unwrap_or_default(),
    ] {
        assert!(!words.contains(&drafts_root), "{words}");
    }
    assert!(app.document_store.id_for_uri(&uris[2]).is_none());
}

/// A failure tab keeps what its first open asked for when the file fails again, whatever failed:
/// its own Open Original (the editor, no caret) fails busy too, and the tab still stands for the
/// Markdown open at the linked heading, which its Retry opens once the draft is free. NEGATIVE
/// CONTROL (scratch): letting the repeat's kind and caret replace the tab's fails it.
#[test]
fn a_repeat_keeps_what_the_first_open_asked_for() {
    let (_dir, uris) = files("first-open", &["source.md", "guide.md"]);
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.open_document_tab(AppKind::Markdown, &uris[0]).unwrap();
    let source = app.document_store.id_for_uri(&uris[0]).unwrap();
    let held = app.hold_draft_lock_for_test(&uris[1]);
    app.open_markdown_link(wid, source, "guide.md#usage")
        .unwrap();
    let (_, view) = app.active_native_view(wid).expect("the busy link shows");
    let first_open = |app: &App| {
        let (recovery, _) = front_recovery(app, wid).expect("the failure is in front");
        assert_eq!(
            recovery.capability,
            Some(RecoveryCapability::Document {
                kind: AppKind::Markdown,
                uri: uris[1].clone(),
                config_editor: false,
            })
        );
        assert!(
            matches!(
                &recovery.origin,
                crate::native_app::RecoveryOrigin::Open {
                    reveal: Some(DocumentReveal::MarkdownFragment(fragment)),
                    entry: false,
                    ..
                } if fragment == "usage"
            ),
            "{:?}",
            recovery.origin
        );
    };
    first_open(&app);
    let press = |app: &mut App, action: &str| {
        app.act_app(crate::app_control::ActRequest {
            view,
            ui_key: action.to_string(),
            action: action.to_string(),
            value: None,
        })
        .unwrap();
    };
    press(&mut app, "recovery/open-original");
    assert_eq!(recovery_tabs(&app, wid), 1, "the repeat is the same tab");
    first_open(&app);

    drop(held);
    press(&mut app, "recovery/retry");
    assert_eq!(front_document(&app, wid).as_deref(), Some(uris[1].as_str()));
    assert_eq!(
        recovery_tabs(&app, wid),
        0,
        "the document took the failure's place"
    );
}

/// A draft another process rewrote between this open's look at it and its taking it opens
/// nothing: the window says the draft changed, and no document, grant or pending admission is
/// left. CONTROL: the next open, under which nothing changes, opens. NEGATIVE CONTROL (scratch):
/// taking the changed draft as the one inspected opens the first and fails it.
#[test]
fn a_changed_draft_says_so_and_opens_nothing() {
    let (_dir, uris) = files("changed", &["changed.md", "settles.md"]);
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.change_drafts_under_opens_for_test(1);
    app.open_path_for_person(wid, AppKind::Editor, &path(&uris[0]), None)
        .unwrap_err();
    let (recovery, _) = front_recovery(&app, wid).expect("the changed draft shows");
    assert_eq!(
        recovery.reason,
        crate::native_document_journal::JournalFault::CHANGED
    );
    assert!(app.document_store.id_for_uri(&uris[0]).is_none());
    assert_eq!(app.native_documents.grants.id_for_uri(&uris[0]), None);
    assert!(app.native_documents.admissions.is_empty());

    app.open_path_for_person(wid, AppKind::Editor, &path(&uris[1]), None)
        .unwrap();
    assert_eq!(
        front_document(&app, wid).as_deref(),
        Some(uris[1].as_str()),
        "CONTROL: the settled draft opens"
    );
    assert_eq!(
        recovery_tabs(&app, wid),
        1,
        "CONTROL: only the changed open's tab"
    );
}

/// A file that became a link between its read and its grant fails the open in the host's own
/// typed words on the page, the detail keeping its whole line. NEGATIVE CONTROL (scratch): the
/// grant's refusal said as the detail's words fails it.
#[cfg(unix)]
#[test]
fn a_grant_refused_after_the_read_says_why_in_its_own_words() {
    let (_dir, uris) = files("refused-grant", &["file.md", "elsewhere.md"]);
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.open_document_tab(AppKind::Editor, &uris[0]).unwrap();
    let key = DocumentAdmissionKey {
        uri: uris[0].clone(),
        kind: AppKind::Markdown,
        edit: None,
        config: false,
    };
    let ticket = app.mint_document_admission(wid, key.clone());
    let options = app.admission_options(&key);
    let admitted =
        App::admit_document_request(&key, DocumentAdmissionPurpose::Open, options, false).unwrap();
    let file = crate::native_document_host::file_uri_path(&uris[0]).unwrap();
    fs::remove_file(&file).unwrap();
    std::os::unix::fs::symlink(
        crate::native_document_host::file_uri_path(&uris[1]).unwrap(),
        &file,
    )
    .unwrap();
    app.complete_document_admission(DocumentAdmissionOutcome {
        ticket,
        wid,
        key,
        result: Ok(Box::new(admitted)),
    })
    .unwrap_err();
    let (recovery, _) = front_recovery(&app, wid).expect("the refused grant shows");
    assert!(!recovery.reason.is_empty());
    assert!(
        recovery.metadata.len() > recovery.reason.len()
            && recovery.metadata.ends_with(recovery.reason.as_str()),
        "the page says the host's words; the detail is the whole line: {:?} / {:?}",
        recovery.reason,
        recovery.metadata
    );
}

/// A busy draft's open shows in its window at once and leaves nothing pending: no admission,
/// no caret request, no document. Its tab keeps the line the open asked for, and its Retry
/// opens the document once the holder lets go. NEGATIVE CONTROL: Retry while the draft is still
/// held says so on the same tab.
#[test]
fn a_busy_draft_shows_at_once_and_its_retry_opens_once_the_holder_lets_go() {
    let (_dir, uris) = files("busy", &["shared.md"]);
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let tabs = app.windows[&wid].tab_set.len();

    let held = app.hold_draft_lock_for_test(&uris[0]);
    let line = app
        .open_path_for_person(
            wid,
            AppKind::Editor,
            &path(&uris[0]),
            Some(DocumentReveal::Line(LineReveal { line: 2, col: None })),
        )
        .unwrap_err();
    assert_eq!(line, InstallFailure::DRAFTS_BUSY);
    assert_eq!(app.windows[&wid].tab_set.len(), tabs + 1);
    let (_, view) = app.active_native_view(wid).expect("the busy open shows");
    let (recovery, state) = front_recovery(&app, wid).expect("the busy open shows");
    assert_eq!(recovery.reason, DRAFTS_BUSY_PROBLEM);
    assert_eq!(state.notice, None);
    assert_eq!(
        recovery.origin,
        crate::native_app::RecoveryOrigin::Open {
            path: shown_path(&uris[0]),
            reveal: Some(DocumentReveal::Line(LineReveal { line: 2, col: None })),
            entry: false,
            asked: None,
        }
    );
    assert!(app.native_documents.admissions.is_empty());
    assert!(app.native_documents.reveals.is_empty());
    assert!(app.document_store.id_for_uri(&uris[0]).is_none());

    let retry = |app: &mut App| {
        app.act_app(crate::app_control::ActRequest {
            view,
            ui_key: "recovery/retry".to_string(),
            action: "recovery/retry".to_string(),
            value: None,
        })
        .unwrap();
    };
    retry(&mut app);
    assert_eq!(
        recovery_tabs(&app, wid),
        1,
        "the held Retry is the same tab"
    );
    let (_, state) = front_recovery(&app, wid).expect("the held Retry stays in front");
    assert_eq!(state.notice.as_deref(), Some(STILL_COULD_NOT_OPEN));
    assert!(app.native_documents.admissions.is_empty());

    drop(held);
    retry(&mut app);
    assert_eq!(front_document(&app, wid).as_deref(), Some(uris[0].as_str()));
    assert_eq!(recovery_tabs(&app, wid), 0);
    assert_eq!(app.windows[&wid].tab_set.len(), tabs + 1);
    assert!(app.native_documents.admissions.is_empty());
    assert!(app.native_documents.reveals.is_empty());
}

/// An open that fails before its document binds the grant it minted forgets that grant, so no
/// capability outlives the failure. NEGATIVE CONTROLS: a file opened before keeps its grant, and
/// the failed file opened again once its draft is free gets a grant afresh.
#[test]
fn a_failed_fresh_install_forgets_the_grant_it_minted() {
    let (_dir, uris) = files("grant", &["a.md", "b.md"]);
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.open_document_tab(AppKind::Editor, &uris[1]).unwrap();
    let b = app.native_documents.grants.id_for_uri(&uris[1]);
    assert!(b.is_some());

    let held = app.hold_draft_lock_for_test(&uris[0]);
    app.open_path_for_person(wid, AppKind::Editor, &path(&uris[0]), None)
        .unwrap_err();
    assert!(front_recovery(&app, wid).is_some(), "the busy open shows");
    assert_eq!(app.native_documents.grants.id_for_uri(&uris[0]), None);
    assert_eq!(app.native_documents.grants.id_for_uri(&uris[1]), b);

    drop(held);
    app.open_path_for_person(wid, AppKind::Editor, &path(&uris[0]), None)
        .unwrap();
    assert_eq!(front_document(&app, wid).as_deref(), Some(uris[0].as_str()));
    let a = app.native_documents.grants.id_for_uri(&uris[0]);
    assert!(a.is_some() && a != b);
    assert_eq!(
        recovery_tabs(&app, wid),
        0,
        "the document took the failure's place"
    );
}

fn commit(app: &mut App, wid: WindowId, text: &str) {
    app.dispatch_native_event(
        wid,
        AppEvent::TextInput(TextInputEvent::Commit(text.to_string())),
    )
    .unwrap();
}

/// What a gesture needs in window 0: a directory listing whose Enter opens an entry, and a
/// Markdown document in front whose links open their targets (and onto which a file drops).
struct GestureRig {
    wid: WindowId,
    source: DocumentId,
    directory: crate::tab_model::ViewId,
}

/// A fresh App holding [`GestureRig`] over `base`, its draft journals under `drafts`.
fn gesture_rig(base: &std::path::Path, drafts: std::path::PathBuf) -> (App, GestureRig) {
    let mut app = App::headless_for_test();
    app.native_documents.journals = JournalSlot::for_test(drafts);
    let wid = WindowId(0);
    app.open_document_tab(
        AppKind::Editor,
        &crate::native_document_host::path_to_file_uri(base).unwrap(),
    )
    .unwrap();
    let (_, directory) = app.active_native_view(wid).unwrap();
    let source_uri =
        crate::native_document_host::path_to_file_uri(&base.join("source.md")).unwrap();
    app.open_document_tab(AppKind::Markdown, &source_uri)
        .unwrap();
    let source = app.document_store.id_for_uri(&source_uri).unwrap();
    (
        app,
        GestureRig {
            wid,
            source,
            directory,
        },
    )
}

type Gesture = fn(&mut App, &GestureRig, &std::path::Path);

/// Every way a person opens a file, each on its own file: File ▸ Open, a drop on a document, a
/// document a menu wrote, a clicked citation, a Markdown link and a directory listing's Enter.
fn gestures() -> [(&'static str, Gesture); 6] {
    [
        ("file-open.md", |app, _, path| {
            let _ = app.open_local_document_path(path);
        }),
        ("drop.md", |app, rig, path| app.drop_file(rig.wid, path)),
        ("menu.md", |app, rig, path| {
            app.open_menu_document(rig.wid, path.to_path_buf());
        }),
        ("citation.md", |app, rig, path| {
            app.open_citation(
                rig.wid,
                &crate::path_link::PathTarget {
                    path: path.to_path_buf(),
                    line: Some(1),
                    col: None,
                    shown: "citation.md:1".to_string(),
                },
            );
        }),
        ("link.md", |app, rig, path| {
            let name = path.file_name().unwrap().to_str().unwrap();
            let _ = app.open_markdown_link(rig.wid, rig.source, name);
        }),
        ("entry.md", |app, rig, path| {
            app.execute_editor_effect(
                rig.directory,
                EditorEffect::OpenPath {
                    uri: crate::native_document_host::path_to_file_uri(path).unwrap(),
                },
            )
            .unwrap();
        }),
    ]
}

/// Each gesture's open of a file whose draft another aterm process holds shows exactly one
/// Recovery tab in the window that asked, in the busy words, standing for the file. A path no file URI names, and a request refused before
/// any admission, show the same way, the first with Retry disabled, as does a Markdown link that
/// no longer resolves; a repeat of either is the same tab. CONTROL: with the draft free, each
/// gesture opens its document and shows no failure. NEGATIVE CONTROLS (scratch): skipping the
/// install failure's show fails the six gestures; skipping it in `open_path_for_person` fails the
/// unnamed path; skipping it in `open_document_tab_revealing` fails the refused request; an
/// unresolved link returned before its show fails the link; matching a tab only by its URI fails
/// the repeats.
#[test]
fn every_gesture_shows_its_failure_in_its_window() {
    let names = [
        "source.md",
        "file-open.md",
        "drop.md",
        "menu.md",
        "citation.md",
        "link.md",
        "entry.md",
        "refused.md",
    ];
    let (dir, _) = files("gestures", &names);
    let base = fs::canonicalize(dir.path()).unwrap();
    let mut rigs = 0;
    let mut drafts = || {
        rigs += 1;
        base.join(format!("drafts-{rigs}"))
    };
    for (name, gesture) in gestures() {
        let path = base.join(name);
        let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
        let (mut app, rig) = gesture_rig(&base, drafts());
        let held = app.hold_draft_lock_for_test(&uri);
        gesture(&mut app, &rig, &path);
        assert_eq!(recovery_tabs(&app, rig.wid), 1, "{name}: one Recovery tab");
        let (recovery, _) = front_recovery(&app, rig.wid)
            .unwrap_or_else(|| panic!("{name}: the failure is in front"));
        assert_eq!(recovery.reason, DRAFTS_BUSY_PROBLEM, "{name}");
        assert!(
            matches!(
                &recovery.capability,
                Some(RecoveryCapability::Document { uri: stands_for, .. }) if *stands_for == uri
            ),
            "{name}: {:?}",
            recovery.capability
        );
        assert!(app.document_store.id_for_uri(&uri).is_none(), "{name}");
        assert!(app.native_documents.admissions.is_empty(), "{name}");
        drop(held);

        let (mut app, rig) = gesture_rig(&base, drafts());
        gesture(&mut app, &rig, &path);
        assert_eq!(
            front_document(&app, rig.wid).as_deref(),
            Some(uri.as_str()),
            "CONTROL {name}: the free draft opens"
        );
        assert_eq!(recovery_tabs(&app, rig.wid), 0, "CONTROL {name}");
    }

    #[cfg(unix)]
    {
        use crate::native_app::NativeAppModel;
        use std::os::unix::ffi::OsStrExt;
        let (mut app, rig) = gesture_rig(&base, drafts());
        let path = base.join(std::ffi::OsStr::from_bytes(b"caf\xe9.md"));
        for attempt in 0..2 {
            app.open_path_for_person(rig.wid, AppKind::Markdown, &path, None)
                .unwrap_err();
            assert_eq!(
                recovery_tabs(&app, rig.wid),
                1,
                "the unnamed path shows on one tab, try {attempt}"
            );
        }
        let (recovery, state) = front_recovery(&app, rig.wid).expect("the unnamed path's tab");
        assert_eq!(recovery.capability, None);
        assert_eq!(recovery.reason, UNNAMED_PATH_PROBLEM);
        assert_eq!(
            state.notice.as_deref(),
            Some(STILL_COULD_NOT_OPEN),
            "the repeat says so in place"
        );
        let mut commands = Vec::new();
        recovery.commands(state, &mut commands);
        for id in ["recovery/retry", "recovery/open-original"] {
            let command = commands
                .iter()
                .find(|command| command.id.as_str() == id)
                .unwrap();
            assert!(!command.enabled, "{id}");
        }
        assert!(app.native_documents.admissions.is_empty());
    }

    // A Markdown link that no longer resolves when it lands (its target names no file URI).
    let (mut app, rig) = gesture_rig(&base, drafts());
    for attempt in 0..2 {
        app.open_markdown_link(rig.wid, rig.source, "caf%E9.md#top")
            .unwrap_err();
        assert_eq!(
            recovery_tabs(&app, rig.wid),
            1,
            "the unresolved link shows on one tab, try {attempt}"
        );
    }
    let (recovery, _) = front_recovery(&app, rig.wid).expect("the unresolved link's tab");
    assert_eq!(recovery.capability, None);
    assert!(app.native_documents.admissions.is_empty());

    let (mut app, rig) = gesture_rig(&base, drafts());
    let refused = base.join("refused.md");
    let refused_uri = crate::native_document_host::path_to_file_uri(&refused).unwrap();
    let why = app
        .open_path_for_person(rig.wid, AppKind::Settings, &refused, None)
        .unwrap_err();
    assert_eq!(recovery_tabs(&app, rig.wid), 1, "the refused request shows");
    let (recovery, _) = front_recovery(&app, rig.wid).expect("the refused request's tab");
    assert_eq!(recovery.reason, why);
    assert!(
        matches!(
            &recovery.capability,
            Some(RecoveryCapability::Document { uri, .. }) if *uri == refused_uri
        ),
        "{:?}",
        recovery.capability
    );
    assert!(app.native_documents.admissions.is_empty());
}

/// A failed open's Retry opens the file again as the person first asked, and once the file
/// opens its document takes the failure tab's place, with the draft the busy opens never wrote
/// to. While the draft is still held, Retry says so on the same tab. NEGATIVE CONTROL: a
/// Restore-origin Retry of the same file answers its own caller, so while the draft is held it
/// says busy there and leaves the failure tab as it is.
#[test]
fn retry_on_a_failure_tab_opens_the_file_and_takes_its_place() {
    let (dir, uris) = files("retry", &["retry.md", "before.md", "after.md"]);
    let drafts = fs::canonicalize(dir.path()).unwrap().join("drafts");
    let wid = WindowId(0);
    let mut crashed = App::headless_for_test();
    crashed.native_documents.journals = JournalSlot::for_test(drafts.clone());
    crashed
        .open_document_tab(AppKind::Editor, &uris[0])
        .unwrap();
    commit(&mut crashed, wid, "draft ");
    crashed.exit_for_test();
    let mut app = App::headless_for_test();
    app.native_documents.journals = JournalSlot::for_test(drafts);
    let journal = app.draft_journal_for_test(&uris[0]);
    let drafted = fs::read(&journal).expect("the crashed App's draft");

    app.open_document_tab(AppKind::Editor, &uris[1]).unwrap();
    let held = app.hold_draft_lock_for_test(&uris[0]);
    app.open_path_for_person(wid, AppKind::Markdown, &path(&uris[0]), None)
        .unwrap_err();
    let (_, failure_view) = app.active_native_view(wid).unwrap();
    assert!(front_recovery(&app, wid).is_some(), "the busy open shows");
    let failure = app.windows[&wid].tab_set.active_id().unwrap();
    let index = |app: &App, tab| {
        app.windows[&wid]
            .tab_set
            .tabs()
            .iter()
            .position(|candidate| candidate.id == tab)
    };
    let at = index(&app, failure).unwrap();
    app.open_document_tab(AppKind::Editor, &uris[2]).unwrap();
    app.windows
        .get_mut(&wid)
        .unwrap()
        .tab_set
        .switch_to(failure);
    app.resync_active_or_window(wid);
    assert_eq!(
        app.active_native_view(wid).map(|(_, view)| view),
        Some(failure_view)
    );
    assert_eq!(
        fs::read(&journal).unwrap(),
        drafted,
        "the busy open wrote nothing"
    );

    let answered = std::rc::Rc::new(std::cell::RefCell::new(None));
    let answer = std::rc::Rc::clone(&answered);
    app.execute_recovery_request(
        wid,
        crate::native_app::RecoveryRequest::Retry(RecoveryCapability::Document {
            kind: AppKind::Markdown,
            uri: uris[0].clone(),
            config_editor: false,
        }),
        |_| true,
        move |_, outcome| {
            *answer.borrow_mut() = Some(outcome);
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(
        answered.borrow_mut().take(),
        Some(crate::native_app::RecoveryOutcome::Failed {
            message: InstallFailure::DRAFTS_BUSY.to_string(),
        }),
        "CONTROL: the Restore-origin Retry answers its caller"
    );
    assert_eq!(
        index(&app, failure),
        Some(at),
        "CONTROL: the failure tab stays"
    );
    let (_, state) = front_recovery(&app, wid).expect("the failure tab is still in front");
    assert_eq!(state.notice, None, "CONTROL: nothing was said on the tab");

    let retry = |app: &mut App| {
        app.act_app(crate::app_control::ActRequest {
            view: failure_view,
            ui_key: "recovery/retry".to_string(),
            action: "recovery/retry".to_string(),
            value: None,
        })
        .unwrap();
    };
    retry(&mut app);
    let (_, state) = front_recovery(&app, wid).expect("the held Retry keeps its tab");
    assert_eq!(state.pending, None);
    assert_eq!(state.notice.as_deref(), Some(STILL_COULD_NOT_OPEN));
    assert_eq!(index(&app, failure), Some(at));
    assert_eq!(
        fs::read(&journal).unwrap(),
        drafted,
        "the busy Retry wrote nothing"
    );

    drop(held);
    retry(&mut app);
    assert_eq!(index(&app, failure), None, "the failure tab is gone");
    assert_eq!(recovery_tabs(&app, wid), 0);
    assert_eq!(front_document(&app, wid).as_deref(), Some(uris[0].as_str()));
    let document = app.document_store.id_for_uri(&uris[0]).unwrap();
    let tab = app.windows[&wid].tab_set.active_id().unwrap();
    assert_eq!(
        index(&app, tab),
        Some(at),
        "the document takes the failure's place"
    );
    assert_eq!(
        app.document_store.snapshot(document).unwrap().text.as_ref(),
        "draft # retry.md\n",
        "it opens with its draft"
    );
    assert_eq!(app.document_store.dirty(document), Some(true));
}

/// With a worker to report back, a failed open's Retry is in flight: its tab says it is opening
/// the file and keeps its place, and when the worker reports, the document takes that place. A
/// read that fails on the worker shows on that same tab again. NEGATIVE CONTROL (scratch): a
/// read's failure left unshown fails it.
#[test]
fn a_retry_in_flight_says_it_is_opening_and_lands_on_its_tab() {
    let (_dir, uris) = files("in-flight", &["flight.md", "after.md"]);
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let key = DocumentAdmissionKey {
        uri: uris[0].clone(),
        kind: AppKind::Markdown,
        edit: None,
        config: false,
    };
    let first = app.mint_document_admission(wid, key.clone());
    app.complete_document_admission(evicted(first, wid, &key))
        .unwrap_err();
    let (_, failure_view) = app
        .active_native_view(wid)
        .expect("the failure is in front");
    let failure = app.windows[&wid].tab_set.active_id().unwrap();
    let index = |app: &App, tab| {
        app.windows[&wid]
            .tab_set
            .tabs()
            .iter()
            .position(|candidate| candidate.id == tab)
    };
    let at = index(&app, failure).unwrap();
    app.open_document_tab(AppKind::Editor, &uris[1]).unwrap();
    app.windows
        .get_mut(&wid)
        .unwrap()
        .tab_set
        .switch_to(failure);
    app.resync_active_or_window(wid);

    let (delivered, deliveries) = std::sync::mpsc::channel();
    DOCUMENT_ADMISSION_DELIVERY.set(Some(std::sync::Arc::new(move |outcome| {
        delivered.send(outcome).unwrap();
    })));
    let retry = |app: &mut App| {
        app.act_app(crate::app_control::ActRequest {
            view: failure_view,
            ui_key: "recovery/retry".to_string(),
            action: "recovery/retry".to_string(),
            value: None,
        })
        .unwrap();
    };
    let opening = |app: &App| {
        let (_, state) = front_recovery(app, wid).expect("Retry keeps its tab while it opens");
        assert_eq!(state.pending, None);
        assert!(
            state
                .notice
                .as_deref()
                .is_some_and(|notice| notice.contains("flight.md")),
            "the tab says it is opening the file: {:?}",
            state.notice
        );
        assert_eq!(app.native_documents.admissions.len(), 1);
    };

    retry(&mut app);
    opening(&app);
    let read = deliveries
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .unwrap();
    app.complete_document_admission(evicted(read.ticket, wid, &key))
        .unwrap_err();
    assert_eq!(
        index(&app, failure),
        Some(at),
        "the failed Retry keeps its tab"
    );
    assert_eq!(recovery_tabs(&app, wid), 1);
    let (_, state) = front_recovery(&app, wid).expect("the failed Retry stays in front");
    assert_eq!(state.notice.as_deref(), Some(STILL_COULD_NOT_OPEN));
    assert!(app.native_documents.admissions.is_empty());

    retry(&mut app);
    opening(&app);
    let read = deliveries
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .unwrap();
    DOCUMENT_ADMISSION_DELIVERY.set(None);
    app.complete_document_admission(read).unwrap();
    let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while front_document(&app, wid).as_deref() != Some(uris[0].as_str()) {
        assert!(
            std::time::Instant::now() < deadline,
            "the Retry's document did not open"
        );
        app.progress_document_preparations();
        aterm_uds::deadline::poll_yield();
    }
    assert_eq!(index(&app, failure), None, "the failure tab is gone");
    assert_eq!(recovery_tabs(&app, wid), 0);
    let tab = app.windows[&wid].tab_set.active_id().unwrap();
    assert_eq!(
        index(&app, tab),
        Some(at),
        "the document takes the failure's place"
    );
    assert!(app.native_documents.admissions.is_empty());
}

/// A Markdown link whose heading is missing still opens its document, whose status says the
/// heading was not found; no Recovery tab says it failed. CONTROL: a heading that exists sets no
/// such status. NEGATIVE CONTROL (scratch): dropping the opened view's status fails the first.
#[test]
fn a_link_whose_heading_is_missing_opens_and_says_so() {
    let (_dir, uris) = files("heading", &["source.md", "target.md", "present.md"]);
    fs::write(
        crate::native_document_host::file_uri_path(&uris[2]).unwrap(),
        "# Present\n",
    )
    .unwrap();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.open_document_tab(AppKind::Markdown, &uris[0]).unwrap();
    let source = app.document_store.id_for_uri(&uris[0]).unwrap();
    let status = |app: &App| {
        let (_, view) = app.active_native_view(wid)?;
        match app.native_runtime.view_state(view)? {
            AppViewState::Editor(state) => state.status.clone(),
            _ => None,
        }
    };

    app.open_markdown_link(wid, source, "target.md#missing")
        .unwrap();
    assert_eq!(front_document(&app, wid).as_deref(), Some(uris[1].as_str()));
    assert!(
        status(&app).is_some_and(|status| status.contains("#missing")),
        "the opened view names the missing heading: {:?}",
        status(&app)
    );
    assert_eq!(recovery_tabs(&app, wid), 0);

    app.open_markdown_link(wid, source, "present.md#present")
        .unwrap();
    assert_eq!(front_document(&app, wid).as_deref(), Some(uris[2].as_str()));
    assert!(
        !status(&app).is_some_and(|status| status.contains("#present")),
        "CONTROL: {:?}",
        status(&app)
    );
    assert_eq!(recovery_tabs(&app, wid), 0);
}

/// The route a real window takes (a worker reads the file and a worker prepares its draft): a
/// person's open of a file whose draft another aterm process holds shows in its window once that
/// draft stayed busy through two attempts, never sooner and never without bound. Its tab keeps
/// what the open asked for; its Retry is answered by an attempt of its own, not by the earlier
/// open's backoff, says so again while the draft is held, and opens the document in the tab's
/// place once the holder lets go.
///
/// NEGATIVE CONTROLS: a draft let go after one failed attempt opens with no failure tab; a clock
/// that never moves gets the Retry its attempt only because the Retry made it due; (scratch) a
/// request never refused leaves the first wait to its hang detector; (scratch) a Retry that
/// does not hurry its owner leaves the frozen-clock wait to its hang detector.
#[cfg(unix)]
#[test]
fn a_busy_draft_under_preparation_shows_after_two_attempts_and_its_retry_opens_once_free() {
    let (_dir, uris) = files("prepared-busy", &["held.md", "freed.md"]);
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let tabs = app.windows[&wid].tab_set.len();
    let opens = WorkerOpens::start();
    let settled = |app: &App| app.native_documents.admissions.is_empty().then_some(());

    let held = app.hold_draft_lock_for_test(&uris[0]);
    let line = app
        .open_path_for_person(
            wid,
            AppKind::Editor,
            &path(&uris[0]),
            Some(DocumentReveal::Line(LineReveal { line: 2, col: None })),
        )
        .expect("a worker reads the file");
    assert_eq!(line, format!("admitting editor {}", uris[0]));
    assert_eq!(recovery_tabs(&app, wid), 0, "nothing shows before the read");
    opens.until(&mut app, settled);
    assert_eq!(
        WorkerOpens::failed_attempts(&app, &uris[0]),
        2,
        "the open waited through exactly two attempts"
    );
    assert_eq!(app.windows[&wid].tab_set.len(), tabs + 1);
    let (_, view) = app.active_native_view(wid).expect("the busy open shows");
    let failure = app.windows[&wid].tab_set.active_id().unwrap();
    let (recovery, state) = front_recovery(&app, wid).expect("the busy open shows");
    assert_eq!(recovery.reason, DRAFTS_BUSY_PROBLEM);
    assert_eq!(recovery.metadata, InstallFailure::DRAFTS_BUSY);
    assert_eq!(state.notice, None);
    assert_eq!(
        recovery.capability,
        Some(RecoveryCapability::Document {
            kind: AppKind::Editor,
            uri: uris[0].clone(),
            config_editor: false,
        })
    );
    assert_eq!(
        recovery.origin,
        crate::native_app::RecoveryOrigin::Open {
            path: shown_path(&uris[0]),
            asked: None,
            reveal: Some(DocumentReveal::Line(LineReveal { line: 2, col: None })),
            entry: false,
        }
    );
    assert!(app.native_documents.reveals.is_empty());
    assert!(
        app.open_native_documents().is_empty(),
        "no view shows the held file"
    );

    let retry = |app: &mut App| {
        app.act_app(crate::app_control::ActRequest {
            view,
            ui_key: "recovery/retry".to_string(),
            action: "recovery/retry".to_string(),
            value: None,
        })
        .unwrap();
    };
    retry(&mut app);
    opens.until_without_time(&mut app, |app| {
        (WorkerOpens::failed_attempts(app, &uris[0]) == 3).then_some(())
    });
    assert_eq!(
        app.native_documents.admissions.len(),
        1,
        "one failed attempt refuses nothing"
    );
    opens.until(&mut app, settled);
    assert_eq!(WorkerOpens::failed_attempts(&app, &uris[0]), 4);
    assert_eq!(
        recovery_tabs(&app, wid),
        1,
        "the held Retry is the same tab"
    );
    assert_eq!(app.windows[&wid].tab_set.active_id(), Some(failure));
    let (_, state) = front_recovery(&app, wid).expect("the held Retry stays in front");
    assert_eq!(state.notice.as_deref(), Some(STILL_COULD_NOT_OPEN));

    drop(held);
    retry(&mut app);
    opens.until(&mut app, |app| {
        (front_document(app, wid).as_deref() == Some(uris[0].as_str())).then_some(())
    });
    assert_eq!(
        recovery_tabs(&app, wid),
        0,
        "the document took the failure's place"
    );
    assert_eq!(app.windows[&wid].tab_set.len(), tabs + 1);
    assert!(app.native_documents.admissions.is_empty());
    let (_, opened) = app.active_native_view(wid).unwrap();
    assert!(
        matches!(
            app.native_runtime.view_state(opened),
            Some(AppViewState::Editor(state)) if state.status.as_deref() == Some("Line 2")
        ),
        "the caret lands where the first open asked"
    );

    let freed = app.hold_draft_lock_for_test(&uris[1]);
    app.open_path_for_person(wid, AppKind::Editor, &path(&uris[1]), None)
        .unwrap();
    opens.until(&mut app, |app| {
        (WorkerOpens::failed_attempts(app, &uris[1]) == 1).then_some(())
    });
    assert_eq!(app.native_documents.admissions.len(), 1);
    drop(freed);
    opens.until(&mut app, |app| {
        (front_document(app, wid).as_deref() == Some(uris[1].as_str())).then_some(())
    });
    assert_eq!(
        recovery_tabs(&app, wid),
        0,
        "one failed attempt shows nothing"
    );
    assert_eq!(app.windows[&wid].tab_set.len(), tabs + 2);
}

/// A read's failure says what went wrong on its page and keeps the stage that met it for the
/// diagnostics. NEGATIVE CONTROL (scratch): the page given the host error whole fails it.
#[test]
fn a_failed_read_names_its_stage_in_the_diagnostics_only() {
    let (_dir, uris) = files("stage", &["evicted.md"]);
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let key = DocumentAdmissionKey {
        uri: uris[0].clone(),
        kind: AppKind::Markdown,
        edit: None,
        config: false,
    };
    let ticket = app.mint_document_admission(wid, key.clone());
    app.complete_document_admission(evicted(ticket, wid, &key))
        .unwrap_err();
    let (recovery, _) = front_recovery(&app, wid).expect("the failed read shows");
    assert_eq!(
        recovery.reason,
        "the file is evicted from iCloud Drive and must be downloaded in Finder first"
    );
    assert_eq!(
        recovery.metadata,
        format!("document open failed: Preflight: {}", recovery.reason)
    );
}

/// A failure tab is found by either URI it stands for, however each is spelled: the canonical
/// one its read resolved, and the one the open asked for (a link to it, say). A later failure
/// under another spelling of either is the same tab, which goes on standing for both, and the
/// document that opens takes its place. NEGATIVE CONTROLS: another file's open leaves the tab
/// standing; (scratch) URIs compared as text make the respelled failure a second tab.
#[test]
fn a_failure_tab_stands_for_the_asked_and_the_canonical_uri_however_spelled() {
    let (_dir, uris) = files("spelled", &["a b.md", "other.md"]);
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let canonical = &uris[0];
    let folder = canonical
        .strip_suffix("a%20b.md")
        .expect("the space is escaped");
    let link = format!("{folder}link.md");
    let link_respelled = format!("{folder}%6Cink.md");
    let fail = |app: &mut App, requested: &str, canonical: Option<&str>| {
        app.show_failed_open(
            wid,
            FailedOpen {
                kind: AppKind::Markdown,
                requested_uri: Some(requested.to_string()),
                canonical_uri: canonical.map(str::to_string),
                path_shown: shown_path(requested),
                problem: "it did not open".to_string(),
                detail: "it did not open".to_string(),
                reveal: None,
                entry: false,
            },
        );
    };
    let stands_for = |app: &App| {
        let (recovery, _) = front_recovery(app, wid).expect("the failure is in front");
        let Some(RecoveryCapability::Document { uri, .. }) = &recovery.capability else {
            panic!("the tab names its file: {:?}", recovery.capability);
        };
        let crate::native_app::RecoveryOrigin::Open { asked, .. } = &recovery.origin else {
            panic!("the tab is a failed open's: {:?}", recovery.origin);
        };
        (uri.clone(), asked.clone())
    };

    fail(&mut app, &link, Some(canonical));
    assert_eq!(recovery_tabs(&app, wid), 1);
    assert_eq!(stands_for(&app), (canonical.clone(), Some(link.clone())));

    fail(&mut app, &link_respelled, None);
    assert_eq!(
        recovery_tabs(&app, wid),
        1,
        "the asked URI respelled is the same tab"
    );
    let (_, state) = front_recovery(&app, wid).unwrap();
    assert_eq!(state.notice.as_deref(), Some(STILL_COULD_NOT_OPEN));
    assert_eq!(
        stands_for(&app),
        (link_respelled.clone(), Some(canonical.clone())),
        "it still stands for both"
    );

    app.open_document_tab(AppKind::Editor, &uris[1]).unwrap();
    assert_eq!(
        recovery_tabs(&app, wid),
        1,
        "another file's open leaves the tab standing"
    );
    app.open_document_tab(AppKind::Editor, canonical).unwrap();
    assert_eq!(
        front_document(&app, wid).as_deref(),
        Some(canonical.as_str())
    );
    assert_eq!(
        recovery_tabs(&app, wid),
        0,
        "the document took the failure's place"
    );
}
