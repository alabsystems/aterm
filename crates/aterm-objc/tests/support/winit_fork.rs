// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The absorbed winit's macOS backend (`crates/aterm-window`), read as SOURCE
//! by `winit_seam.rs`.

use std::path::{Path, PathBuf};

/// The fork's macOS backend, as a path this crate can read.
///
pub fn fork_backend() -> PathBuf {
    let base =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../aterm-window/src/platform_impl/macos");
    assert!(
        base.is_dir(),
        "the absorbed winit's macOS backend is not at {}",
        base.display()
    );
    base
}

/// Every `.rs` file under [`fork_backend`], as `(path relative to it, source)`,
/// sorted by path.
///
/// RECURSIVE: a `read_dir` that assumed the flat directory of today would go
/// blind the day someone adds a subdirectory.
pub fn fork_rs_files() -> Vec<(String, String)> {
    let dir = fork_backend();
    let mut stack = vec![dir.clone()];
    let mut paths = Vec::new();
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).expect("the fork's macOS backend is readable") {
            let path = entry.expect("a readable entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|x| x == "rs") {
                paths.push(path);
            }
        }
    }
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let file = path
                .strip_prefix(&dir)
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned();
            let src = std::fs::read_to_string(&path).expect("readable");
            (file, src)
        })
        .collect()
}

/// [`fork_rs_files`] with `//` comments removed and the newlines kept, so a
/// line number is still recoverable. The fork's own prose spells the patterns
/// the walks look for (`.install(`, `sel!(`, `class(c"`), and so does the prose
/// of the files that walk it.
pub fn fork_sources() -> Vec<(String, String)> {
    fork_rs_files()
        .into_iter()
        .map(|(file, src)| {
            let stripped: String = src
                .lines()
                .map(|l| l.split("//").next().unwrap_or(""))
                .collect::<Vec<_>>()
                .join("\n");
            (file, stripped)
        })
        .collect()
}
