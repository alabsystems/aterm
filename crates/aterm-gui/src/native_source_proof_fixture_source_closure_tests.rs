// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
#[test]
fn guardian_large_reply_preserves_offset_under_backpressure() {
    let (mut writer, mut reader) = CtlStream::pair().unwrap();
    writer.set_nonblocking(true).unwrap();
    let size: libc::c_int = 1024;
    // SAFETY: valid socket, integer option pointer and its exact byte size.
    assert_eq!(
        unsafe {
            libc::setsockopt(
                std::os::fd::AsRawFd::as_raw_fd(&writer),
                libc::SOL_SOCKET,
                libc::SO_SNDBUF,
                (&size as *const libc::c_int).cast(),
                std::mem::size_of_val(&size) as libc::socklen_t,
            )
        },
        0
    );
    let (start, wait) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        wait.recv_timeout(MUST_HAPPEN).unwrap();
        let mut received = Vec::new();
        reader.read_to_end(&mut received).unwrap();
        received
    });
    let payload =
        vec![b'x'; aterm_agent::harness::codex_source_registry::SOURCE_PROOF_OBSERVATION_LIMIT];
    let mut pumps = 0;
    write_guardian(&mut writer, &payload, || {
        pumps += 1;
        if pumps == 2 {
            start.send(()).unwrap();
        }
    })
    .unwrap();
    assert!(pumps >= 2);
    drop(writer);
    assert_eq!(worker.join().unwrap(), payload);
}

#[test]
fn earlier_member_closed_cannot_cover_last_release_window() {
    let epoch = "0123456789abcdef";
    let mut closed = std::collections::BTreeSet::new();
    closed.insert(format!("{epoch}{:016x}", 1));
    assert!(!all_members_closed(&closed, epoch, 2));
    closed.insert(format!("{epoch}{:016x}", 2));
    assert!(all_members_closed(&closed, epoch, 2));
    assert!(!all_members_closed(&closed, "fedcba9876543210", 2));
}
