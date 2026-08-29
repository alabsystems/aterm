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

/// THE PREFIX-TRANSIT GATE (2026-08-29, the `dmg` report): a curse cue fires
/// the moment the live caret COMPLETES an unambiguous profanity surface, so a
/// surface that is a strict PREFIX of an ordinary English word bonks mid-word
/// — the typist of `dmg` transits `dm`, of `pistol` transits `pis`, of
/// `fickle` transits `fick`. The dictionary walk above only catches whole-word
/// collisions; this one simulates the keystroke path: no proper prefix of any
/// ordinary English word may scan as an unambiguous whole-token Profanity
/// match. 201 dictionary words bonked mid-typing when this gate landed
/// (pis 101, cono 28, sitt 9, picka 8, cok 8, bok 7, ...).
///
/// The allowlist is for surfaces whose transit words are too obscure to
/// outweigh a language's pinned default coverage — each needs a reason.
const INTENTIONAL_PREFIXES: &[&str] = &[
    // `fok` is Afrikaans' primary f-bomb and the ONLY af surface the
    // major-language coverage pin (`profanity_reaches_major_languages`) can
    // match by default; its lone dictionary transit is "fokker".
    "fok",
    // `fokk` is Icelandic's primary f-bomb loan; the same lone dictionary
    // transit ("fokker") as `fok`, and the same call.
    "fokk",
];

#[test]
fn no_english_word_transits_an_unambiguous_profanity_prefix() {
    let Ok(raw) = std::fs::read_to_string("/usr/share/dict/words") else {
        eprintln!("no system word list; prefix-transit gate skipped");
        return;
    };
    let lex = aterm_lexicon::Lexicon::builtin();
    let opts = aterm_lexicon::ScanOptions::default();
    // The live-bonk surface set: every folded spaced surface that, scanned as
    // its own whole token, yields an unambiguous Profanity match. This is
    // EXACTLY the set whose live-caret completion cues the bonk — ambiguous
    // surfaces defer at the caret, and marks-required surfaces never match
    // their bare skeleton (both checked via the public scan, not internals).
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

/// THE `dmg` REPORT (owner, 2026-08-29): "typing 'dmg' triggered the swear
/// noise. it's not a swear." Vietnamese `đm` folds `đ` → `d` into the bare key
/// `dm`, which the live caret completed one keystroke into `dmg`. The fix is
/// the RULE — a surface that loses its marks into bare ASCII only matches
/// mark-bearing tokens — so the whole `dm*` namespace stays silent while the
/// marked form keeps its cue.
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
    // Non-vacuous, and the data keeps its coverage: the tone-marked
    // Vietnamese form (precomposed, uppercase, and decomposed-mark spellings)
    // still classifies as an immediate profanity match.
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
