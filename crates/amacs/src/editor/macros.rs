// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Keyboard macros: the one last macro, recorded command by command with each
//! command's own argument, and its replay.

use super::command::{EditorCommand, PrefixArgument};
use super::view::EditorBufferView;
use super::workspace::{EditorEffect, EditorError, EditorWorkspace};
use crate::store::DocumentStore;

/// One recorded step of a keyboard macro. A command carries the `C-u`
/// argument it took, so neither `C-u` nor the digits after it are steps of
/// their own: an argument nothing took (one pending at `C-x )`) records
/// nothing, and a replay hands each command exactly the argument it had.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum MacroStep {
    Command(EditorCommand, Option<PrefixArgument>),
    Text(String),
}

impl EditorWorkspace {
    /// `start-kbd-macro` (C-x (): record what follows as the next macro.
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

    pub(super) fn play_macro(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
    ) -> Result<(), EditorError> {
        if self.playing_macro {
            return Err(EditorError::MacroRecursion);
        }
        self.playing_macro = true;
        let steps = self.last_macro.clone();
        let result = (|| {
            for step in steps {
                match step {
                    MacroStep::Command(command, argument) => {
                        // The step hands its command the argument it took when
                        // recorded, and the argument never outlives the step,
                        // even when the command refuses to run.
                        view.prefix_argument = argument;
                        let result = self.execute(store, view, command);
                        view.take_prefix_argument();
                        result?;
                    }
                    MacroStep::Text(text) => {
                        self.insert_text(store, view, &text)?;
                    }
                }
            }
            Ok(())
        })();
        self.playing_macro = false;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::projection::byte_of_line;
    use crate::editor::{EditorWorkspace, KeyChord, editor};
    use crate::store::DocumentStore;

    /// A keyboard macro records the argument with the command that took it.
    /// The digits after C-u replay as the argument whether they arrived as text
    /// or as a key, and a C-u still pending at C-x ) is no part of the macro.
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

        // C-u 2 C-n, the digit sent as text (as the host sends it) and then as
        // a key: each records and replays a two-line move.
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

        // C-x ( C-f C-u C-x ): the trailing C-u argued with C-x ), not with the
        // macro, so C-x e moves one character and leaves nothing pending.
        let (mut store, mut workspace, mut view, _) = editor(&text);
        press(&mut store, &mut workspace, &mut view, "C-x ( C-f C-u C-x )");
        assert_eq!(view.primary_selection().head, 1);
        press(&mut store, &mut workspace, &mut view, "C-x e");
        assert_eq!(view.primary_selection().head, 2);
        assert_eq!(view.prefix_hud, None);
        press(&mut store, &mut workspace, &mut view, "C-f");
        assert_eq!(view.primary_selection().head, 3);
    }
}
