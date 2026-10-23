// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Keyboard macros: recording and replaying the last macro.

use super::command::{EditorCommand, PrefixArgument};
use super::view::EditorBufferView;
use super::workspace::{EditorEffect, EditorError, EditorWorkspace, Recorded};
use crate::store::DocumentStore;

/// One recorded step of a keyboard macro.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum MacroStep {
    Command(EditorCommand, Option<PrefixArgument>, Vec<String>),
    Text(String),
}

impl EditorWorkspace {
    /// `start-kbd-macro` (`C-x (`): record what follows as the next macro.
    pub(super) fn start_kbd_macro(&mut self) -> Vec<EditorEffect> {
        self.recording = Some(Vec::new());
        vec![EditorEffect::Status("Defining keyboard macro…".into())]
    }

    /// `end-kbd-macro` (C-x )): keep what was recorded as the last macro.
    pub(super) fn end_kbd_macro(&mut self) -> Vec<EditorEffect> {
        if let Some(recording) = self.recording.take() {
            self.last_macro = recording;
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
        let steps = self.last_macro.clone();
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
    use crate::editor::{EditorWorkspace, Invocation, KeyChord, Selection, editor};
    use crate::store::DocumentStore;

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
