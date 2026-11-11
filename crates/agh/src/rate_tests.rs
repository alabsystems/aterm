// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use super::*;

#[test]
fn rate_headers_and_cli_flags_reject_signed_numbers() {
    let headers = vec![
        ("x-ratelimit-limit".to_string(), "+5000".to_string()),
        ("x-ratelimit-remaining".to_string(), "4999".to_string()),
        ("x-ratelimit-reset".to_string(), "1700000000".to_string()),
    ];
    assert!(RateSnapshot::from_headers(&headers).is_none());
    let ok_headers = vec![
        ("x-ratelimit-limit".to_string(), "5000".to_string()),
        ("x-ratelimit-remaining".to_string(), "4999".to_string()),
        ("x-ratelimit-reset".to_string(), "1700000000".to_string()),
        ("x-ratelimit-used".to_string(), "+1".to_string()),
    ];
    let snap = RateSnapshot::from_headers(&ok_headers).expect("valid rate headers");
    assert_eq!(snap.used, 1);

    assert!(crate::cli::args::parse(&["ls", "o/r", "--limit", "+5"].map(String::from)).is_err());
    assert!(
        crate::cli::args::parse(&["ls", "o/r", "--max-pages", "+5"].map(String::from)).is_err()
    );
}
