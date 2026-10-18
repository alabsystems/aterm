// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE HANDOFF ENVIRONMENT SNAPSHOT (docs/DESIGN-warm-successor-2026-09-29.md
//! §1, phase P1).
//!
//! An update handoff hands its successor authority through the process
//! environment: the manifest, nonce, layout and descriptor list
//! (`ATERM_SEAMLESS_*`), the readiness and Commit pipes, the parent's identity
//! and exit capabilities, the rendezvous and its claim, the proof term, the
//! grant capabilities and the control-socket witness (`ATERM_HANDOFF_*`). Until
//! this module every one of them was read — and the launched lane's claimed
//! descriptors were even WRITTEN, by `ClaimedHandoff::publish` — through
//! `std::env` at the point of use, so the whole intake had to sit on the
//! single-threaded prologue of `main_entry`: `set_var`/`remove_var` are unsound
//! beside any other thread.
//!
//! [`HandoffEnv::capture`] reads AND CLEARS every one of those names ONCE, as
//! the first thing `main_entry` does, before any thread exists. Everything
//! after works on the in-memory snapshot:
//!
//! * `seamless::prearm_incoming_fds_from` attests the parent and re-arms the
//!   descriptors from it, and removes from IT what it used to remove from the
//!   process;
//! * the boot apply's re-exec carries the snapshot's remaining pairs onto the
//!   new image ([`HandoffEnv::exec_env`]), exactly the pairs the ambient
//!   environment used to carry across that `execve`;
//! * the launched lane's claim takes the rendezvous, claim and capabilities
//!   from it, and the granted descriptors land in it
//!   (`handoff_rendezvous::ClaimedIntake::install`) instead of in `environ`;
//! * the `_from` intakes (`take_incoming_from`, `take_ready_fd_from`,
//!   `take_commit_fd_from`, `take_target_identity_from`,
//!   `control_socket_identity::consume_incoming_from`) run the SAME
//!   authentication over the same strings.
//!
//! Same authentication, a different transport: no timing or behaviour change is
//! intended, and after it where the claim sits no longer matters to env
//! soundness (the precondition of P2, which moves work ahead of the dial).
//!
//! [`HandoffName`] fences it: a handoff name has no `AsRef<OsStr>` in shipped
//! code, so nothing outside this file can read or mutate one through the
//! process environment. (A parent naming one on a CHILD's environment is not
//! the process environment: [`HandoffName::pair`].) The model is
//! `NativeUpdateSuccessorWarmBeforeClaim` (aterm-spec); its Tier-1 bind is
//! `conformance` below.

use std::ffi::{OsStr, OsString};

/// The name of one piece of handoff authority in the process environment.
///
/// It has no `AsRef<OsStr>` outside test builds, so shipped code cannot hand
/// one to `std::env` or `aterm_log::env`: [`HandoffEnv::capture`] is the one
/// read of the process environment for a handoff name, and every later reader
/// works on that snapshot (a `set_var` beside another thread is a data race).
/// A parent names one on a CHILD's environment, which is not this process's,
/// with [`Self::pair`], which hands out the name only bound to its value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct HandoffName(&'static str);

impl HandoffName {
    /// A handoff name; each reader declares its own beside its decoder.
    pub(crate) const fn new(name: &'static str) -> Self {
        Self(name)
    }

    /// This name set to `value` in a CHILD's environment: a `Command`'s
    /// `envs`, a launch request's pairs, the re-exec image's restored pairs.
    /// The name leaves this file only bound to a value, never as a bare string
    /// a read of this process's own environment could take.
    #[cfg(unix)]
    #[must_use]
    pub(crate) fn pair(self, value: impl AsRef<OsStr>) -> (OsString, OsString) {
        (OsString::from(self.0), value.as_ref().to_os_string())
    }

    /// The bare name, for a test's assertions.
    #[cfg(test)]
    pub(crate) const fn name(self) -> &'static str {
        self.0
    }
}

impl std::fmt::Display for HandoffName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

/// Test builds set handoff names in the process to drive the env-shaped entry
/// points ([`through_process_env`]).
#[cfg(test)]
impl AsRef<OsStr> for HandoffName {
    fn as_ref(&self) -> &OsStr {
        OsStr::new(self.0)
    }
}

#[cfg(test)]
impl From<HandoffName> for &'static str {
    fn from(name: HandoffName) -> Self {
        name.0
    }
}

#[cfg(test)]
impl From<HandoffName> for OsString {
    fn from(name: HandoffName) -> Self {
        Self::from(name.0)
    }
}

/// Every process-environment name a successor reads as handoff authority on
/// THIS platform, and so every name [`HandoffEnv::capture`] takes out of the
/// process: the readers' own constants. Per platform exactly as the readers
/// are: the rendezvous, the parent identity and the parent's exit capabilities
/// are unix-only, the proof term and the grant capabilities ride only beside a
/// rendezvous (macOS). A reader asking for a name not listed here trips the
/// accessors' debug assertion.
pub(crate) const HANDOFF_ENV_KEYS: &[HandoffName] = &[
    crate::seamless::ENV_MANIFEST,
    crate::seamless::ENV_FDS,
    crate::seamless::ENV_NONCE,
    crate::seamless::ENV_LAYOUT,
    crate::seamless::ENV_TARGET,
    crate::seamless::ENV_READY_FD,
    crate::seamless::ENV_COMMIT_FD,
    crate::control_socket_identity::ENV_IDENTITY,
    #[cfg(unix)]
    crate::seamless::ENV_RENDEZVOUS,
    #[cfg(unix)]
    crate::seamless::ENV_CLAIM,
    #[cfg(unix)]
    crate::seamless::ENV_PARENT_PID,
    #[cfg(unix)]
    crate::seamless::ENV_PARENT_BIRTH,
    // What the parent reads of this process's exit: published beside its
    // identity on both lanes, taken before any shell.
    #[cfg(unix)]
    crate::seamless::ENV_EXIT_CAPS,
    #[cfg(target_os = "macos")]
    crate::handoff_rendezvous::ENV_GRANT_CAPS,
    #[cfg(target_os = "macos")]
    crate::handoff_rendezvous::ENV_PROOF_TERM,
    // The advisory warm hint (P2): captured like the rest so no helper or
    // shell ever sees it, and read only as a guess (`handoff_warm_hint`).
    #[cfg(target_os = "macos")]
    crate::handoff_warm_hint::ENV_WARM_HINT,
    #[cfg(target_os = "macos")]
    crate::handoff_rendezvous::ENV_LAUNCHER_NOFILE,
    // Beside it: the limit the predecessor held before a park raised it, for
    // an older successor of this one.
    #[cfg(target_os = "macos")]
    crate::handoff_rendezvous::ENV_PARK_RAISED_FROM,
];

/// The handoff authority this process was launched with, held in memory.
///
/// Every accessor has the semantics of the `std::env` call it replaces:
/// [`Self::os`] is `var_os`, [`Self::string`] is `var(..).ok()` (UTF-8 only),
/// [`Self::take`] is `aterm_log::env::take`, [`Self::remove`] is
/// `aterm_log::env::unset`. A reader moved onto the snapshot therefore decides
/// exactly what it decided over the process environment.
#[derive(Default)]
pub(crate) struct HandoffEnv {
    /// `(name, value)` in [`HANDOFF_ENV_KEYS`] order; a name appears at most
    /// once, and only while present.
    slots: Vec<(HandoffName, OsString)>,
}

impl HandoffEnv {
    /// Read and CLEAR every [`HANDOFF_ENV_KEYS`] name, once.
    ///
    /// Position (the caller's obligation, the only one left): before the first
    /// thread of the process. `main_entry` calls it as its first act on the
    /// handoff, so this is the one process-environment mutation the handoff
    /// makes in the successor, and it happens while the process is provably
    /// single-threaded. Every later handoff read is of `self`.
    #[must_use]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "native_update_successor_warm_before_claim",
            action = "Capture",
            project = "aterm_gui::seamless::handoff_env_conformance::project"
        )
    )]
    pub(crate) fn capture() -> Self {
        let mut slots = Vec::new();
        for key in HANDOFF_ENV_KEYS {
            // `take` removes the name whether or not it was set: after this
            // loop no handoff name is left in the process environment, for a
            // helper, a shell or a later reader to observe.
            if let Some(value) = aterm_log::env::take(key.0) {
                slots.push((*key, value));
            }
        }
        Self { slots }
    }

    fn slot(&self, key: HandoffName) -> Option<&OsString> {
        debug_assert!(
            HANDOFF_ENV_KEYS.contains(&key),
            "{key} is not a handoff name this platform captures"
        );
        self.slots
            .iter()
            .find_map(|(name, value)| (*name == key).then_some(value))
    }

    /// Whether `key` arrived (`std::env::var_os(key).is_some()`).
    #[must_use]
    pub(crate) fn present(&self, key: HandoffName) -> bool {
        self.slot(key).is_some()
    }

    /// The raw value (`std::env::var_os(key)`), not consumed.
    #[must_use]
    pub(crate) fn os(&self, key: HandoffName) -> Option<&OsStr> {
        self.slot(key).map(OsString::as_os_str)
    }

    /// The value as UTF-8 (`std::env::var(key).ok()`), not consumed: a
    /// non-UTF-8 value answers `None`, as that call's `Err` did.
    #[must_use]
    pub(crate) fn string(&self, key: HandoffName) -> Option<String> {
        self.slot(key)
            .and_then(|value| value.to_str())
            .map(str::to_owned)
    }

    /// Read and remove (`aterm_log::env::take(key)`): a one-shot authority is
    /// consumable once from the snapshot, as it was from the environment.
    pub(crate) fn take(&mut self, key: HandoffName) -> Option<OsString> {
        debug_assert!(
            HANDOFF_ENV_KEYS.contains(&key),
            "{key} is not a handoff name this platform captures"
        );
        let index = self.slots.iter().position(|(name, _)| *name == key)?;
        Some(self.slots.remove(index).1)
    }

    /// Remove, discarding the value (`aterm_log::env::unset(key)`).
    pub(crate) fn remove(&mut self, key: HandoffName) {
        let _ = self.take(key);
    }

    /// Set `key` to `value` — the launched lane's granted descriptors landing
    /// where the fork lane's arrive (`handoff_rendezvous::ClaimedIntake`). The
    /// snapshot is this process's own memory: no other thread can observe it,
    /// so unlike the `set_var` it replaces this is sound at any point. The
    /// launched lane (`handoff_rendezvous`) is macOS's.
    #[cfg(any(target_os = "macos", all(test, unix)))]
    pub(crate) fn set(&mut self, key: HandoffName, value: impl Into<OsString>) {
        let value = value.into();
        let _ = self.take(key);
        // Keep the capture order, so `exec_env` is stable.
        let rank = |name: HandoffName| HANDOFF_ENV_KEYS.iter().position(|key| *key == name);
        let at = self
            .slots
            .iter()
            .position(|(name, _)| rank(*name) > rank(key))
            .unwrap_or(self.slots.len());
        self.slots.insert(at, (key, value));
    }

    /// The pairs still held, to restore onto an `execve` of this process's own
    /// successor image (the boot apply's re-exec). Before the snapshot those
    /// names were simply still in `environ` at the exec, and the new image read
    /// them there; its `capture` reads them here.
    #[must_use]
    pub(crate) fn exec_env(&self) -> Vec<(OsString, OsString)> {
        self.slots
            .iter()
            .map(|(name, value)| (OsString::from(name.0), value.clone()))
            .collect()
    }

    /// The names still held, for the conformance projection.
    #[cfg(all(test, unix))]
    pub(crate) fn held(&self) -> Vec<HandoffName> {
        self.slots.iter().map(|(name, _)| *name).collect()
    }
}

/// THE ENV-SHAPED TEST ENTRY POINTS, kept so the handoff suites run unchanged:
/// a suite sets names in the process, calls the retired env-reading function,
/// and asserts on the process afterwards. This reads `keys` (the footprint the
/// retired function read) into a snapshot WITHOUT clearing them, runs `body`
/// over it, then clears from the process exactly the names `body` consumed —
/// the old function's effect on the process environment, name for name. Test
/// builds only: a shipped reader has the one snapshot `main_entry` captured. The
/// suites that drive it are Unix's.
#[cfg(all(test, unix))]
pub(crate) fn through_process_env<T>(
    keys: &[HandoffName],
    body: impl FnOnce(&mut HandoffEnv) -> T,
) -> T {
    let mut env = HandoffEnv::default();
    for &key in keys {
        if let Some(value) = std::env::var_os(key) {
            env.set(key, value);
        }
    }
    let before = env.held();
    let out = body(&mut env);
    for key in before {
        if !env.present(key) {
            aterm_log::env::unset(key);
        }
    }
    out
}

/// The machine's actions with no unit-drivable code, each waived by name. Bound
/// elsewhere are `Capture` (`HandoffEnv::capture`), `Dial`/`ClaimFail`
/// (`handoff_rendezvous::claim_incoming`), `ClaimOk` (`ClaimedIntake::install`),
/// `TakeOwned` (`seamless::take_incoming_from`), `Adopt`
/// (`seamless::take_commit_fd_from`) and, from P2, `SpawnWarm`
/// (`crate::warm_order`), `Reshape` (`crate::warm_miss_px`) and `Rewarm`
/// (`crate::warm_config_moved`), all driven by
/// `seamless::handoff_env_conformance`.
#[cfg(test)]
mod waivers {
    #[aterm_spec_anchor::spec_unmodeled(
        machine = "native_update_successor_warm_before_claim",
        action = "RelativeSocket",
        reason = "The environment: the launch's own `--control-sock` flag. `warm_order`'s Tier-1 \
                  drives the real decision over every shape, relative included."
    )]
    #[aterm_spec_anchor::spec_unmodeled(
        machine = "native_update_successor_warm_before_claim",
        action = "WarmDone",
        reason = "The return of `warm_prologue`'s main-thread section (lib.rs): it builds a GPU \
                  device and cannot run in a unit test (AGENTS.md); the conformance \
                  realizes it as a stand-in that returns without joining its thread, and asserts \
                  that thread is still running at the real dial."
    )]
    #[aterm_spec_anchor::spec_unmodeled(
        machine = "native_update_successor_warm_before_claim",
        action = "StaleHint",
        reason = "The environment: the outgoing process's window changed between its launch-time \
                  hint and its capture. `warm_miss_px`'s Tier-1 drives the real hint parser over \
                  stale and hostile hints."
    )]
    #[aterm_spec_anchor::spec_unmodeled(
        machine = "native_update_successor_warm_before_claim",
        action = "ConfigEdit",
        reason = "The environment: the outgoing process applies an edit to aterm.toml, or to a \
                  theme file or feed it names, during the dial's hold. `warm_config_moved`'s \
                  Tier-1 writes the edit into a real file between the real reads."
    )]
    #[aterm_spec_anchor::spec_unmodeled(
        machine = "native_update_successor_warm_before_claim",
        action = "PrepareSocketDir",
        reason = "ControlSocketIdentity::prepare_incoming_directory's chdir, driven by \
                  control_socket_identity's own suites; P2's part is keeping threads away from it, \
                  bound through `warm_order`."
    )]
    #[aterm_spec_anchor::spec_unmodeled(
        machine = "native_update_successor_warm_before_claim",
        action = "SpawnLate",
        reason = "`warm_prologue` on the late order, after the intake (lib.rs `main_entry`): a GPU \
                  device, as WarmDone; the order decision itself is bound through `warm_order`."
    )]
    #[aterm_spec_anchor::spec_unmodeled(
        machine = "native_update_successor_warm_before_claim",
        action = "WarmPresent",
        reason = "P4's Warm-origin present into a hidden window; no code until then."
    )]
    #[aterm_spec_anchor::spec_unmodeled(
        machine = "native_update_successor_warm_before_claim",
        action = "RealPresent",
        reason = "A window's first present of the carried screens (app_render's post-present \
                  hook); a window cannot be driven from a unit test (AGENTS.md)."
    )]
    #[aterm_spec_anchor::spec_unmodeled(
        machine = "native_update_successor_warm_before_claim",
        action = "Reveal",
        reason = "The hidden window's reveal after a real present (app_render); a window, as \
                  RealPresent."
    )]
    #[aterm_spec_anchor::spec_unmodeled(
        machine = "native_update_successor_warm_before_claim",
        action = "Prove",
        reason = "Bound by NativeUpdateSeamlessHandoffOwnership's Tier-1 (ProofMatched); P1 changes \
                  nothing after the intake."
    )]
    #[aterm_spec_anchor::spec_unmodeled(
        machine = "native_update_successor_warm_before_claim",
        action = "Commit",
        reason = "Bound by NativeUpdateSeamlessHandoffOwnership's Tier-1 (Committed)."
    )]
    #[aterm_spec_anchor::spec_unmodeled(
        machine = "native_update_successor_warm_before_claim",
        action = "PublishToEnv",
        reason = "The Buggy=1 mutant: the retired ClaimedHandoff::publish; no shipping code \
                  implements it, and the conformance's negative control replays and catches it."
    )]
    #[aterm_spec_anchor::spec_unmodeled(
        machine = "native_update_successor_warm_before_claim",
        action = "BindWhileWarming",
        reason = "The Buggy=1 mutant: an owned resource taken before the claim; no shipping code."
    )]
    #[aterm_spec_anchor::spec_unmodeled(
        machine = "native_update_successor_warm_before_claim",
        action = "RevealPlaceholder",
        reason = "The Buggy=1 mutant: a warm window shown before the claim; no shipping code."
    )]
    #[aterm_spec_anchor::spec_unmodeled(
        machine = "native_update_successor_warm_before_claim",
        action = "WarmProves",
        reason = "The Buggy=1 mutant: a warm present standing for the carried screens; no \
                  shipping code."
    )]
    #[aterm_spec_anchor::spec_unmodeled(
        machine = "native_update_successor_warm_before_claim",
        action = "FoldHint",
        reason = "The Buggy=1 mutant: the launch hint folded into the digests; no shipping code, \
                  and the conformance replays it and sees it prove something else."
    )]
    #[aterm_spec_anchor::spec_unmodeled(
        machine = "native_update_successor_warm_before_claim",
        action = "WarmBeforeDialOnRelativeSocket",
        reason = "The Buggy=1 mutant: the prologue before the dial on the relative-socket shape; \
                  no shipping code (`warm_order` refuses it), replayed and caught by the \
                  conformance."
    )]
    #[aterm_spec_anchor::spec_unmodeled(
        machine = "native_update_successor_warm_before_claim",
        action = "PresentAtHintedSize",
        reason = "The Buggy=1 mutant: a carried present at a stale hint's size; no shipping code \
                  (`finalize_backend` re-selects first)."
    )]
    #[aterm_spec_anchor::spec_unmodeled(
        machine = "native_update_successor_warm_before_claim",
        action = "PresentUnderStaleConfig",
        reason = "The Buggy=1 mutant: a carried present under the config the early warm read \
                  after the outgoing process applied an edit (P2 as first built); no shipping \
                  code (`main_entry` warms the config again first), replayed and caught by the \
                  conformance."
    )]
    #[aterm_spec_anchor::spec_unmodeled(
        machine = "native_update_successor_warm_before_claim",
        action = "JoinWorkerBeforeDial",
        reason = "The Buggy=1 liveness mutant: a prologue that joins its backend worker before \
                  the dial; no shipping code (the worker is joined only at the first attach)."
    )]
    #[expect(
        dead_code,
        reason = "carrier for the `spec_unmodeled` waivers above; nothing calls it"
    )]
    fn warm_before_claim_waivers() {}

    /// The claim is the launched lane's, macOS's alone.
    #[cfg(not(target_os = "macos"))]
    #[aterm_spec_anchor::spec_unmodeled(
        machine = "native_update_successor_warm_before_claim",
        action = "Dial",
        reason = "The rendezvous dial is macOS's alone; the fork lane inherits its descriptors."
    )]
    #[aterm_spec_anchor::spec_unmodeled(
        machine = "native_update_successor_warm_before_claim",
        action = "ClaimFail",
        reason = "The rendezvous claim is macOS's alone."
    )]
    #[aterm_spec_anchor::spec_unmodeled(
        machine = "native_update_successor_warm_before_claim",
        action = "ClaimOk",
        reason = "The rendezvous claim is macOS's alone."
    )]
    #[expect(
        dead_code,
        reason = "carrier for the `spec_unmodeled` waivers above; nothing calls it"
    )]
    fn claim_is_macos_only() {}

    /// The overlap channels are unix's alone.
    #[cfg(not(unix))]
    #[aterm_spec_anchor::spec_unmodeled(
        machine = "native_update_successor_warm_before_claim",
        action = "Adopt",
        reason = "The overlap channels are a unix mechanism; off unix nothing is adopted."
    )]
    #[expect(
        dead_code,
        reason = "carrier for the `spec_unmodeled` waivers above; nothing calls it"
    )]
    fn overlap_is_unix_only() {}
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The accessors keep the semantics of the calls they replace: `string`
    /// refuses non-UTF-8 as `var` did, `take` is one-shot, `set` keeps the
    /// capture order `exec_env` reports in.
    #[cfg(unix)]
    #[test]
    fn accessors_mirror_the_env_calls_they_replace() {
        use crate::seamless::{ENV_COMMIT_FD, ENV_FDS, ENV_MANIFEST};
        use std::os::unix::ffi::OsStringExt as _;
        let mut env = HandoffEnv::default();
        env.set(ENV_COMMIT_FD, "9");
        env.set(ENV_FDS, OsString::from_vec(vec![0xff, b'1']));
        env.set(ENV_MANIFEST, "/m");
        assert!(env.present(ENV_FDS));
        assert_eq!(env.string(ENV_FDS), None, "not UTF-8");
        assert!(env.os(ENV_FDS).is_some());
        assert_eq!(env.held(), [ENV_MANIFEST, ENV_FDS, ENV_COMMIT_FD]);
        assert_eq!(env.take(ENV_COMMIT_FD), Some("9".into()));
        assert_eq!(env.take(ENV_COMMIT_FD), None, "one-shot");
        env.remove(ENV_FDS);
        assert_eq!(
            env.exec_env(),
            [(
                OsString::from("ATERM_SEAMLESS_MANIFEST"),
                OsString::from("/m")
            )]
        );
    }
}
