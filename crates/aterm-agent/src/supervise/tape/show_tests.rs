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
