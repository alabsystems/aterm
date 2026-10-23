// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A vendor-direct program's stamp file under mutations, each answered as the
//! committed corpus `tests/fixtures/serde-corpus/stamp.txt` records. Unknown keys
//! ride `#[serde(flatten)]` into an `aterm_toml::Value` map and are written back;
//! `last_head`, `high_water` and the legacy pair use `Version`'s serde impls.

use crate::serde_corpus_tests::answers::Corpus;

use super::StampFile;

/// A stamp a 0.104.0 build wrote on a live machine.
const STAMP: &str = r#"schema = 1
last_checked_at = 1792106797
last_head = "2.1.301"
etag = "\"98eef3cbab3b496f63bda8ca3461b290\""
high_water = "2.1.301"
verdict = "claude 2.1.301 is Anthropic's latest"
legacy_build = 2026092201
legacy_version = "2.1.280"
legacy_root_build = 2026092201
legacy_tree_root = "cea22abbd151945cce20f03819fc7667296222f8131849fc5ecf5447236bc6f3"
"#;

/// `STAMP` with each `(from, to)` applied; `from` must occur exactly once, so
/// a mutation can never silently leave the document as it was.
fn mutate(edits: &[(&str, &str)]) -> String {
    let mut doc = STAMP.to_string();
    for (from, to) in edits {
        assert_eq!(doc.matches(from).count(), 1, "{from:?} must occur once");
        doc = doc.replacen(from, to, 1);
    }
    doc
}

fn read(doc: &str) -> Result<String, String> {
    let file: StampFile = aterm_toml::from_str(doc).map_err(|e| e.to_string())?;
    aterm_toml::to_string(&file).map_err(|e| format!("re-serialize: {e}"))
}

#[test]
fn stamp_files_answer_as_recorded() {
    let mut corpus = Corpus::new(env!("CARGO_MANIFEST_DIR"), "stamp");
    corpus.record("written-by-0.104.0", read(STAMP));
    corpus.record("minimal", read("schema = 1\nlast_checked_at = 0\n"));
    let cases: &[(&str, &[(&str, &str)])] = &[
        (
            "extra/datetime",
            &[("verdict = ", "checked = 2026-10-15T20:38:59Z\nverdict = ")],
        ),
        (
            "extra/local-date",
            &[("verdict = ", "day = 2026-10-15\nverdict = ")],
        ),
        (
            "extra/table",
            &[(
                "legacy_tree_root = ",
                "future = { a = [1, 2.5, \"x\"], at = 20:38:59 }\nlegacy_tree_root = ",
            )],
        ),
        (
            "extra/after-table",
            &[(
                "legacy_tree_root = \"cea22abbd151945cce20f03819fc7667296222f8131849fc5ecf5447236bc6f3\"\n",
                "legacy_tree_root = \"cea22abbd151945cce20f03819fc7667296222f8131849fc5ecf5447236bc6f3\"\n\n[future]\nlevel = 2\n",
            )],
        ),
        (
            "collision/key-named-extra",
            &[("verdict = ", "extra = 1\nverdict = ")],
        ),
        (
            "collision/known-field-wrong-type",
            &[("legacy_build = 2026092201", "legacy_build = \"2026092201\"")],
        ),
        (
            "collision/version-wrong-type",
            &[("last_head = \"2.1.301\"", "last_head = 2")],
        ),
        (
            "version/unparseable",
            &[("last_head = \"2.1.301\"", "last_head = \"two\"")],
        ),
        (
            "version/empty",
            &[("high_water = \"2.1.301\"", "high_water = \"\"")],
        ),
        ("missing-key/schema", &[("schema = 1\n", "")]),
        (
            "missing-key/last-checked",
            &[("last_checked_at = 1792106797\n", "")],
        ),
        (
            "missing-key/defaulted-verdict",
            &[("verdict = \"claude 2.1.301 is Anthropic's latest\"\n", "")],
        ),
        (
            "wrong-type/etag-int",
            &[(
                "etag = \"\\\"98eef3cbab3b496f63bda8ca3461b290\\\"\"",
                "etag = 98",
            )],
        ),
        (
            "wrong-type/verdict-array",
            &[(
                "verdict = \"claude 2.1.301 is Anthropic's latest\"",
                "verdict = [\"latest\"]",
            )],
        ),
        (
            "int/checked-negative",
            &[(
                "last_checked_at = 1792106797",
                "last_checked_at = -1792106797",
            )],
        ),
        (
            "int/checked-i64-max",
            &[(
                "last_checked_at = 1792106797",
                "last_checked_at = 9223372036854775807",
            )],
        ),
        (
            "int/checked-i64-overflow",
            &[(
                "last_checked_at = 1792106797",
                "last_checked_at = 9223372036854775808",
            )],
        ),
        (
            "int/legacy-negative",
            &[("legacy_build = 2026092201", "legacy_build = -1")],
        ),
        (
            "int/schema-u32-overflow",
            &[("schema = 1", "schema = 4294967297")],
        ),
        (
            "zero/checked-negative-int",
            &[("last_checked_at = 1792106797", "last_checked_at = -0")],
        ),
        (
            "zero/checked-negative-float",
            &[("last_checked_at = 1792106797", "last_checked_at = -0.0")],
        ),
        (
            "float/checked-nan",
            &[("last_checked_at = 1792106797", "last_checked_at = nan")],
        ),
        (
            "float/checked-inf",
            &[("last_checked_at = 1792106797", "last_checked_at = inf")],
        ),
        (
            "array-for-table/whole",
            &[("schema = 1\n", "schema = 1\nrows = [[1, 2]]\n")],
        ),
    ];
    for (id, edits) in cases {
        corpus.record(*id, read(&mutate(edits)));
    }
    corpus.record("toml/empty", read(""));
    corpus.finish();
}
