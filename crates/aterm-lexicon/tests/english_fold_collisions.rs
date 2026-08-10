// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! THE FOLD-COLLISION GATE.
//!
//! Matching folds diacritics away (`fold.rs`), which is what lets `Scheiße`
//! match `scheisse` — and also what quietly turns a foreign expletive into an
//! ordinary English word. Turkish `piç` folds to `pic`, so typing `pick`
//! detonated an f-bomb at the moment the prefix `pic` was complete. It was not
//! alone: Swedish `skit`, Latvian `sūds` -> `suds`, Slovak `piča` -> `pica`
//! (which is also a typographic unit), Indonesian `tai`, Estonian `perse`/`sita`,
//! Catalan `cony`, Danish `satan` and Portuguese `foder` all shipped
//! `ambiguous = false`, i.e. loaded for every user on earth.
//!
//! This walks the system English dictionary and fails if ANY ordinary English
//! word classifies as a sparkle class without being opted into. The allowlist
//! below is the set of English words that are SUPPOSED to sparkle.

use std::collections::HashSet;

/// English words that legitimately classify — the `en` entries themselves, plus
/// a few obscure dictionary words that are genuinely another language's word
/// for the animal and only ever draw a (harmless) animal sparkle: `gata` is the
/// Spanish/Portuguese she-cat, `chien`/`chiot` the French dog/puppy, `gos` the
/// Catalan dog.
const INTENTIONAL: &[&str] = &[
    "kitten", "kitty", "pussycat", "orca", "gata", "dog", "dogs", "doggy", "doggo", "pooch", "pup",
    "puppy", "woof", "chien", "chiot", "gos",
];

#[test]
fn no_unambiguous_surface_is_an_ordinary_english_word() {
    let Ok(raw) = std::fs::read_to_string("/usr/share/dict/words") else {
        eprintln!("no system word list; fold-collision gate skipped");
        return;
    };
    let allow: HashSet<&str> = INTENTIONAL.iter().copied().collect();
    let lex = aterm_lexicon::Lexicon::builtin();
    let opts = aterm_lexicon::ScanOptions::default();
    let mut offenders: Vec<String> = Vec::new();
    for word in raw.lines() {
        let w = word.trim().to_lowercase();
        // Short ASCII words are where the collisions live and where a false
        // positive is most infuriating (they are also prefixes of longer words,
        // so they fire MID-TYPING — `pic` inside `pick`).
        if w.len() < 2 || w.len() > 8 || !w.is_ascii() || allow.contains(w.as_str()) {
            continue;
        }
        for hit in lex.scan(&w, &opts) {
            if hit.start == 0 && hit.end == w.len() && !hit.ambiguous {
                offenders.push(format!("{w} -> {:?}", hit.class));
            }
        }
    }
    offenders.sort();
    offenders.dedup();
    assert!(
        offenders.is_empty(),
        "ordinary English words classify as a sparkle class without being opted \
         into. Either mark the owning entry `ambiguous = true` (so it loads only \
         for users who list that language) or add it to INTENTIONAL with a \
         reason:\n  {}",
        offenders.join("\n  ")
    );
}

/// The report that started it: typing `pick` must stay silent — both the whole
/// word and the `pic` prefix a live scanner sees one keystroke earlier.
#[test]
fn typing_pick_is_silent_all_the_way_through() {
    let lex = aterm_lexicon::Lexicon::builtin();
    let opts = aterm_lexicon::ScanOptions::default();
    for prefix in [
        "p", "pi", "pic", "pick", "picks", "picking", "picture", "picnic",
    ] {
        assert!(
            lex.scan(prefix, &opts).is_empty(),
            "{prefix:?} classifies — typing `pick` fires a cue mid-word"
        );
    }
    // Non-vacuous: the real expletive still fires.
    assert!(
        !lex.scan("fuck", &opts).is_empty(),
        "the gate must not be vacuous"
    );
}
