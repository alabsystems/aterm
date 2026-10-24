// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Authenticated `agh` read client for an admitted initial-setup development token.

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt as _;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use agh::identity::{EnvSource, ProcessEnv};
use agh::policy::{RouteRequest, RouteTarget, Router};
use agh::retry::RetryPolicy;
use agh::wire::http::HttpTransport;
use agh::{Call, GithubBuilder};

use super::{GithubError, GithubFailure, GithubResponse, Secret};

const PROFILE: &str = "initial-setup-development";
const TOKEN_SOURCE: &str = "ATERM_INITIAL_SETUP_ADMITTED_TOKEN";
const RESPONSE_MAX: u64 = 1 << 20;
const CONFIG: &str = "[profiles.initial-setup-development]\n\
                      env = 'ATERM_INITIAL_SETUP_ADMITTED_TOKEN'\n\
                      tier = 'dev'\n\
                      [default]\n\
                      read = ['initial-setup-development']\n";

struct AdmittedToken(Secret);

impl EnvSource for AdmittedToken {
    fn var_os(&self, name: &str) -> Option<OsString> {
        (name == TOKEN_SOURCE).then(|| OsString::from_vec(self.0.bytes().to_vec()))
    }
}

fn client(token: &Secret) -> Result<GithubBuilder, String> {
    let config =
        agh::config::parse(CONFIG).map_err(|_| "the initial GitHub routing profile is invalid")?;
    let validated = agh::config::validate(&config, Path::new("/"), &|_| None)
        .map_err(|_| "the initial GitHub routing profile is invalid")?;
    let identity = AdmittedToken(
        Secret::new(token.bytes()).map_err(|_| "the development GitHub token is too long")?,
    );
    Ok(GithubBuilder::new(Router::from_validated(validated))
        .env(Arc::new(identity))
        .transport(Arc::new(HttpTransport::new(Arc::new(ProcessEnv))))
        .caller("aterm-initial-setup")
        .retry(RetryPolicy::NONE)
        .backstop(Some(Duration::from_secs(115))))
}

pub(super) fn read(token: &Secret, endpoint: &str) -> Result<GithubResponse, GithubError> {
    read_with(
        &client(token).map_err(GithubError::other)?.build(),
        endpoint,
    )
}

/// What kind of failure agh's classified error is, for a reader that words it for a
/// person: no answer reached (`CouldNotRun`), a token that does not sign in
/// (`Unauthenticated`), or anything else.
fn failure(kind: agh::Kind) -> GithubFailure {
    match kind {
        agh::Kind::CouldNotRun => GithubFailure::Unreached,
        agh::Kind::Unauthenticated => GithubFailure::Unauthenticated,
        _ => GithubFailure::Other,
    }
}

fn read_with(client: &agh::Github, endpoint: &str) -> Result<GithubResponse, GithubError> {
    if !endpoint.starts_with('/')
        || endpoint.starts_with("//")
        || endpoint.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(GithubError::other("invalid GitHub API endpoint"));
    }
    let target = if endpoint == "/user" {
        RouteTarget::Identity
    } else {
        RouteTarget::Unscoped
    };
    let call = Call::get(
        endpoint,
        RouteRequest::read(target).with_as(Some(PROFILE.into())),
    )
    .limit(RESPONSE_MAX)
    .probe_404(false)
    .follow_moved(false);
    let answer = match client.call(&call) {
        Ok(answer) => answer,
        Err(error) if error.http.as_ref().is_some_and(|http| http.status == 401) => {
            return Ok(GithubResponse {
                status: 401,
                headers: Vec::new(),
                body: Vec::new(),
            });
        }
        Err(error) => {
            return Err(GithubError {
                failure: failure(error.kind),
                why: "GitHub rejected the credential or repository request (output withheld)"
                    .into(),
            });
        }
    };
    Ok(GithubResponse {
        status: answer.status,
        headers: answer.headers,
        body: answer.body,
    })
}

#[cfg(test)]
mod tests {
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
        ] {
            assert!(read_with(&client, endpoint).is_err());
        }
        assert_eq!(transport.requests().len(), 2);
    }
}
