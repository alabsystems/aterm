// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Seed reconcile — turn the compiled-in [`crate::companions`] manifest into installed,
//! shimmed tools "batteries included". This module owns the **gating policy** (what may be
//! source-built, and when), the **status ledger** (`seed-status.toml`, so a background
//! reconcile is observable and rate-limited), and the **reseed-only-on-change** logic. The
//! network/signed-install orchestration (the prebuilt lane, 2a) stays in `main.rs` where the
//! fetcher lives; this module drives the source lane (2b) and honest skips (2c).
//!
//! The gates enforce the review's blockers:
//! * source-build is **DEFAULT-OFF**, opted in per-machine via `ATPKG_SOURCE_BUILD=1`
//!   (toolchain presence is never consent), and runs **regardless of the root key** — it
//!   never consults `enabled()`/the signed gate, and never relaxes it;
//! * it **never** runs when offline, `ATPKG_DISABLE`/`ATPKG_NO_SOURCE_BUILD`/`ATPKG_MANAGED`
//!   is set, or the companion policy forbids source-build;
//! * a transiently-failing build is capped at `retry_cap` attempts per pinned commit, then
//!   goes quiescent until the pin changes (no laptop-cooking rebuild storm).

use std::collections::BTreeMap;
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::companions::{Companion, Manifest, SeedPolicy};
use crate::sourcebuild;
use crate::store::Layout;

/// Why a companion's source-build lane was declined (each is an honest, actionable skip).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// The machine has not opted in (`ATPKG_SOURCE_BUILD=1`).
    NotOptedIn,
    /// The companion is `prebuilt-only` (never source-built on a user box).
    PrebuiltOnly,
    /// `ATPKG_DISABLE` / `ATPKG_NO_SOURCE_BUILD` / `ATPKG_MANAGED` is set.
    OptedOut(&'static str),
    /// A required tool (git/cargo) is missing.
    MissingToolchain(&'static str),
    /// `rustc` is below the companion's `min_toolchain`.
    RustcTooOld { have: String, need: String },
    /// No network — never spawn a fetch/build offline.
    Offline,
}

impl std::fmt::Display for SkipReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotOptedIn => write!(
                f,
                "source-build not opted in (set ATPKG_SOURCE_BUILD=1 to build companions from source)"
            ),
            Self::PrebuiltOnly => {
                write!(f, "policy is prebuilt-only (a signed package is required)")
            }
            Self::OptedOut(v) => write!(f, "{v} is set"),
            Self::MissingToolchain(t) => write!(f, "{t} is not installed"),
            Self::RustcTooOld { have, need } => {
                write!(f, "rustc {have} is below the required {need}")
            }
            Self::Offline => write!(f, "offline"),
        }
    }
}

/// Whether this machine has opted into building companions from source. DEFAULT-OFF; the
/// manifest default may turn it on globally, and `ATPKG_SOURCE_BUILD` (truthy) opts a single
/// machine in. Toolchain presence is deliberately NOT part of this.
#[must_use]
pub fn opted_in(seed: &SeedPolicy) -> bool {
    seed.source_build_default || env_truthy("ATPKG_SOURCE_BUILD")
}

fn env_truthy(key: &str) -> bool {
    std::env::var(key)
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

/// The split-gated source-build predicate (the review's blocker #1/#2). Gathers the env +
/// toolchain + network signals, then defers to the pure [`gate_pure`] for the decision ORDER.
/// ALL conditions must hold. Never consults the root key.
pub fn source_build_gate(c: &Companion, seed: &SeedPolicy) -> Result<(), SkipReason> {
    // Cheap env/policy signals first; probe git/cargo only if still in the running; the
    // network probe (the most expensive) strictly last.
    let disabled = std::env::var_os("ATPKG_DISABLE").is_some();
    let no_source_build = std::env::var_os("ATPKG_NO_SOURCE_BUILD").is_some();
    let managed = std::env::var_os("ATPKG_MANAGED").is_some();
    let pre = gate_pure(
        disabled,
        no_source_build,
        managed,
        c.source_build_allowed(),
        opted_in(seed),
        true, // toolchain/online resolved below; keep the pure order but short-circuit early
        None,
        true,
    );
    pre?; // env/policy/opt-in gate

    if !sourcebuild::have_tool("git") {
        return Err(SkipReason::MissingToolchain("git"));
    }
    if !sourcebuild::have_tool("cargo") {
        return Err(SkipReason::MissingToolchain("cargo"));
    }
    if !c.min_toolchain.is_empty()
        && let (Some(have), Some(need)) = (
            sourcebuild::probe_rustc_version(),
            sourcebuild::parse_semver(&c.min_toolchain),
        )
        && have < need
    {
        return Err(SkipReason::RustcTooOld {
            have: format!("{}.{}.{}", have.0, have.1, have.2),
            need: c.min_toolchain.clone(),
        });
    }
    if !is_online() {
        return Err(SkipReason::Offline);
    }
    Ok(())
}

/// The PURE gate policy — no env reads, no probes — so the decision order is unit-testable
/// without mutating process-global state. `rustc_too_old` carries `(have, need)` when the
/// installed rustc is below the companion floor. Used by [`source_build_gate`] for the
/// env/policy/opt-in prefix and exercised directly in tests.
#[allow(clippy::fn_params_excessive_bools)]
#[allow(clippy::too_many_arguments)] // Pure decision seam mirrors eight independent gate inputs.
pub fn gate_pure(
    disabled: bool,
    no_source_build: bool,
    managed: bool,
    source_build_allowed: bool,
    opted_in: bool,
    toolchain_present: bool,
    rustc_too_old: Option<(String, String)>,
    online: bool,
) -> Result<(), SkipReason> {
    if disabled {
        return Err(SkipReason::OptedOut("ATPKG_DISABLE"));
    }
    if no_source_build {
        return Err(SkipReason::OptedOut("ATPKG_NO_SOURCE_BUILD"));
    }
    if managed {
        return Err(SkipReason::OptedOut("ATPKG_MANAGED"));
    }
    if !source_build_allowed {
        return Err(SkipReason::PrebuiltOnly);
    }
    if !opted_in {
        return Err(SkipReason::NotOptedIn);
    }
    if !toolchain_present {
        return Err(SkipReason::MissingToolchain("cargo"));
    }
    if let Some((have, need)) = rustc_too_old {
        return Err(SkipReason::RustcTooOld { have, need });
    }
    if !online {
        return Err(SkipReason::Offline);
    }
    Ok(())
}

/// Best-effort connectivity probe: a bounded TCP connect to `github.com:443`. Returning
/// `false` means "do not even spawn a fetch/build" (honors the no-network-fingerprint gate).
#[must_use]
pub fn is_online() -> bool {
    let Ok(mut addrs) = ("github.com", 443).to_socket_addrs() else {
        return false;
    };
    let Some(addr) = addrs.next() else {
        return false;
    };
    TcpStream::connect_timeout(&addr, Duration::from_secs(3)).is_ok()
}

// ---------------------------------------------------------------------- the ledger ------

/// One companion's line in `seed-status.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LedgerEntry {
    /// `building` | `ready` | `failed` | `skipped`.
    pub state: String,
    /// `source` | `signed` | `""`.
    #[serde(default)]
    pub provenance: String,
    /// The pinned commit this line is about (retry-cap + reseed key).
    #[serde(default)]
    pub commit: String,
    /// The installed store build number, when ready.
    #[serde(default)]
    pub build: u64,
    /// Source-build attempts for THIS commit (retry cap).
    #[serde(default)]
    pub attempts: u32,
    /// A human reason for `failed`/`skipped`.
    #[serde(default)]
    pub reason: String,
    /// Unix seconds of the last update.
    #[serde(default)]
    pub updated_unix: u64,
}

/// The whole seed status ledger.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Ledger {
    /// Per-companion state, keyed by program name.
    #[serde(default)]
    pub companions: BTreeMap<String, LedgerEntry>,
}

impl Ledger {
    /// The ledger path under the hardened prefix.
    fn path(layout: &Layout) -> std::path::PathBuf {
        layout.prefix.join("seed-status.toml")
    }

    /// Read the ledger (an absent/corrupt file reads as empty).
    #[must_use]
    pub fn read(layout: &Layout) -> Self {
        std::fs::read_to_string(Self::path(layout))
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default()
    }

    /// Persist the ledger (best-effort; a failure here never fails a reconcile).
    pub fn write(&self, layout: &Layout) {
        if let Ok(text) = toml::to_string(self) {
            let _ = std::fs::write(Self::path(layout), text);
        }
    }

    fn set(&mut self, name: &str, entry: LedgerEntry) {
        self.companions.insert(name.to_string(), entry);
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The commit of the currently-installed (active) build of `program`, from its provenance
/// sidecar. `None` if not installed or the active build is not source-built.
#[must_use]
pub fn installed_commit(layout: &Layout, program: &str) -> Option<String> {
    let build = crate::ops::active_builds(layout).get(program).copied()?;
    sourcebuild::read_provenance(layout, program, build).map(|p| p.commit)
}

/// Whether `c` needs a (re)build: not installed, or installed at a DIFFERENT commit than the
/// manifest now pins (reseed-only-on-change — never a rebuild storm on every manifest bump).
#[must_use]
pub fn needs_reseed(layout: &Layout, c: &Companion) -> bool {
    match installed_commit(layout, &c.name) {
        Some(commit) => commit != c.commit,
        None => !crate::ops::active_builds(layout).contains_key(&c.name),
    }
}

/// The outcome of one companion in a reconcile.
#[derive(Debug, Clone)]
pub struct SeedResult {
    /// The program id.
    pub name: String,
    /// `ready` | `reused` | `failed` | `skipped`.
    pub state: String,
    /// A human detail line.
    pub detail: String,
}

/// Run the SOURCE lane of a reconcile over the manifest's seed set (the signed lane, 2a, is
/// handled by the caller when `enabled()`). Idempotent, per-companion isolated, ledgered,
/// retry-capped. `log` receives progress lines. `force` ignores the retry cap.
pub fn reconcile_source(
    layout: &Layout,
    manifest: &Manifest,
    force: bool,
    log: &mut dyn FnMut(&str),
) -> Vec<SeedResult> {
    // The hardened prefix must exist BEFORE the first ledger write / installing-shim (those
    // are best-effort and would otherwise silently no-op until `build_and_install` creates
    // it), so the `building` state + progress UX are observable from the start.
    let _ = crate::platform::ensure_private_dir(&layout.prefix);
    let mut ledger = Ledger::read(layout);
    let mut out = Vec::new();

    for c in manifest.seed_set() {
        // Already installed for the pinned commit → nothing to do.
        if !needs_reseed(layout, c) {
            let build = crate::ops::active_builds(layout)
                .get(&c.name)
                .copied()
                .unwrap_or(0);
            // Derive provenance from the sidecar, not a hardcode — a signed active build must
            // not be mislabelled `source` in the ledger.
            let provenance = if sourcebuild::read_provenance(layout, &c.name, build).is_some() {
                "source"
            } else {
                "signed"
            };
            ledger.set(
                &c.name,
                LedgerEntry {
                    state: "ready".to_string(),
                    provenance: provenance.to_string(),
                    commit: c.commit.clone(),
                    build,
                    attempts: ledger
                        .companions
                        .get(&c.name)
                        .map(|e| e.attempts)
                        .unwrap_or(0),
                    reason: String::new(),
                    updated_unix: now_unix(),
                },
            );
            out.push(SeedResult {
                name: c.name.clone(),
                state: "reused".to_string(),
                detail: format!("already installed for {}", short(&c.commit)),
            });
            continue;
        }

        // Gate the source lane.
        if let Err(reason) = source_build_gate(c, &manifest.seed) {
            log(&format!("{}: skipped — {reason}", c.name));
            ledger.set(
                &c.name,
                LedgerEntry {
                    state: "skipped".to_string(),
                    provenance: String::new(),
                    commit: c.commit.clone(),
                    build: 0,
                    attempts: ledger
                        .companions
                        .get(&c.name)
                        .map(|e| e.attempts)
                        .unwrap_or(0),
                    reason: reason.to_string(),
                    updated_unix: now_unix(),
                },
            );
            out.push(SeedResult {
                name: c.name.clone(),
                state: "skipped".to_string(),
                detail: reason.to_string(),
            });
            continue;
        }

        // Retry cap: a failed OR interrupted build for THIS commit stays quiescent past
        // retry_cap. A persisted `building` on entry means the previous attempt was killed
        // (crash / OOM / timeout) without finishing — count it toward the cap too, else a
        // build that always dies mid-flight would retry forever.
        let prior = ledger.companions.get(&c.name).cloned().unwrap_or_default();
        let attempts_for_commit = if prior.commit == c.commit {
            prior.attempts
        } else {
            0
        };
        let prior_incomplete = prior.state == "failed" || prior.state == "building";
        if !force
            && prior.commit == c.commit
            && prior_incomplete
            && attempts_for_commit >= manifest.seed.retry_cap
        {
            let detail = format!(
                "quiescent after {attempts_for_commit} failed attempts (retry with `atpkg seed --force`)"
            );
            log(&format!("{}: skipped — {detail}", c.name));
            out.push(SeedResult {
                name: c.name.clone(),
                state: "skipped".to_string(),
                detail,
            });
            continue;
        }

        // Mark building + install a transient "installing" shim so bare `<tool>` mid-build
        // reports progress instead of a bare shell error.
        ledger.set(
            &c.name,
            LedgerEntry {
                state: "building".to_string(),
                provenance: "source".to_string(),
                commit: c.commit.clone(),
                build: 0,
                attempts: attempts_for_commit + 1,
                reason: String::new(),
                updated_unix: now_unix(),
            },
        );
        ledger.write(layout);
        install_installing_shims(layout, c);

        match sourcebuild::build_and_install(layout, c, &manifest.seed, log) {
            Ok(installed) => {
                ledger.set(
                    &c.name,
                    LedgerEntry {
                        state: "ready".to_string(),
                        provenance: "source".to_string(),
                        commit: c.commit.clone(),
                        build: installed.build,
                        attempts: attempts_for_commit + 1,
                        reason: String::new(),
                        updated_unix: now_unix(),
                    },
                );
                out.push(SeedResult {
                    name: c.name.clone(),
                    state: if installed.reused { "reused" } else { "ready" }.to_string(),
                    detail: format!("build {} @ {}", installed.build, short(&c.commit)),
                });
            }
            Err(e) => {
                let detail = e.to_string();
                log(&format!("{}: FAILED — {detail}", c.name));
                ledger.set(
                    &c.name,
                    LedgerEntry {
                        state: "failed".to_string(),
                        provenance: "source".to_string(),
                        commit: c.commit.clone(),
                        build: 0,
                        attempts: attempts_for_commit + 1,
                        reason: detail.clone(),
                        updated_unix: now_unix(),
                    },
                );
                out.push(SeedResult {
                    name: c.name.clone(),
                    state: "failed".to_string(),
                    detail,
                });
            }
        }
        ledger.write(layout);
    }

    ledger.write(layout);
    out
}

/// Install a transient "still installing" shim for each exposed name (a failing script that
/// says so), replaced by the real forwarding shim on success. Reuses the tombstone backend
/// through the SAME `shim_allowed` gate, so a sensitive name is still never shadowed.
fn install_installing_shims(layout: &Layout, c: &Companion) {
    // The bin dir must exist before writing a shim into it (the platform backend writes the
    // file but does not create its parent).
    let _ = crate::platform::ensure_private_dir(&layout.bin_dir());
    for raw in &c.expose {
        // Admission IS the deny-list check: a sensitive name has no `ToolName`, so it cannot
        // be turned into a `bin/` path here at all.
        let Some(tool) = crate::store::ToolName::new(raw) else {
            continue;
        };
        let shim = layout.shim(&tool);
        // If a real (forwarding) shim already exists, leave it — a rebuild keeps the old
        // tool runnable until the new build is ready.
        if crate::platform::resolve_shim(&shim).is_some() {
            continue;
        }
        let msg = format!("atpkg: {raw} is still installing — run `aterm pkg status` to watch");
        let _ = crate::platform::install_tombstone_shim(&shim, &msg);
    }
}

/// First 12 chars of a commit, for compact logs.
fn short(commit: &str) -> &str {
    commit.get(..12).unwrap_or(commit)
}

#[cfg(test)]
mod tests {
    use super::*;

    // All gate tests exercise the PURE policy — no process-global env mutation, so they are
    // parallel-safe (no flakiness against tests that read ATPKG_DISABLE / enabled()).

    #[test]
    fn gate_order_disabled_wins_over_everything() {
        assert_eq!(
            gate_pure(true, false, false, true, true, true, None, true),
            Err(SkipReason::OptedOut("ATPKG_DISABLE"))
        );
    }

    #[test]
    fn gate_declines_when_not_opted_in() {
        assert_eq!(
            gate_pure(false, false, false, true, false, true, None, true),
            Err(SkipReason::NotOptedIn)
        );
    }

    #[test]
    fn prebuilt_only_never_source_builds_even_if_opted_in() {
        assert_eq!(
            gate_pure(false, false, false, false, true, true, None, true),
            Err(SkipReason::PrebuiltOnly)
        );
    }

    #[test]
    fn gate_declines_offline_last() {
        assert_eq!(
            gate_pure(false, false, false, true, true, true, None, false),
            Err(SkipReason::Offline)
        );
    }

    #[test]
    fn gate_passes_when_all_hold() {
        assert_eq!(
            gate_pure(false, false, false, true, true, true, None, true),
            Ok(())
        );
    }

    #[test]
    fn rustc_too_old_reports_versions() {
        assert_eq!(
            gate_pure(
                false,
                false,
                false,
                true,
                true,
                true,
                Some(("1.73.0".into(), "1.74.0".into())),
                true
            ),
            Err(SkipReason::RustcTooOld {
                have: "1.73.0".into(),
                need: "1.74.0".into()
            })
        );
    }

    #[test]
    fn opted_in_via_manifest_default_needs_no_env() {
        let mut seed = SeedPolicy::default();
        assert!(!seed.source_build_default);
        seed.source_build_default = true;
        assert!(opted_in(&seed));
    }
}
