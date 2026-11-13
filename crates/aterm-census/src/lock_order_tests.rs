// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;
use crate::scan_set::test_fixtures::synth_tree;

fn def_crate(def_file: &'static str) -> &'static str {
    def_file
        .strip_prefix("crates/")
        .and_then(|rest| rest.split('/').next())
        .unwrap_or_else(|| panic!("{def_file}: the synthetic workspace models crates/<name>/ only"))
}

fn registered_def_files() -> impl Iterator<Item = &'static str> {
    GUARD_HELPERS
        .iter()
        .map(|h| h.def_file)
        .chain(VOCABULARY_INTERIORS.iter().map(|v| v.def_file))
}

fn synth_helper_crates() -> Vec<(&'static str, &'static str)> {
    let mut crates: Vec<(&'static str, &'static str)> = Vec::new();
    for krate in registered_def_files().map(def_crate) {
        if krate != "aterm-gui"
            && krate != "aterm-types"
            && crates.iter().all(|(name, _)| *name != krate)
        {
            crates.push((krate, ""));
        }
    }
    crates
}

fn synth_helper_files() -> Vec<(String, String)> {
    let mut files = crate::scan_set::test_fixtures::workspace_manifests(&synth_helper_crates());
    let mut sources: Vec<(String, String)> = Vec::new();
    let mut define =
        |def_file: &str, item: String| match sources.iter_mut().find(|(p, _)| p == def_file) {
            Some((_, text)) => text.push_str(&item),
            None => sources.push((def_file.to_string(), item)),
        };
    for h in GUARD_HELPERS {
        define(
            h.def_file,
            format!(
                "fn {}() -> std::sync::MutexGuard<'static, ()> {{\n    \
                 {}.lock().unwrap_or_else(std::sync::PoisonError::into_inner)\n}}\n",
                h.symbol, h.identity
            ),
        );
    }
    for v in VOCABULARY_INTERIORS {
        define(
            v.def_file,
            format!(
                "impl<T> MutexExt<T> for Mutex<T> {{\n    \
                 fn {}(&self) -> MutexGuard<'_, T> {{\n        \
                 match self.lock() {{\n            Ok(guard) => guard,\n            \
                 Err(poisoned) => poisoned.into_inner(),\n        }}\n    }}\n}}\n",
                v.symbol
            ),
        );
    }
    files.extend(sources);
    files
}

fn replace_file(files: &mut [(String, String)], rel: &str, contents: &str) {
    let slot = files
        .iter_mut()
        .find(|(p, _)| p == rel)
        .unwrap_or_else(|| panic!("fixture has no {rel}"));
    slot.1 = contents.to_string();
}

fn run_synth(name: &str, gui_extra: &str) -> CensusOutcome {
    let mut files = synth_helper_files();
    files.push((
        "crates/aterm-gui/src/extra.rs".to_string(),
        gui_extra.to_string(),
    ));
    run_lock_order_census(&synth_tree("aterm-lock-census", name, &files))
}

fn run_synth_files(name: &str, gui_extra: &str, extra: &[(&str, &str)]) -> CensusOutcome {
    let mut files = synth_helper_files();
    files.push((
        "crates/aterm-gui/src/extra.rs".to_string(),
        gui_extra.to_string(),
    ));
    for (rel, contents) in extra {
        files.push((rel.to_string(), contents.to_string()));
    }
    run_lock_order_census(&synth_tree("aterm-lock-census", name, &files))
}

#[test]
fn qualified_module_hops_follow_actual_path_mounts_and_preserve_cycles() {
    let caller = "#[path = \"worker.rs\"]\nmod child;\nfn first() {\n    let a = alpha.lock().unwrap();\n    child::take();\n}\nfn reverse() {\n    let b = beta.lock().unwrap();\n    alpha.lock();\n}\n";
    let red = run_synth_files(
        "qualified-path-cycle",
        caller,
        &[(
            "crates/aterm-gui/src/worker.rs",
            "fn take() {\n    beta.lock();\n}\n",
        )],
    );
    assert!(red.log.contains("CYCLE"), "{}", red.log);
    let green = run_synth_files(
        "qualified-path-isolation",
        &format!("{caller}fn take() {{\n    alpha.lock();\n}}\n"),
        &[("crates/aterm-gui/src/worker.rs", "fn take() {}\n")],
    );
    assert!(green.ok, "{}", green.log);
    let reexport_caller = format!("#[path = \"other.rs\"]\nmod other;\n{caller}");
    let reexport = run_synth_files(
        "qualified-reexport-cycle",
        &reexport_caller,
        &[
            (
                "crates/aterm-gui/src/worker.rs",
                "pub use super::other::take;\n",
            ),
            (
                "crates/aterm-gui/src/other.rs",
                "pub fn take() {\n    beta.lock();\n}\n",
            ),
        ],
    );
    assert!(reexport.log.contains("CYCLE"), "{}", reexport.log);
    let shadow = run_synth_files(
        "qualified-path-alias-shadow",
        &format!(
            "{}fn take() {{\n    alpha.lock();\n}}\n",
            caller.replace(
                "    child::take();",
                "    use unknown as child;\n    child::take();"
            )
        ),
        &[("crates/aterm-gui/src/worker.rs", "fn take() {}\n")],
    );
    assert!(!shadow.ok, "{}", shadow.log);
}

#[test]
fn unresolved_qualified_and_bare_calls_keep_conservative_edges() {
    for (name, prelude, call) in [
        ("alias", "use unknown as alias;", "alias::take()"),
        ("ufcs", "", "<Thing as Trait>::take()"),
        ("bare", "", "take()"),
        ("generic", "", "T::take()"),
    ] {
        let generic = if name == "generic" { "<T>" } else { "" };
        let text = format!(
            "{prelude}\nfn first{generic}() {{\n    let a = alpha.lock().unwrap();\n    {call};\n}}\nfn take() {{\n    beta.lock();\n}}\nfn reverse() {{\n    let b = beta.lock().unwrap();\n    alpha.lock();\n}}\n"
        );
        let out = run_synth(name, &text);
        assert!(out.log.contains("CYCLE"), "{name}: {}", out.log);
    }
}

#[test]
fn external_roots_need_absence_of_shadowing_and_unknown_globs() {
    let body = "fn first() {\n    let a = alpha.lock().unwrap();\n    std::thread::sleep();\n}\nfn sleep() {\n    beta.lock();\n}\nfn reverse() {\n    let b = beta.lock().unwrap();\n    alpha.lock();\n}\n";
    let green = run_synth("external-root", body);
    assert!(green.ok, "{}", green.log);
    let included = run_synth_files(
        "external-include-shadow",
        &format!("include!(\"bindings.rs\");\n{body}"),
        &[(
            "crates/aterm-gui/src/bindings.rs",
            "mod std {\n    pub mod thread {\n        pub fn sleep() {\n            beta.lock();\n        }\n    }\n}\n",
        )],
    );
    assert!(included.log.contains("CYCLE"), "{}", included.log);
    let type_shadow = run_synth(
        "external-type-shadow",
        &body.replace("fn first()", "fn first<std>()"),
    );
    assert!(type_shadow.log.contains("CYCLE"), "{}", type_shadow.log);
    let file_owner = body
        .replace("std::thread::sleep()", "std::fs::File::open()")
        .replace(
            "fn sleep() {\n    beta.lock();\n}",
            "impl File {\n    fn open() {\n        beta.lock();\n    }\n}",
        );
    let external_file = run_synth("external-file-owner", &file_owner);
    assert!(external_file.ok, "{}", external_file.log);
    for (name, prefix) in [
        ("module-shadow", "mod std {}"),
        ("raw-module-shadow", "mod r#std {}"),
        ("raw-type-shadow", "type r#std = Local;"),
        ("alias-shadow", "use other as std;"),
        ("glob-shadow", "use other::*;"),
        ("multiline-glob", "use\n other::{*};"),
    ] {
        let out = run_synth(name, &format!("{prefix}\n{body}"));
        assert!(out.log.contains("CYCLE"), "{name}: {}", out.log);
    }
}

#[test]
fn unsupported_unicode_qualifiers_never_panic_or_prove_an_ascii_suffix() {
    for path in ["éstd::thread::take", "module::éKey::take"] {
        let source = format!(
            "fn first() {{\n    let a = alpha.lock().unwrap();\n    {path}();\n}}\nstruct Key;\nimpl Key {{\n    fn take() {{}}\n}}\nfn take() {{\n    beta.lock();\n}}\nfn reverse() {{\n    let b = beta.lock().unwrap();\n    alpha.lock();\n}}\n"
        );
        let out = run_synth("unicode-qualified-hop", &source);
        assert!(out.log.contains("CYCLE"), "{path}: {}", out.log);
        let (calls, _) = held_call_targets(&format!("{path}();"));
        assert_eq!(calls.len(), 1);
        assert!(calls[0].1.path.is_empty());
        assert!(calls[0].1.qualifier.is_none());
    }
}

#[test]
fn whitespace_qualified_paths_never_prove_a_discarded_external_suffix() {
    qualified_suffix_cycles(&[
        "other :: std::thread::take",
        "other:: std::thread::take",
        "other ::std::thread::take",
        "other\t::\tstd::thread::take",
        "other::std ::thread::take",
    ]);
}

#[test]
fn raw_qualified_paths_never_prove_a_discarded_impl_owner() {
    qualified_suffix_cycles(&["other::r#Key::take", "other::r#std::thread::take"]);
}

#[test]
fn macro_metavariable_roots_never_prove_external_paths() {
    macro_metavariable_cycle(
        "std",
        "$std::thread::take",
        "pub mod thread {\n    pub fn take() {\n        beta.lock();\n    }\n}\n",
    );
}

#[test]
fn macro_metavariable_owners_never_prove_an_ascii_impl() {
    macro_metavariable_cycle(
        "Key",
        "$Key::take",
        "pub fn take() {\n    beta.lock();\n}\n",
    );
}

fn macro_metavariable_cycle(parameter: &str, path: &str, other: &str) {
    let source = format!(
        "mod other;\nstruct Key;\nimpl Key {{\n    fn take() {{}}\n}}\nfn first() {{\n    let a = alpha.lock().unwrap();\n    macro_rules! go {{\n        (${parameter}:ident) => {{\n            {path}();\n        }};\n    }}\n    go!(other);\n}}\nfn reverse() {{\n    let b = beta.lock().unwrap();\n    alpha.lock();\n}}\n"
    );
    let out = run_synth_files(
        "macro-qualified-hop",
        &source,
        &[("crates/aterm-gui/src/other.rs", other)],
    );
    assert!(out.log.contains("CYCLE"), "{path}: {}", out.log);
    let (calls, _) = held_call_targets(&format!("{path}();"));
    assert_eq!(calls.len(), 1);
    assert!(calls[0].1.path.is_empty(), "{path}");
    assert!(calls[0].1.qualifier.is_none(), "{path}");
}

fn qualified_suffix_cycles(paths: &[&str]) {
    for path in paths {
        let source = format!(
            "mod other;\nstruct Key;\nimpl Key {{\n    fn take() {{}}\n}}\nfn first() {{\n    let a = alpha.lock().unwrap();\n    {path}();\n}}\nfn reverse() {{\n    let b = beta.lock().unwrap();\n    alpha.lock();\n}}\n"
        );
        let out = run_synth_files(
            "whitespace-qualified-hop",
            &source,
            &[(
                "crates/aterm-gui/src/other.rs",
                "pub mod std {\n    pub mod thread {\n        pub fn take() {\n            beta.lock();\n        }\n    }\n}\npub mod r#Key {\n    pub fn take() {\n        beta.lock();\n    }\n}\n",
            )],
        );
        assert!(out.log.contains("CYCLE"), "{path}: {}", out.log);
        let (calls, _) = held_call_targets(&format!("{path}();"));
        assert_eq!(calls.len(), 1);
        assert!(calls[0].1.path.is_empty(), "{path}");
        assert!(calls[0].1.qualifier.is_none(), "{path}");
    }
}

#[test]
fn strict_test_cfg_masks_only_complete_provably_false_items() {
    for attr in [
        "#[cfg(all(test, unix))]",
        "#[cfg(\n all(test,\n unix)\n)]",
        "#[cfg(not(not(test)))]",
    ] {
        let source = format!(
            "{attr}\nmod hidden {{\n    fn cycle() {{\n        let a = alpha.lock().unwrap();\n        alpha.lock();\n    }}\n}}\nfn visible() {{\n    beta.lock();\n}}\n"
        );
        let masked = lock_graph_code(&source);
        assert!(!masked.contains("fn cycle"), "{attr}: {masked}");
        assert!(masked.contains("fn visible"));
    }
    for attr in [
        "#[cfg(any(test, debug_assertions))]",
        "#[cfg(any(test, feature = \"x\"))]",
        "#[cfg(not(test))]",
        "#[cfg(all(test, bogus(,)))]",
        "#[cfg(all(test, unix)) trailing)]",
    ] {
        let source = format!("{attr}\nfn visible() {{\n    beta.lock();\n}}\n");
        assert!(lock_graph_code(&source).contains("fn visible"), "{attr}");
    }
    let cycle = "mod gated {\n    fn cycle() {\n        let a = alpha.lock().unwrap();\n        alpha.lock();\n    }\n}\nfn visible() {\n    beta.lock();\n}\n";
    let absent = run_synth(
        "false-cfg-cycle",
        &format!("#[cfg(\n all(test, unix)\n)]\n{cycle}"),
    );
    assert!(absent.ok, "{}", absent.log);
    let retained = run_synth(
        "unknown-cfg-cycle",
        &format!("#[cfg(any(test, debug_assertions))]\n{cycle}"),
    );
    assert!(!retained.ok, "{}", retained.log);
    let array_then_module = "#[cfg(test)]\nconst KEY: [u8; 32] = [0; 32];\n#[cfg(test)]\nmod later {\n    fn hidden() { alpha.lock(); }\n}\nfn shipped() { beta.lock(); }\n";
    let masked = lock_graph_code(array_then_module);
    assert!(!masked.contains("const KEY"), "{masked}");
    assert!(!masked.contains("fn hidden"), "{masked}");
    assert!(masked.contains("fn shipped"), "{masked}");
    let closure_then_module = "fn first() {\n    #[cfg(test)]\n    WAITER.with(|waiter| {\n        take(waiter);\n    });\n}\n#[cfg(test)]\nmod later {\n    fn hidden() { alpha.lock(); }\n}\nfn shipped() { beta.lock(); }\n";
    let masked = lock_graph_code(closure_then_module);
    assert!(!masked.contains("WAITER"), "{masked}");
    assert!(!masked.contains("fn hidden"), "{masked}");
    assert!(masked.contains("fn shipped"), "{masked}");
    let multiline = "#[cfg(test)]\nfn hidden(\n    argument: usize,\n) {\n    alpha.lock();\n}\n#[cfg(test)]\nconst CALLED: usize = identity(\n    1,\n);\n#[cfg(test)]\nmod later {\n    fn hidden_too() { beta.lock(); }\n}\nfn shipped() { beta.lock(); }\n";
    let masked = lock_graph_code(multiline);
    assert!(!masked.contains("fn hidden"), "{masked}");
    assert!(!masked.contains("const CALLED"), "{masked}");
    assert!(masked.contains("fn shipped"), "{masked}");
    let mixed_const = lock_graph_code(
        "#[cfg(test)]\nconst HIDDEN: u8 = 0; fn shipped() { alpha.lock(); }\n#[cfg(test)]\nfn later() { beta.lock(); }\n",
    );
    assert!(mixed_const.contains("fn shipped"), "{mixed_const}");
    assert!(!mixed_const.contains("fn later"), "{mixed_const}");
    let mixed_then_module = "#[cfg(test)]\nfn hidden() {} fn shipped() { alpha.lock(); }\n#[cfg(test)]\nfn later() { beta.lock(); }\n";
    let masked = lock_graph_code(mixed_then_module);
    assert!(masked.contains("fn shipped"), "{masked}");
    assert!(!masked.contains("fn later"), "{masked}");
    let prefixed = "// λ padded   \r\nconst NOTE: &str = \"λ\";   \r\n#[cfg(\r\n all(test, unix)\r\n)]\r\nfn hidden() {\r\n    alpha.lock();\r\n}\r\nfn visible() {\r\n    beta.lock();\r\n}\r\n";
    let masked = lock_graph_code(prefixed);
    assert!(!masked.contains("fn hidden"), "{masked}");
    assert!(masked.contains("fn visible"), "{masked}");
    let same_line = lock_graph_code(
        "#[cfg(all(test, unix))] fn hidden() {}\nfn visible() {\n    beta.lock();\n}\n",
    );
    assert!(same_line.contains("fn hidden"));
    assert!(same_line.contains("fn visible"));
    let shared_line =
        lock_graph_code("#[cfg(test)] fn hidden() {} fn shipped() { alpha.lock(); }\n");
    assert!(shared_line.contains("fn shipped"));
    let literal = "const SOURCE: &str = r#\"\n#[cfg(all(test, unix))]\nfn fake() {}\n\"#;\nfn visible() {\n    beta.lock();\n}\n";
    assert!(lock_graph_code(literal).contains("fn visible"));
}

fn no_aliases() -> BTreeMap<String, (i32, String)> {
    BTreeMap::new()
}

fn bare_ctx(aliases: &BTreeMap<String, (i32, String)>) -> SiteCtx<'_> {
    SiteCtx {
        impls: &[],
        lineno: 1,
        aliases,
    }
}

#[test]
fn receiver_resolution_handles_fields_accessors_and_unknowns() {
    let aliases = no_aliases();
    let ctx = bare_ctx(&aliases);
    let acqs = acquisitions_on("let g = self.store.read().unwrap();", &ctx);
    assert_eq!(acqs.len(), 1);
    assert_eq!(acqs[0].identity.as_deref(), Some("store"));
    let acqs = acquisitions_on("proxies().write().unwrap().insert(child, entry);", &ctx);
    assert_eq!(acqs[0].identity.as_deref(), Some("proxies"));
    let acqs = acquisitions_on("*self.0.lock().unwrap() = None;", &ctx);
    assert_eq!(acqs.len(), 1);
    assert!(acqs[0].identity.is_none());
    let acqs = acquisitions_on("a.read().unwrap().clone()", &ctx);
    assert!(acqs[0].identity.is_none());
    let acqs = acquisitions_on("let mut t = term_lock(&s.term);", &ctx);
    assert_eq!(acqs[0].identity.as_deref(), Some("term"));
    assert!(acquisitions_on("pub(crate) fn term_lock(term: &Mutex<Terminal>) {", &ctx).is_empty());
}

#[test]
fn helper_calls_need_an_identifier_boundary_before_their_symbol() {
    let aliases = no_aliases();
    let ctx = bare_ctx(&aliases);
    let kinds = |line: &str| -> Vec<&'static str> {
        acquisitions_on(line, &ctx).iter().map(|a| a.kind).collect()
    };
    assert!(kinds("let t = xterm_lock(&s.term);").is_empty());
    assert_eq!(kinds("let p = judge_posts(&s);"), ["judge_posts"]);
    assert_eq!(
        kinds("let p = (posts(&s), term_lock(&t));"),
        ["posts", "term_lock"]
    );
}

#[test]
fn one_lexing_gives_the_lexer_views_and_outer_attributes() {
    let fixture = "#![allow(dead_code)]\n#[inline] #[cfg(test)]\n\
                   fn f() { r#type(\"x\", 'y', b\"z\"); }\n\
                   /* #[not] */ const S: &str = \"#[no]\";\n\
                   const R: &str = r#\"\n#[nor]\n\"#;\n#[derive(\n    Debug,\n)]\nstruct T;\n";
    for src in [
        fixture,
        include_str!("lex.rs"),
        include_str!("lock_order.rs"),
    ] {
        let lexed = lexed_views(src, &crate::lex::spans(src));
        let want = crate::lex::lex(src);
        assert!(lexed.code == want.code, "the code view differs");
        assert!(lexed.text == want.text, "the text view differs");
        assert!(
            outer_attributes_of(&lexed.code) == crate::lex::outer_attributes(src),
            "the outer attributes differ"
        );
    }
}

#[test]
fn of_two_attributes_on_one_line_the_later_gates_the_item() {
    let masked = lock_graph_code(
        "#[inline] #[cfg(test)]\nfn hidden() {\n    alpha.lock();\n}\n\
         fn shipped() {\n    beta.lock();\n}\n",
    );
    assert!(!masked.contains("fn hidden"), "{masked}");
    assert!(masked.contains("fn shipped"), "{masked}");
    let kept = lock_graph_code("#[cfg(test)] #[inline]\nfn shown() {\n    alpha.lock();\n}\n");
    assert!(kept.contains("fn shown"), "{kept}");
}

#[test]
fn unsafe_and_default_impls_open_impl_spans() {
    let spans: Vec<(usize, usize)> = impl_spans(
        "unsafe impl Send for Spill {}\n    default impl<T> Fill for Wrap<T> {\n        \
         fn f(&self) {}\n    }\n// impl Gone {\nimplied();\n",
    )
    .iter()
    .map(|span| (span.start, span.end))
    .collect();
    assert_eq!(spans, [(1, 1), (2, 4)]);
}

#[test]
fn line_helpers_cut_comments_join_continuations_and_split_ascii_words() {
    assert_eq!(before_double_slash("a / b // c // d"), "a / b ");
    assert_eq!(before_double_slash("x/y/"), "x/y/");
    assert_eq!(before_double_slash("//"), "");
    let body: Vec<String> = ["    let g = alpha", "        .lock()", "", "    drop(g);"]
        .map(String::from)
        .to_vec();
    let lines: Vec<(String, usize)> = logical_lines(&body, 7)
        .into_iter()
        .map(|line| (line.text, line.lineno))
        .collect();
    assert_eq!(
        lines,
        [
            ("let g = alpha.lock()".to_string(), 7),
            ("drop(g);".to_string(), 10)
        ]
    );
    assert_eq!(
        ascii_words("std2::é9x _").collect::<Vec<_>>(),
        ["std2", "9x", "_"]
    );
    let src = include_str!("lock_order.rs");
    let split: Vec<&str> = src
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|word| !word.is_empty())
        .collect();
    assert!(
        ascii_words(src).eq(split),
        "ascii_words differs from a split"
    );
}

#[test]
fn a_raw_identifier_spelling_a_root_never_proves_it_external() {
    let raw = "fn f() { r#std::thread::yield_now(); }";
    let facts = ShadowFacts::of(raw);
    assert!(facts.root_may_be_shadowed(raw, "std"));
    assert!(facts.module_root_shadowed(raw, "std"));
    let plain = "fn f() { std::thread::yield_now(); }";
    let facts = ShadowFacts::of(plain);
    assert!(!facts.root_may_be_shadowed(plain, "std"));
    assert!(!facts.module_root_shadowed(plain, "std"));
}

#[test]
fn self_receivers_take_the_enclosing_impl_type() {
    let impls = impl_spans("impl Spill {\n    fn f(&self) {\n        drop(0);\n    }\n}\n");
    let aliases = no_aliases();
    let ctx = SiteCtx {
        impls: &impls,
        lineno: 3,
        aliases: &aliases,
    };
    assert_eq!(
        acquisitions_on("*self.0.lock().unwrap() = None;", &ctx)[0]
            .identity
            .as_deref(),
        Some("Spill::0")
    );
    assert_eq!(
        acquisitions_on("let g = self.lock().unwrap();", &ctx)[0]
            .identity
            .as_deref(),
        Some("Spill::self")
    );
    assert_eq!(
        acquisitions_on("let g = self.pair.1.read().unwrap();", &ctx)[0]
            .identity
            .as_deref(),
        Some("Spill::pair.1")
    );
    assert_eq!(
        acquisitions_on("*self.0?.lock().unwrap() = None;", &ctx)[0]
            .identity
            .as_deref(),
        Some("Spill::0")
    );
    assert_eq!(
        acquisitions_on("let g = self.pair.1??.read().unwrap();", &ctx)[0]
            .identity
            .as_deref(),
        Some("Spill::pair.1")
    );
    assert_eq!(
        acquisitions_on("let g = self.store.read().unwrap();", &ctx)[0]
            .identity
            .as_deref(),
        Some("store")
    );
    assert!(
        acquisitions_on("let g = pair.0.lock().unwrap();", &ctx)[0]
            .identity
            .is_none()
    );
    let outside = SiteCtx {
        impls: &impls,
        lineno: 99,
        aliases: &aliases,
    };
    assert!(
        acquisitions_on("*self.0.lock().unwrap() = None;", &outside)[0]
            .identity
            .is_none()
    );
}

#[test]
fn impl_headers_resolve_to_a_nameable_self_type_or_none() {
    for (header, want) in [
        ("impl Spill ", Some("Spill")),
        ("impl<T> Spill<T> ", Some("Spill")),
        ("impl fmt::Display for Spill ", Some("Spill")),
        (
            "impl<'a, T: Clone> Sink<'a> for Spill<'a, T> ",
            Some("Spill"),
        ),
        (
            "impl<K, V, S> IndexMap<K, V, S> where S: Hash, ",
            Some("IndexMap"),
        ),
        ("unsafe impl Send for Spill ", Some("Spill")),
        ("impl Sink for &'a mut Spill ", Some("Spill")),
        ("impl<T> MutexExt<T> for Mutex<T> ", None),
        ("impl<T> Ext for Arc<T> ", None),
        ("impl<T> ToSmolStr for T ", None),
        ("impl<F: Fn() -> u32, T> Ext for T ", None),
        ("impl<T> UpdateSlice for &[T] ", None),
        ("impl FfiErrorCode for $ty ", None),
    ] {
        assert_eq!(
            impl_self_type(header).as_deref(),
            want,
            "impl header `{header}`"
        );
    }
}

#[test]
fn single_letter_receivers_take_the_path_they_alias() {
    let aliases = no_aliases();
    assert_eq!(
        alias_target("let m = &self.spill;", &bare_ctx(&aliases)).as_deref(),
        Some("spill")
    );
    assert_eq!(
        alias_target("let q = &QUEUE;", &bare_ctx(&aliases)).as_deref(),
        Some("QUEUE")
    );
    assert!(alias_target("let m = pick(a, b);", &bare_ctx(&aliases)).is_none());
    assert!(alias_target("let m = self.spill.lock().unwrap();", &bare_ctx(&aliases)).is_none());
    assert!(alias_target("let m = if x { &a } else { &b };", &bare_ctx(&aliases)).is_none());
    let mut aliases = no_aliases();
    aliases.insert("m".to_string(), (0, "spill".to_string()));
    assert_eq!(
        acquisitions_on("let g = m.lock().unwrap();", &bare_ctx(&aliases))[0]
            .identity
            .as_deref(),
        Some("spill")
    );
}

#[test]
fn guard_final_value_distinguishes_bound_guard_from_consumed_value() {
    let aliases = no_aliases();
    let ctx = bare_ctx(&aliases);
    let line = "let g = store.read().unwrap_or_else(|p| p.into_inner());";
    let acq = &acquisitions_on(line, &ctx)[0];
    assert!(guard_is_final_value(line, acq));
    let line = "let gone = self.store.read().unwrap_or_else(|p| p.into_inner()).by_local(s);";
    let acq = &acquisitions_on(line, &ctx)[0];
    assert!(!guard_is_final_value(line, acq));
    let line = "let mut term = term_lock(&s.term);";
    let acq = &acquisitions_on(line, &ctx)[0];
    assert!(guard_is_final_value(line, acq));
}

#[test]
fn literals_are_masked_before_token_scans() {
    let aliases = no_aliases();
    let ctx = bare_ctx(&aliases);
    assert!(
        acquisitions_on(
            &mask_literals(r#"log!("call .lock() and term_lock(x)");"#),
            &ctx
        )
        .is_empty()
    );
    let masked = mask_literals(r#"let b = matches!(c, '{');"#);
    assert_eq!(masked.matches('{').count(), 0);
}

#[test]
fn gated_items_end_at_their_own_shape_not_at_a_brace_count() {
    fn src(lines: &[&str]) -> String {
        let mut s = lines.join("\n");
        s.push('\n');
        s
    }
    let gates: &[&str] = &["#[cfg(test)]"];

    let m = mask_gated_items(
        &src(&[
            "#[cfg(test)]",
            "mod tests {",
            "    fn t() {",
            "        assert!(matches!(c, '{'), \"{}\", render());",
            "    }",
            "}",
            "fn spawn_pkg_update_check() {",
            "    let g = pkg.lock();",
            "}",
        ]),
        gates,
    );
    assert!(
        !m.contains("render()"),
        "the test mod must be blanked:\n{m}"
    );
    assert!(
        m.contains("fn spawn_pkg_update_check") && m.contains("pkg.lock()"),
        "shipped code AFTER a gated mod must survive:\n{m}"
    );
    assert_eq!(
        m.matches('{').count(),
        m.matches('}').count(),
        "the mask must leave the file brace-balanced:\n{m}"
    );

    let m = mask_gated_items(
        &src(&[
            "enum Ev {",
            "    Shipped(u8),",
            "    #[cfg(test)]",
            "    TestOnly,",
            "    AlsoShipped {",
            "        payload: u8,",
            "    },",
            "}",
        ]),
        gates,
    );
    assert!(!m.contains("TestOnly"), "the gated variant must go:\n{m}");
    assert!(
        m.contains("AlsoShipped") && m.contains("payload"),
        "the variants AFTER it must survive:\n{m}"
    );

    let m = mask_gated_items(
        &src(&[
            "fn settings(&self) -> Option<&S> {",
            "    match &self.overlay {",
            "        #[cfg(test)]",
            "        Some(Overlay::Settings(s)) => Some(s),",
            "        _ => None,",
            "    }",
            "}",
        ]),
        gates,
    );
    assert!(!m.contains("Overlay::Settings"), "arm must go:\n{m}");
    assert!(m.contains("_ => None"), "the next arm must survive:\n{m}");

    let m = mask_gated_items(
        &src(&[
            "#[cfg(test)]",
            "fn helper(",
            "    a: u32,",
            ") -> u32 {",
            "    alpha.lock();",
            "}",
            "fn shipped() {}",
        ]),
        gates,
    );
    assert!(!m.contains("alpha.lock()"), "gated body must go:\n{m}");
    assert!(m.contains("fn shipped"), "the next item must survive:\n{m}");

    let m = mask_gated_items(
        &src(&[
            "fn truncate(&mut self, n: usize) {",
            "    #[cfg(test)]",
            "    debug_assert!(",
            "        n <= self.count,",
            "        \"truncate({n}) exceeds count({})\",",
            "        self.count",
            "    );",
            "    self.count -= n;",
            "}",
        ]),
        gates,
    );
    assert!(!m.contains("debug_assert"), "the assert must go:\n{m}");
    assert!(
        m.contains("self.count -= n;"),
        "the shipped statement after it must survive:\n{m}"
    );
    assert_eq!(
        m.matches('{').count(),
        m.matches('}').count(),
        "the enclosing fn must still be brace-balanced:\n{m}"
    );

    let m = mask_gated_items(
        &src(&[
            "#[cfg(test)]",
            "fn odd() {",
            "    let g = alpha.lock();",
            "  }",
            "fn shipped() {}",
        ]),
        gates,
    );
    assert!(
        m.contains("fn odd") && m.contains("alpha.lock()"),
        "an unrecognised shape must be masked LESS, not more:\n{m}"
    );
    assert_eq!(
        m.matches('{').count(),
        m.matches('}').count(),
        "the mask must never splice a brace off its partner:\n{m}"
    );

    let m = mask_gated_items(
        &src(&[
            "enum Ev {",
            "    A,",
            "    #[cfg(test)]",
            "    B",
            "}",
            "fn shipped() {",
            "    alpha.lock();",
            "}",
        ]),
        gates,
    );
    assert!(!m.contains('B'), "the gated variant must go:\n{m}");
    assert!(
        m.contains("fn shipped") && m.contains("alpha.lock()"),
        "the item after the enum must survive:\n{m}"
    );
    assert_eq!(
        m.matches('{').count(),
        m.matches('}').count(),
        "the enum's close must survive:\n{m}"
    );

    let raw = src(&["#[cfg(test)]", "mod t {", "    fn a() {}", "}", "fn b() {}"]);
    assert_eq!(
        mask_gated_items(&raw, gates).lines().count(),
        raw.lines().count()
    );
}

#[test]
fn gated_item_boundaries_ignore_multiline_literals_and_comments() {
    let source = r####"
const QUOTED: &str = r###"
#[cfg(test)]
mod quoted_gate {
}
"###;
#[cfg(test)]
mod tests {
    const EXAMPLE: &str = r##"
}
fn dims(term: &Mutex<Terminal>) {
    (term_lock(term).rows(), term_lock(term).cols())
}
"##;
    /*
}
    */
}
fn shipped() {
    let guard = real.lock();
}
"####;
    let masked = mask_cfg_test_items(source);
    assert!(masked.contains("mod quoted_gate"), "a quoted gate is data");
    assert!(
        !masked.contains("fn dims"),
        "the whole actual test module is masked"
    );
    assert!(
        masked.contains("fn shipped") && masked.contains("real.lock()"),
        "real code after the test module survives"
    );
    assert_eq!(masked.lines().count(), source.lines().count());
}

/// A literal's later lines sit at any column: one at column 0 opens inside the literal, so it
/// is no dedent, and its `;` still ends the gated statement. Read on raw indents, each tail
/// below was kept as shipped code.
#[test]
fn a_literal_tail_at_column_0_ends_no_gated_item() {
    let text = [
        "fn shipped() {",
        "    #[cfg(test)]",
        "    let cooked = \"",
        "term_lock(&t).resize(1, 2)\";",
        "    #[cfg(test)]",
        "    let raw = r#\"",
        "gamma.lock()",
        "\"#;",
        "    #[cfg(test)]",
        "    let listed = [\"a",
        "\", delta.lock()];",
        "    beta.lock();",
        "}",
        "",
    ]
    .join("\n");
    let m = mask_cfg_test_items(&text);
    for gone in ["term_lock", "gamma.lock()", "\"#;", "delta.lock()"] {
        assert!(!m.contains(gone), "`{gone}` is test code:\n{m}");
    }
    assert!(
        m.contains("fn shipped() {") && m.contains("beta.lock();"),
        "the fn's shipped lines survive:\n{m}"
    );
    assert_eq!(m.lines().count(), text.lines().count());
}

#[test]
fn synthetic_diamond_without_cycle_is_green() {
    let out = run_synth(
        "diamond",
        "fn ab() {\n    let g = alpha_lock.lock().unwrap();\n    \
         let h = beta_lock.lock().unwrap();\n}\n\
         fn ac() {\n    let g = alpha_lock.lock().unwrap();\n    \
         let h = gamma_lock.lock().unwrap();\n}\n\
         fn bd() {\n    let g = beta_lock.lock().unwrap();\n    \
         let h = delta_lock.lock().unwrap();\n}\n\
         fn cd() {\n    let g = gamma_lock.lock().unwrap();\n    \
         let h = delta_lock.lock().unwrap();\n}\n",
    );
    assert!(out.ok, "diamond must be GREEN:\n{}", out.log);
    assert!(out.log.contains("ACYCLIC"), "log:\n{}", out.log);
    assert!(
        out.log.contains("alpha_lock -> beta_lock"),
        "edges must be enumerated; log:\n{}",
        out.log
    );
}

#[test]
fn synthetic_abba_cycle_is_red_naming_both_sites() {
    let out = run_synth(
        "abba",
        "fn take_ab() {\n    let g = alpha_lock.lock().unwrap();\n    \
         let h = beta_lock.lock().unwrap();\n}\n\
         fn take_ba() {\n    let g = beta_lock.lock().unwrap();\n    \
         let h = alpha_lock.lock().unwrap();\n}\n",
    );
    assert!(!out.ok, "ABBA must be RED:\n{}", out.log);
    assert!(out.log.contains("[OB-7]"), "log:\n{}", out.log);
    assert!(out.log.contains("CYCLE"), "log:\n{}", out.log);
    assert!(
        out.log.contains("EDGE alpha_lock -> beta_lock"),
        "log:\n{}",
        out.log
    );
    assert!(
        out.log.contains("EDGE beta_lock -> alpha_lock"),
        "log:\n{}",
        out.log
    );
    assert!(
        out.log.contains("crates/aterm-gui/src/extra.rs:2")
            && out.log.contains("crates/aterm-gui/src/extra.rs:3")
            && out.log.contains("crates/aterm-gui/src/extra.rs:6")
            && out.log.contains("crates/aterm-gui/src/extra.rs:7"),
        "both sites of both edges must be named; log:\n{}",
        out.log
    );
    assert!(out.log.contains("HOW TO REPAIR"), "log:\n{}", out.log);
    assert!(
        out.log.contains("NO waiver channel"),
        "the no-waiver stance must be stated; log:\n{}",
        out.log
    );
}

#[test]
fn synthetic_one_hop_interprocedural_cycle_is_red() {
    let out = run_synth(
        "onehop",
        "fn f() {\n    let g = alpha_lock.lock().unwrap();\n    helper_b();\n}\n\
         fn helper_b() {\n    beta_lock.lock().unwrap().push(1);\n}\n\
         fn g() {\n    let g = beta_lock.lock().unwrap();\n    helper_a();\n}\n\
         fn helper_a() {\n    alpha_lock.lock().unwrap().push(1);\n}\n",
    );
    assert!(!out.ok, "one-hop ABBA must be RED:\n{}", out.log);
    assert!(
        out.log.contains("[via call to `helper_b`"),
        "log:\n{}",
        out.log
    );
    assert!(
        out.log.contains("[via call to `helper_a`"),
        "log:\n{}",
        out.log
    );
}

#[test]
fn qualified_associated_calls_keep_their_impl_owner() {
    let prefix = "struct Key;\nstruct Sources;\ntype Alias = Sources;\n\
                  impl Key {\n    fn capture() {}\n}\n\
                  impl Sources {\n    fn capture() {\n        beta_lock.lock().unwrap();\n    }\n}\n\
                  fn reverse() {\n    let g = beta_lock.lock().unwrap();\n    alpha_lock.lock().unwrap();\n}\n";
    for (owner, cycle) in [
        ("Key", false),
        ("Sources", true),
        ("crate::Key", false),
        ("Key ", false),
        ("Alias", true),
        ("unknown_module", true),
    ] {
        let out = run_synth(
            owner,
            &format!(
                "{prefix}fn forward() {{\n    let g = alpha_lock.lock().unwrap();\n    {owner}::capture();\n}}\n"
            ),
        );
        assert_eq!(
            out.ok, !cycle,
            "the named impl's locks, not another impl's same-named function: {}",
            out.log
        );
    }
}

#[test]
fn local_free_function_resolution_shadows_duplicate_names_without_hiding_qualified_hops() {
    let foreign = "fn admitted() {\n    beta_lock.lock().unwrap();\n}\n\
        fn reverse() {\n    let held = beta_lock.lock().unwrap();\n    alpha_lock.lock().unwrap();\n}\n";
    for (call, local_body, cycle) in [
        ("admitted()", "", false),
        ("admitted()", "beta_lock.lock().unwrap();", true),
        ("crate::foreign::admitted()", "", true),
    ] {
        let source = format!(
            "fn admitted() {{\n    {local_body}\n}}\n\
            fn forward() {{\n    let held = alpha_lock.lock().unwrap();\n    {call};\n}}\n"
        );
        let out = run_synth_files(
            "local-free-priority",
            &source,
            &[("crates/aterm-gui/src/foreign.rs", foreign)],
        );
        assert_eq!(out.ok, !cycle, "{call}, local={local_body}: {}", out.log);
        assert_eq!(
            out.log.contains("EDGE alpha_lock -> beta_lock"),
            cycle,
            "{}",
            out.log
        );
    }
    let out = run_synth_files(
        "unshadowed-free-fallback",
        "fn forward() {\n    let held = alpha_lock.lock().unwrap();\n    admitted();\n}\n",
        &[("crates/aterm-gui/src/foreign.rs", foreign)],
    );
    assert!(
        !out.ok,
        "an unshadowed imported helper still supplies its edge: {}",
        out.log
    );
}

#[test]
fn module_free_function_resolution_inside_an_impl_keeps_nested_modules_conservative() {
    let foreign = "fn admitted() {\n    beta_lock.lock().unwrap();\n}\n\
        fn reverse() {\n    let held = beta_lock.lock().unwrap();\n    alpha_lock.lock().unwrap();\n}\n";
    let out = run_synth_files(
        "impl-module-free-priority",
        "fn admitted() {}\nstruct Caller;\nimpl Caller {\n    fn forward(&self) {\n        let held = alpha_lock.lock().unwrap();\n        admitted();\n    }\n}\n",
        &[("crates/aterm-gui/src/foreign.rs", foreign)],
    );
    assert!(
        out.ok,
        "an impl's bare free-function call uses its module: {}",
        out.log
    );
    let out = run_synth(
        "inline-module-free-fallback",
        "fn admitted() {}\nmod inner {\n    fn admitted() {\n        beta_lock.lock().unwrap();\n    }\n    fn forward() {\n        let held = alpha_lock.lock().unwrap();\n        admitted();\n    }\n}\nfn reverse() {\n    let held = beta_lock.lock().unwrap();\n    alpha_lock.lock().unwrap();\n}\n",
    );
    assert!(
        !out.ok,
        "an unproved inline module cannot borrow the file root's harmless definition: {}",
        out.log
    );
}

#[test]
fn self_and_ufcs_calls_keep_their_impl_owner() {
    let prefix = "struct Key;\nstruct Sources;\n\
                  impl Key {\n    fn capture(&self) {}\n}\n\
                  impl Sources {\n    fn capture(&self) {\n        beta_lock.lock().unwrap();\n    }\n}\n\
                  fn reverse() {\n    let g = beta_lock.lock().unwrap();\n    alpha_lock.lock().unwrap();\n}\n";
    for (owner, cycle) in [("Key", false), ("Sources", true)] {
        for call in [
            "Self::capture(self)".to_string(),
            format!("{owner}::capture(self)"),
        ] {
            let out = run_synth(
                owner,
                &format!(
                    "{prefix}impl {owner} {{\n    fn forward(&self) {{\n        let g = alpha_lock.lock().unwrap();\n        {call};\n    }}\n}}\n"
                ),
            );
            assert_eq!(
                out.ok, !cycle,
                "Self and UFCS preserve the selected impl's real locks: {}",
                out.log
            );
        }
    }
}

#[test]
fn generic_type_qualifiers_do_not_borrow_a_concrete_impl_owner() {
    let prefix = "trait Capture {\n    fn capture();\n}\n\
                  struct Key;\nstruct Sources;\n\
                  impl Key {\n    fn capture() {}\n}\n\
                  impl Capture for Sources {\n    fn capture() {\n        beta_lock.lock().unwrap();\n    }\n}\n\
                  fn reverse() {\n    let g = beta_lock.lock().unwrap();\n    alpha_lock.lock().unwrap();\n}\n";
    for (label, forward) in [
        (
            "fn-generic",
            "fn forward<Key: Capture>() {\n    let g = alpha_lock.lock().unwrap();\n    Key::capture();\n}\nfn instantiate() { forward::<Sources>(); }\n",
        ),
        (
            "impl-generic",
            "struct Holder<T>(std::marker::PhantomData<T>);\nimpl<Key: Capture> Holder<Key> {\n    fn forward() {\n        let g = alpha_lock.lock().unwrap();\n        Key::capture();\n    }\n}\nfn instantiate() { Holder::<Sources>::forward(); }\n",
        ),
    ] {
        let out = run_synth(label, &format!("{prefix}{forward}"));
        assert!(
            !out.ok,
            "a lexical type parameter can be Sources; concrete Key's harmless body is no proof: {}",
            out.log
        );
        let concrete = run_synth(
            &format!("concrete-{label}"),
            &format!(
                "{prefix}{}",
                forward.replace("Key::capture()", "crate::Key::capture()")
            ),
        );
        assert!(
            concrete.ok,
            "an explicit crate path still selects concrete Key despite the generic name: {}",
            concrete.log
        );
    }
}

#[test]
fn callable_parameters_shadow_only_bare_one_hop_targets() {
    let prefix = "fn post() {\n    posting.lock().unwrap();\n}\n";
    for (label, signature, call) in [
        ("impl", "fn with_post(post: impl FnOnce() -> ())", "post()"),
        (
            "generic",
            "fn with_post<F: FnOnce(u8, u8) -> ()>(mut post: F)",
            "post(1, 2)",
        ),
        (
            "pointer",
            "fn with_post(post: fn(u8, u8) -> ())",
            "post(1, 2)",
        ),
    ] {
        let out = run_synth(
            label,
            &format!(
                "{prefix}{signature} {{\n    let g = posting.lock().unwrap();\n    {call};\n}}\n"
            ),
        );
        assert!(
            out.ok,
            "the parameter shadows the unrelated fn, not its locks:\n{}",
            out.log
        );
        assert!(
            out.log.contains("parameter `post`"),
            "an unresolved callback must remain visible in the census:\n{}",
            out.log
        );
    }
    for (label, signature, call) in [
        ("free", "fn with_post()", "post()"),
        (
            "qualified",
            "fn with_post(post: impl FnOnce())",
            "crate::post()",
        ),
        (
            "qualified-space",
            "fn with_post(post: impl FnOnce())",
            "crate :: post()",
        ),
        (
            "method",
            "fn with_post(&self, post: impl FnOnce())",
            "self.post()",
        ),
    ] {
        let out = run_synth(
            label,
            &format!(
                "{prefix}{signature} {{\n    let g = posting.lock().unwrap();\n    {call};\n}}\n"
            ),
        );
        assert!(
            !out.ok && out.log.contains("RE-ENTRANCY SUSPECT"),
            "an actual fn or method call still acquires the held lock:\n{}",
            out.log
        );
    }
}

#[test]
fn error_mapping_preserves_a_guard_until_consumed_or_dropped() {
    let reverse = "fn reverse() {\n    let g = beta_lock.lock().unwrap();\n    \
                   let h = alpha_lock.lock().unwrap();\n}\n";
    for (label, tail, release, cycle) in [
        ("mapped-question", "?", "", true),
        ("mapped-result", "", "", true),
        ("mapped-consumed", ".is_ok()", "", false),
        ("mapped-dropped", "?", "drop(g);", false),
    ] {
        let out = run_synth(
            label,
            &format!(
                "fn forward() -> Result<(), ()> {{\n    \
                 let g = alpha_lock.lock().map_err(|_| ()){tail};\n    \
                 {release}\n    let h = beta_lock.lock().unwrap();\n    \
                 Ok(())\n}}\n{reverse}"
            ),
        );
        assert_eq!(
            !out.ok, cycle,
            "mapping an error leaves the Ok guard held; consuming or dropping \
             the result releases it:\n{}",
            out.log
        );
        if cycle {
            assert!(
                out.log.contains("EDGE alpha_lock -> beta_lock")
                    && out.log.contains("EDGE beta_lock -> alpha_lock"),
                "both real orders must form the cycle:\n{}",
                out.log
            );
        }
    }
}

#[test]
fn local_bindings_shadow_only_live_bare_one_hop_targets() {
    for (call, ok) in [("maintain()", true), ("self.maintain()", false)] {
        let projection = run_synth(
            "projection-callback",
            &format!(
                "impl Projection {{\n    fn maintain(&self) {{\n        let mut inputs = self.inputs.lock().unwrap();\n        if let Some(maintain) = &mut inputs.maintenance {{\n            {call};\n        }}\n    }}\n}}\n"
            ),
        );
        assert_eq!(projection.ok, ok, "{}", projection.log);
        if ok {
            assert!(projection.log.contains("local `maintain`"));
        } else {
            assert!(projection.log.contains("RE-ENTRANCY SUSPECT"));
        }
    }
    for (label, body) in [
        (
            "local-pointer",
            "unsafe {\n        let post: unsafe extern \"C-unwind\" fn() = msg();\n        post();\n    }",
        ),
        ("local-closure", "let post = || ();\n    post();"),
        (
            "local-nested",
            "let post = || ();\n    {\n        let post = || ();\n        post();\n    }\n    post();",
        ),
        (
            "local-multiline",
            "let post = {\n        || ()\n    };\n    post();",
        ),
        (
            "conditional-local",
            "if let Some(post) = callback {\n        post();\n    }",
        ),
        (
            "conditional-ref-mut-local",
            "while let Some(ref mut post) = callback {\n        post();\n    }",
        ),
        (
            "destructured-local",
            "let (_, post) = callbacks;\n    post();",
        ),
        (
            "conditional-field-local",
            "if let Callback { run: post, .. } = callback {\n        post();\n    }",
        ),
    ] {
        let out = run_synth(
            label,
            &format!(
                "fn post() {{\n    posting.lock().unwrap();\n}}\n\
                 fn caller() {{\n    let g = posting.lock().unwrap();\n    {body}\n}}\n"
            ),
        );
        assert!(
            out.ok,
            "a local callback is not the corpus fn:\n{}",
            out.log
        );
        assert!(
            out.log.contains("local `post`"),
            "the unresolved local target must remain reported:\n{}",
            out.log
        );
    }
}

#[test]
fn local_binding_initializers_and_out_of_scope_calls_keep_real_edges() {
    for (label, body) in [
        ("local-rhs", "let post = post();"),
        ("local-wrapped-rhs", "let post =\n        post();"),
        (
            "local-block-rhs",
            "let post = {\n        post();\n        || ()\n    };",
        ),
        (
            "local-else",
            "let Some(post) = callback else {\n        post();\n        return;\n    };\n    post();",
        ),
        (
            "local-expired",
            "{\n        let post = || ();\n        post();\n    }\n    post();",
        ),
        ("local-qualified", "let post = || ();\n    crate :: post();"),
        ("local-method", "let post = || ();\n    self.post();"),
        (
            "conditional-initializer",
            "if let Some(post) = post() {\n        post();\n    }",
        ),
        (
            "conditional-else",
            "if let Some(post) = callback {\n        post();\n    } else {\n        post();\n    }",
        ),
        (
            "conditional-expired",
            "if let Some(post) = callback {\n        post();\n    }\n    post();",
        ),
        (
            "conditional-method",
            "if let Some(post) = callback {\n        self.post();\n    }",
        ),
        (
            "conditional-qualified",
            "if let Some(post) = callback {\n        crate::post();\n    }",
        ),
        (
            "conditional-field-label",
            "if let Callback { post: other } = callback {\n        post();\n    }",
        ),
        (
            "conditional-path-variant",
            "if let Callback::post = callback {\n        post();\n    }",
        ),
        (
            "conditional-path-constructor",
            "if let module::post(other) = callback {\n        post();\n    }",
        ),
        (
            "local-type-name",
            "let other: post = callback;\n    post();",
        ),
    ] {
        let out = run_synth(
            label,
            &format!(
                "fn post() {{\n    posting.lock().unwrap();\n}}\n\
                 fn caller(&self) {{\n    let g = posting.lock().unwrap();\n    {body}\n}}\n"
            ),
        );
        assert!(
            !out.ok && out.log.contains("RE-ENTRANCY SUSPECT"),
            "this call resolves to the actual fn or method:\n{}",
            out.log
        );
    }
}

#[test]
fn synthetic_self_edge_is_red_reentrancy_suspect() {
    let out = run_synth(
        "reentrant",
        "fn nest_same() {\n    let g = alpha_lock.lock().unwrap();\n    \
         let h = alpha_lock.lock().unwrap();\n}\n",
    );
    assert!(!out.ok, "A-while-A must be RED:\n{}", out.log);
    assert!(out.log.contains("RE-ENTRANCY SUSPECT"), "log:\n{}", out.log);
    assert!(out.log.contains("alpha_lock"), "log:\n{}", out.log);
}

#[test]
fn synthetic_unknown_identity_is_counted_never_dropped() {
    let out = run_synth(
        "unknown",
        "fn opaque() {\n    let g = m.lock().unwrap();\n    let h = beta_lock.lock().unwrap();\n}\n",
    );
    assert!(out.ok, "an UNKNOWN cannot cycle — GREEN:\n{}", out.log);
    assert!(
        out.log.contains("1 UNKNOWN-identity site(s)"),
        "the UNKNOWN must be counted; log:\n{}",
        out.log
    );
    assert!(
        out.log.contains("UNKNOWN-identity sites"),
        "the UNKNOWN must be listed; log:\n{}",
        out.log
    );
    assert!(
        out.log
            .contains("UNKNOWN@crates/aterm-gui/src/extra.rs:2 -> beta_lock"),
        "log:\n{}",
        out.log
    );
}

#[test]
fn synthetic_multiline_lock_tokens_are_data_but_real_locks_remain() {
    let out = run_synth(
        "multiline-lock-data",
        r###"
fn quoted() {
    let raw = r##"
    .lock()
fn invented() { term_lock(term).resize(1, 1); }
"##;
    let cooked = "quoted
    .lock()
    ";
    let guard = quoted_real_lock.lock().unwrap();
}
"###,
    );
    assert!(out.ok, "{}", out.log);
    assert!(
        out.log.contains("0 UNKNOWN-identity site(s)"),
        "{}",
        out.log
    );
    assert!(out.log.contains("quoted_real_lock"), "{}", out.log);
}

#[test]
fn synthetic_tuple_field_abba_across_two_types_is_red() {
    let out = run_synth(
        "selffieldabba",
        "impl Alpha {\n    fn ab(&self) {\n        \
         let g = self.0.lock().unwrap();\n        self.beta_step();\n    }\n    \
         fn alpha_step(&self) {\n        self.0.lock().unwrap().push(1);\n    }\n}\n\
         impl Beta {\n    fn ba(&self) {\n        \
         let g = self.0.lock().unwrap();\n        self.alpha_step();\n    }\n    \
         fn beta_step(&self) {\n        self.0.lock().unwrap().push(1);\n    }\n}\n",
    );
    assert!(!out.ok, "a `self.0` ABBA must be RED:\n{}", out.log);
    assert!(
        out.log.contains("EDGE Alpha::0 -> Beta::0")
            && out.log.contains("EDGE Beta::0 -> Alpha::0"),
        "both edges of the cycle must be named with their qualified \
         identities:\n{}",
        out.log
    );
    assert!(
        out.log.contains("crates/aterm-gui/src/extra.rs:3")
            && out.log.contains("crates/aterm-gui/src/extra.rs:16"),
        "both sites of the cycle must be named:\n{}",
        out.log
    );
    assert!(
        out.log.contains("[via call to `beta_step`")
            && out.log.contains("[via call to `alpha_step`"),
        "the one-hop witness of each half must be named:\n{}",
        out.log
    );
}

#[test]
fn synthetic_same_field_on_different_types_does_not_merge() {
    let out = run_synth(
        "selffieldnomerge",
        "impl Alpha {\n    fn ab(&self) {\n        \
         let g = self.0.lock().unwrap();\n        \
         let h = shared_lock.lock().unwrap();\n    }\n}\n\
         impl Beta {\n    fn ba(&self) {\n        \
         let g = shared_lock.lock().unwrap();\n        \
         let h = self.0.lock().unwrap();\n    }\n}\n",
    );
    assert!(
        out.ok,
        "same-named fields on DIFFERENT types must not merge into a \
         cycle:\n{}",
        out.log
    );
    assert!(
        out.log.contains("Alpha::0 -> shared_lock") && out.log.contains("shared_lock -> Beta::0"),
        "both nestings must be tracked, under distinct identities:\n{}",
        out.log
    );
    assert!(
        out.log.contains("Alpha::0(1)") && out.log.contains("Beta::0(1)"),
        "each type's field must be its own identity in the ledger:\n{}",
        out.log
    );
}

#[test]
fn synthetic_unqualifiable_impl_types_keep_the_per_site_node() {
    let out = run_synth(
        "blanketself",
        "impl<T> Peek for T {\n    fn peek(&self) {\n        \
         self.0.lock().unwrap().len();\n    }\n}\n\
         impl<T> Ext for Mutex<T> {\n    fn twice(&self) {\n        \
         self.lock().unwrap().len();\n    }\n}\n",
    );
    assert!(out.ok, "GREEN expected:\n{}", out.log);
    assert!(
        out.log.contains("2 UNKNOWN-identity site(s)"),
        "neither shape may borrow a type name:\n{}",
        out.log
    );
}

#[test]
fn synthetic_vocabulary_interior_is_audited_not_unknown() {
    let out = run_synth(
        "vocab",
        "fn plain() {\n    let g = alpha_lock.lock().unwrap();\n}\n",
    );
    assert!(out.ok, "GREEN expected:\n{}", out.log);
    assert!(
        out.log.contains("1 audited vocabulary-interior site(s)"),
        "log:\n{}",
        out.log
    );
    assert!(
        out.log.contains("0 UNKNOWN-identity site(s)"),
        "the interior must not count as UNKNOWN; log:\n{}",
        out.log
    );
    assert!(
        out.log
            .contains("audit: MutexExt::lock_or_recover, the poison-recovery impl"),
        "the audit note must be rendered; log:\n{}",
        out.log
    );
}

#[test]
fn synthetic_stale_vocabulary_registration_is_red() {
    let mut files = synth_helper_files();
    replace_file(
        &mut files,
        "crates/aterm-types/src/mutex_ext.rs",
        "// no lock_or_recover here\n",
    );
    files.push((
        "crates/aterm-gui/src/extra.rs".to_string(),
        "fn f() {\n    let g = alpha_lock.lock().unwrap();\n}\n".to_string(),
    ));
    let root = synth_tree("aterm-lock-census", "stalevocab", &files);
    let out = run_lock_order_census(&root);
    assert!(
        !out.ok,
        "a stale vocabulary entry must be RED:\n{}",
        out.log
    );
    assert!(
        out.log.contains("vocabulary interior `lock_or_recover`")
            && out.log.contains("no longer defined"),
        "log:\n{}",
        out.log
    );
    let mut files = synth_helper_files();
    replace_file(
        &mut files,
        "crates/aterm-types/src/mutex_ext.rs",
        "fn lock_or_recover() -> u32 {\n    OTHER.lock().unwrap()\n}\n",
    );
    let root = synth_tree("aterm-lock-census", "driftvocab", &files);
    let out = run_lock_order_census(&root);
    assert!(
        !out.ok,
        "a drifted vocabulary entry must be RED:\n{}",
        out.log
    );
    assert!(
        out.log
            .contains("no longer contains the bare-`self` acquisition"),
        "log:\n{}",
        out.log
    );
}

#[test]
fn synthetic_file_advisory_lock_is_categorized_not_unknown() {
    let out = run_synth(
        "fileadvisory",
        "fn ledger_lock(path: &Path) -> Option<std::fs::File> {\n    \
         let f = std::fs::OpenOptions::new().read(true).open(path).ok()?;\n    \
         f.lock().ok()?;\n    Some(f)\n}\n\
         fn opaque() {\n    let g = m.lock().unwrap();\n}\n",
    );
    assert!(out.ok, "GREEN expected:\n{}", out.log);
    assert!(
        out.log.contains("1 OS file-advisory"),
        "the advisory site must be counted in the summary; log:\n{}",
        out.log
    );
    assert!(
        out.log.contains("1 UNKNOWN-identity site(s)"),
        "the File-less receiver must STAY UNKNOWN; log:\n{}",
        out.log
    );
    assert!(
        out.log.contains("EXCLUDED from the mutex order graph"),
        "the exclusion rationale must be printed; log:\n{}",
        out.log
    );
    assert!(
        out.log
            .contains("proven std::fs::File by its binding at crates/aterm-gui/src/extra.rs:2"),
        "the binding evidence span must be printed; log:\n{}",
        out.log
    );
}

#[test]
fn synthetic_file_ascription_is_evidence_and_generics_are_not() {
    let out = run_synth(
        "fileascription",
        "fn ledger_lock(path: &Path) -> Option<std::fs::File> {\n    \
         let f: std::fs::File = open_lock_file(path).ok()?;\n    \
         f.lock().ok()?;\n    Some(f)\n}\n\
         fn generic_is_not_evidence(path: &Path) {\n    \
         let g: Option<std::fs::File> = maybe_file(path);\n    \
         let g = g.unwrap();\n    g.lock().unwrap();\n}\n",
    );
    assert!(out.ok, "GREEN expected:\n{}", out.log);
    assert!(
        out.log.contains("1 OS file-advisory"),
        "the ascribed binding is advisory evidence; log:\n{}",
        out.log
    );
    assert!(
        out.log.contains("1 UNKNOWN-identity site(s)"),
        "the generic-typed rebind must stay UNKNOWN; log:\n{}",
        out.log
    );
}

#[test]
fn synthetic_file_advisory_lock_joins_no_mutex_edges() {
    let out = run_synth(
        "fileexclude",
        "fn hold_mutex_then_flock(path: &Path) {\n    \
         let g = alpha_lock.lock().unwrap();\n    \
         let f = std::fs::File::create(path).unwrap();\n    \
         f.lock().unwrap();\n    \
         let h = beta_lock.lock().unwrap();\n}\n",
    );
    assert!(out.ok, "GREEN expected:\n{}", out.log);
    assert!(
        out.log.contains("alpha_lock -> beta_lock"),
        "the mutex order must still be tracked across the flock; log:\n{}",
        out.log
    );
    assert!(
        !out.log.contains("UNKNOWN@"),
        "the advisory site must not appear as an UNKNOWN graph node; log:\n{}",
        out.log
    );
    assert!(out.log.contains("1 OS file-advisory"), "log:\n{}", out.log);
}

#[test]
fn synthetic_file_evidence_is_fail_closed_on_rebind_and_scope() {
    let out = run_synth(
        "filerebind",
        "fn rebind(path: &Path) {\n    \
         let f = std::fs::File::open(path).unwrap();\n    \
         f.lock().unwrap();\n    \
         let f = some_mutex_handle();\n    \
         f.lock().unwrap();\n}\n\
         fn scoped(path: &Path) {\n    {\n        \
         let f = std::fs::File::open(path).unwrap();\n        \
         f.lock().unwrap();\n    }\n    \
         f.lock().unwrap();\n}\n",
    );
    assert!(out.ok, "GREEN expected:\n{}", out.log);
    assert!(
        out.log.contains("2 OS file-advisory"),
        "only the evidenced sites are advisory; log:\n{}",
        out.log
    );
    assert!(
        out.log.contains("2 UNKNOWN-identity site(s)"),
        "rebound/out-of-scope receivers must fall back to UNKNOWN; log:\n{}",
        out.log
    );
}

#[test]
fn a_return_type_must_name_a_guard_not_merely_contain_the_letters() {
    for ret in [
        "std::sync::MutexGuard<'static, Supervisor>",
        "MutexGuard<'_, T>",
        "Option<MutexGuard<'_, T>>",
        "io::Result<RwLockReadGuard<'_, T>>",
        "TermGuard<'_>",
        "RwLockWriteGuard<'a, Store>",
        "MappedMutexGuard<'_, T>",
    ] {
        assert!(
            super::return_type_names_a_guard(ret),
            "a real guard must still be detected: {ret}"
        );
    }
    for ret in [
        "GuardedInput",
        "Guarded<T>",
        "Vec<GuardedInput>",
        "Result<GuardedInput, Error>",
        "GuardRail",
        "bool",
    ] {
        assert!(
            !super::return_type_names_a_guard(ret),
            "a type that is not a guard must not be flagged: {ret}"
        );
    }
}

#[test]
fn synthetic_drop_and_statement_scope_end_the_hold() {
    let out = run_synth(
        "scopes",
        "fn dropped() {\n    let g = alpha_lock.lock().unwrap();\n    drop(g);\n    \
         let h = beta_lock.lock().unwrap();\n}\n\
         fn consumed() {\n    let n = alpha_lock.lock().unwrap().len();\n    \
         let h = beta_lock.lock().unwrap();\n}\n\
         fn scoped() {\n    {\n        let g = alpha_lock.lock().unwrap();\n    }\n    \
         let h = beta_lock.lock().unwrap();\n}\n",
    );
    assert!(out.ok, "GREEN expected:\n{}", out.log);
    assert!(
        !out.log.contains("alpha_lock -> beta_lock"),
        "no alpha edge may survive drop/statement/block scope end; log:\n{}",
        out.log
    );
}

#[test]
fn synthetic_multiline_chain_resolves_receiver_and_nests() {
    let out = run_synth(
        "chain",
        "fn chained(s: &S) {\n    let g = s\n        .store\n        .read()\n        \
         .unwrap_or_else(|p| p.into_inner());\n    let t = term_lock(&s.term);\n}\n",
    );
    assert!(out.ok, "GREEN expected:\n{}", out.log);
    assert!(
        out.log.contains("store -> term"),
        "the chained receiver must resolve and the edge must appear; log:\n{}",
        out.log
    );
}

#[test]
fn synthetic_try_lock_is_a_source_but_never_a_target() {
    let out = run_synth(
        "trylock",
        "fn try_then_block() {\n    let Ok(g) = alpha_lock.try_lock() else { return };\n    \
         let h = beta_lock.lock().unwrap();\n}\n\
         fn block_then_try() {\n    let g = beta_lock.lock().unwrap();\n    \
         let h = alpha_lock.try_lock();\n}\n",
    );
    assert!(out.ok, "try_lock cannot be the waiting half:\n{}", out.log);
    assert!(
        out.log.contains("alpha_lock -> beta_lock"),
        "a held try-guard is still a hold; log:\n{}",
        out.log
    );
    assert!(
        !out.log.contains("beta_lock -> alpha_lock"),
        "a try acquisition never blocks, so it is no edge target; log:\n{}",
        out.log
    );
}

/// A guard-returning helper in a module only a test build compiles is test code, as one in
/// a `tests.rs` is. NEGATIVE CONTROL: the same module declared for every build is shipped
/// code, and both of its helpers are red.
#[test]
fn synthetic_test_gated_module_files_are_left_out() {
    let sim = [
        (
            "crates/aterm-gui/src/extra/sim/mod.rs",
            "fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {\n    \
             m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)\n}\n",
        ),
        (
            "crates/aterm-gui/src/extra/sim/world.rs",
            "fn world_guard() -> std::sync::MutexGuard<'static, u32> {\n    \
             WORLD_LOCK.lock().unwrap()\n}\n",
        ),
    ];
    let gated = run_synth_files("testgated", "/// The sim.\n#[cfg(test)]\nmod sim;\n", &sim);
    assert!(gated.ok, "a test-gated module is test code:\n{}", gated.log);
    assert!(
        gated
            .log
            .contains("2 file(s) a `#[cfg(test)] mod` declares left out"),
        "the files left out are counted:\n{}",
        gated.log
    );
    let shipped = run_synth_files("testgated-control", "/// The sim.\nmod sim;\n", &sim);
    assert!(
        !shipped.ok,
        "an ungated module is shipped code:\n{}",
        shipped.log
    );
    for helper in ["`fn lock`", "`fn world_guard`"] {
        assert!(
            shipped
                .log
                .lines()
                .any(|l| l.contains("UNREGISTERED GUARD-RETURNING HELPER") && l.contains(helper)),
            "{helper} is red:\n{}",
            shipped.log
        );
    }
}

#[test]
fn synthetic_unregistered_guard_returning_helper_is_red() {
    let out = run_synth(
        "helpergap",
        "fn my_guard() -> std::sync::MutexGuard<'static, u32> {\n    \
         MY_LOCK.lock().unwrap()\n}\n",
    );
    assert!(
        !out.ok,
        "an unregistered acquire-and-return helper hides holds:\n{}",
        out.log
    );
    assert!(
        out.log.contains("UNREGISTERED GUARD-RETURNING HELPER"),
        "log:\n{}",
        out.log
    );
    assert!(out.log.contains("my_guard"), "log:\n{}", out.log);
}

#[test]
fn synthetic_raw_ptr_reads_are_categorized_not_unknown() {
    let out = run_synth(
        "rawptr",
        "fn chain(buf: &[u8], pos: usize) -> u8 {\n    \
         unsafe { buf.as_ptr().add(pos).read() }\n}\n\
         fn bound(buf: &[u8]) -> u8 {\n    let src_ptr = buf.as_ptr();\n    \
         unsafe { src_ptr.read() }\n}\n\
         fn param(input_ptr: &mut *const u8) -> u8 {\n    \
         unsafe { input_ptr.read() }\n}\n\
         fn no_evidence() {\n    let v = *opaque.read().unwrap();\n}\n",
    );
    assert!(out.ok, "GREEN expected:\n{}", out.log);
    assert!(
        out.log.contains("3 raw-pointer ptr::read"),
        "all three evidence shapes must be categorized; log:\n{}",
        out.log
    );
    assert!(
        out.log.contains("raw-pointer-producing method")
            && out.log.contains("proven a raw pointer by its binding at")
            && out
                .log
                .contains("declared `*const`/`*mut` in the fn signature"),
        "each site must carry its evidence; log:\n{}",
        out.log
    );
    assert!(
        out.log.contains("opaque(1)"),
        "the evidence-less zero-arg .read() must KEEP its RwLock resolution \
         (fail-closed); log:\n{}",
        out.log
    );
    assert!(
        out.log.contains("0 UNKNOWN-identity site(s)"),
        "raw-ptr sites must not be UNKNOWN; log:\n{}",
        out.log
    );
}

#[test]
fn synthetic_raw_ptr_read_joins_no_mutex_edges() {
    let out = run_synth(
        "rawptredge",
        "fn hold_then_ptr_then_lock(buf: &[u8]) {\n    \
         let g = alpha_lock.lock().unwrap();\n    \
         let b = unsafe { buf.as_ptr().add(1).read() };\n    \
         let h = beta_lock.lock().unwrap();\n}\n",
    );
    assert!(out.ok, "GREEN expected:\n{}", out.log);
    assert!(
        out.log.contains("alpha_lock -> beta_lock"),
        "the mutex order must survive across the ptr::read; log:\n{}",
        out.log
    );
    assert!(
        out.log.contains("1 raw-pointer ptr::read"),
        "log:\n{}",
        out.log
    );
    assert!(
        !out.log.contains("UNKNOWN@"),
        "the raw-ptr site must not appear as an UNKNOWN graph node; log:\n{}",
        out.log
    );
}

#[test]
fn synthetic_free_fn_with_standard_name_is_not_exempt() {
    let out = run_synth(
        "freefnlock",
        "fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {\n    \
         m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)\n}\n",
    );
    assert!(
        !out.ok,
        "a free guard-returning fn named `lock` hides its callers' holds:\n{}",
        out.log
    );
    assert!(
        out.log.contains("UNREGISTERED GUARD-RETURNING HELPER"),
        "log:\n{}",
        out.log
    );
    let out = run_synth(
        "methodlock",
        "impl Wrapper {\n    pub fn lock(&self) -> MutexGuard<'_, u32> {\n        \
         self.raw.lock().unwrap()\n    }\n}\n",
    );
    assert!(
        out.ok,
        "a METHOD named `lock` is token-captured at its call sites:\n{}",
        out.log
    );
    let out = run_synth(
        "cratelock",
        "impl Wrapper {\n    pub(crate) fn lock(&self) -> MutexGuard<'_, u32> {\n        \
         self.raw.lock().unwrap()\n    }\n}\n",
    );
    assert!(
        out.ok,
        "a `pub(crate)` METHOD named `lock` is token-captured too:\n{}",
        out.log
    );
}

#[test]
fn synthetic_vendored_identities_are_namespaced_and_never_merge() {
    let out = run_synth_files(
        "vendoredns",
        "fn aterm_order() {\n    let g = state.lock().unwrap();\n    \
         let h = queue.lock().unwrap();\n}\n",
        &[(
            "crates/aterm-window/src/planted.rs",
            "fn winit_order() {\n    let g = queue.lock().unwrap();\n    \
             let h = state.lock().unwrap();\n}\n",
        )],
    );
    assert!(
        out.ok,
        "namespaced foreign receivers must NOT merge into a cycle:\n{}",
        out.log
    );
    assert!(
        out.log.contains("aterm_window::state(1)") && out.log.contains("aterm_window::queue(1)"),
        "vendored identities must be namespaced in the ledger:\n{}",
        out.log
    );
    assert!(
        out.log.contains("state -> queue")
            && out
                .log
                .contains("aterm_window::queue -> aterm_window::state"),
        "both crates' nestings must be tracked, separately:\n{}",
        out.log
    );
}

#[test]
fn external_workspace_libraries_are_isolated_but_their_own_cycles_still_fail() {
    for cycle in [false, true] {
        let mut files = synth_helper_files();
        let gui = files
            .iter_mut()
            .find(|(path, _)| path == "crates/aterm-gui/Cargo.toml")
            .unwrap();
        gui.1
            .push_str("\n[dependencies.foreign]\npath = \"../../foreign/lib\"\n");
        files.push(("crates/aterm-gui/src/extra.rs".into(), "fn caller() {\n let a = beta_lock.lock().unwrap();\n let b = alpha_lock.lock().unwrap();\n}\n".into()));
        files.push((
            "foreign/Cargo.toml".into(),
            "[workspace]\nmembers = [\"lib\"]\n".into(),
        ));
        files.push((
            "foreign/lib/Cargo.toml".into(),
            "[package]\nname = \"foreign\"\n".into(),
        ));
        let mut source = "fn forward() {\n let a = alpha_lock.lock().unwrap();\n let b = beta_lock.lock().unwrap();\n}\n".to_string();
        if cycle {
            source.push_str("fn backward() {\n let a = beta_lock.lock().unwrap();\n let b = alpha_lock.lock().unwrap();\n}\n");
        }
        files.push(("foreign/lib/src/lib.rs".into(), source));
        files.push(("foreign/lib/src/bin/dev.rs".into(), "fn dev() {}\n".into()));
        let result = run_lock_order_census(&synth_tree(
            "aterm-lock-census",
            "external-workspace",
            &files,
        ));
        assert_eq!(result.ok, !cycle, "{}", result.log);
        assert!(
            result.log.contains("external::foreign::alpha_lock"),
            "{}",
            result.log
        );
    }
}

#[test]
fn an_external_workspace_guard_helper_is_red_and_spelled_out_not_registered() {
    let mut files = synth_helper_files();
    let gui = files
        .iter_mut()
        .find(|(path, _)| path == "crates/aterm-gui/Cargo.toml")
        .unwrap();
    gui.1
        .push_str("\n[dependencies.foreign]\npath = \"../../foreign/lib\"\n");
    files.push((
        "foreign/Cargo.toml".into(),
        "[workspace]\nmembers = [\"lib\"]\n".into(),
    ));
    files.push((
        "foreign/lib/Cargo.toml".into(),
        "[package]\nname = \"foreign\"\n".into(),
    ));
    files.push((
        "foreign/lib/src/lib.rs".into(),
        "fn foreign_guard() -> std::sync::MutexGuard<'static, ()> {\n    \
         alpha_lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner)\n}\n"
            .into(),
    ));
    let out = run_lock_order_census(&synth_tree(
        "aterm-lock-census",
        "external-guard-helper",
        &files,
    ));
    let line = out
        .log
        .lines()
        .find(|l| l.contains("UNREGISTERED GUARD-RETURNING HELPER — `fn foreign_guard`"));
    assert!(
        !out.ok && line.is_some(),
        "another workspace's guard helper must be RED:\n{}",
        out.log
    );
    assert!(
        line.is_some_and(|l| !l.contains("Register it in GUARD_HELPERS")),
        "a row cannot name another workspace's fn, so the remedy is not one:\n{}",
        out.log
    );
}

#[test]
fn synthetic_cross_boundary_one_hop_edge_is_graphed() {
    let out = run_synth_files(
        "xboundedge",
        "fn hold_and_call() {\n    let g = alpha_lock.lock().unwrap();\n    \
         winit_side_helper();\n}\n",
        &[(
            "crates/aterm-window/src/planted.rs",
            "fn winit_side_helper() {\n    event_state.lock().unwrap().push(1);\n}\n",
        )],
    );
    assert!(out.ok, "GREEN expected:\n{}", out.log);
    assert!(
        out.log.contains("alpha_lock -> aterm_window::event_state"),
        "the aterm-held -> winit-acquire edge must be in the ledger:\n{}",
        out.log
    );
}

#[test]
fn a_vendored_fn_name_cannot_capture_an_aterm_one_hop_call() {
    let out = run_synth_files(
        "nsshadow",
        "fn start() {\n    aterm_trace.lock().unwrap().push(1);\n}\n\
         fn aterm_hold_and_call() {\n    let g = alpha_lock.lock().unwrap();\n    \
         start();\n}\n",
        &[(
            "vendor/indexmap/src/planted.rs",
            "fn start() {\n    writer.lock().unwrap().push(1);\n}\n\
             fn vendored_hold_and_call() {\n    let g = stream.lock().unwrap();\n    \
             start();\n}\n",
        )],
    );
    assert!(out.ok, "GREEN expected:\n{}", out.log);
    assert!(
        out.log.contains("alpha_lock -> aterm_trace"),
        "the aterm call site must still resolve to ATERM's `start`:\n{}",
        out.log
    );
    assert!(
        !out.log.contains("alpha_lock -> indexmap::writer"),
        "an aterm hold must NOT inherit a vendored lock identity through a \
         shared fn name:\n{}",
        out.log
    );
    assert!(
        out.log.contains("indexmap::stream -> indexmap::writer"),
        "the vendored call site must resolve to the VENDORED `start`:\n{}",
        out.log
    );
    assert!(
        !out.log.contains("indexmap::stream -> aterm_trace"),
        "and the boundary holds in both directions — a vendored hold must \
         not reach an aterm fn of the same name:\n{}",
        out.log
    );
}

#[test]
fn a_question_mark_receiver_still_names_its_lock() {
    let aliases = no_aliases();
    let ctx = bare_ctx(&aliases);
    let acqs = acquisitions_on(
        "let g = self.store?.read().unwrap_or_else(|p| p.into_inner());",
        &ctx,
    );
    assert_eq!(acqs.len(), 1);
    assert_eq!(
        acqs[0].identity.as_deref(),
        Some("store"),
        "a `?` before the acquisition dot must not erase the receiver"
    );
    let acqs = acquisitions_on("self.inner??.lock().unwrap();", &ctx);
    assert_eq!(acqs[0].identity.as_deref(), Some("inner"));
    let acqs = acquisitions_on("*self.0?.lock().unwrap() = None;", &ctx);
    assert!(acqs[0].identity.is_none());
    let acqs = acquisitions_on("a?.read().unwrap().clone()", &ctx);
    assert!(acqs[0].identity.is_none());
}

#[test]
fn a_field_hop_is_followed_when_the_field_type_and_the_callee_both_resolve() {
    let out = run_synth(
        "fieldhop",
        "struct Holder {\n    shared: Arc<Shared>,\n}\nstruct Shared {\n    inner_lock: Mutex<u8>,\n}\nfn peek_shared(&self) {\n    beta_lock.lock().unwrap();\n}\nfn hold_and_hop(&self) {\n    let g = alpha_lock.lock().unwrap();\n    self.shared.peek_shared();\n}\n",
    );
    assert!(
        out.log.contains("alpha_lock -> beta_lock"),
        "a field hop with a corpus field type AND a unique callee must be \
         FOLLOWED — dropping it is a silent missed edge:\n{}",
        out.log
    );
    assert!(
        !out.log.contains("shared.peek_shared"),
        "a followed hop must not also be listed as not-followed:\n{}",
        out.log
    );
}

#[test]
fn a_field_hop_is_not_followed_when_the_callee_name_is_ambiguous() {
    let out = run_synth(
        "fieldambig",
        "struct Holder {\n    parked: Condvar,\n}\nstruct Condvar {\n    raw: u8,\n}\nfn wait(&self) {\n    beta_lock.lock().unwrap();\n}\nfn hold_and_hop(&self) {\n    let g = alpha_lock.lock().unwrap();\n    self.parked.wait(g);\n}\nfn wait(&self) {\n    gamma_lock.lock().unwrap();\n}\n",
    );
    assert!(
        !out.log.contains("alpha_lock -> beta_lock")
            && !out.log.contains("alpha_lock -> gamma_lock"),
        "an ambiguous callee name must NOT be resolved by picking a \
         candidate — that is how the phantom cycle was fabricated:\n{}",
        out.log
    );
    assert!(
        out.log.contains("parked.wait"),
        "a hop the census declined to follow must be LISTED, so the \
         narrowing is auditable rather than silent:\n{}",
        out.log
    );
}

#[test]
fn a_field_hop_is_not_followed_when_the_field_type_is_foreign() {
    let out = run_synth(
        "fieldforeign",
        "struct Holder {\n    open: AtomicBool,\n}\nfn load(&self) {\n    beta_lock.lock().unwrap();\n}\nfn hold_and_hop(&self) {\n    let g = alpha_lock.lock().unwrap();\n    self.open.load(Ordering::Relaxed);\n}\n",
    );
    assert!(
        !out.log.contains("alpha_lock -> beta_lock"),
        "a foreign field type must not resolve to a same-named corpus \
         fn:\n{}",
        out.log
    );
    assert!(
        out.log.contains("open.load"),
        "the declined hop must be listed:\n{}",
        out.log
    );
}

#[test]
fn synthetic_cross_boundary_abba_is_red_across_the_namespace() {
    let out = run_synth_files(
        "xboundabba",
        "fn hold_and_call() {\n    let g = alpha_lock.lock().unwrap();\n    \
         winit_side_helper();\n}\n\
         fn aterm_side_helper() {\n    alpha_lock.lock().unwrap().push(1);\n}\n",
        &[(
            "crates/aterm-window/src/planted.rs",
            "fn winit_side_helper() {\n    event_state.lock().unwrap().push(1);\n}\n\
             fn winit_hold_and_call() {\n    \
             let g = event_state.lock().unwrap();\n    aterm_side_helper();\n}\n",
        )],
    );
    assert!(!out.ok, "a cross-boundary ABBA must be RED:\n{}", out.log);
    assert!(
        out.log.contains("CYCLE")
            && out.log.contains("alpha_lock")
            && out.log.contains("aterm_window::event_state"),
        "the cycle must name both sides of the namespace boundary:\n{}",
        out.log
    );
    assert!(
        out.log.contains("crates/aterm-window/src/planted.rs:2")
            && out.log.contains("crates/aterm-gui/src/extra.rs:2"),
        "both crates' sites must be named in the diagnostic:\n{}",
        out.log
    );
}

#[test]
fn synthetic_platform_slice_sites_are_counted_never_graphed() {
    let out = run_synth_files(
        "sliceabba",
        "fn plain() {\n    let g = alpha_lock.lock().unwrap();\n}\n",
        &[(
            "crates/aterm-window/src/platform_impl/linux/planted.rs",
            "fn ab() {\n    let g = sctk_state.lock().unwrap();\n    \
             let h = x11_state.lock().unwrap();\n}\n\
             fn ba() {\n    let g = x11_state.lock().unwrap();\n    \
             let h = sctk_state.lock().unwrap();\n}\n",
        )],
    );
    assert!(
        out.ok,
        "platform-slice code links into no shipped process — never graphed:\n{}",
        out.log
    );
    assert!(
        out.log.contains("linux 4"),
        "the slice's sites must be counted under its label:\n{}",
        out.log
    );
    assert!(
        !out.log.contains("aterm_window::sctk_state")
            && !out.log.contains("aterm_window::x11_state"),
        "slice sites must not appear as graphed identities:\n{}",
        out.log
    );
}

#[test]
fn synthetic_vendored_unknown_is_summarized_per_crate() {
    let out = run_synth_files(
        "vendorunknown",
        "fn plain() {\n    let g = alpha_lock.lock().unwrap();\n}\n",
        &[(
            "crates/aterm-window/src/planted.rs",
            "fn opaque(&self) {\n    let g = self.0.lock().unwrap();\n    \
             let h = event_state.lock().unwrap();\n}\n",
        )],
    );
    assert!(
        out.ok,
        "a vendored UNKNOWN cannot cycle — GREEN:\n{}",
        out.log
    );
    assert!(
        out.log
            .contains("0 UNKNOWN-identity site(s) + 1 vendored UNKNOWN site(s)"),
        "the vendored UNKNOWN must be counted separately from aterm's:\n{}",
        out.log
    );
    assert!(
        out.log.contains("aterm_window: 1 site(s)"),
        "the per-crate vendored-UNKNOWN ledger must name the crate:\n{}",
        out.log
    );
    assert!(
        out.log.contains(
            "aterm_window::UNKNOWN@crates/aterm-window/src/planted.rs:2 -> aterm_window::event_state"
        ),
        "the vendored unknown's held-acquire edge must still be reported, \
         namespaced:\n{}",
        out.log
    );
}

#[test]
fn synthetic_propagated_raw_ptr_binding_is_categorized() {
    let out = run_synth(
        "ptrpropagate",
        "fn walk(entries: &mut [u8]) -> u8 {\n    \
         let base = entries.as_mut_ptr();\n    \
         let item = base.add(1);\n    \
         unsafe { item.read() }\n}\n\
         fn no_evidence(other: &Wrapper) {\n    \
         let thing = other.add(1);\n    \
         let v = *thing.read().unwrap();\n}\n",
    );
    assert!(out.ok, "GREEN expected:\n{}", out.log);
    assert!(
        out.log.contains("1 raw-pointer ptr::read")
            && out
                .log
                .contains("receiver `item` proven a raw pointer by its binding"),
        "the propagated binding must be categorized with its evidence:\n{}",
        out.log
    );
    assert!(
        out.log.contains("thing(1)"),
        "an unproven receiver must KEEP its RwLock resolution (fail-closed):\n{}",
        out.log
    );
}

#[test]
fn synthetic_cfg_test_items_are_masked() {
    let out = run_synth(
        "testmask",
        "fn shipped() {\n    let g = alpha_lock.lock().unwrap();\n}\n\
         #[cfg(test)]\nmod tests {\n    fn test_only_abba() {\n        \
         let g = alpha_lock.lock().unwrap();\n        \
         let h = beta_lock.lock().unwrap();\n    }\n    fn test_only_baab() {\n        \
         let g = beta_lock.lock().unwrap();\n        \
         let h = alpha_lock.lock().unwrap();\n    }\n}\n",
    );
    assert!(out.ok, "cfg(test) lock use must not graph:\n{}", out.log);
    assert!(
        !out.log.contains("alpha_lock -> beta_lock"),
        "test-mod edges must be masked; log:\n{}",
        out.log
    );
}

#[test]
fn synthetic_raw_test_fixtures_cannot_end_the_cfg_module_mask() {
    // This source contains multiline raw fixtures with column-zero braces,
    // including the exact dims() double-acquire that must stay test-only.
    let fixtures = include_str!("held_guard.rs");
    let quiet = run_synth("raw-test-fixtures", fixtures);
    assert!(
        quiet.ok,
        "test fixture text is not shipped code:\n{}",
        quiet.log
    );
    let live = format!(
        "{fixtures}\nfn shipped_reentrant() {{\n    let g = alpha_lock.lock().unwrap();\n    let h = alpha_lock.lock().unwrap();\n}}\n"
    );
    let red = run_synth("raw-test-fixtures-live-control", &live);
    assert!(
        !red.ok && red.log.contains("RE-ENTRANCY SUSPECT"),
        "real production acquisition must still fire:\n{}",
        red.log
    );
    assert!(red.log.contains("fn `shipped_reentrant`"), "{}", red.log);
    assert!(
        !red.log.contains("fn `dims`"),
        "raw fixture escaped its test module:\n{}",
        red.log
    );
}

#[test]
fn synthetic_literal_and_comment_attributes_cannot_hide_shipped_locks() {
    for (name, fixture) in [
        ("raw", "const NOTE: &str = r##\"\n#[cfg(test)]\n\"##;"),
        ("raw-byte", "const NOTE: &[u8] = br#\"\n#[cfg(test)]\n\"#;"),
        ("raw-c", "const NOTE: &CStr = cr###\"\n#[cfg(test)]\n\"###;"),
        ("comment", "/* outer\n/* nested */\n#[cfg(test)]\n*/"),
    ] {
        let raw = format!(
            "{fixture}\nfn shipped_reentrant() {{\n    let g = alpha_lock.lock().unwrap();\n    let h = alpha_lock.lock().unwrap();\n}}\n"
        );
        let out = run_synth(name, &raw);
        assert!(
            !out.ok && out.log.contains("RE-ENTRANCY SUSPECT"),
            "{name}: data must not hide production code:\n{}",
            out.log
        );
        assert!(out.log.contains("fn `shipped_reentrant`"), "{}", out.log);
    }
}

#[test]
fn synthetic_diagnostic_literals_are_not_lock_code_and_hide_no_real_code() {
    for (name, prefix) in [
        (
            "cooked-diagnostic",
            "fn logs() {\n    emit(\"a multiline diagnostic\n.lock() is mentioned here\nfinished\");\n",
        ),
        (
            "url-diagnostic",
            "fn logs() {\n    emit(\"https://example.test/.lock()\");\n",
        ),
        (
            "raw-function-diagnostic",
            "fn logs() {\n    emit(r##\"\nfn phantom_reentrant() {\n    let g = alpha_lock.lock();\n    let h = alpha_lock.lock();\n}\n\"##);\n",
        ),
    ] {
        let quiet = run_synth(name, &format!("{prefix}}}\n"));
        assert!(
            quiet.ok,
            "{name}: diagnostic text is not code:\n{}",
            quiet.log
        );
        let live = format!(
            "{prefix}    let g = alpha_lock.lock().unwrap();\n    let h = alpha_lock.lock().unwrap();\n}}\n"
        );
        let red = run_synth(&format!("{name}-live-control"), &live);
        assert!(
            !red.ok && red.log.contains("RE-ENTRANCY SUSPECT"),
            "{name}: real code after the literal must fire:\n{}",
            red.log
        );
        assert!(red.log.contains("fn `logs`"), "{}", red.log);
        assert!(!red.log.contains("fn `phantom_reentrant`"), "{}", red.log);
    }
}

#[test]
fn synthetic_trailing_close_comment_keeps_real_reentrancy_visible() {
    let source = "fn shipped_reentrant() {\n    let g = alpha_lock.lock().unwrap();\n    let h = alpha_lock.lock().unwrap();\n} // the function still ends here\n";
    let out = run_synth("trailing-close-comment", source);
    assert!(
        !out.ok && out.log.contains("RE-ENTRANCY SUSPECT"),
        "a closing-brace comment must not hide the function body:\n{}",
        out.log
    );
    assert!(out.log.contains("fn `shipped_reentrant`"), "{}", out.log);
}

#[test]
fn gated_item_predicates_keep_quoted_values_and_ignore_trailing_comments() {
    let raw = "#[cfg(target_os = \"macos\")] // target-only\nfn hidden() {\n    alpha_lock.lock();\n}\nfn shipped() {\n    beta_lock.lock();\n}\n";
    let masked = mask_items_where(raw, &|attr| attr == "#[cfg(target_os = \"macos\")]");
    assert!(
        !masked.contains("hidden") && !masked.contains("alpha_lock"),
        "target value was lost:\n{masked}"
    );
    assert!(
        masked.contains("fn shipped") && masked.contains("beta_lock"),
        "following production code was masked:\n{masked}"
    );
    assert_eq!(masked.lines().count(), raw.lines().count());
}

#[test]
fn synthetic_standalone_tests_module_does_not_hide_production_unknowns() {
    let out = run_synth_files(
        "standalone-tests",
        "#[cfg(test)]\nmod tests;\n\
         fn shipped() {\n    let g = m.lock().unwrap();\n}\n",
        &[(
            "crates/aterm-gui/src/tests.rs",
            "fn test_only() {\n    let g = factory(42).lock().unwrap();\n}\n",
        )],
    );
    assert!(out.ok, "an UNKNOWN cannot cycle:\n{}", out.log);
    assert!(
        out.log.contains("1 UNKNOWN-identity site(s)"),
        "only the production UNKNOWN must be counted:\n{}",
        out.log
    );
    assert!(out.log.contains("fn `shipped`"), "{}", out.log);
    assert!(!out.log.contains("fn `test_only`"), "{}", out.log);
}

#[test]
fn synthetic_empty_tree_fails_closed() {
    let mut files = crate::scan_set::test_fixtures::workspace_manifests(&[]);
    files.push((
        "crates/aterm-gui/src/main.rs".to_string(),
        "// empty\n".to_string(),
    ));
    files.push((
        "crates/aterm-types/src/lib.rs".to_string(),
        "// empty\n".to_string(),
    ));
    let root = synth_tree("aterm-lock-census", "empty", &files);
    let out = run_lock_order_census(&root);
    assert!(
        !out.ok,
        "a census that walked nothing must FAIL:\n{}",
        out.log
    );
    assert!(
        out.log.contains("ZERO lock-acquisition"),
        "log:\n{}",
        out.log
    );
}

#[test]
fn synthetic_tree_without_manifests_fails_closed() {
    let root = synth_tree(
        "aterm-lock-census",
        "nomanifest",
        &[(
            "crates/aterm-gui/src/main.rs".to_string(),
            "// no Cargo.toml anywhere\n".to_string(),
        )],
    );
    let out = run_lock_order_census(&root);
    assert!(!out.ok, "derivation must fail closed:\n{}", out.log);
    assert!(
        out.log.contains("SCAN-SET DERIVATION FAILED"),
        "log:\n{}",
        out.log
    );
}

#[test]
fn synthetic_stale_helper_registration_is_red() {
    let mut files = synth_helper_files();
    replace_file(
        &mut files,
        "crates/aterm-gui/src/lib.rs",
        "// no term_lock here\n",
    );
    files.push((
        "crates/aterm-gui/src/extra.rs".to_string(),
        "fn f() {\n    let g = alpha_lock.lock().unwrap();\n}\n".to_string(),
    ));
    let root = synth_tree("aterm-lock-census", "stalehelper", &files);
    let out = run_lock_order_census(&root);
    assert!(
        !out.ok,
        "a stale helper registration must be RED:\n{}",
        out.log
    );
    assert!(out.log.contains("no longer defined"), "log:\n{}", out.log);
}

#[test]
fn synthetic_new_gui_dependency_is_scanned_automatically() {
    let newdep = ("aterm-newdep", "[dependencies]\n");
    let extras: Vec<(&str, &str)> = std::iter::once(newdep)
        .chain(synth_helper_crates())
        .collect();
    let mut files = crate::scan_set::test_fixtures::workspace_manifests(&extras);
    files.extend(synth_helper_files().into_iter().filter(|(p, _)| {
        // Keep one manifest set.
        !p.ends_with("Cargo.toml")
    }));
    files.push((
        "crates/aterm-newdep/src/lib.rs".to_string(),
        "fn newdep_touch() {\n    let g = newdep_registry.lock().unwrap();\n    \
         let h = newdep_queue.lock().unwrap();\n}\n"
            .to_string(),
    ));
    let root = synth_tree("aterm-lock-census", "newdepgreen", &files);
    let out = run_lock_order_census(&root);
    assert!(out.ok, "GREEN expected:\n{}", out.log);
    let crates = 3 + synth_helper_crates().len();
    assert!(
        out.log
            .contains(&format!("across {crates} workspace crate(s)"))
            && out.log.contains("crates/aterm-newdep/src"),
        "the new dependency must be scanned automatically:\n{}",
        out.log
    );
    assert!(
        out.log.contains("newdep_registry(1)")
            && out.log.contains("newdep_registry -> newdep_queue"),
        "its sites and edges must be counted:\n{}",
        out.log
    );
    let mut files = crate::scan_set::test_fixtures::workspace_manifests(&extras);
    files.extend(
        synth_helper_files()
            .into_iter()
            .filter(|(p, _)| !p.ends_with("Cargo.toml")),
    );
    files.push((
        "crates/aterm-gui/src/extra.rs".to_string(),
        "fn take_ab() {\n    let g = alpha_lock.lock().unwrap();\n    \
         let h = beta_lock.lock().unwrap();\n}\n"
            .to_string(),
    ));
    files.push((
        "crates/aterm-newdep/src/lib.rs".to_string(),
        "fn take_ba() {\n    let g = beta_lock.lock().unwrap();\n    \
         let h = alpha_lock.lock().unwrap();\n}\n"
            .to_string(),
    ));
    let root = synth_tree("aterm-lock-census", "newdepabba", &files);
    let out = run_lock_order_census(&root);
    assert!(
        !out.ok,
        "an ABBA half-planted in the new dependency must be RED:\n{}",
        out.log
    );
    assert!(
        out.log.contains("CYCLE") && out.log.contains("crates/aterm-newdep/src/lib.rs:2"),
        "the cycle diagnostic must name the new crate's sites:\n{}",
        out.log
    );
}
