// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn fixture() -> (
    EditorApp,
    EditorViewState,
    crate::document_store::DocumentSnapshot,
) {
    let mut store = crate::test_document_store();
    let uri = "file:///workspace/notes.md".to_string();
    let document = store.open(
        uri.clone(),
        "# Heading\n\nFirst paragraph.\n\n## Next\n\nSecond paragraph.\n".to_string(),
    );
    let snapshot = store.snapshot(document).unwrap();
    let mut app = EditorApp::new_with_uri(document, "notes.md".to_string(), uri);
    app.markdown.as_mut().unwrap().parsed = crate::native_markdown::parse(&snapshot.text);
    let view = EditorViewState {
        buffer: Some(crate::native_editor::EditorBufferView::new(
            document,
            crate::document_store::DocumentViewId(1),
            snapshot.seq,
        )),
        markdown: Some(MarkdownWorkspaceView::default()),
        ..EditorViewState::default()
    };
    (app, view, snapshot)
}

fn cx<'a>(rect: LogicalRect, snapshot: &'a crate::document_store::DocumentSnapshot) -> ViewCx<'a> {
    ViewCx {
        viewport: rect,
        animation_phase_ms: 0,
        motion: ViewMotionCx::default(),
        terminal_font_px: 12.0,
        terminal_scale: 1.0,
        semantic_font: None,
        document: Some(snapshot),
    }
}

fn has_markdown(node: &UiNode) -> bool {
    matches!(node.content, UiContent::MarkdownBlock(_)) || node.children.iter().any(has_markdown)
}

#[test]
fn modes_share_identity_and_split_uses_the_real_editor_at_both_orientations() {
    let (mut app, mut view, snapshot) = fixture();
    for dirty in [false, true] {
        app.dirty = dirty;
        for width in [320.0, 640.0, 1280.0] {
            for mode in [
                MarkdownWorkspaceMode::Read,
                MarkdownWorkspaceMode::Edit,
                MarkdownWorkspaceMode::Split,
            ] {
                app.set_markdown_mode(&mut view, mode);
                let rect = LogicalRect::new(11.0, 17.0, width, 800.0);
                let layout = app.workspace_layout(&view, rect);
                let tree = app.view(&view, &cx(rect, &snapshot));
                assert_eq!(
                    has_markdown(&tree.root),
                    mode != MarkdownWorkspaceMode::Edit
                );
                let compiled = tree.compile(rect).unwrap();
                compiled.validate_parity().unwrap();
                assert_eq!(
                    compiled
                        .hits
                        .iter()
                        .any(|hit| hit.action.as_str() == "editor/save"),
                    dirty,
                    "Save follows document state at {width} in {mode:?}"
                );
                assert!(
                    compiled
                        .semantic(&UiKey::new("editor/save-button"))
                        .is_none()
                );
                assert!(
                    compiled
                        .semantic(&UiKey::new("editor/command-bar"))
                        .is_none()
                );
                assert_eq!(
                    compiled.semantic(&UiKey::new("editor/buffer")).is_some(),
                    mode != MarkdownWorkspaceMode::Read
                );
                assert!(
                    compiled.semantic(&UiKey::new("markdown/source")).is_none(),
                    "Split never has a read-only source imitation"
                );
                for choice in [
                    MarkdownWorkspaceMode::Read,
                    MarkdownWorkspaceMode::Edit,
                    MarkdownWorkspaceMode::Split,
                ] {
                    let control = compiled.semantic(&UiKey::new(choice.action())).unwrap();
                    assert_eq!(control.state.unwrap().selected, choice == mode);
                    assert!(
                        compiled.hits.iter().any(|hit| hit.key == control.key),
                        "all mode choices remain reachable at {width}"
                    );
                }
                if mode == MarkdownWorkspaceMode::Split {
                    let editor = layout.editor.unwrap();
                    let reader = layout.reader.unwrap();
                    assert_eq!(layout.stacked, width < 900.0);
                    if layout.stacked {
                        assert!(editor.bottom() <= reader.y);
                    } else {
                        assert!(editor.right() <= reader.x);
                    }
                    let divider = layout.divider.unwrap();
                    assert!(
                        (i32::from(layout.resize_per_mille(divider.x, divider.y)) - 500).abs() <= 1
                    );
                }
                assert_eq!(view.buffer.as_ref().unwrap().document, app.document);
            }
        }
    }
}

#[test]
fn read_roundtrip_keeps_cursor_selection_and_retires_delayed_input() {
    let (app, mut view, _) = fixture();
    let buffer = view.buffer.as_mut().unwrap();
    buffer.selections = vec![crate::native_editor::Selection {
        anchor: 11,
        head: 16,
    }];
    buffer.viewport_anchor = 11;
    let before = buffer.clone();
    let lifetime = view.common.input_lifetime();
    assert!(app.set_markdown_mode(&mut view, MarkdownWorkspaceMode::Read));
    assert!(!Arc::ptr_eq(&lifetime, &view.common.input_lifetime()));
    assert!(app.set_markdown_mode(&mut view, MarkdownWorkspaceMode::Edit));
    assert_eq!(view.buffer.as_ref().unwrap(), &before);
    app.set_markdown_mode(&mut view, MarkdownWorkspaceMode::Read);
    view.markdown.as_mut().unwrap().reader.source_anchor = 29;
    app.set_markdown_mode(&mut view, MarkdownWorkspaceMode::Split);
    assert_eq!(view.buffer.as_ref().unwrap().viewport_anchor, 29);
    assert_eq!(view.buffer.as_ref().unwrap().selections, before.selections);
    assert_eq!(
        view.buffer.as_ref().unwrap().document_view,
        before.document_view
    );
}

#[test]
fn expanding_split_keeps_reader_position_while_edit_to_read_follows_source() {
    for focus in [MarkdownPane::Editor, MarkdownPane::Reader] {
        let (app, mut view, _) = fixture();
        view.buffer.as_mut().unwrap().viewport_anchor = 11;
        app.set_markdown_mode(&mut view, MarkdownWorkspaceMode::Split);
        let workspace = view.markdown.as_mut().unwrap();
        workspace.focus = focus;
        workspace.reader.source_anchor = 29;
        workspace.reader.visual_row = 2;
        workspace.reader.selection = Some(29..36);
        workspace
            .reader
            .history
            .visit(crate::native_markdown::MarkdownLocation::new(29, 2));
        let reader_before = workspace.reader.clone();
        let editor_before = view.buffer.clone();

        assert!(app.set_markdown_mode(&mut view, MarkdownWorkspaceMode::Read));
        assert_eq!(view.markdown.as_ref().unwrap().reader, reader_before);
        assert_eq!(view.buffer, editor_before);
        assert!(app.set_markdown_mode(&mut view, MarkdownWorkspaceMode::Split));
        assert_eq!(view.markdown.as_ref().unwrap().reader, reader_before);
        assert_eq!(view.buffer, editor_before);

        app.set_markdown_mode(&mut view, MarkdownWorkspaceMode::Edit);
        app.set_markdown_mode(&mut view, MarkdownWorkspaceMode::Read);
        let reader = &view.markdown.as_ref().unwrap().reader;
        assert_eq!((reader.source_anchor, reader.visual_row), (11, 0));
        assert_eq!(view.buffer, editor_before);
    }
}

#[test]
fn document_completion_and_conflict_controls_survive_read_and_split() {
    let (mut app, mut view, snapshot) = fixture();
    view.edit_header = Some(EditHeader {
        for_word: Some("review".to_string()),
        waiter: EditWaiter::Waiting,
        read_only: false,
    });
    let rect = LogicalRect::new(0.0, 0.0, 640.0, 800.0);
    for mode in [
        MarkdownWorkspaceMode::Read,
        MarkdownWorkspaceMode::Edit,
        MarkdownWorkspaceMode::Split,
    ] {
        app.set_markdown_mode(&mut view, mode);
        let compiled = app.view(&view, &cx(rect, &snapshot)).compile(rect).unwrap();
        assert!(compiled.semantic(&UiKey::new("editor/finish")).is_some());
        assert!(compiled.semantic(&UiKey::new("editor/abort")).is_some());
        compiled.validate_parity().unwrap();
    }
    app.disk_notice = Some(DiskNotice::Changed);
    app.disk_conflict = true;
    app.set_markdown_mode(&mut view, MarkdownWorkspaceMode::Read);
    let compiled = app.view(&view, &cx(rect, &snapshot)).compile(rect).unwrap();
    assert!(
        compiled
            .semantic(&UiKey::new("editor/disk-reload"))
            .is_some()
    );
    assert!(compiled.semantic(&UiKey::new("editor/disk-keep")).is_some());
    assert!(compiled.semantic(&UiKey::new("editor/finish")).is_none());
}

#[test]
fn concealed_markdown_never_enters_the_reader_publication() {
    let (mut app, mut view, snapshot) = fixture();
    app.concealed = Some(crate::document_store::Concealed::All);
    assert!(!app.set_markdown_mode(&mut view, MarkdownWorkspaceMode::Read));
    let rect = LogicalRect::new(0.0, 0.0, 900.0, 700.0);
    let tree = app.view(&view, &cx(rect, &snapshot));
    assert!(!has_markdown(&tree.root));
    let compiled = tree.compile(rect).unwrap();
    assert!(
        !compiled
            .semantic(&UiKey::new("editor/markdown/read"))
            .unwrap()
            .state
            .unwrap()
            .enabled
    );
}

#[test]
fn preview_publishes_unsaved_canonical_text_without_replacing_the_view() {
    let (app, state, mut snapshot) = fixture();
    let document = app.document;
    let mut runtime = NativeRuntime::new();
    let instance = runtime
        .insert_instance(NativeApp::Editor(Box::new(app)))
        .unwrap();
    let view = ViewId::from_stored(12);
    runtime
        .attach_view(view, instance, AppViewState::Editor(Box::new(state)))
        .unwrap();
    runtime.set_editor_markdown_mode(instance, view, MarkdownWorkspaceMode::Split);
    snapshot.text = Arc::from("# Unsaved revision\n\nThis has not reached disk.\n");
    snapshot.seq.0 += 1;
    runtime.publish_document(document, &snapshot.text, snapshot.seq.0, true);
    runtime
        .dispatch(
            instance,
            view,
            AppEvent::DocumentChanged {
                document,
                revision: snapshot.seq.0,
            },
        )
        .unwrap();
    let Some(NativeApp::Editor(app)) = runtime.app(instance) else {
        panic!("canonical editor replaced");
    };
    assert_eq!(
        app.markdown.as_ref().unwrap().parsed.outline[0].text,
        "Unsaved revision"
    );
    assert!(app.dirty);
    let rect = LogicalRect::new(0.0, 0.0, 1280.0, 800.0);
    let tree = runtime
        .render(instance, view, &cx(rect, &snapshot))
        .unwrap();
    let compiled = tree.compile(rect).unwrap();
    assert!(compiled.semantic(&UiKey::new("editor/buffer")).is_some());
    assert!(
        compiled
            .semantics
            .iter()
            .any(|node| node.label.contains("Unsaved revision"))
    );
    let Some(AppViewState::Editor(state)) = runtime.view_state(view) else {
        panic!("view replaced");
    };
    assert_eq!(
        state.buffer.as_ref().unwrap().document_view,
        crate::document_store::DocumentViewId(1)
    );
}

#[test]
fn composition_and_minibuffer_keep_focus_against_actions_and_generic_focus() {
    for composing in [false, true] {
        let (app, mut state, _) = fixture();
        app.set_markdown_mode(&mut state, MarkdownWorkspaceMode::Split);
        if composing {
            state.preedit = "正在".to_string();
        } else {
            state.buffer.as_mut().unwrap().minibuffer = crate::native_editor::Minibuffer::Command {
                query: "find".to_string(),
                selected: 0,
            };
        }
        let mut runtime = NativeRuntime::new();
        let instance = runtime
            .insert_instance(NativeApp::Editor(Box::new(app)))
            .unwrap();
        let view = ViewId::from_stored(19);
        runtime
            .attach_view(view, instance, AppViewState::Editor(Box::new(state)))
            .unwrap();
        for event in [
            AppEvent::Action(ActionInvocation {
                id: ActionId::new("editor/markdown/focus-reader"),
                value: None,
            }),
            AppEvent::FocusChanged(Some(UiKey::new("markdown/outline-item/0"))),
        ] {
            runtime.dispatch(instance, view, event).unwrap();
            let Some(AppViewState::Editor(state)) = runtime.view_state(view) else {
                panic!("view replaced");
            };
            assert!(state.markdown.as_ref().unwrap().editor_focused());
            assert_eq!(
                state.common.last_focus.as_ref().unwrap().as_str(),
                "editor/buffer"
            );
        }
    }
}

#[test]
fn inserts_and_deletes_above_reader_keep_its_paragraph_and_independent_editor_selection() {
    let (_, mut view, snapshot) = fixture();
    let original = snapshot.text.as_ref();
    let anchor = original.find("## Next").unwrap();
    let buffer = view.buffer.as_mut().unwrap();
    buffer.selections = vec![crate::native_editor::Selection { anchor: 2, head: 6 }];
    let editor_before = buffer.clone();
    let workspace = view.markdown.as_mut().unwrap();
    workspace.mode = MarkdownWorkspaceMode::Split;
    workspace.reader.source_anchor = anchor;
    workspace.reader.visual_row = 3;
    workspace.reader.selection = Some(anchor..anchor + "## Next".len());
    workspace.reader_entry_anchor = Some(anchor);
    workspace
        .reader
        .history
        .visit(crate::native_markdown::MarkdownLocation::new(11, 1));
    workspace
        .reader
        .history
        .visit(crate::native_markdown::MarkdownLocation::new(anchor, 3));
    let added = format!("# Préface\n\nNew material.\n\n{original}");
    let deltas = |from: &str, to: &str| {
        amacs::diff::minimal_edits(from, to)
            .into_iter()
            .map(|edit| crate::document_store::EditDelta {
                old: edit.range,
                inserted_len: edit.insert.len(),
            })
            .collect::<Vec<_>>()
    };
    workspace.observe_document_edits(&deltas(original, &added));
    assert!(added[workspace.reader.source_anchor..].starts_with("## Next"));
    assert_eq!(
        &added[workspace.reader.selection.clone().unwrap()],
        "## Next"
    );
    assert_eq!(
        workspace.reader_entry_anchor,
        Some(workspace.reader.source_anchor)
    );
    assert_eq!(workspace.reader.visual_row, 3);
    let earlier = workspace.reader.history.back().unwrap();
    assert!(added[earlier.source_anchor..].starts_with("First paragraph."));
    assert_eq!(earlier.visual_row, 1);
    assert_eq!(
        workspace.reader.history.forward().unwrap().source_anchor,
        workspace.reader.source_anchor
    );
    workspace.observe_document_edits(&deltas(&added, original));
    assert_eq!(workspace.reader.source_anchor, anchor);
    assert_eq!(
        workspace.reader.selection,
        Some(anchor..anchor + "## Next".len())
    );
    assert_eq!(workspace.reader_entry_anchor, Some(anchor));
    assert_eq!(
        workspace.reader.history.current().unwrap(),
        crate::native_markdown::MarkdownLocation::new(anchor, 3)
    );
    assert!(workspace.reader.history.can_back());
    assert!(!workspace.reader.history.can_forward());
    assert_eq!(
        view.buffer.as_ref().unwrap(),
        &editor_before,
        "rebasing a preview never moves its source caret twice"
    );
}

#[test]
fn config_authority_removes_markdown_presentation_even_when_canonical_path_is_md() {
    let (app, mut state, snapshot) = fixture();
    let document = app.document;
    app.set_markdown_mode(&mut state, MarkdownWorkspaceMode::Read);
    let old_lifetime = state.common.input_lifetime();
    let mut runtime = NativeRuntime::new();
    let instance = runtime
        .insert_instance(NativeApp::Editor(Box::new(app)))
        .unwrap();
    let view = ViewId::from_stored(20);
    runtime
        .attach_view(view, instance, AppViewState::Editor(Box::new(state)))
        .unwrap();
    assert!(runtime.enable_config_editor(document, &snapshot.text, snapshot.seq.0));
    let Some(NativeApp::Editor(app)) = runtime.app(instance) else {
        panic!("editor replaced");
    };
    assert!(app.markdown.is_none());
    let Some(AppViewState::Editor(state)) = runtime.view_state(view) else {
        panic!("view replaced");
    };
    assert!(state.markdown.is_none());
    assert!(!Arc::ptr_eq(&old_lifetime, &state.common.input_lifetime()));
    assert_eq!(
        state.common.last_focus.as_ref().unwrap().as_str(),
        "editor/buffer"
    );
}

#[test]
fn read_keeps_save_failure_and_recovery_visible_beside_ticket_controls() {
    let (mut app, mut view, snapshot) = fixture();
    app.recovery_status = Some("Recovered an unsaved draft".to_string());
    view.edit_header = Some(EditHeader {
        for_word: None,
        waiter: EditWaiter::Waiting,
        read_only: false,
    });
    app.set_markdown_mode(&mut view, MarkdownWorkspaceMode::Read);
    view.markdown.as_mut().unwrap().reader.notice =
        Some("Could not copy the selection".to_string());
    let rect = LogicalRect::new(0.0, 0.0, 640.0, 800.0);
    for outcome in [
        "Save failed: permission denied",
        "Not done: the save did not reach the text you finished",
        "Not aborted: another view is editing the document",
    ] {
        view.status = Some(outcome.to_string());
        let layout = app.workspace_layout(&view, rect);
        let compiled = app.view(&view, &cx(rect, &snapshot)).compile(rect).unwrap();
        let status = compiled
            .semantic(&UiKey::new("editor/markdown/status-words"))
            .unwrap();
        assert!(status.label.contains(outcome));
        assert!(status.label.contains("Recovered an unsaved draft"));
        assert!(status.label.contains("Could not copy the selection"));
        assert!(compiled.semantic(&UiKey::new("editor/finish")).is_some());
        assert!(compiled.semantic(&UiKey::new("editor/abort")).is_some());
        assert!(layout.status.unwrap().bottom() <= layout.reader.unwrap().y);
        compiled.validate_parity().unwrap();
    }
    app.recovery_status = None;
    view.status = Some(String::new());
    view.markdown.as_mut().unwrap().reader.notice = Some("  ".to_string());
    assert!(app.workspace_layout(&view, rect).status.is_none());
}

fn local_source() -> &'static str {
    if cfg!(windows) {
        "file:///C:/docs/readme.md"
    } else {
        "file:///docs/readme.md"
    }
}

#[test]
fn local_markdown_links_decode_one_exact_path_and_preserve_heading_fragments() {
    let (uri, fragment) =
        resolve_markdown_link(local_source(), "guide%20%C3%A9%2Emd#caf%C3%A9").unwrap();
    assert!(uri.ends_with("/docs/guide%20%C3%A9.md"));
    assert_eq!(fragment.as_deref(), Some("café"));
    let (uri, fragment) = resolve_markdown_link(local_source(), "../next.md#section").unwrap();
    assert!(uri.ends_with("/docs/../next.md"));
    assert_eq!(fragment.as_deref(), Some("section"));
    let root = if cfg!(windows) {
        "file:///C:/readme.md"
    } else {
        "file:///readme.md"
    };
    assert!(
        resolve_markdown_link(root, "next.md")
            .unwrap()
            .0
            .ends_with("/next.md")
    );
    for refused in [
        "",
        "#heading",
        "guide.txt",
        "guide.md?query",
        "https://example.test/guide.md",
        "file:///guide.md",
        "//server/guide.md",
        "\\server\\guide.md",
        "guide%ZZ.md",
        "guide%00.md",
        "guide.md#%00",
    ] {
        assert!(
            resolve_markdown_link(local_source(), refused).is_err(),
            "{refused}"
        );
    }
}

#[test]
fn local_document_effect_is_emitted_only_for_an_explicit_admitted_markdown_link() {
    let (mut app, mut state, snapshot) = fixture();
    let document = app.document;
    let reader = app.markdown.as_mut().unwrap();
    reader.canonical_uri = local_source().to_string();
    reader.parsed = crate::native_markdown::parse("[Guide](guide.md#part) [Text](notes.txt)");
    app.set_markdown_mode(&mut state, MarkdownWorkspaceMode::Read);
    let mut runtime = NativeRuntime::new();
    let instance = runtime
        .insert_instance(NativeApp::Editor(Box::new(app)))
        .unwrap();
    let view = ViewId::from_stored(30);
    runtime
        .attach_view(view, instance, AppViewState::Editor(Box::new(state)))
        .unwrap();
    let changed = runtime
        .dispatch(
            instance,
            view,
            AppEvent::DocumentChanged {
                document,
                revision: snapshot.seq.0,
            },
        )
        .unwrap();
    assert!(
        !changed
            .effects
            .iter()
            .any(|effect| matches!(effect, AppEffect::OpenMarkdownDocument { .. }))
    );
    let clicked = runtime
        .dispatch(
            instance,
            view,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new("markdown/link/0"),
                value: None,
            }),
        )
        .unwrap();
    assert!(
        matches!(clicked.effects.as_slice(), [AppEffect::OpenMarkdownDocument { document: opened, destination }] if *opened == document && destination == "guide.md#part")
    );
    let denied = runtime
        .dispatch(
            instance,
            view,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new("markdown/link/1"),
                value: None,
            }),
        )
        .unwrap();
    assert!(
        !denied
            .effects
            .iter()
            .any(|effect| matches!(effect, AppEffect::OpenMarkdownDocument { .. }))
    );
}
