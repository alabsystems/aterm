// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded GitHub REST client over `aterm-http`.

use std::time::Duration;

use aterm_http::{Client, ProxyMode, Trust};

use super::{GithubResponse, Secret};

/// The API origin every live request goes to.
pub(super) const API: &str = "https://api.github.com";

/// The bound on one answer's body: a key listing page of 100 is a few tens of KiB.
const RESPONSE_MAX: usize = 1 << 20;

/// The bound on one request, connect to last body byte.
const BUDGET: Duration = Duration::from_secs(60);

/// The REST version every request names (GitHub's `X-GitHub-Api-Version`).
const API_VERSION: &str = "2022-11-28";

/// A REST method the controller sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestMethod {
    /// A read.
    Get,
    /// A creation (`POST /user/keys`).
    Post,
    /// A deletion (`DELETE /user/keys/{id}`).
    Delete,
}

impl RestMethod {
    /// The method's token on the request line.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Delete => "DELETE",
        }
    }

    const fn http(self) -> aterm_http::Method {
        match self {
            Self::Get => aterm_http::Method::Get,
            Self::Post => aterm_http::Method::Post,
            Self::Delete => aterm_http::Method::Delete,
        }
    }
}

impl std::fmt::Display for RestMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The live client: the operating system's trust store, and the environment's proxy.
pub(super) fn live_client() -> Client {
    Client::new(Trust::PlatformVerifier, ProxyMode::Environment, BUDGET)
}

/// Whether `endpoint` is one path (and query) on the API origin it is appended to: it starts
/// with one `/` and holds printable ASCII alone, with no `#` and no `\`, which a URL parser
/// may read as `/` and so as the start of another host.
pub(super) fn one_path(endpoint: &str) -> bool {
    endpoint.starts_with('/')
        && !endpoint.starts_with("//")
        && endpoint
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && !matches!(byte, b'#' | b'\\'))
}

/// Send `method endpoint` to `origin` carrying `token`, with `body` as JSON when it is given.
/// # Errors
/// An endpoint that is not one absolute path, a request that failed or was cut short, an
/// answer over the bound, or a redirect. No error carries the token.
pub(super) fn send(
    client: &Client,
    origin: &str,
    token: &Secret,
    method: RestMethod,
    endpoint: &str,
    body: Option<&[u8]>,
) -> Result<GithubResponse, String> {
    if !one_path(endpoint) {
        return Err("invalid GitHub API endpoint".into());
    }
    let mut header = Vec::with_capacity(7 + token.bytes().len());
    header.extend_from_slice(b"Bearer ");
    header.extend_from_slice(token.bytes());
    let authorization = aterm_uds::secret::Secret::from_vec(header);
    let mut request = client
        .request(method.http(), &format!("{origin}{endpoint}"))
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", API_VERSION)
        .header("User-Agent", "aterm-machines")
        .secret_header("Authorization", authorization)
        .limit(RESPONSE_MAX);
    if body.is_some() {
        request = request.header("Content-Type", "application/json");
    }
    let answer = request
        .send(body.unwrap_or_default())
        .map_err(|error| format!("GitHub {method} {endpoint}: {error}"))?;
    let status = answer.status();
    if (300..400).contains(&status) {
        return Err(format!(
            "GitHub {method} {endpoint} answered HTTP {status}, a redirect, which is never \
             followed with the machine user's token"
        ));
    }
    Ok(GithubResponse {
        status,
        headers: answer
            .headers()
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
            .collect(),
        body: answer.body().to_vec(),
    })
}

/// A loopback GitHub for this crate's tests.
#[cfg(test)]
#[path = "github_rest_fixture_tests.rs"]
pub(crate) mod fixture;

#[cfg(test)]
#[path = "github_rest_tests.rs"]
mod tests;
