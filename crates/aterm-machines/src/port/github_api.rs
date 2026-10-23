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

use super::{GithubResponse, Secret};

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

pub(super) fn read(token: &Secret, endpoint: &str) -> Result<GithubResponse, String> {
    read_with(&client(token)?.build(), endpoint)
}

fn read_with(client: &agh::Github, endpoint: &str) -> Result<GithubResponse, String> {
    if !endpoint.starts_with('/')
        || endpoint.starts_with("//")
        || endpoint.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err("invalid GitHub API endpoint".into());
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
    let answer = client
        .call(&call)
        .map_err(|_| "GitHub rejected the credential or repository request (output withheld)")?;
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
            let why = read_with(&client, "/user").unwrap_err();
            assert!(!why.contains(std::str::from_utf8(TOKEN).unwrap()));
            let requests = transport.requests();
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0].auth_profile.as_deref(), Some(PROFILE));
        }
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
        let why = read_with(&client, "/user").unwrap_err();
        assert!(!why.contains(std::str::from_utf8(TOKEN).unwrap()));
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
