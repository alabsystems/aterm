// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! build.rs — derive the updater's DEFAULT release source (owner/repo) from the
//! SINGLE source of truth: the `[workspace.package] repository` URL, inherited into
//! this crate's `CARGO_PKG_REPOSITORY` via `repository.workspace = true`. Emitting it
//! here (consumed by `env!` in `src/source.rs`) keeps "where aterm lives" in exactly
//! one place — the manifest — instead of a literal duplicated across the crate and the
//! release scripts. The runtime env/config knobs (`ATERM_UPDATE_OWNER`/`_REPO`,
//! `[update]` config) still override this compiled-in default.
//!
//! Best-effort: an absent or non-`github.com/<owner>/<repo>` URL degrades to
//! `alabsystems/aterm` rather than failing the build.

fn main() {
    let url = std::env::var("CARGO_PKG_REPOSITORY").unwrap_or_default();
    let (owner, repo) = parse_github_owner_repo(&url).unwrap_or(("alabsystems", "aterm"));
    println!("cargo:rustc-env=ATERM_DEFAULT_OWNER={owner}");
    println!("cargo:rustc-env=ATERM_DEFAULT_REPO={repo}");
    // Re-derive if the `repository` field changes. It is inherited via
    // `repository.workspace = true`, so the authoritative file is the WORKSPACE-root
    // manifest (two levels up); watch this crate's manifest too for belt-and-suspenders.
    println!("cargo:rerun-if-changed=../../Cargo.toml");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-env-changed=CARGO_PKG_REPOSITORY");
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
