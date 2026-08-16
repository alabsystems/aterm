// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! build.rs — derive the updater's DEFAULT release source (owner/repo) from the
//! SINGLE tracked source of truth in the WORKSPACE manifest, emitting it (consumed
//! by `env!` in `src/source.rs`) so "where installed copies look for updates" lives
//! in exactly one place instead of a literal duplicated across crates and scripts.
//!
//! Precedence, highest first:
//!
//! 1. `[workspace.metadata.aterm] update_channel = "OWNER/REPO"` — the PUBLIC
//!    update channel. This is the key that matters: releases are cut privately
//!    and mirrored to it by `cargo ship cut`'s `mirror` step, so a shipped build
//!    can read the channel with no credential at all.
//! 2. `[workspace.package] repository` (inherited here as `CARGO_PKG_REPOSITORY`)
//!    — the source/publish repo, used when no separate channel is declared.
//! 3. the compiled-in `alabsystems/aterm` fallback.
//!
//! Rungs 2+3 are ALSO emitted on their own as `ATERM_PUBLISH_OWNER`/`_REPO`, the
//! account this project is published under, which is independent of where installed
//! copies fetch updates from.
//!
//! A THIRD slug rides along: `[workspace.metadata.atpkg] account`, emitted as
//! `ATERM_ATPKG_INDEX_OWNER` (falling back to the publish owner when absent or
//! malformed) — the account the SIGNED PACKAGE INDEX is published under. atpkg's
//! account-bound trust (§8) reads THIS key: not `update_channel`, because
//! repointing the app channel at a mirror must never move the index's trust
//! root, and not `repository`, because that is the private staging repo a
//! default-configured (tokenless) install cannot read at all.
//!
//! The runtime knobs (`ATERM_UPDATE_OWNER`/`_REPO`, `[update]` config) still
//! override whatever is emitted here.
//!
//! Best-effort by construction: an unreadable manifest, an absent key, or a
//! non-`OWNER/REPO` value degrades to the next rung rather than failing the build.
//! The parse is hand-rolled on purpose — a build-dependency on a TOML crate would
//! add a link to the build-time trust chain of every client binary for two string
//! reads.

use std::path::PathBuf;

fn main() {
    // `[workspace.metadata]` is NOT exposed to build scripts by cargo, so the
    // workspace manifest is read directly. It is two levels up from this crate
    // (`crates/aterm-update-core/`); resolve it from CARGO_MANIFEST_DIR rather
    // than the process cwd, which cargo does not guarantee.
    let workspace_manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default())
        .join("..")
        .join("..")
        .join("Cargo.toml");
    let workspace_text = std::fs::read_to_string(&workspace_manifest).unwrap_or_default();

    let channel = table_string(
        &workspace_text,
        "workspace.metadata.aterm",
        "update_channel",
    )
    .and_then(|slug| {
        split_owner_repo(&slug).map(|(owner, repo)| (owner.to_string(), repo.to_string()))
    });

    let url = std::env::var("CARGO_PKG_REPOSITORY").unwrap_or_default();
    // The SOURCE/publish repo, derived from `repository` alone and NEVER from
    // `update_channel`. Emitted separately because the two slugs answer different
    // questions and only coincidentally matched before the public mirror existed:
    // consumers that mean "the account this project is published under" (the
    // token chain's private-repo target; the absent-key fallback for the atpkg
    // index account below) must not follow the update channel when it is
    // repointed at a mirror.
    let (publish_owner, publish_repo) =
        parse_github_owner_repo(&url).unwrap_or(("alabsystems", "aterm"));
    let (owner, repo) = match channel {
        Some(slug) => slug,
        None => (publish_owner.to_string(), publish_repo.to_string()),
    };
    // The account the SIGNED PACKAGE INDEX lives under, from its own tracked key.
    // Neither slug above fits: `update_channel` is the app-update knob the
    // account-bound index (§8) must not follow, and `repository` is the private
    // staging repo — a tokenless default-configured install 404s there, which
    // orphaned every such install from the published registry. Absent/malformed
    // degrades to the publish owner (the pre-key behavior), per the best-effort
    // contract above.
    let index_owner = table_string(&workspace_text, "workspace.metadata.atpkg", "account")
        .filter(|s| valid_segment(s))
        .unwrap_or_else(|| publish_owner.to_string());
    println!("cargo:rustc-env=ATERM_DEFAULT_OWNER={owner}");
    println!("cargo:rustc-env=ATERM_DEFAULT_REPO={repo}");
    println!("cargo:rustc-env=ATERM_PUBLISH_OWNER={publish_owner}");
    println!("cargo:rustc-env=ATERM_PUBLISH_REPO={publish_repo}");
    println!("cargo:rustc-env=ATERM_ATPKG_INDEX_OWNER={index_owner}");
    // Re-derive if either input changes. `repository` is inherited via
    // `repository.workspace = true` and `update_channel` lives only in the
    // workspace root, so that manifest is the authoritative file; watch this
    // crate's manifest too for belt-and-suspenders.
    println!("cargo:rerun-if-changed=../../Cargo.toml");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-env-changed=CARGO_PKG_REPOSITORY");
}

/// Read one `key = "value"` string out of one `[table]` of a TOML document.
///
/// Deliberately minimal: it recognizes exactly the shape this repository's
/// manifest uses (a line-oriented `[header]`, then `key = "value"` with optional
/// surrounding whitespace and an optional trailing `# comment`). Anything else —
/// an inline table, a multi-line string — is treated as absent, which falls
/// through to the next precedence rung instead of guessing.
fn table_string(toml: &str, table: &str, key: &str) -> Option<String> {
    let header = format!("[{table}]");
    let mut in_table = false;
    for line in toml.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_table = line == header;
            continue;
        }
        if !in_table {
            continue;
        }
        let Some((name, rest)) = line.split_once('=') else {
            continue;
        };
        if name.trim() != key {
            continue;
        }
        // The value is the first double-quoted run; a trailing `# comment` is
        // outside it and therefore ignored for free.
        let rest = rest.trim().strip_prefix('"')?;
        let (value, _) = rest.split_once('"')?;
        return Some(value.to_string());
    }
    None
}

/// Split a bare `OWNER/REPO` slug, rejecting anything that is not exactly two
/// non-empty segments of the GitHub name alphabet. Fail closed: a bad value must
/// fall through to the next precedence rung, never become a path-injecting slug.
/// This mirrors `source::is_valid_slug`, which re-checks the value at runtime.
fn split_owner_repo(slug: &str) -> Option<(&str, &str)> {
    let (owner, repo) = slug.trim().split_once('/')?;
    (valid_segment(owner) && valid_segment(repo)).then_some((owner, repo))
}

/// Whether one bare GitHub name segment (an owner OR a repo — never a full
/// `OWNER/REPO` slug) is safe to embed in the API URL as one path segment.
/// Shared by [`split_owner_repo`] and the `[workspace.metadata.atpkg] account`
/// read; mirrors `source::is_valid_slug`, which re-checks values at runtime.
fn valid_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment.len() <= 100
        && segment != "."
        && segment != ".."
        && segment
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// Extract `(owner, repo)` from a GitHub repository URL, accepting the `https://`
/// and `git@` forms and tolerating a trailing `.git` or `/`. Returns `None` for any
/// other host/shape so the caller can fall back to the compiled-in default.
fn parse_github_owner_repo(url: &str) -> Option<(&str, &str)> {
    let rest = url
        .trim()
        .strip_prefix("https://github.com/")
        .or_else(|| url.trim().strip_prefix("http://github.com/"))
        .or_else(|| url.trim().strip_prefix("git@github.com:"))?;
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let (owner, repo) = rest.split_once('/')?;
    // A well-formed slug is exactly two non-empty segments — reject extra path parts.
    if owner.is_empty() || repo.is_empty() || repo.contains('/') {
        return None;
    }
    Some((owner, repo))
}
