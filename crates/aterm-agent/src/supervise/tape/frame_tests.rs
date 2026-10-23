// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Frame encoding, recovery, and crash-truncation tests for supervisor tapes.

use std::io::{ErrorKind, Write};
use std::sync::{Arc, Mutex, PoisonError};

use astream_effects::EffectRecord;
use aterm_spec::rng::XorShift;

use super::codec::{End, Item, Mark, Stamp, ctl_request};
use super::doors;
use super::frame::{
    FRAME_RAW, Format, FrameSink, HEAD, Hint, Kind, MAGIC, PLACES, Stopped, TapeWriter, checksum,
    frame_bytes, place_named, place_word,
};
use super::recover::{Recovered, Tail, Why, recover, salvage};

/// What a [`Memory`] sink was handed.
#[derive(Debug, Default)]
pub(crate) struct Taken {
    /// Every byte written, in order.
    pub(crate) bytes: Vec<u8>,
    /// Each write's length.
    pub(crate) writes: Vec<usize>,
    pub(crate) syncs: usize,
    /// Writes after this many fail.
    pub(crate) fail_after: Option<usize>,
    /// The failing write still lands this many bytes first.
    pub(crate) partial: usize,
}

/// A sink in memory that a test reads while the writer holds it.
#[derive(Debug, Clone, Default)]
pub(crate) struct Memory(pub(crate) Arc<Mutex<Taken>>);

impl Memory {
    pub(crate) fn taken(&self) -> std::sync::MutexGuard<'_, Taken> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn bytes(&self) -> Vec<u8> {
        self.taken().bytes.clone()
    }
}

impl FrameSink for Memory {
    fn write_frame(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        let mut t = self.taken();
        if t.fail_after.is_some_and(|n| t.writes.len() >= n) {
            let partial = t.partial.min(bytes.len());
            t.bytes.extend_from_slice(&bytes[..partial]);
            return Err(std::io::Error::from(ErrorKind::PermissionDenied));
        }
        t.bytes.extend_from_slice(bytes);
        t.writes.push(bytes.len());
        Ok(())
    }

    fn sync(&mut self) -> std::io::Result<()> {
        self.taken().syncs += 1;
        Ok(())
    }
}

/// A warning sink a test reads.
#[derive(Debug, Clone, Default)]
pub(crate) struct Said(Arc<Mutex<Vec<u8>>>);

impl Said {
    pub(crate) fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap_or_else(PoisonError::into_inner)).into_owned()
    }
}

impl Write for Said {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The header text the tests write.
pub(super) const HEADER: &str = "atape 1\ntape=t-1\nbuild=g0\nentry=watch\n";

/// A writer on a fresh memory sink, with what it writes and says.
pub(super) fn writer() -> (TapeWriter, Memory, Said) {
    let sink = Memory::default();
    let said = Said::default();
    let w = TapeWriter::start(
        Box::new(sink.clone()),
        "/t/tape.atape",
        HEADER,
        Box::new(said.clone()),
    );
    (w, sink, said)
}

/// A screen read: a reply that repeats most of the last one.
pub(super) fn screen_read(step: usize) -> Item {
    let rows: String = (0..40)
        .map(|r| {
            format!(
                "\"row {r} of the worker's screen{}\",",
                if r == 39 { step } else { 0 }
            )
        })
        .collect();
    Item::stamped(
        EffectRecord::Exchange {
            door: doors::CTL.to_string(),
            request: ctl_request(&["text", "--json", "tail=40"]),
            reply: format!("\0\0{{\"rows\":[{rows}]}}").into_bytes(),
        },
        Stamp {
            mono_ns: 20_000_000_000 * step as u64,
            wall_ms: 1_790_000_000_000 + 20_000 * step as i64,
        },
    )
}

/// An output of `n` bytes that do not compress.
pub(super) fn noise(rng: &mut XorShift, n: usize) -> Item {
    Item::effect(EffectRecord::Emit {
        door: doors::OUT.to_string(),
        bytes: (0..n).map(|_| (rng.next_u64() & 0xff) as u8).collect(),
    })
}

/// A clock read.
pub(super) fn mono(ns: u64) -> Item {
    Item::effect(EffectRecord::Mono(ns))
}

/// Each frame of a tape, read off its heads (an oracle beside the reader).
fn frames_of(bytes: &[u8]) -> Vec<(usize, usize, u8, u32)> {
    let mut out = Vec::new();
    let mut at = MAGIC.len();
    while at + HEAD <= bytes.len() {
        let raw = u32::from_le_bytes(bytes[at + 1..at + 5].try_into().expect("four bytes"));
        let comp = u32::from_le_bytes(bytes[at + 5..at + 9].try_into().expect("four bytes"));
        let end = at + HEAD + if comp == 0 { raw } else { comp } as usize;
        out.push((at, end, bytes[at], comp));
        at = end;
    }
    out
}

/// A tape of a header and `frames` records frames.
fn tape(frames: usize, rng: &mut XorShift) -> (Vec<u8>, Vec<Vec<Item>>) {
    let (mut w, sink, _) = writer();
    let mut per_frame = Vec::new();
    for f in 0..frames {
        let items = vec![mono(f as u64 * 7), screen_read(f), noise(rng, 30 + f)];
        for item in &items {
            w.append(item);
        }
        w.flush_hint(Hint::Act);
        per_frame.push(items);
    }
    drop(w);
    (sink.bytes(), per_frame)
}

/// What a recovery must hold after `whole` records frames.
fn prefix(per_frame: &[Vec<Item>], whole: usize) -> Vec<Item> {
    per_frame[..whole].iter().flatten().cloned().collect()
}

/// A tape's magic, then its whole frames in `order` (the header is frame 0).
pub(super) fn arranged(bytes: &[u8], order: &[usize]) -> Vec<u8> {
    let frames = frames_of(bytes);
    let mut out = bytes[..MAGIC.len()].to_vec();
    for &i in order {
        out.extend_from_slice(&bytes[frames[i].0..frames[i].1]);
    }
    out
}

/// Re-tags a tape as format 1 ([`a_format_1_tape_reads_as_it_always_did`]).
pub(super) fn as_format_1(bytes: &[u8]) -> Vec<u8> {
    let mut out = bytes.to_vec();
    out[..MAGIC.len()].copy_from_slice(&Format::One.magic());
    for (start, end, kind, _) in frames_of(bytes) {
        let kind = Kind::of(kind).expect("a frame's kind");
        let sum = checksum(
            kind,
            &bytes[start + 1..start + 9],
            &bytes[start + HEAD..end],
        );
        out[start + 9..start + HEAD].copy_from_slice(&sum.to_le_bytes());
    }
    out
}

/// A tape and the same frames as format 1 wrote them, each named.
fn both_formats(bytes: &[u8]) -> [(&'static str, Vec<u8>); 2] {
    [
        ("format 2", bytes.to_vec()),
        ("format 1", as_format_1(bytes)),
    ]
}

#[test]
fn a_written_tape_reads_back_whole_and_clean() {
    let mut rng = XorShift::new(7);
    let (mut w, sink, said) = writer();
    let mut items = Vec::new();
    for step in 0..30 {
        let batch = [
            mono(step * 1_000),
            screen_read(step as usize),
            noise(&mut rng, 40),
        ];
        for item in &batch {
            w.append(item);
        }
        items.extend(batch);
        w.flush_hint(if step % 2 == 0 {
            Hint::Act
        } else {
            Hint::BeforeWait
        });
    }
    let note = Item::Note {
        kind: "turn_end".to_string(),
        text: "TurnEndState { .. }".to_string(),
    };
    w.append(&note);
    items.push(note);
    let end = Item::End(End::Returned {
        how: "EXIT stopped".to_string(),
    });
    w.append(&end);
    items.push(end);
    assert_eq!(w.stopped(), Some(Stopped::Ended));
    let bytes = sink.bytes();
    let read = recover(&bytes);
    assert_eq!(read.header.as_deref(), Some(HEADER));
    assert_eq!(read.items, items);
    assert_eq!(read.tail, Tail::Clean);
    let frames = frames_of(&bytes);
    assert_eq!(read.frames, frames.len());
    assert!(frames.iter().any(|f| f.3 != 0), "a frame compressed");
    let noise_only = {
        let (mut w, sink, _) = writer();
        w.append(&noise(&mut rng, 2_000));
        w.flush_hint(Hint::Act);
        sink.bytes()
    };
    assert!(
        frames_of(&noise_only).last().is_some_and(|f| f.3 == 0),
        "a frame that does not compress is stored raw"
    );
    assert!(said.text().is_empty());
}

#[test]
fn each_frame_is_one_write_closed_where_the_plan_closes_it() {
    let (mut w, sink, _) = writer();
    assert_eq!(sink.taken().writes.len(), 1);
    assert!(sink.bytes().starts_with(&MAGIC));
    assert_eq!(frames_of(&sink.bytes())[0].2, Kind::Header as u8);
    for i in 0..3 {
        w.append(&mono(i));
    }
    assert_eq!(sink.taken().writes.len(), 1, "items wait in the open frame");
    w.flush_hint(Hint::Act);
    assert_eq!(sink.taken().writes.len(), 2);
    w.flush_hint(Hint::BeforeWait);
    assert_eq!(
        sink.taken().writes.len(),
        2,
        "an empty frame is not written"
    );
    w.append(&mono(9));
    w.flush_hint(Hint::BeforeWait);
    assert_eq!(sink.taken().writes.len(), 3);
    w.append(&Item::Mark(Mark::Nested {
        inner: "probe.git_view".to_string(),
        outer: "host.at_idle".to_string(),
    }));
    assert_eq!(sink.taken().writes.len(), 4, "a mark is written at once");
    assert_eq!(sink.taken().syncs, 0, "no sync before the end");
    w.append(&mono(10));
    w.append(&Item::End(End::Suspended));
    assert_eq!(sink.taken().writes.len(), 5, "the end closes its frame");
    assert_eq!(sink.taken().syncs, 1, "and is the one sync");
    w.append(&mono(11));
    w.flush_hint(Hint::Act);
    assert_eq!(sink.taken().writes.len(), 5, "nothing after the end");
}

#[test]
fn a_frame_closes_at_64_kib_of_raw_items() {
    let mut rng = XorShift::new(3);
    let (mut w, sink, _) = writer();
    for _ in 0..20 {
        w.append(&noise(&mut rng, 10_000));
    }
    let bytes = sink.bytes();
    let records: Vec<_> = frames_of(&bytes).into_iter().skip(1).collect();
    assert_eq!(
        records.len(),
        2,
        "200 KB of items, no hint: two full frames, the rest open"
    );
    for (start, ..) in &records {
        let raw =
            u32::from_le_bytes(bytes[start + 1..start + 5].try_into().expect("four")) as usize;
        assert!((FRAME_RAW..FRAME_RAW + 10_100).contains(&raw), "{raw}");
    }
    drop(w);
    assert_eq!(
        recover(&sink.bytes()).items.len(),
        20,
        "the drop wrote the rest"
    );
}

#[test]
fn every_cut_reads_back_a_prefix_of_whole_frames() {
    let mut rng = XorShift::new(11);
    let (written, per_frame) = tape(4, &mut rng);
    let ends: Vec<usize> = frames_of(&written).iter().map(|f| f.1).collect();
    assert_eq!(ends.len(), 5);
    for (format, bytes) in both_formats(&written) {
        for cut in 0..=bytes.len() {
            let read = recover(&bytes[..cut]);
            let whole = ends.iter().filter(|e| **e <= cut).count();
            assert_eq!(read.frames, whole, "{format}, cut {cut}");
            let records = whole.saturating_sub(1);
            assert_eq!(
                read.items,
                prefix(&per_frame, records),
                "{format}, cut {cut}"
            );
            let expected = if cut == 0 || ends.contains(&cut) {
                Tail::Clean
            } else if cut < MAGIC.len() {
                Tail::Torn { at: 0 }
            } else {
                Tail::Torn {
                    at: ends[..whole].last().copied().unwrap_or(MAGIC.len()),
                }
            };
            assert_eq!(read.tail, expected, "{format}, cut {cut}");
            assert_eq!(
                read.header.as_deref(),
                (whole > 0).then_some(HEADER),
                "{format}, cut {cut}"
            );
        }
    }
}

#[test]
fn a_flip_before_a_whole_frame_is_refused_and_one_in_the_last_is_torn() {
    let mut rng = XorShift::new(5);
    let (written, per_frame) = tape(3, &mut rng);
    let frames = frames_of(&written);
    let last = frames.last().expect("frames").0;
    for (format, bytes) in both_formats(&written) {
        for at in 0..bytes.len() {
            for mask in [0x01u8, 0x80, 0xff] {
                let mut damaged = bytes.clone();
                damaged[at] ^= mask;
                let read = recover(&damaged);
                let what = format!("{format}, byte {at} ^ {mask:#x}");
                if at >= last {
                    assert_eq!(read.tail, Tail::Torn { at: last }, "{what}");
                    assert_eq!(
                        read.items,
                        prefix(&per_frame, per_frame.len() - 1),
                        "{what}"
                    );
                    continue;
                }
                let start = frames
                    .iter()
                    .map(|f| f.0)
                    .filter(|s| *s <= at)
                    .max()
                    .unwrap_or(0);
                match &read.tail {
                    Tail::Fault { at: fault, why } => {
                        assert_eq!(*fault, start, "{what}: {why}");
                    }
                    other => panic!("{what}: {other:?}"),
                }
                assert_eq!(read.header, None, "a refused tape serves nothing");
                assert!(read.items.is_empty() && read.frames == 0);
                let kept = salvage(&damaged);
                assert_eq!(kept.tail, read.tail);
                let whole = frames.iter().filter(|f| f.1 <= start).count();
                assert_eq!(kept.frames, whole, "{what}");
                assert_eq!(kept.items, prefix(&per_frame, whole.saturating_sub(1)));
            }
        }
    }
}

#[test]
fn whole_frames_out_of_place_are_refused_where_the_first_stands() {
    let mut rng = XorShift::new(41);
    let (bytes, per_frame) = tape(4, &mut rng);
    for (what, order, place, found) in [
        ("repeated", [0, 1, 2, 2, 3, 4].as_slice(), 3, 2),
        ("dropped", &[0, 1, 3, 4], 2, 3),
        ("moved", &[0, 1, 3, 2, 4], 2, 3),
        ("the last repeated", &[0, 1, 2, 3, 4, 4], 5, 4),
        ("the first dropped", &[0, 2, 3, 4], 1, 2),
        ("the header moved", &[1, 0, 2, 3, 4], 0, 1),
    ] {
        let moved = arranged(&bytes, order);
        let at = MAGIC.len()
            + frames_of(&moved)[..place]
                .iter()
                .map(|f| f.1 - f.0)
                .sum::<usize>();
        let why = Why::OutOfPlace {
            expected: place as u64,
            found,
        };
        let read = recover(&moved);
        assert_eq!(
            read.tail,
            Tail::Fault {
                at,
                why: why.clone()
            },
            "{what}: {order:?}"
        );
        assert_eq!(
            (read.header, read.items.len(), read.frames),
            (None, 0, 0),
            "{what}: a refused tape serves nothing"
        );
        let kept = salvage(&moved);
        assert_eq!(kept.tail, read.tail, "{what}");
        assert_eq!(kept.frames, place, "{what}");
        assert_eq!(
            kept.items,
            prefix(&per_frame, place.saturating_sub(1)),
            "{what}: salvage keeps what was written"
        );
    }
    let lost_last = recover(&arranged(&bytes, &[0, 1, 2, 3]));
    assert_eq!(lost_last.tail, Tail::Clean);
    assert_eq!(lost_last.items, prefix(&per_frame, 3));
    let (ended, items) = golden_run();
    let unended = recover(&arranged(&ended, &[0, 1]));
    assert_eq!(unended.tail, Tail::Clean);
    assert_eq!(unended.items, items[..2]);
    assert!(matches!(items.last(), Some(Item::End(_))));
    assert_eq!(
        Why::OutOfPlace {
            expected: 2,
            found: 3
        }
        .to_string(),
        "frame 3 where frame 2 belongs: whole frames repeated, dropped or moved"
    );
}

#[test]
fn format_1_reads_whole_frames_out_of_place_clean() {
    let mut rng = XorShift::new(41);
    let (bytes, per_frame) = tape(4, &mut rng);
    let one = as_format_1(&bytes);
    let written = prefix(&per_frame, 4);
    for (what, order) in [
        ("repeated", [0, 1, 2, 2, 3, 4].as_slice()),
        ("dropped", &[0, 1, 3, 4]),
        ("moved", &[0, 1, 3, 2, 4]),
    ] {
        let read = recover(&arranged(&one, order));
        assert_eq!(read.tail, Tail::Clean, "{what}: format 1 cannot tell");
        assert_eq!(read.frames, order.len(), "{what}");
        assert_ne!(read.items, written, "{what}");
        let theirs: Vec<Item> = order[1..]
            .iter()
            .flat_map(|i| per_frame[i - 1].clone())
            .collect();
        assert_ne!(
            read.items, theirs,
            "{what}: replies decoded on the wrong base"
        );
        assert!(matches!(
            recover(&arranged(&bytes, order)).tail,
            Tail::Fault {
                why: Why::OutOfPlace { .. },
                ..
            }
        ));
    }
}

/// The run [`GOLDEN_FORMAT_1`] holds: two records frames.
fn golden_run() -> (Vec<u8>, Vec<Item>) {
    let ask = |last: &str| {
        let mut reply = "row of the worker's screen\n".repeat(20);
        reply.push_str(last);
        Item::effect(EffectRecord::Exchange {
            door: doors::CTL.to_string(),
            request: ctl_request(&["text"]),
            reply: reply.into_bytes(),
        })
    };
    let end = Item::End(End::Returned {
        how: "EXIT stopped".to_string(),
    });
    let items = vec![mono(5), ask("0"), mono(9), ask("1"), end];
    let (mut w, sink, _) = writer();
    for (i, item) in items.iter().enumerate() {
        w.append(item);
        if i == 1 {
            w.flush_hint(Hint::Act);
        }
    }
    (sink.bytes(), items)
}

/// [`golden_run`]'s tape as the format-1 writer wrote it.
const GOLDEN_FORMAT_1: &str = concat!(
    "415441504500010a002600000000000000b9e64bf2be72f2aa61746170652031",
    "0a746170653d742d310a6275696c643d67300a656e7472793d77617463680a01",
    "2f0200003a000000972739857c9e054dff1e050a06000363746c060104746578",
    "74009d04726f77206f662074686520776f726b657227732073637265656e0a1b",
    "00ffea607265656e0a30012000000000000000298cbeb4d6e96d5a0508060106",
    "010474657874029c040001310b000c455849542073746f70706564",
);

#[test]
fn a_format_1_tape_reads_as_it_always_did() {
    let golden = aterm_codec::hex::decode_vec(GOLDEN_FORMAT_1.as_bytes()).expect("hex");
    let (bytes, items) = golden_run();
    assert_eq!(as_format_1(&bytes), golden);
    for (format, tape) in [("format 1", &golden), ("format 2", &bytes)] {
        let read = recover(tape);
        assert_eq!(read.header.as_deref(), Some(HEADER), "{format}");
        assert_eq!(read.items, items, "{format}");
        assert_eq!((read.frames, read.tail), (3, Tail::Clean), "{format}");
    }
    let differ: Vec<usize> = (0..bytes.len())
        .filter(|&i| bytes[i] != golden[i])
        .collect();
    let sums: Vec<usize> = frames_of(&bytes).iter().map(|f| f.0 + 9).collect();
    assert!(differ.contains(&6), "the magic's number");
    assert!(
        differ
            .iter()
            .all(|&i| i == 6 || sums.iter().any(|&s| (s..s + 8).contains(&i))),
        "only the magic's number and the sums: {differ:?}"
    );
}

/// [`golden_run`]'s tape as the format-2 writer wrote it.
const GOLDEN_FORMAT_2: &str = concat!(
    "415441504500020a002600000000000000e79143a3fda7acc061746170652031",
    "0a746170653d742d310a6275696c643d67300a656e7472793d77617463680a01",
    "2f0200003a000000a1d3d42cd9475f54ff1e050a06000363746c060104746578",
    "74009d04726f77206f662074686520776f726b657227732073637265656e0a1b",
    "00ffea607265656e0a30012000000000000000c1b686c74dad71900508060106",
    "010474657874029c040001310b000c455849542073746f70706564",
);

#[test]
fn a_format_2_tape_reads_as_it_always_did() {
    let golden = aterm_codec::hex::decode_vec(GOLDEN_FORMAT_2.as_bytes()).expect("hex");
    let (bytes, items) = golden_run();
    let read = recover(&golden);
    assert_eq!(read.header.as_deref(), Some(HEADER));
    assert_eq!(read.items, items);
    assert_eq!((read.frames, read.tail), (3, Tail::Clean));
    assert_eq!(Format::of_magic(&golden), Some(Format::Two));
    for (place, (start, end, kind, _)) in frames_of(&golden).into_iter().enumerate() {
        let kind = Kind::of(kind).expect("a frame's kind");
        let hashed = checksum(
            kind,
            &golden[start + 1..start + 9],
            &golden[start + HEAD..end],
        );
        let sum = u64::from_le_bytes(golden[start + 9..start + HEAD].try_into().expect("eight"));
        assert_eq!(
            place_named(sum, hashed),
            Some(place as u64),
            "frame {place}"
        );
    }
    assert_eq!(bytes, golden, "this build writes format 2's bytes");
}

#[test]
fn a_magic_naming_the_other_format_is_refused() {
    let (bytes, _) = golden_run();
    for (format, tape, named, frames) in [
        ("format 2", bytes.clone(), 1u8, 2u8),
        ("format 1", as_format_1(&bytes), 2, 1),
    ] {
        let mut damaged = tape;
        damaged[6] = named;
        assert_eq!(
            recover(&damaged).tail,
            Tail::Fault {
                at: 0,
                why: Why::MagicFormat { named, frames }
            },
            "{format}"
        );
    }
}

#[test]
fn a_place_reads_back_and_a_damaged_sum_names_none() {
    let mut damages: Vec<u64> = (0..64).map(|bit| 1u64 << bit).collect();
    for byte in 0..8 {
        for mask in [0x01u64, 0x80, 0xff] {
            damages.push(mask << (8 * byte));
        }
    }
    for n in (0..4_096).chain([PLACES - 1]) {
        let word = place_word(n);
        assert_eq!(place_named(word, 0), Some(n));
        let hashed = 0x9e37_79b9_7f4a_7c15_u64.wrapping_mul(n + 1);
        assert_eq!(place_named(word ^ hashed, hashed), Some(n));
        for e in &damages {
            assert_eq!(place_named(word ^ e, 0), None, "place {n}, damage {e:#x}");
        }
    }
    assert_eq!(place_named(place_word(PLACES), 0), None);
    assert_eq!(place_named(0, 0), None, "a format-1 sum names no place");
}

#[test]
fn what_is_not_a_tape_of_this_build_is_named() {
    let fault = |r: Recovered| match r.tail {
        Tail::Fault { at, why } => (at, why),
        other => panic!("{other:?}"),
    };
    assert_eq!(
        recover(b""),
        Recovered {
            header: None,
            items: Vec::new(),
            frames: 0,
            tail: Tail::Clean,
        }
    );
    assert_eq!(
        fault(recover(b"# a markdown file, no tape")),
        (0, Why::NotATape)
    );
    let mut newer = MAGIC.to_vec();
    newer[6] = 3;
    assert_eq!(fault(recover(&newer)), (0, Why::Format(3)));
    assert_eq!(recover(&MAGIC).tail, Tail::Torn { at: MAGIC.len() });
    assert_eq!(recover(&MAGIC[..5]).tail, Tail::Torn { at: 0 });
    let one = Format::One.magic();
    assert_eq!(recover(&one[..7]).tail, Tail::Torn { at: 0 });
    assert_eq!(recover(&one).tail, Tail::Torn { at: MAGIC.len() });
    let records = |items: &[Item]| {
        let mut enc = super::codec::Encoder::default();
        let mut raw = Vec::new();
        for item in items {
            enc.item(item, &mut raw);
        }
        frame_bytes(Kind::Records, 0, &raw)
    };
    let header = frame_bytes(Kind::Header, 0, HEADER.as_bytes());
    let no_header = [MAGIC.as_slice(), &records(&[mono(1)])].concat();
    assert_eq!(fault(recover(&no_header)), (8, Why::NoHeader));
    let second = frame_bytes(Kind::Header, 1, HEADER.as_bytes());
    let two = [MAGIC.as_slice(), &header, &second].concat();
    assert_eq!(fault(recover(&two)), (8 + header.len(), Why::SecondHeader));
    let again = [MAGIC.as_slice(), &header, &header].concat();
    assert_eq!(
        fault(recover(&again)),
        (
            8 + header.len(),
            Why::OutOfPlace {
                expected: 1,
                found: 0
            }
        )
    );
    let wrong = [
        MAGIC.as_slice(),
        &header,
        &frame_bytes(Kind::Records, 1, &[0x7f]),
    ]
    .concat();
    assert!(matches!(
        fault(recover(&wrong)),
        (at, Why::Unreadable(_)) if at == 8 + header.len()
    ));
}

#[test]
fn the_cap_leaves_a_truncation_mark_and_stops() {
    let mut rng = XorShift::new(13);
    let (w, sink, said) = writer();
    let mut w = w.capped_at(4_096);
    let mut appended = Vec::new();
    for _ in 0..40 {
        let item = noise(&mut rng, 300);
        w.append(&item);
        w.flush_hint(Hint::Act);
        appended.push(item);
    }
    assert_eq!(w.stopped(), Some(Stopped::Capped));
    let at_cap = sink.bytes();
    assert!(at_cap.len() <= 4_096, "{}", at_cap.len());
    let read = recover(&at_cap);
    assert_eq!(read.tail, Tail::Clean);
    let (kept, mark) = read.items.split_at(read.items.len() - 1);
    assert_eq!(
        mark,
        [Item::Mark(Mark::Truncated {
            door: kept.len() as u64
        })]
    );
    assert_eq!(kept, &appended[..kept.len()]);
    assert!(kept.len() < appended.len());
    w.append(&noise(&mut rng, 10));
    w.append(&Item::End(End::Returned {
        how: "EXIT".to_string(),
    }));
    assert_eq!(sink.bytes(), at_cap, "nothing past the mark");
    assert_eq!(sink.taken().syncs, 1, "the end syncs the capped tape");
    assert!(said.text().is_empty(), "the cap is no failure");
}

#[test]
fn a_sink_that_refuses_every_write_is_said_once() {
    let sink = Memory::default();
    sink.taken().fail_after = Some(0);
    let said = Said::default();
    let mut w = TapeWriter::start(
        Box::new(sink.clone()),
        "/t/denied.atape",
        HEADER,
        Box::new(said.clone()),
    );
    assert!(w.failed());
    let mut rng = XorShift::new(1);
    for step in 0..200 {
        w.append(&screen_read(step));
        w.append(&noise(&mut rng, 100));
        w.flush_hint(Hint::Act);
    }
    w.append(&Item::End(End::Unwound {
        message: "a panic".to_string(),
    }));
    assert!(w.failed());
    let text = said.text();
    assert_eq!(text.lines().count(), 1, "{text}");
    assert!(
        text.contains("/t/denied.atape"),
        "it names the tape: {text}"
    );
    assert!(sink.bytes().is_empty());
    assert_eq!(sink.taken().syncs, 0);
}

#[test]
fn a_sink_that_fails_midway_leaves_whole_frames_then_a_torn_tail() {
    let sink = Memory::default();
    {
        let mut t = sink.taken();
        t.fail_after = Some(3);
        t.partial = 9;
    }
    let said = Said::default();
    let mut w = TapeWriter::start(
        Box::new(sink.clone()),
        "/t/full.atape",
        HEADER,
        Box::new(said.clone()),
    );
    for step in 0..6 {
        w.append(&screen_read(step));
        w.flush_hint(Hint::Act);
    }
    assert!(w.failed());
    let read = recover(&sink.bytes());
    assert_eq!(read.frames, 3, "the header and two records frames");
    assert_eq!(read.items, vec![screen_read(0), screen_read(1)]);
    let whole = frames_of(&sink.bytes())[2].1;
    assert_eq!(read.tail, Tail::Torn { at: whole });
    assert_eq!(said.text().lines().count(), 1);
}

#[test]
fn a_file_sinks_end_flush_is_ordered() {
    let dir = crate::supervise::ScratchDir::new("tape", "file-sink");
    let mut file = std::fs::File::create(dir.join("tape")).expect("tape file");
    file.write_frame(b"frame").expect("write");
    let recording = aterm_durable::seam::Recording::start();
    FrameSink::sync(&mut file).expect("sync");
    assert_eq!(recording.take(), [aterm_durable::Flush::Ordered]);
}
