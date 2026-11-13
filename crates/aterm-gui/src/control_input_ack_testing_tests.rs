// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::io::Read;
use std::os::fd::{AsRawFd, OwnedFd};

pub(crate) fn owned_handle(local: u64) -> (crate::session_store::SessionHandle, std::fs::File) {
    let (reader, writer) = aterm_uds::pipe().expect("private pipe");
    let reader = std::fs::File::from(OwnedFd::from(reader));
    let writer = OwnedFd::from(writer);
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
