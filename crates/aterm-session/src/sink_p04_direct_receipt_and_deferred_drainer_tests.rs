// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::io::Read as _;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicUsize, Ordering};

fn sink_and_reader() -> (Arc<SinkWriter>, UnixStream) {
    let (reader, writer) = UnixStream::pair().expect("socketpair");
    reader
        .set_read_timeout(Some(std::time::Duration::from_secs(60)))
        .expect("timeout");
    let owned: OwnedFd = writer.into();
    (Arc::new(SinkWriter::new_owned(owned)), reader)
}

fn read_exactly(reader: &mut UnixStream, want: usize) -> Vec<u8> {
    let mut out = vec![0u8; want];
    reader.read_exact(&mut out).expect("bytes reach the peer");
    out
}

#[test]
fn a_whole_frame_on_the_direct_lane_is_direct_and_a_spilled_one_is_not() {
    let (sink, mut reader) = sink_and_reader();
    let direct = sink
        .write_frame_nonparking_with_receipt(b"a")
        .expect("write");
    assert!(
        direct.is_direct(),
        "an uncontended keystroke goes straight to the kernel"
    );
    assert_eq!(direct.accepted(), 1);
    let held = sink.shared.lock.lock().unwrap();
    let spilled = sink
        .write_frame_nonparking_with_receipt(b"b")
        .expect("spill");
    assert!(!spilled.is_direct(), "a conceded frame sits in the spill");
    assert_eq!(
        spilled.accepted(),
        1,
        "…but it IS accepted (ordered delivery)"
    );
    drop(held);
    assert_eq!(read_exactly(&mut reader, 2), b"ab");
}

#[test]
fn the_egress_fence_keeps_every_frame_out_of_the_kernel_until_it_lifts() {
    let (sink, mut reader) = sink_and_reader();
    let mid_frame = sink.shared.lock.lock().unwrap();
    assert!(sink.try_fence_egress().is_none(), "a frame part-way in");
    drop(mid_frame);
    let fence = sink.try_fence_egress().expect("nothing in flight");
    assert_eq!(sink.write_frame_nonparking(b"after").expect("accepted"), 5);
    assert_eq!(sink.spilled_bytes(), 5, "queued whole behind the fence");
    reader.set_nonblocking(true).expect("nonblocking");
    assert_eq!(
        reader.read(&mut [0u8; 8]).map_err(|e| e.kind()),
        Err(std::io::ErrorKind::WouldBlock),
        "no byte reached the kernel"
    );
    reader.set_nonblocking(false).expect("blocking");
    drop(fence);
    assert_eq!(read_exactly(&mut reader, 5), b"after");
}

#[test]
fn a_concession_with_an_arranger_defers_the_spawn_to_the_worker() {
    static POKES: AtomicUsize = AtomicUsize::new(0);
    let (sink, mut reader) = sink_and_reader();
    sink.install_spill_arranger(|| {
        POKES.fetch_add(1, Ordering::SeqCst);
    });
    let held = sink.shared.lock.lock().unwrap();
    let receipt = sink
        .write_frame_nonparking_with_receipt(b"xyz")
        .expect("concede to the spill");
    assert_eq!(receipt.accepted(), 3);
    assert!(!receipt.is_direct());
    assert_eq!(
        POKES.load(Ordering::SeqCst),
        1,
        "the arranger was poked exactly once"
    );
    {
        let s = sink.shared.spill.lock().unwrap();
        assert!(
            s.draining && s.arrange_pending,
            "committed under a PENDING mark, no thread yet"
        );
        assert_eq!(s.buf.len(), 3);
    }
    let receipt2 = sink
        .write_frame_nonparking_with_receipt(b"!")
        .expect("append");
    assert_eq!(receipt2.accepted(), 1);
    assert_eq!(
        POKES.load(Ordering::SeqCst),
        2,
        "pending ⇒ the poke is renewed"
    );
    drop(held);
    assert_eq!(sink.try_egress_drained_to_kernel(), Some(false));
    sink.arrange_pending_drainer();
    assert_eq!(read_exactly(&mut reader, 4), b"xyz!");
    let t0 = std::time::Instant::now();
    while sink.try_egress_drained_to_kernel() != Some(true) {
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(60),
            "drainer never emptied"
        );
        aterm_uds::deadline::poll_yield();
    }
    assert!(!sink.shared.spill.lock().unwrap().arrange_pending);
}

#[test]
fn without_an_arranger_the_concession_arranges_inline_as_before() {
    let (sink, mut reader) = sink_and_reader();
    let held = sink.shared.lock.lock().unwrap();
    let receipt = sink
        .write_frame_nonparking_with_receipt(b"q")
        .expect("concede");
    assert_eq!(receipt.accepted(), 1);
    {
        let s = sink.shared.spill.lock().unwrap();
        assert!(
            s.draining && !s.arrange_pending,
            "inline arrangement: the thread exists"
        );
    }
    drop(held);
    assert_eq!(read_exactly(&mut reader, 1), b"q");
}

#[test]
fn the_drainer_never_parks_in_its_write_under_the_spill_mutex() {
    for declared in [false, true] {
        let (reader, writer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        writer.set_nonblocking(true).expect("nonblocking for fill");
        while let Ok(n) = aterm_pty::write_some(writer.as_raw_fd(), &[b'.'; 4096]) {
            if n == 0 {
                break;
            }
        }
        writer.set_nonblocking(declared).expect("the description");
        let sink = Arc::new(SinkWriter::new(writer.as_raw_fd()));
        sink.note_master_nonblocking(declared);
        let (at_tx, at_rx) = std::sync::mpsc::channel::<bool>();
        let at_tx = Mutex::new(at_tx);
        let shared = Arc::downgrade(&sink.shared);
        let fired = AtomicBool::new(false);
        sink.seam_for_testing(move |seam| {
            if seam != Seam::DrainWrite || fired.swap(true, Ordering::SeqCst) {
                return;
            }
            let free = shared
                .upgrade()
                .is_some_and(|shared| shared.spill.try_lock().is_ok());
            let _ = at_tx.lock().expect("at").send(free);
        });
        assert_eq!(sink.write_frame_nonparking(b"s").expect("spilled"), 1);
        let free = at_rx
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the drainer reached its write");
        assert_eq!(
            free, !declared,
            "declared={declared}: the mutex at the write"
        );
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let key = {
            let sink = Arc::clone(&sink);
            std::thread::spawn(move || {
                let _ = done_tx.send(sink.write_frame_nonparking(b"k").map_err(|e| e.kind()));
            })
        };
        assert_eq!(
            done_rx
                .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                .expect("the key never queued behind a parked drainer"),
            Ok(1)
        );
        key.join().expect("the key");
        drop(reader);
    }
}

#[test]
fn the_guarded_writes_rest_defers_its_drainer_to_the_arranger() {
    static POKES: AtomicUsize = AtomicUsize::new(0);
    for arranged in [true, false] {
        let (read_end, write_end) = aterm_uds::pipe().expect("pipe");
        let (read_end, write_end): (OwnedFd, OwnedFd) = (read_end.into(), write_end.into());
        aterm_pty::set_nonblocking(write_end.as_raw_fd(), true).expect("nonblocking");
        let mut filled = 0_usize;
        while let Ok(n) = aterm_pty::write_some(write_end.as_raw_fd(), &[b'.'; 4096]) {
            filled += n;
        }
        let sink = SinkWriter::new_owned(write_end);
        sink.note_master_nonblocking(true);
        if arranged {
            sink.install_spill_arranger(|| {
                POKES.fetch_add(1, Ordering::SeqCst);
            });
        }
        let mut reader = std::fs::File::from(read_end);
        let room = filled.min(8 * 1024);
        let mut made = vec![0_u8; room];
        reader.read_exact(&mut made).expect("make partial room");

        let frame = vec![b'P'; SinkWriter::IMMEDIATE_FRAME_MAX];
        let (outcome, _) = sink.try_write_frame_whole_with_receipt(&frame);
        assert!(
            matches!(outcome, ImmediateWrite::Queued { accepted } if accepted < frame.len()),
            "{outcome:?}"
        );
        {
            let s = sink.shared.spill.lock().unwrap();
            assert!(s.draining, "a drainer owes the rest");
            assert_eq!(s.arrange_pending, arranged, "pending iff deferred");
        }
        if arranged {
            assert_eq!(POKES.load(Ordering::SeqCst), 1, "poked exactly once");
            sink.arrange_pending_drainer();
        }
        aterm_pty::set_nonblocking(reader.as_raw_fd(), true).expect("nonblocking reader");
        let want = filled - room + frame.len();
        let mut landed = Vec::with_capacity(want);
        let came = aterm_spec::wait::within(
            aterm_spec::wait::MUST_HAPPEN,
            std::time::Duration::from_millis(5),
            || {
                let mut buf = [0_u8; 16 * 1024];
                if let Ok(n) = reader.read(&mut buf) {
                    landed.extend_from_slice(&buf[..n]);
                }
                (landed.len() >= want).then_some(())
            },
        );
        assert!(came.is_some(), "{arranged}: {} of {want}", landed.len());
        assert!(landed[..filled - room].iter().all(|b| *b == b'.'));
        assert!(landed[filled - room..].iter().all(|b| *b == b'P'));
    }
}
