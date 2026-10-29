// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Keyboard macros: recording and replaying the last macro.

use super::command::{CommandAuthority, EditorCommand, PrefixArgument};
use super::view::EditorBufferView;
use super::workspace::{EditorEffect, EditorError, EditorWorkspace, Recorded};
use crate::store::DocumentStore;

/// One recorded step of a keyboard macro.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum MacroStep {
    Command(EditorCommand, Option<PrefixArgument>, Vec<String>),
    Text(String),
}

/// One input's keyboard macros: the one it is recording, and the last it defined.
#[derive(Debug, Default)]
pub(super) struct KbdMacros {
    pub(super) recording: Option<Vec<MacroStep>>,
    pub(super) last: Vec<MacroStep>,
}

impl EditorWorkspace {
    /// The macros input that carries `granted` records and replays: the person's
    /// ([`EditorWorkspace::macros`]) for input with [`CommandAuthority::Clipboard`], as for their
    /// kill ring ([`EditorWorkspace::kills`]), else [`EditorWorkspace::guest_macros`]. A macro
    /// keeps the text typed while it was recorded, so input without that authority (in aterm, a
    /// control client's) never replays the person's, and its keys never join it.
    pub(super) fn macros_for(&self, granted: &[CommandAuthority]) -> &KbdMacros {
        if granted.contains(&CommandAuthority::Clipboard) {
            &self.macros
        } else {
            &self.guest_macros
        }
    }

    /// [`Self::macros_for`] the input being reduced, to change.
    pub(super) fn macros_mut(&mut self) -> &mut KbdMacros {
        if self.granted.contains(&CommandAuthority::Clipboard) {
            &mut self.macros
        } else {
            &mut self.guest_macros
        }
    }

    /// `start-kbd-macro` (`C-x (`): record what follows as the next macro.
    pub(super) fn start_kbd_macro(&mut self) -> Vec<EditorEffect> {
        self.macros_mut().recording = Some(Vec::new());
        vec![EditorEffect::Status("Defining keyboard macro…".into())]
    }

    /// `end-kbd-macro` (C-x )): keep what was recorded as the last macro.
    pub(super) fn end_kbd_macro(&mut self) -> Vec<EditorEffect> {
        let macros = self.macros_mut();
        if let Some(recording) = macros.recording.take() {
            macros.last = recording;
        }
        vec![EditorEffect::Status("Keyboard macro defined".into())]
    }

    /// `call-last-kbd-macro` (C-x e): replays the last macro's steps and returns host actions.
    pub(super) fn play_macro(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
    ) -> Result<Vec<EditorEffect>, EditorError> {
        if self.playing_macro {
            return Err(EditorError::MacroRecursion);
        }
        self.playing_macro = true;
        let steps = self.macros_for(&self.granted).last.clone();
        let result = (|| {
            let mut effects = Vec::new();
            for step in steps {
                match step {
                    MacroStep::Command(command, argument, args) => {
                        view.prefix_argument = argument;
                        let result = self.execute_with(store, view, command, args, Recorded::No);
                        view.take_prefix_argument();
                        effects.extend(result?);
                    }
                    MacroStep::Text(text) => {
                        effects.extend(self.insert_text(store, view, &text)?);
                    }
                }
            }
            Ok(effects)
        })();
        self.playing_macro = false;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::projection::byte_of_line;
    use crate::editor::{
        CommandAuthority, EditorWorkspace, Invocation, KeyChord, Selection, editor,
    };
    use crate::store::DocumentStore;

    /// A KEYBOARD MACRO IS REPLAYED ONLY BY THE INPUT THAT RECORDED IT: the person's macro keeps
    /// what they typed while recording it (a password, say), as private as their kills, so input
    /// without clipboard authority (in aterm, a control client's) records and replays a macro of
    /// its own. Its C-x e never replays the person's text into a document it reads; its keys
    /// while the person records never join the person's macro, to run later with the person's
    /// authority; its own macro never replaces theirs; and the person's C-x e replays their own.
    /// NEGATIVE CONTROL: against main's code at this leaf (`147b22abdc`), one macro served both:
    /// the guest's C-x e replayed the person's `pass` and the guest's own `G` typed mid-recording
    /// (`passGpassG`); so it did again with `macros_for` and `macros_mut` mutated to pick the
    /// person's macros for every input.
    #[test]
    fn a_keyboard_macro_is_replayed_only_by_the_input_that_recorded_it() {
        fn press(
            store: &mut DocumentStore,
            workspace: &mut EditorWorkspace,
            view: &mut EditorBufferView,
            keys: &str,
        ) {
            for key in keys.split_whitespace() {
                workspace
                    .handle_chord(store, view, KeyChord::parse(key).unwrap())
                    .unwrap();
            }
        }
        let guest = || {
            vec![
                CommandAuthority::Read,
                CommandAuthority::Edit,
                CommandAuthority::Save,
            ]
        };
        let (mut store, mut workspace, mut view, document) = editor("");
        let text = |store: &DocumentStore| store.snapshot(document).unwrap().text.to_string();

        press(&mut store, &mut workspace, &mut view, "C-x (");
        workspace
            .insert_text(&mut store, &mut view, "pass")
            .unwrap();
        workspace.granted = guest();
        workspace.insert_text(&mut store, &mut view, "G").unwrap();
        workspace.granted = CommandAuthority::ALL.to_vec();
        press(&mut store, &mut workspace, &mut view, "C-x )");
        assert_eq!(text(&store), "passG", "PRECONDITION: both typed");

        workspace.granted = guest();
        press(&mut store, &mut workspace, &mut view, "C-x e");
        assert_eq!(text(&store), "passG", "the guest's C-x e");
        press(&mut store, &mut workspace, &mut view, "C-x (");
        workspace.insert_text(&mut store, &mut view, "x").unwrap();
        press(&mut store, &mut workspace, &mut view, "C-x ) C-x e");
        assert_eq!(text(&store), "passGxx", "the guest's own macro");

        workspace.granted = CommandAuthority::ALL.to_vec();
        press(&mut store, &mut workspace, &mut view, "C-x e");
        assert_eq!(text(&store), "passGxxpass", "the person's C-x e");
    }

    #[test]
    fn macro_publication_rebases_siblings_through_every_recorded_edit() {
        for parts in [["x", "y"], ["🦀", "λ"]] {
            for replay_at_start in [true, false] {
                let (mut store, mut workspace, mut writer, document) = editor("needle tail");
                let mut reader = workspace
                    .attach(&mut store, document, crate::store::DocumentViewId(92))
                    .unwrap();
                workspace
                    .execute(&mut store, &mut writer, EditorCommand::StartMacro)
                    .unwrap();
                for text in parts {
                    workspace
                        .insert_text(&mut store, &mut writer, text)
                        .unwrap();
                    for (seq, deltas) in workspace.take_commits(document) {
                        reader.observe_external(seq, &deltas);
                    }
                }
                workspace
                    .execute(&mut store, &mut writer, EditorCommand::EndMacro)
                    .unwrap();
                let prefix = parts.concat();
                reader.selections = vec![Selection {
                    anchor: prefix.len(),
                    head: prefix.len() + "needle".len(),
                }];
                assert_eq!(
                    store
                        .snapshot(document)
                        .unwrap()
                        .text
                        .get(reader.primary_selection().range()),
                    Some("needle")
                );
                if replay_at_start {
                    workspace
                        .execute(&mut store, &mut writer, EditorCommand::BeginningOfBuffer)
                        .unwrap();
                }
                workspace
                    .execute(&mut store, &mut writer, EditorCommand::PlayMacro)
                    .unwrap();
                for (seq, deltas) in workspace.take_commits(document) {
                    reader.observe_external(seq, &deltas);
                }
                let snapshot = store.snapshot(document).unwrap();
                assert_eq!(
                    snapshot.text.as_ref(),
                    format!("{prefix}{prefix}needle tail")
                );
                assert_eq!(reader.anchor_seq(), snapshot.seq);
                assert_eq!(
                    snapshot.text.get(reader.primary_selection().range()),
                    Some("needle"),
                    "parts={parts:?}, replay_at_start={replay_at_start}, reader={:?}",
                    reader.primary_selection()
                );
                assert_eq!(
                    reader.primary_selection().range(),
                    2 * prefix.len()..2 * prefix.len() + "needle".len()
                );
            }
        }
    }

    #[test]
    fn a_macro_replays_each_commands_own_argument() {
        let text = (0..10)
            .map(|line| format!("line {line}\n"))
            .collect::<String>();
        let press = |store: &mut DocumentStore,
                     workspace: &mut EditorWorkspace,
                     view: &mut EditorBufferView,
                     keys: &str| {
            for key in keys.split_whitespace() {
                workspace
                    .handle_chord(
                        store,
                        view,
                        KeyChord::parse(key).expect("test chord parses"),
                    )
                    .unwrap();
            }
        };

        for digit_as_text in [true, false] {
            let (mut store, mut workspace, mut view, document) = editor(&text);
            press(&mut store, &mut workspace, &mut view, "C-x ( C-u");
            if digit_as_text {
                workspace.insert_text(&mut store, &mut view, "2").unwrap();
            } else {
                press(&mut store, &mut workspace, &mut view, "2");
            }
            press(&mut store, &mut workspace, &mut view, "C-n C-x )");
            assert_eq!(view.primary_selection().head, byte_of_line(&text, 2));
            press(&mut store, &mut workspace, &mut view, "C-x e");
            assert_eq!(
                view.primary_selection().head,
                byte_of_line(&text, 4),
                "digit as text: {digit_as_text}"
            );
            assert_eq!(store.snapshot(document).unwrap().text.as_ref(), text);
        }

        let (mut store, mut workspace, mut view, _) = editor(&text);
        press(&mut store, &mut workspace, &mut view, "C-x ( C-f C-u C-x )");
        assert_eq!(view.primary_selection().head, 1);
        press(&mut store, &mut workspace, &mut view, "C-x e");
        assert_eq!(view.primary_selection().head, 2);
        assert_eq!(view.prefix_hud, None);
        press(&mut store, &mut workspace, &mut view, "C-f");
        assert_eq!(view.primary_selection().head, 3);
    }

    #[test]
    fn a_macro_replays_its_prompts_answers() {
        fn press(
            store: &mut DocumentStore,
            workspace: &mut EditorWorkspace,
            view: &mut EditorBufferView,
            keys: &str,
        ) {
            for key in keys.split_whitespace() {
                workspace
                    .handle_chord(store, view, KeyChord::parse(key).unwrap())
                    .unwrap();
            }
        }
        let text = (0..10)
            .map(|line| format!("line {line}\n"))
            .collect::<String>();
        let (mut store, mut workspace, mut view, _) = editor(&text);
        let answer = |store: &mut DocumentStore,
                      workspace: &mut EditorWorkspace,
                      view: &mut EditorBufferView,
                      keys: &str,
                      answer: &str| {
            press(store, workspace, view, keys);
            workspace.insert_text(store, view, answer).unwrap();
            workspace.submit_minibuffer(store, view).unwrap()
        };
        press(&mut store, &mut workspace, &mut view, "C-x (");
        answer(&mut store, &mut workspace, &mut view, "M-g g", "3");
        answer(
            &mut store,
            &mut workspace,
            &mut view,
            "C-x b",
            "mem://notes",
        );
        answer(&mut store, &mut workspace, &mut view, "C-x k", "mem://old");
        answer(&mut store, &mut workspace, &mut view, "M-x", "forward-char");
        press(&mut store, &mut workspace, &mut view, "C-x )");
        assert_eq!(view.primary_selection().head, byte_of_line(&text, 2) + 1);

        view.selections = vec![Selection::caret(0)];
        press(&mut store, &mut workspace, &mut view, "C-x");
        let effects = workspace
            .handle_chord(&mut store, &mut view, KeyChord::parse("e").unwrap())
            .unwrap();
        assert!(!view.minibuffer_active(), "{:?}", view.minibuffer);
        assert_eq!(view.primary_selection().head, byte_of_line(&text, 2) + 1);
        assert_eq!(
            effects,
            [
                EditorEffect::Status("Line 3".to_string()),
                EditorEffect::SwitchBuffer {
                    query: "mem://notes".to_string()
                },
                EditorEffect::KillBuffer {
                    query: "mem://old".to_string()
                },
            ]
        );

        let (mut store, mut workspace, mut view, _) = editor(&text);
        press(&mut store, &mut workspace, &mut view, "C-x (");
        workspace
            .invoke(
                &mut store,
                &mut view,
                Invocation {
                    command: EditorCommand::GotoLine,
                    count: None,
                    args: vec!["5".to_string()],
                },
            )
            .unwrap();
        press(&mut store, &mut workspace, &mut view, "C-x )");
        view.selections = vec![Selection::caret(0)];
        press(&mut store, &mut workspace, &mut view, "C-x e");
        assert_eq!(view.primary_selection().head, byte_of_line(&text, 4));
    }
}
