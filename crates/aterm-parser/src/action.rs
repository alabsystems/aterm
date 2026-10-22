// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Parser actions and the sink trait.
//!
//! # PTY provenance (Phase 1 of #7877, issue #8005)
//!
//! Every slice-bearing argument on [`ActionSink`] is
//! wrapped in [`Provenance<_, Pty>`] (from `aterm-provenance`). This tags the
//! bytes at the type level as originating from an adversarial PTY, and the
//! Rust compiler then refuses to let them reach a `Host`-privileged sink
//! without an explicit `authorize_*` ceremony.
//!
//! The parser wraps each slice at its PTY entry point via
//! [`aterm_provenance::pty_wrap_ref`] (see `dispatch.rs`). That is the only
//! place the `Pty` tag is introduced; every downstream `ActionSink` method
//! already receives pre-tagged data.
//!
//! Implementers of `ActionSink` consume the wrapped values via `.as_ref()`
//! — the wrapper is `#[repr(transparent)]` so this is zero-cost. Scalars
//! (`u8`, `char`, `u16`, `bool`) remain unwrapped: the trait's hot paths
//! dispatch per-byte and the slice wrappers already protect the byte
//! payload carriers where escape-sequence injection is possible.

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
///
/// Only [`print`](Self::print), [`execute`](Self::execute) and
/// [`csi_dispatch`](Self::csi_dispatch) are required; the rest default to
/// no-ops or to those. Slice arguments are PTY data (`Provenance<_, Pty>`),
/// read with `.as_ref()`.
pub trait ActionSink {
    /// Print a character to the screen at the cursor position.
    fn print(&mut self, c: char);

    /// Bulk print a run of printable ASCII (`data` holds only 0x20-0x7E).
    /// Default: per-character `print`.
    #[inline]
    fn print_ascii_bulk(&mut self, data: &Provenance<[u8], Pty>) {
        for &b in data.as_ref() {
            self.print(b as char);
        }
    }

    /// Bulk print a run of two or more decoded non-ASCII characters, so a sink
    /// can amortize per-character work over it. Default: per-character `print`.
    #[inline]
    fn print_unicode_bulk(&mut self, chars: &Provenance<[char], Pty>) {
        for &c in chars.as_ref() {
            self.print(c);
        }
    }

    /// Execute a C0 control function (0x00-0x1F) or C1 (0x80-0x9F).
    fn execute(&mut self, byte: u8);

    /// A CSI sequence has been parsed.
    ///
    /// # Parameters
    /// - `params`: Numeric parameters (e.g., `[31]` for `ESC[31m`)
    /// - `intermediates`: Intermediate bytes (e.g., [?] for `ESC[?1h`)
    /// - `final_byte`: The final byte (e.g., 'm' for SGR)
    fn csi_dispatch(
        &mut self,
        params: &Provenance<[u16], Pty>,
        intermediates: &Provenance<[u8], Pty>,
        final_byte: u8,
    );

    /// A CSI sequence has been parsed (extended version with subparameter info).
    ///
    /// This method is called instead of `csi_dispatch` when subparameters are present.
    /// The `subparam_mask` indicates which params were preceded by a colon (`:`)
    /// rather than a semicolon (`;`), marking them as subparameters.
    ///
    /// # Parameters
    /// - `params`: Numeric parameters
    /// - `intermediates`: Intermediate bytes
    /// - `final_byte`: The final byte
    /// - `subparam_mask`: Bitmask where bit `i` is set if `params[i]` is a subparameter
    ///
    /// Default implementation calls `csi_dispatch`, ignoring subparam info.
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
    ///
    /// Default: no-op.
    #[inline]
    fn esc_dispatch(&mut self, _intermediates: &Provenance<[u8], Pty>, _final_byte: u8) {}

    /// An OSC sequence has been parsed.
    ///
    /// Default: no-op.
    #[inline]
    fn osc_dispatch(&mut self, _params: &Provenance<[&[u8]], Pty>) {}

    /// An OSC sequence has been parsed, with terminator type.
    ///
    /// `bel_terminated` is true when the OSC was ended by BEL (0x07),
    /// false when ended by ST (ESC \\ or C1 0x9C). Response-generating
    /// handlers (e.g., OSC 52 clipboard query) should echo the same
    /// terminator in their response (#7548).
    ///
    /// Default delegates to `osc_dispatch` for backward compatibility.
    #[inline]
    fn osc_dispatch_with_terminator(
        &mut self,
        params: &Provenance<[&[u8]], Pty>,
        _bel_terminated: bool,
    ) {
        self.osc_dispatch(params);
    }

    /// Start of a DCS sequence.
    ///
    /// Default: no-op.
    #[inline]
    fn dcs_hook(
        &mut self,
        _params: &Provenance<[u16], Pty>,
        _intermediates: &Provenance<[u8], Pty>,
        _final_byte: u8,
    ) {
    }

    /// Data byte within a DCS sequence.
    ///
    /// Default: no-op.
    #[inline]
    fn dcs_put(&mut self, _byte: u8) {}

    /// Bulk data bytes within a DCS sequence.
    ///
    /// Default falls back to per-byte `dcs_put`. Implementations can
    /// override for better throughput on Sixel, tmux passthrough, etc.
    #[inline]
    fn dcs_put_bulk(&mut self, data: &Provenance<[u8], Pty>) {
        for &b in data.as_ref() {
            self.dcs_put(b);
        }
    }

    /// End of a DCS sequence.
    ///
    /// `canceled` is `true` when the string was aborted by CAN (0x18) or SUB
    /// (0x1A), or broken off by an ESC that does not begin ST (`ESC [`, a second
    /// ESC, ...), rather than terminated by ST (`ESC \` or 0x9C): the sink should
    /// DISCARD any accumulated payload (e.g. drop a half-decoded Sixel image,
    /// leave a query unanswered) instead of finalizing it. `false` for ST. An
    /// ESC's verdict waits for the byte after it, so the call comes with that
    /// byte, before whatever it dispatches.
    ///
    /// Default: no-op.
    #[inline]
    fn dcs_unhook(&mut self, _canceled: bool) {}

    /// Start of an APC (Application Program Command): ESC _ or 0x9F, up to ST.
    ///
    /// Default: no-op.
    #[inline]
    fn apc_start(&mut self) {}

    /// Data byte within an APC sequence.
    ///
    /// Default: no-op.
    #[inline]
    fn apc_put(&mut self, _byte: u8) {}

    /// Bulk data bytes within an APC sequence.
    ///
    /// Default falls back to per-byte `apc_put`. Implementations can
    /// override for better throughput on Kitty graphics payloads, which
    /// stream multi-MB base64 through this path (kitten icat, chafa, timg).
    #[inline]
    fn apc_put_bulk(&mut self, data: &Provenance<[u8], Pty>) {
        for &b in data.as_ref() {
            self.apc_put(b);
        }
    }

    /// End of an APC sequence. `canceled`: CAN or SUB ended it, so discard the
    /// payload (a truncated kitty graphics command), as
    /// [`dcs_unhook`](Self::dcs_unhook) does.
    ///
    /// Default: no-op.
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
