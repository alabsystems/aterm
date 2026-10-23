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
}

impl Shortcut {
    /// The shortcut Super+`key` runs (⌘A, ⌘Z, ⌘⇧Z, ⌘Y). ⌘S is not here: it
    /// saves ahead of every other reading of a key (see `reduce_editor_key`).
    #[inline]
    const fn of(key: char, mods: Modifiers) -> Option<Self> {
        match key {
            'a' | 'A' => Some(Self::SelectAll),
            'z' | 'Z' if mods.contains(Modifiers::SHIFT) => Some(Self::Redo),
            'z' | 'Z' => Some(Self::Undo),
            'y' | 'Y' => Some(Self::Redo),
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
            EditorInput::Focus(_) => Ok(Reduction::ignored()),
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
mod tests {
    use super::*;
    use crate::editor::{KeyResolution, Keymap, Platform, Profile};
    use crate::store::{DocumentId, DocumentViewId};

    const PROFILES: [Profile; 4] = [
        Profile::Gui(Platform::Mac),
        Profile::Gui(Platform::Windows),
        Profile::Gui(Platform::Other),
        Profile::Terminal,
    ];

    fn editor(
        profile: Profile,
        text: &str,
    ) -> (DocumentStore, EditorWorkspace, EditorBufferView, DocumentId) {
        let mut store = DocumentStore::for_test();
        let document = store.open("mem://input".into(), text.into());
        let mut workspace = EditorWorkspace::with_keymap(Keymap::emacs(profile));
        let view = workspace
            .attach(&mut store, document, DocumentViewId(7))
            .unwrap();
        (store, workspace, view, document)
    }

    fn key(key: Key, mods: Modifiers) -> EditorInput {
        EditorInput::Key {
            key,
            mods,
            text: None,
        }
    }

    fn press(
        store: &mut DocumentStore,
        workspace: &mut EditorWorkspace,
        view: &mut EditorBufferView,
        input: EditorInput,
    ) -> Reduction {
        workspace.handle_input(store, view, input).unwrap()
    }

    fn resolve(profile: Profile, chord: &str) -> KeyResolution {
        Keymap::emacs(profile).resolve(&[KeyChord::parse(chord).expect("test chord parses")])
    }

    #[test]
    fn profile_tables() {
        let command = KeyResolution::Command;
        assert_eq!(
            resolve(Profile::Terminal, "C-s"),
            command(EditorCommand::IncrementalSearch)
        );
        assert_eq!(resolve(Profile::Terminal, "M-s"), KeyResolution::Unbound);
        let windows = Profile::Gui(Platform::Windows);
        assert_eq!(resolve(windows, "C-s"), command(EditorCommand::Save));
        assert_eq!(
            resolve(windows, "M-s"),
            command(EditorCommand::IncrementalSearch)
        );
        for profile in PROFILES {
            let windows = profile == Profile::Gui(Platform::Windows);
            for (chord, page) in [
                ("C-v", EditorCommand::ScrollPageDown),
                ("M-v", EditorCommand::ScrollPageUp),
            ] {
                assert_eq!(
                    resolve(profile, chord),
                    if windows {
                        KeyResolution::Unbound
                    } else {
                        command(page)
                    },
                    "{chord} on {profile:?}"
                );
            }
        }
        assert_eq!(
            resolve(Profile::Gui(Platform::Mac), "M-left"),
            command(EditorCommand::MoveWordBackward)
        );
        for profile in PROFILES {
            assert_eq!(
                profile.super_shortcuts(),
                profile != Profile::Terminal,
                "{profile:?}"
            );
            assert_eq!(Keymap::emacs(profile).profile(), profile);
            assert_eq!(
                resolve(profile, "Tab"),
                command(EditorCommand::IndentForTab),
                "Tab indents on {profile:?}"
            );
        }
    }

    #[test]
    fn escape_is_meta_only_in_the_terminal_face() {
        let escape = || key(Key::Named(NamedKey::Escape), Modifiers::empty());
        for profile in PROFILES {
            let meta = profile == Profile::Terminal;
            assert_eq!(profile.escape_is_meta(), meta, "{profile:?}");
            let (mut store, mut workspace, mut view, document) = editor(profile, "abc");
            workspace
                .execute(&mut store, &mut view, EditorCommand::SetMark)
                .unwrap();
            press(
                &mut store,
                &mut workspace,
                &mut view,
                key(Key::Character('u'), Modifiers::CTRL),
            );
            let reduction = press(&mut store, &mut workspace, &mut view, escape());
            assert_eq!(view.escape_pending(), meta, "{profile:?}");
            assert_eq!(view.mark_active, meta, "{profile:?}");
            assert_eq!(
                view.prefix_hud
                    .as_deref()
                    .is_some_and(|hud| hud.starts_with("C-u 4 ESC")),
                meta,
                "{profile:?}"
            );
            assert_eq!(
                reduction.effects,
                if meta {
                    Vec::new()
                } else {
                    vec![EditorEffect::Status("Quit".to_string())]
                },
                "{profile:?}"
            );
            press(
                &mut store,
                &mut workspace,
                &mut view,
                key(Key::Character('x'), Modifiers::empty()),
            );
            assert!(!view.chord_pending(), "{profile:?}");
            if meta {
                assert!(
                    matches!(view.minibuffer, Minibuffer::Command { .. }),
                    "ESC x opens M-x on {profile:?}"
                );
                assert_eq!(store.snapshot(document).unwrap().text.as_ref(), "abc");
            } else {
                assert!(!view.minibuffer_active(), "{profile:?}");
                assert_eq!(
                    store.snapshot(document).unwrap().text.as_ref(),
                    "xabc",
                    "Escape, then x, types x on {profile:?}"
                );
            }

            let (mut store, mut workspace, mut view, _) = editor(profile, "abc");
            for _ in 0..2 {
                press(&mut store, &mut workspace, &mut view, escape());
            }
            assert!(!view.chord_pending(), "{profile:?}");
            assert_eq!(view.prefix_hud, None, "{profile:?}");

            press(
                &mut store,
                &mut workspace,
                &mut view,
                key(Key::Character('x'), Modifiers::CTRL),
            );
            assert_eq!(
                press(&mut store, &mut workspace, &mut view, escape()).effects,
                [EditorEffect::Status("C-x escape is not bound".to_string())],
                "{profile:?}"
            );
            assert!(!view.chord_pending(), "{profile:?}");
        }
    }

    #[test]
    fn escape_ends_for_a_key_it_cannot_spell() {
        let text = "alpha beta\ngamma delta\nthird\nfourth\nfifth\nsixth\n";
        let escape = || key(Key::Named(NamedKey::Escape), Modifiers::empty());
        for profile in PROFILES {
            let meta = profile.escape_is_meta();
            let (mut store, mut workspace, mut view, document) = editor(profile, text);
            press(&mut store, &mut workspace, &mut view, escape());
            let reduction = press(
                &mut store,
                &mut workspace,
                &mut view,
                key(Key::Named(NamedKey::ArrowDown), Modifiers::empty()),
            );
            assert_eq!(reduction.follow, Follow::Reveal, "{profile:?}");
            assert_eq!(view.primary_selection().head, 11, "{profile:?}: Down moved");
            assert!(!view.chord_pending(), "{profile:?}");
            assert_eq!(view.prefix_hud, None, "{profile:?}");
            press(
                &mut store,
                &mut workspace,
                &mut view,
                key(Key::Character('d'), Modifiers::empty()),
            );
            assert!(
                store
                    .snapshot(document)
                    .unwrap()
                    .text
                    .starts_with("alpha beta\ndgamma"),
                "{profile:?}"
            );

            press(&mut store, &mut workspace, &mut view, escape());
            let reduction = press(
                &mut store,
                &mut workspace,
                &mut view,
                key(Key::Named(NamedKey::F5), Modifiers::empty()),
            );
            assert_eq!(
                reduction,
                if meta {
                    Reduction::reveal(Vec::new())
                } else {
                    Reduction::ignored()
                },
                "{profile:?}"
            );
            assert!(!view.chord_pending(), "{profile:?}");

            for input in [
                EditorInput::Text("é".to_string()),
                EditorInput::Paste("p".to_string()),
            ] {
                press(&mut store, &mut workspace, &mut view, escape());
                press(&mut store, &mut workspace, &mut view, input.clone());
                assert!(!view.chord_pending(), "{profile:?} {input:?}");
            }
            assert!(
                store
                    .snapshot(document)
                    .unwrap()
                    .text
                    .starts_with("alpha beta\ndép"),
                "{profile:?}"
            );

            press(
                &mut store,
                &mut workspace,
                &mut view,
                key(Key::Character('x'), Modifiers::CTRL),
            );
            let reduction = press(
                &mut store,
                &mut workspace,
                &mut view,
                key(Key::Named(NamedKey::ArrowUp), Modifiers::empty()),
            );
            assert_eq!(reduction, Reduction::ignored(), "{profile:?}");
            assert!(view.chord_pending(), "{profile:?}");
        }

        let (mut store, mut workspace, mut view, document) =
            editor(Profile::Gui(Platform::Mac), "abc");
        press(
            &mut store,
            &mut workspace,
            &mut view,
            key(Key::Character('x'), Modifiers::empty()),
        );
        press(&mut store, &mut workspace, &mut view, escape());
        press(
            &mut store,
            &mut workspace,
            &mut view,
            key(Key::Character('z'), Modifiers::SUPER),
        );
        assert_eq!(store.snapshot(document).unwrap().text.as_ref(), "abc");
        assert!(!view.chord_pending());

        let (mut store, mut workspace, mut view, _) = editor(Profile::Terminal, text);
        press(
            &mut store,
            &mut workspace,
            &mut view,
            key(Key::Character('u'), Modifiers::CTRL),
        );
        press(&mut store, &mut workspace, &mut view, escape());
        press(
            &mut store,
            &mut workspace,
            &mut view,
            key(Key::Named(NamedKey::F5), Modifiers::empty()),
        );
        assert!(view.prefix_hud.as_deref().unwrap().starts_with("C-u 4 —"));
        press(
            &mut store,
            &mut workspace,
            &mut view,
            key(Key::Named(NamedKey::ArrowDown), Modifiers::empty()),
        );
        assert_eq!(view.primary_selection().head, text.find("fifth").unwrap());
        assert_eq!(view.prefix_hud, None);
    }

    #[test]
    fn a_minibuffer_opened_outside_the_keys_ends_a_pending_sequence() {
        let escape = key(Key::Named(NamedKey::Escape), Modifiers::empty());
        let c_x = key(Key::Character('x'), Modifiers::CTRL);
        for profile in PROFILES {
            let befores = if profile.escape_is_meta() {
                vec![&escape, &c_x]
            } else {
                vec![&c_x]
            };
            for before in befores {
                let (mut store, mut workspace, mut view, _) = editor(profile, "abc abc");
                press(&mut store, &mut workspace, &mut view, before.clone());
                assert!(view.chord_pending());
                workspace
                    .execute(&mut store, &mut view, EditorCommand::IncrementalSearch)
                    .unwrap();
                assert!(!view.chord_pending(), "{profile:?} {before:?}");
                assert_eq!(view.prefix_hud, None, "{profile:?} {before:?}");
                for character in "bc".chars() {
                    press(
                        &mut store,
                        &mut workspace,
                        &mut view,
                        key(Key::Character(character), Modifiers::empty()),
                    );
                }
                assert!(
                    matches!(&view.minibuffer, Minibuffer::Search { query, .. } if query == "bc"),
                    "{profile:?} {before:?}: {:?}",
                    view.minibuffer
                );
                press(
                    &mut store,
                    &mut workspace,
                    &mut view,
                    key(Key::Named(NamedKey::Enter), Modifiers::empty()),
                );
                assert!(!view.minibuffer_active(), "{profile:?} {before:?}");
            }
        }

        let (mut store, mut workspace, mut view, _) = editor(Profile::Terminal, "abcdefgh");
        press(
            &mut store,
            &mut workspace,
            &mut view,
            key(Key::Character('u'), Modifiers::CTRL),
        );
        workspace
            .execute(&mut store, &mut view, EditorCommand::ExecuteCommand)
            .unwrap();
        press(
            &mut store,
            &mut workspace,
            &mut view,
            EditorInput::Text("forward-char".to_string()),
        );
        press(
            &mut store,
            &mut workspace,
            &mut view,
            key(Key::Named(NamedKey::Enter), Modifiers::empty()),
        );
        assert_eq!(view.primary_selection().head, 4);
    }

    #[test]
    fn escape_is_meta_for_one_key() {
        let text = "alpha beta gamma";
        let (mut store, mut workspace, mut view, _) = editor(Profile::Terminal, text);
        view.selections = vec![Selection::caret(text.len())];
        let escape = || key(Key::Named(NamedKey::Escape), Modifiers::empty());
        press(&mut store, &mut workspace, &mut view, escape());
        assert!(view.prefix_hud.as_deref().unwrap().starts_with("ESC"));
        press(
            &mut store,
            &mut workspace,
            &mut view,
            key(Key::Character('b'), Modifiers::empty()),
        );
        assert_eq!(view.primary_selection().head, 11);
        press(
            &mut store,
            &mut workspace,
            &mut view,
            key(Key::Character('b'), Modifiers::empty()),
        );
        assert_eq!(view.primary_selection().head, 12);

        press(&mut store, &mut workspace, &mut view, escape());
        press(
            &mut store,
            &mut workspace,
            &mut view,
            key(Key::Character('g'), Modifiers::CTRL),
        );
        assert!(!view.chord_pending());

        view.selections = vec![Selection::caret(0)];
        press(
            &mut store,
            &mut workspace,
            &mut view,
            key(Key::Character('u'), Modifiers::CTRL),
        );
        press(&mut store, &mut workspace, &mut view, escape());
        assert!(view.prefix_hud.as_deref().unwrap().starts_with("C-u 4 ESC"));
        press(
            &mut store,
            &mut workspace,
            &mut view,
            key(Key::Character('x'), Modifiers::empty()),
        );
        assert!(matches!(view.minibuffer, Minibuffer::Command { .. }));
        press(
            &mut store,
            &mut workspace,
            &mut view,
            EditorInput::Text("forward-char".to_string()),
        );
        press(
            &mut store,
            &mut workspace,
            &mut view,
            key(Key::Named(NamedKey::Enter), Modifiers::empty()),
        );
        assert_eq!(view.primary_selection().head, 4);

        press(
            &mut store,
            &mut workspace,
            &mut view,
            key(Key::Character('x'), Modifiers::ALT),
        );
        press(&mut store, &mut workspace, &mut view, escape());
        assert!(!view.minibuffer_active());
        assert!(!view.chord_pending());
    }

    #[test]
    fn tab_indents_to_the_next_tab_stop() {
        let tab = || key(Key::Named(NamedKey::Tab), Modifiers::empty());
        let (mut store, mut workspace, mut view, document) =
            editor(Profile::Terminal, "ab\nx\n\tz");
        view.selections = vec![
            Selection::caret(2),
            Selection::caret(3),
            Selection::caret(6),
        ];
        let reduction = press(&mut store, &mut workspace, &mut view, tab());
        assert_eq!(reduction, Reduction::reveal(Vec::new()));
        assert_eq!(
            store.snapshot(document).unwrap().text.as_ref(),
            "ab  \n    x\n\t    z"
        );
        assert_eq!(
            view.selections,
            vec![
                Selection::caret(4),
                Selection::caret(9),
                Selection::caret(16)
            ]
        );
        workspace
            .execute(&mut store, &mut view, EditorCommand::Undo)
            .unwrap();
        assert_eq!(
            store.snapshot(document).unwrap().text.as_ref(),
            "ab\nx\n\tz"
        );

        let (mut store, mut workspace, mut view, document) = editor(Profile::Terminal, "ab");
        view.selections = vec![Selection::caret(1), Selection::caret(2)];
        press(&mut store, &mut workspace, &mut view, tab());
        assert_eq!(store.snapshot(document).unwrap().text.as_ref(), "a   b   ");
        assert_eq!(
            view.selections,
            vec![Selection::caret(4), Selection::caret(8)]
        );

        let (mut store, mut workspace, mut view, document) = editor(Profile::Terminal, "one two");
        view.selections = vec![Selection { anchor: 0, head: 3 }];
        press(&mut store, &mut workspace, &mut view, tab());
        assert_eq!(store.snapshot(document).unwrap().text.as_ref(), "one  two");
        assert_eq!(view.selections, vec![Selection::caret(4)]);

        for mods in [Modifiers::SHIFT, Modifiers::CTRL, Modifiers::ALT] {
            let reduction = press(
                &mut store,
                &mut workspace,
                &mut view,
                key(Key::Named(NamedKey::Tab), mods),
            );
            assert_eq!(reduction, Reduction::ignored(), "{mods:?}");
        }
        assert_eq!(store.snapshot(document).unwrap().text.as_ref(), "one  two");
        press(
            &mut store,
            &mut workspace,
            &mut view,
            key(Key::Character('x'), Modifiers::CTRL),
        );
        let reduction = press(
            &mut store,
            &mut workspace,
            &mut view,
            key(Key::Named(NamedKey::Tab), Modifiers::SHIFT),
        );
        assert_eq!(
            reduction.effects,
            [EditorEffect::Status("C-x S-tab is not bound".to_string())]
        );

        press(
            &mut store,
            &mut workspace,
            &mut view,
            key(Key::Character('x'), Modifiers::ALT),
        );
        for character in "und".chars() {
            press(
                &mut store,
                &mut workspace,
                &mut view,
                key(Key::Character(character), Modifiers::empty()),
            );
        }
        press(&mut store, &mut workspace, &mut view, tab());
        assert!(matches!(
            &view.minibuffer,
            Minibuffer::Command { query, .. } if query == "undo"
        ));
        assert_eq!(store.snapshot(document).unwrap().text.as_ref(), "one  two");
    }

    #[test]
    fn super_shortcuts_are_the_gui_profiles() {
        let save = |profile: Profile, before: &[EditorInput]| {
            let (mut store, mut workspace, mut view, document) = editor(profile, "abc");
            for input in before {
                press(&mut store, &mut workspace, &mut view, input.clone());
            }
            let reduction = press(
                &mut store,
                &mut workspace,
                &mut view,
                key(Key::Character('s'), Modifiers::SUPER),
            );
            (reduction, document, view.chord_pending())
        };
        let (reduction, document, _) = save(Profile::Gui(Platform::Mac), &[]);
        assert!(matches!(
            reduction.effects.as_slice(),
            [EditorEffect::SaveDocument { document: saved, .. }] if *saved == document
        ));
        let c_x = key(Key::Character('x'), Modifiers::CTRL);
        let (reduction, _, pending) = save(Profile::Gui(Platform::Mac), &[c_x]);
        assert!(matches!(
            reduction.effects.as_slice(),
            [EditorEffect::SaveDocument { .. }]
        ));
        assert!(pending, "the sequence ⌘S interrupted still waits");
        let (reduction, _, _) = save(Profile::Terminal, &[]);
        assert_eq!(reduction, Reduction::ignored());

        let (mut store, mut workspace, mut view, document) =
            editor(Profile::Gui(Platform::Other), "abc");
        press(
            &mut store,
            &mut workspace,
            &mut view,
            key(Key::Character('a'), Modifiers::SUPER),
        );
        assert_eq!(view.selections, vec![Selection { anchor: 0, head: 3 }]);
        let (mut store, mut workspace, mut view, _) = editor(Profile::Terminal, "abc");
        let reduction = press(
            &mut store,
            &mut workspace,
            &mut view,
            key(Key::Character('a'), Modifiers::SUPER),
        );
        assert_eq!(reduction, Reduction::ignored());
        assert_eq!(store.snapshot(document).unwrap().text.as_ref(), "abc");
    }

    #[test]
    fn plain_keys_type_paste_scroll_and_ignore() {
        let (mut store, mut workspace, mut view, document) = editor(Profile::Terminal, "");
        press(
            &mut store,
            &mut workspace,
            &mut view,
            EditorInput::Key {
                key: Key::Character('2'),
                mods: Modifiers::SHIFT,
                text: Some("\"".to_string()),
            },
        );
        press(
            &mut store,
            &mut workspace,
            &mut view,
            key(Key::Named(NamedKey::Numpad5), Modifiers::empty()),
        );
        assert_eq!(store.snapshot(document).unwrap().text.as_ref(), "\"5");

        workspace
            .execute(&mut store, &mut view, EditorCommand::GotoLine)
            .unwrap();
        let before = store.snapshot(document).unwrap().seq;
        press(
            &mut store,
            &mut workspace,
            &mut view,
            EditorInput::Paste("1\n2".to_string()),
        );
        assert!(matches!(
            &view.minibuffer,
            Minibuffer::GotoLine { query, .. } if query == "1\n2"
        ));
        assert_eq!(store.snapshot(document).unwrap().seq, before);
        press(
            &mut store,
            &mut workspace,
            &mut view,
            key(Key::Character('g'), Modifiers::CTRL),
        );
        let before = store.snapshot(document).unwrap().seq;
        press(
            &mut store,
            &mut workspace,
            &mut view,
            EditorInput::Paste("one\ntwo".to_string()),
        );
        let snapshot = store.snapshot(document).unwrap();
        assert_eq!(snapshot.text.as_ref(), "\"5one\ntwo");
        assert_eq!(snapshot.seq.0, before.0 + 1, "a paste is one transaction");

        press(
            &mut store,
            &mut workspace,
            &mut view,
            key(Key::Character('u'), Modifiers::CTRL),
        );
        press(
            &mut store,
            &mut workspace,
            &mut view,
            EditorInput::Paste("7".to_string()),
        );
        assert_eq!(
            store.snapshot(document).unwrap().text.as_ref(),
            "\"5one\ntwo7"
        );
        assert_eq!(view.prefix_hud, None);
        press(
            &mut store,
            &mut workspace,
            &mut view,
            key(Key::Character('x'), Modifiers::empty()),
        );
        assert_eq!(
            store.snapshot(document).unwrap().text.as_ref(),
            "\"5one\ntwo7x",
            "no argument is left to repeat the next key"
        );

        let long = "line\n".repeat(40);
        let (mut store, mut workspace, mut view, _) = editor(Profile::Terminal, &long);
        let reduction = press(
            &mut store,
            &mut workspace,
            &mut view,
            key(Key::Named(NamedKey::PageDown), Modifiers::empty()),
        );
        assert_eq!(reduction, Reduction::reveal(Vec::new()));
        assert_eq!(view.primary_selection().head, "line\n".len() * 2);
        assert_eq!(view.viewport_anchor, "line\n".len() * 2);

        for ignored in [
            key(Key::Named(NamedKey::F5), Modifiers::empty()),
            key(Key::Named(NamedKey::ArrowUp), Modifiers::CTRL),
            EditorInput::Focus(false),
        ] {
            assert_eq!(
                press(&mut store, &mut workspace, &mut view, ignored.clone()),
                Reduction::ignored(),
                "{ignored:?}"
            );
        }
    }
}
