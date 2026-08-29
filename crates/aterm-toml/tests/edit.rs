// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The document half, driven through the exact surface aterm uses it for.
//!
//! Two consumers define that surface: the Preferences window's non-destructive
//! save (`prefs::apply_prefs_edits`), and the config editor's diagnostics
//! (`native_config_language`, which walks the tree and underlines spans). Each
//! operation below is checked against `toml_edit` doing the same thing, so the
//! swap is behaviour-preserving and not merely compiling.

use aterm_toml::edit::{DocumentMut, Item, Key, Table, Value};

const CONFIG: &str = "\
# aterm configuration
font_px = 12.0  # cozy
blink   = true

# networking
[net]
listen = \"127.0.0.1:7777\"  # local only

[[keybind]]
keys = \"ctrl+c\"
";

/// The prefs contract: change only the named keys, keep every other byte —
/// including the edited key's own trailing comment.
#[test]
fn a_non_destructive_edit_keeps_every_other_byte() {
    let ours = edit_ours(CONFIG);
    let theirs = edit_theirs(CONFIG);
    assert_eq!(ours, theirs, "our edit differs from toml_edit's");

    assert!(
        ours.contains("# aterm configuration"),
        "the header comment was lost"
    );
    assert!(ours.contains("# networking"), "a table's comment was lost");
    assert!(
        ours.contains("font_px = 18.0  # cozy"),
        "the edited key lost its inline comment:\n{ours}"
    );
    assert!(
        ours.contains("listen = \"0.0.0.0:9999\"  # local only"),
        "the nested edit lost its comment:\n{ours}"
    );
    assert!(
        !ours.contains("blink"),
        "the removed key is still there:\n{ours}"
    );
    assert!(
        ours.contains("timeout_ms = 500"),
        "the new nested key is missing:\n{ours}"
    );
    assert!(
        ours.contains("keys = \"ctrl+c\""),
        "an untouched array-of-tables entry changed:\n{ours}"
    );

    // And the result still means what it should.
    let value: aterm_toml::Value =
        aterm_toml::from_str(&ours).expect("the edit produced valid TOML");
    assert_eq!(
        value.get("font_px").and_then(aterm_toml::Value::as_float),
        Some(18.0)
    );
    assert_eq!(
        value
            .get("net")
            .and_then(|n| n.get("timeout_ms"))
            .and_then(aterm_toml::Value::as_integer),
        Some(500)
    );
}

fn edit_ours(source: &str) -> String {
    let mut doc: DocumentMut = source.parse().expect("parse");

    // Top-level replacement, adopting the old value's same-line decor.
    let mut item = Item::Value(Value::from(18.0));
    adopt_ours(doc.get("font_px"), &mut item);
    doc["font_px"] = item;

    doc.remove("blink");

    // Dotted-leaf replacement through the table chain.
    set_nested_ours(
        &mut doc,
        &["net", "listen"],
        Item::Value(Value::from("0.0.0.0:9999")),
    );
    set_nested_ours(
        &mut doc,
        &["net", "timeout_ms"],
        Item::Value(Value::from(500i64)),
    );

    doc.to_string()
}

fn adopt_ours(old: Option<&Item>, new: &mut Item) {
    let Some(old) = old.and_then(Item::as_value) else {
        return;
    };
    let Some(new) = new.as_value_mut() else {
        return;
    };
    let decor = old.decor().clone();
    if let Some(prefix) = decor.prefix() {
        new.decor_mut().set_prefix(prefix.clone());
    }
    if let Some(suffix) = decor.suffix() {
        new.decor_mut().set_suffix(suffix.clone());
    }
}

fn set_nested_ours(doc: &mut DocumentMut, path: &[&str], mut item: Item) {
    let (leaf, tables) = path.split_last().expect("a path has segments");
    let mut cur: &mut Item = doc.as_item_mut();
    for part in tables {
        let table = cur.as_table_like_mut().expect("a table on the way down");
        if table.get(part).is_none_or(Item::is_none) {
            let mut fresh = Table::new();
            fresh.set_implicit(true);
            table.insert(part, Item::Table(fresh));
        }
        cur = table.get_mut(part).expect("just ensured present");
    }
    let table = cur
        .as_table_like_mut()
        .expect("the leaf's parent is a table");
    adopt_ours(table.get(leaf), &mut item);
    table.insert(leaf, item);
}

fn edit_theirs(source: &str) -> String {
    let mut doc: toml_edit::DocumentMut = source.parse().expect("parse");

    let mut item = toml_edit::Item::Value(toml_edit::Value::from(18.0));
    adopt_theirs(doc.get("font_px"), &mut item);
    doc["font_px"] = item;

    doc.remove("blink");

    set_nested_theirs(
        &mut doc,
        &["net", "listen"],
        toml_edit::Item::Value(toml_edit::Value::from("0.0.0.0:9999")),
    );
    set_nested_theirs(
        &mut doc,
        &["net", "timeout_ms"],
        toml_edit::Item::Value(toml_edit::Value::from(500i64)),
    );

    doc.to_string()
}

fn adopt_theirs(old: Option<&toml_edit::Item>, new: &mut toml_edit::Item) {
    let Some(old) = old.and_then(toml_edit::Item::as_value) else {
        return;
    };
    let Some(new) = new.as_value_mut() else {
        return;
    };
    let decor = old.decor().clone();
    if let Some(prefix) = decor.prefix() {
        new.decor_mut().set_prefix(prefix.clone());
    }
    if let Some(suffix) = decor.suffix() {
        new.decor_mut().set_suffix(suffix.clone());
    }
}

fn set_nested_theirs(doc: &mut toml_edit::DocumentMut, path: &[&str], mut item: toml_edit::Item) {
    let (leaf, tables) = path.split_last().expect("a path has segments");
    let mut cur: &mut toml_edit::Item = doc.as_item_mut();
    for part in tables {
        let table = cur.as_table_like_mut().expect("a table on the way down");
        if table.get(part).is_none_or(toml_edit::Item::is_none) {
            let mut fresh = toml_edit::Table::new();
            fresh.set_implicit(true);
            table.insert(part, toml_edit::Item::Table(fresh));
        }
        cur = table.get_mut(part).expect("just ensured present");
    }
    let table = cur
        .as_table_like_mut()
        .expect("the leaf's parent is a table");
    adopt_theirs(table.get(leaf), &mut item);
    table.insert(leaf, item);
}

/// The diagnostics surface: a dotted lookup, and a span that points at the
/// authored token.
///
/// This is the one place the replacement is strictly MORE capable than the
/// crate it replaces, and the difference is measured here rather than claimed.
/// `str::parse::<toml_edit::DocumentMut>()` parses into an immutable document
/// and then despans it on the way to the mutable one, so EVERY `Item::span()`
/// on a `toml_edit::DocumentMut` is `None` — which is why
/// `native_config_language` carries a hand-rolled `source_value_range` that
/// re-finds tokens in the source text. This crate keeps spans on the mutable
/// document, so that fallback stops being load-bearing.
#[test]
fn a_dotted_lookup_finds_the_value_and_keeps_its_span() {
    let doc: DocumentMut = CONFIG.parse().expect("parse");

    let item = dotted_item(&doc, "net.listen").expect("net.listen is present");
    assert_eq!(item.as_str(), Some("127.0.0.1:7777"));
    let span = item.span().expect("a parsed value keeps its span");
    assert_eq!(&CONFIG[span], "\"127.0.0.1:7777\"");

    let font = dotted_item(&doc, "font_px").expect("font_px is present");
    assert_eq!(&CONFIG[font.span().expect("span")], "12.0");
    let table = doc.get("net").and_then(Item::as_table).expect("[net]");
    assert_eq!(&CONFIG[table.span().expect("a header has a span")], "[net]");

    // The same walk over the oracle finds the same VALUE and no span at all.
    let theirs: toml_edit::DocumentMut = CONFIG.parse().expect("parse");
    let their_item = dotted_item_theirs(&theirs, "net.listen").expect("the oracle finds it too");
    assert_eq!(their_item.as_str(), item.as_str(), "the values must agree");
    assert!(
        their_item.span().is_none(),
        "toml_edit 0.22 despans DocumentMut; if that changed, revisit this note"
    );

    assert_eq!(
        dotted_item(&doc, "font_px").and_then(Item::as_float),
        Some(12.0)
    );
    assert_eq!(
        dotted_item(&doc, "blink").and_then(Item::as_bool),
        Some(true)
    );
    assert!(dotted_item(&doc, "net.missing").is_none());
    assert!(dotted_item(&doc, "font_px.deeper").is_none());
}

fn dotted_item<'a>(doc: &'a DocumentMut, key: &str) -> Option<&'a Item> {
    let mut item = doc.as_item();
    for segment in Key::parse(key).ok()? {
        item = item.as_table_like()?.get(segment.get())?;
    }
    Some(item)
}

fn dotted_item_theirs<'a>(
    doc: &'a toml_edit::DocumentMut,
    key: &str,
) -> Option<&'a toml_edit::Item> {
    let mut item = doc.as_item();
    for segment in toml_edit::Key::parse(key).ok()? {
        item = item.as_table_like()?.get(segment.get())?;
    }
    Some(item)
}

/// `Key::parse` is a parser, not a `split('.')`: a dot inside a quoted segment
/// is part of the name.
#[test]
fn key_parsing_and_canonical_spelling_match_the_oracle() {
    for expression in [
        "a",
        "a.b.c",
        " a . b ",
        "\"quoted key\"",
        "site.\"google.com\"",
        "'literal.key'.tail",
        "1234",
    ] {
        let ours = Key::parse(expression).unwrap_or_else(|e| panic!("{expression:?}: {e}"));
        let theirs = toml_edit::Key::parse(expression)
            .unwrap_or_else(|e| panic!("oracle {expression:?}: {e}"));
        let ours: Vec<&str> = ours.iter().map(Key::get).collect();
        let theirs: Vec<&str> = theirs.iter().map(toml_edit::Key::get).collect();
        assert_eq!(ours, theirs, "{expression:?}");
    }
    for bad in ["a.", ".a", "a..b", "a b", "", "a.'unterminated"] {
        assert!(Key::parse(bad).is_err(), "{bad:?} is not a key expression");
        assert!(
            toml_edit::Key::parse(bad).is_err(),
            "the oracle accepts {bad:?}"
        );
    }

    // The canonical escaped spelling, used to build config key paths.
    for (raw, spelled) in [
        ("plain", "plain"),
        ("with space", "\"with space\""),
        ("with.dot", "\"with.dot\""),
        ("", "\"\""),
    ] {
        assert_eq!(Key::new(raw).to_string(), spelled);
        assert_eq!(toml_edit::Key::new(raw).to_string(), spelled);
    }
}

/// The tree-walking surface: every arm of `Item`, both table spellings, and the
/// type names diagnostics print.
#[test]
fn the_walking_surface_reports_what_the_oracle_reports() {
    let source = "\
scalar = 1
arr = [\"a\", \"b\"]
mixed = [\"a\", 1]
inline = { x = 1 }

[explicit]
k = true

[implicit.child]
k = 1

[[aot]]
k = 1

[[aot]]
k = 2
";
    let doc: DocumentMut = source.parse().expect("parse");

    assert!(doc.get("missing").is_none());
    assert!(
        doc["missing"].is_none(),
        "indexing an absent key must vivify to the vacant item"
    );
    assert!(matches!(doc.get("scalar"), Some(Item::Value(_))));
    assert!(matches!(doc.get("explicit"), Some(Item::Table(_))));
    assert!(matches!(doc.get("aot"), Some(Item::ArrayOfTables(_))));

    // An implicit parent knows it was never authored.
    let implicit = doc
        .get("implicit")
        .and_then(Item::as_table)
        .expect("implicit table");
    assert!(
        implicit.is_implicit(),
        "`[implicit.child]` should leave `implicit` implicit"
    );
    let explicit = doc
        .get("explicit")
        .and_then(Item::as_table)
        .expect("explicit table");
    assert!(!explicit.is_implicit());

    // Arrays, and the all-strings predicate the config editor uses.
    let arr = doc.get("arr").and_then(Item::as_array).expect("an array");
    assert!(arr.iter().all(Value::is_str));
    let mixed = doc.get("mixed").and_then(Item::as_array).expect("an array");
    assert!(!mixed.iter().all(Value::is_str));
    assert!(arr.span().is_some());

    // Inline tables answer the table-like questions too.
    let inline = doc.get("inline").expect("inline");
    assert!(inline.is_table_like());
    assert_eq!(inline.get("x").and_then(Item::as_integer), Some(1));
    assert_eq!(
        inline
            .as_value()
            .and_then(Value::as_inline_table)
            .map(aterm_toml::edit::InlineTable::len),
        Some(1)
    );

    let aot = doc
        .get("aot")
        .and_then(Item::as_array_of_tables)
        .expect("an array of tables");
    assert_eq!(aot.len(), 2);
    assert_eq!(
        aot.iter()
            .filter_map(|t| t.get("k"))
            .filter_map(Item::as_integer)
            .sum::<i64>(),
        3
    );

    // Type names, as printed in diagnostics.
    let theirs: toml_edit::DocumentMut = source.parse().expect("parse");
    for key in ["scalar", "arr", "inline", "explicit", "aot"] {
        let ours = doc.get(key).expect("present");
        let their = theirs.get(key).expect("present");
        let same_kind = match (ours, their) {
            (Item::Value(a), toml_edit::Item::Value(b)) => a.type_name() == b.type_name(),
            (Item::Table(_), toml_edit::Item::Table(_)) => true,
            (Item::ArrayOfTables(_), toml_edit::Item::ArrayOfTables(_)) => true,
            _ => false,
        };
        assert!(same_kind, "{key}: kinds disagree");
    }

    // Top-level iteration order is the authored order.
    let keys: Vec<&str> = doc.iter().map(|(k, _)| k).collect();
    assert_eq!(
        keys,
        [
            "scalar", "arr", "mixed", "inline", "explicit", "implicit", "aot"
        ]
    );
}

/// Interleaved dotted keys are the case a tree-shaped model gets wrong: the
/// three lines share one `net` node, and grouping them would rewrite a file the
/// save was supposed to leave alone.
#[test]
fn interleaved_dotted_keys_keep_their_authored_order() {
    let source = "net.listen = \"a\"\nfont_px = 12\nnet.timeout_ms = 5\n";
    let doc: DocumentMut = source.parse().expect("parse");
    assert_eq!(doc.to_string(), source);

    let mut doc: DocumentMut = source.parse().expect("parse");
    set_nested_ours(
        &mut doc,
        &["net", "timeout_ms"],
        Item::Value(Value::from(9i64)),
    );
    assert_eq!(
        doc.to_string(),
        "net.listen = \"a\"\nfont_px = 12\nnet.timeout_ms = 9\n"
    );
}

/// A brand-new document built from nothing has to print legal TOML, since that
/// is what the serializer does on every `to_string`.
#[test]
fn a_document_built_by_hand_prints_legal_toml() {
    let mut doc = DocumentMut::new();
    doc["name"] = Item::Value(Value::from("aterm"));
    doc["list"] = Item::Value(Value::Array(["a", "b"].into_iter().collect()));
    let mut table = Table::new();
    table.insert("nested", Item::Value(Value::from(1i64)));
    doc["section"] = Item::Table(table);

    let printed = doc.to_string();
    assert_eq!(
        printed,
        "name = \"aterm\"\nlist = [\"a\", \"b\"]\n\n[section]\nnested = 1\n"
    );
    let value: aterm_toml::Value = aterm_toml::from_str(&printed).expect("valid TOML");
    assert_eq!(
        value
            .get("section")
            .and_then(|s| s.get("nested"))
            .and_then(aterm_toml::Value::as_integer),
        Some(1)
    );
}

/// `Value::from_str` — the surface `native_settings` counts a list with.
///
/// Its contract is STRICT, and that was measured rather than assumed: the
/// oracle refuses ` 1 `, `1\n`, `# c\n1` and `1 # c`. A value is the whole
/// input or the input is not a value. Both implementations are asserted here,
/// so the day the oracle changes its mind this test says so instead of
/// silently blessing a divergence.
#[test]
fn a_standalone_value_parses_exactly_as_toml_edit_parses_it() {
    const CASES: &[&str] = &[
        // Accepted by both.
        "1",
        "-0",
        "1.5",
        "inf",
        "true",
        "\"a\"",
        "'x'",
        "\"\"\"a\"\"\"",
        "[1, 2]",
        "[\n  1,\n  2,\n]",
        "[]",
        "{ a = 1 }",
        "{}",
        "{ a.b = 1 }",
        "1979-05-27",
        "1979-05-27T07:32:00Z",
        "07:32:00",
        // Refused by both — trivia is not part of a value.
        " 1 ",
        "1\n",
        "\n1",
        "# c\n1",
        "1 # c",
        "1 # c\n",
        "\t[1]\t",
        "[1]\n\n",
        "",
        "  ",
        // Refused by both — not one value.
        "1 2",
        "a = 1",
        "1\nx = 2",
        "[1,",
        "{ a = 1",
    ];

    for case in CASES {
        let ours = case.parse::<Value>();
        let theirs = case.parse::<toml_edit::Value>();
        assert_eq!(
            ours.is_ok(),
            theirs.is_ok(),
            "verdict differs on {case:?}: ours={:?} toml_edit={:?}",
            ours.as_ref()
                .map(ToString::to_string)
                .map_err(ToString::to_string),
            theirs
                .as_ref()
                .map(ToString::to_string)
                .map_err(ToString::to_string),
        );
        if let (Ok(ours), Ok(theirs)) = (&ours, &theirs) {
            assert_eq!(
                ours.to_string(),
                theirs.to_string(),
                "rendering differs on {case:?}"
            );
            assert_eq!(
                ours.to_string(),
                *case,
                "a standalone value must print back byte-for-byte: {case:?}"
            );
        }
        if let Err(e) = &ours {
            let span = e.span().expect("a refusal carries a span");
            assert!(
                span.start <= case.len() && span.end <= case.len(),
                "span {span:?} is outside {case:?}"
            );
        }
    }
}

/// `Item::is_*` — the predicates `native_config_language` types a setting with.
/// Every kind, against the oracle, in both polarities.
#[test]
fn item_kind_predicates_agree_with_toml_edit() {
    const SOURCE: &str = "\
s = \"x\"
i = 1
f = 1.5
b = true
d = 1979-05-27
arr = [1]
inline = { a = 1 }

[table]
k = 1

[[aot]]
k = 1
";
    let ours: DocumentMut = SOURCE.parse().expect("parse");
    let theirs: toml_edit::DocumentMut = SOURCE.parse().expect("oracle parses");

    // NOT "absent": indexing a key that is not there is the one place these
    // two disagree, and deliberately. `toml_edit` panics; `Item::None` is this
    // crate's vacant state, so `doc["absent"]` yields a place to write. That is
    // asserted below rather than smuggled past the comparison.
    for key in ["s", "i", "f", "b", "d", "arr", "inline", "table", "aot"] {
        let a = &ours[key];
        let b = &theirs[key];
        let ours_kinds = [
            a.is_none(),
            a.is_value(),
            a.is_str(),
            a.is_integer(),
            a.is_float(),
            a.is_bool(),
            a.is_datetime(),
            a.is_array(),
            a.is_table(),
            a.is_table_like(),
            a.is_array_of_tables(),
        ];
        let their_kinds = [
            b.is_none(),
            b.is_value(),
            b.is_str(),
            b.is_integer(),
            b.is_float(),
            b.is_bool(),
            b.is_datetime(),
            b.is_array(),
            b.is_table(),
            b.is_table_like(),
            b.is_array_of_tables(),
        ];
        assert_eq!(
            ours_kinds, their_kinds,
            "the kind predicates disagree for `{key}`"
        );
    }

    // The divergence, pinned in both directions so it cannot drift unnoticed.
    let vacant = &ours["absent"];
    assert!(vacant.is_none(), "a missing key is the vacant entry");
    assert!(!vacant.is_value() && !vacant.is_str() && !vacant.is_table_like());
    assert!(
        std::panic::catch_unwind(|| {
            let theirs: toml_edit::DocumentMut = SOURCE.parse().expect("oracle parses");
            let _ = theirs["absent"].is_none();
        })
        .is_err(),
        "toml_edit is expected to panic here — if it stopped, this divergence is gone"
    );
}
