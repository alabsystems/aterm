// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! What a hostile document can make the runtime do, where upstream's suite
//! leaves it unpinned: a second tag key is an error rather than a silent
//! override, and a length hint cannot size an allocation past the 1 MiB
//! preallocation cap (`private::size_hint::cautious`). aterm reads every
//! document it persists through these paths, from files any process can
//! write.

use crate::support::{Deserializer, Token, assert_de_tokens_error};
use aterm_serde::de::Deserialize;
use serde_derive::Deserialize;
use std::collections::HashMap;

/// The cap `size_hint::cautious` keeps preallocation under.
const MAX_PREALLOC_BYTES: usize = 1024 * 1024;

#[derive(Debug, PartialEq, Deserialize)]
#[serde(tag = "type")]
enum Tagged {
    A { a: u8 },
}

#[test]
fn a_second_tag_key_is_a_duplicate_field() {
    assert_de_tokens_error::<Tagged>(
        &[
            Token::Map { len: None },
            Token::Str("type"),
            Token::Str("A"),
            Token::Str("a"),
            Token::U8(1),
            Token::Str("type"),
        ],
        "duplicate field `type`",
    );
}

fn from_tokens<'de, T: Deserialize<'de>>(tokens: &'de [Token]) -> T {
    let mut de = Deserializer::new(tokens);
    let value = T::deserialize(&mut de).expect("tokens deserialize");
    assert_eq!(de.remaining(), 0, "every token is read");
    value
}

#[test]
fn a_huge_seq_hint_preallocates_at_most_the_cap() {
    let v: Vec<u64> = from_tokens(&[
        Token::Seq {
            len: Some(usize::MAX),
        },
        Token::U64(1),
        Token::U64(2),
        Token::SeqEnd,
    ]);
    assert_eq!(v, [1, 2]);
    assert!(
        v.capacity() * size_of::<u64>() <= MAX_PREALLOC_BYTES,
        "capacity {} came from the hint",
        v.capacity()
    );
}

#[test]
fn a_huge_map_hint_does_not_size_the_table() {
    let m: HashMap<String, u32> = from_tokens(&[
        Token::Map {
            len: Some(usize::MAX),
        },
        Token::Str("k"),
        Token::U32(1),
        Token::MapEnd,
    ]);
    assert_eq!(m.get("k"), Some(&1));
    assert_eq!(m.len(), 1);
}

#[derive(Debug, PartialEq, Deserialize)]
#[serde(untagged)]
enum Buffered {
    Seq(Vec<u8>),
}

#[test]
fn a_huge_hint_into_the_untagged_buffer_preallocates_at_most_the_cap() {
    // `untagged` buffers the input as `Content` before any variant reads it;
    // that buffer is sized by the same cap.
    let v: Buffered = from_tokens(&[
        Token::Seq {
            len: Some(usize::MAX),
        },
        Token::U8(7),
        Token::SeqEnd,
    ]);
    assert_eq!(v, Buffered::Seq(vec![7]));
}
