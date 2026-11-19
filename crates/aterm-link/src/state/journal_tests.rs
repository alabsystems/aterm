// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::state::PublishIdentity;
use astream_wire::steps::encode_step;

/// A reservation's pin at `seq`, its head at `40 + seq`; but sequence 8's is anywhere (a log that
/// continues none of its own, ATERM-KI-1114-1414), so every round trip reads one back.
fn pin(seq: u64) -> PublishPin {
    PublishPin {
        seq,
        via_key: false,
        identity: Some(PublishIdentity {
            subject: "/f/sim/in/n-1/s-b/n-1/s-a/note".to_string(),
            digest: "a".repeat(64),
        }),
        original: None,
        original_off: None,
        owner: Some(4),
        at: Some(if seq == 8 {
            Head::Anywhere
        } else {
            Head::From(40 + seq)
        }),
    }
}

fn entry(seq: u64) -> InFlight {
    InFlight {
        pin: pin(seq),
        subject: "/f/sim/in/n-1/s-b/n-1/s-a/note".to_string(),
        body: b"v=1 t=5 text=hello".to_vec(),
        log: astream_broker::LogIdentity::parse(&format!("{}.3", "5a".repeat(16))),
        at: Some(Head::From(50 + seq)),
        own_log: None,
    }
}

fn journal(frames: &[Frame]) -> Vec<u8> {
    encode_frames(0, frames).expect("frames").0
}

fn every_kind() -> Vec<Frame> {
    vec![
        Frame::Attached(11),
        Frame::Attached(13),
        Frame::Attached(12),
        Frame::Reserve {
            sid: "s-a".to_string(),
            id: 1,
            pin: pin(7),
        },
        Frame::InFlight(entry(7)),
        Frame::Reserve {
            sid: "s-a".to_string(),
            id: 2,
            pin: pin(8),
        },
        Frame::InFlight(entry(8)),
        Frame::Landed(7),
        Frame::Retire {
            sid: "s-a".to_string(),
            id: 1,
        },
        Frame::InFlight(InFlight {
            log: None,
            ..entry(9)
        }),
        Frame::Dropped(9),
    ]
}

/// Every kind of record reads back as written, the index is what they say in order, and a
/// rewrite from the index reads back to the same index.
#[test]
fn every_record_round_trips_and_a_rewrite_holds_the_same_index() {
    let frames = every_kind();
    for frame in &frames {
        assert_eq!(
            Frame::of_record(frame.body().as_bytes(), frame.tail().as_bytes()).as_ref(),
            Some(frame)
        );
    }
    let bytes = journal(&frames);
    let read = read(&bytes);
    assert_eq!(read.end, End::Clean);
    assert_eq!(read.valid_len, bytes.len() as u64);
    assert_eq!(read.next_step, frames.len() as u64);
    let mut expected = Index::default();
    expected.apply(Frame::Reserve {
        sid: "s-a".to_string(),
        id: 2,
        pin: pin(8),
    });
    expected.apply(Frame::InFlight(entry(8)));
    expected.attached = Some(13);
    assert_eq!(read.index, expected);
    let (rewritten, steps) = read.index.encode().expect("rewrite");
    assert_eq!(
        steps, 3,
        "the newest attach, a reservation and a record in flight"
    );
    let again = super::read(&rewritten);
    assert_eq!((again.index, again.end), (expected, End::Clean));
    assert!(
        rewritten.len() < bytes.len(),
        "the rewrite holds only what is live"
    );
}

/// The read of a build of the journal before the heads (B19's, on aterm main at 19f6a3e034):
/// each record's body by astream's step scan, parsed strictly, its tail never looked at.
fn read_as_before_the_heads(bytes: &[u8]) -> Option<Index> {
    let scan = scan_steps_from(bytes, 0);
    let mut index = Index::default();
    for (_, body) in &scan.steps {
        index.apply(Frame::parse(bytes.get(body.clone())?)?);
    }
    (scan.end == StepEnd::Clean).then_some(index)
}

/// **A JOURNAL THIS BUILD WROTE READS, RECORD FOR RECORD, TO A BUILD BEFORE THE HEADS**
/// (aterm-link B05 review): the heads ride in each record's tail, which astream's step scan
/// leaves out of the body, so the strict parse of an earlier build reads every record as the
/// one written, without its heads, and a rewrite from the index too; the text form never
/// names a head. CONTROL: this build reads the heads back from the same bytes.
#[test]
fn a_journal_with_heads_reads_record_for_record_to_a_build_before_them() {
    let frames = every_kind();
    let headless = |frame: &Frame| match frame {
        Frame::Reserve { sid, id, pin } => Frame::Reserve {
            sid: sid.clone(),
            id: *id,
            pin: pin.without_head(),
        },
        Frame::InFlight(entry) => Frame::InFlight(entry.without_head()),
        other => other.clone(),
    };
    assert!(frames.iter().any(|frame| *frame != headless(frame)));
    for frame in &frames {
        assert_eq!(
            Frame::parse(frame.body().as_bytes()),
            Some(headless(frame)),
            "an earlier build's parse of the body alone"
        );
        assert!(!frame.body().contains(" at ") && !frame.body().contains("\nat="));
    }
    assert_eq!(
        pin(7).render(),
        format!(
            "7 identity /f/sim/in/n-1/s-b/n-1/s-a/note {} owner 4",
            "a".repeat(64)
        ),
        "the reservation's text form is B19's"
    );
    let bytes = journal(&frames);
    let mut headless_index = Index::default();
    for frame in &frames {
        headless_index.apply(headless(frame));
    }
    assert_eq!(
        read_as_before_the_heads(&bytes),
        Some(headless_index.clone())
    );
    let (rewritten, _) = read(&bytes).index.encode().expect("rewrite");
    assert_eq!(
        read_as_before_the_heads(&rewritten),
        Some(headless_index),
        "a rewrite from the index, without its heads"
    );
    let mut with_heads = Index::default();
    for frame in &frames {
        with_heads.apply(frame.clone());
    }
    assert_eq!(
        read(&bytes).index,
        with_heads,
        "CONTROL: this build reads the heads"
    );
    assert_eq!(read(&rewritten).index, with_heads);
}

/// A tail this build cannot read names no head: the record reads as its body alone.
#[test]
fn a_tail_this_build_cannot_read_names_no_head() {
    let entry = Frame::InFlight(entry(7));
    for tail in [
        "at",
        "at x",
        "at Any",
        "at any any",
        "at own",
        "at Own4",
        "at own+4",
        "at met",
        "at Met4",
        "at met+4",
        "at metown4",
        "at 1 at 2",
        "pin-at 3 later 4",
        "\u{ff}",
    ] {
        assert_eq!(
            Frame::of_record(entry.body().as_bytes(), tail.as_bytes()),
            Frame::parse(entry.body().as_bytes()),
            "{tail:?}"
        );
    }
    assert_eq!(
        Frame::of_record(entry.body().as_bytes(), b"at 57 pin-at 47"),
        Some(entry),
        "CONTROL"
    );
}

/// **A HEAD ANYWHERE IS `any`** (ATERM-KI-1114-1414): a record's head and its pin's each read
/// back as written, alone or beside a number, and the journal encodes them; a build before it
/// reads `any` as it reads any tail it cannot read, as no head (the record published again at
/// its pin, as every build before this one published such a record).
#[test]
fn a_head_anywhere_reads_back_as_any() {
    let anywhere = InFlight {
        at: Some(Head::Anywhere),
        ..entry(8)
    };
    for (entry, tail) in [
        (entry(8), "at 58 pin-at any"),
        (anywhere.clone(), "at any pin-at any"),
        (
            InFlight {
                pin: pin(7),
                ..anywhere
            },
            "at any pin-at 47",
        ),
    ] {
        let frame = Frame::InFlight(entry);
        assert_eq!(frame.tail(), tail);
        assert_eq!(
            Frame::of_record(frame.body().as_bytes(), tail.as_bytes()).as_ref(),
            Some(&frame),
            "{tail}"
        );
        encode(0, &frame).expect("the journal encodes it");
    }
    let reserve = Frame::Reserve {
        sid: "s-a".to_string(),
        id: 2,
        pin: pin(8),
    };
    assert_eq!(reserve.tail(), "at any");
    assert_eq!(
        Frame::of_record(reserve.body().as_bytes(), b"at any").as_ref(),
        Some(&reserve)
    );
}

/// **A KEYED RE-POST'S OWN HEAD IS `own<n>`** (ATERM-KI-1114-1414, `Head::OwnFrom`): a record's
/// head and its pin's each read back as written, beside a number or `any`, and the journal
/// encodes them. A build before it reads one as no head, as it reads `any`: its `Head::of_word`
/// takes `any` and a number of digits and nothing else, and a tail with a word it cannot read
/// names no head at all (the record published again at its pin, as every build before this one
/// published such a record).
#[test]
fn a_keyed_own_head_reads_back_as_own() {
    let own = InFlight {
        at: Some(Head::OwnFrom(61)),
        ..entry(7)
    };
    for (entry, tail) in [
        (own.clone(), "at own61 pin-at 47"),
        (
            InFlight {
                pin: PublishPin {
                    at: Some(Head::OwnFrom(0)),
                    ..pin(8)
                },
                ..own.clone()
            },
            "at own61 pin-at own0",
        ),
        (
            InFlight {
                at: Some(Head::From(0)),
                ..own.clone()
            },
            "at 0 pin-at 47",
        ),
        (InFlight { pin: pin(8), ..own }, "at own61 pin-at any"),
    ] {
        let frame = Frame::InFlight(entry);
        assert_eq!(frame.tail(), tail);
        assert_eq!(
            Frame::of_record(frame.body().as_bytes(), tail.as_bytes()).as_ref(),
            Some(&frame),
            "{tail}"
        );
        encode(0, &frame).expect("the journal encodes it");
    }
    let reserve = Frame::Reserve {
        sid: "s-a".to_string(),
        id: 2,
        pin: PublishPin {
            at: Some(Head::OwnFrom(90)),
            ..pin(7)
        },
    };
    assert_eq!(reserve.tail(), "at own90");
    assert_eq!(
        Frame::of_record(reserve.body().as_bytes(), b"at own90").as_ref(),
        Some(&reserve)
    );
    assert_eq!(Head::of_word("own90"), Some(Head::OwnFrom(90)));
    assert_eq!(
        Head::of_word("met90"),
        None,
        "a word this build never wrote"
    );
    assert_eq!(
        super::number("own90"),
        None,
        "a number of digits, the only head a build before `any` reads"
    );
}

/// The tail grammar of every build of the heads before ATERM-KI-1114-1414's step S6, copied from
/// main at 928adfca967 (its `Frame::with_tail`): `at` and `pin-at`, each once, each a head
/// `Head::of_word` reads (`any`, `own<n>`, digits: unchanged since), and any other pair names no
/// head at all.
fn with_main_tail(mut frame: Frame, tail: &[u8]) -> Frame {
    let mut at = None;
    let mut pin_at = None;
    let words: Option<Vec<&str>> = std::str::from_utf8(tail)
        .ok()
        .map(|text| text.split_ascii_whitespace().collect());
    for pair in words.as_deref().unwrap_or_default().chunks(2) {
        let (slot, value) = match pair {
            ["at", n] if at.is_none() => (&mut at, n),
            ["pin-at", n] if pin_at.is_none() => (&mut pin_at, n),
            _ => return frame,
        };
        let Some(value) = Head::of_word(value) else {
            return frame;
        };
        *slot = Some(value);
    }
    match &mut frame {
        Frame::Reserve { pin, .. } if pin_at.is_none() => pin.at = at,
        Frame::InFlight(entry) => {
            entry.at = at;
            entry.pin.at = pin_at;
        }
        _ => {}
    }
    frame
}

/// A record as main at 928adfca967 reads it: its body by the strict parse (`Frame::parse`, which
/// main shares but for this build's refusal of a reservation under [`OWN_LOG`]), its tail by
/// [`with_main_tail`].
#[derive(Debug, PartialEq)]
struct MainRecord(Frame);

impl Record for MainRecord {
    const JOURNAL: &'static str = "post journal";

    fn body(&self) -> String {
        self.0.body()
    }

    fn tail(&self) -> String {
        self.0.tail()
    }

    fn of_record(body: &[u8], tail: &[u8]) -> Option<Self> {
        Some(Self(with_main_tail(Frame::parse(body)?, tail)))
    }
}

/// What main at 928adfca967 reads a post journal as: its records ([`MainRecord`]) applied in
/// order by its index, this build's but for the own log's record, which no frame main parses is,
/// and with no fact begun across two records (`Replay::begun`, which main's read lacks).
#[derive(Debug, Default)]
struct MainIndex(Index);

impl Replay for MainIndex {
    type Record = MainRecord;

    fn apply(&mut self, record: MainRecord) {
        self.0.apply(record.0);
    }
}

/// A keyed re-post's entry moved off its own log `77…77.2`, at its heads there.
fn moved(seq: u64) -> (InFlight, OwnLog) {
    let log = astream_broker::LogIdentity::parse(&format!("{}.2", "77".repeat(16))).expect("a log");
    let own = OwnLog {
        log,
        at: Head::OwnFrom(60),
        pin_at: Some(Head::OwnFrom(61)),
    };
    let entry = InFlight {
        own_log: Some(own),
        ..entry(seq)
    };
    (entry, own)
}

/// **AN ENTRY'S OWN LOG IS A RECORD OF ITS OWN, WHICH MAIN READS BESIDE EVERY HEAD OF THE ENTRY**
/// (ATERM-KI-1114-1414's step S6, `InFlight::own_log`, `Frame::OwnLog`; the step's third review,
/// confirmed): an entry that keeps an own log is written as `retire own-log <seq>`, its tail
/// `log <id> at <n>` and `pin-at <n>` where its pin had a head there, and then the entry's own
/// record with its heads alone. This build reads both back as the entry with its own log, and so
/// does a rewrite from its index. Main (928adfca967, steps S1 to S5), which reads a tail by its
/// own grammar ([`with_main_tail`], copied), reads the first as a retirement of no post, which
/// changes nothing, and the entry with every head it reads today; a rewrite from main's index
/// drops the own log, never a head, and this build then reads the entry where main left it. A
/// build before the heads (B19) reads it record for record without heads, as every record.
/// CONTROL: the third build of the step wrote the own log in the entry's own tail, which main's
/// grammar reads as no head at all.
#[test]
fn an_entrys_own_log_is_a_record_main_reads_beside_every_head_of_the_entry() {
    let (moved, own) = moved(7);
    let id = own.log.to_string();
    let heads = InFlight {
        own_log: None,
        ..moved.clone()
    };
    let reserve = Frame::Reserve {
        sid: "s-a".to_string(),
        id: 2,
        pin: pin(7),
    };
    let frames = vec![
        Frame::Attached(3),
        reserve.clone(),
        Frame::InFlight(moved.clone()),
    ];
    let (bytes, steps) = encode_frames(0, &frames).expect("frames");
    let mut on_main = Index::default();
    for frame in [Frame::Attached(3), reserve, Frame::InFlight(heads.clone())] {
        on_main.apply(frame);
    }
    let main = read_as::<MainIndex>(&bytes);
    assert_eq!(main.end, End::Clean);
    assert_eq!(
        main.index.0, on_main,
        "main reads the reservation's head and the entry's heads, and the own log changes nothing"
    );
    let mut index = Index::default();
    for frame in &frames {
        index.apply(frame.clone());
    }
    let read = read(&bytes);
    assert_eq!(
        (read.end.clone(), read.valid_len, read.next_step),
        (End::Clean, bytes.len() as u64, steps)
    );
    assert_eq!(read.index, index, "this build reads the own log back");
    assert_eq!(read.index.in_flight.get(&7), Some(&moved));
    let (rewritten, _) = read.index.encode().expect("rewrite");
    assert_eq!(super::read(&rewritten).index, index, "this build's rewrite");
    assert_eq!(read_as::<MainIndex>(&rewritten).index.0, on_main);
    let (mains, _) = main.index.0.encode().expect("main's rewrite");
    assert_eq!(
        super::read(&mains).index,
        on_main,
        "main's rewrite drops the own log, never a head"
    );
    let mut headless = Index::default();
    for frame in &frames {
        headless.apply(match frame {
            Frame::InFlight(entry) => Frame::InFlight(entry.without_head()),
            Frame::Reserve { sid, id, pin } => Frame::Reserve {
                sid: sid.clone(),
                id: *id,
                pin: pin.without_head(),
            },
            other => other.clone(),
        });
    }
    assert_eq!(
        read_as_before_the_heads(&bytes),
        Some(headless),
        "a build before the heads reads it record for record"
    );
    let records = Frame::InFlight(moved.clone()).records();
    assert_eq!(
        records,
        vec![
            Frame::OwnLog { seq: 7, own },
            Frame::InFlight(heads.clone())
        ],
        "the own log's record, then the entry's"
    );
    assert_eq!(
        steps, 4,
        "the attach, the reservation, the own log, the entry"
    );
    assert_eq!(records[0].body(), "retire own-log 7");
    assert_eq!(records[0].tail(), format!("log {id} at own60 pin-at own61"));
    assert_eq!(
        records[1].tail(),
        "at 57 pin-at 47",
        "the entry's heads alone"
    );
    let bare = Frame::OwnLog {
        seq: 7,
        own: OwnLog {
            at: Head::From(9),
            pin_at: None,
            ..own
        },
    };
    assert_eq!(bare.tail(), format!("log {id} at 9"));
    for record in records.iter().chain([&bare]) {
        assert_eq!(
            Frame::of_record(record.body().as_bytes(), record.tail().as_bytes()).as_ref(),
            Some(record),
            "{record:?}"
        );
        encode(0, record).expect("the journal encodes it");
    }
    assert_eq!(
        encode(0, &Frame::InFlight(moved.clone()))
            .expect_err("an own log in the entry's record")
            .kind(),
        io::ErrorKind::InvalidInput,
        "an entry's record never carries its own log"
    );
    let third = format!("at 57 pin-at 47 own-log {id} own-at own60 own-pin-at own61");
    let body = records[1].body();
    assert_eq!(
        MainRecord::of_record(body.as_bytes(), third.as_bytes()).map(|record| record.0),
        Some(Frame::InFlight(heads.without_head())),
        "CONTROL: the third build's own log in the entry's tail, which main reads with no head"
    );
}

/// **AN OWN LOG'S RECORD NAMES THE OWN LOG OF THE ENTRY RIGHT AFTER IT, OR NONE**
/// (`Frame::OwnLog`, `Index::apply`, `read_as`): written in one append with its entry's record
/// after it, a journal that ends at it ends in that append cut short (`End::Cut`, the run ending
/// where it starts, so the next write rewrites the journal without it), and so does one cut inside
/// the entry's record; one followed by another record (main appending after an append cut short
/// there), or by the entry on the own log itself, names none. A retirement of `own-log` whose tail
/// names no own log is that retirement, of no post; a reservation under `own-log` is refused.
/// CONTROL: the whole pair names the own log.
#[test]
fn an_own_logs_record_names_the_own_log_of_the_entry_right_after_it_or_none() {
    let (moved, own) = moved(7);
    let id = own.log.to_string();
    let (before, at) = encode_frames(0, &[Frame::InFlight(entry(7))]).expect("an entry");
    let (pair, _) = encode_frames(at, &[Frame::InFlight(moved.clone())]).expect("its move");
    let own_record = encode(at, &Frame::OwnLog { seq: 7, own })
        .expect("the own log's record")
        .len();
    let mut standing = Index::default();
    standing.apply(Frame::InFlight(entry(7)));
    for (cut, what) in [
        (own_record, "at the own log's record's end"),
        (pair.len() - 1, "inside the entry's record"),
    ] {
        let mut bytes = before.clone();
        bytes.extend_from_slice(&pair[..cut]);
        let read = read(&bytes);
        assert_eq!(
            (read.end, read.valid_len, read.next_step),
            (End::Cut, before.len() as u64, at),
            "{what}: the append cut short"
        );
        assert_eq!(read.index, standing, "{what}: the entry as it stood");
    }
    let mut whole = before.clone();
    whole.extend_from_slice(&pair);
    assert_eq!(
        read(&whole).index.in_flight.get(&7),
        Some(&moved),
        "CONTROL: the whole pair"
    );
    let on_its_own_log = InFlight {
        log: Some(own.log),
        own_log: None,
        ..entry(7)
    };
    for (next, seq) in [
        (Frame::InFlight(entry(8)), 8),
        (Frame::Landed(9), 7),
        (Frame::InFlight(on_its_own_log), 7),
    ] {
        let mut bytes = before.clone();
        bytes.extend(encode(at, &Frame::OwnLog { seq: 7, own }).expect("the own log"));
        bytes.extend(encode(at + 1, &next).expect("the record after it"));
        let read = read(&bytes);
        assert_eq!(read.end, End::Clean, "{next:?}");
        assert!(
            read.index.in_flight.values().all(|e| e.own_log.is_none()),
            "{next:?}: no own log"
        );
        assert!(read.index.in_flight.contains_key(&seq), "{next:?}");
    }
    let retire = |tail: &str| Frame::of_record(b"retire own-log 7", tail.as_bytes());
    for tail in [
        String::new(),
        format!("log {id}"),
        "at own60".to_string(),
        format!("log {id} at own60 at 9"),
        format!("log {id} log {id} at own60"),
        format!("log {id} at met60"),
        "log x at own60".to_string(),
        format!("log {id} at own60 later 4"),
    ] {
        assert_eq!(
            retire(&tail),
            Some(Frame::Retire {
                sid: OWN_LOG.to_string(),
                id: 7
            }),
            "{tail:?}"
        );
    }
    assert_eq!(
        retire(&format!("log {id} at own60 pin-at own61")),
        Some(Frame::OwnLog { seq: 7, own }),
        "CONTROL"
    );
    assert!(
        encode(
            0,
            &Frame::Reserve {
                sid: OWN_LOG.to_string(),
                id: 1,
                pin: pin(7),
            }
        )
        .is_err(),
        "no post is reserved under own-log"
    );
}

/// A cut anywhere in the last record keeps every record before it and reads as that record
/// cut short; a broken last record that is not one cut short is torn; damage before a whole
/// record is a fault. CONTROL: an intact journal is clean at every record boundary.
#[test]
fn a_cut_last_record_is_cut_a_broken_one_torn_and_damage_before_a_whole_one_a_fault() {
    let frames = every_kind();
    let bytes = journal(&frames);
    let last_start = journal(&frames[..frames.len() - 1]).len();
    let before = super::read(&bytes[..last_start]);
    assert_eq!(before.end, End::Clean, "CONTROL: a boundary is clean");
    for cut in last_start + 1..bytes.len() {
        let read = super::read(&bytes[..cut]);
        assert_eq!(read.end, End::Cut, "cut at {cut}");
        assert_eq!(read.index, before.index, "cut at {cut}");
        assert_eq!(read.valid_len, last_start as u64);
    }
    for at in [last_start, last_start + 2, last_start + 13, bytes.len() - 1] {
        let mut broken = bytes.clone();
        broken[at] ^= 0x01;
        let read = super::read(&broken);
        assert_eq!(read.end, End::Torn, "the last record broken at {at}");
        assert_eq!(read.index, before.index);
    }
    let mut wrong_step = journal(&frames[..frames.len() - 1]);
    let next = encode(frames.len() as u64, frames.last().expect("frames")).expect("frames");
    wrong_step.extend_from_slice(&next[..next.len() - 1]);
    assert_eq!(
        super::read(&wrong_step).end,
        End::Torn,
        "a cut record that names another step is not this one cut short"
    );
    assert_eq!(super::read(b"not a journal entry\n").end, End::Torn);
    let mut damaged = bytes.clone();
    damaged[20] ^= 0x40;
    assert!(
        matches!(super::read(&damaged).end, End::Fault { at: 0, .. }),
        "a flipped byte in the first record, whole records after it"
    );
    let mut foreign = journal(&frames[..2]);
    let at = foreign.len() as u64;
    foreign.extend(encode_step(2, b"unknown 5").expect("frames"));
    let read = super::read(&foreign);
    assert_eq!(read.valid_len, at);
    assert!(
        matches!(read.end, End::Fault { at: a, .. } if a == at),
        "a record this build did not write: {:?}",
        read.end
    );
    let mut out_of_order = journal(&frames[..2]);
    out_of_order.extend(encode(5, &frames[2]).expect("frames"));
    assert!(matches!(super::read(&out_of_order).end, End::Fault { .. }));
}

/// A record's body is text, so a session's text that holds the frame magic cannot make a cut
/// read as a fault: the body is hex.
#[test]
fn a_body_holding_a_whole_frame_still_reads_cut_when_cut() {
    let nested = encode(0, &Frame::Landed(3)).expect("frames");
    let frame = Frame::InFlight(InFlight {
        body: nested,
        ..entry(11)
    });
    let bytes = journal(std::slice::from_ref(&frame));
    for cut in 1..bytes.len() {
        assert_eq!(super::read(&bytes[..cut]).end, End::Cut, "cut at {cut}");
    }
    assert!(!frame.body().bytes().any(|b| b >= 0x80));
}

/// THE SWEEP'S SALVAGE: damage before whole records keeps every whole record after it, in
/// order, and loses only what the damaged record said (here a landing, so its record in
/// flight stays); an intact record whose body this build does not read is passed over the
/// same way. CONTROL: an intact journal, and one whose last record is cut, salvage to what
/// their read keeps.
#[test]
fn a_salvage_keeps_every_whole_record_after_damage() {
    let frames = every_kind();
    let bytes = journal(&frames);
    let whole = super::read(&bytes).index;
    assert_eq!(salvage(&bytes), whole, "CONTROL: an intact journal");
    let cut = &bytes[..bytes.len() - 3];
    assert_eq!(
        salvage(cut),
        super::read(cut).index,
        "CONTROL: a cut last record"
    );
    let landed = frames
        .iter()
        .position(|f| *f == Frame::Landed(7))
        .expect("a landing");
    let start = journal(&frames[..landed]).len();
    let mut damaged = bytes.clone();
    damaged[start + HEADER_SIZE + STEP_HEADER + 2] ^= 0x01;
    let read = super::read(&damaged);
    assert!(matches!(read.end, End::Fault { .. }), "{:?}", read.end);
    assert!(
        read.index.pin("s-a", 1).is_some(),
        "the read stops at the damage, before the retirement after it"
    );
    let mut expected = whole.clone();
    expected.apply(Frame::InFlight(entry(7)));
    expected.in_flight_order = vec![7, 8];
    assert_eq!(salvage(&damaged), expected);
    let mut first = bytes.clone();
    first[20] ^= 0x40;
    assert!(matches!(super::read(&first).end, End::Fault { at: 0, .. }));
    assert_eq!(
        salvage(&first),
        whole,
        "the first record's attach is below a later one"
    );
    let mut foreign = journal(&frames[..2]);
    foreign.extend(encode_step(2, b"unknown 5").expect("frames"));
    for (step, frame) in frames[2..].iter().enumerate() {
        foreign.extend(encode(step as u64 + 3, frame).expect("frames"));
    }
    assert!(matches!(super::read(&foreign).end, End::Fault { .. }));
    assert_eq!(salvage(&foreign), whole);
}

/// THE SALVAGE'S SEARCH PAST DAMAGE IS BOUNDED: damage that reads as a frame header every
/// twelve bytes, each naming a length to near the end, spends the search's CRC budget, and
/// the salvage keeps what it found before the damage rather than hash every candidate's
/// length (a search that would cost about the square of the bytes). CONTROL: the same whole
/// record after a few such headers is found.
#[test]
fn a_salvage_search_past_damage_is_bounded() {
    use astream_wire::frame::{MAGIC, VERSION};
    let frames = every_kind();
    let before = journal(&frames[..4]);
    let after = encode(9, &frames[4]).expect("a record");
    let lookalikes = |n: usize, total: usize| {
        let mut bytes = Vec::new();
        for k in 0..n {
            let len = (total - 12 * (k + 1)) as u32;
            bytes.extend_from_slice(&[MAGIC[0], MAGIC[1], VERSION, 0]);
            bytes.extend_from_slice(&len.to_le_bytes());
            bytes.extend_from_slice(&[0, 0, 0, 0]);
        }
        bytes
    };
    let salvaged = |n: usize, total: usize| {
        let mut bytes = before.clone();
        bytes.extend(lookalikes(n, total));
        bytes.resize(before.len() + total, b' ');
        bytes.extend_from_slice(&after);
        salvage(&bytes)
    };
    let prefix = super::read(&before).index;
    let mut found = prefix.clone();
    found.apply(frames[4].clone());
    assert_eq!(
        salvaged(3, 4096),
        found,
        "CONTROL: a few look-alikes passed over"
    );
    let total = 2 * 1024 * 1024;
    assert_eq!(
        salvaged(total / 12 - 1, total),
        prefix,
        "the budget spent, what came before the damage kept"
    );
}

#[test]
fn publication_order_survives_replacement_removal_and_compaction() {
    let mut index = Index::default();
    index.apply(Frame::InFlight(entry(90)));
    index.apply(Frame::InFlight(entry(2)));
    index.apply(Frame::InFlight(InFlight {
        at: Some(Head::From(100)),
        body: b"v=1 t=99 text=hello".to_vec(),
        ..entry(90)
    }));
    let sequences = |index: &Index| {
        index
            .in_publication_order()
            .map(|(seq, _)| seq)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        sequences(&index),
        [90, 2],
        "a head and timestamp update keeps publication order"
    );
    let compacted = read(&index.encode_from(0).unwrap().0).index;
    assert_eq!(
        sequences(&compacted),
        [90, 2],
        "compaction keeps publication order"
    );
    let mut replacement = entry(90);
    replacement.body = b"a later keyed post".to_vec();
    replacement.pin.identity.as_mut().unwrap().digest = "b".repeat(64);
    index.apply(Frame::InFlight(replacement));
    assert_eq!(
        sequences(&index),
        [2, 90],
        "new words are a new publication"
    );
    index.apply(Frame::Landed(2));
    index.apply(Frame::InFlight(entry(1)));
    assert_eq!(sequences(&index), [90, 1]);
    assert_eq!(read(&index.encode_from(0).unwrap().0).index, index);
}
