// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Parser actions and sink trait.
//!
//! # PTY provenance (Phase 1 of #7877, issue #8005)
//!
//! Every slice-bearing argument on [`ActionSink`] and [`BatchActionSink`] is
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

/// Trait for receiving parser actions.
///
/// Implement this trait to handle escape sequences from the parser.
///
/// Only three methods are required: [`print`](Self::print),
/// [`execute`](Self::execute), and [`csi_dispatch`](Self::csi_dispatch).
/// All other methods have default no-op implementations, so simple consumers
/// only need to implement what they care about.
///
/// # Provenance
///
/// Slice arguments are tagged `Provenance<_, Pty>` to mark them as
/// adversarial PTY data at the type level (Phase 1 of #7877, #8005).
/// Implementers consume them via `.as_ref()` and MUST NOT forward them to a
/// host-privileged sink without going through an `authorize_*` ceremony.
pub trait ActionSink {
    /// Print a character to the screen at the cursor position.
    fn print(&mut self, c: char);

    /// Bulk print ASCII bytes (0x20-0x7E) in one call.
    ///
    /// Default implementation falls back to per-character `print()`.
    /// Implementations can override for better performance by avoiding
    /// per-character overhead for runs of ASCII text.
    ///
    /// # Safety
    /// The `data` slice is guaranteed to contain only printable ASCII (0x20-0x7E).
    #[inline]
    fn print_ascii_bulk(&mut self, data: &Provenance<[u8], Pty>) {
        for &b in data.as_ref() {
            self.print(b as char);
        }
    }

    /// Bulk print decoded non-ASCII characters in one call.
    ///
    /// Default implementation falls back to per-character `print()`.
    /// Implementations can override to amortize per-character overhead
    /// (charset, clipboard, char_width, style checks) over a run of
    /// consecutive non-ASCII characters decoded from UTF-8.
    ///
    /// Called by the parser when it decodes 2+ consecutive multi-byte
    /// UTF-8 sequences in its inline fast path.
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
    fn esc_dispatch(&mut self, _intermediates: &Provenance<[u8], Pty>, _final_byte: u8) {}

    /// An OSC sequence has been parsed.
    ///
    /// Default: no-op.
    fn osc_dispatch(&mut self, _params: &Provenance<[&[u8]], Pty>) {}

    /// An OSC sequence has been parsed, with terminator type.
    ///
    /// `bel_terminated` is true when the OSC was ended by BEL (0x07),
    /// false when ended by ST (ESC \\ or C1 0x9C). Response-generating
    /// handlers (e.g., OSC 52 clipboard query) should echo the same
    /// terminator in their response (#7548).
    ///
    /// Default delegates to `osc_dispatch` for backward compatibility.
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
    fn dcs_put(&mut self, _byte: u8) {}

    /// Bulk data bytes within a DCS sequence.
    ///
    /// Default falls back to per-byte `dcs_put`. Implementations can
    /// override for better throughput on Sixel, tmux passthrough, etc.
    fn dcs_put_bulk(&mut self, data: &Provenance<[u8], Pty>) {
        for &b in data.as_ref() {
            self.dcs_put(b);
        }
    }

    /// End of a DCS sequence.
    ///
    /// `canceled` is `true` when the string was aborted by CAN (0x18) or SUB
    /// (0x1A) rather than terminated by ST / `ESC \`: the sink should DISCARD
    /// any accumulated payload (e.g. drop a half-decoded Sixel image) instead of
    /// finalizing it. `false` for a legitimate terminator.
    ///
    /// Default: no-op.
    fn dcs_unhook(&mut self, _canceled: bool) {}

    /// Start of an APC (Application Program Command) sequence.
    ///
    /// Called when ESC _ or 0x9F is received. The sequence continues
    /// until ST (ESC \ or 0x9C) is received.
    ///
    /// Default: no-op.
    fn apc_start(&mut self) {}

    /// Data byte within an APC sequence.
    ///
    /// Default: no-op.
    fn apc_put(&mut self, _byte: u8) {}

    /// Bulk data bytes within an APC sequence.
    ///
    /// Default falls back to per-byte `apc_put`. Implementations can
    /// override for better throughput on Kitty graphics payloads, which
    /// stream multi-MB base64 through this path (kitten icat, chafa, timg).
    fn apc_put_bulk(&mut self, data: &Provenance<[u8], Pty>) {
        for &b in data.as_ref() {
            self.apc_put(b);
        }
    }

    /// End of an APC sequence.
    ///
    /// Default: no-op.
    fn apc_end(&mut self) {}
}

/// Extended trait for batch-optimized action handling.
///
/// This trait adds `print_str` for handling runs of printable ASCII
/// in a single call, which can be more efficient than per-character
/// `print` calls.
///
/// Test-only: production uses `advance_fast`/`ActionSink`. The batch path
/// (`advance_batch`) — the only consumer of this trait — is `#[cfg(test)]`,
/// so the trait and its impls are gated out of release builds.
#[cfg(test)]
pub trait BatchActionSink: ActionSink {
    /// Print a string of characters.
    ///
    /// The string is guaranteed to contain only printable ASCII (0x20-0x7E).
    /// This allows efficient batch processing without per-character overhead.
    fn print_str(&mut self, s: &Provenance<str, Pty>);
}

/// A sink that discards all actions.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullSink;

impl ActionSink for NullSink {
    fn print(&mut self, _: char) {}
    fn print_ascii_bulk(&mut self, _: &Provenance<[u8], Pty>) {}
    fn execute(&mut self, _: u8) {}
    fn csi_dispatch(&mut self, _: &Provenance<[u16], Pty>, _: &Provenance<[u8], Pty>, _: u8) {}
    fn esc_dispatch(&mut self, _: &Provenance<[u8], Pty>, _: u8) {}
    fn osc_dispatch(&mut self, _: &Provenance<[&[u8]], Pty>) {}
    fn dcs_hook(&mut self, _: &Provenance<[u16], Pty>, _: &Provenance<[u8], Pty>, _: u8) {}
    fn dcs_put(&mut self, _: u8) {}
    fn dcs_unhook(&mut self, _: bool) {}
    fn apc_start(&mut self) {}
    fn apc_put(&mut self, _: u8) {}
    fn apc_end(&mut self) {}
}

#[cfg(test)]
impl BatchActionSink for NullSink {
    fn print_str(&mut self, _: &Provenance<str, Pty>) {}
}
