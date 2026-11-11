// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

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
