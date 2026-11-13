// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The outgoing process's judgment of a granted successor's progress while every session is
//! parked ([`super::ProgressWatch`]), through the real readiness-channel wait.

use super::{
    PROGRESS_SILENCE_BOUND, ProgressWatch, SuccessorStart, SuccessorWork, WorkWindow,
    wait_handoff_ready,
};
use crate::UpdateHandoffOutcome;
use crate::app_native::{HandoffFailureLane, PhysicalFailureShape};
use crate::handoff_progress::{Report, Reporter};
use crate::native_updater_service::ApplyMode;
use std::os::fd::{AsRawFd as _, OwnedFd};
use std::time::{Duration, Instant};

fn expected_proof() -> crate::seamless::AdoptionProof {
    crate::seamless::adoption_proof(
        "progress-watch",
        2,
        "abcdef0",
        &[0x11; 32],
        &[0x22; 32],
        &[],
    )
    .expect("bounded proof fixture")
}

fn booked(outcome: UpdateHandoffOutcome) -> HandoffFailureLane {
    HandoffFailureLane::classify(
        ApplyMode::Automatic,
        outcome,
        crate::ChildDeathEvidence::Unobserved,
        false,
    )
}

#[test]
fn a_successor_that_keeps_reporting_is_never_ended_by_the_progress_bound() {
    let start = Instant::now();
    let mut watch = ProgressWatch::new(start, PROGRESS_SILENCE_BOUND);
    assert!(
        !watch.silent_at(start + Duration::from_secs(50)),
        "one that never reported is judged by the hang detector alone"
    );
    let step = PROGRESS_SILENCE_BOUND - Duration::from_millis(1);
    let mut at = start;
    for n in 0..40 {
        at += step;
        assert!(
            !watch.silent_at(at),
            "report {n} comes just inside the bound, {} s into the wait",
            at.duration_since(start).as_secs()
        );
        watch.hear(Report::new("gate:waiting", n > 3), at);
    }
    assert!(!watch.silent_at(at + step));
    assert!(
        watch.silent_at(at + PROGRESS_SILENCE_BOUND),
        "the first whole bound without a report ends it"
    );
    assert_eq!(
        watch.expired(),
        UpdateHandoffOutcome::TimedOut,
        "one that launched and kept reporting until the hang detector timed out"
    );
    assert_eq!(
        booked(watch.expired()),
        HandoffFailureLane::Physical(PhysicalFailureShape::Transient),
        "which says nothing against its image"
    );
}

#[test]
fn the_hang_detector_books_a_successor_that_never_finished_launching_as_stalled() {
    let start = Instant::now();
    let mut watch = ProgressWatch::new(start, PROGRESS_SILENCE_BOUND);
    assert_eq!(
        watch.expired(),
        UpdateHandoffOutcome::Stalled,
        "silent since its grant (or a successor that does not report)"
    );
    watch.hear(Report::new("claimed", false), start);
    watch.hear(Report::new("app_built", false), start);
    assert_eq!(
        watch.expired(),
        UpdateHandoffOutcome::Stalled,
        "reporting, but its native launch never finished"
    );
    watch.hear(Report::new("resumed", true), start);
    assert_eq!(watch.expired(), UpdateHandoffOutcome::TimedOut);
    assert_eq!(
        booked(UpdateHandoffOutcome::Stalled),
        HandoffFailureLane::Physical(PhysicalFailureShape::Unexplained)
    );
}

/// The successor wrote three reports and then nothing, its channel still open: alive and silent.
#[test]
fn a_silent_successor_ends_the_parked_wait_at_the_progress_bound_and_is_booked_against_its_image() {
    let (proof_rd, proof_wr) = aterm_uds::pipe().expect("proof pipe");
    let proof_rd = OwnedFd::from(proof_rd);
    let mut successor = Reporter::on(proof_wr.as_raw_fd());
    for (label, launched) in [
        ("claimed", false),
        ("resumed", true),
        ("first_present", true),
    ] {
        assert!(successor.send(Report::new(label, launched)));
    }
    let (_cancel_tx, cancel_rx) = std::sync::mpsc::sync_channel(1);
    let silence = Duration::from_millis(100);
    let end = wait_handoff_ready(
        &proof_rd,
        expected_proof(),
        &cancel_rx,
        &[],
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        silence,
        &mut || None,
        &mut || false,
        #[cfg(target_os = "macos")]
        None,
    );
    assert_eq!(
        end.outcome,
        UpdateHandoffOutcome::Stalled,
        "ended by its silence, not at the hang detector: {}",
        end.progress
    );
    assert_eq!(end.progress.reports, 3);
    assert!(end.progress.launched);
    assert_eq!(
        end.progress.last.as_ref().map(Report::label),
        Some("first_present")
    );
    assert!(end.progress.quiet_for >= silence, "{}", end.progress);
    assert_eq!(
        booked(end.outcome),
        HandoffFailureLane::Physical(PhysicalFailureShape::Unexplained),
        "a granted successor that went silent is evidence against its image"
    );
    drop(proof_wr);
}

#[test]
fn reports_before_the_proof_keep_its_framing() {
    let (proof_rd, proof_wr) = aterm_uds::pipe().expect("proof pipe");
    let proof_rd = OwnedFd::from(proof_rd);
    let mut successor = Reporter::on(proof_wr.as_raw_fd());
    for n in 0..64 {
        assert!(successor.send(Report::new(&format!("gate:{n}"), n > 10)));
    }
    let wire = expected_proof().to_wire();
    // SAFETY: one bounded write of the fixed proof wire to the test-owned pipe.
    let wrote = unsafe { libc::write(proof_wr.as_raw_fd(), wire.as_ptr().cast(), wire.len()) };
    assert_eq!(wrote, isize::try_from(wire.len()).unwrap_or(-1));
    let (_cancel_tx, cancel_rx) = std::sync::mpsc::sync_channel(1);
    let end = wait_handoff_ready(
        &proof_rd,
        expected_proof(),
        &cancel_rx,
        &[],
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        PROGRESS_SILENCE_BOUND,
        &mut || None,
        &mut || false,
        #[cfg(target_os = "macos")]
        None,
    );
    assert_eq!(end.outcome, UpdateHandoffOutcome::ProofReady);
    assert_eq!(end.progress.reports, 64);
    assert_eq!(
        end.progress.last.as_ref().map(Report::label),
        Some("gate:63")
    );
}

/// A SUCCESSOR THAT ENDS IS HEARD TO ITS LAST REPORT, AND STARTED ONLY ONCE IT PRESENTED A WINDOW:
/// the wait looked for the candidate's end before it read the channel, so reports a successor
/// wrote just before it ended went unread; and a report that its native launch had finished
/// (winit's first `resumed`) counted as its bytes starting here, though it then makes its first
/// window, surface and frame, which the node job's headless trial never does either, so a
/// successor that crashed there was no evidence against its bytes. What it left in the channel is
/// now heard before its death is judged (whether a stray writer still holds the channel or not),
/// and only a report of its first present says its bytes start here. NEGATIVE CONTROLS: without
/// hearing what is left, its last reports go unread ("THE LAST WORD"); with a finished launch
/// taken for a first present, the crash making its first window marks nothing ("THE FIRST
/// WINDOW").
#[test]
fn a_successor_that_ends_is_heard_to_its_last_report_and_started_only_once_it_presented() {
    use crate::app_native::PhysicalFailureShape;
    let crash = crate::ChildDeathEvidence::Signalled {
        signal: libc::SIGABRT,
    };
    let ended_after = |reports: &[Report], channel_held: bool| {
        let (proof_rd, proof_wr) = aterm_uds::pipe().expect("proof pipe");
        let proof_rd = OwnedFd::from(proof_rd);
        let mut successor = Reporter::on(proof_wr.as_raw_fd());
        for report in reports {
            assert!(successor.send(*report));
        }
        let stray = channel_held.then_some(proof_wr);
        let (_cancel_tx, cancel_rx) = std::sync::mpsc::sync_channel(1);
        let end = wait_handoff_ready(
            &proof_rd,
            expected_proof(),
            &cancel_rx,
            &[],
            Instant::now() + aterm_spec::wait::MUST_HAPPEN,
            PROGRESS_SILENCE_BOUND,
            &mut || None,
            &mut || true,
            #[cfg(target_os = "macos")]
            None,
        );
        drop(stray);
        end
    };
    let claimed = Report::new("claimed", false);
    let resumed = Report::new("resumed", true);
    let first_present = Report::new("first_present", true).with_presented(true);

    let end = ended_after(&[claimed, resumed], true);
    assert_eq!(end.outcome, UpdateHandoffOutcome::ChildDied);
    assert_eq!(
        end.progress.reports, 2,
        "THE LAST WORD: what it wrote before it ended is heard: {}",
        end.progress
    );
    assert!(
        end.progress.launched && !end.progress.presented,
        "THE FIRST WINDOW: its native launch finished, and it presented no window: {}",
        end.progress
    );
    assert!(
        PhysicalFailureShape::successor_failed_to_start(
            UpdateHandoffOutcome::ChildDied,
            crash,
            end.progress.start()
        ),
        "THE FIRST WINDOW: a successor that crashed making its first window failed to start here"
    );

    let end = ended_after(&[claimed, resumed, first_present], false);
    assert_eq!(end.outcome, UpdateHandoffOutcome::ChildDied);
    assert!(
        end.progress.presented,
        "THE LAST WORD: a first present written just before its end is heard: {}",
        end.progress
    );
    assert!(
        !PhysicalFailureShape::successor_failed_to_start(
            UpdateHandoffOutcome::ChildDied,
            crash,
            end.progress.start()
        ),
        "a successor that presented its first window started here, whatever ended it after"
    );
}

/// The outgoing side's revocation lands in the same turn of the wait in which the successor's
/// silence passes the bound: the attempt stands down as activity, never as the image's stall.
#[test]
fn an_outgoing_side_revocation_is_never_filed_as_the_successor_s_stall() {
    let (proof_rd, proof_wr) = aterm_uds::pipe().expect("proof pipe");
    let proof_rd = OwnedFd::from(proof_rd);
    assert!(Reporter::on(proof_wr.as_raw_fd()).send(Report::new("claimed", false)));
    let (cancel_tx, cancel_rx) = std::sync::mpsc::sync_channel(1);
    let silence = Duration::from_millis(50);
    let mut looks = 0;
    let end = wait_handoff_ready(
        &proof_rd,
        expected_proof(),
        &cancel_rx,
        &[],
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        silence,
        &mut || None,
        &mut || {
            looks += 1;
            // The first look comes before the report is read; by the second it has been.
            if looks == 2 {
                std::thread::sleep(silence * 2);
                cancel_tx.try_send(()).expect("queue the revocation");
            }
            false
        },
        #[cfg(target_os = "macos")]
        None,
    );
    assert_eq!(end.progress.reports, 1);
    assert_eq!(
        end.outcome,
        UpdateHandoffOutcome::ActivityRevoked,
        "{}",
        end.progress
    );
    assert_eq!(booked(end.outcome), HandoffFailureLane::ActivityRevoked);
    assert_eq!(
        HandoffFailureLane::classify(
            ApplyMode::Automatic,
            UpdateHandoffOutcome::Stalled,
            crate::ChildDeathEvidence::Unobserved,
            true,
        ),
        HandoffFailureLane::ActivityRevoked,
        "activity the parent saw during the wait is its own, whatever the wait ended on"
    );
    drop(proof_wr);
}

fn work(cpu_ms: u64, pageins: u64) -> Option<SuccessorWork> {
    Some(SuccessorWork {
        cpu_ns: cpu_ms * 1_000_000,
        pageins,
    })
}

/// What counts as working across a silence: a main thread computing (5% of a core and up), or a
/// successor reading its pages back under swap; an idle run loop's trickle does not.
#[test]
fn work_across_a_silence_is_cpu_or_page_ins_above_an_idle_trickle() {
    let wall = Duration::from_secs(3);
    let window = |cpu_ms, pageins| WorkWindow {
        cpu: Duration::from_millis(cpu_ms),
        pageins,
        wall,
    };
    assert!(window(150, 0).working(), "5% of a core is working");
    assert!(!window(149, 0).working());
    assert!(
        !window(30, 0).working(),
        "1%, above what idle AppKit processes used on m16, is not"
    );
    assert!(
        window(0, 64).working(),
        "a successor faulting its pages in works"
    );
    assert!(!window(0, 63).working(), "a few pages touched is not work");
    assert!(
        !WorkWindow {
            wall: Duration::ZERO,
            ..window(1, 0)
        }
        .working(),
        "no window, no judgment"
    );
}

/// The judge reads the successor's work across each silence: work carries it to a new silence,
/// none ends it, and a kernel that does not say is no evidence of work.
#[test]
fn a_successor_working_through_its_silence_gets_another_bound() {
    let start = Instant::now();
    let mut watch = ProgressWatch::new(start, PROGRESS_SILENCE_BOUND);
    assert!(!watch.mark_due(), "nothing to mark before a report");
    watch.hear(Report::new("resumed", true), start);
    assert!(watch.mark_due());
    watch.mark(work(1_000, 10), start);
    assert!(!watch.mark_due());
    let first = start + PROGRESS_SILENCE_BOUND;
    assert!(watch.silent_at(first));
    assert!(
        watch.worked_through(work(1_600, 10), first),
        "600 ms of CPU across the silence is work"
    );
    assert!(
        !watch.silent_at(first),
        "a new silence starts at the judgment"
    );
    let second = first + PROGRESS_SILENCE_BOUND;
    assert!(watch.silent_at(second));
    assert!(
        !watch.worked_through(work(1_610, 12), second),
        "10 ms and two page-ins is not"
    );
    let heard = watch.heard(second);
    assert_eq!(heard.worked_through, 1);
    assert_eq!(
        heard.quiet_for,
        PROGRESS_SILENCE_BOUND * 2,
        "quiet since the last report, not since the last judgment"
    );
    assert_eq!(
        heard.last_window,
        Some(WorkWindow {
            cpu: Duration::from_millis(10),
            pageins: 2,
            wall: PROGRESS_SILENCE_BOUND,
        })
    );
    let said = heard.to_string();
    assert!(
        said.contains("carried 1 silence(s)") && said.contains("10 ms CPU and 2 page-in(s)"),
        "{said}"
    );

    let mut unread = ProgressWatch::new(start, PROGRESS_SILENCE_BOUND);
    unread.hear(Report::new("claimed", false), start);
    unread.mark(None, start);
    assert!(
        !unread.worked_through(work(9_000, 9_000), first),
        "no work read where the silence began: judged by its length alone"
    );
    let mut gone = ProgressWatch::new(start, PROGRESS_SILENCE_BOUND);
    gone.hear(Report::new("claimed", false), start);
    gone.mark(work(0, 0), start);
    assert!(!gone.worked_through(None, first), "nor where it ended");
}

/// A successor that reported and then went quiet inside one long step, still computing, is not
/// ended by its silence; it is ended once its work stops too.
///
/// NEGATIVE CONTROL: end the wait at the first silence regardless of `worked_through` and it ends
/// with no silence carried.
#[test]
fn a_silent_successor_still_working_is_ended_only_once_its_work_stops() {
    let (proof_rd, proof_wr) = aterm_uds::pipe().expect("proof pipe");
    let proof_rd = OwnedFd::from(proof_rd);
    assert!(Reporter::on(proof_wr.as_raw_fd()).send(Report::new("resumed", true)));
    let (_cancel_tx, cancel_rx) = std::sync::mpsc::sync_channel(1);
    let mut reads = 0u64;
    let end = wait_handoff_ready(
        &proof_rd,
        expected_proof(),
        &cancel_rx,
        &[],
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        Duration::from_millis(50),
        // A second of CPU between reads for the first three reads (the mark and two silences),
        // then none: the step finished working and hung.
        &mut || {
            reads += 1;
            work(reads.min(3) * 1_000, 0)
        },
        &mut || false,
        #[cfg(target_os = "macos")]
        None,
    );
    assert_eq!(
        end.outcome,
        UpdateHandoffOutcome::Stalled,
        "{}",
        end.progress
    );
    assert_eq!(end.progress.worked_through, 2, "{}", end.progress);
    assert_eq!(reads, 4, "the mark, then one read per silence");
    assert_eq!(
        end.progress.last_window.map(|window| window.cpu),
        Some(Duration::ZERO)
    );
    drop(proof_wr);
}

/// A successor that keeps working silently is held only by the hang detector, which books a
/// launched one as timed out (Transient), not stalled; short of its first window, it failed to
/// start here all the same (`PhysicalFailureShape::successor_failed_to_start`), and once it
/// presented it did not. NEGATIVE CONTROL: with every timeout passed over, the successor that hung
/// making its first window marks nothing ("THE HANG DETECTOR").
#[test]
fn a_successor_that_keeps_working_silently_reaches_the_hang_detector() {
    let (proof_rd, proof_wr) = aterm_uds::pipe().expect("proof pipe");
    let proof_rd = OwnedFd::from(proof_rd);
    assert!(Reporter::on(proof_wr.as_raw_fd()).send(Report::new("resumed", true)));
    let (_cancel_tx, cancel_rx) = std::sync::mpsc::sync_channel(1);
    let mut cpu_ms = 0;
    let end = wait_handoff_ready(
        &proof_rd,
        expected_proof(),
        &cancel_rx,
        &[],
        Instant::now() + Duration::from_millis(400),
        Duration::from_millis(50),
        &mut || {
            cpu_ms += 1_000;
            work(cpu_ms, 0)
        },
        &mut || false,
        #[cfg(target_os = "macos")]
        None,
    );
    assert_eq!(
        end.outcome,
        UpdateHandoffOutcome::TimedOut,
        "{}",
        end.progress
    );
    assert!(end.progress.worked_through >= 2, "{}", end.progress);
    assert_eq!(end.progress.start(), SuccessorStart::Launched);
    assert!(
        PhysicalFailureShape::successor_failed_to_start(
            end.outcome,
            crate::ChildDeathEvidence::Unobserved,
            end.progress.start()
        ),
        "THE HANG DETECTOR: a successor that launched and was still short of its first window \
         when the wait ran out failed to start here: {}",
        end.progress
    );
    assert!(
        !PhysicalFailureShape::successor_failed_to_start(
            end.outcome,
            crate::ChildDeathEvidence::Unobserved,
            SuccessorStart::Presented
        ),
        "one that presented its first window before the wait ran out started here"
    );
    drop(proof_wr);
}

/// A WAIT THAT ENDS SHORT OF THE PROOF HEARS EVERY REPORT LEFT FIRST: only a candidate's end
/// heard what was still in the readiness channel, so a worker descheduled across the deadline
/// (a machine in swap) or past a revocation judged the hang detector's outcome and the first
/// present without the reports the successor had already written: one that presented read as one
/// that never launched (`Stalled`), marking bytes that start here, and its present cleared no
/// earlier mark. Every end short of the proof now hears them first, without waiting. A proof
/// behind them at the hang detector is the successor's proof (it did not hang, however late this
/// thread ran to read it); behind a revocation it decides nothing. Every report and the proof are
/// in the channel before the wait starts, at a deadline already past or with the revocation
/// already queued, so the wait ends at its first look having read nothing itself. NEGATIVE
/// CONTROL: without the drain, each end has heard no report ("THE REPORTS LEFT").
#[test]
fn a_wait_that_ends_short_of_the_proof_hears_every_report_left_first() {
    let reports = [
        Report::new("claimed", false),
        Report::new("resumed", true),
        Report::new("first_present", true).with_presented(true),
    ];
    let ended = |proven: bool, revoked: bool| {
        let (proof_rd, proof_wr) = aterm_uds::pipe().expect("proof pipe");
        let proof_rd = OwnedFd::from(proof_rd);
        let (cancel_tx, cancel_rx) = std::sync::mpsc::sync_channel(1);
        let mut successor = Reporter::on(proof_wr.as_raw_fd());
        for report in reports {
            assert!(successor.send(report));
        }
        if proven {
            let wire = expected_proof().to_wire();
            // SAFETY: one bounded write of the fixed proof wire to the test-owned pipe.
            let wrote =
                unsafe { libc::write(proof_wr.as_raw_fd(), wire.as_ptr().cast(), wire.len()) };
            assert_eq!(wrote, isize::try_from(wire.len()).unwrap_or(-1));
        }
        let deadline = if revoked {
            cancel_tx.try_send(()).expect("queue the revocation");
            Instant::now() + aterm_spec::wait::MUST_HAPPEN
        } else {
            Instant::now()
        };
        let end = wait_handoff_ready(
            &proof_rd,
            expected_proof(),
            &cancel_rx,
            &[],
            deadline,
            PROGRESS_SILENCE_BOUND,
            &mut || None,
            &mut || false,
            #[cfg(target_os = "macos")]
            None,
        );
        drop(proof_wr);
        end
    };
    for (proven, revoked) in [(false, false), (true, false), (false, true), (true, true)] {
        let end = ended(proven, revoked);
        let case = format!(
            "{}, proof written: {proven}: {}",
            if revoked {
                "revoked"
            } else {
                "past the deadline"
            },
            end.progress
        );
        assert_eq!(
            end.progress.reports, 3,
            "THE REPORTS LEFT: every report written before the wait ended is heard ({case})"
        );
        assert_eq!(
            end.progress.start(),
            SuccessorStart::Presented,
            "THE REPORTS LEFT: its first present is heard ({case})"
        );
        assert_eq!(
            end.outcome,
            match (proven, revoked) {
                (_, true) => UpdateHandoffOutcome::ActivityRevoked,
                (true, false) => UpdateHandoffOutcome::ProofReady,
                (false, false) => UpdateHandoffOutcome::TimedOut,
            },
            "THE PROOF BEHIND: a revocation stands; at the hang detector a proof behind the \
             reports is the proof, and with none the hang detector judges a finished launch \
             ({case})"
        );
        assert!(
            !PhysicalFailureShape::successor_failed_to_start(
                end.outcome,
                crate::ChildDeathEvidence::Unobserved,
                end.progress.start()
            ),
            "a successor that presented its first window started here ({case})"
        );
    }
}

/// A report written inside the bound while this thread ran late (it was descheduled, or the
/// candidate check was slow) is read before the silence is judged, never left in the pipe while
/// the attempt is booked against the successor's image.
///
/// NEGATIVE CONTROL: judge the silence before the turn's poll (the order this replaced) and the
/// wait ends `Stalled` with one report heard.
#[test]
fn a_report_waiting_in_the_pipe_is_heard_before_the_silence_is_judged() {
    let (proof_rd, proof_wr) = aterm_uds::pipe().expect("proof pipe");
    let proof_rd = OwnedFd::from(proof_rd);
    let mut successor = Reporter::on(proof_wr.as_raw_fd());
    assert!(successor.send(Report::new("claimed", false)));
    let (_cancel_tx, cancel_rx) = std::sync::mpsc::sync_channel(1);
    let silence = Duration::from_millis(100);
    let mut looks = 0;
    let end = wait_handoff_ready(
        &proof_rd,
        expected_proof(),
        &cancel_rx,
        &[],
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        silence,
        &mut || None,
        &mut || {
            looks += 1;
            // The first look comes before `claimed` is read; by the second it has been.
            if looks == 2 {
                std::thread::sleep(silence / 2);
                assert!(successor.send(Report::new("armed", false)));
                std::thread::sleep(silence);
            }
            false
        },
        #[cfg(target_os = "macos")]
        None,
    );
    assert_eq!(end.progress.reports, 2, "{}", end.progress);
    assert_eq!(end.progress.last.as_ref().map(Report::label), Some("armed"));
    assert_eq!(
        end.outcome,
        UpdateHandoffOutcome::Stalled,
        "{}",
        end.progress
    );
    drop(proof_wr);
}

/// A handed session that dies as the successor's silence expires is the desk changing, filed as
/// activity, never as the successor's stall.
///
/// NEGATIVE CONTROL: judge the silence before the turn's poll and the wait ends `Stalled`.
#[test]
fn a_session_death_as_the_silence_expires_is_activity() {
    let (proof_rd, proof_wr) = aterm_uds::pipe().expect("proof pipe");
    let proof_rd = OwnedFd::from(proof_rd);
    assert!(Reporter::on(proof_wr.as_raw_fd()).send(Report::new("claimed", false)));
    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    let mut slave = Some(slave);
    let (_cancel_tx, cancel_rx) = std::sync::mpsc::sync_channel(1);
    let silence = Duration::from_millis(50);
    let mut looks = 0;
    let end = wait_handoff_ready(
        &proof_rd,
        expected_proof(),
        &cancel_rx,
        &[master],
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        silence,
        &mut || None,
        &mut || {
            looks += 1;
            if looks == 2 {
                std::thread::sleep(silence * 2);
                aterm_pty::close_fd(slave.take().expect("the slave, once"));
                let shown = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
                while !super::handoff_masters_closed(&[(1, master, 4242)]) {
                    assert!(Instant::now() < shown, "the hang-up shows");
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            false
        },
        #[cfg(target_os = "macos")]
        None,
    );
    assert_eq!(end.progress.reports, 1);
    assert_eq!(
        end.outcome,
        UpdateHandoffOutcome::ActivityRevoked,
        "{}",
        end.progress
    );
    if let Some(slave) = slave {
        aterm_pty::close_fd(slave);
    }
    aterm_pty::close_fd(master);
    drop(proof_wr);
}
