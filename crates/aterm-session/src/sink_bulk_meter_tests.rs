// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::io::Read;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

fn sink_and_reader() -> (Arc<SinkWriter>, UnixStream) {
    let (reader, writer) = UnixStream::pair().expect("socketpair");
    reader
        .set_read_timeout(Some(Duration::from_secs(60)))
        .expect("timeout");
    let owned: OwnedFd = writer.into();
    (Arc::new(SinkWriter::new_owned(owned)), reader)
}

fn bracketed(body: &[u8]) -> Vec<u8> {
    [PASTE_OPEN, body, PASTE_CLOSE].concat()
}

#[test]
fn a_stop_cut_keeps_the_frame_well_formed() {
    let plain = "ab\u{e9}cd".as_bytes(); // a b [c3 a9] c d
    assert_eq!(bulk_stop_cut(plain, 0), (0, plain.len()));
    assert_eq!(bulk_stop_cut(plain, 1), (1, plain.len()));
    assert_eq!(bulk_stop_cut(plain, 3), (4, plain.len()));
    let framed = bracketed(b"hello");
    let len = framed.len();
    assert_eq!(bulk_stop_cut(&framed, 2), (6, len - 6));
    assert_eq!(bulk_stop_cut(&framed, 8), (8, len - 6));
    assert_eq!(bulk_stop_cut(&framed, len - 3), (len - 3, len - 3));
    assert_eq!(bulk_stop_cut(&framed, 0), (0, len));
}

#[test]
fn a_frame_stopped_before_it_begins_writes_nothing() {
    let (sink, mut reader) = sink_and_reader();
    let meter = BulkMeter::new();
    meter.request_stop();
    let receipt = sink
        .write_frame_metered_with_receipt(&bracketed(&[b'x'; 4096]), &meter)
        .expect("write");
    assert_eq!(receipt.accepted(), 0);
    assert_eq!(meter.progress().state, BulkState::Stopped);
    assert_eq!(meter.progress().sent, 0);
    sink.write_frame(b"k").expect("the next input goes through");
    let mut one = [0u8; 1];
    reader.read_exact(&mut one).expect("read");
    assert_eq!(&one, b"k", "the stopped frame sent no byte");
}

#[test]
fn a_metered_frame_reports_progress_and_a_stop_drops_the_unsent_middle() {
    let (sink, mut reader) = sink_and_reader();
    let body = vec![b'p'; 1 << 20];
    let frame = bracketed(&body);
    let meter = Arc::new(BulkMeter::new());
    let writer = {
        let (sink, meter, frame) = (sink.clone(), meter.clone(), frame.clone());
        std::thread::spawn(move || {
            let receipt = sink
                .write_frame_metered_with_receipt(&frame, &meter)
                .expect("write");
            sink.write_frame(b"K").expect("the key after it");
            receipt
        })
    };
    let mut got = vec![0u8; 64 * 1024];
    reader.read_exact(&mut got).expect("read a prefix");
    let deadline = Instant::now() + Duration::from_secs(60);
    let progress = loop {
        let p = meter.progress();
        if p.sent >= got.len() as u64 || Instant::now() > deadline {
            break p;
        }
        std::thread::sleep(Duration::from_millis(1));
    };
    assert_eq!(progress.state, BulkState::Writing);
    assert_eq!(progress.total, frame.len() as u64);
    assert!(progress.sent >= got.len() as u64, "{progress:?}");
    assert!(progress.sent < frame.len() as u64, "the writer is parked");
    meter.request_stop();
    let mut rest = Vec::new();
    let mut chunk = [0u8; 8192];
    while !rest.ends_with(b"K") {
        let n = reader.read(&mut chunk).expect("read the rest");
        assert!(n > 0, "peer open");
        rest.extend_from_slice(&chunk[..n]);
    }
    let receipt = writer.join().expect("writer");
    got.extend_from_slice(&rest);
    assert_eq!(got.last(), Some(&b'K'), "the key follows the paste");
    got.pop();
    assert!(got.starts_with(PASTE_OPEN));
    assert!(got.ends_with(PASTE_CLOSE), "the paste is closed");
    let middle = &got[PASTE_OPEN.len()..got.len() - PASTE_CLOSE.len()];
    assert!(middle.iter().all(|b| *b == b'p'));
    assert!(middle.len() < body.len(), "the unsent middle was dropped");
    assert_eq!(receipt.accepted(), got.len());
    assert!(!receipt.is_direct(), "a cut frame is not a whole one");
    let end = meter.progress();
    assert_eq!(end.state, BulkState::Stopped);
    assert_eq!(end.sent, got.len() as u64);
}

#[test]
fn an_unstopped_metered_frame_is_the_plain_frame() {
    let (sink, mut reader) = sink_and_reader();
    let frame = bracketed(b"hello world");
    let meter = BulkMeter::new();
    let receipt = sink
        .write_frame_metered_with_receipt(&frame, &meter)
        .expect("write");
    assert!(receipt.is_direct());
    let mut got = vec![0u8; frame.len()];
    reader.read_exact(&mut got).expect("read");
    assert_eq!(got, frame);
    assert_eq!(
        meter.progress(),
        BulkProgress {
            sent: frame.len() as u64,
            total: frame.len() as u64,
            state: BulkState::Delivered,
        }
    );
}
