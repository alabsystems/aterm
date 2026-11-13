// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

#[test]
fn test_disabled_evaluation_requires_complete_syntax_and_settles_only_test() {
    for predicate in [
        "test",
        "all(test, unix)",
        "all(feature = \"a,b\", test)",
        "not(not(test))",
        "any(test, test)",
    ] {
        assert_eq!(when_test_disabled(predicate), Truth::False, "{predicate}");
    }
    for predicate in [
        "any(test, debug_assertions)",
        "any(test, feature = \"x\")",
        "all(test, broken(,))",
        "all(test, unix))",
        "all(test,,unix)",
        "all(test, feature = \"\\q\")",
        "all(test, feature = \"unfinished)",
    ] {
        assert_eq!(when_test_disabled(predicate), Truth::Unknown, "{predicate}");
    }
    assert_eq!(when_test_disabled("not(test)"), Truth::True);
    assert_eq!(on_wasm32("all(unix, broken(,))"), Truth::Unknown);
    assert_eq!(on_wasm32("target_os = \"un\\known\""), Truth::Unknown);
}

#[test]
fn the_platforms_wasm32_is_not_are_false_and_its_own_are_true() {
    for pred in [
        "unix",
        "windows",
        "test",
        "target_os = \"macos\"",
        "target_os = \"linux\"",
        "target_vendor = \"apple\"",
        "target_family = \"unix\"",
        "target_arch = \"aarch64\"",
        "not(target_arch = \"wasm32\")",
        "any(unix, windows)",
        "all(windows, any(test, debug_assertions))",
        "any(target_vendor = \"apple\", target_os = \"linux\")",
    ] {
        assert_eq!(on_wasm32(pred), Truth::False, "{pred}");
    }
    for pred in [
        "target_arch = \"wasm32\"",
        "target_family = \"wasm\"",
        "target_os = \"unknown\"",
        "not(unix)",
        "not(any(unix, windows))",
        "any(unix, target_arch = \"wasm32\")",
        "all(not(windows), target_pointer_width = \"32\")",
    ] {
        assert_eq!(on_wasm32(pred), Truth::True, "{pred}");
    }
}

#[test]
fn what_the_target_does_not_settle_is_unknown() {
    for pred in [
        "debug_assertions",
        "feature = \"dev-seams\"",
        "trust_verify",
        "target_feature = \"atomics\"",
        "any(test, debug_assertions)",
        "not(feature = \"x\")",
        "all(target_arch = \"wasm32\", feature = \"x\")",
        "any(windows, feature = \"x\")",
        r#"target_arch = "wasm\u{33}2""#,
        r#"target_arch = "wasm\x33\x32""#,
        "not(unix, windows)",
        "target_os = macos",
        "",
    ] {
        assert_eq!(on_wasm32(pred), Truth::Unknown, "{pred}");
    }
}

#[test]
fn a_target_table_links_unless_it_is_false_on_wasm32() {
    assert!(!target_links_on_wasm32("cfg(windows)"));
    assert!(!target_links_on_wasm32("cfg(any(unix, windows))"));
    assert!(!target_links_on_wasm32("cfg(target_os = \"macos\")"));
    assert!(!target_links_on_wasm32("x86_64-pc-windows-msvc"));
    assert!(target_links_on_wasm32("cfg(target_arch = \"wasm32\")"));
    assert!(target_links_on_wasm32("cfg(feature = \"x\")"));
    assert!(target_links_on_wasm32("cfg(not(unix))"));
    assert!(target_links_on_wasm32("wasm32-unknown-unknown"));
}

#[test]
fn only_a_test_or_a_false_gate_drops_an_item() {
    assert!(attr_never_on_wasm32("#[test]"));
    assert!(attr_never_on_wasm32("#[cfg(test)]"));
    assert!(attr_never_on_wasm32("#[cfg(windows)]"));
    assert!(attr_never_on_wasm32(
        "#[cfg(not(target_arch = \"wasm32\"))]"
    ));
    assert!(!attr_never_on_wasm32("#[cfg(debug_assertions)]"));
    assert!(!attr_never_on_wasm32("#[cfg(not(windows))]"));
    assert!(!attr_never_on_wasm32(
        "#[cfg_attr(windows, allow(dead_code))]"
    ));
    assert!(!attr_never_on_wasm32("#[cfg(windows)] fn f() {}"));
}

fn src_tree(files: &[(&str, &str)]) -> (BTreeMap<PathBuf, String>, BTreeSet<PathBuf>) {
    let map = files
        .iter()
        .map(|(p, t)| (Path::new("/c/src").join(p), (*t).to_string()))
        .collect();
    let roots = [Path::new("/c/src/lib.rs").to_path_buf()].into();
    (map, roots)
}

fn names(set: &BTreeSet<PathBuf>) -> Vec<String> {
    set.iter()
        .map(|p| crate::rel_path(Path::new("/c/src"), p))
        .collect()
}

#[test]
fn a_module_only_a_false_gate_loads_is_no_wasm32_file() {
    let (files, roots) = src_tree(&[
        (
            "lib.rs",
            "// The Metal arm.\n#[cfg(target_os = \"macos\")]\nmod metal;\n\
             #[cfg(feature = \"x\")]\nmod featured;\npub(crate) mod plain;\n\
             #[cfg(all(test, target_os = \"macos\"))]\n#[path = \"plain/shared.rs\"]\n\
             mod shared_twin;\npub mod a;\n",
        ),
        ("metal/mod.rs", "mod worker;\npub fn f() {}\n"),
        (
            "metal/worker.rs",
            "pub fn go() {\n    std::thread::spawn(|| {});\n}\n",
        ),
        ("featured.rs", "pub fn g() {}\n"),
        ("plain/mod.rs", "mod shared;\n"),
        ("plain/shared.rs", "pub fn s() {}\n"),
        ("a.rs", "#[cfg(windows)]\nmod b;\nmod c;\n"),
        ("a/b.rs", "pub fn b() {}\n"),
        ("a/c.rs", "pub fn c() {}\n"),
    ]);
    let got = unloaded_on_wasm32(&files, &roots);
    assert_eq!(
        names(&got.files),
        ["a/b.rs", "metal/mod.rs", "metal/worker.rs"],
        "{got:?}"
    );
    assert_eq!(got.unplaced, None);
    let cuts: Vec<String> = got
        .cuts
        .iter()
        .map(|c| {
            format!(
                "{}:{} `{}` under {}",
                crate::slash(&c.file),
                c.line,
                c.loader,
                c.gate
            )
        })
        .collect();
    assert_eq!(
        cuts,
        [
            "/c/src/a.rs:2 `mod b;` under #[cfg(windows)]",
            "/c/src/lib.rs:3 `mod metal;` under #[cfg(target_os = \"macos\")]",
        ]
    );
}

#[test]
fn a_crate_root_is_never_left_out() {
    let (files, _) = src_tree(&[
        (
            "main.rs",
            "#[cfg(windows)]\nmod lib2;\n#[cfg(windows)]\nmod inner;\n",
        ),
        ("lib2.rs", "mod inner;\n"),
        ("inner.rs", "pub fn i() {}\n"),
    ]);
    let roots: BTreeSet<PathBuf> = [Path::new("/c/src/lib2.rs").to_path_buf()].into();
    let got = unloaded_on_wasm32(&files, &roots);
    assert!(got.files.is_empty(), "{got:?}");
}

#[test]
fn a_module_tree_this_reader_cannot_place_leaves_every_file_in() {
    let gated = "#[cfg(windows)]\nmod win;\n";
    for (what, extra) in [
        ("inline", "mod outer {\n    mod win;\n}\n"),
        ("include", "include!(concat!(\"gen\", \".rs\"));\n"),
        ("cfg_attr", "#[cfg_attr(unix, path = \"u.rs\")]\nmod imp;\n"),
        ("path", "#[path = concat!(\"w\", \".rs\")]\nmod imp;\n"),
    ] {
        let lib = format!("{gated}{extra}");
        let (files, roots) = src_tree(&[("lib.rs", &lib), ("win.rs", "pub fn w() {}\n")]);
        let got = unloaded_on_wasm32(&files, &roots);
        assert!(got.files.is_empty(), "{what}: {got:?}");
        assert!(got.unplaced.is_some(), "{what}: says why");
    }
    let false_alias = "#[cfg(windows)]\n#[path = \"outer/live.rs\"]\nmod hidden;\n";
    for (extra, stays_in) in [
        ("mod outer { mod live; }\n", true),
        ("mod outer {\nmod live;\n}\n", true),
        ("mod outer {\n    mod live;\n}\n", true),
        (
            "mod outer {\nconst S: &str = r#\"\n}\n\"#;\n/* } */\nmod live;\n}\n",
            true,
        ),
        ("", false),
        ("mod outer {}\n", false),
        ("#[cfg(windows)]\nmod outer {\nmod live;\n}\n", false),
    ] {
        let lib = format!("{false_alias}{extra}");
        let (files, roots) = src_tree(&[("lib.rs", &lib), ("outer/live.rs", "pub fn live() {}\n")]);
        let got = unloaded_on_wasm32(&files, &roots);
        if stays_in {
            assert!(
                got.files.is_empty(),
                "live nested loader {extra:?}: {got:?}"
            );
            assert!(got.unplaced.is_some(), "nested loader is not placed");
        } else {
            assert_eq!(names(&got.files), ["outer/live.rs"], "CONTROL: {got:?}");
            assert_eq!(got.unplaced, None);
        }
    }
    let lib = format!(
        "{false_alias}mod harmless {{\n    fn f() {{}}\n}}\n\
         #[path = \"outer/live.rs\"]\nmod live;\n"
    );
    let (files, roots) = src_tree(&[("lib.rs", &lib), ("outer/live.rs", "pub fn live() {}\n")]);
    let got = unloaded_on_wasm32(&files, &roots);
    assert!(
        got.files.is_empty(),
        "root loader after inline module: {got:?}"
    );
    assert_eq!(got.unplaced, None, "the root loader is placed");
    for extra in [
        "",
        "#[cfg(windows)]\nmod outer {\n    mod win;\n}\n",
        "pub(crate) mod webgl {\n    fn f() {}\n}\n",
    ] {
        let lib = format!("{gated}{extra}");
        let (files, roots) = src_tree(&[("lib.rs", &lib), ("win.rs", "pub fn w() {}\n")]);
        let got = unloaded_on_wasm32(&files, &roots);
        assert_eq!(names(&got.files), ["win.rs"], "CONTROL {extra:?}: {got:?}");
    }
}

#[test]
fn an_include_is_a_loader_like_a_mod() {
    let (files, roots) = src_tree(&[
        (
            "lib.rs",
            "#[cfg(windows)]\nfn win() {\n    include!(\"win_gen.rs\");\n}\n\
             fn table() {\n    include!(\"table_gen.rs\");\n}\n\
             #[cfg(windows)]\nmod table_gen;\n\
             include!(concat!(env!(\"OUT_DIR\"), \"/built.rs\"));\n",
        ),
        ("win_gen.rs", "pub fn w() {}\n"),
        ("table_gen.rs", "pub fn t() {}\n"),
    ]);
    let got = unloaded_on_wasm32(&files, &roots);
    assert_eq!(names(&got.files), ["win_gen.rs"], "{got:?}");
    assert_eq!(got.unplaced, None);
    assert_eq!(
        got.cuts
            .iter()
            .map(|c| (c.line, c.gate.as_str()))
            .collect::<Vec<_>>(),
        [(3, "#[cfg(windows)]")],
        "the masked fn's gate is the include's: {got:?}"
    );
    let (files, roots) = src_tree(&[
        (
            "lib.rs",
            "#[cfg(windows)]\nmod win;\ninclude!(\"parts.rs\");\n",
        ),
        ("parts.rs", "mod part;\n"),
        ("part.rs", "pub fn p() {}\n"),
        ("win.rs", "pub fn w() {}\n"),
    ]);
    let got = unloaded_on_wasm32(&files, &roots);
    assert!(got.files.is_empty(), "{got:?}");
    assert!(
        got.unplaced
            .is_some_and(|why| why.contains("included file"))
    );
}
