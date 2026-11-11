// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The shared input reducer (`EditorInput`).

use aterm_types::keyboard::{Key, Modifiers, NamedKey};

use crate::editor::{
    EditorBufferView, EditorCommand, EditorCompletionAction, EditorEffect, EditorError,
    EditorWorkspace, KeyChord, Minibuffer, Selection,
};
use crate::store::DocumentStore;

/// One input a face hands the engine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditorInput {
    /// A key press or auto-repeat (a face never sends a release).
    Key {
        key: Key,
        mods: Modifiers,
        text: Option<String>,
    },
    /// Committed text that is not one key.
    Text(String),
    /// A paste: inserted as one transaction, never read as keys, so a newline in it is a newline
    /// and never submits a minibuffer.
    Paste(String),
    /// A pointer press or drag at the document byte the face's hit test found.
    Pointer {
        byte: usize,
        extend: bool,
        visible_lines: usize,
    },
    /// A press on a grid cell a face's hit test found; `open` (a double-click) opens its editor.
    GridPointer {
        record: usize,
        column: usize,
        open: bool,
    },
    /// A sideways scroll of a grid by whole columns (a sideways wheel or swipe, or a press on a
    /// `◂`/`▸` mark): negative shows the columns to the left.
    GridScroll { columns: i32 },
    /// A press in a grid's cell panel at byte `byte` of the value it shows
    /// ([`crate::editor::GridProjection::panel_byte`]): the caret goes there, the cell's editor
    /// opening first when it was closed.
    GridPanelPointer { byte: usize },
    /// The view gained (`true`) or lost keyboard focus.
    Focus(bool),
}

/// What one input did, for the face that delivered it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reduction {
    /// What the face carries out, in order: a save, a buffer switch, a status line.
    pub effects: Vec<EditorEffect>,
    /// How the face follows up.
    pub follow: Follow,
}

/// How a face follows up one input.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Follow {
    /// The input was the editor's: the face brings the primary caret into view and repaints.
    #[default]
    Reveal,
    /// The input scrolled the viewport and left the caret where it was, possibly off screen.
    Scrolled,
    /// The input changed nothing: a key nothing names, a focus change.
    Ignored,
}

impl Reduction {
    /// The input was the editor's, with these effects.
    #[inline]
    pub const fn reveal(effects: Vec<EditorEffect>) -> Self {
        Self {
            effects,
            follow: Follow::Reveal,
        }
    }

    /// The input changed nothing.
    #[inline]
    pub const fn ignored() -> Self {
        Self {
            effects: Vec::new(),
            follow: Follow::Ignored,
        }
    }
}

/// A Super (⌘) shortcut of a GUI profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shortcut {
    SelectAll,
    Undo,
    Redo,
    /// ⌘E: a table's grid view ([`EditorCommand::ToggleGridView`]).
    ToggleGrid,
}

impl Shortcut {
    /// The shortcut Super+`key` runs (⌘A, ⌘Z, ⌘⇧Z, ⌘Y, ⌘E). ⌘S is not here: it
    /// saves ahead of every other reading of a key (see `reduce_editor_key`).
    #[inline]
    const fn of(key: char, mods: Modifiers) -> Option<Self> {
        match key {
            'a' | 'A' => Some(Self::SelectAll),
            'z' | 'Z' if mods.contains(Modifiers::SHIFT) => Some(Self::Redo),
            'z' | 'Z' => Some(Self::Undo),
            'y' | 'Y' => Some(Self::Redo),
            'e' | 'E' => Some(Self::ToggleGrid),
            _ => None,
        }
    }
}

impl EditorWorkspace {
    /// Reduce one input from a face.
    pub fn handle_input(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
        input: EditorInput,
    ) -> Result<Reduction, EditorError> {
        if view.grid().is_some() && !view.minibuffer_active() {
            return self.grid_input(store, view, input);
        }
        match input {
            EditorInput::Key { key, mods, text } => {
                self.reduce_editor_key(store, view, key, mods, text)
            }
            EditorInput::Text(text) => {
                view.end_escape_prefix();
                self.insert_text(store, view, &text).map(Reduction::reveal)
            }
            EditorInput::Paste(text) => {
                view.end_escape_prefix();
                self.insert_paste(store, view, &text).map(Reduction::reveal)
            }
            EditorInput::Pointer {
                byte,
                extend,
                visible_lines,
            } => {
                let snapshot = store
                    .snapshot(view.document)
                    .ok_or(EditorError::UnknownDocument)?;
                Ok(
                    if view.pointer_select(&snapshot.text, byte, extend, visible_lines) {
                        Reduction::reveal(Vec::new())
                    } else {
                        Reduction::ignored()
                    },
                )
            }
            EditorInput::Focus(_)
            | EditorInput::GridPointer { .. }
            | EditorInput::GridScroll { .. }
            | EditorInput::GridPanelPointer { .. } => Ok(Reduction::ignored()),
        }
    }

    /// Select the whole document (⌘A, and a face's Select All menu item).
    pub fn select_whole_document(
        &mut self,
        store: &DocumentStore,
        view: &mut EditorBufferView,
    ) -> Result<Vec<EditorEffect>, EditorError> {
        if let Some(effects) = self.minibuffer_blocks_document_input(view) {
            return Ok(effects);
        }
        if view.grid().is_some() {
            return Ok(Vec::new());
        }
        let len = store
            .snapshot(view.document)
            .ok_or(EditorError::UnknownDocument)?
            .text
            .len();
        view.selections = vec![Selection {
            anchor: 0,
            head: len,
        }];
        view.primary = 0;
        Ok(Vec::new())
    }

    fn reduce_editor_key(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
        key: Key,
        mods: Modifiers,
        text: Option<String>,
    ) -> Result<Reduction, EditorError> {
        let (key, text) = match key.main_block_twin() {
            Some(Key::Character(glyph)) => (Key::Character(glyph), Some(glyph.to_string())),
            Some(twin) => (twin, text),
            None => (key, text),
        };
        if store.directory(view.document).is_some()
            && !view.minibuffer_active()
            && !view.chord_pending()
            && !mods.intersects(
                Modifiers::CTRL
                    | Modifiers::ALT
                    | Modifiers::META
                    | Modifiers::SUPER
                    | Modifiers::HYPER,
            )
        {
            let typed = match text.as_deref() {
                Some(text) => {
                    let mut chars = text.chars();
                    chars.next().filter(|_| chars.next().is_none())
                }
                None => match key {
                    Key::Character(character) => Some(character),
                    _ => None,
                },
            };
            if let Some(character @ ('n' | 'p' | 'g' | '^' | 'f' | 'q')) = typed
                && let Some(chord) =
                    KeyChord::from_key(&Key::Character(character), Modifiers::empty())
            {
                return self.handle_chord(store, view, chord).map(Reduction::reveal);
            }
        }
        let spelled = !mods.intersects(Modifiers::SUPER | Modifiers::HYPER)
            && KeyChord::from_key(&key, mods).is_some();
        if !spelled && view.end_escape_prefix() {
            let reduction = self.reduce_key(store, view, key, mods, text)?;
            return Ok(match reduction.follow {
                Follow::Ignored => Reduction::reveal(reduction.effects),
                _ => reduction,
            });
        }
        self.reduce_key(store, view, key, mods, text)
    }

    /// [`Self::reduce_editor_key`] once the keypad is folded and a pending ESC has had its say.
    fn reduce_key(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
        key: Key,
        mods: Modifiers,
        text: Option<String>,
    ) -> Result<Reduction, EditorError> {
        let super_shortcuts = self.keymap.profile().super_shortcuts();
        if super_shortcuts
            && mods.contains(Modifiers::SUPER)
            && matches!(key, Key::Character('s' | 'S'))
        {
            return self
                .execute(store, view, EditorCommand::Save)
                .map(Reduction::reveal);
        }
        if matches!(key, Key::Named(NamedKey::Tab)) && !view.chord_pending() {
            if matches!(view.minibuffer, Minibuffer::Command { .. }) {
                return self
                    .command_completion_action(store, view, EditorCompletionAction::Complete)
                    .map(Reduction::reveal);
            }
            if !mods.is_empty() {
                return Ok(Reduction::ignored());
            }
        }
        if view.chord_pending()
            || mods.intersects(Modifiers::CTRL | Modifiers::ALT | Modifiers::META)
        {
            return match KeyChord::from_key(&key, mods) {
                Some(chord) => self.handle_chord(store, view, chord).map(Reduction::reveal),
                None => Ok(Reduction::ignored()),
            };
        }
        let palette = matches!(view.minibuffer, Minibuffer::Command { .. });
        let effects = match key {
            Key::Character(character) => {
                if mods.contains(Modifiers::SUPER) {
                    let shortcut = Shortcut::of(character, mods).filter(|_| super_shortcuts);
                    match shortcut {
                        Some(Shortcut::SelectAll) => self.select_whole_document(store, view)?,
                        Some(Shortcut::Undo) => {
                            self.document_gesture(store, view, EditorCommand::Undo)?
                        }
                        Some(Shortcut::Redo) => {
                            self.document_gesture(store, view, EditorCommand::Redo)?
                        }
                        Some(Shortcut::ToggleGrid) => {
                            self.execute(store, view, EditorCommand::ToggleGridView)?
                        }
                        None => return Ok(Reduction::ignored()),
                    }
                } else if mods.contains(Modifiers::HYPER) {
                    return Ok(Reduction::ignored());
                } else {
                    let text = text.unwrap_or_else(|| character.to_string());
                    self.insert_text(store, view, &text)?
                }
            }
            Key::Named(NamedKey::Space) => {
                if mods.intersects(Modifiers::SUPER | Modifiers::HYPER) {
                    return Ok(Reduction::ignored());
                }
                self.insert_text(store, view, " ")?
            }
            Key::Named(NamedKey::Backspace) => {
                if view.minibuffer_active() {
                    self.minibuffer_backspace(store, view)?
                } else {
                    self.execute(store, view, EditorCommand::DeleteBackward)?
                }
            }
            Key::Named(NamedKey::Delete) => {
                self.document_gesture(store, view, EditorCommand::DeleteForward)?
            }
            Key::Named(arrow @ (NamedKey::ArrowLeft | NamedKey::ArrowRight)) => {
                let command = if arrow == NamedKey::ArrowLeft {
                    EditorCommand::MoveBackward
                } else {
                    EditorCommand::MoveForward
                };
                let anchors = if mods.contains(Modifiers::SHIFT) {
                    view.selections.iter().map(|s| s.anchor).collect()
                } else {
                    Vec::new()
                };
                let effects = self.document_gesture(store, view, command)?;
                for (selection, anchor) in view.selections.iter_mut().zip(anchors) {
                    selection.anchor = anchor;
                }
                effects
            }
            Key::Named(NamedKey::ArrowUp) if palette => {
                self.command_completion_action(store, view, EditorCompletionAction::Previous)?
            }
            Key::Named(NamedKey::ArrowDown) if palette => {
                self.command_completion_action(store, view, EditorCompletionAction::Next)?
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.execute(store, view, EditorCommand::MoveLineUp)?
            }
            Key::Named(NamedKey::ArrowDown) => {
                self.execute(store, view, EditorCommand::MoveLineDown)?
            }
            Key::Named(NamedKey::Home) => {
                self.execute(store, view, EditorCommand::MoveLineStart)?
            }
            Key::Named(NamedKey::End) => self.execute(store, view, EditorCommand::MoveLineEnd)?,
            Key::Named(NamedKey::Enter) => {
                if view.minibuffer_active() {
                    self.submit_minibuffer(store, view)?
                } else {
                    self.execute(store, view, EditorCommand::Newline)?
                }
            }
            Key::Named(NamedKey::Escape)
                if view.minibuffer_active()
                    || !(self.keymap.profile().escape_is_meta() || view.chord_pending()) =>
            {
                self.execute(store, view, EditorCommand::Abort)?
            }
            Key::Named(NamedKey::Escape) => {
                let escape =
                    KeyChord::from_key(&key, Modifiers::empty()).expect("Escape has a chord name");
                self.handle_chord(store, view, escape)?
            }
            Key::Named(NamedKey::PageUp) => {
                self.execute(store, view, EditorCommand::ScrollPageUp)?
            }
            Key::Named(NamedKey::PageDown) => {
                self.execute(store, view, EditorCommand::ScrollPageDown)?
            }
            Key::Named(NamedKey::Tab) => match KeyChord::from_key(&key, mods) {
                Some(chord) => self.handle_chord(store, view, chord)?,
                None => return Ok(Reduction::ignored()),
            },
            _ => return Ok(Reduction::ignored()),
        };
        Ok(Reduction::reveal(effects))
    }

    fn document_gesture(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
        command: EditorCommand,
    ) -> Result<Vec<EditorEffect>, EditorError> {
        match self.minibuffer_blocks_document_input(view) {
            Some(effects) => Ok(effects),
            None => self.execute(store, view, command),
        }
    }
}

#[cfg(test)]
#[path = "input_tests.rs"]
mod tests;
