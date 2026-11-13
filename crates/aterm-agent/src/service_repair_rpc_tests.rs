// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
#[test]
fn bounded_dispatch_preserves_caller_and_signed_payload() {
    let payload = json!({"signed":{"request":{"nonce":"same-nonce-123456"},"key":"key","signature":"signature"}});
    let line = encode("activate", &payload).unwrap();
    let rest = line.strip_prefix("service-repair ").unwrap();
    let reply = serve_with(
        "proved-node",
        "proved-key",
        rest,
        |op, value, caller, key| {
            assert_eq!(op, "activate");
            assert_eq!(caller, "proved-node");
            assert_eq!(key, "proved-key");
            assert_eq!(*value, payload);
            Ok(json!({"signed-result":"unchanged"}))
        },
    );
    assert!(reply.starts_with("OK 1\n"));
    assert_eq!(
        decode("activate", reply.strip_prefix("OK 1\n").unwrap()).unwrap(),
        json!({"signed-result":"unchanged"})
    );
}
#[test]
fn malformed_or_oversized_input_never_dispatches() {
    for rest in [
        "{}".to_owned(),
        "x".repeat(MAX_REQUEST + 1),
        json!({"schema":1,"op":"shell","payload":{"argv":["rm"]}}).to_string(),
        json!({"schema":1,"op":"inspect","payload":{"job":"server","path":"/etc"}}).to_string(),
    ] {
        assert!(
            serve_with("node", "key", &rest, |_, _, _, _| panic!(
                "invalid input dispatched"
            ))
            .starts_with("ERR ")
        );
    }
    let line = encode("inspect", &json!({"job":"server"})).unwrap();
    assert!(
        serve_with(
            "",
            "",
            line.strip_prefix("service-repair ").unwrap(),
            |_, _, _, _| panic!("anonymous dispatch")
        )
        .starts_with("ERR ")
    );
}
#[test]
fn reply_budget_and_wrong_operation_fail_closed() {
    let line = encode("inspect", &json!({"job":"server"})).unwrap();
    assert!(
        serve_with(
            "node",
            "key",
            line.strip_prefix("service-repair ").unwrap(),
            |_, _, _, _| Ok(json!("x".repeat(MAX_REPLY)))
        )
        .starts_with("ERR ")
    );
    assert!(
        decode(
            "activate",
            &json!({"schema":1,"op":"inspect","result":{}}).to_string()
        )
        .is_err()
    );
}

#[cfg(unix)]
#[test]
fn maximal_local_outcome_with_escaped_diagnostics_fits_remote_recovery() {
    use aterm_secrets::{
        service::Job,
        service_repair::{
            self, HealthRequirement, Observed, Outcome, Request as Activation, Unit, Verdict,
        },
    };
    let unit = Unit {
        program: "/store/aterm".into(),
        args: vec!["secrets".into(), "serve".into()],
        definition: String::new(),
        image: "image".into(),
        executable_sha256: "digest".into(),
    };
    let mut request = Activation {
        node: "node".into(),
        requester: "requester".into(),
        nonce: "0123456789abcdef".into(),
        job: Job::Server,
        current_image: "old".into(),
        desired_image: "image".into(),
        precondition: Observed {
            unit: Some(unit.clone()),
            off: false,
            pin: None,
        },
        unit,
        health: HealthRequirement {
            vault: "vault".into(),
            signer: "signer".into(),
            records: vec!["record".into()],
            frontier: 1,
        },
        required_protocols: vec![1],
    };
    let empty_size = aterm_json::to_vec(&service_repair::request_value(&request))
        .unwrap()
        .len();
    request.unit.definition = "x".repeat(service_repair::MAX_REQUEST_BYTES - empty_size);
    assert_eq!(
        aterm_json::to_vec(&service_repair::request_value(&request))
            .unwrap()
            .len(),
        service_repair::MAX_REQUEST_BYTES
    );
    let outcome = Outcome {
        request,
        verdict: Verdict::RollbackFailed,
        detail: "\0".repeat(16 * 1024),
    };
    let line = encode("recover", &json!({"job":"server"})).unwrap();
    let reply = serve_with(
        "caller",
        "key",
        line.strip_prefix("service-repair ").unwrap(),
        |_, _, _, _| {
            Ok(
                json!({"payload":service_repair::outcome_value(&outcome),"node":"node","key":"a".repeat(64),"signature":"b".repeat(128)}),
            )
        },
    );
    let body = reply
        .strip_prefix("OK 1\n")
        .expect("maximal local outcome fits signed RPC reply");
    assert!(body.len() > 256 * 1024 && body.len() <= MAX_REPLY);
    assert_eq!(
        decode("recover", body).unwrap()["payload"]["detail"].as_str(),
        Some(outcome.detail.as_str())
    );
}

#[test]
fn result_identity_must_match_the_actual_handshake() {
    let result = json!({"node":"target","key":"target-key"});
    assert!(verify_peer(&result, "target", "target-key").is_ok());
    assert!(verify_peer(&result, "other-node", "target-key").is_err());
    assert!(verify_peer(&result, "target", "substituted-key").is_err());
}

#[test]
fn isolated_and_custom_contexts_never_dispatch_to_live_services() {
    use std::path::{Path, PathBuf};
    let live = || {
        Ok((
            PathBuf::from("live-home"),
            PathBuf::from("live-vault"),
            PathBuf::from("live-state"),
        ))
    };
    assert!(
        in_context(
            None,
            Path::new("live-state"),
            || panic!("missing context read live state"),
            || panic!("isolated context dispatched")
        )
        .starts_with("ERR ")
    );
    for (home, vault, state) in [
        ("custom-home", "custom-vault", "live-state"),
        ("live-home", "custom-vault", "live-state"),
        ("live-home", "live-vault", "custom-state"),
    ] {
        assert!(
            in_context(
                Some((Path::new(home), Path::new(vault))),
                Path::new(state),
                live,
                || panic!("custom context dispatched")
            )
            .starts_with("ERR ")
        );
    }
    assert_eq!(
        in_context(
            Some((Path::new("live-home"), Path::new("live-vault"))),
            Path::new("live-state"),
            live,
            || "dispatched".into()
        ),
        "dispatched"
    );
}

#[cfg(unix)]
#[test]
fn startup_registration_is_inert() {
    fn must_not_run(_: aterm_secrets::client::Paths, _: std::path::PathBuf) {
        panic!("registration launched recovery");
    }
    let registry = OnceLock::new();
    assert!(register_recovery(&registry, must_not_run).is_ok());
    assert!(register_recovery(&registry, must_not_run).is_err());
}

#[test]
fn startup_context_refusal_does_not_consume_the_once_only_launch() {
    use std::{
        cell::Cell,
        path::{Path, PathBuf},
        sync::atomic::AtomicBool,
    };
    let started = AtomicBool::new(false);
    let calls = Cell::new(0);
    let live = || {
        Ok((
            PathBuf::from("home"),
            PathBuf::from("vault"),
            PathBuf::from("state"),
        ))
    };
    let launch = || {
        launch_once(&started, || calls.set(calls.get() + 1));
        String::new()
    };
    let _ = in_context(None, Path::new("state"), live, launch);
    let _ = in_context(
        Some((Path::new("custom"), Path::new("vault"))),
        Path::new("state"),
        live,
        launch,
    );
    assert_eq!(calls.get(), 0);
    let _ = in_context(
        Some((Path::new("home"), Path::new("vault"))),
        Path::new("state"),
        live,
        launch,
    );
    let _ = in_context(
        Some((Path::new("home"), Path::new("vault"))),
        Path::new("state"),
        live,
        launch,
    );
    assert_eq!(calls.get(), 1);
}

#[cfg(unix)]
#[test]
fn counted_wire_round_trip_preserves_signed_result() {
    use std::io::{BufRead as _, Write as _};
    let (mut server, client) = std::os::unix::net::UnixStream::pair().unwrap();
    server
        .set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    server
        .set_write_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    client
        .set_write_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    let worker = std::thread::spawn(move || {
        let mut line = String::new();
        std::io::BufReader::new(&mut server)
            .read_line(&mut line)
            .unwrap();
        let rest = line.trim_end().strip_prefix("service-repair ").unwrap();
        let reply = serve_with("caller", "key", rest, |operation, payload, caller, key| {
            assert_eq!(operation, "inspect");
            assert_eq!(payload["job"].as_str(), Some("server"));
            assert_eq!((caller, key), ("caller", "key"));
            Ok(
                json!({"payload":{"unit":null},"node":"target","key":"signed-key","signature":"signature"}),
            )
        });
        server.write_all(reply.as_bytes()).unwrap();
    });
    let line = encode("inspect", &json!({"job":"server"})).unwrap();
    let (header, body) = crate::RelayClient::new(client)
        .request_counted_with_limit(&line, MAX_REPLY)
        .unwrap();
    assert_eq!(header, "OK 1");
    assert_eq!(
        decode("inspect", &body).unwrap()["signature"].as_str(),
        Some("signature")
    );
    worker.join().unwrap();
}
