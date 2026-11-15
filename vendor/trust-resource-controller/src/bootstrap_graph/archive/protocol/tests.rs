//! Codec controls exercise DATA only, with no native execution/closure claim.
use super::*;
use crate::{CompletionCause, Outcome};

const DIGEST: [u8; 32] = [7; 32];
fn request() -> Request {
    super::super::tests::request()
}
fn prepared(request: &Request) -> MetadataPrepared {
    MetadataPrepared {
        root_attempt: request.key.graph.query.binding.root_attempt,
        member_attempt: request.key.operation,
        root: request.sdk_root.clone(),
        policy: super::super::PREPARED_POLICY.into(),
        scope: super::super::PREPARED_SCOPE.into(),
        executable: request.driver.clone(),
        runtime: vec![],
        loader_paths: vec![],
        system_dependencies: vec![],
        host: request.host.clone(),
        boot: request.boot.clone(),
    }
}
fn terminal(request: &Request) -> Terminal {
    Terminal::Completed {
        completion: Box::new(MetadataCompletion {
            finished: StatusFrame {
                root_attempt: request.key.graph.query.binding.root_attempt,
                member_attempt: request.key.operation,
                status: Status::Finished {
                    outcome: Outcome::Exit(7),
                    cause: CompletionCause::Exited,
                    runtime_ns: 8,
                },
            },
            inputs_unchanged: Err("actual input changed".into()),
        }),
        observation: crate::accounting::Observation::uncollected(),
    }
}
fn events(request: &Request) -> Vec<Event> {
    vec![
        Event::Prepared(Box::new(prepared(request))),
        Event::Started(crate::bootstrap_graph::query::tests::started()),
        Event::Output {
            stream: Stream::Stdout,
            offset: 0,
            bytes: b"copy report".to_vec(),
        },
        Event::Output {
            stream: Stream::Stderr,
            offset: 3,
            bytes: b"diagnostic".to_vec(),
        },
        Event::Eof {
            stream: Stream::Stdout,
            bytes: 11,
        },
        Event::Eof {
            stream: Stream::Stderr,
            bytes: 13,
        },
    ]
}

#[test]
fn all_events_and_failure_diagnostics_roundtrip_without_becoming_success() {
    let request = request();
    for event in events(&request) {
        assert_eq!(
            Event::decode(&event.encode(&request, DIGEST).unwrap(), &request, DIGEST).unwrap(),
            event
        );
    }
    for terminal in [
        terminal(&request),
        Terminal::Refused {
            reason: "no actual supplementary Root credit".into(),
        },
        Terminal::Unknown {
            reason: "native closure unresolved".into(),
        },
    ] {
        let bytes = terminal.encode(&request, DIGEST).unwrap();
        assert_eq!(
            Terminal::decode(&bytes, &request, DIGEST).unwrap(),
            terminal
        );
        let ack = Ack::encode(&request, DIGEST, &terminal).unwrap();
        assert_eq!(Ack::decode(&ack, &request, DIGEST, &terminal).unwrap(), Ack);
    }
}

#[test]
fn complete_request_binding_refuses_replay_even_when_the_expected_digest_is_reused() {
    let original = request();
    let event = Event::Eof {
        stream: Stream::Stdout,
        bytes: 0,
    };
    let event_bytes = event.encode(&original, DIGEST).unwrap();
    let terminal = terminal(&original);
    let terminal_bytes = terminal.encode(&original, DIGEST).unwrap();
    let ack = Ack::encode(&original, DIGEST, &terminal).unwrap();
    for case in 0..9 {
        let mut changed = original.clone();
        match case {
            0 => changed.host.push('x'),
            1 => changed.boot.push('x'),
            2 => changed.archives[0].sha256[0] ^= 1,
            3 => changed.declarations[0].sha256[0] ^= 1,
            4 => changed.driver.sha256[0] ^= 1,
            5 => changed.contents_directory.inode += 1,
            6 => changed.timeout_ns -= 1,
            7 => changed.key.sequence += 1,
            8 => {
                changed.gnu = crate::bootstrap_graph::GnuIdentity::NamedFifo {
                    path: b"/other/credits".to_vec(),
                    backing: crate::FileIdentity {
                        device: 3,
                        inode: 4,
                    },
                }
            }
            _ => unreachable!(),
        }
        changed.validate().unwrap();
        assert!(
            Event::decode(&event_bytes, &changed, DIGEST).is_err(),
            "event case {case}"
        );
        assert!(
            Terminal::decode(&terminal_bytes, &changed, DIGEST).is_err(),
            "terminal case {case}"
        );
        assert!(
            Ack::decode(&ack, &changed, DIGEST, &terminal).is_err(),
            "ack case {case}"
        );
    }
    assert!(Event::decode(&event_bytes, &original, [8; 32]).is_err());
    assert!(Terminal::decode(&terminal_bytes, &original, [8; 32]).is_err());
    assert!(Ack::decode(&ack, &original, [8; 32], &terminal).is_err());
}

#[test]
fn prepared_data_must_match_original_host_boot_and_distinct_profile() {
    let request = request();
    for case in 0..5 {
        let mut prepared = prepared(&request);
        match case {
            0 => prepared.host.push('x'),
            1 => prepared.boot.push('x'),
            2 => prepared.policy = crate::bootstrap_graph::query::PREPARED_POLICY.into(),
            3 => prepared.scope = "arbitrary-process".into(),
            4 => prepared.executable.sha256[0] ^= 1,
            _ => unreachable!(),
        }
        assert!(
            Event::Prepared(Box::new(prepared))
                .encode(&request, DIGEST)
                .is_err()
        );
    }
}

#[test]
fn output_bounds_refuse_empty_chunks_overflow_and_oversized_transcripts() {
    let request = request();
    for (offset, bytes) in [
        (0, vec![]),
        (0, vec![0; MAX_CHUNK + 1]),
        (u64::MAX, vec![0]),
        (MAX_OUTPUT, vec![0]),
    ] {
        assert!(
            Event::Output {
                stream: Stream::Stdout,
                offset,
                bytes
            }
            .encode(&request, DIGEST)
            .is_err()
        );
    }
    Event::Output {
        stream: Stream::Stdout,
        offset: MAX_OUTPUT - MAX_CHUNK as u64,
        bytes: vec![0; MAX_CHUNK],
    }
    .encode(&request, DIGEST)
    .unwrap();
    for stream in [Stream::Stdout, Stream::Stderr] {
        assert!(
            Event::Eof {
                stream,
                bytes: MAX_OUTPUT + 1
            }
            .encode(&request, DIGEST)
            .is_err()
        );
        Event::Eof {
            stream,
            bytes: MAX_OUTPUT,
        }
        .encode(&request, DIGEST)
        .unwrap();
    }
}

#[test]
fn acknowledgement_is_bound_to_the_exact_completed_terminal() {
    let request = request();
    let first = terminal(&request);
    let ack = Ack::encode(&request, DIGEST, &first).unwrap();
    let mut changed = first.clone();
    let Terminal::Completed { completion, .. } = &mut changed else {
        unreachable!()
    };
    completion.inputs_unchanged = Ok(());
    assert!(Ack::decode(&ack, &request, DIGEST, &changed).is_err());
    let changed = Terminal::Unknown {
        reason: "different original delivery".into(),
    };
    assert!(Ack::decode(&ack, &request, DIGEST, &changed).is_err());
}

#[test]
fn incomplete_trailing_and_cross_protocol_frames_refuse() {
    let request = request();
    let event = Event::Eof {
        stream: Stream::Stdout,
        bytes: 0,
    };
    let terminal = terminal(&request);
    let frames = [
        event.encode(&request, DIGEST).unwrap(),
        terminal.encode(&request, DIGEST).unwrap(),
        Ack::encode(&request, DIGEST, &terminal).unwrap(),
    ];
    for (kind, frame) in frames.iter().enumerate() {
        let accepts = |bytes: &[u8]| match kind {
            0 => Event::decode(bytes, &request, DIGEST).is_ok(),
            1 => Terminal::decode(bytes, &request, DIGEST).is_ok(),
            _ => Ack::decode(bytes, &request, DIGEST, &terminal).is_ok(),
        };
        for cut in 0..frame.len() {
            assert!(!accepts(&frame[..cut]), "kind {kind} cut {cut}");
        }
        let mut trailing = frame.clone();
        trailing.push(0);
        assert!(!accepts(&trailing));
        for (other, frame) in frames.iter().enumerate() {
            if other != kind {
                assert!(!accepts(frame));
            }
        }
        assert!(!accepts(&request.encode().unwrap()));
    }
}

#[test]
fn malformed_diagnostics_and_foreign_completion_attempts_refuse() {
    let request = request();
    for reason in [
        String::new(),
        "control\ntext".into(),
        "x".repeat(MAX_REASON + 1),
    ] {
        assert!(
            Terminal::Refused {
                reason: reason.clone()
            }
            .encode(&request, DIGEST)
            .is_err()
        );
        assert!(
            Terminal::Unknown { reason }
                .encode(&request, DIGEST)
                .is_err()
        );
    }
    for root in [true, false] {
        let mut terminal = terminal(&request);
        let Terminal::Completed { completion, .. } = &mut terminal else {
            unreachable!()
        };
        if root {
            completion.finished.root_attempt = [9; 16];
        } else {
            completion.finished.member_attempt = [9; 16];
        }
        assert!(terminal.encode(&request, DIGEST).is_err());
    }
}
