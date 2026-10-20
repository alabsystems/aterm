// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::layout::{OFF_HIST_CAP, PAGE};
use crate::records::{KIND_BYTES, KIND_PAD, TailHeader, decode_keyframe};
use crate::store::{RegionView, VecStore};
use aterm_core::terminal::Terminal;

const WRITER: WriterIdentity<'static> = WriterIdentity {
    pid: 4242,
    birth_us: 1_700_000_000_000_000,
    local_id: 3,
    build: 105,
    sid: "s-0123",
};

fn small(tail_cap: usize, hist_cap: usize) -> JournalWriter<VecStore> {
    let geometry = Geometry::custom(64 * 1024, tail_cap, hist_cap).expect("a test layout");
    JournalWriter::create(VecStore::new(geometry.region_len), geometry, &WRITER).expect("created")
}

fn word_at<S: RegionStore>(w: &JournalWriter<S>, at: usize) -> u64 {
    w.store().load_u64_acquire(at).expect("a header word")
}

/// Every byte of the writer's region.
fn region_of<S: RegionStore>(w: &JournalWriter<S>) -> Vec<u8> {
    let mut bytes = vec![0u8; w.store().len()];
    w.store().read(0, &mut bytes).expect("the whole region");
    bytes
}

fn tail_header(w: &JournalWriter<VecStore>, pos: u64) -> TailHeader {
    let cap = w.geometry().tail_cap as u64;
    let at = w.geometry().tail_off + (pos % cap) as usize;
    let mut head = [0u8; TAIL_RECORD_HEADER];
    w.store().read(at, &mut head).expect("inside");
    TailHeader::read(&head)
}

/// The header carries the writer's identity and the geometry, and the magic is written
/// last: a region read before it is no region.
#[test]
fn create_lays_the_header_out_magic_last() {
    let geometry = Geometry::custom(64 * 1024, 4096, 4096).expect("layout");
    let store = crate::store::CountdownStore::new(geometry.region_len, None);
    let w = JournalWriter::create(store, geometry, &WRITER).expect("created");
    let stores = w.store().stores();
    for budget in 0..stores {
        let partial = crate::store::CountdownStore::new(geometry.region_len, Some(budget));
        let _ = JournalWriter::create(partial, geometry, &WRITER).map(|w| {
            assert_eq!(
                w.store()
                    .snapshot()
                    .load_u64_acquire(crate::layout::OFF_MAGIC),
                Ok(0),
                "a creator stopped after {budget} of {stores} stores leaves no magic"
            );
        });
    }
    assert_eq!(word_at(&w, crate::layout::OFF_WRITER_PID), 4242);
    assert_eq!(word_at(&w, crate::layout::OFF_TAIL_CAP), 4096);
    assert_eq!(word_at(&w, OFF_HIST_CAP), 4096);
    let mut sid = [0u8; 6];
    w.store()
        .read(crate::layout::OFF_SID, &mut sid)
        .expect("inside");
    assert_eq!(&sid, b"s-0123");
    assert_eq!(geometry.region_len % PAGE, 0);
    assert!(matches!(
        JournalWriter::create(VecStore::new(PAGE * 2), geometry, &WRITER),
        Err(WriteError::Layout(_))
    ));
    let misaligned = Geometry {
        tail_cap: geometry.tail_cap - 4,
        ..geometry
    };
    assert!(matches!(
        JournalWriter::create(VecStore::new(geometry.region_len), misaligned, &WRITER),
        Err(WriteError::Layout(LayoutError::Misaligned))
    ));
    // The session id keeps its printable ASCII only.
    let odd = WriterIdentity {
        sid: "s 1\u{7}\u{e9}x",
        ..WRITER
    };
    let w =
        JournalWriter::create(VecStore::new(geometry.region_len), geometry, &odd).expect("created");
    let mut sid = [0xFF; 4];
    w.store()
        .read(crate::layout::OFF_SID, &mut sid)
        .expect("inside");
    assert_eq!(&sid, b"s1x\0");
}

/// The tail ring: records land 8-aligned and in order, the commit and stream words
/// follow each, the floor moves only onto record boundaries and only when a record would
/// overwrite bytes above it, and a record that would straddle the ring's end goes to the
/// start after a PAD, a gap of exactly a header's size included.
#[test]
fn the_tail_moves_its_floor_onto_boundaries_before_it_overwrites() {
    let mut w = small(96, 4096);
    w.append_slice(b"abcde").expect("one"); // 24 bytes: [0, 24)
    w.append_slice(b"fghij").expect("two"); // [24, 48)
    w.append_slice(b"klmno").expect("three"); // [48, 72)
    assert_eq!((w.tail_commit(), w.tail_floor()), (72, 0));
    assert_eq!(word_at(&w, OFF_TAIL_COMMIT), 72);
    assert_eq!(word_at(&w, OFF_STREAM_PROCESSED), 15);
    // A 40-byte record (24 bytes of payload) would straddle the end at 72: a PAD fills
    // [72, 96), the record lands at [96, 136) and overwrites [0, 40), so the floor moves
    // past the two records it touches, onto the third's start.
    w.append_slice(b"0123456789abcdefghijklmn").expect("four");
    assert_eq!(w.tail_commit(), 136);
    assert_eq!(w.tail_floor(), 48, "the first record it does not overwrite");
    assert_eq!(word_at(&w, OFF_TAIL_FLOOR), 48);
    assert_eq!(tail_header(&w, 72).kind, KIND_PAD);
    assert_eq!(tail_header(&w, 72).record_len(), 24);
    // With no keyframe committed, a settle has nothing to settle: the bytes below the
    // floor stay as they are.
    let before = region_of(&w);
    w.settle_clear().expect("nothing to do");
    assert!(region_of(&w) == before, "no byte changed");
    let moved = tail_header(&w, 96);
    assert_eq!(
        (moved.kind, moved.payload_len, moved.stream_end),
        (KIND_BYTES, 24, 39)
    );
    // A gap of exactly a header gets its PAD too: the reader skips only a gap under a
    // header, and reads these 16 bytes, here the last lap's payload, as a record.
    let mut exact = small(96, 4096);
    for _ in 0..3 {
        exact.append_slice(b"0123456789abcdef").expect("[0, 96)"); // 32 bytes each
    }
    for _ in 0..2 {
        exact
            .append_slice(b"0123456789abcdefghij")
            .expect("[96, 176)"); // 40 bytes each
    }
    exact
        .append_slice(b"tailend")
        .expect("a PAD, then [192, 216)");
    let pad = tail_header(&exact, 176);
    assert_eq!((pad.kind, pad.record_len()), (KIND_PAD, 16));
    // Its one byte of padding is zeroed, not left as the last lap's payload.
    let mut padding = [0xFF];
    exact
        .store()
        .read(exact.geometry().tail_off + 23, &mut padding)
        .expect("inside");
    assert_eq!(padding, [0]);
    // A gap under a header holds no PAD, and nothing lands past the ring's end. The
    // record goes to the start over the last lap's payload, its 7 bytes of padding zeroed.
    let mut short = small(96, 4096);
    for bytes in [&b"abcdefgh"[..], b"0123456789abcdef", b"0123456789abcdef"] {
        short.append_slice(bytes).expect("[0, 88)"); // 24, 32 and 32 bytes
    }
    let past_end = short.geometry().tail_off + 96;
    let mut past = [0u8; 8];
    short.store().read(past_end, &mut past).expect("inside");
    short
        .append_slice(b"t")
        .expect("an 8-byte gap, then [96, 120)");
    assert_eq!(short.tail_commit(), 120);
    let mut after = [0xFF; 8];
    short.store().read(past_end, &mut after).expect("inside");
    assert_eq!(after, past, "the bytes past the ring are as they were");
    let mut padding = [0xFF; 7];
    short
        .store()
        .read(short.geometry().tail_off + 17, &mut padding)
        .expect("inside");
    assert_eq!(padding, [0; 7]);
    // Synthesized bytes are journaled but add nothing to the stream.
    w.append_synthesized(b"\x1bc").expect("five");
    assert_eq!(w.stream_processed(), 39);
    // A slice longer than the ring's largest record is split.
    let mut big = small(64, 4096);
    big.append_slice(&[b'x'; 100]).expect("split");
    assert_eq!(big.stream_processed(), 100);
    assert!(big.tail_floor() > 0);
    // In a full-size ring, at MAX_BYTES_PAYLOAD a record.
    let v1 = Geometry::v1();
    let mut full = JournalWriter::create(VecStore::new(v1.region_len), v1, &WRITER).expect("c");
    full.append_slice(&vec![b'x'; MAX_BYTES_PAYLOAD + 1])
        .expect("split");
    let two = 2 * TAIL_RECORD_HEADER + MAX_BYTES_PAYLOAD + 8;
    assert_eq!(full.tail_commit(), two as u64);
    // A record larger than the whole ring is refused, and nothing is stored.
    let mut least = small(24, 4096);
    assert_eq!(
        least.append_resize(4, 20, ResizePolicy::Native, 1, 1),
        Err(WriteError::TooLarge { len: 40, cap: 24 })
    );
    assert_eq!(least.tail_commit(), 0);
}

/// Keyframes alternate between the slots by sequence parity; a begun keyframe zeroes its
/// slot's commit word before anything else and publishes it last; a keyframe too large
/// for a slot stores nothing; forgetting the other slot leaves only the newest.
#[test]
fn keyframes_alternate_slots_and_publish_last() {
    let mut w = small(4096, 4096);
    let mut t = Terminal::new(4, 20);
    t.process(b"hello");
    let checkpoint = t.checkpoint_journal(0).expect("ground");
    let ki = KeyframeInput {
        checkpoint: &checkpoint,
        fence: t.history_fence(),
        resize_lifetime: t.resize_lifetime(),
        resize_ordinal: t.resize_ordinal(),
        bridge_lines: 0,
        config_fp: 1,
        integration_on: t.shell_integration_posture()
            == aterm_core::terminal::ShellIntegrationPosture::On,
    };
    assert_eq!(w.write_keyframe(&ki), Ok(1));
    assert_eq!(word_at(&w, kf_commit_word(1)), 1, "seq 1 lands in slot 1");
    w.append_slice(b" world").expect("tail");
    assert_eq!(w.begin_keyframe(&ki), Ok(2));
    assert!(w.keyframe_in_progress());
    assert_eq!(word_at(&w, kf_commit_word(0)), 0, "being written");
    assert_eq!(w.begin_keyframe(&ki), Err(KeyframeError::InProgress));
    assert_eq!(w.finish_keyframe(), Ok(2));
    assert_eq!(word_at(&w, kf_commit_word(0)), 2);
    assert_eq!(w.finish_keyframe(), Err(WriteError::Sequence));
    let len = word_at(&w, kf_len_word(0)) as usize;
    let mut record = vec![0; len];
    w.store()
        .read(w.geometry().kf_slot_off(0), &mut record)
        .expect("inside");
    let kf = decode_keyframe(&record, 2).expect("decodes");
    assert_eq!(kf.head.tail_from, w.tail_commit(), "it rebased the tail");
    assert_eq!(w.tail_since_keyframe(), 0);

    w.note_marks(2).expect("marks");
    assert_eq!(word_at(&w, OFF_TAIL_MARKS), 2);
    assert_eq!(w.write_keyframe(&ki), Ok(3));
    assert_eq!(word_at(&w, OFF_TAIL_MARKS), 0, "reset after the commit");

    w.settle_clear().expect("settled");
    assert_eq!(word_at(&w, kf_commit_word(0)), 0, "the older slot is empty");
    assert_eq!(word_at(&w, kf_len_word(0)), 0);
    let mut zeroed = vec![0xFF; len];
    w.store()
        .read(w.geometry().kf_slot_off(0), &mut zeroed)
        .expect("inside");
    assert!(zeroed.iter().all(|b| *b == 0), "and its bytes are gone");
    assert_eq!(word_at(&w, kf_commit_word(1)), 3, "the newest stays");

    let geometry = Geometry::custom(64, 4096, 4096).expect("a tiny slot");
    let mut tiny =
        JournalWriter::create(VecStore::new(geometry.region_len), geometry, &WRITER).expect("c");
    assert!(matches!(
        tiny.begin_keyframe(&ki),
        Err(KeyframeError::TooLarge { .. })
    ));
    assert!(!tiny.keyframe_in_progress());
    assert_eq!(word_at(&tiny, kf_commit_word(1)), 0, "nothing stored");

    // A keyframe exactly a slot's size fits it, and a restore reads it.
    let (fit, len) = (0..64u8)
        .find_map(|n| {
            let mut t = Terminal::new(4, 20);
            t.process(&(0..n).map(|k| b'a' + k % 26).collect::<Vec<_>>());
            let mut w = small(4096, 4096);
            keyframe_of(&mut w, &t, 0);
            let len = word_at(&w, kf_len_word(1)) as usize;
            len.is_multiple_of(8).then_some((t, len))
        })
        .expect("a keyframe a multiple of 8 long");
    let geometry = Geometry::custom(len, 4096, 4096).expect("a slot of its size");
    let mut exact =
        JournalWriter::create(VecStore::new(geometry.region_len), geometry, &WRITER).expect("c");
    assert_eq!(keyframe_of(&mut exact, &fit, 0), 1);
    let restored = crate::recover::recover(
        exact.store(),
        crate::recover::RecoverPolicy {
            tail: true,
            config_fp: 1,
            scratch_nonce: [0xA5; 32],
        },
        Terminal::new,
    );
    assert!(
        matches!(restored, crate::recover::Recovery::Restored(ref r) if r.slot == 1),
        "{restored:?}"
    );

    // A settle while a keyframe is being written leaves every byte as it was, the slot
    // being written included (its older keyframe's bytes stay until the copy).
    let mut busy = small(4096, 4096);
    for _ in 0..2 {
        busy.write_keyframe(&ki).expect("committed");
    }
    assert_eq!(busy.begin_keyframe(&ki), Ok(3), "in slot 1, over seq 1");
    let before = region_of(&busy);
    busy.settle_clear().expect("nothing to do");
    assert!(region_of(&busy) == before, "no byte changed");
    // A record appended between a keyframe's begin and its finish, which the writer
    // accepts, lies past that keyframe's tail_from: it is tail since the keyframe, and a
    // settle's floor stays below it.
    busy.append_slice(b"after the capture").expect("appended");
    assert_eq!(busy.finish_keyframe(), Ok(3));
    assert_eq!(busy.tail_since_keyframe(), busy.tail_commit());
    busy.settle_clear().expect("settled");
    assert_eq!(busy.tail_floor(), 0, "the keyframe's tail_from");
}

/// The history ring: chunks are dense steps whose run the words bound; a chunk that would
/// straddle the end goes to the start after a zeroed gap, evicting what it overlaps; a
/// reset publishes the ring empty, and a clear zeroes every byte it ever held.
#[test]
fn the_history_ring_wraps_evicts_and_clears() {
    let mut w = small(4096, 1024);
    let mut t = Terminal::new(4, 30);
    for i in 0..200 {
        t.process(format!("history line {i}\r\n").as_bytes());
    }
    let fence = t.history_fence();
    let identity = HistoryIdentity::of(&fence);
    w.reset_history(identity, fence.oldest, 1024, false)
        .expect("reset");
    let mut row = fence.oldest;
    let mut seqs = Vec::new();
    // Chunks of 8, 3 and 5 lines, so the laps do not line up and a gap falls over an
    // earlier lap's chunk.
    let to_end = |w: &JournalWriter<VecStore>, hi: u64| {
        let at = (hi % 1024) as usize;
        let mut bytes = vec![0xFF; 1024 - at];
        w.store()
            .read(w.geometry().hist_off + at, &mut bytes)
            .expect("inside");
        bytes
    };
    let mut stale_gaps = 0;
    for take in [8, 3, 5].into_iter().cycle() {
        if row + 8 > fence.end {
            break;
        }
        let lines = t
            .history_lines_since_fence(&fence, row, take)
            .expect("holds");
        let hi = w.history.hi;
        let before = to_end(&w, hi);
        seqs.push(w.append_history(row, &lines).expect("fits"));
        row += lines.len() as u64;
        if (w.history.hi - 1) / 1024 > hi / 1024 {
            // It straddled the ring's end: the gap before it is zeroed, and touched.
            assert!(
                to_end(&w, hi).iter().all(|b| *b == 0),
                "a zeroed gap at {hi}"
            );
            assert_eq!(w.history.touched(), 1024);
            stale_gaps += usize::from(before.iter().any(|b| *b != 0));
        }
    }
    assert!(stale_gaps > 0, "premise: a gap over an earlier lap's chunk");
    assert!(seqs.windows(2).all(|p| p[1] == p[0] + 1), "dense steps");
    let lo = word_at(&w, OFF_HIST_LO);
    let hi = word_at(&w, OFF_HIST_HI);
    assert!(hi > 1024, "the ring wrapped");
    assert!(hi - lo <= 1024, "the run fits the ring");
    assert_eq!(word_at(&w, OFF_HIST_END_ROW), row);
    assert!(
        word_at(&w, OFF_HIST_LO_SEQ) > 0,
        "the oldest chunks were evicted"
    );

    let mut cleared = identity;
    cleared.clear_gen += 1;
    w.reset_history(cleared, row, 1024, true).expect("clear");
    assert_eq!(word_at(&w, OFF_HIST_LO), word_at(&w, OFF_HIST_HI), "empty");
    assert_eq!(word_at(&w, OFF_HIST_IDENTITY + 8), cleared.clear_gen);
    let mut ring = vec![0xFF; 1024];
    w.store()
        .read(w.geometry().hist_off, &mut ring)
        .expect("inside");
    assert!(ring.iter().all(|b| *b == 0), "no pre-clear byte survives");
    w.reset_history(cleared, row, 64, false)
        .expect("a tiny modulus");
    let lines = t
        .history_lines_since_fence(&fence, fence.oldest, 8)
        .expect("holds");
    assert!(matches!(
        w.append_history(fence.oldest, &lines),
        Err(WriteError::TooLarge { .. })
    ));
    // A reset's modulus is a multiple of 8, and never past the physical ring.
    w.reset_history(cleared, row, 1023, false)
        .expect("an odd cap");
    assert_eq!(w.history.cap(), 1016);
    w.reset_history(cleared, row, 4096, false)
        .expect("a cap past the ring");
    assert_eq!((w.history.cap(), word_at(&w, OFF_HIST_CAP)), (1024, 1024));

    // A chunk exactly the ring's size fits it; one exactly filling the room left to the
    // ring's end lands there, with no gap and nothing evicted.
    let (lines, len) = (1..64usize)
        .find_map(|n| {
            let text: String = (0..n).map(|k| char::from(b'a' + (k % 26) as u8)).collect();
            let lines = vec![Line::from(text.as_str())];
            let len = encode_history_chunk(0, 0, identity, &lines)
                .expect("encodes")
                .len();
            len.is_multiple_of(8).then_some((lines, len))
        })
        .expect("a chunk a multiple of 8 long");
    let mut fit = small(4096, 1024);
    fit.reset_history(identity, 0, len, false).expect("reset");
    assert_eq!(fit.append_history(0, &lines), Ok(0), "the whole ring");
    fit.reset_history(identity, 1, 2 * len, false)
        .expect("twice the modulus: the run starts halfway");
    assert_eq!(fit.append_history(1, &lines), Ok(1), "the room left");
    let len = len as u64;
    assert_eq!(
        (word_at(&fit, OFF_HIST_LO), word_at(&fit, OFF_HIST_HI)),
        (len, 2 * len)
    );
}

/// The words other threads own go through free functions over the store.
#[test]
fn other_threads_words_need_no_writer() {
    let w = small(4096, 4096);
    store_gathered(w.store(), 99).expect("gathered");
    store_resize_word(w.store(), 7, 12).expect("resize");
    store_config_fp(w.store(), 0xAB).expect("config");
    assert_eq!(word_at(&w, OFF_STREAM_GATHERED), 99);
    assert_eq!(word_at(&w, OFF_ENGINE_RESIZE_LIFETIME), 7);
    assert_eq!(word_at(&w, OFF_ENGINE_RESIZE_ORDINAL), 12);
    assert_eq!(word_at(&w, OFF_CONFIG_FP), 0xAB);
}

/// A line of program output nothing else in the region could hold by chance: random
/// hex, which LZ4 keeps as readable literals, so a byte search finds it wherever a
/// keyframe, a tail record or a history chunk still holds it.
fn token(i: u64) -> String {
    let mut x = i.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03;
    x ^= x >> 31;
    x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^= x >> 29;
    format!("Q{x:016x}{:016x}Z", x.rotate_left(17))
}

/// How many of the tokens `range` a byte search of the whole region still finds: every
/// 19-byte window of every run of lowercase hex the region holds, against the first 19
/// hex digits of each token.
fn tokens_left<S: RegionStore>(w: &JournalWriter<S>, range: std::ops::Range<u64>) -> usize {
    const NEEDLE: usize = 19;
    let mut bytes = vec![0u8; w.store().len()];
    w.store().read(0, &mut bytes).expect("the whole region");
    let mut windows = std::collections::HashSet::new();
    for run in bytes.split(|b| !matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        for window in run.windows(NEEDLE) {
            windows.insert(window.to_vec());
        }
    }
    range
        .filter(|i| windows.contains(&token(*i).as_bytes()[1..=NEEDLE]))
        .count()
}

fn keyframe_of<S: RegionStore>(w: &mut JournalWriter<S>, t: &Terminal, bridge: usize) -> u64 {
    let checkpoint = t.checkpoint_journal(bridge).expect("ground");
    w.write_keyframe(&KeyframeInput {
        checkpoint: &checkpoint,
        fence: t.history_fence(),
        resize_lifetime: t.resize_lifetime(),
        resize_ordinal: t.resize_ordinal(),
        bridge_lines: u32::try_from(bridge).unwrap_or(u32::MAX),
        config_fp: 1,
        integration_on: t.shell_integration_posture()
            == aterm_core::terminal::ShellIntegrationPosture::On,
    })
    .expect("a keyframe fits")
}

/// A clear (`clear`'s ED2 + ED3) leaves no byte from before it anywhere in the region
/// (the plan's privacy claim 4): not in the history ring (the worker's reset zeroes it),
/// not in the tail, not in the slot the forced keyframe did not take, and not in the
/// slot it did take past its own record, where an earlier, longer keyframe left its
/// tail bytes.
#[test]
fn a_clear_leaves_no_byte_from_before_it_in_the_region() {
    let geometry = Geometry::v1();
    let mut w =
        JournalWriter::create(VecStore::new(geometry.region_len), geometry, &WRITER).expect("c");
    let mut t = Terminal::new(10, 80);
    let fence = t.history_fence();
    w.reset_history(
        HistoryIdentity::of(&fence),
        fence.oldest,
        geometry.hist_cap_phys,
        false,
    )
    .expect("the ring");
    keyframe_of(&mut w, &t, 512);
    let mut exported = fence.oldest;
    for i in 0..700u64 {
        let line = format!("{}\r\n", token(i));
        t.process(line.as_bytes());
        w.append_slice(line.as_bytes()).expect("appended");
        if i == 599 {
            // A big keyframe (seq 2, slot 0): 512 lines of bridge.
            keyframe_of(&mut w, &t, 512);
        }
        if i % 100 == 99 {
            let fence = t.history_fence();
            let lines = t
                .history_lines_since_fence(&fence, exported, 1024)
                .expect("holds");
            w.append_history(exported, &lines).expect("fits");
            exported += lines.len() as u64;
        }
    }
    keyframe_of(&mut w, &t, 512); // seq 3, slot 1
    assert_eq!(
        tokens_left(&w, 0..700),
        700,
        "the control: all of it is there"
    );

    let clear = b"\x1b[H\x1b[2J\x1b[3J";
    t.process(clear);
    w.append_slice(clear).expect("the clear's slice");
    let fence = t.history_fence();
    w.reset_history(
        HistoryIdentity::of(&fence),
        fence.oldest,
        geometry.hist_cap_phys,
        true,
    )
    .expect("the worker's reset");
    assert_eq!(
        keyframe_of(&mut w, &t, 512),
        4,
        "the forced keyframe, slot 0"
    );
    w.settle_clear().expect("settled");
    assert_eq!(tokens_left(&w, 0..700), 0, "no pre-clear line survives");
    assert_eq!(word_at(&w, kf_commit_word(1)), 0, "the older slot is empty");
    assert_eq!(
        word_at(&w, kf_commit_word(0)),
        4,
        "the forced keyframe stays"
    );
    assert_eq!(w.tail_floor(), w.tail_commit(), "the tail is rebased");
}

/// A clear zeroes every byte the history ring ever held, even after the adaptive cap
/// shrank (plan §2.6 recomputes it every export tick) and the smaller ring wrapped: the
/// bytes the larger ring wrote past the smaller one's end are pre-clear lines too.
#[test]
fn a_clear_after_the_cap_shrank_zeroes_every_byte_the_ring_held() {
    let geometry = Geometry::custom(64 * 1024, 4096, 16 * 1024).expect("a test layout");
    let mut w =
        JournalWriter::create(VecStore::new(geometry.region_len), geometry, &WRITER).expect("c");
    let mut t = Terminal::new(4, 60);
    let fence = t.history_fence();
    let identity = HistoryIdentity::of(&fence);
    w.reset_history(identity, fence.oldest, 16 * 1024, false)
        .expect("the full ring");
    let mut n = 0u64;
    let mut exported = fence.oldest;
    // Export eight lines a chunk until the ring has written `upto` bytes and wrapped
    // twice under its current modulus.
    let pump = |w: &mut JournalWriter<VecStore>,
                t: &mut Terminal,
                n: &mut u64,
                exported: &mut u64,
                upto: usize| {
        let from = w.history.hi;
        while w.history.touched < upto || w.history.hi < from + 2 * word(w.history.cap) {
            for _ in 0..8 {
                t.process(format!("{}\r\n", token(*n)).as_bytes());
                *n += 1;
            }
            let fence = t.history_fence();
            let lines = t
                .history_lines_since_fence(&fence, *exported, 8)
                .expect("holds");
            w.append_history(*exported, &lines).expect("fits");
            *exported += lines.len() as u64;
        }
    };
    pump(&mut w, &mut t, &mut n, &mut exported, 16 * 1024);
    let before_shrink = n;
    w.reset_history(identity, exported, 4096, false)
        .expect("the cap shrank");
    pump(&mut w, &mut t, &mut n, &mut exported, 0);
    assert!(
        tokens_left(&w, 0..before_shrink) > 0,
        "the control: the larger ring's chunks are still there past the smaller one"
    );
    let mut cleared = identity;
    cleared.clear_gen += 1;
    w.reset_history(cleared, exported, 4096, true)
        .expect("the clear");
    let mut ring = vec![0xFF; geometry.hist_cap_phys];
    w.store()
        .read(geometry.hist_off, &mut ring)
        .expect("the ring");
    assert_eq!(
        ring.iter().filter(|b| **b != 0).count(),
        0,
        "every byte the ring ever held is zeroed"
    );
}

/// A stop anywhere inside the clear settle restores as the region did before it: the
/// newest keyframe and the tail past it are untouched, the other slot reads empty or
/// whole, and the floor never passes the newest keyframe's `tail_from`. So the settle
/// costs no restore anything, at any instruction.
#[test]
fn a_stop_anywhere_in_the_clear_settle_restores_as_before() {
    use crate::recover::{RecoverPolicy, Recovery, recover};
    use crate::store::CountdownStore;
    let geometry = Geometry::custom(64 * 1024, 1024, 4096).expect("a small region");
    // The session up to the settle, and the settle itself when `settle`. (Each build
    // gets a fresh engine, so the restores compare the keyframe by its positions, not
    // by the engine-lifetime numbers in its head.)
    let build = |budget: Option<usize>, settle: bool| {
        let store = CountdownStore::new(geometry.region_len, budget);
        let mut w = JournalWriter::create(store, geometry, &WRITER).expect("created");
        let mut t = Terminal::new(4, 30);
        keyframe_of(&mut w, &t, 64);
        for i in 0..40u64 {
            let line = format!("{}\r\n", &token(i)[..24]);
            t.process(line.as_bytes());
            w.append_slice(line.as_bytes()).expect("appended");
            if i == 29 {
                keyframe_of(&mut w, &t, 64); // a long keyframe: 26 lines of bridge
            }
        }
        let clear = b"\x1b[H\x1b[2J\x1b[3J";
        t.process(clear);
        w.append_slice(clear).expect("the clear");
        keyframe_of(&mut w, &t, 64); // the forced keyframe: short, in the long one's slot
        // A slice after it: the settle must keep the live window.
        t.process(b"after\r\n");
        w.append_slice(b"after\r\n").expect("after");
        let before = w.store().stores();
        if settle {
            w.settle_clear().expect("settled");
        }
        (w, before)
    };
    let restore = |region: &VecStore, tail: bool| match recover(
        region,
        RecoverPolicy {
            tail,
            config_fp: 1,
            scratch_nonce: [0xA5; 32],
        },
        Terminal::new,
    ) {
        Recovery::Restored(r) => (
            r.checkpoint,
            r.exact,
            r.losses,
            r.slot,
            (r.keyframe.seq, r.keyframe.tail_from, r.keyframe.stream_at),
            r.replayed_records,
        ),
        Recovery::Meta(why) => panic!("the META rung: {why:?}"),
    };
    let (unsettled, before) = build(None, false);
    let want = [true, false].map(|tail| restore(&unsettled.store().snapshot(), tail));
    assert!(want[0].1, "the control restores exactly: {:?}", want[0].2);
    assert_eq!(
        want[0].5, 1,
        "and replays the slice after the forced keyframe"
    );
    let (settled, _) = build(None, true);
    let total = settled.store().stores();
    assert!(total > before + 3, "the settle stores more than its words");
    for budget in before..=total {
        let (dying, _) = build(Some(budget), true);
        let region = dying.store().snapshot();
        for (i, tail) in [true, false].into_iter().enumerate() {
            assert_eq!(
                restore(&region, tail),
                want[i],
                "a stop after {} of the settle's {} stores (tail: {tail})",
                budget - before,
                total - before
            );
        }
    }
}

/// The settle's floor is the keyframe's `tail_from` itself: when the record after the
/// keyframe went to the ring's start behind a PAD, a floor on that record's start would
/// read as an overflow of the keyframe's tail. The restore after the settle still replays
/// it, exactly.
#[test]
fn the_settle_keeps_a_record_behind_a_pad() {
    use crate::recover::{RecoverPolicy, Recovery, recover};
    let mut w = small(96, 4096);
    let mut t = Terminal::new(4, 30);
    keyframe_of(&mut w, &t, 0);
    for bytes in [&b"abcde"[..], b"fghij", b"\x1b[3J\r\n"] {
        t.process(bytes);
        w.append_slice(bytes).expect("appended"); // 24 bytes each: [0, 72)
    }
    keyframe_of(&mut w, &t, 0); // the clear's keyframe at tail_from 72
    let after = b"0123456789abcdefghijklmn";
    t.process(after);
    w.append_slice(after)
        .expect("a PAD [72, 96), then the record at 96");
    assert_eq!(tail_header(&w, 72).kind, KIND_PAD);
    w.settle_clear().expect("settled");
    assert_eq!(w.tail_floor(), 72, "the floor is the keyframe's tail_from");
    let Recovery::Restored(r) = recover(
        w.store(),
        RecoverPolicy {
            tail: true,
            config_fp: 1,
            scratch_nonce: [0xA5; 32],
        },
        Terminal::new,
    ) else {
        panic!("the META rung");
    };
    assert!(r.exact, "{:?}", r.losses);
    assert_eq!(r.replayed_records, 1);
    assert_eq!(
        Terminal::from_checkpoint(&r.checkpoint).row_text(1),
        t.row_text(1)
    );
    // A record ending exactly one lap above the floor overwrites nothing the floor
    // guards: the floor stays on tail_from, not on the record behind the PAD.
    w.append_slice(b"0123456789abcdef")
        .expect("[136, 168), over [40, 72)");
    assert_eq!(w.tail_floor(), 72);
}

/// The adaptive cap (the plan's §2.6): a line limit at the corpus's rate with a quarter to
/// spare, never under 32 bytes a line or 256 KiB, never over the physical ring, the whole
/// ring for an unlimited scrollback, always a multiple of 8.
#[test]
fn the_adaptive_cap_follows_the_line_limit_and_the_corpus() {
    let phys = Geometry::v1().hist_cap_phys;
    assert_eq!(adaptive_hist_cap(None, 10, phys), phys / 8 * 8);
    assert_eq!(
        adaptive_hist_cap(Some(100_000), 10, phys),
        4_000_000,
        "32 B a line at least"
    );
    assert_eq!(adaptive_hist_cap(Some(100_000), 40, phys), 5_000_000);
    assert_eq!(
        adaptive_hist_cap(Some(100_000), 1_000, phys),
        phys / 8 * 8,
        "the ring"
    );
    assert_eq!(
        adaptive_hist_cap(Some(10), 40, phys),
        MIN_HIST_CAP,
        "the floor"
    );
    assert_eq!(
        adaptive_hist_cap(Some(10), 40, 4096),
        4096,
        "never past the ring"
    );
    assert_eq!(
        adaptive_hist_cap(Some(usize::MAX), u64::MAX, phys),
        phys / 8 * 8
    );
    for limit in [1, 999, 12_345, 1 << 20] {
        assert_eq!(adaptive_hist_cap(Some(limit), 33, phys) % 8, 0);
    }
}

/// The ring's writer is a value of its own over the region's store (the journal worker's,
/// apart from the parse thread's writer): its chunks and words are what a single writer's
/// would be, and its mirror says what the ring holds — the identity, the first and end
/// rows, the bytes a line, and the bytes it ever touched.
#[test]
fn the_ring_writer_stands_apart_from_the_journal_writer() {
    let mut w = small(4096, 8192);
    let mut t = Terminal::new(4, 30);
    for i in 0..100 {
        t.process(format!("history line {i}\r\n").as_bytes());
    }
    let fence = t.history_fence();
    let identity = HistoryIdentity::of(&fence);
    let mut ring = HistoryWriter::new(w.geometry());
    assert_eq!(ring.first_row(), None);
    assert_eq!(ring.bytes_per_line(), None);
    ring.reset_ring(w.store(), identity, fence.oldest, 8192, false)
        .expect("reset");
    assert_eq!(ring.end_row(), fence.oldest);
    let mut row = fence.oldest;
    while row < fence.end {
        let lines = t.history_lines_since_fence(&fence, row, 16).expect("holds");
        ring.append_chunk(w.store(), row, &lines).expect("fits");
        row += lines.len() as u64;
    }
    assert_eq!(ring.identity(), identity);
    assert_eq!(ring.first_row(), Some(fence.oldest));
    assert_eq!(ring.end_row(), fence.end);
    assert_eq!(word_at(&w, OFF_HIST_END_ROW), fence.end);
    assert_eq!(
        word_at(&w, OFF_HIST_HI) - word_at(&w, OFF_HIST_LO),
        ring.hi - ring.lo
    );
    let per_line = ring.bytes_per_line().expect("lines");
    assert!(per_line > 0 && per_line < 200, "{per_line}");
    assert!(ring.touched() > 0 && ring.touched() <= 8192);
    // The first chunk after a reset, straddling the new modulus's end, starts the run past
    // the gap: the run's first word, and its bytes a line, count the chunk alone.
    let lines = t
        .history_lines_since_fence(&fence, fence.oldest, 4)
        .expect("holds");
    let fresh = small(4096, 8192);
    let mut ring = HistoryWriter::new(fresh.geometry());
    ring.reset_ring(fresh.store(), identity, 0, 8192, false)
        .expect("reset");
    ring.append_chunk(fresh.store(), 0, &lines).expect("fits");
    let modulus = (ring.hi as usize).next_multiple_of(8) + 8;
    ring.reset_ring(fresh.store(), identity, 4, modulus, false)
        .expect("a modulus just past the run");
    ring.append_chunk(fresh.store(), 4, &lines)
        .expect("after the gap");
    let len = ring.hi - modulus as u64;
    assert_eq!(word_at(&fresh, OFF_HIST_LO), modulus as u64);
    assert_eq!(ring.bytes_per_line(), Some(len.div_ceil(4)));
    // The next straddles too and evicts the whole run: the run starts past its gap, at
    // that chunk alone.
    ring.append_chunk(fresh.store(), 8, &lines)
        .expect("after the gap");
    let start = 2 * modulus as u64;
    assert_eq!(
        (word_at(&fresh, OFF_HIST_LO), ring.first_row()),
        (start, Some(8))
    );
    assert_eq!(ring.bytes_per_line(), Some((ring.hi - start).div_ceil(4)));
    // The tail goes on beside it, through the journal writer.
    w.append_slice(b"the tail, beside the ring")
        .expect("appended");
    assert_eq!(
        word_at(&w, OFF_HIST_END_ROW),
        fence.end,
        "the ring's words stand"
    );
}
