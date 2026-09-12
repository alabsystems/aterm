// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHO WROTE THIS CODE — the provenance classifier, and the THIRD SHAPE
//! `vendor/` grew on 2026-09-10.
//!
//! # The two shapes forge knew, and the one it did not
//!
//! Until v0.81.0 every path package in this workspace was one of two things,
//! and a DIRECTORY PREFIX told them apart:
//!
//!   * `crates/<name>` — code aterm WROTE. First-party. Not a redistribution,
//!     so it owes none of attest's `[OB-3]`..`[OB-10]` (the `[workspace]`
//!     stub, `.cargo_vcs_info.json`, a retained upstream LICENSE, a NOTICE
//!     row, the Apache §4(b) pristine diff, the marker census, the SPDX
//!     allowlist, the ignore sweep). Includes the EIGHT first-party
//!     `[patch.crates-io]` replacements, which are workspace members that
//!     happen to be patched in by package name.
//!   * `vendor/<name>` — upstream source aterm FORKED and must keep reviewing
//!     forever: winit, indexmap, libm, pkg-config, smol_str. Third-party. Owes
//!     every one of those obligations, and `[OB-1]` holds the patch table and
//!     the directory listing to agreeing in both directions.
//!
//! v0.81.0 added a third: **a first-party path dependency, vendored in-tree.**
//! `vendor/astream` is a copy of `github.com/alabsystems/astream`, whose
//! owner is the owner of `github.com/alabsystems/aterm` — the same account,
//! not a similar name — and `crates/aterm-link` path-depends on three of its
//! crates directly. It is in `vendor/` for a build reason, not a provenance
//! one: `crates/aterm-link` used to path-depend on a SIBLING CHECKOUT most
//! clones do not have, so `cargo build` never built the fabric bridge and no
//! release ever carried it.
//!
//! The prefix rule called that third-party, and everything downstream believed
//! it: 11,122 lines of aterm's own code entered `third_party_loc`, three
//! packages entered `third_party_packages`, `[OB-1]` demanded a
//! `[patch.crates-io]` entry that CANNOT EXIST (these crates are not on
//! crates.io, and a patch entry replaces a registry package — there is no
//! registry package to replace), and `blame` reported
//! `crates.io registry (not forked, not owned)` for a directory aterm owns.
//!
//! # The rule, and why it is a ROSTER
//!
//! A `vendor/<dir>` on [`FIRST_PARTY_VENDORED`] is aterm's own code. Everything
//! else under `vendor/` is a third-party fork and keeps every obligation it had.
//! The roster is fail-closed BOTH ways: a row whose directory is gone is a
//! stale-review error, and a directory that is on neither the roster nor the
//! patch table is still `[OB-1]`'s "dead weight" failure.
//!
//! THE ROSTER IS NOT THE ONLY PLACE THE CLAIM COULD LIVE, and the two
//! alternatives were rejected for the same reason:
//!
//!   * `vendor/astream/README.md`'s `Upstream:` line states the fact in prose,
//!     but it lives INSIDE the synced tree. The README's own re-sync recipe is
//!     `rsync -a --exclude target ../astream/crates/$c/ vendor/astream/crates/$c/`,
//!     and a rule that reads a file the next sync can overwrite is a rule the
//!     next sync can silently rewrite. A fork's README could also simply claim
//!     it, which is the wrong direction for a fail-closed check to be wrong in.
//!   * A manifest key (`[package.metadata.aterm] provenance = …`) is in the
//!     same blast radius, four times over: `vendor/astream/README.md` already
//!     lists THREE manifest edits this tree carries that must be re-applied by
//!     hand after every sync, and this would be a fourth, per crate.
//!
//! The roster is in aterm's OWN source, outside the synced tree, is one row
//! rather than four, and puts the provenance claim where a human reviews it —
//! which is exactly the discipline `aterm_census::scan_set::
//! REVIEWED_VENDORED_CRATES` already establishes for the third-party
//! direction. This is that constant's mirror image, and the two must never
//! name the same directory ([`crate::attest`] checks that).
//!
//! # What this does NOT relax
//!
//! Being first-party buys exemption from the REDISTRIBUTION obligations and
//! nothing else. A roster directory must still be REACHED — the "dead weight
//! that ships in the source distribution" tooth `[OB-1]` exists for — and
//! [`path_dependants`] is how that is measured: some workspace member's
//! manifest must carry a `path = …` that resolves inside it. Nor does it make
//! the code free: the packages a first-party vendored crate DRAGS IN stay
//! third-party and stay on the ratchet, which is the whole story of `sha2`
//! entering the shipped graph behind `astream-cap`'s capability mint.

use std::path::{Component, Path, PathBuf};

/// One directory under `vendor/` that holds aterm's OWN code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FirstPartyVendored {
    /// Repo-relative directory, always `vendor/<dir>`, no trailing slash.
    pub dir: &'static str,
    /// The upstream this tree is a copy of, so the claim can be checked.
    pub upstream: &'static str,
    /// Why it is aterm's own code rather than a redistribution, and why it is
    /// under `vendor/` at all. Printed by `[OB-1]` on every run.
    pub why: &'static str,
}

/// THE ROSTER. Reviewed by a human, one row per directory.
pub const FIRST_PARTY_VENDORED: &[FirstPartyVendored] = &[FirstPartyVendored {
    dir: "vendor/astream",
    upstream: "github.com/alabsystems/astream (branch main, commit bb98d61)",
    why: "Same owner as github.com/alabsystems/aterm — the same account, not a similar \
          name — so this is aterm's own code, not a redistribution. It is under vendor/ for a \
          BUILD reason: crates/aterm-link path-depended on a sibling checkout most clones do \
          not have, so cargo build never built the fabric bridge and no release ever shipped \
          it. Four crates are here (wire, cap, broker, aead); the sealed transport's aead is \
          vendored but off by default. Not on crates.io, so no [patch.crates-io] entry could \
          ever name it: a patch entry replaces a registry package and there is none to \
          replace. The third-party packages it DRAGS IN (the sha2 chain under astream-cap) \
          are unaffected by this row and stay on the ratchet.",
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

/// Is this absolute package directory aterm's OWN code — a workspace member
/// under `crates/`, or a first-party vendored root?
///
/// This is the one predicate `loc::measure` inverts to decide
/// `PkgFacts::is_third_party`, so it is the definition of the number this
/// whole crate exists to shrink.
#[must_use]
pub fn is_first_party(root: &Path, dir: &Path) -> bool {
    dir.starts_with(root.join("crates")) || is_first_party_vendored(root, dir)
}

/// Does the roster DESCRIBE this root?
///
/// [`FIRST_PARTY_VENDORED`] is compiled in and it is a statement about the
/// aterm workspace, exactly as `aterm_census::scan_set::
/// REVIEWED_VENDORED_CRATES` is. Applying either to an arbitrary root invents
/// findings: attest's own fixtures build miniature workspaces in a temp
/// directory, and an unscoped staleness sweep reported `vendor/astream` MISSING
/// in every one of them — a failure about a repository the fixture is not.
/// Probed on the file the constant lives in, so the scoping cannot drift from
/// the thing it scopes.
#[must_use]
pub fn applies_to(root: &Path) -> bool {
    root.join("crates/aterm-forge/src/provenance.rs").is_file()
}

/// How many roster rows apply to this root — zero where the roster is not
/// about it. Reported rather than assumed, so a log line cannot claim a
/// first-party exemption that was never in force.
#[must_use]
pub fn roster_len(root: &Path) -> usize {
    if applies_to(root) {
        FIRST_PARTY_VENDORED.len()
    } else {
        0
    }
}

/// Roster rows whose directory does not exist. A stale row is a hard error,
/// not a shrug: it would exempt a directory nobody can see from every
/// redistribution obligation. Callers must gate this on [`applies_to`].
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
/// This is the LIVENESS half of `[OB-1]` for a roster directory: a patch entry
/// proves a fork is reached, and a roster row has no patch entry to prove it
/// with, so the edge itself is measured instead. An empty answer means the
/// directory really is the dead weight `[OB-1]` refuses.
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

/// Every `path = "…"` value in a manifest. Deliberately a scan for the key
/// rather than a TOML parse: the values wanted here are relative directories,
/// and a dependency table shape this misses is one that names no path at all.
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

/// Lexically resolve `.` and `..`. Not `canonicalize`: the target of a
/// dependency edge is a claim about paths, and it must classify the same on a
/// machine whose checkout is behind a symlink as on one whose is not.
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

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("crates/aterm-forge sits two levels under the workspace root")
            .to_path_buf()
    }

    /// A roster row that names a directory nobody has is an exemption granted
    /// to nothing, and it would go on being granted forever.
    #[test]
    fn every_roster_row_names_a_directory_that_exists() {
        let stale = stale_rows(&repo_root());
        assert!(
            stale.is_empty(),
            "stale FIRST_PARTY_VENDORED row(s): {:?}",
            stale.iter().map(|r| r.dir).collect::<Vec<_>>()
        );
    }

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

    /// THE DISCRIMINATION, which is the whole point: `vendor/` is no longer one
    /// answer. A sibling prefix must not be swallowed either.
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
        for fork in ["winit", "indexmap", "libm", "pkg-config", "smol_str"] {
            assert!(
                !is_first_party(&root, &root.join("vendor").join(fork)),
                "`vendor/{fork}` is a REDISTRIBUTED fork and must stay third-party"
            );
        }
        assert!(is_first_party(&root, &root.join("crates/aterm-core")));
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

    /// The roster is scoped, and the probe must answer both ways — an
    /// unscoped roster reported a missing `vendor/astream` inside attest's
    /// synthetic fixtures, which are workspaces this constant says nothing
    /// about.
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
