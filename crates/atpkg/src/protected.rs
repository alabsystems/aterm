// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! macOS TCC-protected folders ([`under_protected_root`](crate::protected::under_protected_root), [`HOME_FOLDERS`](crate::protected::HOME_FOLDERS),
//! [`LIBRARY_DOMAINS`](crate::protected::LIBRARY_DOMAINS), [`aterm_containment::sbpl::APP_DATA_ROOTS`], [`ABSOLUTE_ROOTS`](crate::protected::ABSOLUTE_ROOTS)).

use std::ffi::OsStr;
use std::path::{Component, Components, Path};

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

/// Whether `path` lies at or under a root macOS guards with a consent dialog.
#[must_use]
pub fn under_protected_root(home: &Path, path: &Path) -> bool {
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
    }) || under_app_data(home, path)
}

/// Whether `path` is under a file-provider domain ([`under_protected_root`], [`LIBRARY_DOMAINS`]).
#[must_use]
pub fn under_file_provider_domain(home: &Path, path: &Path) -> bool {
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
mod mac_alias_tests {
    use super::*;

    #[test]
    fn mac_alias_spellings_keep_component_boundaries_and_raw_nonprotected_names() {
        let home = Path::new("/Users//CaseUser");
        for path in [
            "/Users//CaseUser/documents/dots",
            "/users/CASEUSER/Documentſ/dots",
            "/USERS/caseuser/lIbRaRy/cLoUdStOrAgE/dots",
            "/Users//CaseUser/LIBRARY/GROUP CONTAINERS/dots",
            "/volumes/External/dots",
        ] {
            assert!(under_protected_root(home, Path::new(path)), "{path}");
        }
        assert!(under_protected_root(
            Path::new("/Users/Åhome"),
            Path::new("/users/a\u{30a}HOME/Documents/dots")
        ));
        assert!(under_protected_root(
            Path::new("/Users/ßhome"),
            Path::new("/users/SSHOME/Documents/dots")
        ));
        assert!(under_file_provider_domain(
            home,
            Path::new("/users/caseuser/LIBRARY/CLOUDSTORAGE/dots")
        ));
        assert!(under_app_data(
            home,
            Path::new("/users/caseuser/library/containers/app")
        ));
        for path in [
            "/Users//CaseUser/documents-backup/dots",
            "/Users//CaseUser/Library/CloudStorageElsewhere/dots",
            "/Users//CaseUser/Library/Application Support/aterm",
            "/Users//CaseUser/dot-雪",
            "/Users//CaseUser/Döcuments/dots",
            "/Users//CaseUserOther/Documents/dots",
            "/VolumesElsewhere/dots",
        ] {
            assert!(!under_protected_root(home, Path::new(path)), "{path}");
        }
        use std::os::unix::ffi::OsStringExt as _;
        let raw = home.join(std::ffi::OsString::from_vec(b"dot-\xff".to_vec()));
        assert!(!under_protected_root(home, &raw));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// The file-provider predicate is strictly narrower than its sibling: true for
    /// the two sync domains, false for every other protected root.
    #[test]
    fn only_the_file_provider_domains_are_named_by_the_narrow_predicate() {
        let home = PathBuf::from("/Users//someone");
        let provider = [
            "/Users//someone/Library/Mobile Documents",
            "/Users//someone/Library/Mobile Documents/com~apple~CloudDocs/notes.md",
            "/Users//someone/Library/CloudStorage",
            "/Users//someone/Library/CloudStorage/SomeProvider-someone/notes.md",
        ];
        for p in provider {
            assert!(
                under_file_provider_domain(&home, Path::new(p)),
                "{p} is a file-provider domain"
            );
            assert!(
                under_protected_root(&home, Path::new(p)),
                "{p} is also protected at large"
            );
        }
        let protected_elsewhere = [
            "/Users//someone/Documents/notes.md",
            "/Users//someone/Desktop/notes.md",
            "/Users//someone/Downloads/a/b",
            "/Users//someone/Pictures/x",
            "/Users//someone/Library/Containers/com.apple.Notes/Data",
            "/Users//someone/Library/Group Containers/group.com.apple.notes",
            "/Volumes/External/notes.md",
        ];
        for p in protected_elsewhere {
            assert!(
                !under_file_provider_domain(&home, Path::new(p)),
                "{p} is protected but is not a file-provider domain"
            );
        }
        for p in [
            "/Users//someone/Library",
            "/Users//someone/Library/CloudStoragex/a",
            "/Users//someone/Library/Application Support/aterm",
            "/Users//other/Library/CloudStorage/x",
            "/Users//someone",
        ] {
            assert!(
                !under_file_provider_domain(&home, Path::new(p)),
                "{p} must not match"
            );
        }
    }

    /// Every protected root answers `true`; shapes that merely resemble one answer `false`.
    #[test]
    fn the_protected_roots_are_recognised_per_component_and_nothing_else_is() {
        let home = PathBuf::from("/Users//someone");
        let protected = [
            "/Users//someone/Documents",
            "/Users//someone/Documents/dotfiles/zshrc",
            "/Users//someone/Desktop/notes.md",
            "/Users//someone/Downloads/a/b/c",
            "/Users//someone/Pictures/x",
            "/Users//someone/Movies/x",
            "/Users//someone/Music/x",
            "/Users//someone/Library/Mobile Documents",
            "/Users//someone/Library/Mobile Documents/com~apple~CloudDocs/dotfiles/zshrc",
            "/Users//someone/Library/CloudStorage",
            "/Users//someone/Library/CloudStorage/SomeProvider-someone/dotfiles/zshrc",
            "/Users//someone/Library/Containers",
            "/Users//someone/Library/Containers/com.apple.Notes/Data/zshrc",
            "/Users//someone/Library/Group Containers",
            "/Users//someone/Library/Group Containers/group.com.apple.notes/zshrc",
            "/Volumes/External/dotfiles/zshrc",
            "/Volumes",
        ];
        for p in protected {
            assert!(
                under_protected_root(&home, Path::new(p)),
                "{p} is under a protected root"
            );
        }
        let clear = [
            "/Users//someone",
            "/Users//someone/.zshrc",
            "/Users//someone/.config/fish/config.fish",
            "/Users//someone/dotfiles/zshrc",
            "/Users//someone/Documentsx/zshrc",
            "/Users//someone/src/Documents/zshrc",
            "/Users//someone/Library",
            "/Users//someone/Library/Application Support/aterm/pkg",
            "/Users//someone/Library/Caches/aterm",
            "/Users//someone/Library/Preferences/com.apple.Terminal.plist",
            "/Users//someone/Library/Containersx/a",
            "/Users//someone/Library/Group Containersx/a",
            "/Users//someone/Library/Mobile",
            "/Users//someone/Library/CloudStoragex",
            "/Users//other/Documents/zshrc",
            "/private/tmp/Documents/zshrc",
            "/Volumesx/y",
        ];
        for p in clear {
            assert!(
                !under_protected_root(&home, Path::new(p)),
                "{p} is not under a protected root"
            );
        }
    }

    /// `under_app_data` matches `~/Library/Containers` and
    /// `~/Library/Group Containers` per component, implies `under_protected_root`,
    /// fails closed on `..` below `home`, and leaves sibling `~/Library` paths and
    /// other protected folders alone.
    #[test]
    fn other_apps_data_is_recognised_per_component_and_nothing_else_is() {
        let home = PathBuf::from("/Users//someone");
        let app_data = [
            "/Users//someone/Library/Containers",
            "/Users//someone/Library/Containers/com.apple.Notes",
            "/Users//someone/Library/Containers/com.apple.Notes/Data/Documents/note.txt",
            "/Users//someone/Library/Group Containers",
            "/Users//someone/Library/Group Containers/group.com.apple.notes/NoteStore.sqlite",
            "/Users//someone/Library/Caches/../Containers/com.apple.Notes",
        ];
        for p in app_data {
            assert!(
                under_app_data(&home, Path::new(p)),
                "{p} is under an App Data root"
            );
            assert!(
                under_protected_root(&home, Path::new(p)),
                "{p} is also protected at large"
            );
        }
        let not_app_data = [
            "/Users//someone",
            "/Users//someone/Documents/notes.md",
            "/Users//someone/Library",
            "/Users//someone/Library/Application Support/aterm",
            "/Users//someone/Library/Caches/com.apple.Notes",
            "/Users//someone/Library/Mobile Documents/com~apple~CloudDocs",
            "/Users//someone/Library/CloudStorage/Provider",
            "/Users//someone/Library/Containersx/com.apple.Notes",
            "/Users//someone/Library/Group Containersx/group.x",
            "/Users//someone/src/Library/Containers/com.apple.Notes",
            "/Users//other/Library/Containers/com.apple.Notes",
            "/Volumes/External",
        ];
        for p in not_app_data {
            assert!(
                !under_app_data(&home, Path::new(p)),
                "{p} must not match under_app_data"
            );
        }
    }
}
