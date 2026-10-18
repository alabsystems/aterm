// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// The host's answers and arguments on a tape (`host.rs`): every answer type
// the host's trait returns reads back as written, an `Instant` in it placed
// at the same span from the reading's origin as from the writer's; a byte
// this build did not write is refused, never guessed; and a request names
// each argument, and fails this crate's tests if it holds a raw `Instant`.

use std::time::{Duration, Instant};

use super::*;

/// How far the replay's origin stands from the recording's in these tests:
/// as far as a replay's own does (`replay::ORIGIN_AHEAD`).
const AHEAD: Duration = Duration::from_secs(400 * 24 * 3600);

/// `value` written from one origin and read back onto another.
fn moved<A: HostReply>(value: &A, from: Instant, to: Instant) -> A {
    let bytes = value.put(from);
    A::take(&bytes, to).unwrap_or_else(|e| {
        panic!(
            "{e}: {:?} does not read back",
            String::from_utf8_lossy(&bytes)
        )
    })
}

/// `value` written and read back on one origin, equal to what was written.
fn same<A: HostReply + PartialEq + fmt::Debug>(value: A) {
    let origin = Instant::now();
    assert_eq!(moved(&value, origin, origin), value);
}

/// EVERY ANSWER THE HOST'S TRAIT RETURNS reads back as written: each shape of
/// each type, its words and its nones.
#[test]
fn every_host_answer_reads_back_as_written() {
    same(true);
    same(false);
    for v in [
        None,
        Some(String::new()),
        Some("claude --resume ab \"x\"".to_string()),
    ] {
        same(v);
    }
    for v in [None, Some(true), Some(false)] {
        same(v);
    }
    for v in [None, Some(0u32), Some(u32::MAX)] {
        same(v);
    }
    for v in [None, Some(0i64), Some(-1), Some(i64::MAX)] {
        same(v);
    }
    for v in [None, Some(0u64), Some(u64::MAX)] {
        same(v);
    }
    same(0u64);
    same(u64::MAX);
    same(None::<HostStep>);
    for typed in [
        None,
        Some(HostTurn::Notice),
        Some(HostTurn::CarryOn),
        Some(HostTurn::Check),
    ] {
        same(Some(HostStep {
            line: "HOST seq=102 carry-on step=continued".to_string(),
            moved: true,
            typed,
            repeat: false,
        }));
    }
    same(None::<StandingLimit>);
    same(Some(StandingLimit {
        text: "You've hit your weekly limit · resets 6pm".to_string(),
        until: Some(1_789_840_800),
        window: Some("seven_day".to_string()),
        at: Some(1_789_660_500),
        row: Some("6f1c".to_string()),
    }));
    same(Some(StandingLimit {
        text: String::new(),
        until: None,
        window: None,
        at: None,
        row: None,
    }));
    same(None::<AgentMemory>);
    same(Some(AgentMemory::default()));
    for pressure in [Pressure::Normal, Pressure::Warn, Pressure::Critical] {
        same(Some(AgentMemory {
            footprint_mib: 4096,
            physical_mib: Some(131_072),
            available_mib: Some(2048),
            used_mib: Some(120_000),
            pressure: Some(pressure),
            swap_kib_s: Some(9000),
            largest_other_mib: Some(1024),
        }));
    }
    same(Ok::<(), ProbeNo>(()));
    for no in ProbeNo::ALL {
        same(Err::<(), ProbeNo>(no));
    }
    same(ResetWord::Again);
    for no in ResetNo::ALL {
        same(ResetWord::No(no));
    }
    same(ResetWord::Take(ResetGo {
        until: 1_789_840_800,
        long: true,
        limits: vec!["weekly".to_string(), "Opus".to_string()],
        left: 2,
        day: "Oct 5".to_string(),
    }));
    for outcome in [
        ResetOutcome::Pending,
        ResetOutcome::Spent {
            asks: true,
            left: Some(1),
            cleared_at: 1_789_660_500,
        },
        ResetOutcome::Spent {
            asks: false,
            left: None,
            cleared_at: 0,
        },
        ResetOutcome::Lifted {
            asks: true,
            cleared_at: 1_789_660_500,
        },
        ResetOutcome::Kept,
        ResetOutcome::Failed { login: true },
        ResetOutcome::Failed { login: false },
        ResetOutcome::Unread,
    ] {
        same(outcome);
    }
    same(Reach::Unknown);
    same(None::<AccountAdvice>);
    same(Some(AccountAdvice::Move {
        to: "work".to_string(),
    }));
    same(Some(AccountAdvice::Resume {
        cleared_at: 1_789_660_500,
    }));
    same(Some(AccountAdvice::Probe { otherwise: None }));
    same(Some(AccountAdvice::Probe {
        otherwise: Some(Box::new(AccountAdvice::Move {
            to: "home".to_string(),
        })),
    }));
    same(None::<Instant>);
}

/// AN ANSWER THAT HOLDS AN INSTANT is written from the recording's origin and
/// read back onto the replay's, at the same span from it — before the origin
/// as well as after — so the replayed loop measures the host's instant
/// against its own clock reads exactly as the recorded loop did.
#[test]
fn an_answers_instant_keeps_its_span_from_the_origin() {
    let from = Instant::now();
    let to = from + AHEAD;
    for span in [
        Duration::ZERO,
        Duration::from_nanos(1),
        Duration::from_secs(5),
        Duration::new(3600, 7),
    ] {
        let later = from + span;
        assert_eq!(moved(&Some(later), from, to), Some(to + span));
        assert_eq!(
            moved(&Reach::Down { since: later }, from, to),
            Reach::Down { since: to + span }
        );
        assert_eq!(
            moved(&Reach::Up { since: later }, from, to),
            Reach::Up { since: to + span }
        );
        let wait = Some(AccountAdvice::Wait { until: later });
        assert_eq!(
            moved(&wait, from, to),
            Some(AccountAdvice::Wait { until: to + span })
        );
        let probed = Some(AccountAdvice::Probe {
            otherwise: Some(Box::new(AccountAdvice::Wait { until: later })),
        });
        assert_eq!(
            moved(&probed, from, to),
            Some(AccountAdvice::Probe {
                otherwise: Some(Box::new(AccountAdvice::Wait { until: to + span })),
            })
        );
        // An instant the host placed BEFORE the tape's origin (a measure's
        // run begun before the loop) lands as far before the replay's.
        if let Some(earlier) = from.checked_sub(span) {
            assert_eq!(
                moved(&Reach::Down { since: earlier }, from, to),
                Reach::Down { since: to - span }
            );
        }
    }
    assert_eq!(
        offset_ns(from, from + Duration::from_secs(1)),
        1_000_000_000
    );
    if let Some(before) = from.checked_sub(Duration::from_secs(1)) {
        assert_eq!(offset_ns(from, before), -1_000_000_000);
    }
}

/// A BYTE THIS BUILD DID NOT WRITE is refused by every reader, never guessed:
/// a word it never gives, a shape it does not know, bytes that are no JSON.
#[test]
fn an_answer_this_build_did_not_write_is_refused() {
    fn refused<A: HostReply>(bytes: &[u8]) -> bool {
        A::take(bytes, Instant::now()).is_err()
    }
    assert!(refused::<Result<(), ProbeNo>>(b"\"not-a-word\""));
    assert!(refused::<ResetWord>(b"{\"no\":\"never\"}"));
    assert!(refused::<ResetWord>(b"\"maybe\""));
    assert!(refused::<ResetOutcome>(b"\"spent\""));
    assert!(refused::<Reach>(b"\"sideways\""));
    assert!(refused::<Option<HostStep>>(
        b"{\"line\":\"x\",\"moved\":true,\"typed\":\"song\",\"repeat\":false}"
    ));
    assert!(refused::<Option<AgentMemory>>(
        b"{\"footprint_mib\":1,\"physical_mib\":null,\"available_mib\":null,\"used_mib\":null,\
          \"pressure\":\"panic\",\"swap_kib_s\":null,\"largest_other_mib\":null}"
    ));
    assert!(refused::<Option<String>>(b"not json"));
    assert!(refused::<Option<AccountAdvice>>(b"{\"wait\":{}}"));
}

/// A REQUEST names each argument and its `Debug` text, a line each, in the
/// signature's order, and nothing for a call with none.
#[test]
fn a_request_names_each_argument() {
    assert!(request(&[]).is_empty());
    let point = IdlePoint {
        done_under_land: true,
    };
    let text = "a \"quoted\"\nline";
    assert_eq!(
        String::from_utf8(request(&[("point", &point), ("text", &text)])).expect("utf-8"),
        "point=IdlePoint { done_under_land: true }\ntext=\"a \\\"quoted\\\"\\nline\""
    );
}

/// An argument whose `Debug` holds a RAW `Instant` fails this crate's tests
/// where the request is built: such a request would differ on every replay.
#[test]
fn an_argument_that_holds_an_instant_is_refused_in_tests() {
    #[derive(Debug)]
    struct Leaky {
        #[expect(dead_code, reason = "read through Debug alone")]
        at: Instant,
    }
    impl TapeArg for Leaky {}
    let leaky = Leaky { at: Instant::now() };
    let built = std::panic::catch_unwind(|| request(&[("leaky", &leaky)]));
    assert!(
        built.is_err(),
        "a raw Instant in a request is a failed test"
    );
    // The same argument with its instant as an offset is fine.
    let fine = std::panic::catch_unwind(|| {
        request(&[(
            "wait",
            &WaitPlan {
                at_unix: 1,
                why: "x".to_string(),
            },
        )])
    });
    assert!(fine.is_ok());
}
