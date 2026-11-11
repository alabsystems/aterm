// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A seeded xorshift stream.
fn stream(seed: u64) -> impl FnMut() -> u64 {
    let mut x = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    }
}

/// Payload pieces from every class the writer escapes: quotes, backslash and
/// slash; C0 in its short and `\u` forms; DEL and C1; bidi controls and line
/// separators; multibyte text.
const PIECES: [&str; 6] = [
    "plain text ",
    "\"q\\/",
    "\x1b[31m\x07\x00\x08\x0c\r\n\t",
    "\u{7f}\u{85}\u{9b}",
    "\u{200e}\u{2066}x\u{2069}\u{2028}\u{2029}",
    "é漢🙂",
];

/// A header with every field drawn from the headers producers write: one
/// that carries the resize-law key counts drops, none included; one without
/// it says nothing of drops, or discloses some; its laws are the ones its
/// key names.
fn seeded_header(next: &mut impl FnMut() -> u64) -> CastHeader {
    let law = match next() % 5 {
        0 => None,
        1 => Some(0),
        v => Some(v - 1),
    };
    let resize_undo = law.map(|v| v != 0);
    let bursts = match next() % 2 {
        0 => 0,
        _ => 1 + next() % 10_000,
    };
    let drops = CastDrops {
        bursts,
        bytes: if bursts == 0 {
            0
        } else {
            bursts + next() % (1 << 40)
        },
    };
    let dropped = match resize_undo {
        Some(_) => Some(drops),
        None => (drops.bursts > 0 && next().is_multiple_of(2)).then_some(drops),
    };
    let handoff = match next() % 3 {
        0 => None,
        1 => Some(HandoffHeader { from_build: None }),
        _ => Some(HandoffHeader {
            from_build: Some(next()),
        }),
    };
    CastHeader {
        width: 1 + (next() % u64::from(u16::MAX)) as u16,
        height: 1 + (next() % u64::from(u16::MAX)) as u16,
        evicted: match next() % 2 {
            0 => 0,
            _ => 1 + next() % 1_000_000,
        },
        dropped,
        resize_undo,
        resize_laws: CastHeader::laws_named(law),
        handoff,
    }
}

/// Events on microsecond timestamps (the format prints six decimals), from
/// an offset up to 10^6 s, with payloads built from [`PIECES`].
fn seeded_events(next: &mut impl FnMut() -> u64, n: usize) -> Vec<CastEvent<Vec<u8>>> {
    let mut micros = next() % 1_000_000_000_000;
    (0..n)
        .map(|_| {
            micros += next() % 2_000_000;
            let t = micros as f64 / 1e6;
            match next() % 5 {
                0 => CastEvent::Resize {
                    t,
                    cols: next() as u16,
                    rows: next() as u16,
                },
                _ => CastEvent::Output {
                    t,
                    data: (0..next() % 6)
                        .map(|_| PIECES[(next() % 6) as usize])
                        .collect::<String>()
                        .into_bytes(),
                },
            }
        })
        .collect()
}

fn written<D: AsRef<[u8]>>(header: &CastHeader, events: &[CastEvent<D>]) -> String {
    let mut out = String::new();
    write_into(&mut out, header, events);
    out
}

/// What the writer is given the reader returns: every header field, the
/// handoff included, and every event, for headers producers write and
/// timestamps at the format's resolution; the seeds draw every form of
/// every field.
#[test]
fn write_then_parse_round_trips() {
    let mut forms = std::collections::BTreeSet::new();
    for seed in 0..64 {
        let mut next = stream(seed);
        let header = seeded_header(&mut next);
        let events = seeded_events(&mut next, 64);
        forms.insert(format!("undo {:?}", header.resize_undo));
        forms.insert(format!("laws {:?}", header.resize_laws));
        forms.insert(format!(
            "dropped {:?}",
            header.dropped.map(|d| d.bursts > 0)
        ));
        forms.insert(format!(
            "handoff {:?}",
            header.handoff.map(|h| h.from_build.is_some())
        ));
        forms.insert(format!("evicted {}", header.evicted > 0));
        let text = written(&header, &events);
        assert_eq!(
            parse(&text),
            Ok(Asciicast { header, events }),
            "seed {seed}:\n{text}"
        );
    }
    assert_eq!(
        forms.len(),
        3 + 3 + 3 + 3 + 2,
        "every form drawn: {forms:?}"
    );

    let header = CastHeader {
        width: 80,
        height: 24,
        evicted: 0,
        dropped: Some(CastDrops::default()),
        resize_undo: Some(true),
        resize_laws: CastHeader::laws_named(Some(ResizeLaw::default().header_value())),
        handoff: None,
    };
    let raw = [b'A', 0x80, 0xff, b'B', 0xc3];
    let lossy = parse(&written(
        &header,
        &[CastEvent::Output { t: 0.5, data: raw }],
    ))
    .expect("parses");
    assert_eq!(
        lossy.events,
        [CastEvent::Output {
            t: 0.5,
            data: String::from_utf8_lossy(&raw).into_owned().into_bytes(),
        }]
    );

    let none: [CastEvent<Vec<u8>>; 0] = [];
    let unsaid = CastHeader {
        resize_undo: None,
        ..header.clone()
    };
    assert_eq!(
        parse(&written(&unsaid, &none)).unwrap().header.dropped,
        None
    );
    let uncounted = CastHeader {
        dropped: None,
        ..header
    };
    assert_eq!(
        parse(&written(&uncounted, &none)).unwrap().header.dropped,
        Some(CastDrops::default())
    );
}

/// A time no recording can hold (NaN, infinite, negative, or one that prints as `-0`) is
/// written as 0, so every event line stays one the reader takes; a real time is written as
/// given.
#[test]
fn an_impossible_event_time_is_written_as_zero() {
    let header = CastHeader {
        width: 80,
        height: 24,
        evicted: 0,
        dropped: None,
        resize_undo: Some(true),
        resize_laws: CastHeader::laws_named(Some(ResizeLaw::default().header_value())),
        handoff: None,
    };
    let output = |t: f64| CastEvent::Output {
        t,
        data: b"a".to_vec(),
    };
    let mut events = [f64::NAN, -1.0, f64::INFINITY, -1e-9, -0.0]
        .map(output)
        .to_vec();
    events.push(CastEvent::Resize {
        t: f64::NEG_INFINITY,
        cols: 100,
        rows: 30,
    });
    let text = written(&header, &events);
    assert!(!text.contains("-0.000000"), "{text}");
    let read = parse(&text).expect("every line is a valid event");
    assert!(read.events.iter().all(|e| e.t().to_bits() == 0), "{text}");

    let real = written(&header, &[output(1.5)]);
    assert_eq!(
        parse(&real).expect("parses").events,
        [output(1.5)],
        "{real}"
    );
}

/// The header key: `3` reads the undo and the current law, `2` the undo and
/// the two laws aterms wrote it under (the newest first), `1` the undo
/// only, `0` `Some(false)`, absent `None` (a file from an aterm before the
/// undo), the last three under the law before the rows hand-back, a value
/// only a newer aterm writes under the newest law this one knows, and the
/// rest of the header is read as ever.
#[test]
fn the_resize_law_key_is_read_from_the_header() {
    let read = |header: &str| {
        parse(&format!("{header}\n[0.1, \"o\", \"hi\"]\n"))
            .expect("parses")
            .header
    };
    let keyed = |value: u64| {
        read(&format!(
            "{{\"version\": 2, \"width\": 80, \"height\": 24, \"aterm_resize_undo\": {value}}}"
        ))
    };
    let current = keyed(3);
    assert_eq!(
        (current.resize_undo, current.resize_laws),
        (Some(true), &[ResizeLaw::Rewrap][..])
    );
    assert_eq!(current.resize_laws, &[ResizeLaw::default()]);
    let handback = keyed(2);
    assert_eq!(
        (handback.resize_undo, handback.resize_laws),
        (
            Some(true),
            &[ResizeLaw::PushedRows, ResizeLaw::HandBack][..]
        )
    );
    let undo = keyed(1);
    assert_eq!(
        (undo.resize_undo, undo.resize_laws),
        (Some(true), &[ResizeLaw::Undo][..])
    );
    assert_eq!((undo.width, undo.height), (80, 24));
    assert_eq!(undo.dropped, Some(CastDrops::default()), "counted, none");
    let off = keyed(0);
    assert_eq!(
        (off.resize_undo, off.resize_laws),
        (Some(false), &[ResizeLaw::Undo][..])
    );
    let plain = read("{\"version\": 2, \"width\": 80, \"height\": 24}");
    assert_eq!(
        (plain.resize_undo, plain.resize_laws),
        (None, &[ResizeLaw::Undo][..])
    );
    assert_eq!(plain.dropped, None, "a producer that does not say");
    let newer = keyed(9);
    assert_eq!(newer.resize_laws, &[ResizeLaw::Rewrap]);
    let zero = read("{\"version\": 2, \"width\": 0, \"height\": 0}");
    assert_eq!((zero.width, zero.height), (1, 1));
    let huge = read("{\"version\": 2, \"width\": 100000, \"height\": 100000}");
    assert_eq!((huge.width, huge.height), (u16::MAX, u16::MAX));
}

#[test]
fn parser_reads_events_with_every_escape() {
    let text = concat!(
        "{\"version\": 2, \"width\": 135, \"height\": 63, \"env\": {\"TERM\": \"xterm\"}}\n",
        "[0.150484, \"o\", \"\\u001b[16t\\\"q\\\\ \\/ \\b\\f\\n\\r\\t\"]\n",
        "\n",
        "[1.655311, \"r\", \"135x64\"]\n",
        "[1.7, \"i\", \"typed\"]\n",
        "[1.8, \"o\", \"\\ud83d\\ude00 \\u00e9 ✻\"]\n",
        "[1.9, \"o\", \"lone \\ud83d x \\ude00 y\"]\n",
        "[2.0, \"o\", \"hi then \\ud83d\\u0041\"]\n",
    );
    let cast = parse(text).expect("parses");
    assert_eq!(
        cast.header,
        CastHeader {
            width: 135,
            height: 63,
            evicted: 0,
            dropped: None,
            resize_undo: None,
            resize_laws: &[ResizeLaw::Undo],
            handoff: None,
        }
    );
    assert_eq!(cast.events.len(), 5, "the `i` event is skipped");
    assert_eq!(
        cast.events[0],
        CastEvent::Output {
            t: 0.150484,
            data: b"\x1b[16t\"q\\ / \x08\x0c\n\r\t".to_vec(),
        }
    );
    assert_eq!(
        cast.events[1],
        CastEvent::Resize {
            t: 1.655311,
            cols: 135,
            rows: 64,
        }
    );
    let text_of = |i: usize| match &cast.events[i] {
        CastEvent::Output { data, .. } => String::from_utf8(data.clone()).unwrap(),
        CastEvent::Resize { .. } => panic!("not output"),
    };
    assert_eq!(text_of(2), "😀 é ✻", "a surrogate pair joins");
    assert_eq!(text_of(3), "lone \u{FFFD} x \u{FFFD} y", "lone surrogates");
    assert_eq!(
        text_of(4),
        "hi then \u{FFFD}A",
        "a high surrogate before a BMP escape"
    );
    assert_eq!(
        format!("{:.6}", cast.events[1].t()),
        "1.655311",
        "the time round-trips"
    );
}

/// The truncation, drop and handoff disclosures are read from the header,
/// whatever else their objects hold.
#[test]
fn parser_reads_the_aterm_disclosures() {
    let text = concat!(
        "{\"version\": 2, \"width\": 80, \"height\": 24, \"aterm_truncated\": ",
        "{\"evicted_events\": 17, \"note\": \"drop-oldest evicted the head\"}, ",
        "\"aterm_dropped\": {\"bursts\": 3, \"bytes\": 4096}, ",
        "\"aterm_handoff\": {\"carried\": 0, \"from_build\": 7, \"note\": \"n\"}}\n",
        "[0.000000, \"o\", \"x\"]\n",
    );
    let cast = parse(text).expect("parses");
    assert_eq!(cast.header.evicted, 17);
    assert_eq!(
        cast.header.dropped,
        Some(CastDrops {
            bursts: 3,
            bytes: 4096,
        })
    );
    assert_eq!(
        cast.header.handoff,
        Some(HandoffHeader {
            from_build: Some(7)
        })
    );
    assert_eq!(cast.clone().until(0.0).events.len(), 1);
    assert_eq!(cast.until(-1.0).events.len(), 0);
    let unknown = parse(
        "{\"version\": 2, \"width\": 80, \"height\": 24, \"aterm_handoff\": \
         {\"carried\": 0, \"from_build\": null}}\n",
    )
    .expect("parses");
    assert_eq!(
        unknown.header.handoff,
        Some(HandoffHeader { from_build: None })
    );
}

#[test]
fn parser_names_the_bad_line() {
    for (text, want) in [
        ("", "no header"),
        (
            "{\"version\": 1, \"width\": 80, \"height\": 24}\n",
            "line 1",
        ),
        ("{\"version\": 2, \"height\": 24}\n", "`width`"),
        (
            "{\"version\": 2, \"width\": 80, \"height\": 24}\n[0.1, \"o\", \"x\"]\n[0.2, \"r\", \"80by24\"]\n",
            "line 3",
        ),
        (
            "{\"version\": 2, \"width\": 80, \"height\": 24}\n[0.2, \"r\", \"+80x24\"]\n",
            "line 2",
        ),
        (
            "{\"version\": 2, \"width\": 80, \"height\": 24}\n[0.2, \"r\", \"80x+24\"]\n",
            "line 2",
        ),
        (
            "{\"version\": 2, \"width\": 80, \"height\": 24}\n[0.1, \"o\", \"\\q\"]\n",
            "line 2",
        ),
        (
            "{\"version\": 2, \"width\": 80, \"height\": 24}\n[0.1, \"o\", \"open\n",
            "line 2",
        ),
    ] {
        let err = parse(text).expect_err(text);
        assert!(err.contains(want), "{err} should name {want}");
    }
    let deep = format!("{}{}", "[".repeat(100), "]".repeat(100));
    assert!(parse_json(&deep).is_err(), "nesting is bounded");
    for malformed in [
        "\"\\u+041\"",
        "\"\\u-041\"",
        "\"\x00\"",
        "\"\t\"",
        "\"\x1f\"",
        "01",
        "-01",
        "1.",
        "1e",
        "1e+",
    ] {
        assert!(
            parse_json(malformed).is_err(),
            "{malformed} is not RFC 8259 JSON"
        );
    }
}
