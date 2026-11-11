// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn head_number_rejects_signed_tag_suffix() {
    let rel = |tag: &str| crate::host::Release {
        published_at: None,
        tag: tag.to_string(),
        draft: false,
        prerelease: false,
        assets: Vec::new(),
    };
    assert_eq!(
        head_number(&[rel("atpkg-index-5"), rel("atpkg-index-+99")]),
        Some(5)
    );
}

#[test]
fn a_transport_owned_head_download_has_exactly_one_retry_owner() {
    let mut authenticated_transport_calls = 0;
    let result = download_head_with(true, std::time::Duration::ZERO, || {
        authenticated_transport_calls += 1;
        Err(HostError::Unreachable("transient".into()))
    });
    assert!(matches!(result, Err(HostError::Unreachable(_))));
    assert_eq!(
        authenticated_transport_calls, 1,
        "an authenticated API plus anonymous CDN composite owns one transport budget"
    );

    let mut model_calls = 0;
    let result = download_head_with(false, std::time::Duration::ZERO, || {
        model_calls += 1;
        Err(HostError::Unreachable("transient".into()))
    });
    assert!(matches!(result, Err(HostError::Unreachable(_))));
    assert_eq!(
        model_calls, 3,
        "a model with no transport budget retains its bound"
    );
}
