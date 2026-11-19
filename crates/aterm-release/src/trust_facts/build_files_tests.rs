// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::ledger::{git_ok, testing};
use crate::trust_facts::{COSTS, tree_inputs};
use std::fs;
use std::os::unix::fs::symlink;

#[test]
fn script_declarations_require_regular_keyed_files_of_an_unambiguous_package() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    testing::init(
        &repo,
        &[
            ("user.name", "Test"),
            ("user.email", "test@example.invalid"),
            ("commit.gpgsign", "false"),
        ],
    );
    let package = repo.join("crates/aterm-gui");
    fs::create_dir_all(&package).unwrap();
    let manifest = package.join("Cargo.toml");
    fs::write(&manifest, "[package]\nname = 'aterm-gui'\n").unwrap();
    fs::write(package.join("input"), "tracked").unwrap();
    let sibling = repo.join("crates/aterm-update-core/src");
    fs::create_dir_all(&sibling).unwrap();
    fs::write(sibling.join("pins.rs"), "tracked pins").unwrap();
    fs::write(repo.join(".gitignore"), "ignored\n").unwrap();
    fs::write(package.join("ignored"), "outside the key").unwrap();
    fs::create_dir_all(repo.join(COSTS).parent().unwrap()).unwrap();
    fs::write(repo.join(COSTS), "tracked but outside the key").unwrap();
    symlink("input", package.join("link")).unwrap();
    symlink("aterm-gui", repo.join("crates/alias")).unwrap();
    let git = testing::Git::new(&repo);
    git_ok(&git, &["add", "."]).unwrap();
    git_ok(&git, &["commit", "-qm", "source and declared inputs"]).unwrap();
    fs::write(package.join("loose"), "untracked").unwrap();

    let mut tracked = BTreeSet::new();
    tree_inputs(&git, "HEAD", |path| {
        tracked.insert(path.to_owned());
    })
    .unwrap();
    let metadata = aterm_json::json!({"packages": [
        {"name":"aterm-gui","manifest_path":manifest},
        {"name":"ambiguous","manifest_path":manifest,"version":"1.0.0"},
        {"name":"ambiguous","manifest_path":tmp.path().join("Cargo.toml"),"version":"2.0.0"},
        {"name":"unavailable"},
    ]});
    let roots = Packages::from_metadata(&metadata, &repo);
    let gui_input = |path: &Path| roots.covers(&tracked, "aterm-gui", path);
    for path in [
        "input",
        "../aterm-update-core/src/pins.rs",
        "../../crates/aterm-update-core/src/pins.rs",
        "../aterm-update-core/src/../../aterm-gui/input",
    ] {
        assert!(gui_input(Path::new(path)), "{path}");
        assert!(gui_input(&package.join(path)), "{path}");
    }
    for path in [
        "missing/../input",
        "input/../input",
        "link/../input",
        "../alias/../aterm-gui/input",
        "../aterm-update/../aterm-gui/input",
        "../../../repo/crates/aterm-gui/input",
        "../../crates/aterm-release/trust/costs.tsv",
        "../aterm-update-core/src",
        "../aterm-update-core/src/pins.rs/.",
        "input/",
        "ignored",
        "loose",
        "link",
        ".",
    ] {
        assert!(!gui_input(Path::new(path)), "{path}");
        assert!(!gui_input(&package.join(path)), "{path}");
    }
    for path in [
        tmp.path().join("input"),
        repo.join(COSTS),
        repo.join("crates/alias/input"),
    ] {
        assert!(!gui_input(&path), "{}", path.display());
    }
    for name in ["ambiguous", "unavailable", "unknown"] {
        assert!(!roots.covers(&tracked, name, Path::new("input")), "{name}");
    }
    let overlong = "./".repeat(16 * 1024) + "input";
    assert!(!gui_input(Path::new(&overlong)));
}

fn target(name: &str, kind: &[&str], types: &[&str]) -> Value {
    aterm_json::json!({"name":name, "kind":kind, "crate_types":types})
}

#[test]
fn examples_use_exact_target_names_and_declared_crate_types() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let root = tmp.path().join("repo");
    let manifest = root.join("Cargo.toml");
    let inventory = vec![
        target("ordinary-example", &["example"], &["bin"]),
        target("library-example", &["example"], &["rlib", "staticlib"]),
        target("library-example", &["bin"], &["bin"]),
        target("plain", &["lib"], &["lib"]),
    ];
    let package =
        aterm_json::json!({"name":"example-shapes", "manifest_path":manifest, "targets":inventory});
    let metadata = aterm_json::json!({"packages":[package.clone()]});
    let packages = Packages::from_metadata(&metadata, &root);
    assert_eq!(
        packages.example_is_binary("example-shapes", "ordinary-example"),
        Some(true)
    );
    assert_eq!(
        packages.example_is_binary("example-shapes", "library-example"),
        Some(false)
    );
    assert_eq!(
        packages.example_is_binary("example-shapes", "ordinary_example"),
        None
    );
    assert_eq!(packages.example_is_binary("example-shapes", "plain"), None);
    assert!(packages.build_scripts("example-shapes").unwrap().is_empty());

    let outside = aterm_json::json!({"name":"example-shapes", "manifest_path":tmp.path().join("Cargo.toml"), "targets":inventory});
    for rows in [vec![outside.clone()], vec![package, outside]] {
        let unknown = Packages::from_metadata(&aterm_json::json!({"packages":rows}), &root);
        assert_eq!(
            unknown.example_is_binary("example-shapes", "ordinary-example"),
            None
        );
        assert_eq!(unknown.build_scripts("example-shapes"), None);
    }
}

#[test]
fn build_script_inventory_preserves_raw_targets_and_refuses_incomplete_metadata() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let root = tmp.path();
    let metadata = |targets: Vec<Value>| {
        Packages::from_metadata(
            &aterm_json::json!({"packages":[{
                "name":"tools", "manifest_path":root.join("Cargo.toml"), "targets":targets
            }]}),
            root,
        )
    };
    let library = target("build_script_support", &["lib"], &["lib"]);
    let declared = |name: &str, path: &Path| {
        aterm_json::json!({
            "name":name, "kind":["custom-build"], "crate_types":["bin"], "src_path":path
        })
    };
    let local = root.join("scripts/foo-bar.rs");
    let outside = root.parent().unwrap().join("foo_bar.rs");
    let first = declared("build-script-foo-bar", &local);
    let second = declared("build-script-foo_bar", &outside);
    let packages = metadata(vec![library.clone(), first.clone(), second]);
    let scripts = packages.build_scripts("tools").unwrap();
    assert_eq!(scripts.len(), 2);
    let keyed = [
        PathBuf::from("Cargo.toml"),
        PathBuf::from("scripts/foo-bar.rs"),
    ]
    .into();
    assert!(scripts["build-script-foo-bar"].covered_by(|path| {
        assert_eq!(path, local);
        packages.covers(&keyed, "tools", path)
    }));
    assert!(!scripts["build-script-foo_bar"].covered_by(|path| {
        assert_eq!(path, outside);
        packages.covers(&keyed, "tools", path)
    }));
    let null = |name: &str| {
        aterm_json::json!({
            "name":name, "kind":["custom-build"], "crate_types":["bin"], "src_path":null
        })
    };
    let generated = metadata(vec![library.clone(), null("metabuild-tools")]);
    assert!(generated.build_scripts("tools").unwrap()["metabuild-tools"].covered_by(|_| false));
    let missing = metadata(vec![
        library.clone(),
        target("metabuild-tools", &["custom-build"], &["bin"]),
        null("metabuild-other"),
        null("build-script-ordinary"),
        target("build-script-missing", &["custom-build"], &["bin"]),
        declared("build-script-relative", Path::new("relative.rs")),
    ]);
    let sources = missing.build_scripts("tools").unwrap();
    for name in [
        "metabuild-tools",
        "metabuild-other",
        "build-script-ordinary",
        "build-script-missing",
        "build-script-relative",
    ] {
        assert!(!sources[name].covered_by(|_| true), "{name}");
    }
    assert!(
        metadata(vec![library.clone()])
            .build_scripts("tools")
            .unwrap()
            .is_empty()
    );
    assert_eq!(metadata(vec![]).build_scripts("tools"), None);

    let example = target("ordinary", &["example"], &["bin"]);
    for invalid in [
        aterm_json::json!({"name":"partial","crate_types":["bin"]}),
        aterm_json::json!({"name":"partial","kind":["example"]}),
        target("partial", &[], &["bin"]),
        target("foreign", &["unrecognized"], &["bin"]),
        target("", &["custom-build"], &["bin"]),
        target("build-script-", &["custom-build"], &["bin"]),
        target("unexpected", &["custom-build"], &["bin"]),
        target("build-script-nested/path", &["custom-build"], &["bin"]),
        target("build-script-lib", &["custom-build"], &["rlib"]),
        target("empty", &["example"], &[]),
        target("unknown", &["example"], &["future"]),
        target("mixed", &["example"], &["bin", "rlib"]),
        first.clone(),
        example.clone(),
    ] {
        let packages = metadata(vec![
            library.clone(),
            example.clone(),
            first.clone(),
            invalid,
        ]);
        assert_eq!(packages.build_scripts("tools"), None);
        assert_eq!(packages.example_is_binary("tools", "ordinary"), None);
    }
}
