// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use super::*;

#[test]
fn private_header_trace_recovers_numeric_retry_after_then_is_removed() {
    let root = aterm_tempfile::Builder::new()
        .prefix("agh-git-trace-")
        .tempdir()
        .unwrap();
    let trace = root.path().join("curl.trace");
    std::fs::write(
        &trace,
        b"00:00:00 <= Recv header: HTTP/2 429\r\n\
          00:00:00 <= Recv header: Retry-After: 7\r\n",
    )
    .unwrap();
    assert_eq!(
        git_trace_observation(&trace, None),
        GitObservation::from_trace(
            "00:00:00 <= Recv header: HTTP/2 429\r\n\
             00:00:00 <= Recv header: Retry-After: 7\r\n"
        )
    );
    assert!(!trace.exists());
}

#[test]
fn private_header_trace_uses_the_run_clock_for_http_date_retry_after() {
    let root = aterm_tempfile::Builder::new()
        .prefix("agh-git-trace-date-")
        .tempdir()
        .unwrap();
    let trace = root.path().join("curl.trace");
    let text = "00:00:00 <= Recv header: HTTP/2 429\r\n\
                00:00:00 <= Recv header: Retry-After: Sun, 08 Nov 2026 12:00:00 GMT\r\n";
    std::fs::write(&trace, text).unwrap();
    let now = 1_762_600_000;

    assert_eq!(
        git_trace_observation(&trace, Some(now)),
        GitObservation::from_trace_at(text, Some(now))
    );
    assert!(!trace.exists());
}

/// NEGATIVE CONTROL: publishing directly into `destination`, or clearing it after a
/// failed attempt, removes `caller's-file` and fails this assertion.
#[test]
fn clone_stage_refuses_to_replace_a_concurrent_destination_file() {
    let root = aterm_tempfile::Builder::new()
        .prefix("agh-clone-stage-")
        .tempdir()
        .unwrap();
    let destination = root.path().join("destination");
    let stage = CloneStage::for_op(&GitOp::Clone {
        dest: destination.clone(),
        depth: None,
    })
    .unwrap()
    .unwrap();
    let stage_path = stage.path().to_path_buf();
    std::fs::write(stage.path().join("downloaded"), b"network bytes").unwrap();

    std::fs::create_dir(&destination).unwrap();
    std::fs::write(destination.join("caller's-file"), b"keep").unwrap();
    let error = stage
        .publish()
        .expect_err("a nonempty destination wins the publish race");

    assert!(error.why.contains("without replacing caller files"));
    assert_eq!(
        std::fs::read(destination.join("caller's-file")).unwrap(),
        b"keep"
    );
    assert!(!destination.join("downloaded").exists());
    assert!(!stage_path.exists(), "only the task-owned stage is cleaned");
}

#[test]
fn clone_stage_publishes_over_the_still_empty_admitted_destination() {
    let root = aterm_tempfile::Builder::new()
        .prefix("agh-clone-publish-")
        .tempdir()
        .unwrap();
    let destination = root.path().join("destination");
    std::fs::create_dir(&destination).unwrap();
    let stage = CloneStage::for_op(&GitOp::Clone {
        dest: destination.clone(),
        depth: None,
    })
    .unwrap()
    .unwrap();
    std::fs::write(stage.path().join("downloaded"), b"complete").unwrap();

    stage.publish().unwrap();

    assert_eq!(
        std::fs::read(destination.join("downloaded")).unwrap(),
        b"complete"
    );
}

/// NEGATIVE CONTROL: passing the caller destination to the retry cleanup removes its
/// file and fails this test.
#[test]
fn retry_cleanup_removes_only_the_private_clone_stage() {
    let root = aterm_tempfile::Builder::new()
        .prefix("agh-clone-retry-cleanup-")
        .tempdir()
        .unwrap();
    let destination = root.path().join("destination");
    let stage = CloneStage::for_op(&GitOp::Mirror {
        dest: destination.clone(),
    })
    .unwrap()
    .unwrap();
    std::fs::write(stage.path().join("partial"), b"partial").unwrap();
    std::fs::create_dir(&destination).unwrap();
    std::fs::write(destination.join("caller's-file"), b"keep").unwrap();

    clear_clone_stage(Some(stage.path())).unwrap();

    assert!(std::fs::read_dir(stage.path()).unwrap().next().is_none());
    assert_eq!(
        std::fs::read(destination.join("caller's-file")).unwrap(),
        b"keep"
    );
}
