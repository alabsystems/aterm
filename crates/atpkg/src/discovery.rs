// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Resolving the configurable account and the one bootstrap asset it points at (§5).
//!
//! The manager **never enumerates** the account. It resolves the configurable owner
//! (R3, default `alabsystems`) by reusing `aterm-update-core`'s URL-safety slug
//! resolution verbatim — [`pick_slug`](aterm_update_core::pick_slug) /
//! [`is_valid_slug`](aterm_update_core::is_valid_slug), the same gate that keeps a
//! stray/hostile config value from redirecting fetches off the GitHub API — and then
//! fetches exactly one asset: `index.toml` on `<account>/aterm-toolchain-index`
//! ([`crate::manifest::INDEX_REPO`]). Everything installable flows from that one
//! root-signed document; an unlisted repo is unreachable by construction (§5,
//! [`crate::manifest::Index::installable`]).

use crate::manifest::INDEX_REPO;

/// The env override for the account (owner) the index lives under. Precedence is
/// **env > config > default**, mirroring the updater's source resolution but on a
/// package-manager-specific key so the two never cross-talk.
const ACCOUNT_ENV: &str = "ATPKG_ACCOUNT";

/// The env override for which repo hosts the signed index.
const INDEX_REPO_ENV: &str = "ATPKG_INDEX_REPO";

/// The repository (under the resolved account) that hosts the signed index, with the
/// `ATPKG_INDEX_REPO` env override falling back to [`INDEX_REPO`] (the `aterm` repo). The
/// override is URL-safety-validated so a malformed value can never redirect the fetch off
/// the GitHub API; an invalid value falls back to the default. Repointing the *host* is
/// never an authenticity change — the index is still verified against the pinned root key.
#[must_use]
pub fn index_repo() -> String {
    std::env::var(INDEX_REPO_ENV)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| aterm_update_core::is_valid_slug(s))
        .unwrap_or_else(|| INDEX_REPO.to_string())
}

/// The resolved index location: `github.com/<owner>/aterm-toolchain-index`. `repo` is
/// always [`INDEX_REPO`]; only the `owner` (account) is configurable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexRepo {
    /// The GitHub account (owner) the toolchain index + program repos live under.
    pub owner: String,
    /// Always [`INDEX_REPO`] — the well-known index repository name.
    pub repo: String,
}

impl IndexRepo {
    /// `<owner>/<repo>` — the slug used to build the Releases API URL for the index.
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

/// Resolve the index account by precedence **env (`ATPKG_ACCOUNT`) > config > default
/// (`alabsystems`)**, validated by the shared URL-safety allowlist so a malformed
/// value can never redirect fetches at a different host/path (it falls through to the
/// next source). `cfg_account` is the value the GUI threads in from `[packages].account`
/// (`None` when unset).
///
/// Repointing the account is **not** an authenticity downgrade: the install gate is the
/// root/release signature (§8), not where the bytes come from — and pointing at a
/// *different owner* additionally requires pinning that owner's root key (§8
/// account-bound trust), so a bare repoint is same-owner mirror/relocation only.
#[must_use]
pub fn resolve_account(cfg_account: Option<&str>) -> IndexRepo {
    let env = std::env::var(ACCOUNT_ENV).ok();
    resolve_account_with(env.as_deref(), cfg_account)
}

/// Pure core of [`resolve_account`]: precedence + URL-safety validation over an
/// already-read `env` value, so the env-override branch is unit-testable without
/// mutating the process environment (which is `unsafe`/UB-prone under edition 2024).
#[must_use]
pub fn resolve_account_with(env: Option<&str>, cfg_account: Option<&str>) -> IndexRepo {
    let owner = aterm_update_core::pick_slug(
        ACCOUNT_ENV,
        env,
        cfg_account,
        aterm_update_core::DEFAULT_OWNER,
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
    fn defaults_to_alabsystems_index_repo() {
        // With no env override and no config, the default account + fixed index repo.
        // (ATPKG_ACCOUNT is atpkg-specific and unset in dev/CI shells.)
        if std::env::var_os(ACCOUNT_ENV).is_none() {
            let r = resolve_account(None);
            assert_eq!(r.owner, aterm_update_core::DEFAULT_OWNER);
            assert_eq!(r.owner, "alabsystems");
            assert_eq!(r.repo, INDEX_REPO);
            assert_eq!(r.repo, "aterm"); // the index rides the aterm repo itself (§16)
            assert_eq!(r.slug(), "alabsystems/aterm");
        }
    }

    #[test]
    fn index_repo_defaults_to_aterm_when_unset() {
        // No env mutation (process-global): assert only the default branch, which is what
        // ships. The override path reuses the shared, separately-tested `is_valid_slug`.
        if std::env::var_os(INDEX_REPO_ENV).is_none() {
            assert_eq!(index_repo(), INDEX_REPO);
            assert_eq!(index_repo(), "aterm");
        }
    }

    #[test]
    fn config_account_overrides_default_but_is_validated() {
        if std::env::var_os(ACCOUNT_ENV).is_none() {
            // A valid config account is used.
            assert_eq!(resolve_account(Some("my-org")).owner, "my-org");
            // An invalid (URL-metacharacter) account is rejected → falls back to default,
            // so it can never redirect the index fetch off api.github.com.
            assert_eq!(
                resolve_account(Some("evil.com/x")).owner,
                aterm_update_core::DEFAULT_OWNER
            );
            assert_eq!(
                resolve_account(Some("a b")).owner,
                aterm_update_core::DEFAULT_OWNER
            );
        }
    }

    // The env-override branch, exercised through the pure split (no process-env mutation):
    // env wins over config when valid; an invalid env value is skipped (falls through to
    // config, then default) — it can never redirect the index fetch.
    #[test]
    fn env_override_precedence_and_validation() {
        // env beats config.
        assert_eq!(
            resolve_account_with(Some("env-org"), Some("cfg-org")).owner,
            "env-org"
        );
        // env present but config absent.
        assert_eq!(resolve_account_with(Some("env-org"), None).owner, "env-org");
        // invalid env → skip to config.
        assert_eq!(
            resolve_account_with(Some("bad/env"), Some("cfg-org")).owner,
            "cfg-org"
        );
        // invalid env AND no/invalid config → trusted default (never an attacker slug).
        assert_eq!(
            resolve_account_with(Some("e v i l"), Some("c/d")).owner,
            aterm_update_core::DEFAULT_OWNER
        );
        // blank env is treated as absent.
        assert_eq!(
            resolve_account_with(Some("   "), Some("cfg-org")).owner,
            "cfg-org"
        );
        // the index repo is always the fixed well-known name.
        assert_eq!(resolve_account_with(Some("env-org"), None).repo, INDEX_REPO);
    }
}
