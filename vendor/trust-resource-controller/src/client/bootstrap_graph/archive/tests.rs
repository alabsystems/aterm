// SPDX-License-Identifier: Apache-2.0 OR MIT
// Copyright 2026 Andrew Yates

//! Actual socket/state DATA controls. No test constructs a CompletedArchive,
//! original Host/graph, SDK, native retirement, loan return or producer owner.
use std::io::Write as _;
use std::os::unix::net::UnixStream;

use super::*;

const DIGEST: [u8; 32] = [7; 32];
fn request() -> wire::Request {
    let query = crate::bootstrap_graph::query::tests::request();
    let file = |path: &[u8], inode| crate::MetadataFile {
        path: path.to_vec(),
        sha256: [1; 32],
        bytes: 7,
        device: 1,
        inode,
    };
    wire::Request {
        key: query.key,
        gnu: query.gnu,
        sdk_root: b"/sdk".to_vec(),
        driver: file(b"/sdk/bin/targo", 3),
        host: "fixture-machine".into(),
        boot: "fixture-boot".into(),
        source_root: b"/source".to_vec(),
        source_directory: crate::FileIdentity { device: 1, inode: 4 },
        declarations: [
            file(b"/source/src/stage0", 5),
            file(b"/source/bootstrap/trust-stage0/seed-ledger.toml", 6),
            file(b"/source/bootstrap/trust-stage0/dist/2026-11-07/channel-rust-trust.toml", 7),
        ],
        date: "2026-11-07".into(),
        version: "0.13.0-trust".into(),
        archives: (0..wire::ARCHIVE_COUNT)
            .map(|index| wire::ArchivePin {
                relative: wire::relative_archive("2026-11-07", "0.13.0-trust", index).unwrap(),
                sha256: [index as u8; 32],
            })
            .collect(),
        contents_directory: crate::FileIdentity { device: 1, inode: 8 },
        timeout_ns: wire::MAX_WORK_NS,
    }
}
fn prepared(request: &wire::Request) -> MetadataPrepared {
    MetadataPrepared {
        root_attempt: request.key.graph.query.binding.root_attempt,
        member_attempt: request.key.operation,
        root: request.sdk_root.clone(),
        policy: wire::PREPARED_POLICY.into(),
        scope: wire::PREPARED_SCOPE.into(),
        executable: request.driver.clone(),
        runtime: vec![],
        loader_paths: vec![],
        system_dependencies: vec![],
        host: request.host.clone(),
        boot: request.boot.clone(),
    }
}
fn completion(request: &wire::Request) -> MetadataCompletion {
    MetadataCompletion {
        finished: crate::StatusFrame {
            root_attempt: request.key.graph.query.binding.root_attempt,
            member_attempt: request.key.operation,
            status: crate::Status::Finished {
                outcome: crate::Outcome::Exit(0),
                cause: crate::CompletionCause::Exited,
                runtime_ns: 8,
            },
        },
        inputs_unchanged: Ok(()),
    }
}
fn terminal(request: &wire::Request) -> wire::Terminal {
    wire::Terminal::Completed {
        completion: Box::new(completion(request)),
        observation: crate::accounting::Observation::uncollected(),
    }
}
fn events(request: &wire::Request) -> Vec<wire::Event> {
    vec![
        wire::Event::Prepared(Box::new(prepared(request))),
        wire::Event::Started(crate::bootstrap_graph::query::tests::started()),
        wire::Event::Output { stream: wire::Stream::Stdout, offset: 0, bytes: vec![0, 255, 1] },
        wire::Event::Eof { stream: wire::Stream::Stdout, bytes: 3 },
        wire::Event::Eof { stream: wire::Stream::Stderr, bytes: 0 },
    ]
}
fn frames(request: &wire::Request) -> Vec<Vec<u8>> {
    let mut frames = events(request)
        .iter()
        .map(|event| event.encode(request, DIGEST).unwrap())
        .collect::<Vec<_>>();
    frames.push(terminal(request).encode(request, DIGEST).unwrap());
    frames
}
fn expected(request: &wire::Request) -> ExpectedPreparation<'_> {
    ExpectedPreparation { request, digest: DIGEST, host: &request.host, boot: &request.boot }
}
fn framed(bytes: &[u8]) -> Vec<u8> {
    let mut output = (bytes.len() as u32).to_be_bytes().to_vec();
    output.extend(bytes);
    output
}
fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(5)
}

#[test]
fn actual_socket_full_terminal_ack_and_two_eofs_remain_data_only() {
    let request = request();
    let term = terminal(&request);
    let frames = frames(&request);
    let raw = frames.iter().flat_map(|frame| framed(frame)).collect::<Vec<_>>();
    let (left, right) = UnixStream::pair().unwrap();
    left.set_nonblocking(true).unwrap();
    right.set_nonblocking(true).unwrap();
    let until = deadline();
    let server_request = request.clone();
    let server_terminal = term.clone();
    let server = std::thread::spawn(move || {
        let mut channel = Channel::new(right.try_clone().unwrap()).unwrap();
        for frame in frames {
            channel.send(&frame, until).unwrap();
        }
        right.shutdown(std::net::Shutdown::Write).unwrap();
        let ack = channel.receive(until).unwrap();
        wire::Ack::decode(&ack, &server_request, DIGEST, &server_terminal).unwrap();
        while !channel.at_eof().unwrap() {
            time(until).unwrap();
            std::thread::sleep(Duration::from_millis(1));
        }
        ack
    });
    let mut channel = Channel::new(left).unwrap();
    let mut transcript = Transcript::new();
    let mut queries = super::super::QueryState::new();
    queries.begin().unwrap();
    let (completion, accounting) =
        receive_operation(&mut channel, expected(&request), until, &mut transcript).unwrap();
    assert_eq!(accounting, crate::accounting::Observation::uncollected());
    require_success(&completion).unwrap();
    acknowledge(&mut channel, &request, DIGEST, &term, until, &mut transcript).unwrap();
    assert_eq!(server.join().unwrap(), wire::Ack::encode(&request, DIGEST, &term).unwrap());
    assert_eq!(transcript.stdout, [0, 255, 1]);
    assert!(transcript.stderr.is_empty());
    assert_eq!(transcript.raw, raw);
    assert_eq!(transcript.bytes_seen, raw.len() as u64);
    assert_eq!(transcript.frames_seen, 6);
    // Neither successful decoding nor ACK is the opaque completed client path.
    assert!(queries.check(2).is_err());
    assert_eq!(queries.active, Some(1));
}

fn receive_frames(
    frames: Vec<Vec<u8>>,
    raw_tail: Vec<u8>,
) -> (io::Result<(MetadataCompletion, crate::accounting::Observation)>, Transcript, Channel) {
    let request = request();
    let until = deadline();
    let (left, mut right) = UnixStream::pair().unwrap();
    left.set_nonblocking(true).unwrap();
    right.set_nonblocking(true).unwrap();
    let cancel = left.try_clone().unwrap();
    let server = std::thread::spawn(move || -> io::Result<()> {
        let mut channel = Channel::new(right.try_clone().unwrap()).unwrap();
        for frame in frames {
            channel.send(&frame, until)?;
        }
        let mut offset = 0;
        while offset < raw_tail.len() {
            time(until)?;
            match right.write(&raw_tail[offset..]) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(count) => offset += count,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(1))
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
        right.shutdown(std::net::Shutdown::Write)
    });
    let mut channel = Channel::new(left).unwrap();
    let mut transcript = Transcript::new();
    let result = receive_operation(&mut channel, expected(&request), until, &mut transcript);
    if result.is_err() {
        match cancel.shutdown(std::net::Shutdown::Both) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotConnected => {}
            Err(error) => panic!("owned fixture socket shutdown failed: {error}"),
        }
    }
    let sent = server.join().unwrap();
    if result.is_ok() {
        sent.unwrap();
    }
    (result, transcript, channel)
}
#[test]
fn foreign_request_or_digest_poison_and_preserve_actual_frame_prefix() {
    let request = request();
    for foreign in [false, true] {
        let mut other = request.clone();
        if foreign {
            other.contents_directory.inode += 10;
        }
        let frame = wire::Event::Prepared(Box::new(prepared(&other)))
            .encode(&other, if foreign { DIGEST } else { [8; 32] })
            .unwrap();
        let raw = framed(&frame);
        let (result, transcript, channel) = receive_frames(vec![frame], vec![]);
        assert!(result.is_err());
        assert!(channel.poisoned());
        assert!(transcript.failed);
        assert_eq!(transcript.raw, raw);
        assert_eq!(transcript.bytes_seen, raw.len() as u64);
        assert_eq!(transcript.frames_seen, 1);
    }
}
#[test]
fn partial_bad_length_and_partial_payload_preserve_physical_bytes() {
    let tails = [vec![0, 0], vec![0, 0, 0, 0], vec![0, 0, 0, 9, 1, 2, 3]];
    for tail in tails {
        let (result, transcript, channel) = receive_frames(vec![], tail.clone());
        assert!(result.is_err());
        assert!(channel.poisoned());
        assert!(transcript.failed);
        assert_eq!(transcript.raw, tail);
        assert_eq!(transcript.bytes_seen, tail.len() as u64);
        assert_eq!(transcript.frames_seen, 0);
    }
}
#[test]
fn complete_or_partial_trailing_frame_is_retained_and_refused() {
    let request = request();
    for complete in [false, true] {
        let base = frames(&request);
        let prior = base.iter().map(|frame| frame.len() + 4).sum::<usize>();
        let tail = if complete { framed(&[1, 2, 3]) } else { vec![0, 0, 0, 9, 1, 2] };
        let (result, mut transcript, mut channel) = receive_frames(base, tail.clone());
        result.unwrap();
        assert!(require_final_eof(&mut channel, deadline(), &mut transcript).is_err());
        assert!(channel.poisoned());
        assert!(transcript.raw.ends_with(&tail));
        assert_eq!(transcript.bytes_seen, (prior + tail.len()) as u64);
        assert_eq!(transcript.frames_seen, if complete { 7 } else { 6 });
    }
}
#[test]
fn terminal_requires_both_stream_eofs_and_exact_event_order() {
    let request = request();
    for case in 0..5 {
        let mut events = events(&request);
        match case {
            0 => {
                events.remove(0);
            }
            1 => {
                events.swap(0, 1);
            }
            2 => {
                events.pop();
            }
            3 => events.push(events[3].clone()),
            4 => {
                events[2] =
                    wire::Event::Output { stream: wire::Stream::Stdout, offset: 1, bytes: vec![9] }
            }
            _ => unreachable!(),
        }
        let mut frames =
            events.iter().map(|event| event.encode(&request, DIGEST).unwrap()).collect::<Vec<_>>();
        frames.push(terminal(&request).encode(&request, DIGEST).unwrap());
        let (result, transcript, channel) = receive_frames(frames, vec![]);
        assert!(result.is_err(), "case {case}");
        assert!(transcript.failed);
        assert!(channel.poisoned());
        assert_eq!(
            transcript.frames_seen,
            [1, 1, 5, 6, 3][case],
            "the intended invalid frame must actually reach the reader"
        );
    }
}
#[test]
fn failed_terminal_and_absent_accounting_remain_observations_with_query_veto() {
    let request = request();
    for case in 0..3 {
        let mut completion = completion(&request);
        match case {
            0 => completion.inputs_unchanged = Err("original changed".into()),
            1 => {
                completion.finished.status = crate::Status::Finished {
                    outcome: crate::Outcome::Exit(7),
                    cause: crate::CompletionCause::Exited,
                    runtime_ns: 8,
                }
            }
            2 => {
                completion.finished.status = crate::Status::Finished {
                    outcome: crate::Outcome::Signal(9),
                    cause: crate::CompletionCause::Cancelled,
                    runtime_ns: 8,
                }
            }
            _ => unreachable!(),
        }
        assert!(require_success(&completion).is_err());
        let mut state = super::super::QueryState::new();
        state.begin().unwrap();
        assert!(state.check(2).is_err());
    }
    for terminal in [
        wire::Terminal::Refused { reason: "native loan unavailable".into() },
        wire::Terminal::Unknown { reason: "retirement unknown".into() },
    ] {
        let frame = terminal.encode(&request, DIGEST).unwrap();
        let (result, transcript, channel) = receive_frames(vec![frame], vec![]);
        assert!(result.is_err());
        assert!(channel.poisoned());
        assert_eq!(transcript.terminal, Some(terminal));
    }
}
#[test]
fn transcript_is_monotone_after_offset_overflow_or_duplicate_start() {
    let request = request();
    for case in 0..3 {
        let mut transcript = Transcript::new();
        transcript.event(wire::Event::Prepared(Box::new(prepared(&request)))).unwrap();
        transcript
            .event(wire::Event::Started(crate::bootstrap_graph::query::tests::started()))
            .unwrap();
        if case == 0 {
            for offset in (0..wire::MAX_OUTPUT).step_by(wire::MAX_CHUNK) {
                transcript
                    .event(wire::Event::Output {
                        stream: wire::Stream::Stdout,
                        offset,
                        bytes: vec![9; wire::MAX_CHUNK],
                    })
                    .unwrap();
            }
        }
        let event = match case {
            0 => wire::Event::Output {
                stream: wire::Stream::Stdout,
                offset: wire::MAX_OUTPUT,
                bytes: vec![9],
            },
            1 => wire::Event::Output {
                stream: wire::Stream::Stdout,
                offset: u64::MAX,
                bytes: vec![9],
            },
            _ => wire::Event::Started(crate::bootstrap_graph::query::tests::started()),
        };
        assert!(transcript.event(event).is_err());
        assert!(transcript.failed);
        assert!(
            transcript.event(wire::Event::Eof { stream: wire::Stream::Stdout, bytes: 0 }).is_err()
        );
    }
}
#[test]
fn final_expiry_and_unwind_leave_original_query_restriction_active() {
    let mut state = super::super::QueryState::new();
    state.begin().unwrap();
    let (left, _right) = UnixStream::pair().unwrap();
    let mut channel = Channel::new(left).unwrap();
    let mut transcript = Transcript::new();
    assert!(require_final_eof(&mut channel, Instant::now(), &mut transcript).is_err());
    assert!(channel.poisoned());
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        panic!("before opaque completion")
    }));
    assert!(result.is_err());
    assert!(state.enrollment_allowed().is_err());
    assert!(state.check(2).is_err());
}
