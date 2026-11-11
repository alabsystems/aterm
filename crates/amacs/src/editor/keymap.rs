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

/// The built-in single chords. A row with more than one spelling lists
/// ALTERNATE keys for its command, and each binds as a chord of its own
/// ([`Keymap::bind_alternatives`]); no row here is a sequence.
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
    // Emacs's `kill-whole-line` key: in a table's grid it deletes the row.
    (&["C-S-backspace"], EditorCommand::GridDeleteRow),
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

/// The built-in key sequences, each one space-separated string. They are the
/// built-in keymap's only multi-chord bindings.
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
    ("C-c C-t", EditorCommand::ToggleGridView),
    ("C-c u", EditorCommand::UndoAgentEdit),
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

/// WINDOWS: Ctrl+S SAVES (audit I9). This is not a preference — it is the one
/// chord every Windows application, from Notepad to Visual Studio, has
/// answered for thirty years, and the editor answering it with an INCREMENTAL
/// SEARCH is the single most alarming thing the built-in editor did: the user
/// hits ^S to protect their work and the buffer starts hunting for text
/// instead.
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
    const fn chord_rows(self) -> &'static [&'static [ChordRow]] {
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
#[path = "keymap_tests.rs"]
mod tests;
