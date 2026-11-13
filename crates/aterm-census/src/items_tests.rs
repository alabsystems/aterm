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
        .code()
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
    assert!(source.code().is_empty());
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
    let first = source.code()[0];
    assert_eq!((first.text, first.line, first.before), ("pub", 3, "  "));
    // Characters of another width before a token change its column as rustc counts it, whatever
    // the bytes.
    let narrow = read("#[cfg(test)] const S: &str = \"ab\"; pub fn f() -> u32 { column!() }\n");
    let wide = read("#[cfg(test)] const S: &str = \"\u{e9}\"; pub fn f() -> u32 { column!() }\n");
    assert_ne!(narrow.code(), wide.code());
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
    let texts: Vec<_> = source.code().iter().map(|tok| tok.text).collect();
    assert!(texts.contains(&"r#type"), "{texts:?}");
    assert!(texts.contains(&"'a"), "{texts:?}");
    assert!(texts.contains(&"1.5f64"), "{texts:?}");
    assert!(!texts.iter().any(|t| t.contains("..")));
    let source = read("const R: core::ops::Range<u8> = 1..2;\n");
    let range: Vec<_> = source.code().iter().map(|tok| tok.text).collect::<Vec<_>>();
    assert!(
        range.windows(4).any(|w| w == ["1", ".", ".", "2"]),
        "{range:?}"
    );
}

/// A comment edit on its own line leaves the code and its every place. CONTROL: a doc that grows a
/// line moves the code after it, and an inline comment that changes before code on its line
/// moves that code along it.
#[test]
fn code_drops_comments_and_keeps_every_place() {
    let base = "//! File.\n/// Doc.\npub fn f() -> u8 {\n    // why\n    1\n}\n";
    for edited in [
        base.replace("//! File.", "//! The file."),
        base.replace("/// Doc.", "/// The doc."),
        base.replace("// why", "// because"),
        base.replace("/// Doc.", "// Doc."),
    ] {
        assert_eq!(read(base).code(), read(&edited).code(), "{edited}");
        assert_ne!(read(base).comments(), read(&edited).comments(), "{edited}");
    }
    let longer = base.replace("/// Doc.", "/// Doc.\n/// More.");
    assert_ne!(read(base).code(), read(&longer).code());
    let inline = "pub fn f() -> u8 { /* a */ 1 }\n";
    assert_ne!(
        read(inline).code(),
        read(&inline.replace("/* a */", "/* bb */")).code()
    );
}

/// Each comment says how many code tokens come before it; those of test-only items are not
/// listed.
#[test]
fn comments_say_where_they_sit_among_the_code() {
    let src = "//! File.\n/// Doc.\npub fn f() {} // after\n//// plain\n/** block */\nfn g() {}\n#[cfg(test)]\nmod tests {\n    // hidden\n}\n";
    let source = read(src);
    let got: Vec<_> = source
        .comments()
        .iter()
        .map(|c| (c.at.text, c.at.line, c.gap))
        .collect();
    assert_eq!(
        got,
        [
            ("//! File.", 1, 0),
            ("/// Doc.", 2, 0),
            ("// after", 3, 7),
            ("//// plain", 4, 7),
            ("/** block */", 5, 7),
        ]
    );
}

/// Whether the comment holding `needle` in `src` is one a macro may read.
fn carried(src: &str, needle: &str) -> bool {
    let source = read(src);
    let found: Vec<_> = source
        .comments()
        .into_iter()
        .filter(|c| c.at.text.contains(needle))
        .collect();
    assert_eq!(found.len(), 1, "{needle} is one comment of {src}");
    found[0].carried
}

/// A comment a macro may read is carried, one case per carrier, each beside the same comment
/// without it.
#[test]
fn a_comment_a_macro_may_read_is_carried() {
    for (with, without) in [
        // Macro invocation arguments, `macro_rules!` and `macro` bodies, attribute brackets.
        (
            "fn f() { m!(1, /* c */ 2); }\n",
            "fn f() { g(1, /* c */ 2); }\n",
        ),
        (
            "m! {\n    /// c\n    struct S;\n}\n",
            "mod m {\n    /// c\n    struct S;\n}\n",
        ),
        (
            "aterm_spec::ty_model! {\n    // c\n    x\n}\n",
            "fn f() {\n    // c\n}\n",
        ),
        (
            "macro_rules! n {\n    () => {\n        // c\n    };\n}\n",
            "fn n() {\n    // c\n}\n",
        ),
        ("macro n() {\n    // c\n}\n", "fn n() {\n    // c\n}\n"),
        (
            "#[allow(/* c */ dead_code)]\nfn f() {}\n",
            "#[allow(dead_code)]\nfn f() { /* c */ }\n",
        ),
        // A derived item, its fields and variants, alone or under `cfg_attr`.
        (
            "/// c\n#[derive(Debug)]\nstruct S;\n",
            "/// c\n#[must_use]\nstruct S;\n",
        ),
        (
            "#[derive(Debug)]\nstruct S {\n    /// c\n    a: u8,\n}\n",
            "struct S {\n    /// c\n    a: u8,\n}\n",
        ),
        (
            "#[derive(Debug)]\nenum E {\n    /// c\n    A,\n}\n",
            "enum E {\n    /// c\n    A,\n}\n",
        ),
        (
            "/// c\n#[cfg_attr(feature = \"x\", derive(Debug))]\nstruct S;\n",
            "/// c\n#[cfg_attr(feature = \"x\", allow(dead_code))]\nstruct S;\n",
        ),
        // An attribute this reader cannot vouch for, on a fn, its body included.
        (
            "/// c\n#[x::y]\nfn f() {}\n",
            "/// c\n#[rustfmt::skip]\nfn f() {}\n",
        ),
        (
            "#[cfg_attr(any(test, debug_assertions), aterm_spec_anchor::refines(m))]\nfn f() {\n    // c\n}\n",
            "#[cfg_attr(any(test, debug_assertions), trust::skip)]\nfn f() {\n    // c\n}\n",
        ),
        // `doc = …` on the item; the list form does not carry.
        (
            "/// c\n#[doc = \"more\"]\nfn f() {}\n",
            "/// c\n#[doc(hidden)]\nfn f() {}\n",
        ),
        // A local derived item in a body the item reader did not read.
        (
            "fn f() {\n    /// c\n    #[derive(Debug)]\n    struct L;\n}\n",
            "fn f() {\n    /// c\n    struct L;\n}\n",
        ),
        // A method under an attribute macro, inside an impl the reader read.
        (
            "impl S {\n    /// c\n    #[x::y]\n    fn f() {}\n}\n",
            "impl S {\n    /// c\n    fn f() {}\n}\n",
        ),
        // A file whose inner attribute carries.
        (
            "#![x::y]\n// c\nfn f() {}\n",
            "#![allow(unused)]\n// c\nfn f() {}\n",
        ),
        // An item the reader cannot end leaves the rest of its level unread.
        ("struct\n// c\n", "struct S;\n// c\n"),
        // A local derived item a block deeper in a body the item reader did not read.
        (
            "fn f() {\n    if true {\n        #[derive(Debug)]\n        struct L {\n            /// c\n            a: u8,\n        }\n    }\n}\n",
            "fn f() {\n    if true {\n        struct L {\n            /// c\n            a: u8,\n        }\n    }\n}\n",
        ),
        // A Trust `clean { … }` island, whose text the Clean front end reads: a span that lexes as
        // a Rust comment there is code to it. At the top of a file, and in a body the item reader
        // did not read.
        (
            "clean {\n    -- glob src/*.rs\n    def c (x : UInt64) : UInt64 := x + 1\n    -- end */\n}\n",
            "fn clean() {\n    -- glob src/*.rs\n    def c (x : UInt64) : UInt64 := x + 1\n    -- end */\n}\n",
        ),
        (
            "fn f() {\n    clean {\n        /* c */\n    }\n}\n",
            "fn f() {\n    if x {\n        /* c */\n    }\n}\n",
        ),
    ] {
        assert!(carried(with, "c"), "{with}");
        assert!(!carried(without, "c"), "{without}");
    }
}

/// A carrier reaches past the first `{…}` that ends an item early. CONTROL: the same struct with no
/// derive carries nothing.
#[test]
fn a_carrier_reaches_its_items_real_end() {
    let derived = "#[derive(Debug)]\nstruct S<const N: usize = { 1 }> {\n    /// d\n    a: [u8; N],\n}\n// after\n";
    assert!(carried(derived, "/// d"));
    assert!(!carried(derived, "// after"));
    assert!(!carried(
        &derived.replace("#[derive(Debug)]\n", ""),
        "/// d"
    ));
    // A `->` closes no `<`: a braced const default after one is still inside the generics.
    let arrow = "#[derive(Debug)]\nstruct S<F = fn() -> u8, const N: usize = { 1 }> {\n    /// d\n    a: F,\n}\n";
    assert!(carried(arrow, "/// d"));
    assert!(!carried(&arrow.replace("#[derive(Debug)]\n", ""), "/// d"));
    // A const whose value is a block ends at its `;`.
    let block = "#[x::y]\nconst C: u8 = { 1 } + {\n    // tail\n    2\n};\n";
    assert!(carried(block, "// tail"));
}

/// A module declaration says whether a macro may read its module. CONTROL: inert attributes leave
/// it unread.
#[test]
fn mounts_say_whether_a_macro_may_read_them() {
    let source = read(
        "#[x::y]\nmod a;\n#[allow(unused)]\n#[path = \"b.rs\"]\nmod b;\n#[cfg_attr(unix, derive(Debug))]\nmod c;\nmacro_rules! m { () => { mod d; }; }\n",
    );
    let got: Vec<_> = source
        .mounts()
        .iter()
        .map(|m| (m.name.as_deref(), m.carried))
        .collect();
    assert_eq!(
        got,
        [
            (Some("a"), true),
            (Some("b"), false),
            (Some("c"), true),
            (None, true)
        ]
    );
}

/// Each test module's name, enclosing inline modules, and the text of its item, head and body.
fn modules(text: &str) -> Vec<(String, Vec<String>, String, String, String)> {
    read(text)
        .test_modules()
        .iter()
        .map(|m| {
            (
                m.name.clone(),
                m.within.clone(),
                text[m.item.0..m.item.1].to_string(),
                text[m.head..m.open].to_string(),
                text[m.body.0..m.body.1].to_string(),
            )
        })
        .collect()
}

/// An inline test module is recorded with its docs and attributes, its visibility and name, and
/// a body that is exactly what its braces hold.
///
/// NEGATIVE CONTROLS: recording an item whose `cfg` is not test-only adds `seam`; reading the name
/// from the brace group's own position gives `{` instead of `tests`.
#[test]
fn an_inline_test_module_is_recorded_with_its_attributes_head_and_body() {
    let src = "pub fn f() -> u8 { 1 }\n\n/// The tests.\n#[cfg(test)]\n#[allow(dead_code)]\npub(crate) mod tests {\n    use super::f;\n}\n\n#[cfg(any(test, debug_assertions))]\nmod seam {}\n\n#[cfg(all(test, unix))]\nmod r#unix {}\n";
    let got = modules(src);
    assert_eq!(got.len(), 2, "{got:?}");
    let (name, within, item, head, body) = &got[0];
    assert_eq!(name, "tests");
    assert!(within.is_empty());
    assert!(item.starts_with("/// The tests.\n#[cfg(test)]") && item.ends_with("use super::f;\n}"));
    assert_eq!(head, "pub(crate) mod tests ");
    assert_eq!(body, "\n    use super::f;\n");
    assert_eq!(got[1].0, "unix");
}

/// A test module inside a shipped inline module names that module, and says whether one of them
/// carries `#[path]`; one inside a test-only item, inside a container an attribute macro may
/// rewrite, or in a literal or macro body is no module of its own.
///
/// NEGATIVE CONTROLS: recursing into test-only items adds `inner`; treating a non-inert attribute
/// as inert adds `rewritten`; reading literals as code adds `quoted`; reading `#[path]` off the
/// test module rather than its containers marks `moved` unpathed, and reading only outer
/// attributes marks `inside` unpathed.
#[test]
fn only_outermost_test_modules_of_shipped_containers_are_recorded() {
    let src = "mod a {\n    mod b {\n        #[cfg(test)]\n        mod deep {}\n    }\n}\n#[path = \"elsewhere\"]\nmod p {\n    #[cfg(test)]\n    mod moved {}\n}\nmod i {\n    #![path = \"x\"]\n    #[cfg(test)]\n    mod inside {}\n}\n#[cfg(test)]\nmod outer {\n    #[cfg(test)]\n    mod inner {}\n}\n#[some::rewrite]\nmod m {\n    #[cfg(test)]\n    mod rewritten {}\n}\nconst S: &str = \"#[cfg(test)] mod quoted {}\";\nmacro_rules! gen {\n    () => {\n        #[cfg(test)]\n        mod generated {}\n    };\n}\n";
    let source = read(src);
    let got: Vec<_> = source
        .test_modules()
        .iter()
        .map(|m| (m.name.as_str(), m.within.join("::"), m.pathed))
        .collect();
    assert_eq!(
        got,
        [
            ("deep", "a::b".to_string(), false),
            ("moved", "p".to_string(), true),
            ("inside", "i".to_string(), true),
            ("outer", String::new(), false),
        ]
    );
}

/// A file whose inner attributes make it test-only says so, and `code_in` reads the code tokens
/// of a range, comments left out.
///
/// NEGATIVE CONTROL: keeping comment tokens in `code_in` adds `// note`.
#[test]
fn whole_files_and_the_code_of_a_range_are_read() {
    assert!(read("#![cfg(test)]\nfn f() {}\n").whole());
    assert!(!read("#![allow(dead_code)]\nfn f() {}\n").whole());
    let src = "fn f() {\n    // note\n    g(\"}\");\n}\n";
    let source = read(src);
    let start = src.find('{').unwrap();
    assert_eq!(
        source.code_in(start, src.len()),
        ["{", "g", "(", "\"}\"", ")", ";", "}"]
    );
}

/// A test module gated by its own inner `#![cfg(test)]` is recorded as one whose `mod` line is not
/// gated, and one carrying an attribute a macro may read says so.
///
/// NEGATIVE CONTROLS: recording only outer gates loses `inner`; reading every attribute as inert
/// marks `read` as not rewritten.
#[test]
fn inner_gated_and_macro_read_test_modules_say_so() {
    let src = "mod inner {\n    #![cfg(test)]\n    fn t() {}\n}\n#[cfg(test)]\n#[some::wrap]\nmod read {}\n\
               #[cfg(test)]\n#[allow(dead_code)]\nmod plain {}\n";
    let source = read(src);
    let got: Vec<_> = source
        .test_modules()
        .iter()
        .map(|m| (m.name.as_str(), m.inner_gate, m.rewritten))
        .collect();
    assert_eq!(
        got,
        [
            ("inner", true, false),
            ("read", false, true),
            ("plain", false, false)
        ]
    );
}
