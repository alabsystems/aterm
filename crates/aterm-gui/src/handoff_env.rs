// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE HANDOFF ENVIRONMENT SNAPSHOT (docs/DESIGN-warm-successor-2026-09-29.md
//! §1, phase P1).
//!
//! An update handoff hands its successor authority through the process
//! environment: the manifest, nonce, layout and descriptor list
//! (`ATERM_SEAMLESS_*`), the readiness and Commit pipes, the parent's identity,
//! the rendezvous and its claim, the proof term, the grant capabilities and the
//! control-socket witness (`ATERM_HANDOFF_*`). Until this module every one of
//! them was read — and the launched lane's claimed descriptors were even
//! WRITTEN, by `ClaimedHandoff::publish` — through `std::env` at the point of
//! use, so the whole intake had to sit on the single-threaded prologue of
//! `main_entry`: `set_var`/`remove_var` are unsound beside any other thread.
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
//! `tools/grep_guard.sh` B20 fences it: shipped aterm-gui code outside this file
//! may neither read nor mutate a handoff name through the process environment.
//! (A parent naming them on a CHILD's `Command` is not the process environment
//! and is not fenced.) The model is `NativeUpdateSuccessorWarmBeforeClaim`
//! (aterm-spec); its Tier-1 bind is `conformance` below.

use std::ffi::{OsStr, OsString};

/// Every process-environment name a successor reads as handoff authority on
/// THIS platform — and so every name [`HandoffEnv::capture`] takes out of the
/// process. Per platform exactly as the readers were: the rendezvous and the
/// parent identity are unix-only, the proof term and the grant capabilities
/// ride only beside a rendezvous (macOS). `names_are_the_readers_constants`
/// pins each spelling to the constant its reader names it by.
pub(crate) const HANDOFF_ENV_KEYS: &[&str] = &[
    "ATERM_SEAMLESS_MANIFEST",
    "ATERM_SEAMLESS_FDS",
    "ATERM_SEAMLESS_NONCE",
    "ATERM_SEAMLESS_LAYOUT",
    "ATERM_SEAMLESS_TARGET",
    "ATERM_HANDOFF_READY_FD",
    "ATERM_HANDOFF_COMMIT_FD",
    "ATERM_HANDOFF_CONTROL_SOCKET_IDENTITY",
    #[cfg(unix)]
    "ATERM_HANDOFF_RENDEZVOUS",
    #[cfg(unix)]
    "ATERM_HANDOFF_CLAIM",
    #[cfg(unix)]
    "ATERM_HANDOFF_PARENT_PID",
    #[cfg(unix)]
    "ATERM_HANDOFF_PARENT_BIRTH",
    #[cfg(target_os = "macos")]
    "ATERM_HANDOFF_GRANT_CAPS",
    #[cfg(target_os = "macos")]
    "ATERM_HANDOFF_PROOF_TERM",
    // The advisory warm hint (P2): captured like the rest so no helper or
    // shell ever sees it, and read only as a guess (`handoff_warm_hint`).
    #[cfg(target_os = "macos")]
    "ATERM_HANDOFF_WARM_HINT",
    #[cfg(target_os = "macos")]
    "ATERM_HANDOFF_LAUNCHER_NOFILE",
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
    slots: Vec<(&'static str, OsString)>,
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
        test,
        aterm_spec::refines(
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
            if let Some(value) = aterm_log::env::take(key) {
                slots.push((*key, value));
            }
        }
        Self { slots }
    }

    fn slot(&self, key: &str) -> Option<&OsString> {
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
    pub(crate) fn present(&self, key: &str) -> bool {
        self.slot(key).is_some()
    }

    /// The raw value (`std::env::var_os(key)`), not consumed.
    #[must_use]
    pub(crate) fn os(&self, key: &str) -> Option<&OsStr> {
        self.slot(key).map(OsString::as_os_str)
    }

    /// The value as UTF-8 (`std::env::var(key).ok()`), not consumed: a
    /// non-UTF-8 value answers `None`, as that call's `Err` did.
    #[must_use]
    pub(crate) fn string(&self, key: &str) -> Option<String> {
        self.slot(key)
            .and_then(|value| value.to_str())
            .map(str::to_owned)
    }

    /// Read and remove (`aterm_log::env::take(key)`): a one-shot authority is
    /// consumable once from the snapshot, as it was from the environment.
    pub(crate) fn take(&mut self, key: &str) -> Option<OsString> {
        debug_assert!(
            HANDOFF_ENV_KEYS.contains(&key),
            "{key} is not a handoff name this platform captures"
        );
        let index = self.slots.iter().position(|(name, _)| *name == key)?;
        Some(self.slots.remove(index).1)
    }

    /// Remove, discarding the value (`aterm_log::env::unset(key)`).
    pub(crate) fn remove(&mut self, key: &str) {
        let _ = self.take(key);
    }

    /// Set `key` to `value` — the launched lane's granted descriptors landing
    /// where the fork lane's arrive (`handoff_rendezvous::ClaimedIntake`). The
    /// snapshot is this process's own memory: no other thread can observe it,
    /// so unlike the `set_var` it replaces this is sound at any point.
    pub(crate) fn set(&mut self, key: &'static str, value: impl Into<OsString>) {
        let value = value.into();
        let _ = self.take(key);
        // Keep the capture order, so `exec_env` is stable.
        let rank = |name: &str| HANDOFF_ENV_KEYS.iter().position(|key| *key == name);
        let at = self
            .slots
            .iter()
            .position(|(name, _)| rank(name) > rank(key))
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
            .map(|(name, value)| (OsString::from(*name), value.clone()))
            .collect()
    }

    /// The names still held, for the conformance projection.
    #[cfg(test)]
    pub(crate) fn held(&self) -> Vec<&'static str> {
        self.slots.iter().map(|(name, _)| *name).collect()
    }
}

/// THE ENV-SHAPED TEST ENTRY POINTS, kept so the handoff suites run unchanged:
/// a suite sets names in the process, calls the retired env-reading function,
/// and asserts on the process afterwards. This reads `keys` (the footprint the
/// retired function read) into a snapshot WITHOUT clearing them, runs `body`
/// over it, then clears from the process exactly the names `body` consumed —
/// the old function's effect on the process environment, name for name. Test
/// builds only: a shipped reader has the one snapshot `main_entry` captured.
#[cfg(test)]
pub(crate) fn through_process_env<T>(
    keys: &[&'static str],
    body: impl FnOnce(&mut HandoffEnv) -> T,
) -> T {
    let mut env = HandoffEnv::default();
    for key in keys {
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
/// (`crate::warm_order`) and `Reshape` (`crate::warm_miss_px`), all driven by
/// `seamless::handoff_env_conformance`.
#[cfg(test)]
mod waivers {
    #[aterm_spec::spec_unmodeled(
        machine = "native_update_successor_warm_before_claim",
        action = "RelativeSocket",
        reason = "The environment: the launch's own `--control-sock` flag. `warm_order`'s Tier-1 \
                  drives the real decision over every shape, relative included."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "native_update_successor_warm_before_claim",
        action = "WarmDone",
        reason = "The return of `warm_prologue`'s main-thread section (lib.rs): it builds a GPU \
                  device and cannot run in a unit test (AGENTS.md rule 5); the conformance \
                  realizes it as a stand-in that returns without joining its thread, and asserts \
                  that thread is still running at the real dial."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "native_update_successor_warm_before_claim",
        action = "StaleHint",
        reason = "The environment: the outgoing process's window changed between its launch-time \
                  hint and its capture. `warm_miss_px`'s Tier-1 drives the real hint parser over \
                  stale and hostile hints."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "native_update_successor_warm_before_claim",
        action = "PrepareSocketDir",
        reason = "ControlSocketIdentity::prepare_incoming_directory's chdir, driven by \
                  control_socket_identity's own suites; P2's part is keeping threads away from it, \
                  bound through `warm_order`."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "native_update_successor_warm_before_claim",
        action = "SpawnLate",
        reason = "`warm_prologue` on the late order, after the intake (lib.rs `main_entry`): a GPU \
                  device, as WarmDone; the order decision itself is bound through `warm_order`."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "native_update_successor_warm_before_claim",
        action = "WarmPresent",
        reason = "P4's Warm-origin present into a hidden window; no code until then."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "native_update_successor_warm_before_claim",
        action = "RealPresent",
        reason = "A window's first present of the carried screens (app_render's post-present \
                  hook); a window cannot be driven from a unit test (AGENTS.md rule 5)."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "native_update_successor_warm_before_claim",
        action = "Reveal",
        reason = "The hidden window's reveal after a real present (app_render); a window, as \
                  RealPresent."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "native_update_successor_warm_before_claim",
        action = "Prove",
        reason = "Bound by NativeUpdateSeamlessHandoffOwnership's Tier-1 (ProofMatched); P1 changes \
                  nothing after the intake."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "native_update_successor_warm_before_claim",
        action = "Commit",
        reason = "Bound by NativeUpdateSeamlessHandoffOwnership's Tier-1 (Committed)."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "native_update_successor_warm_before_claim",
        action = "PublishToEnv",
        reason = "The Buggy=1 mutant: the retired ClaimedHandoff::publish; no shipping code \
                  implements it, and the conformance's negative control replays and catches it."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "native_update_successor_warm_before_claim",
        action = "BindWhileWarming",
        reason = "The Buggy=1 mutant: an owned resource taken before the claim; no shipping code."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "native_update_successor_warm_before_claim",
        action = "RevealPlaceholder",
        reason = "The Buggy=1 mutant: a warm window shown before the claim; no shipping code."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "native_update_successor_warm_before_claim",
        action = "WarmProves",
        reason = "The Buggy=1 mutant: a warm present standing for the carried screens; no \
                  shipping code."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "native_update_successor_warm_before_claim",
        action = "FoldHint",
        reason = "The Buggy=1 mutant: the launch hint folded into the digests; no shipping code, \
                  and the conformance replays it and sees it prove something else."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "native_update_successor_warm_before_claim",
        action = "WarmBeforeDialOnRelativeSocket",
        reason = "The Buggy=1 mutant: the prologue before the dial on the relative-socket shape; \
                  no shipping code (`warm_order` refuses it), replayed and caught by the \
                  conformance."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "native_update_successor_warm_before_claim",
        action = "PresentAtHintedSize",
        reason = "The Buggy=1 mutant: a carried present at a stale hint's size; no shipping code \
                  (`finalize_backend` re-selects first)."
    )]
    #[aterm_spec::spec_unmodeled(
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
    #[aterm_spec::spec_unmodeled(
        machine = "native_update_successor_warm_before_claim",
        action = "Dial",
        reason = "The rendezvous dial is macOS's alone; the fork lane inherits its descriptors."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "native_update_successor_warm_before_claim",
        action = "ClaimFail",
        reason = "The rendezvous claim is macOS's alone."
    )]
    #[aterm_spec::spec_unmodeled(
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
    #[aterm_spec::spec_unmodeled(
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

    /// Each captured spelling is the constant its reader names it by, and every
    /// reader's constant is captured — so no reader can be left reading a name
    /// `capture` already cleared, or a name `capture` never took.
    #[test]
    fn names_are_the_readers_constants() {
        let mut readers = vec![
            crate::seamless::ENV_FDS,
            crate::seamless::ENV_READY_FD,
            crate::seamless::ENV_COMMIT_FD,
            crate::seamless::ENV_MANIFEST,
            crate::seamless::ENV_NONCE,
            crate::seamless::ENV_LAYOUT,
            crate::seamless::ENV_TARGET,
            crate::control_socket_identity::ENV_IDENTITY,
        ];
        #[cfg(unix)]
        readers.extend([
            crate::seamless::ENV_RENDEZVOUS,
            crate::seamless::ENV_CLAIM,
            crate::seamless::ENV_PARENT_PID,
            crate::seamless::ENV_PARENT_BIRTH,
        ]);
        #[cfg(target_os = "macos")]
        readers.extend([
            crate::handoff_rendezvous::ENV_GRANT_CAPS,
            crate::handoff_rendezvous::ENV_PROOF_TERM,
            crate::handoff_warm_hint::ENV_WARM_HINT,
            crate::handoff_rendezvous::ENV_LAUNCHER_NOFILE,
        ]);
        let mut captured = HANDOFF_ENV_KEYS.to_vec();
        readers.sort_unstable();
        captured.sort_unstable();
        assert_eq!(readers, captured);
    }

    /// The accessors keep the semantics of the calls they replace: `string`
    /// refuses non-UTF-8 as `var` did, `take` is one-shot, `set` keeps the
    /// capture order `exec_env` reports in.
    #[cfg(unix)]
    #[test]
    fn accessors_mirror_the_env_calls_they_replace() {
        use std::os::unix::ffi::OsStringExt as _;
        let mut env = HandoffEnv::default();
        env.set("ATERM_HANDOFF_COMMIT_FD", "9");
        env.set("ATERM_SEAMLESS_FDS", OsString::from_vec(vec![0xff, b'1']));
        env.set("ATERM_SEAMLESS_MANIFEST", "/m");
        assert!(env.present("ATERM_SEAMLESS_FDS"));
        assert_eq!(env.string("ATERM_SEAMLESS_FDS"), None, "not UTF-8");
        assert!(env.os("ATERM_SEAMLESS_FDS").is_some());
        assert_eq!(
            env.held(),
            [
                "ATERM_SEAMLESS_MANIFEST",
                "ATERM_SEAMLESS_FDS",
                "ATERM_HANDOFF_COMMIT_FD"
            ]
        );
        assert_eq!(env.take("ATERM_HANDOFF_COMMIT_FD"), Some("9".into()));
        assert_eq!(env.take("ATERM_HANDOFF_COMMIT_FD"), None, "one-shot");
        env.remove("ATERM_SEAMLESS_FDS");
        assert_eq!(
            env.exec_env(),
            [(
                OsString::from("ATERM_SEAMLESS_MANIFEST"),
                OsString::from("/m")
            )]
        );
    }
}
