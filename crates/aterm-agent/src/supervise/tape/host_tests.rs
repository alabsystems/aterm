// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::time::{Duration, Instant};

use super::*;

const AHEAD: Duration = Duration::from_secs(400 * 24 * 3600);

fn moved<A: HostReply>(value: &A, from: Instant, to: Instant) -> A {
    let bytes = value.put(from);
    A::take(&bytes, to).unwrap_or_else(|e| {
        panic!(
            "{e}: {:?} does not read back",
            String::from_utf8_lossy(&bytes)
        )
    })
}

fn same<A: HostReply + PartialEq + fmt::Debug>(value: A) {
    let origin = Instant::now();
    assert_eq!(moved(&value, origin, origin), value);
}

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
    same(crate::harness::repo_work::RepoRead::Clean);
    same(crate::harness::repo_work::RepoRead::Unread);
    same(crate::harness::repo_work::RepoRead::Found(
        crate::harness::repo_work::RepoWork {
            repo: "/Users//a/Developer/r".to_string(),
            changed: 2,
            untracked: 1,
            stashes: 1,
            unpushed: 3,
            unlanded: 4,
            main_ref: "origin/main".to_string(),
            paths: vec!["src/a.rs".to_string(), "notes/".to_string()],
            state: Some(u64::MAX),
        },
    ));
    let old =
        br#"{"repo":"/r","changed":1,"untracked":0,"stashes":0,"unpushed":2,"paths":["a.rs"]}"#;
    assert_eq!(
        <crate::harness::repo_work::RepoRead as TapeValue>::from_tape(old)
            .expect("an older tape's finding"),
        crate::harness::repo_work::RepoRead::Found(crate::harness::repo_work::RepoWork {
            repo: "/r".to_string(),
            changed: 1,
            unpushed: 2,
            paths: vec!["a.rs".to_string()],
            ..Default::default()
        })
    );
    assert_eq!(
        <crate::harness::repo_work::RepoRead as TapeValue>::from_tape(b"null")
            .expect("an older tape's clean probe"),
        crate::harness::repo_work::RepoRead::Clean
    );
    for (main_ref, remote) in [("origin/main", true), ("", false)] {
        let checked =
            crate::harness::repo_work::RepoRead::Checked(crate::harness::repo_work::CleanAt {
                repo: "/Users//a/Developer/r".to_string(),
                head: "3f9a1c2e4b7d".repeat(4)[..40].to_string(),
                main_ref: main_ref.to_string(),
                remote,
            });
        let bytes = checked.to_tape();
        assert!(
            bytes.starts_with(br#"{"at":{"#),
            "a checked reading is taped under `at`: {}",
            String::from_utf8_lossy(&bytes)
        );
        same(Some(checked.clone()));
        same(checked);
    }
    same(None::<crate::harness::repo_work::RepoRead>);
    same(Some(crate::harness::repo_work::RepoRead::Unread));
    for (old, read) in [
        (&b"null"[..], crate::harness::repo_work::RepoRead::Clean),
        (
            &b"\"unread\""[..],
            crate::harness::repo_work::RepoRead::Unread,
        ),
        (
            &br#"{"repo":"/r","changed":1,"untracked":0,"stashes":0,"unpushed":0,"paths":[]}"#[..],
            crate::harness::repo_work::RepoRead::Found(crate::harness::repo_work::RepoWork {
                repo: "/r".to_string(),
                changed: 1,
                ..Default::default()
            }),
        ),
    ] {
        assert_eq!(
            <Option<crate::harness::repo_work::RepoRead> as TapeValue>::from_tape(old)
                .expect("an answer a tape held before a probe could read on"),
            Some(read),
            "{}",
            String::from_utf8_lossy(old)
        );
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
    same(None::<TapWindow>);
    same(None::<crate::harness::profile::AccountKey>);
    same(crate::harness::profile::AccountKey::claude("u-invented"));
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
    let call = |task: Option<&str>, wait: Option<&str>, birth: Option<u64>| BackgroundCall {
        pid: 27098,
        pgid: u32::MAX,
        birth,
        exe: "aterm_gui-e0964a".to_string(),
        task: task.map(str::to_string),
        quiet_ms: u64::MAX,
        wait: wait.map(str::to_string),
    };
    for (task, wait, birth) in [
        (None, None, None),
        (Some("bplle5dxj"), Some("mutex"), Some(u64::MAX)),
    ] {
        same(BackgroundEvidence::NoProgress {
            worst: call(task, wait, birth),
        });
        same(Some(crate::supervise::FactsReading {
            as_of_ms: i64::MIN,
            no_progress: vec![call(task, wait, birth), call(None, None, Some(1))],
        }));
    }
    same(None::<crate::supervise::FactsReading>);
    same(Some(crate::supervise::FactsReading {
        as_of_ms: 1_760_000_000_000,
        no_progress: Vec::new(),
    }));
    let before_birth =
        br#"{"no-progress":{"pid":1,"pgid":2,"exe":"x","task":null,"quiet_ms":3,"wait":null}}"#;
    assert_eq!(
        BackgroundEvidence::take(before_birth, Instant::now()).ok(),
        Some(BackgroundEvidence::NoProgress {
            worst: BackgroundCall {
                pid: 1,
                pgid: 2,
                birth: None,
                exe: "x".to_string(),
                task: None,
                quiet_ms: 3,
                wait: None,
            }
        }),
        "a tape from before the leader's birth was read reads back without one"
    );
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

fn ready(label: &str) -> Option<Ready> {
    Some(Ready {
        label: label.to_string(),
    })
}

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

#[test]
fn a_request_names_each_argument() {
    assert!(request(&[]).is_empty());
    let point = IdlePoint {
        done_under_land: true,
        held: None,
        theirs: None,
    };
    let text = "a \"quoted\"\nline";
    assert_eq!(
        String::from_utf8(request(&[("point", &point), ("text", &text)])).expect("utf-8"),
        "point=IdlePoint { done_under_land: true }\ntext=\"a \\\"quoted\\\"\\nline\"",
        "a point nothing holds is spelled as tapes recorded before `held` spell it"
    );
    let held = IdlePoint {
        held: Some(Guard::Person),
        ..point
    };
    assert_eq!(
        String::from_utf8(request(&[("point", &held)])).expect("utf-8"),
        "point=IdlePoint { done_under_land: true, held: Person }"
    );
    let theirs = IdlePoint {
        held: Some(Guard::Exchange),
        theirs: Some(Guard::Exchange),
        ..point
    };
    assert_eq!(
        String::from_utf8(request(&[("point", &theirs)])).expect("utf-8"),
        "point=IdlePoint { done_under_land: true, held: Exchange, theirs: Exchange }"
    );
}

/// The notification keeps its original six-field request, while its live
/// recipient still receives the full sample. New input observation times are
/// restamped by the recipient, not a reason to change a tape's request.
#[test]
fn an_account_probe_request_keeps_its_six_original_fields() {
    use astream_effects::{EffectRecord, ReplayDivergence};

    use crate::supervise::IdleHost;
    use crate::supervise::fx::ClockSource;
    use crate::supervise::tape::header::VERBATIM;
    use crate::supervise::tape::replay::ReplayCore;
    use crate::supervise::tape::report::Cause;
    use crate::supervise::tape::testing::{MemoryTape, TAPE_ID};

    const SIX: &str = "Sample { session_pct: Some(12), session_reset: Some(100), week_pct: Some(34), week_reset: Some(200), at: 42, by: \"m27\" }";
    const DOOR: &str = "host.account_probed";

    #[derive(Debug, Default)]
    struct HearsProbe(std::sync::Mutex<Vec<Result<Sample, ProbeMiss>>>);

    impl IdleHost for HearsProbe {
        fn wants(&self) -> bool {
            false
        }
        fn at_idle(&self) -> Option<crate::supervise::run::HostStep> {
            None
        }
        fn owns_turn_end(&self) -> bool {
            false
        }
        fn account_probed(&self, read: Result<&Sample, ProbeMiss>) {
            self.0.lock().expect("told").push(read.cloned());
        }
    }

    let sample = Sample {
        session_pct: Some(12),
        session_reset: Some(100),
        week_pct: Some(34),
        week_reset: Some(200),
        at: 42,
        by: "m27".to_string(),
        session_read_at: Some(40),
        week_read_at: Some(41),
    };
    let historical = format!("read=Ok({SIX})").into_bytes();
    let tape = MemoryTape::over(&Fx::over(ClockSource::Scripted(Arc::default())));
    let inner = Arc::new(HearsProbe::default());
    let host = TapedHost::new(Arc::clone(&inner) as Arc<dyn IdleHost>, tape.fx.clone());
    IdleHost::account_probed(&host, Ok(&sample));
    IdleHost::account_probed(&host, Err(ProbeMiss::Renderer));
    assert_eq!(
        *inner.0.lock().expect("told"),
        [Ok(sample.clone()), Err(ProbeMiss::Renderer)],
        "the live recipient gets both observation times unchanged"
    );
    let records: Vec<EffectRecord> = tape
        .finish("done")
        .items
        .iter()
        .filter_map(|item| item.record().cloned())
        .collect();
    assert_eq!(
        records,
        [
            EffectRecord::Emit {
                door: DOOR.to_string(),
                bytes: historical.clone(),
            },
            EffectRecord::Emit {
                door: DOOR.to_string(),
                bytes: b"read=Err(Renderer)".to_vec(),
            },
        ],
        "the generated host method records the historical request"
    );
    let replay = |bytes: &[u8], read: Result<&Sample, ProbeMiss>| {
        let core = ReplayCore::start(
            vec![EffectRecord::Emit {
                door: DOOR.to_string(),
                bytes: bytes.to_vec(),
            }],
            VERBATIM,
            VERBATIM,
            TAPE_ID,
            None,
        );
        let host = ReplayHost::new(Fx::replay(Arc::clone(&core)));
        let told = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            IdleHost::account_probed(&host, read);
        }));
        (told.is_ok(), core.left(), core.diverged())
    };
    for (session_read_at, week_read_at) in [
        (sample.session_read_at, sample.week_read_at),
        (None, None),
        (Some(i64::MIN), Some(i64::MAX)),
    ] {
        let incoming = Sample {
            session_read_at,
            week_read_at,
            ..sample.clone()
        };
        assert_eq!(replay(&historical, Ok(&incoming)), (true, 0, None));
    }
    assert_eq!(
        replay(b"read=Err(Renderer)", Err(ProbeMiss::Renderer)),
        (true, 0, None)
    );
    let changed = [
        Sample {
            session_pct: Some(13),
            ..sample.clone()
        },
        Sample {
            session_reset: Some(101),
            ..sample.clone()
        },
        Sample {
            week_pct: Some(35),
            ..sample.clone()
        },
        Sample {
            week_reset: Some(201),
            ..sample.clone()
        },
        Sample {
            at: 43,
            ..sample.clone()
        },
        Sample {
            by: "other".to_string(),
            ..sample.clone()
        },
    ];
    let differs = |bytes: &[u8], read| {
        let (finished, left, divergence) = replay(bytes, read);
        assert!(!finished && left == 1, "the mismatch is not consumed");
        let divergence = divergence.expect("latched");
        assert_eq!(divergence.door.as_deref(), Some(DOOR));
        assert!(matches!(
            divergence.cause,
            Cause::Refused(ReplayDivergence::OutputDiverged { recorded, emitted, .. })
                if recorded == bytes && emitted != recorded
        ));
    };
    for incoming in &changed {
        differs(&historical, Ok(incoming));
    }
    differs(b"read=Err(Renderer)", Err(ProbeMiss::Unsettled));

    // Short-lived development tapes with the expanded diagnostic shape stay
    // strict: this does not normalize their bytes or claim they are compatible.
    let diagnostic = format!("read={:?}", Ok::<_, ProbeMiss>(&sample));
    assert!(diagnostic.contains("session_read_at: Some(40)"));
    assert!(diagnostic.contains("week_read_at: Some(41)"));
    differs(diagnostic.as_bytes(), Ok(&sample));

    let wrapped = Some(Ok::<_, ProbeMiss>(&sample));
    let values = [wrapped, None];
    assert_eq!(
        request(&[("nested", &(wrapped, &values[..]))]),
        format!("nested=(Some(Ok({SIX})), [Some(Ok({SIX})), None])").into_bytes()
    );
    let ordinary = (
        Some(Ok::<_, &str>("a \"quote\"\nline")),
        &[None, Some("b\\c")][..],
    );
    assert_eq!(
        request(&[("ordinary", &ordinary)]),
        format!("ordinary={ordinary:?}").into_bytes(),
        "nested non-sample arguments retain ordinary Debug text"
    );
}

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
        self.0
    }
}

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
    same(full);
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
        Arc::new(Occupied(full)) as Arc<dyn IdleHost>,
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

#[test]
fn the_doors_host_calls_are_taped() {
    use astream_effects::EffectRecord;

    use crate::supervise::IdleHost;
    use crate::supervise::fx::ClockSource;
    use crate::supervise::occupancy::{
        OccupancyReading, PaceGrant, PaceRefusal, RuleId, SlotsSeen,
    };
    use crate::supervise::tape::header::VERBATIM;
    use crate::supervise::tape::replay::ReplayCore;
    use crate::supervise::tape::testing::{MemoryTape, TAPE_ID};

    let reread = OccupancyReading {
        taken_ms: 1_760_000_000_000,
        stale: false,
        avail_mib: Some(64 * 1024),
        pressure: Some(Pressure::Normal),
        disk: Some(crate::harness::disk::Level::Ok),
        slots: Some(SlotsSeen { usable: 2, n: 3 }),
        slots_free_ms: Some(1_500_000),
        room: Some(true),
        held_slot_last_ms: Some(1_759_999_940_000),
    };
    for grant in [
        PaceGrant::Local,
        PaceGrant::Granted,
        PaceGrant::Room(OccupancyReading::default()),
        PaceGrant::Room(reread),
        PaceGrant::Machine,
        PaceGrant::Reserved,
        PaceGrant::Stale,
        PaceGrant::Unmet,
    ]
    .into_iter()
    .chain(PaceRefusal::ALL.map(PaceGrant::Refused))
    {
        same(grant);
    }
    let read = |bytes: &[u8]| PaceGrant::take(bytes, Instant::now());
    let refused = |bytes: &[u8]| read(bytes).is_err();
    assert!(refused(b"\"refused:tired\""));
    assert!(refused(b"\"slow\""));
    assert!(
        refused(b"\"room\""),
        "a re-read board's word without its reading"
    );
    let room = String::from_utf8(PaceGrant::Room(reread).put(Instant::now())).expect("utf-8");
    assert!(room.starts_with("{\"room\":{"), "{room}");
    assert!(refused(room.replace("\"normal\"", "\"panic\"").as_bytes()));
    assert!(!refused(b"\"refused:carry-owed\""));
    for (word, grant) in [
        (&b"\"granted\""[..], PaceGrant::Granted),
        (b"\"local\"", PaceGrant::Local),
        (b"\"machine\"", PaceGrant::Machine),
        (b"\"reserved\"", PaceGrant::Reserved),
    ] {
        assert_eq!(read(word).ok(), Some(grant));
    }

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
        rules: vec![
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
        ready_idle: None,
        wake: None,
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

/// A host that keeps a book of wakes and a task record: slice 2's doors answer what it holds,
/// the rest are the defaults.
#[derive(Debug)]
struct Booked;

impl crate::supervise::IdleHost for Booked {
    fn wants(&self) -> bool {
        false
    }
    fn at_idle(&self) -> Option<crate::supervise::run::HostStep> {
        None
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn wakes(&self) -> WakesSeen {
        booked_wakes()
    }
    fn wake_claim(&self, _: u16, _: u64, _: WakeDoor) -> WakeClaim {
        WakeClaim::Busy
    }
    fn task_fact(&self, word: Option<FactWord>) -> NoteOutcome {
        if word.is_some() {
            NoteOutcome::Fenced
        } else {
            NoteOutcome::Cleared
        }
    }
}

/// [`Booked`]'s book: a pending wake on a file, and a critical one an `await wake` answered,
/// whose need names the build slots.
fn booked_wakes() -> WakesSeen {
    WakesSeen {
        wakes: vec![
            WakeSeen {
                ord: 1,
                class: Declarer::Agent,
                prio: Prio::Normal,
                state: WakeState::Pending,
                declared_ms: 1_000,
                ready_since_ms: None,
                kinds: vec![AtomKind::File],
                need: None,
                need_slots: false,
                need_met: true,
                await_only: false,
            },
            WakeSeen {
                ord: 3,
                class: Declarer::Owner,
                prio: Prio::Critical,
                state: WakeState::Answered { at_ms: 9_000 },
                declared_ms: 2_000,
                ready_since_ms: Some(8_000),
                kinds: vec![AtomKind::Match, AtomKind::Sys, AtomKind::In],
                need: Some("sys slots>=1 for=1200000ms".to_string()),
                need_slots: true,
                need_met: true,
                await_only: false,
            },
        ],
    }
}

/// A host with only the methods the trait requires: every slice-2 door is the trait's default.
#[derive(Debug)]
struct Bare;

impl crate::supervise::IdleHost for Bare {
    fn wants(&self) -> bool {
        false
    }
    fn at_idle(&self) -> Option<crate::supervise::run::HostStep> {
        None
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
}

/// SLICE 2'S DOORS ARE INERT BY DEFAULT: a host that answers none of them keeps no book (the
/// empty one), holds no wake for any door (every claim `Gone`), and keeps no task record (every
/// fact, a word or a clear, `NoTask`), so a host that never learns a door grants no line by it.
/// NEGATIVE CONTROL: [`Booked`], which answers them, is told apart on each.
#[test]
fn slice_twos_doors_hold_nothing_on_a_host_that_answers_none() {
    use crate::supervise::IdleHost;

    let word = FactWord::new(crate::supervise::occupancy::RuleId::HoldRoom, 1).expect("ordinal");
    let bare: &dyn IdleHost = &Bare;
    assert_eq!(bare.wakes(), WakesSeen::default());
    for door in [WakeDoor::Break, WakeDoor::Point, WakeDoor::Await] {
        assert_eq!(bare.wake_claim(1, 7, door), WakeClaim::Gone, "{door:?}");
    }
    bare.wake_release(1, 7);
    bare.wake_told(1, 7, WakeDoor::Break);
    assert_eq!(bare.task_fact(Some(word)), NoteOutcome::NoTask);
    assert_eq!(bare.task_fact(None), NoteOutcome::NoTask);

    let booked: &dyn IdleHost = &Booked;
    assert_ne!(booked.wakes(), WakesSeen::default());
    assert_ne!(booked.wake_claim(1, 7, WakeDoor::Break), WakeClaim::Gone);
    assert_ne!(booked.task_fact(Some(word)), NoteOutcome::NoTask);
    assert_ne!(booked.task_fact(None), NoteOutcome::NoTask);
}

/// SLICE 2'S DOORS ON A TAPE (the slice-2 plan's 2.0): each answer reads back as written, and a
/// word this build does not know is refused; `wakes` is a round trip, `wake_claim` and
/// `task_fact` round trips that act, `wake_release` an output that acts and `wake_told` an
/// output, each request naming its arguments in closed words and numbers; a replay of the same
/// calls answers from the tape alone. Where the tape's header says the recording build had no
/// `host.wakes` (or names no door at all), a replay reads the empty book and takes no record, and
/// [`Fx::slice2_recorded`] is false there alone. NEGATIVE CONTROLS: a header that names the door
/// over a tape that never asked diverges; `wake_claim`, which has no absent form, diverges where
/// the build lacked it; a replay asked another wake's claim diverges at the door.
#[test]
fn slice_twos_doors_are_taped_and_the_book_absent_only_where_the_build_lacked_it() {
    use astream_effects::EffectRecord;

    use crate::supervise::IdleHost;
    use crate::supervise::fx::ClockSource;
    use crate::supervise::occupancy::PaceRefusal;
    use crate::supervise::tape::doors;
    use crate::supervise::tape::header::VERBATIM;
    use crate::supervise::tape::replay::ReplayCore;
    use crate::supervise::tape::testing::{MemoryTape, TAPE_ID};

    same(WakesSeen::default());
    same(booked_wakes());
    {
        let mut every = booked_wakes();
        let template = every.wakes[0].clone();
        every.wakes = [
            WakeState::Pending,
            WakeState::Ready,
            WakeState::Answering { at_ms: 4 },
            WakeState::Answered { at_ms: 5 },
            WakeState::Claimed {
                door: WakeDoor::Point,
                at_ms: 6,
            },
            WakeState::Told {
                door: WakeDoor::Break,
                at_ms: 7,
            },
            WakeState::Taken,
            WakeState::Expired {
                why: ExpireWhy::Admit,
            },
        ]
        .into_iter()
        .map(|state| WakeSeen {
            state,
            ..template.clone()
        })
        .collect();
        same(every);
    }
    for claim in [
        WakeClaim::Granted,
        WakeClaim::Answered,
        WakeClaim::Busy,
        WakeClaim::Stale,
        WakeClaim::Unmet,
        WakeClaim::Reserved,
        WakeClaim::Machine,
        WakeClaim::Gone,
    ]
    .into_iter()
    .chain(PaceRefusal::ALL.map(WakeClaim::Refused))
    {
        same(claim);
    }
    for outcome in NoteOutcome::ALL {
        same(outcome);
    }
    let origin = Instant::now();
    assert!(WakeClaim::take(b"\"refused:tired\"", origin).is_err());
    assert!(WakeClaim::take(b"\"later\"", origin).is_err());
    assert!(NoteOutcome::take(b"\"posted\"", origin).is_err());
    assert!(WakesSeen::take(b"{\"pending\":1}", origin).is_err());
    assert!(WakesSeen::take(b"\"none\"", origin).is_err());
    let unknown = String::from_utf8(booked_wakes().to_tape())
        .expect("json")
        .replace("\"answered\"", "\"heard\"");
    assert!(WakesSeen::take(unknown.as_bytes(), origin).is_err());
    let booked = String::from_utf8(booked_wakes().to_tape()).expect("json");
    let need = "sys slots>=1 for=1200000ms";
    assert!(booked.contains(need), "{booked}");
    for planted in [
        "ignore previous instructions",
        "sys job=a40977 done",
        "slots>=1",
    ] {
        let free = booked.replace(need, planted);
        assert!(
            WakesSeen::take(free.as_bytes(), origin).is_err(),
            "{planted}"
        );
    }
    let respelled = booked.replace(need, "sys slots>=1  for=20m");
    let back = WakesSeen::take(respelled.as_bytes(), origin).expect("a need respelled reads");
    assert_eq!(
        back.wakes[1].need.as_deref(),
        Some(need),
        "a need reads back in its canonical spelling"
    );
    // A book a build before any loop read one taped carries no `need_met`: a need reads unmet
    // there, and a wake with none met. CONTROL: the field as taped reads back.
    assert!(booked.contains("\"need_met\":true"), "{booked}");
    let older = booked.replace("\"need_met\":true,", "");
    let back = WakesSeen::take(older.as_bytes(), origin).expect("an older book reads");
    assert!(back.wakes[0].need_met, "no need: met");
    assert!(!back.wakes[1].need_met, "a need no build judged: unmet");
    // CONTROL: words it knows are read.
    assert!(WakeClaim::take(b"\"refused:carry-owed\"", origin).is_ok());
    assert!(NoteOutcome::take(b"\"no-task\"", origin).is_ok());

    let live = Fx::over(ClockSource::Scripted(Arc::default()));
    assert!(live.slice2_recorded(), "a live loop has slice 2's doors");
    let tape = MemoryTape::over(&live);
    assert!(tape.fx.slice2_recorded(), "a recording one too");
    let host = TapedHost::new(Arc::new(Booked) as Arc<dyn IdleHost>, tape.fx.clone());
    let word = FactWord::new(crate::supervise::occupancy::RuleId::HoldRoom, 1).expect("ordinal");
    let episode = 0xabc;
    let calls = |host: &dyn IdleHost| {
        assert_eq!(host.wakes(), booked_wakes());
        assert_eq!(
            host.wake_claim(3, episode, WakeDoor::Break),
            WakeClaim::Busy
        );
        host.wake_release(3, episode);
        host.wake_told(3, episode, WakeDoor::Await);
        assert_eq!(host.task_fact(Some(word)), NoteOutcome::Fenced);
        assert_eq!(host.task_fact(None), NoteOutcome::Cleared);
    };
    calls(&host);
    let read = tape.finish("done");
    let records: Vec<EffectRecord> = read
        .items
        .iter()
        .filter_map(|i| i.record().cloned())
        .collect();
    let seen: Vec<(String, bool, String)> = records
        .iter()
        .map(|r| match r {
            EffectRecord::Exchange { door, request, .. } => (
                door.clone(),
                true,
                String::from_utf8_lossy(request).into_owned(),
            ),
            EffectRecord::Emit { door, bytes } => (
                door.clone(),
                false,
                String::from_utf8_lossy(bytes).into_owned(),
            ),
            other => panic!("a host call: {other:?}"),
        })
        .collect();
    let at = format!("ord=3\nepisode={episode}");
    assert_eq!(
        seen,
        [
            ("host.wakes".to_string(), true, String::new()),
            (
                "host.wake_claim".to_string(),
                true,
                format!("{at}\ndoor={:?}", WakeDoor::Break)
            ),
            ("host.wake_release".to_string(), false, at.clone()),
            (
                "host.wake_told".to_string(),
                false,
                format!("{at}\ndoor={:?}", WakeDoor::Await)
            ),
            (
                "host.task_fact".to_string(),
                true,
                format!("word={:?}", Some(word))
            ),
            ("host.task_fact".to_string(), true, "word=None".to_string()),
        ]
    );
    for (door, _, request) in &seen {
        assert!(
            !request.contains('"'),
            "closed words and numbers: {request}"
        );
        let act = ["host.wake_claim", "host.wake_release", "host.task_fact"].contains(&&**door);
        assert_eq!(doors::is_host_act(door), act, "{door}");
    }

    let core = ReplayCore::start(records.clone(), VERBATIM, VERBATIM, TAPE_ID, None);
    let replay = ReplayHost::new(Fx::replay(Arc::clone(&core)));
    calls(&replay);
    assert_eq!(core.left(), 0, "answered from the tape");
    assert!(core.diverged().is_none());

    // A BUILD WITHOUT THE DOOR: the empty book, no record taken, and the key false.
    let named = |names: &[&str]| names.iter().map(ToString::to_string).collect::<Vec<_>>();
    for doors in [named(&["host.wants", "host.occupancy_said"]), Vec::new()] {
        let core = ReplayCore::start(Vec::new(), VERBATIM, VERBATIM, TAPE_ID, None);
        core.recorded_doors(doors);
        let fx = Fx::replay(Arc::clone(&core));
        assert!(!fx.slice2_recorded());
        let replay = ReplayHost::new(fx);
        assert_eq!(replay.wakes(), WakesSeen::default());
        assert!(core.diverged().is_none());
    }

    // NEGATIVE CONTROL: a header naming the door over a tape that never asked diverges.
    let core = ReplayCore::start(Vec::new(), VERBATIM, VERBATIM, TAPE_ID, None);
    core.recorded_doors(named(&["host.wakes"]));
    let fx = Fx::replay(Arc::clone(&core));
    assert!(fx.slice2_recorded());
    let replay = ReplayHost::new(fx);
    let asked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| replay.wakes()));
    assert!(asked.is_err(), "the replay halts: the tape never asked");
    assert!(core.diverged().is_some());

    // NEGATIVE CONTROL: a claim has no absent form.
    let core = ReplayCore::start(Vec::new(), VERBATIM, VERBATIM, TAPE_ID, None);
    core.recorded_doors(named(&["host.wants"]));
    let replay = ReplayHost::new(Fx::replay(Arc::clone(&core)));
    let asked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        replay.wake_claim(3, episode, WakeDoor::Break)
    }));
    assert!(asked.is_err(), "no absent answer: the replay halts");

    // NEGATIVE CONTROL: another wake's claim diverges at the door.
    let core = ReplayCore::start(records, VERBATIM, VERBATIM, TAPE_ID, None);
    let replay = ReplayHost::new(Fx::replay(Arc::clone(&core)));
    assert_eq!(replay.wakes().pending(), 2);
    let asked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        replay.wake_claim(4, episode, WakeDoor::Break)
    }));
    assert!(asked.is_err(), "the replay halts at the divergence");
    let d = core.diverged().expect("latched");
    assert_eq!(d.door.as_deref(), Some("host.wake_claim"), "{d:?}");
}
