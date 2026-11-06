// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Revealed values stay out of the accessibility tree (one-mesh design §4.8, §11 A17b): a
//! private document's grid publishes its table's shape, its headers and the columns its opener
//! did not name secret, and each concealed cell as `hidden value` with no value; its text view
//! publishes no line. A copy of a hidden value reaches the clipboard on the person's own keys
//! only. Each test searches the whole update the window would send (and the virtual text the
//! adapter serves from it) for every secret; an ordinary document beside it, and the plain
//! publication [`crate::native_app::EditorApp::plain_for_negative_control`] restores, show the
//! same search finds a value where one is.

use aterm_types::keyboard::{Key, Modifiers, NamedKey};

use crate::native_app::AppKind;
use crate::native_editor::{CONCEALED_STAYS, COPIED_HIDDEN, HIDDEN_VALUE};
use crate::tab_model::AppInstanceId;
use crate::{App, WindowId};

const SECRETS: [&str; 2] = ["hunter2-Zq9", "s3cr3t-Wv7"];
const EMAILS: [&str; 2] = ["ada@example.test", "bob@example.test"];
const WID: WindowId = WindowId(0);

/// A reveal's grid as `db alab --show-passwords` writes one: a header row, then records whose
/// `password` cells hold the secrets.
fn reveal_text() -> String {
    format!(
        "address\tpassword\tnote\n{}\t{}\tfirst\n{}\t{}\tsecond\n",
        EMAILS[0], SECRETS[0], EMAILS[1], SECRETS[1]
    )
}

/// `name` holding [`reveal_text`] in a private edit's folder (`private`) or an ordinary one.
struct Fixture {
    dir: aterm_tempfile::TempDir,
    uri: String,
}

fn fixture(private: bool, name: &str) -> Fixture {
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-conceal-")
        .tempdir()
        .unwrap();
    let folder = dir.path().join(if private {
        "att-edit-t.private.noindex"
    } else {
        "att-edit-t.noindex"
    });
    std::fs::create_dir(&folder).unwrap();
    let path = folder.join(name);
    std::fs::write(&path, reveal_text()).unwrap();
    let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
    Fixture { dir, uri }
}

/// A headless app whose drafts go under `f`'s folder.
fn app_for(f: &Fixture) -> App {
    let mut app = App::headless_for_test();
    app.use_document_journal_root_for_test(f.dir.path().join("drafts"));
    app
}

/// `@<session> edit <uri> ro=1 [secret=…]`, answered inline: its reply line.
fn reveal(app: &mut App, uri: &str, secret: Option<&[&str]>) -> String {
    app.serve_edits_headless_for_test();
    let session = super::private_tests::caller(app);
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
            read_only: true,
            secret: secret.map(|headers| headers.iter().map(|&header| header.to_owned()).collect()),
        },
        reply,
    });
    answer.try_recv().expect("an inline edit answers at once")
}

/// The front Editor's instance.
fn editor(app: &App) -> AppInstanceId {
    app.active_native_view(WID).expect("a front Editor view").0
}

/// The accessibility update the window would send now, with the virtual text its adapter serves
/// from it, as one searchable text.
fn published(app: &mut App) -> String {
    app.windows.get_mut(&WID).unwrap().a11y_active = true;
    let (_, view) = app.active_native_view(WID).expect("a front Editor view");
    assert!(app.prepare_native_input_scratch(WID));
    let plan = app.active_visible_leaf_plan(WID).expect("a visible plan");
    app.stage_native_accessibility(WID, view, &plan);
    let (update, published) = app
        .take_native_accessibility_update(WID)
        .expect("an accessibility update")
        .expect("a valid accessibility update");
    format!("{update:?}\n{published:?}")
}

/// The secrets `tree` holds.
fn secrets_in(tree: &str) -> Vec<&'static str> {
    SECRETS
        .into_iter()
        .filter(|secret| tree.contains(secret))
        .collect()
}

fn key(key: Key, mods: Modifiers) -> crate::input::InputEvent {
    crate::input::InputEvent::Key {
        key,
        mods,
        base_layout: None,
        layout_text: None,
        event_type: aterm_types::keyboard::KeyEventType::Press,
    }
}

/// The front Editor view's status line.
fn status(app: &App) -> Option<String> {
    let (_, view) = app.active_native_view(WID)?;
    match app.native_runtime.view_state(view) {
        Some(crate::native_app::AppViewState::Editor(state)) => state.status.clone(),
        _ => None,
    }
}

/// What the clipboard holds (the test build's stand-in).
fn board() -> Option<String> {
    crate::control::PBPASTE_STUB.with(|board| board.borrow().clone())
}

/// A reveal opened with `secret=password` publishes the grid's shape, its headers and every
/// address, and each password cell as `hidden value`; the current cell on a password reads
/// `hidden value`, and so does the status a copy of it leaves. Opened with no `secret=`, or as an
/// ordinary open of the private file, every cell is hidden, the addresses too; its text view
/// publishes no line. `secret=` on a file outside a private edit's folder is refused.
/// NEGATIVE CONTROL: the plain publication
/// ([`crate::native_app::NativeRuntime::publish_plain_for_negative_control`]) puts both
/// passwords back in the tree, as the ordinary grid and text view beside them do, so with
/// concealment undone the first check fails: `` a revealed value reached the accessibility tree ``.
#[test]
fn a_reveal_s_secrets_stay_out_of_the_accessibility_tree() {
    // The reveal: the password column hidden, the rest read out.
    let f = fixture(true, "address.tsv");
    let mut app = app_for(&f);
    let reply = reveal(&mut app, &f.uri, Some(&["password"]));
    assert!(reply.starts_with("OK edit "), "{reply}");
    let tree = published(&mut app);
    assert_eq!(
        secrets_in(&tree),
        Vec::<&str>::new(),
        "a revealed value reached the accessibility tree"
    );
    for wanted in [
        EMAILS[0],
        EMAILS[1],
        "address",
        "password",
        "note",
        "first",
        HIDDEN_VALUE,
        "2 rows, 3 columns",
    ] {
        assert!(tree.contains(wanted), "{wanted} is read out: {tree}");
    }
    // The current cell on a password, and a copy of it.
    for step in [NamedKey::ArrowDown, NamedKey::ArrowRight] {
        assert!(app.native_input_event(WID, &key(Key::Named(step), Modifiers::empty())));
    }
    let tree = published(&mut app);
    assert!(
        tree.contains(&format!("Row 2, password: {HIDDEN_VALUE}")),
        "the current cell is the first password: {tree}"
    );
    assert!(app.native_input_event(WID, &key(Key::Character('w'), Modifiers::ALT)));
    assert_eq!(board().as_deref(), Some(SECRETS[0]), "the person's copy");
    let tree = published(&mut app);
    assert!(tree.contains(COPIED_HIDDEN), "{tree}");
    assert_eq!(secrets_in(&tree), Vec::<&str>::new());

    // The negative control: concealment undone, the same search finds both passwords.
    let instance = editor(&app);
    assert!(
        app.native_runtime
            .publish_plain_for_negative_control(instance)
    );
    assert_eq!(secrets_in(&published(&mut app)), SECRETS);

    // No `secret=`, and an ordinary open of the private file: every cell hidden.
    for by_edit in [true, false] {
        let f = fixture(true, "address.tsv");
        let mut app = app_for(&f);
        if by_edit {
            let reply = reveal(&mut app, &f.uri, None);
            assert!(reply.starts_with("OK edit "), "{reply}");
        } else {
            app.open_document_tab(AppKind::Editor, &f.uri).unwrap();
        }
        let document = app.native_runtime.document_id(editor(&app)).unwrap();
        assert_eq!(
            app.document_store.concealed(document),
            Some(&crate::document_store::Concealed::All),
            "the store marks it, so a copy's status and grant see it (by_edit={by_edit})"
        );
        let tree = published(&mut app);
        assert_eq!(secrets_in(&tree), Vec::<&str>::new(), "by_edit={by_edit}");
        assert!(
            EMAILS.iter().all(|email| !tree.contains(email)),
            "every cell is hidden (by_edit={by_edit}): {tree}"
        );
        assert!(tree.contains(HIDDEN_VALUE), "{tree}");
    }

    // A private text view publishes no line; an ordinary grid and text view publish theirs.
    for (private, name) in [
        (true, "address.txt"),
        (false, "address.txt"),
        (false, "address.tsv"),
    ] {
        let f = fixture(private, name);
        let mut app = app_for(&f);
        app.open_document_tab(AppKind::Editor, &f.uri).unwrap();
        let tree = published(&mut app);
        let expected: Vec<&str> = if private {
            Vec::new()
        } else {
            SECRETS.to_vec()
        };
        assert_eq!(secrets_in(&tree), expected, "{name} private={private}");
        assert_eq!(
            tree.contains(crate::native_accessibility::PRIVATE_TEXT_WITHHELD),
            private,
            "{name} private={private}"
        );
    }

    // `secret=` names the columns of a private grid only.
    let f = fixture(false, "address.tsv");
    let mut app = app_for(&f);
    let reply = reveal(&mut app, &f.uri, Some(&["password"]));
    assert!(
        reply.starts_with("ERR refused open: secret= names the columns of a private grid"),
        "{reply}"
    );
}

/// A hidden value reaches the clipboard on the person's own keys only. Another process's posted
/// keys and an accessibility client's request copy none of it and say why, by the grid's copy, by
/// its text face selected whole, or by a kill; Edit ▸ Copy and the palette's Copy row, which an
/// accessibility client can press with no event of the person's behind it, are greyed over it;
/// and the person's own copy reaches the clipboard while the kill ring every document yanks from
/// keeps none of it. The same posted copies in an ordinary grid reach the clipboard, the whole
/// text with both secrets included (the negative control: without the concealed grant in
/// `dispatch_editor_event`, the posted copy of the password lands on the clipboard,
/// `` posted keys copied a hidden value ``; without the private check in `copy_selection_in`,
/// the text face's, `` a posted copy of the text face ``).
#[test]
fn only_the_person_s_own_keys_copy_a_hidden_value() {
    use crate::native_app::{AppEvent, TextInputEvent};
    use crate::native_editor::EditorCommand;
    use aterm_window::event::InputOrigin;
    for private in [true, false] {
        let f = fixture(private, "address.tsv");
        let mut app = app_for(&f);
        if private {
            let reply = reveal(&mut app, &f.uri, Some(&["password"]));
            assert!(reply.starts_with("OK edit "), "{reply}");
        } else {
            app.open_document_tab(AppKind::Editor, &f.uri).unwrap();
        }
        for step in [NamedKey::ArrowDown, NamedKey::ArrowRight] {
            assert!(app.native_input_event(WID, &key(Key::Named(step), Modifiers::empty())));
        }
        assert!(
            app.native_has_selection(WID),
            "the current cell holds a value"
        );
        assert_eq!(
            app.menu_copies(WID),
            !private,
            "Edit ▸ Copy (private={private})"
        );
        app.frontmost_window = Some(WID);
        assert_eq!(
            app.palette_live().has_selection,
            !private,
            "the palette's Copy row"
        );
        let copy = key(Key::Character('w'), Modifiers::ALT);
        assert!(app.with_origin_mark(InputOrigin::Relayed, |app| {
            app.native_input_event(WID, &copy)
        }));
        assert_eq!(
            board().is_some(),
            !private,
            "posted keys copied a hidden value (private={private})"
        );
        if private {
            assert!(app.with_assistive_source(|app| app.native_input_event(WID, &copy)));
            assert_eq!(
                board(),
                None,
                "an accessibility client copied a hidden value"
            );
            assert_eq!(
                status(&app).as_deref(),
                Some("kill-ring-save needs clipboard authority, which this input does not carry")
            );
            // A kill, which would go to the guest's own ring, is refused before anything else.
            assert!(app.with_origin_mark(InputOrigin::Relayed, |app| {
                app.native_input_event(WID, &key(Key::Character('k'), Modifiers::CTRL))
            }));
            assert_eq!(status(&app).as_deref(), Some(CONCEALED_STAYS));
            assert_eq!(board(), None);
            assert!(app.native_input_event(WID, &copy));
            assert_eq!(
                board().as_deref(),
                Some(SECRETS[0]),
                "the person's own copy"
            );
            assert!(
                !app.editor_workspace
                    .kill_ring
                    .iter()
                    .any(|kill| SECRETS.iter().any(|secret| kill.contains(secret))),
                "the kill ring keeps no hidden value"
            );
            crate::control::PBPASTE_STUB.with(|board| *board.borrow_mut() = None);
        }
        // The text face, selected whole and copied, all by posted input.
        let copied = app.with_origin_mark(InputOrigin::Relayed, |app| {
            app.dispatch_native_event(WID, AppEvent::EditorCommand(EditorCommand::ToggleGridView))
                .unwrap();
            app.dispatch_native_event(WID, AppEvent::TextInput(TextInputEvent::SelectAll))
                .unwrap();
            app.copy_in(WID)
        });
        assert_eq!(
            copied, !private,
            "a posted copy of the text face (private={private})"
        );
        assert_eq!(
            board().is_some_and(|text| SECRETS.iter().all(|secret| text.contains(secret))),
            !private,
            "a posted copy of the text face (private={private})"
        );
        crate::control::PBPASTE_STUB.with(|board| *board.borrow_mut() = None);
    }
}
