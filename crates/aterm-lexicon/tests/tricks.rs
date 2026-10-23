// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The kitty-command vocabulary contract.

use std::collections::HashSet;

use aterm_lexicon::tricks::row_line;
use aterm_lexicon::{
    Class, Lexicon, RowKind, ScanOptions, Trick, TrickLexicon, TrickRole, TrickRow, fold,
    is_code_adjacent_punct,
};

fn classify(lex: &TrickLexicon, token: &str) -> Option<TrickRole> {
    lex.classify(token, &mut String::new())
}

fn plain(trick: Trick) -> Option<TrickRole> {
    Some(TrickRole::Trick {
        trick,
        needs_vocative: false,
    })
}

fn address(trick: Trick) -> Option<TrickRole> {
    Some(TrickRole::Trick {
        trick,
        needs_vocative: true,
    })
}

fn expected_roles(row: &TrickRow) -> Vec<(&str, TrickRole)> {
    let lone = |s: &str| row.cjk && s.chars().count() == 1;
    let mut out = Vec::new();
    for (list, is_address) in [(&row.words, false), (&row.address, true)] {
        for s in list {
            let role = match row.kind {
                RowKind::Trick(trick) => TrickRole::Trick {
                    trick,
                    needs_vocative: is_address || lone(s),
                },
                RowKind::Vocative => TrickRole::Vocative,
                RowKind::Filler => TrickRole::Filler,
            };
            out.push((s.as_str(), role));
        }
    }
    out
}

fn languages_in_the_file() -> Vec<String> {
    let mut langs: Vec<String> = Vec::new();
    for row in TrickLexicon::all_rows() {
        if !langs.contains(&row.lang) {
            langs.push(row.lang);
        }
    }
    langs
}

fn compile(src: &str, langs: &[&str]) -> TrickLexicon {
    TrickLexicon::from_source(src, langs).expect("the synthetic document parses")
}

fn conflict_naming<'a>(lex: &'a TrickLexicon, needle: &str) -> Option<&'a String> {
    lex.conflicts().iter().find(|c| c.contains(needle))
}

#[test]
fn the_embedded_vocabulary_has_no_conflicts_under_any_language_configuration() {
    let langs = languages_in_the_file();
    let mut configs: Vec<Vec<&str>> = vec![vec![], vec!["en"], vec!["all"]];
    for lang in &langs {
        configs.push(vec![lang.as_str()]);
        configs.push(vec!["en", lang.as_str()]);
    }
    aterm_spec::check::each(&configs, |config| {
        let lex = TrickLexicon::with_languages(config);
        assert!(
            lex.conflicts().is_empty(),
            "{config:?}: tricks.toml conflicts: {:#?}",
            lex.conflicts()
        );
        assert!(
            lex.surface_count() > 100,
            "{config:?}: only {} surfaces compiled — English is never gated, so \
             every configuration must carry at least the English seed",
            lex.surface_count()
        );
    });
}

#[test]
fn every_trick_has_a_plain_english_word() {
    let rows = TrickLexicon::all_rows();
    let en = TrickLexicon::shared_en();
    for trick in Trick::ALL {
        let fires_unaddressed = rows
            .iter()
            .filter(|row| row.lang == "en" && row.kind == RowKind::Trick(trick))
            .flat_map(|row| row.words.iter())
            .any(|w| classify(en, w) == plain(trick));
        assert!(
            fires_unaddressed,
            "{trick:?} has no PLAIN English word: a trick reachable only through \
             `kitty <word>` cannot be discovered by just typing it"
        );
    }
}

#[test]
fn every_authored_surface_round_trips_to_its_own_role() {
    let rows = TrickLexicon::all_rows();
    assert!(rows.len() >= Trick::ALL.len() + 2, "the listing is vacuous");
    let all = TrickLexicon::with_languages(&["all"]);
    let probed: usize = aterm_spec::check::each(&languages_in_the_file(), |lang| {
        let own = TrickLexicon::with_languages(&[lang.as_str()]);
        let mut probed = 0usize;
        for row in rows.iter().filter(|row| &row.lang == lang) {
            for (surface, role) in expected_roles(row) {
                assert_eq!(
                    classify(&all, surface),
                    Some(role),
                    "{surface:?} ({}) does not classify as its own role under [\"all\"]",
                    row.lang
                );
                assert_eq!(
                    classify(&own, surface),
                    Some(role),
                    "{surface:?} does not classify as its own role under its own \
                     language [{:?}]",
                    row.lang
                );
                probed += 1;
            }
        }
        probed
    })
    .into_iter()
    .sum();
    assert!(probed > 100, "only {probed} surfaces probed");
}

#[test]
fn authored_surfaces_are_lowercase_unique_within_a_language_and_fit_the_token_window() {
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for row in TrickLexicon::all_rows() {
        assert_eq!(row.lang, row.lang.trim(), "untrimmed lang {:?}", row.lang);
        assert!(
            !row.words.is_empty() || !row.address.is_empty(),
            "an empty row ({:?}, {}) lists nothing",
            row.kind,
            row.lang
        );
        if !matches!(row.kind, RowKind::Trick(_)) {
            assert!(
                row.address.is_empty(),
                "only a trick row has an address list"
            );
        }
        for (surface, _) in expected_roles(&row) {
            assert_eq!(
                surface,
                surface.to_lowercase(),
                "{surface:?} ({}) is not lowercase: the table folds case, so an \
                 authored capital is only noise in the listing",
                row.lang
            );
            assert!(
                surface.chars().count() <= TrickLexicon::MAX_SURFACE_CHARS,
                "{surface:?} is longer than the typed-token window"
            );
            assert!(
                seen.insert((row.lang.clone(), surface.to_string())),
                "{surface:?} is listed twice in language {:?}",
                row.lang
            );
        }
    }
}

#[test]
fn listing_rows_are_grouped_by_language_with_tricks_then_vocatives_then_fillers() {
    fn assert_grouped(rows: &[TrickRow]) {
        let rank = |kind: RowKind| match kind {
            RowKind::Trick(_) => 0,
            RowKind::Vocative => 1,
            RowKind::Filler => 2,
        };
        let mut closed: Vec<&str> = Vec::new();
        let mut current: Option<(&str, i32)> = None;
        for row in rows {
            match current {
                Some((lang, last)) if lang == row.lang => {
                    assert!(
                        rank(row.kind) >= last,
                        "{}: a {:?} row after a later kind",
                        row.lang,
                        row.kind
                    );
                }
                Some((lang, _)) => {
                    closed.push(lang);
                    assert!(
                        !closed.contains(&row.lang.as_str()),
                        "language {:?} appears in two separate runs",
                        row.lang
                    );
                }
                None => {}
            }
            current = Some((row.lang.as_str(), rank(row.kind)));
        }
    }
    assert_grouped(&TrickLexicon::all_rows());

    let src = r#"
        [[trick]]
        id = "sit"
        lang = "en"
        words = ["sit"]
        [[trick]]
        id = "sit"
        lang = "de"
        words = ["sitz"]
        [[trick]]
        id = "jump"
        lang = "en"
        words = ["jump"]
        [[vocative]]
        lang = "de"
        words = ["mieze"]
        [[vocative]]
        lang = "en"
        words = ["kitty"]
        [[filler]]
        lang = "en"
        words = ["please"]
    "#;
    let rows = TrickLexicon::rows_from_source(src).expect("parses");
    assert_grouped(&rows);
    let order: Vec<(String, RowKind)> = rows.iter().map(|r| (r.lang.clone(), r.kind)).collect();
    assert_eq!(
        order,
        vec![
            ("en".to_string(), RowKind::Trick(Trick::Sit)),
            ("en".to_string(), RowKind::Trick(Trick::Jump)),
            ("en".to_string(), RowKind::Vocative),
            ("en".to_string(), RowKind::Filler),
            ("de".to_string(), RowKind::Trick(Trick::Sit)),
            ("de".to_string(), RowKind::Vocative),
        ]
    );
}

const LINE_OPENERS: &[&str] = &[
    "sleep", "wait", "look", "play", "watch", "say", "yes", "time", "test", "kill", "touch",
    "find", "fetch", "top", "open", "read", "echo", "make", "man", "more", "less", "head", "tail",
    "cut", "paste", "join", "split", "sort", "fold", "nice", "talk", "write", "who", "which",
    "type", "set", "let", "help", "history", "jobs", "exit", "quit", "clear", "reset", "date",
    "cal", "tee", "true", "false", "env", "export", "source", "alias", "eval", "exec", "trap",
    "shift", "return", "break", "continue", "cat", "bat", "fish", "yum", "ruff", "yarn", "go",
    "git", "node", "cargo", "install", "link", "mount", "patch", "diff", "expand", "strings",
    "strip", "size", "file", "last", "leave", "lock", "login", "logout", "screen", "script",
    "units", "factor", "banner", "at", "batch", "vault", "for", "if", "then", "while", "do",
    "done", "case", "select", "until", "add", "fix", "update", "run", "check", "show", "explain",
    "remove", "delete", "rename", "move", "change", "create", "build", "use", "try", "keep",
    "hold", "drop", "pick", "give", "take", "tell", "see", "turn", "put", "get", "can", "could",
    "would", "should", "what", "why", "how", "when", "where", "is", "are", "the", "this", "that",
    "it", "i", "we", "you", "please", "ok", "okay", "now", "so", "and", "also", "but", "no",
    "nope", "good", "great", "perfect", "awesome", "thanks", "thank", "hey", "hi", "hello", "stop",
    "stay", "come", "roll", "jump", "hide", "treat", "stretch", "sit", "clean", "tidy", "walk",
    "spin", "flip", "catch", "zoom", "rest", "relax", "chill", "calm", "lay", "freeze", "focus",
    "listen", "here", "love", "bad", "off", "enough", "quiet", "surprise", "spring", "feed", "eat",
    "up", "down", "over", "away", "back", "gotcha", "yikes", "wow", "hmm", "oops",
];

const PLAIN_ON_PURPOSE: &[(&str, &str)] = &[
    (
        "sit",
        "the owner's own example; `sit tight ...` is revoked at `tight`",
    ),
    (
        "play",
        "a core verb people type first; sox `play x.wav` / `play with ...` are rare",
    ),
    (
        "jump",
        "a core verb people type first; `jump to line 40` is rare at a prompt",
    ),
    ("stretch", "the natural word; `stretch the image` is rare"),
    (
        "stay",
        "the other half of `sit, stay`; `stay on this branch` is revoked at `on`",
    ),
    (
        "down",
        "the dog-school word; almost no ordinary line STARTS with it",
    ),
    (
        "sleep",
        "THE thing one tells a cat; `sleep 5` blinks the word and is revoked at `5`",
    ),
    (
        "roll",
        "an iconic verb; `roll back the commit` is revoked at `back`",
    ),
    (
        "hide",
        "an iconic verb; `hide the sidebar` is revoked at `the`",
    ),
    (
        "treat",
        "an iconic word; `treat warnings as errors` is revoked at `warnings`",
    ),
];

#[test]
fn no_plain_english_word_opens_ordinary_command_lines_or_chat_messages() {
    let openers: HashSet<&str> = LINE_OPENERS.iter().copied().collect();
    let on_purpose: HashSet<&str> = PLAIN_ON_PURPOSE.iter().map(|(w, _)| *w).collect();
    let mut plain_words: HashSet<String> = HashSet::new();
    let mut offenders: Vec<String> = Vec::new();
    for row in TrickLexicon::all_rows() {
        if row.lang != "en" || !matches!(row.kind, RowKind::Trick(_)) {
            continue;
        }
        for w in &row.words {
            let key = fold(w);
            if openers.contains(key.as_str()) && !on_purpose.contains(key.as_str()) {
                offenders.push(format!("{w} ({:?})", row.kind));
            }
            plain_words.insert(key);
        }
    }
    assert!(
        offenders.is_empty(),
        "these PLAIN English words open ordinary lines, so typing that line \
         would flash the word and latch the pet. Move each to `address` (it \
         then needs the pet named), or add it to PLAIN_ON_PURPOSE with a \
         reason:\n  {}",
        offenders.join("\n  ")
    );
    for (word, _reason) in PLAIN_ON_PURPOSE {
        assert!(
            openers.contains(word),
            "{word:?} is excused but is not a listed line opener"
        );
        assert!(
            plain_words.contains(*word),
            "{word:?} is excused but is no longer a plain English word"
        );
    }
}

#[test]
fn the_iconic_bare_verbs_answer_unaddressed_and_the_everyday_openers_do_not() {
    let en = TrickLexicon::shared_en();
    for (word, trick) in [
        ("sit", Trick::Sit),
        ("stay", Trick::Sit),
        ("down", Trick::Down),
        ("sleep", Trick::Sleep),
        ("nap", Trick::Sleep),
        ("stretch", Trick::Stretch),
        ("jump", Trick::Jump),
        ("play", Trick::Play),
        ("roll", Trick::Roll),
        ("hide", Trick::Hide),
        ("treat", Trick::Treat),
    ] {
        assert_eq!(classify(en, word), plain(trick), "{word:?}");
    }
    for (word, trick) in [
        ("look", Trick::Look),
        ("come", Trick::Look),
        ("here", Trick::Look),
        ("run", Trick::Play),
        ("fetch", Trick::Play),
        ("wait", Trick::Sit),
        ("tidy", Trick::Groom),
    ] {
        assert_eq!(classify(en, word), address(trick), "{word:?}");
    }
}

#[test]
fn no_surface_would_hurt_someone_who_presses_enter() {
    for row in TrickLexicon::all_rows() {
        for surface in row.words.iter().chain(&row.address) {
            assert!(
                !surface.contains(['\'', '\u{2019}', '"', '`']),
                "{surface:?} ({}) carries a quote character",
                row.lang
            );
            assert!(
                !(surface.len() >= 3 && surface.chars().all(|c| c == 'z')),
                "{surface:?} ({}) is the BSD suspend command",
                row.lang
            );
        }
    }
}

#[test]
fn a_drawn_out_word_is_still_the_word() {
    let en = TrickLexicon::shared_en();
    for (typed, role) in [
        ("siiiit", plain(Trick::Sit)),
        ("staaay", plain(Trick::Sit)),
        ("sleeeep", plain(Trick::Sleep)),
        ("sleeeeeeeeep", plain(Trick::Sleep)),
        ("purrrrr", plain(Trick::Purr)),
        ("meooow", plain(Trick::Speak)),
        ("psssst", plain(Trick::Look)),
        ("JUUUMP", plain(Trick::Jump)),
        ("goooood", address(Trick::Purr)),
        ("nooo", address(Trick::Scold)),
        ("kittyyyy", Some(TrickRole::Vocative)),
        ("pleeease", Some(TrickRole::Filler)),
    ] {
        assert_eq!(classify(en, typed), role, "{typed:?}");
    }
    for typed in [
        "sitt", "sitting", "sleepy", "jumpp", "kitt", "zzz", "zzzzzz", "qqq", "sss", "siiite",
        "pllaaay",
    ] {
        assert_eq!(classify(en, typed), None, "{typed:?}");
    }
    let long = format!("s{}t", "i".repeat(200));
    assert_eq!(classify(en, &long), None);
    let tiny = TrickLexicon::from_source(
        "[[trick]]\nid = \"sit\"\nlang = \"zh\"\nwords = [\"坐下\"]\ncjk = true\n",
        &["all"],
    )
    .expect("test vocabulary parses");
    assert_eq!(classify(&tiny, "坐下"), plain(Trick::Sit));
    assert_eq!(classify(&tiny, "坐坐坐下"), None);
}

#[test]
fn praise_and_scolding_never_fire_without_the_pet_being_named() {
    let en = TrickLexicon::shared_en();
    for word in [
        "good", "nice", "great", "perfect", "awesome", "bravo", "clever",
    ] {
        assert_eq!(classify(en, word), address(Trick::Purr), "{word:?}");
    }
    for word in ["bad", "no", "nope", "stop", "naughty", "enough", "off"] {
        assert_eq!(classify(en, word), address(Trick::Scold), "{word:?}");
    }
    assert_eq!(classify(en, "goodboy"), plain(Trick::Purr));
    assert_eq!(classify(en, "badkitty"), plain(Trick::Scold));
}

#[test]
fn the_cat_speak_the_internet_knows_is_understood() {
    let en = TrickLexicon::shared_en();
    for (word, trick) in [
        ("pspsps", Trick::Look),
        ("zoomies", Trick::Play),
        ("loaf", Trick::Down),
        ("sploot", Trick::Down),
        ("boop", Trick::Paw),
        ("bap", Trick::Paw),
        ("biscuits", Trick::Purr),
        ("mlem", Trick::Groom),
        ("blep", Trick::Groom),
        ("scritches", Trick::Purr),
        ("catnip", Trick::Treat),
        ("cucumber", Trick::Boo),
        ("meow", Trick::Speak),
    ] {
        assert_eq!(classify(en, word), plain(trick), "{word:?}");
    }
    for name in [
        "kitty", "cat", "kitten", "chonk", "floof", "dog", "doggo", "puppy", "pupper", "boy",
        "girl",
    ] {
        assert_eq!(classify(en, name), Some(TrickRole::Vocative), "{name:?}");
    }
    for word in ["please", "now", "whos", "a", "up", "time"] {
        assert_eq!(classify(en, word), Some(TrickRole::Filler), "{word:?}");
    }
}

#[test]
fn the_natural_phrases_are_made_of_vocabulary_words_only() {
    let en = TrickLexicon::shared_en();
    for phrase in [
        "kitty sit",
        "good kitty",
        "whos a good boy",
        "such a clever girl",
        "lie down",
        "settle down",
        "sit down",
        "roll over kitty",
        "wake up",
        "rise and shine",
        "big stretch",
        "nap time",
        "go to sleep kitty",
        "good night kitty",
        "high five",
        "gimme paw",
        "shake hands",
        "here kitty kitty",
        "come here kitty",
        "hey kitty",
        "go away kitty",
        "good job kitty",
        "love you kitty",
        "thank you kitty",
        "bad dog",
        "no kitty",
    ] {
        for word in phrase.split(' ') {
            assert!(
                classify(en, word).is_some(),
                "{word:?} in {phrase:?} is not vocabulary, so the phrase goes impure"
            );
        }
    }
    for word in [
        "the", "tight", "back", "at", "with", "line", "npm", "build", "git",
    ] {
        assert_eq!(
            classify(en, word),
            None,
            "{word:?} must end a line's purity"
        );
    }
}

#[test]
fn inflections_and_lookalikes_never_classify() {
    let all = TrickLexicon::with_languages(&["all"]);
    for token in [
        "site",
        "sitting",
        "sits",
        "sitter",
        "runtime",
        "running",
        "sleepy",
        "sleeping",
        "sleeps",
        "napkin",
        "jumper",
        "jumping",
        "player",
        "playing",
        "playground",
        "rollback",
        "rolling",
        "lookup",
        "looking",
        "hideout",
        "hidden",
        "treaty",
        "booth",
        "boot",
        "pawn",
        "nightly",
        "purrfect",
        "stretched",
        "kitty's",
        "sit5",
        "run_tests",
        "",
    ] {
        assert_eq!(
            classify(&all, token),
            None,
            "{token:?} classifies — the table is whole-token and carries no \
             inflection sweep, so this is an authored surface that should not exist"
        );
    }
}

#[test]
fn classify_folds_case_and_strippable_marks() {
    let en = TrickLexicon::shared_en();
    for token in ["sit", "Sit", "SIT", "sIt", "si\u{0301}t", "sít"] {
        assert_eq!(classify(en, token), plain(Trick::Sit), "{token:?}");
    }
    assert_eq!(classify(en, "KITTY"), Some(TrickRole::Vocative));
    assert_eq!(classify(en, "High-Five"), plain(Trick::Paw));
    assert_eq!(classify(en, "CMERE"), plain(Trick::Look));
}

#[test]
fn the_cached_english_table_is_the_default_configuration() {
    let a = TrickLexicon::shared_en();
    let b = TrickLexicon::shared_en();
    assert!(
        std::ptr::eq(a, b),
        "shared_en must be built once and cached"
    );
    let fresh = TrickLexicon::with_languages(&["en"]);
    assert_eq!(a.surface_count(), fresh.surface_count());
    for row in TrickLexicon::all_rows() {
        for (surface, _) in expected_roles(&row) {
            assert_eq!(
                classify(a, surface),
                classify(&fresh, surface),
                "{surface:?}"
            );
        }
    }
    let empty = TrickLexicon::default();
    assert_eq!(empty.surface_count(), 0);
    assert_eq!(classify(&empty, "sit"), None);
    assert!(empty.conflicts().is_empty());
}

const SHARED_VOICE: &[&str] = &["meow", "mew", "miaow"];

fn shared_class(shared: &Lexicon, surface: &str) -> Option<Class> {
    let opts = ScanOptions {
        allow_bare_cat: true,
        cjk_single_char: true,
        ignore: None,
    };
    shared
        .classify_token(surface)
        .or_else(|| shared.scan(surface, &opts).first().map(|m| m.class))
}

#[test]
fn no_trick_address_or_filler_surface_is_a_shared_sparkle_lexicon_surface() {
    let shared = Lexicon::with_languages(&["all"]);
    assert_eq!(
        shared_class(&shared, "meow"),
        Some(Class::Feline),
        "the detector must not be vacuous"
    );
    assert_eq!(shared_class(&shared, "bat"), Some(Class::Animal));
    let mut offenders: Vec<String> = Vec::new();
    for row in TrickLexicon::all_rows() {
        if row.kind == RowKind::Vocative {
            continue;
        }
        for (surface, _) in expected_roles(&row) {
            let Some(class) = shared_class(&shared, surface) else {
                continue;
            };
            let the_cats_voice = row.kind == RowKind::Trick(Trick::Speak)
                && class == Class::Feline
                && SHARED_VOICE.contains(&fold(surface).as_str());
            if !the_cats_voice {
                offenders.push(format!(
                    "{surface} ({}, {:?}) -> shared {class:?}",
                    row.lang, row.kind
                ));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "these trick surfaces are already owned by the screen scanner. Drop or \
         replace each one (only meow / mew / miaow under `speak` are excused):\n  {}",
        offenders.join("\n  ")
    );
    let en = TrickLexicon::shared_en();
    for voice in SHARED_VOICE {
        assert_eq!(classify(en, voice), plain(Trick::Speak), "{voice:?}");
    }
}

#[test]
fn nothing_in_the_vocabulary_is_profanity_in_any_language() {
    let shared = Lexicon::with_languages(&["all"]);
    assert_eq!(shared_class(&shared, "fuck"), Some(Class::Profanity));
    for row in TrickLexicon::all_rows() {
        for (surface, _) in expected_roles(&row) {
            assert_ne!(
                shared_class(&shared, surface),
                Some(Class::Profanity),
                "{surface:?} ({}) is an expletive in some language",
                row.lang
            );
        }
    }
}

const DICTIONARY_EXCUSED: &[(&str, &str)] = &[
    (
        "aport",
        "THE fetch command across the Slavic and Baltic languages; an obscure Webster's-Second headword",
    ),
    (
        "apport",
        "THE fetch command in German, Dutch and the Nordic languages; an obscure Webster's-Second headword",
    ),
    (
        "assis",
        "THE French sit command; a heraldry term in Webster's Second",
    ),
    ("assise", "the feminine of THE French sit command"),
    (
        "bain",
        "fr groom word; a Webster's-Second curiosity no English line opens with",
    ),
    (
        "baith",
        "hi sit word; a Webster's-Second curiosity no English line opens with",
    ),
    (
        "balza",
        "it jump word; a Webster's-Second curiosity no English line opens with",
    ),
    (
        "bate",
        "pt,ro paw word; a Webster's-Second curiosity no English line opens with",
    ),
    (
        "betise",
        "fr scold word; a Webster's-Second curiosity no English line opens with",
    ),
    (
        "bota",
        "ca jump word; a Webster's-Second curiosity no English line opens with",
    ),
    (
        "capriola",
        "it roll word; a Webster's-Second curiosity no English line opens with",
    ),
    (
        "choca",
        "es paw word; a Webster's-Second curiosity no English line opens with",
    ),
    (
        "cinque",
        "it paw word; a Webster's-Second curiosity no English line opens with",
    ),
    (
        "comer",
        "es treat word; a Webster's-Second curiosity no English line opens with",
    ),
    (
        "dolent",
        "ca scold word; a Webster's-Second curiosity no English line opens with",
    ),
    (
        "friandise",
        "fr treat word; a Webster's-Second curiosity no English line opens with",
    ),
    (
        "gambade",
        "fr play word; a Webster's-Second curiosity no English line opens with",
    ),
    (
        "griffe",
        "fr paw word; a Webster's-Second curiosity no English line opens with",
    ),
    (
        "hau",
        "fi speak word; a Webster's-Second curiosity no English line opens with",
    ),
    (
        "jama",
        "sv speak word; a Webster's-Second curiosity no English line opens with",
    ),
    (
        "kala",
        "fi treat word; a Webster's-Second curiosity no English line opens with",
    ),
    (
        "kuku",
        "peekaboo across six central-European languages; a plant name in Webster's Second",
    ),
    (
        "lecker",
        "German `yummy`; a dialect headword in Webster's Second",
    ),
    (
        "lek",
        "THE Scandinavian play command; to English speakers a currency and a grouse display, never a line opener",
    ),
    (
        "linge",
        "ro groom word; a Webster's-Second curiosity no English line opens with",
    ),
    (
        "lur",
        "da,is,no sleep word; a Webster's-Second curiosity no English line opens with",
    ),
    (
        "maha",
        "et down word; a Webster's-Second curiosity no English line opens with",
    ),
    (
        "maius",
        "et treat word; a Webster's-Second curiosity no English line opens with",
    ),
    (
        "miao",
        "the Italian meow; to Webster's Second a people of China",
    ),
    (
        "nam",
        "the `yum` of five languages (es ñam); not a word English speakers open a line with",
    ),
    (
        "natt",
        "Scandinavian `night`; not a word English speakers type",
    ),
    (
        "pac",
        "sk paw word; a Webster's-Second curiosity no English line opens with",
    ),
    (
        "palma",
        "ro paw word; a Webster's-Second curiosity no English line opens with",
    ),
    (
        "parado",
        "pt sit word; a Webster's-Second curiosity no English line opens with",
    ),
    (
        "parle",
        "fr speak word; a Webster's-Second curiosity no English line opens with",
    ),
    (
        "pata",
        "THE paw command in Spanish and Portuguese; an obscure headword",
    ),
    ("patte", "THE French paw command; an obscure headword"),
    (
        "pepino",
        "es,pt boo word; a Webster's-Second curiosity no English line opens with",
    ),
    (
        "pfui",
        "THE German scolding interjection; listed by Webster's Second as exactly that",
    ),
    (
        "pist",
        "tr boo word; a Webster's-Second curiosity no English line opens with",
    ),
    (
        "plass",
        "no down word; a Webster's-Second curiosity no English line opens with",
    ),
    (
        "pote",
        "THE Danish and Norwegian paw command; an obsolete English verb",
    ),
    (
        "rull",
        "no roll word; a Webster's-Second curiosity no English line opens with",
    ),
    (
        "rusine",
        "ro scold word; a Webster's-Second curiosity no English line opens with",
    ),
    (
        "salta",
        "THE jump command in Spanish, Italian, Portuguese and Catalan; a place name in Webster's Second",
    ),
    (
        "saut",
        "THE French jump noun people call out; an obsolete headword in Webster's Second",
    ),
    (
        "sitta",
        "THE Swedish sit command; a bird genus in Webster's Second",
    ),
    (
        "sov",
        "THE Scandinavian sleep command; not a word English speakers type",
    ),
    (
        "speel",
        "THE Dutch play command; a dialect headword in Webster's Second",
    ),
    (
        "streck",
        "de stretch word; a Webster's-Second curiosity no English line opens with",
    ),
    (
        "tala",
        "is,sv speak word; a Webster's-Second curiosity no English line opens with",
    ),
    (
        "thon",
        "fr treat word; a Webster's-Second curiosity no English line opens with",
    ),
    (
        "tope",
        "fr paw word; a Webster's-Second curiosity no English line opens with",
    ),
    (
        "vila",
        "sv down word; a Webster's-Second curiosity no English line opens with",
    ),
    (
        "vina",
        "ca look word; a Webster's-Second curiosity no English line opens with",
    ),
    (
        "vira",
        "pt roll word; a Webster's-Second curiosity no English line opens with",
    ),
    (
        "wouf",
        "fr speak word; a Webster's-Second curiosity no English line opens with",
    ),
    (
        "yat",
        "tr down word; a Webster's-Second curiosity no English line opens with",
    ),
    (
        "zat",
        "ro boo word; a Webster's-Second curiosity no English line opens with",
    ),
];

fn ungated_foreign_surfaces_that_are_english_words<'a>(
    rows: &[TrickRow],
    default_config: &TrickLexicon,
    dictionary: impl Iterator<Item = &'a str>,
) -> Vec<String> {
    let english: HashSet<String> = rows
        .iter()
        .filter(|row| row.lang == "en")
        .flat_map(|row| row.words.iter().chain(row.address.iter()))
        .map(|s| fold(s))
        .collect();
    let mut offenders: Vec<String> = dictionary
        .filter_map(|word| {
            let w = word.trim().to_lowercase();
            if w.is_empty() || !w.is_ascii() || english.contains(&w) {
                return None;
            }
            match classify(default_config, &w) {
                Some(
                    role @ TrickRole::Trick {
                        needs_vocative: false,
                        ..
                    },
                ) => Some(format!("{w} -> {role:?}")),
                _ => None,
            }
        })
        .collect();
    offenders.sort();
    offenders.dedup();
    offenders
}

#[test]
fn no_ungated_non_english_surface_is_an_english_dictionary_word() {
    let raw = crate::web2_words();
    let rows = TrickLexicon::all_rows();
    let excused: HashSet<&str> = DICTIONARY_EXCUSED.iter().map(|(w, _)| *w).collect();
    let offenders: Vec<String> = ungated_foreign_surfaces_that_are_english_words(
        &rows,
        &TrickLexicon::with_languages(&["en"]),
        raw.lines(),
    )
    .into_iter()
    .filter(|line| !excused.contains(line.split(' ').next().unwrap_or("")))
    .collect();
    assert!(
        offenders.is_empty(),
        "ungated non-English trick surfaces are ordinary English words. Mark \
         the owning row `gated = true` (split the risky surface into a gated \
         sibling row), or add it to DICTIONARY_EXCUSED with a reason:\n  {}",
        offenders.join("\n  ")
    );
}

#[test]
fn the_dictionary_gate_catches_an_ungated_foreign_english_word_and_clears_a_gated_one() {
    let doc = |gated: bool| {
        format!(
            r#"
            [[trick]]
            id = "sit"
            lang = "en"
            words = ["sit"]
            [[trick]]
            id = "play"
            lang = "id"
            words = ["main", "bermain"]
            gated = {gated}
            [[trick]]
            id = "down"
            lang = "fr"
            words = ["couché"]
            "#
        )
    };
    let dictionary = ["sit", "main", "couche", "zebra"];
    let ungated = doc(false);
    let offenders = ungated_foreign_surfaces_that_are_english_words(
        &TrickLexicon::rows_from_source(&ungated).expect("parses"),
        &compile(&ungated, &["en"]),
        dictionary.into_iter(),
    );
    assert_eq!(
        offenders.len(),
        1,
        "exactly `main` must be charged: {offenders:?}"
    );
    assert!(offenders[0].starts_with("main -> "), "{offenders:?}");

    let gated = doc(true);
    let offenders = ungated_foreign_surfaces_that_are_english_words(
        &TrickLexicon::rows_from_source(&gated).expect("parses"),
        &compile(&gated, &["en"]),
        dictionary.into_iter(),
    );
    assert!(
        offenders.is_empty(),
        "a gated row is the remedy: {offenders:?}"
    );
}

const ROSTER: &[&str] = &[
    "en", "es", "fr", "de", "it", "pt", "nl", "sv", "pl", "ru", "uk", "tr", "ar", "he", "fa", "hi",
    "bn", "id", "vi", "th", "zh", "ja", "ko",
];

const CORE_GAPS: &[(&str, &str, &str)] = &[];

fn reaches(rows: &[TrickRow], lex: &TrickLexicon, lang: &str, trick: Trick) -> bool {
    rows.iter()
        .filter(|row| row.lang == lang && row.kind == RowKind::Trick(trick))
        .flat_map(|row| row.words.iter().chain(row.address.iter()))
        .any(|s| matches!(classify(lex, s), Some(TrickRole::Trick { trick: t, .. }) if t == trick))
}

#[test]
fn every_language_in_the_file_reaches_every_trick() {
    let rows = TrickLexicon::all_rows();
    let langs = languages_in_the_file();
    assert!(langs.iter().any(|l| l == "en"), "the file lost English");
    aterm_spec::check::each(&langs, |lang| {
        let lex = TrickLexicon::with_languages(&[lang.as_str()]);
        for trick in Trick::ALL {
            let excused = CORE_GAPS
                .iter()
                .any(|(l, code, _)| l == lang && *code == trick.code());
            let reached = reaches(&rows, &lex, lang, trick);
            assert!(
                reached || excused,
                "language {lang:?} does not reach the trick {:?}: add a \
                 surface, or record the gap in CORE_GAPS with the reason no \
                 natural single token exists",
                trick.code()
            );
            assert!(
                !(reached && excused),
                "CORE_GAPS excuses ({lang:?}, {:?}) but the data reaches it — \
                 delete the stale row",
                trick.code()
            );
        }
    });
    for (lang, code, _) in CORE_GAPS {
        assert!(
            langs.iter().any(|l| l == lang) && Trick::from_code(code).is_some(),
            "CORE_GAPS names ({lang:?}, {code:?}), which is not in the file"
        );
    }
    let absent: Vec<&str> = ROSTER
        .iter()
        .copied()
        .filter(|r| !langs.iter().any(|l| l == r))
        .collect();
    if !absent.is_empty() {
        eprintln!("tricks coverage: roster languages not in the file yet: {absent:?}");
    }
}

#[test]
fn trick_codes_round_trip_and_all_is_in_declaration_order() {
    let mut codes: HashSet<&str> = HashSet::new();
    for (i, trick) in Trick::ALL.into_iter().enumerate() {
        assert_eq!(trick as usize, i, "{trick:?} is out of declaration order");
        let code = trick.code();
        assert_eq!(Trick::from_code(code), Some(trick));
        assert!(codes.insert(code), "two tricks share the code {code:?}");
        assert!(
            !code.is_empty() && code.bytes().all(|b| b.is_ascii_lowercase()),
            "{code:?} is not a bare lowercase id"
        );
    }
    for not_a_code in ["", "Sit", "SIT", " sit", "sit ", "sitting", "meow", "kitty"] {
        assert_eq!(Trick::from_code(not_a_code), None, "{not_a_code:?}");
    }
}

#[test]
fn row_line_is_stable() {
    let row = |kind, words: &[&str], address: &[&str], gated| TrickRow {
        kind,
        lang: "en".to_string(),
        words: words.iter().map(|s| (*s).to_string()).collect(),
        address: address.iter().map(|s| (*s).to_string()).collect(),
        gated,
        cjk: false,
        unglued: Vec::new(),
    };
    assert_eq!(
        row_line(&row(
            RowKind::Trick(Trick::Down),
            &["loaf", "sploot"],
            &["down"],
            false
        )),
        "command trick=down lang=en gated=0 words=loaf,sploot address=down does=lies_down_in_a_loaf"
    );
    assert_eq!(
        row_line(&row(RowKind::Trick(Trick::Boo), &["boo"], &[], true)),
        "command trick=boo lang=en gated=1 words=boo address=- does=jumps_out_of_its_skin"
    );
    assert_eq!(
        row_line(&row(RowKind::Trick(Trick::Scold), &[], &["no"], false)),
        "command trick=scold lang=en gated=0 words=- address=no \
         does=flattens_its_ears_for_a_moment"
    );
    assert_eq!(
        row_line(&row(RowKind::Vocative, &["kitty", "cat"], &[], false)),
        "vocative lang=en gated=0 words=kitty,cat"
    );
    assert_eq!(
        row_line(&row(RowKind::Filler, &["please", "who's"], &[], false)),
        "filler lang=en gated=0 words=please,who's"
    );
    let ja = TrickRow {
        kind: RowKind::Trick(Trick::Sit),
        lang: "ja".to_string(),
        words: vec!["おすわり".to_string(), "お座り".to_string()],
        address: vec![],
        gated: false,
        cjk: true,
        unglued: vec!["お座り".to_string()],
    };
    assert_eq!(
        row_line(&ja),
        "command trick=sit lang=ja gated=0 words=おすわり,お座り address=- \
         does=sits_up_and_looks_at_you_for_a_few_seconds"
    );
}

#[test]
fn every_listing_line_of_the_embedded_file_is_one_line_of_key_value_fields() {
    let rows = TrickLexicon::all_rows();
    let mut codes_listed: HashSet<&str> = HashSet::new();
    for row in &rows {
        let line = row_line(row);
        assert!(!line.contains('\n') && !line.contains("  "), "{line:?}");
        let mut fields = line.split(' ');
        let head = fields.next().expect("a line has a head word");
        let expect_keys: &[&str] = match row.kind {
            RowKind::Trick(trick) => {
                assert_eq!(head, "command");
                codes_listed.insert(trick.code());
                &["trick", "lang", "gated", "words", "address", "does"]
            }
            RowKind::Vocative => {
                assert_eq!(head, "vocative");
                &["lang", "gated", "words"]
            }
            RowKind::Filler => {
                assert_eq!(head, "filler");
                &["lang", "gated", "words"]
            }
        };
        let keys: Vec<&str> = fields
            .map(|f| f.split_once('=').expect("every field is key=value").0)
            .collect();
        assert_eq!(keys, expect_keys, "{line:?}");
    }
    for trick in Trick::ALL {
        assert!(
            codes_listed.contains(trick.code()),
            "the listing never shows {:?}",
            trick.code()
        );
    }
}

#[test]
fn an_unknown_trick_id_is_a_conflict_and_its_row_is_skipped() {
    let src = r#"
        [[trick]]
        id = "sit"
        lang = "en"
        words = ["sit"]
        [[trick]]
        id = "moonwalk"
        lang = "en"
        words = ["moonwalk"]
    "#;
    let lex = compile(src, &["en"]);
    assert_eq!(classify(&lex, "sit"), plain(Trick::Sit));
    assert_eq!(classify(&lex, "moonwalk"), None);
    assert_eq!(lex.conflicts().len(), 1, "{:?}", lex.conflicts());
    assert!(
        conflict_naming(&lex, "unknown trick id \"moonwalk\"").is_some(),
        "{:?}",
        lex.conflicts()
    );
    assert_eq!(
        TrickLexicon::rows_from_source(src).expect("parses").len(),
        1
    );
}

#[test]
fn a_surface_claimed_under_two_roles_keeps_the_first_and_says_so() {
    let src = r#"
        [[trick]]
        id = "sit"
        lang = "en"
        words = ["sit", "down"]
        [[trick]]
        id = "down"
        lang = "en"
        words = ["loaf"]
        address = ["down", "sit"]
        [[vocative]]
        lang = "en"
        words = ["kitty", "loaf"]
        [[filler]]
        lang = "en"
        words = ["kitty", "please"]
    "#;
    let lex = compile(src, &["en"]);
    assert_eq!(classify(&lex, "down"), plain(Trick::Sit));
    assert_eq!(classify(&lex, "sit"), plain(Trick::Sit));
    assert_eq!(classify(&lex, "loaf"), plain(Trick::Down));
    assert_eq!(classify(&lex, "kitty"), Some(TrickRole::Vocative));
    assert_eq!(classify(&lex, "please"), Some(TrickRole::Filler));
    assert_eq!(lex.conflicts().len(), 4, "{:#?}", lex.conflicts());
    for word in ["\"down\"", "\"sit\"", "\"loaf\"", "\"kitty\""] {
        let c = conflict_naming(&lex, word).unwrap_or_else(|| panic!("no conflict names {word}"));
        assert!(c.contains("kept the first"), "{c}");
    }
}

#[test]
fn a_cross_language_claim_is_won_by_the_language_that_opens_the_file() {
    let de_first = r#"
        [[trick]]
        id = "jump"
        lang = "de"
        words = ["hopp", "spring"]
        [[trick]]
        id = "jump"
        lang = "en"
        address = ["spring"]
    "#;
    let lex = compile(de_first, &["all"]);
    assert_eq!(classify(&lex, "spring"), plain(Trick::Jump));
    assert_eq!(lex.conflicts().len(), 1);

    let en_first = r#"
        [[trick]]
        id = "sit"
        lang = "en"
        words = ["sit"]
        [[trick]]
        id = "jump"
        lang = "de"
        words = ["hopp", "spring"]
        [[trick]]
        id = "jump"
        lang = "en"
        address = ["spring"]
    "#;
    let lex = compile(en_first, &["all"]);
    assert_eq!(classify(&lex, "spring"), address(Trick::Jump));
    assert_eq!(lex.conflicts().len(), 1);
}

#[test]
fn the_same_role_claimed_again_merges_silently() {
    let src = r#"
        [[trick]]
        id = "jump"
        lang = "es"
        words = ["salta"]
        [[trick]]
        id = "jump"
        lang = "it"
        words = ["salta", "salta"]
        [[trick]]
        id = "jump"
        lang = "pt"
        words = ["salta"]
    "#;
    let lex = compile(src, &["all"]);
    assert_eq!(classify(&lex, "salta"), plain(Trick::Jump));
    assert!(lex.conflicts().is_empty(), "{:?}", lex.conflicts());
    assert_eq!(lex.surface_count(), 1);
}

#[test]
fn a_surface_that_is_not_one_token_is_dropped_and_named() {
    let src = r#"
        [[trick]]
        id = "roll"
        lang = "en"
        words = ["rollover", "roll over", "roll.over", "roll-over", "-roll", "c'mon", "", "   "]
        [[trick]]
        id = "sit"
        lang = "zh"
        words = ["坐下"]
    "#;
    let lex = compile(src, &["all"]);
    for ok in ["rollover", "roll-over", "c'mon"] {
        assert_eq!(classify(&lex, ok), plain(Trick::Roll), "{ok:?}");
    }
    for dropped in ["roll over", "roll.over", "-roll", "坐下", "roll", "over"] {
        assert_eq!(classify(&lex, dropped), None, "{dropped:?}");
    }
    assert_eq!(lex.conflicts().len(), 4, "{:#?}", lex.conflicts());
    for word in ["\"roll over\"", "\"roll.over\"", "\"-roll\"", "\"坐下\""] {
        let c = conflict_naming(&lex, word).unwrap_or_else(|| panic!("no conflict names {word}"));
        assert!(c.contains("not a single whole-word token"), "{c}");
    }
}

#[test]
fn a_cjk_row_takes_no_space_script_surfaces_only_and_matches_the_whole_run() {
    let src = r#"
        [[trick]]
        id = "sit"
        lang = "zh"
        words = ["坐下", "sit下", "坐 下"]
        cjk = true
        [[trick]]
        id = "sit"
        lang = "ko"
        words = ["앉아"]
        cjk = true
        [[trick]]
        id = "sit"
        lang = "th"
        words = ["นั่ง"]
        cjk = true
        [[vocative]]
        lang = "ja"
        words = ["ねこ"]
        cjk = true
    "#;
    let lex = compile(src, &["all"]);
    assert_eq!(classify(&lex, "坐下"), plain(Trick::Sit));
    assert_eq!(classify(&lex, "앉아"), plain(Trick::Sit));
    assert_eq!(classify(&lex, "นั่ง"), plain(Trick::Sit));
    assert_eq!(classify(&lex, "ねこ"), Some(TrickRole::Vocative));
    for run in ["坐下来", "请坐下", "坐", "下", "앉아서", "앉", "ねこちゃん"] {
        assert_eq!(classify(&lex, run), None, "{run:?}");
    }
    assert_eq!(lex.conflicts().len(), 2, "{:#?}", lex.conflicts());
    for word in ["\"sit下\"", "\"坐 下\""] {
        let c = conflict_naming(&lex, word).unwrap_or_else(|| panic!("no conflict names {word}"));
        assert!(c.contains("no-space-script surfaces only"), "{c}");
    }
}

#[test]
fn a_single_character_no_space_surface_always_needs_the_vocative() {
    let src = r#"
        [[trick]]
        id = "sit"
        lang = "zh"
        words = ["坐", "坐下"]
        address = ["坐好"]
        cjk = true
        [[trick]]
        id = "jump"
        lang = "zh"
        words = ["跳"]
        cjk = true
        [[trick]]
        id = "jump"
        lang = "ja"
        address = ["跳"]
        cjk = true
        [[vocative]]
        lang = "zh"
        words = ["猫", "猫咪"]
        cjk = true
    "#;
    let lex = compile(src, &["all"]);
    assert_eq!(classify(&lex, "坐"), address(Trick::Sit));
    assert_eq!(classify(&lex, "跳"), address(Trick::Jump));
    assert_eq!(classify(&lex, "坐下"), plain(Trick::Sit));
    assert_eq!(classify(&lex, "坐好"), address(Trick::Sit));
    assert_eq!(classify(&lex, "猫"), Some(TrickRole::Vocative));
    assert!(lex.conflicts().is_empty(), "{:?}", lex.conflicts());
}

#[test]
fn no_lone_ideograph_in_the_embedded_file_fires_unaddressed() {
    let all = TrickLexicon::with_languages(&["all"]);
    for row in TrickLexicon::all_rows() {
        if !row.cjk || !matches!(row.kind, RowKind::Trick(_)) {
            continue;
        }
        for s in row.words.iter().chain(row.address.iter()) {
            if s.chars().count() == 1 {
                assert!(
                    matches!(
                        classify(&all, s),
                        Some(TrickRole::Trick {
                            needs_vocative: true,
                            ..
                        })
                    ),
                    "lone {s:?} ({}) fires unaddressed",
                    row.lang
                );
            }
        }
    }
}

#[test]
fn a_marked_surface_needs_marked_typing_unless_the_bare_spelling_is_listed() {
    let src = r#"
        [[trick]]
        id = "down"
        lang = "fr"
        words = ["couché"]
        [[trick]]
        id = "sit"
        lang = "es"
        words = ["siéntate", "sientate"]
        [[trick]]
        id = "sleep"
        lang = "ru"
        words = ["спать"]
        [[trick]]
        id = "jump"
        lang = "vi"
        words = ["nhảy"]
    "#;
    let lex = compile(src, &["all"]);
    assert!(lex.conflicts().is_empty(), "{:?}", lex.conflicts());
    assert_eq!(classify(&lex, "couché"), plain(Trick::Down));
    assert_eq!(classify(&lex, "COUCHÉ"), plain(Trick::Down));
    assert_eq!(classify(&lex, "couche\u{0301}"), plain(Trick::Down));
    assert_eq!(classify(&lex, "couche"), None);
    assert_eq!(classify(&lex, "COUCHE"), None);
    assert_eq!(classify(&lex, "siéntate"), plain(Trick::Sit));
    assert_eq!(classify(&lex, "sientate"), plain(Trick::Sit));
    assert_eq!(classify(&lex, "спать"), plain(Trick::Sleep));
    assert_eq!(classify(&lex, "СПАТЬ"), plain(Trick::Sleep));
    assert_eq!(classify(&lex, "nhảy"), plain(Trick::Jump));
    assert_eq!(classify(&lex, "nhay"), None);
}

#[test]
fn gated_rows_stay_silent_until_their_language_is_listed_and_english_is_never_gated() {
    let src = r#"
        [[trick]]
        id = "sit"
        lang = "en"
        words = ["sit"]
        gated = true
        [[trick]]
        id = "play"
        lang = "id"
        words = ["bermain"]
        [[trick]]
        id = "play"
        lang = "id"
        words = ["main"]
        gated = true
        [[vocative]]
        lang = "id"
        words = ["kucing"]
        gated = true
    "#;
    for config in [&[][..], &["en"][..], &["fr"][..]] {
        let lex = compile(src, config);
        assert_eq!(classify(&lex, "sit"), plain(Trick::Sit), "{config:?}");
        assert_eq!(classify(&lex, "bermain"), plain(Trick::Play), "{config:?}");
        assert_eq!(classify(&lex, "main"), None, "{config:?}");
        assert_eq!(classify(&lex, "kucing"), None, "{config:?}");
    }
    for config in [&["id"][..], &["en", "id"][..], &["all"][..]] {
        let lex = compile(src, config);
        assert_eq!(classify(&lex, "main"), plain(Trick::Play), "{config:?}");
        assert_eq!(
            classify(&lex, "kucing"),
            Some(TrickRole::Vocative),
            "{config:?}"
        );
        assert_eq!(classify(&lex, "sit"), plain(Trick::Sit), "{config:?}");
        assert_eq!(lex.conflicts().len(), 1, "{:?}", lex.conflicts());
        assert!(conflict_naming(&lex, "English is never gated").is_some());
    }
}

#[test]
fn gated_rows_of_the_embedded_file_are_silent_by_default() {
    let rows = TrickLexicon::all_rows();
    let default_config = TrickLexicon::with_languages(&["en"]);
    let ungated: HashSet<String> = rows
        .iter()
        .filter(|row| !row.gated)
        .flat_map(|row| row.words.iter().chain(row.address.iter()))
        .flat_map(|s| [s.clone(), fold(s)])
        .collect();
    for row in rows.iter().filter(|row| row.gated) {
        assert_ne!(row.lang, "en", "English is never gated");
        for s in row.words.iter().chain(row.address.iter()) {
            if !ungated.contains(s) && !ungated.contains(&fold(s)) {
                assert_eq!(
                    classify(&default_config, s),
                    None,
                    "gated {s:?} ({}) loads by default",
                    row.lang
                );
            }
        }
    }
}

#[test]
fn a_surface_longer_than_the_token_window_is_dropped_and_named() {
    let fits = "z".repeat(TrickLexicon::MAX_SURFACE_CHARS);
    let too_long = "z".repeat(TrickLexicon::MAX_SURFACE_CHARS + 1);
    let src = format!(
        r#"
        [[trick]]
        id = "sleep"
        lang = "en"
        words = ["{fits}", "{too_long}"]
        "#
    );
    let lex = compile(&src, &["en"]);
    assert_eq!(classify(&lex, &fits), plain(Trick::Sleep));
    assert_eq!(classify(&lex, &too_long), None);
    assert_eq!(lex.conflicts().len(), 1);
    assert!(conflict_naming(&lex, "longer than the typed-token window").is_some());
    assert_eq!(TrickLexicon::MAX_SURFACE_CHARS, 32);
}

#[test]
fn a_row_without_a_language_is_a_conflict_and_a_typo_in_a_key_is_a_parse_error() {
    let no_lang = r#"
        [[trick]]
        id = "sit"
        lang = "  "
        words = ["sit"]
        [[filler]]
        lang = ""
        words = ["please"]
    "#;
    let lex = compile(no_lang, &["all"]);
    assert_eq!(lex.surface_count(), 0);
    assert_eq!(lex.conflicts().len(), 2, "{:?}", lex.conflicts());
    assert!(conflict_naming(&lex, "row without lang (trick sit)").is_some());
    assert!(conflict_naming(&lex, "row without lang (filler)").is_some());

    let typo = r#"
        [[trick]]
        id = "sit"
        lang = "en"
        words = ["sit"]
        adress = ["stay"]
    "#;
    assert!(TrickLexicon::from_source(typo, &["en"]).is_err());
    assert!(TrickLexicon::rows_from_source(typo).is_err());
    assert!(TrickLexicon::from_source("[[trick]]\nid = \"sit\"\n", &["en"]).is_err());
    assert!(TrickLexicon::from_source("[[trick", &["en"]).is_err());
    let empty = compile("", &["all"]);
    assert_eq!(empty.surface_count(), 0);
    assert!(empty.conflicts().is_empty());
}

#[test]
fn the_exported_code_context_set_is_the_one_the_scanner_uses() {
    let shared = Lexicon::builtin();
    let opts = ScanOptions::default();
    let mut code_like = 0;
    for c in (0x21u8..=0x7e).map(char::from) {
        if !c.is_ascii_punctuation() || c == '_' {
            continue;
        }
        let refused = shared.scan(&format!("{c}kitty"), &opts).is_empty();
        assert_eq!(
            refused,
            is_code_adjacent_punct(c) || c == '-',
            "{c:?}: the scanner and the exported predicate disagree"
        );
        code_like += usize::from(is_code_adjacent_punct(c));
    }
    assert_eq!(code_like, 16, "the code-context set changed size");
    for prose in ['.', ',', '!', '?', '\'', '"', '(', ')', ' ', 'a', '5'] {
        assert!(!is_code_adjacent_punct(prose), "{prose:?}");
    }
}

#[test]
fn an_unglued_surface_is_a_word_whole_and_no_half_of_a_run() {
    let lex = compile(
        r#"
[[trick]]
id      = "play"
lang    = "zh"
words   = ["玩耍", "玩具"]
cjk     = true
unglued = ["玩具"]

[[vocative]]
lang    = "zh"
words   = ["猫咪", "宝宝"]
cjk     = true
unglued = ["宝宝"]
"#,
        &["all"],
    );
    assert!(lex.conflicts().is_empty(), "{:?}", lex.conflicts());
    let mut scratch = String::new();
    assert_eq!(classify(&lex, "玩具"), plain(Trick::Play));
    assert_eq!(classify(&lex, "宝宝"), Some(TrickRole::Vocative));
    assert_eq!(lex.classify_glued_half("玩具", &mut scratch), None);
    assert_eq!(lex.classify_glued_half("宝宝", &mut scratch), None);
    assert_eq!(
        lex.classify_glued_half("玩耍", &mut scratch),
        plain(Trick::Play)
    );
    assert_eq!(
        lex.classify_glued_half("猫咪", &mut scratch),
        Some(TrickRole::Vocative)
    );
    let shipped = TrickLexicon::with_languages(&["all"]);
    for word in ["喂", "มอง"] {
        assert_eq!(classify(&shipped, word), address(Trick::Look), "{word}");
        assert_eq!(
            shipped.classify_glued_half(word, &mut scratch),
            None,
            "{word}"
        );
    }
}

#[test]
fn an_unglued_entry_that_names_nothing_in_its_row_is_a_conflict() {
    let lex = compile(
        r#"
[[trick]]
id      = "play"
lang    = "zh"
words   = ["玩具"]
cjk     = true
unglued = ["玩县"]

[[trick]]
id      = "sit"
lang    = "en"
words   = ["sit"]
unglued = ["sit"]
"#,
        &["all"],
    );
    assert!(
        conflict_naming(&lex, "玩县").is_some(),
        "{:?}",
        lex.conflicts()
    );
    assert!(
        conflict_naming(&lex, "\"sit\"").is_some(),
        "a spaced row has no glued reading: {:?}",
        lex.conflicts()
    );
    assert_eq!(
        lex.classify_glued_half("玩具", &mut String::new()),
        plain(Trick::Play),
        "the typo marked nothing"
    );
}

#[test]
fn one_claimant_keeping_a_surface_out_of_the_glued_reading_keeps_it_out() {
    let lex = compile(
        r#"
[[trick]]
id      = "paw"
lang    = "zh"
words   = ["握手"]
cjk     = true

[[trick]]
id      = "paw"
lang    = "ja"
words   = ["握手"]
cjk     = true
unglued = ["握手"]
"#,
        &["all"],
    );
    assert!(lex.conflicts().is_empty(), "{:?}", lex.conflicts());
    assert_eq!(classify(&lex, "握手"), plain(Trick::Paw));
    assert_eq!(lex.classify_glued_half("握手", &mut String::new()), None);
}
