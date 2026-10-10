// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The running binary's build facts: version, build number, commit, compiler,
//! profile, verification state and build time.
//!
//! There is exactly ONE version: `[workspace.package] version` in Cargo.toml,
//! always `MAJOR.MINOR.0` (VERSIONING.md). Every build reports it as written —
//! a release, a development build and a public-source build alike — and it is
//! also the release's `vMAJOR.MINOR.0` tag and its `aterm-<version>.dmg` asset
//! name. A release differs from a development build by its ledger build number
//! and the cutter's release marker ([`BuildStamp::release_build`]), never by a
//! second version.
//!
//! THE BINARY OWNS EVERY BUILD FACT. `crates/aterm` compiles them in (its
//! `main.rs` and `build.rs`) and [`install`]s them as one [`BuildStamp`] first
//! thing in `main`; every library reads them at runtime through [`installed`] or
//! [`app_version`]. No library compiles a version, commit, compiler or profile
//! in: a commit recompiles the binary crate alone, and no library's compile
//! records any of them as an input. A process whose `main` installs nothing (a
//! dev-only `[[bin]]`, an example, another crate's tests) reads
//! [`BuildStamp::UNSTAMPED`].

use std::sync::OnceLock;

/// The running binary's build facts, compiled into the binary crate and
/// installed once ([`install`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BuildStamp {
    /// `[workspace.package] version`, as written.
    pub app_version: &'static str,
    /// Short commit (12 hex, `-dirty` suffixed for an uncommitted tree), or
    /// `"unknown"` without git.
    pub git_commit: &'static str,
    /// All forty hexadecimal commit characters, or `"unknown"`.
    pub git_commit_full: &'static str,
    /// `"true"` when the tree had uncommitted changes, else `"false"`.
    pub git_dirty: &'static str,
    /// UTC build timestamp (RFC3339), or `"unknown"`.
    pub build_time: &'static str,
    /// Monotonic build number: the release's ledger claim, else HEAD's committer
    /// epoch; `"0"` without git.
    pub build_number: &'static str,
    /// Commits since the newest release tag; `"0"` without git or the tag.
    pub dev_commits: &'static str,
    /// Full first line of the producing compiler's `-vV`.
    pub compiler_version_line: &'static str,
    /// The producing compiler's full commit hash, or `"unknown"`.
    pub compiler_commit: &'static str,
    /// The producing compiler's host triple, or `"unknown"`.
    pub compiler_host: &'static str,
    /// Trust's own version (the `trust:` line of `-vV`); `""` when the compiler
    /// reported none.
    pub compiler_trust_version: &'static str,
    /// `"r"` (upstream Rust) or `"t"` (Trust).
    pub compiler_flavor: &'static str,
    /// The cargo profile the binary was compiled under (`"debug"`/`"release"`).
    pub build_profile: &'static str,
    /// `"on"` iff the Trust verification pipeline was in the binary's compile.
    pub trust_verify: &'static str,
    /// Whether a release lane produced the binary (the cutter's
    /// `ATERM_RELEASE_BUILD` marker was set for its compile).
    pub release_build: bool,
}

impl BuildStamp {
    /// What a process reads before (or without) [`install`]: the facts a build
    /// without git always carried (`unknown`, build `0`), version `0.0.0`.
    pub const UNSTAMPED: Self = Self {
        app_version: "0.0.0",
        git_commit: "unknown",
        git_commit_full: "unknown",
        git_dirty: "false",
        build_time: "unknown",
        build_number: "0",
        dev_commits: "0",
        compiler_version_line: "unknown",
        compiler_commit: "unknown",
        compiler_host: "unknown",
        compiler_trust_version: "",
        compiler_flavor: "r",
        build_profile: "unknown",
        trust_verify: "off",
        release_build: false,
    };
}

static STAMP: OnceLock<BuildStamp> = OnceLock::new();

/// Install the running binary's [`BuildStamp`]. The binary's `main` calls this
/// first, before anything reads a build fact; the first install wins, so nothing
/// later in the process can re-stamp it.
pub fn install(stamp: BuildStamp) {
    let _ = STAMP.set(stamp);
}

/// The installed [`BuildStamp`], or `None` in a process that installed none.
#[must_use]
pub fn installed() -> Option<&'static BuildStamp> {
    STAMP.get()
}

/// The version shown by the application, command line, diagnostics and terminal
/// protocol identity: the installed stamp's, else [`BuildStamp::UNSTAMPED`]'s.
#[must_use]
pub fn app_version() -> &'static str {
    installed().unwrap_or(&BuildStamp::UNSTAMPED).app_version
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Before an install every read is UNSTAMPED; the first install is what
    /// every later read sees, and a second install changes nothing.
    #[test]
    fn install_first_wins() {
        assert_eq!(installed(), None);
        assert_eq!(app_version(), BuildStamp::UNSTAMPED.app_version);
        let first = BuildStamp {
            app_version: "9.8.0",
            build_number: "1",
            ..BuildStamp::UNSTAMPED
        };
        install(first);
        install(BuildStamp {
            app_version: "9.9.0",
            ..first
        });
        assert_eq!(installed(), Some(&first));
        assert_eq!(app_version(), "9.8.0");
    }
}
