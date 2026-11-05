// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The real wire client must retain successes around refused pipeline entries.

#[test]
fn quota_retries_preserve_sequences_and_drain_failure_windows() {
    use super::*;
    use astream_broker::proto::{
        ERR_RATE_LIMITED, ERR_RECORD_REFUSED, RATE_LIMITED_PREFIX, Request, Response,
        decode_request, encode_response, read_frame, write_frame,
    };
    use std::io::Cursor;
    use std::sync::{Arc, Mutex};

    struct Wire {
        replies: Cursor<Vec<u8>>,
        sent: Arc<Mutex<Vec<u8>>>,
    }

    impl Read for Wire {
        fn read(&mut self, into: &mut [u8]) -> io::Result<usize> {
            self.replies.read(into)
        }
    }

    impl Write for Wire {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.sent.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn ack(offset: u64) -> Response {
        Response::PublishAck {
            offset,
            deduped: false,
        }
    }

    fn limited() -> Response {
        Response::Error {
            code: ERR_RATE_LIMITED,
            msg: format!("{RATE_LIMITED_PREFIX}1 (record rate exceeded)"),
        }
    }

    fn wire(
        replies: impl IntoIterator<Item = Response>,
    ) -> (astream_broker::Client<Wire>, Arc<Mutex<Vec<u8>>>) {
        let mut bytes = Vec::new();
        for reply in replies {
            write_frame(&mut bytes, &encode_response(&reply)).unwrap();
        }
        let sent = Arc::new(Mutex::new(Vec::new()));
        (
            astream_broker::Client::from_stream(Wire {
                replies: Cursor::new(bytes),
                sent: Arc::clone(&sent),
            }),
            sent,
        )
    }

    fn sequences(sent: &Mutex<Vec<u8>>) -> Vec<u64> {
        let bytes = sent.lock().unwrap();
        let mut input = Cursor::new(bytes.as_slice());
        let mut seqs = Vec::new();
        while (input.position() as usize) < bytes.len() {
            let frame = read_frame(&mut input).unwrap().expect("a complete frame");
            let Request::Publish { producer_seq, .. } = decode_request(&frame).unwrap() else {
                panic!("expected a publish");
            };
            seqs.push(producer_seq);
        }
        seqs
    }

    let n = FILL_WINDOW + 3;
    let mut replies: Vec<_> = (1..=FILL_WINDOW as u64)
        .map(|seq| {
            if matches!(seq, 2 | 4) {
                limited()
            } else {
                ack(seq)
            }
        })
        .collect();
    replies.extend([ack(2), ack(4)]);
    replies.extend((FILL_WINDOW as u64 + 1..=n as u64).map(ack));
    replies.push(ack(9_999));
    let (mut client, sent) = wire(replies);
    fill_records(&mut client, FILL_PRODUCER, "/bench/fill", n).unwrap();
    assert_eq!(client.owed_acks(), 0);
    assert_eq!(
        client.publish(99, 7, "/bench/after", b"after").unwrap(),
        (9_999, false)
    );
    let mut expected: Vec<_> = (1..=FILL_WINDOW as u64).collect();
    expected.extend([2, 4]);
    expected.extend(FILL_WINDOW as u64 + 1..=n as u64);
    expected.push(7);
    assert_eq!(
        sequences(&sent),
        expected,
        "successful sequences are never replayed"
    );
    for bad in [
        Response::Error {
            code: ERR_RECORD_REFUSED,
            msg: "record refused".into(),
        },
        Response::PublishAck {
            offset: 1,
            deduped: true,
        },
    ] {
        let (mut client, sent) = wire([limited(), bad, ack(3), ack(9_999)]);
        fill_records(&mut client, FILL_PRODUCER, "/bench/fill", 3)
            .expect_err("do not hide a failed fill");
        assert_eq!(
            client.owed_acks(),
            0,
            "the rate refusal does not strand later replies"
        );
        assert_eq!(
            client.publish(99, 7, "/bench/after", b"after").unwrap(),
            (9_999, false)
        );
        assert_eq!(sequences(&sent), [1, 2, 3, 7]);
    }
}

#[test]
fn a_real_rate_limited_broker_fills_more_than_one_window_exactly_once() {
    use super::*;
    use astream_broker::{Broker, Durability, OpenOptions, Prepared, RateLimit};

    let scratch = crate::harness::sock_dir("backfill");
    let opts = OpenOptions::new(Durability::Relaxed)
        .with_rate_limit(RateLimit::records(10_000, 4).unwrap());
    let broker =
        Broker::open_opts(scratch.path().join("b.log"), opts, || Ok(Prepared::Nothing)).unwrap();
    let sock = scratch.path().join("b.sock");
    let _server = broker.serve(&sock).unwrap();
    let mut client = crate::harness::broker_client(&sock);
    let n = FILL_WINDOW + 3;
    fill_records(&mut client, FILL_PRODUCER, "/bench/fill", n).unwrap();
    let mut from = 0;
    let mut rows = Vec::new();
    loop {
        let (page, (next, head)) = client.fetch(from, "/bench/fill", 1024).unwrap();
        assert!(next > from || next >= head, "a page must advance");
        rows.extend(page);
        if next >= head {
            break;
        }
        from = next;
    }
    assert_eq!(rows.len(), n);
    assert!(rows.iter().all(|(_, _, body)| body == &FILL_BODY));
    assert!(rows.windows(2).all(|rows| rows[0].0 < rows[1].0));
    assert_eq!(client.owed_acks(), 0);
}
