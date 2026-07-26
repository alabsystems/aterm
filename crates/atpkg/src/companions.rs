// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The compiled-in **companion-tools manifest** (`companions.toml`, baked via
//! [`include_str!`]): the source of truth for the tools aterm ships "batteries included".
//!
//! It rides aterm's OWN notarized/signed release, so it is trusted **independently of the
//! atpkg signed index** — which is what lets the source-build lane run on a machine whose
//! atpkg root key is absent (the keyless POC). Adding the next public repo is pure data:
//! one more `[[companion]]` block, no code change.
//!
//! Two trust rules are load-bearing and enforced by [`Manifest::validate`] (and the
//! `manifest_is_valid` unit test, which fails the build on violation):
//!
//! * **`expose`/`build_args` are OWNER-authoritative.** The runtime never reads a fetched
//!   repo's unsigned `[workspace.metadata.atpkg]`; only what is declared here (and thus
//!   attested by aterm's release signature) decides which binaries get PATH shims and how
//!   they build.
//! * **`coherence` implies `prebuilt-only`.** A source build can never produce a coherent
//!   attested tuple across the trust chain's mutually-incompatible nightlies
//!   (`docs/ATERM-DISTRIBUTION-WEDGE.md` §2.2), so a coherence-grouped companion may never
//!   be source-built on a user's machine.

use serde::Deserialize;

/// The manifest text baked into the binary at compile time.
pub const COMPANIONS_TOML: &str = include_str!("../companions.toml");

/// The parsed, validated companion manifest.
#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    /// Field-shape version; bumped on any schema change. Feeds seed-marker staleness.
    pub schema: u32,
    /// Global seed policy (compiled defaults; env may only tighten, never loosen).
    #[serde(default)]
    pub seed: SeedPolicy,
    /// The companion entries. `#[serde(rename)]` maps the TOML `[[companion]]` array.
    #[serde(default, rename = "companion")]
    pub companions: Vec<Companion>,
}

/// Global seed policy knobs.
#[derive(Debug, Clone, Deserialize)]
pub struct SeedPolicy {
    /// Whether source-build is opted in by default on every machine. DEFAULT-OFF; a machine
    /// opts in with `ATPKG_SOURCE_BUILD=1`. Toolchain presence is never, by itself, consent.
    #[serde(default)]
    pub source_build_default: bool,
    /// Hard wall-clock budget for a source build; the build process-tree is killed past it.
    #[serde(default = "default_build_timeout_secs")]
    pub build_timeout_secs: u64,
    /// Advisory RSS ceiling for the build subprocess (a full watchdog is production TODO).
    #[serde(default = "default_build_rss_limit_mb")]
    pub build_rss_limit_mb: u64,
    /// Source-build attempts per pinned commit before going quiescent (no rebuild storm).
    #[serde(default = "default_retry_cap")]
    pub retry_cap: u32,
    /// HARD free-space gate (GiB) before a source build; budgets the multi-GB target dir.
    #[serde(default = "default_target_free_gb_min")]
    pub target_free_gb_min: u64,
}

impl Default for SeedPolicy {
    fn default() -> Self {
        Self {
            source_build_default: false,
            build_timeout_secs: default_build_timeout_secs(),
            build_rss_limit_mb: default_build_rss_limit_mb(),
            retry_cap: default_retry_cap(),
            target_free_gb_min: default_target_free_gb_min(),
        }
    }
}

const fn default_build_timeout_secs() -> u64 {
    1800
}
const fn default_build_rss_limit_mb() -> u64 {
    4096
}
const fn default_retry_cap() -> u32 {
    3
}
const fn default_target_free_gb_min() -> u64 {
    8
}

/// One companion tool: a public repo aterm ships batteries-included.
#[derive(Debug, Clone, Deserialize)]
pub struct Companion {
    /// The program id (store key + `aterm <name>` verb). Unique across the manifest.
    pub name: String,
    /// The public `owner/repo` slug the source is fetched from (e.g. `alabsystems/ay`).
    pub repo: String,
    /// The MANDATORY 40-hex commit pin — the source-build trust basis. The checked-out HEAD
    /// is asserted byte-equal to this after a full clone; a moving ref is never trusted.
    pub commit: String,
    /// sha256 of the repo's `Cargo.lock` at `commit`; re-checked post-fetch. Pins the build
    /// CLOSURE (transitive deps), not just the repo tree. Mandatory for source-buildable.
    #[serde(default)]
    pub cargo_lock_sha256: String,
    /// OWNER-authoritative PATH-shim set. Each name must pass [`crate::store::shim_allowed`].
    /// Mandatory (non-empty) for a source-buildable policy.
    #[serde(default)]
    pub expose: Vec<String>,
    /// OWNER-authoritative `cargo build` args. Mandatory (non-empty) for source-buildable.
    #[serde(default)]
    pub build_args: Vec<String>,
    /// `source-or-prebuilt` | `prebuilt-only` | `source-only`.
    pub policy: String,
    /// Minimum `rustc` version for a source build (`""` = any stable).
    #[serde(default)]
    pub min_toolchain: String,
    /// Rough build size (MiB) for UX copy + the free-space precheck.
    #[serde(default)]
    pub size_hint_mb: u64,
    /// Coherence group id; non-empty REQUIRES `policy == "prebuilt-only"` (validator).
    #[serde(default)]
    pub coherence: String,
    /// Whether this companion is in the first-run seed set.
    #[serde(default)]
    pub default: bool,
    /// `headline` | `chain` | `optional` — cosmetic tier for `atpkg list`/`doctor`.
    #[serde(default)]
    pub tier: String,
}

impl Companion {
    /// Whether this companion's policy permits a from-source build on a user's machine.
    #[must_use]
    pub fn source_build_allowed(&self) -> bool {
        matches!(self.policy.as_str(), "source-or-prebuilt" | "source-only")
    }

    /// Whether this companion may ONLY be installed as a signed prebuilt (never source-built).
    #[must_use]
    pub fn prebuilt_only(&self) -> bool {
        self.policy == "prebuilt-only"
    }
}

/// Parse + validate the compiled-in manifest. `Err` carries every validation failure joined,
/// so the `manifest_is_valid` test names all problems at once.
pub fn load() -> Result<Manifest, String> {
    let manifest: Manifest =
        toml::from_str(COMPANIONS_TOML).map_err(|e| format!("companions.toml parse error: {e}"))?;
    manifest.validate()?;
    Ok(manifest)
}

impl Manifest {
    /// The companions in the first-run seed set (`default = true`).
    #[must_use]
    pub fn seed_set(&self) -> Vec<&Companion> {
        self.companions.iter().filter(|c| c.default).collect()
    }

    /// Look a companion up by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Companion> {
        self.companions.iter().find(|c| c.name == name)
    }

    /// Enforce the load-bearing invariants. Returns `Err(joined-messages)` on any violation.
    pub fn validate(&self) -> Result<(), String> {
        let mut errs: Vec<String> = Vec::new();
        let mut seen: Vec<&str> = Vec::new();

        for c in &self.companions {
            let who = &c.name;

            // 1. name is a safe, unique program id.
            if !is_valid_program_id(&c.name) {
                errs.push(format!("companion '{who}': name is not a valid program id"));
            }
            if seen.contains(&c.name.as_str()) {
                errs.push(format!("companion '{who}': duplicate name"));
            }
            seen.push(&c.name);

            // 2. commit is exactly 40 lowercase hex.
            if !is_40_hex(&c.commit) {
                errs.push(format!(
                    "companion '{who}': commit must be exactly 40 lowercase hex (got '{}')",
                    c.commit
                ));
            }

            // 3. a valid policy string.
            if !matches!(
                c.policy.as_str(),
                "source-or-prebuilt" | "prebuilt-only" | "source-only"
            ) {
                errs.push(format!(
                    "companion '{who}': policy '{}' is not one of source-or-prebuilt|prebuilt-only|source-only",
                    c.policy
                ));
            }

            // 4. source-buildable ⇒ expose, build_args, cargo_lock_sha256 all present.
            if c.source_build_allowed() {
                if c.expose.is_empty() {
                    errs.push(format!(
                        "companion '{who}': source-buildable policy requires a non-empty expose set"
                    ));
                }
                if c.build_args.is_empty() {
                    errs.push(format!(
                        "companion '{who}': source-buildable policy requires non-empty build_args"
                    ));
                }
                if !is_64_hex(&c.cargo_lock_sha256) {
                    errs.push(format!(
                        "companion '{who}': source-buildable policy requires a 64-hex cargo_lock_sha256"
                    ));
                }
            }

            // 5. coherence ⇒ prebuilt-only (a source build can never join an attested tuple).
            if !c.coherence.is_empty() && !c.prebuilt_only() {
                errs.push(format!(
                    "companion '{who}': coherence group '{}' requires policy = prebuilt-only",
                    c.coherence
                ));
            }

            // 6. every exposed name must be shim-allowed (never shadow sudo/ssh/git/...).
            for e in &c.expose {
                if !crate::store::shim_allowed(e) {
                    errs.push(format!(
                        "companion '{who}': exposed name '{e}' is refused a shim (sensitive/malformed)"
                    ));
                }
            }
        }

        if errs.is_empty() {
            Ok(())
        } else {
            Err(errs.join("; "))
        }
    }
}

/// A valid program id: non-empty, ≤ 64 chars, `[a-z0-9][a-z0-9._-]*`, and itself shim-safe.
#[must_use]
fn is_valid_program_id(s: &str) -> bool {
    if s.is_empty() || s.len() > 64 {
        return false;
    }
    let mut chars = s.chars();
    let first_ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    let rest_ok = s
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'));
    first_ok && rest_ok && crate::store::shim_allowed(s)
}

/// Whether `s` is exactly `n` lowercase-hex characters.
#[must_use]
fn is_n_hex(s: &str, n: usize) -> bool {
    s.len() == n && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[must_use]
fn is_40_hex(s: &str) -> bool {
    is_n_hex(s, 40)
}

#[must_use]
fn is_64_hex(s: &str) -> bool {
    is_n_hex(s, 64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The COMPILED-IN manifest must parse and satisfy every invariant. This test failing
    /// IS the "fails the build" load-time validator the design mandates.
    #[test]
    fn manifest_is_valid() {
        let m = load().unwrap_or_else(|e| panic!("companions.toml invalid: {e}"));
        assert!(m.schema >= 1);
        // The POC ships exactly one live, source-buildable headline companion: ay.
        let ay = m.get("ay").expect("ay companion present");
        assert_eq!(ay.repo, "alabsystems/ay");
        assert!(ay.source_build_allowed());
        assert!(ay.default);
        assert!(ay.expose.contains(&"ay".to_string()));
        assert!(is_40_hex(&ay.commit));
        assert!(is_64_hex(&ay.cargo_lock_sha256));
        assert_eq!(m.seed_set().len(), 1);
    }

    #[test]
    fn coherence_requires_prebuilt_only() {
        let toml = r#"
schema = 3
[[companion]]
name = "ty"
repo = "alabsystems/ty"
commit = "0000000000000000000000000000000000000000"
policy = "source-or-prebuilt"
coherence = "rustc-tuple"
"#;
        let m: Manifest = toml::from_str(toml).unwrap();
        let err = m.validate().unwrap_err();
        assert!(err.contains("requires policy = prebuilt-only"), "{err}");
    }

    #[test]
    fn rejects_non_40_hex_commit_and_sensitive_expose() {
        let toml = r#"
schema = 3
[[companion]]
name = "bad"
repo = "x/bad"
commit = "deadbeef"
cargo_lock_sha256 = "5ead6ece266701790822f094b44a4bb913e4b5240e2d710ab90322deed43fb3d"
expose = ["ssh"]
build_args = ["--bin", "bad"]
policy = "source-or-prebuilt"
"#;
        let m: Manifest = toml::from_str(toml).unwrap();
        let err = m.validate().unwrap_err();
        assert!(err.contains("40 lowercase hex"), "{err}");
        assert!(err.contains("refused a shim"), "{err}");
    }

    #[test]
    fn source_buildable_requires_closure_pins() {
        let toml = r#"
schema = 3
[[companion]]
name = "np"
repo = "x/np"
commit = "8af5cbb3a7aa7779f7a429c1f5772b59737b6cd1"
policy = "source-only"
"#;
        let m: Manifest = toml::from_str(toml).unwrap();
        let err = m.validate().unwrap_err();
        assert!(err.contains("non-empty expose"), "{err}");
        assert!(err.contains("non-empty build_args"), "{err}");
        assert!(err.contains("64-hex cargo_lock_sha256"), "{err}");
    }
}
