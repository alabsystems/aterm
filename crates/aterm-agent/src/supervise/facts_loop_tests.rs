// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the supervision-loop facts seam across turn-end riders and break lines.

use super::*;
use crate::supervise::BackgroundCall;
use crate::supervise::facts::EpisodeKey;

const NO_PROGRESS: &str = "fact-no-progress@v1";

const BORN: u64 = 1_792_498_000_123_456;
const REBORN: u64 = BORN + 1;

fn hung(birth: u64) -> BackgroundCall {
    BackgroundCall {
        pid: 27098,
        pgid: 27098,
        birth: Some(birth),
        exe: PLANT.to_string(),
        task: Some(PLANT.to_string()),
        quiet_ms: 31 * MIN.unsigned_abs(),
        wait: Some(PLANT.to_string()),
    }
}

fn told(birth: u64) -> String {
    hung(birth).told()
}

/// What a step of a run reads: a break of the agent's background work, or a turn end the
/// policy decides at (the agent's last words numbered by the step).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Break,
    End,
}

struct Run<'a> {
    tag: &'a str,
    policy: SupervisorConfig,
    steps: Vec<(i64, Step)>,
    host: Arc<DoorHost>,
    seed: Option<(String, i64)>,
}

struct Both {
    ran: Ran,
    /// Each step's writes' texts (`send … -- <text>`).
    sent: Vec<Vec<String>>,
    told: Vec<bool>,
}

impl Both {
    fn sends(&self) -> Vec<String> {
        self.sent.iter().flatten().cloned().collect()
    }

    fn facts(&self, door: &str) -> Vec<String> {
        let mark = format!(" door={door} ");
        self.ran
            .noted
            .iter()
            .filter(|l| l.starts_with("FACT "))
            .filter_map(|l| l.split_once(&mark).map(|(_, w)| w.to_string()))
            .collect()
    }

    fn rows(&self) -> Vec<(String, String, String)> {
        self.ran
            .ledger
            .lines()
            .filter_map(|line| aterm_json::from_str::<aterm_json::Value>(line).ok())
            .map(|v| {
                let text = |k: &str| {
                    v.get(k)
                        .and_then(aterm_json::Value::as_str)
                        .unwrap_or("")
                        .to_string()
                };
                (text("rule_id"), text("decision"), text("reason"))
            })
            .filter(|(rule, _, reason)| {
                rule == NO_PROGRESS || (rule == INTENT && reason.starts_with("kind=no-progress "))
            })
            .collect()
    }
}

fn sent_text(request: &str) -> Option<String> {
    request
        .starts_with("send ")
        .then(|| request.split_once(" -- ").map(|(_, t)| t.to_string()))
        .flatten()
}

fn riding(birth: u64) -> String {
    format!(" (aterm: {})", told(birth))
}

fn stuck(birth: u64) -> String {
    templates::stuck_line(&told(birth))
}

fn keyed(occupancy: Occupancy, key: bool) -> SupervisorConfig {
    SupervisorConfig {
        stuck_break_note: key,
        ..policy(occupancy)
    }
}

impl<'a> Run<'a> {
    fn new(tag: &'a str, policy: SupervisorConfig, steps: &[(i64, Step)]) -> Self {
        let host = Arc::new(DoorHost::default());
        *host.facts.lock().expect("facts") = Some(vec![hung(BORN)]);
        Self {
            tag,
            policy,
            steps: steps.to_vec(),
            host,
            seed: None,
        }
    }

    fn run(
        self,
        asked: &[u64],
        before: &mut dyn FnMut(&mut Session<'_, Logged<'_>>, &DoorHost, usize),
    ) -> Both {
        let (dir, path) = ledger_at(self.tag);
        let mut m = Mock::new(vec![break_screen()]);
        m.gen_fence = true;
        m.sends_gen = true;
        m.human_seq = Some(7);
        m.human_ms = Some(60 * MIN.unsigned_abs());
        m.help = "key [id=<key>] [if=<re>] [if-gen=<e.s>] [if-human=<n>] <name>: send a named \
                  key\nsend [id=<key>] [if=<re>] [if-gen=<e.s>] [if-human=<n>] <text>: write \
                  text\n"
            .to_string();
        let start_ms = self.seed.as_ref().map_or(T0, |(_, at)| *at);
        if let Some((body, _)) = &self.seed {
            std::fs::write(&path, body).expect("the predecessor's ledger");
        }
        let mut logged = Logged {
            m: &mut m,
            ledger: path.clone(),
            seen: Vec::new(),
        };
        let opts = SuperviseOpts {
            policy: self.policy.clone(),
            idle_host: Some(Arc::clone(&self.host) as Arc<dyn IdleHost>),
            ..hosted(30)
        };
        let mut sent = Vec::new();
        let (noted, told) = {
            let clock = Arc::new(crate::supervise::fx::ScriptedClock::default());
            clock.stand_mono();
            clock.set_at_ms(start_ms);
            let mut s = session(&mut logged, Some("@s-1".to_string()));
            s.set_fx(Fx::over(ClockSource::Scripted(Arc::clone(&clock))));
            s.ledger = approvals::Ledger::open(Some(&path), Some("@s-1"), &mut Vec::new());
            s.stall_host = Some(Arc::clone(&self.host) as Arc<dyn IdleHost>);
            s.setup.background_settle = Duration::ZERO;
            s.set_turn_end_timing(TurnEndTiming {
                take_within: Duration::ZERO,
                ..TurnEndTiming::default()
            });
            let mut heard = Heard::default();
            let body = self.seed.as_ref().map_or("", |(body, _)| body.as_str());
            let start = s.fx.mono();
            s.hold_seed_from(body, start, start_ms, false, &self.policy, &heard);
            let mut since = None;
            let mut at_ms = start_ms;
            for (i, (s_at, step)) in self.steps.iter().enumerate() {
                let ahead = start_ms + s_at * 1000 - at_ms;
                if ahead > 0 {
                    skip(&clock, &mut at_ms, ahead);
                }
                *self.host.facts_at.lock().expect("facts_at") = at_ms;
                before(&mut s, &self.host, i);
                let rows = match step {
                    Step::Break => break_screen(),
                    Step::End => ran_then_ended(&format!(
                        "Stage {i} of the parser is in; the lexer is next."
                    )),
                };
                s.ctl.m.screens = vec![rows];
                s.ctl.m.served = 0;
                let from = s.ctl.seen.len();
                let screen = s.screen().expect("the step's read");
                s.clock_marks = Some((s.fx.mono(), at_ms));
                match step {
                    Step::Break => s
                        .host_steps_in_background(
                            &screen,
                            &mut since,
                            Some(&self.policy),
                            &mut heard,
                        )
                        .expect("the break's step"),
                    Step::End => {
                        since = None;
                        let point = s.turn_of(screen);
                        let action = s
                            .turn_end_at_point(&point, Some(Duration::from_secs(720)), &opts)
                            .expect("the point's decision");
                        s.turn_end_execute(&point, action, &opts, &[], &mut heard)
                            .expect("the point's act");
                    }
                }
                sent.push(
                    s.ctl.seen[from..]
                        .iter()
                        .filter_map(|(r, _)| sent_text(r))
                        .collect(),
                );
            }
            let told = asked
                .iter()
                .map(|&birth| s.facts.state.is_told(EpisodeKey::no_progress(27098, birth)))
                .collect();
            (heard.noted.clone(), told)
        };
        let requests = logged.seen;
        let rows = fact_rows(&path);
        let ledger = std::fs::read_to_string(&path).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&dir);
        Both {
            ran: Ran {
                noted,
                requests,
                rows,
                calls: self.host.calls(),
                ledger,
                land: false,
                in_flight: false,
                said: self.host.said.lock().expect("said").clone(),
            },
            sent,
            told,
        }
    }
}

/// A turn of work ([`ended`]) that ran a call before its words.
fn ran_then_ended(said: &str) -> Vec<String> {
    let mut r = ended(said);
    let at = r
        .iter()
        .position(|row| row.starts_with("⏺ "))
        .expect("the words");
    r.splice(
        at..at,
        [
            "⏺ Bash(targo --unverified test -p parser)",
            "  ⎿  test result: ok",
            "",
        ]
        .map(str::to_owned),
    );
    r
}

fn took(s: &mut Session<'_, Logged<'_>>) {
    s.took(7, Some(&policy(Occupancy::Off)), &mut Heard::default());
}

fn taking(s: &mut Session<'_, Logged<'_>>, _: &DoorHost, i: usize) {
    if i > 0 {
        took(s);
    }
}

#[test]
fn a_hung_call_is_told_once_per_episode_across_both_doors() {
    let at = |m: i64, step| (m * 60, step);
    let rider = Run::new(
        "facts-rider-first",
        keyed(Occupancy::Observe, true),
        &[
            at(0, Step::End),
            at(1, Step::Break),
            at(2, Step::End),
            at(3, Step::End),
            at(4, Step::Break),
            at(5, Step::End),
        ],
    )
    .run(&[BORN, REBORN], &mut |s, host, i| {
        taking(s, host, i);
        match i {
            3 => *host.facts.lock().expect("facts") = Some(vec![hung(REBORN)]),
            4 => *host.facts.lock().expect("facts") = Some(Vec::new()),
            5 => *host.facts.lock().expect("facts") = Some(vec![hung(REBORN)]),
            _ => {}
        }
    });
    let sent = &rider.sent;
    let rode = |i: usize, birth: u64| sent[i].len() == 1 && sent[i][0].ends_with(&riding(birth));
    assert!(rode(0, BORN), "{sent:#?}");
    assert!(sent[1].is_empty(), "told: no line at the break: {sent:#?}");
    assert!(
        sent[2].len() == 1 && !sent[2][0].contains(" (aterm: "),
        "{sent:#?}"
    );
    assert!(rode(3, REBORN), "another birth: {sent:#?}");
    assert!(rode(5, REBORN), "over, then quiet again: {sent:#?}");
    assert_eq!(
        rider.told,
        [false, true],
        "the first ended, the second told"
    );
    assert!(rider.facts("break").is_empty(), "{:#?}", rider.ran.noted);
    let informed: Vec<&String> = rider
        .ran
        .calls
        .iter()
        .filter(|c| c.starts_with("inform "))
        .collect();
    assert_eq!(
        informed,
        [BORN, REBORN, REBORN]
            .map(|b| format!("inform {}", stuck(b)))
            .iter()
            .collect::<Vec<_>>(),
        "the person's record of each fact told"
    );
    let words = |rows: &[(String, String, String)]| -> Vec<(String, String)> {
        rows.iter()
            .map(|(rule, decision, _)| (rule.clone(), decision.clone()))
            .collect()
    };
    let told_rows = [
        (INTENT.to_string(), "waiting".to_string()),
        (NO_PROGRESS.to_string(), "typed".to_string()),
    ];
    let thrice: Vec<_> = told_rows.iter().cycle().take(6).cloned().collect();
    assert_eq!(words(&rider.rows()), thrice);
    assert!(
        rider
            .rows()
            .iter()
            .all(|(_, _, reason)| reason.contains(" door=turn-end")),
        "{:#?}",
        rider.rows()
    );

    let first = Run::new(
        "facts-break-first",
        keyed(Occupancy::Off, true),
        &[at(0, Step::Break), at(1, Step::End), at(2, Step::Break)],
    )
    .run(&[BORN], &mut taking);
    assert_eq!(first.sent[0], [stuck(BORN)], "{:#?}", first.ran.noted);
    assert!(
        first.sent[1].len() == 1 && !first.sent[1][0].contains(" (aterm: "),
        "{:#?}",
        first.sent
    );
    assert!(first.sent[2].is_empty(), "{:#?}", first.sent);
    assert_eq!(first.facts("break"), ["typed"]);
    assert_eq!(first.told, [true]);
    assert_eq!(words(&first.rows()), told_rows);
}

#[test]
fn the_stuck_fact_goes_after_the_hosts_notice_and_before_the_due_nudge() {
    let at = |m: i64, step| (m * 60, step);
    let run = Run {
        host: {
            let host = Arc::new(DoorHost::default());
            host.wants.store(true, Ordering::SeqCst);
            host
        },
        ..Run::new(
            "facts-rank",
            keyed(Occupancy::Prompt, true),
            &[
                at(0, Step::Break),
                at(31, Step::Break),
                at(32, Step::Break),
                at(33, Step::Break),
                at(53, Step::Break),
                at(54, Step::Break),
                at(55, Step::End),
            ],
        )
    };
    let ran = run.run(&[BORN], &mut |s, host, i| match i {
        1 => {
            *host.facts.lock().expect("facts") = Some(vec![hung(BORN)]);
            *host.notice.lock().expect("notice") = Some("the notice".to_string());
        }
        2 | 3 | 6 => took(s),
        _ => {}
    });
    let sends = ran.sends();
    assert_eq!(sends.len(), 3, "{sends:#?}");
    assert_eq!(sends[0], stuck(BORN), "{:#?}", ran.ran.noted);
    assert!(
        sends[1].ends_with(&format!("(rule {NUDGE})")),
        "the nudge after it: {sends:#?}"
    );
    assert!(!sends[2].contains(" (aterm: "), "told: {sends:#?}");
    assert_eq!(
        ran.sent[2],
        [stuck(BORN)],
        "at the break after the notice's"
    );
    assert_eq!(ran.facts("break"), ["held:act-untaken", "typed"]);
    let nudge = wrote(&ran.ran.noted);
    assert_eq!(
        nudge.first(),
        Some(&said(NUDGE, "held:host-line")),
        "{nudge:?}"
    );
    assert_eq!(nudge.last(), Some(&said(NUDGE, "typed")), "{nudge:?}");
    assert!(
        nudge.iter().any(|(_, w)| w.starts_with("paced ")),
        "the session's pace past the stuck line: {nudge:?}"
    );
    let calls = &ran.ran.calls;
    let at_call = |c: &str| calls.iter().position(|k| k == c);
    let notice = at_call("at_background").expect("the notice");
    let line = at_call(&format!("inform {}", stuck(BORN))).expect("the stuck line told");
    let paced = at_call(&format!("occupancy_pace {NUDGE}")).expect("the nudge's write");
    assert!(notice < line && line < paced, "{calls:#?}");
}

#[test]
fn the_stuck_line_waits_out_the_pace_of_a_nudge_the_door_typed() {
    let at = |m: i64, step| (m * 60, step);
    let run = Run {
        host: Arc::new(DoorHost::default()),
        ..Run::new(
            "facts-after-nudge",
            keyed(Occupancy::Prompt, true),
            &[
                at(0, Step::Break),
                at(31, Step::Break),
                at(33, Step::Break),
                at(52, Step::Break),
            ],
        )
    };
    let ran = run.run(&[BORN], &mut |s, host, i| {
        if i == 2 {
            took(s);
            *host.facts.lock().expect("facts") = Some(vec![hung(BORN)]);
        }
    });
    assert!(
        ran.sent[1].len() == 1 && ran.sent[1][0].ends_with(&format!("(rule {NUDGE})")),
        "the nudge: {:#?}",
        ran.sent
    );
    assert!(ran.sent[2].is_empty(), "{:#?}", ran.ran.noted);
    assert_eq!(ran.sent[3], [stuck(BORN)], "{:#?}", ran.ran.noted);
    assert_eq!(ran.facts("break"), ["paced", "typed"]);
}

#[test]
fn with_the_key_off_a_break_is_mains_exactly() {
    let run = |tag: &str, occupancy, key: bool, reading: bool| {
        let host = Arc::new(DoorHost::default());
        *host.facts.lock().expect("facts") = reading.then(|| vec![hung(BORN)]);
        Script {
            policy: keyed(occupancy, key),
            host,
            ..Script::new(tag, occupancy, &[0, 31 * 60, 32 * 60])
        }
        .run(&mut |s, i| {
            if i == 2 {
                took(s);
            }
        })
    };
    for occupancy in [Occupancy::Off, Occupancy::Observe, Occupancy::Prompt] {
        let read = run("facts-key-off-read", occupancy, false, true);
        let none = run("facts-key-off-none", occupancy, false, false);
        assert_eq!(read.requests(), none.requests(), "{occupancy:?}");
        assert_eq!(read.calls, none.calls, "{occupancy:?}");
        assert!(!read.calls.iter().any(|c| c == "facts"), "{occupancy:?}");
        assert_eq!(read.rows, none.rows, "{occupancy:?}");
        assert_eq!(read.noted, none.noted, "{occupancy:?}");
    }
    let on = |occupancy| run("facts-key-on", occupancy, true, true);
    let (off, observe) = (on(Occupancy::Off), on(Occupancy::Observe));
    for (mode, ran) in [("off", &off), ("observe", &observe)] {
        let sends: Vec<String> = ran.requests().iter().filter_map(|r| sent_text(r)).collect();
        assert_eq!(sends, [stuck(BORN)], "{mode}: {:#?}", ran.noted);
        let booked: Vec<(String, String)> = ran
            .ledger
            .lines()
            .filter(|l| l.contains("kind=no-progress"))
            .map(|l| {
                let v = aterm_json::from_str::<aterm_json::Value>(l).expect("a row");
                let text = |k: &str| v.get(k).and_then(aterm_json::Value::as_str).unwrap_or("");
                (text("rule_id").to_string(), text("decision").to_string())
            })
            .collect();
        assert_eq!(
            booked,
            [
                (INTENT.to_string(), "waiting".to_string()),
                (NO_PROGRESS.to_string(), "typed".to_string()),
            ],
            "{mode}"
        );
    }
    assert_eq!(
        off.requests(),
        observe.requests(),
        "off and observe ask alike"
    );
}

#[test]
fn a_fact_that_would_not_fit_leaves_the_prompt_alone_and_stays_untold() {
    let long = "keep going ".repeat(69).trim_end().to_string();
    let ran = Run::new(
        "facts-unfit",
        SupervisorConfig {
            continue_text: long.clone(),
            ..keyed(Occupancy::Off, true)
        },
        &[(0, Step::End), (60, Step::Break)],
    )
    .run(&[BORN], &mut taking);
    assert_eq!(ran.sent[0].len(), 1, "{:#?}", ran.ran.noted);
    assert!(ran.sent[0][0].starts_with(&long), "{:#?}", ran.sent);
    assert!(!ran.sent[0][0].contains(" (aterm: "), "{:#?}", ran.sent);
    assert!(
        ran.rows()
            .iter()
            .all(|(_, _, reason)| !reason.contains(" door=turn-end")),
        "nothing booked at the prompt: {:#?}",
        ran.rows()
    );
    assert_eq!(ran.sent[1], [stuck(BORN)], "untold: {:#?}", ran.ran.noted);
}

#[test]
fn facts_off_tells_none_at_either_door() {
    let ran = Run::new(
        "facts-off",
        SupervisorConfig {
            facts: false,
            ..keyed(Occupancy::Prompt, true)
        },
        &[(0, Step::End), (60, Step::Break), (120, Step::End)],
    )
    .run(&[BORN], &mut taking);
    assert!(
        ran.sends()
            .iter()
            .all(|t| !t.contains(" (aterm: ") && *t != stuck(BORN)),
        "{:#?}",
        ran.sent
    );
    assert_eq!(
        ran.sent[0].len(),
        1,
        "the prompt still went: {:#?}",
        ran.sent
    );
    assert!(
        !ran.ran.calls.iter().any(|c| c == "facts"),
        "{:#?}",
        ran.ran.calls
    );
    assert!(ran.rows().is_empty(), "{:#?}", ran.rows());
}

#[test]
fn a_restart_does_not_retell_a_told_call() {
    let first = Run::new(
        "facts-seed-1",
        keyed(Occupancy::Off, true),
        &[(0, Step::Break)],
    )
    .run(&[BORN], &mut taking);
    assert_eq!(first.sent[0], [stuck(BORN)]);
    let successor = |tag: &str, body: &str| {
        let at_ms = T0 + 2 * MIN;
        Run {
            seed: Some((stamped(body, at_ms - MIN), at_ms)),
            ..Run::new(
                tag,
                keyed(Occupancy::Off, true),
                &[(0, Step::Break), (60, Step::End)],
            )
        }
        .run(&[BORN], &mut taking)
    };
    let kept = successor("facts-seed-2", &first.ran.ledger);
    assert!(kept.sent[0].is_empty(), "{:#?}", kept.ran.noted);
    assert!(
        kept.sent[1].len() == 1 && !kept.sent[1][0].contains(" (aterm: "),
        "{:#?}",
        kept.sent
    );
    let crashed: String = first
        .ran
        .ledger
        .lines()
        .filter(|l| !l.contains(&format!("\"rule_id\":\"{NO_PROGRESS}\"")))
        .map(|l| format!("{l}\n"))
        .collect();
    assert!(crashed.contains("kind=no-progress"), "the intent stands");
    let retold = successor("facts-seed-3", &crashed);
    assert_eq!(retold.sent[0], [stuck(BORN)], "{:#?}", retold.ran.noted);
    assert!(
        !retold.sent[1][0].contains(" (aterm: "),
        "once: {:#?}",
        retold.sent
    );
}

#[test]
fn a_prompt_left_in_the_composer_says_its_fact_not_submitted() {
    let fenced = Run::new(
        "facts-left",
        keyed(Occupancy::Off, false),
        &[(0, Step::End)],
    )
    .run(&[BORN], &mut |s, _, _| {
        s.ctl
            .m
            .key_replies
            .push_back(ok("OK skipped reason=person seq=101\n"));
    });
    assert!(
        fenced.sent[0].len() == 1 && fenced.sent[0][0].ends_with(&riding(BORN)),
        "{:#?}",
        fenced.sent
    );
    let guarded = |tag: &str, reply: CtlReply| {
        let left = Run::new(tag, keyed(Occupancy::Off, false), &[(0, Step::End)]).run(
            &[BORN],
            &mut |s, _, _| {
                s.caps.gen_fence = Some(false);
                s.ctl
                    .m
                    .verb_replies
                    .insert("turn", VecDeque::from([reply.clone()]));
            },
        );
        assert!(
            left.ran
                .requests
                .iter()
                .any(|(r, _)| r.starts_with("turn ") && r.ends_with(&riding(BORN))),
            "{tag}: {:#?}",
            left.ran.requests
        );
        left
    };
    let missed = guarded(
        "facts-left-guard",
        ok("OK 0 turn skipped reason=guard submitted=0 seq=102 id=1\n"),
    );
    let halted = guarded(
        "facts-left-halted",
        ok("OK 0 turn skipped reason=halted submitted=0 seq=102 id=1\n"),
    );
    let busy = guarded(
        "facts-left-busy",
        err(aterm_types::control_verbs::TURN_SUBMIT_BUSY_REPLY
            .strip_prefix("ERR ")
            .expect("an ERR reply")),
    );
    let busy_before = guarded("facts-left-busy-older", err("busy sink"));
    for left in [fenced, missed, halted, busy, busy_before] {
        let rows = left.rows();
        assert_eq!(
            rows.last()
                .map(|(rule, decision, _)| (rule.as_str(), decision.as_str())),
            Some((NO_PROGRESS, "skipped")),
            "{rows:#?}"
        );
        assert!(
            rows.last()
                .is_some_and(|(_, _, reason)| reason.ends_with(" why=not-submitted")),
            "{rows:#?}"
        );
        assert_eq!(left.told, [false], "{:#?}", left.ran.noted);
    }
}

#[test]
fn the_stuck_lines_pace_counts_lines_that_went_across_a_restart() {
    use crate::supervise::occupancy::rules::{RuleId, SESSION_PACE_MS, WOULD_RULE, would_reason};
    let first = Run::new(
        "facts-pace-1",
        keyed(Occupancy::Off, true),
        &[(0, Step::Break)],
    )
    .run(&[BORN], &mut taking);
    assert_eq!(first.sent[0], [stuck(BORN)], "{:#?}", first.ran.noted);
    let at_ms = T0 + 10 * MIN;
    let after = |tag: &str, body: String, occupancy| {
        let run = Run::new(tag, keyed(occupancy, true), &[(0, Step::Break)]);
        *run.host.facts.lock().expect("facts") = Some(vec![hung(REBORN)]);
        Run {
            seed: Some((body, at_ms)),
            ..run
        }
        .run(&[REBORN], &mut taking)
    };
    for went in [at_ms - 5 * MIN, at_ms + MIN].map(|ts| stamped(&first.ran.ledger, ts)) {
        for occupancy in [Occupancy::Off, Occupancy::Observe, Occupancy::Prompt] {
            let paced = after("facts-pace-2", went.clone(), occupancy);
            assert!(
                paced.sent[0].is_empty(),
                "{occupancy:?}: {:#?}",
                paced.ran.noted
            );
            assert_eq!(
                paced.facts("break"),
                ["paced"],
                "{occupancy:?}: {:#?}",
                paced.ran.noted
            );
        }
    }
    let reason = would_reason(RuleId::WaitNudge, 1, 31 * MIN, false);
    let would = approvals::Row {
        rule_id: WOULD_RULE,
        outcome: approvals::Outcome::Waiting,
        command: "",
        reason: &reason,
        box_seq: 1,
    }
    .to_json(at_ms - 5 * MIN, Some("@s-1"));
    let ahead = stamped(&first.ran.ledger, at_ms + SESSION_PACE_MS);
    for body in [would, ahead] {
        for occupancy in [Occupancy::Off, Occupancy::Observe, Occupancy::Prompt] {
            let free = after("facts-pace-3", body.clone(), occupancy);
            assert_eq!(
                free.sent[0],
                [stuck(REBORN)],
                "{occupancy:?}: {:#?}",
                free.ran.noted
            );
        }
    }
    use crate::supervise::occupancy::door::{Told, outcome_reason};
    let reason = outcome_reason(RuleId::WaitNudge, 1, 31 * MIN, false, Told::Typed);
    let nudged = approvals::Row {
        rule_id: crate::supervise::facts::FactKind::Hold.rule(),
        outcome: approvals::Outcome::Typed,
        command: "a nudge",
        reason: &reason,
        box_seq: 1,
    }
    .to_json(at_ms - 5 * MIN, Some("@s-1"));
    for occupancy in [Occupancy::Off, Occupancy::Observe, Occupancy::Prompt] {
        let paced = after("facts-pace-4", nudged.clone(), occupancy);
        assert!(
            paced.sent[0].is_empty(),
            "{occupancy:?}: {:#?}",
            paced.ran.noted
        );
        assert_eq!(
            paced.facts("break"),
            ["paced"],
            "{occupancy:?}: {:#?}",
            paced.ran.noted
        );
    }
}

#[test]
fn a_planted_word_reaches_no_write_at_either_door() {
    let runs = [
        Run::new(
            "facts-plant-end",
            keyed(Occupancy::Off, true),
            &[(0, Step::End)],
        )
        .run(&[BORN], &mut taking),
        Run::new(
            "facts-plant-break",
            keyed(Occupancy::Prompt, true),
            &[(0, Step::Break)],
        )
        .run(&[BORN], &mut taking),
    ];
    for ran in &runs {
        assert!(ran.told[0], "told: {:#?}", ran.ran.noted);
        let typed = ran.sends();
        assert!(!typed.is_empty(), "{:#?}", ran.ran.requests());
        assert!(typed.iter().all(|t| !t.contains(PLANT)), "{typed:#?}");
        let informed: Vec<&String> = ran
            .ran
            .calls
            .iter()
            .filter(|c| c.starts_with("inform "))
            .collect();
        assert!(informed.iter().all(|c| !c.contains(PLANT)), "{informed:#?}");
    }
}
