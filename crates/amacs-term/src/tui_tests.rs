// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn directory_opens_refreshes_and_visits_a_file() {
    use aterm_tty::tty::MemTty;
    // SAFETY: this test launches the editor with only its own scratch files.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    let mut tui = Tui::new(
        authority.grant(aterm_cap::Tier::Trusted),
        Options::default(),
    );
    let scratch = crate::test_scratch_dir("dir-refresh");
    let path = std::fs::canonicalize(scratch.path()).unwrap();
    let mut buffer = tui
        .open(&Target {
            path: path.clone(),
            line: None,
        })
        .unwrap();
    std::fs::write(path.join("entry.txt"), "contents").unwrap();
    let (mut tty, _peer) = MemTty::pair(Size { cols: 80, rows: 24 });
    tui.input(
        &mut tty,
        &mut buffer,
        EditorInput::Key {
            key: Key::Character('g'),
            mods: Modifiers::empty(),
            text: Some("g".into()),
        },
    )
    .unwrap();
    let listing = buffer.store.directory(buffer.document).unwrap();
    assert!(listing.text.contains("entry.txt"));
    let origin = listing.text.len();
    buffer.view.minibuffer = amacs::editor::Minibuffer::Search {
        query: "entry.txt".into(),
        origin,
    };
    std::fs::remove_file(path.join("entry.txt")).unwrap();
    let document = buffer.document;
    tui.effect(
        &mut tty,
        &mut buffer,
        EditorEffect::RefreshDirectory { document },
    )
    .unwrap();
    let amacs::editor::Minibuffer::Search { origin, .. } = &buffer.view.minibuffer else {
        panic!("search remains open")
    };
    let origin = *origin;
    assert_eq!(origin, buffer.text().len());
    assert!(buffer.text().is_char_boundary(origin));
    buffer.view.clear_minibuffer();
    std::fs::write(path.join("entry.txt"), "contents").unwrap();
    tui.effect(
        &mut tty,
        &mut buffer,
        EditorEffect::RefreshDirectory { document },
    )
    .unwrap();
    let listing = buffer.store.directory(buffer.document).unwrap();
    buffer.view.selections = vec![Selection::caret(listing.entries.last().unwrap().0.start)];
    let effects = tui
        .workspace
        .execute(
            &mut buffer.store,
            &mut buffer.view,
            amacs::editor::EditorCommand::Newline,
        )
        .unwrap();
    for effect in effects {
        tui.effect(&mut tty, &mut buffer, effect).unwrap();
    }
    assert_eq!(buffer.text().as_ref(), "contents");
    assert!(buffer.store.directory(buffer.document).is_none());
}

/// G2 review: a kill holding CRLF (from a file held byte for byte) yanked into a file whose
/// every line ends CRLF goes in as LF, the text it is held as, so the file does not save CR
/// CR LF. *Control:* before `crlf_on_disk` the yank wrote `x\r\ny` into the LF-held text.
#[test]
fn a_crlf_kill_yanks_into_a_crlf_file_as_its_held_lf() {
    use aterm_tty::tty::MemTty;

    // SAFETY: the test is a launcher with no untrusted input; its capability
    // is used only for this scratch document.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    let mut tui = Tui::new(
        authority.grant(aterm_cap::Tier::Trusted),
        Options::default(),
    );
    let scratch = crate::test_scratch_dir("crlf-kill-yank");
    let path = scratch.path().join("held.txt");
    std::fs::write(&path, "a\r\nb\r\n").unwrap();
    let mut buffer = tui.open(&Target { path, line: None }).unwrap();
    assert_eq!(buffer.text().as_ref(), "a\nb\n", "held with LF");
    tui.workspace.kill_ring.push_front("x\r\ny".to_string());
    let (mut tty, _peer) = MemTty::pair(Size { cols: 80, rows: 24 });
    let yank = EditorInput::Key {
        key: Key::Character('y'),
        mods: Modifiers::CTRL,
        text: None,
    };
    assert!(tui.input(&mut tty, &mut buffer, yank).unwrap().is_none());
    assert_eq!(buffer.text().as_ref(), "x\nya\nb\n");
}

#[test]
fn input_releases_commit_projections_even_after_a_partial_macro_error() {
    use amacs::editor::EditorCommand;
    use aterm_tty::tty::MemTty;

    // SAFETY: the test is a launcher with no untrusted input; its capability
    // is used only for this scratch document.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    let mut tui = Tui::new(
        authority.grant(aterm_cap::Tier::Trusted),
        Options::default(),
    );
    let scratch = crate::test_scratch_dir("macro-commits");
    let path = scratch.path().join("publication.txt");
    std::fs::write(&path, "base").unwrap();
    let mut buffer = tui.open(&Target { path, line: None }).unwrap();
    let (mut tty, _peer) = MemTty::pair(Size { cols: 80, rows: 24 });
    let key = |character, mods| EditorInput::Key {
        key: Key::Character(character),
        mods,
        text: None,
    };
    let mut send = |tui: &mut Tui, buffer: &mut Buffer, input| {
        assert!(tui.input(&mut tty, buffer, input).unwrap().is_none());
        assert!(tui.workspace.take_commits(buffer.document).is_empty());
    };

    send(
        &mut tui,
        &mut buffer,
        EditorInput::Text("seed ".to_string()),
    );
    assert_eq!(buffer.text().as_ref(), "seed base");
    tui.workspace
        .execute(
            &mut buffer.store,
            &mut buffer.view,
            EditorCommand::StartMacro,
        )
        .unwrap();
    send(&mut tui, &mut buffer, EditorInput::Text("x".to_string()));
    for _ in 0..2 {
        send(&mut tui, &mut buffer, key('/', Modifiers::CTRL));
    }
    tui.workspace
        .execute(&mut buffer.store, &mut buffer.view, EditorCommand::EndMacro)
        .unwrap();
    assert_eq!(buffer.text().as_ref(), "base");
    let before = buffer.store.snapshot(buffer.document).unwrap();

    send(&mut tui, &mut buffer, key('x', Modifiers::CTRL));
    send(&mut tui, &mut buffer, key('e', Modifiers::empty()));
    let after = buffer.store.snapshot(buffer.document).unwrap();
    assert_eq!(after.text, before.text);
    assert!(after.seq > before.seq);
    assert_eq!(
        tui.status.as_deref(),
        Some(error_text(&EditorError::NothingToUndo))
    );
    assert!(!tui.workspace.can_undo(buffer.document));
    assert!(tui.workspace.can_redo(buffer.document));
}

#[test]
fn a_position_names_a_line_and_a_character_on_it() {
    let text = "one\ntwo λx\nthree";
    assert_eq!(line_column_offset(text, 1, None), 0);
    assert_eq!(line_column_offset(text, 2, None), 4);
    assert_eq!(line_column_offset(text, 2, Some(5)), 4 + "two ".len());
    assert_eq!(line_column_offset(text, 2, Some(6)), 4 + "two λ".len());
    assert_eq!(line_column_offset(text, 2, Some(99)), 4 + "two λx".len());
    assert_eq!(line_column_offset(text, 99, None), "one\ntwo λx\n".len());
    assert_eq!(line_column_offset("", 3, Some(3)), 0);
}

#[test]
fn relexing_an_edit_matches_a_fresh_lex() {
    let texts = [
        "# Title\n\nsome *text*\n```\ncode\n```\n",
        "# Title\nsome *text*\n```\ncode\n```\n",
        "# Title\n\nsome *text*\n``\ncode\n```\n",
        "",
        "```\n",
        "a\nb\nc",
        "a\nX\nc",
        "ab",
        "a\nb",
    ];
    for old in texts {
        for new in texts {
            let mut cache = LexCache::new(Language::Markdown, old);
            relex(&mut cache, old, new);
            assert_eq!(
                cache,
                LexCache::new(Language::Markdown, new),
                "{old:?} -> {new:?}"
            );
        }
    }
}

#[test]
fn a_key_answers_with_the_byte_it_types() {
    let none = Modifiers::empty();
    assert_eq!(
        answer_byte(&Key::Character('y'), none, Some('y')),
        Some(b'y')
    );
    assert_eq!(
        answer_byte(&Key::Character('y'), Modifiers::SHIFT, Some('Y')),
        Some(b'Y')
    );
    assert_eq!(
        answer_byte(&Key::Named(NamedKey::Space), none, Some(' ')),
        Some(b' ')
    );
    assert_eq!(
        answer_byte(&Key::Named(NamedKey::Backspace), none, None),
        Some(0x7f)
    );
    assert_eq!(
        answer_byte(&Key::Named(NamedKey::Escape), none, None),
        Some(0x1b)
    );
    assert_eq!(
        answer_byte(&Key::Character('g'), Modifiers::CTRL, None),
        Some(0x07)
    );
    assert_eq!(
        answer_byte(&Key::Character('y'), Modifiers::ALT, None),
        None
    );
    assert_eq!(answer_byte(&Key::Character('λ'), none, Some('λ')), None);
    assert_eq!(
        answer_byte(&Key::Named(NamedKey::ArrowUp), none, None),
        None
    );
    for byte in 0..=u8::MAX {
        let classes = [saves_on(byte), discards_on(byte), cancels_on(byte)];
        assert!(
            classes.iter().filter(|class| **class).count() <= 1,
            "{byte:#04x}"
        );
    }
}

#[test]
fn a_paste_breaks_lines_with_line_feeds() {
    assert_eq!(paste_text("a\rb\r\nc\nd"), "a\nb\nc\nd");
}
