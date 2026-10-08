// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tier-1 for `NativeUpdateRendezvousAdmission` (aterm-spec's
//! `native_update_rendezvous_admission_model`; round eight of the update audit,
//! U36 and its review): the outgoing process grants the launched lane's
//! descriptors only to a dialer whose CODE is the successor's, commits to none
//! LaunchServices disowns, holds a dialer whose code check macOS's code-signing
//! service does not answer instead of turning it away, and files a successor it
//! did turn away by the refusal's cause.
//!
//! Driven for real: the rendezvous gate (`Rendezvous::accept_claim`, through
//! `gate_one`) against dialers that are OTHER PROCESSES — this test binary run as
//! a stand-in, which copied the claim exactly as a same-uid process reading the
//! successor's launch environment (`KERN_PROCARGS2`) would, and which meets one
//! code requirement and not another; the requirement the shipping lane states,
//! read off the job `prelaunch_out_of_band_handoff` builds for each kind of
//! attempt (a fresh cached pass among them); the production tail from the dial
//! wait to the grant (`await_launched_dial`, then `ClaimedPeer::transfer`); the
//! hold, through the process's one `trustd` gate made overdue, in a child of its
//! own; the filing of a turned-away successor (`code_refused_completion`, booked
//! by `HandoffFailureLane::classify`); and the corroboration after the dial
//! (`LaunchCorroboration::poll`) against live, exited-and-unreaped and reaped
//! processes. LaunchServices is scripted, with the answers it was measured to
//! give (`LaunchError::ExitedBeforeCheckIn`), because a unit test may not launch
//! an application (AGENTS.md rule 5).
//!
//! Negative controls: the gate at the same-uid floor — the launched lane before
//! 2026-10-02, the secret alone — run for real, grants the stranger every
//! descriptor, and the model's `GrantOnSecretAlone` reaches the state its
//! invariant refuses; the corroboration's 2026-09-29 reading of an exit answer is
//! `CommitOverDisowning`; the first code gate's closing of an unanswered check is
//! `RefuseUnanswered`; and its filing of the turned-away successor as a death
//! before its dial, booked for real, is the unexplained shape the model's
//! `FileAsUnexplainedDeath` reaches — each refused by its invariant.

use super::{
    HandoffCandidate, LaunchCorroboration, LaunchedDialEnd, PreDialDeath, await_launched_dial,
    code_refused_completion, code_refused_dialer_was_launched, launched_dialer_code,
};
use crate::app_launch_successor::{LaunchError, LaunchInFlight};
use crate::app_native::{HandoffFailureLane, PhysicalFailureShape};
use crate::handoff_rendezvous::{
    CodeRefusal, CodeRefusedDialer, CodeRefusedEnd, DialerCode, Rendezvous, RendezvousError,
    TRUSTD_SILENT, dial_for_test, held_for_trustd,
};
use crate::native_updater_service::{ApplyAttemptTicket, ApplyMode};
use aterm_spec::derive::{Model, native_update_rendezvous_admission_model};
use aterm_spec::interp::{State, with_buggy};
use std::cell::RefCell;
use std::os::fd::{AsFd as _, FromRawFd as _, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender;
use std::time::{Duration, Instant};

/// The stand-in's rendezvous path and the claim it presents, in its own
/// environment.
const STRANGER_PATH: &str = "ATERM_TEST_STRANGER_RENDEZVOUS";
const STRANGER_CLAIM: &str = "ATERM_TEST_STRANGER_CLAIM";
/// What the stand-in prints, with the count of descriptors it received.
const RECEIVED: &str = "STRANGER RECEIVED";
/// The team every shipped candidate is verified against
/// (`aterm_update_core::pins::APPLE_TEAM_ID`).
const TEAM: &str = aterm_update_core::pins::APPLE_TEAM_ID;
/// How an aterm release's own requirement reads back from Security.framework:
/// the identifier every cut stamps, then the Developer ID requirement.
const RELEASE_REQUIREMENT: &str = "requirement: identifier \"com.aterm.aterm\" and anchor apple \
                                   generic and certificate 1[field.1.2.840.113635.100.6.2.6]";
/// A candidate's signing identifier as the worker reads it before the launch:
/// an aterm release's.
fn release_identifier() -> Result<String, String> {
    Ok(crate::handoff_rendezvous::RELEASE_SIGNING_IDENTIFIER.to_string())
}

/// A claim of the right shape that is not the rendezvous's.
const WRONG_CLAIM: &str = "abababababababababababababababababababababababababababababababab";

/// The outgoing process, holding one dialer's claim: the model's terms.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Admission {
    secret: bool,
    /// The gate established that the dialer runs the successor's code. A lookup
    /// that failed establishes nothing, and is closed like a refusal.
    code: bool,
    /// `trustd` answers the check.
    answered: bool,
    disowned: bool,
    /// The model's `phase`: 0 holding, 1 granted, 3 refused, 4 the deadline
    /// ended the wait while the dialer was held.
    phase: i64,
    /// The model's `filed`: 0 not yet, 1 by the refusal's cause, 2 as a death
    /// nobody explained.
    filed: i64,
}

impl Admission {
    /// A dialer whose claim is held, before any decision.
    const fn holding(secret: bool, code: bool, answered: bool, disowned: bool) -> Self {
        Self {
            secret,
            code,
            answered,
            disowned,
            phase: 0,
            filed: 0,
        }
    }

    /// The same dialer at another phase.
    const fn at(self, phase: i64) -> Self {
        Self { phase, ..self }
    }
}

/// Project an admission onto the model's state.
pub(crate) fn project(model: &Model, seen: Admission) -> State {
    let mut state = model.init_state();
    state.insert("secret", i64::from(seen.secret));
    state.insert("code", i64::from(seen.code));
    state.insert("answered", i64::from(seen.answered));
    state.insert("disowned", i64::from(seen.disowned));
    state.insert("phase", seen.phase);
    state.insert("filed", seen.filed);
    state
}

/// This process's pid, as the kernel attests a dialer of this process.
fn own_pid() -> i32 {
    i32::try_from(std::process::id()).expect("a pid fits pid_t")
}

/// The job the launched lane's worker receives for one attempt, built by the
/// shipping `prelaunch_out_of_band_handoff` from the facts
/// `start_unix_update_handoff` decides: an apply ticket (none for the same-image
/// seam), the seam, whether the worker verifies the candidate again (false for a
/// fresh cached pass), and an installed activation.
fn launched_job(
    ticket: bool,
    seam: bool,
    verify_staged_candidate: bool,
    installed_activation: bool,
) -> super::HandoffWorkerJob {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = crate::App::headless_for_test();
    let ticket = ticket.then(|| {
        ApplyAttemptTicket::for_test(
            crate::running_build_number().saturating_add(1),
            "",
            &"ab".repeat(32),
        )
    });
    let target_build = ticket.as_ref().map_or_else(
        crate::running_build_number,
        ApplyAttemptTicket::target_build,
    );
    let (cancel, cancelled) = std::sync::mpsc::sync_channel(1);
    let (job_tx, job_rx) = std::sync::mpsc::sync_channel::<super::HandoffWorkerJob>(1);
    let (reconcile_worker, _reconcile_rx) = std::sync::mpsc::sync_channel(1);
    let reconcile_ticket = app
        .mint_native_update_reconcile_ticket()
        .expect("a reconcile ticket");
    app.prelaunch_out_of_band_handoff(super::PrelaunchArgs {
        attempt_id: 41,
        build: crate::running_build_number(),
        mode: ApplyMode::Automatic,
        apply_attempt: ticket,
        same_image: seam.then_some(super::SameImageHandoff::DebugSeam),
        target_build,
        target_commit: String::new(),
        // std on purpose: the shipped `PrelaunchArgs::command` is a std one.
        command: std::process::Command::new("/usr/bin/true"),
        verify_staged_candidate,
        grant_chunks: false,
        installed_activation,
        bundle: None,
        cancel,
        cancelled,
        job_tx,
        reconcile_worker,
        reconcile_ticket,
    })
    .expect("the launched lane records its attempt");
    let job = job_rx.try_recv().expect("the worker's job was sent");
    app.update_handoff_prelaunch = None;
    job
}

/// The code a fresh cached pass's attempt states for its dialer — the usual
/// update, since the automatic lane pre-verifies at arm time — read off the job
/// the shipping lane builds for it.
fn cached_pass_dialer_code(bundle: &Path) -> DialerCode {
    let job = launched_job(true, false, false, false);
    assert!(!job.verify_staged_candidate, "PRECONDITION: a cached pass");
    job.prelaunch
        .as_ref()
        .expect("the launched lane")
        .dialer_code(TEAM, bundle, release_identifier)
        .expect("the requirement compiles")
}

fn bind() -> Rendezvous {
    let nonce = aterm_uds::rand::hex_token::<16>().expect("a nonce");
    Rendezvous::bind(&nonce).expect("bind a rendezvous for the Tier-1")
}

fn pipe() -> (OwnedFd, OwnedFd) {
    let mut raw = [0i32; 2];
    // SAFETY: `pipe` fills two fresh descriptors into the array.
    assert_eq!(unsafe { libc::pipe(raw.as_mut_ptr()) }, 0, "pipe");
    // SAFETY: both are fresh descriptors this test owns from here.
    unsafe { (OwnedFd::from_raw_fd(raw[0]), OwnedFd::from_raw_fd(raw[1])) }
}

fn this_binary() -> PathBuf {
    std::env::current_exe().expect("the test binary")
}

/// STAND-IN: a same-uid process that copied a claim out of the successor's
/// launch environment (`ps eww`, `KERN_PROCARGS2`) dials, presents it, and
/// waits for a grant; it prints how many descriptors arrived (none, once the
/// outgoing process closes the connection). A no-op in the ordinary run.
///
/// The read has no timer of its own: the connection ends only when the gate
/// grants or closes it, and the gate closes every connection by its own deadline.
/// So the stand-in exits only on the gate's answer — a connection HELD for a code
/// check `trustd` did not answer keeps it alive until the deadline names the
/// hold, and a hold can never pass for a refusal by racing a second timer here.
/// `test_alone::Running::wait` is the hang detector for the process itself.
///
/// A GRANT IS READ WHOLE before the stand-in exits, as the successor reads it
/// (`dial_and_claim`): the descriptors ride the header's `sendmsg` and the body
/// is a second write, so a stand-in that left on the header alone could close
/// its end between the two, and the grant then failed `TransferPartial(EPIPE)`
/// — the outgoing side's correct answer to a peer that hung up mid-grant, not a
/// verdict of the gate (2026-10-06).
#[test]
fn stranger_dialer_stand_in() {
    let (Some(path), Ok(claim)) = (
        std::env::var_os(STRANGER_PATH),
        std::env::var(STRANGER_CLAIM),
    ) else {
        return;
    };
    let stream = dial_for_test(Path::new(&path), &claim).expect("dial and claim");
    let received = read_whole_grant(&stream);
    println!("{RECEIVED} {received} DESCRIPTORS");
}

/// The descriptors of the one-message grant `stream` receives, its header and
/// body read to their end; none once the outgoing process closes the
/// connection.
fn read_whole_grant(stream: &aterm_uds::CtlStream) -> usize {
    use std::io::Read as _;
    // `ATRZ1G` and a big-endian u16 body length (`grant_header`).
    let mut header = [0u8; 8];
    let Ok(received) =
        aterm_uds::fdpass::recv_with_fds(stream, &mut header, aterm_uds::fdpass::MAX_FDS)
    else {
        return 0;
    };
    let count = received.fds.len();
    if count == 0 {
        return 0;
    }
    let mut reader = stream;
    reader
        .read_exact(&mut header[received.bytes..])
        .expect("the grant header");
    assert_eq!(&header[..6], b"ATRZ1G", "a one-message grant");
    let mut body = vec![0u8; usize::from(u16::from_be_bytes([header[6], header[7]]))];
    reader.read_exact(&mut body).expect("the grant body");
    count
}

/// The stand-in dialing `rendezvous` with `claim`.
fn stranger(rendezvous: &Rendezvous, claim: &str) -> crate::test_alone::Running {
    let mut stand_in =
        crate::test_alone::StandIn::helper(module_path!(), "stranger_dialer_stand_in");
    stand_in
        .command()
        .env(STRANGER_PATH, rendezvous.path())
        .env(STRANGER_CLAIM, claim);
    stand_in.spawn()
}

/// How many descriptors the stand-in said it received; it must have run its one
/// test and passed it.
fn received_by(stranger: crate::test_alone::Running) -> usize {
    let ran = stranger.passed();
    let line = ran
        .stdout
        .lines()
        .find_map(|line| line.split(RECEIVED).nth(1))
        .unwrap_or_else(|| panic!("the stand-in never said what it received:\n{}", ran.stdout));
    line.split_whitespace()
        .next()
        .and_then(|count| count.parse().ok())
        .unwrap_or_else(|| panic!("an unreadable count: {line}"))
}

/// The production tail after an accepted dial: one master and both pipes,
/// granted on the held claim as `run_prelaunched_handoff` grants them.
fn grant(peer: &crate::handoff_rendezvous::ClaimedPeer) {
    let (master, _master_peer) = pipe();
    let (_ready_rd, ready_wr) = pipe();
    let (commit_rd, _commit_wr) = pipe();
    peer.transfer(
        "0123456789abcdef0123456789abcdef",
        &[(1, 4242, master.as_fd())],
        ready_wr.as_fd(),
        commit_rd.as_fd(),
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .expect("the grant is sent to an accepted dialer");
}

/// Sets the flag when dropped: the main thread's "the gate has answered" for a
/// watcher, raised on a panic too, so the scope's join never waits on a stand-in
/// that is still connected.
struct Answered<'a>(&'a AtomicBool);

impl Drop for Answered<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

/// Poll the stand-in until it exits (true) or the gate has answered (false).
/// No timer of its own: the gate's deadline is the one hang detector, and the
/// main thread raises `answered` once the gate returns.
fn exited_before_the_answer(
    running: &mut crate::test_alone::Running,
    answered: &AtomicBool,
) -> bool {
    loop {
        if running.exited() {
            return true;
        }
        if answered.load(Ordering::Acquire) {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// One dialer through the real gate, `claim` presented and `code` required:
/// whether it was admitted, and how many descriptors it then held (the grant is
/// sent to an admitted dialer, as the shipping lane sends it). The wait ends when
/// the stand-in does — a refused dialer reads EOF and exits — or at the gate's
/// own deadline, the hang detector; a dialer held there for `trustd` is
/// [`TRUSTD_SILENT`]. A refused dialer that presented the right claim was turned
/// away by a VERDICT on its code, never by a check that did not happen.
fn through_the_gate(claim_is_right: bool, code: &DialerCode) -> (bool, usize) {
    let rendezvous = bind();
    let claim = if claim_is_right {
        rendezvous.claim().to_string()
    } else {
        WRONG_CLAIM.to_string()
    };
    let running = RefCell::new(stranger(&rendezvous, &claim));
    let turned_away = RefCell::new(Vec::new());
    let accepted = rendezvous.accept_claim_observed(
        None,
        code,
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        &|| running.borrow_mut().exited(),
        &|dialer| turned_away.borrow_mut().push(dialer.clone()),
    );
    let turned_away = turned_away.into_inner();
    if turned_away
        .iter()
        .any(|dialer| matches!(dialer.refusal, CodeRefusal::TrustdSilent(_)))
    {
        panic!("{TRUSTD_SILENT}: {turned_away:?}");
    }
    let running = running.into_inner();
    match &accepted {
        Ok(peer) => {
            grant(peer);
            (true, received_by(running))
        }
        Err(error) if held_for_trustd(error) => panic!("{TRUSTD_SILENT}: {error}"),
        Err(RendezvousError::Cancelled) => {
            // The stand-in has exited on this path: its own failure (a dial
            // that failed, a write after the gate dropped the stream) is named
            // with its output before any refusal is judged the gate's.
            let received = received_by(running);
            if claim_is_right {
                assert!(
                    matches!(
                        turned_away.as_slice(),
                        [CodeRefusedDialer {
                            refusal: CodeRefusal::Refused(_),
                            end: CodeRefusedEnd::Closed,
                            ..
                        }]
                    ),
                    "the right claim was refused by a verdict on its code: {turned_away:?}"
                );
            } else {
                assert!(
                    turned_away.is_empty(),
                    "a wrong claim never reaches the code check: {turned_away:?}"
                );
            }
            (false, received)
        }
        Err(other) => panic!("the wait ended on {other} before the stand-in did"),
    }
}

/// One stand-in holding `rendezvous`'s claim through the SHIPPING dial wait
/// (`await_launched_dial`), `code` required: the wait's end and the stand-in.
/// When the stand-in exits before the wait returns, the watcher runs `on_exit`,
/// handed the wait's cancel: a cancel, or LaunchServices' answer that the
/// launched process is gone ([`launch_services_reports_the_exit`]). The gate's
/// deadline is the one hang detector, and a dialer HELD for `trustd` — the
/// rendezvous's held refusal, or one that left while held — is
/// [`TRUSTD_SILENT`]. An accepted peer is returned ungranted; the stand-in
/// waits for its grant with no timer of its own.
fn through_the_dial_wait(
    rendezvous: &Rendezvous,
    in_flight: &LaunchInFlight,
    code: &DialerCode,
    on_exit: impl FnOnce(&SyncSender<()>) + Send,
) -> (
    Result<crate::handoff_rendezvous::ClaimedPeer, LaunchedDialEnd>,
    crate::test_alone::Running,
) {
    let running = stranger(rendezvous, rendezvous.claim());
    let (cancel_tx, cancel_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let answered_flag = AtomicBool::new(false);
    let (ended, running) = std::thread::scope(|scope| {
        let answered_flag = &answered_flag;
        let watcher = scope.spawn(move || {
            let mut running = running;
            if exited_before_the_answer(&mut running, answered_flag) {
                on_exit(&cancel_tx);
            }
            running
        });
        let answered = Answered(answered_flag);
        let ended = await_launched_dial(
            rendezvous,
            in_flight,
            code,
            Instant::now() + aterm_spec::wait::MUST_HAPPEN,
            &cancel_rx,
        );
        drop(answered);
        (ended, watcher.join().expect("the watcher"))
    });
    match &ended {
        Err(LaunchedDialEnd::Rendezvous(error)) if held_for_trustd(error) => {
            panic!("{TRUSTD_SILENT}: {}: {error}", code.describe())
        }
        Err(end @ LaunchedDialEnd::ExitedWhileHeld { .. }) => {
            panic!("{TRUSTD_SILENT}: {}: {end:?}", code.describe())
        }
        _ => {}
    }
    (ended, running)
}

/// LaunchServices' measured answer for a launched process that exits before it
/// checks in: the dial wait then ends on the death and carries the refusal of
/// the dialer it turned away, if that dialer was the launched process.
fn launch_services_reports_the_exit(in_flight: &LaunchInFlight) {
    in_flight.answer_for_test(LaunchInFlight::admitted_for_test(-1, true, own_pid()));
}

/// THE GATE ADMITS EXACTLY WHAT THE MODEL GRANTS: a dialer that is another
/// process, over both halves a stranger can get wrong — the claim, and the code
/// it runs. The code requirement is realized for real: the code at this test
/// binary's path (which the stand-in runs) and the code at `/usr/bin/true`
/// (which it does not).
#[test]
fn the_real_gate_admits_exactly_what_the_model_grants() {
    let model = native_update_rendezvous_admission_model();
    assert!(
        aterm_spec::xref::reset_entered_anchors(),
        "the evidence window must open"
    );
    let ours = DialerCode::code_at(this_binary());
    let other = DialerCode::code_at(PathBuf::from("/usr/bin/true"));
    let mut granted_any = false;
    for (secret, code) in [(true, true), (true, false), (false, true), (false, false)] {
        let (admitted, received) = through_the_gate(secret, if code { &ours } else { &other });
        let holding = Admission::holding(secret, code, true, false);
        assert_eq!(
            admitted,
            model.action_enabled("Grant", &project(&model, holding)),
            "the gate and the model's Grant disagree at {holding:?}"
        );
        if admitted {
            granted_any = true;
            assert_eq!(
                received, 3,
                "an admitted dialer holds the master and both pipes"
            );
            crate::conformance_support::assert_transition(
                &model,
                &project(&model, holding),
                &project(&model, holding.at(1)),
                "Grant",
            );
        } else {
            assert_eq!(received, 0, "a refused dialer holds nothing: {holding:?}");
            crate::conformance_support::assert_transition(
                &model,
                &project(&model, holding),
                &project(&model, holding.at(3)),
                "Refuse",
            );
        }
    }
    assert!(
        granted_any,
        "the one admitted combination is reachable: a gate that refused everything would \
         otherwise agree above"
    );
    let entered = aterm_spec::xref::entered_anchor_ids();
    aterm_spec::xref::disarm_entered_anchors();
    let grant_anchor = aterm_spec::xref::refinements()
        .find(|anchor| {
            aterm_spec::xref::machine_matches(anchor.machine, "NativeUpdateRendezvousAdmission")
                && anchor.action == "Grant"
        })
        .expect("Grant is anchored on the gate");
    assert!(
        entered.contains(grant_anchor.entry_id),
        "the gate ran on this thread: {entered:?}"
    );
}

/// NEGATIVE CONTROL, ON THE SHIPPING GATE: at the same-uid floor — the launched
/// lane from 2026-09-14 to 2026-10-02, which checked the secret and nothing a
/// stranger could not copy — the gate grants a stranger of another code every
/// descriptor. That is the model's `GrantOnSecretAlone`, and the state it reaches
/// is the one `GrantOnlyToTheSuccessorsCode` refuses.
#[test]
fn the_secret_alone_grants_a_stranger_and_the_model_refuses_it() {
    let (admitted, received) = through_the_gate(true, &DialerCode::same_uid_floor_for_test());
    assert!(
        admitted,
        "the floor admits any same-uid dialer holding the secret"
    );
    assert_eq!(received, 3, "and the stranger holds every descriptor");
    let buggy = with_buggy(&native_update_rendezvous_admission_model(), 1);
    let mut state = buggy.init_state();
    for action in ["CopiedSecretOtherCode", "GrantOnSecretAlone"] {
        assert!(buggy.fire(action, &mut state), "{action}: {state:?}");
    }
    assert!(!buggy.check_invariant("GrantOnlyToTheSuccessorsCode", &state));
}

/// THE STRANGER HOLDING THE SECRET, ON THE SHIPPING LANE (U36): LaunchServices
/// does not answer while the dialer is connected (a real successor checks in
/// only after its grant), and a process that is not the launched successor
/// dials with the claim it read. The
/// production tail — the requirement the lane states for the usual update, a
/// fresh cached pass, read off the job the lane builds for it; the dial wait; the
/// grant on a held claim — sends it nothing. Then the same tail, for a dialer that
/// runs the required code, grants it: the refusal is the code's, not the
/// harness's. (Before U36's review a cached pass stated the code at the install
/// path, which admits whatever a same-uid process put there.)
///
/// The gate's deadline is the one hang detector. The refusing arm ends on the
/// stand-in's exit, reported as LaunchServices reports a launched process that
/// never checked in, so the wait carries the refusal and the test requires a
/// VERDICT on the code: a close for any other reason is not the code's refusal.
/// A dialer HELD because the process's `trustd` gate was overdue runs to the
/// deadline and is [`TRUSTD_SILENT`] in either arm — the machine's moment,
/// never a pass and never the code's verdict.
#[test]
fn a_dialer_that_is_not_the_launched_process_gets_no_descriptor() {
    let bundle = PathBuf::from("/Applications/aterm.app");
    let shipped = cached_pass_dialer_code(&bundle);
    assert!(
        shipped.describe().starts_with(RELEASE_REQUIREMENT),
        "{}",
        shipped.describe()
    );
    let accepting = DialerCode::code_at(this_binary());
    // THE SHIPPED REQUIREMENT REFUSES: the stand-in exits on the gate's close
    // and LaunchServices reports the launched process gone, so the wait ends
    // carrying the refusal — which must be a VERDICT on the code. A close for
    // any other reason (a lookup that failed, a claim read past its budget, an
    // unattested peer) never passes as the code's refusal.
    {
        let rendezvous = bind();
        let in_flight = LaunchInFlight::scripted(None);
        let (ended, running) =
            through_the_dial_wait(&rendezvous, &in_flight, &shipped, |_cancel| {
                launch_services_reports_the_exit(&in_flight);
            });
        assert_eq!(
            received_by(running),
            0,
            "{}: the stand-in's descriptors",
            shipped.describe()
        );
        match &ended {
            Err(LaunchedDialEnd::CodeRefused { refusal, .. }) => assert!(
                refusal.is_verdict(),
                "{}: refused by a verdict on its code, not a check that did not happen: \
                 {refusal:?}",
                shipped.describe()
            ),
            other => panic!(
                "{}: the wait did not end on the code's refusal: {:?}",
                shipped.describe(),
                other.as_ref().err()
            ),
        }
    }
    // THE SAME TAIL GRANTS A DIALER OF THE REQUIRED CODE: the refusal above is
    // the code's, not the harness's.
    {
        let rendezvous = bind();
        let in_flight = LaunchInFlight::scripted(None);
        let (accepted, running) =
            through_the_dial_wait(&rendezvous, &in_flight, &accepting, |cancel| {
                let _ = cancel.send(());
            });
        match &accepted {
            Ok(peer) => grant(peer),
            Err(end) => panic!(
                "{}: the dialer was not admitted: {end:?}",
                accepting.describe()
            ),
        }
        assert_eq!(
            received_by(running),
            3,
            "{}: the stand-in's descriptors",
            accepting.describe()
        );
    }
}

/// THE REQUIREMENT EACH ATTEMPT STATES, on the job the shipping lane builds for
/// it (U36's review): every attempt with an update candidate under the pinned
/// team — a fresh cached pass that lets the worker skip its own verification
/// (the usual update), a worker that verifies again, an installed activation —
/// states an aterm release's own requirement, the identifier every cut stamps and
/// the Developer ID of the team every candidate was verified against. Only the
/// same-image seam, and an attempt with no team pinned, read the code at the
/// launched bundle. NEGATIVE CONTROL: the rule was chosen on whether the WORKER
/// verifies until the review, which is false for a cached pass — that choice,
/// `launched_dialer_code(job.verify_staged_candidate, ..)` on the cached pass's
/// own job, is the code at the path.
#[test]
fn every_candidate_attempt_states_the_release_requirement() {
    let bundle = Path::new("/Applications/aterm.app");
    let code_at = "the designated requirement of the code at /Applications/aterm.app";
    for (what, ticket, seam, verifies, activation, release) in [
        ("a fresh cached pass", true, false, false, false, true),
        ("a worker that verifies", true, false, true, false, true),
        ("an installed activation", true, false, false, true, true),
        ("the same-image seam", false, true, false, false, false),
    ] {
        let job = launched_job(ticket, seam, verifies, activation);
        assert_eq!(
            job.verify_staged_candidate, verifies,
            "{what}: PRECONDITION"
        );
        let stated = job
            .prelaunch
            .as_ref()
            .expect("the launched lane")
            .dialer_code(TEAM, bundle, release_identifier)
            .expect("the requirement compiles")
            .describe();
        if release {
            assert!(
                stated.starts_with(RELEASE_REQUIREMENT) && stated.contains(TEAM),
                "{what}: {stated}"
            );
        } else {
            assert_eq!(stated, code_at, "{what}");
        }
        if what == "a fresh cached pass" {
            let before = launched_dialer_code(job.verify_staged_candidate, TEAM, bundle, || {
                unreachable!("the code-at-path rule reads no identifier")
            })
            .expect("no requirement to compile")
            .describe();
            assert_eq!(
                before, code_at,
                "NEGATIVE: the worker's flag picks the path"
            );
        }
    }
    for candidate in [true, false] {
        assert_eq!(
            launched_dialer_code(candidate, "", bundle, || {
                unreachable!("no team pinned reads no identifier")
            })
            .expect("no requirement to compile")
            .describe(),
            code_at,
            "no team pinned"
        );
    }
}

/// THE IDENTIFIER HALF IS ASKED OF THE CANDIDATE BEFORE ANYTHING LAUNCHES (U36's
/// second review; design laws L1 and L4). The release rule requires
/// `identifier "com.aterm.aterm"` on top of the Developer ID, and the candidate's
/// own check tests only the Developer ID — so a candidate cut with another
/// identifier would pass every check, launch, dial, and be refused at the gate on
/// every attempt, filed transient each time. The rule now reads the candidate's
/// identifier first: a release's states the rule; another identifier, or one not
/// read just then, is `Err`, which the worker answers by handing the attempt to
/// the fork lane (`run_prelaunched_handoff`), where no dialer's code is asked.
/// NEGATIVE CONTROL: the rule without the read — the release requirement stated
/// for that same candidate — is what this test refuses to see again.
#[test]
fn a_candidate_signed_otherwise_is_caught_before_the_launch() {
    let bundle = Path::new("/Applications/aterm.app");
    let stated = launched_dialer_code(true, TEAM, bundle, release_identifier)
        .expect("a release's identifier states the rule")
        .describe();
    assert!(stated.starts_with(RELEASE_REQUIREMENT), "{stated}");
    let other = launched_dialer_code(
        true,
        TEAM,
        bundle,
        || Ok("com.aterm.aterm.next".to_string()),
    );
    let Err(why) = other.map(|code| code.describe()) else {
        panic!("NEGATIVE: a candidate signed otherwise was given the release rule");
    };
    assert!(
        why.contains("com.aterm.aterm.next") && why.contains("com.aterm.aterm"),
        "{why}"
    );
    let unread = launched_dialer_code(true, TEAM, bundle, || {
        Err("codesign did not finish".to_string())
    });
    let Err(why) = unread.map(|code| code.describe()) else {
        panic!("an identifier that was not read stated the rule");
    };
    assert!(why.contains("codesign did not finish"), "{why}");
    // The shipped call site reads the identifier through the shipped reader and
    // answers an `Err` with the fork lane, before the rendezvous is bound.
    let src = include_str!("app_update_handoff.rs");
    let lane = &src[src
        .find("fn run_prelaunched_handoff(")
        .expect("the launched lane")..];
    let read = lane
        .find("aterm_update::handoff_candidate_signing_identifier(activation)")
        .expect("the worker reads the candidate's identifier");
    let fork = lane
        .find("the launched lane cannot gate this successor's dial")
        .expect("an unmet rule forks");
    let bind = lane
        .find("Rendezvous::bind(")
        .expect("the rendezvous is bound");
    assert!(read < fork && fork < bind, "read, fork, then bind");
}

/// How a dialer stands when LaunchServices' answer is read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dialer {
    Alive,
    /// Exited, its exit queued, not yet reaped: what a successor that just died
    /// is until launchd reaps it.
    Unreaped,
    Reaped,
}

/// What LaunchServices answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Answer {
    None,
    TheDialer,
    AnotherPid,
    ExitedBeforeCheckIn,
    AnError,
}

/// THE CORROBORATION REFUSES EXACTLY THE DIALERS THE MODEL WILL NOT COMMIT TO,
/// before the grant (the poll right after the dial, and beside the hold) and
/// after it (beside the proof wait): one that LaunchServices names another pid
/// for, and one that LIVES when LaunchServices says the process it launched
/// exited before it checked in. A dialer that exited too — reaped, or still
/// unreaped, which libproc does not report a birth for — is no contradiction:
/// its death ends the attempt by the death checks.
#[test]
fn the_real_corroboration_refuses_exactly_a_disowned_dialer() {
    let model = native_update_rendezvous_admission_model();
    assert!(
        aterm_spec::xref::reset_entered_anchors(),
        "the evidence window must open"
    );
    for dialer in [Dialer::Alive, Dialer::Unreaped, Dialer::Reaped] {
        for answer in [
            Answer::None,
            Answer::TheDialer,
            Answer::AnotherPid,
            Answer::ExitedBeforeCheckIn,
            Answer::AnError,
        ] {
            let mut child = aterm_uds::spawnfd::command("/bin/sh")
                .args(["-c", "read _"])
                .stdin(aterm_uds::spawnfd::Stdio::piped())
                .spawn()
                .expect("spawn the dialer stand-in");
            let candidate = HandoffCandidate::of_unreaped_spawned(&child);
            let watch = aterm_uds::exitwatch::ExitWatch::watch(child.id())
                .expect("EVFILT_PROC attaches to a live child");
            if dialer != Dialer::Alive {
                drop(child.stdin.take());
                let exited = aterm_spec::wait::within(
                    aterm_spec::wait::MUST_HAPPEN,
                    Duration::from_millis(2),
                    || watch.exit_status(),
                );
                assert!(exited.is_some(), "the stand-in exited");
            }
            if dialer == Dialer::Reaped {
                child.wait().expect("reap the stand-in");
            }
            let pid = i32::try_from(candidate.pid).expect("a pid fits pid_t");
            let in_flight = LaunchInFlight::scripted(match answer {
                Answer::TheDialer => Some(pid),
                Answer::AnotherPid => Some(pid.saturating_add(1)),
                _ => None,
            });
            match answer {
                Answer::ExitedBeforeCheckIn => {
                    in_flight.answer_for_test(Err(LaunchError::ExitedBeforeCheckIn));
                }
                Answer::AnError => in_flight.answer_for_test(Err(LaunchError::Rejected {
                    domain: "NSCocoaErrorDomain".to_string(),
                    code: 256,
                    message: "a miscellaneous error occurred".to_string(),
                })),
                _ => {}
            }
            let refused = LaunchCorroboration::of_dialer(&in_flight, candidate)
                .poll()
                .is_err();
            let disowned = match answer {
                Answer::AnotherPid => true,
                Answer::ExitedBeforeCheckIn => dialer == Dialer::Alive,
                Answer::None | Answer::TheDialer | Answer::AnError => false,
            };
            let holding = Admission::holding(true, true, true, disowned);
            let granted = holding.at(1);
            assert_eq!(
                refused,
                !model.action_enabled("Commit", &project(&model, granted)),
                "after the grant: the corroboration and the model's Commit disagree for {dialer:?}                  / {answer:?}"
            );
            assert_eq!(
                refused,
                !model.action_enabled("Grant", &project(&model, holding)),
                "before the grant: the corroboration and the model's Grant disagree for {dialer:?}                  / {answer:?}"
            );
            if dialer != Dialer::Reaped {
                drop(child.stdin.take());
                child.wait().expect("reap the stand-in");
            }
        }
    }
    let entered = aterm_spec::xref::entered_anchor_ids();
    aterm_spec::xref::disarm_entered_anchors();
    let commit_anchor = aterm_spec::xref::refinements()
        .find(|anchor| {
            aterm_spec::xref::machine_matches(anchor.machine, "NativeUpdateRendezvousAdmission")
                && anchor.action == "Commit"
        })
        .expect("Commit is anchored on the corroboration");
    assert!(
        entered.contains(commit_anchor.entry_id),
        "the corroboration ran on this thread: {entered:?}"
    );

    // NEGATIVE CONTROL: the corroboration of 2026-09-29 read the exit answer, like
    // any error, as corroborating the dialer — the model's `CommitOverDisowning`,
    // whose state `NoCommitToADisownedDialer` refuses.
    let buggy = with_buggy(&model, 1);
    let mut state = buggy.init_state();
    for action in ["Grant", "LaunchServicesDisowns", "CommitOverDisowning"] {
        assert!(buggy.fire(action, &mut state), "{action}: {state:?}");
    }
    assert!(!buggy.check_invariant("NoCommitToADisownedDialer", &state));
}

/// THE HOLD, ON THE SHIPPING GATE (U36's review): a dialer whose code check
/// macOS's code-signing service does not answer is neither admitted nor turned
/// away. The gate keeps its connection and checks again; with no answer by the
/// deadline the wait ends there, saying so in typed words, and the dialer was
/// never closed before it; once `trustd` answers, the same connection is admitted
/// and granted. Run alone in a child: it makes the process's one `trustd` gate
/// overdue (`aterm_uds::deadline::TRUST`) exactly as a silent `trustd` does —
/// work that never returns, past its suspect age — which every code and TLS
/// check in the process shares. The dialer runs this test binary's ad-hoc code,
/// whose check needs no `trustd` once the gate lets it through.
///
/// NEGATIVE CONTROL: the first code gate of 2026-10-02 closed an unanswered
/// check at once, so the successor read end of file and exited — the model's
/// `RefuseUnanswered`, whose state `AnUnansweredCheckTurnsNoOneAway` refuses.
#[test]
fn a_code_check_trustd_does_not_answer_holds_the_dialer_until_it_does() {
    if !crate::test_alone::alone() {
        return;
    }
    let model = native_update_rendezvous_admission_model();
    assert!(
        aterm_spec::xref::reset_entered_anchors(),
        "the evidence window must open"
    );
    let code = DialerCode::this_process();
    let (release, held_up) = std::sync::mpsc::channel::<()>();
    let stuck = aterm_uds::deadline::TRUST.run(
        Duration::from_millis(1),
        Duration::from_millis(50),
        move || {
            let _ = held_up.recv();
        },
    );
    assert_eq!(stuck, Err(aterm_uds::deadline::Refused::Unanswered));
    std::thread::sleep(Duration::from_millis(5));
    let holding = Admission::holding(true, true, true, false);
    let silent = Admission {
        answered: false,
        ..holding
    };
    crate::conformance_support::assert_transition(
        &model,
        &project(&model, holding),
        &project(&model, silent),
        "TrustdSilent",
    );

    // 1. NO ANSWER BY THE DEADLINE: the wait ends on it, typed and naming the
    //    service, after holding the dialer the whole time.
    {
        let rendezvous = bind();
        let dialer = dial_for_test(rendezvous.path(), rendezvous.claim()).expect("dial and claim");
        let turned_away = RefCell::new(Vec::new());
        let budget = Duration::from_millis(600);
        let started = Instant::now();
        let ended = rendezvous.accept_claim_observed(
            None,
            &code,
            Instant::now() + budget,
            &|| false,
            &|dialer| turned_away.borrow_mut().push(dialer.clone()),
        );
        assert!(started.elapsed() >= budget, "held to the deadline");
        match &ended {
            Err(RendezvousError::PeerCode {
                pid,
                refusal: CodeRefusal::TrustdSilent(why),
            }) => {
                assert_eq!(*pid, own_pid());
                assert!(why.contains("(trustd) is not answering"), "{why}");
            }
            other => panic!(
                "the deadline did not say the check was unanswered: {:?}",
                other.as_ref().err()
            ),
        }
        assert!(
            turned_away.borrow().is_empty(),
            "held, never turned away: {:?}",
            turned_away.borrow()
        );
        assert!(
            ended
                .as_ref()
                .err()
                .is_some_and(|error| error.to_string().contains("trustd")),
            "the words name the service: {ended:?}",
            ended = ended.as_ref().err()
        );
        crate::conformance_support::assert_transition(
            &model,
            &project(&model, silent),
            &project(&model, silent.at(4)),
            "DeadlineWhileHeld",
        );
        drop(dialer);
    }

    // 2. TRUSTD ANSWERS WHILE THE DIALER IS HELD: the same connection is
    //    admitted and granted.
    {
        let rendezvous = bind();
        let dialer = dial_for_test(rendezvous.path(), rendezvous.claim()).expect("dial and claim");
        let answers_after = Duration::from_millis(400);
        let started = Instant::now();
        let accepted = std::thread::scope(|scope| {
            let release = &release;
            scope.spawn(move || {
                std::thread::sleep(answers_after);
                release.send(()).expect("the stuck check is waiting");
            });
            rendezvous.accept_claim_observed(
                None,
                &code,
                Instant::now() + aterm_spec::wait::MUST_HAPPEN,
                &|| false,
                &|dialer| panic!("turned away while trustd was silent: {dialer:?}"),
            )
        });
        let peer = accepted.expect("admitted once trustd answers");
        assert!(
            started.elapsed() >= answers_after,
            "held until the answer, not admitted before it"
        );
        assert_eq!(peer.pid(), own_pid());
        crate::conformance_support::assert_transition(
            &model,
            &project(&model, silent),
            &project(&model, holding),
            "TrustdAnswers",
        );
        grant(&peer);
        let _ = dialer.set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN));
        let mut header = [0u8; 16];
        let received =
            aterm_uds::fdpass::recv_with_fds(&dialer, &mut header, aterm_uds::fdpass::MAX_FDS)
                .map_or(0, |received| received.fds.len());
        assert_eq!(received, 3, "the held connection itself took the grant");
        crate::conformance_support::assert_transition(
            &model,
            &project(&model, holding),
            &project(&model, holding.at(1)),
            "Grant",
        );
    }
    let entered = aterm_spec::xref::entered_anchor_ids();
    aterm_spec::xref::disarm_entered_anchors();
    for action in ["Grant", "DeadlineWhileHeld"] {
        let anchor = aterm_spec::xref::refinements()
            .find(|anchor| {
                aterm_spec::xref::machine_matches(anchor.machine, "NativeUpdateRendezvousAdmission")
                    && anchor.action == action
            })
            .unwrap_or_else(|| panic!("{action} is anchored"));
        assert!(
            entered.contains(anchor.entry_id),
            "{action} ran on this thread: {entered:?}"
        );
    }

    // NEGATIVE CONTROL, in the model.
    let buggy = with_buggy(&model, 1);
    let mut state = buggy.init_state();
    for action in ["TrustdSilent", "RefuseUnanswered"] {
        assert!(buggy.fire(action, &mut state), "{action}: {state:?}");
    }
    assert!(!buggy.check_invariant("AnUnansweredCheckTurnsNoOneAway", &state));
}

/// A SUCCESSOR TURNED AWAY AT THE CODE GATE IS FILED BY THE REFUSAL (U36's
/// review). A dialer — another process, holding the claim — is turned away by
/// its code; it reads end of file and exits, as a successor's failed claim exits
/// before any window; LaunchServices then reports the launched process
/// terminated before it checked in. The shipping dial wait ends on that death
/// and carries the refusal, since the dialer it turned away is gone too
/// (`code_refused_dialer_was_launched`), and the shipping filing books it by its
/// cause: a verdict `Rejected`, a check that did not happen `ProducerFailed`,
/// transient both, in words that say the successor dialled.
///
/// NEGATIVE CONTROL, booked for real: the attempt was filed as `ChildDied` with
/// nothing observed, "the launched successor exited before it dialled" — the
/// Unexplained shape, which converges on the bytes after six attempts; the model's
/// `FileAsUnexplainedDeath`, whose state `ARefusalIsFiledByItsCause` refuses.
#[test]
fn a_successor_turned_away_at_the_code_gate_is_filed_by_the_refusal() {
    let model = native_update_rendezvous_admission_model();
    assert!(
        aterm_spec::xref::reset_entered_anchors(),
        "the evidence window must open"
    );
    let dir = std::env::temp_dir().join(format!("atrzcode-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a scratch dir");
    let unsigned = dir.join("unsigned");
    std::fs::write(&unsigned, "#!/bin/sh\nexit 0\n").expect("an unsigned file");
    for (code, verdict) in [
        (DialerCode::code_at(PathBuf::from("/usr/bin/true")), true),
        (DialerCode::code_at(unsigned.clone()), false),
    ] {
        let rendezvous = bind();
        let in_flight = LaunchInFlight::scripted(None);
        // The turned-away dialer exits on the gate's close; held for `trustd`
        // it stays, and the gate's deadline names the hold.
        let (ended, running) = through_the_dial_wait(&rendezvous, &in_flight, &code, |_cancel| {
            launch_services_reports_the_exit(&in_flight);
        });
        assert_eq!(
            received_by(running),
            0,
            "a turned-away dialer holds nothing"
        );
        let Err(LaunchedDialEnd::CodeRefused {
            dialer,
            refusal,
            death,
        }) = ended
        else {
            panic!(
                "the death did not carry the refusal: {:?}",
                ended.as_ref().err()
            );
        };
        assert_eq!(refusal.is_verdict(), verdict, "{refusal:?}");
        assert_eq!(death, PreDialDeath::TerminatedBeforeCheckIn);
        let completion = code_refused_completion(
            7,
            "0123456789abcdef".to_string(),
            dialer.pid,
            &refusal,
            death,
        );
        assert_eq!(
            completion.outcome,
            if verdict {
                crate::UpdateHandoffOutcome::Rejected
            } else {
                crate::UpdateHandoffOutcome::ProducerFailed
            }
        );
        assert_eq!(
            HandoffFailureLane::classify(
                ApplyMode::Automatic,
                completion.outcome,
                completion.child_death,
                false
            ),
            HandoffFailureLane::Physical(PhysicalFailureShape::Transient),
            "a passing failure: {}",
            completion.detail
        );
        assert!(
            completion.detail.contains("dialled")
                && !completion.detail.contains("before it dialled")
                && completion.detail.contains(&refusal.to_string()),
            "{}",
            completion.detail
        );
        let refused = Admission::holding(true, false, true, false).at(3);
        crate::conformance_support::assert_transition(
            &model,
            &project(&model, Admission::holding(true, false, true, false)),
            &project(&model, refused),
            "Refuse",
        );
        crate::conformance_support::assert_transition(
            &model,
            &project(&model, refused),
            &project(
                &model,
                Admission {
                    filed: 1,
                    ..refused
                },
            ),
            "FileByCause",
        );

        // NEGATIVE CONTROL: the filing before the review, booked for real.
        let before = crate::UpdateHandoffCompletion::failure(
            7,
            None,
            None,
            crate::UpdateHandoffOutcome::ChildDied,
            "the launched successor exited before it dialled",
        )
        .with_child_death(death.evidence());
        assert_eq!(
            HandoffFailureLane::classify(
                ApplyMode::Automatic,
                before.outcome,
                before.child_death,
                false
            ),
            HandoffFailureLane::Physical(PhysicalFailureShape::Unexplained)
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
    let entered = aterm_spec::xref::entered_anchor_ids();
    aterm_spec::xref::disarm_entered_anchors();
    let file_anchor = aterm_spec::xref::refinements()
        .find(|anchor| {
            aterm_spec::xref::machine_matches(anchor.machine, "NativeUpdateRendezvousAdmission")
                && anchor.action == "FileByCause"
        })
        .expect("FileByCause is anchored on the filing");
    assert!(
        entered.contains(file_anchor.entry_id),
        "the filing ran on this thread: {entered:?}"
    );
    let buggy = with_buggy(&model, 1);
    let mut state = buggy.init_state();
    for action in ["CopiedSecretOtherCode", "Refuse", "FileAsUnexplainedDeath"] {
        assert!(buggy.fire(action, &mut state), "{action}: {state:?}");
    }
    assert!(!buggy.check_invariant("ARefusalIsFiledByItsCause", &state));
}

/// WHICH DEATH A REFUSAL ANSWERS FOR: a dialer the gate turned away is the
/// launched process whose death LaunchServices reports only when it is the very
/// pid LaunchServices named, or — for a death reported with no pid — when it is
/// gone too. One that still lives is a stranger, and the death stays the
/// successor's own; one never witnessed is not concluded to be it.
#[test]
fn a_turned_away_dialer_answers_only_for_its_own_death() {
    let mut child = aterm_uds::spawnfd::command("/bin/sh")
        .args(["-c", "read _"])
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .spawn()
        .expect("spawn the dialer stand-in");
    let dialer = HandoffCandidate::of_unreaped_spawned(&child);
    assert!(dialer.birth.is_some(), "PRECONDITION: witnessed alive");
    let named = |candidate: HandoffCandidate| PreDialDeath::NamedProcessGone {
        candidate,
        death: crate::ChildDeathEvidence::Unobserved,
    };
    assert!(
        !code_refused_dialer_was_launched(dialer, PreDialDeath::TerminatedBeforeCheckIn),
        "a dialer that lives is not the process LaunchServices says terminated"
    );
    assert!(code_refused_dialer_was_launched(dialer, named(dialer)));
    assert!(!code_refused_dialer_was_launched(
        dialer,
        named(HandoffCandidate::from_bare_pid(dialer.pid.wrapping_add(1)))
    ));
    drop(child.stdin.take());
    child.wait().expect("reap the stand-in");
    assert!(
        code_refused_dialer_was_launched(dialer, PreDialDeath::TerminatedBeforeCheckIn),
        "gone too: it was the launched process"
    );
    assert!(
        !code_refused_dialer_was_launched(
            HandoffCandidate::from_bare_pid(dialer.pid),
            PreDialDeath::TerminatedBeforeCheckIn
        ),
        "never witnessed: not concluded"
    );
}

/// The machine's actions with no shipping code of their own here, each waived
/// by name. Bound above: `Grant` (`Rendezvous::gate_one`), `DeadlineWhileHeld`
/// (`handoff_rendezvous::admit_code`), `FileByCause` (`code_refused_completion`)
/// and `Commit` (`LaunchCorroboration::poll`).
mod waivers {
    #[aterm_spec::spec_unmodeled(
        machine = "native_update_rendezvous_admission",
        action = "WrongSecret",
        reason = "The environment: a dialer that does not present this attempt's claim; realized \
                  by a stand-in process presenting another claim."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "native_update_rendezvous_admission",
        action = "CopiedSecretOtherCode",
        reason = "The environment: a same-uid process that copied the claim out of the \
                  successor's launch environment (KERN_PROCARGS2) and runs other code; realized \
                  by a stand-in process that does not meet the stated requirement."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "native_update_rendezvous_admission",
        action = "TrustdSilent",
        reason = "The environment: macOS's code-signing service stops answering; realized by \
                  making the process's one trustd gate overdue (aterm_uds::deadline::TRUST), as \
                  a silent trustd does."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "native_update_rendezvous_admission",
        action = "TrustdAnswers",
        reason = "The environment: the stuck check returns and the gate lets checks through \
                  again; realized by releasing the overdue work."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "native_update_rendezvous_admission",
        action = "LaunchServicesDisowns",
        reason = "The environment: LaunchServices' answer naming another pid, or reporting the \
                  launched process exited; scripted with the measured answers, since a unit test \
                  may not launch an application (AGENTS.md rule 5)."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "native_update_rendezvous_admission",
        action = "Refuse",
        reason = "The refusal's own steps are NativeUpdateOverlapHandoff's: before the grant the \
                  ungranted stand-down (retire_ungranted_successor), after it the kill-and-reap \
                  of the reject path; both are bound by that model's Tier-1."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "native_update_rendezvous_admission",
        action = "GrantOnSecretAlone",
        reason = "The Buggy=1 mutant: the launched lane before 2026-10-02, the secret its one \
                  gate; run for real at the same-uid floor by the conformance, which sees a \
                  stranger granted and the model refuse it."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "native_update_rendezvous_admission",
        action = "CommitOverDisowning",
        reason = "The Buggy=1 mutant: 2026-09-29's corroboration, an exit answer read as \
                  corroborating; no shipping code, replayed in the model by the conformance."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "native_update_rendezvous_admission",
        action = "RefuseUnanswered",
        reason = "The Buggy=1 mutant: the first code gate of 2026-10-02, which closed a dialer \
                  whose check trustd did not answer; replayed in the model by the conformance."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "native_update_rendezvous_admission",
        action = "FileAsUnexplainedDeath",
        reason = "The Buggy=1 mutant: that gate's filing of the turned-away successor as a death \
                  before its dial; booked for real and replayed in the model by the conformance."
    )]
    #[expect(
        dead_code,
        reason = "carrier for the `spec_unmodeled` waivers above; nothing calls it"
    )]
    fn rendezvous_admission_waivers() {}
}
