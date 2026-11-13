// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Identifies the running copy of aterm and any other copies in usual install locations.

use std::path::{Path, PathBuf};

/// Why a running copy is inert ([`Running::InertApp`]).
pub use crate::install_posture::InstallPosture;

/// The macOS classification, shared with the updater's bundle admission.
#[cfg(target_os = "macos")]
pub use crate::bundle::posture_from;

/// The running copy's [`InstallPosture`] ([`crate::bundle::resolve_layout`]).
#[cfg(target_os = "macos")]
#[must_use]
pub fn running_posture() -> Option<InstallPosture> {
    let exe = std::fs::canonicalize(std::env::current_exe().ok()?).ok()?;
    Some(posture_from(&exe))
}

/// What the running copy is — decides whether the other-copy clause is made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Running {
    /// An installed `.app` the updater may replace (`bundle::resolve` semantics).
    InstalledApp,
    /// An `.app` the updater never touches.
    InertApp,
    /// Not a bundle: a bare executable.
    Binary,
}

/// One `aterm.app` found at a usual install location that is not the running one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OtherCopy {
    /// The `.app` root.
    pub path: PathBuf,
    /// Its `CFBundleShortVersionString`.
    pub version: Option<String>,
}

/// The answer: the running copy, what kind of thing it is, and the other copies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WhichCopy {
    /// macOS `.app` root containing the executable; otherwise the executable path.
    pub running: PathBuf,
    /// See [`Running`].
    pub kind: Running,
    /// Other `aterm.app` bundles in the usual places, in probe order.
    pub others: Vec<OtherCopy>,
}

impl WhichCopy {
    /// The running copy's row value.
    #[must_use]
    pub fn running_detail(&self) -> String {
        match self.kind {
            Running::InertApp => format!(
                "{} (the updater leaves this copy alone)",
                self.running.display()
            ),
            Running::InstalledApp | Running::Binary => self.running.display().to_string(),
        }
    }

    /// One other copy's row value.
    #[must_use]
    pub fn other_detail(&self, other: &OtherCopy) -> String {
        let version = other.version.as_deref().unwrap_or("version unknown");
        let mut detail = format!("{} ({version})", other.path.display());
        if self.kind == Running::InstalledApp {
            detail.push_str(" \u{2014} the updater leaves it alone");
        }
        detail
    }

    /// The report as printed lines.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        let mut out = vec![format!("running: {}", self.running_detail())];
        out.extend(
            self.others
                .iter()
                .map(|other| format!("another copy: {}", self.other_detail(other))),
        );
        out
    }
}

/// Observe this process.
#[must_use]
pub fn observe() -> Option<WhichCopy> {
    let exe = std::env::current_exe().ok()?;
    #[cfg(unix)]
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    Some(observe_from(&exe))
}

#[cfg(target_os = "macos")]
fn observe_from(exe: &Path) -> WhichCopy {
    survey(exe, &usual_app_locations())
}

/// [`observe`]'s `kind` alone, without probing for other copies ([`Running::Binary`]).
#[must_use]
pub fn running_kind() -> Running {
    let Ok(exe) = std::env::current_exe() else {
        return Running::Binary;
    };
    #[cfg(unix)]
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    kind_of(&exe)
}

/// [`running_kind`]'s pure core, for a given executable path.
#[must_use]
pub fn kind_of(exe: &Path) -> Running {
    #[cfg(target_os = "macos")]
    {
        classify(exe).1
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = exe;
        Running::Binary
    }
}

/// The running copy's root and kind.
#[cfg(target_os = "macos")]
fn classify(exe: &Path) -> (PathBuf, Running) {
    match crate::bundle::layout_of(exe) {
        Some(bundle) => {
            let owned = crate::bundle::resolve_from(exe)
                .is_some_and(|b| !crate::bundle::is_dev_marked(&b.app_root));
            (
                bundle.app_root,
                if owned {
                    Running::InstalledApp
                } else {
                    Running::InertApp
                },
            )
        }
        None => (exe.to_path_buf(), Running::Binary),
    }
}

/// Off macOS there is no bundle and no updater lane.
#[cfg(not(target_os = "macos"))]
fn observe_from(exe: &Path) -> WhichCopy {
    WhichCopy {
        running: exe.to_path_buf(),
        kind: Running::Binary,
        others: Vec::new(),
    }
}

/// Pure core of [`observe`].
#[cfg(target_os = "macos")]
#[must_use]
pub fn survey(exe: &Path, candidates: &[PathBuf]) -> WhichCopy {
    let (running, kind) = classify(exe);
    let mut others: Vec<OtherCopy> = Vec::new();
    for candidate in candidates {
        if !is_real_dir(candidate) {
            continue;
        }
        if crate::install::same_install_root(candidate, &running)
            || others
                .iter()
                .any(|other| crate::install::same_install_root(candidate, &other.path))
        {
            continue;
        }
        others.push(OtherCopy {
            path: candidate.clone(),
            version: plist_short_version(candidate),
        });
    }
    WhichCopy {
        running,
        kind,
        others,
    }
}

/// A directory that is really there, not a symlink to one.
#[cfg(target_os = "macos")]
fn is_real_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_dir())
}

/// The most Caskroom version directories the report looks inside.
#[cfg(target_os = "macos")]
const MAX_CASKROOM_ENTRIES: usize = 16;

/// The longest version string another copy's plist may contribute.
#[cfg(any(target_os = "macos", test))]
const MAX_VERSION_CHARS: usize = 32;

#[cfg(any(target_os = "macos", test))]
#[must_use]
fn plausible_version(version: &str) -> Option<&str> {
    let ok = !version.is_empty()
        && version.len() <= MAX_VERSION_CHARS
        && version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'-'));
    ok.then_some(version)
}

/// `CFBundleShortVersionString` from a bundle's XML `Info.plist` ([`plausible_version`]).
#[cfg(target_os = "macos")]
fn plist_short_version(app_root: &Path) -> Option<String> {
    let text = crate::read_ledger_text(&app_root.join("Contents/Info.plist"))?;
    crate::manifest::xml_plist_string(&text, "<key>CFBundleShortVersionString</key>")
        .and_then(plausible_version)
        .map(str::to_string)
}

/// The usual places an `aterm.app` lands.
#[cfg(target_os = "macos")]
#[must_use]
pub fn usual_app_locations() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let brew = std::env::var_os("HOMEBREW_PREFIX").map(PathBuf::from);
    let install_dir = std::env::var_os("ATERM_INSTALL_DIR").map(PathBuf::from);
    app_locations(home.as_deref(), brew.as_deref(), install_dir.as_deref())
}

#[cfg(target_os = "macos")]
fn app_locations(
    home: Option<&Path>,
    homebrew_prefix: Option<&Path>,
    install_dir: Option<&Path>,
) -> Vec<PathBuf> {
    let mut out = Vec::new();
    push_unique_location(&mut out, PathBuf::from("/Applications/aterm.app"));
    if let Some(home) = home.filter(|path| path.is_absolute()) {
        push_unique_location(&mut out, home.join("Applications").join("aterm.app"));
    }
    if let Some(dir) = install_dir.filter(|d| !d.as_os_str().is_empty()) {
        push_unique_location(&mut out, dir.join("aterm.app"));
    }
    for prefix in homebrew_prefixes(homebrew_prefix) {
        let caskroom = prefix.join("Caskroom");
        for token in ["aterm", "alab"] {
            for app in caskroom_bundles(&caskroom.join(token)) {
                push_unique_location(&mut out, app);
            }
        }
    }
    out
}

#[cfg(target_os = "macos")]
fn homebrew_prefixes(homebrew_prefix: Option<&Path>) -> Vec<PathBuf> {
    let mut prefixes = vec![PathBuf::from("/opt/homebrew"), PathBuf::from("/usr/local")];
    if let Some(prefix) = homebrew_prefix.filter(|path| path.is_absolute())
        && !prefixes.iter().any(|p| p == prefix)
    {
        prefixes.push(prefix.to_path_buf());
    }
    prefixes
}

#[cfg(target_os = "macos")]
fn push_unique_location(locations: &mut Vec<PathBuf>, path: PathBuf) {
    if !locations.contains(&path) {
        locations.push(path);
    }
}

/// `<caskroom>/<version>/aterm.app` for each version directory present ([`MAX_CASKROOM_ENTRIES`]).
#[cfg(target_os = "macos")]
fn caskroom_bundles(caskroom: &Path) -> Vec<PathBuf> {
    if !is_real_dir(caskroom) {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(caskroom) else {
        return Vec::new();
    };
    let mut versions: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| {
            !entry.file_name().as_encoded_bytes().starts_with(b".")
                && entry.file_type().is_ok_and(|t| t.is_dir())
        })
        .map(|entry| entry.path())
        .collect();
    versions.sort();
    versions.truncate(MAX_CASKROOM_ENTRIES);
    versions
        .into_iter()
        .map(|version| version.join("aterm.app"))
        .filter(|app| is_real_dir(app))
        .collect()
}

#[cfg(test)]
#[path = "which_copy_tests.rs"]
mod tests;
