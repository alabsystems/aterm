// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn failures_drive_recovery_without_parsing_messages() {
    for (status, code, kind, recovery) in [
        (422, 8002, Kind::Credentials, Recovery::Stop),
        (422, 9001, Kind::Verification, Recovery::Verify),
        (401, 10013, Kind::SessionExpired, Recovery::Reauthenticate),
        (401, 9001, Kind::Verification, Recovery::Verify),
        (429, 0, Kind::RateLimited, Recovery::RetryRead),
        (503, 0, Kind::Service, Recovery::RetryRead),
        (403, 0, Kind::Refused, Recovery::Stop),
        (422, 5003, Kind::ClientRejected, Recovery::Stop),
        (422, 2011, Kind::Refused, Recovery::Stop),
        (403, 9100, Kind::Scope, Recovery::Stop),
        (422, 9002, Kind::DeviceVerification, Recovery::Verify),
    ] {
        let error = Failure::response("/auth/v4", status, code, None);
        assert_eq!(error.kind, kind);
        assert_eq!(error.recovery(), recovery);
    }
    assert!(Failure::response("/auth/v4/refresh", 422, 10013, None).rejects_refresh_token());
    assert!(!Failure::response("/core/v4/addresses", 401, 401, None).rejects_refresh_token());
    for code in [8002, 9001, 9002, 5003, 9100] {
        assert!(!Failure::response("/auth/v4/refresh", 422, code, None).rejects_refresh_token());
    }
}

#[test]
fn diagnostics_do_not_include_request_paths_or_untrusted_headers() {
    let error = Failure::response(
        "/core/v4/keys?Email=private@example.test",
        429,
        0,
        Some("120"),
    );
    assert_eq!(error.retry_after_seconds, Some(120));
    assert!(!format!("{error:?} {error}").contains("private"));
    assert_eq!(
        Failure::response("/auth/v4", 429, 0, Some("secret")).retry_after_seconds,
        None
    );
}

#[test]
fn transport_failures_do_not_confuse_local_denial_with_transient_outages() {
    for (io_kind, kind, recovery) in [
        (
            std::io::ErrorKind::TimedOut,
            Kind::Timeout,
            Recovery::RetryRead,
        ),
        (
            std::io::ErrorKind::PermissionDenied,
            Kind::AccessDenied,
            Recovery::Stop,
        ),
        (
            std::io::ErrorKind::InvalidData,
            Kind::Protocol,
            Recovery::Stop,
        ),
    ] {
        let failure = Failure::transport(
            "/auth/v4",
            &aterm_http::SendError {
                phase: aterm_http::Phase::NotSent,
                error: aterm_http::Error::Io(std::io::Error::new(
                    io_kind,
                    "private transport detail",
                )),
            },
        );
        assert_eq!(failure.kind, kind);
        assert_eq!(failure.recovery(), recovery);
        assert_eq!(failure.phase, Some(aterm_http::Phase::NotSent));
        assert!(!failure.to_string().contains("private"));
    }
    assert_eq!(
        Failure::response("/auth/v4", 429, 0, Some("Sun, 01 Nov 2026 12:00:00 GMT")).recovery(),
        Recovery::Stop
    );
    assert_eq!(
        Failure::response("/auth/v4", 429, 0, Some("+60")).recovery(),
        Recovery::Stop
    );
    let blank_retry = Failure::response("/auth/v4", 429, 0, Some("   "));
    assert_eq!(blank_retry.retry_after_seconds, None);
    assert!(!blank_retry.retry_after_unparsed);
    assert_eq!(blank_retry.recovery(), Recovery::RetryRead);
}
