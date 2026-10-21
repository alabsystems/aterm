// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! The fold-collision gate.
//!
//! Matching folds diacritics away (`fold.rs`), which lets `Scheiße` match
//! `scheisse` — and can turn a foreign expletive into an ordinary English
//! word: Turkish `piç` folds to `pic`, the prefix of `pick`; Latvian `sūds`
//! folds to `suds`.
//!
//! This walks Webster's Second (`crate::web2_words`) and fails if any ordinary English
//! word classifies as a sparkle class without being opted into. The allowlist
//! below is the set of English words that are supposed to sparkle.

use std::collections::HashSet;

/// English words that legitimately classify: the `en` entries, plus obscure dictionary words that
/// are another language's animal word and draw only a harmless sparkle (`gata` she-cat,
/// `chien`/`chiot` dog/puppy, `gos` Catalan dog).
const INTENTIONAL: &[&str] = &[
    "kitten", "kitty", "pussycat", "gata", "dog", "dogs", "doggy", "doggo", "pooch", "pup",
    "puppy", "woof", "chien", "chiot", "gos",
];

#[test]
fn no_unambiguous_surface_is_an_ordinary_english_word() {
    let raw = crate::web2_words();
    let allow: HashSet<&str> = INTENTIONAL.iter().copied().collect();
    let lex = aterm_lexicon::Lexicon::builtin();
    let opts = aterm_lexicon::ScanOptions::default();
    let mut offenders: Vec<String> = Vec::new();
    for word in raw.lines() {
        let w = word.trim().to_lowercase();
        // Short ASCII words are where collisions live and where a false positive hurts most:
        // they prefix longer words, so they fire mid-typing (`pic` in `pick`).
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

/// The prefix-transit gate: a curse cue fires when the live caret completes an unambiguous
/// profanity surface, so a surface that is a proper prefix of an English word bonks mid-word
/// (`dm` in `dmg`, `pis` in `pistol`, `fick` in `fickle`). No proper prefix of a dictionary word
/// may scan as an unambiguous whole-token profanity match.
///
/// The allowlist is for surfaces whose transit words are too obscure to outweigh a language's
/// default coverage; each needs a reason.
const INTENTIONAL_PREFIXES: &[&str] = &[
    // Afrikaans' primary f-bomb and its only surface the major-language coverage pin
    // (`profanity_reaches_major_languages`) matches by default; its lone transit is "fokker".
    "fok", // Icelandic's primary f-bomb loan, with the same lone transit.
    "fokk",
];

#[test]
fn no_english_word_transits_an_unambiguous_profanity_prefix() {
    let raw = crate::web2_words();
    let lex = aterm_lexicon::Lexicon::builtin();
    let opts = aterm_lexicon::ScanOptions::default();
    // Every folded spaced surface that, scanned alone, is an unambiguous profanity match: the set
    // whose caret completion cues the bonk (ambiguous surfaces defer, marks-required ones never
    // match bare).
    let live: HashSet<&str> = lex
        .iter_spaced()
        .filter(|(_, class)| *class == aterm_lexicon::Class::Profanity)
        .map(|(surface, _)| surface)
        .filter(|surface| {
            lex.scan(surface, &opts)
                .iter()
                .any(|m| !m.ambiguous && m.class == aterm_lexicon::Class::Profanity)
        })
        .collect();
    assert!(
        live.contains("fuck"),
        "the live-surface enumeration must not be vacuous"
    );
    let allow: HashSet<&str> = INTENTIONAL_PREFIXES.iter().copied().collect();
    let mut offenders: Vec<String> = Vec::new();
    for word in raw.lines() {
        let w = word.trim().to_lowercase();
        if !w.is_ascii() || w.len() < 2 {
            continue;
        }
        // Every PROPER prefix the typist's live caret completes on the way.
        for end in 1..w.len() {
            let p = &w[..end];
            if live.contains(p) && !allow.contains(p) {
                offenders.push(format!("{p} (mid-typing {w})"));
            }
        }
    }
    offenders.sort();
    offenders.dedup();
    assert!(
        offenders.is_empty(),
        "typing these ordinary English words fires the curse bonk at the \
         moment the profanity prefix completes. Mark the owning surface's \
         entry `ambiguous = true` (the house remedy — see the Romanian \
         `fut`/`future` precedent) or add it to INTENTIONAL_PREFIXES with a \
         reason:\n  {}",
        offenders.join("\n  ")
    );
}

/// Vietnamese `đm` folds to bare `dm`, which the caret completes on the way to `dmg`: the `dm*`
/// namespace stays silent while the marked form keeps its cue.
#[test]
fn typing_dmg_is_silent_and_the_marked_form_still_fires() {
    let lex = aterm_lexicon::Lexicon::builtin();
    let opts = aterm_lexicon::ScanOptions::default();
    for prefix in [
        "d", "dm", "dmg", "dmesg", "dmarc", "dmz", "dma", "DM", "dm's",
    ] {
        assert!(
            lex.scan(prefix, &opts).is_empty(),
            "{prefix:?} classifies — typing it fires the swear cue"
        );
    }
    // The marked form (precomposed, uppercase, decomposed) still fires immediately.
    for marked in ["đm", "ĐM", "d\u{0303}m"] {
        let hits = lex.scan(marked, &opts);
        assert_eq!(hits.len(), 1, "marked form {marked:?} lost its match");
        assert_eq!(hits[0].class, aterm_lexicon::Class::Profanity);
        assert!(
            !hits[0].ambiguous,
            "marked form {marked:?} must stay an immediate match"
        );
    }
}

/// Typing `pick` stays silent, including at the `pic` prefix one keystroke earlier.
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
