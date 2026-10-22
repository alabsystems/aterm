// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The GitHub release source (`github.com/<owner>/<repo>`): the compiled default, which a
//! development build may repoint through `[update]` owner/repo, with a URL-safety allowlist
//! so a configured value cannot redirect the updater to another host or path. The source
//! decides where bytes come from, never whether they are trusted.

/// Default GitHub owner of the release repository: the channel of every shipped binary
/// (`[update] owner` repoints it in a development build). aterm-release's channel tests hold
/// `[workspace.metadata.aterm] update_channel` equal to `DEFAULT_OWNER/DEFAULT_REPO`. It is
/// the public mirror, readable with no credential, so a fresh install updates before anyone
/// provisions it a token.
/// Defined from `aterm_types::channel::PUBLIC_OWNER`, which agh's release floor reads; it
/// stays its own constant.
pub const DEFAULT_OWNER: &str = aterm_types::channel::PUBLIC_OWNER;

/// Default GitHub repository name the updater pulls releases from (`[update] repo`).
pub const DEFAULT_REPO: &str = "aterm";

/// GitHub account the atpkg signed package index is published under. Its own constant: the
/// index's trust is account-bound, so it must not move when [`DEFAULT_OWNER`] (the app
/// channel) is repointed, and the workspace `repository` owner is private, so a tokenless
/// install 404s there.
/// Defined from `aterm_types::channel::PUBLIC_OWNER`, which agh's release floor reads; it
/// stays its own constant.
pub const ATPKG_INDEX_OWNER: &str = aterm_types::channel::PUBLIC_OWNER;

/// Whether this build honours a repointed update source (`[update]` owner/repo): only a
/// development build does (`debug_assertions` or the `dev-seams` feature, which the release
/// cutter never enables). A shipped binary reads its compiled channel and logs the repoint.
pub const REPOINT_IS_A_DEV_SEAM: bool = cfg!(any(debug_assertions, feature = "dev-seams"));

/// The resolved GitHub release source, `github.com/<owner>/<repo>` ([`Source::resolve`]).
/// Repointing it is no authenticity downgrade: the anchors in [`crate::pins`] still decide
/// whether bytes are trusted, so a source serving attacker-chosen bytes only fails
/// verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// GitHub owner (user/org) the releases live under.
    pub owner: String,
    /// GitHub repository name.
    pub repo: String,
}

impl Source {
    /// Resolve the update source from the GUI config's `[update]` owner/repo (`None` when
    /// unset). Only a development build honours them; a blank or invalid value falls through
    /// to the compiled default.
    #[must_use]
    pub fn resolve(cfg_owner: Option<&str>, cfg_repo: Option<&str>) -> Self {
        Self::resolve_with(REPOINT_IS_A_DEV_SEAM, cfg_owner, cfg_repo)
    }

    /// [`Self::resolve`] with the build's seam posture as an input, so both postures
    /// are pinned by a test in one build.
    #[must_use]
    pub fn resolve_with(dev_seams: bool, cfg_owner: Option<&str>, cfg_repo: Option<&str>) -> Self {
        if !dev_seams {
            if cfg_owner.or(cfg_repo).is_some_and(|v| !v.trim().is_empty()) {
                note_ignored_repoint();
            }
            return Self {
                owner: DEFAULT_OWNER.to_string(),
                repo: DEFAULT_REPO.to_string(),
            };
        }
        Self {
            owner: pick_slug("[update] owner", cfg_owner, DEFAULT_OWNER),
            repo: pick_slug("[update] repo", cfg_repo, DEFAULT_REPO),
        }
    }
}

/// Log, once per process, that a shipped binary ignores a configured `[update]` repoint.
fn note_ignored_repoint() {
    static SAID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !SAID.swap(true, std::sync::atomic::Ordering::Relaxed) {
        aterm_log::warn!(
            "aterm-update: [update] owner/repo is a development setting — this build \
             reads its compiled channel ({DEFAULT_OWNER}/{DEFAULT_REPO}); remove the keys"
        );
    }
}

/// `value` when it is present, non-blank and a valid slug; otherwise the compiled `default`,
/// with a warning naming `origin` (the config key) for a present but invalid value.
#[must_use]
pub fn pick_slug(origin: &str, value: Option<&str>, default: &str) -> String {
    if let Some(v) = value.map(str::trim).filter(|s| !s.is_empty()) {
        if is_valid_slug(v) {
            return v.to_string();
        }
        warn_invalid(origin, v, default);
    }
    default.to_string()
}

/// Whether `s` is safe to interpolate into a GitHub URL as one path segment: non-empty, at
/// most 100 bytes, not `.`/`..`, and only `A–Z a–z 0–9 . _ -`. A URL-safety allowlist, not a
/// GitHub-name validator: a URL-safe name that is no real repo simply 404s.
#[inline]
#[must_use]
pub fn is_valid_slug(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        // Bare `.`/`..` would traverse the URL.
        && s != "."
        && s != ".."
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// Warn that `origin` was ignored because `value` is not a valid GitHub name.
fn warn_invalid(origin: &str, value: &str, default: &str) {
    aterm_log::warn!("ignoring {origin} = {value:?}: not a valid GitHub name; using {default}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_validation_accepts_real_names_rejects_url_metacharacters() {
        for ok in [
            "alabsystems",
            "aterm",
            "a",
            "My-Org_1",
            "repo.name",
            "x.y.z",
        ] {
            assert!(is_valid_slug(ok), "{ok:?} should be valid");
        }
        // Anything that could redirect the API URL at another host/path must be rejected.
        for bad in [
            "",           // empty
            "owner/repo", // path separator
            "..",         // traversal
            "a b",        // space
            "evil.com/x", // host injection
            "x?y",        // query
            "x#y",        // fragment
            "x@y",        // userinfo
            "x\ny",       // newline
            "%2e%2e",     // percent-encoding
            "café",       // non-ASCII
        ] {
            assert!(!is_valid_slug(bad), "{bad:?} should be rejected");
        }
        assert!(!is_valid_slug(&"a".repeat(101)));
        assert!(is_valid_slug(&"a".repeat(100)));
    }

    #[test]
    fn pick_slug_takes_a_valid_value_else_the_default() {
        assert_eq!(
            pick_slug("[update] owner", Some("cfgowner"), "default"),
            "cfgowner"
        );
        assert_eq!(pick_slug("[update] owner", None, "default"), "default");
        // blank/whitespace is treated as absent → the default.
        assert_eq!(
            pick_slug("[update] owner", Some("   "), "default"),
            "default"
        );
        // values are trimmed.
        assert_eq!(
            pick_slug("[update] owner", Some("  cfg \n"), "default"),
            "cfg"
        );
        // An invalid value never redirects: the trusted default.
        assert_eq!(
            pick_slug("[update] owner", Some("c/d"), "default"),
            "default"
        );
        assert_eq!(
            pick_slug("[update] owner", Some("a b"), "default"),
            "default"
        );
    }

    /// A development build honours a configured repoint; a shipped one never does. A test
    /// build is a development build, so the shipped posture goes through `resolve_with`.
    #[test]
    fn a_config_repoint_is_honoured_only_by_a_development_build() {
        let dev = Source::resolve_with(true, Some("fork-owner"), Some("fork-repo"));
        assert_eq!(
            (dev.owner.as_str(), dev.repo.as_str()),
            ("fork-owner", "fork-repo")
        );
        let shipped = Source::resolve_with(false, Some("fork-owner"), Some("fork-repo"));
        assert_eq!(
            (shipped.owner.as_str(), shipped.repo.as_str()),
            (DEFAULT_OWNER, DEFAULT_REPO),
            "a shipped binary is never repointed by a config file"
        );
        // Half a repoint in a dev build keeps the other half compiled.
        let half = Source::resolve_with(true, Some("fork-owner"), None);
        assert_eq!(half.repo, DEFAULT_REPO);
    }
}
