// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The in-flight output state of a [`TerminalCheckpoint`](super::TerminalCheckpoint):
//! what a finished sequence left in the engine (not the parser) that output still
//! queued on the PTY at a seamless-update park relies on. The parser carry
//! (`checkpoint_parser`) continues a sequence split at the park; this carries:
//!
//! * the REP (`CSI b`) character, raw, so `X` then `CSI 40 b` repeats wherever
//!   the park falls;
//! * an SGR 58 underline colour, raw (`0x01_RRGGBB` or `0x02_0000NN`), which the
//!   pen ([`StyleRepr`](super::StyleRepr)) does not hold;
//! * an open OSC 8 link and its `id=`, so the rest of the link's text is linked;
//! * a VT52 `ESC Y` waiting for its row and column bytes, which would otherwise
//!   print as text;
//! * a just-written ZWJ (U+200D), so the emoji after it joins the previous cell;
//! * the XTPUSHSGR stack (`CSI # {`), so a later XTPOPSGR restores the pushed pen;
//! * the XTSAVE'd DEC private modes (`CSI ? Ps s`), so a later XTRESTORE restores
//!   them.
//!
//! [`OutputStateRepr`] crosses as one additive `CheckpointMeta` key, absent when
//! none of it is set. A consumer that predates it ignores the key, so a hop to an
//! older build still re-parks over queued output for an open link or a VT52 address
//! ([`Terminal::output_state_in_flight`]) and loses the rest.
//!
//! The PTY keeper's META (`aterm-gui`'s `seamless::keeper_meta`) carries none of
//! it: that META is resent only when the modes change, so its in-flight state may
//! be hours stale at a crash.

use super::transient_state::Vt52CursorState;
use super::{SGR_STACK_MAX_DEPTH, SgrPushMask, SgrStackEntry, Terminal};
use crate::grid::{CellFlags, CurrentStyle, PackedColor};

/// An OSC 8 link still open at the checkpoint: the URI and its `id=`, as the
/// OSC 8 handler stored them.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct HyperlinkRepr {
    /// The link's URI.
    pub uri: String,
    /// The `id=` parameter that groups the link's cells, when it had one.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub id: Option<String>,
}

/// A VT52 `ESC Y` (direct cursor address) whose address bytes have not all
/// arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Vt52AddressRepr {
    /// The row already read (the address byte less 32), the column still to
    /// come; `None` while both are still to come.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub row: Option<u8>,
}

/// One XTPUSHSGR (`CSI # {`) entry still on the stack: the pen as pushed,
/// raw as the carried pen is (`CheckpointMeta`'s `style_*` bits), its SGR 58
/// underline colour, and which attribute groups the push named (all of them
/// for a bare push).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SgrPushRepr {
    /// The pushed foreground, `PackedColor` bits.
    pub fg: u32,
    /// The pushed background, `PackedColor` bits.
    pub bg: u32,
    /// The pushed SGR cell flags, `CellFlags` bits.
    pub flags: u16,
    /// The pushed DECSCA protection (XTPOPSGR keeps the current one).
    #[cfg_attr(feature = "serde", serde(default, skip_serializing_if = "is_false"))]
    pub protected: bool,
    /// The pushed SGR 58 underline colour, raw.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub underline_color: Option<u32>,
    /// The attribute groups the push named (`SgrPushMask` bits).
    pub mask: u16,
}

/// One XTSAVE'd (`CSI ? Ps s`) DEC private mode and the value it was saved
/// with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SavedModeRepr {
    /// The DEC private mode number.
    pub mode: u16,
    /// Whether it was set when saved.
    pub set: bool,
}

/// The engine's in-flight output state (see the module docs). `Default` is none of
/// it set: a fresh engine, or a producer that predates the key.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct OutputStateRepr {
    /// The character REP (`CSI b`) repeats, raw (before charset translation,
    /// which REP applies at the repeat).
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub rep: Option<char>,
    /// The SGR 58 underline colour, raw.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub underline_color: Option<u32>,
    /// The open OSC 8 link.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub hyperlink: Option<HyperlinkRepr>,
    /// The VT52 `ESC Y` waiting for its address.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub vt52_address: Option<Vt52AddressRepr>,
    /// The last zero-width character written was a ZWJ (U+200D): an emoji
    /// written next joins the previous cell when that cell ends with it.
    #[cfg_attr(feature = "serde", serde(default, skip_serializing_if = "is_false"))]
    pub zwj_pending: bool,
    /// The XTPUSHSGR stack, oldest first.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Vec::is_empty")
    )]
    pub sgr_stack: Vec<SgrPushRepr>,
    /// The XTSAVE'd DEC private modes, by mode number.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Vec::is_empty")
    )]
    pub saved_modes: Vec<SavedModeRepr>,
}

impl OutputStateRepr {
    /// Whether none of it is set (the wire omits the whole projection).
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

#[cfg(feature = "serde")]
#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if hands the predicate a reference"
)]
fn is_false(b: &bool) -> bool {
    !*b
}

/// Read a carried [`OutputStateRepr`], or the default when the value is not one:
/// `CheckpointMeta` is parsed whole (as with `lenient_title`), and state a successor
/// can live without must not cost the screen.
#[cfg(feature = "serde")]
pub(super) fn lenient_output_state<'de, D>(deserializer: D) -> Result<OutputStateRepr, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum Wire {
        State(OutputStateRepr),
        Unreadable(serde::de::IgnoredAny),
    }
    Ok(
        match <Wire as serde::Deserialize>::deserialize(deserializer)? {
            Wire::State(state) => state,
            Wire::Unreadable(_) => OutputStateRepr::default(),
        },
    )
}

impl Terminal {
    /// Project the in-flight output state (see [`OutputStateRepr`]).
    pub(super) fn capture_output_state_repr(&self) -> OutputStateRepr {
        let t = &self.transient;
        OutputStateRepr {
            rep: t.last_graphic_char,
            underline_color: t.current_underline_color,
            hyperlink: t.current_hyperlink.as_deref().map(|uri| HyperlinkRepr {
                uri: uri.to_owned(),
                id: t.current_hyperlink_id.as_deref().map(str::to_owned),
            }),
            vt52_address: match t.vt52_cursor_state {
                Vt52CursorState::None => None,
                Vt52CursorState::WaitingRow => Some(Vt52AddressRepr { row: None }),
                Vt52CursorState::WaitingCol(row) => Some(Vt52AddressRepr { row: Some(row) }),
            },
            zwj_pending: t.last_combining_was_zwj,
            sgr_stack: t
                .sgr_stack
                .iter()
                .map(|entry| SgrPushRepr {
                    fg: entry.style.fg.0,
                    bg: entry.style.bg.0,
                    flags: entry.style.flags.0,
                    protected: entry.style.protected,
                    underline_color: entry.underline_color,
                    mask: entry.mask.bits(),
                })
                .collect(),
            saved_modes: {
                let mut saved: Vec<SavedModeRepr> = t
                    .xtsave_modes
                    .iter()
                    .map(|(&mode, &set)| SavedModeRepr { mode, set })
                    .collect();
                // The map's order is arbitrary and the meta bytes are digested as
                // sent, so sort.
                saved.sort_unstable_by_key(|saved| saved.mode);
                saved
            },
        }
    }

    /// Install a carried [`OutputStateRepr`], replacing this engine's (what the
    /// checkpoint does not hold is cleared). Each value goes through the engine's own
    /// door: the link through the OSC 8 handler, which refuses what it would refuse from
    /// a program (a refused link costs only its continuation); a control REP character
    /// is dropped, as the engine never records one; the SGR stack keeps its newest
    /// `SGR_STACK_MAX_DEPTH` entries, as XTPUSHSGR evicts the oldest; a saved mode
    /// `query_dec_mode` does not know is dropped, which also bounds the map.
    pub(super) fn apply_output_state_repr(&mut self, repr: &OutputStateRepr) {
        self.transient.last_graphic_char = repr.rep.filter(|c| !c.is_control());
        self.transient.current_underline_color = repr.underline_color;
        self.transient.vt52_cursor_state = match repr.vt52_address {
            None => Vt52CursorState::None,
            Some(Vt52AddressRepr { row: None }) => Vt52CursorState::WaitingRow,
            Some(Vt52AddressRepr { row: Some(row) }) => Vt52CursorState::WaitingCol(row),
        };
        self.transient.last_combining_was_zwj = repr.zwj_pending;
        self.transient.sgr_stack = repr
            .sgr_stack
            .iter()
            .skip(repr.sgr_stack.len().saturating_sub(SGR_STACK_MAX_DEPTH))
            .map(|pushed| SgrStackEntry {
                style: CurrentStyle::new(
                    PackedColor(pushed.fg),
                    PackedColor(pushed.bg),
                    CellFlags(pushed.flags),
                    pushed.protected,
                ),
                underline_color: pushed.underline_color,
                mask: SgrPushMask::from_bits(pushed.mask),
            })
            .collect();
        // Closed first: the handler ignores a URI it refuses, which would leave
        // this engine's own link open. An `id=` cannot hold `:` or `;` (the handler
        // splits on both), so a carried id rebuilds its parameter exactly.
        self.transient.current_hyperlink = None;
        self.transient.current_hyperlink_id = None;
        let known: Vec<SavedModeRepr> = {
            let (_parser, mut handler) = self.split_for_process();
            if let Some(link) = &repr.hyperlink {
                let id = link
                    .id
                    .as_ref()
                    .map_or_else(Vec::new, |id| format!("id={id}").into_bytes());
                handler.handle_osc_8(&[b"8", &id, link.uri.as_bytes()]);
            }
            repr.saved_modes
                .iter()
                .filter(|saved| handler.query_dec_mode(saved.mode).is_some())
                .copied()
                .collect()
        };
        self.transient.xtsave_modes = known
            .into_iter()
            .map(|SavedModeRepr { mode, set }| (mode, set))
            .collect();
        self.transient.update_has_transient_extras();
    }
}
