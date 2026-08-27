// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Pure mouse encoding functions for terminal mouse reporting.
//!
//! These functions produce the byte sequences for X10, UTF-8, SGR, and URXVT
//! mouse encoding formats. All coordinates are 0-indexed; the encoding adds
//! the +1 offset required by each protocol.

use super::types::MouseEncoding;

/// Encode mouse coordinates in X10 format.
///
/// Format: ESC [ M Cb Cx Cy
/// Coordinates are 1-indexed and offset by 32 (space character).
/// Limited to column/row 223 (223 + 32 = 255 = max single byte).
/// Coordinates beyond 223 are clamped. Applications needing larger
/// terminal support should enable SGR encoding (mode 1006) instead.
#[must_use]
pub fn encode_x10(cb: u8, col: u16, row: u16) -> Vec<u8> {
    let cx = (col.saturating_add(1).min(223) as u8).saturating_add(32);
    let cy = (row.saturating_add(1).min(223) as u8).saturating_add(32);
    vec![0x1b, b'[', b'M', cb.saturating_add(32), cx, cy]
}

/// Encode a single coordinate as UTF-8 for mouse encoding mode 1005.
///
/// For coordinates <= 95, outputs a single byte (coord + 32).
/// For coordinates 96-2015, outputs a 2-byte UTF-8 sequence.
#[allow(clippy::cast_possible_truncation)]
fn encode_utf8_coord(coord: u16, output: &mut Vec<u8>) {
    let c = coord.saturating_add(32);
    if c < 128 {
        output.push(c as u8);
    } else {
        let c = c.min(2047);
        // Mask to a byte before the cast so the narrowing is provably lossless
        // (`c <= 2047`, so `c >> 6 <= 31`; `& 0xFF` is identity here).
        output.push(0xC0 | (((c >> 6) & 0xFF) as u8));
        output.push(0x80 | ((c & 0x3F) as u8));
    }
}

/// Encode mouse coordinates in UTF-8 format.
///
/// Format: ESC [ M Cb Cx Cy (all values after M are UTF-8 encoded)
/// Like X10 but uses UTF-8 encoding for values > 127, supporting up to 2015.
///
/// Per xterm: ALL values after M — including the button code — are UTF-8
/// encoded. When modifier masks (Shift=4, Alt=8, Ctrl=16) combine with
/// scroll wheel (64+) or additional buttons (128+), cb+32 can exceed 127
/// and requires 2-byte UTF-8 encoding. (#7498)
#[must_use]
pub fn encode_utf8(cb: u8, col: u16, row: u16) -> Vec<u8> {
    let mut result = vec![0x1b, b'[', b'M'];
    // Button code is also UTF-8 encoded (not a raw byte)
    encode_utf8_coord(u16::from(cb), &mut result);
    encode_utf8_coord(col.saturating_add(1), &mut result);
    encode_utf8_coord(row.saturating_add(1), &mut result);
    result
}

/// Append `val` to `buf` as decimal digits, allocation-free.
///
/// The zero-allocation counterpart of `val.to_string()`: the mouse encoders run
/// once per pointer sample under DEC 1002/1003 tracking, so the three throwaway
/// `String`s the `to_string()` form allocated per report are pure waste. Same
/// idiom (and same Trust discipline) as `keyboard::encode::write_u32`.
// Skip: pushes into a `Vec<u8>` — absent std body (alloc class). The emitted
// bytes are pinned by this module's unit tests and by the protocol vector suite.
#[cfg_attr(trust_verify, trust::skip)]
#[allow(clippy::cast_possible_truncation)]
fn push_dec_u16(buf: &mut Vec<u8>, val: u16) {
    if val == 0 {
        buf.push(b'0');
        return;
    }
    // Extract decimal digits least-significant-first into a fixed 8-slot scratch
    // (u16::MAX is 5 digits), then append most-significant-first. Like `write_u32`
    // this uses only the literal divisor 10 (no runtime-divisor division for the
    // Trust gate to guard against a zero divisor), masks every scratch index into
    // the array's 0..=7 range, masks the remainder to a byte before the cast so the
    // narrowing is provably lossless (`n % 10 <= 9`, so `& 0xFF` is identity), and
    // forms each byte with `wrapping_add` (the digit is 0..=9, so it never actually
    // wraps) — no div-by-zero, index-out-of-bounds, or `b'0' + digit` overflow
    // obligation.
    let mut digits = [0u8; 8];
    let mut n = val;
    let mut count = 0usize;
    while n > 0 && count < 5 {
        digits[count & 7] = b'0'.wrapping_add(((n % 10) & 0xFF) as u8);
        n /= 10;
        // saturating: `count < 5` guards the increment and `count > 0` the
        // decrement — exact on every path; the verifier cannot chain either
        // loop condition into the arithmetic.
        count = count.saturating_add(1);
    }
    while count > 0 {
        count = count.saturating_sub(1);
        buf.push(digits[count & 7]);
    }
}

/// Encode mouse coordinates in SGR format.
///
/// Format: ESC [ < Cb ; Cx ; Cy M (press) or ESC [ < Cb ; Cx ; Cy m (release)
/// Coordinates are 1-indexed decimal parameters, no offset needed.
#[must_use]
// Skip: Vec push/extend — absent std bodies (alloc class).
#[cfg_attr(trust_verify, trust::skip)]
pub fn encode_sgr(cb: u8, col: u16, row: u16, release: bool) -> Vec<u8> {
    // Trust gate: manual byte assembly instead of `write!` — runtime-argument
    // `format_args!` cannot be lowered natively. Byte-identical output
    // (decimal `{}` of the integers, then the ASCII final byte).
    let mut buf = Vec::with_capacity(19);
    buf.extend_from_slice(b"\x1b[<");
    push_dec_u16(&mut buf, u16::from(cb));
    buf.push(b';');
    push_dec_u16(&mut buf, col.saturating_add(1));
    buf.push(b';');
    push_dec_u16(&mut buf, row.saturating_add(1));
    buf.push(if release { b'm' } else { b'M' });
    buf
}

/// Encode mouse coordinates in URXVT format.
///
/// Format: ESC [ Cb ; Cx ; Cy M
/// Like SGR but without the '<' prefix; Cb is already offset by 32.
#[must_use]
// Skip: Vec push/extend — absent std bodies (alloc class).
#[cfg_attr(trust_verify, trust::skip)]
pub fn encode_urxvt(cb: u16, col: u16, row: u16) -> Vec<u8> {
    // Trust gate: see `encode_sgr`.
    // 20 = ESC [ (2) + cb (<=5) + ';' + col (<=5) + ';' + row (<=5) + 'M'. The
    // callers inside this crate cap `cb` at 287 (3 digits), but the function is
    // `pub`, so size for the u16 worst case rather than reallocating on it.
    let mut buf = Vec::with_capacity(20);
    buf.extend_from_slice(b"\x1b[");
    push_dec_u16(&mut buf, cb);
    buf.push(b';');
    push_dec_u16(&mut buf, col.saturating_add(1));
    buf.push(b';');
    push_dec_u16(&mut buf, row.saturating_add(1));
    buf.push(b'M');
    buf
}

/// Dispatch to the appropriate encoding format.
///
/// This is a convenience function that selects the encoding based on
/// `MouseEncoding` and delegates to the format-specific encoder.
///
/// For URXVT, `cb` is automatically offset by 32 (callers pass the raw code).
///
/// # Out-of-range X10 coordinates keep X10 framing
///
/// A coordinate past 222 (1-based 223, byte 255) has no single-byte
/// representation, so [`encode_x10`] clamps it to that maximum. It does NOT
/// change the output FORM.
///
/// This encoder used to emit the SGR (1006) form instead whenever either
/// coordinate went out of range — which silently handed an application a
/// protocol it never enabled. An app that set only mode 1000 scans for the
/// fixed 6-byte `ESC [ M Cb Cx Cy` frame; `ESC [ < 0 ; 251 ; 6 M` does not
/// match it, so the parameter bytes are most likely passed through as literal
/// input. On a wide terminal (>223 columns) that turned every far-right click
/// into typed garbage. The frame an application asked for is the frame it
/// gets; an app that wants coordinates past 223 enables SGR (1006) or UTF-8
/// (1005) itself, and both are honoured here without a limit.
#[must_use]
pub fn encode_mouse(cb: u8, col: u16, row: u16, encoding: MouseEncoding, release: bool) -> Vec<u8> {
    // Release signalling is FORMAT-specific, so it is decided here where the
    // actual output form is known: legacy single-byte forms (X10/UTF-8/urxvt)
    // replace the low button bits with 3 (identity lost, per xterm); SGR keeps
    // the button identity and uses the 'm' final byte instead. Callers pass
    // the ORIGINAL button — substituting earlier would leak the legacy
    // button-3 into the SGR fallback (#7473).
    let legacy_cb = if release { (cb & !0b11) | 3 } else { cb };
    match encoding {
        // X10 encoding uses single bytes for coordinates (max 223 + 32 = 255).
        // Out-of-range coordinates are CLAMPED by `encode_x10`; the framing
        // never changes under the application's feet (see this fn's docs).
        MouseEncoding::X10 => encode_x10(legacy_cb, col, row),
        MouseEncoding::Utf8 => encode_utf8(legacy_cb, col, row),
        MouseEncoding::Sgr | MouseEncoding::SgrPixel => encode_sgr(cb, col, row, release),
        MouseEncoding::Urxvt => encode_urxvt(u16::from(legacy_cb.saturating_add(32)), col, row),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x10_basic() {
        // Button 0 (left), col 10, row 5
        let result = encode_x10(0, 10, 5);
        assert_eq!(result, vec![0x1b, b'[', b'M', 32, 43, 38]);
    }

    #[test]
    fn x10_clamps_large_coordinates() {
        // Coordinates > 223 are clamped
        let result = encode_x10(0, 300, 300);
        // (223+1).min(223) = 223; 223 as u8 + 32 = 255
        assert_eq!(result[4], 255);
        assert_eq!(result[5], 255);
    }

    #[test]
    fn utf8_small_coordinates() {
        // Small coords (< 96) produce single bytes; button code is also UTF-8 encoded
        let result = encode_utf8(0, 10, 5);
        assert_eq!(result[0..3], [0x1b, b'[', b'M']);
        // cb 0: 0+32 = 32 (single byte)
        assert_eq!(result[3], 32);
        // col 10: 10+1+32 = 43 (single byte)
        assert_eq!(result[4], 43);
        // row 5: 5+1+32 = 38 (single byte)
        assert_eq!(result[5], 38);
    }

    #[test]
    fn utf8_large_coordinates() {
        // Coords >= 96 produce 2-byte UTF-8
        let result = encode_utf8(0, 200, 5);
        // cb 0: 0+32 = 32 (single byte) at result[3]
        assert_eq!(result[3], 32);
        // col 200: 200+1+32 = 233 → 2-byte UTF-8: 0xC3 0xA9
        assert_eq!(result[4], 0xC3);
        assert_eq!(result[5], 0xA9);
    }

    #[test]
    fn utf8_large_button_code() {
        // Button code > 95 produces 2-byte UTF-8 (#7498)
        // Ctrl+Button8: cb = 128+16 = 144, 144+32 = 176 → 2-byte UTF-8
        let result = encode_utf8(144, 10, 5);
        assert_eq!(result[0..3], [0x1b, b'[', b'M']);
        // cb 144: 144+32 = 176 → 0xC2 0xB0
        assert_eq!(result[3], 0xC2);
        assert_eq!(result[4], 0xB0);
    }

    #[test]
    fn sgr_press() {
        let result = encode_sgr(0, 10, 5, false);
        assert_eq!(result, b"\x1b[<0;11;6M");
    }

    #[test]
    fn sgr_release() {
        let result = encode_sgr(0, 10, 5, true);
        assert_eq!(result, b"\x1b[<0;11;6m");
    }

    #[test]
    fn urxvt_basic() {
        // cb is already offset: button 0 + 32 = 32
        let result = encode_urxvt(32, 10, 5);
        assert_eq!(result, b"\x1b[32;11;6M");
    }

    #[test]
    fn encode_mouse_dispatches_x10() {
        let result = encode_mouse(0, 10, 5, MouseEncoding::X10, false);
        assert_eq!(result, encode_x10(0, 10, 5));
    }

    /// IN RANGE: the last column the X10 byte form can name. 0-based 222 is
    /// 1-based 223 is byte 255 — the boundary the clamp is measured against.
    #[test]
    fn encode_mouse_x10_in_range_is_the_six_byte_frame() {
        let result = encode_mouse(0, 222, 5, MouseEncoding::X10, false);
        assert_eq!(result, vec![0x1b, b'[', b'M', 32, 255, 38]);
    }

    /// PAST 222: still the six-byte X10 frame, coordinate clamped — NEVER the
    /// SGR form. The audit case: 24x260 with only DECSET 1000 set, a click at
    /// 0-based column 250 used to emit `ESC [ < 0 ; 251 ; 6 M`, a 1006 report
    /// the application never enabled and cannot parse.
    #[test]
    fn encode_mouse_x10_past_222_clamps_and_keeps_x10_framing() {
        let result = encode_mouse(0, 250, 5, MouseEncoding::X10, false);
        assert_eq!(
            result,
            vec![0x1b, b'[', b'M', 32, 255, 38],
            "out-of-range X10 must clamp, not switch to SGR"
        );
        assert_ne!(
            result,
            b"\x1b[<0;251;6M".to_vec(),
            "the pre-fix SGR fallback is exactly what must not come back"
        );
        // Both axes out of range, and a RELEASE: still six bytes, and the
        // legacy button-3 substitution the X10 form requires.
        let release = encode_mouse(0, 400, 300, MouseEncoding::X10, true);
        assert_eq!(release, vec![0x1b, b'[', b'M', 32 + 3, 255, 255]);
    }

    #[test]
    fn encode_mouse_dispatches_sgr() {
        let result = encode_mouse(0, 10, 5, MouseEncoding::Sgr, false);
        assert_eq!(result, encode_sgr(0, 10, 5, false));
    }

    #[test]
    fn encode_mouse_dispatches_sgr_release() {
        let result = encode_mouse(0, 10, 5, MouseEncoding::Sgr, true);
        assert_eq!(result, encode_sgr(0, 10, 5, true));
    }

    #[test]
    fn encode_mouse_dispatches_urxvt() {
        let result = encode_mouse(64, 10, 5, MouseEncoding::Urxvt, false);
        assert_eq!(
            result,
            encode_urxvt(u16::from(64u8.saturating_add(32)), 10, 5)
        );
    }

    #[test]
    fn encode_mouse_dispatches_sgr_pixel() {
        // SgrPixel uses same format as Sgr
        let result = encode_mouse(0, 10, 5, MouseEncoding::SgrPixel, false);
        assert_eq!(result, encode_sgr(0, 10, 5, false));
    }

    /// Regression test: u16::MAX (65535) coordinates must not overflow or panic.
    ///
    /// Bug #2775 fix: mouse encoders with u16::MAX coordinates previously could
    /// overflow in the +1 offset calculation. `saturating_add(1)` prevents this.
    #[test]
    fn sgr_u16_max_coordinates_no_overflow() {
        let result = encode_sgr(0, u16::MAX, u16::MAX, false);
        // u16::MAX.saturating_add(1) remains 65535.
        let expected = b"\x1b[<0;65535;65535M";
        assert_eq!(result, expected);
    }

    /// Regression test: URXVT encoding with u16::MAX coordinates.
    #[test]
    fn urxvt_u16_max_coordinates_no_overflow() {
        let result = encode_urxvt(32, u16::MAX, u16::MAX);
        let expected = b"\x1b[32;65535;65535M";
        assert_eq!(result, expected);
    }

    /// Regression test: X10 encoding with u16::MAX coordinates clamps correctly.
    #[test]
    fn x10_u16_max_coordinates_clamps() {
        let result = encode_x10(0, u16::MAX, u16::MAX);
        // X10 clamps at 223, so col/row both become 223 + 32 = 255
        assert_eq!(result[4], 255);
        assert_eq!(result[5], 255);
    }
}
