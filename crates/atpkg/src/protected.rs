// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! macOS TCC-protected folders ([`under_protected_root`](crate::protected::under_protected_root), [`HOME_FOLDERS`](crate::protected::HOME_FOLDERS),
//! [`LIBRARY_DOMAINS`](crate::protected::LIBRARY_DOMAINS), [`aterm_containment::sbpl::APP_DATA_ROOTS`], [`ABSOLUTE_ROOTS`](crate::protected::ABSOLUTE_ROOTS)).

use std::borrow::Cow;
use std::ffi::OsStr;
use std::path::{Component, Components, Path, PathBuf};

#[path = "protected_path.rs"]
mod path;
#[cfg(unix)]
pub(crate) use path::PathRefusal;
pub(crate) use path::resolve_path;

#[cfg(target_os = "macos")]
#[path = "protected_mac.rs"]
mod mac;

/// Compare a spelling before filesystem I/O. macOS reserves case/canonical aliases even on
/// case-sensitive volumes: probing a guarded target to learn its volume's rules can ask for
/// consent. Other platforms retain exact component matching. Invalid UTF-8 remains raw bytes.
fn component_eq(left: &OsStr, right: &OsStr) -> bool {
    if left == right {
        return true;
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::ffi::OsStrExt as _;
        let (a, b) = (left.as_bytes(), right.as_bytes());
        if a.is_ascii() && b.is_ascii() {
            return a.eq_ignore_ascii_case(b);
        }
        match (left.to_str(), right.to_str()) {
            // An allocation failure cannot establish that a possible guarded alias differs.
            (Some(a), Some(b)) => mac::equivalent(a, b).unwrap_or(true),
            _ => a.eq_ignore_ascii_case(b),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// The remaining components when `path` starts with `root`, under the lexical policy above.
fn relative_under<'a>(root: &Path, path: &'a Path) -> Option<Components<'a>> {
    let mut parts = path.components();
    for expected in root.components() {
        if !component_eq(expected.as_os_str(), parts.next()?.as_os_str()) {
            return None;
        }
    }
    Some(parts)
}

/// The per-folder consent services under `$HOME`, by directory name.
pub const HOME_FOLDERS: &[&str] = &[
    "Documents",
    "Desktop",
    "Downloads",
    "Pictures",
    "Movies",
    "Music",
];

/// The file-provider domains under `~/Library`, as `(first, second)` components.
pub const LIBRARY_DOMAINS: &[(&str, &str)] =
    &[("Library", "Mobile Documents"), ("Library", "CloudStorage")];

/// Roots that are protected wherever `$HOME` is.
pub const ABSOLUTE_ROOTS: &[&str] = &["/Volumes"];

/// macOS's data volume: its firmlinks (`/Users`, `/Volumes`, `/private`, …) reach each folder of
/// it at a `/`-rooted spelling too, so `/System/Volumes/Data/Users//x/Documents` is
/// `/Users//x/Documents` before any look.
const DATA_VOLUME: &str = "/System/Volumes/Data";

/// The spellings of `path` that name one folder before any look ([`DATA_VOLUME`]): as written
/// and, under the data volume, at its `/`-rooted spelling.
fn spellings(path: &Path) -> impl Iterator<Item = Cow<'_, Path>> {
    let rooted = relative_under(Path::new(DATA_VOLUME), path)
        .map(|rest| Cow::Owned(PathBuf::from("/").join(rest.as_path())));
    std::iter::once(Cow::Borrowed(path)).chain(rooted)
}

/// Whether `judge` holds for `home` and `path` at any of their [`spellings`].
fn any_spelling(home: &Path, path: &Path, judge: fn(&Path, &Path) -> bool) -> bool {
    spellings(home).any(|home| spellings(path).any(|path| judge(&home, &path)))
}

/// Whether `path` is `root` or lies under it, at any spelling macOS resolves to one folder before
/// a look (case, Unicode form, the data volume's firmlinks), component by component: lexical, so
/// it reads no file. `/USERS/x` and `/System/Volumes/Data/Users//x` are at `/Users//x`, and every
/// folder lies under `/System/Volumes/Data`'s rooted spelling `/`.
#[must_use]
pub fn at_or_under(root: &Path, path: &Path) -> bool {
    spellings(root).any(|root| spellings(path).any(|path| relative_under(&root, &path).is_some()))
}

/// Whether `path` lies at or under a root macOS guards with a consent dialog, at any spelling
/// macOS resolves to that root before a look (case, Unicode form, the data volume's firmlinks).
#[must_use]
pub fn under_protected_root(home: &Path, path: &Path) -> bool {
    any_spelling(home, path, protected_as_spelled)
}

/// [`under_protected_root`] for one spelling of `home` and `path`.
fn protected_as_spelled(home: &Path, path: &Path) -> bool {
    if ABSOLUTE_ROOTS
        .iter()
        .any(|root| relative_under(Path::new(root), path).is_some())
    {
        return true;
    }
    let Some(rest) = relative_under(home, path) else {
        return false;
    };
    if rest.clone().any(|c| matches!(c, Component::ParentDir)) {
        return true;
    }
    let mut components = rest.filter_map(|component| match component {
        Component::Normal(name) => Some(name),
        _ => None,
    });
    let Some(first) = components.next() else {
        return false; // `$HOME` itself
    };
    if HOME_FOLDERS
        .iter()
        .any(|folder| component_eq(first, OsStr::new(folder)))
    {
        return true;
    }
    let Some(second) = components.next() else {
        return false;
    };
    LIBRARY_DOMAINS.iter().any(|(lib, domain)| {
        component_eq(first, OsStr::new(lib)) && component_eq(second, OsStr::new(domain))
    }) || app_data_as_spelled(home, path)
}

/// Whether `path` is under a file-provider domain ([`under_protected_root`], [`LIBRARY_DOMAINS`]).
#[must_use]
pub fn under_file_provider_domain(home: &Path, path: &Path) -> bool {
    any_spelling(home, path, file_provider_as_spelled)
}

/// [`under_file_provider_domain`] for one spelling of `home` and `path`.
fn file_provider_as_spelled(home: &Path, path: &Path) -> bool {
    let Some(rest) = relative_under(home, path) else {
        return false;
    };
    if rest.clone().any(|c| matches!(c, Component::ParentDir)) {
        return true;
    }
    let mut components = rest.filter_map(|component| match component {
        Component::Normal(name) => Some(name),
        _ => None,
    });
    let (Some(first), Some(second)) = (components.next(), components.next()) else {
        return false;
    };
    LIBRARY_DOMAINS.iter().any(|(lib, domain)| {
        component_eq(first, OsStr::new(lib)) && component_eq(second, OsStr::new(domain))
    })
}

/// Whether `path` lies under an app-data root ([`under_file_provider_domain`], [`HOME_FOLDERS`]).
#[must_use]
pub fn under_app_data(home: &Path, path: &Path) -> bool {
    any_spelling(home, path, app_data_as_spelled)
}

/// [`under_app_data`] for one spelling of `home` and `path`.
fn app_data_as_spelled(home: &Path, path: &Path) -> bool {
    let Some(rest) = relative_under(home, path) else {
        return false;
    };
    if rest.clone().any(|c| matches!(c, Component::ParentDir)) {
        return true;
    }
    aterm_containment::sbpl::APP_DATA_ROOTS
        .iter()
        .any(|root| relative_under(Path::new(root), rest.as_path()).is_some())
}

#[cfg(all(test, target_os = "macos"))]
#[path = "protected_mac_alias_tests.rs"]
mod mac_alias_tests;
#[cfg(test)]
#[path = "protected_tests.rs"]
mod tests;
