// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The f-bomb reaches the major languages on the shipped default and does not fire on ordinary
//! Chinese.
//!
//! An `ambiguous = true` entry loads only when its language is un-gated, so the flag on a
//! language's only profanity silences it under the default `languages = ["en"]`. The gate belongs
//! only on surfaces that collide with a benign word (Swedish `fan`, Hebrew `כוס` "cup", Chinese
//! 操/草/干, Romanian `fut` in "future"); a surface that collides only with another language's
//! profanity stays un-gated. `exceptions.toml` keeps maximal munch from finding 我操 inside 我操作.

use aterm_lexicon::{Class, Lexicon, ScanOptions};

/// The shipped default: only English ambiguous entries un-gated.
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
    // Each expletive sits in running text, so the scanner finds it mid-sentence.
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

/// The gate still exists: a surface kept `ambiguous` because it collides with
/// an ordinary word stays dark by default.
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

/// 我操 is an expletive and a prefix of ordinary verbs (我操作 "I operate"); the exceptions keep
/// maximal munch from reaching it there.
#[test]
fn ordinary_chinese_does_not_detonate() {
    let lx = shipped();
    for text in [
        "我操作电脑",     // I operate the computer
        "我操心这件事",   // I worry about this
        "我操控这个系统", // I control this system
    ] {
        assert!(
            !has_profanity(&lx, text),
            "ordinary Chinese detonated an f-bomb: {text}"
        );
    }
    // The genuine expletive still fires: the benign compounds are suppressed, not the word.
    assert!(
        has_profanity(&lx, "我操这也太慢了"),
        "the real zh expletive must still fire"
    );
}
