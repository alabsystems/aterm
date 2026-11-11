// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_long_request_takes_the_big_form() {
    let words = usize::from(u16::MAX) + 1;
    let mut req = vec![0u8; words * 4];
    req[0] = 72;
    req[1] = 2;
    let big = big_request(&req).expect("a whole number of words");
    assert_eq!(big.len(), req.len() + 4);
    assert_eq!(&big[..4], &[72, 2, 0, 0]);
    assert_eq!(
        u32::from_ne_bytes([big[4], big[5], big[6], big[7]]),
        u32::try_from(words + 1).unwrap()
    );
    let small = vec![1u8; 8];
    assert_eq!(big_request(&small).as_deref(), Some(&small[..]));
    assert_eq!(big_request(&[1, 2, 3]), None);
}

#[test]
fn a_reply_shorter_than_its_length_field_is_refused() {
    let mut reply = [0u8; 36];
    reply[0] = 1;
    reply[4..8].copy_from_slice(&2u32.to_ne_bytes());
    assert_eq!(reply_head(&reply).err(), Some(ParseError::Short));
    reply[4..8].copy_from_slice(&1u32.to_ne_bytes());
    assert!(reply_head(&reply).is_ok());
    reply[0] = 0;
    assert_eq!(reply_head(&reply).err(), Some(ParseError::Invalid));
    assert_eq!(reply_head(&reply[..7]).err(), Some(ParseError::Short));
}

#[test]
fn a_value_list_orders_its_bits() {
    let list = ValueList::new()
        .set(0, 99)
        .set(0b11, 88)
        .set(8, 3)
        .set(1, 9)
        .set(8, 4)
        .set(1 << 20, 77);
    assert_eq!(list.mask(), 9 | (1 << 20));
    let mut w = Writer::new(0, 0);
    list.write_masked(0xffff, &mut w);
    let req: Request<Void> = w.finish();
    let want: Vec<u8> = [9u32, 4].iter().flat_map(|v| v.to_ne_bytes()).collect();
    assert_eq!(&req.bytes()[4..], &want[..]);
}
