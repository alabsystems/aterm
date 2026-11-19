// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `SCM_RIGHTS` descriptor passing against a real kernel.
#![cfg(any(
    target_vendor = "apple",
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    )
))]

use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Seek, SeekFrom, Write};
use std::mem::ManuallyDrop;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, RawFd};
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};

use aterm_uds::fdpass::{self, MAX_FDS};
use aterm_uds::{CtlListener, CtlStream};

unsafe extern "C" {
    fn fcntl(fd: RawFd, cmd: i32, ...) -> i32;
}
const F_GETFD: i32 = 1;
const F_SETFD: i32 = 2;
const FD_CLOEXEC: i32 = 1;

/// Descriptor numbers the probes below sweep. Received descriptors land at the
/// lowest free numbers, so a leak always lands well inside this range.
const PROBE_LIMIT: RawFd = 1024;

/// Serialize this file's tests: the descriptor-count probes are process-wide.
/// Poisoning is ignored, so one failing test does not cascade into the rest.
fn serial() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A fresh per-test directory (same shape as `roundtrip.rs`).
fn test_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("aterm-uds-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create test dir");
    dir
}

/// A regular file with 16 bytes in it, seeked to `offset`.
fn seeded_file(dir: &std::path::Path, name: &str, offset: u64) -> File {
    let mut f = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(dir.join(name))
        .expect("open");
    f.write_all(b"0123456789abcdef").expect("write");
    f.seek(SeekFrom::Start(offset)).expect("seek");
    f
}

/// The `fstat`-derived identity of an open file, which survives a descriptor
/// transfer, unlike the number.
fn identity(f: &File) -> (u64, u64) {
    let m = f.metadata().expect("fstat");
    (m.dev(), m.ino())
}

/// This process's open-descriptor count.
fn open_descriptor_count() -> usize {
    (0..PROBE_LIMIT)
        // SAFETY: `F_GETFD` only reads the descriptor flags; an unopened number answers -1/EBADF,
        // which is exactly what is being counted.
        .filter(|&fd| unsafe { fcntl(fd, F_GETFD) } != -1)
        .count()
}

/// How many of this process's descriptors name `(dev, ino)`: unlike the bare
/// count, only another copy of the passed file can perturb it.
fn descriptors_naming(want: (u64, u64)) -> usize {
    (0..PROBE_LIMIT)
        .filter(|&fd| {
            // SAFETY: the `File` is wrapped in `ManuallyDrop` and never dropped, so this borrows
            // the descriptor for one `fstat` and never closes one it does not own; an invalid
            // number fails `metadata()` (EBADF) and counts as no match.
            let f = ManuallyDrop::new(unsafe { File::from_raw_fd(fd) });
            f.metadata().is_ok_and(|m| (m.dev(), m.ino()) == want)
        })
        .count()
}

/// Descriptor flags of `fd`, or -1.
fn descriptor_flags(fd: RawFd) -> i32 {
    // SAFETY: `F_GETFD` only reads the descriptor flags of a descriptor this process owns.
    unsafe { fcntl(fd, F_GETFD) }
}

/// Three descriptors ride one message and each arrives as the same open file
/// description at a different number: `fstat` identity matches and the file
/// offset is shared (which a re-`open` of the path could not fake).
#[test]
fn three_descriptors_arrive_as_the_same_open_file_descriptions() {
    let _guard = serial();
    let dir = test_dir("fdpass-identity");
    let files: Vec<(File, u64)> = [3u64, 5, 7]
        .into_iter()
        .enumerate()
        .map(|(i, offset)| (seeded_file(&dir, &format!("m{i}"), offset), offset))
        .collect();

    let (a, b) = CtlStream::pair().expect("pair");
    let borrowed: Vec<BorrowedFd<'_>> = files.iter().map(|(f, _)| f.as_fd()).collect();
    assert_eq!(
        fdpass::send_with_fds(&a, b"PTY", &borrowed).expect("send"),
        3,
        "the whole header rode one sendmsg"
    );

    let mut buf = [0u8; 16];
    let got = fdpass::recv_with_fds(&b, &mut buf, 3).expect("receive");
    assert_eq!(
        &buf[..got.bytes],
        b"PTY",
        "payload rides with the descriptors"
    );
    assert_eq!(got.fds.len(), 3, "three descriptors on one message");

    for ((original, offset), received) in files.iter().zip(got.fds) {
        assert_ne!(
            received.as_raw_fd(),
            original.as_raw_fd(),
            "the kernel picks the receiver's numbers — this is why the \
             adoption proof cannot name one"
        );
        let mut received = File::from(received);
        assert_eq!(
            identity(&received),
            identity(original),
            "same file, proved by fstat rather than by a number"
        );
        assert_eq!(
            received.stream_position().expect("tell"),
            *offset,
            "same OPEN FILE DESCRIPTION: the offset is shared, which a fresh \
             open of the same path would not be"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// `FD_CLOEXEC` is set by the receiver: it is clear on arrival (Darwin has no
/// `MSG_CMSG_CLOEXEC`), and the sender's own flag is cleared first so the
/// result cannot be inherited.
#[test]
fn received_descriptors_are_close_on_exec_on_arrival() {
    let _guard = serial();
    let dir = test_dir("fdpass-cloexec");
    let f = seeded_file(&dir, "m", 0);
    // SAFETY: clearing the descriptor flags of a descriptor this process owns.
    assert_ne!(unsafe { fcntl(f.as_raw_fd(), F_SETFD, 0) }, -1, "clear");
    assert_eq!(descriptor_flags(f.as_raw_fd()), 0, "sender is NOT cloexec");

    let (a, b) = CtlStream::pair().expect("pair");
    fdpass::send_with_fds(&a, b"1", &[f.as_fd()]).expect("send");
    let mut buf = [0u8; 4];
    let got = fdpass::recv_with_fds(&b, &mut buf, 1).expect("receive");
    let received = got.fds.first().expect("one descriptor");
    assert_eq!(
        descriptor_flags(received.as_raw_fd()),
        FD_CLOEXEC,
        "a received descriptor must be close-on-exec before anything can exec"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A receiver that budgets for fewer descriptors than were sent still gets the
/// extras installed, so an over-count is refused and the refusal closes
/// everything: total descriptors and descriptors naming the passed file are
/// unchanged.
#[test]
fn an_over_budget_receive_is_refused_without_leaking_descriptors() {
    let _guard = serial();
    let dir = test_dir("fdpass-overbudget");
    let f = seeded_file(&dir, "m", 0);
    let want = identity(&f);

    let (a, b) = CtlStream::pair().expect("pair");
    let fd = f.as_fd();
    fdpass::send_with_fds(&a, b"1", &[fd, fd, fd]).expect("send three");

    let before_total = open_descriptor_count();
    let before_named = descriptors_naming(want);
    assert_eq!(before_named, 1, "only the sender's own open, so far");

    let mut buf = [0u8; 4];
    let err = fdpass::recv_with_fds(&b, &mut buf, 2).expect_err("three > two");
    assert_eq!(
        err.kind(),
        ErrorKind::InvalidData,
        "a peer over-send is peer misbehavior, not caller misuse: {err}"
    );
    assert_eq!(
        open_descriptor_count(),
        before_total,
        "the refusal closed every descriptor it received"
    );
    assert_eq!(
        descriptors_naming(want),
        before_named,
        "no copy of the passed file survived the refusal"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A message with no descriptors (an ordinary write) does not read as a
/// truncated control message, a hung-up peer reads as EOF, not an error or an
/// empty success that loops forever, and a send to it is `EPIPE`, not a
/// signal.
#[test]
fn a_plain_message_and_a_hangup_are_ordinary_receives() {
    let _guard = serial();
    let (a, b) = CtlStream::pair().expect("pair");

    fdpass::send_with_fds(&a, b"bare", &[]).expect("send with no descriptors");
    let mut buf = [0u8; 8];
    let got = fdpass::recv_with_fds(&b, &mut buf, 3).expect("receive");
    assert_eq!(&buf[..got.bytes], b"bare");
    assert!(got.fds.is_empty(), "no control message, no descriptors");

    drop(a);
    b.set_nonblocking(true).unwrap();
    let eof = aterm_uds::deadline::once_released(|| fdpass::recv_with_fds(&b, &mut buf, 3));
    assert_eq!(eof.bytes, 0, "a hung-up peer reads as EOF");
    assert!(eof.fds.is_empty());
    let gone = fdpass::send_with_fds(&b, b"x", &[]).expect_err("the peer is gone");
    assert_eq!(gone.kind(), ErrorKind::BrokenPipe);
}

/// The kernel-attested peer pid over a real bind/connect/accept, on both ends.
#[test]
fn peer_pid_is_the_kernel_attested_peer() {
    let _guard = serial();
    let dir = test_dir("fdpass-peerpid");
    let path = dir.join("aterm-9.sock");
    let listener = CtlListener::bind(&path).expect("bind");
    let client = CtlStream::connect(&path).expect("connect");
    let (accepted, _) = aterm_uds::accept(
        &listener,
        aterm_uds::AcceptWait::Forever,
        aterm_uds::AcceptedMode::Blocking,
    )
    .expect("accept");

    let us = std::process::id();
    assert_eq!(fdpass::peer_pid(&client).expect("client side"), us);
    assert_eq!(fdpass::peer_pid(&accepted).expect("server side"), us);
    assert_eq!(
        fdpass::peer_pid_of_fd(accepted.as_raw_fd()).expect("by descriptor"),
        us
    );

    drop(listener);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Caller misuse is refused before the kernel is asked: the peer sees nothing
/// afterwards. A zero-length `sendmsg` would transfer nothing on Linux while
/// consuming the descriptors. The cap itself is no misuse.
#[test]
fn misuse_is_refused_before_the_kernel_is_asked() {
    let _guard = serial();
    let dir = test_dir("fdpass-misuse");
    let f = seeded_file(&dir, "m", 0);
    let (a, b) = CtlStream::pair().expect("pair");

    let at_cap = [f.as_fd(); MAX_FDS];
    fdpass::send_with_fds(&a, b"1", &at_cap).expect("MAX_FDS descriptors");
    let mut buf = [0u8; 4];
    let got = fdpass::recv_with_fds(&b, &mut buf, MAX_FDS).expect("MAX_FDS descriptors");
    assert_eq!(got.fds.len(), MAX_FDS);
    drop(got);

    let empty_payload = fdpass::send_with_fds(&a, b"", &[f.as_fd()]).expect_err("empty payload");
    assert_eq!(empty_payload.kind(), ErrorKind::InvalidInput);

    let too_many = [f.as_fd(); MAX_FDS + 1];
    let over_cap = fdpass::send_with_fds(&a, b"1", &too_many).expect_err("over the cap");
    assert_eq!(over_cap.kind(), ErrorKind::InvalidInput);

    let no_room = fdpass::recv_with_fds(&b, &mut [], 1).expect_err("empty buffer");
    assert_eq!(no_room.kind(), ErrorKind::InvalidInput);
    let over_max = fdpass::recv_with_fds(&b, &mut buf, MAX_FDS + 1).expect_err("over the cap");
    assert_eq!(over_max.kind(), ErrorKind::InvalidInput);

    b.set_nonblocking(true).expect("non-blocking");
    let idle = fdpass::recv_with_fds(&b, &mut buf, 1).expect_err("nothing queued");
    assert_eq!(
        idle.kind(),
        ErrorKind::WouldBlock,
        "a refused send must not have reached the peer"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A received socket right retains its peer after the sender retires its originals.
/// Sender custody stays live until the descriptor and its generation are accepted,
/// as the handoff protocol requires.
#[test]
fn a_received_socket_right_keeps_its_peer_alive_after_the_sender_closes() {
    use std::io::Read as _;

    fn descriptor(socket: &CtlStream) -> (RawFd, i32, i32, std::io::Result<(u64, u64, u32)>) {
        let fd = socket.as_raw_fd();
        // SAFETY: both commands only read flags of the socket borrowed for this call.
        let (flags, status) = unsafe { (fcntl(fd, F_GETFD), fcntl(fd, libc::F_GETFL)) };
        // SAFETY: this live borrowed descriptor is used only for metadata; ManuallyDrop
        // prevents the temporary File from closing it or extending its kernel lifetime.
        let file = ManuallyDrop::new(unsafe { File::from_raw_fd(fd) });
        let identity = file.metadata().map(|m| (m.dev(), m.ino(), m.mode()));
        (fd, flags, status, identity)
    }

    fn failure_state(socket: &CtlStream) -> String {
        let mut peek = [0u8];
        // SAFETY: peek is writable for one byte; MSG_PEEK never consumes it and
        // MSG_DONTWAIT keeps failure diagnostics from waiting on the socket.
        let n = unsafe {
            libc::recv(
                socket.as_raw_fd(),
                peek.as_mut_ptr().cast(),
                peek.len(),
                libc::MSG_PEEK | libc::MSG_DONTWAIT,
            )
        };
        let peeked = if n < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok((n, peek))
        };
        let mut error: libc::c_int = 0;
        let mut len =
            u32::try_from(std::mem::size_of_val(&error)).expect("socket error size fits socklen_t");
        // SAFETY: error and len are correctly sized writable getsockopt outputs.
        // SO_ERROR clears pending error state, so this runs only after the decisive read failed.
        let rc = unsafe {
            libc::getsockopt(
                socket.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_ERROR,
                (&raw mut error).cast(),
                &raw mut len,
            )
        };
        let error = if rc < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok((error, len))
        };
        format!(
            "descriptor={:?} peek={peeked:?} SO_ERROR={error:?}",
            descriptor(socket)
        )
    }

    let _guard = serial();
    let (sender, receiver) = aterm_uds::kept_out(CtlStream::pair).expect("transport pair");
    let (capability, mut peer) = aterm_uds::kept_out(CtlStream::pair).expect("capability pair");
    for stream in [&sender, &receiver, &capability, &peer] {
        stream.set_nonblocking(true).expect("nonblocking socket");
    }
    let original = [
        ("sender", descriptor(&sender)),
        ("receiver", descriptor(&receiver)),
        ("capability", descriptor(&capability)),
        ("peer", descriptor(&peer)),
    ];

    assert_eq!(
        fdpass::send_with_fds(&sender, b"Q0123456789abcdef", &[capability.as_fd()])
            .expect("queue socket right"),
        17
    );
    let mut byte = [0];
    assert_eq!(
        peer.read(&mut byte)
            .expect_err("sender custody must keep the peer live before receive")
            .kind(),
        ErrorKind::WouldBlock,
        "the sender keeps custody until the receiver accepts the socket"
    );

    let mut transferred = aterm_uds::kept_out(|| {
        let mut marker = [0];
        let mut received = fdpass::recv_with_fds(&receiver, &mut marker, 1)?;
        assert_eq!(received.bytes, 1, "one queued payload byte");
        assert_eq!(&marker, b"Q");
        assert_eq!(received.fds.len(), 1, "one queued socket right");
        Ok::<_, std::io::Error>(CtlStream::from(received.fds.pop().expect("socket right")))
    })
    .expect("receive the queued socket right");
    transferred
        .set_nonblocking(true)
        .expect("nonblocking transferred socket");
    let installed = [
        ("receiver", descriptor(&receiver)),
        ("transferred", descriptor(&transferred)),
        ("peer", descriptor(&peer)),
    ];
    let mut generation = [0; 16];
    (&receiver)
        .read_exact(&mut generation)
        .expect("remaining payload after the descriptor marker");
    assert_eq!(&generation, b"0123456789abcdef");
    drop(capability);
    drop(sender);
    let sender_retired = [
        ("receiver", descriptor(&receiver)),
        ("peer", descriptor(&peer)),
    ];
    drop(receiver);
    let transport_retired = [
        ("transferred", descriptor(&transferred)),
        ("peer", descriptor(&peer)),
    ];

    assert_eq!(
        transferred
            .write(b"C")
            .expect("write through received right"),
        1
    );
    assert_eq!(peer.read(&mut byte).expect("read from received right"), 1);
    assert_eq!(&byte, b"C");
    assert_eq!(peer.write(b"P").expect("write to received right"), 1);
    let reverse = transferred.read(&mut byte);
    if !matches!(&reverse, Ok(1)) {
        eprintln!(
            "received socket reverse read={reverse:?} byte={byte:?}\n\
             descriptor tuple=(raw_fd, descriptor_flags, status_flags, fstat(dev,ino,mode))\n\
             original={original:?}\nsender_retired={sender_retired:?}\n\
             installed={installed:?}\ntransport_retired={transport_retired:?}\n\
             transferred_now={}\npeer_now={}",
            failure_state(&transferred),
            failure_state(&peer),
        );
    }
    assert_eq!(reverse.expect("read through received right"), 1);
    assert_eq!(&byte, b"P");

    drop(transferred);
    assert_eq!(
        aterm_uds::deadline::once_released(|| peer.read(&mut byte)),
        0,
        "closing the received right releases its peer"
    );
}

/// A separately executed receiver adopts a fresh socket after proving its own
/// startup, then exchanges the native monitor's request and response sizes.
#[test]
fn a_self_executed_receiver_accepts_socket_ownership_before_sender_retirement() {
    use aterm_spec::wait::until_while_working;
    use std::io::Read as _;

    const ROLE: &str = "ATERM_UDS_FDPASS_RECEIVER_PARENT";
    const TEST: &str = "a_self_executed_receiver_accepts_socket_ownership_before_sender_retirement";
    const TRANSFER: &[u8; 17] = b"M0123456789abcdef";

    // The parent owns the helper's progress guard and cleanup. Readiness puts
    // the helper to sleep, so its polling CPU cannot hide a stalled exchange.
    fn read_ready(stream: &mut CtlStream, mut bytes: &mut [u8]) {
        while !bytes.is_empty() {
            match stream.read(bytes) {
                Ok(0) => panic!("socket reached EOF before all expected bytes arrived"),
                Ok(n) => bytes = &mut bytes[n..],
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    aterm_uds::ready::any(&[stream.as_fd()], None).expect("socket readable");
                }
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) => panic!("socket read failed: {error}"),
            }
        }
    }

    fn read_from_child(pid: u32, stream: &mut CtlStream, bytes: &mut [u8], what: &str) {
        let mut used = 0;
        until_while_working(pid, what, || match stream.read(&mut bytes[used..]) {
            Ok(0) => panic!("child socket reached EOF before {what}"),
            Ok(n) => {
                used += n;
                (used == bytes.len()).then_some(())
            }
            Err(error)
                if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) =>
            {
                None
            }
            Err(error) => panic!("child socket read failed while waiting for {what}: {error}"),
        });
    }

    let request: [u8; 48] = std::array::from_fn(|i| u8::try_from(i).unwrap());
    let response: [u8; 442] = std::array::from_fn(|i| u8::try_from(i % 251).unwrap());

    if let Some(parent) = std::env::var_os(ROLE) {
        let parent: u32 = parent.to_str().expect("parent PID text").parse().unwrap();
        assert_eq!(
            aterm_uds::process::read_process_parent(i32::try_from(std::process::id()).unwrap()),
            Some(parent),
            "the receiver was executed by this test's owning parent"
        );
        let mut bootstrap = CtlStream::from(
            aterm_uds::kept_out(|| std::io::stdin().as_fd().try_clone_to_owned())
                .expect("kept-out bootstrap clone"),
        );
        bootstrap.set_nonblocking(true).unwrap();
        assert_eq!(bootstrap.write(b"B").expect("prove helper execution"), 1);

        let mut capability = loop {
            aterm_uds::ready::any(&[bootstrap.as_fd()], None).expect("delegation readable");
            let received = aterm_uds::kept_out(|| {
                let mut marker = [0];
                let mut got = fdpass::recv_with_fds(&bootstrap, &mut marker, 1)?;
                assert_eq!(got.bytes, 1, "the marker is received separately");
                assert_eq!(marker, [TRANSFER[0]]);
                assert_eq!(got.fds.len(), 1, "one fresh socket capability");
                Ok::<_, std::io::Error>(CtlStream::from(got.fds.pop().unwrap()))
            });
            match received {
                Ok(stream) => break stream,
                Err(error)
                    if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => {}
                Err(error) => panic!("receive delegated socket: {error}"),
            }
        };
        let mut generation = [0; 16];
        read_ready(&mut bootstrap, &mut generation);
        assert_eq!(&generation, &TRANSFER[1..]);
        assert_eq!(
            bootstrap.write(b"A").expect("accept the actual descriptor"),
            1
        );
        let mut retired = [0];
        read_ready(&mut bootstrap, &mut retired);
        assert_eq!(&retired, b"D", "the sender retired its capability copy");
        loop {
            aterm_uds::ready::any(&[bootstrap.as_fd()], None).expect("bootstrap close readable");
            match bootstrap.read(&mut retired) {
                Ok(0) => break,
                Ok(_) => panic!("bootstrap had bytes beyond retirement confirmation"),
                Err(error)
                    if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => {}
                Err(error) => panic!("bootstrap retirement: {error}"),
            }
        }
        drop(bootstrap);
        // The original bootstrap at stdin 0 intentionally remains open.
        capability.set_nonblocking(true).unwrap();
        let mut empty = [0];
        assert_eq!(
            capability
                .read(&mut empty)
                .expect_err("the parent waits for the request before writing")
                .kind(),
            ErrorKind::WouldBlock,
            "an empty live transferred socket must not report EOF"
        );
        assert_eq!(
            capability.write(&request).expect("write 48-byte request"),
            48
        );
        let mut header = [0; 28];
        let mut body = [0; 414];
        read_ready(&mut capability, &mut header);
        read_ready(&mut capability, &mut body);
        assert_eq!(&header, &response[..28]);
        assert_eq!(&body, &response[28..]);
        drop(capability);
        return;
    }

    let _serial = serial();
    struct Owned(aterm_uds::spawnfd::Child);
    impl Drop for Owned {
        fn drop(&mut self) {
            aterm_uds::deadline::settle_child(&mut self.0);
        }
    }

    let exe = std::env::current_exe().expect("this Cargo-named test binary");
    let (mut sender, receiver) = aterm_uds::kept_out(CtlStream::pair).expect("bootstrap pair");
    sender.set_nonblocking(true).unwrap();
    // This temporary command drops its copy of the child's stdin immediately
    // after spawn; the parent retains only its own bootstrap endpoint.
    let mut child = Owned(
        aterm_uds::spawnfd::command(&exe)
            .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
            .env(ROLE, std::process::id().to_string())
            .stdin(std::os::fd::OwnedFd::from(receiver))
            .process_group(0)
            .spawn()
            .expect("execute socket receiver"),
    );
    let pid = child.0.id();
    let mut started = [0];
    read_from_child(pid, &mut sender, &mut started, "helper execution byte");
    assert_eq!(&started, b"B");

    let (capability, mut peer) =
        aterm_uds::kept_out(CtlStream::pair).expect("fresh capability pair");
    peer.set_nonblocking(true).unwrap();
    assert_eq!(
        fdpass::send_with_fds(&sender, TRANSFER, &[capability.as_fd()])
            .expect("delegate fresh socket after helper execution"),
        17
    );
    let mut accepted = [0];
    read_from_child(
        pid,
        &mut sender,
        &mut accepted,
        "receiver accepted the actual descriptor",
    );
    assert_eq!(&accepted, b"A");
    drop(capability);
    assert_eq!(
        sender.write(b"D").expect("confirm capability retirement"),
        1
    );
    drop(sender);

    let mut actual_request = [0; 48];
    read_from_child(pid, &mut peer, &mut actual_request, "48-byte request");
    assert_eq!(actual_request, request);
    assert_eq!(peer.write(&response).expect("write 442-byte response"), 442);
    let mut byte = [0];
    until_while_working(
        pid,
        "receiver consumed its response and closed its socket",
        || match peer.read(&mut byte) {
            Ok(0) => Some(()),
            Ok(_) => panic!("the receiver sent bytes beyond its exact 48-byte request"),
            Err(error)
                if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) =>
            {
                None
            }
            Err(error) => panic!("waiting for receiver socket close: {error}"),
        },
    );
    let status = until_while_working(pid, "receiver owning exit", || {
        child.0.try_wait().expect("wait for this test's receiver")
    });
    assert!(status.success(), "receiver exited {status:?}");
}
