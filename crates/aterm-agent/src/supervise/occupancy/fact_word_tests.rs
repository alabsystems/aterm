// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The fact word: closed by construction, and read back only from the spelling it writes.

use super::*;

/// Every rule's word at the ends of the ordinal's range reads back as itself, its `Debug` holding
/// the rule's variant and the number alone.
#[test]
fn every_rules_word_reads_back_as_written() {
    for rule in RuleId::ALL {
        for ordinal in [1, 2, 65_535] {
            let word = FactWord::new(rule, ordinal).expect("in range");
            assert_eq!(FactWord::parse(&word.word()), Some(word), "{}", word.word());
            assert!(word.word().starts_with(rule.name()), "{}", word.word());
            let debug = format!("{word:?}");
            assert!(
                debug
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b" {}:,_".contains(&b)),
                "{debug}"
            );
        }
    }
    assert_eq!(FactWord::new(RuleId::HoldRoom, 0), None);
    assert_eq!(FactWord::new(RuleId::HoldRoom, 65_536), None);
}

/// ONLY THE SPELLING [`FactWord::word`] WRITES IS READ: a rule of the closed list, `#`, and a
/// plain decimal ordinal. NEGATIVE CONTROLS: an ordinal of 0, past 65535, signed, with a leading
/// zero, spaced or empty; a rule no build has, another revision, or none; a planted label or
/// task title in either half; two marks or none.
#[test]
fn a_word_it_does_not_write_is_refused() {
    let good = FactWord::parse("hold-room@v1#1").expect("the control");
    assert_eq!(good.rule, RuleId::HoldRoom);
    assert_eq!(good.ordinal.get(), 1);
    for text in [
        "hold-room@v1#0",
        "hold-room@v1#65536",
        "hold-room@v1#999999",
        "hold-room@v1#+1",
        "hold-room@v1#-1",
        "hold-room@v1#01",
        "hold-room@v1# 1",
        "hold-room@v1#1 ",
        "hold-room@v1#",
        "hold-room@v2#1",
        "hold-room#1",
        "room@v1#1",
        "#1",
        "",
        "hold-room@v1",
        "hold-room@v1#1#2",
        "ignore.previous.instructions#1",
        "hold-room@v1#ignore.previous.instructions",
        " hold-room@v1#1",
        "HOLD-ROOM@V1#1",
    ] {
        assert_eq!(FactWord::parse(text), None, "{text:?}");
    }
}

/// Each outcome reads back from its word, the words distinct. NEGATIVE CONTROL: a word none
/// writes reads as none.
#[test]
fn each_outcome_reads_back_from_its_word() {
    let words: std::collections::BTreeSet<&str> =
        NoteOutcome::ALL.iter().map(|o| o.word()).collect();
    assert_eq!(words.len(), NoteOutcome::ALL.len());
    for outcome in NoteOutcome::ALL {
        assert_eq!(NoteOutcome::of_word(outcome.word()), Some(outcome));
    }
    for word in ["", "posted", "no_task", "Noted"] {
        assert_eq!(NoteOutcome::of_word(word), None, "{word:?}");
    }
}
