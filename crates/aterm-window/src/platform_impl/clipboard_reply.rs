// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Added by the aterm project in 2026; see the repository NOTICE.

//! The required loop acknowledgement for an off-loop clipboard copy.

use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::Duration;

/// The hang detector for an acknowledgement the event loop must send.
pub(super) const COPY_HANG: Duration = Duration::from_secs(60);

/// Receive the loop's actual copy verdict.
///
/// # Errors
/// Timeout after the hang detector, or disconnected when no queued reply remains.
pub(super) fn receive<T>(reply: &Receiver<T>) -> Result<T, RecvTimeoutError> {
    receive_after(reply, Duration::ZERO)
}

fn receive_after<T>(reply: &Receiver<T>, elapsed: Duration) -> Result<T, RecvTimeoutError> {
    let remaining = COPY_HANG
        .checked_sub(elapsed)
        .filter(|remaining| !remaining.is_zero())
        .ok_or(RecvTimeoutError::Timeout)?;
    reply.recv_timeout(remaining)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_clipboard_reply_waits_for_a_scheduled_loop() {
        let (send, reply) = std::sync::mpsc::channel();
        send.send(3).unwrap();
        send.send(7).unwrap();
        assert_eq!(receive(&reply), Ok(3));
        assert_eq!(receive_after(&reply, Duration::from_secs(4)), Ok(7));

        send.send(9).unwrap();
        assert_eq!(receive_after(&reply, Duration::from_secs(61)), Err(RecvTimeoutError::Timeout));
        assert_eq!(reply.try_recv(), Ok(9), "expiry never consumes a queued verdict");
        drop(send);
        assert_eq!(receive(&reply), Err(RecvTimeoutError::Disconnected));
    }
}
