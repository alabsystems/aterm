// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The journal's line codec (design §5.3, phase 2B): every frame round-trips;
//! a torn line glued onto the next never decodes as a record, a chimera or
//! another session's line; a partial last line is never a line; unknown keys
//! and kinds are skipped; the names a retention pass may touch are exactly
//! the writer's spelling; and the content tier never reaches a line.

use aterm_slog::journal::{
    EndWhy, Frame, Inst, LINE_MAX, LineError, LossWhy, complete_lines, decode_line, encode_frame,
    is_end_line, line_subject, lock_name, parse_lock_name, parse_segment_name, segment_name,
};
use aterm_slog::{
    Actor, Cap, Conf, FIELDS, Fields, GapWhy, Key, Kind, Log, Mark, Node, ShortId, Src, Tier,
    encode_with_table,
};

use crate::support::{at, lines};

fn sid() -> ShortId {
    ShortId::new("s-9f3c0a").expect("a sid")
}

fn inst() -> Inst {
    Inst::new("p4242-0123456789abcdef01234567").expect("an instance")
}

/// A log with a session, a run and a mark, for real records.
fn sample() -> Log {
    let mut log = Log::new(1);
    let _ = crate::support::agent_session(&mut log, 0);
    let _ = log.mark(
        Mark::new(Kind::Human, Src::Input, Conf::Exact).by(Actor::Human),
        at(5),
    );
    log
}

fn every_frame() -> Vec<Frame> {
    let log = sample();
    let mut out = vec![
        Frame::Seg {
            v: 1,
            inst: inst(),
            n: 3,
            t: 1_760_600_000_000,
            build: "1.4.0+g99af1a6".to_string(),
        },
        Frame::Snap {
            sid: sid(),
            head: 17,
            ep: 2,
            base: 1,
            first: 1,
            open: 2,
            t: 1_760_600_000_001,
        },
        Frame::Gap {
            sid: sid(),
            why: LossWhy::Evicted,
            lost: 3,
            after: 2,
            through: 6,
        },
        Frame::Gap {
            sid: sid(),
            why: LossWhy::Dropped,
            lost: 0,
            after: 9,
            through: 9,
        },
        Frame::Hole {
            sid: sid(),
            lo: 7,
            hi: 65543,
            why: GapWhy::Handoff,
        },
        Frame::Handed {
            sid: sid(),
            fence: 42,
        },
        Frame::End {
            why: EndWhy::Quit,
            t: 9,
        },
        Frame::End {
            why: EndWhy::Handoff,
            t: 10,
        },
    ];
    for (i, r) in log.window().enumerate() {
        out.push(Frame::Rec {
            sid: sid(),
            rec: r.clone(),
            restated: i == 0,
        });
    }
    out
}

/// A record decodes at the host tier, so its frame round-trips through its
/// host-tier copy.
fn host_copy(f: &Frame) -> Frame {
    match f {
        Frame::Rec { sid, rec, restated } => Frame::Rec {
            sid: *sid,
            rec: aterm_slog::decode(&aterm_slog::encode(rec, Tier::Host).expect("fits"))
                .expect("decodes"),
            restated: *restated,
        },
        other => other.clone(),
    }
}

#[test]
fn every_frame_round_trips() {
    for f in every_frame() {
        let line = encode_frame(&f).expect("encodes");
        assert!(line.len() <= LINE_MAX, "{line}");
        assert!(line.is_ascii() && !line.contains('\n'), "{line}");
        assert_eq!(decode_line(line.as_bytes()), Ok(host_copy(&f)), "{line}");
    }
    assert_eq!(encode_frame(&Frame::Unknown), None);
}

#[test]
fn a_torn_line_glued_on_the_next_never_decodes() {
    let frames = every_frame();
    let lines: Vec<String> = frames
        .iter()
        .map(|f| encode_frame(f).expect("encodes"))
        .collect();
    for a in &lines {
        for b in &lines {
            for k in 1..a.len() {
                let glued = format!("{}{b}", &a[..k]);
                assert!(
                    decode_line(glued.as_bytes()).is_err(),
                    "a tear at {k} of {a:?} glued onto {b:?} decoded"
                );
                // The writer abandons a segment after a short write, so a
                // torn tail is a segment's last bytes: never a line.
                let tail = format!("{b}\n{}", &a[..k]);
                let whole: Vec<&[u8]> = complete_lines(tail.as_bytes()).collect();
                assert_eq!(whole, vec![b.as_bytes()], "{tail:?}");
                // And a tear followed by a line of its own decodes alone.
                let split = format!("{}\n{b}\n", &a[..k]);
                let got: Vec<&[u8]> = complete_lines(split.as_bytes()).collect();
                assert_eq!(got.len(), 2);
                assert!(decode_line(got[1]).is_ok(), "{b}");
            }
        }
    }
}

#[test]
fn the_decoder_refuses_what_is_not_a_line() {
    let good = encode_frame(&every_frame()[2]).expect("encodes");
    assert!(decode_line(good.as_bytes()).is_ok());
    let long = format!("{}{}", good, "x".repeat(LINE_MAX));
    assert_eq!(decode_line(long.as_bytes()), Err(LineError::TooLong));
    let utf = good.replace("why=", "wh\u{e9}=");
    assert_eq!(decode_line(utf.as_bytes()), Err(LineError::NotAscii));
    let cr = format!("{good}\r");
    assert!(decode_line(cr.as_bytes()).is_err());
    assert_eq!(
        decode_line(good.replace("s1\t", "s2\t").as_bytes()),
        Err(LineError::NotS1)
    );
    assert_eq!(
        decode_line(good.trim_end_matches("end=").as_bytes()),
        Err(LineError::NoEnd)
    );
    let twice = good.replace("\tend=", "\tend=\tend=");
    assert_eq!(decode_line(twice.as_bytes()), Err(LineError::Glued));
    // Unknown keys and kinds are skipped, not refused.
    let extra = good.replace("\tend=", "\tnewer_key=7\tend=");
    assert_eq!(
        decode_line(extra.as_bytes()),
        decode_line(good.as_bytes()),
        "an unknown key is skipped"
    );
    assert_eq!(
        decode_line(b"s1\ts-9f3c0a\t#newer\tk=1\tend="),
        Ok(Frame::Unknown)
    );
    assert_eq!(decode_line(b"s1\t*\t#newer\tend="), Ok(Frame::Unknown));
    // A record with an unknown key keeps the rest.
    let rec = format!("s1\ts-9f3c0a\t{} future=1\tend=", lines(&sample())[1]);
    assert!(matches!(decode_line(rec.as_bytes()), Ok(Frame::Rec { .. })));
    // A complete line is one with its LF.
    let bytes = format!("{good}\n{good}");
    assert_eq!(complete_lines(bytes.as_bytes()).count(), 1);
    assert_eq!(complete_lines(b"").count(), 0);
    assert_eq!(complete_lines(b"\n").count(), 1);
    assert_eq!(line_subject(good.as_bytes()), Some(&b"s-9f3c0a"[..]));
    let fin = encode_frame(&Frame::End {
        why: EndWhy::Quit,
        t: 1,
    })
    .expect("encodes");
    assert!(is_end_line(fin.as_bytes()));
    assert!(!is_end_line(good.as_bytes()));
}

#[test]
fn segment_and_lock_names_are_exactly_the_writers() {
    let i = inst();
    assert_eq!(
        parse_segment_name(&segment_name(&i, 1)),
        Some((i.clone(), 1))
    );
    assert_eq!(
        parse_segment_name(&segment_name(&i, u32::MAX)),
        Some((i.clone(), u32::MAX))
    );
    assert_eq!(parse_lock_name(&lock_name(&i)), Some(i.clone()));
    let decimal = Inst::new("p7-1760600000123456789").expect("a nanosecond nonce");
    assert_eq!(
        parse_segment_name("p7-1760600000123456789.2.slog"),
        Some((decimal, 2))
    );
    for hostile in [
        "notes.txt",
        "x.1.slog.bak",
        "P1-aa.1.slog",
        "p1-AA.1.slog",
        "p1-aa.01.slog",
        "p1-aa.0.slog",
        "p1-aa.-1.slog",
        "p1-aa.+1.slog",
        ".p1-aa.1.slog.tmp",
        "p1-aa.1.slog/",
        "p1-aa.1.slog\0",
        "p1-aa.4294967296.slog",
        "p1-aa..slog",
        "p1-aa.1",
        "p-aa.1.slog",
        "p12345678901-aa.1.slog",
        "p1-.1.slog",
        "p1-aa\u{301}.1.slog",
        "p1-aa.1.slogx",
        "p1-aa.1.SLOG",
        "prune.lock",
        "../p1-aa.1.slog",
    ] {
        assert_eq!(parse_segment_name(hostile), None, "{hostile:?}");
    }
    for hostile in [
        "p1-aa.lock.pending",
        ".p1-aa.lock",
        "p1-aa.Lock",
        "prune.lock",
    ] {
        assert_eq!(parse_lock_name(hostile), None, "{hostile:?}");
    }
}

#[test]
fn the_content_tier_never_reaches_a_line() {
    let mut log = Log::new(1);
    let _ = crate::support::agent_session(&mut log, 0);
    let f = Fields::new()
        .text(Key::Title, "SECRET-TITLE-7")
        .expect("title is a text key");
    log.mark(
        Mark::new(Kind::Title, Src::Status, Conf::Exact).fields(f),
        at(1),
    )
    .expect("a title mark");
    let rec = log.window().last().expect("the mark").clone();
    let line = encode_frame(&Frame::Rec {
        sid: sid(),
        rec: rec.clone(),
        restated: false,
    })
    .expect("encodes");
    assert!(!line.contains("SECRET-TITLE"), "{line}");
    assert!(
        line.contains(" title src="),
        "the record stays, its title gone: {line}"
    );
    // NEGATIVE CONTROL: with the title mistiered as host, the same encode
    // carries it.
    let mistiered: Vec<(Key, Tier, Cap)> = FIELDS
        .iter()
        .map(|&(k, t, c)| (k, if k == Key::Title { Tier::Host } else { t }, c))
        .collect();
    let leaked = encode_with_table(&rec, Tier::Host, &mistiered).expect("fits");
    assert!(leaked.contains("SECRET-TITLE-7"), "{leaked}");
    let _ = Node::SESSION;
}
