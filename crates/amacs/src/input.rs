// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The shared input reducer, `EditorInput` (design §4.4; slice S4).
//!
//! One key-to-command semantics for both faces: aterm's native Editor view
//! maps each winit event to an [`EditorInput`], the terminal face maps each
//! decoded terminal event, and [`EditorWorkspace::handle_input`] decides what
//! it means. Chords and prefixes (`C-x`, `M-g`, ESC as Meta), the `C-u`
//! argument, self-insertion, the minibuffer's own keys and a paste are all
//! decided here, so the two faces cannot fork. The keymap's [`Profile`]
//! carries what differs by face: the GUI's platform spellings and its Super
//! (⌘) shortcuts.
//!
//! What stays with a face is what only it can see: focus traversal between
//! its controls (F6 in the GUI), its own panels (the config editor's
//! completion list), the input method's preedit, and pointer hit testing
//! (the face hands over the document byte it found).

use aterm_types::keyboard::{Key, Modifiers, NamedKey};

use crate::editor::{
    EditorBufferView, EditorCommand, EditorCompletionAction, EditorEffect, EditorError,
    EditorWorkspace, KeyChord, Minibuffer, Selection,
};
use crate::store::DocumentStore;

/// One input a face hands the engine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditorInput {
    /// A key press or auto-repeat (a face never sends a release). `key` is
    /// the key as the layout names it without Shift, and `text` is what the
    /// layout typed for it, when it types (Shift+a's `A`, a German Shift+2's
    /// `"`): a self-inserting key inserts `text`, or `key`'s own character
    /// when there is none.
    Key {
        key: Key,
        mods: Modifiers,
        text: Option<String>,
    },
    /// Committed text that is not one key: an input method's commit, an
    /// assistive technology's insert.
    Text(String),
    /// A paste: inserted as one transaction, never read as keys, so a
    /// newline in it is a newline and never submits a minibuffer.
    Paste(String),
    /// A pointer press or drag at the document byte the face's hit test
    /// found. `extend` keeps the anchor (Shift-click, a drag), and
    /// `visible_lines` is how many lines the face shows.
    Pointer {
        byte: usize,
        extend: bool,
        visible_lines: usize,
    },
    /// The view gained (`true`) or lost keyboard focus. A pending key
    /// sequence survives it, as in Emacs.
    Focus(bool),
}

/// What one input did, for the face that delivered it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reduction {
    /// What the face carries out, in order: a save, a buffer switch, a
    /// status line.
    pub effects: Vec<EditorEffect>,
    /// How the face follows up.
    pub follow: Follow,
}

/// How a face follows up one input.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Follow {
    /// The input was the editor's: the face brings the primary caret into
    /// view and repaints.
    #[default]
    Reveal,
    /// The input scrolled the viewport and left the caret where it was,
    /// possibly off screen: the face repaints without revealing it. No key
    /// does this (Page Down pages the caret too); a face's own scrolling,
    /// such as the wheel, does.
    Scrolled,
    /// The input changed nothing: a key nothing names, a focus change. The
    /// face may give such a key a meaning of its own (the GUI moves focus on
    /// a modified Tab).
    Ignored,
}

impl Reduction {
    /// The input was the editor's, with these effects.
    pub const fn reveal(effects: Vec<EditorEffect>) -> Self {
        Self {
            effects,
            follow: Follow::Reveal,
        }
    }

    /// The input changed nothing.
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
    fn of(key: char, mods: Modifiers) -> Option<Self> {
        match key.to_ascii_lowercase() {
            'a' => Some(Self::SelectAll),
            'z' if mods.contains(Modifiers::SHIFT) => Some(Self::Redo),
            'z' => Some(Self::Undo),
            'y' => Some(Self::Redo),
            _ => None,
        }
    }
}

impl EditorWorkspace {
    /// Reduce one input from a face: the one place a key becomes a command.
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
            // Committed text and a paste are not keys: each ends an ESC
            // waiting for one rather than being read as Meta, and inserts as
            // one transaction.
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
    /// The minibuffer, while open, owns the gesture and refuses it.
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
        // A keypad key is its main-block twin: KP_5 types `5`, KP_Enter is
        // Enter. Only the keypad's digits and operators type.
        let (key, text) = match key.main_block_twin() {
            Some(Key::Character(glyph)) => (Key::Character(glyph), Some(glyph.to_string())),
            Some(twin) => (twin, text),
            None => (key, text),
        };
        // ESC is Meta for the next key the keymap can spell. A key it has no
        // name for (Up, Home, a function key) and a ⌘ key (a chord has no
        // Super) end the ESC instead, and do what they do without it: so
        // Escape, then Down, moves down, and Escape, then ⌘Z, undoes. The
        // ESC's HUD goes either way, so the face repaints even for a key
        // that then does nothing.
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

    /// [`Self::reduce_editor_key`] once the keypad is folded and a pending
    /// ESC has had its say.
    fn reduce_key(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
        key: Key,
        mods: Modifiers,
        text: Option<String>,
    ) -> Result<Reduction, EditorError> {
        let super_shortcuts = self.keymap.profile().super_shortcuts();
        // ⌘S saves whatever else is held or half typed: the platform's
        // reflex outranks a pending sequence, which stays pending (all but
        // an ESC, which a ⌘ key ends).
        if super_shortcuts
            && mods.contains(Modifiers::SUPER)
            && matches!(key, Key::Character('s' | 'S'))
        {
            return self
                .execute(store, view, EditorCommand::Save)
                .map(Reduction::reveal);
        }
        // TAB completes an M-x query whatever is held. A modified TAB no
        // sequence waits for is the face's (Shift+Tab and Control-Tab are a
        // GUI's focus traversal), so the reducer declines it; plain TAB is
        // the keymap's, below.
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
        // A modified key, or any key while a sequence (or ESC) waits, is a
        // chord for the keymap. A key the keymap cannot name is dropped and
        // leaves the sequence waiting.
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
                // Shift extends: every selection keeps its anchor.
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
                    self.insert_text(store, view, "\n")?
                }
            }
            // Escape closes an open minibuffer at once; otherwise it is the
            // ESC prefix, Meta for the next key (`handle_chord`), and in a
            // GUI it quits first, as a GUI's Escape always has
            // (`Profile::escape_quits`).
            Key::Named(NamedKey::Escape) if view.minibuffer_active() => {
                self.execute(store, view, EditorCommand::Abort)?
            }
            Key::Named(NamedKey::Escape) => {
                let mut effects = if self.keymap.profile().escape_quits() {
                    self.execute(store, view, EditorCommand::Abort)?
                } else {
                    Vec::new()
                };
                let escape =
                    KeyChord::from_key(&key, Modifiers::empty()).expect("Escape has a chord name");
                effects.extend(self.handle_chord(store, view, escape)?);
                effects
            }
            // Page Up and Page Down page the caret with the view, Emacs'
            // scroll-down/up-command (`C-v` and `M-v` off the Windows GUI).
            Key::Named(NamedKey::PageUp) => {
                self.execute(store, view, EditorCommand::ScrollPageUp)?
            }
            Key::Named(NamedKey::PageDown) => {
                self.execute(store, view, EditorCommand::ScrollPageDown)?
            }
            // Plain TAB is the keymap's: indent-for-tab-command.
            Key::Named(NamedKey::Tab) => match KeyChord::from_key(&key, mods) {
                Some(chord) => self.handle_chord(store, view, chord)?,
                None => return Ok(Reduction::ignored()),
            },
            _ => return Ok(Reduction::ignored()),
        };
        Ok(Reduction::reveal(effects))
    }

    /// Run `command` on the document, unless a minibuffer is open: it owns
    /// the keys then, and a document gesture (Delete, an arrow, ⌘Z) is
    /// refused with a status line rather than leaking into the text.
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

    /// An editor on `text` whose workspace uses `profile`'s keymap.
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

    /// The profiles differ only where a face's platform has a reflex of its
    /// own, and ESC is Meta in every one of them.
    #[test]
    fn profile_tables() {
        let command = KeyResolution::Command;
        // The terminal face is Emacs: C-s searches, and nothing else saves.
        assert_eq!(
            resolve(Profile::Terminal, "C-s"),
            command(EditorCommand::IncrementalSearch)
        );
        assert_eq!(resolve(Profile::Terminal, "M-s"), KeyResolution::Unbound);
        // Windows saves on C-s and keeps isearch on M-s.
        let windows = Profile::Gui(Platform::Windows);
        assert_eq!(resolve(windows, "C-s"), command(EditorCommand::Save));
        assert_eq!(
            resolve(windows, "M-s"),
            command(EditorCommand::IncrementalSearch)
        );
        // C-v and M-v page, except on the Windows GUI, where Ctrl+V pastes.
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
        // ⌥← is word motion on the Mac.
        assert_eq!(
            resolve(Profile::Gui(Platform::Mac), "M-left"),
            command(EditorCommand::MoveWordBackward)
        );
        // Only a GUI has a Super key.
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
        // ESC x is M-x in every profile, and ESC ESC quits.
        for profile in PROFILES {
            let (mut store, mut workspace, mut view, _) = editor(profile, "abc");
            press(
                &mut store,
                &mut workspace,
                &mut view,
                key(Key::Named(NamedKey::Escape), Modifiers::empty()),
            );
            assert!(view.chord_pending(), "ESC waits for its key on {profile:?}");
            press(
                &mut store,
                &mut workspace,
                &mut view,
                key(Key::Character('x'), Modifiers::empty()),
            );
            assert!(
                matches!(view.minibuffer, Minibuffer::Command { .. }),
                "ESC x opens M-x on {profile:?}"
            );
            assert!(!view.chord_pending());

            let (mut store, mut workspace, mut view, _) = editor(profile, "abc");
            for _ in 0..2 {
                press(
                    &mut store,
                    &mut workspace,
                    &mut view,
                    key(Key::Named(NamedKey::Escape), Modifiers::empty()),
                );
            }
            assert!(!view.chord_pending(), "ESC ESC quits on {profile:?}");
            assert_eq!(view.prefix_hud, None);

            // A GUI's Escape quits before it arms ESC: the region and a C-u
            // argument go. The terminal face's ESC only arms.
            let (mut store, mut workspace, mut view, _) = editor(profile, "abc");
            workspace
                .execute(&mut store, &mut view, EditorCommand::SetMark)
                .unwrap();
            press(
                &mut store,
                &mut workspace,
                &mut view,
                key(Key::Character('u'), Modifiers::CTRL),
            );
            let reduction = press(
                &mut store,
                &mut workspace,
                &mut view,
                key(Key::Named(NamedKey::Escape), Modifiers::empty()),
            );
            assert!(view.escape_pending(), "{profile:?}");
            let quits = profile != Profile::Terminal;
            assert_eq!(profile.escape_quits(), quits);
            assert_eq!(view.mark_active, !quits, "{profile:?}");
            assert_eq!(
                view.prefix_hud.as_deref().unwrap().starts_with("C-u 4 ESC"),
                !quits,
                "{profile:?}"
            );
            assert_eq!(
                reduction.effects,
                if quits {
                    vec![EditorEffect::Status("Quit".to_string())]
                } else {
                    Vec::new()
                },
                "{profile:?}"
            );
        }
    }

    /// ESC is Meta only for a key the keymap can spell. A key it cannot
    /// (Down, a function key) or a ⌘ key ends the ESC and does what it does
    /// without it, and so do committed text and a paste; a pending C-u
    /// survives the ESC it outlived. Any other pending sequence waits on.
    #[test]
    fn escape_ends_for_a_key_it_cannot_spell() {
        let text = "alpha beta\ngamma delta\nthird\nfourth\nfifth\nsixth\n";
        let escape = || key(Key::Named(NamedKey::Escape), Modifiers::empty());
        for profile in PROFILES {
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
            // The next letter types: the ESC is gone.
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

            // A key nothing reads still ends the ESC, and the face repaints.
            press(&mut store, &mut workspace, &mut view, escape());
            let reduction = press(
                &mut store,
                &mut workspace,
                &mut view,
                key(Key::Named(NamedKey::F5), Modifiers::empty()),
            );
            assert_eq!(reduction, Reduction::reveal(Vec::new()), "{profile:?}");
            assert!(!view.chord_pending(), "{profile:?}");

            // Committed text and a paste insert, and end the ESC.
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

            // C-x waits on through a key it cannot spell.
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

        // A GUI's ⌘Z after Escape undoes, as it did when Escape only quit.
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

        // In the terminal face C-u outlives the ESC: C-u ESC Down moves four
        // lines, and the argument's HUD is back while it waits.
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

    /// A minibuffer a face opens outside the keys (the Find, Go to Line and
    /// Commands buttons) ends a pending sequence, which could never complete
    /// inside it; a pending C-u still reaches the command M-x runs.
    #[test]
    fn a_minibuffer_opened_outside_the_keys_ends_a_pending_sequence() {
        let escape = key(Key::Named(NamedKey::Escape), Modifiers::empty());
        let c_x = key(Key::Character('x'), Modifiers::CTRL);
        for profile in PROFILES {
            for before in [&escape, &c_x] {
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

    /// ESC is Meta for exactly one key: ESC b moves back a word, ESC C-g
    /// cancels, and a C-u before it reaches the M- command.
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
        // The next b types: ESC armed one key only.
        press(
            &mut store,
            &mut workspace,
            &mut view,
            key(Key::Character('b'), Modifiers::empty()),
        );
        assert_eq!(view.primary_selection().head, 12);

        // ESC C-g cancels the prefix.
        press(&mut store, &mut workspace, &mut view, escape());
        press(
            &mut store,
            &mut workspace,
            &mut view,
            key(Key::Character('g'), Modifiers::CTRL),
        );
        assert!(!view.chord_pending());

        // C-u ESC x: M-x keeps the argument for the command it runs, so
        // `forward-char` moves four.
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

        // In an open minibuffer one Escape closes it, with nothing pending.
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

    /// TAB indents to the next tab stop at every caret, never over a region,
    /// and completes an M-x query instead while one is open.
    #[test]
    fn tab_indents_to_the_next_tab_stop() {
        let tab = || key(Key::Named(NamedKey::Tab), Modifiers::empty());
        let (mut store, mut workspace, mut view, document) =
            editor(Profile::Terminal, "ab\nx\n\tz");
        // Column 2 → 4; column 0 → 4; after a tab (column 4) → 8.
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
        // One undo takes the whole indent back.
        workspace
            .execute(&mut store, &mut view, EditorCommand::Undo)
            .unwrap();
        assert_eq!(
            store.snapshot(document).unwrap().text.as_ref(),
            "ab\nx\n\tz"
        );

        // Two carets on one line both land on a tab stop: the second counts
        // the indent the first put in before it.
        let (mut store, mut workspace, mut view, document) = editor(Profile::Terminal, "ab");
        view.selections = vec![Selection::caret(1), Selection::caret(2)];
        press(&mut store, &mut workspace, &mut view, tab());
        assert_eq!(store.snapshot(document).unwrap().text.as_ref(), "a   b   ");
        assert_eq!(
            view.selections,
            vec![Selection::caret(4), Selection::caret(8)]
        );

        // A region keeps its text: the indent goes in at the head.
        let (mut store, mut workspace, mut view, document) = editor(Profile::Terminal, "one two");
        view.selections = vec![Selection { anchor: 0, head: 3 }];
        press(&mut store, &mut workspace, &mut view, tab());
        assert_eq!(store.snapshot(document).unwrap().text.as_ref(), "one  two");
        assert_eq!(view.selections, vec![Selection::caret(4)]);

        // A modified TAB is the face's: declined, and the text is untouched.
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
        // ... unless a sequence waits for it: C-x S-tab is the keymap's.
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

        // In M-x, TAB completes the query and never edits the document.
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

    /// The ⌘ shortcuts belong to the GUI profiles; the terminal face drops a
    /// Super key, and ⌘S saves even with a sequence half typed.
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

        // ⌘A selects the document; in the terminal face Super+a types nothing.
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

    /// Keys that type insert their layout's text, a paste is one transaction
    /// whose newline is text, PageDown pages the caret with the view, and a
    /// key the editor has no use for is ignored.
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

        // A paste into an open minibuffer stays in it, newline and all.
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

        // A paste is never read as keys: one digit after C-u is text, and
        // the argument ends with it.
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
        // A four-line view pages two lines (a screen less two of context),
        // and the caret goes with it.
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
