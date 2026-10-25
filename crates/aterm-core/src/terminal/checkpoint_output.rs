// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! In-flight terminal output state carried across a [`TerminalCheckpoint`](super::TerminalCheckpoint):
//! REP (`CSI b`), SGR 58 underline colour, open OSC 8 hyperlink, pending VT52 `ESC Y` address,
//! pending ZWJ (U+200D), XTPUSHSGR stack (`CSI # {`), XTSAVE'd DEC private modes (`CSI ? Ps s`),
//! and the program's own state report standing (OSC 7501, which a re-send of it relies on).
//! [`OutputStateRepr`] crosses as one optional `CheckpointMeta` key, omitted when default.

use super::transient_state::Vt52CursorState;
use super::{SGR_STACK_MAX_DEPTH, SgrPushMask, SgrStackEntry, Terminal};
use crate::grid::{CellFlags, CurrentStyle, PackedColor};

/// An OSC 8 hyperlink still open at the checkpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct HyperlinkRepr {
    /// The link's URI.
    pub uri: String,
    /// Optional `id=` parameter grouping the link's cells.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub id: Option<String>,
}

/// A VT52 `ESC Y` direct cursor address whose address bytes have not all arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Vt52AddressRepr {
    /// Decoded row (address byte minus 32) when waiting for column, or `None` when waiting for both.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub row: Option<u8>,
}

/// One XTPUSHSGR (`CSI # {`) entry on the SGR stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SgrPushRepr {
    /// Pushed foreground (`PackedColor` bits).
    pub fg: u32,
    /// Pushed background (`PackedColor` bits).
    pub bg: u32,
    /// Pushed SGR cell flags (`CellFlags` bits).
    pub flags: u16,
    /// Pushed DECSCA protection flag.
    #[cfg_attr(feature = "serde", serde(default, skip_serializing_if = "is_false"))]
    pub protected: bool,
    /// Pushed SGR 58 underline colour, raw.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub underline_color: Option<u32>,
    /// Attribute groups named by the push (`SgrPushMask` bits).
    pub mask: u16,
}

/// One XTSAVE'd (`CSI ? Ps s`) DEC private mode and its saved state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SavedModeRepr {
    /// DEC private mode number.
    pub mode: u16,
    /// Whether the mode was set when saved.
    pub set: bool,
}

/// In-flight terminal output state carried across a checkpoint (`Default` when none is active).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct OutputStateRepr {
    /// Raw character repeated by REP (`CSI b`) before charset translation.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub rep: Option<char>,
    /// SGR 58 underline colour, raw.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub underline_color: Option<u32>,
    /// Open OSC 8 hyperlink.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub hyperlink: Option<HyperlinkRepr>,
    /// Pending VT52 `ESC Y` cursor address.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub vt52_address: Option<Vt52AddressRepr>,
    /// Whether the last zero-width character written was a ZWJ (U+200D).
    #[cfg_attr(feature = "serde", serde(default, skip_serializing_if = "is_false"))]
    pub zwj_pending: bool,
    /// XTPUSHSGR stack, oldest first.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Vec::is_empty")
    )]
    pub sgr_stack: Vec<SgrPushRepr>,
    /// XTSAVE'd DEC private modes sorted by mode number.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Vec::is_empty")
    )]
    pub saved_modes: Vec<SavedModeRepr>,
    /// The program's own state report standing (OSC 7501), as the body that says it: a restore
    /// reads it with the engine's own parser and installs it as a report that begins no turn.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub program_state: Option<String>,
}

impl OutputStateRepr {
    /// Returns whether no in-flight output state is set.
    #[inline]
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
#[inline]
const fn is_false(b: &bool) -> bool {
    !*b
}

/// Deserializes a carried [`OutputStateRepr`], falling back to `Default` if unreadable.
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
                saved.sort_unstable_by_key(|saved| saved.mode);
                saved
            },
            program_state: self.program_state.carried(),
        }
    }

    /// Installs a carried [`OutputStateRepr`], sanitizing each field through the engine's bounds and handlers.
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
        self.program_state.restore(repr.program_state.as_deref());
    }
}
