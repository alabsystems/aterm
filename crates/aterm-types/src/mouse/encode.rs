// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Pure mouse encoding functions for terminal mouse reporting.
//!
//! These functions produce the byte sequences for X10, UTF-8, SGR, and URXVT
//! mouse encoding formats. All coordinates are 0-indexed; the encoding adds
//! the +1 offset required by each protocol.

use super::types::MouseEncoding;

/// Encode mouse coordinates in X10 format: `ESC [ M Cb Cx Cy`, 1-based and offset by 32.
/// Coordinates past 223 (223 + 32 = 255) are clamped; SGR (mode 1006) has no such limit.
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

/// Encode mouse coordinates in UTF-8 format (mode 1005): `ESC [ M Cb Cx Cy` with every value
/// after M UTF-8 encoded, the button code included (xterm), up to 2015. With modifier bits on
/// a wheel (64+) or extra button (128+) code, cb+32 exceeds 127 and takes two bytes (#7498).
#[must_use]
pub fn encode_utf8(cb: u8, col: u16, row: u16) -> Vec<u8> {
    let mut result = vec![0x1b, b'[', b'M'];
    // Button code is also UTF-8 encoded (not a raw byte)
    encode_utf8_coord(u16::from(cb), &mut result);
    encode_utf8_coord(col.saturating_add(1), &mut result);
    encode_utf8_coord(row.saturating_add(1), &mut result);
    result
}

/// Append `val` to `buf` as decimal digits, allocation-free: the encoders run once per pointer
/// sample under DEC 1002/1003 tracking. Same idiom as `keyboard::encode::write_u32`.
// Skip: pushes into a `Vec<u8>`, an absent std body (alloc class); the tests pin the bytes.
#[cfg_attr(trust_verify, trust::skip)]
#[allow(clippy::cast_possible_truncation)]
fn push_dec_u16(buf: &mut Vec<u8>, val: u16) {
    if val == 0 {
        buf.push(b'0');
        return;
    }
    // Digits go least-significant-first into an 8-slot scratch (u16::MAX has 5), then out in
    // reverse, as in `write_u32`: a literal divisor, masked indices and remainder, and
    // `wrapping_add` on a 0..=9 digit leave the Trust gate nothing to discharge.
    let mut digits = [0u8; 8];
    let mut n = val;
    let mut count = 0usize;
    while n > 0 && count < 5 {
        digits[count & 7] = b'0'.wrapping_add(((n % 10) & 0xFF) as u8);
        n /= 10;
        // Saturating is exact (`count < 5`, `count > 0`); the verifier cannot carry the loop
        // conditions into the arithmetic.
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
    // Manual byte assembly, not `write!`: Trust cannot lower a runtime-argument `format_args!`.
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
    // Manual bytes, as in `encode_sgr`. 20 covers the u16 worst case (ESC [, three 5-digit
    // numbers, two ';', 'M'); `encode_mouse` passes a cb of at most 255.
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

/// Encode in `encoding`'s format. For URXVT `cb` is offset by 32 here (callers pass the raw
/// code).
///
/// # Out-of-range X10 coordinates keep X10 framing
///
/// A coordinate past 222 (1-based 223, byte 255) has no single-byte form, so [`encode_x10`]
/// clamps it; the output form never changes. An app that enabled only mode 1000 parses the
/// fixed 6-byte `ESC [ M Cb Cx Cy` frame, and an SGR report would reach it as typed garbage.
/// An app that wants coordinates past 223 enables SGR (1006) or UTF-8 (1005).
#[must_use]
pub fn encode_mouse(cb: u8, col: u16, row: u16, encoding: MouseEncoding, release: bool) -> Vec<u8> {
    // Release signalling is format-specific. The legacy forms (X10/UTF-8/urxvt) send button 3
    // with only the modifier bits (Shift 4, Alt 8, Ctrl 16; motion 32 is never set on a
    // release), as xterm's `BtnCode(-1)` does: bits 64 and 128 name the button, and 128|3
    // would read as a button-11 press. SGR keeps the button and ends in 'm', which is why
    // callers pass the original button (#7473).
    let legacy_cb = if release { (cb & 0b0011_1100) | 3 } else { cb };
    match encoding {
        // X10 clamps out-of-range coordinates; the framing never changes (see the docs).
        MouseEncoding::X10 => encode_x10(legacy_cb, col, row),
        MouseEncoding::Utf8 => encode_utf8(legacy_cb, col, row),
        MouseEncoding::Sgr | MouseEncoding::SgrPixel => encode_sgr(cb, col, row, release),
        MouseEncoding::Urxvt => encode_urxvt(u16::from(legacy_cb.saturating_add(32)), col, row),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A thumb button's legacy release is button 3 with the modifier bits
    /// only: kept as 128|3 it read as a button-11 press (xterm's
    /// `BtnCode(-1)`). SGR keeps the button and says `m`.
    #[test]
    fn a_thumb_buttons_legacy_release_is_button_three() {
        let back = 128;
        assert_eq!(
            encode_mouse(back, 10, 5, MouseEncoding::X10, true),
            [0x1b, b'[', b'M', 32 + 3, 43, 38]
        );
        assert_eq!(
            encode_mouse(back | 16, 10, 5, MouseEncoding::X10, true),
            [0x1b, b'[', b'M', 32 + 16 + 3, 43, 38]
        );
        assert_eq!(
            encode_mouse(back, 10, 5, MouseEncoding::Urxvt, true),
            b"\x1b[35;11;6M"
        );
        assert_eq!(
            encode_mouse(back, 10, 5, MouseEncoding::Sgr, true),
            b"\x1b[<128;11;6m"
        );
        assert_eq!(
            encode_mouse(back, 10, 5, MouseEncoding::X10, false),
            [0x1b, b'[', b'M', 32 + 128, 43, 38]
        );
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
    fn encode_mouse_dispatches_sgr_pixel() {
        // SgrPixel uses same format as Sgr
        let result = encode_mouse(0, 10, 5, MouseEncoding::SgrPixel, false);
        assert_eq!(result, encode_sgr(0, 10, 5, false));
    }

    /// u16::MAX coordinates do not overflow the +1 offset (#2775); X10 clamps to its 223
    /// maximum.
    #[test]
    fn u16_max_coordinates_never_overflow() {
        assert_eq!(
            encode_sgr(0, u16::MAX, u16::MAX, false),
            b"\x1b[<0;65535;65535M"
        );
        assert_eq!(
            encode_urxvt(32, u16::MAX, u16::MAX),
            b"\x1b[32;65535;65535M"
        );
        let x10 = encode_x10(0, u16::MAX, u16::MAX);
        assert_eq!((x10[4], x10[5]), (255, 255));
    }
}
