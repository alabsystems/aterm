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
#[path = "macros_tests.rs"]
mod tests;
