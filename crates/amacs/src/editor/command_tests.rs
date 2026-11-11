// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::editor::projection::byte_of_line;
use crate::editor::{
    CloseWhy, EditorBufferView, EditorEffect, EditorLines, KeyChord, KeyResolution, Keymap,
    Minibuffer, Platform, Profile, Selection, edit, editor, fill,
};
use crate::store::DocumentStore;

#[test]
fn repeat_count_is_the_clamp_it_replaced() {
    for value in [i64::MIN, -1, 0, 1, 2, 9_999, 10_000, 10_001, i64::MAX] {
        assert_eq!(repeat_count(value), value.clamp(1, 10_000));
    }
}

#[test]
fn universal_argument_is_consumed_once() {
    let text = (0..10)
        .map(|line| format!("line {line}\n"))
        .collect::<String>();
    let (mut store, mut workspace, mut view, document) = editor(&text);
    let press = |store: &mut DocumentStore,
                 workspace: &mut EditorWorkspace,
                 view: &mut EditorBufferView,
                 key: &str| {
        workspace
            .handle_chord(
                store,
                view,
                KeyChord::parse(key).expect("test chord parses"),
            )
            .unwrap()
    };

    press(&mut store, &mut workspace, &mut view, "C-u");
    workspace.insert_text(&mut store, &mut view, "4").unwrap();
    assert_eq!(
        view.prefix_hud.as_deref(),
        Some("C-u 4 — awaiting command (C-g cancels)")
    );
    press(&mut store, &mut workspace, &mut view, "C-n");
    assert_eq!(view.primary_selection().head, byte_of_line(&text, 4));
    assert_eq!(view.prefix_hud, None);
    press(&mut store, &mut workspace, &mut view, "C-f");
    assert_eq!(view.primary_selection().head, byte_of_line(&text, 4) + 1);
    assert_eq!(store.snapshot(document).unwrap().text.as_ref(), text);

    view.selections = vec![Selection::caret(0)];
    press(&mut store, &mut workspace, &mut view, "C-u");
    press(&mut store, &mut workspace, &mut view, "C-n");
    assert_eq!(view.primary_selection().head, byte_of_line(&text, 4));
    press(&mut store, &mut workspace, &mut view, "C-f");
    assert_eq!(view.primary_selection().head, byte_of_line(&text, 4) + 1);

    view.selections = vec![Selection::caret(0)];
    press(&mut store, &mut workspace, &mut view, "C-u");
    press(&mut store, &mut workspace, &mut view, "C-x");
    let hud = view.prefix_hud.clone().unwrap_or_default();
    assert!(hud.starts_with("C-u 4 C-x — "), "{hud}");
    let effects = press(&mut store, &mut workspace, &mut view, "C-s");
    assert!(matches!(
        effects.as_slice(),
        [EditorEffect::SaveDocument { .. }]
    ));
    press(&mut store, &mut workspace, &mut view, "C-f");
    assert_eq!(view.primary_selection().head, 1);
    assert_eq!(view.prefix_hud, None);

    for (digits, count) in [(None, 4), (Some("2"), 2)] {
        view.selections = vec![Selection::caret(0)];
        press(&mut store, &mut workspace, &mut view, "C-u");
        if let Some(digits) = digits {
            workspace
                .insert_text(&mut store, &mut view, digits)
                .unwrap();
        }
        let effects = press(&mut store, &mut workspace, &mut view, "M-x");
        assert_eq!(effects, [EditorEffect::ShowCommands]);
        workspace
            .insert_text(&mut store, &mut view, "forward-char")
            .unwrap();
        workspace.submit_minibuffer(&mut store, &mut view).unwrap();
        assert_eq!(view.primary_selection().head, count);
        assert_eq!(view.prefix_hud, None);
        press(&mut store, &mut workspace, &mut view, "C-f");
        assert_eq!(view.primary_selection().head, count + 1);
        assert_eq!(store.snapshot(document).unwrap().text.as_ref(), text);
    }

    view.selections = vec![Selection::caret(0)];
    press(&mut store, &mut workspace, &mut view, "C-u");
    press(&mut store, &mut workspace, &mut view, "M-x");
    workspace
        .insert_text(&mut store, &mut view, "save-buffer")
        .unwrap();
    let effects = workspace.submit_minibuffer(&mut store, &mut view).unwrap();
    assert!(matches!(
        effects.as_slice(),
        [EditorEffect::SaveDocument { .. }]
    ));
    press(&mut store, &mut workspace, &mut view, "C-f");
    assert_eq!(view.primary_selection().head, 1);
    assert_eq!(view.prefix_hud, None);

    view.selections = vec![Selection::caret(0)];
    press(&mut store, &mut workspace, &mut view, "C-u");
    press(&mut store, &mut workspace, &mut view, "M-x");
    press(&mut store, &mut workspace, &mut view, "C-g");
    press(&mut store, &mut workspace, &mut view, "C-f");
    assert_eq!(view.primary_selection().head, 1);
    assert_eq!(view.prefix_hud, None);
    assert_eq!(store.snapshot(document).unwrap().text.as_ref(), text);

    for keys in [&["M-z"][..], &["C-x", "z"][..]] {
        view.selections = vec![Selection::caret(0)];
        press(&mut store, &mut workspace, &mut view, "C-u");
        let mut effects = Vec::new();
        for &key in keys {
            effects = press(&mut store, &mut workspace, &mut view, key);
        }
        assert!(
            matches!(
                effects.as_slice(),
                [EditorEffect::Status(status)] if status.ends_with("is not bound")
            ),
            "C-u {keys:?}: {effects:?}"
        );
        assert_eq!(view.prefix_hud, None);
        press(&mut store, &mut workspace, &mut view, "C-f");
        assert_eq!(view.primary_selection().head, 1, "after C-u {keys:?}");
    }

    view.selections = vec![Selection::caret(0)];
    press(&mut store, &mut workspace, &mut view, "C-u");
    press(&mut store, &mut workspace, &mut view, "2");
    press(&mut store, &mut workspace, &mut view, "C-k");
    let after = store.snapshot(document).unwrap().text;
    assert_eq!(after.as_ref(), &text[byte_of_line(&text, 2)..]);
    assert_eq!(
        workspace.kill_ring.front().map(String::as_str),
        Some("line 0\nline 1\n")
    );

    workspace
        .execute(&mut store, &mut view, EditorCommand::UniversalArgument)
        .unwrap();
    workspace.insert_text(&mut store, &mut view, "x").unwrap();
    workspace
        .execute(&mut store, &mut view, EditorCommand::MoveForward)
        .unwrap();
    assert_eq!(view.primary_selection().head, 2);
    assert!(
        store
            .snapshot(document)
            .unwrap()
            .text
            .starts_with("xline 2\n")
    );
}

#[test]
fn a_second_universal_argument_closes_the_number() {
    let (mut store, mut workspace, mut view, document) = editor("ab");
    view.selections = vec![Selection::caret(1)];
    let universal = |store: &mut DocumentStore,
                     workspace: &mut EditorWorkspace,
                     view: &mut EditorBufferView| {
        workspace
            .execute(store, view, EditorCommand::UniversalArgument)
            .unwrap();
    };
    universal(&mut store, &mut workspace, &mut view);
    workspace.insert_text(&mut store, &mut view, "1").unwrap();
    universal(&mut store, &mut workspace, &mut view);
    assert_eq!(
        view.prefix_hud.as_deref(),
        Some("C-u 1 — awaiting command (C-g cancels)")
    );
    workspace.insert_text(&mut store, &mut view, "3").unwrap();
    assert_eq!(store.snapshot(document).unwrap().text.as_ref(), "a3b");
    assert_eq!(view.prefix_hud, None);

    universal(&mut store, &mut workspace, &mut view);
    workspace.insert_text(&mut store, &mut view, "2").unwrap();
    universal(&mut store, &mut workspace, &mut view);
    universal(&mut store, &mut workspace, &mut view);
    assert_eq!(
        view.prefix_hud.as_deref(),
        Some("C-u 4 — awaiting command (C-g cancels)")
    );
}

fn keys(
    store: &mut DocumentStore,
    workspace: &mut EditorWorkspace,
    view: &mut EditorBufferView,
    keys: &str,
) -> Vec<EditorEffect> {
    let mut effects = Vec::new();
    for key in keys.split_whitespace() {
        effects = workspace
            .handle_chord(
                store,
                view,
                KeyChord::parse(key).expect("test chord parses"),
            )
            .unwrap();
    }
    effects
}

fn text(store: &DocumentStore, document: crate::store::DocumentId) -> String {
    store.snapshot(document).unwrap().text.to_string()
}

#[test]
fn authority_table_is_total_and_pinned() {
    use CommandAuthority::{Clipboard, Edit, Read, Save};
    let pinned: &[(&str, CommandAuthority)] = &[
        ("save-buffer", Save),
        ("undo", Edit),
        ("redo", Edit),
        ("undo-agent-edit", Edit),
        ("isearch-forward", Read),
        ("search-next", Read),
        ("search-previous", Read),
        ("goto-line", Read),
        ("switch-to-buffer", Read),
        ("revert-buffer", Edit),
        ("keep-buffer-over-disk", Edit),
        ("toggle-grid-view", Read),
        ("grid-insert-row", Edit),
        ("grid-delete-row", Edit),
        ("set-mark-command", Read),
        ("kill-region", Edit),
        ("kill-line", Edit),
        ("kill-word", Edit),
        ("backward-kill-word", Edit),
        ("kill-ring-save", Clipboard),
        ("yank", Edit),
        ("yank-pop", Edit),
        ("finish-edit", Save),
        ("abort-edit", Save),
        ("kill-buffer", Save),
        ("save-buffers-kill-terminal", Save),
        ("backward-char", Read),
        ("forward-char", Read),
        ("previous-line", Read),
        ("next-line", Read),
        ("scroll-up-command", Read),
        ("scroll-down-command", Read),
        ("recenter-top-bottom", Read),
        ("move-beginning-of-line", Read),
        ("move-end-of-line", Read),
        ("backward-word", Read),
        ("forward-word", Read),
        ("beginning-of-buffer", Read),
        ("end-of-buffer", Read),
        ("exchange-point-and-mark", Read),
        ("mark-whole-buffer", Read),
        ("pop-to-mark-command", Read),
        ("backward-delete-char", Edit),
        ("delete-char", Edit),
        ("newline", Edit),
        ("open-line", Edit),
        ("transpose-chars", Edit),
        ("indent-for-tab-command", Edit),
        ("fill-paragraph", Edit),
        ("keyboard-quit", Read),
        ("universal-argument", Read),
        ("digit-argument", Read),
        ("execute-extended-command", Read),
        ("start-kbd-macro", Read),
        ("end-kbd-macro", Read),
        ("call-last-kbd-macro", Edit),
    ];
    let table = EditorCommand::ALL
        .iter()
        .map(|command| (command.name(), command.authority()))
        .collect::<Vec<_>>();
    assert_eq!(table, pinned, "the table lists every command, in M-x order");
    assert!(EditorCommand::ALL.iter().all(|command| !matches!(
        command.authority(),
        CommandAuthority::Files | CommandAuthority::Spawn
    )));
}

#[test]
fn from_name_round_trips_every_command() {
    let mut names = std::collections::BTreeSet::new();
    for command in EditorCommand::ALL {
        assert!(names.insert(command.name()), "{} twice", command.name());
        assert_eq!(EditorCommand::from_name(command.name()), Some(command));
    }
    assert_eq!(names.len(), EditorCommand::ALL.len());
    for near in [
        "kill",
        "Kill-Buffer",
        " kill-buffer",
        "kill-buffer ",
        "",
        "kill buffer",
    ] {
        assert_eq!(EditorCommand::from_name(near), None, "{near:?}");
    }
}

#[test]
fn m_x_lists_every_command() {
    for command in EditorCommand::ALL {
        assert_eq!(
            command_completions(command.name()).first(),
            Some(&command),
            "{}",
            command.name()
        );
    }
    assert_eq!(
        command_completions("kill"),
        [
            EditorCommand::KillRegion,
            EditorCommand::KillLine,
            EditorCommand::KillWord,
            EditorCommand::KillRingSave,
            EditorCommand::KillBuffer,
            EditorCommand::BackwardKillWord,
            EditorCommand::SaveBuffersKillTerminal,
        ]
    );
}

#[test]
fn an_invocation_carries_its_count_and_answers() {
    let text = "one\ntwo\nthree\nfour\n";
    let (mut store, mut workspace, mut view, document) = editor(text);
    let invocation = |command, count, args: &[&str]| Invocation {
        command,
        count,
        args: args.iter().map(|arg| arg.to_string()).collect(),
    };
    workspace
        .invoke(
            &mut store,
            &mut view,
            invocation(EditorCommand::MoveLineDown, Some(2), &[]),
        )
        .unwrap();
    assert_eq!(view.primary_selection().head, byte_of_line(text, 2));
    let effects = workspace
        .invoke(
            &mut store,
            &mut view,
            invocation(EditorCommand::GotoLine, None, &["4"]),
        )
        .unwrap();
    assert_eq!(effects, [EditorEffect::Status("Line 4".to_string())]);
    assert_eq!(view.primary_selection().head, byte_of_line(text, 3));
    assert!(!view.minibuffer_active());
    let effects = workspace
        .invoke(
            &mut store,
            &mut view,
            invocation(EditorCommand::KillBuffer, None, &[""]),
        )
        .unwrap();
    assert_eq!(
        effects,
        [EditorEffect::CloseView {
            why: CloseWhy::Plain
        }]
    );
    let effects = workspace
        .invoke(
            &mut store,
            &mut view,
            invocation(EditorCommand::KillBuffer, None, &["other.md"]),
        )
        .unwrap();
    assert_eq!(
        effects,
        [EditorEffect::KillBuffer {
            query: "other.md".to_string()
        }]
    );
    let effects = workspace
        .invoke(
            &mut store,
            &mut view,
            invocation(EditorCommand::Save, None, &["x"]),
        )
        .unwrap();
    assert_eq!(
        effects,
        [EditorEffect::Status(
            "save-buffer takes 0 arguments, not 1".to_string()
        )]
    );
    workspace
        .invoke(
            &mut store,
            &mut view,
            invocation(EditorCommand::KillBuffer, Some(3), &[]),
        )
        .unwrap();
    assert_eq!(
        view.minibuffer,
        Minibuffer::Prompt {
            invocation: invocation(EditorCommand::KillBuffer, Some(3), &[]),
            query: String::new(),
        }
    );
    assert_eq!(
        view.minibuffer.prompt_label(),
        Some("Kill buffer (default this buffer): ")
    );
    assert_eq!(text_of(&store, document), text);
}

fn text_of(store: &DocumentStore, document: crate::store::DocumentId) -> String {
    text(store, document)
}

#[test]
fn kill_buffer_asks_and_defaults_to_this_buffer() {
    let (mut store, mut workspace, mut view, document) = editor("keep");
    keys(&mut store, &mut workspace, &mut view, "C-x k");
    assert!(matches!(view.minibuffer, Minibuffer::Prompt { .. }));
    let effects = workspace.submit_minibuffer(&mut store, &mut view).unwrap();
    assert_eq!(
        effects,
        [EditorEffect::CloseView {
            why: CloseWhy::Plain
        }]
    );
    keys(&mut store, &mut workspace, &mut view, "C-x k");
    workspace
        .insert_text(&mut store, &mut view, "notes.md")
        .unwrap();
    workspace.minibuffer_backspace(&store, &mut view).unwrap();
    workspace.insert_text(&mut store, &mut view, "d").unwrap();
    let effects = workspace.submit_minibuffer(&mut store, &mut view).unwrap();
    assert_eq!(
        effects,
        [EditorEffect::KillBuffer {
            query: "notes.md".to_string()
        }]
    );
    assert!(!view.minibuffer_active());
    keys(&mut store, &mut workspace, &mut view, "C-x k C-g");
    assert!(!view.minibuffer_active());
    assert_eq!(text(&store, document), "keep");
    assert_eq!(workspace.kill_ring.len(), 0);
}

#[test]
fn closing_keys_ask_the_host_to_close_the_view() {
    let (mut store, mut workspace, mut view, document) = editor("text");
    for (sequence, why) in [
        ("C-x C-c", CloseWhy::Plain),
        ("C-c C-c", CloseWhy::Finish),
        ("C-x #", CloseWhy::Finish),
        ("C-c C-k", CloseWhy::Abort),
    ] {
        assert_eq!(
            keys(&mut store, &mut workspace, &mut view, sequence),
            [EditorEffect::CloseView { why }],
            "{sequence}"
        );
    }
    assert_eq!(text(&store, document), "text");
}

#[test]
fn buffer_ends_leave_the_mark_behind() {
    let (mut store, mut workspace, mut view, _) = editor("alpha\nbeta\ngamma");
    view.selections = vec![Selection::caret(7)];
    let effects = keys(&mut store, &mut workspace, &mut view, "M->");
    assert_eq!(effects, [EditorEffect::Status("Mark set".to_string())]);
    assert_eq!(view.selections, [Selection::caret(16)]);
    assert!(!view.mark_active);
    keys(&mut store, &mut workspace, &mut view, "C-u C-Space");
    assert_eq!(view.selections, [Selection::caret(7)]);
    keys(&mut store, &mut workspace, &mut view, "M-S-,");
    assert_eq!(view.selections, [Selection::caret(0)]);
    view.selections = vec![Selection::caret(7)];
    keys(&mut store, &mut workspace, &mut view, "C-Space M-<");
    assert_eq!(view.selections, [Selection { anchor: 7, head: 0 }]);
    assert!(view.mark_active);
    keys(&mut store, &mut workspace, &mut view, "M-S-.");
    assert_eq!(
        view.selections,
        [Selection {
            anchor: 7,
            head: 16
        }]
    );
}

#[test]
fn recenter_puts_the_line_middle_top_then_bottom() {
    let text = (0..100)
        .map(|line| format!("line {line}\n"))
        .collect::<String>();
    let (mut store, mut workspace, mut view, _) = editor(&text);
    view.reconcile_viewport(&text, 10);
    view.selections = vec![Selection::caret(byte_of_line(&text, 50))];
    let lines = EditorLines::scanning(&text);
    let top = |view: &EditorBufferView| lines.number_at(view.viewport_anchor);
    for expected in [45, 50, 41, 45] {
        keys(&mut store, &mut workspace, &mut view, "C-l");
        assert_eq!(top(&view), expected);
    }
    keys(&mut store, &mut workspace, &mut view, "C-l C-f C-b C-l");
    assert_eq!(top(&view), 45);
    keys(&mut store, &mut workspace, &mut view, "C-u 0 C-l");
    assert_eq!(top(&view), 50);
    keys(&mut store, &mut workspace, &mut view, "C-u 3 C-l");
    assert_eq!(top(&view), 47);
    view.selections = vec![Selection::caret(text.len())];
    keys(&mut store, &mut workspace, &mut view, "C-l C-l");
    assert_eq!(top(&view), 91);
    assert_eq!(
        view.primary_selection().head,
        text.len(),
        "C-l never moves the caret"
    );
}

#[test]
fn kill_word_kills_by_words() {
    let (mut store, mut workspace, mut view, document) = editor("alpha beta gamma delta");
    view.selections = vec![Selection::caret(6)];
    keys(&mut store, &mut workspace, &mut view, "M-d");
    assert_eq!(text(&store, document), "alpha  gamma delta");
    assert_eq!(view.selections, [Selection::caret(6)]);
    assert_eq!(
        workspace.kill_ring.front().map(String::as_str),
        Some("beta")
    );
    keys(&mut store, &mut workspace, &mut view, "C-e M-backspace");
    assert_eq!(text(&store, document), "alpha  gamma ");
    assert_eq!(
        workspace.kill_ring.front().map(String::as_str),
        Some("delta")
    );
    keys(&mut store, &mut workspace, &mut view, "C-a C-u 2 M-d");
    assert_eq!(text(&store, document), " ");
    assert_eq!(
        workspace.kill_ring.front().map(String::as_str),
        Some("alpha  gamma")
    );
    assert_eq!(
        keys(&mut store, &mut workspace, &mut view, "C-e M-d"),
        [EditorEffect::Status("End of buffer".to_string())]
    );
    let windows = Keymap::emacs(Profile::Gui(Platform::Windows));
    assert_eq!(
        windows.resolve(&[KeyChord::parse("C-backspace").unwrap()]),
        KeyResolution::Command(EditorCommand::BackwardKillWord)
    );
}

#[test]
fn kill_ring_save_copies_the_region() {
    let (mut store, mut workspace, mut view, document) = editor("copy this text");
    view.selections = vec![Selection::caret(5)];
    keys(&mut store, &mut workspace, &mut view, "C-Space M-f M-w");
    assert_eq!(text(&store, document), "copy this text");
    assert_eq!(
        workspace.kill_ring.front().map(String::as_str),
        Some("this")
    );
    assert_eq!(view.selections, [Selection::caret(9)]);
    assert!(!view.mark_active);
    assert_eq!(
        keys(&mut store, &mut workspace, &mut view, "M-w"),
        [EditorEffect::Status("No region selected".to_string())]
    );
    keys(&mut store, &mut workspace, &mut view, "C-a C-y");
    assert_eq!(text(&store, document), "thiscopy this text");
}

#[test]
fn kills_reach_the_clipboard_only_with_clipboard_authority() {
    let clipboard = |effects: Vec<EditorEffect>| {
        effects
            .into_iter()
            .filter_map(|effect| match effect {
                EditorEffect::KillToClipboard { text } => Some(text),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let (mut store, mut workspace, mut view, document) = editor("one\ntwo\n");
    assert_eq!(
        clipboard(keys(&mut store, &mut workspace, &mut view, "C-k")),
        ["one"]
    );
    assert_eq!(
        clipboard(keys(&mut store, &mut workspace, &mut view, "C-k")),
        ["one\n"]
    );
    assert!(clipboard(keys(&mut store, &mut workspace, &mut view, "C-f")).is_empty());
    view.selections = vec![Selection { anchor: 0, head: 3 }];
    assert_eq!(
        clipboard(keys(&mut store, &mut workspace, &mut view, "M-w")),
        ["two"]
    );

    workspace.granted = vec![
        CommandAuthority::Read,
        CommandAuthority::Edit,
        CommandAuthority::Save,
    ];
    assert!(clipboard(keys(&mut store, &mut workspace, &mut view, "C-k")).is_empty());
    assert_eq!(text(&store, document), "two");
    assert_eq!(workspace.guest_kill_ring, ["\n"]);
    assert_eq!(workspace.kill_ring.front().map(String::as_str), Some("two"));
    workspace.granted = CommandAuthority::ALL.to_vec();
    assert!(clipboard(keys(&mut store, &mut workspace, &mut view, "C-a")).is_empty());

    let (mut store, mut workspace, mut view, _) = editor("a b c d");
    keys(&mut store, &mut workspace, &mut view, "C-x ( M-d M-d C-x )");
    assert_eq!(
        clipboard(keys(&mut store, &mut workspace, &mut view, "C-x e")),
        [" c d"]
    );
    assert_eq!(workspace.kill_ring, [" c d", "a b"]);
}

/// INPUT WITHOUT CLIPBOARD AUTHORITY KEEPS A KILL RING OF ITS OWN: the kill ring holds what
/// the clipboard-holding input (the person) cut and copied, each of which reaches the
/// clipboard, so input without that authority (in aterm, a control client's) kills to and
/// yanks from a ring of its own: its C-y and M-y never reach the person's kill, and the
/// person's C-y never yanks its kills.
/// NEGATIVE CONTROL: against main's code at this leaf (`4d738a3c55`), one ring served both:
/// the guest's kill joined the person's `secret`, and its C-y yanked `secret mine`.
#[test]
fn input_without_clipboard_authority_keeps_a_kill_ring_of_its_own() {
    let (mut store, mut workspace, mut view, document) = editor("secret mine");
    keys(&mut store, &mut workspace, &mut view, "M-d");
    assert_eq!(text(&store, document), " mine");
    assert_eq!(workspace.kill_ring, ["secret"]);

    workspace.granted = vec![
        CommandAuthority::Read,
        CommandAuthority::Edit,
        CommandAuthority::Save,
    ];
    keys(&mut store, &mut workspace, &mut view, "M-d");
    assert_eq!(text(&store, document), "", "the guest's kill");
    keys(&mut store, &mut workspace, &mut view, "C-y M-y");
    assert_eq!(text(&store, document), " mine", "the guest's yanks");
    assert_eq!(workspace.kill_ring, ["secret"], "the person's ring");

    workspace.granted = CommandAuthority::ALL.to_vec();
    keys(&mut store, &mut workspace, &mut view, "C-a C-y");
    assert_eq!(text(&store, document), "secret mine", "the person's yank");
}

#[test]
fn a_command_runs_only_with_the_authority_it_needs() {
    let refusal = || {
        vec![EditorEffect::Status(
            "kill-ring-save needs clipboard authority, which this input does not carry".to_string(),
        )]
    };
    let (mut store, mut workspace, mut view, document) = editor("copy this text");
    keys(
        &mut store,
        &mut workspace,
        &mut view,
        "C-x ( C-Space M-f M-w C-x )",
    );
    assert_eq!(workspace.kill_ring.len(), 1);
    workspace.kill_ring.clear();
    workspace.granted = vec![
        CommandAuthority::Read,
        CommandAuthority::Edit,
        CommandAuthority::Save,
    ];
    view.selections = vec![Selection::caret(5)];
    assert_eq!(
        keys(&mut store, &mut workspace, &mut view, "C-Space M-f C-u M-w"),
        refusal()
    );
    assert_eq!(view.prefix_argument, None);
    assert!(view.mark_active, "the region survives the refusal");
    keys(&mut store, &mut workspace, &mut view, "M-x");
    workspace
        .insert_text(&mut store, &mut view, "kill-ring-save")
        .unwrap();
    assert_eq!(
        workspace.submit_minibuffer(&mut store, &mut view).unwrap(),
        refusal()
    );
    assert_eq!(
        workspace
            .invoke(
                &mut store,
                &mut view,
                Invocation::new(EditorCommand::KillRingSave)
            )
            .unwrap(),
        refusal()
    );
    // The person's macro needs what its steps need; the guest replays a macro of its own.
    assert_eq!(
        workspace.authorities_for(EditorCommand::PlayMacro, &CommandAuthority::ALL),
        [
            CommandAuthority::Read,
            CommandAuthority::Edit,
            CommandAuthority::Clipboard
        ]
    );
    assert_eq!(
        workspace.refusal(
            EditorCommand::PlayMacro,
            &[CommandAuthority::Edit, CommandAuthority::Clipboard]
        ),
        Some(
            "call-last-kbd-macro needs read authority, which this input does not carry".to_string()
        )
    );
    assert_eq!(
        workspace.authorities(EditorCommand::PlayMacro),
        [CommandAuthority::Edit]
    );
    assert_eq!(keys(&mut store, &mut workspace, &mut view, "C-x e"), vec![]);
    assert!(workspace.kill_ring.is_empty());
    keys(&mut store, &mut workspace, &mut view, "C-w");
    assert_eq!(text(&store, document), "copy  text");
    workspace.granted = CommandAuthority::ALL.to_vec();
    keys(&mut store, &mut workspace, &mut view, "C-a C-Space M-f M-w");
    assert_eq!(
        workspace.kill_ring.front().map(String::as_str),
        Some("copy")
    );
}

#[test]
fn exchange_point_and_mark_swaps_them() {
    let (mut store, mut workspace, mut view, document) = editor("one two three");
    assert_eq!(
        keys(&mut store, &mut workspace, &mut view, "C-x C-x"),
        [EditorEffect::Status(
            "No mark set in this buffer".to_string()
        )]
    );
    view.selections = vec![Selection::caret(4)];
    keys(&mut store, &mut workspace, &mut view, "C-Space M-f M-f");
    assert_eq!(
        view.selections,
        [Selection {
            anchor: 4,
            head: 13
        }]
    );
    keys(&mut store, &mut workspace, &mut view, "C-x C-x");
    assert_eq!(
        view.selections,
        [Selection {
            anchor: 13,
            head: 4
        }]
    );
    assert!(view.mark_active);
    keys(&mut store, &mut workspace, &mut view, "C-g C-e C-x C-x");
    assert_eq!(
        view.selections,
        [Selection {
            anchor: 13,
            head: 4
        }]
    );
    assert!(view.mark_active);
    assert_eq!(text(&store, document), "one two three");
}

#[test]
fn mark_whole_buffer_selects_everything() {
    let (mut store, mut workspace, mut view, document) = editor("all\nof it");
    view.selections = vec![Selection::caret(5)];
    let effects = keys(&mut store, &mut workspace, &mut view, "C-x h");
    assert_eq!(effects, [EditorEffect::Status("Mark set".to_string())]);
    assert_eq!(view.selections, [Selection { anchor: 9, head: 0 }]);
    assert!(view.mark_active);
    keys(&mut store, &mut workspace, &mut view, "C-w");
    assert_eq!(text(&store, document), "");
    assert_eq!(
        workspace.kill_ring.front().map(String::as_str),
        Some("all\nof it")
    );
    assert_eq!(
        workspace.global_mark_ring.front().map(|mark| mark.offset),
        Some(0)
    );
}

#[test]
fn pop_mark_cycles_through_this_buffers_marks() {
    let (mut store, mut workspace, mut view, document) = editor("a b c d e");
    assert_eq!(
        keys(&mut store, &mut workspace, &mut view, "C-u C-Space"),
        [EditorEffect::Status(
            "No mark set in this buffer".to_string()
        )]
    );
    for at in [2, 6] {
        view.selections = vec![Selection::caret(at)];
        keys(&mut store, &mut workspace, &mut view, "C-Space C-g");
    }
    view.selections = vec![Selection::caret(0)];
    workspace.insert_text(&mut store, &mut view, "__").unwrap();
    assert_eq!(text(&store, document), "__a b c d e");
    for expected in [8, 4, 8] {
        keys(&mut store, &mut workspace, &mut view, "C-u C-Space");
        assert_eq!(view.selections, [Selection::caret(expected)]);
        assert!(!view.mark_active);
    }
    workspace
        .execute(&mut store, &mut view, EditorCommand::PopMark)
        .unwrap();
    assert_eq!(view.selections, [Selection::caret(4)]);
}

#[test]
fn open_line_and_newline_break_the_line() {
    let (mut store, mut workspace, mut view, document) = editor("ab");
    view.selections = vec![Selection::caret(1)];
    keys(&mut store, &mut workspace, &mut view, "C-o");
    assert_eq!(text(&store, document), "a\nb");
    assert_eq!(view.selections, [Selection::caret(1)]);
    keys(&mut store, &mut workspace, &mut view, "C-u 2 C-o");
    assert_eq!(text(&store, document), "a\n\n\nb");
    assert_eq!(view.selections, [Selection::caret(1)]);
    workspace
        .execute(&mut store, &mut view, EditorCommand::Newline)
        .unwrap();
    assert_eq!(text(&store, document), "a\n\n\n\nb");
    assert_eq!(view.selections, [Selection::caret(2)]);
    keys(&mut store, &mut workspace, &mut view, "C-u 2");
    workspace
        .execute(&mut store, &mut view, EditorCommand::Newline)
        .unwrap();
    assert_eq!(text(&store, document), "a\n\n\n\n\n\nb");
    assert_eq!(view.selections, [Selection::caret(4)]);
    keys(&mut store, &mut workspace, &mut view, "C-/");
    assert_eq!(text(&store, document), "a\n\n\n\nb");

    let (mut store, mut workspace, mut view, document) = editor("ab\r\ncd\r\n");
    view.selections = vec![Selection::caret(1)];
    keys(&mut store, &mut workspace, &mut view, "C-o");
    assert_eq!(text(&store, document), "a\r\nb\r\ncd\r\n");
    assert_eq!(view.selections, [Selection::caret(1)]);
    workspace
        .execute(&mut store, &mut view, EditorCommand::Newline)
        .unwrap();
    assert_eq!(text(&store, document), "a\r\n\r\nb\r\ncd\r\n");
    assert_eq!(view.selections, [Selection::caret(3)]);
}

#[test]
fn crlf_line_ends_stop_before_the_carriage_return() {
    let (mut store, mut workspace, mut view, document) = editor("abc\r\nd\r\nefg");
    keys(&mut store, &mut workspace, &mut view, "C-e");
    assert_eq!(view.selections, [Selection::caret(3)]);
    keys(&mut store, &mut workspace, &mut view, "C-n");
    assert_eq!(view.selections, [Selection::caret(6)]);
    keys(&mut store, &mut workspace, &mut view, "C-n");
    assert_eq!(view.selections, [Selection::caret(11)]);
    keys(&mut store, &mut workspace, &mut view, "C-p C-p");
    assert_eq!(view.selections, [Selection::caret(3)]);
    workspace.insert_text(&mut store, &mut view, "x").unwrap();
    assert_eq!(text(&store, document), "abcx\r\nd\r\nefg");

    let (mut store, mut workspace, mut view, document) = editor("ab\r\ncd\r\n");
    keys(&mut store, &mut workspace, &mut view, "C-k");
    assert_eq!(text(&store, document), "\r\ncd\r\n");
    keys(&mut store, &mut workspace, &mut view, "C-k");
    assert_eq!(text(&store, document), "cd\r\n");
    assert_eq!(
        workspace.kill_ring.front().map(String::as_str),
        Some("ab\r\n")
    );

    let (mut store, mut workspace, mut view, document) = editor("ab\r\ncd");
    keys(&mut store, &mut workspace, &mut view, "C-e C-t");
    assert_eq!(text(&store, document), "ba\r\ncd");
    assert_eq!(view.selections, [Selection::caret(2)]);
}

#[test]
fn transpose_chars_swaps_around_the_caret() {
    let (mut store, mut workspace, mut view, document) = editor("abcd\nxy");
    view.selections = vec![Selection::caret(1)];
    keys(&mut store, &mut workspace, &mut view, "C-t");
    assert_eq!(text(&store, document), "bacd\nxy");
    assert_eq!(view.selections, [Selection::caret(2)]);
    keys(&mut store, &mut workspace, &mut view, "C-e C-t");
    assert_eq!(text(&store, document), "badc\nxy");
    assert_eq!(view.selections, [Selection::caret(4)]);
    view.selections = vec![Selection::caret(1)];
    keys(&mut store, &mut workspace, &mut view, "C-u 2 C-t");
    assert_eq!(text(&store, document), "adbc\nxy");
    assert_eq!(view.selections, [Selection::caret(3)]);
    view.selections = vec![Selection::caret(0)];
    assert_eq!(
        keys(&mut store, &mut workspace, &mut view, "C-t"),
        [EditorEffect::Status("Beginning of buffer".to_string())]
    );
    let (mut store, mut workspace, mut view, document) = editor("e\u{301}x");
    view.selections = vec![Selection::caret(3)];
    keys(&mut store, &mut workspace, &mut view, "C-t");
    assert_eq!(text(&store, document), "xe\u{301}");
}

#[test]
fn tab_reads_the_indent_options() {
    let (mut store, mut workspace, mut view, document) = editor("ab");
    workspace.options.tab_width = 8;
    view.selections = vec![Selection::caret(2)];
    keys(&mut store, &mut workspace, &mut view, "Tab");
    assert_eq!(text(&store, document), "ab      ");
    workspace.options.indent_tabs = true;
    keys(&mut store, &mut workspace, &mut view, "Tab");
    assert_eq!(text(&store, document), "ab      \t");
    assert_eq!(edit::tab_stop_columns(5, 4), 3);
    assert_eq!(edit::tab_stop_columns(8, 4), 4);
    assert_eq!(
        edit::tab_stop_columns(3, 0),
        1,
        "a zero width is one column"
    );
}

#[test]
fn fill_paragraph_fills_to_the_fill_column() {
    let text = "Subject\n\nThe body of a commit message that someone typed as one very long line without any breaks.\n\nNext paragraph.\n";
    let (mut store, mut workspace, mut view, document) = editor(text);
    workspace.options.fill_column = 30;
    let caret = text.find("someone").unwrap() + 2;
    view.selections = vec![Selection::caret(caret)];
    keys(&mut store, &mut workspace, &mut view, "M-q");
    let filled = text_of(&store, document);
    assert_eq!(
        filled,
        "Subject\n\nThe body of a commit message\nthat someone typed as one very\nlong line without any breaks.\n\nNext paragraph.\n"
    );
    assert_eq!(&filled[view.primary_selection().head..][..5], "meone");
    let seq = store.snapshot(document).unwrap().seq;
    keys(&mut store, &mut workspace, &mut view, "M-q");
    assert_eq!(store.snapshot(document).unwrap().seq, seq);
    keys(&mut store, &mut workspace, &mut view, "C-/");
    assert_eq!(text_of(&store, document), text);

    let comment = "    // A comment that runs on and on past the column\n    // and then some more.\nfn main() {}\n";
    let (mut store, mut workspace, mut view, document) = editor(comment);
    workspace.options.fill_column = 32;
    view.selections = vec![Selection::caret(10)];
    keys(&mut store, &mut workspace, &mut view, "M-q");
    assert_eq!(
        text_of(&store, document),
        "    // A comment that runs on\n    // and on past the column\n    // and then some more.\nfn main() {}\n"
    );

    let list = "- an item whose text is long enough to wrap\n- a second item\n";
    let (mut store, mut workspace, mut view, document) = editor(list);
    workspace.options.fill_column = 20;
    keys(&mut store, &mut workspace, &mut view, "M-q");
    assert_eq!(
        text_of(&store, document),
        "- an item whose text\n  is long enough to\n  wrap\n- a second item\n"
    );

    let crlf = "aaa bbb\r\nccc ddd\r\n\r\nnext\r\n";
    let (mut store, mut workspace, mut view, document) = editor(crlf);
    keys(&mut store, &mut workspace, &mut view, "M-q");
    assert_eq!(text_of(&store, document), "aaa bbb ccc ddd\r\n\r\nnext\r\n");
    workspace.options.fill_column = 8;
    keys(&mut store, &mut workspace, &mut view, "M-q");
    assert_eq!(text_of(&store, document), crlf);
    let (mut store, mut workspace, mut view, document) = editor("x\r\n\r\nthree four\r");
    workspace.options.fill_column = 6;
    view.selections = vec![Selection::caret(6)];
    keys(&mut store, &mut workspace, &mut view, "M-q");
    assert_eq!(text_of(&store, document), "x\r\n\r\nthree\r\nfour\r");

    let (mut store, mut workspace, mut view, _) = editor("a\n\nb");
    view.selections = vec![Selection::caret(2)];
    assert_eq!(
        keys(&mut store, &mut workspace, &mut view, "M-q"),
        [EditorEffect::Status(
            "No paragraph to fill here".to_string()
        )]
    );
}

#[test]
fn word_fits_is_room_for_a_space_and_the_word() {
    let fits =
        |line: u64, word: u64, column: u64| fill::word_fits(column.saturating_sub(line), word);
    for line in 0..12_u64 {
        for word in 0..12_u64 {
            for column in 0..12_u64 {
                assert_eq!(
                    fits(line, word, column),
                    u128::from(line) + 1 + u128::from(word) <= u128::from(column),
                    "{line} {word} {column}"
                );
            }
        }
    }
    assert!(!fits(u64::MAX, u64::MAX, u64::MAX));
    assert!(fits(0, u64::MAX - 1, u64::MAX));
    assert!(!fits(1, u64::MAX - 1, u64::MAX));
}

#[test]
fn redo_key_and_digit_argument() {
    let (mut store, mut workspace, mut view, document) = editor("");
    workspace.insert_text(&mut store, &mut view, "x").unwrap();
    keys(&mut store, &mut workspace, &mut view, "C-/");
    assert_eq!(text(&store, document), "");
    keys(&mut store, &mut workspace, &mut view, "C-?");
    assert_eq!(text(&store, document), "x");
    keys(&mut store, &mut workspace, &mut view, "C-/ C-S-/");
    assert_eq!(text(&store, document), "x");

    let text = "abcdefghijklmnopqrstuvwxyz";
    let (mut store, mut workspace, mut view, _) = editor(text);
    keys(&mut store, &mut workspace, &mut view, "M-3");
    assert_eq!(
        view.prefix_hud.as_deref(),
        Some("C-u 3 — awaiting command (C-g cancels)")
    );
    keys(&mut store, &mut workspace, &mut view, "C-f");
    assert_eq!(view.primary_selection().head, 3);
    keys(&mut store, &mut workspace, &mut view, "M-1 2 C-f");
    assert_eq!(view.primary_selection().head, 15);
    keys(&mut store, &mut workspace, &mut view, "M-1 M-0 C-b");
    assert_eq!(view.primary_selection().head, 5);
    assert_eq!(view.prefix_hud, None);
    assert_eq!(
        workspace
            .execute(&mut store, &mut view, EditorCommand::DigitArgument)
            .unwrap(),
        [EditorEffect::Status(
            "digit-argument is typed as M-0 … M-9".to_string()
        )]
    );
}

#[test]
fn command_completion_is_bounded_prefix_first_and_fuzzy_typed() {
    let broad = command_completions("");
    assert_eq!(broad.len(), COMMAND_COMPLETION_LIMIT);
    assert_eq!(broad[0], EditorCommand::Save);
    assert_eq!(command_completions("save")[0], EditorCommand::Save);
    assert_eq!(command_completions("svb")[0], EditorCommand::Save);
    let move_commands = command_completions("move-");
    assert_eq!(
        move_commands,
        [EditorCommand::MoveLineStart, EditorCommand::MoveLineEnd]
    );
    assert!(command_completions("definitely-not-a-command").is_empty());
}
