// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Frame encoding, recovery, and crash-truncation tests for supervisor tapes.

use std::io::{ErrorKind, Write};
use std::sync::{Arc, Mutex, PoisonError};

use astream_effects::EffectRecord;
use astream_wire::steps::{StepFault, encode_step};
use aterm_spec::rng::XorShift;

use super::codec::{Encoder, End, Item, Mark, Stamp, ctl_request};
use super::doors;
use super::frame::{
    BODY_HEAD, FRAME_FORMAT, FRAME_RAW, FRAME_RAW_MAX, FrameSink, Hint, Kind, MAGIC, OVERHEAD,
    Stopped, TapeWriter, frame_bytes,
};
use super::recover::{RECORD_HEAD, Recovered, Tail, Why, recover, salvage, steps};

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

/// A whole frame of a tape, as astream's scan finds it.
#[derive(Debug, Clone, Copy)]
pub(super) struct Framed {
    /// Where its record starts in the tape's bytes.
    pub(super) start: usize,
    /// Where the next one starts.
    pub(super) end: usize,
    pub(super) kind: u8,
    /// Its raw length, as its body says.
    pub(super) raw: usize,
    /// Whether its payload is stored compressed (shorter than its raw bytes).
    pub(super) packed: bool,
}

/// The whole frames of a tape's bytes, by astream's scan of what follows the magic.
pub(super) fn frames_of(bytes: &[u8]) -> Vec<Framed> {
    let records = &bytes[MAGIC.len()..];
    steps(records)
        .steps
        .into_iter()
        .map(|(_, body)| {
            let b = &records[body.clone()];
            let raw = u32::from_le_bytes(b[1..BODY_HEAD].try_into().expect("four bytes")) as usize;
            Framed {
                start: MAGIC.len() + body.start - RECORD_HEAD,
                end: MAGIC.len() + body.end,
                kind: b[0],
                raw,
                packed: b.len() - BODY_HEAD < raw,
            }
        })
        .collect()
}

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

fn prefix(per_frame: &[Vec<Item>], whole: usize) -> Vec<Item> {
    per_frame[..whole].iter().flatten().cloned().collect()
}

/// A tape's magic, then its whole frames in `order` (the header is frame 0).
pub(super) fn arranged(bytes: &[u8], order: &[usize]) -> Vec<u8> {
    let frames = frames_of(bytes);
    let mut out = bytes[..MAGIC.len()].to_vec();
    for &i in order {
        out.extend_from_slice(&bytes[frames[i].start..frames[i].end]);
    }
    out
}

/// NEGATIVE CONTROL: a reader that reads a fault as a torn tail, keeping the
/// whole frames before it as a torn tail's reader does.
pub(super) fn fault_as_torn(bytes: &[u8]) -> Recovered {
    let mut read = salvage(bytes);
    if let Tail::Fault { at, .. } = read.tail {
        read.tail = Tail::Torn { at };
    }
    read
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
    assert_eq!(frames.last().map(|f| f.end), Some(bytes.len()));
    assert!(frames.iter().any(|f| f.packed), "a frame compressed");
    let noise_only = {
        let (mut w, sink, _) = writer();
        w.append(&noise(&mut rng, 2_000));
        w.flush_hint(Hint::Act);
        sink.bytes()
    };
    assert!(
        frames_of(&noise_only).last().is_some_and(|f| !f.packed),
        "a frame that does not compress is stored raw"
    );
    assert!(said.text().is_empty());
}

#[test]
fn each_frame_is_one_write_closed_where_the_plan_closes_it() {
    let (mut w, sink, _) = writer();
    assert_eq!(sink.taken().writes.len(), 1);
    assert!(sink.bytes().starts_with(&MAGIC));
    assert_eq!(frames_of(&sink.bytes())[0].kind, Kind::Header as u8);
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
    for frame in &records {
        assert!(
            (FRAME_RAW..FRAME_RAW + 10_100).contains(&frame.raw),
            "{}",
            frame.raw
        );
    }
    drop(w);
    assert_eq!(
        recover(&sink.bytes()).items.len(),
        20,
        "the drop wrote the rest"
    );
}

#[test]
fn a_frame_is_an_astream_step_record_whose_step_is_its_place() {
    let raw = b"atape 1\n";
    let mut body = vec![Kind::Header as u8];
    body.extend_from_slice(&8u32.to_le_bytes());
    body.extend_from_slice(raw);
    assert_eq!(
        frame_bytes(Kind::Header, 0, raw).expect("a frame"),
        encode_step(0, &body).expect("a step record"),
        "the kind, the raw length and the raw bytes, stored as they are"
    );
    let (bytes, _) = tape(3, &mut XorShift::new(17));
    let records = &bytes[MAGIC.len()..];
    let scan = steps(records);
    assert_eq!(scan.valid_len, records.len());
    assert_eq!(
        scan.steps.iter().map(|(step, _)| *step).collect::<Vec<_>>(),
        [0, 1, 2, 3],
        "the header is step 0, and each frame's step is its place"
    );
    assert_eq!(OVERHEAD, RECORD_HEAD + BODY_HEAD);
    assert_eq!(MAGIC[6], FRAME_FORMAT);
}

#[test]
fn every_cut_reads_back_a_prefix_of_whole_frames() {
    let mut rng = XorShift::new(11);
    let (bytes, per_frame) = tape(4, &mut rng);
    let ends: Vec<usize> = frames_of(&bytes).iter().map(|f| f.end).collect();
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

/// What `read` makes of every byte of a three-frame tape flipped under three
/// masks, against what a reader must: a flip in the last frame is a torn tail
/// that keeps the frames before it; one anywhere before is a fault at the
/// damaged frame (the magic's, at 0), refused whole, never a torn tail, and
/// salvage keeps the whole frames before it.
fn flips_read(read: fn(&[u8]) -> Recovered) -> Result<usize, String> {
    let mut rng = XorShift::new(5);
    let (bytes, per_frame) = tape(3, &mut rng);
    let frames = frames_of(&bytes);
    let last = frames.last().expect("frames").start;
    let mut flips = 0;
    for at in 0..bytes.len() {
        for mask in [0x01u8, 0x80, 0xff] {
            let mut damaged = bytes.clone();
            damaged[at] ^= mask;
            let got = read(&damaged);
            let what = format!("byte {at} ^ {mask:#x}");
            flips += 1;
            if at >= last {
                let torn = Tail::Torn { at: last };
                if got.tail != torn || got.items != prefix(&per_frame, per_frame.len() - 1) {
                    return Err(format!("{what}: {:?}, not {torn:?}", got.tail));
                }
                continue;
            }
            let start = frames
                .iter()
                .map(|f| f.start)
                .filter(|s| *s <= at)
                .max()
                .unwrap_or(0);
            match &got.tail {
                Tail::Fault { at: fault, .. } if *fault == start => {}
                other => return Err(format!("{what}: {other:?}, not a fault at {start}")),
            }
            if got.header.is_some() || !got.items.is_empty() || got.frames != 0 {
                return Err(format!(
                    "{what}: a refused tape served {} frames",
                    got.frames
                ));
            }
            let kept = salvage(&damaged);
            let whole = frames.iter().filter(|f| f.end <= start).count();
            if kept.tail != got.tail
                || kept.frames != whole
                || kept.items != prefix(&per_frame, whole.saturating_sub(1))
            {
                return Err(format!("{what}: salvage kept {} frames", kept.frames));
            }
        }
    }
    Ok(flips)
}

#[test]
fn a_flip_before_a_whole_frame_is_refused_and_one_in_the_last_is_torn() {
    let flips = flips_read(recover).unwrap_or_else(|e| panic!("{e}"));
    assert!(flips > 1_000, "{flips} flips");
    let control = flips_read(fault_as_torn);
    assert!(
        control.as_ref().is_err_and(|e| e.contains("Torn")),
        "NEGATIVE CONTROL: a reader that reads a fault as torn passes: {control:?}"
    );
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
                .map(|f| f.end - f.start)
                .sum::<usize>();
        let why = Why::Steps(StepFault::OutOfOrder {
            expected: place as u64,
            found,
        });
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
        Why::Steps(StepFault::OutOfOrder {
            expected: 2,
            found: 3
        })
        .to_string(),
        "out-of-order step record (step 3, expected 2)"
    );
}

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

/// [`golden_run`]'s tape in frame format 3: the magic, then astream step records
/// (the astream frame head `a571 01 00`, its length and CRC-32; the step's version,
/// number and body length; the body's kind and raw length, then its payload).
const GOLDEN_FORMAT_3: &str = concat!(
    "415441504500030aa571010038000000d19ebba40100000000000000002b0000",
    "000026000000617461706520310a746170653d742d310a6275696c643d67300a",
    "656e7472793d77617463680aa57101004c00000037b5bec50101000000000000",
    "003f000000012f020000ff1e050a06000363746c06010474657874009d04726f",
    "77206f662074686520776f726b657227732073637265656e0a1b00ffea607265",
    "656e0a30a571010032000000a81f75d501020000000000000025000000012000",
    "00000508060106010474657874029c040001310b000c455849542073746f7070",
    "6564",
);

#[test]
fn a_format_3_tape_reads_as_it_always_did() {
    let (bytes, items) = golden_run();
    assert_eq!(
        aterm_codec::hex::encode_lower(&bytes),
        GOLDEN_FORMAT_3,
        "this build writes format 3's bytes"
    );
    let golden = aterm_codec::hex::decode_vec(GOLDEN_FORMAT_3.as_bytes()).expect("hex");
    let read = recover(&golden);
    assert_eq!(read.header.as_deref(), Some(HEADER));
    assert_eq!(read.items, items);
    assert_eq!((read.frames, read.tail), (3, Tail::Clean));
}

/// [`golden_run`]'s tape as frame format 1 wrote it: aterm's own frame heads, each
/// summed with xxh64.
const GOLDEN_FORMAT_1: &str = concat!(
    "415441504500010a002600000000000000b9e64bf2be72f2aa61746170652031",
    "0a746170653d742d310a6275696c643d67300a656e7472793d77617463680a01",
    "2f0200003a000000972739857c9e054dff1e050a06000363746c060104746578",
    "74009d04726f77206f662074686520776f726b657227732073637265656e0a1b",
    "00ffea607265656e0a30012000000000000000298cbeb4d6e96d5a0508060106",
    "010474657874029c040001310b000c455849542073746f70706564",
);

/// [`golden_run`]'s tape as frame format 2 wrote it: format 1's, each sum also
/// naming its frame's place.
const GOLDEN_FORMAT_2: &str = concat!(
    "415441504500020a002600000000000000e79143a3fda7acc061746170652031",
    "0a746170653d742d310a6275696c643d67300a656e7472793d77617463680a01",
    "2f0200003a000000a1d3d42cd9475f54ff1e050a06000363746c060104746578",
    "74009d04726f77206f662074686520776f726b657227732073637265656e0a1b",
    "00ffea607265656e0a30012000000000000000c1b686c74dad71900508060106",
    "010474657874029c040001310b000c455849542073746f70706564",
);

#[test]
fn a_tape_of_an_earlier_frame_format_is_refused_by_its_number() {
    for (format, hex) in [(1u8, GOLDEN_FORMAT_1), (2, GOLDEN_FORMAT_2)] {
        let tape = aterm_codec::hex::decode_vec(hex.as_bytes()).expect("hex");
        let why = Why::Format(format);
        let refused = Tail::Fault {
            at: 0,
            why: why.clone(),
        };
        let read = recover(&tape);
        assert_eq!(read.tail, refused, "format {format}");
        assert_eq!((read.header, read.frames), (None, 0), "format {format}");
        let kept = salvage(&tape);
        assert_eq!((kept.tail, kept.frames), (refused, 0), "format {format}");
        assert_eq!(
            why.to_string(),
            format!(
                "a tape of frame format {format}: this build reads only format 3, astream step \
                 records"
            )
        );
        let err = super::show::open(&tape, false).expect_err("refused");
        assert!(err.contains(&format!("frame format {format}")), "{err}");
    }
}

#[test]
fn a_magic_damaged_before_this_builds_records_is_named_damaged() {
    // One byte of the magic damaged at rest, the step records after it intact:
    // this build's tape, not one of the format a rotted format byte names (3
    // under 0x01 is 2, under 0x02 is 1) and not no tape at all.
    let mut rng = XorShift::new(23);
    for frames in 0..=2 {
        let (bytes, _) = tape(frames, &mut rng);
        for byte in 0..MAGIC.len() {
            for mask in [0x01u8, 0x02, 0x80, 0xff] {
                let mut damaged = bytes.clone();
                damaged[byte] ^= mask;
                let what = format!("{frames} records frames, byte {byte} ^ {mask:#04x}");
                let read = recover(&damaged);
                let err = super::show::open(&damaged, false).expect_err("refused");
                assert!(
                    err.starts_with(&format!(
                        "refused at byte 0: a tape magic damaged at byte {byte}: frame format 3's \
                         astream step records follow it"
                    )),
                    "{what}: {err}"
                );
                let refused = Tail::Fault {
                    at: 0,
                    why: Why::DamagedMagic { byte },
                };
                assert_eq!(read.tail, refused, "{what}");
                assert_eq!((read.header, read.frames), (None, 0), "{what}");
                let kept = salvage(&damaged);
                assert_eq!((kept.tail, kept.frames), (read.tail, 0), "{what}");
            }
        }
    }
}

#[test]
fn a_last_frame_that_nests_an_astream_frame_reads_a_cut_past_it_as_a_fault() {
    // astream's rule reads bytes, not intent (`astream_wire::steps`, "A body
    // that holds a frame of its own"): a raw frame whose items hold a whole
    // astream frame, cut past that frame's last byte, looks like a rotted
    // length, so the read refuses where a crash left only a torn tail. Salvage
    // keeps what a torn read keeps; a cut before the nested frame is whole is
    // torn.
    let mut rng = XorShift::new(29);
    let (mut w, sink, _) = writer();
    let first = vec![mono(1), noise(&mut rng, 40)];
    for item in &first {
        w.append(item);
    }
    w.flush_hint(Hint::Act);
    let nested = encode_step(0, b"a step record read back from a file").expect("a step record");
    let mut read_bytes: Vec<u8> = (0..24).map(|_| (rng.next_u64() & 0xff) as u8).collect();
    read_bytes.extend_from_slice(&nested);
    read_bytes.extend((0..24).map(|_| (rng.next_u64() & 0xff) as u8));
    w.append(&Item::effect(EffectRecord::File {
        path: "/l/steps.bin".to_string(),
        bytes: read_bytes,
    }));
    w.flush_hint(Hint::Act);
    drop(w);
    let bytes = sink.bytes();
    let frames = frames_of(&bytes);
    let last = *frames.last().expect("frames");
    assert_eq!(frames.len(), 3, "the header and two records frames");
    assert!(!last.packed, "stored raw, so the nested frame stands whole");
    let nested_at = bytes
        .windows(nested.len())
        .position(|w| w == nested.as_slice())
        .expect("the nested frame in the tape's bytes");
    assert!(nested_at > last.start, "inside the last frame");
    let nested_end = nested_at + nested.len();
    for cut in last.start + 1..last.end {
        let read = recover(&bytes[..cut]);
        let kept = salvage(&bytes[..cut]);
        if cut < nested_end {
            assert_eq!(read.tail, Tail::Torn { at: last.start }, "cut {cut}");
            assert_eq!((read.frames, &read.items), (2, &first), "cut {cut}");
        } else {
            let fault = Tail::Fault {
                at: last.start,
                why: Why::Steps(StepFault::LengthPastEnd),
            };
            assert_eq!(read.tail, fault, "cut {cut}");
            assert_eq!((read.header, read.frames), (None, 0), "cut {cut}: refused");
            assert_eq!(kept.tail, fault, "cut {cut}");
        }
        assert_eq!(
            (kept.header.as_deref(), kept.frames, &kept.items),
            (Some(HEADER), 2, &first),
            "cut {cut}: salvage keeps the frames before the last"
        );
    }
    let mut rot = bytes.clone();
    rot[last.end - 1] ^= 0x01;
    assert!(
        matches!(
            recover(&rot).tail,
            Tail::Fault { at, why: Why::Steps(StepFault::CorruptFrame(_)) } if at == last.start
        ),
        "damage to it is a fault too: {:?}",
        recover(&rot).tail
    );
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
    newer[6] = FRAME_FORMAT + 1;
    assert_eq!(fault(recover(&newer)), (0, Why::Format(FRAME_FORMAT + 1)));
    assert_eq!(recover(&MAGIC).tail, Tail::Torn { at: MAGIC.len() });
    assert_eq!(recover(&MAGIC[..5]).tail, Tail::Torn { at: 0 });
    let records = |items: &[Item]| {
        let mut enc = Encoder::default();
        let mut raw = Vec::new();
        for item in items {
            enc.item(item, &mut raw);
        }
        frame_bytes(Kind::Records, 0, &raw).expect("a frame")
    };
    let header = frame_bytes(Kind::Header, 0, HEADER.as_bytes()).expect("a frame");
    let after = MAGIC.len() + header.len();
    let no_header = [MAGIC.as_slice(), &records(&[mono(1)])].concat();
    assert_eq!(fault(recover(&no_header)), (8, Why::NoHeader));
    let second = frame_bytes(Kind::Header, 1, HEADER.as_bytes()).expect("a frame");
    let two = [MAGIC.as_slice(), &header, &second].concat();
    assert_eq!(fault(recover(&two)), (after, Why::SecondHeader));
    let again = [MAGIC.as_slice(), &header, &header].concat();
    assert_eq!(
        fault(recover(&again)),
        (
            after,
            Why::Steps(StepFault::OutOfOrder {
                expected: 1,
                found: 0
            })
        )
    );
    let wrong = [
        MAGIC.as_slice(),
        &header,
        &frame_bytes(Kind::Records, 1, &[0x7f]).expect("a frame"),
    ]
    .concat();
    assert!(matches!(
        fault(recover(&wrong)),
        (at, Why::Unreadable(_)) if at == after
    ));
    let body = |kind: u8, raw_len: u32, payload: &[u8]| {
        let mut body = vec![kind];
        body.extend_from_slice(&raw_len.to_le_bytes());
        body.extend_from_slice(payload);
        let record = encode_step(1, &body).expect("a step record");
        [MAGIC.as_slice(), &header, &record].concat()
    };
    for (what, tape) in [
        ("a body shorter than its head", {
            let record = encode_step(1, &[1, 0, 0]).expect("a step record");
            [MAGIC.as_slice(), &header, &record].concat()
        }),
        ("a kind this build lacks", body(2, 0, &[])),
        ("a payload longer than its raw bytes", body(1, 1, &[0, 0])),
        (
            "a payload that does not decompress",
            body(1, 64, &[0xff; 8]),
        ),
        (
            "a raw length past a frame's",
            body(1, u32::try_from(FRAME_RAW_MAX + 1).expect("u32"), &[]),
        ),
    ] {
        assert!(
            matches!(fault(recover(&tape)), (at, Why::Unreadable(_)) if at == after),
            "{what}"
        );
    }
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

/// An output whose item encodes to `len` bytes, of bytes that do not compress.
fn output_of(rng: &mut XorShift, len: usize) -> Item {
    let encoded = |item: &Item| {
        let mut bytes = Vec::new();
        Encoder::default().item(item, &mut bytes);
        bytes.len()
    };
    // An output of 2^22 bytes spells its length in as many bytes as one of `len`'s.
    let probe = 1 << 22;
    let overhead = encoded(&noise(rng, probe)) - probe;
    let item = noise(rng, len - overhead);
    assert_eq!(encoded(&item), len);
    item
}

#[test]
fn an_item_past_what_a_frame_holds_is_truncated_with_its_mark() {
    let mut rng = XorShift::new(19);
    let largest = output_of(&mut rng, FRAME_RAW_MAX);
    let (mut w, sink, said) = writer();
    w.append(&largest);
    w.append(&mono(1));
    w.flush_hint(Hint::Act);
    assert_eq!(
        w.stopped(),
        None,
        "an item of a frame's most is written whole"
    );
    let read = recover(&sink.bytes());
    assert_eq!(read.tail, Tail::Clean);
    assert_eq!(read.items, [largest, mono(1)]);
    let frames = frames_of(&sink.bytes());
    assert_eq!(
        (frames[1].raw, frames[1].packed),
        (FRAME_RAW_MAX, false),
        "alone in its frame, stored raw: the largest body a step record carries"
    );

    let over = output_of(&mut rng, FRAME_RAW_MAX + 1);
    let (mut w, sink, _) = writer();
    w.append(&mono(2));
    w.append(&over);
    w.append(&mono(3));
    w.flush_hint(Hint::Act);
    assert_eq!(w.stopped(), Some(Stopped::Capped));
    let read = recover(&sink.bytes());
    assert_eq!(read.tail, Tail::Clean);
    assert_eq!(
        read.items,
        [mono(2), Item::Mark(Mark::Truncated { door: 1 })],
        "what came before, then the mark at the first effect not kept"
    );
    assert!(said.text().is_empty(), "the truncation is no failure");
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
    let whole = frames_of(&sink.bytes())[2].end;
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
