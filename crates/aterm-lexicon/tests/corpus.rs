// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Positive / negative corpus and the lexicon validator.

use aterm_lexicon::{Class, Lexicon, ScanOptions, primary_lang};

fn lex() -> Lexicon {
    Lexicon::with_languages(&["all"])
}

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

#[test]
fn listed_surfaces_reach_their_class() {
    let o = default_opts();
    let profanity = [
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
        "merde",    // fr
        "scheiße",  // de
        "scheisse", // de (ss surface)
        "Scheiße",
        "SCHEISSE",
        "SCHEIẞE",
        "mierda", // es
        "cazzo",  // it
        "kurwa",  // pl
        "vittu",  // fi
    ];
    let feline = [
        "cats",
        "kitty",
        "kitties",
        "kitten",
        "kittens",
        "pussycat",
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
    ];
    for (words, class) in [
        (&profanity[..], Class::Profanity),
        (&feline[..], Class::Feline),
    ] {
        for w in words {
            assert_eq!(classes(w, &o), vec![class], "expected {class:?} for {w:?}");
        }
    }
}

#[test]
fn bare_cat_is_opt_in() {
    assert_eq!(classes("cat", &default_opts()), Vec::<Class>::new());
    assert_eq!(classes("cat", &opts_allow_cat()), vec![Class::Feline]);
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
fn decomposed_diacritic_matches() {
    let o = default_opts();
    let decomposed = "ka\u{0308}tzchen";
    assert_eq!(classes(decomposed, &o), vec![Class::Feline]);
}

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
        "cat_food",
        "my_cat",
        "concat_str",
        "cat5",
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
        "github.com/cat",
        "https://example.com/cats",
        "--cat",
        "cat=1",
        "$cat",
    ] {
        assert_eq!(
            classes(code, &o),
            Vec::<Class>::new(),
            "false positive in code context {code:?}"
        );
    }
}

#[test]
fn code_and_path_context_suppressed_in_no_space_scripts_too() {
    let o = opts_allow_cat();
    for code in [
        "子猫.txt",
        "/api/子猫/list",
        "https://example.com/子猫",
        "--子猫",
        "子猫=1",
        "$子猫",
        "/etc/고양이/conf",
        "--고양이",
        "แมว.log",
        "$แมว",
    ] {
        assert_eq!(
            classes(code, &o),
            Vec::<Class>::new(),
            "false positive in a no-space code context {code:?}"
        );
    }
    for prose in ["子猫 が かわいい", "고양이 귀엽다", "แมว น่ารัก"]
    {
        assert!(
            !classes(prose, &o).is_empty(),
            "the no-space arm must still decorate ordinary prose: {prose:?}"
        );
    }
}

#[test]
fn a_marks_required_short_feline_is_not_the_bare_cat_command() {
    let vi = Lexicon::with_languages(&["vi"]);
    let o = default_opts();
    let chars: Vec<char> = "mèo".chars().collect();
    let got: Vec<Class> = vi.scan("mèo", &o).into_iter().map(|m| m.class).collect();
    assert_eq!(got, vec![Class::Feline], "vi lost its only cat word");
    assert_eq!(chars.len(), 3, "fixture: the surface really is three chars");
    assert!(
        vi.scan("meo", &o).is_empty() && vi.scan("meo", &opts_allow_cat()).is_empty(),
        "bare ASCII must never reach a marks-required key"
    );
    assert!(
        classes("cat file.txt", &default_opts()).is_empty(),
        "bare `cat` must stay opt-in"
    );
}

#[test]
fn a_bare_claimant_keeps_its_key_matchable_across_a_precedence_replacement() {
    let o = opts_allow_cat();
    let bare = |lx: &Lexicon| -> Vec<Class> {
        lx.scan("cicak", &o).into_iter().map(|m| m.class).collect()
    };
    let id_only = Lexicon::with_languages(&["id"]);
    assert_eq!(
        bare(&id_only),
        vec![Class::Animal],
        "fixture: id alone works"
    );
    for langs in [&["hu", "id"][..], &["id", "hu"][..], &["all"][..]] {
        assert!(
            !bare(&Lexicon::with_languages(langs)).is_empty(),
            "{langs:?}: adding a language silenced the bare Indonesian word"
        );
    }
    for langs in [&["hu"][..], &["all"][..]] {
        let lx = Lexicon::with_languages(langs);
        let got: Vec<Class> = lx.scan("cicák", &o).into_iter().map(|m| m.class).collect();
        assert_eq!(got, vec![Class::Feline], "{langs:?}: hu lost cicák");
    }

    let rows =
        |first: &str, second: &str| format!("{first}\n{second}\n", first = first, second = second);
    let feline = "[[entry]]\nclass=\"feline\"\nlang=\"hu\"\nmode=\"forms\"\ncjk=false\nambiguous=false\nforms=[\"zzcicák\"]\n";
    let animal = "[[entry]]\nclass=\"animal\"\nlang=\"id\"\nmode=\"forms\"\ncjk=false\nambiguous=false\nspecies=\"lizard\"\nforms=[\"zzcicak\"]\n";
    for (order, src) in [
        ("bare first", rows(animal, feline)),
        ("marked first", rows(feline, animal)),
    ] {
        let lx = Lexicon::from_sources(&src, "", &["all"]).expect("synthetic lexicon parses");
        let got: Vec<Class> = lx
            .scan("zzcicak", &o)
            .into_iter()
            .map(|m| m.class)
            .collect();
        assert_eq!(
            got,
            vec![Class::Feline],
            "{order}: the bare spelling must stay matchable whichever claimant \
             the file lists first (it keeps the WINNING class)"
        );
        let marked: Vec<Class> = lx
            .scan("zzcicák", &o)
            .into_iter()
            .map(|m| m.class)
            .collect();
        assert_eq!(marked, vec![Class::Feline], "{order}: the marked spelling");
    }
}

#[test]
fn cjk_feline_compounds_match() {
    let o = default_opts();
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
    let on = ScanOptions {
        cjk_single_char: true,
        ..ScanOptions::default()
    };
    for decoy in ["猫車", "猫背", "招き猫", "海猫"] {
        assert_eq!(
            classes(decoy, &on),
            Vec::<Class>::new(),
            "exception compound matched: {decoy:?}"
        );
    }
}

fn marked_witness(folded_key: &str) -> String {
    let mut chars = folded_key.chars();
    let mut out = String::with_capacity(folded_key.len() + 2);
    if let Some(first) = chars.next() {
        out.push(first);
        out.push('\u{0301}');
    }
    out.extend(chars);
    out
}

#[test]
fn every_surface_round_trips() {
    let lx = lex();
    let o = ScanOptions {
        allow_bare_cat: true,
        cjk_single_char: true,
        ..ScanOptions::default()
    };
    for (surface, class) in lx.iter_spaced() {
        if lx.surface_requires_marks(surface) {
            assert!(
                lx.scan(surface, &o).is_empty(),
                "marks-required surface {surface:?} matched its bare skeleton"
            );
            let witness = marked_witness(surface);
            let got = lx.scan(&witness, &o);
            assert!(
                got.iter().any(|m| m.class == class && !m.langs.is_empty()),
                "marks-required surface {surface:?} ({class:?}) did not \
                 round-trip, with its langs, via marked witness {witness:?}; got {got:?}"
            );
            continue;
        }
        let got = lx.scan(surface, &o);
        assert!(
            got.iter().any(|m| m.class == class && !m.langs.is_empty()),
            "surface {surface:?} ({class:?}) did not round-trip with its langs; got {got:?}"
        );
    }
    for (surface, class) in lx.iter_cjk() {
        let got = lx.scan(surface, &o);
        assert!(
            got.iter().any(|m| m.class == class && !m.langs.is_empty()),
            "cjk surface {surface:?} ({class:?}) did not round-trip with its langs; got {got:?}"
        );
    }
}

#[test]
fn cross_class_homograph_resolves_to_profanity() {
    assert_eq!(classes("poes", &default_opts()), vec![Class::Profanity]);
}

#[test]
fn arabic_hebrew_clitics_are_stripped() {
    let lx = lex();
    assert_eq!(lx.classify_token("القطة"), Some(Class::Feline));
    assert_eq!(lx.classify_token("وقطة"), Some(Class::Feline));
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
    assert_eq!(lx.scan("fucked", &opts).len(), 1);
}

#[test]
fn user_override_extends_coverage() {
    let extra =
        "[[entry]]\nclass=\"feline\"\nlang=\"en\"\nmode=\"forms\"\nforms=[\"floof\",\"floofs\"]\n";
    let lx = Lexicon::with_languages_and_override(&["all"], Some(extra)).expect("override parses");
    assert!(lx.conflicts().is_empty());
    let o = ScanOptions::default();
    let m = lx.scan("look a floof", &o);
    assert_eq!(m.len(), 1, "override surface must match");
    assert_eq!(m[0].class, Class::Feline);
    assert_eq!(lx.scan("fucking", &o).len(), 1);
}

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
    let single = ScanOptions {
        cjk_single_char: true,
        ..ScanOptions::default()
    };
    assert_eq!(lx.scan("これは犬です", &single).len(), 1);
    assert!(lx.scan("これは犬です", &ScanOptions::default()).is_empty());
    assert!(lx.scan("go abc猫 now", &ScanOptions::default()).is_empty());
}

#[test]
fn lossy_folded_surfaces_do_not_match_their_bare_skeleton() {
    let o = default_opts();
    for (marked, bare) in [("đm", "dm"), ("coño", "cono"), ("pička", "picka")] {
        assert_eq!(
            classes(marked, &o),
            vec![Class::Profanity],
            "marked form {marked:?} must keep its match"
        );
        assert_eq!(
            classes(bare, &o),
            Vec::<Class>::new(),
            "bare skeleton {bare:?} must not match {marked:?}"
        );
    }
    assert_eq!(classes("scheisse", &o), vec![Class::Profanity]);
}

#[test]
fn user_override_opts_a_bare_skeleton_in() {
    let over = "[[entry]]\nclass=\"profanity\"\nlang=\"vi\"\nmode=\"forms\"\nforms=[\"dm\"]\n";
    let lx = Lexicon::with_languages_and_override(&["all"], Some(over)).expect("override parses");
    let m = lx.scan("dm", &default_opts());
    assert_eq!(m.len(), 1, "explicitly listed bare surface must match");
    assert_eq!(m[0].class, Class::Profanity);
}

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

#[test]
fn class_precedence_total_order() {
    let over = r#"
[[entry]]
class    = "emphasis"
lang     = "en"
mode     = "forms"
stems    = []
suffixes = []
forms    = ["kitty", "fucking", "megasplash", "megapod"]
cjk      = false
ambiguous= false

[[entry]]
class    = "animal"
lang     = "en"
mode     = "forms"
species  = "whale"
stems    = []
suffixes = []
forms    = ["megasplash"]
cjk      = false
ambiguous= false

[[entry]]
class    = "orca"
lang     = "en"
mode     = "forms"
stems    = []
suffixes = []
forms    = ["megaorca", "megapod"]
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
    assert_eq!(one("megasplash"), Class::Animal, "animal outranks emphasis");
    assert!(
        lx.scan("megaorca", &o).is_empty(),
        "a retired class = \"orca\" entry loads as nothing, not as an error"
    );
    assert_eq!(
        one("megapod"),
        Class::Emphasis,
        "a retired orca entry no longer claims a word a lower class also lists"
    );
}

#[test]
fn form_hash_populated_and_position_independent() {
    let lx = lex();
    let o = opts_allow_cat();
    let ms = lx.scan("cat and CAT and cats", &o);
    assert_eq!(ms.len(), 3);
    assert_eq!(ms[0].form_hash, aterm_lexicon::form_hash("cat"));
    assert_eq!(ms[2].form_hash, aterm_lexicon::form_hash("cats"));
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

fn langs_of(lx: &Lexicon, text: &str, opts: &ScanOptions) -> Vec<String> {
    let ms = lx.scan(text, opts);
    assert_eq!(ms.len(), 1, "expected one match in {text:?}, got {ms:?}");
    ms[0]
        .langs
        .iter()
        .map(|id| lx.lang_code(id).to_string())
        .collect()
}

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
    let g = gated.scan("zzmiaou", &o)[0].langs;
    let u = open.scan("zzmiaou", &o)[0].langs;
    assert_eq!(
        u.union(g),
        u,
        "gated langs must be a subset under equal ids"
    );
    assert!(g.len() == 1 && u.len() == 2);
}

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

#[test]
fn empty_lang_interns_as_unknown() {
    let toml = "[[entry]]\nclass=\"feline\"\nmode=\"forms\"\nforms=[\"zzfloofy\"]\n";
    let lx = Lexicon::from_sources(toml, "", &["all"]).expect("sources parse");
    assert_eq!(langs_of(&lx, "zzfloofy", &default_opts()), vec!["unknown"]);
}

#[test]
fn form_hash_populated_on_cjk_path() {
    let lx = lex();
    let o = default_opts();
    let a = lx.scan("子猫", &o);
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].form_hash, aterm_lexicon::form_hash("子猫"));
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

#[test]
fn non_user_short_emphasis_surface_stays_suppressed() {
    let toml = "[[entry]]\nclass=\"emphasis\"\nlang=\"en\"\nmode=\"forms\"\nforms=[\"gg\"]\n";
    let lx = Lexicon::from_sources(toml, "", &["all"]).expect("sources parse");
    assert!(
        lx.scan("gg well played", &default_opts()).is_empty(),
        "short emphasis surfaces without user consent never fire"
    );
}

#[test]
fn form_hash_helpers_match_scan_semantics() {
    assert!(aterm_lexicon::is_no_space_surface("猫"));
    assert!(aterm_lexicon::is_no_space_surface("ねこちゃん"));
    assert!(!aterm_lexicon::is_no_space_surface("cat"));
    assert!(!aterm_lexicon::is_no_space_surface("猫cat"));
    assert!(!aterm_lexicon::is_no_space_surface(""));

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

    let over =
        "[[entry]]\nclass=\"emphasis\"\nlang=\"ja\"\nmode=\"forms\"\ncjk=true\nforms=[\"超考\"]\n";
    let lx = Lexicon::with_languages_and_override(&["all"], Some(over)).expect("override parses");
    let m = lx.scan("これは超考です", &default_opts());
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].form_hash, aterm_lexicon::form_hash("超考"));
}

#[test]
fn a_configured_language_the_data_never_names_is_reported_not_swallowed() {
    for bad in ["DA", "da-DK", "dk", "ALL", "ger", "english"] {
        let lx = Lexicon::with_languages(&[bad]);
        assert!(
            lx.conflicts().iter().any(|c| c.contains(bad)),
            "{bad:?} un-gated nothing and nothing said so: {:?}",
            lx.conflicts()
        );
    }
    for good in [&["da"][..], &["en", "de"][..], &["all"][..], &[][..]] {
        let lx = Lexicon::with_languages(good);
        assert_eq!(
            lx.conflicts(),
            &[] as &[String],
            "{good:?} is a valid configuration"
        );
    }
}

#[test]
fn an_unknown_entry_mode_is_reported_and_its_stems_are_not_lost_in_silence() {
    let src = "[[entry]]\nclass=\"feline\"\nlang=\"en\"\nmode=\"form\"\ncjk=false\n\
               ambiguous=false\nstems=[\"zzkitten\"]\nsuffixes=[\"\"]\nforms=[]\n";
    let lx = Lexicon::from_sources(src, "", &["all"]).expect("parses");
    assert!(
        lx.conflicts().iter().any(|c| c.contains("unknown mode")),
        "a typo'd mode discarded its stems in silence: {:?}",
        lx.conflicts()
    );
    for mode in ["suffix", "forms"] {
        let ok = src.replace("mode=\"form\"", &format!("mode=\"{mode}\""));
        let lx = Lexicon::from_sources(&ok, "", &["all"]).expect("parses");
        assert_eq!(lx.conflicts(), &[] as &[String], "mode={mode:?}");
    }
}

#[test]
fn a_typod_key_in_lexicon_data_is_a_parse_error_not_an_empty_entry() {
    for (bad, good) in [
        ("form=[\"zzkitten\"]", "forms=[\"zzkitten\"]"),
        ("stem=[\"zzkitten\"]", "stems=[\"zzkitten\"]"),
    ] {
        let src = |kv: &str| {
            format!(
                "[[entry]]\nclass=\"feline\"\nlang=\"en\"\nmode=\"forms\"\ncjk=false\nambiguous=false\n{kv}\n"
            )
        };
        assert!(
            Lexicon::from_sources(&src(bad), "", &["all"]).is_err(),
            "{bad:?} parsed instead of failing"
        );
        assert!(
            Lexicon::from_sources(&src(good), "", &["all"]).is_ok(),
            "fixture: {good:?} must still parse"
        );
    }
    assert!(
        Lexicon::from_sources("", "[[exception]]\nwords=\"zz\"\n", &["all"]).is_err(),
        "a typo'd exception key parsed instead of failing"
    );
    assert!(
        Lexicon::from_sources("", "[[exception]]\nword=\"zz\"\n", &["all"]).is_ok(),
        "fixture: the real key must still parse"
    );
}

#[test]
fn the_builtin_decoded_at_build_time_is_the_lexicon_of_the_data_files() {
    const LEXICON: &str = include_str!("../data/lexicon.toml");
    const EXCEPTIONS: &str = include_str!("../data/exceptions.toml");
    #[derive(serde::Deserialize)]
    struct Doc {
        entry: Vec<Row>,
    }
    #[derive(serde::Deserialize)]
    struct Row {
        #[serde(default)]
        lang: String,
    }
    let doc: Doc = aterm_toml::from_str(LEXICON).expect("the data file decodes");
    let mut langs: Vec<String> = Vec::new();
    for row in doc.entry {
        if !langs.contains(&row.lang) {
            langs.push(row.lang);
        }
    }
    assert!(langs.len() > 10, "fixture: the data names its languages");
    let mut sets: Vec<Vec<&str>> = vec![vec![], vec!["en"], vec!["all"]];
    sets.extend(langs.iter().map(|lang| vec![lang.as_str()]));
    for set in &sets {
        let decoded =
            Lexicon::from_sources(LEXICON, EXCEPTIONS, set).expect("the data files decode");
        assert_eq!(
            format!("{:?}", Lexicon::with_languages(set)),
            format!("{decoded:?}"),
            "with_languages({set:?})"
        );
        let unextended =
            Lexicon::with_languages_and_override(set, None).expect("no override to refuse");
        assert_eq!(
            format!("{unextended:?}"),
            format!("{decoded:?}"),
            "with_languages_and_override({set:?}, None)"
        );
    }
}

#[test]
fn an_entry_whose_every_surface_is_unmatchable_is_reported() {
    let head =
        "[[entry]]\nclass=\"canine\"\nlang=\"id\"\nmode=\"forms\"\ncjk=false\nambiguous=false\n";
    let dead = Lexicon::from_sources(&format!("{head}forms=[\"anak anjing\"]\n"), "", &["all"])
        .expect("parses");
    assert!(
        dead.conflicts()
            .iter()
            .any(|c| c.contains("NO scannable surface")),
        "an entry that compiled to nothing said nothing: {:?}",
        dead.conflicts()
    );
    let live = Lexicon::from_sources(
        &format!("{head}forms=[\"guguk\", \"anak anjing\"]\n"),
        "",
        &["all"],
    )
    .expect("parses");
    assert_eq!(live.conflicts(), &[] as &[String]);
}
