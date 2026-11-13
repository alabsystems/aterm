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
    let mut builder = client(token).map_err(GithubError::other)?;
    if let Some(dirs) = agh::config::Dirs::from_env() {
        builder = builder.rate_book(agh::rate::RateBook::at(dirs.rate()));
    }
    read_with(&builder.build(), endpoint)
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
    if !super::github_rest::one_path(endpoint) {
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
#[path = "github_api_tests.rs"]
mod tests;
