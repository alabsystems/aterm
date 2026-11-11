// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn scanned(files: &[(&str, &str)]) -> Inventory {
    let mut inv = Inventory::default();
    for (rel, src) in files {
        inv.scan(rel, src);
    }
    inv
}

#[test]
fn the_lexer_skips_comments_and_keeps_string_contents() {
    let toks = lex("a /* x /* y */ z */ // c\n r#\"q\"# 'b' 'life \"e\\\"s\" 1.5f32");
    let texts: Vec<(Kind, &str, usize)> = toks
        .iter()
        .map(|t| (t.kind, t.text.as_str(), t.line))
        .collect();
    assert_eq!(
        texts,
        vec![
            (Kind::Ident, "a", 1),
            (Kind::Str, "q", 2),
            (Kind::Lit, "'b'", 2),
            (Kind::Lit, "'life", 2),
            (Kind::Str, "e\\\"s", 2),
            (Kind::Lit, "1.5f32", 2),
        ]
    );
}

#[test]
fn items_are_filed_by_where_they_sit() {
    let inv = scanned(&[(
        "crates/x/src/lib.rs",
        "#[derive(Serialize, Deserialize)]\n\
         #[serde(rename_all = \"snake_case\", deny_unknown_fields)]\n\
         pub enum E {\n\
             #[serde(rename = \"a\")]\n\
             A { #[serde(default)] x: u8 },\n\
             B(#[serde(skip)] u8),\n\
         }\n\
         struct S { #[cfg_attr(test, serde(default = \"f\"))] pub f: u8 }\n\
         impl<'de> serde::Deserialize<'de> for S {}\n",
    )]);
    let keys: Vec<(Position, &str, &str)> = inv
        .rows
        .keys()
        .map(|(p, k, f)| (*p, k.as_str(), f.as_str()))
        .collect();
    assert_eq!(
        keys,
        vec![
            (Position::Container, "deny_unknown_fields", "bare"),
            (Position::Container, "rename_all", "= \"snake_case\""),
            (Position::Variant, "rename", "= <string>"),
            (Position::Field, "default", "= <string>"),
            (Position::Field, "default", "bare"),
            (Position::Field, "skip", "bare"),
        ]
    );
    assert_eq!(
        inv.rows[&(Position::Field, "default".into(), "= <string>".into())],
        vec![("crates/x/src/lib.rs:8".to_string(), true)]
    );
    assert!(inv.combos.contains_key(&(
        Position::Container,
        "deny_unknown_fields + rename_all = \"snake_case\"".to_string()
    )));
    assert_eq!(
        inv.derive_sites.keys().cloned().collect::<Vec<_>>(),
        vec![
            ("Deserialize".to_string(), "enum".to_string()),
            ("Serialize".to_string(), "enum".to_string())
        ]
    );
    assert_eq!(inv.manual["Deserialize"], vec!["crates/x/src/lib.rs:9"]);
}

#[test]
fn nested_forms_and_test_paths() {
    let inv = scanned(&[
        (
            "crates/x/tests/serde_test.rs",
            "struct S { #[serde(with(a = \"b\", c))] f: u8 }",
        ),
        ("crates/x/src/broken_eof.rs", "#[serde(with("),
        ("crates/x/src/shebang_eof.rs", "#!"),
        (
            "crates/x/src/broken_item.rs",
            "#[serde(with(a) b)] struct B { r#struct: u8, #[serde(r#type = \"T\")] f: u8 }",
        ),
    ]);
    let rendered = inv.render("crates/");
    assert!(rendered.contains(
        "field\twith\t(a = <string>, c bare)\t1\t1\t0\tcrates/x/tests/serde_test.rs:1\n"
    ));
    assert!(
        rendered.contains("container\twith\t?( a ) b\t1\t0\t0\tcrates/x/src/broken_item.rs:1\n")
    );
    assert!(rendered.contains("field\ttype\t= <string>\t1\t0\t0\tcrates/x/src/broken_item.rs:1\n"));
    assert!(test_path("crates/a/src/foo_tests.rs") && !test_path("crates/a/src/latest.rs"));
}

#[test]
fn the_implementation_is_listed_only_when_named() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    for (rel, src) in [
        (
            "crates/aterm-serde/src/lib.rs",
            "struct I { #[serde(skip)] f: u8 }",
        ),
        (
            "crates/x/src/lib.rs",
            "struct M { #[serde(default)] f: u8 }",
        ),
    ] {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, src).expect("write");
    }
    let default = inventory(root, &[]).expect("default tree");
    let named = inventory(root, &["crates/aterm-serde".into()]).expect("named tree");
    assert!(
        default.contains("\tdefault\t") && !default.contains("\tskip\t"),
        "{default}"
    );
    assert!(default.starts_with(
        "# serde attribute inventory: every #[serde(...)] key and form under crates/\n"
    ));
    assert!(
        named.contains("\tskip\t") && !named.contains("\tdefault\t"),
        "{named}"
    );
    assert!(
        named.contains("form under crates/aterm-serde/\n"),
        "{named}"
    );
}

#[cfg(unix)]
#[test]
fn rs_files_ignores_symlinked_rs_files() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    std::fs::write(root.join("real.rs"), "struct S;\n").expect("real.rs");
    std::os::unix::fs::symlink(root.join("missing.rs"), root.join("dangling.rs")).expect("symlink");
    let mut out = Vec::new();
    rs_files(root, None, &mut out).expect("walk");
    assert_eq!(out, vec![root.join("real.rs")]);
}
