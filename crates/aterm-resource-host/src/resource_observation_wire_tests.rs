// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const SESSION: &str = "t000000000a";

fn observed(session: &str) -> Observation {
    Observation {
        host: ProcessKey {
            pid: 10,
            start: 100,
        },
        host_child_cpu_ns: Some(400),
        at_unix_ms: 12_000,
        first_seq: 1,
        next_seq: 35,
        unproven_active: 0,
        lost_origins: 0,
        originals: vec![Original {
            session: session.into(),
            process: ProcessKey {
                pid: 30,
                start: 300,
            },
            cpu_ns: Some(50),
            child_cpu_ns: Some(0),
            reaps_unwaited: Some(false),
            footprint: Some(1024),
            peak_footprint: Some(2048),
        }],
        sessions: vec![SessionUsage {
            session: session.into(),
            reaped_cpu_ns: 400,
            unread: 0,
        }],
        outstanding_sessions: vec![session.into()],
        terminals: (1..=35)
            .map(|seq| Terminal {
                seq,
                session: session.into(),
                process: ProcessKey {
                    pid: seq as u32 + 100,
                    start: seq * 100,
                },
                uid: Some(501),
                coalition: Some(900),
                version: Some(2),
                cpu_ns: Some(seq),
                child_cpu_ns: Some(0),
                wait_cpu_ns: Some(seq),
                reaps_unwaited: Some(false),
                at_unix_ms: 11_000 + seq,
                exit_cut_ns: Some(seq * 100),
            })
            .collect(),
    }
}

fn decode(frames: Vec<aterm_json::Value>) -> std::io::Result<Observation> {
    let mut frames = frames.into_iter();
    Observation::receive(0, || {
        frames
            .next()
            .ok_or_else(|| std::io::ErrorKind::UnexpectedEof.into())
    })
}

#[test]
fn private_observations_preserve_every_generation_across_multiple_frames() {
    for session in [SESSION, "s-0000000000000000000a"] {
        let source = observed(session);
        assert!(source.frames().len() > 3);
        assert_eq!(decode(source.frames()).unwrap(), source);
    }
    for session in [
        "s-a",
        "s-0000000000000000000A",
        "t000000000u",
        "../../session",
    ] {
        assert!(decode(observed(session).frames()).is_err(), "{session}");
    }
}

#[test]
fn an_early_done_or_a_missing_middle_terminal_cannot_advance_the_cursor() {
    let source = observed(SESSION);
    let frames = source.frames();
    assert!(decode(vec![frames[0].clone(), aterm_json::json!({"done":true})]).is_err());
    let mut missing = source;
    missing.terminals.remove(20);
    assert!(
        decode(missing.frames()).is_err(),
        "declared counts alone cannot prove sequence coverage"
    );
}

#[test]
fn duplicate_originals_sessions_and_unknown_counters_do_not_become_ownership() {
    let mut duplicate = observed(SESSION);
    assert!(
        decode(duplicate.frames()).is_ok(),
        "the unmodified observation is valid"
    );
    duplicate.originals.push(duplicate.originals[0].clone());
    assert!(decode(duplicate.frames()).is_err());
    let mut reused = observed(SESSION);
    let mut newer = reused.originals[0].clone();
    newer.process.start += 1;
    reused.originals.push(newer);
    assert!(
        decode(reused.frames()).is_err(),
        "two active generations cannot share one PID"
    );
    let mut duplicate = observed(SESSION);
    duplicate.sessions.push(duplicate.sessions[0].clone());
    assert!(decode(duplicate.frames()).is_err());
    let mut duplicate = observed(SESSION);
    duplicate.outstanding_sessions.push(SESSION.into());
    assert!(decode(duplicate.frames()).is_err());
    let mut unread = observed(SESSION);
    unread.terminals[0].cpu_ns = None;
    unread.terminals[0].child_cpu_ns = None;
    let decoded = decode(unread.frames()).unwrap();
    assert_eq!(decoded.terminals[0].cpu_ns, None);
    assert_eq!(decoded.terminals[0].child_cpu_ns, None);
    assert_eq!(
        decoded.terminals[0].wait_cpu_ns,
        Some(1),
        "aggregate remains an aggregate"
    );
}
