// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::host::fake::FakeHost;

#[test]
fn an_answered_public_404_is_not_polled_again() {
    let host = FakeHost::new();
    let slug = Slug::new("public", "program");
    assert_eq!(probe_served(&host, &slug, "v1", "asset"), Probe::Absent);
    assert_eq!(host.calls(), ["probe public/program v1 asset"]);
}
