// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Parser actions and the sink trait.

use aterm_provenance::{Provenance, Pty, pty_wrap_ref};

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

    /// Bulk print a run of two or more decoded characters that begins with a non-ASCII one and may
    /// hold printable ASCII (0x20-0x7E) after it: the spaces and punctuation between words, which
    /// the parser joins into the run where a multi-byte sequence follows them.
    #[inline]
    fn print_unicode_bulk(&mut self, chars: &Provenance<[char], Pty>) {
        for &c in chars.as_ref() {
            self.print(c);
        }
    }

    /// Execute a C0 control function (0x00-0x1F) or C1 (0x80-0x9F).
    fn execute(&mut self, byte: u8);

    /// Whether this sink takes a run of text lines whole ([`ActionSink::print_lines`]). The
    /// parser hands a run over only to a sink that says so; any other sink gets the run's prints
    /// and line controls one call at a time, as it always has.
    const TAKES_LINES: bool = false;

    /// A run of text lines in the ground state, from a line feed on: `lines` starts with LF and
    /// holds only printable ASCII (`0x20..=0x7E`), CR and LF. It stands for exactly what
    /// [`deliver_lines`] makes of it, which is what the parser delivers for those bytes one at a
    /// time: each maximal printable run as one `print_ascii_bulk` and each CR and LF as an
    /// `execute`, in order. A sink overrides it to take the run in fewer steps.
    #[inline]
    fn print_lines(&mut self, lines: &Provenance<[u8], Pty>) {
        deliver_lines(self, lines.as_ref());
    }

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

/// Deliver a run of text lines (`lines` holds only printable ASCII, CR and LF) to `sink` one
/// action at a time: each maximal printable run as one `print_ascii_bulk`, each CR and LF as an
/// `execute`. [`ActionSink::print_lines`] means this, and a sink that cannot take a run whole
/// at the moment falls back on it.
pub fn deliver_lines<S: ActionSink + ?Sized>(sink: &mut S, lines: &[u8]) {
    let mut rest = lines;
    while !rest.is_empty() {
        let (text, tail) = rest.split_at(printable_len(rest));
        if !text.is_empty() {
            sink.print_ascii_bulk(pty_wrap_ref(text));
        }
        let Some((&control, after)) = tail.split_first() else {
            return;
        };
        sink.execute(control);
        rest = after;
    }
}

/// The length of `bytes`' printable-ASCII (`0x20..=0x7E`) prefix, found with the parser's own
/// scanner: in a run of text lines, the text before the next CR or LF. The first two bytes are
/// read one at a time first, so a line of at most one glyph (`yes`, a region's scroll) is
/// measured without the chunked scanner, whose setup costs more than such a line.
#[must_use]
#[inline]
pub fn printable_len(bytes: &[u8]) -> usize {
    match bytes {
        [b0, ..] if !(0x20..=0x7E).contains(b0) => 0,
        [_, b1, ..] if !(0x20..=0x7E).contains(b1) => 1,
        _ => bytes
            .get(2..)
            .map_or(bytes.len(), |rest| 2 + crate::simd::count_printable(rest)),
    }
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
