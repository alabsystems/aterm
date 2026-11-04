// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::age::Identity;
use crate::commons::binding::Request;
use crate::membership_log::{Entrant, Record, Store};

fn store() -> Store {
    let identity = Identity::from_secret_bytes([57; 32]);
    let request =
        Request::create([58; 16], [59; 32], [60; 16], [61; 16], &identity, &[62; 32]).unwrap();
    let genesis = Record::found(
        &[63; 32],
        vec![Entrant {
            request,
            node_key: [64; 32],
            name: "writer".into(),
            label: "m1".into(),
        }],
        1,
    )
    .unwrap();
    let mut store = Store::for_vault("writer").unwrap();
    store.insert(genesis).unwrap();
    store
}

#[derive(Default)]
struct Sink {
    bytes: Vec<u8>,
    calls: usize,
    fail: bool,
}

impl Write for Sink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.calls += 1;
        if self.fail {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "write failed"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn held_record_buffer_flushes_small_output_and_reports_final_failure() {
    let store = store();
    let expected = store.log().unwrap().genesis().bytes();
    assert!(expected.len() < 64 * 1024);
    let mut sink = Sink::default();
    write_held_buffered(&mut sink, &store, 0).unwrap();
    assert_eq!(sink.bytes, expected);
    assert_eq!(sink.calls, 1);
    let mut failed = Sink {
        fail: true,
        ..Sink::default()
    };
    let error = write_held_buffered(&mut failed, &store, 0).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    assert!(failed.bytes.is_empty());
}
