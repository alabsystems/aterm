// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Serde corpus for the sparkle-words custom fragment and Toy Packs.

#[path = "../../aterm-json/test_support/serde_corpus.rs"]
mod answers;

use answers::Corpus;
use aterm_effects::spec::{compile_toy_pack_toml, parse_custom_toml};

fn mutate(base: &str, edits: &[(&str, &str)]) -> String {
    let mut doc = base.to_string();
    for (from, to) in edits {
        assert_eq!(doc.matches(from).count(), 1, "{from:?} must occur once");
        doc = doc.replacen(from, to, 1);
    }
    doc
}

fn lf(text: &str) -> String {
    text.replace("\r\n", "\n")
}

fn sorted_overrides(debug: &str) -> String {
    const OPEN: &str = "overrides: {";
    let mut out = String::with_capacity(debug.len());
    let mut rest = debug;
    while let Some(at) = rest.find(OPEN) {
        let open = at + OPEN.len();
        out.push_str(&rest[..open]);
        let close = open + rest[open..].find('}').expect("the map closes");
        let mut entries: Vec<(u64, &str)> = rest[open..close]
            .split(", ")
            .filter(|entry| !entry.is_empty())
            .map(|entry| {
                let (hash, _) = entry.split_once(": ").expect("a map entry");
                (hash.parse().expect("a form hash"), entry)
            })
            .collect();
        entries.sort_unstable();
        let entries: Vec<&str> = entries.into_iter().map(|(_, entry)| entry).collect();
        out.push_str(&entries.join(", "));
        rest = &rest[close..];
    }
    out.push_str(rest);
    out
}

const CUSTOM: &str = r#"[[sparkle_words.custom]]
words = ["shipit", "deployed"]
ink = "rainbow"
burst = { kind = "starburst", chance = 40 }
graphic = { collection = "cats" }
"#;

#[test]
fn custom_word_fragments_answer_as_recorded() {
    let mut corpus = Corpus::new(env!("CARGO_MANIFEST_DIR"), "custom-words");
    let read = |doc: &str| parse_custom_toml(doc).map(|entries| format!("{entries:?}"));
    corpus.record("base", read(CUSTOM));
    corpus.record("empty", read(""));
    let cases: &[(&str, &[(&str, &str)])] = &[
        (
            "ink/table",
            &[(
                "ink = \"rainbow\"",
                "ink = { colorway = \"twotone:#112233,#445566\" }",
            )],
        ),
        (
            "ink/table-sweep",
            &[(
                "ink = \"rainbow\"",
                "ink = { colorway = \"rainbow\", sweep_once = true }",
            )],
        ),
        (
            "ink/table-unknown-key",
            &[(
                "ink = \"rainbow\"",
                "ink = { colorway = \"rainbow\", hue = 3 }",
            )],
        ),
        (
            "ink/table-missing-colorway",
            &[("ink = \"rainbow\"", "ink = { sweep_once = true }")],
        ),
        (
            "ink/table-wrong-sweep",
            &[(
                "ink = \"rainbow\"",
                "ink = { colorway = \"rainbow\", sweep_once = \"yes\" }",
            )],
        ),
        ("ink/integer", &[("ink = \"rainbow\"", "ink = 5")]),
        (
            "ink/array-one",
            &[("ink = \"rainbow\"", "ink = [\"rainbow\"]")],
        ),
        (
            "ink/array-two",
            &[("ink = \"rainbow\"", "ink = [\"rainbow\", true]")],
        ),
        ("ink/array-empty", &[("ink = \"rainbow\"", "ink = []")]),
        ("ink/empty-table", &[("ink = \"rainbow\"", "ink = {}")]),
        (
            "missing/words",
            &[("words = [\"shipit\", \"deployed\"]\n", "")],
        ),
        ("missing/burst-kind", &[("kind = \"starburst\", ", "")]),
        ("unknown/entry", &[("graphic = ", "colour = 1\ngraphic = ")]),
        (
            "unknown/root",
            &[(
                "[[sparkle_words.custom]]",
                "theme = \"x\"\n[[sparkle_words.custom]]",
            )],
        ),
        (
            "wrong-type/words-string",
            &[("words = [\"shipit\", \"deployed\"]", "words = \"shipit\"")],
        ),
        (
            "wrong-type/words-mixed",
            &[(
                "words = [\"shipit\", \"deployed\"]",
                "words = [\"shipit\", 3]",
            )],
        ),
        (
            "wrong-type/graphic-string",
            &[("graphic = { collection = \"cats\" }", "graphic = \"cats\"")],
        ),
        (
            "array-for-table/burst",
            &[(
                "burst = { kind = \"starburst\", chance = 40 }",
                "burst = [\"starburst\", 40]",
            )],
        ),
        (
            "array-for-table/burst-short",
            &[(
                "burst = { kind = \"starburst\", chance = 40 }",
                "burst = [\"starburst\"]",
            )],
        ),
        (
            "array-for-table/burst-long",
            &[(
                "burst = { kind = \"starburst\", chance = 40 }",
                "burst = [\"starburst\", 40, 1]",
            )],
        ),
        (
            "table-for-array/custom",
            &[("[[sparkle_words.custom]]", "[sparkle_words.custom]")],
        ),
        (
            "array-for-table/sparkle-words",
            &[(
                "[[sparkle_words.custom]]\n",
                "sparkle_words = []\n[[other]]\n",
            )],
        ),
        (
            "int/chance-u32-overflow",
            &[("chance = 40", "chance = 4294967296")],
        ),
        ("int/chance-negative", &[("chance = 40", "chance = -1")]),
        (
            "zero/chance-negative-int",
            &[("chance = 40", "chance = -0")],
        ),
        ("float/chance-nan", &[("chance = 40", "chance = nan")]),
        ("float/chance-inf", &[("chance = 40", "chance = inf")]),
    ];
    for (id, edits) in cases {
        corpus.record(*id, read(&mutate(CUSTOM, edits)));
    }
    corpus.finish();
}

const TOY_PACK: &str = r##"schema = 1

[pack]
id = "community.example.corpus"
name = "Corpus"
version = 3
authors = ["aterm"]
license = "Apache-2.0"

[[toy]]
id = "spark"
lang = "en"
words = ["shipit"]
ink = { kind = "two_tone", colors = ["#58D8FF", "#FF73B9"] }
burst = { kind = "starburst", chance_pct = 35 }

[[toy]]
id = "purr"
lang = "en"
words = ["purrfect"]
ink = { kind = "self_glow", lift = 0.76, amplitude = 0.48, window_ms = 1350 }
graphic = { collection = "cats", dwell_ms = [2300, 3400] }
"##;

#[test]
fn toy_packs_answer_as_recorded() {
    let mut corpus = Corpus::new(env!("CARGO_MANIFEST_DIR"), "toy-pack");
    let read =
        |doc: &str| compile_toy_pack_toml(doc).map(|pack| sorted_overrides(&format!("{pack:?}")));
    corpus.record(
        "bundled/tiny-triumphs",
        read(&lf(include_str!(
            "../toy-packs/community/tiny-triumphs/pack.toml"
        ))),
    );
    corpus.record("base", read(TOY_PACK));
    let toy_tables = &TOY_PACK[TOY_PACK.find("[[toy]]").expect("toy tables")..];
    let cases: &[(&str, &[(&str, &str)])] = &[
        (
            "unknown-key/root",
            &[("schema = 1\n", "schema = 1\nsigned = true\n")],
        ),
        (
            "unknown-key/pack",
            &[(
                "license = \"Apache-2.0\"\n",
                "license = \"Apache-2.0\"\nhomepage = \"x\"\n",
            )],
        ),
        (
            "unknown-key/toy",
            &[("id = \"purr\"\n", "id = \"purr\"\nsound = \"meow\"\n")],
        ),
        (
            "unknown-key/ink",
            &[("window_ms = 1350 }", "window_ms = 1350, hue = 2 }")],
        ),
        (
            "unknown-key/burst",
            &[("chance_pct = 35 }", "chance_pct = 35, loud = true }")],
        ),
        (
            "unknown-key/graphic",
            &[(
                "dwell_ms = [2300, 3400] }",
                "dwell_ms = [2300, 3400], size = 2 }",
            )],
        ),
        ("missing-key/schema", &[("schema = 1\n", "")]),
        ("missing-key/pack-version", &[("version = 3\n", "")]),
        (
            "missing-key/toy-lang",
            &[(
                "lang = \"en\"\nwords = [\"shipit\"]",
                "words = [\"shipit\"]",
            )],
        ),
        ("missing-key/ink-kind", &[("kind = \"self_glow\", ", "")]),
        ("missing-key/toys", &[(toy_tables, "")]),
        (
            "wrong-type/version-string",
            &[("version = 3", "version = \"3\"")],
        ),
        (
            "wrong-type/authors-string",
            &[("authors = [\"aterm\"]", "authors = \"aterm\"")],
        ),
        (
            "wrong-type/lift-string",
            &[("lift = 0.76", "lift = \"0.76\"")],
        ),
        ("wrong-type/lift-int", &[("lift = 0.76", "lift = 1")]),
        (
            "array-for-table/pack",
            &[(
                "[pack]\nid = \"community.example.corpus\"\nname = \"Corpus\"\nversion = 3\nauthors = [\"aterm\"]\nlicense = \"Apache-2.0\"\n",
                "pack = [\"community.example.corpus\", \"Corpus\", 3, [\"aterm\"], \"Apache-2.0\"]\n",
            )],
        ),
        (
            "array-for-table/burst",
            &[(
                "burst = { kind = \"starburst\", chance_pct = 35 }",
                "burst = [\"starburst\", 35]",
            )],
        ),
        (
            "fixed-array/colors-three",
            &[(
                "colors = [\"#58D8FF\", \"#FF73B9\"]",
                "colors = [\"#58D8FF\", \"#FF73B9\", \"#000000\"]",
            )],
        ),
        (
            "fixed-array/colors-one",
            &[(
                "colors = [\"#58D8FF\", \"#FF73B9\"]",
                "colors = [\"#58D8FF\"]",
            )],
        ),
        (
            "fixed-array/dwell-three",
            &[("dwell_ms = [2300, 3400]", "dwell_ms = [2300, 3400, 5000]")],
        ),
        (
            "int/version-u32-overflow",
            &[("version = 3", "version = 4294967296")],
        ),
        (
            "int/chance-negative",
            &[("chance_pct = 35", "chance_pct = -35")],
        ),
        (
            "int/window-i64-overflow",
            &[("window_ms = 1350", "window_ms = 99999999999999999999")],
        ),
        (
            "zero/version-negative-int",
            &[("version = 3", "version = -0")],
        ),
        (
            "zero/lift-negative-float",
            &[("lift = 0.76", "lift = -0.0")],
        ),
        ("float/lift-nan", &[("lift = 0.76", "lift = nan")]),
        ("float/lift-inf", &[("lift = 0.76", "lift = inf")]),
        (
            "float/lift-f32-overflow",
            &[("lift = 0.76", "lift = 1e300")],
        ),
        (
            "float/amplitude-tiny",
            &[("amplitude = 0.48", "amplitude = 1e-300")],
        ),
    ];
    for (id, edits) in cases {
        corpus.record(*id, read(&mutate(TOY_PACK, edits)));
    }
    corpus.finish();
}
