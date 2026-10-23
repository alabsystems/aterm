// Copyright (c) HarfBuzz developers
// Copyright (c) 2020 Yevhenii Reizner
// SPDX-License-Identifier: MIT
//
// Derived from rustybuzz 0.20.1 src/hb/common.rs (`Direction`) and
// src/hb/tag.rs (the script half of `tags_from_script_and_language`) and
// modified by the aterm project in 2026.
// See ../../LICENSE-RUSTYBUZZ for the upstream license.

//! Run directions and OpenType script tags selected by a Unicode script.

use crate::Tag;
use aterm_grapheme::Script;

/// The direction a run is set in.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum Direction {
    /// Not yet decided.
    #[default]
    Invalid,
    /// Horizontal, left to right.
    LeftToRight,
    /// Horizontal, right to left.
    RightToLeft,
    /// Vertical, top to bottom.
    TopToBottom,
    /// Vertical, bottom to top.
    BottomToTop,
}

impl Direction {
    #[inline]
    pub(crate) const fn is_horizontal(self) -> bool {
        matches!(self, Direction::LeftToRight | Direction::RightToLeft)
    }

    #[inline]
    pub(crate) const fn is_forward(self) -> bool {
        matches!(self, Direction::LeftToRight | Direction::TopToBottom)
    }

    #[inline]
    pub(crate) const fn is_backward(self) -> bool {
        !self.is_forward()
    }

    #[inline]
    pub(crate) const fn reverse(self) -> Self {
        match self {
            Direction::Invalid => Direction::Invalid,
            Direction::LeftToRight => Direction::RightToLeft,
            Direction::RightToLeft => Direction::LeftToRight,
            Direction::TopToBottom => Direction::BottomToTop,
            Direction::BottomToTop => Direction::TopToBottom,
        }
    }

    /// Returns the horizontal direction `script` is written in, or `None` for boustrophedon scripts (harfbuzz#1000).
    #[inline]
    pub(crate) const fn from_script(script: Script) -> Option<Self> {
        match &script.0 {
            b"Arab" | b"Hebr" | b"Syrc" | b"Thaa" | b"Cprt" | b"Khar" | b"Phnx" | b"Nkoo"
            | b"Lydi" | b"Avst" | b"Armi" | b"Phli" | b"Prti" | b"Sarb" | b"Orkh" | b"Samr"
            | b"Mand" | b"Merc" | b"Mero" | b"Mani" | b"Mend" | b"Nbat" | b"Narb" | b"Palm"
            | b"Phlp" | b"Hatr" | b"Adlm" | b"Rohg" | b"Sogo" | b"Sogd" | b"Elym" | b"Chrs"
            | b"Yezi" | b"Ougr" | b"Gara" => Some(Direction::RightToLeft),
            b"Hung" | b"Ital" | b"Runr" | b"Tfng" => None,
            _ => Some(Direction::LeftToRight),
        }
    }
}

/// Returns the OpenType script tags to try for `script` in priority order (v3, v2, then legacy ISO 15924).
pub(crate) fn script_tags(script: Option<Script>) -> Vec<Tag> {
    let mut tags = Vec::with_capacity(3);
    let Some(script) = script else {
        return tags;
    };
    if let Some(tag) = new_tag_from_script(script) {
        // Myanmar's new tag is `mym2`; there is no `mym3`.
        if tag != Tag::from_bytes(b"mym2") {
            let mut tag3 = tag.to_bytes();
            tag3[3] = b'3';
            tags.push(Tag::from_bytes(&tag3));
        }
        tags.push(tag);
    }
    tags.push(old_tag_from_script(script));
    tags
}

#[inline]
const fn new_tag_from_script(script: Script) -> Option<Tag> {
    let tag: &[u8; 4] = match &script.0 {
        b"Beng" => b"bng2",
        b"Deva" => b"dev2",
        b"Gujr" => b"gjr2",
        b"Guru" => b"gur2",
        b"Knda" => b"knd2",
        b"Mlym" => b"mlm2",
        b"Orya" => b"ory2",
        b"Taml" => b"tml2",
        b"Telu" => b"tel2",
        b"Mymr" => b"mym2",
        _ => return None,
    };
    Some(Tag::from_bytes(tag))
}

#[inline]
const fn old_tag_from_script(script: Script) -> Tag {
    match &script.0 {
        // Katakana and Hiragana both map to `kana`.
        b"Hira" => Tag::from_bytes(b"kana"),
        // Spaces at the end are kept, unlike ISO 15924.
        b"Laoo" => Tag::from_bytes(b"lao "),
        b"Yiii" => Tag::from_bytes(b"yi  "),
        b"Nkoo" => Tag::from_bytes(b"nko "),
        b"Vaii" => Tag::from_bytes(b"vai "),
        code => Tag(u32::from_be_bytes(*code) | 0x2000_0000),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(code: &[u8; 4]) -> Vec<[u8; 4]> {
        script_tags(Some(Script(*code)))
            .into_iter()
            .map(Tag::to_bytes)
            .collect()
    }

    #[test]
    fn script_tags_try_new_then_old() {
        assert_eq!(tags(b"Latn"), [*b"latn"]);
        assert_eq!(tags(b"Arab"), [*b"arab"]);
        assert_eq!(tags(b"Deva"), [*b"dev3", *b"dev2", *b"deva"]);
        assert_eq!(tags(b"Mymr"), [*b"mym2", *b"mymr"]);
        assert_eq!(tags(b"Hira"), [*b"kana"]);
        assert_eq!(tags(b"Laoo"), [*b"lao "]);
        assert!(script_tags(None).is_empty());
    }

    #[test]
    fn directions_of_scripts() {
        assert_eq!(
            Direction::from_script(Script::ARABIC),
            Some(Direction::RightToLeft)
        );
        assert_eq!(
            Direction::from_script(Script::LATIN),
            Some(Direction::LeftToRight)
        );
        assert_eq!(Direction::from_script(Script(*b"Runr")), None);
    }
}
