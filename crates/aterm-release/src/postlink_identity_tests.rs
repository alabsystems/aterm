// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

/// A previous hop's stamp cannot answer for the current reservation.
#[test]
fn a_runtime_identity_names_this_stamp_once() {
    let stamp = aterm_stamp::Stamp {
        app_version: "1.2.0",
        git_commit: "123456789abc",
        build_time: "2026-11-07T00:00:00Z",
        ..aterm_stamp::Stamp::UNSTAMPED
    };
    let report = "version: 1.2.0 (123456789abc, built 2026-11-07T00:00:00Z)\n";
    super::identity_report(&stamp, report).unwrap();
    assert!(
        super::identity_report(&stamp, &report.replace("123456789abc", "aaaaaaaaaaaa")).is_err()
    );
    assert!(super::identity_report(&stamp, &report.replace("2026-11-07", "2026-11-06")).is_err());
    assert!(super::identity_report(&stamp, &report.repeat(2)).is_err());
}
