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
//!   verifies it, and stages it for the *next* launch. It never touches the UI
//!   and never blocks the event loop.
//!
//! # Delivery model: what actually reaches a machine, and when
//!
//! Read this before adding a scheduler. The updater has exactly two moving parts,
//! and the honest bound on "how stale can a machine be" follows from them:
//!
//! * **Staging** happens on [`spawn_background_check`]'s thread, which runs its
//!   FIRST check immediately at launch and then on the `cadence` schedule. So a
//!   running app stages a new release within ~a minute of publish; an app that is
//!   started stages within seconds of start.
//! * **Applying** happens either at the top of the next `main()` (always works), or
//!   in-session through the seamless overlap handoff. The in-session lane is
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
//! never *needs* to be — and the first launch after a release stages it, so the
//! machine is at worst **one launch behind**, not indefinitely stale.
//!
//! ## Why there is no LaunchAgent
//!
//! An obvious proposal is a `launchd` agent that checks periodically so a machine
//! is current before it is even launched. It was considered and REJECTED, for
//! reasons that are about cost and honesty, not taste:
//!
//! 1. **It would buy exactly one launch.** Per the bound above, the agent's only
//!    effect is that a release stages before the launch rather than during it —
//!    the swap still waits for a `main()`. It cannot update an app nobody runs,
//!    because applying an update means re-execing a process.
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
//! * **Tier REPO (default, no secret).** Trust is "it came from my authenticated
//!   PRIVATE GitHub repo over TLS", plus (2). The `.app` must still pass a *structural*
//!   `codesign --verify` (an ad-hoc signature suffices — arm64 requires one to run) so
//!   corruption/tamper is caught, but **no Apple anchor / Team ID / notarization is
//!   required**. This is the internal-distribution baseline.
//! * **Tier SIG (a signing key — Apple-free cryptographic authenticity).** If a public
//!   key is compiled in ([`PINNED_UPDATE_PUBKEY`] from `ATERM_UPDATE_PUBKEY`), every
//!   release manifest MUST carry a valid Ed25519 signature verifiable against it (the
//!   offline private key lives in CI secrets / offline). Since the manifest pins the
//!   sha256 of every downloadable container (DMG and zip), this authenticates the
//!   artifact whichever one is staged, even against a repo-write attacker —
//!   with no Apple Developer ID. Same primitive `atpkg` pins (see [`sig`]).
//! * **Tier APPLE (a Developer ID — optional, additive).** If [`PINNED_TEAM_ID`] is
//!   set, `codesign --verify` also runs with a designated requirement (`-R`) pinning
//!   the Apple anchor + Developer-ID chain + Team ID (Gatekeeper-independent), plus
//!   `spctl -a -t exec` notarization.
//!
//! All *configured* anchors must pass (defense in depth). Everything shells out to
//! `codesign`/`spctl`/`hdiutil`/`ditto`/`curl`/`shasum`; the only crypto is `ring`,
//! used ONLY for the optional Tier SIG manifest check.

#[cfg(target_os = "macos")]
mod bundle;
#[cfg(target_os = "macos")]
mod cadence;
#[cfg(target_os = "macos")]
mod github;
#[cfg(target_os = "macos")]
mod health;
#[cfg(target_os = "macos")]
mod install;
#[cfg(target_os = "macos")]
mod manifest;
#[cfg(target_os = "macos")]
mod no_token;
#[cfg(target_os = "macos")]
mod paths;
// Not macOS-only: every platform relaunches aterm (see the module's own doc).
mod relaunch;
#[cfg(target_os = "macos")]
mod sig;
#[cfg(target_os = "macos")]
mod status;
#[cfg(target_os = "macos")]
mod sys;
#[cfg(target_os = "macos")]
mod verify;

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
/// threads the resulting `Source` into [`spawn_background_check`], so the type it
/// passes must be the very same `aterm_update_core::Source` the inherent `resolve`
/// (carrying aterm's compiled-in `alabsystems`/`aterm` channel defaults + the
/// `ATERM_UPDATE_OWNER`/`_REPO`
/// env keys) is defined on. A newtype here would break that call site.
pub use aterm_update_core::{DEFAULT_OWNER, DEFAULT_REPO, Source};

/// Re-exported for every OTHER lane that re-launches aterm forwarding its own
/// argv (the GUI's cold-exec/seamless/Windows successor spawns): strip the
/// leading `--window` mode pins so no relaunch path can re-grow the argv the
/// boot swap deliberately pins exactly once. See the function's own doc for
/// the accumulation this closes.
pub use relaunch::reexec_forwarded_args;

/// The Apple Developer **Team ID** for the OPTIONAL Tier APPLE anchor, baked in at
/// compile time from `ATERM_EXPECTED_TEAM_ID`. Empty (the default) does **not** disable
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
/// with `ATERM_EXPECTED_TEAM_ID` baked in. Once there is a Developer ID to require,
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

/// The base64 Ed25519 **public key** for the OPTIONAL Tier SIG anchor (the Apple-free
/// signed channel), baked in from `ATERM_UPDATE_PUBKEY` at build time. Empty (the
/// default) disables signature checking; when set, every release manifest MUST carry a
/// valid `aterm-appcast.toml.sig` verifying against it (mint the keypair with
/// `atpkg-keys setup`/`join` — the machine key; the secret never leaves its machine). See [`sig`].
/// The CURRENT signing key. Verification must accept ANY key in
/// [`aterm_update_core::pins::UPDATE_CHANNEL_PUBKEYS`] so a rotation does not strand
/// clients; this constant names only the key new releases are signed with.
pub const PINNED_UPDATE_PUBKEY: &str = aterm_update_core::pins::update_channel_signing_pubkey();

/// The full channel KEYSET clients verify against — any member is authoritative.
///
/// Verification uses this; [`PINNED_UPDATE_PUBKEY`] (the head) is only for the
/// cutter, which produces one signature, and for the build stamp that proves which
/// anchor reached the artifact. Keeping them separate is deliberate: the embedded
/// `__aterm_upin` record must name exactly one key, while a client must accept
/// several or a rotation strands it.
///
/// # Only while the paper master is UNPINNED
///
/// This keyset is what authorizes a release in a build whose
/// `pins::PAPER_MASTER_PUBKEYS` is empty — which is every build shipped so far. Arm the
/// master and the master-signed machine roster becomes the sole authority
/// (`github::fetch_authoritative_release`); this slice then survives as the allowance
/// held by clients that predate the roster, which is a fact about THEM and is enforced
/// at the producer, not here.
pub const PINNED_UPDATE_PUBKEYS: &[&str] = aterm_update_core::pins::UPDATE_CHANNEL_PUBKEYS;

/// SHA-256 of the raw 32-byte Ed25519 update key, for shipping introspection.
///
/// `Ok(None)` is the explicit no-pin state. Invalid/non-32-byte pins are errors,
/// never silently reported as absent. Hashing the decoded key (the exact bytes
/// consumed by signature verification) avoids brittle searches for a base64
/// literal that an optimizer may transform or eliminate.
pub fn update_pubkey_sha256(encoded: &str) -> Result<Option<String>, String> {
    use aterm_digest::Sha256;
    use base64::Engine as _;

    if encoded.is_empty() {
        return Ok(None);
    }
    let raw = base64::engine::general_purpose::STANDARD
        .decode(encoded)
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

/// Stable diagnostic value for the actual compile-time updater key:
/// 64 lowercase hex, `empty`, or `invalid`.
#[must_use]
pub fn compiled_update_pin_sha256() -> String {
    match update_pubkey_sha256(PINNED_UPDATE_PUBKEY) {
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

/// Whether the updater may run: macOS-only and not opted out via
/// `ATERM_NO_AUTO_UPDATE`. It no longer requires a pinned anchor — the default Tier
/// REPO works with none (see the crate-level trust model), so an internal build with no
/// Apple Developer ID and no signing key still self-updates. Dev-build inertness comes
/// from [`bundle::resolve`], not from a missing pin, and has two sources there: the
/// LAYOUT (a `cargo run` / `target/` binary is not an installed `.app`) and the
/// codesign-sealed [`bundle::DEV_BUILD_KEY`] mark (a local build installed in place,
/// which layout alone cannot distinguish from a release). On non-macOS targets both
/// entry points are unconditional no-ops, so this is false.
#[must_use]
pub fn enabled() -> bool {
    cfg!(target_os = "macos") && std::env::var_os("ATERM_NO_AUTO_UPDATE").is_none()
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
/// Best-effort by construction: observability must never be able to block or fail
/// an update.
#[cfg(target_os = "macos")]
pub fn record_apply_failure(current_build: u64, reason: &str) {
    let Some(staging) = paths::Staging::resolve() else {
        return;
    };
    health::Health::record_apply_failure(&staging.health(), current_build, reason);
    status::record(
        &staging,
        current_build,
        &format!("staged build did not apply: {reason}"),
    );
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
pub fn record_apply_failure(_current_build: u64, _reason: &str) {}

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
/// The apply-failure STREAK is deliberately absent: `UpdateStatus::failing_applies`
/// already carries it, and two reads of one ledger racing each other could disagree
/// about a number that has exactly one source of truth. This carries only what
/// nothing else reports — the REASONS.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplyLaneReport {
    /// Reason of the most recent apply failure; empty when none is standing.
    pub last_failure: String,
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
    ApplyOutcome::NotApplicable
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

/// Non-macOS no-op: there is no self-swap to confirm.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn confirm_boot_health_exact(_current_build: u64, _current_commit: &str) -> bool {
    true
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

/// A toolchain install is extracting FROM THIS BUNDLE'S sealed payload right now.
///
/// The batteries-included seal lives inside the app bundle, and `atpkg seed` reads
/// it lazily by PATH over minutes and gigabytes. An automatic apply in that window
/// `RENAME_SWAP`s the bundle out from under it — and the replacement came from the
/// lean updater zip, which has the seal stripped. The installer's next read resolves
/// into a bundle whose payload does not exist, mid-transaction, on exactly the
/// machines the offline seal exists for (2026-08-20 round-8 audit).
///
/// An apply deferred here is not an apply lost: the lane retries on its own cadence,
/// and the install it is waiting for is minutes, not hours.
///
/// The marker is written by the READER — atpkg claims it with its own pid at the
/// seal-fetcher choke point (`aterm_update_core::seal_guard`), so every spawn lane
/// (the GUI loop, the Settings worker, a user-run CLI) is guarded by construction.
/// Five rounds of GUI-side choreography (8–12, each patching a race the split
/// ownership created) were deleted with that move; this probe is the surviving
/// half, and it self-heals a marker whose pid is gone rather than blocking
/// updates forever.
#[must_use]
pub fn is_toolchain_install_active() -> bool {
    aterm_update_core::seal_guard::seal_read_active()
}

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
pub type HealthNotify = Box<dyn Fn(String, String) + Send>;

/// A GUI-supplied hook fired when a strictly-newer build has just been STAGED, so the
/// GUI can show the "relaunch to apply" nudge (RFC Rung 2). `(build, version)`.
pub type StagedNotify = Box<dyn Fn(u64, String) + Send>;

/// One process-wide network/staging lane shared by the periodic scheduler and
/// every user-triggered check. The GUI reducer adds generation semantics above
/// this seam; the mutex ensures older callers cannot create a second concurrent
/// download/verify transaction underneath it.
#[cfg(target_os = "macos")]
fn check_lane() -> &'static std::sync::Mutex<()> {
    static LANE: std::sync::Mutex<()> = std::sync::Mutex::new(());
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

#[cfg(all(test, target_os = "macos"))]
mod persistent_notice_tests {
    use super::persistent_notice_is_new;

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
    if !enabled() {
        return;
    }
    // Not an installed `.app` (dev build / `cargo run` / `target/` binary) → nothing to
    // swap; don't spawn a thread that would only ever no-op. Now that `enabled()` no
    // longer requires a pinned anchor, this is what keeps dev runs inert.
    if bundle::resolve().is_none() {
        return;
    }
    log(&format!(
        "checking github.com/{}/{} for updates",
        source.owner, source.repo
    ));
    std::thread::Builder::new()
        .name("aterm-update".into())
        .spawn(move || {
            // Re-check on a short cadence so a running session picks a release up
            // within ~a minute of publish (the owner's "no passive scheduler —
            // immediate" directive). In practice this cadence buys a fast STAGE and
            // the swap happens at the next launch (see the module docs' delivery
            // model for what the seamless handoff does and does not deliver).
            // Cost honesty: a steady-state check on the armed tier spends 5 requests
            // (list + manifest + roster + both signatures; 6 with a container). WITH
            // a token that is ~240/h against the 5000/h budget; WITHOUT one (the
            // public channel, no credential provisioned) the budget is ~60/h PER IP
            // and the cadence has to be far slower or every check is rate-limited —
            // hence the
            // per-lane interval below, adopted as soon as a check reveals which lane
            // this machine is on. `ATERM_UPDATE_INTERVAL_SECS` overrides BOTH (and is
            // then never second-guessed); 0 means check once and stop.
            //
            // This is the BASE interval only. The wait actually taken is jittered and
            // backs off while checks fail, and returns early when the Mac turns out to
            // have been asleep — see `cadence`, which owns all three policies.
            let configured = std::env::var("ATERM_UPDATE_INTERVAL_SECS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok());
            let interval = configured.unwrap_or(cadence::AUTHENTICATED_INTERVAL_SECS);
            let mut schedule = cadence::Cadence::new(std::time::Duration::from_secs(interval));
            let mut failures = cadence::FailureLog::default();
            // Once per process: a channel this machine cannot read is a configuration
            // defect, not an event, so it is announced once and then lives in
            // `status.toml` (rewritten every check) rather than nagging.
            let mut notified_no_token = false;
            // Per-process dedup, seeded from the clock at thread start so history
            // never re-notifies on every launch: the persistent-failure notice
            // requires the streak's latest failure to postdate this thread (RFC3339
            // strings compare chronologically), so a stale streak from a build that
            // isn't even checking any more (e.g. no token) stays quiet.
            let started = install::now_rfc3339();
            // KEYED on the class that was announced, not a bare bool. As a bool this
            // latch swallowed every class after the first for the life of the process:
            // a machine whose downloads broke (announced) and whose apply lane then
            // stranded it heard about the download only. The two notices name
            // different lanes and ask for different fixes, so dropping the second is
            // losing a message, not deduping one. `None` = nothing announced yet.
            let mut notified_failing: Option<&'static str> = None;
            // The installed bundle we last told the GUI about (by build). Announced
            // once per build, re-announced when the bundle moves again. And the last
            // (build, error) we could NOT verify, so a permanently unverifiable bundle
            // is logged once, not every cycle.
            let mut announced_installed: Option<u64> = None;
            let mut unverifiable_installed: Option<(u64, String)> = None;
            loop {
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
                    Ok(_lane) => {
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
                        // THE WINDOW MUST BE SIZED FOR THE LANE THIS MACHINE IS
                        // ACTUALLY ON (2026-08-24 audit). `Cadence` is always
                        // constructed at the AUTHENTICATED base, and only adopts the
                        // anonymous one after a check has completed and revealed the
                        // lane — but `github::lane()` is a process-local static, so a
                        // freshly spawned process ALWAYS starts on the fast base, and
                        // every terminal session runs this loop. Sizing the dedup
                        // window off 75 s meant each new session more than ~52 s after
                        // the last one spent a full 5-request check: twelve launches in
                        // an hour is the whole ~60/hour anonymous per-IP budget, which
                        // is precisely the "update check deferred: GitHub rate limit"
                        // an owner sees. Guessing FAST costs the shared budget;
                        // guessing SLOW only delays a first check a sibling has already
                        // made — so while the lane is unknown, assume the slow one.
                        //
                        // This cannot starve the process: it has not completed a check
                        // yet, so it has no stamp of its own in the ledger to mistake
                        // for another checker's, and the first completed check both
                        // stamps the ledger and reveals the lane.
                        let dedup_base = dedup_window_base(
                            configured.is_some(),
                            github::lane(),
                            schedule.base(),
                        );
                        if let Some(reason) =
                            checker_staging.as_ref().and_then(|s| checker_skip(s, dedup_base))
                        {
                            log(reason);
                            // Release the flock BEFORE sleeping — holding it through
                            // the wait would serialize every other process on OUR
                            // timer — then take the same jittered wait the loop tail
                            // takes (a bare `continue` would skip the tail's sleep
                            // and spin hot). The wake subtleties (settle window,
                            // still-failing suppression) only matter ahead of a
                            // network check, which this cycle deliberately isn't.
                            drop(_checker_gate);
                            let (_delay, waited) = cadence::wait(&schedule);
                            if matches!(waited, cadence::Waited::Woke(_)) {
                                schedule.woke();
                            }
                            continue;
                        }
                        // Stamped BEFORE the check so the ledger can be asked, after
                        // it, whether THIS check recorded a failure (see the `Ok(None)`
                        // arm). RFC3339 strings compare chronologically.
                        let check_started = install::now_rfc3339();
                        match github::check_and_stage(current_build, &source) {
                            Ok(Some(v)) => {
                                emit(failures.success());
                                schedule.succeeded();
                                // "is staged", not "was staged just now": the check also
                                // answers `Some` for a build that was already published and
                                // is only waiting to be applied (its re-download may be
                                // backed off — a stage backoff never gates an apply).
                                log(&format!(
                                    "update {v} is staged — the GUI auto-applies it now (or next launch)"
                                ));
                                // RFC Rung 2: surface the staged build to the GUI so it can show
                                // the "relaunch to apply" nudge. The staged build number comes
                                // from the ready marker (status reads it, no I/O).
                                if let Some(cb) = on_staged.as_ref()
                                    && let Some(b) =
                                        status(current_build).and_then(|s| s.staged_build)
                                {
                                    cb(b, v);
                                }
                            }
                            Ok(None) if no_token::is_stranded() => {
                                // Not a success: GitHub answered that this machine
                                // cannot read the channel at all. Back off, because a
                                // 75 s retry of something that cannot succeed only
                                // re-spawns `security`/`gh` and burns requests
                                // forever. The backoff clears on the first readable
                                // check, so fixing the channel (or provisioning a
                                // token) mid-session is noticed within one backoff
                                // ceiling at worst — `max(MAX_BACKOFF,
                                // MAX_BACKOFF_INTERVALS × base)`, i.e. ~15 min on the
                                // authenticated lane and ~1 h on the slow anonymous
                                // one, which is the lane a missing token puts you on.
                                schedule.failed();
                            }
                            Ok(None) if github::rate_limited() => {
                                // GitHub asked us to slow down. That is a CADENCE
                                // problem, not a broken updater: lengthen the wait
                                // (the entire remedy) but emit no failure line and no
                                // ledger entry, so a machine sharing an IP's ~60/hour
                                // anonymous budget never accrues the streak that
                                // fires "your update pipeline is likely broken".
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
                                let recorded_failure =
                                    paths::Staging::resolve().is_some_and(|s| {
                                        health::Health::read(&s.health())
                                            .last_failure_at
                                            .as_str()
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
                if no_token::is_stranded()
                    && !notified_no_token
                    && let Some(cb) = notify.as_ref()
                {
                    notified_no_token = true;
                    let (title, body) = no_token::notification();
                    cb(title, body);
                }
                if let (Some(cb), Some(staging)) = (notify.as_ref(), paths::Staging::resolve()) {
                    let h = health::Health::read(&staging.health());
                    let active = h.last_failure_at.as_str() >= started.as_str();
                    // DURATION gate beside the count gate: at the immediate-update
                    // cadence three consecutive failures span ~4 minutes — a blip,
                    // not the "pipeline is broken" signal the loud notice promises.
                    // Require the streak to have PERSISTED (>= 30 min of failing)
                    // like it inherently did at the old 6h cadence.
                    //
                    // MEASURED ON THE ESCALATING CLASS'S OWN CLOCK. `failing_since` is
                    // any-class: it is stamped by the FIRST class to break and is
                    // deliberately never cleared while an apply streak survives
                    // (`Health::record_success`). Reading the gate from it meant a
                    // single stale apply failure from days ago backdated every later
                    // streak, so a four-minute manifest blip cleared a thirty-minute
                    // gate the instant it crossed the count — firing the loud notice
                    // for exactly the transient this gate exists to swallow, and
                    // dating it days before the problem existed.
                    let escalated = h.persistent_class();
                    let long_lived = escalated.is_some_and(|(class, _)| {
                        install::rfc3339_delta_secs(h.class_since(class), &install::now_rfc3339())
                            .is_some_and(|d| d >= 30 * 60)
                    });
                    let notice_owed =
                        persistent_notice_is_owed(notified_failing, escalated.map(|(c, _)| c));
                    if let Some((class, count)) = escalated
                        && active
                        && long_lived
                        && notice_owed
                    {
                        notified_failing = Some(class);
                        // The COUNT and the sentence both come from the class that
                        // escalated. A single hardcoded pipeline story told an
                        // apply-stranded machine "0 consecutive checks … cannot be
                        // downloaded" — wrong number, wrong lane, and it sent the
                        // reader hunting a download fault that did not exist.
                        let cause = match class {
                            "manifest" => format!(
                                "the newest release cannot be trusted ({}) — this machine \
                                 stays on build {current_build} until it is republished",
                                h.last_error
                            ),
                            "stage" => format!(
                                "updates download but will not verify or install ({})",
                                h.last_error
                            ),
                            "apply" => format!(
                                "an update is downloaded and verified but will not start \
                                 ({})",
                                h.last_apply_error
                            ),
                            // "pipeline", and any future class, keeps the original text.
                            _ => "release manifests exist but cannot be downloaded — \
                                  this build's update pipeline is likely broken"
                                .to_string(),
                        };
                        cb(
                            "aterm auto-update is failing".to_string(),
                            format!(
                                "{count} consecutive checks since {}: {cause}. Run \
                                 `aterm-ctl update status` for details.",
                                h.class_since(class)
                            ),
                        );
                    } else if !h.is_persistent() {
                        notified_failing = None; // all healed → any class may speak again
                    }
                }
                if interval == 0 {
                    break;
                }
                // Adopt the cadence the credential lane can actually afford, now that
                // a completed check has revealed it. An explicitly configured interval
                // is never overridden — an operator who set one owns the consequence.
                if configured.is_none() {
                    schedule.set_base(std::time::Duration::from_secs(
                        match github::lane() {
                            github::Lane::Anonymous => cadence::ANONYMOUS_INTERVAL_SECS,
                            github::Lane::Authenticated | github::Lane::Unknown => {
                                cadence::AUTHENTICATED_INTERVAL_SECS
                            }
                        },
                    ));
                }
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

/// Non-macOS no-op.
#[cfg(not(target_os = "macos"))]
pub fn spawn_background_check(
    _current_build: u64,
    _source: Source,
    _notify: Option<HealthNotify>,
    _on_staged: Option<StagedNotify>,
) {
}

/// Consecutive same-class failed checks at which the failure is called PERSISTENT
/// (drives [`UpdateStatus::is_failing_persistently`], the status wording, the
/// GUI notification, and the `aterm-ctl update` `persistent=` field). Cross-platform
/// so status consumers can reason about it; the macOS `health` ledger enforces the
/// same threshold.
pub const PERSISTENT_AFTER: u32 = 3;

/// Serializes the tests that mutate the PROCESS-GLOBAL "this machine cannot read its
/// release channel" latch (`no_token::STRANDED`, `github::LANE`). Cargo runs a crate's
/// tests in parallel threads of ONE process, so without this an assertion about the
/// latch can observe a sibling test's transient state and fail intermittently.
#[cfg(all(test, target_os = "macos"))]
pub(crate) static STRANDED_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A snapshot of the updater's state for the "Check for Updates" menu and the
/// `aterm-ctl update` query. Read from the durable status/ready markers, so it reflects
/// the last background check + any staged build WITHOUT triggering network I/O.
#[derive(Debug, Clone)]
pub struct UpdateStatus {
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
    /// RFC3339 UTC time of the last check (empty if never).
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
        match (&self.staged_version, self.staged_build) {
            // NOT "applies on next launch": a relaunch is the FALLBACK, not the
            // delivery mechanism. The staged build is verified and ready for the
            // in-session apply lane right now, and telling the user to quit is
            // exactly the advice this updater exists to make unnecessary.
            (Some(v), Some(b)) => {
                format!("update {v} (build {b}) staged and ready to apply")
            }
            _ if !self.outcome.is_empty() => self.outcome.clone(),
            _ if !self.enabled => "auto-update is disabled on this build".to_string(),
            _ => "no update check has run yet".to_string(),
        }
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
        || persisted.contains("applies on next launch")
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

/// The base interval the cross-process dedup window is measured against.
///
/// NOT always `schedule.base()`. [`cadence::Cadence`] is always constructed at
/// the AUTHENTICATED interval and only adopts the anonymous one once a completed
/// check has revealed the lane — and the lane lives in a PROCESS-LOCAL static, so
/// every freshly spawned process starts on the fast base no matter what this
/// machine has already learned. Since the one-binary era each terminal SESSION
/// runs the check loop, so sizing the window off 75 s made every launch more than
/// ~52 s after the last one spend a full 5-request check: a dozen launches in an
/// hour is the entire ~60/hour anonymous per-IP budget, and the machine lives in
/// "update check deferred: GitHub rate limit" — the invariant the loop's own
/// comment promises ("N processes cost one check per interval, not N") failing
/// for precisely the check every short-lived process makes.
///
/// So while the lane is UNKNOWN, assume the slow one: guessing fast spends a
/// budget shared with every other machine on the IP, while guessing slow only
/// defers a first check that a sibling has already made. It cannot starve the
/// caller — a process with no completed check has no stamp of its own in the
/// ledger to mistake for another checker's, and the first completed check both
/// stamps the ledger and reveals the lane. An explicitly configured interval is
/// never second-guessed: an operator who set one owns the consequence.
#[cfg(any(target_os = "macos", test))]
fn dedup_window_base(
    configured: bool,
    lane: github::Lane,
    base: std::time::Duration,
) -> std::time::Duration {
    if configured || lane != github::Lane::Unknown {
        return base;
    }
    std::time::Duration::from_secs(cadence::ANONYMOUS_INTERVAL_SECS).max(base)
}

/// How much a RECORDED DEFERRAL widens the machine-wide freshness window, as a
/// multiple of the base interval.
///
/// A rate limit is measured per IP, so the retreat has to be measured per
/// MACHINE. `Cadence::failed` lengthens the wait of the one process that saw the
/// 429 — but the ledger stamp it leaves behind was, until this constant existed,
/// judged against every sibling's own un-backed-off base, so siblings kept poking
/// GitHub at full cadence for the whole backoff and the machine never actually
/// slowed to the rate it had just computed. One doubling mirrors the first rung
/// of `Cadence`'s ladder, applied to every process rather than to one.
#[cfg(target_os = "macos")]
const DEFERRED_WINDOW_INTERVALS: u32 = 2;

/// Why this cycle must NOT spend a network check, if it must not — i.e. whether
/// the shared ledger records a check completed WITHIN the freshness window, so
/// another aterm process (the window, or a sibling session) has already spent
/// this interval's network budget.
///
/// The window is 70% of the base interval: strictly below the jittered minimum
/// wait (80% of nominal), so a process can never mistake its OWN previous
/// cycle's stamp for another checker's and starve itself.
///
/// Deferrals (rate limit, stranded) also stamp the ledger, AND widen the window
/// by [`DEFERRED_WINDOW_INTERVALS`]: a machine that was just told to slow down
/// must not be re-poked by a sibling on the sibling's own faster timer. The
/// widened window is still bounded — the next healthy check overwrites the
/// outcome and the window returns to the base — so the retreat self-heals
/// exactly as the per-process backoff does.
#[cfg(target_os = "macos")]
fn checker_skip(staging: &paths::Staging, base: std::time::Duration) -> Option<&'static str> {
    let text = read_ledger_text(&staging.status)?;
    let v = text.parse::<toml::Value>().ok()?;
    let updated = v.get("updated_at").and_then(toml::Value::as_str)?;
    if updated.is_empty() {
        return None;
    }
    let deferred = v
        .get("outcome")
        .and_then(toml::Value::as_str)
        .is_some_and(|outcome| outcome.contains("deferred"));
    let window = if deferred {
        base.saturating_mul(DEFERRED_WINDOW_INTERVALS)
    } else {
        base
    };
    let fresh_window = window.as_secs().saturating_mul(7) / 10;
    if rfc3339_older_than(updated, fresh_window) {
        return None;
    }
    Some(if deferred {
        "the shared update ledger records a deferred check — this machine is \
         holding off GitHub for the rest of the backoff"
    } else {
        "another aterm process completed this interval's update check"
    })
}

#[cfg(target_os = "macos")]
#[must_use]
pub fn status(current_build: u64) -> Option<UpdateStatus> {
    let staging = paths::Staging::resolve()?;
    // Parse the status marker best-effort for the last outcome + timestamp.
    let (mut checked_from_build, mut outcome, mut updated_at) =
        (current_build, String::new(), String::new());
    if let Some(text) = read_ledger_text(&staging.status)
        && let Ok(v) = text.parse::<toml::Value>()
    {
        // Best-effort (via `u64::try_from` — the verifier cannot lower i64<->u64
        // `as` casts): absent or negative reads keep the running build.
        checked_from_build = v
            .get("current_build")
            .and_then(toml::Value::as_integer)
            .and_then(|n| u64::try_from(n).ok())
            .unwrap_or(checked_from_build);
        outcome = v
            .get("outcome")
            .and_then(toml::Value::as_str)
            .unwrap_or("")
            .to_string();
        updated_at = v
            .get("updated_at")
            .and_then(toml::Value::as_str)
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
        channel_unreadable: no_token::is_stranded(),
    })
}

/// Non-macOS stub: no updater state to report.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn status(_current_build: u64) -> Option<UpdateStatus> {
    None
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

/// Run ONE update check + stage synchronously and return the resulting [`UpdateStatus`]
/// (the "Check for Updates" action). BLOCKS on network + disk (download/verify/stage up
/// to tens of seconds), so callers MUST run it off the UI/event-loop thread. A no-op
/// that just reports current state when the updater is disabled or not an installed app.
#[cfg(target_os = "macos")]
pub fn check_now(current_build: u64, source: &Source) -> UpdateStatus {
    if enabled() {
        // Take the lane if it is free (or poison-recover it) and run the check; if it is
        // BUSY, remember that so we can join the in-flight transaction below. The
        // try-lock guard is scoped to THIS match and is dropped before the join's
        // blocking `lock()` — kept sequential (not lexically nested inside the match)
        // so the lock-order census does not read a `check_lane`→`check_lane` self-edge.
        let joined = match check_lane().try_lock() {
            Ok(_lane) => {
                if let Err(e) = github::check_and_stage(current_build, source) {
                    warn(&format!("manual update check failed: {e}"));
                }
                false
            }
            Err(std::sync::TryLockError::Poisoned(poisoned)) => {
                let _lane = poisoned.into_inner();
                if let Err(e) = github::check_and_stage(current_build, source) {
                    warn(&format!(
                        "manual update check failed after lane recovery: {e}"
                    ));
                }
                false
            }
            Err(std::sync::TryLockError::WouldBlock) => true,
        };
        if joined {
            // Join the exact in-flight transaction and consume its durable status
            // instead of immediately issuing the same request again.
            drop(
                check_lane()
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()),
            );
        }
    }
    status(current_build).unwrap_or_else(|| {
        UpdateStatus::empty(
            enabled(),
            current_build,
            if enabled() {
                "update check could not run".into()
            } else {
                "auto-update is disabled on this build".into()
            },
        )
    })
}

/// Non-macOS stub.
#[cfg(not(target_os = "macos"))]
pub fn check_now(current_build: u64, _source: &Source) -> UpdateStatus {
    UpdateStatus::empty(false, current_build, "auto-update is macOS-only".into())
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

#[cfg(test)]
mod checker_gate_tests {
    use std::time::Duration;

    use super::dedup_window_base;
    use crate::cadence;
    use crate::github::Lane;

    const AUTH: Duration = Duration::from_secs(cadence::AUTHENTICATED_INTERVAL_SECS);
    const ANON: Duration = Duration::from_secs(cadence::ANONYMOUS_INTERVAL_SECS);

    /// THE LAUNCH-COST OBLIGATION the steady-state budget test cannot express.
    /// A process that has not completed a check does not know its lane, and its
    /// `Cadence` is still on the authenticated base — so the dedup window it is
    /// judged against must be the SLOW one, or N launches cost N × 5 anonymous
    /// requests instead of one.
    #[test]
    fn an_unknown_lane_is_deduped_at_the_anonymous_interval() {
        assert_eq!(
            dedup_window_base(false, Lane::Unknown, AUTH),
            ANON,
            "a freshly spawned process must not spend the shared budget on a guess"
        );
        // Twelve launches in an hour, against the ~60 req/hour anonymous budget:
        // the freshness window (70% of the base) must exceed the spacing, so at
        // most one of them reaches the network.
        let window = dedup_window_base(false, Lane::Unknown, AUTH).as_secs() * 7 / 10;
        let spacing = 3600 / 12;
        assert!(
            window > spacing,
            "12 launches/hour ({spacing}s apart) must dedup inside a {window}s window"
        );
    }

    #[test]
    fn a_known_lane_and_a_configured_interval_are_taken_at_face_value() {
        assert_eq!(dedup_window_base(false, Lane::Authenticated, AUTH), AUTH);
        assert_eq!(dedup_window_base(false, Lane::Anonymous, ANON), ANON);
        // An operator interval owns its own consequence, fast or slow.
        let configured = Duration::from_secs(10);
        assert_eq!(
            dedup_window_base(true, Lane::Unknown, configured),
            configured
        );
    }

    /// The window may only ever GROW relative to the schedule's own base, so this
    /// gate can never shorten a cadence the lane or the operator already accepted.
    #[test]
    fn the_dedup_window_never_undercuts_the_schedules_own_base() {
        for lane in [Lane::Unknown, Lane::Authenticated, Lane::Anonymous] {
            for base in [
                Duration::from_secs(1),
                AUTH,
                ANON,
                Duration::from_secs(7200),
            ] {
                for configured in [false, true] {
                    assert!(
                        dedup_window_base(configured, lane, base) >= base,
                        "{lane:?} {base:?} configured={configured}"
                    );
                }
            }
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod checker_skip_tests {
    use std::time::Duration;

    use super::{checker_skip, paths::Staging};

    fn staging(name: &str) -> Staging {
        let root = std::env::temp_dir().join(format!(
            "aterm-checker-{}-{name}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&root).expect("scratch root");
        Staging {
            apply_lock: root.join("apply.lock"),
            stage_lock: root.join("stage.lock"),
            download: root.join("download"),
            staged_app: root.join("staged").join("aterm.app"),
            ready: root.join("ready.toml"),
            status: root.join("status.toml"),
            root,
        }
    }

    fn write_ledger(s: &Staging, age_secs: u64, outcome: &str) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_secs();
        let stamp = aterm_types::rfc3339::format_rfc3339(now.saturating_sub(age_secs));
        std::fs::write(
            &s.status,
            format!(
                "schema = 1
updated_at = \"{stamp}\"
outcome = \"{outcome}\"
"
            ),
        )
        .expect("write ledger");
    }

    const BASE: Duration = Duration::from_secs(30 * 60);

    #[test]
    fn a_fresh_stamp_skips_and_a_stale_one_checks() {
        let s = staging("fresh");
        write_ledger(&s, 60, "up to date");
        assert!(
            checker_skip(&s, BASE).is_some(),
            "a sibling checked a minute ago — this cycle owes GitHub nothing"
        );
        // 70% of 30 min is 21 min; 25 minutes is past it.
        write_ledger(&s, 25 * 60, "up to date");
        assert!(
            checker_skip(&s, BASE).is_none(),
            "past the freshness window the check is this process's to make"
        );
        let _ = std::fs::remove_dir_all(&s.root);
    }

    /// THE RETREAT IS MEASURED PER MACHINE, because the rate limit is. Without
    /// this a sibling on its own un-backed-off timer re-poked GitHub at full
    /// cadence for the whole backoff, so a machine that had just been told to
    /// slow down never actually did.
    #[test]
    fn a_recorded_deferral_holds_off_every_process_not_just_the_one_that_saw_it() {
        let s = staging("deferred");
        write_ledger(&s, 25 * 60, "update check deferred: GitHub rate limit");
        let reason = checker_skip(&s, BASE).expect("the deferral widens the window");
        assert!(
            reason.contains("deferred"),
            "the log line names the real reason: {reason}"
        );
        // It is bounded, not permanent: past the widened window (70% of 2×base
        // = 42 min) the machine tries again, and one healthy check overwrites
        // the outcome and restores the base window.
        write_ledger(&s, 50 * 60, "update check deferred: GitHub rate limit");
        assert!(
            checker_skip(&s, BASE).is_none(),
            "the machine-wide retreat self-heals"
        );
        write_ledger(&s, 25 * 60, "up to date");
        assert!(
            checker_skip(&s, BASE).is_none(),
            "a healthy check returns the window to the base interval"
        );
        let _ = std::fs::remove_dir_all(&s.root);
    }

    #[test]
    fn a_missing_or_empty_ledger_never_defers_a_check() {
        let s = staging("missing");
        assert!(checker_skip(&s, BASE).is_none(), "no ledger: check");
        std::fs::write(
            &s.status,
            "schema = 1
updated_at = \"\"
",
        )
        .expect("write");
        assert!(checker_skip(&s, BASE).is_none(), "empty stamp: check");
        std::fs::write(&s.status, "not toml at all {{{").expect("write");
        assert!(checker_skip(&s, BASE).is_none(), "unparseable: check");
        let _ = std::fs::remove_dir_all(&s.root);
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
        use base64::Engine as _;

        let encoded = base64::engine::general_purpose::STANDARD.encode([0_u8; 32]);
        assert_eq!(
            update_pubkey_sha256(&encoded).unwrap().as_deref(),
            Some("66687aadf862bd776c8fc18b8e9f8e20089714856ee233b3902a591d0d5f2925")
        );
        assert_eq!(update_pubkey_sha256("").unwrap(), None);
        assert!(update_pubkey_sha256("not-base64").is_err());
        let short = base64::engine::general_purpose::STANDARD.encode([0_u8; 31]);
        assert!(update_pubkey_sha256(&short).is_err());

        let compiled = compiled_update_pin_sha256();
        assert!(
            compiled == "empty"
                || compiled == "invalid"
                || (compiled.len() == 64 && compiled.bytes().all(|byte| byte.is_ascii_hexdigit())),
            "stable diagnostic shape: {compiled}"
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
