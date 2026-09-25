// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `aterm-update` — silent, signature-pinned in-app self-update for the macOS
//! `aterm.app`.
//!
//! Two entry points, both no-ops unless the running process is a real installed
//! `.app` bundle and the updater is configured + enabled:
//!
//! * [`apply_staged_if_ready_preserving_fds_exact`] — call **very early in
//!   `main()`**, before any window/thread. If a previous run staged a verified,
//!   *newer* build, this atomically swaps it into place and re-execs the new binary
//!   (the swap is invisible: same PID/tty/parent). Otherwise it returns and the
//!   current build keeps running. This is the ONE apply entry point the shipping
//!   GUI calls (`aterm-gui`'s `lib.rs`, the boot apply); the plainer
//!   [`apply_staged_if_ready`] and [`apply_staged_if_ready_preserving_fds`]
//!   wrappers exist for callers with no descriptors or no commit to bind, and
//!   nothing in this workspace uses them.
//! * [`spawn_background_check`] — call once the GUI is up. Spawns a detached
//!   thread that talks to the private GitHub Release, downloads the newer DMG,
//!   verifies it, and stages it for the in-session apply lane: the GUI hands the
//!   live session to it in place, and the next launch picks it up only if no
//!   handoff ever completes. It never touches the UI and never blocks the event
//!   loop.
//!
//! # Delivery model: what actually reaches a machine, and when
//!
//! Read this before adding a scheduler. The updater has exactly two moving parts,
//! and the honest bound on "how stale can a machine be" follows from them:
//!
//! * **Staging** happens on [`spawn_background_check`]'s thread, which runs its
//!   FIRST check immediately at launch and then every `cadence::INTERVAL_SECS`
//!   (10 minutes, ±20% jitter; a check that could not reach the network at all
//!   retries sooner, on `cadence::OFFLINE_RETRY`). So a running app stages a new
//!   release within one jittered interval plus the download — up to ~12 minutes
//!   plus the download after publish; an app that is started stages within
//!   seconds of start plus the download.
//! * **Applying** happens in-session through the seamless overlap handoff
//!   (default-on: automatically at the first quiet moment and in any case within
//!   `aterm-gui`'s `LANDS_WITHIN` plus its switch — under a minute — of the stage,
//!   or on one click); so publish-to-applied on a running window is bounded by
//!   about 13 minutes plus the download, not "a minute". The top of the next
//!   `main()` is the fallback
//!   for a stage no handoff ever completed. The in-session lane is
//!   FIELD-PROVEN ACROSS A REAL VERSION BOUNDARY as of 2026-07-28: a released
//!   `v0.6.0` bundle (build 1785122258), installed from the public channel and
//!   launched cold, staged and applied `v0.7.0` (build 1785125098) in-session,
//!   carrying one live PTY across the exec — `overlap handoff: exact adoption
//!   proof for 1 PTY(s) written` then `committing exact readerless handoff to
//!   child`, ~56 s from launch. This supersedes the earlier "never observed to
//!   complete on a real machine" note, which was written 2026-07-24 and was
//!   already stale by 2026-07-25 (same-binary QA-seam proof); see
//!   `docs/RFC-proof-carrying-dsu.md`.
//!
//! Composing those: a machine that never runs aterm is never updated, but it also
//! never *needs* to be — and the first launch after a release stages it and the
//! running app applies it, so the machine is at worst **one apply behind**, not
//! indefinitely stale.
//!
//! ## Why there is no LaunchAgent
//!
//! An obvious proposal is a `launchd` agent that checks periodically so a machine
//! is current before it is even launched. It was considered and REJECTED, for
//! reasons that are about cost and honesty, not taste:
//!
//! 1. **It would buy exactly one launch.** Per the bound above, the agent's only
//!    effect is that a release stages before the launch rather than during it —
//!    the fallback swap still waits for a `main()`, and the in-session lane for a
//!    running app. It cannot update an app nobody runs, because applying an
//!    update means re-execing a process.
//! 2. **There is nothing for it to run.** Every verified path (release selection,
//!    signature/digest checks, DMG mount, staging) lives in this crate, reachable
//!    only from the one shipped Mach-O — and that binary is the GUI: invoking it
//!    from `launchd` opens a terminal window. A headless entry point would mean a
//!    second signed binary inside the notarized bundle, changing what
//!    `crates/aterm-release` assembles and what `codesign`/`spctl` are asked to
//!    accept. Re-implementing the pipeline in a shell script instead would create
//!    a second, unverified download path — the precise shape of the build-826
//!    incident (`health`).
//! 3. **The lane lock is process-local.** `check_lane` is a `Mutex` inside one
//!    process. An agent checking while the app checks would serialize only on the
//!    coarser `stage_lock` flock, after both have already spent the network round
//!    trips.
//!
//! The gap worth closing is therefore the seamless APPLY, not the check cadence.
//! If a LaunchAgent is ever revisited, (2) is the precondition: a headless
//! `aterm update check` verb in the one binary, and a bundle assembly that signs
//! it. Do not ship a plist before that exists.
//!
//! # Trust model (tiered — works with NO Apple Developer ID, stronger with a key)
//!
//! Two gates ALWAYS hold, regardless of tier:
//! 1. **No downgrade** — the candidate's build number is strictly greater than the
//!    running [`build`](apply_staged_if_ready) number (and never below the persisted
//!    monotonic `min_build`/high-water floor — that blocks replay/rollback + yank).
//! 2. **Integrity** — the downloaded DMG's SHA-256 equals the manifest's.
//!
//! The **authenticity** gate is whichever of these is configured — the strongest
//! available wins, and it works by default with NONE of them:
//!
//! * **Tier REPO (a fork with no master of its own).** Trust is "it came from my channel
//!   repository over TLS", plus (2). The `.app` must still pass a *structural*
//!   `codesign --verify` (an ad-hoc signature suffices — arm64 requires one to run) so
//!   corruption/tamper is caught, but **no Apple anchor / Team ID / notarization is
//!   required**.
//! * **Tier ROSTER (this tree — Apple-free cryptographic authenticity).** The paper
//!   master is compiled in (`aterm_update_core::pins::PAPER_MASTER_PUBKEYS`, a committed
//!   constant — no env var), so every release MUST carry the master-signed machine
//!   roster and an Ed25519 appcast signature by a machine that roster names and has not
//!   revoked (`github::authorize_by_roster`). Since the manifest pins the sha256 of every
//!   downloadable container (DMG and zip), this authenticates the artifact whichever one
//!   is staged, even against a repo-write attacker — with no Apple Developer ID. Same
//!   roster `atpkg` verifies its index under.
//! * **Tier APPLE (a Developer ID — optional, additive).** If [`PINNED_TEAM_ID`] is
//!   set, `codesign --verify` also runs with a designated requirement (`-R`) pinning
//!   the Apple anchor + Developer-ID chain + Team ID (Gatekeeper-independent), plus
//!   `spctl -a -t exec` notarization.
//!
//! All *configured* anchors must pass (defense in depth). Everything shells out to
//! `codesign`/`spctl`/`hdiutil`/`ditto`/`curl`/`shasum`; the Ed25519 checks are
//! `aterm_update_core::roster`'s.

#[cfg(target_os = "macos")]
mod bundle;
#[cfg(target_os = "macos")]
mod cadence;
#[cfg(target_os = "macos")]
mod check_lane;
#[cfg(target_os = "macos")]
mod check_receipt;
/// The 2026-09-14 multi-process coordination audit's failing tests (checker gate
/// vs. the apply lane's ledger writes, a future-dated stamp, cross-build streak
/// expiry). Kept in their own file so the audit's laws read as one document.
#[cfg(all(test, target_os = "macos"))]
mod coordination_audit_tests;
#[cfg(target_os = "macos")]
mod github;
#[cfg(target_os = "macos")]
mod health;
#[cfg(target_os = "macos")]
mod install;
mod install_posture;
/// Signed single-executable Linux update transactions and explicit enrollment.
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
mod manifest;
#[cfg(target_os = "macos")]
mod paths;
mod progress;

/// Machine-readable compiled identity; no installation or updater state access.
pub fn binary_identity_json(
    identity: &aterm_update_core::linux::BinaryIdentity,
) -> Result<String, String> {
    aterm_json::to_string(identity).map_err(|error| error.to_string())
}
// Not macOS-only: every platform relaunches aterm (see the module's own doc).
mod relaunch;
#[cfg(target_os = "macos")]
mod status;
#[cfg(target_os = "macos")]
mod sys;
#[cfg(target_os = "macos")]
mod unreadable;
#[cfg(target_os = "macos")]
mod verify;
// Check-channel audit (2026-09-14): failing tests for the cross-process checker gate.
#[cfg(all(test, target_os = "macos"))]
mod check_channel_audit_tests;
// Not macOS-only: every platform names the copy of aterm it is running (S12 of
// `docs/DESIGN-which-copy-runs-2026-08-27.md`); only the other-copy probe is `.app`-shaped.
pub mod which_copy;

// The FailedMark writer/reader suppression projection (proof/test builds only),
// re-exported at the crate root so the `#[refines]` anchors in `manifest` can
// name it by the same `aterm_update::…` path convention as
// `status_reconciliation_projection` below.
#[cfg(all(target_os = "macos", any(test, feature = "spec-anchors")))]
pub use manifest::failed_mark_suppression_projection;

/// Hard cap for every small TOML ledger/marker consumed by the updater. The cap is
/// checked on the opened descriptor before allocation or parsing, then enforced again
/// with `take` so a concurrent grow cannot turn a local marker into unbounded work.
#[cfg(target_os = "macos")]
const MAX_LEDGER_BYTES: u64 = 256 * 1024;

#[cfg(target_os = "macos")]
fn read_ledger_text(path: &std::path::Path) -> Option<String> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;

    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > MAX_LEDGER_BYTES {
        return None;
    }
    let initial_capacity = usize::try_from(metadata.len()).ok()?;
    let mut bytes = Vec::with_capacity(initial_capacity);
    file.take(MAX_LEDGER_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    if u64::try_from(bytes.len()).ok()? > MAX_LEDGER_BYTES {
        return None;
    }
    String::from_utf8(bytes).ok()
}

/// The resolved GitHub release source plus the compiled-in default owner/repo, all
/// re-exported VERBATIM from [`aterm_update_core`]. These are pure re-exports, not
/// wrappers: the GUI calls `aterm_update::Source::resolve(cfg_owner, cfg_repo)` and
/// supplies the resulting `Source` through [`spawn_background_check_with_source`], so the type it
/// passes must be the very same `aterm_update_core::Source` the inherent `resolve`
/// (carrying aterm's compiled-in `alabsystems`/`aterm` channel defaults, and the
/// development-only `[update]` repoint) is defined on. A newtype here would break that
/// call site.
pub use aterm_update_core::{DEFAULT_OWNER, DEFAULT_REPO, Source};

/// Startup channel retained for callers of the legacy configured-source API.
/// The GUI's live checks use their current config snapshot and its background
/// checker samples [`SourceProvider`]; this fallback cannot override those.
static CONFIGURED_SOURCE: std::sync::OnceLock<Source> = std::sync::OnceLock::new();

/// Record the legacy startup source. First call wins; later calls are ignored.
/// Hosts needing config reloads use [`SourceProvider`] instead.
pub fn set_configured_source(source: Source) {
    let _ = CONFIGURED_SOURCE.set(source);
}

/// The recorded startup source, or the compiled default when unset.
/// This compatibility API does not query a running GUI's current config.
#[must_use]
pub fn configured_source() -> Source {
    CONFIGURED_SOURCE
        .get()
        .cloned()
        .unwrap_or_else(|| Source::resolve(None, None))
}

#[cfg(test)]
mod configured_source_tests {
    use super::{Source, configured_source, set_configured_source};

    /// The compatibility startup source remains first-wins: unset, the
    /// compiled default; set, the caller's initial source. The GUI's live
    /// config provider is independent of this retained API.
    /// (The only test in this binary that touches the process-global.)
    #[test]
    fn the_configured_source_is_the_loops_and_the_first_setting_stands() {
        assert_eq!(configured_source(), Source::resolve(None, None));
        let private = Source {
            owner: "private-org".to_string(),
            repo: "aterm-fork".to_string(),
        };
        set_configured_source(private.clone());
        assert_eq!(configured_source(), private);
        set_configured_source(Source::resolve(None, None));
        assert_eq!(
            configured_source(),
            private,
            "a later setting does not move the channel under a running loop"
        );
    }
}
pub use progress::{Progress, ProgressNotify, set_progress_observer};

/// Re-exported for every OTHER lane that re-launches aterm forwarding its own
/// argv (the GUI's cold-exec/seamless/Windows successor spawns): strip the
/// leading `--window` mode pins so no relaunch path can re-grow the argv the
/// boot swap deliberately pins exactly once. See the function's own doc for
/// the accumulation this closes.
pub use relaunch::reexec_forwarded_args;

/// The Apple Developer **Team ID** for the OPTIONAL Tier APPLE anchor, read from the
/// committed constant `aterm_update_core::pins::APPLE_TEAM_ID` — NOT from any build
/// environment variable (the old `ATERM_EXPECTED_TEAM_ID` input was retired when the
/// anchors became constants; see that module's header). Empty (the default) does **not** disable
/// the updater — it just skips the codesign/notarization anchor, leaving the
/// repo-trust / signed-manifest tiers (see the crate-level trust model). Set it (the
/// owner's Developer-ID build) to additionally require the swapped bundle be
/// Developer-ID signed by this team.
pub const PINNED_TEAM_ID: &str = aterm_update_core::pins::APPLE_TEAM_ID;

/// Runtime RAISE of the Tier-APPLE anchor, from `[update] require_team_id` in the
/// GUI config. Set once, early in `main`, by [`set_required_team_id`].
static REQUIRED_TEAM_ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Opt IN to Developer-ID + notarization enforcement at RUNTIME, without a rebuild.
///
/// # Why this exists, and why it can only tighten
///
/// Shipped aterm pins the real Team ID (Tier APPLE armed 2026-08-15), so
/// [`PINNED_TEAM_ID`] is non-empty and this call is a no-op there. It exists for
/// FORKS and self-hosted builds compiled with an empty pin: their default is the
/// structural `codesign --verify` only (what makes an unsigned build updatable at
/// all), and this lets such a deployment opt into Developer-ID enforcement.
///
/// The gap it left was that the STRICTER posture was only reachable by rebuilding
/// from a source edit to `pins::APPLE_TEAM_ID`. Once there is a Developer ID to require,
/// requiring it should be a setting, not a compile.
///
/// This is deliberately ONE-WAY: it can install a team requirement where there was
/// none, and it can never remove or replace one that was compiled in. A config file
/// (or anything that can write one) must not be able to downgrade a shipped build's
/// trust anchor — that would turn a settings file into a verification bypass, which
/// is the opposite of what a "protection setting" is for. Concretely:
///
/// * compiled pin non-empty ⇒ the compiled pin always wins, this call is ignored;
/// * compiled pin empty + `Some(team)` ⇒ that team is now required;
/// * compiled pin empty + `None`/blank ⇒ unchanged (structural-only, the default).
///
/// Idempotent-by-`OnceLock`: only the first call takes effect, so a later config
/// reload cannot loosen the anchor mid-session either.
pub fn set_required_team_id(team: Option<&str>) {
    if !PINNED_TEAM_ID.is_empty() {
        return;
    }
    let Some(team) = team.map(str::trim).filter(|t| !t.is_empty()) else {
        return;
    };
    let _ = REQUIRED_TEAM_ID.set(team.to_string());
}

/// The Team ID verification must actually satisfy: the compiled-in pin when there
/// is one, otherwise whatever [`set_required_team_id`] installed, otherwise empty
/// (structural-only Tier REPO — the shipped default).
///
/// Every bundle-verification call site reads this rather than [`PINNED_TEAM_ID`]
/// directly, so the runtime opt-in cannot be accidentally bypassed by a call site
/// that forgot about it.
#[must_use]
pub fn effective_team_id() -> &'static str {
    if !PINNED_TEAM_ID.is_empty() {
        return PINNED_TEAM_ID;
    }
    REQUIRED_TEAM_ID.get().map_or("", String::as_str)
}

/// SHA-256 of a raw 32-byte Ed25519 key, for shipping introspection.
///
/// `Ok(None)` is the explicit no-pin state. Invalid/non-32-byte pins are errors,
/// never silently reported as absent. Hashing the decoded key (the exact bytes
/// consumed by signature verification) avoids brittle searches for a base64
/// literal that an optimizer may transform or eliminate.
pub fn update_pubkey_sha256(encoded: &str) -> Result<Option<String>, String> {
    use aterm_digest::Sha256;

    if encoded.is_empty() {
        return Ok(None);
    }
    let raw = aterm_codec::base64::decode_strict(encoded.as_bytes())
        .map_err(|_| "compiled update public key is not standard base64".to_string())?;
    if raw.len() != 32 {
        return Err(format!(
            "compiled update public key decodes to {} bytes, not 32",
            raw.len()
        ));
    }
    Ok(Some(
        Sha256::digest(&raw)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    ))
}

/// THE UPDATE PIN: the fingerprint of the one anchor that authorizes a release —
/// SHA-256 of the raw 32 bytes of `pins::PAPER_MASTER_PUBKEYS[0]` — as 64 lowercase
/// hex, `empty` when the roster tier is unarmed (a fork), or `invalid` for a malformed
/// pin. `aterm-gui`'s `build.rs` embeds the same value in the Mach-O `__aterm_upin`
/// record, and the release cutter proves every shipped slice carries it. After a master
/// rotation this is what tells a stranded client from a healthy one.
#[must_use]
pub fn compiled_update_pin_sha256() -> String {
    fingerprint_state(
        aterm_update_core::pins::PAPER_MASTER_PUBKEYS
            .first()
            .copied()
            .unwrap_or(""),
    )
}

fn fingerprint_state(encoded: &str) -> String {
    match update_pubkey_sha256(encoded) {
        Ok(Some(fingerprint)) => fingerprint,
        Ok(None) => "empty".to_string(),
        Err(_) => "invalid".to_string(),
    }
}

/// Outcome of an [`apply_staged_if_ready`] call. Every variant is non-fatal: the
/// caller continues launching the current build (the one variant that *would*
/// replace it, [`ApplyOutcome::ReExecFailed`], only happens after a swap that was
/// rolled back). On a successful apply the function never returns — it `exec`s the
/// new binary — so there is no "applied" variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// Not an installed `.app` launch (dev build / `cargo run` / translocated /
    /// run from a mounted DMG), or the updater is disabled. No-op.
    NotApplicable,
    /// Nothing staged, or what's staged is not strictly newer. No-op.
    NoUpdate,
    /// A newer build is staged but could not be applied right now (e.g. the
    /// install location is not writable, or apply-time re-verification failed).
    /// The staged build is left in place for a future attempt; carries a reason.
    Deferred(String),
    /// The bundle was swapped but re-exec into the new binary failed; the swap
    /// was rolled back and the caller should keep running the old build.
    ReExecFailed(String),
}

/// Whether the updater runs on this platform at all: macOS or Linux. Every lane a PERSON starts
/// reads this and nothing more — `aterm update check` / Settings ▸ Software Update's
/// Check for Updates ([`check_now`]), Update Now and `aterm ctl update apply` (the
/// window's apply lane), and the launch-time swap of a build already staged. It no
/// longer requires a pinned anchor — the default Tier REPO works with none (see the
/// crate-level trust model), so an internal build with no Apple Developer ID and no
/// signing key still self-updates. Dev-build inertness comes from [`bundle::resolve`],
/// not from a missing pin, and has two sources there: the LAYOUT (a `cargo run` /
/// `target/` binary is not an installed `.app`) and the codesign-sealed
/// [`bundle::DEV_BUILD_KEY`] mark (a local build installed in place, which layout alone
/// cannot distinguish from a release). Linux instead requires explicit enrollment
/// of its safe executable path and always authenticates signed native artifacts;
/// merely running a checkout does not enroll it. Other platforms are unsupported.
///
/// NOT the setting. "Check for updates automatically" (`[update] enabled`) is
/// [`automatic`]'s, and it switches off the BACKGROUND CHECKER alone (2026-09-23 review):
/// the row says "automatically", so turning it off must leave the checks and the apply a
/// person asks for working — the Sparkle convention every Mac user knows. Until then it
/// fed this function and refused a typed `aterm update check` and a clicked Update Now
/// as "disabled on this build", so the only way to get current with the switch off was
/// to turn it on and relaunch.
#[must_use]
pub fn enabled() -> bool {
    cfg!(any(target_os = "macos", target_os = "linux"))
}

/// "Check for updates automatically" — `[update] enabled`, Settings ▸ Terminal ▸ Updates,
/// read once per process by [`aterm_update_core::settings::update_enabled`] — on a
/// platform the updater runs on ([`enabled`]). It gates the one lane nobody asked for:
/// the background checker ([`spawn_background_check_with_source`]) in the window and in
/// every terminal session. It replaces `ATERM_NO_AUTO_UPDATE` (2026-09-23): an
/// environment variable reached only what one shell launched, never the window a Dock
/// click starts, and the owner's rule is Settings, not env. Applying a staged build is
/// `[update] auto_apply`'s question, not this one's.
#[must_use]
pub fn automatic() -> bool {
    enabled() && aterm_update_core::settings::update_enabled()
}

/// Apply a staged update if one is ready and strictly newer than `current_build`
/// (the running build number = the version's timestamp patch). On success this
/// **does not return** — it re-execs the freshly swapped-in binary. See the
/// module docs for the full ordered sequence and the crate-level trust model.
#[cfg(target_os = "macos")]
#[must_use]
pub fn apply_staged_if_ready(current_build: u64) -> ApplyOutcome {
    install::apply_staged_if_ready(current_build, None, &[], &[], false)
}

/// GUI handoff variant: inherited PTY/proof descriptors stay CLOEXEC for every
/// verification helper and are exposed only to the updater's final exec image.
/// `handoff_env` rides the same contract: authority variables the caller's
/// prearm consumed out of the ambient environment (so no helper can see them),
/// restored exclusively onto the final exec image — the re-exec'd successor
/// must re-validate the inherited handoff and cannot without them.
#[cfg(target_os = "macos")]
#[must_use]
pub fn apply_staged_if_ready_preserving_fds(
    current_build: u64,
    handoff_fds: &[i32],
    handoff_env: &[(std::ffi::OsString, std::ffi::OsString)],
) -> ApplyOutcome {
    install::apply_staged_if_ready(current_build, None, handoff_fds, handoff_env, false)
}

/// Exact-identity GUI handoff variant. In addition to preserving descriptors,
/// this binds the canonical OLD rollback source and health trial to the compiled
/// git commit of the running binary, preventing same-build/different-source reuse.
#[cfg(target_os = "macos")]
#[must_use]
pub fn apply_staged_if_ready_preserving_fds_exact(
    current_build: u64,
    current_commit: &str,
    handoff_fds: &[i32],
    handoff_env: &[(std::ffi::OsString, std::ffi::OsString)],
    handoff_target_is_this_build: bool,
) -> ApplyOutcome {
    install::apply_staged_if_ready(
        current_build,
        Some(current_commit),
        handoff_fds,
        handoff_env,
        handoff_target_is_this_build,
    )
}

/// The one law for "this refusal is a person's to clear": the INSTALLED bundle
/// cannot be the swap's rollback source (`install::rollback_source_refusal`), so
/// no lane can apply anything until the bundle is changed (2026-09-14).
#[cfg(target_os = "macos")]
pub fn refusal_needs_person(reason: &str) -> bool {
    install::is_rollback_source_refusal(reason)
        || reason.contains("does not run from an installed bundle")
}

#[cfg(not(target_os = "macos"))]
pub fn refusal_needs_person(_reason: &str) -> bool {
    false
}

/// Overlap-handoff PRE-PARK verification: authenticate the staged candidate
/// (codesign policy + sealed build/commit rebinding, bound to the authorized
/// artifact identity) AND prove the bundle it would replace can become the
/// swap's rollback source — both while the calling process's PTY readers are
/// all still live. The handoff child re-runs the complete gate at swap time —
/// this call only moves the FIRST verdict out of the activity-sensitive parked
/// window so a doomed candidate never parks a reader. See
/// `install::preverify_staged_handoff_candidate` for the exact obligations, and
/// `install::preverify_installed_rollback_source` for why the second half is
/// not optional.
#[cfg(target_os = "macos")]
pub fn preverify_staged_for_handoff(
    current_build: u64,
    current_commit: Option<&str>,
    expected_build: Option<u64>,
    expected_commit: Option<&str>,
) -> Result<(), String> {
    install::preverify_staged_handoff_candidate(
        current_build,
        current_commit,
        expected_build,
        expected_commit,
    )
}

/// Non-macOS: there is no `.app` bundle, so there is nothing to pre-verify and
/// nothing this could refuse. The only overlap lane reachable off macOS is the
/// same-binary `ATERM_DEBUG_SEAMLESS_REEXEC` QA path, which skips pre-verify.
#[cfg(not(target_os = "macos"))]
pub fn preverify_staged_for_handoff(
    _current_build: u64,
    _current_commit: Option<&str>,
    _expected_build: Option<u64>,
    _expected_commit: Option<&str>,
) -> Result<(), String> {
    Ok(())
}

/// Record that a staged build FAILED to become the running build, so the failure
/// is durable and visible to `aterm-ctl update status` instead of living only in
/// the GUI's in-memory `auto_apply_manual_only` latch and a log line.
///
/// The apply lane is the GUI's (the handoff protocol needs the event loop, the
/// session registry and the PTY readers), but the LEDGER is this crate's, so the
/// GUI reports outcomes here rather than reaching into `Updates/` itself.
///
/// `reason` should be the typed outcome — `ChildDied`, `AdoptionMismatch`,
/// `ActivityRevoked`, `TimedOut`, `PreparationFailed`, `re-exec failed` — because
/// which one it is decides whether the answer is "your machine was too busy" or
/// "these two builds cannot hand off to each other".
///
/// `target_build` names the artifact the attempt was trying to reach; see
/// [`health::Health::apply_failures_for_target`].
///
/// RETURNS WHAT IT JUST WROTE, and callers with a surface are expected to use it.
/// The GUI's ledger write happens on the event loop, but the facts that carry the
/// count and the reason BACK to the window are gathered by a worker that has
/// already run — so the first failed apply was durably recorded and then not
/// mentioned by the menu, the palette or the update card until some later,
/// unrelated reconcile happened to land. The first failure is precisely the one a
/// person is entitled to hear about, so the writer hands the numbers straight back
/// rather than making the reader wait for a re-read.
///
/// `None` means nothing was recorded (no staging root); it must NOT be read as
/// "zero failures", or a surface would erase a streak it merely failed to observe.
///
/// Best-effort by construction: observability must never be able to block or fail
/// an update.
#[cfg(target_os = "macos")]
pub fn record_apply_failure(
    current_build: u64,
    target_build: u64,
    reason: &str,
) -> Option<RecordedApplyFailure> {
    let staging = paths::Staging::resolve()?;
    let ledger = health::Health::record_apply_failure(
        &staging.health(),
        current_build,
        target_build,
        reason,
    );
    status::record(
        &staging,
        current_build,
        &format!("staged build did not apply: {reason}"),
    );
    let persistent = ledger.is_persistent();
    Some(RecordedApplyFailure {
        apply_failures: ledger.apply_failures,
        failures_for_target: ledger.apply_failures_for_target,
        target_build: ledger.last_apply_failure_target_build,
        reason: ledger.last_apply_error,
        persistent,
    })
}

/// Exactly what one [`record_apply_failure`] left in the ledger, read back from
/// inside the same lock scope that wrote it.
///
/// Taken under the write lock on purpose: a second read to fetch these numbers
/// could interleave with `expire_stale_apply_streak` (which runs on the check
/// lane's own cadence) and report a streak that no longer matches the one just
/// recorded.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecordedApplyFailure {
    /// The ESCALATION streak after this write (`UpdateStatus::failing_applies`).
    pub apply_failures: u32,
    /// Consecutive failures of [`Self::target_build`] after this write.
    pub failures_for_target: u32,
    /// The artifact this failure was about (0 when the caller did not know one).
    pub target_build: u64,
    /// The stored reason, already truncated exactly as the ledger stores it.
    pub reason: String,
    /// Whether the ledger now reads as persistently failing — the loud verdict, so
    /// a surface can escalate at the write instead of at the next check.
    pub persistent: bool,
}

/// Record that an apply SUCCEEDED — the staged build is the running build now.
/// Clears the apply streak only; acquisition streaks are the check lane's.
#[cfg(target_os = "macos")]
pub fn record_apply_success(_current_build: u64) {
    let Some(staging) = paths::Staging::resolve() else {
        return;
    };
    health::Health::record_apply_success(&staging.health());
}

/// Record that an apply was REFUSED — blocked, deferred, or held — rather than
/// attempted and failed.
///
/// This exists because the opposite policy produced a silent updater. Refusals
/// are correctly excluded from the failure streaks (a busy terminal must not
/// manufacture a persistent-failure escalation), and that meant they were
/// recorded nowhere at all: on the machine this was written for, `update apply`
/// answered "OK apply requested", the request was refused, and `update status`
/// kept reporting a healthy updater with a build "staged … applies on next
/// launch" for hours. A refusal nobody can observe reads exactly like an updater
/// that is not running.
///
/// So this writes BOTH surfaces an operator actually looks at: the reason lands
/// in the health ledger ([`apply_lane_report`], the control socket's
/// `apply_refusal=`), and the status outcome stops advertising a stage as though
/// nothing had tried to apply it.
///
/// `reason` must name what refused and why — it is the entire answer to "the
/// build is staged, so why is it not running?".
///
/// Best-effort, and REQUEST-RATE only: this takes the ledger lock and writes two
/// small files, so it belongs on explicit apply requests and terminal verdicts,
/// never on a per-frame or per-poll path.
#[cfg(target_os = "macos")]
pub fn record_apply_refusal(current_build: u64, reason: &str) {
    let Some(staging) = paths::Staging::resolve() else {
        return;
    };
    health::Health::record_apply_refusal(&staging.health(), current_build, reason);
    // Name the artifact when one is genuinely publishable, so the line answers
    // "which build, and why is it not running" in one read. `status::record`
    // re-derives `staged_build` from the same publishable marker — two
    // independent reads of the same source, so they agree unless the marker
    // changes BETWEEN them. That window is real but harmless: a marker that
    // moved mid-refusal means a newer stage just landed, and the next status
    // write re-derives both halves from it.
    let outcome = match manifest::Ready::read_publishable(&staging) {
        Some(ready) => format!(
            "staged {} (build {}) — NOT applied: {reason}",
            ready.version, ready.build_number
        ),
        None => format!("apply refused: {reason}"),
    };
    status::record(&staging, current_build, &outcome);
}

/// Non-macOS: no apply lane exists, so there is nothing to record.
#[cfg(not(target_os = "macos"))]
pub fn record_apply_failure(
    _current_build: u64,
    _target_build: u64,
    _reason: &str,
) -> Option<RecordedApplyFailure> {
    None
}

/// Non-macOS counterpart to [`record_apply_success`].
#[cfg(not(target_os = "macos"))]
pub fn record_apply_success(_current_build: u64) {}

/// Non-macOS counterpart to [`record_apply_refusal`].
#[cfg(not(target_os = "macos"))]
pub fn record_apply_refusal(_current_build: u64, _reason: &str) {}

/// The apply lane's own answer to "a build is staged — why is it not running?".
///
/// Kept OUT of [`UpdateStatus`] on purpose: that value is the CHECK lane's
/// projection, every consumer builds it field-by-field, and the two lanes fail
/// independently. Consumers that need apply-lane detail (today: the control
/// socket's `update` reply) read this alongside it.
///
/// The ESCALATION streak is deliberately absent: `UpdateStatus::failing_applies`
/// already carries it, and two reads of one ledger racing each other could disagree
/// about a number that has exactly one source of truth. This carries what nothing
/// else reports — the REASONS, and the artifact those reasons are about.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ApplyLaneReport {
    /// Reason of the most recent apply failure; empty when none is standing.
    pub last_failure: String,
    /// The build [`Self::last_failure`] was trying to reach (0 when unknown), and
    /// [`Self::failures_for_target`] the number of consecutive attempts on THAT
    /// artifact. Read together they answer the only question a surface offering a
    /// staged build can act on: has THIS one been tried, and how often?
    ///
    /// `failing_applies` cannot answer it. That streak is expiry-bound to the
    /// RUNNING build, so a newer download inherits the count and the reason of the
    /// artifact it replaced.
    pub last_failure_target_build: u64,
    /// See [`Self::last_failure_target_build`].
    pub failures_for_target: u32,
    /// Reason of the most recent apply REFUSAL — a verdict that stopped an apply
    /// before it could fail. Empty once an apply succeeds or hard-fails.
    pub last_refusal: String,
    /// RFC3339 UTC of [`Self::last_refusal`]; empty when there is none.
    pub last_refusal_at: String,
}

/// Read the apply lane's durable record as it applies to `current_build`.
///
/// A refusal recorded by a build that is no longer running is dropped here: a
/// successful in-session apply execs away and never returns to clear the slot, so
/// the running build is the only honest expiry. `None` only when there is no
/// staging root to read.
#[cfg(target_os = "macos")]
#[must_use]
pub fn apply_lane_report(current_build: u64) -> Option<ApplyLaneReport> {
    let staging = paths::Staging::resolve()?;
    let ledger = health::Health::read(&staging.health());
    let standing = ledger.apply_refusal_applies_to(current_build);
    Some(ApplyLaneReport {
        last_failure: ledger.last_apply_error,
        last_failure_target_build: ledger.last_apply_failure_target_build,
        failures_for_target: ledger.apply_failures_for_target,
        last_refusal: if standing {
            ledger.last_apply_refusal
        } else {
            String::new()
        },
        last_refusal_at: if standing {
            ledger.last_apply_refusal_at
        } else {
            String::new()
        },
    })
}

/// Non-macOS: there is no apply lane, so there is nothing to report.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn apply_lane_report(_current_build: u64) -> Option<ApplyLaneReport> {
    None
}

/// Non-macOS no-op: there is no `.app` bundle to swap.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn apply_staged_if_ready(_current_build: u64) -> ApplyOutcome {
    ApplyOutcome::NotApplicable
}

#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn apply_staged_if_ready_preserving_fds(
    _current_build: u64,
    _handoff_fds: &[i32],
    _handoff_env: &[(std::ffi::OsString, std::ffi::OsString)],
) -> ApplyOutcome {
    ApplyOutcome::NotApplicable
}

#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn apply_staged_if_ready_preserving_fds_exact(
    _current_build: u64,
    _current_commit: &str,
    _handoff_fds: &[i32],
    _handoff_env: &[(std::ffi::OsString, std::ffi::OsString)],
    _handoff_target_is_this_build: bool,
) -> ApplyOutcome {
    #[cfg(target_os = "linux")]
    {
        linux::boot(_current_build, _current_commit)
    }
    #[cfg(not(target_os = "linux"))]
    {
        ApplyOutcome::NotApplicable
    }
}

/// Confirm the running build reached a healthy checkpoint (window up / first
/// frame): clears the boot-health sentinel and GCs the retained rollback bundle,
/// binding both to the compiled git commit of the running binary. Call **once,
/// from the GUI, after deep init** so a crash BEFORE this point is caught and
/// auto-reverted by [`apply_staged_if_ready`]'s boot-health check on the next
/// launch(es), while a crash AFTER it is a normal fault the updater ignores.
/// Idempotent, best-effort, and a no-op when the last launch was not a self-update.
#[cfg(target_os = "macos")]
#[must_use]
pub fn confirm_boot_health_exact(current_build: u64, current_commit: &str) -> bool {
    install::confirm_boot_health(current_build, Some(current_commit))
}

/// Linux confirms its exact inode trial; other platforms have nothing to confirm.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn confirm_boot_health_exact(_current_build: u64, _current_commit: &str) -> bool {
    #[cfg(target_os = "linux")]
    {
        linux::confirm(_current_build, _current_commit)
    }
    #[cfg(not(target_os = "linux"))]
    {
        true
    }
}

/// Sealed installed-bundle identity plus the durable exact-artifact receipt written
/// by the most recent successful swap. A returned apply may claim "installed" only
/// when all four values bind to its ticket; build number alone is insufficient.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledUpdateFacts {
    pub build_number: u64,
    pub git_commit: String,
    /// The bundle's marketing version (`CFBundleShortVersionString`), display only —
    /// what the update screen names when the installed bundle is newer than the running
    /// process and is about to be ACTIVATED in place. `None` when the plist lacks it.
    pub version: Option<String>,
    pub receipt_build_number: Option<u64>,
    pub receipt_dmg_sha256: Option<String>,
    /// The installed build sits BELOW the operator apply floor (`Floor::min_build`,
    /// a yank). Still reported — the bundle is what it is — but an activation of
    /// it is refused at every gate, and the GUI does not stage it as one.
    pub yanked: bool,
}

/// Collect installed provenance and the trial marker. This may spawn PlistBuddy and
/// read a ledger, so GUI callers must invoke it only on their updater facts worker.
#[cfg(target_os = "macos")]
#[must_use]
pub fn installed_update_facts() -> Option<InstalledUpdateFacts> {
    // Pure observation — a dev-marked build's provenance is no less true, so report
    // it rather than blanking the panel. See `bundle::resolve_layout`.
    let installed = bundle::resolve_layout()?;
    // Info.plist values are only codesign-sealed evidence after the complete
    // configured policy gate succeeds. This runs on the updater facts worker,
    // never the event loop, so fail-closed verification adds no input latency.
    verify::verify_bundle_policy(&installed.app_root, effective_team_id()).ok()?;
    let build_number = verify::bundle_build_number(&installed.app_root).ok()?;
    let git_commit = verify::bundle_git_commit(&installed.app_root).ok()?;
    let version = verify::bundle_short_version(&installed.app_root).ok();
    let staging = paths::Staging::resolve();
    let receipt = staging
        .as_ref()
        .and_then(|staging| manifest::InstalledReceipt::read(&staging.installed_receipt()))
        .filter(|receipt| receipt.matches_sealed(build_number, &git_commit));
    let yanked = staging
        .as_ref()
        .is_some_and(|staging| build_number < manifest::Floor::read(&staging.floor()).min_build);
    Some(InstalledUpdateFacts {
        build_number,
        git_commit,
        version,
        receipt_build_number: receipt.as_ref().map(|receipt| receipt.build_number),
        receipt_dmg_sha256: receipt.map(|receipt| receipt.dmg_sha256),
        yanked,
    })
}

/// THE ACTIVATION PRE-VERIFY (seamless seam 1 for an INSTALLED bundle): before the
/// GUI parks a single reader to hand off to a NEWER build that is already at its own
/// bundle path (installed by another producer — the release cutter writing into the
/// bundle it launched from, a user dragging a new `.app` over the old one, a sibling
/// aterm process that swapped it), prove that bundle is exactly what the reducer
/// authorized: a non-symlink directory that passes the complete configured codesign
/// policy, whose SEALED build and commit equal `expected_build` / `expected_commit`,
/// and whose build is strictly newer than the running one. Anything else refuses, so
/// a bundle swapped again between the observation and the handoff is caught before
/// the terminal is touched. Runs on the handoff worker (codesign is not free), never
/// the event loop.
#[cfg(target_os = "macos")]
pub fn preverify_installed_for_handoff(
    current_build: u64,
    expected_build: u64,
    expected_commit: &str,
) -> Result<(), String> {
    let installed = bundle::resolve_layout()
        .ok_or_else(|| "no installed bundle at this executable's path".to_string())?;
    let (build, commit) = install::verified_bundle_identity_at(&installed.app_root)?;
    // The operator apply floor (a yank) gates an ACTIVATION exactly as it gates a
    // staged swap (`install.rs`): a yanked build found under our own path is still a
    // yanked build (2026-08-19 review).
    if let Some(staging) = paths::Staging::resolve() {
        let floor = manifest::Floor::read(&staging.floor());
        if build < floor.min_build {
            return Err(format!(
                "installed bundle build {build} is below the operator apply floor {} (yanked); \
                 not activating it",
                floor.min_build
            ));
        }
    }
    if build != expected_build {
        return Err(format!(
            "installed bundle is build {build}, not the authorized build {expected_build}"
        ));
    }
    if !commit_matches(&commit, expected_commit) {
        return Err(format!(
            "installed bundle commit {commit} is not the authorized commit {expected_commit}"
        ));
    }
    if build <= current_build {
        return Err(format!(
            "installed bundle build {build} is not newer than the running build {current_build}"
        ));
    }
    Ok(())
}

/// The OUTGOING process killed an overlap-handoff candidate of `target_build` for a
/// reason of its own (readiness deadline, user activity, proof mismatch, a session
/// closing) — NOT because the candidate died. That candidate observed a trial launch
/// at boot exactly as a crash would have; give it back, so a busy machine's bounded
/// automatic re-attempts cannot walk a healthy build to `MAX_BOOT_ATTEMPTS`, revert
/// it and poison it. Runs on the handoff worker after the reap; the apply lock
/// serializes it against a concurrent swap/confirm exactly like `check_boot_health`.
/// Best-effort: nothing here can fail an apply, and a sentinel for any other build
/// (or none) is untouched.
#[cfg(target_os = "macos")]
pub fn forgive_trial_launch(target_build: u64) {
    install::forgive_trial_launch(target_build);
}

/// THIS PROCESS IS A HANDOFF CANDIDATE THAT HAS NOT TAKEN OVER YET. Set by the GUI
/// at boot when the launch carries an overlap handoff, cleared when the outgoing
/// process commits it (or never, if the candidate is rejected and exits).
///
/// It gates exactly one thing: [`health::Health::expire_stale_apply_streak`]. A
/// candidate spawns its own background check within milliseconds of booting, and
/// that check was expiring the apply streak its OWN attempt was about to add to —
/// so `failing_applies` could never pass 1 and an update that downloads and
/// verifies but will not start never reached the persistent verdict (2026-08-19
/// round-4 audit). Scoped to the candidate rather than to "has reached a health
/// checkpoint" so an ordinary launch — including a headless one, a `--version`
/// run, or a session that never presents a frame — still heals a stale streak on
/// its first check, exactly as before.
static UNCOMMITTED_CANDIDATE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Mark this process an uncommitted handoff candidate (`true` at boot when the
/// launch carries a handoff; `false` once it is committed).
pub fn set_uncommitted_handoff_candidate(uncommitted: bool) {
    UNCOMMITTED_CANDIDATE.store(uncommitted, std::sync::atomic::Ordering::SeqCst);
}

/// Whether this process is a handoff candidate that has not been committed.
#[must_use]
pub fn is_uncommitted_handoff_candidate() -> bool {
    UNCOMMITTED_CANDIDATE.load(std::sync::atomic::Ordering::SeqCst)
}

/// Block the calling background thread while this process is an UNCOMMITTED
/// handoff candidate, up to `bound`; `true` when it waited at all. A successor
/// that has not been committed may still be rejected by the outgoing process
/// (2026-09-19): nothing it does before Commit may reach the network, the store
/// or the ledger — no update check, no toolchain pass — because those would be
/// the acts of a process that is about to be killed, beside a parent that is
/// still the owner. Polled, not signalled: the flag is cleared on the main thread
/// at `Wake::ActivateCommittedHandoff`, and a 50 ms cadence is far inside any
/// hold. The bound outlasts the late park's grant budget so a candidate the
/// parent never answered (it stands down by EOF and exits) can never spin.
pub fn wait_while_uncommitted_handoff_candidate(bound: std::time::Duration) -> bool {
    let started = std::time::Instant::now();
    let mut waited = false;
    while is_uncommitted_handoff_candidate() && started.elapsed() < bound {
        waited = true;
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    waited
}

/// The bound on [`wait_while_uncommitted_handoff_candidate`] for the two
/// background lanes a successor starts at boot: longer than the successor's own
/// grant-hold budget, so the lanes start only after Commit or after the
/// candidate has already been told to exit.
pub const UNCOMMITTED_CANDIDATE_HOLD_BOUND: std::time::Duration =
    std::time::Duration::from_secs(10 * 60);

/// Consecutive unconfirmed launches after which a freshly-swapped build is
/// auto-reverted to its predecessor and marked failed.
///
/// Exposed because a caller that DECLINES to forgive a counted trial launch is
/// spending this budget, and the two have to be reasoned about together: a retry
/// schedule longer than this walks a healthy build into the revert. `aterm-gui`'s
/// handoff lane compile-time-asserts its structural budget against it.
#[cfg(target_os = "macos")]
pub const MAX_BOOT_ATTEMPTS: u32 = install::MAX_BOOT_ATTEMPTS;

/// Launches the boot sentinel has counted for `build` right now — the snapshot a
/// parent takes before launching a candidate.
#[cfg(target_os = "macos")]
#[must_use]
pub fn trial_launch_count(build: u64) -> u32 {
    install::trial_launch_count(build)
}

/// Non-macOS: no sentinel, nothing counted.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn trial_launch_count(_build: u64) -> u32 {
    0
}

/// [`forgive_trial_launch`], but ONLY if this candidate actually observed a launch:
/// the counter must have MOVED since `before` and stand above zero. A candidate
/// killed in its first milliseconds — before `check_boot_health` runs — counted
/// nothing, and forgiving then erases a launch some EARLIER, genuinely crashed
/// candidate observed, which is the crash signal the sentinel exists to keep
/// (2026-08-19 round-4 audit).
///
/// "MOVED", not "advanced": a candidate that swaps re-arms the trial
/// (`prepare_trial` resets the count to 0) and then counts 1, so a stale-high
/// snapshot from an earlier trial of the same build would suppress a legitimate
/// forgive if this compared only `>` (round-4 skeptics).
#[cfg(target_os = "macos")]
pub fn forgive_trial_launch_if_advanced(target_build: u64, before: u32) {
    let now = install::trial_launch_count(target_build);
    if now > 0 && now != before {
        install::forgive_trial_launch(target_build);
    }
}

/// Non-macOS: nothing to forgive.
#[cfg(not(target_os = "macos"))]
pub fn forgive_trial_launch_if_advanced(_target_build: u64, _before: u32) {}

/// Non-macOS: no boot sentinel is ever armed by a swap.
#[cfg(not(target_os = "macos"))]
pub fn forgive_trial_launch(_target_build: u64) {}

/// Non-macOS: there is no `.app` bundle to activate.
#[cfg(not(target_os = "macos"))]
pub fn preverify_installed_for_handoff(
    _current_build: u64,
    _expected_build: u64,
    _expected_commit: &str,
) -> Result<(), String> {
    Err("installed-bundle activation is macOS-only".to_string())
}

/// Non-macOS has neither an installed bundle nor a self-update receipt.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn installed_update_facts() -> Option<InstalledUpdateFacts> {
    None
}

/// A GUI-supplied hook the background check uses to SURFACE self-healing events to
/// the user (`(title, body)` — e.g. posted to the event loop and shown as an OS
/// notification). Health problems must not stay buried in `status.toml`: the
/// build-826 incident was a persistently-broken updater that nothing surfaced.
///
/// The hook also carries the RECOVERY: once this process has reported a persistent
/// failure and a later check finds the ledger healed, it is called once more with
/// [`HEALTH_RECOVERED_TITLE`] and, as the body, the CLASS this process had
/// announced (`"apply"`, `"manifest"`, …), so a surface still showing the failure
/// (the update row, Settings' headline) knows to clear it — and knows what the
/// healing proves. The ledger's heal is proof only for the class it counted: a
/// healed download streak says nothing about an install warning another source
/// raised (the GUI's convergence notice, the overdue notice), and reading it as
/// proof put "aterm updates work again" on record over a build still stranded.
/// No notification is owed for it — the warning leaving is the whole message.
pub type HealthNotify = Box<dyn Fn(String, String) + Send>;

/// The `title` [`HealthNotify`] is called with when a persistent failure this
/// process reported has HEALED. Pinned so the GUI matches on it rather than on the
/// shape of the body.
pub const HEALTH_RECOVERED_TITLE: &str = "aterm auto-update recovered";

/// A GUI-supplied hook fired when a strictly-newer build has just been STAGED, so the
/// GUI can arm the in-session apply lane and show the update-ready nudge (the Version
/// menu ⬆️ / tab-strip ↻; RFC Rung 2). `(build, version)`.
pub type StagedNotify = Box<dyn Fn(u64, String) + Send>;

/// One process-wide network/staging lane shared by the periodic scheduler and
/// every user-triggered check. The GUI reducer adds generation semantics above
/// this seam; the mutex ensures older callers cannot create a second concurrent
/// download/verify transaction underneath it.
#[cfg(target_os = "macos")]
fn check_lane() -> &'static check_lane::Lane {
    static LANE: check_lane::Lane = check_lane::Lane::new();
    &LANE
}

/// Whether the persistent-failure notice should be spoken for `class`, given the class
/// this process has already announced (`None` = none yet).
///
/// The latch behind this used to be a bare `bool`, which made the answer "no" for every
/// class after the first: one notice per process, whatever else broke afterwards. That
/// is the wrong dedup key, because each class asks the user for a different thing — a
/// `manifest` escalation says the newest release cannot be trusted, an `apply` one says
/// a verified build will not start — so a machine that announced one lane and then
/// stranded another told the user about the lane that was no longer the problem.
///
/// Keying on the class keeps the property that mattered (the SAME class never nags once
/// per check) and re-opens the one that was lost. A class that heals and later breaks
/// again is a new episode and speaks again; the caller's 30-minute persistence gate,
/// measured on that class's own clock, is what bounds how often that can happen.
#[cfg(target_os = "macos")]
fn persistent_notice_is_new(announced: Option<&str>, class: &str) -> bool {
    announced != Some(class)
}

/// Whether the loud "auto-update is failing" notice is OWED right now, given what was
/// last announced and which class (if any) has currently escalated.
///
/// Folds the "nothing is persistently failing, so nothing is owed" case into the same
/// predicate as the keyed latch, so the caller cannot answer one of the two questions
/// and forget the other.
#[cfg(target_os = "macos")]
fn persistent_notice_is_owed(announced: Option<&str>, class: Option<&str>) -> bool {
    class.is_some_and(|class| persistent_notice_is_new(announced, class))
}

/// The `title` [`HealthNotify`] is called with when a persistent failure of
/// `class` is first announced — the half of updating that is broken, in the
/// words a person reads on the update row (2026-09-23: one "aterm auto-update is
/// failing" for every class sent the reader after the wrong half). `apply`: a
/// verified build will not start; `pipeline` / `stage`: downloads fail or will
/// not verify; `manifest` (and any future class): the newest release cannot be
/// read or trusted.
#[must_use]
pub fn health_failing_title(class: &str) -> &'static str {
    match class {
        "apply" => "aterm can't install updates",
        "pipeline" | "stage" => "aterm can't download updates",
        _ => "aterm can't check for updates",
    }
}

/// The instant a persistent-failure notice's body dates its streak from — the
/// RFC 3339 stamp after "since " that `persistent_failure_notice` writes — or
/// `None` for a body that carries none. The reader of the writer below, beside
/// it, so the row's "since <date>" can never drift from the sentence it is cut
/// from.
#[must_use]
pub fn health_notice_since(body: &str) -> Option<&str> {
    let (_, rest) = body.split_once(" since ")?;
    let stamp = rest.split_whitespace().next()?.trim_end_matches(':');
    let date = stamp.get(..10)?;
    let well_formed = date.char_indices().all(|(i, c)| match i {
        4 | 7 => c == '-',
        _ => c.is_ascii_digit(),
    });
    well_formed.then_some(stamp)
}

/// The `title` [`HealthNotify`] is called with when the class this process ALREADY
/// announced is still failing and its count has moved (2026-09-14): the log and
/// Settings restate its number and no notification or row is owed — the person was
/// told. The body is the same sentence the announcement carried, with the new count.
///
/// Before this the count a person read was frozen at the announcement for the life
/// of the process: the bar said "3 consecutive checks since …" all day while
/// `health.toml` climbed to 6 (it moved once, because a SECOND process announced).
/// The row could always be rewritten in place; nothing ever asked it to.
pub const HEALTH_RESTATED_TITLE: &str = "aterm auto-update still failing";

/// What the check loop tells the GUI about the ledger's health, decided from the
/// SHARED ledger on EVERY cycle (2026-09-14) — a cycle this process skipped
/// because a sibling had already checked reads the same evidence as one it ran.
///
/// Before this the block lived after the network check only, so the skip path
/// (`checker_skip_at`) bypassed it: on 2026-09-13 the apply streak crossed the
/// 30-minute gate at 23:12:50, the GUI's next two cycles were skips, and the
/// notice fired at 00:12:34 — an hour late on a one-process machine, and for as
/// long as the GUI keeps losing the window's race with several sessions alive.
/// The recovery line (the standing row leaving) was gated the same way.
#[cfg(target_os = "macos")]
struct HealthAnnouncer {
    /// The clock at thread start: a streak whose latest failure predates this
    /// process is history, not news, and never re-notifies on every launch.
    started: String,
    /// The class this process announced and the count it spoke; `None` = none yet.
    /// Keyed on the class (not a bool) so a second stranded lane still speaks, and
    /// carrying the count so a moved count restates without re-notifying.
    announced: Option<(&'static str, u32)>,
    /// The pending build this process already announced as OVERDUE
    /// ([`Self::tick_overdue`]); `None` = none yet. Once per build per process:
    /// the notice stands as a row until the machine moves, so saying it again
    /// every cycle would be a nag, not news.
    overdue_announced: Option<u64>,
}

#[cfg(target_os = "macos")]
impl HealthAnnouncer {
    fn new(started: String) -> Self {
        Self {
            started,
            announced: None,
            overdue_announced: None,
        }
    }

    /// The loud notice owed because a newer build has waited on this machine for
    /// more than [`PENDING_UPDATE_OVERDUE_SECS`] while `current_build` still runs,
    /// or `None` (the 2026-09-22/23 update audit, plan P1-1(b)).
    ///
    /// ESCALATE ON THE MACHINE'S STATE, NOT ON A STREAK COUNT. The persistent
    /// notice above needs [`PERSISTENT_AFTER`] consecutive failures of one class,
    /// and a stranded apply lane never produced them: a structural failure
    /// converges after TWO attempts, and a capture refusal filed as a park miss
    /// never counted at all. So the owner's 0.90 and 0.91 each waited 20–25
    /// hours behind a verified build with no notice of any kind. Whatever the
    /// lane did or did not count, "a newer build has been here for an hour and
    /// is not running" is always true of a stuck update and never of a healthy
    /// one — a healthy ladder lands inside the hour's first minute.
    ///
    /// Pure over the ledger so the law is testable without a thread; the ledger's
    /// pending clock ([`health::Health::note_pending_update`]) is what makes the
    /// hour an hour rather than "since the last re-stage".
    ///
    /// `automatic` is the host's `[update] auto_apply` ([`set_automatic_apply`]):
    /// with it off a staged build waits for a person by design, which is not a
    /// failure and is never announced as one.
    fn tick_overdue(
        &mut self,
        h: &health::Health,
        now: &str,
        current_build: u64,
        automatic: bool,
    ) -> Option<(String, String)> {
        if !automatic {
            return None;
        }
        if h.pending_build <= current_build || h.pending_since.is_empty() {
            self.overdue_announced = None;
            return None;
        }
        if self.overdue_announced == Some(h.pending_build) {
            return None;
        }
        let waited = install::rfc3339_delta_secs(&h.pending_since, now)?;
        if waited < PENDING_UPDATE_OVERDUE_SECS {
            return None;
        }
        self.overdue_announced = Some(h.pending_build);
        Some(pending_update_overdue_notice(h, current_build))
    }

    /// The `(title, body)` owed to the GUI for the ledger as it stands at `now`,
    /// or `None`. Pure over the ledger so the law is testable without a thread.
    fn tick(
        &mut self,
        h: &health::Health,
        now: &str,
        current_build: u64,
    ) -> Option<(String, String)> {
        let active = h.last_failure_at.as_str() >= self.started.as_str();
        // DURATION gate beside the count gate: at the immediate-update cadence three
        // consecutive failures span ~4 minutes — a blip, not the "pipeline is
        // broken" signal the loud notice promises. Require the streak to have
        // PERSISTED (>= 30 min of failing) like it inherently did at the old 6h
        // cadence.
        //
        // MEASURED ON THE ESCALATING CLASS'S OWN CLOCK. `failing_since` is any-class:
        // it is stamped by the FIRST class to break and is deliberately never cleared
        // while an apply streak survives (`Health::record_success`). Reading the gate
        // from it meant a single stale apply failure from days ago backdated every
        // later streak, so a four-minute manifest blip cleared a thirty-minute gate
        // the instant it crossed the count — firing the loud notice for exactly the
        // transient this gate exists to swallow, and dating it days before the
        // problem existed.
        let escalated = h.persistent_class();
        let long_lived = escalated.is_some_and(|(class, _)| {
            install::rfc3339_delta_secs(h.class_since(class), now).is_some_and(|d| d >= 30 * 60)
        });
        if let Some((class, count)) = escalated
            && active
            && long_lived
        {
            let announced_class = self.announced.map(|(c, _)| c);
            if persistent_notice_is_owed(announced_class, Some(class)) {
                self.announced = Some((class, count));
                return Some(persistent_failure_notice(class, count, h, current_build));
            }
            if self.announced != Some((class, count)) {
                // The same class, a moved count: restate quietly.
                self.announced = Some((class, count));
                let (_, body) = persistent_failure_notice(class, count, h, current_build);
                return Some((HEALTH_RESTATED_TITLE.to_string(), body));
            }
            return None;
        }
        if !h.is_persistent()
            && let Some((class, _)) = self.announced.take()
        {
            // All healed → any class may speak again. A failure THIS process
            // reported gets its recovery reported too, once, so the standing
            // pull-down row can leave — naming the class, which is all the
            // heal proves (see [`HealthNotify`]).
            return Some((HEALTH_RECOVERED_TITLE.to_string(), class.to_string()));
        }
        None
    }
}

/// The loud notice for the class that escalated, as `(title, body)`: the title says
/// which half of updating is broken ([`health_failing_title`]), the body the count,
/// the date ([`health_notice_since`] reads it back), the cause and where the rest
/// is — the log line and Settings carry it whole; the update row keeps the date.
///
/// The COUNT and the sentence both come from the class that escalated. A single
/// hardcoded pipeline story told an apply-stranded machine "0 consecutive checks …
/// cannot be downloaded" — wrong number, wrong lane, and it sent the reader hunting
/// a download fault that did not exist.
#[cfg(target_os = "macos")]
fn persistent_failure_notice(
    class: &str,
    count: u32,
    h: &health::Health,
    current_build: u64,
) -> (String, String) {
    let cause = match class {
        // A stranded client (2026-09-14): the reason already names the anchor and
        // the reinstall, and "until it is republished" would be false — no release
        // the publisher cuts can be verified by this build.
        "manifest" if github::is_stale_anchor_refusal(&h.last_error) => format!(
            "{} — this machine stays on build {current_build} until it is reinstalled",
            h.last_error
        ),
        "manifest" => format!(
            "the newest release cannot be trusted ({}) — this machine stays on build \
             {current_build} until it is republished",
            h.last_error
        ),
        "stage" => format!(
            "updates download but will not verify or install ({})",
            h.last_error
        ),
        "apply" => format!(
            "an update is downloaded and verified but will not start ({})",
            h.last_apply_error
        ),
        // "pipeline", and any future class, keeps the original text.
        _ => "release manifests exist but cannot be downloaded — this build's update \
              pipeline is likely broken"
            .to_string(),
    };
    // The noun names the lane the count came from (2026-09-14): an apply streak
    // is failed attempts to install a build every check fetched fine — "3
    // consecutive checks" sent the owner after a download fault that did not exist.
    let counted = if class == "apply" {
        "failed attempts to install"
    } else {
        "failed checks"
    };
    (
        health_failing_title(class).to_string(),
        format!(
            "{count} {counted} in a row since {}: {cause}. Run `aterm ctl update status` \
             for details.",
            h.class_since(class)
        ),
    )
}

/// The loud notice for a newer build that has waited on this machine past
/// [`PENDING_UPDATE_OVERDUE_SECS`], as `(title, body)`, carrying the TYPED cause
/// the apply lane left in the ledger (plan P1-1(b)): its failure count and
/// reason for THAT build, the standing schedule or refusal it recorded, or — the
/// case that used to be invisible — that no apply attempt was recorded at all.
#[cfg(target_os = "macos")]
fn pending_update_overdue_notice(h: &health::Health, current_build: u64) -> (String, String) {
    let failed = (h.last_apply_failure_target_build == h.pending_build
        && h.apply_failures_for_target > 0)
        .then(|| {
            format!(
                "the apply lane has failed it {}× ({})",
                h.apply_failures_for_target, h.last_apply_error
            )
        });
    let standing = h
        .apply_refusal_applies_to(current_build)
        .then(|| h.last_apply_refusal.clone());
    let cause = match (failed, standing) {
        (Some(failed), Some(standing)) => format!("{failed}; {standing}"),
        (Some(failed), None) => failed,
        (None, Some(standing)) => format!("the apply lane is holding it back: {standing}"),
        (None, None) => "no apply attempt has been recorded for it".to_string(),
    };
    (
        health_failing_title("apply").to_string(),
        format!(
            "build {} has been waiting on this machine since {} and build {current_build} is \
             still running: {cause}. Run `aterm ctl update status` for details.",
            h.pending_build, h.pending_since
        ),
    )
}

#[cfg(all(test, target_os = "macos"))]
mod persistent_notice_tests {
    use super::persistent_notice_is_new;

    /// THE OWNER'S PULL-DOWN ON 2026-09-14 (aterm.log 1789369954): "aterm auto-update
    /// is failing — 3 consecutive checks since 2026-09-14T05:42:50Z: an update is
    /// downloaded and verified but will not start (…)". Every CHECK that day
    /// succeeded (health.toml: network/pipeline/manifest/stage streaks 0, kind =
    /// "apply", failing_applies = 6). The count is the APPLY streak — three automatic
    /// apply attempts — and the sentence calls them checks, which sends the reader
    /// after a check fault that does not exist. The apply class must count what it
    /// counts.
    #[test]
    fn observability_audit_an_apply_class_notice_counts_applies_not_checks() {
        let ledger = super::health::Health {
            apply_failures: 3,
            apply_since: "2026-09-14T05:42:50Z".to_string(),
            kind: "apply".to_string(),
            last_apply_error: "installed bundle failed pre-park verification: the installed \
                               bundle at /Applications/aterm.app cannot be the rollback \
                               source the swap installs"
                .to_string(),
            ..super::health::Health::default()
        };
        let (title, body) = super::persistent_failure_notice("apply", 3, &ledger, 1789276245);
        assert_eq!(title, "aterm can't install updates");
        assert!(
            !body.contains("checks"),
            "an apply streak is not a run of failed checks — every check that day \
             succeeded: {body}"
        );
        assert!(
            body.starts_with("3 failed attempts to install in a row"),
            "the count must name the lane it counts: {body}"
        );
        assert!(
            body.contains("2026-09-14T05:42:50Z"),
            "dated by the apply class's own clock: {body}"
        );
        // The acquisition classes really are checks, and keep saying so.
        let pipeline = super::health::Health {
            pipeline_failures: 20,
            pipeline_since: "2026-08-27T22:04:36Z".to_string(),
            ..super::health::Health::default()
        };
        let (title, body) = super::persistent_failure_notice("pipeline", 20, &pipeline, 1);
        assert_eq!(title, "aterm can't download updates");
        assert!(
            body.starts_with("20 failed checks in a row since 2026-08-27T22:04:36Z"),
            "{body}"
        );
        assert!(
            body.contains("`aterm ctl update status`") && !body.contains("aterm-ctl"),
            "the command is spelled the way the one binary takes it: {body}"
        );
    }

    /// THE ROW'S DATE IS READ FROM THE SENTENCE IT IS CUT FROM (2026-09-23): the
    /// update row says "since Sep 14", and the reader beside the writer finds the
    /// stamp every class's body carries — and nothing in a body without one.
    #[test]
    fn the_notice_body_hands_back_the_date_it_was_written_with() {
        for (class, count) in [
            ("apply", 3),
            ("pipeline", 20),
            ("stage", 4),
            ("manifest", 5),
        ] {
            let ledger = super::health::Health {
                apply_since: "2026-09-14T05:42:50Z".to_string(),
                pipeline_since: "2026-09-14T05:42:50Z".to_string(),
                stage_since: "2026-09-14T05:42:50Z".to_string(),
                manifest_since: "2026-09-14T05:42:50Z".to_string(),
                last_error: "signature did not verify".to_string(),
                last_apply_error: "the successor died".to_string(),
                ..super::health::Health::default()
            };
            let (title, body) = super::persistent_failure_notice(class, count, &ledger, 7);
            assert_eq!(title, super::health_failing_title(class));
            assert_eq!(
                super::health_notice_since(&body),
                Some("2026-09-14T05:42:50Z"),
                "{class}: {body}"
            );
        }
        assert_eq!(super::health_notice_since("no date here"), None);
        assert_eq!(
            super::health_notice_since("broken since yesterday: it failed"),
            None
        );
        assert_eq!(
            super::health_failing_title("manifest"),
            "aterm can't check for updates"
        );
    }

    /// A NEWER BUILD WAITING FOR AN HOUR IS SAID OUT LOUD, ONCE, WITH ITS CAUSE (the
    /// 2026-09-22/23 update audit, plan P1-1(b)). The replayed incident: 0.91
    /// staged, two failed applies (a structural lane converges after two, under
    /// `PERSISTENT_AFTER`), then 25 hours in which the log held zero
    /// `update-health:` lines. Before the hour: nothing (the ladder lands within
    /// fifteen minutes on a healthy machine). Past it: the loud title, the
    /// waiting build, the running one and the lane's own words — once. A
    /// machine with no recorded attempt says THAT. A caught-up machine is quiet
    /// and a later build is news again.
    #[test]
    fn an_overdue_pending_build_is_announced_once_with_the_typed_cause() {
        use super::health::Health;
        let mut announcer = super::HealthAnnouncer::new("2026-09-22T00:00:00Z".to_string());
        let stranded = Health {
            apply_failures: 2,
            apply_since: "2026-09-22T00:11:30Z".to_string(),
            last_failure_at: "2026-09-22T00:11:31Z".to_string(),
            kind: "apply".to_string(),
            last_apply_error: "overlap handoff failed safely: handoff proof ended \
                               AdoptionMismatch"
                .to_string(),
            last_apply_failure_build: 1790019739,
            last_apply_failure_target_build: 1790120495,
            apply_failures_for_target: 2,
            last_apply_refusal: "automatic apply of build 1790120495 is out of retries".to_string(),
            last_apply_refusal_at: "2026-09-22T00:11:32Z".to_string(),
            last_apply_refusal_build: 1790019739,
            pending_build: 1790120495,
            pending_since: "2026-09-22T00:10:45Z".to_string(),
            ..Health::default()
        };
        // Two failures: the streak notice is not owed (the incident's silence)…
        assert!(stranded.persistent_class().is_none());
        // With automatic apply switched off a staged build waits for a person by
        // design: never "failing", however long it waits.
        assert!(
            announcer
                .tick_overdue(&stranded, "2026-09-25T00:00:00Z", 1790019739, false)
                .is_none(),
            "auto_apply = false is not a failure"
        );
        // …and inside the hour the overdue one is not either.
        assert!(
            announcer
                .tick_overdue(&stranded, "2026-09-22T01:10:00Z", 1790019739, true)
                .is_none(),
            "fifty-nine minutes is not overdue"
        );
        let (title, body) = announcer
            .tick_overdue(&stranded, "2026-09-22T01:11:00Z", 1790019739, true)
            .expect("past the hour the stranded machine is announced");
        assert_eq!(
            title,
            super::health_failing_title("apply"),
            "an update that will not land is the install half failing"
        );
        assert!(
            body.contains("`aterm ctl update status`") && !body.contains("aterm-ctl"),
            "{body}"
        );
        assert!(
            body.contains(
                "build 1790120495 has been waiting on this machine since \
                           2026-09-22T00:10:45Z"
            ) && body.contains("build 1790019739 is still running"),
            "{body}"
        );
        assert!(
            body.contains(
                "failed it 2× (overlap handoff failed safely: handoff proof ended \
                           AdoptionMismatch)"
            ) && body.contains("out of retries"),
            "the typed cause, in the lane's own words: {body}"
        );
        assert!(
            announcer
                .tick_overdue(&stranded, "2026-09-22T09:00:00Z", 1790019739, true)
                .is_none(),
            "once per build: the row stands, the notice does not repeat"
        );
        // Caught up: quiet, and the latch resets for the next episode.
        assert!(
            announcer
                .tick_overdue(&stranded, "2026-09-23T00:00:00Z", 1790120495, true)
                .is_none()
        );
        // A different build, never attempted: the cause says exactly that.
        let unattempted = Health {
            pending_build: 1790300000,
            pending_since: "2026-09-23T00:00:00Z".to_string(),
            ..Health::default()
        };
        let (_, body) = announcer
            .tick_overdue(&unattempted, "2026-09-23T02:00:00Z", 1790120495, true)
            .expect("a new episode is news");
        assert!(
            body.contains("no apply attempt has been recorded for it"),
            "{body}"
        );
    }

    #[test]
    fn a_second_persistent_class_still_speaks_while_the_announced_one_stays_quiet() {
        let mut announced: Option<&'static str> = None;
        assert!(
            persistent_notice_is_new(announced, "pipeline"),
            "the first escalation of the process is always news"
        );
        announced = Some("pipeline");
        assert!(
            !persistent_notice_is_new(announced, "pipeline"),
            "the same class must not re-announce itself every check"
        );
        assert!(
            persistent_notice_is_new(announced, "apply"),
            "a DIFFERENT stranded lane is a different message and must still be told — \
             the bare-bool latch swallowed exactly this one"
        );
        announced = Some("apply");
        assert!(!persistent_notice_is_new(announced, "apply"));
        assert!(
            persistent_notice_is_new(announced, "pipeline"),
            "and a class that is escalating again after healing is a new episode"
        );
    }

    /// THE STANDING ROW'S COUNT FOLLOWS THE LEDGER (2026-09-14, audit OBS-3): the
    /// announced class restates its number through [`HEALTH_RESTATED_TITLE`] when the
    /// count moves — no second loud notice — and speaks the loud title again only for
    /// a different class or a new episode after healing. And the whole decision is a
    /// pure function of the ledger, which is what lets the loop ask it on a skipped
    /// cycle too (COORD-3).
    #[test]
    fn the_announcer_restates_a_moved_count_quietly_and_re_announces_only_a_new_class() {
        use super::health::Health;
        let started = "2026-09-13T22:00:00Z".to_string();
        let mut announcer = super::HealthAnnouncer::new(started);
        let ledger = |apply_failures: u32, last: &str| Health {
            apply_failures,
            apply_since: "2026-09-13T22:40:00Z".to_string(),
            last_failure_at: last.to_string(),
            kind: "apply".to_string(),
            last_apply_error: "the successor died".to_string(),
            ..Health::default()
        };
        // Not yet 30 minutes on the class's clock: nothing.
        assert!(
            announcer
                .tick(
                    &ledger(3, "2026-09-13T22:50:00Z"),
                    "2026-09-13T23:00:00Z",
                    7
                )
                .is_none()
        );
        // Past the gate: the loud announcement, once.
        let (title, body) = announcer
            .tick(
                &ledger(3, "2026-09-13T23:12:50Z"),
                "2026-09-13T23:13:44Z",
                7,
            )
            .expect("the first escalation is announced");
        assert_eq!(title, super::health_failing_title("apply"));
        assert!(
            body.starts_with("3 failed attempts to install in a row"),
            "{body}"
        );
        // Same class, same count: silence.
        assert!(
            announcer
                .tick(
                    &ledger(3, "2026-09-13T23:12:50Z"),
                    "2026-09-13T23:37:45Z",
                    7
                )
                .is_none()
        );
        // Same class, the count moved: a QUIET restate carrying the new number.
        let (title, body) = announcer
            .tick(
                &ledger(6, "2026-09-14T00:10:00Z"),
                "2026-09-14T00:12:34Z",
                7,
            )
            .expect("a moved count is restated");
        assert_eq!(title, super::HEALTH_RESTATED_TITLE);
        assert!(
            body.starts_with("6 failed attempts to install in a row"),
            "{body}"
        );
        // A failure that predates this process is history: no restate for it.
        let mut fresh = super::HealthAnnouncer::new("2026-09-14T01:00:00Z".to_string());
        assert!(
            fresh
                .tick(
                    &ledger(6, "2026-09-14T00:10:00Z"),
                    "2026-09-14T01:05:00Z",
                    7
                )
                .is_none()
        );
        // Healed: the recovery line, once, then nothing.
        let healed = Health::default();
        let (title, body) = announcer
            .tick(&healed, "2026-09-14T02:00:00Z", 7)
            .expect("a failure this process reported gets its recovery reported");
        assert_eq!(title, super::HEALTH_RECOVERED_TITLE);
        assert_eq!(body, "apply", "the heal names the class it proves");
        assert!(announcer.tick(&healed, "2026-09-14T02:01:00Z", 7).is_none());
        // A new episode after healing speaks the loud title again.
        let (title, _) = announcer
            .tick(
                &ledger(3, "2026-09-14T03:30:00Z"),
                "2026-09-14T03:31:00Z",
                7,
            )
            .expect("a new episode is news");
        assert_eq!(title, super::health_failing_title("apply"));
    }
}

/// Spawn the background update check + stage on a detached thread. Returns
/// immediately; the work (network + disk I/O) happens off the event loop and is a
/// no-op when the updater is disabled or this is not an installed `.app`.
///
/// `notify` (optional) surfaces self-healing ledger events OBSERVED DURING THIS
/// PROCESS'S LIFETIME (the watermark seeds from the clock at thread start, so
/// history never re-notifies on every launch): a pipeline-failure streak crossing
/// [`PERSISTENT_AFTER`] with this process contributing its latest failure (once per
/// streak).
#[cfg(target_os = "macos")]
pub fn spawn_background_check(
    current_build: u64,
    source: Source,
    notify: Option<HealthNotify>,
    on_staged: Option<StagedNotify>,
) {
    spawn_background_check_with_source(
        current_build,
        std::sync::Arc::new(move || Some(source.clone())),
        notify,
        on_staged,
    );
}

/// Supplies the current configured channel at the start of each background cycle.
/// The callback runs on the checker thread and must return within a bounded time.
/// `None` skips network work and retries the provider in five seconds (once-only
/// mode stops); a running check retains its source snapshot.
pub type SourceProvider = std::sync::Arc<dyn Fn() -> Option<Source> + Send + Sync>;

/// One request-local configuration observation. The provider is sampled again
/// before Linux replacement, so a reload during download cannot bypass a veto.
#[derive(Clone, Debug)]
pub struct CheckSettings {
    pub source: Source,
    pub auto_apply: bool,
}

pub type CheckSettingsProvider = std::sync::Arc<dyn Fn() -> Option<CheckSettings> + Send + Sync>;

pub fn check_now_with_settings(
    current_build: u64,
    provider: &CheckSettingsProvider,
) -> UpdateStatus {
    #[cfg(target_os = "linux")]
    {
        linux::check_with_settings(current_build, provider, true)
    }
    #[cfg(not(target_os = "linux"))]
    {
        match provider() {
            Some(settings) => check_now(current_build, &settings.source),
            None => UpdateStatus::empty(
                enabled(),
                current_build,
                "current update settings could not be read".into(),
            ),
        }
    }
}

pub fn spawn_background_check_with_settings(
    current_build: u64,
    provider: CheckSettingsProvider,
    notify: Option<HealthNotify>,
    on_staged: Option<StagedNotify>,
) {
    #[cfg(target_os = "linux")]
    {
        let _ = on_staged;
        linux::spawn_background_check_with_settings(current_build, provider, notify);
    }
    #[cfg(not(target_os = "linux"))]
    {
        spawn_background_check_with_source(
            current_build,
            std::sync::Arc::new(move || provider().map(|settings| settings.source)),
            notify,
            on_staged,
        );
    }
}

/// Linux disk-stage/trial facts are not a macOS DMG or a live-session handoff.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LinuxUpdateStatus {
    pub installed_build: u64,
    pub staged_build: Option<u64>,
    pub staged_version: Option<String>,
    pub staged_commit: Option<String>,
    pub trial_phase: Option<String>,
    pub trial_starts: u32,
    pub trial_healthy: bool,
}

/// The same single background checker, sampling its source before each cycle so
/// configuration reloads take effect without restarting the process.
#[cfg(target_os = "macos")]
pub fn spawn_background_check_with_source(
    current_build: u64,
    source_provider: SourceProvider,
    notify: Option<HealthNotify>,
    on_staged: Option<StagedNotify>,
) {
    if !automatic() {
        // Said once per process, where the updater's lines go: the one record of why this
        // machine is not checking by itself. A typed or clicked check still runs.
        if enabled() {
            log(
                "automatic update checks are off — [update] enabled = false (Settings ▸ \
                 Terminal ▸ Updates); Check for Updates still checks when asked",
            );
        }
        return;
    }
    // Not an installed `.app` (dev build / `cargo run` / `target/` binary) → nothing to
    // swap; don't spawn a thread that would only ever no-op. Now that `enabled()` no
    // longer requires a pinned anchor, this is what keeps dev runs inert.
    if bundle::resolve().is_none() {
        return;
    }
    std::thread::Builder::new()
        .name("aterm-update".into())
        .spawn(move || {
            // One cadence (`cadence::INTERVAL_SECS`, no knob). Cost honesty: a check
            // spends ZERO metered requests — one HEAD of the evergreen
            // github.com/…/releases/latest/download/aterm-appcast.toml, whose 302
            // names the newest tag, and tag-specific GETs on the same unmetered host
            // only when that tag moved. The interval is a courtesy to the download
            // host and a bound on staleness; the in-session lane applies what it
            // stages (see the module docs' delivery model).
            //
            // This is the BASE interval only. The wait actually taken is jittered and
            // backs off while checks fail, and returns early when the Mac turns out to
            // have been asleep — see `cadence`, which owns all three policies.
            let interval = cadence::INTERVAL_SECS;
            let mut schedule = cadence::Cadence::new(std::time::Duration::from_secs(interval));
            let mut failures = cadence::FailureLog::default();
            // Once per process: a channel this machine cannot read is a configuration
            // defect, not an event, so it is announced once and then lives in
            // `status.toml` (rewritten every check) rather than nagging.
            let mut notified_unreadable = false;
            // Per-process dedup, seeded from the clock at thread start so history
            // never re-notifies on every launch: the persistent-failure notice
            // requires the streak's latest failure to postdate this thread (RFC3339
            // strings compare chronologically), so a stale streak from a build that
            // isn't even checking any more (e.g. no token) stays quiet.
            // KEYED on the class that was announced (see `HealthAnnouncer`), not a
            // bare bool. As a bool the latch swallowed every class after the first
            // for the life of the process: a machine whose downloads broke
            // (announced) and whose apply lane then stranded it heard about the
            // download only. The two notices name different lanes and ask for
            // different fixes, so dropping the second is losing a message, not
            // deduping one.
            let mut announcer = HealthAnnouncer::new(install::now_rfc3339());
            // The ledger's verdict, spoken to the GUI. Called on EVERY cycle — the
            // skip path included (2026-09-14): the ledger is shared, so a cycle a
            // sibling checked for us carries exactly the same evidence.
            //
            // AND THE MACHINE'S STATE BESIDE THE STREAKS (2026-09-22/23 update
            // audit, plan P1-1(b)): which newer build is waiting here — the staged
            // marker, or a bundle already installed under this older image — is
            // noted in the ledger's pending clock every cycle, and a build that has
            // waited past `PENDING_UPDATE_OVERDUE_SECS` is announced with its
            // typed cause, whatever the failure streaks say.
            let speak_health = |announcer: &mut HealthAnnouncer| {
                if let (Some(cb), Some(staging)) = (notify.as_ref(), paths::Staging::resolve()) {
                    let staged =
                        manifest::Ready::read_publishable(&staging).map(|ready| ready.build_number);
                    let installed = bundle::resolve()
                        .and_then(|installed| verify::bundle_build_number(&installed.app_root).ok())
                        .filter(|build| *build > current_build);
                    let h = health::Health::note_pending_update(
                        &staging.health(),
                        current_build,
                        staged.max(installed),
                    );
                    let now = install::now_rfc3339();
                    if let Some((title, body)) = announcer.tick(&h, &now, current_build) {
                        cb(title, body);
                    }
                    if let Some((title, body)) =
                        announcer.tick_overdue(&h, &now, current_build, automatic_apply_on())
                    {
                        cb(title, body);
                    }
                }
            };
            // The installed bundle we last told the GUI about (by build). Announced
            // once per build, re-announced when the bundle moves again. And the last
            // (build, error) we could NOT verify, so a permanently unverifiable bundle
            // is logged once, not every cycle.
            let mut announced_installed: Option<u64> = None;
            let mut unverifiable_installed: Option<(u64, String)> = None;
            // The newest staged build this process has told `on_staged` about, so a
            // stage a SIBLING process published is announced once per build from the
            // skip path (see `announce_sibling_stage`).
            let mut announced_stage: Option<u64> = None;
            // AN UNCOMMITTED CANDIDATE CHECKS NOTHING (2026-09-19): a handoff
            // successor holds this thread until the outgoing process has
            // committed to it — a check from a process the parent may still
            // reject is a wasted request at best, and at worst the stage it
            // finds is read by two processes with two opinions of it.
            if wait_while_uncommitted_handoff_candidate(UNCOMMITTED_CANDIDATE_HOLD_BOUND) {
                log("update checks waited for this handoff to be committed before the first check");
            }
            loop {
                let Some(source) = source_provider() else {
                    // A failed bounded config query is not permission to use a
                    // stale channel. Retry the provider without holding any lane.
                    std::thread::sleep(std::time::Duration::from_secs(5));
                    continue;
                };
                // THE BUNDLE UNDER OUR OWN EXECUTABLE MAY HAVE MOVED ON WITHOUT A STAGE.
                // The release cutter rewrites the bundle it was launched from, a user
                // drags a new `.app` over the running one, a sibling process swaps
                // it — none of that writes `ready.toml`, so `on_staged` above never
                // fires and, before 2026-08-18, the running process learned about the
                // newer bundle only at startup or when a stage happened to land. A
                // plist read per cycle, and — only when it says newer — the same
                // codesign policy the GUI's facts worker applies, is what makes the
                // activation lane fire on its own instead of waiting for a coincidence.
                // A DEV-MARKED bundle is skipped outright (`bundle::resolve` is the
                // dev-mark-aware resolver): it can never pass the shipped tier, so
                // verifying it every cycle would only spawn codesign forever.
                if let Some(cb) = on_staged.as_ref()
                    && let Some(installed) = bundle::resolve()
                    && let Ok(installed_build) = verify::bundle_build_number(&installed.app_root)
                    && installed_build > current_build
                    // A build below the operator apply floor (a yank) is not an update
                    // wherever it sits; announcing it would stage something every
                    // handoff then refuses.
                    && paths::Staging::resolve().is_none_or(|s| {
                        installed_build >= manifest::Floor::read(&s.floor()).min_build
                    })
                    && announced_installed != Some(installed_build)
                {
                    // ANNOUNCE ONLY WHAT THE GUI CAN IMPORT. The plist is written
                    // first and signed/notarized minutes later (the cutter lays the
                    // bundle out in place; Gatekeeper refuses it until the ticket is
                    // stapled), and the GUI's facts worker imports nothing it cannot
                    // verify — so an announcement latched on plist evidence alone
                    // landed inside that window, the import failed silently, and
                    // nothing ever re-announced the same build (2026-08-19 audit).
                    // Verify HERE, on this thread, before latching: an unverifiable
                    // newer bundle is retried next cycle, not remembered.
                    match verify::verify_bundle_policy(&installed.app_root, effective_team_id()) {
                        Ok(()) => {
                            announced_installed = Some(installed_build);
                            let version = verify::bundle_short_version(&installed.app_root)
                                .unwrap_or_else(|_| format!("build {installed_build}"));
                            log(&format!(
                                "the bundle at this executable's path is already build \
                                 {installed_build} (running {current_build}) — the GUI \
                                 activates it"
                            ));
                            cb(installed_build, version);
                        }
                        Err(error) => {
                            // Once per (build, reason): the notarize window is minutes,
                            // a broken seal is forever, and neither deserves a log line
                            // per cycle.
                            let key = (installed_build, error.clone());
                            if unverifiable_installed.as_ref() != Some(&key) {
                                log(&format!(
                                    "the bundle at this executable's path reports build \
                                     {installed_build} (running {current_build}) but does not \
                                     verify ({error}); re-checking each cycle until it does"
                                ));
                                unverifiable_installed = Some(key);
                            }
                        }
                    }
                }
                match check_lane().try_lock() {
                    Ok(mut lane) => {
                        // CROSS-PROCESS DEDUP. The lane mutex above is process-local
                        // (the module docs say so), and since the one-binary era every
                        // terminal SESSION runs this same loop — round-11 found that
                        // sessions used to run NONE of it, so terminal-only Macs never
                        // updated at all. N aterm processes must cost the shared
                        // GitHub budget ~one check per interval, not N: the flock
                        // serializes checkers machine-wide (the holder is bounded by
                        // the network timeouts), and the ledger re-read under it turns
                        // "another process just completed this interval's check" into
                        // a quiet skip. The freshness window is 70% of the base —
                        // strictly below the jittered minimum wait (80%), so a
                        // process can never mistake its OWN previous stamp for
                        // another checker's and starve itself.
                        let checker_staging = paths::Staging::resolve();
                        let _checker_gate = checker_staging.as_ref().and_then(|s| {
                            aterm_update_core::FileLock::acquire(
                                &s.status.with_file_name("checker.lock"),
                            )
                            .ok()
                        });
                        let now_unix = unix_now_secs();
                        if let Some((reason, window_expiry)) =
                            checker_staging.as_ref().and_then(|s| {
                                checker_skip_for(
                                    s,
                                    current_build,
                                    &source,
                                    schedule.base(),
                                    now_unix,
                                )
                            })
                        {
                            log(&reason);
                            announce_sibling_stage(
                                &mut announced_stage,
                                current_build,
                                status(current_build).as_ref(),
                                on_staged.as_ref(),
                            );
                            // Point the timer at the WINDOW'S END (scattered), not at the
                            // next tick of the base (2026-09-14): a fresh process used to
                            // re-read and re-log the same skip every tick for the whole
                            // window.
                            let until = skip_timer_target(window_expiry, cadence::entropy_byte());
                            schedule.hold_until(
                                std::time::Instant::now()
                                    + std::time::Duration::from_secs(
                                        until.saturating_sub(now_unix),
                                    ),
                            );
                            // Release the flock AND local lane BEFORE sleeping: the
                            // former blocks sibling processes, the latter blocks
                            // this process's manual checks — then take the same jittered wait the loop tail
                            // takes (a bare `continue` would skip the tail's sleep
                            // and spin hot). The wake subtleties (settle window,
                            // still-failing suppression) only matter ahead of a
                            // network check, which this cycle deliberately isn't.
                            check_lane::after_skip(_checker_gate, lane, interval, || {
                                speak_health(&mut announcer);
                                let (_delay, waited) = cadence::wait(&schedule);
                                if matches!(waited, cadence::Waited::Woke(_)) {
                                    schedule.woke();
                                }
                            });
                            continue;
                        }
                        // Stamped BEFORE the check so the ledger can be asked, after
                        // it, whether THIS check recorded a failure (see the `Ok(None)`
                        // arm). RFC3339 strings compare chronologically.
                        let check_started = install::now_rfc3339();
                        let result = github::check_and_stage(current_build, &source);
                        check_lane().complete(&mut lane, current_build, &source);
                        match result {
                            Ok(Some(v)) => {
                                emit(failures.success());
                                schedule.succeeded();
                                // "is staged", not "was staged just now": the check also
                                // answers `Some` for a build that was already published and
                                // is only waiting to be applied (its re-download may be
                                // backed off — a stage backoff never gates an apply).
                                //
                                // SAY WHAT THE LEDGER KNOWS ABOUT THIS BUILD (2026-09-14).
                                // "the GUI applies it in place; the next launch is the
                                // fallback" was logged twenty times across ~10 h while the
                                // apply lane had failed that very build six times and stood
                                // down, and on an unsigned install the promised fallback
                                // refuses for the same reason. The stand-down itself lives
                                // only in GUI memory; the failure count and its reason are
                                // the ledger's, so those are what the line carries.
                                let staged_build =
                                    status(current_build).and_then(|s| s.staged_build);
                                let lane = apply_lane_report(current_build).filter(|r| {
                                    staged_build.is_some_and(|b| b == r.last_failure_target_build)
                                        && r.failures_for_target > 0
                                });
                                match lane {
                                    Some(r) if refusal_needs_person(&r.last_failure) => {
                                        log(&format!(
                                            "update {v} is staged but this install cannot apply \
                                             it by any lane ({}× refused: {}); run `aterm-ctl \
                                             update status`",
                                            r.failures_for_target, r.last_failure
                                        ));
                                    }
                                    Some(r) => {
                                        log(&format!(
                                            "update {v} is staged; the apply lane has failed it \
                                             {}× ({}) — run `aterm-ctl update status`; the next \
                                             launch is the fallback",
                                            r.failures_for_target, r.last_failure
                                        ));
                                    }
                                    None => {
                                        log(&format!(
                                            "update {v} is staged — the GUI applies it in place \
                                             (auto-apply); the next launch is the fallback"
                                        ));
                                    }
                                }
                                // RFC Rung 2: surface the staged build to the GUI so it can arm
                                // the in-session apply lane and show the update-ready nudge
                                // (Version menu ⬆️ / tab-strip ↻). The staged build number comes
                                // from the ready marker (status reads it, no I/O).
                                if let Some(cb) = on_staged.as_ref()
                                    && let Some(b) =
                                        status(current_build).and_then(|s| s.staged_build)
                                {
                                    announced_stage = Some(b);
                                    cb(b, v);
                                }
                            }
                            Ok(None) if unreadable::is_stranded() => {
                                // Not a success: GitHub answered that this machine
                                // cannot read the channel at all. Back off; the backoff
                                // clears on the first readable check, so a channel
                                // repaired mid-session is noticed within one backoff
                                // ceiling at worst — `max(MAX_BACKOFF,
                                // MAX_BACKOFF_INTERVALS × base)`.
                                schedule.failed();
                            }
                            Ok(None) if github::rate_limited() => {
                                // The download host asked us to slow down. That is a
                                // CADENCE problem, not a broken updater: lengthen the
                                // wait (the entire remedy) but emit no failure line and
                                // no ledger entry, so a 429 never accrues the streak
                                // that fires "your update pipeline is likely broken".
                                schedule.failed();
                            }
                            Ok(None) => {
                                // A completed check that found nothing to do is a
                                // SUCCESS: the network and the token both worked.
                                //
                                // Unless it wrote a FAILURE to the ledger on its way
                                // here. The manifest dead-end — an authoritative
                                // release that cannot be trusted — records its class
                                // and then returns `Ok(None)`, so it used to land in
                                // this arm and be counted as a success: no failure
                                // line, and `schedule.succeeded()` kept the cadence at
                                // full speed. That is why the 2026-07-25 machine
                                // reached 597 failures instead of backing off — ~13h
                                // at an un-backed-off 75s cadence is ~624 checks.
                                // Ask the ledger rather than trusting the return
                                // value: a check that recorded a failure is not a
                                // success, whatever it returned.
                                let recorded_failure = paths::Staging::resolve().is_some_and(|s| {
                                    health::Health::read(&s.health()).last_failure_at.as_str()
                                        >= check_started.as_str()
                                });
                                if recorded_failure {
                                    emit(Some(failures.failure(
                                        "no usable release this check — run \
                                         `aterm-ctl update status` for the reason",
                                    )));
                                    schedule.failed();
                                } else {
                                    emit(failures.success());
                                    schedule.succeeded();
                                }
                            }
                            // A network that could not be reached at all — the first
                            // check after a cold boot, or on a Wi-Fi still joining —
                            // is retried within seconds on the cadence's short rungs,
                            // and said at INFO while those run: it is expected, and a
                            // WARN there buried the warnings that matter (2026-09-23).
                            Err(e)
                                if paths::Staging::resolve().is_some_and(|s| {
                                    unreachable_before_the_channel(
                                        &e,
                                        &health::Health::read(&s.health()),
                                        &check_started,
                                    )
                                }) =>
                            {
                                schedule.failed_offline();
                                emit(Some(
                                    failures.failure_expected(&e, schedule.retrying_offline()),
                                ));
                            }
                            Err(e) => {
                                emit(Some(failures.failure(&e)));
                                schedule.failed();
                            }
                        }
                    }
                    Err(std::sync::TryLockError::WouldBlock) => {
                        log("periodic update tick joined an already-running check");
                    }
                    Err(std::sync::TryLockError::Poisoned(poisoned)) => {
                        drop(poisoned.into_inner());
                        warn("update check lane recovered after a worker panic");
                    }
                }
                // A machine that cannot READ its release channel can never update, and
                // nothing else in the updater will ever report a failure for it (this
                // is deliberately not a health-ledger failure — a configuration state
                // is not a transient fault). Raise it on the SAME channel the
                // broken-pipeline notice uses, once, so the user actually learns that
                // this Mac is stranded.
                if unreadable::is_stranded()
                    && !notified_unreadable
                    && let Some(cb) = notify.as_ref()
                {
                    notified_unreadable = true;
                    let (title, body) = unreadable::notification();
                    cb(title, body);
                }
                speak_health(&mut announcer);
                // Jittered, backed-off, wake-aware wait. A detected wake returns early
                // and clears the backoff — the outage the backoff was about belonged
                // to a network this Mac is no longer on — then lets the network
                // associate before the next check, instead of burning a guaranteed
                // DNS failure the moment the lid opens.
                let (delay, waited) = cadence::wait(&schedule);
                if let cadence::Waited::Woke(gap) = waited {
                    log(&format!(
                        "woke after ~{}s of system sleep (during a {}s wait) — letting \
                         the network settle for {}s, then checking",
                        gap.as_secs(),
                        delay.as_secs(),
                        cadence::WAKE_SETTLE.as_secs()
                    ));
                    schedule.woke();
                    // Suppress the "still failing" carry-over too: a pre-sleep DNS
                    // failure is not evidence about the post-wake network.
                    failures = cadence::FailureLog::default();
                    std::thread::sleep(cadence::WAKE_SETTLE);
                }
            }
        })
        .ok();
}

/// A SIBLING'S STAGE IS THIS PROCESS'S NEWS TOO (audit AU-5). A skipping cycle never
/// reaches the check that would answer `Some`, and the sibling that just checked may
/// have staged a build this process was never told about — its `on_staged` fired in
/// ITS process — so a window whose sessions won every check race kept the stage hidden
/// until relaunch. Read the shared stage and announce a newer one ONCE per build
/// (`announced` is the latch, shared with the check path's own announcement), so this
/// process's apply lane arms. Returns whether it announced.
#[cfg(target_os = "macos")]
fn announce_sibling_stage(
    announced: &mut Option<u64>,
    current_build: u64,
    status: Option<&UpdateStatus>,
    on_staged: Option<&StagedNotify>,
) -> bool {
    let (Some(cb), Some(status)) = (on_staged, status) else {
        return false;
    };
    let Some(build) = status.staged_build.filter(|build| *build > current_build) else {
        return false;
    };
    if *announced == Some(build) {
        return false;
    }
    *announced = Some(build);
    let version = status
        .staged_version
        .clone()
        .unwrap_or_else(|| format!("build {build}"));
    log(&format!(
        "update {version} (build {build}) is staged by another aterm process — the GUI \
         applies it in place"
    ));
    cb(build, version);
    true
}

/// Route a [`cadence::LogAction`] to the app log. `None` (nothing to say) is the
/// common case, so the call sites stay one line.
#[cfg(target_os = "macos")]
fn emit(action: Option<cadence::LogAction>) {
    match action {
        Some(cadence::LogAction::Warn(text)) => warn(&text),
        Some(cadence::LogAction::Log(text)) => log(&text),
        Some(cadence::LogAction::Suppress) | None => {}
    }
}

/// Linux runs its enrolled single-executable checker; other platforms are inert.
#[cfg(not(target_os = "macos"))]
pub fn spawn_background_check(
    _current_build: u64,
    _source: Source,
    _notify: Option<HealthNotify>,
    _on_staged: Option<StagedNotify>,
) {
    #[cfg(target_os = "linux")]
    linux::spawn_background_check(
        _current_build,
        std::sync::Arc::new(move || Some(_source.clone())),
        _notify,
    );
}

/// Non-macOS no-op.
#[cfg(not(target_os = "macos"))]
pub fn spawn_background_check_with_source(
    _current_build: u64,
    _source_provider: SourceProvider,
    _notify: Option<HealthNotify>,
    _on_staged: Option<StagedNotify>,
) {
    #[cfg(target_os = "linux")]
    linux::spawn_background_check(_current_build, _source_provider, _notify);
}

/// Consecutive same-class failed checks at which the failure is called PERSISTENT
/// (drives [`UpdateStatus::is_failing_persistently`], the status wording, the
/// GUI notification, and the `aterm-ctl update` `persistent=` field). Cross-platform
/// so status consumers can reason about it; the macOS `health` ledger enforces the
/// same threshold.
pub const PERSISTENT_AFTER: u32 = 3;

/// How long a newer build may wait on this machine — staged, or installed under a
/// running image that is older — before the updater says so out loud with the
/// loud notice (`health_failing_title("apply")`, "aterm can't install updates")
/// and the typed cause (the 2026-09-22/23
/// update audit, plan P1-1(b)).
///
/// One hour is far past the automatic apply ladder's own bound (aterm-gui's
/// `LANDS_WITHIN` plus its `SWITCH_ALLOWANCE` — "within a minute" since the
/// 2026-09-23 retune) and past the physical lane's first retry (600 s); the GUI
/// pins itself against this constant at compile time. An update that has not
/// landed by then is not "waiting for a quiet moment", it is stuck. Before this, nothing read how long a build had waited:
/// 0.90 and 0.91 each sat staged beside an older running build for 20–25 hours,
/// with zero `update-health:` lines in the log, until a person noticed.
/// Cross-platform so the GUI's assert can name it.
pub const PENDING_UPDATE_OVERDUE_SECS: u64 = 60 * 60;

/// Whether the host's AUTOMATIC apply lane is on (`[update] auto_apply`), as the
/// GUI last reported it through [`set_automatic_apply`]. Defaults to on, the
/// shipped default.
static AUTOMATIC_APPLY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Tell the updater whether the host applies staged builds by itself (the
/// 2026-09-22/23 update audit, plan P1-1(b)). The overdue notice
/// ([`PENDING_UPDATE_OVERDUE_SECS`]) is a claim that an update which should
/// have landed on its own has not; with `[update] auto_apply = false` a build
/// waits for a person BY DESIGN, and calling that "auto-update is failing"
/// would be false. So does a lane the host holds on purpose: unsaved work a
/// person has to save, or an in-session handoff the process cannot run, each
/// already said where the person looks. The switch and those postures are the
/// GUI's, not this crate's, so the GUI reports `on` = "lands by itself" here
/// whenever it changes.
pub fn set_automatic_apply(on: bool) {
    AUTOMATIC_APPLY.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// See [`set_automatic_apply`].
#[cfg(target_os = "macos")]
fn automatic_apply_on() -> bool {
    AUTOMATIC_APPLY.load(std::sync::atomic::Ordering::Relaxed)
}

/// Serializes the tests that mutate the PROCESS-GLOBAL "this machine cannot read its
/// release channel" latch (`unreadable::STRANDED`). Cargo runs a crate's
/// tests in parallel threads of ONE process, so without this an assertion about the
/// latch can observe a sibling test's transient state and fail intermittently.
#[cfg(all(test, target_os = "macos"))]
pub(crate) static STRANDED_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A snapshot of the updater's state for the "Check for Updates" menu and the
/// `aterm-ctl update` query. Read from the durable status/ready markers, so it reflects
/// the last background check + any staged build WITHOUT triggering network I/O.
#[derive(Debug, Clone)]
pub struct UpdateStatus {
    pub linux: Option<LinuxUpdateStatus>,
    /// Whether the updater is configured to act on this build/machine.
    pub enabled: bool,
    /// Whether this launch has a bundle the updater could actually REPLACE.
    /// `false` for a run from the mounted DMG, a Gatekeeper-translocated copy, or
    /// a dev-marked install (`bundle::resolve` returns `None`) — states in which
    /// `spawn_background_check` never even starts a thread, so nothing is ever
    /// written to the ledger and the panel would otherwise report the pristine
    /// "You're up to date" of a machine that structurally cannot update
    /// (2026-08-19 round-5 audit).
    pub installable: bool,
    /// The running build number.
    pub current_build: u64,
    /// Build number staged for next-launch apply, if any.
    pub staged_build: Option<u64>,
    /// Human version of the staged build, if any.
    pub staged_version: Option<String>,
    /// Git commit of the staged build's source (from the ready marker, which copies
    /// it from the release manifest at stage time), if known. Lets a controller
    /// compare the staged build against a repo commit before applying.
    pub staged_commit: Option<String>,
    /// Canonical SHA-256 of the staged DMG from the ready marker. Unlike commit
    /// provenance, this identifies the exact artifact bytes authorized for apply.
    pub staged_dmg_sha256: Option<String>,
    /// "What changed" notes for the staged build (from the manifest), if any.
    pub changelog: Option<String>,
    /// The updater's last decision (e.g. `"up to date (latest release build 824)"`).
    pub outcome: String,
    /// RFC3339 UTC time of the last completed-check receipt (empty if absent).
    /// Use [`last_check_at`] to require the current source and running build.
    pub updated_at: String,
    /// SELF-HEALING ledger snapshot: total consecutive failed checks across classes
    /// (0 = healthy).
    pub failing_checks: u32,
    /// Class of the MOST RECENT failure: `"network"` / `"pipeline"` / `"manifest"` /
    /// `"stage"` / `"apply"` / `""`.
    pub failing_kind: String,
    /// Consecutive APPLY-lane failures: a verified build staged but never became
    /// the running build. Broken out of `failing_checks` because it answers a
    /// different question — "can this machine download an update?" versus "can it
    /// actually move to it?" — and because an all-zero acquisition score beside a
    /// non-zero value here is precisely the state that read green for three
    /// releases while every seamless handoff failed.
    pub failing_applies: u32,
    /// RFC3339 UTC start of the current unhealthy period (empty if healthy).
    pub failing_since: String,
    /// Whether the ledger's `pipeline` streak crossed [`PERSISTENT_AFTER`] — the
    /// "this build cannot download while releases are right there" state.
    pub failing_persistent: bool,
    /// Always 0 since v0.26: the independent RESCUE download path (a second fetch
    /// implementation added after the build-826 brick) was deleted as a never-
    /// executed lane. The field itself is retained so the control-socket protocol
    /// line (`aterm-ctl update status` prints `rescues=`) and its consumers stay
    /// byte-stable through the bridge release; it goes with the Phase-2 diet.
    pub rescues: u64,
    /// The class of the STANDING acquisition streak — the one [`Self::failing_checks`]
    /// counts — or `""` when every acquisition streak is clear. Distinct from
    /// [`Self::failing_kind`] (the most recent failure of ANY class, apply included)
    /// on purpose: rendering `failing=<n>:<kind>` from the pair spliced an
    /// acquisition COUNT with an apply LABEL whenever an apply failure landed last,
    /// sending a reader after an acquisition fault that did not exist.
    pub failing_checks_kind: String,
    /// The STRANDED verdict: the last completed check proved this machine cannot
    /// READ its release channel (401/403/404 with nothing to try, or a renamed
    /// repo) and will NEVER update until an operator acts. Deliberately records
    /// ZERO health-ledger failures — a configuration state is not a transient
    /// fault — which is exactly why [`Self::failing_persistent`] can never say it
    /// and surfaces keyed on that field alone headlined "You're up to date." at
    /// permanently stranded machines (round-11 audit). The full explanation, with
    /// the remedy, rides in [`Self::outcome`].
    pub channel_unreadable: bool,
}

impl UpdateStatus {
    /// A single-line human summary (for a menu title / `OK` status line).
    #[must_use]
    pub fn summary(&self) -> String {
        // A Ready marker certifies staged bytes, not permission to apply them.
        // Keep the recorded decision, including refusals with zero failures,
        // instead of replacing it with a readiness claim. Health classes remain
        // the ledger's decision; rendering must not infer one from this text.
        let fallback;
        let text = if !self.outcome.trim().is_empty() {
            self.outcome.trim()
        } else {
            fallback = match (&self.staged_version, self.staged_build) {
                (Some(v), Some(b)) => format!("update {v} (build {b}) staged"),
                _ if !self.enabled => "aterm updates itself on macOS only".to_string(),
                _ => "no update check has run yet".to_string(),
            };
            &fallback
        };
        let mut chars = text.chars();
        let mut line: String = chars
            .by_ref()
            .take(512)
            .map(|c| {
                if c.is_whitespace() || c.is_control() {
                    ' '
                } else {
                    c
                }
            })
            .collect();
        if chars.next().is_some() {
            line.pop();
            line.push('…');
        }
        line
    }

    /// Whether the ledger says update checks are PERSISTENTLY failing on the pipeline
    /// class (releases visible, downloads impossible) — the surface-it-loudly state.
    #[must_use]
    pub fn is_failing_persistently(&self) -> bool {
        self.failing_persistent
    }

    /// An empty (all-defaults) snapshot for the stub/fallback constructors.
    fn empty(enabled: bool, current_build: u64, outcome: String) -> Self {
        Self {
            linux: None,
            enabled,
            // The fallback/stub path has no bundle to speak for; claiming one would
            // be the very over-claim `installable` exists to prevent.
            installable: false,
            current_build,
            staged_build: None,
            staged_version: None,
            staged_commit: None,
            staged_dmg_sha256: None,
            changelog: None,
            outcome,
            updated_at: String::new(),
            failing_checks: 0,
            failing_kind: String::new(),
            failing_applies: 0,
            failing_since: String::new(),
            failing_persistent: false,
            rescues: 0,
            failing_checks_kind: String::new(),
            channel_unreadable: false,
        }
    }
}

/// "Check for updates automatically" is NOT whether the updater runs (2026-09-23
/// review): [`enabled`] is the platform alone, so a typed `aterm update check`, a clicked
/// Check for Updates and Update Now are never refused by the switch — only the background
/// checker ([`automatic`]) is, and it can never be on where the updater cannot run.
#[cfg(test)]
mod switch_scope_tests {
    #[test]
    fn the_settings_switch_gates_the_background_checker_alone() {
        assert_eq!(
            super::enabled(),
            cfg!(any(target_os = "macos", target_os = "linux"))
        );
        assert!(!super::automatic() || super::enabled());
    }
}

#[cfg(all(test, target_os = "macos"))]
mod sibling_stage_tests {
    use super::{StagedNotify, UpdateStatus, announce_sibling_stage};
    use std::sync::{Arc, Mutex};

    fn staged(build: Option<u64>) -> UpdateStatus {
        let mut status = UpdateStatus::empty(true, 81, "another aterm process completed".into());
        status.staged_build = build;
        status.staged_version = build.map(|b| format!("0.{b}.0"));
        status
    }

    /// AU-5: a skip with a newer staged build calls `on_staged` exactly once across
    /// two skips; a later, newer stage is news again. Negative controls: a stage that
    /// is not newer than the running build, no stage, and no hook announce nothing.
    #[test]
    fn a_skip_announces_a_siblings_newer_stage_once_per_build() {
        let calls: Arc<Mutex<Vec<(u64, String)>>> = Arc::default();
        let sink = Arc::clone(&calls);
        let hook: StagedNotify = Box::new(move |build, version| {
            sink.lock().unwrap().push((build, version));
        });
        let mut announced = None;
        let newer = staged(Some(85));
        assert!(announce_sibling_stage(
            &mut announced,
            81,
            Some(&newer),
            Some(&hook)
        ));
        assert!(!announce_sibling_stage(
            &mut announced,
            81,
            Some(&newer),
            Some(&hook)
        ));
        assert_eq!(*calls.lock().unwrap(), vec![(85, "0.85.0".to_string())]);
        // A still newer stage is news.
        let newest = staged(Some(86));
        assert!(announce_sibling_stage(
            &mut announced,
            81,
            Some(&newest),
            Some(&hook)
        ));
        assert_eq!(calls.lock().unwrap().len(), 2);

        // Negative controls.
        let mut fresh = None;
        for (label, current, status) in [
            ("same build as running", 85, staged(Some(85))),
            ("older than running", 90, staged(Some(85))),
            ("nothing staged", 81, staged(None)),
        ] {
            assert!(
                !announce_sibling_stage(&mut fresh, current, Some(&status), Some(&hook)),
                "{label}"
            );
        }
        assert!(
            !announce_sibling_stage(&mut fresh, 81, None, Some(&hook)),
            "no status"
        );
        assert!(
            !announce_sibling_stage(&mut fresh, 81, Some(&newer), None),
            "no hook"
        );
        assert_eq!(fresh, None, "nothing was latched without an announcement");
        assert_eq!(calls.lock().unwrap().len(), 2);
    }
}

#[cfg(test)]
mod status_summary_tests {
    use super::UpdateStatus;

    fn staged(outcome: &str) -> UpdateStatus {
        let mut status = UpdateStatus::empty(true, 81, outcome.to_string());
        status.staged_build = Some(85);
        status.staged_version = Some("0.85.0".to_string());
        status
    }

    #[test]
    fn a_staged_summary_preserves_refusals_and_independent_failure_classes() {
        let refusal = "staged 0.85.0 (build 85) — NOT applied: seamless handoff unavailable because the successor could not be prepared.";
        let cases = [
            (refusal, 0, 0, ""),
            (
                "staged build did not apply: child exited before Ready",
                0,
                1,
                "apply",
            ),
            ("update check failed: network unavailable", 1, 0, "network"),
            (
                "staged 0.85.0 (build 85) — verified and ready to apply",
                0,
                0,
                "",
            ),
        ];
        for (outcome, checks, applies, kind) in cases {
            let mut status = staged(outcome);
            status.failing_checks = checks;
            status.failing_applies = applies;
            status.failing_kind = kind.to_string();
            assert_eq!(status.summary(), outcome);
            assert_eq!(
                (status.failing_checks, status.failing_applies),
                (checks, applies)
            );
            assert_eq!(status.failing_kind, kind);
            assert!(!status.is_failing_persistently());
            // Historical negative control: the stage-first branch concealed
            // every one of these actual decisions behind the same ready line.
            let old = format!(
                "update {} (build {}) staged and ready to apply",
                status.staged_version.as_deref().unwrap(),
                status.staged_build.unwrap()
            );
            assert_ne!(old, outcome);
        }
    }

    #[test]
    fn a_stage_without_a_decision_makes_no_apply_readiness_claim() {
        let mut status = staged(" \n\t");
        status.failing_applies = 1;
        assert_eq!(status.summary(), "update 0.85.0 (build 85) staged");
        status.staged_build = None;
        status.staged_version = None;
        assert_eq!(status.summary(), "no update check has run yet");
        status.enabled = false;
        assert_eq!(status.summary(), "aterm updates itself on macOS only");
    }

    #[test]
    fn summary_is_one_bounded_unicode_line_for_outcomes_and_fallbacks() {
        let status = staged(" apply refused:\nkeep\tthese sessions open\r\n");
        assert_eq!(status.summary(), "apply refused: keep these sessions open");
        let long = format!("apply refused: {}", "猫".repeat(600));
        let line = staged(&long).summary();
        assert!(line.starts_with("apply refused: "));
        assert_eq!(line.chars().count(), 512);
        assert!(line.ends_with('…'));
        assert_eq!(staged(&"猫".repeat(512)).summary(), "猫".repeat(512));
        let mut status = staged("");
        status.staged_version = Some("猫\n".repeat(600));
        let line = status.summary();
        assert_eq!(line.chars().count(), 512);
        assert!(!line.contains('\n'));
        assert!(line.ends_with('…'));
    }
}

/// Canonical equivalence for two git commit stamps drawn from aterm's build metadata.
///
/// The pieces do NOT agree on representation: the release manifest (`aterm-appcast.toml`,
/// `Manifest::commit`) carries the FULL 40-hex commit, while the compiled-in
/// `ATERM_GIT_COMMIT` (`build_info::GIT_COMMIT`) and the bundle's `ATermGitCommit` plist
/// key carry a SHORT 12-hex form — optionally with a `-dirty` suffix. So a plain `==`
/// would report a false mismatch between two stamps of the same commit. This is the ONE
/// place that reconciles them; every commit comparison must route through it rather than
/// re-implementing prefix logic (that footgun is exactly what this closes).
///
/// Equivalence rule, after `trim` + lowercase:
/// * empty, `"unknown"`, a `-dirty` suffix, or any non-hex input ⇒ never matches. A dirty
///   tree diverged from every committed source, so it is honestly *not* that commit — a
///   conservative `false` is the safe answer for "is this build exactly commit X".
/// * otherwise the shorter must be a hex PREFIX of the longer, and be at least 7 chars
///   (git's default abbreviation floor) so a stub can't match everything. Two full hashes
///   reduce to plain equality; a 12-hex short vs a 40-hex full matches iff the 12 lead.
#[must_use]
pub fn commit_matches(a: &str, b: &str) -> bool {
    fn norm(s: &str) -> Option<String> {
        let s = s.trim().to_ascii_lowercase();
        if s.is_empty() || s == "unknown" || s.ends_with("-dirty") {
            return None;
        }
        if !s.bytes().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        Some(s)
    }
    match (norm(a), norm(b)) {
        (Some(x), Some(y)) => {
            let (short, long) = if x.len() <= y.len() {
                (&x, &y)
            } else {
                (&y, &x)
            };
            short.len() >= 7 && long.starts_with(short.as_str())
        }
        _ => false,
    }
}

/// Read the durable updater state (last outcome + any staged build's version/changelog)
/// without any network I/O. `None` only when the staging area can't be resolved (no
/// `HOME`); otherwise returns a snapshot even if no check has run yet.
#[cfg(any(target_os = "macos", test, feature = "spec-anchors"))]
fn persisted_claims_stage(persisted: &str) -> bool {
    // Every phrasing that asserts "a build is staged" must be listed here, or a
    // stale line survives `reconcile_status_outcome` and keeps advertising a
    // stage that no longer exists. "applies on next launch" is RETIRED wording
    // and stays only because a status.toml written by an older build still says
    // it; "ready to apply" is what the stage/backoff lanes write now.
    let persisted = persisted.trim_start();
    persisted.starts_with("staged ")
        || persisted.contains("applies on next launch") // retired-wording detector, not a promise
        || persisted.contains("ready to apply")
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, PartialEq, Eq)]
enum ReconciledStatusOutcome {
    Preserved(String),
    Neutralized(String),
}

#[cfg(any(target_os = "macos", test))]
impl ReconciledStatusOutcome {
    fn into_string(self) -> String {
        match self {
            Self::Preserved(outcome) | Self::Neutralized(outcome) => outcome,
        }
    }
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, PartialEq, Eq)]
struct StatusReconciliation {
    current_build: u64,
    ready_present: bool,
    outcome: ReconciledStatusOutcome,
}

/// Project one observed call of [`reconcile_status_outcome`] onto the bounded
/// `NativeUpdateStatusReconciliation.ReconcileStatus` state transition.
///
/// This is compiled only for proof/test builds. Its inputs are observations of
/// the real reducer (including which enum branch it returned), rather than a
/// second implementation of the reducer's decision rule.
#[cfg(any(test, feature = "spec-anchors"))]
#[doc(hidden)]
#[must_use]
pub fn status_reconciliation_projection(
    running_build: u64,
    checked_from_build: u64,
    ready_present: bool,
    persisted: &str,
    reported_build: u64,
    reported_outcome: &str,
    neutralized: bool,
) -> (
    std::collections::BTreeMap<&'static str, i64>,
    std::collections::BTreeMap<&'static str, i64>,
) {
    let previous = std::collections::BTreeMap::from([
        ("phase", 1),
        (
            "running_build",
            i64::try_from(running_build).expect("bounded running build fits i64"),
        ),
        (
            "ledger_build",
            i64::try_from(checked_from_build).expect("bounded ledger build fits i64"),
        ),
        ("ready_present", i64::from(ready_present)),
        (
            "persisted_staged_claim",
            i64::from(persisted_claims_stage(persisted)),
        ),
        ("reported_build", 0),
        ("reported_staged_claim", 0),
        ("neutralized", 0),
    ]);
    let mut next = previous.clone();
    next.insert("phase", 2);
    next.insert(
        "reported_build",
        i64::try_from(reported_build).expect("bounded reported build fits i64"),
    );
    next.insert(
        "reported_staged_claim",
        i64::from(persisted_claims_stage(reported_outcome)),
    );
    next.insert("neutralized", i64::from(neutralized));
    (previous, next)
}

#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "NativeUpdateStatusReconciliation",
        action = "ReconcileStatus",
        project = "aterm_update::status_reconciliation_projection"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "NativeUpdateStatusReconciliation",
        action = "PickStatusInputs",
        reason = "Bounded nondeterministic environment selection, not a shipping updater transition; Tier-1 exhaustively enumerates every projected input class before driving ReconcileStatus."
    )
)]
#[cfg(any(target_os = "macos", test))]
fn reconcile_status_outcome(
    running_build: u64,
    checked_from_build: u64,
    ready_build: Option<u64>,
    persisted: String,
) -> StatusReconciliation {
    // A marker for the running build (or an older one) is historical residue,
    // not a staged update. Keep this rule in the reconciler so every caller and
    // the Tier-1 projection observe the exact same strict-newer decision.
    let ready_present = ready_build.is_some_and(|build| build > running_build);
    let claims_staged = persisted_claims_stage(&persisted);
    if !ready_present && (checked_from_build != running_build || claims_staged) {
        StatusReconciliation {
            current_build: running_build,
            ready_present,
            outcome: ReconciledStatusOutcome::Neutralized(format!(
                "running build {running_build}; no update is staged"
            )),
        }
    } else {
        StatusReconciliation {
            current_build: running_build,
            ready_present,
            outcome: ReconciledStatusOutcome::Preserved(persisted),
        }
    }
}

/// Whether a failed check takes the quick [`cadence::OFFLINE_RETRY`] rungs: its
/// message says curl could not reach the network ([`cadence::is_network_unreachable`])
/// AND the health ledger filed THIS check's failure (`last_failure_at` at or after
/// `check_started`) as `network` — the channel itself was never reached. The same curl
/// exit on the container download, after the channel answered, is a `pipeline`
/// failure: the network is up, and four quick retries would download the container
/// four more times in eight minutes and, three failures in, show "Updates are failing"
/// in Settings about a slow link.
#[cfg(target_os = "macos")]
fn unreachable_before_the_channel(
    error: &str,
    health: &health::Health,
    check_started: &str,
) -> bool {
    cadence::is_network_unreachable(error)
        && health.kind == "network"
        && health.last_failure_at.as_str() >= check_started
}

/// How much a RECORDED DEFERRAL widens the machine-wide freshness window, as a
/// multiple of the base interval.
///
/// A 429 is measured per IP, so the retreat has to be measured per MACHINE.
/// `Cadence::failed` lengthens the wait of the one process that saw it — but the
/// receipt it leaves behind is judged against every sibling's own un-backed-off base,
/// so without this siblings kept poking the host at full cadence for the whole backoff.
/// One doubling mirrors the first rung of `Cadence`'s ladder, applied to every
/// process rather than to one.
#[cfg(target_os = "macos")]
const DEFERRED_WINDOW_INTERVALS: u32 = 2;

/// How far past a sibling's window a skipping process scatters its own wake (0–60 s,
/// from one entropy byte), so N siblings released by the same receipt do not all check
/// in the same second — and the skew a receipt stamp may sit AHEAD of the clock before
/// it is treated as absent.
#[cfg(target_os = "macos")]
const SKIP_JITTER_SECS: u64 = 60;

/// Why this cycle must NOT spend a network check, if it must not — i.e. whether
/// the source/build-bound receipt records a check completed WITHIN the window, so
/// another aterm process (the window, or a sibling session) has already made this
/// interval's check.
///
/// The window is 70% of the base interval: strictly below the jittered minimum
/// wait (80% of nominal), so a process can never mistake its OWN previous
/// cycle's stamp for another checker's and starve itself.
///
/// Deferrals stamp the completed-check receipt and widen the window by
/// [`DEFERRED_WINDOW_INTERVALS`]: a machine that was just told to slow down must not
/// be re-poked by a sibling on the sibling's own faster timer. The widened window is
/// still bounded — the next healthy check overwrites the receipt and the window returns
/// to the base — so the retreat self-heals exactly as the per-process backoff does.
#[cfg(all(target_os = "macos", test))]
fn checker_skip(staging: &paths::Staging, base: std::time::Duration) -> Option<String> {
    checker_skip_at(staging, base, unix_now_secs()).map(|(reason, _)| reason)
}

/// Where a skipping process points its timer (2026-09-14, audit COORD-5/CC-6): the
/// sibling window's expiry, scattered by 0–[`SKIP_JITTER_SECS`], because every fresh
/// sibling derives the identical epoch from the same receipt and would otherwise wake
/// in the same second.
#[cfg(target_os = "macos")]
fn skip_timer_target(window_expiry: u64, entropy: u8) -> u64 {
    window_expiry.saturating_add(u64::from(entropy) * SKIP_JITTER_SECS / 256)
}

#[cfg(all(target_os = "macos", test))]
fn checker_skip_at(
    staging: &paths::Staging,
    base: std::time::Duration,
    now: u64,
) -> Option<(String, u64)> {
    checker_skip_for(
        staging,
        42,
        &Source {
            owner: "fixture".into(),
            repo: "channel".into(),
        },
        base,
        now,
    )
}

/// [`checker_skip`] for this build and source at an injected `now`: the reason, and the
/// epoch the fresh window ends at (for [`skip_timer_target`]). One clock read and one
/// parse decide both.
#[cfg(target_os = "macos")]
fn checker_skip_for(
    staging: &paths::Staging,
    current_build: u64,
    source: &Source,
    base: std::time::Duration,
    now: u64,
) -> Option<(String, u64)> {
    let text = read_ledger_text(&check_receipt::path(staging))?;
    let v = text.parse::<aterm_toml::Value>().ok()?;
    if !check_receipt::matches(&v, current_build, source) {
        return None;
    }
    // This file is written only after a completed check; status/apply writes
    // cannot refresh its timestamp, deferral or source.
    let checked = v.get("updated_at").and_then(aterm_toml::Value::as_str)?;
    if checked.is_empty() {
        return None;
    }
    // THE WIDENED WINDOW BELONGS TO THE HOST'S BACKOFF, and only the check receipt's
    // own `outcome = "deferred"` says so: the apply lane spells "deferred: install
    // location not writable" and friends for deferrals that touched no network at all,
    // and an admin-owned /Applications used to cost every launch a 42-minute check
    // holiday blamed on a backoff that never happened (2026-09-14).
    let deferred = v
        .get("outcome")
        .and_then(aterm_toml::Value::as_str)
        .is_some_and(|outcome| outcome == "deferred");
    let window = if deferred {
        base.saturating_mul(DEFERRED_WINDOW_INTERVALS)
    } else {
        base
    };
    let fresh_window = window.as_secs().saturating_mul(7) / 10;
    // A stamp AHEAD of the clock is treated as absent (2026-09-14): a clock stepped
    // backwards after the write (an NTP correction of a fast clock) used to hold every
    // checker on the machine for the skew plus the window, and because every loop
    // skipped, nothing overwrote it.
    let checked_epoch = rfc3339_to_unix(checked)?;
    if checked_epoch > now.saturating_add(SKIP_JITTER_SECS) {
        return None;
    }
    if now.saturating_sub(checked_epoch) >= fresh_window {
        return None;
    }
    Some((
        String::from(if deferred {
            "the shared update ledger records a deferred check — this machine is \
             holding off the download host for the rest of the backoff"
        } else {
            "another aterm process completed this interval's update check"
        }),
        checked_epoch.saturating_add(fresh_window),
    ))
}

/// Unix seconds now, `0` when the clock cannot be read (every window then reads as
/// expired, which only releases). The macOS check lane's clock (the checker window and
/// its receipt); the Linux updater keeps its own.
#[cfg(target_os = "macos")]
fn unix_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// How the last check's assets were delivered, from the status ledger: `deferred` (the
/// host asked us to wait) or `blocked` (the download host did not serve an asset the
/// release names), when the last check did not simply succeed. `None` for a healthy
/// check — [`Self::status_line_suffix`] then adds nothing.
///
/// Read separately from [`UpdateStatus`] (rather than as a new field on it) so every
/// consumer that constructs an `UpdateStatus` by hand keeps compiling; the two are
/// read from the same file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Delivery {
    pub note: Option<String>,
}

impl Delivery {
    /// Parse the delivery note out of a `status.toml` text; absent ⇒ `None`.
    #[must_use]
    pub fn from_ledger_text(text: &str) -> Self {
        let note = text.parse::<aterm_toml::Value>().ok().and_then(|v| {
            v.get("delivery")
                .and_then(aterm_toml::Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        });
        Self { note }
    }

    /// The ` delivery=<note>` token for the `aterm ctl update status` line, with a
    /// leading space so the caller can splice it onto the line as-is. Empty for a
    /// healthy check.
    #[must_use]
    pub fn status_line_suffix(&self) -> String {
        self.note
            .as_deref()
            .map(|note| format!(" delivery={note}"))
            .unwrap_or_default()
    }
}

/// The [`Delivery`] facts in this machine's status ledger, or `None` when there is no
/// ledger to read. No network I/O.
#[cfg(target_os = "macos")]
#[must_use]
pub fn delivery() -> Option<Delivery> {
    let staging = paths::Staging::resolve()?;
    let text = read_ledger_text(&staging.status)?;
    Some(Delivery::from_ledger_text(&text))
}

/// Non-macOS stub: no ledger.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn delivery() -> Option<Delivery> {
    None
}

#[cfg(target_os = "macos")]
#[must_use]
pub fn status(current_build: u64) -> Option<UpdateStatus> {
    let staging = paths::Staging::resolve()?;
    // Outcomes come from the any-writer status marker; only the completed-check
    // receipt can supply the last-check timestamp.
    let (mut checked_from_build, mut outcome) = (current_build, String::new());
    let updated_at = check_receipt::completed_at(&staging).unwrap_or_default();
    if let Some(text) = read_ledger_text(&staging.status)
        && let Ok(v) = text.parse::<aterm_toml::Value>()
    {
        // Best-effort (via `u64::try_from` — the verifier cannot lower i64<->u64
        // `as` casts): absent or negative reads keep the running build.
        checked_from_build = v
            .get("current_build")
            .and_then(aterm_toml::Value::as_integer)
            .and_then(|n| u64::try_from(n).ok())
            .unwrap_or(checked_from_build);
        outcome = v
            .get("outcome")
            .and_then(aterm_toml::Value::as_str)
            .unwrap_or("")
            .to_string();
    }
    // Staged build details come from the ready marker (present only when one is
    // staged).
    let mut ready = manifest::Ready::read_publishable(&staging);
    let reconciliation = reconcile_status_outcome(
        current_build,
        checked_from_build,
        ready.as_ref().map(|ready| ready.build_number),
        outcome,
    );
    if !reconciliation.ready_present {
        ready = None;
    }
    let current_build = reconciliation.current_build;
    outcome = reconciliation.outcome.into_string();
    // Self-healing ledger snapshot (per-class failure streaks).
    let h = health::Health::read(&staging.health());
    Some(UpdateStatus {
        linux: None,
        enabled: enabled(),
        installable: bundle::resolve().is_some(),
        current_build,
        staged_build: ready.as_ref().map(|r| r.build_number),
        staged_version: ready.as_ref().map(|r| r.version.clone()),
        staged_commit: ready.as_ref().and_then(|r| r.commit.clone()),
        staged_dmg_sha256: ready.as_ref().map(|r| r.dmg_sha256.clone()),
        changelog: ready.as_ref().and_then(|r| r.changelog.clone()),
        outcome,
        updated_at,
        failing_checks: h.acquisition_failures(),
        failing_persistent: h.is_persistent(),
        failing_checks_kind: h
            .standing_acquisition_class()
            .unwrap_or_default()
            .to_string(),
        failing_kind: h.kind,
        failing_applies: h.apply_failures,
        failing_since: h.failing_since,
        // The rescue lane is gone (v0.26); the protocol field stays, pinned to 0.
        rescues: 0,
        channel_unreadable: unreadable::is_stranded(),
    })
}

/// Timestamp of the last completed check for this source and running build.
/// General status/apply writes cannot refresh it; absent/legacy receipts return
/// `None` rather than advertising a check that has not run on this channel.
#[cfg(target_os = "macos")]
#[must_use]
pub fn last_check_at(current_build: u64, source: &Source) -> Option<String> {
    let staging = paths::Staging::resolve()?;
    let text = read_ledger_text(&check_receipt::path(&staging))?;
    let value: aterm_toml::Value = text.parse().ok()?;
    if !check_receipt::matches(&value, current_build, source) {
        return None;
    }
    value
        .get("updated_at")
        .and_then(aterm_toml::Value::as_str)
        .filter(|stamp| !stamp.is_empty())
        .map(str::to_owned)
}

/// Non-macOS stub: no updater check receipts.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn last_check_at(_current_build: u64, _source: &Source) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        linux::last_check_at(_current_build, _source)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Linux reports its enrolled executable state; other platforms have no updater.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn status(_current_build: u64) -> Option<UpdateStatus> {
    #[cfg(target_os = "linux")]
    {
        Some(linux::status(_current_build))
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Whether `stamp` — an RFC3339 UTC timestamp as this crate writes them
/// ([`UpdateStatus::updated_at`], the health ledger) — is older than `secs` ago.
///
/// The ledger's fixed-shape UTC strings compare chronologically as strings, so
/// this is one lexical comparison against `now - secs` rendered the same way —
/// the idiom `health.rs` already leans on. An empty or oddly-shaped stamp is NOT
/// stale (a never-checked ledger has its own signals, and a malformed one must
/// not raise a scary flag over a formatting difference): anything not starting
/// with an ASCII digit is refused outright.
#[must_use]
pub fn rfc3339_older_than(stamp: &str, secs: u64) -> bool {
    if !stamp.as_bytes().first().is_some_and(u8::is_ascii_digit) {
        return false;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let threshold = aterm_types::rfc3339::format_rfc3339(now.saturating_sub(secs));
    !threshold.is_empty() && *stamp < *threshold
}

/// The unix epoch a ledger stamp names — the exact inverse of
/// [`aterm_types::rfc3339::format_rfc3339`]'s fixed `YYYY-MM-DDTHH:MM:SSZ` shape, and
/// nothing looser: an offset, a fraction or a missing `Z` is `None`. The ledger writes
/// only that shape, and a hold epoch read from anything else would be a guess.
#[must_use]
pub fn rfc3339_to_unix(stamp: &str) -> Option<u64> {
    let bytes = stamp.as_bytes();
    if bytes.len() != 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || bytes[19] != b'Z'
    {
        return None;
    }
    let field = |from: usize, to: usize| -> Option<i64> {
        let text = std::str::from_utf8(&bytes[from..to]).ok()?;
        if !text.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        text.parse::<i64>().ok()
    };
    let (y, m, d) = (field(0, 4)?, field(5, 7)?, field(8, 10)?);
    let (hh, mm, ss) = (field(11, 13)?, field(14, 16)?, field(17, 19)?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || hh > 23 || mm > 59 || ss > 60 {
        return None;
    }
    let days = aterm_types::rfc3339::days_from_civil(y, m, d);
    let secs = days
        .checked_mul(86_400)?
        .checked_add(hh * 3600 + mm * 60 + ss)?;
    u64::try_from(secs).ok()
}

/// Run ONE update check + stage synchronously and return the resulting [`UpdateStatus`]
/// (the "Check for Updates" action). BLOCKS on network + disk (download/verify/stage up
/// to tens of seconds), so callers MUST run it off the UI/event-loop thread. A person
/// asked for it, so "Check for updates automatically" being off does not stop it
/// ([`enabled`], not [`automatic`]); on an uninstalled copy it just reports state.
#[cfg(target_os = "macos")]
pub fn check_now(current_build: u64, source: &Source) -> UpdateStatus {
    if enabled() {
        check_lane().run_or_join(current_build, source, || {
            if let Err(error) = github::check_and_stage(current_build, source) {
                warn(&format!("manual update check failed: {error}"));
            }
        });
    }
    status(current_build).unwrap_or_else(|| {
        UpdateStatus::empty(
            enabled(),
            current_build,
            if enabled() {
                "update check could not run".into()
            } else {
                "aterm updates itself on macOS only".into()
            },
        )
    })
}

/// Non-macOS stub.
#[cfg(not(target_os = "macos"))]
pub fn check_now(current_build: u64, _source: &Source) -> UpdateStatus {
    #[cfg(target_os = "linux")]
    {
        linux::check_now(current_build, _source)
    }
    #[cfg(not(target_os = "linux"))]
    {
        UpdateStatus::empty(
            false,
            current_build,
            "auto-update is unsupported on this platform".into(),
        )
    }
}

/// Emit an informational updater line to stderr (captured by the GUI's logger).
/// Kept deliberately low-volume: silent operation means most runs print nothing.
/// Routed through `aterm_log` (the global logger `aterm-gui` installs before the
/// updater runs), so it lands in the app log FILE — visible for a Finder-launched
/// `.app`, unlike stderr. A no-op if no logger is installed (e.g. a dev harness).
#[cfg(target_os = "macos")]
pub(crate) fn log(msg: &str) {
    #[cfg(test)]
    log_capture::record(aterm_log::Level::Info, msg);
    aterm_log::info!("aterm-update: {msg}");
}

/// Emit an updater line at DEBUG (see [`log`]): a routine fact kept out of the default
/// log — the lane this process reads, the machine that signed a release it has already
/// named once.
#[cfg(target_os = "macos")]
pub(crate) fn debug(msg: &str) {
    #[cfg(test)]
    log_capture::record(aterm_log::Level::Debug, msg);
    aterm_log::debug!("aterm-update: {msg}");
}

/// Whether `key` is new to this process — for a line said once per process (or once
/// per release per process) however many checks meet the same fact. Under `cfg(test)`
/// the memory is per THREAD, like [`log_capture`], so a test's first sighting is not
/// swallowed by a sibling test that met the same fixture first.
#[cfg(target_os = "macos")]
pub(crate) fn first_in_process(key: &str) -> bool {
    #[cfg(not(test))]
    {
        static SEEN: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        let mut seen = SEEN
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if seen.iter().any(|k| k == key) {
            return false;
        }
        seen.push(key.to_string());
        true
    }
    #[cfg(test)]
    {
        thread_local! {
            static SEEN: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
        }
        SEEN.with(|seen| {
            let mut seen = seen.borrow_mut();
            if seen.iter().any(|k| k == key) {
                return false;
            }
            seen.push(key.to_string());
            true
        })
    }
}

/// Emit a non-fatal updater warning to the app log (see [`log`]).
#[cfg(target_os = "macos")]
pub(crate) fn warn(msg: &str) {
    #[cfg(test)]
    log_capture::record(aterm_log::Level::Warn, msg);
    aterm_log::warn!("aterm-update: {msg}");
}

/// Test-only capture of the updater's own log lines, WITH THEIR LEVEL.
///
/// Some of the updater's log contracts are a level, not just a wording — above all the
/// roster-authorized rotation note, which must be INFO exactly once (a WARN there taught
/// every pristine post-rotation install to distrust its own verified signature, see
/// `github::fetch_authoritative_release`). `aterm_log`'s global logger cannot pin that
/// per-test — it is process-wide, install-once, and shared by every parallel test thread
/// — so [`log`]/[`warn`] feed a thread-local here under `cfg(test)`: each test observes
/// exactly the lines its own thread emitted, race-free.
#[cfg(all(test, target_os = "macos"))]
pub(crate) mod log_capture {
    use std::cell::RefCell;

    thread_local! {
        static LINES: RefCell<Vec<(aterm_log::Level, String)>> = const { RefCell::new(Vec::new()) };
    }

    pub(crate) fn record(level: aterm_log::Level, msg: &str) {
        LINES.with(|lines| lines.borrow_mut().push((level, msg.to_string())));
    }

    /// Drain this thread's captured lines. Call once BEFORE the action under test to
    /// clear residue from earlier code on the same thread, and again after to read.
    pub(crate) fn take() -> Vec<(aterm_log::Level, String)> {
        LINES.with(|lines| std::mem::take(&mut *lines.borrow_mut()))
    }
}

// The cadence module is macOS-only (the updater lane ships there), and so is the
// health ledger this reads; the module compiles only where they exist.
#[cfg(all(test, target_os = "macos"))]
mod checker_gate_tests {
    use super::unreachable_before_the_channel;

    /// THE QUICK RUNGS ARE FOR A CHANNEL THAT WAS NEVER REACHED. The boot failure the
    /// owner's log recorded — a DNS error on the HEAD, filed `network` by this check —
    /// takes them. The same curl exit on the container download (filed `pipeline`: the
    /// channel answered first), a `network` record an EARLIER check left, and a
    /// `network` failure that is not a transport exit do not.
    #[test]
    fn only_this_checks_unreachable_channel_takes_the_quick_rungs() {
        let dns = "curl HEAD https://github.com/alabsystems/aterm/releases/latest/download/\
                   aterm-appcast.toml failed (exit status: 6): curl: (6) Could not resolve \
                   host: github.com";
        let stalled = "zip download failed: curl download failed (exit status: 28): curl: \
                       (28) Operation too slow. Less than 4096 bytes/sec transferred the last \
                       30 seconds";
        let started = "2026-09-23T12:28:12Z";
        let filed = |kind: &str, at: &str| crate::health::Health {
            kind: kind.to_string(),
            last_failure_at: at.to_string(),
            ..Default::default()
        };
        assert!(unreachable_before_the_channel(
            dns,
            &filed("network", started),
            started
        ));
        assert!(unreachable_before_the_channel(
            dns,
            &filed("network", "2026-09-23T12:28:15Z"),
            started
        ));
        assert!(
            !unreachable_before_the_channel(stalled, &filed("pipeline", started), started),
            "a stalled container download is not a network that is down"
        );
        assert!(
            !unreachable_before_the_channel(
                dns,
                &filed("network", "2026-09-23T12:18:12Z"),
                started
            ),
            "an earlier check's record says nothing about this one"
        );
        assert!(!unreachable_before_the_channel(
            "parse appcast: EOF while parsing",
            &filed("network", started),
            started
        ));
        assert!(!unreachable_before_the_channel(
            dns,
            &crate::health::Health::default(),
            started
        ));
    }
}

#[cfg(all(test, target_os = "macos"))]
mod checker_skip_tests {
    use std::time::Duration;

    use super::{
        SKIP_JITTER_SECS, checker_skip, checker_skip_at, paths::Staging, skip_timer_target,
    };

    fn staging(name: &str) -> Staging {
        Staging::scratch(&format!("checker-{name}"))
    }

    fn now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_secs()
    }

    /// A check receipt exactly as `check_receipt::record` writes one: `outcome` is
    /// `completed` or `deferred`.
    fn write_receipt(s: &Staging, age_secs: u64, outcome: &str) {
        let stamp = aterm_types::rfc3339::format_rfc3339(now().saturating_sub(age_secs));
        std::fs::write(
            super::check_receipt::path(s),
            format!(
                "schema = 1\ncurrent_build = 42\nsource = \"fixture/channel\"\n\
                 updated_at = \"{stamp}\"\noutcome = \"{outcome}\"\n"
            ),
        )
        .expect("write receipt");
    }

    const BASE: Duration = Duration::from_secs(30 * 60);

    /// A SKIPPING PROCESS SLEEPS TO THE WINDOW'S END (2026-09-14, audit COORD-5 /
    /// CC-6): a sibling's fresh check hands the skipper the expiry of the window it is
    /// honouring — `checked_at + 0.7 × base` — scattered by 0–60 s so N fresh siblings
    /// do not wake in the same second.
    #[test]
    fn a_skipping_process_points_its_timer_at_the_windows_end() {
        let s = staging("window-end");
        write_receipt(&s, 60, "completed");
        let now = now();
        let (_, expiry) = checker_skip_at(&s, BASE, now).expect("a fresh stamp skips");
        let fresh = BASE.as_secs() * 7 / 10;
        assert!(
            ((now - 60 + fresh - 1)..=(now - 60 + fresh + 1)).contains(&expiry),
            "checked_at + 0.7 × base: {expiry} vs now={now}"
        );
        assert_eq!(
            skip_timer_target(expiry, 0),
            expiry,
            "no entropy, no scatter"
        );
        let scattered = skip_timer_target(expiry, 255);
        assert!(
            (expiry..expiry + SKIP_JITTER_SECS).contains(&scattered),
            "the scatter is under a minute: {scattered} vs {expiry}"
        );
        let _ = std::fs::remove_dir_all(&s.root);
    }

    #[test]
    fn a_fresh_stamp_skips_and_a_stale_one_checks() {
        let s = staging("fresh");
        write_receipt(&s, 60, "completed");
        assert!(
            checker_skip(&s, BASE).is_some(),
            "a sibling checked a minute ago — this cycle owes the host nothing"
        );
        // 70% of 30 min is 21 min; 25 minutes is past it.
        write_receipt(&s, 25 * 60, "completed");
        assert!(
            checker_skip(&s, BASE).is_none(),
            "past the freshness window the check is this process's to make"
        );
        let _ = std::fs::remove_dir_all(&s.root);
    }

    /// THE RETREAT IS MEASURED PER MACHINE, because a 429 is. Without this a sibling
    /// on its own un-backed-off timer re-poked the host at full cadence for the whole
    /// backoff, so a machine that had just been told to slow down never actually did.
    #[test]
    fn a_recorded_deferral_holds_off_every_process_not_just_the_one_that_saw_it() {
        let s = staging("deferred");
        write_receipt(&s, 25 * 60, "deferred");
        let reason = checker_skip(&s, BASE).expect("the deferral widens the window");
        assert!(reason.contains("deferred"), "{reason}");
        // Bounded, not permanent: past the widened window (70% of 2×base = 42 min) the
        // machine tries again, and one healthy check restores the base window.
        write_receipt(&s, 50 * 60, "deferred");
        assert!(checker_skip(&s, BASE).is_none(), "the retreat self-heals");
        write_receipt(&s, 25 * 60, "completed");
        assert!(
            checker_skip(&s, BASE).is_none(),
            "a healthy check returns the window to the base interval"
        );
        // NEGATIVE CONTROL: only the receipt's own `deferred` widens. An apply-lane
        // sentence that merely CONTAINS "deferred" (it touched no network) does not.
        write_receipt(&s, 25 * 60, "deferred: install location not writable");
        assert!(checker_skip(&s, BASE).is_none());
        let _ = std::fs::remove_dir_all(&s.root);
    }

    #[test]
    fn a_missing_or_empty_ledger_never_defers_a_check() {
        let s = staging("missing");
        assert!(checker_skip(&s, BASE).is_none(), "no ledger: check");
        std::fs::write(
            super::check_receipt::path(&s),
            "schema = 1\ncurrent_build = 42\nsource = \"fixture/channel\"\nupdated_at = \"\"\n",
        )
        .expect("write");
        assert!(checker_skip(&s, BASE).is_none(), "empty stamp: check");
        std::fs::write(super::check_receipt::path(&s), "not toml at all {{{").expect("write");
        assert!(checker_skip(&s, BASE).is_none(), "unparseable: check");
        let _ = std::fs::remove_dir_all(&s.root);
    }

    /// The `delivery=` token appears ONLY when the ledger recorded a note, so a healthy
    /// line carries nothing.
    #[test]
    fn the_status_line_carries_delivery_only_when_recorded() {
        use super::Delivery;
        assert_eq!(
            Delivery::from_ledger_text("schema = 1\noutcome = \"up to date\"\n")
                .status_line_suffix(),
            ""
        );
        assert_eq!(
            Delivery::from_ledger_text("not toml {{{").status_line_suffix(),
            ""
        );
        for note in ["deferred", "blocked"] {
            assert_eq!(
                Delivery::from_ledger_text(&format!("schema = 1\ndelivery = \"{note}\"\n"))
                    .status_line_suffix(),
                format!(" delivery={note}")
            );
        }
    }

    #[test]
    fn rfc3339_round_trips_through_the_ledgers_own_shape_and_refuses_looser_ones() {
        for secs in [0u64, 1_788_392_970, 4_102_444_800] {
            let stamp = aterm_types::rfc3339::format_rfc3339(secs);
            assert_eq!(super::rfc3339_to_unix(&stamp), Some(secs), "{stamp}");
        }
        for bad in [
            "",
            "2026-09-03T00:00:00",
            "2026-09-03T00:00:00+00:00",
            "2026-09-03T00:00:00.000Z",
            "2026-13-03T00:00:00Z",
            "2026-09-03T24:00:00Z",
            "2026-09-0xT00:00:00Z",
            "not a stamp at all!",
        ] {
            assert_eq!(super::rfc3339_to_unix(bad), None, "{bad:?}");
        }
    }
}

#[cfg(test)]
mod delivery_story_tests {
    /// The crate's own metadata and its module doc teach the in-session apply
    /// lane as the mechanism and the next-launch swap as the fallback. The
    /// description is what `targo metadata`, trustdoc and every crate listing
    /// show; the module doc is what the next edit copies from — and both said
    /// "swaps the .app on next launch" / "stages it for the *next* launch" /
    /// "at the top of the next `main()` (always works)" until 2026-08-30, over a
    /// lane that had been the mechanism since Rung 1b. Twin of grep_guard's B11,
    /// which sees the handbooks this file cannot.
    #[test]
    fn the_crate_teaches_the_in_session_apply_lane_not_the_next_launch_swap() {
        let description = env!("CARGO_PKG_DESCRIPTION");
        assert!(
            description.contains("in place") && description.contains("fallback"),
            "{description}"
        );
        assert!(
            !description.contains("on next launch"),
            "the description teaches the fallback as the mechanism: {description}"
        );
        let module_doc: Vec<&str> = include_str!("lib.rs")
            .lines()
            .skip_while(|line| !line.starts_with("//!"))
            .take_while(|line| line.starts_with("//!"))
            .collect();
        let module_doc = module_doc.join("\n");
        for stale in [
            "stages it for the *next* launch",
            "(always works)",
            "one launch behind",
            "the swap still waits for a `main()`",
        ] {
            assert!(
                !module_doc.contains(stale),
                "the module doc still teaches {stale:?}"
            );
        }
        assert!(
            module_doc.contains("seamless overlap handoff") && module_doc.contains("fallback"),
            "the module doc names the mechanism and the fallback"
        );
    }
}

#[cfg(test)]
mod rfc3339_age_tests {
    use super::rfc3339_older_than;

    #[test]
    fn empty_and_malformed_stamps_are_never_stale() {
        // "never checked" and "someone hand-edited the ledger" both have their own
        // signals; a scary staleness flag must not be one formatting slip away.
        assert!(!rfc3339_older_than("", 60));
        assert!(!rfc3339_older_than("never", 60));
        assert!(!rfc3339_older_than("-", 60));
    }

    #[test]
    fn ancient_stamps_are_stale_and_future_stamps_are_not() {
        assert!(rfc3339_older_than("2001-01-01T00:00:00Z", 60));
        assert!(!rfc3339_older_than("2999-01-01T00:00:00Z", 60));
    }
}

#[cfg(test)]
mod commit_match_tests {
    use std::collections::BTreeSet;

    use super::{
        ReconciledStatusOutcome, StatusReconciliation, commit_matches, compiled_update_pin_sha256,
        persisted_claims_stage, reconcile_status_outcome, status_reconciliation_projection,
        update_pubkey_sha256,
    };
    // The predicate under test is itself macOS-gated (the notice belongs to the
    // updater, which has no other lane) — the import and its test ride the same cfg.
    #[cfg(target_os = "macos")]
    use super::persistent_notice_is_owed;

    #[test]
    fn update_pin_fingerprint_hashes_decoded_key_and_fails_closed() {
        let encoded = aterm_codec::base64::encode(&[0_u8; 32]).unwrap();
        assert_eq!(
            update_pubkey_sha256(&encoded).unwrap().as_deref(),
            Some("66687aadf862bd776c8fc18b8e9f8e20089714856ee233b3902a591d0d5f2925")
        );
        assert_eq!(update_pubkey_sha256("").unwrap(), None);
        assert!(update_pubkey_sha256("not-base64").is_err());
        let short = aterm_codec::base64::encode(&[0_u8; 31]).unwrap();
        assert!(update_pubkey_sha256(&short).is_err());

        // THE update pin is the paper master's fingerprint — armed in this tree, so 64
        // hex, and exactly the master head's.
        let compiled = compiled_update_pin_sha256();
        assert!(
            compiled.len() == 64 && compiled.bytes().all(|b| b.is_ascii_hexdigit()),
            "the armed master prints its fingerprint: {compiled}"
        );
        assert_eq!(
            compiled,
            update_pubkey_sha256(aterm_update_core::pins::PAPER_MASTER_PUBKEYS[0])
                .unwrap()
                .unwrap()
        );
    }

    fn observed_outcome(reconciliation: &StatusReconciliation) -> (&str, bool) {
        match &reconciliation.outcome {
            ReconciledStatusOutcome::Preserved(outcome) => (outcome, false),
            ReconciledStatusOutcome::Neutralized(outcome) => (outcome, true),
        }
    }

    #[test]
    fn status_reconciliation_exhaustively_refines_the_model() {
        let model = aterm_spec::derive::native_update_status_reconciliation_model();
        let reachable_inputs: BTreeSet<_> = model
            .successors("PickStatusInputs", &model.init_state())
            .into_iter()
            .collect();
        assert_eq!(
            reachable_inputs.len(),
            16,
            "2 running × 2 ledger × 2 Ready × 2 persisted classes"
        );

        let persisted_cases = [
            ("staged 0.2 (build 2) — applies on next launch", true),
            ("network check failed", false),
        ];
        // None plus older/equal/newer concrete Ready markers. Some(3) is needed
        // to exercise a strictly-newer marker for running build 2; the model
        // abstracts all concrete marker builds to `ready_present ∈ {0,1}`.
        let ready_builds = [None, Some(1), Some(2), Some(3)];
        let mut projected_inputs = BTreeSet::new();
        let mut calls = 0usize;

        for running_build in 1..=2 {
            for checked_from_build in 1..=2 {
                for ready_build in ready_builds {
                    for (persisted, staged_claim) in persisted_cases {
                        assert_eq!(persisted_claims_stage(persisted), staged_claim);
                        let reconciliation = reconcile_status_outcome(
                            running_build,
                            checked_from_build,
                            ready_build,
                            persisted.to_string(),
                        );
                        let expected_ready = ready_build.is_some_and(|build| build > running_build);
                        assert_eq!(
                            reconciliation.current_build, running_build,
                            "the ledger may never relabel its caller"
                        );
                        assert_eq!(
                            reconciliation.ready_present, expected_ready,
                            "Ready must be effective iff its build is strictly newer"
                        );

                        let expected_neutralized = !expected_ready
                            && (checked_from_build != running_build || staged_claim);
                        let (reported_outcome, neutralized) = observed_outcome(&reconciliation);
                        assert_eq!(neutralized, expected_neutralized);
                        if expected_neutralized {
                            assert_eq!(
                                reported_outcome,
                                format!("running build {running_build}; no update is staged")
                            );
                        } else {
                            assert_eq!(reported_outcome, persisted);
                        }

                        let (previous, next) = status_reconciliation_projection(
                            running_build,
                            checked_from_build,
                            reconciliation.ready_present,
                            persisted,
                            reconciliation.current_build,
                            reported_outcome,
                            neutralized,
                        );
                        assert!(
                            reachable_inputs.contains(&previous),
                            "real input projection is unreachable: {previous:?}"
                        );
                        projected_inputs.insert(previous.clone());

                        let label = format!(
                            "status reconciliation running={running_build} ledger={checked_from_build} ready={ready_build:?} staged={staged_claim}"
                        );
                        let (admitted, why) = aterm_spec::verify::validate_transition_tiered(
                            &model,
                            &[],
                            &previous,
                            &next,
                            Some("ReconcileStatus"),
                            &label,
                        );
                        assert!(admitted, "real reducer transition rejected: {why}");
                        for invariant in &model.invariants {
                            assert!(
                                model.check_invariant(invariant.name, &next),
                                "real reducer violated {}: {next:?}",
                                invariant.name
                            );
                        }
                        calls += 1;
                    }
                }
            }
        }

        assert_eq!(
            calls, 32,
            "the concrete decision lattice must stay exhaustive"
        );
        assert_eq!(
            projected_inputs, reachable_inputs,
            "Tier-1 must cover every bounded PickStatusInputs class"
        );
    }

    #[test]
    fn status_reconciliation_model_negative_controls_are_non_vacuous() {
        let model = aterm_spec::derive::native_update_status_reconciliation_model();
        let buggy = aterm_spec::interp::with_buggy(&model, 1);

        // Historical defect 1: a mismatched ledger relabels the running caller.
        let (caller_previous, caller_bug) = status_reconciliation_projection(
            1,
            2,
            false,
            "network check failed",
            2,
            "network check failed",
            false,
        );
        let (admitted, why) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            &caller_previous,
            &caller_bug,
            Some("ReconcileStatus"),
            "status reconciliation caller-build negative control",
        );
        assert!(!admitted, "healthy model admitted caller relabeling: {why}");
        assert!(
            buggy
                .successors("ReconcileStatus", &caller_previous)
                .contains(&caller_bug),
            "Buggy=1 must reproduce caller relabeling"
        );
        assert!(!buggy.check_invariant("CallerBuildIsAuthoritative", &caller_bug));

        // Historical defect 2: mere marker presence (here equal, not newer) is
        // treated as an active stage and preserves stale staged prose.
        let persisted = "staged 0.2 (build 2) — applies on next launch";
        let real = reconcile_status_outcome(2, 2, Some(2), persisted.to_string());
        assert!(
            !real.ready_present,
            "an equal-build Ready marker must be treated as absent"
        );
        let (ready_previous, _) = status_reconciliation_projection(
            2,
            2,
            real.ready_present,
            persisted,
            real.current_build,
            observed_outcome(&real).0,
            observed_outcome(&real).1,
        );
        let (_, ready_bug) =
            status_reconciliation_projection(2, 2, false, persisted, 2, persisted, false);
        let (admitted, why) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            &ready_previous,
            &ready_bug,
            Some("ReconcileStatus"),
            "status reconciliation strict-newer negative control",
        );
        assert!(!admitted, "healthy model admitted stale Ready prose: {why}");
        assert!(
            buggy
                .successors("ReconcileStatus", &ready_previous)
                .contains(&ready_bug),
            "Buggy=1 must reproduce the mere-presence defect"
        );
        assert!(!buggy.check_invariant("AbsentReadyCannotAdvertiseStage", &ready_bug));
    }

    #[test]
    fn status_reconciliation_shipping_anchor_is_linked() {
        let refinements: Vec<_> = aterm_spec::xref::refinements()
            .filter(|anchor| anchor.machine == "NativeUpdateStatusReconciliation")
            .collect();
        assert_eq!(refinements.len(), 1);
        assert_eq!(refinements[0].action, "ReconcileStatus");
        assert_eq!(refinements[0].rust_method, "reconcile_status_outcome");
        assert_eq!(
            refinements[0].project,
            "aterm_update::status_reconciliation_projection"
        );

        let input_waivers: Vec<_> = aterm_spec::xref::waivers()
            .filter(|waiver| waiver.machine == "NativeUpdateStatusReconciliation")
            .collect();
        assert_eq!(input_waivers.len(), 1);
        assert_eq!(input_waivers[0].action, "PickStatusInputs");
        assert_eq!(input_waivers[0].rust_method, "reconcile_status_outcome");
    }

    #[test]
    fn status_reconciliation_never_reports_an_old_build_or_absent_stage() {
        let cases = [
            // The reproduced v0.53 -> v0.54 post-swap ledger.
            (
                54,
                53,
                None,
                "staged 0.54 (build 54) — applies on next launch",
                false,
            ),
            // An overlapping OLD process reading the NEW activation ledger.
            (
                53,
                54,
                None,
                "installed 0.54 (build 54); activating now",
                false,
            ),
            // Same-build honest terminal outcomes remain useful.
            (54, 54, None, "up to date (latest release build 54)", true),
            (54, 54, None, "network check failed", true),
            // A complete ready marker remains the authority for staged details.
            (
                53,
                52,
                Some(54),
                "staged 0.54 (build 54) — applies on next launch",
                true,
            ),
            // Even a same-build stale stage sentence is suppressed when Ready is gone.
            (
                53,
                53,
                None,
                "staged 0.54 (build 54) — applies on next launch",
                false,
            ),
            // The wording the stage/backoff lanes write NOW. It makes the same
            // claim as the retired "applies on next launch" phrasing, and the
            // claim — not the phrasing — is what must be suppressed once the
            // marker is gone; otherwise renaming the line would have quietly
            // resurrected stale staged prose on every machine.
            (
                53,
                53,
                None,
                "skipping re-stage of build 54 for another 5m; NOT skipping apply: \
                 staged 0.54 (build 54) is verified and ready to apply",
                false,
            ),
        ];
        for (running, ledger, ready, persisted, preserve) in cases {
            let reconciliation =
                reconcile_status_outcome(running, ledger, ready, persisted.to_string());
            assert_eq!(reconciliation.current_build, running);
            let got = reconciliation.outcome.into_string();
            if preserve {
                assert_eq!(got, persisted);
            } else {
                assert_eq!(got, format!("running build {running}; no update is staged"));
                assert!(!got.contains("applies on next launch"));
            }
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn ledger_reader_rejects_oversized_and_non_regular_inputs() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-update-ledger-cap-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let valid = dir.join("valid.toml");
        std::fs::write(&valid, b"enabled = true\n").unwrap();
        assert_eq!(
            super::read_ledger_text(&valid).as_deref(),
            Some("enabled = true\n")
        );

        let oversized = dir.join("oversized.toml");
        let file = std::fs::File::create(&oversized).unwrap();
        file.set_len(super::MAX_LEDGER_BYTES + 1).unwrap();
        assert!(super::read_ledger_text(&oversized).is_none());
        assert!(super::read_ledger_text(&dir).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn short_binary_stamp_matches_full_manifest_hash() {
        // The real case the caveat is about: 12-hex `GIT_COMMIT` vs 40-hex appcast commit.
        let short = "4e91d3334041";
        let full = "4e91d3334041788ad92f5b6568cb648cb25805d6";
        assert!(commit_matches(short, full));
        assert!(commit_matches(full, short), "order must not matter");
    }

    #[test]
    fn case_and_whitespace_insensitive() {
        assert!(commit_matches(
            "  4E91D3334041 ",
            "4e91d3334041788ad92f5b6568cb648cb25805d6"
        ));
    }

    #[test]
    fn identical_full_hashes_match() {
        let full = "4e91d3334041788ad92f5b6568cb648cb25805d6";
        assert!(commit_matches(full, full));
    }

    #[test]
    fn different_commits_do_not_match() {
        assert!(!commit_matches(
            "4e91d3334041",
            "deadbeefcafe0000000000000000000000000000"
        ));
    }

    #[test]
    fn dirty_never_matches_even_on_the_same_base() {
        // A dirty build is not reproducibly its base commit → conservative non-match.
        let full = "4e91d3334041788ad92f5b6568cb648cb25805d6";
        assert!(!commit_matches("4e91d3334041-dirty", full));
        assert!(!commit_matches("4e91d3334041-dirty", "4e91d3334041-dirty"));
    }

    #[test]
    fn unknown_empty_nonhex_and_the_dash_placeholder_never_match() {
        let full = "4e91d3334041788ad92f5b6568cb648cb25805d6";
        assert!(!commit_matches("unknown", full));
        assert!(!commit_matches("", full));
        assert!(!commit_matches("-", full)); // the control-socket "no staged commit" sentinel
        assert!(!commit_matches("nothexatall12", full));
    }

    #[test]
    fn too_short_a_prefix_is_rejected() {
        // 6 hex chars is below git's abbreviation floor — refuse to call it a match.
        assert!(!commit_matches(
            "4e91d3",
            "4e91d3334041788ad92f5b6568cb648cb25805d6"
        ));
        // 7 is accepted.
        assert!(commit_matches(
            "4e91d33",
            "4e91d3334041788ad92f5b6568cb648cb25805d6"
        ));
    }

    /// A SECOND persistent class must still be able to speak. The latch used to be one
    /// unkeyed bool, so a machine that could not download (`pipeline`) and later also
    /// could not APPLY what it had staged told the user about the first fault only —
    /// for however long that process ran.
    ///
    /// MUTATION: make the predicate `announced.is_none()` (the old bool) and the
    /// different-class assertion fails.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_second_persistent_class_still_speaks_but_the_same_one_stays_quiet() {
        // Nothing escalating: nothing to say, whatever was announced before.
        assert!(!persistent_notice_is_owed(None, None));
        assert!(!persistent_notice_is_owed(Some("pipeline"), None));
        // The first escalation always speaks.
        assert!(persistent_notice_is_owed(None, Some("pipeline")));
        // The SAME class on the next tick is the "once per streak" promise: the loud
        // notice must not become a nag.
        assert!(!persistent_notice_is_owed(
            Some("pipeline"),
            Some("pipeline")
        ));
        // A DIFFERENT class is news. THIS is the regression.
        assert!(persistent_notice_is_owed(Some("pipeline"), Some("apply")));
        // …and having spoken about it, it goes quiet too.
        assert!(!persistent_notice_is_owed(Some("apply"), Some("apply")));
    }
}

#[cfg(test)]
mod team_pin_tests {
    /// The runtime opt-in must be ONE-WAY, and a compiled pin is ABSOLUTE. This tree
    /// pins the real Team ID (Tier APPLE armed 2026-08-15, replacing the ad-hoc-era
    /// version of this test), so the pin decides and every runtime call — blank,
    /// different, anything — is a no-op: a settings file must not be a verification
    /// bypass, and it also must not be able to swap the team a build was armed with.
    #[test]
    fn the_runtime_team_requirement_can_only_tighten() {
        assert_eq!(
            super::PINNED_TEAM_ID,
            "A66A9P66Z7",
            "Tier APPLE is armed; an empty pin changes this test's meaning"
        );
        assert_eq!(super::effective_team_id(), "A66A9P66Z7", "the pin decides");

        // Every runtime call is inert against a compiled pin.
        super::set_required_team_id(None);
        super::set_required_team_id(Some("   "));
        super::set_required_team_id(Some("ZZZZZ99999"));
        assert_eq!(
            super::effective_team_id(),
            "A66A9P66Z7",
            "a settings file can neither relax nor replace a compiled pin"
        );
    }
}
