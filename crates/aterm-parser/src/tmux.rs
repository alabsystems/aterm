// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! tmux passthrough, unwrapped where no tmux is there to unwrap it.
//!
//! A program that finds `TMUX` in its environment wraps each sequence tmux
//! would not forward in tmux's passthrough DCS, `ESC P tmux; <payload> ESC \`,
//! with every ESC of the payload doubled. A tab that inherited `TMUX` from the
//! pane aterm was started in has no tmux in it, so the wrapper reached the
//! parser itself: its doubled ESC broke the wrapper off, and the payload's own
//! ST, doubled into `ESC ESC \`, broke the payload off too. A wrapped sixel
//! was never placed and a wrapped DECRQSS or XTGETTCAP never answered.
//!
//! So the parser reads the wrapper as the transport it is, as tmux 3.5a
//! reads it (`input.c`, the DCS handler and its escape state; measured
//! 2026-10-19 through a real tmux 3.5a with `allow-passthrough on`, which
//! wrote `ESC P $ q m ESC \` to its own terminal for the wrapped
//! `ESC P tmux; ESC ESC P $ q m ESC ESC \ ESC \`, and the sixel, XTGETTCAP
//! and kitty APC likewise): from `ESC P tmux;` on, every byte reaches the
//! state machine as if it had arrived bare, except that a doubled ESC is one
//! ESC and `ESC \` ends the wrapper. CAN, SUB and the 8-bit ST are payload
//! (tmux forwards them), so they act on the payload's own sequence and leave
//! the wrapper open. A CSI or escape the payload left unfinished at the
//! wrapper's end goes on in what follows, as it does behind tmux; a string
//! (OSC, DCS, APC, PM, SOS) it left open ends with the wrapper, as at its ST.
//! RIS (`ESC c`) ends the wrapper with everything else it resets.
//!
//! Departures from tmux, where an ESC the program did not double meets
//! nothing to undouble: such an ESC keeps both bytes (`ESC ] 2 ; t BEL`),
//! where tmux drops it and forwards `] 2 ; t BEL` as text (measured
//! likewise), and an `ESC \` the program meant as its string's ST ends that
//! string as it ends the wrapper. Such a sequence ran before the wrapper was
//! unwrapped (its ESC broke the wrapper off), and it still runs.
//!
//! A wrapper inside a wrapper (a program that believes it runs in a tmux
//! inside a tmux) is unwrapped once; the inner one is an ignored DCS.

/// The bytes after `ESC P` that open a wrapper; its final byte `t` is the
/// first.
pub(crate) const TMUX_HEAD: &[u8; 5] = b"tmux;";

/// Where the parser stands with respect to a tmux passthrough wrapper.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TmuxPassthrough {
    /// No wrapper.
    #[default]
    Off,
    /// `ESC P t` with no parameter or intermediate was read, and this many
    /// bytes of `tmux;` (1 to 4) so far: the parser ignores the string
    /// (`DcsIgnore`) until the head decides. Any other byte makes it an
    /// ignored DCS, as a DCS ending in `t` always was.
    Head(u8),
    /// Inside a wrapper: each byte reaches the state machine unwrapped.
    Open,
    /// Inside a wrapper, after an ESC: the next byte decides (ESC: one ESC
    /// of the payload; `\`: the wrapper's end; anything else: both bytes).
    Escape,
}

impl TmuxPassthrough {
    /// Whether a wrapper is open: the bytes read reach the state machine
    /// through it.
    #[inline]
    #[must_use]
    pub fn is_open(self) -> bool {
        matches!(self, Self::Open | Self::Escape)
    }

    /// A stable name for a wire (`""` for [`Self::Off`]); [`Self::from_name`]
    /// reads it back.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Off => "",
            Self::Head(1) => "head1",
            Self::Head(2) => "head2",
            Self::Head(3) => "head3",
            Self::Head(4) => "head4",
            // No other head is reachable (`Parser::assert_invariants`), and a
            // carry naming one is refused; on a wire it reads as no wrapper.
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

    /// The head after `byte`, read in `DcsIgnore` with `stays` (the byte keeps
    /// the parser in the ignored string): the next head, [`Self::Open`] once
    /// `tmux;` is whole, or [`Self::Off`] for any other byte.
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
