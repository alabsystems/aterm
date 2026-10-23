// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The need table (`docs/DESIGN-owner-surfaces-2026-10-16.md` §10).

use super::*;
use aterm_types::needs::Severity;

const NOW: i64 = 1_789_660_500;

fn agent(word: &'static str) -> Option<AgentFact> {
    Some(AgentFact {
        word,
        detail: None,
        rev: 7,
        subject: None,
    })
}

fn boxed(detail: &str, subject: &str) -> Option<AgentFact> {
    Some(AgentFact {
        word: "prompt",
        detail: Some(detail.to_string()),
        rev: 9,
        subject: Some(subject.to_string()),
    })
}

fn entry(owner: &str, text: &str, kind: Option<AttentionKind>) -> AttentionFact {
    AttentionFact {
        owner: owner.to_string(),
        text: text.to_string(),
        kind,
    }
}

fn facts() -> NeedFacts {
    NeedFacts {
        now_unix: NOW,
        ..NeedFacts::default()
    }
}

/// A stall whose restart the pill carries (or a stopped job's, whose Resume
/// it carries): its words alone are the need's.
fn stall(stopped: bool) -> Option<StallFact> {
    Some(StallFact {
        text: format!("claude is frozen: not reading input since 14:02 {STALL_REMEDY}"),
        key: 0xfeed,
        stopped,
        act: !stopped,
        hand_back_frees: false,
    })
}

/// The agent-facing remedy a stall's text ends with
/// (`input_stall::attention_text`).
const STALL_REMEDY: &str = "\u{2014} restart it: aterm ctl @s-b7cf523445a1b0d8658e signal term";

/// An identified agent's verdict while the server publishes its input
/// stall (`AgentPublication::effective`): `wall:unresponsive`, detail
/// `stopped` for a stopped job.
fn unresponsive(stopped: bool, rev: u64) -> Option<AgentFact> {
    Some(AgentFact {
        word: "wall:unresponsive",
        detail: stopped.then(|| "stopped".to_string()),
        rev,
        subject: None,
    })
}

fn stalled(why: &'static str) -> Option<TaskFact> {
    Some(TaskFact {
        stalled: true,
        why: Some(why),
        entered: 4,
    })
}

/// `(need word, wait word)` of a verdict.
fn words(v: &Verdict) -> (Option<NeedWord>, Option<WaitWord>) {
    (
        v.need.as_ref().map(|n| n.word),
        v.waiting.as_ref().map(|w| w.word),
    )
}

/// THE TABLE: every §2.2 case and every Waiting cause, each row naming the
/// need and the wait it gives.
#[test]
fn every_need_and_every_wait_has_its_row() {
    use AttentionKind::{Ask, Wait};
    use NeedWord::*;
    use WaitWord::{Api, Entry, Limit, MemoryRestart, Overloaded};
    let usage = |reset_unix: Option<i64>, answer: WallAnswer| NeedFacts {
        agent: Some(AgentFact {
            detail: Some("7:30pm".to_string()),
            ..agent("wall:usage-session").unwrap()
        }),
        wall_answer: answer,
        reset_unix,
        ..facts()
    };
    type Row = (
        &'static str,
        NeedFacts,
        (Option<NeedWord>, Option<WaitWord>),
    );
    let table: Vec<Row> = vec![
        ("nothing", facts(), (None, None)),
        (
            "a busy agent",
            NeedFacts {
                agent: agent("busy"),
                ..facts()
            },
            (None, None),
        ),
        (
            "supervised box",
            NeedFacts {
                agent: boxed("bash:not-read-only", "rm -rf build"),
                supervised: true,
                ..facts()
            },
            (None, None),
        ),
        (
            "unsupervised box",
            NeedFacts {
                agent: boxed("bash:not-read-only", "rm -rf build"),
                ..facts()
            },
            (Some(Approval), None),
        ),
        (
            "held and supervised box: a hold voids supervision",
            NeedFacts {
                agent: boxed("bash:not-read-only", "rm -rf build"),
                supervised: true,
                hold: Some(HoldWord::Local),
                ..facts()
            },
            (Some(Approval), None),
        ),
        (
            "unsupervised question",
            NeedFacts {
                agent: agent("question"),
                ..facts()
            },
            (Some(Question), None),
        ),
        (
            "the question tool's box is a question",
            NeedFacts {
                agent: boxed("question", "which branch?"),
                ..facts()
            },
            (Some(Question), None),
        ),
        (
            "supervised question",
            NeedFacts {
                agent: agent("question"),
                supervised: true,
                ..facts()
            },
            (None, None),
        ),
        (
            "kind=ask",
            NeedFacts {
                attention: vec![entry("supervisor", "claude bash: rm (outside)", Some(Ask))],
                ..facts()
            },
            (Some(NeedWord::Ask), None),
        ),
        (
            "kind=wait: the writer's words, no limit named",
            NeedFacts {
                attention: vec![entry("supervisor", "limited: resets 7pm", Some(Wait))],
                ..facts()
            },
            (None, Some(Entry)),
        ),
        (
            "a person's bare entry",
            NeedFacts {
                attention: vec![entry("-", "check the deploy", Some(Ask))],
                ..facts()
            },
            (Some(NeedWord::Ask), None),
        ),
        (
            "legacy prefix with the harness answering the wall",
            NeedFacts {
                attention: vec![entry("supervisor", "limited: resets 7pm", None)],
                ..usage(None, WallAnswer::Harness)
            },
            (None, Some(Limit)),
        ),
        (
            "legacy prefix with nobody answering the wall",
            NeedFacts {
                attention: vec![entry("supervisor", "limited: resets 7pm", None)],
                ..usage(Some(NOW + 600), WallAnswer::Nobody)
            },
            (Some(NeedWord::Ask), Some(Limit)),
        ),
        (
            "legacy prefix an older server stored behind kind=",
            NeedFacts {
                attention: vec![entry("supervisor", "kind=wait limited: resets 7pm", None)],
                ..usage(None, WallAnswer::Harness)
            },
            (None, Some(Limit)),
        ),
        (
            "the upgrade's entry is told elsewhere",
            NeedFacts {
                attention: vec![entry("upgrade", "the upgrade waits", None)],
                ..facts()
            },
            (None, None),
        ),
        (
            "the server's stall entry is the stall's",
            NeedFacts {
                attention: vec![entry("aterm", "claude is frozen", None)],
                ..facts()
            },
            (None, None),
        ),
        (
            "frozen outranks everything",
            NeedFacts {
                stall: stall(false),
                task: stalled("held"),
                attention: vec![entry("supervisor", "a box", Some(Ask))],
                agent: agent("wall:auth"),
                ..facts()
            },
            (Some(Frozen), None),
        ),
        (
            "stopped",
            NeedFacts {
                stall: stall(true),
                ..facts()
            },
            (Some(Stopped), None),
        ),
        (
            "stopped, on an identified agent: the stall's word, not frozen",
            NeedFacts {
                stall: stall(true),
                agent: unresponsive(true, 8),
                supervised: true,
                attention: vec![entry("supervisor", "answer this box", None)],
                ..facts()
            },
            (Some(Stopped), None),
        ),
        (
            "frozen, on an identified agent",
            NeedFacts {
                stall: stall(false),
                agent: unresponsive(false, 8),
                ..facts()
            },
            (Some(Frozen), None),
        ),
        (
            "the verdict's own stall word",
            NeedFacts {
                agent: agent("wall:unresponsive"),
                ..facts()
            },
            (Some(Frozen), None),
        ),
        (
            "the verdict's own stopped word",
            NeedFacts {
                agent: unresponsive(true, 8),
                ..facts()
            },
            (Some(Stopped), None),
        ),
        (
            "stalled outranks ask",
            NeedFacts {
                task: stalled("startup"),
                attention: vec![entry("-", "lunch", Some(Ask))],
                ..facts()
            },
            (Some(Stalled), None),
        ),
        (
            "a working task is no need",
            NeedFacts {
                task: Some(TaskFact {
                    stalled: false,
                    why: Some("moved"),
                    entered: 2,
                }),
                ..facts()
            },
            (None, None),
        ),
        (
            "logged out, carried by this host's harness",
            NeedFacts {
                agent: agent("wall:auth"),
                supervised: true,
                wall_answer: WallAnswer::Harness,
                ..facts()
            },
            (None, Some(WaitWord::Login)),
        ),
        (
            "logged out, nobody answers (the negative control)",
            NeedFacts {
                agent: agent("wall:auth"),
                supervised: true,
                ..facts()
            },
            (Some(Login), None),
        ),
        (
            "logged out under a hold: the person's",
            NeedFacts {
                agent: agent("wall:auth"),
                supervised: true,
                wall_answer: WallAnswer::Harness,
                hold: Some(HoldWord::Local),
                ..facts()
            },
            (Some(Login), None),
        ),
        (
            "memory the host restarts",
            NeedFacts {
                agent: agent("wall:memory"),
                restarts: true,
                ..facts()
            },
            (None, Some(MemoryRestart)),
        ),
        (
            "memory nobody restarts (the negative control)",
            NeedFacts {
                agent: agent("wall:memory"),
                ..facts()
            },
            (Some(Memory), None),
        ),
        (
            "API unreachable, unsupervised",
            NeedFacts {
                agent: Some(AgentFact {
                    subject: Some(crate::presence::API_UNREACHABLE.to_string()),
                    ..agent("wall:api-error").unwrap()
                }),
                ..facts()
            },
            (None, Some(Api)),
        ),
        (
            "another API error nobody answers",
            NeedFacts {
                agent: agent("wall:api-error"),
                ..facts()
            },
            (Some(Wall), None),
        ),
        (
            "an API error the harness retries",
            NeedFacts {
                agent: agent("wall:api-error"),
                wall_answer: WallAnswer::Harness,
                ..facts()
            },
            (None, None),
        ),
        (
            "overloaded, retried",
            NeedFacts {
                agent: agent("wall:overloaded"),
                wall_answer: WallAnswer::Harness,
                ..facts()
            },
            (None, Some(Overloaded)),
        ),
        (
            "overloaded, nobody retries",
            NeedFacts {
                agent: agent("wall:overloaded"),
                ..facts()
            },
            (Some(Wall), None),
        ),
        (
            "context full, unsupervised",
            NeedFacts {
                agent: agent("wall:context"),
                ..facts()
            },
            (Some(Wall), None),
        ),
        (
            "overloaded under another supervisor's claim",
            NeedFacts {
                agent: agent("wall:overloaded"),
                supervised: true,
                other_supervisor: true,
                claim_live: true,
                ..facts()
            },
            (None, Some(Overloaded)),
        ),
        (
            "an API error under another supervisor's claim",
            NeedFacts {
                agent: agent("wall:api-error"),
                supervised: true,
                other_supervisor: true,
                claim_live: true,
                ..facts()
            },
            (None, None),
        ),
        (
            "context full under another supervisor's claim",
            NeedFacts {
                agent: agent("wall:context"),
                supervised: true,
                other_supervisor: true,
                claim_live: true,
                ..facts()
            },
            (None, None),
        ),
        (
            "context full under another supervisor's claim, held",
            NeedFacts {
                agent: agent("wall:context"),
                supervised: true,
                other_supervisor: true,
                claim_live: true,
                hold: Some(HoldWord::Bridge),
                ..facts()
            },
            (Some(Wall), None),
        ),
        (
            "a limit past its reset under another supervisor's claim, no wait said",
            NeedFacts {
                supervised: true,
                other_supervisor: true,
                claim_live: true,
                ..usage(Some(NOW - 1), WallAnswer::Nobody)
            },
            (Some(Wall), None),
        ),
        (
            "context full where this host supervises and does not compact",
            NeedFacts {
                agent: agent("wall:context"),
                supervised: true,
                ..facts()
            },
            (Some(Wall), None),
        ),
        (
            "context full, compacted by the harness",
            NeedFacts {
                agent: agent("wall:context"),
                wall_answer: WallAnswer::Harness,
                ..facts()
            },
            (None, None),
        ),
        (
            "context full the harness compacts, held",
            NeedFacts {
                agent: agent("wall:context"),
                supervised: true,
                wall_answer: WallAnswer::Harness,
                hold: Some(HoldWord::Local),
                ..facts()
            },
            (Some(Wall), None),
        ),
        (
            "an API error the harness retries, held",
            NeedFacts {
                agent: agent("wall:api-error"),
                supervised: true,
                wall_answer: WallAnswer::Harness,
                hold: Some(HoldWord::Bridge),
                ..facts()
            },
            (Some(Wall), None),
        ),
        (
            "overloaded, retried by the harness, held",
            NeedFacts {
                agent: agent("wall:overloaded"),
                supervised: true,
                wall_answer: WallAnswer::Harness,
                hold: Some(HoldWord::Local),
                ..facts()
            },
            (Some(Wall), None),
        ),
        (
            "a usage limit the harness answers, held: the answer outlives the hold",
            NeedFacts {
                supervised: true,
                hold: Some(HoldWord::Local),
                ..usage(Some(NOW - 1), WallAnswer::Harness)
            },
            (None, Some(Limit)),
        ),
        (
            "a spend limit with no reset, unsupervised",
            NeedFacts {
                agent: agent("wall:spend"),
                ..facts()
            },
            (Some(Wall), None),
        ),
        (
            "a spend limit the harness asks to lift",
            NeedFacts {
                agent: agent("wall:spend"),
                wall_answer: WallAnswer::HarnessAsks("add credits"),
                attention: vec![entry("supervisor", "limited: add funds", Some(Ask))],
                ..facts()
            },
            (Some(NeedWord::Ask), Some(Limit)),
        ),
        (
            "a usage limit the harness answers",
            usage(None, WallAnswer::Harness),
            (None, Some(Limit)),
        ),
        (
            "an unsupervised usage limit before its reset",
            usage(Some(NOW + 60), WallAnswer::Nobody),
            (None, Some(Limit)),
        ),
        (
            "an unsupervised usage limit once its reset has passed",
            usage(Some(NOW - 1), WallAnswer::Nobody),
            (Some(Wall), None),
        ),
        (
            "an unsupervised usage limit naming no reset this machine can place",
            usage(None, WallAnswer::Nobody),
            (Some(Wall), None),
        ),
        (
            "past its reset, another supervisor's kind=wait settles it",
            NeedFacts {
                attention: vec![entry("supervisor", "limited: resets 7pm", Some(Wait))],
                supervised: true,
                other_supervisor: true,
                claim_live: true,
                ..usage(Some(NOW - 1), WallAnswer::Nobody)
            },
            (None, Some(Limit)),
        ),
        (
            "past its reset, a script's kind=wait under a live claim settles nothing",
            NeedFacts {
                attention: vec![entry("ci", "building", Some(Wait))],
                supervised: true,
                other_supervisor: true,
                claim_live: true,
                ..usage(Some(NOW - 1), WallAnswer::Nobody)
            },
            (Some(Wall), None),
        ),
        (
            "past its reset, a kind=wait whose writer's claim lapsed",
            NeedFacts {
                attention: vec![entry("supervisor", "limited: resets 7pm", Some(Wait))],
                supervised: true,
                ..usage(Some(NOW - 1), WallAnswer::Nobody)
            },
            (Some(Wall), None),
        ),
        (
            "before its reset, a kind=wait whose writer's claim lapsed",
            NeedFacts {
                attention: vec![entry("supervisor", "limited: resets 7pm", Some(Wait))],
                ..usage(Some(NOW + 60), WallAnswer::Nobody)
            },
            (None, Some(Limit)),
        ),
    ];
    for (case, facts, want) in &table {
        let verdict = of(facts);
        assert_eq!(words(&verdict), *want, "{case}");
        if let Some(need) = &verdict.need {
            assert!(!need.what.is_empty() && !need.what.contains('\n'), "{case}");
        }
    }
    let reached: std::collections::BTreeSet<NeedWord> = table
        .iter()
        .filter_map(|(_, facts, _)| of(facts).need.map(|n| n.word))
        .collect();
    assert_eq!(reached, NeedWord::ALL.into_iter().collect());
    let waits: std::collections::BTreeSet<WaitWord> = table
        .iter()
        .filter_map(|(_, facts, _)| of(facts).waiting.map(|w| w.word))
        .collect();
    assert_eq!(waits, WaitWord::ALL.into_iter().collect());
}

/// An unsupervised limit before its reset arms the publisher's wake AT the
/// reset (the injected clock), and the same facts read past it are a `wall`.
#[test]
fn an_unsupervised_limit_rechecks_at_its_reset() {
    let at = NOW + 90;
    let mut f = NeedFacts {
        agent: agent("wall:usage-weekly"),
        reset_unix: Some(at),
        ..facts()
    };
    let before = of(&f);
    assert_eq!(before.recheck_unix, Some(at));
    assert_eq!(words(&before), (None, Some(WaitWord::Limit)));
    f.now_unix = at;
    let after = of(&f);
    assert_eq!(after.recheck_unix, None);
    assert_eq!(words(&after), (Some(NeedWord::Wall), None));
    f.now_unix = NOW;
    f.attention = vec![entry(
        "supervisor",
        "limited: resets",
        Some(AttentionKind::Wait),
    )];
    assert_eq!(of(&f).recheck_unix, Some(at));
    f.supervised = true;
    f.claim_live = true;
    assert_eq!(of(&f).recheck_unix, None);
}

/// The keys: one stall episode, one task entry, one agent rev, one text — so
/// a re-read of the same need is the same transition and a new one is new.
#[test]
fn a_needs_key_is_its_transitions_identity() {
    let frozen = of(&NeedFacts {
        stall: stall(false),
        ..facts()
    })
    .need
    .unwrap();
    assert_eq!((frozen.word, frozen.key), (NeedWord::Frozen, 0xfeed));
    for (stopped, rev, word) in [(false, 8, NeedWord::Frozen), (true, 9, NeedWord::Stopped)] {
        let on_agent = of(&NeedFacts {
            stall: stall(stopped),
            agent: unresponsive(stopped, rev),
            ..facts()
        })
        .need
        .unwrap();
        assert_eq!((on_agent.word, on_agent.key), (word, 0xfeed), "{word:?}");
        assert_eq!(
            on_agent.what, "claude is frozen: not reading input since 14:02",
            "{word:?}"
        );
        assert!(stall(stopped).unwrap().text.ends_with(STALL_REMEDY));
    }
    let alone = of(&NeedFacts {
        agent: unresponsive(true, 9),
        ..facts()
    })
    .need
    .unwrap();
    assert_eq!((alone.word, alone.key), (NeedWord::Stopped, 9));
    let task = of(&NeedFacts {
        task: stalled("held"),
        ..facts()
    })
    .need
    .unwrap();
    assert_eq!((task.word, task.key), (NeedWord::Stalled, 4));
    let boxed = of(&NeedFacts {
        agent: boxed("bash", "ls"),
        ..facts()
    })
    .need
    .unwrap();
    assert_eq!(boxed.key, 9);
    let ask = |text: &str| {
        of(&NeedFacts {
            attention: vec![entry("-", text, Some(AttentionKind::Ask))],
            ..facts()
        })
        .need
        .unwrap()
        .key
    };
    assert_eq!(ask("a"), ask("a"));
    assert_ne!(ask("a"), ask("b"));
}

/// The severity the word carries: the stuck words are stuck, the asks decide.
#[test]
fn the_words_carry_their_severity() {
    let sev = |f: NeedFacts| of(&f).need.map(|n| n.word.severity());
    assert_eq!(
        sev(NeedFacts {
            stall: stall(false),
            ..facts()
        }),
        Some(Severity::Stuck)
    );
    assert_eq!(
        sev(NeedFacts {
            agent: agent("question"),
            ..facts()
        }),
        Some(Severity::Decide)
    );
}

/// The ask that shows is the most recent ask; a wait beside it does not hide
/// it, and a hold is the owner's own act, never a need.
#[test]
fn the_most_recent_ask_shows_and_a_hold_is_no_need() {
    let v = of(&NeedFacts {
        attention: vec![
            entry("human", "check the deploy", Some(AttentionKind::Ask)),
            entry(
                "supervisor",
                "limited: resets 7pm",
                Some(AttentionKind::Wait),
            ),
            entry("ci", "older", Some(AttentionKind::Ask)),
        ],
        ..facts()
    });
    assert_eq!(
        v.need.as_ref().map(|n| n.what.as_str()),
        Some("check the deploy")
    );
    assert_eq!(v.waiting.map(|w| w.word), Some(WaitWord::Entry));
    let held = of(&NeedFacts {
        hold: Some(HoldWord::Bridge),
        ..facts()
    });
    assert_eq!((held.need, held.held), (None, Some(HoldWord::Bridge)));
    assert_eq!(held_words(), ("-", "bridge"));
}

fn held_words() -> (&'static str, &'static str) {
    let v = of(&NeedFacts {
        hold: Some(HoldWord::Bridge),
        ..facts()
    });
    (v.needs_word(), v.hold_word())
}

/// A deterministic stream of facts for the agreement property: every corner
/// of the inputs the classifier reads, drawn by a seeded xorshift so a
/// failure names its case and reruns it.
struct Gen(u64);

impl Gen {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn pick<T: Clone>(&mut self, of: &[T]) -> T {
        let at = usize::try_from(self.next() % of.len() as u64).unwrap_or(0);
        of[at].clone()
    }

    /// One session's facts.
    fn facts(&mut self, case: u64) -> NeedFacts {
        let words: [&'static str; 14] = [
            "busy",
            "idle",
            "prompt",
            "question",
            "wall:usage-session",
            "wall:usage-weekly",
            "wall:spend",
            "wall:auth",
            "wall:memory",
            "wall:api-error",
            "wall:overloaded",
            "wall:context",
            "wall:unresponsive",
            "survey",
        ];
        let agent = self.pick(&[None, Some(())]).map(|()| AgentFact {
            word: self.pick(&words),
            detail: self.pick(&[
                None,
                Some("bash:not-read-only".to_string()),
                Some("7:30pm".to_string()),
            ]),
            rev: self.next() % 50,
            subject: self.pick(&[
                None,
                Some("rm -rf build".to_string()),
                Some(crate::presence::API_UNREACHABLE.to_string()),
            ]),
        });
        let entries = self.next() % 3;
        let attention = (0..entries)
            .map(|n| AttentionFact {
                owner: self
                    .pick(&["-", "supervisor", "human", "ci", "aterm", "upgrade"])
                    .to_string(),
                text: format!(
                    "{}secret{case}x{n}",
                    self.pick(&["", "limited: ", "kind=wait limited: "])
                ),
                kind: self.pick(&[None, Some(AttentionKind::Ask), Some(AttentionKind::Wait)]),
            })
            .collect();
        let supervised = self.next().is_multiple_of(2);
        let other_supervisor = supervised && self.next().is_multiple_of(2);
        let stall = self.pick(&[None, None, None, stall(false), stall(true)]);
        let agent = match (agent, &stall) {
            (Some(agent), Some(stall)) => Some(AgentFact {
                word: "wall:unresponsive",
                detail: stall.stopped.then(|| "stopped".to_string()),
                ..agent
            }),
            (agent, _) => agent,
        };
        NeedFacts {
            stall,
            task: self.pick(&[
                None,
                None,
                stalled("startup"),
                stalled("held"),
                Some(TaskFact {
                    stalled: false,
                    why: None,
                    entered: 1,
                }),
            ]),
            attention,
            agent,
            supervised,
            other_supervisor,
            claim_live: other_supervisor || self.next().is_multiple_of(2),
            wall_answer: self.pick(&[
                WallAnswer::Nobody,
                WallAnswer::Harness,
                WallAnswer::HarnessAsks("add credits"),
            ]),
            restarts: self.next().is_multiple_of(2),
            resume: None,
            hold: self.pick(&[None, None, Some(HoldWord::Local), Some(HoldWord::Bridge)]),
            reset_unix: self.pick(&[None, Some(NOW - 60), Some(NOW + 60)]),
            now_unix: NOW,
            told: crate::needs::ToldNeeds::default(),
        }
    }
}

/// THE AGREEMENT PROPERTY (`docs/DESIGN-owner-surfaces-2026-10-16.md` §10):
/// for generated facts, [`of`]'s verdict, once published on the session's
/// timeline, is what every reader says — `status needs=` and `hold_by=`, the
/// survey's facts row as `aterm ctl survey` ranks it (stuck is stalled,
/// decide is asking, no need is neither), and the menu-bar glance's badge
/// and row, the window's presence border and the tab's mark (folded from
/// the facts the presence driver senses, projected by the App's own driver,
/// with nothing published again) — and the facts row carries no byte of any
/// attention text.
#[test]
fn every_reader_says_the_published_verdict() {
    use aterm_messages::presence::drive::{self, Desk};
    let mut app = crate::App::headless_for_test();
    let ctx = app.pool.get(0).expect("session 0").ctx.clone();
    let field = |line: &str, key: &str| {
        line.split(' ')
            .find_map(|kv| kv.strip_prefix(&format!("{key}=")))
            .map(str::to_string)
    };
    let mut generator = Gen(0x9e37_79b9_7f4a_7c15);
    let mut seen = std::collections::BTreeSet::new();
    for case in 0..600 {
        let facts = generator.facts(case);
        let verdict = of(&facts);
        seen.insert(verdict.needs_word());
        if let Some(stall) = &facts.stall {
            let need = verdict.need.as_ref().expect("a stall is a need");
            let word = if stall.stopped {
                NeedWord::Stopped
            } else {
                NeedWord::Frozen
            };
            assert_eq!((need.word, need.key), (word, stall.key), "case {case}");
        }
        ctx.timeline.lock().unwrap().set_need(verdict.clone());
        let status = app.session_status_record(0).expect("a status record");
        assert_eq!(
            field(&status, "needs").as_deref(),
            Some(verdict.needs_word()),
            "case {case}: {facts:?}"
        );
        assert_eq!(
            field(&status, "hold_by").as_deref(),
            Some(verdict.hold_word()),
            "case {case}"
        );
        let here = crate::survey::Here {
            host: None,
            instances: 1,
            drive: false,
        };
        let survey = crate::survey::survey_rows(
            &app.store,
            crate::survey::SurveyLevel::Facts,
            usize::MAX,
            &here,
        );
        let row = survey
            .lines()
            .find(|line| line.starts_with("session "))
            .expect("a session row");
        assert_eq!(
            field(row, "needs").as_deref(),
            Some(verdict.needs_word()),
            "case {case}"
        );
        let decoded: Vec<(String, String)> = row
            .split(' ')
            .skip(1)
            .filter_map(|kv| kv.split_once('='))
            .map(|(k, v)| (k.to_string(), aterm_control::wire::pct_decode(v)))
            .collect();
        let rank = aterm_ctl::survey_rank(decoded.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        let want = match verdict.need.as_ref().map(|need| need.word.severity()) {
            Some(Severity::Stuck) => Some(1),
            Some(Severity::Decide) => Some(0),
            None => None,
        };
        match want {
            Some(rank_of_need) => assert_eq!(rank, rank_of_need, "case {case}: {row}"),
            None => assert!(
                rank > 1,
                "case {case}: no need ranks below asking and stalled: {row}"
            ),
        }
        assert!(
            !row.contains(&format!("secret{case}x")),
            "case {case}: {row}"
        );
        let glance = app.status_glance();
        assert_eq!(
            glance.need_count(),
            usize::from(verdict.need.is_some()),
            "case {case}"
        );
        assert_eq!(
            glance.button_title() == "\u{276f}1",
            verdict.need.is_some(),
            "case {case}"
        );
        let sensed = Desk::sense(&app, 0).expect("session 0 is pooled");
        Desk::table_mut(&mut app).absorb(0, sensed, std::time::Instant::now());
        drive::refresh_window(&mut app, crate::WindowId(0));
        let severity = verdict
            .need
            .as_ref()
            .map(|need| match need.word.severity() {
                Severity::Stuck => crate::presence::NeedSeverity::Stuck,
                Severity::Decide => crate::presence::NeedSeverity::Decide,
            });
        let rim = app
            .presence_view(crate::WindowId(0))
            .map(|view| view.rim)
            .expect("window 0");
        assert_eq!(
            rim,
            severity.map_or(crate::presence::Rim::None, crate::presence::Rim::of),
            "case {case}: {facts:?}"
        );
        let chip = app.presence.slot(0).map(|slot| slot.chip());
        let agrees = matches!(
            (severity, chip),
            (
                Some(crate::presence::NeedSeverity::Stuck),
                Some(crate::presence::ChipLevel::Stop(_))
            ) | (
                Some(crate::presence::NeedSeverity::Decide),
                Some(crate::presence::ChipLevel::Wait)
            ) | (None, Some(crate::presence::ChipLevel::Off))
        );
        assert!(agrees, "case {case}: {severity:?} marked {chip:?}");
    }
    let mut every: std::collections::BTreeSet<&str> =
        NeedWord::ALL.iter().map(|need| need.word()).collect();
    every.insert("-");
    assert_eq!(seen, every);
}

/// AN OUTAGE IS ONE ENTRY, NOT A ROW PER TAB (§2.2, scenario 6): two tabs
/// whose agent cannot reach its API raise no need, no menu row and no
/// notice, and the digest holds ONE `ApiOutage` entry for both; it is stated
/// again as tabs leave the wait, and the last one leaving names nothing.
#[test]
fn an_api_outage_is_one_digest_entry_and_no_need() {
    let mut app = crate::App::headless_for_test();
    app.push_stub_tab(crate::WindowId(0), crate::stub_session(app.next_session_id));
    let at_wall = |app: &crate::App, id: u64, subject: Option<&str>| {
        let ctx = &app.pool.get(id).unwrap().ctx;
        let mut timeline = ctx.timeline.lock().unwrap();
        timeline.publish_agent(
            "wall:api-error",
            None,
            subject.map(str::to_string),
            Some(aterm_phase::Program::Claude),
            crate::session_timeline::AgentStamp {
                generation: crate::control::ScreenGen {
                    epoch: 1,
                    seq: id + 1,
                },
                fp: id + 1,
            },
        );
    };
    let outage_titles = |app: &crate::App| -> Vec<String> {
        app.messages
            .digest()
            .entries
            .iter()
            .filter(|e| {
                e.key
                    .starts_with(&aterm_messages::exceptions::Family::ApiOutage.prefix())
            })
            .map(|e| e.title.clone())
            .collect()
    };
    let counting = |tabs: usize| {
        crate::message_reporters::api_outage_entry(tabs, None)
            .message()
            .expect("the outage entry's words")
            .title
    };
    for id in 0..2 {
        at_wall(&app, id, Some(crate::presence::API_UNREACHABLE));
        app.refresh_presence_session(id);
    }
    assert_eq!(
        outage_titles(&app),
        vec![counting(2)],
        "one entry, both tabs"
    );
    assert_ne!(counting(2), counting(1), "PRECONDITION: the words count");
    assert_eq!(app.api_outage.len(), 2);
    assert!(app.status_glance().needs.is_empty(), "no row per tab");
    for id in 0..2 {
        let verdict = app
            .pool
            .get(id)
            .unwrap()
            .ctx
            .timeline
            .lock()
            .unwrap()
            .need()
            .clone();
        assert_eq!(verdict.waiting.map(|w| w.word), Some(WaitWord::Api));
        assert_eq!(verdict.need, None);
    }
    at_wall(&app, 1, None);
    app.refresh_presence_session(1);
    assert_eq!(app.api_outage.len(), 1);
    assert_eq!(outage_titles(&app), vec![counting(1)], "tab 0 alone");
    assert_eq!(app.status_glance().need_count(), 1);
    at_wall(&app, 0, None);
    app.refresh_presence_session(0);
    assert!(app.api_outage.is_empty(), "the last tab left the wait");
}

/// The memory remedy names the tab's OWN conversation whole — the line with
/// its flags, else a shorter form, else the restart alone — and never the
/// banner's `claude --continue` (2026-09-26): a cut id names no conversation.
#[test]
fn the_memory_remedy_names_the_tabs_own_resume_line_whole() {
    let id = "5f1c2d3e-4b5a-4c6d-8e7f-0a1b2c3d4e5f";
    let own = format!("claude --model opus --resume {id}");
    let remedy = memory_remedy(Some(&own));
    assert!(remedy.ends_with(&own), "{remedy}");
    let long = format!(
        "claude {}--resume {id}",
        "--add-dir /some/long/path ".repeat(12)
    );
    let remedy = memory_remedy(Some(&long));
    assert!(remedy.len() <= REMEDY_TEXT_MAX, "{remedy}");
    assert!(
        remedy.ends_with(id) || !remedy.contains("--resume"),
        "{remedy}"
    );
    for remedy in [
        memory_remedy(Some(&own)),
        memory_remedy(Some(&long)),
        memory_remedy(None),
    ] {
        assert!(!remedy.contains("--continue"), "{remedy}");
    }
    assert!(!memory_remedy(None).contains("--resume"));
}

/// A STALLED TASK IS A NEED ONCE ITS WAKE LANDS (§9 S3): the mark posts
/// `Wake::TaskChanged`, its arm publishes the verdict, and `status` says
/// `needs=stalled`; the worker moving again clears it the same way.
#[test]
fn a_stalled_task_is_a_need_once_its_wake_lands() {
    use crate::task_record::{TaskMark, TaskState, mark_session_task};
    let mut app = crate::App::headless_for_test();
    let ctx = app.pool.get(0).expect("session 0").ctx.clone();
    let needs = |app: &crate::App| {
        let status = app.session_status_record(0).expect("a status record");
        status
            .split(' ')
            .find_map(|kv| kv.strip_prefix("needs="))
            .map(str::to_string)
    };
    let _ = crate::presence::take_task_wakes();
    let stalled = TaskMark {
        why: Some("held"),
        ..TaskMark::new(TaskState::Stalled)
    };
    mark_session_task(&ctx, 0, &stalled).expect("unfenced");
    assert_eq!(needs(&app).as_deref(), Some("-"), "not before its wake");
    let wakes = crate::presence::take_task_wakes();
    assert_eq!(wakes, [0]);
    for session in wakes {
        app.on_task_changed(session);
    }
    assert_eq!(needs(&app).as_deref(), Some("stalled"));
    let moved = TaskMark {
        why: Some("moved"),
        ..TaskMark::new(TaskState::Working)
    };
    mark_session_task(&ctx, 0, &moved).expect("unfenced");
    for session in crate::presence::take_task_wakes() {
        app.on_task_changed(session);
    }
    assert_eq!(needs(&app).as_deref(), Some("-"));
}

/// THE COALESCED GLANCE (§9 S3): thirty-two tabs whose need moves in one
/// turn owe ONE rebuild of the menu-bar item's glance, taken once on the
/// loop's way to its wait.
#[test]
fn a_turn_of_many_changes_owes_one_glance_rebuild() {
    let mut app = crate::App::headless_for_test();
    for _ in 1..32 {
        let id = app.next_session_id;
        app.push_stub_tab(crate::WindowId(0), crate::stub_session(id));
    }
    let sessions: Vec<u64> = app.pool.iter().map(|session| session.id).collect();
    assert_eq!(sessions.len(), 32);
    let _ = app.take_status_rebuild();
    assert!(!app.take_status_rebuild(), "a quiet turn owes nothing");
    for session in &sessions {
        let ctx = app.pool.get(*session).unwrap().ctx.clone();
        ctx.meta
            .lock()
            .unwrap()
            .set("attention", Some(format!("look at {session}")));
        app.on_meta_changed(*session);
    }
    assert_eq!(app.status_glance().need_count(), 32);
    assert!(app.take_status_rebuild(), "the turn owes a rebuild");
    assert!(!app.take_status_rebuild(), "and only one");
}

/// Put session `id`'s agent at `word` (a wall) with `detail` and `subject`,
/// a new reading each call.
fn publish_wall(
    app: &crate::App,
    id: u64,
    word: &'static str,
    detail: Option<&str>,
    subject: Option<&str>,
) {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let ctx = &app.pool.get(id).unwrap().ctx;
    ctx.timeline.lock().unwrap().publish_agent(
        word,
        detail.map(str::to_string),
        subject.map(str::to_string),
        Some(aterm_phase::Program::Claude),
        crate::session_timeline::AgentStamp {
            generation: crate::control::ScreenGen { epoch: 1, seq },
            fp: seq,
        },
    );
}

/// How many `ApiOutage` entries the digest holds.
fn outage_entries(app: &crate::App) -> usize {
    let prefix = aterm_messages::exceptions::Family::ApiOutage.prefix();
    app.messages
        .digest()
        .entries
        .iter()
        .filter(|e| e.key.starts_with(&prefix))
        .count()
}

/// The digest's own clock run past every pending end.
fn settle_digest(app: &mut crate::App) {
    if let Some(due) = app.messages_deadline() {
        let _ = app.settle_messages(due + std::time::Duration::from_secs(1));
    }
}

/// `status needs=` of session `id`.
fn needs_of(app: &crate::App, id: u64) -> Option<String> {
    app.session_status_record(id)
        .ok()?
        .split(' ')
        .find_map(|kv| kv.strip_prefix("needs="))
        .map(str::to_string)
}

/// A TAB CLOSED IN AN OUTAGE LEAVES IT (scenario 6): no
/// publication runs for a closed tab again, so its close is what takes it out
/// of the one `ApiOutage` entry's count and drops its need's recheck — the
/// entry counts the tab left, and goes once the last tab in the outage
/// closes.
#[test]
fn a_tab_closed_in_an_outage_leaves_its_count() {
    let mut app = crate::App::headless_for_test();
    for _ in 0..3 {
        let id = app.next_session_id;
        app.push_stub_tab(crate::WindowId(0), crate::stub_session(id));
    }
    for id in [1, 2] {
        publish_wall(
            &app,
            id,
            "wall:api-error",
            None,
            Some(crate::presence::API_UNREACHABLE),
        );
        app.refresh_presence_session(id);
    }
    publish_wall(&app, 3, "wall:usage-session", Some("in 3h"), None);
    app.refresh_presence_session(3);
    assert_eq!(app.api_outage.len(), 2);
    assert!(app.need_rechecks.contains_key(&3));
    assert_eq!(outage_entries(&app), 1);

    assert!(app.close_session_by_id(1).is_ok());
    assert!(app.pool.get(1).is_none());
    assert_eq!(
        app.api_outage.keys().copied().collect::<Vec<_>>(),
        [2],
        "the closed tab left the count"
    );
    settle_digest(&mut app);
    assert_eq!(outage_entries(&app), 1, "tab 2 still waits on the API");
    assert!(app.close_session_by_id(3).is_ok());
    assert!(
        app.need_rechecks.is_empty(),
        "the closed tab's recheck went"
    );
    assert!(app.close_session_by_id(2).is_ok());
    assert!(app.api_outage.is_empty());
    settle_digest(&mut app);
    assert_eq!(outage_entries(&app), 0, "the outage is over here");
}

/// AN OUTAGE ENTRY AN EARLIER PROCESS LEFT OPEN ENDS (scenario 6):
/// the family is told only as tabs enter and leave the wait, so
/// this process states its whole answer at its first look whether or not a
/// tab moved, and the entry goes through the digest's clock when no tab here
/// waits on the API.
#[test]
fn an_outage_entry_an_earlier_process_left_ends_when_no_tab_waits() {
    let earlier = || crate::message_reporters::api_outage_entry(3, Some("14:02"));
    let mut app = crate::App::headless_for_test();
    app.except_replace(aterm_messages::exceptions::Family::ApiOutage, &[earlier()]);
    assert_eq!(outage_entries(&app), 1, "the earlier process's entry");
    publish_wall(&app, 0, "busy", None, None);
    app.refresh_presence_session(0);
    app.ripen_exceptions(std::time::Instant::now());
    settle_digest(&mut app);
    assert_eq!(outage_entries(&app), 0, "swept: no tab here waits");

    let mut app = crate::App::headless_for_test();
    app.except_replace(aterm_messages::exceptions::Family::ApiOutage, &[earlier()]);
    publish_wall(
        &app,
        0,
        "wall:api-error",
        None,
        Some(crate::presence::API_UNREACHABLE),
    );
    app.refresh_presence_session(0);
    app.ripen_exceptions(std::time::Instant::now());
    settle_digest(&mut app);
    assert_eq!(outage_entries(&app), 1, "a tab here still waits");
}

/// A RESET SLEPT THROUGH IS DUE AT THE FIRST TURN AFTER THE WAKE (§2.2):
/// an unsupervised limit's recheck is kept on the wall
/// clock, which a Mac's sleep advances and `Instant` does not, so a wall
/// clock past the reset with the presence clock not moved at all finds the
/// recheck due now, and the tick publishes the `wall` need.
#[test]
fn a_reset_slept_through_is_due_at_the_first_turn_after_the_wake() {
    use crate::app_presence::{set_need_unix_clock_for_test, set_presence_clock_for_test};
    struct Clocks;
    impl Drop for Clocks {
        fn drop(&mut self) {
            set_need_unix_clock_for_test(None);
            set_presence_clock_for_test(None);
        }
    }
    let _clocks = Clocks;
    let reset = "Sep 19 at 11am";
    let at = crate::presence::place_reset(reset, NOW).expect("a placeable reset");
    assert_eq!(crate::presence::place_reset(reset, at + 1), Some(at));
    let instant = std::time::Instant::now();
    set_presence_clock_for_test(Some(instant));
    set_need_unix_clock_for_test(Some(at - 3600));
    let mut app = crate::App::headless_for_test();
    publish_wall(&app, 0, "wall:usage-weekly", Some(reset), None);
    app.refresh_presence_session(0);
    assert_eq!(
        needs_of(&app, 0).as_deref(),
        Some("-"),
        "waiting for its reset"
    );
    let deadline = app
        .presence_deadline(instant)
        .expect("the recheck is armed");
    assert!(deadline >= instant + std::time::Duration::from_secs(3599));
    let _ = app.presence_tick(instant);
    assert_eq!(needs_of(&app, 0).as_deref(), Some("-"), "not due yet");
    set_need_unix_clock_for_test(Some(at + 60));
    assert_eq!(app.presence_deadline(instant), Some(instant), "due now");
    let _ = app.presence_tick(instant);
    assert_eq!(needs_of(&app, 0).as_deref(), Some("wall"));
    assert!(app.need_rechecks.is_empty());
}

/// AN ENTRY CLEARED UNDER A NEWER ONE MOVES THE NEED:
/// the classifier reads every attention entry, so clearing an older ask under
/// a newer wait — the bare one, or a keyed one — answers a change (what the
/// wire's `meta` arm posts `Wake::MetaChanged` on), and the need it
/// republishes is none.
#[test]
fn clearing_an_older_ask_under_a_newer_wait_moves_the_need() {
    let mut app = crate::App::headless_for_test();
    let (term, ctx) = {
        let s = app.pool.get(0).unwrap();
        (s.term.clone(), s.ctx.clone())
    };
    let store = app.store.clone();
    let meta = |app: &mut crate::App, rest: &str| {
        let (reply, changed) = crate::control::cmd_meta(&term, &store, 0, &ctx, rest);
        assert!(reply.starts_with("OK"), "{rest}: {reply}");
        if changed {
            app.on_meta_changed(0);
        }
        changed
    };
    for (older, newer, clear) in [
        (
            "set attention lunch",
            "set attention owner=sup kind=wait limited: resets 8pm",
            "unset attention",
        ),
        (
            "set attention owner=ci kind=ask look at the build",
            "set attention owner=sup kind=wait limited: resets 9pm",
            "unset attention owner=ci",
        ),
    ] {
        assert!(meta(&mut app, older));
        assert!(meta(&mut app, newer));
        assert_eq!(needs_of(&app, 0).as_deref(), Some("ask"), "{older}");
        assert!(meta(&mut app, clear), "{clear}: an entry went");
        assert_eq!(needs_of(&app, 0).as_deref(), Some("-"), "{clear}");
        assert!(!meta(&mut app, clear), "{clear}: nothing left to clear");
    }
}

/// A FROZEN PROGRAM THE PILL OFFERS NO RESTART FOR SAYS WHAT TO DO: an
/// unsupervised Claude Code, a program that is no agent, and a tab under a
/// hold all keep a person's remedy — the window's own ways out and the tab's
/// resume line, whole (its shorter form where the whole does not fit), or
/// Hand Back where only a hold Hand Back lifts refuses the restart.
#[test]
fn a_frozen_program_with_no_act_keeps_a_persons_remedy() {
    let line = "claude --model opus --resume 5f1c2d3e-4b5a-4c6d-8e7f-0a1b2c3d4e5f";
    let frozen = |act: bool, hand_back_frees: bool, resume: Option<&str>| {
        of(&NeedFacts {
            stall: Some(StallFact {
                text: format!(
                    "claude is frozen: not reading input since 08:36 (2 B queued) {STALL_REMEDY}"
                ),
                key: 1,
                stopped: false,
                act,
                hand_back_frees,
            }),
            resume: resume.map(str::to_string),
            ..facts()
        })
        .need
        .unwrap()
        .what
    };
    let facts_alone = "claude is frozen: not reading input since 08:36 (2 B queued)";
    assert_eq!(
        frozen(true, false, Some(line)),
        facts_alone,
        "the pill's act"
    );
    assert_eq!(
        frozen(false, false, Some(line)),
        format!("{facts_alone} \u{2014} \u{2303}C or close the tab, then {line}")
    );
    assert_eq!(
        frozen(false, false, None),
        format!("{facts_alone} \u{2014} \u{2303}C or close the tab"),
        "a program that is no agent: no resume line"
    );
    assert_eq!(
        frozen(false, true, Some(line)),
        format!("{facts_alone} \u{2014} Hand Back to let aterm restart it")
    );
    assert!(memory_remedy(Some(line)).ends_with(line));
    let long = format!(
        "claude --permission-mode acceptEdits{} --resume 5f1c2d3e-4b5a-4c6d-8e7f-0a1b2c3d4e5f",
        " --add-dir /x".repeat(30)
    );
    let what = frozen(false, false, Some(&long));
    assert!(what.len() <= REMEDY_TEXT_MAX, "{what}");
    assert!(
        what.ends_with("--resume 5f1c2d3e-4b5a-4c6d-8e7f-0a1b2c3d4e5f"),
        "its shorter form, whole: {what}"
    );
    let stopped = of(&NeedFacts {
        stall: Some(StallFact {
            text: format!("claude is stopped with input queued since 08:36 {STALL_REMEDY}"),
            key: 2,
            stopped: true,
            act: false,
            hand_back_frees: false,
        }),
        ..facts()
    })
    .need
    .unwrap();
    assert_eq!(
        stopped.what,
        "claude is stopped with input queued since 08:36"
    );
}

/// AN UNSUPERVISED LIMIT PAST ITS RESET says it passed and what to do, never
/// `until`, which reads as a limit still waited out; before its reset it is
/// the Waiting row's, `until` intact there.
#[test]
fn a_limit_past_its_reset_says_continue_it() {
    let usage = |reset_unix: Option<i64>| NeedFacts {
        agent: Some(AgentFact {
            detail: Some("5:11pm".to_string()),
            ..agent("wall:usage-session").unwrap()
        }),
        reset_unix,
        ..facts()
    };
    let past = of(&usage(Some(NOW - 1))).need.unwrap();
    assert_eq!(
        (past.word, past.what.as_str()),
        (
            NeedWord::Wall,
            "session usage limit reset at 5:11pm \u{2014} continue it there"
        )
    );
    let before = of(&usage(Some(NOW + 60)));
    assert_eq!(words(&before), (None, Some(WaitWord::Limit)));
    let unplaced = of(&usage(None)).need.unwrap();
    assert_eq!(unplaced.what, "session usage limit until 5:11pm");
}

/// A WRITER'S `kind=wait` IS A WAIT IN ITS OWN WORDS, never a usage limit
/// by default: the entry's words and whose they are ride the Waiting, the
/// most recent entry first, and the Overview's words for it are the same
/// ([`wait_words`]).
#[test]
fn a_writers_wait_carries_its_words() {
    let v = of(&NeedFacts {
        attention: vec![
            entry("ci", "waiting for CI on PR 412", Some(AttentionKind::Wait)),
            entry("deploy", "older", Some(AttentionKind::Wait)),
        ],
        ..facts()
    });
    let wait = v.waiting.expect("a wait");
    assert_eq!(wait.word, WaitWord::Entry);
    assert_eq!(wait_words(&wait, false), "waiting for CI on PR 412 (ci)");
    let bare = of(&NeedFacts {
        attention: vec![entry("-", "kind=wait back after lunch", None)],
        ..facts()
    });
    assert_eq!(bare.waiting, None);
    let legacy = of(&NeedFacts {
        attention: vec![entry(
            "-",
            "kind=wait back after lunch",
            Some(AttentionKind::Wait),
        )],
        ..facts()
    });
    assert_eq!(
        legacy.waiting.as_ref().map(|w| wait_words(w, false)),
        Some("back after lunch".to_string())
    );
    let walled = of(&NeedFacts {
        attention: vec![entry("ci", "waiting for CI", Some(AttentionKind::Wait))],
        wall_answer: WallAnswer::Harness,
        ..NeedFacts {
            agent: Some(AgentFact {
                detail: Some("7pm".to_string()),
                ..agent("wall:usage-session").unwrap()
            }),
            ..facts()
        }
    });
    let wall = walled.waiting.expect("the wall's wait");
    assert_eq!(
        (wall.word, wait_words(&wall, false)),
        (
            WaitWord::Limit,
            "waiting out the session usage limit".to_string()
        )
    );
}

/// A WORKER'S NEEDS YOU ENTRY IS AN ASK (Autopilot Cruise / Land): the
/// loop raises `owner=needs kind=ask "<program> needs you: … — carrying on"`
/// for a report's `NEEDS YOU:` lines, and the classifier reads it as the ask
/// it is — the border, the pill, `◇`, `❯N` and the menu bar's "needs you" —
/// with no reader of its own.
#[test]
fn a_workers_needs_you_entry_is_an_ask() {
    let owner = crate::harness_host::NEEDS_ATTENTION_OWNER;
    let text = "Claude needs you: approval to publish \u{2014} carrying on";
    let v = of(&NeedFacts {
        attention: vec![entry(owner, text, Some(AttentionKind::Ask))],
        ..facts()
    });
    assert_eq!(words(&v), (Some(NeedWord::Ask), None));
    assert_eq!(
        v.need.as_ref().map(|n| (n.what.as_str(), n.key)),
        Some(("approval to publish", ask_key(text)))
    );
    let waited = of(&NeedFacts {
        attention: vec![entry(owner, text, Some(AttentionKind::Wait))],
        ..facts()
    });
    assert_eq!(
        words(&waited),
        (None, Some(WaitWord::Entry)),
        "CONTROL: kind=wait"
    );
}

/// Need (a) of design §12.2.1, whole: longer than the 64 cells the entry's
/// writer keeps.
const NEED_A: &str = "grant Screen Recording to com.aterm.aterm in System Settings \u{25b8} \
                      Privacy & Security \u{25b8} Screen Recording, so the windowed check \
                      can capture the menu";

/// `need` cut as the entry's writer cuts it (`escalate::clip_to`, 64 cells,
/// `…` the last), for a need of one-cell characters.
fn cut64(need: &str) -> String {
    if need.chars().count() <= 64 {
        return need.to_string();
    }
    let mut out: String = need.chars().take(63).collect();
    out.push('\u{2026}');
    out
}

/// The harness's entry as its writer words it
/// (`escalate::needs_attention_text`).
fn harness_entry(program: &str, first: &str, more: u16) -> String {
    let more = if more == 0 {
        String::new()
    } else {
        format!(" (+{more} more)")
    };
    format!("{program} needs you: {first}{more} \u{2014} carrying on")
}

/// The verdict a lone `owner=needs` ask of `text` gives.
fn harness_verdict(text: &str) -> Verdict {
    of(&NeedFacts {
        attention: vec![entry(
            crate::harness_host::NEEDS_ATTENTION_OWNER,
            text,
            Some(AttentionKind::Ask),
        )],
        ..facts()
    })
}

/// THE NEED WITHOUT ATERM'S FRAME (Autopilot v5 §12.2.3, §12.2.6): a whole
/// entry gives its need and program; one a byte cap cut inside ` — carrying
/// on` gives the same need; `(+3 more)` is counted and comes off; a text that
/// names no program, and another owner's ask, are not read (they stay
/// whole).
#[test]
fn need_words_takes_the_frame_off_a_harness_entry() {
    use aterm_phase::Program;
    let whole = harness_entry("claude", "the release credential to sign 0.48.0", 0);
    assert_eq!(
        need_words(&whole),
        Some(NeedWords {
            first: "the release credential to sign 0.48.0",
            more: 0,
            program: Program::Claude,
        })
    );
    let tail = " \u{2014} carrying on";
    for (at, _) in tail.char_indices().skip(1) {
        let cut = format!("agy needs you: sign it (+1 more){}", &tail[..at]);
        assert_eq!(
            need_words(&cut).map(|w| (w.first, w.more, w.program)),
            Some(("sign it", 1, Program::Agy)),
            "{cut:?}"
        );
    }
    let three = harness_entry("muse", &cut64(NEED_A), 3);
    let read = need_words(&three).expect("a counted entry");
    assert_eq!(
        (read.first, read.more, read.program),
        (cut64(NEED_A).as_str(), 3, Program::Muse)
    );
    for text in [
        "the build needs you: look \u{2014} carrying on",
        "claude needs you: sign it (+1 mo",
        "please look at the release",
    ] {
        assert_eq!(need_words(text), None, "{text:?}");
    }
    let other = of(&NeedFacts {
        attention: vec![entry("lab", &whole, Some(AttentionKind::Ask))],
        ..facts()
    });
    let need = other.need.expect("an ask");
    assert_eq!(
        (need.harness, need.what.as_str()),
        (None, whole.as_str()),
        "another owner's ask stays whole"
    );
}

/// A HARNESS'S ASK KEEPS ITS LINE WITHIN ITS BYTES (§12.2.3, §12.2.6): need
/// (a), and a need of `▸`s and CJK text, each cut at 64 cells by the entry's
/// writer and counted `(+1 more)`: `what` is at most [`ROW_WHAT_MAX`] bytes
/// with ` (+1 more)` whole at its end, and `harness.first` holds the need
/// as the entry carried it.
#[test]
fn a_harness_ask_keeps_its_line_within_its_bytes() {
    let wide = format!(
        "{}\u{2026}",
        "\u{25b8}\u{4e0a}\u{7ebf}"
            .repeat(13)
            .chars()
            .take(38)
            .collect::<String>()
    );
    for need in [cut64(NEED_A), wide] {
        let text = harness_entry("claude", &need, 1);
        let got = harness_verdict(&text).need.expect("the ask");
        let ask = got.harness.as_ref().expect("the harness's own words");
        assert_eq!(
            (ask.first.as_str(), ask.more, ask.program),
            (need.as_str(), 1, aterm_phase::Program::Claude)
        );
        assert!(
            got.what.len() <= ROW_WHAT_MAX && got.what.ends_with(" (+1 more)"),
            "{} bytes: {:?}",
            got.what.len(),
            got.what
        );
        assert!(!got.what.contains("needs you"), "{:?}", got.what);
        assert_eq!(got.key, ask_key(&text), "the key stays the entry's");
    }
}

/// Need (b) of design §12.2.1.
const NEED_B: &str = "the release credential (~/.aterm/release-credentials.toml) to sign 0.48.0";

/// The facts of a lone needs owner's entry `entry`, with `needs` told to
/// the window with `told_with` (the set told last).
fn told_with(entry: &str, needs: &[&str], told_with: &str) -> NeedFacts {
    let set: Vec<String> = needs.iter().map(|need| (*need).to_string()).collect();
    let mut told = ToldNeeds::default();
    told.tell(
        HarnessNeeds::told(&set, told_with).map(std::sync::Arc::new),
        Some(entry),
    );
    NeedFacts {
        attention: vec![entry_of(entry)],
        told,
        ..facts()
    }
}

/// The needs owner's ask of `text`.
fn entry_of(text: &str) -> AttentionFact {
    entry(
        crate::harness_host::NEEDS_ATTENTION_OWNER,
        text,
        Some(AttentionKind::Ask),
    )
}

#[test]
fn a_reported_need_yields_to_other_outstanding_asks() {
    let report = harness_entry("claude", "sign the release", 0);
    for report_first in [true, false] {
        let mut attention = vec![
            entry("ops", "check the deploy", Some(AttentionKind::Ask)),
            entry("ci", "older ask", Some(AttentionKind::Ask)),
            entry("build", "building", Some(AttentionKind::Wait)),
        ];
        attention.insert(usize::from(!report_first), entry_of(&report));
        let mut f = NeedFacts {
            attention,
            ..facts()
        };
        let v = of(&f);
        let need = v.need.unwrap();
        assert_eq!(need.what, "check the deploy");
        assert!(crate::presence::need_fact(&need).show_pill);
        assert_eq!(v.waiting.unwrap().word, WaitWord::Entry);

        f.attention.retain(|entry| entry.owner != "ops");
        assert_eq!(of(&f).need.unwrap().what, "older ask");
        f.attention.retain(|entry| entry.owner != "ci");
        let restored = of(&f).need.unwrap();
        assert_eq!(restored.key, ask_key(&report));
        assert!(!crate::presence::need_fact(&restored).show_pill);
    }
}

#[test]
fn a_reported_need_yields_to_an_unanswered_box_and_returns_afterward() {
    let report = harness_entry("claude", "sign the release", 0);
    for (agent, word) in [
        (boxed("bash:ask", "git push"), NeedWord::Approval),
        (agent("question"), NeedWord::Question),
    ] {
        for (supervised, hold) in [(false, None), (true, Some(HoldWord::Local))] {
            let mut f = NeedFacts {
                agent: agent.clone(),
                attention: vec![entry_of(&report)],
                supervised,
                hold,
                ..facts()
            };
            let need = of(&f).need.expect("the unanswered box");
            assert_eq!(need.word, word);
            assert!(crate::presence::need_fact(&need).show_pill);
            f.agent = None;
            let restored = of(&f).need.expect("the report still stands");
            assert_eq!(restored.key, ask_key(&report));
            assert!(restored.harness.is_some());
            assert!(!crate::presence::need_fact(&restored).show_pill);

            f.agent = agent.clone();
            f.attention = vec![entry("-", "please look", Some(AttentionKind::Ask))];
            assert_eq!(of(&f).need.unwrap().word, NeedWord::Ask);
        }
        let supervised = NeedFacts {
            agent,
            attention: vec![entry_of(&report)],
            supervised: true,
            ..facts()
        };
        assert!(of(&supervised).harness_ask().is_some());
    }
}

/// A HARNESS'S NEED READS IN ITS OWN WORDS (Autopilot v5 §12.2.3 item 4,
/// §12.2.6): where the loop told the window the whole set with
/// the very entry that stands, the verdict's `harness.first` is need (a)
/// whole — longer than the 64 cells the entry carries — with `more = 1`
/// and need (b) among its `others`; `what` stays within [`ROW_WHAT_MAX`]
/// with ` (+1 more)` whole, `needs_n=` counts 2, and the key stays the
/// entry's.
#[test]
fn a_harness_need_reads_in_its_own_words() {
    let entry = harness_entry("claude", &cut64(NEED_A), 1);
    let v = of(&told_with(&entry, &[NEED_A, NEED_B], &entry));
    let need = v.need.as_ref().expect("the ask");
    let ask = v.harness_ask().expect("the harness's own words");
    assert_eq!(
        (
            ask.first.as_str(),
            ask.more,
            ask.others.as_slice(),
            ask.program
        ),
        (
            NEED_A,
            1,
            [NEED_B.to_string()].as_slice(),
            aterm_phase::Program::Claude
        ),
        "need (a) whole, (b) beside it"
    );
    assert_eq!(ask.count(), 2);
    assert!(
        need.what.len() <= ROW_WHAT_MAX && need.what.ends_with(" (+1 more)"),
        "{:?}",
        need.what
    );
    assert_eq!(need.key, ask_key(&entry), "the key stays the entry's");

    let other = harness_entry("claude", "something else", 0);
    let parsed = of(&told_with(&entry, &[NEED_A, NEED_B], &other));
    let ask = parsed.harness_ask().expect("the entry's own parse");
    assert_eq!(
        (ask.first.as_str(), ask.more, ask.others.len()),
        (cut64(NEED_A).as_str(), 1, 0),
        "words told with another entry fall back to the parse"
    );

    let moved = of(&told_with(&entry, &[NEED_A, "a third thing"], &entry));
    assert_ne!(moved, v, "the set moved under one entry");
    assert_eq!(moved.need.as_ref().map(|n| n.key), Some(need.key));
    assert_eq!(
        moved.harness_ask().map(|a| a.others.clone()),
        Some(vec!["a third thing".to_string()])
    );
}

#[test]
fn an_old_truncated_report_stays_out_of_the_pill() {
    // The original writer capped the whole frame at 200 bytes. A three-byte
    // subject could fill that cap before the count and carrying-on suffix.
    let long = format!("claude needs you: {}", "\u{915}".repeat(60));
    assert_eq!(long.len(), 198);
    for text in [long.as_str(), "agy needs you: sign it (+1 mo"] {
        assert_eq!(need_words(text), None);
        let v = of(&NeedFacts {
            attention: vec![entry_of(text)],
            ..facts()
        });
        let need = v.need.expect("the stored report still stands");
        let ask = need
            .harness
            .as_ref()
            .expect("the report's known owner and program");
        assert_eq!(ask.first, text.split_once(NEEDS_YOU).unwrap().1);
        assert_eq!(ask.more, 0);
        assert!(!crate::presence::need_fact(&need).show_pill);

        let direct = of(&NeedFacts {
            attention: vec![entry("ops", text, Some(AttentionKind::Ask))],
            ..facts()
        });
        assert!(crate::presence::need_fact(&direct.need.unwrap()).show_pill);
    }
    for text in ["nobody needs you: sign it", "claude needs you: "] {
        let v = of(&NeedFacts {
            attention: vec![entry_of(text)],
            ..facts()
        });
        assert!(v.harness_ask().is_none());
    }
}

/// THE PROGRAM IS THE ENTRY'S, AND A NEED THAT FOLDS TO NOTHING IS NO
/// NEED: the told set is said under the program the entry names
/// (`agy needs you:` reads as agy), even where a cap cut the entry past its
/// count, so its parse reads nothing; a told need that folds to nothing is
/// passed over, and a set of none such falls back to the parse.
#[test]
fn a_told_set_reads_under_its_entrys_program() {
    let cut = "agy needs you: sign the release (+1 mo";
    assert_eq!(need_words(cut), None);
    let v = of(&told_with(
        cut,
        &["sign the release", "rotate the key"],
        cut,
    ));
    let ask = v.harness_ask().expect("the told set, under agy");
    assert_eq!(
        (ask.first.as_str(), ask.more, ask.program),
        ("sign the release", 1, aterm_phase::Program::Agy)
    );
    let entry = harness_entry("claude", "sign it", 0);
    let v = of(&told_with(&entry, &["\u{200b} ", "sign it"], &entry));
    assert_eq!(
        v.harness_ask().map(|a| (a.first.as_str(), a.more)),
        Some(("sign it", 0))
    );
    let alien = "nobody needs you: sign it \u{2014} carrying on";
    let v = of(&told_with(alien, &["sign it"], alien));
    assert_eq!(v.harness_ask(), None);
}

/// A NEED IN ANY SCRIPT READS WHOLE (§12.2.1's hover, "each need whole, up
/// to the policy's 160 characters"; the review of 2d): two needs of the
/// policy's full 160 characters in letters of three bytes each (480 bytes,
/// twice the banner's [`NEED_WHAT_MAX`]) told with the entry that stands
/// read whole in the verdict and say themselves whole on the tab's hover,
/// a line each; the banner's body still clips the first to its own
/// [`NEED_WHAT_MAX`].
#[test]
fn a_told_need_in_any_script_reads_whole() {
    let long: String = "\u{9700}".repeat(aterm_agent::supervise::policy::needs::NEED_CHARS);
    assert_eq!(long.len(), 480, "PRECONDITION: three bytes a letter");
    let entry = harness_entry("claude", &cut64(&long), 1);
    let v = of(&told_with(&entry, &[&long, &long], &entry));
    let ask = v.harness_ask().expect("the told set");
    assert_eq!(
        (ask.first.as_str(), ask.others.as_slice()),
        (long.as_str(), [long.clone()].as_slice()),
        "each need whole"
    );
    let hover = crate::app_presence::harness_need_hover(ask);
    assert_eq!(
        hover.get(..2),
        Some([format!("Needs you: {long}"), format!("Needs you: {long}")].as_slice()),
        "{hover:#?}"
    );
    let body = ask.line(NEED_WHAT_MAX + ask.more_words().len());
    assert!(
        body.len() <= NEED_WHAT_MAX + " (+1 more)".len() && body.ends_with("\u{2026} (+1 more)"),
        "{body}"
    );
}
