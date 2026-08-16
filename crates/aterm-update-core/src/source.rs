// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Resolving the GitHub release source (`github.com/<owner>/<repo>`) by the
//! precedence **env > config > compiled default**, with a URL-safety allowlist on
//! every candidate so a configured value can never redirect the updater at a
//! different host/path. Artifact-agnostic: the owner/repo only decide *where the
//! bytes come from*, never *whether they are trusted* (the authenticity anchor
//! lives in the consuming crate, e.g. the pinned Team ID + Apple notarization).

/// Default GitHub owner of the release repository (the `OWNER` in
/// `github.com/OWNER/REPO`). This is the *fallback* — the source is configurable at
/// runtime (env `ATERM_UPDATE_OWNER`, then the GUI's `[update] owner` config), so a
/// fork/relocation needs no code change. Compiled in only as the last resort.
///
/// NOT a hand-maintained literal: `build.rs` derives it from the single tracked
/// source of truth — `[workspace.metadata.aterm] update_channel` in the workspace
/// `Cargo.toml`, falling back to `[workspace.package] repository` when no separate
/// channel is declared — so the binary's default channel can never drift from the
/// channel the release pipeline actually mirrors to.
///
/// That channel is the PUBLIC mirror, not the private publish repo: it can be read
/// with no credential, which is what lets a freshly installed machine update before
/// anyone has provisioned it a token.
pub const DEFAULT_OWNER: &str = env!("ATERM_DEFAULT_OWNER");

/// Default GitHub repository name the updater pulls releases from. Overridable at
/// runtime exactly like [`DEFAULT_OWNER`] (env `ATERM_UPDATE_REPO`, then `[update]
/// repo` config), and likewise derived from the workspace manifest by `build.rs`.
pub const DEFAULT_REPO: &str = env!("ATERM_DEFAULT_REPO");

/// GitHub account this project is PUBLISHED under — derived by `build.rs` from
/// `[workspace.package] repository` ALONE, never from `update_channel`.
///
/// Deliberately separate from [`DEFAULT_OWNER`]. The two were the same string until
/// the update channel was repointed at a public mirror, and code that means "the
/// account this project belongs to" must not drift with the channel. (The package
/// index reads its own key — [`ATPKG_INDEX_OWNER`] — because binding it here
/// pointed default installs at the private staging repo; this constant remains
/// that key's absent-key fallback and the slug atpkg's token chain resolves
/// against.)
pub const PUBLISH_OWNER: &str = env!("ATERM_PUBLISH_OWNER");

/// Repository name this project is published under, the companion to
/// [`PUBLISH_OWNER`] and derived the same way.
pub const PUBLISH_REPO: &str = env!("ATERM_PUBLISH_REPO");

/// GitHub account the atpkg SIGNED PACKAGE INDEX is published under — stamped by
/// `build.rs` from its own tracked key, `[workspace.metadata.atpkg] account`,
/// falling back to [`PUBLISH_OWNER`] only when that key is absent.
///
/// A third knob on purpose. [`DEFAULT_OWNER`] is the APP update channel: the
/// index's trust is ACCOUNT-BOUND (§8), so it must not move when the channel is
/// repointed at a mirror. [`PUBLISH_OWNER`] is the PRIVATE staging repo: a
/// default-configured (tokenless) install 404s there, so defaulting the index to
/// it orphaned every such install from the published registry. The account only
/// decides where the index BYTES come from; authenticity is the pinned root key
/// (`pins`), which verifies the same signed index wherever it is hosted.
pub const ATPKG_INDEX_OWNER: &str = env!("ATERM_ATPKG_INDEX_OWNER");

/// The resolved GitHub release source: `github.com/<owner>/<repo>`. Construct it
/// with [`Source::resolve`], which applies the precedence
/// **env > config > compiled default**:
///
/// 1. `$ATERM_UPDATE_OWNER` / `$ATERM_UPDATE_REPO` (per-machine override);
/// 2. the values the caller threads in from the GUI's `[update]` config table;
/// 3. [`DEFAULT_OWNER`] / [`DEFAULT_REPO`].
///
/// Repointing the source is **not** an authenticity downgrade: the real anchor is
/// the compiled-in pinned Team ID (plus Apple notarization), so even a source
/// that serves attacker-chosen bytes cannot get an untrusted bundle installed — it
/// just fails verification and nothing is staged. The owner/repo only decide *where
/// the bytes come from*, never *whether they are trusted*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// GitHub owner (user/org) the releases live under.
    pub owner: String,
    /// GitHub repository name.
    pub repo: String,
}

impl Source {
    /// Resolve the update source. `cfg_owner`/`cfg_repo` are the values the caller
    /// read from the GUI config (`None` when unset); env overrides them, and an
    /// unset/blank/syntactically-invalid value at any level falls through to the next.
    #[must_use]
    pub fn resolve(cfg_owner: Option<&str>, cfg_repo: Option<&str>) -> Self {
        Self {
            owner: resolve_slug("ATERM_UPDATE_OWNER", cfg_owner, DEFAULT_OWNER),
            repo: resolve_slug("ATERM_UPDATE_REPO", cfg_repo, DEFAULT_REPO),
        }
    }
}

/// Resolve one slug (owner or repo) by precedence env > config > default. Reads the
/// environment, then delegates the (pure, testable) precedence + validation to
/// [`pick_slug`].
fn resolve_slug(env_key: &str, cfg: Option<&str>, default: &str) -> String {
    let env_val = std::env::var(env_key).ok();
    pick_slug(env_key, env_val.as_deref(), cfg, default)
}

/// Pure precedence + validation for one slug: the first of `env`, then `cfg` that is
/// present, non-blank, AND a valid GitHub owner/repo name wins; a present-but-invalid
/// value is skipped with a warning; if neither qualifies, the (trusted, compiled-in)
/// `default` is used. Side-effect-free apart from the warning, so it is unit-testable
/// without mutating the process environment.
// Skip: the `for .. in [..; 2]` array-literal iterator's drop glue (std
// array::IntoIter over ManuallyDrop internals) is not yet in the drop
// classifier's std scaffold set; the elements are refs + Option<&str>
// (no drop glue of their own). Pure precedence logic, unit-tested.
// Droppable when the array::IntoIter scaffold arm lands.
#[cfg_attr(trust_verify, trust::skip)]
pub fn pick_slug(env_key: &str, env: Option<&str>, cfg: Option<&str>, default: &str) -> String {
    for (origin, cand) in [(env_key, env), ("[update] config", cfg)] {
        let Some(v) = cand.map(str::trim).filter(|s| !s.is_empty()) else {
            continue;
        };
        if is_valid_slug(v) {
            return v.to_string();
        }
        warn_invalid(&format!("{origin} = {v:?}"));
    }
    default.to_string()
}

/// Whether `s` is safe to interpolate into the Releases API URL as one path segment.
/// This is a **URL-safety allowlist**, deliberately a *superset* of the names GitHub
/// actually accepts (`A–Z a–z 0–9 . _ -`, non-empty, length-capped, excluding the bare
/// `.`/`..` traversal segments) — NOT a faithful GitHub-name validator. Its only job is
/// to forbid the metacharacters (`/`, whitespace, `?`, `#`, `@`, …) that could redirect
/// the updater at a different host/path; a value that is URL-safe but not a real repo
/// simply 404s, which is GitHub's authoritative existence check. Fail closed — anything
/// outside the set is rejected and the next source in precedence is used.
#[must_use]
pub fn is_valid_slug(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        // `.` / `..` are valid characters mid-name (e.g. `repo.name`) but the bare
        // path segments are reserved and would traverse the API URL — reject them.
        && s != "."
        && s != ".."
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// Warn that a configured update source was ignored because it isn't a valid
/// owner/repo name.
fn warn_invalid(what: &str) {
    aterm_log::warn!(
        "aterm-update: ignoring invalid update source {what} (not a valid GitHub \
         owner/repo name); falling back"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_validation_accepts_real_names_rejects_url_metacharacters() {
        // GitHub owner/repo names: alphanumerics plus . _ -
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
        // Over-long is rejected (DoS / absurd-value guard).
        assert!(!is_valid_slug(&"a".repeat(101)));
        assert!(is_valid_slug(&"a".repeat(100)));
    }

    #[test]
    fn pick_slug_precedence_env_over_config_over_default() {
        // env wins when valid.
        assert_eq!(
            pick_slug("E", Some("envowner"), Some("cfgowner"), "default"),
            "envowner"
        );
        // config used when env absent.
        assert_eq!(
            pick_slug("E", None, Some("cfgowner"), "default"),
            "cfgowner"
        );
        // default used when both absent.
        assert_eq!(pick_slug("E", None, None, "default"), "default");
        // blank/whitespace at a level is treated as absent → fall through.
        assert_eq!(
            pick_slug("E", Some("   "), Some("cfgowner"), "default"),
            "cfgowner"
        );
        // values are trimmed.
        assert_eq!(
            pick_slug("E", Some("  envowner \n"), None, "default"),
            "envowner"
        );
    }

    #[test]
    fn pick_slug_skips_invalid_and_falls_through() {
        // An invalid env value is skipped, the valid config value is used.
        assert_eq!(
            pick_slug("E", Some("bad/owner"), Some("goodowner"), "default"),
            "goodowner"
        );
        // Invalid at env AND config → trusted default (never an attacker-shaped slug).
        assert_eq!(
            pick_slug("E", Some("a b"), Some("c/d"), "default"),
            "default"
        );
    }

    #[test]
    fn source_resolve_defaults_when_nothing_configured() {
        // The documented default channel is the PUBLIC mirror,
        // github.com/alabsystems/aterm — deliberately NOT the private publish
        // repo named by `[workspace.package] repository`. These constants are
        // DERIVED by build.rs from `[workspace.metadata.aterm] update_channel`,
        // so this also end-to-end checks that the build-time parse read that key
        // (a fall-through to `repository` would spell "alabsystems" here).
        // The release cutter's `mirror` step targets the same slug; the binding
        // is asserted publisher-side in aterm-release's `mirror` module.
        // (Env-independent — the env-override path is covered by `pick_slug`.)
        assert_eq!(DEFAULT_OWNER, "alabsystems");
        assert_eq!(DEFAULT_REPO, "aterm");
        // `Source::resolve` reads the real `ATERM_UPDATE_OWNER`/`_REPO`; only assert it
        // yields the defaults when neither is set, so an ambient override (e.g. a dev
        // testing a fork) can't make this flake.
        if std::env::var_os("ATERM_UPDATE_OWNER").is_none()
            && std::env::var_os("ATERM_UPDATE_REPO").is_none()
        {
            let s = Source::resolve(None, None);
            assert_eq!(s.owner, DEFAULT_OWNER);
            assert_eq!(s.repo, DEFAULT_REPO);
        }
    }

    #[test]
    fn atpkg_index_owner_is_the_public_package_org() {
        // Stamped from `[workspace.metadata.atpkg] account` — NOT from
        // `repository` (a fall-through would spell the private staging owner
        // here: the repo no tokenless install can read, so this assert also
        // tripwires DELETION of the metadata key) and NOT from
        // `update_channel` (the app knob). The literal holds in BOTH trees:
        // the publish/ export rewrites the staging owner into the public org
        // and leaves this value untouched.
        assert_eq!(ATPKG_INDEX_OWNER, "alabsystems");
        assert!(is_valid_slug(ATPKG_INDEX_OWNER));
        // In the private staging tree the index account and the publish owner
        // MUST differ — binding the index default to the publish owner is the
        // exact regression that pointed every default-configured install at a
        // 404 (private) index host. The guard is scoped by SHAPE (publish
        // owner differs from the compiled update-channel owner — exactly the
        // split private-staging/public-channel configuration), never by
        // spelling the staging owner as a literal: the publish/ export
        // blanket-rewrites that literal into the public org, so a spelled
        // guard flips to always-true in the exported tree and deterministically
        // fails there, where publish == channel == index owner is the
        // documented single-public-repo configuration, not a regression.
        if PUBLISH_OWNER != DEFAULT_OWNER {
            assert_ne!(
                ATPKG_INDEX_OWNER, PUBLISH_OWNER,
                "the package index must default to a publicly readable account"
            );
        }
    }
}
