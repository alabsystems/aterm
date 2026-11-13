// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn oscillation_is_bounded_and_does_not_hide_the_terminal_record() {
    let mut progress = Progress::default();
    let mut record = Record {
        phase: Phase::Waiting,
        control_pending: true,
        control_failed: false,
        restore_pending: true,
        manifest_windows: 1,
        pending_adoptions: 0,
        first_present_pending: true,
        unpainted_windows: 1,
    };
    let mut emitted = Vec::new();
    for n in 0..1000 {
        record.unpainted_windows = n % 2;
        progress.note(record, &mut |r| emitted.push(r));
    }
    assert_eq!(emitted.len(), usize::from(Progress::LIMIT));
    record.phase = Phase::Rejected;
    progress.note(record, &mut |r| emitted.push(r));
    progress.note(record, &mut |r| emitted.push(r));
    assert_eq!(emitted.len(), usize::from(Progress::LIMIT) + 1);
    assert_eq!(emitted.last().unwrap().phase, Phase::Rejected);
    progress.clear();
    record.phase = Phase::Waiting;
    progress.note(record, &mut |r| emitted.push(r));
    assert_eq!(emitted.len(), usize::from(Progress::LIMIT) + 2);
}

/// Every whole report waiting in `read`, without blocking.
#[cfg(unix)]
fn waiting_reports(read: &std::io::PipeReader) -> Vec<Report> {
    use std::os::fd::AsRawFd as _;
    let fd = read.as_raw_fd();
    // SAFETY: F_GETFL/F_SETFL on a descriptor this test owns.
    unsafe {
        libc::fcntl(
            fd,
            libc::F_SETFL,
            libc::fcntl(fd, libc::F_GETFL) | libc::O_NONBLOCK,
        )
    };
    let mut reports = Vec::new();
    loop {
        let mut wire = [0u8; crate::seamless::READY_WIRE_LEN];
        // SAFETY: a bounded read into a fixed local buffer.
        let n = unsafe { libc::read(fd, wire.as_mut_ptr().cast(), wire.len()) };
        if n != isize::try_from(wire.len()).unwrap_or(-1) {
            return reports;
        }
        reports.push(Report::from_wire(&wire).expect("only reports were written"));
    }
}

#[cfg(unix)]
#[test]
fn a_report_round_trips_and_never_reads_as_the_proof() {
    let report = Report::new("first_present", true).with_presented(true);
    assert_eq!(Report::from_wire(&report.to_wire()), Some(report));
    assert_eq!(report.label(), "first_present");
    assert!(report.launched() && report.presented());
    let resumed = Report::from_wire(&Report::new("resumed", true).to_wire()).expect("a report");
    assert!(
        resumed.launched() && !resumed.presented(),
        "launched is not presented"
    );
    let early = Report::from_wire(&Report::new("intake", false).to_wire()).expect("a report");
    assert!(!early.launched() && !early.presented());

    let proof = crate::seamless::adoption_proof("n", 2, "abcdef0", &[1; 32], &[2; 32], &[])
        .expect("a bounded proof");
    assert_eq!(
        Report::from_wire(&proof.to_wire()),
        None,
        "the proof is no report"
    );
    assert_eq!(
        crate::seamless::AdoptionProof::from_wire(&report.to_wire()),
        None,
        "and a report is no proof"
    );

    let long = "x".repeat(2 * crate::seamless::READY_WIRE_LEN);
    assert_eq!(Report::new(&long, false).label().len(), REPORT_LABEL_MAX);
    let mut later = Report::new("gate:waiting", false).to_wire();
    later[4] |= 0x80;
    later[6] = 0x07;
    let read = Report::from_wire(&later).expect("a later successor's report is a report");
    assert!(
        !read.launched() && !read.presented(),
        "an unknown flag bit is ignored"
    );
    assert_eq!(read.label(), "?ate:waiting");
}

/// A report is written by the successor's main thread whether or not the outgoing process is
/// reading: the cap keeps every report, with the claim and the proof, inside one pipe buffer.
#[cfg(unix)]
#[test]
fn a_reporter_stops_at_its_cap_and_never_waits_on_an_unread_channel() {
    use std::os::fd::AsRawFd as _;
    let (read, write) = aterm_uds::pipe().expect("pipe");
    let mut reporter = Reporter::on(write.as_raw_fd());
    let sent = (0..MAX_REPORTS + 8)
        .filter(|n| reporter.send(Report::new(&format!("r{n}"), false)))
        .count();
    assert_eq!(sent, usize::from(MAX_REPORTS));
    let reports = waiting_reports(&read);
    assert_eq!(reports.len(), usize::from(MAX_REPORTS));
    assert_eq!(reports[0].label(), "r0");
}

/// The only test that arms the process-wide channel. Other tests' readiness visits may report
/// into it while it is armed, so it looks for its own reports rather than counting.
#[cfg(unix)]
#[test]
fn an_offered_readiness_signal_reports_until_it_drops_and_an_unoffered_one_never() {
    use std::os::fd::IntoRawFd as _;
    let admit = |offered: bool, fd: i32| {
        let mut env = crate::handoff_env::HandoffEnv::default();
        if offered {
            env.set(
                crate::seamless::ENV_READY_CAPS,
                format!("future1, {}", crate::seamless::READY_CAPS_PROGRESS),
            );
        }
        env.set(crate::seamless::ENV_READY_FD, fd.to_string());
        let signal = crate::seamless::take_ready_fd_from(
            &mut env,
            Some("progress-nonce".to_string()),
            Some([1; 32]),
            Some([2; 32]),
            Some(crate::seamless::HandoffTarget {
                build: 2,
                commit: "abcdef0".to_string(),
            }),
            &[],
        )
        .expect("the channel is admitted");
        assert!(
            !env.present(crate::seamless::ENV_READY_CAPS),
            "the offer is taken"
        );
        signal
    };

    let (read, write) = aterm_uds::pipe().expect("pipe");
    let fd = write.into_raw_fd();
    let signal = admit(true, fd);
    assert_eq!(armed_fd(), Some(fd));
    report("intake");
    drop(signal);
    assert_ne!(
        armed_fd(),
        Some(fd),
        "the signal disarms its channel before closing it"
    );
    report("after_drop");
    let labels = waiting_reports(&read)
        .iter()
        .map(|report| report.label().to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        labels.first().map(String::as_str),
        Some("armed"),
        "{labels:?}"
    );
    assert!(labels.iter().any(|label| label == "intake"), "{labels:?}");
    assert!(
        !labels.iter().any(|label| label == "after_drop"),
        "{labels:?}"
    );

    let (read, write) = aterm_uds::pipe().expect("pipe");
    let fd = write.into_raw_fd();
    let signal = admit(false, fd);
    assert_ne!(armed_fd(), Some(fd), "an older parent offered nothing");
    drop(signal);
    assert!(
        waiting_reports(&read).is_empty(),
        "so nothing but the proof is written"
    );

    let (read, write) = aterm_uds::pipe().expect("pipe");
    the_proof_is_the_last_frame_a_racing_report_cannot_follow(
        admit(true, write.into_raw_fd()),
        &read,
    );
}

/// The proof is the channel's last frame: `signal_proof` disarms, under the lock every report
/// takes, before it writes. This test holds that lock, as a report in flight on the successor's
/// main thread would: the proof waits for it, and the report lands before the proof, never
/// after (a parent stops reading at the proof). Part of the arming test above, which is the
/// only one that arms the process-wide channel.
///
/// NEGATIVE CONTROL: drop the disarm from `signal_proof` and the proof is written while the
/// report holds the lock, so the quarter second below sees it.
#[cfg(unix)]
fn the_proof_is_the_last_frame_a_racing_report_cannot_follow(
    signal: crate::seamless::ReadySignal,
    read: &std::io::PipeReader,
) {
    use std::os::fd::AsRawFd as _;
    let fd = signal.raw_fd();
    assert_eq!(armed_fd(), Some(fd));
    let mut in_flight = ARMED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Other tests' reports may have reached the armed channel before the lock was taken.
    let before = waiting_reports(read);
    assert_eq!(
        before.first().map(Report::label),
        Some("armed"),
        "{before:?}"
    );
    let proof =
        crate::seamless::adoption_proof("progress-nonce", 2, "abcdef0", &[1; 32], &[2; 32], &[])
            .expect("a bounded proof");
    let writer = std::thread::spawn(move || signal.signal_proof(proof));
    let mut readable = libc::pollfd {
        fd: read.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one pollfd on a descriptor this test owns, for at most a quarter second.
    let seen = unsafe { libc::poll(&raw mut readable, 1, 250) };
    assert_eq!(
        seen, 0,
        "no proof may be written while a report holds the reports' lock"
    );
    assert!(
        in_flight
            .as_mut()
            .expect("still armed while the proof waits")
            .send(Report::new("racing", false)),
        "the report in flight lands"
    );
    drop(in_flight);
    assert!(writer.join().expect("the proof writer"), "the proof lands");
    assert_ne!(armed_fd(), Some(fd), "disarmed by the proof");
    let mut frames = Vec::new();
    loop {
        let mut wire = [0u8; crate::seamless::READY_WIRE_LEN];
        // SAFETY: a bounded read into a fixed local buffer; the write end closed with the
        // signal, so this ends at EOF.
        let n = unsafe { libc::read(read.as_raw_fd(), wire.as_mut_ptr().cast(), wire.len()) };
        if n != isize::try_from(wire.len()).unwrap_or(-1) {
            assert_eq!(n, 0, "whole frames, then EOF");
            break;
        }
        frames.push(wire);
    }
    assert_eq!(
        frames.last(),
        Some(&proof.to_wire()),
        "the proof is the last frame"
    );
    assert_eq!(
        frames
            .iter()
            .filter_map(Report::from_wire)
            .map(|report| report.label().to_string())
            .collect::<Vec<_>>()
            .first()
            .map(String::as_str),
        Some("racing"),
        "the report in flight landed before the proof"
    );
}
