// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The roster asset-dir reader the five `gen_*_glyphs` generators share
//! (`#[path]`-included, so it compiles into each example binary and into the
//! drift tests, never into the library): every `.toml` under one roster dir as
//! `(file name, text)` pairs ordered by file name — the shape
//! `aterm_effects::cat_glyphs_codegen` consumes.
//!
//! It lives beside the examples rather than in the crate because the SHIPPED
//! engine names no `std::fs` (docs/ATERM_DESIGN.md §2.7; `tools/grep_guard.sh`
//! lane 6a fences aterm-effects since 2026-08-30, the design's Phase 0):
//! reading files is the generator BINARY's job, the generator itself is a pure
//! function of the texts, and the drift tests regenerate from this same reader
//! so what they compare is exactly what the example would write.

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
