// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Resolving the configurable account and the one bootstrap asset it points at (§5).
//!
//! The manager **never enumerates** the account. It resolves the configurable owner
//! (R3, default `alabsystems`) by reusing `aterm-update-core`'s URL-safety slug
//! resolution verbatim — [`pick_slug`](aterm_update_core::pick_slug) /
//! [`is_valid_slug`](aterm_update_core::is_valid_slug), the same gate that keeps a
//! stray/hostile config value from redirecting fetches off GitHub — and then reads the
//! signed index from `<account>/aterm`'s release download host
//! ([`crate::manifest::INDEX_REPO`]). Everything installable flows from that one
//! root-signed document; an unlisted repo is unreachable by construction (§5,
//! [`crate::manifest::Index::installable`]).

use crate::manifest::INDEX_REPO;

/// The repository (under the resolved account) that hosts the signed index: always
/// [`INDEX_REPO`] (the `aterm` repo). The `ATPKG_INDEX_REPO` override is gone
/// (2026-09-23, R2: one true path, no env alternatives) — it had no config twin and
/// contradicted this module's own rule that only the account is configurable.
#[must_use]
pub fn index_repo() -> String {
    INDEX_REPO.to_string()
}

/// The resolved index location: `github.com/<owner>/aterm`. `repo` is
/// always [`INDEX_REPO`]; only the `owner` (account) is configurable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexRepo {
    /// The GitHub account (owner) the toolchain index + program repos live under.
    pub owner: String,
    /// Always [`INDEX_REPO`] — the well-known index repository name.
    pub repo: String,
}

impl IndexRepo {
    /// `<owner>/<repo>` — the slug the index's release download URLs are built from.
    #[must_use]
    pub fn slug(&self) -> String {
        // Manual concat of the previous `format!("{}/{}", self.owner, self.repo)`
        // — byte-identical: the `format!` expansion embeds `fmt::Arguments`
        // construction (with inlined `unsafe`) that the strict Trust gate cannot
        // lower and fails closed on.
        let mut s = self.owner.clone();
        s.push('/');
        s.push_str(&self.repo);
        s
    }
}

/// Resolve the index account: `cfg_account` — the `[packages].account` value, which
/// only a DEVELOPMENT build ever hands over (`config::admit_packages` drops it from a
/// shipped binary's table, like every repoint) — else the compiled
/// [`aterm_update_core::ATPKG_INDEX_OWNER`] (`alabsystems`). Validated by the shared
/// URL-safety allowlist so a malformed value can never redirect fetches at a different
/// host/path (it falls through to the default). The `ATPKG_ACCOUNT` env override is gone
/// (2026-09-23).
///
/// Repointing the account is **not** an authenticity downgrade: the install gate is the
/// root/release signature (§8), not where the bytes come from — and pointing at a
/// *different owner* additionally requires pinning that owner's root key (§8
/// account-bound trust), so a bare repoint is same-owner mirror/relocation only.
#[must_use]
pub fn resolve_account(cfg_account: Option<&str>) -> IndexRepo {
    // ATPKG_INDEX_OWNER — the package index's OWN tracked key
    // (`[workspace.metadata.atpkg] account`), neither of the updater's slugs:
    //   * not DEFAULT_OWNER — that is the APP update channel; following it would
    //     silently move the index's ACCOUNT-BOUND trust root (§8) whenever the
    //     channel is repointed, turning a channel change into an authenticity
    //     change;
    //   * not PUBLISH_OWNER — that is the PRIVATE staging repo, which a
    //     default-configured (tokenless) install cannot read at all, so binding
    //     the index to it orphaned every such install from the published
    //     registry.
    // Repointing the compiled default is a HOST decision only: the index still
    // verifies against the pinned root key wherever it is fetched from.
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
    fn defaults_to_the_public_alabsystems_index_repo() {
        // With no config, the default account + fixed index repo — on every machine:
        // no environment is read.
        let r = resolve_account(None);
        assert_eq!(r.owner, aterm_update_core::ATPKG_INDEX_OWNER);
        assert_eq!(r.owner, "alabsystems");
        assert_eq!(r.repo, INDEX_REPO);
        assert_eq!(r.repo, "aterm"); // the index rides the aterm repo itself (§16)
        assert_eq!(r.slug(), "alabsystems/aterm");
    }

    #[test]
    fn config_account_overrides_default_but_is_validated() {
        // A valid config account is used (only a development build hands one over).
        assert_eq!(resolve_account(Some("my-org")).owner, "my-org");
        assert_eq!(resolve_account(Some("my-org")).repo, INDEX_REPO);
        // An invalid (URL-metacharacter) account is rejected → falls back to default,
        // so it can never redirect the index fetch off api.github.com.
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
