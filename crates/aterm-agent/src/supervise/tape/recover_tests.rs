// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Reader bound tests on hand-crafted supervisor tapes.

use astream_effects::EffectRecord;

use super::super::codec::{Encoder, Item};
use super::super::doors;
use super::super::frame::{Kind, MAGIC, frame_bytes};
use super::super::frame_tests::{HEADER, mono};
use super::*;

/// A hold small enough that a test reaches it with small bytes.
const SMALL: usize = 1 << 20;

/// The header frame and one records frame of `items`, encoded in order by one encoder.
fn one_frame(items: &[Item]) -> (Vec<u8>, usize) {
    let mut enc = Encoder::default();
    let mut raw = Vec::new();
    for item in items {
        enc.item(item, &mut raw);
    }
    let header = frame_bytes(Kind::Header, 0, HEADER.as_bytes()).expect("a frame");
    let at = MAGIC.len() + header.len();
    let records = frame_bytes(Kind::Records, 1, &raw).expect("a frame");
    ([MAGIC.as_slice(), &header, &records].concat(), at)
}

/// A read of `bytes` holding at most [`SMALL`], refusing a fault or keeping the prefix before it.
fn read_small(bytes: &[u8], keep: Keep) -> Recovered {
    read(bytes, keep, SMALL, steps)
}

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
    let refused = read_small(&bytes, Keep::Refuse);
    assert_eq!(
        refused.tail,
        Tail::Fault {
            at,
            why: Why::TooLarge { limit: SMALL }
        }
    );
    assert_eq!(
        (refused.header, refused.items.len(), refused.frames),
        (None, 0, 0),
        "a refused tape serves nothing"
    );
    let kept = read_small(&bytes, Keep::Prefix);
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
        read_small(&bytes, Keep::Refuse).tail,
        Tail::Fault {
            at,
            why: Why::TooLarge { limit: SMALL }
        }
    );
    assert_eq!(recover(&bytes).items, named);
    let bare: Vec<Item> = (0..(SMALL / std::mem::size_of::<Item>()) as u64 + 1)
        .map(mono)
        .collect();
    let (bytes, at) = one_frame(&bare);
    assert_eq!(
        read_small(&bytes, Keep::Refuse).tail,
        Tail::Fault {
            at,
            why: Why::TooLarge { limit: SMALL }
        }
    );
    assert_eq!(recover(&bytes).items.len(), bare.len());
}
