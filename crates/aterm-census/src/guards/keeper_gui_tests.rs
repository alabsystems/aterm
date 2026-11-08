// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::super::{Corpus, Tree};
use super::keeper_gui_deps;
use crate::scan_set::test_fixtures::{disk_tree, workspace_manifests};

fn findings(
    bridge: &str,
    manifest: &str,
    source: &str,
    extra: &[(&str, &str)],
) -> Vec<super::Finding> {
    let mut files = workspace_manifests(&[
        (
            "aterm-keeper",
            "[dependencies]\naterm-uds = { workspace = true }\n",
        ),
        (
            "aterm-uds",
            &format!(
                "[dependencies]\nbridge = {{ package = \"{bridge}\", path = \"../{bridge}\" }}\n"
            ),
        ),
        (bridge, manifest),
    ]);
    for dir in ["aterm-keeper", "aterm-uds"] {
        files.push((format!("crates/{dir}/src/lib.rs"), String::new()));
    }
    files.push((format!("crates/{bridge}/src/lib.rs"), source.to_owned()));
    files.push(("Cargo.lock".into(), "version = 4\n".into()));
    files.extend(
        extra
            .iter()
            .map(|(path, text)| (format!("crates/{bridge}/{path}"), (*text).to_owned())),
    );
    let root = disk_tree("aterm-census-guards", "keeper-native-source", &files);
    let corpus = Corpus::read(&root).unwrap();
    let tree = Tree {
        root: &root,
        corpus: &corpus,
        engine: crate::guards::tests::NO_ENGINE,
    };
    let mut out = Vec::new();
    keeper_gui_deps(&tree, &mut out);
    let _ = std::fs::remove_dir_all(root);
    out
}

#[test]
fn foundation_declarations_alone_do_not_name_gui_apis() {
    let source = "#[link(name = \"objc\")]\nunsafe extern \"C\" { fn objc_msgSend(); }\n\
                  #[link(name = \"Foundation\", kind = \"framework\")]\nunsafe extern \"C\" {}\n\
                  #[link(name = \"CoreFoundation\", kind = \"framework\")]\nunsafe extern \"C\" {}\n";
    assert!(findings("aterm-objc", "", source, &[]).is_empty());
}

#[test]
fn new_dependency_gui_link_is_found_in_loaded_module() {
    let found = findings(
        "platform-glue",
        "",
        "mod ffi;\n",
        &[(
            "src/ffi.rs",
            "#[link(name = \"CoreGraphics\", kind = \"framework\")]\nunsafe extern \"C\" {}\n",
        )],
    );
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].path, "crates/platform-glue/src/ffi.rs");
    assert!(found[0].detail.contains("CoreGraphics"));
}

#[test]
fn dynamically_opened_gui_framework_is_found() {
    let found = findings(
        "platform-glue",
        "",
        "fn open() { load(c\"/System/Library/Frameworks/AppKit.framework/AppKit\"); }\n",
        &[],
    );
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].detail.contains("AppKit"));
}

#[test]
fn test_only_gui_module_does_not_change_process_linkage() {
    let found = findings(
        "aterm-objc",
        "",
        "#[cfg(test)]\nmod ffi;\n",
        &[(
            "src/ffi.rs",
            "#[link(name = \"AppKit\", kind = \"framework\")]\nunsafe extern \"C\" {}\n",
        )],
    );
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn custom_library_root_is_inspected_instead_of_unused_default_source() {
    let found = findings(
        "platform-glue",
        "[lib]\npath = \"native/entry.rs\"\n",
        "#[link(name = \"AppKit\", kind = \"framework\")]\nunsafe extern \"C\" {}\n",
        &[(
            "native/entry.rs",
            "#[link(name = \"Metal\", kind = \"framework\")]\nunsafe extern \"C\" {}\n",
        )],
    );
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].path, "crates/platform-glue/native/entry.rs");
}

#[test]
fn build_script_gui_link_directive_is_found() {
    let found = findings(
        "platform-glue",
        "",
        "",
        &[(
            "build.rs",
            "fn main() { println!(\"cargo:rustc-link-lib=framework=AppKit\"); }\n",
        )],
    );
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].path, "crates/platform-glue/build.rs");
}

#[test]
fn known_gui_crate_name_without_gui_source_is_not_a_finding() {
    assert!(
        findings(
            "aterm-render",
            "",
            "pub fn add(a: u32, b: u32) -> u32 { a + b }\n",
            &[]
        )
        .is_empty()
    );
}

#[test]
fn pure_dependency_is_a_positive_control() {
    assert!(findings("platform-glue", "", "pub fn value() -> u32 { 1 }\n", &[]).is_empty());
}

#[test]
fn opaque_gui_package_is_still_rejected() {
    let found = findings(
        "platform-glue",
        "[dependencies]\nwinit = \"0.30\"\n",
        "",
        &[],
    );
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].detail.contains("winit"));
}

#[test]
fn custom_build_script_gui_link_is_found() {
    let found = findings(
        "platform-glue",
        "build = \"native/link.rs\"\n",
        "",
        &[(
            "native/link.rs",
            "fn main() { println!(\"cargo::rustc-link-lib=framework=Metal\"); }\n",
        )],
    );
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].path, "crates/platform-glue/native/link.rs");
}

#[test]
fn explicitly_disabled_build_script_does_not_link_gui() {
    let found = findings(
        "platform-glue",
        "build = false\n",
        "",
        &[(
            "build.rs",
            "fn main() { println!(\"cargo:rustc-link-lib=framework=AppKit\"); }\n",
        )],
    );
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn direct_keeper_and_uds_runtime_sends_remain_forbidden() {
    let corpus = Corpus::of(&[
        (
            "crates/aterm-keeper/src/lib.rs",
            "unsafe extern \"C\" { fn objc_msgSend(); }\n",
        ),
        (
            "crates/aterm-uds/src/lib.rs",
            "unsafe extern \"C\" { fn objc_msgSend(); }\n",
        ),
    ]);
    let tree = Tree {
        root: std::path::Path::new("."),
        corpus: &corpus,
        engine: crate::guards::tests::NO_ENGINE,
    };
    let mut found = Vec::new();
    super::keeper_gui(&tree, &mut found);
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(
        found
            .iter()
            .all(|f| f.rule == "keeper-gui" && f.detail.contains("objc_msgSend"))
    );
}
