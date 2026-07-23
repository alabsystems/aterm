// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Protocol-oriented grouped terminal state.

use super::callbacks::DcsCallback;

/// Type of DCS sequence being processed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(in crate::terminal) enum DcsType {
    /// No DCS sequence active.
    #[default]
    None,
    /// DECRQSS - Request Selection or Setting.
    Decrqss,
    /// Sixel graphics (DCS q).
    #[cfg(feature = "sixel")]
    Sixel,
    /// XTGETTCAP - xterm termcap/terminfo query (DCS + q).
    Xtgettcap,
    /// Unknown or unsupported DCS sequence.
    Unknown,
}

/// Grouped state for DCS (Device Control String) sequence processing.
///
/// Bundles the fields needed during `dcs_hook`/`dcs_put`/`dcs_unhook` and
/// APC handling (which reuses the data buffer). Extracted from flat
/// `Terminal` fields to reduce `TerminalHandler`'s field surface.
pub(in crate::terminal) struct DcsState {
    /// DCS sequence type currently being processed.
    pub(in crate::terminal) dcs_type: DcsType,
    /// Accumulated DCS data bytes (also reused as scratch buffer for APC).
    pub(in crate::terminal) data: Vec<u8>,
    /// Total bytes currently held in DCS buffers (global budget tracking).
    /// Used to prevent unbounded memory growth from DCS sequences.
    /// Enforced against `MAX_DCS_GLOBAL_BUDGET` in handler.rs.
    pub(in crate::terminal) total_bytes: usize,
    /// Callback for DCS payloads.
    pub(in crate::terminal) callback: Option<DcsCallback>,
    /// Final byte for the active DCS sequence.
    pub(in crate::terminal) final_byte: Option<u8>,
    /// Bytes consumed by the current DCS sequence (reset in hook, released in unhook).
    /// Separate from `data.len()` because Sixel feeds bytes to the decoder, not `data`.
    pub(in crate::terminal) sequence_bytes: usize,
}

impl DcsState {
    pub(in crate::terminal) fn new() -> Self {
        Self {
            dcs_type: DcsType::None,
            data: Vec::new(),
            total_bytes: 0,
            sequence_bytes: 0,
            callback: None,
            final_byte: None,
        }
    }

    /// Reset DCS processing state while preserving callback.
    ///
    /// Clears any in-progress DCS sequence, data buffers, and passthrough state.
    /// Budget tracking (`total_bytes`) is reset since all sequences are abandoned.
    pub(in crate::terminal) fn reset(&mut self) {
        self.dcs_type = DcsType::None;
        self.data.clear();
        self.total_bytes = 0;
        self.sequence_bytes = 0;
        self.final_byte = None;
    }
}

/// Grouped state for Sixel graphics processing.
///
/// Bundles the decoder, pending image output, and ID counter used during
/// DCS Sixel sequences. Accessed from `dcs_hook`/`dcs_put`/`dcs_unhook`
/// and the public Sixel image retrieval API.
#[cfg(feature = "sixel")]
pub(in crate::terminal) struct SixelState {
    /// Sixel graphics decoder.
    pub(in crate::terminal) decoder: crate::sixel::SixelDecoder,
    /// Pending Sixel image ready for display.
    pub(in crate::terminal) pending_image: Option<crate::sixel::SixelImage>,
    /// Counter for generating unique Sixel image IDs.
    pub(in crate::terminal) next_id: u64,
}

#[cfg(feature = "sixel")]
impl SixelState {
    pub(in crate::terminal) fn new() -> Self {
        Self {
            decoder: crate::sixel::SixelDecoder::default(),
            pending_image: None,
            next_id: 0,
        }
    }

    /// Reset Sixel state to initial values.
    ///
    /// Resets the decoder, clears any pending image, and resets the ID counter.
    pub(in crate::terminal) fn reset(&mut self) {
        *self = Self::new();
    }
}
