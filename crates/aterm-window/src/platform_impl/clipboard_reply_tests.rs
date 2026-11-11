// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Added by the aterm project in 2026; see the repository NOTICE.

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
