// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The key → vi-action table for keyboard copy-mode: one bare keystroke (while vi mode is
//! active) maps to a motion, inline search, visual toggle or exit. Stateless: the caller's
//! pending state completes the two-key sequences (`ge`/`gE` via [`g_prefix_motion`], and
//! `f`/`F`/`t`/`T` awaiting a target character).

use aterm_core::{InlineSearchKind, ViMotion, ViVisualType};
use winit::keyboard::{Key, ModifiersState, NamedKey};

/// One resolved vi keystroke. `BeginInline` and `GPrefix` open a two-key sequence the
/// dispatcher completes with the next key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ViAction {
    /// A cursor motion (always grid-bounded in normal mode).
    Motion(ViMotion),
    /// `f`/`F`/`t`/`T`: the next printable key is the search target.
    BeginInline(InlineSearchKind),
    /// `;` / `,`: repeat the last inline search (forward / reverse).
    RepeatInline { reverse: bool },
    /// `v` / `V`: toggle visual selection of the given type.
    ToggleVisual(ViVisualType),
    /// `g`: the next key selects `ge` / `gE` (see [`g_prefix_motion`]).
    GPrefix,
    /// `Esc`: leave vi mode.
    Exit,
}

/// Map a bare keystroke to a [`ViAction`] while vi mode is active. `None` for an unmapped
/// key or one with Ctrl/Alt/Super held (those stay with the app's chords). Shift is
/// already folded into the produced character (`$`, `W`, `N`, …), so it is allowed.
#[must_use]
pub(crate) fn key_to_vi_action(key: &Key, mods: ModifiersState) -> Option<ViAction> {
    if mods.control_key() || mods.alt_key() || mods.super_key() {
        return None;
    }
    if matches!(key, Key::Named(NamedKey::Escape)) {
        return Some(ViAction::Exit);
    }
    let Key::Character(s) = key else {
        return None;
    };
    let c = s.chars().next()?;
    use ViMotion::{
        Bracket, Down, First, FirstOccupied, High, Last, Left, Low, Middle, ParagraphDown,
        ParagraphUp, Right, SearchNext, SearchPrevious, SemanticLeft, SemanticRight,
        SemanticRightEnd, Up, WordLeft, WordRight, WordRightEnd,
    };
    Some(match c {
        'h' => ViAction::Motion(Left),
        'j' => ViAction::Motion(Down),
        'k' => ViAction::Motion(Up),
        'l' => ViAction::Motion(Right),
        '0' => ViAction::Motion(First),
        '$' => ViAction::Motion(Last),
        '^' => ViAction::Motion(FirstOccupied),
        'w' => ViAction::Motion(SemanticRight),
        'b' => ViAction::Motion(SemanticLeft),
        'e' => ViAction::Motion(SemanticRightEnd),
        'W' => ViAction::Motion(WordRight),
        'B' => ViAction::Motion(WordLeft),
        'E' => ViAction::Motion(WordRightEnd),
        'H' => ViAction::Motion(High),
        'M' => ViAction::Motion(Middle),
        'L' => ViAction::Motion(Low),
        '%' => ViAction::Motion(Bracket),
        '{' => ViAction::Motion(ParagraphUp),
        '}' => ViAction::Motion(ParagraphDown),
        'n' => ViAction::Motion(SearchNext),
        'N' => ViAction::Motion(SearchPrevious),
        'f' => ViAction::BeginInline(InlineSearchKind::FindRight),
        'F' => ViAction::BeginInline(InlineSearchKind::FindLeft),
        't' => ViAction::BeginInline(InlineSearchKind::TillRight),
        'T' => ViAction::BeginInline(InlineSearchKind::TillLeft),
        ';' => ViAction::RepeatInline { reverse: false },
        ',' => ViAction::RepeatInline { reverse: true },
        'v' => ViAction::ToggleVisual(ViVisualType::Char),
        'V' => ViAction::ToggleVisual(ViVisualType::Line),
        'g' => ViAction::GPrefix,
        _ => return None,
    })
}

/// The second key of a `g`-prefixed motion: `ge` (end of the previous semantic word) or
/// `gE` (end of the previous whitespace word); any other key cancels. The engine has no
/// `gg`/`G`.
#[must_use]
pub(crate) fn g_prefix_motion(c: char) -> Option<ViMotion> {
    match c {
        'e' => Some(ViMotion::SemanticLeftEnd),
        'E' => Some(ViMotion::WordLeftEnd),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(c: &str) -> Key {
        Key::Character(c.into())
    }
    fn no_mods() -> ModifiersState {
        ModifiersState::empty()
    }

    #[test]
    fn single_key_motions_map() {
        let m = no_mods();
        assert_eq!(
            key_to_vi_action(&ch("j"), m),
            Some(ViAction::Motion(ViMotion::Down))
        );
        assert_eq!(
            key_to_vi_action(&ch("k"), m),
            Some(ViAction::Motion(ViMotion::Up))
        );
        assert_eq!(
            key_to_vi_action(&ch("h"), m),
            Some(ViAction::Motion(ViMotion::Left))
        );
        assert_eq!(
            key_to_vi_action(&ch("l"), m),
            Some(ViAction::Motion(ViMotion::Right))
        );
        assert_eq!(
            key_to_vi_action(&ch("w"), m),
            Some(ViAction::Motion(ViMotion::SemanticRight))
        );
        assert_eq!(
            key_to_vi_action(&ch("%"), m),
            Some(ViAction::Motion(ViMotion::Bracket))
        );
        // Shifted glyphs arrive as the final char (Shift folded in) — still map.
        assert_eq!(
            key_to_vi_action(&ch("$"), m),
            Some(ViAction::Motion(ViMotion::Last))
        );
        assert_eq!(
            key_to_vi_action(&ch("W"), m),
            Some(ViAction::Motion(ViMotion::WordRight))
        );
        assert_eq!(
            key_to_vi_action(&ch("N"), m),
            Some(ViAction::Motion(ViMotion::SearchPrevious))
        );
    }

    #[test]
    fn two_key_openers_and_visual_and_exit() {
        let m = no_mods();
        assert_eq!(
            key_to_vi_action(&ch("f"), m),
            Some(ViAction::BeginInline(InlineSearchKind::FindRight))
        );
        assert_eq!(
            key_to_vi_action(&ch("T"), m),
            Some(ViAction::BeginInline(InlineSearchKind::TillLeft))
        );
        assert_eq!(key_to_vi_action(&ch("g"), m), Some(ViAction::GPrefix));
        assert_eq!(g_prefix_motion('e'), Some(ViMotion::SemanticLeftEnd));
        assert_eq!(g_prefix_motion('E'), Some(ViMotion::WordLeftEnd));
        assert_eq!(g_prefix_motion('x'), None);
        assert_eq!(
            key_to_vi_action(&ch("v"), m),
            Some(ViAction::ToggleVisual(ViVisualType::Char))
        );
        assert_eq!(
            key_to_vi_action(&ch("V"), m),
            Some(ViAction::ToggleVisual(ViVisualType::Line))
        );
        assert_eq!(
            key_to_vi_action(&ch(";"), m),
            Some(ViAction::RepeatInline { reverse: false })
        );
        assert_eq!(
            key_to_vi_action(&ch(","), m),
            Some(ViAction::RepeatInline { reverse: true })
        );
        assert_eq!(
            key_to_vi_action(&Key::Named(NamedKey::Escape), m),
            Some(ViAction::Exit)
        );
    }

    #[test]
    fn modified_and_unmapped_keys_pass_through() {
        // Ctrl/Alt/Super held → None (the key stays available to the app's own chords).
        assert_eq!(key_to_vi_action(&ch("j"), ModifiersState::CONTROL), None);
        assert_eq!(key_to_vi_action(&ch("l"), ModifiersState::SUPER), None);
        assert_eq!(key_to_vi_action(&ch("w"), ModifiersState::ALT), None);
        // Unmapped bare keys → None.
        assert_eq!(key_to_vi_action(&ch("z"), no_mods()), None);
        assert_eq!(
            key_to_vi_action(&Key::Named(NamedKey::Enter), no_mods()),
            None
        );
    }
}
