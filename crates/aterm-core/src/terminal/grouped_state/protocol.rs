// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Protocol-oriented grouped terminal state.

/// Type of DCS sequence being processed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(in crate::terminal) enum DcsType {
    /// No DCS sequence active.
    #[default]
    None,
    /// DECRQSS - Request Selection or Setting.
    Decrqss,
    /// Sixel graphics (DCS q).
    Sixel,
    /// XTGETTCAP - xterm termcap/terminfo query (DCS + q).
    Xtgettcap,
    /// Unknown or unsupported DCS sequence.
    Unknown,
}

/// Grouped state for DCS (Device Control String) sequence processing.
pub(in crate::terminal) struct DcsState {
    /// DCS sequence type currently being processed.
    pub(in crate::terminal) dcs_type: DcsType,
    /// Accumulated DCS data bytes (also reused as scratch buffer for APC).
    pub(in crate::terminal) data: Vec<u8>,
    /// Total bytes held in DCS buffers.
    pub(in crate::terminal) total_bytes: usize,
    /// Bytes consumed by the current DCS sequence (reset in hook, released in unhook).
    pub(in crate::terminal) sequence_bytes: usize,
}

impl DcsState {
    #[inline]
    pub(in crate::terminal) const fn new() -> Self {
        Self {
            dcs_type: DcsType::None,
            data: Vec::new(),
            total_bytes: 0,
            sequence_bytes: 0,
        }
    }

    /// Reset the in-progress sequence, its buffer and the byte counts.
    #[inline]
    pub(in crate::terminal) fn reset(&mut self) {
        self.dcs_type = DcsType::None;
        self.data.clear();
        self.total_bytes = 0;
        self.sequence_bytes = 0;
    }
}

/// Sixel decoding state: the decoder, the pending image and the ID counter.
pub(in crate::terminal) struct SixelState {
    /// Sixel graphics decoder.
    pub(in crate::terminal) decoder: crate::sixel::SixelDecoder,
    /// Pending Sixel image ready for display.
    pub(in crate::terminal) pending_image: Option<crate::sixel::SixelImage>,
    /// Counter for generating unique Sixel image IDs.
    pub(in crate::terminal) next_id: u64,
}

impl SixelState {
    #[inline]
    pub(in crate::terminal) fn new() -> Self {
        Self {
            decoder: crate::sixel::SixelDecoder::default(),
            pending_image: None,
            next_id: 0,
        }
    }

    /// Reset the decoder, the pending image and the ID counter.
    #[inline]
    pub(in crate::terminal) fn reset(&mut self) {
        *self = Self::new();
    }
}
