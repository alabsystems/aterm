// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for reading, salvaging, summarizing, and sanitizing supervisor tapes without replay.

use astream_effects::EffectRecord;

use super::super::codec::{Item, Stamp, ctl_reply, ctl_request};
use super::super::core::DecideMark;
use super::super::frame::{FrameSink, TapeWriter};
use super::super::frame_tests::{Memory, Said};
use super::*;

const HOUR_NS: u64 = 3_600_000_000_000;

fn ok(stdout: &str) -> Vec<u8> {
    ctl_reply(&Ok(CtlReply {
        code: 0,
        stdout: stdout.to_string(),
        stderr: String::new(),
    }))
}

fn exchange(words: &[&str], reply: Vec<u8>, mono_ns: u64) -> Item {
    Item::stamped(
        EffectRecord::Exchange {
            door: doors::CTL.to_string(),
            request: ctl_request(words),
            reply,
        },
        Stamp {
            mono_ns,
            wall_ms: 1_789_660_500_000 + i64::try_from(mono_ns / 1_000_000).unwrap_or(0),
        },
    )
}

fn emit(door: &str, bytes: &[u8]) -> Item {
    Item::effect(EffectRecord::Emit {
        door: door.to_string(),
        bytes: bytes.to_vec(),
    })
}

fn approval_items() -> Vec<Item> {
    let screen = "{\"rows\":[\"⏺ Bash(ls -la)\",\"Do you want to proceed?\",\"❯ 1. Yes\",\"  2. No\"],\
                  \"cursor\":{\"row\":2,\"col\":2},\"seq\":41,\"gen\":\"3.41\"}";
    let mark = DecideMark {
        screen: Some(0),
        seq: 41,
        generation: Some("3.41".to_string()),
        rule: Some("safe-read@v1".to_string()),
        outcome: "Approve { rule_id: \"safe-read@v1\", choice: Digit(1) }".to_string(),
        facts: None,
    };
    vec![
        exchange(
            &["@s-1", "text", "--json", "tail=40"],
            ok(screen),
            1_000_000,
        ),
        Item::effect(EffectRecord::Mono(2_000_000)),
        emit(doors::DECIDE_APPROVAL, mark.encode().as_bytes()),
        exchange(
            &["@s-1", "meta", "set", "supervisor", "x"],
            ok("OK\n"),
            3_000_000,
        ),
        exchange(
            &["@s-1", "key", "if-gen=3.41", "1"],
            ok("OK seq=42\n"),
            4_000_000,
        ),
        emit(
            doors::JOURNAL,
            b"{\"t\":1789660500004,\"sid\":\"s-1\",\"kind\":\"approved\",\"phase\":\"prompt\",\
              \"seq\":41,\"complete\":null,\"rows\":null,\"summary\":\"ls -la\",\
              \"line\":\"APPROVED seq=41 ls -la\",\"turn\":null}\n",
        ),
        exchange(
            &["@s-1", "text", "--json", "tail=40"],
            ok("{\"rows\":[\"❯ \"],\"seq\":43}"),
            HOUR_NS,
        ),
        Item::End(End::Returned {
            how: "exit 0".to_string(),
        }),
    ]
}

fn written(items: &[Item]) -> Vec<u8> {
    let sink = Memory::default();
    let mut w = TapeWriter::start(
        Box::new(sink.clone()) as Box<dyn FrameSink>,
        "t.atape",
        "atape 1\ntape=t\n",
        Box::new(Said::default()),
    );
    for item in items {
        w.append(item);
    }
    drop(w);
    sink.bytes()
}

#[test]
fn a_decision_comes_back_with_its_screen_its_act_and_its_line() {
    let tape = open(&written(&approval_items()), false).expect("reads");
    assert_eq!(
        decisions(&tape),
        vec![Decision {
            door: doors::DECIDE_APPROVAL.to_string(),
            item: 2,
            rule: Some("safe-read@v1".to_string()),
            outcome: "Approve { rule_id: \"safe-read@v1\", choice: Digit(1) }".to_string(),
            seq: 41,
            screen_record: Some(0),
            screen: Some(vec![
                "⏺ Bash(ls -la)".to_string(),
                "Do you want to proceed?".to_string(),
                "❯ 1. Yes".to_string(),
                "  2. No".to_string(),
            ]),
            act: Some(vec![
                "@s-1".to_string(),
                "key".to_string(),
                "if-gen=3.41".to_string(),
                "1".to_string(),
            ]),
            reply: Some("0 OK seq=42".to_string()),
            journal: Some("APPROVED seq=41 ls -la".to_string()),
        }]
    );
    let text = render_decisions(&decisions(&tape));
    assert!(text.contains("│ ❯ 1. Yes"), "{text}");
}

#[test]
fn typed_attention_and_task_decisions_keep_the_act_and_its_authority() {
    use crate::harness::contract::Scope;
    use crate::supervise::tape::{corpus, ctl};

    let cases: [(Scope, u64, &[&str]); 3] = [
        (
            Scope::Questions,
            41,
            &["@s-1", "meta", "set", "attention", "question"],
        ),
        (Scope::TurnEnd, 42, &["@s-1", "meta", "unset", "attention"]),
        (Scope::TurnEnd, 43, &["@s-1", "task", "mark", "done"]),
    ];
    let mut items = Vec::new();
    let mut expected = Vec::new();
    for (scope, revision, argv) in cases {
        let mark = DecideMark {
            screen: None,
            seq: revision,
            generation: None,
            rule: None,
            outcome: "recorded action".to_string(),
            facts: None,
        };
        items.push(emit(doors::DECIDE_TURN_END, mark.encode().as_bytes()));
        for words in [&["@s-1", "status"][..], argv] {
            items.push(Item::effect(EffectRecord::Exchange {
                door: doors::CTL_DUTY.to_string(),
                request: ctl::duty_request(scope, revision, words),
                reply: ctl::encoded_reply(
                    doors::CTL_DUTY,
                    Some(Ok(CtlReply {
                        code: 0,
                        stdout: "OK\n".to_string(),
                        stderr: String::new(),
                    })),
                ),
            }));
        }
        let mut words: Vec<String> = argv.iter().map(|word| (*word).to_string()).collect();
        words.extend([
            format!("harness-duty={}", scope.word()),
            format!("if-supervisor-revision={revision}"),
        ]);
        expected.push(words);
    }
    let tape = open(&written(&items), false).expect("binary typed tape");
    let decided = decisions(&tape);
    assert_eq!(decided.len(), expected.len());
    let rendered = render_decisions(&decided);
    for (decision, words) in decided.iter().zip(expected) {
        assert_eq!(decision.act.as_ref(), Some(&words));
        assert_eq!(decision.reply.as_deref(), Some("0 OK"));
        assert!(rendered.contains(&words.join(" ")), "{rendered}");
    }
    assert_eq!(corpus::census(&tape).acts, 3, "typed reads are not acts");
}

#[test]
fn either_form_reads_and_damage_is_refused_unless_salvaged() {
    let items = approval_items();
    let bytes = written(&items);
    let binary = open(&bytes, false).expect("binary");
    let text = super::super::text::render(binary.header.as_deref(), &binary.items);
    let from_text = open(text.as_bytes(), false).expect("text");
    assert_eq!(from_text.items, binary.items);
    assert_eq!(from_text.header, binary.header);
    assert_eq!(binary.items, items);
    assert_eq!(tail_note(&binary), None);

    let torn = &bytes[..bytes.len() - 3];
    let read = open(torn, false).expect("a torn tape reads");
    assert!(
        tail_note(&read).is_some_and(|n| n.contains("torn")),
        "{:?}",
        read.tail
    );
    assert!(read.items.len() < items.len());

    let mut damaged = bytes.clone();
    let at = super::super::frame::MAGIC.len() + super::super::frame::HEAD + 2;
    damaged[at] ^= 0x01;
    let err = open(&damaged, false).expect_err("refused");
    assert!(err.contains("--salvage"), "{err}");
    let salvaged = open(&damaged, true).expect("salvaged");
    assert!(salvaged.items.is_empty(), "nothing whole before the header");
    assert!(tail_note(&salvaged).is_some_and(|n| n.starts_with("salvaged")));
}

#[test]
fn the_stats_count_what_the_tape_holds_and_what_it_cost() {
    let bytes = written(&approval_items());
    let s = stats(&open(&bytes, false).expect("reads"));
    assert_eq!(s.bytes, bytes.len());
    assert_eq!(s.items, 8);
    assert_eq!(s.effects, 7);
    assert_eq!(s.kinds.get("Exchange"), Some(&4));
    assert_eq!(s.kinds.get("Emit"), Some(&2));
    assert_eq!(s.kinds.get("Mono"), Some(&1));
    assert_eq!(s.doors.get(doors::CTL).map(|d| d.records), Some(4));
    assert_eq!(s.doors.get(doors::JOURNAL).map(|d| d.records), Some(1));
    assert_eq!(s.decisions.get(doors::DECIDE_APPROVAL), Some(&1));
    assert_eq!(s.span_ns, HOUR_NS, "the last stamp is an hour on");
    let kb = s.kb_per_hour.expect("spans an hour");
    assert!((kb - bytes.len() as f64 / 1000.0).abs() < 1e-9, "{kb}");
    assert_eq!(s.end.as_deref(), Some("returned exit 0"));
    let text = render_stats(&s);
    assert!(text.contains("decisions 1 (approval 1)"), "{text}");
    let still = stats(&open(&written(&[emit(doors::JOURNAL, b"x")]), false).expect("reads"));
    assert_eq!(still.kb_per_hour, None);
}

#[test]
fn nothing_a_tape_holds_reaches_the_reader_as_a_control_character() {
    const OSC52: &str = "\u{1b}]52;c;cHduZWQ=\u{7}";
    let hostile = format!("{OSC52}hello\u{9b}2J\u{7f}\u{202e}");
    let screen = format!(
        "{{\"rows\":[{}],\"seq\":41}}",
        crate::supervise::journal::json_str(&hostile)
    );
    let mark = DecideMark {
        screen: Some(0),
        seq: 41,
        generation: None,
        rule: Some(format!("rule{OSC52}")),
        outcome: format!("Approve {OSC52}"),
        facts: None,
    };
    let journal = format!(
        "{{\"t\":1,\"sid\":\"s-1\",\"kind\":\"approved\",\"phase\":\"prompt\",\"seq\":41,\
         \"complete\":null,\"rows\":null,\"summary\":\"x\",\"line\":{},\"turn\":null}}\n",
        crate::supervise::journal::json_str(&hostile)
    );
    let items = vec![
        exchange(&["@s-1", "text", "--json", "tail=40"], ok(&screen), 1_000),
        emit(doors::DECIDE_APPROVAL, mark.encode().as_bytes()),
        exchange(
            &["@s-1", "key", hostile.as_str()],
            ok(&format!("{hostile}\n")),
            2_000,
        ),
        emit(doors::JOURNAL, journal.as_bytes()),
        emit(&format!("door{OSC52}"), hostile.as_bytes()),
        Item::effect(EffectRecord::File {
            path: format!("@ledger/{OSC52}"),
            bytes: hostile.as_bytes().to_vec(),
        }),
        Item::Note {
            kind: format!("kind{OSC52}"),
            text: hostile.clone(),
        },
        Item::End(End::Unwound {
            message: hostile.clone(),
        }),
    ];
    let tape = open(&written(&items), false).expect("reads");
    let decided = decisions(&tape);
    assert_eq!(
        decided[0].screen.as_deref(),
        Some(&[hostile.clone()][..]),
        "the structured answer keeps the row as it was"
    );
    for (what, text) in [
        ("cat", skim(&tape)),
        ("show --decisions", render_decisions(&decided)),
        ("stats", render_stats(&stats(&tape))),
    ] {
        let raw: Vec<char> = text
            .chars()
            .filter(|c| aterm_codec::tty::is_active(*c) && *c != '\n')
            .collect();
        assert!(raw.is_empty(), "{what} printed {raw:?}:\n{text}");
        assert!(
            text.contains("\\x1b]52;c;"),
            "{what} writes the ESC out:\n{text}"
        );
    }
    let shown_row = render_decisions(&decided);
    assert!(
        shown_row.contains("│ \\x1b]52;c;cHduZWQ=\\x07hello\\u{009b}2J\\x7f\\u{202e}"),
        "{shown_row}"
    );
}

/// A duty act (`ctl.duty`) answered `stdout`, stamped `wall_ms`.
fn duty_act(words: &[&str], stdout: &str, wall_ms: i64) -> Item {
    use crate::harness::contract::Scope;
    use crate::supervise::tape::ctl;
    Item::stamped(
        EffectRecord::Exchange {
            door: doors::CTL_DUTY.to_string(),
            request: ctl::duty_request(Scope::TurnEnd, 2, words),
            reply: ctl::encoded_reply(
                doors::CTL_DUTY,
                Some(Ok(CtlReply {
                    code: 0,
                    stdout: stdout.to_string(),
                    stderr: String::new(),
                })),
            ),
        },
        Stamp {
            mono_ns: 0,
            wall_ms,
        },
    )
}

/// A read (`ctl`) answered `stdout`, stamped `wall_ms`.
fn read_at(words: &[&str], stdout: &str, wall_ms: i64) -> Item {
    Item::stamped(
        EffectRecord::Exchange {
            door: doors::CTL.to_string(),
            request: ctl_request(words),
            reply: ok(stdout),
        },
        Stamp {
            mono_ns: 0,
            wall_ms,
        },
    )
}

/// A turn-end decision of `rule` at `seq`.
fn decided(rule: Option<&str>, seq: u64, outcome: &str) -> Item {
    let mark = DecideMark {
        screen: None,
        seq,
        generation: None,
        rule: rule.map(str::to_string),
        outcome: outcome.to_string(),
        facts: None,
    };
    emit(doors::DECIDE_TURN_END, mark.encode().as_bytes())
}

/// The words Land's instruction opens with, as a test writes them.
const LAND_WORDS: &str = "[aterm harness] Autopilot is Land: land ALL of this work into the \
                          remote main branch, no exceptions.";

/// A screen read (`text --json`) at `wall_ms` whose composer shows `composer`, wrapped at 40
/// columns as a program draws a long line.
fn screen_at(composer: &str, seq: u64, wall_ms: i64) -> Item {
    let mut rows = vec![
        "⏺ Implemented the parser module.".to_string(),
        String::new(),
    ];
    let chars: Vec<char> = format!("❯ {composer}").chars().collect();
    rows.extend(chars.chunks(40).map(|c| c.iter().collect::<String>()));
    let json = format!(
        "{{\"rows\":[{}],\"seq\":{seq}}}",
        rows.iter()
            .map(|r| crate::supervise::journal::json_str(r))
            .collect::<Vec<_>>()
            .join(",")
    );
    read_at(&["@s-1", "text", "--json"], &json, wall_ms)
}

/// THE LOOP'S WRITE IS TIMED APART FROM THE PROGRAM'S DRAW (rv1-l2's land-n, 2026-11-01): the
/// decision at the point's clock, the `send` 70 ms on, the program drawing nothing for 24.8 s
/// (a `status` read finding the text unread meanwhile), then aterm's Enter 0.6 s after the read
/// that showed it. The hold is the write to the last read that did not show the text, and the
/// `await seq` that then woke on its draw; the `await idle` settle after it is aterm's. An Enter
/// the host turned away is no Enter, nor a decision that writes nothing, nor a write it turned
/// away; a later write's reads are its own. Controls by mutation: a hold that counts every wait
/// (or none), an Enter taken whatever the host answered.
#[test]
fn a_write_is_timed_from_its_decision_apart_from_the_programs_draw() {
    const P: i64 = 1_793_556_697_159;
    let send = [
        "@s-1",
        "send",
        "if-human=0",
        "if-gen=0.868",
        "--",
        LAND_WORDS,
    ];
    let nudge = [
        "@s-1",
        "send",
        "if-human=0",
        "if-gen=0.1301",
        "--",
        "Keep landing.",
    ];
    // The generation is bound apart from `key`: on one line the two read to gitleaks'
    // generic-api-key rule as a key and its value, and the export guard refuses them.
    let gen_1096 = "if-gen=0.1096";
    let enter = ["@s-1", "key", gen_1096, "enter"];
    let items = vec![
        Item::effect(EffectRecord::Clock(u64::try_from(P).unwrap() * 1_000_000)),
        screen_at("", 868, P),
        decided(Some("land@v1"), 868, "Type { rule_id: \"land@v1\" }"),
        duty_act(&["@s-1", "lease", "acquire"], "OK lease acquired\n", P + 68),
        duty_act(&send, "OK seq=868\n", P + 70),
        read_at(
            &["@s-1", "await", "seq", "868", "timeout", "10000"],
            "OK timeout\n",
            P + 10_075,
        ),
        read_at(
            &["@s-1", "status"],
            "OK sid=4 input=pending input_bytes=565 input_wait_ms=10004 fg_rss_mb=-\n",
            P + 10_080,
        ),
        read_at(
            &["@s-1", "await", "idle", "600"],
            "OK idle 868\n",
            P + 10_700,
        ),
        screen_at("", 868, P + 10_710),
        read_at(
            &["@s-1", "await", "seq", "868", "timeout", "20000"],
            "OK seq 1096\n",
            P + 24_910,
        ),
        read_at(
            &["@s-1", "await", "idle", "600"],
            "OK idle 1096\n",
            P + 25_400,
        ),
        screen_at(LAND_WORDS, 1096, P + 25_410),
        read_at(
            &["@s-1", "status"],
            "OK sid=4 input=pending input_bytes=1 input_wait_ms=30500 fg_rss_mb=-\n",
            P + 25_420,
        ),
        duty_act(&enter, "OK skipped changed\n", P + 25_430),
        duty_act(&enter, "OK seq=1097\n", P + 25_460),
        decided(Some("continue@v1"), 1200, "Type"),
        duty_act(&nudge, "OK skipped changed\n", P + 30_000),
        decided(Some("land-nudge@v1"), 1300, "Type"),
        Item::effect(EffectRecord::Clock(
            u64::try_from(P + 40_000).unwrap() * 1_000_000,
        )),
        decided(Some("land-nudge@v1"), 1301, "Type"),
        duty_act(&nudge, "OK seq=1301\n", P + 40_050),
        decided(None, 1301, "WaitUntil"),
        read_at(
            &["@s-1", "status"],
            "OK sid=4 input=pending input_bytes=3 input_wait_ms=900 fg_rss_mb=-\n",
            P + 41_000,
        ),
        screen_at("", 1301, P + 41_500),
    ];
    let tape = open(&written(&items), false).expect("reads");
    let wrote = typed(&tape);
    assert_eq!(
        wrote,
        vec![
            Typed {
                rule: Some("land@v1".to_string()),
                seq: 868,
                decided_ms: Some(P),
                sent_ms: P + 70,
                drawn_ms: Some(P + 25_410),
                held_ms: Some(10_640 + 14_200),
                unshown_ms: None,
                unread: Some(Unread {
                    at_ms: P + 10_080,
                    word: "pending".to_string(),
                    bytes: Some(565),
                    wait_ms: Some(10_004),
                }),
                reaction: Some(false),
                enter_ms: Some(P + 25_460),
            },
            Typed {
                rule: Some("land-nudge@v1".to_string()),
                seq: 1301,
                decided_ms: Some(P + 40_000),
                sent_ms: P + 40_050,
                unshown_ms: Some(P + 41_500),
                unread: Some(Unread {
                    at_ms: P + 41_000,
                    word: "pending".to_string(),
                    bytes: Some(3),
                    wait_ms: Some(900),
                }),
                ..Typed::default()
            },
        ]
    );
    assert_eq!(
        wrote[0].aterm_ms(P),
        Some(620),
        "the point to the Enter, less the hold"
    );
    assert_eq!(wrote[1].aterm_ms(P + 40_000), None, "no Enter taken");
}

/// A LINE DRAWN PAST THE ECHO'S HANG (rv1-ab3's land-n): the write given up on, a decision that
/// waits, then the line seen drawn and sent by a later decision of its rule, whose Enter is the
/// write's. The program's hold runs to the last read that did not show it; a look aterm took late
/// (here a wait that never woke on the screen) is aterm's, not the program's. An Enter under
/// another rule's decision is not the write's. Controls by mutation: an Enter taken under any
/// decision; a hold to the read that showed the line.
#[test]
fn a_line_drawn_late_is_the_programs_only_while_aterm_watched_for_it() {
    const P: i64 = 1_793_600_000_000;
    let send = [
        "@s-1",
        "send",
        "if-human=0",
        "if-gen=0.891",
        "--",
        LAND_WORDS,
    ];
    let enter = ["@s-1", "key", "if-gen=0.1115", "enter"];
    let items = |submit_rule: &str| {
        vec![
            Item::effect(EffectRecord::Clock(u64::try_from(P).unwrap() * 1_000_000)),
            screen_at("", 891, P),
            decided(Some("land@v1"), 891, "Type"),
            duty_act(&send, "OK seq=891\n", P + 4),
            read_at(
                &["@s-1", "await", "seq", "891", "timeout", "10000"],
                "OK timeout\n",
                P + 10_004,
            ),
            read_at(
                &["@s-1", "status"],
                "OK sid=4 input=pending input_bytes=565 input_wait_ms=10001 fg_rss_mb=-\n",
                P + 10_010,
            ),
            screen_at("", 891, P + 60_010),
            decided(None, 891, "WaitUntil"),
            read_at(
                &["@s-1", "await", "agent", "891", "timeout", "60000"],
                "OK agent idle\n",
                P + 90_000,
            ),
            screen_at(LAND_WORDS, 1115, P + 90_010),
            decided(Some(submit_rule), 1115, "Submit"),
            duty_act(&enter, "OK seq=1116\n", P + 90_700),
        ]
    };
    let tape = open(&written(&items("land@v1")), false).expect("reads");
    let wrote = typed(&tape);
    assert_eq!(wrote.len(), 1, "{wrote:#?}");
    assert_eq!(wrote[0].drawn_ms, Some(P + 90_010));
    assert_eq!(
        wrote[0].held_ms,
        Some(60_006),
        "to the last read that did not show it"
    );
    assert_eq!(
        wrote[0].enter_ms,
        Some(P + 90_700),
        "the later Submit's Enter"
    );
    assert_eq!(wrote[0].reaction, Some(false), "its first look ran out");
    assert_eq!(wrote[0].aterm_ms(P), Some(90_700 - 60_006));
    let tape = open(&written(&items("land-check@v1")), false).expect("reads");
    assert_eq!(
        typed(&tape)[0].enter_ms,
        None,
        "another rule's Enter is not the write's"
    );
}
