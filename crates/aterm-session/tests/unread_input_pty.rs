// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `SinkWriter::unread_input`, the one reader of a program's unread keys, against a real pty on
//! each platform that counts them.
#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use aterm_session::sink::{Seam, SinkWriter};
use aterm_spec::wait::{MUST_HAPPEN, within};

/// Sets `slave`'s line discipline raw, or canonical when `canonical`.
fn slave_mode(slave: i32, canonical: bool) {
    // SAFETY: `termios` is plain integers and bytes; all-zero is valid until `tcgetattr` fills it.
    let mut t: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: `slave` is the test's live slave and `t` a writable `termios`.
    assert_eq!(unsafe { libc::tcgetattr(slave, &raw mut t) }, 0);
    if canonical {
        t.c_lflag |= libc::ICANON;
    } else {
        // SAFETY: `t` is a whole `termios`.
        unsafe { libc::cfmakeraw(&raw mut t) };
    }
    // SAFETY: as above; `t` is the slave's own mode, changed.
    assert_eq!(
        unsafe { libc::tcsetattr(slave, libc::TCSANOW, &raw const t) },
        0
    );
}

/// The program at `slave` reads one byte.
fn read_byte(slave: i32) -> u8 {
    let mut b = [0_u8; 1];
    // SAFETY: a one-byte read into `b`, which lives for the call.
    let r = unsafe { libc::read(slave, b.as_mut_ptr().cast(), 1) };
    assert_eq!(r, 1, "the program reads a byte");
    b[0]
}

/// The program's unread input is read through the master on macOS and through a descriptor of
/// the slave on Linux, never the master's own count there (the program's output waiting for
/// aterm): keys written count until the program reads them, output it writes never does, and the
/// backlog's queue is that same count.
#[test]
fn unread_input_is_the_keys_the_program_has_not_read_and_the_backlog_reads_it() {
    let (master, slave) = aterm_pty::open_pty_pair().expect("a pty");
    slave_mode(slave, false);
    let sink = SinkWriter::new(master);
    // Linux hands a write to the slave from a worker: a hang detector, never a budget.
    let reads = |n: usize| {
        within(MUST_HAPPEN, Duration::from_millis(1), || {
            (sink.unread_input() == Some(n)).then_some(())
        })
        .is_some()
    };
    assert_eq!(sink.unread_input(), Some(0), "nothing typed");
    // SAFETY: `slave` is live and the five bytes live for the call.
    let wrote = unsafe { libc::write(slave, b"hello".as_ptr().cast(), 5) };
    assert_eq!(wrote, 5, "the program's output");
    assert_eq!(sink.unread_input(), Some(0), "output is not input");
    assert_eq!(sink.write_frame(b"abc").expect("keys"), 3);
    assert!(
        reads(3),
        "the keys the program has not read: {:?}",
        sink.unread_input()
    );
    assert_eq!(
        sink.input_backlog().map(|b| b.queued),
        Some(3),
        "the backlog's queue is that count"
    );
    let mut got = [0_u8; 3];
    let mut n = 0;
    while n < got.len() {
        // SAFETY: a bounded read into the unfilled tail of `got`.
        let r = unsafe { libc::read(slave, got[n..].as_mut_ptr().cast(), 1) };
        assert_eq!(r, 1, "the program reads a key");
        n += 1;
        assert!(
            reads(got.len() - n),
            "the program read {n}: {:?}",
            sink.unread_input()
        );
    }
    assert_eq!(&got, b"abc", "the keys in order");
    drop(sink);
    // SAFETY: both fds are this test's own, closed once, no sink left over them.
    unsafe {
        libc::close(slave);
        libc::close(master);
    }
}

/// A look reads every driven byte read only once none is on its way, spilled or queued: a key
/// held before its fd lock is unread though the queue is empty, a queued one until the program
/// reads it, and a reply moves no mark but is unread while queued. A canonical slave, whose queue
/// does not count a partial line, is never read, and another sink's mark differs; a sink with no
/// tty can never tell ([`aterm_session::sink::InputLook::known`]).
#[test]
fn an_input_look_is_read_once_its_program_has_read_every_driven_byte() {
    let (master, slave) = aterm_pty::open_pty_pair().expect("a pty");
    slave_mode(slave, false);
    let sink = Arc::new(SinkWriter::new(master));
    let (at_lock, reached) = mpsc::channel();
    let (go, gate) = mpsc::channel::<()>();
    let gate = Mutex::new(gate);
    let armed = AtomicBool::new(true);
    sink.seam_for_testing(move |seam| {
        if seam == Seam::FrameLock && armed.swap(false, Ordering::SeqCst) {
            let _ = at_lock.send(());
            let _ = gate.lock().map(|gate| gate.recv());
        }
    });
    let looks = |read: bool| {
        within(MUST_HAPPEN, Duration::from_millis(1), || {
            let look = sink.input_look();
            (look.read == read).then_some(look)
        })
        .expect("the look comes to read so")
    };
    let quiet = sink.input_look();
    assert!(quiet.read, "nothing driven");
    assert!(quiet.known, "a pty's look can tell");
    assert_eq!(sink.input_look(), quiet, "a look moves nothing");

    let key = {
        let sink = Arc::clone(&sink);
        std::thread::spawn(move || sink.write_frame(b"k").expect("a key"))
    };
    reached
        .recv_timeout(MUST_HAPPEN)
        .expect("the key reached its fd lock");
    let on_its_way = sink.input_look();
    assert_eq!(sink.unread_input(), Some(0), "nothing queued yet");
    assert!(!on_its_way.read, "a key not yet placed is unread");
    assert_ne!(
        on_its_way.mark, quiet.mark,
        "its reservation moves the mark"
    );
    go.send(()).expect("the key goes on");
    assert_eq!(key.join().expect("the key's writer"), 1);
    let queued = looks(false);
    assert_eq!(queued.mark, on_its_way.mark, "placing it moves no mark");
    assert_eq!(read_byte(slave), b'k');
    let read = looks(true);
    assert_eq!(read.mark, on_its_way.mark);

    assert_eq!(sink.write_reply(b"R").expect("a reply"), 1);
    within(MUST_HAPPEN, Duration::from_millis(1), || {
        (sink.unread_input() == Some(1)).then_some(())
    })
    .expect("the reply queued");
    let replied = sink.input_look();
    assert!(!replied.read, "a queued reply is unread");
    assert_eq!(replied.mark, read.mark, "a reply moves no mark");
    assert_eq!(read_byte(slave), b'R');
    assert_eq!(looks(true).mark, read.mark);

    slave_mode(slave, true);
    let canonical = sink.input_look();
    assert!(!canonical.read, "a canonical queue hides a partial line");
    assert_eq!(canonical.mark, read.mark);
    slave_mode(slave, false);
    assert!(sink.input_look().read, "control: raw again, nothing queued");

    let other_sink = SinkWriter::new(-1);
    let _ = other_sink.write_frame(b"k");
    let other = other_sink.input_look();
    assert_ne!(
        other.mark, read.mark,
        "another sink's mark, after as much driven into it"
    );
    assert!(
        !other.read && !other.known,
        "a sink with no tty is never read and cannot tell"
    );
    drop(sink);
    // SAFETY: both fds are this test's own, closed once, no sink left over them.
    unsafe {
        libc::close(slave);
        libc::close(master);
    }
}

/// A key driven and read while a look is under way leaves that look unread: the look's counts
/// and queue said nothing was waiting, but its mark cannot name the key, so a look that called
/// itself read would hide input its program read since the look before it. The key is written
/// and read at the seam after the look read the slave's queue and mode. Control: the next look
/// is read, under the key's mark.
#[test]
fn a_key_driven_and_read_during_a_look_leaves_it_unread() {
    let (master, slave) = aterm_pty::open_pty_pair().expect("a pty");
    slave_mode(slave, false);
    let sink = Arc::new(SinkWriter::new(master));
    let (at_look, reached) = mpsc::channel();
    let (done, finished) = mpsc::channel::<()>();
    let finished = Mutex::new(finished);
    let armed = Arc::new(AtomicBool::new(false));
    {
        let armed = Arc::clone(&armed);
        sink.seam_for_testing(move |seam| {
            if seam == Seam::InputLook && armed.swap(false, Ordering::SeqCst) {
                let _ = at_look.send(());
                let _ = finished.lock().map(|f| f.recv_timeout(MUST_HAPPEN));
            }
        });
    }
    let before = sink.input_look();
    assert!(before.read, "nothing driven");
    let program = {
        let sink = Arc::clone(&sink);
        std::thread::spawn(move || {
            reached
                .recv_timeout(MUST_HAPPEN)
                .expect("the look reached its seam");
            assert_eq!(sink.write_frame(b"k").expect("a key"), 1);
            assert_eq!(read_byte(slave), b'k');
            let _ = done.send(());
        })
    };
    armed.store(true, Ordering::SeqCst);
    let during = sink.input_look();
    program.join().expect("the key's writer and reader");
    assert_eq!(sink.unread_input(), Some(0), "the program read the key");
    assert!(
        !during.read,
        "a look the key was driven and read under is unread"
    );
    let after = sink.input_look();
    assert!(after.read, "control: the next look is read");
    assert_ne!(after.mark, before.mark, "under the key's mark");
    assert_eq!(after.mark, during.mark, "the look under way names the key");
    drop(sink);
    // SAFETY: both fds are this test's own, closed once, no sink left over them.
    unsafe {
        libc::close(slave);
        libc::close(master);
    }
}

/// Bytes the sink still holds in its spill keep the look unread though the slave's queue is
/// empty: the program read every byte the kernel took while the drainer, parked waiting for the
/// kernel's room with the spill's lock let go, holds the rest. The look reads the spill's length
/// (the lock is free), not a lock it could not take. Control: once the drainer writes them and
/// the program reads them, the look is read.
#[test]
fn a_byte_left_in_the_spill_keeps_the_look_unread() {
    const FRAME: usize = 4000;
    let (master, slave) = aterm_pty::open_pty_pair().expect("a pty");
    slave_mode(slave, false);
    // SAFETY: `master` is this test's live fd; F_GETFL/F_SETFL take ints.
    let flags = unsafe { libc::fcntl(master, libc::F_GETFL) };
    assert!(flags >= 0);
    // SAFETY: as above.
    assert_eq!(
        unsafe { libc::fcntl(master, libc::F_SETFL, flags | libc::O_NONBLOCK) },
        0
    );
    let sink = SinkWriter::new(master);
    sink.note_master_nonblocking(true);
    let (parked, at_write) = mpsc::channel();
    let (go, gate) = mpsc::channel::<()>();
    let gate = Mutex::new(gate);
    let armed = Arc::new(AtomicBool::new(false));
    {
        let armed = Arc::clone(&armed);
        sink.seam_for_testing(move |seam| {
            if seam == Seam::DrainPark && armed.swap(false, Ordering::SeqCst) {
                let _ = parked.send(());
                let _ = gate.lock().map(|gate| gate.recv());
            }
        });
    }
    armed.store(true, Ordering::SeqCst);
    assert_eq!(
        sink.write_frame_nonparking(&[b'p'; FRAME])
            .expect("a paste"),
        FRAME
    );
    at_write
        .recv_timeout(MUST_HAPPEN)
        .expect("the drainer parked for the kernel's room");
    let mut read = 0;
    while let Some(n) = sink.unread_input().filter(|n| *n > 0) {
        for _ in 0..n {
            assert_eq!(read_byte(slave), b'p');
        }
        read += n;
    }
    assert!(
        read < FRAME,
        "PRECONDITION: the kernel took part of the paste"
    );
    assert_eq!(
        sink.unread_input(),
        Some(0),
        "PRECONDITION: the queue is empty"
    );
    assert_eq!(
        sink.try_egress_drained_to_kernel(),
        Some(false),
        "PRECONDITION: the spill holds bytes and its lock is free"
    );
    let look = sink.input_look();
    assert!(look.known, "PRECONDITION: a look that can tell");
    assert!(!look.read, "bytes left in the spill keep the look unread");
    go.send(()).expect("the drainer goes on");
    while read < FRAME {
        assert_eq!(read_byte(slave), b'p');
        read += 1;
    }
    assert!(sink.wait_egress_drained_to_kernel());
    within(MUST_HAPPEN, Duration::from_millis(1), || {
        sink.input_look().read.then_some(())
    })
    .expect("NEGATIVE CONTROL: every byte read, the look is read");
    drop(sink);
    // SAFETY: both fds are this test's own, closed once, no sink left over them.
    unsafe {
        libc::close(slave);
        libc::close(master);
    }
}
