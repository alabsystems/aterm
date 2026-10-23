// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Reader bound and recovery tests on hand-crafted supervisor tapes.

use astream_effects::EffectRecord;

use super::super::codec::{Encoder, Item};
use super::super::doors;
use super::super::frame::{HEAD, Kind, MAGIC, frame_bytes};
use super::super::frame_tests::{HEADER, mono};
use super::*;

/// Bounds small enough that a test reaches them with small bytes.
const SMALL: Limits = Limits {
    held: 1 << 20,
    search_floor: 0,
};

/// The header frame and one records frame of `items`, encoded in order by one encoder.
fn one_frame(items: &[Item]) -> (Vec<u8>, usize) {
    let mut enc = Encoder::default();
    let mut raw = Vec::new();
    for item in items {
        enc.item(item, &mut raw);
    }
    let header = frame_bytes(Kind::Header, 0, HEADER.as_bytes());
    let at = MAGIC.len() + header.len();
    let bytes = [
        MAGIC.as_slice(),
        &header,
        &frame_bytes(Kind::Records, 1, &raw),
    ]
    .concat();
    (bytes, at)
}

/// A read of `bytes`, refusing a fault or keeping the prefix before it.
fn read_with(bytes: &[u8], keep: Keep, limits: Limits) -> Recovered {
    read(bytes, keep, Check::Xxh64, limits)
}

/// A 64 KiB reply stored once and then read again `n - 1` times: each repeat
/// is one byte of body standing for a copy of the whole reply.
#[test]
fn a_body_repeated_past_what_a_read_holds_is_refused() {
    let reply: Vec<u8> = (0..64 * 1024).map(|i| (i % 251) as u8).collect();
    let items = vec![
        Item::effect(EffectRecord::Exchange {
            door: doors::CTL.to_string(),
            request: Vec::new(),
            reply,
        });
        64
    ];
    let (bytes, at) = one_frame(&items);
    assert!(
        bytes.len() < 4_096,
        "{} bytes stand for 4 MiB of replies",
        bytes.len()
    );
    let refused = read_with(&bytes, Keep::Refuse, SMALL);
    assert_eq!(
        refused.tail,
        Tail::Fault {
            at,
            why: Why::TooLarge { limit: SMALL.held }
        }
    );
    assert_eq!(
        (refused.header, refused.items.len(), refused.frames),
        (None, 0, 0),
        "a refused tape serves nothing"
    );
    let kept = read_with(&bytes, Keep::Prefix, SMALL);
    assert_eq!(kept.tail, refused.tail);
    assert_eq!(kept.header.as_deref(), Some(HEADER));
    assert_eq!(
        (kept.items.len(), kept.frames),
        (0, 1),
        "salvage keeps the frames before the one that crossed the bound, and none of it"
    );
    let whole = recover(&bytes);
    assert_eq!(whole.tail, Tail::Clean);
    assert_eq!(whole.items, items);
}

/// Every copy a read makes counts: a name spelled once and named by every
/// item, and items that own nothing at all, which still take their own size.
#[test]
fn a_name_and_items_that_own_nothing_count_too() {
    let named = vec![
        Item::effect(EffectRecord::Raised {
            door: "d".repeat(64 * 1024),
            false_reads: 0,
        });
        16
    ];
    let (bytes, at) = one_frame(&named);
    assert_eq!(
        read_with(&bytes, Keep::Refuse, SMALL).tail,
        Tail::Fault {
            at,
            why: Why::TooLarge { limit: SMALL.held }
        }
    );
    assert_eq!(recover(&bytes).items, named);
    let bare: Vec<Item> = (0..(SMALL.held / std::mem::size_of::<Item>()) as u64 + 1)
        .map(mono)
        .collect();
    let (bytes, at) = one_frame(&bare);
    assert_eq!(
        read_with(&bytes, Keep::Refuse, SMALL).tail,
        Tail::Fault {
            at,
            why: Why::TooLarge { limit: SMALL.held }
        }
    );
    assert_eq!(recover(&bytes).items.len(), bare.len());
}

/// Header plus a damaged records frame followed by frame-like heads every 17 bytes to `len`.
fn crowded(len: usize, last: &[u8]) -> (Vec<u8>, usize) {
    let header = frame_bytes(Kind::Header, 0, HEADER.as_bytes());
    let at = MAGIC.len() + header.len();
    let mut bad = frame_bytes(Kind::Records, 1, &{
        let mut raw = Vec::new();
        Encoder::default().item(&mono(5), &mut raw);
        raw
    });
    *bad.last_mut().expect("a payload") ^= 0x01;
    let mut bytes = [MAGIC.as_slice(), &header, &bad].concat();
    let total = len + last.len();
    while bytes.len() + HEAD <= len {
        let q = bytes.len();
        let raw_len = u32::try_from(total - q - HEAD).expect("a short test file");
        bytes.push(Kind::Records as u8);
        bytes.extend_from_slice(&raw_len.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes());
    }
    bytes.resize(len, 0);
    bytes.extend_from_slice(last);
    assert_eq!(bytes.len(), total);
    (bytes, at)
}

/// Bytes crowded with frame-like heads after a damaged frame cost the search
/// a bounded amount, not the square of the file, and are refused rather than
/// guessed torn: a quarter MiB of them would hash about 2 GiB unbounded, and
/// the search hashes at most `SEARCH_FLOOR` (64 MiB) here.
#[test]
fn bytes_crowded_with_frame_heads_are_refused_unsearched() {
    let (bytes, at) = crowded(256 * 1024, &[]);
    let kept = salvage(&bytes);
    assert_eq!(
        kept.tail,
        Tail::Fault {
            at,
            why: Why::Unsearched
        }
    );
    assert_eq!(
        (kept.header.as_deref(), kept.frames),
        (Some(HEADER), 1),
        "salvage keeps the whole frames before the damage"
    );
}

/// Within the bound the search is the exhaustive one: a whole frame behind a
/// crowd it can afford to check makes the damage a fault, as it always did; a
/// bound too small for the same crowd says only that it could not tell.
#[test]
fn within_its_bound_the_search_still_finds_a_whole_frame() {
    let whole = frame_bytes(Kind::Records, 2, &{
        let mut raw = Vec::new();
        Encoder::default().item(&mono(9), &mut raw);
        raw
    });
    let (bytes, at) = crowded(8 * 1024, &whole);
    assert_eq!(
        recover(&bytes).tail,
        Tail::Fault {
            at,
            why: Why::Damaged
        }
    );
    assert_eq!(
        read_with(&bytes, Keep::Refuse, SMALL).tail,
        Tail::Fault {
            at,
            why: Why::Unsearched
        }
    );
    let (bytes, at) = crowded(8 * 1024, &[]);
    assert_eq!(recover(&bytes).tail, Tail::Torn { at });
}
