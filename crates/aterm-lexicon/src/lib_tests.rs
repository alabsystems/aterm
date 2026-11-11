// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn lex() -> Lexicon {
    Lexicon::with_languages(&["all"])
}

#[test]
fn cjk_prefilter_admits_every_surface_it_gates() {
    for langs in [&["en"][..], &["all"][..], &["ja", "th"][..]] {
        let lx = Lexicon::with_languages(langs);
        let surfaces = lx
            .cjk_forms
            .keys()
            .chain(lx.cjk_exceptions.iter())
            .map(String::as_str);
        for surface in surfaces {
            let first = surface
                .chars()
                .next()
                .expect("builder skips empty CJK surfaces");
            let len = surface.chars().count();
            let head_max = lx.cjk_heads.get(&first).copied().unwrap_or(0);
            assert!(
                head_max >= len,
                "{langs:?}: surface {surface:?} ({len} chars) is gated behind \
                 first character {first:?} whose stored maximum is {head_max} \
                 — the scan would clamp its window below this surface and \
                 never probe it"
            );
            assert!(
                head_max <= lx.max_cjk,
                "{langs:?}: first character {first:?} stores {head_max}, \
                 above max_cjk {}",
                lx.max_cjk
            );
        }
    }
}

#[test]
fn romanian_fut_forms_are_ambiguous_in_ro_and_all_but_not_default_english() {
    let en = Lexicon::with_languages(&["en"]);
    let opts = ScanOptions::default();

    for prefix in ["f", "fu", "fut", "futu", "futur", "future"] {
        assert!(
            en.scan(prefix, &opts).is_empty(),
            "default English lexicon classified future prefix {prefix:?}"
        );
    }
    assert!(en.scan("fuc", &opts).is_empty());

    for language in ["ro", "all"] {
        let lexicon = Lexicon::with_languages(&[language]);
        for prefix in ["f", "fu"] {
            assert!(lexicon.scan(prefix, &opts).is_empty());
        }
        for prefix in ["fut", "futu"] {
            let hits = lexicon.scan(prefix, &opts);
            assert_eq!(hits.len(), 1, "{language} lost exact form {prefix:?}");
            assert_eq!(hits[0].class, Class::Profanity);
            assert!(
                hits[0].ambiguous,
                "{language} {prefix:?} must defer at a caret"
            );

            let delimited = lexicon.scan(&format!("{prefix} "), &opts);
            assert_eq!(delimited.len(), 1, "{language} lost delimited {prefix:?}");
            assert!(delimited[0].ambiguous);
        }
        for completed in ["futur", "future", "fuc", "fix", "fix!", "fixes"] {
            assert!(
                lexicon.scan(completed, &opts).is_empty(),
                "{language} classified harmless token {completed:?}"
            );
        }
        let fuck = lexicon.scan("fuck", &opts);
        assert_eq!(fuck.len(), 1);
        assert_eq!(fuck[0].class, Class::Profanity);
        assert!(
            !fuck[0].ambiguous,
            "fuck must remain an immediate exact match"
        );
    }
}

#[test]
fn resident_scan_scratch_matches_compatibility_and_stops_growing() {
    let lx = lex();
    let opts = ScanOptions {
        allow_bare_cat: true,
        cjk_single_char: true,
        ignore: None,
    };
    let text = "kitty's 子猫 القطة وقطة fuck";
    let compat_chars: Vec<char> = text.chars().collect();
    let compat_hits = lx.scan(text, &opts);

    let mut chars = Vec::new();
    let mut hits = Vec::new();
    let mut scratch = ScanScratch::default();
    lx.scan_into_with_scratch(text, &opts, &mut chars, &mut hits, &mut scratch);
    assert_eq!(chars, compat_chars);
    assert_eq!(hits, compat_hits);
    assert_eq!(hits.len(), 5, "all scratch lanes must reach a real match");

    let capacities = |scratch: &ScanScratch, chars: &Vec<char>, hits: &Vec<Match>| {
        [
            chars.capacity(),
            hits.capacity(),
            scratch.token.capacity(),
            scratch.folded.capacity(),
            scratch.candidate_folded.capacity(),
            scratch.window.capacity(),
            scratch.bounds.capacity(),
        ]
    };
    let warm = capacities(&scratch, &chars, &hits);
    for _ in 0..1000 {
        lx.scan_into_with_scratch(text, &opts, &mut chars, &mut hits, &mut scratch);
        assert_eq!(hits, compat_hits);
    }
    assert_eq!(
        capacities(&scratch, &chars, &hits),
        warm,
        "resident scanner buffers must stop growing after warmup"
    );
}

#[test]
fn cjk_run_scratch_reuse_preserves_matches_and_ignore() {
    use std::collections::HashSet;
    let lx = lex();
    let o = ScanOptions::default();
    let got = lx.scan("子猫 ねこ", &o);
    assert_eq!(got.len(), 2, "got {got:?}");
    assert_eq!(
        (got[0].start, got[0].end, got[0].class),
        (0, 2, Class::Feline)
    );
    assert_eq!(
        (got[1].start, got[1].end, got[1].class),
        (3, 5, Class::Feline)
    );
    let ignore: HashSet<String> = ["子猫".to_string()].into_iter().collect();
    let oi = ScanOptions {
        ignore: Some(&ignore),
        ..ScanOptions::default()
    };
    assert!(
        lx.scan("子猫", &oi).is_empty(),
        "ignored CJK compound must not match"
    );
}

#[test]
fn ignore_guard_keys_on_full_token_fold_not_stripped_stem() {
    use std::collections::HashSet;
    let lx = lex();
    assert_eq!(lx.scan("kitty's", &ScanOptions::default()).len(), 1);

    let full: HashSet<String> = ["kitty's".to_string()].into_iter().collect();
    let opt_full = ScanOptions {
        ignore: Some(&full),
        ..ScanOptions::default()
    };
    assert!(
        lx.scan("kitty's", &opt_full).is_empty(),
        "ignoring the full surface must suppress the match"
    );

    let stem: HashSet<String> = ["kitty".to_string()].into_iter().collect();
    let opt_stem = ScanOptions {
        ignore: Some(&stem),
        ..ScanOptions::default()
    };
    assert_eq!(
        lx.scan("kitty's", &opt_stem).len(),
        1,
        "ignoring only the stripped stem must NOT suppress (full-token fold is the key)"
    );
}

#[test]
fn suffix_expansion_is_globally_bounded() {
    let mut toml = String::new();
    toml.push_str(
        "[[entry]]\nclass = \"feline\"\nlang = \"xx\"\nmode = \"suffix\"\n\
         stems = [\"qwxfeline\", \"qwxkitten\"]\nsuffixes = [\"\", \"z\"]\n\n",
    );
    toml.push_str("[[entry]]\nclass = \"feline\"\nlang = \"yy\"\nmode = \"suffix\"\nstems = [");
    for i in 0..2000 {
        if i > 0 {
            toml.push_str(", ");
        }
        toml.push_str(&format!("\"qwxstem{i}\""));
    }
    toml.push_str("]\nsuffixes = [");
    for i in 0..2000 {
        if i > 0 {
            toml.push_str(", ");
        }
        toml.push_str(&format!("\"qwxsuf{i}\""));
    }
    toml.push_str("]\n");

    let lx = Lexicon::with_languages_and_override(&["all"], Some(&toml))
        .expect("override lexicon must parse");

    assert!(
        lx.spaced_form_count() <= MAX_TOTAL_SURFACES,
        "surface count {} exceeded cap {MAX_TOTAL_SURFACES}",
        lx.spaced_form_count(),
    );
    assert!(
        lx.conflicts().iter().any(|c| c.contains("surface cap")),
        "expected a surface-cap conflict, got {:?}",
        lx.conflicts(),
    );
    assert_eq!(lx.classify_token("qwxfeline"), Some(Class::Feline));
    assert_eq!(lx.classify_token("qwxfelinez"), Some(Class::Feline));
    assert_eq!(lx.classify_token("qwxkittenz"), Some(Class::Feline));
    assert_eq!(lx.classify_token("qwxstem0qwxsuf0"), None);
}

#[test]
fn no_space_surface_length_is_bounded() {
    let long = "\u{9F98}".repeat(4096);
    let fits = format!("{}\u{9F8D}", "\u{9F98}".repeat(MAX_CJK_SURFACE_CHARS - 1));
    let toml = format!(
        "[[entry]]\nclass = \"feline\"\nlang = \"zh\"\nmode = \"forms\"\ncjk = true\n\
         forms = [\"{long}\", \"{fits}\"]\n"
    );
    let lx = Lexicon::with_languages_and_override(&["all"], Some(&toml))
        .expect("override lexicon must parse");
    assert!(
        lx.max_cjk <= MAX_CJK_SURFACE_CHARS,
        "the scan window is {} characters, above the {MAX_CJK_SURFACE_CHARS} bound",
        lx.max_cjk
    );
    assert!(
        lx.cjk_heads
            .values()
            .all(|&len| len <= MAX_CJK_SURFACE_CHARS)
    );
    assert!(
        lx.conflicts().iter().any(|c| c.contains("4096 characters")),
        "the dropped surface must be reported: {:?}",
        lx.conflicts()
    );
    let line = format!("{}{fits}", "\u{9F98}".repeat(20_000));
    let hits = lx.scan(&line, &ScanOptions::default());
    let spans: Vec<(usize, usize)> = hits.iter().map(|m| (m.start, m.end)).collect();
    assert_eq!(spans, [(20_000, 20_000 + MAX_CJK_SURFACE_CHARS)]);
}

#[test]
fn langset_bitset_mechanics() {
    let a = LangSet::EMPTY.with(LangId(3)).with(LangId(7));
    let b = LangSet::EMPTY.with(LangId(1));
    let u = a.union(b);
    assert!(u.contains(LangId(1)) && u.contains(LangId(3)) && u.contains(LangId(7)));
    assert!(!u.contains(LangId(0)));
    assert_eq!(u.len(), 3);
    assert!(!u.is_empty());
    assert!(LangSet::EMPTY.is_empty());
    assert_eq!(
        u.iter().collect::<Vec<_>>(),
        vec![LangId(1), LangId(3), LangId(7)]
    );
    assert_eq!(primary_lang(u), LangId(1));
    assert_eq!(primary_lang(LangSet::EMPTY), UNKNOWN_LANG);
    assert_eq!(lex().lang_code(primary_lang(LangSet::EMPTY)), "unknown");
}

#[test]
fn lang_interning_overflow_collapses_to_unknown() {
    let mut toml = String::new();
    for i in 0..70 {
        toml.push_str(&format!(
            "[[entry]]\nclass=\"feline\"\nlang=\"l{i}\"\nmode=\"forms\"\nforms=[\"qwxlang{i}\"]\n\n"
        ));
    }
    let lx = Lexicon::from_sources(&toml, "", &["all"]).expect("sources parse");
    let o = ScanOptions::default();
    for i in [0usize, 62] {
        let ms = lx.scan(&format!("qwxlang{i}"), &o);
        assert_eq!(ms.len(), 1);
        assert_eq!(lx.lang_code(primary_lang(ms[0].langs)), format!("l{i}"));
    }
    for i in [63usize, 69] {
        let ms = lx.scan(&format!("qwxlang{i}"), &o);
        assert_eq!(ms.len(), 1);
        assert_eq!(lx.lang_code(primary_lang(ms[0].langs)), "unknown");
    }
}

#[test]
fn padded_entry_lang_trims_consistently_for_interning_gating_and_known_check() {
    let toml = "[[entry]]\nclass=\"feline\"\nlang=\" ro \"\nmode=\"forms\"\n\
                ambiguous=true\nforms=[\"qwxromcat\"]\n";
    let gated = Lexicon::from_sources(toml, "", &["en"]).expect("sources parse");
    assert!(gated.scan("qwxromcat", &ScanOptions::default()).is_empty());

    let ungated = Lexicon::from_sources(toml, "", &["ro"]).expect("sources parse");
    assert!(
        ungated.conflicts().is_empty(),
        "trimmed entry lang must satisfy configured \"ro\" without unknown-lang conflict: {:?}",
        ungated.conflicts()
    );
    let hits = ungated.scan("qwxromcat", &ScanOptions::default());
    assert_eq!(hits.len(), 1);
    assert_eq!(ungated.lang_code(primary_lang(hits[0].langs)), "ro");
}

/// The form hash is aterm_hash's FNV-1a/64 over the key's bytes: the spelled-out
/// fold and `aterm_hash::fnv1a_64` agree, and FNV-1a's published vectors hold.
#[test]
fn the_form_hash_folds_as_aterm_hash_does() {
    assert_eq!(fnv1a64(""), 0xcbf2_9ce4_8422_2325);
    assert_eq!(fnv1a64("a"), 0xaf63_dc4c_8601_ec8c);
    assert_eq!(fnv1a64("foobar"), 0x8594_4171_f739_67e8);
    for s in ["", "a", "cat", "chat's", "cat\u{2019}s", "猫", "ⲙⲟⲩⲥⲉ"] {
        assert_eq!(fnv1a64(s), aterm_hash::fnv1a_64(s.as_bytes()), "{s:?}");
    }
}

/// A possessive's form variant is its suffix's place in [`POSSESSIVES`], from 1; a suffix
/// that would leave no stem is passed over.
#[test]
fn possessive_variants_count_from_one_in_suffix_order() {
    assert_eq!(strip_possessive("cat's"), Some(("cat", 1)));
    assert_eq!(strip_possessive("cat\u{2019}s"), Some(("cat", 2)));
    assert_eq!(strip_possessive("cats'"), Some(("cats", 3)));
    assert_eq!(strip_possessive("cats\u{2019}"), Some(("cats", 4)));
    assert_eq!(strip_possessive("'s"), None, "no stem");
    assert_eq!(strip_possessive("cat"), None);
}
