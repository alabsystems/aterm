// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::layout::{OFF_HIST_CAP, PAGE};
use crate::records::{KIND_BYTES, KIND_PAD, TailHeader, decode_keyframe};
use crate::recover::ControlVerdict;
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

/// Header layout writes writer identity and geometry, publishing the magic word last.
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

/// Tail ring advances `tail_floor` onto record boundaries before overwriting and pads ring-end
/// wraps.
#[test]
fn the_tail_moves_its_floor_onto_boundaries_before_it_overwrites() {
    let mut w = small(96, 4096);
    w.append_slice(b"abcde").expect("one"); // 24 bytes: [0, 24)
    w.append_slice(b"fghij").expect("two"); // [24, 48)
    w.append_slice(b"klmno").expect("three"); // [48, 72)
    assert_eq!((w.tail_commit(), w.tail_floor()), (72, 0));
    assert_eq!(word_at(&w, OFF_TAIL_COMMIT), 72);
    assert_eq!(word_at(&w, OFF_STREAM_PROCESSED), 15);
    w.append_slice(b"0123456789abcdefghijklmn").expect("four");
    assert_eq!(w.tail_commit(), 136);
    assert_eq!(w.tail_floor(), 48, "the first record it does not overwrite");
    assert_eq!(word_at(&w, OFF_TAIL_FLOOR), 48);
    assert_eq!(tail_header(&w, 72).kind, KIND_PAD);
    assert_eq!(tail_header(&w, 72).record_len(), 24);
    let before = region_of(&w);
    w.settle_clear().expect("nothing to do");
    assert!(region_of(&w) == before, "no byte changed");
    let moved = tail_header(&w, 96);
    assert_eq!(
        (moved.kind, moved.payload_len, moved.stream_end),
        (KIND_BYTES, 24, 39)
    );
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
    let mut padding = [0xFF];
    exact
        .store()
        .read(exact.geometry().tail_off + 23, &mut padding)
        .expect("inside");
    assert_eq!(padding, [0]);
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
    w.append_synthesized(b"\x1bc").expect("five");
    assert_eq!(w.stream_processed(), 39);
    let mut big = small(64, 4096);
    big.append_slice(&[b'x'; 100]).expect("split");
    assert_eq!(big.stream_processed(), 100);
    assert!(big.tail_floor() > 0);
    let v1 = Geometry::v1();
    let mut full = JournalWriter::create(VecStore::new(v1.region_len), v1, &WRITER).expect("c");
    full.append_slice(&vec![b'x'; MAX_BYTES_PAYLOAD + 1])
        .expect("split");
    let two = 2 * TAIL_RECORD_HEADER + MAX_BYTES_PAYLOAD + 8;
    assert_eq!(full.tail_commit(), two as u64);
    let mut least = small(24, 4096);
    assert_eq!(
        least.append_resize(4, 20, ResizePolicy::Native, 1, 1),
        Err(WriteError::TooLarge { len: 40, cap: 24 })
    );
    assert_eq!(least.tail_commit(), 0);
}

/// Keyframes alternate slots by sequence parity, publish their commit word last, and clear the
/// older slot on `settle_clear`.
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

    let mut busy = small(4096, 4096);
    for _ in 0..2 {
        busy.write_keyframe(&ki).expect("committed");
    }
    assert_eq!(busy.begin_keyframe(&ki), Ok(3), "in slot 1, over seq 1");
    let before = region_of(&busy);
    busy.settle_clear().expect("nothing to do");
    assert!(region_of(&busy) == before, "no byte changed");
    busy.append_slice(b"after the capture").expect("appended");
    assert_eq!(busy.finish_keyframe(), Ok(3));
    assert_eq!(busy.tail_since_keyframe(), busy.tail_commit());
    busy.settle_clear().expect("settled");
    assert_eq!(busy.tail_floor(), 0, "the keyframe's tail_from");
}

/// History ring wraps after zeroing end gaps, evicts overlapped chunks, and zeroes touched bytes
/// on a clearing reset.
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
    w.reset_history(cleared, row, 1023, false)
        .expect("an odd cap");
    assert_eq!(w.history.cap(), 1016);
    w.reset_history(cleared, row, 4096, false)
        .expect("a cap past the ring");
    assert_eq!((w.history.cap(), word_at(&w, OFF_HIST_CAP)), (1024, 1024));

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

/// Pseudo-random hex token that LZ4 keeps as readable literals for region byte searches.
fn token(i: u64) -> String {
    let mut x = i.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03;
    x ^= x >> 31;
    x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^= x >> 29;
    format!("Q{x:016x}{:016x}Z", x.rotate_left(17))
}

/// Count how many tokens in `range` appear in a raw byte scan of the region.
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

/// A clear followed by `reset_history(..., true)`, a forced keyframe, and `settle_clear` leaves
/// no pre-clear bytes in the region.
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

/// A clearing reset zeroes every byte the history ring ever touched, even after the adaptive cap
/// shrank.
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

/// Stopping at any store inside `settle_clear` restores identically to the unsettled region.
#[test]
fn a_stop_anywhere_in_the_clear_settle_restores_as_before() {
    use crate::recover::{RecoverPolicy, Recovery, recover};
    use crate::store::CountdownStore;
    let geometry = Geometry::custom(64 * 1024, 1024, 4096).expect("a small region");
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

/// `settle_clear` sets `tail_floor` to the keyframe's `tail_from` so a record wrapped behind a
/// PAD still replays cleanly.
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
    w.append_slice(b"0123456789abcdef")
        .expect("[136, 168), over [40, 72)");
    assert_eq!(w.tail_floor(), 72);
}

/// `erase_region` zeroes all recorded content, preserves the turn-count marker, touches no
/// unwritten pages, and never restores a partial screen if interrupted.
#[test]
fn an_erased_region_holds_nothing_it_recorded_and_reads_as_no_region() {
    use crate::recover::{RecoverPolicy, Recovery, recover};
    use crate::store::{CountdownStore, Touched};
    let geometry = Geometry::custom(64 * 1024, 4096, 16 * 1024).expect("a small region");
    let build = |budget: Option<usize>, erase: bool| {
        let store = Touched::new(CountdownStore::new(geometry.region_len, budget), None);
        let mut w = JournalWriter::create(store, geometry, &WRITER).expect("created");
        let mut t = Terminal::new(4, 30);
        let fence = t.history_fence();
        w.reset_history(
            HistoryIdentity::of(&fence),
            fence.oldest,
            geometry.hist_cap_phys,
            false,
        )
        .expect("the ring");
        keyframe_of(&mut w, &t, 64);
        for i in 0..40u64 {
            let line = format!("{}\r\n", &token(i)[..24]);
            t.process(line.as_bytes());
            w.append_slice(line.as_bytes()).expect("appended");
            if i == 29 {
                keyframe_of(&mut w, &t, 64);
            }
        }
        let fence = t.history_fence();
        let lines = t
            .history_lines_since_fence(&fence, fence.oldest, 1024)
            .expect("holds");
        w.append_history(fence.oldest, &lines).expect("fits");
        store_ctl_count(w.store(), 47_000).expect("count before clear");
        let before = w.store().get_ref().stores();
        if erase {
            erase_region(w.store()).expect("erased");
        }
        (w, before)
    };
    let restores = |w: &JournalWriter<Touched<CountdownStore>>| {
        matches!(
            recover(
                &w.store().get_ref().snapshot(),
                RecoverPolicy {
                    tail: true,
                    config_fp: 1,
                    scratch_nonce: [0xA5; 32],
                },
                Terminal::new,
            ),
            Recovery::Restored(_)
        )
    };
    let (kept, _) = build(None, false);
    assert_eq!(
        tokens_left(&kept, 0..40),
        40,
        "the control: all of it is there"
    );
    assert!(restores(&kept), "the control restores a screen");

    let (erased, before) = build(None, true);
    assert_eq!(
        tokens_left(&erased, 0..40),
        0,
        "nothing it recorded is left"
    );
    let bytes = region_of(&erased);
    assert!(
        bytes.iter().enumerate().all(|(at, &b)| {
            (OFF_MAGIC..OFF_MAGIC + 8).contains(&at)
                || (OFF_CTL_COUNT..OFF_CTL_COUNT + 8).contains(&at)
                || b == 0
        }),
        "only the counter-only marker and count remain"
    );
    assert_eq!(
        crate::read_erased_turn_count(&erased.store().get_ref().snapshot()),
        Some(47_000)
    );
    assert!(!restores(&erased), "the META rung");
    let legacy = erased.store().get_ref().snapshot();
    legacy
        .store_u64_release(OFF_MAGIC, 0)
        .expect("old erased marker");
    assert_eq!(
        legacy.load_u64_acquire(OFF_CTL_COUNT).expect("count"),
        47_000
    );
    assert_eq!(crate::read_erased_turn_count(&legacy), None);
    assert!(crate::read_header(&legacy).is_err());
    assert_eq!(
        erased.store().touched_bytes(),
        kept.store().touched_bytes(),
        "no page nothing wrote is made resident"
    );
    let total = erased.store().get_ref().stores();
    assert!(total > before + 1, "the erase stores more than the magic");
    for budget in before..=total {
        let (dying, _) = build(Some(budget), true);
        let image = dying.store().get_ref().snapshot();
        assert_eq!(
            image.load_u64_acquire(OFF_CTL_COUNT).expect("count"),
            47_000
        );
        assert_eq!(
            crate::read_erased_turn_count(&image),
            (budget > before).then_some(47_000)
        );
        assert_eq!(
            restores(&dying),
            budget == before,
            "a stop after {} of the erase's {} stores",
            budget - before,
            total - before
        );
    }
}

/// `adaptive_hist_cap` scales with the line limit and bytes-per-line floor while staying
/// 8-byte aligned and within `[MIN_HIST_CAP, hist_cap_phys]`.
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

/// `HistoryWriter` operates independently over the store and tracks identity, row bounds,
/// bytes per line, and touched bytes.
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
    ring.append_chunk(fresh.store(), 8, &lines)
        .expect("after the gap");
    let start = 2 * modulus as u64;
    assert_eq!(
        (word_at(&fresh, OFF_HIST_LO), ring.first_row()),
        (start, Some(8))
    );
    assert_eq!(ring.bytes_per_line(), Some((ring.hi - start).div_ceil(4)));
    w.append_slice(b"the tail, beside the ring")
        .expect("appended");
    assert_eq!(
        word_at(&w, OFF_HIST_END_ROW),
        fence.end,
        "the ring's words stand"
    );
}

/// Build a test geometry with a `ctl_cap`-byte control slot between the tail and history rings.
fn with_control(ctl_cap: usize) -> Geometry {
    let g = Geometry::custom(4096, 1024, 4096).expect("a test layout");
    let hist_off = g.tail_off + g.tail_cap + ctl_cap;
    let geometry = Geometry {
        hist_off,
        region_len: (hist_off + g.hist_cap_phys).div_ceil(PAGE) * PAGE,
        ..g
    };
    assert_eq!(geometry.validate(), Ok(()));
    assert_eq!(geometry.ctl_cap(), ctl_cap);
    geometry
}

/// Alternating long (odd `i`) and short (even `i`) control payloads.
fn control_carry(i: u64) -> Vec<u8> {
    let n = if i % 2 == 1 { 300 } else { 7 };
    (0..n)
        .flat_map(|k| format!("turn {i}.{k} {}; ", k * 7919 % 1009).into_bytes())
        .collect()
}

fn control_of<V: RegionView + ?Sized>(view: &V, geometry: &Geometry) -> ControlVerdict {
    crate::recover::read_control(view, geometry)
}

/// Control records round-trip through the slot and zero any trailing bytes from a longer prior
/// record.
#[test]
fn a_control_record_round_trips_and_replaces_the_one_before() {
    let geometry = with_control(8192);
    let store = VecStore::new(geometry.region_len);
    assert_eq!(control_of(&store, &geometry), ControlVerdict::Empty);
    let mut ctl = ControlWriter::new();
    for i in 1..=4 {
        let carry = control_carry(i);
        assert_eq!(ctl.write(&store, &geometry, &carry), Ok(i), "write {i}");
        assert_eq!(
            control_of(&store, &geometry),
            ControlVerdict::Valid { seq: i, carry },
            "write {i}"
        );
        let len = usize::try_from(store.load_u64_acquire(OFF_CTL_LEN).expect("len")).expect("len");
        let mut past = vec![0xAA; geometry.ctl_cap() - len];
        store
            .read(geometry.ctl_off() + len, &mut past)
            .expect("the slot past the record");
        assert!(
            past.iter().all(|b| *b == 0),
            "write {i}: no byte of a longer record stays past this one"
        );
    }
    assert!(
        control_carry(1).len() > 4 * control_carry(2).len(),
        "premise: the long carries are long"
    );
}

/// An oversized control carry is refused without modifying the slot.
#[test]
fn a_control_carry_past_the_slot_is_refused_and_stores_nothing() {
    let geometry = with_control(256);
    let store = crate::store::CountdownStore::new(geometry.region_len, None);
    let mut ctl = ControlWriter::new();
    let small = b"turn 1 settled".to_vec();
    assert_eq!(ctl.write(&store, &geometry, &small), Ok(1));
    let stores = store.stores();
    let big: Vec<u8> = (0u64..4096)
        .flat_map(|k| k.wrapping_mul(0x9E37_79B9_7F4A_7C15).to_le_bytes())
        .collect();
    assert!(matches!(
        ctl.write(&store, &geometry, &big),
        Err(WriteError::TooLarge { cap: 256, .. })
    ));
    assert_eq!(store.stores(), stores, "nothing was stored");
    assert_eq!(ctl.seq(), 1);
    assert_eq!(
        control_of(&store, &geometry),
        ControlVerdict::Valid {
            seq: 1,
            carry: small
        }
    );
    let none = Geometry::custom(4096, 1024, 4096).expect("a test layout");
    assert!(matches!(
        ControlWriter::new().write(&VecStore::new(none.region_len), &none, b"x"),
        Err(WriteError::TooLarge { cap: 0, .. })
    ));
    assert_eq!(
        control_of(&VecStore::new(none.region_len), &none),
        ControlVerdict::NoSlot
    );
}

/// Run `writes` control writes stopped after `budget` primitive stores (with optional `fault`).
fn control_death(
    geometry: &Geometry,
    writes: u64,
    budget: Option<usize>,
    fault: Option<WriterFault>,
) -> (ControlVerdict, u64, usize) {
    let store = crate::store::CountdownStore::new(geometry.region_len, budget);
    store_ctl_count(&store, CONTROL_COUNT).expect("a dropped store is no refusal");
    let mut ctl = ControlWriter::new();
    if let Some(fault) = fault {
        ctl.plant(fault);
    }
    for i in 1..=writes {
        ctl.write(&store, geometry, &control_carry(i))
            .expect("a dropped store is no refusal");
    }
    let snapshot = store.snapshot();
    let count = snapshot
        .load_u64_acquire(crate::layout::OFF_CTL_COUNT)
        .expect("the count word");
    (control_of(&snapshot, geometry), count, store.stores())
}

const CONTROL_COUNT: u64 = 4_242;

/// Return any `(store_budget, verdict)` pairs across a 4-write run that violate the one-slot
/// control protocol or lose `OFF_CTL_COUNT`.
fn control_sweep(fault: Option<WriterFault>) -> Vec<(usize, ControlVerdict)> {
    let geometry = with_control(8192);
    let writes = 4;
    let (whole, _, stores) = control_death(&geometry, writes, None, fault);
    assert_eq!(
        whole,
        ControlVerdict::Valid {
            seq: writes,
            carry: control_carry(writes)
        },
        "premise: the whole run ends holding its last carry"
    );
    let valid = |seq: u64| ControlVerdict::Valid {
        seq,
        carry: control_carry(seq),
    };
    let mut ends = vec![control_death(&geometry, 0, None, fault).2];
    for i in 1..=writes {
        ends.push(control_death(&geometry, i, None, fault).2);
    }
    let mut bad = Vec::new();
    for k in 0..=stores {
        let (verdict, count, _) = control_death(&geometry, writes, Some(k), fault);
        let ended = ends.iter().position(|&at| at == k).map(|i| i as u64);
        let inside = ends.iter().position(|&at| at > k).map(|i| i as u64);
        let promised = match (ended, inside) {
            (Some(0), _) => verdict == ControlVerdict::Empty,
            (Some(i), _) => verdict == valid(i),
            (None, Some(i)) => verdict == ControlVerdict::Empty || verdict == valid(i),
            (None, None) => false,
        };
        let counted = k == 0 || count == CONTROL_COUNT;
        if !promised || !counted {
            bad.push((k, verdict));
        }
    }
    bad
}

/// Stopping at any store during control writes yields `Empty` or a complete record, whereas
/// `ControlPublishEarly` is caught.
#[test]
fn every_death_inside_a_control_write_leaves_no_carry_or_a_whole_one() {
    assert_eq!(control_sweep(None), Vec::new());
    let caught = control_sweep(Some(WriterFault::ControlPublishEarly));
    assert!(
        caught
            .iter()
            .any(|(_, v)| matches!(v, ControlVerdict::Record(_) | ControlVerdict::Length(_))),
        "the planted early commit is refused somewhere: {caught:?}"
    );
}

/// `carry_into` copies the control record atomically into a fresh region.
#[test]
fn a_fresh_region_is_given_the_control_record_whole() {
    let geometry = with_control(8192);
    let old = VecStore::new(geometry.region_len);
    let mut ctl = ControlWriter::new();
    for i in 1..=3 {
        ctl.write(&old, &geometry, &control_carry(i))
            .expect("written");
    }
    let want = control_of(&old, &geometry);
    assert!(matches!(want, ControlVerdict::Valid { seq: 3, .. }));
    let whole = crate::store::CountdownStore::new(geometry.region_len, None);
    ctl.carry_into(&old, &whole, &geometry).expect("copied");
    assert_eq!(control_of(&whole.snapshot(), &geometry), want);
    for k in 0..whole.stores() {
        let fresh = crate::store::CountdownStore::new(geometry.region_len, Some(k));
        let mut copy = ControlWriter::new();
        copy.carry_into(&old, &fresh, &geometry).expect("copied");
        let got = control_of(&fresh.snapshot(), &geometry);
        assert!(
            got == ControlVerdict::Empty || got == want,
            "a death after {k} stores: {got:?}"
        );
    }
    ctl.write(&whole, &geometry, &control_carry(4))
        .expect("written");
    assert_eq!(
        control_of(&whole.snapshot(), &geometry),
        ControlVerdict::Valid {
            seq: 4,
            carry: control_carry(4)
        }
    );
    let empty = VecStore::new(geometry.region_len);
    let fresh = crate::store::CountdownStore::new(geometry.region_len, None);
    ControlWriter::new()
        .carry_into(&empty, &fresh, &geometry)
        .expect("nothing to copy");
    assert_eq!(fresh.stores(), 0, "an empty slot copies nothing");
}

/// Single-byte corruption, mismatched commit sequence, or out-of-range length in the control
/// slot is refused.
#[test]
fn a_damaged_control_slot_is_refused_never_read_as_a_carry() {
    let geometry = with_control(8192);
    let store = VecStore::new(geometry.region_len);
    let mut ctl = ControlWriter::new();
    ctl.write(&store, &geometry, &control_carry(1))
        .expect("written");
    let len = usize::try_from(store.load_u64_acquire(OFF_CTL_LEN).expect("len")).expect("len");
    let mut record = vec![0u8; len];
    store.read(geometry.ctl_off(), &mut record).expect("record");
    for (at, byte) in record.iter().enumerate() {
        let damaged = store.snapshot();
        damaged
            .write(geometry.ctl_off() + at, &[byte ^ 0x5A])
            .expect("inside");
        assert!(
            matches!(control_of(&damaged, &geometry), ControlVerdict::Record(_)),
            "a changed byte {at} is refused"
        );
    }
    let other = store.snapshot();
    other.store_u64_release(OFF_CTL_COMMIT, 2).expect("word");
    assert!(matches!(
        control_of(&other, &geometry),
        ControlVerdict::Record(_)
    ));
    for bad in [0, geometry.ctl_cap() as u64 + 1, u64::MAX] {
        let long = store.snapshot();
        long.store_u64_release(OFF_CTL_LEN, bad).expect("word");
        assert_eq!(
            control_of(&long, &geometry),
            ControlVerdict::Length(bad),
            "length {bad}"
        );
    }
}
