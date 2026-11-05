// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn id(pid: i32) -> Identity {
    Identity {
        pid,
        birth: aterm_uds::process::Birth(1_792_098_840_004_410),
        idversion: Some(3),
    }
}

/// Every done word reads back as itself, and nothing else reads as one.
#[test]
fn a_left_word_reads_back_and_nothing_else_does() {
    for word in [
        LeftWord::None,
        LeftWord::Kept(1),
        LeftWord::Kept(16),
        LeftWord::Left(1),
        LeftWord::Left(u16::MAX),
        LeftWord::Unchecked,
    ] {
        assert_eq!(LeftWord::of_word(&word.word()), Some(word), "{word:?}");
    }
    for bad in [
        "", "-1", "+1", "01", "kept:", "kept:0", "kept:01", "kept:x", "65536", "left", "1 ", "none",
    ] {
        assert_eq!(LeftWord::of_word(bad), None, "{bad:?}");
    }
}

/// The word of a read: none where nothing was asked, `unchecked` for a view that could not say,
/// and of rows the unkept ones before the kept ones.
#[test]
fn a_left_word_names_the_unkept_rows_first() {
    let kept = |row: &Identity| row.pid == 7;
    assert_eq!(LeftWord::of(&LeftRead::NotAsked, kept), None);
    assert_eq!(
        LeftWord::of(&LeftRead::Unchecked, kept),
        Some(LeftWord::Unchecked)
    );
    assert_eq!(
        LeftWord::of(&LeftRead::Rows(Vec::new()), kept),
        Some(LeftWord::None)
    );
    assert_eq!(
        LeftWord::of(&LeftRead::Rows(vec![id(7)]), kept),
        Some(LeftWord::Kept(1))
    );
    assert_eq!(
        LeftWord::of(&LeftRead::Rows(vec![id(7), id(8), id(9)]), kept),
        Some(LeftWord::Left(2))
    );
}

/// A token reads in either kernel's shape, and a word that is no token reads as none.
#[test]
fn a_token_reads_in_either_shape() {
    assert_eq!(
        token_of("2606@1792098840004410:3").map(|i| i.pid),
        Some(2606)
    );
    assert_eq!(
        token_of("2606@1792098840004410").map(|i| i.idversion),
        Some(None)
    );
    for bad in ["2606", "it", "2606@", "@1", "1@5", "2606@01", "2606@5:"] {
        assert_eq!(token_of(bad), None, "{bad:?}");
    }
}
