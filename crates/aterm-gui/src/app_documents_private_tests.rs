// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A private edit's document ([`aterm_types::private_edit`]): no draft journal, an earlier one
//! purged at open, and no closed-tab reopening keeps it in memory; no capture, controller input,
//! `act`, `pointer`, `invoke`, `hwkey`, restore or reopening carries its view (one-mesh design
//! §4.8, §11 A17); `edit … ro=1` opens it read-only. An ordinary document beside it is each test's negative control.

use std::path::{Path, PathBuf};

use crate::document_store::DocumentId;
use crate::native_app::{AppEvent, AppKind, TextInputEvent};
use crate::native_document_io::JournalDocumentKey;
use crate::native_document_journal::{DocumentJournalStore, private_uri};
use crate::{App, WindowId};

const TYPED: &str = "hunter2-typed ";

/// A file in a private edit's folder (`private`) or in an ordinary scratch folder.
struct Fixture {
    dir: aterm_tempfile::TempDir,
    uri: String,
}

fn fixture(private: bool) -> Fixture {
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-private-doc-")
        .tempdir()
        .unwrap();
    let folder = dir.path().join(if private {
        "att-edit-t.private.noindex"
    } else {
        "att-edit-t.noindex"
    });
    std::fs::create_dir(&folder).unwrap();
    // A text file: a `.tsv` opens as a grid, which a typed commit does not edit.
    let path = folder.join("address.txt");
    std::fs::write(&path, "name secret\n").unwrap();
    let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
    Fixture { dir, uri }
}

/// Every file under `root` whose name is a journal's (`.atdj`, its lock, its temporaries).
fn journal_files(root: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(root)
        .map(|listing| {
            listing
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .filter(|name| name.contains(".atdj"))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// Whether any file under `root` holds `needle`.
fn holds(root: &Path, needle: &str) -> bool {
    std::fs::read_dir(root).is_ok_and(|listing| {
        listing.flatten().any(|entry| {
            std::fs::read(entry.path()).is_ok_and(|bytes| {
                bytes
                    .windows(needle.len())
                    .any(|window| window == needle.as_bytes())
            })
        })
    })
}

fn opened(app: &mut App, f: &Fixture) -> (DocumentId, String, PathBuf) {
    app.use_document_journal_root_for_test(f.dir.path().join("drafts"));
    app.open_document_tab(AppKind::Editor, &f.uri).unwrap();
    let (instance, _) = app.active_native_view(WindowId(0)).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();
    let canonical = app
        .document_store
        .canonical_uri(document)
        .unwrap()
        .to_owned();
    (document, canonical, f.dir.path().join("drafts"))
}

#[test]
fn a_private_document_edited_and_closed_leaves_nothing_in_drafts() {
    for private in [true, false] {
        let f = fixture(private);
        let mut app = App::headless_for_test();
        let (document, canonical, drafts) = opened(&mut app, &f);
        assert_eq!(private_uri(&canonical), private);
        app.dispatch_native_event(
            WindowId(0),
            AppEvent::TextInput(TextInputEvent::Commit(TYPED.to_owned())),
        )
        .unwrap();
        assert_eq!(
            app.document_store.dirty(document),
            Some(true),
            "private={private}: {:?}",
            app.document_recovery_status_for_test(document)
        );
        if private {
            assert_eq!(journal_files(&drafts), Vec::<String>::new());
            assert!(!holds(&drafts, TYPED));
        } else {
            // The negative control: an ordinary document's typing is journaled.
            assert!(
                app.native_documents
                    .journals
                    .as_ref()
                    .unwrap()
                    .path_for_uri(&canonical)
                    .exists()
            );
            assert!(!journal_files(&drafts).is_empty());
        }
        app.close_active_native_tab(WindowId(0)).unwrap();
        assert_eq!(app.document_store.view_count(document), Some(0));
        if private {
            assert_eq!(journal_files(&drafts), Vec::<String>::new());
            assert!(!holds(&drafts, TYPED));
        }
        assert_eq!(
            app.release_unheld_documents(0),
            usize::from(private),
            "a closed private document leaves memory at once; an ordinary one waits for its \
             closed tab's reopening (private={private})"
        );
        assert_eq!(app.document_store.id_for_uri(&canonical).is_none(), private);
    }
}

#[test]
fn declining_a_journal_purges_what_an_earlier_open_left_and_makes_its_writes_no_ops() {
    let private = fixture(true);
    let plain = fixture(false);
    let root = private.dir.path().join("drafts");
    let mut documents = crate::test_document_store();
    let mut journals = DocumentJournalStore::for_test(root.clone()).unwrap();
    let mut ids = Vec::new();
    for f in [&private, &plain] {
        let document = documents.open(f.uri.clone(), "name secret\n".to_owned());
        let disk = documents.snapshot(document).unwrap();
        let decision = journals.inspect_open(&f.uri, disk.text.as_bytes()).unwrap();
        journals.initialize(decision, &disk, &disk).unwrap();
        ids.push(document);
    }
    let (secret, ordinary) = (ids[0], ids[1]);
    let keyed = journals.path_for_uri(&private.uri);
    let kept = journals.path_for_uri(&plain.uri);
    assert!(
        keyed.exists() && kept.exists(),
        "an earlier open's journals"
    );
    // What a crash or an older build leaves of the private document's journals.
    let name = keyed.file_name().unwrap().to_str().unwrap().to_owned();
    let stem = keyed.file_stem().unwrap().to_str().unwrap().to_owned();
    let legacy_stem = format!(
        "{:016x}",
        JournalDocumentKey::legacy_unkeyed(&private.uri).0
    );
    let leftovers = [
        root.join(format!("{legacy_stem}.atdj")),
        root.join(format!(".{legacy_stem}.atdj.lock")),
        root.join(format!("{legacy_stem}.preserved-2.atdj")),
        root.join(format!("{stem}.preserved-1.atdj")),
        root.join(format!(".{name}.lock")),
        root.join(format!(".{name}.77-ab.tmp")),
    ];
    for leftover in &leftovers {
        std::fs::write(leftover, TYPED).unwrap();
    }
    // The negative controls: the ordinary document's preserved copy, and a name that only
    // starts like the private one's.
    let kept_name = kept.file_stem().unwrap().to_str().unwrap().to_owned();
    let others = [
        root.join(format!("{kept_name}.preserved-1.atdj")),
        root.join(format!("{stem}.preserved-x.atdj")),
    ];
    for other in &others {
        std::fs::write(other, "kept").unwrap();
    }

    journals.decline(secret, &private.uri).unwrap();
    assert!(journals.declined(secret));
    assert!(!keyed.exists());
    for leftover in &leftovers {
        assert!(!leftover.exists(), "{}", leftover.display());
    }
    assert!(kept.exists());
    for other in &others {
        assert!(other.exists(), "{}", other.display());
    }
    assert!(!holds(&root, TYPED));

    for document in [secret, ordinary] {
        let head = documents.snapshot(document).unwrap();
        let _ = documents.transact(
            document,
            head.seq,
            vec![crate::document_store::TextEdit {
                range: 0..0,
                insert: TYPED.to_owned(),
            }],
        );
        assert!(documents.snapshot(document).unwrap().seq > head.seq);
        journals
            .observe_commit(&documents.snapshot(document).unwrap())
            .unwrap();
    }
    assert!(journals.next_effect(secret).unwrap().is_none());
    assert!(journals.settled(secret));
    assert!(
        journals.next_effect(ordinary).unwrap().is_some(),
        "the ordinary document's commit owes its journal an append"
    );
    assert!(!keyed.exists());

    journals.forget(&[secret]);
    assert!(!journals.declined(secret));
}

#[test]
fn inspect_and_the_visible_text_withhold_a_private_document_s_text() {
    use aterm_types::app_inspection::InspectionProjection;
    for private in [true, false] {
        let f = fixture(private);
        let mut app = App::headless_for_test();
        let (_, canonical, _) = opened(&mut app, &f);
        assert_eq!(private_uri(&canonical), private);
        let (_, view) = app.active_native_view(WindowId(0)).unwrap();
        assert_eq!(app.view_is_private(view), private);
        let mut served = String::new();
        for projection in [
            InspectionProjection::Text,
            InspectionProjection::Controls,
            InspectionProjection::Tree,
            InspectionProjection::Audit,
        ] {
            let lines = app
                .inspect_app(crate::app_control::InspectRequest::View { view, projection })
                .unwrap();
            assert_eq!(
                lines
                    .iter()
                    .any(|line| line == crate::app_control::PRIVATE_VIEW_WITHHELD),
                private,
                "{projection:?}"
            );
            served.push_str(&lines.join("\n"));
        }
        assert!(app.prepare_native_input_scratch(WindowId(0)));
        let input = app.windows[&WindowId(0)].input_scratch.clone();
        served.push_str(&app.snapshot_visible_text(
            WindowId(0),
            app.active_visible_content_route(WindowId(0)).unwrap(),
            &app.active_visible_leaf_plan(WindowId(0)).unwrap(),
            &input,
            usize::from(app.tab_strip_rows),
            usize::from(app.windows[&WindowId(0)].cols),
        ));
        // The negative control: an ordinary document's text is served.
        assert_eq!(served.contains("secret"), !private, "{served}");
    }
}

/// A front terminal tab in window 0 with a session id no other test takes, as `edit` needs.
pub(super) fn caller(app: &mut App) -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(5 << 40);
    let sid = NEXT.fetch_add(1 << 12, Ordering::Relaxed);
    app.next_session_id = sid;
    app.push_stub_tab(WindowId(0), crate::stub_session(sid));
    app.next_session_id = sid + 1;
    sid
}

/// `@<session> edit <uri> [ro=1]`, answered inline: its reply line.
fn edit(app: &mut App, session: u64, uri: &str, read_only: bool) -> String {
    let (reply, answer) = std::sync::mpsc::channel();
    app.begin_edit(crate::control_edit::EditCall {
        ticket: crate::control_edit::mint_ticket(),
        session,
        real_uri: crate::control_edit::real_file_uri(uri),
        request: amacs::protocol::edit::EditRequest {
            uri: uri.to_owned(),
            line: None,
            create: false,
            for_word: None,
            wait: true,
            read_only,
            secret: None,
        },
        reply,
    });
    answer.try_recv().expect("an inline edit answers at once")
}

/// `ro=1` opens the file read-only: typing changes nothing and the document says so; a file a
/// buffer already holds is refused rather than made read-only under its reader.
/// NEGATIVE CONTROL: with `install_admitted_document` ignoring the ticket's `read_only`, the
/// document opens writable and the test fails at its `read_only` check.
#[test]
fn a_read_only_edit_refuses_typing_and_a_buffer_already_open() {
    for read_only in [true, false] {
        let f = fixture(true);
        let mut app = App::headless_for_test();
        app.use_document_journal_root_for_test(f.dir.path().join("drafts"));
        app.serve_edits_headless_for_test();
        let session = caller(&mut app);
        let reply = edit(&mut app, session, &f.uri, read_only);
        assert!(reply.starts_with("OK edit "), "{reply}");
        let (instance, _) = app.active_native_view(WindowId(0)).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        assert_eq!(app.document_store.read_only(document), read_only);
        app.dispatch_native_event(
            WindowId(0),
            AppEvent::TextInput(TextInputEvent::Commit(TYPED.to_owned())),
        )
        .unwrap();
        assert_eq!(
            app.document_store.dirty(document),
            Some(!read_only),
            "ro=1 refuses typing (read_only={read_only})"
        );
        assert_eq!(
            app.document_store
                .snapshot(document)
                .unwrap()
                .text
                .contains(TYPED),
            !read_only
        );
    }
    // A buffer already open: `ro=1` is refused, the ordinary edit opens a second view of it.
    for read_only in [true, false] {
        let f = fixture(true);
        let mut app = App::headless_for_test();
        let _ = opened(&mut app, &f);
        app.serve_edits_headless_for_test();
        let session = caller(&mut app);
        let reply = edit(&mut app, session, &f.uri, read_only);
        assert_eq!(
            reply.starts_with("ERR refused open: a read-only edit opens a file no buffer holds"),
            read_only,
            "{reply}"
        );
    }
}

/// While a private document's view is the front tab's content, `image` and `video` refuse,
/// controller input other than geometry and focus is refused at delivery, and the bare-verb
/// surface names it; a view pinned to a session is not the front's.
/// NEGATIVE CONTROL: with `resolve_capture_source_at` skipping its private-view check, the
/// private front's image is taken: `` image of a private front ``.
#[test]
fn a_private_front_refuses_image_video_and_controller_input() {
    use crate::app_control::PRIVATE_VIEW_REFUSED;
    use crate::input::InputEvent;
    for private in [true, false] {
        let f = fixture(private);
        let mut app = App::headless_for_test();
        let _ = opened(&mut app, &f);
        let wid = WindowId(0);
        assert_eq!(app.window_shows_private(wid), private);
        assert_eq!(
            app.front_control_surface(),
            if private {
                crate::control::FrontControlSurface::PrivateDocument
            } else {
                crate::control::FrontControlSurface::OtherNative
            }
        );
        assert_eq!(
            app.video_request_refusal(),
            private.then_some(PRIVATE_VIEW_REFUSED)
        );
        let paste = InputEvent::Paste(TYPED.to_owned(), crate::input::PasteFraming::AtDrain);
        let text = InputEvent::Text(TYPED.to_owned());
        for event in [&paste, &text] {
            assert_eq!(
                app.controller_input_refusal(wid, None, event),
                private.then_some(PRIVATE_VIEW_REFUSED)
            );
            assert_eq!(app.controller_input_refusal(wid, Some(7), event), None);
        }
        assert_eq!(
            app.controller_input_refusal(wid, None, &InputEvent::Focus(true)),
            None
        );
        let (reply, answer) = std::sync::mpsc::channel();
        app.render_image(crate::control::ImageReq {
            handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
            target: crate::control_auth::ConfinedImage::in_memory(),
            clean: false,
            session: None,
            want_bytes: true,
            want_metadata: false,
            frame_metadata: std::sync::Arc::new(std::sync::OnceLock::new()),
            cancel: crate::control::CaptureCancellation::new(),
            reply,
        });
        let image = answer
            .recv_timeout(std::time::Duration::from_secs(60))
            .expect("an image reply");
        match image {
            Err(error) => assert!(private, "image of a private front: {error}"),
            Ok(_) => assert!(!private, "image of a private front"),
        }
    }
}

/// `act` on a private document's view, and `pointer` and `invoke` while it shows in the front
/// tab, are refused; `hwkey` is refused while any tab of the window holds it, in front or not.
/// NEGATIVE CONTROL: with `window_holds_private` answering `false`, `hwkey` is let through:
/// `` assertion `left == right` failed: hwkey left: None right: Some("refused private view: the
/// front window holds a private document") ``.
#[test]
fn act_pointer_invoke_and_hwkey_refuse_a_private_document() {
    use crate::app_control::{ActRequest, PRIVATE_VIEW_REFUSED, PRIVATE_WINDOW_REFUSED};
    for private in [true, false] {
        let f = fixture(private);
        let mut app = App::headless_for_test();
        app.use_document_journal_root_for_test(f.dir.path().join("drafts"));
        app.serve_edits_headless_for_test();
        let wid = WindowId(0);
        let session = caller(&mut app);
        let callers_tab = app.windows[&wid].tab_set.active_index().unwrap();
        let reply = edit(&mut app, session, &f.uri, true);
        assert!(reply.starts_with("OK edit "), "{reply}");
        let editor_tab = app.windows[&wid].tab_set.active_index().unwrap();
        let (_, view) = app.active_native_view(wid).unwrap();
        let in_view = private.then_some(PRIVATE_VIEW_REFUSED);
        let in_window = private.then_some(PRIVATE_WINDOW_REFUSED);

        assert_eq!(app.invoke_private_refusal(), in_view, "invoke");
        assert_eq!(app.hwkey_front_refusal(), in_window, "hwkey");
        let pointer = app.pointer_cmd(crate::app_mouse::PointerAction::Status);
        assert_eq!(
            pointer.as_ref().err().map(String::as_str) == Some(PRIVATE_VIEW_REFUSED),
            private,
            "pointer: {pointer:?}"
        );

        // The caller's own tab in front: the Editor tab is out of view, still in the window.
        app.switch_tab_in(wid, callers_tab);
        assert_eq!(
            app.invoke_private_refusal(),
            None,
            "invoke, caller in front"
        );
        assert_eq!(app.hwkey_front_refusal(), in_window, "hwkey");
        let pointer = app.pointer_cmd(crate::app_mouse::PointerAction::Status);
        assert_ne!(
            pointer.as_ref().err().map(String::as_str),
            Some(PRIVATE_VIEW_REFUSED),
            "pointer, caller in front"
        );

        app.switch_tab_in(wid, editor_tab);
        let act = app.act_app(ActRequest {
            view,
            ui_key: "editor/finish".to_string(),
            action: "editor/finish".to_string(),
            value: None,
        });
        if private {
            assert_eq!(act, Err(PRIVATE_VIEW_REFUSED.to_owned()));
        } else {
            assert!(act.is_ok(), "act on an ordinary edit: {act:?}");
        }
    }
}

/// A private document's tab is neither restored nor kept for reopening; in a split its sibling
/// takes the split's place and the focus moves there.
/// NEGATIVE CONTROL: with `tab_restore_descriptor_carrying` describing private leaves, the
/// single-leaf private tab is described: `` restored private tab ``.
#[test]
fn a_private_document_tab_is_neither_restored_nor_reopened() {
    use crate::restore::{RestoreBranch, RestoredSplitTree, RestoredView};
    for private in [true, false] {
        let f = fixture(private);
        let mut app = App::headless_for_test();
        let _ = opened(&mut app, &f);
        let wid = WindowId(0);
        let tab = app.windows[&wid].tab_set.active().unwrap().clone();
        let editor = tab.focus;
        assert_eq!(
            app.tab_restore_descriptor(&tab).is_none(),
            private,
            "restored private tab"
        );
        assert_eq!(app.native_restore_descriptor(&tab).is_none(), private);
        assert_eq!(app.view_restore_descriptor(editor).is_none(), private);

        let (_, terminal) =
            app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Vertical);
        for focus in [terminal, editor] {
            let tab = {
                let tab = app
                    .windows
                    .get_mut(&wid)
                    .unwrap()
                    .tab_set
                    .active_mut()
                    .unwrap();
                assert!(tab.set_focus(focus));
                tab.clone()
            };
            let described = app
                .tab_restore_descriptor(&tab)
                .expect("the split is described");
            match (&described.root, private) {
                (RestoredSplitTree::Leaf { view }, true) => {
                    assert!(matches!(view, RestoredView::Terminal(_)), "{view:?}");
                    assert_eq!(described.focused_path, Vec::<RestoreBranch>::new());
                }
                (RestoredSplitTree::Split { .. }, false) => {
                    assert_eq!(described.focused_path.len(), 1);
                }
                (root, _) => panic!("private={private}: {root:?}"),
            }
        }
        assert_eq!(
            app.closed_view_record_for(wid, tab.id, editor).is_none(),
            private,
            "a closed private pane is not kept for reopening"
        );
    }
}

/// A DESK OF ONLY A PRIVATE DOCUMENT HOLDS NO UPDATE. No capture describes it, so a kept image's
/// handoff had nothing to carry and the update waited until the tab closed, though a handoff from
/// any other desk drops the same tab. A Recovery tab now stands in, and neither its path nor its
/// text crosses. CONTROL: a quit keeps nothing, an ordinary document is carried as itself, and a
/// tab beside it leaves the private one out as A17 does.
#[test]
fn a_handoff_with_nothing_else_stands_a_recovery_tab_in_for_a_private_document() {
    use crate::restore::{RestoreManifest, RestoredSplitTree, RestoredView};
    let wid = WindowId(0);
    let lone = |private: bool| {
        let f = fixture(private);
        let mut app = App::headless_for_test();
        let shell = app.focused_session_id(wid).unwrap();
        let _ = opened(&mut app, &f);
        assert!(app.close_session_tab_for_test(shell));
        app.sync_window(wid);
        assert_eq!(
            app.windows[&wid].tab_set.len(),
            1,
            "PRECONDITION: it stands alone"
        );
        (app, f)
    };

    let (app, _f) = lone(true);
    let layout = app.capture_handoff_layout();
    assert_eq!(layout.windows.len(), 1);
    let row = &layout.windows[0];
    assert_eq!(row.active_item, Some(0));
    assert!(!row.workers_in_front);
    assert!(row.holds_no_terminal(), "a shell the person never had");
    assert_eq!(row.restored_tabs.len(), 1);
    let RestoredSplitTree::Leaf {
        view: RestoredView::Placeholder(stand_in),
    } = &row.restored_tabs[0].root
    else {
        panic!("{:?}", row.restored_tabs[0].root);
    };
    assert!(stand_in.reason.contains("private document"), "{stand_in:?}");
    let wire = layout.to_toml().expect("it serializes");
    assert!(
        !wire.contains("private.noindex") && !wire.contains("address.txt"),
        "no path crosses: {wire}"
    );
    assert_eq!(RestoreManifest::from_toml(&wire).as_ref(), Some(&layout));
    assert!(!layout.is_empty(), "a sessionless target places it");
    assert!(
        app.capture_restore_manifest().is_empty(),
        "CONTROL: a quit keeps nothing of it"
    );

    let (app, _f) = lone(false);
    let RestoredSplitTree::Leaf {
        view: RestoredView::Native(_),
    } = &app.capture_handoff_layout().windows[0].restored_tabs[0].root
    else {
        panic!("CONTROL: an ordinary document is carried as itself");
    };

    let f = fixture(true);
    let mut app = App::headless_for_test();
    let _ = opened(&mut app, &f);
    let layout = app.capture_handoff_layout();
    assert!(
        layout.windows[0].restored_tabs.iter().all(|tab| !matches!(
            tab.root,
            RestoredSplitTree::Leaf {
                view: RestoredView::Placeholder(_)
            }
        )),
        "CONTROL: a shell beside it needs no stand-in"
    );
}
