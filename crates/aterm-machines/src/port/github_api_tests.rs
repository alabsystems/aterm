// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use agh::wire::scripted::ScriptedTransport;

const TOKEN: &[u8] = b"fixture_development_token_only";

fn scripted(token: &[u8]) -> (agh::Github, Arc<ScriptedTransport>) {
    let transport = Arc::new(ScriptedTransport::new());
    let secret = Secret::new(token).unwrap();
    let client = client(&secret)
        .unwrap()
        .transport(transport.clone())
        .fx(Arc::new(agh::Fx::simulated(0)))
        .build();
    (client, transport)
}

#[test]
fn agh_identity_is_private_and_the_authenticated_read_keeps_pagination_headers() {
    let source = AdmittedToken(Secret::new(TOKEN).unwrap());
    assert!(source.var_os("GH_TOKEN").is_none());
    assert!(source.var_os("GITHUB_TOKEN").is_none());
    assert!(source.var_os("HTTPS_PROXY").is_none());
    let (client, transport) = scripted(TOKEN);
    transport.answer(
        200,
        &[("Link", "one"), ("Link", "two")],
        br#"{"login":"alice"}"#,
    );
    let reply = read_with(&client, "/user").unwrap();
    assert_eq!(reply.status, 200);
    assert_eq!(
        reply.headers,
        [("link".into(), "one".into()), ("link".into(), "two".into())]
    );
    let requests = transport.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].auth_profile.as_deref(), Some(PROFILE));
    assert!(requests[0].auth_fingerprint.is_some());
    assert!(!format!("{requests:?}").contains(std::str::from_utf8(TOKEN).unwrap()));
    assert_eq!(
        client.redactor().apply(std::str::from_utf8(TOKEN).unwrap()),
        "[redacted]"
    );
    for endpoint in [
        "/user/repos?visibility=all&per_page=30&page=1",
        "/users/team",
        "/orgs/team/repos?type=all&page=1",
    ] {
        transport.answer(200, &[], b"[]");
        read_with(&client, endpoint).unwrap();
    }
    assert_eq!(transport.requests().len(), 4);
    assert!(
        transport
            .requests()
            .iter()
            .all(|request| request.auth_profile.as_deref() == Some(PROFILE))
    );
}

#[test]
fn malformed_or_refresh_credentials_fail_before_any_network_or_anonymous_fallback() {
    for token in [
        b"short".as_slice(),
        b"ghr_123456789012345678901234567890", // gitleaks:allow
        b"fixture development token only",
    ] {
        let (client, transport) = scripted(token);
        assert!(read_with(&client, "/user").is_err());
        assert!(transport.requests().is_empty());
    }
}

#[test]
fn authentication_failure_and_redirects_are_not_retried_or_sent_anonymously() {
    for status in [401, 302] {
        let (client, transport) = scripted(TOKEN);
        transport.answer(
            status,
            &[("Location", "https://example.invalid/next")],
            TOKEN,
        );
        match read_with(&client, "/user") {
            Ok(refused) => {
                assert_eq!(status, 401, "only a refused token is an answer");
                assert_eq!(refused.status, 401);
                assert!(refused.headers.is_empty() && refused.body.is_empty());
            }
            Err(error) => {
                assert_eq!(status, 302);
                assert!(!error.why.contains(std::str::from_utf8(TOKEN).unwrap()));
            }
        }
        let requests = transport.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].auth_profile.as_deref(), Some(PROFILE));
    }
}

/// A read that reached no answer (the network, a proxy, a deadline) is told
/// apart from one GitHub refused, by agh's kind and never by its words: System
/// State says "GitHub can't be reached" of the one and not of the other.
#[test]
fn a_read_that_reached_no_answer_is_unreached_and_a_refused_token_is_not() {
    for kind in [
        agh::wire::WireKind::Dns,
        agh::wire::WireKind::Connect,
        agh::wire::WireKind::Timeout,
    ] {
        let (client, transport) = scripted(TOKEN);
        transport.push(agh::wire::scripted::Scripted::Fail(agh::wire::WireError {
            phase: agh::wire::Phase::NotSent,
            kind,
            detail: std::str::from_utf8(TOKEN).unwrap().to_owned(),
        }));
        let error = read_with(&client, "/user").unwrap_err();
        assert_eq!(error.failure, GithubFailure::Unreached, "{kind:?}");
        assert!(!error.why.contains(std::str::from_utf8(TOKEN).unwrap()));
    }
    assert_eq!(
        failure(agh::Kind::Unauthenticated),
        GithubFailure::Unauthenticated
    );
    assert_eq!(failure(agh::Kind::Server), GithubFailure::Other);
}

#[test]
fn incomplete_or_oversized_answers_cannot_become_repository_inventory() {
    let (client, transport) = scripted(TOKEN);
    transport.answer(200, &[], &vec![b'x'; RESPONSE_MAX as usize + 1]);
    assert!(read_with(&client, "/user").is_err());
    assert_eq!(transport.requests().len(), 1);
    transport.push(agh::wire::scripted::Scripted::Fail(agh::wire::WireError {
        phase: agh::wire::Phase::Answered,
        kind: agh::wire::WireKind::Truncated,
        detail: std::str::from_utf8(TOKEN).unwrap().to_owned(),
    }));
    let error = read_with(&client, "/user").unwrap_err();
    assert!(!error.why.contains(std::str::from_utf8(TOKEN).unwrap()));
    assert_eq!(transport.requests().len(), 2);
    for endpoint in [
        "https://example.invalid/user",
        "//example.invalid/user",
        "/user\n",
        "/user name",
        "/user#frag",
        "/\\example.invalid/user",
        "/user/\u{e9}",
    ] {
        assert!(read_with(&client, endpoint).is_err());
    }
    assert_eq!(transport.requests().len(), 2);
}
