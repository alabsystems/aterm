// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Cross-checks between `atpkg` and harness ALab package marker classification.

use std::path::Path;

use super::codex_alab as cx;
use crate::supervise::ScratchDir;

/// `<root>/bin/codex` under a `codex-package.json` of `manifest`.
fn lay(root: &Path, manifest: &str, marker: Marker) {
    std::fs::create_dir_all(root.join("bin")).expect("bin");
    std::fs::write(root.join("bin/codex"), b"x").expect("exe");
    std::fs::write(root.join(cx::PACKAGE_JSON), manifest).expect("manifest");
    match marker {
        Marker::None => {}
        Marker::File => std::fs::write(root.join(cx::ALAB_BUILD_JSON), "{}").expect("marker"),
        Marker::Dir => std::fs::create_dir_all(root.join(cx::ALAB_BUILD_JSON)).expect("dir"),
    }
}

#[derive(Clone, Copy, Debug)]
enum Marker {
    None,
    File,
    Dir,
}

#[test]
fn atpkg_and_the_harness_give_one_answer_on_every_package_shape() {
    let dir = ScratchDir::new("alab-cross", "corpus");
    let good = r#"{"layoutVersion":1,"version":"0.164.0-dev.12+gabc","entrypoint":"bin/codex"}"#;
    let corpus: &[(&str, &str, Marker, bool)] = &[
        ("alab", good, Marker::File, true),
        ("vendor", good, Marker::None, false),
        ("marker-dir", good, Marker::Dir, false),
        (
            "layout-2",
            r#"{"layoutVersion":2,"entrypoint":"bin/codex"}"#,
            Marker::File,
            false,
        ),
        (
            "escapes",
            r#"{"layoutVersion":1,"entrypoint":"../bin/codex"}"#,
            Marker::File,
            false,
        ),
        (
            "absolute",
            r#"{"layoutVersion":1,"entrypoint":"/bin/codex"}"#,
            Marker::File,
            false,
        ),
        (
            "other-entry",
            r#"{"layoutVersion":1,"entrypoint":"bin/other"}"#,
            Marker::File,
            false,
        ),
        (
            "dot-entry",
            r#"{"layoutVersion":1,"entrypoint":"./bin/codex"}"#,
            Marker::File,
            true,
        ),
        (
            "empty-entry",
            r#"{"layoutVersion":1,"entrypoint":""}"#,
            Marker::File,
            false,
        ),
        ("no-entry", r#"{"layoutVersion":1}"#, Marker::File, false),
        ("not-json", "not json", Marker::File, false),
    ];
    for (name, manifest, marker, alab) in corpus {
        let root = dir.join(name);
        lay(&root, manifest, *marker);
        let exe = root.join("bin/codex");
        assert!(
            !atpkg::alab::exe_is_alab(&exe) || cx::alab_bytes(&exe),
            "{name}: atpkg calls ALab what the harness does not"
        );
        assert_eq!(cx::alab_bytes(&exe), *alab, "{name}: the harness");
        assert_eq!(
            atpkg::alab::exe_is_alab(&exe),
            *alab,
            "{name}: atpkg's exe form"
        );
        assert_eq!(
            atpkg::alab::is_alab_package(&root),
            *alab,
            "{name}: atpkg's root form"
        );
        assert_eq!(
            cx::package_entrypoint(manifest),
            atpkg::alab::package_entrypoint(manifest),
            "{name}: the entrypoint"
        );
    }
    #[cfg(unix)]
    {
        let link = dir.join("current");
        std::os::unix::fs::symlink(dir.join("alab"), &link).expect("link");
        let entry = dir.join("exec-codex");
        std::os::unix::fs::symlink(dir.join("alab/bin/codex"), &entry).expect("entry");
        for exe in [link.join("bin/codex"), entry] {
            assert!(cx::alab_bytes(&exe), "{}", exe.display());
            assert!(atpkg::alab::exe_is_alab(&exe), "{}", exe.display());
        }
    }
    #[cfg(unix)]
    for (name, linked) in [
        ("linked-manifest", cx::PACKAGE_JSON),
        ("linked-entry", "bin/codex"),
        ("linked-marker", cx::ALAB_BUILD_JSON),
    ] {
        let root = dir.join(name);
        lay(&root, good, Marker::File);
        let real = dir.join(format!("{name}.real"));
        std::fs::rename(root.join(linked), &real).expect("move");
        std::os::unix::fs::symlink(&real, root.join(linked)).expect("link");
        let exe = root.join("bin/codex");
        assert!(!cx::alab_bytes(&exe), "{name}: the harness");
        assert!(!atpkg::alab::exe_is_alab(&exe), "{name}: atpkg's exe form");
        assert!(
            !atpkg::alab::is_alab_package(&root),
            "{name}: atpkg's root form"
        );
    }
    #[cfg(unix)]
    {
        let root = dir.join("linked-in-root-entry");
        lay(
            &root,
            r#"{"layoutVersion":1,"version":"0.164.0-dev.12+gabc","entrypoint":"bin/alias"}"#,
            Marker::File,
        );
        std::os::unix::fs::symlink(root.join("bin/codex"), root.join("bin/alias")).expect("alias");
        let exe = root.join("bin/codex");
        assert!(!cx::alab_bytes(&exe), "linked-in-root-entry: the harness");
        assert!(
            !atpkg::alab::exe_is_alab(&exe),
            "linked-in-root-entry: atpkg's exe form"
        );
        assert!(
            !atpkg::alab::is_alab_package(&root),
            "linked-in-root-entry: atpkg's root form"
        );
    }
}
