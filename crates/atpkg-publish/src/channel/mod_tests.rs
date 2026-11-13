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

#[test]
fn a_written_asset_is_read_until_the_route_serves_it() {
    let host = FakeHost::new();
    let slug = Slug::new("public", "program");
    host.put_asset(&slug, "v1", "asset", b"bytes".to_vec());
    host.lag_public(&slug, "v1", "asset", 4);
    assert_eq!(await_served(&host, &slug, "v1", "asset"), Probe::Present);
    assert_eq!(host.calls().len(), 5, "{:?}", host.calls());
}

#[test]
fn an_unknown_answer_is_not_read_again_and_absence_is_bounded() {
    let host = FakeHost::new();
    let mut reads = 0;
    let answer = await_written(&host, || {
        reads += 1;
        Probe::Unknown("no route".into())
    });
    assert_eq!(answer, Probe::Unknown("no route".into()));
    assert_eq!(reads, 1, "the transport already spent its retries");
    let slug = Slug::new("public", "program");
    assert_eq!(await_served(&host, &slug, "v1", "never"), Probe::Absent);
    assert_eq!(
        host.calls().len(),
        usize::try_from(SETTLE_READS).expect("small")
    );
}
