// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Actual private-lane rights and held backend observations. This component
//! does not enroll a GUI, allocate a root or select a private CPU domain.
use super::*;
use std::{fs::File, os::fd::AsRawFd as _};

const ROLE: &str = "ATERM_ORIGINAL_STORE_FORWARD_TEST";
const EXACT: &str =
    "forward::store_tests::original_store_forwarding_checks_actual_backend_before_root_attachments";

#[test]
fn original_store_forwarding_checks_actual_backend_before_root_attachments() {
    if std::env::var_os(ROLE).is_some() {
        return child();
    }
    let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
    command
        .args(["--exact", EXACT, "--nocapture"])
        .env(ROLE, "1");
    let output = aterm_uds::deadline::output_within(&mut command, Duration::from_secs(60)).unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("original-store-forwarding-checked"));
}

fn child() {
    let selected = aterm_tempfile::tempdir().unwrap();
    let foreign = aterm_tempfile::tempdir().unwrap();
    let root = selected.path().canonicalize().unwrap().join("store");
    let _store = aterm_capacity::host_executable::Store::prepare_at(&root).unwrap();
    crate::runtime::test_endpoint_with_store(&root).unwrap();
    assert!(crate::runtime::store_expectation_supported());
    let expected: OwnedFd = File::open(&root).unwrap().into();
    let wrong: OwnedFd = File::open(foreign.path()).unwrap().into();
    let (client, server) = CtlStream::pair().unwrap();
    let peer = wire::local_peer(&client).unwrap();
    let worker = std::thread::spawn(move || {
        serve(&server, |actual| {
            if actual == peer {
                Ok(())
            } else {
                Err(refused())
            }
        })
    });
    let lane = Forwarder::new(client, peer).unwrap();
    assert!(lane.matches_original_store(&expected).unwrap());
    assert!(!lane.matches_original_store(&wrong).unwrap());
    assert!(
        !lane.is_broken(),
        "a known mismatch must not invent a transport failure"
    );
    // Same-name substitution cannot replace the backend retained in ENDPOINT.
    std::fs::rename(&root, selected.path().join("original")).unwrap();
    std::fs::create_dir(&root).unwrap();
    let replacement: OwnedFd = File::open(&root).unwrap().into();
    assert!(!lane.matches_original_store(&replacement).unwrap());
    assert!(lane.matches_original_store(&expected).unwrap());

    let request = Submission {
        binding: aterm_types::resource_admission::Binding {
            session: "s-acde".into(),
            attempt: [46; 16],
        },
        peer_pid: peer.pid,
        peer_birth: peer.birth,
        argv: vec![
            b"/sdk/bin/targo".to_vec(),
            b"--resource-workload-v1".to_vec(),
        ],
        cwd: b"/work".to_vec(),
        env: vec![],
        cpu_units: 1,
        memory_bytes: 1,
        timeout_ms: Some(1000),
        label: "foreign original Store expectation".into(),
    };
    // Send only op7+request+foreign expectation. The custodian must answer
    // false while NO stdin/stdout/stderr/events descriptions have been offered.
    lane.with(|stream, deadline| {
        let mut frame = vec![SUBMIT_ACCOUNTED_EXPECT_STORE];
        frame.extend(request.encode()?);
        wire::write_frame(stream, &frame, deadline)?;
        wire::store_expectation::send(stream, wrong.as_fd(), deadline)?;
        assert_eq!(wire::read_frame(stream, deadline)?, [0]);
        Ok(())
    })
    .unwrap();
    assert!(lane.matches_original_store(&expected).unwrap());
    assert!(
        aterm_uds::spawnfd::descriptor_metadata(expected.as_raw_fd())
            .unwrap()
            .is_dir()
    );
    drop(lane);
    assert!(worker.join().unwrap().is_err());
    println!("original-store-forwarding-checked");
}
