// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

fn read(text: &str) -> Source {
    Source::read(text.to_string()).expect("the fixture lexes")
}

/// The shipped tokens' text, joined by spaces.
fn shipped(text: &str) -> String {
    read(text)
        .shipped()
        .iter()
        .map(|tok| tok.text)
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn a_test_module_and_its_attributes_leave_the_shipped_tokens() {
    let src = "pub fn f() -> u8 { 1 }\n\n/// The tests.\n#[cfg(test)]\n// why\nmod tests {\n    #[test]\n    fn t() { assert_eq!(super::f(), 1); }\n}\n";
    assert_eq!(shipped(src), "pub fn f ( ) - > u8 { 1 }");
    let source = read(src);
    assert!(source.test_only(src.find("/// The").unwrap()));
    assert!(source.test_only(src.find("assert_eq").unwrap()));
    assert!(!source.test_only(src.find("{ 1 }").unwrap()));
}

#[test]
fn a_seam_both_builds_compile_ships() {
    for cfg in [
        "any(test, debug_assertions)",
        "not(test)",
        "any(test, feature = \"x\")",
    ] {
        let src = format!("#[cfg({cfg})]\nfn seam() {{}}\n");
        assert_eq!(
            shipped(&src),
            format!("# [ cfg ( {} ) ] fn seam ( ) {{ }}", spaced(cfg)),
            "{cfg}"
        );
    }
    for cfg in [
        "test",
        "all(test, unix)",
        "all(unix, test, feature = \"x\")",
    ] {
        let src = format!("#[cfg({cfg})]\nfn only() {{}}\n");
        assert_eq!(shipped(&src), "", "{cfg}");
    }
}

/// `cfg` text as the tokenizer splits it.
fn spaced(cfg: &str) -> String {
    cfg.replace('(', " ( ")
        .replace(')', " ) ")
        .replace(',', " , ")
        .replace('=', " = ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn code_after_a_test_module_on_its_own_line_ships() {
    // A line or indentation masker reads the shipped function as part of the module.
    let src = "#[cfg(test)] mod t { fn a() {} } pub fn shipped() -> u8 { 1 }\n#[cfg(test)]\nmod u {\nfn b() {}\n}\npub fn after() {}\n";
    assert_eq!(
        shipped(src),
        "pub fn shipped ( ) - > u8 { 1 } pub fn after ( ) { }"
    );
}

#[test]
fn delimiters_inside_literals_and_comments_nest_nothing() {
    let src = "#[cfg(test)]\nmod tests {\n    const A: &str = \"}\";\n    const B: char = '}';\n    const C: &str = r#\"} \" }\"#;\n    // }\n    /* } /* } */ } */\n    fn f<'a>(x: &'a str) -> &'a str { x }\n}\npub fn kept() {}\n";
    assert_eq!(shipped(src), "pub fn kept ( ) { }");
}

#[test]
fn test_items_inside_inert_containers_are_found() {
    let src = "pub mod outer {\n    #[cfg(test)]\n    fn helper() {}\n    pub fn f() {}\n}\nimpl S {\n    #[cfg(test)]\n    fn probe(&self) {}\n}\n#[allow(dead_code)]\ntrait T {\n    #[cfg(test)]\n    fn hidden();\n}\n";
    assert_eq!(
        shipped(src),
        "pub mod outer { pub fn f ( ) { } } impl S { } # [ allow ( dead_code ) ] trait T { }"
    );
}

#[test]
fn an_attribute_macro_around_a_test_item_keeps_it_shipped() {
    // The macro receives `helper` before its `cfg` is read and may keep it in any build.
    let src = "#[some_macro]\nmod outer {\n    #[cfg(test)]\n    fn helper() {}\n}\n";
    assert!(shipped(src).contains("helper"));
    let inner = "mod outer {\n    #![some_macro]\n    #[cfg(test)]\n    fn helper() {}\n}\n";
    assert!(shipped(inner).contains("helper"));
    let file = "#![some_macro]\n#[cfg(test)]\nfn helper() {}\n";
    assert!(shipped(file).contains("helper"));
    // Negative control: inert attributes on the container and the file read on.
    let inert = "#![allow(dead_code)]\n#[allow(unused)]\nmod outer {\n    #![allow(dead_code)]\n    #[cfg(test)]\n    fn helper() {}\n}\n";
    assert!(!shipped(inert).contains("helper"));
}

#[test]
fn a_cfg_attr_never_makes_an_item_test_only() {
    let src = "#[cfg_attr(test, derive(Debug))]\nstruct S;\n#[cfg_attr(feature = \"x\", cfg(test))]\nfn maybe() {}\n";
    let out = shipped(src);
    assert!(out.contains("struct S"), "{out}");
    assert!(out.contains("fn maybe"), "{out}");
}

#[test]
fn a_test_only_inner_attribute_makes_the_file_test_only() {
    let src = "//! Fixtures.\n#![cfg(test)]\nmod nested;\npub fn f() {}\n";
    let source = read(src);
    assert!(source.shipped().is_empty());
    assert!(source.mounts().iter().all(|mount| mount.test));
    let inline = "pub mod m {\n    #![cfg(all(test, unix))]\n    pub fn f() {}\n}\npub fn g() {}\n";
    assert_eq!(shipped(inline), "pub fn g ( ) { }");
}

#[test]
fn every_item_kind_ends_where_its_grammar_says() {
    let src = "#[cfg(test)] use a::{b, c};\n#[cfg(test)] const X: u8 = { 1 };\n#[cfg(test)] static Y: [u8; 1] = [0];\n#[cfg(test)] struct T(u8);\n#[cfg(test)] enum E { A }\n#[cfg(test)] type Z = u8;\n#[cfg(test)] macro_rules! m { () => {} }\n#[cfg(test)] m!(x);\n#[cfg(test)] other::m! { y }\n#[cfg(test)] extern crate alloc;\n#[cfg(test)] pub(crate) const unsafe extern \"C\" fn c() {}\n#[cfg(test)] impl<T: Fn() -> u8> W<T> where T: Copy { fn w() {} }\npub fn kept() {}\n";
    assert_eq!(shipped(src), "pub fn kept ( ) { }");
}

/// A `const trait` or `const impl` ends at its body, never at a later item's `;`.
#[test]
fn a_const_trait_ends_at_its_body() {
    for kind in ["trait T", "impl T for S"] {
        let src = format!(
            "#[cfg(test)]\nconst {kind} {{ fn f() {{}} }}\npub fn shipped() {{}}\nuse x::y;\n"
        );
        assert_eq!(
            shipped(&src),
            "pub fn shipped ( ) { } use x : : y ;",
            "{kind}"
        );
    }
}

#[test]
fn an_item_never_reads_past_its_first_possible_end() {
    // The braced const argument ends the item early; what follows reads as shipped.
    let src = "#[cfg(test)]\nfn f<const N: usize>() -> W<{ N }> { W }\n";
    assert_eq!(shipped(src), "> { W }");
}

#[test]
fn shipped_tokens_carry_their_place() {
    let src = "#[cfg(test)]\nmod tests {}\n  pub fn f() {}\n";
    let source = read(src);
    let first = source.shipped()[0];
    assert_eq!((first.text, first.line, first.before), ("pub", 3, "  "));
    // Characters of another width before a token change its column as rustc counts it, whatever
    // the bytes.
    let narrow = read("#[cfg(test)] const S: &str = \"ab\"; pub fn f() -> u32 { column!() }\n");
    let wide = read("#[cfg(test)] const S: &str = \"\u{e9}\"; pub fn f() -> u32 { column!() }\n");
    assert_ne!(narrow.shipped(), wide.shipped());
}

#[test]
fn module_declarations_name_their_file() {
    let src = "mod plain;\n#[cfg(test)]\n#[path = \"plain_tests.rs\"]\nmod tests;\n#[cfg_attr(unix, path = \"u.rs\")]\nmod plat;\n#[path = \"a.rs\"]\n#[path = \"b.rs\"]\nmod twice;\nmacro_rules! m { ($n:ident) => { mod $n; }; }\nfn f() { mod inner; }\n#[cfg(test)]\nmod t {\n    mod deep;\n}\nmod r#type;\npub mod outer {\n    mod nested;\n}\n";
    let source = read(src);
    let got: Vec<_> = source
        .mounts()
        .iter()
        .map(|m| (m.name.as_deref(), m.path.clone(), m.test, m.top))
        .collect();
    assert_eq!(
        got,
        [
            (Some("plain"), MountPath::Default, false, true),
            (
                Some("tests"),
                MountPath::Literal("plain_tests.rs".to_string()),
                true,
                true
            ),
            (Some("plat"), MountPath::Unsettled, false, true),
            (Some("twice"), MountPath::Unsettled, false, true),
            (Some("type"), MountPath::Default, false, true),
            (Some("nested"), MountPath::Default, false, false),
            (None, MountPath::Unsettled, false, false),
            (Some("inner"), MountPath::Unsettled, false, false),
            (Some("deep"), MountPath::Unsettled, true, false),
        ]
    );
}

#[test]
fn a_module_declaration_an_attribute_macro_may_rewrite_is_unsettled() {
    let source = read("#[some_macro]\nmod outer {\n    #[path = \"x.rs\"]\n    mod x;\n}\n");
    assert_eq!(source.mounts()[0].path, MountPath::Unsettled);
    // Negative control: inert attributes, a tool's among them, leave it placed.
    let inert = read(
        "#[allow(unused)]\n#[rustfmt::skip]\nmod outer {\n    #[path = \"x.rs\"]\n    mod x;\n}\n",
    );
    assert_eq!(
        inert.mounts()[0].path,
        MountPath::Literal("x.rs".to_string())
    );
}

#[test]
fn includes_say_what_they_read() {
    let src = "const A: &str = include_str!(\"a.txt\");\ninclude!(concat!(env!(\"OUT_DIR\"), \"/gen.rs\"));\nconst B: &[u8] = include_bytes!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/data/x.bin\"));\nmacro_rules! fixture { ($p:literal) => { include_str!($p) }; }\nconst C: &str = include_str!(concat!(env!(\"HOME\"), \"/x\"));\n#[cfg(test)]\nconst D: &str = include_str!(\"d.txt\");\n";
    let source = read(src);
    let got: Vec<_> = source
        .includes()
        .iter()
        .map(|inc| {
            (
                inc.kind,
                inc.arg.clone(),
                inc.in_macro,
                source.test_only(inc.at),
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            (
                IncludeKind::Text,
                IncludeArg::Relative("a.txt".to_string()),
                false,
                false
            ),
            (IncludeKind::Tokens, IncludeArg::OutDir, false, false),
            (
                IncludeKind::Bytes,
                IncludeArg::ManifestDir("/data/x.bin".to_string()),
                false,
                false
            ),
            (IncludeKind::Text, IncludeArg::Unknown, true, false),
            (IncludeKind::Text, IncludeArg::Unknown, false, false),
            (
                IncludeKind::Text,
                IncludeArg::Relative("d.txt".to_string()),
                false,
                true
            ),
        ]
    );
}

#[test]
fn string_values_decode_cooked_and_raw_literals() {
    let source = read(
        "#[path = \"m.rs\"]\nmod m;\nconst I: &str = include_str!(concat!(env!(\"OUT_DIR\"), \"/i\"));\nconst A: &str = \"cargo:rerun-if-changed=src/a.rs\";\nconst B: &str = r#\"x\"y\"#;\nconst C: &str = \"\\u{41}\\x42\\\n    c\";\nconst D: &str = \"\\q\";\n",
    );
    let got: Vec<_> = source.strings().map(|(value, _)| value).collect();
    assert_eq!(
        got,
        [
            Some("cargo:rerun-if-changed=src/a.rs"),
            Some("x\"y"),
            Some("ABc"),
            None
        ]
    );
}

#[test]
fn what_does_not_lex_is_not_read() {
    assert!(Source::read("const A: &str = \"open;\n".to_string()).is_none());
    assert!(Source::read("/* open /* nested */\nfn f() {}\n".to_string()).is_none());
    assert!(Source::read("fn f() { (] }\n".to_string()).is_none());
    assert!(Source::read("fn f() {\n".to_string()).is_none());
    assert!(Source::read("fn f() {} }\n".to_string()).is_none());
}

#[test]
fn raw_identifiers_and_lifetimes_are_single_tokens() {
    let src = "fn r#type<'a>(x: &'a u8) -> u8 { 1.5f64 as u8 + x.0 }\n";
    let source = read(src);
    let texts: Vec<_> = source.shipped().iter().map(|tok| tok.text).collect();
    assert!(texts.contains(&"r#type"), "{texts:?}");
    assert!(texts.contains(&"'a"), "{texts:?}");
    assert!(texts.contains(&"1.5f64"), "{texts:?}");
    assert!(!texts.iter().any(|t| t.contains("..")));
    let source = read("const R: core::ops::Range<u8> = 1..2;\n");
    let range: Vec<_> = source
        .shipped()
        .iter()
        .map(|tok| tok.text)
        .collect::<Vec<_>>();
    assert!(
        range.windows(4).any(|w| w == ["1", ".", ".", "2"]),
        "{range:?}"
    );
}
