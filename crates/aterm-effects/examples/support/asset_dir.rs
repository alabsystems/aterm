// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Roster `.toml` asset-dir reader shared by the glyph generators and drift tests.

use std::path::Path;

/// Reads every `.toml` file in `dir`, ordered by file name.
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
