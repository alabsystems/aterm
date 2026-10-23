// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! GitHub release source (`github.com/<owner>/<repo>`) resolution and slug validation.

/// Default GitHub owner of the public release repository.
pub const DEFAULT_OWNER: &str = aterm_types::channel::PUBLIC_OWNER;

/// Default GitHub repository name the updater pulls releases from (`[update] repo`).
pub const DEFAULT_REPO: &str = "aterm";

/// GitHub account the `atpkg` signed package index is published under.
pub const ATPKG_INDEX_OWNER: &str = aterm_types::channel::PUBLIC_OWNER;

/// Whether this build honours a repointed update source (`[update]` `owner`/`repo`).
pub const REPOINT_IS_A_DEV_SEAM: bool = cfg!(any(debug_assertions, feature = "dev-seams"));

/// Resolved GitHub release source (`github.com/<owner>/<repo>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// GitHub owner (user/org) the releases live under.
    pub owner: String,
    /// GitHub repository name.
    pub repo: String,
}

impl Source {
    /// Resolve the update source from `[update]` `owner`/`repo` (honoured only in dev builds).
    #[must_use]
    pub fn resolve(cfg_owner: Option<&str>, cfg_repo: Option<&str>) -> Self {
        Self::resolve_with(REPOINT_IS_A_DEV_SEAM, cfg_owner, cfg_repo)
    }

    /// [`Self::resolve`] with an explicit `dev_seams` posture.
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

fn note_ignored_repoint() {
    static SAID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !SAID.swap(true, std::sync::atomic::Ordering::Relaxed) {
        aterm_log::warn!(
            "aterm-update: [update] owner/repo is a development setting — this build \
             reads its compiled channel ({DEFAULT_OWNER}/{DEFAULT_REPO}); remove the keys"
        );
    }
}

/// Return trimmed `value` if it is a valid slug, or `default` (logging a warning if invalid).
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

/// Whether `s` is safe as a single GitHub URL path segment (`1..=100` ASCII `[A-Za-z0-9._-]` bytes, not `.`/`..`).
#[inline]
#[must_use]
pub fn is_valid_slug(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && s != "."
        && s != ".."
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

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
        assert_eq!(
            pick_slug("[update] owner", Some("   "), "default"),
            "default"
        );
        assert_eq!(
            pick_slug("[update] owner", Some("  cfg \n"), "default"),
            "cfg"
        );
        assert_eq!(
            pick_slug("[update] owner", Some("c/d"), "default"),
            "default"
        );
        assert_eq!(
            pick_slug("[update] owner", Some("a b"), "default"),
            "default"
        );
    }

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
        let half = Source::resolve_with(true, Some("fork-owner"), None);
        assert_eq!(half.repo, DEFAULT_REPO);
    }
}
