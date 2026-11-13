// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// One offer per address per [`EVERY`], read on an injected clock; another address is its
/// own.
#[test]
fn an_address_is_offered_the_record_at_most_once_a_period() {
    let start = Instant::now();
    let (one, two): (SocketAddr, SocketAddr) = (
        "192.0.2.1:7100".parse().unwrap(),
        "192.0.2.2:7100".parse().unwrap(),
    );
    let mut offered = BTreeMap::new();
    assert!(due(&mut offered, one, start));
    assert!(!due(
        &mut offered,
        one,
        start + EVERY - Duration::from_secs(1)
    ));
    assert!(due(&mut offered, two, start + Duration::from_secs(1)));
    assert!(due(&mut offered, one, start + EVERY));
    assert!(!due(
        &mut offered,
        one,
        start + EVERY + Duration::from_secs(1)
    ));
}
