// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;

/// One message each time `sink` passes `park`.
pub(super) fn parks(sink: &SinkWriter, park: Seam) -> mpsc::Receiver<()> {
    let (tx, rx) = mpsc::channel();
    sink.seam_for_testing(move |seam| {
        if seam == park {
            let _ = tx.send(());
        }
    });
    rx
}

#[test]
fn input_backlog_is_none_off_a_tty() {
    let (_reader, writer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
    let sink = SinkWriter::new(writer.as_raw_fd());
    assert_eq!(sink.write_frame(b"abc").expect("write"), 3);
    assert_eq!(sink.input_backlog(), None);
    assert_eq!(sink.tty_echo(), None);
    assert_eq!(
        sink.foreground_group(),
        None,
        "a socket leads no foreground"
    );
    assert_eq!(SinkWriter::new(-1).input_backlog(), None);
    assert_eq!(SinkWriter::new(-1).tty_echo(), None);
    assert_eq!(SinkWriter::new(-1).foreground_group(), None);
}

#[test]
fn the_input_hook_says_whether_anything_but_an_answer_landed() {
    let (mut reader, writer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
    let sink = SinkWriter::new(writer.as_raw_fd());
    let fired = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&fired);
    sink.install_input_hook(move || {
        seen.fetch_add(1, Ordering::SeqCst);
    });
    assert!(!sink.take_input_hook_driven(), "nothing landed");
    assert_eq!(sink.write_reply(b"\x1b[?62c").expect("answer"), 6);
    assert_eq!(fired.load(Ordering::SeqCst), 1, "an answer fires the hook");
    assert!(!sink.take_input_hook_driven(), "but drove nothing");

    assert_eq!(sink.write_frame_nonparking(b"k").expect("a key"), 1);
    assert_eq!(fired.load(Ordering::SeqCst), 1, "the arming took the fire");
    assert!(sink.take_input_hook_driven(), "the key marked it");
    assert!(!sink.take_input_hook_driven(), "taken");

    sink.rearm_input_hook();
    assert!(sink.wake_input_hook(), "an explicit wake fires");
    assert!(sink.take_input_hook_driven(), "and marks it");
    let interactive = sink
        .write_frame_interactive_with_receipt(b"i")
        .expect("an interactive key");
    assert_eq!(interactive.accepted(), 1);
    assert!(sink.take_input_hook_driven(), "an interactive key marks it");
    assert_eq!(sink.write_frame(b"p").expect("a blocking write"), 1);
    assert!(sink.take_input_hook_driven(), "a blocking write marks it");
    assert_eq!(sink.write_reply(b"\x1b[0n").expect("answer"), 4);
    assert!(
        !sink.take_input_hook_driven(),
        "an answer still marks nothing"
    );

    let mut drained = [0u8; 13];
    reader.read_exact(&mut drained).expect("drain small writes");
    sink.rearm_input_hook();
    let bulk_answer = vec![b'r'; SinkWriter::NONPARK_MAX + 1];
    assert_eq!(
        sink.write_reply(&bulk_answer).expect("bulk answer"),
        bulk_answer.len()
    );
    assert_eq!(
        fired.load(Ordering::SeqCst),
        3,
        "a bulk answer fires the rearmed hook"
    );
    assert!(
        !sink.take_input_hook_driven(),
        "a bulk answer over NONPARK_MAX still marks nothing"
    );
    let mut got_bulk = vec![0u8; bulk_answer.len()];
    reader.read_exact(&mut got_bulk).expect("drain bulk answer");

    sink.rearm_input_hook();
    let fence = sink.try_fence_egress().expect("fence");
    assert_eq!(sink.write_reply(b"\x1b[0n").expect("spilled answer"), 4);
    drop(fence);
    assert!(
        sink.wait_egress_drained_to_kernel(),
        "spilled answer drained"
    );
    assert_eq!(
        fired.load(Ordering::SeqCst),
        4,
        "a spilled answer fires the rearmed hook once drained"
    );
    assert!(
        !sink.take_input_hook_driven(),
        "a spilled answer drained by the drainer still marks nothing"
    );
    drop(writer);
}

#[test]
fn input_hook_fires_once_until_rearmed() {
    let (_reader, writer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
    let sink = SinkWriter::new(writer.as_raw_fd());
    assert_eq!(sink.write_frame(b"x").expect("write"), 1, "no hook yet");
    let fired = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&fired);
    sink.install_input_hook(move || {
        seen.fetch_add(1, Ordering::SeqCst);
    });
    sink.install_input_hook(|| panic!("the first install wins"));
    assert_eq!(fired.load(Ordering::SeqCst), 0, "installing fires nothing");

    assert_eq!(sink.write_frame(b"").expect("empty"), 0);
    assert_eq!(
        fired.load(Ordering::SeqCst),
        0,
        "an empty frame lands nothing"
    );
    assert_eq!(sink.write_frame(b"\r").expect("write"), 1);
    assert_eq!(sink.write_frame_nonparking(b"\x1b[B").expect("write"), 3);
    assert_eq!(fired.load(Ordering::SeqCst), 1, "two writes, one wake");

    sink.rearm_input_hook();
    assert_eq!(fired.load(Ordering::SeqCst), 1, "rearming fires nothing");
    assert_eq!(sink.write_frame(b"y").expect("write"), 1);
    assert_eq!(
        fired.load(Ordering::SeqCst),
        2,
        "rearmed, the next write wakes"
    );
    drop(writer);
}

#[test]
fn an_explicit_wake_shares_the_input_hooks_arming() {
    let (_reader, writer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
    let sink = SinkWriter::new(writer.as_raw_fd());
    assert!(!sink.wake_input_hook(), "no hook installed: nothing runs");
    let fired = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&fired);
    sink.install_input_hook(move || {
        seen.fetch_add(1, Ordering::SeqCst);
    });
    assert!(
        sink.wake_input_hook(),
        "the no-hook call armed nothing: the first wake runs"
    );
    assert!(!sink.wake_input_hook(), "fired, not rearmed: no second run");
    assert_eq!(sink.write_frame(b"\r").expect("write"), 1);
    assert_eq!(fired.load(Ordering::SeqCst), 1, "a write behind it waits");

    sink.rearm_input_hook();
    assert_eq!(sink.write_frame(b"x").expect("write"), 1);
    assert!(!sink.wake_input_hook(), "the write fired first");
    assert_eq!(fired.load(Ordering::SeqCst), 2);
    drop(writer);
}

#[test]
fn write_frame_is_whole_frame_atomic_across_threads() {
    let (mut reader, writer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
    let sink = Arc::new(SinkWriter::new(writer.as_raw_fd()));

    const N: u8 = 4;
    const LEN: usize = 128 * 1024; // > any default socket buffer -> forces short writes

    let reader_handle = thread::spawn(move || {
        let mut buf = vec![0u8; (N as usize) * LEN];
        reader.read_exact(&mut buf).expect("read_exact");
        buf
    });

    let mut handles = Vec::new();
    for i in 0..N {
        let s = Arc::clone(&sink);
        handles.push(thread::spawn(move || {
            let frame = vec![b'A' + i; LEN];
            assert_eq!(
                s.write_frame(&frame).expect("write_frame"),
                LEN,
                "whole frame accepted"
            );
        }));
    }
    for h in handles {
        h.join().expect("writer thread");
    }
    let buf = reader_handle.join().expect("reader thread");
    drop(writer); // keep the borrowed fd alive until here

    let runs = runs_of(&buf);
    assert_eq!(
        runs.len(),
        N as usize,
        "expected {N} contiguous frames; interleaving fragmented them into {} runs",
        runs.len()
    );
    for (byte, len) in &runs {
        assert_eq!(
            *len, LEN,
            "frame for byte {byte} was split — writers interleaved"
        );
    }
    let mut distinct: Vec<u8> = runs.iter().map(|(b, _)| *b).collect();
    distinct.sort_unstable();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        N as usize,
        "every frame's byte must appear exactly once"
    );
}

fn runs_of(buf: &[u8]) -> Vec<(u8, usize)> {
    let mut runs: Vec<(u8, usize)> = Vec::new();
    for &b in buf {
        match runs.last_mut() {
            Some((rb, n)) if *rb == b => *n += 1,
            _ => runs.push((b, 1)),
        }
    }
    runs
}

#[test]
fn receipt_twins_report_actual_acceptance_in_increasing_order() {
    let (mut reader, writer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
    let sink = SinkWriter::new(writer.as_raw_fd());

    let empty = sink
        .write_frame_with_receipt(b"")
        .expect("empty blocking receipt");
    assert_eq!(empty.accepted(), 0);
    assert_eq!(empty.order(), None, "empty writes do not enter sink order");

    let blocking = sink
        .write_frame_with_receipt(b"B")
        .expect("blocking receipt");
    let nonparking = sink
        .write_frame_nonparking_with_receipt(b"N")
        .expect("non-parking receipt");

    writer.set_nonblocking(true).expect("nonblocking writer");
    sink.note_master_nonblocking(true);
    let (immediate, immediate_order) = sink.try_write_frame_immediate_with_receipt(b"I");
    assert_eq!(immediate, ImmediateWrite::Full);

    assert_eq!(blocking.accepted(), 1);
    assert_eq!(nonparking.accepted(), 1);
    let blocking_order = blocking.order().expect("blocking accepted order");
    let nonparking_order = nonparking.order().expect("non-parking accepted order");
    let immediate_order = immediate_order.expect("immediate accepted order");
    assert!(blocking_order < nonparking_order);
    assert!(nonparking_order < immediate_order);

    let mut bytes = [0_u8; 3];
    reader.read_exact(&mut bytes).expect("read receipt frames");
    assert_eq!(&bytes, b"BNI");
}

#[test]
fn peer_closed_immediate_write_has_no_accepted_order() {
    let (reader, writer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
    writer.set_nonblocking(true).expect("nonblocking writer");
    let sink = SinkWriter::new(writer.as_raw_fd());
    sink.note_master_nonblocking(true);
    drop(reader);

    aterm_uds::deadline::once_released(|| {
        if aterm_uds::hangup::fd_peer_closed(writer.as_raw_fd()) {
            Ok(())
        } else {
            Err(io::ErrorKind::WouldBlock.into())
        }
    });

    let (write, order) = sink.try_write_frame_immediate_with_receipt(b"closed");
    assert_eq!(write, ImmediateWrite::BusyZero);
    assert_eq!(order, None, "a minted-but-unaccepted token stays private");
}

#[test]
fn accepted_order_follows_forced_spill_linearization_not_attempt_epoch() {
    let (mut reader, writer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
    let sink = Arc::new(SinkWriter::new(writer.as_raw_fd()));
    let held = sink.shared.lock.lock().unwrap();
    let initial_epoch = sink.input_epoch();
    let at_lock = parks(&sink, Seam::FrameLock);

    let blocked_sink = Arc::clone(&sink);
    let blocked = thread::spawn(move || {
        blocked_sink
            .write_frame_with_receipt(b"A")
            .expect("delayed blocking receipt")
    });
    // A passes this seam only after it found the spill empty, so from here its one way on is
    // the lock `held` keeps.
    at_lock
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("blocking writer reached the fd lock");
    assert_ne!(
        sink.input_epoch(),
        initial_epoch,
        "blocking writer reserved its earlier attempt"
    );

    let first = sink
        .write_frame_nonparking_with_receipt(b"B")
        .expect("later attempt spills without waiting");
    assert_eq!(first.accepted(), 1);
    assert!(!blocked.is_finished(), "fd-lock holder still blocks A");
    drop(held);

    let second = blocked.join().expect("blocking writer thread");
    assert_eq!(second.accepted(), 1);
    assert!(
        first.order().expect("B order") < second.order().expect("A order"),
        "receipts must follow spill/direct serialization, not attempt reservation"
    );

    let mut bytes = [0_u8; 2];
    reader
        .read_exact(&mut bytes)
        .expect("read reordered frames");
    assert_eq!(&bytes, b"BA", "kernel/spill order agrees with receipts");
}

#[test]
fn degraded_locked_fallback_rechecks_a_racing_spill() {
    let (_reader, writer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
    let sink = SinkWriter::new(writer.as_raw_fd());
    {
        let mut spill = sink.shared.spill.lock().unwrap();
        spill.buf.push_back(b'B');
        sink.shared.spilled_bytes.store(1, Ordering::Release);
        sink.shared
            .order_state
            .store(Shared::SPILL_ACTIVE_BIT | 1, Ordering::Release);
        spill.draining = true;
    }

    let receipt = sink
        .write_frame_locked(
            b"A",
            None,
            sink.shared.discard_epoch(),
            Landing::Driven,
            false,
        )
        .expect("fallback queues behind the racing spill");
    assert_eq!(receipt.accepted(), 1);
    assert_eq!(receipt.order(), Some(AcceptedOrder(2)));
    let spill = sink.shared.spill.lock().unwrap();
    assert_eq!(spill.buf.iter().copied().collect::<Vec<_>>(), b"BA");
}

#[test]
fn try_egress_drained_is_nonblocking_and_recovers_poison() {
    let (_reader, writer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
    let sink = Arc::new(SinkWriter::new(writer.as_raw_fd()));
    assert_eq!(sink.try_egress_drained_to_kernel(), Some(true));

    {
        let mut spill = sink.shared.spill.lock().unwrap();
        assert_eq!(
            sink.try_egress_drained_to_kernel(),
            None,
            "contended observation must not wait"
        );
        spill.buf.push_back(b'x');
    }
    assert_eq!(sink.try_egress_drained_to_kernel(), Some(false));
    sink.shared.spill.lock().unwrap().buf.clear();

    let poison_sink = Arc::clone(&sink);
    assert!(
        thread::spawn(move || {
            let _spill = poison_sink.shared.spill.lock().unwrap();
            panic!("poison spill mutex for recovery coverage");
        })
        .join()
        .is_err()
    );
    assert_eq!(
        sink.try_egress_drained_to_kernel(),
        Some(true),
        "a poisoned try_lock already owns the guard and remains non-parking"
    );
}

#[test]
fn public_writes_reserve_one_epoch_per_nonempty_attempt() {
    let (mut reader, writer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
    let sink = SinkWriter::new(writer.as_raw_fd());
    let large = vec![b'L'; SinkWriter::NONPARK_MAX + 1];
    let expected_len = 2 + large.len() + 1;
    let reader_handle = thread::spawn(move || {
        let mut bytes = vec![0_u8; expected_len];
        reader.read_exact(&mut bytes).expect("read all attempts");
        bytes
    });

    let initial = sink.input_epoch().0;
    assert_eq!(sink.write_frame(b"").expect("empty blocking write"), 0);
    assert_eq!(
        sink.input_epoch().0,
        initial,
        "empty write is not an attempt"
    );

    assert_eq!(sink.write_frame(b"B").expect("blocking write"), 1);
    assert_eq!(sink.input_epoch().0, initial + 1);

    assert_eq!(
        sink.write_frame_nonparking(b"N")
            .expect("small non-parking write"),
        1
    );
    assert_eq!(sink.input_epoch().0, initial + 2);

    assert_eq!(
        sink.write_frame_nonparking(&large)
            .expect("large delegated write"),
        large.len()
    );
    assert_eq!(
        sink.input_epoch().0,
        initial + 3,
        "delegation must not reserve a second epoch"
    );
    assert_eq!(sink.driven_input().0, 3, "every write so far was driven");

    assert_eq!(sink.write_reply(b"").expect("empty reply"), 0);
    assert_eq!(sink.write_reply(b"R").expect("a reply"), 1);
    assert_eq!(sink.input_epoch().0, initial + 4, "a reply is an attempt");
    assert_eq!(sink.driven_input().0, 3, "but nobody drove it");

    let bytes = reader_handle.join().expect("reader thread");
    assert_eq!(&bytes[..2], b"BN");
    assert!(bytes[2..expected_len - 1].iter().all(|byte| *byte == b'L'));
    assert_eq!(bytes[expected_len - 1], b'R');
    drop(writer);
}

#[test]
fn a_driven_input_watch_follows_its_own_sink_only() {
    let (mut reader, writer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
    let sink = SinkWriter::new(writer.as_raw_fd());
    let watch = sink.driven_input_watch();
    let other = SinkWriter::new(-1).driven_input_watch();
    let (before, theirs) = (watch.get(), other.get());
    assert_eq!(before, sink.driven_input());
    assert_eq!(sink.write_frame(b"K").expect("a driven write"), 1);
    assert_ne!(watch.get(), before, "a key moves it");
    assert_eq!(watch.get(), sink.driven_input());
    let keyed = watch.get();
    assert_eq!(sink.write_reply(b"R").expect("a reply"), 1);
    assert_eq!(watch.get(), keyed, "a reply does not");
    assert_eq!(other.get(), theirs, "another sink's count stays");
    let mut bytes = [0_u8; 2];
    reader.read_exact(&mut bytes).expect("both bytes");
    assert_eq!(&bytes, b"KR");
}

#[test]
fn immediate_write_refuses_contention_without_delayed_injection() {
    let (mut reader, writer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
    writer.set_nonblocking(true).expect("nonblocking writer");
    let sink = SinkWriter::new(writer.as_raw_fd());
    sink.note_master_nonblocking(true);

    let held = sink.shared.lock.lock().unwrap();
    assert_eq!(
        sink.try_write_frame_immediate(b"REJECTED"),
        ImmediateWrite::BusyZero,
        "try-lock contention must accept zero bytes"
    );
    drop(held);

    assert_eq!(
        sink.try_write_frame_immediate(b"accepted"),
        ImmediateWrite::Full
    );
    let mut got = [0_u8; 8];
    reader.read_exact(&mut got).expect("read accepted frame");
    assert_eq!(&got, b"accepted");
}

#[test]
fn immediate_write_refuses_spill_without_delayed_injection() {
    let (mut reader, writer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
    writer.set_nonblocking(true).expect("nonblocking writer");
    let sink = Arc::new(SinkWriter::new(writer.as_raw_fd()));
    sink.note_master_nonblocking(true);

    let mut filled = 0_usize;
    loop {
        match aterm_pty::write_some(writer.as_raw_fd(), &[b'.'; 4096]) {
            Ok(n) if n > 0 => filled += n,
            _ => break,
        }
    }
    assert!(filled > 0, "socket send buffer filled");
    assert_eq!(
        sink.write_frame_nonparking(b"OLDER").expect("spill older"),
        5
    );
    assert!(!sink.shared.spill_is_empty(), "older frame entered spill");

    let started = std::time::Instant::now();
    assert_eq!(
        sink.try_write_frame_immediate(b"REJECTED"),
        ImmediateWrite::BusyZero
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(60),
        "immediate refusal must not wait for the wedged peer"
    );

    let mut got = vec![0_u8; filled + 5];
    reader
        .read_exact(&mut got)
        .expect("drain fill and older frame");
    assert_eq!(&got[filled..], b"OLDER");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let settled = {
            let spill = sink.shared.spill.lock().unwrap();
            spill.buf.is_empty() && !spill.draining
        };
        if settled {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "spill did not settle");
        std::thread::sleep(std::time::Duration::from_millis(2));
    }

    assert_eq!(
        sink.try_write_frame_immediate(b"accepted"),
        ImmediateWrite::Full
    );
    let mut accepted = [0_u8; 8];
    reader
        .read_exact(&mut accepted)
        .expect("read accepted frame");
    assert_eq!(&accepted, b"accepted");
}

#[test]
fn owned_fd_stays_open_until_last_clone_drops() {
    let (mut reader, writer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
    let owned: OwnedFd = writer.into();
    let sink = Arc::new(SinkWriter::new_owned(owned));
    let clone = Arc::clone(&sink);

    drop(sink);
    assert_eq!(
        clone.project_fd_state(),
        (true, true),
        "fd_lifecycle projection: open and owned while a clone lives"
    );
    assert_eq!(
        clone
            .write_frame(b"alive")
            .expect("write while a clone holds the fd"),
        5
    );

    drop(clone);
    let mut buf = Vec::new();
    reader.read_to_end(&mut buf).expect("read_to_end");
    assert_eq!(
        &buf, b"alive",
        "peer got the bytes then EOF — fd closed on last clone drop"
    );
}

/// At the final exit the owner lets go of its master while clones still live: the peer reads
/// what was written and then end of file, and a sink that does not own its descriptor leaves it
/// alone.
#[test]
fn retiring_the_master_at_exit_ends_it_while_clones_live() {
    use std::io::Write as _;
    let (mut reader, writer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
    reader
        .set_read_timeout(Some(std::time::Duration::from_secs(60)))
        .expect("the hang detector");
    let sink = Arc::new(SinkWriter::new_owned(OwnedFd::from(writer)));
    let clone = Arc::clone(&sink);
    assert_eq!(clone.write_frame(b"before").expect("a write"), 6);
    sink.sever_input();
    assert!(sink.retire_master_at_exit(), "the owner retires its master");
    let mut buf = Vec::new();
    reader
        .read_to_end(&mut buf)
        .expect("end of file while both clones live, not the hang detector");
    assert_eq!(&buf, b"before");

    let (mut kept_reader, mut kept) = std::os::unix::net::UnixStream::pair().expect("socketpair");
    let borrowed = SinkWriter::new(kept.as_raw_fd());
    assert!(
        !borrowed.retire_master_at_exit(),
        "a borrowed descriptor is not retired"
    );
    kept.write_all(b"x")
        .expect("the borrowed descriptor still writes");
    let mut one = [0u8; 1];
    kept_reader
        .read_exact(&mut one)
        .expect("its peer still reads");
}

#[test]
fn small_write_below_spill_cap_returns_promptly_and_preserves_order() {
    let (mut reader, writer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
    let sink = Arc::new(SinkWriter::new(writer.as_raw_fd()));

    writer.set_nonblocking(true).expect("nonblocking for fill");
    let mut wedged = 0usize;
    loop {
        match aterm_pty::write_some(writer.as_raw_fd(), &[b'.'; 4096]) {
            Ok(n) if n > 0 => wedged += n,
            _ => break,
        }
    }
    writer.set_nonblocking(false).expect("back to blocking");
    assert!(wedged > 0, "buffer filled");

    let t0 = std::time::Instant::now();
    assert_eq!(
        sink.write_frame_nonparking(b"AAAA")
            .expect("spill accepted"),
        4
    );
    assert_eq!(
        sink.write_frame_nonparking(b"BBBB")
            .expect("spill accepted"),
        4
    );
    assert_eq!(sink.write_frame(b"CCCC").expect("spill accepted"), 4);
    assert!(
        t0.elapsed() < std::time::Duration::from_secs(60),
        "small writes below a fresh spill cap must not wait for the wedge to clear"
    );

    let (completion_tx, completion_rx) = std::sync::mpsc::channel();
    let completion_sink = sink.clone();
    let completion_thread = std::thread::spawn(move || {
        completion_tx
            .send(completion_sink.wait_egress_drained_to_kernel())
            .expect("completion receiver alive");
    });

    let mut got = Vec::new();
    let expect = wedged + 12;
    let mut chunk = [0u8; 65536];
    while got.len() < expect {
        let n = reader.read(&mut chunk).expect("drain");
        assert!(n > 0, "peer closed early");
        got.extend_from_slice(&chunk[..n]);
    }
    assert_eq!(&got[wedged..], b"AAAABBBBCCCC", "spill delivered in order");
    assert!(
        completion_rx
            .recv_timeout(std::time::Duration::from_secs(60))
            .expect("completion fence settles after drain")
    );
    completion_thread.join().expect("completion thread");

    assert_eq!(sink.write_frame_nonparking(b"D").expect("direct"), 1);
    let mut one = [0u8; 8];
    let n = reader.read(&mut one).expect("read D");
    assert_eq!(&one[..n], b"D");
}

#[test]
fn egress_completion_fails_closed_when_the_spill_peer_dies() {
    let (reader, writer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
    let sink = Arc::new(SinkWriter::new(writer.as_raw_fd()));

    writer.set_nonblocking(true).expect("nonblocking for fill");
    let mut wedged = 0usize;
    loop {
        match aterm_pty::write_some(writer.as_raw_fd(), &[b'.'; 4096]) {
            Ok(n) if n > 0 => wedged += n,
            _ => break,
        }
    }
    writer.set_nonblocking(false).expect("back to blocking");
    assert!(wedged > 0, "buffer filled");
    assert_eq!(sink.write_frame_nonparking(b"lost").expect("spill"), 4);
    assert!(!sink.egress_drained_to_kernel());

    drop(reader);
    assert!(
        !sink.wait_egress_drained_to_kernel(),
        "peer-death discard cannot satisfy the blocking completion fence"
    );
    assert!(
        !sink.wait_egress_drained_to_kernel(),
        "the failure is sticky: a second completion fence over the same sink still fails"
    );
    assert!(
        sink.egress_drained_to_kernel(),
        "nothing process-local remains after the discard: the polling handoff fence is settled"
    );
    assert_eq!(
        sink.try_egress_drained_to_kernel(),
        Some(true),
        "the non-parking twin agrees with the polling fence"
    );
}

#[test]
fn egress_drained_predicate_follows_the_spill() {
    let (mut reader, writer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
    let sink = Arc::new(SinkWriter::new(writer.as_raw_fd()));
    assert!(
        sink.egress_drained_to_kernel(),
        "a fresh sink has nothing in its process-local egress"
    );

    writer.set_nonblocking(true).expect("nonblocking for fill");
    let mut wedged = 0usize;
    loop {
        match aterm_pty::write_some(writer.as_raw_fd(), &[b'.'; 4096]) {
            Ok(n) if n > 0 => wedged += n,
            _ => break,
        }
    }
    writer.set_nonblocking(false).expect("back to blocking");
    assert!(wedged > 0, "buffer filled");

    assert_eq!(sink.write_frame_nonparking(b"held").expect("spill"), 4);
    assert!(
        !sink.egress_drained_to_kernel(),
        "tolerated bytes trapped in the spill must read as NOT drained"
    );

    let mut sink_bytes = Vec::new();
    let mut chunk = [0u8; 65536];
    while sink_bytes.len() < wedged + 4 {
        let n = reader.read(&mut chunk).expect("drain");
        assert!(n > 0, "peer closed early");
        sink_bytes.extend_from_slice(&chunk[..n]);
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !sink.egress_drained_to_kernel() {
        assert!(
            std::time::Instant::now() < deadline,
            "the spill never drained to the kernel"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(
        &sink_bytes[wedged..],
        b"held",
        "the held bytes reached the kernel"
    );
}

#[test]
fn declared_nonblocking_master_writes_whole_frames_in_order() {
    let (mut reader, writer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
    let sink = Arc::new(SinkWriter::new(writer.as_raw_fd()));
    assert!(
        !sink.shared.master_nonblocking.load(Ordering::Relaxed),
        "a fresh sink assumes the description may be BLOCKING (per-byte cadence)"
    );

    writer.set_nonblocking(true).expect("nonblocking master");
    sink.note_master_nonblocking(true);
    assert!(sink.shared.master_nonblocking.load(Ordering::Relaxed));

    assert_eq!(
        sink.write_frame_nonparking(b"whole-frame").expect("write"),
        11
    );
    assert!(
        sink.shared.spill_is_empty(),
        "an unwedged fd must not spill on the whole-frame unit"
    );
    let mut buf = [0u8; 16];
    let n = reader.read(&mut buf).expect("read");
    assert_eq!(&buf[..n], b"whole-frame");

    let mut wedged = 0usize;
    loop {
        match aterm_pty::write_some(writer.as_raw_fd(), &[b'.'; 4096]) {
            Ok(n) if n > 0 => wedged += n,
            _ => break,
        }
    }
    assert!(wedged > 0, "buffer filled");
    assert_eq!(sink.write_frame_nonparking(b"SPLIT").expect("accepted"), 5);

    let mut got = Vec::new();
    let expect = wedged + 5;
    let mut chunk = [0u8; 65536];
    while got.len() < expect {
        let n = reader.read(&mut chunk).expect("drain");
        assert!(n > 0, "peer closed early");
        got.extend_from_slice(&chunk[..n]);
    }
    assert_eq!(&got[wedged..], b"SPLIT", "spill delivered in order");
}

#[test]
fn contended_nonparking_write_concedes_within_a_bounded_spin() {
    const HOLD_CAP: std::time::Duration = std::time::Duration::from_secs(120);
    let (mut reader, writer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
    let sink = Arc::new(SinkWriter::new(writer.as_raw_fd()));

    let holding = Arc::new(AtomicBool::new(false));
    let released = Arc::new(AtomicBool::new(false));
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let holder = {
        let sink = Arc::clone(&sink);
        let holding = Arc::clone(&holding);
        let released = Arc::clone(&released);
        thread::spawn(move || {
            let guard = sink.shared.lock.lock().unwrap_or_else(|p| p.into_inner());
            holding.store(true, Ordering::SeqCst);
            let _ = release_rx.recv_timeout(HOLD_CAP);
            released.store(true, Ordering::SeqCst);
            drop(guard);
        })
    };
    while !holding.load(Ordering::SeqCst) {
        std::hint::spin_loop();
    }

    let t0 = std::time::Instant::now();
    assert_eq!(sink.write_frame_nonparking(b"K").expect("accepted"), 1);
    let took = t0.elapsed();
    assert!(
        !released.load(Ordering::SeqCst),
        "the write returned only after the holder let go: a wait on the holder, not a \
         bounded spin (took {took:?})"
    );
    assert!(
        took < aterm_spec::wait::MUST_HAPPEN,
        "the retry budget must be a bounded spin, not a wait on the holder (took {took:?})"
    );

    release_tx.send(()).expect("holder alive");
    holder.join().expect("holder thread");
    let mut buf = [0u8; 4];
    let n = reader.read(&mut buf).expect("read");
    assert_eq!(&buf[..n], b"K");
}
