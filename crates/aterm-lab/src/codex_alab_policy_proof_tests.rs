// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_refusal_requires_its_completed_past_server_write() {
    let row = |status, id, request, written| {
        J::obj([
            ("tag", J::s("policy:refused")),
            ("status", J::Int(status)),
            ("request_id", J::Int(id)),
            ("mono_ms", J::Int(request)),
            ("reply_ms", J::Int(written)),
        ])
    };
    assert!(refusal_written(&[row(400, 1, 5, 10)], "policy:refused", 11).is_some());
    for bad in [
        row(200, 1, 5, 10),
        row(400, 0, 5, 10),
        row(400, 1, 10, 5),
        row(400, 1, 5, 12),
    ] {
        assert!(refusal_written(&[bad], "policy:refused", 11).is_none());
    }
    assert!(refusal_written(&[row(400, 1, 5, 10)], "policy:other", 11).is_none());
}
