// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
#[test]
fn a_valid_private_reply_survives_scheduling_delay() {
    let (client, server) = aterm_uds::kept_out(CtlStream::pair).unwrap();
    client.set_nonblocking(true).unwrap();
    server.set_nonblocking(true).unwrap();
    let custodian = wire::local_peer(&client).unwrap();
    let forwarder = Forwarder::new(client, custodian).unwrap();
    let worker = std::thread::spawn(move || {
        let until = Instant::now() + Duration::from_secs(60);
        assert_eq!(wire::read_frame(&server, until).unwrap(), [CAPABILITIES]);
        let _ = json_write(&server, &aterm_json::json!({"ready":true}), until);
    });
    let result = forwarder.with_at(
        Instant::now() - Duration::from_secs(3),
        |stream, deadline| {
            wire::write_frame(stream, &[CAPABILITIES], deadline)?;
            json_reply(stream, deadline)
        },
    );
    worker.join().unwrap();
    assert_eq!(result.unwrap(), aterm_json::json!({"ready":true}));
    assert!(!forwarder.is_broken());
    assert!(
        forwarder
            .with_at(
                Instant::now() - Duration::from_secs(61),
                |stream, deadline| { wire::write_frame(stream, &[CAPABILITIES], deadline) }
            )
            .is_err()
    );
    assert!(forwarder.is_broken());
}
