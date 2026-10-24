// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The worker shell's pure parts: framing, faults, the thread spread, the stderr tail and the
//! shared channel's back-pressure.

use std::io::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;

use aterm_effects::aos_momentum::dist::{FrameFault, MAX_FRAME, Msg, Sender};
use aterm_effects::aos_momentum::lease::{LIMITS_V1, Token, WorkerId};
use aterm_effects::aos_momentum::m4::Spread;
use aterm_spec::wait::{MUST_HAPPEN, until_within};

use super::{
    Fault, FaultKind, PoolArgs, STDERR_TAIL, Tail, Threads, Timed, frame_bytes, percentiles,
    read_frame, read_worker,
};

fn progress(k: u16) -> Vec<u8> {
    let token = Token {
        search: 1,
        slot: 2,
        attempt: 1,
        worker: 0,
    };
    Msg::Progress { token, k }.encode()
}

/// A frame round-trips; a clean end between frames is `None`; every damaged frame is refused by
/// variant before its payload is trusted.
#[test]
fn frames_round_trip_and_each_damage_is_refused_by_name() {
    let payload = progress(3);
    let bytes = frame_bytes(&payload).expect("small");
    let mut two = bytes.clone();
    two.extend_from_slice(&bytes);
    let mut r = two.as_slice();
    assert_eq!(read_frame(&mut r), Ok(Some(payload.clone())));
    assert_eq!(read_frame(&mut r), Ok(Some(payload.clone())));
    assert_eq!(read_frame(&mut r), Ok(None), "a clean end");
    assert_eq!(
        Msg::decode(&payload, Sender::Worker).map(|m| m.tag()),
        Ok(4)
    );

    let corrupt =
        |r: Result<Option<Vec<u8>>, FrameFault>| matches!(r, Err(FrameFault::Corrupt { .. }));
    let cut = bytes.get(..5).expect("a header's start");
    assert!(corrupt(read_frame(&mut &cut[..])), "a cut header");
    let cut = bytes.get(..bytes.len() - 1).expect("a payload's start");
    assert!(corrupt(read_frame(&mut &cut[..])), "a cut payload");
    let mut crc = bytes.clone();
    if let Some(b) = crc.last_mut() {
        *b ^= 1;
    }
    assert!(corrupt(read_frame(&mut crc.as_slice())), "a bad CRC");
    let mut magic = bytes.clone();
    magic[0] ^= 1;
    assert!(corrupt(read_frame(&mut magic.as_slice())), "a bad magic");

    // A header that declares more than 64 KiB is refused before any payload byte is read.
    let mut big = bytes.clone();
    let len = u32::try_from(MAX_FRAME + 1).expect("small");
    big[4..8].copy_from_slice(&len.to_le_bytes());
    assert_eq!(
        read_frame(&mut big.as_slice()),
        Err(FrameFault::Oversize {
            len: u64::from(len)
        })
    );
    let at_bound = vec![0u8; MAX_FRAME];
    let framed = frame_bytes(&at_bound).expect("within astream's bound");
    assert_eq!(read_frame(&mut framed.as_slice()), Ok(Some(at_bound)));
}

/// Every fault spelling parses to its key; each malformed one is refused with a reason.
#[test]
fn fault_specs_parse_by_lease_and_refuse_by_reason() {
    let at = |kind, search, slot, attempt| Fault::At {
        kind,
        search,
        slot,
        attempt,
    };
    for (spec, want) in [
        ("kill@1.6.1:3", at(FaultKind::Kill(3), 1, 6, 1)),
        ("hang@2.1.1:2", at(FaultKind::Hang(2), 2, 1, 1)),
        ("slow@1.0.1:3", at(FaultKind::Slow(3), 1, 0, 1)),
        ("late@1.1.1", at(FaultKind::Late, 1, 1, 1)),
        ("late-lie@1.1.2", at(FaultKind::LateLie, 1, 1, 2)),
        ("dup@1.0.1", at(FaultKind::Dup, 1, 0, 1)),
        ("dup-lie@1.0.1", at(FaultKind::DupLie, 1, 0, 1)),
        ("post-lie@1.5.1", at(FaultKind::PostLie, 1, 5, 1)),
        ("lie-refusal@1.0.1", at(FaultKind::LieRefusal, 1, 0, 1)),
        ("lie-best@1.1.1", at(FaultKind::LieBest, 1, 1, 1)),
        ("inflate@1.0.1", at(FaultKind::Inflate, 1, 0, 1)),
        ("garbage@1.0.1", at(FaultKind::Garbage, 1, 0, 1)),
        ("flood@1.0.1", at(FaultKind::Flood, 1, 0, 1)),
        ("foreign@2", Fault::Foreign { worker: 2 }),
    ] {
        assert_eq!(Fault::parse(spec), Ok(want), "{spec}");
    }
    for spec in [
        "kill@1.6.1",
        "dup@1.0.1:3",
        "late@1.1",
        "late@1.1.1.1",
        "late@0.1.1",
        "late@1.1.0",
        "late@x.1.1",
        "nothing@1.1.1",
        "late",
        "foreign@x",
        "kill@1.0.1:x",
    ] {
        assert!(Fault::parse(spec).is_err(), "{spec}");
    }
    let t = Token {
        search: 1,
        slot: 6,
        attempt: 1,
        worker: 4,
    };
    let kill = Fault::parse("kill@1.6.1:3").expect("parses");
    assert_eq!(kill.on(&t), Some(FaultKind::Kill(3)), "any worker draws it");
    assert_eq!(
        kill.on(&Token { attempt: 2, ..t }),
        None,
        "the retry runs clean"
    );
    assert_eq!(kill.on(&Token { search: 2, ..t }), None, "another search");
    assert_eq!(Fault::Foreign { worker: 4 }.on(&t), None);
}

/// The thread spread runs every index exactly once at any width, so it changes only speed.
#[test]
fn threads_run_every_index_once() {
    for width in [1, 3, 16] {
        for n in [0, 1, 7, 50] {
            let hits: Vec<AtomicUsize> = (0..n).map(|_| AtomicUsize::new(0)).collect();
            Threads(width).run(n, &|i| {
                hits[i].fetch_add(1, Ordering::SeqCst);
            });
            assert!(
                hits.iter().all(|h| h.load(Ordering::SeqCst) == 1),
                "width {width}, n {n}"
            );
        }
    }
    let timed = Timed::new(&Threads(2));
    timed.run(4, &|_| {});
    timed.run(0, &|_| {});
    let calls: Vec<usize> = timed.calls().iter().map(|(n, _)| *n).collect();
    assert_eq!(calls, vec![4, 0]);
}

/// Nearest-rank percentiles, in milliseconds.
#[test]
fn percentiles_are_nearest_rank() {
    assert_eq!(percentiles(&[]), None);
    let ms: Vec<u64> = (1..=100).map(|i| i * 1_000_000).collect();
    assert_eq!(percentiles(&ms), Some((50.0, 95.0, 99.0)));
    assert_eq!(percentiles(&[7_000_000]), Some((7.0, 7.0, 7.0)));
}

/// The stderr tail keeps the last 64 KiB and counts each fault a worker says fired, across
/// chunk boundaries.
#[test]
fn the_stderr_tail_is_bounded_and_counts_fired_faults() {
    let mut tail = Tail::default();
    tail.take(b"aos-worker pid=7\naos-worker fault=kill@1.6.1:3 fi");
    tail.take(b"red\naos-worker fault=dup@1.0.1 fired\naos-worker fault=dup@1.0.1 fired\n");
    assert_eq!(
        tail.fired
            .iter()
            .map(|(k, v)| (k.as_str(), *v))
            .collect::<Vec<_>>(),
        vec![("dup@1.0.1", 2), ("kill@1.6.1:3", 1)]
    );
    tail.take(&vec![b'x'; STDERR_TAIL + 10]);
    assert_eq!(tail.bytes.len(), STDERR_TAIL);
}

/// `--lease-ms` overrides the window alone; the pool's size bounds the ledger's workers.
#[test]
fn pool_limits_are_v1_but_the_window_and_the_size() {
    let args = PoolArgs {
        workers: 3,
        lease_ms: Some(10_000),
        faults: Vec::new(),
    };
    let l = args.limits();
    assert_eq!(l.window_ns, 10_000_000_000);
    assert_eq!(l.max_workers, 3);
    assert_eq!(
        (l.hello_ns, l.grace_ns, l.max_attempts, l.items),
        (
            LIMITS_V1.hello_ns,
            LIMITS_V1.grace_ns,
            LIMITS_V1.max_attempts,
            LIMITS_V1.items
        )
    );
    let none = PoolArgs {
        lease_ms: None,
        ..args
    };
    assert_eq!(none.limits().window_ns, LIMITS_V1.window_ns);
}

/// Dropping the receiver releases blocked sends, which closes each reader's pipe and releases any
/// writer blocked in it; the drop runs, and joins every thread, on an assertion failure too.
#[derive(Default)]
struct Readers {
    rx: Option<mpsc::Receiver<(WorkerId, aterm_effects::aos_momentum::dist::Event)>>,
    readers: Vec<std::thread::JoinHandle<()>>,
    writers: Vec<std::thread::JoinHandle<()>>,
}

impl Drop for Readers {
    fn drop(&mut self) {
        drop(self.rx.take());
        for h in self.readers.drain(..).chain(self.writers.drain(..)) {
            let _ = h.join();
        }
    }
}

/// Three readers into one channel of capacity 2 × 3 (design §11.5): while the coordinator reads
/// nothing, at most capacity plus one in hand per reader are ever counted queued; once it reads,
/// every frame arrives, in order per worker, and nothing is lost.
#[test]
fn a_full_channel_blocks_its_readers_and_loses_nothing() {
    const READERS: u32 = 3;
    const FRAMES: u16 = 40;
    let cap = 2 * usize::try_from(READERS).expect("small");
    let (tx, rx) = mpsc::sync_channel(cap);
    let queued = Arc::new(AtomicUsize::new(0));
    let mut owned = Readers {
        rx: Some(rx),
        readers: Vec::new(),
        writers: Vec::new(),
    };
    for w in 0..READERS {
        let (reader, mut writer) = std::io::pipe().expect("a pipe");
        let (tx, queued) = (tx.clone(), Arc::clone(&queued));
        owned.readers.push(std::thread::spawn(move || {
            read_worker(WorkerId(w), reader, tx, queued)
        }));
        owned.writers.push(std::thread::spawn(move || {
            for k in 0..FRAMES {
                let bytes = frame_bytes(&progress(k)).expect("small");
                writer.write_all(&bytes).expect("the reader is alive");
            }
        }));
    }
    drop(tx);
    let bound = cap + usize::try_from(READERS).expect("small");
    until_within(MUST_HAPPEN, "all readers behind a full channel", || {
        (queued.load(Ordering::SeqCst) == bound).then_some(())
    });
    let rx = owned.rx.as_ref().expect("held until cleanup");
    let mut next = [0u16; 3];
    let mut eofs = 0;
    while let Ok((w, ev)) = rx.recv_timeout(MUST_HAPPEN) {
        queued.fetch_sub(1, Ordering::SeqCst);
        let i = usize::try_from(w.0).expect("small");
        match ev {
            aterm_effects::aos_momentum::dist::Event::Frame(p) => {
                assert_eq!(p, progress(next[i]), "worker {i} in order");
                next[i] += 1;
            }
            aterm_effects::aos_momentum::dist::Event::Eof => eofs += 1,
            aterm_effects::aos_momentum::dist::Event::Broken(f) => panic!("{f}"),
        }
        if eofs == READERS {
            break;
        }
    }
    assert_eq!(next, [FRAMES; 3]);
    assert_eq!(eofs, READERS);
    let mut panicked = false;
    for h in owned.readers.drain(..).chain(owned.writers.drain(..)) {
        panicked |= h.join().is_err();
    }
    assert!(!panicked, "a reader or writer panicked");
}
