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

#[cfg(unix)]
#[derive(Clone, Copy)]
enum Drain {
    Available,
    Eof,
}

#[cfg(unix)]
const NO_PROGRESS: std::time::Duration = std::time::Duration::from_secs(60);

#[cfg(unix)]
fn remaining(since: std::time::Instant) -> std::io::Result<std::time::Duration> {
    NO_PROGRESS
        .checked_sub(since.elapsed())
        .filter(|left| !left.is_zero())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "no readiness bytes or EOF for one minute",
            )
        })
}

/// Partial reads stay in their frame. Only a zero-byte read between frames proves EOF;
/// interruptions and readiness wakes do not renew the no-progress bound.
#[cfg(unix)]
fn read_frames(
    drain: Drain,
    mut read: impl FnMut(&mut [u8]) -> std::io::Result<usize>,
    mut wait: impl FnMut(std::time::Duration) -> std::io::Result<()>,
) -> std::io::Result<Vec<[u8; crate::seamless::READY_WIRE_LEN]>> {
    let mut frames = Vec::new();
    let mut wire = [0u8; crate::seamless::READY_WIRE_LEN];
    let mut filled = 0;
    let mut progress = std::time::Instant::now();
    loop {
        remaining(progress)?;
        match read(&mut wire[filled..]) {
            Ok(0) if filled == 0 => return Ok(frames),
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    format!("readiness EOF after {filled} of {} frame bytes", wire.len()),
                ));
            }
            Ok(n) => {
                if n > wire.len() - filled {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "readiness read exceeded its buffer",
                    ));
                }
                filled += n;
                progress = std::time::Instant::now();
                if filled == wire.len() {
                    frames.push(wire);
                    wire = [0; crate::seamless::READY_WIRE_LEN];
                    filled = 0;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if matches!(drain, Drain::Available) && filled == 0 {
                    return Ok(frames);
                }
                match wait(remaining(progress)?) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(error) => return Err(error),
                }
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(unix)]
fn nonblocking(read: &std::io::PipeReader) -> std::io::Result<i32> {
    use std::os::fd::AsRawFd as _;
    let fd = read.as_raw_fd();
    // SAFETY: F_GETFL/F_SETFL on a descriptor this test owns.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(fd)
}

#[cfg(unix)]
fn read_pipe(fd: i32, bytes: &mut [u8]) -> std::io::Result<usize> {
    // SAFETY: the caller keeps the pipe open; bytes is a live bounded output buffer.
    let n = unsafe { libc::read(fd, bytes.as_mut_ptr().cast(), bytes.len()) };
    if n < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(usize::try_from(n).expect("a nonnegative read length"))
    }
}

#[cfg(unix)]
fn wait_readable(fd: i32, left: std::time::Duration) -> std::io::Result<()> {
    let mut ready = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let millis = i32::try_from(left.as_millis().max(1)).unwrap_or(i32::MAX);
    // SAFETY: one pollfd on the caller's live pipe, within the remaining hang bound.
    let n = unsafe { libc::poll(&raw mut ready, 1, millis) };
    if n < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if n > 0
        && (ready.revents & (libc::POLLERR | libc::POLLNVAL) != 0
            || ready.revents & (libc::POLLIN | libc::POLLHUP) == 0)
    {
        return Err(std::io::Error::other(format!(
            "readiness poll returned unexpected revents={:#x}",
            ready.revents
        )));
    }
    // HUP is a reason to read again: buffered bytes still precede the real zero-byte EOF.
    Ok(())
}

/// Every report currently available in `read`; the owned reader stays nonblocking.
#[cfg(unix)]
fn waiting_reports(read: &std::io::PipeReader) -> Vec<Report> {
    let fd = nonblocking(read).expect("nonblocking readiness reader");
    read_frames(
        Drain::Available,
        |bytes| read_pipe(fd, bytes),
        |left| wait_readable(fd, left),
    )
    .expect("whole available readiness frames")
    .iter()
    .map(|wire| Report::from_wire(wire).expect("only reports were written"))
    .collect()
}

#[cfg(unix)]
fn frames_until_eof(
    read: &std::io::PipeReader,
) -> std::io::Result<Vec<[u8; crate::seamless::READY_WIRE_LEN]>> {
    let fd = nonblocking(read)?;
    read_frames(
        Drain::Eof,
        |bytes| {
            // A sibling fork may hold a writer until exec on a kernel that ignores
            // close-on-fork. Wait while descendants work; only real bytes or EOF advance
            // the framed reader, and real read errors still fail it.
            aterm_spec::wait::until_while_descendants_work(
                "the readiness channel's next bytes or EOF",
                || match read_pipe(fd, bytes) {
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                        ) =>
                    {
                        None
                    }
                    result => Some(result),
                },
            )
        },
        |left| wait_readable(fd, left),
    )
}

/// An open duplicate is not EOF, even after the original writer dropped. Observe the reader's
/// WouldBlock boundary before releasing that duplicate, then require the real EOF.
#[cfg(unix)]
#[test]
fn a_frame_drain_waits_for_the_last_writer_to_close() {
    use std::io::Write as _;
    use std::os::fd::AsRawFd as _;
    let (read, mut write) = aterm_uds::pipe().expect("pipe");
    let fd = nonblocking(&read).expect("nonblocking pipe");
    let mut empty = [0; crate::seamless::READY_WIRE_LEN];
    let pending = read_pipe(fd, &mut empty).expect_err("an open empty pipe is not EOF");
    assert_eq!(pending.kind(), std::io::ErrorKind::WouldBlock);
    let held = aterm_uds::kept_out(|| write.try_clone()).expect("retain a writer");
    let expected = Report::new("before_eof", false).to_wire();
    write.write_all(&expected).expect("whole report");
    drop(write);
    enum Event {
        Waiting,
        Finished(std::io::Result<Vec<[u8; crate::seamless::READY_WIRE_LEN]>>),
    }
    let (events, next) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::sync_channel(0);
    let reader = std::thread::spawn(move || {
        let fd = read.as_raw_fd();
        let mut observed = false;
        let result = read_frames(
            Drain::Eof,
            |bytes| read_pipe(fd, bytes),
            |left| {
                if !observed {
                    observed = true;
                    events.send(Event::Waiting).expect("announce WouldBlock");
                    released.recv_timeout(left).map_err(std::io::Error::other)?;
                }
                wait_readable(fd, left)
            },
        );
        events
            .send(Event::Finished(result))
            .expect("return the drain's result");
    });
    match next.recv_timeout(NO_PROGRESS).expect("the reader answered") {
        Event::Waiting => {}
        Event::Finished(result) => panic!("the open writer was treated as EOF: {result:?}"),
    }
    assert!(
        matches!(next.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)),
        "WouldBlock was not accepted as EOF"
    );
    drop(held);
    release.send(()).expect("the final writer is closed");
    let frames = match next
        .recv_timeout(NO_PROGRESS)
        .expect("the reader completed")
    {
        Event::Finished(result) => result.expect("whole frames and real EOF"),
        Event::Waiting => panic!("the last-writer boundary was already observed"),
    };
    reader.join().expect("the frame reader");
    assert_eq!(frames, [expected]);
}

#[cfg(unix)]
#[test]
fn a_frame_drain_keeps_partial_bytes_across_interruptions() {
    let wire = Report::new("fragmented", false).to_wire();
    let mut step = 0;
    let mut waits = 0;
    let frames = read_frames(
        Drain::Eof,
        |bytes| {
            step += 1;
            match step {
                1 => Err(std::io::Error::from(std::io::ErrorKind::Interrupted)),
                2 => {
                    bytes[..3].copy_from_slice(&wire[..3]);
                    Ok(3)
                }
                3 => Err(std::io::Error::from(std::io::ErrorKind::WouldBlock)),
                4 => {
                    bytes.copy_from_slice(&wire[3..]);
                    Ok(wire.len() - 3)
                }
                5 => Ok(0),
                _ => panic!("EOF is terminal"),
            }
        },
        |left| {
            assert!(!left.is_zero() && left <= NO_PROGRESS);
            waits += 1;
            Err(std::io::Error::from(std::io::ErrorKind::Interrupted))
        },
    )
    .expect("partial frames survive interrupted reads and waits");
    assert_eq!(waits, 1);
    assert_eq!(frames, [wire]);
}

#[cfg(unix)]
#[test]
fn a_frame_drain_rejects_truncated_eof_and_preserves_io_errors() {
    let mut first = true;
    let error = read_frames(
        Drain::Eof,
        |bytes| {
            if first {
                first = false;
                bytes[0] = 1;
                Ok(1)
            } else {
                Ok(0)
            }
        },
        |_| panic!("the truncated input never waits"),
    )
    .expect_err("a partial frame is not EOF");
    assert_eq!(error.kind(), std::io::ErrorKind::UnexpectedEof);
    let error = read_frames(
        Drain::Eof,
        |_| Err(std::io::Error::from_raw_os_error(libc::EBADF)),
        |_| panic!("a real read error never waits"),
    )
    .expect_err("a real read error is not EOF");
    assert_eq!(error.raw_os_error(), Some(libc::EBADF));
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
/// main thread would. The write-boundary observer requires disarming before the actual proof
/// write, independent of thread scheduling; the report in flight still precedes that proof.
/// Part of the arming test above, the only test that arms the process-wide channel.
///
/// NEGATIVE CONTROL: remove the pre-write disarm; the observer fails even though Drop later
/// disarms the signal. No absence window can credit a writer that was merely not scheduled.
#[cfg(unix)]
fn the_proof_is_the_last_frame_a_racing_report_cannot_follow(
    signal: crate::seamless::ReadySignal,
    read: &std::io::PipeReader,
) {
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
    let (finished, done) = std::sync::mpsc::channel();
    let writer = std::thread::spawn(move || {
        let wrote = signal.signal_proof_observing(proof, || {
            assert_ne!(
                armed_fd(),
                Some(fd),
                "the channel is disarmed before the actual proof write"
            );
        });
        let _ = finished.send(());
        wrote
    });
    assert!(
        in_flight
            .as_mut()
            .expect("still armed while the proof waits")
            .send(Report::new("racing", false)),
        "the report in flight lands"
    );
    drop(in_flight);
    match done.recv_timeout(NO_PROGRESS) {
        Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {}
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            panic!("the proof writer made no progress for one minute")
        }
    }
    assert!(writer.join().expect("the proof writer"), "the proof lands");
    assert_ne!(armed_fd(), Some(fd), "disarmed by the proof");
    let frames = frames_until_eof(read).expect("whole readiness frames, then real EOF");
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
