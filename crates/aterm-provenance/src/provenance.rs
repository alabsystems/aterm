// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//! The [`Provenance<T, O>`] `#[repr(transparent)]` wrapper and its
//! constructor [`Provenance::from_pty`].

use core::marker::PhantomData;

use crate::origin::{Origin, Pty};

/// A value tagged with its static origin `O` (invariant, `#[repr(transparent)]`).
#[repr(transparent)]
pub struct Provenance<T: ?Sized, O: Origin> {
    // Placed before `value` because Rust requires a struct's unsized field (`[u8]`, `str`) to come last.
    pub(crate) _origin: PhantomData<fn() -> O>,
    pub(crate) value: T,
}

impl<T: Clone, O: Origin> Clone for Provenance<T, O> {
    #[inline]
    fn clone(&self) -> Self {
        Self {
            _origin: PhantomData,
            value: self.value.clone(),
        }
    }
}

impl<T: Copy, O: Origin> Copy for Provenance<T, O> {}

impl<T: core::fmt::Debug + ?Sized, O: Origin> core::fmt::Debug for Provenance<T, O> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Provenance")
            .field("origin", &core::any::type_name::<O>())
            .field("value", &&self.value)
            .finish()
    }
}

impl<T: PartialEq + ?Sized, O: Origin> PartialEq for Provenance<T, O> {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}

impl<T: Eq + ?Sized, O: Origin> Eq for Provenance<T, O> {}

impl<T> Provenance<T, Pty> {
    /// Tag `value` with [`Pty`] provenance.
    #[inline]
    #[must_use]
    pub const fn from_pty(value: T) -> Self {
        Self {
            value,
            _origin: PhantomData,
        }
    }
}

impl<T: ?Sized, O: Origin> Provenance<T, O> {
    /// Borrow the inner value without its [`Origin`] tag (does not implement `AsRef` or `Deref`).
    #[allow(clippy::should_implement_trait)]
    #[inline]
    #[must_use]
    pub const fn as_ref(&self) -> &T {
        &self.value
    }
}

/// Read a single PTY-tainted byte from `bytes` at `index` if in bounds.
#[inline]
#[must_use]
pub const fn pty_slice_byte_at(bytes: &[u8], index: usize) -> Option<Provenance<u8, Pty>> {
    if index >= bytes.len() {
        return None;
    }
    Some(Provenance::from_pty(bytes[index]))
}

#[inline]
const fn u16_from_le(b0: u8, b1: u8) -> u16 {
    (b0 as u16).wrapping_add((b1 as u16).wrapping_mul(256))
}

#[inline]
const fn u16_from_be(b0: u8, b1: u8) -> u16 {
    (b1 as u16).wrapping_add((b0 as u16).wrapping_mul(256))
}

#[inline]
const fn u32_from_le(b0: u8, b1: u8, b2: u8, b3: u8) -> u32 {
    let lo = u16_from_le(b0, b1) as u32;
    let hi = u16_from_le(b2, b3) as u32;
    lo.wrapping_add(hi.wrapping_mul(65536))
}

#[inline]
const fn u32_from_be(b0: u8, b1: u8, b2: u8, b3: u8) -> u32 {
    let hi = u16_from_be(b0, b1) as u32;
    let lo = u16_from_be(b2, b3) as u32;
    lo.wrapping_add(hi.wrapping_mul(65536))
}

#[allow(clippy::too_many_arguments)]
#[inline]
const fn u64_from_le(b0: u8, b1: u8, b2: u8, b3: u8, b4: u8, b5: u8, b6: u8, b7: u8) -> u64 {
    let lo = u32_from_le(b0, b1, b2, b3) as u64;
    let hi = u32_from_le(b4, b5, b6, b7) as u64;
    lo.wrapping_add(hi.wrapping_mul(0x1_0000_0000))
}

#[allow(clippy::too_many_arguments)]
#[inline]
const fn u64_from_be(b0: u8, b1: u8, b2: u8, b3: u8, b4: u8, b5: u8, b6: u8, b7: u8) -> u64 {
    let hi = u32_from_be(b0, b1, b2, b3) as u64;
    let lo = u32_from_be(b4, b5, b6, b7) as u64;
    lo.wrapping_add(hi.wrapping_mul(0x1_0000_0000))
}

/// Read a little-endian `u16` from `bytes` at `offset` with [`Pty`] provenance.
#[inline]
#[must_use]
pub const fn pty_slice_u16_le_at(bytes: &[u8], offset: usize) -> Option<Provenance<u16, Pty>> {
    let Some(i1) = offset.checked_add(1) else {
        return None;
    };
    if offset >= bytes.len() || i1 >= bytes.len() {
        return None;
    }
    let v = u16_from_le(bytes[offset], bytes[i1]);
    Some(Provenance::from_pty(v))
}

/// Read a big-endian `u16` from `bytes` at `offset` with [`Pty`] provenance.
#[inline]
#[must_use]
pub const fn pty_slice_u16_be_at(bytes: &[u8], offset: usize) -> Option<Provenance<u16, Pty>> {
    let Some(i1) = offset.checked_add(1) else {
        return None;
    };
    if offset >= bytes.len() || i1 >= bytes.len() {
        return None;
    }
    let v = u16_from_be(bytes[offset], bytes[i1]);
    Some(Provenance::from_pty(v))
}

/// Read a little-endian `u32` from `bytes` at `offset` with [`Pty`] provenance.
#[inline]
#[must_use]
pub const fn pty_slice_u32_le_at(bytes: &[u8], offset: usize) -> Option<Provenance<u32, Pty>> {
    let Some(i1) = offset.checked_add(1) else {
        return None;
    };
    let Some(i2) = offset.checked_add(2) else {
        return None;
    };
    let Some(i3) = offset.checked_add(3) else {
        return None;
    };
    let len = bytes.len();
    if offset >= len || i1 >= len || i2 >= len || i3 >= len {
        return None;
    }
    let v = u32_from_le(bytes[offset], bytes[i1], bytes[i2], bytes[i3]);
    Some(Provenance::from_pty(v))
}

/// Read a big-endian `u32` from `bytes` at `offset` with [`Pty`] provenance.
#[inline]
#[must_use]
pub const fn pty_slice_u32_be_at(bytes: &[u8], offset: usize) -> Option<Provenance<u32, Pty>> {
    let Some(i1) = offset.checked_add(1) else {
        return None;
    };
    let Some(i2) = offset.checked_add(2) else {
        return None;
    };
    let Some(i3) = offset.checked_add(3) else {
        return None;
    };
    let len = bytes.len();
    if offset >= len || i1 >= len || i2 >= len || i3 >= len {
        return None;
    }
    let v = u32_from_be(bytes[offset], bytes[i1], bytes[i2], bytes[i3]);
    Some(Provenance::from_pty(v))
}

/// Read a little-endian `u64` from `bytes` at `offset` with [`Pty`] provenance.
#[inline]
#[must_use]
pub const fn pty_slice_u64_le_at(bytes: &[u8], offset: usize) -> Option<Provenance<u64, Pty>> {
    let Some(i1) = offset.checked_add(1) else {
        return None;
    };
    let Some(i2) = offset.checked_add(2) else {
        return None;
    };
    let Some(i3) = offset.checked_add(3) else {
        return None;
    };
    let Some(i4) = offset.checked_add(4) else {
        return None;
    };
    let Some(i5) = offset.checked_add(5) else {
        return None;
    };
    let Some(i6) = offset.checked_add(6) else {
        return None;
    };
    let Some(i7) = offset.checked_add(7) else {
        return None;
    };
    let len = bytes.len();
    if offset >= len
        || i1 >= len
        || i2 >= len
        || i3 >= len
        || i4 >= len
        || i5 >= len
        || i6 >= len
        || i7 >= len
    {
        return None;
    }
    let v = u64_from_le(
        bytes[offset],
        bytes[i1],
        bytes[i2],
        bytes[i3],
        bytes[i4],
        bytes[i5],
        bytes[i6],
        bytes[i7],
    );
    Some(Provenance::from_pty(v))
}

/// Read a big-endian `u64` from `bytes` at `offset` with [`Pty`] provenance.
#[inline]
#[must_use]
pub const fn pty_slice_u64_be_at(bytes: &[u8], offset: usize) -> Option<Provenance<u64, Pty>> {
    let Some(i1) = offset.checked_add(1) else {
        return None;
    };
    let Some(i2) = offset.checked_add(2) else {
        return None;
    };
    let Some(i3) = offset.checked_add(3) else {
        return None;
    };
    let Some(i4) = offset.checked_add(4) else {
        return None;
    };
    let Some(i5) = offset.checked_add(5) else {
        return None;
    };
    let Some(i6) = offset.checked_add(6) else {
        return None;
    };
    let Some(i7) = offset.checked_add(7) else {
        return None;
    };
    let len = bytes.len();
    if offset >= len
        || i1 >= len
        || i2 >= len
        || i3 >= len
        || i4 >= len
        || i5 >= len
        || i6 >= len
        || i7 >= len
    {
        return None;
    }
    let v = u64_from_be(
        bytes[offset],
        bytes[i1],
        bytes[i2],
        bytes[i3],
        bytes[i4],
        bytes[i5],
        bytes[i6],
        bytes[i7],
    );
    Some(Provenance::from_pty(v))
}

/// True when `bytes` begins with a 7-bit CSI prefix (`ESC [`).
#[inline]
#[must_use]
pub const fn pty_slice_has_csi_prefix(bytes: &[u8]) -> bool {
    bytes.len() >= 2 && bytes[0] == 0x1b && bytes[1] == b'['
}

/// True when `bytes` begins with a 7-bit OSC prefix (`ESC ]`).
#[inline]
#[must_use]
pub const fn pty_slice_has_osc_prefix(bytes: &[u8]) -> bool {
    bytes.len() >= 2 && bytes[0] == 0x1b && bytes[1] == b']'
}

/// True when `bytes` begins with a 7-bit DCS prefix (`ESC P`).
#[inline]
#[must_use]
pub const fn pty_slice_has_dcs_prefix(bytes: &[u8]) -> bool {
    bytes.len() >= 2 && bytes[0] == 0x1b && bytes[1] == b'P'
}

impl Provenance<u8, Pty> {
    /// True if the tainted byte is an ASCII C0 control byte (`0x00..=0x1f`) or `DEL` (`0x7f`).
    #[inline]
    #[must_use]
    pub const fn is_c0_or_del(self) -> bool {
        self.value <= 0x1f || self.value == 0x7f
    }

    /// True if the tainted byte is a printable 7-bit ASCII byte (`0x20..=0x7e`).
    #[inline]
    #[must_use]
    pub const fn is_printable_ascii(self) -> bool {
        self.value >= 0x20 && self.value <= 0x7e
    }

    /// True if the tainted byte is a valid CSI parameter byte (`0x30..=0x3f`).
    #[inline]
    #[must_use]
    pub const fn is_csi_param_byte(self) -> bool {
        self.value >= 0x30 && self.value <= 0x3f
    }

    /// True if the tainted byte is a valid CSI final byte (`0x40..=0x7e`).
    #[inline]
    #[must_use]
    pub const fn is_csi_final_byte(self) -> bool {
        self.value >= 0x40 && self.value <= 0x7e
    }

    /// Decode an ASCII decimal digit (`'0'..='9'`) into `0..=9` with [`Pty`] provenance.
    #[inline]
    #[must_use]
    pub const fn to_ascii_digit(self) -> Option<Provenance<u8, Pty>> {
        if self.value < b'0' || self.value > b'9' {
            return None;
        }
        Some(Provenance::from_pty(self.value - b'0'))
    }
}

impl Provenance<u16, Pty> {
    /// Clamp a PTY-origin `u16` parameter into `[min_val, max_val]`.
    #[inline]
    #[must_use]
    pub const fn clamp_param(self, min_val: u16, max_val: u16) -> Self {
        let bounds = [min_val, max_val];
        let lo = bounds[0];
        let raw_hi = bounds[1];
        let hi = if lo > raw_hi { lo } else { raw_hi };
        let v = if self.value < lo {
            lo
        } else if self.value > hi {
            hi
        } else {
            self.value
        };
        Provenance::from_pty(v)
    }

    /// Accumulate one ASCII decimal digit (`0..=9`) onto a CSI parameter, saturating at `max_val`.
    #[inline]
    #[must_use]
    pub const fn push_csi_digit_capped(self, digit: Provenance<u8, Pty>, max_val: u16) -> Self {
        let d = (digit.value % 10) as u16;
        let next = self.value.saturating_mul(10).saturating_add(d);
        let bounded = if next > max_val { max_val } else { next };
        Provenance::from_pty(bounded)
    }

    /// Encode this PTY-origin `u16` into 2 little-endian bytes.
    #[inline]
    #[must_use]
    pub const fn to_le_bytes_pty(self) -> Provenance<[u8; 2], Pty> {
        let b = self.value.to_le_bytes();
        Provenance::from_pty([b[0], b[1]])
    }

    /// Decode a PTY-origin `u16` from 2 little-endian bytes.
    #[inline]
    #[must_use]
    pub const fn from_le_bytes_pty(b: Provenance<[u8; 2], Pty>) -> Self {
        let raw = b.value;
        Provenance::from_pty(u16_from_le(raw[0], raw[1]))
    }
}

impl Provenance<(u16, u16, u16, u16), Pty> {
    /// Encode a PTY-origin CSI rectangle `(top, left, bottom, right)` into 8 little-endian bytes.
    #[inline]
    #[must_use]
    pub const fn encode_csi_rect_le(self) -> Provenance<[u8; 8], Pty> {
        let (top, left, bottom, right) = self.value;
        let t = top.to_le_bytes();
        let l = left.to_le_bytes();
        let b = bottom.to_le_bytes();
        let r = right.to_le_bytes();
        Provenance::from_pty([t[0], t[1], l[0], l[1], b[0], b[1], r[0], r[1]])
    }

    /// Decode a PTY-origin CSI rectangle `(top, left, bottom, right)` from 8 little-endian bytes.
    #[inline]
    #[must_use]
    pub const fn decode_csi_rect_le(wire: Provenance<[u8; 8], Pty>) -> Self {
        let w = wire.value;
        let top = u16_from_le(w[0], w[1]);
        let left = u16_from_le(w[2], w[3]);
        let bottom = u16_from_le(w[4], w[5]);
        let right = u16_from_le(w[6], w[7]);
        Provenance::from_pty((top, left, bottom, right))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pty_wrap_ref;

    #[test]
    fn pty_slice_inspectors_preserve_provenance_and_bounds() {
        let raw = [0x1b, b'[', 0x34, 0x12, 0x78, 0x56, 0xef, 0xcd, 0xab, 0x89];
        let tainted: &Provenance<[u8], Pty> = pty_wrap_ref(&raw[..]);
        let bytes = tainted.as_ref();
        assert!(pty_slice_has_csi_prefix(bytes));
        assert!(!pty_slice_has_osc_prefix(bytes));
        assert!(!pty_slice_has_dcs_prefix(bytes));

        let b0 = pty_slice_byte_at(bytes, 0).unwrap();
        assert!(b0.is_c0_or_del());
        assert!(!b0.is_printable_ascii());
        let b1 = pty_slice_byte_at(bytes, 1).unwrap();
        assert!(b1.is_printable_ascii());
        assert!(b1.is_csi_final_byte());
        assert!(pty_slice_byte_at(bytes, 10).is_none());

        assert_eq!(pty_slice_u16_le_at(bytes, 2).unwrap().as_ref(), &0x1234);
        assert_eq!(pty_slice_u16_be_at(bytes, 2).unwrap().as_ref(), &0x3412);
        assert_eq!(
            pty_slice_u32_le_at(bytes, 2).unwrap().as_ref(),
            &0x5678_1234
        );
        assert_eq!(
            pty_slice_u32_be_at(bytes, 2).unwrap().as_ref(),
            &0x3412_7856
        );
        assert_eq!(
            pty_slice_u64_le_at(bytes, 2).unwrap().as_ref(),
            &0x89ab_cdef_5678_1234
        );
        assert_eq!(
            pty_slice_u64_be_at(bytes, 2).unwrap().as_ref(),
            &0x3412_7856_efcd_ab89
        );
        assert!(pty_slice_u64_le_at(bytes, 3).is_none());

        let d5 = Provenance::from_pty(b'5').to_ascii_digit().unwrap();
        let param = Provenance::from_pty(12u16)
            .push_csi_digit_capped(d5, 9999)
            .clamp_param(1, 500);
        assert_eq!(param.as_ref(), &125);
        let param_wire = param.to_le_bytes_pty();
        assert_eq!(Provenance::from_le_bytes_pty(param_wire), param);

        let rect = Provenance::from_pty((1u16, 2u16, 24u16, 80u16));
        let rect_wire = rect.encode_csi_rect_le();
        assert_eq!(Provenance::decode_csi_rect_le(rect_wire), rect);
    }
}
