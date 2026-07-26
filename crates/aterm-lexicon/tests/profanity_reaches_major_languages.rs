// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! REGRESSION PIN: the f-bomb reaches the major languages ON THE SHIPPED
//! DEFAULT, and does not fire on ordinary Chinese.
//!
//! Owner, 2026-07-24: "make sure that chinese and other major languages also
//! have f-bombs". The lexicon *contained* 51 profanity entries across 49
//! languages, which made the feature look done — but 18 of those entries were
//! flagged `ambiguous = true`, and `Lexicon` drops every ambiguous entry whose
//! language is not explicitly un-gated. With the shipped default
//! (`languages = ["en"]`) that left **97 of 323 profanity surfaces, 30%, never
//! loaded**: Polish `kurwa`, Arabic, Hebrew, Persian, Bengali, Swedish and
//! Tagalog silently had no f-bomb at all.
//!
//! The `ambiguous` gate is doing real work and is NOT disabled here. It stays
//! on surfaces that genuinely collide with a BENIGN word — Swedish `fan` vs
//! English "fan", Hebrew `כוס` (also "cup"), Chinese 操/草/干 (everyday words),
//! Romanian `fut` (prefixes "future"). What the gate does not belong on is a
//! surface whose only collision is with ANOTHER language's profanity, which
//! fires profanity either way. Those were split into un-gated siblings.
//!
//! The second half pins a FALSE POSITIVE that shipped: the CJK scanner is
//! maximal-munch from the run start, so the real expletive 我操 ("I fuck")
//! matched inside the extremely common 我操作… ("I operate…") — "I operate the
//! computer" detonated an f-bomb. Suppressed via `exceptions.toml`, whose
//! mechanism is class-agnostic even though it began as a feline-only list.
//! This matters more now that detonations are ~3x more frequent.

use aterm_lexicon::{Class, Lexicon, ScanOptions};

/// The SHIPPED default: only English ambiguous entries un-gated.
fn shipped() -> Lexicon {
    Lexicon::with_languages(&["en"])
}

fn has_profanity(lx: &Lexicon, text: &str) -> bool {
    lx.scan(text, &ScanOptions::default())
        .into_iter()
        .any(|m| matches!(m.class, Class::Profanity))
}

#[test]
fn major_languages_have_a_working_f_bomb_by_default() {
    let lx = shipped();
    // Each case is the expletive IN RUNNING TEXT, not bare — the scanner has to
    // find it mid-sentence, which is how anyone actually types it.
    let cases: &[(&str, &str)] = &[
        ("en", "what the fuck is this"),
        ("pl", "ale kurwa co to jest"),
        ("cs", "kurva to je blbost"),
        ("sk", "ty kurva"),
        ("uk", "яка курва справа"),
        ("ru", "какая хуйня блядь"),
        ("sv", "vilket jävla skit"),
        ("af", "fok dit alles"),
        ("ar", "هذا نيك كامل"),
        ("he", "איזה חרא"),
        ("fa", "کیر توش"),
        ("bn", "চোদা ফালতু"),
        ("tl", "putangina naman"),
        ("zh", "这个功能他妈的太难用了"),
        ("ja", "くそ、また落ちた"),
        ("ko", "씨발 또 안 되네"),
        ("es", "joder que mierda"),
        ("de", "so eine scheiße"),
        ("fr", "putain de merde"),
        ("pt", "que merda foder"),
    ];
    let dark: Vec<&str> = cases
        .iter()
        .filter(|(_, text)| !has_profanity(&lx, text))
        .map(|(tag, _)| *tag)
        .collect();
    assert!(
        dark.is_empty(),
        "these languages have NO f-bomb on the shipped default: {dark:?}"
    );
}

/// The gate still exists. A surface kept `ambiguous` because it collides with an
/// ordinary word must STAY dark by default — otherwise this fix would have
/// traded silent under-coverage for noisy false positives.
#[test]
fn genuinely_ambiguous_surfaces_stay_gated() {
    let lx = shipped();
    // Swedish `fan` is also the English word, and `fan` alone must not decorate.
    assert!(
        !has_profanity(&lx, "I am a big fan of this"),
        "English \"fan\" must not read as Swedish profanity"
    );
    // Romanian `fut`/`futu` prefix "future".
    assert!(
        !has_profanity(&lx, "in the future we will see"),
        "\"future\" must not read as Romanian profanity"
    );
}

/// THE FALSE POSITIVE. 我操 is a real expletive AND a prefix of ordinary verbs;
/// maximal munch reached the expletive before 操作 ("operate") was considered.
#[test]
fn ordinary_chinese_does_not_detonate() {
    let lx = shipped();
    for text in [
        "我操作电脑",       // I operate the computer
        "我操心这件事",     // I worry about this
        "我操控这个系统",   // I control this system
    ] {
        assert!(
            !has_profanity(&lx, text),
            "ordinary Chinese detonated an f-bomb: {text}"
        );
    }
    // …while the genuine expletive still fires, so the fix is a suppression of
    // the benign compounds and not a removal of the word.
    assert!(
        has_profanity(&lx, "我操这也太慢了"),
        "the real zh expletive must still fire"
    );
}
