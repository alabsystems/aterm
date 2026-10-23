// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Parser actions and the sink trait.

use aterm_provenance::{Provenance, Pty};

/// Action produced by the parser.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Action<'a> {
    /// Print a character to the screen.
    Print(char),

    /// Execute a C0 or C1 control function.
    Execute(u8),

    /// Dispatch a CSI sequence.
    CsiDispatch {
        /// Numeric parameters (separated by ; or :)
        params: &'a [u16],
        /// Intermediate bytes (0x20-0x2F)
        intermediates: &'a [u8],
        /// Final byte (0x40-0x7E)
        final_byte: u8,
    },

    /// Dispatch an escape sequence.
    EscDispatch {
        /// Intermediate bytes
        intermediates: &'a [u8],
        /// Final byte
        final_byte: u8,
    },

    /// Dispatch an OSC sequence.
    OscDispatch {
        /// OSC parameters (separated by ;)
        params: &'a [&'a [u8]],
    },

    /// Hook a DCS sequence.
    DcsHook {
        /// Numeric parameters
        params: &'a [u16],
        /// Intermediate bytes
        intermediates: &'a [u8],
        /// Final byte
        final_byte: u8,
    },

    /// Put a byte into the DCS handler.
    DcsPut(u8),

    /// Start an APC sequence (ESC _ or 0x9F).
    ApcStart,

    /// Put a byte into the APC handler.
    ApcPut(u8),

    /// End of an APC sequence.
    ApcEnd,
}

/// Receives parser actions.
pub trait ActionSink {
    /// Print a character to the screen at the cursor position.
    fn print(&mut self, c: char);

    /// Bulk print a run of printable ASCII (`data` holds only 0x20-0x7E).
    #[inline]
    fn print_ascii_bulk(&mut self, data: &Provenance<[u8], Pty>) {
        for &b in data.as_ref() {
            self.print(b as char);
        }
    }

    /// Bulk print a run of two or more decoded non-ASCII characters.
    #[inline]
    fn print_unicode_bulk(&mut self, chars: &Provenance<[char], Pty>) {
        for &c in chars.as_ref() {
            self.print(c);
        }
    }

    /// Execute a C0 control function (0x00-0x1F) or C1 (0x80-0x9F).
    fn execute(&mut self, byte: u8);

    /// A CSI sequence has been parsed.
    fn csi_dispatch(
        &mut self,
        params: &Provenance<[u16], Pty>,
        intermediates: &Provenance<[u8], Pty>,
        final_byte: u8,
    );

    /// A CSI sequence with colon subparameters has been parsed, in place of `csi_dispatch`.
    #[inline]
    fn csi_dispatch_with_subparams(
        &mut self,
        params: &Provenance<[u16], Pty>,
        intermediates: &Provenance<[u8], Pty>,
        final_byte: u8,
        _subparam_mask: u32,
    ) {
        self.csi_dispatch(params, intermediates, final_byte);
    }

    /// An escape sequence has been parsed.
    #[inline]
    fn esc_dispatch(&mut self, _intermediates: &Provenance<[u8], Pty>, _final_byte: u8) {}

    /// An OSC sequence has been parsed.
    #[inline]
    fn osc_dispatch(&mut self, _params: &Provenance<[&[u8]], Pty>) {}

    /// An OSC sequence has been parsed.
    #[inline]
    fn osc_dispatch_with_terminator(
        &mut self,
        params: &Provenance<[&[u8]], Pty>,
        _bel_terminated: bool,
    ) {
        self.osc_dispatch(params);
    }

    /// Start of a DCS sequence.
    #[inline]
    fn dcs_hook(
        &mut self,
        _params: &Provenance<[u16], Pty>,
        _intermediates: &Provenance<[u8], Pty>,
        _final_byte: u8,
    ) {
    }

    /// Data byte within a DCS sequence.
    #[inline]
    fn dcs_put(&mut self, _byte: u8) {}

    /// Bulk data bytes within a DCS sequence (sixel, tmux passthrough).
    #[inline]
    fn dcs_put_bulk(&mut self, data: &Provenance<[u8], Pty>) {
        for &b in data.as_ref() {
            self.dcs_put(b);
        }
    }

    /// End of a DCS sequence.
    #[inline]
    fn dcs_unhook(&mut self, _canceled: bool) {}

    /// Start of an APC (Application Program Command): ESC _ or 0x9F, up to ST.
    #[inline]
    fn apc_start(&mut self) {}

    /// Data byte within an APC sequence.
    #[inline]
    fn apc_put(&mut self, _byte: u8) {}

    /// Bulk data bytes within an APC sequence; kitty graphics streams multi-MB base64 through it.
    #[inline]
    fn apc_put_bulk(&mut self, data: &Provenance<[u8], Pty>) {
        for &b in data.as_ref() {
            self.apc_put(b);
        }
    }

    /// End of an APC sequence.
    #[inline]
    fn apc_end(&mut self, _canceled: bool) {}
}

/// A sink that discards all actions.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullSink;

impl ActionSink for NullSink {
    #[inline]
    fn print(&mut self, _: char) {}
    #[inline]
    fn print_ascii_bulk(&mut self, _: &Provenance<[u8], Pty>) {}
    #[inline]
    fn execute(&mut self, _: u8) {}
    #[inline]
    fn csi_dispatch(&mut self, _: &Provenance<[u16], Pty>, _: &Provenance<[u8], Pty>, _: u8) {}
    #[inline]
    fn esc_dispatch(&mut self, _: &Provenance<[u8], Pty>, _: u8) {}
    #[inline]
    fn osc_dispatch(&mut self, _: &Provenance<[&[u8]], Pty>) {}
    #[inline]
    fn dcs_hook(&mut self, _: &Provenance<[u16], Pty>, _: &Provenance<[u8], Pty>, _: u8) {}
    #[inline]
    fn dcs_put(&mut self, _: u8) {}
    #[inline]
    fn dcs_unhook(&mut self, _: bool) {}
    #[inline]
    fn apc_start(&mut self) {}
    #[inline]
    fn apc_put(&mut self, _: u8) {}
    #[inline]
    fn apc_end(&mut self, _: bool) {}
}
