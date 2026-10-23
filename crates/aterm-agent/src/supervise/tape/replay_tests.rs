// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for [`ReplayCore`] door dispatch, divergence unwinding, progress bounds, and classes.

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

/// AN ACT'S ROW REPLAYS IN THE BOOT SPELLING ITS TAPE RECORDED: a tape a
/// build before the boot was read as bytes recorded (the kernel's text as it
/// came — Linux's lower-case `boot_id`, macOS's upper-case
/// `kern.bootsessionuuid` — in the count and in the act's ledger row) is fed
/// back through the probe door the loop reads ([`Fx::asleep_for_ledger`])
/// and its validated counted writer
/// (`approvals::Ledger::write_counted_with_boot_text`),
/// and the row it writes is the recorded one, byte for byte: the boot keeps
/// the case it was read in (`policy::sleeps::Boot`), while the count compares
/// by the 16 bytes alone. A count whose boot text names no boot (the all-zero
/// id, a test's old name) replays as no named boot, and its row holds no count.
/// Broader accepted text (mixed case, missing or displaced hyphens, and a
/// NUL-framed UUID) also retains its exact recorded JSON spelling, without
/// changing this host's live boot form. These are admitted-reader controls,
/// not claims about text a normal historical kernel produced.
/// NEGATIVE CONTROL: the Linux tape against a row written upper-case — what a
/// build that spells every boot as macOS does would emit — diverges at the
/// ledger door.
#[test]
fn an_acts_row_replays_in_the_boot_spelling_its_tape_recorded() {
    use crate::supervise::approvals::{Ledger, Outcome, Row};
    use crate::supervise::policy::sleeps::{Boot, SleepStamp};
    const UPPER: &str = "4D5A1B2C-3E4F-5061-7283-94A5B6C7D8E9";
    const LOWER_ROW: &str = r#"{"ts":1790000000000,"sid":"s-1","rule_id":"limit-resume","decision":"typed","cmd_sha256":"e256ee8e7aff6957a781d8328f0f68e26996564c81fa458da59fbca2305138ad","command":"continue","reason":"the limit reset","box_seq":3,"slept_ms":1800003,"boot":"4d5a1b2c-3e4f-5061-7283-94a5b6c7d8e9"}"#;
    const UPPER_ROW: &str = r#"{"ts":1790000000000,"sid":"s-1","rule_id":"limit-resume","decision":"typed","cmd_sha256":"e256ee8e7aff6957a781d8328f0f68e26996564c81fa458da59fbca2305138ad","command":"continue","reason":"the limit reset","box_seq":3,"slept_ms":1800003,"boot":"4D5A1B2C-3E4F-5061-7283-94A5B6C7D8E9"}"#;
    const UNCOUNTED_ROW: &str = r#"{"ts":1790000000000,"sid":"s-1","rule_id":"limit-resume","decision":"typed","cmd_sha256":"e256ee8e7aff6957a781d8328f0f68e26996564c81fa458da59fbca2305138ad","command":"continue","reason":"the limit reset","box_seq":3}"#;
    const LOWER: &[u8] = b"1800003 4d5a1b2c-3e4f-5061-7283-94a5b6c7d8e9";
    let row = Row {
        rule_id: "limit-resume",
        outcome: Outcome::Typed,
        command: "continue",
        reason: "the limit reset",
        box_seq: 3,
    };
    let replay_act = |reply: &[u8], recorded: &str| {
        let core = core_of(vec![
            exchange(doors::PROBE_ASLEEP, b"", reply),
            EffectRecord::Clock(1_790_000_000_000_000_000),
            EffectRecord::Emit {
                door: doors::LEDGER.to_string(),
                bytes: recorded.as_bytes().to_vec(),
            },
        ]);
        let fx = Fx::replay(Arc::clone(&core));
        let (count, spelling) = fx.asleep_for_ledger();
        let count = count.expect("the recorded count");
        let mut ledger = Ledger::named(Some(Path::new("/not-opened/ledger")), Some("@s-1"));
        let line = ledger
            .write_counted_with_boot_text(
                fx.stamp_ms(),
                &row,
                Some(&count),
                spelling.as_deref(),
                &mut io::sink(),
            )
            .expect("a named ledger returns its row without opening it");
        let emitted = run(|| fx.emit(doors::LEDGER, || line.into_bytes()));
        (count, emitted, core)
    };
    for (reply, recorded, named) in [
        (LOWER, LOWER_ROW, true),
        (
            b"1800003 4D5A1B2C-3E4F-5061-7283-94A5B6C7D8E9".as_slice(),
            UPPER_ROW,
            true,
        ),
        (
            b"1800003 00000000-0000-0000-0000-000000000000".as_slice(),
            UNCOUNTED_ROW,
            false,
        ),
        (b"1800003 test-boot".as_slice(), UNCOUNTED_ROW, false),
    ] {
        let (count, emitted, core) = replay_act(reply, recorded);
        assert_eq!(
            count.boot.map(|b| b.id()),
            named.then(|| Boot::of(UPPER).expect("a boot").id())
        );
        assert_eq!(
            count.since(&SleepStamp {
                asleep: Duration::from_millis(1_800_000),
                boot: Boot::of(UPPER),
            }),
            named.then(|| Duration::from_millis(3)),
            "the spelling never decides the comparison"
        );
        assert!(emitted.is_ok(), "{:?}", core.diverged());
        assert_eq!(core.consumed(), (3, 3));
        assert!(core.diverged().is_none());
    }
    for (boot, recorded_boot) in [
        (
            "4D5a1B2C-3E4F-5061-7283-94A5B6C7D8E9",
            "4D5a1B2C-3E4F-5061-7283-94A5B6C7D8E9",
        ),
        (
            "4D5A1B2C3E4F5061728394A5B6C7D8E9",
            "4D5A1B2C3E4F5061728394A5B6C7D8E9",
        ),
        (
            "4D5A1B2C-3E4F-5061-72839-4A5B6C7D8E9-",
            "4D5A1B2C-3E4F-5061-72839-4A5B6C7D8E9-",
        ),
        (
            "4D5A1B2C-3E4F-5061-7283-94A5B6C7D8E9\0a",
            r"4D5A1B2C-3E4F-5061-7283-94A5B6C7D8E9\u0000a",
        ),
    ] {
        let reply = format!("1800003 {boot}");
        let recorded = UPPER_ROW.replace(UPPER, recorded_boot);
        let (count, emitted, core) = replay_act(reply.as_bytes(), &recorded);
        assert_eq!(count.boot, Boot::of(UPPER), "same typed boot: {boot:?}");
        assert!(emitted.is_ok(), "{boot:?}: {:?}", core.diverged());
        assert_eq!(core.consumed(), (3, 3));
        assert!(core.diverged().is_none());
        let mut ledger = Ledger::named(Some(Path::new("/not-opened/ledger")), Some("s-1"));
        for other in [
            "00000000-0000-0000-0000-000000000000",
            "4D5A1B2C-3E4F-5061-7283-94A5B6C7D8EA",
        ] {
            assert_eq!(
                ledger.write_counted_with_boot_text(
                    1_790_000_000_000,
                    &row,
                    Some(&count),
                    Some(other),
                    &mut io::sink(),
                ),
                Some(row.to_json_counted(1_790_000_000_000, Some("s-1"), Some(&count))),
                "CONTROL: another boot cannot be substituted",
            );
        }
    }
    let (_, emitted, core) = replay_act(LOWER, UPPER_ROW);
    assert!(emitted.is_err(), "CONTROL: a respelt boot diverges");
    let d = core.diverged().expect("latched");
    assert_eq!(d.door.as_deref(), Some(doors::LEDGER), "CONTROL: {d:?}");
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
