// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use std::path::{Path, PathBuf};

use super::{ABSORBED_CRATES, AbsorbedCrate, FORK_LEDGER};
pub(crate) use crate::tree::memory::Tree;

/// Default `(package, path, namespace)` fork tuple used by test fixtures.
pub(crate) const FIXTURE_FORK: (&str, &str, &str) = ("indexmap", "vendor/indexmap", "indexmap");

/// Formats a synthetic `vendor/forge.toml` ledger for `forks`.
pub(crate) fn ledger(forks: &[(&str, &str, &str)]) -> String {
    let mut text = String::from("[forge]\nloc_method = \"x\"\n");
    for (package, path, namespace) in forks {
        text.push_str(&format!(
            "\n# census review: a fixture fork\n[[fork]]\nname = \"{package}\"\n\
             version = \"1.0.0\"\npath = \"{path}\"\nlicense = \"MIT\"\n\
             apache_notice = false\ncensus.mode = \"scanned\"\n\
             census.namespace = \"{namespace}\"\n"
        ));
    }
    text
}

/// Returns the repository root directory.
pub(crate) fn repo_root() -> PathBuf {
    aterm_tempfile::manifest_dir()
        .parent()
        .and_then(Path::parent)
        .expect("crates/aterm-census lives two levels under the repo root")
        .to_path_buf()
}

/// Builds an in-memory synthetic checkout tree from `files`.
pub(crate) fn synth_tree(prefix: &str, name: &str, files: &[(String, String)]) -> Tree {
    Tree::new(&format!("{prefix}-{name}"), files)
}

/// Writes `files` into a temporary directory on disk.
pub(crate) fn disk_tree(prefix: &str, name: &str, files: &[(String, String)]) -> PathBuf {
    let root = aterm_tempfile::Builder::new()
        .prefix(&format!("{prefix}-{name}-"))
        .suffix(".noindex")
        .tempdir()
        .expect("fixture directory")
        .keep();
    for (rel, contents) in files {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().expect("rel has a parent")).expect("mkdir");
        std::fs::write(&path, contents).expect("write synth file");
    }
    root
}

/// Derives the fixture package name for an [`AbsorbedCrate`].
pub(crate) fn member_name(row: &AbsorbedCrate) -> &'static str {
    row.path.rsplit('/').next().unwrap_or(row.path)
}

/// Builds a minimal `[patch.crates-io]` table and vendor stub files for tests.
pub(crate) fn patch_fixture() -> (String, Vec<(String, String)>) {
    let mut table = String::from("[patch.crates-io]\n");
    let (package, path, _) = FIXTURE_FORK;
    table.push_str(&format!("{package} = {{ path = \"{path}\" }}\n"));
    let mut stubs = vec![
        (
            format!("{path}/Cargo.toml"),
            format!("[package]\nname = \"{package}\"\n"),
        ),
        (format!("{path}/src/lib.rs"), "// stub\n".to_string()),
        (FORK_LEDGER.to_string(), ledger(&[FIXTURE_FORK])),
    ];
    let mut notice = String::from("NOTICE\n");
    for a in ABSORBED_CRATES {
        notice.push_str(&format!("- {} (`{}/`)\n", a.upstream, a.path));
        stubs.push((
            format!("{}/Cargo.toml", a.path),
            format!("[package]\nname = \"{}\"\n", member_name(a)),
        ));
        stubs.push((format!("{}/LICENSE", a.path), "upstream\n".to_string()));
        stubs.push((format!("{}/src/lib.rs", a.path), "// stub\n".to_string()));
        for slice in a.platform_slices {
            for p in slice.paths {
                let rel = if p.ends_with(".rs") {
                    format!("{}/{p}", a.path)
                } else {
                    format!("{}/{p}/stub.rs", a.path)
                };
                stubs.push((rel, "// stub\n".to_string()));
            }
        }
    }
    stubs.push(("NOTICE".to_string(), notice));
    (table, stubs)
}

/// Builds minimal workspace manifests with `aterm-gui`, `aterm-types`, and `extras`.
pub(crate) fn workspace_manifests(extras: &[(&str, &str)]) -> Vec<(String, String)> {
    let (patch_table, mut files) = patch_fixture();
    let mut root = String::from("[workspace]\nmembers = [\"crates/*\"]\n");
    root.push_str("[workspace.dependencies]\n");
    root.push_str("aterm-types = { path = \"crates/aterm-types\" }\n");
    for (name, _) in extras {
        root.push_str(&format!("{name} = {{ path = \"crates/{name}\" }}\n"));
    }
    root.push_str(&patch_table);
    files.push(("Cargo.toml".to_string(), root));
    let mut gui = String::from(
        "[package]\nname = \"aterm-gui\"\n[dependencies]\n\
         aterm-types = { workspace = true }\n",
    );
    for (name, _) in extras {
        gui.push_str(&format!("{name} = {{ workspace = true }}\n"));
    }
    files.push(("crates/aterm-gui/Cargo.toml".to_string(), gui));
    files.push((
        "crates/aterm-types/Cargo.toml".to_string(),
        "[package]\nname = \"aterm-types\"\n".to_string(),
    ));
    for (name, sections) in extras {
        files.push((
            format!("crates/{name}/Cargo.toml"),
            format!("[package]\nname = \"{name}\"\n{sections}"),
        ));
    }
    files
}
