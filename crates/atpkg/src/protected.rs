// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The folders macOS guards with a per-folder consent dialog, named ONCE for atpkg's
//! UNATTENDED lanes — the six-hourly update pass, the first-launch seed, the doctor's
//! ambient walk — so none of them ever opens one by accident.
//!
//! # Why an unattended lane must never open one of these
//!
//! macOS attributes the dialog to the RESPONSIBLE app, and for everything atpkg does
//! from inside aterm that is aterm. A pass that opens a file under `~/Documents` at
//! 3 am raises `“aterm” would like to access files in your Documents folder.` in front
//! of nobody, parks the pass on a modal that has no timeout
//! (`docs/DESIGN-macos-tcc-prompts-2026-08-30.md` §1.4), and — because the dialog
//! arrived with no visible cause — reads to the owner as a bug in aterm. The consent
//! design's whole point is to move that interruption to a moment a human chose (§3.5);
//! an unattended lane that touches these roots undoes it in aterm's own name.
//!
//! Two lanes have now made this mistake: the home walk of [`crate::noindex`] (2026-09-10,
//! pruned) and the rc wiring of [`crate::hooks`], which followed a symlinked `~/.zshrc`
//! (or a symlinked `~/.config`) into wherever the dotfiles repo lived (2026-09-12). This
//! module exists so the THIRD lane finds the predicate instead of rediscovering the
//! incident.
//!
//! # What is and is not gated
//!
//! Per the design's §1.5 measurements, `stat`/`lstat`/`realpath` on a protected path
//! are NOT gated — only `open()` of file data, directory enumeration, `access(R|W_OK)`,
//! extended-attribute reads and writes/creates are. So a caller may `canonicalize` a
//! path and hand the result to [`under_protected_root`] BEFORE its first open; the
//! predicate itself is purely lexical and touches nothing.
//!
//! # Which roots
//!
//! * the six per-folder services under `$HOME` ([`HOME_FOLDERS`]) — `Documents`,
//!   `Desktop`, `Downloads`, `Pictures`, `Movies`, `Music` — the same names
//!   [`crate::noindex`]'s walk prunes and `aterm_containment::sbpl::PRIVATE_SUBDIRS`
//!   denies (atpkg does not depend on that crate; `noindex`'s test pins the two lists
//!   to each other, and this module's pins `noindex`'s);
//! * the two file-provider domains under `~/Library` ([`LIBRARY_DOMAINS`]) — iCloud
//!   Drive (`Mobile Documents`) and every third-party sync provider (`CloudStorage`),
//!   which prompt as `NSFileProviderDomainUsageDescription` and which the design records
//!   as EPERM even under a held Full Disk Access grant (§3.4);
//! * `/Volumes` ([`ABSOLUTE_ROOTS`]) — network and removable volumes, each its own
//!   service.
//!
//! `~/Library` as a whole is deliberately NOT here: atpkg's own store lives under
//! `~/Library/Application Support`, and the App Data service is per-container, not a
//! root this predicate can name.

use std::path::{Component, Path};

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
///
/// Purely lexical: no symlink is followed and nothing is stat'ed, so the caller decides
/// how resolved `path` is. Hand it a `canonicalize`d path (not gated, §1.5) and the answer
/// is about the file that would actually be opened; hand it the raw spelling and the
/// answer is about that spelling only. `home` is compared as given — a caller whose home
/// may itself be reached through a link canonicalizes both sides.
///
/// `$HOME/Documentsx` is not `$HOME/Documents`: the comparison is per component, never
/// a string prefix.
#[must_use]
pub fn under_protected_root(home: &Path, path: &Path) -> bool {
    if ABSOLUTE_ROOTS
        .iter()
        .any(|root| path.starts_with(Path::new(root)))
    {
        return true;
    }
    let Ok(rest) = path.strip_prefix(home) else {
        return false;
    };
    let mut components = rest.components().filter_map(|component| match component {
        Component::Normal(name) => Some(name),
        _ => None,
    });
    let Some(first) = components.next() else {
        return false; // `$HOME` itself
    };
    if HOME_FOLDERS.iter().any(|folder| first == *folder) {
        return true;
    }
    let Some(second) = components.next() else {
        return false;
    };
    LIBRARY_DOMAINS
        .iter()
        .any(|(lib, domain)| first == *lib && second == *domain)
}

/// Whether `path` lies at or under a FILE PROVIDER domain specifically — iCloud
/// Drive (`Library/Mobile Documents`) or a third-party sync provider
/// (`Library/CloudStorage`).
///
/// Narrower than [`under_protected_root`] on purpose. The `$HOME` folders and
/// `/Volumes` are classes a held Full Disk Access grant reaches, so refusing
/// them would block ordinary work the owner has already consented to once. The
/// file-provider domains are the one class the design records as **not**
/// reliably covered by that grant (`docs/DESIGN-macos-tcc-prompts-2026-08-30.md`
/// §3.4, and `NEVER_COVERED` in the `privacy` verb): an access there can raise
/// `"aterm" wants to access files managed by "iCloud Drive"` no matter what the
/// owner has granted.
///
/// That matters wherever a path can arrive from INSIDE a session, because a
/// consent dialog a program can raise in aterm's name is a consent surface an
/// agent controls — the same rule that fences the warm-up and `tccutil reset`
/// to the Security panel.
///
/// Purely lexical, per component, like its sibling. Shares [`LIBRARY_DOMAINS`],
/// so the two predicates cannot drift.
#[must_use]
pub fn under_file_provider_domain(home: &Path, path: &Path) -> bool {
    let Ok(rest) = path.strip_prefix(home) else {
        return false;
    };
    let mut components = rest.components().filter_map(|component| match component {
        Component::Normal(name) => Some(name),
        _ => None,
    });
    let (Some(first), Some(second)) = (components.next(), components.next()) else {
        return false;
    };
    LIBRARY_DOMAINS
        .iter()
        .any(|(lib, domain)| first == *lib && second == *domain)
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// The FILE PROVIDER predicate is strictly narrower than its sibling: it
    /// answers `true` for the two sync domains and `false` for every other
    /// protected root, because those are reached by a held Full Disk Access
    /// grant and these are not.
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
        // Protected, but NOT file-provider: a held grant reaches these, so the
        // narrow predicate must not claim them or it would refuse ordinary work.
        let protected_elsewhere = [
            "/Users//someone/Documents/notes.md",
            "/Users//someone/Desktop/notes.md",
            "/Users//someone/Downloads/a/b",
            "/Users//someone/Pictures/x",
            "/Volumes/External/notes.md",
        ];
        for p in protected_elsewhere {
            assert!(
                !under_file_provider_domain(&home, Path::new(p)),
                "{p} is protected but is not a file-provider domain"
            );
        }
        // Per component, never a string prefix, and `~/Library` alone is neither.
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

    /// The predicate, as a table: every root the design names answers `true`, and the
    /// shapes that merely resemble one answer `false`.
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
            "/Users//someone/Library/Application Support/aterm/pkg",
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

    /// A file-provider domain is protected from its ROOT down. The providers inside
    /// `~/Library/CloudStorage` are the domains that prompt, but an unattended lane has
    /// no business at that root either, and refusing more is the harmless direction —
    /// nothing atpkg lays ever resolves there on purpose.
    #[test]
    fn a_file_provider_domain_is_protected_from_its_root_down() {
        let home = PathBuf::from("/Users//someone");
        for p in [
            "/Users//someone/Library/CloudStorage",
            "/Users//someone/Library/CloudStorage/OneDrive",
            "/Users//someone/Library/Mobile Documents",
            "/Users//someone/Library/Mobile Documents/com~apple~CloudDocs",
        ] {
            assert!(under_protected_root(&home, Path::new(p)), "{p}");
        }
        // `~/Library` itself is not a root this predicate names: atpkg's own store
        // lives under `~/Library/Application Support`.
        assert!(!under_protected_root(
            &home,
            Path::new("/Users//someone/Library")
        ));
    }

    /// The six names are the SAME six the home walk prunes. Two lists that could drift
    /// apart would be two definitions of "protected"; this pins them.
    #[test]
    fn the_home_folders_match_the_noindex_walks_prune_list() {
        let source = include_str!("noindex.rs");
        let start = source
            .find("const SKIP_DIRS: &[&str] = &[")
            .expect("noindex.rs names its prune list");
        let block = &source[start..];
        let end = block.find("];").expect("the prune list closes");
        let block = &block[..end];
        for folder in HOME_FOLDERS {
            assert!(
                block.contains(&format!("\"{folder}\"")),
                "noindex::SKIP_DIRS must prune {folder:?} — the two lists have drifted"
            );
        }
    }
}
