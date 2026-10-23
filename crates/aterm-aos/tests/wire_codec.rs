// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Wire codec round-trip and non-canonical rejection tests.

use core::time::Duration;

use aterm_aos::wire::{Dec, Enc, WireFault};
use aterm_aos::{ArtifactId, ArtifactKind, Digest, TaskIdentity};

use crate::toy::{Variant, identity};

#[test]
fn every_field_round_trips_through_enc_and_dec() {
    let id = identity(Variant::Good);
    let art = ArtifactId::new(ArtifactKind::Code, "demo", 7, b"descriptor");
    let nan = f32::from_bits(0x7fc0_1234);
    let mut e = Enc::new();
    e.u8(0xab);
    e.u16(0xbeef);
    e.u32(0xdead_beef);
    e.u64(u64::MAX - 1);
    e.i32(-7);
    e.i64(i64::MIN);
    e.bool(true);
    e.bool(false);
    e.f32_bits(-0.0);
    e.f32_bits(nan);
    e.duration(Duration::new(3, 999_999_999));
    e.offset(-123_456_789_012_345_678_901_234_567);
    e.bytes(b"payload");
    e.bytes(b"");
    e.str("näme");
    e.opt(None::<u32>, |e, v| e.u32(v));
    e.opt(Some(9u32), |e, v| e.u32(v));
    e.digest(&Digest([7; 32]));
    e.artifact(&art);
    e.identity(&id);
    let bytes = e.finish();

    let mut d = Dec::new(&bytes);
    assert_eq!(d.u8("a"), Ok(0xab));
    assert_eq!(d.u16("b"), Ok(0xbeef));
    assert_eq!(d.u32("c"), Ok(0xdead_beef));
    assert_eq!(d.u64("d"), Ok(u64::MAX - 1));
    assert_eq!(d.i32("e"), Ok(-7));
    assert_eq!(d.i64("f"), Ok(i64::MIN));
    assert_eq!(d.bool("g"), Ok(true));
    assert_eq!(d.bool("h"), Ok(false));
    assert_eq!(d.f32_bits("i").map(f32::to_bits), Ok((-0.0f32).to_bits()));
    assert_eq!(d.f32_bits("j").map(f32::to_bits), Ok(nan.to_bits()));
    assert_eq!(d.duration("k"), Ok(Duration::new(3, 999_999_999)));
    assert_eq!(d.offset("l"), Ok(-123_456_789_012_345_678_901_234_567));
    assert_eq!(d.bytes("m"), Ok(&b"payload"[..]));
    assert_eq!(d.bytes("n"), Ok(&b""[..]));
    assert_eq!(d.str("o"), Ok("näme"));
    assert_eq!(d.opt("p", |d| d.u32("p")), Ok(None));
    assert_eq!(d.opt("q", |d| d.u32("q")), Ok(Some(9)));
    assert_eq!(d.digest("r"), Ok(Digest([7; 32])));
    assert_eq!(d.artifact("s"), Ok(art));
    assert_eq!(d.identity(), Ok(id));
    assert_eq!(d.finish(), Ok(()));
}

#[test]
fn a_truncated_field_is_refused_naming_it_and_what_it_needed() {
    let mut d = Dec::new(&[1, 2, 3]);
    assert_eq!(
        d.u32("count"),
        Err(WireFault::Truncated {
            field: "count",
            need: 4,
            have: 3
        })
    );
    assert_eq!(
        Dec::new(&[]).u8("tag"),
        Err(WireFault::Truncated {
            field: "tag",
            need: 1,
            have: 0
        })
    );
}

#[test]
fn trailing_bytes_are_refused_with_their_count() {
    let mut d = Dec::new(&[0, 0, 0, 0, 9, 9]);
    assert_eq!(d.u32("x"), Ok(0));
    assert_eq!(d.finish(), Err(WireFault::Trailing { extra: 2 }));
}

#[test]
fn a_bool_or_option_tag_other_than_zero_or_one_is_a_bad_tag() {
    assert_eq!(
        Dec::new(&[2]).bool("flag"),
        Err(WireFault::BadTag {
            field: "flag",
            tag: 2
        })
    );
    assert_eq!(
        Dec::new(&[7, 1, 0, 0, 0]).opt("maybe", |d| d.u32("maybe")),
        Err(WireFault::BadTag {
            field: "maybe",
            tag: 7
        })
    );
}

#[test]
fn an_artifact_kind_byte_no_kind_uses_is_a_bad_tag() {
    let mut e = Enc::new();
    e.artifact(&ArtifactId::new(ArtifactKind::Task, "t", 1, b""));
    let mut bytes = e.finish();
    bytes[0] = 9;
    assert_eq!(
        Dec::new(&bytes).artifact("art"),
        Err(WireFault::BadTag {
            field: "art",
            tag: 9
        })
    );
    for b in 0..=u8::MAX {
        let kind = ArtifactKind::from_byte(b);
        assert_eq!(kind.map(ArtifactKind::byte), kind.map(|_| b));
        assert_eq!(kind.is_some(), (1..=5).contains(&b), "byte {b}");
    }
}

#[test]
fn an_identity_slot_holding_another_kind_is_a_bad_tag_naming_the_slot() {
    let good = identity(Variant::Good);
    let swapped = TaskIdentity {
        code: good.contract.clone(),
        contract: good.code.clone(),
        ..good
    };
    let mut e = Enc::new();
    e.identity(&swapped);
    let bytes = e.finish();
    assert_eq!(
        Dec::new(&bytes).identity(),
        Err(WireFault::BadTag {
            field: "identity.code",
            tag: ArtifactKind::Contract.byte()
        })
    );
}

#[test]
fn a_duration_with_a_full_second_of_nanos_is_non_canonical() {
    let mut e = Enc::new();
    e.u64(1);
    e.u32(1_000_000_000);
    let bytes = e.finish();
    let mut d = Dec::new(&bytes);
    assert_eq!(
        d.duration("interval"),
        Err(WireFault::NonCanonical { field: "interval" })
    );
    assert_eq!(d.position(), 0);
}

#[test]
fn the_long_length_form_is_refused_for_a_length_the_short_form_holds() {
    let mut e = Enc::new();
    e.u32(u32::MAX);
    e.u64(5);
    e.raw(b"hello");
    let bytes = e.finish();
    assert_eq!(
        Dec::new(&bytes).bytes("blob"),
        Err(WireFault::NonCanonical { field: "blob" })
    );
}

#[test]
fn the_long_length_form_is_read_for_a_length_that_needs_it() {
    let mut e = Enc::new();
    e.u32(u32::MAX);
    e.u64(u64::from(u32::MAX));
    e.raw(b"ab");
    let bytes = e.finish();
    assert_eq!(
        Dec::new(&bytes).bytes("blob"),
        Err(WireFault::Truncated {
            field: "blob",
            need: u32::MAX as usize,
            have: 2
        })
    );
}

#[test]
fn a_length_past_the_remaining_bytes_is_refused_before_anything_is_copied() {
    let mut e = Enc::new();
    e.u32(1_000_000_000);
    e.raw(b"xy");
    let bytes = e.finish();
    let mut d = Dec::new(&bytes);
    assert_eq!(
        d.bytes("blob"),
        Err(WireFault::Truncated {
            field: "blob",
            need: 1_000_000_000,
            have: 2
        })
    );
    assert_eq!(d.position(), 0);
}

#[test]
fn a_count_past_its_limit_is_too_long() {
    let mut e = Enc::new();
    e.u64(11);
    let bytes = e.finish();
    assert_eq!(
        Dec::new(&bytes).count("items", 10),
        Err(WireFault::TooLong {
            field: "items",
            len: 11,
            limit: 10
        })
    );
    assert_eq!(Dec::new(&bytes).count("items", 11), Ok(11));
}

#[test]
fn a_string_that_is_not_utf8_is_non_canonical() {
    let mut e = Enc::new();
    e.bytes(&[0xff, 0xfe]);
    let bytes = e.finish();
    let mut d = Dec::new(&bytes);
    assert_eq!(
        d.str("name"),
        Err(WireFault::NonCanonical { field: "name" })
    );
    assert_eq!(d.position(), 0);
}

#[test]
fn every_prefix_of_an_identity_is_refused_without_panicking() {
    let mut e = Enc::new();
    e.identity(&identity(Variant::Good));
    let bytes = e.finish();
    for cut in 0..bytes.len() {
        let mut d = Dec::new(&bytes[..cut]);
        assert!(
            matches!(d.identity(), Err(WireFault::Truncated { .. })),
            "a {cut}-byte prefix"
        );
        assert_eq!(d.position(), 0, "a refused read moves nothing");
    }
}

#[test]
fn every_byte_flip_of_an_identity_is_refused_or_decodes_to_another_identity() {
    let id = identity(Variant::Good);
    let mut e = Enc::new();
    e.identity(&id);
    let bytes = e.finish();
    for at in 0..bytes.len() {
        for mask in [0x01u8, 0x80, 0xff] {
            let mut bad = bytes.clone();
            bad[at] ^= mask;
            let mut d = Dec::new(&bad);
            if let Ok(other) = d.identity() {
                assert_ne!(other, id, "byte {at} mask {mask:#x}");
            }
        }
    }
}

/// Writes a composite test value into `e`.
fn write_value(e: &mut Enc, n: u64) {
    e.u8(u8::try_from(n % 251).unwrap_or(0));
    e.u64(n.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    e.bytes(&vec![0x5a; usize::try_from(n % 97).unwrap_or(0)]);
    e.opt(n.checked_sub(3), |e, v| {
        e.u32(u32::try_from(v % 7).unwrap_or(0))
    });
}

/// A cleared encoder produces the same bytes as a fresh encoder.
#[test]
fn a_cleared_encoder_writes_what_a_fresh_encoder_writes() {
    let mut reused = Enc::new();
    for n in [40u64, 0, 96, 1, 95, 3, 200, 2] {
        reused.clear();
        assert!(reused.is_empty());
        reused.reserve(usize::try_from(n).unwrap_or(0));
        assert!(reused.is_empty(), "reserve wrote nothing");
        write_value(&mut reused, n);
        let mut fresh = Enc::new();
        write_value(&mut fresh, n);
        assert_eq!(reused.as_bytes(), fresh.as_bytes(), "value {n}");
        assert_eq!(reused.clone().finish(), fresh.finish(), "value {n}");
    }
}
