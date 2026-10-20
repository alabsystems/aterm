// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The closed table of editor commands, their Emacs names and the M-x
//! completion that ranks them; the `C-u` argument a command takes; and the
//! workspace's history of the commands it ran.

use super::workspace::EditorWorkspace;

const COMMAND_HISTORY_LIMIT: usize = 200;
const COMMAND_COMPLETION_LIMIT: usize = 8;

/// The columns between tab stops: the same four-cell phase a tab character
/// is drawn with ([`super::editor_grapheme_columns`]).
pub const TAB_STOP: usize = 4;

/// A pending `C-u` argument: Emacs's raw prefix argument, waiting for the one
/// command that takes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PrefixArgument {
    /// `C-u` alone, pressed n times: 4ⁿ.
    Raw(i64),
    /// Decimal digits typed after `C-u` (`C-u 2`). While `open`, one more
    /// digit extends the number; a `C-u` after the digits closes it, as
    /// Emacs's `universal-argument-more` does, so a digit after that is text.
    Numeric { value: i64, open: bool },
}

impl PrefixArgument {
    fn value(self) -> i64 {
        let (Self::Raw(value) | Self::Numeric { value, .. }) = self;
        value
    }

    /// How many times a counting command repeats: zero counts as one, and no
    /// count exceeds 10,000, the clamp every counting command has always had.
    pub(super) fn count(self) -> usize {
        self.value().clamp(1, 10_000) as usize
    }

    /// The argument after one more `C-u`, as in Emacs: it multiplies a bare
    /// `C-u` by four, closes a number being typed, and starts over at four
    /// after a closed one.
    pub(super) fn after_universal(argument: Option<Self>) -> Self {
        match argument {
            None | Some(Self::Numeric { open: false, .. }) => Self::Raw(4),
            Some(Self::Raw(value)) => Self::Raw(value.saturating_mul(4)),
            Some(Self::Numeric { value, open: true }) => Self::Numeric { value, open: false },
        }
    }

    /// The argument after one more decimal digit, or `None` once the number
    /// is closed. The first digit replaces the raw 4ⁿ.
    pub(super) fn after_digit(self, digit: u32) -> Option<Self> {
        let digit = i64::from(digit);
        match self {
            Self::Raw(_) => Some(Self::Numeric {
                value: digit,
                open: true,
            }),
            Self::Numeric { value, open: true } => Some(Self::Numeric {
                value: value.saturating_mul(10).saturating_add(digit),
                open: true,
            }),
            Self::Numeric { open: false, .. } => None,
        }
    }

    /// `C-u 4`: how the HUD names the argument, alone or before a half-typed
    /// key sequence.
    pub(super) fn label(self) -> String {
        format!("C-u {}", self.value())
    }

    pub(super) fn hud(self) -> String {
        format!("{} — awaiting command (C-g cancels)", self.label())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorCommand {
    MoveBackward,
    MoveForward,
    MoveLineUp,
    MoveLineDown,
    MoveLineStart,
    MoveLineEnd,
    MoveWordBackward,
    MoveWordForward,
    DeleteBackward,
    DeleteForward,
    /// TAB: indent to the next tab stop. Until the Language table's indent
    /// rules arrive (slice L1) it inserts spaces at each caret up to the next
    /// multiple of [`TAB_STOP`] columns.
    IndentForTab,
    SetMark,
    KillRegion,
    KillLine,
    Yank,
    YankPop,
    Undo,
    Redo,
    Save,
    Abort,
    UniversalArgument,
    ExecuteCommand,
    IncrementalSearch,
    GotoLine,
    SwitchBuffer,
    RevertBuffer,
    StartMacro,
    EndMacro,
    PlayMacro,
    /// Page Down (and `C-v` off Windows, `PAGE_CHORDS`): the view and every
    /// caret a screen down, less two lines of context (Emacs'
    /// `next-screen-context-lines`).
    ScrollPageDown,
    /// Page Up (and `M-v` off Windows): the same, a screen up.
    ScrollPageUp,
}

impl EditorCommand {
    pub const ALL: [Self; 31] = [
        Self::Save,
        Self::Undo,
        Self::Redo,
        Self::IncrementalSearch,
        Self::GotoLine,
        Self::SwitchBuffer,
        Self::RevertBuffer,
        Self::SetMark,
        Self::KillRegion,
        Self::KillLine,
        Self::Yank,
        Self::YankPop,
        Self::MoveBackward,
        Self::MoveForward,
        Self::MoveLineUp,
        Self::MoveLineDown,
        Self::ScrollPageDown,
        Self::ScrollPageUp,
        Self::MoveLineStart,
        Self::MoveLineEnd,
        Self::MoveWordBackward,
        Self::MoveWordForward,
        Self::DeleteBackward,
        Self::DeleteForward,
        Self::IndentForTab,
        Self::Abort,
        Self::UniversalArgument,
        Self::ExecuteCommand,
        Self::StartMacro,
        Self::EndMacro,
        Self::PlayMacro,
    ];

    pub const fn name(&self) -> &'static str {
        match self {
            Self::MoveBackward => "backward-char",
            Self::MoveForward => "forward-char",
            Self::MoveLineUp => "previous-line",
            Self::MoveLineDown => "next-line",
            Self::MoveLineStart => "move-beginning-of-line",
            Self::MoveLineEnd => "move-end-of-line",
            Self::MoveWordBackward => "backward-word",
            Self::MoveWordForward => "forward-word",
            Self::DeleteBackward => "backward-delete-char",
            Self::DeleteForward => "delete-char",
            Self::IndentForTab => "indent-for-tab-command",
            Self::SetMark => "set-mark-command",
            Self::KillRegion => "kill-region",
            Self::KillLine => "kill-line",
            Self::Yank => "yank",
            Self::YankPop => "yank-pop",
            Self::Undo => "undo",
            Self::Redo => "redo",
            Self::Save => "save-buffer",
            Self::Abort => "keyboard-quit",
            Self::UniversalArgument => "universal-argument",
            Self::ExecuteCommand => "execute-extended-command",
            Self::IncrementalSearch => "isearch-forward",
            Self::GotoLine => "goto-line",
            Self::SwitchBuffer => "switch-to-buffer",
            Self::RevertBuffer => "revert-buffer",
            Self::StartMacro => "start-kbd-macro",
            Self::EndMacro => "end-kbd-macro",
            Self::PlayMacro => "call-last-kbd-macro",
            Self::ScrollPageDown => "scroll-up-command",
            Self::ScrollPageUp => "scroll-down-command",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorCompletionAction {
    Previous,
    Next,
    Complete,
    Choose(usize),
}

/// Bounded prefix/fuzzy projection used by reducer, renderer, accessibility,
/// touch actions, and introspection. Ranking never authorizes a command by
/// itself: the result is still a typed `EditorCommand` from the closed table.
pub fn command_completions(query: &str) -> Vec<EditorCommand> {
    let normalized = query.trim().to_ascii_lowercase();
    let mut ranked = EditorCommand::ALL
        .into_iter()
        .enumerate()
        .filter_map(|(default_rank, command)| {
            command_match_score(command.name(), &normalized)
                .map(|score| (score, default_rank, command.name(), command))
        })
        .collect::<Vec<_>>();
    ranked.sort_by_key(|(score, default_rank, name, _)| (*score, *default_rank, *name));
    ranked
        .into_iter()
        .take(COMMAND_COMPLETION_LIMIT)
        .map(|(_, _, _, command)| command)
        .collect()
}

fn command_match_score(name: &str, query: &str) -> Option<(u8, usize, usize)> {
    if query.is_empty() {
        // A blank M-x shows every command in the CURATED table order (Save
        // first): the score must be constant so the `default_rank` tiebreak
        // decides — a `name.len()` component here would instead surface the
        // shortest names ("undo") ahead of the curation.
        return Some((5, 0, 0));
    }
    let hyphenated = query
        .split_whitespace()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if name == hyphenated {
        return Some((0, 0, name.len()));
    }
    if name.starts_with(&hyphenated) {
        // Prefix matches tie as a CLASS and fall to the curated table order:
        // ranking by the unmatched remainder would surface the shortest
        // completion ("move-end-of-line") over the curation ("move-beginning-
        // of-line" first).
        return Some((1, 0, 0));
    }
    if let Some(position) = name.find(&hyphenated) {
        return Some((2, position, name.len()));
    }
    let needle = hyphenated
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .collect::<Vec<_>>();
    if needle.is_empty() {
        // Punctuation-only queries degenerate to the blank-query broad list:
        // constant score, curated order (see above).
        return Some((5, 0, 0));
    }
    let mut matched = 0;
    let mut first = None;
    for (index, character) in name
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .enumerate()
    {
        if needle.get(matched).copied() == Some(character) {
            first.get_or_insert(index);
            matched += 1;
            if matched == needle.len() {
                // `index` is the position of the LAST matched character: the
                // return runs in the very iteration that completed the needle.
                let first = first.unwrap_or(0);
                return Some((
                    3,
                    index.saturating_sub(first + needle.len() - 1),
                    name.len(),
                ));
            }
        }
    }
    None
}

impl EditorWorkspace {
    pub(super) fn remember_command(&mut self, command: &str) {
        if self
            .command_history
            .back()
            .is_some_and(|last| last == command)
        {
            return;
        }
        self.command_history.push_back(command.to_owned());
        while self.command_history.len() > COMMAND_HISTORY_LIMIT {
            self.command_history.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::projection::byte_of_line;
    use crate::editor::{EditorBufferView, EditorEffect, KeyChord, Selection, editor};
    use crate::store::DocumentStore;

    /// One C-u argument goes to exactly one command. Line motion, kill-line and
    /// yank take it; a command with no use for it drops it, so it can never
    /// multiply a later C-f. The HUD clears with it.
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

        // C-u 4 C-n moves four lines, and the C-f after it one character.
        // The host sends the digit as text once C-u has resolved; it is the
        // argument, never document text.
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

        // A bare C-u is four: C-u C-n moves four lines, then C-f one character.
        view.selections = vec![Selection::caret(0)];
        press(&mut store, &mut workspace, &mut view, "C-u");
        press(&mut store, &mut workspace, &mut view, "C-n");
        assert_eq!(view.primary_selection().head, byte_of_line(&text, 4));
        press(&mut store, &mut workspace, &mut view, "C-f");
        assert_eq!(view.primary_selection().head, byte_of_line(&text, 4) + 1);

        // A command with no use for the argument drops it: C-u C-x C-s saves,
        // and the C-f after it still moves one character. While C-x waits for
        // its second key the HUD still names the argument.
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

        // M-x forwards both a bare C-u and a typed number to the chosen
        // command. Enter accepts it once; the following C-f has no argument.
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

        // A chosen command that does not use the argument drops it too. Save
        // is only an effect here: no host save or filesystem action is run.
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

        // Cancelling M-x cancels its argument, rather than passing it to C-f.
        view.selections = vec![Selection::caret(0)];
        press(&mut store, &mut workspace, &mut view, "C-u");
        press(&mut store, &mut workspace, &mut view, "M-x");
        press(&mut store, &mut workspace, &mut view, "C-g");
        press(&mut store, &mut workspace, &mut view, "C-f");
        assert_eq!(view.primary_selection().head, 1);
        assert_eq!(view.prefix_hud, None);
        assert_eq!(store.snapshot(document).unwrap().text.as_ref(), text);

        // An undefined key ends the argument too, as in Emacs: C-u M-z, or
        // C-u C-x z, is not bound, and the C-f after it moves one character.
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

        // C-u 2 C-k kills two whole lines, newlines included. A digit chord
        // after C-u is the argument too.
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

        // Typed text takes no argument and drops it: C-u x inserts one x, and
        // the C-f after it moves one character.
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

    /// A second C-u closes a typed number, as in Emacs: C-u 1 C-u 3 types a 3
    /// rather than making the argument 13, and a C-u after a closed number
    /// starts over at four.
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
}
