// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The replay's core on tapes made by hand: what each door serves, that a
//! divergence unwinds from the door it is found at and nothing is served after
//! it, the progress guard, and the classes a divergence is sorted into.

use super::*;
use crate::supervise::tape::header::VERBATIM;

/// A core over `records`, its roots empty.
fn core_of(records: Vec<EffectRecord>) -> Arc<ReplayCore> {
    ReplayCore::start(records, VERBATIM, VERBATIM, "test", None)
}

/// `f` run against the core, the halt it unwinds with caught: `Err` when the
/// replay halted.
fn run<T>(f: impl FnOnce() -> T) -> Result<T, ()> {
    std::panic::catch_unwind(AssertUnwindSafe(f)).map_err(|payload| {
        assert!(
            payload.is::<Halted>(),
            "a replay halts with its own payload"
        );
    })
}

fn exchange(door: &str, request: &[u8], reply: &[u8]) -> EffectRecord {
    EffectRecord::Exchange {
        door: door.to_string(),
        request: request.to_vec(),
        reply: reply.to_vec(),
    }
}

/// Each door serves its record: a monotonic read as an instant that far past
/// the replay's origin, a wall read as its nanoseconds, a round trip as the
/// recorded reply to the recorded request, a file as its bytes, an output
/// compared; the control lane's requests and the printed lines are kept for
/// the report.
#[test]
fn each_door_serves_its_record() {
    let core = core_of(vec![
        EffectRecord::Mono(1_500),
        EffectRecord::Clock(1_789_660_500_000_000_000),
        exchange(
            doors::CTL,
            &codec::ctl_request(&["status"]),
            b"\x00\x00\x00OK",
        ),
        EffectRecord::File {
            path: "/l/s-1.jsonl".to_string(),
            bytes: b"{}\n".to_vec(),
        },
        exchange(doors::OUT, b"APPROVED seq=1 ls", b""),
        EffectRecord::Emit {
            door: doors::JOURNAL.to_string(),
            bytes: b"{\"t\":1}\n".to_vec(),
        },
    ]);
    let at = core.mono();
    assert_eq!(core.offset_text(at), "+1500ns");
    assert_eq!(core.clock(), 1_789_660_500_000_000_000);
    assert_eq!(
        core.exchange(doors::CTL, &codec::ctl_request(&["status"])),
        b"\x00\x00\x00OK"
    );
    assert_eq!(
        core.file(Path::new("/l/s-1.jsonl")).expect("the bytes"),
        b"{}\n"
    );
    assert!(core.exchange(doors::OUT, b"APPROVED seq=1 ls").is_empty());
    core.emit(doors::JOURNAL, b"{\"t\":1}\n");
    assert_eq!(core.consumed(), (6, 6));
    let st = core.lock();
    assert_eq!(st.requests, [vec!["status".to_string()]]);
    assert_eq!(st.lines, ["APPROVED seq=1 ls"]);
}

/// A divergence unwinds from the door it is found at, latched as the first:
/// its place, its door and its class; and nothing is served after it — a
/// door asked again (as a loop that caught the unwind would) unwinds again,
/// the first divergence kept.
#[test]
fn a_divergence_unwinds_and_nothing_is_served_after_it() {
    let core = core_of(vec![
        EffectRecord::Mono(1),
        exchange(
            doors::CTL,
            &codec::ctl_request(&["key", "1"]),
            b"\x00\x00\x00OK",
        ),
        EffectRecord::Mono(2),
    ]);
    run(|| core.mono()).expect("served");
    assert!(run(|| core.exchange(doors::CTL, &codec::ctl_request(&["key", "2"]))).is_err());
    assert!(run(|| core.mono()).is_err(), "nothing is served after it");
    let st = core.lock();
    let d = st.diverged.clone().expect("latched");
    assert_eq!((d.at, d.door.as_deref()), (1, Some(doors::CTL)));
    assert_eq!(d.class, Class::Decision, "an act differs");
    assert!(matches!(
        d.cause,
        Cause::Refused(ReplayDivergence::RequestDiverged { .. })
    ));
}

/// A flag read finds its record only where the recording's first true read
/// was — after as many false reads as it counted — and is true from then on;
/// flag reads that consume nothing past the guard's bound are no progress.
#[test]
fn a_flag_is_raised_where_it_was_and_the_guard_bounds_idle_reads() {
    let core = core_of(vec![
        EffectRecord::Raised {
            door: doors::RAISED_STOP.to_string(),
            false_reads: 2,
        },
        EffectRecord::Mono(5),
    ]);
    assert!(!core.raised(doors::RAISED_STOP));
    assert!(!core.raised(doors::RAISED_STOP));
    assert!(
        core.raised(doors::RAISED_STOP),
        "the recorded first true read"
    );
    assert!(core.raised(doors::RAISED_STOP), "latched");
    let _ = core.mono();
    let stuck = core_of(vec![EffectRecord::Mono(1)]);
    let halted = run(|| {
        for _ in 0..=PROGRESS_READS {
            let _ = stuck.raised(doors::RAISED_HANDOVER);
        }
    });
    assert!(halted.is_err(), "no progress");
    let d = stuck.lock().diverged.clone().expect("latched");
    assert!(
        matches!(d.cause, Cause::NoProgress { reads } if reads == PROGRESS_READS + 1),
        "{d:?}"
    );
}

/// The classes: an act on either side is a decision (the host's acts among
/// them), a decision's mark is a decision, an output, a write or a host's
/// notification at the recorded door is an output, anything else drifts.
#[test]
fn a_divergence_is_sorted_by_what_differs() {
    let key = codec::ctl_request(&["key", "1"]);
    let status = codec::ctl_request(&["status"]);
    let found_key = exchange(doors::CTL, &key, b"");
    let found_status = exchange(doors::CTL, &status, b"");
    let asked = |door, request| Asked::Exchange { door, request };
    assert_eq!(
        class_of(
            &asked(doors::CTL, &status),
            Some(&found_key),
            Some(doors::CTL)
        ),
        Class::Decision,
        "the tape holds an act where the loop asks a read"
    );
    assert_eq!(
        class_of(
            &asked(doors::CTL, &key),
            Some(&found_status),
            Some(doors::CTL)
        ),
        Class::Decision,
        "the loop acts where the tape holds a read"
    );
    assert_eq!(
        class_of(
            &asked(doors::CTL, &status),
            Some(&found_status),
            Some(doors::CTL)
        ),
        Class::Drift
    );
    let mark = EffectRecord::Emit {
        door: doors::DECIDE_APPROVAL.to_string(),
        bytes: Vec::new(),
    };
    assert_eq!(
        class_of(&Asked::Emit, Some(&mark), Some(doors::DECIDE_APPROVAL)),
        Class::Decision
    );
    // The host's acts are decisions, its other answers drift, and a
    // notification told otherwise is an output.
    let step = exchange(
        "host.at_idle_on",
        b"point=IdlePoint { done_under_land: false }",
        b"null",
    );
    assert_eq!(
        class_of(
            &asked("host.at_idle_on", b"x"),
            Some(&step),
            Some("host.at_idle_on")
        ),
        Class::Decision
    );
    assert_eq!(
        class_of(&asked(doors::CTL, &status), Some(&step), Some(doors::CTL)),
        Class::Decision,
        "a read where the tape holds the host's step misses a decision"
    );
    let wants = exchange("host.wants", b"", b"1");
    assert_eq!(
        class_of(&asked("host.wants", b"x"), Some(&wants), Some("host.wants")),
        Class::Drift
    );
    let told = EffectRecord::Emit {
        door: "host.turn_ran".to_string(),
        bytes: Vec::new(),
    };
    assert_eq!(
        class_of(&Asked::Emit, Some(&told), Some("host.turn_ran")),
        Class::Output
    );
    let line = exchange(doors::OUT, b"a", b"");
    assert_eq!(
        class_of(&asked(doors::OUT, b"b"), Some(&line), Some(doors::OUT)),
        Class::Output
    );
    assert_eq!(
        class_of(
            &asked(doors::OUT, b"b"),
            Some(&found_status),
            Some(doors::OUT)
        ),
        Class::Drift,
        "an output where the tape holds a read is no output divergence"
    );
    assert_eq!(
        class_of(&Asked::Read, Some(&EffectRecord::Mono(1)), None),
        Class::Drift
    );
}
