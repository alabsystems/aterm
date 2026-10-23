// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Seamless update handoff, candidate launch, reader parking, and apply ladder execution.
#[cfg(unix)]
use aterm_window::event_loop::ActiveEventLoop;

use crate::App;
#[cfg(unix)]
use crate::Wake;
#[cfg(unix)]
use crate::app_input::paste_order;

#[cfg(clean_islands)]
#[path = "app_update_handoff_island.rs"]
mod island;

#[cfg(all(test, target_os = "macos"))]
#[path = "app_update_handoff_dial_wait_tests.rs"]
pub(crate) mod launched_dial_wait_conformance;

#[cfg(all(test, target_os = "macos"))]
#[path = "app_update_handoff_dialer_admission_tests.rs"]
pub(crate) mod launched_dialer_admission_conformance;

#[cfg(unix)]
struct HandoffCapture {
    #[cfg(target_os = "macos")]
    park_at: std::time::Instant,
    #[cfg(target_os = "macos")]
    captured_at: std::time::Instant,
    manifest: crate::session_store::SessionHandoff,
    fds: crate::session_store::HandoffFds,
    screens: Vec<(u64, aterm_core::terminal::TerminalCheckpoint)>,
    repaint: Vec<u64>,
    carries: Vec<crate::handoff_carry::CarrySource>,
    window: Option<crate::session_store::WindowCarry>,
    layout: crate::restore::RestoreManifest,
    layout_digest: [u8; 32],
    screen_digest: [u8; 32],
    live: Vec<(u64, i32, i32)>,
    proof_identities: Vec<(u64, i32, i32)>,
    _owned_masters: Vec<std::os::fd::OwnedFd>,
    history_heads: Vec<crate::handoff_history::HistoryHead>,
    history: crate::handoff_history::HistoryPlan,
    held: Vec<crate::seamless::HeldScreen>,
}

/// The park's product, sent from the main thread to a worker that is already holding a successor's claim.
#[cfg(any(target_os = "macos", all(test, unix)))]
pub(crate) struct HandoffTransferJob {
    capture: HandoffCapture,
    proof_deadline: std::time::Instant,
}

/// The main thread's typed reason for standing a held successor down before the park delivered anything.
#[cfg(any(target_os = "macos", all(test, unix)))]
#[derive(Debug)]
pub(crate) struct HandoffStandDown {
    pub(crate) outcome: crate::UpdateHandoffOutcome,
    pub(crate) detail: String,
}

#[cfg(target_os = "macos")]
struct HandoffPrelaunchLane {
    nonce: String,
    transfer: std::sync::mpsc::Receiver<HandoffTransferJob>,
    stand_down: std::sync::mpsc::Receiver<HandoffStandDown>,
    stand_down_ack: std::sync::mpsc::Receiver<()>,
    hold_bound: std::time::Duration,
    warm_hint: String,
    speculative_layout: Option<crate::restore::RestoreManifest>,
    successor_runs_candidate: bool,
}

#[cfg(target_os = "macos")]
impl HandoffPrelaunchLane {
    fn dialer_code(
        &self,
        team: &str,
        bundle: &std::path::Path,
        candidate_identifier: impl FnOnce() -> Result<String, String>,
    ) -> Result<crate::handoff_rendezvous::DialerCode, String> {
        launched_dialer_code(
            self.successor_runs_candidate,
            team,
            bundle,
            candidate_identifier,
        )
    }
}

#[cfg(unix)]
struct HandoffWorkerJob {
    attempt_id: u64,
    current_build: u64,
    target_build: u64,
    target_commit: String,
    verify_staged_candidate: bool,
    installed_activation: bool,
    preverified: Option<PreverifyPublisher>,
    parked_under: Option<aterm_update_core::handoff_policy::CarryCeiling>,
    successor_grant_chunks: bool,
    command: aterm_uds::spawnfd::StdCommand,
    capture: Option<HandoffCapture>,
    #[cfg(target_os = "macos")]
    prelaunch: Option<HandoffPrelaunchLane>,
    #[cfg(target_os = "macos")]
    lane: HandoffLane,
    #[cfg(target_os = "macos")]
    bundle: Option<std::path::PathBuf>,
    #[cfg(target_os = "macos")]
    expected: Option<(u64, String, String)>,
    #[cfg(target_os = "macos")]
    renews_launch_record: bool,
    cleanup: HandoffWorkerCleanup,
    cancel: std::sync::mpsc::Receiver<()>,
    arbiter: crate::HandoffAttemptArbiter,
}

/// What the successor's signed handoff policy asks of one park, as [`crate::App::park_policy`] reads it for every lane.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ParkPolicy {
    pub(crate) policy: Option<aterm_update_core::handoff_policy::HandoffPolicy>,
    pub(crate) known: bool,
    pub(crate) grant_chunks: bool,
}

#[cfg(unix)]
impl ParkPolicy {
    /// The highest rung the park may carry a session at.
    #[must_use]
    pub(crate) fn carry_ceiling(self) -> aterm_update_core::handoff_policy::CarryCeiling {
        self.policy.map_or(
            aterm_update_core::handoff_policy::CarryCeiling::Full,
            |policy| policy.carry_ceiling(),
        )
    }

    /// Whether the automatic lane's `Land` phase may park over queued output.
    #[cfg(any(target_os = "macos", all(test, unix)))]
    #[must_use]
    pub(crate) fn relaxes_land_gate(self) -> bool {
        self.policy.is_some_and(|policy| policy.relaxes_land_gate())
    }
}

#[cfg(unix)]
fn held_capture_deadline(
    deadline: std::time::Instant,
    now: std::time::Instant,
    comfort: std::time::Duration,
) -> Option<std::time::Instant> {
    if deadline.saturating_duration_since(now) < comfort {
        return None;
    }
    Some(deadline.checked_sub(comfort / 2).unwrap_or(deadline))
}

/// Refuses a fork-lane park taken above its successor's policy, before the child starts.
#[cfg(unix)]
pub(crate) fn capture_exceeds_policy(
    parked_under: Option<aterm_update_core::handoff_policy::CarryCeiling>,
    adopted: Option<aterm_update_core::handoff_policy::HandoffPolicy>,
) -> Option<String> {
    let parked_under = parked_under?;
    let policy = adopted?;
    (policy.carry_ceiling() > parked_under).then(|| {
        format!(
            "the successor's handoff policy ({policy}) asks for less than this park carried \
             (carry={}), and the fork lane read it only after parking; nothing was handed \
             over, and the next attempt parks under it",
            parked_under.as_str()
        )
    })
}

#[cfg(unix)]
struct PreverifyPublisher {
    slot: std::sync::Arc<std::sync::Mutex<Option<crate::HandoffPreverification>>>,
    build: u64,
    commit: String,
    artifact: String,
}

#[cfg(unix)]
impl PreverifyPublisher {
    fn for_attempt(
        slot: &std::sync::Arc<std::sync::Mutex<Option<crate::HandoffPreverification>>>,
        attempt: Option<&crate::native_updater_service::ApplyAttemptTicket>,
    ) -> Option<Self> {
        let attempt = attempt?;
        Some(Self {
            slot: std::sync::Arc::clone(slot),
            build: attempt.target_build(),
            commit: attempt.target_commit().to_string(),
            artifact: attempt.target_dmg_sha256().to_string(),
        })
    }

    fn publish_pass(
        &self,
        facts: &aterm_update::HandoffCandidateFacts,
        current_build: u64,
        candidate: &str,
    ) -> Option<aterm_update_core::handoff_policy::HandoffPolicy> {
        let policy =
            aterm_update_core::handoff_policy::adopt(&facts.policy, current_build, candidate);
        *self
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(crate::HandoffPreverification {
                build: self.build,
                commit: self.commit.clone(),
                artifact: self.artifact.clone(),
                at: std::time::Instant::now(),
                passed: true,
                reason: None,
                policy,
                grant_chunks: facts.grant_chunks,
            });
        policy
    }

    fn publish_refusal(&self, reason: &str) {
        if aterm_update::is_passing_refusal(reason) {
            return;
        }
        *self
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(crate::HandoffPreverification {
                build: self.build,
                commit: self.commit.clone(),
                artifact: self.artifact.clone(),
                at: std::time::Instant::now(),
                passed: false,
                reason: Some(reason.to_string()),
                policy: None,
                grant_chunks: false,
            });
    }

    #[cfg(test)]
    fn publish_policy_pass(
        &self,
        read: &aterm_update_core::handoff_policy::PolicyRead,
        current_build: u64,
        candidate: &str,
    ) -> Option<aterm_update_core::handoff_policy::HandoffPolicy> {
        self.publish_pass(
            &aterm_update::HandoffCandidateFacts {
                policy: read.clone(),
                grant_chunks: false,
            },
            current_build,
            candidate,
        )
    }
}

#[cfg(unix)]
impl HandoffWorkerJob {
    const fn capture(&self) -> &HandoffCapture {
        self.capture
            .as_ref()
            .expect("the handoff worker reads the capture only after the park delivered it")
    }
}

/// Why an attempt may run with no staged artifact to authenticate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SameImageHandoff {
    DebugSeam,
    #[cfg(target_os = "macos")]
    RenewLaunchRecord,
}

impl SameImageHandoff {
    #[must_use]
    #[cfg_attr(
        not(unix),
        expect(dead_code, reason = "only the unix handoff lanes ask")
    )]
    pub(crate) const fn renews_launch_record(self) -> bool {
        #[cfg(target_os = "macos")]
        {
            matches!(self, Self::RenewLaunchRecord)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = self;
            false
        }
    }
}

#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HandoffLane {
    Fork,
    OutOfBand,
}

#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HandoffLaneFacts {
    bundled: bool,
    launcher_available: bool,
    socket_path_fits: bool,
    target_not_older: bool,
    sessions: usize,
    grant_chunks: bool,
    environment_is_a_merge: bool,
}

#[cfg(target_os = "macos")]
#[must_use]
const fn out_of_band_lane_refusal(facts: HandoffLaneFacts) -> Option<&'static str> {
    if !facts.launcher_available {
        return Some("this platform has no LaunchServices launcher");
    }
    if !facts.bundled {
        return Some("this process does not run from a .app bundle");
    }
    if !facts.socket_path_fits {
        return Some("the rendezvous path does not fit sun_path on this machine");
    }
    if !facts.target_not_older {
        return Some("the authorized target build is older than this build");
    }
    if facts.sessions == 0
        || facts.sessions > crate::handoff_rendezvous::grant_session_limit(facts.grant_chunks)
    {
        return Some(if facts.grant_chunks {
            "the session count does not fit the successor's chunked descriptor grant"
        } else {
            "the session count does not fit one descriptor message"
        });
    }
    if !facts.environment_is_a_merge {
        return Some("the launch environment needs a removal a merge cannot express");
    }
    None
}

/// The `.app` root containing `exe` (`<bundle>.app/Contents/MacOS/<bin>`), if any.
#[cfg(target_os = "macos")]
#[must_use]
pub(crate) fn app_bundle_root(exe: &std::path::Path) -> Option<std::path::PathBuf> {
    let bundle = exe.parent()?.parent()?.parent()?;
    (bundle
        .extension()
        .is_some_and(|extension| extension == "app")
        && bundle.is_dir())
    .then(|| bundle.to_path_buf())
}

#[cfg(unix)]
#[derive(Clone, Default)]
struct HandoffWorkerCleanup {
    parent_socket: Option<(std::path::PathBuf, String)>,
    reconcile: Option<(
        crate::app_native::NativeUpdateReconcileSender,
        crate::app_native::NativeUpdateReconcileTicket,
    )>,
}

#[cfg(unix)]
const fn successor_reads_fabric_carry(target_build: u64, running_build: u64) -> bool {
    target_build >= running_build
}

/// The hold fence the Commit raises ([`App::fence_hold_serials`]).
#[cfg(unix)]
pub(crate) struct HoldFence(Vec<std::sync::Arc<crate::fabric::SessionFabric>>);

#[cfg(unix)]
impl Drop for HoldFence {
    fn drop(&mut self) {
        for fabric in &self.0 {
            fabric.lift_hold_fence();
        }
    }
}

#[cfg(unix)]
impl HoldFence {
    fn serials(&self) -> u64 {
        self.0
            .iter()
            .map(|fabric| fabric.carry_serial())
            .fold(0, u64::wrapping_add)
    }
}

/// A Commit that holds its claim through one more input-dispatch fence.
#[cfg(unix)]
pub(crate) struct HeldCommit {
    hold_fence: HoldFence,
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HandoffCommitFacts {
    exact_sessions: bool,
    exact_layout: bool,
    exact_agent_carry: bool,
    recovery_count_ready: bool,
    exact_activity: bool,
    teardown_allows_commit: bool,
    parent_still_parked: bool,
    sessions_alive: bool,
    input_dispatch_fenced: bool,
    egress_settled: bool,
    native_safe: bool,
    proof_exact: bool,
    commit_channel: bool,
}

#[cfg(unix)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativeUpdateSettingsDraftCarry",
        action = "Commit",
        project = "aterm_gui::settings_draft_carry_conformance::Rig::project"
    )
)]
pub(crate) fn commit_layout_topology(
    layout: &crate::restore::RestoreManifest,
) -> crate::restore::RestoreManifest {
    fn strip_pane(node: &mut crate::restore::PaneLayout) {
        match node {
            crate::restore::PaneLayout::Leaf { cwd, title, .. } => {
                *cwd = None;
                title.clear();
            }
            crate::restore::PaneLayout::Split { first, second, .. } => {
                strip_pane(first);
                strip_pane(second);
            }
        }
    }

    fn strip_tree(node: &mut crate::restore::RestoredSplitTree) {
        match node {
            crate::restore::RestoredSplitTree::Leaf {
                view: crate::restore::RestoredView::Terminal(terminal),
            } => {
                terminal.cwd = None;
                terminal.title.clear();
                terminal.agent = None;
                terminal.supervised = None;
                terminal.lost = None;
            }
            crate::restore::RestoredSplitTree::Leaf { .. } => {}
            crate::restore::RestoredSplitTree::Split { first, second, .. } => {
                strip_tree(first);
                strip_tree(second);
            }
        }
    }

    let mut topology = layout.clone();
    for window in &mut topology.windows {
        window.outer_x = None;
        window.outer_y = None;
        window.maximized = None;
        window.show = crate::restore::WindowShow::UNKNOWN;
        window.inner_px = None;
        window.presence_row = false;
        for tab in &mut window.tabs {
            strip_pane(tab);
        }
        for tab in &mut window.restored_tabs {
            strip_tree(&mut tab.root);
        }
    }
    topology
}

/// What the handoff layout's leaves carry of the supervisor host's agents, in layout order.
#[cfg(unix)]
pub(crate) fn commit_agent_carry(layout: &crate::restore::RestoreManifest) -> Vec<AgentCarry> {
    layout
        .handed_off_leaves()
        .into_iter()
        .map(|leaf| {
            let watched = leaf.supervised.map(|agent| (agent.pid, agent.start));
            (leaf.local_id, leaf.agent, watched, leaf.lost)
        })
        .collect()
}

/// One handoff leaf's carry of the supervisor host's agents ([`commit_agent_carry`]).
#[cfg(unix)]
pub(crate) type AgentCarry = (
    Option<u64>,
    Option<Box<crate::restore::AgentRestore>>,
    Option<(u32, String)>,
    Option<Box<crate::restore::LostRestore>>,
);

#[cfg(unix)]
#[must_use]
const fn handoff_commit_admitted(facts: HandoffCommitFacts) -> bool {
    facts.exact_sessions
        && facts.exact_layout
        && facts.exact_agent_carry
        && facts.recovery_count_ready
        && facts.exact_activity
        && facts.teardown_allows_commit
        && facts.parent_still_parked
        && facts.sessions_alive
        && facts.input_dispatch_fenced
        && facts.egress_settled
        && facts.native_safe
        && facts.proof_exact
        && facts.commit_channel
}

#[cfg(unix)]
fn handoff_rejection_reason(
    facts: HandoffCommitFacts,
    native_safety: &Result<crate::app_native::NativeUpdateSafetyToken, Vec<String>>,
    commit_lost_arbiter: bool,
    commit_write_failed: bool,
) -> String {
    if !facts.exact_sessions {
        "live terminal set changed during async preparation".to_string()
    } else if !facts.exact_layout {
        "window/tab/pane topology changed during async preparation".to_string()
    } else if !facts.exact_agent_carry {
        "an agent's relaunch became owed or was settled during async preparation".to_string()
    } else if !facts.recovery_count_ready {
        "recovery count custody unconfirmed".to_string()
    } else if !facts.exact_activity {
        "structural activity arrived before Commit".to_string()
    } else if !facts.teardown_allows_commit {
        "destructive intent revoked Commit before teardown replay".to_string()
    } else if !facts.sessions_alive {
        "a handed-off PTY session closed before Commit".to_string()
    } else if !facts.input_dispatch_fenced {
        "the OS input queue did not dispatch into the masters before Commit".to_string()
    } else if !facts.egress_settled {
        "tolerated input was still on its way to a PTY at Commit (it outlasted the \
         egress-flush budget, or began after it)"
            .to_string()
    } else if !facts.parent_still_parked {
        "a parent PTY reader resumed before Commit".to_string()
    } else if let Err(reasons) = native_safety {
        format!(
            "native safety changed before Commit: {}",
            reasons.join(" · ")
        )
    } else if commit_lost_arbiter {
        "worker atomically revoked the handoff before Commit".to_string()
    } else if commit_write_failed {
        "attempt-bound Commit pipe closed before its atomic write".to_string()
    } else {
        "attempt-bound Commit could not be delivered atomically".to_string()
    }
}

#[cfg(unix)]
const fn handoff_rejection_activity_shaped(facts: HandoffCommitFacts) -> bool {
    !facts.exact_sessions
        || !facts.exact_layout
        || !facts.exact_agent_carry
        || !facts.recovery_count_ready
        || !facts.exact_activity
        || !facts.teardown_allows_commit
        || !facts.sessions_alive
        || !facts.input_dispatch_fenced
        || !facts.egress_settled
}

#[cfg(unix)]
fn emergency_reap_and_report(
    child_pid: u32,
    attempt_id: u64,
    arbiter: &crate::HandoffAttemptArbiter,
    cleanup: &HandoffWorkerCleanup,
    nonce: Option<String>,
    detail: String,
    proxy: &aterm_window::event_loop::EventLoopProxy<Wake>,
) {
    emergency_kill_and_reap_handoff_child(child_pid).announce(Some(child_pid));
    let completed = arbiter.finish_reap(crate::HandoffReaperOwner::Emergency);
    debug_assert!(completed, "emergency reaper retained sole ownership");
    cleanup.complete(nonce.as_deref());
    let _ = proxy.send_event(Wake::UpdateHandoffFinished(
        crate::UpdateHandoffCompletion {
            attempt_id,
            nonce,
            child_pid: Some(child_pid),
            outcome: crate::UpdateHandoffOutcome::Rejected,
            commit_fd: None,
            reject: None,
            reconcile: None,
            detail,
            input_drain_spins: 0,
            child_death: crate::ChildDeathEvidence::Unobserved,
        },
    ));
}

#[cfg(unix)]
impl HandoffWorkerCleanup {
    fn complete(&self, nonce: Option<&str>) {
        if let Some(nonce) = nonce {
            crate::seamless::discard_outgoing(nonce);
        }
        if let Some((latest_link, socket_path)) = &self.parent_socket {
            crate::control_auth::publish_latest_link(latest_link, socket_path);
        }
    }
}

#[cfg(unix)]
fn make_cloexec_pipe() -> Option<(std::os::fd::OwnedFd, std::os::fd::OwnedFd)> {
    use std::os::fd::FromRawFd as _;
    let mut raw = [0i32; 2];
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let created = unsafe { libc::pipe2(raw.as_mut_ptr(), libc::O_CLOEXEC) };
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    let created = unsafe { libc::pipe(raw.as_mut_ptr()) };
    if created != 0 {
        return None;
    }
    // SAFETY: fresh pipe fds, exclusively owned from here.
    let rd = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw[0]) };
    let wr = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw[1]) };
    use std::os::fd::AsRawFd as _;
    if aterm_pty::set_cloexec(rd.as_raw_fd(), true).is_err()
        || aterm_pty::set_cloexec(wr.as_raw_fd(), true).is_err()
    {
        return None;
    }
    Some((rd, wr))
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
enum HandoffRollbackWarrant {
    NoCandidate,
    #[cfg(target_os = "macos")]
    NeverTransferred,
    Reaped,
    Vanished,
}

#[cfg(unix)]
impl HandoffRollbackWarrant {
    fn announce(self, candidate_pid: Option<u32>) {
        if self == Self::Vanished {
            aterm_log::info!(
                "update apply: rollback licensed by outside proof; candidate {candidate_pid:?} \
                 terminated without being ours to reap"
            );
        }
    }
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HandoffCandidateBirth {
    seconds: u64,
    microseconds: u64,
}

#[cfg(target_os = "macos")]
fn read_candidate_birth(pid: u32) -> Option<HandoffCandidateBirth> {
    let pid = libc::pid_t::try_from(pid).ok()?;
    if pid <= 1 {
        return None;
    }
    let info = crate::quit_safety::pid_bsdinfo(pid)?;
    Some(HandoffCandidateBirth {
        seconds: info.pbi_start_tvsec,
        microseconds: info.pbi_start_tvusec,
    })
}

#[cfg(all(unix, not(target_os = "macos")))]
fn read_candidate_birth(_pid: u32) -> Option<HandoffCandidateBirth> {
    None
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HandoffCandidate {
    pid: u32,
    birth: Option<HandoffCandidateBirth>,
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HandoffCandidateIdentity {
    Corroborated,
    Recycled,
    Unwitnessed,
}

#[cfg(unix)]
impl HandoffCandidate {
    fn of_unreaped_child(child: &std::process::Child) -> Self {
        let pid = child.id();
        Self {
            pid,
            birth: read_candidate_birth(pid),
        }
    }

    #[cfg(test)]
    fn of_unreaped_spawned(child: &aterm_uds::spawnfd::Child) -> Self {
        let pid = child.id();
        Self {
            pid,
            birth: read_candidate_birth(pid),
        }
    }

    #[cfg(target_os = "macos")]
    fn of_attested_peer(pid: u32) -> Self {
        Self {
            pid,
            birth: read_candidate_birth(pid),
        }
    }

    #[cfg(target_os = "macos")]
    fn of_named_pid(pid: u32) -> Self {
        Self {
            pid,
            birth: read_candidate_birth(pid),
        }
    }

    #[cfg(target_os = "macos")]
    fn of_stamped_launch(launch: aterm_update_core::LaunchName) -> Self {
        let (seconds, microseconds) = launch.started;
        Self {
            pid: launch.pid,
            birth: u64::try_from(seconds)
                .ok()
                .zip(u64::try_from(microseconds).ok())
                .map(|(seconds, microseconds)| HandoffCandidateBirth {
                    seconds,
                    microseconds,
                }),
        }
    }

    const fn from_bare_pid(pid: u32) -> Self {
        Self { pid, birth: None }
    }

    fn identity(self) -> HandoffCandidateIdentity {
        match (self.birth, read_candidate_birth(self.pid)) {
            (Some(captured), Some(current)) if captured == current => {
                HandoffCandidateIdentity::Corroborated
            }
            (Some(_), Some(_)) => HandoffCandidateIdentity::Recycled,
            _ => HandoffCandidateIdentity::Unwitnessed,
        }
    }
}

#[cfg(unix)]
#[must_use]
fn handoff_candidate_terminated(candidate: HandoffCandidate) -> bool {
    let Ok(pid) = libc::pid_t::try_from(candidate.pid) else {
        return false;
    };
    // SAFETY: signal 0 checks process existence and permission without delivering a signal.
    if unsafe { libc::kill(pid, 0) } != 0
        && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    {
        return true;
    }
    candidate.identity() == HandoffCandidateIdentity::Recycled
}

/// Whether this process leads its own process group, so a rejecting parent's `kill` reaches the updater helpers a candidate forks.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProcessGroupContainment {
    OwnGroupLeader,
    Foreign {
        group: libc::pid_t,
        own: libc::pid_t,
    },
}

/// Puts the calling process in its own process group and proves it, so helpers it forks are inside the group a rejecting parent sweeps.
#[cfg(unix)]
#[must_use]
pub(crate) fn contain_own_process_group() -> ProcessGroupContainment {
    // SAFETY: `setpgid(0, 0)`, `getpgrp`, and `getpid` act only on the calling process.
    let (group, own) = unsafe {
        let _ = libc::setpgid(0, 0);
        (libc::getpgrp(), libc::getpid())
    };
    if group == own {
        ProcessGroupContainment::OwnGroupLeader
    } else {
        ProcessGroupContainment::Foreign { group, own }
    }
}

#[cfg(unix)]
fn candidate_is_our_child(pid: libc::pid_t) -> bool {
    matches!(
        probe_handoff_candidate(pid),
        HandoffCandidateProbe::Running | HandoffCandidateProbe::Exited
    )
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HandoffCandidateProbe {
    NotOurChild,
    Running,
    Exited,
}

#[cfg(unix)]
fn probe_handoff_candidate(pid: libc::pid_t) -> HandoffCandidateProbe {
    let Ok(id) = libc::id_t::try_from(pid) else {
        return HandoffCandidateProbe::NotOurChild;
    };
    // SAFETY: `info` is a zeroed `siginfo_t` out-parameter and `WNOWAIT` leaves the child waitable.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::waitid(
            libc::P_PID,
            id,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if rc != 0 {
        return HandoffCandidateProbe::NotOurChild;
    }
    match info.si_code {
        libc::CLD_EXITED | libc::CLD_KILLED | libc::CLD_DUMPED => HandoffCandidateProbe::Exited,
        _ => HandoffCandidateProbe::Running,
    }
}

#[cfg(target_os = "macos")]
struct CandidateExitWatch(aterm_uds::exitwatch::ExitWatch);

#[cfg(target_os = "macos")]
impl CandidateExitWatch {
    fn watch(pid: u32) -> Option<Self> {
        aterm_uds::exitwatch::ExitWatch::watch(pid).map(Self)
    }

    fn exit_status(&self) -> Option<std::process::ExitStatus> {
        use std::os::unix::process::ExitStatusExt as _;
        self.0.exit_status().map(std::process::ExitStatus::from_raw)
    }
}

#[cfg(unix)]
fn candidate_leads_its_own_group(pid: libc::pid_t) -> bool {
    // SAFETY: `getpgid` is a side-effect-free libc getter.
    pid > 1 && unsafe { libc::getpgid(pid) } == pid
}

#[cfg(unix)]
fn kill_corroborated_candidate(pid: libc::pid_t) {
    if pid <= 1 {
        return;
    }
    if candidate_leads_its_own_group(pid) || candidate_is_our_child(pid) {
        // SAFETY: `SIGKILL` to the verified candidate process group.
        unsafe { libc::kill(-pid, libc::SIGKILL) };
    } else {
        aterm_log::warn!(
            "handoff candidate {pid} leads no process group of its own (it never \
             reached its containment); sending the direct signal only"
        );
    }
    // SAFETY: SIGKILL to the candidate itself, a pid just proven to name it.
    unsafe { libc::kill(pid, libc::SIGKILL) };
}

#[cfg(unix)]
fn signal_handoff_candidate(candidate: HandoffCandidate) {
    let Ok(pid) = libc::pid_t::try_from(candidate.pid) else {
        return;
    };
    if pid <= 1 {
        return;
    }
    match candidate.identity() {
        HandoffCandidateIdentity::Recycled => (),
        HandoffCandidateIdentity::Corroborated => kill_corroborated_candidate(pid),
        HandoffCandidateIdentity::Unwitnessed => {
            // A group kill is sound only while the pid is pinned: a launched candidate can
            // be reaped by launchd, and `-pid` would then name a stranger's group. So ask
            // the kernel now (`WNOWAIT`; ECHILD means not pinned).
            if candidate_is_our_child(pid) {
                // SAFETY: `SIGKILL` to the verified candidate process group and candidate PID.
                unsafe {
                    libc::kill(-pid, libc::SIGKILL);
                    libc::kill(pid, libc::SIGKILL);
                }
            } else {
                aterm_log::warn!(
                    "handoff candidate {pid} is not our child, so its pid is not pinned and \
                     -{pid} may name a stranger; skipping the process-group sweep"
                );
            }
        }
    }
}

#[cfg(unix)]
fn wait_for_handoff_candidate_to_terminate(candidate: HandoffCandidate) -> HandoffRollbackWarrant {
    const PROBE: std::time::Duration = std::time::Duration::from_millis(2);
    const COMPLAIN_EVERY: std::time::Duration = std::time::Duration::from_secs(10);
    let mut complain_at = std::time::Instant::now() + COMPLAIN_EVERY;
    loop {
        if handoff_candidate_terminated(candidate) {
            return HandoffRollbackWarrant::Vanished;
        }
        let now = std::time::Instant::now();
        if now >= complain_at {
            aterm_log::warn!(
                "update apply: handoff candidate {} has not terminated; the parked PTY readers \
                 stay parked until it does",
                candidate.pid
            );
            complain_at = now + COMPLAIN_EVERY;
        }
        std::thread::sleep(PROBE);
    }
}

#[cfg(unix)]
struct ReapedHandoffCandidate {
    warrant: HandoffRollbackWarrant,
    status: Option<std::process::ExitStatus>,
}

#[cfg(unix)]
fn kill_and_reap_handoff_child(
    candidate: HandoffCandidate,
    handle: &mut HandoffCandidateHandle,
) -> ReapedHandoffCandidate {
    #[cfg_attr(
        not(target_os = "macos"),
        expect(
            clippy::infallible_destructuring_match,
            reason = "`Launched` is the macOS-only arm"
        )
    )]
    let child = match handle {
        HandoffCandidateHandle::Forked(child) => child,
        #[cfg(target_os = "macos")]
        HandoffCandidateHandle::Launched(_) => {
            signal_handoff_candidate(candidate);
            return ReapedHandoffCandidate {
                warrant: wait_for_handoff_candidate_to_terminate(candidate),
                status: None,
            };
        }
    };
    signal_handoff_candidate(candidate);
    let child_is_candidate = child.id() == candidate.pid;
    let reaped = child.wait().ok();
    if let Some(status) = reaped
        && child_is_candidate
    {
        return ReapedHandoffCandidate {
            warrant: HandoffRollbackWarrant::Reaped,
            status: Some(status),
        };
    }
    ReapedHandoffCandidate {
        warrant: wait_for_handoff_candidate_to_terminate(candidate),
        status: None,
    }
}

#[cfg(unix)]
enum HandoffCandidateHandle {
    Forked(std::process::Child),
    #[cfg(target_os = "macos")]
    Launched(Option<CandidateExitWatch>),
}

#[cfg(unix)]
impl HandoffCandidateHandle {
    fn witnessed_exit_status(&self) -> Option<std::process::ExitStatus> {
        match self {
            Self::Forked(_) => None,
            #[cfg(target_os = "macos")]
            Self::Launched(watch) => watch.as_ref().and_then(CandidateExitWatch::exit_status),
        }
    }

    fn candidate_gone(&self, candidate: HandoffCandidate) -> bool {
        match self {
            Self::Forked(child) if child.id() == candidate.pid => {
                libc::pid_t::try_from(candidate.pid)
                    .is_ok_and(|pid| probe_handoff_candidate(pid) == HandoffCandidateProbe::Exited)
            }
            Self::Forked(_) => handoff_candidate_terminated(candidate),
            #[cfg(target_os = "macos")]
            Self::Launched(_) => handoff_candidate_terminated(candidate),
        }
    }
}

#[cfg(unix)]
fn worker_claim_handoff_reaper(arbiter: &crate::HandoffAttemptArbiter) -> bool {
    loop {
        match arbiter.phase() {
            crate::HandoffAttemptPhase::Waiting => {
                if !arbiter.try_begin_reject() {
                    continue;
                }
            }
            crate::HandoffAttemptPhase::Rejecting => {}
            crate::HandoffAttemptPhase::Committing => return false,
        }
        return arbiter.claim_reaper(crate::HandoffReaperOwner::Worker);
    }
}

#[cfg(unix)]
fn emergency_kill_and_reap_handoff_child(pid: u32) -> HandoffRollbackWarrant {
    let candidate = HandoffCandidate::from_bare_pid(pid);
    signal_handoff_candidate(candidate);
    let Ok(raw) = libc::pid_t::try_from(pid) else {
        return wait_for_handoff_candidate_to_terminate(candidate);
    };
    let mut status = 0i32;
    let reaped = loop {
        // SAFETY: blocking wait for one exact pid into a local status slot.
        let waited = unsafe { libc::waitpid(raw, &mut status, 0) };
        if waited == raw {
            break true;
        }
        if waited < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        break false;
    };
    if reaped {
        return HandoffRollbackWarrant::Reaped;
    }
    wait_for_handoff_candidate_to_terminate(candidate)
}

/// Pre-park admission peek.
#[cfg(unix)]
pub(crate) fn handoff_masters_have_activity(live: &[(u64, i32, i32)]) -> bool {
    peek_handoff_masters(live, |revents| {
        revents & libc::POLLIN != 0 && revents & HANDOFF_MASTER_DEAD == 0
    })
}

/// Mid-flight death peek.
#[cfg(unix)]
pub(crate) fn handoff_masters_closed(live: &[(u64, i32, i32)]) -> bool {
    peek_handoff_masters(live, |revents| revents & HANDOFF_MASTER_DEAD != 0)
}

#[cfg(unix)]
const HANDOFF_MASTER_DEAD: libc::c_short = libc::POLLHUP | libc::POLLERR | libc::POLLNVAL;

#[cfg(unix)]
fn peek_handoff_masters(live: &[(u64, i32, i32)], hit: impl Fn(libc::c_short) -> bool) -> bool {
    let mut fds = live
        .iter()
        .map(|(_, fd, _)| libc::pollfd {
            fd: *fd,
            events: libc::POLLIN,
            revents: 0,
        })
        .collect::<Vec<_>>();
    if fds.is_empty() {
        return false;
    }
    // SAFETY: initialized stable pollfd slice; timeout 0 is a non-consuming peek.
    let polled = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, 0) };
    polled > 0 && fds.iter().any(|fd| hit(fd.revents))
}

#[cfg(unix)]
fn handoff_egress_settled(
    pool: &crate::SessionPool,
    live: &[(u64, i32, i32)],
    replies: &crate::control::InputReplies,
) -> bool {
    !replies.owed()
        && live.iter().all(|(_, master, _)| {
            if let Some(session) = pool.iter().find(|session| session.master == *master) {
                fence_session_egress(&session.ctx.sink).is_some()
            } else {
                !paste_order::is_master_ordering_for_handoff(*master)
            }
        })
}

#[cfg(unix)]
fn fence_session_egress(
    sink: &aterm_session::sink::SinkWriter,
) -> Option<aterm_session::sink::EgressFence<'_>> {
    if paste_order::is_ordering(sink) {
        return None;
    }
    sink.try_fence_egress()
}

#[cfg(unix)]
fn handoff_sinks(
    pool: &crate::SessionPool,
    live: &[(u64, i32, i32)],
) -> Vec<std::sync::Arc<aterm_session::sink::SinkWriter>> {
    pool.iter()
        .filter(|session| live.iter().any(|(_, master, _)| *master == session.master))
        .map(|session| session.ctx.sink.clone())
        .collect()
}

#[cfg(unix)]
fn fence_handoff_egress<'a>(
    sinks: &'a [std::sync::Arc<aterm_session::sink::SinkWriter>],
    replies: &'a crate::control::InputReplies,
) -> Option<HandoffEgressFence<'a>> {
    let frames = sinks
        .iter()
        .map(|sink| fence_session_egress(sink))
        .collect::<Option<Vec<_>>>()?;
    let admission = replies.try_fence()?;
    Some(HandoffEgressFence {
        _frames: frames,
        _admission: admission,
    })
}

#[cfg(unix)]
struct HandoffEgressFence<'a> {
    _frames: Vec<aterm_session::sink::EgressFence<'a>>,
    _admission: crate::control::InputRepliesFence<'a>,
}

const EXPECTED_BUILD_ENV: &str = "ATERM_UPDATE_EXPECTED_BUILD";
const EXPECTED_COMMIT_ENV: &str = "ATERM_UPDATE_EXPECTED_COMMIT";
const EXPECTED_DIGEST_ENV: &str = "ATERM_UPDATE_EXPECTED_DMG_SHA256";

fn bind_expected_update_artifact(
    command: &mut aterm_uds::spawnfd::StdCommand,
    attempt: Option<&crate::native_updater_service::ApplyAttemptTicket>,
) {
    command
        .env_remove(EXPECTED_BUILD_ENV)
        .env_remove(EXPECTED_COMMIT_ENV)
        .env_remove(EXPECTED_DIGEST_ENV);
    if let Some(attempt) = attempt {
        command
            .env(EXPECTED_BUILD_ENV, attempt.target_build().to_string())
            .env(EXPECTED_COMMIT_ENV, attempt.target_commit())
            .env(EXPECTED_DIGEST_ENV, attempt.target_dmg_sha256());
    }
}

#[cfg(unix)]
const COLD_LANE_BUDGET_ENV: &str = "ATERM_UPDATE_COLD_LANE_BUDGET";

/// The automatic lane's memory of one artifact, carried across the cold lane's `exec`.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ColdLaneBudget {
    pub(crate) failures: Option<CarriedFailures>,
    pub(crate) verdict: Option<CarriedVerdict>,
}

/// `auto_apply_physical_retry`, as [`ColdLaneBudget`] carries it.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CarriedFailures {
    pub(crate) cycles: u8,
    pub(crate) since_secs: u64,
    pub(crate) activation: bool,
}

/// `auto_apply_structural_verdict`, as [`ColdLaneBudget`] carries it.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CarriedVerdict {
    pub(crate) resample_in_secs: Option<u64>,
    pub(crate) activation: bool,
}

#[cfg(unix)]
impl ColdLaneBudget {
    /// `field=value` pairs joined by `;`, keyed by the artifact they are about.
    pub(crate) fn encode(&self, build: u64, artifact: &str) -> String {
        let mut fields = vec![format!("build={build}"), format!("artifact={artifact}")];
        if let Some(failures) = self.failures {
            fields.push(format!("cycles={}", failures.cycles));
            fields.push(format!("since={}", failures.since_secs));
            fields.push(format!("activation={}", u8::from(failures.activation)));
        }
        if let Some(verdict) = self.verdict {
            fields.push(format!(
                "verdict={}",
                verdict
                    .resample_in_secs
                    .map_or_else(|| "spent".to_string(), |secs| secs.to_string())
            ));
            fields.push(format!(
                "verdict_activation={}",
                u8::from(verdict.activation)
            ));
        }
        fields.join(";")
    }

    /// [`Self::encode`]'s inverse.
    pub(crate) fn decode(value: &str, build: u64, artifact: &str) -> Option<Self> {
        let field = |name: &str| {
            value
                .split(';')
                .find_map(|pair| pair.strip_prefix(name)?.strip_prefix('='))
        };
        let flag = |name: &str| match field(name) {
            Some("1") => Some(true),
            Some("0") => Some(false),
            _ => None,
        };
        if field("build")?.parse::<u64>().ok()? != build || field("artifact")? != artifact {
            return None;
        }
        let failures = (|| {
            Some(CarriedFailures {
                cycles: field("cycles")?.parse().ok()?,
                since_secs: field("since")?.parse().ok()?,
                activation: flag("activation")?,
            })
        })();
        let verdict = (|| {
            Some(CarriedVerdict {
                resample_in_secs: match field("verdict")? {
                    "spent" => None,
                    secs => Some(secs.parse().ok()?),
                },
                activation: flag("verdict_activation")?,
            })
        })();
        Some(Self { failures, verdict })
    }
}

#[cfg(unix)]
fn bind_cold_lane_budget(
    command: &mut aterm_uds::spawnfd::StdCommand,
    attempt: Option<&crate::native_updater_service::ApplyAttemptTicket>,
    budget: Option<ColdLaneBudget>,
) {
    command.env_remove(COLD_LANE_BUDGET_ENV);
    if let (Some(attempt), Some(budget)) = (attempt, budget) {
        command.env(
            COLD_LANE_BUDGET_ENV,
            budget.encode(attempt.target_build(), attempt.target_dmg_sha256()),
        );
    }
}

/// The name the cold-lane parent's unanswered update-health warnings travel under across its `exec`.
pub(crate) const COLD_LANE_HEALTH_SAID_ENV: &str = "ATERM_UPDATE_HEALTH_SAID";

/// Carries the parent's unanswered update-health warnings across the cold lane.
#[cfg(unix)]
pub(crate) fn bind_cold_lane_health_said(
    command: &mut aterm_uds::spawnfd::StdCommand,
    said: &[crate::session_store::CarriedHealthSaid],
) {
    command.env_remove(COLD_LANE_HEALTH_SAID_ENV);
    if said.is_empty() {
        return;
    }
    if let Ok(wire) = aterm_json::to_string(said) {
        command.env(COLD_LANE_HEALTH_SAID_ENV, wire);
    }
}

/// The cold-lane parent's unanswered update-health warnings.
pub(crate) fn take_cold_lane_health_said(
    just_updated: bool,
) -> Vec<crate::session_store::CarriedHealthSaid> {
    let wire = aterm_log::env::take(COLD_LANE_HEALTH_SAID_ENV);
    if !just_updated {
        return Vec::new();
    }
    parse_cold_lane_health_said(wire.as_deref())
}

/// [`take_cold_lane_health_said`]'s reading of one value.
pub(crate) fn parse_cold_lane_health_said(
    wire: Option<&std::ffi::OsStr>,
) -> Vec<crate::session_store::CarriedHealthSaid> {
    wire.and_then(std::ffi::OsStr::to_str)
        .and_then(|wire| aterm_json::from_str(wire).ok())
        .unwrap_or_default()
}

/// This launch is the cold lane's relaunch of its own build.
#[cfg(unix)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ColdRelaunch {
    build: u64,
    commit: String,
    artifact: String,
    budget: Option<ColdLaneBudget>,
}

/// What a refused cold relaunch hands its automatic lane.
#[cfg(unix)]
#[derive(Clone, Debug)]
pub(crate) struct ColdRelaunchSeed {
    pub(crate) verdict: crate::HandoffPreverification,
    pub(crate) roster_wait: Option<String>,
    pub(crate) dmg_sha256: Option<[u8; 32]>,
    pub(crate) budget: ColdLaneBudget,
}

#[cfg(unix)]
impl ColdRelaunch {
    /// Read from this process's environment before the boot apply consumes the binding.
    pub(crate) fn of_this_launch(handoff_successor: bool) -> Option<Self> {
        let budget = aterm_log::env::take(COLD_LANE_BUDGET_ENV);
        if handoff_successor {
            return None;
        }
        Self::from_values(
            std::env::var_os("ATERM_UPDATED_FROM").as_deref(),
            std::env::var_os(EXPECTED_BUILD_ENV).as_deref(),
            std::env::var_os(EXPECTED_COMMIT_ENV).as_deref(),
            std::env::var_os(EXPECTED_DIGEST_ENV).as_deref(),
            budget.as_deref(),
            crate::build_info::build_number(),
        )
    }

    /// [`Self::of_this_launch`] over explicit values.
    pub(crate) fn from_values(
        updated_from: Option<&std::ffi::OsStr>,
        build: Option<&std::ffi::OsStr>,
        commit: Option<&std::ffi::OsStr>,
        digest: Option<&std::ffi::OsStr>,
        budget: Option<&std::ffi::OsStr>,
        running: &str,
    ) -> Option<Self> {
        if updated_from? != std::ffi::OsStr::new(running) {
            return None;
        }
        let build = build?.to_str()?.parse::<u64>().ok().filter(|b| *b != 0)?;
        let commit = commit?.to_str().filter(|c| !c.is_empty())?.to_string();
        let artifact = digest?.to_str().filter(|d| !d.is_empty())?.to_string();
        let budget = budget
            .and_then(std::ffi::OsStr::to_str)
            .and_then(|value| ColdLaneBudget::decode(value, build, &artifact));
        Some(Self {
            build,
            commit,
            artifact,
            budget,
        })
    }

    /// What this relaunch's boot apply leaves its automatic lane, `Some` when the swap refused (`Deferred`) or was rolled back (`ReExecFailed`).
    pub(crate) fn seed(&self, outcome: &aterm_update::ApplyOutcome) -> Option<ColdRelaunchSeed> {
        let reason = match outcome {
            aterm_update::ApplyOutcome::Deferred(reason)
            | aterm_update::ApplyOutcome::ReExecFailed(reason)
            | aterm_update::ApplyOutcome::Reverted(reason) => reason,
            aterm_update::ApplyOutcome::NotApplicable
            | aterm_update::ApplyOutcome::NoUpdate
            | aterm_update::ApplyOutcome::LeftForSession { .. } => {
                return None;
            }
        };
        let budget = self.budget.unwrap_or_default();
        let roster_wait = matches!(outcome, aterm_update::ApplyOutcome::Deferred(_))
            .then_some(reason)
            .filter(|reason| aterm_update::is_roster_confirmation_wait(reason))
            .cloned();
        if roster_wait.is_some() {
            aterm_log::info!(
                "update apply: this launch is the cold relaunch of build {} for build {}, and \
                 its boot swap waits for an update check to confirm the stage's signer \
                 ({reason}); the automatic lane holds for that check, charged nothing, \
                 instead of relaunching into it",
                crate::build_info::build_number(),
                self.build,
            );
        } else {
            aterm_log::warn!(
                "update apply: this launch is the cold relaunch of build {} for build {}, and \
                 its boot swap refused ({reason}); the automatic lane books that refusal ({} \
                 earlier failure(s) of these bytes carried across the relaunch) instead of \
                 relaunching into it again",
                crate::build_info::build_number(),
                self.build,
                budget.failures.map_or(0, |failures| failures.cycles)
            );
        }
        Some(ColdRelaunchSeed {
            verdict: crate::HandoffPreverification {
                build: self.build,
                commit: self.commit.clone(),
                artifact: self.artifact.clone(),
                at: std::time::Instant::now(),
                passed: false,
                reason: Some(format!(
                    "the relaunched app's install of build {} refused: {reason}",
                    self.build
                )),
                policy: None,
                grant_chunks: false,
            },
            roster_wait,
            dmg_sha256: crate::app_native::decode_dmg_sha256(&self.artifact),
            budget,
        })
    }
}

#[cfg(unix)]
impl crate::UpdateHandoffCompletion {
    fn failure(
        attempt_id: u64,
        nonce: Option<String>,
        child_pid: Option<u32>,
        outcome: crate::UpdateHandoffOutcome,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            attempt_id,
            nonce,
            child_pid,
            outcome,
            commit_fd: None,
            reject: None,
            reconcile: None,
            detail: detail.into(),
            input_drain_spins: 0,
            child_death: crate::ChildDeathEvidence::Unobserved,
        }
    }

    #[must_use]
    const fn with_child_death(mut self, death: crate::ChildDeathEvidence) -> Self {
        self.child_death = death;
        self
    }
}

#[cfg(unix)]
fn send_warranted_handoff_failure(
    warrant: HandoffRollbackWarrant,
    cleanup: &HandoffWorkerCleanup,
    proxy: &aterm_window::event_loop::EventLoopProxy<Wake>,
    current_build: u64,
    completion: crate::UpdateHandoffCompletion,
) {
    if publish_warranted_handoff_failure(warrant, cleanup, proxy, completion) {
        send_post_failure_reconcile_facts(cleanup, proxy, current_build);
    }
}

#[cfg(unix)]
fn publish_warranted_handoff_failure(
    warrant: HandoffRollbackWarrant,
    cleanup: &HandoffWorkerCleanup,
    proxy: &aterm_window::event_loop::EventLoopProxy<Wake>,
    completion: crate::UpdateHandoffCompletion,
) -> bool {
    warrant.announce(completion.child_pid);
    cleanup.complete(completion.nonce.as_deref());
    proxy
        .send_event(Wake::UpdateHandoffFinished(completion))
        .is_ok()
}

#[cfg(unix)]
fn send_post_failure_reconcile_facts(
    cleanup: &HandoffWorkerCleanup,
    proxy: &aterm_window::event_loop::EventLoopProxy<Wake>,
    current_build: u64,
) {
    if let Some(facts) = cleanup.reconcile.as_ref().and_then(|(worker, ticket)| {
        crate::app_native::collect_native_update_reconcile_facts(worker, *ticket, current_build)
    }) {
        let _ = proxy.send_event(Wake::NativeUpdateReconcileFinished {
            purpose: crate::app_native::NativeUpdateReconcilePurpose::Startup,
            facts,
        });
    }
}

#[cfg(unix)]
fn worker_reject_and_reap_handoff_child(
    job: &HandoffWorkerJob,
    proxy: &aterm_window::event_loop::EventLoopProxy<Wake>,
    handle: &mut HandoffCandidateHandle,
    candidate: HandoffCandidate,
    nonce: &str,
    outcome: crate::UpdateHandoffOutcome,
    detail: String,
) -> bool {
    if !worker_claim_handoff_reaper(&job.arbiter) {
        return false;
    }
    let (warrant, child_death) = observe_candidate_death(outcome, candidate, handle);
    let completed = job.arbiter.finish_reap(crate::HandoffReaperOwner::Worker);
    debug_assert!(
        completed,
        "the worker must retain its unique reaper ownership"
    );
    if outcome == crate::UpdateHandoffOutcome::ChildDied {
        aterm_log::warn!(
            "update apply: candidate {} died before proving adoption; observed {child_death:?} \
             (this decides whether the automatic lane retries these bytes or converges on them)",
            candidate.pid
        );
    }
    let published = publish_warranted_handoff_failure(
        warrant,
        &job.cleanup,
        proxy,
        crate::UpdateHandoffCompletion::failure(
            job.attempt_id,
            Some(nonce.to_string()),
            Some(candidate.pid),
            outcome,
            detail,
        )
        .with_child_death(child_death),
    );
    if forgives_the_counted_trial_launch(child_death, job.current_build, job.target_build) {
        forgive_candidate_trial_launch(job, candidate);
    }
    if published {
        send_post_failure_reconcile_facts(&job.cleanup, proxy, job.current_build);
    }
    true
}

#[cfg(unix)]
fn forgive_candidate_trial_launch(job: &HandoffWorkerJob, candidate: HandoffCandidate) {
    let started = candidate
        .birth
        .map(|birth| (birth.seconds, birth.microseconds));
    let _ = aterm_update::forgive_trial_launch_of(job.target_build, candidate.pid, started);
}

#[cfg(unix)]
fn observe_candidate_death(
    outcome: crate::UpdateHandoffOutcome,
    candidate: HandoffCandidate,
    handle: &mut HandoffCandidateHandle,
) -> (HandoffRollbackWarrant, crate::ChildDeathEvidence) {
    if outcome != crate::UpdateHandoffOutcome::ChildDied {
        let reaped = kill_and_reap_handoff_child(candidate, handle);
        return (reaped.warrant, crate::ChildDeathEvidence::Unobserved);
    }
    let witnessed_before_the_kill = handle.witnessed_exit_status();
    let died_before_we_signalled = witnessed_before_the_kill.is_some()
        || libc::pid_t::try_from(candidate.pid)
            .is_ok_and(|pid| probe_handoff_candidate(pid) == HandoffCandidateProbe::Exited);
    let reaped = kill_and_reap_handoff_child(candidate, handle);
    let status = witnessed_before_the_kill
        .or(reaped.status)
        .or_else(|| handle.witnessed_exit_status());
    (
        reaped.warrant,
        handoff_child_death(died_before_we_signalled, status),
    )
}

#[cfg(unix)]
#[must_use]
fn forgives_the_counted_trial_launch(
    death: crate::ChildDeathEvidence,
    current_build: u64,
    target_build: u64,
) -> bool {
    target_build > current_build
        && crate::app_native::PhysicalFailureShape::of_child_death(death)
            != crate::app_native::PhysicalFailureShape::Structural
}

#[cfg(unix)]
#[must_use]
fn handoff_child_death(
    died_before_we_signalled: bool,
    status: Option<std::process::ExitStatus>,
) -> crate::ChildDeathEvidence {
    use std::os::unix::process::ExitStatusExt as _;
    let Some(status) = status else {
        return crate::ChildDeathEvidence::Unobserved;
    };
    if let Some(code) = status.code() {
        return crate::ChildDeathEvidence::Exited { code };
    }
    if let Some(signal) = status.signal()
        && (died_before_we_signalled || signal != libc::SIGKILL)
    {
        return crate::ChildDeathEvidence::Signalled { signal };
    }
    crate::ChildDeathEvidence::Unobserved
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HandoffRejectDelivery {
    WorkerOwned,
    Disconnected,
}

#[cfg(unix)]
fn deliver_handoff_rejection(
    reject: Option<std::sync::mpsc::SyncSender<()>>,
) -> HandoffRejectDelivery {
    let Some(reject) = reject else {
        return HandoffRejectDelivery::Disconnected;
    };
    match reject.try_send(()) {
        Ok(()) | Err(std::sync::mpsc::TrySendError::Full(())) => HandoffRejectDelivery::WorkerOwned,
        Err(std::sync::mpsc::TrySendError::Disconnected(())) => HandoffRejectDelivery::Disconnected,
    }
}

#[cfg(unix)]
fn handoff_preparation_cancelled(
    job: &HandoffWorkerJob,
    proxy: &aterm_window::event_loop::EventLoopProxy<Wake>,
    nonce: Option<String>,
) -> bool {
    if job.cancel.try_recv().is_err() {
        return false;
    }
    send_preparation_cancelled(job, proxy, nonce);
    true
}

#[cfg(unix)]
fn send_preparation_cancelled(
    job: &HandoffWorkerJob,
    proxy: &aterm_window::event_loop::EventLoopProxy<Wake>,
    nonce: Option<String>,
) {
    send_warranted_handoff_failure(
        HandoffRollbackWarrant::NoCandidate,
        &job.cleanup,
        proxy,
        job.current_build,
        crate::UpdateHandoffCompletion::failure(
            job.attempt_id,
            nonce,
            None,
            crate::UpdateHandoffOutcome::ActivityRevoked,
            "activity revoked handoff during physical preparation",
        ),
    );
}

#[cfg(unix)]
fn send_handoff_producer_failure(
    job: &HandoffWorkerJob,
    proxy: &aterm_window::event_loop::EventLoopProxy<Wake>,
    nonce: Option<String>,
    detail: impl Into<String>,
) {
    send_warranted_handoff_failure(
        HandoffRollbackWarrant::NoCandidate,
        &job.cleanup,
        proxy,
        job.current_build,
        crate::UpdateHandoffCompletion::failure(
            job.attempt_id,
            nonce,
            None,
            crate::UpdateHandoffOutcome::ProducerFailed,
            detail,
        ),
    );
}

#[cfg(unix)]
fn send_handoff_preparation_failure(
    job: &HandoffWorkerJob,
    proxy: &aterm_window::event_loop::EventLoopProxy<Wake>,
    nonce: Option<String>,
    outcome: crate::UpdateHandoffOutcome,
    detail: impl Into<String>,
) {
    send_warranted_handoff_failure(
        HandoffRollbackWarrant::NoCandidate,
        &job.cleanup,
        proxy,
        job.current_build,
        crate::UpdateHandoffCompletion::failure(job.attempt_id, nonce, None, outcome, detail),
    );
}

/// The outcome a refused pre-park verification is filed under, read off the verifier's own words.
#[cfg(unix)]
#[must_use]
pub(crate) fn pre_park_refusal_outcome(error: &str) -> crate::UpdateHandoffOutcome {
    if aterm_update::is_roster_confirmation_wait(error) {
        crate::UpdateHandoffOutcome::StageUnconfirmed
    } else if aterm_update::is_passing_refusal(error) {
        crate::UpdateHandoffOutcome::ProducerFailed
    } else {
        crate::UpdateHandoffOutcome::PreparationFailed
    }
}

#[cfg(unix)]
fn skipped_verification_floor_refusal(
    job: &HandoffWorkerJob,
    floor: impl FnOnce(u64, Option<&str>) -> Option<String>,
) -> Option<String> {
    if job.verify_staged_candidate || job.preverified.is_none() {
        return None;
    }
    let staged_commit = (!job.installed_activation).then_some(job.target_commit.as_str());
    floor(job.target_build, staged_commit)
}

#[cfg(unix)]
#[derive(Debug, PartialEq, Eq)]
enum PreParkRefusal {
    Verification {
        outcome: crate::UpdateHandoffOutcome,
        detail: String,
    },
    Policy(String),
}

#[cfg(unix)]
fn verify_candidate_for_handoff(
    job: &HandoffWorkerJob,
) -> Result<aterm_update::HandoffCandidateFacts, String> {
    if job.installed_activation {
        aterm_update::preverify_installed_for_handoff(
            job.current_build,
            job.target_build,
            &job.target_commit,
        )
    } else {
        aterm_update::preverify_staged_for_handoff(
            job.current_build,
            Some(crate::build_info::git_commit()),
            Some(job.target_build),
            Some(&job.target_commit),
        )
    }
}

#[cfg(unix)]
fn worker_pre_park_check(
    job: &mut HandoffWorkerJob,
    verify: impl FnOnce(&HandoffWorkerJob) -> Result<aterm_update::HandoffCandidateFacts, String>,
    floor: impl FnOnce(u64, Option<&str>) -> Option<String>,
) -> Result<(), PreParkRefusal> {
    let which = if job.installed_activation {
        "installed bundle"
    } else {
        "staged update"
    };
    let refused = |error: String| PreParkRefusal::Verification {
        outcome: pre_park_refusal_outcome(&error),
        detail: format!("{which} failed pre-park verification: {error}"),
    };
    if job.verify_staged_candidate {
        let facts = match verify(job) {
            Ok(facts) => facts,
            Err(error) => {
                // The verdict is the arm-time read's word too, and is kept
                // where that read keeps it ([`PreverifyPublisher::publish_refusal`]).
                if let Some(publisher) = job.preverified.as_ref() {
                    publisher
                        .publish_refusal(&format!("{which} failed pre-park verification: {error}"));
                }
                return Err(refused(error));
            }
        };
        job.successor_grant_chunks = facts.grant_chunks;
        let adopted = job.preverified.as_ref().and_then(|publisher| {
            publisher.publish_pass(
                &facts,
                job.current_build,
                &format!("{which} build {}", job.target_build),
            )
        });
        if let Some(refusal) = capture_exceeds_policy(job.parked_under, adopted) {
            return Err(PreParkRefusal::Policy(refusal));
        }
    } else if let Some(error) = skipped_verification_floor_refusal(job, floor) {
        return Err(refused(error));
    }
    Ok(())
}

#[cfg(unix)]
fn cold_exec_hand_over() -> Option<crate::lifeline::InheritedAcrossExec> {
    aterm_uds::spawnfd::InheritStrip::prepare().apply();
    crate::lifeline::inherit_across_exec()
}

/// # Safety
/// Only in a forked child, before its `exec`: it makes only async-signal-safe
/// calls (`setpgid`, `fcntl`, `InheritStrip::apply`) over plain integers.
#[cfg(unix)]
unsafe fn fork_lane_child_holds(
    strip: aterm_uds::spawnfd::InheritStrip,
    handed: &[std::os::fd::RawFd],
) -> std::io::Result<()> {
    // SAFETY: the function's contract — a forked child before its `exec`.
    unsafe {
        if libc::setpgid(0, 0) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        strip.apply();
        for fd in handed {
            let flags = libc::fcntl(*fd, libc::F_GETFD);
            if flags < 0 || libc::fcntl(*fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
fn run_handoff_worker(
    mut job: HandoffWorkerJob,
    proxy: aterm_window::event_loop::EventLoopProxy<Wake>,
) {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::process::CommandExt as _;

    if handoff_preparation_cancelled(&job, &proxy, None) {
        return;
    }

    if let Err(refusal) = worker_pre_park_check(
        &mut job,
        verify_candidate_for_handoff,
        aterm_update::handoff_cached_pass_refusal,
    ) {
        match refusal {
            PreParkRefusal::Verification { outcome, detail } => {
                send_handoff_preparation_failure(&job, &proxy, None, outcome, detail);
            }
            PreParkRefusal::Policy(detail) => {
                send_handoff_producer_failure(&job, &proxy, None, detail);
            }
        }
        return;
    }
    if handoff_preparation_cancelled(&job, &proxy, None) {
        return;
    }

    let nonce;
    #[cfg(target_os = "macos")]
    {
        if job.lane == HandoffLane::OutOfBand {
            let lane = job
                .prelaunch
                .take()
                .expect("the out-of-band lane is constructed with its prelaunch channels");
            match run_prelaunched_handoff(&mut job, &proxy, lane) {
                Prelaunched::Finished => return,
                Prelaunched::ForkInstead {
                    nonce: returned_nonce,
                    transfer,
                } => {
                    let HandoffTransferJob {
                        capture,
                        proof_deadline: _,
                    } = *transfer;
                    job.capture = Some(capture);
                    nonce = returned_nonce;
                }
            }
        } else {
            nonce = crate::seamless::mint_outgoing_nonce();
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        nonce = crate::seamless::mint_outgoing_nonce();
    }

    let PreparedArtifacts {
        manifest_path: path,
        layout_path,
        fds_wire: wire,
        expected,
        proof_rd,
        proof_wr,
        commit_rd,
        commit_wr,
    } = match prepare_outgoing_artifacts(&mut job, &nonce, EarlyArtifacts::default()) {
        Ok(prepared) => prepared,
        Err(failure) => {
            report_preparation_failure(&job, &proxy, nonce, failure);
            return;
        }
    };

    #[cfg(target_os = "macos")]
    {
        let proof_term = match job.lane {
            HandoffLane::OutOfBand => "device",
            HandoffLane::Fork => "fd",
        };
        job.command
            .envs([crate::handoff_rendezvous::ENV_PROOF_TERM.pair(proof_term)]);
    }
    crate::descriptor_limit::carry_launcher_limit(&mut job.command, job.target_build);
    job.command.envs([
        crate::seamless::ENV_MANIFEST.pair(path),
        crate::seamless::ENV_NONCE.pair(&nonce),
        crate::seamless::ENV_FDS.pair(wire),
        crate::seamless::ENV_LAYOUT.pair(layout_path),
        crate::seamless::ENV_TARGET.pair(crate::seamless::encode_target_identity(
            job.target_build,
            &job.target_commit,
        )),
        crate::seamless::ENV_READY_FD.pair(proof_wr.as_raw_fd().to_string()),
        crate::seamless::ENV_COMMIT_FD.pair(commit_rd.as_raw_fd().to_string()),
    ]);
    for (key, value) in crate::seamless::outgoing_parent_env() {
        job.command.env(key, value);
    }
    let mut child_inherit = job
        .capture()
        .live
        .iter()
        .map(|(_, master, _)| *master)
        .collect::<Vec<_>>();
    child_inherit.push(proof_wr.as_raw_fd());
    child_inherit.push(commit_rd.as_raw_fd());
    let handed = child_inherit.clone();
    let strip = aterm_uds::spawnfd::InheritStrip::prepare();
    // SAFETY: pre-exec closure calls only async-signal-safe functions (`setpgid`, `fcntl`, `InheritStrip::apply`) on captured integers.
    unsafe {
        job.command
            .pre_exec(move || fork_lane_child_holds(strip, &child_inherit));
    }
    crate::descriptor_limit::start_older_image_at_its_limit(
        &mut job.command,
        job.target_build,
        job.current_build,
        &handed,
    );
    if handoff_preparation_cancelled(&job, &proxy, Some(nonce.clone())) {
        return;
    }
    let child = match job.command.spawn() {
        Ok(child) => child,
        Err(error) => {
            send_handoff_producer_failure(
                &job,
                &proxy,
                Some(nonce),
                format!("handoff process could not start: {error}"),
            );
            return;
        }
    };
    let candidate = HandoffCandidate::of_unreaped_child(&child);
    crate::keeper_link::pending_before_grant(candidate.pid);
    let _transfer = crate::identity_claim::register_handoff_candidate(candidate.pid);
    let mut handle = HandoffCandidateHandle::Forked(child);
    drop(proof_wr);
    drop(commit_rd);
    run_handoff_decision(
        &job,
        &proxy,
        &nonce,
        expected,
        &proof_rd,
        commit_wr,
        candidate,
        &mut handle,
        handoff_ready_deadline(),
        #[cfg(target_os = "macos")]
        None,
    );
}

#[cfg(unix)]
struct PreparedArtifacts {
    manifest_path: String,
    layout_path: std::path::PathBuf,
    fds_wire: String,
    expected: crate::seamless::AdoptionProof,
    proof_rd: std::os::fd::OwnedFd,
    proof_wr: std::os::fd::OwnedFd,
    commit_rd: std::os::fd::OwnedFd,
    commit_wr: std::os::fd::OwnedFd,
}

#[cfg(unix)]
struct AttemptPipes {
    proof_rd: std::os::fd::OwnedFd,
    proof_wr: std::os::fd::OwnedFd,
    commit_rd: std::os::fd::OwnedFd,
    commit_wr: std::os::fd::OwnedFd,
}

#[cfg(unix)]
impl AttemptPipes {
    fn make() -> Result<Self, String> {
        let Some((proof_rd, proof_wr)) = make_cloexec_pipe() else {
            let kind = std::io::Error::last_os_error().kind();
            return Err(format!(
                "could not create the adoption-proof channel ({kind})"
            ));
        };
        let Some((commit_rd, commit_wr)) = make_cloexec_pipe() else {
            let kind = std::io::Error::last_os_error().kind();
            return Err(format!(
                "could not create the handoff-commit channel ({kind})"
            ));
        };
        Ok(Self {
            proof_rd,
            proof_wr,
            commit_rd,
            commit_wr,
        })
    }
}

#[cfg(unix)]
#[derive(Default)]
struct EarlyArtifacts {
    pipes: Option<AttemptPipes>,
    layout: Option<crate::handoff_diet::SpeculativeLayout>,
}

#[cfg(unix)]
enum PreparationFailure {
    Cancelled,
    Producer(String),
    Refused(String),
}

#[cfg(unix)]
fn preparation_failure_of_write(
    failure: crate::seamless::WriteOutgoingFailure,
) -> PreparationFailure {
    let detail = format!("could not write the authenticated handoff manifest ({failure})");
    match failure {
        crate::seamless::WriteOutgoingFailure::ManifestOverCap => {
            PreparationFailure::Refused(detail)
        }
        crate::seamless::WriteOutgoingFailure::NoPrivateDir
        | crate::seamless::WriteOutgoingFailure::Inconsistent
        | crate::seamless::WriteOutgoingFailure::Io(_) => PreparationFailure::Producer(detail),
    }
}

#[cfg(unix)]
fn prepare_outgoing_artifacts(
    job: &mut HandoffWorkerJob,
    nonce: &str,
    early: EarlyArtifacts,
) -> Result<PreparedArtifacts, PreparationFailure> {
    let started = std::time::Instant::now();
    let target_build = job.target_build;
    let target_commit = job.target_commit.clone();
    let capture = job
        .capture
        .as_mut()
        .expect("the artifacts are prepared from the park's capture");
    let Some(layout_wire) = capture.layout.to_toml().ok().filter(|wire| {
        !capture.layout.is_empty()
            && crate::restore::RestoreManifest::from_toml(wire).as_ref() == Some(&capture.layout)
    }) else {
        return Err(PreparationFailure::Producer(
            "could not persist the bounded handoff layout".to_string(),
        ));
    };
    if crate::seamless::layout_wire_digest(&layout_wire) != Some(capture.layout_digest) {
        return Err(PreparationFailure::Producer(
            "handoff layout carry did not match the committed layout digest".to_string(),
        ));
    }
    if job.cancel.try_recv().is_ok() {
        return Err(PreparationFailure::Cancelled);
    }
    let turn_id_ceiling = crate::control::turn_ids_for_handoff();
    let controls =
        crate::handoff_carry::export_for_target(&capture.carries, turn_id_ceiling, target_build);
    capture.manifest.next_turn_id = crate::handoff_carry::manifest_turn_id(turn_id_ceiling);
    job.arbiter.set_turn_id_ceiling(turn_id_ceiling);
    let mut held_history = crate::handoff_history::HeldExports::default();
    if let Some(dir) = crate::control_auth::socket_dir() {
        let mut results = std::mem::replace(
            &mut capture.history,
            crate::handoff_history::HistoryPlan::Unexported,
        )
        .results(&dir);
        held_history = results.take_held(
            &capture
                .held
                .iter()
                .map(|pane| pane.local_id)
                .collect::<Vec<_>>(),
            !crate::handoff_history::exports_held_for_target(
                crate::running_build_number(),
                target_build,
            ),
        );
        let verdicts = crate::handoff_history::stamp_manifest(
            &mut capture.manifest,
            &capture.screens,
            &capture.history_heads,
            results,
            &dir,
            nonce,
        );
        crate::handoff_history::log_verdicts(&verdicts);
    }
    let carried_at = std::time::Instant::now();
    let outgoing = crate::seamless::write_outgoing_with_held(
        &capture.manifest,
        &capture.fds,
        &capture.screens,
        &capture.repaint,
        capture.window.clone(),
        &controls,
        &capture.held,
        held_history,
        nonce,
    )
    .map_err(preparation_failure_of_write)?;
    let crate::seamless::OutgoingHandoff {
        manifest_path: path,
        nonce: echoed_nonce,
        fds_wire,
        screen_digest: carried_screen_digest,
        connections_left_out,
    } = outgoing;
    job.arbiter.set_connections_left_out(connections_left_out);
    debug_assert_eq!(
        crate::seamless::outgoing_manifest_path(nonce).as_deref(),
        Some(std::path::Path::new(&path)),
        "the manifest writer and the launch environment name the manifest differently"
    );
    debug_assert_eq!(echoed_nonce, nonce, "the writer echoes the minted nonce");
    if carried_screen_digest != capture.screen_digest {
        return Err(PreparationFailure::Producer(
            "handoff screen carry did not match the committed screen digest".to_string(),
        ));
    }
    let manifest_at = std::time::Instant::now();
    if job.cancel.try_recv().is_ok() {
        return Err(PreparationFailure::Cancelled);
    }
    let EarlyArtifacts {
        pipes: early_pipes,
        layout: speculative_layout,
    } = early;
    let layout_path = std::path::Path::new(&path).with_extension("layout.toml");
    let layout_seal =
        match crate::handoff_diet::seal_layout(&layout_path, &layout_wire, speculative_layout) {
            Ok(seal) => seal,
            Err(error) => {
                return Err(PreparationFailure::Producer(format!(
                    "could not write the attempt-bound handoff layout ({error})"
                )));
            }
        };
    let layout_at = std::time::Instant::now();
    aterm_log::info!("update apply: layout sidecar — {layout_seal}");
    if job.cancel.try_recv().is_ok() {
        return Err(PreparationFailure::Cancelled);
    }
    let expected = crate::seamless::adoption_proof(
        nonce,
        target_build,
        &target_commit,
        &capture.layout_digest,
        &capture.screen_digest,
        &capture.proof_identities,
    )
    .map_err(|refusal| PreparationFailure::Producer(refusal.to_string()))?;
    let pipes_made_early = early_pipes.is_some();
    let AttemptPipes {
        proof_rd,
        proof_wr,
        commit_rd,
        commit_wr,
    } = match early_pipes {
        Some(pipes) => pipes,
        None => AttemptPipes::make().map_err(PreparationFailure::Producer)?,
    };
    if job.cancel.try_recv().is_ok() {
        return Err(PreparationFailure::Cancelled);
    }
    let ms = |from: std::time::Instant, to: std::time::Instant| {
        to.saturating_duration_since(from).as_secs_f64() * 1000.0
    };
    aterm_log::info!(
        "update apply: artifacts split — carry export {:.1} + manifest {:.1} + layout {:.1} + \
         proof and pipes {:.1} ms (pipes made {})",
        ms(started, carried_at),
        ms(carried_at, manifest_at),
        ms(manifest_at, layout_at),
        ms(layout_at, std::time::Instant::now()),
        if pipes_made_early {
            "before the park"
        } else {
            "at the park"
        }
    );
    Ok(PreparedArtifacts {
        manifest_path: path,
        layout_path,
        fds_wire,
        expected,
        proof_rd,
        proof_wr,
        commit_rd,
        commit_wr,
    })
}

#[cfg(unix)]
fn report_preparation_failure(
    job: &HandoffWorkerJob,
    proxy: &aterm_window::event_loop::EventLoopProxy<Wake>,
    nonce: String,
    failure: PreparationFailure,
) {
    match failure {
        PreparationFailure::Cancelled => send_preparation_cancelled(job, proxy, Some(nonce)),
        PreparationFailure::Producer(detail) => {
            send_handoff_producer_failure(job, proxy, Some(nonce), detail);
        }
        PreparationFailure::Refused(detail) => send_warranted_handoff_failure(
            HandoffRollbackWarrant::NoCandidate,
            &job.cleanup,
            proxy,
            job.current_build,
            crate::UpdateHandoffCompletion::failure(
                job.attempt_id,
                Some(nonce),
                None,
                crate::UpdateHandoffOutcome::CaptureRefused,
                detail,
            ),
        ),
    }
}

#[cfg(target_os = "macos")]
enum Prelaunched {
    Finished,
    ForkInstead {
        nonce: String,
        transfer: Box<HandoffTransferJob>,
    },
}

#[cfg(target_os = "macos")]
struct HeldSuccessor {
    rendezvous: crate::handoff_rendezvous::Rendezvous,
    peer: crate::handoff_rendezvous::ClaimedPeer,
    candidate: HandoffCandidate,
    exit_watch: Option<CandidateExitWatch>,
    dialled_at: std::time::Instant,
}

#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StandDownStep {
    Revoke,
    SuccessorExited,
    Kill,
    Reap,
}

#[cfg(all(test, target_os = "macos"))]
impl StandDownStep {
    #[must_use]
    fn model_action(self) -> &'static str {
        match self {
            Self::Revoke => "RevokeUngrantedSuccessor",
            Self::SuccessorExited => "SuccessorExitsOnRevoke",
            Self::Kill => "KillUngrantedSuccessor",
            Self::Reap => "ReapUngrantedSuccessor",
        }
    }
}

#[cfg(target_os = "macos")]
struct StoodDownSuccessor {
    warrant: HandoffRollbackWarrant,
    death: crate::ChildDeathEvidence,
    signalled: bool,
    steps: Vec<StandDownStep>,
}

#[cfg(target_os = "macos")]
impl StoodDownSuccessor {
    #[must_use]
    fn forgives_the_counted_trial_launch(&self, current_build: u64, target_build: u64) -> bool {
        forgives_the_counted_trial_launch(self.death, current_build, target_build)
    }
}

#[cfg(target_os = "macos")]
const STAND_DOWN_SELF_EXIT_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

#[cfg(target_os = "macos")]
const STAND_DOWN_ACK_BUDGET: std::time::Duration = std::time::Duration::from_millis(500);

#[cfg(target_os = "macos")]
const PRELAUNCH_HOLD_BOUND: std::time::Duration = std::time::Duration::from_secs(4 * 60);

#[cfg(target_os = "macos")]
fn stand_down_held_successor(
    held: HeldSuccessor,
    grace: std::time::Duration,
) -> StoodDownSuccessor {
    stand_down_held_successor_between(held, grace, &mut || {})
}

#[cfg(target_os = "macos")]
fn stand_down_held_successor_between(
    held: HeldSuccessor,
    grace: std::time::Duration,
    between_read_and_probe: &mut dyn FnMut(),
) -> StoodDownSuccessor {
    let HeldSuccessor {
        rendezvous,
        peer,
        candidate,
        exit_watch,
        ..
    } = held;
    drop(peer);
    drop(rendezvous);
    prove_stood_down(candidate, exit_watch, grace, between_read_and_probe)
}

#[cfg(target_os = "macos")]
fn prove_stood_down(
    candidate: HandoffCandidate,
    exit_watch: Option<CandidateExitWatch>,
    grace: std::time::Duration,
    between_read_and_probe: &mut dyn FnMut(),
) -> StoodDownSuccessor {
    const PROBE: std::time::Duration = std::time::Duration::from_millis(2);
    let mut steps = vec![StandDownStep::Revoke];
    let grace_deadline = std::time::Instant::now() + grace;
    let mut witnessed = None;
    let mut exited_on_its_own = false;
    loop {
        if let Some(status) = exit_watch
            .as_ref()
            .and_then(CandidateExitWatch::exit_status)
        {
            witnessed = Some(status);
            exited_on_its_own = true;
            break;
        }
        between_read_and_probe();
        if handoff_candidate_terminated(candidate) {
            exited_on_its_own = true;
            break;
        }
        if std::time::Instant::now() >= grace_deadline {
            break;
        }
        std::thread::sleep(PROBE);
    }
    if exited_on_its_own {
        steps.push(StandDownStep::SuccessorExited);
        let warrant = wait_for_handoff_candidate_to_terminate(candidate);
        steps.push(StandDownStep::Reap);
        let witnessed = witnessed.or_else(|| {
            exit_watch
                .as_ref()
                .and_then(CandidateExitWatch::exit_status)
        });
        return StoodDownSuccessor {
            warrant,
            death: handoff_child_death(true, witnessed),
            signalled: false,
            steps,
        };
    }
    signal_handoff_candidate(candidate);
    steps.push(StandDownStep::Kill);
    let warrant = wait_for_handoff_candidate_to_terminate(candidate);
    steps.push(StandDownStep::Reap);
    let status = exit_watch
        .as_ref()
        .and_then(CandidateExitWatch::exit_status);
    StoodDownSuccessor {
        warrant,
        death: handoff_child_death(false, status),
        signalled: true,
        steps,
    }
}

#[cfg(target_os = "macos")]
fn retire_ungranted_successor(
    job: &HandoffWorkerJob,
    proxy: &aterm_window::event_loop::EventLoopProxy<Wake>,
    lane: &HandoffPrelaunchLane,
    held: HeldSuccessor,
    outcome: crate::UpdateHandoffOutcome,
    detail: String,
) {
    let nonce = lane.nonce.as_str();
    if proxy
        .send_event(Wake::UpdateHandoffStandingDown {
            attempt_id: job.attempt_id,
        })
        .is_ok()
        && let Err(error) = lane.stand_down_ack.recv_timeout(STAND_DOWN_ACK_BUDGET)
    {
        aterm_log::warn!(
            "update apply: the outgoing process did not answer the stand-down within {} ms \
             ({error}); revoking anyway — a candidate left alive is worse than a late reader \
             resume",
            STAND_DOWN_ACK_BUDGET.as_millis()
        );
    }
    if !worker_claim_handoff_reaper(&job.arbiter) {
        debug_assert!(false, "Commit is unreachable before the grant");
        return;
    }
    let candidate = held.candidate;
    let held_for = held.dialled_at.elapsed();
    let stood = stand_down_held_successor(held, STAND_DOWN_SELF_EXIT_GRACE);
    let completed = job.arbiter.finish_reap(crate::HandoffReaperOwner::Worker);
    debug_assert!(
        completed,
        "the worker must retain its unique reaper ownership"
    );
    aterm_log::info!(
        "update apply: stood the held successor (pid {}) down after holding its claim {} ms — \
         {detail}; steps {:?} ({})",
        candidate.pid,
        held_for.as_millis(),
        stood.steps,
        if stood.signalled {
            "it ignored the closed rendezvous for the grace and was killed and proven terminated"
        } else {
            "it was gone with no signal of ours"
        }
    );
    let published = publish_warranted_handoff_failure(
        stood.warrant,
        &job.cleanup,
        proxy,
        crate::UpdateHandoffCompletion::failure(
            job.attempt_id,
            Some(nonce.to_string()),
            Some(candidate.pid),
            outcome,
            detail,
        )
        .with_child_death(stood.death),
    );
    if stood.forgives_the_counted_trial_launch(job.current_build, job.target_build) {
        forgive_candidate_trial_launch(job, candidate);
    }
    if published {
        send_post_failure_reconcile_facts(&job.cleanup, proxy, job.current_build);
    }
}

#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug)]
struct ParkTransferSplit {
    park_at: std::time::Instant,
    captured_at: std::time::Instant,
    picked_up_at: std::time::Instant,
    artifacts_at: std::time::Instant,
    sendmsg_at: std::time::Instant,
    granted_at: std::time::Instant,
}

#[cfg(target_os = "macos")]
fn park_transfer_split_text(split: ParkTransferSplit) -> String {
    let slice = |from: std::time::Instant, to: std::time::Instant| {
        to.saturating_duration_since(from).as_secs_f64() * 1000.0
    };
    format!(
        "capture {:.1} + hand-over {:.1} + artifacts {:.1} + pre-grant {:.1} + sendmsg {:.1} ms",
        slice(split.park_at, split.captured_at),
        slice(split.captured_at, split.picked_up_at),
        slice(split.picked_up_at, split.artifacts_at),
        slice(split.artifacts_at, split.sendmsg_at),
        slice(split.sendmsg_at, split.granted_at),
    )
}

#[cfg(target_os = "macos")]
enum HoldOutcome {
    Transfer(Box<HandoffTransferJob>),
    StandDown {
        outcome: crate::UpdateHandoffOutcome,
        detail: String,
    },
}

#[cfg(target_os = "macos")]
fn hold_for_park(
    job: &HandoffWorkerJob,
    lane: &HandoffPrelaunchLane,
    held: &HeldSuccessor,
    mut corroboration: Option<&mut LaunchCorroboration<'_>>,
) -> HoldOutcome {
    const SLICE: std::time::Duration = std::time::Duration::from_millis(2);
    let hold_deadline = held.dialled_at + lane.hold_bound;
    loop {
        match lane.transfer.recv_timeout(SLICE) {
            Ok(transfer) => return HoldOutcome::Transfer(Box::new(transfer)),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return HoldOutcome::StandDown {
                    outcome: crate::UpdateHandoffOutcome::Rejected,
                    detail: "the attempt record was dropped before the park".to_string(),
                };
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
        if let Ok(stand_down) = lane.stand_down.try_recv() {
            return HoldOutcome::StandDown {
                outcome: stand_down.outcome,
                detail: stand_down.detail,
            };
        }
        if job.cancel.try_recv().is_ok() {
            return HoldOutcome::StandDown {
                outcome: crate::UpdateHandoffOutcome::ActivityRevoked,
                detail: "structural activity revoked the handoff while the successor held the \
                         claim"
                    .to_string(),
            };
        }
        if held.peer.poll_hangup() {
            return HoldOutcome::StandDown {
                outcome: crate::UpdateHandoffOutcome::ChildDied,
                detail: "the successor closed the rendezvous while holding the claim".to_string(),
            };
        }
        if held
            .exit_watch
            .as_ref()
            .and_then(CandidateExitWatch::exit_status)
            .is_some()
        {
            return HoldOutcome::StandDown {
                outcome: crate::UpdateHandoffOutcome::ChildDied,
                detail: "the successor exited while holding the claim".to_string(),
            };
        }
        if let Some(corroboration) = corroboration.as_deref_mut()
            && let Err(detail) = corroboration.poll()
        {
            return HoldOutcome::StandDown {
                outcome: crate::UpdateHandoffOutcome::Rejected,
                detail,
            };
        }
        if std::time::Instant::now() >= hold_deadline {
            return HoldOutcome::StandDown {
                outcome: crate::UpdateHandoffOutcome::TimedOut,
                detail: format!(
                    "the outgoing process did not park within {} s of the successor's dial",
                    lane.hold_bound.as_secs()
                ),
            };
        }
    }
}

#[cfg(target_os = "macos")]
fn fork_after_park(
    job: &HandoffWorkerJob,
    proxy: &aterm_window::event_loop::EventLoopProxy<Wake>,
    lane: HandoffPrelaunchLane,
    why: &str,
) -> Prelaunched {
    const SLICE: std::time::Duration = std::time::Duration::from_millis(2);
    if job.renews_launch_record {
        aterm_log::warn!("launch record: {why}; a launch-record renewal never forks");
        send_handoff_producer_failure(
            job,
            proxy,
            Some(lane.nonce),
            format!("{why}; a launch-record renewal never forks"),
        );
        return Prelaunched::Finished;
    }
    let HandoffPrelaunchLane {
        nonce,
        transfer,
        stand_down,
        stand_down_ack: _,
        hold_bound,
        warm_hint: _,
        speculative_layout: _,
        successor_runs_candidate: _,
    } = lane;
    aterm_log::warn!("update apply: {why}; forking instead — asking the outgoing process to park");
    if proxy
        .send_event(Wake::UpdateHandoffAwaitingPark {
            attempt_id: job.attempt_id,
            dialer_pid: None,
            grant_limit: None,
        })
        .is_err()
    {
        send_handoff_producer_failure(job, proxy, Some(nonce), "event loop closed before the park");
        return Prelaunched::Finished;
    }
    let hold_deadline = std::time::Instant::now() + hold_bound;
    loop {
        match transfer.recv_timeout(SLICE) {
            Ok(transfer) => {
                return Prelaunched::ForkInstead {
                    nonce,
                    transfer: Box::new(transfer),
                };
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                send_handoff_producer_failure(
                    job,
                    proxy,
                    Some(nonce),
                    "the attempt record was dropped before the park",
                );
                return Prelaunched::Finished;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
        if let Ok(stand_down) = stand_down.try_recv() {
            send_warranted_handoff_failure(
                HandoffRollbackWarrant::NoCandidate,
                &job.cleanup,
                proxy,
                job.current_build,
                crate::UpdateHandoffCompletion::failure(
                    job.attempt_id,
                    Some(nonce),
                    None,
                    stand_down.outcome,
                    stand_down.detail,
                ),
            );
            return Prelaunched::Finished;
        }
        if job.cancel.try_recv().is_ok() {
            send_warranted_handoff_failure(
                HandoffRollbackWarrant::NoCandidate,
                &job.cleanup,
                proxy,
                job.current_build,
                crate::UpdateHandoffCompletion::failure(
                    job.attempt_id,
                    Some(nonce),
                    None,
                    crate::UpdateHandoffOutcome::ActivityRevoked,
                    "structural activity revoked the handoff before the park",
                ),
            );
            return Prelaunched::Finished;
        }
        if std::time::Instant::now() >= hold_deadline {
            send_warranted_handoff_failure(
                HandoffRollbackWarrant::NoCandidate,
                &job.cleanup,
                proxy,
                job.current_build,
                crate::UpdateHandoffCompletion::failure(
                    job.attempt_id,
                    Some(nonce),
                    None,
                    crate::UpdateHandoffOutcome::TimedOut,
                    format!(
                        "the outgoing process did not park within {} s",
                        hold_bound.as_secs()
                    ),
                ),
            );
            return Prelaunched::Finished;
        }
    }
}

#[cfg(target_os = "macos")]
fn prepare_early_artifacts(
    layout_path: &std::path::Path,
    layout: Option<crate::restore::RestoreManifest>,
) -> EarlyArtifacts {
    let started = std::time::Instant::now();
    let pipes = match AttemptPipes::make() {
        Ok(pipes) => Some(pipes),
        Err(why) => {
            aterm_log::warn!(
                "update apply: {why} before the park; the park makes the pipes itself"
            );
            None
        }
    };
    let layout = layout.and_then(|layout| {
        match crate::handoff_diet::speculate_layout(layout_path, &layout) {
            Ok(speculative) => Some(speculative),
            Err(why) => {
                aterm_log::info!(
                    "update apply: the layout sidecar was not written before the park ({why}); \
                     the park writes it"
                );
                None
            }
        }
    });
    aterm_log::info!(
        "update apply: before the park, in {:.1} ms with every reader live: {} and {}",
        started.elapsed().as_secs_f64() * 1000.0,
        if pipes.is_some() {
            "made the attempt's pipes"
        } else {
            "made no pipes"
        },
        if layout.is_some() {
            "wrote the layout sidecar the park will verify"
        } else {
            "wrote no layout sidecar"
        },
    );
    EarlyArtifacts { pipes, layout }
}

#[cfg(target_os = "macos")]
fn installed_bundle_is_target(build: u64, commit: &str) -> bool {
    installed_bundle_unproven(build, commit).is_none()
}

#[cfg(target_os = "macos")]
fn installed_bundle_unproven(build: u64, commit: &str) -> Option<String> {
    match aterm_update::installed_update_facts_checked() {
        Ok(Some(facts))
            if facts.build_number == build
                && aterm_update::commit_matches(&facts.git_commit, commit) =>
        {
            None
        }
        Ok(Some(facts)) => Some(format!(
            "the installed bundle holds build {} at commit {}",
            facts.build_number, facts.git_commit
        )),
        Ok(None) => Some("this process does not run from an installed bundle".to_string()),
        Err(refusal) => Some(format!("the installed bundle is not proven ({refusal})")),
    }
}

#[cfg(target_os = "macos")]
fn pre_launch_swap_failure(
    outcome: &Result<u64, aterm_update::ApplyOutcome>,
    target_build: u64,
) -> String {
    use aterm_update::ApplyOutcome;
    let cause = match outcome {
        Ok(installed) => {
            format!("it installed build {installed}, which is not the target build {target_build}")
        }
        Err(ApplyOutcome::NoUpdate) => format!(
            "nothing newer than this build is staged, and the installed bundle is not build \
             {target_build} (another install moved the stage on, or the stage was withdrawn)"
        ),
        Err(ApplyOutcome::NotApplicable) => {
            "this process does not run from an installed bundle the updater may replace".to_string()
        }
        Err(ApplyOutcome::Deferred(refusal)) => refusal.clone(),
        Err(ApplyOutcome::ReExecFailed(why)) => format!("the swap was rolled back ({why})"),
        Err(ApplyOutcome::Reverted(why)) => format!("the installed build was reverted ({why})"),
        Err(ApplyOutcome::LeftForSession { build, .. }) => {
            format!("build {build} was left staged and not installed")
        }
    };
    format!("the stage could not be installed before the launch: {cause}; nothing was launched")
}

#[cfg(target_os = "macos")]
fn run_prelaunched_handoff(
    job: &mut HandoffWorkerJob,
    proxy: &aterm_window::event_loop::EventLoopProxy<Wake>,
    mut lane: HandoffPrelaunchLane,
) -> Prelaunched {
    use std::os::fd::AsFd as _;

    let Some(bundle) = job.bundle.clone() else {
        return fork_after_park(
            job,
            proxy,
            lane,
            "this process does not run from a .app bundle",
        );
    };
    let activation = job.installed_activation.then_some(bundle.as_path());
    let dialer_code = match lane.dialer_code(aterm_update::effective_team_id(), &bundle, || {
        aterm_update::handoff_candidate_signing_identifier(activation)
    }) {
        Ok(code) => code,
        Err(error) => {
            return fork_after_park(
                job,
                proxy,
                lane,
                &format!("the launched lane cannot gate this successor's dial ({error})"),
            );
        }
    };
    let rendezvous = match crate::handoff_rendezvous::Rendezvous::bind(&lane.nonce) {
        Ok(rendezvous) => rendezvous,
        Err(error) => {
            return fork_after_park(
                job,
                proxy,
                lane,
                &format!("the handoff rendezvous could not be bound ({error})"),
            );
        }
    };
    let Some(rendezvous_path) = rendezvous.path().to_str().map(str::to_string) else {
        drop(rendezvous);
        return fork_after_park(job, proxy, lane, "the handoff rendezvous path is not UTF-8");
    };
    let Some(mut environment) = launch_environment(&job.command) else {
        drop(rendezvous);
        return fork_after_park(
            job,
            proxy,
            lane,
            "the launch environment needs a removal a merged launch cannot express",
        );
    };
    let Some(manifest_path) = crate::seamless::outgoing_manifest_path(&lane.nonce) else {
        send_handoff_producer_failure(
            job,
            proxy,
            Some(lane.nonce.clone()),
            "no private control directory to write the handoff manifest into",
        );
        return Prelaunched::Finished;
    };
    let layout_path = manifest_path.with_extension("layout.toml");
    environment.push(crate::seamless::ENV_MANIFEST.pair(manifest_path));
    environment.push(crate::seamless::ENV_NONCE.pair(&lane.nonce));
    environment.push(crate::seamless::ENV_LAYOUT.pair(&layout_path));
    environment.push(
        crate::seamless::ENV_TARGET.pair(crate::seamless::encode_target_identity(
            job.target_build,
            &job.target_commit,
        )),
    );
    environment.push(crate::handoff_rendezvous::ENV_RENDEZVOUS.pair(rendezvous_path));
    environment.push(crate::handoff_rendezvous::ENV_CLAIM.pair(rendezvous.claim()));
    if job.successor_grant_chunks {
        environment.push(
            crate::handoff_rendezvous::ENV_GRANT_CAPS
                .pair(crate::handoff_rendezvous::GRANT_CAPS_CHUNKS),
        );
    }
    environment.push(crate::handoff_warm_hint::ENV_WARM_HINT.pair(&lane.warm_hint));
    for (key, value) in crate::seamless::outgoing_parent_env() {
        environment.push((key, value));
    }
    let arguments = job
        .command
        .get_args()
        .map(std::ffi::OsStr::to_os_string)
        .collect::<Vec<_>>();

    if handoff_preparation_cancelled(job, proxy, Some(lane.nonce.clone())) {
        return Prelaunched::Finished;
    }
    if !job.installed_activation
        && let Some((expected_build, expected_commit, expected_digest)) = job.expected.clone()
    {
        let swap_at = std::time::Instant::now();
        match aterm_update::swap_staged_for_launch(
            job.current_build,
            crate::build_info::git_commit(),
            (expected_build, &expected_commit, &expected_digest),
        ) {
            Ok(installed) if installed == job.target_build => aterm_log::info!(
                "update apply: installed build {installed} in {} ms with every reader live, \
                 before the launch; the successor starts as that build and swaps nothing",
                swap_at.elapsed().as_millis()
            ),
            Err(aterm_update::ApplyOutcome::NoUpdate)
                if installed_bundle_is_target(job.target_build, &job.target_commit) =>
            {
                aterm_log::info!(
                    "update apply: build {} is already installed; launching it as it is",
                    job.target_build
                );
            }
            Err(aterm_update::ApplyOutcome::Deferred(refusal)) => {
                drop(rendezvous);
                aterm_log::warn!(
                    "update apply: the stage could not be installed before the launch \
                     ({refusal}); nothing was launched"
                );
                send_handoff_preparation_failure(
                    job,
                    proxy,
                    Some(lane.nonce.clone()),
                    pre_park_refusal_outcome(&refusal),
                    refusal,
                );
                return Prelaunched::Finished;
            }
            other => {
                drop(rendezvous);
                let detail = pre_launch_swap_failure(&other, job.target_build);
                aterm_log::warn!("update apply: {detail}");
                send_handoff_producer_failure(job, proxy, Some(lane.nonce.clone()), detail);
                return Prelaunched::Finished;
            }
        }
        if handoff_preparation_cancelled(job, proxy, Some(lane.nonce.clone())) {
            return Prelaunched::Finished;
        }
    }
    if job.renews_launch_record
        && let Some(gap) = installed_bundle_unproven(job.target_build, &job.target_commit)
    {
        drop(rendezvous);
        let detail = format!(
            "{gap}; a launch-record renewal launches only a bundle proven to be build {} at \
             commit {}, so nothing was launched",
            job.target_build, job.target_commit
        );
        aterm_log::warn!("launch record: {detail}");
        send_handoff_producer_failure(job, proxy, Some(lane.nonce.clone()), detail);
        return Prelaunched::Finished;
    }
    if job.renews_launch_record
        && handoff_preparation_cancelled(job, proxy, Some(lane.nonce.clone()))
    {
        return Prelaunched::Finished;
    }
    let dial_deadline = handoff_ready_deadline();
    let launch_at = std::time::Instant::now();
    let in_flight =
        match crate::app_launch_successor::begin_launch(&bundle, &arguments, &environment, false) {
            Ok(in_flight) => in_flight,
            Err(error) => {
                drop(rendezvous);
                return fork_after_park(
                    job,
                    proxy,
                    lane,
                    &format!("LaunchServices refused the successor ({error})"),
                );
            }
        };
    aterm_log::info!(
        "update apply: asked LaunchServices for the successor as its own launchd application \
         job with every reader live (not activated; activation is the parent's at Commit); \
         awaiting its rendezvous dial from code that meets {}",
        dialer_code.describe()
    );
    let early = prepare_early_artifacts(&layout_path, lane.speculative_layout.take());
    let named = std::cell::RefCell::new(NamedSuccessorWatch::default());
    let peer = match await_launched_dial(
        &rendezvous,
        &in_flight,
        &dialer_code,
        dial_deadline,
        &job.cancel,
        &named,
    ) {
        Ok(peer) => peer,
        Err(end @ (LaunchedDialEnd::Cancelled | LaunchedDialEnd::Rendezvous(_))) => {
            let retire = file_undialled_retire(
                job,
                lane.nonce,
                &end,
                rendezvous,
                &in_flight,
                named.into_inner(),
                STAND_DOWN_SELF_EXIT_GRACE,
                UNDIALLED_NAMING_PATIENCE,
            );
            send_undialled_retire(job, proxy, retire);
            return Prelaunched::Finished;
        }
        Err(LaunchedDialEnd::SuccessorExited(death)) => {
            send_pre_dial_death(
                job,
                proxy,
                lane.nonce,
                death,
                None,
                launch_at.elapsed(),
                &crate::handoff_rendezvous::launch_stamp_path(rendezvous.path()),
            );
            return Prelaunched::Finished;
        }
        Err(LaunchedDialEnd::CodeRefused {
            dialer,
            refusal,
            death,
        }) => {
            send_code_refused_dial(job, proxy, lane.nonce, dialer, &refusal, death);
            return Prelaunched::Finished;
        }
        Err(LaunchedDialEnd::ExitedWhileHeld { dialer, death }) => {
            send_pre_dial_death(
                job,
                proxy,
                lane.nonce,
                death,
                Some(dialer.pid),
                launch_at.elapsed(),
                &crate::handoff_rendezvous::launch_stamp_path(rendezvous.path()),
            );
            return Prelaunched::Finished;
        }
    };
    let dialled_at = std::time::Instant::now();
    aterm_log::info!(
        "update apply: successor dialled — launch->dial {} ms with every reader live; holding \
         its claim until the outgoing process parks",
        dialled_at.saturating_duration_since(launch_at).as_millis(),
    );
    let candidate = HandoffCandidate::of_attested_peer(pid_for_completion(peer.pid()));
    let _transfer = crate::identity_claim::register_handoff_candidate(candidate.pid);
    let exit_watch = CandidateExitWatch::watch(candidate.pid);
    if exit_watch.is_none() {
        aterm_log::warn!(
            "update apply: could not watch launched candidate {} for exit; a death on this \
             lane will be reported as Unobserved",
            candidate.pid
        );
    }
    let held = HeldSuccessor {
        rendezvous,
        peer,
        candidate,
        exit_watch,
        dialled_at,
    };
    let mut corroboration =
        LaunchCorroboration::of_dialer(&in_flight, candidate).reporting_to(proxy, job.attempt_id);
    if let Err(detail) = corroboration.poll() {
        retire_ungranted_successor(
            job,
            proxy,
            &lane,
            held,
            crate::UpdateHandoffOutcome::Rejected,
            detail,
        );
        return Prelaunched::Finished;
    }
    if !corroboration.answered {
        aterm_log::info!(
            "update apply: LaunchServices has not answered yet; the dial is the liveness \
             signal — the answer is read beside the hold and the proof wait"
        );
    }
    if held.peer.poll_hangup() {
        retire_ungranted_successor(
            job,
            proxy,
            &lane,
            held,
            crate::UpdateHandoffOutcome::ChildDied,
            "the successor closed the rendezvous after dialling, before any descriptor was sent"
                .to_string(),
        );
        return Prelaunched::Finished;
    }
    let mut corroboration = (!corroboration.answered).then_some(corroboration);
    if proxy
        .send_event(Wake::UpdateHandoffAwaitingPark {
            attempt_id: job.attempt_id,
            dialer_pid: Some(candidate.pid),
            grant_limit: Some(held.peer.grant_session_limit()),
        })
        .is_err()
    {
        retire_ungranted_successor(
            job,
            proxy,
            &lane,
            held,
            crate::UpdateHandoffOutcome::Rejected,
            "event loop closed before the park".to_string(),
        );
        return Prelaunched::Finished;
    }
    let transfer = match hold_for_park(job, &lane, &held, corroboration.as_mut()) {
        HoldOutcome::Transfer(transfer) => transfer,
        HoldOutcome::StandDown { outcome, detail } => {
            retire_ungranted_successor(job, proxy, &lane, held, outcome, detail);
            return Prelaunched::Finished;
        }
    };
    let HandoffTransferJob {
        capture,
        proof_deadline,
    } = *transfer;
    let park_at = capture.park_at;
    let captured_at = capture.captured_at;
    let picked_up_at = std::time::Instant::now();
    job.capture = Some(capture);
    aterm_log::info!(
        "update apply: the outgoing process parked {} ms after the dial; writing the \
         artifacts and granting on the held claim",
        park_at.saturating_duration_since(dialled_at).as_millis()
    );
    let prepared = match prepare_outgoing_artifacts(job, &lane.nonce, early) {
        Ok(prepared) => prepared,
        Err(PreparationFailure::Cancelled) => {
            retire_ungranted_successor(
                job,
                proxy,
                &lane,
                held,
                crate::UpdateHandoffOutcome::ActivityRevoked,
                "activity revoked handoff during physical preparation".to_string(),
            );
            return Prelaunched::Finished;
        }
        Err(PreparationFailure::Producer(detail)) => {
            retire_ungranted_successor(
                job,
                proxy,
                &lane,
                held,
                crate::UpdateHandoffOutcome::ProducerFailed,
                detail,
            );
            return Prelaunched::Finished;
        }
        Err(PreparationFailure::Refused(detail)) => {
            retire_ungranted_successor(
                job,
                proxy,
                &lane,
                held,
                crate::UpdateHandoffOutcome::CaptureRefused,
                detail,
            );
            return Prelaunched::Finished;
        }
    };
    let artifacts_at = std::time::Instant::now();
    if held.peer.poll_hangup() {
        retire_ungranted_successor(
            job,
            proxy,
            &lane,
            held,
            crate::UpdateHandoffOutcome::ChildDied,
            "the successor closed the rendezvous before the grant".to_string(),
        );
        return Prelaunched::Finished;
    }
    let sessions = job
        .capture()
        .live
        .iter()
        .map(|(local_id, master, pid)| {
            // SAFETY: `_owned_masters` keeps the duplicated master fds alive for the duration of the attempt.
            (*local_id, *pid, unsafe {
                std::os::fd::BorrowedFd::borrow_raw(*master)
            })
        })
        .collect::<Vec<_>>();
    crate::keeper_link::pending_before_grant(held.candidate.pid);
    let sendmsg_at = std::time::Instant::now();
    let transfer_failed = held
        .peer
        .transfer(
            &lane.nonce,
            &sessions,
            prepared.proof_wr.as_fd(),
            prepared.commit_rd.as_fd(),
            proof_deadline,
            Some(park_at),
        )
        .err();
    if let Some(error) = transfer_failed {
        // Only a failure before the descriptors left is an ungranted stand-down; after a
        // partial send the successor holds duplicates, and rollback owes the candidate
        // proof.
        if matches!(
            error,
            crate::handoff_rendezvous::RendezvousError::TransferPartial(_)
        ) {
            let HeldSuccessor {
                rendezvous,
                peer,
                candidate,
                exit_watch,
                ..
            } = held;
            drop(rendezvous);
            drop(peer);
            drop(prepared.proof_wr);
            drop(prepared.commit_rd);
            let mut handle = HandoffCandidateHandle::Launched(exit_watch);
            let rejected = worker_reject_and_reap_handoff_child(
                job,
                proxy,
                &mut handle,
                candidate,
                &lane.nonce,
                crate::UpdateHandoffOutcome::Rejected,
                format!("the handoff descriptors were delivered but the grant was not: {error}"),
            );
            debug_assert!(rejected, "Commit is unreachable before the grant body");
            return Prelaunched::Finished;
        }
        let outcome = match error {
            crate::handoff_rendezvous::RendezvousError::TooManySessions { .. } => {
                crate::UpdateHandoffOutcome::ProducerFailed
            }
            crate::handoff_rendezvous::RendezvousError::GrantDeadline => {
                crate::UpdateHandoffOutcome::TimedOut
            }
            _ => crate::UpdateHandoffOutcome::Rejected,
        };
        retire_ungranted_successor(
            job,
            proxy,
            &lane,
            held,
            outcome,
            format!("the handoff descriptors could not be delivered: {error}"),
        );
        return Prelaunched::Finished;
    }
    let granted_at = std::time::Instant::now();
    aterm_log::info!(
        "update apply: granted on the held claim — park->transfer {} ms (launch->dial was {} \
         ms, with every reader live)",
        granted_at.saturating_duration_since(park_at).as_millis(),
        dialled_at.saturating_duration_since(launch_at).as_millis(),
    );
    aterm_log::info!(
        "update apply: park->transfer split — {}",
        park_transfer_split_text(ParkTransferSplit {
            park_at,
            captured_at,
            picked_up_at,
            artifacts_at,
            sendmsg_at,
            granted_at,
        })
    );
    let HeldSuccessor {
        rendezvous,
        peer,
        candidate,
        exit_watch,
        ..
    } = held;
    drop(rendezvous);
    drop(peer);
    drop(prepared.proof_wr);
    drop(prepared.commit_rd);
    let mut handle = HandoffCandidateHandle::Launched(exit_watch);
    run_handoff_decision(
        job,
        proxy,
        &lane.nonce,
        prepared.expected,
        &prepared.proof_rd,
        prepared.commit_wr,
        candidate,
        &mut handle,
        proof_deadline,
        corroboration,
    );
    Prelaunched::Finished
}

#[cfg(target_os = "macos")]
#[must_use]
fn pid_for_completion(pid: i32) -> u32 {
    u32::try_from(pid).unwrap_or_default()
}

#[cfg(target_os = "macos")]
fn launch_environment(
    command: &aterm_uds::spawnfd::StdCommand,
) -> Option<Vec<(std::ffi::OsString, std::ffi::OsString)>> {
    launch_environment_over(command, |key| std::env::var_os(key))
}

#[cfg(target_os = "macos")]
const CONTROL_DIR_KEYS: [&str; 2] = ["XDG_RUNTIME_DIR", "HOME"];

#[cfg(target_os = "macos")]
fn launch_environment_over(
    command: &aterm_uds::spawnfd::StdCommand,
    launcher: impl Fn(&std::ffi::OsStr) -> Option<std::ffi::OsString>,
) -> Option<Vec<(std::ffi::OsString, std::ffi::OsString)>> {
    let mut pairs = Vec::new();
    for (key, value) in command.get_envs() {
        match value {
            Some(value) => pairs.push((key.to_os_string(), value.to_os_string())),
            None if launcher(key).is_some() => return None,
            None => (),
        }
    }
    for key in CONTROL_DIR_KEYS {
        if pairs.iter().any(|(set, _)| set == key) {
            continue;
        }
        if let Some(value) = launcher(key.as_ref()) {
            pairs.push((key.into(), value));
        }
    }
    Some(pairs)
}

#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
fn run_handoff_decision(
    job: &HandoffWorkerJob,
    proxy: &aterm_window::event_loop::EventLoopProxy<Wake>,
    nonce: &str,
    expected: crate::seamless::AdoptionProof,
    proof_rd: &std::os::fd::OwnedFd,
    commit_wr: std::os::fd::OwnedFd,
    candidate: HandoffCandidate,
    handle: &mut HandoffCandidateHandle,
    deadline: std::time::Instant,
    #[cfg(target_os = "macos")] corroboration: Option<LaunchCorroboration<'_>>,
) {
    let live = job.capture().live.clone();
    let proof_outcome = wait_handoff_ready(
        proof_rd,
        expected,
        &job.cancel,
        &live.iter().map(|(_, fd, _)| *fd).collect::<Vec<_>>(),
        deadline,
        &mut || handle.candidate_gone(candidate),
        #[cfg(target_os = "macos")]
        corroboration,
    );
    if proof_outcome != crate::UpdateHandoffOutcome::ProofReady {
        // A refusing, crashed or starved successor all close the readiness channel alike;
        // the successor logs its own refusal and the reject path adds what this process
        // saw.
        if proof_outcome == crate::UpdateHandoffOutcome::ChildDied {
            aterm_log::warn!(
                "update apply: the successor ended or closed the readiness channel without proving \
                 adoption. A REFUSAL, a CRASH and a STARVED child look identical here — read \
                 this log just above for the successor's own `seamless handoff refused:` / \
                 `overlap handoff:` line, which names the reason (most often: it never became \
                 the authorized build because its boot apply could not swap), and just below \
                 for the death this process could witness (`observed …`)."
            );
        }
        let rejected = worker_reject_and_reap_handoff_child(
            job,
            proxy,
            handle,
            candidate,
            nonce,
            proof_outcome,
            format!("handoff proof ended {proof_outcome:?}"),
        );
        debug_assert!(rejected, "Commit is unreachable before ProofReady");
        return;
    }
    crate::seamless::retire_outgoing_controls(nonce);

    let (reject, rejected) = std::sync::mpsc::sync_channel(1);
    let ready = Wake::UpdateHandoffFinished(crate::UpdateHandoffCompletion {
        attempt_id: job.attempt_id,
        nonce: Some(nonce.to_string()),
        child_pid: Some(candidate.pid),
        outcome: crate::UpdateHandoffOutcome::ProofReady,
        commit_fd: Some(commit_wr),
        reject: Some(reject),
        reconcile: None,
        detail: "child painted and proved exact readerless adoption".to_string(),
        input_drain_spins: 0,
        child_death: crate::ChildDeathEvidence::Unobserved,
    });
    if proxy.send_event(ready).is_err() {
        let rejected = worker_reject_and_reap_handoff_child(
            job,
            proxy,
            handle,
            candidate,
            nonce,
            crate::UpdateHandoffOutcome::Rejected,
            "event loop closed before final handoff admission".to_string(),
        );
        debug_assert!(rejected, "Commit is unreachable after a failed ready wake");
        return;
    }

    let decision_deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        // Readable PTY output does not revoke here (post-park bytes wait in the
        // kernel for the child). Only a cancel poke (`ActivityRevoked`) or session
        // death (HUP/ERR) rejects before Commit.
        if job.cancel.try_recv().is_ok()
            && worker_reject_and_reap_handoff_child(
                job,
                proxy,
                handle,
                candidate,
                nonce,
                crate::UpdateHandoffOutcome::ActivityRevoked,
                "structural activity revoked handoff before Commit".to_string(),
            )
        {
            return;
        }
        if handoff_masters_closed(&live)
            && worker_reject_and_reap_handoff_child(
                job,
                proxy,
                handle,
                candidate,
                nonce,
                crate::UpdateHandoffOutcome::ActivityRevoked,
                "a handed-off PTY session closed before Commit".to_string(),
            )
        {
            return;
        }
        match rejected.try_recv() {
            Ok(()) | Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                if worker_reject_and_reap_handoff_child(
                    job,
                    proxy,
                    handle,
                    candidate,
                    nonce,
                    crate::UpdateHandoffOutcome::Rejected,
                    "final handoff admission rejected before Commit".to_string(),
                ) {
                    return;
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
        if handle.candidate_gone(candidate)
            && worker_reject_and_reap_handoff_child(
                job,
                proxy,
                handle,
                candidate,
                nonce,
                crate::UpdateHandoffOutcome::Rejected,
                "the successor ended after proving adoption, before Commit".to_string(),
            )
        {
            return;
        }
        if std::time::Instant::now() >= decision_deadline
            && worker_reject_and_reap_handoff_child(
                job,
                proxy,
                handle,
                candidate,
                nonce,
                crate::UpdateHandoffOutcome::Rejected,
                "main-thread final handoff decision timed out".to_string(),
            )
        {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

/// Ceiling on the decode authority one capture may hand its successor.
#[cfg(unix)]
pub(crate) const MAX_HANDOFF_CAPTURE_BUDGET_BYTES: u64 = 4 * 1024 * 1024 * 1024;

#[cfg(unix)]
fn try_lock_by<T>(
    lock: &std::sync::Mutex<T>,
    deadline: std::time::Instant,
) -> Option<std::sync::MutexGuard<'_, T>> {
    loop {
        match lock.try_lock() {
            Ok(guard) => return Some(guard),
            Err(std::sync::TryLockError::Poisoned(poison)) => return Some(poison.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => {
                if std::time::Instant::now() >= deadline {
                    #[cfg(test)]
                    park_budget_for_test::gave_up(lock);
                    return None;
                }
                std::thread::yield_now();
            }
        }
    }
}

/// The wall-clock window the park and capture must fit in, from the readers' park.
#[cfg(any(unix, test))]
pub(crate) fn handoff_freeze_budget(
    mode: crate::native_updater_service::ApplyMode,
    prior_physical_failures: u8,
    seed: FreezeSeed,
) -> std::time::Duration {
    use crate::native_updater_service::ApplyMode;
    let rung0 = seed.rung0();
    match mode {
        ApplyMode::Immediate => FREEZE_LAST_RUNG,
        ApplyMode::Automatic | ApplyMode::AutomaticPastGrace => match prior_physical_failures {
            0 => rung0,
            1 => rung0.saturating_mul(2).max(FREEZE_RUNG0_CEILING),
            _ => FREEZE_LAST_RUNG,
        },
    }
}

/// The first automatic rung's floor, and what an unmeasured desk gets.
#[cfg(any(unix, test))]
pub(crate) const FREEZE_RUNG0_FLOOR: std::time::Duration = std::time::Duration::from_millis(20);

/// The first automatic rung's ceiling.
#[cfg(any(unix, test))]
pub(crate) const FREEZE_RUNG0_CEILING: std::time::Duration = std::time::Duration::from_millis(80);

#[cfg(any(unix, test))]
const FREEZE_LAST_RUNG: std::time::Duration = std::time::Duration::from_millis(250);

/// Rung 0 from a measured freeze.
#[cfg(any(unix, test))]
#[must_use]
pub(crate) fn freeze_rung0_from_capture(capture: std::time::Duration) -> std::time::Duration {
    (capture.saturating_mul(3) / 2).clamp(FREEZE_RUNG0_FLOOR, FREEZE_RUNG0_CEILING)
}

/// Where an attempt's first automatic freeze rung came from.
#[cfg(any(unix, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FreezeSeed {
    Default,
    #[cfg(any(target_os = "macos", test))]
    DryRun {
        capture: std::time::Duration,
        sessions: usize,
        last_park: Option<std::time::Duration>,
    },
    Ledger {
        capture: std::time::Duration,
        build: u64,
    },
}

#[cfg(any(unix, test))]
impl FreezeSeed {
    /// The first automatic rung this seed buys.
    #[must_use]
    pub(crate) fn rung0(self) -> std::time::Duration {
        match self {
            Self::Default => FREEZE_RUNG0_FLOOR,
            #[cfg(any(target_os = "macos", test))]
            Self::DryRun {
                capture, last_park, ..
            } => freeze_rung0_from_capture(capture.max(last_park.unwrap_or_default())),
            Self::Ledger { capture, .. } => freeze_rung0_from_capture(capture),
        }
    }

    /// The ledger's prior, or the default when neither was measured.
    #[must_use]
    pub(crate) const fn from_prior(prior: Option<&aterm_update::HandoffCaptureRecord>) -> Self {
        let Some(record) = prior else {
            return Self::Default;
        };
        let (need_us, build) = if record.park_us >= record.capture_us {
            (record.park_us, record.park_build)
        } else {
            (record.capture_us, record.build)
        };
        if need_us == 0 {
            return Self::Default;
        }
        Self::Ledger {
            capture: std::time::Duration::from_micros(need_us),
            build,
        }
    }

    /// The last landed park the ledger holds, if any.
    #[cfg(any(target_os = "macos", test))]
    #[must_use]
    pub(crate) fn last_park_of(
        prior: Option<&aterm_update::HandoffCaptureRecord>,
    ) -> Option<std::time::Duration> {
        prior
            .filter(|record| record.park_us > 0)
            .map(|record| std::time::Duration::from_micros(record.park_us))
    }
}

#[cfg(any(unix, test))]
impl std::fmt::Display for FreezeSeed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Default => formatter.write_str("rung 0 at its default: no capture was timed"),
            #[cfg(any(target_os = "macos", test))]
            Self::DryRun {
                capture,
                sessions,
                last_park,
            } => {
                write!(
                    formatter,
                    "rung 0 seeded from this attempt's dry-run capture of {:.1} ms over \
                     {sessions} session(s)",
                    capture.as_secs_f64() * 1e3
                )?;
                match last_park {
                    Some(park) => write!(
                        formatter,
                        " and the last landed park's {:.1} ms",
                        park.as_secs_f64() * 1e3
                    ),
                    None => formatter.write_str(" (no landed park is on record)"),
                }
            }
            Self::Ledger { capture, build } => write!(
                formatter,
                "rung 0 seeded from the ledger's last timed freeze, {:.1} ms (build {build})",
                capture.as_secs_f64() * 1e3
            ),
        }
    }
}

/// Records what a landed park cost, for the next update's first rung, on its own thread.
#[cfg(unix)]
pub(crate) fn keep_landed_park(
    parked_in: std::time::Duration,
) -> Option<std::thread::JoinHandle<()>> {
    let build = crate::running_build_number();
    spawn_ledger_writer("aterm-park-cost", move || {
        aterm_update::record_handoff_park(build, parked_in);
    })
    .ok()
}

#[cfg(unix)]
fn spawn_ledger_writer(
    name: &str,
    write: impl FnOnce() + Send + 'static,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    #[cfg(test)]
    let in_flight = ledger_writers::InFlight::enter();
    std::thread::Builder::new()
        .name(name.to_string())
        .spawn(move || {
            #[cfg(test)]
            let _in_flight = in_flight;
            crate::qos::set_self(crate::qos::Role::Background);
            write();
        })
}

#[cfg(unix)]
type ChildOnlyMasters = (Vec<std::os::fd::OwnedFd>, Vec<(u64, i32, i32)>);

#[cfg(unix)]
fn reserve_child_only_masters(live: &[(u64, i32, i32)]) -> std::io::Result<ChildOnlyMasters> {
    use std::os::fd::{AsRawFd as _, FromRawFd as _};
    let mut owned_masters = Vec::with_capacity(live.len());
    let mut adoption = Vec::with_capacity(live.len());
    for (local_id, master, pid) in live {
        // SAFETY: `F_DUPFD_CLOEXEC` duplicates one live master fd without reading memory.
        let duplicate = unsafe { libc::fcntl(*master, libc::F_DUPFD_CLOEXEC, 3) };
        if duplicate < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: F_DUPFD_CLOEXEC returned a fresh descriptor owned here.
        let owned = unsafe { std::os::fd::OwnedFd::from_raw_fd(duplicate) };
        adoption.push((*local_id, owned.as_raw_fd(), *pid));
        owned_masters.push(owned);
    }
    Ok((owned_masters, adoption))
}

#[cfg(all(test, unix))]
pub(crate) mod ledger_writers {
    use std::sync::{Condvar, Mutex, PoisonError};

    static IN_FLIGHT: Mutex<usize> = Mutex::new(0);
    static LANDED: Condvar = Condvar::new();

    /// One writer in flight, until it is dropped.
    pub(crate) struct InFlight(());

    impl InFlight {
        pub(crate) fn enter() -> Self {
            *IN_FLIGHT.lock().unwrap_or_else(PoisonError::into_inner) += 1;
            Self(())
        }
    }

    impl Drop for InFlight {
        fn drop(&mut self) {
            let mut in_flight = IN_FLIGHT.lock().unwrap_or_else(PoisonError::into_inner);
            *in_flight -= 1;
            drop(in_flight);
            LANDED.notify_all();
        }
    }

    /// Block until no detached ledger writer is in flight.
    pub(crate) fn wait_until_landed() {
        let in_flight = IN_FLIGHT.lock().unwrap_or_else(PoisonError::into_inner);
        let (_in_flight, waited) = LANDED
            .wait_timeout_while(in_flight, std::time::Duration::from_secs(60), |n| *n > 0)
            .unwrap_or_else(PoisonError::into_inner);
        assert!(
            !waited.timed_out(),
            "a detached update-ledger writer never finished"
        );
    }
}

/// The dry run's whole window.
#[cfg(any(target_os = "macos", all(test, unix)))]
pub(crate) const DRY_RUN_CAPTURE_WINDOW: std::time::Duration =
    std::time::Duration::from_micros(53_334);

#[cfg(any(target_os = "macos", all(test, unix)))]
const _: () =
    assert!(DRY_RUN_CAPTURE_WINDOW.as_micros() * 3 >= FREEZE_RUNG0_CEILING.as_micros() * 2);

/// What one dry run of the park's capture measured.
#[cfg(any(target_os = "macos", all(test, unix)))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DryRunCapture {
    Measured {
        took: std::time::Duration,
        sessions: usize,
    },
    AtLeast {
        took: std::time::Duration,
        sessions: usize,
    },
    Unmeasured(&'static str),
}

/// Whether one session may spend `optional` on a budget on top of its `own_mandatory`.
#[cfg(unix)]
pub(crate) fn optional_carry_fits(
    used: u64,
    own_mandatory: u64,
    optional: u64,
    later_mandatory: u64,
    ceiling: u64,
) -> bool {
    used.checked_add(own_mandatory)
        .and_then(|total| total.checked_add(optional))
        .and_then(|total| total.checked_add(later_mandatory))
        .is_some_and(|total| total <= ceiling)
}

/// Why the park's capture could not produce a committed screen set.
#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CaptureFailure {
    Deadline {
        freeze_ms: u128,
    },
    EngineBusy,
    Storage,
    MidSequence {
        local_id: u64,
        state: &'static str,
    },
    Refused {
        local_id: Option<u64>,
        cause: String,
    },
}

#[cfg(unix)]
impl std::fmt::Display for CaptureFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Deadline { freeze_ms } => write!(
                formatter,
                "bounded visible-screen capture exceeded {freeze_ms} ms"
            ),
            Self::EngineBusy => formatter
                .write_str("a terminal engine stayed busy for the whole handoff capture window"),
            Self::Storage => {
                formatter.write_str("visible checkpoint set could not reserve bounded storage")
            }
            Self::MidSequence { local_id, state } => write!(
                formatter,
                "session {local_id}'s parser was parked mid-sequence ({state}) with the rest of \
                 that sequence still queued on its PTY; the park retries on a sequence boundary"
            ),
            Self::Refused { cause, .. } => formatter.write_str(cause),
        }?;
        formatter.write_str("; update handoff stayed in place")
    }
}

#[cfg(unix)]
struct ParkedScreens {
    screens: Vec<(u64, aterm_core::terminal::TerminalCheckpoint)>,
    carries: Vec<crate::handoff_carry::CarrySource>,
    screen_digest: [u8; 32],
    repaint: Vec<u64>,
    fg_holders: Vec<(u64, i32)>,
    render_debts: Vec<(u64, aterm_core::terminal::RenderDebt)>,
    history_heads: Vec<crate::handoff_history::HistoryHead>,
    held: Vec<crate::seamless::HeldScreen>,
}

/// Why the overlap handoff cannot run in this process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HandoffUnavailable {
    UnownedControlSocket,
    Headless,
    NoEventLoopProxy,
}

impl HandoffUnavailable {
    /// Every reason, in the order the predicate asks.
    pub(crate) const ALL: [Self; 3] = [
        Self::UnownedControlSocket,
        Self::Headless,
        Self::NoEventLoopProxy,
    ];

    /// What is set.
    #[must_use]
    pub(crate) const fn cause(self) -> &'static str {
        match self {
            Self::UnownedControlSocket => {
                "the configured control socket is not owned by this process"
            }
            Self::Headless => "--headless (no window)",
            Self::NoEventLoopProxy => "no event loop",
        }
    }

    /// The log line's remedy, for the reasons that have one.
    #[must_use]
    #[cfg(unix)]
    pub(crate) const fn remedy(self) -> &'static str {
        match self {
            Self::UnownedControlSocket => {
                " Resolve the control-socket ownership conflict before updating."
            }
            Self::Headless => " A windowed launch restores the default.",
            Self::NoEventLoopProxy => "",
        }
    }

    fn holds_for(self, app: &App) -> bool {
        match self {
            Self::UnownedControlSocket => {
                use aterm_types::control_socket::SocketDirective;
                match crate::cli::launch_flags().socket_directive() {
                    SocketDirective::Explicit(path) => {
                        let plan = crate::control_auth::SocketPlan {
                            sock_path: path.to_string(),
                            token_path: crate::control_auth::token_path_for_socket(&path),
                            latest_link: None,
                        };
                        !crate::control_socket_identity::published()
                            .is_some_and(|identity| identity.matches_plan_path(&plan))
                    }
                    _ => false,
                }
            }
            Self::Headless => app.headless,
            Self::NoEventLoopProxy => app.proxy.is_none(),
        }
    }
}

#[cfg(all(test, unix))]
pub(crate) mod park_budget_for_test {
    use std::cell::Cell;
    use std::sync::{Condvar, Mutex, PoisonError};
    use std::time::Duration;

    thread_local! {
        static REAL: Cell<bool> = const { Cell::new(false) };
    }

    /// A test park's deadline unless [`real`] is held: a hang detector.
    pub(crate) const HANG_DETECTOR: Duration = Duration::from_secs(60);

    /// Holds the real budget on this thread until dropped.
    #[must_use = "the park runs on the real budget only while the guard is held"]
    pub(crate) struct Real(());

    impl Drop for Real {
        fn drop(&mut self) {
            REAL.with(|real| real.set(false));
        }
    }

    pub(crate) fn real() -> Real {
        REAL.with(|real| real.set(true));
        Real(())
    }

    /// The budget the park's deadline is stamped from.
    pub(crate) fn deadline_budget(budget: Duration) -> Duration {
        if REAL.with(Cell::get) {
            budget
        } else {
            HANG_DETECTOR
        }
    }

    static WATCHED: Mutex<Vec<(usize, bool)>> = Mutex::new(Vec::new());
    static GAVE_UP: Condvar = Condvar::new();

    fn address<T>(lock: &Mutex<T>) -> usize {
        std::ptr::from_ref(lock).addr()
    }

    /// A bounded take of `lock` ran past its deadline.
    pub(super) fn gave_up<T>(lock: &Mutex<T>) {
        let at = address(lock);
        let mut watched = WATCHED.lock().unwrap_or_else(PoisonError::into_inner);
        for (watched_at, gave_up) in watched.iter_mut() {
            if *watched_at == at {
                *gave_up = true;
            }
        }
        GAVE_UP.notify_all();
    }

    /// Watch `lock` from now.
    pub(crate) fn watch<T>(lock: &Mutex<T>) {
        WATCHED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((address(lock), false));
    }

    /// Blocks until a bounded take of the [`watch`]ed `lock` has given up, and stops watching it.
    pub(crate) fn wait_gave_up<T>(lock: &Mutex<T>) {
        let at = address(lock);
        let watched = WATCHED.lock().unwrap_or_else(PoisonError::into_inner);
        let (mut watched, waited) = GAVE_UP
            .wait_timeout_while(watched, HANG_DETECTOR, |watched| {
                !watched.contains(&(at, true))
            })
            .unwrap_or_else(PoisonError::into_inner);
        assert!(
            !waited.timed_out(),
            "no bounded take of the watched lock gave up"
        );
        watched.retain(|(watched_at, _)| *watched_at != at);
    }
}

impl App {
    /// Whether the overlap handoff can run here.
    #[must_use]
    pub(crate) fn seamless_handoff_unavailable(&self) -> Option<HandoffUnavailable> {
        HandoffUnavailable::ALL
            .into_iter()
            .find(|why| why.holds_for(self))
    }

    /// This process keeps the image it was launched with (`App::launch_image_kept`).
    #[must_use]
    pub(crate) const fn launch_keeps_image(&self) -> bool {
        #[cfg(unix)]
        {
            self.launch_image_kept
        }
        #[cfg(not(unix))]
        {
            false
        }
    }

    /// A process that keeps its image with no live terminal session to hand over.
    #[must_use]
    pub(crate) fn kept_image_waits_for_terminal(&self) -> bool {
        #[cfg(unix)]
        {
            if !self.launch_image_kept {
                return false;
            }
            let store = self
                .store
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            !self.pool.iter().any(|session| {
                !store.by_local(session.id).is_some_and(|handle| {
                    handle.state == crate::session_store::SessionState::Exited
                })
            })
        }
        #[cfg(not(unix))]
        {
            false
        }
    }
}

impl App {
    /// Applies a staged update now by re-executing.
    pub(crate) fn apply_staged_update_now(
        &mut self,
        safety_token: crate::app_native::NativeUpdateSafetyToken,
        mode: crate::native_updater_service::ApplyMode,
        apply_attempt: Option<crate::native_updater_service::ApplyAttemptTicket>,
        same_image: Option<SameImageHandoff>,
    ) -> Result<(), crate::UpdateHandoffStartError> {
        if self.update_handoff_in_flight() {
            #[cfg(target_os = "macos")]
            if self.launch_record_renewal_in_flight() {
                return Err(crate::UpdateHandoffStartError::activity(
                    "a launch-record renewal's handoff is in flight; the update follows it",
                ));
            }
            return Err(crate::UpdateHandoffStartError::failed(
                "an update handoff is already in flight",
            ));
        }
        #[cfg(target_os = "macos")]
        if self.workstation_setup.pending() {
            return Err(crate::UpdateHandoffStartError::activity(
                "the Mac setup countdown is still open",
            ));
        }
        #[cfg(target_os = "macos")]
        if self.askpass_sheet.is_some() {
            return Err(crate::UpdateHandoffStartError::activity(
                "a sudo password prompt is still open",
            ));
        }
        self.retire_spare("update", None);
        if !self.tab_spawns.is_empty() {
            #[cfg(unix)]
            return Err(crate::UpdateHandoffStartError::activity(
                "a new tab is still starting",
            ));
            #[cfg(not(unix))]
            return Err(crate::UpdateHandoffStartError::refused(
                "a new tab is still starting",
            ));
        }
        let build = crate::build_info::build_number()
            .parse::<u64>()
            .unwrap_or(0);
        let same_image = match same_image {
            Some(SameImageHandoff::DebugSeam) | None => {
                crate::app_update_screen::debug_seamless_reexec_armed()
                    .then_some(SameImageHandoff::DebugSeam)
            }
            #[cfg(target_os = "macos")]
            Some(SameImageHandoff::RenewLaunchRecord) => {
                if apply_attempt.is_some()
                    || !mode.is_automatic()
                    || !self.launch_record_renewal_authorizes()
                {
                    return Err(crate::UpdateHandoffStartError::failed(
                        "a launch-record renewal is not armed",
                    ));
                }
                Some(SameImageHandoff::RenewLaunchRecord)
            }
        };
        if same_image.is_none() && apply_attempt.is_none() {
            let message = "no exact verified update authority was supplied".to_string();
            aterm_log::info!("update apply: {message}");
            return Err(crate::UpdateHandoffStartError::failed(message));
        }
        let exe = match std::env::current_exe() {
            Ok(exe) => exe,
            Err(error) => {
                let message = format!("current executable is unavailable: {error}");
                aterm_log::warn!("update apply: {message}; cannot re-exec");
                return Err(crate::UpdateHandoffStartError::failed(message));
            }
        };
        aterm_log::info!("update apply: authorized process replacement from build {build}");
        #[cfg(unix)]
        {
            let started = self.start_unix_update_handoff(
                exe,
                build,
                safety_token,
                mode,
                apply_attempt,
                same_image,
            );
            if let Err(error) = &started
                && error.held_for_read_of().is_none()
            {
                self.retire_update_installing();
                if !(error.is_capture_refused() && mode.is_automatic())
                    && !same_image.is_some_and(SameImageHandoff::renews_launch_record)
                {
                    self.record_update_switch_stopped(
                        error.stops_routinely(mode),
                        &error.to_string(),
                    );
                }
            }
            return started;
        }
        #[cfg(windows)]
        {
            let _ = mode;
            let live_ptys = self.pool.iter().count();
            let facts = crate::native_update_admission::AdmissionFacts {
                staged_verified: same_image.is_some() || apply_attempt.is_some(),
                seamless_capable: false,
                native_state_certified: safety_token.is_certified(),
                live_ptys,
                foreground_jobs: 0,
                unknown_foregrounds: live_ptys,
                image_kept: false,
            };
            match crate::native_update_admission::classify(facts) {
                crate::native_update_admission::AdmissionDecision::Apply(
                    crate::native_update_admission::ApplyLane::Cold,
                ) if live_ptys == 0 => {}
                crate::native_update_admission::AdmissionDecision::Block(reason) => {
                    return Err(crate::UpdateHandoffStartError::refused(
                        reason.message(facts),
                    ));
                }
                _ => {
                    return Err(crate::UpdateHandoffStartError::refused(
                        "Windows update replacement requires an exact zero-session state",
                    ));
                }
            }
            self.flush_messages_log_within(crate::messages_host::MESSAGES_FLUSH_EXIT_WAIT);
            let spawn = || {
                let _ = crate::logging::flush_within(crate::logging::LOG_FLUSH_EXIT_WAIT);
                let mut cmd = aterm_uds::spawnfd::command(exe);
                cmd.args(aterm_update::reexec_forwarded_args(
                    std::env::args_os().skip(1),
                ))
                .env("ATERM_UPDATED_FROM", build.to_string());
                bind_expected_update_artifact(&mut cmd, apply_attempt.as_ref());
                match cmd.spawn() {
                    Ok(_) => std::process::exit(0),
                    Err(error) => Err(error),
                }
            };
            match spawn() {
                Ok(()) => {
                    return Err(crate::UpdateHandoffStartError::failed(
                        "replacement returned after a successful Windows spawn",
                    ));
                }
                Err(err) => {
                    aterm_log::warn!("update apply: re-spawn failed: {err}");
                    return Err(crate::UpdateHandoffStartError::failed(format!(
                        "replacement process could not start: {err}"
                    )));
                }
            }
        }

        #[allow(unreachable_code)]
        Err(crate::UpdateHandoffStartError::failed(
            "process replacement returned without applying the update",
        ))
    }

    /// The cold lane's steps before the `exec`.
    #[cfg(unix)]
    pub(crate) fn prepare_cold_exec(&mut self, build: u64, same_image: bool, automatic: bool) {
        self.note_update_switch_started(build, same_image, automatic);
        self.flush_messages_log_within(MESSAGES_FLUSH_COMMIT_WAIT);
        self.consent_observer.stop();
        if !aterm_pty::wait_for_reaps(COLD_EXEC_REAP_WAIT) {
            aterm_log::warn!(
                "update apply: a closed tab's shell reap had not settled after {} ms; the exec goes on",
                COLD_EXEC_REAP_WAIT.as_millis()
            );
        }
    }

    /// The cold `exec`, the control socket retired right before it.
    #[cfg(unix)]
    pub(crate) fn run_cold_exec(&self, exec: impl FnOnce() -> std::io::Error) -> std::io::Error {
        let Some(_quit_held) = crate::owned_endpoint::hold_quit_for_exec() else {
            aterm_log::info!(
                "update apply: a terminating signal asked this instance to quit; the exec \
                 is not run"
            );
            return std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "a terminating signal asked this instance to quit first",
            );
        };
        let retired = self.control_endpoint.retire();
        aterm_log::info!(
            "update apply: control socket retired for the exec (listener closed: {}, \
             connections shut down: {})",
            retired.listener,
            retired.connections
        );
        let _ = crate::logging::flush_within(crate::logging::LOG_FLUSH_EXIT_WAIT);
        exec()
    }

    /// A cold `exec` that did not happen (it returned an error) leaves this process running.
    #[cfg(unix)]
    pub(crate) fn resume_after_cold_exec(&mut self) {
        if !self.control_endpoint.reopen() {
            aterm_log::warn!(
                "update apply: the control socket could not be reopened after the exec \
                 returned; its watchdog rebinds it"
            );
        }
        let _ = self.reconcile_consent_observer();
    }

    #[cfg(unix)]
    fn handoff_parent_socket(&self) -> Option<(std::path::PathBuf, String)> {
        self.sock_bound
            .load(std::sync::atomic::Ordering::Acquire)
            .then(|| {
                let plan = self.sock_plan.as_ref()?;
                Some((plan.latest_link.clone()?, plan.sock_path.clone()))
            })
            .flatten()
    }

    #[cfg(unix)]
    fn start_unix_update_handoff(
        &mut self,
        exe: std::path::PathBuf,
        build: u64,
        safety_token: crate::app_native::NativeUpdateSafetyToken,
        mode: crate::native_updater_service::ApplyMode,
        apply_attempt: Option<crate::native_updater_service::ApplyAttemptTicket>,
        same_image: Option<SameImageHandoff>,
    ) -> Result<(), crate::UpdateHandoffStartError> {
        let renewal = same_image.is_some_and(SameImageHandoff::renews_launch_record);
        let live: Vec<(u64, i32, i32)> = self.handoff_live_sessions();
        let automatic_activity_epoch = (mode
            == crate::native_updater_service::ApplyMode::Automatic)
            .then_some(self.update_handoff_activity_epoch);
        if automatic_activity_epoch.is_some()
            && !self.automatic_update_activity_quiet(std::time::Instant::now())
        {
            return Err(crate::UpdateHandoffStartError::activity(
                "terminal activity has not reached the automatic-update quiet epoch",
            ));
        }
        let (foreground_jobs, unknown_foregrounds) = live.iter().fold(
            (0usize, 0usize),
            |(jobs, unknown), (_, master, shell_pid)| {
                let foreground = crate::quit_safety::foreground_pgrp(*master);
                if foreground <= 0 {
                    (jobs, unknown + 1)
                } else if foreground != *shell_pid {
                    (jobs + 1, unknown)
                } else {
                    (jobs, unknown)
                }
            },
        );
        let handoff_unavailable = self.seamless_handoff_unavailable();
        if let Some(why) = handoff_unavailable {
            if self.launch_image_kept {
                aterm_log::warn!(
                    "update apply: {} — the seamless overlap handoff is DISABLED for this \
                     process, and it keeps its image, so it never re-execs cold either: \
                     nothing installs in this process, open terminals or not; the next \
                     launch installs it.{}",
                    why.cause(),
                    why.remedy()
                );
            } else {
                aterm_log::warn!(
                    "update apply: {} — the seamless overlap handoff is DISABLED for this \
                     process. The update is refused while any terminal session is open; \
                     with none open it re-execs cold (the staged build swaps in at the top \
                     of the new main), which is not the path this build's handoff proofs \
                     cover.{}",
                    why.cause(),
                    why.remedy()
                );
            }
        }
        if live.is_empty()
            && !(self.launch_image_kept && handoff_unavailable.is_some())
            && !self.handoff_exited_session_ids().is_empty()
        {
            return Err(crate::UpdateHandoffStartError::refused(
                if self.launch_image_kept {
                    "every open terminal pane's command has exited (panes kept open by \
                     --hold); the update applies once a new tab opens, because this aterm \
                     hands its terminals to the new build and never restarts in place"
                } else {
                    "every open terminal pane's command has exited (panes kept open by \
                     --hold); the update applies once one is closed or a new tab opens, \
                     because replacing the app now would close them"
                },
            ));
        }
        let overlap_available = handoff_unavailable.is_none();
        let facts = crate::native_update_admission::AdmissionFacts {
            staged_verified: same_image.is_some() || apply_attempt.is_some(),
            seamless_capable: overlap_available && !live.is_empty(),
            native_state_certified: safety_token.is_certified(),
            live_ptys: live.len(),
            foreground_jobs,
            unknown_foregrounds,
            image_kept: self.launch_image_kept || renewal,
        };
        let decision = crate::native_update_admission::classify(facts);
        let target_build = apply_attempt
            .as_ref()
            .map_or(build, |attempt| attempt.target_build());
        if let crate::native_update_admission::AdmissionDecision::Apply(lane) = decision
            && let Some(refusal) = lane_carry_refusal(&safety_token, lane, build, target_build)
        {
            return Err(crate::UpdateHandoffStartError::refused(refusal));
        }
        match decision {
            crate::native_update_admission::AdmissionDecision::Block(reason) => {
                return Err(update_admission_refusal(reason, facts, handoff_unavailable));
            }
            crate::native_update_admission::AdmissionDecision::Apply(
                crate::native_update_admission::ApplyLane::Cold,
            ) => {
                // The admission already refuses it (`image_kept`); said again where the
                // exec is, whatever the facts.
                if renewal {
                    return Err(crate::UpdateHandoffStartError::refused(
                        "a launch-record renewal never execs in place: only a fresh launch \
                         renews the record",
                    ));
                }
                debug_assert!(live.is_empty());
                self.cold_lane_verdict_gate(mode, apply_attempt.as_ref())?;
                let lifeline_fd = crate::lifeline::relaunch_descriptor();
                #[expect(
                    clippy::disallowed_types,
                    reason = "the update successor's raw Command: it hands over exactly its own descriptors, which a strip at the exec would undo"
                )]
                let mut command = std::process::Command::new(exe);
                command
                    .args(aterm_update::running_window_successor_args(
                        std::env::args_os().skip(1),
                        lifeline_fd,
                    ))
                    .env("ATERM_UPDATED_FROM", build.to_string())
                    .env(aterm_update::RELAUNCH_OF_RUNNING_ENV, "1");
                let bound = apply_attempt
                    .as_ref()
                    .filter(|attempt| !attempt.is_installed_activation());
                bind_expected_update_artifact(&mut command, bound);
                bind_cold_lane_budget(
                    &mut command,
                    bound,
                    bound.map(|attempt| self.cold_lane_budget(attempt, std::time::Instant::now())),
                );
                bind_cold_lane_health_said(&mut command, &self.carried_health_said());
                let layout = self
                    .cold_lane_layout()
                    .map_err(crate::UpdateHandoffStartError::refused)?;
                let run = self
                    .crash_journal
                    .run()
                    .map(crate::crash_journal::JournalId::stem);
                let left = leave_cold_lane_layout(layout.as_ref(), |layout| {
                    crate::restore::write_quit_within(
                        layout,
                        run.as_deref(),
                        COLD_LANE_LAYOUT_LOCK_WAIT,
                    )
                })?;
                if left == LayoutLeft::Written {
                    self.cold_lane_layout_rewritten();
                } else if let Err(refusal) =
                    self.settle_owed_layout_take_back_before_switch(std::time::Instant::now())
                {
                    return Err(crate::UpdateHandoffStartError::refused(refusal));
                }
                crate::descriptor_limit::carry_launcher_limit(&mut command, target_build);
                if let LayoutLeft::NotKept(cause) = &left {
                    self.say_cold_lane_tabs_not_kept(target_build, cause);
                }
                self.prepare_cold_exec(build, same_image.is_some(), mode.is_automatic());
                let error = self.run_cold_exec(|| {
                    let _lifeline = cold_exec_hand_over();
                    let _older_image =
                        crate::descriptor_limit::lower_for_older_image(target_build, build);
                    aterm_update::exec_keeping_sigpipe(&mut command)
                });
                let why = crate::UpdateHandoffStartError::failed(format!(
                    "process replacement failed: {error}"
                ));
                if left == LayoutLeft::Written {
                    self.take_back_cold_lane_layout(run, std::time::Instant::now());
                }
                self.resume_after_cold_exec();
                return Err(why);
            }
            crate::native_update_admission::AdmissionDecision::Apply(
                crate::native_update_admission::ApplyLane::Seamless,
            ) => {}
        }

        let target_commit = apply_attempt.as_ref().map_or_else(
            || crate::build_info::git_commit().to_string(),
            |attempt| attempt.target_commit().to_string(),
        );
        if let Err(refusal) = crate::seamless::handoff_commit_stamp(&target_commit) {
            return Err(crate::UpdateHandoffStartError::refused(format!(
                "{refusal}; the terminal was left untouched"
            )));
        }
        if let Err(refusal) =
            self.settle_owed_layout_take_back_before_switch(std::time::Instant::now())
        {
            return Err(crate::UpdateHandoffStartError::refused(format!(
                "{refusal}; the terminal was left untouched"
            )));
        }

        if mode.is_automatic()
            && let Some(attempt) = apply_attempt.as_ref()
        {
            self.answer_with_refused_cold_relaunch(attempt);
        }
        let preverified = apply_attempt
            .as_ref()
            .and_then(|attempt| self.cached_handoff_preverification(attempt));
        if preverified == Some(false) && mode.is_automatic() {
            let reason = apply_attempt
                .as_ref()
                .and_then(|attempt| self.cached_handoff_preverification_reason(attempt))
                .unwrap_or_else(|| "the staged update failed verification".to_string());
            if aterm_update::is_roster_confirmation_wait(&reason)
                && let Some(attempt) = apply_attempt.as_ref()
            {
                if let Some(dmg_sha256) =
                    crate::app_native::decode_dmg_sha256(attempt.target_dmg_sha256())
                {
                    let _ = self.hold_for_roster_confirmation(
                        attempt.target_build(),
                        dmg_sha256,
                        &reason,
                        std::time::Instant::now(),
                        true,
                        true,
                    );
                }
                return Err(crate::UpdateHandoffStartError::activity(format!(
                    "{reason}; the terminal was left untouched"
                )));
            }
            return Err(crate::UpdateHandoffStartError::failed(format!(
                "{reason}; the terminal was left untouched"
            )));
        }
        let verify_staged_candidate = same_image.is_none() && preverified != Some(true);

        let Some(proxy) = self.proxy.clone() else {
            return Err(crate::UpdateHandoffStartError::failed(
                "overlap handoff has no event-loop completion channel",
            ));
        };
        let Some(reconcile_ticket) = self.mint_native_update_reconcile_ticket() else {
            return Err(crate::UpdateHandoffStartError::failed(
                "updater reconciliation identity space is exhausted",
            ));
        };
        let Some(reconcile_worker) = self.native_update_reconcile_worker.clone() else {
            return Err(crate::UpdateHandoffStartError::failed(
                "updater reconciliation worker is unavailable",
            ));
        };
        crate::descriptor_limit::raise_descriptor_limit_for_park(target_build, build);
        for (_, master, _) in &live {
            if aterm_pty::set_cloexec(*master, true).is_err() {
                return Err(crate::UpdateHandoffStartError::failed(
                    "could not enforce CLOEXEC on every parent PTY master",
                ));
            }
        }

        if !renewal {
            self.begin_update_installing(matches!(
                mode,
                crate::native_updater_service::ApplyMode::Immediate
            ));
        }
        self.record_update_switch_started(build, same_image.is_some(), mode.is_automatic());

        #[cfg(target_os = "macos")]
        let rendezvous_path_fits = crate::handoff_rendezvous::rendezvous_path_fits();
        #[cfg(target_os = "macos")]
        let bundle = app_bundle_root(&exe);

        let (cancel, cancelled) = std::sync::mpsc::sync_channel(1);
        let (job_tx, job_rx) = std::sync::mpsc::sync_channel::<HandoffWorkerJob>(1);
        let worker_proxy = proxy.clone();
        if let Err(error) = std::thread::Builder::new()
            .name("aterm-update-handoff".to_string())
            .spawn(move || {
                if let Ok(job) = job_rx.recv() {
                    run_handoff_worker(job, worker_proxy);
                }
            })
        {
            return Err(crate::UpdateHandoffStartError::failed(format!(
                "overlap handoff worker could not start: {error}"
            )));
        }

        let attempt_id = self.next_update_handoff_id;
        let Some(next_attempt_id) = attempt_id.checked_add(1) else {
            return Err(crate::UpdateHandoffStartError::failed(
                "handoff identity space is exhausted",
            ));
        };
        self.next_update_handoff_id = next_attempt_id;
        #[expect(
            clippy::disallowed_types,
            reason = "the update successor's raw Command: it hands over exactly its own descriptors, which a strip at the exec would undo"
        )]
        let mut command = std::process::Command::new(exe);
        crate::control_socket_identity::bind_command(&mut command);
        command
            .args(aterm_update::running_window_successor_args(
                std::env::args_os().skip(1),
                None,
            ))
            .env("ATERM_UPDATED_FROM", build.to_string())
            .env(aterm_update::RELAUNCH_OF_RUNNING_ENV, "1");
        #[cfg(target_os = "macos")]
        if renewal {
            command.env(
                crate::launch_record_renewal::RENEWED_FROM_ENV,
                std::process::id().to_string(),
            );
        }
        let installed_activation = apply_attempt
            .as_ref()
            .is_some_and(|attempt| attempt.is_installed_activation());
        bind_expected_update_artifact(
            &mut command,
            apply_attempt.as_ref().filter(|_| !installed_activation),
        );
        #[cfg(target_os = "macos")]
        let lane_grant_chunks = self.park_policy(apply_attempt.as_ref()).grant_chunks || renewal;
        #[cfg(target_os = "macos")]
        let lane = {
            let facts = HandoffLaneFacts {
                bundled: bundle.is_some(),
                launcher_available: true,
                socket_path_fits: rendezvous_path_fits,
                target_not_older: target_build >= build,
                sessions: live.len(),
                grant_chunks: lane_grant_chunks,
                environment_is_a_merge: launch_environment(&command).is_some(),
            };
            let never_forks = |why: &str| {
                crate::UpdateHandoffStartError::refused(format!(
                    "{why}; a launch-record renewal never forks: a forked successor keeps \
                     this process's launchd job, which ends with it"
                ))
            };
            match out_of_band_lane_refusal(facts) {
                None => {
                    if crate::handoff_rendezvous::proof_identities_in_device_terms(&live).is_some()
                    {
                        HandoffLane::OutOfBand
                    } else {
                        let why = "a handed-off PTY would not answer fstat, so the out-of-band \
                                   proof term cannot be computed";
                        if renewal {
                            return Err(never_forks(why));
                        }
                        aterm_log::warn!("update apply: forking instead of launching — {why}");
                        HandoffLane::Fork
                    }
                }
                Some(reason) if renewal => return Err(never_forks(reason)),
                Some(reason) => {
                    aterm_log::info!("update apply: forking instead of launching — {reason}");
                    HandoffLane::Fork
                }
            }
        };
        if self.update_handoff_activity_epoch == u64::MAX {
            return Err(crate::UpdateHandoffStartError::failed(
                "handoff activity identity space is exhausted",
            ));
        }

        #[cfg(target_os = "macos")]
        if lane == HandoffLane::OutOfBand {
            return self.prelaunch_out_of_band_handoff(PrelaunchArgs {
                attempt_id,
                build,
                mode,
                apply_attempt,
                same_image,
                target_build,
                target_commit,
                command,
                verify_staged_candidate,
                grant_chunks: lane_grant_chunks,
                installed_activation,
                bundle,
                cancel,
                cancelled,
                job_tx,
                reconcile_worker,
                reconcile_ticket,
            });
        }

        let freeze_seed = FreezeSeed::from_prior(aterm_update::handoff_capture_prior().as_ref());

        let hold_serials = self.hold_serials();
        let manifest = match self.store.try_read() {
            Ok(store) => crate::session_store::SessionHandoff::from_store(&store),
            Err(std::sync::TryLockError::Poisoned(poison)) => {
                let store = poison.into_inner();
                crate::session_store::SessionHandoff::from_store(&store)
            }
            Err(std::sync::TryLockError::WouldBlock) => {
                return Err(crate::UpdateHandoffStartError::failed(
                    "session registry was busy; update handoff stayed in place",
                ));
            }
        };
        let manifest = handed_sessions_only(manifest, &live);
        let carried_claims = carried_claims_of(&manifest);
        let Ok((owned_masters, adoption)) = reserve_child_only_masters(&live) else {
            return Err(crate::UpdateHandoffStartError::failed(
                "could not reserve child-only PTY descriptors",
            ));
        };
        let fds = crate::session_store::HandoffFds {
            entries: adoption.clone(),
        };
        let window = self.park_window_carry(target_build);
        let proof_identities = adoption.clone();

        if automatic_activity_epoch.is_some_and(|epoch| {
            self.update_handoff_activity_epoch != epoch
                || !self.automatic_update_activity_quiet(std::time::Instant::now())
                || handoff_masters_have_activity(&live)
        }) {
            return Err(crate::UpdateHandoffStartError::activity(
                "input or PTY output arrived before automatic reader park",
            ));
        }
        if mode.is_automatic() && handoff_masters_closed(&live) {
            return Err(crate::UpdateHandoffStartError::activity(
                "a session's command has exited but its pane is still open",
            ));
        }
        if let Some(reason) = recording_park_refusal(
            mode,
            self.automatic_phase_for(mode, std::time::Instant::now()),
            self.video_request_live(),
            std::time::Duration::ZERO,
        ) {
            return Err(crate::UpdateHandoffStartError::activity(reason));
        }
        let fork_park_misses = self.fork_park_misses_of(target_build);
        let park_policy = self.park_policy(apply_attempt.as_ref());
        let ceiling = park_policy.carry_ceiling();
        if let Some(policy) = park_policy.policy {
            aterm_log::info!(
                "update apply: parking under build {target_build}'s handoff policy ({policy})"
            );
        }
        let policy_unread = apply_attempt.is_some() && same_image.is_none() && !park_policy.known;
        if let Some(wait) = self.fork_lane_policy_wait(mode, target_build, policy_unread) {
            return Err(wait);
        }
        if policy_unread {
            aterm_log::info!(
                "update apply: build {target_build} has no verified pass on record, so its \
                 handoff policy is read after this park; a capture it forbids is not handed \
                 over"
            );
        }
        let prior_physical_failures = self
            .prior_physical_failures_of(apply_attempt.as_ref())
            .saturating_add(fork_park_misses);
        let freeze_budget = handoff_freeze_budget(mode, prior_physical_failures, freeze_seed);
        let freeze_ms = freeze_budget.as_millis();
        let handoff_history_comfort = freeze_budget / 2;
        aterm_log::info!(
            "update apply: freeze budget {freeze_ms} ms ({mode:?}, {prior_physical_failures} prior \
             physical failure(s) of the target artifact incl. {fork_park_misses} fork-lane park \
             miss(es); {freeze_seed}; running build {build})"
        );
        self.charge_upgrade_rim_at_park(build);
        let park_at = std::time::Instant::now();
        let deadline = park_at + freeze_budget;
        if !self.park_all_readers(deadline) {
            self.rollback_overlap(None, &live);
            return Err(self.fork_park_missed(
                target_build,
                format!("a PTY reader missed the {freeze_ms} ms handoff park deadline"),
            ));
        }
        if mode.is_automatic() && handoff_masters_closed(&live) {
            self.rollback_overlap(None, &live);
            return Err(crate::UpdateHandoffStartError::activity(
                "a PTY session closed during automatic reader park",
            ));
        }
        let ParkedScreens {
            screens,
            carries,
            screen_digest,
            repaint,
            fg_holders,
            render_debts,
            history_heads,
            held,
        } = match self.capture_parked_screens(
            &live,
            deadline,
            freeze_ms,
            handoff_history_comfort,
            crate::seamless::WireCaps::for_target(target_build),
            ceiling,
        ) {
            Ok(captured) => captured,
            Err(failure) => {
                self.rollback_overlap(None, &live);
                if policy_unread {
                    self.spawn_staged_handoff_preverification(target_build);
                }
                return Err(match classify_capture_failure(&failure) {
                    ParkAttempt::Refused(refusal) => {
                        crate::UpdateHandoffStartError::capture_refused(refusal.to_string())
                    }
                    ParkAttempt::Missed(reason) => self.fork_park_missed(target_build, reason),
                    #[cfg(any(target_os = "macos", all(test, unix)))]
                    ParkAttempt::Parked
                    | ParkAttempt::NotYet(_)
                    | ParkAttempt::Pausing
                    | ParkAttempt::MissedMidSequence(_)
                    | ParkAttempt::Failed(_) => {
                        self.fork_park_missed(target_build, failure.to_string())
                    }
                });
            }
        };
        let manifest = stamp_fg_holders(manifest, &fg_holders);
        let manifest = stamp_render_debts(manifest, &render_debts);
        let manifest = stamp_journal_keys(manifest, &self.journal_keys());
        let manifest = stamp_status_seen(manifest, &self.status_carry_rows());
        let manifest = stamp_autopilot_end(manifest, &self.end_carry_rows());
        let manifest = stamp_fleet_told(manifest, self.fleet_told_rows());

        let layout = self.capture_handoff_layout();

        let layout_digest = match commit_handoff_layout(&layout) {
            Ok(digest) => digest,
            Err(refusal) => {
                self.rollback_overlap(None, &live);
                return Err(crate::UpdateHandoffStartError::capture_refused(
                    refusal.to_string(),
                ));
            }
        };
        if std::time::Instant::now() >= deadline {
            self.rollback_overlap(None, &live);
            return Err(self.fork_park_missed(
                target_build,
                format!("handoff proof capture exceeded the {freeze_ms} ms deadline"),
            ));
        }
        let _ledger_write = keep_landed_park(park_at.elapsed());
        let preverified =
            PreverifyPublisher::for_attempt(&self.handoff_preverified, apply_attempt.as_ref());
        let activity_epoch = self.update_handoff_activity_epoch;
        let arbiter = crate::HandoffAttemptArbiter::new();
        self.pending_update_handoff = Some(crate::PendingUpdateHandoff {
            attempt_id,
            park_at,
            proof_ready_at: None,
            nonce: None,
            live: live.clone(),
            adoption: proof_identities.clone(),
            child_pid: None,
            mode,
            apply_attempt,
            same_image,
            target_build,
            target_commit: target_commit.clone(),
            layout: layout.clone(),
            layout_digest,
            screen_digest,
            activity_epoch,
            hold_serials,
            carried_claims,
            cancel: cancel.clone(),
            arbiter: arbiter.clone(),
            teardown: crate::DeferredHandoffTeardown::None,
            commit_drain_started: None,
            commit_flush: None,
            held_commit: None,
            revoked_by_activity: false,
        });
        let cleanup = HandoffWorkerCleanup {
            parent_socket: self.handoff_parent_socket(),
            reconcile: Some((reconcile_worker, reconcile_ticket)),
        };
        let job = HandoffWorkerJob {
            attempt_id,
            current_build: build,
            target_build,
            target_commit,
            verify_staged_candidate,
            installed_activation,
            preverified,
            parked_under: Some(ceiling),
            successor_grant_chunks: false,
            command,
            capture: Some(HandoffCapture {
                #[cfg(target_os = "macos")]
                park_at,
                #[cfg(target_os = "macos")]
                captured_at: std::time::Instant::now(),
                manifest,
                fds,
                screens,
                repaint,
                carries,
                window,
                layout,
                layout_digest,
                screen_digest,
                live: adoption,
                proof_identities,
                _owned_masters: owned_masters,
                history_heads,
                history: crate::handoff_history::HistoryPlan::deferred_under(ceiling, || {
                    self.history_sessions(target_build)
                }),
                held,
            }),
            #[cfg(target_os = "macos")]
            prelaunch: None,
            #[cfg(target_os = "macos")]
            lane,
            #[cfg(target_os = "macos")]
            bundle,
            #[cfg(target_os = "macos")]
            expected: None,
            #[cfg(target_os = "macos")]
            renews_launch_record: renewal,
            cleanup,
            cancel: cancelled,
            arbiter,
        };
        if job_tx.send(job).is_err() {
            self.pending_update_handoff = None;
            let live: Vec<_> = self
                .pool
                .iter()
                .map(|session| (session.id, session.master, session.pid))
                .collect();
            self.rollback_overlap(None, &live);
            return Err(crate::UpdateHandoffStartError::failed(
                "overlap handoff worker stopped before preparation",
            ));
        }
        Ok(())
    }

    /// The sessions an overlap handoff hands over, as `(local_id, master, pid)`.
    #[cfg(unix)]
    pub(crate) fn handoff_live_sessions(&self) -> Vec<(u64, i32, i32)> {
        let exited = self.handoff_exited_session_ids();
        self.pool
            .iter()
            .filter(|session| !exited.contains(&session.id))
            .map(|session| (session.id, session.master, session.pid))
            .collect()
    }

    #[cfg(unix)]
    fn handoff_window_carry(&self, target_build: u64) -> Option<crate::session_store::WindowCarry> {
        let carried = self.carried_messages();
        let zoom = self.handoff_font_zoom();
        self.windows.values().next().map(|state| {
            let position = state
                .os_window
                .as_ref()
                .and_then(|window| window.outer_position().ok());
            crate::session_store::WindowCarry {
                rows: state.rows,
                cols: state.cols,
                outer_x: position.map(|point| point.x),
                outer_y: position.map(|point| point.y),
                inner_px: state
                    .win_px
                    .filter(|size| size.width > 0 && size.height > 0)
                    .map(|size| [size.width, size.height]),
                presence_row: false,
                status_bar_rows: self.message_band_rows,
                messages: carried.messages,
                next_message_id: carried.next_message_id,
                update_verified_unix_ms: crate::update_words::verified_unix_ms(
                    self.update_verified,
                    target_build,
                    std::time::Instant::now(),
                    std::time::SystemTime::now(),
                ),
                font_px_milli: zoom.device.map(|(px, _)| px),
                font_reset_px_milli: zoom.device.map(|(_, reset)| reset),
                font_size_milli: zoom.size.map(|(size, _)| size),
                font_reset_size_milli: zoom.size.map(|(_, reset)| reset),
                update_health_said: carried.update_health_said,
                messages_captured_unix_ms: carried.captured_unix_ms,
            }
        })
    }

    #[cfg(unix)]
    fn park_window_carry(
        &mut self,
        target_build: u64,
    ) -> Option<crate::session_store::WindowCarry> {
        let window = self.handoff_window_carry(target_build);
        self.handoff_carried_messages = window
            .as_ref()
            .map(|window| window.messages.iter().map(|message| message.id).collect())
            .unwrap_or_default();
        window
    }

    /// The advisory warm hint a launched successor starts with.
    #[cfg(target_os = "macos")]
    pub(crate) fn handoff_warm_hint(&self) -> crate::handoff_warm_hint::WarmHint {
        let zoom = self.handoff_font_zoom();
        let (cols, rows) = self
            .windows
            .values()
            .next()
            .map_or((0, 0), |state| (state.cols, state.rows));
        crate::handoff_warm_hint::WarmHint {
            windows: u32::try_from(self.windows.len()).unwrap_or(u32::MAX),
            sessions: u32::try_from(self.pool.iter().count()).unwrap_or(u32::MAX),
            cols,
            rows,
            zoom: zoom.device,
            size_zoom: zoom.size,
        }
    }

    /// The live font zoom as the window carry writes it, in thousandths, only while zoomed.
    #[cfg(unix)]
    pub(crate) fn handoff_font_zoom(&self) -> crate::app_config::CarriedZoom {
        use crate::app_config::font_px_milli;
        if (self.font_size - self.default_font_size).abs() < 0.5 {
            return crate::app_config::CarriedZoom::default();
        }
        let scale = self.front_scale();
        let pair = |a: f32, b: f32| font_px_milli(a).zip(font_px_milli(b));
        crate::app_config::CarriedZoom {
            size: pair(self.font_size, self.default_font_size),
            device: pair(
                crate::font_px_for_scale(self.font_size, scale),
                crate::font_px_for_scale(self.default_font_size, scale),
            ),
        }
    }

    /// The pooled sessions whose command has exited, which [`Self::handoff_live_sessions`] leaves out.
    #[cfg(unix)]
    pub(crate) fn handoff_exited_session_ids(&self) -> Vec<u64> {
        let store = self
            .store
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.pool
            .iter()
            .map(|session| session.id)
            .filter(|id| {
                store.by_local(*id).is_some_and(|handle| {
                    handle.state == crate::session_store::SessionState::Exited
                })
            })
            .collect()
    }

    #[cfg(target_os = "macos")]
    fn start_history_export(
        &self,
        target_build: u64,
        ceiling: aterm_update_core::handoff_policy::CarryCeiling,
    ) -> Option<crate::handoff_history::HistoryExporter> {
        if !ceiling.carries_scrollback() {
            aterm_log::info!(
                "update apply: build {target_build}'s handoff policy (carry={}) carries no \
                 scrollback; no scrollback export is started",
                ceiling.as_str()
            );
            return None;
        }
        let sessions = self.history_sessions(target_build)?;
        let dir = crate::control_auth::socket_dir()?;
        match crate::handoff_history::HistoryExporter::start(dir, sessions, true) {
            Ok(export) => Some(export),
            Err(error) => {
                aterm_log::warn!(
                    "update apply: the scrollback export could not start ({error}); each tab \
                     carries the screen carry's bounded history"
                );
                None
            }
        }
    }

    #[cfg(any(target_os = "macos", all(test, unix)))]
    fn withhold_history_export(
        &mut self,
        target_build: u64,
        ceiling: aterm_update_core::handoff_policy::CarryCeiling,
    ) {
        let Some(mut export) = self
            .update_handoff_prelaunch
            .as_mut()
            .and_then(|prelaunch| prelaunch.history_export.take())
        else {
            return;
        };
        let stopped = export.halt(crate::handoff_history::HALT_WAIT);
        drop(export);
        aterm_log::info!(
            "update apply: build {target_build}'s handoff policy (carry={}) carries no \
             scrollback; the scrollback export started beside the launch {}, and every file it \
             wrote is removed as it stops",
            ceiling.as_str(),
            if stopped {
                "has stopped"
            } else {
                "was asked to stop (it had not answered)"
            }
        );
    }

    #[cfg(unix)]
    fn history_sessions(
        &self,
        target_build: u64,
    ) -> Option<crate::handoff_history::ExportSessions> {
        let running = crate::running_build_number();
        if !crate::handoff_history::exports_for_target(running, target_build) {
            aterm_log::info!(
                "update apply: the successor (build {target_build}) is older than this build and \
                 imports no scrollback sidecar; each tab carries the screen carry's bounded \
                 history"
            );
            return None;
        }
        if crate::control_auth::socket_dir().is_none() {
            aterm_log::warn!(
                "update apply: no private control dir for the scrollback export; each tab \
                 carries the screen carry's bounded history"
            );
            return None;
        }
        let live = self.handoff_live_sessions();
        let mut sessions = crate::handoff_history::ExportSessions::default();
        let held = crate::handoff_history::exports_held_for_target(running, target_build);
        for session in self.pool.iter() {
            let entry = (session.id, std::sync::Arc::clone(&session.term));
            if live.iter().any(|(id, _, _)| *id == session.id) {
                sessions.handed.push(entry);
            } else if held {
                sessions.held.push(entry);
            }
        }
        sessions.handed.sort_unstable_by_key(|(id, _)| *id);
        sessions.held.sort_unstable_by_key(|(id, _)| *id);
        Some(sessions)
    }

    /// The successor's half of the history carry, at Commit.
    #[cfg(unix)]
    pub(crate) fn start_handoff_history_imports(&mut self) {
        let jobs = self.pool.take_handoff_histories();
        if jobs.is_empty() {
            return;
        }
        let not_run: Vec<crate::handoff_history::ImportReport> = jobs
            .iter()
            .map(|job| {
                crate::handoff_history::ImportReport::not_run(
                    job,
                    "the import worker could not start",
                )
            })
            .collect();
        let journals: Vec<_> = jobs
            .iter()
            .filter_map(|job| {
                self.pool
                    .iter()
                    .find(|session| session.id == job.session)
                    .and_then(|session| session.ctx.crash_journal.get().cloned())
            })
            .collect();
        let held = journals.clone();
        let store = std::sync::Arc::clone(&self.store);
        let proxy = self.proxy.clone();
        let spawned = std::thread::Builder::new()
            .name("aterm-history-import".to_string())
            .spawn(move || {
                crate::qos::set_self(crate::qos::Role::Responsive);
                let reports = crate::handoff_history::run_imports(jobs);
                for journal in &journals {
                    journal.import_settled();
                }
                crate::handoff_history::record_reports(&store, &reports);
                if let Some(proxy) = proxy {
                    let _ = proxy.send_event(Wake::HandoffHistorySettled(reports));
                }
            });
        if let Err(error) = spawned {
            aterm_log::warn!("overlap handoff: the scrollback import could not start ({error})");
            for journal in &held {
                journal.import_settled();
            }
            crate::handoff_history::record_reports(&self.store, &not_run);
            self.settle_handoff_history(&not_run);
        }
    }

    /// A keeper recovery's half of the history carry, once its restore has drained.
    #[cfg(unix)]
    pub(crate) fn start_keeper_history_imports(&mut self) {
        self.handoff_history_loss_said = crate::handoff_history::carried_loss(
            self.pool
                .iter()
                .filter_map(|session| session.handoff_history.as_ref()),
        );
        self.start_handoff_history_imports();
    }

    /// Says what the history imports left behind once they settle, in one band row, posting only a change from [`Self::say_carried_history_loss`].
    #[cfg(unix)]
    pub(crate) fn settle_handoff_history(
        &mut self,
        reports: &[crate::handoff_history::ImportReport],
    ) {
        let loss = crate::handoff_history::loss_summary(reports);
        let said = std::mem::take(&mut self.handoff_history_loss_said);
        if loss.lines > 0 && loss != said {
            self.post_message(crate::update_words::scrollback_left_behind(
                loss.lines,
                loss.tabs,
                loss.withheld,
                loss.held,
                loss.held_failed,
            ));
        }
    }

    /// Posts the history loss the parent counted, inside the Commit's re-grid batch, for [`Self::settle_handoff_history`] to compare against.
    #[cfg(unix)]
    pub(crate) fn say_carried_history_loss(&mut self) {
        let loss = crate::handoff_history::carried_loss(
            self.pool
                .iter()
                .filter_map(|session| session.handoff_history.as_ref()),
        );
        if loss.lines > 0 {
            self.post_message(crate::update_words::scrollback_left_behind(
                loss.lines,
                loss.tabs,
                loss.withheld,
                loss.held,
                loss.held_failed,
            ));
        }
        self.handoff_history_loss_said = loss;
    }

    #[cfg(unix)]
    fn soft_miss_may_repark(&self) -> bool {
        #[cfg(any(target_os = "macos", all(test, unix)))]
        {
            self.update_handoff_prelaunch
                .as_ref()
                .is_some_and(|prelaunch| {
                    prelaunch.park_mid_sequence_reparks < PRELAUNCH_MAX_MID_SEQUENCE_REPARKS
                })
        }
        #[cfg(not(any(target_os = "macos", all(test, unix))))]
        {
            false
        }
    }

    #[cfg(unix)]
    fn capture_parked_screens(
        &mut self,
        live: &[(u64, i32, i32)],
        deadline: std::time::Instant,
        freeze_ms: u128,
        handoff_history_comfort: std::time::Duration,
        caps: crate::seamless::WireCaps,
        ceiling: aterm_update_core::handoff_policy::CarryCeiling,
    ) -> Result<ParkedScreens, CaptureFailure> {
        use crate::seamless::{CarryRung, WireCarry};

        let soft_miss_may_repark = self.soft_miss_may_repark();
        let mut wire: Vec<WireCarry> = Vec::new();
        let mut carries = Vec::new();
        let carry_room = carries.try_reserve_exact(live.len()).is_ok();
        let mut fg_holders = Vec::new();
        let holder_room = fg_holders.try_reserve_exact(live.len()).is_ok();
        let mut render_debts = Vec::new();
        let debt_room = render_debts.try_reserve_exact(live.len()).is_ok();
        let mut history_heads = Vec::new();
        if wire.try_reserve_exact(live.len()).is_err()
            || history_heads.try_reserve_exact(live.len()).is_err()
        {
            return Err(CaptureFailure::Storage);
        }
        let mut capture_failed = None;
        let mut capture_budget = 0_u64;
        let mut capture_cells = 0_u64;
        let mut history_latched_off = false;
        let mut cells_reserve = 0_u64;
        let mut bytes_reserve = 0_u64;
        let handed = |id: u64| live.iter().any(|(local_id, _, _)| *local_id == id);
        let mut sessions = Vec::new();
        if sessions.try_reserve_exact(live.len()).is_err() {
            return Err(CaptureFailure::Storage);
        }
        sessions.extend(self.pool.iter().filter(|session| handed(session.id)));
        sessions.sort_unstable_by_key(|session| session.id);
        for session in sessions.iter().copied() {
            let terminal = match try_lock_by(&session.term, deadline) {
                Some(guard) => guard,
                None => {
                    cells_reserve = u64::MAX;
                    bytes_reserve = u64::MAX;
                    break;
                }
            };
            let (rows, cols) = (terminal.rows(), terminal.cols());
            let mandatory_bytes =
                crate::seamless::checkpoint_capture_budget_bytes(rows, cols, 0).unwrap_or(u64::MAX);
            cells_reserve = cells_reserve
                .saturating_add(crate::seamless::mandatory_checkpoint_cells(rows, cols));
            bytes_reserve = bytes_reserve.saturating_add(mandatory_bytes);
        }
        for session in sessions.iter().copied() {
            if std::time::Instant::now() >= deadline {
                capture_failed = Some(CaptureFailure::Deadline { freeze_ms });
                break;
            }
            let Some(terminal) = try_lock_by(&session.term, deadline) else {
                capture_failed = Some(CaptureFailure::EngineBusy);
                break;
            };
            let partial = if caps.carries_parser() {
                terminal.partial_sequence_uncarried().or_else(|| {
                    terminal.partial_sequence_swallowed().filter(|_| {
                        soft_miss_may_repark || !terminal.partial_sequence_swallows_picture()
                    })
                })
            } else {
                terminal.partial_sequence_state()
            }
            .or_else(|| {
                (!caps.carries_output_state())
                    .then(|| terminal.output_state_in_flight())
                    .flatten()
                    .filter(|state| soft_miss_may_repark || *state != OPEN_LINK_STATE)
            });
            if let Some(state) = partial
                && live
                    .iter()
                    .find(|(local_id, _, _)| *local_id == session.id)
                    .is_some_and(|entry| handoff_masters_have_activity(std::slice::from_ref(entry)))
            {
                capture_failed = Some(CaptureFailure::MidSequence {
                    local_id: session.id,
                    state,
                });
                break;
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let (rows, cols) = (terminal.rows(), terminal.cols());
            let own_cells = crate::seamless::mandatory_checkpoint_cells(rows, cols);
            let own_bytes =
                crate::seamless::checkpoint_capture_budget_bytes(rows, cols, 0).unwrap_or(u64::MAX);
            cells_reserve = cells_reserve.saturating_sub(own_cells);
            bytes_reserve = bytes_reserve.saturating_sub(own_bytes);
            let carry = if ceiling.carries_scrollback() {
                crate::seamless::max_handoff_history_lines()
            } else {
                0
            };
            let history_cells = u64::from(cols).saturating_mul(u64::from(carry));
            let history_bytes = crate::seamless::checkpoint_capture_budget_bytes(rows, cols, carry)
                .map_or(u64::MAX, |carried| carried.saturating_sub(own_bytes));
            let history_fits_cells = optional_carry_fits(
                capture_cells,
                own_cells,
                history_cells,
                cells_reserve,
                crate::seamless::max_handoff_aggregate_grid_cells(),
            );
            let history_fits_bytes = optional_carry_fits(
                capture_budget,
                own_bytes,
                history_bytes,
                bytes_reserve,
                MAX_HANDOFF_CAPTURE_BUDGET_BYTES,
            );
            let history = if remaining >= handoff_history_comfort
                && !history_latched_off
                && history_fits_cells
                && history_fits_bytes
            {
                carry
            } else {
                0
            };
            let visible_authority = crate::seamless::checkpoint_capture_budget_bytes(rows, cols, 0)
                .and_then(|bytes| capture_budget.checked_add(bytes))
                .filter(|total| *total <= MAX_HANDOFF_CAPTURE_BUDGET_BYTES);
            let (checkpoint, rung, _cause) = if visible_authority.is_some() {
                crate::seamless::carry_for_wire_within(
                    &terminal,
                    session.id,
                    history,
                    &mut capture_cells,
                    caps,
                    ceiling,
                )
            } else {
                crate::seamless::repaint_carry_for_wire(
                    &terminal,
                    session.id,
                    &mut capture_cells,
                    caps,
                    format!(
                        "{rows}x{cols} would take the pool past the \
                         {MAX_HANDOFF_CAPTURE_BUDGET_BYTES}-byte decode-authority budget"
                    ),
                )
            };
            if history != 0 && rung == CarryRung::VisibleOnly {
                history_latched_off = true;
            }
            capture_budget = crate::seamless::checkpoint_capture_budget_bytes(
                checkpoint.rows,
                checkpoint.cols,
                checkpoint.history_lines,
            )
            .map_or(u64::MAX, |bytes| capture_budget.saturating_add(bytes));
            let differ = deadline.saturating_duration_since(std::time::Instant::now())
                >= handoff_history_comfort;
            if holder_room {
                fg_holders.push((
                    session.id,
                    session.fg_holder.load(std::sync::atomic::Ordering::Acquire),
                ));
            }
            if debt_room && rung != CarryRung::Repaint && terminal.modes().alternate_screen {
                let debt = terminal.render_debt();
                if debt.owed() {
                    render_debts.push((session.id, debt));
                }
            }
            history_heads.push(crate::handoff_history::capture_head(session.id, &terminal));
            if carry_room {
                let mut source = crate::handoff_carry::capture_head(
                    session.id,
                    &terminal,
                    &session.term,
                    &session.ctx.turns,
                    Some(&session.ctx.timeline),
                    differ && rung.keeps_differ_state(),
                );
                if !rung.keeps_differ_state() {
                    source.without_differ();
                }
                carries.push(source);
            }
            wire.push(WireCarry {
                local_id: session.id,
                checkpoint,
                rung,
            });
            if std::time::Instant::now() >= deadline {
                capture_failed = Some(CaptureFailure::Deadline { freeze_ms });
                break;
            }
        }
        if capture_failed.is_none() && std::time::Instant::now() >= deadline {
            capture_failed = Some(CaptureFailure::Deadline { freeze_ms });
        }
        if let Some(failure) = capture_failed {
            return Err(failure);
        }
        let screen_digest =
            crate::seamless::settle_wire_carries(&mut wire, caps).map_err(|refusal| {
                CaptureFailure::Refused {
                    local_id: refusal.local_id(),
                    cause: format!(
                        "visible checkpoint set could not be committed canonically: {refusal}"
                    ),
                }
            })?;
        carries.retain_mut(|source: &mut crate::handoff_carry::CarrySource| {
            let Some(carry) = wire
                .iter()
                .find(|carry| carry.local_id == source.local_id())
            else {
                return false;
            };
            if !carry.rung.keeps_differ_state() {
                source.without_differ();
            }
            true
        });
        let mut screens = Vec::new();
        let mut repaint = Vec::new();
        if screens.try_reserve_exact(wire.len()).is_err()
            || repaint.try_reserve_exact(wire.len()).is_err()
        {
            return Err(CaptureFailure::Storage);
        }
        for carry in wire {
            if carry.rung.needs_repaint() {
                repaint.push(carry.local_id);
            }
            screens.push((carry.local_id, carry.checkpoint));
        }
        let held = match held_capture_deadline(
            deadline,
            std::time::Instant::now(),
            handoff_history_comfort,
        ) {
            Some(held_deadline) => self.capture_held_screens(
                live,
                held_deadline,
                &mut capture_cells,
                caps,
                ceiling,
                &mut carries,
            ),
            None => Vec::new(),
        };
        Ok(ParkedScreens {
            screens,
            carries,
            screen_digest,
            repaint,
            fg_holders,
            render_debts,
            history_heads,
            held,
        })
    }

    #[cfg(unix)]
    fn capture_held_screens(
        &self,
        live: &[(u64, i32, i32)],
        deadline: std::time::Instant,
        capture_cells: &mut u64,
        caps: crate::seamless::WireCaps,
        ceiling: aterm_update_core::handoff_policy::CarryCeiling,
        carries: &mut Vec<crate::handoff_carry::CarrySource>,
    ) -> Vec<crate::seamless::HeldScreen> {
        let mut held: Vec<_> = self
            .pool
            .iter()
            .filter(|session| !live.iter().any(|(id, _, _)| *id == session.id))
            .collect();
        held.sort_unstable_by_key(|session| session.id);
        let mut screens = Vec::new();
        let mut held_bytes = 0_u64;
        for session in held {
            if std::time::Instant::now() >= deadline
                || screens.len() >= crate::seamless::MAX_HELD_PANES
            {
                break;
            }
            let Ok(terminal) = session.term.try_lock() else {
                continue;
            };
            let mut cells = *capture_cells;
            let (checkpoint, rung, _cause) = crate::seamless::carry_for_wire_within(
                &terminal, session.id, 0, &mut cells, caps, ceiling,
            );
            let history_dropped =
                u64::try_from(terminal.main_grid().scrollback_lines()).unwrap_or(u64::MAX);
            let history_head = Some(terminal.history_fence());
            let mut control = crate::handoff_carry::capture_head(
                session.id,
                &terminal,
                &session.term,
                &session.ctx.turns,
                None,
                false,
            );
            control.without_differ();
            drop(terminal);
            if rung.needs_repaint() || screens.try_reserve(1).is_err() {
                continue;
            }
            if let Err(why) = crate::seamless::held_screen_meta(&checkpoint) {
                aterm_log::warn!(
                    "update handoff: held pane {} is not carried: {why}",
                    session.id
                );
                continue;
            }
            let screen = crate::seamless::HeldScreen {
                local_id: session.id,
                sid: session.ctx.self_id.clone(),
                checkpoint,
                exit: self.exit_status(session.id, crate::app_tabs::ExitLook::Retry),
                history_dropped,
                history_head,
                failure_seen: self.session_status.failure_seen(session.id),
            };
            let bytes = screen.grid_bytes();
            if held_bytes.saturating_add(bytes) > crate::seamless::MAX_HELD_AGGREGATE_GRID_BYTES {
                continue;
            }
            held_bytes = held_bytes.saturating_add(bytes);
            *capture_cells = cells;
            screens.push(screen);
            if carries.try_reserve(1).is_ok() {
                carries.push(control);
            }
        }
        screens
    }

    /// The dry run.
    #[cfg(any(target_os = "macos", all(test, unix)))]
    pub(crate) fn dry_run_handoff_capture(
        &mut self,
        caps: crate::seamless::WireCaps,
    ) -> DryRunCapture {
        let live = self.handoff_live_sessions();
        if live.is_empty() {
            return DryRunCapture::Unmeasured("no live session to capture");
        }
        let Some(ceiling) = self.dry_run_ceiling() else {
            return DryRunCapture::Unmeasured(
                "the successor's handoff policy has not been read yet",
            );
        };
        let started = std::time::Instant::now();
        let captured = self.capture_parked_screens(
            &live,
            started + DRY_RUN_CAPTURE_WINDOW,
            DRY_RUN_CAPTURE_WINDOW.as_millis(),
            std::time::Duration::ZERO,
            caps,
            ceiling,
        );
        let took = started.elapsed();
        match captured {
            Ok(parked) => DryRunCapture::Measured {
                took,
                sessions: parked.screens.len(),
            },
            Err(CaptureFailure::Deadline { .. } | CaptureFailure::EngineBusy) => {
                DryRunCapture::AtLeast {
                    took: took.max(DRY_RUN_CAPTURE_WINDOW),
                    sessions: live.len(),
                }
            }
            Err(CaptureFailure::MidSequence { .. }) => {
                DryRunCapture::Unmeasured("a parser was mid-sequence over live output")
            }
            Err(CaptureFailure::Storage) => {
                DryRunCapture::Unmeasured("the capture could not reserve its storage")
            }
            Err(CaptureFailure::Refused { .. }) => {
                DryRunCapture::Unmeasured("the capture refused the desk")
            }
        }
    }

    #[cfg(any(target_os = "macos", all(test, unix)))]
    fn dry_run_ceiling(&self) -> Option<aterm_update_core::handoff_policy::CarryCeiling> {
        let attempt = self
            .update_handoff_prelaunch
            .as_ref()
            .and_then(|prelaunch| prelaunch.apply_attempt.as_ref());
        let policy = self.park_policy(attempt);
        (attempt.is_none() || policy.known).then(|| policy.carry_ceiling())
    }

    /// Seeds this attempt's first automatic freeze rung from the dry run (never below the last landed park).
    #[cfg(any(target_os = "macos", all(test, unix)))]
    pub(crate) fn seed_first_freeze_rung(
        &mut self,
        mode: crate::native_updater_service::ApplyMode,
        caps: crate::seamless::WireCaps,
    ) -> (FreezeSeed, Option<std::thread::JoinHandle<()>>) {
        if !mode.is_automatic() {
            return (FreezeSeed::Default, None);
        }
        let dry = self.dry_run_handoff_capture(caps);
        let prior = aterm_update::handoff_capture_prior();
        let mut kept = None;
        let seed = match dry {
            DryRunCapture::Measured { took, sessions }
            | DryRunCapture::AtLeast { took, sessions } => {
                let took = std::time::Duration::from_micros(
                    u64::try_from(took.as_micros()).unwrap_or(u64::MAX).max(1),
                );
                let seed = FreezeSeed::DryRun {
                    capture: took,
                    sessions,
                    last_park: FreezeSeed::last_park_of(prior.as_ref()),
                };
                let record = aterm_update::HandoffCaptureRecord {
                    capture_us: u64::try_from(took.as_micros()).unwrap_or(u64::MAX),
                    sessions: u32::try_from(sessions).unwrap_or(u32::MAX),
                    freeze_seed_ms: u64::try_from(seed.rung0().as_millis()).unwrap_or(u64::MAX),
                    ..aterm_update::HandoffCaptureRecord::default()
                };
                let build = crate::running_build_number();
                match spawn_ledger_writer("aterm-freeze-seed", move || {
                    aterm_update::record_handoff_capture(build, &record);
                }) {
                    Ok(handle) => kept = Some(handle),
                    Err(error) => aterm_log::warn!(
                        "update apply: the timed capture could not be kept for the next update \
                         ({error}); this attempt still seeds from it"
                    ),
                }
                seed
            }
            DryRunCapture::Unmeasured(why) => {
                aterm_log::info!("update apply: the dry-run capture measured nothing ({why})");
                FreezeSeed::from_prior(prior.as_ref())
            }
        };
        let saturated = if matches!(dry, DryRunCapture::AtLeast { .. }) {
            " — it ran out of its window, so the rung takes its ceiling"
        } else {
            ""
        };
        aterm_log::info!(
            "update apply: first freeze rung {} ms — {seed}{saturated}",
            seed.rung0().as_millis()
        );
        if let Some(prelaunch) = self.update_handoff_prelaunch.as_mut() {
            prelaunch.freeze_seed = seed;
        }
        (seed, kept)
    }

    /// The prelaunched attempt's budget for its next park, as `(prior failures incl. misses, budget, seed)`.
    #[cfg(any(target_os = "macos", all(test, unix)))]
    pub(crate) fn prelaunched_park_budget(&self) -> Option<(u8, std::time::Duration, FreezeSeed)> {
        let prelaunch = self.update_handoff_prelaunch.as_ref()?;
        let prior = self
            .prior_physical_failures_of(prelaunch.apply_attempt.as_ref())
            .saturating_add(prelaunch.park_misses);
        let budget = handoff_freeze_budget(prelaunch.mode, prior, prelaunch.freeze_seed);
        Some((prior, budget, prelaunch.freeze_seed))
    }

    #[cfg(target_os = "macos")]
    fn prelaunch_out_of_band_handoff(
        &mut self,
        args: PrelaunchArgs,
    ) -> Result<(), crate::UpdateHandoffStartError> {
        let PrelaunchArgs {
            attempt_id,
            build,
            mode,
            apply_attempt,
            same_image,
            target_build,
            target_commit,
            command,
            verify_staged_candidate,
            grant_chunks,
            installed_activation,
            bundle,
            cancel,
            cancelled,
            job_tx,
            reconcile_worker,
            reconcile_ticket,
        } = args;
        let nonce = crate::seamless::mint_outgoing_nonce();
        let warm_hint = self.handoff_warm_hint();
        let speculative_layout = Some(self.capture_handoff_layout());
        let (stand_down_tx, stand_down_rx) = std::sync::mpsc::sync_channel(1);
        let (stand_down_ack_tx, stand_down_ack_rx) = std::sync::mpsc::sync_channel(1);
        let (transfer_tx, transfer_rx) = std::sync::mpsc::sync_channel(1);
        let arbiter = crate::HandoffAttemptArbiter::new();
        let preverified =
            PreverifyPublisher::for_attempt(&self.handoff_preverified, apply_attempt.as_ref());
        let successor_runs_candidate = apply_attempt.is_some() && same_image.is_none();
        let expected = apply_attempt
            .as_ref()
            .filter(|_| successor_runs_candidate && !installed_activation)
            .map(|attempt| {
                (
                    attempt.target_build(),
                    attempt.target_commit().to_string(),
                    attempt.target_dmg_sha256().to_string(),
                )
            });
        let history_export = self.start_history_export(
            target_build,
            self.park_policy(apply_attempt.as_ref()).carry_ceiling(),
        );
        self.update_handoff_prelaunch = Some(crate::HandoffPrelaunch {
            attempt_id,
            nonce: nonce.clone(),
            mode,
            apply_attempt,
            same_image,
            target_build,
            target_commit: target_commit.clone(),
            cancel: cancel.clone(),
            stand_down: stand_down_tx,
            stand_down_ack: stand_down_ack_tx,
            transfer: transfer_tx,
            arbiter: arbiter.clone(),
            begun_at: std::time::Instant::now(),
            dialled: None,
            park_retry_at: None,
            park_misses: 0,
            park_mid_sequence_reparks: 0,
            freeze_seed: FreezeSeed::Default,
            land_waits: 0,
            last_wait: None,
            stood_down: false,
            teardown: crate::DeferredHandoffTeardown::None,
            revoked_by_activity: false,
            booked_launch_unregistered: false,
            history_export,
        });
        let cleanup = HandoffWorkerCleanup {
            parent_socket: self.handoff_parent_socket(),
            reconcile: Some((reconcile_worker, reconcile_ticket)),
        };
        let job = HandoffWorkerJob {
            attempt_id,
            current_build: build,
            target_build,
            target_commit,
            verify_staged_candidate,
            installed_activation,
            preverified,
            parked_under: None,
            successor_grant_chunks: grant_chunks,
            command,
            capture: None,
            prelaunch: Some(HandoffPrelaunchLane {
                nonce,
                transfer: transfer_rx,
                stand_down: stand_down_rx,
                stand_down_ack: stand_down_ack_rx,
                hold_bound: PRELAUNCH_HOLD_BOUND,
                warm_hint: warm_hint.encode(),
                speculative_layout,
                successor_runs_candidate,
            }),
            lane: HandoffLane::OutOfBand,
            bundle,
            expected,
            renews_launch_record: same_image.is_some_and(SameImageHandoff::renews_launch_record),
            cleanup,
            cancel: cancelled,
            arbiter,
        };
        if job_tx.send(job).is_err() {
            self.update_handoff_prelaunch = None;
            return Err(crate::UpdateHandoffStartError::failed(
                "overlap handoff worker stopped before preparation",
            ));
        }
        aterm_log::info!(
            "update apply: launching the successor with every reader live (the late park); \
             the outgoing process parks once it has dialled"
        );
        let _seeded =
            self.seed_first_freeze_rung(mode, crate::seamless::WireCaps::for_target(target_build));
        Ok(())
    }

    /// `Wake::UpdateLaunchUnregistered`.
    #[cfg(target_os = "macos")]
    pub(crate) fn book_launch_unregistered(&mut self, attempt_id: u64, pid: i32) {
        let entry = crate::update_words::launch_unregistered_entry(pid);
        let key = entry.key();
        if self.messages.digest_keys_under(&key).contains(&key) {
            aterm_log::info!(
                "update apply: {key} is already open; the successor of attempt {attempt_id} \
                 (pid {pid}) adds nothing to it"
            );
            return;
        }
        let Some(prelaunch) = self
            .update_handoff_prelaunch
            .as_mut()
            .filter(|prelaunch| prelaunch.attempt_id == attempt_id)
        else {
            aterm_log::info!(
                "update apply: attempt {attempt_id} has ended; its successor's (pid {pid}) \
                 stale LaunchServices record is not booked"
            );
            return;
        };
        if prelaunch
            .same_image
            .is_some_and(SameImageHandoff::renews_launch_record)
        {
            aterm_log::warn!(
                "launch record: LaunchServices answered the renewal's launch of pid {pid} with \
                 no pid; the successor's own measure decides"
            );
            return;
        }
        prelaunch.booked_launch_unregistered = true;
        self.except(&entry);
    }

    /// `Wake::OwnLaunchRecord`.
    #[cfg(target_os = "macos")]
    pub(crate) fn note_own_launch_record(
        &mut self,
        record: crate::app_launch_successor::OwnLaunchRecord,
    ) {
        use crate::app_launch_successor::OwnLaunchRecord;
        let pid = std::process::id();
        self.own_launch_record = Some(record);
        match record {
            OwnLaunchRecord::Named => {
                match self.launch_record_renewal.renewed_from {
                    Some(from) => aterm_log::info!(
                        "launch record: LaunchServices names this process (pid {pid}), renewed \
                         from pid {from}"
                    ),
                    None => aterm_log::info!(
                        "launch record: LaunchServices names this process (pid {pid})"
                    ),
                }
                self.except_clear(&crate::update_words::launch_unregistered_entry(0).key());
            }
            OwnLaunchRecord::Stale => self.note_stale_launch_record(std::time::Instant::now()),
            OwnLaunchRecord::Unknown => aterm_log::info!(
                "launch record: LaunchServices' list of this app neither names pid {pid} nor \
                 holds an entry without a pid"
            ),
        }
    }

    /// `Wake::UpdateHandoffAwaitingPark`.
    #[cfg(any(target_os = "macos", all(test, unix)))]
    pub(crate) fn on_update_handoff_awaiting_park(
        &mut self,
        attempt_id: u64,
        dialer_pid: Option<u32>,
        grant_limit: Option<usize>,
    ) {
        let now = std::time::Instant::now();
        let Some(prelaunch) = self.update_handoff_prelaunch.as_mut() else {
            aterm_log::warn!(
                "update apply: ignored a park cue for attempt {attempt_id} — no attempt is \
                 prelaunched"
            );
            return;
        };
        if prelaunch.attempt_id != attempt_id {
            aterm_log::warn!(
                "update apply: ignored a stale park cue for attempt {attempt_id} (the \
                 prelaunched attempt is {})",
                prelaunch.attempt_id
            );
            return;
        }
        prelaunch.dialled = Some(crate::DialledSuccessor {
            pid: dialer_pid,
            grant_limit,
            at: now,
        });
        match dialer_pid {
            Some(pid) => aterm_log::info!(
                "update apply: the successor (pid {pid}) dialled {} ms after the attempt began \
                 and holds its claim; parking when the gate admits",
                now.saturating_duration_since(prelaunch.begun_at)
                    .as_millis()
            ),
            None => aterm_log::info!(
                "update apply: the launched lane refused at runtime; the worker forks once \
                 the outgoing process has parked"
            ),
        }
        self.try_park_for_prelaunched_successor(now);
    }

    /// The park gate for a prelaunched attempt.
    #[cfg(any(target_os = "macos", all(test, unix)))]
    pub(crate) fn try_park_for_prelaunched_successor(&mut self, now: std::time::Instant) {
        let Some(prelaunch) = self.update_handoff_prelaunch.as_ref() else {
            return;
        };
        if prelaunch.dialled.is_none() {
            return;
        }
        if prelaunch.stood_down || prelaunch.park_retry_at.is_some_and(|at| now < at) {
            return;
        }
        let attempt = self.park_and_transfer_to_prelaunched_successor(now);
        if !matches!(attempt, ParkAttempt::Parked | ParkAttempt::Pausing)
            && let Some(export) = self
                .update_handoff_prelaunch
                .as_mut()
                .and_then(|prelaunch| prelaunch.history_export.as_mut())
        {
            export.resume();
        }
        match attempt {
            ParkAttempt::Parked => {
                if let Some(prelaunch) = self.update_handoff_prelaunch.as_mut() {
                    prelaunch.park_retry_at = None;
                }
            }
            ParkAttempt::Pausing => {
                if let Some(prelaunch) = self.update_handoff_prelaunch.as_mut() {
                    prelaunch.park_retry_at = Some(now + PRELAUNCH_PAUSE_POLL);
                }
            }
            ParkAttempt::NotYet(reason) => {
                if let Some(prelaunch) = self.update_handoff_prelaunch.as_mut() {
                    prelaunch.park_retry_at = Some(now + PRELAUNCH_PARK_RETRY);
                    aterm_log::debug!("update apply: not parking yet — {reason}");
                }
            }
            ParkAttempt::Refused(refusal) => {
                aterm_log::warn!(
                    "update apply: {refusal}; a wider freeze rung cannot change that, so the \
                     held successor stands down and the lane retries once the desk changes"
                );
                self.stand_down_prelaunched_successor(capture_refusal_stand_down(&refusal));
            }
            ParkAttempt::Missed(reason) => {
                debug_assert!(
                    self.pending_update_handoff.is_none(),
                    "a park that missed its budget must leave no parked attempt behind"
                );
                let misses = self
                    .update_handoff_prelaunch
                    .as_ref()
                    .map_or(u8::MAX, |prelaunch| prelaunch.park_misses);
                self.dispose_park_miss(now, park_miss_disposition(misses, reason));
            }
            ParkAttempt::MissedMidSequence(reason) => {
                debug_assert!(
                    self.pending_update_handoff.is_none(),
                    "a park that missed its budget must leave no parked attempt behind"
                );
                let (misses, reparks) = self
                    .update_handoff_prelaunch
                    .as_ref()
                    .map_or((u8::MAX, u8::MAX), |prelaunch| {
                        (prelaunch.park_misses, prelaunch.park_mid_sequence_reparks)
                    });
                self.dispose_park_miss(now, mid_sequence_miss_disposition(misses, reparks, reason));
            }
            ParkAttempt::Failed(stand_down) => self.stand_down_prelaunched_successor(stand_down),
        }
    }

    #[cfg(any(target_os = "macos", all(test, unix)))]
    fn retire_rim_while_the_gate_waits(&mut self) {
        if self.pending_update_handoff.is_none() {
            self.retire_charging_surge();
        }
    }

    #[cfg(any(target_os = "macos", all(test, unix)))]
    fn dispose_park_miss(&mut self, now: std::time::Instant, disposition: ParkMissDisposition) {
        match disposition {
            ParkMissDisposition::StandDown(stand_down) => {
                self.stand_down_prelaunched_successor(stand_down);
            }
            ParkMissDisposition::RetryMidSequence { reparks, reason } => {
                if let Some(prelaunch) = self.update_handoff_prelaunch.as_mut() {
                    prelaunch.park_mid_sequence_reparks = reparks;
                    prelaunch.park_retry_at = Some(now + PRELAUNCH_PARK_RETRY);
                    aterm_log::debug!(
                        "update apply: {reason}; re-parking at the next batch boundary \
                         (mid-sequence re-park {reparks} of \
                         {PRELAUNCH_MAX_MID_SEQUENCE_REPARKS}, no rung spent)"
                    );
                }
            }
            ParkMissDisposition::Repark { misses, reason } => {
                if let Some(prelaunch) = self.update_handoff_prelaunch.as_mut() {
                    prelaunch.park_misses = misses;
                    prelaunch.park_retry_at = Some(now + PRELAUNCH_REPARK_DELAY);
                    aterm_log::warn!(
                        "update apply: the park missed its budget ({reason}); the \
                         successor keeps holding and the park retries on the next \
                         rung (miss {misses} of {PRELAUNCH_MAX_PARK_MISSES})"
                    );
                }
            }
        }
    }

    /// Tells the worker to stand the held successor down with a typed reason.
    #[cfg(any(target_os = "macos", all(test, unix)))]
    pub(crate) fn stand_down_prelaunched_successor(&mut self, stand_down: HandoffStandDown) {
        let Some(prelaunch) = self.update_handoff_prelaunch.as_mut() else {
            return;
        };
        if prelaunch.stood_down {
            return;
        }
        prelaunch.stood_down = true;
        prelaunch.park_retry_at = None;
        aterm_log::warn!(
            "update apply: standing the prelaunched successor down — {}",
            stand_down.detail
        );
        if prelaunch.stand_down.try_send(stand_down).is_err() {
            let _ = prelaunch.cancel.try_send(());
        }
    }

    /// `Wake::UpdateHandoffStandingDown`.
    #[cfg(any(target_os = "macos", all(test, unix)))]
    pub(crate) fn answer_prelaunched_stand_down(&mut self, attempt_id: u64) {
        let mut ack = None;
        if let Some(prelaunch) = self.update_handoff_prelaunch.as_mut()
            && prelaunch.attempt_id == attempt_id
        {
            prelaunch.stood_down = true;
            prelaunch.park_retry_at = None;
            ack = Some(prelaunch.stand_down_ack.clone());
        }
        if let Some(pending) = self
            .pending_update_handoff
            .take_if(|pending| pending.attempt_id == attempt_id)
        {
            if let Some(prelaunch) = self.update_handoff_prelaunch.as_mut()
                && prelaunch.attempt_id == attempt_id
            {
                prelaunch.teardown.merge(pending.teardown);
                prelaunch.revoked_by_activity |= pending.revoked_by_activity;
            }
            self.rollback_overlap(pending.nonce.as_deref(), &pending.live);
            aterm_log::info!(
                "update apply: readers resumed {} ms after the park — the held successor is \
                 being stood down before the attempt retires",
                pending.park_at.elapsed().as_millis()
            );
        }
        if let Some(ack) = ack {
            let _ = ack.try_send(());
        }
    }

    #[cfg(any(target_os = "macos", all(test, unix)))]
    fn handoff_gate_phase(
        &self,
        mode: crate::native_updater_service::ApplyMode,
        same_image: Option<SameImageHandoff>,
        now: std::time::Instant,
    ) -> crate::native_update_auto_intent::ApplyPhase {
        #[cfg(target_os = "macos")]
        if same_image.is_some_and(SameImageHandoff::renews_launch_record) {
            return self.launch_record_renewal.gate_phase(now);
        }
        #[cfg(not(target_os = "macos"))]
        let _ = same_image;
        self.automatic_phase_for(mode, now)
    }

    #[cfg(any(target_os = "macos", all(test, unix)))]
    fn park_and_transfer_to_prelaunched_successor(
        &mut self,
        now: std::time::Instant,
    ) -> ParkAttempt {
        let Some(prelaunch) = self.update_handoff_prelaunch.as_ref() else {
            return ParkAttempt::NotYet("no attempt is prelaunched");
        };
        let attempt_id = prelaunch.attempt_id;
        let mode = prelaunch.mode;
        let apply_attempt = prelaunch.apply_attempt.clone();
        let same_image = prelaunch.same_image;
        let target_build = prelaunch.target_build;
        let target_commit = prelaunch.target_commit.clone();
        let nonce = prelaunch.nonce.clone();
        let cancel = prelaunch.cancel.clone();
        let arbiter = prelaunch.arbiter.clone();
        let dialled = prelaunch.dialled;
        let dialer_pid = dialled.and_then(|dialled| dialled.pid);
        let grant_limit = dialled.and_then(|dialled| dialled.grant_limit);
        let dialled_at = dialled.map_or(now, |dialled| dialled.at);
        let park_misses = prelaunch.park_misses;
        let land_waits = prelaunch.land_waits;
        let build = crate::build_info::build_number()
            .parse::<u64>()
            .unwrap_or(0);
        let policy = self.park_policy(apply_attempt.as_ref());
        let ceiling = policy.carry_ceiling();
        if !ceiling.carries_scrollback() {
            self.withhold_history_export(target_build, ceiling);
        }

        let live: Vec<(u64, i32, i32)> = self.handoff_live_sessions();
        if live.is_empty() {
            return ParkAttempt::Failed(HandoffStandDown {
                outcome: crate::UpdateHandoffOutcome::ActivityRevoked,
                detail: "every terminal session closed while the successor booted".to_string(),
            });
        }
        let pause_asked = self
            .update_handoff_prelaunch
            .as_ref()
            .and_then(|prelaunch| prelaunch.history_export.as_ref())
            .is_some_and(crate::handoff_history::HistoryExporter::pause_asked);
        if !pause_asked {
            let exporting = self
                .update_handoff_prelaunch
                .as_mut()
                .and_then(|prelaunch| prelaunch.history_export.as_mut())
                .is_some_and(|export| !export.passed());
            if let Some(reason) = crate::handoff_history::park_wait(
                exporting,
                now.saturating_duration_since(dialled_at),
            ) {
                self.retire_rim_while_the_gate_waits();
                return ParkAttempt::NotYet(reason);
            }
            let gate_facts = ParkGateFacts {
                mode,
                phase: self.handoff_gate_phase(mode, same_image, now),
                activity: self.automatic_activity_facts(mode, now),
                masters_quiet: !handoff_masters_have_activity(&live),
                masters_alive: !handoff_masters_closed(&live),
                land_waits,
                land_gate_relaxed: policy.relaxes_land_gate(),
                held_for: now.saturating_duration_since(dialled_at),
                recording: self.video_request_live(),
            };
            let gate = prelaunch_park_admitted(gate_facts, prelaunch_hold_cap(mode));
            let last_wait = self
                .update_handoff_prelaunch
                .as_ref()
                .and_then(|prelaunch| prelaunch.last_wait);
            if let Some(prelaunch) = self.update_handoff_prelaunch.as_mut() {
                prelaunch.land_waits = match gate {
                    ParkGate::Wait(_)
                        if gate_facts.phase
                            == crate::native_update_auto_intent::ApplyPhase::Land =>
                    {
                        land_waits.saturating_add(1)
                    }
                    _ => 0,
                };
                if let ParkGate::Wait(reason) = gate {
                    prelaunch.last_wait = Some(reason);
                }
            }
            if !gate_facts.masters_quiet && gate == ParkGate::Park && mode.is_automatic() {
                if land_waits < PRELAUNCH_LAND_MAX_WAITS && gate_facts.land_gate_relaxed {
                    aterm_log::info!(
                        "update apply: parking over output still queued on a master after \
                         {land_waits} consecutive waits, as build {target_build}'s handoff policy \
                         asks; the successor replays it after Commit"
                    );
                } else {
                    aterm_log::info!(
                        "update apply: parking at the bound over output still queued on a master \
                         after {land_waits} consecutive waits; the successor replays it after Commit"
                    );
                }
            }
            if gate == ParkGate::Park
                && let Some(policy) = policy.policy
            {
                aterm_log::info!(
                    "update apply: parking under build {target_build}'s handoff policy ({policy})"
                );
            }
            match gate {
                ParkGate::Park => {}
                ParkGate::Wait(reason) => {
                    self.retire_rim_while_the_gate_waits();
                    return ParkAttempt::NotYet(reason);
                }
                ParkGate::StandDown(reason) => {
                    return ParkAttempt::Failed(HandoffStandDown {
                        outcome: crate::UpdateHandoffOutcome::ActivityRevoked,
                        detail: hold_cap_stand_down_detail(reason, gate_facts.held_for, last_wait),
                    });
                }
            }
        }
        if let Some(export) = self
            .update_handoff_prelaunch
            .as_mut()
            .and_then(|prelaunch| prelaunch.history_export.as_mut())
        {
            let caught_up = export.caught_up();
            let halt_wait = crate::handoff_history::park_halt_wait();
            let paused = match export.ask_pause(halt_wait) {
                crate::handoff_history::PauseAnswer::Asked => return ParkAttempt::Pausing,
                crate::handoff_history::PauseAnswer::Paused => true,
                crate::handoff_history::PauseAnswer::Unanswered => false,
            };
            if !caught_up {
                aterm_log::warn!(
                    "update apply: the scrollback export ran past {} s after the dial; {} it \
                     before the park — the tabs it had not reached carry the screen carry's \
                     bounded history, counted",
                    crate::handoff_history::EXPORT_PATIENCE.as_secs(),
                    if paused {
                        "paused"
                    } else {
                        "asked to pause (it had not answered)"
                    }
                );
            } else if !paused {
                aterm_log::warn!(
                    "update apply: the scrollback export had not paused {} ms after the park \
                     asked; the park goes ahead, and the worker hands over what it finished",
                    halt_wait.as_millis()
                );
            }
        }
        if let Some(limit) = grant_limit
            && live.len() > limit
        {
            return ParkAttempt::Failed(HandoffStandDown {
                outcome: crate::UpdateHandoffOutcome::ProducerFailed,
                detail: format!(
                    "more sessions opened than the successor's descriptor grant carries ({} \
                     open, its rendezvous claim carries at most {limit})",
                    live.len()
                ),
            });
        }
        let hold_serials = self.hold_serials();
        let manifest = match self.store.try_read() {
            Ok(store) => crate::session_store::SessionHandoff::from_store(&store),
            Err(std::sync::TryLockError::Poisoned(poison)) => {
                let store = poison.into_inner();
                crate::session_store::SessionHandoff::from_store(&store)
            }
            Err(std::sync::TryLockError::WouldBlock) => {
                return ParkAttempt::NotYet("the session registry is busy");
            }
        };
        let manifest = handed_sessions_only(manifest, &live);
        let carried_claims = carried_claims_of(&manifest);
        let (owned_masters, adoption) = match reserve_child_only_masters(&live) {
            Ok(reserved) => reserved,
            Err(error) => {
                let kind = error.kind();
                return ParkAttempt::Failed(HandoffStandDown {
                    outcome: crate::UpdateHandoffOutcome::ProducerFailed,
                    detail: format!("could not reserve child-only PTY descriptors ({kind})"),
                });
            }
        };
        let fds = crate::session_store::HandoffFds {
            entries: adoption.clone(),
        };
        let window = self.park_window_carry(target_build);
        #[cfg(target_os = "macos")]
        let Some(proof_identities) =
            crate::handoff_rendezvous::proof_identities_in_device_terms(&adoption)
        else {
            return ParkAttempt::Failed(HandoffStandDown {
                outcome: crate::UpdateHandoffOutcome::ProducerFailed,
                detail: "a handed-off PTY would not answer fstat, so the out-of-band proof \
                         term cannot be computed"
                    .to_string(),
            });
        };
        #[cfg(not(target_os = "macos"))]
        let proof_identities = adoption.clone();
        if self.update_handoff_activity_epoch == u64::MAX {
            return ParkAttempt::Failed(HandoffStandDown {
                outcome: crate::UpdateHandoffOutcome::ProducerFailed,
                detail: "handoff activity identity space is exhausted".to_string(),
            });
        }
        let Some((prior_physical_failures, freeze_budget, freeze_seed)) =
            self.prelaunched_park_budget()
        else {
            return ParkAttempt::NotYet("no attempt is prelaunched");
        };
        #[cfg(test)]
        let freeze_budget = park_budget_for_test::deadline_budget(freeze_budget);
        let freeze_ms = freeze_budget.as_millis();
        let handoff_history_comfort = freeze_budget / 2;
        aterm_log::info!(
            "update apply: freeze budget {freeze_ms} ms ({mode:?}, {prior_physical_failures} prior \
             physical failure(s) of the target artifact incl. {park_misses} park miss(es) of this \
             attempt; {freeze_seed}; running build {build}) — parking {} ms after the successor's \
             dial",
            now.saturating_duration_since(dialled_at).as_millis()
        );
        self.charge_upgrade_rim_at_park(build);
        let park_at = std::time::Instant::now();
        let deadline = park_at + freeze_budget;
        if !self.park_all_readers(deadline) {
            self.rollback_overlap(None, &live);
            return ParkAttempt::Missed(format!(
                "a PTY reader missed the {freeze_ms} ms handoff park deadline"
            ));
        }
        let readers_parked_at = std::time::Instant::now();
        if mode.is_automatic() && handoff_masters_closed(&live) {
            self.rollback_overlap(None, &live);
            return ParkAttempt::Failed(HandoffStandDown {
                outcome: crate::UpdateHandoffOutcome::ActivityRevoked,
                detail: "a PTY session closed during automatic reader park".to_string(),
            });
        }
        let ParkedScreens {
            screens,
            carries,
            screen_digest,
            repaint,
            fg_holders,
            render_debts,
            history_heads,
            held,
        } = match self.capture_parked_screens(
            &live,
            deadline,
            freeze_ms,
            handoff_history_comfort,
            crate::seamless::WireCaps::for_target(target_build),
            ceiling,
        ) {
            Ok(captured) => captured,
            Err(failure) => {
                let readers_ms = readers_parked_at
                    .saturating_duration_since(park_at)
                    .as_millis();
                let capture_ms = readers_parked_at.elapsed().as_millis();
                self.rollback_overlap(None, &live);
                aterm_log::info!(
                    "update apply: the park stopped the readers in {readers_ms} ms and its \
                     capture ran {capture_ms} ms before it stopped ({freeze_ms} ms budget)"
                );
                if matches!(failure, CaptureFailure::MidSequence { .. }) {
                    return ParkAttempt::MissedMidSequence(failure.to_string());
                }
                return classify_capture_failure(&failure);
            }
        };
        let manifest = stamp_fg_holders(manifest, &fg_holders);
        let manifest = stamp_render_debts(manifest, &render_debts);
        let manifest = stamp_journal_keys(manifest, &self.journal_keys());
        let manifest = stamp_status_seen(manifest, &self.status_carry_rows());
        let manifest = stamp_autopilot_end(manifest, &self.end_carry_rows());
        let manifest = stamp_fleet_told(manifest, self.fleet_told_rows());
        let layout = self.capture_handoff_layout();
        let layout_digest = match commit_handoff_layout(&layout) {
            Ok(digest) => digest,
            Err(refusal) => {
                self.rollback_overlap(None, &live);
                return ParkAttempt::Refused(refusal);
            }
        };
        if std::time::Instant::now() >= deadline {
            self.rollback_overlap(None, &live);
            return ParkAttempt::Missed(format!(
                "handoff proof capture exceeded the {freeze_ms} ms deadline"
            ));
        }
        let _ledger_write = keep_landed_park(park_at.elapsed());
        let activity_epoch = self.update_handoff_activity_epoch;
        let proof_deadline = handoff_proof_deadline(mode, prior_physical_failures, park_at);
        self.pending_update_handoff = Some(crate::PendingUpdateHandoff {
            attempt_id,
            park_at,
            proof_ready_at: None,
            nonce: Some(nonce),
            live: live.clone(),
            adoption: proof_identities.clone(),
            child_pid: dialer_pid,
            mode,
            apply_attempt,
            same_image,
            target_build,
            target_commit,
            layout: layout.clone(),
            layout_digest,
            screen_digest,
            activity_epoch,
            hold_serials,
            carried_claims,
            cancel,
            arbiter,
            teardown: crate::DeferredHandoffTeardown::None,
            commit_drain_started: None,
            commit_flush: None,
            held_commit: None,
            revoked_by_activity: false,
        });
        let transfer = HandoffTransferJob {
            capture: HandoffCapture {
                #[cfg(target_os = "macos")]
                park_at,
                #[cfg(target_os = "macos")]
                captured_at: std::time::Instant::now(),
                manifest,
                fds,
                screens,
                repaint,
                carries,
                window,
                layout,
                layout_digest,
                screen_digest,
                live: adoption,
                proof_identities,
                _owned_masters: owned_masters,
                history_heads,
                history: crate::handoff_history::HistoryPlan::exported_under(
                    ceiling,
                    self.update_handoff_prelaunch
                        .as_mut()
                        .and_then(|prelaunch| prelaunch.history_export.take()),
                ),
                held,
            },
            proof_deadline,
        };
        let carried = transfer.capture.screens.len();
        let proof_budget_ms = transfer
            .proof_deadline
            .saturating_duration_since(park_at)
            .as_millis();
        let sent = self
            .update_handoff_prelaunch
            .as_ref()
            .is_some_and(|prelaunch| prelaunch.transfer.try_send(transfer).is_ok());
        if !sent {
            self.pending_update_handoff = None;
            self.rollback_overlap(None, &live);
            return ParkAttempt::Failed(HandoffStandDown {
                outcome: crate::UpdateHandoffOutcome::Rejected,
                detail: "overlap handoff worker stopped before the transfer".to_string(),
            });
        }
        aterm_log::info!(
            "update apply: parked and captured {carried} screen(s) in {} ms (the readers stopped \
             in {} ms, the capture and its hand-over took {} ms); the capture is on its way to \
             the held successor (proof due {proof_budget_ms} ms after the park)",
            park_at.elapsed().as_millis(),
            readers_parked_at
                .saturating_duration_since(park_at)
                .as_millis(),
            readers_parked_at.elapsed().as_millis()
        );
        ParkAttempt::Parked
    }

    #[cfg(unix)]
    fn handoff_drain_gate(
        &mut self,
        completion: crate::UpdateHandoffCompletion,
    ) -> Option<(crate::UpdateHandoffCompletion, bool, bool)> {
        const HANDOFF_INPUT_DRAIN_SPIN_CAP: u32 = 4_000;
        const HANDOFF_INPUT_DISPATCH_FENCE: std::time::Duration =
            std::time::Duration::from_millis(30);
        const HANDOFF_INPUT_DRAIN_DEADLINE: std::time::Duration =
            std::time::Duration::from_millis(3_000);
        let drained_for = {
            let now = std::time::Instant::now();
            self.pending_update_handoff.as_mut().map(|pending| {
                pending.proof_ready_at.get_or_insert(now);
                now.saturating_duration_since(*pending.commit_drain_started.get_or_insert(now))
            })
        };
        let drained_for = drained_for.unwrap_or(HANDOFF_INPUT_DRAIN_DEADLINE);
        let input_dispatch_fenced =
            completion.input_drain_spins >= 1 && drained_for >= HANDOFF_INPUT_DISPATCH_FENCE;
        let egress_settled = self
            .pending_update_handoff
            .as_ref()
            .map(|pending| pending.live.clone())
            .is_none_or(|live| {
                let replies = crate::control::input_replies_of(&self.store);
                handoff_egress_settled(&self.pool, &live, &replies)
            });
        let held = self
            .pending_update_handoff
            .as_ref()
            .is_some_and(|pending| pending.held_commit.is_some());
        if (!input_dispatch_fenced || !egress_settled)
            && completion.input_drain_spins < HANDOFF_INPUT_DRAIN_SPIN_CAP
            && drained_for < HANDOFF_INPUT_DRAIN_DEADLINE
            && self.handoff_loop_present()
        {
            std::thread::sleep(HANDOFF_INPUT_DRAIN_YIELD);
            let respin = crate::UpdateHandoffCompletion {
                input_drain_spins: completion.input_drain_spins.saturating_add(1),
                ..completion
            };
            return match self.repost_handoff_completion(respin) {
                Ok(()) => None,
                Err(respin) if held => Some((*respin, false, egress_settled)),
                Err(_) => None,
            };
        }
        Some((completion, input_dispatch_fenced, egress_settled))
    }

    #[cfg(unix)]
    const fn handoff_loop_present(&self) -> bool {
        #[cfg(test)]
        if self.handoff_reposts.is_some() {
            return true;
        }
        self.proxy.is_some()
    }

    #[cfg(unix)]
    fn repost_handoff_completion(
        &mut self,
        completion: crate::UpdateHandoffCompletion,
    ) -> Result<(), Box<crate::UpdateHandoffCompletion>> {
        #[cfg(test)]
        if let Some(reposts) = self.handoff_reposts.as_mut() {
            reposts.push(completion);
            return Ok(());
        }
        let Some(proxy) = self.proxy.clone() else {
            return Err(Box::new(completion));
        };
        proxy
            .send_event(Wake::UpdateHandoffFinished(completion))
            .map_err(|closed| {
                let Wake::UpdateHandoffFinished(completion) = closed.0 else {
                    unreachable!("a closed loop hands back the wake it was sent");
                };
                Box::new(completion)
            })
    }

    #[cfg(unix)]
    fn commit_flush_refences(&mut self) -> bool {
        if self
            .pending_update_handoff
            .as_ref()
            .is_none_or(|pending| pending.commit_flush.is_some())
        {
            return false;
        }
        let writer = self.messages_log.is_some();
        let started = std::time::Instant::now();
        let landed = self.flush_messages_log_within(MESSAGES_FLUSH_COMMIT_WAIT);
        let held = if writer {
            started.elapsed()
        } else {
            std::time::Duration::ZERO
        };
        self.settle_commit_flush(landed, held)
    }

    #[cfg(unix)]
    fn settle_commit_flush(&mut self, landed: bool, held: std::time::Duration) -> bool {
        let refence = held >= HANDOFF_INPUT_DRAIN_YIELD;
        if let Some(pending) = self.pending_update_handoff.as_mut() {
            pending.commit_flush = Some(landed);
            if refence {
                pending.commit_drain_started = None;
            }
        }
        refence
    }

    /// The pairs this process's crash journal keys its handed shells by.
    #[cfg(unix)]
    pub(crate) fn journal_keys(&self) -> Vec<(u64, (u32, u64))> {
        let Some(parent) = self.crash_journal.inherited_image_parent() else {
            return Vec::new();
        };
        self.pool
            .sessions
            .iter()
            .filter_map(|(id, pooled)| {
                let session = &pooled.session;
                session
                    .journal_key
                    .or_else(|| session.handoff_local_id.map(|was| (parent, was)))
                    .map(|key| (*id, key))
            })
            .collect()
    }

    /// Every pooled session's hold serial, summed.
    #[cfg(unix)]
    pub(crate) fn hold_serials(&self) -> u64 {
        self.pool
            .iter()
            .map(|session| session.ctx.fabric.carry_serial())
            .fold(0, u64::wrapping_add)
    }

    /// Return whether every handed session's [`crate::session_store::CarriedClaims`] still holds.
    #[cfg(unix)]
    pub(crate) fn carried_claims_held(
        &self,
        drawn: &[crate::session_store::CarriedClaims],
    ) -> bool {
        let now_us = crate::metrics::now_us();
        let own_holder = crate::harness_host::holder();
        drawn.iter().all(|claims| {
            self.pool.get(claims.local_id).is_some_and(|session| {
                crate::session_store::CarriedClaims::of_session(
                    claims.local_id,
                    &session.ctx,
                    now_us,
                    &own_holder,
                ) == *claims
            })
        })
    }

    #[cfg(unix)]
    fn ids_past_handoff_ceiling(&self) -> bool {
        self.pool.iter().any(|session| {
            let timeline = session
                .ctx
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .past_handoff_ceiling();
            timeline || session.ctx.fabric.past_handoff_ceiling()
        }) || self
            .store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .mirror()
            .past_draw()
    }

    #[cfg(unix)]
    fn fence_hold_serials(&self, successor_reads_carry: bool) -> (u64, HoldFence) {
        let fabrics: Vec<std::sync::Arc<crate::fabric::SessionFabric>> = self
            .pool
            .iter()
            .map(|session| std::sync::Arc::clone(&session.ctx.fabric))
            .collect();
        let serials = fabrics
            .iter()
            .map(|fabric| fabric.fence_hold(successor_reads_carry))
            .fold(0, u64::wrapping_add);
        (serials, HoldFence(fabrics))
    }

    #[cfg(unix)]
    #[allow(clippy::type_complexity)]
    fn collect_handoff_commit_facts(
        &mut self,
        nonce: Option<&str>,
        input_dispatch_fenced: bool,
        egress_settled: bool,
        commit_channel: bool,
        held_fence: Option<HoldFence>,
    ) -> Option<(
        HandoffCommitFacts,
        Result<crate::app_native::NativeUpdateSafetyToken, Vec<String>>,
        Option<crate::seamless::AdoptionProof>,
        crate::HandoffAttemptArbiter,
        HoldFence,
    )> {
        let pending = self.pending_update_handoff.as_ref()?;
        let pending_live = pending.live.clone();
        let pending_adoption = pending.adoption.clone();
        let pending_target_build = pending.target_build;
        let pending_target_commit = pending.target_commit.clone();
        let pending_layout = pending.layout.clone();
        let pending_layout_digest = pending.layout_digest;
        let pending_screen_digest = pending.screen_digest;
        let pending_activity_epoch = pending.activity_epoch;
        let pending_hold_serials = pending.hold_serials;
        let pending_carried_claims = pending.carried_claims.clone();
        let arbiter = pending.arbiter.clone();
        let teardown_allows_commit = matches!(
            pending.teardown,
            crate::DeferredHandoffTeardown::None | crate::DeferredHandoffTeardown::CleanQuitReady
        );
        let (commit_hold_serials, hold_fence) = match held_fence {
            Some(fence) => (fence.serials(), fence),
            None => self.fence_hold_serials(successor_reads_fabric_carry(
                pending_target_build,
                crate::running_build_number(),
            )),
        };
        let turn_ids_held =
            !crate::control::turn_ids_past_handoff_ceiling(arbiter.turn_id_ceiling());
        if !turn_ids_held {
            aterm_log::warn!(
                "update apply: this process minted more turn ids since the handoff manifest \
                 than its reserve holds; standing the Commit down so no id is minted twice"
            );
        }
        let claims_held = self.carried_claims_held(&pending_carried_claims);
        if !claims_held {
            aterm_log::warn!(
                "update apply: a session's topics, escalations or supervisor claim moved since \
                 the handoff manifest was drawn; standing the Commit down so the next attempt \
                 carries them"
            );
        }
        let exact_activity = self.update_handoff_activity_epoch == pending_activity_epoch
            && !self.workstation_setup.pending()
            && self.tab_spawns.is_empty()
            && {
                #[cfg(target_os = "macos")]
                {
                    self.askpass_sheet.is_none()
                }
                #[cfg(not(target_os = "macos"))]
                {
                    true
                }
            }
            && commit_hold_serials == pending_hold_serials
            && !self.ids_past_handoff_ceiling()
            && turn_ids_held
            && claims_held;
        let exited = self.handoff_exited_session_ids();
        let mut current_live: Vec<(u64, i32, i32)> = self
            .pool
            .iter()
            .filter(|s| {
                !exited.contains(&s.id) || pending_live.iter().any(|(id, _, _)| *id == s.id)
            })
            .map(|s| (s.id, s.master, s.pid))
            .collect();
        current_live.sort_unstable();
        let mut expected_live = pending_live.clone();
        expected_live.sort_unstable();
        let exact_sessions = current_live == expected_live;
        let live_layout = self.capture_handoff_layout();
        let exact_layout =
            commit_layout_topology(&live_layout) == commit_layout_topology(&pending_layout);
        let exact_agent_carry =
            commit_agent_carry(&live_layout) == commit_agent_carry(&pending_layout);
        if !exact_agent_carry {
            aterm_log::info!(
                "update apply: a hosted agent's relaunch became owed (or was settled) after the \
                 handoff layout was captured; standing the Commit down so the successor is not \
                 handed a layout without it — this process relaunches it, and the next attempt \
                 carries what is still owed"
            );
        }
        if exact_layout && exact_agent_carry && live_layout != pending_layout {
            aterm_log::info!(
                "update apply: the Commit-time capture differs from the committed snapshot \
                 only in window position, window show state or degradable session metadata \
                 — topology is unchanged, so Commit stays admitted"
            );
        }
        let parent_still_parked = self
            .pool
            .iter()
            .all(|session| session.reader_join.is_none());
        let native_safety = self.revalidate_native_update_safety();
        let sessions_alive = !handoff_masters_closed(&pending_live);
        let proof = nonce.and_then(|nonce| {
            crate::seamless::adoption_proof(
                nonce,
                pending_target_build,
                &pending_target_commit,
                &pending_layout_digest,
                &pending_screen_digest,
                &pending_adoption,
            )
            .ok()
        });
        let facts = HandoffCommitFacts {
            exact_sessions,
            exact_layout,
            exact_agent_carry,
            recovery_count_ready: self.recovery_count_admission.ready(),
            exact_activity,
            teardown_allows_commit,
            parent_still_parked,
            sessions_alive,
            input_dispatch_fenced,
            egress_settled,
            native_safe: native_safety.is_ok(),
            proof_exact: proof.is_some(),
            commit_channel,
        };
        Some((facts, native_safety, proof, arbiter, hold_fence))
    }

    /// Main-thread completion of the overlap proof.
    #[cfg(unix)]
    pub(crate) fn finish_update_handoff(
        &mut self,
        el: &ActiveEventLoop,
        completion: crate::UpdateHandoffCompletion,
    ) {
        let matches_pending = self
            .pending_update_handoff
            .as_ref()
            .is_some_and(|pending| pending.attempt_id == completion.attempt_id);
        #[cfg(any(target_os = "macos", all(test, unix)))]
        let matches_pending = matches_pending
            || self
                .update_handoff_prelaunch
                .as_ref()
                .is_some_and(|prelaunch| prelaunch.attempt_id == completion.attempt_id);
        if !matches_pending {
            let attempt_id = completion.attempt_id;
            let _ = deliver_handoff_rejection(completion.reject);
            aterm_log::warn!("update apply: ignored stale handoff completion {attempt_id}");
            return;
        }
        if completion.outcome == crate::UpdateHandoffOutcome::ProofReady {
            self.finish_proven_handoff(completion);
            return;
        }

        let Some(teardown) = self.reduce_returned_handoff_completion(completion) else {
            return;
        };
        let deferred_exits = std::mem::take(&mut self.log_closes_deferred);
        self.refresh_version_menu();
        match teardown {
            crate::DeferredHandoffTeardown::None => {
                let _ = crate::menu::cancel_current_native_termination();
            }
            crate::DeferredHandoffTeardown::Mutations(mutations) => {
                let _ = crate::menu::cancel_current_native_termination();
                let closing_windows: std::collections::BTreeSet<_> = mutations
                    .iter()
                    .filter_map(|mutation| match mutation {
                        crate::DeferredHandoffMutation::CloseWindow(window) => Some(*window),
                        _ => None,
                    })
                    .collect();
                let closing_tabs: std::collections::BTreeSet<_> = mutations
                    .iter()
                    .filter_map(|mutation| match mutation {
                        crate::DeferredHandoffMutation::CloseTab { window, tab } => {
                            Some((*window, *tab))
                        }
                        _ => None,
                    })
                    .collect();
                let mut replay_close_windows = closing_windows.clone();
                for mutation in mutations {
                    match mutation {
                        crate::DeferredHandoffMutation::ExitSession(session) => {
                            replay_close_windows.extend(self.exit_session_logical(session));
                        }
                        crate::DeferredHandoffMutation::CloseView { window, tab, view }
                            if !closing_windows.contains(&window)
                                && !closing_tabs.contains(&(window, tab)) =>
                        {
                            if let Some(window) =
                                self.replay_deferred_handoff_view_close(window, tab, view)
                            {
                                replay_close_windows.insert(window);
                            }
                        }
                        crate::DeferredHandoffMutation::CloseTab { window, tab }
                            if !closing_windows.contains(&window) =>
                        {
                            if self.replay_deferred_handoff_tab_close(window, tab) {
                                replay_close_windows.insert(window);
                            }
                        }
                        crate::DeferredHandoffMutation::CloseWindow(_)
                        | crate::DeferredHandoffMutation::CloseView { .. }
                        | crate::DeferredHandoffMutation::CloseTab { .. } => {}
                    }
                }
                for window in replay_close_windows {
                    self.close_window(el, window);
                }
            }
            crate::DeferredHandoffTeardown::QuitRequested => {
                let _ = crate::menu::cancel_current_native_termination();
                self.on_quit_requested(el);
            }
            crate::DeferredHandoffTeardown::NativeTerminate { generation } => {
                self.on_native_terminate_requested(el, generation);
            }
            crate::DeferredHandoffTeardown::CleanQuitReady => {
                // A native view or document may have gained unsaved work during the
                // rollback, so both are asked again at the exit edge.
                if self.replay_clean_quit_ready() {
                    el.exit();
                }
            }
        }
        for session in deferred_exits {
            self.settle_log_close(session);
        }
    }

    #[cfg(unix)]
    fn finish_proven_handoff(&mut self, completion: crate::UpdateHandoffCompletion) {
        let first_proof = {
            let Some(pending) = self.pending_update_handoff.as_mut() else {
                if let Some(reject) = completion.reject {
                    let _ = reject.try_send(());
                }
                return;
            };
            pending.nonce = completion.nonce.clone();
            pending.child_pid = completion.child_pid;
            pending.proof_ready_at.is_none()
        };
        if first_proof && let Some(title) = self.publish_handoff_target_to_menu_bar() {
            aterm_log::info!("update apply: Version menu now reads the successor's {title}");
        }
        let Some((completion, input_dispatch_fenced, egress_settled)) =
            self.handoff_drain_gate(completion)
        else {
            return;
        };
        let held = self
            .pending_update_handoff
            .as_mut()
            .and_then(|pending| pending.held_commit.take());
        let claimed = held.is_some();
        let held_fence = held.map(|held| held.hold_fence);
        let Some((mut facts, native_safety, proof, arbiter, hold_fence)) = self
            .collect_handoff_commit_facts(
                completion.nonce.as_deref(),
                input_dispatch_fenced,
                egress_settled,
                completion.commit_fd.is_some(),
                held_fence,
            )
        else {
            // The pending attempt vanished mid-drain: nothing can be
            // committed; ask the worker to reject and reap.
            if let Some(reject) = completion.reject {
                let _ = reject.try_send(());
            }
            return;
        };
        let commit_admitted = handoff_commit_admitted(facts);
        let completion =
            if !claimed && commit_admitted && proof.is_some() && self.commit_flush_refences() {
                match self.handoff_drain_gate(crate::UpdateHandoffCompletion {
                    input_drain_spins: 0,
                    ..completion
                }) {
                    Some((completion, ..)) => completion,
                    None => return,
                }
            } else {
                completion
            };
        let crate::UpdateHandoffCompletion {
            attempt_id,
            nonce,
            child_pid,
            outcome: _outcome,
            commit_fd,
            reject,
            reconcile: _reconcile,
            detail: _detail,
            input_drain_spins: _input_drain_spins,
            child_death: _child_death,
        } = completion;
        let admitted = commit_admitted && commit_fd.is_some() && proof.is_some();
        let claim = claimed || (admitted && arbiter.try_begin_commit());
        let commit_lost_arbiter = admitted && !claim;
        let mut commit_write_failed = false;
        if claim && !claimed {
            let answered_in = self.answer_at_commit_claim(child_pid);
            #[cfg(test)]
            let answered_in = self
                .handoff_commit_answers_took
                .take()
                .unwrap_or(answered_in);
            if answered_in >= HANDOFF_INPUT_DRAIN_YIELD {
                aterm_log::info!(
                    "update apply: the Commit's answers took {} ms; its claim is held through \
                     one more input-dispatch fence before the write",
                    answered_in.as_millis()
                );
                if let Some(pending) = self.pending_update_handoff.as_mut() {
                    pending.commit_drain_started = None;
                    pending.held_commit = Some(HeldCommit { hold_fence });
                }
                self.finish_proven_handoff(crate::UpdateHandoffCompletion {
                    attempt_id,
                    nonce,
                    child_pid,
                    outcome: _outcome,
                    commit_fd,
                    reject,
                    reconcile: _reconcile,
                    detail: _detail,
                    input_drain_spins: 0,
                    child_death: _child_death,
                });
                return;
            }
        }
        let publications_settled =
            !claim || !admitted || crate::proxy::flush_publications(HANDOFF_INPUT_DRAIN_YIELD);
        if claim
            && admitted
            && publications_settled
            && let (Some(commit_fd), Some(proof)) = (commit_fd.as_ref(), proof)
        {
            let closed = self.close_uncarried_messages();
            if (claimed || closed > 0)
                && !matches!(
                    self.pending_update_handoff
                        .as_ref()
                        .map(|pending| pending.commit_flush),
                    Some(Some(false))
                )
            {
                self.flush_messages_log_within(HANDOFF_INPUT_DRAIN_YIELD);
            }
            let carried: Vec<aterm_session::SessionId> = self
                .store
                .read()
                .unwrap_or_else(|p| p.into_inner())
                .snapshot()
                .into_iter()
                .map(|h| h.sid.clone())
                .collect();
            if let Some(pid) = child_pid {
                crate::identity_claim::mark_successor(&carried, pid);
            }
            if let Some(host) = self.harness.as_ref() {
                host.suspend();
            }
            #[cfg(unix)]
            crate::fabric_launch::bye_at_handoff(&self.store, attempt_id);
            let person_stamps = nonce
                .as_deref()
                .and_then(|nonce| self.write_person_stamps_at_commit(nonce));
            let left_out = arbiter.connections_left_out();
            #[cfg(test)]
            if let Some(begin) = self.handoff_before_egress_fence.take() {
                begin();
            }
            let sinks = self
                .pending_update_handoff
                .as_ref()
                .map(|pending| handoff_sinks(&self.pool, &pending.live));
            let replies = crate::control::input_replies_of(&self.store);
            let commit_result = match sinks
                .as_deref()
                .and_then(|sinks| fence_handoff_egress(sinks, &replies))
            {
                None => None,
                Some(_egress) => {
                    #[cfg(target_os = "macos")]
                    self.activate_successor_at_write(child_pid);
                    Self::audit_connections_left_out(left_out);
                    #[cfg(target_os = "macos")]
                    self.note_launch_left_stage_landing();
                    Some(crate::seamless::commit_and_exit(commit_fd, proof))
                }
            };
            if commit_result.as_ref().is_some_and(Result::is_err) && !left_out.is_empty() {
                aterm_log::warn!(
                    "update apply: the Commit was not written, so the {} connection \
                     drop(s) just audited for it did not happen: this process keeps \
                     every one of those connections",
                    left_out.len()
                );
            }
            #[cfg(target_os = "macos")]
            if commit_result.as_ref().is_some_and(Result::is_err)
                && let Some((left, _)) = self.launch_left_stage
                && let Some(target) = self
                    .pending_update_handoff
                    .as_ref()
                    .map(|pending| pending.target_build)
            {
                if target == left {
                    aterm_log::warn!(
                        "update apply: the Commit was not written, so launch-owed build {left} \
                         did not land in place of this launch"
                    );
                } else {
                    aterm_log::warn!(
                        "update apply: the Commit was not written, so build {target} did not \
                         land in place of this launch"
                    );
                }
            }
            if let Some(host) = self.harness.as_ref() {
                host.resume();
            }
            if let Some(path) = person_stamps {
                let _ = std::fs::remove_file(path);
            }
            if let Some(pid) = child_pid {
                crate::identity_claim::withdraw_successor_markers(&carried, pid);
            }
            match commit_result {
                Some(Err(_)) => commit_write_failed = true,
                None => facts.egress_settled = false,
                Some(Ok(never)) => match never {},
            }
            let _ = arbiter.commit_failed_to_rejecting();
        } else if claim {
            aterm_log::warn!(
                "update apply: the Commit's admission, read again after the fence its claim \
                 was held through, refused it; a live video request it answered stays \
                 answered"
            );
            let _ = arbiter.commit_failed_to_rejecting();
        }

        let rejection = if !publications_settled {
            "discovery publications did not settle before the Commit".to_string()
        } else {
            handoff_rejection_reason(
                facts,
                &native_safety,
                commit_lost_arbiter,
                commit_write_failed,
            )
        };
        let activity_shaped = handoff_rejection_activity_shaped(facts);
        if activity_shaped && let Some(pending) = self.pending_update_handoff.as_mut() {
            pending.revoked_by_activity = true;
        }
        aterm_log::warn!("update apply: {rejection}; rejecting readerless child");
        #[cfg(test)]
        self.handoff_rejections.push(rejection.clone());
        let rejection_started =
            arbiter.try_begin_reject() || arbiter.phase() == crate::HandoffAttemptPhase::Rejecting;
        let delivery = rejection_started.then(|| deliver_handoff_rejection(reject));
        if delivery == Some(HandoffRejectDelivery::Disconnected)
            && child_pid.is_some()
            && self.proxy.is_some()
            && arbiter.claim_reaper(crate::HandoffReaperOwner::Emergency)
        {
            aterm_log::warn!(
                "update apply: handoff reaper channel closed; starting emergency reaper"
            );
            if let (Some(child_pid), Some(proxy)) = (child_pid, self.proxy.clone()) {
                let cleanup = HandoffWorkerCleanup {
                    parent_socket: self.handoff_parent_socket(),
                    reconcile: None,
                };
                let emergency_nonce = nonce.clone();
                let detail = format!("emergency reaper completed after: {rejection}");
                let thread_arbiter = arbiter.clone();
                let thread_cleanup = cleanup.clone();
                let thread_nonce = emergency_nonce.clone();
                let thread_detail = detail.clone();
                let thread_proxy = proxy.clone();
                let spawned = std::thread::Builder::new()
                    .name("aterm-handoff-emergency-reaper".to_string())
                    .spawn(move || {
                        crate::qos::set_self(crate::qos::Role::Background);
                        emergency_reap_and_report(
                            child_pid,
                            attempt_id,
                            &thread_arbiter,
                            &thread_cleanup,
                            thread_nonce,
                            thread_detail,
                            &thread_proxy,
                        );
                    });
                if spawned.is_err() {
                    emergency_reap_and_report(
                        child_pid,
                        attempt_id,
                        &arbiter,
                        &cleanup,
                        emergency_nonce,
                        detail,
                        &proxy,
                    );
                }
            }
        } else if !rejection_started {
            aterm_log::warn!(
                "update apply: Commit owns the attempt; rejection left child untouched"
            );
        }
    }

    #[cfg(unix)]
    fn answer_at_commit_claim(&mut self, child_pid: Option<u32>) -> std::time::Duration {
        let started = std::time::Instant::now();
        let (park_to_proof_ms, proof_to_commit_ms) = self
            .pending_update_handoff
            .as_ref()
            .map(|pending| {
                let now = std::time::Instant::now();
                let proof = pending.proof_ready_at.unwrap_or(now);
                (
                    proof.saturating_duration_since(pending.park_at).as_millis(),
                    now.saturating_duration_since(proof).as_millis(),
                )
            })
            .unwrap_or((0, 0));
        aterm_log::info!(
            "update apply: committing exact readerless handoff to child {:?} \
             (screen was frozen {park_to_proof_ms}ms park->proof + \
             {proof_to_commit_ms}ms proof->commit)",
            child_pid
        );
        if let Some(pending) = self
            .pending_update_handoff
            .as_ref()
            .filter(|pending| pending.mode.is_automatic())
            && let Some(ladder) = self
                .auto_apply_ladder
                .filter(|ladder| ladder.build == pending.target_build)
        {
            aterm_log::info!(
                "update auto-apply landed for build {}: committing {:.1} s after \
                 it was armed",
                ladder.build,
                ladder.armed_at.elapsed().as_secs_f64()
            );
        }
        let video = self.video_answer_before_commit(VIDEO_EXPORT_COMMIT_WAIT);
        if video.answered {
            aterm_log::info!("update apply: answered a live video recording or export at Commit");
        }
        if !video.export_settled || video.unwritten > 0 {
            aterm_log::warn!(
                "update apply: a video reply was still owed {} ms into Commit (export \
                 settled: {}, replies unwritten: {}); committing without it",
                VIDEO_EXPORT_COMMIT_WAIT.as_millis(),
                video.export_settled,
                video.unwritten
            );
        }
        if !matches!(
            self.pending_update_handoff
                .as_ref()
                .map(|pending| pending.commit_flush),
            Some(Some(false))
        ) {
            self.flush_messages_log_within(MESSAGES_FLUSH_COMMIT_WAIT);
        }
        started.elapsed()
    }

    #[cfg(target_os = "macos")]
    fn activate_successor_at_write(&mut self, child_pid: Option<u32>) {
        let Some(pid) = child_pid.and_then(|pid| i32::try_from(pid).ok()) else {
            return;
        };
        if !self.any_os_window_focused() {
            aterm_log::info!(
                "update apply: not activating the successor at Commit (no aterm \
                 window has focus at the write; the person keeps the app they are \
                 in)"
            );
            return;
        }
        #[cfg(test)]
        let accepted = {
            self.handoff_activations.push(pid);
            true
        };
        #[cfg(not(test))]
        let accepted = crate::app_launch_successor::activate_running(pid);
        aterm_log::info!(
            "update apply: activating the successor at Commit (an aterm \
             window has focus at the write): {}",
            if accepted {
                "accepted"
            } else {
                "refused by AppKit — the successor stays behind"
            }
        );
    }

    #[cfg(target_os = "macos")]
    fn note_launch_left_stage_landing(&self) {
        let (Some((left, launched)), Some(target)) = (
            self.launch_left_stage,
            self.pending_update_handoff
                .as_ref()
                .map(|pending| pending.target_build),
        ) else {
            return;
        };
        let after = launched.elapsed().as_secs();
        if target == left {
            aterm_log::info!(
                "update apply: writing the Commit that lands launch-owed build {left}, {after} s \
                 after launch"
            );
        } else {
            aterm_log::info!(
                "update apply: writing the Commit that lands build {target}, {after} s after \
                 launch, in place of the launch-owed build {left}"
            );
        }
    }

    /// The cached verdict's own reason for a refusal of this artifact, read by the same key as [`Self::cached_handoff_preverification`].
    #[cfg(unix)]
    #[must_use]
    pub(crate) fn cached_handoff_preverification_reason(
        &self,
        attempt: &crate::native_updater_service::ApplyAttemptTicket,
    ) -> Option<String> {
        self.read_fresh_preverification(attempt, |entry| entry.reason.clone())
    }

    /// How many physical handoff failures this process has booked against `attempt`'s artifact, which the freeze budget widens on.
    #[must_use]
    #[cfg(unix)]
    pub(crate) fn prior_physical_failures_of(
        &self,
        attempt: Option<&crate::native_updater_service::ApplyAttemptTicket>,
    ) -> u8 {
        let Some(attempt) = attempt else {
            return 0;
        };
        self.auto_apply_physical_retry
            .filter(|retry| retry.build == attempt.target_build())
            .map_or(0, |retry| retry.cycles)
    }

    /// How many times the fork lane's park has missed its freeze budget for `target_build`.
    #[must_use]
    #[cfg(unix)]
    pub(crate) fn fork_park_misses_of(&self, target_build: u64) -> u8 {
        self.fork_park_misses
            .filter(|(build, _)| *build == target_build)
            .map_or(0, |(_, misses)| misses)
    }

    #[cfg(unix)]
    fn fork_lane_policy_wait(
        &mut self,
        mode: crate::native_updater_service::ApplyMode,
        target_build: u64,
        policy_unread: bool,
    ) -> Option<crate::UpdateHandoffStartError> {
        self.preverify_read_wait(mode, target_build, policy_unread, PreverifyWaiter::ForkPark)
    }

    /// The cold lane answers to the pre-park verdict before it writes the quit layout and replaces this process.
    #[cfg(unix)]
    pub(crate) fn cold_lane_verdict_gate(
        &mut self,
        mode: crate::native_updater_service::ApplyMode,
        attempt: Option<&crate::native_updater_service::ApplyAttemptTicket>,
    ) -> Result<(), crate::UpdateHandoffStartError> {
        let Some(attempt) = attempt else {
            return Ok(());
        };
        let now = std::time::Instant::now();
        if let Some(known) = self.known_roster_wait(attempt, now) {
            if !known.held
                && let Some(dmg_sha256) =
                    crate::app_native::decode_dmg_sha256(attempt.target_dmg_sha256())
            {
                let _ = self.hold_for_roster_confirmation(
                    attempt.target_build(),
                    dmg_sha256,
                    &known.reason,
                    known.answered_at,
                    mode.is_automatic(),
                    true,
                );
            }
            return Err(crate::UpdateHandoffStartError::activity(format!(
                "{}; the app was not relaunched",
                known.reason
            )));
        }
        if !mode.is_automatic() {
            return Ok(());
        }
        self.answer_with_refused_cold_relaunch(attempt);
        match self.cached_handoff_preverification(attempt) {
            Some(true) => Ok(()),
            Some(false) => {
                let reason = self
                    .cached_handoff_preverification_reason(attempt)
                    .unwrap_or_else(|| "the staged update failed verification".to_string());
                if aterm_update::is_roster_confirmation_wait(&reason) {
                    if let Some(dmg_sha256) =
                        crate::app_native::decode_dmg_sha256(attempt.target_dmg_sha256())
                    {
                        let _ = self.hold_for_roster_confirmation(
                            attempt.target_build(),
                            dmg_sha256,
                            &reason,
                            now,
                            true,
                            true,
                        );
                    }
                    return Err(crate::UpdateHandoffStartError::activity(format!(
                        "{reason}; the app was not relaunched"
                    )));
                }
                Err(crate::UpdateHandoffStartError::failed(format!(
                    "{reason}; the app was not relaunched"
                )))
            }
            None => {
                let read = self
                    .handoff_preverify_in_flight
                    .as_ref()
                    .filter(|read| read.reads(attempt));
                if read.is_some_and(|read| {
                    read.ended_passing(now)
                        .is_some_and(aterm_update::is_roster_confirmation_wait)
                }) {
                    let started = crate::app_native::decode_dmg_sha256(attempt.target_dmg_sha256())
                        .is_some_and(|dmg_sha256| {
                            self.start_fresh_preverification(attempt.target_build(), dmg_sha256)
                        });
                    if started {
                        return Err(preverify_read_activity(
                            attempt.target_build(),
                            PreverifyWaiter::ColdRelaunch,
                        ));
                    }
                    return Ok(());
                }
                if let Some(reason) = read.and_then(|read| read.ended_passing(now)) {
                    return Err(crate::UpdateHandoffStartError::failed(format!(
                        "{reason}; the app was not relaunched, and the update is tried again \
                         later"
                    )));
                }
                if read.is_some_and(|read| read.running_for(attempt.target_build(), now)) {
                    return Err(preverify_read_activity(
                        attempt.target_build(),
                        PreverifyWaiter::ColdRelaunch,
                    ));
                }
                self.preverify_read_wait(
                    mode,
                    attempt.target_build(),
                    true,
                    PreverifyWaiter::ColdRelaunch,
                )
                .map_or(Ok(()), Err)
            }
        }
    }

    #[cfg(unix)]
    fn preverify_read_wait(
        &mut self,
        mode: crate::native_updater_service::ApplyMode,
        target_build: u64,
        unread: bool,
        waiter: PreverifyWaiter,
    ) -> Option<crate::UpdateHandoffStartError> {
        if !unread {
            return None;
        }
        let now = std::time::Instant::now();
        let until = match self.preverify_read_waited {
            Some((target, until))
                if target == target_build && now < until + crate::HANDOFF_PREVERIFY_FRESHNESS =>
            {
                until
            }
            _ => {
                let until = now + crate::HANDOFF_PREVERIFY_READ_CEILING;
                self.preverify_read_waited = Some((target_build, until));
                self.spawn_staged_handoff_preverification(target_build);
                until
            }
        };
        let reading = self
            .handoff_preverify_in_flight
            .as_ref()
            .is_some_and(|read| read.running_for(target_build, now));
        if !reading || now >= until {
            if reading {
                aterm_log::info!(
                    "update apply: build {target_build}'s pre-park verification is still marked \
                     running past the {} s {} waits for it — past the lock wait and \
                     verification budget that bound it, so its thread never ran; {} without it",
                    crate::HANDOFF_PREVERIFY_READ_CEILING.as_secs(),
                    waiter.lane(),
                    waiter.acting(),
                );
            }
            return None;
        }
        Some(if mode.is_automatic() {
            preverify_read_activity(target_build, waiter)
        } else {
            preverify_read_hold(target_build, waiter, until)
        })
    }

    /// Audits every connection the manifest left out as dropped (`action=drop origin=handoff`).
    #[cfg(unix)]
    pub(crate) fn audit_connections_left_out(rows: &[crate::session_store::ConnectionCarry]) {
        for row in rows {
            crate::session_edge_audit::emit(
                crate::session_edge_audit::EdgeAction::Drop,
                "handoff",
                &aterm_session::SessionId::new(row.src.clone()),
                &aterm_session::SessionId::new(row.dst.clone()),
                &row.op,
            );
        }
    }

    /// Hold a person's press for the candidate's pre-park read.
    #[cfg(unix)]
    pub(crate) fn hold_person_apply(&mut self, held: crate::HeldPersonApply) {
        aterm_log::info!(
            "update apply: a person's Install is held for build {}'s pre-park read, at most \
             {} ms more; its row stays up and it runs as soon as the read ends",
            held.target_build,
            held.until
                .saturating_duration_since(std::time::Instant::now())
                .as_millis()
        );
        self.person_apply_held = Some(held);
    }

    /// The event loop's next look at a held press.
    #[cfg(unix)]
    #[must_use]
    pub(crate) fn held_person_apply_deadline(
        &self,
        now: std::time::Instant,
    ) -> Option<std::time::Instant> {
        self.person_apply_held
            .map(|held| held.until.min(now + crate::app_native::PREVERIFY_READ_POLL))
    }

    /// Execute a held user-initiated update install (`Self::hold_person_apply`).
    #[cfg(unix)]
    pub(crate) fn run_held_person_apply(&mut self, now: std::time::Instant) {
        use crate::native_app::UpdateOutcome;
        let Some(held) = self.person_apply_held else {
            return;
        };
        let reading = self
            .handoff_preverify_in_flight
            .as_ref()
            .is_some_and(|read| read.running_for(held.target_build, now));
        if reading && now < held.until {
            return;
        }
        self.person_apply_held = None;
        let staged = self
            .native_updater_service
            .snapshot()
            .staged
            .as_ref()
            .map(|staged| staged.build);
        if staged != Some(held.target_build) {
            aterm_log::info!(
                "update apply: the Install held for build {}'s pre-park read is dropped: the \
                 staged build is now {staged:?}",
                held.target_build
            );
            self.retire_update_installing();
            self.record_update_switch_stopped(false, HELD_PRESS_SUPERSEDED);
            return;
        }
        aterm_log::info!(
            "update apply: build {}'s pre-park read {}; the Install a person pressed runs now",
            held.target_build,
            if reading {
                "outran its window"
            } else {
                "ended"
            }
        );
        let outcome = self.apply_native_update(crate::native_updater_service::ApplyMode::Immediate);
        if self.person_apply_held.is_some() {
            return;
        }
        let stopped = match &outcome {
            UpdateOutcome::Deferred { reason } => Some(reason.clone()),
            UpdateOutcome::Blocked { reasons } => {
                Some(reasons.join(crate::native_ui::FACT_SEPARATOR))
            }
            UpdateOutcome::Failed { message } | UpdateOutcome::CaptureRefused { message } => {
                Some(message.clone())
            }
            UpdateOutcome::Accepted | UpdateOutcome::InstalledNeedsRelaunch { .. } => None,
        };
        if let Some(why) = stopped {
            self.retire_update_installing();
            self.record_update_switch_stopped(false, &why);
        }
        self.surface_update_apply_outcome(HELD_PRESS_SOURCE, outcome, false);
    }

    #[cfg(unix)]
    fn fork_park_missed(
        &mut self,
        target_build: u64,
        reason: String,
    ) -> crate::UpdateHandoffStartError {
        let misses = self.fork_park_misses_of(target_build).saturating_add(1);
        self.fork_park_misses = Some((target_build, misses));
        aterm_log::warn!(
            "update apply: the fork lane's park missed its budget ({reason}); the next attempt \
             widens to the next rung (miss {misses}) and is retried on the activity spacing"
        );
        crate::UpdateHandoffStartError::park_missed(reason)
    }

    /// The cached verdict's reason keyed by the artifact alone, for the submission-time failure arm.
    #[cfg(unix)]
    #[must_use]
    pub(crate) fn cached_handoff_refusal_reason(&self, build: u64) -> Option<String> {
        let snapshot = self.native_updater_service.snapshot();
        let artifact = snapshot
            .staged
            .as_ref()
            .filter(|staged| staged.build == build)
            .map(|staged| staged.dmg_sha256.clone())?;
        let cached = self
            .handoff_preverified
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cached
            .as_ref()
            .filter(|entry| {
                !entry.passed
                    && entry.build == build
                    && entry.artifact == artifact
                    && entry.at.elapsed() < crate::HANDOFF_PREVERIFY_FRESHNESS
            })
            .and_then(|entry| entry.reason.clone())
    }

    #[cfg(unix)]
    fn answer_with_refused_cold_relaunch(
        &mut self,
        attempt: &crate::native_updater_service::ApplyAttemptTicket,
    ) {
        let Some(refused) = self.cold_relaunch_refused.take_if(|refused| {
            refused.build == attempt.target_build()
                && refused.commit == attempt.target_commit()
                && refused.artifact == attempt.target_dmg_sha256()
        }) else {
            return;
        };
        *self
            .handoff_preverified
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(crate::HandoffPreverification {
                at: std::time::Instant::now(),
                ..refused
            });
    }

    /// The cached pre-park verification verdict for this exact artifact, or `None` with no fresh entry.
    #[cfg(unix)]
    #[must_use]
    pub(crate) fn cached_handoff_preverification(
        &self,
        attempt: &crate::native_updater_service::ApplyAttemptTicket,
    ) -> Option<bool> {
        self.read_fresh_preverification(attempt, |entry| Some(entry.passed))
    }

    #[cfg(unix)]
    fn read_fresh_preverification<T>(
        &self,
        attempt: &crate::native_updater_service::ApplyAttemptTicket,
        read: impl FnOnce(&crate::HandoffPreverification) -> Option<T>,
    ) -> Option<T> {
        let cached = self
            .handoff_preverified
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cached
            .as_ref()
            .filter(|entry| {
                entry.build == attempt.target_build()
                    && entry.commit == attempt.target_commit()
                    && entry.artifact == attempt.target_dmg_sha256()
                    && entry.at.elapsed() < crate::HANDOFF_PREVERIFY_FRESHNESS
            })
            .and_then(read)
    }

    /// The successor's handoff policy as every park follows it, from the passed pre-verification of exactly the targeted artifact.
    #[cfg(unix)]
    #[must_use]
    pub(crate) fn park_policy(
        &self,
        attempt: Option<&crate::native_updater_service::ApplyAttemptTicket>,
    ) -> ParkPolicy {
        let Some(attempt) = attempt else {
            return ParkPolicy::default();
        };
        let cached = self
            .handoff_preverified
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cached
            .as_ref()
            .filter(|entry| {
                entry.passed
                    && entry.build == attempt.target_build()
                    && entry.commit == attempt.target_commit()
                    && entry.artifact == attempt.target_dmg_sha256()
            })
            .map_or_else(ParkPolicy::default, |entry| ParkPolicy {
                policy: entry.policy,
                known: true,
                grant_chunks: entry.grant_chunks,
            })
    }

    /// [`Self::park_policy`]'s policy by the cache's own key.
    #[cfg(all(test, unix))]
    #[must_use]
    pub(crate) fn handoff_policy_for(
        &self,
        build: u64,
        commit: &str,
        artifact: &str,
    ) -> Option<aterm_update_core::handoff_policy::HandoffPolicy> {
        self.park_policy(Some(
            &crate::native_updater_service::ApplyAttemptTicket::for_test(build, commit, artifact),
        ))
        .policy
    }

    /// Reduces one returned (non-ready) completion.
    #[cfg(unix)]
    pub(crate) fn reduce_returned_handoff_completion(
        &mut self,
        completion: crate::UpdateHandoffCompletion,
    ) -> Option<crate::DeferredHandoffTeardown> {
        let crate::UpdateHandoffCompletion {
            attempt_id: _attempt_id,
            nonce,
            child_pid: _child_pid,
            outcome,
            commit_fd: _commit_fd,
            reject: _reject,
            reconcile,
            detail,
            input_drain_spins: _input_drain_spins,
            child_death,
        } = completion;
        let parked = self
            .pending_update_handoff
            .take_if(|pending| pending.attempt_id == _attempt_id);
        #[cfg(any(target_os = "macos", all(test, unix)))]
        let prelaunch = self
            .update_handoff_prelaunch
            .take_if(|prelaunch| prelaunch.attempt_id == _attempt_id);
        #[cfg(target_os = "macos")]
        let withdraw_launch_unregistered = prelaunch
            .as_ref()
            .is_some_and(|prelaunch| prelaunch.booked_launch_unregistered);
        let pending = match parked {
            Some(parked) => {
                #[cfg(any(target_os = "macos", all(test, unix)))]
                let parked = {
                    let mut parked = parked;
                    if let Some(prelaunch) = prelaunch {
                        parked.teardown.merge(prelaunch.teardown);
                        parked.revoked_by_activity |= prelaunch.revoked_by_activity;
                    }
                    parked
                };
                crate::ReturnedHandoffRecord {
                    mode: parked.mode,
                    apply_attempt: parked.apply_attempt,
                    same_image: parked.same_image,
                    teardown: parked.teardown,
                    revoked_by_activity: parked.revoked_by_activity,
                    parked_live: Some(parked.live),
                }
            }
            #[cfg(any(target_os = "macos", all(test, unix)))]
            None => {
                let prelaunch = prelaunch?;
                crate::ReturnedHandoffRecord {
                    mode: prelaunch.mode,
                    apply_attempt: prelaunch.apply_attempt,
                    same_image: prelaunch.same_image,
                    teardown: prelaunch.teardown,
                    revoked_by_activity: prelaunch.revoked_by_activity,
                    parked_live: None,
                }
            }
            #[cfg(not(any(target_os = "macos", all(test, unix))))]
            None => return None,
        };
        let lane = crate::app_native::HandoffFailureLane::classify(
            pending.mode,
            outcome,
            child_death,
            pending.revoked_by_activity,
        );
        let teardown = pending.teardown;
        if let Some(live) = pending.parked_live.as_deref() {
            self.rollback_overlap(nonce.as_deref(), live);
        }
        self.drive_owed_document_journals();
        self.retire_update_installing();
        #[cfg(target_os = "macos")]
        if withdraw_launch_unregistered
            && self.own_launch_record != Some(crate::app_launch_successor::OwnLaunchRecord::Stale)
        {
            self.except_clear(&crate::update_words::launch_unregistered_entry(0).key());
        }
        #[cfg(target_os = "macos")]
        if pending
            .same_image
            .is_some_and(SameImageHandoff::renews_launch_record)
        {
            let routine = self.reduce_returned_launch_record_renewal(lane, &detail);
            self.record_update_switch_stopped(routine, &detail);
            return Some(teardown);
        }
        let debug_seam = matches!(pending.same_image, Some(SameImageHandoff::DebugSeam));
        let attempted_build = pending
            .apply_attempt
            .as_ref()
            .map(|attempt| attempt.target_build());
        let surfaced = match (pending.apply_attempt, reconcile) {
            (Some(attempt), Some(facts)) => self.finish_async_native_update_handoff(
                attempt,
                facts,
                format!("overlap handoff failed safely: {detail}"),
                lane,
            ),
            (None, _) => Some(crate::native_app::UpdateOutcome::Failed {
                message: format!("debug overlap handoff failed safely: {detail}"),
            }),
            (Some(attempt), None) => Some(self.abort_reaped_native_apply_before_reconcile(
                &attempt,
                format!("overlap handoff failed safely: {detail}"),
                lane,
            )),
        };
        let routine = matches!(
            lane,
            crate::app_native::HandoffFailureLane::ActivityRevoked
                | crate::app_native::HandoffFailureLane::AwaitsRosterConfirmation { .. }
        ) || (pending.mode.is_automatic()
            && attempted_build.is_some_and(|build| self.automatic_apply_retry_scheduled(build)));
        if !matches!(
            surfaced,
            Some(crate::native_app::UpdateOutcome::CaptureRefused { .. })
        ) {
            self.record_update_switch_stopped(routine, &detail);
        }
        if let Some(surfaced) = surfaced {
            let source = if pending.mode.is_automatic() {
                "automatic handoff"
            } else {
                "manual handoff"
            };
            if debug_seam {
                self.react_to_update_apply_outcome(source, surfaced, false);
            } else {
                self.surface_update_apply_outcome_for_target(
                    source,
                    surfaced,
                    false,
                    attempted_build.unwrap_or(0),
                );
            }
        }
        Some(teardown)
    }

    /// Overlap-handoff failure rollback.
    #[cfg(unix)]
    pub(crate) fn rollback_overlap(&mut self, _nonce: Option<&str>, live: &[(u64, i32, i32)]) {
        for (_, master, _) in live {
            let _ = aterm_pty::set_cloexec(*master, true);
        }
        self.lift_session_log_fences();
        self.owe_parked_winsizes(live);
        self.resume_deferred_readers_nonblocking();
        self.reseat_document_journals_after_rollback();
        if let Some(host) = self.harness.clone() {
            host.resume_restored();
            host.relaunch_stranded(&|sid| self.upgrade_place(sid));
        }
    }
}

impl App {
    #[cfg(unix)]
    fn owe_parked_winsizes(&mut self, live: &[(u64, i32, i32)]) {
        self.owed_parked_winsizes
            .extend(live.iter().map(|(id, master, _)| (*id, *master)));
    }

    /// Puts each owed PTY back at its engine's size, never waiting on an engine.
    #[cfg(unix)]
    pub(crate) fn reassert_owed_winsizes(&mut self) -> bool {
        let pool = &self.pool;
        self.owed_parked_winsizes.retain(|(id, master)| {
            let Some(session) = pool.get(*id).filter(|session| session.master == *master) else {
                return false;
            };
            let Some(engine) = crate::term_try_lock(&session.term) else {
                return true;
            };
            let (rows, cols, cell_px) =
                (engine.rows(), engine.cols(), engine.host_cell_pixel_size());
            drop(engine);
            aterm_pty::resize_with_cell_px(*master, rows, cols, cell_px);
            false
        });
        self.owed_parked_winsizes.is_empty()
    }
}

#[cfg(unix)]
const HELD_PRESS_SOURCE: &str = "manual, held for the candidate's read";

#[cfg(unix)]
const HELD_PRESS_SUPERSEDED: &str =
    "the update it was installing is no longer the one ready to install";

#[cfg(unix)]
fn preverify_read_activity(
    target_build: u64,
    waiter: PreverifyWaiter,
) -> crate::UpdateHandoffStartError {
    let reason = format!(
        "build {target_build}'s {} is being read before {} {}; the attempt after it ends \
         {} under it",
        waiter.what(),
        waiter.lane(),
        waiter.acts(),
        waiter.acts(),
    );
    aterm_log::info!("update apply: {reason}");
    crate::UpdateHandoffStartError::activity(reason)
}

#[cfg(unix)]
fn preverify_read_hold(
    target_build: u64,
    waiter: PreverifyWaiter,
    until: std::time::Instant,
) -> crate::UpdateHandoffStartError {
    let reason = format!(
        "build {target_build}'s {} is being read before {} {}; this Install runs as soon \
         as the read ends",
        waiter.what(),
        waiter.lane(),
        waiter.acts(),
    );
    aterm_log::info!("update apply: {reason}");
    crate::UpdateHandoffStartError::held_for_read(target_build, until, reason)
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PreverifyWaiter {
    ForkPark,
    ColdRelaunch,
}

#[cfg(unix)]
impl PreverifyWaiter {
    const fn lane(self) -> &'static str {
        match self {
            Self::ForkPark => "the fork lane",
            Self::ColdRelaunch => "the cold lane",
        }
    }

    const fn what(self) -> &'static str {
        match self {
            Self::ForkPark => "handoff policy",
            Self::ColdRelaunch => "pre-park verification",
        }
    }

    const fn acts(self) -> &'static str {
        match self {
            Self::ForkPark => "parks",
            Self::ColdRelaunch => "relaunches",
        }
    }

    const fn acting(self) -> &'static str {
        match self {
            Self::ForkPark => "parking",
            Self::ColdRelaunch => "relaunching",
        }
    }
}

/// The cold lane's refusal of a token carrying unsaved editor drafts, in the preflight's person-facing shape.
#[cfg(unix)]
pub(crate) fn cold_lane_carried_drafts_refusal(carried: usize) -> String {
    format!(
        "{} {carried} document(s) have uncheckpointed edits an update cannot carry: no \
         terminal session is open to carry the editor across, so replacing the app now would \
         close it",
        crate::app_native::DIRTY_DOCUMENTS_BLOCK,
    )
}

#[cfg(unix)]
impl crate::App {
    /// What the cold lane leaves its successor to reopen.
    pub(crate) fn cold_lane_layout(
        &self,
    ) -> Result<Option<crate::restore::RestoreManifest>, String> {
        if self.headless {
            return Ok(None);
        }
        let layout = self.capture_restore_manifest();
        if layout.is_empty() {
            return Ok(None);
        }
        if !self.config.restore_session_or_default() {
            return Err(
                "open tabs (documents or Settings) would close: session restore is \
                        off, so the relaunched app could not reopen them; the update applies \
                        once they are closed or a terminal tab opens"
                    .to_string(),
            );
        }
        Ok(Some(layout))
    }

    fn say_cold_lane_tabs_not_kept(&mut self, target_build: u64, cause: &str) {
        aterm_log::warn!(
            "update apply: the open windows could not be saved for the new build ({cause}); \
             the cold lane goes on without them"
        );
        let said = (target_build, cause.to_string());
        if self.cold_lane_layout_debt.not_kept_said.as_ref() != Some(&said) {
            self.record_message(crate::update_words::cold_lane_tabs_not_kept(cause));
            self.cold_lane_layout_debt.not_kept_said = Some(said);
        }
    }

    fn take_back_cold_lane_layout(&mut self, run: Option<String>, now: std::time::Instant) {
        let _ = self.take_back_cold_lane_layout_with(run, now, |run| {
            crate::restore::unwrite_quit_within(run, COLD_LANE_LAYOUT_LOCK_WAIT)
        });
    }

    fn take_back_cold_lane_layout_with(
        &mut self,
        run: Option<String>,
        now: std::time::Instant,
        unwrite: impl FnOnce(Option<&str>) -> Result<(), crate::restore::RestoreFault>,
    ) -> Result<(), crate::restore::RestoreFault> {
        let refused_before = self
            .cold_lane_layout_debt
            .owed_take_back
            .as_ref()
            .map_or(0, |owed| owed.refusals);
        match unwrite(run.as_deref()) {
            Err(fault) if !fault.published() => {
                let refusals = refused_before.saturating_add(1);
                let spacing = OwedTakeBack::spacing(refusals);
                aterm_log::warn!(
                    "update apply: the cold lane's layout was not taken back ({fault}); \
                     owed, tried again in {} s",
                    spacing.as_secs()
                );
                self.cold_lane_layout_debt.owed_take_back = Some(OwedTakeBack {
                    run,
                    retry_at: now + spacing,
                    refusals,
                });
                Err(fault)
            }
            _ => {
                if refused_before > 0 {
                    aterm_log::info!(
                        "update apply: the cold lane's layout was taken back after \
                         {refused_before} refusal(s)"
                    );
                }
                self.cold_lane_layout_debt.owed_take_back = None;
                Ok(())
            }
        }
    }

    fn cold_lane_layout_rewritten(&mut self) {
        self.cold_lane_layout_debt.owed_take_back = None;
    }

    /// When the event loop tries an owed take-back again ([`Self::take_back_cold_lane_layout`]).
    pub(crate) fn owed_layout_take_back_at(&self) -> Option<std::time::Instant> {
        self.cold_lane_layout_debt
            .owed_take_back
            .as_ref()
            .map(|owed| owed.retry_at)
    }

    /// The event loop's try of an owed take-back once due.
    pub(crate) fn retry_owed_layout_take_back(&mut self, now: std::time::Instant) {
        self.retry_owed_layout_take_back_with(now, |run| {
            crate::restore::unwrite_quit_within(run, COLD_LANE_LAYOUT_LOCK_WAIT)
        });
    }

    fn retry_owed_layout_take_back_with(
        &mut self,
        now: std::time::Instant,
        unwrite: impl FnOnce(Option<&str>) -> Result<(), crate::restore::RestoreFault>,
    ) {
        let Some(run) = self
            .cold_lane_layout_debt
            .owed_take_back
            .as_ref()
            .filter(|owed| now >= owed.retry_at)
            .map(|owed| owed.run.clone())
        else {
            return;
        };
        let _ = self.take_back_cold_lane_layout_with(run, now, unwrite);
    }

    fn settle_owed_layout_take_back_before_switch(
        &mut self,
        now: std::time::Instant,
    ) -> Result<(), String> {
        self.settle_owed_layout_take_back_before_switch_with(now, |run| {
            crate::restore::unwrite_quit_within(run, COLD_LANE_LAYOUT_LOCK_WAIT)
        })
    }

    fn settle_owed_layout_take_back_before_switch_with(
        &mut self,
        now: std::time::Instant,
        unwrite: impl FnOnce(Option<&str>) -> Result<(), crate::restore::RestoreFault>,
    ) -> Result<(), String> {
        let Some(run) = self
            .cold_lane_layout_debt
            .owed_take_back
            .as_ref()
            .map(|owed| owed.run.clone())
        else {
            return Ok(());
        };
        match self.take_back_cold_lane_layout_with(run, now, unwrite) {
            Ok(()) => Ok(()),
            Err(fault) if fault.passing() => Err(format!(
                "this window's layout, written for an update that did not start, could not \
                 be taken back from the restore file ({fault}); the update applies once it is"
            )),
            Err(fault) => {
                aterm_log::warn!(
                    "update apply: the switch goes on with the cold lane's earlier layout \
                     still in the restore file ({fault})"
                );
                Ok(())
            }
        }
    }

    /// At quit: an owed take-back is replaced by the quit's own write of this run's desk (`quit_wrote_its_desk`).
    pub(crate) fn settle_owed_layout_take_back_at_quit(&mut self, quit_wrote_its_desk: bool) {
        self.settle_owed_layout_take_back_at_quit_with(quit_wrote_its_desk, |run| {
            crate::restore::unwrite_quit_within(run, QUIT_TAKE_BACK_LOCK_WAIT)
        });
    }

    fn settle_owed_layout_take_back_at_quit_with(
        &mut self,
        quit_wrote_its_desk: bool,
        unwrite: impl FnOnce(Option<&str>) -> Result<(), crate::restore::RestoreFault>,
    ) {
        if quit_wrote_its_desk {
            self.cold_lane_layout_rewritten();
            return;
        }
        let Some(run) = self
            .cold_lane_layout_debt
            .owed_take_back
            .as_ref()
            .map(|owed| owed.run.clone())
        else {
            return;
        };
        let _ = self.take_back_cold_lane_layout_with(run, std::time::Instant::now(), unwrite);
    }
}

#[cfg(unix)]
const COLD_LANE_LAYOUT_LOCK_WAIT: std::time::Duration = std::time::Duration::from_millis(500);

#[cfg(unix)]
const OWED_TAKE_BACK_RETRY_FIRST: std::time::Duration = std::time::Duration::from_secs(2);

#[cfg(unix)]
const OWED_TAKE_BACK_RETRY_CAP: std::time::Duration = std::time::Duration::from_secs(60);

#[cfg(unix)]
const QUIT_TAKE_BACK_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// What the cold lane's quit layout still owes after an attempt that did not `exec`.
#[cfg(unix)]
#[derive(Debug, Default)]
pub(crate) struct ColdLaneLayoutDebt {
    not_kept_said: Option<(u64, String)>,
    owed_take_back: Option<OwedTakeBack>,
}

#[cfg(unix)]
#[derive(Clone, Debug, PartialEq, Eq)]
struct OwedTakeBack {
    run: Option<String>,
    retry_at: std::time::Instant,
    refusals: u32,
}

#[cfg(unix)]
impl OwedTakeBack {
    fn spacing(refusals: u32) -> std::time::Duration {
        let doublings = refusals.saturating_sub(1).min(16);
        OWED_TAKE_BACK_RETRY_FIRST
            .saturating_mul(1 << doublings)
            .min(OWED_TAKE_BACK_RETRY_CAP)
    }
}

#[cfg(unix)]
#[derive(Clone, Debug, PartialEq, Eq)]
enum LayoutLeft {
    Nothing,
    Written,
    NotKept(String),
}

#[cfg(unix)]
fn leave_cold_lane_layout(
    layout: Option<&crate::restore::RestoreManifest>,
    write: impl FnOnce(&crate::restore::RestoreManifest) -> Result<(), crate::restore::RestoreFault>,
) -> Result<LayoutLeft, crate::UpdateHandoffStartError> {
    let Some(layout) = layout else {
        return Ok(LayoutLeft::Nothing);
    };
    match write(layout) {
        Ok(()) => Ok(LayoutLeft::Written),
        Err(fault) if fault.published() => {
            aterm_log::warn!("update apply: the cold lane's layout is on disk, but {fault}");
            Ok(LayoutLeft::Written)
        }
        Err(fault) if fault.passing() => Err(crate::UpdateHandoffStartError::refused(format!(
            "could not save the open windows for the relaunch: {fault}; the update \
             applies once the layout can be saved"
        ))),
        Err(fault) => Ok(LayoutLeft::NotKept(fault.to_string())),
    }
}

#[cfg(unix)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativeUpdateSettingsDraftCarry",
        action = "ColdExec",
        project = "aterm_gui::settings_draft_carry_conformance::Rig::project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativeUpdateSettingsDraftCarry",
        action = "ParkOlder",
        project = "aterm_gui::settings_draft_carry_conformance::Rig::project"
    )
)]
pub(crate) fn lane_carry_refusal(
    token: &crate::app_native::NativeUpdateSafetyToken,
    lane: crate::native_update_admission::ApplyLane,
    running_build: u64,
    target_build: u64,
) -> Option<String> {
    use crate::native_update_admission::ApplyLane;
    match lane {
        ApplyLane::Cold if token.carried_drafts() > 0 => {
            Some(cold_lane_carried_drafts_refusal(token.carried_drafts()))
        }
        ApplyLane::Cold if token.carried_settings_drafts() > 0 => Some(format!(
            "{} {} unsaved Settings draft(s) an update cannot carry: no terminal session is \
             open to carry Settings across, so replacing the app now would close it",
            crate::app_native::SETTINGS_DRAFTS_BLOCK,
            token.carried_settings_drafts()
        )),
        ApplyLane::Cold if token.failed_checkpoints() > 0 => Some(format!(
            "{} {} document checkpoint(s) previously failed",
            crate::app_native::FAILED_CHECKPOINTS_BLOCK,
            token.failed_checkpoints()
        )),
        ApplyLane::Seamless
            if token.carried_settings_drafts() > 0 && target_build < running_build =>
        {
            Some(format!(
                "{} {} unsaved Settings draft(s) an update cannot carry: the build it installs \
                 is older than this one and would reopen Settings without them",
                crate::app_native::SETTINGS_DRAFTS_BLOCK,
                token.carried_settings_drafts()
            ))
        }
        ApplyLane::Cold | ApplyLane::Seamless => None,
    }
}

#[cfg(unix)]
fn carried_claims_of(
    manifest: &crate::session_store::SessionHandoff,
) -> Vec<crate::session_store::CarriedClaims> {
    manifest
        .sessions
        .iter()
        .map(crate::session_store::CarriedClaims::of_record)
        .collect()
}

#[cfg(unix)]
fn handed_sessions_only(
    mut manifest: crate::session_store::SessionHandoff,
    live: &[(u64, i32, i32)],
) -> crate::session_store::SessionHandoff {
    manifest.sessions.retain(|record| {
        live.iter()
            .any(|(local_id, _, _)| *local_id == record.local_id)
    });
    manifest
}

#[cfg(unix)]
fn stamp_fg_holders(
    mut manifest: crate::session_store::SessionHandoff,
    holders: &[(u64, i32)],
) -> crate::session_store::SessionHandoff {
    for record in &mut manifest.sessions {
        record.fg_holder = holders
            .iter()
            .find(|(local_id, _)| *local_id == record.local_id)
            .map(|(_, holder)| *holder)
            .filter(|holder| *holder > 0);
    }
    manifest
}

#[cfg(unix)]
fn stamp_status_seen(
    mut manifest: crate::session_store::SessionHandoff,
    rows: &[(u64, String)],
) -> crate::session_store::SessionHandoff {
    for record in &mut manifest.sessions {
        record.status_seen = rows
            .iter()
            .find(|(local_id, _)| *local_id == record.local_id)
            .map(|(_, row)| row.clone());
    }
    manifest
}

#[cfg(unix)]
fn stamp_autopilot_end(
    mut manifest: crate::session_store::SessionHandoff,
    rows: &[(u64, String)],
) -> crate::session_store::SessionHandoff {
    for record in &mut manifest.sessions {
        record.autopilot_end = rows
            .iter()
            .find(|(local_id, _)| *local_id == record.local_id)
            .map(|(_, row)| row.clone());
    }
    manifest
}

#[cfg(unix)]
fn stamp_fleet_told(
    mut manifest: crate::session_store::SessionHandoff,
    rows: Vec<String>,
) -> crate::session_store::SessionHandoff {
    manifest.fleet_told = rows;
    manifest
}

#[cfg(unix)]
fn stamp_journal_keys(
    mut manifest: crate::session_store::SessionHandoff,
    keys: &[(u64, (u32, u64))],
) -> crate::session_store::SessionHandoff {
    for record in &mut manifest.sessions {
        record.journal_key = keys
            .iter()
            .find(|(local_id, _)| *local_id == record.local_id)
            .map(|(_, key)| crate::session_store::journal_key_row(*key));
    }
    manifest
}

#[cfg(unix)]
fn stamp_render_debts(
    mut manifest: crate::session_store::SessionHandoff,
    debts: &[(u64, aterm_core::terminal::RenderDebt)],
) -> crate::session_store::SessionHandoff {
    for record in &mut manifest.sessions {
        let debt = debts
            .iter()
            .find(|(local_id, _)| *local_id == record.local_id)
            .map(|(_, debt)| *debt);
        record.carry_render_debt(debt);
    }
    manifest
}

#[cfg(unix)]
fn update_admission_refusal(
    reason: crate::native_update_admission::AdmissionBlock,
    facts: crate::native_update_admission::AdmissionFacts,
    unavailable: Option<HandoffUnavailable>,
) -> crate::UpdateHandoffStartError {
    let message = if reason == crate::native_update_admission::AdmissionBlock::LivePtysNeedSeamless
        && facts.foreground_jobs <= facts.live_ptys
        && let Some(why) = unavailable
    {
        format!(
            "Update kept {} live terminal session(s), including {} foreground job(s), running: seamless handoff unavailable because {}.{}",
            facts.live_ptys,
            facts.foreground_jobs,
            why.cause(),
            why.remedy(),
        )
    } else if reason == crate::native_update_admission::AdmissionBlock::KeptImageNeedsTerminal
        && let Some(why) = unavailable
    {
        format!(
            "The update installs at the next launch: seamless handoff unavailable because {}, and this aterm never restarts in place.{}",
            why.cause(),
            why.remedy(),
        )
    } else {
        reason.message(facts)
    };
    crate::UpdateHandoffStartError::refused(message)
}

#[cfg(all(test, unix))]
mod admission_refusal_detail_tests {
    use super::{HandoffUnavailable, update_admission_refusal};
    use crate::native_update_admission::{
        AdmissionBlock, AdmissionDecision, AdmissionFacts, classify,
    };

    fn live_facts() -> AdmissionFacts {
        AdmissionFacts {
            staged_verified: true,
            seamless_capable: false,
            native_state_certified: true,
            live_ptys: 3,
            foreground_jobs: 2,
            unknown_foregrounds: 0,
            image_kept: false,
        }
    }

    #[test]
    fn live_session_refusal_retains_exact_handoff_cause_and_remedy() {
        for unavailable in [
            HandoffUnavailable::UnownedControlSocket,
            HandoffUnavailable::Headless,
        ] {
            let (cause, remedy) = (unavailable.cause(), unavailable.remedy());
            for (foreground_jobs, unknown_foregrounds) in [(2, 0), (0, 1), (1, 2), (0, 3)] {
                let facts = AdmissionFacts {
                    foreground_jobs,
                    unknown_foregrounds,
                    ..live_facts()
                };
                let AdmissionDecision::Block(reason) = classify(facts) else {
                    panic!("live terminals without seamless handoff must block");
                };
                let refusal = update_admission_refusal(reason, facts, Some(unavailable));
                assert!(
                    refusal.is_refused(),
                    "a refusal cannot increment a failure streak"
                );
                let message = refusal.into_message();
                assert!(message.contains(cause), "{facts:?}: {message}");
                assert!(message.contains(remedy), "{facts:?}: {message}");
                assert!(
                    message.chars().count() <= 400,
                    "durable health truncates at 400 characters"
                );
                assert!(!reason.message(facts).contains(cause));
            }
            let kept = AdmissionFacts {
                live_ptys: 0,
                foreground_jobs: 0,
                image_kept: true,
                ..live_facts()
            };
            let refusal = update_admission_refusal(
                AdmissionBlock::KeptImageNeedsTerminal,
                kept,
                Some(unavailable),
            );
            assert!(refusal.is_refused());
            let message = refusal.into_message();
            assert!(message.contains(cause), "{message}");
        }
    }

    #[test]
    fn unrelated_admission_refusals_keep_the_primary_reason() {
        let base = live_facts();
        for (facts, expected) in [
            (
                AdmissionFacts {
                    staged_verified: false,
                    ..base
                },
                AdmissionBlock::UnverifiedStage,
            ),
            (
                AdmissionFacts {
                    native_state_certified: false,
                    ..base
                },
                AdmissionBlock::NativeStateUncertified,
            ),
            (
                AdmissionFacts {
                    live_ptys: 0,
                    foreground_jobs: 0,
                    unknown_foregrounds: 1,
                    ..base
                },
                AdmissionBlock::ForegroundProbeUnknown,
            ),
            (
                AdmissionFacts {
                    foreground_jobs: 4,
                    ..base
                },
                AdmissionBlock::LivePtysNeedSeamless,
            ),
        ] {
            assert_eq!(classify(facts), AdmissionDecision::Block(expected));
            let refusal =
                update_admission_refusal(expected, facts, Some(HandoffUnavailable::Headless));
            assert!(refusal.is_refused());
            assert_eq!(refusal.into_message(), expected.message(facts));
        }
        assert_eq!(
            update_admission_refusal(AdmissionBlock::LivePtysNeedSeamless, base, None)
                .into_message(),
            AdmissionBlock::LivePtysNeedSeamless.message(base),
        );
    }
}

#[cfg(unix)]
#[must_use]
fn readiness_proof_matches(
    expected: crate::seamless::AdoptionProof,
    wire: &[u8; crate::seamless::READY_WIRE_LEN],
) -> bool {
    crate::seamless::AdoptionProof::from_wire(wire) == Some(expected)
}

#[cfg(unix)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ReadyPollAction {
    SessionDied,
    ReadProof,
    NoProgress,
}

#[cfg(unix)]
#[must_use]
fn classify_ready_poll(pollfds: &[libc::pollfd]) -> ReadyPollAction {
    let dead = libc::POLLHUP | libc::POLLERR | libc::POLLNVAL;
    if pollfds.iter().skip(1).any(|pfd| pfd.revents & dead != 0) {
        return ReadyPollAction::SessionDied;
    }
    let proof_readable = libc::POLLIN | libc::POLLHUP | libc::POLLERR | libc::POLLNVAL;
    if pollfds
        .first()
        .is_some_and(|proof| proof.revents & proof_readable != 0)
    {
        return ReadyPollAction::ReadProof;
    }
    ReadyPollAction::NoProgress
}

#[cfg(target_os = "macos")]
struct PrelaunchArgs {
    attempt_id: u64,
    build: u64,
    mode: crate::native_updater_service::ApplyMode,
    apply_attempt: Option<crate::native_updater_service::ApplyAttemptTicket>,
    same_image: Option<SameImageHandoff>,
    target_build: u64,
    target_commit: String,
    command: aterm_uds::spawnfd::StdCommand,
    verify_staged_candidate: bool,
    grant_chunks: bool,
    installed_activation: bool,
    bundle: Option<std::path::PathBuf>,
    cancel: std::sync::mpsc::SyncSender<()>,
    cancelled: std::sync::mpsc::Receiver<()>,
    job_tx: std::sync::mpsc::SyncSender<HandoffWorkerJob>,
    reconcile_worker: crate::app_native::NativeUpdateReconcileSender,
    reconcile_ticket: crate::app_native::NativeUpdateReconcileTicket,
}

/// The facts the park gate reads, measured on the main thread right before it runs, in a shape a test can enumerate.
#[cfg(any(target_os = "macos", all(test, unix)))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ParkGateFacts {
    pub(crate) mode: crate::native_updater_service::ApplyMode,
    pub(crate) phase: crate::native_update_auto_intent::ApplyPhase,
    pub(crate) activity: crate::native_update_auto_intent::ActivityFacts,
    pub(crate) masters_quiet: bool,
    pub(crate) masters_alive: bool,
    pub(crate) land_waits: u8,
    pub(crate) land_gate_relaxed: bool,
    pub(crate) held_for: std::time::Duration,
    pub(crate) recording: bool,
}

/// What the park gate decides.
#[cfg(any(target_os = "macos", all(test, unix)))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ParkGate {
    Park,
    Wait(&'static str),
    StandDown(&'static str),
}

/// When the terminal may be frozen.
#[cfg(any(target_os = "macos", all(test, unix)))]
#[must_use]
pub(crate) fn prelaunch_park_admitted(
    facts: ParkGateFacts,
    hold_cap: Option<std::time::Duration>,
) -> ParkGate {
    use crate::native_updater_service::ApplyMode;
    if matches!(facts.mode, ApplyMode::Immediate) {
        return ParkGate::Park;
    }
    if hold_cap.is_some_and(|cap| facts.held_for >= cap) {
        return ParkGate::StandDown(
            "the terminal never offered a moment to pause in within the hold cap",
        );
    }
    if let Some(reason) =
        crate::native_update_auto_intent::automatic_park_refusal(facts.phase, facts.activity)
    {
        return ParkGate::Wait(reason);
    }
    if let Some(reason) =
        recording_park_refusal(facts.mode, facts.phase, facts.recording, facts.held_for)
    {
        return ParkGate::Wait(reason);
    }
    if !facts.masters_alive {
        return ParkGate::Wait("a session's command has exited but its pane is still open");
    }
    if !facts.masters_quiet
        && !(facts.phase == crate::native_update_auto_intent::ApplyPhase::Land
            && (facts.land_waits >= PRELAUNCH_LAND_MAX_WAITS || facts.land_gate_relaxed))
    {
        return ParkGate::Wait("a session has output waiting that its reader has not taken");
    }
    ParkGate::Park
}

/// How long a live `video` take may hold an automatic park before `Land`.
#[cfg(unix)]
pub(crate) const VIDEO_PARK_WAIT_MAX: std::time::Duration =
    crate::control::control_media::VIDEO_MAX_DURATION;

#[cfg(unix)]
const _: () = assert!(
    VIDEO_PARK_WAIT_MAX.as_secs() + 10 < PRELAUNCH_HOLD_MAX.as_secs(),
    "a recording's wait must come due well inside the hold, or a take stands the successor \
     down instead of being answered"
);

/// Whether a live `video` take holds this park.
#[cfg(unix)]
#[must_use]
pub(crate) fn recording_park_refusal(
    mode: crate::native_updater_service::ApplyMode,
    phase: crate::native_update_auto_intent::ApplyPhase,
    recording: bool,
    held_for: std::time::Duration,
) -> Option<&'static str> {
    (recording
        && mode.is_automatic()
        && phase != crate::native_update_auto_intent::ApplyPhase::Land
        && held_for < VIDEO_PARK_WAIT_MAX)
        .then_some("a video recording is running")
}

/// The detail a hold-cap stand-down carries.
#[cfg(any(target_os = "macos", all(test, unix)))]
#[must_use]
pub(crate) fn hold_cap_stand_down_detail(
    reason: &str,
    held_for: std::time::Duration,
    last_wait: Option<&str>,
) -> String {
    let held = held_for.as_secs();
    match last_wait {
        Some(waiting_for) => format!(
            "{reason} ({held} s after the successor dialled; the park was last held back \
             because {waiting_for})"
        ),
        None => format!("{reason} ({held} s after the successor dialled)"),
    }
}

/// How many consecutive `Wait` answers the gate gives in `Land` before parking over queued output.
#[cfg(unix)]
pub(crate) const PRELAUNCH_LAND_MAX_WAITS: u8 = 50;

#[cfg(unix)]
const _: () = assert!(
    PRELAUNCH_PARK_RETRY.as_millis() * (PRELAUNCH_LAND_MAX_WAITS as u128)
        < PRELAUNCH_HOLD_MAX.as_millis(),
    "the Land relaxation must come due well inside the hold it is spending, or a \
     flooded master still stands the successor down"
);

/// The hold cap for `mode`.
#[cfg(any(target_os = "macos", all(test, unix)))]
#[must_use]
pub(crate) fn prelaunch_hold_cap(
    mode: crate::native_updater_service::ApplyMode,
) -> Option<std::time::Duration> {
    mode.is_automatic().then_some(PRELAUNCH_HOLD_MAX)
}

/// One run of the park gate for a prelaunched attempt.
#[cfg(unix)]
#[derive(Debug)]
pub(crate) enum ParkAttempt {
    #[cfg(any(target_os = "macos", all(test, unix)))]
    Parked,
    #[cfg(any(target_os = "macos", all(test, unix)))]
    NotYet(&'static str),
    #[cfg(any(target_os = "macos", all(test, unix)))]
    Pausing,
    Missed(String),
    #[cfg(any(target_os = "macos", all(test, unix)))]
    MissedMidSequence(String),
    Refused(CaptureRefusal),
    #[cfg(any(target_os = "macos", all(test, unix)))]
    Failed(HandoffStandDown),
}

/// A deterministic refusal of the park's capture, naming the session that held the update back if any.
#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CaptureRefusal {
    pub(crate) local_id: Option<u64>,
    pub(crate) cause: String,
}

#[cfg(unix)]
impl std::fmt::Display for CaptureRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.local_id {
            Some(local_id) => write!(
                formatter,
                "the park's capture refused session {local_id}: {}",
                self.cause
            ),
            None => write!(formatter, "the park's capture refused: {}", self.cause),
        }
    }
}

/// Which lane a capture failure belongs to, typed on the variant.
#[cfg(unix)]
#[must_use]
pub(crate) fn classify_capture_failure(failure: &CaptureFailure) -> ParkAttempt {
    match failure {
        CaptureFailure::Deadline { .. }
        | CaptureFailure::EngineBusy
        | CaptureFailure::Storage
        | CaptureFailure::MidSequence { .. } => ParkAttempt::Missed(failure.to_string()),
        CaptureFailure::Refused { local_id, .. } => ParkAttempt::Refused(CaptureRefusal {
            local_id: *local_id,
            cause: failure.to_string(),
        }),
    }
}

/// The park's commitment to its post-park handoff layout.
#[cfg(unix)]
pub(crate) fn commit_handoff_layout(
    layout: &crate::restore::RestoreManifest,
) -> Result<[u8; 32], CaptureRefusal> {
    if let Some(cause) = layout.why_not_whole() {
        return Err(CaptureRefusal {
            local_id: None,
            cause,
        });
    }
    crate::seamless::layout_digest(layout).ok_or_else(|| CaptureRefusal {
        local_id: None,
        cause: "handoff layout could not be committed canonically".to_string(),
    })
}

#[cfg(all(test, unix))]
impl App {
    pub(crate) fn capture_park_outcome_for_conformance(
        &mut self,
        caps: crate::seamless::WireCaps,
    ) -> Result<Vec<u64>, ParkAttempt> {
        let live: Vec<(u64, i32, i32)> = self.handoff_live_sessions();
        let window = std::time::Duration::from_secs(30);
        self.capture_parked_screens(
            &live,
            std::time::Instant::now() + window,
            window.as_millis(),
            window / 2,
            caps,
            aterm_update_core::handoff_policy::CarryCeiling::Full,
        )
        .map(|parked| parked.repaint)
        .map_err(|failure| classify_capture_failure(&failure))
    }
}

/// What one park hands the worker, projected from a whole [`App`] as the fork lane does.
#[cfg(all(test, unix))]
pub(crate) struct ParkedDeskForTest {
    pub(crate) live: Vec<(u64, i32, i32)>,
    pub(crate) manifest: crate::session_store::SessionHandoff,
    pub(crate) window: Option<crate::session_store::WindowCarry>,
    pub(crate) screens: Vec<(u64, aterm_core::terminal::TerminalCheckpoint)>,
    pub(crate) carries: Vec<crate::handoff_carry::CarrySource>,
    pub(crate) screen_digest: [u8; 32],
    pub(crate) repaint: Vec<u64>,
    pub(crate) history_heads: Vec<crate::handoff_history::HistoryHead>,
    pub(crate) history: crate::handoff_history::HistoryPlan,
    pub(crate) layout: crate::restore::RestoreManifest,
    pub(crate) layout_digest: Option<[u8; 32]>,
    pub(crate) held: Vec<crate::seamless::HeldScreen>,
}

#[cfg(all(test, unix))]
impl App {
    /// The park described above, under the successor's policy for `attempt` ([`Self::park_policy`]).
    pub(crate) fn park_desk_for_test(
        &mut self,
        attempt: Option<&crate::native_updater_service::ApplyAttemptTicket>,
    ) -> Result<ParkedDeskForTest, CaptureFailure> {
        let target_build = attempt.map_or_else(crate::running_build_number, |attempt| {
            attempt.target_build()
        });
        let ceiling = self.park_policy(attempt).carry_ceiling();
        let live = self.handoff_live_sessions();
        let manifest = handed_sessions_only(
            crate::session_store::SessionHandoff::from_store(
                &self
                    .store
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            ),
            &live,
        );
        let window = self.park_window_carry(target_build);
        let budget = std::time::Duration::from_secs(30);
        let ParkedScreens {
            screens,
            carries,
            screen_digest,
            repaint,
            fg_holders,
            render_debts,
            history_heads,
            held,
        } = self.capture_parked_screens(
            &live,
            std::time::Instant::now() + budget,
            budget.as_millis(),
            budget / 2,
            crate::seamless::WireCaps::for_target(target_build),
            ceiling,
        )?;
        let manifest = stamp_fg_holders(manifest, &fg_holders);
        let manifest = stamp_render_debts(manifest, &render_debts);
        let manifest = stamp_journal_keys(manifest, &self.journal_keys());
        let manifest = stamp_status_seen(manifest, &self.status_carry_rows());
        let manifest = stamp_autopilot_end(manifest, &self.end_carry_rows());
        let manifest = stamp_fleet_told(manifest, self.fleet_told_rows());
        let layout = self.capture_handoff_layout();
        let layout_digest = crate::seamless::layout_digest(&layout);
        let history = crate::handoff_history::HistoryPlan::deferred_under(ceiling, || {
            self.history_sessions(target_build)
        });
        Ok(ParkedDeskForTest {
            live,
            manifest,
            window,
            screens,
            carries,
            screen_digest,
            repaint,
            history_heads,
            history,
            layout,
            layout_digest,
            held,
        })
    }
}

/// The stand-down a [`ParkAttempt::Refused`] park sends.
#[cfg(any(target_os = "macos", all(test, unix)))]
#[must_use]
pub(crate) fn capture_refusal_stand_down(refusal: &CaptureRefusal) -> HandoffStandDown {
    HandoffStandDown {
        outcome: crate::UpdateHandoffOutcome::CaptureRefused,
        detail: format!("{refusal}; the lane retries once the desk changes"),
    }
}

#[cfg(unix)]
const PRELAUNCH_PARK_RETRY: std::time::Duration = std::time::Duration::from_millis(20);

#[cfg(any(target_os = "macos", all(test, unix)))]
const PRELAUNCH_PAUSE_POLL: std::time::Duration = std::time::Duration::from_millis(1);

#[cfg(any(target_os = "macos", all(test, unix)))]
const PRELAUNCH_REPARK_DELAY: std::time::Duration = std::time::Duration::from_millis(500);

/// How many times one prelaunched attempt re-parks before standing its successor down.
#[cfg(any(target_os = "macos", all(test, unix)))]
pub(crate) const PRELAUNCH_MAX_PARK_MISSES: u8 = 2;

/// What one park that missed its freeze budget costs the prelaunched attempt.
#[cfg(any(target_os = "macos", all(test, unix)))]
#[derive(Debug)]
pub(crate) enum ParkMissDisposition {
    Repark { misses: u8, reason: String },
    RetryMidSequence { reparks: u8, reason: String },
    StandDown(HandoffStandDown),
}

/// How many mid-sequence misses one prelaunched attempt re-parks on for free, at the gate's cadence without climbing the rung.
#[cfg(any(target_os = "macos", all(test, unix)))]
pub(crate) const PRELAUNCH_MAX_MID_SEQUENCE_REPARKS: u8 = 32;

#[cfg(unix)]
const OPEN_LINK_STATE: &str = "Osc8HyperlinkOpen";

#[cfg(unix)]
const HANDOFF_INPUT_DRAIN_YIELD: std::time::Duration = std::time::Duration::from_millis(2);

#[cfg(any(target_os = "macos", all(test, unix)))]
const _: () = assert!(
    (PRELAUNCH_MAX_MID_SEQUENCE_REPARKS as u64) * 20 <= 1_000
        && PRELAUNCH_PARK_RETRY.as_millis() == 20
);

/// A mid-sequence miss.
#[cfg(any(target_os = "macos", all(test, unix)))]
#[must_use]
pub(crate) fn mid_sequence_miss_disposition(
    misses: u8,
    reparks: u8,
    reason: String,
) -> ParkMissDisposition {
    if reparks < PRELAUNCH_MAX_MID_SEQUENCE_REPARKS {
        ParkMissDisposition::RetryMidSequence {
            reparks: reparks.saturating_add(1),
            reason,
        }
    } else {
        park_miss_disposition(misses, reason)
    }
}

/// A park miss is a fact about the machine's moment, never about the bytes.
#[cfg(any(target_os = "macos", all(test, unix)))]
#[must_use]
pub(crate) fn park_miss_disposition(misses: u8, reason: String) -> ParkMissDisposition {
    if misses >= PRELAUNCH_MAX_PARK_MISSES {
        ParkMissDisposition::StandDown(HandoffStandDown {
            outcome: crate::UpdateHandoffOutcome::ActivityRevoked,
            detail: format!(
                "the park missed its budget on every rung ({reason}); the machine is busy, \
                 the candidate is not in question, and the ladder retries"
            ),
        })
    } else {
        ParkMissDisposition::Repark {
            misses: misses.saturating_add(1),
            reason,
        }
    }
}

/// How long Commit waits at most for every live `video` request it answers to reach its client before `_exit`.
#[cfg(unix)]
pub(crate) const VIDEO_EXPORT_COMMIT_WAIT: std::time::Duration = std::time::Duration::from_secs(1);

/// How long the seamless parent at Commit, and the cold lane before `exec`, wait for the message-log writer.
#[cfg(unix)]
pub(crate) const MESSAGES_FLUSH_COMMIT_WAIT: std::time::Duration =
    std::time::Duration::from_millis(250);

#[cfg(all(unix, not(test)))]
const COLD_EXEC_REAP_WAIT: std::time::Duration = std::time::Duration::from_secs(1);
#[cfg(all(unix, test))]
const COLD_EXEC_REAP_WAIT: std::time::Duration = aterm_spec::wait::MUST_HAPPEN;

/// How long a dialled successor is held for a quiet moment before the attempt stands down (activity-revoked, retried later).
#[cfg(unix)]
pub(crate) const PRELAUNCH_HOLD_MAX: std::time::Duration = std::time::Duration::from_secs(120);

#[cfg(unix)]
const _: () = assert!(
    crate::native_update_auto_intent::LANDS_WITHIN.as_secs() + 10 < PRELAUNCH_HOLD_MAX.as_secs()
);

#[cfg(any(target_os = "macos", all(test, unix)))]
#[must_use]
fn handoff_proof_deadline(
    mode: crate::native_updater_service::ApplyMode,
    prior_physical_failures: u8,
    park_at: std::time::Instant,
) -> std::time::Instant {
    use crate::native_updater_service::ApplyMode;
    let ms = aterm_types::dev_seam!("ATERM_HANDOFF_PROOF_TIMEOUT_MS")
        .and_then(|value| value.to_str()?.parse::<u64>().ok())
        .unwrap_or(match mode {
            ApplyMode::Immediate => 30_000,
            ApplyMode::Automatic | ApplyMode::AutomaticPastGrace => match prior_physical_failures {
                0 => 3_000,
                _ => 6_000,
            },
        })
        .clamp(1_000, 120_000);
    park_at + std::time::Duration::from_millis(ms)
}

#[cfg(unix)]
#[must_use]
fn handoff_ready_deadline() -> std::time::Instant {
    let timeout_ms = aterm_types::dev_seam!("ATERM_HANDOFF_READY_TIMEOUT_MS")
        .and_then(|v| v.to_str()?.parse::<u64>().ok())
        .unwrap_or(30_000)
        .clamp(1_000, 120_000);
    std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms)
}

#[cfg(target_os = "macos")]
#[derive(Debug, PartialEq, Eq)]
enum LaunchedDialEnd {
    Cancelled,
    SuccessorExited(PreDialDeath),
    CodeRefused {
        dialer: HandoffCandidate,
        refusal: crate::handoff_rendezvous::CodeRefusal,
        death: PreDialDeath,
    },
    ExitedWhileHeld {
        dialer: HandoffCandidate,
        death: PreDialDeath,
    },
    Rendezvous(crate::handoff_rendezvous::RendezvousError),
}

#[cfg(target_os = "macos")]
fn code_refused_dialer_was_launched(dialer: HandoffCandidate, death: PreDialDeath) -> bool {
    match death {
        PreDialDeath::NamedProcessGone { candidate, .. } => {
            candidate.pid == dialer.pid
                && (candidate.birth.is_none()
                    || dialer.birth.is_none()
                    || candidate.birth == dialer.birth)
        }
        PreDialDeath::TerminatedBeforeCheckIn => {
            dialer.birth.is_some() && dialer.identity() != HandoffCandidateIdentity::Corroborated
        }
    }
}

#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PreDialDeath {
    TerminatedBeforeCheckIn,
    NamedProcessGone {
        candidate: HandoffCandidate,
        death: crate::ChildDeathEvidence,
    },
}

#[cfg(target_os = "macos")]
impl PreDialDeath {
    const fn evidence(self) -> crate::ChildDeathEvidence {
        match self {
            Self::TerminatedBeforeCheckIn => crate::ChildDeathEvidence::Unobserved,
            Self::NamedProcessGone { death, .. } => death,
        }
    }
}

#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DialWaitAbort {
    Cancelled,
    SuccessorExited(PreDialDeath),
}

#[cfg(target_os = "macos")]
#[derive(Default)]
struct NamedSuccessorWatch {
    seen: Option<(HandoffCandidate, Option<CandidateExitWatch>)>,
}

#[cfg(target_os = "macos")]
impl NamedSuccessorWatch {
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "native_update_launched_dial_wait",
            action = "EndOnNamedExit",
            project = "aterm_gui::app_update_handoff::launched_dial_wait_conformance::project"
        )
    )]
    fn gone(
        &mut self,
        named: Option<i32>,
    ) -> Option<(HandoffCandidate, crate::ChildDeathEvidence)> {
        if self.seen.is_none() {
            let pid = u32::try_from(named?).ok()?;
            let candidate = HandoffCandidate::of_named_pid(pid);
            self.seen = Some((candidate, CandidateExitWatch::watch(pid)));
        }
        let (candidate, watch) = self.seen.as_ref()?;
        if let Some(status) = watch.as_ref().and_then(CandidateExitWatch::exit_status) {
            return Some((*candidate, handoff_child_death(true, Some(status))));
        }
        if handoff_candidate_terminated(*candidate) {
            let status = watch.as_ref().and_then(CandidateExitWatch::exit_status);
            return Some((*candidate, handoff_child_death(true, status)));
        }
        None
    }
}

#[cfg(target_os = "macos")]
fn launched_dialer_code(
    successor_runs_candidate: bool,
    team: &str,
    bundle: &std::path::Path,
    candidate_identifier: impl FnOnce() -> Result<String, String>,
) -> Result<crate::handoff_rendezvous::DialerCode, String> {
    if successor_runs_candidate && !team.is_empty() {
        let release = crate::handoff_rendezvous::RELEASE_SIGNING_IDENTIFIER;
        let identifier = candidate_identifier()
            .map_err(|error| format!("the candidate's signing identifier was not read: {error}"))?;
        if identifier != release {
            return Err(format!(
                "the candidate is signed as {identifier:?}, and the rendezvous admits only code \
                 signed as {release:?}"
            ));
        }
        crate::handoff_rendezvous::DialerCode::release(team)
    } else {
        Ok(crate::handoff_rendezvous::DialerCode::code_at(
            bundle.to_path_buf(),
        ))
    }
}

#[cfg(target_os = "macos")]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "native_update_launched_dial_wait",
        action = "Accept",
        project = "aterm_gui::app_update_handoff::launched_dial_wait_conformance::project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "native_update_launched_dial_wait",
        action = "TimeOut",
        project = "aterm_gui::app_update_handoff::launched_dial_wait_conformance::project"
    )
)]
fn await_launched_dial(
    rendezvous: &crate::handoff_rendezvous::Rendezvous,
    in_flight: &crate::app_launch_successor::LaunchInFlight,
    code: &crate::handoff_rendezvous::DialerCode,
    deadline: std::time::Instant,
    cancel: &std::sync::mpsc::Receiver<()>,
    named: &std::cell::RefCell<NamedSuccessorWatch>,
) -> Result<crate::handoff_rendezvous::ClaimedPeer, LaunchedDialEnd> {
    let fired = std::cell::Cell::new(None);
    let abort = || {
        if cancel.try_recv().is_ok() {
            fired.set(Some(DialWaitAbort::Cancelled));
            return true;
        }
        if in_flight.exited_before_check_in() {
            fired.set(Some(DialWaitAbort::SuccessorExited(
                PreDialDeath::TerminatedBeforeCheckIn,
            )));
            return true;
        }
        if let Some((candidate, death)) = named.borrow_mut().gone(in_flight.named_pid()) {
            fired.set(Some(DialWaitAbort::SuccessorExited(
                PreDialDeath::NamedProcessGone { candidate, death },
            )));
            return true;
        }
        false
    };
    let turned_away = std::cell::RefCell::new(None);
    let refused = |dialer: &crate::handoff_rendezvous::CodeRefusedDialer| {
        *turned_away.borrow_mut() = u32::try_from(dialer.pid)
            .ok()
            .map(|pid| (HandoffCandidate::of_attested_peer(pid), dialer.clone()));
    };
    match rendezvous.accept_claim_observed(None, code, deadline, &abort, &refused) {
        Ok(peer) => Ok(peer),
        Err(crate::handoff_rendezvous::RendezvousError::Cancelled) => Err(match fired.get() {
            Some(DialWaitAbort::SuccessorExited(death)) => {
                let launched = turned_away
                    .take()
                    .filter(|(witness, _)| code_refused_dialer_was_launched(*witness, death));
                match launched {
                    Some((witness, turned))
                        if turned.end == crate::handoff_rendezvous::CodeRefusedEnd::Closed =>
                    {
                        LaunchedDialEnd::CodeRefused {
                            dialer: witness,
                            refusal: turned.refusal,
                            death,
                        }
                    }
                    Some((witness, _)) => LaunchedDialEnd::ExitedWhileHeld {
                        dialer: witness,
                        death,
                    },
                    None => LaunchedDialEnd::SuccessorExited(death),
                }
            }
            Some(DialWaitAbort::Cancelled) | None => LaunchedDialEnd::Cancelled,
        }),
        Err(error) => Err(LaunchedDialEnd::Rendezvous(error)),
    }
}

#[cfg(target_os = "macos")]
const UNDIALLED_NAMING_PATIENCE: std::time::Duration = std::time::Duration::from_secs(30);

#[cfg(target_os = "macos")]
enum UndialledName {
    Named(HandoffCandidate, Option<CandidateExitWatch>),
    Died {
        named: Option<HandoffCandidate>,
        death: crate::ChildDeathEvidence,
    },
    Unnamed,
}

#[cfg(target_os = "macos")]
fn name_undialled_successor(
    in_flight: &crate::app_launch_successor::LaunchInFlight,
    named: &mut NamedSuccessorWatch,
    launch_stamp: &std::path::Path,
    patience: std::time::Duration,
) -> UndialledName {
    const PROBE: std::time::Duration = std::time::Duration::from_millis(10);
    let ours =
        |candidate: &HandoffCandidate| candidate.pid > 1 && candidate.pid != std::process::id();
    let stamped = || {
        aterm_update::read_launch_stamp(launch_stamp)
            .map(HandoffCandidate::of_stamped_launch)
            .filter(ours)
    };
    let give_up = std::time::Instant::now() + patience;
    loop {
        if in_flight.exited_before_check_in() {
            return UndialledName::Died {
                named: stamped(),
                death: PreDialDeath::TerminatedBeforeCheckIn.evidence(),
            };
        }
        if let Some((candidate, death)) = named.gone(in_flight.named_pid()) {
            return UndialledName::Died {
                named: Some(candidate),
                death,
            };
        }
        if let Some((candidate, watch)) = named.seen.take_if(|(candidate, _)| ours(candidate)) {
            return UndialledName::Named(candidate, watch);
        }
        if let Some(candidate) = stamped() {
            let watch = CandidateExitWatch::watch(candidate.pid)
                .filter(|_| candidate.identity() == HandoffCandidateIdentity::Corroborated);
            return UndialledName::Named(candidate, watch);
        }
        if std::time::Instant::now() >= give_up {
            return UndialledName::Unnamed;
        }
        std::thread::sleep(PROBE);
    }
}

#[cfg(target_os = "macos")]
#[derive(Debug, PartialEq, Eq)]
enum UndialledStandDown {
    Gone {
        named: Option<HandoffCandidate>,
        death: crate::ChildDeathEvidence,
        steps: Vec<StandDownStep>,
    },
    Unnamed,
}

#[cfg(target_os = "macos")]
fn stand_down_undialled_successor(
    rendezvous: crate::handoff_rendezvous::Rendezvous,
    in_flight: &crate::app_launch_successor::LaunchInFlight,
    mut named: NamedSuccessorWatch,
    grace: std::time::Duration,
    patience: std::time::Duration,
) -> UndialledStandDown {
    let launch_stamp = crate::handoff_rendezvous::launch_stamp_path(rendezvous.path());
    let name = name_undialled_successor(in_flight, &mut named, &launch_stamp, patience);
    drop(rendezvous);
    match name {
        UndialledName::Named(candidate, watch) => {
            let stood = prove_stood_down(candidate, watch, grace, &mut || {});
            UndialledStandDown::Gone {
                named: Some(candidate),
                death: stood.death,
                steps: stood.steps,
            }
        }
        UndialledName::Died { named, death } => UndialledStandDown::Gone {
            named,
            death,
            steps: vec![
                StandDownStep::Revoke,
                StandDownStep::SuccessorExited,
                StandDownStep::Reap,
            ],
        },
        UndialledName::Unnamed => UndialledStandDown::Unnamed,
    }
}

#[cfg(target_os = "macos")]
#[derive(Debug)]
struct UndialledRetire {
    completion: crate::UpdateHandoffCompletion,
    give_back: Option<HandoffCandidate>,
}

#[cfg(target_os = "macos")]
#[allow(clippy::too_many_arguments)]
fn file_undialled_retire(
    job: &HandoffWorkerJob,
    nonce: String,
    end: &LaunchedDialEnd,
    rendezvous: crate::handoff_rendezvous::Rendezvous,
    in_flight: &crate::app_launch_successor::LaunchInFlight,
    named: NamedSuccessorWatch,
    grace: std::time::Duration,
    patience: std::time::Duration,
) -> UndialledRetire {
    let (outcome, detail) = match end {
        LaunchedDialEnd::Rendezvous(error) => (
            crate::UpdateHandoffOutcome::TimedOut,
            if matches!(
                error,
                crate::handoff_rendezvous::RendezvousError::PeerCode { .. }
            ) {
                format!("no rendezvous dialer was admitted before the dial deadline: {error}")
            } else {
                format!("the launched successor never claimed the handoff: {error}")
            },
        ),
        _ => (
            crate::UpdateHandoffOutcome::ActivityRevoked,
            "structural activity revoked the handoff before the successor dialled".to_string(),
        ),
    };
    let stood = stand_down_undialled_successor(rendezvous, in_flight, named, grace, patience);
    let (named, death) = match &stood {
        UndialledStandDown::Gone {
            named,
            death,
            steps,
        } => {
            aterm_log::info!(
                "update apply: stood the undialled successor ({}) down — {detail}; steps \
                 {steps:?}",
                named.map_or_else(
                    || "nothing named it, and LaunchServices reports it terminated".to_string(),
                    |named| format!("pid {}", named.pid)
                )
            );
            (*named, *death)
        }
        UndialledStandDown::Unnamed => {
            aterm_log::warn!(
                "update apply: nothing named the launched successor within {} s — neither \
                 LaunchServices nor a launch stamp; it was never seen to run, and is left to \
                 its own rule (a dial now finds no rendezvous) — {detail}",
                patience.as_secs()
            );
            (None, crate::ChildDeathEvidence::Unobserved)
        }
    };
    UndialledRetire {
        completion: crate::UpdateHandoffCompletion::failure(
            job.attempt_id,
            Some(nonce),
            None,
            outcome,
            detail,
        )
        .with_child_death(death),
        give_back: named.filter(|_| {
            forgives_the_counted_trial_launch(death, job.current_build, job.target_build)
        }),
    }
}

#[cfg(target_os = "macos")]
fn send_undialled_retire(
    job: &HandoffWorkerJob,
    proxy: &aterm_window::event_loop::EventLoopProxy<Wake>,
    retire: UndialledRetire,
) {
    let published = publish_warranted_handoff_failure(
        HandoffRollbackWarrant::NeverTransferred,
        &job.cleanup,
        proxy,
        retire.completion,
    );
    if let Some(candidate) = retire.give_back {
        forgive_candidate_trial_launch(job, candidate);
    }
    if published {
        send_post_failure_reconcile_facts(&job.cleanup, proxy, job.current_build);
    }
}

#[cfg(target_os = "macos")]
fn send_pre_dial_death(
    job: &HandoffWorkerJob,
    proxy: &aterm_window::event_loop::EventLoopProxy<Wake>,
    nonce: String,
    death: PreDialDeath,
    held: Option<u32>,
    since_launch: std::time::Duration,
    launch_stamp: &std::path::Path,
) {
    let evidence = death.evidence();
    let seen = pre_dial_death_seen(death);
    let what = held.map_or_else(
        || "the launched successor exited before it dialled".to_string(),
        |pid| {
            format!(
                "the launched successor dialled (pid {pid}) and exited while its code check \
                 waited on macOS's code-signing service"
            )
        },
    );
    aterm_log::warn!(
        "update apply: {what} — {seen} ({evidence:?}), {} ms after the launch; rolling back \
         with every reader live instead of waiting out the dial budget",
        since_launch.as_millis()
    );
    let published = publish_warranted_handoff_failure(
        HandoffRollbackWarrant::NeverTransferred,
        &job.cleanup,
        proxy,
        crate::UpdateHandoffCompletion::failure(
            job.attempt_id,
            Some(nonce),
            None,
            crate::UpdateHandoffOutcome::ChildDied,
            format!("{what}: {seen}"),
        )
        .with_child_death(evidence),
    );
    if forgives_the_counted_trial_launch(evidence, job.current_build, job.target_build) {
        match death {
            PreDialDeath::NamedProcessGone { candidate, .. } => {
                forgive_candidate_trial_launch(job, candidate);
            }
            PreDialDeath::TerminatedBeforeCheckIn => {
                aterm_update::forgive_stamped_trial_launch(job.target_build, launch_stamp);
            }
        }
    }
    if published {
        send_post_failure_reconcile_facts(&job.cleanup, proxy, job.current_build);
    }
}

#[cfg(target_os = "macos")]
fn pre_dial_death_seen(death: PreDialDeath) -> String {
    match death {
        PreDialDeath::TerminatedBeforeCheckIn => {
            "LaunchServices reports the application terminated before it checked in".to_string()
        }
        PreDialDeath::NamedProcessGone { candidate, .. } => {
            format!(
                "the process LaunchServices named (pid {}) is gone",
                candidate.pid
            )
        }
    }
}

#[cfg(target_os = "macos")]
fn code_refusal_outcome(
    refusal: &crate::handoff_rendezvous::CodeRefusal,
) -> crate::UpdateHandoffOutcome {
    if refusal.is_verdict() {
        crate::UpdateHandoffOutcome::Rejected
    } else {
        crate::UpdateHandoffOutcome::ProducerFailed
    }
}

#[cfg(target_os = "macos")]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "native_update_rendezvous_admission",
        action = "FileByCause",
        project = "aterm_gui::app_update_handoff::launched_dialer_admission_conformance::project"
    )
)]
fn code_refused_completion(
    attempt_id: u64,
    nonce: String,
    dialer_pid: u32,
    refusal: &crate::handoff_rendezvous::CodeRefusal,
    death: PreDialDeath,
) -> crate::UpdateHandoffCompletion {
    crate::UpdateHandoffCompletion::failure(
        attempt_id,
        Some(nonce),
        None,
        code_refusal_outcome(refusal),
        format!(
            "the launched successor dialled (pid {dialer_pid}) and was not admitted: {refusal}; \
             it then left by its own rule ({})",
            pre_dial_death_seen(death)
        ),
    )
}

#[cfg(target_os = "macos")]
fn send_code_refused_dial(
    job: &HandoffWorkerJob,
    proxy: &aterm_window::event_loop::EventLoopProxy<Wake>,
    nonce: String,
    dialer: HandoffCandidate,
    refusal: &crate::handoff_rendezvous::CodeRefusal,
    death: PreDialDeath,
) {
    let completion = code_refused_completion(job.attempt_id, nonce, dialer.pid, refusal, death);
    aterm_log::warn!(
        "update apply: {}; rolling back with every reader live ({:?}, retried on the transient \
         schedule)",
        completion.detail,
        completion.outcome
    );
    let published = publish_warranted_handoff_failure(
        HandoffRollbackWarrant::NeverTransferred,
        &job.cleanup,
        proxy,
        completion,
    );
    if forgives_the_counted_trial_launch(
        crate::ChildDeathEvidence::Unobserved,
        job.current_build,
        job.target_build,
    ) {
        forgive_candidate_trial_launch(job, dialer);
    }
    if published {
        send_post_failure_reconcile_facts(&job.cleanup, proxy, job.current_build);
    }
}

#[cfg(target_os = "macos")]
struct LaunchCorroboration<'a> {
    in_flight: &'a crate::app_launch_successor::LaunchInFlight,
    dialer_pid: i32,
    dialer: HandoffCandidate,
    answered: bool,
    report: Option<(aterm_window::event_loop::EventLoopProxy<Wake>, u64)>,
}

#[cfg(target_os = "macos")]
impl<'a> LaunchCorroboration<'a> {
    fn of_dialer(
        in_flight: &'a crate::app_launch_successor::LaunchInFlight,
        dialer: HandoffCandidate,
    ) -> Self {
        Self {
            in_flight,
            dialer_pid: i32::try_from(dialer.pid).unwrap_or(-1),
            dialer,
            answered: false,
            report: None,
        }
    }

    fn reporting_to(
        mut self,
        proxy: &aterm_window::event_loop::EventLoopProxy<Wake>,
        attempt_id: u64,
    ) -> Self {
        self.report = Some((proxy.clone(), attempt_id));
        self
    }
}

#[cfg(target_os = "macos")]
impl LaunchCorroboration<'_> {
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "native_update_launched_dial_wait",
            action = "Corroborate",
            project = "aterm_gui::app_update_handoff::launched_dial_wait_conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "native_update_rendezvous_admission",
            action = "Commit",
            project = "aterm_gui::app_update_handoff::launched_dialer_admission_conformance::project"
        )
    )]
    fn poll(&mut self) -> Result<(), String> {
        if self.answered {
            return Ok(());
        }
        match self.in_flight.wait(std::time::Duration::ZERO) {
            Ok(successor) if successor.pid() == self.dialer_pid => {
                self.answered = true;
                aterm_log::info!(
                    "update apply: LaunchServices corroborates the dialer as pid {}",
                    self.dialer_pid
                );
                Ok(())
            }
            Ok(successor) => {
                self.answered = true;
                let detail = format!(
                    "the dialer (pid {}) is not the successor LaunchServices launched (pid {}); \
                     refusing the handoff",
                    self.dialer_pid,
                    successor.pid()
                );
                aterm_log::warn!("update apply: {detail}");
                Err(detail)
            }
            Err(crate::app_launch_successor::LaunchError::Timeout(_)) => Ok(()),
            Err(crate::app_launch_successor::LaunchError::ExitedBeforeCheckIn) => {
                self.answered = true;
                if self.dialer.identity() == HandoffCandidateIdentity::Corroborated {
                    let detail = format!(
                        "LaunchServices reports the process it launched exited before it checked \
                         in, and the dialer (pid {}) is alive, so it is not the launched \
                         successor; refusing the handoff",
                        self.dialer_pid
                    );
                    aterm_log::warn!("update apply: {detail}");
                    Err(detail)
                } else {
                    aterm_log::info!(
                        "update apply: LaunchServices reports the launched process exited before \
                         it checked in, and the dialer (pid {}) is gone too: its death ends the \
                         attempt",
                        self.dialer_pid
                    );
                    Ok(())
                }
            }
            Err(crate::app_launch_successor::LaunchError::PidUnknown(pid)) => {
                self.answered = true;
                aterm_log::warn!(
                    "update apply: LaunchServices does not know the launched successor's pid \
                     (it answered {pid}) after the dialer (pid {}) dialled: its record of the \
                     app is stale, so System Settings' Quit & Reopen, the menu-bar item and \
                     TCC's attribution can miss the successor; proceeding on the attested \
                     dialer",
                    self.dialer_pid
                );
                if let Some((proxy, attempt_id)) = &self.report {
                    let _ = proxy.send_event(Wake::UpdateLaunchUnregistered {
                        attempt_id: *attempt_id,
                        pid: self.dialer_pid,
                    });
                }
                Ok(())
            }
            Err(error) => {
                self.answered = true;
                aterm_log::warn!(
                    "update apply: LaunchServices answered the launch with an error after the \
                     successor had already dialled ({error}); proceeding on the attested dialer"
                );
                Ok(())
            }
        }
    }
}

#[cfg(unix)]
fn wait_handoff_ready(
    rd: &std::os::fd::OwnedFd,
    expected: crate::seamless::AdoptionProof,
    cancel: &std::sync::mpsc::Receiver<()>,
    masters: &[i32],
    deadline: std::time::Instant,
    candidate_gone: &mut dyn FnMut() -> bool,
    #[cfg(target_os = "macos")] mut corroboration: Option<LaunchCorroboration<'_>>,
) -> crate::UpdateHandoffOutcome {
    use std::os::fd::AsRawFd as _;
    let mut wire = [0u8; crate::seamless::READY_WIRE_LEN];
    let mut offset = 0usize;
    let mut pollfds = Vec::with_capacity(masters.len().saturating_add(1));
    pollfds.push(libc::pollfd {
        fd: rd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    });
    pollfds.extend(masters.iter().copied().map(|fd| libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    }));
    loop {
        if cancel.try_recv().is_ok() {
            return crate::UpdateHandoffOutcome::ActivityRevoked;
        }
        if std::time::Instant::now() >= deadline {
            return crate::UpdateHandoffOutcome::TimedOut;
        }
        #[cfg(target_os = "macos")]
        if let Some(corroboration) = corroboration.as_mut()
            && corroboration.poll().is_err()
        {
            return crate::UpdateHandoffOutcome::Rejected;
        }
        if candidate_gone() {
            return crate::UpdateHandoffOutcome::ChildDied;
        }
        for pfd in &mut pollfds {
            pfd.revents = 0;
        }
        // SAFETY: a stable initialized pollfd slice; 10 ms bounds death abort.
        let n = unsafe { libc::poll(pollfds.as_mut_ptr(), pollfds.len() as libc::nfds_t, 10) };
        if n <= 0 {
            continue; // timeout slice or EINTR — re-check the candidate + deadline (poll already blocked)
        }
        match classify_ready_poll(&pollfds) {
            ReadyPollAction::SessionDied => return crate::UpdateHandoffOutcome::ActivityRevoked,
            ReadyPollAction::NoProgress => {
                std::thread::sleep(std::time::Duration::from_millis(2));
                continue;
            }
            ReadyPollAction::ReadProof => {}
        }
        // SAFETY: bounded read into the unfilled suffix of a fixed local wire.
        let r = unsafe {
            libc::read(
                rd.as_raw_fd(),
                wire[offset..].as_mut_ptr().cast(),
                wire.len() - offset,
            )
        };
        match r {
            n if n > 0 => {
                offset += usize::try_from(n).unwrap_or(0);
                if offset != wire.len() {
                    continue;
                }
                if !readiness_proof_matches(expected, &wire) {
                    return crate::UpdateHandoffOutcome::AdoptionMismatch;
                }
                if cancel.try_recv().is_ok() {
                    return crate::UpdateHandoffOutcome::ActivityRevoked;
                }
                return crate::UpdateHandoffOutcome::ProofReady;
            }
            0 => return crate::UpdateHandoffOutcome::ChildDied,
            _ => continue, // EINTR/EAGAIN — re-loop
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
#[path = "app_update_handoff_held_successor_stand_down_tests.rs"]
mod held_successor_stand_down_tests;

#[cfg(all(test, unix))]
#[path = "app_update_handoff_late_park_record_tests.rs"]
mod late_park_record_tests;

#[cfg(all(test, unix))]
impl App {
    pub(crate) fn publish_preverified_pass_for_test(
        &self,
        attempt: &crate::native_updater_service::ApplyAttemptTicket,
        read: &aterm_update_core::handoff_policy::PolicyRead,
    ) -> Option<aterm_update_core::handoff_policy::HandoffPolicy> {
        PreverifyPublisher::for_attempt(&self.handoff_preverified, Some(attempt))
            .expect("an attempt names its artifact")
            .publish_policy_pass(read, crate::running_build_number(), "the test candidate")
    }
}

#[cfg(all(test, unix))]
#[path = "app_update_handoff_preverify_publisher_tests.rs"]
mod preverify_publisher_tests;

#[cfg(all(test, unix))]
#[path = "app_update_handoff_park_gate_tests.rs"]
mod park_gate_tests;

#[cfg(all(test, unix))]
#[path = "app_update_handoff_handed_set_and_mid_sequence_tests.rs"]
mod handed_set_and_mid_sequence_tests;

#[cfg(test)]
#[path = "app_update_handoff_freeze_budget_tests.rs"]
mod freeze_budget_tests;

#[cfg(all(test, unix))]
#[path = "app_update_handoff_dry_run_capture_tests.rs"]
pub(crate) mod dry_run_capture_tests;

#[cfg(all(test, unix))]
mod capture_budget_reservation_tests {
    use super::{MAX_HANDOFF_CAPTURE_BUDGET_BYTES, optional_carry_fits};
    use crate::seamless::{
        admit_checkpoint_dimensions, checkpoint_capture_budget_bytes, mandatory_checkpoint_cells,
        max_handoff_aggregate_grid_cells, max_handoff_history_lines,
    };

    fn walk_pool(rows: u16, cols: u16, sessions: u64) -> Result<u64, u64> {
        let carry = max_handoff_history_lines();
        let mandatory_cells = mandatory_checkpoint_cells(rows, cols);
        let mandatory_bytes = checkpoint_capture_budget_bytes(rows, cols, 0)
            .expect("PRECONDITION: the test geometry must be admissible at all");
        let history_cells = u64::from(cols) * u64::from(carry);
        let history_bytes = checkpoint_capture_budget_bytes(rows, cols, carry)
            .expect("PRECONDITION: the test geometry must be admissible with history")
            - mandatory_bytes;

        let mut cells_reserve = mandatory_cells * sessions;
        let mut bytes_reserve = mandatory_bytes * sessions;
        let mut used_cells = 0_u64;
        let mut used_bytes = 0_u64;
        let mut carried = 0_u64;
        for index in 0..sessions {
            cells_reserve -= mandatory_cells;
            bytes_reserve -= mandatory_bytes;
            let cells_fit = optional_carry_fits(
                used_cells,
                mandatory_cells,
                history_cells,
                cells_reserve,
                max_handoff_aggregate_grid_cells(),
            );
            let bytes_fit = optional_carry_fits(
                used_bytes,
                mandatory_bytes,
                history_bytes,
                bytes_reserve,
                MAX_HANDOFF_CAPTURE_BUDGET_BYTES,
            );
            let history = if cells_fit && bytes_fit { carry } else { 0 };
            let Ok(per_grid) =
                admit_checkpoint_dimensions(&mut used_cells, rows, cols, history, true)
            else {
                return Err(index);
            };
            used_bytes += per_grid * 2;
            if used_bytes > MAX_HANDOFF_CAPTURE_BUDGET_BYTES {
                return Err(index);
            }
            carried += u64::from(history != 0);
        }
        Ok(carried)
    }

    #[test]
    fn a_heavy_pool_is_admitted_even_when_it_must_drop_history() {
        for (rows, cols) in [(49_u16, 110_u16), (60, 200)] {
            for sessions in [12_u64, 24, 64] {
                let carried = walk_pool(rows, cols, sessions).unwrap_or_else(|index| {
                    panic!(
                        "session {index} of {sessions} at {rows}x{cols} was refused; a pool \
                         whose visible screens fit must never cost the update"
                    )
                });
                assert!(
                    carried > 0,
                    "{sessions} panes at {rows}x{cols} carried no scrollback at all — the \
                     reservation must buy the pool a SMALLER carry, not abolish it"
                );
            }
        }
    }

    #[test]
    fn a_pool_is_refused_only_when_its_mandatory_total_does_not_fit() {
        let (rows, cols) = (60_u16, 200_u16);
        let fits = max_handoff_aggregate_grid_cells() / mandatory_checkpoint_cells(rows, cols);
        assert!(
            walk_pool(rows, cols, fits).is_ok(),
            "{fits} panes at {rows}x{cols} are exactly what the aggregate holds \
             visible-only, so every one of them must be admitted"
        );
        assert_eq!(
            walk_pool(rows, cols, fits + 1),
            Err(fits),
            "one pane past the visible-only ceiling must be refused, and refused as the \
             LAST session — never an earlier one that lost its budget to somebody \
             else's scrollback"
        );
    }

    #[test]
    fn every_full_screen_display_geometry_is_admitted_for_a_pool_of_panes() {
        use aterm_window::dpi::PhysicalSize;

        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        {
            let ws = app.windows.get_mut(&wid).expect("the headless window");
            ws.metrics.pad = 24;
            ws.metrics.pad_top = 4;
            ws.metrics.head = 80;
        }
        let mut over_the_old_cap = Vec::new();
        for (width, height) in [
            (3024_u32, 1964_u32),
            (3456, 2234),
            (5120, 2880),
            (6016, 3384),
        ] {
            for (shape, size) in [
                ("full screen", PhysicalSize::new(width, height)),
                ("maximized", PhysicalSize::new(width, height - 74 - 140)),
            ] {
                let smallest = crate::FONT_PX_MIN as u32;
                for font_px in smallest..=48 {
                    app.windows
                        .get_mut(&wid)
                        .expect("the headless window")
                        .metrics
                        .font_px = font_px as f32;
                    let (rows, cols) = app.grid_dims_for(wid, size);
                    let cells = u64::from(rows) * u64::from(cols);
                    if cells > 32 * 1024 {
                        over_the_old_cap.push((width, height, shape, font_px, rows, cols));
                    }
                    for panes in 1..=8_u16 {
                        let pane_cols = (cols / panes).max(1);
                        let mut used = 0_u64;
                        for pane in 0..panes {
                            assert!(
                                admit_checkpoint_dimensions(&mut used, rows, pane_cols, 0, true)
                                    .is_ok(),
                                "{width}x{height} {shape} at font {font_px} px ({rows}x{cols}): \
                                 pane {pane} of {panes} ({rows}x{pane_cols}) was refused"
                            );
                        }
                    }
                }
            }
        }
        assert!(
            !over_the_old_cap.is_empty(),
            "PRECONDITION: the table must reach past the old 32 Ki per-grid cap, or it \
             could not have caught the incident"
        );
    }
}

#[cfg(all(test, unix))]
#[path = "app_update_handoff_commit_layout_topology_tests.rs"]
mod commit_layout_topology_tests;

#[cfg(all(test, unix))]
mod pinnedness_tests {
    use super::candidate_is_our_child;

    #[test]
    fn only_a_child_of_ours_pins_its_pid() {
        let mut child = aterm_uds::spawnfd::command("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("spawn a child to own a pid");
        let pid = libc::pid_t::try_from(child.id()).expect("pid fits");
        assert!(
            candidate_is_our_child(pid),
            "a live child of ours pins its number, so -pid still names its group"
        );

        let own = libc::pid_t::try_from(std::process::id()).expect("pid fits");
        assert!(
            !candidate_is_our_child(own),
            "a process that is not our child must never license a group kill"
        );
        assert!(!candidate_is_our_child(1), "launchd is not ours either");

        child.kill().expect("kill the probe child");
        let status = child.wait().expect("the probe left it waitable");
        assert!(!status.success(), "it was killed, not exited cleanly");
    }
}

#[cfg(all(test, unix))]
#[path = "app_update_handoff_candidate_death_evidence_tests.rs"]
mod candidate_death_evidence_tests;

#[cfg(all(test, unix))]
mod trial_launch_forgiveness_tests {
    use super::forgives_the_counted_trial_launch;
    use crate::ChildDeathEvidence as Death;

    const CURRENT: u64 = 1_787_699_398;
    const TARGET: u64 = 1_787_699_399;

    fn forgives(death: Death) -> bool {
        forgives_the_counted_trial_launch(death, CURRENT, TARGET)
    }

    #[test]
    fn only_a_death_the_bytes_answer_for_keeps_its_counted_trial_launch() {
        for (death, keeps_its_count, why) in [
            (
                Death::Exited { code: 0 },
                true,
                "the image reached an `exit` instruction: it ran, it decided, and \
                 a launch that ended that way is the sentinel's to keep",
            ),
            (
                Death::Exited { code: 74 },
                true,
                "same reading; the code is recorded, not judged",
            ),
            (
                Death::Signalled {
                    signal: libc::SIGSEGV,
                },
                true,
                "a fault is the image executing itself into a wall",
            ),
            (
                Death::Signalled {
                    signal: libc::SIGKILL,
                },
                false,
                "THE FIELD CASE: macOS jetsam reclaiming memory says nothing about \
                 the bytes, so the launch goes back — otherwise three busy \
                 afternoons revert a healthy build",
            ),
            (
                Death::Unobserved,
                false,
                "and an unattributed death least of all: this is the majority \
                 answer on the shipping lane, and it is retried six times",
            ),
        ] {
            assert_eq!(forgives(death), !keeps_its_count, "{death:?}: {why}");
        }
    }

    #[test]
    fn an_attempt_that_authorizes_no_newer_build_has_no_launch_to_forgive() {
        for death in [
            Death::Unobserved,
            Death::Exited { code: 0 },
            Death::Signalled {
                signal: libc::SIGKILL,
            },
        ] {
            assert!(
                !forgives_the_counted_trial_launch(death, CURRENT, CURRENT),
                "{death:?}: an installed activation is its own target, and the QA \
                 seam authorizes none at all"
            );
            assert!(
                !forgives_the_counted_trial_launch(death, CURRENT, CURRENT - 1),
                "{death:?}: and an older target is never applied"
            );
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
#[path = "app_update_handoff_candidate_exit_watch_tests.rs"]
mod candidate_exit_watch_tests;

#[cfg(all(test, unix))]
#[path = "app_update_handoff_handoff_process_group_tests.rs"]
mod handoff_process_group_tests;

#[cfg(all(test, unix))]
#[path = "app_update_handoff_returned_handoff_completion_lane_tests.rs"]
mod returned_handoff_completion_lane_tests;

#[cfg(all(test, target_os = "macos"))]
#[path = "app_update_handoff_handoff_lane_tests.rs"]
mod handoff_lane_tests;

#[cfg(all(test, unix))]
#[path = "app_update_handoff_ownership_conformance_tests.rs"]
mod ownership_conformance;

#[cfg(all(test, unix))]
#[path = "app_update_handoff_font_zoom_carry_tests.rs"]
mod font_zoom_carry_tests;

#[cfg(all(test, unix))]
#[path = "app_update_handoff_uncarried_messages_tests.rs"]
mod uncarried_messages_tests;

#[cfg(all(test, unix))]
mod held_capture_bound_tests {
    use std::sync::Arc;

    fn linked(rows: u16) -> aterm_core::terminal::Terminal {
        let mut t = aterm_core::terminal::Terminal::new(rows, 200);
        let url_tail = "a".repeat(2090);
        for row in 0..rows {
            let mut line = Vec::new();
            for link in 0..45 {
                line.extend_from_slice(
                    format!("\x1b]8;;https://e/{row}-{link}-{url_tail}\x07X\x1b]8;;\x07")
                        .as_bytes(),
                );
            }
            if row + 1 < rows {
                line.extend_from_slice(b"\r\n");
            }
            t.process(&line);
        }
        t
    }

    #[test]
    fn a_held_screen_the_writer_refuses_spends_none_of_the_held_budget() {
        let grid = |rows| {
            u64::try_from(
                linked(rows)
                    .checkpoint_carry_abandoning_partial(0)
                    .0
                    .grid
                    .len(),
            )
            .expect("a length")
        };
        assert!(
            3 * grid(50) <= crate::seamless::MAX_HELD_AGGREGATE_GRID_BYTES
                && 3 * grid(50) + grid(30) > crate::seamless::MAX_HELD_AGGREGATE_GRID_BYTES,
            "{} {}",
            grid(50),
            grid(30)
        );
        let mut app = crate::App::headless_for_test();
        let mut ids = Vec::new();
        for rows in [50, 50, 50, 30] {
            let id = app.next_session_id;
            let session = crate::ptyless_session(
                id,
                aterm_session::SessionId::generate(),
                linked(rows),
                Arc::new(crate::SinkWriter::new(-1)),
            );
            app.push_stub_tab(crate::WindowId(0), session);
            ids.push(id);
        }
        let (dense, fits) = (&ids[..3], ids[3]);
        let held = app.capture_held_screens(
            &[],
            std::time::Instant::now() + std::time::Duration::from_secs(60),
            &mut 0,
            crate::seamless::WireCaps::current(),
            aterm_update_core::handoff_policy::CarryCeiling::Full,
            &mut Vec::new(),
        );
        let captured: Vec<u64> = held.iter().map(|screen| screen.local_id).collect();
        assert!(
            captured.contains(&fits),
            "the pane that fits is carried: {captured:?}"
        );
        assert!(
            !captured.iter().any(|id| dense.contains(id)),
            "screens the writer refuses are not captured: {captured:?}"
        );
    }
}

#[cfg(all(test, unix))]
#[path = "app_update_handoff_parked_commit_tests.rs"]
mod parked_commit_tests;

#[cfg(all(test, unix))]
#[path = "app_update_handoff_ab_book_tests.rs"]
mod ab_book_tests;

#[cfg(all(test, unix))]
#[path = "app_update_handoff_carried_end_tests.rs"]
mod carried_end_tests;

#[cfg(all(test, unix))]
#[path = "app_update_handoff_video_park_tests.rs"]
mod video_park_tests;

#[cfg(all(test, target_os = "macos"))]
#[path = "app_update_handoff_launched_lane_grant_tests.rs"]
mod launched_lane_grant_tests;

#[cfg(all(test, unix))]
mod rollback_winsize_tests {
    use crate::{App, term_lock};

    fn winsize_of(fd: i32) -> (u16, u16) {
        // SAFETY: `winsize` is a valid zeroed out-parameter filled by `TIOCGWINSZ` on an open PTY fd.
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        // SAFETY: as above; `fd` is open for the whole test.
        assert_eq!(unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) }, 0);
        (ws.ws_row, ws.ws_col)
    }

    #[test]
    fn a_rolled_back_commit_puts_each_parked_ptys_own_size_back() {
        let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
        let mut app = App::headless_for_test();
        let session = &mut app.pool.sessions.get_mut(&0).expect("session 0").session;
        session.master = master;
        let parked = {
            let t = term_lock(&session.term);
            (t.rows(), t.cols())
        };
        aterm_pty::resize(master, parked.0, parked.1);
        aterm_pty::resize(master, parked.0 + 6, parked.1 + 20);
        assert_ne!(winsize_of(slave), parked, "the successor really moved it");
        app.rollback_overlap(None, &[(0, master, -1)]);
        assert_eq!(
            winsize_of(slave),
            parked,
            "the program resumes at the size its engine was parked at"
        );
        // SAFETY: both fds were opened above and remain exclusively owned here.
        unsafe {
            libc::close(master);
            libc::close(slave);
        }
    }

    #[test]
    fn a_rollback_never_waits_on_a_held_engine_and_puts_its_size_back_once_free() {
        let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
        let mut app = App::headless_for_test();
        let session = &mut app.pool.sessions.get_mut(&0).expect("session 0").session;
        session.master = master;
        let term = std::sync::Arc::clone(&session.term);
        let parked = {
            let t = term_lock(&term);
            (t.rows(), t.cols())
        };
        aterm_pty::resize(master, parked.0, parked.1);
        let moved = (parked.0 + 6, parked.1 + 20);
        aterm_pty::resize(master, moved.0, moved.1);
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let (returned_tx, returned_rx) = std::sync::mpsc::channel::<()>();
        let holder = std::thread::spawn(move || {
            let guard = term_lock(&term);
            held_tx.send(()).expect("the test waits");
            let waited_out = returned_rx
                .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                .is_err();
            drop(guard);
            waited_out
        });
        held_rx.recv().expect("the engine is held");
        app.rollback_overlap(None, &[(0, master, -1)]);
        let size_while_held = winsize_of(slave);
        let _ = returned_tx.send(());
        let waited_out = holder.join().expect("the holder finished");
        assert!(!waited_out, "the rollback waited for the engine's holder");
        assert_eq!(
            size_while_held, moved,
            "PRECONDITION — the held engine's size is owed, not taken"
        );
        assert!(
            app.deferred_reader_resume_at.is_some(),
            "the owed size is the event loop's to retry"
        );
        app.resume_deferred_readers_nonblocking();
        assert_eq!(
            winsize_of(slave),
            parked,
            "the program resumes at the size its engine was parked at"
        );
        assert!(app.owed_parked_winsizes.is_empty(), "nothing is owed");
        // SAFETY: both fds were opened above and remain exclusively owned here.
        unsafe {
            libc::close(master);
            libc::close(slave);
        }
    }
}

#[cfg(all(test, unix))]
#[path = "app_update_handoff_fork_lane_policy_wait_tests.rs"]
mod fork_lane_policy_wait_tests;

#[cfg(all(test, unix))]
mod manifest_over_cap_tests {
    use super::{PreparationFailure, preparation_failure_of_write};
    use crate::seamless::WriteOutgoingFailure;

    #[test]
    fn an_over_cap_manifest_is_filed_on_the_refusal_lane() {
        let refused = preparation_failure_of_write(WriteOutgoingFailure::ManifestOverCap);
        assert!(
            matches!(refused, PreparationFailure::Refused(_)),
            "a deterministic refusal of the desk's content"
        );
        for own in [
            WriteOutgoingFailure::NoPrivateDir,
            WriteOutgoingFailure::Inconsistent,
            WriteOutgoingFailure::Io(std::io::ErrorKind::StorageFull),
        ] {
            assert!(
                matches!(
                    preparation_failure_of_write(own),
                    PreparationFailure::Producer(_)
                ),
                "{own}: this process's own trouble"
            );
        }
        assert_eq!(
            crate::app_native::HandoffFailureLane::classify(
                crate::native_updater_service::ApplyMode::Automatic,
                crate::UpdateHandoffOutcome::CaptureRefused,
                crate::ChildDeathEvidence::Unobserved,
                false,
            ),
            crate::app_native::HandoffFailureLane::Refused
        );
    }
}

#[cfg(all(test, unix))]
mod fabric_carry_successor_tests {
    use super::successor_reads_fabric_carry;

    #[test]
    fn only_a_successor_at_least_as_new_holds_retirements_back() {
        assert!(successor_reads_fabric_carry(1_790_700_000, 1_790_607_299));
        assert!(successor_reads_fabric_carry(1_790_607_299, 1_790_607_299));
        assert!(!successor_reads_fabric_carry(1_790_585_188, 1_790_607_299));
    }
}

#[cfg(all(test, unix))]
#[path = "app_update_handoff_cold_lane_layout_tests.rs"]
mod cold_lane_layout_tests;

#[cfg(all(test, unix))]
#[path = "app_update_handoff_cold_lane_verdict_tests.rs"]
mod cold_lane_verdict_tests;

#[cfg(all(test, unix))]
mod fork_lane_child_tests {
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
    use std::os::unix::process::CommandExt as _;

    fn stand_in(cloexec: bool) -> OwnedFd {
        let null = std::fs::File::open("/dev/null").expect("open /dev/null");
        let cmd = if cloexec {
            libc::F_DUPFD_CLOEXEC
        } else {
            libc::F_DUPFD
        };
        // SAFETY: F_DUPFD(_CLOEXEC) on a descriptor this test owns.
        let fd = unsafe { libc::fcntl(null.as_raw_fd(), cmd, 64) };
        assert!(fd >= 64, "a stand-in at 64 or above: {fd}");
        // SAFETY: a fresh descriptor this test exclusively owns.
        unsafe { OwnedFd::from_raw_fd(fd) }
    }

    fn child_holds(numbers: &[i32], hook: Option<Vec<i32>>) -> String {
        let probe = numbers
            .iter()
            .map(|&fd| {
                let held = aterm_spec::host::sh_held(fd);
                format!("{held} && printf 'held:{fd} '; ")
            })
            .collect::<String>();
        let Some(handed) = hook else {
            let dir = aterm_tempfile::tempdir().expect("a temp dir");
            let report = dir.path().join("holds");
            let status = aterm_uds::spawnfd::transparent("/bin/sh")
                .args([
                    "-c",
                    &format!("{{ {probe}echo end; }} >\"$1\" </dev/null"),
                    "sh",
                ])
                .arg(&report)
                .status()
                .expect("run the control");
            assert!(status.success(), "{status}");
            return std::fs::read_to_string(&report)
                .expect("the control's report")
                .trim()
                .to_string();
        };
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.args(["-c", &format!("{probe}echo end")]);
        let strip = aterm_uds::spawnfd::InheritStrip::prepare();
        // SAFETY: the hook runs between fork and exec; see the function.
        unsafe {
            cmd.pre_exec(move || super::fork_lane_child_holds(strip, &handed));
        }
        let out = cmd.output().expect("run the stand-in successor");
        assert!(out.status.success(), "{out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    #[test]
    fn the_fork_lane_successor_holds_what_it_is_handed_and_nothing_else() {
        let stray = stand_in(false);
        let handed = stand_in(true);
        let (s, h) = (stray.as_raw_fd(), handed.as_raw_fd());
        assert_eq!(
            child_holds(&[s, h], None),
            format!("held:{s} end"),
            "CONTROL: the transparent spawn carries the stray and not the flagged one"
        );
        assert_eq!(
            child_holds(&[s, h], Some(vec![h])),
            format!("held:{h} end"),
            "the successor holds the handed descriptor and not the stray"
        );
        drop((stray, handed));
    }

    #[test]
    fn the_cold_lane_exec_hands_over_nothing_this_process_holds() {
        const STAGE: &str = "ATERM_TEST_COLD_EXEC_HAND_OVER";
        if let Ok(stage) = std::env::var(STAGE) {
            let stray = stand_in(false);
            let fd = stray.as_raw_fd();
            let _lifeline = (stage == "hand-over").then(super::cold_exec_hand_over);
            let error = std::process::Command::new("/bin/sh")
                .args(["-c", &aterm_spec::host::held_probe(fd)])
                .exec();
            panic!("the probe did not replace this helper: {error}");
        }
        let probe = |stage: &str| {
            let mut helper = crate::test_alone::StandIn::this_test();
            helper.command().env(STAGE, stage);
            helper
                .spawn()
                .selected()
                .status
                .and_then(|status| status.code())
        };
        assert_eq!(
            probe("plain"),
            Some(3),
            "CONTROL: a plain exec hands the probe the stray"
        );
        assert_eq!(
            probe("hand-over"),
            Some(0),
            "the cold lane's exec handed the probe the stray"
        );
    }
}

#[cfg(all(test, unix))]
mod connection_drop_audit_tests {
    use crate::session_store::ConnectionCarry;

    fn row(src: &str, op: &str) -> ConnectionCarry {
        ConnectionCarry {
            src: src.to_string(),
            dst: "s-0000000001000007d7d7".to_string(),
            op: op.to_string(),
        }
    }

    #[test]
    fn the_attempt_carries_what_its_manifest_left_out_to_the_commit_which_audits_it() {
        crate::test_log::install();
        let src = "s-0000000001000006c6c6";
        let arbiter = crate::HandoffAttemptArbiter::new();
        assert!(arbiter.connections_left_out().is_empty(), "nothing yet");
        arbiter.set_connections_left_out(Vec::new());
        assert!(arbiter.connections_left_out().is_empty(), "none left out");
        let left_out = vec![row(src, "read-screen"), row(src, "signal")];
        arbiter.set_connections_left_out(left_out.clone());
        arbiter.set_connections_left_out(vec![row(src, "write-input")]);
        assert_eq!(
            arbiter.connections_left_out(),
            left_out.as_slice(),
            "one manifest per attempt: the first record stands"
        );
        let mark = format!("EDGE: action=drop origin=handoff src={src}");
        assert!(crate::test_log::containing(&mark).is_empty());
        crate::App::audit_connections_left_out(arbiter.connections_left_out());
        let audited = crate::test_log::containing(&mark);
        assert_eq!(audited.len(), 2, "one drop per row: {audited:?}");
        assert!(audited.iter().all(|record| record.target == "session_edge"));
        assert!(
            audited[0].message.ends_with("op=read-screen"),
            "{audited:?}"
        );
        assert!(audited[1].message.ends_with("op=signal"), "{audited:?}");
    }
}

#[cfg(all(any(test, debug_assertions), target_os = "macos"))]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "native_update_launched_dial_wait",
    action = "Dial",
    reason = "The successor's dial (handoff_rendezvous::claim_incoming), bound by \
              NativeUpdateSuccessorWarmBeforeClaim's Tier-1; this conformance dials the real \
              listener with the real claim frame."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "native_update_launched_dial_wait",
    action = "Die",
    reason = "The environment: a successor that dies before its dial (a warm-prologue fatal, \
              a boot crash). Driven live through the ATERM_DEBUG_DIE_BEFORE_DIAL seam in \
              main_entry; a unit test may not launch an application (AGENTS.md), so \
              the named-pid half ends a real stand-in process instead."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "native_update_launched_dial_wait",
    action = "Expire",
    reason = "The environment: the dial budget running out (handoff_ready_deadline)."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "native_update_launched_dial_wait",
    action = "EndOnAnyAnswer",
    reason = "The Buggy=1 mutant: an answer that proves nothing read as death; no shipping \
              code, and the conformance's negative control replays it and sees it refused."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "native_update_launched_dial_wait",
    action = "ConsumingPoll",
    reason = "The Buggy=1 mutant: the pre-dial poll taking the answer; no shipping code (the \
              wait peeks), replayed for real and caught by the conformance."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "native_update_launched_dial_wait",
    action = "PollCancelOnly",
    reason = "The Buggy=1 liveness mutant: the retired cancel-only abort; replayed for real \
              by the conformance, which waits out its budget beside a reported death."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "native_update_launched_dial_wait",
    action = "EndOnNamedPid",
    reason = "The Buggy=1 mutant: a named pid taken for gone without the kernel's word; no \
              shipping code, replayed for real on pid 1 and refused by the conformance."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "native_update_launched_dial_wait",
    action = "IgnoreNamedPid",
    reason = "The Buggy=1 liveness mutant: the abort before the named-pid watch; replayed \
              for real by the conformance, which waits out its budget beside a named \
              process that is gone."
)]
#[allow(
    dead_code,
    reason = "carrier for the `spec_unmodeled` waivers above; nothing calls it. An allow, \
              not an expect: rustc does not lint a fn the declarative anchors rebuild"
)]
fn launched_dial_wait_waivers() {}

#[cfg(all(any(test, debug_assertions), target_os = "macos"))]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "native_update_rendezvous_admission",
    action = "WrongSecret",
    reason = "The environment: a dialer that does not present this attempt's claim; realized \
              by a stand-in process presenting another claim."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "native_update_rendezvous_admission",
    action = "CopiedSecretOtherCode",
    reason = "The environment: a same-uid process that copied the claim out of the \
              successor's launch environment (KERN_PROCARGS2) and runs other code; realized \
              by a stand-in process that does not meet the stated requirement."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "native_update_rendezvous_admission",
    action = "TrustdSilent",
    reason = "The environment: macOS's code-signing service stops answering; realized by \
              making the process's one trustd gate overdue (aterm_uds::deadline::TRUST), as \
              a silent trustd does."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "native_update_rendezvous_admission",
    action = "TrustdAnswers",
    reason = "The environment: the stuck check returns and the gate lets checks through \
              again; realized by releasing the overdue work."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "native_update_rendezvous_admission",
    action = "LaunchServicesDisowns",
    reason = "The environment: LaunchServices' answer naming another pid, or reporting the \
              launched process exited; scripted with the measured answers, since a unit test \
              may not launch an application (AGENTS.md)."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "native_update_rendezvous_admission",
    action = "Refuse",
    reason = "The refusal's own steps are NativeUpdateOverlapHandoff's: before the grant the \
              ungranted stand-down (retire_ungranted_successor), after it the kill-and-reap \
              of the reject path; both are bound by that model's Tier-1."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "native_update_rendezvous_admission",
    action = "GrantOnSecretAlone",
    reason = "The Buggy=1 mutant: the launched lane before 2026-10-02, the secret its one \
              gate; run for real at the same-uid floor by the conformance, which sees a \
              stranger granted and the model refuse it."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "native_update_rendezvous_admission",
    action = "CommitOverDisowning",
    reason = "The Buggy=1 mutant: 2026-09-29's corroboration, an exit answer read as \
              corroborating; no shipping code, replayed in the model by the conformance."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "native_update_rendezvous_admission",
    action = "RefuseUnanswered",
    reason = "The Buggy=1 mutant: the first code gate of 2026-10-02, which closed a dialer \
              whose check trustd did not answer; replayed in the model by the conformance."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "native_update_rendezvous_admission",
    action = "FileAsUnexplainedDeath",
    reason = "The Buggy=1 mutant: that gate's filing of the turned-away successor as a death \
              before its dial; booked for real and replayed in the model by the conformance."
)]
#[allow(
    dead_code,
    reason = "carrier for the `spec_unmodeled` waivers above; nothing calls it. An allow, \
              not an expect: rustc does not lint a fn the declarative anchors rebuild"
)]
fn rendezvous_admission_waivers() {}

#[cfg(all(any(test, debug_assertions), unix))]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "NativeUpdateSettingsDraftCarry",
    action = "Type",
    reason = "The person's keystroke: every Settings text reducer edits the draft, and each \
              schedule here drives the real view's focus, select-all and commit to make one."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "NativeUpdateSettingsDraftCarry",
    action = "Rollback",
    reason = "The handoff's own step: a refused Commit rolls the overlap back and the outgoing \
              process keeps its view untouched; nothing of the Settings carry is undone, so the \
              step is the rig forgetting the killed successor and its layout."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "NativeUpdateSettingsDraftCarry",
    action = "StandDown",
    reason = "The handoff's own step: a lane the gate refused starts nothing, so standing down \
              is dropping the token; the outgoing view is untouched."
)]
#[allow(
    dead_code,
    reason = "carrier for the `spec_unmodeled` waivers above; nothing calls it. An allow, \
              not an expect: rustc does not lint a fn the declarative anchors rebuild"
)]
fn settings_draft_carry_waivers() {}
