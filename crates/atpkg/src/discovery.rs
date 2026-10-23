// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Index account and repository resolution ([`crate::manifest::Index::installable`]).

use crate::manifest::INDEX_REPO;

/// The repository under the resolved account that hosts the signed index ([`INDEX_REPO`]).
#[must_use]
pub fn index_repo() -> String {
    INDEX_REPO.to_string()
}

/// The resolved index location, `github.com/<owner>/aterm`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexRepo {
    /// The GitHub account the index and program repos live under.
    pub owner: String,
    /// Always [`INDEX_REPO`].
    pub repo: String,
}

impl IndexRepo {
    /// `<owner>/<repo>`.
    #[must_use]
    pub fn slug(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }
}

/// Resolve the index account ([`aterm_update_core::ATPKG_INDEX_OWNER`]).
#[must_use]
pub fn resolve_account(cfg_account: Option<&str>) -> IndexRepo {
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
        assert_eq!(
            resolve_account(Some("evil.com/x")).owner,
            aterm_update_core::ATPKG_INDEX_OWNER
        );
        assert_eq!(
            resolve_account(Some("a b")).owner,
            aterm_update_core::ATPKG_INDEX_OWNER
        );
        assert_eq!(
            resolve_account(Some("   ")).owner,
            aterm_update_core::ATPKG_INDEX_OWNER
        );
    }
}
