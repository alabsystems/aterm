// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Serde corpus (docs/AUDIT-third-party-2026-10-15.md, SP.3) for the
//! sparkle-words custom fragment, Toy Packs and Trail Packs. Packs are
//! third-party input and every pack table is `deny_unknown_fields`; the custom
//! fragment is the opposite (container `default`, unknown keys ignored, an
//! `untagged` ink). None of these models is `Serialize`, so an `Ok` is recorded
//! as its `Debug`.

#[path = "../../aterm-json/test_support/serde_corpus.rs"]
mod answers;

use answers::Corpus;
use aterm_effects::spec::{compile_toy_pack_toml, parse_custom_toml};
use aterm_effects::trail_pack::compile_trail_pack_toml;

/// `base` with each `(from, to)` applied; `from` must occur exactly once, so
/// a mutation can never silently leave the document as it was.
fn mutate(base: &str, edits: &[(&str, &str)]) -> String {
    let mut doc = base.to_string();
    for (from, to) in edits {
        assert_eq!(doc.matches(from).count(), 1, "{from:?} must occur once");
        doc = doc.replacen(from, to, 1);
    }
    doc
}

/// A bundled document as the repository holds it. A Windows checkout under
/// `core.autocrlf` hands `include_str!` a CRLF copy, and the mutations below
/// spell their anchors with `\n`.
fn lf(text: &str) -> String {
    text.replace("\r\n", "\n")
}

/// `debug` with each `overrides: {..}` map in key order. The map is an
/// `FxHashMap`, so its own order is the toolchain's hash table's, not the
/// pack's, and a toolchain bump would move the answer with no serde change.
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
        // The untagged ink: a string, a table, and what neither variant takes.
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
        // Container default: missing and unknown keys are fine, wrong types are not.
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

const SYNTHWAVE: &str = include_str!("../assets/trail-packs/synthwave.toml");

#[test]
fn trail_packs_answer_as_recorded() {
    let mut corpus = Corpus::new(env!("CARGO_MANIFEST_DIR"), "trail-pack");
    let read = |doc: &str| compile_trail_pack_toml(doc).map(|pack| format!("{pack:?}"));
    let synthwave = lf(SYNTHWAVE);
    corpus.record("bundled/synthwave", read(&synthwave));
    corpus.record(
        "bundled/emberfall",
        read(&lf(include_str!("../assets/trail-packs/emberfall.toml"))),
    );
    let layers_start = synthwave.find("[[beam.layers]]").expect("the beam layers");
    let layers_end = synthwave.find("[ramp]").expect("the ramp table");
    let layers = &synthwave[layers_start..layers_end];
    let cases: &[(&str, &[(&str, &str)])] = &[
        (
            "unknown-key/root",
            &[(
                "name = \"Synthwave\"\n",
                "name = \"Synthwave\"\nhomepage = \"x\"\n",
            )],
        ),
        (
            "unknown-key/beam",
            &[(
                "enabled = true\ncell_frac",
                "enabled = true\nglow = 1\ncell_frac",
            )],
        ),
        (
            "unknown-key/envelope",
            &[("hold_frac = 0.45 }", "hold_frac = 0.45, tail = 1 }")],
        ),
        (
            "unknown-key/ring",
            &[("life_ms = 320\n", "life_ms = 320\nwidth = 2\n")],
        ),
        ("missing-key/pack", &[("pack = 1 ", "")]),
        (
            "missing-key/layer-thickness",
            &[("thickness_mul = 1.45\n", "")],
        ),
        (
            "missing-key/defaulted-name",
            &[("name = \"Synthwave\"\n", "")],
        ),
        (
            "wrong-type/cell-frac-string",
            &[("cell_frac = 0.16", "cell_frac = \"0.16\"")],
        ),
        (
            "wrong-type/enabled-int",
            &[("glow_add = true", "glow_add = 1")],
        ),
        (
            "wrong-type/layers-table",
            &[(
                layers,
                "[beam.layers.wide]\nthickness_mul = 2.0\ncoverage_mul = 0.14\nstride = 3\n\n",
            )],
        ),
        (
            "array-for-table/envelope",
            &[(
                "envelope = { kind = \"hold_cosine\", hold_frac = 0.45 }",
                "envelope = [\"hold_cosine\", 0.45]",
            )],
        ),
        (
            "int/stride-u32-overflow",
            &[("stride = 3", "stride = 4294967296")],
        ),
        ("int/life-negative", &[("life_ms = 320", "life_ms = -320")]),
        ("int/pack-hex", &[("pack = 1 ", "pack = 0x1 ")]),
        ("zero/pack-negative-int", &[("pack = 1 ", "pack = -0 ")]),
        (
            "zero/gravity-negative-float",
            &[("gravity = -0.4", "gravity = -0.0")],
        ),
        (
            "float/cell-frac-nan",
            &[("cell_frac = 0.16", "cell_frac = nan")],
        ),
        (
            "float/cell-frac-negative-nan",
            &[("cell_frac = 0.16", "cell_frac = -nan")],
        ),
        ("float/sat-inf", &[("sat = 0.92", "sat = inf")]),
        (
            "float/thickness-int",
            &[("thickness_mul = 2.0", "thickness_mul = 2")],
        ),
        ("float/vx-short", &[("vx = [-0.4, 0.4]", "vx = [-0.4]")]),
    ];
    for (id, edits) in cases {
        corpus.record(*id, read(&mutate(&synthwave, edits)));
    }
    corpus.finish();
}
