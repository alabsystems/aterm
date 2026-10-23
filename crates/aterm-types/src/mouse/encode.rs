// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Pure mouse encoding functions for terminal mouse reporting.

use super::types::MouseEncoding;

/// Encode mouse coordinates in X10 format: `ESC [ M Cb Cx Cy`, 1-based and offset by 32.
#[inline]
#[must_use]
pub fn encode_x10(cb: u8, col: u16, row: u16) -> Vec<u8> {
    let cx = (col.saturating_add(1).min(223) as u8).saturating_add(32);
    let cy = (row.saturating_add(1).min(223) as u8).saturating_add(32);
    vec![0x1b, b'[', b'M', cb.saturating_add(32), cx, cy]
}

/// Encode a single coordinate as UTF-8 for mouse encoding mode 1005.
#[inline]
#[allow(clippy::cast_possible_truncation)]
fn encode_utf8_coord(coord: u16, output: &mut Vec<u8>) {
    let c = coord.saturating_add(32);
    if c < 128 {
        output.push(c as u8);
    } else {
        let c = c.min(2047);
        output.push(0xC0 | (((c >> 6) & 0xFF) as u8));
        output.push(0x80 | ((c & 0x3F) as u8));
    }
}

/// Encode mouse coordinates in UTF-8 format (mode 1005).
#[inline]
#[must_use]
pub fn encode_utf8(cb: u8, col: u16, row: u16) -> Vec<u8> {
    let mut result = vec![0x1b, b'[', b'M'];
    encode_utf8_coord(u16::from(cb), &mut result);
    encode_utf8_coord(col.saturating_add(1), &mut result);
    encode_utf8_coord(row.saturating_add(1), &mut result);
    result
}

/// Append `val` to `buf` as decimal digits, allocation-free: the encoders run once per pointer
/// sample under DEC 1002/1003 tracking. Same idiom as `keyboard::encode::write_u32`.
#[inline]
#[cfg_attr(trust_verify, trust::skip)]
#[allow(clippy::cast_possible_truncation)]
fn push_dec_u16(buf: &mut Vec<u8>, val: u16) {
    if val == 0 {
        buf.push(b'0');
        return;
    }
    let mut digits = [0u8; 8];
    let mut n = val;
    let mut count = 0usize;
    while n > 0 && count < 5 {
        digits[count & 7] = b'0'.wrapping_add(((n % 10) & 0xFF) as u8);
        n /= 10;
        count = count.saturating_add(1);
    }
    while count > 0 {
        count = count.saturating_sub(1);
        buf.push(digits[count & 7]);
    }
}

/// Encode mouse coordinates in SGR format.
#[inline]
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn encode_sgr(cb: u8, col: u16, row: u16, release: bool) -> Vec<u8> {
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
#[inline]
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn encode_urxvt(cb: u16, col: u16, row: u16) -> Vec<u8> {
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

/// Encode in `encoding`'s format.
#[inline]
#[must_use]
pub fn encode_mouse(cb: u8, col: u16, row: u16, encoding: MouseEncoding, release: bool) -> Vec<u8> {
    let legacy_cb = if release { (cb & 0b0011_1100) | 3 } else { cb };
    match encoding {
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
        let result = encode_utf8(144, 10, 5);
        assert_eq!(result[0..3], [0x1b, b'[', b'M']);
        assert_eq!(result[3], 0xC2);
        assert_eq!(result[4], 0xB0);
    }

    #[test]
    fn encode_mouse_dispatches_sgr_pixel() {
        let result = encode_mouse(0, 10, 5, MouseEncoding::SgrPixel, false);
        assert_eq!(result, encode_sgr(0, 10, 5, false));
    }

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
