// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::io::BufRead as _;
use std::os::fd::AsRawFd as _;

#[test]
fn unready_preflight_is_a_refusal_without_submitting_execution_descriptors() {
    refusing_preflight(false, false);
}

#[test]
fn missing_original_store_capability_refuses_without_submitting_root_descriptors() {
    refusing_preflight(true, true);
}

#[test]
fn owner_refusal_keeps_its_authority_error_before_any_execution_attachment() {
    refusing_reply(
        Some("ERR resource-admission-v3 refused local-owner-only\n"),
        false,
        io::ErrorKind::PermissionDenied,
        "local-owner-only",
    );
}

#[test]
fn malformed_capabilities_and_connection_loss_are_not_capacity_waits() {
    for (reply, reason) in [
        (
            "OK resource-admission-v3 capabilities\nnot-json\n",
            "invalid host capability response",
        ),
        (
            "OK resource-admission-v3 capabilities\n{\"version\":2,\"ready\":false}\n",
            "invalid host capability response",
        ),
        ("ERR unknown verb\n", "ERR unknown verb"),
    ] {
        refusing_reply(Some(reply), false, io::ErrorKind::InvalidData, reason);
    }
    refusing_reply(
        None,
        false,
        io::ErrorKind::UnexpectedEof,
        "without responding",
    );
}

fn refusing_preflight(ready: bool, expectation: bool) {
    let reply = format!(
        "OK resource-admission-v3 capabilities\n{{\"version\":3,\"ready\":{ready},\"accounting\":\"owning-wait-accounting-v1\"}}\n"
    );
    refusing_reply(
        Some(&reply),
        expectation,
        if ready && expectation {
            io::ErrorKind::Unsupported
        } else {
            io::ErrorKind::WouldBlock
        },
        "retained host",
    );
}

fn refusing_reply(reply: Option<&str>, expectation: bool, kind: io::ErrorKind, reason: &str) {
    let reply = reply.map(str::to_owned);
    let directory = crate::test_scratch_dir("resource-unready");
    let path = directory.path().join("socket");
    let listener = aterm_uds::CtlListener::bind(&path).unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        )
        .unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut reader = std::io::BufReader::new(&stream);
        let mut request = String::new();
        reader.read_line(&mut request).unwrap();
        if request.starts_with("AUTH ") {
            request.clear();
            reader.read_line(&mut request).unwrap();
        }
        assert_eq!(request, "resource-admission-v3 capabilities\n");
        if let Some(reply) = reply {
            stream.write_all(reply.as_bytes()).unwrap();
        }
        listener
    });
    let (mut read, write) = aterm_uds::pipe().unwrap();
    let args = super::super::Args {
        sock: None,
        session: "@self".into(),
        ai: None,
        job: "unready-test".into(),
        command: vec!["/usr/bin/false".into()],
        capabilities: false,
        accounted: expectation,
        expected_store: expectation.then(|| std::fs::File::open(directory.path()).unwrap()),
        resources: Some(super::super::Resources {
            cpu_units: 1,
            memory_bytes: 1024,
            timeout_ms: Some(1),
            status_fd: Some(write.as_raw_fd()),
        }),
    };
    let wire = super::super::super::ConnWire {
        path: path.to_string_lossy().into_owned(),
        origin: crate::TargetOrigin::Pinned,
    };
    let error = run(&wire, "s-a", &args).unwrap_err();
    assert_eq!(error.kind(), kind);
    assert!(error.to_string().contains(reason), "{error}");
    drop(write);
    let mut status = String::new();
    read.read_to_string(&mut status).unwrap();
    let value: aterm_json::Value = aterm_json::from_str(status.trim()).unwrap();
    assert_eq!(value["event"].as_str(), Some("refused"));
    assert_eq!(value["version"].as_u64(), Some(3));
    assert!(!status.contains("transport_unknown"));
    assert!(value["reason"].as_str().unwrap().contains(reason));
    let listener = server.join().unwrap();
    assert_eq!(
        aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Ready,
            aterm_uds::AcceptedMode::Nonblocking,
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::WouldBlock
    );
}

#[test]
fn accounted_root_requires_new_terminal_and_clean_eof_preserving_observed_zero() {
    for (legacy, trailing) in [(true, false), (false, true), (false, false)] {
        let (client, host) = CtlStream::pair().unwrap();
        let binding = Binding {
            session: "s-a".into(),
            attempt: [12; 16],
        };
        let expected = binding.clone();
        let writer = std::thread::spawn(move || {
            let deadline = Instant::now() + HANDOFF;
            transport::send_event(
                &host,
                &EventFrame {
                    binding: expected.clone(),
                    event: Event::Started {
                        pid: 2,
                        birth: 1,
                        monotonic_ns: 1,
                        unix_ns: 1,
                    },
                },
                deadline,
            )
            .unwrap();
            let terminal = EventFrame {
                binding: expected,
                event: Event::Finished {
                    outcome: Outcome::Exit(7),
                    cause: CompletionCause::Exited,
                    runtime_ns: 1,
                },
            };
            if legacy {
                transport::send_event(&host, &terminal, deadline).unwrap();
            } else {
                transport::write_frame(
                    &host,
                    &AccountedEventFrame {
                        finished: terminal,
                        observation: AccountingObservation {
                            scope: AccountingScope::ChildAndWaitedDescendantsV1,
                            cpu_times: Some(aterm_types::resource_admission::CpuTimes {
                                user_ns: 0,
                                system_ns: 0,
                            }),
                            peak_rss_bytes: Some(0),
                        },
                    }
                    .encode()
                    .unwrap(),
                    deadline,
                )
                .unwrap();
                if trailing {
                    transport::write_all(&host, &[0], deadline).unwrap();
                }
            }
        });
        let result = receive_mode(
            &client,
            &binding,
            &mut None,
            Some(Instant::now() + HANDOFF),
            true,
        );
        assert_eq!(result.is_ok(), !legacy && !trailing);
        if let Ok(code) = result {
            assert_eq!(code, ExitCode::from(7));
        }
        writer.join().unwrap();
    }
}
