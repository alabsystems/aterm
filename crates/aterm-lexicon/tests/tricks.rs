// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE KITTY-COMMAND VOCABULARY CONTRACT.
//!
//! Two kinds of test live here and both matter:
//!
//! * DATA tests walk the embedded `data/tricks.toml` through the public API.
//!   They are DATA-DRIVEN on purpose — they iterate the languages and rows the
//!   file actually holds — so they keep their meaning when the multilingual
//!   groups are appended after the English seed. Where a law is VACUOUS for
//!   an English-only file (gating, the English-dictionary collision, the
//!   lone-ideograph law), a synthetic twin proves the detector itself fires,
//!   so "green" never means "blind".
//! * LOAD-LAW tests feed `TrickLexicon::from_source` documents that BREAK a
//!   law and assert the loss is named on `conflicts()`, never silent.

use std::collections::{HashMap, HashSet};

use aterm_lexicon::tricks::row_line;
use aterm_lexicon::{
    Class, Lexicon, RowKind, ScanOptions, Trick, TrickLexicon, TrickRole, TrickRow, fold,
    is_code_adjacent_punct,
};

// ---------------------------------------------------------------- helpers ----

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

/// Every authored surface of a row with the role a lookup must report for it:
/// the list it sits in, with the lone-ideograph law applied.
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

/// The languages the embedded file holds, in listing (= first-appearance) order.
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

// ------------------------------------------------------------- data: shape ----

#[test]
fn the_embedded_vocabulary_has_no_conflicts_under_any_language_configuration() {
    let langs = languages_in_the_file();
    let mut configs: Vec<Vec<&str>> = vec![vec![], vec!["en"], vec!["all"]];
    for lang in &langs {
        configs.push(vec![lang.as_str()]);
        configs.push(vec!["en", lang.as_str()]);
    }
    for config in configs {
        let lex = TrickLexicon::with_languages(&config);
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
    }
}

#[test]
fn english_is_the_first_language_in_the_file() {
    assert_eq!(
        languages_in_the_file().first().map(String::as_str),
        Some("en"),
        "English is the priority language: it is the first claimant of every \
         shared surface, so its group must open the file"
    );
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
    let mut per_lang: HashMap<String, TrickLexicon> = HashMap::new();
    let mut probed = 0usize;
    for row in &rows {
        let own = per_lang
            .entry(row.lang.clone())
            .or_insert_with(|| TrickLexicon::with_languages(&[row.lang.as_str()]));
        for (surface, role) in expected_roles(row) {
            // The authored spelling carries its own marks, so it is the
            // marked witness the marks-required gate demands.
            assert_eq!(
                classify(&all, surface),
                Some(role),
                "{surface:?} ({}) does not classify as its own role under [\"all\"]",
                row.lang
            );
            assert_eq!(
                classify(own, surface),
                Some(role),
                "{surface:?} does not classify as its own role under its own \
                 language [{:?}]",
                row.lang
            );
            probed += 1;
        }
    }
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
            // Keyed on the AUTHORED string, not its fold: `siéntate` beside
            // `sientate` is the marks-required idiom (two strings, one key),
            // but the very same string twice in one language is a slip.
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

    // The synthetic twin: a document that INTERLEAVES languages inside each
    // array still lists language by language, first appearance first, and
    // keeps each array's file order inside a language.
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

// ---------------------------------------------------- data: the word laws ----

/// Executables, shell builtins and everyday openers of a message to a coding
/// assistant: words that START ordinary lines. None of them may be a PLAIN
/// word — a plain word flashes and latches the pet at its trailing space — so
/// each is either absent from the vocabulary, an address word, or a filler.
const LINE_OPENERS: &[&str] = &[
    // Commands and builtins that are English words (coreutils, BSD / macOS
    // userland, the shells, the usual developer toolbox).
    "sleep", "wait", "look", "play", "watch", "say", "yes", "time", "test", "kill", "touch", "find",
    "fetch", "top", "open", "read", "echo", "make", "man", "more", "less", "head", "tail", "cut",
    "paste", "join", "split", "sort", "fold", "nice", "talk", "write", "who", "which", "type",
    "set", "let", "help", "history", "jobs", "exit", "quit", "clear", "reset", "date", "cal",
    "tee", "true", "false", "env", "export", "source", "alias", "eval", "exec", "trap", "shift",
    "return", "break", "continue", "cat", "bat", "fish", "yum", "ruff", "yarn", "go", "git",
    "node", "cargo", "install", "link", "mount", "patch", "diff", "expand", "strings", "strip",
    "size", "file", "last", "leave", "lock", "login", "logout", "screen", "script", "units",
    "factor", "banner", "at", "batch", "vault", "for", "if", "then", "while", "do", "done", "case",
    "select", "until", // Everyday openers of a chat message to a coding assistant.
    "add", "fix", "update", "run", "check", "show", "explain", "remove", "delete", "rename",
    "move", "change", "create", "build", "use", "try", "keep", "hold", "drop", "pick", "give",
    "take", "tell", "see", "turn", "put", "get", "can", "could", "would", "should", "what", "why",
    "how", "when", "where", "is", "are", "the", "this", "that", "it", "i", "we", "you", "please",
    "ok", "okay", "now", "so", "and", "also", "but", "no", "nope", "good", "great", "perfect",
    "awesome", "thanks", "thank", "hey", "hi", "hello", "stop", "stay", "come", "roll", "jump",
    "hide", "treat", "stretch", "sit", "clean", "tidy", "walk", "spin", "flip", "catch", "zoom",
    "rest", "relax", "chill", "calm", "lay", "freeze", "focus", "listen", "here", "love", "bad",
    "off", "enough", "quiet", "surprise", "spring", "feed", "eat", "up", "down", "over", "away",
    "back", "gotcha", "yikes", "wow", "hmm", "oops",
];

/// The line openers that are PLAIN anyway, each with the reason the misfire
/// is accepted. The listener's tentative tier is what pays for them: an
/// unaddressed fire is revoked the moment the line turns into prose.
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
    // Two-sided: an exception that is no longer an opener, or no longer a
    // plain word, is a stale sentence in this file.
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

/// THE `sleep 5` RULING. `sleep` is a shell command people type by hand, and
/// it is PLAIN anyway: a direct request is answered every time (the owner's
/// law), and the listener's tentative tier makes the collision cheap — `sleep
/// 5` blinks the word and is revoked at `5`; the pet never moves. The same
/// holds for every iconic bare verb. Pinned by name because "make `sleep` an
/// address word" is the first thing a future editor will want to do, and it
/// turns the feature's most obvious command into one that silently fails.
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
    // The bare verbs that open everyday assistant requests need the pet named.
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

/// A surface typed at a real shell is often SUBMITTED. An apostrophe opens a
/// `quote>` continuation there, and bare `zzz` is a real suspend command on
/// FreeBSD / OpenBSD / Void — so neither may be vocabulary, in any language.
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

/// THE ELONGATION FOLD: a held key is still the word. On a miss only, runs
/// of three or more identical characters collapse to two, then to one.
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
    // Two first, then one: `goood` is `good`, never `god`-shaped guessing; a
    // doubled letter is spelling and is never touched, so a TYPO stays a miss.
    for typed in [
        "sitt", "sitting", "sleepy", "jumpp", "kitt", "zzz", "zzzzzz", "qqq", "sss", "siiite",
        "pllaaay",
    ] {
        assert_eq!(classify(en, typed), None, "{typed:?}");
    }
    // A token longer than the fold's window is not a drawn-out word; it must
    // miss without panicking (the window walk is bounds-checked).
    let long = format!("s{}t", "i".repeat(200));
    assert_eq!(classify(en, &long), None);
    // No-space scripts never take the fold: their keys are raw whole runs.
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
    // The one-token compounds carry their own vocative, so they are plain.
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

/// LAW 3 of the data file: a surface is one token, so a natural multi-word
/// phrase lands only if EVERY word of it is vocabulary (the listener drops a
/// line's purity at the first word that is not). These are the phrases the
/// English seed was shaped around.
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
    // ... and the words that turn a command into a sentence are NOT
    // vocabulary, so these lines lose their purity and a tentative fire is
    // revoked: `stretch the image`, `sit tight`, `roll back`, `look at`,
    // `play with`, `jump to line`, `npm run build`, `git fetch`.
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
    // The empty vocabulary a host holds before its compile lands.
    let empty = TrickLexicon::default();
    assert_eq!(empty.surface_count(), 0);
    assert_eq!(classify(&empty, "sit"), None);
    assert!(empty.conflicts().is_empty());
}

// ------------------------------------------- data: the cross-table laws ----

/// The ONLY trick surfaces allowed to be shared sparkle-lexicon surfaces: the
/// cat's own voice, and only as feline surfaces under `speak`.
const SHARED_VOICE: &[&str] = &["meow", "mew", "miaow"];

/// How a surface classifies under the all-languages SHARED sparkle lexicon,
/// by either route the screen scanner has: the whole-token lookup (spaced
/// scripts, with its possessive / clitic fallbacks) or a scan of the surface
/// with every opt-in on (which is how a no-space compound, or a lone
/// ideograph, would be found on screen).
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

/// ONE WORD, ONE EMITTER. A typed trick word flashes rainbow where it was
/// typed; a shared-lexicon surface is ALSO decorated by the screen scanner
/// when the shell echoes it. So no trick / address / filler surface may be a
/// shared surface of ANY class in ANY language (`bat`, `duck`, `fish`, `woof`,
/// `miau` are out), except the cat's own voice. Vocatives are exempt: they
/// are the feline / canine words on purpose and never flash.
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
    // The excuse list must not rot: each voice is really a speak word.
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
        // Vocatives included: they may be feline / canine, never this.
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

/// English dictionary words that an UNGATED non-English row may still claim as
/// a PLAIN word, each with a reason. "English dictionary" here is Webster's
/// Second (the system word list): every entry below is a headword nobody
/// types at the start of a line, and most are THE command of their language,
/// which a gate would switch off for every default user. A word English
/// speakers really do type (`main`, `gel`, `vino`, `pied`) is never excused —
/// it rides a gated sibling row instead.
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

/// The words of `dictionary` that the DEFAULT configuration (`["en"]`: every
/// ungated row of every language, no gated row) classifies although no
/// English row authored them — i.e. an ungated foreign surface that is also
/// an English word. Judged through `classify`, so a marks-required surface
/// (fr `couché`) is correctly NOT charged with its bare skeleton (`couche`).
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
            // Only a PLAIN word is charged: it is the one role that fires by
            // itself, at the first space of an ordinary English line. A foreign
            // address word still needs the pet named, and a foreign vocative or
            // filler fires nothing at all — gating THOSE would only switch
            // `bravo`, `la` and `gato` off for the people who mean them.
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

/// LAW 7. Indonesian `main` (play), Turkish `gel` (come), German `spring`
/// (jump) are English words too, and as ungated PLAIN words they would blink
/// at the first space of every English line that opens with them. The remedy
/// is the sparkle lexicon's: `gated = true` — for the words English speakers
/// really type. The system word list is Webster's Second, though, a quarter
/// of a million headwords deep: `assis`, `salta`, `sov` are in it and no one
/// alive opens a line with them, while gating them would switch THE sit
/// command of French and Swedish off for every default user. Those ride
/// [`DICTIONARY_EXCUSED`], each with its reason; the gate stays for the rest.
#[test]
fn no_ungated_non_english_surface_is_an_english_dictionary_word() {
    let Ok(raw) = std::fs::read_to_string("/usr/share/dict/words") else {
        eprintln!("no system word list; the tricks dictionary gate is skipped");
        return;
    };
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

/// The synthetic twin of the gate above, with its own three-word dictionary,
/// so the detector is proven to FIRE whatever the embedded file holds.
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

// ------------------------------------------------------- data: coverage ----

/// The roster the multilingual data is expected to grow into. NOT asserted
/// present — this test runs over the languages the file actually holds — only
/// reported, so the run says honestly how far the data has come.
const ROSTER: &[&str] = &[
    "en", "es", "fr", "de", "it", "pt", "nl", "sv", "pl", "ru", "uk", "tr", "ar", "he", "fa", "hi",
    "bn", "id", "vi", "th", "zh", "ja", "ko",
];

/// `(lang, trick code, why)` — a trick a language honestly cannot reach with
/// one natural typed token. Two-sided: a listed gap that IS reachable fails,
/// so the table can only describe reality.
const CORE_GAPS: &[(&str, &str, &str)] = &[];

/// Whether `lang` reaches `trick` when the user has listed that language:
/// some authored surface of a `(trick, lang)` row classifies back to it.
fn reaches(rows: &[TrickRow], lex: &TrickLexicon, lang: &str, trick: Trick) -> bool {
    rows.iter()
        .filter(|row| row.lang == lang && row.kind == RowKind::Trick(trick))
        .flat_map(|row| row.words.iter().chain(row.address.iter()))
        .any(|s| matches!(classify(lex, s), Some(TrickRole::Trick { trick: t, .. }) if t == trick))
}

/// EVERY trick, in EVERY language the file holds — not a core four. A trick
/// added to the enum and authored in English only is NOT a multilingual
/// vocabulary with one gap in it: the word a French or Korean speaker types
/// for that trick is almost always still sitting in some OTHER trick's row,
/// so their request classifies, fires, and is answered with the wrong
/// performance while every test stays green. `sing` shipped exactly that way
/// — it was a `speak` surface in 44 languages, and `speak` deals ONE note —
/// and a four-trick pin could not see it. Widening this is the durable half
/// of that fix: the data half alone would decay at the next new trick.
#[test]
fn every_language_in_the_file_reaches_every_trick() {
    let rows = TrickLexicon::all_rows();
    let langs = languages_in_the_file();
    assert!(langs.iter().any(|l| l == "en"), "the file lost English");
    for lang in &langs {
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
    }
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
fn english_reaches_all_seventeen_tricks() {
    let rows = TrickLexicon::all_rows();
    // Under the DEFAULT configuration and under one that does not even list
    // English: English is never gated.
    for config in [&["en"][..], &[][..], &["ja"][..]] {
        let lex = TrickLexicon::with_languages(config);
        for trick in Trick::ALL {
            assert!(
                reaches(&rows, &lex, "en", trick),
                "{config:?}: English does not reach {trick:?}"
            );
        }
    }
    assert_eq!(Trick::ALL.len(), 17);
}

// -------------------------------------------------- the enum and listing ----

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
    assert_eq!(Trick::ALL[0].code(), "sit");
    assert_eq!(Trick::ALL[16].code(), "treat");
    // Ids are data keys, not typed words: nothing is folded or trimmed.
    for not_a_code in ["", "Sit", "SIT", " sit", "sit ", "sitting", "meow", "kitty"] {
        assert_eq!(Trick::from_code(not_a_code), None, "{not_a_code:?}");
    }
}

#[test]
fn blurbs_are_one_clean_listing_field() {
    for trick in Trick::ALL {
        let blurb = trick.blurb();
        assert!(!blurb.is_empty(), "{trick:?} has no blurb");
        assert!(
            blurb
                .chars()
                .all(|c| c.is_ascii_lowercase() || c == ' ' || c == '-'),
            "{trick:?}: {blurb:?} must be lowercase words and single spaces — \
             row_line spells spaces as `_`, so `_`, `=` or a newline would \
             corrupt the field"
        );
        assert!(
            !blurb.starts_with(' ') && !blurb.ends_with(' ') && !blurb.contains("  "),
            "{trick:?}: {blurb:?} has stray spaces"
        );
        assert!(blurb.len() <= 48, "{trick:?}: a blurb is one short clause");
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
    // An empty list is `-`, so every field always has a value.
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
    // Original spellings, not folded keys, and the row's own language.
    let ja = TrickRow {
        kind: RowKind::Trick(Trick::Sit),
        lang: "ja".to_string(),
        words: vec!["おすわり".to_string(), "お座り".to_string()],
        address: vec![],
        gated: false,
        cjk: true,
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

// ---------------------------------------------------------- the load laws ----

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
    // The listing skips the row too (it has no Trick to show it under).
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
    // First claimant wins: file order, tricks before vocatives before fillers.
    assert_eq!(classify(&lex, "down"), plain(Trick::Sit));
    assert_eq!(classify(&lex, "sit"), plain(Trick::Sit));
    assert_eq!(classify(&lex, "loaf"), plain(Trick::Down));
    assert_eq!(classify(&lex, "kitty"), Some(TrickRole::Vocative));
    assert_eq!(classify(&lex, "please"), Some(TrickRole::Filler));
    // ... and every loss is named: down (other trick), sit (plain vs
    // address IS another role), loaf (trick vs vocative), kitty (vocative vs
    // filler).
    assert_eq!(lex.conflicts().len(), 4, "{:#?}", lex.conflicts());
    for word in ["\"down\"", "\"sit\"", "\"loaf\"", "\"kitty\""] {
        let c = conflict_naming(&lex, word).unwrap_or_else(|| panic!("no conflict names {word}"));
        assert!(c.contains("kept the first"), "{c}");
    }
}

/// Claims are resolved in LISTING order — language groups in first-appearance
/// order — so the language that opens the file wins a cross-language claim
/// with ALL of its rows, wherever they sit in the arrays. That is why the
/// embedded file must open with English (pinned above).
#[test]
fn a_cross_language_claim_is_won_by_the_language_that_opens_the_file() {
    // German opens this document, so German's plain `spring` beats English's
    // address `spring`.
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

    // English opens this one with an unrelated row. Its `spring` row still
    // comes AFTER the German row in the array — and wins.
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
    // Four named losses; the empty and blank surfaces are trimmed away
    // quietly (they are whitespace, not a word someone meant).
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
    // WHOLE-RUN EQUALITY, no maximal munch: a longer run that merely contains
    // (or starts with) a surface is a different word.
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
    // Authored PLAIN, reported as needing the vocative.
    assert_eq!(classify(&lex, "坐"), address(Trick::Sit));
    assert_eq!(classify(&lex, "跳"), address(Trick::Jump));
    // A compound keeps the list it was authored in.
    assert_eq!(classify(&lex, "坐下"), plain(Trick::Sit));
    assert_eq!(classify(&lex, "坐好"), address(Trick::Sit));
    // The law is about TRICK words; a one-character vocative is a vocative.
    assert_eq!(classify(&lex, "猫"), Some(TrickRole::Vocative));
    // zh lists 跳 as plain and ja as address: once the lone-ideograph law has
    // spoken those are the SAME role, so it is a merge, not a conflict.
    assert!(lex.conflicts().is_empty(), "{:?}", lex.conflicts());
}

/// The data-driven twin: whatever no-space rows the embedded file holds, a
/// one-character trick surface never reports `needs_vocative = false`.
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
    // fr `couché` folds to the bare-ASCII key `couche` — which is another
    // French word (a layer, a nappy). Only MARKED typing reaches it.
    assert_eq!(classify(&lex, "couché"), plain(Trick::Down));
    assert_eq!(classify(&lex, "COUCHÉ"), plain(Trick::Down));
    assert_eq!(classify(&lex, "couche\u{0301}"), plain(Trick::Down));
    assert_eq!(classify(&lex, "couche"), None);
    assert_eq!(classify(&lex, "COUCHE"), None);
    // es lists both spellings, so the bare one is consent (the two claims
    // merge and AND the gate away).
    assert_eq!(classify(&lex, "siéntate"), plain(Trick::Sit));
    assert_eq!(classify(&lex, "sientate"), plain(Trick::Sit));
    // A key that stays non-ASCII was never gated: matching text had to carry
    // those letters anyway.
    assert_eq!(classify(&lex, "спать"), plain(Trick::Sleep));
    assert_eq!(classify(&lex, "СПАТЬ"), plain(Trick::Sleep));
    // vi `nhảy` folds its hook away into bare `nhay`: gated like `couché`.
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
        // The gated English row loaded anyway — and the file is told off.
        assert_eq!(classify(&lex, "sit"), plain(Trick::Sit), "{config:?}");
        assert_eq!(lex.conflicts().len(), 1, "{:?}", lex.conflicts());
        assert!(conflict_naming(&lex, "English is never gated").is_some());
    }
}

/// The data-driven twin of the gate test (vacuous for an English-only file):
/// under the default configuration a gated row's surfaces are silent unless
/// an ungated row claims the same key.
#[test]
fn gated_rows_of_the_embedded_file_are_silent_by_default() {
    let rows = TrickLexicon::all_rows();
    let default_config = TrickLexicon::with_languages(&["en"]);
    // Keys an UNGATED row compiles: the raw spelling (no-space rows) and the
    // fold (spaced rows). A gated surface sharing one of them loads through
    // that other row, which is not a leak.
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

    // `adress` must not become a list that silently never loads.
    let typo = r#"
        [[trick]]
        id = "sit"
        lang = "en"
        words = ["sit"]
        adress = ["stay"]
    "#;
    assert!(TrickLexicon::from_source(typo, &["en"]).is_err());
    assert!(TrickLexicon::rows_from_source(typo).is_err());
    // ... and neither must a missing `lang`, nor a document that is not TOML.
    assert!(TrickLexicon::from_source("[[trick]]\nid = \"sit\"\n", &["en"]).is_err());
    assert!(TrickLexicon::from_source("[[trick", &["en"]).is_err());
    // An empty document is a well-formed empty vocabulary.
    let empty = compile("", &["all"]);
    assert_eq!(empty.surface_count(), 0);
    assert!(empty.conflicts().is_empty());
}

// ------------------------------------------- the shared code-context set ----

/// `is_code_adjacent_punct` is exported so the typed-line listener draws the
/// code / prose line exactly where the screen scanner draws it. This pins the
/// export AGAINST THE SCANNER'S BEHAVIOUR, not against a restated list: for
/// every ASCII punctuation character, the scanner refuses `<c>kitty` exactly
/// when the exported predicate (or the scanner's one extra left rule, a
/// leading `-` flag) says so.
#[test]
fn the_exported_code_context_set_is_the_one_the_scanner_uses() {
    let shared = Lexicon::builtin();
    let opts = ScanOptions::default();
    let mut code_like = 0;
    for c in (0x21u8..=0x7e).map(char::from) {
        // Token characters and `_` would join the word instead of sitting
        // beside it.
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
