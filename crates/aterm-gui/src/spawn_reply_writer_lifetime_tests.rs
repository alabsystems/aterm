// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{ReportRoute, spawn_reply_writer};
use aterm_session::sink::SinkWriter;
use std::io::Read as _;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::Duration;

use aterm_spec::wait::MUST_HAPPEN;

fn sink_and_peer() -> (Arc<SinkWriter>, UnixStream) {
    let (peer, writer) = UnixStream::pair().expect("socketpair");
    peer.set_read_timeout(Some(MUST_HAPPEN))
        .expect("read timeout");
    writer.set_nonblocking(true).expect("O_NONBLOCK");
    let owned: std::os::fd::OwnedFd = writer.into();
    let sink = SinkWriter::new_owned(owned);
    sink.note_master_nonblocking(true);
    (Arc::new(sink), peer)
}

const SPILL_FRAME: usize = 1 << 20;

fn spill_is_delivered(sink: &SinkWriter, peer: &mut UnixStream) {
    let frame: Vec<u8> = (0..=250u8).cycle().take(SPILL_FRAME).collect();
    let receipt = sink
        .write_frame_interactive_with_receipt(&frame)
        .expect("the frame is accepted, its tail spilled");
    assert_eq!(receipt.accepted(), frame.len(), "the whole frame is taken");
    assert!(
        sink.spilled_bytes() > 0,
        "the frame must spill, or the arranger is never asked"
    );
    let mut got = vec![0u8; frame.len()];
    peer.read_exact(&mut got)
        .expect("every spilled byte is delivered");
    assert!(got == frame, "the spill is delivered in order");
}

fn wait_finished(join: &std::thread::JoinHandle<()>, what: &str) {
    let exited = aterm_spec::wait::within(MUST_HAPPEN, Duration::from_millis(1), || {
        join.is_finished().then_some(())
    });
    assert!(exited.is_some(), "{what} never exited");
}

#[cfg(target_os = "macos")]
#[test]
fn the_reply_writer_runs_at_the_class_the_keystroke_path_needs() {
    let (sink, mut peer) = sink_and_peer();
    let (reply_tx, join) = spawn_reply_writer(sink.clone(), ReportRoute::default()).expect("spawn");
    assert!(reply_tx.submit(Arc::from(&b"ok"[..])), "queued");
    let mut buf = [0u8; 2];
    peer.read_exact(&mut buf).expect("the reply is written");
    assert_eq!(&buf, b"ok");
    let observed = crate::qos::class_of(&join).expect("the writer's QoS class");
    assert_eq!(
        observed,
        crate::qos::class_for(crate::qos::Role::Interactive),
        "the reply writer runs at 0x{observed:x}, not the class the keystroke \
         path needs: it holds the sink lock the UI thread's own write spins on"
    );
    drop(reply_tx);
    drop(sink);
    wait_finished(&join, "the reply writer");
}

#[test]
fn a_reattached_writer_ends_with_its_reader_and_the_first_with_the_sink() {
    let (sink, mut peer) = sink_and_peer();
    let (first_tx, first_join) =
        spawn_reply_writer(sink.clone(), ReportRoute::default()).expect("spawn");
    let (second_tx, second_join) =
        spawn_reply_writer(sink.clone(), ReportRoute::default()).expect("re-attach");
    drop(first_tx);
    drop(second_tx);
    wait_finished(&second_join, "the re-attached reply writer");
    spill_is_delivered(&sink, &mut peer);
    drop(sink);
    wait_finished(&first_join, "the first reply writer");
    let n = peer
        .read(&mut [0u8; 1])
        .expect("EOF, not a read timeout: the master fd must have closed");
    assert_eq!(n, 0, "EOF");
}

#[test]
fn a_failed_spawn_leaves_the_arranger_slot_to_the_retried_attach() {
    let (sink, mut peer) = sink_and_peer();
    super::REPLY_WRITER_SPAWN_FAULT.with(|f| f.set(true));
    spawn_reply_writer(sink.clone(), ReportRoute::default())
        .expect_err("the injected spawn failure surfaces");
    assert!(
        !super::take_injected_reply_writer_spawn_fault(),
        "the fault is one-shot: consumed by the failed attempt"
    );
    let (retry_tx, retry_join) =
        spawn_reply_writer(sink.clone(), ReportRoute::default()).expect("the retry spawns");
    assert!(retry_tx.submit(Arc::from(&b"ok"[..])), "queued");
    let mut buf = [0u8; 2];
    peer.read_exact(&mut buf)
        .expect("the retry's writer writes");
    assert_eq!(&buf, b"ok");
    drop(retry_tx);
    spill_is_delivered(&sink, &mut peer);
    drop(sink);
    wait_finished(&retry_join, "the retried reply writer");
    let n = peer
        .read(&mut buf)
        .expect("EOF, not a read timeout: the master fd must have closed");
    assert_eq!(n, 0, "EOF");
}
