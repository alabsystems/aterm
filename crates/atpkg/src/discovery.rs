// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Resolving the index account and repository (§5).
//!
//! The manager never enumerates the account. It resolves the owner through
//! `aterm-update-core`'s slug gate ([`pick_slug`](aterm_update_core::pick_slug)), which keeps a
//! stray or hostile config value from redirecting fetches off GitHub, then reads the
//! root-signed index from `<owner>/aterm`'s releases. Everything installable flows from that
//! index; an unlisted repo is unreachable ([`crate::manifest::Index::installable`]).

use crate::manifest::INDEX_REPO;

/// The repository under the resolved account that hosts the signed index: always
/// [`INDEX_REPO`]. Only the account is configurable.
#[must_use]
pub fn index_repo() -> String {
    INDEX_REPO.to_string()
}

/// The resolved index location, `github.com/<owner>/aterm`; only `owner` is configurable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexRepo {
    /// The GitHub account the index and program repos live under.
    pub owner: String,
    /// Always [`INDEX_REPO`].
    pub repo: String,
}

impl IndexRepo {
    /// `<owner>/<repo>` — the slug the index's release download URLs are built from.
    #[must_use]
    pub fn slug(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }
}

/// Resolve the index account: `cfg_account` (`[packages].account`, which only a development
/// build keeps: `config::admit_packages` drops it from a shipped binary's table), else
/// [`aterm_update_core::ATPKG_INDEX_OWNER`]. A value that fails the shared URL-safety slug
/// check falls through to the default, so it can never redirect fetches at another host.
///
/// Repointing is no authenticity downgrade: installs are gated on the root and release
/// signatures (§8), and a different owner also needs that owner's root key pinned.
#[must_use]
pub fn resolve_account(cfg_account: Option<&str>) -> IndexRepo {
    // The index has its own owner constant, not `DEFAULT_OWNER` (the app update channel:
    // following it would move the index's account-bound trust root whenever the channel is
    // repointed) and not the workspace `repository` owner (a private repo a tokenless
    // install cannot read).
    let owner = aterm_update_core::pick_slug(
        "[packages] account",
        cfg_account,
        aterm_update_core::ATPKG_INDEX_OWNER,
    );
    IndexRepo {
        owner,
        repo: index_repo(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_account_overrides_default_but_is_validated() {
        assert_eq!(resolve_account(Some("my-org")).owner, "my-org");
        assert_eq!(resolve_account(Some("my-org")).repo, INDEX_REPO);
        // An account with URL metacharacters falls back to the default.
        assert_eq!(
            resolve_account(Some("evil.com/x")).owner,
            aterm_update_core::ATPKG_INDEX_OWNER
        );
        assert_eq!(
            resolve_account(Some("a b")).owner,
            aterm_update_core::ATPKG_INDEX_OWNER
        );
        // Blank is absent.
        assert_eq!(
            resolve_account(Some("   ")).owner,
            aterm_update_core::ATPKG_INDEX_OWNER
        );
    }
}
