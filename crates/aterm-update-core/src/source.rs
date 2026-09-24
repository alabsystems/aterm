// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Resolving the GitHub release source (`github.com/<owner>/<repo>`): the compiled
//! default, which a DEVELOPMENT build may repoint through `[update]` owner/repo in
//! aterm.toml, with a URL-safety allowlist on every candidate so a configured value can
//! never redirect the updater at a different host/path. Artifact-agnostic: the owner/repo only decide *where the
//! bytes come from*, never *whether they are trusted* (the authenticity anchor
//! lives in the consuming crate, e.g. the pinned Team ID + Apple notarization).

/// Default GitHub owner of the release repository (the `OWNER` in
/// `github.com/OWNER/REPO`) — THE channel of every shipped binary. A development build
/// may repoint it through the GUI's `[update] owner` config ([`REPOINT_IS_A_DEV_SEAM`]);
/// the `$ATERM_UPDATE_OWNER` override is gone (2026-09-23, R2: no env alternatives).
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

/// Default GitHub repository name the updater pulls releases from. Repointable in a
/// development build exactly like [`DEFAULT_OWNER`] (`[update] repo` config), and
/// likewise derived from the workspace manifest by `build.rs`.
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

/// Whether this build honours a REPOINTED update source (`[update]` owner/repo in
/// aterm.toml): only a development build does — `debug_assertions`, or this crate's
/// `dev-seams` feature, which the release cutter never enables. The owner's rule
/// (2026-09-23): one true path; alternatives are for development. A shipped binary
/// reads its compiled channel and names a configured repoint once in the log.
pub const REPOINT_IS_A_DEV_SEAM: bool = cfg!(any(debug_assertions, feature = "dev-seams"));

/// The resolved GitHub release source: `github.com/<owner>/<repo>`. Construct it
/// with [`Source::resolve`]:
///
/// 1. in a DEVELOPMENT build ([`REPOINT_IS_A_DEV_SEAM`]), the values the caller threads
///    in from the GUI's `[update]` config table;
/// 2. [`DEFAULT_OWNER`] / [`DEFAULT_REPO`] — the only source of a shipped binary.
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
    /// read from the GUI config (`None` when unset). A development build honours them
    /// ([`REPOINT_IS_A_DEV_SEAM`]); a shipped binary names them once in the log and
    /// reads its compiled channel. An unset/blank/syntactically-invalid value falls
    /// through to the compiled default.
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

/// The one log line a shipped binary gives a configured `[update]` repoint it will not
/// use — once per process, however often the source is resolved.
fn note_ignored_repoint() {
    static SAID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !SAID.swap(true, std::sync::atomic::Ordering::Relaxed) {
        aterm_log::warn!(
            "aterm-update: [update] owner/repo is a development setting — this build \
             reads its compiled channel ({DEFAULT_OWNER}/{DEFAULT_REPO}); remove the keys"
        );
    }
}

/// Validation for one slug: `value` wins when it is present, non-blank AND a valid
/// GitHub owner/repo name; a present-but-invalid value is skipped with a warning that
/// names `origin` (the config key it came from); otherwise the (trusted, compiled-in)
/// `default` is used. Side-effect-free apart from the warning, so it is unit-testable.
#[must_use]
pub fn pick_slug(origin: &str, value: Option<&str>, default: &str) -> String {
    if let Some(v) = value.map(str::trim).filter(|s| !s.is_empty()) {
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
        assert_eq!(DEFAULT_OWNER, "alabsystems");
        assert_eq!(DEFAULT_REPO, "aterm");
        // No environment is read any more, so this holds on every machine.
        let s = Source::resolve(None, None);
        assert_eq!(s.owner, DEFAULT_OWNER);
        assert_eq!(s.repo, DEFAULT_REPO);
    }

    /// THE REPOINT IS A DEVELOPMENT SEAM (2026-09-23). A development build honours a
    /// configured `[update]` owner/repo; a shipped binary reads its compiled channel
    /// whatever the file says — and a test build is a development build, so the
    /// shipped posture is pinned through [`Source::resolve_with`].
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
