// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The keymap: a trie from key sequences to editor commands, and the built-in Emacs tables each face's keymap is made of (its [`Profile`]).

use std::collections::BTreeMap;

use super::chord::KeyChord;
use super::command::EditorCommand;

#[derive(Clone, Debug, Default)]
struct KeyNode {
    command: Option<EditorCommand>,
    children: BTreeMap<KeyChord, KeyNode>,
}

#[derive(Clone, Debug, Default)]
pub struct Keymap {
    root: KeyNode,
    profile: Profile,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyResolution {
    Command(EditorCommand),
    Prefix {
        display: String,
        reachable: Vec<String>,
    },
    Unbound,
}

/// One row of single chords: alternate spellings of one key for one command.
type ChordRow = (&'static [&'static str], EditorCommand);

/// The built-in single chords.
const EMACS_CHORDS: &[ChordRow] = &[
    (&["C-b"], EditorCommand::MoveBackward),
    (&["C-f"], EditorCommand::MoveForward),
    (&["C-p"], EditorCommand::MoveLineUp),
    (&["C-n"], EditorCommand::MoveLineDown),
    (&["C-a"], EditorCommand::MoveLineStart),
    (&["C-e"], EditorCommand::MoveLineEnd),
    (&["M-b", "M-left"], EditorCommand::MoveWordBackward),
    (&["M-f", "M-right"], EditorCommand::MoveWordForward),
    (&["Backspace"], EditorCommand::DeleteBackward),
    (&["Tab"], EditorCommand::IndentForTab),
    (&["C-d"], EditorCommand::DeleteForward),
    (&["C-Space"], EditorCommand::SetMark),
    (&["C-w"], EditorCommand::KillRegion),
    (&["C-k"], EditorCommand::KillLine),
    (&["C-y"], EditorCommand::Yank),
    (&["M-y"], EditorCommand::YankPop),
    (&["C-/"], EditorCommand::Undo),
    (&["C-u"], EditorCommand::UniversalArgument),
    (&["M-x"], EditorCommand::ExecuteCommand),
    (&["C-s"], EditorCommand::IncrementalSearch),
    (&["C-g"], EditorCommand::Abort),
    (&["M-<", "M-S-,"], EditorCommand::BeginningOfBuffer),
    (&["M->", "M-S-."], EditorCommand::EndOfBuffer),
    (&["C-l"], EditorCommand::RecenterTopBottom),
    (&["M-d"], EditorCommand::KillWord),
    (&["M-backspace"], EditorCommand::BackwardKillWord),
    (&["M-w"], EditorCommand::KillRingSave),
    (&["C-o"], EditorCommand::OpenLine),
    (&["C-t"], EditorCommand::TransposeChars),
    (&["M-q"], EditorCommand::FillParagraph),
    (&["C-?", "C-S-/"], EditorCommand::Redo),
    (
        &[
            "M-0", "M-1", "M-2", "M-3", "M-4", "M-5", "M-6", "M-7", "M-8", "M-9",
        ],
        EditorCommand::DigitArgument,
    ),
];

/// The built-in key sequences, each one space-separated string.
const EMACS_SEQUENCES: &[(&str, EditorCommand)] = &[
    ("C-x u", EditorCommand::Undo),
    ("C-x C-s", EditorCommand::Save),
    ("C-x b", EditorCommand::SwitchBuffer),
    ("C-x (", EditorCommand::StartMacro),
    ("C-x )", EditorCommand::EndMacro),
    ("C-x e", EditorCommand::PlayMacro),
    ("M-g g", EditorCommand::GotoLine),
    ("C-x k", EditorCommand::KillBuffer),
    ("C-x C-c", EditorCommand::SaveBuffersKillTerminal),
    ("C-x C-x", EditorCommand::ExchangePointAndMark),
    ("C-x h", EditorCommand::MarkWholeBuffer),
    ("C-c C-c", EditorCommand::FinishEdit),
    ("C-x #", EditorCommand::FinishEdit),
    ("C-c C-k", EditorCommand::AbortEdit),
];

/// Off macOS, C-left/C-right are the PLATFORM word-motion spelling (the
/// Windows/GTK text convention) — and real emacs binds the very same commands
/// to them (`right-word`/`left-word`), so no emacs muscle memory is
/// contradicted.
const OFF_MAC_CHORDS: &[ChordRow] = &[
    (&["C-left"], EditorCommand::MoveWordBackward),
    (&["C-right"], EditorCommand::MoveWordForward),
    (&["C-backspace"], EditorCommand::BackwardKillWord),
];

/// WINDOWS: Ctrl+S SAVES (audit I9).
const WINDOWS_CHORDS: &[ChordRow] = &[
    (&["C-s"], EditorCommand::Save),
    (&["M-s"], EditorCommand::IncrementalSearch),
];

/// Emacs' page commands, `C-v` (`scroll-up-command`) and `M-v`
/// (`scroll-down-command`).
const PAGE_CHORDS: &[ChordRow] = &[
    (&["C-v"], EditorCommand::ScrollPageDown),
    (&["M-v"], EditorCommand::ScrollPageUp),
];

/// The platform a GUI keymap is made for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Mac,
    Windows,
    Other,
}

impl Platform {
    /// The platform this build runs on.
    pub const HOST: Self = if cfg!(target_os = "macos") {
        Self::Mac
    } else if cfg!(windows) {
        Self::Windows
    } else {
        Self::Other
    };
}

/// Which face a built-in keymap serves (design §4.4).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Profile {
    /// aterm's native Editor view on `Platform`: Emacs, plus that platform's spellings (Ctrl+arrow word motion off macOS, `C-s` Save on Windows) and its Super (⌘) shortcuts.
    Gui(Platform),
    /// The terminal face: Emacs alone.
    #[default]
    Terminal,
}

impl Profile {
    /// The rows this profile adds after [`EMACS_CHORDS`], in binding order.
    fn chord_rows(self) -> &'static [&'static [ChordRow]] {
        match self {
            Self::Gui(Platform::Mac) => &[PAGE_CHORDS],
            Self::Gui(Platform::Other) => &[PAGE_CHORDS, OFF_MAC_CHORDS],
            Self::Gui(Platform::Windows) => &[OFF_MAC_CHORDS, WINDOWS_CHORDS],
            Self::Terminal => &[PAGE_CHORDS, OFF_MAC_CHORDS],
        }
    }

    /// Whether the Super (⌘) shortcuts apply: Save, Select All, Undo, Redo.
    pub const fn super_shortcuts(self) -> bool {
        matches!(self, Self::Gui(_))
    }

    /// Whether ESC is Meta for the next key, as in Emacs (`ESC x` is `M-x`): in the terminal face only, where Alt often types characters and ESC is how a terminal sends Meta anyway, so `C-u ESC x` keeps its argument and ESC ESC quits.
    pub const fn escape_is_meta(self) -> bool {
        matches!(self, Self::Terminal)
    }
}

impl Keymap {
    /// The built-in Emacs keymap for `profile`.
    pub fn emacs(profile: Profile) -> Self {
        let mut map = Self {
            profile,
            ..Self::default()
        };
        let profile_rows = profile.chord_rows().iter().copied().flatten();
        for (chords, command) in EMACS_CHORDS.iter().chain(profile_rows) {
            map.bind_alternatives(chords, *command)
                .expect("built-in editor chord is valid");
        }
        for (sequence, command) in EMACS_SEQUENCES {
            map.bind(sequence, *command)
                .expect("built-in editor key sequence is valid");
        }
        map
    }

    /// The face this keymap was built for.
    pub const fn profile(&self) -> Profile {
        self.profile
    }

    /// Bind a key SEQUENCE, written as one space-separated string (`"C-x C-s"`); binding a bound sequence again replaces its command.
    pub fn bind(&mut self, sequence: &str, command: EditorCommand) -> Result<(), &'static str> {
        let chords = sequence
            .split_whitespace()
            .map(|source| KeyChord::parse(source).ok_or("invalid chord"))
            .collect::<Result<Vec<_>, _>>()?;
        if chords.is_empty() {
            return Err("empty key sequence");
        }
        self.bind_chords(chords, command);
        Ok(())
    }

    /// Bind each element of `chords` as a one-chord binding of its own: alternate spellings of one key (`M-b`, and ⌥← as `M-left`), never a sequence.
    pub fn bind_alternatives(
        &mut self,
        chords: &[&str],
        command: EditorCommand,
    ) -> Result<(), &'static str> {
        let chords = chords
            .iter()
            .map(|source| KeyChord::parse(source).ok_or("invalid chord"))
            .collect::<Result<Vec<_>, _>>()?;
        if chords.is_empty() {
            return Err("no key chord");
        }
        for chord in chords {
            self.bind_chords(vec![chord], command);
        }
        Ok(())
    }

    fn bind_chords(&mut self, sequence: Vec<KeyChord>, command: EditorCommand) {
        let mut node = &mut self.root;
        for chord in sequence {
            node = node.children.entry(chord).or_default();
        }
        node.command = Some(command);
    }

    /// Every binding, as its key sequence and command, in key order.
    pub fn bindings(&self) -> Vec<(Vec<KeyChord>, EditorCommand)> {
        fn walk(
            node: &KeyNode,
            path: &mut Vec<KeyChord>,
            out: &mut Vec<(Vec<KeyChord>, EditorCommand)>,
        ) {
            if let Some(command) = node.command {
                out.push((path.clone(), command));
            }
            for (chord, child) in &node.children {
                path.push(chord.clone());
                walk(child, path, out);
                path.pop();
            }
        }
        let mut out = Vec::new();
        walk(&self.root, &mut Vec::new(), &mut out);
        out
    }

    pub fn resolve(&self, sequence: &[KeyChord]) -> KeyResolution {
        let mut node = &self.root;
        for chord in sequence {
            let Some(next) = node.children.get(chord) else {
                return KeyResolution::Unbound;
            };
            node = next;
        }
        if let Some(command) = &node.command {
            return KeyResolution::Command(*command);
        }
        if node.children.is_empty() {
            return KeyResolution::Unbound;
        }
        KeyResolution::Prefix {
            display: sequence
                .iter()
                .map(KeyChord::display)
                .collect::<Vec<_>>()
                .join(" "),
            reachable: node
                .children
                .iter()
                .map(|(chord, child)| {
                    let suffix = child.command.as_ref().map_or("prefix", EditorCommand::name);
                    format!("{}  {suffix}", chord.display())
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{EditorBufferView, EditorLines, Selection, editor};

    const PROFILES: [Profile; 4] = [
        Profile::Gui(Platform::Mac),
        Profile::Gui(Platform::Windows),
        Profile::Gui(Platform::Other),
        Profile::Terminal,
    ];

    /// PAGE DOWN AND PAGE UP PAGE THE CARET WITH THE VIEW: a screen less two
    /// lines of context each press, the caret keeping its row on screen and
    /// its goal column across a short line; at the end the view stops and the
    /// caret goes on to the last line, so no press is a no-op before the end
    /// of the document; Page Up mirrors it back to the top.
    #[test]
    fn page_down_and_up_move_the_caret_and_the_view_a_screen_at_a_time() {
        let mut text = (0..300)
            .map(|line| format!("line {line:03}\n"))
            .collect::<String>();
        text = text.replacen("line 033\n", "s\n", 1);
        let (mut store, mut workspace, mut view, _) = editor(&text);
        view.reconcile_viewport(&text, 35);
        view.selections = vec![Selection::caret("line ".len())];
        let lines = EditorLines::scanning(&text);
        let at = |view: &EditorBufferView| {
            (
                lines.number_at(view.viewport_anchor),
                lines.number_at(view.primary_selection().head),
            )
        };
        workspace
            .execute(&mut store, &mut view, EditorCommand::ScrollPageDown)
            .unwrap();
        assert_eq!(at(&view), (33, 33));
        assert_eq!(view.primary_selection().head, text.find("s\n").unwrap() + 1);
        workspace
            .execute(&mut store, &mut view, EditorCommand::ScrollPageDown)
            .unwrap();
        assert_eq!(at(&view), (66, 66));
        let line_66 = text.find("line 066").unwrap();
        assert_eq!(
            view.primary_selection().head,
            line_66 + "line ".len(),
            "goal column"
        );
        let mut previous = at(&view);
        loop {
            workspace
                .execute(&mut store, &mut view, EditorCommand::ScrollPageDown)
                .unwrap();
            let now = at(&view);
            if now == previous {
                break;
            }
            assert!(
                now.1 > previous.1,
                "every press moves the caret until the end"
            );
            previous = now;
        }
        assert_eq!(
            previous.1, 300,
            "the caret reaches the last line (after the final newline)"
        );
        assert_eq!(previous.0, 301 - 35, "the view stops at the last full page");
        while at(&view).1 > 0 {
            let before = at(&view);
            workspace
                .execute(&mut store, &mut view, EditorCommand::ScrollPageUp)
                .unwrap();
            assert!(at(&view).1 < before.1, "every press moves the caret back");
        }
        assert_eq!(at(&view), (0, 0));

        if Platform::HOST == Platform::Windows {
            // Ctrl+V is paste there; Page Down and Page Up are the keys.
            return;
        }
        let (mut store, mut workspace, mut view, _) = editor(&text);
        view.reconcile_viewport(&text, 35);
        workspace
            .handle_chord(&mut store, &mut view, KeyChord::parse("C-v").unwrap())
            .unwrap();
        assert_eq!(at(&view), (33, 33));
        workspace
            .handle_chord(&mut store, &mut view, KeyChord::parse("M-v").unwrap())
            .unwrap();
        assert_eq!(at(&view), (0, 0));
    }

    /// Each spelling in a row of alternates is a chord of its own: `M-b` and
    /// ⌥← (`M-left`) both move back a word, `M-f` and ⌥→ (`M-right`) forward,
    /// and none is a prefix waiting for the other.
    #[test]
    fn alternate_keys_bind_single_chords() {
        let chord = |s: &str| KeyChord::parse(s).expect("test chord parses");
        for profile in PROFILES {
            let map = Keymap::emacs(profile);
            let mut word_keys = vec![
                ("M-b", EditorCommand::MoveWordBackward),
                ("M-left", EditorCommand::MoveWordBackward),
                ("M-f", EditorCommand::MoveWordForward),
                ("M-right", EditorCommand::MoveWordForward),
            ];
            if profile != Profile::Gui(Platform::Mac) {
                word_keys.push(("C-left", EditorCommand::MoveWordBackward));
                word_keys.push(("C-right", EditorCommand::MoveWordForward));
            }
            for (key, command) in word_keys {
                assert_eq!(
                    map.resolve(&[chord(key)]),
                    KeyResolution::Command(command),
                    "{key} is one chord on {profile:?}"
                );
            }
            assert_eq!(
                map.resolve(&[chord("M-b"), chord("M-left")]),
                KeyResolution::Unbound
            );
            assert_eq!(
                map.resolve(&[chord("C-b")]),
                KeyResolution::Command(EditorCommand::MoveBackward),
                "C-b stays by-character on {profile:?}"
            );
        }

        let text = "alpha beta gamma";
        let (mut store, mut workspace, mut view, _) = editor(text);
        view.selections = vec![Selection::caret(text.len())];
        for (key, head) in [("M-left", 11), ("M-b", 6), ("M-right", 10), ("M-f", 16)] {
            workspace
                .handle_chord(&mut store, &mut view, chord(key))
                .unwrap();
            assert_eq!(view.primary_selection().head, head, "after {key}");
            assert!(!view.chord_pending(), "{key} left a prefix pending");
        }
    }

    /// The bindings of `map` with more than one chord that `EMACS_SEQUENCES`
    /// does not declare, each as its display string.
    fn undeclared_sequences(map: &Keymap) -> Vec<String> {
        fn walk(node: &KeyNode, path: &mut Vec<KeyChord>, out: &mut Vec<Vec<KeyChord>>) {
            if node.command.is_some() && path.len() > 1 {
                out.push(path.clone());
            }
            for (chord, child) in &node.children {
                path.push(chord.clone());
                walk(child, path, out);
                path.pop();
            }
        }
        let declared = EMACS_SEQUENCES
            .iter()
            .map(|(sequence, _)| {
                sequence
                    .split_whitespace()
                    .map(|chord| KeyChord::parse(chord).expect("declared chord parses"))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let mut bound = Vec::new();
        walk(&map.root, &mut Vec::new(), &mut bound);
        bound
            .into_iter()
            .filter(|sequence| !declared.contains(sequence))
            .map(|sequence| {
                sequence
                    .iter()
                    .map(KeyChord::display)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect()
    }

    /// Every multi-chord binding in each platform's built-in keymap is a
    /// declared sequence, so no row of alternates, platform arm or later edit
    /// can bind one by accident; every alternate is bound as a chord; and no
    /// bound chord is also a prefix, which would hide the sequences under it.
    #[test]
    fn no_row_binds_an_accidental_sequence() {
        fn no_shadowed_prefix(node: &KeyNode) {
            for (chord, child) in &node.children {
                assert!(
                    child.command.is_none() || child.children.is_empty(),
                    "{} is both a command and a prefix",
                    chord.display()
                );
                no_shadowed_prefix(child);
            }
        }
        for profile in PROFILES {
            let map = Keymap::emacs(profile);
            assert_eq!(
                undeclared_sequences(&map),
                Vec::<String>::new(),
                "{profile:?}"
            );
            let profile_rows = profile.chord_rows().iter().copied().flatten();
            for (chords, _) in EMACS_CHORDS.iter().chain(profile_rows) {
                for key in *chords {
                    let chord = KeyChord::parse(key).expect("built-in chord parses");
                    assert!(
                        matches!(map.resolve(&[chord]), KeyResolution::Command(_)),
                        "{key} is not bound as one chord on {profile:?}"
                    );
                }
            }
            no_shadowed_prefix(&map.root);
        }
        let mut refused = Keymap::default();
        assert!(
            refused
                .bind_alternatives(&["M-b", "C-x u"], EditorCommand::MoveWordBackward)
                .is_err()
        );
        assert_eq!(
            refused.resolve(&[KeyChord::parse("M-b").unwrap()]),
            KeyResolution::Unbound
        );

        let mut old = Keymap::emacs(Profile::Terminal);
        old.bind("M-b M-left", EditorCommand::MoveWordBackward)
            .unwrap();
        assert_eq!(undeclared_sequences(&old), ["M-b M-left"]);
    }

    /// The real sequences still bind as sequences: their first chord is a
    /// prefix and the whole sequence runs its command.
    #[test]
    fn sequences_still_bind_as_prefixes() {
        let keys = |sequence: &str| {
            sequence
                .split_whitespace()
                .map(|chord| KeyChord::parse(chord).expect("test chord parses"))
                .collect::<Vec<_>>()
        };
        for profile in PROFILES {
            let map = Keymap::emacs(profile);
            for (sequence, command) in [
                ("C-x C-s", EditorCommand::Save),
                ("C-x u", EditorCommand::Undo),
                ("C-x (", EditorCommand::StartMacro),
                ("M-g g", EditorCommand::GotoLine),
            ] {
                let chords = keys(sequence);
                assert!(
                    matches!(map.resolve(&chords[..1]), KeyResolution::Prefix { .. }),
                    "{sequence} starts with a prefix on {profile:?}"
                );
                assert_eq!(
                    map.resolve(&chords),
                    KeyResolution::Command(command),
                    "{sequence} on {profile:?}"
                );
            }
        }
    }

    /// The macOS keymap is byte-identical to what shipped before the platform
    /// additions: Ctrl+arrow stays unbound there (⌥ owns word motion; Ctrl+arrow
    /// belongs to Mission Control).
    #[test]
    fn ctrl_arrows_stay_unbound_on_macos() {
        let map = Keymap::emacs(Profile::Gui(Platform::Mac));
        let chord = |s: &str| KeyChord::parse(s).expect("test chord parses");
        assert_eq!(map.resolve(&[chord("C-left")]), KeyResolution::Unbound);
        assert_eq!(map.resolve(&[chord("C-right")]), KeyResolution::Unbound);
    }

    /// AUDIT I9, Windows: `C-s` SAVES — the reflex every Windows app owes its
    /// user, and previously the one chord that did something alarming instead
    /// (it started an incremental search).
    #[test]
    fn ctrl_s_saves_on_windows_and_isearch_keeps_a_chord() {
        let map = Keymap::emacs(Profile::Gui(Platform::Windows));
        let chord = |s: &str| KeyChord::parse(s).expect("test chord parses");
        assert_eq!(
            map.resolve(&[chord("C-s")]),
            KeyResolution::Command(EditorCommand::Save),
            "Ctrl+S must save, not search"
        );
        assert_eq!(
            map.resolve(&[chord("M-s")]),
            KeyResolution::Command(EditorCommand::IncrementalSearch),
            "isearch keeps a chord of its own"
        );
        assert_eq!(
            map.resolve(&[chord("C-x"), chord("C-s")]),
            KeyResolution::Command(EditorCommand::Save),
            "the emacs spelling is untouched"
        );
    }

    /// …and OFF Windows the table is exactly what it always was: `C-s` is
    /// isearch (emacs's own binding) and `M-s` is unbound.
    #[test]
    fn ctrl_s_stays_incremental_search_off_windows() {
        let chord = |s: &str| KeyChord::parse(s).expect("test chord parses");
        for profile in [
            Profile::Gui(Platform::Mac),
            Profile::Gui(Platform::Other),
            Profile::Terminal,
        ] {
            let map = Keymap::emacs(profile);
            assert_eq!(
                map.resolve(&[chord("C-s")]),
                KeyResolution::Command(EditorCommand::IncrementalSearch),
                "{profile:?}"
            );
            assert_eq!(
                map.resolve(&[chord("M-s")]),
                KeyResolution::Unbound,
                "{profile:?}"
            );
        }
    }
}
