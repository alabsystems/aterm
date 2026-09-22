// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Seamless update handoff: `apply_staged_update_now` and the unix overlap
//! worker it starts (`run_handoff_worker`, the bounded readiness wait, the
//! unique-reaper and rejection plumbing), the reap authority that licenses a
//! rollback (`HandoffRollbackWarrant`), the Commit-time admission facts, and
//! `finish_update_handoff`'s drain gate, Commit, and rollback.
//! A verbatim inherent-impl split of `App`.
//!
//! The successor this module spawns races the parent's in-flight `atpkg` pass
//! exactly as a second window does: the parent's child KEEPS RUNNING once the
//! parent has exited (atpkg's orphan watch re-points its stdio at
//! `orphan-pass.log`, 2026-09-14 — it used to die at its next print), so it holds
//! the store lock for the whole of its pass, and the successor's own launch lanes
//! `--wait-lock` behind it. A successor on a NEW build runs one full update pass
//! after its seed whatever the store's record says (`crate::launch_update_park`,
//! 2026-09-22 — the seed absorbs the wait, so that pass is not the one that stands
//! down); a same-build relaunch owes no pass while the store's last pass is fresh
//! (the due gate, 2026-09-18); an update child that itself waited stands down when
//! the holder finished a pass (`atpkg::cli`); and NO row is painted for the wait
//! (`Wake::PkgLockWaiting` is a log line). A REJECTED candidate is the one shape that is not covered: its
//! sweep SIGKILLs the candidate's process group, atpkg child included (the store
//! is crash-consistent), and the parked parent's loop picks the remainder up at
//! its next tick or bump.

use winit::event_loop::ActiveEventLoop;

use crate::App;
#[cfg(unix)]
use crate::Wake;
#[cfg(unix)]
use crate::app_input::paste_order;

// THE SEAMLESSNESS INVARIANT, PROVED BY THE COMPILER — see the sibling
// `app_update_handoff_island.rs`, which carries the `clean { … }` island and
// the full account of what it proves and what it does not.
//
// WHY IT IS A SIBLING FILE AND NOT INLINE HERE, which is where it lived until
// 2026-08-12: an island body reaches the Clean parser as a RUST token stream,
// so a compiler without the Clean surface does not skip the island — it fails
// to LEX it, reporting `missing \`enum\` for enum definition` at the `clean`
// keyword. That kills the whole file, and with it every method this module
// defines. `cfg` cannot help inline, because cfg-stripping happens after the
// file is parsed; only a `mod` declaration decides whether a file is READ at
// all. So the island moves behind one, and the gate is `clean_islands` —
// set in .cargo/config.toml on the native triple, which is exactly the set of
// lanes that run the Trust toolchain.
//
// The lanes this rescues are real and were both broken: the release's
// x86_64-apple-darwin compat slice and the Windows cfg-validation build both
// run upstream stable BY DESIGN (Trust has no std for either), and neither
// could compile aterm-gui at all while the island was inline. The proof is not
// weakened by this — it is checked on every native build, which is every build
// that can check it, and a sabotaged protocol still stops the crate compiling.
#[cfg(clean_islands)]
#[path = "app_update_handoff_island.rs"]
mod island;

/// Everything one attempt captured UNDER THE PARK — the facts that depend on
/// every reader being stopped, and nothing else.
///
/// Split out of [`HandoffWorkerJob`] for the late park (2026-09-19): on the
/// launched lane the worker is already running — it has launched the successor
/// and is holding its rendezvous claim — when the main thread finally parks, so
/// the park's product has to cross to the worker as a message of its own
/// ([`HandoffTransferJob`]). The fork lane fills it at the job's construction,
/// exactly as before, because there the park still precedes the spawn.
#[cfg(unix)]
struct HandoffCapture {
    /// When the parent STARTED parking its readers — the zero point of the
    /// freeze the user actually feels, and the same instant the 20 ms capture
    /// deadline is measured from. (It is stamped immediately BEFORE
    /// `park_all_readers`, not after, so the interval includes the park itself;
    /// the park is bounded by that same 20 ms, so the inclusion is negligible
    /// — but the zero point is the park's start.)
    ///
    /// Carried so the worker can report the park->transfer half on the launched
    /// lane at the moment it becomes observable. Data only: nothing branches on it.
    park_at: std::time::Instant,
    manifest: crate::session_store::SessionHandoff,
    fds: crate::session_store::HandoffFds,
    screens: Vec<(u64, aterm_core::terminal::TerminalCheckpoint)>,
    /// Each session's CONTROL CARRY capture from the freeze — the archive's
    /// fence, counters and differ state, and the engine and ledger to export
    /// the rest from on this worker (`crate::handoff_carry`).
    carries: Vec<crate::handoff_carry::CarrySource>,
    window: Option<crate::session_store::WindowCarry>,
    layout: crate::restore::RestoreManifest,
    layout_digest: [u8; 32],
    screen_digest: [u8; 32],
    live: Vec<(u64, i32, i32)>,
    /// The identity triples the adoption proof hashes, which are NOT `live`.
    ///
    /// `live` is transport: real descriptor numbers this process can `poll` and
    /// hand over. The proof's middle term is whatever BOTH sides can compute
    /// independently, and that depends on how the descriptors travel — the fork
    /// lane's `execve` copies the table verbatim so the number itself works,
    /// while `SCM_RIGHTS` installs the receiver's own numbers and the term
    /// becomes the PTY device (`handoff_rendezvous::pty_device_term`). Keeping
    /// the two vectors separate is what stops a lane change from silently
    /// pointing `poll` at a device number.
    proof_identities: Vec<(u64, i32, i32)>,
    _owned_masters: Vec<std::os::fd::OwnedFd>,
}

/// The park's product, sent from the main thread to a worker that is already
/// holding a successor's claim (or already knows it must fork instead).
#[cfg(unix)]
pub(crate) struct HandoffTransferJob {
    capture: HandoffCapture,
    /// When the post-park proof wait gives up — computed by the main thread
    /// from the park instant ([`handoff_proof_deadline`]), so the freeze the
    /// user feels is what the deadline bounds, not the launch.
    proof_deadline: std::time::Instant,
}

/// The main thread's typed reason for standing a held successor down before the
/// park delivered anything: which outcome the completion should carry and why.
#[cfg(unix)]
#[derive(Debug)]
pub(crate) struct HandoffStandDown {
    pub(crate) outcome: crate::UpdateHandoffOutcome,
    pub(crate) detail: String,
}

/// The launched lane's side of the late park: the attempt nonce (minted on the
/// main thread, so the launch environment can name the manifest before it is
/// written), the channel the park's product arrives on, the channel a typed
/// stand-down arrives on, and how long the worker will hold a dialled successor
/// for a park that never comes.
#[cfg(target_os = "macos")]
struct HandoffPrelaunchLane {
    nonce: String,
    transfer: std::sync::mpsc::Receiver<HandoffTransferJob>,
    stand_down: std::sync::mpsc::Receiver<HandoffStandDown>,
    /// Where the main thread answers `Wake::UpdateHandoffStandingDown`: the
    /// worker waits on this before it revokes, so the park gate is provably
    /// closed and any readers the park had stopped are provably resumed before
    /// anything is killed. See [`retire_ungranted_successor`].
    stand_down_ack: std::sync::mpsc::Receiver<()>,
    hold_bound: std::time::Duration,
}

#[cfg(unix)]
struct HandoffWorkerJob {
    attempt_id: u64,
    current_build: u64,
    target_build: u64,
    target_commit: String,
    /// Run the staged-candidate pre-verification (codesign + sealed rebinding)
    /// as the worker's first action, off the GUI main thread. False for the
    /// same-binary debug re-exec, which has no staged `.app` to authenticate.
    verify_staged_candidate: bool,
    /// This attempt ACTIVATES the installed bundle at the executable's own path
    /// (`ApplyAttemptTicket::is_installed_activation`): the pre-verification runs
    /// against THAT bundle (there is no staged `.app`), and the successor is
    /// handed no expected-artifact triple (it has nothing to swap; it simply IS
    /// the newer build).
    installed_activation: bool,
    command: std::process::Command,
    /// What the park produced. `Some` from construction on the fork lane;
    /// `None` on the launched lane until the main thread's [`HandoffTransferJob`]
    /// arrives, which is AFTER the successor has been launched and has dialled.
    /// Every step from the artifact write onward reads it through
    /// [`Self::capture`], and the pre-grant stand-down never reads it at all.
    capture: Option<HandoffCapture>,
    /// The late park's channels, present exactly on the launched lane.
    #[cfg(target_os = "macos")]
    prelaunch: Option<HandoffPrelaunchLane>,
    /// Which transport this attempt chose, decided on the main thread before
    /// anything parked (see [`out_of_band_lane_refusal`]). macOS-only because
    /// the alternative to forking is macOS-only; every other unix has exactly
    /// one lane and carries no field for it.
    #[cfg(target_os = "macos")]
    lane: HandoffLane,
    /// Boot-trial launches the sentinel had counted for `target_build` BEFORE this
    /// candidate was launched. Forgiveness compares against it, so a candidate
    /// killed before it ever reached `check_boot_health` cannot give back a launch
    /// some earlier, genuinely crashed candidate observed.
    trial_launches_before: u32,
    /// The `.app` ROOT to hand LaunchServices on the out-of-band lane. `None`
    /// whenever this process is not running from a bundle, which is one of the
    /// reasons that lane is refused.
    #[cfg(target_os = "macos")]
    bundle: Option<std::path::PathBuf>,
    /// This attempt must take the out-of-band lane or not happen at all — a provenance
    /// repair, whose whole purpose is a successor launchd mints from a clean image
    /// ([`SameImageHandoff::requires_out_of_band`]). `run_out_of_band_handoff` can still
    /// decide at RUNTIME that it must fork (five sites: no bundle, a rendezvous that
    /// will not bind, a non-UTF-8 path, an environment needing a removal, a
    /// LaunchServices refusal), and this is what turns that one rejoining arm into an
    /// abort. Derived from the authority at the single construction site, so the two
    /// spellings of one fact cannot disagree across the thread boundary.
    #[cfg(target_os = "macos")]
    require_out_of_band: bool,
    cleanup: HandoffWorkerCleanup,
    cancel: std::sync::mpsc::Receiver<()>,
    arbiter: crate::HandoffAttemptArbiter,
}

#[cfg(unix)]
impl HandoffWorkerJob {
    /// The park's product. Every caller sits AFTER the park by construction —
    /// the fork lane fills the capture at construction, the launched lane only
    /// proceeds past its hold once the transfer job has landed — so an absent
    /// capture here is a logic error, not a runtime condition.
    fn capture(&self) -> &HandoffCapture {
        self.capture
            .as_ref()
            .expect("the handoff worker reads the capture only after the park delivered it")
    }
}

/// WHY an attempt may run with no staged artifact to authenticate.
///
/// Both variants mean the same thing to every gate downstream — "this attempt re-runs the
/// image we are already executing, and swaps nothing" — which is why they share one
/// parameter rather than two booleans that could disagree. The type is a zero-field
/// discriminant ON PURPOSE: it carries no build, no commit, no digest and no path, so
/// `target_build` and `target_commit` fall through to this build's own by construction
/// and there is no field an authority could point at another image.
///
/// `ProvenanceRepair` is additionally EARNED, not merely typed: `apply_staged_update_now`
/// accepts it only when the App's own measured verdict is consumable
/// ([`crate::provenance_repair::RepairPosture::take_eligible`]). A caller that passes it
/// without one falls through to the unchanged "no exact verified update authority"
/// refusal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SameImageHandoff {
    /// `ATERM_DEBUG_SEAMLESS_REEXEC=1` — the QA seam that exercises the full handoff and
    /// adopt path without a release. Byte-identical in behaviour to before this type.
    DebugSeam,
    /// A provenance self-repair: this process is tracked, the bundle is clean, and the
    /// only way to stop being tracked is to be re-minted by launchd from that bundle
    /// ([`crate::provenance_repair`]). Unlike the seam, this one REFUSES rather than
    /// falling back to the fork lane, because a forked successor inherits the tag and
    /// the whole replacement would buy nothing.
    ProvenanceRepair,
}

impl SameImageHandoff {
    /// Whether this attempt must take the out-of-band lane or not run at all.
    pub(crate) fn requires_out_of_band(self) -> bool {
        matches!(self, Self::ProvenanceRepair)
    }
}

/// How this attempt hands its descriptors to the successor.
///
/// The two lanes are not a preference, and this is not a feature flag: the fork
/// lane is the only one that exists on a machine without a bundle, and the
/// out-of-band lane is the only one that produces a successor with a launchd
/// application job of its own (`tests/handoff_launchd_job.rs`). The choice is
/// made ONCE, on the main thread, before any reader parks, because it decides
/// which term the adoption proof hashes — and that term has to be recorded in
/// the pending attempt the main thread will later re-derive the proof from.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HandoffLane {
    /// `fork` + `execve`: descriptors travel by inheritance and the proof hashes
    /// the descriptor NUMBER, which both sides agree on because `execve` copies
    /// the table verbatim. Every build in the field speaks this and only this.
    Fork,
    /// LaunchServices + a single-use `SCM_RIGHTS` rendezvous. The successor is
    /// launchd's child, so it gets its own application job; the descriptors
    /// arrive at numbers the receiver's kernel chose, so the proof hashes the
    /// PTY device instead.
    OutOfBand,
}

/// Facts the lane choice is made from. Pure, so the whole decision — including
/// every reason to fall back — is testable without a bundle, a socket or a
/// terminal.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HandoffLaneFacts {
    /// This process runs from inside a `.app` whose root we could name. Only a
    /// bundle gets an `application.<bundle-id>.<hex>` job, which is the entire
    /// point of the lane.
    bundled: bool,
    /// A launcher for this platform is compiled in.
    launcher_available: bool,
    /// The composed rendezvous path fits `sun_path` on this machine. It ALMOST
    /// does not — see `handoff_rendezvous`'s module docs for the 16 bytes of
    /// headroom `$HOME` has.
    socket_path_fits: bool,
    /// The authorized `target_build` is at least this build.
    ///
    /// THE ONE GUARD THE RETIRED VERSION ADVERTISEMENT CARRIED. "Presence of the
    /// transport IS the version" settles old-parent/new-child — a parent with no
    /// out-of-band code sends `ATERM_SEAMLESS_FDS` and is answered in v1 — and
    /// says nothing at all about NEW-parent/OLD-child. So the guard moves here,
    /// to the transport choice: an older successor is never handed descriptors
    /// it has no code to receive.
    target_not_older: bool,
    /// Sessions to hand over.
    sessions: usize,
    /// The launch environment can be expressed as a MERGE. A LaunchServices
    /// launch merges over this process's environment and cannot express a
    /// removal, so a command that needs one is not representable on this lane.
    environment_is_a_merge: bool,
}

/// Why this attempt may NOT take the out-of-band lane, or `None` when it may.
///
/// Returning the reason rather than a bare bool is what makes a fallback
/// diagnosable: "the update applied but the survivor is still an orphan" and
/// "the update applied through the new lane" look identical from the outside,
/// and the difference is one of these strings.
#[cfg(target_os = "macos")]
#[must_use]
fn out_of_band_lane_refusal(facts: HandoffLaneFacts) -> Option<&'static str> {
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
    if facts.sessions == 0 || facts.sessions > crate::handoff_rendezvous::MAX_RENDEZVOUS_SESSIONS {
        return Some("the session count does not fit one descriptor message");
    }
    if !facts.environment_is_a_merge {
        return Some("the launch environment needs a removal a merge cannot express");
    }
    None
}

/// The `.app` ROOT containing `exe`, when there is one.
///
/// `<bundle>.app/Contents/MacOS/<bin>` — the same two-levels-up shape
/// `menu.rs::bundled_resource` uses for `Contents/Resources`, plus one. The
/// `.app` suffix is CHECKED rather than assumed: `cargo run`, a dev build and
/// the test harness all live three levels below some directory too, and handing
/// LaunchServices one of those would turn a wiring mistake into an opaque
/// launch failure a whole deadline later instead of an immediate fallback.
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

/// Fresh mutable facts required immediately before the attempt-wide Commit CAS.
/// Keeping this conjunction pure gives the derived handoff model a shipping
/// decision seam; `ProofReady` alone never grants replacement authority.
///
/// SEAMLESS ADMISSION (deliberate 2026-07 semantics change): queued PTY OUTPUT
/// is no longer a fact here at all. The screen-carry digest is captured
/// post-park at parser ground and the parent provably consumes no further PTY
/// bytes, so bytes queued in the kernel replay through the child's fresh
/// parser after Commit — the carried checkpoint stays a valid ground-state
/// prefix and nothing is lost. What remains from the old `ptys_still_quiet`
/// conjunct is its fail-closed core, `sessions_alive`: POLLHUP/POLLERR on a
/// master means the shell (live-set identity) died, which must still reject.
/// Queued-but-undispatched HARDWARE input is likewise no longer an admission
/// fact — the completion path DRAINS it (re-posting itself so the run loop
/// dispatches the queued events into the still-open masters) rather than
/// revoking; see `finish_update_handoff`'s drain gate.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HandoffCommitFacts {
    exact_sessions: bool,
    exact_layout: bool,
    /// The activity epoch is unchanged since the attempt was armed.
    ///
    /// MODE-BLIND ON PURPOSE, and mandatory in every lane — including
    /// `AutomaticPastGrace`. That lane drops the idle WAIT at the entry; it does
    /// not get to commit against a snapshot that activity has already moved out
    /// from under. (Several comments in this tree used to claim otherwise; they
    /// were corrected on 2026-08-28, and this is the sentence to trust.)
    exact_activity: bool,
    teardown_allows_commit: bool,
    parent_still_parked: bool,
    sessions_alive: bool,
    /// The OS input queue has been DISPATCHED into the masters: the main thread
    /// ran the event loop for a bounded interval after ProofReady, so every
    /// hardware event the OS had already accepted has flowed through the
    /// tolerated input path to the still-open PTY masters. This replaced "no
    /// hardware event happened in the last 50 ms", which was unsatisfiable on a
    /// machine in use AND never the property that mattered (see the drain gate
    /// in `finish_update_handoff`).
    input_dispatch_fenced: bool,
    /// PROCESS-LOCAL egress is drained to the kernel: no tolerated keystroke is
    /// still sitting in this process's paste-order FIFO or a wedged-tty sink
    /// spill. Distinct from `input_dispatch_fenced` (the OS/AppKit hardware
    /// queue) — that input, once dispatched, may land in THESE queues, and they
    /// die with `_exit` unless flushed to the master first. Below the spec
    /// model's abstraction (the model's single "deliver queued input to the
    /// masters" step), so it is asserted directly rather than model-fired.
    egress_settled: bool,
    native_safe: bool,
    proof_exact: bool,
    commit_channel: bool,
}

/// The Commit-time layout comparison, reduced to the fields that actually mean
/// "window/tab/pane topology". Two of them are normalized away, and each was
/// independently killing healthy in-flight seamless updates:
///
/// WINDOW POSITION. `capture_restore_manifest` reads `outer_position()` LIVE
/// into `outer_x`/`outer_y`, and `WindowLayout` derives `PartialEq` over them,
/// so simply DRAGGING the window during the successor's boot rejected the
/// Commit as "window/tab/pane topology changed". `WindowEvent::Moved` is
/// classified `Tolerated` in `lib.rs` precisely so a drag can never revoke an
/// overlap, and that classification was being defeated right here. The carried
/// position is NOT re-read at spawn: `start_unix_update_handoff` snapshots the
/// `WindowCarry` once, before the readers park, and the worker ships that
/// snapshot verbatim — so a drag during the boot was never going to follow the
/// window across the swap anyway. The successor reappearing at the pre-drag
/// position is cosmetic; losing the whole update over it was not.
///
/// PER-SESSION cwd/title. The post-park capture is proof-protected — the
/// checkpoint loop above it proved every `term` was lockable — but this
/// re-capture has no such proof: it runs later, on the live event loop, where
/// the scrollback-compression worker can still hold a `term` transiently.
/// `restore_session_meta` degrades to `(None, String::new())` on a `WouldBlock`
/// try_lock, so a contended mutex turned real cwd/title into empty ones and the
/// derived `PartialEq` reported that DEGRADATION as a topology change —
/// nondeterministically, for a session that had not changed at all.
///
/// Comparing the projection rather than probing the locks is the race-free fix:
/// a probe-then-capture would only move the contention window, while full
/// equality implies projection equality, so this can only ever admit
/// differences confined to those degradable metadata fields. Admitting them is
/// safe: the child inherits the live shells (cwd/title matter only to a cold
/// respawn), and the digest the child re-proves is taken over the PENDING
/// layout captured at attempt start, never over this re-capture.
#[cfg(unix)]
fn commit_layout_topology(
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
            }
            // Native/placeholder leaves carry no session-lock-derived field, so
            // they keep comparing in full — a native tab appearing or changing
            // IS structural. The terminal leaf's USER metadata
            // (`user_title`/`description`/`icon`/`role`/`attention`) is read
            // under a BLOCKING lock in `view_restore_descriptor`, so it cannot
            // degrade and is deliberately left in the comparison too. It has to
            // stay there: the successor puts those five fields back on the
            // sessions it adopts FROM THE PENDING LAYOUT
            // (`App::carry_restored_identity`, onto the shell each leaf's
            // `local_id` names), so a `meta set` THIS process answers between the
            // capture and the Commit-time re-capture must reject this Commit:
            // admitting it would commit a successor that shows the value from
            // before the edit. That is all the comparison covers. A `meta set`
            // this process answers AFTER the re-capture — its control workers
            // serve until `commit_and_exit` — is lost with it: nothing compares
            // again, and the successor restores the pending value. A `meta set`
            // the SUCCESSOR answers (it serves each adopted bootstrap's sid from
            // its own bind, before its deferred restore) never reaches this
            // process at all; the successor's graft keeps it instead
            // (`session_timeline::restore_carried_meta`).
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
        // Same class as the position: live SHOW STATE, not topology. Captures
        // currently write it on Windows only (this fn is unix-gated), but the
        // derived `PartialEq` covers the field, so normalize it here too —
        // otherwise the day a unix capture starts recording it, zooming the
        // window during the successor's boot would kill a healthy Commit
        // exactly the way dragging it used to.
        window.maximized = None;
        // BOTH projections, deliberately: the capture writes the same live
        // session's cwd/title into the legacy `tabs` mirror and the canonical
        // `restored_tabs` tree, so normalizing only one of them would leave the
        // other still reporting a degraded read as a changed layout.
        for tab in &mut window.tabs {
            strip_pane(tab);
        }
        for tab in &mut window.restored_tabs {
            strip_tree(&mut tab.root);
        }
    }
    topology
}

#[cfg(unix)]
#[must_use]
fn handoff_commit_admitted(facts: HandoffCommitFacts) -> bool {
    facts.exact_sessions
        && facts.exact_layout
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

/// The human-readable reason a ProofReady completion did not Commit, derived
/// from the SAME [`HandoffCommitFacts`] the admission read so the string
/// cascade and the admission can never drift. The two Commit-race flags arrive
/// separately because they are decided after admission; `native_safety` rides
/// along for its `Err` reasons (`facts.native_safe` is its projection).
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
    } else if !facts.exact_activity {
        "structural activity arrived before Commit".to_string()
    } else if !facts.teardown_allows_commit {
        "destructive intent revoked Commit before teardown replay".to_string()
    } else if !facts.sessions_alive {
        "a handed-off PTY session closed before Commit".to_string()
    } else if !facts.input_dispatch_fenced {
        "the OS input queue did not dispatch into the masters before Commit".to_string()
    } else if !facts.egress_settled {
        "tolerated input outlasted the pre-Commit egress-flush budget".to_string()
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

/// TYPED RETRY CLASSIFICATION: whether a rejection was activity-shaped (the
/// terminal's world moved — sessions, layout, epoch, deferred teardown,
/// undrainable typing) versus genuine (safety/proof/channel/arbiter faults).
/// Activity rollback is lossless and repeatable, so automatic mode may spend
/// bounded retry budget on it; the worker's later `Rejected` completion reads
/// the flag this sets in `finish_update_handoff`'s non-ready arm. Never
/// decided by string matching — derived from the same facts as the admission.
///
/// SESSION DEATH IS NOT ACTIVITY (consistency with the worker): a handed-off
/// shell dying mid-overlap is a GENUINE failure — exactly as
/// `wait_handoff_ready` and the worker decision loop classify it (a plain
/// `Rejected` with no activity flag → manual-only). The adoption proof's
/// live-set identity is gone, and reclassifying that as retry-eligible only
/// here — because the main thread happened to observe the HUP first — would
/// spend the automatic budget on a handoff that can never re-prove the same
/// set. `sessions_alive` is therefore deliberately absent from this set.
#[cfg(unix)]
fn handoff_rejection_activity_shaped(facts: HandoffCommitFacts) -> bool {
    !facts.exact_sessions
        || !facts.exact_layout
        || !facts.exact_activity
        || !facts.teardown_allows_commit
        || !facts.input_dispatch_fenced
        || !facts.egress_settled
}

/// The emergency reaper's one job, shared verbatim by the spawned reaper
/// thread and the spawn-failed inline fallback: kill the readerless candidate
/// and PROVE it terminated, release the emergency reaper claim, run the worker
/// cleanup, and report the rejected completion back to the event loop. The
/// completion is what licenses rollback, so it is emitted only after the
/// warrant exists.
#[cfg(unix)]
fn emergency_reap_and_report(
    child_pid: u32,
    attempt_id: u64,
    arbiter: &crate::HandoffAttemptArbiter,
    cleanup: &HandoffWorkerCleanup,
    nonce: Option<String>,
    detail: String,
    proxy: &winit::event_loop::EventLoopProxy<Wake>,
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
            // WE ended this candidate, off a bare pid from the completion wire.
            // Nothing here witnessed a death of its own.
            child_death: crate::ChildDeathEvidence::Unobserved,
        },
    ));
}

#[cfg(unix)]
impl HandoffWorkerCleanup {
    /// Complete all filesystem repair before the UI is told rollback is safe.
    /// Thus the event-loop completion performs no directory scan, unlink,
    /// symlink publication, status read, or child-process probe.
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
    // A real one-way pipe gives every fixed <=PIPE_BUF proof/Commit wire the
    // atomic all-or-nothing write property used by `commit_and_exit`. A byte
    // stream socketpair does not provide that theorem and may short-write.
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

/// WHY a parked PTY reader may be resumed. Rollback restarts a reader on every
/// handed-off master, so it is sound only while nothing else can still be
/// reading those masters: two readers on one master silently interleave and
/// destroy the stream, which is unrecoverable and invisible. Each variant names
/// a fact that rules the overlap candidate out as such a reader.
///
/// There is deliberately NO variant for "we waited as long as we were willing
/// to". A candidate that MIGHT still be alive is exactly the case rollback must
/// not run in, so the functions below have no give-up path — `Child::wait`, the
/// authority they generalize, has none either.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
enum HandoffRollbackWarrant {
    /// The attempt failed before any candidate was spawned, so nothing outside
    /// this process has ever held a handed-off master.
    NoCandidate,
    /// A successor was LAUNCHED, but the out-of-band transfer never completed —
    /// no descriptor of ours left this process, so the successor cannot be
    /// holding, let alone reading, a handed-off master.
    ///
    /// This is the one warrant that does not rest on the candidate being gone,
    /// and it is sound for a reason the others cannot use: on this lane the
    /// descriptors move in ONE `sendmsg`, so "the send did not happen" is a
    /// complete account of what the successor holds. That is strictly stronger
    /// than what the fork lane can say at the same point, where `execve` has
    /// already copied the table. Nothing is killed or waited on here — the
    /// successor discovers the vanished rendezvous, refuses itself, and exits.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    NeverTransferred,
    /// `wait` consumed the candidate: it terminated and THIS process reaped it.
    /// Available only to its parent, and strictly the best answer — reaping is
    /// what frees the pid, so nothing can recycle it between proof and use.
    Reaped,
    /// We were not the candidate's parent, so no `wait` could answer for it, and
    /// the pid it was born at provably no longer names it. See
    /// [`handoff_candidate_terminated`] for the two proofs and why they
    /// establish the same fact `Reaped` does.
    Vanished,
}

#[cfg(unix)]
impl HandoffRollbackWarrant {
    /// Say which authority licensed a rollback, once, at the moment it is used.
    /// The outside proof is worth a line — it is the difference between "we
    /// reaped the candidate" and "the candidate was never ours to reap", i.e.
    /// which launch shape this build actually ran — while the ordinary parent
    /// reap stays as quiet as it has always been.
    fn announce(self, candidate_pid: Option<u32>) {
        if self == Self::Vanished {
            aterm_log::info!(
                "update apply: rollback licensed by outside proof; candidate {candidate_pid:?} \
                 terminated without being ours to reap"
            );
        }
    }
}

/// The kernel's birth record for whatever process currently occupies a pid: the
/// microsecond instant it was created. The kernel assigns it, so no process can
/// choose its own, and two processes that reuse a pid cannot share one. Compared
/// for EQUALITY only — it is an identity token, never a clock reading.
///
/// `seamless::ProcessBirth` reads the same kernel fact for the OPPOSITE
/// conclusion — "is my handoff parent still ALIVE" — and the two are not
/// negations of each other. An unreadable record must make that probe answer
/// dead (fail-safe there: the readerless successor kills itself) and must make
/// this one answer "not proven" (fail-safe here: the parent keeps its readers
/// parked). One shared probe would put one of the two lanes on the dangerous
/// default, so this lane carries its own with its own failure direction.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HandoffCandidateBirth {
    seconds: u64,
    microseconds: u64,
}

/// WHICH process is at `pid` right now — not whether it is alive. Liveness is
/// `kill(2)`'s answer; this is the identity half, and it is deliberately not
/// filtered by process status, because every reason the kernel might decline to
/// answer — including a zombie, which libproc may or may not report — carries
/// the same meaning for this lane: nothing is concluded either way.
#[cfg(target_os = "macos")]
fn read_candidate_birth(pid: u32) -> Option<HandoffCandidateBirth> {
    let pid = libc::pid_t::try_from(pid).ok()?;
    if pid <= 1 {
        return None;
    }
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let size = i32::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
    // SAFETY: `info` points at `size` writable bytes of exactly the structure
    // PROC_PIDTBSDINFO fills; libproc returns the number of bytes it wrote.
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if read != size {
        return None;
    }
    // SAFETY: the exact-size success above initialized the whole record.
    let info = unsafe { info.assume_init() };
    // A record about some OTHER pid could only mislead the comparison this
    // feeds, so a kernel that disagrees about the pid it was asked about is no
    // witness at all.
    if u32::try_from(pid).ok()? != info.pbi_pid {
        return None;
    }
    Some(HandoffCandidateBirth {
        seconds: info.pbi_start_tvsec,
        microseconds: info.pbi_start_tvusec,
    })
}

/// Off macOS there is no birth-record primitive wired here, and none is needed:
/// the successor is unconditionally a fork child there (the `spawn` in
/// [`run_handoff_worker`] is the only launch shape, as the matching stub in
/// `seamless::read_process_birth` records), so `wait` always answers and the
/// fallback authority never has to. A non-fork transport off macOS must
/// implement this FIRST — `/proc/<pid>/stat` field 22 is Linux's equivalent —
/// because without it the fallback can prove termination only by pid vacancy.
#[cfg(all(unix, not(target_os = "macos")))]
fn read_candidate_birth(_pid: u32) -> Option<HandoffCandidateBirth> {
    None
}

/// The process that must be proven terminated before any parked reader resumes,
/// plus whatever identity keeps a RECYCLED pid from impersonating it.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HandoffCandidate {
    pid: u32,
    /// The kernel birth stamp for `pid`, captured while the pid provably still
    /// named the candidate. `None` never weakens the PROOF (pid vacancy alone is
    /// sound — see [`handoff_candidate_terminated`]); it costs only the two
    /// things identity buys: concluding termination from a recycled pid instead
    /// of waiting for a number that will never come free, and keeping a SIGKILL
    /// off whoever recycled it.
    birth: Option<HandoffCandidateBirth>,
}

/// What the kernel says about the pid the candidate was born at, right now.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HandoffCandidateIdentity {
    /// The pid still names the candidate: the kernel's stamp for it equals the
    /// captured one.
    Corroborated,
    /// The pid names a DIFFERENT process. The kernel does not reallocate a pid
    /// before its previous owner is reaped, so the candidate has terminated.
    Recycled,
    /// No stamp was captured, or the kernel will not produce one now. Nothing is
    /// concluded in either direction.
    Unwitnessed,
}

#[cfg(unix)]
impl HandoffCandidate {
    /// Capture the identity of a fork child this process has NOT reaped. The
    /// unreaped entry pins the number — the kernel cannot reallocate a pid a
    /// zombie still owns — so the stamp read here is provably that child's own.
    fn of_unreaped_child(child: &std::process::Child) -> Self {
        let pid = child.id();
        Self {
            pid,
            birth: read_candidate_birth(pid),
        }
    }

    /// Capture the identity of a candidate this process did NOT fork, from the
    /// pid the kernel attested at the rendezvous accept (`LOCAL_PEERPID`).
    ///
    /// The pid is not PINNED the way an unreaped child's is — launchd may reap
    /// the successor at any moment and free the number — so the stamp read here
    /// is what turns a recyclable integer back into an identity. Strictly better
    /// than [`Self::from_bare_pid`], which is what this lane would otherwise be
    /// reduced to, and the reason `signal_handoff_candidate` may aim a DIRECT
    /// signal at a launched candidate at all.
    #[cfg(target_os = "macos")]
    fn of_attested_peer(pid: u32) -> Self {
        Self {
            pid,
            birth: read_candidate_birth(pid),
        }
    }

    /// A candidate known only by its pid. This is what the emergency reaper gets:
    /// the completion wire carries `child_pid`, a bare number, so no stamp can
    /// ride along with it. Sound — vacancy is what proves termination — but it
    /// cannot conclude termination FROM a recycled pid, and it signals exactly
    /// as the pre-0.14 code did (the process group only).
    fn from_bare_pid(pid: u32) -> Self {
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

/// Has the candidate TERMINATED — can it no longer be holding, let alone
/// reading, the handed-off PTY masters?
///
/// Two independent proofs, each about the candidate itself:
///
/// * PID VACANCY. `kill(pid, 0)` answering ESRCH means the kernel will deliver
///   nothing at that number. A running process's pid never changes, so the only
///   way to get that answer about the candidate is for the candidate to have
///   terminated — and a terminated process runs no further user code and holds
///   no descriptors. pid REUSE can only HIDE this answer (somebody else now
///   answers to the number), never fabricate it, so this proof needs no identity
///   check to be sound.
/// * IDENTITY. Something does answer at the pid, but the kernel's birth stamp
///   for it disagrees with the candidate's. A pid is not reallocated until its
///   previous owner has been reaped, so the candidate terminated.
///
/// Both therefore establish what `Child::wait` establishes; they differ only in
/// WHO reaped it. Every other answer — a zombie, an unreadable record, a pid
/// this build cannot convert — is UNPROVEN, and unproven never resumes a reader.
#[cfg(unix)]
#[must_use]
fn handoff_candidate_terminated(candidate: HandoffCandidate) -> bool {
    let Ok(pid) = libc::pid_t::try_from(candidate.pid) else {
        return false;
    };
    // SAFETY: signal 0 performs kill(2)'s existence/permission check only and
    // delivers nothing.
    if unsafe { libc::kill(pid, 0) } != 0
        && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    {
        return true;
    }
    candidate.identity() == HandoffCandidateIdentity::Recycled
}

/// Does THIS process lead its own process group — the precondition that makes a
/// rejecting parent's `kill(-pid)` reach the updater helpers a candidate forks,
/// and not only the candidate itself?
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProcessGroupContainment {
    /// The kernel reports `getpgrp() == getpid()`. A child forked from here
    /// inherits that group, so `kill(-pid)` names a group whose every member
    /// descends from this process.
    OwnGroupLeader,
    /// The kernel reports a group this process does not lead. Two things fail at
    /// once: `kill(-pid)` would not reach a helper forked from here (it is in
    /// the OTHER group), and the group it does name belongs to whoever leads it.
    /// Nothing may fork an updater helper from this state. The two numbers are
    /// carried so the refusal can say what the kernel actually answered — the
    /// errno cannot, since it is not what this decision is read from.
    Foreign {
        group: libc::pid_t,
        own: libc::pid_t,
    },
}

/// Put the calling process in a process group of its own and PROVE it, so that
/// helpers it forks later are inside the group a rejecting parent sweeps.
///
/// This is the successor-side twin of the `pre_exec` `setpgid(0, 0)` in
/// [`run_handoff_worker`], for the launch shape that has no pre-exec hook: a
/// successor started through LaunchServices is launchd's child rather than a
/// fork of ours, so no fork-time hook of the parent's can run inside it and the
/// process has to contain ITSELF. `seamless::prearm_incoming_fds` carries the
/// design note that proposed this, but the call lands in [`crate::main_entry`]
/// instead — still ahead of everything in that process able to run another
/// program — because `prearm_incoming_fds` is also exercised IN-PROCESS by unit
/// tests, and a process-wide, irreversible `setpgid` inside it would move the
/// test binary out of its harness's process group. The ordering obligation the
/// call site owes is stated at that call site.
///
/// THE RETURN VALUE OF `setpgid` IS NOT THE ANSWER; `getpgrp()` IS. At this one
/// call shape — target self, requested group "my own pid" — the only failure the
/// macOS contract admits is EPERM "the process indicated by the pid argument is a
/// session leader". Every other documented error is out of reach here: `EACCES`
/// and `ESRCH` need `pid` to name a CHILD, the other two `EPERM` clauses need a
/// different euid or a `pgid` naming somebody else's group, and `EINVAL` needs a
/// negative or unsupported `pgid`, which 0 — "the target's own pid" — is not.
/// That one refusal reports the property already holding rather than denying it,
/// because a session leader has `pgid == sid == pid`: `setsid` sets the three
/// equal, and `setpgid` refusing session leaders is exactly what stops anything
/// from moving one out again. But this function does not rest on that reading, or
/// on any other enumeration of errnos: it reads the postcondition back from the
/// kernel, so an errno this code did not anticipate is caught by the check
/// instead of being argued away.
///
/// Idempotent, which is what lets callers run it unconditionally on their lane:
/// a process already leading its own group gets a second no-op success, and the
/// updater's boot-apply re-exec preserves the process group across `execve`, so
/// the re-exec'd image re-running this sees the group it established before.
#[cfg(unix)]
#[must_use]
pub(crate) fn contain_own_process_group() -> ProcessGroupContainment {
    // SAFETY: `setpgid(0, 0)` acts on the calling process only; `getpgrp` and
    // `getpid` are side-effect-free getters. The `setpgid` result is discarded
    // deliberately — what this function answers with is the postcondition read
    // back from the kernel immediately after it, for the reason stated above.
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

/// SIGKILL the candidate, before anything waits on it.
///
/// The GROUP sweep (`-pid`) is the pre-existing behaviour and the reason
/// `pre_exec` puts the candidate in a group of its own: it is what stops the
/// candidate's ditto/codesign/spctl descendants from continuing to mutate fixed
/// updater paths after the leader is gone.
///
/// WHICH CONTAINMENT THE SWEEP RELIES ON — the two are not equally strong, and a
/// reader must not assume the second one is the first:
///
/// * A CANDIDATE WE FORKED (today's `spawn`). `run_handoff_worker`'s `pre_exec`
///   `setpgid(0, 0)` runs between fork and exec, so the candidate leads its own
///   group BEFORE its image runs: `-pid` is a valid handle from the instant
///   `spawn` returns, and there is provably no instant at which a helper of the
///   candidate's exists outside that group.
/// * A CANDIDATE WE DID NOT FORK (the LaunchServices lane B3 exists for). The
///   candidate contains ITSELF with [`contain_own_process_group`] on entry,
///   before its own update logic can fork the first helper, and refuses to
///   continue when it cannot — so the "no helper outside the group" property is
///   the same one. What is NOT the same is our knowledge of it: the readiness
///   wire is a fixed proof record with no field for a process-group id, so this
///   process cannot distinguish "the successor contained itself" from "the
///   successor never reached that instruction". On that lane `-pid` is an
///   UNPROVEN sweep and nothing may be concluded from it; what licenses rollback
///   is [`handoff_candidate_terminated`], never the group signal. Carrying an
///   attested pgid is B4's control-socket work.
///
/// The DIRECT signal is what a candidate this process did not fork needs. Such a
/// candidate may not be a group leader at all, and then `-pid` names no group and
/// sweeps nothing. It is withheld unless the identity is CORROBORATED, because a
/// bare pid that has been recycled names a stranger and this lane must never
/// SIGKILL one. With no witness the behaviour is exactly what it has always been:
/// the group sweep alone, aimed at a pid that today's unreaped fork child keeps
/// pinned. That PIN is what keeps an unwitnessed sweep aimed at us, and it is
/// precisely what a candidate launchd owns lacks — once launchd reaps it the
/// number is free, and `-pid` then names whatever group its new owner leads. So
/// on that lane an unwitnessed sweep is not merely unproven, it is unsafe, and
/// the candidate has to arrive with an identity (B2/B4) rather than as a bare
/// pid.
/// Is `pid` still a CHILD of this process, and therefore pinned to its number?
///
/// `false` is the safe direction: it only ever withholds a signal. A child that
/// has already EXITED is still pinned — an unreaped zombie owns its number — so
/// both affirmative answers of [`probe_handoff_candidate`] count here; only
/// `ECHILD` withholds.
#[cfg(unix)]
fn candidate_is_our_child(pid: libc::pid_t) -> bool {
    matches!(
        probe_handoff_candidate(pid),
        HandoffCandidateProbe::Running | HandoffCandidateProbe::Exited
    )
}

/// What this process can say about a candidate WITHOUT changing anything.
///
/// One `waitid(WNOHANG | WNOWAIT)` answers two questions, and both have a caller:
///   * IS IT OURS — `ECHILD` is the discriminator, and measured on this platform
///     it is what both a launchd-owned process and an already-reaped one answer.
///     [`candidate_is_our_child`] needs it before a process-group sweep;
///   * HAS IT DIED YET — [`worker_reject_and_reap_handoff_child`] needs it to know
///     whether the status it is about to reap belongs to the CANDIDATE or to the
///     SIGKILL it is about to send.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HandoffCandidateProbe {
    /// `waitid` refused: not a child of ours (the launched lane), or already
    /// reaped. Nothing may be concluded, and nothing is pinned.
    NotOurChild,
    /// Our child, and it has not exited: whatever ends it has not happened yet.
    ///
    /// NOT "IT REFUSED", however tempting. A candidate observed alive at proof EOF
    /// really has closed the readiness channel without dying — which is what a
    /// deliberate refusal does — but it is also what a process being torn down
    /// looks like for the microseconds between the kernel closing its descriptors
    /// and its zombie state becoming visible. Under exactly the pathological load
    /// this classification exists for, a dying candidate is likelier to be caught
    /// in that window, so reading `Running` as a refusal would put a race back at
    /// the seam a race already broke once.
    Running,
    /// Our child, already exited, and still waitable — so its status is intact for
    /// the reap that follows.
    Exited,
}

/// NON-DESTRUCTIVE BY CONSTRUCTION. `WNOWAIT` leaves an already-exited child in
/// its waitable state, so the reaper's own `wait` still collects it — and still
/// collects the status this probe deliberately does not read.
///
/// `si_signo` rather than `si_pid`/`si_status` is the portability decision: it is
/// a PUBLIC FIELD of libc's `siginfo_t` on both platforms this crate builds for,
/// while the other two are fields on the BSDs and union accessors on Linux. A
/// child that has not changed state leaves the zeroed struct untouched, so
/// `si_signo == 0`; a reportable exit writes `SIGCHLD` there, which is non-zero
/// everywhere. The exit STATUS is read later, in safe Rust, from the
/// `Child::wait` this lane already performs.
#[cfg(unix)]
fn probe_handoff_candidate(pid: libc::pid_t) -> HandoffCandidateProbe {
    let Ok(id) = libc::id_t::try_from(pid) else {
        return HandoffCandidateProbe::NotOurChild;
    };
    // SAFETY: `info` is a zeroed out-parameter of exactly the type waitid fills,
    // and WNOWAIT means this call consumes no child. Zeroing FIRST is what makes
    // the `si_signo` read below meaningful: POSIX does not require an
    // implementation to write that field when WNOHANG finds nothing to report.
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
    if info.si_signo == 0 {
        HandoffCandidateProbe::Running
    } else {
        HandoffCandidateProbe::Exited
    }
}

/// A NON-PARENT'S WITNESS TO HOW A CANDIDATE DIED.
///
/// THE LAUNCHED LANE HAS NO `wait`, and that is not a gap in this file — it is
/// what "launchd's child" means. `waitid` answers `ECHILD` about somebody else's
/// child, so on the SHIPPING macOS lane the parent could observe nothing whatever
/// about a `ChildDied` and every one of them had to be classified from inference.
/// That is precisely how a starved candidate came to be charged as a refusing
/// successor (see [`crate::ChildDeathEvidence`]).
///
/// DARWIN HAS A SECOND CHANNEL FOR EXACTLY THIS FACT. `kqueue`'s `EVFILT_PROC`
/// with `NOTE_EXIT | NOTE_EXITSTATUS` delivers a process's FULL `wait(2)`-encoded
/// status to a watcher that is not its parent. XNU's `filt_procattach` gates
/// `NOTE_EXITSTATUS` on being the parent, the tracer, OR being permitted to
/// `SIGKILL` the target — and this lane SIGKILLs the candidate a few lines later,
/// so the permission it needs is one it demonstrably already holds.
///
/// MEASURED ON THIS PLATFORM (Darwin 25.5.0) against a process deliberately
/// reparented to launchd, i.e. the exact shape of a LaunchServices-launched
/// successor, watched by a process that is not its parent:
///   * `exit(7)` → `fflags` carries `NOTE_EXIT|NOTE_EXITSTATUS` and `data` is
///     `0x0700`: `WIFEXITED`, code 7;
///   * `SIGKILL` → `data` is `0x9`: `WIFSIGNALED`, signal 9.
///
/// Registration succeeded on the orphan in both cases. The failing direction is a
/// registration `ESRCH` — the candidate is already gone — which answers `None` and
/// leaves the verdict exactly where it was before this type existed.
///
/// TWO PROPERTIES MAKE IT STRICTLY BETTER THAN [`probe_handoff_candidate`], not a
/// substitute for it:
///   * THE EVENT IS DURABLE. XNU queues the knote inside `proc_exit` and it stays
///     in THIS process's queue until read, so launchd reaping the candidate cannot
///     take the fact away. A zombie probe loses the same fact to a reap it does
///     not control.
///   * ASKING LATE COSTS NOTHING. So [`observe_candidate_death`] reads it once
///     BEFORE its own SIGKILL — anything there is unambiguously the candidate's
///     own death — and again after the candidate is provably gone, which is sound
///     for every status except a bare `SIGKILL`, the only signal this process
///     ever sends.
#[cfg(target_os = "macos")]
struct CandidateExitWatch {
    kq: std::os::fd::OwnedFd,
    ident: libc::uintptr_t,
}

#[cfg(target_os = "macos")]
impl CandidateExitWatch {
    /// Register WHILE THE CANDIDATE IS STILL ALIVE. On the launched lane that is
    /// the rendezvous accept — the one instant the kernel has just attested the
    /// pid — and the registration is the last thing that has to happen before the
    /// candidate can start dying.
    ///
    /// Every failure answers `None`, and every failure is SAFE: it costs the
    /// evidence, never the reap, and the classification degrades to
    /// [`crate::ChildDeathEvidence::Unobserved`], which retries on a bounded
    /// budget.
    fn watch(pid: u32) -> Option<Self> {
        use std::os::fd::{AsRawFd as _, FromRawFd as _};
        let pid = libc::pid_t::try_from(pid).ok()?;
        // 0, -1 and launchd are `kill`'s special targets and the init process;
        // none is ever a candidate, and none is a process to attach a filter to.
        if pid <= 1 {
            return None;
        }
        let ident = libc::uintptr_t::try_from(pid).ok()?;
        // SAFETY: `kqueue()` takes no arguments and returns a new descriptor or -1.
        let raw = unsafe { libc::kqueue() };
        if raw < 0 {
            return None;
        }
        // SAFETY: `raw` is a fresh descriptor this process exclusively owns.
        let kq = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) };
        // CLOSE-ON-EXEC BY HOUSE RULE RATHER THAN BY NECESSITY: Darwin does not
        // inherit a kqueue across `fork` at all, so nothing this process spawns can
        // carry it and the non-atomic gap after `kqueue()` cannot leak. Set it
        // anyway, so an audit of "which descriptors can leave this process" needs
        // no platform footnote.
        // SAFETY: `F_SETFD` takes an int and touches only this descriptor's flags.
        unsafe { libc::fcntl(kq.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
        let change = libc::kevent {
            ident,
            filter: libc::EVFILT_PROC,
            flags: libc::EV_ADD | libc::EV_ENABLE,
            fflags: libc::NOTE_EXIT | libc::NOTE_EXITSTATUS,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        // NO OUTPUT SLOT, ON PURPOSE. With `nevents == 0` a registration failure
        // comes back through `kevent`'s own return value instead of being buried in
        // an `EV_ERROR` event, which is what keeps "the candidate is already gone"
        // from being confused with "the candidate exited while we were registering".
        // SAFETY: one change entry, live for the call; no event list is requested.
        let rc = unsafe {
            libc::kevent(
                kq.as_raw_fd(),
                &change,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        };
        (rc == 0).then_some(Self { kq, ident })
    }

    /// The candidate's own `wait(2)` status IF THE KERNEL HAS ALREADY RECORDED
    /// ONE. Never blocks (zero timeout) and never reaps — the candidate is not
    /// this process's child to reap.
    fn exit_status(&self) -> Option<std::process::ExitStatus> {
        use std::os::fd::AsRawFd as _;
        use std::os::unix::process::ExitStatusExt as _;
        // SAFETY: a zeroed out-parameter of exactly the type `kevent` fills.
        let mut event: libc::kevent = unsafe { std::mem::zeroed() };
        let timeout = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: one event slot and one timespec, both live for the call; no
        // changes are submitted.
        let rc = unsafe {
            libc::kevent(
                self.kq.as_raw_fd(),
                std::ptr::null(),
                0,
                &mut event,
                1,
                &timeout,
            )
        };
        if rc != 1 || event.flags & libc::EV_ERROR != 0 {
            return None;
        }
        // ABOUT THE RIGHT PROCESS, AND ABOUT AN EXIT. Neither can currently be
        // otherwise — one registration, one filter — and both are cheap to keep
        // true if a second registration is ever added to this queue.
        if event.ident != self.ident
            || event.fflags & libc::NOTE_EXIT == 0
            || event.fflags & libc::NOTE_EXITSTATUS == 0
        {
            return None;
        }
        // The status is the low 32 bits of `data`, in the same `wait(2)` encoding
        // `Child::wait` yields. MEASURED: `exit(7)` gives `0x0700`, `SIGKILL`
        // gives `0x9`.
        let raw = i32::try_from(event.data & 0xffff_ffff).ok()?;
        Some(std::process::ExitStatus::from_raw(raw))
    }
}

/// Off macOS every handoff candidate is a fork child of this process, so `wait`
/// always answers and there is no lane for a non-parent witness to serve. The
/// type stays so the decision tail is ONE function on every unix; nothing here
/// can produce one, because the `watch` constructor is macOS-only and there is no
/// other.
#[cfg(all(unix, not(target_os = "macos")))]
#[allow(dead_code)]
struct CandidateExitWatch;

#[cfg(all(unix, not(target_os = "macos")))]
impl CandidateExitWatch {
    fn exit_status(&self) -> Option<std::process::ExitStatus> {
        None
    }
}

#[cfg(unix)]
fn signal_handoff_candidate(candidate: HandoffCandidate) {
    let Ok(pid) = libc::pid_t::try_from(candidate.pid) else {
        return;
    };
    // `-pid` is kill(2)'s process-GROUP target and -1 is its BROADCAST target,
    // so a pid below 2 must never reach it.
    if pid <= 1 {
        return;
    }
    match candidate.identity() {
        // Somebody else answers to the number now. There is nothing of ours to
        // signal, and signalling would land on them.
        HandoffCandidateIdentity::Recycled => (),
        HandoffCandidateIdentity::Corroborated => {
            // SAFETY: SIGKILL to the candidate's process group and then to the
            // candidate itself, both against a pid just proven to name it.
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
                libc::kill(pid, libc::SIGKILL);
            }
        }
        HandoffCandidateIdentity::Unwitnessed => {
            // A GROUP KILL IS ONLY SOUND WHILE THE PID IS PINNED, and unwitnessed
            // means we cannot tell from the candidate alone. The fork lane pins it:
            // an unreaped child owns its number until it is waited on, so `-pid`
            // still names the group `pre_exec` put it in. A LAUNCHED candidate is
            // nobody's child — launchd may reap it at any moment and free the
            // number — so the same `-pid` can come to name a stranger's group, and
            // SIGKILL to a stranger is not a best-effort sweep, it is damage.
            //
            // The wire cannot answer this: `child_pid` arrives as a bare integer.
            // So ask the KERNEL instead, at the moment of use, with the one probe
            // that is both non-destructive and unforgeable. `WNOWAIT` leaves an
            // exited child waitable — the later `waitpid` still reaps it — and
            // ECHILD is precisely "not a child of mine", which is precisely
            // "not pinned".
            if candidate_is_our_child(pid) {
                // SAFETY: SIGKILL to the candidate's process group, whose number
                // the kernel just confirmed is still held by a child of ours, and
                // then to the candidate ITSELF.
                //
                // The group sweep alone reaches a candidate only while it leads a
                // group of its own, which is true of the lane's own `pre_exec`
                // children and of nothing else: a child that inherited this
                // process's group has no group numbered `pid`, so `-pid` finds
                // nothing and the candidate lives. Nothing downstream survives
                // that — the reject path's `wait` has no deadline, and
                // `wait_for_handoff_candidate_to_terminate` is unbounded ON
                // PURPOSE, so a candidate that is never signalled parks the
                // terminal for as long as it chooses to run.
                //
                // The direct signal needs no argument the sweep did not already
                // need: `candidate_is_our_child` just pinned this number, and a
                // pinned pid is exactly what makes `kill(pid, …)` land on the
                // candidate rather than on a stranger. It is the same pair the
                // corroborated arm sends, for the same reason.
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

/// Block until the candidate is provably terminated, then license rollback.
///
/// UNBOUNDED ON PURPOSE. The proof is a PRECONDITION for resuming the parent's
/// parked readers, not a preference: resuming while the candidate might still be
/// reading the masters is the two-readers-on-one-master corruption the overlap
/// protocol exists to prevent, and it is silent when it happens. So there is no
/// deadline after which this returns anyway — `Child::wait`, the authority it
/// stands in for, blocks without one for exactly the same reason, and a bounded
/// probe that gave up would be strictly weaker than the code it replaces.
///
/// The interval below therefore decides only when a candidate that will not die
/// starts SAYING SO. The parked terminal is the visible symptom either way; the
/// log line is what makes it diagnosable rather than mysterious.
#[cfg(unix)]
fn wait_for_handoff_candidate_to_terminate(candidate: HandoffCandidate) -> HandoffRollbackWarrant {
    /// Probe cadence — the same ~2 ms yield the worker's decision loop uses.
    const PROBE: std::time::Duration = std::time::Duration::from_millis(2);
    /// How long a SIGKILLed candidate may take before this becomes loud, and how
    /// often it repeats afterwards.
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

/// The reap's two products: the rollback warrant, and — when this process's own
/// `wait` answered for the CANDIDATE — the status that `wait` collected.
///
/// THE STATUS USED TO BE DISCARDED (`child.wait().is_ok()`), which threw away the
/// only direct statement a candidate ever makes about why it stopped. It is
/// carried now, and it is an `Option` because on the launched lane nobody's `wait`
/// answers, and because a launcher-shaped child's status belongs to the launcher.
/// It is only ever READ once [`HandoffCandidateProbe::Exited`] has proved the
/// candidate was already dead before this lane signalled it — see
/// [`worker_reject_and_reap_handoff_child`].
#[cfg(unix)]
struct ReapedHandoffCandidate {
    warrant: HandoffRollbackWarrant,
    status: Option<std::process::ExitStatus>,
}

/// Kill the rejected candidate and prove it gone. Runs only on the handoff
/// worker, and the returned warrant is what licenses [`App::rollback_overlap`]
/// to resume the parked readers.
///
/// CONTAINMENT THIS RELIES ON: the FORK lane's. `child` is our own `spawn`, so
/// `run_handoff_worker`'s `pre_exec` `setpgid(0, 0)` established the candidate's
/// group before its image ran, and the opening group sweep in
/// [`signal_handoff_candidate`] therefore reaches its ditto/codesign/spctl
/// helpers. Reached with a candidate this process did not fork, that sweep would
/// be the weaker, unobserved kind — read [`signal_handoff_candidate`] before
/// assuming otherwise. The warrant returned here never rests on the sweep either
/// way: it comes from `wait` on our own child, or from
/// [`wait_for_handoff_candidate_to_terminate`]'s outside proof.
/// Also hands back the candidate's own exit status when our `wait` answered for
/// it; see [`ReapedHandoffCandidate`].
#[cfg(unix)]
fn kill_and_reap_handoff_child(
    candidate: HandoffCandidate,
    handle: &mut HandoffCandidateHandle,
) -> ReapedHandoffCandidate {
    let child = match handle {
        HandoffCandidateHandle::Forked(child) => child,
        HandoffCandidateHandle::Launched(_) => {
            // NOBODY'S `wait` ANSWERS FOR THIS ONE. `waitpid` is not an
            // authority about somebody else's child (it says `ECHILD`, which is
            // not evidence of anything), so the outside proof stands in for it —
            // exactly the substitution B2 built `handoff_candidate_terminated`
            // for. The signal still goes first, for the same reason it does
            // below: descendants must be condemned before anything blocks.
            signal_handoff_candidate(candidate);
            return ReapedHandoffCandidate {
                warrant: wait_for_handoff_candidate_to_terminate(candidate),
                status: None,
            };
        }
    };
    // Signal BEFORE any wait, so descendants are already condemned when the
    // direct child is reaped.
    signal_handoff_candidate(candidate);
    let child_is_candidate = child.id() == candidate.pid;
    // PREFERRED AUTHORITY: `wait` on our own fork child proves termination AND
    // consumes the identity in one step, so nothing can recycle the pid between
    // the proof and its use. It answers only for a child of THIS process
    // (`ECHILD` otherwise), and only about the candidate when the child IS the
    // candidate — a launcher-shaped child (`open -n`, which exits as soon as
    // LaunchServices holds the successor) would be reaped here while proving
    // nothing about the process holding the masters.
    let reaped = child.wait().ok();
    if let Some(status) = reaped
        && child_is_candidate
    {
        return ReapedHandoffCandidate {
            warrant: HandoffRollbackWarrant::Reaped,
            status: Some(status),
        };
    }
    // FALLBACK: no `wait` of ours answers for the candidate. Prove it terminated
    // from the outside instead. The status is withheld with it: a launcher-shaped
    // child's exit describes the launcher, not the process holding the masters.
    ReapedHandoffCandidate {
        warrant: wait_for_handoff_candidate_to_terminate(candidate),
        status: None,
    }
}

/// WHAT THIS PROCESS MAY DO TO THE CANDIDATE — which is a property of how the
/// candidate was started, not of what we would like to do.
///
/// `Child::wait` is the best reap authority there is: it proves termination AND
/// consumes the identity in one step, so nothing can recycle the pid between the
/// proof and its use. It exists only for a process we forked. Modelling that as
/// a type rather than an `Option<&mut Child>` is what keeps the launched lane
/// from silently inheriting a `wait` that would answer `ECHILD` and prove
/// nothing — the compiler makes the caller say which world it is in.
#[cfg(unix)]
enum HandoffCandidateHandle {
    /// Our own `spawn`. `wait` is available and is the preferred warrant.
    Forked(std::process::Child),
    /// launchd's, not ours. Nothing here may `wait`, and the warrant comes from
    /// [`handoff_candidate_terminated`]'s outside proof.
    ///
    /// The [`CandidateExitWatch`] is this lane's ONLY possible statement about how
    /// the candidate died, registered at the rendezvous accept. `None` whenever it
    /// could not be registered, and always `None` off macOS, where nothing
    /// launches a candidate at all.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Launched(Option<CandidateExitWatch>),
}

#[cfg(unix)]
impl HandoffCandidateHandle {
    /// The candidate's own status IF the kernel has already recorded one, WITHOUT
    /// blocking and without reaping.
    ///
    /// `None` on the fork lane by construction: its status comes from `wait`,
    /// which is a stronger authority and the one [`kill_and_reap_handoff_child`]
    /// already collects. Only the launched lane, which has no `wait` to collect,
    /// answers from a witness.
    fn witnessed_exit_status(&self) -> Option<std::process::ExitStatus> {
        match self {
            Self::Forked(_) => None,
            Self::Launched(watch) => watch.as_ref().and_then(CandidateExitWatch::exit_status),
        }
    }
}

/// Acquire the worker's unique reaper capability.  Losing to `Committing`
/// means exactly what it says: this worker must neither signal nor reap the
/// candidate while the UI thread is performing the atomic Commit write.
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

/// The emergency reaper's kill, off a bare `child_pid` from the completion wire
/// (on its own thread, or inline when that thread cannot be created).
///
/// CONTAINMENT THIS RELIES ON: whichever one made the candidate a group leader —
/// and this function cannot tell which, because a bare pid carries no evidence
/// of either. On the fork lane it is `pre_exec`'s, established before the
/// candidate's image ran. On a lane where the candidate was launched instead of
/// forked it would be the candidate's own [`contain_own_process_group`], which
/// no wire reports to us, so the group sweep below would be an unproven
/// best-effort rather than the helper kill it is today. The returned warrant is
/// unaffected either way: it comes from `waitpid` or from the outside proof, and
/// [`signal_handoff_candidate`] states what the sweep does and does not buy.
#[cfg(unix)]
fn emergency_kill_and_reap_handoff_child(pid: u32) -> HandoffRollbackWarrant {
    // PRECONDITION: the caller won the attempt-wide Emergency reaper CAS, so no
    // worker can concurrently consume the candidate's identity. While the
    // candidate is a fork child, that claim also pins `pid` to it (an unreaped
    // zombie owns its number); a candidate launchd owns has no such pin, which
    // is why the fallback below re-derives the fact rather than assuming it.
    // Signal the process group BEFORE any wait: `waitpid` also reaps an
    // already-dead leader, and the old ordering then returned while its
    // ditto/codesign/spctl descendants continued mutating fixed updater paths.
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
        // ECHILD — and, defensively, any other refusal. In the fork lane this
        // can only be a teardown-time reap that already happened. A candidate
        // launchd owns answers this way from the start, and THAT is blocker B2:
        // `waitpid` is simply not an authority about somebody else's child, so
        // the outside proof has to stand in for it. What must never happen is
        // treating the refusal itself as evidence the candidate is gone.
        break false;
    };
    if reaped {
        return HandoffRollbackWarrant::Reaped;
    }
    wait_for_handoff_candidate_to_terminate(candidate)
}

/// PRE-PARK admission peek only: any readable byte OR error/hangup counts.
/// Automatic mode refuses to even BEGIN an overlap while output is actively
/// flowing (the quiet-epoch policy). Once an attempt is in flight this
/// function must NOT be used — mid-flight, queued output is tolerated and
/// only session death revokes; use [`handoff_masters_closed`] there.
#[cfg(unix)]
fn handoff_masters_have_activity(live: &[(u64, i32, i32)]) -> bool {
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
    let activity = libc::POLLIN | libc::POLLHUP | libc::POLLERR | libc::POLLNVAL;
    polled > 0 && fds.iter().any(|fd| fd.revents & activity != 0)
}

/// MID-FLIGHT death peek: true only when a handed-off master reports
/// POLLHUP/POLLERR/POLLNVAL — the session (the live-set identity the adoption
/// proof committed to) is gone or the descriptor is invalid. Readable output
/// deliberately does NOT count: post-park bytes wait gap-free in the kernel
/// queue for the child's fresh parser, so output during the overlap is
/// buffered through, never revoking. `events: POLLIN` is load-bearing despite
/// POLLIN being ignored in the answer: macOS's poll(2) evaluates a PTY
/// master's stream state only for requested events and reports a dead slave
/// as `POLLIN|POLLHUP` — with `events: 0` it reports NOTHING, ever (verified
/// by the paired unit test). The filter to HUP/ERR/NVAL in `revents` is what
/// makes plain readable output invisible here.
#[cfg(unix)]
fn handoff_masters_closed(live: &[(u64, i32, i32)]) -> bool {
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
    let dead = libc::POLLHUP | libc::POLLERR | libc::POLLNVAL;
    polled > 0 && fds.iter().any(|fd| fd.revents & dead != 0)
}

/// Whether every handed-off session's PROCESS-LOCAL egress has reached the
/// kernel — no keystroke tolerated into the overlap is still queued in this
/// process (the paste-order FIFO or a wedged-tty sink spill) where `_exit`
/// would destroy it. A master no longer in the pool is treated as settled: its
/// session is gone, `sessions_alive`/`exact_sessions` own that rejection, and
/// there is no sink left to flush. Cheap on the fast path (no paste in flight,
/// nothing spilled → one relaxed atomic load + one empty-buffer check).
#[cfg(unix)]
fn handoff_egress_settled(pool: &crate::SessionPool, live: &[(u64, i32, i32)]) -> bool {
    live.iter().all(|(_, master, _)| {
        !paste_order::is_ordering(*master)
            && pool
                .iter()
                .find(|session| session.master == *master)
                .is_none_or(|session| session.ctx.sink.egress_drained_to_kernel())
    })
}

fn bind_expected_update_artifact(
    command: &mut std::process::Command,
    attempt: Option<&crate::native_updater_service::ApplyAttemptTicket>,
) {
    const BUILD: &str = "ATERM_UPDATE_EXPECTED_BUILD";
    const COMMIT: &str = "ATERM_UPDATE_EXPECTED_COMMIT";
    const DIGEST: &str = "ATERM_UPDATE_EXPECTED_DMG_SHA256";
    command
        .env_remove(BUILD)
        .env_remove(COMMIT)
        .env_remove(DIGEST);
    if let Some(attempt) = attempt {
        command
            .env(BUILD, attempt.target_build().to_string())
            .env(COMMIT, attempt.target_commit())
            .env(DIGEST, attempt.target_dmg_sha256());
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
            // NOTHING OBSERVED is the right default for every producer but one.
            // Preparation failures and cancels have no candidate at all, and every
            // other returned outcome describes a candidate THIS process ended — a
            // status that is ours, not evidence. Only the `ChildDied` producer
            // overrides it, and only with what it actually saw.
            child_death: crate::ChildDeathEvidence::Unobserved,
        }
    }

    /// Attach what the worker observed about a candidate that died on its own.
    #[must_use]
    fn with_child_death(mut self, death: crate::ChildDeathEvidence) -> Self {
        self.child_death = death;
        self
    }
}

/// Publish a non-ready completion — the event-loop message that runs
/// [`App::rollback_overlap`] and therefore RESUMES the parked readers. The
/// warrant is the point of the name: this must not be called until the caller
/// holds one, because the completion crosses a channel and cannot carry the
/// proof with it.
#[cfg(unix)]
fn send_warranted_handoff_failure(
    warrant: HandoffRollbackWarrant,
    cleanup: &HandoffWorkerCleanup,
    proxy: &winit::event_loop::EventLoopProxy<Wake>,
    current_build: u64,
    completion: crate::UpdateHandoffCompletion,
) {
    warrant.announce(completion.child_pid);
    cleanup.complete(completion.nonce.as_deref());
    // Reader rollback and reducer re-arm are latency-critical. Publish the
    // candidate-terminated fact before waiting behind the updater FIFO for disk
    // facts.
    if proxy
        .send_event(Wake::UpdateHandoffFinished(completion))
        .is_err()
    {
        return;
    }
    if let Some(facts) = cleanup.reconcile.as_ref().and_then(|(worker, ticket)| {
        crate::app_native::collect_native_update_reconcile_facts(worker, *ticket, current_build)
    }) {
        let _ = proxy.send_event(Wake::NativeUpdateReconcileFinished {
            purpose: crate::app_native::NativeUpdateReconcilePurpose::Startup,
            facts,
        });
    }
}

/// Reject, kill, and prove terminated on the worker, only after winning the
/// attempt-wide arbiter. `false` means Commit won the race and the caller must
/// keep the candidate untouched while waiting for Commit success or its explicit
/// failure transfer.
#[cfg(unix)]
fn worker_reject_and_reap_handoff_child(
    job: &HandoffWorkerJob,
    proxy: &winit::event_loop::EventLoopProxy<Wake>,
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
        // WRITE THE EVIDENCE DOWN. The completion detail lands on the update bar's
        // row and stays short, so the durable log is where a future field report finds out which
        // of the three `ChildDied` events this was — and, when the answer is
        // `Unobserved`, that the answer is genuinely unknown rather than assumed.
        aterm_log::warn!(
            "update apply: candidate {} died before proving adoption; observed {child_death:?} \
             (this decides whether the automatic lane retries these bytes or converges on them)",
            candidate.pid
        );
    }
    send_warranted_handoff_failure(
        warrant,
        &job.cleanup,
        proxy,
        job.current_build,
        crate::UpdateHandoffCompletion::failure(
            job.attempt_id,
            Some(nonce.to_string()),
            Some(candidate.pid),
            outcome,
            detail,
        )
        .with_child_death(child_death),
    );
    // GIVE THE COUNTED TRIAL LAUNCH BACK unless the bytes answer for this death —
    // see `forgives_the_counted_trial_launch`, which is the whole of the decision.
    //
    // AFTER THE WAKE, DELIBERATELY, and for the same reason
    // `send_warranted_handoff_failure` publishes before it collects reconcile
    // facts: the rollback resumes the user's parked readers and is
    // latency-critical, while this is a durable ledger edit nothing is waiting on.
    // It is not a free read — `forgive_trial_launch_if_advanced` resolves the
    // staging root (a `mkdir` + `stat` + `chmod`), reads the sentinel, and takes
    // the apply lock to write — and it now runs on the `ChildDied` path too, which
    // is precisely the path a machine under pathological load reaches. The
    // candidate is already dead and the next attempt is minutes away, so nothing
    // depends on this having happened first.
    if forgives_the_counted_trial_launch(child_death, job.current_build, job.target_build) {
        aterm_update::forgive_trial_launch_if_advanced(job.target_build, job.trial_launches_before);
    }
    true
}

/// KILL THE CANDIDATE, PROVE IT TERMINATED, AND SAY WHAT KILLED IT — the whole of
/// the reject path's evidence gathering, in one place so a test can drive it end
/// to end against a real process.
///
/// It was three statements inlined in [`worker_reject_and_reap_handoff_child`],
/// which needs a worker job, an event-loop proxy and an arbiter to call at all;
/// nothing could reach them, and a one-token mutation in any of them inverted the
/// field classification with the whole suite green.
///
/// THE ORDER IS THE PROTOCOL, and each step is here because the step before it
/// destroys something:
///
///  1. BEFORE ANY SIGNAL, ask what is already known. On the fork lane that is
///     [`probe_handoff_candidate`]'s `waitid(WNOWAIT)`; on the launched lane it is
///     the [`CandidateExitWatch`] registered at the accept. Whatever answers here
///     is UNAMBIGUOUSLY the candidate's own death, because this process has not
///     yet touched it.
///  2. THE KILL AND THE TERMINATION PROOF, unchanged, and still the thing that
///     licenses the parked readers to resume.
///  3. AFTER IT IS PROVABLY GONE, ask again. This is not a second guess at step 1
///     — it is a strictly later question with a strictly weaker answer, and
///     [`handoff_child_death`] is what knows which parts of that answer survive.
///     On the fork lane it is the status `wait` just collected; on the launched
///     lane the kqueue knote, which XNU queued inside `proc_exit` and which is
///     therefore GUARANTEED to be there once termination is proven — no race, only
///     an attribution question.
///
/// Only [`crate::UpdateHandoffOutcome::ChildDied`] has a death of the candidate's
/// OWN to describe. Every other outcome is a candidate this process decided to
/// end, so its status is our SIGKILL and is evidence about nothing.
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
    // FIRST ANSWER WINS, in the order they were asked: the pre-kill witness is the
    // only one that needs no attribution argument at all, `wait` is this process's
    // own authority over its own child, and the post-termination knote is the
    // launched lane's last resort.
    let status = witnessed_before_the_kill
        .or(reaped.status)
        .or_else(|| handle.witnessed_exit_status());
    (
        reaped.warrant,
        handoff_child_death(died_before_we_signalled, status),
    )
}

/// Does the reject path GIVE BACK the boot-trial launch this candidate counted?
///
/// Which is one question with the polarity that matters here: the launch stays
/// counted only when the NEW BYTES ANSWER FOR THE DEATH, and every other answer —
/// including "we could not tell" — hands it back.
///
/// ONE QUESTION, ASKED ONCE, so the retry budget and the boot sentinel can never
/// drift apart. Both are counters over the same artifact and both end in the same
/// place if they disagree: the retry schedule keeps launching a candidate, every
/// launch stays counted, and on the third one `check_boot_health` reverts the
/// bundle and marks the build failed — poisoning bytes that never failed, which is
/// exactly what `forgive_trial_launch_if_advanced` was added to prevent.
///
/// The answer is the SHAPE, not the outcome. Gating on `outcome != ChildDied`
/// (what this used to do) exempted the whole of `ChildDied` on the argument that
/// it "IS the crash signal", and that argument died with the classification above
/// it: a starved candidate produces proof EOF as readily as a faulting one, and
/// its launch must be given back. The exemption was SAFE only while `ChildDied`
/// was also capped at two attempts; the moment the classification could retry one
/// six or nine times, an unforgiven count reached `MAX_BOOT_ATTEMPTS` on the third
/// and the retry lane itself became the thing that reverted a healthy bundle.
///
/// EVERY OTHER OUTCOME KEEPS THE ANSWER IT ALWAYS HAD, through the same predicate
/// rather than beside it: a candidate THIS process ended carries
/// [`crate::ChildDeathEvidence::Unobserved`], which is never `Structural`, so the
/// bounded automatic re-attempts a busy machine legitimately makes (`TimedOut` and
/// `ActivityRevoked` are scheduling facts, not evidence against the artifact) go
/// on forgiving exactly as before.
///
/// `false` for everything the parent could not attribute — `Unobserved` above all
/// — because forgiving costs the sentinel nothing it can prove it is owed
/// (`forgive_trial_launch_if_advanced` gives back only a launch that MOVED past
/// this attempt's pre-launch snapshot), while withholding it spends a budget
/// against bytes nobody has evidence against. And the sentinel keeps working
/// either way: the swap is already durable, so a successor that truly cannot boot
/// counts its launches on the user's ordinary relaunches, where nothing forgives.
///
/// THE WHOLE DECISION LIVES HERE, real-apply guard included, so none of it is left
/// at a call site no test can reach: the reject path either calls this and acts on
/// it, or the sentinel keeps the launch.
#[cfg(unix)]
#[must_use]
fn forgives_the_counted_trial_launch(
    death: crate::ChildDeathEvidence,
    current_build: u64,
    target_build: u64,
) -> bool {
    // REAL APPLY ONLY. The QA seam authorizes no newer target, so nothing armed a
    // sentinel for it and there is no counted launch to give back.
    target_build > current_build
        && crate::app_native::PhysicalFailureShape::of_child_death(death)
            != crate::app_native::PhysicalFailureShape::Structural
}

/// TURN ONE `wait(2)` STATUS INTO EVIDENCE, or refuse to.
///
/// THIS PROCESS ONLY EVER SENDS `SIGKILL` (`signal_handoff_candidate`, every arm),
/// and that single fact decides the whole function:
///
///   * AN EXIT CODE CANNOT BE OURS. `SIGKILL` is uncatchable and never yields
///     `WIFEXITED`, so `status.code()` being `Some` is by itself proof that the
///     candidate reached an `exit` instruction — which a starved process never
///     does. This is the tree's COMMONEST refusal (`main_entry` returns without a
///     window when the overlap authority is incomplete, exit code `0`) and it is
///     read unconditionally.
///   * A SIGNAL THAT IS NOT `SIGKILL` CANNOT BE OURS EITHER. `SIGSEGV`, `SIGBUS`,
///     `SIGABRT` and friends arrive from the image executing itself into a wall;
///     nothing in this file sends them.
///   * A BARE `SIGKILL` IS THE ONLY AMBIGUOUS ANSWER, and it is the one
///     `died_before_we_signalled` exists for. With it, the kill is the machine's
///     (macOS jetsam reclaiming memory — the field case). Without it, the honest
///     answer is that we cannot tell ours from theirs, so nothing is claimed.
///
/// THE PRECONDITION USED TO GUARD ALL THREE, and that is what this function's
/// previous version got wrong: it discarded status bits that provably could not be
/// this process's own signal, so a deliberate `exit(0)` — the refusal the whole
/// classification most wants to catch — degraded to `Unobserved` whenever the
/// parent lost a race it deliberately refuses to wait out. The gate now guards
/// exactly the one arm that needs it.
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
    /// `Ok` and `Full` both prove that the worker receiver still owns the
    /// rejection. A full one-slot channel is an already-queued command, not a
    /// disconnected worker and never authority for an emergency reaper.
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
    proxy: &winit::event_loop::EventLoopProxy<Wake>,
    nonce: Option<String>,
) -> bool {
    if job.cancel.try_recv().is_err() {
        return false;
    }
    // Cancellation during PREPARATION precedes the spawn, so there is no
    // candidate to prove anything about.
    send_warranted_handoff_failure(
        HandoffRollbackWarrant::NoCandidate,
        &job.cleanup,
        proxy,
        job.current_build,
        crate::UpdateHandoffCompletion::failure(
            job.attempt_id,
            nonce,
            None,
            // Typed activity classification: a cancel poke during preparation
            // is user/structural activity, never evidence against the staged
            // artifact — automatic mode may re-attempt at a later quiet window.
            crate::UpdateHandoffOutcome::ActivityRevoked,
            "activity revoked handoff during physical preparation",
        ),
    );
    true
}

/// Every preparation failure is raised BEFORE `spawn`, including the one for a
/// `spawn` that itself failed, so no candidate has ever held a master.
#[cfg(unix)]
fn send_handoff_preparation_failure(
    job: &HandoffWorkerJob,
    proxy: &winit::event_loop::EventLoopProxy<Wake>,
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
            crate::UpdateHandoffOutcome::PreparationFailed,
            detail,
        ),
    );
}

#[cfg(unix)]
// The worker's muts serve the macOS handoff arms; on other platforms those arms
// are configured out and the bindings are read-only.
#[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
fn run_handoff_worker(mut job: HandoffWorkerJob, proxy: winit::event_loop::EventLoopProxy<Wake>) {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::process::CommandExt as _;

    if handoff_preparation_cancelled(&job, &proxy, None) {
        return;
    }

    // STAGED-CANDIDATE PRE-VERIFICATION (seamless seam 1), off the GUI thread.
    // The `codesign --deep` + bundle-flock authenticity check that used to freeze
    // the main thread before every handoff runs HERE, as the worker's first real
    // action, so a doomed candidate is refused before any manifest is written or
    // any child is spawned — and the UI thread never blocks on it. Still strictly
    // additive: the child re-runs the complete gate under the apply lock at swap
    // time. A refusal is an ordinary `PreparationFailed` (manual-only).
    if job.verify_staged_candidate {
        let verified = if job.installed_activation {
            // ACTIVATION: the artifact is the bundle under the running executable.
            // Prove it is exactly the authorized sealed identity, codesign-valid and
            // newer than this process — a bundle swapped again since the observation
            // is refused before a single reader is parked.
            aterm_update::preverify_installed_for_handoff(
                job.current_build,
                job.target_build,
                &job.target_commit,
            )
            .map_err(|error| format!("installed bundle failed pre-park verification: {error}"))
        } else {
            aterm_update::preverify_staged_for_handoff(
                job.current_build,
                Some(crate::build_info::GIT_COMMIT),
                Some(job.target_build),
                Some(&job.target_commit),
            )
            .map_err(|error| format!("staged update failed pre-park verification: {error}"))
        };
        if let Err(error) = verified {
            send_handoff_preparation_failure(&job, &proxy, None, error);
            return;
        }
    }
    if handoff_preparation_cancelled(&job, &proxy, None) {
        return;
    }

    // THE LANES SPLIT HERE, BEFORE ANYTHING IS WRITTEN (2026-09-19, the late
    // park). The launched lane arrives with NO capture: the main thread returned
    // with every reader live, and this worker now launches the successor, holds
    // its rendezvous claim, and asks the main thread to park only once the
    // successor has dialled — so the swap, the second `execve` and the boot check
    // all happen while the terminal still echoes. That lane finishes the attempt
    // itself (committed, rolled back or reported) and returns; only a runtime
    // refusal that happened before anything was launched (no bundle, no
    // rendezvous, LaunchServices) hands the attempt back to be FORKED, and then
    // it rejoins here WITH the main thread's capture, exactly where the fork lane
    // — which still parks before it spawns — starts.
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
                        // THE POST-PARK DEADLINE IS DROPPED ON PURPOSE. It bounds
                        // the freeze for a successor that has ALREADY swapped,
                        // re-exec'd and boot-checked with the readers live; the
                        // child forked below has done none of that yet and must
                        // do all of it under the park, which is what the fork
                        // lane has always cost and what `handoff_ready_deadline`
                        // (30 s) has always budgeted. Handing it 3 s would fail
                        // every fork-after-park attempt by construction.
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

    // SNAPSHOT THE TRIAL COUNTER HERE, on the WORKER, after the (slow) codesign
    // pre-verification and as late as the lane allows: taken on the main thread at
    // job construction it both froze the terminal for a `Staging::resolve` (which
    // chmods) and could attribute a THIRD party's launch — counted during our own
    // pre-verification — to this candidate (2026-08-19 round-4 skeptics). (The
    // launched lane takes its own snapshot immediately before its launch; a
    // launch LaunchServices refused counted nothing, so the fork below is the
    // first counted launch on that path too.)
    job.trial_launches_before = aterm_update::trial_launch_count(job.target_build);

    let PreparedArtifacts {
        manifest_path: path,
        layout_path,
        fds_wire: wire,
        expected,
        proof_rd,
        proof_wr,
        commit_rd,
        commit_wr,
    } = match prepare_outgoing_artifacts(&mut job, &nonce) {
        Ok(prepared) => prepared,
        Err(failure) => {
            report_preparation_failure(&job, &proxy, nonce, failure);
            return;
        }
    };

    // THE PROOF TERM TRAVELS WITH THE LANE. `expected` was computed once, over the
    // terms the lane choice picked (PTY DEVICE terms for the launched lane, fd
    // numbers for the fork lane). A launched attempt that falls back to the fork
    // lane at runtime (`ForkInstead`: the rendezvous could not bind, LaunchServices
    // refused) keeps that device-term proof — and a forked successor that inferred
    // its term from "no rendezvous in the environment" hashed fd numbers, so every
    // fallback ended in AdoptionMismatch after the child had already swapped the
    // bundle (2026-08-19 round-3 audit). Say the term explicitly; the successor
    // computes device terms on inherited masters just as well.
    #[cfg(target_os = "macos")]
    {
        let proof_term = match job.lane {
            HandoffLane::OutOfBand => "device",
            HandoffLane::Fork => "fd",
        };
        job.command
            .env(crate::handoff_rendezvous::ENV_PROOF_TERM, proof_term);
    }
    job.command
        .env("ATERM_SEAMLESS_MANIFEST", path)
        .env("ATERM_SEAMLESS_NONCE", &nonce)
        .env("ATERM_SEAMLESS_FDS", wire)
        .env("ATERM_SEAMLESS_LAYOUT", layout_path)
        // The candidate proves "I am the build you authorized" by comparison,
        // and logs which half disagreed when it is not. Forging this can only
        // LOSE a handoff — the parent still compares against its own ticket.
        .env(
            "ATERM_SEAMLESS_TARGET",
            crate::seamless::encode_target_identity(job.target_build, &job.target_commit),
        )
        .env("ATERM_HANDOFF_READY_FD", proof_wr.as_raw_fd().to_string())
        .env("ATERM_HANDOFF_COMMIT_FD", commit_rd.as_raw_fd().to_string());
    // Parental authority: our pid AND the kernel's birth record for it. The
    // record is what lets the successor watch us without being our fork child —
    // see `seamless::AttestedParent`. Encoded there, beside its decoder, so the
    // two halves of the wire cannot drift.
    for (key, value) in crate::seamless::outgoing_parent_env() {
        job.command.env(key, value);
    }
    // Parent descriptors remain CLOEXEC for the WHOLE asynchronous interval.
    // Clear only the fork child's copies immediately before its exec image.
    let mut child_inherit = job
        .capture()
        .live
        .iter()
        .map(|(_, master, _)| *master)
        .collect::<Vec<_>>();
    child_inherit.push(proof_wr.as_raw_fd());
    child_inherit.push(commit_rd.as_raw_fd());
    // SAFETY: the closure performs only async-signal-safe fcntl calls between
    // fork and exec and touches captured integer fd values, not shared memory.
    unsafe {
        job.command.pre_exec(move || {
            // Candidate and every helper it later launches inherit a dedicated
            // process group. Adopted shell PTYs/process groups are unrelated.
            //
            // STRICTLY STRONGER than the successor-side
            // `contain_own_process_group` that covers the launch shape with no
            // pre-exec hook, and the reason this stays here rather than being
            // replaced by it: this runs between fork and exec, so the group
            // exists before the candidate's image does — no ordering argument
            // about "the first thing that can fork a helper" is needed, and the
            // failure below aborts the spawn instead of having to be handled by
            // a process that is already running. Keep both.
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            for fd in &child_inherit {
                let flags = libc::fcntl(*fd, libc::F_GETFD);
                if flags < 0 || libc::fcntl(*fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    if handoff_preparation_cancelled(&job, &proxy, Some(nonce.clone())) {
        return;
    }
    // WHY THIS `spawn` IS STILL HERE, AND WHAT NOW STANDS BESIDE IT.
    //
    // `spawn` makes the successor a fork CHILD of THIS process, and on macOS
    // this process is the process of the launchd job
    // `application.com.aterm.aterm.<hex>.<hex>` that LaunchServices created for
    // this app instance. `seamless::commit_and_exit` then `_exit(0)`s it, so
    // launchd tears that job down and the successor is re-parented to pid 1
    // while still holding a bootstrap (XPC) context belonging to a job that no
    // longer exists. Everything it later spawns inherits the dead domain:
    // `hdiutil` fails ENXIO ("Device not configured") — so the process that
    // just applied an update cannot apply the next one — and the same applies
    // to every other framework needing the app's XPC domain (user
    // notifications, LaunchServices opens). `tests/handoff_launchd_job.rs` is
    // that defect's reproducer and regression guard.
    //
    // THE FIX — launch the successor through LaunchServices so launchd mints it
    // its OWN application job — is the `HandoffLane::OutOfBand` lane above,
    // and it needed four properties this `spawn` gets for free. All four now
    // exist: B1 parent attestation (`seamless::AttestedParent`, the kernel birth
    // record, because a launched successor has ppid 1 from birth), B2 reap
    // authority (`HandoffRollbackWarrant` + `HandoffCandidateHandle`, because
    // `waitpid` answers `ECHILD` about somebody else's child and that is not
    // evidence of anything), B3 process-group containment
    // (`contain_own_process_group` on the successor's entry, since no `pre_exec`
    // hook of ours can run inside a process we did not fork), and B4 transport
    // (`handoff_rendezvous`, since a LaunchServices launch inherits no
    // descriptors at all).
    //
    // SO WHY KEEP FORKING? Because the launched lane is refused for four honest
    // reasons — no `.app` bundle (`cargo run`, a dev binary, the test harness),
    // a `$HOME` long enough to push the rendezvous past `sun_path`, more panes
    // than one `SCM_RIGHTS` message carries, and an authorized target build
    // OLDER than this one (a successor with no out-of-band code must never be
    // handed descriptors it cannot receive). `out_of_band_lane_refusal` names
    // which. On every one of those this lane is the only lane, so it stays
    // exactly as it was: byte-identical behaviour, and the two paths rejoin at
    // `run_handoff_decision` so nothing about the Commit decision can drift
    // between them.
    //
    // B3's RESIDUAL, on the launched lane only: `pre_exec` establishes the
    // candidate's process group before `spawn` returns, so on THIS lane
    // `kill(-pid)` is a valid handle from that instant. A launched successor
    // contains itself instead, and reports nothing, so there `-pid` is an
    // unproven sweep — `signal_handoff_candidate` states what each reaper may
    // conclude. What the rendezvous did close is the identity half: the accept
    // hands back a kernel-attested `LOCAL_PEERPID`, so the DIRECT signal is
    // aimed at a corroborated candidate rather than withheld for lack of one.
    let child = match job.command.spawn() {
        Ok(child) => child,
        Err(error) => {
            send_handoff_preparation_failure(
                &job,
                &proxy,
                Some(nonce),
                format!("handoff process could not start: {error}"),
            );
            return;
        }
    };
    // Capture the candidate's kernel identity while its pid is still PINNED by
    // being an unreaped child of ours. Every later reap authority reads it, so
    // it has to be taken at the one instant the pid provably names the
    // candidate. On the launched lane there is no such pin, and the identity
    // arrives from the rendezvous accept instead (`of_attested_peer`); nothing
    // downstream of here can tell the difference.
    let candidate = HandoffCandidate::of_unreaped_child(&child);
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
        // A forked candidate is our own child: the kernel pinned its identity at
        // the fork, and there is no launch answer to corroborate it with.
        None,
    );
}

/// The physical artifacts and private channels one attempt publishes for its
/// successor, produced by [`prepare_outgoing_artifacts`] from the park's capture.
#[cfg(unix)]
struct PreparedArtifacts {
    manifest_path: String,
    layout_path: std::path::PathBuf,
    fds_wire: String,
    expected: crate::seamless::AdoptionProof,
    /// The two private pipes of one attempt, before either end has been given
    /// away. Which END goes to the successor is the whole protocol (the successor
    /// writes the proof and reads the Commit), and a swapped pair would deadlock
    /// in a way that reads as a slow successor.
    proof_rd: std::os::fd::OwnedFd,
    proof_wr: std::os::fd::OwnedFd,
    commit_rd: std::os::fd::OwnedFd,
    commit_wr: std::os::fd::OwnedFd,
}

/// Why [`prepare_outgoing_artifacts`] stopped. Typed rather than reported so the
/// two callers can attach the warrant each of them actually holds: the fork lane
/// has no candidate at all, while the launched lane is holding a dialled
/// successor that must be stood down and proven gone before the attempt retires.
#[cfg(unix)]
enum PreparationFailure {
    /// The main thread poked the cancel channel: structural activity revoked
    /// the attempt (typed `ActivityRevoked` for the retry budget).
    Cancelled,
    /// A physical step refused; the detail is the status-bar row's sentence.
    Failed(String),
}

/// Every artifact one attempt publishes, in the order the successor consumes
/// them, from the park's capture: the layout round-trip, the control carry's
/// export, the authenticated manifest and its grid sidecars under `nonce`, the
/// layout sidecar, the adoption proof, and the two private pipes. Shared by both
/// lanes so the bytes the successor reads cannot differ by how it was started.
///
/// Reads the capture, so it runs AFTER the park on both lanes; nothing here
/// touches a master, and every reader is stopped (until Commit or rollback), so
/// exporting up to 1 MiB of archive rows per session costs the frozen terminal
/// nothing that a byte-for-byte copy would not.
#[cfg(unix)]
fn prepare_outgoing_artifacts(
    job: &mut HandoffWorkerJob,
    nonce: &str,
) -> Result<PreparedArtifacts, PreparationFailure> {
    let target_build = job.target_build;
    let target_commit = job.target_commit.clone();
    let capture = job
        .capture
        .as_mut()
        .expect("the artifacts are prepared from the park's capture");
    let layout_roundtrip = capture
        .layout
        .to_toml()
        .ok()
        .and_then(|wire| crate::restore::RestoreManifest::from_toml(&wire));
    if capture.layout.is_empty() || layout_roundtrip.as_ref() != Some(&capture.layout) {
        return Err(PreparationFailure::Failed(
            "could not persist the bounded handoff layout".to_string(),
        ));
    }
    if job.cancel.try_recv().is_ok() {
        return Err(PreparationFailure::Cancelled);
    }
    // THE CONTROL CARRY'S EXPORT (round 10), HERE on the worker and not in the
    // freeze: every reader is still parked (until Commit or rollback), so no
    // engine moves, and cloning up to 1 MiB of archive rows per session costs
    // the frozen terminal nothing. What the freeze took is the fence; a
    // session whose archive moved since, or whose lock another thread keeps,
    // carries its counters alone. The turn-id count rides the manifest itself,
    // so a ledger that could not be carried cannot lower it.
    let controls = crate::handoff_carry::export(&capture.carries);
    capture.manifest.next_turn_id =
        crate::handoff_carry::manifest_turn_id(crate::control::turn_ids_minted());
    // The attempt nonce was minted by the CALLER of this writer (2026-09-19): the
    // late park launches the successor with the manifest's path before the
    // manifest exists, so the name must be derivable before the write, and the
    // echoed `outgoing.nonce` below is that very value.
    let Some(outgoing) = crate::seamless::write_outgoing(
        &capture.manifest,
        &capture.fds,
        &capture.screens,
        capture.window.clone(),
        &controls,
        nonce,
    ) else {
        return Err(PreparationFailure::Failed(
            "could not write the authenticated handoff manifest".to_string(),
        ));
    };
    let crate::seamless::OutgoingHandoff {
        manifest_path: path,
        nonce: echoed_nonce,
        fds_wire,
        screen_digest: carried_screen_digest,
    } = outgoing;
    // THE ONE NAME: the late park hands the successor this path BEFORE the
    // writer runs, so the writer's answer must be the path the launch
    // environment would have named from the same nonce.
    debug_assert_eq!(
        crate::seamless::outgoing_manifest_path(nonce).as_deref(),
        Some(std::path::Path::new(&path)),
        "the manifest writer and the launch environment name the manifest differently"
    );
    debug_assert_eq!(echoed_nonce, nonce, "the writer echoes the minted nonce");
    // BYTE-IDENTITY ASSERTION. `capture.screen_digest` was committed on the main
    // thread from the live checkpoints; `carried_screen_digest` was taken over
    // the bytes just written. The child hashes those SAME bytes, so a divergence
    // here would surface later as an unexplained `AdoptionMismatch`. Catch it now
    // as an ordinary, explained preparation failure instead.
    if carried_screen_digest != capture.screen_digest {
        return Err(PreparationFailure::Failed(
            "handoff screen carry did not match the committed screen digest".to_string(),
        ));
    }
    if job.cancel.try_recv().is_ok() {
        return Err(PreparationFailure::Cancelled);
    }
    let layout_path = std::path::Path::new(&path).with_extension("layout.toml");
    if crate::restore::write_to(&layout_path, &capture.layout).is_err() {
        return Err(PreparationFailure::Failed(
            "could not write the attempt-bound handoff layout".to_string(),
        ));
    }
    if job.cancel.try_recv().is_ok() {
        return Err(PreparationFailure::Cancelled);
    }
    let Some(expected) = crate::seamless::adoption_proof(
        nonce,
        target_build,
        &target_commit,
        &capture.layout_digest,
        &capture.screen_digest,
        // NOT `capture.live` — the term the two sides can both compute depends
        // on how the descriptors travel. On the fork lane these are the same
        // vector; on the out-of-band lane the middle field is the PTY device.
        &capture.proof_identities,
    ) else {
        return Err(PreparationFailure::Failed(
            "handoff identity set exceeds the proof format".to_string(),
        ));
    };
    let Some((proof_rd, proof_wr)) = make_cloexec_pipe() else {
        return Err(PreparationFailure::Failed(
            "could not create the adoption-proof channel".to_string(),
        ));
    };
    let Some((commit_rd, commit_wr)) = make_cloexec_pipe() else {
        return Err(PreparationFailure::Failed(
            "could not create the handoff-commit channel".to_string(),
        ));
    };
    if job.cancel.try_recv().is_ok() {
        return Err(PreparationFailure::Cancelled);
    }
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

/// The fork lane's report of a [`PreparationFailure`]: no candidate exists, so
/// the warrant is `NoCandidate` and the completion is exactly what each inline
/// arm used to send — a cancel is `ActivityRevoked`, a refusal is
/// `PreparationFailed`.
#[cfg(unix)]
fn report_preparation_failure(
    job: &HandoffWorkerJob,
    proxy: &winit::event_loop::EventLoopProxy<Wake>,
    nonce: String,
    failure: PreparationFailure,
) {
    match failure {
        PreparationFailure::Cancelled => send_warranted_handoff_failure(
            HandoffRollbackWarrant::NoCandidate,
            &job.cleanup,
            proxy,
            job.current_build,
            crate::UpdateHandoffCompletion::failure(
                job.attempt_id,
                Some(nonce),
                None,
                // Typed activity classification: a cancel poke during preparation
                // is user/structural activity, never evidence against the staged
                // artifact — automatic mode may re-attempt at a later quiet window.
                crate::UpdateHandoffOutcome::ActivityRevoked,
                "activity revoked handoff during physical preparation",
            ),
        ),
        PreparationFailure::Failed(detail) => {
            send_handoff_preparation_failure(job, proxy, Some(nonce), detail);
        }
    }
}

/// How the launched lane's prelaunch phase ended.
#[cfg(target_os = "macos")]
enum Prelaunched {
    /// The attempt is finished either way — committed (this process is gone),
    /// rolled back, or reported — and the worker has nothing left to do.
    Finished,
    /// A runtime refusal BEFORE anything was launched. The main thread has since
    /// parked and sent its capture, and the caller forks with it under the
    /// pre-minted nonce — the trade is the orphaned launchd domain the launched
    /// lane exists to avoid, which the fork lane already makes today, and it
    /// beats not updating at all on a machine LaunchServices refuses.
    ForkInstead {
        nonce: String,
        transfer: Box<HandoffTransferJob>,
    },
}

/// A successor that has DIALLED and is being HELD: the rendezvous it dialled,
/// the accepted claim, its kernel-attested identity and its exit witness. It
/// holds no descriptor — the one `sendmsg` that grants them is the transfer, and
/// that happens only after the park — so standing it down is closing the
/// rendezvous (EOF) and proving it gone, never a rollback of anything.
#[cfg(target_os = "macos")]
struct HeldSuccessor {
    rendezvous: crate::handoff_rendezvous::Rendezvous,
    peer: crate::handoff_rendezvous::ClaimedPeer,
    candidate: HandoffCandidate,
    exit_watch: Option<CandidateExitWatch>,
    dialled_at: std::time::Instant,
}

/// ONE STEP of a pre-grant stand-down, recorded as it is taken so the order can
/// be PROJECTED onto the derived overlap model rather than restated by a test.
///
/// The model's ungranted lane is `UnparkForRepark?` -> `RevokeUngrantedSuccessor`
/// -> (`SuccessorExitsOnRevoke` | `KillUngrantedSuccessor`) ->
/// `ReapUngrantedSuccessor` -> `RetireUngrantedAttempt`, and
/// `UngrantedRetireRequiresReap` is what forbids clearing an attempt whose
/// successor is still alive. A test that types that sequence proves nothing
/// about this file; one that replays what the code EMITTED does.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StandDownStep {
    /// The rendezvous closed: EOF for the successor, which is the revocation.
    Revoke,
    /// It exited on that EOF by its own rule, inside the grace.
    SuccessorExited,
    /// It did not, so it was signalled.
    Kill,
    /// Its termination was proven (a vacant or recycled pid).
    Reap,
}

#[cfg(all(test, target_os = "macos"))]
impl StandDownStep {
    /// The derived model action this step is.
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

/// What [`stand_down_held_successor`] proved.
#[cfg(target_os = "macos")]
struct StoodDownSuccessor {
    warrant: HandoffRollbackWarrant,
    death: crate::ChildDeathEvidence,
    /// `true` when the successor ignored the closed rendezvous for the whole
    /// grace and had to be killed — the one case in which THIS process forgives
    /// its counted trial launch (a successor that exits on EOF forgives its own).
    signalled: bool,
    /// The steps this stand-down actually took, in order.
    steps: Vec<StandDownStep>,
}

/// How long a held successor is given to exit BY ITS OWN RULE after its
/// rendezvous closes, before it is killed. The successor maps a zero-byte
/// `recv` to "the parent closed the rendezvous", forgives its trial launch and
/// exits before any window (`main_entry`), which takes milliseconds; the grace
/// is for a machine under load. Readers are LIVE (or resumed) throughout, so the
/// grace costs the user nothing — it is spent only when the readers are not
/// parked, which the caller guarantees by unparking first.
#[cfg(target_os = "macos")]
const STAND_DOWN_SELF_EXIT_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// How long the worker waits for the main thread to answer a stand-down
/// announcement before revoking anyway. Generous against a busy event loop (the
/// handler itself is a `take` and a reader resume), and far short of the grace
/// it precedes, so a wedged main thread costs a late reader resume rather than a
/// candidate left alive.
#[cfg(target_os = "macos")]
const STAND_DOWN_ACK_BUDGET: std::time::Duration = std::time::Duration::from_millis(500);

/// How long the WORKER holds a dialled successor for a park that never comes:
/// the backstop behind the main thread's own hold cap
/// (`PRELAUNCH_HOLD_MAX`), and shorter than the successor's grant budget
/// (`GRANT_HOLD_BUDGET` in `main_entry`) so the parent stands the successor
/// down by EOF before the successor times out on its own.
#[cfg(target_os = "macos")]
const PRELAUNCH_HOLD_BOUND: std::time::Duration = std::time::Duration::from_secs(4 * 60);

/// Stand a held successor down and PROVE it gone: close the rendezvous (EOF is
/// the stand-down signal the successor already understands), wait the grace for
/// its own exit, then kill and prove termination if it is still there.
///
/// THE ORDER IS THE MODEL'S. `RevokeUngrantedSuccessor` (the EOF) precedes
/// `KillUngrantedSuccessor`, which precedes `ReapUngrantedSuccessor`, and only a
/// reaped attempt may retire (`UngrantedRetireRequiresReap`): a candidate that
/// ever dialled is provably dead before its attempt record is cleared or any
/// retry launches beside it. A successor that exits on the EOF within the grace
/// is proven gone the same way, without a signal.
#[cfg(target_os = "macos")]
fn stand_down_held_successor(
    held: HeldSuccessor,
    grace: std::time::Duration,
) -> StoodDownSuccessor {
    const PROBE: std::time::Duration = std::time::Duration::from_millis(2);
    let HeldSuccessor {
        rendezvous,
        peer,
        candidate,
        exit_watch,
        ..
    } = held;
    // EOF IS THE STAND-DOWN. Closing the accepted stream ends the successor's
    // `recv_with_fds` with zero bytes and no descriptors; dropping the rendezvous
    // closes the listener and unlinks the node, so a re-dial fails closed too.
    drop(peer);
    drop(rendezvous);
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
        // Gone before any signal of ours: whatever the witness recorded is the
        // candidate's OWN death, which is why `died_before_we_signalled` is true.
        steps.push(StandDownStep::SuccessorExited);
        let warrant = wait_for_handoff_candidate_to_terminate(candidate);
        steps.push(StandDownStep::Reap);
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
        // Our own SIGKILL is not evidence about the bytes; `handoff_child_death`
        // reads a bare SIGKILL after our signal as `Unobserved`.
        death: handoff_child_death(false, status),
        signalled: true,
        steps,
    }
}

/// Retire a held, UNGRANTED successor: stand it down, prove it gone, and only
/// then publish the completion that clears the attempt. The sibling of
/// [`worker_reject_and_reap_handoff_child`] for the one state that function
/// cannot describe — a live candidate holding nothing.
///
/// THE MAIN THREAD GOES FIRST, and this is why the announcement is a wait and
/// not a poke (2026-09-19, review round 1). Whether the readers are parked when
/// a stand-down begins is NOT a fact this thread can know: the main thread's
/// park gate re-runs every 20 ms while the successor holds, so a park can land
/// at any instant up to and including the moment the hold loop decides to give
/// up. Passing that guess as a parameter was wrong twice over — a park that
/// landed during the grace would have kept the terminal frozen through the
/// grace, the SIGKILL and the reap, and a park that started afterwards would
/// have frozen it onto a successor that was already dead.
///
/// So `Wake::UpdateHandoffStandingDown` is sent UNCONDITIONALLY and waited for.
/// Its handler runs on the main thread, where the park gate also runs, so the
/// two cannot interleave: it closes the gate for good (`stood_down`) and, if a
/// park had landed, resumes the readers there and then (the model's
/// `UnparkForRepark`). Only after that answer does this thread revoke, which is
/// what makes the modelled order Unpark -> Revoke -> Kill -> Reap -> Retire real
/// rather than merely requested.
///
/// The wait is BOUNDED and fails open: a main thread that cannot answer within
/// [`STAND_DOWN_ACK_BUDGET`] is one that is not going to resume readers either,
/// and a candidate that is never killed is worse than a late unpark. The attempt
/// record outlives all of it until this completion, so no retry can launch
/// beside the candidate being stood down.
#[cfg(target_os = "macos")]
fn retire_ungranted_successor(
    job: &HandoffWorkerJob,
    proxy: &winit::event_loop::EventLoopProxy<Wake>,
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
        // Commit is unreachable before a grant: nothing has been given to the
        // successor, so no proof — and no Commit — can exist for this attempt.
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
    // THE ORDER IT TOOK, in the log and in the completion's evidence. Each step
    // is a derived-model action (`StandDownStep::model_action`), and the tests
    // replay exactly this sequence against the model — so a field report of a
    // stand-down that went wrong names the step, not a symptom.
    aterm_log::info!(
        "update apply: stood the held successor (pid {}) down after holding its claim {} ms — \
         {detail}; steps {:?} ({})",
        candidate.pid,
        held_for.as_millis(),
        stood.steps,
        if stood.signalled {
            "it ignored the closed rendezvous for the grace and was killed and proven terminated"
        } else {
            "it exited on the closed rendezvous by its own rule"
        }
    );
    send_warranted_handoff_failure(
        stood.warrant,
        &job.cleanup,
        proxy,
        job.current_build,
        crate::UpdateHandoffCompletion::failure(
            job.attempt_id,
            Some(nonce.to_string()),
            Some(candidate.pid),
            outcome,
            detail,
        )
        .with_child_death(stood.death),
    );
    // THE COUNTED TRIAL LAUNCH IS FORGIVEN BY EXACTLY ONE PARTY: a successor that
    // exits on the EOF forgives its own (`main_entry`'s claim-error exit); this
    // process forgives only when it had to kill, and only when the death is not
    // the bytes' fault — `forgives_the_counted_trial_launch` is the decision.
    if stood.signalled
        && forgives_the_counted_trial_launch(stood.death, job.current_build, job.target_build)
    {
        aterm_update::forgive_trial_launch_if_advanced(job.target_build, job.trial_launches_before);
    }
}

/// How the worker's hold ended.
#[cfg(target_os = "macos")]
enum HoldOutcome {
    /// The main thread parked and captured; the grant follows.
    Transfer(Box<HandoffTransferJob>),
    /// The successor must be stood down before anything was granted.
    StandDown {
        outcome: crate::UpdateHandoffOutcome,
        detail: String,
    },
}

/// THE HOLD: wait, with every reader live, for the main thread's capture — or
/// for one of the things that end the attempt first. Each slice is a bounded
/// `recv_timeout` on the transfer channel (so the capture is picked up the
/// instant it is sent, which is inside the freeze) followed by the cheap
/// checks: a typed stand-down, the cancel poke, the dialer hanging up or
/// exiting, LaunchServices contradicting the dialer's identity, and the hold
/// bound.
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
            && !corroboration.poll()
        {
            return HoldOutcome::StandDown {
                outcome: crate::UpdateHandoffOutcome::Rejected,
                detail: "LaunchServices names a pid other than the dialer as the successor it \
                         launched"
                    .to_string(),
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

/// A runtime refusal of the launched lane before anything was launched: ask the
/// main thread to park exactly as the dial would have, wait for its capture with
/// every reader live, and hand the attempt to the fork lane. A provenance
/// repair aborts here instead — a forked successor is a replacement that buys it
/// nothing (`SameImageHandoff::requires_out_of_band`) — before any park.
#[cfg(target_os = "macos")]
fn fork_after_park(
    job: &HandoffWorkerJob,
    proxy: &winit::event_loop::EventLoopProxy<Wake>,
    lane: HandoffPrelaunchLane,
    why: &str,
) -> Prelaunched {
    const SLICE: std::time::Duration = std::time::Duration::from_millis(2);
    let HandoffPrelaunchLane {
        nonce,
        transfer,
        stand_down,
        // Nothing has been launched on this path, so nothing will be stood
        // down: the fork below is the attempt's only candidate.
        stand_down_ack: _,
        hold_bound,
    } = lane;
    if job.require_out_of_band {
        send_handoff_preparation_failure(
            job,
            proxy,
            Some(nonce),
            format!("a provenance repair needs the out-of-band lane, and it is unavailable: {why}"),
        );
        return Prelaunched::Finished;
    }
    aterm_log::warn!("update apply: {why}; forking instead — asking the outgoing process to park");
    if proxy
        .send_event(Wake::UpdateHandoffAwaitingPark {
            attempt_id: job.attempt_id,
            dialer_pid: None,
        })
        .is_err()
    {
        send_handoff_preparation_failure(
            job,
            proxy,
            Some(nonce),
            "event loop closed before the park",
        );
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
                send_handoff_preparation_failure(
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
            send_handoff_preparation_failure(
                job,
                proxy,
                Some(nonce),
                format!(
                    "the outgoing process did not park within {} s",
                    hold_bound.as_secs()
                ),
            );
            return Prelaunched::Finished;
        }
    }
}

/// The launched lane, in the late-park order (2026-09-19): bind, launch, accept
/// and HOLD the dial with every reader live; ask the main thread to park; then
/// write the artifacts under the pre-minted nonce, grant on the held stream and
/// hand over to the same decision tail the fork lane uses.
///
/// ORDER IS THE DESIGN. The listener is bound BEFORE the launch, because the
/// successor has to be told a name that already exists; the launch environment
/// names the manifest and layout by PATH before either file exists, because the
/// nonce is minted first; the dial is awaited under the dial deadline with the
/// terminal still echoing, because the successor's dial comes AFTER its whole
/// staged-swap boot apply (`ditto`/`hdiutil`/`codesign` plus a re-exec and the
/// count-only boot check) and that interval — ~1.4 s on this machine — used to
/// be the bulk of the freeze; and the park happens at the dial (or at the next
/// quiet moment after it), so what stays under the park is the capture, the
/// write, one `sendmsg`, and the successor's GUI boot to its first present.
///
/// EVERY FAILURE BEFORE THE TRANSFER KILLS NOTHING BY ROLLBACK AND STRANDS
/// NOTHING BY KILL: no descriptor of ours has left the process, so the
/// successor cannot be a reader; the rendezvous closes (EOF), the successor
/// exits by its own rule, and if it does not it is killed and PROVEN gone
/// before the attempt retires ([`retire_ungranted_successor`]).
#[cfg(target_os = "macos")]
fn run_prelaunched_handoff(
    job: &mut HandoffWorkerJob,
    proxy: &winit::event_loop::EventLoopProxy<Wake>,
    lane: HandoffPrelaunchLane,
) -> Prelaunched {
    use std::os::fd::AsFd as _;

    let Some(bundle) = job.bundle.clone() else {
        // Not a bundle: nothing has moved, so fork rather than skip the update.
        // The fork lane needs no bundle, and a machine that cannot be launched
        // through LaunchServices should still get its update.
        return fork_after_park(
            job,
            proxy,
            lane,
            "this process does not run from a .app bundle",
        );
    };
    let rendezvous = match crate::handoff_rendezvous::Rendezvous::bind(&lane.nonce) {
        Ok(rendezvous) => rendezvous,
        Err(error) => {
            // A stale node, an unwritable support dir, a path that would not fit
            // sun_path: none of it has moved a descriptor, and the fork lane needs
            // no socket at all.
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
    // The launch environment is DERIVED from the very `Command` the fork lane
    // would have used, so argv and every inherited-authority variable are the
    // same on both lanes by construction rather than by two lists staying in
    // sync. What differs is only the transport: no descriptor NUMBER appears
    // here — a launched process inherits none, so those integers would name
    // whatever LaunchServices left in the successor's table — and a rendezvous
    // plus its claim secret appear instead.
    let Some(mut environment) = launch_environment(&job.command) else {
        // A merged launch cannot express a REMOVAL, and the fork lane can — this is
        // the one refusal where forking is not merely acceptable but correct.
        drop(rendezvous);
        return fork_after_park(
            job,
            proxy,
            lane,
            "the launch environment needs a removal a merged launch cannot express",
        );
    };
    // THE ARTIFACTS ARE NAMED BEFORE THEY EXIST: the writer derives the same
    // names from the same nonce after the park (`prepare_outgoing_artifacts`
    // asserts the identity), and the successor reads them only after the grant.
    let Some(manifest_path) = crate::seamless::outgoing_manifest_path(&lane.nonce) else {
        send_handoff_preparation_failure(
            job,
            proxy,
            Some(lane.nonce.clone()),
            "no private control directory to write the handoff manifest into",
        );
        return Prelaunched::Finished;
    };
    let layout_path = manifest_path.with_extension("layout.toml");
    environment.push((
        "ATERM_SEAMLESS_MANIFEST".into(),
        manifest_path.into_os_string(),
    ));
    environment.push(("ATERM_SEAMLESS_NONCE".into(), lane.nonce.clone().into()));
    environment.push(("ATERM_SEAMLESS_LAYOUT".into(), layout_path.into_os_string()));
    environment.push((
        "ATERM_SEAMLESS_TARGET".into(),
        crate::seamless::encode_target_identity(job.target_build, &job.target_commit).into(),
    ));
    environment.push((
        crate::handoff_rendezvous::ENV_RENDEZVOUS.into(),
        rendezvous_path.into(),
    ));
    environment.push((
        crate::handoff_rendezvous::ENV_CLAIM.into(),
        rendezvous.claim().into(),
    ));
    for (key, value) in crate::seamless::outgoing_parent_env() {
        environment.push((key.into(), value.into()));
    }
    let arguments = job
        .command
        .get_args()
        .map(std::ffi::OsStr::to_os_string)
        .collect::<Vec<_>>();

    if handoff_preparation_cancelled(job, proxy, Some(lane.nonce.clone())) {
        return Prelaunched::Finished;
    }
    // SNAPSHOT THE TRIAL COUNTER IMMEDIATELY BEFORE THE LAUNCH, so a third party's
    // launch counted during the (slow) pre-verification above cannot be
    // attributed to this candidate.
    job.trial_launches_before = aterm_update::trial_launch_count(job.target_build);
    let dial_deadline = handoff_ready_deadline();
    let launch_at = std::time::Instant::now();
    // THE LAUNCH IS ISSUED, NOT AWAITED (2026-09-14). The successor dials right
    // after its boot apply — before any NSApplication exists — and LaunchServices
    // answers only once the app has checked in, so the dial is structurally
    // ordered BEFORE the answer. The dial is the liveness signal and the claim
    // secret is the lock; the launch answer is corroboration, read AFTER the dial.
    //
    // A REFUSAL is still a refusal, not a slow answer: nothing was launched, so
    // no successor can dial and the fork lane is untouched and still able to
    // carry this attempt. The rendezvous is dropped on the way out, which closes
    // the listener and unlinks the node.
    let in_flight = match crate::app_launch_successor::begin_launch(
        &bundle,
        &arguments,
        &environment,
        // NEVER activated at launch (2026-09-19): the successor has no window
        // yet and the outgoing one is still the glass; the parent activates
        // it at Commit, when an aterm window had focus at the park
        // (`PendingUpdateHandoff::activate_at_commit`).
        false,
    ) {
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
         awaiting its rendezvous dial"
    );
    // No expected pid at the gate: the claim secret is what admits a dialer, and
    // the kernel-attested peer pid is the identity everything below rests on.
    // The LaunchServices pid, when it lands, corroborates it after the dial.
    let launched: Option<i32> = None;
    let peer =
        match rendezvous.accept_claim(launched, dial_deadline, &|| job.cancel.try_recv().is_ok()) {
            Ok(peer) => peer,
            Err(crate::handoff_rendezvous::RendezvousError::Cancelled) => {
                send_warranted_handoff_failure(
                    HandoffRollbackWarrant::NeverTransferred,
                    &job.cleanup,
                    proxy,
                    job.current_build,
                    crate::UpdateHandoffCompletion::failure(
                        job.attempt_id,
                        Some(lane.nonce),
                        launched.map(pid_for_completion),
                        crate::UpdateHandoffOutcome::ActivityRevoked,
                        "structural activity revoked the handoff before the successor dialled",
                    ),
                );
                return Prelaunched::Finished;
            }
            Err(error) => {
                send_warranted_handoff_failure(
                    HandoffRollbackWarrant::NeverTransferred,
                    &job.cleanup,
                    proxy,
                    job.current_build,
                    crate::UpdateHandoffCompletion::failure(
                        job.attempt_id,
                        Some(lane.nonce),
                        launched.map(pid_for_completion),
                        crate::UpdateHandoffOutcome::TimedOut,
                        format!("the launched successor never claimed the handoff: {error}"),
                    ),
                );
                return Prelaunched::Finished;
            }
        };
    // THE DIAL, with the terminal still echoing. Everything before it — the
    // launch prologue, the bundle swap, the second execve, the boot check — used
    // to sit inside the freeze; now it is the interval this line reports, and the
    // park has not happened yet.
    let dialled_at = std::time::Instant::now();
    aterm_log::info!(
        "update apply: successor dialled — launch->dial {} ms with every reader live; holding \
         its claim until the outgoing process parks",
        dialled_at.saturating_duration_since(launch_at).as_millis(),
    );
    // THE IDENTITY AND THE WITNESS, TAKEN FIRST — before any arm that can end
    // the attempt (2026-09-19, review round 1). A dialer that has been accepted
    // is a live process this attempt is responsible for, and EVERY exit from
    // here owes it the same proof of death that a granted one owes: the two arms
    // below used to publish a completion and return, which cleared the attempt
    // record and let the automatic lane launch a second successor beside a first
    // that was still running. The pid is the KERNEL's attestation for a process
    // we did not fork, plus the kernel's birth stamp for it — together they
    // survive pid reuse, which a bare pid from a completion wire does not — and
    // `EVFILT_PROC` can only be attached to a process that still exists, which
    // is now, while the accept has just proven it alive.
    let candidate = HandoffCandidate::of_attested_peer(pid_for_completion(peer.pid()));
    let exit_watch = CandidateExitWatch::watch(candidate.pid);
    if exit_watch.is_none() {
        aterm_log::warn!(
            "update apply: could not watch launched candidate {} for exit; a death on this \
             lane will be reported as Unobserved",
            candidate.pid
        );
    }
    let dialer_pid = peer.pid();
    let held = HeldSuccessor {
        rendezvous,
        peer,
        candidate,
        exit_watch,
        dialled_at,
    };
    // Corroboration, not the lock: if LaunchServices has answered by now, the pid
    // it names must be the dialer's. A mismatch is a stranger that knew the
    // secret — refuse this peer. No answer yet is what the ordering predicts
    // (the dial is structurally before the answer), so NOTHING WAITS HERE: the
    // answer is polled beside the hold and the proof wait instead
    // (`LaunchCorroboration`); a late mismatch still refuses before Commit.
    let mut corroborated = false;
    match in_flight.wait(std::time::Duration::ZERO) {
        Ok(successor) if successor.pid() == dialer_pid => {
            corroborated = true;
            aterm_log::info!(
                "update apply: LaunchServices corroborates the dialer as pid {}",
                successor.pid()
            );
        }
        // A STRANGER THAT KNEW THE SECRET. Refuse it — and prove THE DIALER
        // gone, because the dialer is the process this attempt accepted and is
        // responsible for; the pid LaunchServices names goes in the sentence,
        // not in the completion, so nothing downstream acts on a process this
        // parent never touched.
        Ok(successor) => {
            let detail = format!(
                "the dialer (pid {dialer_pid}) is not the successor LaunchServices launched \
                 (pid {}); refusing the handoff",
                successor.pid()
            );
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
        Err(crate::app_launch_successor::LaunchError::Timeout(_)) => aterm_log::info!(
            "update apply: LaunchServices has not answered yet; the dial is the liveness \
             signal — the answer is read beside the hold and the proof wait"
        ),
        // A late REFUSAL after a dial: the peer is real (it dialled with the
        // secret); LaunchServices' answer is about a job that evidently ran.
        // Log it, keep going — the kernel-attested pid is the identity.
        Err(error) => {
            corroborated = true;
            aterm_log::warn!(
                "update apply: LaunchServices answered the launch with an error after the \
                 successor had already dialled ({error}); proceeding on the attested dialer"
            );
        }
    }
    // A DIALER THAT ALREADY HUNG UP holds nothing — no descriptor has left — but
    // POLLHUP is not a death: it says the SOCKET closed, and the successor's own
    // error return closes it and then runs its exit path. Prove it gone like
    // every other stood-down dialer, which for one that really did exit is a
    // single `kill(pid, 0)`.
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
    let mut corroboration = (!corroborated).then_some(LaunchCorroboration {
        in_flight: &in_flight,
        dialer_pid,
        answered: false,
    });
    // THE CUE. The main thread parks when its gate admits — at once for an
    // explicit apply, at the next quiet moment for the automatic lane — and
    // sends the capture down the transfer channel.
    if proxy
        .send_event(Wake::UpdateHandoffAwaitingPark {
            attempt_id: job.attempt_id,
            dialer_pid: Some(candidate.pid),
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
    // THE PARK LANDED. From here the readers are parked and NOTHING has been
    // granted: every failure until the `sendmsg` releases the readers first and
    // then stands the successor down.
    let HandoffTransferJob {
        capture,
        proof_deadline,
    } = *transfer;
    let park_at = capture.park_at;
    job.capture = Some(capture);
    aterm_log::info!(
        "update apply: the outgoing process parked {} ms after the dial; writing the \
         artifacts and granting on the held claim",
        park_at.saturating_duration_since(dialled_at).as_millis()
    );
    let prepared = match prepare_outgoing_artifacts(job, &lane.nonce) {
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
        Err(PreparationFailure::Failed(detail)) => {
            retire_ungranted_successor(
                job,
                proxy,
                &lane,
                held,
                crate::UpdateHandoffOutcome::PreparationFailed,
                detail,
            );
            return Prelaunched::Finished;
        }
    };
    // A dialer that hung up DURING the hold or the write holds nothing and is
    // owed nothing but a proof of its death.
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
    // `capture.live` is `(local_id, child-owned master duplicate, shell pid)`;
    // the transfer wants `(local_id, shell pid, master)` in the order it will
    // send them, and that order is what addresses the descriptors on arrival.
    let sessions = job
        .capture()
        .live
        .iter()
        .map(|(local_id, master, pid)| {
            // SAFETY: the capture owns these duplicates for the whole attempt
            // (`_owned_masters`), so the borrow cannot outlive them.
            (*local_id, *pid, unsafe {
                std::os::fd::BorrowedFd::borrow_raw(*master)
            })
        })
        .collect::<Vec<_>>();
    let transfer_failed = held
        .peer
        .transfer(
            &lane.nonce,
            &sessions,
            prepared.proof_wr.as_fd(),
            prepared.commit_rd.as_fd(),
            proof_deadline,
        )
        .err();
    if let Some(error) = transfer_failed {
        // WHICH SIDE OF THE `sendmsg` (2026-09-14, audit AH-6). Only a failure
        // BEFORE the descriptors left is an ungranted stand-down; after it the
        // successor holds duplicates of every master, and the rollback owes the
        // same candidate proof the fork lane owes from `execve` on — kill, reap
        // or witness, then resume the readers. In practice the partial shape is
        // EPIPE from a successor that already closed its socket (and, in code,
        // dropped what it received), so the proof is quick; it is still a proof.
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
        retire_ungranted_successor(
            job,
            proxy,
            &lane,
            held,
            crate::UpdateHandoffOutcome::Rejected,
            format!("the handoff descriptors could not be delivered: {error}"),
        );
        return Prelaunched::Finished;
    }
    aterm_log::info!(
        "update apply: granted on the held claim — park->transfer {} ms (launch->dial was {} \
         ms, with every reader live)",
        park_at.elapsed().as_millis(),
        dialled_at.saturating_duration_since(launch_at).as_millis(),
    );
    // From here the successor holds copies of everything, so this is the first
    // instant at which a rollback owes a candidate proof — and from here the
    // decision is identical to the fork lane's.
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
    // The attempt reached the shared decision tail, so it is finished either way.
    Prelaunched::Finished
}

/// A launched pid in the `u32` shape the completion wire and [`HandoffCandidate`]
/// use.
///
/// The conversion cannot fail in practice — `app_launch_successor` refuses any
/// pid that is not positive, and the rendezvous only accepts a peer whose
/// kernel-attested pid equals that one — so this exists for the case where it
/// somehow does. Zero is the safe answer rather than a wrapped one because every
/// consumer already refuses it: `signal_handoff_candidate` returns for `pid <= 1`
/// (0 and -1 are `kill`'s "my whole process group" and "broadcast"), and
/// `handoff_candidate_terminated` reads a vacancy it can never prove.
#[cfg(target_os = "macos")]
#[must_use]
fn pid_for_completion(pid: i32) -> u32 {
    u32::try_from(pid).unwrap_or_default()
}

/// The launch environment a `Command` describes, or `None` when it cannot be
/// expressed as a MERGE.
///
/// `NSWorkspaceOpenConfiguration.environment` MERGES over the launching
/// process's environment; it cannot REMOVE a variable. `Command::env_remove`
/// can, and `bind_expected_update_artifact` uses it: a successor that inherits a
/// stale `ATERM_UPDATE_EXPECTED_*` would authenticate its staged bundle against
/// the wrong artifact. So a removal that would actually remove something — the
/// variable is set in THIS process — makes the whole lane unavailable, and the
/// attempt forks instead. A removal of something not set is a no-op and is
/// ignored, which is the common case (`env_remove` is called unconditionally).
#[cfg(target_os = "macos")]
fn launch_environment(
    command: &std::process::Command,
) -> Option<Vec<(std::ffi::OsString, std::ffi::OsString)>> {
    let mut pairs = Vec::new();
    for (key, value) in command.get_envs() {
        match value {
            Some(value) => pairs.push((key.to_os_string(), value.to_os_string())),
            None if std::env::var_os(key).is_some() => return None,
            None => (),
        }
    }
    Some(pairs)
}

/// Everything after a candidate exists: the bounded readiness wait, the
/// ProofReady wake, and the decision loop that ends in Commit or a warranted
/// rejection.
///
/// SHARED BY BOTH LANES ON PURPOSE. This is the part that decides whether the
/// user's sessions change hands, and it must not be able to differ between a
/// forked and a launched successor — the only thing that legitimately differs is
/// HOW the candidate is reaped, and that is `handle`'s job, not this function's.
#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
fn run_handoff_decision(
    job: &HandoffWorkerJob,
    proxy: &winit::event_loop::EventLoopProxy<Wake>,
    nonce: &str,
    expected: crate::seamless::AdoptionProof,
    proof_rd: &std::os::fd::OwnedFd,
    commit_wr: std::os::fd::OwnedFd,
    candidate: HandoffCandidate,
    handle: &mut HandoffCandidateHandle,
    deadline: std::time::Instant,
    corroboration: Option<LaunchCorroboration<'_>>,
) {
    let live = job.capture().live.clone();
    let proof_outcome = wait_handoff_ready(
        proof_rd,
        expected,
        &job.cancel,
        &live.iter().map(|(_, fd, _)| *fd).collect::<Vec<_>>(),
        deadline,
        corroboration,
    );
    if proof_outcome != crate::UpdateHandoffOutcome::ProofReady {
        // `ChildDied` IS proof EOF and nothing more: a successor that REFUSES this
        // handoff, one that CRASHED, and one the machine STARVED all close the
        // readiness channel identically. For five field failures the verdict did
        // not have to tell them apart, because the refusing successor said nothing
        // at all; it says so now (`seamless::take_target_identity`, and the
        // degraded-authority exit in `main_entry`), and the reject path below adds
        // what the PARENT saw (`handoff_child_death`). Point the next reader at
        // both rather than leaving `ChildDied` looking like an accusation against
        // the bytes. Log-only: the completion detail stays short because it is a
        // status-bar row's detail.
        if proof_outcome == crate::UpdateHandoffOutcome::ChildDied {
            aterm_log::warn!(
                "update apply: the successor closed the readiness channel without proving \
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
    // The proof checked out, so the successor has consumed everything this
    // attempt published. A successor built before the control carry never
    // reads the `.ctl` sidecars, and nothing else would ever remove the turn
    // text and scrolled-off rows in them: unlink them now, Commit or not.
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
        // A candidate that PROVED adoption is alive and holding the masters; it
        // has no death to describe.
        child_death: crate::ChildDeathEvidence::Unobserved,
    });
    if proxy.send_event(ready).is_err() {
        // No main-thread final validation occurred, therefore Commit is impossible.
        // Kill/reap readerless child; never exit as though authority was granted.
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
        // SEAMLESS: readable PTY output no longer revokes here — post-park
        // bytes wait gap-free in the kernel for the child. Only an explicit
        // cancel poke (structural/mode-changing activity, typed as
        // ActivityRevoked for the retry budget) or session DEATH (HUP/ERR —
        // the proof's live-set identity is stale) rejects before Commit.
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
                crate::UpdateHandoffOutcome::Rejected,
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

/// Ceiling on the DECODE AUTHORITY one capture may hand its successor: the sum,
/// over every carried session, of twice `dimension_grid_cap`.
///
/// This is authority, not memory. `dimension_grid_cap` prices a carried line at
/// 512 bytes per cell plus 16 KiB of framing precisely so a hostile `meta` cannot
/// authorize a huge decode, and a real screen encodes an order of magnitude or two
/// smaller than that ceiling — so this sum overstates what a capture actually
/// costs by roughly the same factor.
///
/// It was 256 MiB, the number the EXACT guards use on REAL bytes, and that made it
/// the next rung of the same ladder `MAX_HANDOFF_AGGREGATE_GRID_CELLS` was on: one
/// session carrying 256 lines at 110 columns books ~42 MiB of it, so SEVEN
/// ordinary panes hard-failed the capture — with no degrade rung at all. Raising
/// only the cell aggregate would have moved the field failure from five panes to
/// seven instead of removing it.
///
/// 16x that number is the pessimism this pre-check carries over the bound it is
/// standing in for, so at 4 GiB it goes back to being what it is: a cheap early
/// refusal for an absurd pool, with a clearer message than the seams downstream.
/// The exact bound is untouched and still decides — `screen_digest_refs` and
/// `write_outgoing` both charge `MAX_HANDOFF_AGGREGATE_GRID_BYTES` (256 MiB)
/// against the REAL serialized bytes before anything is committed or written.
#[cfg(unix)]
const MAX_HANDOFF_CAPTURE_BUDGET_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// Acquire a parked session's engine within the freeze budget instead of failing
/// on the first contention (2026-09-14). After `park_all_readers` every reader is
/// joined, but the scrollback-compression worker (and the render/status paths)
/// can take the mutex the instant a reader releases it, and under a flood they
/// did so on every attempt: an explicit apply while any tab streamed output
/// failed deterministically in ~4 ms with "a terminal engine was busy", and the
/// automatic lane burned an attempt per cycle on it (QA runs 1–3 on this
/// machine; runs 4–6, idle or with the flood finished, committed). The budget
/// that already bounds the capture — 250 ms explicit, 20 ms first automatic —
/// is what the wait spends; a poisoned lock is taken as before.
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
                    return None;
                }
                std::thread::yield_now();
            }
        }
    }
}

/// The wall-clock window the park + capture of a seamless handoff must fit in, measured
/// from the instant the readers park — the freeze the user actually feels.
///
/// 20 ms is the imperceptible default and stays the FIRST automatic attempt's budget.
/// It is not a correctness bound, and treating it as one made an update refuse forever
/// on a machine that was merely busy: measured 2026-09-10 on m22 with two `trustc`
/// processes pinning the cores (load average 12.8), the automatic attempt missed the
/// park deadline and the owner's manual attempt three minutes later missed the capture
/// deadline — same 20 ms, same five sessions, and nothing about either would ever
/// change while the compiler ran. The module's own rule for the scrollback carry is
/// "the failure mode is less scrollback, never the update did not apply"; this is that
/// rule applied to time. A miss widens the window (80 ms, then a quarter second),
/// and the widening happens INSIDE one attempt — a park that missed re-parks on the
/// next rung 500 ms later with the successor still holding
/// (`PRELAUNCH_MAX_PARK_MISSES`) — as well as across attempts after a physical
/// failure. An EXPLICIT apply gets the widest window at once: the user asked for
/// the update, not for a sub-frame swap.
pub(crate) fn handoff_freeze_budget(
    mode: crate::native_updater_service::ApplyMode,
    prior_physical_failures: u8,
) -> std::time::Duration {
    use crate::native_updater_service::ApplyMode;
    let ms = match mode {
        ApplyMode::Immediate | ApplyMode::CleanQuit => 250,
        ApplyMode::Automatic | ApplyMode::AutomaticPastGrace => match prior_physical_failures {
            0 => 20,
            1 => 80,
            _ => 250,
        },
    };
    std::time::Duration::from_millis(ms)
}

/// Whether one session may spend `optional` on a budget, on top of the
/// `own_mandatory` it must spend regardless.
///
/// `used` is what the pool has already been charged, and `later_mandatory` is the
/// summed non-degradable cost of every session NOT yet admitted. Overflow is a
/// refusal, never a wrap.
///
/// That last term is the whole point, and it is what both capture budgets were
/// missing. They were handed out greedily in pool order: the first sessions each
/// took a full 256 lines of scrollback, and a later session then found no room
/// even for its VISIBLE screen — which is not degradable, so the capture failed
/// and the update did not apply, deterministically, on every retry, for anyone
/// with more than a handful of panes. Reserving the mandatory total up front
/// inverts that: a session can be refused only when the pool's visible-only total
/// genuinely exceeds the ceiling, which is what makes "the failure mode is *less
/// scrollback*, never *the update did not apply*" true rather than aspirational.
#[cfg(unix)]
fn optional_carry_fits(
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

/// `$ATERM_NO_SEAMLESS_UPDATE` is set: the ONE deliberate opt-out from the overlap
/// handoff. A plain read every time, never memoised. Folded into
/// [`App::seamless_handoff_unavailable`], the reading the apply gate and the
/// status bar's posture share.
#[must_use]
pub(crate) fn seamless_handoff_opted_out() -> bool {
    // Value semantics via the ONE shared flag rule (`env_flag_engaged`):
    // unset, EMPTY and "0" are NOT an opt-out. `is_some()` here made a
    // present-but-empty inherited var silently reroute every update to the
    // cold lane — the same species as the empty-$ATERM_CONTROL_SOCK veto
    // fixed on 2026-09-01, and inconsistent with $ATERM_NO_CONTROL_SOCK,
    // whose empty value has always meant "not disabled".
    aterm_types::control_socket::env_flag_engaged(
        std::env::var_os("ATERM_NO_SEAMLESS_UPDATE")
            .map(|v| v.to_string_lossy().into_owned())
            .as_deref(),
    )
}

/// Why the in-session overlap handoff cannot run in THIS process. ONE predicate
/// ([`App::seamless_handoff_unavailable`]) answers it for both readers — the
/// apply gate in `start_unix_update_handoff` (`seamless_capable`) and the status
/// bar's posture (`App::apply_posture_for`) — so the bar can never promise a
/// landing, or a manual affordance, that the gate refuses. With any of these the
/// admission classifier (`native_update_admission::classify`) admits only the
/// cold lane, which it grants at exactly zero live PTYs: the apply is REFUSED
/// while any terminal is open, no shell dies, and the build lands at the next
/// launch (or once every terminal is closed). Until 2026-08-30 the gate AND-ed
/// four conjuncts while the posture folded only the first, so a `--control-sock`
/// or `--headless` process painted "applies in place within ~2 min" over an apply
/// the gate refused with any terminal open.
/// Fixed control sockets support overlap too: the candidate's control worker
/// shares the authenticated reader gate and binds only after Commit and the
/// parent's exit. Before Commit the parent alone owns the socket and token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HandoffUnavailable {
    /// `$ATERM_NO_SEAMLESS_UPDATE` — the one deliberate opt-out.
    OptedOut,
    /// A fixed path is configured, but this process never proved it bound it.
    UnownedControlSocket,
    /// `--headless` / `$ATERM_HEADLESS`: no window to hand across.
    Headless,
    /// No event-loop proxy to drive the handoff's wakes — only
    /// `headless_for_test` builds such an App.
    NoEventLoopProxy,
}

impl HandoffUnavailable {
    /// Every reason, in the order the predicate asks: the deliberate opt-out
    /// first (the one a person set on purpose), then the launch shape, then the
    /// process shape.
    pub(crate) const ALL: [Self; 4] = [
        Self::OptedOut,
        Self::UnownedControlSocket,
        Self::Headless,
        Self::NoEventLoopProxy,
    ];

    /// What is set — the clause the reader can act on, which is why every
    /// sentence built on it leads with it (the status bar truncates from the
    /// right when the window is narrow).
    #[must_use]
    pub(crate) fn cause(self) -> &'static str {
        match self {
            Self::OptedOut => "$ATERM_NO_SEAMLESS_UPDATE is set",
            Self::UnownedControlSocket => {
                "the configured control socket is not owned by this process"
            }
            Self::Headless => "--headless (no window)",
            Self::NoEventLoopProxy => "no event loop",
        }
    }

    /// The log line's remedy, for the reasons that have one.
    #[must_use]
    pub(crate) fn remedy(self) -> &'static str {
        match self {
            Self::OptedOut => " Unset it to restore the default.",
            Self::UnownedControlSocket => {
                " Resolve the control-socket ownership conflict before updating."
            }
            Self::Headless => " A windowed launch restores the default.",
            Self::NoEventLoopProxy => "",
        }
    }

    /// Whether this reason holds for `app` — a plain read each time.
    fn holds_for(self, app: &App) -> bool {
        match self {
            Self::OptedOut => seamless_handoff_opted_out(),
            Self::UnownedControlSocket => {
                use aterm_types::control_socket::{SocketDirective, socket_directive};
                let socket = std::env::var_os("ATERM_CONTROL_SOCK")
                    .map(|v| v.to_string_lossy().into_owned());
                let disabled = std::env::var_os("ATERM_NO_CONTROL_SOCK")
                    .map(|v| v.to_string_lossy().into_owned());
                match socket_directive(socket.as_deref(), disabled.as_deref()) {
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

impl App {
    /// The ONE reading of whether the overlap handoff can run here — the first
    /// true reason in [`HandoffUnavailable::ALL`]'s order, or `None` when the
    /// seamless lane is available. Read by the apply gate
    /// (`start_unix_update_handoff`) and by the status bar's posture
    /// ([`App::apply_posture_for`]), and nowhere else, so the two cannot
    /// disagree. Plain reads only — environment, two fields and the published
    /// bound-socket witness; no filesystem probes or recursive memo: a
    /// memo whose initializer could call back into its owner parked the main
    /// thread forever on 2026-08-30.
    ///
    /// NOT folded into `arm_native_auto_apply`'s `enabled` on purpose: the cold
    /// lane is a real automatic path for a headless or opted-out process
    /// whose last terminal has closed (the classifier admits it at zero live
    /// PTYs, and the cold spawn re-injects `ATERM_HEADLESS`), so a lane that
    /// never arms would never take it.
    #[must_use]
    pub(crate) fn seamless_handoff_unavailable(&self) -> Option<HandoffUnavailable> {
        HandoffUnavailable::ALL
            .into_iter()
            .find(|why| why.holds_for(self))
    }
}

impl App {
    /// PROOF-CARRYING DSU (RFC Rung 1): APPLY a staged update now by re-execing — the
    /// staged build swaps in at the top of the new `main` (`apply_staged_if_ready`).
    /// Reached from `Wake::ApplyStagedUpdate` (the `aterm-ctl update apply` verb / the
    /// GUI's update-ready nudge). No-op unless a STRICTLY-NEWER build is actually staged
    /// (never a pointless re-exec). Rung 1b live wiring (DEFAULT-ON, opt out with
    /// `ATERM_NO_SEAMLESS_UPDATE`): hands every live PTY master, its exact visible-screen
    /// checkpoint, and a `SessionHandoff` manifest to the new process so the running shell survives
    /// (the round-trip that makes that safe is
    /// proven — `SessionHandoff` + `handoff_roundtrip_model`; the single-use nonce by
    /// `seamless_nonce_model`). Scope: the live process, visible rows, terminal modes/cursors,
    /// and output queued after reader park survive. Preexisting off-screen scrollback is
    /// carried only up to `seamless::MAX_HANDOFF_HISTORY_LINES` (256) per session, and
    /// DROPPED ENTIRELY for a session when the time ladder or the aggregate cell budget
    /// says so — the standing policy is "the failure mode is less scrollback, never the
    /// update did not apply". A tab configured for the default 100,000-line ring
    /// therefore keeps its processes, its visible screen and its queued output across an
    /// in-session update, but not its history. (This sentence used to say scrollback was
    /// "deliberately excluded", which stopped being true when the 256-line carry landed
    /// and understated what survives; 2026-08-19.) A cold relaunch is
    /// permitted only when no foreground terminal job would be destroyed. Every path that
    /// leaves this process alive returns an actionable failure so the updater reducer can
    /// re-arm the verified stage instead of remaining stuck in `Applying`.
    pub(crate) fn apply_staged_update_now(
        &mut self,
        safety_token: crate::app_native::NativeUpdateSafetyToken,
        mode: crate::native_updater_service::ApplyMode,
        apply_attempt: Option<crate::native_updater_service::ApplyAttemptTicket>,
        same_image: Option<SameImageHandoff>,
    ) -> Result<(), crate::UpdateHandoffStartError> {
        if self.update_handoff_in_flight() {
            return Err(crate::UpdateHandoffStartError::failed(
                "an update handoff is already in flight",
            ));
        }
        let build = crate::build_info::BUILD_NUMBER.parse::<u64>().unwrap_or(0);
        // QA SEAM: `ATERM_DEBUG_SEAMLESS_REEXEC=1` re-execs the SAME binary (no staged
        // build, no bundle swap) but exercises the FULL seamless handoff + adopt path, so
        // the shell-survives-an-update contract is testable end-to-end without a release.
        // The QA seam is derived here exactly as before; a caller may also present one
        // of the other same-image authorities, and a provenance repair must additionally
        // be EARNED — the App's own measured verdict, consumed on the way through, so a
        // caller cannot conjure the variant and a refusal downstream is never retried.
        let same_image = match same_image {
            // THE CALLER'S AUTHORITY IS DECIDED FIRST, and a repair is never rewritten
            // into the seam. It used to be: with `ATERM_DEBUG_SEAMLESS_REEXEC` armed the
            // seam branch won unconditionally, which silently dropped
            // `requires_out_of_band` (so a repair could be FORKED, inheriting the tag it
            // exists to shed) and skipped `take_eligible` (so the one-shot verdict was
            // never consumed and the trigger retried on every event-loop tick).
            Some(SameImageHandoff::ProvenanceRepair) => self
                .provenance_repair
                .take_eligible()
                .then_some(SameImageHandoff::ProvenanceRepair),
            // The seam supplies its own authority only where the caller offered none.
            Some(SameImageHandoff::DebugSeam) | None => {
                crate::app_update_screen::debug_seamless_reexec_armed()
                    .then_some(SameImageHandoff::DebugSeam)
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
        // Re-exec THIS binary; the swap to the staged build happens at the top of the
        // new `main`. `exec` never returns on success. (Rung 1b: serialize the session
        // + pass the PTY master fds here so the new process re-adopts them.)
        // Hand the OLD build number to the post-update process via `ATERM_UPDATED_FROM`
        // (inherited through `apply_staged_if_ready`'s own re-exec) so it shows the quiet
        // cursor-themed "leveled-up" notice on startup. Read + cleared at startup so it
        // never leaks into the user's shell children (`App::take_just_updated`).
        #[cfg(unix)]
        {
            // THE ROW IS RETIRED BY WHOEVER BROKE THE PROMISE. `start_unix_update_
            // handoff` puts "Installing aterm vX" (or "Installing update") on the
            // update bar before it builds the carry, and several of its later `Err`
            // exits (a missed 20 ms park deadline, masters closed under it, a
            // reservation failure) return WITHOUT producing a completion — so the
            // completion path cannot be the only place that clears it, or a refused
            // attempt leaves a row claiming an install that never started standing
            // to its `HANDOFF_STALE` cap. Binding the result at the ONE call site
            // covers every such exit and cannot rot as new ones are added.
            let started = self.start_unix_update_handoff(
                exe,
                build,
                safety_token,
                mode,
                apply_attempt,
                same_image,
            );
            if started.is_err() {
                self.retire_update_installing();
            }
            return started;
        }
        // Windows has no exec(2); the analog is spawn-the-new-then-exit. Dead in
        // practice today (the updater is macOS-only, so `staged` is always None
        // above), but kept correct for when a Windows update lane exists.
        #[cfg(windows)]
        {
            let live_ptys = self.pool.iter().count();
            let facts = crate::native_update_admission::AdmissionFacts {
                staged_verified: same_image.is_some() || apply_attempt.is_some(),
                seamless_capable: false,
                native_state_certified: safety_token.is_certified(),
                live_ptys,
                foreground_jobs: 0,
                // ConPTY currently has no exact foreground-pgrp proof. Any live
                // session is therefore both non-cold and foreground-unknown.
                unknown_foregrounds: live_ptys,
            };
            match crate::native_update_admission::classify(facts) {
                crate::native_update_admission::AdmissionDecision::Apply(
                    crate::native_update_admission::ApplyLane::Cold,
                ) if live_ptys == 0 => {}
                crate::native_update_admission::AdmissionDecision::Block(reason) => {
                    // Refusal, not failure — the unix arm's law.
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
            let operator_quiesce = match self.operator_control.as_ref() {
                Some(control) => Some(control.try_begin_update_quiesce().map_err(|error| {
                    crate::UpdateHandoffStartError::failed(format!(
                        "resident operator could not quiesce for replacement: {error}"
                    ))
                })?),
                None => None,
            };
            self.shutdown_title_summaries();
            let spawn = || {
                let mut cmd = std::process::Command::new(exe);
                // Forward argv minus the leading `--window` pins earlier boot
                // swaps prepended — every successor lane strips them so no
                // relaunch path can re-grow the argv (aterm-update's
                // reexec_forwarded_args doc has the accumulation story).
                cmd.args(aterm_update::reexec_forwarded_args(
                    std::env::args_os().skip(1),
                ))
                .env("ATERM_UPDATED_FROM", build.to_string());
                bind_expected_update_artifact(&mut cmd, apply_attempt.as_ref());
                // Same headless re-injection as the unix exec path above (and for
                // the same reason: argv passes through verbatim, so the env
                // channel — the flag's exact equivalent — is what survives an
                // `-e` boundary).
                if self.headless {
                    cmd.env("ATERM_HEADLESS", "1");
                }
                match cmd.spawn() {
                    Ok(_) => std::process::exit(0),
                    Err(error) => Err(error),
                }
            };
            let spawn_result = match operator_quiesce.as_ref() {
                Some(quiesce) => quiesce.with_commit_permit(spawn),
                None => Ok(spawn()),
            };
            match spawn_result {
                Ok(Ok(())) => {
                    return Err(crate::UpdateHandoffStartError::failed(
                        "replacement returned after a successful Windows spawn",
                    ));
                }
                Ok(Err(err)) => {
                    // A failed spawn leaves this process live. Restore a fresh exact
                    // authority/worker after the pre-replacement shutdown.
                    self.reconfigure_title_summaries();
                    aterm_log::warn!("update apply: re-spawn failed: {err}");
                    return Err(crate::UpdateHandoffStartError::failed(format!(
                        "replacement process could not start: {err}"
                    )));
                }
                Err(error) => {
                    self.reconfigure_title_summaries();
                    return Err(crate::UpdateHandoffStartError::failed(format!(
                        "resident operator revoked replacement: {error}"
                    )));
                }
            }
        }

        #[allow(unreachable_code)]
        Err(crate::UpdateHandoffStartError::failed(
            "process replacement returned without applying the update",
        ))
    }

    /// The control-socket pair a [`HandoffWorkerCleanup`] must republish after
    /// the overlap resolves: `Some((latest_link, sock_path))` only while the
    /// socket is actually bound and the plan mints a latest link. One
    /// projection for the worker cleanup and the emergency-reaper cleanup, so
    /// the two can never disagree about what gets republished.
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
        use std::os::unix::process::CommandExt as _;

        let live: Vec<(u64, i32, i32)> =
            self.pool.iter().map(|s| (s.id, s.master, s.pid)).collect();
        // THIS EPOCH GATES THE ENTRY, NOT THE FLIGHT — and the comment that
        // stood here said the opposite, four lines above the code that refutes
        // it. `automatic_activity_epoch` reaches exactly two places, both BEFORE
        // the readers park: the quiet-epoch admission just below, and the
        // pre-park TOCTOU re-check. The watcher that can revoke a parked handoff
        // mid-flight is `pending.activity_epoch`, armed UNCONDITIONALLY when the
        // attempt is stored, and read at Commit as the mandatory `exact_activity`
        // fact. So `AutomaticPastGrace` opts out of WAITING for a quiet moment;
        // it does not, and cannot, opt out of being revoked by one that arrives.
        // That is deliberate: the forced lane's job is to land on a machine that
        // is never quiet, not to outrank what the adoption proof must compare.
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
        // ONE name for one boolean. `ATERM_NO_OVERLAP_HANDOFF` used to sit two
        // lines below this, AND-ed into the same value: two spellings of the same
        // opt-out, only one of which appeared in any document, neither of which
        // said anything when honoured. It is gone; this is the opt-out.
        //
        // And it is LOUD. Suppressing the overlap leaves the update only the
        // cold lane — which the classifier below admits for an exact zero-PTY
        // state and nothing else, so with any terminal open the apply is REFUSED
        // (`LivePtysNeedSeamless`), no shell dies, and the build waits for the
        // next launch. A different route from the one the release was tested
        // on, so a binary that takes it says so, once, by name. A silent env var
        // that reroutes an update is how a shipped binary comes to behave
        // differently from the one that was proven. (This line used to say live
        // shells would NOT survive — a kill the classifier never permits; the
        // status bar repeated it until 2026-08-30.)
        //
        // ONE PREDICATE FOR BOTH READERS. Four conjuncts were AND-ed right here —
        // the opt-out, `$ATERM_CONTROL_SOCK`, `headless`, `proxy` — while the
        // status bar's posture folded only the first, so a `--control-sock` or
        // `--headless` process painted "applies in place within ~2 min" over an
        // apply this gate refused (2026-08-30). `seamless_handoff_unavailable`
        // is now the only place the remaining vetoes are read, and every reason is said in
        // the log the way the opt-out always was.
        let handoff_unavailable = self.seamless_handoff_unavailable();
        if let Some(why) = handoff_unavailable {
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
        let overlap_available = handoff_unavailable.is_none();
        let facts = crate::native_update_admission::AdmissionFacts {
            staged_verified: same_image.is_some() || apply_attempt.is_some(),
            seamless_capable: overlap_available && !live.is_empty(),
            native_state_certified: safety_token.is_certified(),
            live_ptys: live.len(),
            foreground_jobs,
            unknown_foregrounds,
        };
        match crate::native_update_admission::classify(facts) {
            crate::native_update_admission::AdmissionDecision::Block(reason) => {
                // A Block is a REFUSAL TO ATTEMPT, never a failed attempt — see
                // [`crate::UpdateHandoffStartError::Refused`]. Minting it as
                // `failed` counted every deterministic "the seamless lane is
                // unavailable here" as a hard apply failure (the field's
                // failing_applies=23) and erased the standing explanation.
                return Err(update_admission_refusal(reason, facts, handoff_unavailable));
            }
            crate::native_update_admission::AdmissionDecision::Apply(
                crate::native_update_admission::ApplyLane::Cold,
            ) => {
                // A REPAIR HAS NOTHING TO GAIN HERE. The cold lane `exec`s this image in
                // place, and the provenance tag survives `exec` into a clean image
                // (`atpkg::provenance`), so the successor would be tracked exactly as we
                // are — a full process replacement that changes the one thing it exists
                // to change: nothing. Unreachable in practice (a repair is gated on at
                // least one live PTY, and this arm is an exact zero-PTY state), but
                // written rather than argued: `classify` runs far from the lane decision
                // below, and an unreachable refusal costs a branch while an unwritten one
                // costs the user their session for no benefit.
                if same_image.is_some_and(SameImageHandoff::requires_out_of_band) {
                    return Err(crate::UpdateHandoffStartError::refused(
                        "a provenance repair has no session to hand over; the cold lane \
                         would exec this image in place and stay tracked",
                    ));
                }
                // Direct exec is authorized only for an exact zero-PTY state.
                debug_assert!(live.is_empty());
                let mut command = std::process::Command::new(exe);
                command
                    // Leading `--window` pins from earlier boot swaps are
                    // stripped (see the Windows spawn above) — the successor's
                    // own boot swap re-pins exactly one when it needs it.
                    .args(aterm_update::reexec_forwarded_args(
                        std::env::args_os().skip(1),
                    ))
                    .env("ATERM_UPDATED_FROM", build.to_string());
                // Same rule as the seamless lane below: an ACTIVATION binds no
                // expected artifact. The successor swaps nothing; binding the
                // activation digest would make its `apply_staged_if_ready` refuse a
                // newer `ready.toml` as "no longer matches" and write a spurious
                // apply refusal into the ledger.
                bind_expected_update_artifact(
                    &mut command,
                    apply_attempt
                        .as_ref()
                        .filter(|attempt| !attempt.is_installed_activation()),
                );
                // Headless survives the re-exec. The ENV channel, not the flag,
                // is right here even though `--headless` is the canonical
                // user-facing spelling: the successor inherits our argv (minus
                // the leading pins), and argv may carry an `-e`/`--command`
                // boundary that swallows every token after it — an appended flag
                // would become part of the child's command line, and a prepended
                // one would reorder a payload we must pass through unchanged.
                // `$ATERM_HEADLESS=1` is an exact equivalent of the flag and is
                // consumed once at the successor's boot, so this re-injection is
                // the whole handoff and it leaks nowhere further.
                if self.headless {
                    command.env("ATERM_HEADLESS", "1");
                }
                let operator_quiesce = match self.operator_control.as_ref() {
                    Some(control) => Some(control.try_begin_update_quiesce().map_err(|error| {
                        crate::UpdateHandoffStartError::failed(format!(
                            "resident operator could not quiesce for replacement: {error}"
                        ))
                    })?),
                    None => None,
                };
                self.shutdown_title_summaries();
                let error = match operator_quiesce.as_ref() {
                    Some(quiesce) => match quiesce.with_commit_permit(|| command.exec()) {
                        Ok(error) => error,
                        Err(error) => {
                            self.reconfigure_title_summaries();
                            return Err(crate::UpdateHandoffStartError::failed(format!(
                                "resident operator revoked replacement: {error}"
                            )));
                        }
                    },
                    None => command.exec(),
                };
                // `exec` returns only on failure. Recreate the worker/authority so
                // Smart Titles continue in the still-running old process.
                self.reconfigure_title_summaries();
                return Err(crate::UpdateHandoffStartError::failed(format!(
                    "process replacement failed: {error}"
                )));
            }
            crate::native_update_admission::AdmissionDecision::Apply(
                crate::native_update_admission::ApplyLane::Seamless,
            ) => {}
        }

        // PRE-VERIFY THE STAGED CANDIDATE (seamless seam 1) — codesign policy +
        // sealed build/commit rebinding, bound to the exact authorized artifact.
        // This authenticates a doomed candidate BEFORE the worker spawns the
        // child, so no doomed child is ever launched. It is strictly ADDITIVE
        // authority: the child re-runs the complete gate at swap time under the
        // apply lock (the TOCTOU defence is unchanged; `Ok` here is a latency +
        // warm-cache optimization, never a grant).
        //
        // OFF THE UI THREAD: the check is `codesign --deep` plus a bundle flock —
        // unbounded, disk-bound work that MUST NOT run on the GUI main thread
        // (it froze every frame before every handoff). It now runs as the
        // worker's FIRST action (see `run_handoff_worker`), so the main thread
        // parks readers and returns without ever blocking on codesign. A failing
        // candidate is caught there and rolled back via the ordinary
        // `PreparationFailed` completion (manual-only, like every other worker-
        // stage preparation failure). The same-binary debug re-exec has no
        // staged bundle, so it carries `verify_staged_candidate: false`.
        //
        // …AND BETTER STILL, OUT OF THE PARKED WINDOW ENTIRELY: when this exact
        // artifact already passed `spawn_staged_handoff_preverification` while
        // every reader was still live, the worker skips the repeat and the
        // parked interval shrinks by the whole `codesign --deep` cost. A cached
        // REFUSAL short-circuits the attempt right here, before anything parks.
        // Never an authorization either way — the child re-runs the complete
        // gate under the apply lock at swap time.
        let preverified = apply_attempt
            .as_ref()
            .and_then(|attempt| self.cached_handoff_preverification(attempt));
        // ONLY THE AUTOMATIC MODES HONOUR A CACHED REFUSAL (2026-09-14). An
        // explicit apply is the person's own request, made — in the case this
        // was written for — right after they put the signed bundle back: a
        // refusal cached up to ten minutes earlier must not answer for the
        // bundle that is there now. The worker re-verifies for them; a cached
        // PASS is still honoured by every mode, since it only shrinks the park.
        if preverified == Some(false) && mode.is_automatic() {
            // The CAUSE the verifier gave, not a generic verdict: an installed
            // bundle that fails the signing policy (a local build swapped into
            // /Applications) is the owner's to fix, and only the message can
            // tell them so.
            let reason = apply_attempt
                .as_ref()
                .and_then(|attempt| self.cached_handoff_preverification_reason(attempt))
                .unwrap_or_else(|| "the staged update failed verification".to_string());
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
        let Some(reconcile_worker) = self.native_update_reconcile_worker() else {
            return Err(crate::UpdateHandoffStartError::failed(
                "updater reconciliation worker is unavailable",
            ));
        };
        // Parent master flags are an invariant, not mutable handoff state. Child
        // inheritance is installed later by `pre_exec` on child copies only.
        for (_, master, _) in &live {
            if aterm_pty::set_cloexec(*master, true).is_err() {
                return Err(crate::UpdateHandoffStartError::failed(
                    "could not enforce CLOEXEC on every parent PTY master",
                ));
            }
        }

        // APPLY BEGINS ON THE STATUS BAR (2026-09-07) — sited BEFORE the carry
        // below is built, so the row count and words the successor inherits are
        // the ones the user is looking at. An update row already up changes its
        // WORDS — "Installing aterm vX — your shells are safe; the screen pauses
        // for a moment" — a repaint, never a re-grid. With no row up, an EXPLICIT
        // apply (the Version menu, Software Update, a clean quit) ADDS the row
        // here: its re-grid moves `ws.rows` before the carry reads it and before
        // `exact_layout` freezes what Commit compares, so the successor is sized
        // for the row it is told about. The AUTOMATIC lane never adds one: the
        // re-grid's SIGWINCH is PTY activity its own re-check (below) reads as a
        // revocation, so there the border SURGE — charging from this instant
        // until the successor takes over — is the whole explanation of the
        // frozen frame. Every refusal from here on puts the words (or the
        // absence of a row) back and ends the surge: the synchronous ones through
        // the caller's `retire_update_installing`, the asynchronous ones after
        // the park through `reduce_returned_handoff_completion`.
        self.begin_update_installing(
            build,
            matches!(
                mode,
                crate::native_updater_service::ApplyMode::Immediate
                    | crate::native_updater_service::ApplyMode::CleanQuit
            ),
        );

        // PROBED BEFORE THE PARK, and this is the reason: resolving the control
        // directory can create and `chmod` it, and every instruction between
        // `park_all_readers` and the capture deadline is spent inside a 20 ms
        // budget with the user's terminal frozen. The answer cannot change in
        // that window — it is a function of `$HOME` and this process's pid — so
        // taking it here costs the attempt nothing and keeps a filesystem
        // round-trip out of the parked interval. (The rest of the lane decision
        // — arithmetic plus one `fstat` per session — now sits below the worker
        // spawn for the same reason; this fact moves further up only because it
        // can touch the filesystem.)
        #[cfg(target_os = "macos")]
        let rendezvous_path_fits = crate::handoff_rendezvous::rendezvous_path_fits();
        // Same reasoning, same window: `app_bundle_root` ends in an `is_dir`.
        #[cfg(target_os = "macos")]
        let bundle = app_bundle_root(&exe);

        // Provision the worker BEFORE readers park. A resource-exhausted thread
        // creation therefore returns with the terminal completely untouched.
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

        // HOISTED OUT OF THE PARKED WINDOW. Nothing below this comment consumes
        // a byte from a master, and nothing above the park has stopped one yet:
        // the attempt's identity, its successor `Command`, and the LANE DECISION
        // with its proof identities are all pure functions of facts settled
        // BEFORE the park — the pool snapshot, the dup'd masters, the apply
        // ticket. What remains under the park is exactly what DEPENDS on the
        // parked state: the two digests over the captured screens and layout.
        //
        // WHAT THIS BUYS, precisely — an earlier version of this comment (and of
        // the CHANGELOG and the RFC) claimed the capture ladder "gets that
        // budget back", and that was FALSE: in the parent revision this block
        // ran AFTER the ladder had already finished, so the ladder was never
        // charged a microsecond of it and its `park_at + 20ms` window is
        // byte-identical either way. What the move actually does is (a) take
        // this work out of the frozen interval altogether, which is the freeze
        // the user feels, and (b) stop attempts dying at the deadline check that
        // follows the digests, which this block used to push past 20 ms.
        //
        // It also mints the attempt id before the park, so an attempt that dies
        // after this point burns one `u64` id. Nothing keys on id contiguity.
        //
        // These early returns therefore do NOT roll back the overlap — no
        // reader is parked, so there is nothing to re-attach. (What it would do
        // here is not nothing: `rollback_overlap` re-asserts a `set_cloexec` the
        // mandatory loop above already established, and schedules a reader
        // resume for readers that were never stopped. Both are harmless; neither
        // describes what happened at this point, which is why the call is gone
        // rather than kept "just in case".) A return also
        // drops `job_tx`, which is how the worker spawned just above learns to
        // exit: the same shape the activity re-check below already relies on.

        let attempt_id = self.next_update_handoff_id;
        let Some(next_attempt_id) = attempt_id.checked_add(1) else {
            return Err(crate::UpdateHandoffStartError::failed(
                "handoff identity space is exhausted",
            ));
        };
        self.next_update_handoff_id = next_attempt_id;
        let mut command = std::process::Command::new(exe);
        crate::control_socket_identity::bind_command(&mut command);
        command
            // Leading `--window` pins stripped, as on the cold/Windows lanes.
            .args(aterm_update::reexec_forwarded_args(
                std::env::args_os().skip(1),
            ))
            .env("ATERM_UPDATED_FROM", build.to_string());
        // An ACTIVATION binds no expected artifact: the successor has nothing to swap
        // (its `apply_staged_if_ready` finds no newer stage and returns NoUpdate) and
        // it simply IS the authorized build — the identity check below still names it.
        let installed_activation = apply_attempt
            .as_ref()
            .is_some_and(|attempt| attempt.is_installed_activation());
        bind_expected_update_artifact(
            &mut command,
            apply_attempt.as_ref().filter(|_| !installed_activation),
        );
        let target_build = apply_attempt
            .as_ref()
            .map_or(build, |attempt| attempt.target_build());
        let target_commit = apply_attempt.as_ref().map_or_else(
            || crate::build_info::GIT_COMMIT.to_string(),
            |attempt| attempt.target_commit().to_string(),
        );
        // THE LANE IS DECIDED HERE AND NOWHERE ELSE, and it has to be settled
        // before the pending attempt is recorded: it selects which term the
        // adoption proof hashes, and the main thread re-derives that proof from
        // the pending attempt at Commit time. A lane chosen later would leave
        // the two halves of one proof speaking different terms — the exact
        // failure that shows up as an unexplained `AdoptionMismatch`.
        //
        // Decided on the LIVE snapshot (2026-09-19, the late park): the launched
        // lane duplicates its masters and computes its device-term proof
        // identities AT THE PARK, which is now seconds or minutes away, over
        // whatever the pool holds then; the fork lane duplicates them just below,
        // before it parks, exactly as before. The `fstat` probe here answers the
        // same for a live master as for its later duplicate (same device), so
        // the lane and the proof term cannot disagree.
        #[cfg(target_os = "macos")]
        let lane = {
            let facts = HandoffLaneFacts {
                bundled: bundle.is_some(),
                // A build for this platform has one compiled in. The fact is
                // still a field rather than an assumption so the refusal it
                // guards is reachable from a test.
                launcher_available: true,
                socket_path_fits: rendezvous_path_fits,
                target_not_older: target_build >= build,
                sessions: live.len(),
                environment_is_a_merge: launch_environment(&command).is_some(),
            };
            match out_of_band_lane_refusal(facts) {
                None => {
                    if crate::handoff_rendezvous::proof_identities_in_device_terms(&live).is_some()
                    {
                        HandoffLane::OutOfBand
                    } else {
                        // A master that will not answer `fstat` cannot be given a
                        // device term, and a proof missing one term is a proof over
                        // a different session set. Forking is exact here rather than
                        // degraded: the fd-number term needs nothing from the kernel.
                        //
                        // A REPAIR NEVER FORKS — see the cold-lane refusal above for
                        // why a forked successor is a replacement that buys nothing.
                        // This return is pre-park (the readers are parked further
                        // down), so there is no overlap to roll back and dropping the
                        // worker's job channel is how it learns to exit.
                        if same_image.is_some_and(SameImageHandoff::requires_out_of_band) {
                            return Err(crate::UpdateHandoffStartError::refused(
                                "a provenance repair needs the out-of-band lane, and a \
                                 handed-off PTY would not answer fstat",
                            ));
                        }
                        aterm_log::warn!(
                            "update apply: forking instead of launching — a handed-off PTY would \
                             not answer fstat, so the out-of-band proof term cannot be computed"
                        );
                        HandoffLane::Fork
                    }
                }
                Some(reason) => {
                    // The same rule, through the same pure predicate, so the repair and
                    // the apply gate can never disagree about which lane is available.
                    if same_image.is_some_and(SameImageHandoff::requires_out_of_band) {
                        aterm_log::info!(
                            "provenance self-repair: not attempted — the out-of-band lane is \
                             unavailable here ({reason})"
                        );
                        return Err(crate::UpdateHandoffStartError::refused(
                            "a provenance repair needs the out-of-band lane",
                        ));
                    }
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

        // THE LAUNCHED LANE LEAVES HERE WITH EVERY READER LIVE (2026-09-19, the
        // late park). Its worker launches the successor, holds its rendezvous
        // claim, and asks this thread to park only once the successor has
        // dialled; the park, the capture and the transfer follow in
        // `park_and_transfer_to_prelaunched_successor`. Everything below this
        // line is the fork lane, which still parks before it spawns.
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
                installed_activation,
                bundle,
                cancel,
                cancelled,
                job_tx,
                reconcile_worker,
                reconcile_ticket,
            });
        }

        // Capture the session registry BEFORE parking, because this projection can
        // FAIL: a `WouldBlock` here must return with the terminal completely
        // untouched, which is only true while no reader has been stopped. It
        // performs no disk I/O.
        //
        // The restore manifest deliberately does NOT travel with it — see the
        // post-park capture below for why capturing it here revoked the handoff.
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
        use std::os::fd::{AsRawFd as _, FromRawFd as _};
        let mut owned_masters = Vec::with_capacity(live.len());
        let mut adoption = Vec::with_capacity(live.len());
        for (local_id, master, pid) in &live {
            // SAFETY: duplicates one live parent master as an independent CLOEXEC
            // descriptor. The original can later close/reuse its number without
            // changing the open-file-description inherited by the child.
            let duplicate = unsafe { libc::fcntl(*master, libc::F_DUPFD_CLOEXEC, 3) };
            if duplicate < 0 {
                return Err(crate::UpdateHandoffStartError::failed(
                    "could not reserve child-only PTY descriptors",
                ));
            }
            // SAFETY: F_DUPFD_CLOEXEC returned a fresh descriptor owned here.
            let owned = unsafe { std::os::fd::OwnedFd::from_raw_fd(duplicate) };
            adoption.push((*local_id, owned.as_raw_fd(), *pid));
            owned_masters.push(owned);
        }
        let fds = crate::session_store::HandoffFds {
            entries: adoption.clone(),
        };
        let window = self.windows.values().next().map(|state| {
            let position = state
                .os_window
                .as_ref()
                .and_then(|window| window.outer_position().ok());
            crate::session_store::WindowCarry {
                rows: state.rows,
                cols: state.cols,
                outer_x: position.map(|point| point.x),
                outer_y: position.map(|point| point.y),
                // The committed status-bar rows and their words, so the
                // successor reserves the same rows before it sizes its window
                // and paints carried content in them until Commit.
                status_bar_rows: self.status_bar_rows,
                bars: self.status_bars.carried(),
            }
        });
        // The fork lane's proof term is the descriptor number: `execve` copies
        // the table verbatim, so the number itself is what both sides compute.
        let proof_identities = adoption.clone();

        // Close the synchronous-preparation TOCTOU immediately before the first
        // reader stop. Activity defers automatic apply while every reader is
        // still live; manual explicit apply bypasses only this quiet policy.
        if automatic_activity_epoch.is_some_and(|epoch| {
            self.update_handoff_activity_epoch != epoch
                || !self.automatic_update_activity_quiet(std::time::Instant::now())
                || handoff_masters_have_activity(&live)
        }) {
            return Err(crate::UpdateHandoffStartError::activity(
                "input or PTY output arrived before automatic reader park",
            ));
        }
        // How much of the capture window must remain before a session is willing to
        // serialize scrollback as well as its visible screen. Half the freeze budget: the
        // visible screen is mandatory and cheap, history is optional and priced per
        // line, so once the window is half gone every remaining session drops to
        // visible-only rather than risking the deadline for a bonus. This is what
        // makes carrying history safe to enable by default — the failure mode is
        // "less scrollback", never "the update did not apply".
        // The budget this attempt gets: 20 ms on a first automatic attempt, wider after
        // a physical failure of these same bytes, widest for an explicit apply — see
        // `handoff_freeze_budget`. Everything below that used to spell "20 ms" now
        // spells the budget it actually had.
        // Keyed by the ATTEMPT'S TARGET (2026-09-14): every writer of the
        // physical-retry record stores the staged build it failed to reach,
        // while `build` here is the RUNNING one — so the filter never matched,
        // every automatic retry got the first attempt's 20 ms, and the wider
        // rungs (5f77ff084) were dead on the lane they were written for.
        let prior_physical_failures = self.prior_physical_failures_of(apply_attempt.as_ref());
        let freeze_budget = handoff_freeze_budget(mode, prior_physical_failures);
        let freeze_ms = freeze_budget.as_millis();
        let handoff_history_comfort = freeze_budget / 2;
        aterm_log::info!(
            "update apply: freeze budget {freeze_ms} ms ({mode:?}, {prior_physical_failures} prior \
             physical failure(s) of the target artifact; running build {build})"
        );
        // THE INSTANT THE TERMINAL STOPS ECHOING — the start of the freeze the
        // user experiences, and the zero point of the two numbers reported at
        // Commit. The capture deadline hangs off the same stamp.
        let park_at = std::time::Instant::now();
        let deadline = park_at + freeze_budget;
        if !self.park_all_readers(deadline) {
            self.rollback_overlap(None, &live);
            return Err(crate::UpdateHandoffStartError::failed(format!(
                "a PTY reader missed the {freeze_ms} ms handoff park deadline"
            )));
        }
        // SEAMLESS: the post-park re-check tolerates activity that used to
        // revoke here. A final burst consumed by a reader during the bounded
        // park is already inside the engine, so the checkpoints captured next
        // carry it; bytes that arrived after park wait in the kernel for the
        // child; hardware input queued during the ~20 ms park is dispatched
        // normally after this function returns and delivers to the still-open
        // masters. (The activity EPOCH provably cannot move here: every bump
        // happens on this thread, which is inside this function.) What must
        // still reject mid-flight is session DEATH — a HUP/ERR master means
        // the live-set identity the proof would commit to is already stale.
        // Classified as an activity deferral (matching the old disposition for
        // a death observed here): intent is retained and the next quiet-window
        // attempt sees the post-exit session set.
        // GATED ON THE LANE, NOT ON THE ACTIVITY EPOCH. Session DEATH is a
        // safety fact, not an idleness preference: `AutomaticPastGrace` opts out
        // of activity revocation above, but it must still refuse to commit an
        // adoption proof whose live set died underneath it.
        if mode.is_automatic() && handoff_masters_closed(&live) {
            self.rollback_overlap(None, &live);
            return Err(crate::UpdateHandoffStartError::activity(
                "a PTY session closed during automatic reader park",
            ));
        }
        let (screens, carries) = match self.capture_parked_screens(
            &live,
            deadline,
            freeze_ms,
            handoff_history_comfort,
        ) {
            Ok(captured) => captured,
            Err(reason) => {
                self.rollback_overlap(None, &live);
                return Err(crate::UpdateHandoffStartError::failed(reason));
            }
        };

        // POST-PARK, AND DELIBERATELY SO. Every reader is stopped and joined, and
        // the capture loop above just proved every session's `term` was lockable,
        // so `restore_session_meta`'s try_lock cannot silently degrade cwd/title
        // here.
        //
        // This used to be captured before the park, and that is a self-inflicted
        // revocation: `restore_session_meta` returns `(None, String::new())` on a
        // contended try_lock, and a producing session's reader thread holds its
        // term mutex for the whole of each `process()` slice. So a busy machine
        // captured a DEGRADED layout, `collect_handoff_commit_facts` later compared
        // it against the free post-park projection, `exact_layout` went false, and
        // the attempt was reported as "window/tab/pane topology changed during
        // async preparation" — a wrong cause, on a lane (`AutomaticPastGrace`,
        // `Immediate`) whose ENTRY is not gated on a quiet epoch — the guard
        // itself still applies to all of them — burning an
        // `ActivityRevoked` cycle every time. A shell writing OSC 7 during the park
        // did the same thing with no lock contention involved.
        //
        // After the capture loop rather than immediately after `park_all_readers`:
        // the scrollback-compression worker can still transiently hold a `term`
        // mutex once readers park, and if it did, the loop above has already
        // aborted the attempt cleanly ("a terminal engine was busy during handoff
        // capture") instead of producing a degraded layout here.
        let layout = self.capture_restore_manifest();

        let Some(layout_digest) = crate::seamless::layout_digest(&layout) else {
            self.rollback_overlap(None, &live);
            return Err(crate::UpdateHandoffStartError::failed(
                "handoff layout could not be committed canonically",
            ));
        };
        // The refusal REASON rides the message: this arm used to surface a dozen
        // distinct causes as one opaque sentence, so a stuck update was invisible
        // until someone read the source. It reaches the user through
        // `aterm ctl update status`'s `apply_failure=`.
        let screen_digest = match crate::seamless::screen_digest(&screens) {
            Ok(digest) => digest,
            Err(refusal) => {
                self.rollback_overlap(None, &live);
                return Err(crate::UpdateHandoffStartError::failed(format!(
                    "visible checkpoint set could not be committed canonically: {refusal}"
                )));
            }
        };
        if std::time::Instant::now() >= deadline {
            self.rollback_overlap(None, &live);
            return Err(crate::UpdateHandoffStartError::failed(
                "handoff proof capture exceeded the 20 ms deadline",
            ));
        }
        let activity_epoch = self.update_handoff_activity_epoch;
        let arbiter = crate::HandoffAttemptArbiter::new();
        self.pending_update_handoff = Some(crate::PendingUpdateHandoff {
            attempt_id,
            park_at,
            proof_ready_at: None,
            // Sampled AT THE PARK: whether the user was looking at aterm when the
            // screen it will hand over was the one on glass.
            activate_at_commit: self.any_os_window_focused(),
            nonce: None,
            live: live.clone(),
            // The PROOF identities, in whichever term this attempt's lane can
            // prove — see `HandoffWorkerJob::proof_identities`. Never `live`.
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
            cancel: cancel.clone(),
            arbiter: arbiter.clone(),
            teardown: if mode == crate::native_updater_service::ApplyMode::CleanQuit {
                crate::DeferredHandoffTeardown::CleanQuitReady
            } else {
                crate::DeferredHandoffTeardown::None
            },
            commit_drain_started: None,
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
            command,
            // The fork lane parks first, so its capture travels with the job.
            capture: Some(HandoffCapture {
                park_at,
                manifest,
                fds,
                screens,
                carries,
                window,
                layout,
                layout_digest,
                screen_digest,
                live: adoption,
                proof_identities,
                _owned_masters: owned_masters,
            }),
            #[cfg(target_os = "macos")]
            prelaunch: None,
            #[cfg(target_os = "macos")]
            lane,
            // Set by the WORKER immediately before the candidate is launched; the
            // main thread must not touch the staging directory here.
            trial_launches_before: 0,
            #[cfg(target_os = "macos")]
            bundle,
            // Derived from the authority HERE and nowhere else (see the field's doc).
            #[cfg(target_os = "macos")]
            require_out_of_band: same_image.is_some_and(SameImageHandoff::requires_out_of_band),
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

    /// THE CAPTURE LADDER, under the park: every session's visible screen (and
    /// as much scrollback as the remaining budget allows) plus the control
    /// carry's head, priced against the freeze budget and the aggregate cell and
    /// decode-authority ceilings. Shared by both lanes (2026-09-19): the fork
    /// lane runs it inline before its spawn, the launched lane at the park it
    /// takes once the successor has dialled. `Err` is the refusal's sentence;
    /// the caller rolls the readers back.
    #[cfg(unix)]
    #[allow(clippy::type_complexity)]
    fn capture_parked_screens(
        &mut self,
        live: &[(u64, i32, i32)],
        deadline: std::time::Instant,
        freeze_ms: u128,
        handoff_history_comfort: std::time::Duration,
    ) -> Result<
        (
            Vec<(u64, aterm_core::terminal::TerminalCheckpoint)>,
            Vec<crate::handoff_carry::CarrySource>,
        ),
        String,
    > {
        let mut screens = Vec::new();
        // The control carry's captures. Storage it cannot reserve carries
        // nothing (`carry_room` false) — never a failed capture.
        let mut carries = Vec::new();
        let carry_room = carries.try_reserve_exact(live.len()).is_ok();
        if screens.try_reserve_exact(live.len()).is_err() {
            return Err("visible checkpoint set could not reserve bounded storage".to_string());
        }
        let mut capture_failed = None;
        let mut capture_budget = 0_u64;
        let mut capture_cells = 0_u64;
        // Latched once the aggregate cell budget has refused a carried-history
        // checkpoint: every later session goes straight to visible-only rather than
        // paying a probe that this pool has already proven cannot pass. TWO causes
        // arm it — the budget refusing the carry, and the produced blob refusing the
        // wire's shape — which is why it is named for its EFFECT and not for either
        // one of them.
        let mut history_latched_off = false;
        // MANDATORY FIRST, OPTIONAL SECOND. Price the visible+alt grids of the WHOLE
        // POOL before charging any of it, in both budgets. Without this the budgets
        // are spent greedily in pool order and a later session finds nothing left
        // for the one thing it cannot degrade — see `optional_carry_fits`. Readers
        // are parked, so no geometry can move between this pass and the capture loop
        // below, and the two passes therefore price the same pool.
        //
        // An unreadable engine reserves EVERYTHING, i.e. nobody carries scrollback:
        // the capture loop reports that busy engine itself a moment later, and a
        // guess made here must never be the optimistic one.
        let mut cells_reserve = 0_u64;
        let mut bytes_reserve = 0_u64;
        for session in self.pool.iter() {
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
        for session in self.pool.iter() {
            if std::time::Instant::now() >= deadline {
                capture_failed = Some(format!(
                    "bounded visible-screen capture exceeded {freeze_ms} ms"
                ));
                break;
            }
            let Some(terminal) = try_lock_by(&session.term, deadline) else {
                capture_failed = Some(format!(
                    "a terminal engine stayed busy for the whole {freeze_ms} ms handoff \
                     capture window"
                ));
                break;
            };
            if !terminal.parser_is_ground() {
                capture_failed =
                    Some("a terminal parser was mid-sequence during handoff capture".to_string());
                break;
            }
            // SCROLLBACK IS BEST-EFFORT AND MUST NEVER COST THE HANDOFF.
            //
            // Carrying history is what stops an in-session update truncating every
            // tab to one screen, but it is strictly a bonus: the visible screen is
            // what adoption actually requires. So the depth is chosen per session
            // against the REMAINING time, and collapses to zero once the window is
            // more than half spent — and against the REMAINING budget, once the
            // pool's own mandatory reservation is taken off the top. Failing the
            // handoff to protect scrollback would trade the whole update for the
            // thing the update was carrying.
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let (rows, cols) = (terminal.rows(), terminal.cols());
            let own_cells = crate::seamless::mandatory_checkpoint_cells(rows, cols);
            let own_bytes =
                crate::seamless::checkpoint_capture_budget_bytes(rows, cols, 0).unwrap_or(u64::MAX);
            // Both reserves now cover only the sessions AFTER this one, so the two
            // checks below price this session's scrollback against what everybody
            // still waiting genuinely needs, and never against its own mandatory
            // cost twice.
            cells_reserve = cells_reserve.saturating_sub(own_cells);
            bytes_reserve = bytes_reserve.saturating_sub(own_bytes);
            let carry = crate::seamless::max_handoff_history_lines();
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
            let mut history = if remaining >= handoff_history_comfort
                && !history_latched_off
                && history_fits_cells
                && history_fits_bytes
            {
                carry
            } else {
                0
            };
            // Reject decoded allocation dimensions BEFORE the checkpoint serializes
            // anything. Conservatively reserve main+alt because querying the copied
            // checkpoint to discover alt presence is exactly the potentially
            // expensive work this admission must precede.
            let mut per_grid = crate::seamless::admit_checkpoint_dimensions(
                &mut capture_cells,
                rows,
                cols,
                history,
                true,
            );
            // DEGRADE ON BUDGET, exactly as the note above promises — this is the
            // arm that was missing, and its absence is why an update could refuse
            // to install forever.
            //
            // The ladder above reacts only to TIME. `admit_checkpoint_dimensions`
            // also refuses on SPACE: the process-wide
            // `MAX_HANDOFF_AGGREGATE_GRID_CELLS` is charged
            // `cols * (2*rows + history)` per session across every tab and pane of
            // every window, and carrying 256 history lines multiplies what a session
            // costs. So a handful of ordinary sessions exhausts it — and a refusal
            // there used to fall straight through to `capture_failed`, trading the
            // whole update for the scrollback it was carrying. That is the precise
            // failure this comment's own "MUST NEVER COST THE HANDOFF" forbids, and
            // it is deterministic: same sessions, same geometry, same constants
            // every attempt, so the automatic lane retried it indefinitely.
            //
            // A refusal costs nothing to recover from: the admission mutates the
            // aggregate ONLY on success ("an over-budget checkpoint cannot leave
            // partial authority"), so re-probing visible-only is exact, not
            // approximate. The latch stops every later session paying the same
            // doomed probe once the budget is known to be tight.
            if per_grid.is_none() && history != 0 {
                // SAY SO. Dropping a tab's history is invisible to the user until they
                // scroll up and find it gone, on a machine configured for 100,000
                // lines. The decision is deliberate and stays deliberate — but it is
                // recorded where every other handoff decision is (2026-08-19, raised
                // by an external reviewer of the product's public claim).
                aterm_log::info!(
                    "update apply: carrying no scrollback for this session — the \
                     handoff's aggregate cell budget cannot fit {history} history \
                     line(s) beside the visible {rows}x{cols} screen. Processes, the \
                     visible screen and queued output still survive the update."
                );
                history = 0;
                history_latched_off = true;
                per_grid = crate::seamless::admit_checkpoint_dimensions(
                    &mut capture_cells,
                    rows,
                    cols,
                    0,
                    true,
                );
            }
            // Only now is a refusal real: the VISIBLE screen is what adoption
            // requires, so a checkpoint that cannot be admitted even without history
            // is a genuine blocker rather than a carried bonus. Name the cap that
            // actually bound — the aggregate is in grid CELLS, and reporting every
            // refusal as the byte cap sent this exact investigation looking for a
            // 256 MiB allocation that was never involved.
            if per_grid.is_none() {
                capture_failed = Some(
                    "visible-screen capture exceeded the aggregate grid-cell budget".to_string(),
                );
                break;
            }
            capture_budget = per_grid
                .and_then(|bytes| bytes.checked_mul(2))
                .and_then(|bytes| capture_budget.checked_add(bytes))
                .unwrap_or(u64::MAX);
            // Same ladder, one budget over. This charge is not degradable here —
            // the history that would have been dropped was already refused above by
            // `history_fits`, which prices this ceiling too — so reaching it means
            // the pool's MANDATORY visible screens alone do not fit, which is a
            // genuine blocker rather than a carried bonus. Name the authority, not
            // "memory": nothing here allocates these bytes, they are the decode
            // ceiling the successor would be granted.
            if capture_budget > MAX_HANDOFF_CAPTURE_BUDGET_BYTES {
                capture_failed = Some(
                    "visible-screen capture exceeded the aggregate decode-authority budget"
                        .to_string(),
                );
                break;
            }
            let Some(mut checkpoint) = terminal.checkpoint_carry(history as usize) else {
                capture_failed =
                    Some("a terminal parser left Ground during handoff capture".to_string());
                break;
            };
            // DEGRADE ON SHAPE — the sibling of the budget arm above, and the arm
            // whose absence stranded a machine on an update it had already
            // downloaded and verified.
            //
            // The budget ladder prices TIME and SPACE. It cannot see the third way
            // a carry can be wrong: the produced blob not matching the shape the
            // wire allows. That is what `checkpoint_shape_refusal` asks, and until
            // now nobody asked it here — the checkpoint went straight into
            // `screens` and the first shape check ran in `screen_digest`, past the
            // point where the carry could still be lowered, with nothing left to do
            // but refuse the whole update. It refused every ~10 minutes for four
            // days.
            //
            // Lowering the carry is the cure for this whole CLASS, not one bug: the
            // inactive grid's blob is the only one whose record count the wire
            // cannot describe, and at `history == 0` it is exactly `rows` records by
            // construction, whatever the producer believed about the slot.
            //
            // Re-probing is exact rather than approximate, for the same reason the
            // budget arm gives: nothing here has been published yet. The aggregates
            // are function locals, `screen_digest`'s own aggregate is fresh per
            // call, and this session's cells were already admitted at the HIGHER
            // carry — so a re-carry at 0 leaves the pool charged for history it no
            // longer holds. That over-charge is deliberate: it is conservative, it
            // only ever makes a LATER session degrade sooner, and refunding a
            // transactional admission would be the kind of partial-authority
            // bookkeeping `admit_checkpoint_dimensions` exists to avoid.
            if let Some(refusal) =
                crate::seamless::checkpoint_shape_refusal(session.id, &checkpoint)
            {
                if history == 0 {
                    // Nothing left to lower. A visible-only capture that still will
                    // not take the wire's shape is a genuine blocker, and now it
                    // says which check refused instead of dying anonymously.
                    capture_failed = Some(format!(
                        "a visible checkpoint could not be shaped for the wire even without carried scrollback ({refusal})"
                    ));
                    break;
                }
                aterm_log::warn!(
                    "update apply: carrying no scrollback for the rest of this handoff — the carried shape was refused ({refusal}). Processes, the visible screen and queued output still survive the update."
                );
                // Only the LATCH carries this decision forward. Unlike the budget
                // arm — which lowers `history` before the carry that reads it —
                // this one runs after, and re-carries explicitly below; assigning
                // `history` here would be writing to a value nothing reads again.
                history_latched_off = true;
                let Some(visible_only) = terminal.checkpoint_carry(0) else {
                    capture_failed =
                        Some("a terminal parser left Ground during handoff capture".to_string());
                    break;
                };
                if let Some(refusal) =
                    crate::seamless::checkpoint_shape_refusal(session.id, &visible_only)
                {
                    capture_failed = Some(format!(
                        "a visible checkpoint could not be shaped for the wire even without carried scrollback ({refusal})"
                    ));
                    break;
                }
                checkpoint = visible_only;
            }
            screens.push((session.id, checkpoint));
            // THE CONTROL CARRY'S SHARE OF THE FREEZE (round 10), and all of it:
            // the archive's fence and counters, plus the differ's screen-sized
            // state — under this same lock, so it describes exactly the screen
            // this checkpoint carries. The rows and the ledger are exported on
            // the worker, behind the fence. It cannot fail the capture, and past
            // half the budget it leaves even the differ's state out — the worker
            // takes it then, while the fence's fingerprint of it holds (nothing
            // committed since), so the adopting engine still goes on exactly:
            // the carry is never worth the deadline.
            let differ = deadline.saturating_duration_since(std::time::Instant::now())
                >= handoff_history_comfort;
            // Reserved for every live session above, so this never allocates.
            if carry_room {
                carries.push(crate::handoff_carry::capture_head(
                    session.id,
                    &terminal,
                    &session.term,
                    &session.ctx.turns,
                    differ,
                ));
            }
            if std::time::Instant::now() >= deadline {
                capture_failed = Some(format!(
                    "bounded visible-screen capture exceeded {freeze_ms} ms"
                ));
                break;
            }
        }
        if capture_failed.is_none() && std::time::Instant::now() >= deadline {
            capture_failed = Some(format!(
                "bounded visible-screen capture exceeded {freeze_ms} ms"
            ));
        }
        if let Some(reason) = capture_failed {
            return Err(format!("{reason}; update handoff stayed in place"));
        }
        Ok((screens, carries))
    }

    /// The launched lane's first half (2026-09-19, the late park): record the
    /// attempt, hand the worker everything it needs to launch and hold the
    /// successor, and RETURN WITH EVERY READER LIVE. The park is taken later, on
    /// this thread, when the worker reports the successor's dial
    /// (`Wake::UpdateHandoffAwaitingPark`) and the park gate admits.
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
            installed_activation,
            bundle,
            cancel,
            cancelled,
            job_tx,
            reconcile_worker,
            reconcile_ticket,
        } = args;
        // MINTED HERE, BEFORE THE LAUNCH: the launch environment names the
        // manifest and layout by path, and the writer derives the same names
        // from this nonce after the park.
        let nonce = crate::seamless::mint_outgoing_nonce();
        let (stand_down_tx, stand_down_rx) = std::sync::mpsc::sync_channel(1);
        let (stand_down_ack_tx, stand_down_ack_rx) = std::sync::mpsc::sync_channel(1);
        let (transfer_tx, transfer_rx) = std::sync::mpsc::sync_channel(1);
        let arbiter = crate::HandoffAttemptArbiter::new();
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
            launched_at: std::time::Instant::now(),
            dialled: None,
            park_retry_at: None,
            park_misses: 0,
            stood_down: false,
            teardown: crate::DeferredHandoffTeardown::None,
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
            command,
            // The park has not happened: the capture arrives on the transfer
            // channel once it has.
            capture: None,
            prelaunch: Some(HandoffPrelaunchLane {
                nonce,
                transfer: transfer_rx,
                stand_down: stand_down_rx,
                stand_down_ack: stand_down_ack_rx,
                hold_bound: PRELAUNCH_HOLD_BOUND,
            }),
            lane: HandoffLane::OutOfBand,
            // Set by the WORKER immediately before the candidate is launched; the
            // main thread must not touch the staging directory here.
            trial_launches_before: 0,
            bundle,
            // Derived from the authority HERE and nowhere else (see the field's doc).
            require_out_of_band: same_image.is_some_and(SameImageHandoff::requires_out_of_band),
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
        Ok(())
    }

    /// `Wake::UpdateHandoffAwaitingPark`: the worker has the successor's dial in
    /// hand (or must fork), so the park may now be taken — at once for an
    /// explicit apply, at the next quiet moment for the automatic lane.
    #[cfg(unix)]
    pub(crate) fn on_update_handoff_awaiting_park(
        &mut self,
        attempt_id: u64,
        dialer_pid: Option<u32>,
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
            at: now,
        });
        match dialer_pid {
            Some(pid) => aterm_log::info!(
                "update apply: the successor (pid {pid}) dialled {} ms after the launch and \
                 holds its claim; parking when the gate admits",
                now.saturating_duration_since(prelaunch.launched_at)
                    .as_millis()
            ),
            None => aterm_log::info!(
                "update apply: the launched lane refused at runtime; the worker forks once \
                 the outgoing process has parked"
            ),
        }
        self.try_park_for_prelaunched_successor(now);
    }

    /// THE PARK GATE for a prelaunched attempt, re-run from the event loop every
    /// [`PRELAUNCH_PARK_RETRY`] while the automatic lane waits for a quiet
    /// moment, and bounded by [`PRELAUNCH_HOLD_MAX`]: an attempt held past it
    /// stands down as activity-revoked (no physical budget spent), and the
    /// automatic lane retries at a later quiet window.
    #[cfg(unix)]
    pub(crate) fn try_park_for_prelaunched_successor(&mut self, now: std::time::Instant) {
        let Some(prelaunch) = self.update_handoff_prelaunch.as_ref() else {
            return;
        };
        if prelaunch.dialled.is_none() {
            // The worker has not reported the dial yet: nothing has been
            // launched-and-held to park for.
            return;
        }
        if prelaunch.stood_down || prelaunch.park_retry_at.is_some_and(|at| now < at) {
            return;
        }
        match self.park_and_transfer_to_prelaunched_successor(now) {
            ParkAttempt::Parked => {
                if let Some(prelaunch) = self.update_handoff_prelaunch.as_mut() {
                    prelaunch.park_retry_at = None;
                }
            }
            ParkAttempt::NotYet(reason) => {
                if let Some(prelaunch) = self.update_handoff_prelaunch.as_mut() {
                    prelaunch.park_retry_at = Some(now + PRELAUNCH_PARK_RETRY);
                    aterm_log::debug!("update apply: not parking yet — {reason}");
                }
            }
            // A PARK THAT MISSED ITS BUDGET IS NOT A FAILED UPDATE (2026-09-19).
            // The readers are back (nothing was granted, so the rollback is
            // exact), the successor is still booted and holding, and the only
            // thing that went wrong is that a 20 ms window was not enough on a
            // busy machine. Re-park on the ladder's next rung rather than
            // throwing away a whole relaunch to buy the same rung later — and
            // once every rung has been tried, stand down as what it is: a busy
            // machine, retried on the activity spacing, never a latch
            // (`park_miss_disposition`).
            ParkAttempt::Missed(reason) => {
                // EVERY MISS IS PRE-RECORD. The parked attempt is stored at the
                // very end of the park half, after the last thing that can miss,
                // so a miss leaves only the prelaunch record and resumed readers
                // — which is exactly what makes re-parking under the same nonce
                // sound (no artifact has been written, nothing has been granted).
                debug_assert!(
                    self.pending_update_handoff.is_none(),
                    "a park that missed its budget must leave no parked attempt behind"
                );
                let misses = self
                    .update_handoff_prelaunch
                    .as_ref()
                    .map_or(u8::MAX, |prelaunch| prelaunch.park_misses);
                match park_miss_disposition(misses, reason) {
                    ParkMissDisposition::StandDown(stand_down) => {
                        self.stand_down_prelaunched_successor(stand_down);
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
            ParkAttempt::Failed(stand_down) => self.stand_down_prelaunched_successor(stand_down),
        }
    }

    /// Tell the worker to stand the held successor down with a typed reason.
    /// Idempotent per attempt; the completion the worker then sends is what
    /// clears the attempt record.
    #[cfg(unix)]
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
            // The worker is past its hold (or gone): the cancel poke reaches every
            // later wait it might be in.
            let _ = prelaunch.cancel.try_send(());
        }
    }

    /// `Wake::UpdateHandoffStandingDown`: the worker is about to revoke a held,
    /// UNGRANTED successor and is waiting for this answer before it does.
    ///
    /// Two things happen here and they happen on the main thread, which is where
    /// the park gate also runs — so a park can neither be in progress nor start
    /// afterwards:
    ///
    ///  1. THE GATE CLOSES FOR GOOD. Nothing may park onto a successor that is
    ///     being killed; before this existed a 20 ms gate tick could freeze the
    ///     terminal onto a candidate the worker had already given up on.
    ///  2. IF A PARK ALREADY LANDED, its readers resume NOW — the successor
    ///     holds no descriptor (the one `sendmsg` is the grant and it has not
    ///     happened), so this is exact, and it is the model's `UnparkForRepark`.
    ///     Without it the user paid the stand-down's grace, SIGKILL and reap as a
    ///     frozen terminal.
    ///
    /// The attempt RECORD outlives both, until the completion: it is what stops a
    /// retry launching beside the candidate being stood down. The parked record's
    /// deferred teardown and activity flag move onto it so the completion still
    /// replays them.
    #[cfg(unix)]
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
        // ANSWERED LAST, after the gate is shut and the readers are back: the
        // worker revokes on this, so everything it must not race has to be done
        // before it is sent. A full or disconnected channel means the worker
        // stopped waiting, which its own bounded wait already reports.
        if let Some(ack) = ack {
            let _ = ack.try_send(());
        }
    }

    /// The launched lane's second half: the park itself, at the dial or at the
    /// quiet moment after it. Everything that depends on the readers being
    /// stopped happens here — the registry projection, the descriptor
    /// duplicates, the window carry, the device-term proof identities, the
    /// capture ladder, the layout and both digests — and the product crosses to
    /// the worker holding the successor's claim as one [`HandoffTransferJob`].
    ///
    /// `NotYet` is "try again in a moment" (the gate, a busy registry);
    /// `Failed` stands the successor down with the outcome the completion should
    /// carry. Every `Failed` after `park_all_readers` has already rolled the
    /// readers back: the successor holds nothing, so the readers never wait for
    /// its stand-down.
    #[cfg(unix)]
    fn park_and_transfer_to_prelaunched_successor(
        &mut self,
        now: std::time::Instant,
    ) -> ParkAttempt {
        use std::os::fd::{AsRawFd as _, FromRawFd as _};

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
        let dialled_at = dialled.map_or(now, |dialled| dialled.at);
        let park_misses = prelaunch.park_misses;
        let build = crate::build_info::BUILD_NUMBER.parse::<u64>().unwrap_or(0);

        let live: Vec<(u64, i32, i32)> =
            self.pool.iter().map(|s| (s.id, s.master, s.pid)).collect();
        if live.is_empty() {
            return ParkAttempt::Failed(HandoffStandDown {
                outcome: crate::UpdateHandoffOutcome::ActivityRevoked,
                detail: "every terminal session closed while the successor booted".to_string(),
            });
        }
        // THE GATE — the only decision that costs the user anything, taken here
        // and nowhere else (`prelaunch_park_admitted` says what each mode owes).
        // Session DEATH is re-checked under the park below for every automatic
        // mode; this is about idleness, which is a different question.
        let gate_facts = ParkGateFacts {
            mode,
            phase: self.automatic_phase_for(mode, now),
            activity: self.automatic_activity_facts(mode, now),
            masters_quiet: !handoff_masters_have_activity(&live),
            held_for: now.saturating_duration_since(dialled_at),
        };
        match prelaunch_park_admitted(gate_facts, prelaunch_hold_cap(mode)) {
            ParkGate::Park => {}
            ParkGate::Wait(reason) => return ParkAttempt::NotYet(reason),
            ParkGate::StandDown(reason) => {
                return ParkAttempt::Failed(HandoffStandDown {
                    outcome: crate::UpdateHandoffOutcome::ActivityRevoked,
                    detail: format!(
                        "{reason} ({} s after the successor dialled)",
                        gate_facts.held_for.as_secs()
                    ),
                });
            }
        }
        #[cfg(target_os = "macos")]
        if live.len() > crate::handoff_rendezvous::MAX_RENDEZVOUS_SESSIONS {
            return ParkAttempt::Failed(HandoffStandDown {
                outcome: crate::UpdateHandoffOutcome::PreparationFailed,
                detail: "more sessions opened than one descriptor message carries".to_string(),
            });
        }
        // Capture the session registry BEFORE parking, because this projection can
        // FAIL: a `WouldBlock` here must return with the terminal completely
        // untouched, which is only true while no reader has been stopped. It
        // performs no disk I/O.
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
        let mut owned_masters = Vec::with_capacity(live.len());
        let mut adoption = Vec::with_capacity(live.len());
        for (local_id, master, pid) in &live {
            // SAFETY: duplicates one live parent master as an independent CLOEXEC
            // descriptor. The original can later close/reuse its number without
            // changing the open-file-description inherited by the child.
            let duplicate = unsafe { libc::fcntl(*master, libc::F_DUPFD_CLOEXEC, 3) };
            if duplicate < 0 {
                return ParkAttempt::Failed(HandoffStandDown {
                    outcome: crate::UpdateHandoffOutcome::PreparationFailed,
                    detail: "could not reserve child-only PTY descriptors".to_string(),
                });
            }
            // SAFETY: F_DUPFD_CLOEXEC returned a fresh descriptor owned here.
            let owned = unsafe { std::os::fd::OwnedFd::from_raw_fd(duplicate) };
            adoption.push((*local_id, owned.as_raw_fd(), *pid));
            owned_masters.push(owned);
        }
        let fds = crate::session_store::HandoffFds {
            entries: adoption.clone(),
        };
        let window = self.windows.values().next().map(|state| {
            let position = state
                .os_window
                .as_ref()
                .and_then(|window| window.outer_position().ok());
            crate::session_store::WindowCarry {
                rows: state.rows,
                cols: state.cols,
                outer_x: position.map(|point| point.x),
                outer_y: position.map(|point| point.y),
                // The committed status-bar rows and their words, so the
                // successor reserves the same rows before it sizes its window
                // and paints carried content in them until Commit.
                status_bar_rows: self.status_bar_rows,
                bars: self.status_bars.carried(),
            }
        });
        // THE PROOF TERM IS THE PTY DEVICE on this lane (the descriptors travel
        // by `SCM_RIGHTS`, which installs the receiver's own numbers), and a
        // launched attempt that must fork at runtime keeps that term — the
        // worker says so in the successor's environment either way.
        #[cfg(target_os = "macos")]
        let Some(proof_identities) =
            crate::handoff_rendezvous::proof_identities_in_device_terms(&adoption)
        else {
            return ParkAttempt::Failed(HandoffStandDown {
                outcome: crate::UpdateHandoffOutcome::PreparationFailed,
                detail: "a handed-off PTY would not answer fstat, so the out-of-band proof \
                         term cannot be computed"
                    .to_string(),
            });
        };
        #[cfg(not(target_os = "macos"))]
        let proof_identities = adoption.clone();
        if self.update_handoff_activity_epoch == u64::MAX {
            return ParkAttempt::Failed(HandoffStandDown {
                outcome: crate::UpdateHandoffOutcome::PreparationFailed,
                detail: "handoff activity identity space is exhausted".to_string(),
            });
        }
        // THE LADDER, PLUS THIS ATTEMPT'S OWN MISSES. A park that missed its
        // budget re-parks on the next rung (20 -> 80 -> 250 ms) without giving
        // the booted successor back: the rung a physical failure would have
        // bought the NEXT attempt, bought here for 500 ms instead of a relaunch.
        let prior_physical_failures = self
            .prior_physical_failures_of(apply_attempt.as_ref())
            .saturating_add(park_misses);
        let freeze_budget = handoff_freeze_budget(mode, prior_physical_failures);
        let freeze_ms = freeze_budget.as_millis();
        let handoff_history_comfort = freeze_budget / 2;
        aterm_log::info!(
            "update apply: freeze budget {freeze_ms} ms ({mode:?}, {prior_physical_failures} prior \
             physical failure(s) of the target artifact incl. {park_misses} park miss(es) of this \
             attempt; running build {build}) — parking {} ms after the successor's dial",
            now.saturating_duration_since(dialled_at).as_millis()
        );
        // THE INSTANT THE TERMINAL STOPS ECHOING — the start of the freeze the
        // user experiences, and the zero point of the numbers reported at Commit.
        let park_at = std::time::Instant::now();
        let deadline = park_at + freeze_budget;
        if !self.park_all_readers(deadline) {
            self.rollback_overlap(None, &live);
            return ParkAttempt::Missed(format!(
                "a PTY reader missed the {freeze_ms} ms handoff park deadline"
            ));
        }
        // Session DEATH is a safety fact, not an idleness preference: every
        // automatic mode refuses to commit an adoption proof whose live set died
        // underneath it.
        if mode.is_automatic() && handoff_masters_closed(&live) {
            self.rollback_overlap(None, &live);
            return ParkAttempt::Failed(HandoffStandDown {
                outcome: crate::UpdateHandoffOutcome::ActivityRevoked,
                detail: "a PTY session closed during automatic reader park".to_string(),
            });
        }
        let (screens, carries) = match self.capture_parked_screens(
            &live,
            deadline,
            freeze_ms,
            handoff_history_comfort,
        ) {
            Ok(captured) => captured,
            Err(reason) => {
                self.rollback_overlap(None, &live);
                return ParkAttempt::Missed(reason);
            }
        };
        // POST-PARK, AND DELIBERATELY SO — see the fork lane for why a pre-park
        // layout capture revoked healthy attempts.
        let layout = self.capture_restore_manifest();
        let Some(layout_digest) = crate::seamless::layout_digest(&layout) else {
            self.rollback_overlap(None, &live);
            return ParkAttempt::Missed(
                "handoff layout could not be committed canonically".to_string(),
            );
        };
        let screen_digest = match crate::seamless::screen_digest(&screens) {
            Ok(digest) => digest,
            Err(refusal) => {
                self.rollback_overlap(None, &live);
                return ParkAttempt::Missed(format!(
                    "visible checkpoint set could not be committed canonically: {refusal}"
                ));
            }
        };
        if std::time::Instant::now() >= deadline {
            self.rollback_overlap(None, &live);
            return ParkAttempt::Missed(format!(
                "handoff proof capture exceeded the {freeze_ms} ms deadline"
            ));
        }
        let activity_epoch = self.update_handoff_activity_epoch;
        let proof_deadline = handoff_proof_deadline(mode, prior_physical_failures, park_at);
        self.pending_update_handoff = Some(crate::PendingUpdateHandoff {
            attempt_id,
            park_at,
            proof_ready_at: None,
            // Sampled AT THE PARK: whether the user was looking at aterm when the
            // screen it will hand over was the one on glass.
            activate_at_commit: self.any_os_window_focused(),
            nonce: Some(nonce),
            live: live.clone(),
            // The PROOF identities, in the device term — see
            // `HandoffCapture::proof_identities`. Never `live`.
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
            cancel,
            arbiter,
            teardown: if mode == crate::native_updater_service::ApplyMode::CleanQuit {
                crate::DeferredHandoffTeardown::CleanQuitReady
            } else {
                crate::DeferredHandoffTeardown::None
            },
            commit_drain_started: None,
            revoked_by_activity: false,
        });
        let transfer = HandoffTransferJob {
            capture: HandoffCapture {
                park_at,
                manifest,
                fds,
                screens,
                carries,
                window,
                layout,
                layout_digest,
                screen_digest,
                live: adoption,
                proof_identities,
                _owned_masters: owned_masters,
            },
            proof_deadline,
        };
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
            "update apply: parked and captured in {} ms; the capture is on its way to the \
             held successor",
            park_at.elapsed().as_millis()
        );
        ParkAttempt::Parked
    }

    /// The pre-Commit input-drain gate. Returns `None` when the completion was
    /// re-posted for another drain spin (the caller must return), otherwise
    /// the completion handed back plus the `input_dispatch_fenced` /
    /// `egress_settled` admission facts.
    ///
    /// DRAIN, DON'T DIE (seamless: OS-accepted input): hardware events
    /// accepted immediately before this callback may not yet have been
    /// dispatched through winit and would die with `_exit`. Defer Commit
    /// across a mandatory dispatch FENCE — measured in loop iterations that
    /// really dispatched events, not in wall clock alone. Re-posting this
    /// exact completion gives the run loop time to dispatch those events —
    /// their bytes flow through the tolerated input path into the
    /// still-open PTY masters — and the re-post re-runs this admission
    /// against a drained queue. Bounded by the drain DEADLINE below (3 s,
    /// which at the 2 ms yield is reached around spin ~1500 — the 4000
    /// spin cap is the backstop behind it, not the operative bound; this
    /// comment used to name the cap and claim sustained typing exhausts
    /// it into an activity revocation, which was unreachable arithmetic)
    /// and absolutely by the worker's 15 s decision deadline. A failed re-post means the event
    /// loop is closing; dropping the completion drops the reject sender,
    /// which the worker observes as Disconnected and rejects/reaps.
    ///
    /// …AND DON'T LEAVE IT IN A RUST QUEUE EITHER: a tolerated keystroke,
    /// once dispatched, does not go straight to the master — under a live
    /// paste it rides the per-session paste-order FIFO, and against a
    /// wedged tty it lands in the sink's spill buffer. Both are
    /// PROCESS-LOCAL: they die with `_exit` exactly like the AppKit queue.
    /// So Commit also waits until every handed-off session's egress has
    /// reached the kernel (`handoff_egress_settled`) — the drainer/writer
    /// threads flush it to the still-open master between re-posts. Same
    /// bounded, lossless defer; the fact fences a budget-exhausted
    /// Commit so an unflushable spill fails closed (rollback) instead of
    /// `_exit`ing over undelivered bytes.
    ///
    /// The hard admission facts are the dispatch fence and settled
    /// egress (see `HandoffCommitFacts::input_dispatch_fenced` /
    /// `egress_settled`). A quiet window used to be PREFERRED on top of
    /// them — waited for up to 400 ms, then abandoned.
    ///
    /// THAT WAIT IS GONE (2026-08-27), and deleting it weakened no
    /// admission fact: `quiet_window_settled` was read in the respin
    /// condition and nowhere else, and `handoff_commit_admitted`'s
    /// eleven-way conjunction never mentioned it. What it cost was real.
    /// `user_input_recent` reads the MACHINE-WIDE kernel HID idle clock,
    /// so any mouse twitch anywhere on the desktop held the gate to its
    /// full 400 ms ceiling; and after the reveal the successor is the key
    /// window, so the keystrokes it was DEFERRING held the PARENT's gate
    /// open on a queue the parent could no longer receive from — typing
    /// lengthened the very freeze that was swallowing the typing. The
    /// gate now settles at its ~30-45 ms floor: the mandatory dispatch
    /// fence, and egress that has actually reached the kernel.
    #[cfg(unix)]
    fn handoff_drain_gate(
        &mut self,
        completion: crate::UpdateHandoffCompletion,
    ) -> Option<(crate::UpdateHandoffCompletion, bool, bool)> {
        const HANDOFF_INPUT_DRAIN_SPIN_CAP: u32 = 4_000;
        /// MANDATORY minimum event-loop time between ProofReady and Commit.
        const HANDOFF_INPUT_DISPATCH_FENCE: std::time::Duration =
            std::time::Duration::from_millis(30);
        /// Per-respin yield, so the fence is measured in loop iterations
        /// that really dispatched events instead of a busy spin.
        const HANDOFF_INPUT_DRAIN_YIELD: std::time::Duration = std::time::Duration::from_millis(2);
        /// Absolute wall-clock backstop. An egress queue that never settles
        /// must fail CLOSED (rollback), never `_exit` over undelivered bytes.
        const HANDOFF_INPUT_DRAIN_DEADLINE: std::time::Duration =
            std::time::Duration::from_millis(3_000);
        // A vanished pending attempt cannot be drained toward Commit; fall
        // straight through to the rejection path rather than respinning on
        // a clock that would restart every iteration.
        let drained_for = {
            let now = std::time::Instant::now();
            self.pending_update_handoff.as_mut().map(|pending| {
                // Same edge, so the same stamp: the first ProofReady observation
                // is when the successor's proof landed, which closes the park→proof
                // half of the freeze and opens proof→commit.
                pending.proof_ready_at.get_or_insert(now);
                now.saturating_duration_since(*pending.commit_drain_started.get_or_insert(now))
            })
        };
        let drained_for = drained_for.unwrap_or(HANDOFF_INPUT_DRAIN_DEADLINE);
        // The fence needs BOTH a completed re-post (so the loop really
        // iterated) and the elapsed floor.
        let input_dispatch_fenced =
            completion.input_drain_spins >= 1 && drained_for >= HANDOFF_INPUT_DISPATCH_FENCE;
        let egress_settled = self
            .pending_update_handoff
            .as_ref()
            .map(|pending| pending.live.clone())
            .is_none_or(|live| handoff_egress_settled(&self.pool, &live));
        if (!input_dispatch_fenced || !egress_settled)
            && completion.input_drain_spins < HANDOFF_INPUT_DRAIN_SPIN_CAP
            && drained_for < HANDOFF_INPUT_DRAIN_DEADLINE
            && let Some(proxy) = self.proxy.clone()
        {
            // Yield the main thread so the run loop actually dispatches the
            // queued NSEvents before our re-post comes back around. Cheap
            // and bounded: the frozen frame is already parked.
            std::thread::sleep(HANDOFF_INPUT_DRAIN_YIELD);
            let respin = crate::UpdateHandoffCompletion {
                input_drain_spins: completion.input_drain_spins.saturating_add(1),
                ..completion
            };
            let _ = proxy.send_event(Wake::UpdateHandoffFinished(respin));
            return None;
        }
        Some((completion, input_dispatch_fenced, egress_settled))
    }

    /// Snapshot the pending attempt and collect every Commit admission fact in
    /// one place (see [`HandoffCommitFacts`]). `None` when the pending attempt
    /// vanished mid-drain — the caller rejects and returns. Also hands back
    /// the fresh native-safety evidence (its `Err` carries the human-readable
    /// reasons), the exact adoption proof, and the attempt arbiter.
    #[cfg(unix)]
    #[allow(clippy::type_complexity)]
    fn collect_handoff_commit_facts(
        &mut self,
        nonce: Option<&str>,
        input_dispatch_fenced: bool,
        egress_settled: bool,
        commit_channel: bool,
    ) -> Option<(
        HandoffCommitFacts,
        Result<crate::app_native::NativeUpdateSafetyToken, Vec<String>>,
        Option<crate::seamless::AdoptionProof>,
        crate::HandoffAttemptArbiter,
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
        let arbiter = pending.arbiter.clone();
        let teardown_allows_commit = matches!(
            pending.teardown,
            crate::DeferredHandoffTeardown::None | crate::DeferredHandoffTeardown::CleanQuitReady
        );
        let exact_activity = self.update_handoff_activity_epoch == pending_activity_epoch;
        let mut current_live: Vec<(u64, i32, i32)> =
            self.pool.iter().map(|s| (s.id, s.master, s.pid)).collect();
        current_live.sort_unstable();
        let mut expected_live = pending_live.clone();
        expected_live.sort_unstable();
        let exact_sessions = current_live == expected_live;
        // TOPOLOGY, not the raw capture — see `commit_layout_topology` for the
        // two fields it normalizes away and why each of them was rejecting
        // perfectly healthy attempts. The raw inequality is still worth one log
        // line: it is the only place a window drag or a contended session lock
        // becomes visible in the field, and the whole point of this change is
        // that neither may ever again be REPORTED as a topology change.
        let live_layout = self.capture_restore_manifest();
        let exact_layout =
            commit_layout_topology(&live_layout) == commit_layout_topology(&pending_layout);
        if exact_layout && live_layout != pending_layout {
            aterm_log::info!(
                "update apply: the Commit-time capture differs from the committed snapshot \
                 only in window position or degradable session metadata — topology is \
                 unchanged, so Commit stays admitted"
            );
        }
        let parent_still_parked = self
            .pool
            .iter()
            .all(|session| session.reader_join.is_none());
        let native_safety = self.revalidate_native_update_safety();
        // Death-only peek: queued output on a master is tolerated (it
        // waits in the kernel for the child); HUP/ERR means the adopted
        // live-set identity is already stale and must reject.
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
        });
        let facts = HandoffCommitFacts {
            exact_sessions,
            exact_layout,
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
        Some((facts, native_safety, proof, arbiter))
    }

    /// Main-thread completion of the asynchronous overlap proof. `ProofReady` is
    /// deliberately not sufficient by itself: native state and the exact live PTY
    /// identity set may have changed while the child booted. Only a fresh proof plus
    /// unchanged sessions authorizes the destructor-free parent exit.
    #[cfg(unix)]
    pub(crate) fn finish_update_handoff(
        &mut self,
        el: &ActiveEventLoop,
        completion: crate::UpdateHandoffCompletion,
    ) {
        let matches_pending = self
            .pending_update_handoff
            .as_ref()
            .is_some_and(|pending| pending.attempt_id == completion.attempt_id)
            || self
                .update_handoff_prelaunch
                .as_ref()
                .is_some_and(|prelaunch| prelaunch.attempt_id == completion.attempt_id);
        if !matches_pending {
            // A stale proof can never authorize Commit. Ask its still-owning worker
            // to kill/reap readerless child; never wait on the event loop.
            let attempt_id = completion.attempt_id;
            let _ = deliver_handoff_rejection(completion.reject);
            aterm_log::warn!("update apply: ignored stale handoff completion {attempt_id}");
            return;
        }
        if completion.outcome == crate::UpdateHandoffOutcome::ProofReady {
            {
                let Some(pending) = self.pending_update_handoff.as_mut() else {
                    if let Some(reject) = completion.reject {
                        let _ = reject.try_send(());
                    }
                    return;
                };
                pending.nonce = completion.nonce.clone();
                pending.child_pid = completion.child_pid;
            }
            let Some((completion, input_dispatch_fenced, egress_settled)) =
                self.handoff_drain_gate(completion)
            else {
                // Re-posted for another drain spin (or the event loop is
                // closing and the drop rejects the attempt).
                return;
            };
            // Unused fields keep their underscore-prefixed bindings so their
            // drops still run at the end of this call, exactly as when the
            // whole completion was destructured on entry.
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
            // Quiesce the resident operator only for the final admission seam.
            // A rejection drops this reversible token; a successful Commit
            // `_exit`s while its gate is still held by `with_commit_permit`.
            let (operator_quiesce, mut operator_quiesce_error) =
                match self.operator_control.as_ref() {
                    Some(control) => match control.try_begin_update_quiesce() {
                        Ok(quiesce) => (Some(quiesce), None),
                        Err(error) => (None, Some(error)),
                    },
                    None => (None, None),
                };
            let Some((facts, native_safety, proof, arbiter)) = self.collect_handoff_commit_facts(
                nonce.as_deref(),
                input_dispatch_fenced,
                egress_settled,
                commit_fd.is_some(),
            ) else {
                // The pending attempt vanished mid-drain: nothing can be
                // committed; ask the worker to reject and reap.
                if let Some(reject) = reject {
                    let _ = reject.try_send(());
                }
                return;
            };
            let commit_admitted =
                handoff_commit_admitted(facts) && operator_quiesce_error.is_none();
            let mut commit_lost_arbiter = false;
            let mut commit_write_failed = false;
            if commit_admitted && let (Some(commit_fd), Some(proof)) = (commit_fd.as_ref(), proof) {
                if arbiter.try_begin_commit() {
                    // THE FREEZE, IN TWO NUMBERS. Everything from the park to here
                    // is a terminal that echoed nothing; the split says which half
                    // to attack. park→proof is the successor's bundle swap, second
                    // `execve` and cold GUI boot; proof→commit is this process's
                    // dispatch fence plus egress settle. Emitted once per apply, on
                    // the path that ends in `_exit`, so it is the last thing this
                    // process says about a freeze the user just sat through.
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
                    // THE SUCCESSOR TAKES THE FRONT AT COMMIT, and only when an
                    // aterm window had focus at the park (2026-09-19): the launch
                    // never activated, so this is the one instant the user's
                    // focus moves — onto the window that is about to be theirs,
                    // never onto a candidate. With no aterm window focused the
                    // user is in another app and keeps it.
                    if self
                        .pending_update_handoff
                        .as_ref()
                        .is_some_and(|pending| pending.activate_at_commit)
                        && let Some(pid) = child_pid.and_then(|pid| i32::try_from(pid).ok())
                    {
                        let accepted = crate::app_launch_successor::activate_running(pid);
                        aterm_log::info!(
                            "update apply: activating the successor at Commit (an aterm window \
                             had focus at the park): {}",
                            if accepted {
                                "accepted"
                            } else {
                                "refused by AppKit — the successor stays behind"
                            }
                        );
                    }
                    // Success cannot return: `commit_and_exit` performs the one
                    // atomic <=PIPE_BUF write and `_exit(0)` in the same typed
                    // operation. EPIPE explicitly transfers Committing back to
                    // Rejecting so one reaper can restore the parent.
                    let commit_result = match operator_quiesce.as_ref() {
                        Some(quiesce) => quiesce.with_commit_permit(|| {
                            crate::seamless::commit_and_exit(commit_fd, proof)
                        }),
                        None => Ok(crate::seamless::commit_and_exit(commit_fd, proof)),
                    };
                    match commit_result {
                        Ok(Err(_)) => commit_write_failed = true,
                        Err(error) => operator_quiesce_error = Some(error),
                        Ok(Ok(never)) => match never {},
                    }
                    let _ = arbiter.commit_failed_to_rejecting();
                } else {
                    commit_lost_arbiter = true;
                }
            }

            let rejection = operator_quiesce_error.map_or_else(
                || {
                    handoff_rejection_reason(
                        facts,
                        &native_safety,
                        commit_lost_arbiter,
                        commit_write_failed,
                    )
                },
                |error| format!("resident operator could not quiesce for Commit: {error}"),
            );
            let activity_shaped = handoff_rejection_activity_shaped(facts);
            if activity_shaped && let Some(pending) = self.pending_update_handoff.as_mut() {
                pending.revoked_by_activity = true;
            }
            aterm_log::warn!("update apply: {rejection}; rejecting readerless child");
            let rejection_started = arbiter.try_begin_reject()
                || arbiter.phase() == crate::HandoffAttemptPhase::Rejecting;
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
                            // QoS (port of 61a6c8b62): a REAPER is `Background`,
                            // never `Housekeeping` — it enforces a kill-and-reap
                            // deadline, and a starved reaper leaks the process
                            // whose CPU use is the problem (see `qos::Role`).
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
                        // Resource exhaustion cannot strand a readerless child.
                        // This fail-safe blocks only after both the normal worker
                        // and emergency thread creation have failed.
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
                // A live `Committing` owner is the only state in which rejection
                // may not proceed. Never signal or reap out from under its write.
                aterm_log::warn!(
                    "update apply: Commit owns the attempt; rejection left child untouched"
                );
            }
            return;
        }

        // Every non-ready completion is emitted only AFTER the worker killed and
        // reaped the child. It is now safe to restore parent readers and reduce the
        // exact ticket using disk facts that were also collected off the UI thread.
        let Some(teardown) = self.reduce_returned_handoff_completion(completion) else {
            return;
        };
        // The child process group is reaped and overlap rollback has run. Only now
        // may the event-loop lane replay destructive intent. A whole-app request
        // dominates individual closes; AppKit generation ownership is preserved.
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
                // The rollback worker may have taken long enough for a native
                // view to acquire new unsaved work after the document barrier's
                // completion. Revalidate at the actual exit edge and surface the
                // reducer's recovery payload instead of replaying a stale Ready.
                match self.prepare_quit_native_shutdown() {
                    Ok(true) => {
                        let _ = crate::menu::complete_current_native_termination();
                        el.exit();
                    }
                    Ok(false) => {
                        let _ = crate::menu::cancel_current_native_termination();
                    }
                    Err(error) => {
                        aterm_log::warn!("deferred clean-quit native barrier: {error}");
                        let _ = crate::menu::cancel_current_native_termination();
                    }
                }
            }
        }
    }

    /// The cached pre-park verification verdict for THIS exact artifact, or `None`
    /// when there is no fresh entry bound to its build and commit.
    ///
    /// `Some(false)` is the only answer with authority: it short-circuits
    /// [`Self::start_unix_update_handoff`] before a single reader parks. Split out
    /// of that function so the verdict a fixture seeds can be read back the way
    /// production reads it — a test that parks nothing (every headless one, since
    /// `native_update_admission` refuses the seamless lane without an event-loop
    /// proxy) cannot otherwise tell a healthy candidate from one production would
    /// decline outright, which is how a whole retry-policy suite came to be written
    /// against a `passed: false` fixture.
    /// The cached verdict's own reason for a refusal, when the entry is the
    /// fresh one for THIS artifact (the same key [`Self::cached_handoff_preverification`]
    /// reads by).
    #[cfg(unix)]
    #[must_use]
    pub(crate) fn cached_handoff_preverification_reason(
        &self,
        attempt: &crate::native_updater_service::ApplyAttemptTicket,
    ) -> Option<String> {
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
            .and_then(|entry| entry.reason.clone())
    }

    /// How many physical handoff failures this process has already booked
    /// against the artifact `attempt` targets — the number the freeze budget
    /// widens on. `0` with no attempt (the QA seam) or no record.
    #[must_use]
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

    /// The cached verdict's reason keyed by the ARTIFACT alone — for the
    /// submission-time failure arm, which has no ticket in hand (the attempt
    /// failed before any candidate was spawned) but knows the intent's build
    /// and digest. `Some(reason)` only for a fresh REFUSAL of that artifact.
    #[cfg(unix)]
    #[must_use]
    pub(crate) fn cached_handoff_refusal_reason(&self, build: u64) -> Option<String> {
        // The artifact the verdict is about is the live stage's digest for
        // this build — the same stage `poll` admitted the intent against.
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
    #[must_use]
    pub(crate) fn cached_handoff_preverification(
        &self,
        attempt: &crate::native_updater_service::ApplyAttemptTicket,
    ) -> Option<bool> {
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
            .map(|entry| entry.passed)
    }

    /// Reduce one RETURNED (non-ready) handoff completion: resume the parked
    /// readers, reduce the exact apply ticket against the lane its mode
    /// authorized, surface the verdict, and hand back the structural teardown the
    /// caller must replay. `None` means there was no matching pending attempt to
    /// reduce and nothing may be replayed.
    ///
    /// SPLIT OUT OF [`Self::finish_update_handoff`] SO IT CAN BE PROVEN. Every
    /// line here is event-loop-free — the `&ActiveEventLoop` the caller holds is
    /// needed only by the teardown replay below it — and this is the only place
    /// the attempt's `ApplyMode` still exists, which makes it the only place the
    /// automatic/person-initiated split can be gotten right (see
    /// [`crate::app_native::HandoffFailureLane`]).
    #[cfg(unix)]
    fn reduce_returned_handoff_completion(
        &mut self,
        completion: crate::UpdateHandoffCompletion,
    ) -> Option<crate::DeferredHandoffTeardown> {
        // (Unused fields keep underscore-prefixed bindings so their drops still
        // run at the end of this call, as with the old entry destructure.)
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
        // STATE-KEYED (2026-09-19, the late park). Two records can describe one
        // attempt: the PARKED one (`pending_update_handoff`, readers stopped)
        // and the PRELAUNCHED one (`update_handoff_prelaunch`, a successor
        // launched and held). A completion rolls the readers back iff the
        // parked record exists — never by a worker-reported phase, because a
        // stand-down can land after the park — and clears both records here and
        // only here, after the worker's death proof, so a retry can never launch
        // beside a live candidate. The parked record's facts win when both
        // exist: it is the later and fuller of the two.
        let parked = self
            .pending_update_handoff
            .take_if(|pending| pending.attempt_id == _attempt_id);
        let prelaunch = self
            .update_handoff_prelaunch
            .take_if(|prelaunch| prelaunch.attempt_id == _attempt_id);
        let pending = match (parked, prelaunch) {
            (Some(mut parked), prelaunch) => {
                if let Some(prelaunch) = prelaunch {
                    parked.teardown.merge(prelaunch.teardown);
                    parked.revoked_by_activity |= prelaunch.revoked_by_activity;
                }
                crate::ReturnedHandoffRecord {
                    mode: parked.mode,
                    apply_attempt: parked.apply_attempt,
                    same_image: parked.same_image,
                    teardown: parked.teardown,
                    revoked_by_activity: parked.revoked_by_activity,
                    parked_live: Some(parked.live),
                }
            }
            (None, Some(prelaunch)) => crate::ReturnedHandoffRecord {
                mode: prelaunch.mode,
                apply_attempt: prelaunch.apply_attempt,
                same_image: prelaunch.same_image,
                teardown: prelaunch.teardown,
                revoked_by_activity: prelaunch.revoked_by_activity,
                parked_live: None,
            },
            (None, None) => return None,
        };
        // Lane classification for the bounded automatic retry budgets, from the two
        // typed facts this reduction still holds: the mode the apply was authorized
        // under, and the worker's outcome (plus the main thread's own
        // activity-shaped rejection flag, the other half of the activity
        // observation).
        //
        // THE MODE IS CARRIED, NOT DROPPED. It used to reach only this line and
        // then vanish, so a failure a PERSON asked for was charged to the
        // automatic budget — converging the background lane on human retries and
        // silencing the row for the human who asked.
        //
        // AND NEITHER IS THE OUTCOME, WHICH IS THE SAME BUG ONE LEVEL DOWN. The
        // outcome used to be collapsed into a bare `activity_revoked` bool here,
        // so `TimedOut` (a missed deadline on a busy machine) and
        // `AdoptionMismatch` (two images that cannot agree on a proof) arrived at
        // the budget indistinguishable and were charged the same nine attempts
        // across fourteen hours. `classify` now sees the kind and
        // `PhysicalFailureShape` decides what it costs.
        //
        // …AND `child_death` IS THE THIRD FACT, for the one outcome that is not a
        // classification of anything: `ChildDied` is proof EOF, which a successor
        // that refused, one that faulted and one the machine starved all produce
        // identically. What the worker SAW at that instant travels here beside the
        // outcome rather than being re-inferred from a message string. See
        // [`crate::ChildDeathEvidence`].
        let lane = crate::app_native::HandoffFailureLane::classify(
            pending.mode,
            outcome,
            child_death,
            pending.revoked_by_activity,
        );
        let teardown = match (pending.mode, pending.teardown) {
            // Construction records this eagerly; keep a fail-safe derivation from
            // the typed mode so a future constructor cannot strand an authorized
            // clean quit merely by omitting the replay marker.
            (
                crate::native_updater_service::ApplyMode::CleanQuit,
                crate::DeferredHandoffTeardown::None,
            ) => crate::DeferredHandoffTeardown::CleanQuitReady,
            (_, teardown) => teardown,
        };
        // ROLLBACK IFF PARKED: a prelaunched attempt that never parked has
        // nothing to resume, and one whose readers were released ahead of the
        // stand-down (`Wake::UpdateHandoffStandingDown`) already resumed them.
        if let Some(live) = pending.parked_live.as_deref() {
            self.rollback_overlap(nonce.as_deref(), live);
        }
        // THE ROW SAID "INSTALLING" FOR THIS ATTEMPT, and the attempt is over:
        // put the words (or the absence of a row) back and end the charging
        // surge BEFORE the outcome below decides whether to say anything — a
        // silent outcome (the automatic lane standing down on activity) must not
        // leave a frozen "Installing" on the bar.
        self.retire_update_installing();
        // QA SEAM, READ BEFORE THE MATCH CONSUMES IT. A `None` ticket reaches this
        // reduction from exactly one place — `ATERM_DEBUG_SEAMLESS_REEXEC`, which
        // `start_native_update_handoff` is the only caller allowed to pair with a
        // missing attempt — so this outcome describes a SIMULATED apply of the
        // running binary, not a staged build that would not run. It must be logged
        // and shown, and it must not touch the durable ledger; the apply streak it
        // used to write is cleared only by a real successful apply, so QA runs
        // accrued forever and escalated to the persistent-failure notification.
        // WHICH same-image attempt this was, carried rather than inferred. This used to
        // read `pending.apply_attempt.is_none()`, whose comment said a None ticket
        // reaches this reduction from exactly one place — the QA seam. A provenance
        // repair also carries no ticket, so a failed repair was reported as a failed
        // debug-seam UPDATE and painted "Update stopped safely" for an update that never
        // existed.
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
        if let Some(surfaced) = surfaced {
            // The source names the lane the attempt actually rode, so the log can
            // no longer report a person's Version-menu apply as background work.
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

    #[cfg(not(unix))]
    pub(crate) fn finish_update_handoff(
        &mut self,
        _el: &ActiveEventLoop,
        _completion: crate::UpdateHandoffCompletion,
    ) {
        aterm_log::warn!("ignored unix-only handoff completion on this platform");
    }

    /// OVERLAP-HANDOFF failure rollback: restore this (still-running) parent to
    /// a fully-working terminal after a candidate that never became ready.
    ///
    /// PRECONDITION — the candidate was SIGNALLED and then proven TERMINATED, so
    /// exactly zero readers exist when ours restart. The proof is a
    /// [`HandoffRollbackWarrant`], minted on the worker (or the emergency
    /// reaper) before the completion that reaches this function is ever sent —
    /// see [`send_warranted_handoff_failure`]. It cannot be re-checked here: the
    /// completion crosses a channel and a warrant is not a value that survives
    /// the trip, so the ordering at the send site IS the guarantee. The
    /// pre-spawn failure paths in [`App::start_unix_update_handoff`] call this
    /// directly under the same rule, with no candidate to prove anything about.
    ///
    /// With that established:
    /// * the worker has already retired attempt artifacts and republished the
    ///   parent socket link before emitting this completion;
    /// * re-arm CLOEXEC on every master (the exact pre-apply fd posture);
    /// * resume the parked readers ([`Self::attach_deferred_readers`] — parked
    ///   sessions are self-describing: `reader_join: None`);
    ///
    /// The event-loop half performs no filesystem I/O, update-status read, or
    /// child-process probe.
    #[cfg(unix)]
    fn rollback_overlap(&mut self, _nonce: Option<&str>, live: &[(u64, i32, i32)]) {
        for (_, master, _) in live {
            let _ = aterm_pty::set_cloexec(*master, true);
        }
        self.resume_deferred_readers_nonblocking();
    }
}

/// Keep the actionable handoff cause in the returned refusal, which the updater
/// records in its durable status. A separate warning cannot answer a later
/// `update status`, and an unrelated admission failure must keep its own reason.
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
        }
    }

    #[test]
    fn live_session_refusal_retains_exact_handoff_cause_and_remedy() {
        for (unavailable, cause, remedy) in [
            (
                HandoffUnavailable::OptedOut,
                "$ATERM_NO_SEAMLESS_UPDATE is set",
                "Unset it to restore the default.",
            ),
            (
                HandoffUnavailable::Headless,
                "--headless (no window)",
                "A windowed launch restores the default.",
            ),
        ] {
            let facts = live_facts();
            let AdmissionDecision::Block(reason) = classify(facts) else {
                panic!("live terminals without seamless handoff must block");
            };
            let refusal = update_admission_refusal(reason, facts, Some(unavailable));
            assert!(
                refusal.is_refused(),
                "a refusal cannot increment a failure streak"
            );
            let message = refusal.into_message();
            assert!(message.contains(cause), "{message}");
            assert!(message.contains(remedy), "{message}");
            assert!(message.contains("3 live terminal session(s)"), "{message}");
            assert!(message.contains("2 foreground job(s)"), "{message}");
            assert!(
                message.chars().count() <= 400,
                "durable health truncates at 400 characters"
            );
            // Historical construction kept the counts but lost both actionable facts.
            assert!(!reason.message(facts).contains(cause));
            assert!(!reason.message(facts).contains(remedy));
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
                update_admission_refusal(expected, facts, Some(HandoffUnavailable::OptedOut));
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

/// Block (bounded) until the overlap-handoff child signals readiness, closes its
/// proof pipe, or times out. Crucially this never calls `Child::try_wait`: that
/// API reaps an exited group leader and destroys the PID identity before the
/// unique reaper can signal all descendants. Proof EOF detects pre-ready death;
/// after a full proof, an exited child is rejected by the atomic Commit pipe
/// write (EPIPE). The deadline defaults to 15 s — generous against a
/// staged-swap re-exec + GPU init + multi-window present (~1-2 s observed) —
/// and is tunable via `ATERM_HANDOFF_READY_TIMEOUT_MS` for the QA seam.
///
/// SEAMLESS: `masters` are watched for session DEATH (HUP/ERR/NVAL →
/// `Rejected` — the adoption proof's live-set identity is stale) but NOT for
/// readable output — shell bytes produced while the child boots wait in the
/// kernel queues and replay through the child's fresh parser after Commit, so
/// output during the overlap must never abort the wait. A cancel poke returns
/// the typed `ActivityRevoked` so the retry budget can classify it.
/// The three mutually exclusive verdicts on one `poll` answer during the ready
/// wait. Split out from the loop so the anti-spin contract is provable without
/// timing: queued master output must land on [`ReadyPollAction::NoProgress`]
/// (the yielding branch), NEVER on a path that re-polls immediately.
#[cfg(unix)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ReadyPollAction {
    /// A handed-off master reported HUP/ERR/NVAL — the adopted live-set identity
    /// is stale; reject.
    SessionDied,
    /// The proof fd is readable (POLLIN or its own HUP/ERR/NVAL) — read the wire.
    ReadProof,
    /// Neither: `poll` returned because a master had queued readable output
    /// (plain POLLIN — deliberately not an abort) or a bare slice. No progress
    /// toward readiness, so the caller must YIELD before re-polling; a master
    /// that stays readable would otherwise make `poll` return instantly forever.
    NoProgress,
}

/// Classify one `poll` answer. Death on ANY master dominates (the proof is
/// meaningless if the adopted set is already stale); otherwise a readable proof
/// fd is progress; otherwise there is nothing to do but yield. `pollfds[0]` is
/// the proof fd, `pollfds[1..]` the watched masters (see [`wait_handoff_ready`]).
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

/// What the main thread hands `prelaunch_out_of_band_handoff`: every pre-park
/// fact the launched lane needs, taken from `start_unix_update_handoff` at the
/// point the lane was decided.
#[cfg(target_os = "macos")]
struct PrelaunchArgs {
    attempt_id: u64,
    build: u64,
    mode: crate::native_updater_service::ApplyMode,
    apply_attempt: Option<crate::native_updater_service::ApplyAttemptTicket>,
    same_image: Option<SameImageHandoff>,
    target_build: u64,
    target_commit: String,
    command: std::process::Command,
    verify_staged_candidate: bool,
    installed_activation: bool,
    bundle: Option<std::path::PathBuf>,
    cancel: std::sync::mpsc::SyncSender<()>,
    cancelled: std::sync::mpsc::Receiver<()>,
    job_tx: std::sync::mpsc::SyncSender<HandoffWorkerJob>,
    reconcile_worker: crate::app_native::NativeUpdateReconcileSender,
    reconcile_ticket: crate::app_native::NativeUpdateReconcileTicket,
}

/// The facts the park gate reads, in the shape a test can enumerate.
///
/// Every one of them is measured on the main thread immediately before the gate
/// runs; none is remembered from the attempt's entry, because the entry is now
/// seconds or minutes earlier — the successor was launched there and has been
/// booting and holding since.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ParkGateFacts {
    pub(crate) mode: crate::native_updater_service::ApplyMode,
    /// `App::automatic_apply_phase`: where the automatic lane stands on its
    /// ladder at this instant. Ignored by the explicit modes.
    pub(crate) phase: crate::native_update_auto_intent::ApplyPhase,
    /// `App::automatic_activity_facts`: the terminal's quiet / keys / output /
    /// focus facts, sampled at this instant.
    pub(crate) activity: crate::native_update_auto_intent::ActivityFacts,
    /// `!handoff_masters_have_activity`: no master has bytes waiting that a
    /// reader has not consumed.
    pub(crate) masters_quiet: bool,
    /// How long the successor has been holding its claim.
    pub(crate) held_for: std::time::Duration,
}

/// What the park gate decides.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ParkGate {
    /// Park now.
    Park,
    /// Not now; the successor keeps holding and the gate re-runs.
    Wait(&'static str),
    /// The hold cap expired without a moment to park in: stand the successor
    /// down. Activity-revoked, so the intent is retained and the automatic lane
    /// tries again at a later quiet window.
    StandDown(&'static str),
}

/// WHEN THE TERMINAL MAY BE FROZEN, now that freezing it is a separate decision
/// from starting the update (2026-09-19, the late park).
///
/// Before this, one gate at the attempt's ENTRY decided both, because the park
/// was the first thing an attempt did. The entry gate still stands and still
/// decides whether an attempt starts at all — `AutomaticAttemptRequiresQuiet\
/// OrClosedGraceWindow` binds the launch through it — but the launch costs the
/// user nothing: every reader stays live through the successor's swap, second
/// start and boot check. This gate decides the only thing the user feels.
///
/// So the rule is the one the ENTRY gate applied, re-read at the instant it
/// matters:
///
/// * `Immediate` / `CleanQuit` — the person asked for this. Park at once; they
///   are waiting for it, and a gate here would be aterm deciding it knows better.
/// * the automatic modes — THE LADDER
///   (`native_update_auto_intent::automatic_park_refusal`), read at its live
///   phase: a quiet moment while the lane prefers idle, then a gap in output
///   while an aterm window is focused, then a gap in typing, then nothing. A
///   successor launched under `Automatic` and held into a later phase parks by
///   that later phase's rule; the mode only says which entry admitted it.
///
/// Plus one fact the ladder never relaxes: bytes waiting on a master that its
/// reader has not taken. Parking on top of them commits a screen digest the
/// successor cannot reproduce, so that is a correctness gate, not a comfort,
/// and it holds in every phase (the reader takes bytes within microseconds, so
/// the 20 ms re-run finds a clean instant).
///
/// MONOTONE IN `held_for`: past the cap every automatic mode stands down, and
/// nothing can make a stood-down gate admit again. That is what bounds the hold
/// — a booted successor is not left waiting behind a terminal that never goes
/// quiet; the ladder's next phase admits the next attempt sooner.
#[cfg(unix)]
#[must_use]
pub(crate) fn prelaunch_park_admitted(
    facts: ParkGateFacts,
    hold_cap: Option<std::time::Duration>,
) -> ParkGate {
    use crate::native_updater_service::ApplyMode;
    if matches!(facts.mode, ApplyMode::Immediate | ApplyMode::CleanQuit) {
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
    if !facts.masters_quiet {
        return ParkGate::Wait("a session has output waiting that its reader has not taken");
    }
    ParkGate::Park
}

/// The hold cap for `mode`: the automatic lanes are bounded, the explicit ones
/// park at once and so can never reach a cap.
#[cfg(unix)]
#[must_use]
pub(crate) fn prelaunch_hold_cap(
    mode: crate::native_updater_service::ApplyMode,
) -> Option<std::time::Duration> {
    mode.is_automatic().then_some(PRELAUNCH_HOLD_MAX)
}

/// One run of the park gate for a prelaunched attempt.
#[cfg(unix)]
enum ParkAttempt {
    /// Parked, captured, and the capture is on its way to the worker.
    Parked,
    /// Not now — re-run the gate after [`PRELAUNCH_PARK_RETRY`].
    NotYet(&'static str),
    /// The park itself missed: the readers were parked and rolled back, nothing
    /// was granted, and the successor is still holding. Re-park once, after
    /// [`PRELAUNCH_REPARK_DELAY`] and on the ladder's NEXT freeze-budget rung.
    Missed(String),
    /// Stand the held successor down with this outcome; readers (if they were
    /// parked) have already been rolled back.
    Failed(HandoffStandDown),
}

/// How often the event loop re-runs the park gate while a prelaunched attempt
/// waits for a quiet moment. Short enough that the park lands inside the quiet
/// epoch it is waiting for (500 ms), long enough to be no load.
#[cfg(unix)]
const PRELAUNCH_PARK_RETRY: std::time::Duration = std::time::Duration::from_millis(20);

/// How long the gate waits before re-parking after a park that missed its
/// budget. Long enough for whatever took the engine lock (the scrollback
/// compressor, a render) to finish, short enough that the successor's hold is
/// not visibly spent on it.
#[cfg(unix)]
const PRELAUNCH_REPARK_DELAY: std::time::Duration = std::time::Duration::from_millis(500);

/// How many times one prelaunched attempt re-parks before it stands its
/// successor down. TWO: the whole freeze ladder (20 ms -> 80 ms -> 250 ms,
/// [`handoff_freeze_budget`]) is climbed INSIDE one attempt, 500 ms apart, with
/// the successor still booted and holding — three parks cost the user at most
/// 350 ms of stalled echo across a second, against a whole relaunch per rung.
///
/// It used to be ONE, with the second miss handed to "the ledger's own rung
/// widening": the stand-down was filed as `PreparationFailed`, which
/// [`crate::app_native::PhysicalFailureShape::of_outcome`] charges as
/// STRUCTURAL — a verdict about the BYTES, with a two-attempt lifetime. Two
/// busy moments ten minutes apart therefore converged the automatic lane to a
/// deadline-less manual-only latch for the artifact (2026-09-21, the second
/// finding of the ladder audit). A miss is the parent's scheduling failure and
/// says nothing about the candidate, so the ledger is exactly the wrong place
/// for it; the attempt climbs the rungs itself and, past the last one, stands
/// down as a busy-machine fact ([`park_miss_disposition`]).
#[cfg(unix)]
pub(crate) const PRELAUNCH_MAX_PARK_MISSES: u8 = 2;

/// What one park that missed its freeze budget costs the prelaunched attempt.
#[cfg(unix)]
#[derive(Debug)]
pub(crate) enum ParkMissDisposition {
    /// Re-park after [`PRELAUNCH_REPARK_DELAY`] on the ladder's next rung;
    /// `misses` is the attempt's new miss count, which is also the rung the
    /// re-park widens to.
    Repark { misses: u8, reason: String },
    /// Every rung was tried. Stand the successor down.
    StandDown(HandoffStandDown),
}

/// A PARK MISS IS A FACT ABOUT THE MACHINE'S MOMENT, NEVER ABOUT THE BYTES.
///
/// The readers came back, nothing was granted, no artifact was written; the only
/// thing that happened is that a busy machine did not stop its readers (or
/// capture its screens) inside a comfort budget. That is the same kind of fact
/// as a keystroke or the prelaunch hold cap — a scheduling fact about this
/// terminal — so past the last rung the successor stands down as
/// `ActivityRevoked`: retried on the activity spacing, the ladder's anchor
/// untouched, no physical budget spent, never a manual-only latch. The design's
/// law (docs/DESIGN-auto-apply-ladder-2026-09-21.md, law 2) reserves the latch
/// for a successor that died or a proof that did not match; a stopwatch the
/// parent missed is neither.
#[cfg(unix)]
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

/// How long a dialled successor is held for a quiet moment before the attempt
/// stands down (activity-revoked: no physical budget spent, retried later). The
/// automatic lane's activity grace, so a machine that is never quiet is not
/// left with a booted successor waiting behind it for longer than its own
/// entry policy would wait.
#[cfg(unix)]
const PRELAUNCH_HOLD_MAX: std::time::Duration = std::time::Duration::from_secs(120);

/// When the POST-PARK proof wait gives up on the launched lane (2026-09-19).
///
/// Under the late park everything slow — the launch, the bundle swap, the
/// second `execve`, the boot check — has already happened when the readers
/// park, so what this bounds is the REST of the freeze: the artifacts this
/// attempt still has to write (`prepare_outgoing_artifacts` — the control
/// carry's export, the manifest, a grid sidecar per session, the layout), the
/// one `sendmsg`, and the successor's GUI boot to its first present (~180 ms
/// measured idle, and the dominant term). Anchoring it at the park rather than
/// at the grant is deliberate: what must be bounded is the interval the user's
/// terminal is frozen, not the part of it this process finds convenient. A missed deadline is a kill, a
/// proof of death and a resumed terminal, charged to the physical retry ladder
/// like every other missed proof, so the ladder widens it: 3 s on a first
/// automatic attempt, 6 s after a physical failure of the same bytes, 30 s for
/// an explicit apply (the person asked, and a busy machine may take it).
/// `ATERM_HANDOFF_PROOF_TIMEOUT_MS` overrides all three for the QA seam.
#[cfg(unix)]
#[must_use]
fn handoff_proof_deadline(
    mode: crate::native_updater_service::ApplyMode,
    prior_physical_failures: u8,
    park_at: std::time::Instant,
) -> std::time::Instant {
    use crate::native_updater_service::ApplyMode;
    let ms = std::env::var("ATERM_HANDOFF_PROOF_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(match mode {
            ApplyMode::Immediate | ApplyMode::CleanQuit => 30_000,
            ApplyMode::Automatic | ApplyMode::AutomaticPastGrace => match prior_physical_failures {
                0 => 3_000,
                _ => 6_000,
            },
        })
        .clamp(1_000, 120_000);
    park_at + std::time::Duration::from_millis(ms)
}

/// When this attempt stops waiting for its successor to prove readiness.
///
/// A DEADLINE RATHER THAN A TIMEOUT, because on the out-of-band lane the wait
/// has two stages and they share one budget: the successor has to dial the
/// rendezvous (behind its whole staged-swap boot apply) and only then paint and
/// prove. Two independent 15 s timeouts would let a slow successor park the
/// user's terminal for thirty seconds; one deadline computed once is what makes
/// "the fork lane's budget" mean the same thing on both lanes. The fork lane
/// computes it immediately before its single wait, which is exactly where the
/// function it replaced computed it.
#[cfg(unix)]
#[must_use]
fn handoff_ready_deadline() -> std::time::Instant {
    // DEFAULT RAISED 15 s → 30 s (2026-08-15). The one deadline covers
    // LaunchServices latency, first-launch Gatekeeper assessment, the
    // successor's ENTIRE staged-swap boot apply (ditto/codesign + re-exec),
    // adoption, and the paint of every window — the codebase's own cold
    // measurement is 4.5 s with nothing else running, and this machine's
    // ledger carried a real TimedOut streak whose live failures all landed
    // under compile load (reproduced on demand: a rehearsal handoff during a
    // workspace build times out at 15 s and completes with headroom at more).
    // The cost of a longer deadline is bounded and safe — the parked parent
    // un-parks on expiry exactly as before — while the cost of a short one is
    // an update lane that quietly never succeeds on a busy machine.
    let timeout_ms = std::env::var("ATERM_HANDOFF_READY_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(30_000)
        .clamp(1_000, 120_000);
    std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms)
}

/// LaunchServices' still-pending answer to the launch of the dialer the parent
/// already accepted on the kernel-attested rendezvous (2026-09-19): polled with a
/// ZERO budget on every slice of [`wait_handoff_ready`], so the corroboration
/// never holds the parked terminal, and a mismatch — LaunchServices naming a
/// different pid than the dialer, i.e. a stranger that knew the claim secret —
/// still refuses the handoff before Commit. The descriptors are the peer's by
/// then, so the refusal is a kill-and-reap like every other post-transfer
/// rejection, never a `NeverTransferred`.
#[cfg(unix)]
struct LaunchCorroboration<'a> {
    in_flight: &'a crate::app_launch_successor::LaunchInFlight,
    dialer_pid: i32,
    /// Set once LaunchServices has spoken (a match, or an error about a job that
    /// evidently ran): the answer is consumed by the first `wait` that sees it,
    /// so it is never polled for twice.
    answered: bool,
}

#[cfg(unix)]
impl LaunchCorroboration<'_> {
    /// One zero-budget poll. `false` means the handoff must be REFUSED: the
    /// launch answer names a pid other than the dialer's.
    fn poll(&mut self) -> bool {
        if self.answered {
            return true;
        }
        match self.in_flight.wait(std::time::Duration::ZERO) {
            Ok(successor) if successor.pid() == self.dialer_pid => {
                self.answered = true;
                aterm_log::info!(
                    "update apply: LaunchServices corroborates the dialer as pid {} (answered \
                     beside the proof wait)",
                    self.dialer_pid
                );
                true
            }
            Ok(successor) => {
                self.answered = true;
                aterm_log::warn!(
                    "update apply: LaunchServices names pid {} as the successor it launched; \
                     the dialer is pid {} — refusing the handoff",
                    successor.pid(),
                    self.dialer_pid
                );
                false
            }
            Err(crate::app_launch_successor::LaunchError::Timeout(_)) => true,
            Err(error) => {
                self.answered = true;
                aterm_log::warn!(
                    "update apply: LaunchServices answered the launch with an error after the \
                     successor had already dialled ({error}); proceeding on the attested dialer"
                );
                true
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
    mut corroboration: Option<LaunchCorroboration<'_>>,
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
    // Masters are watched for DEATH only: POLLIN must be REQUESTED (macOS
    // evaluates a PTY's stream state only for requested events and reports a
    // dead slave as POLLIN|POLLHUP; with `events: 0` it reports nothing) but
    // plain POLLIN in the answer is IGNORED below — queued shell output waits
    // in the kernel for the child and must not abort the wait.
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
        if let Some(corroboration) = corroboration.as_mut()
            && !corroboration.poll()
        {
            return crate::UpdateHandoffOutcome::Rejected;
        }
        for pfd in &mut pollfds {
            pfd.revents = 0;
        }
        // SAFETY: a stable initialized pollfd slice; 10 ms bounds death abort.
        let n = unsafe { libc::poll(pollfds.as_mut_ptr(), pollfds.len() as libc::nfds_t, 10) };
        if n <= 0 {
            continue; // timeout slice or EINTR — re-check child + deadline (poll already blocked)
        }
        match classify_ready_poll(&pollfds) {
            ReadyPollAction::SessionDied => return crate::UpdateHandoffOutcome::Rejected,
            ReadyPollAction::NoProgress => {
                // A booting child's shell can produce queued output on a handed-
                // off master (the tolerate-output contract). That master answers
                // plain POLLIN, which makes `poll` return IMMEDIATELY every
                // iteration but matches neither the dead mask nor the proof fd —
                // so without this yield the loop would busy-spin at 100% CPU,
                // starving the very child we are waiting on. The same ~2 ms sleep
                // the post-ProofReady decision loop uses bounds the spin while
                // staying an order of magnitude tighter than the ~1-2 s boot the
                // proof arrives after.
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
                // Recheck AFTER the complete proof. A child that raced death with
                // the final bytes is not an exit authority.
                if cancel.try_recv().is_ok() {
                    return crate::UpdateHandoffOutcome::ActivityRevoked;
                }
                return crate::UpdateHandoffOutcome::ProofReady;
            }
            // EOF: the only live write end was the child's, so it dropped the fd
            // (a failed validation) or died/malformed mid-proof.
            0 => return crate::UpdateHandoffOutcome::ChildDied,
            _ => continue, // EINTR/EAGAIN — re-loop
        }
    }
}

/// THE LATE PARK'S STAND-DOWN, driven against real processes and the real
/// rendezvous (2026-09-19). A held successor holds nothing, so standing it down
/// is: close the rendezvous (EOF), give it the grace to exit by its own rule,
/// kill it if it does not, and prove it gone either way — and say which of the
/// two happened, because exactly one party forgives the counted trial launch.
#[cfg(all(test, target_os = "macos"))]
mod held_successor_stand_down_tests {
    use super::{
        CandidateExitWatch, HandoffCandidate, HandoffRollbackWarrant, HeldSuccessor, StandDownStep,
        StoodDownSuccessor,
    };
    use aterm_spec::derive::{Model, native_update_overlap_handoff_model};
    use aterm_spec::interp::{State, with_buggy};
    use std::io::Read as _;

    /// Bind a rendezvous, dial it from a test thread, accept and HOLD the claim,
    /// and spawn `script` as the process the held claim stands for. `None` when
    /// this machine has no private control dir (then no rendezvous binds at all).
    fn hold(script: &str) -> Option<(HeldSuccessor, std::process::Child, aterm_uds::CtlStream)> {
        let nonce = aterm_uds::rand::hex_token::<16>().ok()?;
        let rendezvous = match crate::handoff_rendezvous::Rendezvous::bind(&nonce) {
            Ok(rendezvous) => rendezvous,
            Err(
                crate::handoff_rendezvous::RendezvousError::NoControlDir
                | crate::handoff_rendezvous::RendezvousError::PathTooLong { .. },
            ) => return None,
            Err(error) => panic!("bind: {error}"),
        };
        let path = rendezvous.path().to_path_buf();
        let secret = rendezvous.claim().to_string();
        let dialer = std::thread::spawn(move || {
            crate::handoff_rendezvous::dial_for_test(&path, &secret).expect("dial")
        });
        let peer = rendezvous
            .accept_claim(
                None,
                std::time::Instant::now() + std::time::Duration::from_secs(10),
                &|| false,
            )
            .expect("the test dialer presents the claim");
        let stream = dialer.join().expect("dialer thread");
        let child = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(script)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn the stand-in candidate");
        let candidate = HandoffCandidate::of_unreaped_child(&child);
        let exit_watch = CandidateExitWatch::watch(child.id());
        assert!(exit_watch.is_some(), "EVFILT_PROC attaches to a live child");
        Some((
            HeldSuccessor {
                rendezvous,
                peer,
                candidate,
                exit_watch,
                dialled_at: std::time::Instant::now(),
            },
            child,
            stream,
        ))
    }

    /// The candidate is our fork child here (the launched lane's is launchd's),
    /// so somebody must reap it or its pid never becomes vacant and the
    /// termination proof cannot complete — launchd does that for the real one.
    fn reap_in_background(
        mut child: std::process::Child,
    ) -> std::thread::JoinHandle<std::process::ExitStatus> {
        std::thread::spawn(move || child.wait().expect("reap the stand-in"))
    }

    /// THE PROJECTION. Replay the steps the REAL stand-down emitted against the
    /// derived overlap model, from the state a launched-but-ungranted successor
    /// is in, and require each one to be an enabled action there. This is what
    /// makes the model a claim about this file: a stand-down that skipped the
    /// kill, or retired before the reap, would produce a step sequence with no
    /// admitted trace and fail here.
    fn project_onto_the_model(model: &Model, stood: &StoodDownSuccessor) -> State {
        let mut state = model.init_state();
        // The lane this stand-down belongs to: launched before the park, nothing
        // granted, and the main thread's stand-down announcement already
        // answered (`answer_prelaunched_stand_down`, which is what the worker
        // waits for before the first step below).
        for action in ["LaunchSuccessorBeforePark", "RevokeUngrantedSuccessor"] {
            let next = model.successors(action, &state);
            assert_eq!(next.len(), 1, "{action} must be enabled at {state:?}");
            state = next[0].clone();
        }
        // `Revoke` is that step; the rest are what the code chose.
        assert_eq!(
            stood.steps.first(),
            Some(&StandDownStep::Revoke),
            "a stand-down always begins by closing the rendezvous"
        );
        for step in stood.steps.iter().skip(1) {
            let action = step.model_action();
            let next = model.successors(action, &state);
            assert_eq!(
                next.len(),
                1,
                "the code emitted {step:?}, which the model does not admit at {state:?}"
            );
            state = next[0].clone();
        }
        // And only now may the attempt retire — the completion the worker sends
        // next is what clears the record.
        let retired = model.successors("RetireUngrantedAttempt", &state);
        assert_eq!(
            retired.len(),
            1,
            "the steps the code took must license the retire: {state:?}"
        );
        let retired = retired[0].clone();
        for invariant in &model.invariants {
            assert!(
                model.check_invariant(invariant.name, &retired),
                "the projected stand-down violates {}: {retired:?}",
                invariant.name
            );
        }
        assert_eq!(retired["parent_readers"], 1, "the readers are the user's");
        assert_eq!(retired["granted"], 0, "nothing was ever granted");
        assert_eq!(retired["child_live"], 0, "the successor is proven gone");
        retired
    }

    /// The negative control that makes the projection above a claim: retiring
    /// while the successor is still live is executable ONLY in the mutant, and
    /// the invariant the projection checks is what catches it.
    fn assert_a_live_successor_cannot_retire(model: &Model) {
        let mut state = model.init_state();
        for action in ["LaunchSuccessorBeforePark", "RevokeUngrantedSuccessor"] {
            state = model.successors(action, &state)[0].clone();
        }
        assert!(
            model
                .successors("RetireUngrantedAttempt", &state)
                .is_empty(),
            "the healthy model never retires over a live successor"
        );
        let buggy = with_buggy(model, 1);
        let mut early = state.clone();
        assert!(buggy.fire("BuggyRetireUngrantedWithSuccessorLive", &mut early));
        assert!(!buggy.check_invariant("UngrantedRetireRequiresReap", &early));
    }

    fn reads_eof(stream: &aterm_uds::CtlStream) -> bool {
        let mut byte = [0u8; 1];
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
        matches!((&*stream).read(&mut byte), Ok(0))
    }

    /// (a) A successor that exits on the EOF by its own rule, inside the grace:
    /// proven gone WITHOUT a signal, its own exit status witnessed, and
    /// `signalled == false` — so the parent does NOT also forgive the trial
    /// launch the successor already forgave.
    #[test]
    fn a_successor_that_exits_on_eof_is_proven_gone_without_a_signal() {
        let Some((held, child, stream)) = hold("sleep 0.2") else {
            return;
        };
        let reaper = reap_in_background(child);
        let stood = super::stand_down_held_successor(held, std::time::Duration::from_secs(10));
        assert!(!stood.signalled, "an EOF exit needs no kill");
        assert_eq!(stood.warrant, HandoffRollbackWarrant::Vanished);
        assert_eq!(
            stood.death,
            crate::ChildDeathEvidence::Exited { code: 0 },
            "the witness records the candidate's OWN exit"
        );
        assert!(reads_eof(&stream), "the dialer read the stand-down as EOF");
        assert_eq!(reaper.join().expect("reaper").code(), Some(0));
        // THE ORDER IT TOOK, against the model: revoke, its own exit, the proof.
        assert_eq!(
            stood.steps,
            vec![
                StandDownStep::Revoke,
                StandDownStep::SuccessorExited,
                StandDownStep::Reap
            ]
        );
        let model = native_update_overlap_handoff_model();
        let retired = project_onto_the_model(&model, &stood);
        assert_eq!(
            retired["group_signaled"], 0,
            "nothing of ours signalled it, which is why the PARENT forgives no \
             trial launch here"
        );
        assert_eq!(retired["successor_exited"], 1);
        assert_a_live_successor_cannot_retire(&model);
    }

    /// (b) A successor that ignores the EOF for the whole grace is killed and
    /// proven gone; the kill is our own, so the death is `Unobserved` and
    /// `signalled == true` is what licenses the parent's forgiveness.
    #[test]
    fn a_successor_that_ignores_eof_is_killed_after_the_grace_and_proven_gone() {
        let Some((held, child, stream)) = hold("sleep 30") else {
            return;
        };
        let reaper = reap_in_background(child);
        let started = std::time::Instant::now();
        let stood = super::stand_down_held_successor(held, std::time::Duration::from_millis(200));
        assert!(
            stood.signalled,
            "the grace expired, so the candidate was killed"
        );
        assert!(
            started.elapsed() >= std::time::Duration::from_millis(200),
            "the grace is honoured before the kill"
        );
        assert_eq!(stood.warrant, HandoffRollbackWarrant::Vanished);
        assert_eq!(
            stood.death,
            crate::ChildDeathEvidence::Unobserved,
            "our own SIGKILL is not evidence about the bytes"
        );
        assert!(reads_eof(&stream), "the dialer read the stand-down as EOF");
        use std::os::unix::process::ExitStatusExt as _;
        assert_eq!(reaper.join().expect("reaper").signal(), Some(libc::SIGKILL));
        // THE ORDER IT TOOK, against the model: revoke, kill, the proof — the
        // order `UngrantedRetireRequiresReap` exists to enforce.
        assert_eq!(
            stood.steps,
            vec![
                StandDownStep::Revoke,
                StandDownStep::Kill,
                StandDownStep::Reap
            ]
        );
        let model = native_update_overlap_handoff_model();
        let retired = project_onto_the_model(&model, &stood);
        assert_eq!(retired["child_killed"], 1);
        assert_eq!(retired["group_signaled"], 1);
        assert_a_live_successor_cannot_retire(&model);
    }
}

/// The late park's attempt records and the completion reducer (2026-09-19).
#[cfg(all(test, unix))]
mod late_park_record_tests {
    use crate::App;
    use crate::native_updater_service::ApplyMode;

    fn prelaunch_record(
        attempt_id: u64,
        mode: ApplyMode,
    ) -> (
        crate::HandoffPrelaunch,
        std::sync::mpsc::Receiver<()>,
        std::sync::mpsc::Receiver<super::HandoffStandDown>,
        std::sync::mpsc::Receiver<super::HandoffTransferJob>,
    ) {
        let (cancel, cancelled) = std::sync::mpsc::sync_channel(1);
        let (stand_down, stood_down) = std::sync::mpsc::sync_channel(1);
        let (transfer, transferred) = std::sync::mpsc::sync_channel(1);
        (
            crate::HandoffPrelaunch {
                stand_down_ack: std::sync::mpsc::sync_channel(1).0,
                attempt_id,
                nonce: "0123456789abcdef0123456789abcdef".to_string(),
                mode,
                apply_attempt: None,
                same_image: Some(super::SameImageHandoff::DebugSeam),
                target_build: crate::build_info::BUILD_NUMBER.parse().unwrap_or(0),
                target_commit: crate::build_info::GIT_COMMIT.to_string(),
                cancel,
                stand_down,
                transfer,
                arbiter: crate::HandoffAttemptArbiter::new(),
                launched_at: std::time::Instant::now(),
                dialled: None,
                park_retry_at: None,
                park_misses: 0,
                stood_down: false,
                teardown: crate::DeferredHandoffTeardown::None,
                revoked_by_activity: false,
            },
            cancelled,
            stood_down,
            transferred,
        )
    }

    fn parked_record(attempt_id: u64, mode: ApplyMode) -> crate::PendingUpdateHandoff {
        let (cancel, _cancelled) = std::sync::mpsc::sync_channel(1);
        crate::PendingUpdateHandoff {
            park_at: std::time::Instant::now(),
            proof_ready_at: None,
            activate_at_commit: false,
            attempt_id,
            nonce: None,
            live: Vec::new(),
            adoption: Vec::new(),
            child_pid: None,
            mode,
            apply_attempt: None,
            same_image: Some(super::SameImageHandoff::DebugSeam),
            target_build: 0,
            target_commit: String::new(),
            layout: crate::restore::RestoreManifest::new(Vec::new()),
            layout_digest: [0; 32],
            screen_digest: [0; 32],
            activity_epoch: 0,
            cancel,
            arbiter: crate::HandoffAttemptArbiter::new(),
            teardown: crate::DeferredHandoffTeardown::None,
            commit_drain_started: None,
            revoked_by_activity: false,
        }
    }

    fn completion(attempt_id: u64) -> crate::UpdateHandoffCompletion {
        crate::UpdateHandoffCompletion {
            attempt_id,
            nonce: None,
            child_pid: None,
            outcome: crate::UpdateHandoffOutcome::ActivityRevoked,
            commit_fd: None,
            reject: None,
            reconcile: None,
            detail: "stood down".to_string(),
            input_drain_spins: 0,
            child_death: crate::ChildDeathEvidence::Unobserved,
        }
    }

    /// A completion for an attempt that only ever PRELAUNCHED clears that
    /// record (so a retry may launch) and derives the clean-quit replay from
    /// the mode, exactly as a parked attempt's would.
    #[test]
    fn a_prelaunched_attempt_is_cleared_by_its_completion_and_keeps_its_clean_quit_replay() {
        let mut app = App::headless_for_test();
        let (record, _cancelled, _stood_down, _transferred) =
            prelaunch_record(9, ApplyMode::CleanQuit);
        app.update_handoff_prelaunch = Some(record);
        assert!(app.update_handoff_in_flight());
        let teardown = app
            .reduce_returned_handoff_completion(completion(9))
            .expect("the prelaunched attempt is reduced");
        assert_eq!(teardown, crate::DeferredHandoffTeardown::CleanQuitReady);
        assert!(app.update_handoff_prelaunch.is_none());
        assert!(app.pending_update_handoff.is_none());
        assert!(!app.update_handoff_in_flight());
    }

    /// A completion whose attempt matches NEITHER record is not reduced and
    /// clears nothing: a stale completion can never retire a newer attempt.
    #[test]
    fn a_stale_completion_clears_no_record() {
        let mut app = App::headless_for_test();
        let (record, _cancelled, _stood_down, _transferred) =
            prelaunch_record(9, ApplyMode::Automatic);
        app.update_handoff_prelaunch = Some(record);
        assert!(
            app.reduce_returned_handoff_completion(completion(8))
                .is_none()
        );
        assert!(app.update_handoff_prelaunch.is_some());
    }

    /// Both records for one attempt: the parked one's facts win, its teardown
    /// merges the prelaunch record's, and both are cleared by the one completion.
    #[test]
    fn both_records_of_one_attempt_are_cleared_together_and_their_teardowns_merge() {
        let mut app = App::headless_for_test();
        let (mut record, _cancelled, _stood_down, _transferred) =
            prelaunch_record(4, ApplyMode::Immediate);
        record.teardown = crate::DeferredHandoffTeardown::QuitRequested;
        app.update_handoff_prelaunch = Some(record);
        app.pending_update_handoff = Some(parked_record(4, ApplyMode::Immediate));
        let teardown = app
            .reduce_returned_handoff_completion(completion(4))
            .expect("reduced");
        assert_eq!(teardown, crate::DeferredHandoffTeardown::QuitRequested);
        assert!(app.update_handoff_prelaunch.is_none());
        assert!(app.pending_update_handoff.is_none());
    }

    /// `Wake::UpdateHandoffStandingDown`: the park gate closes for good, the
    /// parked record is released — its readers resume — and the attempt record
    /// stays, carrying the deferred teardown forward, so nothing can launch
    /// beside the successor being stood down.
    #[test]
    fn a_stand_down_announcement_closes_the_gate_and_releases_the_parked_record() {
        let mut app = App::headless_for_test();
        let (mut record, _cancelled, _stood_down, _transferred) =
            prelaunch_record(4, ApplyMode::Automatic);
        let (ack_tx, ack) = std::sync::mpsc::sync_channel(1);
        record.stand_down_ack = ack_tx;
        app.update_handoff_prelaunch = Some(record);
        let mut parked = parked_record(4, ApplyMode::Automatic);
        parked.teardown = crate::DeferredHandoffTeardown::QuitRequested;
        parked.revoked_by_activity = true;
        app.pending_update_handoff = Some(parked);
        app.answer_prelaunched_stand_down(3);
        assert!(
            app.pending_update_handoff.is_some(),
            "an announcement for another attempt releases nothing"
        );
        assert!(
            !app.update_handoff_prelaunch
                .as_ref()
                .expect("record")
                .stood_down,
            "and closes no gate"
        );
        app.answer_prelaunched_stand_down(4);
        assert!(app.pending_update_handoff.is_none());
        let prelaunch = app
            .update_handoff_prelaunch
            .as_ref()
            .expect("the attempt record outlives the release");
        assert!(
            prelaunch.stood_down,
            "the park gate is shut: nothing may park onto a successor being killed"
        );
        assert!(prelaunch.park_retry_at.is_none());
        assert_eq!(
            prelaunch.teardown,
            crate::DeferredHandoffTeardown::QuitRequested
        );
        assert!(prelaunch.revoked_by_activity);
        assert!(app.update_handoff_in_flight());
        // The worker is waiting on this answer before it revokes.
        assert!(ack.try_recv().is_ok(), "the worker is answered last");
    }

    /// A destructive intent against a HELD successor is no barrier: nothing is
    /// parked and nothing was granted, so the intent proceeds and the worker is
    /// asked to stand the successor down. Against a PARKED attempt it is the
    /// barrier it always was.
    #[test]
    fn a_teardown_is_no_barrier_for_a_held_successor_but_stands_it_down() {
        let mut app = App::headless_for_test();
        let (record, cancelled, stood_down, _transferred) =
            prelaunch_record(2, ApplyMode::Automatic);
        app.update_handoff_prelaunch = Some(record);
        assert!(
            !app.defer_pending_update_handoff_teardown(
                crate::DeferredHandoffTeardown::QuitRequested
            )
        );
        assert!(
            app.update_handoff_prelaunch
                .as_ref()
                .expect("the record outlives the teardown")
                .stood_down,
            "the park gate shuts: nothing may park onto a cancelled attempt"
        );
        assert_eq!(
            stood_down
                .try_recv()
                .expect("a typed stand-down reaches the worker")
                .outcome,
            crate::UpdateHandoffOutcome::ActivityRevoked
        );
        assert!(
            cancelled.try_recv().is_err(),
            "the typed channel carried it; the raw poke is only the fallback"
        );
        app.pending_update_handoff = Some(parked_record(2, ApplyMode::Automatic));
        assert!(
            app.defer_pending_update_handoff_teardown(
                crate::DeferredHandoffTeardown::QuitRequested
            )
        );
    }

    /// The main thread's typed stand-down reaches the worker once; a second
    /// stand-down for the same attempt is a no-op, and a worker that is past
    /// its hold (channel full or gone) is reached through the cancel poke.
    #[test]
    fn a_stand_down_is_typed_once_and_falls_back_to_the_cancel_poke() {
        let mut app = App::headless_for_test();
        let (record, cancelled, stood_down, _transferred) =
            prelaunch_record(6, ApplyMode::Automatic);
        app.update_handoff_prelaunch = Some(record);
        app.stand_down_prelaunched_successor(super::HandoffStandDown {
            outcome: crate::UpdateHandoffOutcome::ActivityRevoked,
            detail: "first".to_string(),
        });
        app.stand_down_prelaunched_successor(super::HandoffStandDown {
            outcome: crate::UpdateHandoffOutcome::TimedOut,
            detail: "second".to_string(),
        });
        let received = stood_down
            .try_recv()
            .expect("the first stand-down is delivered");
        assert_eq!(received.detail, "first");
        assert!(stood_down.try_recv().is_err(), "the second is a no-op");
        assert!(cancelled.try_recv().is_err());
        // A full channel (the worker has not drained the first) falls back to
        // the cancel poke for a fresh attempt.
        let (record, cancelled, stood_down, _transferred) =
            prelaunch_record(7, ApplyMode::Automatic);
        app.update_handoff_prelaunch = Some(record);
        app.update_handoff_prelaunch
            .as_ref()
            .expect("record")
            .stand_down
            .try_send(super::HandoffStandDown {
                outcome: crate::UpdateHandoffOutcome::Rejected,
                detail: "occupying the slot".to_string(),
            })
            .expect("the slot was free");
        app.stand_down_prelaunched_successor(super::HandoffStandDown {
            outcome: crate::UpdateHandoffOutcome::ActivityRevoked,
            detail: "cannot be delivered".to_string(),
        });
        assert!(
            cancelled.try_recv().is_ok(),
            "the cancel poke is the fallback"
        );
        drop(stood_down);
    }

    /// The park cue for the wrong attempt is ignored; the right one records the
    /// dial and runs the gate — which, with no session left to park, stands the
    /// attempt down as activity-revoked rather than parking nothing.
    #[test]
    fn the_park_cue_binds_to_its_attempt_and_an_empty_pool_stands_down() {
        let mut app = App::headless_for_test();
        app.pool.sessions.clear();
        let (record, _cancelled, stood_down, _transferred) =
            prelaunch_record(11, ApplyMode::Immediate);
        app.update_handoff_prelaunch = Some(record);
        app.on_update_handoff_awaiting_park(10, Some(4242));
        assert!(
            app.update_handoff_prelaunch
                .as_ref()
                .is_some_and(|prelaunch| prelaunch.dialled.is_none()),
            "a cue for another attempt records no dial"
        );
        app.on_update_handoff_awaiting_park(11, Some(4242));
        let prelaunch = app.update_handoff_prelaunch.as_ref().expect("record");
        assert_eq!(
            prelaunch.dialled.and_then(|dialled| dialled.pid),
            Some(4242)
        );
        assert!(prelaunch.stood_down, "nothing to park is a stand-down");
        let received = stood_down.try_recv().expect("typed stand-down");
        assert_eq!(
            received.outcome,
            crate::UpdateHandoffOutcome::ActivityRevoked
        );
    }

    /// The headless fixture's session has no real master: the park half's
    /// descriptor duplicate fails, the readers it parked are rolled back, and
    /// the held successor is stood down as a PHYSICAL preparation failure —
    /// never left holding a claim for a park that cannot complete.
    #[test]
    fn a_park_half_failure_stands_the_held_successor_down_as_a_preparation_failure() {
        let mut app = App::headless_for_test();
        let (record, _cancelled, stood_down, transferred) =
            prelaunch_record(12, ApplyMode::Immediate);
        app.update_handoff_prelaunch = Some(record);
        app.on_update_handoff_awaiting_park(12, Some(4242));
        let prelaunch = app.update_handoff_prelaunch.as_ref().expect("record");
        assert!(prelaunch.stood_down);
        assert!(app.pending_update_handoff.is_none(), "nothing stays parked");
        assert!(transferred.try_recv().is_err(), "no capture was sent");
        let received = stood_down.try_recv().expect("typed stand-down");
        assert_eq!(
            received.outcome,
            crate::UpdateHandoffOutcome::PreparationFailed
        );
    }

    /// The post-park proof deadline ladder: short on the automatic lane (the
    /// slow parts already happened with readers live), wider after a physical
    /// failure of the same bytes, widest for an explicit apply.
    #[test]
    fn the_post_park_proof_deadline_widens_on_retries_and_for_an_explicit_apply() {
        if std::env::var_os("ATERM_HANDOFF_PROOF_TIMEOUT_MS").is_some() {
            return;
        }
        let park_at = std::time::Instant::now();
        let budget = |mode, prior| {
            super::handoff_proof_deadline(mode, prior, park_at).saturating_duration_since(park_at)
        };
        assert_eq!(
            budget(ApplyMode::Automatic, 0),
            std::time::Duration::from_secs(3)
        );
        assert_eq!(
            budget(ApplyMode::AutomaticPastGrace, 1),
            std::time::Duration::from_secs(6)
        );
        assert_eq!(
            budget(ApplyMode::Immediate, 0),
            std::time::Duration::from_secs(30)
        );
        assert_eq!(
            budget(ApplyMode::CleanQuit, 2),
            std::time::Duration::from_secs(30)
        );
        assert!(matches!(
            crate::update_handoff_wake_class(&crate::Wake::UpdateHandoffAwaitingPark {
                attempt_id: 1,
                dialer_pid: None
            }),
            crate::UpdateHandoffEventClass::Exempt
        ));
        assert!(matches!(
            crate::update_handoff_wake_class(&crate::Wake::UpdateHandoffStandingDown {
                attempt_id: 1
            }),
            crate::UpdateHandoffEventClass::Exempt
        ));
    }
}

/// THE PARK GATE, enumerated. The late park (2026-09-19) made "start the
/// update" and "freeze the terminal" two decisions; this is the second one, and
/// every rule in it is the ladder's rule for the live phase plus the one
/// correctness fact (unconsumed master bytes) the ladder never relaxes.
#[cfg(all(test, unix))]
mod park_gate_tests {
    use super::{
        PRELAUNCH_HOLD_MAX, ParkGate, ParkGateFacts, prelaunch_hold_cap, prelaunch_park_admitted,
    };
    use crate::native_update_auto_intent::{ActivityFacts, ApplyPhase, automatic_park_refusal};
    use crate::native_updater_service::ApplyMode;

    const MODES: [ApplyMode; 4] = [
        ApplyMode::Automatic,
        ApplyMode::AutomaticPastGrace,
        ApplyMode::Immediate,
        ApplyMode::CleanQuit,
    ];
    const PHASES: [ApplyPhase; 4] = [
        ApplyPhase::PreferIdle,
        ApplyPhase::PreferOutputGap,
        ApplyPhase::KeysOnly,
        ApplyPhase::Land,
    ];

    /// A machine that is idle in every sense: every mode parks on these.
    fn calm(mode: ApplyMode, phase: ApplyPhase) -> ParkGateFacts {
        ParkGateFacts {
            mode,
            phase,
            activity: ActivityFacts {
                quiet: true,
                hands_off_keys: true,
                output_quiet: true,
                focused: true,
            },
            masters_quiet: true,
            held_for: std::time::Duration::ZERO,
        }
    }

    fn facts_from_bits(mode: ApplyMode, phase: ApplyPhase, bits: u32) -> ParkGateFacts {
        ParkGateFacts {
            mode,
            phase,
            activity: ActivityFacts {
                quiet: bits & 1 != 0,
                hands_off_keys: bits & 2 != 0,
                output_quiet: bits & 4 != 0,
                focused: bits & 8 != 0,
            },
            masters_quiet: bits & 16 != 0,
            held_for: std::time::Duration::ZERO,
        }
    }

    fn gate(facts: ParkGateFacts) -> ParkGate {
        prelaunch_park_admitted(facts, prelaunch_hold_cap(facts.mode))
    }

    /// EVERY COMBINATION of the five boolean facts, for every mode and every
    /// phase, against the rule written out independently here. A truth table
    /// rather than a handful of cases, because the failure this guards is one
    /// arm quietly admitting a park the ladder would have refused — or refusing
    /// one it owes.
    #[test]
    fn the_gate_admits_exactly_what_the_ladder_owes() {
        for mode in MODES {
            for phase in PHASES {
                for bits in 0..32u32 {
                    let facts = facts_from_bits(mode, phase, bits);
                    let a = facts.activity;
                    let expected = if !mode.is_automatic() {
                        // The person asked and is waiting for it.
                        true
                    } else {
                        facts.masters_quiet
                            && match phase {
                                ApplyPhase::PreferIdle => a.hands_off_keys && a.quiet,
                                ApplyPhase::PreferOutputGap => {
                                    a.hands_off_keys && !(a.focused && !a.output_quiet)
                                }
                                ApplyPhase::KeysOnly => a.hands_off_keys,
                                ApplyPhase::Land => true,
                            }
                    };
                    assert_eq!(
                        gate(facts) == ParkGate::Park,
                        expected,
                        "{mode:?} {phase:?} with {facts:?} disagreed with the rule"
                    );
                    // And the park's refusal is the ENTRY's refusal, word for
                    // word: one predicate, two instants.
                    if mode.is_automatic() && facts.masters_quiet {
                        let entry = automatic_park_refusal(phase, a);
                        match gate(facts) {
                            ParkGate::Park => assert_eq!(entry, None),
                            ParkGate::Wait(reason) => assert_eq!(entry, Some(reason)),
                            ParkGate::StandDown(_) => unreachable!("held_for is zero"),
                        }
                    }
                }
            }
        }
    }

    /// The typing gap holds every automatic phase but the last: a keystroke
    /// inside it means fingers are on the keys. `Land` is the bound that keeps
    /// the ladder from being a refusal that can repeat forever; the deferred
    /// input queue is what makes overriding the gap there safe.
    #[test]
    fn a_keystroke_inside_the_typing_gap_holds_every_automatic_phase_but_land() {
        for mode in [ApplyMode::Automatic, ApplyMode::AutomaticPastGrace] {
            for phase in [
                ApplyPhase::PreferIdle,
                ApplyPhase::PreferOutputGap,
                ApplyPhase::KeysOnly,
            ] {
                let facts = ParkGateFacts {
                    activity: ActivityFacts {
                        hands_off_keys: false,
                        ..calm(mode, phase).activity
                    },
                    ..calm(mode, phase)
                };
                assert!(
                    matches!(gate(facts), ParkGate::Wait(_)),
                    "{mode:?} {phase:?}"
                );
            }
            let landing = ParkGateFacts {
                activity: ActivityFacts {
                    hands_off_keys: false,
                    quiet: false,
                    output_quiet: false,
                    focused: true,
                },
                ..calm(mode, ApplyPhase::Land)
            };
            assert_eq!(gate(landing), ParkGate::Park, "{mode:?} lands at the bound");
        }
        for mode in [ApplyMode::Immediate, ApplyMode::CleanQuit] {
            let facts = ParkGateFacts {
                activity: ActivityFacts {
                    hands_off_keys: false,
                    ..calm(mode, ApplyPhase::PreferIdle).activity
                },
                ..calm(mode, ApplyPhase::PreferIdle)
            };
            assert_eq!(
                gate(facts),
                ParkGate::Park,
                "{mode:?}: the person asked for this pause"
            );
        }
    }

    /// THE MACHINE THIS LADDER EXISTS FOR: an agent streaming into a focused
    /// pane, never quiet, never pausing its output. It waits through the first
    /// two phases (the owner's 2026-09-18 ruling, kept as a bounded preference)
    /// and lands in the third, with the user's hands off the keys.
    #[test]
    fn a_focused_never_quiet_stream_lands_in_the_keys_only_phase() {
        let streaming = |phase| ParkGateFacts {
            activity: ActivityFacts {
                quiet: false,
                hands_off_keys: true,
                output_quiet: false,
                focused: true,
            },
            ..calm(ApplyMode::AutomaticPastGrace, phase)
        };
        assert!(matches!(
            gate(streaming(ApplyPhase::PreferIdle)),
            ParkGate::Wait(_)
        ));
        assert_eq!(
            gate(streaming(ApplyPhase::PreferOutputGap)),
            ParkGate::Wait("terminal output is still streaming")
        );
        assert_eq!(gate(streaming(ApplyPhase::KeysOnly)), ParkGate::Park);
        assert_eq!(gate(streaming(ApplyPhase::Land)), ParkGate::Park);
        // With no aterm window focused the stall is invisible: the output-gap
        // phase lands too.
        assert_eq!(
            gate(ParkGateFacts {
                activity: ActivityFacts {
                    focused: false,
                    ..streaming(ApplyPhase::PreferOutputGap).activity
                },
                ..streaming(ApplyPhase::PreferOutputGap)
            }),
            ParkGate::Park,
            "with the user in another app the stall is invisible"
        );
    }

    /// MONOTONE IN `held_for`, which is what bounds the hold: past the cap every
    /// automatic mode stands down whatever else is true, and nothing later can
    /// make it admit. The explicit modes have no cap because they never wait.
    #[test]
    fn the_hold_cap_stands_down_and_never_admits_again() {
        for mode in [ApplyMode::Automatic, ApplyMode::AutomaticPastGrace] {
            assert_eq!(prelaunch_hold_cap(mode), Some(PRELAUNCH_HOLD_MAX));
            for phase in PHASES {
                for bits in 0..32u32 {
                    let facts = ParkGateFacts {
                        held_for: PRELAUNCH_HOLD_MAX,
                        ..facts_from_bits(mode, phase, bits)
                    };
                    assert!(
                        matches!(gate(facts), ParkGate::StandDown(_)),
                        "{mode:?} past the cap must stand down: {facts:?}"
                    );
                    assert!(
                        matches!(
                            gate(ParkGateFacts {
                                held_for: PRELAUNCH_HOLD_MAX * 2,
                                ..facts
                            }),
                            ParkGate::StandDown(_)
                        ),
                        "and stays stood down"
                    );
                }
            }
            // Just inside the cap a calm machine still parks: the cap bounds the
            // wait, it does not shorten it.
            assert_eq!(
                gate(ParkGateFacts {
                    held_for: PRELAUNCH_HOLD_MAX - std::time::Duration::from_millis(1),
                    ..calm(mode, ApplyPhase::PreferIdle)
                }),
                ParkGate::Park
            );
        }
        for mode in [ApplyMode::Immediate, ApplyMode::CleanQuit] {
            assert_eq!(prelaunch_hold_cap(mode), None);
            assert_eq!(
                gate(ParkGateFacts {
                    held_for: PRELAUNCH_HOLD_MAX * 10,
                    ..calm(mode, ApplyPhase::PreferIdle)
                }),
                ParkGate::Park
            );
        }
    }

    /// WHAT A RE-PARK BUYS. A park that missed its 20 ms budget re-parks on the
    /// ladder's next rung with the successor still booted and holding, and the
    /// attempt climbs the WHOLE freeze ladder that way — 20, 80, 250 ms — before
    /// it gives the successor back. Past the last rung it stands down as a
    /// busy-machine fact, in the ACTIVITY lane: no physical budget, no latch.
    ///
    /// THE DEAD MUTANT (2026-09-21): the second miss used to stand down as
    /// `PreparationFailed`, which the shape classifier files as STRUCTURAL — a
    /// verdict about the bytes with a two-attempt lifetime — so two busy moments
    /// ten minutes apart converged the automatic lane to a deadline-less
    /// manual-only latch. Every rung of that sequence is pinned here: the
    /// disposition at every miss count, the outcome it stands down with, and the
    /// lane the shipping classifier puts that outcome in.
    #[test]
    fn a_park_miss_buys_the_ladders_next_rung_without_a_relaunch() {
        use super::{
            PRELAUNCH_MAX_PARK_MISSES, PRELAUNCH_REPARK_DELAY, ParkMissDisposition,
            handoff_freeze_budget, park_miss_disposition,
        };
        use crate::app_native::{HandoffFailureLane, PhysicalFailureShape};

        let rung = |misses: u8| handoff_freeze_budget(ApplyMode::Automatic, misses);
        assert_eq!(rung(0), std::time::Duration::from_millis(20));
        assert_eq!(rung(1), std::time::Duration::from_millis(80));
        assert_eq!(rung(2), std::time::Duration::from_millis(250));
        assert_eq!(
            PRELAUNCH_MAX_PARK_MISSES, 2,
            "the attempt climbs every rung itself; nothing is left for the ledger"
        );
        // Every miss short of the last rung re-parks one rung wider.
        for misses in 0..PRELAUNCH_MAX_PARK_MISSES {
            match park_miss_disposition(misses, "a PTY reader missed the deadline".into()) {
                ParkMissDisposition::Repark {
                    misses: next,
                    reason,
                } => {
                    assert_eq!(next, misses + 1, "the re-park is the next rung");
                    assert!(rung(next) > rung(misses), "and the next rung is wider");
                    assert_eq!(reason, "a PTY reader missed the deadline");
                }
                ParkMissDisposition::StandDown(stand_down) => panic!(
                    "miss {misses} of {PRELAUNCH_MAX_PARK_MISSES} must re-park, not stand \
                     down ({})",
                    stand_down.detail
                ),
            }
        }
        // The last rung missed: the successor is stood down as ACTIVITY, and the
        // shipping classifier keeps it out of every physical shape.
        let ParkMissDisposition::StandDown(stand_down) =
            park_miss_disposition(PRELAUNCH_MAX_PARK_MISSES, "capture exceeded 250 ms".into())
        else {
            panic!("past the last rung the attempt must stand down");
        };
        assert_eq!(
            stand_down.outcome,
            crate::UpdateHandoffOutcome::ActivityRevoked,
            "a stopwatch the parent missed is a fact about the machine's moment, never \
             about the bytes"
        );
        assert!(stand_down.detail.contains("capture exceeded 250 ms"));
        for mode in [ApplyMode::Automatic, ApplyMode::AutomaticPastGrace] {
            let lane = HandoffFailureLane::classify(
                mode,
                stand_down.outcome,
                crate::ChildDeathEvidence::Unobserved,
                false,
            );
            assert_eq!(lane, HandoffFailureLane::ActivityRevoked, "{mode:?}");
            assert_ne!(
                lane,
                HandoffFailureLane::Physical(PhysicalFailureShape::Structural),
                "{mode:?}: the two-attempt structural lifetime is the mutant"
            );
        }
        // A prior physical failure of the same bytes still compounds: an attempt
        // that already failed once starts one rung wider and saturates at 250 ms.
        assert_eq!(
            handoff_freeze_budget(ApplyMode::Automatic, 1 + 1),
            std::time::Duration::from_millis(250)
        );
        assert_eq!(
            handoff_freeze_budget(ApplyMode::Automatic, 1 + PRELAUNCH_MAX_PARK_MISSES),
            std::time::Duration::from_millis(250)
        );
        assert!(
            PRELAUNCH_REPARK_DELAY * u32::from(PRELAUNCH_MAX_PARK_MISSES) < PRELAUNCH_HOLD_MAX,
            "every re-park must fit inside the hold it is spending"
        );
    }

    /// Bytes waiting on a master that its reader has not taken hold every
    /// automatic lane IN EVERY PHASE, `Land` included: parking on top of them
    /// is how an attempt commits a screen digest the successor cannot
    /// reproduce. A correctness gate, not a comfort, so the ladder never
    /// relaxes it.
    #[test]
    fn unconsumed_master_output_holds_every_automatic_phase() {
        for mode in [ApplyMode::Automatic, ApplyMode::AutomaticPastGrace] {
            for phase in PHASES {
                assert!(
                    matches!(
                        gate(ParkGateFacts {
                            masters_quiet: false,
                            ..calm(mode, phase)
                        }),
                        ParkGate::Wait(_)
                    ),
                    "{mode:?} {phase:?}"
                );
            }
        }
    }
}

#[cfg(test)]
mod freeze_budget_tests {
    use super::handoff_freeze_budget;
    use crate::native_updater_service::ApplyMode;
    use std::time::Duration;

    /// The first automatic attempt keeps the imperceptible 20 ms; a physical failure of
    /// the same bytes buys a wider window, then the widest; an explicit apply gets the
    /// widest at once. A monotone ladder: more failures never buy LESS time.
    #[test]
    fn freeze_budget_widens_on_retries_and_for_an_explicit_apply() {
        assert_eq!(
            handoff_freeze_budget(ApplyMode::Automatic, 0),
            Duration::from_millis(20)
        );
        assert_eq!(
            handoff_freeze_budget(ApplyMode::AutomaticPastGrace, 0),
            Duration::from_millis(20)
        );
        assert_eq!(
            handoff_freeze_budget(ApplyMode::Automatic, 1),
            Duration::from_millis(80)
        );
        assert_eq!(
            handoff_freeze_budget(ApplyMode::Automatic, 2),
            Duration::from_millis(250)
        );
        assert_eq!(
            handoff_freeze_budget(ApplyMode::AutomaticPastGrace, 7),
            Duration::from_millis(250)
        );
        assert_eq!(
            handoff_freeze_budget(ApplyMode::Immediate, 0),
            Duration::from_millis(250)
        );
        assert_eq!(
            handoff_freeze_budget(ApplyMode::CleanQuit, 0),
            Duration::from_millis(250)
        );
        let mut last = Duration::ZERO;
        for prior in 0..=u8::MAX {
            let now = handoff_freeze_budget(ApplyMode::Automatic, prior);
            assert!(
                now >= last,
                "the ladder must be monotone: {prior} prior failures gave {now:?} after {last:?}"
            );
            last = now;
        }
    }
}

#[cfg(all(test, unix))]
mod capture_budget_reservation_tests {
    use super::{MAX_HANDOFF_CAPTURE_BUDGET_BYTES, optional_carry_fits};
    use crate::seamless::{
        admit_checkpoint_dimensions, checkpoint_capture_budget_bytes, mandatory_checkpoint_cells,
        max_handoff_aggregate_grid_cells, max_handoff_history_lines,
    };

    /// Walk a pool of `sessions` identical panes through the producer's
    /// reserve-then-history rule, charging the REAL admission seam and the REAL
    /// budgets `start_unix_update_handoff` charges.
    ///
    /// The per-session degrade rung is deliberately NOT replicated: if the
    /// reservation is right, nothing ever needs it, so `Ok` here is the stronger
    /// claim. `Err(index)` is the session the capture would have refused, which is
    /// the whole update.
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
            let Some(per_grid) =
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

    /// REGRESSION (the desk that could not update). Both capture budgets used to be
    /// handed out greedily in pool order: the first sessions each took a full 256
    /// lines of scrollback, and a later session then found nothing left for its
    /// MANDATORY visible screen. That is not degradable, so the capture failed and
    /// the seamless update did not apply — deterministically, on every retry, for
    /// anyone past a handful of panes (five at the reported geometry).
    ///
    /// Twelve, twenty-four and sixty-four panes, at the reported geometry and at a
    /// maximized one, must all be admitted in full. Carrying less scrollback is an
    /// allowed answer; refusing the update is not.
    #[test]
    fn a_heavy_pool_is_admitted_even_when_it_must_drop_history() {
        // 49x110 is the window from the field report. 60x200 is a maximized window
        // on a large display, where one session costs more than twice as much.
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

    /// The exact boundary the reservation establishes: a session is refused if and
    /// only if the POOL's mandatory visible+alt total genuinely does not fit. The
    /// count is derived from the constant, so raising the aggregate later moves the
    /// boundary instead of reddening this test.
    ///
    /// The `Err(fits)` is the load-bearing half. Under the old greedy rule the
    /// refusal landed on an EARLY session — one whose own screen fit perfectly well,
    /// but whose budget an earlier pane had already spent on optional scrollback.
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
}

#[cfg(all(test, unix))]
mod commit_layout_topology_tests {
    use super::commit_layout_topology;
    use crate::restore::{
        PaneLayout, RestoreManifest, RestoredSplitTree, RestoredTab, RestoredView, TabOrderEntry,
        TerminalLeafRestore, WindowLayout,
    };

    /// One window, one terminal tab, in the shape `capture_restore_manifest`
    /// really produces: the legacy `tabs` mirror and the canonical
    /// `restored_tabs` tree both carry the SAME live session's cwd/title, so a
    /// degraded read corrupts two places at once and a projection that missed
    /// either one would still reject the Commit.
    fn captured(position: Option<(i32, i32)>, cwd: Option<&str>, title: &str) -> RestoreManifest {
        RestoreManifest::new(vec![WindowLayout {
            rows: 40,
            cols: 120,
            active_tab: 0,
            outer_x: position.map(|(x, _)| x),
            outer_y: position.map(|(_, y)| y),
            maximized: None,
            tabs: vec![PaneLayout::Leaf {
                cwd: cwd.map(str::to_string),
                title: title.to_string(),
                focused: true,
                local_id: Some(7),
            }],
            native_tabs: Vec::new(),
            tab_order: vec![TabOrderEntry::Terminal { index: 0 }],
            active_item: Some(0),
            restored_tabs: vec![RestoredTab {
                root: RestoredSplitTree::leaf(RestoredView::Terminal(TerminalLeafRestore {
                    cwd: cwd.map(str::to_string),
                    title: title.to_string(),
                    profile: None,
                    local_id: Some(7),
                    user_title: None,
                    description: None,
                    icon: None,
                    role: None,
                    attention: None,
                    identity: None,
                })),
                focused_path: Vec::new(),
                zoomed: false,
            }],
        }])
    }

    #[test]
    fn dragging_the_window_while_the_successor_boots_is_not_a_topology_change() {
        let committed = captured(Some((120, 80)), Some("/work"), "zsh");
        let dragged = captured(Some((640, 310)), Some("/work"), "zsh");
        assert_ne!(
            committed, dragged,
            "PRECONDITION: the derived PartialEq must still see the raw position difference — \
             that is exactly what used to reject the Commit"
        );
        assert_eq!(
            commit_layout_topology(&committed),
            commit_layout_topology(&dragged),
            "a drag during the child's boot must not be reported as changed topology; \
             `WindowEvent::Moved` is classified Tolerated for this very reason"
        );
    }

    #[test]
    fn a_contended_session_lock_that_empties_cwd_and_title_is_not_a_topology_change() {
        let committed = captured(Some((120, 80)), Some("/work"), "zsh");
        // EXACTLY what `restore_session_meta` yields on a `WouldBlock` try_lock
        // (a scrollback drain holding the `term` mutex): no cwd, empty title,
        // every structural field untouched.
        let degraded = captured(Some((120, 80)), None, "");
        assert_ne!(
            committed, degraded,
            "PRECONDITION: a degraded capture is not equal to the committed one"
        );
        assert_eq!(
            commit_layout_topology(&committed),
            commit_layout_topology(&degraded),
            "a degraded metadata read must never masquerade as a changed layout — it made the \
             rejection nondeterministic for a session that did not change"
        );
    }

    #[test]
    fn a_real_topology_change_still_rejects_the_commit() {
        let committed = captured(Some((120, 80)), Some("/work"), "zsh");

        let mut resized = captured(Some((120, 80)), Some("/work"), "zsh");
        resized.windows[0].cols = 200;
        assert_ne!(
            commit_layout_topology(&committed),
            commit_layout_topology(&resized),
            "the grid the proof committed to is structural and must still reject"
        );

        let mut readopted = captured(Some((120, 80)), Some("/work"), "zsh");
        readopted.windows[0].restored_tabs[0].root =
            RestoredSplitTree::leaf(RestoredView::Terminal(TerminalLeafRestore {
                cwd: Some("/work".to_string()),
                title: "zsh".to_string(),
                profile: None,
                local_id: Some(9),
                user_title: None,
                description: None,
                icon: None,
                role: None,
                attention: None,
                identity: None,
            }));
        assert_ne!(
            commit_layout_topology(&committed),
            commit_layout_topology(&readopted),
            "`local_id` is the layout↔live-fd bridge the child adopts by, not degradable \
             metadata, so it must survive the projection"
        );

        let mut relabelled = captured(Some((120, 80)), Some("/work"), "zsh");
        let RestoredSplitTree::Leaf {
            view: RestoredView::Terminal(terminal),
        } = &mut relabelled.windows[0].restored_tabs[0].root
        else {
            unreachable!("the fixture builds a single terminal leaf");
        };
        terminal.user_title = Some("deploy".to_string());
        assert_ne!(
            commit_layout_topology(&committed),
            commit_layout_topology(&relabelled),
            "USER metadata is captured under a BLOCKING lock, so it cannot degrade and must \
             not be swept up by the cwd/title normalization"
        );

        let mut extra_tab = captured(Some((120, 80)), Some("/work"), "zsh");
        let tab = extra_tab.windows[0].restored_tabs[0].clone();
        extra_tab.windows[0].restored_tabs.push(tab);
        assert_ne!(
            commit_layout_topology(&committed),
            commit_layout_topology(&extra_tab),
            "a tab that appeared during async preparation is what this gate exists to catch"
        );
    }

    /// `captured`'s single terminal leaf, edited in place.
    fn with_leaf(
        mut layout: RestoreManifest,
        edit: impl FnOnce(&mut TerminalLeafRestore),
    ) -> RestoreManifest {
        let RestoredSplitTree::Leaf {
            view: RestoredView::Terminal(terminal),
        } = &mut layout.windows[0].restored_tabs[0].root
        else {
            unreachable!("the fixture builds a single terminal leaf");
        };
        edit(terminal);
        layout
    }

    fn stamp_all_five(terminal: &mut TerminalLeafRestore) {
        terminal.user_title = Some("fable driver".to_string());
        terminal.description = Some("drives the satcomp worker".to_string());
        terminal.icon = Some("🦊".to_string());
        terminal.role = Some("agent:claude-driver-fable".to_string());
        terminal.attention = Some("waiting on a human review".to_string());
    }

    /// The invariant the comment in `commit_layout_topology` states, for ALL
    /// FIVE user fields rather than the title alone: each stays in the Commit
    /// comparison (so a `meta set` the parent answers before its Commit-time
    /// re-capture is a change that Commit sees), the cwd/title/position
    /// normalization never touches them, and the sidecar the child parses
    /// carries exactly what was compared — which is what lets the successor
    /// put them back on the adopted sessions.
    #[test]
    fn every_user_metadata_field_is_compared_and_rides_the_sidecar() {
        type Clear = fn(&mut TerminalLeafRestore);
        let stamped = with_leaf(
            captured(Some((120, 80)), Some("/work"), "zsh"),
            stamp_all_five,
        );

        let clears: [(&str, Clear); 5] = [
            ("user_title", |terminal| terminal.user_title = None),
            ("description", |terminal| terminal.description = None),
            ("icon", |terminal| terminal.icon = None),
            ("role", |terminal| terminal.role = None),
            ("attention", |terminal| terminal.attention = None),
        ];
        for (field, clear) in clears {
            assert_ne!(
                commit_layout_topology(&stamped),
                commit_layout_topology(&with_leaf(stamped.clone(), clear)),
                "{field} is read under a blocking lock and must stay in the comparison"
            );
        }

        let dragged_and_degraded = with_leaf(captured(Some((640, 310)), None, ""), stamp_all_five);
        assert_eq!(
            commit_layout_topology(&stamped),
            commit_layout_topology(&dragged_and_degraded),
            "the degradable-field normalization must not reach the user fields"
        );

        let wire = stamped.to_toml().expect("the parent serializes its layout");
        let parsed = RestoreManifest::from_toml(&wire)
            .filter(|layout| layout.covers_exact_seamless_ids(&[7]))
            .expect("the child accepts the sidecar");
        assert_eq!(
            commit_layout_topology(&parsed),
            commit_layout_topology(&stamped),
            "the child's layout carries every compared user field"
        );
    }
}

#[cfg(all(test, unix))]
mod pinnedness_tests {
    use super::candidate_is_our_child;

    /// THE PRECONDITION FOR A GROUP KILL, and the reason the emergency reaper may
    /// no longer take it on trust.
    ///
    /// `kill(-pid)` is sound only while the number is PINNED to our candidate. A
    /// fork child pins it — an unreaped child owns its pid until it is waited on.
    /// A launchd-owned successor does not: it is nobody's child, launchd may reap
    /// it at any moment, and the freed number can then name a stranger whose
    /// process GROUP we would be signalling.
    ///
    /// So the reaper asks the kernel. This pins the two answers it relies on: a
    /// live child of ours reads as pinned, and a process that is not our child
    /// (here, this very process, which is certainly alive) does not — the latter
    /// standing in for the launchd-owned successor, which no test can conjure.
    #[test]
    fn only_a_child_of_ours_pins_its_pid() {
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("spawn a child to own a pid");
        let pid = libc::pid_t::try_from(child.id()).expect("pid fits");
        assert!(
            candidate_is_our_child(pid),
            "a live child of ours pins its number, so -pid still names its group"
        );

        // Not our child: alive, real, and never ours to sweep.
        let own = libc::pid_t::try_from(std::process::id()).expect("pid fits");
        assert!(
            !candidate_is_our_child(own),
            "a process that is not our child must never license a group kill"
        );
        assert!(!candidate_is_our_child(1), "launchd is not ours either");

        // WNOWAIT is load-bearing: the probe must not consume the child, or the
        // reaper's own wait would block forever on a pid it had already reaped.
        child.kill().expect("kill the probe child");
        let status = child.wait().expect("the probe left it waitable");
        assert!(!status.success(), "it was killed, not exited cleanly");
    }
}

#[cfg(all(test, unix))]
mod candidate_death_evidence_tests {
    use super::{
        HandoffCandidate, HandoffCandidateHandle, HandoffCandidateProbe, handoff_child_death,
        observe_candidate_death, probe_handoff_candidate,
    };
    use crate::ChildDeathEvidence as Death;
    use std::os::unix::process::ExitStatusExt as _;

    /// One `wait(2)` status in the encoding `ExitStatus` carries, so the unit
    /// tests below can state a signal death and a clean exit without a real corpse.
    fn signalled(signal: i32) -> std::process::ExitStatus {
        std::process::ExitStatus::from_raw(signal)
    }

    /// Spin until `probe` agrees, or fail. Bounded HERE ONLY, so a broken probe
    /// fails a test instead of hanging the suite; nothing in production polls this.
    fn wait_for_probe(pid: libc::pid_t, want: HandoffCandidateProbe) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while probe_handoff_candidate(pid) != want {
            assert!(
                std::time::Instant::now() < deadline,
                "the kernel must reach {want:?} for {pid}"
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    /// THE THREE ANSWERS THE PRE-KILL PROBE HAS TO SEPARATE, against real
    /// processes, because the whole value of the probe is that it is the KERNEL
    /// talking and not us.
    ///
    /// `Running` versus `Exited` is the load-bearing pair: it is what tells the
    /// reject path whether a bare `SIGKILL` in the status it collects belongs to
    /// the candidate or to the signal it is about to send.
    #[test]
    fn the_pre_kill_probe_separates_a_running_candidate_from_a_dead_one() {
        let mut alive = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("spawn a live candidate");
        let alive_pid = libc::pid_t::try_from(alive.id()).expect("pid fits");
        assert_eq!(
            probe_handoff_candidate(alive_pid),
            HandoffCandidateProbe::Running,
            "a child that has not exited has nothing to report yet"
        );

        let mut dead = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("exit 3")
            .spawn()
            .expect("spawn a candidate that exits at once");
        let dead_pid = libc::pid_t::try_from(dead.id()).expect("pid fits");
        wait_for_probe(dead_pid, HandoffCandidateProbe::Exited);

        assert_eq!(
            probe_handoff_candidate(1),
            HandoffCandidateProbe::NotOurChild,
            "launchd is nobody's child of ours, and ECHILD is evidence of nothing"
        );

        // WNOWAIT IS LOAD-BEARING, and this is the assertion that proves it: the
        // probe ran against an exited child and the status is STILL collectable,
        // which is exactly what `kill_and_reap_handoff_child` does next.
        assert_eq!(
            dead.wait().expect("the probe consumed nothing").code(),
            Some(3),
            "the probe must leave the candidate's own status intact"
        );
        alive.kill().expect("kill the live fixture");
        alive.wait().expect("reap the live fixture");
    }

    /// WHICH STATUS BITS CAN POSSIBLY BE OURS, stated as the whole of the
    /// attribution rule, because this process sends exactly one signal ever.
    ///
    /// THE BUG THIS PINS: an earlier draft made `died_before_we_signalled` a
    /// precondition on reading the status AT ALL. That threw away bits that
    /// provably could not be our SIGKILL — an exit code above all — so this tree's
    /// commonest refusal (a deliberate `exit(0)`) degraded to `Unobserved`
    /// whenever the parent lost a race it deliberately refuses to wait out, and
    /// the refusal the classification most wants to catch became the one it could
    /// least often see.
    #[test]
    fn only_a_bare_sigkill_needs_proof_that_the_candidate_died_first() {
        assert_eq!(
            handoff_child_death(false, Some(signalled(libc::SIGKILL))),
            Death::Unobserved,
            "the candidate was still alive when the channel closed, so this \
             SIGKILL can only be ours"
        );
        assert_eq!(
            handoff_child_death(true, Some(signalled(libc::SIGKILL))),
            Death::Signalled {
                signal: libc::SIGKILL
            },
            "…and once the candidate is proven to have died first, the same \
             status IS its own death — the field case"
        );
        assert_eq!(
            handoff_child_death(false, Some(std::process::ExitStatus::from_raw(0))),
            Death::Exited { code: 0 },
            "SIGKILL is uncatchable and never yields WIFEXITED, so an exit code \
             cannot be ours however the race went — and `0` is the refusal"
        );
        assert_eq!(
            handoff_child_death(false, Some(signalled(libc::SIGSEGV))),
            Death::Signalled {
                signal: libc::SIGSEGV
            },
            "nothing in this file sends SIGSEGV either: a fault is always the \
             image's own"
        );
        assert_eq!(
            handoff_child_death(true, None),
            Death::Unobserved,
            "no status is no evidence, however certain the death"
        );
    }

    /// THE PRODUCTION ASSEMBLY, against a REAL child, through the same function
    /// the reject path calls — probe, kill, reap, classify.
    ///
    /// Every other test here hands [`handoff_child_death`] three facts it made up.
    /// This one makes the kernel produce them, which is the only way the ORDER of
    /// the steps is under test at all: swap the pre-kill probe for anything that
    /// answers "already dead" too eagerly and the `exit(0)` below turns into
    /// `Signalled { SIGKILL }` — the machine — with every hand-built assertion in
    /// this module still green.
    #[test]
    fn a_candidate_that_exits_on_its_own_is_read_as_its_own_exit_not_our_kill() {
        // A child that ends itself with the tree's commonest refusal code, then a
        // wait for the kernel to agree it is dead — which is the state the reject
        // path finds a refusing successor in.
        let child = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("exit 0")
            .spawn()
            .expect("spawn a refusing candidate");
        let candidate = HandoffCandidate::of_unreaped_child(&child);
        let pid = libc::pid_t::try_from(candidate.pid).expect("pid fits");
        let mut handle = HandoffCandidateHandle::Forked(child);
        wait_for_probe(pid, HandoffCandidateProbe::Exited);

        let (warrant, death) = observe_candidate_death(
            crate::UpdateHandoffOutcome::ChildDied,
            candidate,
            &mut handle,
        );
        assert_eq!(
            warrant,
            super::HandoffRollbackWarrant::Reaped,
            "our own fork child is still reaped by the strongest authority"
        );
        assert_eq!(
            death,
            Death::Exited { code: 0 },
            "THE REFUSAL: the candidate reached an `exit` instruction before this \
             process signalled anything, and the SIGKILL sent afterwards must not \
             overwrite that"
        );
    }

    /// THE OTHER HALF OF THE SAME ASSEMBLY: a candidate somebody ELSE killed.
    ///
    /// This is the field shape — macOS jetsam reclaiming memory from a process the
    /// machine was starving — reproduced with the one signal that is
    /// indistinguishable from the reject path's own, so the pre-kill probe is the
    /// only thing that can tell them apart.
    #[test]
    fn a_candidate_the_machine_killed_is_read_as_the_machine_s_kill() {
        let child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("spawn a candidate to starve");
        let candidate = HandoffCandidate::of_unreaped_child(&child);
        let pid = libc::pid_t::try_from(candidate.pid).expect("pid fits");
        let mut handle = HandoffCandidateHandle::Forked(child);
        // SOMEBODY ELSE'S SIGKILL, delivered before the reject path runs — exactly
        // what a machine out of memory does, and exactly what the parent's own
        // rejection would look like if the order of the steps were wrong.
        // SAFETY: `pid` names our own unreaped child, so the number is pinned.
        assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
        wait_for_probe(pid, HandoffCandidateProbe::Exited);

        let (_, death) = observe_candidate_death(
            crate::UpdateHandoffOutcome::ChildDied,
            candidate,
            &mut handle,
        );
        assert_eq!(
            death,
            Death::Signalled {
                signal: libc::SIGKILL
            },
            "THE FIELD CASE: the candidate was already dead when the reject path \
             looked, so the SIGKILL in its status is the machine's and the lane \
             must retry"
        );
    }

    /// AND THE CANDIDATE THIS PROCESS ENDED ITSELF, which must claim nothing.
    ///
    /// A live candidate, rejected: the reject path SIGKILLs it and reaps a status
    /// that says `SIGKILL` — its own signal, wearing the candidate's clothes.
    /// Reading that as evidence would file every deliberate rejection as "the
    /// machine reclaimed it" and hand a broken successor the transient lane's nine
    /// attempts.
    #[test]
    fn a_kill_this_process_sends_is_never_read_as_the_candidate_s_own_death() {
        let child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("spawn a live candidate");
        let candidate = HandoffCandidate::of_unreaped_child(&child);
        let mut handle = HandoffCandidateHandle::Forked(child);

        let (_, death) = observe_candidate_death(
            crate::UpdateHandoffOutcome::ChildDied,
            candidate,
            &mut handle,
        );
        assert_eq!(
            death,
            Death::Unobserved,
            "it was alive when we looked, so the SIGKILL we then sent is ours and \
             claims nothing"
        );
    }

    /// EVERY OTHER OUTCOME DESCRIBES A CANDIDATE THIS PROCESS DECIDED TO END, so
    /// the assembly must not even ask — and must not spend a probe on it.
    ///
    /// The child here exits on its own with a code the classifier would happily
    /// call STRUCTURAL; the outcome is what makes that irrelevant.
    #[test]
    fn a_non_child_died_outcome_gathers_no_evidence_at_all() {
        let child = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("exit 74")
            .spawn()
            .expect("spawn a candidate that exits at once");
        let candidate = HandoffCandidate::of_unreaped_child(&child);
        let pid = libc::pid_t::try_from(candidate.pid).expect("pid fits");
        let mut handle = HandoffCandidateHandle::Forked(child);
        wait_for_probe(pid, HandoffCandidateProbe::Exited);

        let (warrant, death) = observe_candidate_death(
            crate::UpdateHandoffOutcome::TimedOut,
            candidate,
            &mut handle,
        );
        assert_eq!(
            warrant,
            super::HandoffRollbackWarrant::Reaped,
            "the reap is unconditional; only the evidence is not"
        );
        assert_eq!(
            death,
            Death::Unobserved,
            "a candidate WE ended has no death of its own to describe, whatever \
             its status happens to say"
        );
    }
}

#[cfg(all(test, unix))]
mod trial_launch_forgiveness_tests {
    use super::forgives_the_counted_trial_launch;
    use crate::ChildDeathEvidence as Death;

    /// The shape of a real apply: a strictly newer authorized target.
    const CURRENT: u64 = 1_787_699_398;
    const TARGET: u64 = 1_787_699_399;

    /// Forgive, or keep the count, for a real apply.
    fn forgives(death: Death) -> bool {
        forgives_the_counted_trial_launch(death, CURRENT, TARGET)
    }

    /// THE DEFECT THIS TABLE EXISTS TO STOP, stated before the table: a `ChildDied`
    /// whose launch stays counted spends the SAME counter the boot sentinel
    /// reverts on. Retry it more times than `MAX_BOOT_ATTEMPTS` and the automatic
    /// lane reverts the bundle and marks the build failed — for bytes that never
    /// failed. `ChildDied` used to be exempted from forgiveness wholesale, which
    /// was safe only while it was also capped at two attempts; the moment it could
    /// be retried six or nine times, the exemption became the bug.
    ///
    /// So forgiveness follows the SHAPE, and only the shape that converges under
    /// `MAX_BOOT_ATTEMPTS` may keep its count.
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

    /// AND THE REAL-APPLY GUARD, which is part of the same decision: with no
    /// strictly newer authorized target nothing armed a sentinel, so there is no
    /// counted launch to give back and the answer is no whatever the death was.
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

    /// THE INVARIANT THAT MAKES THE TABLE ABOVE SAFE, checked against the real
    /// constant rather than restated. Mirrors the compile-time assert beside
    /// `STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS`, so a reader of this module sees WHY
    /// only the structural shape may keep its count.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_one_shape_that_keeps_its_count_converges_before_the_sentinel_reverts() {
        assert!(
            u32::from(crate::app_native::STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS)
                < aterm_update::MAX_BOOT_ATTEMPTS,
            "a shape whose launches stay counted must be finished with the \
             artifact before the boot sentinel would revert it; {} vs {}",
            crate::app_native::STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS,
            aterm_update::MAX_BOOT_ATTEMPTS
        );
        for shape in [
            crate::app_native::PhysicalFailureShape::Transient,
            crate::app_native::PhysicalFailureShape::Unexplained,
        ] {
            assert!(
                u32::from(shape.lifetime_attempts()) >= aterm_update::MAX_BOOT_ATTEMPTS,
                "{shape:?} is retried far enough to reach the revert threshold, \
                 which is precisely why its deaths must forgive"
            );
        }
    }
}

/// THE NON-PARENT WITNESS, against real processes reparented to launchd — the
/// exact shape of a LaunchServices-launched successor, which is what the shipping
/// macOS lane hands this file.
#[cfg(all(test, target_os = "macos"))]
mod candidate_exit_watch_tests {
    use super::CandidateExitWatch;

    /// THE ORPHAN'S GATE: the write end of a pipe the orphan blocks on before it
    /// runs the first command of its script. Nothing the orphan does — exiting
    /// least of all — can happen before the test releases it, which is what lets
    /// a caller attach its [`CandidateExitWatch`] to a process that is provably
    /// still there.
    ///
    /// WHY THE FIXTURE NEEDS ONE (2026-09-18). It used to be `sleep 0.3; exit 7`:
    /// the orphan started dying 300 ms after its fork, and the test thread had to
    /// read the pid, reap the middle, `kqueue()` and `kevent(EV_ADD)` inside that
    /// window. Under the full parallel suite that is a race the test cannot win
    /// by right — a stalled thread comes back to a pid launchd has already reaped,
    /// `proc_find` answers `ESRCH`, `watch` answers `None`, and the
    /// `expect("EVFILT_PROC attaches to a same-user process we may SIGKILL")`
    /// panics. That is the shape of the red main carried from fe4a680fd — this
    /// test passing alone and failing inside a full `test -p aterm-gui --lib` —
    /// though that run's own message was not kept, so the window is named here
    /// as the one hazard the fixture had, not as a captured trace. The same
    /// window sat under the sibling that asserts "it is alive right now"
    /// against a `sleep 0.3` orphan.
    ///
    /// MEASURED before the gate, on this Apple silicon Mac (2026-09-18): 80
    /// concurrent copies of this test under 28 CPU hogs, then two more full
    /// 4944-test runs of the old binary under 20 hogs, and the window did not
    /// fire once — which is why the fixture is fixed by construction rather
    /// than by a longer sleep: a window that fires only under a load this
    /// machine could not reproduce is still a window. The gate is a pipe this
    /// test owns (per test, never a shared path or a global), CLOEXEC on both
    /// ends — but on Darwin [`make_cloexec_pipe`](super::make_cloexec_pipe) is
    /// `pipe()` followed by a separate `set_cloexec` per end, so a sibling's fork on
    /// another thread inside that gap CAN carry a copy of either end. That is
    /// tolerated, not prevented: a carried read end is never read from, and a
    /// carried write end cannot stall the orphan because [`OrphanGate::release`]
    /// writes the newline `read` is waiting for rather than closing to EOF.
    /// The orphan's script waits on descriptor 3 (`read -r go <&3`), because a
    /// non-interactive shell points a background job's STDIN at `/dev/null` and
    /// descriptor 3 is untouched by that rule in bash, dash and zsh alike.
    struct OrphanGate(std::os::fd::OwnedFd);

    impl OrphanGate {
        /// Let the orphan run its script. One newline is what `read` is waiting
        /// for, so the orphan proceeds whether or not any other holder of the
        /// read end is still alive; a broken pipe means the orphan is already
        /// gone, which no caller here reaches and which is not the fixture's
        /// failure to report either way.
        fn release(self) {
            use std::io::Write as _;
            let mut writer = std::fs::File::from(self.0);
            let _ = writer.write_all(b"\n");
        }
    }

    /// Make a process that is NOT our child: fork a middle process, let IT fork the
    /// orphan, then reap the middle so the orphan reparents to launchd. Returns the
    /// orphan's pid, which `waitid` will answer `ECHILD` about forever after, and
    /// the [`OrphanGate`] that keeps the orphan from running `script` until the
    /// caller says so.
    fn spawn_orphan(script: &str) -> (u32, OrphanGate) {
        let (gate_read, gate_write) =
            super::make_cloexec_pipe().expect("a pipe for the orphan's gate");
        // THE ORPHAN'S STANDARD STREAMS GO TO `/dev/null`, not to the pipe this
        // reads: a background job inherits the pipe's write end, so leaving it
        // open would make `read_to_string` below wait for the ORPHAN to exit and
        // hand back a pid that is already gone — which is a fixture that silently
        // tests nothing. The gate's read end arrives as the middle's stdin and is
        // moved to descriptor 3 before the job is forked, so the job's stdin is
        // the `/dev/null` a background job gets anyway and its gate is the one
        // descriptor no shell rewrites.
        let mut middle = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(format!(
                "exec 3<&0 0</dev/null; {{ read -r go <&3; {script} ; }} >/dev/null 2>&1 & echo $!"
            ))
            .stdin(std::process::Stdio::from(gate_read))
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn the middle process");
        let mut pid = String::new();
        {
            use std::io::Read as _;
            middle
                .stdout
                .as_mut()
                .expect("piped")
                .read_to_string(&mut pid)
                .expect("the middle process reports the orphan's pid");
        }
        middle.wait().expect("reap the middle process");
        (pid.trim().parse().expect("a pid"), OrphanGate(gate_write))
    }

    /// Wait for the watch to answer, or fail. Bounded HERE ONLY; the production
    /// reads are both single zero-timeout polls.
    fn wait_for_status(watch: &CandidateExitWatch) -> std::process::ExitStatus {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            if let Some(status) = watch.exit_status() {
                return status;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "an exited process must report through EVFILT_PROC"
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    /// THE CAPABILITY THE SHIPPING LANE HAD NO SUBSTITUTE FOR: a full `wait(2)`
    /// status for a process this one did not fork and may not reap.
    ///
    /// Both halves matter and they are the two verdicts that used to be
    /// indistinguishable there — a candidate that CHOSE to stop, and a candidate
    /// something else stopped.
    #[test]
    fn a_non_parent_can_read_an_orphan_s_own_exit_status() {
        use std::os::unix::process::ExitStatusExt as _;

        let (refused, gate) = spawn_orphan("exit 7");
        // Attached while the orphan is provably alive: it cannot run `exit`
        // before the gate opens, and the gate opens after this returns.
        let watch = CandidateExitWatch::watch(refused)
            .expect("EVFILT_PROC attaches to a same-user process we may SIGKILL");
        gate.release();
        let status = wait_for_status(&watch);
        assert_eq!(
            status.code(),
            Some(7),
            "a clean exit reaches a watcher that is nobody's parent — which is \
             what lets the launched lane tell a refusal from a kill at all"
        );

        let (starved, gate) = spawn_orphan("sleep 30");
        let watch = CandidateExitWatch::watch(starved)
            .expect("EVFILT_PROC attaches to a same-user process we may SIGKILL");
        gate.release();
        assert!(
            watch.exit_status().is_none(),
            "a LIVE candidate must report nothing: the pre-kill read is what \
             makes a SIGKILL attributable, so a false positive there would call \
             every rejection a machine kill"
        );
        let pid = libc::pid_t::try_from(starved).expect("pid fits");
        // SAFETY: `kill` against a positive pid we just created and still see.
        assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
        let status = wait_for_status(&watch);
        assert_eq!(
            status.signal(),
            Some(libc::SIGKILL),
            "and the machine's kill arrives with the same fidelity — this is the \
             field shape the launched lane could previously observe nothing about"
        );
    }

    /// THE SHIPPING macOS LANE, END TO END, WITH THE FIELD SHAPE.
    ///
    /// A LaunchServices successor is launchd's child: `waitid` answers `ECHILD`
    /// forever, so before the witness above this assembly could observe NOTHING
    /// here and every `ChildDied` on the lane the defect was reported on was
    /// classified from inference. This drives the real
    /// [`super::observe_candidate_death`] against a real orphan that a real
    /// external `SIGKILL` ended — macOS jetsam reclaiming memory from a process
    /// the machine was starving — and asserts the two things that have to follow:
    /// the death is read as the MACHINE's, and the automatic lane RETRIES.
    #[test]
    fn a_starved_candidate_on_the_launched_lane_takes_the_retry_lane() {
        use super::{HandoffCandidate, HandoffCandidateHandle, observe_candidate_death};
        use crate::app_native::{HandoffFailureLane as Lane, PhysicalFailureShape as Shape};

        let (starved, gate) = spawn_orphan("sleep 30");
        // Registered while it is alive, exactly as the rendezvous accept does.
        let watch = CandidateExitWatch::watch(starved).expect("the candidate is alive");
        gate.release();
        let pid = libc::pid_t::try_from(starved).expect("pid fits");
        // SOMEBODY ELSE'S SIGKILL — the field shape, and the one signal that is
        // indistinguishable from this lane's own rejection.
        // SAFETY: `pid` is a positive pid we created and can still see.
        assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        // SAFETY: signal 0 performs the existence check only.
        while unsafe { libc::kill(pid, 0) } == 0 {
            assert!(std::time::Instant::now() < deadline, "the orphan must die");
            std::thread::sleep(std::time::Duration::from_millis(2));
        }

        let candidate = HandoffCandidate::of_attested_peer(starved);
        let mut handle = HandoffCandidateHandle::Launched(Some(watch));
        let (_, death) = observe_candidate_death(
            crate::UpdateHandoffOutcome::ChildDied,
            candidate,
            &mut handle,
        );
        assert_eq!(
            death,
            crate::ChildDeathEvidence::Signalled {
                signal: libc::SIGKILL
            },
            "THE FIELD CASE ON THE SHIPPING LANE: a candidate this process never \
             forked, ended by the machine, and the parent can finally say so"
        );
        assert_eq!(
            Lane::classify(
                crate::native_updater_service::ApplyMode::AutomaticPastGrace,
                crate::UpdateHandoffOutcome::ChildDied,
                death,
                false
            ),
            Lane::Physical(Shape::Transient),
            "…and it must reach the RETRY lane. `Structural` here is the defect: \
             two of these converged the automatic lane on bytes that applied \
             perfectly once the machine stopped being busy"
        );
    }

    /// AND THE OTHER HALF OF THE SAME ASSEMBLY: an exit this process did NOT
    /// witness before it acted, recovered after the candidate is provably gone.
    ///
    /// A launched candidate whose kernel birth stamp cannot be read is
    /// `Unwitnessed`, and this lane deliberately signals such a candidate NOTHING
    /// (a `-pid` group kill on an unpinned number can land on a stranger). So the
    /// termination proof waits the candidate out and the witness is read AFTER it
    /// — which is sound for an exit code, because `SIGKILL` is the only signal
    /// this process sends and it never yields `WIFEXITED`.
    ///
    /// Deleting that second read leaves this `Unobserved`: six bounded retries for
    /// a successor that stated in as many words that it had decided to stop.
    #[test]
    fn an_exit_the_parent_did_not_see_coming_is_still_recovered_after_the_wait() {
        use super::{HandoffCandidate, HandoffCandidateHandle, observe_candidate_death};

        let (refusing, gate) = spawn_orphan("exit 0");
        let watch = CandidateExitWatch::watch(refusing).expect("the candidate is alive");
        // A bare pid carries no birth stamp, so nothing is signalled and the
        // candidate reaches its own `exit` — the deterministic form of the race
        // this read exists to widen.
        let candidate = HandoffCandidate::from_bare_pid(refusing);
        let mut handle = HandoffCandidateHandle::Launched(Some(watch));
        // "ALIVE RIGHT NOW" IS A FACT HERE, NOT A HOPE: the gate is still shut,
        // so the orphan has not reached its `exit` and cannot until it opens.
        assert!(
            handle.witnessed_exit_status().is_none(),
            "it is alive right now: the pre-kill read must claim nothing"
        );
        gate.release();
        let (_, death) = observe_candidate_death(
            crate::UpdateHandoffOutcome::ChildDied,
            candidate,
            &mut handle,
        );
        assert_eq!(
            death,
            crate::ChildDeathEvidence::Exited { code: 0 },
            "the knote XNU queued inside `proc_exit` is still there once \
             termination is proven, and an exit code can never be our SIGKILL"
        );
    }

    /// THE FAILING DIRECTION, which must cost only evidence. A pid that names
    /// nothing cannot be attached to, and the answer has to be `None` rather than
    /// an error path the reject lane would have to handle.
    #[test]
    fn watching_a_candidate_that_is_already_gone_answers_none() {
        let (gone, gate) = spawn_orphan("exit 0");
        gate.release();
        let pid = libc::pid_t::try_from(gone).expect("pid fits");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        // SAFETY: signal 0 performs the existence check only and delivers nothing.
        while unsafe { libc::kill(pid, 0) } == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "the orphan must be reaped by launchd"
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(
            CandidateExitWatch::watch(gone).is_none(),
            "ESRCH is not an error here: it is the launched lane losing its \
             witness, which degrades to Unobserved and a bounded retry"
        );
        assert!(
            CandidateExitWatch::watch(1).is_none(),
            "and launchd itself is never a candidate"
        );
    }
}

#[cfg(all(test, unix))]
mod handoff_process_group_tests {
    use super::{
        HandoffCandidate, HandoffCandidateHandle, HandoffCommitFacts, HandoffRejectDelivery,
        HandoffRollbackWarrant, ProcessGroupContainment, ReadyPollAction, classify_ready_poll,
        contain_own_process_group, deliver_handoff_rejection,
        emergency_kill_and_reap_handoff_child, handoff_candidate_terminated,
        handoff_commit_admitted, handoff_masters_closed, handoff_masters_have_activity,
        handoff_ready_deadline, kill_and_reap_handoff_child, make_cloexec_pipe, wait_handoff_ready,
        worker_claim_handoff_reaper,
    };
    use std::io::{BufRead as _, Read as _};
    use std::os::unix::process::CommandExt as _;

    /// Panic-safe ownership for fixtures that deliberately leave a 30-second
    /// descendant alive while assertions exercise reaper ordering.
    struct ProcessGroupCleanup {
        leader: i32,
        armed: bool,
    }

    impl ProcessGroupCleanup {
        fn new(leader: i32) -> Self {
            Self {
                leader,
                armed: true,
            }
        }

        fn disarm(&mut self) {
            self.armed = false;
        }
    }

    impl Drop for ProcessGroupCleanup {
        fn drop(&mut self) {
            if !self.armed {
                return;
            }
            unsafe { libc::kill(-self.leader, libc::SIGKILL) };
            let mut status = 0;
            loop {
                let waited = unsafe { libc::waitpid(self.leader, &mut status, 0) };
                if waited == self.leader
                    || (waited < 0
                        && std::io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD))
                {
                    break;
                }
                if waited < 0
                    && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
                {
                    break;
                }
            }
        }
    }

    fn assert_process_group_gone(leader: i32, descendant: i32, message: &str) {
        // Waits for launchd to REAP an orphaned descendant — work this process does not
        // control and cannot hurry. The old wait also busy-spun on `yield_now()` at
        // 100% CPU, delaying the very reaping it waited for. A real regression leaves
        // the descendant alive indefinitely, so this is a failure bound.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let group_gone = unsafe { libc::kill(-leader, 0) } != 0
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
            let descendant_gone = unsafe { libc::kill(descendant, 0) } != 0
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
            if group_gone && descendant_gone {
                return;
            }
            if std::time::Instant::now() >= deadline {
                // Fail clean: no intentionally long-lived fixture escapes even
                // when the assertion is proving a real production regression.
                unsafe {
                    libc::kill(-leader, libc::SIGKILL);
                    libc::kill(descendant, libc::SIGKILL);
                }
                panic!("{message}");
            }
            // sleep, not yield_now(): yielding keeps this thread RUNNABLE, so on an
            // oversubscribed box it competes with the very work it is waiting for.
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn final_handoff_admission_conforms_to_every_model_guard() {
        let all = HandoffCommitFacts {
            exact_sessions: true,
            exact_layout: true,
            exact_activity: true,
            teardown_allows_commit: true,
            parent_still_parked: true,
            sessions_alive: true,
            input_dispatch_fenced: true,
            egress_settled: true,
            native_safe: true,
            proof_exact: true,
            commit_channel: true,
        };
        assert!(handoff_commit_admitted(all));

        // Process-local egress (paste-order FIFO / sink spill) is one concrete
        // queue BEHIND the spec model's single "deliver queued input to the
        // masters" step, so it is fenced by direct admission assertion rather
        // than a model-fired action: an unflushed egress buffer must block
        // Commit exactly like an undrained OS input queue.
        assert!(
            !handoff_commit_admitted(HandoffCommitFacts {
                egress_settled: false,
                ..all
            }),
            "Commit must not fire while tolerated input is still in a process-local queue"
        );

        let cases = [
            (
                "sessions",
                HandoffCommitFacts {
                    exact_sessions: false,
                    ..all
                },
                "SessionsChange",
            ),
            (
                "layout",
                HandoffCommitFacts {
                    exact_layout: false,
                    ..all
                },
                "LayoutChanges",
            ),
            (
                "activity epoch",
                HandoffCommitFacts {
                    exact_activity: false,
                    ..all
                },
                "ActivityRevokesEpoch",
            ),
            (
                "teardown",
                HandoffCommitFacts {
                    teardown_allows_commit: false,
                    ..all
                },
                "DestructiveIntentRevokesCommit",
            ),
            (
                "parent reader",
                HandoffCommitFacts {
                    parent_still_parked: false,
                    ..all
                },
                "ParentReaderResumesBeforeCommit",
            ),
            (
                "session death",
                HandoffCommitFacts {
                    sessions_alive: false,
                    ..all
                },
                "PtySessionDies",
            ),
            (
                "queued hardware input",
                HandoffCommitFacts {
                    input_dispatch_fenced: false,
                    ..all
                },
                "QueueHardwareInput",
            ),
            (
                "native safety",
                HandoffCommitFacts {
                    native_safe: false,
                    ..all
                },
                "RevokeNativeSafety",
            ),
            (
                "proof",
                HandoffCommitFacts {
                    proof_exact: false,
                    ..all
                },
                "ChildSendsMismatchedProof",
            ),
            (
                "commit channel",
                HandoffCommitFacts {
                    commit_channel: false,
                    ..all
                },
                "LoseCommitChannel",
            ),
        ];
        for (name, facts, revoked_action) in cases {
            assert!(
                !handoff_commit_admitted(facts),
                "missing {name} was admitted"
            );
            let model = aterm_spec::derive::native_update_overlap_handoff_model();
            let mut state = model.init_state();
            assert!(model.fire("ParkParentReaders", &mut state));
            assert!(model.fire("SpawnReaderlessChild", &mut state));
            if revoked_action == "ChildSendsMismatchedProof" {
                assert!(model.fire(revoked_action, &mut state));
            } else {
                assert!(model.fire("ChildPaintsExactProof", &mut state));
                assert!(model.fire(revoked_action, &mut state), "{name}: {state:?}");
            }
            assert!(
                model.successors("MainWinsCommitArbiter", &state).is_empty(),
                "model still authorized Commit without {name}: {state:?}"
            );
        }

        let model = aterm_spec::derive::native_update_overlap_handoff_model();
        let mut exact = model.init_state();
        for action in [
            "ParkParentReaders",
            "SpawnReaderlessChild",
            "ChildPaintsExactProof",
            "MainWinsCommitArbiter",
        ] {
            assert!(model.fire(action, &mut exact), "{action}: {exact:?}");
        }
        assert_eq!(exact["arbiter"], 1);
    }

    /// Seamless seam 2, model level: queued PTY output and queued-then-drained
    /// hardware input BUFFER THROUGH the overlap — Commit stays reachable with
    /// output queued the whole way, while an UNDRAINED OS input queue parks
    /// (never fails) Commit until the drain action delivers it to the masters.
    #[test]
    fn queued_output_and_drained_input_buffer_through_commit_in_the_model() {
        let model = aterm_spec::derive::native_update_overlap_handoff_model();
        let mut state = model.init_state();
        for action in [
            "ParkParentReaders",
            "SpawnReaderlessChild",
            "ChildPaintsExactProof",
            "PtyOutputQueues",
            "QueueHardwareInput",
        ] {
            assert!(model.fire(action, &mut state), "{action}: {state:?}");
        }
        // Undispatched hardware input PARKS Commit (it would die with _exit)…
        assert!(
            model.successors("MainWinsCommitArbiter", &state).is_empty(),
            "commit admitted with an undrained OS input queue: {state:?}"
        );
        // …but is not a failure: the reject arbiter has no authority either.
        assert!(
            model
                .successors("WorkerWinsRejectArbiter", &state)
                .is_empty(),
            "queued input must defer, not fail, the attempt: {state:?}"
        );
        for action in [
            "DrainQueuedHardwareInput",
            "MainWinsCommitArbiter",
            "CommitModern",
            "ReleaseModernReaders",
        ] {
            assert!(model.fire(action, &mut state), "{action}: {state:?}");
        }
        assert_eq!(state["commit"], 1);
        assert_eq!(state["child_readers"], 1);
        assert_eq!(
            state["pty_output_queued"], 1,
            "the committed run carried queued output the whole way — the child drains it"
        );
    }

    /// Seamless seam 2, descriptor level: readable bytes on a handed-off
    /// master are visible to the PRE-PARK admission peek but invisible to the
    /// mid-flight death peek; closing the peer flips the death peek without
    /// consuming the queued bytes (they remain for the child).
    #[test]
    fn queued_output_is_buffered_mid_flight_but_peer_death_still_revokes() {
        // A REAL PTY pair — the deployed descriptor shape — not a pipe:
        // poll(2)'s HUP semantics differ between the two, and the death peek's
        // contract is stated for masters.
        let (mut master, mut slave) = (-1i32, -1i32);
        // SAFETY: openpty(3) into two valid out-slots; no termios/winsize.
        let opened = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(opened, 0, "openpty");
        let live = [(1u64, master, 4242i32)];

        assert!(!handoff_masters_have_activity(&live), "quiet pty is quiet");
        assert!(!handoff_masters_closed(&live), "live peer is not dead");

        // "Shell output" queued in the kernel for the (future) child.
        // SAFETY: bounded write of a stack byte to the test-owned slave.
        assert_eq!(
            unsafe { libc::write(slave, [0x62u8].as_ptr().cast(), 1) },
            1
        );
        assert!(
            handoff_masters_have_activity(&live),
            "pre-park admission still refuses to START over flowing output"
        );
        assert!(
            !handoff_masters_closed(&live),
            "mid-flight, queued output buffers through instead of revoking"
        );

        aterm_pty::close_fd(slave);
        // PROMPT-EVENTUAL, not same-instant: production peeks repeatedly
        // across the overlap, so the contract is that peer death becomes
        // visible to the peek promptly — and under full-suite scheduler load
        // macOS can surface the HUP edge a quantum after close(2), which is
        // exactly where the same-instant version of this assert flaked (twice,
        // never solo). The bounded retry keeps the teeth: a REAL stale-
        // identity bug never reports closed, and two seconds of grace cannot
        // mask it.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !handoff_masters_closed(&live) {
            assert!(
                std::time::Instant::now() < deadline,
                "peer death must still revoke — the live-set identity is stale"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        aterm_pty::close_fd(master);
    }

    /// Seamless seam 2, worker level: the ready wait completes to `ProofReady`
    /// while a handed-off master has queued readable output, and a cancel poke
    /// is typed `ActivityRevoked` (the retry-budget classification), never a
    /// generic rejection.
    #[test]
    fn ready_wait_tolerates_queued_output_and_types_cancel_as_activity() {
        let expected = crate::seamless::adoption_proof(
            "ready-wait-buffered-output",
            2,
            "abcdef0",
            &[0x11; 32],
            &[0x22; 32],
            &[],
        )
        .expect("bounded proof fixture");

        // Master with queued output the whole time.
        let mut master = [0i32; 2];
        // SAFETY: plain pipe(2) into a valid 2-slot out-array.
        assert_eq!(unsafe { libc::pipe(master.as_mut_ptr()) }, 0, "master pipe");
        let (master_rd, master_wr) = (master[0], master[1]);
        // SAFETY: bounded write of a stack byte to the test-owned pipe.
        assert_eq!(
            unsafe { libc::write(master_wr, [0x62u8].as_ptr().cast(), 1) },
            1
        );

        let (proof_rd, proof_wr) = make_cloexec_pipe().expect("proof pipe");
        let wire = expected.to_wire();
        // SAFETY: bounded write of the fixed proof wire to the test-owned pipe.
        let wrote = unsafe {
            use std::os::fd::AsRawFd as _;
            libc::write(proof_wr.as_raw_fd(), wire.as_ptr().cast(), wire.len())
        };
        assert_eq!(wrote as usize, wire.len(), "one complete proof wire");
        let (_cancel_tx, cancel_rx) = std::sync::mpsc::sync_channel(1);
        assert_eq!(
            wait_handoff_ready(
                &proof_rd,
                expected,
                &cancel_rx,
                &[master_rd],
                handoff_ready_deadline(),
                None,
            ),
            crate::UpdateHandoffOutcome::ProofReady,
            "queued shell output must not abort the ready wait"
        );

        // Cancel is the typed activity outcome.
        let (proof_rd, _proof_wr) = make_cloexec_pipe().expect("second proof pipe");
        let (cancel_tx, cancel_rx) = std::sync::mpsc::sync_channel(1);
        cancel_tx.try_send(()).expect("queue cancel poke");
        assert_eq!(
            wait_handoff_ready(
                &proof_rd,
                expected,
                &cancel_rx,
                &[master_rd],
                handoff_ready_deadline(),
                None,
            ),
            crate::UpdateHandoffOutcome::ActivityRevoked,
            "cancel must carry the typed activity classification"
        );

        aterm_pty::close_fd(master_rd);
        aterm_pty::close_fd(master_wr);
    }

    /// S0 ANTI-SPIN: the ready-wait poll classifier must route a master that is
    /// merely READABLE (queued shell output — the exact condition the tolerate-
    /// output contract newly allows) to `NoProgress`, the branch that YIELDS
    /// before re-polling. A plain-POLLIN master answer that fell through to an
    /// immediate `continue` is what pegged a core; proving it lands on the
    /// yielding verdict (and never on `ReadProof`/`SessionDied`) is the fix's
    /// standing guard, with none of a CPU/timing assertion's flakiness.
    #[test]
    fn ready_poll_classifies_queued_master_output_as_a_yield() {
        let pfd = |revents: libc::c_short| libc::pollfd {
            fd: -1,
            events: libc::POLLIN,
            revents,
        };
        // Proof idle, master carrying plain queued output → YIELD (the spin
        // condition). Before the fix this fell through to a sleepless continue.
        assert_eq!(
            classify_ready_poll(&[pfd(0), pfd(libc::POLLIN)]),
            ReadyPollAction::NoProgress,
            "queued master output must yield, never busy-spin"
        );
        // A bare wake with nothing readable is also just a yield.
        assert_eq!(
            classify_ready_poll(&[pfd(0), pfd(0)]),
            ReadyPollAction::NoProgress
        );
        // Death on a master dominates — even while it also reports readable
        // bytes (macOS reports a dead slave as POLLIN|POLLHUP).
        assert_eq!(
            classify_ready_poll(&[pfd(0), pfd(libc::POLLIN | libc::POLLHUP)]),
            ReadyPollAction::SessionDied,
            "a stale live-set identity must reject, not read the proof"
        );
        assert_eq!(
            classify_ready_poll(&[pfd(libc::POLLIN), pfd(libc::POLLERR)]),
            ReadyPollAction::SessionDied,
            "master death outranks a readable proof"
        );
        // A readable proof is progress — even with queued output alongside it.
        assert_eq!(
            classify_ready_poll(&[pfd(libc::POLLIN), pfd(libc::POLLIN)]),
            ReadyPollAction::ReadProof
        );
        assert_eq!(
            classify_ready_poll(&[pfd(libc::POLLHUP), pfd(libc::POLLIN)]),
            ReadyPollAction::ReadProof,
            "proof EOF (its write end dropped) is still a read, detecting ChildDied"
        );
    }

    #[test]
    fn full_reject_channel_remains_worker_owned_but_disconnect_does_not() {
        let (full_sender, full_receiver) = std::sync::mpsc::sync_channel(1);
        full_sender.try_send(()).expect("fill rejection slot");
        assert_eq!(
            deliver_handoff_rejection(Some(full_sender)),
            HandoffRejectDelivery::WorkerOwned,
            "Full means a rejection is already queued for the live worker"
        );
        assert_eq!(full_receiver.try_recv(), Ok(()));

        let (disconnected_sender, disconnected_receiver) = std::sync::mpsc::sync_channel(1);
        drop(disconnected_receiver);
        assert_eq!(
            deliver_handoff_rejection(Some(disconnected_sender)),
            HandoffRejectDelivery::Disconnected,
            "only receiver loss transfers authority to an emergency reaper"
        );
        assert_eq!(
            deliver_handoff_rejection(None),
            HandoffRejectDelivery::Disconnected
        );
    }

    #[test]
    fn cancellation_kills_descendant_process_group_before_returning() {
        let mut command = std::process::Command::new("/bin/sh");
        command
            .arg("-c")
            .arg("sleep 30 & printf '%s\\n' \"$!\"; wait")
            .stdout(std::process::Stdio::piped());
        // SAFETY: async-signal-safe setpgid only; identical to the production
        // update child setup and executed after fork/before exec.
        unsafe {
            command.pre_exec(|| {
                if libc::setpgid(0, 0) == 0 {
                    Ok(())
                } else {
                    Err(std::io::Error::last_os_error())
                }
            });
        }
        let mut child = command.spawn().expect("spawn process-group leader");
        let leader = i32::try_from(child.id()).expect("bounded child pid");
        let mut cleanup = ProcessGroupCleanup::new(leader);
        let mut descendant_line = String::new();
        std::io::BufReader::new(child.stdout.take().expect("child stdout"))
            .read_line(&mut descendant_line)
            .expect("descendant pid line");
        let descendant: i32 = descendant_line
            .trim()
            .parse()
            .expect("numeric descendant pid");
        assert_eq!(unsafe { libc::kill(descendant, 0) }, 0, "descendant live");

        let candidate = HandoffCandidate::of_unreaped_child(&child);
        let mut handle = HandoffCandidateHandle::Forked(child);
        assert_eq!(
            kill_and_reap_handoff_child(candidate, &mut handle).warrant,
            HandoffRollbackWarrant::Reaped,
            "our own fork child must be licensed by the strongest authority"
        );
        cleanup.disarm();
        assert_process_group_gone(
            leader,
            descendant,
            "kill/reap returned but a descendant process-group member survived",
        );
    }

    #[test]
    fn pre_ready_eof_preserves_leader_identity_until_normal_group_reap() {
        let mut command = std::process::Command::new("/bin/sh");
        command
            .arg("-c")
            .arg("sleep 30 >/dev/null 2>&1 & printf '%s\\n' \"$!\"; exit 0")
            .stdout(std::process::Stdio::piped());
        // SAFETY: async-signal-safe setpgid only, matching production.
        unsafe {
            command.pre_exec(|| {
                if libc::setpgid(0, 0) == 0 {
                    Ok(())
                } else {
                    Err(std::io::Error::last_os_error())
                }
            });
        }
        let mut child = command.spawn().expect("spawn exiting group leader");
        let leader = i32::try_from(child.id()).expect("bounded child pid");
        let mut cleanup = ProcessGroupCleanup::new(leader);
        let mut stdout = std::io::BufReader::new(child.stdout.take().expect("child stdout"));
        let mut descendant_line = String::new();
        stdout
            .read_line(&mut descendant_line)
            .expect("descendant pid line");
        let descendant: i32 = descendant_line
            .trim()
            .parse()
            .expect("numeric descendant pid");
        let mut eof = Vec::new();
        stdout.read_to_end(&mut eof).expect("leader stdout EOF");
        assert_eq!(unsafe { libc::kill(descendant, 0) }, 0, "descendant live");

        // Tier-1 projection: EOF observes an exited direct leader while retaining
        // its waitable identity and a live same-group descendant. Rollback is not
        // enabled until the shipping reaper has signaled that group and waited the
        // direct child, in that order.
        let model = aterm_spec::derive::native_update_overlap_handoff_model();
        let mut model_state = model.init_state();
        for action in [
            "ParkParentReaders",
            "SpawnReaderlessChild",
            "SpawnProcessGroupDescendant",
            "LeaderDiesLeavingLiveDescendant",
        ] {
            assert!(
                model.fire(action, &mut model_state),
                "{action}: {model_state:?}"
            );
        }

        let (proof_rd, proof_wr) = make_cloexec_pipe().expect("proof pipe");
        drop(proof_wr); // exact pre-ready child-death signal: proof EOF
        let expected = crate::seamless::adoption_proof(
            "pre-ready-eof-test",
            2,
            "abcdef0",
            &[0x11; 32],
            &[0x22; 32],
            &[],
        )
        .expect("bounded proof fixture");
        let (_cancel_tx, cancel_rx) = std::sync::mpsc::sync_channel(1);
        assert_eq!(
            wait_handoff_ready(
                &proof_rd,
                expected,
                &cancel_rx,
                &[],
                handoff_ready_deadline(),
                None,
            ),
            crate::UpdateHandoffOutcome::ChildDied,
            "proof EOF detects death without reaping the group leader"
        );

        let arbiter = crate::HandoffAttemptArbiter::new();
        assert!(worker_claim_handoff_reaper(&arbiter));
        assert!(model.fire("WorkerWinsRejectArbiter", &mut model_state));

        // Negative control: the removed wait-before-kill ordering is explicitly
        // executable only in the mutant and immediately violates the ordering
        // property while this real descendant is known live.
        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        let mut old_order = model_state.clone();
        assert!(buggy.fire("BuggyWaitBeforeGroupSignal", &mut old_order));
        assert!(!buggy.check_invariant("ProcessGroupSignalPrecedesDirectChildReap", &old_order,));
        assert_eq!(old_order["descendant_live"], 1);

        let candidate = HandoffCandidate::of_unreaped_child(&child);
        let mut handle = HandoffCandidateHandle::Forked(child);
        assert_eq!(
            kill_and_reap_handoff_child(candidate, &mut handle).warrant,
            HandoffRollbackWarrant::Reaped,
            "an exited-but-unreaped fork child is still ours to wait"
        );
        assert!(model.fire("KillRejectedChild", &mut model_state));
        assert!(model.fire("ReapKilledChild", &mut model_state));
        assert!(arbiter.finish_reap(crate::HandoffReaperOwner::Worker));
        cleanup.disarm();
        assert_process_group_gone(
            leader,
            descendant,
            "pre-ready EOF path reaped leader but left its descendant live",
        );
        assert_eq!(model_state["group_signaled"], 1);
        assert_eq!(model_state["descendant_live"], 0);
        assert_eq!(model_state["child_reaped"], 1);
        assert_eq!(
            model
                .successors("ResumeParentAfterReap", &model_state)
                .len(),
            1,
            "rollback becomes enabled only after group signal + direct-child reap"
        );
    }

    /// LAUNCHSERVICES' ANSWER IS READ BESIDE THE PROOF WAIT (2026-09-19): a pid
    /// mismatch that lands AFTER the transfer still refuses the handoff before
    /// Commit, and the refusal is a kill-and-reap of the corroborated candidate
    /// (the descriptors are its by then), after which — and only after which —
    /// the parent may resume its readers.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_launchservices_pid_mismatch_after_transfer_is_rejected_and_reaped() {
        let mut command = std::process::Command::new("/bin/sh");
        command.arg("-c").arg("sleep 30");
        // SAFETY: async-signal-safe setpgid only, matching production.
        unsafe {
            command.pre_exec(|| {
                if libc::setpgid(0, 0) == 0 {
                    Ok(())
                } else {
                    Err(std::io::Error::last_os_error())
                }
            });
        }
        let child = command.spawn().expect("spawn group leader");
        let leader = i32::try_from(child.id()).expect("bounded child pid");
        let mut cleanup = ProcessGroupCleanup::new(leader);

        // Tier-1 projection: a launch answer naming another pid is the candidate
        // failing its identity check — the model's mismatched-proof failure —
        // and the worker then wins the reject arbiter, kills, reaps, and only
        // then may the parent resume.
        let model = aterm_spec::derive::native_update_overlap_handoff_model();
        let mut model_state = model.init_state();
        for action in [
            "ParkParentReaders",
            "SpawnReaderlessChild",
            "ChildSendsMismatchedProof",
        ] {
            assert!(
                model.fire(action, &mut model_state),
                "{action}: {model_state:?}"
            );
        }

        // The proof pipe stays OPEN: nothing but the corroboration ends this wait.
        let (proof_rd, _proof_wr) = make_cloexec_pipe().expect("proof pipe");
        let expected = crate::seamless::adoption_proof(
            "corroboration-test",
            2,
            "abcdef0",
            &[0x11; 32],
            &[0x22; 32],
            &[],
        )
        .expect("bounded proof fixture");
        let (_cancel_tx, cancel_rx) = std::sync::mpsc::sync_channel(1);
        let stranger = leader.saturating_add(1);
        let in_flight = crate::app_launch_successor::LaunchInFlight::scripted(Some(stranger));
        let started = std::time::Instant::now();
        assert_eq!(
            wait_handoff_ready(
                &proof_rd,
                expected,
                &cancel_rx,
                &[],
                std::time::Instant::now() + std::time::Duration::from_secs(10),
                Some(super::LaunchCorroboration {
                    in_flight: &in_flight,
                    dialer_pid: leader,
                    answered: false,
                }),
            ),
            crate::UpdateHandoffOutcome::Rejected,
            "a launch answer naming another pid refuses the handoff"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "the refusal is immediate, not at the deadline: {:?}",
            started.elapsed()
        );

        let arbiter = crate::HandoffAttemptArbiter::new();
        assert!(worker_claim_handoff_reaper(&arbiter));
        assert!(model.fire("WorkerWinsRejectArbiter", &mut model_state));
        let candidate = HandoffCandidate::of_unreaped_child(&child);
        let mut handle = HandoffCandidateHandle::Forked(child);
        assert_eq!(
            kill_and_reap_handoff_child(candidate, &mut handle).warrant,
            HandoffRollbackWarrant::Reaped
        );
        assert!(model.fire("KillRejectedChild", &mut model_state));
        assert!(model.fire("ReapKilledChild", &mut model_state));
        assert!(arbiter.finish_reap(crate::HandoffReaperOwner::Worker));
        cleanup.disarm();
        assert_eq!(
            unsafe { libc::kill(leader, 0) },
            -1,
            "the rejected candidate is dead"
        );
        assert_eq!(
            model
                .successors("ResumeParentAfterReap", &model_state)
                .len(),
            1,
            "rollback becomes enabled only after the kill and the reap"
        );
    }

    /// The matching answer, and no answer at all, change nothing: the wait runs to
    /// its real outcome (here proof EOF, the child's death).
    #[cfg(target_os = "macos")]
    #[test]
    fn a_matching_or_pending_launch_answer_leaves_the_proof_wait_to_its_outcome() {
        let expected = crate::seamless::adoption_proof(
            "corroboration-test",
            2,
            "abcdef0",
            &[0x11; 32],
            &[0x22; 32],
            &[],
        )
        .expect("bounded proof fixture");
        for answer in [Some(4242), None] {
            let (proof_rd, proof_wr) = make_cloexec_pipe().expect("proof pipe");
            drop(proof_wr);
            let (_cancel_tx, cancel_rx) = std::sync::mpsc::sync_channel(1);
            let in_flight = crate::app_launch_successor::LaunchInFlight::scripted(answer);
            assert_eq!(
                wait_handoff_ready(
                    &proof_rd,
                    expected,
                    &cancel_rx,
                    &[],
                    std::time::Instant::now() + std::time::Duration::from_secs(10),
                    Some(super::LaunchCorroboration {
                        in_flight: &in_flight,
                        dialer_pid: 4242,
                        answered: false,
                    }),
                ),
                crate::UpdateHandoffOutcome::ChildDied,
                "answer {answer:?}: the corroboration neither rejects nor ends the wait"
            );
        }
    }

    #[test]
    fn emergency_reaper_kills_group_even_after_leader_exits() {
        let mut command = std::process::Command::new("/bin/sh");
        command
            .arg("-c")
            // The descendant does not retain stdout, so EOF proves the leader
            // completed its immediate exit without our test reaping it.
            .arg("sleep 30 >/dev/null 2>&1 & printf '%s\\n' \"$!\"; exit 0")
            .stdout(std::process::Stdio::piped());
        // SAFETY: async-signal-safe setpgid only, matching production.
        unsafe {
            command.pre_exec(|| {
                if libc::setpgid(0, 0) == 0 {
                    Ok(())
                } else {
                    Err(std::io::Error::last_os_error())
                }
            });
        }
        let mut child = command.spawn().expect("spawn exiting group leader");
        let leader = i32::try_from(child.id()).expect("bounded child pid");
        let mut cleanup = ProcessGroupCleanup::new(leader);
        let mut stdout = std::io::BufReader::new(child.stdout.take().expect("child stdout"));
        let mut descendant_line = String::new();
        stdout
            .read_line(&mut descendant_line)
            .expect("descendant pid line");
        let descendant: i32 = descendant_line
            .trim()
            .parse()
            .expect("numeric descendant pid");
        let mut eof = Vec::new();
        stdout.read_to_end(&mut eof).expect("leader stdout EOF");
        assert_eq!(unsafe { libc::kill(descendant, 0) }, 0, "descendant live");

        let arbiter = crate::HandoffAttemptArbiter::new();
        assert!(arbiter.try_begin_reject());
        assert!(arbiter.claim_reaper(crate::HandoffReaperOwner::Emergency));
        assert_eq!(
            emergency_kill_and_reap_handoff_child(child.id()),
            HandoffRollbackWarrant::Reaped,
            "the emergency reaper still prefers `waitpid` when the candidate is ours"
        );
        // The raw emergency reaper consumed this exact child. An explicit wait
        // observes ECHILD and documents the `Child` handle's completed lifecycle.
        let _ = child.wait();
        assert!(arbiter.finish_reap(crate::HandoffReaperOwner::Emergency));
        cleanup.disarm();
        assert_process_group_gone(
            leader,
            descendant,
            "emergency reap returned but exited leader left a live descendant",
        );
    }

    /// B2, soundness half: the fallback authority's PID-VACANCY proof, in both
    /// directions. A running candidate must never satisfy it (resuming a reader
    /// then is the corruption the overlap exists to prevent), and a candidate
    /// whose pid has come free must always satisfy it — that vacancy is the
    /// same fact `wait` returns, reached without being the parent.
    #[test]
    fn a_running_candidate_is_unproven_and_a_vacant_pid_is_the_proof() {
        let mut child = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("sleep 30")
            .spawn()
            .expect("spawn a live candidate");
        let candidate = HandoffCandidate::of_unreaped_child(&child);
        assert!(
            !handoff_candidate_terminated(candidate),
            "a running candidate must never license a reader resume"
        );

        let pid = i32::try_from(child.id()).expect("bounded child pid");
        // SAFETY: SIGKILL to the test's own child.
        assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0, "kill fixture");
        child.wait().expect("reap the test candidate");
        assert!(
            handoff_candidate_terminated(candidate),
            "a candidate whose pid is vacant has terminated, whoever reaped it"
        );
    }

    /// B2, the blocker itself: a candidate this process did NOT fork — the shape
    /// a LaunchServices-launched successor has — is proven terminated anyway.
    /// `waitpid` answers `ECHILD` for it (asserted below, because that ECHILD is
    /// exactly what costs the old authority its proof), and the orphan is not a
    /// process-group leader either (`sh -c` runs without job control), so
    /// `kill(-pid)` names no group and sweeps nothing: only the
    /// identity-corroborated DIRECT signal can end it. Both halves of the
    /// fallback are therefore load-bearing here.
    ///
    /// Fixture cleanup is the `sleep` itself: every process this spawns exits on
    /// its own within 30 s even if an assertion panics first.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_candidate_we_never_forked_is_still_proven_terminated() {
        let mut command = std::process::Command::new("/bin/sh");
        command
            .arg("-c")
            .arg("sleep 30 >/dev/null 2>&1 & printf '%s\\n' \"$!\"; exit 0")
            .stdout(std::process::Stdio::piped());
        let mut parent = command.spawn().expect("spawn the orphan's parent");
        let mut stdout = std::io::BufReader::new(parent.stdout.take().expect("parent stdout"));
        let mut orphan_line = String::new();
        stdout.read_line(&mut orphan_line).expect("orphan pid line");
        let orphan: u32 = orphan_line.trim().parse().expect("numeric orphan pid");
        // Reaping the middle process is what reparents the orphan to launchd.
        parent.wait().expect("reap the orphan's parent");
        let raw = i32::try_from(orphan).expect("bounded orphan pid");

        let candidate = HandoffCandidate {
            pid: orphan,
            birth: super::read_candidate_birth(orphan),
        };
        assert!(
            candidate.birth.is_some(),
            "a live process has a kernel birth record"
        );
        let mut status = 0i32;
        // SAFETY: a non-blocking wait for a pid that is not our child.
        let waited = unsafe { libc::waitpid(raw, &mut status, libc::WNOHANG) };
        let errno = std::io::Error::last_os_error().raw_os_error();
        assert_eq!(waited, -1, "the orphan must not be waitable by us");
        assert_eq!(errno, Some(libc::ECHILD), "…and the refusal is ECHILD");
        assert!(
            !handoff_candidate_terminated(candidate),
            "the orphan is still running"
        );

        super::signal_handoff_candidate(candidate);
        // Bounded HERE ONLY: a broken fallback must fail this test rather than
        // hang the suite. `wait_for_handoff_candidate_to_terminate` deliberately
        // carries no such bound, because in production the alternative to
        // waiting is resuming a reader without proof.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !handoff_candidate_terminated(candidate) {
            if std::time::Instant::now() >= deadline {
                // Fail clean: no fixture outlives its assertion.
                // SAFETY: SIGKILL to the fixture's own orphan.
                unsafe { libc::kill(raw, libc::SIGKILL) };
                panic!("the outside proof never established that orphan {orphan} terminated");
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// The other edge of the identity check: a pid whose kernel birth stamp
    /// DISAGREES with the candidate's was recycled, which both proves the
    /// candidate terminated and makes signalling that pid an attack on a
    /// bystander. Without a parent's unreaped-child pin — the LaunchServices
    /// shape — this is the only thing standing between the reap path and
    /// SIGKILLing whatever inherited the number.
    ///
    /// Fixture cleanup is the `sleep` itself: the bystander exits on its own
    /// within 30 s even if an assertion panics before the explicit kill.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_pid_whose_birth_record_disagrees_is_proof_of_death_and_is_never_signalled() {
        let mut bystander = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("sleep 30")
            .spawn()
            .expect("spawn a stand-in for whoever recycled the pid");
        let candidate = HandoffCandidate {
            pid: bystander.id(),
            // The pid the candidate was born at, paired with a birth instant
            // that is not the one the kernel reports for it now.
            birth: Some(super::HandoffCandidateBirth {
                seconds: 1,
                microseconds: 1,
            }),
        };
        assert!(
            handoff_candidate_terminated(candidate),
            "a disagreeing birth stamp proves the pid was reallocated"
        );

        super::signal_handoff_candidate(candidate);
        // SIGKILL delivery is not synchronous with `kill(2)` returning, so give
        // a wrongly-sent signal time to actually land before concluding none was.
        std::thread::sleep(std::time::Duration::from_millis(50));
        let pid = i32::try_from(bystander.id()).expect("bounded child pid");
        // SAFETY: signal 0 is kill(2)'s existence check; it delivers nothing.
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            0,
            "the process that recycled the pid must survive the reap path"
        );

        // SAFETY: SIGKILL to the test's own child.
        assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0, "kill fixture");
        bystander.wait().expect("reap the test child");
    }

    /// Run [`contain_own_process_group`] in a FORKED CHILD and report whether the
    /// child came out leading its own process group.
    ///
    /// The fork is not incidental: `setpgid` is a process-wide, irreversible
    /// change, so calling it on the test binary itself would move `cargo test`
    /// out of the process group whoever launched it may later sweep. The child
    /// answers through its exit status instead.
    ///
    /// `become_session_leader` selects the shape under test, and the child also
    /// checks that a SECOND identical `setpgid(0, 0)` is refused exactly when it
    /// is a session leader. That second call changes nothing (the process already
    /// leads its own group either way); it is a witness of WHICH kernel path the
    /// first call took, which is what makes ignoring the errno legitimate.
    ///
    /// libtest runs tests on a THREAD POOL, so this process is multi-threaded at
    /// fork time. That is safe here only because the child touches nothing but
    /// async-signal-safe calls — `setsid`, the `setpgid`/`getpgrp`/`getpid`
    /// inside `contain_own_process_group`, and `_exit`. It never allocates,
    /// locks, or calls back into std.
    fn forked_child_contains_itself(become_session_leader: bool) -> bool {
        // SAFETY: fork from a multi-threaded harness, with a child that performs
        // async-signal-safe calls only — see the doc comment above.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed");
        if pid == 0 {
            if become_session_leader {
                // SAFETY: async-signal-safe. The child inherited the harness's
                // process group and its own pid is fresh, so it cannot be that
                // group's leader and `setsid` is permitted. Should it fail
                // anyway, the child is not a session leader, the refusal check
                // below disagrees with the requested shape, and the test fails.
                unsafe { libc::setsid() };
            }
            let contained = contain_own_process_group() == ProcessGroupContainment::OwnGroupLeader;
            // SAFETY: async-signal-safe; targets the calling process only.
            let refused = unsafe { libc::setpgid(0, 0) } != 0;
            let as_expected = contained && refused == become_session_leader;
            // SAFETY: async-signal-safe process exit; nothing is unwound and no
            // atexit handler of the harness's may run in this child.
            unsafe { libc::_exit(i32::from(!as_expected)) }
        }
        let mut status = 0i32;
        loop {
            // SAFETY: blocking wait for one exact child pid into a local status slot.
            let waited = unsafe { libc::waitpid(pid, &mut status, 0) };
            if waited == pid {
                break;
            }
            assert!(
                waited < 0
                    && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted,
                "waitpid refused to answer for the forked child"
            );
        }
        libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0
    }

    /// The launch shape with no `pre_exec` hook: a successor that starts inside
    /// somebody else's process group must end up leading its own, or a rejecting
    /// parent's `kill(-pid)` reaches none of the helpers it forks afterwards.
    #[test]
    fn a_successor_started_in_a_foreign_group_contains_itself() {
        assert!(
            forked_child_contains_itself(false),
            "setpgid(0, 0) must have moved the child into a group of its own, and \
             a repeat call must be ACCEPTED because it is not a session leader"
        );
    }

    /// The objection this closes: `setpgid(0, 0)` can be refused, and the refusal
    /// is not a failure to contain. A session leader is the one process the call
    /// refuses, and it already has `pgid == sid == pid` — so the postcondition
    /// this code reads back from the kernel holds on exactly the path where the
    /// return value says it does not.
    #[test]
    fn a_session_leader_is_already_contained_when_the_call_refuses_it() {
        assert!(
            forked_child_contains_itself(true),
            "a session leader must both REFUSE the repeat setpgid and still lead \
             its own process group"
        );
    }
}

/// The returned-completion reducer's ONE remaining piece of attempt identity: the
/// [`crate::native_updater_service::ApplyMode`] the apply was authorized under.
#[cfg(all(test, unix))]
mod returned_handoff_completion_lane_tests {
    use crate::App;
    use crate::native_updater_service::{
        ApplyAttemptTicket, ApplyMode, CheckCompletion, CheckStart, DurableUpdateStatus,
    };

    const TEST_COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

    /// Stage one strictly-newer build through the REAL check reducer, so the
    /// artifact the completion reduces below is the one production would hold.
    fn stage_one_build(app: &mut App) -> u64 {
        let current_build = app.native_updater_service.snapshot().current_build;
        let build = current_build + 1;
        let CheckStart::Start(ticket) = app.native_updater_service.request_check() else {
            panic!("a fresh service must start exactly one check");
        };
        assert_eq!(
            app.native_updater_service.finish_check(
                ticket,
                DurableUpdateStatus {
                    enabled: true,
                    current_build,
                    staged_build: Some(build),
                    staged_version: Some(format!("1.0.{build}")),
                    staged_commit: Some(TEST_COMMIT.to_string()),
                    staged_dmg_sha256: Some("ab".repeat(32)),
                    changelog: None,
                    outcome: "staged".to_string(),
                    failing_checks: 0,
                    failing_persistent: false,
                    failing_kind: String::new(),
                    failing_applies: 0,
                    apply_failure: String::new(),
                    apply_failure_build: 0,
                    apply_failures_for_target: 0,
                    installable: true,
                    channel_unreadable: false,
                },
            ),
            CheckCompletion::Reduced,
            "PRECONDITION: the check must reduce, or nothing is staged"
        );
        build
    }

    /// Drive ONE returned handoff for `mode` end to end through the production
    /// reducer: an authorized attempt is pending, the worker reports `outcome`
    /// against the artifact `digest` names, and the reducer decides what that
    /// costs.
    ///
    /// Everything here is what a real attempt leaves behind, not a hand-built
    /// verdict: the ticket is the reducer's own current apply, the pending record
    /// carries the mode the apply was authorized under, and the completion has
    /// `reconcile: None` exactly as every construction site in this crate emits.
    ///
    /// The OUTCOME is a parameter because it is now load-bearing twice over — it
    /// carries the activity classification AND the physical shape — so a fixture
    /// that pinned it to one kind could only ever exercise one of the budgets.
    ///
    /// So is the DEATH EVIDENCE, for the same reason one level down: `ChildDied`
    /// alone decides nothing now, and a fixture that pinned the evidence to
    /// `Unobserved` could only ever exercise the unexplained budget.
    fn reduce_one_returned_failure(
        app: &mut App,
        mode: ApplyMode,
        build: u64,
        digest: &str,
        outcome: crate::UpdateHandoffOutcome,
        child_death: crate::ChildDeathEvidence,
    ) {
        let ticket = ApplyAttemptTicket::for_test(build, TEST_COMMIT, digest);
        ticket.make_current_apply_for_test(&mut app.native_updater_service);
        let (cancel, _cancelled) = std::sync::mpsc::sync_channel(1);
        app.pending_update_handoff = Some(crate::PendingUpdateHandoff {
            park_at: std::time::Instant::now(),
            proof_ready_at: None,
            activate_at_commit: false,
            attempt_id: 1,
            nonce: None,
            live: Vec::new(),
            adoption: Vec::new(),
            child_pid: None,
            mode,
            apply_attempt: Some(ticket),
            same_image: None,
            target_build: build,
            target_commit: TEST_COMMIT.to_string(),
            layout: crate::restore::RestoreManifest::new(Vec::new()),
            layout_digest: [0; 32],
            screen_digest: [0; 32],
            activity_epoch: app.update_handoff_activity_epoch,
            cancel,
            arbiter: crate::HandoffAttemptArbiter::new(),
            teardown: crate::DeferredHandoffTeardown::None,
            commit_drain_started: None,
            revoked_by_activity: false,
        });
        let teardown = app
            .reduce_returned_handoff_completion(crate::UpdateHandoffCompletion {
                attempt_id: 1,
                nonce: None,
                child_pid: None,
                outcome,
                commit_fd: None,
                reject: None,
                reconcile: None,
                detail: format!("handoff proof ended {outcome:?}"),
                input_drain_spins: 0,
                child_death,
            })
            .expect("a matching pending attempt is always reduced");
        assert_eq!(
            teardown,
            crate::DeferredHandoffTeardown::None,
            "PRECONDITION: this fixture requests no structural teardown, so the \
             replay branch cannot be what changed the state asserted on"
        );
    }

    /// THE FINDING: the completion path took `pending.mode`, used it for the
    /// activity classification, and then dropped it — so a physical failure a
    /// PERSON asked for (Version menu, palette, `aterm-ctl update apply`, an
    /// install-on-clean-quit gesture) was charged to the AUTOMATIC lane's
    /// converging budget and latched automatic apply off on their behalf.
    ///
    /// The two lanes are driven here against the SAME artifact in the SAME `App`,
    /// with identical worker reports, and the contrast is the assertion. A test
    /// that only drove one of them would pass with the mode dropped.
    #[test]
    fn a_person_s_returned_handoff_never_spends_the_automatic_lane_s_budget() {
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let mut app = App::headless_for_test();
        let build = stage_one_build(&mut app);
        assert!(
            app.arm_native_auto_apply(build, &"ab".repeat(32)),
            "PRECONDITION: the background lane is armed for these exact bytes, \
             which is the state a person's click has to leave alone"
        );

        // A PERSON'S FAILURE: nothing is charged. Not the physical budget, not the
        // manual-only latch, and not the live automatic intent — the background
        // lane is still going to try this artifact at its own next window.
        reduce_one_returned_failure(
            &mut app,
            ApplyMode::Immediate,
            build,
            &"ab".repeat(32),
            crate::UpdateHandoffOutcome::TimedOut,
            crate::ChildDeathEvidence::Unobserved,
        );
        assert_eq!(
            app.auto_apply_physical_retry.map(|retry| retry.cycles),
            None,
            "a person-initiated handoff failure must not open (or advance) the \
             automatic artifact's converging physical budget"
        );
        assert!(
            app.auto_apply_manual_only.is_none(),
            "a person's failure must not latch automatic apply off — a manual \
             retry that converges the background lane is the finding"
        );
        assert!(
            app.auto_apply_intent
                .is_some_and(|intent| intent.build == build),
            "and it must not retire the live automatic intent either"
        );

        // THE SAME FAILURE, AUTHORIZED IN THE BACKGROUND: charged in full. This is
        // the discriminator — flip the classification to ignore the mode and the
        // block above starts producing exactly this state.
        reduce_one_returned_failure(
            &mut app,
            ApplyMode::AutomaticPastGrace,
            build,
            &"ab".repeat(32),
            crate::UpdateHandoffOutcome::TimedOut,
            crate::ChildDeathEvidence::Unobserved,
        );
        assert_eq!(
            app.auto_apply_physical_retry.map(|retry| retry.cycles),
            Some(1),
            "the automatic lane's own failure is what the budget is for"
        );
        let latched = app
            .auto_apply_manual_only
            .expect("an automatic physical failure latches manual-only")
            .retry_at
            .expect("with a deadline, on its first failure")
            .saturating_duration_since(std::time::Instant::now());
        assert!(
            latched > std::time::Duration::from_secs(500)
                && latched <= std::time::Duration::from_secs(600),
            "and on the physical schedule's first rung (~600s), got {latched:?}"
        );
        assert!(
            app.auto_apply_intent.is_none(),
            "an automatic physical failure retires the intent it was spent on"
        );
    }

    /// The activity classification must ALSO stay pointed at the background lane:
    /// a person's attempt does not arm the revocation watcher at all, so an
    /// activity-shaped rejection recorded against one can only be noise — and it
    /// must not mint an `arm_activity_revoked_overlap_retry` cycle that re-arms
    /// automatic apply on a schedule nobody asked for.
    #[test]
    fn a_person_s_activity_revoked_completion_arms_no_automatic_retry() {
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let mut app = App::headless_for_test();
        let build = stage_one_build(&mut app);
        let ticket = ApplyAttemptTicket::for_test(build, TEST_COMMIT, &"ab".repeat(32));
        ticket.make_current_apply_for_test(&mut app.native_updater_service);
        let (cancel, _cancelled) = std::sync::mpsc::sync_channel(1);
        app.pending_update_handoff = Some(crate::PendingUpdateHandoff {
            park_at: std::time::Instant::now(),
            proof_ready_at: None,
            activate_at_commit: false,
            attempt_id: 2,
            nonce: None,
            live: Vec::new(),
            adoption: Vec::new(),
            child_pid: None,
            mode: ApplyMode::Immediate,
            apply_attempt: Some(ticket),
            same_image: None,
            target_build: build,
            target_commit: TEST_COMMIT.to_string(),
            layout: crate::restore::RestoreManifest::new(Vec::new()),
            layout_digest: [0; 32],
            screen_digest: [0; 32],
            activity_epoch: app.update_handoff_activity_epoch,
            cancel,
            arbiter: crate::HandoffAttemptArbiter::new(),
            teardown: crate::DeferredHandoffTeardown::None,
            commit_drain_started: None,
            revoked_by_activity: true,
        });
        let _ = app.reduce_returned_handoff_completion(crate::UpdateHandoffCompletion {
            attempt_id: 2,
            nonce: None,
            child_pid: None,
            outcome: crate::UpdateHandoffOutcome::ActivityRevoked,
            commit_fd: None,
            reject: None,
            reconcile: None,
            detail: "activity revoked handoff during physical preparation".to_string(),
            input_drain_spins: 0,
            child_death: crate::ChildDeathEvidence::Unobserved,
        });
        assert!(
            app.auto_overlap_retry.is_none(),
            "a person's revoked attempt must not consume — or create — the \
             automatic artifact's activity-revoked retry budget"
        );
        assert!(
            app.auto_apply_manual_only.is_none(),
            "nor may it latch the background lane off"
        );
    }

    /// THE TYPED KIND MUST REACH THE BUDGET, NOT JUST THE LOG LINE.
    ///
    /// `UpdateHandoffOutcome` distinguishes four physical failures and the
    /// completion path collapsed all of them into one bool on its way to the
    /// schedule, so the budget could not tell "the machine missed a 15 s deadline"
    /// from "these two images cannot agree on an adoption proof" and charged both
    /// the nine-attempt, fourteen-hour transient schedule.
    ///
    /// Driven through `reduce_returned_handoff_completion` — the reduction that
    /// owns the classification — with two artifacts in one `App` so each has its
    /// own budget and the only difference between the two arcs is the worker's
    /// verdict.
    #[test]
    fn a_structural_worker_outcome_reaches_a_different_schedule_from_a_transient_one() {
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let mut app = App::headless_for_test();
        let build = stage_one_build(&mut app);
        let deadline = |app: &App| {
            app.auto_apply_manual_only
                .expect("an automatic physical failure always latches manual-only")
                .retry_at
        };

        // AdoptionMismatch, twice: a confirmation and then the end of the lane for
        // those bytes. `retry_at: None` is what `arm` reads as `SuppressManualOnly`
        // until a strictly newer build ships.
        for _ in 0..2 {
            reduce_one_returned_failure(
                &mut app,
                ApplyMode::AutomaticPastGrace,
                build,
                &"ab".repeat(32),
                crate::UpdateHandoffOutcome::AdoptionMismatch,
                crate::ChildDeathEvidence::Unobserved,
            );
        }
        assert_eq!(
            deadline(&app),
            None,
            "two proofs that these two images disagree is not a busy afternoon; \
             the lane must be finished with the artifact, not scheduling a third \
             park/spawn/paint round trip"
        );

        // TimedOut, twice, same `App` and same build — different bytes, so a
        // different budget. Still scheduled, and on the SECOND rung, which is what
        // proves the two arcs really did diverge rather than one of them silently
        // inheriting the other's counter.
        for _ in 0..2 {
            reduce_one_returned_failure(
                &mut app,
                ApplyMode::AutomaticPastGrace,
                build,
                &"cd".repeat(32),
                crate::UpdateHandoffOutcome::TimedOut,
                crate::ChildDeathEvidence::Unobserved,
            );
        }
        let transient = deadline(&app)
            .expect("a transient failure two attempts in is still coming back")
            .saturating_duration_since(std::time::Instant::now());
        assert!(
            transient > std::time::Duration::from_secs(1700)
                && transient <= std::time::Duration::from_secs(1800),
            "the transient lane must be on its second rung (~1800s), got {transient:?}"
        );
    }

    /// THE WHOLE SEQUENCE OF THE 2026-09-21 PARK-MISS FINDING, driven through
    /// the production reducer: an automatic attempt whose park missed every rung
    /// comes back with the outcome `park_miss_disposition` stands it down with,
    /// and the lane treats it as activity — a live retry intent on the spaced
    /// schedule, no manual-only latch, no physical budget spent, and the ladder's
    /// anchor exactly where it was. Twice, because two of them ten minutes apart
    /// was the pair that used to converge the artifact to `retry_at: None`.
    #[test]
    fn a_park_that_missed_every_rung_is_retried_as_activity_and_never_latches() {
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let mut app = App::headless_for_test();
        let build = stage_one_build(&mut app);
        let armed_at = std::time::Instant::now() - std::time::Duration::from_secs(400);
        app.auto_apply_ladder = Some(crate::AutoApplyLadder {
            build,
            armed_at,
            announced: crate::native_update_auto_intent::ApplyPhase::KeysOnly,
        });
        for miss_pair in 1..=2 {
            let super::ParkMissDisposition::StandDown(stand_down) = super::park_miss_disposition(
                super::PRELAUNCH_MAX_PARK_MISSES,
                "a PTY reader missed the 250 ms handoff park deadline".to_string(),
            ) else {
                panic!("past the last rung the attempt stands down");
            };
            reduce_one_returned_failure(
                &mut app,
                ApplyMode::AutomaticPastGrace,
                build,
                &"ab".repeat(32),
                stand_down.outcome,
                crate::ChildDeathEvidence::Unobserved,
            );
            assert!(
                app.auto_apply_manual_only.is_none(),
                "miss pair {miss_pair}: a busy machine never latches the lane manual-only"
            );
            assert!(
                app.auto_apply_intent.is_some(),
                "miss pair {miss_pair}: the lane keeps a live retry intent"
            );
            assert!(
                app.auto_apply_physical_retry.is_none(),
                "miss pair {miss_pair}: no physical budget is spent on a stopwatch"
            );
            assert_eq!(
                app.auto_apply_ladder.map(|ladder| ladder.armed_at),
                Some(armed_at),
                "miss pair {miss_pair}: the ladder's anchor is untouched"
            );
        }
        assert_eq!(
            app.auto_overlap_retry.map(|retry| retry.cycles),
            Some(2),
            "each stood-down attempt costs one rung of the ACTIVITY spacing, which \
             saturates and never ends the lane"
        );
    }

    /// The classification itself, stated as a table so a future outcome variant
    /// has to be placed deliberately rather than falling into whichever arm the
    /// compiler allows. Three axes now, and every one of them used to be lossy:
    /// WHO asked (the mode), WHAT happened (the worker's typed outcome), and — for
    /// the one outcome that is not itself a classification — WHAT THE WORKER SAW.
    ///
    /// The whole table is driven with `Unobserved` evidence, which is the honest
    /// default for every outcome except `ChildDied` (no candidate died, or this
    /// process is the one that ended it). `ChildDied` therefore lands in
    /// `Unexplained` HERE, and its evidence-bearing arms are the table below.
    #[test]
    fn every_handoff_outcome_lands_in_the_lane_its_evidence_earns() {
        use crate::ChildDeathEvidence as Death;
        use crate::UpdateHandoffOutcome as Outcome;
        use crate::app_native::{HandoffFailureLane as Lane, PhysicalFailureShape as Shape};

        for (outcome, expected) in [
            (Outcome::AdoptionMismatch, Lane::Physical(Shape::Structural)),
            (
                Outcome::PreparationFailed,
                Lane::Physical(Shape::Structural),
            ),
            // NOT `Structural` any more, and not `Transient` either: with nothing
            // observed, proof EOF is a failure whose KIND is unknown.
            (Outcome::ChildDied, Lane::Physical(Shape::Unexplained)),
            (Outcome::TimedOut, Lane::Physical(Shape::Transient)),
            (Outcome::Rejected, Lane::Physical(Shape::Transient)),
            (Outcome::ActivityRevoked, Lane::ActivityRevoked),
            // Unreachable as a FAILURE (the commit path handles it), and it fails
            // closed to the forgiving shape rather than converging an artifact on
            // a state nobody understands.
            (Outcome::ProofReady, Lane::Physical(Shape::Transient)),
        ] {
            assert_eq!(
                Lane::classify(
                    ApplyMode::AutomaticPastGrace,
                    outcome,
                    Death::Unobserved,
                    false
                ),
                expected,
                "{outcome:?} in the background lane"
            );
            // A PERSON'S APPLY CHARGES NOTHING, whatever the worker reported: the
            // shape decides how much an AUTOMATIC failure costs, never whether a
            // human's click may converge the background lane.
            assert_eq!(
                Lane::classify(ApplyMode::Immediate, outcome, Death::Unobserved, false),
                Lane::Manual,
                "{outcome:?} from a person"
            );
            // The main thread's own activity-shaped rejection is the other half of
            // the activity observation and dominates the physical shape: a lossless
            // rollback the terminal caused is not evidence about the artifact.
            assert_eq!(
                Lane::classify(
                    ApplyMode::AutomaticPastGrace,
                    outcome,
                    Death::Unobserved,
                    true
                ),
                Lane::ActivityRevoked,
                "{outcome:?} with a main-thread activity revocation"
            );
        }
    }

    /// A STARVED CHILD RETRIES. The field defect, stated as the smallest thing
    /// that has to be true.
    ///
    /// 2026-08-21, owner's desk: build 1787699398 staged, verified,
    /// `relaunch_ready=true`, and twice `apply_failure = "overlap handoff failed
    /// safely: handoff proof ended ChildDied"` — recorded while the machine carried
    /// a load average of 140-160. When that load stopped, THE SAME BUILDS applied
    /// with no intervention. The old classification charged `ChildDied` the
    /// structural budget, so by the second failure the automatic lane was finished
    /// with those bytes and the user was left on "staged, applies on relaunch" —
    /// the exact state the seamless lane exists to delete — for a machine that was
    /// merely busy.
    ///
    /// `SIGKILL` is what that looks like when the parent CAN see it (macOS jetsam
    /// reclaiming memory; no process raises it on itself), so this drives the
    /// structural budget's worth of them and asserts the lane is still coming back.
    /// Then it spends the rest, because a lane that retried forever would be the
    /// opposite defect.
    #[test]
    fn a_starved_child_died_keeps_retrying_past_the_budget_a_refusal_would_spend() {
        use crate::app_native::{
            PHYSICAL_FAILURE_LIFETIME_ATTEMPTS, STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS,
        };
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let mut app = App::headless_for_test();
        let build = stage_one_build(&mut app);
        let starved = crate::ChildDeathEvidence::Signalled {
            signal: libc::SIGKILL,
        };
        let deadline = |app: &App| {
            app.auto_apply_manual_only
                .expect("an automatic physical failure always latches manual-only")
                .retry_at
        };

        for _ in 0..STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS {
            reduce_one_returned_failure(
                &mut app,
                ApplyMode::AutomaticPastGrace,
                build,
                &"ab".repeat(32),
                crate::UpdateHandoffOutcome::ChildDied,
                starved,
            );
        }
        let still_coming = deadline(&app)
            .expect(
                "THE REGRESSION: a machine that killed our candidate has said \
                 nothing about the new bytes, so the automatic lane must still be \
                 scheduled — `None` here is the converged latch the old \
                 unconditional Structural classification minted after two of these",
            )
            .saturating_duration_since(std::time::Instant::now());
        assert!(
            still_coming > std::time::Duration::from_secs(1700)
                && still_coming <= std::time::Duration::from_secs(1800),
            "and it rides the epoch schedule's second rung (~1800s), which is what \
             proves it took the machine-shaped lane rather than a shortened one: \
             got {still_coming:?}"
        );

        // BOUNDED ALL THE SAME. Spend the rest of the transient lifetime and the
        // answer becomes the deadline-less latch: nine failures across three
        // independent epochs is evidence about the artifact however each one died.
        for _ in STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS..PHYSICAL_FAILURE_LIFETIME_ATTEMPTS {
            reduce_one_returned_failure(
                &mut app,
                ApplyMode::AutomaticPastGrace,
                build,
                &"ab".repeat(32),
                crate::UpdateHandoffOutcome::ChildDied,
                starved,
            );
        }
        assert_eq!(
            deadline(&app),
            None,
            "a lane that retries a starved child forever is the opposite defect; \
             the transient budget still ends"
        );
    }

    /// A SUCCESSOR THAT REFUSES CONVERGES, on the structural budget, and the FIRST
    /// failure is not what converges it.
    ///
    /// `Exited { code: 0 }` is the refusal this tree actually produces: a candidate
    /// whose boot apply could not swap stays the OLD build, refuses the authorized
    /// target identity, closes every adopted master and RETURNS from `main_entry`
    /// — a clean exit that the parent sees only as proof EOF. Five field failures
    /// in 2026-08 were exactly that. Reaching an `exit` instruction is what
    /// separates it from the starved child above: a process that never ran decides
    /// nothing.
    #[test]
    fn a_refusing_child_died_converges_to_manual_only_within_its_budget() {
        use crate::app_native::STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS;
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let mut app = App::headless_for_test();
        let build = stage_one_build(&mut app);
        let refused = crate::ChildDeathEvidence::Exited { code: 0 };
        let deadline = |app: &App| {
            app.auto_apply_manual_only
                .expect("an automatic physical failure always latches manual-only")
                .retry_at
        };

        reduce_one_returned_failure(
            &mut app,
            ApplyMode::AutomaticPastGrace,
            build,
            &"ab".repeat(32),
            crate::UpdateHandoffOutcome::ChildDied,
            refused,
        );
        let confirming = deadline(&app)
            .expect("ONE unlucky handoff never converges a lane, whatever it said")
            .saturating_duration_since(std::time::Instant::now());
        assert!(
            confirming > std::time::Duration::from_secs(500)
                && confirming <= std::time::Duration::from_secs(600),
            "the confirming retry is the schedule's first rung (~600s), got \
             {confirming:?}"
        );

        for _ in 1..STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS {
            reduce_one_returned_failure(
                &mut app,
                ApplyMode::AutomaticPastGrace,
                build,
                &"ab".repeat(32),
                crate::UpdateHandoffOutcome::ChildDied,
                refused,
            );
        }
        assert_eq!(
            deadline(&app),
            None,
            "twice told that this successor will not become our successor is not a \
             busy afternoon: the lane is done with these bytes, and `retry_at: \
             None` is what `arm` reads as SuppressManualOnly"
        );
    }

    /// AND THE ONE IN THE MIDDLE — a death nobody witnessed, which is where a
    /// `ChildDied` lands whenever the evidence runs out
    /// ([`crate::ChildDeathEvidence::Unobserved`]). Reachable on both lanes and
    /// still the commonest answer on the shipping macOS one: a successor that
    /// refuses closes the readiness channel and keeps running, so the pre-kill
    /// look often finds it alive and the status that comes back afterwards is this
    /// process's own SIGKILL, which claims nothing.
    ///
    /// That verdict must be weaker than both of the ones above: it converges, but
    /// only after a genuinely independent sample of the machine. The stand-down at
    /// the end of the first epoch is that sample, and it is what the field case
    /// needed — the retry that finally worked came HOURS later, not at the 600 s
    /// and 1800 s rungs that re-ran the same pathological hour.
    #[test]
    fn an_unexplained_child_died_gets_a_second_epoch_and_then_stops() {
        use crate::app_native::{
            PHYSICAL_FAILURE_LIFETIME_ATTEMPTS, PHYSICAL_FAILURES_PER_EPOCH,
            UNEXPLAINED_FAILURE_LIFETIME_ATTEMPTS,
        };
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let mut app = App::headless_for_test();
        let build = stage_one_build(&mut app);
        let deadline = |app: &App| {
            app.auto_apply_manual_only
                .expect("an automatic physical failure always latches manual-only")
                .retry_at
        };
        let spend = |app: &mut App, times: u8| {
            for _ in 0..times {
                reduce_one_returned_failure(
                    app,
                    ApplyMode::AutomaticPastGrace,
                    build,
                    &"ab".repeat(32),
                    crate::UpdateHandoffOutcome::ChildDied,
                    crate::ChildDeathEvidence::Unobserved,
                );
            }
        };

        spend(&mut app, PHYSICAL_FAILURES_PER_EPOCH);
        let stand_down = deadline(&app)
            .expect("the first epoch ends in a stand-down, not in convergence")
            .saturating_duration_since(std::time::Instant::now());
        assert!(
            stand_down > std::time::Duration::from_secs(5 * 60 * 60),
            "and the stand-down is hours, because re-sampling the SAME loaded hour \
             is what the in-epoch rungs already did: got {stand_down:?}"
        );

        spend(
            &mut app,
            UNEXPLAINED_FAILURE_LIFETIME_ATTEMPTS - PHYSICAL_FAILURES_PER_EPOCH,
        );
        assert_eq!(
            deadline(&app),
            None,
            "two independent samples of the machine is where 'the machine was \
             having a bad hour' stops being a credible explanation — and \
             {UNEXPLAINED_FAILURE_LIFETIME_ATTEMPTS} attempts is strictly under \
             the {PHYSICAL_FAILURE_LIFETIME_ATTEMPTS} a named transient failure \
             gets, which the compile-time assert beside the constants pins"
        );
    }

    /// THE EVIDENCE AXIS, as its own total table: every `ChildDeathEvidence` a
    /// worker can produce, and the shape it earns.
    ///
    /// THE FINDING: `ChildDied` was `Structural` unconditionally, on the argument
    /// that a candidate which exits before writing its readiness proof "is the
    /// successor image refusing to boot as a successor … the strongest statement
    /// about the new bytes available at this seam". A child starved of CPU exits
    /// before writing a readiness proof too — it never ran — and the owner's desk
    /// produced exactly that on 2026-08-21: two `ChildDied` applies under a load
    /// average of 140-160, then the SAME builds applying with no intervention once
    /// the load stopped. The verdict was about the machine and was charged to the
    /// bytes.
    ///
    /// Every arm below is a fact the worker OBSERVED, never a duration threshold
    /// and never a message string.
    #[test]
    fn a_dead_candidate_is_classified_by_what_the_worker_saw() {
        use crate::ChildDeathEvidence as Death;
        use crate::UpdateHandoffOutcome as Outcome;
        use crate::app_native::{HandoffFailureLane as Lane, PhysicalFailureShape as Shape};

        for (death, expected, why) in [
            (
                Death::Unobserved,
                Shape::Unexplained,
                "no witnessed status, or one that could only have been our own \
                 SIGKILL — and a verdict nobody witnessed must not be filed as \
                 one somebody did",
            ),
            (
                Death::Exited { code: 0 },
                Shape::Structural,
                "a clean exit is this tree's COMMONEST refusal — `main_entry` \
                 returns without a window when the overlap authority is \
                 incomplete — and reaching an exit instruction at all is proof \
                 the image ran",
            ),
            (
                Death::Exited { code: 74 },
                Shape::Structural,
                "the fail-stop exit a candidate takes when it can never become \
                 authoritative; same reading, which is why the CODE is recorded \
                 and not judged",
            ),
            (
                Death::Signalled {
                    signal: libc::SIGSEGV,
                },
                Shape::Structural,
                "a fault is the image executing itself into a wall, which is the \
                 bytes",
            ),
            (
                Death::Signalled {
                    signal: libc::SIGABRT,
                },
                Shape::Structural,
                "so is an abort",
            ),
            (
                Death::Signalled {
                    signal: libc::SIGKILL,
                },
                Shape::Transient,
                "THE FIELD CASE: no process raises SIGKILL on itself, and macOS \
                 jetsam sends it to reclaim memory. The next attempt on a calmer \
                 machine is the one that wins",
            ),
            (
                Death::Signalled {
                    signal: libc::SIGTERM,
                },
                Shape::Transient,
                "and any other signal from outside the image is equally not a \
                 statement about the bytes",
            ),
        ] {
            assert_eq!(
                Lane::classify(
                    ApplyMode::AutomaticPastGrace,
                    Outcome::ChildDied,
                    death,
                    false
                ),
                Lane::Physical(expected),
                "{death:?}: {why}"
            );
            // THE EVIDENCE IS READ FOR `ChildDied` AND FOR NOTHING ELSE. Every
            // other outcome describes a candidate THIS process ended, so its exit
            // status is our SIGKILL and says nothing about the artifact.
            assert_eq!(
                Lane::classify(
                    ApplyMode::AutomaticPastGrace,
                    Outcome::TimedOut,
                    death,
                    false
                ),
                Lane::Physical(Shape::Transient),
                "{death:?} must not move a TimedOut, whose candidate we killed"
            );
        }
    }
}

/// THE LANE CHOICE, and every reason it falls back.
///
/// This is the decision that says whether the user's next update produces a
/// survivor with a launchd application job or another pid-1 orphan, and it is
/// made from facts that are individually cheap and collectively easy to get
/// wrong. Each test below is one field of [`HandoffLaneFacts`] flipped against
/// an otherwise-eligible attempt, because a predicate that ignored a field would
/// otherwise pass every test written against the happy path.
#[cfg(all(test, target_os = "macos"))]
mod handoff_lane_tests {
    use super::{HandoffLaneFacts, app_bundle_root, launch_environment, out_of_band_lane_refusal};

    /// An attempt with nothing wrong with it.
    fn eligible() -> HandoffLaneFacts {
        HandoffLaneFacts {
            bundled: true,
            launcher_available: true,
            socket_path_fits: true,
            target_not_older: true,
            sessions: 3,
            environment_is_a_merge: true,
        }
    }

    #[test]
    fn an_eligible_attempt_takes_the_out_of_band_lane() {
        assert_eq!(out_of_band_lane_refusal(eligible()), None);
    }

    /// A dev build, `cargo run`, and the test harness all reach the seamless
    /// lane; none of them is a bundle, and LaunchServices has nothing to start.
    #[test]
    fn an_unbundled_process_forks() {
        let facts = HandoffLaneFacts {
            bundled: false,
            ..eligible()
        };
        assert!(out_of_band_lane_refusal(facts).is_some());
    }

    /// THE 16 BYTES. `$HOME` decides whether the rendezvous path fits
    /// `sun_path`, so on some perfectly ordinary machines this lane simply does
    /// not exist — and the fallback has to be silent and total rather than a
    /// bind that fails after the terminal has parked.
    #[test]
    fn a_rendezvous_path_that_does_not_fit_forks() {
        let facts = HandoffLaneFacts {
            socket_path_fits: false,
            ..eligible()
        };
        assert!(out_of_band_lane_refusal(facts).is_some());
    }

    /// THE DOWNGRADE GUARD the retired version advertisement carried. "Presence
    /// of the transport IS the version" closes old-parent/new-child and says
    /// NOTHING about new-parent/old-child — so an older authorized target must
    /// fork, or a successor with no rendezvous code is handed descriptors it
    /// cannot receive and every session is lost to a hang.
    #[test]
    fn an_older_authorized_target_forks() {
        let facts = HandoffLaneFacts {
            target_not_older: false,
            ..eligible()
        };
        assert!(out_of_band_lane_refusal(facts).is_some());
    }

    /// The transport, not the protocol, bounds the pool: one `SCM_RIGHTS`
    /// message carries 64 descriptors and two of them are the pipes.
    #[test]
    fn a_pool_too_wide_for_one_message_forks() {
        let limit = crate::handoff_rendezvous::MAX_RENDEZVOUS_SESSIONS;
        assert_eq!(
            out_of_band_lane_refusal(HandoffLaneFacts {
                sessions: limit,
                ..eligible()
            }),
            None,
            "exactly the limit still fits"
        );
        assert!(
            out_of_band_lane_refusal(HandoffLaneFacts {
                sessions: limit + 1,
                ..eligible()
            })
            .is_some(),
            "one more does not, and must fall back rather than fail at sendmsg"
        );
        assert!(
            out_of_band_lane_refusal(HandoffLaneFacts {
                sessions: 0,
                ..eligible()
            })
            .is_some(),
            "an overlap with no sessions is not an overlap"
        );
    }

    /// A LaunchServices launch MERGES its environment and cannot remove a
    /// variable. `bind_expected_update_artifact` removes three, and a successor
    /// that inherited a stale `ATERM_UPDATE_EXPECTED_*` would authenticate its
    /// staged bundle against the wrong artifact — so a removal that would
    /// actually remove something disqualifies the lane.
    #[test]
    fn an_environment_that_needs_a_removal_forks() {
        let facts = HandoffLaneFacts {
            environment_is_a_merge: false,
            ..eligible()
        };
        assert!(out_of_band_lane_refusal(facts).is_some());
    }

    /// The predicate that produces that fact, against a real `Command`: a
    /// removal of something this process does not have is a no-op (which is the
    /// common case, since `env_remove` is called unconditionally), and a removal
    /// of something it DOES have is what cannot be expressed.
    #[test]
    fn only_a_removal_that_would_remove_something_disqualifies_the_launch() {
        let mut command = std::process::Command::new("/bin/echo");
        command.env("ATERM_LANE_TEST_KEY", "value");
        command.env_remove("ATERM_LANE_TEST_ABSENT_KEY_THAT_IS_NOT_SET");
        let carried = launch_environment(&command).expect("a no-op removal is expressible");
        assert_eq!(
            carried,
            vec![(
                std::ffi::OsString::from("ATERM_LANE_TEST_KEY"),
                std::ffi::OsString::from("value")
            )],
            "only real assignments travel; a vacuous removal contributes nothing"
        );

        // The removal now names something this process really carries. Scoped
        // through the workspace's one lock-scoped env helper so no concurrent
        // test observes the mutation.
        aterm_log::env::scoped("ATERM_LANE_TEST_PRESENT_KEY", "set", || {
            let mut command = std::process::Command::new("/bin/echo");
            command.env_remove("ATERM_LANE_TEST_PRESENT_KEY");
            assert!(
                launch_environment(&command).is_none(),
                "a merge cannot un-set a variable the launcher's own environment has"
            );
        });
    }

    /// The bundle root is `<bundle>.app/Contents/MacOS/<bin>` and the `.app`
    /// suffix is CHECKED — a dev binary also sits three levels below something.
    #[test]
    fn only_a_dot_app_three_levels_up_is_a_bundle_root() {
        let temp = std::env::temp_dir().join(format!("aterm-lane-{}", std::process::id()));
        let bundle = temp.join("aterm.app");
        let macos = bundle.join("Contents/MacOS");
        std::fs::create_dir_all(&macos).expect("fixture bundle");
        assert_eq!(
            app_bundle_root(&macos.join("aterm")).as_deref(),
            Some(bundle.as_path())
        );

        let plain = temp.join("target/debug/deps");
        std::fs::create_dir_all(&plain).expect("fixture dev tree");
        assert_eq!(
            app_bundle_root(&plain.join("aterm-gui")),
            None,
            "a dev build is three levels below a directory too, and is not a bundle"
        );
        assert_eq!(
            app_bundle_root(std::path::Path::new("aterm")),
            None,
            "and a bare name has no three levels at all"
        );
        let _ = std::fs::remove_dir_all(&temp);
    }
}

/// TIER-1 CONFORMANCE for the seamless handoff's PER-SESSION ownership model
/// ([`aterm_spec::derive::native_update_seamless_handoff_ownership_model`]).
///
/// A model that is green with no bind proves a property of the DESCRIPTION of
/// the handoff, not of the handoff. These tests drive the GENUINE decision
/// function out of this module over its whole bounded input space and check
/// that what the shipping code decides is exactly what the model admits.
///
/// IT LIVES HERE, INSIDE THE MODULE IT BINDS, on purpose: as a sibling file it
/// needed ten private items widened to `pub(crate)` — a production diff whose
/// only consumer was a test, which is exactly how a private seam stops being
/// private. A child module sees its parent's privates for free.
///
/// WHAT IS BOUND, and what is not, because that distinction is the whole value.
/// Bound: the final Commit admission — the predicate that decides whether an
/// attempt may transfer ownership at all. Modelled but NOT bound: the physical
/// surface those decisions gate (the ownership transfer and `_exit` inside
/// `seamless::commit_and_exit`, the reader park/resume, the `F_DUPFD_CLOEXEC`
/// duplication), because each either replaces this process or needs a live
/// event loop with real sessions. Those are the QA-seam tests' job.
///
/// Three further binds were written and DELETED rather than shipped: their model
/// assertions were loop-invariant constants sitting beside the real-code
/// assertions, which reads as conformance and is not — the model half asserted
/// the same thing on every iteration regardless of what the code answered. One
/// honest bind is worth more than four that look thorough.
#[cfg(all(test, unix))]
mod ownership_conformance {
    use super::{HandoffCommitFacts, handoff_commit_admitted, handoff_rejection_activity_shaped};
    use aterm_spec::derive::{Model, native_update_seamless_handoff_ownership_model};
    use aterm_spec::interp::{State, admits, with_buggy};

    const TO_DESCRIPTORS_TRANSFERRED: [&str; 4] = [
        "ParkOutgoingReaders",
        "CaptureCheckpoints",
        "StartReaderlessCandidate",
        "DuplicateDescriptorsToCandidate",
    ];

    /// The same prefix plus the proof, i.e. the model's `ProofMatched`.
    const TO_PROOF_MATCHED: [&str; 5] = [
        "ParkOutgoingReaders",
        "CaptureCheckpoints",
        "StartReaderlessCandidate",
        "DuplicateDescriptorsToCandidate",
        "MatchAdoptionProof",
    ];

    /// Fire a deterministic sequence, checking at every step that the model admits
    /// exactly the named action — never that it admits *something*.
    fn walk(model: &Model, actions: &[&'static str]) -> State {
        let mut state = model.init_state();
        for &action in actions {
            let next = model.successors(action, &state);
            assert_eq!(
                next.len(),
                1,
                "{action} must be deterministically enabled at {state:?}"
            );
            assert_eq!(admits(model, &state, &next[0]), Some(action));
            state = next[0].clone();
        }
        state
    }

    fn assert_every_invariant_holds(model: &Model, state: &State) {
        for invariant in &model.invariants {
            assert!(
                model.check_invariant(invariant.name, state),
                "state violates {}::{}: {state:?}",
                model.name,
                invariant.name,
            );
        }
    }

    /// Every reachable state, by BFS over the same successor relation the
    /// interpreter's own bounded model check uses. Small by construction (two
    /// sessions, and the protocol is a chain), so the bound is a regression
    /// tripwire rather than a real limit.
    fn reachable(model: &Model) -> Vec<State> {
        let mut seen: std::collections::BTreeSet<State> = std::collections::BTreeSet::new();
        let mut queue: std::collections::VecDeque<State> = std::collections::VecDeque::new();
        let init = model.init_state();
        seen.insert(init.clone());
        queue.push_back(init);
        while let Some(state) = queue.pop_front() {
            for action in &model.actions {
                for next in model.successors(action.name, &state) {
                    if seen.insert(next.clone()) {
                        queue.push_back(next);
                    }
                }
            }
        }
        assert!(
            seen.len() < 20_000,
            "the ownership model's bounded space regressed to {} states",
            seen.len()
        );
        seen.into_iter().collect()
    }

    /// Is a state satisfying `goal` reachable from `from`?
    fn reaches(model: &Model, from: &State, goal: impl Fn(&State) -> bool) -> bool {
        let mut seen: std::collections::BTreeSet<State> = std::collections::BTreeSet::new();
        let mut queue: std::collections::VecDeque<State> = std::collections::VecDeque::new();
        seen.insert(from.clone());
        queue.push_back(from.clone());
        while let Some(state) = queue.pop_front() {
            if goal(&state) {
                return true;
            }
            for action in &model.actions {
                for next in model.successors(action.name, &state) {
                    if seen.insert(next.clone()) {
                        queue.push_back(next);
                    }
                }
            }
        }
        false
    }

    fn facts_from_bits(bits: u32) -> HandoffCommitFacts {
        HandoffCommitFacts {
            exact_sessions: bits & (1 << 0) != 0,
            exact_layout: bits & (1 << 1) != 0,
            exact_activity: bits & (1 << 2) != 0,
            teardown_allows_commit: bits & (1 << 3) != 0,
            parent_still_parked: bits & (1 << 4) != 0,
            sessions_alive: bits & (1 << 5) != 0,
            input_dispatch_fenced: bits & (1 << 6) != 0,
            egress_settled: bits & (1 << 7) != 0,
            native_safe: bits & (1 << 8) != 0,
            proof_exact: bits & (1 << 9) != 0,
            commit_channel: bits & (1 << 10) != 0,
        }
    }

    /// The model's `revoked` is "some mutable pre-Commit admission fact turned
    /// false". `parent_still_parked` and `proof_exact` are deliberately NOT folded
    /// into it: the model carries those two as variables of their own
    /// (`out_readers`, `proof_matched`) because they are the two facts the ownership
    /// invariants are actually about — who is reading, and what licensed a transfer.
    fn model_revoked(facts: HandoffCommitFacts) -> bool {
        !(facts.exact_sessions
            && facts.exact_layout
            && facts.exact_activity
            && facts.teardown_allows_commit
            && facts.sessions_alive
            && facts.input_dispatch_fenced
            && facts.egress_settled
            && facts.native_safe
            && facts.commit_channel)
    }

    /// Project one bounded fact combination onto the model's pre-Commit state.
    ///
    /// A projection can express states the healthy model proves unreachable — a
    /// parent reader that resumed mid-attempt is exactly one — and that is the
    /// point: the bind then checks that the shipping admission and the model's guard
    /// refuse the SAME combinations.
    fn project_commit_facts(
        transferred: &State,
        matched: &State,
        facts: HandoffCommitFacts,
    ) -> State {
        let mut state = if facts.proof_exact {
            matched.clone()
        } else {
            transferred.clone()
        };
        state.insert("revoked", i64::from(model_revoked(facts)));
        state.insert("out_readers", i64::from(!facts.parent_still_parked));
        state
    }

    /// THE FINAL ADMISSION, exhaustively. `handoff_commit_admitted` is the compiled
    /// conjunction standing immediately before the attempt-wide Commit CAS, so it —
    /// and nothing in this file — decides whether a session's ownership moves. All
    /// 2^11 of its bounded fact combinations are driven through it and through the
    /// model's `CommitAtomically` guard, and the two must agree combination for
    /// combination.
    #[test]
    fn real_commit_admission_conforms_to_the_ownership_model_over_every_bounded_fact_combination() {
        let model = native_update_seamless_handoff_ownership_model();
        let transferred = walk(&model, &TO_DESCRIPTORS_TRANSFERRED);
        let matched = walk(&model, &TO_PROOF_MATCHED);
        let mut admitted_combinations = 0usize;
        let mut refused_parked: std::collections::BTreeSet<State> =
            std::collections::BTreeSet::new();

        for bits in 0..(1u32 << 11) {
            let facts = facts_from_bits(bits);
            let before = project_commit_facts(&transferred, &matched, facts);
            let admitted = handoff_commit_admitted(facts);
            let successors = model.successors("CommitAtomically", &before);
            assert_eq!(
                admitted,
                !successors.is_empty(),
                "the shipping admission and the model's Commit guard disagree for {facts:?}"
            );

            // An activity-shaped rejection is a REFUSAL first and a retry
            // classification second: it may never coexist with an admitted Commit,
            // or the automatic lane would spend budget re-attempting a handoff that
            // already landed.
            if handoff_rejection_activity_shaped(facts) {
                assert!(
                    !admitted,
                    "an activity-shaped rejection cannot also be admitted: {facts:?}"
                );
            }

            if !admitted {
                // The parent's own readers are the one fact that does not describe
                // the attempt's rollback prospects: once a reader has resumed, that
                // axis of the rollback has already happened. Every refusal with the
                // readers still parked must have a path back to Resumed.
                if facts.parent_still_parked {
                    refused_parked.insert(before);
                }
                continue;
            }

            admitted_combinations += 1;
            let after = successors[0].clone();
            assert_eq!(admits(&model, &before, &after), Some("CommitAtomically"));
            assert_every_invariant_holds(&model, &after);
            assert_eq!(after["commits"], 1);
            assert_eq!(after["owner_a"], 2, "session a moved to the candidate");
            assert_eq!(after["owner_b"], 2, "session b moved to the candidate");
            assert_eq!(after["out_live"], 0, "the outgoing process exited");
            assert_eq!(
                after["cand_readers"], 0,
                "the candidate's reader gate opens after Commit, never with it"
            );
        }

        // ROLLBACK IS AVAILABLE FOR EVERY REFUSAL — genuine or activity-shaped, and
        // whichever fact turned false. Checked once per DISTINCT projected state
        // rather than once per fact combination; the projection is many-to-one.
        assert!(refused_parked.len() > 1, "vacuous refusal projection");
        for state in &refused_parked {
            assert!(
                reaches(&model, state, |candidate| {
                    candidate["phase"] == 7
                        && candidate["out_live"] == 1
                        && candidate["owner_a"] == 1
                        && candidate["owner_b"] == 1
                        && candidate["commits"] == 0
                }),
                "no rollback path to Resumed from the refused state {state:?}"
            );
        }

        // Non-vacuity: the compiled conjunction admits EXACTLY the
        // all-facts-hold combination, so the sweep above is not passing because
        // everything happens to be refused.
        assert_eq!(
            admitted_combinations, 1,
            "exactly one bounded combination may authorize a Commit"
        );
    }

    /// THE READINESS WAIT'S VERDICT. `classify_ready_poll` is the compiled function
    /// that turns one `poll` answer into "the adopted set is stale", "read the
    /// proof", or "yield". Exhausted over every `revents` combination for the proof
    /// fd and a two-master pool — the same pool cardinality the model carries.
    #[test]
    fn every_pre_commit_state_has_a_path_back_to_resumed_with_both_sessions_still_ours() {
        let model = native_update_seamless_handoff_ownership_model();
        let states = reachable(&model);
        assert!(states.len() > 10, "vacuous reachable space");

        let (mut pre_commit, mut committed, mut settled) = (0usize, 0usize, 0usize);
        for state in &states {
            assert_every_invariant_holds(&model, state);
            if state["commits"] > 0 {
                committed += 1;
                continue;
            }
            if state["phase"] == 7 || state["phase"] == 8 {
                settled += 1;
                continue;
            }
            if state["phase"] == 0 {
                continue;
            }
            pre_commit += 1;
            assert!(
                reaches(&model, state, |candidate| {
                    candidate["phase"] == 7
                        && candidate["out_live"] == 1
                        && candidate["out_readers"] == 1
                        && candidate["owner_a"] == 1
                        && candidate["owner_b"] == 1
                        && candidate["commits"] == 0
                }),
                "no rollback path to Resumed from {state:?}"
            );
        }
        assert!(
            pre_commit > 0 && committed > 0 && settled > 0,
            "the sweep must see all three lanes: {pre_commit} pre-commit, \
             {committed} committed, {settled} settled"
        );

        let buggy = with_buggy(&model, 1);
        let broken = reachable(&buggy);
        assert!(
            broken
                .iter()
                .any(|state| !buggy.check_invariant("NoSessionIsEverOrphaned", state)),
            "the mutant must be able to orphan a session"
        );
        assert!(
            broken
                .iter()
                .any(|state| !buggy.check_invariant("NeverTwoReadersOnOneMaster", state)),
            "the mutant must be able to put two readers on one master"
        );
    }
}
