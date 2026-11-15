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

#[test]
fn an_unavailable_observation_preserves_the_private_lane_for_the_next_request() {
    let (client, server) = aterm_uds::kept_out(CtlStream::pair).unwrap();
    server.set_nonblocking(true).unwrap();
    let custodian = wire::local_peer(&client).unwrap();
    let forwarder = Forwarder::new(client, custodian).unwrap();
    let worker = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(60);
        assert_eq!(wire::read_frame(&server, deadline).unwrap(), [CAPABILITIES]);
        json_write(
            &server,
            &aterm_json::json!({"resource_observation":"origin-wait-v1"}),
            deadline,
        )
        .unwrap();
        let observation = wire::read_frame(&server, deadline).unwrap();
        assert_eq!(observation[0], RESOURCE_OBSERVATION);
        json_write(
            &server,
            &aterm_json::json!({"version":1,"unavailable":true}),
            deadline,
        )
        .unwrap();
        assert_eq!(wire::read_frame(&server, deadline).unwrap(), [CAPABILITIES]);
        json_write(&server, &aterm_json::json!({"ready":true}), deadline).unwrap();
    });
    assert_eq!(
        forwarder.resource_observation(0).unwrap_err().kind(),
        io::ErrorKind::Other
    );
    assert!(
        !forwarder.is_broken(),
        "a fully framed telemetry refusal is synchronized"
    );
    assert_eq!(
        forwarder.capabilities().unwrap()["ready"].as_bool(),
        Some(true)
    );
    worker.join().unwrap();
}

#[test]
fn a_waiting_observer_does_not_take_the_execution_connections_lock() {
    let (control_client, control_server) = aterm_uds::kept_out(CtlStream::pair).unwrap();
    let (observer_client, observer_server) = aterm_uds::kept_out(CtlStream::pair).unwrap();
    control_server.set_nonblocking(true).unwrap();
    observer_server.set_nonblocking(true).unwrap();
    let custodian = wire::local_peer(&control_client).unwrap();
    let control = Forwarder::new(control_client, custodian).unwrap();
    let observer = Forwarder::new(observer_client, custodian).unwrap();
    let (waiting, observed_wait) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel();
    let responder = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(60);
        assert_eq!(
            wire::read_frame(&observer_server, deadline).unwrap(),
            [CAPABILITIES]
        );
        json_write(
            &observer_server,
            &aterm_json::json!({"resource_observation":"origin-wait-v1"}),
            deadline,
        )
        .unwrap();
        assert_eq!(
            wire::read_frame(&observer_server, deadline).unwrap()[0],
            RESOURCE_OBSERVATION
        );
        waiting.send(()).unwrap();
        released.recv_timeout(Duration::from_secs(60)).unwrap();
        json_write(
            &observer_server,
            &aterm_json::json!({"version":1,"unavailable":true}),
            deadline,
        )
        .unwrap();
    });
    let reader = std::thread::spawn(move || observer.resource_observation(0));
    observed_wait.recv_timeout(Duration::from_secs(60)).unwrap();
    let writer = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(60);
        assert_eq!(
            wire::read_frame(&control_server, deadline).unwrap(),
            [CAPABILITIES]
        );
        json_write(
            &control_server,
            &aterm_json::json!({"ready":true}),
            deadline,
        )
        .unwrap();
    });
    assert_eq!(
        control.capabilities().unwrap()["ready"].as_bool(),
        Some(true)
    );
    release.send(()).unwrap();
    assert_eq!(
        reader.join().unwrap().unwrap_err().kind(),
        io::ErrorKind::Other
    );
    responder.join().unwrap();
    writer.join().unwrap();
}
