// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! The menagerie's zh/ja/ko (+ friends) contract (2026-08-10). Owner verbatim:
//! "these need to work for Chinese and Korean and Japanese and other asian
//! languages." Four laws under test:
//!
//! 1. Multi-character CJK animal compounds load PLAIN — they match on the
//!    DEFAULT builtin (`languages = ["en"]`), inside running CJK text, with
//!    default scan options. No opt-in required: that silence was the bug.
//! 2. Single-character forms (象 / 馬 / 새 / 곰 …) ride the class-agnostic
//!    `cjk_single_char` opt-in: silent without it, species-correct with it.
//! 3. Finite prefix bombs are suppressed via exceptions.toml (カメラ never
//!    grows a turtle), while the bare animal keeps matching.
//! 4. Open-ended in-language homographs ride `ambiguous = true` language
//!    gates (ja くま, ko 판다) — the kuma treatment.

use aterm_lexicon::{Class, Lexicon, ScanOptions};

fn single_species(lex: &Lexicon, text: &str, opts: &ScanOptions) -> String {
    let hits = lex.scan(text, opts);
    let animals: Vec<_> = hits.iter().filter(|m| m.class == Class::Animal).collect();
    assert_eq!(
        animals.len(),
        1,
        "{text:?} must contain exactly one animal match: {hits:?}"
    );
    let id = animals[0]
        .species
        .unwrap_or_else(|| panic!("{text:?}: animal match without species"));
    lex.species_code(id).to_string()
}

fn gate_on() -> ScanOptions<'static> {
    ScanOptions {
        cjk_single_char: true,
        ..ScanOptions::default()
    }
}

/// The owner's own repro word, in all three languages, inside running text,
/// on the DEFAULT builtin: the turkey peeks.
#[test]
fn turkey_speaks_chinese_japanese_and_korean() {
    let lex = Lexicon::builtin();
    let o = ScanOptions::default();
    for (text, gloss) in [
        ("感恩节我们吃火鸡吧", "zh-Hans turkey in a sentence"),
        ("感恩節我們吃火雞吧", "zh-Hant turkey in a sentence"),
        ("今日は七面鳥を焼きます", "ja kanji turkey in a sentence"),
        ("シチメンチョウがいた", "ja katakana turkey in a sentence"),
        ("しちめんちょうかわいい", "ja hiragana turkey in a sentence"),
        ("오늘 칠면조 요리를 했다", "ko turkey in a sentence"),
    ] {
        assert_eq!(single_species(lex, text, &o), "turkey", "{gloss}");
    }
}

/// Plain multi-character forms for a spread of species, mixed into real
/// prose (including mixed-script terminal-ish lines), default options.
#[test]
fn big_animals_match_whole_word_in_mixed_cjk_text() {
    let lex = Lexicon::builtin();
    let o = ScanOptions::default();
    for (text, species) in [
        ("动物园里有大象了", "elephant"),        // zh-Hans
        ("老虎在山上", "tiger"),                 // zh
        ("build 완료 후 원숭이 확인", "monkey"), // ko inside a terminal-ish line
        ("코끼리가 크다", "elephant"),           // ko
        ("ペンギンが好きです", "penguin"),       // ja katakana
        ("小鳥のさえずり", "bird"),              // ja — the generic bird
        ("五只小鸟飞走了", "bird"),              // zh — the generic bird
        ("칠면조", "turkey"),                    // ko bare compound
        ("นกฮูกตัวใหญ่", "owl"),                    // th — compound beats นก
    ] {
        assert_eq!(single_species(lex, text, &o), species, "{text:?}");
    }
}

/// Law 2: lone ideographs / syllables stay silent without the opt-in and
/// resolve to the right species with it.
#[test]
fn single_char_animal_forms_ride_the_cjk_single_char_gate() {
    let lex = Lexicon::builtin();
    let off = ScanOptions::default();
    let on = gate_on();
    for (text, species) in [
        ("象", "elephant"),
        ("虎", "tiger"),
        ("馬", "horse"),
        ("鳥", "bird"),
        ("새", "bird"),
        ("곰", "bear"),
        ("말", "horse"),
        ("蜂", "bee"),
    ] {
        assert!(
            lex.scan(text, &off).is_empty(),
            "{text:?} must stay silent without cjk_single_char"
        );
        assert_eq!(
            single_species(lex, text, &on),
            species,
            "{text:?} under the gate"
        );
    }
}

/// The panda promotion: 熊猫/貓熊 were exceptions.toml suppressions (protecting
/// the cat ideograph); they are now positive panda matches, and the longer
/// compound STILL keeps the lone 猫 from sparkling even under the opt-in.
#[test]
fn panda_compounds_are_pandas_not_suppressions_and_never_cats() {
    let lex = Lexicon::builtin();
    for opts in [ScanOptions::default(), gate_on()] {
        for text in ["熊猫", "熊貓", "貓熊", "大熊猫"] {
            let hits = lex.scan(text, &opts);
            assert!(
                hits.iter().any(|m| m.class == Class::Animal
                    && m.species.map(|s| lex.species_code(s)) == Some("panda")),
                "{text:?} must resolve to the panda: {hits:?}"
            );
            assert!(
                hits.iter().all(|m| m.class != Class::Feline),
                "{text:?} must not sparkle feline: {hits:?}"
            );
        }
    }
}

/// Law 3: the finite prefix bombs stay suppressed even under the opt-in —
/// and the bare animal that motivated each suppression still matches.
#[test]
fn prefix_bomb_exceptions_stay_silent_and_animals_survive() {
    let lex = Lexicon::builtin();
    let on = gate_on();
    for decoy in [
        "カメラ",       // not a turtle
        "カメレオン",   // not a turtle either
        "ヘビー",       // not a snake
        "ヘビメタ",     // still not a snake
        "タコス",       // not an octopus
        "シカゴ",       // not a deer
        "ロバート",     // not a donkey
        "オウム真理教", // grimly not a parrot
        "インコース",   // not a parakeet
        "ハチ公",       // the dog is not a bee
        "クモ膜",       // not a spider
        "사자성어",     // not a lion
        "오리지널",     // not a duck
        "오리엔테이션", // not a duck
        "하마터면",     // not a hippo
        "문어체",       // not an octopus
        "ม้านั่ง",         // a bench, not a horse
        "ลิงก์",          // a link, not a monkey
    ] {
        assert!(
            lex.scan(decoy, &on).is_empty(),
            "exception compound matched: {decoy:?}"
        );
    }
    for (word, species) in [
        ("カメ", "turtle"),
        ("ヘビ", "snake"),
        ("タコ", "octopus"),
        ("シカ", "deer"),
        ("ロバ", "donkey"),
        ("オウム", "parrot"),
        ("インコ", "parrot"),
        ("ハチ", "bee"),
        ("クモ", "spider"),
        ("사자", "lion"),
        ("오리", "duck"),
        ("하마", "hippo"),
        ("문어", "octopus"),
        ("ม้า", "horse"),
        ("ลิง", "monkey"),
    ] {
        assert_eq!(
            single_species(lex, word, &ScanOptions::default()),
            species,
            "{word:?} must still match plainly"
        );
    }
}

/// Law 4, the kuma treatment: open-ended in-language homographs load only for
/// users who list the language — silent on the default builtin.
#[test]
fn homograph_bombs_are_language_gated() {
    let dflt = Lexicon::builtin();
    let o = ScanOptions::default();
    // ja くま (くまなく/くまで/隈), ko 판다 ('sells').
    assert!(dflt.scan("くま", &o).is_empty(), "くま must be ja-gated");
    assert!(dflt.scan("판다", &o).is_empty(), "판다 must be ko-gated");

    let ja = Lexicon::with_languages(&["en", "ja"]);
    assert_eq!(single_species(&ja, "くま", &o), "bear");
    assert!(
        ja.scan("くまなく", &o).is_empty(),
        "the opted-in くま still never fires inside くまなく (exception)"
    );

    let ko = Lexicon::with_languages(&["en", "ko"]);
    assert_eq!(single_species(&ko, "판다", &o), "panda");

    // Latin-script languages ride the same gate for the caret-deferral reason:
    // id 'kalkun' (turkey) is silent by default, live for id users.
    assert!(
        dflt.scan("kalkun", &o).is_empty(),
        "kalkun must be id-gated"
    );
    let id = Lexicon::with_languages(&["en", "id"]);
    assert_eq!(single_species(&id, "ada kalkun di sana", &o), "turkey");
}

/// The whole-word law holds for the new scripts: an animal compound inside a
/// LONGER listed key resolves to the longer key's species (maximal munch), and
/// hangul particles attach without breaking the match.
#[test]
fn maximal_munch_and_particles_behave() {
    let lex = Lexicon::builtin();
    let o = ScanOptions::default();
    // 돌고래 (dolphin) contains 고래 (whale): the dolphin wins.
    assert_eq!(single_species(lex, "돌고래", &o), "dolphin");
    assert_eq!(single_species(lex, "고래", &o), "whale");
    // 도마뱀 (lizard) vs lone 뱀 (snake, gated): lizard needs no gate.
    assert_eq!(single_species(lex, "도마뱀", &o), "lizard");
    // Particle-attached hangul still matches the compound inside the run.
    assert_eq!(single_species(lex, "원숭이가", &o), "monkey");
    // ไก่งวง (turkey) beats ไก่ (chicken).
    assert_eq!(single_species(lex, "ไก่งวง", &o), "turkey");
    assert_eq!(single_species(lex, "ไก่", &o), "chicken");
}

/// The multilingual expansion keeps the builtin conflict-free (species
/// uniqueness per surface, exceptions well-formed) — the data-validation
/// battery's own gate, restated closest to the new data.
#[test]
fn expanded_builtin_stays_conflict_free() {
    assert_eq!(Lexicon::builtin().conflicts(), &[] as &[String]);
    assert_eq!(
        Lexicon::with_languages(&["all"]).conflicts(),
        &[] as &[String]
    );
}
