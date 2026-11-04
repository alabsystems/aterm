// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`crate::owner_keys_save`] on a headless App: Manual bound to a scratch aterm.toml as the
//! aterm.toml in force, every save the editor's own, and the file on disk read back after it.

use std::path::{Path, PathBuf};

use super::JournalSlot;
use crate::document_store::DocumentId;
use crate::native_app::{AppEvent, AppKind, TextInputEvent};
use crate::native_editor::EditorCommand;
use crate::owner_keys_save::fault::{self, Fault};
use crate::owner_keys_save::{Provenance, Unvouched};
use crate::{App, WindowId};

const WID: WindowId = WindowId(0);

/// What the scratch aterm.toml holds on disk before any save.
const DISK: &str = "font_px = 12.0\n";

/// The mode turned up: an owner key.
const PROMPT: &str = "[harness]\noccupancy = \"prompt\"\n";

/// A headless App whose Manual is bound to `dir`'s aterm.toml, which holds [`DISK`]; its
/// journals are kept in `dir` too, so a second App on the same `dir` recovers a draft.
fn manual(dir: &Path) -> (App, PathBuf, DocumentId) {
    let path = dir.join("aterm.toml");
    if !path.exists() {
        crate::test_scratch::write_config(&path, DISK);
    }
    let mut app = App::headless_for_test();
    app.native_documents.journals = JournalSlot::for_test(dir.join("drafts"));
    app.ensure_and_open_config_editor_path_in_window(WID, &path)
        .unwrap();
    let document = front_document(&app);
    (app, path, document)
}

fn front_document(app: &App) -> DocumentId {
    let (instance, _) = app.active_native_view(WID).unwrap();
    app.native_runtime.document_id(instance).unwrap()
}

/// Type `text` at the end of the front buffer, as a control client when `controller`.
fn append(app: &mut App, controller: bool, text: &str) {
    app.with_gesture_source(controller, |app| {
        app.dispatch_native_event(WID, AppEvent::EditorCommand(EditorCommand::EndOfBuffer))
            .unwrap();
        app.dispatch_native_event(
            WID,
            AppEvent::TextInput(TextInputEvent::Commit(text.to_string())),
        )
        .unwrap();
    });
}

/// The front buffer's Save, pressed by a control client when `controller`.
fn save(app: &mut App, controller: bool) -> Result<(), String> {
    app.with_gesture_source(controller, |app| {
        app.dispatch_native_event(WID, AppEvent::EditorCommand(EditorCommand::Save))
            .map(|_| ())
    })
}

fn on_disk(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

/// The refusal the fence words for `provenance` over `text` against [`DISK`], so a test tells
/// the fence's refusal from any other save error (Manual's own checks among them).
fn fenced(provenance: Provenance, app: &App, document: DocumentId) -> String {
    fenced_against(provenance, app, document, Ok(DISK))
}

/// [`fenced`] against `disk`: the file in force's text as the fence reads it, or `Err` where it
/// cannot be read.
fn fenced_against(
    provenance: Provenance,
    app: &App,
    document: DocumentId,
    disk: Result<&str, ()>,
) -> String {
    let saved = app.document_store.snapshot(document).unwrap().text;
    provenance
        .refusal(&saved, disk)
        .expect("the fence refuses this save")
}

/// The aterm.toml in force at `in_force` as the fence reads it: empty where no file is there.
fn disk_as_fenced(in_force: &Path) -> Result<String, ()> {
    crate::native_config_service::VersionedConfigService::observe_path(in_force, true)
        .map(|observation| observation.text)
        .map_err(|_| ())
}

/// A control client's `edit <path> create=1 wait=0` (`amacs --no-wait`) from a terminal tab of
/// its own, for a file that does not exist yet: the Editor view it opens is the front one.
fn edit_create(app: &mut App, path: &Path) {
    app.serve_edits_headless_for_test();
    let session = app.next_session_id;
    app.push_stub_tab(WID, crate::stub_session(session));
    app.next_session_id = session + 1;
    let uri = crate::native_document_host::path_to_file_uri(path).unwrap();
    let (reply, answer) = std::sync::mpsc::channel();
    app.begin_edit(crate::control_edit::EditCall {
        ticket: crate::control_edit::mint_ticket(),
        session,
        request: amacs::protocol::edit::EditRequest::parse(&format!("{uri} create=1 wait=0"))
            .unwrap(),
        real_uri: crate::control_edit::real_file_uri(&uri),
        reply,
    });
    let line = answer.try_recv().expect("an inline edit answers at once");
    assert!(line.starts_with("OK "), "{line}");
    let front = front_document(app);
    assert_eq!(
        app.document_store.snapshot(front).unwrap().text.as_ref(),
        "",
        "the missing file opened empty in the front view"
    );
    assert!(!path.exists(), "opening creates nothing");
}

/// A CONTROL CLIENT'S SAVE KEEPS THE OWNER'S KEYS: a person's `occupancy = "prompt"` saved by a
/// control client is refused, the file unchanged, and the person's own Save writes it.
/// NEGATIVE CONTROL: with the save's gesture ignored (`SaveIgnoresSource`), the control
/// client's save lands.
#[test]
fn a_control_clients_save_keeps_the_owners_keys_and_a_persons_save_writes_them() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let (mut app, path, document) = manual(dir.path());
    append(&mut app, false, PROMPT);
    let refused = save(&mut app, true).unwrap_err();
    assert_eq!(refused, fenced(Provenance::new(true, None), &app, document));
    assert_eq!(on_disk(&path), DISK, "the refused save wrote nothing");
    assert_eq!(app.document_store.dirty(document), Some(true));
    save(&mut app, false).expect("the person's own Save");
    assert_eq!(on_disk(&path), format!("{DISK}{PROMPT}"));

    let control = aterm_tempfile::tempdir().unwrap();
    let (mut app, path, _) = manual(control.path());
    append(&mut app, false, PROMPT);
    fault::with(Fault::SaveIgnoresSource, || save(&mut app, true))
        .expect("the planted fault lets the control client's save through");
    assert_eq!(on_disk(&path), format!("{DISK}{PROMPT}"));
}

/// The owner's rule and source tables are the owner's too: a control client's save that adds
/// either is refused by the fence itself, whatever Manual would say of the table.
#[test]
fn a_control_client_saves_no_owner_rule_or_source_table() {
    for table in [
        "[[harness.occupancy_rule]]\nname = \"trust-base\"\n",
        "[harness.occupancy_source.trust]\nkind = \"git-ref\"\n",
    ] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let (mut app, path, document) = manual(dir.path());
        append(&mut app, false, table);
        let refused = save(&mut app, true).unwrap_err();
        assert_eq!(
            refused,
            fenced(Provenance::new(true, None), &app, document),
            "{table}"
        );
        assert_eq!(on_disk(&path), DISK, "{table}");
    }
}

/// Everything else a control client edits and saves goes on as before: a theme and a
/// `[harness]` key that is not the owner's.
#[test]
fn a_control_client_saves_every_other_key() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let (mut app, path, _) = manual(dir.path());
    let edit = "window_theme = \"dark\"\n[harness]\nfacts = false\n";
    append(&mut app, true, edit);
    save(&mut app, true).expect("no owner key changed");
    assert_eq!(on_disk(&path), format!("{DISK}{edit}"));
}

/// A CONTROL CLIENT'S EDIT IS NOT SAVED BY A PERSON'S SAVE OR CLOSE: the person's Save of a
/// buffer a control client turned `occupancy` up in is refused, and so is the save the tab's
/// close makes; Discard Changes and Reload from Disk gives the person the buffer back, whose
/// own edit then saves.
/// NEGATIVE CONTROL: with the buffer's edits ignored (`SaveIgnoresEdits`), the person's Save
/// writes the control client's line, and so does the close alone.
#[test]
fn a_control_clients_edit_reaches_the_file_by_no_persons_save_or_close() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let (mut app, path, document) = manual(dir.path());
    append(&mut app, true, PROMPT);
    let refused = save(&mut app, false).unwrap_err();
    assert_eq!(
        refused,
        fenced(
            Provenance::new(false, Some(Unvouched::Controller)),
            &app,
            document
        )
    );
    let (_, view) = app.active_native_view(WID).unwrap();
    assert!(
        app.close_editor_view(view).is_err(),
        "the close's save is refused, so the tab stays"
    );
    assert_eq!(on_disk(&path), DISK, "neither the Save nor the close wrote");

    let again = aterm_tempfile::tempdir().unwrap();
    let (mut app, path, document) = manual(again.path());
    append(&mut app, true, PROMPT);
    let (_, view) = app.active_native_view(WID).unwrap();
    app.discard_document_changes(document, view).unwrap();
    assert_eq!(
        app.document_store.snapshot(document).unwrap().text.as_ref(),
        DISK
    );
    append(&mut app, false, PROMPT);
    save(&mut app, false).expect("the person's own edit, after the discard");
    assert_eq!(on_disk(&path), format!("{DISK}{PROMPT}"));

    for close in [false, true] {
        let control = aterm_tempfile::tempdir().unwrap();
        let (mut app, path, _) = manual(control.path());
        append(&mut app, true, PROMPT);
        fault::with(Fault::SaveIgnoresEdits, || {
            if close {
                let (_, view) = app.active_native_view(WID).unwrap();
                let _closed = app.close_editor_view(view);
            } else {
                save(&mut app, false).expect("the planted fault saves the control client's edit");
            }
        });
        assert_eq!(
            on_disk(&path),
            format!("{DISK}{PROMPT}"),
            "close={close}: the planted fault lets the control client's line through"
        );
    }
}

/// A DRAFT RECOVERED FROM A JOURNAL IS NOBODY'S HERE: a buffer whose `occupancy = "prompt"` a
/// journal brought back when the document reopened (a crash, a seamless update's successor)
/// is not saved by a person's Save.
/// NEGATIVE CONTROL: with the buffer's edits ignored (`SaveIgnoresEdits`), it is.
#[test]
fn a_recovered_drafts_owner_keys_are_saved_by_no_persons_save() {
    for planted in [false, true] {
        let dir = aterm_tempfile::tempdir().unwrap();
        {
            let (mut crashed, path, _) = manual(dir.path());
            append(&mut crashed, true, PROMPT);
            assert_eq!(on_disk(&path), DISK);
        }
        let (mut reopened, path, document) = manual(dir.path());
        assert_eq!(
            reopened
                .document_store
                .snapshot(document)
                .unwrap()
                .text
                .as_ref(),
            format!("{DISK}{PROMPT}"),
            "the journal's draft came back"
        );
        if planted {
            fault::with(Fault::SaveIgnoresEdits, || save(&mut reopened, false))
                .expect("the planted fault saves the recovered draft");
            assert_eq!(on_disk(&path), format!("{DISK}{PROMPT}"));
        } else {
            let refused = save(&mut reopened, false).unwrap_err();
            assert_eq!(
                refused,
                fenced(
                    Provenance::new(false, Some(Unvouched::Recovered)),
                    &reopened,
                    document
                )
            );
            assert_eq!(on_disk(&path), DISK);
        }
    }
}

/// A SAVE QUEUED BEHIND ONE IN FLIGHT IS REPLAYED FOR WHOEVER ASKED: a control client's Save of
/// the person's note, asked while the person's theme edit is being written, is replayed as the
/// control client's when that write lands, so the `occupancy = "prompt"` the person typed in
/// between waits for the person's own Save; so is an accessibility client's Save; a person's
/// Save merged into the queue makes the replay the person's.
/// NEGATIVE CONTROL: with the queue forgetting who asked (`QueueDropsSource`), the control
/// client's replay writes it.
#[test]
fn a_queued_save_is_replayed_for_whoever_asked() {
    const THEME: &str = "window_theme = \"dark\"\n";
    const NOTE: &str = "# the person's note\n";
    for (asker, person_too, planted) in [
        (Unvouched::Controller, false, false),
        (Unvouched::Controller, true, false),
        (Unvouched::Controller, false, true),
        (Unvouched::Assistive, false, false),
    ] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let (mut app, path, document) = manual(dir.path());
        let (_, view) = app.active_native_view(WID).unwrap();
        append(&mut app, false, THEME);
        let held = app.hold_document_save_for_test(document);
        // An edit past the save in flight, so the queued save has something to replay.
        append(&mut app, false, NOTE);
        let queue = |app: &mut App| {
            match asker {
                Unvouched::Assistive => app.with_assistive_source(|app| save(app, false)),
                _ => save(app, true),
            }
            .expect("queued behind the save in flight");
        };
        if planted {
            fault::with(Fault::QueueDropsSource, || queue(&mut app));
        } else {
            queue(&mut app);
        }
        append(&mut app, false, PROMPT);
        if person_too {
            save(&mut app, false).expect("the person's Save, queued too");
        }
        let landed = app.land_held_document_save_for_test(document, view, held);
        let case = format!("asker={asker:?} person_too={person_too} planted={planted}");
        if person_too || planted {
            landed.expect(&case);
            assert_eq!(
                on_disk(&path),
                format!("{DISK}{THEME}{NOTE}{PROMPT}"),
                "{case}"
            );
        } else {
            assert_eq!(
                landed,
                Err(fenced(Provenance::new(true, None), &app, document)),
                "{case}"
            );
            assert_eq!(on_disk(&path), format!("{DISK}{THEME}"), "{case}");
        }
    }
}

/// The fence is the aterm.toml in force's alone: another file with the same text saves from a
/// control client (the NEGATIVE CONTROL of the path test), while a hard link to the one in
/// force, the same file by another name, is fenced.
#[test]
fn only_the_aterm_toml_in_force_is_fenced() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let (mut app, path, _) = manual(dir.path());
    let other = dir.path().join("other.toml");
    crate::test_scratch::write_config(&other, DISK);
    app.open_document_tab(
        AppKind::Editor,
        &crate::native_document_host::path_to_file_uri(&other).unwrap(),
    )
    .unwrap();
    append(&mut app, true, PROMPT);
    save(&mut app, true).expect("not the aterm.toml in force");
    assert_eq!(on_disk(&other), format!("{DISK}{PROMPT}"));
    assert_eq!(on_disk(&path), DISK);

    #[cfg(unix)]
    {
        let linked = dir.path().join("linked.toml");
        std::fs::hard_link(&path, &linked).unwrap();
        app.open_document_tab(
            AppKind::Editor,
            &crate::native_document_host::path_to_file_uri(&linked).unwrap(),
        )
        .unwrap();
        let document = front_document(&app);
        append(&mut app, true, PROMPT);
        let refused = save(&mut app, true).unwrap_err();
        assert_eq!(
            refused,
            fenced(
                Provenance::new(true, Some(Unvouched::Controller)),
                &app,
                document
            )
        );
        assert_eq!(on_disk(&linked), DISK);
    }
}

/// The decision alone: nothing is fenced for a person's save of a buffer a person typed, and a
/// fenced save whose file cannot be read is refused.
#[test]
fn the_fence_decides_from_the_saves_provenance_and_both_texts() {
    let prompt = format!("{DISK}{PROMPT}");
    assert!(!Provenance::new(false, None).fenced());
    assert_eq!(
        Provenance::new(false, None).refusal(&prompt, Ok(DISK)),
        None
    );
    for provenance in [
        Provenance::new(true, None),
        Provenance::new(false, Some(Unvouched::Controller)),
        Provenance::new(false, Some(Unvouched::Assistive)),
        Provenance::new(false, Some(Unvouched::Recovered)),
    ] {
        assert!(provenance.fenced(), "{provenance:?}");
        assert!(
            provenance.refusal(&prompt, Ok(DISK)).is_some(),
            "{provenance:?}"
        );
        assert_eq!(provenance.refusal(DISK, Ok(DISK)), None, "{provenance:?}");
        assert!(
            provenance.refusal(DISK, Err(())).is_some(),
            "{provenance:?}"
        );
    }
}

/// WHILE THE ATERM.TOML IN FORCE IS MISSING, NO CONTROL CLIENT'S SAVE MAKES IT UNDER ANOTHER
/// NAME: `edit …/Aterm.toml create=1` beside the missing aterm.toml (a volume that folds case
/// reads one as the other), and `edit` of the missing file a dangling aterm.toml link names (a
/// dotfile manager's), each typed `occupancy = "prompt"` into and saved by a control client, are
/// refused and make nothing.
/// NEGATIVE CONTROL: with the folder not looked at (`CreateIgnoresFolder`), each save makes its
/// file.
#[test]
fn a_missing_aterm_toml_is_made_by_no_control_clients_save_under_another_name() {
    for planted in [false, true] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let (mut app, in_force, _) = manual(dir.path());
        std::fs::remove_file(&in_force).unwrap();
        made_by_no_control_clients_save(
            &mut app,
            &in_force,
            &dir.path().join("Aterm.toml"),
            planted,
        );

        #[cfg(unix)]
        {
            let dir = aterm_tempfile::tempdir().unwrap();
            let (mut app, in_force, _) = manual(dir.path());
            let dotfiles = dir.path().join("dotfiles");
            std::fs::create_dir(&dotfiles).unwrap();
            std::fs::remove_file(&in_force).unwrap();
            std::os::unix::fs::symlink(dotfiles.join("aterm.toml"), &in_force).unwrap();
            made_by_no_control_clients_save(
                &mut app,
                &in_force,
                &dotfiles.join("aterm.toml"),
                planted,
            );
        }
    }
}

/// A control client opens `path` by `edit … create=1`, types `occupancy = "prompt"` and saves,
/// while `in_force` names no file: refused and nothing made, or, with `planted`
/// (`CreateIgnoresFolder`), made.
fn made_by_no_control_clients_save(app: &mut App, in_force: &Path, path: &Path, planted: bool) {
    edit_create(app, path);
    let document = front_document(app);
    append(app, true, PROMPT);
    let case = format!("planted={planted} {}", path.display());
    if planted {
        fault::with(Fault::CreateIgnoresFolder, || save(app, true)).expect(&case);
        assert_eq!(on_disk(path), PROMPT, "{case}");
    } else {
        let disk = disk_as_fenced(in_force);
        assert_eq!(
            save(app, true),
            Err(fenced_against(
                Provenance::new(true, Some(Unvouched::Controller)),
                app,
                document,
                disk.as_deref().map_err(|_| ()),
            )),
            "{case}"
        );
        assert!(!path.exists(), "{case}: nothing was made");
        assert!(
            std::fs::metadata(in_force).is_err(),
            "{case}: the aterm.toml in force still names no file"
        );
    }
}

/// THE PATH ALONE FENCES: the aterm.toml in force and its folder both gone, a control client's
/// save of the buffer Manual still holds is refused by the fence itself, before any write could
/// meet a folder made since (Settings' first write makes one).
/// NEGATIVE CONTROL: with the path not compared (`PathIgnored`), the save goes on to the
/// writer, whose refusal is not the fence's.
#[test]
fn the_aterm_toml_in_force_is_fenced_by_its_path_where_nothing_is_there() {
    for planted in [false, true] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let folder = dir.path().join("aterm");
        std::fs::create_dir(&folder).unwrap();
        let (mut app, _, document) = manual(&folder);
        append(&mut app, true, PROMPT);
        // The folder goes with the journal Manual keeps in it.
        std::fs::remove_dir_all(&folder).unwrap();
        let fence = fenced_against(
            Provenance::new(true, Some(Unvouched::Controller)),
            &app,
            document,
            Ok(""),
        );
        if planted {
            let saved = fault::with(Fault::PathIgnored, || save(&mut app, true));
            assert!(
                saved.as_ref().is_err_and(|refused| *refused != fence),
                "{saved:?}"
            );
        } else {
            assert_eq!(save(&mut app, true), Err(fence));
        }
        assert!(!folder.exists(), "planted={planted}");
    }
}

/// WHERE THE CONFIG SERVICE BOUND NO aterm.toml (its load failed at launch), THE ONE THE WINDOW
/// LOADS BY IS THE ONE IN FORCE: a control client's save of it, opened as any document, is
/// refused.
/// NEGATIVE CONTROL: with no fallback named, the same save writes.
#[test]
fn the_aterm_toml_the_window_loads_by_is_fenced_where_no_service_bound_one() {
    for named in [true, false] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join("aterm.toml");
        crate::test_scratch::write_config(&path, DISK);
        let mut app = App::headless_for_test();
        assert_eq!(app.native_config_service.bound_logical_path(), None);
        app.open_document_tab(
            AppKind::Editor,
            &crate::native_document_host::path_to_file_uri(&path).unwrap(),
        )
        .unwrap();
        let document = front_document(&app);
        append(&mut app, true, PROMPT);
        if named {
            let saved = crate::owner_keys_save::unbound::with(&path, || save(&mut app, true));
            assert_eq!(
                saved,
                Err(fenced(
                    Provenance::new(true, Some(Unvouched::Controller)),
                    &app,
                    document
                ))
            );
            assert_eq!(on_disk(&path), DISK);
        } else {
            save(&mut app, true).expect("no aterm.toml in force is known");
            assert_eq!(on_disk(&path), format!("{DISK}{PROMPT}"));
        }
    }
}

/// A FENCED SAVE OF AN aterm.toml THE WINDOW CANNOT READ IS REFUSED, even one that drops every
/// owner key: the fence cannot tell what it would drop.
/// NEGATIVE CONTROL: the same save against a file it can read writes.
#[test]
fn a_fenced_save_over_an_unreadable_aterm_toml_is_refused() {
    const NOTE: &str = "# a note\n";
    for readable in [false, true] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let (mut app, path, document) = manual(dir.path());
        append(&mut app, true, NOTE);
        if readable {
            save(&mut app, true).expect("no owner key changed");
            assert_eq!(on_disk(&path), format!("{DISK}{NOTE}"));
        } else {
            let unreadable: &[u8] = b"[harness]\noccupancy = \"prompt\"\n\xff\n";
            crate::test_scratch::write_config(&path, unreadable);
            assert_eq!(disk_as_fenced(&path), Err(()), "not UTF-8");
            assert_eq!(
                save(&mut app, true),
                Err(fenced_against(
                    Provenance::new(true, Some(Unvouched::Controller)),
                    &app,
                    document,
                    Err(()),
                ))
            );
            assert_eq!(std::fs::read(&path).unwrap(), unreadable);
        }
    }
}

/// A DISCARD STARTS THE UNDO OF A BUFFER A CONTROL CLIENT EDITED: after the person's Discard
/// Changes and Reload from Disk, neither the person's `C-/` (of the revert) nor redo (of the
/// control client's undone edit, which a discard that changed nothing leaves) puts
/// `occupancy = "prompt"` back for the person's Save to write.
/// NEGATIVE CONTROL: with the history kept (`DiscardKeepsHistory`), each brings it back
/// unmarked, and the person's Save writes it.
#[test]
fn a_discard_leaves_no_control_clients_edit_for_undo_or_redo_to_bring_back() {
    for planted in [false, true] {
        for command in [EditorCommand::Undo, EditorCommand::Redo] {
            let dir = aterm_tempfile::tempdir().unwrap();
            let (mut app, path, document) = manual(dir.path());
            let (_, view) = app.active_native_view(WID).unwrap();
            append(&mut app, true, PROMPT);
            if command == EditorCommand::Redo {
                app.with_gesture_source(true, |app| {
                    app.dispatch_native_event(WID, AppEvent::EditorCommand(EditorCommand::Undo))
                        .unwrap();
                });
                assert_eq!(
                    app.document_store.snapshot(document).unwrap().text.as_ref(),
                    DISK
                );
            }
            let discard = |app: &mut App| app.discard_document_changes(document, view).unwrap();
            if planted {
                fault::with(Fault::DiscardKeepsHistory, || discard(&mut app));
            } else {
                discard(&mut app);
            }
            app.dispatch_native_event(WID, AppEvent::EditorCommand(command))
                .unwrap();
            let case = format!("planted={planted} {command:?}");
            let text = app.document_store.snapshot(document).unwrap().text;
            if planted {
                assert_eq!(text.as_ref(), format!("{DISK}{PROMPT}"), "{case}");
                save(&mut app, false).expect(&case);
                assert_eq!(on_disk(&path), format!("{DISK}{PROMPT}"), "{case}");
            } else {
                assert_eq!(text.as_ref(), DISK, "{case}: nothing came back");
                let _clean = save(&mut app, false);
                assert_eq!(on_disk(&path), DISK, "{case}");
            }
        }
    }
}

/// AN ACCESSIBILITY CLIENT'S EDIT IS NOT SAVED BY A PERSON'S SAVE: `occupancy = "prompt"` put
/// in the buffer through the window's accessibility tree (`setAccessibilitySelectedText:`, which
/// a tab's program sends with aterm's own Accessibility grant, as a screen reader does) is
/// refused by the person's Save, the file unchanged, until Discard Changes and Reload from Disk
/// gives the person the buffer back.
/// NEGATIVE CONTROL: the same insertion as the person's own gesture saves.
#[cfg(a11y_tree)]
#[test]
fn an_accessibility_clients_edit_reaches_the_file_by_no_persons_save() {
    for assistive in [true, false] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let (mut app, path, document) = manual(dir.path());
        let (_, view) = app.active_native_view(WID).unwrap();
        app.dispatch_native_event(WID, AppEvent::EditorCommand(EditorCommand::EndOfBuffer))
            .unwrap();
        if assistive {
            let request = app.accessibility_request_for_test(
                WID,
                view,
                "editor/buffer",
                aterm_a11y::model::Action::ReplaceSelectedText,
                Some(aterm_a11y::model::ActionData::Value(PROMPT.into())),
            );
            app.on_accessibility_action(WID, request);
        } else {
            app.dispatch_native_event(WID, AppEvent::InsertText(PROMPT.to_string()))
                .unwrap();
        }
        let case = format!("assistive={assistive}");
        assert_eq!(
            app.document_store.snapshot(document).unwrap().text.as_ref(),
            format!("{DISK}{PROMPT}"),
            "{case}: the insertion reached the buffer"
        );
        let saved = save(&mut app, false);
        if assistive {
            assert_eq!(
                saved,
                Err(fenced(
                    Provenance::new(false, Some(Unvouched::Assistive)),
                    &app,
                    document
                )),
                "{case}"
            );
            assert_eq!(
                on_disk(&path),
                DISK,
                "{case}: the refused save wrote nothing"
            );
            app.discard_document_changes(document, view).unwrap();
            append(&mut app, false, PROMPT);
            save(&mut app, false).expect("the person's own edit, after the discard");
        } else {
            saved.expect(&case);
        }
        assert_eq!(on_disk(&path), format!("{DISK}{PROMPT}"), "{case}");
    }
}

/// AN ACCESSIBILITY CLIENT'S SAVE KEEPS THE OWNER'S KEYS: the person's `occupancy = "prompt"`
/// saved by an accessibility client's press of the Editor's Save button is refused, the file
/// unchanged.
/// NEGATIVE CONTROL: the person's own press of the same button writes it.
#[cfg(a11y_tree)]
#[test]
fn an_accessibility_clients_save_keeps_the_owners_keys() {
    const SAVE_BUTTON: &str = "editor/save-button";
    for assistive in [true, false] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let (mut app, path, document) = manual(dir.path());
        let (_, view) = app.active_native_view(WID).unwrap();
        append(&mut app, false, PROMPT);
        let case = format!("assistive={assistive}");
        if assistive {
            let request = app.accessibility_request_for_test(
                WID,
                view,
                SAVE_BUTTON,
                aterm_a11y::model::Action::Click,
                None,
            );
            app.on_accessibility_action(WID, request);
            let status = match app.native_runtime.view_state(view) {
                Some(crate::native_app::AppViewState::Editor(state)) => state.status.clone(),
                _ => panic!("the Editor view is live"),
            };
            assert_eq!(
                status,
                Some(fenced(Provenance::new(true, None), &app, document)),
                "{case}: the fence refused the save"
            );
            assert_eq!(
                on_disk(&path),
                DISK,
                "{case}: the refused save wrote nothing"
            );
            assert_eq!(app.document_store.dirty(document), Some(true), "{case}");
        } else {
            app.dispatch_native_control_action(
                WID,
                view,
                crate::native_ui::UiKey::new(SAVE_BUTTON),
                crate::native_app::ActionInvocation {
                    id: crate::native_ui::ActionId::new("editor/save"),
                    value: None,
                },
                false,
            )
            .expect(&case);
            assert_eq!(on_disk(&path), format!("{DISK}{PROMPT}"), "{case}");
        }
    }
}

/// A PALETTE PASTE AN ACCESSIBILITY CLIENT CHOSE IS NO PERSON'S EDIT: the palette posts its Edit
/// rows to the event loop, so the origin it posts carries the marks of the gesture that chose
/// the row and the row runs under them at the wake ([`App::run_posted_menu_action`]). The
/// clipboard's `occupancy = "prompt"` pasted by a row a palette Click through the window's
/// accessibility tree chose (taken under the mark [`App::on_accessibility_action`] sets) is
/// refused by the person's Save, the file unchanged.
/// NEGATIVE CONTROL: the same row the person chose pastes, and the person's Save writes it.
#[test]
fn a_palette_paste_an_accessibility_client_chose_is_no_persons_edit() {
    use crate::app_take_over::GestureMarks;
    use crate::menu::MenuAction;
    for assistive in [true, false] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let (mut app, path, document) = manual(dir.path());
        app.dispatch_native_event(WID, AppEvent::EditorCommand(EditorCommand::EndOfBuffer))
            .unwrap();
        let origin = if assistive {
            app.with_assistive_source(|app| app.palette_menu_origin(WID))
        } else {
            app.palette_menu_origin(WID)
        }
        .expect("a palette origin");
        let case = format!("assistive={assistive}");
        assert_eq!(
            origin.assistive(),
            assistive,
            "{case}: the chooser's mark rides the post"
        );
        crate::control::PBPASTE_STUB.with(|board| *board.borrow_mut() = Some(PROMPT.to_string()));
        app.run_posted_menu_action(
            MenuAction::Paste,
            Some(origin),
            GestureMarks::default(),
            |app, action| {
                app.fence_menu_action(action);
                if !app.divert_menu_action_around_rename(action) {
                    app.paste_into_front_field();
                }
            },
        );
        crate::control::PBPASTE_STUB.with(|board| *board.borrow_mut() = None);
        assert_eq!(
            app.document_store.snapshot(document).unwrap().text.as_ref(),
            format!("{DISK}{PROMPT}"),
            "{case}: the paste reached the buffer"
        );
        let saved = save(&mut app, false);
        if assistive {
            assert_eq!(
                saved,
                Err(fenced(
                    Provenance::new(false, Some(Unvouched::Assistive)),
                    &app,
                    document
                )),
                "{case}"
            );
            assert_eq!(
                on_disk(&path),
                DISK,
                "{case}: the refused save wrote nothing"
            );
        } else {
            saved.expect(&case);
            assert_eq!(on_disk(&path), format!("{DISK}{PROMPT}"), "{case}");
        }
    }
}
