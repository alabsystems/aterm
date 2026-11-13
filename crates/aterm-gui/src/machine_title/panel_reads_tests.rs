// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn superseded_requests_are_released_before_the_worker_reads() {
    let (send, requests) = channel();
    let old = Arc::new(());
    let newest = Arc::new(());
    send.send(old.clone()).unwrap();
    send.send(newest.clone()).unwrap();
    assert_eq!(Arc::strong_count(&old), 1);
    assert_eq!(Arc::strong_count(&newest), 2);
    drop(send);
    let mut seen = 0;
    run(requests, |request| {
        assert!(Arc::ptr_eq(&request, &newest));
        seen += 1;
    });
    assert_eq!(seen, 1);
    assert_eq!(Arc::strong_count(&newest), 1);
}

#[test]
fn a_stopped_worker_rejects_and_releases_the_request() {
    let (send, requests) = channel();
    drop(requests);
    let request = Arc::new(());
    assert!(send.send(request.clone()).is_err());
    assert_eq!(Arc::strong_count(&request), 1);
}

#[test]
fn a_busy_browse_keeps_only_the_newest_pending_panel() {
    let (send, requests) = channel();
    let mut seen = Vec::new();
    send.send(1).unwrap();
    let mut sender = Some(send);
    run(requests, |press| {
        seen.push(press);
        if let Some(send) = sender.take() {
            send.send(2).unwrap();
            send.send(3).unwrap();
        }
    });
    assert_eq!(seen, [1, 3]);
}
