// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Recorder tests: effect ordering, non-nesting doors, failure isolation,
//! decision screen links, and round-trip codecs.

use std::sync::Arc;
use std::time::{Duration, Instant};

use astream_effects::EffectRecord;

use super::super::codec::{End, Item, Mark, Stamp};
use super::super::doors;
use super::super::frame_tests::Memory;
use super::super::header::Roots;
use super::super::recover::recover;
use super::super::testing::MemoryTape;
use super::*;
use crate::supervise::fx::{ClockSource, Fx, VirtualClock};
use crate::supervise::screen::Screen;

/// The wall clock the virtual clocks stand at: 2026-09-17T15:55:00Z.
const WALL_MS: i64 = 1_789_660_500_000;

/// A door over virtual clocks standing at [`WALL_MS`], and those clocks.
fn virtual_fx() -> (Fx, Arc<VirtualClock>) {
    let clock = Arc::new(VirtualClock::new(Instant::now(), WALL_MS));
    (Fx::over(ClockSource::Virtual(Arc::clone(&clock))), clock)
}

/// Every effect of `items`, in order.
fn effects(items: &[Item]) -> Vec<(EffectRecord, Option<Stamp>)> {
    items
        .iter()
        .filter_map(|i| match i {
            Item::Effect { record, stamp } => Some((record.clone(), *stamp)),
            _ => None,
        })
        .collect()
}

/// Effects are taped in observation order with return-time stamps on round trips, sleeps, and raised flags.
#[test]
fn each_value_is_taped_in_the_order_the_door_saw_it() {
    let (fx, clock) = virtual_fx();
    let dir = crate::supervise::ScratchDir::new("tape-core", "order");
    let file = dir.join("standing.md");
    std::fs::write(&file, b"keep the tests green").expect("write");
    let roots = Roots::of(vec![("rules", dir.to_path_buf())]);
    let tape = MemoryTape::under(&fx, roots);
    let fx = &tape.fx;

    let at = fx.mono();
    assert_eq!(fx.wall_ms(), WALL_MS);
    fx.sleep(Duration::from_millis(1500));
    assert_eq!(fx.since(at), Duration::from_millis(1500), "the live clock");
    let reply: Result<crate::supervise::CtlReply, String> = fx.exchange(
        doors::CTL,
        || codec::ctl_request(&["status"]),
        || {
            Ok(crate::supervise::CtlReply {
                code: 0,
                stdout: "OK busy=0\n".to_string(),
                stderr: String::new(),
            })
        },
    );
    assert_eq!(reply.expect("the live reply").stdout, "OK busy=0\n");
    fx.emit(doors::JOURNAL, || b"{\"line\":\"EVENT idle\"}\n".to_vec());
    let flag = [false, false, true, true];
    let seen: Vec<bool> = flag
        .iter()
        .map(|&up| fx.raised(doors::RAISED_STOP, || up))
        .collect();
    assert_eq!(seen, flag, "the flag as the loop saw it");
    fx.note(NOTE_TURN_END, || "the fold".to_string());
    assert_eq!(
        fx.read_file(&file).expect("the live bytes"),
        b"keep the tests green"
    );
    assert!(fx.read_file(&dir.join("absent")).is_err(), "the live error");
    let read = tape.finish("exit 0");
    assert!(
        tape.said.text().is_empty(),
        "nothing said: {}",
        tape.said.text()
    );

    let got = effects(&read.items);
    let records: Vec<EffectRecord> = got.iter().map(|(r, _)| r.clone()).collect();
    assert_eq!(
        records,
        vec![
            EffectRecord::Mono(0),
            EffectRecord::Clock((WALL_MS * 1_000_000) as u64),
            EffectRecord::Exchange {
                door: doors::SLEEP.to_string(),
                request: b"1500000000".to_vec(),
                reply: Vec::new(),
            },
            EffectRecord::Mono(1_500_000_000),
            EffectRecord::Exchange {
                door: doors::CTL.to_string(),
                request: codec::ctl_request(&["status"]),
                reply: codec::ctl_reply(&Ok(crate::supervise::CtlReply {
                    code: 0,
                    stdout: "OK busy=0\n".to_string(),
                    stderr: String::new(),
                })),
            },
            EffectRecord::Emit {
                door: doors::JOURNAL.to_string(),
                bytes: b"{\"line\":\"EVENT idle\"}\n".to_vec(),
            },
            EffectRecord::Raised {
                door: doors::RAISED_STOP.to_string(),
                false_reads: 2,
            },
            EffectRecord::File {
                path: "@rules/standing.md".to_string(),
                bytes: b"keep the tests green".to_vec(),
            },
            EffectRecord::FileErr {
                path: "@rules/absent".to_string(),
                kind: std::io::ErrorKind::NotFound,
            },
        ]
    );
    let moved = Stamp {
        mono_ns: 1_500_000_000,
        wall_ms: WALL_MS + 1500,
    };
    let stamps: Vec<Option<Stamp>> = got.iter().map(|(_, s)| *s).collect();
    assert_eq!(
        stamps,
        vec![
            None,
            None,
            Some(moved),
            None,
            Some(moved),
            None,
            Some(moved),
            None,
            None
        ]
    );
    assert!(
        read.items
            .iter()
            .any(|i| matches!(i, Item::Note { kind, .. } if kind == NOTE_TURN_END)),
        "the note is on the tape"
    );
    assert_eq!(clock.passed(), Duration::from_millis(1500));
}

/// Nested door calls fail a debug assertion, record a `Mark::Nested`, and leave subsequent calls working.
#[test]
fn doors_do_not_nest() {
    let (fx, _) = virtual_fx();
    let tape = MemoryTape::over(&fx);
    let fx = tape.fx.clone();
    let inner_ran = std::sync::atomic::AtomicBool::new(false);
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        fx.exchange(
            doors::WRITE_BOUND,
            || b"write".to_vec(),
            || {
                fx.exchange(
                    doors::PROBE_ZONE_OFFSET,
                    || b"read".to_vec(),
                    || inner_ran.store(true, std::sync::atomic::Ordering::SeqCst),
                );
            },
        );
    }))
    .expect_err("a nested door fails the debug assertion");
    let message = unwound
        .downcast_ref::<String>()
        .cloned()
        .unwrap_or_default();
    assert!(message.contains("doors do not nest"), "{message}");
    assert!(
        !inner_ran.load(std::sync::atomic::Ordering::SeqCst),
        "the assertion fires before the inner call"
    );
    fx.emit(doors::JOURNAL, || b"after".to_vec());
    let read = tape.finish("exit 0");
    let marks: Vec<&Item> = read
        .items
        .iter()
        .filter(|i| matches!(i, Item::Mark(_)))
        .collect();
    assert_eq!(
        marks,
        vec![&Item::Mark(Mark::Nested {
            inner: doors::PROBE_ZONE_OFFSET.to_string(),
            outer: doors::WRITE_BOUND.to_string(),
        })]
    );
    assert_eq!(
        effects(&read.items)
            .into_iter()
            .map(|(r, _)| r)
            .collect::<Vec<_>>(),
        vec![EffectRecord::Emit {
            door: doors::JOURNAL.to_string(),
            bytes: b"after".to_vec(),
        }],
        "nothing of the nested pair is taped, and the door after it is"
    );
}

/// A tape sink failure logs once and stops recording without disrupting live door calls.
#[test]
fn a_failing_tape_never_reaches_the_loop() {
    let (fx, _) = virtual_fx();
    let sink = Memory::default();
    sink.taken().fail_after = Some(1);
    let tape = MemoryTape::on(&fx, super::super::header::VERBATIM, sink);
    for n in 0..200u64 {
        let r: Result<crate::supervise::CtlReply, String> = tape.fx.exchange(
            doors::CTL,
            || codec::ctl_request(&["await", "seq", "1"]),
            || Err(format!("closed {n}")),
        );
        assert_eq!(r, Err(format!("closed {n}")));
        tape.fx.flush(super::super::frame::Hint::Act);
    }
    tape.fx.finish(End::Suspended);
    let said = tape.said.text();
    assert_eq!(
        said.matches("the loop goes on without it").count(),
        1,
        "{said}"
    );
}

fn screen(seq: u64, generation: Option<&str>) -> Screen {
    Screen {
        seq,
        generation: generation.map(str::to_string),
        ..Screen::default()
    }
}

/// A decision mark links to the effect index of the parsed screen it judged.
#[test]
fn a_decision_names_the_record_of_the_screen_it_judged() {
    let (fx, _) = virtual_fx();
    let tape = MemoryTape::over(&fx);
    let fx = &tape.fx;
    let read = |n: u64| {
        let _: Result<crate::supervise::CtlReply, String> = fx.exchange(
            doors::CTL,
            || codec::ctl_request(&["text", "--json"]),
            || Err(format!("screen {n}")),
        );
    };
    read(0);
    fx.screen_read(&screen(7, Some("1.7")));
    fx.mono();
    read(1);
    fx.screen_read(&screen(8, Some("1.8")));
    fx.decide(
        doors::DECIDE_APPROVAL,
        &screen(8, Some("1.8")),
        Some("safe-read@v1"),
        |_| "Approve".to_string(),
        None,
    );
    fx.decide(
        doors::DECIDE_TURN_END,
        &screen(7, Some("1.7")),
        None,
        |at| {
            format!(
                "WaitUntil {{ until: {} }}",
                at(Instant::now() + Duration::from_secs(3600))
            )
        },
        None,
    );
    fx.decide(
        doors::DECIDE_QUESTION,
        &screen(9, None),
        None,
        |_| "Escalate".to_string(),
        None,
    );
    let read = tape.finish("exit 0");
    let marks: Vec<DecideMark> = effects(&read.items)
        .into_iter()
        .filter_map(|(r, _)| match r {
            EffectRecord::Emit { door, bytes } if door.starts_with("decide.") => {
                DecideMark::decode(std::str::from_utf8(&bytes).ok()?)
            }
            _ => None,
        })
        .collect();
    assert_eq!(marks.len(), 3, "{marks:?}");
    assert_eq!(marks[0].screen, Some(2), "the last read: effect #2");
    assert_eq!(marks[0].rule.as_deref(), Some("safe-read@v1"));
    assert_eq!(marks[1].screen, Some(0), "the carried read: effect #0");
    assert!(
        marks[1].outcome.starts_with("WaitUntil { until: +") && marks[1].outcome.ends_with("ns }"),
        "an instant is an offset from the origin: {}",
        marks[1].outcome
    );
    assert_eq!(marks[2].screen, None, "never parsed");
    assert_eq!(marks[2].generation, None);
}

/// Round-trip values and `DecideMark` payloads encode and decode losslessly.
#[test]
fn every_value_reads_back_from_its_bytes() {
    let back =
        |v: &Result<(), String>| Result::<(), String>::from_tape(&v.to_tape()).expect("reads");
    for v in [
        Ok(()),
        Err(String::new()),
        Err("cannot write: EPIPE".to_string()),
    ] {
        assert_eq!(back(&v), v);
    }
    let io_back = |v: std::io::Result<()>| {
        std::io::Result::<()>::from_tape(&v.to_tape())
            .expect("reads")
            .map_err(|e| e.kind())
    };
    assert_eq!(io_back(Ok(())), Ok(()));
    assert_eq!(
        io_back(Err(std::io::Error::from(std::io::ErrorKind::StorageFull))),
        Err(std::io::ErrorKind::StorageFull)
    );
    assert_eq!(<()>::from_tape(&().to_tape()), Ok(()));
    assert!(<()>::from_tape(b"x").is_err());
    use crate::supervise::policy::sleeps::{Boot, SleepStamp};
    let boot = [
        0x4F, 0x1C, 0x2A, 0x9E, 0x3E, 0x4F, 0x50, 0x61, 0x72, 0x83, 0x94, 0xA5, 0xB6, 0xC7, 0xD8,
        0xE9,
    ];
    let counted = SleepStamp {
        asleep: Duration::from_millis(1_800_003),
        boot: Some(Boot::kernel(boot)),
    };
    for v in [
        None,
        Some(counted.clone()),
        Some(SleepStamp {
            asleep: Duration::ZERO,
            boot: None,
        }),
    ] {
        assert_eq!(Option::<SleepStamp>::from_tape(&v.to_tape()), Ok(v));
    }
    for recorded in [
        &b"1800003 4F1C2A9E-3E4F-5061-7283-94A5B6C7D8E9"[..],
        b"1800003 4f1c2a9e-3e4f-5061-7283-94a5b6c7d8e9",
    ] {
        let read = Option::<SleepStamp>::from_tape(recorded);
        assert_eq!(read, Ok(Some(counted.clone())), "{recorded:?}");
        assert_eq!(read.map(|v| v.to_tape()), Ok(recorded.to_vec()));
    }
    let kernel: &[u8] = if cfg!(any(target_os = "linux", target_os = "android")) {
        b"1800003 4f1c2a9e-3e4f-5061-7283-94a5b6c7d8e9"
    } else {
        b"1800003 4F1C2A9E-3E4F-5061-7283-94A5B6C7D8E9"
    };
    assert_eq!(Some(counted.clone()).to_tape(), kernel.to_vec());
    assert_eq!(
        Option::<SleepStamp>::from_tape(b"1800003 00000000-0000-0000-0000-000000000000"),
        Ok(Some(SleepStamp {
            boot: None,
            ..counted
        })),
        "CONTROL"
    );
    for v in [None, Some((WALL_MS, WALL_MS + 1_800_000))] {
        assert_eq!(Option::<(i64, i64)>::from_tape(&v.to_tape()), Ok(v));
    }
    assert!(Option::<SleepStamp>::from_tape(b"soon").is_err());
    assert!(Option::<(i64, i64)>::from_tape(b"1").is_err());
    for mark in [
        DecideMark {
            screen: Some(12),
            seq: 41,
            generation: Some("3.41".to_string()),
            rule: Some("safe-read@v1".to_string()),
            outcome: "Approve { subject: \"ls -la\\n=-\" }\nsecond line".to_string(),
            facts: Some("{\"cwd\":\"@home/src\",\"box_call\":\"a\\nb=c\"}".to_string()),
        },
        DecideMark {
            screen: None,
            seq: 0,
            generation: None,
            rule: None,
            outcome: "-".to_string(),
            facts: None,
        },
    ] {
        assert_eq!(DecideMark::decode(&mark.encode()), Some(mark.clone()));
        let later = format!("{}later=1\n", mark.encode());
        assert_eq!(DecideMark::decode(&later), Some(mark));
    }
    assert_eq!(
        DecideMark::decode("screen=1\n"),
        None,
        "a mark missing fields"
    );
}

/// Compressed frames decode back to the full recorded item sequence.
#[test]
fn the_recorders_bytes_read_back_whole() {
    let (fx, clock) = virtual_fx();
    let tape = MemoryTape::over(&fx);
    for n in 0..300u64 {
        tape.fx.mono();
        tape.fx.sleep(Duration::from_millis(20));
        let _: Result<crate::supervise::CtlReply, String> = tape.fx.exchange(
            doors::CTL,
            || codec::ctl_request(&["text", "--json", "tail=40"]),
            || {
                Ok(crate::supervise::CtlReply {
                    code: 0,
                    stdout: format!(
                        "{{\"rows\":[\"{}\"],\"seq\":{}}}",
                        "row ".repeat(200),
                        n / 10
                    ),
                    stderr: String::new(),
                })
            },
        );
        if n % 50 == 0 {
            tape.fx.flush(super::super::frame::Hint::BeforeWait);
        }
    }
    let read = tape.finish("exit 0");
    assert_eq!(tape.core().effects(), 900);
    assert_eq!(effects(&read.items).len(), 900);
    assert!(read.frames > 2, "flushed into frames: {}", read.frames);
    assert_eq!(clock.passed(), Duration::from_millis(6000));
    let raw: usize = 300 * 830;
    assert!(
        tape.sink.bytes().len() * 10 < raw,
        "{} bytes for {raw} of replies",
        tape.sink.bytes().len()
    );
    assert_eq!(recover(&tape.sink.bytes()).items, read.items);
}

/// Recorder clock peeks for origin and stamps do not consume a staged `ScriptedClock` jump.
#[test]
fn a_stamp_never_makes_the_read_a_staged_jump_lands_at() {
    use crate::supervise::fx::ScriptedClock;
    let reads = |recorded: bool, consume: bool| {
        let clock = Arc::new(ScriptedClock::default());
        clock.set_at_ms(WALL_MS);
        clock.set_jump(3600);
        let fx = Fx::over(ClockSource::Scripted(Arc::clone(&clock)));
        let tape = MemoryTape::over(&fx);
        let fx = if recorded { tape.fx.clone() } else { fx };
        let _: Result<crate::supervise::CtlReply, String> = fx.exchange(
            doors::CTL,
            || codec::ctl_request(&["status"]),
            || {
                if consume {
                    let _ = fx.wall_ms();
                }
                Err("closed".to_string())
            },
        );
        let got = (fx.wall_ms(), fx.wall_ms());
        drop(tape);
        got
    };
    let shipped = reads(false, false);
    assert_eq!(shipped, (WALL_MS, WALL_MS + 3_600_000));
    assert_eq!(reads(true, false), shipped, "the recorder moved nothing");
    assert_ne!(
        reads(false, true),
        shipped,
        "the negative control: a read in its place lands the jump early"
    );
}

/// `Instant` values formatted into notes are rewritten as signed nanosecond offsets from the tape origin.
#[test]
fn a_notes_instants_are_written_as_offsets_from_the_origin() {
    let (fx, _clock) = virtual_fx();
    let tape = MemoryTape::over(&fx);
    let (origin, _) = fx.peek();
    let raw = format!("{origin:?}");
    assert!(
        InstantForm::learn(origin).is_some(),
        "this build's Instant is written as {raw}, a form the recorder must learn"
    );
    let later = origin + Duration::new(1, 500_000_000);
    let earlier = origin
        .checked_sub(Duration::from_secs(2))
        .expect("the clock runs past two seconds");
    let text = format!("{:?}", (Some(later), "Instant { soon }", [earlier, origin]));
    tape.fx.note("turn_end", || text.clone());
    let read = tape.finish("exit 0");
    let note = read
        .items
        .iter()
        .find_map(|i| match i {
            Item::Note { text, .. } => Some(text.clone()),
            _ => None,
        })
        .expect("the note");
    assert_eq!(
        note, "(Some(+1500000000ns), \"Instant { soon }\", [-2000000000ns, +0ns])",
        "from {text}"
    );
    assert_eq!(instants_as_offsets(&raw, origin), "+0ns");
}

/// Windows' `Instant` is a `Duration` and writes as one (`Instant { t: 12.5s }`):
/// its fraction's trailing zeros trimmed, and no point on a whole second. The
/// form reads every spelling at its nanosecond, on any build's clock.
#[test]
fn a_fraction_form_instant_reads_at_its_nanosecond() {
    let form = InstantForm::shape("Instant { t: 4611707930.767866819s }").expect("two numbers");
    assert_eq!(
        (form.head.as_str(), form.mid.as_str(), form.tail.as_str()),
        ("Instant { t: ", ".", "s }")
    );
    for (text, ns) in [
        ("Instant { t: 12.767866819s }", 12_767_866_819),
        ("Instant { t: 12.76786682s }", 12_767_866_820),
        ("Instant { t: 12.5s }", 12_500_000_000),
        ("Instant { t: 12.000000001s }", 12_000_000_001),
        ("Instant { t: 12s }", 12_000_000_000),
    ] {
        assert_eq!(form.read(text), Some((text.len(), ns)), "{text}");
    }
    for inert in [
        "Instant { t: 12.s }",
        "Instant { t: 12.1234567890s }",
        "Instant { t: 12.5 }",
    ] {
        assert_eq!(form.read(inert), None, "{inert}");
    }
}

/// Malformed or overflowing `Instant`-shaped substrings in notes remain literal text without panicking.
#[test]
fn instant_shaped_note_text_cannot_overflow_the_recorder() {
    let origin = Instant::now();
    let form = InstantForm::learn(origin).expect("this build's supported Instant form");
    let clock = Arc::new(VirtualClock::new(origin, WALL_MS));
    let fx = Fx::over(ClockSource::Virtual(clock));
    let note = |text: &str| {
        let tape = MemoryTape::over(&fx);
        tape.fx.note(NOTE_TURN_END, || text.to_string());
        let read = tape.finish("note");
        read.items
            .into_iter()
            .find_map(|item| match item {
                Item::Note { kind, text } if kind == NOTE_TURN_END => Some(text),
                _ => None,
            })
            .expect("the actual note was recorded")
    };
    let later = origin
        .checked_add(Duration::from_secs(2))
        .expect("later Instant");
    let earlier = origin
        .checked_sub(Duration::from_secs(2))
        .expect("earlier Instant");
    assert_eq!(note(&format!("{origin:?}")), "+0ns");
    assert_eq!(note(&format!("{later:?}")), "+2000000000ns");
    assert_eq!(note(&format!("{earlier:?}")), "-2000000000ns");
    let literal = |secs: &str, nanos: &str| {
        format!("{}{}{}{}{}", form.head, secs, form.mid, nanos, form.tail)
    };
    for inert in [
        literal("not-a-second", "0"),
        literal("1", "1000000000"),
        literal("9999999999999999999999999999999", "0"),
    ] {
        assert_eq!(note(&inert), inert, "malformed text stays text");
    }
    let bad = [
        literal("999999999999999999999999999999", "0"),
        literal("-999999999999999999999999999999", "0"),
        literal("170141183460469231731687303715", "999999999"),
    ];
    let failed: Vec<_> = bad
        .iter()
        .filter_map(|text| {
            let got = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| note(text)));
            match got {
                Ok(got) if got == *text => None,
                Ok(got) => Some(format!("changed {text:?} into {got:?}")),
                Err(_) => Some(format!("panicked on {text:?}")),
            }
        })
        .collect();
    assert!(
        failed.is_empty(),
        "Instant-shaped note text must stay inert: {failed:?}"
    );
}
