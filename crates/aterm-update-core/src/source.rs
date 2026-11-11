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
            if has_nonempty_repoint(cfg_owner, cfg_repo) {
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

fn has_nonempty_repoint(cfg_owner: Option<&str>, cfg_repo: Option<&str>) -> bool {
    cfg_owner.is_some_and(|v| !v.trim().is_empty())
        || cfg_repo.is_some_and(|v| !v.trim().is_empty())
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

/// The value is the person's aterm.toml, so aterm.log keeps its length (DESIGN §3.6).
fn warn_invalid(origin: &str, value: &str, default: &str) {
    aterm_log::warn!(
        "ignoring {origin} ({}): not a valid GitHub name; using {default}",
        aterm_log::words::Len(value)
    );
}

#[cfg(test)]
#[path = "source_tests.rs"]
mod tests;
