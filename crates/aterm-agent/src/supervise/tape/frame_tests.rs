// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A tape's frames and its writer: what is written reads back whole; every
//! cut a crash can make reads back as a prefix of whole frames, said torn;
//! damage before a whole frame is refused and damage to the last one is the
//! torn tail; each frame is one write, closed where the plan closes it; the
//! cap leaves a mark; and a sink that fails is said once and never reaches
//! the loop.

use std::io::{ErrorKind, Write};
use std::sync::{Arc, Mutex, PoisonError};

use astream_effects::EffectRecord;
use aterm_spec::rng::XorShift;

use super::codec::{End, Item, Mark, Stamp, ctl_request};
use super::doors;
use super::frame::{
    FRAME_RAW, FrameSink, HEAD, Hint, Kind, MAGIC, Stopped, TapeWriter, frame_bytes,
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
    /// The failing write still lands this many bytes first (a write the
    /// disk filled partway through).
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

/// The header text the tests write: the frame layer carries any text.
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

/// Each frame of a tape, read off its heads (an oracle beside the reader):
/// its start, its end, its kind byte and its payload length field.
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

/// A tape of a header and `frames` records frames, each closed by an act,
/// with the items of each records frame.
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

/// A run's items, acts and waits between them, then its end, read back
/// exactly: the header, every item, the end, a clean tail — with a frame that
/// compressed and one that did not among them.
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

/// One write per frame: the first holds the magic and the header; an act or
/// a wait closes the open frame (an empty one writes nothing); a mark closes
/// its frame; the end closes its frame and is the one sync.
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

/// With no act or wait, a frame closes once its raw items reach 64 KiB.
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

/// Every byte-offset cut of a tape reads back the whole frames before the
/// cut — said clean only at a frame's end, torn anywhere else.
#[test]
fn every_cut_reads_back_a_prefix_of_whole_frames() {
    let mut rng = XorShift::new(11);
    let (bytes, per_frame) = tape(4, &mut rng);
    let ends: Vec<usize> = frames_of(&bytes).iter().map(|f| f.1).collect();
    assert_eq!(ends.len(), 5);
    for cut in 0..=bytes.len() {
        let read = recover(&bytes[..cut]);
        let whole = ends.iter().filter(|e| **e <= cut).count();
        assert_eq!(read.frames, whole, "cut {cut}");
        let records = whole.saturating_sub(1);
        assert_eq!(read.items, prefix(&per_frame, records), "cut {cut}");
        let expected = if cut == 0 || ends.contains(&cut) {
            Tail::Clean
        } else if cut < MAGIC.len() {
            Tail::Torn { at: 0 }
        } else {
            Tail::Torn {
                at: ends[..whole].last().copied().unwrap_or(MAGIC.len()),
            }
        };
        assert_eq!(read.tail, expected, "cut {cut}");
        assert_eq!(
            read.header.as_deref(),
            (whole > 0).then_some(HEADER),
            "cut {cut}"
        );
    }
}

/// Every single-byte flip: one with a whole frame after it refuses the tape
/// at the damaged frame (salvage keeps the frames before it); one in the last
/// frame is the torn tail, and the frames before it come back.
#[test]
fn a_flip_before_a_whole_frame_is_refused_and_one_in_the_last_is_torn() {
    let mut rng = XorShift::new(5);
    let (bytes, per_frame) = tape(3, &mut rng);
    let frames = frames_of(&bytes);
    let last = frames.last().expect("frames").0;
    for at in 0..bytes.len() {
        for mask in [0x01u8, 0x80, 0xff] {
            let mut damaged = bytes.clone();
            damaged[at] ^= mask;
            let read = recover(&damaged);
            if at >= last {
                assert_eq!(read.tail, Tail::Torn { at: last }, "byte {at} ^ {mask:#x}");
                assert_eq!(read.items, prefix(&per_frame, per_frame.len() - 1));
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
                    assert_eq!(*fault, start, "byte {at} ^ {mask:#x}: {why}");
                }
                other => panic!("byte {at} ^ {mask:#x}: {other:?}"),
            }
            assert_eq!(read.header, None, "a refused tape serves nothing");
            assert!(read.items.is_empty() && read.frames == 0);
            let kept = salvage(&damaged);
            assert_eq!(kept.tail, read.tail);
            let whole = frames.iter().filter(|f| f.1 <= start).count();
            assert_eq!(kept.frames, whole, "byte {at}");
            assert_eq!(kept.items, prefix(&per_frame, whole.saturating_sub(1)));
        }
    }
}

/// Bytes that are no tape, a tape of another frame format, a tape cut at its
/// magic, an empty file, and frames that are whole but wrong in their place.
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
    newer[6] = 2;
    assert_eq!(fault(recover(&newer)), (0, Why::Format(2)));
    assert_eq!(recover(&MAGIC).tail, Tail::Torn { at: MAGIC.len() });
    assert_eq!(recover(&MAGIC[..5]).tail, Tail::Torn { at: 0 });
    let records = |items: &[Item]| {
        let mut enc = super::codec::Encoder::default();
        let mut raw = Vec::new();
        for item in items {
            enc.item(item, &mut raw);
        }
        frame_bytes(Kind::Records, &raw)
    };
    let header = frame_bytes(Kind::Header, HEADER.as_bytes());
    let no_header = [MAGIC.as_slice(), &records(&[mono(1)])].concat();
    assert_eq!(fault(recover(&no_header)), (8, Why::NoHeader));
    let two = [MAGIC.as_slice(), &header, &header].concat();
    assert_eq!(fault(recover(&two)), (8 + header.len(), Why::SecondHeader));
    // A whole last frame whose items this build does not read is a fault,
    // not a torn tail: no crash writes a whole frame of the wrong bytes.
    let wrong = [
        MAGIC.as_slice(),
        &header,
        &frame_bytes(Kind::Records, &[0x7f]),
    ]
    .concat();
    assert!(matches!(
        fault(recover(&wrong)),
        (at, Why::Unreadable(_)) if at == 8 + header.len()
    ));
}

/// At the cap the writer writes `Mark::Truncated` naming the first effect it
/// did not keep, and stops: the prefix reads back clean, the tape is within
/// the cap, and the end still makes it durable.
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

/// A sink that refuses every write: the writer says so once, stops, and
/// never panics or reaches its caller, whatever the loop then appends.
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

/// A sink that fails partway through — the disk filling inside a frame's
/// write — leaves whole frames and then the torn tail; the writer stops there.
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

/// A file sink's end flush is only ordered (aterm-durable's counting seam): the
/// tape is a diagnostic record whose torn tail recovery reports, kept ahead of
/// the run's later writes and promised to nothing off the device.
#[test]
fn a_file_sinks_end_flush_is_ordered() {
    let dir = crate::supervise::ScratchDir::new("tape", "file-sink");
    let mut file = std::fs::File::create(dir.join("tape")).expect("tape file");
    file.write_frame(b"frame").expect("write");
    let recording = aterm_durable::seam::Recording::start();
    FrameSink::sync(&mut file).expect("sync");
    assert_eq!(recording.take(), [aterm_durable::Flush::Ordered]);
}
