// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The vendored winit fork's macOS backend, read as SOURCE by the guards in
//! `winit_seam.rs` and `winit_sent_prototypes.rs`.

use std::path::{Path, PathBuf};

/// The fork's macOS backend, as a path this crate can read.
///
/// Factored out because several guards in two files read the fork rather than
/// one, and a census whose file path is written at each of them is a census
/// with that many places to go stale.
pub fn fork_backend() -> PathBuf {
    let base =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../vendor/winit/src/platform_impl/macos");
    assert!(
        base.is_dir(),
        "the vendored winit macOS backend is not at {}",
        base.display()
    );
    base
}

/// Every `.rs` file under [`fork_backend`], as `(path relative to it, source)`,
/// sorted by path.
///
/// RECURSIVE. The directory is flat today, and a `read_dir` that assumed so
/// would be a scope that goes stale the day someone adds a subdirectory — the
/// failure mode `objc2_exit_condition.rs` exists to name, where a blind spot
/// widens silently and every test stays green.
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
