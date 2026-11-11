// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use super::*;
use crate::error::Sub;

#[test]
fn a_tape_cut_short_by_a_write_error_takes_its_bodies_with_it() {
    let dir = crate::test_fixtures::scratch_dir("tape");
    let path = dir.path().join("t.atape");
    let argv = ["release".to_string(), "download".to_string()];
    let header = TapeHeader::for_run(&argv, &crate::wire::Endpoints::github_com());
    let sink = TapeSink::create(&path, &header).expect("a sink");
    let stored = sink
        .store_body(0, &mut &b"the bytes of a body"[..], 1 << 20)
        .expect("stored");
    let body = sidecar_dir(&path).join(&stored.hex);
    assert!(body.exists());
    // The tape's handle made read-only: the next append fails to write, as on a full disk.
    let read_only = std::fs::File::open(&path).expect("the tape, read-only");
    sink.state
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .file = read_only;
    sink.append(&EffectRecord::Clock(1));
    let refused = sink.finish().expect_err("a write error");
    assert_eq!(refused.sub, Sub::Local);
    assert!(path.exists(), "the partial tape is kept");
    assert!(!body.exists(), "its body is removed");
    assert!(!sidecar_dir(&path).exists());
}
