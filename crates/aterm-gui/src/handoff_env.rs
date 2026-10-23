// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Single-threaded process-environment capture and in-memory snapshot for seamless handoff.

use std::ffi::{OsStr, OsString};

/// Environment variable name carrying seamless handoff authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct HandoffName(&'static str);

impl HandoffName {
    /// Declares a handoff environment variable name.
    pub(crate) const fn new(name: &'static str) -> Self {
        Self(name)
    }

    /// Pairs this handoff variable name with `value` for a child environment.
    #[cfg(unix)]
    #[must_use]
    pub(crate) fn pair(self, value: impl AsRef<OsStr>) -> (OsString, OsString) {
        (OsString::from(self.0), value.as_ref().to_os_string())
    }

    /// Returns the raw variable name for test assertions.
    #[cfg(all(test, unix))]
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

/// Handoff environment variable names captured and cleared by [`HandoffEnv::capture`].
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
    #[cfg(unix)]
    crate::seamless::ENV_EXIT_CAPS,
    #[cfg(target_os = "macos")]
    crate::handoff_rendezvous::ENV_GRANT_CAPS,
    #[cfg(target_os = "macos")]
    crate::handoff_rendezvous::ENV_PROOF_TERM,
    #[cfg(target_os = "macos")]
    crate::handoff_warm_hint::ENV_WARM_HINT,
    #[cfg(unix)]
    crate::descriptor_limit::ENV_LAUNCHER_NOFILE,
    #[cfg(unix)]
    crate::descriptor_limit::ENV_PARK_RAISED_FROM,
];

/// In-memory snapshot of the handoff authority variables captured at process start.
#[derive(Default)]
pub(crate) struct HandoffEnv {
    /// `(name, value)` in [`HANDOFF_ENV_KEYS`] order; a name appears at most
    /// once, and only while present.
    slots: Vec<(HandoffName, OsString)>,
}

impl HandoffEnv {
    /// Reads and clears every [`HANDOFF_ENV_KEYS`] entry from the process environment once.
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
            // `take` removes the name whether or not it was set, so no handoff name is left
            // for a helper, a shell or a later reader to observe.
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

    /// Returns whether `key` is present in the snapshot.
    #[must_use]
    pub(crate) fn present(&self, key: HandoffName) -> bool {
        self.slot(key).is_some()
    }

    /// Returns the raw `OsStr` value of `key` without consuming it.
    #[must_use]
    pub(crate) fn os(&self, key: HandoffName) -> Option<&OsStr> {
        self.slot(key).map(OsString::as_os_str)
    }

    /// Returns the UTF-8 value of `key` without consuming it.
    #[must_use]
    pub(crate) fn string(&self, key: HandoffName) -> Option<String> {
        self.slot(key)
            .and_then(|value| value.to_str())
            .map(str::to_owned)
    }

    /// Removes and returns the value of `key`.
    pub(crate) fn take(&mut self, key: HandoffName) -> Option<OsString> {
        debug_assert!(
            HANDOFF_ENV_KEYS.contains(&key),
            "{key} is not a handoff name this platform captures"
        );
        let index = self.slots.iter().position(|(name, _)| *name == key)?;
        Some(self.slots.remove(index).1)
    }

    /// Removes `key` from the snapshot.
    pub(crate) fn remove(&mut self, key: HandoffName) {
        let _ = self.take(key);
    }

    /// Sets `key` to `value` in capture order.
    #[cfg(any(target_os = "macos", all(test, unix)))]
    pub(crate) fn set(&mut self, key: HandoffName, value: impl Into<OsString>) {
        let value = value.into();
        let _ = self.take(key);
        let rank = |name: HandoffName| HANDOFF_ENV_KEYS.iter().position(|key| *key == name);
        let at = self
            .slots
            .iter()
            .position(|(name, _)| rank(*name) > rank(key))
            .unwrap_or(self.slots.len());
        self.slots.insert(at, (key, value));
    }

    /// Returns the remaining `(name, value)` pairs to restore across re-exec.
    #[must_use]
    pub(crate) fn exec_env(&self) -> Vec<(OsString, OsString)> {
        self.slots
            .iter()
            .map(|(name, value)| (OsString::from(name.0), value.clone()))
            .collect()
    }

    /// Returns the variable names still held in the snapshot.
    #[cfg(all(test, unix))]
    pub(crate) fn held(&self) -> Vec<HandoffName> {
        self.slots.iter().map(|(name, _)| *name).collect()
    }
}

/// Test adapter that captures `keys` from the process environment and runs `body`.
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// The accessors mirror the env calls: `string` refuses non-UTF-8 as `var` does, `take` is
    /// one-shot, `set` keeps the capture order `exec_env` reports in.
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

/// The machine's actions with no unit-drivable code, each waived by name.
#[cfg(any(test, debug_assertions))]
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
#[allow(
    dead_code,
    reason = "carrier for the `spec_unmodeled` waivers above; nothing calls it. An allow, \
              not an expect: rustc does not lint a fn the declarative anchors rebuild"
)]
fn warm_before_claim_waivers() {}

/// The claim is the launched lane's, macOS's alone.
#[cfg(all(any(test, debug_assertions), not(target_os = "macos")))]
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
#[allow(
    dead_code,
    reason = "carrier for the `spec_unmodeled` waivers above; nothing calls it. An allow, \
              not an expect: rustc does not lint a fn the declarative anchors rebuild"
)]
fn claim_is_macos_only() {}

/// The overlap channels are unix's alone.
#[cfg(all(any(test, debug_assertions), not(unix)))]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "native_update_successor_warm_before_claim",
    action = "Adopt",
    reason = "The overlap channels are a unix mechanism; off unix nothing is adopted."
)]
#[allow(
    dead_code,
    reason = "carrier for the `spec_unmodeled` waivers above; nothing calls it. An allow, \
              not an expect: rustc does not lint a fn the declarative anchors rebuild"
)]
fn overlap_is_unix_only() {}
