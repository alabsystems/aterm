// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

pub(crate) fn owned_handle(local: u64) -> (crate::session_store::SessionHandle, std::fs::File) {
    let mut fds = [-1; 2];
    // SAFETY: pipe writes two fresh descriptors on success.
    let result = unsafe { libc::pipe(fds.as_mut_ptr()) };
    assert_eq!(result, 0, "private pipe");
    // SAFETY: both descriptors were just created, and each is adopted once.
    let reader = unsafe { std::fs::File::from_raw_fd(fds[0]) };
    let writer = unsafe { OwnedFd::from_raw_fd(fds[1]) };
    for fd in [reader.as_raw_fd(), writer.as_raw_fd()] {
        // SAFETY: an owned descriptor remains alive across this call.
        assert_eq!(
            unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) },
            0
        );
    }
    let flags = unsafe { libc::fcntl(reader.as_raw_fd(), libc::F_GETFL) };
    assert!(flags >= 0);
    assert_eq!(
        unsafe { libc::fcntl(reader.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) },
        0
    );
    let mut handle = crate::session_store::test_handle_on(local, writer.as_raw_fd());
    Arc::get_mut(&mut handle.ctx)
        .expect("unpublished context")
        .sink = Arc::new(aterm_session::sink::SinkWriter::new_owned(writer));
    (handle, reader)
}

pub(crate) fn paste(
    handle: &crate::session_store::SessionHandle,
    mark: &InputMark,
    text: &str,
) -> bool {
    let event =
        crate::input::InputEvent::Paste(text.to_owned(), crate::input::PasteFraming::AtDrain);
    input(handle, mark, &event)
}

/// `event` written to the program under `mark`, as the control path writes it.
pub(crate) fn input(
    handle: &crate::session_store::SessionHandle,
    mark: &InputMark,
    event: &crate::input::InputEvent,
) -> bool {
    let receipt = crate::app_input::tracked_egress(
        &handle.term,
        &handle.ctx.modes,
        &handle.ctx.sink,
        &handle.ctx.output_echo,
        event,
        crate::input::EgressMode::Backpressured,
    );
    let accepted = receipt.fully_accepted_nonempty();
    mark.completion().finish(receipt);
    accepted
}

pub(crate) fn input_count(handle: &crate::session_store::SessionHandle) -> usize {
    handle
        .ctx
        .timeline
        .lock()
        .unwrap()
        .slog()
        .log()
        .window()
        .filter(|r| r.k == aterm_slog::Kind::Input)
        .count()
}

pub(crate) fn byte(reader: &mut std::fs::File) -> u8 {
    let mut one = [0];
    assert_eq!(
        reader
            .read(&mut one)
            .expect("accepted byte remains readable"),
        1
    );
    one[0]
}
