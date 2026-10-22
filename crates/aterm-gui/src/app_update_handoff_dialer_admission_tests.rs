// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tier-1 for `NativeUpdateRendezvousAdmission` (aterm-spec's
//! `native_update_rendezvous_admission_model`): the outgoing process grants the launched
//! lane's descriptors only to a dialer running the successor's code, commits to none
//! LaunchServices disowns, holds (never turns away) a dialer whose code check `trustd` does not
//! answer, and files a successor it turned away by the refusal's cause.
//!
//! Dialers are other processes: this test binary run as a stand-in that copied the claim, as a
//! same-uid process reading `KERN_PROCARGS2` could. The gate, dial wait, grant, filing and
//! corroboration are the shipping code; LaunchServices is scripted with its measured answers,
//! since a unit test may not launch an application.
//!
//! Negative controls, each refused by its invariant: `GrantOnSecretAlone` (run for real at
//! the same-uid floor), `CommitOverDisowning`, `RefuseUnanswered` and
//! `FileAsUnexplainedDeath` (booked for real).

use super::{
    HandoffCandidate, LaunchCorroboration, LaunchedDialEnd, PreDialDeath, await_launched_dial,
    code_refused_completion, code_refused_dialer_was_launched, launched_dialer_code,
};
use crate::app_launch_successor::{LaunchError, LaunchInFlight};
use crate::app_native::{HandoffFailureLane, PhysicalFailureShape};
use crate::handoff_rendezvous::{
    CodeRefusal, CodeRefusedDialer, CodeRefusedEnd, DialerCode, Rendezvous, RendezvousError,
    dial_for_test, held_for_trustd, holds_for_test, set_io_clock_for_test, trustd_silent,
};
use crate::native_updater_service::{ApplyAttemptTicket, ApplyMode};
use aterm_models_update::native_update_rendezvous_admission_model;
use aterm_spec::derive::Model;
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

/// The worker's job for one attempt, built by the shipping `prelaunch_out_of_band_handoff`:
/// an apply ticket (none for the same-image seam), the seam, whether the worker verifies the
/// candidate again, and an installed activation.
pub(super) fn launched_job(
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

/// The code a fresh cached pass's attempt (the usual update: the automatic lane pre-verifies
/// at arm time) states for its dialer, read off the job the shipping lane builds.
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

/// Stand-in: a same-uid process that copied a claim out of the successor's launch
/// environment (`ps eww`, `KERN_PROCARGS2`) dials, presents it, and prints how many
/// descriptors arrived. A no-op in the ordinary run.
///
/// The read has no timer of its own: the stand-in exits only on the gate's answer, so a hold
/// for `trustd` can never pass for a refusal by racing a second timer here;
/// `test_alone::Running::wait` is the hang detector. The grant is read whole, as
/// `dial_and_claim` reads it: the descriptors ride the header and the body is a second write,
/// so leaving after the header could fail the grant with `TransferPartial(EPIPE)`.
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
        None,
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

/// Poll the stand-in until it exits (true) or the gate has answered (false). No timer of its
/// own: the gate's deadline is the one hang detector.
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

/// One dialer through the real gate, `claim` presented and `code` required: whether it was
/// admitted, and how many descriptors it then held (an admitted dialer is granted). A refused
/// dialer that presented the right claim must have been refused by a verdict on its code.
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
        trustd_silent(format_args!("{turned_away:?}"));
    }
    let running = running.into_inner();
    match &accepted {
        Ok(peer) => {
            grant(peer);
            (true, received_by(running))
        }
        Err(error) if held_for_trustd(error) => trustd_silent(error),
        Err(RendezvousError::Cancelled) => {
            // The stand-in has exited: its own failure is named with its output before any
            // refusal is judged the gate's.
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

/// One stand-in holding `rendezvous`'s claim through the shipping dial wait, `code` required:
/// the wait's end and the stand-in. If the stand-in exits first, the watcher runs `on_exit`
/// with the wait's cancel. A dialer held for `trustd` is COULD NOT RUN ([`trustd_silent`]); an
/// accepted peer is returned ungranted.
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
            &Default::default(),
        );
        drop(answered);
        (ended, watcher.join().expect("the watcher"))
    });
    match &ended {
        Err(LaunchedDialEnd::Rendezvous(error)) if held_for_trustd(error) => {
            trustd_silent(format_args!("{}: {error}", code.describe()))
        }
        Err(end @ LaunchedDialEnd::ExitedWhileHeld { .. }) => {
            trustd_silent(format_args!("{}: {end:?}", code.describe()))
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

/// The gate admits exactly what the model grants, for a dialer that is another process, over
/// both halves a stranger can get wrong: the claim, and the code it runs (this test binary,
/// which the stand-in runs, or `/usr/bin/true`, which it does not).
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

/// Negative control on the shipping gate: at the same-uid floor (the secret alone) the gate
/// grants a stranger of another code every descriptor, the model's `GrantOnSecretAlone`,
/// whose state `GrantOnlyToTheSuccessorsCode` refuses.
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

/// A process that is not the launched successor dials with the claim it read before
/// LaunchServices answers, and the production tail (a fresh cached pass's requirement, the dial
/// wait, the grant) sends it nothing; the same tail grants a dialer of the required code, so
/// the refusal is the code's. A dialer held for an unanswered `trustd` is COULD NOT RUN
/// ([`trustd_silent`]), never a pass.
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
    // The shipped requirement refuses. A close for any other reason (a failed lookup, a claim
    // read past its budget, an unattested peer) never passes as the code's refusal.
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
    // The same tail grants a dialer of the required code: the refusal above is the code's.
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

/// Every attempt with a candidate under the pinned team states an aterm release's requirement
/// (identifier plus the team's Developer ID); only the same-image seam, and an attempt with no
/// team pinned, read the code at the launched bundle. Negative control: choosing the rule on
/// `job.verify_staged_candidate` gives a cached pass the code at the path.
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

/// The release rule requires `identifier "com.aterm.aterm"`, which the candidate's own
/// Developer ID check does not test; a candidate cut with another identifier would launch and
/// be refused at the gate on every attempt. So the identifier is read before anything
/// launches: a release's states the rule; another, or one not read, is `Err`, which the worker
/// answers with the fork lane (`run_prelaunched_handoff`).
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
}

/// How a dialer stands when LaunchServices' answer is read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dialer {
    Alive,
    /// Exited, its exit queued, not yet reaped by launchd.
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

/// The corroboration refuses, before and after the grant, exactly the dialers the model will
/// not commit to: one LaunchServices names another pid for, and a live one when LaunchServices
/// says its process exited before check-in (a dialer that exited too is no contradiction).
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

    // Negative control: reading the exit answer, like any error, as corroborating the dialer
    // is the model's `CommitOverDisowning`, refused by `NoCommitToADisownedDialer`.
    let buggy = with_buggy(&model, 1);
    let mut state = buggy.init_state();
    for action in ["Grant", "LaunchServicesDisowns", "CommitOverDisowning"] {
        assert!(buggy.fire(action, &mut state), "{action}: {state:?}");
    }
    assert!(!buggy.check_invariant("NoCommitToADisownedDialer", &state));
}

/// A dialer whose code check `trustd` does not answer is held, neither admitted nor turned
/// away: the deadline ends the wait saying so, and once `trustd` answers the same connection is
/// granted. Runs alone in a child, because it makes the process's one `trustd` gate
/// (`aterm_uds::deadline::TRUST`) overdue.
///
/// Negative control: closing an unanswered check at once is the model's `RefuseUnanswered`,
/// refused by `AnUnansweredCheckTurnsNoOneAway`.
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
    // Unanswered only at its 50 ms deadline, so the stuck check is past its 1 ms suspect age:
    // from here the gate refuses every check at once, until `release`.
    assert_eq!(stuck, Err(aterm_uds::deadline::Refused::Unanswered));
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

    // 1. No answer by the deadline: the wait ends on it, typed and naming the service, after
    //    holding the dialer the whole time. The deadline comes at the hold, on this thread's
    //    clock, however late the thread got there.
    {
        let rendezvous = bind();
        let dialer = dial_for_test(rendezvous.path(), rendezvous.claim()).expect("dial and claim");
        let turned_away = RefCell::new(Vec::new());
        let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        let held = holds_for_test();
        set_io_clock_for_test(Some(Box::new(move || {
            if holds_for_test() > held {
                deadline
            } else {
                Instant::now()
            }
        })));
        let ended = rendezvous.accept_claim_observed(None, &code, deadline, &|| false, &|dialer| {
            turned_away.borrow_mut().push(dialer.clone())
        });
        set_io_clock_for_test(None);
        assert_eq!(holds_for_test(), held + 1, "held until the deadline");
        match &ended {
            Err(RendezvousError::PeerCode {
                pid,
                refusal: CodeRefusal::TrustdSilent(_),
            }) => {
                assert_eq!(*pid, own_pid());
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

    // 2. Trustd answers while the dialer is held: the same connection is admitted and granted.
    //    Trustd answers at the hold's first abort poll.
    {
        let rendezvous = bind();
        let dialer = dial_for_test(rendezvous.path(), rendezvous.claim()).expect("dial and claim");
        let held = holds_for_test();
        let released = std::cell::Cell::new(false);
        let accepted = rendezvous.accept_claim_observed(
            None,
            &code,
            Instant::now() + aterm_spec::wait::MUST_HAPPEN,
            &|| {
                if holds_for_test() > held && !released.replace(true) {
                    release.send(()).expect("the stuck check is waiting");
                }
                false
            },
            &|dialer| panic!("turned away while trustd was silent: {dialer:?}"),
        );
        assert_eq!(holds_for_test(), held + 1, "held once, then admitted");
        let peer = accepted.expect("admitted once trustd answers");
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

    // Negative control, in the model.
    let buggy = with_buggy(&model, 1);
    let mut state = buggy.init_state();
    for action in ["TrustdSilent", "RefuseUnanswered"] {
        assert!(buggy.fire(action, &mut state), "{action}: {state:?}");
    }
    assert!(!buggy.check_invariant("AnUnansweredCheckTurnsNoOneAway", &state));
}

/// A successor turned away at the code gate is filed by the refusal: the dial wait ends on its
/// reported death carrying the refusal (`code_refused_dialer_was_launched`), booked transient
/// as `Rejected` (a verdict) or `ProducerFailed` (a check that did not happen), in words that
/// say the successor dialled.
///
/// Negative control, booked for real: filing it as `ChildDied` with nothing observed is the
/// Unexplained shape, which converges on the bytes after six attempts (the model's
/// `FileAsUnexplainedDeath`, refused by `ARefusalIsFiledByItsCause`).
#[test]
fn a_successor_turned_away_at_the_code_gate_is_filed_by_the_refusal() {
    let model = native_update_rendezvous_admission_model();
    assert!(
        aterm_spec::xref::reset_entered_anchors(),
        "the evidence window must open"
    );
    let dir = aterm_tempfile::TempDir::new().expect("a scratch dir");
    let unsigned = dir.path().join("unsigned");
    std::fs::write(&unsigned, "#!/bin/sh\nexit 0\n").expect("an unsigned file");
    for (code, verdict) in [
        (DialerCode::code_at(PathBuf::from("/usr/bin/true")), true),
        (DialerCode::code_at(unsigned.clone()), false),
    ] {
        let rendezvous = bind();
        let in_flight = LaunchInFlight::scripted(None);
        // The turned-away dialer exits on the gate's close; one held for `trustd` stays, and
        // the gate's deadline names the hold.
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
            completion.detail.contains(&refusal.to_string()),
            "the filing carries the refusal: {}",
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

        // Negative control, booked for real: the filing as an unexplained death.
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
    drop(dir);
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

/// A dialer the gate turned away answers for the death LaunchServices reports only when it is
/// the pid LaunchServices named, or, for a death reported with no pid, when it is gone too. One
/// that still lives is a stranger; one never witnessed is not concluded to be the successor.
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
