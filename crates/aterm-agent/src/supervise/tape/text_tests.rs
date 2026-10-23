// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A tape's text reads back to the very same items.

use std::io::ErrorKind;

use astream_effects::EffectRecord;
use aterm_spec::rng::XorShift;

use super::*;
use crate::supervise::tape::codec::KINDS;

fn text_of(rng: &mut XorShift, max: usize) -> String {
    const PIECES: [&str; 12] = [
        "a", "Z", " ", "\n", "\0", "\t", "\r", "\\", "é", "猫", "=", "|",
    ];
    (0..rng.index(max + 1))
        .map(|_| *rng.pick(&PIECES))
        .collect()
}

fn bytes_of(rng: &mut XorShift, max: usize) -> Vec<u8> {
    match rng.index(5) {
        0 => Vec::new(),
        1 => b"\n".to_vec(),
        2 => text_of(rng, max).into_bytes(),
        _ => (0..rng.index(max + 1))
            .map(|_| (rng.next_u64() & 0xff) as u8)
            .collect(),
    }
}

/// A screen read's reply text, as `text --json` writes one or nearly: its
/// rows any text (escaped as JSON), its final newline or not — and, one in
/// three, broken the ways the row cutter must decline or survive: cut short
/// anywhere, a space or a raw newline between rows, a row that is no string.
fn screen_of(rng: &mut XorShift) -> String {
    let rows: Vec<String> = (0..rng.index(6))
        .map(|_| crate::supervise::journal::json_str(&text_of(rng, 16)))
        .collect();
    let sep = match rng.index(6) {
        0 => ", ",
        1 => ",\n",
        _ => ",",
    };
    let mut text = format!(
        "{{\"rows\":[{}],\"seq\":{}}}",
        rows.join(sep),
        rng.index(99)
    );
    if rng.index(2) == 0 {
        text.push('\n');
    }
    match rng.index(9) {
        0 => text.replace("\"rows\":[\"", "\"rows\":[7,\""),
        1 => text.replace("\"rows\":[\"", "\"rows\":[\"\\"),
        2 => {
            let at = rng.index(text.chars().count() + 1);
            text.chars().take(at).collect()
        }
        _ => text,
    }
}

fn reply_of(rng: &mut XorShift) -> Result<CtlReply, String> {
    if rng.index(4) == 0 {
        Err(text_of(rng, 30))
    } else {
        Ok(CtlReply {
            code: [0, 1, 124, -1, i32::MIN][rng.index(5)],
            stdout: if rng.index(3) == 0 {
                screen_of(rng)
            } else {
                text_of(rng, 200)
            },
            stderr: text_of(rng, 20),
        })
    }
}

fn item_of(rng: &mut XorShift) -> Item {
    let doors = [
        doors::CTL,
        doors::TELLER,
        doors::SLEEP,
        doors::WRITE_BOUND,
        "a door with spaces=and equals",
        "",
    ];
    let record = match rng.index(11) {
        0 => EffectRecord::Clock(rng.next_u64()),
        1 => EffectRecord::Mono(rng.next_u64()),
        2 => EffectRecord::Rand(rng.next_u64()),
        3 => EffectRecord::File {
            path: text_of(rng, 20),
            bytes: bytes_of(rng, 300),
        },
        4 => EffectRecord::FileErr {
            path: text_of(rng, 20),
            kind: rng.pick(&KINDS).1,
        },
        5 | 6 => {
            let words: Vec<String> = (0..rng.index(5)).map(|_| text_of(rng, 12)).collect();
            let words: Vec<&str> = words.iter().map(String::as_str).collect();
            EffectRecord::Exchange {
                door: doors::CTL.to_string(),
                request: codec::ctl_request(&words),
                reply: codec::ctl_reply(&reply_of(rng)),
            }
        }
        7 => EffectRecord::Exchange {
            door: (*rng.pick(&doors)).to_string(),
            request: bytes_of(rng, 40),
            reply: bytes_of(rng, 200),
        },
        8 => EffectRecord::Emit {
            door: (*rng.pick(&doors)).to_string(),
            bytes: bytes_of(rng, 200),
        },
        9 => EffectRecord::Raised {
            door: doors::RAISED_STOP.to_string(),
            false_reads: rng.next_u64(),
        },
        _ => {
            return match rng.index(7) {
                0 => Item::Note {
                    kind: text_of(rng, 8),
                    text: text_of(rng, 80),
                },
                1 => Item::Mark(Mark::Nested {
                    inner: text_of(rng, 6),
                    outer: text_of(rng, 6),
                }),
                2 => Item::Mark(Mark::Truncated {
                    door: rng.next_u64(),
                }),
                3 => Item::End(End::Returned {
                    how: text_of(rng, 20),
                }),
                4 => Item::End(End::Suspended),
                _ => Item::End(End::Unwound {
                    message: text_of(rng, 20),
                }),
            };
        }
    };
    let stamp = (rng.index(2) == 0).then(|| Stamp {
        mono_ns: rng.next_u64(),
        wall_ms: [i64::MIN, -1, 0, i64::MAX, 1_790_000_000_000][rng.index(5)],
    });
    Item::Effect { record, stamp }
}

/// Generated tapes, many seeds over, read back from their text exactly —
/// with and without a header — and every kind of item was generated.
#[test]
fn every_item_reads_back_from_its_text() {
    let mut kinds = std::collections::BTreeSet::new();
    for seed in 1..=300u64 {
        let mut rng = XorShift::new(seed);
        let items: Vec<Item> = (0..rng.range(0, 40)).map(|_| item_of(&mut rng)).collect();
        let header = (rng.index(3) != 0).then(|| text_of(&mut rng, 120));
        let text = render(header.as_deref(), &items);
        let (h, back) = parse(&text).unwrap_or_else(|e| panic!("seed {seed}: {e}\n{text}"));
        assert_eq!(h, header, "seed {seed}");
        assert_eq!(back, items, "seed {seed}:\n{text}");
        for item in &items {
            kinds.insert(match item {
                Item::Effect { record, .. } => record.kind(),
                Item::Note { .. } => "Note",
                Item::Mark(_) => "Mark",
                Item::End(_) => "End",
            });
        }
    }
    assert_eq!(kinds.len(), 10, "every kind generated: {kinds:?}");
}

/// A control round trip reads as its words and reply; its stdout's final
/// newline is its last, empty line; and the same bytes padded out (a varint
/// in another spelling) are written raw and read back as they were.
#[test]
fn a_control_round_trip_is_words_and_a_reply_unless_its_bytes_say_otherwise() {
    let reply = Ok(CtlReply {
        code: 0,
        stdout: "{\"rows\":[\"⏺ Done.\"],\"seq\":41}\n".to_string(),
        stderr: String::new(),
    });
    let item = Item::stamped(
        EffectRecord::Exchange {
            door: doors::CTL.to_string(),
            request: codec::ctl_request(&["text", "--json", "tail=40"]),
            reply: codec::ctl_reply(&reply),
        },
        Stamp {
            mono_ns: 20_003_000_123,
            wall_ms: 1_789_660_520_123,
        },
    );
    let text = render(None, std::slice::from_ref(&item));
    assert_eq!(
        text,
        "atape-text 1\nexchange ctl stamp=20003000123,1789660520123\n> text\n> --json\n> tail=40\n\
         < ok 0\n!\n| {\"rows\":[\n= \"⏺ Done.\"\n= ],\"seq\":41}\n|\n"
    );
    assert_eq!(parse(&text).expect("reads").1, vec![item]);
    let mut padded = vec![0x83, 0x00];
    padded.extend_from_slice(&codec::ctl_request(&["a", "b", "c"])[1..]);
    let raw = Item::effect(EffectRecord::Exchange {
        door: doors::CTL.to_string(),
        request: padded,
        reply: codec::ctl_reply(&reply),
    });
    let text = render(None, std::slice::from_ref(&raw));
    assert!(text.contains("exchange ctl raw\n"), "{text}");
    assert_eq!(parse(&text).expect("reads").1, vec![raw]);
}

/// Text this build did not write is refused, by line: another first line, an
/// unknown item, a body missing, a broken escape, a control reply that is
/// not one.
#[test]
fn text_it_did_not_write_is_refused_by_line() {
    for (text, says) in [
        ("atape 1\n", "first line"),
        ("atape-text 1\nwobble 3\n", "line 2"),
        ("atape-text 1\nfile @ledger/x\n", "body"),
        ("atape-text 1\nnote k\n| \\q\n", "line 3"),
        (
            "atape-text 1\nexchange ctl\n> text\n< maybe\n",
            "not a control reply",
        ),
        ("atape-text 1\nmono 12x\n", "no number"),
        ("atape-text 1\nfile-err NoSuchKind /x\n", "no error kind"),
        ("atape-text 1\nnote k\n|x\n", "no space"),
    ] {
        let err = parse(text).expect_err(text);
        assert!(err.contains(says), "{text:?}: {err}");
    }
    assert_eq!(
        parse("atape-text 1\nfile-err NotFound /x\n")
            .expect("reads")
            .1,
        vec![Item::effect(EffectRecord::FileErr {
            path: "/x".to_string(),
            kind: ErrorKind::NotFound,
        })]
    );
}

/// A SCREEN IS WRITTEN A ROW TO A LINE (the plan's §6.6): each row of a
/// `text --json` reply on a line of its own behind `=`, read back to the
/// very same bytes — and two tapes whose screens differ in one row differ
/// in one line of their text, so a corpus diff reads as the row that
/// changed.
#[test]
fn a_screen_is_written_a_row_to_a_line_and_diffs_as_the_row_that_changed() {
    let screen = |last: &str| {
        let item = Item::effect(EffectRecord::Exchange {
            door: doors::CTL.to_string(),
            request: codec::ctl_request(&["text", "--json", "tail=40"]),
            reply: codec::ctl_reply(&Ok(CtlReply {
                code: 0,
                stdout: format!(
                    "{{\"rows\":[\"⏺ Done.\",\"\",\"say \\\"hi\\\" \\\\ \\u001b\",{}],\"seq\":41}}\n",
                    crate::supervise::journal::json_str(last)
                ),
                stderr: String::new(),
            })),
        });
        (render(None, std::slice::from_ref(&item)), item)
    };
    let (text, item) = screen("❯ ");
    assert_eq!(parse(&text).expect("reads").1, vec![item]);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines[lines.len() - 7..],
        [
            "| {\"rows\":[",
            "= \"⏺ Done.\",",
            "= \"\",",
            "= \"say \\\\\"hi\\\\\" \\\\\\\\ \\\\u001b\",",
            "= \"❯ \"",
            "= ],\"seq\":41}",
            "|",
        ],
        "{text}"
    );
    let (other, _) = screen("❯ git status");
    let changed: Vec<(&str, &str)> = text
        .lines()
        .zip(other.lines())
        .filter(|(a, b)| a != b)
        .collect();
    assert_eq!(text.lines().count(), other.lines().count());
    assert_eq!(changed, [("= \"❯ \"", "= \"❯ git status\"")], "{other}");
    for stdout in [
        "{\"rows\":[],\"seq\":1}",
        "{\"rows\":[\"a\", \"b\"],\"seq\":1}",
        "{\"rows\":[\"a\",7],\"seq\":1}",
        "{\"rows\":[\"a\"",
        "plain words",
    ] {
        let item = Item::effect(EffectRecord::Exchange {
            door: doors::CTL.to_string(),
            request: codec::ctl_request(&["text"]),
            reply: codec::ctl_reply(&Ok(CtlReply {
                code: 0,
                stdout: stdout.to_string(),
                stderr: String::new(),
            })),
        });
        let text = render(None, std::slice::from_ref(&item));
        assert!(!text.contains("\n= "), "{stdout:?}:\n{text}");
        assert_eq!(parse(&text).expect("reads").1, vec![item], "{stdout:?}");
    }
}

/// Human-readable tape text keeps ordinary Unicode readable while active
/// display characters remain inert and every original byte reads back.
#[test]
fn tape_text_is_inert_and_round_trips_active_unicode() {
    let round_trip = |value: &str| {
        let items = vec![Item::Note {
            kind: value.to_string(),
            text: value.to_string(),
        }];
        let text = render(Some(value), &items);
        let (header, back) = parse(&text).expect("rendered tape reads back");
        assert_eq!(header.as_deref(), Some(value));
        assert_eq!(back, items);
        text
    };
    let safe = round_trip(r"literal \x1b café 日本 🦀");
    assert!(
        safe.contains("café 日本 🦀"),
        "ordinary Unicode stays readable"
    );
    assert!(safe.contains(r"\\x1b"), "a literal escape stays literal");
    for value in ["before\u{1b}after", "before\u{009b}after"] {
        let text = round_trip(value);
        assert!(!text.contains('\u{1b}') && !text.contains('\u{009b}'));
    }

    let mut active_output = Vec::new();
    for active in [
        '\u{061c}', '\u{200e}', '\u{202e}', '\u{2066}', '\u{2028}', '\u{2029}',
    ] {
        let value = format!("before{active}after");
        let items = vec![Item::Note {
            kind: value.clone(),
            text: value.clone(),
        }];
        let text = render(Some(&value), &items);
        match parse(&text) {
            Ok((header, back)) if header.as_deref() == Some(value.as_str()) && back == items => {}
            other => active_output.push(format!(
                "U+{:04X} did not round trip: {other:?}",
                u32::from(active)
            )),
        }
        if text.contains(active) {
            active_output.push(format!("U+{:04X} emitted raw", u32::from(active)));
        }
    }
    assert!(
        active_output.is_empty(),
        "active display characters escaped in tape text: {active_output:?}"
    );
}

/// agh's tapes are a subset of atape-text 1 (design §21.3 of docs/DESIGN-agh-2026-10-19.md):
/// a fixture agh recorded from GitHub reads here as effects alone, with no stamp, and renders
/// back to the very same text. aterm-agent never links agh; it reads agh's file.
#[test]
fn agh_fixture_tape_is_atape_text() {
    let text = include_str!("../../../../agh/tests/tapes/repo-view.atape");
    let (header, items) = parse(text).expect("agh's tape reads as atape-text 1");
    assert!(!items.is_empty());
    for item in &items {
        assert!(
            matches!(item, Item::Effect { stamp: None, .. }),
            "not a bare effect: {item:?}"
        );
    }
    assert_eq!(render(header.as_deref(), &items), text);
}
