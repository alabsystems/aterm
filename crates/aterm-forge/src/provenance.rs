// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Who wrote this code: the provenance classifier.
//!
//! Path packages in this workspace are one of three shapes:
//!
//!   * `crates/<name>`: code aterm owns. First-party, so it is billed to the
//!     workspace, never to the third-party surface. Includes the first-party
//!     `[patch.crates-io]` replacements, which are members patched in by
//!     package name, and the ABSORBED crates (`crates/aterm-window`, winit
//!     0.30.13), upstream source aterm took over as members: those still owe
//!     attest's license obligations (`[OB-5]`/`[OB-6]`/`[OB-7]`/`[OB-9]`).
//!   * `vendor/<name>`: upstream source aterm forked and keeps reviewing
//!     (indexmap). Third-party; owes every obligation, and `[OB-1]` holds the
//!     patch table and the directory listing in agreement.
//!   * a first-party path dependency vendored in-tree: `vendor/astream`, the
//!     same owner's astream repository as a git submodule
//!     (`vendor/README-astream.md`), path-depended on by `crates/aterm-link`.
//!     It is in `vendor/` so a clean clone builds; it has no crates.io package,
//!     so no patch entry can name it.
//!
//! # The rule is a roster
//!
//! A `vendor/<dir>` on [`FIRST_PARTY_VENDORED`] is aterm's own code; everything
//! else under `vendor/` is a third-party fork. The roster is fail-closed both
//! ways: a row whose directory is gone is a stale-review error, and a directory
//! on neither the roster nor the patch table fails `[OB-1]`.
//!
//! The claim lives in aterm's own source rather than in a file or manifest key
//! inside the vendored tree, which the next submodule bump could rewrite. It
//! mirrors `aterm_census::scan_set::REVIEWED_VENDORED_CRATES`, and the two
//! must never name the same directory ([`crate::attest`] checks that).
//!
//! # What this does not relax
//!
//! Being first-party exempts only the redistribution obligations. A roster
//! directory must still be reached by some member's `path = …`
//! ([`path_dependants`]), and the packages it pulls in stay third-party and on
//! the ratchet.

use std::path::{Component, Path, PathBuf};

/// One directory under `vendor/` that holds aterm's OWN code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FirstPartyVendored {
    /// Repo-relative directory, always `vendor/<dir>`, no trailing slash.
    pub dir: &'static str,
    /// The upstream repository this tree comes from. Names the repository,
    /// never a commit; any pin is read from the tree (`[OB-1]` prints a
    /// submodule's gitlink).
    pub upstream: &'static str,
    /// Why it is aterm's own code rather than a redistribution, and why it is
    /// under `vendor/` at all. Printed by `[OB-1]` on every run.
    pub why: &'static str,
}

/// THE ROSTER. Reviewed by a human, one row per directory.
pub const FIRST_PARTY_VENDORED: &[FirstPartyVendored] = &[FirstPartyVendored {
    dir: "vendor/astream",
    upstream: "github.com/alabsystems/astream, a git submodule pinned by this tree's gitlink",
    why: "Same owner as github.com/alabsystems/aterm — the same account, not a similar \
          name — so this is aterm's own code, not a redistribution. It is under vendor/ for a \
          BUILD reason: crates/aterm-link path-depended on a sibling checkout most clones do \
          not have, so cargo build never built the fabric bridge and no release ever shipped \
          it. It is a git submodule (vendor/README-astream.md), and aterm resolves five of its \
          crates (wire, digest, cap, broker, aead) by path, outside this workspace; aead only \
          under aterm-link's off-by-default sealed feature. Not on crates.io, so no \
          [patch.crates-io] entry could ever name it: a patch entry replaces a registry \
          package and there is none to replace. The third-party packages it DRAGS IN (none in \
          a default build since astream-cap's SHA-256 became astream-digest, 2026-10-15; \
          chacha20poly1305 and getrandom under aterm-link's sealed feature) are unaffected \
          by this row and stay on the ratchet.",
}];

/// The roster row whose directory contains `rel`, if any. `rel` is
/// repo-relative and may be the root itself or anything beneath it.
#[must_use]
pub fn roster_row(rel: &str) -> Option<&'static FirstPartyVendored> {
    FIRST_PARTY_VENDORED
        .iter()
        .find(|r| rel == r.dir || rel.starts_with(&format!("{}/", r.dir)))
}

/// Is this absolute package directory inside a first-party vendored root?
#[must_use]
pub fn is_first_party_vendored(root: &Path, dir: &Path) -> bool {
    FIRST_PARTY_VENDORED
        .iter()
        .any(|r| dir.starts_with(root.join(r.dir)))
}

/// Is this absolute package directory aterm's own code: a workspace member
/// under `crates/`, or a first-party vendored root? `loc::measure` inverts
/// this to decide `PkgFacts::is_third_party`.
#[must_use]
pub fn is_first_party(root: &Path, dir: &Path) -> bool {
    dir.starts_with(root.join("crates")) || is_first_party_vendored(root, dir)
}

/// Does the roster describe this root?
///
/// [`FIRST_PARTY_VENDORED`] is a statement about the aterm workspace, like
/// `aterm_census::scan_set::REVIEWED_VENDORED_CRATES`; applied to another root
/// (attest's temp-dir fixtures) it would invent findings. Probed on the file
/// the constant lives in.
#[must_use]
pub fn applies_to(root: &Path) -> bool {
    root.join("crates/aterm-forge/src/provenance.rs").is_file()
}

/// How many roster rows apply to this root (zero where the roster does not
/// describe it), so a log line cannot claim an exemption not in force.
#[must_use]
pub fn roster_len(root: &Path) -> usize {
    if applies_to(root) {
        FIRST_PARTY_VENDORED.len()
    } else {
        0
    }
}

/// Roster rows whose directory does not exist: a hard error, since a stale row
/// would exempt an invisible directory. Callers gate this on [`applies_to`].
#[must_use]
pub fn stale_rows(root: &Path) -> Vec<&'static FirstPartyVendored> {
    FIRST_PARTY_VENDORED
        .iter()
        .filter(|r| !root.join(r.dir).is_dir())
        .collect()
}

/// Workspace members under `crates/` whose manifest carries a `path = …`
/// dependency resolving inside `rel_root`, sorted and deduplicated.
///
/// The liveness half of `[OB-1]` for a roster directory, which has no patch
/// entry to prove it is reached. Empty means dead weight.
#[must_use]
pub fn path_dependants(root: &Path, rel_root: &str) -> Vec<String> {
    let target = root.join(rel_root);
    let mut out: Vec<String> = Vec::new();
    let Ok(entries) = std::fs::read_dir(root.join("crates")) else {
        return out;
    };
    for entry in entries.flatten() {
        let member = entry.path();
        let manifest = member.join("Cargo.toml");
        let Ok(text) = std::fs::read_to_string(&manifest) else {
            continue;
        };
        if !path_deps(&text)
            .iter()
            .any(|p| normalize(&member.join(p)).starts_with(&target))
        {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        out.push(name);
    }
    out.sort();
    out.dedup();
    out
}

/// Every `path = "…"` value in a manifest: a key scan rather than a TOML
/// parse, since only relative directories are wanted.
fn path_deps(manifest: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (i, _) in manifest.match_indices("path") {
        let rest = &manifest[i + "path".len()..];
        let rest = rest.trim_start();
        let Some(rest) = rest.strip_prefix('=') else {
            continue;
        };
        let rest = rest.trim_start();
        let Some(rest) = rest.strip_prefix('"') else {
            continue;
        };
        let Some(end) = rest.find('"') else { continue };
        out.push(rest[..end].to_string());
    }
    out
}

/// Lexically resolve `.` and `..`. Not `canonicalize`, so classification is
/// the same whether or not the checkout sits behind a symlink.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::repo_root;

    /// The roster's shape is load-bearing: `roster_row` matches on a `/`
    /// boundary, so a row must not carry one at either end.
    #[test]
    fn the_roster_is_well_formed() {
        for row in FIRST_PARTY_VENDORED {
            assert!(
                row.dir.starts_with("vendor/") && !row.dir.ends_with('/'),
                "`{}` must be `vendor/<dir>` with no trailing slash",
                row.dir
            );
            assert!(
                row.dir.matches('/').count() == 1,
                "`{}`: the roster names a TOP-LEVEL vendor directory, because that is what \
                 [OB-1]'s directory sweep compares against",
                row.dir
            );
            assert!(!row.upstream.is_empty() && row.why.len() > 80);
        }
    }

    /// `vendor/` holds both answers, and a sibling prefix is not swallowed.
    #[test]
    fn the_roster_separates_astream_from_the_real_forks() {
        let root = repo_root();
        assert!(is_first_party_vendored(
            &root,
            &root.join("vendor/astream/crates/astream-broker")
        ));
        assert!(is_first_party(
            &root,
            &root.join("vendor/astream/crates/astream-cap")
        ));
        assert!(
            !is_first_party(&root, &root.join("vendor/indexmap")),
            "`vendor/indexmap` is a REDISTRIBUTED fork and must stay third-party"
        );
        assert!(is_first_party(&root, &root.join("crates/aterm-core")));
        assert!(
            is_first_party(&root, &root.join("crates/aterm-window")),
            "the absorbed winit is a workspace member, billed as aterm's own lines"
        );
        assert_eq!(
            roster_row("vendor/astream-broker"),
            None,
            "prefix, not name"
        );
        assert!(roster_row("vendor/astream").is_some());
        assert!(roster_row("vendor/astream/crates/astream-wire").is_some());
    }

    /// The liveness tooth, on the real tree: crates/aterm-link is why this
    /// directory is here at all, so it must be the answer.
    #[test]
    fn the_roster_directory_is_reached_by_a_workspace_member() {
        let dependants = path_dependants(&repo_root(), "vendor/astream");
        assert!(
            dependants.contains(&"aterm-link".to_string()),
            "expected aterm-link among {dependants:?}"
        );
    }

    /// The roster is scoped: it applies to this workspace and not to a
    /// synthetic fixture.
    #[test]
    fn the_roster_applies_to_this_workspace_and_not_to_an_arbitrary_root() {
        assert!(applies_to(&repo_root()));
        assert!(!applies_to(Path::new("/")));
        assert!(!applies_to(&std::env::temp_dir()));
    }

    /// A directory nothing points at must read EMPTY, or the tooth is blunt.
    #[test]
    fn a_directory_no_member_path_depends_on_has_no_dependants() {
        assert!(path_dependants(&repo_root(), "vendor/does-not-exist").is_empty());
    }

    #[test]
    fn path_values_are_read_out_of_a_manifest_and_relative_parts_resolve() {
        let m = "[dependencies]\nastream-broker = { path = \"../../vendor/astream/crates/astream-broker\", features = [\"cap\"] }\nother.workspace = true\n";
        assert_eq!(
            path_deps(m),
            vec!["../../vendor/astream/crates/astream-broker".to_string()]
        );
        assert_eq!(
            normalize(Path::new(
                "/w/crates/aterm-link/../../vendor/astream/crates/x"
            )),
            PathBuf::from("/w/vendor/astream/crates/x")
        );
        assert_eq!(
            path_deps("[dependencies]\nx = \"1\"\n"),
            Vec::<String>::new()
        );
    }
}
