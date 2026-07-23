// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Positive / negative corpus and the lexicon validator.
//!
//! These are the correctness contract for the matcher: every listed surface form
//! must classify as itself, every Scunthorpe / code-context decoy must NOT match,
//! and CJK incidental compounds must be suppressed.

use aterm_lexicon::{Class, Lexicon, ScanOptions, primary_lang};

fn lex() -> Lexicon {
    Lexicon::with_languages(&["all"])
}

/// All matched surfaces in `text` with their class.
fn hits(text: &str, opts: &ScanOptions) -> Vec<(String, Class)> {
    let lx = lex();
    let chars: Vec<char> = text.chars().collect();
    lx.scan(text, opts)
        .into_iter()
        .map(|m| (chars[m.start..m.end].iter().collect::<String>(), m.class))
        .collect()
}

fn classes(text: &str, opts: &ScanOptions) -> Vec<Class> {
    hits(text, opts).into_iter().map(|(_, c)| c).collect()
}

fn default_opts() -> ScanOptions<'static> {
    ScanOptions::default()
}

fn opts_allow_cat() -> ScanOptions<'static> {
    ScanOptions {
        allow_bare_cat: true,
        ..ScanOptions::default()
    }
}

// ------------------------------------------------------------------ positives

#[test]
fn english_profanity_forms() {
    let o = default_opts();
    for w in [
        "fuck",
        "fucking",
        "fucked",
        "fucks",
        "fucker",
        "fuckers",
        "fuckin",
        "motherfucker",
        "clusterfuck",
        "unfuck",
    ] {
        assert_eq!(
            classes(w, &o),
            vec![Class::Profanity],
            "expected profanity for {w:?}"
        );
    }
}

#[test]
fn english_feline_forms() {
    let o = default_opts();
    // Longer feline forms are eligible by default.
    for w in ["cats", "kitty", "kitties", "kitten", "kittens", "pussycat"] {
        assert_eq!(
            classes(w, &o),
            vec![Class::Feline],
            "expected feline for {w:?}"
        );
    }
}

#[test]
fn bare_cat_is_opt_in() {
    assert_eq!(classes("cat", &default_opts()), Vec::<Class>::new());
    assert_eq!(classes("cat", &opts_allow_cat()), vec![Class::Feline]);
    // even with bare cat off, "cats" still matches
    assert_eq!(classes("cats", &default_opts()), vec![Class::Feline]);
}

#[test]
fn possessive_and_punctuation() {
    let o = opts_allow_cat();
    assert_eq!(classes("the cat's toy", &o), vec![Class::Feline]);
    assert_eq!(classes("kitty's", &default_opts()), vec![Class::Feline]);
    assert_eq!(classes("oh fuck!", &default_opts()), vec![Class::Profanity]);
    assert_eq!(
        classes("(fucking)", &default_opts()),
        vec![Class::Profanity]
    );
}

#[test]
fn multilingual_feline() {
    let o = default_opts();
    // Romance / Germanic / others — longer than 3 chars so eligible by default.
    for w in [
        "gato",
        "gatos",
        "gatito", // es
        "gatto",
        "gattino", // it
        "chaton",  // fr
        "katze",
        "kätzchen", // de
        "katt",
        "kattunge", // sv
        "kissa",
        "kissanpentu", // fi
        "kucing",      // id
    ] {
        let c = classes(w, &o);
        assert_eq!(c, vec![Class::Feline], "expected feline for {w:?}");
    }
}

#[test]
fn multilingual_profanity() {
    let o = default_opts();
    for w in [
        "merde",    // fr
        "scheiße",  // de
        "scheisse", // de (ss surface)
        "mierda",   // es
        "cazzo",    // it
        "kurwa",    // pl
        "vittu",    // fi
    ] {
        assert_eq!(
            classes(w, &o),
            vec![Class::Profanity],
            "expected profanity for {w:?}"
        );
    }
}

#[test]
fn german_eszett_both_surfaces() {
    let o = default_opts();
    assert_eq!(classes("Scheiße", &o), vec![Class::Profanity]);
    assert_eq!(classes("SCHEISSE", &o), vec![Class::Profanity]);
}

#[test]
fn decomposed_diacritic_matches() {
    let o = default_opts();
    // "kätzchen" written decomposed (a + combining diaeresis) must still match.
    let decomposed = "ka\u{0308}tzchen";
    assert_eq!(classes(decomposed, &o), vec![Class::Feline]);
}

// ------------------------------------------------------------------ negatives

#[test]
fn scunthorpe_problem() {
    let o = opts_allow_cat(); // even with bare cat ON, none of these are whole-word cat
    for decoy in [
        "scatter",
        "concatenate",
        "category",
        "cater",
        "scarface",
        "Scunthorpe",
        "classic",
        "communication",
        "scat",
        "wildcat", // not a listed form; "cat" is a substring only
        "bobcat",
        "located",
    ] {
        assert_eq!(
            classes(decoy, &o),
            Vec::<Class>::new(),
            "false positive on {decoy:?}"
        );
    }
}

#[test]
fn code_and_path_context_suppressed() {
    let o = opts_allow_cat();
    for code in [
        "cat.txt",
        "/api/cat/list",
        "format.cat",
        "cat_food",
        "my_cat",
        "concat_str",
        "github.com/cat",
        "https://example.com/cats",
        "--cat",
        "cat=1",
        "$cat",
        "cat5",
    ] {
        assert_eq!(
            classes(code, &o),
            Vec::<Class>::new(),
            "false positive in code context {code:?}"
        );
    }
}

#[test]
fn cat_command_not_decorated_by_default() {
    // The literal `cat` shell command is a bare 3-letter token → opt-in only.
    assert_eq!(
        classes("cat file.txt | grep foo", &default_opts()),
        Vec::<Class>::new()
    );
}

#[test]
fn prose_cat_decorated_when_enabled() {
    assert_eq!(
        classes("I love my cat", &opts_allow_cat()),
        vec![Class::Feline]
    );
}

// ------------------------------------------------------------------ CJK

#[test]
fn cjk_feline_compounds_match() {
    let o = default_opts();
    // Whole compounds (length > 1) match without the single-char opt-in.
    assert_eq!(classes("子猫", &o), vec![Class::Feline]); // kitten (ja)
    assert_eq!(classes("ねこがすき", &o), vec![Class::Feline]); // ねこ within a run
    assert_eq!(classes("고양이", &o), vec![Class::Feline]); // cat (ko)
}

#[test]
fn cjk_single_char_is_opt_in() {
    let off = default_opts();
    let on = ScanOptions {
        cjk_single_char: true,
        ..ScanOptions::default()
    };
    assert_eq!(classes("猫", &off), Vec::<Class>::new());
    assert_eq!(classes("猫", &on), vec![Class::Feline]);
}

#[test]
fn cjk_incidental_compounds_suppressed() {
    // Even with single-char matching ON, exception compounds must not fire.
    let on = ScanOptions {
        cjk_single_char: true,
        ..ScanOptions::default()
    };
    for decoy in ["熊猫", "猫車", "猫背"] {
        assert_eq!(
            classes(decoy, &on),
            Vec::<Class>::new(),
            "exception compound matched: {decoy:?}"
        );
    }
}

// ------------------------------------------------------------------ validator

#[test]
fn every_surface_round_trips() {
    let lx = lex();
    let o = ScanOptions {
        allow_bare_cat: true,
        cjk_single_char: true,
        ..ScanOptions::default()
    };
    // Spaced surfaces: each folded form, scanned alone, classifies as itself.
    for (surface, class) in lx.iter_spaced() {
        let got = lx.scan(surface, &o);
        assert!(
            got.iter().any(|m| m.class == class),
            "surface {surface:?} ({class:?}) did not round-trip; got {got:?}"
        );
    }
    // CJK surfaces likewise (unless it is itself an exception substring — none are).
    for (surface, class) in lx.iter_cjk() {
        let got = lx.scan(surface, &o);
        assert!(
            got.iter().any(|m| m.class == class),
            "cjk surface {surface:?} ({class:?}) did not round-trip; got {got:?}"
        );
    }
}

#[test]
fn cross_class_homograph_resolves_to_profanity() {
    // "poes" is feline in Dutch (nl) and profanity in Afrikaans (af); the
    // builder's "profanity wins" rule must make the merged surface profanity.
    assert_eq!(classes("poes", &default_opts()), vec![Class::Profanity]);
}

#[test]
fn arabic_hebrew_clitics_are_stripped() {
    let lx = lex();
    // Arabic definite article ال + single-letter proclitic و before قطة (cat).
    assert_eq!(lx.classify_token("القطة"), Some(Class::Feline));
    assert_eq!(lx.classify_token("وقطة"), Some(Class::Feline));
    // Hebrew proclitic ה before חתול (cat).
    assert_eq!(lx.classify_token("החתול"), Some(Class::Feline));
}

#[test]
fn ignore_set_suppresses_matches() {
    use std::collections::HashSet;
    let lx = lex();
    let ignore: HashSet<String> = ["fucking".to_string(), "kitty".to_string()]
        .into_iter()
        .collect();
    let opts = ScanOptions {
        ignore: Some(&ignore),
        ..ScanOptions::default()
    };
    assert!(
        lx.scan("fucking", &opts).is_empty(),
        "ignored word must not match"
    );
    assert!(lx.scan("kitty", &opts).is_empty());
    // A non-ignored sibling still matches.
    assert_eq!(lx.scan("fucked", &opts).len(), 1);
}

#[test]
fn user_override_extends_coverage() {
    // A user entry adds a new feline surface ("floof") without disturbing builtins.
    let extra =
        "[[entry]]\nclass=\"feline\"\nlang=\"en\"\nmode=\"forms\"\nforms=[\"floof\",\"floofs\"]\n";
    let lx = Lexicon::with_languages_and_override(&["all"], Some(extra)).expect("override parses");
    assert!(lx.conflicts().is_empty());
    let o = ScanOptions::default();
    let m = lx.scan("look a floof", &o);
    assert_eq!(m.len(), 1, "override surface must match");
    assert_eq!(m[0].class, Class::Feline);
    // Builtins still present.
    assert_eq!(lx.scan("fucking", &o).len(), 1);
}

/// v3 §6 (FIX III): USER surfaces that can never scan as written are no
/// longer silently accepted — a single-char CJK form (dropped at scan unless
/// `cjk_single_char = true`) and a mixed-script form (dropped at insert) both
/// surface on the `conflicts` channel. Builtin data stays conflict-free (the
/// embedded single-char 操/草/干 and multi-word "con mèo" entries are
/// deliberate and quiet) — pinned by `no_class_conflicts_in_embedded_data`.
#[test]
fn user_unscannable_surfaces_surface_as_conflicts() {
    let extra = concat!(
        "[[entry]]\nclass=\"emphasis\"\nlang=\"en\"\nmode=\"forms\"\ncjk=true\nforms=[\"犬\"]\n",
        "[[entry]]\nclass=\"emphasis\"\nlang=\"en\"\nmode=\"forms\"\nforms=[\"abc猫\"]\n",
    );
    let lx = Lexicon::with_languages_and_override(&["en"], Some(extra)).expect("override parses");
    assert!(
        lx.conflicts()
            .iter()
            .any(|c| c.contains("\"犬\"") && c.contains("requires cjk_single_char = true")),
        "single-char CJK user surface warns, got {:?}",
        lx.conflicts()
    );
    assert!(
        lx.conflicts()
            .iter()
            .any(|c| c.contains("\"abc猫\"") && c.contains("dropped")),
        "mixed-script user surface warns dropped, got {:?}",
        lx.conflicts()
    );
    // The single-char form still scans under the opt-in (it was inserted,
    // only warned about); the mixed-script one never does.
    let single = ScanOptions {
        cjk_single_char: true,
        ..ScanOptions::default()
    };
    assert_eq!(lx.scan("これは犬です", &single).len(), 1);
    assert!(lx.scan("これは犬です", &ScanOptions::default()).is_empty());
    assert!(lx.scan("go abc猫 now", &ScanOptions::default()).is_empty());
}

#[test]
fn fold_idempotent_for_turkish_dotted_i() {
    // 'İ'.to_lowercase() emits 'i' + combining dot; fold must drop it and stay idempotent.
    assert_eq!(aterm_lexicon::fold("İ"), aterm_lexicon::fold("i"));
    assert_eq!(
        aterm_lexicon::fold(&aterm_lexicon::fold("İ")),
        aterm_lexicon::fold("İ")
    );
}

#[test]
fn no_class_conflicts_in_embedded_data() {
    let lx = lex();
    assert!(
        lx.conflicts().is_empty(),
        "data conflicts: {:#?}",
        lx.conflicts()
    );
}

// ===================== emphasis class (sparkle-words v2 P0) =====================

/// An emphasis override entry for the neutral coined word "megathink" — the
/// shape `[sparkle_words.emphasis].extra_words` compiles into. The builtin
/// ships no emphasis forms, so an override is the only way this class matches.
fn lex_emphasis_megathink() -> Lexicon {
    let over = r#"
[[entry]]
class    = "emphasis"
lang     = "en"
mode     = "forms"
stems    = []
suffixes = []
forms    = ["megathink"]
cjk      = false
ambiguous= false
"#;
    Lexicon::with_languages_and_override(&["all"], Some(over)).expect("override parses")
}

#[test]
fn emphasis_positive_forms() {
    let lx = lex_emphasis_megathink();
    let o = default_opts();
    let m = lx.scan("ship it with megathink now", &o);
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].class, Class::Emphasis);
    let m = lx.scan("MEGATHINK", &o);
    assert_eq!(m.len(), 1, "matching is case-folded");
    assert_eq!(m[0].class, Class::Emphasis);
}

#[test]
fn emphasis_negatives() {
    let lx = lex_emphasis_megathink();
    let o = default_opts();
    assert!(lx.scan("mega think", &o).is_empty());
    assert!(
        lx.scan("megathinker megathinks", &o).is_empty(),
        "forms-only class: derived tokens must not match"
    );
    assert!(
        lx.scan("--megathink=1", &o).is_empty(),
        "code/flag context suppressed"
    );
}

/// PIN (2026-07-02 decision): the builtin lexicon ships ZERO emphasis forms —
/// the earlier hype-oriented seeds are removed, and the emphasis
/// class is populated solely via `[sparkle_words.emphasis].extra_words` (a
/// user override entry). Guards against the seeds silently returning.
#[test]
fn builtin_ships_no_emphasis_forms() {
    let lx = lex();
    assert_eq!(
        lx.iter_spaced()
            .filter(|(_, c)| *c == Class::Emphasis)
            .count(),
        0,
        "no spaced emphasis surfaces in the builtin"
    );
    assert_eq!(
        lx.iter_cjk().filter(|(_, c)| *c == Class::Emphasis).count(),
        0,
        "no CJK emphasis surfaces in the builtin"
    );
    assert!(
        lx.scan(concat!("ultra", "code ultrathink"), &default_opts())
            .is_empty(),
        "the removed seeds must not match the builtin"
    );
}

#[test]
fn emphasis_short_forms_match_only_with_user_consent() {
    // v3 §6 FLIP of the former `emphasis_short_forms_never_match` pin: a
    // USER-supplied (override) short emphasis surface now matches — explicit
    // config is consent. Non-user short surfaces stay suppressed (see
    // `non_user_short_emphasis_surface_stays_suppressed`).
    let over = r#"
[[entry]]
class    = "emphasis"
lang     = "en"
mode     = "forms"
stems    = []
suffixes = []
forms    = ["wow"]
cjk      = false
ambiguous= false
"#;
    let lx = Lexicon::with_languages_and_override(&["all"], Some(over)).expect("override parses");
    assert_eq!(
        lx.scan("wow", &default_opts()).len(),
        1,
        "user-supplied <= 3-folded-char emphasis surfaces match (v3 §6 consent rule)"
    );
}

#[test]
fn class_precedence_total_order() {
    // profanity > feline > orca > emphasis, independent of insertion order.
    let over = r#"
[[entry]]
class    = "emphasis"
lang     = "en"
mode     = "forms"
stems    = []
suffixes = []
forms    = ["kitty", "fucking", "megasplash"]
cjk      = false
ambiguous= false

[[entry]]
class    = "orca"
lang     = "en"
mode     = "forms"
stems    = []
suffixes = []
forms    = ["megasplash"]
cjk      = false
ambiguous= false
"#;
    let lx = Lexicon::with_languages_and_override(&["all"], Some(over)).expect("override parses");
    let o = default_opts();
    let one = |t: &str| {
        let ms = lx.scan(t, &o);
        assert_eq!(ms.len(), 1, "{t}");
        ms[0].class
    };
    assert_eq!(
        one("kitty"),
        Class::Feline,
        "builtin feline outranks user emphasis"
    );
    assert_eq!(
        one("fucking"),
        Class::Profanity,
        "profanity outranks everything"
    );
    assert_eq!(one("megasplash"), Class::Orca, "orca outranks emphasis");
}

// =============== Match.form_id / form_hash (sparkle identity P0) ================

#[test]
fn form_hash_populated_and_position_independent() {
    let lx = lex();
    let o = opts_allow_cat();
    let ms = lx.scan("cat and CAT and cats", &o);
    assert_eq!(ms.len(), 3);
    assert_ne!(ms[0].form_hash, 0);
    assert_eq!(
        ms[0].form_id, ms[1].form_id,
        "form_id is the collision-free exact folded key: case/position independent"
    );
    assert_eq!(
        ms[0].form_hash, ms[1].form_hash,
        "form_hash is over the FOLDED key: case- and position-independent"
    );
    assert_ne!(
        ms[0].form_hash, ms[2].form_hash,
        "different surface, different hash"
    );
    assert_ne!(
        ms[0].form_id, ms[2].form_id,
        "different exact surfaces always receive different IDs"
    );
}

#[test]
fn form_id_distinguishes_supported_morphology_from_its_stem() {
    let lx = lex();
    let ms = lx.scan("kitty kitty's kitty’s", &default_opts());
    assert_eq!(ms.len(), 3);
    assert_ne!(ms[0].form_id, ms[1].form_id, "stem versus possessive");
    assert_ne!(
        ms[1].form_id, ms[2].form_id,
        "ASCII and curly possessives are exact recognized surfaces"
    );
    assert_ne!(ms[0].form_hash, ms[1].form_hash);
    assert_ne!(ms[1].form_hash, ms[2].form_hash);
}

// ===================== language attribution (sparkle-words v2.2 F4.5) =====================

/// Lang codes of the single match in `text`, in [`aterm_lexicon::LangSet`]
/// iteration order (ascending id = lexicon first-appearance order, so the
/// first code is the primary language).
fn langs_of(lx: &Lexicon, text: &str, opts: &ScanOptions) -> Vec<String> {
    let ms = lx.scan(text, opts);
    assert_eq!(ms.len(), 1, "expected one match in {text:?}, got {ms:?}");
    ms[0]
        .langs
        .iter()
        .map(|id| lx.lang_code(id).to_string())
        .collect()
}

/// The same-class union policy: `kucing` is listed by the id, ms AND jv
/// entries, so one match honestly claims all three, with the primary being the
/// first TOML appearance (id).
#[test]
fn kucing_langs_union_id_ms_jv() {
    let lx = lex();
    let o = default_opts();
    assert_eq!(langs_of(&lx, "kucing", &o), vec!["id", "ms", "jv"]);
    let m = lx.scan("kucing", &o)[0];
    assert_eq!(m.class, Class::Feline);
    assert_eq!(lx.lang_code(primary_lang(m.langs)), "id");
}

/// `ambiguous` gating still keys on the RAW entry lang (semantics unchanged):
/// under the default `["en"]` the ambiguous id entry stays gated out, so
/// `kucing` still matches via the un-gated ms/jv entries — and its language
/// set honestly reflects only the loaded claimants.
#[test]
fn kucing_under_default_languages_is_ms_jv() {
    let lx = Lexicon::with_languages(&["en"]);
    let o = default_opts();
    assert_eq!(langs_of(&lx, "kucing", &o), vec!["ms", "jv"]);
    let m = lx.scan("kucing", &o)[0];
    assert_eq!(lx.lang_code(primary_lang(m.langs)), "ms");
}

/// The 'meow → Malay' misattribution fix: meow/mew (+ plurals) now live in the
/// en feline entry, and the ms `meow` listing unions in rather than owning the
/// surface. Holds under the default `["en"]` config too (the ms entry is not
/// ambiguous).
#[test]
fn meow_is_english_first_union_malay() {
    let o = default_opts();
    for lx in [lex(), Lexicon::with_languages(&["en"])] {
        assert_eq!(langs_of(&lx, "meow", &o), vec!["en", "ms"]);
        let m = lx.scan("meow", &o)[0];
        assert_eq!(m.class, Class::Feline);
        assert_eq!(lx.lang_code(primary_lang(m.langs)), "en");
        // The en-only plurals, and the 3-folded-char "mew" behind the
        // bare-cat opt-in (like "cat" itself).
        assert_eq!(langs_of(&lx, "meows", &o), vec!["en"]);
        assert_eq!(langs_of(&lx, "mews", &o), vec!["en"]);
        assert!(lx.scan("mew", &o).is_empty(), "3-char feline is opt-in");
        assert_eq!(langs_of(&lx, "mew", &opts_allow_cat()), vec!["en"]);
    }
}

/// The CJK homograph 猫 is listed by BOTH the ja and zh entries: the union
/// carries both, primary = ja (first TOML appearance). Single-language
/// compounds stay single-language.
#[test]
fn cjk_homograph_neko_unions_ja_zh() {
    let lx = lex();
    let on = ScanOptions {
        cjk_single_char: true,
        ..ScanOptions::default()
    };
    assert_eq!(langs_of(&lx, "猫", &on), vec!["ja", "zh"]);
    let m = lx.scan("猫", &on)[0];
    assert_eq!(lx.lang_code(primary_lang(m.langs)), "ja");
    assert_eq!(langs_of(&lx, "子猫", &default_opts()), vec!["ja"]);
    assert_eq!(langs_of(&lx, "猫咪", &default_opts()), vec!["zh"]);
}

/// Primary = lowest interned id = FIRST TOML appearance, pinned on a synthetic
/// two-language lexicon so the ordering claim is independent of builtin data.
#[test]
fn primary_lang_is_first_toml_appearance() {
    let toml = r#"
[[entry]]
class = "feline"
lang  = "aa"
mode  = "forms"
forms = ["zzpurr"]

[[entry]]
class = "feline"
lang  = "bb"
mode  = "forms"
forms = ["zzpurr", "zzmrrp"]
"#;
    let lx = Lexicon::from_sources(toml, "", &["all"]).expect("sources parse");
    let o = default_opts();
    assert_eq!(langs_of(&lx, "zzpurr", &o), vec!["aa", "bb"]);
    let m = lx.scan("zzpurr", &o)[0];
    assert_eq!(lx.lang_code(primary_lang(m.langs)), "aa");
    assert_eq!(langs_of(&lx, "zzmrrp", &o), vec!["bb"]);
}

/// Lang ids are interned BEFORE the ambiguous gating, so a code resolves to
/// the same id whether or not its entries loaded — attribution ids are
/// config-independent, while the gating itself keeps its meaning.
#[test]
fn ambiguous_gating_keeps_semantics_and_ids_stay_config_independent() {
    let toml = r#"
[[entry]]
class = "feline"
lang  = "aa"
mode  = "forms"
forms = ["zzmiaou"]
ambiguous = true

[[entry]]
class = "feline"
lang  = "bb"
mode  = "forms"
forms = ["zzmiaou"]
"#;
    let o = default_opts();
    let gated = Lexicon::from_sources(toml, "", &["en"]).expect("sources parse");
    assert_eq!(langs_of(&gated, "zzmiaou", &o), vec!["bb"]);
    let open = Lexicon::from_sources(toml, "", &["aa"]).expect("sources parse");
    assert_eq!(langs_of(&open, "zzmiaou", &o), vec!["aa", "bb"]);
    // Same numeric ids in both builds: the gated set is a strict subset.
    let g = gated.scan("zzmiaou", &o)[0].langs;
    let u = open.scan("zzmiaou", &o)[0].langs;
    assert_eq!(
        u.union(g),
        u,
        "gated langs must be a subset under equal ids"
    );
    assert!(g.len() == 1 && u.len() == 2);
}

/// Cross-class precedence is unchanged AND the winning class keeps ONLY its
/// own languages: "poes" (nl feline, af profanity) resolves to profanity and
/// must not attribute nl.
#[test]
fn cross_class_winner_keeps_only_its_own_langs() {
    let lx = lex();
    let o = default_opts();
    let m = lx.scan("poes", &o)[0];
    assert_eq!(m.class, Class::Profanity);
    let codes = langs_of(&lx, "poes", &o);
    assert!(codes.contains(&"af".to_string()), "got {codes:?}");
    assert!(!codes.contains(&"nl".to_string()), "got {codes:?}");
}

/// An entry with no `lang` key interns as the reserved "unknown" bucket.
#[test]
fn empty_lang_interns_as_unknown() {
    let toml = "[[entry]]\nclass=\"feline\"\nmode=\"forms\"\nforms=[\"zzfloofy\"]\n";
    let lx = Lexicon::from_sources(toml, "", &["all"]).expect("sources parse");
    assert_eq!(langs_of(&lx, "zzfloofy", &default_opts()), vec!["unknown"]);
}

/// Every builtin surface carries at least one claiming language.
#[test]
fn every_surface_has_langs() {
    let lx = lex();
    let o = ScanOptions {
        allow_bare_cat: true,
        cjk_single_char: true,
        ..ScanOptions::default()
    };
    for (surface, class) in lx.iter_spaced().chain(lx.iter_cjk()) {
        let got = lx.scan(surface, &o);
        assert!(
            got.iter().any(|m| m.class == class && !m.langs.is_empty()),
            "surface {surface:?} ({class:?}) has no langs; got {got:?}"
        );
    }
}

#[test]
fn form_hash_populated_on_cjk_path() {
    let lx = lex();
    let o = default_opts();
    let a = lx.scan("子猫", &o);
    assert_eq!(a.len(), 1);
    assert_ne!(a[0].form_hash, 0);
    let b = lx.scan("これは子猫です", &o);
    assert_eq!(b.len(), 1);
    assert_eq!(
        a[0].form_hash, b[0].form_hash,
        "same compound, same hash, any position"
    );
    assert_eq!(
        a[0].form_id, b[0].form_id,
        "same raw CJK compound keeps one collision-free ID at every position"
    );
}

// --------------------------------------------------- v3 §6 user-surface set

/// v3 §6: user-supplied (override) emphasis surfaces are EXEMPT from the
/// unconditional ≤3-folded-chars guard — explicit config is consent. The
/// 2-char custom word pin.
#[test]
fn user_short_emphasis_surface_matches() {
    let over = "[[entry]]\nclass=\"emphasis\"\nlang=\"en\"\nmode=\"forms\"\nforms=[\"gg\"]\n";
    let lx = Lexicon::with_languages_and_override(&["all"], Some(over)).expect("override parses");
    let m = lx.scan("gg well played", &default_opts());
    assert_eq!(m.len(), 1, "the 2-char USER emphasis word scans");
    assert_eq!(m[0].class, Class::Emphasis);
    assert_eq!(
        m[0].form_hash,
        aterm_lexicon::form_hash("gg"),
        "scan-time hash == the resolver's fold-then-hash key"
    );
}

/// Negative control: the SAME short surface loaded as builtin-shaped data
/// (`from_sources`, no user marking) stays suppressed — the guard itself is
/// unchanged for non-user surfaces.
#[test]
fn non_user_short_emphasis_surface_stays_suppressed() {
    let toml = "[[entry]]\nclass=\"emphasis\"\nlang=\"en\"\nmode=\"forms\"\nforms=[\"gg\"]\n";
    let lx = Lexicon::from_sources(toml, "", &["all"]).expect("sources parse");
    assert!(
        lx.scan("gg well played", &default_opts()).is_empty(),
        "short emphasis surfaces without user consent never fire"
    );
}

/// v3 §6 helpers: `is_no_space_surface` classifies exactly like the scanner's
/// script split, and possessive scans report the FULL-token fold hash (the
/// resolver must insert the four possessive-variant hashes).
#[test]
fn form_hash_helpers_match_scan_semantics() {
    assert!(aterm_lexicon::is_no_space_surface("猫"));
    assert!(aterm_lexicon::is_no_space_surface("ねこちゃん"));
    assert!(!aterm_lexicon::is_no_space_surface("cat"));
    assert!(!aterm_lexicon::is_no_space_surface("猫cat"));
    assert!(!aterm_lexicon::is_no_space_surface(""));

    // Possessive: the match carries fnv(fold(full token)).
    let over =
        "[[entry]]\nclass=\"emphasis\"\nlang=\"en\"\nmode=\"forms\"\nforms=[\"ultrathink\"]\n";
    let lx = Lexicon::with_languages_and_override(&["all"], Some(over)).expect("override parses");
    let m = lx.scan("Ultrathink's power", &default_opts());
    assert_eq!(m.len(), 1);
    assert_eq!(
        m[0].form_hash,
        aterm_lexicon::form_hash("ultrathink's"),
        "possessive hits carry the FULL-token hash"
    );

    // CJK: the match carries fnv(RAW compound).
    let over =
        "[[entry]]\nclass=\"emphasis\"\nlang=\"ja\"\nmode=\"forms\"\ncjk=true\nforms=[\"超考\"]\n";
    let lx = Lexicon::with_languages_and_override(&["all"], Some(over)).expect("override parses");
    let m = lx.scan("これは超考です", &default_opts());
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].form_hash, aterm_lexicon::form_hash("超考"));
}
