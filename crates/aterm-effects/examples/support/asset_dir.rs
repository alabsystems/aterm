// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The roster asset-dir reader the `gen_*_glyphs` generators share
//! (`#[path]`-included into each example binary and the drift tests, never the
//! library): every `.toml` under one roster dir as `(file name, text)` pairs
//! ordered by file name, the shape `aterm_effects::cat_glyphs_codegen`
//! consumes.
//!
//! It lives beside the examples because the shipped engine names no `std::fs`
//! (docs/ATERM_DESIGN.md §2.7; fenced by `tools/grep_guard.sh` lane 6a):
//! reading files is the generator binary's job, and the drift tests regenerate
//! through this same reader.

use std::path::Path;

/// Read every `.toml` in `dir` — non-TOML files (the authoring scripts that
/// live beside the pet poses) are skipped by extension — ordered by file name.
///
/// # Errors
/// An unreadable dir or file, named.
pub fn read_toml_dir(dir: &Path) -> Result<Vec<(String, String)>, String> {
    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("read_dir {}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("toml"))
        .collect();
    files.sort();
    files
        .iter()
        .map(|path| {
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            let text = std::fs::read_to_string(path).map_err(|e| format!("read {name}: {e}"))?;
            Ok((name, text))
        })
        .collect()
}
