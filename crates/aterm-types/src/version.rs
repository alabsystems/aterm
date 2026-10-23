// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The running binary's [`BuildStamp`].

use std::sync::OnceLock;

/// The running binary's build facts, compiled into the binary crate and installed once ([`install`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BuildStamp {
    /// `[workspace.package] version`, as written.
    pub app_version: &'static str,
    /// Short commit (12 hex, `-dirty` suffixed for an uncommitted tree), or `"unknown"` without git.
    pub git_commit: &'static str,
    /// All forty hexadecimal commit characters, or `"unknown"`.
    pub git_commit_full: &'static str,
    /// `"true"` when the tree had uncommitted changes, else `"false"`.
    pub git_dirty: &'static str,
    /// UTC build timestamp (RFC3339), or `"unknown"`.
    pub build_time: &'static str,
    /// Monotonic build number: the release's ledger claim, else HEAD's committer epoch; `"0"` without git.
    pub build_number: &'static str,
    /// Commits since the newest release tag; `"0"` without git or the tag.
    pub dev_commits: &'static str,
    /// Full first line of the producing compiler's `-vV`.
    pub compiler_version_line: &'static str,
    /// The producing compiler's full commit hash, or `"unknown"`.
    pub compiler_commit: &'static str,
    /// The producing compiler's host triple, or `"unknown"`.
    pub compiler_host: &'static str,
    /// Trust's own version (the `trust:` line of `-vV`); `""` when the compiler reported none.
    pub compiler_trust_version: &'static str,
    /// `"r"` (upstream Rust) or `"t"` (Trust).
    pub compiler_flavor: &'static str,
    /// The cargo profile the binary was compiled under (`"debug"`/`"release"`).
    pub build_profile: &'static str,
    /// `"on"` iff the Trust verification pipeline was in the binary's compile.
    pub trust_verify: &'static str,
    /// Whether a release lane produced the binary (`ATERM_RELEASE_BUILD`).
    pub release_build: bool,
}

impl BuildStamp {
    /// Default stamp read before (or without) [`install`].
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

/// Install the running binary's [`BuildStamp`] (first call wins).
pub fn install(stamp: BuildStamp) {
    let _ = STAMP.set(stamp);
}

/// The installed [`BuildStamp`], or `None` in a process that installed none.
#[must_use]
pub fn installed() -> Option<&'static BuildStamp> {
    STAMP.get()
}

/// The installed stamp's `app_version`, else [`BuildStamp::UNSTAMPED`]'s.
#[must_use]
pub fn app_version() -> &'static str {
    installed().unwrap_or(&BuildStamp::UNSTAMPED).app_version
}

/// The running binary's `<version>+g<commit>` token.
#[must_use]
pub fn build_id() -> String {
    let stamp = installed().unwrap_or(&BuildStamp::UNSTAMPED);
    format!("{}+g{}", stamp.app_version, stamp.git_commit)
}

#[cfg(test)]
mod tests {
    use super::*;

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
