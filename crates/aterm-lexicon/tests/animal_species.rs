// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Builtin English animal surfaces scan as [`Class::Animal`] with a species id naming their
//! sprite, and are `ambiguous = true` (live-caret deferral). Existing classes keep their homograph
//! wins (`kitty` stays feline); `orca` is no class's word.

use aterm_lexicon::{Class, Lexicon, ScanOptions};

fn scan_one(lex: &Lexicon, word: &str) -> aterm_lexicon::Match {
    let hits = lex.scan(word, &ScanOptions::default());
    assert_eq!(hits.len(), 1, "{word:?} must scan exactly once: {hits:?}");
    hits[0]
}

#[test]
fn animal_surfaces_scan_with_their_species() {
    let lex = Lexicon::builtin();
    for (word, species) in [
        ("monkey", "monkey"),
        ("monkeys", "monkey"),
        ("camel", "camel"),
        ("bunny", "rabbit"),
        ("bunnies", "rabbit"),
        ("pony", "horse"),
        ("gator", "crocodile"),
        ("hippopotamus", "hippo"),
        ("geese", "goose"),
        ("dino", "dinosaur"),
        ("fox", "fox"), // 3 folded chars: the short-word guards are feline/emphasis-only
        ("owl", "owl"),
        ("bee", "bee"),
        ("turkey", "turkey"),
        ("turkeys", "turkey"),
        ("Turkey", "turkey"), // the country folds into the bird — deliberate, see the entry's notes
        ("bird", "bird"),
        ("birds", "bird"),
        ("birdie", "bird"),
    ] {
        let m = scan_one(lex, word);
        assert_eq!(m.class, Class::Animal, "{word:?}");
        assert!(
            m.ambiguous,
            "{word:?} must be ambiguous (live-caret deferral is the mid-typing guard)"
        );
        let id = m
            .species
            .unwrap_or_else(|| panic!("{word:?} has no species"));
        assert_eq!(lex.species_code(id), species, "{word:?}");
    }
}

#[test]
fn animal_class_never_steals_existing_families() {
    let lex = Lexicon::builtin();
    // The generic whale is the animal sprite; `orca` decorates nothing rather
    // than falling through to it.
    assert!(lex.scan("orca", &ScanOptions::default()).is_empty());
    assert_eq!(scan_one(lex, "whale").class, Class::Animal);
    // kitty stays feline (rank), and bare `cat` stays the short-feline opt-in.
    assert_eq!(scan_one(lex, "kitty").class, Class::Feline);
    assert!(lex.scan("cat", &ScanOptions::default()).is_empty());
}

#[test]
fn non_animal_matches_carry_no_species() {
    let lex = Lexicon::builtin();
    assert_eq!(scan_one(lex, "kitty").species, None);
    assert_eq!(scan_one(lex, "doggo").species, None);
}

/// Animal nouns inside longer tokens never match (`bee` in `been`, `cow` in `coward`).
#[test]
fn animal_words_are_whole_word_only() {
    let lex = Lexicon::builtin();
    let opts = ScanOptions::default();
    for text in ["been", "coward", "monkeywrench", "foxglove2", "sealed"] {
        assert!(
            lex.scan(text, &opts).is_empty(),
            "{text:?} must not match any animal surface"
        );
    }
}
