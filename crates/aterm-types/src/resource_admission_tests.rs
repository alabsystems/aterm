// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
fn sample() -> Submission {
    Submission {
        binding: Binding {
            session: "s-a912afd80a5a598f9ace".into(),
            attempt: [2; 16],
        },
        peer_pid: 1,
        peer_birth: 2,
        argv: vec![b"/bin/x".to_vec(), vec![255, 10]],
        cwd: b"/tmp".to_vec(),
        env: vec![(b"PRIVATE".to_vec(), vec![254, 10])],
        cpu_units: 1,
        memory_bytes: 1,
        label: "job".into(),
        timeout_ms: Some(12),
    }
}
#[test]
fn lossless_and_truncated() {
    let s = sample();
    let encoded = s.encode().unwrap();
    assert!(Submission::decode(&encoded).unwrap() == s);
    for n in 0..encoded.len() {
        assert!(Submission::decode(&encoded[..n]).is_err())
    }
    let mut extra = encoded;
    extra.push(0);
    assert!(Submission::decode(&extra).is_err());
}
#[test]
fn cancel_is_bound_to_attempt_and_session() {
    let b = sample().binding;
    let bytes = Cancel { binding: b.clone() }.encode().unwrap();
    assert!(Cancel::decode(&bytes, &b).is_ok());
    for other in [
        Binding {
            session: "s-3".into(),
            ..b.clone()
        },
        Binding {
            attempt: [3; 16],
            ..b.clone()
        },
    ] {
        assert!(Cancel::decode(&bytes, &other).is_err())
    }
    for session in ["s-", "s-G", "s-A", "a-12", "s-12\n"] {
        let malformed = Cancel {
            binding: Binding {
                session: session.into(),
                ..b.clone()
            },
        };
        assert!(malformed.encode().is_err());
    }
    // Exact spelling remains distinct, even when two IDs parse to the
    // same integer. Session identity is never padded or normalized.
    let short = Binding {
        session: "s-1".into(),
        ..b.clone()
    };
    let padded = Binding {
        session: "s-01".into(),
        ..b
    };
    let bytes = Cancel { binding: short }.encode().unwrap();
    assert!(Cancel::decode(&bytes, &padded).is_err());
}
#[test]
fn started_and_finished_are_separate_from_queue() {
    let b = sample().binding;
    for event in [
        Event::Queued,
        Event::Started {
            pid: 7,
            birth: 8,
            monotonic_ns: 99,
            unix_ns: 100,
        },
        Event::Finished {
            outcome: Outcome::Signal(9),
            cause: CompletionCause::Cancelled,
            runtime_ns: 1,
        },
        Event::Finished {
            outcome: Outcome::Signal(9),
            cause: CompletionCause::Timeout,
            runtime_ns: 1,
        },
        Event::Finished {
            outcome: Outcome::Signal(9),
            cause: CompletionCause::Exited,
            runtime_ns: 1,
        },
        Event::Finished {
            outcome: Outcome::Signal(9),
            cause: CompletionCause::ResourceLimit,
            runtime_ns: 1,
        },
        Event::Finished {
            outcome: Outcome::Signal(9),
            cause: CompletionCause::HostFault,
            runtime_ns: 1,
        },
    ] {
        let f = EventFrame {
            binding: b.clone(),
            event,
        };
        assert_eq!(EventFrame::decode(&f.encode().unwrap(), &b).unwrap(), f)
    }
    let finished = EventFrame {
        binding: b.clone(),
        event: Event::Finished {
            outcome: Outcome::Exit(0),
            cause: CompletionCause::Timeout,
            runtime_ns: 4,
        },
    };
    let mut bytes = finished.encode().unwrap();
    for n in 0..bytes.len() {
        assert!(EventFrame::decode(&bytes[..n], &b).is_err());
    }
    let cause_offset = bytes.len() - 16;
    bytes[cause_offset..cause_offset + 8].copy_from_slice(&99u64.to_be_bytes());
    assert!(EventFrame::decode(&bytes, &b).is_err());
}
