// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::super::run_lazy_init_census;

const PURE: &str = "pub fn read(_cache: &std::cell::OnceCell<bool>) -> bool { false }\n";
const REENTER: &str = "pub fn read(cache: &std::cell::OnceCell<bool>) -> bool {\n    *cache.get_or_init(|| true)\n}\n";

fn check(
    label: &str,
    prelude: &str,
    expression: &str,
    provider: &str,
    extra: &[(&str, &str)],
    red: bool,
) {
    let mut files = vec![
        ("Cargo.toml".into(), "[workspace]\nmembers = [\"crates/aterm-gui\", \"crates/parser-impl\"]\n".into()),
        ("crates/aterm-gui/Cargo.toml".into(), "[package]\nname = \"aterm-gui\"\n[dependencies]\nparser = { package = \"parser-implementation\", path = \"../parser-impl\" }\n".into()),
        ("crates/parser-impl/Cargo.toml".into(), "[package]\nname = \"parser-implementation\"\n".into()),
        ("crates/aterm-gui/src/lib.rs".into(), format!("{prelude}\nstruct Cache {{\n    cache: std::cell::OnceCell<bool>,\n}}\nimpl Cache {{\n    fn read(&self) -> bool {{\n        *self.cache.get_or_init({expression})\n    }}\n}}\n")),
        ("crates/parser-impl/src/lib.rs".into(), provider.into()),
    ];
    files.extend(
        extra
            .iter()
            .map(|(path, body)| ((*path).into(), (*body).into())),
    );
    let tree = crate::scan_set::test_fixtures::synth_tree("lazy-qualified-controls", label, &files);
    let out = run_lazy_init_census(&tree);
    assert_eq!(out.ok, !red, "{label}: {}", out.log);
}

#[test]
fn parenthesized_dependency_paths_preserve_context_and_cell_arguments() {
    for (label, provider, red) in [("pure", PURE, false), ("reentry", REENTER, true)] {
        check(
            label,
            "",
            "|| (\n ((parser :: read))\n)(&self.cache)",
            provider,
            &[],
            red,
        );
    }
}

#[test]
fn joined_calls_still_exclude_each_declaration_head() {
    let scopes = super::DependencyScopes::new();
    let exports = super::Exports::new();
    let scope = super::CallScope::new("crates/demo/src/lib.rs", "", &[], &scopes, &exports);
    let body = vec![
        "fn outer() {".into(),
        "    fn nested() {}".into(),
        "    (actual)();".into(),
        "}".into(),
    ];
    let calls = scope.callees(&body);
    assert!(!calls.contains("outer"));
    assert!(!calls.contains("nested"));
    assert!(calls.contains("actual"));
}

#[test]
fn computed_callees_are_not_promoted_to_direct_paths() {
    for expression in [
        "factory(reenter)",
        "(factory(reenter))",
        "factory!(reenter)",
        "factory::<bool>(reenter)",
        "maybe_factory()?(reenter)",
        "(reenter, pure)",
        "(reenter as fn() -> bool)",
        "(reenter + other)",
        "(reenter)(pure)",
    ] {
        assert!(super::direct_callee(expression).is_none(), "{expression}");
    }
    for expression in [
        "(reenter)",
        "(((crate :: reenter)))",
        "(\n reenter\n)",
        "!(reenter)",
        "if !(reenter)",
        "break 'done !(reenter)",
    ] {
        assert!(super::direct_callee(expression).is_some(), "{expression}");
    }
}

#[test]
fn qualified_calls_preserve_real_and_conservative_reentry() {
    for (label, prelude, expression) in [
        ("crate", "", "|| crate::read(self)"),
        ("self", "", "|| self::read(self)"),
        ("Self", "", "|| Self::read(self)"),
        ("unknown", "", "|| unknown::read(self)"),
        (
            "module",
            "mod parser {\n    pub fn read(cache: &super::Cache) -> bool { cache.read() }\n}",
            "|| parser::read(self)",
        ),
        (
            "import",
            "use crate::local as parser;\nmod local {\n    pub fn read(cache: &super::Cache) -> bool { cache.read() }\n}",
            "|| parser::read(self)",
        ),
        (
            "type",
            "type parser = Local;\nstruct Local;\nimpl Local {\n    fn read(cache: &Cache) -> bool { cache.read() }\n}",
            "|| parser::read(self)",
        ),
        (
            "fn-generic",
            "fn forward<parser: Capture>(cache: &Cache) -> bool {\n    parser::read(cache)\n}\ntrait Capture {}",
            "|| parser::read(self)",
        ),
        (
            "impl-generic",
            "struct Forward<T>(T);\nimpl<parser: Capture> Forward<parser> {\n    fn call(cache: &Cache) -> bool { parser::read(cache) }\n}\ntrait Capture {}",
            "|| parser::read(self)",
        ),
        (
            "mixed-local-external",
            "",
            "|| { parser::read(&self.cache); self.read() }",
        ),
        ("deep-path", "", "|| parser::nested::read(self)"),
    ] {
        check(label, prelude, expression, PURE, &[], true);
    }
}

#[test]
fn only_known_public_free_exports_narrow_calls_and_function_references() {
    for (label, provider, red) in [
        ("public", PURE, false),
        ("borrowed-cell", REENTER, true),
        ("private", "fn read() -> bool { false }\n", true),
        ("missing", "pub fn other() -> bool { false }\n", true),
        (
            "foreign-reexport",
            "pub use elsewhere::read;\nfn read() -> bool { false }\n",
            true,
        ),
        ("glob-reexport", "pub use elsewhere::*;\n", true),
    ] {
        check(
            label,
            "",
            "|| parser::read(&self.cache)",
            provider,
            &[],
            red,
        );
    }
    for (label, provider, red) in [
        ("reference-pure", "pub fn read() -> bool { false }\n", false),
        (
            "reference-reenters",
            "pub fn read() -> bool {\n    let cache: &std::cell::OnceCell<bool> = unreachable!();\n    *cache.get_or_init(|| true)\n}\n",
            true,
        ),
    ] {
        check(label, "", "parser::read", provider, &[], red);
    }
    for (label, implementation, red) in [
        ("local-export-pure", PURE, false),
        ("local-export-borrowed", REENTER, true),
    ] {
        check(
            label,
            "",
            "|| parser::read(&self.cache)",
            "mod parsing;\npub use parsing::read;\n",
            &[("crates/parser-impl/src/parsing.rs", implementation)],
            red,
        );
    }
}

/// Each way a file shadows an alias, judged by the one-pass [`super::Shadows`] (and, for
/// an alias that is not one word, by the full scan it falls back to) exactly as the full
/// scan [`super::unshadowed`] judges it.
#[test]
fn every_shadowing_shape_is_judged_per_alias() {
    for (text, alias, unshadowed) in [
        ("fn f() { parser::read(); }\n", "parser", true),
        ("fn f() { parser :: read(); }\n", "parser", true),
        ("fn f() { other::read(); }\n", "parser", true),
        (
            "// parser\nfn f() { parser::read(); } // parser alone\n",
            "parser",
            true,
        ),
        ("fn f() { parsers::read(); parser_x(); }\n", "parser", true),
        ("fn f() { éparser::read(); parseré(); }\n", "parser", true),
        ("use crate::local as parser;\n", "parser", false),
        ("fn f() { x::parser::read(); }\n", "parser", false),
        (
            "macro_rules! m { () => { $parser::read() }; }\n",
            "parser",
            false,
        ),
        ("fn f() { parser é::read(); }\n", "parser", false),
        (
            "use parser::{self, Read};\nfn f() { parser::read(); }\n",
            "parser",
            false,
        ),
        (
            "use parser::{\n    self,\n};\nfn f() { parser::read(); }\n",
            "parser",
            false,
        ),
        (
            "use other::{self};\nfn f() { parser::read(); }\n",
            "parser",
            true,
        ),
        (
            "use other::*;\nfn f() { parser::read(); }\n",
            "parser",
            false,
        ),
        (
            "fn f() { parser::read(); }\nuse other::thing",
            "parser",
            false,
        ),
        ("fn f() { a-b::read(); }\n", "a-b", true),
        ("fn f() { let x = a-b; }\n", "a-b", false),
        ("use a-b::{self};\n", "a-b", false),
    ] {
        let code: Vec<&str> = text.lines().map(crate::strip_line_comment).collect();
        let words = super::WordSet::new([alias].into_iter().filter(|a| super::word(a)));
        let shadows = super::Shadows::of(&code, &words);
        assert_eq!(
            shadows.unshadowed(text, alias),
            unshadowed,
            "{alias:?} in {text:?}"
        );
        assert_eq!(
            super::unshadowed(text, alias),
            unshadowed,
            "{alias:?} in {text:?}, by the full scan"
        );
    }
}

#[test]
fn a_renamed_borrowed_cell_cannot_be_proved_pure() {
    check(
        "renamed-borrow",
        "",
        "|| parser::read(&self.cache)",
        "pub fn read(borrowed: &std::cell::OnceCell<bool>) -> bool {\n    *borrowed.get_or_init(|| true)\n}\n",
        &[],
        true,
    );
}

#[test]
fn a_same_file_helper_cannot_hide_a_borrowed_cell() {
    check(
        "helper-borrow",
        "",
        "|| parser::read(&self.cache)",
        "pub fn read(borrowed: &std::cell::OnceCell<bool>) -> bool {\n    fill(borrowed)\n}\nfn fill(borrowed: &std::cell::OnceCell<bool>) -> bool {\n    *borrowed.get_or_init(|| true)\n}\n",
        &[],
        true,
    );
}

#[test]
fn a_comment_cannot_prove_a_macro_generated_export() {
    check(
        "comment-export",
        "",
        "|| parser::read(&self.cache)",
        "/*\npub fn read() -> bool { false }\n*/\nmake_reader!();\n",
        &[],
        true,
    );
}

#[test]
fn a_multiline_string_cannot_prove_a_macro_generated_export() {
    check(
        "string-export",
        "",
        "|| parser::read(&self.cache)",
        "const GUIDE: &str = \"\npub fn read() -> bool { false }\n\";\nmake_reader!();\n",
        &[],
        true,
    );
}
