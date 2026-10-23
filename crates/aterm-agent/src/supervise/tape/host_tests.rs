// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
        Some(HostTurn::Fact),
    ] {
        same(Some(HostStep {
            line: "HOST seq=102 carry-on step=continued".to_string(),
            moved: true,
            typed,
            repeat: false,
        }));
    }
    same(None::<(String, HostTurn)>);
    for turn in [
        HostTurn::Notice,
        HostTurn::CarryOn,
        HostTurn::Check,
        HostTurn::Fact,
    ] {
        same(Some(("released:give-up".to_string(), turn)));
    }
    same(None::<LastTurn>);
    same(Some(LastTurn::default()));
    same(Some(LastTurn {
        last: Some("answer:a-2".to_string()),
        opened: Some("prompt:u-2".to_string()),
        opened_said: Some(u64::MAX),
        ended: Some(Duration::new(41, 7)),
        opened_ms: Some(1_789_660_500_000),
        asked: vec!["prompt:c-1".to_string(), "prompt:c-2".to_string()],
    }));
    same(None::<crate::harness::repo_work::RepoWork>);
    same(Some(crate::harness::repo_work::RepoWork {
        repo: "/Users//a/Developer/r".to_string(),
        changed: 2,
        untracked: 1,
        stashes: 1,
        unpushed: 3,
        unlanded: 4,
        main_ref: "origin/main".to_string(),
        paths: vec!["src/a.rs".to_string(), "notes/".to_string()],
        state: Some(u64::MAX),
    }));
    let old =
        br#"{"repo":"/r","changed":1,"untracked":0,"stashes":0,"unpushed":2,"paths":["a.rs"]}"#;
    assert_eq!(
        <Option<crate::harness::repo_work::RepoWork> as TapeValue>::from_tape(old)
            .expect("an older tape's finding"),
        Some(crate::harness::repo_work::RepoWork {
            repo: "/r".to_string(),
            changed: 1,
            unpushed: 2,
            paths: vec!["a.rs".to_string()],
            ..Default::default()
        })
    );
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
    same(None::<TapWindow>);
    for figure in [
        None,
        Some(crate::harness::tap::Window {
            pct: 100,
            resets_at: 1_789_660_500,
            pct_x10: Some(996),
        }),
        Some(crate::harness::tap::Window {
            pct: 42,
            resets_at: 1_789_660_500,
            pct_x10: None,
        }),
    ] {
        same(Some(TapWindow {
            figure,
            read_at: 1_789_650_000,
        }));
    }
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
    same(BackgroundEvidence::Unknown);
    for (task, wait) in [(None, None), (Some("bplle5dxj"), Some("mutex"))] {
        same(BackgroundEvidence::NoProgress {
            worst: BackgroundCall {
                pid: 27098,
                pgid: u32::MAX,
                exe: "aterm_gui-e0964a".to_string(),
                task: task.map(str::to_string),
                quiet_ms: u64::MAX,
                wait: wait.map(str::to_string),
            },
        });
    }
    same(Ok::<(), ProbeNo>(()));
    for no in ProbeNo::ALL {
        same(Err::<(), ProbeNo>(no));
    }
    same(ResetWord::Again);
    for no in ResetNo::ALL {
        same(ResetWord::No(no));
    }
    for kind in aterm_phase::ResetKind::ALL {
        same(ResetWord::Take(ResetGo {
            until: 1_789_840_800,
            long: true,
            limits: vec!["weekly".to_string(), "Opus".to_string()],
            kind,
            left: 2,
            day: "Oct 5".to_string(),
        }));
    }
    for outcome in [
        ResetOutcome::Pending,
        ResetOutcome::Spent {
            kind: aterm_phase::ResetKind::Weekly,
            asks: true,
            left: Some(1),
            cleared_at: 1_789_660_500,
        },
        ResetOutcome::Spent {
            kind: aterm_phase::ResetKind::Full,
            asks: false,
            left: None,
            cleared_at: 0,
        },
        ResetOutcome::Walled {
            kind: aterm_phase::ResetKind::Fable,
            left: 0,
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
    let next = |label: &str, renew| {
        Some(crate::supervise::policy::turn_end::SignInNext {
            label: label.to_string(),
            renew,
        })
    };
    for word in [
        None,
        Some(RosterWord::Unlisted),
        Some(RosterWord::Unread),
        Some(RosterWord::Row {
            next: None,
            ready: None,
            held: None,
        }),
        Some(RosterWord::Row {
            next: next("work-2", true),
            ready: None,
            held: None,
        }),
        Some(RosterWord::Row {
            next: next("work-2", true),
            ready: None,
            held: Some(HeldBy::Earlier),
        }),
        Some(RosterWord::Row {
            next: None,
            ready: ready("pool-a"),
            held: Some(HeldBy::Earlier),
        }),
        Some(RosterWord::Row {
            next: next("work-2", false),
            ready: ready("pool-a"),
            held: Some(HeldBy::Pin),
        }),
        Some(RosterWord::Row {
            next: None,
            ready: ready("pool-a"),
            held: Some(HeldBy::Reset),
        }),
        Some(RosterWord::Row {
            next: None,
            ready: ready("pool-a"),
            held: None,
        }),
    ] {
        same(word);
    }
}

/// A row ready to run now.
fn ready(label: &str) -> Option<Ready> {
    Some(Ready {
        label: label.to_string(),
    })
}

/// THE ROSTER'S WORD ON A TAPE AN EARLIER BUILD WROTE, from before it named
/// a row ready to run now (acct3-5), reads back with none ready and nothing
/// holding the tab; one from before it said what holds the tab off that row
/// (ox4-5), with nothing holding it — which the notice's `{why}` beside a
/// ready row tells as the limit from before this Claude Code, as that build
/// did — and one from before a hold was kept with no row ready (r4h-4), as
/// the hold it names. NEGATIVE CONTROLS: the word with a row ready and its
/// hold reads back with both; a hold this build does not know is refused,
/// not read as another.
#[test]
fn a_rosters_word_an_earlier_build_taped_reads_back_with_none_ready() {
    let origin = Instant::now();
    let take = |bytes: &[u8]| <Option<RosterWord> as HostReply>::take(bytes, origin).ok();
    let work_2 = Some(crate::supervise::policy::turn_end::SignInNext {
        label: "work-2".to_string(),
        renew: true,
    });
    assert_eq!(
        take(b"{\"row\":[\"work-2\",true]}"),
        Some(Some(RosterWord::Row {
            next: work_2.clone(),
            ready: None,
            held: None,
        }))
    );
    assert_eq!(
        take(b"{\"ready\":{\"next\":null,\"ready\":\"pool-a\"}}"),
        Some(Some(RosterWord::Row {
            next: None,
            ready: ready("pool-a"),
            held: None,
        }))
    );
    assert_eq!(
        take(b"{\"ready\":{\"next\":null,\"ready\":\"pool-a\",\"held\":\"pin\"}}"),
        Some(Some(RosterWord::Row {
            next: None,
            ready: ready("pool-a"),
            held: Some(HeldBy::Pin),
        }))
    );
    assert_eq!(
        take(b"{\"held\":{\"next\":[\"work-2\",true],\"held\":\"earlier\"}}"),
        Some(Some(RosterWord::Row {
            next: work_2,
            ready: None,
            held: Some(HeldBy::Earlier),
        }))
    );
    for unknown in [
        &b"{\"ready\":{\"next\":null,\"ready\":\"pool-a\",\"held\":\"later\"}}"[..],
        &b"{\"held\":{\"next\":null,\"held\":\"later\"}}"[..],
    ] {
        assert_eq!(take(unknown), None);
    }
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
    let take = |kind: &str| {
        format!(
            "{{\"take\":{{\"until\":1,\"long\":true,\"limits\":[\"weekly\"],{kind}\"left\":2,\
             \"day\":\"Oct 5\"}}}}"
        )
    };
    assert!(refused::<ResetWord>(
        take("\"kind\":\"hourly\",").as_bytes()
    ));
    assert!(refused::<ResetWord>(take("").as_bytes()));
    assert!(!refused::<ResetWord>(
        take("\"kind\":\"weekly\",").as_bytes()
    ));
    assert!(refused::<ResetOutcome>(b"\"spent\""));
    let spent =
        |kind: &str| format!("{{\"spent\":{{{kind}\"asks\":true,\"left\":1,\"cleared_at\":5}}}}");
    assert!(refused::<ResetOutcome>(
        spent("\"kind\":\"hourly\",").as_bytes()
    ));
    assert!(refused::<ResetOutcome>(spent("").as_bytes()));
    assert!(!refused::<ResetOutcome>(
        spent("\"kind\":\"full\",").as_bytes()
    ));
    assert!(refused::<ResetOutcome>(b"{\"walled\":{\"left\":0}}"));
    assert!(refused::<Reach>(b"\"sideways\""));
    assert!(refused::<Option<HostStep>>(
        b"{\"line\":\"x\",\"moved\":true,\"typed\":\"song\",\"repeat\":false}"
    ));
    assert!(refused::<Option<AgentMemory>>(
        b"{\"footprint_mib\":1,\"physical_mib\":null,\"available_mib\":null,\"used_mib\":null,\
          \"pressure\":\"panic\",\"swap_kib_s\":null,\"largest_other_mib\":null}"
    ));
    assert!(refused::<Option<(String, HostTurn)>>(
        b"{\"line\":\"x\",\"turn\":\"song\"}"
    ));
    assert!(refused::<Option<(String, HostTurn)>>(b"{\"line\":\"x\"}"));
    assert!(refused::<Option<String>>(b"not json"));
    assert!(refused::<Option<AccountAdvice>>(b"{\"wait\":{}}"));
    assert!(refused::<BackgroundEvidence>(b"\"all-stuck\""));
    assert!(refused::<BackgroundEvidence>(
        b"{\"no-progress\":{\"pid\":1,\"exe\":\"x\",\"task\":null,\"quiet_ms\":1,\"wait\":null}}"
    ));
}

/// A LINE TYPED AT A BREAK ON A TAPE AN EARLIER BUILD WROTE, from before
/// such a line had a kind (`at_background`'s answer was the line alone),
/// reads back as the notice, as that build's loop took every such line; its
/// none reads back as none.
#[test]
fn a_break_line_an_earlier_build_taped_reads_back_as_the_notice() {
    let origin = Instant::now();
    assert_eq!(
        <Option<(String, HostTurn)> as HostReply>::take(b"\"notice:announced\"", origin).ok(),
        Some(Some(("notice:announced".to_string(), HostTurn::Notice)))
    );
    assert_eq!(
        <Option<(String, HostTurn)> as HostReply>::take(b"null", origin).ok(),
        Some(None)
    );
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
    let fine = std::panic::catch_unwind(|| {
        request(&[(
            "wait",
            &WaitPlan {
                at_unix: 1,
                why: "x".to_string(),
                kind: crate::supervise::WaitKind::Retry,
            },
        )])
    });
    assert!(fine.is_ok());
}

/// A window's host that hears the worker's NEEDS YOU set
/// ([`crate::supervise::IdleHost::needs_standing`]) and keeps each tell.
#[derive(Debug, Default)]
struct Hears {
    told: std::sync::Mutex<Vec<(Vec<String>, String)>>,
}

impl crate::supervise::IdleHost for Hears {
    fn wants(&self) -> bool {
        false
    }
    fn at_idle(&self) -> Option<crate::supervise::run::HostStep> {
        None
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn needs_standing(&self, needs: &[String], entry: &str) {
        self.told
            .lock()
            .expect("told")
            .push((needs.to_vec(), entry.to_string()));
    }
}

/// THE NEEDS SET IS A TAPED NOTIFICATION (`docs/DESIGN-harness-autopilot-2026-10-15.md`
/// §12.2.3, §12.2.6): the taped host tells the window's host the whole set
/// and the entry's text — and the take-down's `(&[], "")` — and tapes each
/// as an output at `host.needs_standing`, the set as each need's `Debug`
/// text in order (the `[T]` and `String` argument forms); a replay of the
/// same calls answers from the tape alone, equal. NEGATIVE CONTROL: a replay
/// told another set — one need fewer — diverges at `host.needs_standing`.
#[test]
fn needs_standing_is_a_taped_notification() {
    use astream_effects::EffectRecord;

    use crate::supervise::IdleHost;
    use crate::supervise::fx::ClockSource;
    use crate::supervise::tape::header::VERBATIM;
    use crate::supervise::tape::replay::ReplayCore;
    use crate::supervise::tape::testing::{MemoryTape, TAPE_ID};

    let set = vec![
        "grant Screen Recording to com.aterm.aterm in System Settings ▸ Privacy & Security"
            .to_string(),
        "the release credential (~/.aterm/release-credentials.toml) to sign 0.48.0".to_string(),
    ];
    let entry = "claude needs you: grant Screen Recording to com.aterm.aterm in System S… (+1 \
                 more) — carrying on";
    let tape = MemoryTape::over(&Fx::over(ClockSource::Scripted(Arc::default())));
    let inner = Arc::new(Hears::default());
    let host = TapedHost::new(Arc::clone(&inner) as Arc<dyn IdleHost>, tape.fx.clone());
    IdleHost::needs_standing(&host, &set, entry);
    IdleHost::needs_standing(&host, &[], "");
    assert_eq!(
        *inner.told.lock().expect("told"),
        [
            (set.clone(), entry.to_string()),
            (Vec::new(), String::new())
        ],
        "the window's host told each, the set whole"
    );
    let read = tape.finish("done");
    let records: Vec<EffectRecord> = read
        .items
        .iter()
        .filter_map(|i| i.record().cloned())
        .collect();
    let told: Vec<(String, String)> = records
        .iter()
        .map(|r| match r {
            EffectRecord::Emit { door, bytes } => {
                (door.clone(), String::from_utf8_lossy(bytes).into_owned())
            }
            other => panic!("a notification is an output: {other:?}"),
        })
        .collect();
    assert_eq!(
        told,
        [
            (
                "host.needs_standing".to_string(),
                format!("needs={set:?}\nentry={entry:?}")
            ),
            (
                "host.needs_standing".to_string(),
                "needs=[]\nentry=\"\"".to_string()
            ),
        ]
    );
    let core = ReplayCore::start(records.clone(), VERBATIM, VERBATIM, TAPE_ID, None);
    let replay = ReplayHost::new(Fx::replay(Arc::clone(&core)));
    IdleHost::needs_standing(&replay, &set, entry);
    IdleHost::needs_standing(&replay, &[], "");
    assert_eq!(core.left(), 0, "every tell matched on the tape");
    assert!(core.diverged().is_none());

    let core = ReplayCore::start(records, VERBATIM, VERBATIM, TAPE_ID, None);
    let replay = ReplayHost::new(Fx::replay(Arc::clone(&core)));
    let told = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        IdleHost::needs_standing(&replay, &set[..1], entry);
    }));
    assert!(told.is_err(), "the replay halts at the divergence");
    let d = core.diverged().expect("latched");
    assert_eq!(d.door.as_deref(), Some("host.needs_standing"), "{d:?}");
}

/// A hold with every field set (`docs/DESIGN-harness-autopilot-2026-10-15.md`
/// §12.3.3): a hold read at a nudge's yield, its open words two of the
/// strict reading's own, the repository's counts all non-zero.
fn a_full_hold() -> crate::harness::acts::LandHeld {
    use crate::harness::acts::{HoldLeg, LandHeld, OpenWord};
    LandHeld {
        since_ms: Some(1_760_000_000_000),
        nudges: 3,
        read: true,
        legs: HoldLeg::ALL.iter().fold(0, |bits, leg| bits | leg.bit()),
        open: [
            OpenWord::of("not merged"),
            OpenWord::of("follow-up"),
            None,
            None,
        ],
        changed: 1,
        untracked: 2,
        stashes: 3,
        unpushed: 4,
        unlanded: u16::MAX,
    }
}

/// A LAND HOLD IS A TAPE ARGUMENT WITHOUT AN INSTANT (§12.3.6, 3b): its
/// `Debug` — every field set — passes the request's raw-`Instant` assert,
/// and prints its open words as the strict reading spelled them, never an
/// index. NEGATIVE CONTROL: the test's own copy of it with an `Instant`
/// beside the hold trips that assert.
#[test]
fn a_land_hold_is_a_tape_arg_without_an_instant() {
    let held = a_full_hold();
    let text = String::from_utf8(request(&[("held", &Some(held))])).expect("utf-8");
    assert!(!super::super::core::holds_instant(&text), "{text}");
    assert!(
        text.starts_with("held=Some(LandHeld { since_ms: Some(1760000000000), nudges: 3,"),
        "{text}"
    );
    assert!(
        text.contains("open: [Some(\"not merged\"), Some(\"follow-up\"), None, None]"),
        "{text}"
    );
    assert_eq!(
        request(&[("held", &None::<crate::harness::acts::LandHeld>)]),
        b"held=None"
    );

    #[derive(Debug)]
    struct HeldAt {
        #[expect(dead_code, reason = "read through Debug alone")]
        held: crate::harness::acts::LandHeld,
        #[expect(dead_code, reason = "read through Debug alone")]
        at: Instant,
    }
    impl TapeArg for HeldAt {}
    let leaky = HeldAt {
        held,
        at: Instant::now(),
    };
    let built = std::panic::catch_unwind(|| request(&[("held", &leaky)]));
    assert!(built.is_err(), "a raw Instant beside the hold is refused");
}

/// A window's host that hears why Land still holds
/// ([`crate::supervise::IdleHost::land_held`]) and keeps each tell.
#[derive(Debug, Default)]
struct HearsHold {
    told: std::sync::Mutex<Vec<Option<crate::harness::acts::LandHeld>>>,
}

impl crate::supervise::IdleHost for HearsHold {
    fn wants(&self) -> bool {
        false
    }
    fn at_idle(&self) -> Option<crate::supervise::run::HostStep> {
        None
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn land_held(&self, held: Option<crate::harness::acts::LandHeld>) {
        self.told.lock().expect("told").push(held);
    }
}

/// THE HOLD IS A TAPED NOTIFICATION (§12.3.3, item 2): the taped host tells
/// the window's host each hold, and `None`, and tapes each as an output at
/// `host.land_held`, the hold as its `Debug` text; a replay of the same calls
/// answers from the tape alone, equal. NEGATIVE CONTROL: a replay told
/// another hold — one nudge fewer — diverges at `host.land_held`.
#[test]
fn land_held_is_a_taped_notification() {
    use astream_effects::EffectRecord;

    use crate::supervise::IdleHost;
    use crate::supervise::fx::ClockSource;
    use crate::supervise::tape::header::VERBATIM;
    use crate::supervise::tape::replay::ReplayCore;
    use crate::supervise::tape::testing::{MemoryTape, TAPE_ID};

    let held = a_full_hold();
    let tape = MemoryTape::over(&Fx::over(ClockSource::Scripted(Arc::default())));
    let inner = Arc::new(HearsHold::default());
    let host = TapedHost::new(Arc::clone(&inner) as Arc<dyn IdleHost>, tape.fx.clone());
    IdleHost::land_held(&host, Some(held));
    IdleHost::land_held(&host, None);
    assert_eq!(
        *inner.told.lock().expect("told"),
        [Some(held), None],
        "the window's host told each"
    );
    let read = tape.finish("done");
    let records: Vec<EffectRecord> = read
        .items
        .iter()
        .filter_map(|i| i.record().cloned())
        .collect();
    let told: Vec<(String, String)> = records
        .iter()
        .map(|r| match r {
            EffectRecord::Emit { door, bytes } => {
                (door.clone(), String::from_utf8_lossy(bytes).into_owned())
            }
            other => panic!("a notification is an output: {other:?}"),
        })
        .collect();
    assert_eq!(
        told,
        [
            (
                "host.land_held".to_string(),
                format!("held={:?}", Some(held))
            ),
            ("host.land_held".to_string(), "held=None".to_string()),
        ]
    );
    let core = ReplayCore::start(records.clone(), VERBATIM, VERBATIM, TAPE_ID, None);
    let replay = ReplayHost::new(Fx::replay(Arc::clone(&core)));
    IdleHost::land_held(&replay, Some(held));
    IdleHost::land_held(&replay, None);
    assert_eq!(core.left(), 0, "every tell matched on the tape");
    assert!(core.diverged().is_none());

    let core = ReplayCore::start(records, VERBATIM, VERBATIM, TAPE_ID, None);
    let replay = ReplayHost::new(Fx::replay(Arc::clone(&core)));
    let other = crate::harness::acts::LandHeld {
        nudges: held.nudges - 1,
        ..held
    };
    let told = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        IdleHost::land_held(&replay, Some(other));
    }));
    assert!(told.is_err(), "the replay halts at the divergence");
    let d = core.diverged().expect("latched");
    assert_eq!(d.door.as_deref(), Some("host.land_held"), "{d:?}");
}

/// A host whose board reads the reading it holds.
#[derive(Debug)]
struct Occupied(crate::supervise::occupancy::OccupancyReading);

impl crate::supervise::IdleHost for Occupied {
    fn wants(&self) -> bool {
        false
    }
    fn at_idle(&self) -> Option<crate::supervise::run::HostStep> {
        None
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn occupancy(&self) -> crate::supervise::occupancy::OccupancyReading {
        self.0.clone()
    }
}

/// THE OCCUPANCY READING ON A TAPE (the occupancy red-team's F8): it reads
/// back as written, every field, and a word this build does not know is
/// refused. A replay answers it from the tape; where the tape's header says
/// the recording build had no `host.occupancy`, it answers the empty reading
/// and takes no record. NEGATIVE CONTROLS: where the header names the door
/// and the tape does not hold it, the replay diverges — a newer build's tape
/// never replays a default silently — and a door the header lacks whose
/// answer has no absent form (`host.wants`) diverges as it always did.
#[test]
fn the_occupancy_reading_is_taped_and_absent_only_where_the_build_lacked_it() {
    use astream_effects::EffectRecord;

    use crate::supervise::IdleHost;
    use crate::supervise::fx::ClockSource;
    use crate::supervise::occupancy::{OccupancyReading, SlotsSeen};
    use crate::supervise::tape::header::VERBATIM;
    use crate::supervise::tape::replay::ReplayCore;
    use crate::supervise::tape::testing::{MemoryTape, TAPE_ID};

    let full = OccupancyReading {
        taken_ms: 1_760_000_000_000,
        stale: false,
        avail_mib: Some(96 * 1024),
        pressure: Some(Pressure::Warn),
        disk: Some(crate::harness::disk::Level::Ok),
        slots: Some(SlotsSeen { usable: 1, n: 3 }),
        slots_free_ms: Some(1_200_000),
        room: Some(true),
        held_slot_last_ms: Some(1_759_999_940_000),
    };
    same(OccupancyReading::default());
    same(full.clone());
    let refused = |bytes: &[u8]| OccupancyReading::take(bytes, Instant::now()).is_err();
    let wire = |pressure: &str, disk: &str| {
        format!(
            "{{\"taken-ms\":1,\"stale\":false,\"avail-mib\":null,\"pressure\":{pressure},\
             \"disk\":{disk},\"slots\":null,\"slots-free-ms\":null,\"room\":null,\
             \"held-slot-last-ms\":null}}"
        )
    };
    assert!(refused(wire("\"panic\"", "null").as_bytes()));
    assert!(refused(wire("null", "\"full\"").as_bytes()));
    assert!(!refused(wire("\"critical\"", "\"emergency\"").as_bytes()));

    let tape = MemoryTape::over(&Fx::over(ClockSource::Scripted(Arc::default())));
    let host = TapedHost::new(
        Arc::new(Occupied(full.clone())) as Arc<dyn IdleHost>,
        tape.fx.clone(),
    );
    assert_eq!(IdleHost::occupancy(&host), full);
    let read = tape.finish("done");
    let records: Vec<EffectRecord> = read
        .items
        .iter()
        .filter_map(|i| i.record().cloned())
        .collect();
    assert!(
        matches!(&records[..], [EffectRecord::Exchange { door, .. }] if door == "host.occupancy"),
        "{records:?}"
    );

    let doors = |names: &[&str]| names.iter().map(ToString::to_string).collect::<Vec<_>>();
    for named in [None, Some(doors(&["host.wants", "host.occupancy"]))] {
        let core = ReplayCore::start(records.clone(), VERBATIM, VERBATIM, TAPE_ID, None);
        if let Some(named) = named {
            core.recorded_doors(named);
        }
        let replay = ReplayHost::new(Fx::replay(Arc::clone(&core)));
        assert_eq!(IdleHost::occupancy(&replay), full);
        assert_eq!(core.left(), 0, "answered from the tape");
    }

    for named in [doors(&["host.wants"]), Vec::new()] {
        let core = ReplayCore::start(Vec::new(), VERBATIM, VERBATIM, TAPE_ID, None);
        core.recorded_doors(named);
        let replay = ReplayHost::new(Fx::replay(Arc::clone(&core)));
        assert_eq!(IdleHost::occupancy(&replay), OccupancyReading::default());
        assert!(core.diverged().is_none());
    }

    let core = ReplayCore::start(Vec::new(), VERBATIM, VERBATIM, TAPE_ID, None);
    core.recorded_doors(doors(&["host.occupancy"]));
    let replay = ReplayHost::new(Fx::replay(Arc::clone(&core)));
    let asked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        IdleHost::occupancy(&replay)
    }));
    assert!(asked.is_err(), "the replay halts: the tape never asked");
    assert!(core.diverged().is_some());

    let core = ReplayCore::start(Vec::new(), VERBATIM, VERBATIM, TAPE_ID, None);
    core.recorded_doors(Vec::new());
    let replay = ReplayHost::new(Fx::replay(Arc::clone(&core)));
    let asked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| IdleHost::wants(&replay)));
    assert!(
        asked.is_err(),
        "no absent answer: the replay halts as before"
    );
}

/// A host whose machine pace answers what it holds, and whose upgrade's
/// notice is due.
#[derive(Debug)]
struct Paced(crate::supervise::occupancy::PaceGrant);

impl crate::supervise::IdleHost for Paced {
    fn wants(&self) -> bool {
        false
    }
    fn at_idle(&self) -> Option<crate::supervise::run::HostStep> {
        None
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn upgrade_due(&self) -> bool {
        true
    }
    fn occupancy_pace(
        &self,
        _: crate::supervise::occupancy::RuleId,
        _: i64,
    ) -> crate::supervise::occupancy::PaceGrant {
        self.0
    }
}

/// THE DOOR'S HOST CALLS ON A TAPE (the occupancy red-team's F10, F11): every
/// pace answer reads back as written and a word this build does not know is
/// refused; `occupancy_pace` is a round trip whose request names the rule and
/// the hold's start, `upgrade_due` a round trip, `break_taken` an output, and
/// a replay of the same calls answers from the tape alone. NEGATIVE CONTROL: a
/// replay asked the pace for another rule diverges at `host.occupancy_pace`.
#[test]
fn the_doors_host_calls_are_taped() {
    use astream_effects::EffectRecord;

    use crate::supervise::IdleHost;
    use crate::supervise::fx::ClockSource;
    use crate::supervise::occupancy::{PaceGrant, PaceRefusal, RuleId};
    use crate::supervise::tape::header::VERBATIM;
    use crate::supervise::tape::replay::ReplayCore;
    use crate::supervise::tape::testing::{MemoryTape, TAPE_ID};

    for grant in [
        PaceGrant::Local,
        PaceGrant::Granted,
        PaceGrant::Machine,
        PaceGrant::Reserved,
    ]
    .into_iter()
    .chain(PaceRefusal::ALL.map(PaceGrant::Refused))
    {
        same(grant);
    }
    let refused = |bytes: &[u8]| PaceGrant::take(bytes, Instant::now()).is_err();
    assert!(refused(b"\"refused:tired\""));
    assert!(refused(b"\"slow\""));
    // CONTROL: a word it knows is read.
    assert!(!refused(b"\"refused:carry-owed\""));

    let since = 1_760_000_000_000;
    let tape = MemoryTape::over(&Fx::over(ClockSource::Scripted(Arc::default())));
    let host = TapedHost::new(
        Arc::new(Paced(PaceGrant::Reserved)) as Arc<dyn IdleHost>,
        tape.fx.clone(),
    );
    assert!(IdleHost::upgrade_due(&host));
    assert_eq!(
        IdleHost::occupancy_pace(&host, RuleId::HoldRoom, since),
        PaceGrant::Reserved
    );
    IdleHost::break_taken(&host);
    let read = tape.finish("done");
    let records: Vec<EffectRecord> = read
        .items
        .iter()
        .filter_map(|i| i.record().cloned())
        .collect();
    let seen: Vec<(String, String)> = records
        .iter()
        .map(|r| match r {
            EffectRecord::Exchange { door, request, .. } => {
                (door.clone(), String::from_utf8_lossy(request).into_owned())
            }
            EffectRecord::Emit { door, bytes } => {
                (door.clone(), String::from_utf8_lossy(bytes).into_owned())
            }
            other => panic!("a host call: {other:?}"),
        })
        .collect();
    assert_eq!(
        seen,
        [
            ("host.upgrade_due".to_string(), String::new()),
            (
                "host.occupancy_pace".to_string(),
                format!("rule=HoldRoom\nhold_since_ms={since}")
            ),
            ("host.break_taken".to_string(), String::new()),
        ]
    );
    assert!(matches!(&records[1], EffectRecord::Exchange { .. }));
    assert!(matches!(&records[2], EffectRecord::Emit { .. }));

    let core = ReplayCore::start(records.clone(), VERBATIM, VERBATIM, TAPE_ID, None);
    let replay = ReplayHost::new(Fx::replay(Arc::clone(&core)));
    assert!(IdleHost::upgrade_due(&replay));
    assert_eq!(
        IdleHost::occupancy_pace(&replay, RuleId::HoldRoom, since),
        PaceGrant::Reserved
    );
    IdleHost::break_taken(&replay);
    assert_eq!(core.left(), 0, "answered from the tape");
    assert!(core.diverged().is_none());

    // NEGATIVE CONTROL: another rule's pace diverges at the door.
    let core = ReplayCore::start(records, VERBATIM, VERBATIM, TAPE_ID, None);
    let replay = ReplayHost::new(Fx::replay(Arc::clone(&core)));
    assert!(IdleHost::upgrade_due(&replay));
    let asked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        IdleHost::occupancy_pace(&replay, RuleId::WaitNudge, since)
    }));
    assert!(asked.is_err(), "the replay halts at the divergence");
    let d = core.diverged().expect("latched");
    assert_eq!(d.door.as_deref(), Some("host.occupancy_pace"), "{d:?}");
}

/// WHAT THE DOOR TELLS ITS HOST IS TAPED (`host.occupancy_said`, 1e): an output whose request
/// is the told word's `Debug`, `said=<OccupancySaid { … }>`, and a replay of the same tell
/// reproduces from the tape alone. NEGATIVE CONTROL: a replay told another word (the hold
/// declared) diverges at the door.
#[test]
fn the_doors_tell_is_taped() {
    use astream_effects::EffectRecord;

    use crate::supervise::IdleHost;
    use crate::supervise::config::Occupancy;
    use crate::supervise::fx::ClockSource;
    use crate::supervise::occupancy::{
        OccupancyReading, OccupancySaid, PaceGrant, RuleId, SaidHold, SaidRule, SaidVerdict,
    };
    use crate::supervise::tape::header::VERBATIM;
    use crate::supervise::tape::replay::ReplayCore;
    use crate::supervise::tape::testing::{MemoryTape, TAPE_ID};

    let since = 1_760_000_000_000;
    let said = OccupancySaid {
        mode: Occupancy::Observe,
        hold: Some(SaidHold {
            since_ms: since,
            declared: false,
        }),
        ready: false,
        guards: Some(crate::supervise::occupancy::rules::Guards {
            person: true,
            ..crate::supervise::occupancy::rules::Guards::clear(
                crate::supervise::config::Autopilot::Cruise,
            )
        }),
        rules: [
            SaidRule {
                rule: RuleId::WaitNudge,
                verdict: SaidVerdict::Held(crate::supervise::Guard::Person),
                at_ms: since + 1_800_000,
            },
            SaidRule {
                rule: RuleId::HoldRoom,
                verdict: SaidVerdict::Quiet,
                at_ms: 0,
            },
        ],
        at_ms: since + 1_800_000,
        reading: OccupancyReading::default(),
    };
    let tape = MemoryTape::over(&Fx::over(ClockSource::Scripted(Arc::default())));
    let host = TapedHost::new(
        Arc::new(Paced(PaceGrant::Local)) as Arc<dyn IdleHost>,
        tape.fx.clone(),
    );
    IdleHost::occupancy_said(&host, &said);
    let read = tape.finish("done");
    let records: Vec<EffectRecord> = read
        .items
        .iter()
        .filter_map(|i| i.record().cloned())
        .collect();
    let [EffectRecord::Emit { door, bytes }] = records.as_slice() else {
        panic!("one output: {records:?}");
    };
    assert_eq!(door, "host.occupancy_said");
    assert_eq!(
        String::from_utf8_lossy(bytes),
        format!("said={said:?}"),
        "the request is the word's Debug"
    );

    let core = ReplayCore::start(records.clone(), VERBATIM, VERBATIM, TAPE_ID, None);
    let replay = ReplayHost::new(Fx::replay(Arc::clone(&core)));
    IdleHost::occupancy_said(&replay, &said);
    assert_eq!(core.left(), 0, "answered from the tape");
    assert!(core.diverged().is_none());

    // NEGATIVE CONTROL: another word diverges at the door.
    let core = ReplayCore::start(records, VERBATIM, VERBATIM, TAPE_ID, None);
    let replay = ReplayHost::new(Fx::replay(Arc::clone(&core)));
    let mut declared = said.clone();
    declared.hold = Some(SaidHold {
        since_ms: since,
        declared: true,
    });
    let told = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        IdleHost::occupancy_said(&replay, &declared);
    }));
    let d = core.diverged();
    assert!(
        told.is_err() || d.is_some(),
        "the replay halts or latches at the divergence"
    );
    assert_eq!(
        d.expect("latched").door.as_deref(),
        Some("host.occupancy_said")
    );
}
