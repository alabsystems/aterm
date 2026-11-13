// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn canonical_text(phrase: &Phrase) -> String {
    String::from_utf8(phrase.canonical().as_slice().to_vec()).expect("ASCII")
}

#[test]
fn the_list_is_bip39_english_byte_for_byte() {
    let mut file = String::new();
    for word in WORDS {
        file.push_str(word);
        file.push('\n');
    }
    assert_eq!(
        aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(file.as_bytes())),
        "2f5eed53a4727b4bf8880d8f3f199efc90e58503646d9ff8eff3a2ed3b24dbda"
    );
    assert_eq!(WORDS.len(), WORD_COUNT);
}

#[test]
fn every_word_is_short_lower_case_and_alone_in_its_first_four_letters() {
    let mut starts = std::collections::BTreeSet::new();
    for word in WORDS {
        assert!((3..=WORD_MAX).contains(&word.len()), "{word}");
        assert!(word.bytes().all(|b| b.is_ascii_lowercase()), "{word}");
        let start = &word[..word.len().min(PREFIX_LETTERS)];
        assert!(starts.insert(start), "two words start {start}");
    }
    // A 3-letter word is also no other word's first 3 letters followed by nothing: "act" and
    // "action" differ in their fourth letter, which "act" lacks.
    assert_eq!(lookup(b"act"), Some(19));
    assert_eq!(lookup(b"acti"), Some(20));
}

#[test]
fn typing_is_case_blind_any_separator_works_and_four_letters_are_enough() {
    let expected = "maple river ocean";
    for typed in [
        "maple river ocean",
        "MAPLE River oCeAn",
        "  maple-river.ocean\n",
        "mapl rive ocea",
        "maple,  river_ocean!",
        "maple\triver\u{a0}ocean",
        "Mapl/RIVE/ocea",
    ] {
        let phrase = Phrase::parse(typed).expect(typed);
        assert_eq!(canonical_text(&phrase), expected, "{typed:?}");
        assert_eq!(phrase.words(), ["maple", "river", "ocean"]);
    }
}

#[test]
fn what_is_not_three_listed_words_is_refused_without_naming_a_word() {
    for typed in [
        "",
        "maple river",
        "maple river ocean maple",
        "map river ocean",
        "maplex river ocean",
        "maples river ocean",
        "maple river océan",
        "zzzz river ocean",
        "abandonedx river ocean",
    ] {
        let error = Phrase::parse(typed).expect_err(typed).to_string();
        for word in ["maple", "river", "ocean", "map", "zzzz"] {
            assert!(!error.contains(word), "{error}");
        }
    }
}

#[test]
fn every_word_round_trips_through_the_canonical_form() {
    for (i, word) in WORDS.iter().enumerate() {
        let j = (i * 7 + 3) % WORD_COUNT;
        let k = (i * 13 + 1000) % WORD_COUNT;
        let phrase = Phrase::parse(&format!("{word} {} {}", WORDS[j], WORDS[k])).expect(word);
        assert_eq!(phrase.index, [i as u16, j as u16, k as u16]);
        assert_eq!(
            canonical_text(&phrase),
            format!("{word} {} {}", WORDS[j], WORDS[k])
        );
        let short: String = word.chars().take(PREFIX_LETTERS).collect();
        assert_eq!(
            Phrase::parse(&format!("{short} {} {}", WORDS[j], WORDS[k]))
                .expect(word)
                .index,
            phrase.index
        );
        // Every start from 4 letters up names the word.
        for n in PREFIX_LETTERS..word.len() {
            assert_eq!(
                lookup(&word.as_bytes()[..n]),
                Some(i as u16),
                "{word} at {n}"
            );
        }
    }
}

#[test]
fn a_generated_phrase_parses_back_from_its_words_and_never_prints_them() {
    for _ in 0..64 {
        let phrase = Phrase::generate().expect("CSPRNG");
        let typed = phrase.words().join(" ");
        assert_eq!(
            Phrase::parse(&typed).expect("generated").index,
            phrase.index
        );
        assert_eq!(canonical_text(&phrase), typed);
        assert_eq!(format!("{phrase:?}"), "Phrase(..)");
    }
}

#[test]
fn a_generated_phrase_draws_each_word_from_its_own_bits() {
    // 33 random bits: in 128 phrases, each of the 33 index bits must be seen both set and
    // clear, and no two words may always match. A fair draw fails either with odds below
    // 2^-120.
    let phrases: Vec<Phrase> = (0..128)
        .map(|_| Phrase::generate().expect("CSPRNG"))
        .collect();
    for k in 0..3 {
        for bit in 0..11 {
            let set = phrases
                .iter()
                .filter(|p| p.index[k] >> bit & 1 == 1)
                .count();
            assert!(
                0 < set && set < phrases.len(),
                "word {k} bit {bit} never varies"
            );
        }
    }
    for (a, b) in [(0, 1), (0, 2), (1, 2)] {
        assert!(
            phrases.iter().any(|p| p.index[a] != p.index[b]),
            "words {a} and {b} always match"
        );
    }
}

#[test]
fn the_masks_answer_as_comparisons_do() {
    for a in 0..40 {
        for b in 0..40 {
            assert_eq!(mask_eq(a, b), if a == b { 0xff } else { 0 }, "{a} == {b}");
            assert_eq!(mask_lt(a, b), if a < b { 0xff } else { 0 }, "{a} < {b}");
        }
    }
    assert_eq!(mask_eq(0, 1 << 31), 0);
    assert_eq!(mask_lt(1 << 40, 3), 0);
}
