// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Vendor-file admission shared by the explicit drift bench and Unix fixtures.

use crate::vendor_drift::version_in;

// Mirrors `atpkg::platform::default_prefix` (`aterm-phase` has no `atpkg` dependency).
fn canary_prefix(home: &std::path::Path) -> std::path::PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library")
            .join("Application Support")
            .join("aterm")
            .join("pkg")
    } else {
        home.join(".local").join("share").join("aterm").join("pkg")
    }
}

/// Resolves a managed binary path from `aterm pkg which <program>` output.
pub(crate) fn canary_path(
    which_line: &str,
    home: Option<&std::path::Path>,
) -> Option<std::path::PathBuf> {
    let (head, said) = which_line
        .split_once(" \u{2014} ")
        .unwrap_or((which_line, ""));
    let program = head.split(" \u{2192} ").next()?.trim();
    if said.starts_with("managed")
        && let Some(end) = head
            .rsplit(" \u{2192} ")
            .next()
            .map(str::trim)
            .filter(|end| in_store(end, program))
    {
        return Some(end.into());
    }
    let current = canary_prefix(home?)
        .join("store")
        .join(program)
        .join("current")
        .join("bin")
        .join(program);
    current.exists().then_some(current)
}

fn in_store(path: &str, program: &str) -> bool {
    let parts: Vec<&std::ffi::OsStr> = std::path::Path::new(path)
        .components()
        .map(|c| c.as_os_str())
        .collect();
    parts.windows(2).any(|w| w[0] == "store" && w[1] == program)
}

/// Reads the build version from the sibling `<build>.vendor` record ([`version_in`]).
pub(crate) fn build_version(path: &str) -> Option<String> {
    use std::path::{Path, PathBuf};
    let path = Path::new(path);
    let build_of = |bin_dir: Option<PathBuf>| bin_dir?.parent().map(Path::to_path_buf);
    let record = [
        build_of(path.parent().and_then(|d| d.canonicalize().ok())),
        build_of(
            path.canonicalize()
                .ok()
                .and_then(|f| f.parent().map(Path::to_path_buf)),
        ),
    ]
    .into_iter()
    .flatten()
    .find_map(|build| std::fs::read_to_string(format!("{}.vendor", build.display())).ok())?;
    let version = record
        .lines()
        .filter_map(|l| l.split_once('='))
        .find(|(k, _)| k.trim() == "version")?
        .1;
    version_in(version.trim().trim_matches('"'))
}

/// Loads Gemini CLI's npm `.js` bundle and version from `bin`.
pub(crate) fn gemini_bundle(bin: &std::path::Path) -> Option<(String, Vec<u8>, Option<String>)> {
    let script = std::fs::canonicalize(bin).ok()?;
    let dir = script.parent()?;
    let package = std::fs::read_to_string(dir.parent()?.join("package.json")).ok()?;
    if !package.contains("\"@google/gemini-cli\"") {
        return None;
    }
    let version = package
        .split_once("\"version\"")
        .and_then(|(_, rest)| rest.split('"').nth(1))
        .map(str::to_string);
    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "js"))
        .collect();
    files.sort();
    let mut bytes = Vec::new();
    for file in files {
        bytes.extend(std::fs::read(file).ok()?);
        bytes.push(0);
    }
    Some((dir.display().to_string(), bytes, version))
}
