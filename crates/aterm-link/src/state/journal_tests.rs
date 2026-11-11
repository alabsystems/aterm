// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::state::PublishIdentity;
use astream_wire::steps::encode_step;

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
        at: Some(40 + seq),
    }
}

fn entry(seq: u64) -> InFlight {
    InFlight {
        pin: pin(seq),
        subject: "/f/sim/in/n-1/s-b/n-1/s-a/note".to_string(),
        body: b"v=1 t=5 text=hello".to_vec(),
        log: astream_broker::LogIdentity::parse(&format!("{}.3", "5a".repeat(16))),
        at: Some(50 + seq),
    }
}

fn journal(frames: &[Frame]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for (step, frame) in frames.iter().enumerate() {
        bytes.extend(encode(step as u64, frame).expect("frames"));
    }
    bytes
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
    expected.in_flight.insert(8, entry(8));
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
    for tail in ["at", "at x", "at 1 at 2", "pin-at 3 later 4", "\u{ff}"] {
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
    expected.in_flight.insert(7, entry(7));
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
