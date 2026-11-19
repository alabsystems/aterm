// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
fn sample() -> Submission {
    Submission {
        binding: Binding {
            session: "tqn5r6e0148".into(),
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
            session: "t0123456789".into(),
            ..b.clone()
        },
        Binding {
            session: "s-a912afd80a5a598f9ace".into(),
            ..b.clone()
        },
        Binding {
            session: "s-a912afd80a5a598f9acf".into(),
            ..b.clone()
        },
        Binding {
            attempt: [3; 16],
            ..b.clone()
        },
    ] {
        assert!(
            Cancel {
                binding: other.clone()
            }
            .encode()
            .is_ok()
        );
        assert!(Cancel::decode(&bytes, &other).is_err())
    }
    for session in [
        "s-",
        "s-G",
        "s-A",
        "a-12",
        "s-12\n",
        "s-a912afd80a5a598f9ace\n",
    ] {
        let malformed = Cancel {
            binding: Binding {
                session: session.into(),
                ..b.clone()
            },
        };
        assert!(malformed.encode().is_err());
    }
    // Distinct valid current IDs do not match, even with the same attempt.
    let minted = Binding {
        session: "t0000000001".into(),
        ..b.clone()
    };
    let bytes = Cancel { binding: minted }.encode().unwrap();
    assert!(Cancel::decode(&bytes, &b).is_err());
}
/// A binding's session is a local ID in either form `Sid::parse` reads, exactly what the host's
/// journal can name its file by, so nothing the wire admits is refused later in the pipeline.
/// NEGATIVE CONTROL: a malformed ID of each form is refused on encode and on decode, including
/// an `s-` ID of other than 20 hex digits, which the earlier wire rule (`s-` and 1 to 62 hex)
/// admitted and the journal then refused.
#[test]
fn a_binding_session_is_an_exact_local_session_id() {
    for session in ["tqn5r6e0148", "t0123456789", "s-0123456789abcdef0123"] {
        let mut s = sample();
        s.binding.session = session.into();
        let encoded = s.encode().unwrap();
        assert!(Submission::decode(&encoded).unwrap() == s, "{session}");
        let bytes = Cancel {
            binding: s.binding.clone(),
        }
        .encode()
        .unwrap();
        assert!(Cancel::decode(&bytes, &s.binding).is_ok(), "{session}");
        assert!(
            crate::control_request::Sid::parse(session).is_some(),
            "the journal names {session}"
        );
    }
    let long = format!("s-{}", "a".repeat(62));
    for session in [
        "s-acde",
        "s-0123456789abcdef012",
        "s-0123456789abcdef01234",
        long.as_str(),
        "S-0123456789ABCDEF0123",
        "tqn5r6e014",
        "tqn5r6e01488",
        "tqn5r6e014u",
        "TQN5R6E0148",
    ] {
        let mut s = sample();
        s.binding.session = session.into();
        assert!(s.encode().is_err(), "CONTROL encode: {session}");
    }
    // Decode, too: patch a well-formed `t` binding to an `s-` session of the same length that
    // the earlier rule admitted.
    let mut s = sample();
    s.binding.session = "tqn5r6e0148".into();
    let encoded = s.encode().unwrap();
    let at = encoded
        .windows(11)
        .position(|w| w == b"tqn5r6e0148")
        .unwrap();
    let mut patched = encoded.clone();
    patched[at..at + 11].copy_from_slice(b"s-acdeacde0");
    assert!(Submission::decode(&patched).is_err(), "CONTROL decode");
    // An exact legacy local binding also reads back only with its original lower-case text.
    s.binding.session = "s-0123456789abcdef0123".into();
    let mut patched = s.encode().unwrap();
    let at = patched
        .windows(22)
        .position(|w| w == b"s-0123456789abcdef0123")
        .unwrap();
    patched[at + 12] = b'A';
    assert!(
        Submission::decode(&patched).is_err(),
        "CONTROL legacy decode"
    );
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
