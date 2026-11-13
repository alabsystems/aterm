// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_scheduled_loop_must_acknowledge_the_fresh_fence() {
    let (send, reply) = std::sync::mpsc::channel();
    let started = Instant::now() - Duration::from_millis(250);
    let mut pumped = 0;
    assert!(observe(&reply, started, || {
        pumped += 1;
        send.send(()).unwrap();
    }));
    assert_eq!(pumped, 1);
    send.send(()).unwrap();
    assert!(!observe(
        &reply,
        Instant::now() - Duration::from_secs(61),
        || panic!("expired fence was pumped")
    ));
    assert_eq!(
        reply.try_recv(),
        Ok(()),
        "expiry cannot consume a late acknowledgement"
    );
    drop(send);
    assert!(!observe(&reply, Instant::now(), || panic!(
        "disconnected fence was pumped"
    )));
}
