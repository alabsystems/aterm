// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    CMSG_DATA_OFFSET, CMSG_HDR_LEN, CONTROL_LEN, FD_LEN, abi, checked_pid, cmsg_align, cmsg_len,
    cmsg_space, scan_control, write_i32,
};

/// One `SCM_RIGHTS` header whose `cmsg_len` claims `claimed` descriptors, then
/// `present` descriptor numbers, cut to `keep` bytes (what `msg_controllen` would
/// say): the shape a truncated receive leaves.
fn control(claimed: usize, present: &[i32], keep: usize) -> Vec<u8> {
    let mut buf = vec![0u8; CONTROL_LEN];
    abi::write_cmsg_len(&mut buf, cmsg_len(claimed * FD_LEN));
    write_i32(&mut buf, abi::LEVEL_OFFSET, abi::SOL_SOCKET);
    write_i32(&mut buf, abi::TYPE_OFFSET, abi::SCM_RIGHTS);
    for (i, fd) in present.iter().enumerate() {
        write_i32(&mut buf, CMSG_DATA_OFFSET + i * FD_LEN, *fd);
    }
    buf.truncate(keep);
    buf
}

/// 12 on Darwin (4-byte alignment), 16 on Linux.
#[test]
fn the_data_offset_matches_the_platform_header() {
    let expected = if cfg!(target_vendor = "apple") {
        12
    } else {
        16
    };
    assert_eq!(CMSG_DATA_OFFSET, expected, "CMSG_DATA(cmsg) - cmsg");
    assert_eq!(cmsg_len(4), expected + 4, "CMSG_LEN(4)");
}

/// Every message is walked, each header at the aligned end of the one before (here a
/// foreign one whose one data byte leaves padding), and only `SCM_RIGHTS` yields.
#[test]
fn a_complete_message_yields_every_descriptor_in_order() {
    let buf = control(3, &[7, 8, 9], cmsg_space(3 * FD_LEN));
    let scan = scan_control(&buf);
    assert_eq!(scan.fds, [7, 8, 9]);
    assert!(!scan.truncated, "a complete message is not truncated");

    let next = cmsg_align(cmsg_len(1));
    let mut two = vec![0u8; next + cmsg_space(2 * FD_LEN)];
    abi::write_cmsg_len(&mut two, cmsg_len(1));
    write_i32(&mut two, abi::LEVEL_OFFSET, abi::SOL_SOCKET);
    write_i32(&mut two, abi::TYPE_OFFSET, abi::SCM_RIGHTS + 1);
    two[CMSG_DATA_OFFSET] = 0xff;
    two[next..].copy_from_slice(&control(2, &[7, 8], cmsg_space(2 * FD_LEN)));
    let scan = scan_control(&two);
    assert_eq!(scan.fds, [7, 8], "the message after a foreign one");
    assert!(!scan.truncated);
}

/// What `getsockopt` answered for the peer pid is taken only whole and only as a pid.
#[test]
fn a_peer_pid_is_a_whole_answer_naming_a_process() {
    assert_eq!(checked_pid(7, 4, 4).ok(), Some(7));
    for (pid, got_len) in [(7, 3), (0, 4), (-1, 4)] {
        let refused = checked_pid(pid, got_len, 4).err().map(|e| e.kind());
        assert_eq!(
            refused,
            Some(std::io::ErrorKind::InvalidData),
            "{pid} {got_len}"
        );
    }
}

/// `cmsg_len` claims three descriptors, the buffer holds one, or only the header.
#[test]
fn a_cmsg_len_past_the_buffer_is_clamped_and_flagged() {
    let buf = control(3, &[7], CMSG_DATA_OFFSET + FD_LEN);
    let scan = scan_control(&buf);
    assert_eq!(scan.fds, [7], "only the descriptor really present");
    assert!(scan.truncated, "an overrunning cmsg_len is truncation");

    let scan = scan_control(&control(1, &[7], CMSG_HDR_LEN));
    assert!(scan.fds.is_empty());
    assert!(scan.truncated, "a header whose descriptors were cut off");
}

/// The whole descriptors are reported (ours to close) and the half-copied one makes
/// the message fatal.
#[test]
fn a_partial_descriptor_at_the_tail_is_truncation() {
    let buf = control(3, &[7, 8], CMSG_DATA_OFFSET + FD_LEN + 2);
    let scan = scan_control(&buf);
    assert_eq!(scan.fds, [7]);
    assert!(scan.truncated);

    let mut buf = control(2, &[7, 8], cmsg_space(2 * FD_LEN));
    abi::write_cmsg_len(&mut buf, cmsg_len(FD_LEN + 2));
    let scan = scan_control(&buf);
    assert_eq!(scan.fds, [7]);
    assert!(scan.truncated, "half a descriptor inside the buffer");
}

/// Neither a non-`SCM_RIGHTS` message (another type, or `SCM_RIGHTS`'s number at
/// another level) nor a header shorter than its own data area yields a descriptor;
/// the runt is refused, and a header with no data at all is no runt.
#[test]
fn foreign_and_malformed_headers_yield_no_descriptors() {
    for (level, kind) in [
        (abi::SOL_SOCKET, abi::SCM_RIGHTS + 1),
        (abi::SOL_SOCKET + 1, abi::SCM_RIGHTS),
    ] {
        let mut foreign = control(2, &[7, 8], cmsg_space(2 * FD_LEN));
        write_i32(&mut foreign, abi::LEVEL_OFFSET, level);
        write_i32(&mut foreign, abi::TYPE_OFFSET, kind);
        let scan = scan_control(&foreign);
        assert!(scan.fds.is_empty(), "not SCM_RIGHTS: no descriptors");
        assert!(!scan.truncated);
    }

    let scan = scan_control(&control(0, &[], cmsg_space(0)));
    assert!(scan.fds.is_empty());
    assert!(!scan.truncated, "a header exactly its own length is whole");

    let mut runt = control(2, &[7, 8], cmsg_space(2 * FD_LEN));
    abi::write_cmsg_len(&mut runt, CMSG_DATA_OFFSET - 1);
    let scan = scan_control(&runt);
    assert!(scan.fds.is_empty(), "malformed: no descriptors");
    assert!(scan.truncated, "malformed is refused, not ignored");
}

/// A peer that sends past `MAX_FDS` (`send_with_fds` refuses to; a raw `sendmsg` does
/// not), within the complete kernel-message storage, is still a visible
/// over-count: every descriptor it sent is named, closed and refused, and none stays
/// here.
#[test]
fn an_over_send_past_the_cap_is_named_closed_and_refused() {
    use std::os::fd::{AsRawFd as _, FromRawFd as _};
    use std::os::unix::fs::MetadataExt as _;
    const SENT: usize = super::MAX_FDS + 1;
    #[repr(C, align(8))]
    struct SenderControl([u8; cmsg_space(SENT * FD_LEN)]);
    let scratch = aterm_tempfile::Builder::new()
        .prefix("aterm-uds-oversend.")
        .tempdir()
        .expect("scratch");
    let file = std::fs::File::create(scratch.path().join("passed")).expect("file");
    let want = file.metadata().map(|m| (m.dev(), m.ino())).expect("fstat");
    let copies = || {
        (0..4096)
            .filter(|&fd| {
                // SAFETY: `raw` is borrowed for a single `fstat` and not closed.
                let f = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(fd) });
                f.metadata().is_ok_and(|m| (m.dev(), m.ino()) == want)
            })
            .count()
    };
    let (a, b) = crate::CtlStream::pair().expect("pair");
    let mut control = SenderControl([0; cmsg_space(SENT * FD_LEN)]);
    abi::write_cmsg_len(&mut control.0, cmsg_len(SENT * FD_LEN));
    write_i32(&mut control.0, abi::LEVEL_OFFSET, abi::SOL_SOCKET);
    write_i32(&mut control.0, abi::TYPE_OFFSET, abi::SCM_RIGHTS);
    for slot in control.0[CMSG_DATA_OFFSET..cmsg_len(SENT * FD_LEN)]
        .as_chunks_mut::<FD_LEN>()
        .0
    {
        slot.copy_from_slice(&file.as_raw_fd().to_ne_bytes());
    }
    let payload = [1u8];
    let mut iov = super::IoVec {
        iov_base: payload.as_ptr().cast_mut().cast(),
        iov_len: payload.len(),
    };
    let msg = abi::msghdr(&mut iov, 1, control.0.as_mut_ptr(), cmsg_len(SENT * FD_LEN));
    // SAFETY: `msg` references live `iov` and `control` buffers, and the control length it
    // declares is inside `control`.
    let sent = unsafe { super::sendmsg(a.as_raw_fd(), &msg, abi::SEND_FLAGS) };
    assert_eq!(sent, 1, "{}", std::io::Error::last_os_error());
    let before = copies();
    let mut buf = [0u8; 4];
    let refused = super::recv_with_fds(&b, &mut buf, super::MAX_FDS).err();
    assert_eq!(
        refused.map(|e| e.kind()),
        Some(std::io::ErrorKind::InvalidData)
    );
    assert_eq!(copies(), before, "a descriptor the peer sent stayed open");
}

#[test]
fn subgroup_maximum_rights_keep_original_open_descriptions() {
    use std::io::{Seek as _, SeekFrom};
    use std::os::fd::{AsFd as _, AsRawFd as _};
    let scratch = aterm_tempfile::tempdir().unwrap();
    let mut original = std::fs::File::create(scratch.path().join("original")).unwrap();
    let (sender, receiver) = crate::CtlStream::pair().unwrap();
    let rights: Vec<_> = (0..super::MAX_FDS).map(|_| original.as_fd()).collect();
    assert_eq!(super::send_with_fds(&sender, &[17], &rights).unwrap(), 1);
    let mut marker = [0; 2];
    let mut received = super::recv_with_fds(&receiver, &mut marker, super::MAX_FDS).unwrap();
    assert_eq!(
        (received.bytes, marker[0], received.fds.len()),
        (1, 17, super::MAX_FDS)
    );
    for fd in &received.fds {
        // SAFETY: each borrowed descriptor remains owned and live in
        // the receive vector; F_GETFD only reads its descriptor flags.
        assert_ne!(
            unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC,
            0
        );
    }
    let mut first = std::fs::File::from(received.fds.remove(0));
    let mut last = std::fs::File::from(received.fds.pop().unwrap());
    first.seek(SeekFrom::Start(7)).unwrap();
    assert_eq!(last.stream_position().unwrap(), 7);
    assert_eq!(original.stream_position().unwrap(), 7);
}

/// A receiver at its descriptor limit, where the kernel installs fewer descriptors than
/// were sent, refuses the message and keeps none that did arrive: Linux installs what
/// fits, names only those and flags `MSG_CTRUNC` (the flag alone tells it from a whole
/// message), and Darwin fails the `recvmsg` (`EMFILE`).
#[test]
fn a_message_the_kernel_could_not_install_whole_is_refused() {
    use std::os::fd::{AsFd as _, AsRawFd as _};
    unsafe extern "C" {
        fn alarm(seconds: u32) -> u32;
    }
    const REFUSED: i32 = 0;
    const KEPT: i32 = 1;
    const ACCEPTED: i32 = 2;
    const NO_SETUP: i32 = 3;
    let (a, b) = crate::CtlStream::pair().expect("pair");
    super::send_with_fds(&a, b"x", &[a.as_fd(), a.as_fd()]).expect("sent");
    crate::spawnfd::pass_libnotify_gate();
    // SAFETY: child only runs async-signal-safe syscalls before `_exit`.
    let child = unsafe { libc::fork() };
    if child == 0 {
        let receive = || {
            let fd = b.as_raw_fd();
            // SAFETY: `dup2` and `close` on valid descriptors in the child process.
            unsafe {
                alarm(60);
                let (first, second) = (libc::dup(fd), libc::dup(fd));
                let mut lim = libc::rlimit {
                    rlim_cur: 0,
                    rlim_max: 0,
                };
                if first < 0
                    || second < 0
                    || libc::close(first) != 0
                    || libc::close(second) != 0
                    || libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) != 0
                {
                    return NO_SETUP;
                }
                lim.rlim_cur = second as libc::rlim_t;
                if libc::setrlimit(libc::RLIMIT_NOFILE, &lim) != 0 {
                    return NO_SETUP;
                }
                let mut buf = [0u8; 4];
                match super::recv_with_fds(&b, &mut buf, super::MAX_FDS) {
                    Ok(_) => ACCEPTED,
                    Err(_) if libc::dup(fd) == first => REFUSED,
                    Err(_) => KEPT,
                }
            }
        };
        // SAFETY: child process exits immediately via `_exit`.
        unsafe { libc::_exit(receive()) }
    }
    assert!(child > 0, "fork: {}", std::io::Error::last_os_error());
    let mut status = 0;
    // SAFETY: reaps the child process spawned above.
    assert_eq!(unsafe { libc::waitpid(child, &mut status, 0) }, child);
    assert!(libc::WIFEXITED(status), "the child died: {status:#x}");
    assert_eq!(libc::WEXITSTATUS(status), REFUSED);
}

/// A buffer too short to hold a header is no message, not a truncated one.
#[test]
fn an_empty_control_area_is_empty_not_truncated() {
    for keep in [0, 1, CMSG_HDR_LEN - 1] {
        let buf = control(1, &[7], keep);
        let scan = scan_control(&buf);
        assert!(scan.fds.is_empty());
        assert!(!scan.truncated, "{keep} bytes is no message at all");
    }
}
