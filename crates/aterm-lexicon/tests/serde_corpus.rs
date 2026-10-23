// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Serde corpus tests for lexicon, exceptions, and tricks documents.

#[path = "../../aterm-json/test_support/serde_corpus.rs"]
mod answers;

use answers::Corpus;
use aterm_lexicon::Lexicon;
use aterm_lexicon::tricks::TrickLexicon;

const LEXICON: &str = include_str!("../data/lexicon.toml");
const EXCEPTIONS: &str = include_str!("../data/exceptions.toml");
const TRICKS: &str = include_str!("../data/tricks.toml");

const ENTRY: &str = r#"[[entry]]
class    = "profanity"
lang     = "en"
mode     = "suffix"
stems    = ["frak"]
suffixes = ["", "ing", "ed"]
forms    = ["frakker"]
cjk      = false
ambiguous= false
notes    = "corpus"

[[entry]]
class    = "feline"
lang     = "ja"
forms    = ["ねこ"]
cjk      = true
"#;

const EXCEPTION: &str = r#"[[exception]]
word = "山猫"
lang = "ja"
meaning = "lynx / wildcat"
"#;

const TRICK: &str = r#"[[trick]]
id       = "sit"
lang     = "en"
words    = ["sit", "stay"]
address  = ["wait"]
gated    = false
cjk      = false
notes    = "corpus"

[[vocative]]
lang  = "en"
words = ["kitty"]

[[filler]]
lang  = "en"
words = ["please"]
"#;

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

fn lexicon(lexicon: &str, exceptions: &str) -> Result<String, String> {
    let built =
        Lexicon::from_sources(lexicon, exceptions, &["en", "ja"]).map_err(|e| e.to_string())?;
    let mut spaced: Vec<String> = built
        .iter_spaced()
        .map(|(form, class)| format!("{form}={class:?}"))
        .collect();
    let mut cjk: Vec<String> = built
        .iter_cjk()
        .map(|(form, class)| format!("{form}={class:?}"))
        .collect();
    spaced.sort_unstable();
    cjk.sort_unstable();
    Ok(format!(
        "spaced {spaced:?} cjk {cjk:?} species {:?} conflicts {:?}",
        built.species_codes(),
        built.conflicts()
    ))
}

fn tricks(doc: &str) -> Result<String, String> {
    TrickLexicon::rows_from_source(doc)
        .map(|rows| format!("{rows:?}"))
        .map_err(|e| e.to_string())
}

#[test]
fn lexicon_documents_answer_as_recorded() {
    let mut corpus = Corpus::new(env!("CARGO_MANIFEST_DIR"), "lexicon");
    corpus.record("bundled/lexicon", lexicon(&lf(LEXICON), &lf(EXCEPTIONS)));
    corpus.record("bundled/tricks", tricks(&lf(TRICKS)));
    corpus.record("entry/base", lexicon(ENTRY, EXCEPTION));
    corpus.record("entry/empty", lexicon("", ""));
    corpus.record("trick/base", tricks(TRICK));
    corpus.record("trick/empty", tricks(""));

    let entries: &[(&str, &[(&str, &str)])] = &[
        (
            "unknown-key",
            &[(
                "notes    = \"corpus\"\n",
                "notes    = \"corpus\"\nregion = \"us\"\n",
            )],
        ),
        (
            "unknown-key/root",
            &[(
                "[[entry]]\nclass    = \"profanity\"",
                "version = 1\n[[entry]]\nclass    = \"profanity\"",
            )],
        ),
        (
            "unknown-key/skipped-field",
            &[(
                "notes    = \"corpus\"\n",
                "notes    = \"corpus\"\nuser = true\n",
            )],
        ),
        (
            "unknown-key/renamed-field",
            &[("notes    = \"corpus\"\n", "_notes = \"corpus\"\n")],
        ),
        ("missing-key/class", &[("class    = \"profanity\"\n", "")]),
        (
            "missing-key/defaulted-mode",
            &[("mode     = \"suffix\"\n", "")],
        ),
        ("missing-key/defaulted-lang", &[("lang     = \"en\"\n", "")]),
        (
            "wrong-type/stems-string",
            &[("stems    = [\"frak\"]", "stems    = \"frak\"")],
        ),
        (
            "wrong-type/cjk-string",
            &[("cjk      = false", "cjk      = \"no\"")],
        ),
        (
            "wrong-type/class-int",
            &[("class    = \"profanity\"", "class    = 1")],
        ),
        (
            "wrong-type/notes-array",
            &[("notes    = \"corpus\"", "notes    = [\"corpus\"]")],
        ),
        (
            "table-for-array/entry",
            &[(
                "[[entry]]\nclass    = \"feline\"",
                "[entry.second]\nclass    = \"feline\"",
            )],
        ),
        (
            "wrong-type/ambiguous-string",
            &[("ambiguous= false", "ambiguous= \"no\"")],
        ),
        (
            "wrong-type/suffixes-int",
            &[("suffixes = [\"\", \"ing\", \"ed\"]", "suffixes = [\"\", 1]")],
        ),
        (
            "gated/ambiguous-unconfigured-lang",
            &[
                ("lang     = \"en\"", "lang     = \"de\""),
                ("ambiguous= false", "ambiguous= true"),
            ],
        ),
        (
            "gated/plain-unconfigured-lang",
            &[("lang     = \"en\"", "lang     = \"de\"")],
        ),
        ("zero/cjk-int", &[("cjk      = false", "cjk      = -0")]),
        ("float/cjk-nan", &[("cjk      = false", "cjk      = nan")]),
    ];
    for (id, edits) in entries {
        corpus.record(
            format!("entry/{id}"),
            lexicon(&mutate(ENTRY, edits), EXCEPTION),
        );
    }
    corpus.record(
        "entry/array-for-table/inline-row",
        lexicon(
            "entry = [[\"profanity\", \"en\", \"forms\", [], [], [\"frak\"]]]\n",
            "",
        ),
    );
    let exceptions: &[(&str, &[(&str, &str)])] = &[
        (
            "unknown-key",
            &[(
                "meaning = \"lynx / wildcat\"\n",
                "meaning = \"lynx / wildcat\"\nsource = \"x\"\n",
            )],
        ),
        ("missing-key/word", &[("word = \"山猫\"\n", "")]),
        ("missing-key/renamed-lang", &[("lang = \"ja\"\n", "")]),
        (
            "wrong-type/meaning-int",
            &[("meaning = \"lynx / wildcat\"", "meaning = 1")],
        ),
    ];
    for (id, edits) in exceptions {
        corpus.record(
            format!("exception/{id}"),
            lexicon(ENTRY, &mutate(EXCEPTION, edits)),
        );
    }
    let trick_cases: &[(&str, &[(&str, &str)])] = &[
        (
            "unknown-key",
            &[("gated    = false\n", "gated    = false\nloud = true\n")],
        ),
        (
            "unknown-key/vocative",
            &[(
                "words = [\"kitty\"]\n",
                "words = [\"kitty\"]\ngated = false\ncjk = false\nnotes = \"n\"\nextra = 1\n",
            )],
        ),
        (
            "unknown-key/root",
            &[("[[filler]]", "[[noise]]\nlang = \"en\"\n[[filler]]")],
        ),
        ("missing-key/id", &[("id       = \"sit\"\n", "")]),
        (
            "missing-key/defaulted-words",
            &[("words    = [\"sit\", \"stay\"]\n", "")],
        ),
        (
            "wrong-type/gated-string",
            &[("gated    = false", "gated    = \"false\"")],
        ),
        (
            "wrong-type/address-string",
            &[("address  = [\"wait\"]", "address  = \"wait\"")],
        ),
        (
            "unknown-trick-id",
            &[("id       = \"sit\"", "id       = \"moonwalk\"")],
        ),
    ];
    for (id, edits) in trick_cases {
        corpus.record(format!("trick/{id}"), tricks(&mutate(TRICK, edits)));
    }
    corpus.finish();
}
