// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Streaming output sanitizer for [`OutputCapability::Filtered`] containment mode.
//!
//! In the `Containment` tier of the four-mode policy, the hosted program is
//! assumed hostile. Raw escape sequences it emits can be used to:
//!
//! - Rewrite the clipboard (`OSC 52`)
//! - Spoof the window title (`OSC 0`/`OSC 1`/`OSC 2`)
//! - Inject arbitrary hyperlinks (`OSC 8`)
//! - Encode tmux passthrough / Sixel / Kitty image data (DCS)
//! - Run generic application program commands (APC — used by terminal AI
//!   features in some emulators and unsafe to trust from a hostile agent)
//! - Probe or respond to status sequences via SOS/PM strings
//!
//! This module implements a deliberately conservative, first-wave
//! sanitizer that, **when invoked**, strips all OSC, DCS, APC, SOS, and PM
//! sequences before the bytes would reach the terminal emulator or the
//! attached clients.
//!
//! **Enforcement status (HONEST):** as of this increment [`OutputSanitizer`]
//! has NO production caller — it is NOT wired into any PTY or output path, so
//! no output is filtered at runtime, and input filtering is unimplemented. This
//! type is a model/proof artifact backing the [`OutputCapability::Filtered`]
//! policy tier; wiring it into the reader loop is an outstanding follow-up.
//!
//! # What is stripped (dropped from the stream)
//!
//! - Operating System Command sequences: `ESC ]` … `BEL` or `ESC ]` … `ST`
//! - Device Control Strings: `ESC P` … `ST`
//! - Application Program Commands: `ESC _` … `ST`
//! - Start Of String strings: `ESC X` … `ST`
//! - Privacy Message strings: `ESC ^` … `ST`
//!
//! # A note on C1 single-byte introducers (0x80-0x9F)
//!
//! Modern terminals run in UTF-8 mode, where the byte range 0x80-0xBF is
//! reserved for UTF-8 continuation bytes. The aterm parser follows this:
//! by default `c1_controls_enabled = false`, so naked 0x90/0x9D/0x9F
//! bytes are ignored by the terminal emulator itself. To avoid corrupting
//! legitimate UTF-8 text (where, e.g., the emoji 🌍 encodes as `F0 9F 8C
//! 8D` and contains 0x9F as a continuation byte), this sanitizer matches
//! the emulator's default and does **not** treat naked C1 bytes as
//! sequence introducers. Only the 7-bit ESC-prefixed forms
//! (`ESC ]`, `ESC P`, `ESC X`, `ESC ^`, `ESC _`) are stripped. This is
//! consistent with xterm's and Terminal's behavior in UTF-8 mode.
//!
//! If a future deployment enables C1-controls-in-parser, the sanitizer
//! must be extended to match — but doing so unconditionally here would
//! break UTF-8 text, which is the worse of two failure modes (silent
//! text corruption vs. ignoring a rare and parser-disabled code path).
//!
//! # What is preserved (passes through)
//!
//! - Printable bytes (text, UTF-8 content)
//! - C0 controls (0x00-0x1F including CR, LF, TAB, BS, ESC)
//! - CSI sequences (`ESC [` … final byte) — needed for cursor movement,
//!   SGR styling, erase operations
//! - ESC sequences without an intermediate OSC/DCS/APC/SOS/PM role
//!   (e.g. `ESC 7` save cursor, `ESC =` keypad, `ESC \\` String Terminator
//!   when encountered standalone)
//! - DEL (0x7F)
//!
//! # Trade-offs
//!
//! This is a **deny-OSC/DCS/APC/SOS/PM** sanitizer, not a full allowlist.
//! CSI sequences pass through even though some CSI variants can still be
//! annoying (e.g. cursor teleportation, scroll-region abuse). A stricter
//! allowlist sanitizer is tracked as follow-up work. NOTE: an earlier draft
//! cited an `aterm-daemon` reader loop (`aterm-daemon/src/session/reader.rs`)
//! as the integration point; that crate no longer exists, and there is
//! currently NO production caller — wiring this sanitizer into the PTY/output
//! path is the outstanding follow-up.
//!
//! # Streaming guarantee
//!
//! The sanitizer processes bytes incrementally. A sequence split across
//! multiple [`OutputSanitizer::sanitize`] calls is still fully stripped.
//! This matches how PTY bytes arrive: a single `OSC 52;c;<payload> BEL`
//! may span multiple 4 KiB reads.
//!
//! # Fail-closed property
//!
//! If the sanitizer is mid-OSC/DCS/APC/SOS/PM when a call returns, the
//! partially-stripped state is carried into the next call. No bytes from
//! a stripped sequence are ever emitted, even on premature stream end or
//! mid-byte truncation.

#![allow(
    clippy::must_use_candidate,
    reason = "sanitizer constructors are expected to be used but need not panic if ignored"
)]

use core::mem;

/// Streaming sanitizer for `OutputCapability::Filtered` containment mode.
///
/// See module documentation for the full stripping policy. This type is
/// `Clone` and `Debug` but **not** `Copy` — mutation through `&mut self`
/// is required so the internal state machine can advance across calls.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct OutputSanitizer {
    state: SanitizerState,
    /// Drop-counter. Counts stripped bytes of the currently-active
    /// sequence (for diagnostics / future rate limiting). Not exposed
    /// publicly beyond [`Self::stripped_bytes`].
    stripped: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum SanitizerState {
    /// Normal byte stream. Bytes forwarded unchanged unless they enter an
    /// OSC/DCS/APC/SOS/PM sequence.
    #[default]
    Ground,
    /// Saw `ESC` (0x1B). Next byte decides whether this is an OSC/DCS/APC/
    /// SOS/PM intro (which we strip) or any other ESC sequence (which we
    /// pass through, including the `ESC` byte we held back).
    EscapePending,
    /// Inside an OSC string (entered via `ESC ]` or the C1 OSC `0x9D`).
    /// Terminates on `BEL` or `ESC \\` (ST). Any other byte is dropped.
    OscString,
    /// Inside an OSC string and just saw `ESC`. If followed by `\`, this
    /// is the ST terminator; anything else is invalid OSC content which
    /// we drop and remain in OSC.
    OscStringEsc,
    /// Inside a DCS string (entered via `ESC P` or C1 DCS `0x90`). Runs
    /// until `ESC \\` (ST). Any other byte is dropped.
    DcsString,
    /// Inside a DCS string and just saw `ESC`.
    DcsStringEsc,
    /// Inside SOS/PM/APC string (entered via `ESC X`, `ESC ^`, `ESC _`,
    /// or their C1 equivalents). All bytes dropped until ST.
    SosPmApcString,
    /// Inside SOS/PM/APC string and just saw `ESC`.
    SosPmApcStringEsc,
}

/// C0 control code constants used by the sanitizer.
///
/// Note: C1 single-byte introducers (0x90, 0x98, 0x9D, 0x9E, 0x9F) are
/// **intentionally** not treated as ground-state introducers here — see
/// module docs. UTF-8 mode reclaims that byte range for continuation
/// bytes, and aterm's parser does not honor naked C1 controls by default.
///
/// However, once already inside a stripped sequence (OSC/DCS/APC/SOS/PM),
/// we accept the C1 String Terminator (0x9C) as a terminator. This is
/// safe because:
/// 1. We only reach those states via a 7-bit `ESC`-prefixed introducer,
///    which cannot appear inside a valid UTF-8 multi-byte sequence (ESC
///    is ASCII).
/// 2. Inside those states every byte is being dropped anyway; treating
///    0x9C as a terminator only shortens what we drop.
const BEL: u8 = 0x07;
const ESC: u8 = 0x1B;
const C1_ST: u8 = 0x9C;

impl OutputSanitizer {
    /// Construct a fresh sanitizer in the [`SanitizerState::Ground`] state.
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `true` if the sanitizer is currently mid-sequence.
    ///
    /// Useful for tests and diagnostics. A sanitizer that is not in the
    /// ground state is silently swallowing bytes — this is expected for
    /// streams that legitimately span multiple read chunks.
    #[inline]
    pub fn in_sequence(&self) -> bool {
        !matches!(self.state, SanitizerState::Ground)
    }

    /// Total number of bytes dropped by this sanitizer over its lifetime.
    ///
    /// Counts every byte of every stripped OSC/DCS/APC/SOS/PM sequence,
    /// including the introducer and terminator. Intended for diagnostics
    /// and denial logging.
    #[inline]
    pub fn stripped_bytes(&self) -> u64 {
        self.stripped
    }

    /// Sanitize a slice of PTY output, returning the filtered byte stream.
    ///
    /// The returned `Vec<u8>` contains only bytes that should be forwarded
    /// to the terminal emulator and/or attached clients. All dangerous
    /// OSC/DCS/APC/SOS/PM content — whether wholly contained in `input`
    /// or split across earlier/later calls — is dropped.
    ///
    /// Allocates a single output buffer sized to `input.len()` (pre-sized up
    /// to 64 KiB; larger inputs grow amortized past that). For an all-benign
    /// PTY-read-sized chunk this is a straight copy (no reallocations); for
    /// streams dominated by stripped sequences the returned vector is
    /// smaller.
    pub fn sanitize(&mut self, input: &[u8]) -> Vec<u8> {
        // Capacity hint clamped to 64 KiB: an input-length-derived capacity is
        // unbounded under the verifier's open model and refutes the
        // bulk-allocation obligation, so each `with_capacity` call site is
        // directly dominated by a bound check (a hoisted clamp loses the bound
        // across the join). PTY read chunks are at most tens of KiB, so the
        // pre-size is unchanged in practice; larger inputs merely reallocate
        // on growth. Capacity is not observable behavior.
        let mut out = if input.len() < 65536 {
            Vec::with_capacity(input.len())
        } else {
            Vec::with_capacity(65536)
        };
        self.sanitize_into(input, &mut out);
        out
    }

    /// Sanitize a slice of PTY output, appending to an existing buffer.
    ///
    /// This is the zero-alloc variant when a reusable `Vec<u8>` scratch
    /// buffer is available (e.g. in a hot PTY reader loop).
    pub fn sanitize_into(&mut self, input: &[u8], out: &mut Vec<u8>) {
        for &byte in input {
            self.feed(byte, out);
        }
    }

    /// Feed a single byte. Appends to `out` iff the byte should pass the
    /// filter. All stripped bytes are counted in [`Self::stripped`].
    #[inline]
    fn feed(&mut self, byte: u8, out: &mut Vec<u8>) {
        // Using mem::take to let the match arms freely mutate self while
        // still having the old state available.
        let prev = mem::take(&mut self.state);
        self.state = match prev {
            SanitizerState::Ground => Self::feed_ground(byte, out),
            SanitizerState::EscapePending => self.feed_escape_pending(byte, out),
            SanitizerState::OscString => self.feed_osc(byte),
            SanitizerState::OscStringEsc => self.feed_osc_esc(byte),
            SanitizerState::DcsString => self.feed_dcs(byte),
            SanitizerState::DcsStringEsc => self.feed_dcs_esc(byte),
            SanitizerState::SosPmApcString => self.feed_sos_pm_apc(byte),
            SanitizerState::SosPmApcStringEsc => self.feed_sos_pm_apc_esc(byte),
        };
    }

    #[inline]
    fn feed_ground(byte: u8, out: &mut Vec<u8>) -> SanitizerState {
        if byte == ESC {
            // Hold back the ESC — the next byte decides whether this is
            // an OSC/DCS/APC/SOS/PM intro or a benign ESC sequence.
            SanitizerState::EscapePending
        } else {
            // Pass all other bytes through — including 0x80-0x9F, which
            // are UTF-8 continuation bytes in modern terminals.
            out.push(byte);
            SanitizerState::Ground
        }
    }

    #[inline]
    fn feed_escape_pending(&mut self, byte: u8, out: &mut Vec<u8>) -> SanitizerState {
        match byte {
            // OSC introducer: ESC ] — drop the ESC we held back too.
            b']' => {
                self.stripped = self.stripped.saturating_add(2);
                SanitizerState::OscString
            }
            // DCS introducer: ESC P
            b'P' => {
                self.stripped = self.stripped.saturating_add(2);
                SanitizerState::DcsString
            }
            // SOS, PM, APC introducers: ESC X, ESC ^, ESC _
            b'X' | b'^' | b'_' => {
                self.stripped = self.stripped.saturating_add(2);
                SanitizerState::SosPmApcString
            }
            // Any other ESC sequence (CSI, charset select, single-shift,
            // cursor save/restore, keypad mode, etc.) — emit the held-back
            // ESC and the current byte and return to ground. A stray
            // second ESC begins a new pending sequence.
            ESC => {
                out.push(ESC);
                SanitizerState::EscapePending
            }
            _ => {
                out.push(ESC);
                out.push(byte);
                SanitizerState::Ground
            }
        }
    }

    // ---- OSC ----

    #[inline]
    fn feed_osc(&mut self, byte: u8) -> SanitizerState {
        self.stripped = self.stripped.saturating_add(1);
        match byte {
            BEL | C1_ST => SanitizerState::Ground,
            ESC => SanitizerState::OscStringEsc,
            _ => SanitizerState::OscString,
        }
    }

    #[inline]
    fn feed_osc_esc(&mut self, byte: u8) -> SanitizerState {
        self.stripped = self.stripped.saturating_add(1);
        match byte {
            // ESC \ = String Terminator — end of OSC.
            b'\\' => SanitizerState::Ground,
            // Nested ESC — stay in OSC-ESC; only `\` can terminate.
            ESC => SanitizerState::OscStringEsc,
            // Any other byte — back to plain OSC string, keep dropping.
            _ => SanitizerState::OscString,
        }
    }

    // ---- DCS ----

    #[inline]
    fn feed_dcs(&mut self, byte: u8) -> SanitizerState {
        self.stripped = self.stripped.saturating_add(1);
        match byte {
            C1_ST => SanitizerState::Ground,
            ESC => SanitizerState::DcsStringEsc,
            _ => SanitizerState::DcsString,
        }
    }

    #[inline]
    fn feed_dcs_esc(&mut self, byte: u8) -> SanitizerState {
        self.stripped = self.stripped.saturating_add(1);
        match byte {
            b'\\' => SanitizerState::Ground,
            ESC => SanitizerState::DcsStringEsc,
            _ => SanitizerState::DcsString,
        }
    }

    // ---- SOS / PM / APC ----

    #[inline]
    fn feed_sos_pm_apc(&mut self, byte: u8) -> SanitizerState {
        self.stripped = self.stripped.saturating_add(1);
        match byte {
            C1_ST => SanitizerState::Ground,
            ESC => SanitizerState::SosPmApcStringEsc,
            _ => SanitizerState::SosPmApcString,
        }
    }

    #[inline]
    fn feed_sos_pm_apc_esc(&mut self, byte: u8) -> SanitizerState {
        self.stripped = self.stripped.saturating_add(1);
        match byte {
            b'\\' => SanitizerState::Ground,
            ESC => SanitizerState::SosPmApcStringEsc,
            _ => SanitizerState::SosPmApcString,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sanitize(input: &[u8]) -> Vec<u8> {
        let mut s = OutputSanitizer::new();
        s.sanitize(input)
    }

    #[test]
    fn passes_plain_text_unchanged() {
        let out = sanitize(b"hello world\n");
        assert_eq!(out, b"hello world\n");
    }

    #[test]
    fn preserves_csi_sgr_coloring() {
        // Red foreground via SGR — must survive.
        let input = b"\x1b[31mred\x1b[0m";
        assert_eq!(sanitize(input), input);
    }

    #[test]
    fn preserves_csi_cursor_move() {
        let input = b"\x1b[2J\x1b[H\x1b[10;5Hxx";
        assert_eq!(sanitize(input), input);
    }

    #[test]
    fn strips_osc_title_bel_terminated() {
        // OSC 2 ; hostile-title BEL — common window-title hijack.
        let input = b"before\x1b]2;EVIL TITLE\x07after";
        assert_eq!(sanitize(input), b"beforeafter");
    }

    #[test]
    fn strips_osc_title_st_terminated() {
        let input = b"before\x1b]2;TITLE\x1b\\after";
        assert_eq!(sanitize(input), b"beforeafter");
    }

    #[test]
    fn strips_osc52_clipboard_write() {
        // OSC 52 ; c ; base64-of-payload BEL — clipboard write primitive.
        let input = b"\x1b]52;c;aGVsbG8gd29ybGQ=\x07okay";
        assert_eq!(sanitize(input), b"okay");
    }

    #[test]
    fn strips_osc8_hyperlink() {
        // OSC 8 ; ; URL ST TEXT OSC 8 ; ; ST — the whole hyperlink wrapping
        // is OSC. The inner TEXT is preserved.
        let input = b"\x1b]8;;https://evil.example\x1b\\TEXT\x1b]8;;\x1b\\";
        assert_eq!(sanitize(input), b"TEXT");
    }

    #[test]
    fn strips_dcs_tmux_passthrough() {
        // DCS tmux pass-through: ESC P tmux; <inner> ESC \
        let input = b"pre\x1bPtmux;\x1b]0;inner\x07\x1b\\post";
        assert_eq!(sanitize(input), b"prepost");
    }

    #[test]
    fn strips_apc() {
        let input = b"a\x1b_some apc payload\x1b\\b";
        assert_eq!(sanitize(input), b"ab");
    }

    #[test]
    fn strips_sos() {
        let input = b"a\x1bXpayload\x1b\\b";
        assert_eq!(sanitize(input), b"ab");
    }

    #[test]
    fn strips_pm() {
        let input = b"a\x1b^payload\x1b\\b";
        assert_eq!(sanitize(input), b"ab");
    }

    #[test]
    fn c1_naked_bytes_pass_through_for_utf8_safety() {
        // Emoji 🌍 = F0 9F 8C 8D — contains 0x9F. The sanitizer must not
        // confuse continuation bytes for C1 APC intros. See module docs.
        let emoji = "🌍".as_bytes();
        assert_eq!(sanitize(emoji), emoji);
        // Similarly for code points that include other C1-range bytes.
        let mixed = "café 😀".as_bytes();
        assert_eq!(sanitize(mixed), mixed);
    }

    #[test]
    fn streaming_preserves_split_osc() {
        let mut s = OutputSanitizer::new();
        let mut out = Vec::new();
        // Split an OSC across three calls.
        s.sanitize_into(b"hello\x1b]", &mut out);
        s.sanitize_into(b"0;title", &mut out);
        s.sanitize_into(b"\x07world", &mut out);
        assert_eq!(out, b"helloworld");
    }

    #[test]
    fn streaming_preserves_split_esc() {
        // ESC delivered alone, then the terminator of a CSI sequence.
        let mut s = OutputSanitizer::new();
        let mut out = Vec::new();
        s.sanitize_into(b"a\x1b", &mut out);
        s.sanitize_into(b"[31mX", &mut out);
        assert_eq!(out, b"a\x1b[31mX");
    }

    #[test]
    fn preserves_esc_non_string_introducers() {
        // ESC 7 = DECSC, ESC = = keypad, ESC c = RIS — none should be
        // stripped (they aren't string intros).
        let input = b"\x1b7before\x1b=\x1bc";
        assert_eq!(sanitize(input), input);
    }

    #[test]
    fn double_esc_escapes_first() {
        // Two ESCs in a row: the first is a cancelled pending escape that
        // emits itself; the second starts a new pending escape.
        let mut s = OutputSanitizer::new();
        let mut out = Vec::new();
        s.sanitize_into(b"\x1b\x1b[31mX", &mut out);
        // First ESC emits because the next byte was ESC (not a string intro);
        // then `[31mX` is a CSI SGR sequence which must survive.
        assert_eq!(out, b"\x1b\x1b[31mX");
    }

    #[test]
    fn counts_stripped_bytes() {
        let mut s = OutputSanitizer::new();
        // 6 stripped bytes: `\x1b` + `]` + `2;X` + `\x07`
        let _ = s.sanitize(b"\x1b]2;X\x07keep");
        assert!(s.stripped_bytes() >= 6);
    }

    #[test]
    fn in_sequence_tracks_state() {
        let mut s = OutputSanitizer::new();
        let _ = s.sanitize(b"\x1b]2;partial");
        assert!(s.in_sequence(), "expected sanitizer mid-OSC");
        let _ = s.sanitize(b"\x07");
        assert!(!s.in_sequence(), "expected ground state after BEL");
    }

    #[test]
    fn bare_esc_at_end_is_held_until_next_chunk() {
        // A trailing ESC with no follow-up is held until next sanitize()
        // call. If the stream ends there, the byte is effectively dropped,
        // but no invalid byte is emitted prematurely.
        let mut s = OutputSanitizer::new();
        let out = s.sanitize(b"hi\x1b");
        assert_eq!(out, b"hi");
        assert!(s.in_sequence());
        // Next byte is `[` — it becomes the CSI intro, and both ESC and `[`
        // emit.
        let out2 = s.sanitize(b"[0mX");
        assert_eq!(out2, b"\x1b[0mX");
    }

    #[test]
    fn empty_input_is_noop() {
        let mut s = OutputSanitizer::new();
        let out = s.sanitize(&[]);
        assert!(out.is_empty());
        assert!(!s.in_sequence());
    }

    #[test]
    fn preserves_utf8_multibyte() {
        let input = "héllo 你好 🌍\n".as_bytes();
        assert_eq!(sanitize(input), input);
    }

    #[test]
    fn strips_consecutive_osc_sequences() {
        let input = b"\x1b]0;A\x07\x1b]0;B\x07\x1b]0;C\x07end";
        assert_eq!(sanitize(input), b"end");
    }

    #[test]
    fn strips_nested_like_osc_with_inner_esc() {
        // OSC string may contain arbitrary bytes except BEL/ST, including
        // embedded ESC bytes that are not followed by `\`. The sanitizer
        // must treat those as OSC content, not a terminator.
        let input = b"pre\x1b]8;;http://x\x1bblah\x1b\\post";
        // ESC-but-not-\ inside OSC stays as OSC content.
        assert_eq!(sanitize(input), b"prepost");
    }
}
