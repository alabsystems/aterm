// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Editable TOML document (`aterm_toml::edit`) tests.

use aterm_toml::edit::{DocumentMut, Item, Key, Table, TableLike, Value};

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

#[test]
fn a_non_destructive_edit_keeps_every_other_byte() {
    let ours = edit_ours(CONFIG);
    assert_eq!(
        ours,
        "\
# aterm configuration
font_px = 18.0  # cozy

# networking
[net]
listen = \"0.0.0.0:9999\"  # local only
timeout_ms = 500

[[keybind]]
keys = \"ctrl+c\"
"
    );

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

    let mut item = Item::Value(Value::from(18.0));
    adopt_ours(doc.get("font_px"), &mut item);
    doc["font_px"] = item;

    doc.remove("blink");

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

#[test]
fn a_removed_line_keeps_the_comment_lines_above_it() {
    let mut doc: DocumentMut = "\
# rain
rain.on = true  # bright
font_px = 12.0

[net]
listen = \"x\"
# old port
port = 1

[ui]
a = 1
"
    .parse()
    .expect("parse");
    assert!(doc.remove_path(&["rain", "on"]).is_some());
    assert!(doc.remove_path(&["net", "port"]).is_some());
    set_nested_ours(
        &mut doc,
        &["net", "extra", "k"],
        Item::Value(Value::from(1i64)),
    );
    assert_eq!(
        doc.to_string(),
        "\
# rain
font_px = 12.0

[net]
listen = \"x\"
# old port

[net.extra]
k = 1

[ui]
a = 1
"
    );
}

#[test]
fn a_removal_prunes_the_tables_it_left_bare() {
    fn pruned(text: &str, path: &[&str]) -> String {
        let mut doc: DocumentMut = text.parse().expect("parse");
        assert!(
            doc.remove_path_pruning(path).is_some(),
            "{path:?} in {text:?}"
        );
        doc.to_string()
    }
    assert_eq!(
        pruned(
            "font_px = 12.0\n\n[sparkle_words.feline]\nenabled = false\n",
            &["sparkle_words", "feline", "enabled"]
        ),
        "font_px = 12.0\n"
    );
    assert_eq!(
        pruned(
            "[update]\nauto_apply = false\n[machine]\nx = 1\n",
            &["update", "auto_apply"]
        ),
        "[machine]\nx = 1\n"
    );
    assert_eq!(pruned("a.b.c = 1\nd = 2\n", &["a", "b", "c"]), "d = 2\n");
    assert_eq!(
        pruned("rain = { on = true }\nx = 1\n", &["rain", "on"]),
        "x = 1\n"
    );
    assert_eq!(
        pruned(
            "[s.feline]\nenabled = false\n[s.profanity]\nenabled = false\n",
            &["s", "feline", "enabled"]
        ),
        "[s.profanity]\nenabled = false\n"
    );
    let text = "x = 1\n\n[s]\nenabled = false\n\n[s.feline]\nenabled = false\n";
    let mut doc: DocumentMut = text.parse().expect("parse");
    assert!(doc.remove_path_pruning(&["s", "enabled"]).is_some());
    assert_eq!(doc.to_string(), "x = 1\n\n[s.feline]\nenabled = false\n");
    set_nested_ours(&mut doc, &["s", "enabled"], Item::Value(Value::from(false)));
    assert_eq!(doc.to_string(), text);
    assert_eq!(
        pruned(
            "# mine\n[s]\nenabled = false\n[s.feline]\nenabled = false\n",
            &["s", "enabled"]
        ),
        "# mine\n[s]\n[s.feline]\nenabled = false\n",
        "a commented header stays"
    );
    for text in [
        "# mine\n[rain]\non = true\n",
        "[rain] # mine\non = true\n",
        "[rain]\n# mine\non = true\n",
    ] {
        let out = pruned(text, &["rain", "on"]);
        assert!(
            out.contains("[rain]") && out.contains("# mine"),
            "{text:?} -> {out:?}"
        );
    }
    let out = pruned("rain = { on = true } # mine\n", &["rain", "on"]);
    assert!(out.contains("# mine"), "{out:?}");
    let mut doc: DocumentMut = "[rain]\n".parse().expect("parse");
    assert!(doc.remove_path_pruning(&["rain", "on"]).is_none());
    assert_eq!(doc.to_string(), "[rain]\n");
}

#[test]
fn the_comment_lines_under_a_tables_last_line_are_that_tables() {
    fn pruned(text: &str, path: &[&str]) -> String {
        let mut doc: DocumentMut = text.parse().expect("parse");
        assert!(
            doc.remove_path_pruning(path).is_some(),
            "{path:?} in {text:?}"
        );
        doc.to_string()
    }
    let text = "[machine]\nu = 1\n[s.feline]\nenabled = false\n# kitties\n\n[harness]\nh = 1\n";
    assert_eq!(
        pruned(text, &["s", "feline", "enabled"]),
        "[machine]\nu = 1\n[s.feline]\n# kitties\n\n[harness]\nh = 1\n"
    );
    let at_end = "[machine]\nu = 1\n[s.feline]\nenabled = false\n# kitties\n";
    assert_eq!(
        pruned(at_end, &["s", "feline", "enabled"]),
        "[machine]\nu = 1\n[s.feline]\n# kitties\n"
    );
    for original in [text, at_end] {
        let mut doc: DocumentMut = original.parse().expect("parse");
        doc.remove_path_pruning(&["s", "feline", "enabled"]);
        set_nested_ours(
            &mut doc,
            &["s", "feline", "enabled"],
            Item::Value(Value::from(false)),
        );
        assert_eq!(doc.to_string(), original);
    }
    let mut doc: DocumentMut = text.parse().expect("parse");
    set_nested_ours(
        &mut doc,
        &["s", "emphasis", "enabled"],
        Item::Value(Value::from(false)),
    );
    let out = doc.to_string();
    assert!(
        out.contains("enabled = false\n# kitties\n")
            && out.find("# kitties") < out.find("[s.emphasis]"),
        "{out:?}"
    );
    assert_eq!(
        pruned("z = 0\n[a]\nx = 1\n\n# about b\n[b]\ny = 1\n", &["a", "x"]),
        "z = 0\n\n# about b\n[b]\ny = 1\n"
    );
    assert_eq!(
        pruned("z = 0\n[a]\nx = 1\n# about b\n[b]\ny = 1\n", &["a", "x"]),
        "z = 0\n# about b\n[b]\ny = 1\n",
        "with no blank line the comment is the header's"
    );
    for shape in [
        text,
        at_end,
        "x = 1\n# root note\n\n[a]\ny = 2\n",
        "[[r]]\nn = 1\n# first record\n\n[[r]]\nn = 2\n# last\n",
        "a = 1\n# end without newline",
        "[a]\n# only a comment\n\n[b]\n",
    ] {
        let doc: DocumentMut = shape.parse().expect("parse");
        assert_eq!(doc.to_string(), shape);
    }
}

#[test]
fn a_byte_order_mark_stays_first() {
    fn print(doc: &DocumentMut) -> String {
        let text = doc.to_string();
        aterm_toml::from_str::<aterm_toml::Value>(&text).expect("it still parses");
        text
    }
    let mut doc: DocumentMut = "\u{feff}font_px = 14.0\nx = 1\n".parse().expect("parse");
    assert!(doc.remove("font_px").is_some());
    assert_eq!(print(&doc), "\u{feff}x = 1\n");

    let mut doc: DocumentMut =
        "\u{feff}# my aterm settings\nfont_px = 14.0\n\n[update]\nenabled = false\n"
            .parse()
            .expect("parse");
    assert!(doc.remove("font_px").is_some());
    let reset = print(&doc);
    assert_eq!(
        reset,
        "\u{feff}# my aterm settings\n\n[update]\nenabled = false\n"
    );
    let mut doc: DocumentMut = reset.parse().expect("parse");
    doc["font_px"] = Item::Value(Value::from(14.0));
    assert_eq!(
        print(&doc),
        "\u{feff}font_px = 14.0\n# my aterm settings\n\n[update]\nenabled = false\n"
    );

    let mut doc: DocumentMut = "\u{feff}# pkgs\npackages.auto_update = false\n[update]\n"
        .parse()
        .expect("parse");
    set_nested_ours(
        &mut doc,
        &["packages", "enabled"],
        Item::Value(Value::from(true)),
    );
    assert!(doc.remove_path(&["packages", "auto_update"]).is_some());
    assert_eq!(
        print(&doc),
        "\u{feff}packages.enabled = true\n# pkgs\n[update]\n"
    );
}

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

#[test]
fn key_parsing_and_canonical_spelling() {
    for (expression, segments) in [
        ("a", &["a"][..]),
        ("a.b.c", &["a", "b", "c"]),
        (" a . b ", &["a", "b"]),
        ("\"quoted key\"", &["quoted key"]),
        ("site.\"google.com\"", &["site", "google.com"]),
        ("'literal.key'.tail", &["literal.key", "tail"]),
        ("1234", &["1234"]),
    ] {
        let ours = Key::parse(expression).unwrap_or_else(|e| panic!("{expression:?}: {e}"));
        let ours: Vec<&str> = ours.iter().map(Key::get).collect();
        assert_eq!(ours, segments, "{expression:?}");
    }
    for bad in ["a.", ".a", "a..b", "a b", "", "a.'unterminated"] {
        assert!(Key::parse(bad).is_err(), "{bad:?} is not a key expression");
    }

    for (raw, spelled) in [
        ("plain", "plain"),
        ("with space", "\"with space\""),
        ("with.dot", "\"with.dot\""),
        ("", "\"\""),
    ] {
        assert_eq!(Key::new(raw).to_string(), spelled);
    }
}

#[test]
fn the_walking_surface() {
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
        "shared Index on an absent key returns the static vacant item"
    );
    assert!(doc.get("missing").is_none());
    assert!(matches!(doc.get("scalar"), Some(Item::Value(_))));
    assert!(matches!(doc.get("explicit"), Some(Item::Table(_))));
    assert!(matches!(doc.get("aot"), Some(Item::ArrayOfTables(_))));

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

    let arr = doc.get("arr").and_then(Item::as_array).expect("an array");
    assert!(arr.iter().all(Value::is_str));
    let mixed = doc.get("mixed").and_then(Item::as_array).expect("an array");
    assert!(!mixed.iter().all(Value::is_str));
    assert!(arr.span().is_some());

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

    let keys: Vec<&str> = doc.iter().map(|(k, _)| k).collect();
    assert_eq!(
        keys,
        [
            "scalar", "arr", "mixed", "inline", "explicit", "implicit", "aot"
        ]
    );
}

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

#[test]
fn a_standalone_value_is_the_whole_input() {
    const ACCEPTED: &[&str] = &[
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
    ];
    const REFUSED: &[&str] = &[
        " 1 ", "1\n", "\n1", "# c\n1", "1 # c", "1 # c\n", "\t[1]\t", "[1]\n\n", "", "  ", "1 2",
        "a = 1", "1\nx = 2", "[1,", "{ a = 1",
    ];

    for case in ACCEPTED {
        let ours = case
            .parse::<Value>()
            .unwrap_or_else(|e| panic!("{case:?}: {e}"));
        assert_eq!(
            ours.to_string(),
            *case,
            "a standalone value must print back byte-for-byte: {case:?}"
        );
    }
    for case in REFUSED {
        let e = case
            .parse::<Value>()
            .expect_err(&format!("{case:?} is not a value"));
        let span = e.span().expect("a refusal carries a span");
        assert!(
            span.start <= case.len() && span.end <= case.len(),
            "span {span:?} is outside {case:?}"
        );
    }
}

#[test]
fn item_kind_predicates() {
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

    const F: bool = false;
    const T: bool = true;
    for (key, expected) in [
        ("s", [F, T, T, F, F, F, F, F, F, F, F]),
        ("i", [F, T, F, T, F, F, F, F, F, F, F]),
        ("f", [F, T, F, F, T, F, F, F, F, F, F]),
        ("b", [F, T, F, F, F, T, F, F, F, F, F]),
        ("d", [F, T, F, F, F, F, T, F, F, F, F]),
        ("arr", [F, T, F, F, F, F, F, T, F, F, F]),
        ("inline", [F, T, F, F, F, F, F, F, F, T, F]),
        ("table", [F, F, F, F, F, F, F, F, T, T, F]),
        ("aot", [F, F, F, F, F, F, F, F, F, F, T]),
    ] {
        let a = &ours[key];
        let kinds = [
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
        assert_eq!(kinds, expected, "the kind predicates for `{key}`");
    }

    let vacant = &ours["absent"];
    assert!(vacant.is_none(), "a missing key is the vacant entry");
    assert!(!vacant.is_value() && !vacant.is_str() && !vacant.is_table_like());
}

#[test]
fn a_key_added_after_an_unterminated_final_line_starts_its_own_line() {
    let cases: &[(&str, &[&str], &[&str])] = &[
        ("a = 1", &["b"], &["a"]),
        ("a = 1 # note", &["b"], &["a"]),
        ("theme = \"dark\" # my fav", &["cursor_blink"], &["theme"]),
        ("[t]\nx = 1 # c", &["t", "y"], &["t", "x"]),
        (
            "[net]\nlisten = \"x\"",
            &["net", "timeout_ms"],
            &["net", "listen"],
        ),
        ("a = 1\r\nb = 2", &["c"], &["b"]),
        ("[t]", &["t", "x"], &[]),
        ("[t] # c", &["t", "x"], &[]),
    ];
    for &(source, new, old) in cases {
        let mut doc: DocumentMut = source.parse().expect("parse");
        assert_eq!(
            doc.to_string(),
            source,
            "unedited, the document is the source"
        );
        set_nested_ours(&mut doc, new, Item::Value(Value::from(7i64)));
        let printed = doc.to_string();
        assert!(
            printed.starts_with(source),
            "the source's bytes changed: {source:?} printed {printed:?}"
        );

        let value: aterm_toml::Value = aterm_toml::from_str(&printed)
            .unwrap_or_else(|e| panic!("{source:?} printed {printed:?}, which is not TOML: {e}"));
        let lookup = |path: &[&str]| path.iter().try_fold(&value, |at, part| at.get(*part));
        assert_eq!(
            lookup(new).and_then(aterm_toml::Value::as_integer),
            Some(7),
            "{source:?} printed {printed:?}: the new key is lost"
        );
        assert!(
            lookup(old).is_some(),
            "{source:?} printed {printed:?}: the old key is lost"
        );
    }
}

#[test]
fn a_table_added_after_an_unterminated_final_line_starts_its_own_line() {
    for source in ["a = 1", "a = 1 # note", "[s]\nx = 1", "[[k]]\nx = 1 # c"] {
        for compact in [false, true] {
            let mut doc: DocumentMut = source.parse().expect("parse");
            let mut table = Table::new();
            table.insert("y", Item::Value(Value::from(2i64)));
            if compact {
                TableLike::decor_mut(&mut table).set_prefix("");
            }
            match doc.as_table_mut().get_mut("k") {
                Some(Item::ArrayOfTables(aot)) => aot.push(table),
                _ => doc["t"] = Item::Table(table),
            }
            let printed = doc.to_string();
            assert!(
                printed.starts_with(source),
                "{source:?} printed {printed:?}"
            );

            let value: aterm_toml::Value = aterm_toml::from_str(&printed)
                .unwrap_or_else(|e| panic!("{source:?} printed {printed:?}: {e}"));
            let added = match value.get("k") {
                Some(k) => {
                    assert_eq!(k[0]["x"].as_integer(), Some(1), "{printed:?}");
                    k.get(1)
                }
                None => value.get("t"),
            };
            assert_eq!(
                added
                    .and_then(|t| t.get("y"))
                    .and_then(aterm_toml::Value::as_integer),
                Some(2),
                "{source:?} printed {printed:?}"
            );
        }
    }
}

#[test]
fn a_nested_write_through_an_inline_table_lands_in_it() {
    let source = "# sparkles\nsparkle_words = { lexicon = \"my.txt\" } # keep\nfont_px = 12\n";
    let mut doc: DocumentMut = source.parse().expect("parse");
    set_nested_ours(
        &mut doc,
        &["sparkle_words", "profanity", "enabled"],
        Item::Value(Value::from(false)),
    );
    let printed = doc.to_string();
    assert_ne!(printed, source, "the write was dropped");
    for untouched in [
        "# sparkles\nsparkle_words = { lexicon = \"my.txt\" ",
        "} # keep\nfont_px = 12\n",
    ] {
        assert!(
            printed.contains(untouched),
            "{untouched:?} changed: {printed:?}"
        );
    }

    let value: aterm_toml::Value =
        aterm_toml::from_str(&printed).unwrap_or_else(|e| panic!("{printed:?} is not TOML: {e}"));
    let words = value.get("sparkle_words").expect("sparkle_words");
    assert_eq!(
        words.get("lexicon").and_then(aterm_toml::Value::as_str),
        Some("my.txt")
    );
    assert_eq!(
        words
            .get("profanity")
            .and_then(|p| p.get("enabled"))
            .and_then(aterm_toml::Value::as_bool),
        Some(false),
        "the leaf is missing from {printed:?}"
    );
    assert_eq!(
        value.get("font_px").and_then(aterm_toml::Value::as_integer),
        Some(12)
    );

    let mut doc: DocumentMut = source.parse().expect("parse");
    let words = doc
        .as_table_mut()
        .get_mut("sparkle_words")
        .and_then(Item::as_table_like_mut)
        .expect("an inline table");
    let mut inner = Table::new();
    inner.insert("n", Item::Value(Value::from(3i64)));
    let mut element = Table::new();
    element.insert("inner", Item::Table(inner));
    let mut aot = aterm_toml::edit::ArrayOfTables::new();
    aot.push(element);
    words.insert("packs", Item::ArrayOfTables(aot));
    let printed = doc.to_string();
    let value: aterm_toml::Value =
        aterm_toml::from_str(&printed).unwrap_or_else(|e| panic!("{printed:?} is not TOML: {e}"));
    assert_eq!(
        value
            .get("sparkle_words")
            .and_then(|w| w.get("packs"))
            .and_then(|p| p.get(0))
            .and_then(|e| e.get("inner"))
            .and_then(|i| i.get("n"))
            .and_then(aterm_toml::Value::as_integer),
        Some(3),
        "the array of tables is missing from {printed:?}"
    );
}
