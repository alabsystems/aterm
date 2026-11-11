// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The explicitly selected macOS CLI, separate from a window's physical bundle.

use std::path::Path;

/// Prefers the installed app selected by `~/.local/bin/aterm` behind the managed reroute and agent directories.
#[must_use]
pub fn selected_path(
    home: Option<&Path>,
    reroute: Option<&str>,
    agents: Option<&str>,
    inherited: Option<&str>,
) -> Option<(String, String)> {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = home?.join(".local/bin");
        let link = dir.join("aterm");
        if !std::fs::symlink_metadata(&link)
            .ok()?
            .file_type()
            .is_symlink()
        {
            return None;
        }
        let target = link.canonicalize().ok()?;
        let metadata = target.metadata().ok()?;
        let macos = target.parent()?;
        let app = macos.parent()?.parent()?;
        if !metadata.is_file()
            || metadata.permissions().mode() & 0o111 == 0
            || target.file_name()? != "aterm"
            || !macos.ends_with("Contents/MacOS")
            || app.extension()? != "app"
        {
            return None;
        }
        let selected = dir.to_str()?;
        let mut front = Vec::new();
        for entry in [reroute, agents, Some(selected)].into_iter().flatten() {
            if !front.contains(&entry) {
                front.push(entry);
            }
        }
        let mut entries = front.clone();
        entries.extend(
            inherited
                .filter(|path| !path.is_empty())
                .into_iter()
                .flat_map(|path| path.split(':'))
                .filter(|entry| !front.contains(entry)),
        );
        Some(("PATH".into(), entries.join(":")))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (home, reroute, agents, inherited);
        None
    }
}

#[cfg(all(test, target_os = "macos"))]
#[path = "cli_path_tests.rs"]
mod tests;
