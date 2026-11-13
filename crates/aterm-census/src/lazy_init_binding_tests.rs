// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn read_tree(label: &str, files: Vec<(String, String)>) -> CensusOutcome {
    let tree = crate::scan_set::test_fixtures::synth_tree("lazy-binding", label, &files);
    run_lazy_init_census(&tree)
}

#[test]
fn parentheses_do_not_hide_a_direct_reentrant_call() {
    for (label, call, red) in [
        ("plain-reentry", "reenter()", true),
        ("parenthesized-pure", "(pure)()", false),
        ("parenthesized-reentry", "(reenter)()", true),
        ("nested-reentry", "(((reenter)))()", true),
        ("multiline-reentry", "(\n    reenter\n)()", true),
        ("qualified-reentry", "(crate::reenter)()", true),
        ("nested-pure", "(((pure)))()", false),
        ("not-reentry", "!(reenter)()", true),
        ("not-pure", "!(pure)()", false),
        (
            "conditional-reentry",
            "{ if (reenter)() { true } else { false } }",
            true,
        ),
        (
            "conditional-pure",
            "{ if (pure)() { true } else { false } }",
            false,
        ),
        (
            "conditional-not-reentry",
            "{ if !(reenter)() { true } else { false } }",
            true,
        ),
        (
            "conditional-not-pure",
            "{ if !(pure)() { true } else { false } }",
            false,
        ),
        (
            "labeled-break-reentry",
            "{ 'done: loop { break 'done (reenter)(); } }",
            true,
        ),
        (
            "labeled-break-pure",
            "{ 'done: loop { break 'done (pure)(); } }",
            false,
        ),
        (
            "labeled-break-not-reentry",
            "{ 'done: loop { break 'done !(reenter)(); } }",
            true,
        ),
        (
            "labeled-break-not-pure",
            "{ 'done: loop { break 'done !(pure)(); } }",
            false,
        ),
        (
            "match-reentry",
            "{ match (reenter)() { value => value } }",
            true,
        ),
        ("while-reentry", "{ while (reenter)() {} false }", true),
        ("helper-reentry", "helper()", true),
        ("nested-declaration-only", "declaration_only()", false),
        ("call-result", "(factory(reenter))()", false),
        ("try-call-result", "maybe_helper().unwrap_or(false)", false),
        (
            "after-semicolon-reentry",
            "{ let _unused = false; (reenter)() }",
            true,
        ),
        (
            "generic-call-result",
            "generic_factory::<bool>(reenter)()",
            false,
        ),
        ("comparison-reentry", "{ 1 > (reenter)() as u8 }", true),
        ("comparison-pure", "{ 1 > (pure)() as u8 }", false),
        ("macro-result", "factory_macro!(reenter)()", false),
    ] {
        let source = format!(
            r#"
static CELL: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
fn pure() -> bool {{
    false
}}
fn reenter() -> bool {{
    *CELL.get_or_init(|| true)
}}
fn helper() -> bool {{
    (
        reenter
    )()
}}
fn declaration_only() -> bool {{
    fn reenter() -> bool {{ false }}
    false
}}
fn factory(_ignored: fn() -> bool) -> fn() -> bool {{ pure }}
fn generic_factory<T>(_ignored: fn() -> bool) -> fn() -> bool {{ pure }}
fn maybe_factory() -> Option<fn(fn() -> bool) -> fn() -> bool> {{ Some(factory) }}
fn maybe_helper() -> Option<bool> {{
    Some(maybe_factory()?(reenter)())
}}
macro_rules! factory_macro {{ ($ignored:expr) => {{ pure }} }}
pub fn read() -> bool {{
    *CELL.get_or_init(|| {call})
}}
"#
        );
        let out = read_tree(label, vec![("crates/demo/src/lib.rs".into(), source)]);
        assert_eq!(out.ok, !red, "{label}: {}", out.log);
    }
}

#[test]
fn const_generic_impl_headers_preserve_reentrant_owner() {
    for (label, declaration, header, initializer, red) in [
        (
            "nongeneric-blocking",
            "struct CellOwner;",
            "impl CellOwner",
            "Self::reenter()",
            true,
        ),
        (
            "const-generic-pure",
            "struct CellOwner<const N: usize>;",
            "impl CellOwner<{1 + 1}>\nwhere\n    Self: Sized,",
            "true",
            false,
        ),
        (
            "const-generic-one-line-blocking",
            "struct CellOwner<const N: usize>;",
            "impl CellOwner<{1 + 1}>",
            "Self::reenter()",
            true,
        ),
        (
            "const-generic-where-blocking",
            "struct CellOwner<const N: usize>;",
            "impl CellOwner<{1 + 1}>\nwhere\n    Self: Sized,",
            "Self::reenter()",
            true,
        ),
        (
            "opaque-const-header-blocking",
            "struct CellOwner<const N: usize>;",
            "impl CellOwner<{ let _ = 2 > 1; let _ = 3 > 2; let r#for = 2; r#for }>\nwhere\n    Self: Sized,",
            "Self::reenter()",
            true,
        ),
    ] {
        let body_open = if label == "const-generic-one-line-blocking" {
            " "
        } else {
            "\n"
        };
        let source = format!(
            r#"
static CELL: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
{declaration}
{header}{body_open}{{
    fn init() -> bool {{
        *CELL.get_or_init(|| {initializer})
    }}
    fn reenter() -> bool {{
        *CELL.get_or_init(|| true)
    }}
}}
"#
        );
        let out = read_tree(label, vec![("crates/demo/src/lib.rs".into(), source)]);
        eprintln!("impl-header control {label}: census_ok={}", out.ok);
        assert_eq!(out.ok, !red, "{label}: {}", out.log);
    }
}

#[test]
fn a_dot_getter_does_not_call_the_same_named_free_initializer() {
    for (label, getter, red) in [
        ("pure", "false", false),
        ("reentrant", "*READ.get_or_init(|| true)", true),
        ("calls-free", "modern_standby()", true),
    ] {
        let source = format!(
            r#"
static READ: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
struct Capability;
impl Capability {{
    fn modern_standby(&self) -> bool {{ {getter} }}
}}
fn power_capabilities() -> Capability {{ Capability }}
pub fn modern_standby() -> bool {{
    *READ.get_or_init(|| power_capabilities().modern_standby())
}}
"#
        );
        let out = read_tree(label, vec![("crates/power/src/lib.rs".into(), source)]);
        assert_eq!(out.ok, !red, "{label}: {}", out.log);
    }
}

#[test]
fn equal_static_spellings_in_distinct_functions_have_distinct_custody() {
    let source = r#"
fn first() -> bool {
    static READ: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *READ.get_or_init(|| false)
}
fn second() -> bool {
    static READ: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *READ.get_or_init(first)
}
"#;
    let files = vec![("crates/demo/src/lib.rs".into(), source.into())];
    let out = read_tree("fn-static", files.clone());
    assert!(out.ok, "{}", out.log);
    let derived = derive_from_sources(&files);
    assert_eq!(derived.cells.len(), 2);
    assert_ne!(derived.cells[0].id, derived.cells[1].id);
    let edges = derive_edges(&derived);
    assert!(edges.iter().any(|edge| edge.from != edge.to));
    assert!(find_cycles(&edges).0.is_empty());
}

fn crates(cycle: bool) -> Vec<(String, String)> {
    let next = if cycle { "beta::beta_read()" } else { "false" };
    vec![
        ("Cargo.toml".into(), "[workspace]\nmembers = [\"crates/alpha\", \"crates/beta\"]\n".into()),
        ("crates/alpha/Cargo.toml".into(), "[package]\nname = \"alpha\"\n[dependencies]\nbeta = { path = \"../beta\" }\n".into()),
        ("crates/beta/Cargo.toml".into(), "[package]\nname = \"beta\"\n[dependencies]\nalpha = { path = \"../alpha\" }\n".into()),
        ("crates/alpha/src/lib.rs".into(), format!("pub fn alpha_read() -> bool {{\n    static READ: std::sync::OnceLock<bool> = std::sync::OnceLock::new();\n    *READ.get_or_init(|| {next})\n}}\n")),
        ("crates/beta/src/lib.rs".into(), "pub fn beta_read() -> bool {\n    static READ: std::sync::OnceLock<bool> = std::sync::OnceLock::new();\n    *READ.get_or_init(alpha::alpha_read)\n}\n".into()),
    ]
}

#[test]
fn duplicate_cross_crate_statics_stay_distinct_and_real_source_cycles_stay_red() {
    let green = read_tree("distinct-crates", crates(false));
    assert!(green.ok, "{}", green.log);
    let red = read_tree("qualified-cycle", crates(true));
    assert!(!red.ok, "{}", red.log);
}

#[test]
fn trait_default_methods_and_associated_functions_keep_the_method_domain() {
    for (label, signature, body, red) in [
        ("trait-self", "&self", "self.value()", true),
        ("trait-associated", "", "Self::value()", true),
        ("trait-pure", "&self", "false", false),
    ] {
        let source = format!(
            r#"
trait Capability {{
    fn value({signature}) -> bool {{
        static READ: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *READ.get_or_init(|| {body})
    }}
}}
fn value() -> bool {{ false }}
"#
        );
        let out = read_tree(label, vec![("crates/traits/src/lib.rs".into(), source)]);
        assert_eq!(out.ok, !red, "{label}: {}", out.log);
    }
}

#[test]
fn a_nested_impl_type_cannot_reclassify_an_unrelated_module_call() {
    let source = r#"
mod Helper {
    pub fn read() -> bool {
        static READ: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *READ.get_or_init(crate::build)
    }
}
mod inner {
    struct Helper;
    impl Helper {
        fn other() {}
    }
}
fn build() -> bool { Helper::read() }
"#;
    let out = read_tree(
        "nested-type",
        vec![("crates/shadow/src/lib.rs".into(), source.into())],
    );
    assert!(!out.ok, "{}", out.log);
}

#[test]
fn a_bodyless_trait_alias_does_not_capture_the_following_free_function() {
    let source = r#"
#![feature(trait_alias)]
trait Alias = Send;
fn value() -> bool {
    static READ: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *READ.get_or_init(value)
}
"#;
    let out = read_tree(
        "trait-alias",
        vec![("crates/alias/src/lib.rs".into(), source.into())],
    );
    assert!(!out.ok, "{}", out.log);
}

#[test]
fn local_static_shadowing_requires_its_actual_enclosing_block() {
    for (label, local, red) in [
        (
            "ended-inner",
            "    {\n        static READ: std::sync::OnceLock<bool> = std::sync::OnceLock::new();\n    }\n    *READ",
            true,
        ),
        (
            "visible-shadow",
            "    static READ: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| false);\n    *READ",
            false,
        ),
        (
            "visible-before-declaration",
            "    let value = *READ;\n    static READ: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| false);\n    value",
            false,
        ),
    ] {
        let source = format!(
            "static READ: std::sync::LazyLock<bool> = std::sync::LazyLock::new(read);\nfn read() -> bool {{\n{local}\n}}\n"
        );
        let out = read_tree(label, vec![("crates/blocks/src/lib.rs".into(), source)]);
        assert_eq!(out.ok, !red, "{label}: {}", out.log);
    }
}

#[test]
fn a_declaration_is_not_a_call_but_same_line_body_calls_remain() {
    let scopes = DependencyScopes::new();
    let exports = BTreeMap::new();
    let calls = CallScope::new("crates/demo/src/lib.rs", "", &[], &scopes, &exports);
    let mut found = BTreeSet::new();
    calls.line_callees(
        "fn value(&self) -> bool { helper() || fnvalue() || self.value() }",
        &mut found,
    );
    assert_eq!(
        found,
        BTreeSet::from(["helper".into(), "fnvalue".into(), method_fn("value")])
    );
    calls.line_callees("fn value() -> bool { value() }", &mut found);
    assert!(found.contains("value"));
}

#[test]
fn ambiguous_constructors_do_not_export_another_owners_transitive_cell_effect() {
    for (label, initializer, red) in [
        ("constructors", "Enc::new", false),
        ("real-reentry", "read", true),
    ] {
        let mut files = vec![(
            "crates/demo/src/lib.rs".into(),
            format!(
                r#"
mod first;
mod second;
mod third;
struct Enc;
impl Enc {{
    fn new() -> bool {{ false }}
}}
struct Replay;
impl Replay {{
    fn new() -> bool {{ read() }}
}}
fn read() -> bool {{
    static ARTIFACT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ARTIFACT.get_or_init({initializer})
}}
"#
            ),
        )];
        // Other genuine constructor owners make the bare associated spelling
        // ambiguous, as in a workspace containing independent builders.
        for owner in ["First", "Second", "Third"] {
            files.push((
                format!("crates/demo/src/{}.rs", owner.to_lowercase()),
                format!("struct {owner};\nimpl {owner} {{\n    fn new() -> bool {{ false }}\n}}\n"),
            ));
        }
        let out = read_tree(label, files);
        assert_eq!(out.ok, !red, "{label}: {}", out.log);
    }
}

#[test]
fn only_actual_cell_values_can_feed_an_unresolved_borrowed_helper() {
    for (label, body, red) in [
        ("module", "consume(cache::value())", false),
        (
            "local",
            "let cache = std::sync::OnceLock::new(); consume(&cache)",
            false,
        ),
        (
            "literal",
            r#"let _description = "cache"; let unrelated = std::sync::OnceLock::new(); consume(&unrelated)"#,
            false,
        ),
        ("field", "consume(&self.cache)", true),
    ] {
        let source = format!(
            r#"
struct Reader {{
    cache: std::sync::OnceLock<bool>,
}}
impl Reader {{
    fn read(&self) -> bool {{
        *self.cache.get_or_init(|| {{ {body} }})
    }}
}}
fn consume(borrowed: &std::sync::OnceLock<bool>) -> bool {{
    *borrowed.get_or_init(|| true)
}}
mod cache {{
    pub fn value() -> &'static std::sync::OnceLock<bool> {{
        static OTHER: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        &OTHER
    }}
}}
"#
        );
        let out = read_tree(label, vec![("crates/value/src/lib.rs".into(), source)]);
        assert_eq!(out.ok, !red, "{label}: {}", out.log);
    }
    let source = r#"
static VALUE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
fn read() -> bool {
    *VALUE.get_or_init(|| consume(&VALUE))
}
fn consume(borrowed: &std::sync::OnceLock<bool>) -> bool {
    *borrowed.get_or_init(|| true)
}
"#;
    let out = read_tree(
        "static-value",
        vec![("crates/value/src/lib.rs".into(), source.into())],
    );
    assert!(!out.ok, "{}", out.log);
}

#[test]
fn an_unrelated_cell_mention_does_not_change_another_calls_actual_argument() {
    for (label, argument, red) in [
        ("other-argument", "OTHER", false),
        ("same-argument", "VALUE", true),
        ("parenthesized-value", "(VALUE)", true),
    ] {
        let source = format!(
            r#"
static VALUE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
static OTHER: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
fn read() -> bool {{
    *VALUE.get_or_init(|| {{
        let _mentioned = &VALUE;
        consume(&{argument})
    }})
}}
fn consume(borrowed: &std::sync::OnceLock<bool>) -> bool {{
    *borrowed.get_or_init(|| true)
}}
"#
        );
        let out = read_tree(label, vec![("crates/argument/src/lib.rs".into(), source)]);
        assert_eq!(out.ok, !red, "{label}: {}", out.log);
    }
}

#[test]
fn diagnostic_literals_are_not_actual_blocking_helper_code() {
    for (label, helper, red) in [
        (
            "quoted-method",
            r##"let _hint = "borrowed.get_or_init(|| true)"; false"##,
            false,
        ),
        (
            "raw-multiline",
            "let _hint = r#\"https://example.invalid/\nborrowed.get_or_init(|| true)\nfn phantom() { borrowed.get_or_init(|| true); }\n\"#; false",
            false,
        ),
        ("actual-method", "*borrowed.get_or_init(|| true)", true),
    ] {
        let source = format!(
            r#"
static VALUE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
fn read() -> bool {{
    *VALUE.get_or_init(|| describe(&VALUE))
}}
fn describe(borrowed: &std::sync::OnceLock<bool>) -> bool {{
    {helper}
}}
"#
        );
        let out = read_tree(label, vec![("crates/literal/src/lib.rs".into(), source)]);
        assert_eq!(out.ok, !red, "{label}: {}", out.log);
    }
}

#[test]
fn a_scalar_cast_type_is_not_a_cell_value_argument() {
    let source = r#"
type VALUE = usize;
const N: usize = 0;
static VALUE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
fn read() -> bool {
    *VALUE.get_or_init(|| consume(N as VALUE))
}
fn consume(_: VALUE) -> bool {
    let local = std::sync::OnceLock::new();
    *local.get_or_init(|| false)
}
"#;
    let out = read_tree(
        "cast-type",
        vec![("crates/cast/src/lib.rs".into(), source.into())],
    );
    assert!(out.ok, "{}", out.log);
}

#[test]
fn unresolved_transitive_parameter_substitution_remains_a_candidate_not_a_proof() {
    let source = r#"
static VALUE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
static OTHER: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
fn read() -> bool {
    *VALUE.get_or_init(|| f(&VALUE))
}
fn f(_ignored: &std::sync::OnceLock<bool>) -> bool {
    g(&OTHER)
}
fn g(borrowed: &std::sync::OnceLock<bool>) -> bool {
    *borrowed.get_or_init(|| false)
}
"#;
    let out = read_tree(
        "ignored-parameter",
        vec![("crates/ignored/src/lib.rs".into(), source.into())],
    );
    assert!(
        !out.ok,
        "the lexical guard cannot prove parameter substitution: {}",
        out.log
    );
    let files = vec![("crates/ignored/src/lib.rs".into(), source.into())];
    let derived = derive_from_sources(&files);
    let edges = derive_edges(&derived);
    assert!(
        edges
            .iter()
            .any(|edge| edge.from == edge.to && edge.conservative)
    );
}

#[test]
fn a_provider_method_cannot_inherit_another_files_same_named_lazy_trigger() {
    for (label, body, red) in [
        ("pure-provider", "false", false),
        ("provider-borrow", "*borrowed.get_or_init(|| true)", true),
    ] {
        let files = vec![
            ("Cargo.toml".into(), "[workspace]\nmembers = [\"crates/client\", \"crates/provider\"]\n".into()),
            ("crates/client/Cargo.toml".into(), "[package]\nname = \"client\"\n[dependencies]\nprovider = { path = \"../provider\" }\n".into()),
            ("crates/provider/Cargo.toml".into(), "[package]\nname = \"provider\"\n".into()),
            ("crates/client/src/lib.rs".into(), r#"
struct Cache {
    cache: std::sync::OnceLock<bool>,
}
impl Cache {
    fn get(&self) -> bool {
        *self.cache.get_or_init(|| provider::load(&self.cache))
    }
}
"#.into()),
            ("crates/provider/src/lib.rs".into(), format!(r#"
pub fn load(borrowed: &std::sync::OnceLock<bool>) -> bool {{
    Sink::get(borrowed)
}}
struct Sink;
impl Sink {{
    fn get(borrowed: &std::sync::OnceLock<bool>) -> bool {{ {body} }}
}}
"#)),
        ];
        let out = read_tree(label, files);
        assert_eq!(out.ok, !red, "{label}: {}", out.log);
    }
}

#[test]
fn an_unproved_module_call_keeps_its_provider_despite_a_same_file_free_function() {
    for (label, provider, red) in [
        ("module-reentry", "*crate::VALUE.get_or_init(|| true)", true),
        ("module-pure", "false", false),
    ] {
        let files = vec![
            (
                "crates/demo/src/lib.rs".into(),
                r#"
mod provider;
static VALUE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
fn load() -> bool { false }
fn read() -> bool {
    *VALUE.get_or_init(provider::load)
}
"#
                .into(),
            ),
            (
                "crates/demo/src/provider.rs".into(),
                format!("pub fn load() -> bool {{\n    {provider}\n}}\n"),
            ),
        ];
        let out = read_tree(label, files);
        assert_eq!(out.ok, !red, "{label}: {}", out.log);
    }
}

#[test]
fn receiver_dispatch_preserves_multiline_methods_and_real_reentry() {
    let source = r#"
struct Capabilities;
impl Capabilities {
    fn modern_standby(&self) -> bool {
        true
    }
}
fn modern_standby() -> Option<bool> {
    static READ: std::sync::OnceLock<Option<bool>> = std::sync::OnceLock::new();
    *READ.get_or_init(|| Some(Capabilities.modern_standby()))
}
"#;
    let files = |body: &str| vec![("crates/power/src/lib.rs".to_owned(), body.to_owned())];
    for source in [
        source.to_owned(),
        source.replace(
            "fn modern_standby(&self)",
            "fn modern_standby(\n        &self,\n    )",
        ),
    ] {
        let pure = run_synth_sources(&files(&source));
        assert!(
            pure.ok,
            "a receiver call cannot invoke the same-named free function: {}",
            pure.log
        );
        // A real method-to-free-function reentry still blocks its own initializer.
        let recursive = source.replace("        true", "        modern_standby().unwrap_or(false)");
        let reentry = run_synth_sources(&files(&recursive));
        assert!(
            !reentry.ok && reentry.log.contains("[OB-19]"),
            "{}",
            reentry.log
        );
        // A method can touch the cache directly as well; method dispatch must retain it.
        let declaration =
            "static READ: std::sync::OnceLock<Option<bool>> = std::sync::OnceLock::new();";
        let direct = format!(
            "{declaration}\n{}",
            source.replace(declaration, "").replace(
                "        true",
                "        READ.get_or_init(|| Some(false)).unwrap_or(false)"
            )
        );
        let reentry = run_synth_sources(&files(&direct));
        assert!(
            !reentry.ok && reentry.log.contains("[OB-19]"),
            "{}",
            reentry.log
        );
    }
    // Parameters do not turn an ordinary free function into a receiver method.
    let parameterized = source.replace("fn modern_standby()", "fn modern_standby(_flag: bool)");
    assert!(run_synth_sources(&files(&parameterized)).ok);
    // A range endpoint remains a free call, even though two dots precede it.
    let range = source.replace(
        "Some(Capabilities.modern_standby())",
        "Some((0..modern_standby().unwrap_or(false) as u8).is_empty())",
    );
    let reentry = run_synth_sources(&files(&range));
    assert!(
        !reentry.ok && reentry.log.contains("[OB-19]"),
        "{}",
        reentry.log
    );
}
