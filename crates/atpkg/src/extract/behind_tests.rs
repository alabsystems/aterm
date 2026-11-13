// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

#[test]
fn pieces_handed_in_any_order_land_at_their_offsets_once_flushed() {
    let dir = crate::testdir::dir("atpkg-behind-offsets-");
    let path = Arc::new(dir.join("big"));
    let body = pattern(5 * CHUNK + 123, 7);
    let file = Arc::new(File::create(&*path).unwrap());
    run(|behind| {
        let behind = behind.expect("writers start");
        // Last piece first: each lands at its own offset whatever the order.
        let pieces: Vec<(usize, &[u8])> = body.chunks(CHUNK).enumerate().collect();
        for (i, piece) in pieces.iter().rev() {
            let mut buf = behind.buffer();
            buf.extend_from_slice(piece);
            behind.hand(&file, &path, (i * CHUNK) as u64, buf).unwrap();
        }
        behind.flush().unwrap();
    });
    drop(file);
    assert_eq!(std::fs::read(&*path).unwrap(), body);
}

#[test]
fn a_failed_write_is_answered_at_the_next_hand_over_or_flush() {
    let dir = crate::testdir::dir("atpkg-behind-failed-");
    let path = Arc::new(dir.join("read-only"));
    std::fs::write(&*path, b"x").unwrap();
    // Opened for reading only: every positioned write to it fails.
    let file = Arc::new(File::open(&*path).unwrap());
    let (first, second) = run(|behind| {
        let behind = behind.expect("writers start");
        let mut buf = behind.buffer();
        buf.extend_from_slice(b"bytes");
        behind.hand(&file, &path, 0, buf).unwrap();
        let flushed = behind.flush().map_err(|(_, at)| at);
        // The failure is answered once; what follows it is judged on its own.
        (flushed, behind.flush().is_ok())
    });
    assert_eq!(
        first,
        Err(PathBuf::clone(&path)),
        "the flush names the file"
    );
    assert!(second, "a failure is reported once");
    assert_eq!(std::fs::read(&*path).unwrap(), b"x", "nothing was written");
}

#[test]
fn a_walk_that_returns_early_stops_the_writers() {
    // The scope joins every writer: a writer that did not stop would hang this test.
    let got = run(|behind| behind.is_some());
    assert!(got);
}

#[test]
fn hand_over_waits_while_the_writers_are_behind_by_the_bound() {
    let dir = crate::testdir::dir("atpkg-behind-bound-");
    let path = Arc::new(dir.join("bounded"));
    let file = Arc::new(File::create(&*path).unwrap());
    let body = pattern(IN_FLIGHT * 2 + CHUNK, 3);
    run(|behind| {
        let behind = behind.expect("writers start");
        for (i, piece) in body.chunks(CHUNK).enumerate() {
            let mut buf = behind.buffer();
            buf.extend_from_slice(piece);
            behind.hand(&file, &path, (i * CHUNK) as u64, buf).unwrap();
            assert!(
                behind.shared.lock().in_flight <= IN_FLIGHT,
                "never more than the bound in flight"
            );
        }
        behind.flush().unwrap();
    });
    drop(file);
    assert_eq!(std::fs::read(&*path).unwrap(), body);
}
