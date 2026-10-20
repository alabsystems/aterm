// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Vendor-file admission shared by the explicit drift bench and Unix fixtures.

use crate::vendor_drift::version_in;

/// Where the store keeps managed programs under `home`: MIRRORS
/// `atpkg::platform::default_prefix` (this crate has no dependencies, and a
/// moved prefix makes a canary say `COULD NOT RUN`, never pass).
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

/// The path the drift canaries read a program's build at, from the first line
/// `aterm pkg which <program>` prints. Its head ends in the copy that runs in
/// the shell that asked, and that is the MANAGED build only when it lies in
/// the store (`claude → <shim> → … → <prefix>/store/claude/<build>/bin/claude
/// — managed 2.1.286 — …`). Any other — a system copy, one SHADOWED by a copy
/// ahead on PATH, or a managed program whose row says `not what runs outside
/// aterm: … this shell's own copy` (atpkg's outside-aterm rows, `state.rs`) —
/// names a copy aterm does not manage (an old brew build judged against the
/// managed build's anchors fails the test). The managed copy is then read
/// where the store keeps it
/// (`<prefix>/store/<program>/current/bin/<program>`), and `None` where that
/// is not there either.
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

/// Whether `path` lies in a store's tree for `program` (`…/store/<program>/…`),
/// the layout [`canary_path`]'s fallback reads.
fn in_store(path: &str, program: &str) -> bool {
    let parts: Vec<&std::ffi::OsStr> = std::path::Path::new(path)
        .components()
        .map(|c| c.as_os_str())
        .collect();
    parts.windows(2).any(|w| w[0] == "store" && w[1] == program)
}

/// The version atpkg's record beside the build names (`<build>.vendor`'s
/// `version = "…"`: the build of the binary's directory, reached through
/// a `current` link, else of the file a link from outside the store
/// names), or `None` where there is no record ([`version_in`]). The binary is never
/// run, as the Gemini canary reads its `package.json`: a vendor build the
/// suite starts runs with the owner's HOME, and its `--version` is no
/// pure read there (measured 2026-10-17 in a scratch HOME: Muse Code
/// 1.4.10's takes `~/.config/muse/.auth.json.lock`, Codex 0.163.0's makes
/// its PATH-alias helpers under `~/.codex`).
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

/// Gemini CLI's npm bundle, found from `bin` (a `gemini` on PATH, a
/// Homebrew link or the script itself): the directory the script
/// resolves into, every `.js` file in it (joined, NUL between), and the
/// version its package's `package.json` (one up) names. `None` where
/// `bin` leads to no `@google/gemini-cli` package: a stub, another
/// program of that name.
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
