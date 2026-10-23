// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! tmux passthrough, unwrapped where no tmux is there to unwrap it.

/// The bytes after `ESC P` that open a wrapper; its final byte `t` is the first.
pub(crate) const TMUX_HEAD: &[u8; 5] = b"tmux;";

/// Where the parser stands with respect to a tmux passthrough wrapper.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TmuxPassthrough {
    /// No wrapper.
    #[default]
    Off,
    /// `ESC P t` with `1..=4` bytes of `tmux;` read so far (`DcsIgnore` until the head decides).
    Head(u8),
    /// Inside a wrapper: each byte reaches the state machine unwrapped.
    Open,
    /// Inside a wrapper, after an `ESC`: the next byte decides payload `ESC`, end `\`, or both.
    Escape,
}

impl TmuxPassthrough {
    /// Whether a wrapper is open: the bytes read reach the state machine through it.
    #[inline]
    #[must_use]
    pub const fn is_open(self) -> bool {
        matches!(self, Self::Open | Self::Escape)
    }

    /// A stable name for a wire (`""` for [`Self::Off`]); [`Self::from_name`] reads it back.
    #[inline]
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Off => "",
            Self::Head(1) => "head1",
            Self::Head(2) => "head2",
            Self::Head(3) => "head3",
            Self::Head(4) => "head4",
            Self::Head(_) => "",
            Self::Open => "open",
            Self::Escape => "escape",
        }
    }

    /// The wrapper [`Self::name`] names, or `None` for a name it never writes.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "" => Self::Off,
            "head1" => Self::Head(1),
            "head2" => Self::Head(2),
            "head3" => Self::Head(3),
            "head4" => Self::Head(4),
            "open" => Self::Open,
            "escape" => Self::Escape,
            _ => return None,
        })
    }

    /// Advances the `tmux;` header state after reading `byte` in `DcsIgnore` with `stays`.
    #[inline]
    pub(crate) fn after_head_byte(self, byte: u8, stays: bool) -> Self {
        let Self::Head(matched) = self else {
            return self;
        };
        match TMUX_HEAD.get(usize::from(matched)) {
            Some(&want) if stays && want == byte => {
                let matched = matched.saturating_add(1);
                if usize::from(matched) == TMUX_HEAD.len() {
                    Self::Open
                } else {
                    Self::Head(matched)
                }
            }
            _ => Self::Off,
        }
    }
}
