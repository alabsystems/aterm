// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::io::Read as _;
use std::os::fd::AsRawFd as _;
use std::sync::mpsc;
use std::time::Duration;

/// A wait for something that must happen: a hang detector, not a latency budget.
const HANG: Duration = Duration::from_secs(60);

fn set_nonblocking(fd: RawFd, on: bool) {
    // SAFETY: `fd` is a live descriptor this test owns.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        assert!(flags >= 0, "F_GETFL: {}", std::io::Error::last_os_error());
        let flags = if on {
            flags | libc::O_NONBLOCK
        } else {
            flags & !libc::O_NONBLOCK
        };
        assert_eq!(libc::fcntl(fd, libc::F_SETFL, flags), 0);
    }
}

/// Fill the pipe to capacity, then leave its write end blocking, as a terminal's is.
fn fill(fd: RawFd) {
    set_nonblocking(fd, true);
    let block = [b'x'; 4096];
    // Large blocks, then single bytes into whatever room the large ones left.
    for size in [block.len(), 1] {
        loop {
            // SAFETY: `block` is a valid buffer of at least `size` bytes; `fd` is live.
            let wrote = unsafe { libc::write(fd, block.as_ptr().cast(), size) };
            if wrote < 0 {
                assert_eq!(
                    std::io::Error::last_os_error().kind(),
                    std::io::ErrorKind::WouldBlock
                );
                break;
            }
        }
    }
    set_nonblocking(fd, false);
}

/// Read everything the pipe holds now, without waiting for more.
fn drain(reader: &mut std::io::PipeReader) -> Vec<u8> {
    set_nonblocking(reader.as_raw_fd(), true);
    let mut all = Vec::new();
    let mut buf = [0u8; 65536];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => all.extend_from_slice(&buf[..n]),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) => panic!("read: {error}"),
        }
    }
    all
}

/// Write `count` lines from another thread, so a writer that blocks fails the hang detector
/// instead of hanging the test.
fn lines_off_thread(mut writer: DropOnFull, count: usize) -> (Vec<Delivery>, DropOnFull) {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let delivered = (0..count)
            .map(|i| writer.line(&format!("retry {i}")))
            .collect::<Vec<_>>();
        let _ = tx.send((delivered, writer));
    });
    rx.recv_timeout(HANG)
        .expect("a progress writer blocked on a terminal nobody reads")
}

#[test]
fn a_full_terminal_nobody_reads_drops_and_counts_each_line_without_blocking() {
    let (_reader, pipe) = aterm_uds::pipe().unwrap();
    fill(pipe.as_raw_fd());
    let (delivered, writer) = lines_off_thread(DropOnFull::new(pipe.as_raw_fd()), 20);
    assert_eq!(delivered, vec![Delivery::Dropped; 20]);
    assert_eq!(writer.dropped(), 20);
}

#[test]
fn the_next_line_with_room_says_how_many_were_dropped() {
    let (mut reader, pipe) = aterm_uds::pipe().unwrap();
    fill(pipe.as_raw_fd());
    let (_, mut writer) = lines_off_thread(DropOnFull::new(pipe.as_raw_fd()), 3);
    let filler = drain(&mut reader);
    assert!(
        filler.iter().all(|&b| b == b'x'),
        "only the filler was written"
    );
    assert_eq!(writer.line("catalog read"), Delivery::Written);
    assert_eq!(writer.dropped(), 0);
    assert_eq!(
        String::from_utf8(drain(&mut reader)).unwrap(),
        "(3 progress lines dropped while nothing read this terminal)\ncatalog read\n"
    );
}

#[test]
fn settle_says_the_count_once_the_reader_returns() {
    let (mut reader, pipe) = aterm_uds::pipe().unwrap();
    fill(pipe.as_raw_fd());
    let (_, mut writer) = lines_off_thread(DropOnFull::new(pipe.as_raw_fd()), 1);
    drain(&mut reader);
    assert_eq!(writer.settle(), Delivery::Written);
    assert_eq!(writer.settle(), Delivery::Written);
    assert_eq!(
        String::from_utf8(drain(&mut reader)).unwrap(),
        "(1 progress line dropped while nothing read this terminal)\n"
    );
}

#[test]
fn settle_on_a_full_terminal_nobody_reads_returns_and_keeps_the_count() {
    let (mut reader, pipe) = aterm_uds::pipe().unwrap();
    fill(pipe.as_raw_fd());
    let (_, writer) = lines_off_thread(DropOnFull::new(pipe.as_raw_fd()), 2);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut writer = writer;
        let settled = writer.settle();
        let _ = tx.send((settled, writer));
    });
    let (settled, mut writer) = rx
        .recv_timeout(HANG)
        .expect("settle blocked on a terminal nobody reads");
    assert_eq!(settled, Delivery::Dropped);
    assert_eq!(writer.dropped(), 2);
    drain(&mut reader);
    assert_eq!(writer.line("catalog read"), Delivery::Written);
    assert_eq!(
        String::from_utf8(drain(&mut reader)).unwrap(),
        "(2 progress lines dropped while nothing read this terminal)\ncatalog read\n"
    );
}

#[test]
fn a_long_line_is_cut_to_one_chunk_at_a_character_boundary() {
    let (mut reader, pipe) = aterm_uds::pipe().unwrap();
    let mut writer = DropOnFull::new(pipe.as_raw_fd());
    let long = "\u{e9}".repeat(CHUNK);
    assert_eq!(writer.line(&long), Delivery::Written);
    let said = String::from_utf8(drain(&mut reader)).unwrap();
    assert!(said.len() <= CHUNK, "{} bytes", said.len());
    assert!(said.ends_with('\n'));
}
