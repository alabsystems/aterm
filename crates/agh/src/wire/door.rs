// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! THE credential door (design §6.6): the one file that spells the header's name, and the one
//! place that says where a credential may go. A header is built only for an origin that is
//! both this client's api or uploads origin and one of GitHub's two API hosts over TLS, so no
//! `Endpoints` can steer a token elsewhere; never for the web origin, the CDN or a proxy. Its
//! one caller outside tests is the client's pipeline (`client.rs`), once `Router::decide` has
//! answered `Send`. The census rule `github-auth-door` holds the header's name to this file.

use aterm_uds::secret::Secret;

use crate::identity::Credential;
use crate::wire::{Endpoints, Origin};

/// The header's name. The only literal of it in agh's source.
pub(crate) const AUTHORIZATION: &str = "Authorization";
/// The only hosts a credential is ever sent to, over `https` on port 443. The door spells them
/// itself rather than reading `Endpoints::github_com()`, so no edit outside this file widens it.
const CREDENTIAL_HOSTS: [&str; 2] = ["api.github.com", "uploads.github.com"];

/// Whether `name` is the credential header's name, ASCII case-insensitively.
#[must_use]
pub fn is_authorization(name: &str) -> bool {
    name.eq_ignore_ascii_case(AUTHORIZATION)
}

/// A built credential header. No `Clone`; `Debug` names the profile and fingerprint only.
pub struct AuthHeader {
    value: Secret,
    profile: String,
    fingerprint: String,
    origin: Origin,
}

/// Condition 2: GitHub's own API hosts over TLS, or, in a test or debug build, plain HTTP on
/// `127.0.0.1` (agh-fake). The `cfg!` is a constant, so a release build's loopback branch is
/// false.
fn credential_origin(origin: &Origin) -> bool {
    let github = origin.scheme() == "https"
        && origin.port() == 443
        && CREDENTIAL_HOSTS.contains(&origin.host());
    let loopback = cfg!(any(test, debug_assertions))
        && origin.scheme() == "http"
        && origin.host() == "127.0.0.1";
    github || loopback
}

impl AuthHeader {
    /// `Some` only when both hold:
    /// 1. `origin` equals `endpoints.api()` or `endpoints.uploads()` in scheme, host and port;
    /// 2. `origin` is `https`, port 443, on one of `CREDENTIAL_HOSTS`; or, where
    ///    `cfg!(any(test, debug_assertions))` holds, `http` on `127.0.0.1` at any port
    ///    (agh-fake). The `cfg!` is a constant, so the code is the same in every build and a
    ///    release build's loopback branch is false.
    ///
    /// The value is `Bearer ` followed by the token, held in a `Secret`, and the header keeps
    /// `origin`. Crate-private (plan §1, rule 9).
    pub(crate) fn for_origin(
        cred: &Credential,
        endpoints: &Endpoints,
        origin: &Origin,
    ) -> Option<AuthHeader> {
        let ours = origin == endpoints.api() || origin == endpoints.uploads();
        if !ours || !credential_origin(origin) {
            return None;
        }
        let token = cred.expose();
        let mut value = Vec::with_capacity(token.len() + 7);
        value.extend_from_slice(b"Bearer ");
        value.extend_from_slice(token);
        Some(AuthHeader {
            value: Secret::from_vec(value),
            profile: cred.profile().to_string(),
            fingerprint: cred.fingerprint().to_string(),
            origin: origin.clone(),
        })
    }

    /// `for_origin`, for the integration tests (plan §1, rule 9): its body is that one call.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn for_origin_for_tests(
        cred: &Credential,
        endpoints: &Endpoints,
        origin: &Origin,
    ) -> Option<AuthHeader> {
        AuthHeader::for_origin(cred, endpoints, origin)
    }

    /// The profile the credential is.
    #[must_use]
    pub fn profile(&self) -> &str {
        &self.profile
    }

    /// The token's fingerprint.
    #[must_use]
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// The origin it was built for. `HttpTransport` sends it to no other.
    #[must_use]
    pub fn origin(&self) -> &Origin {
        &self.origin
    }

    /// The header's name and value, for `HttpTransport` and `ScriptedTransport` alone.
    pub(crate) fn parts(&self) -> (&'static str, &Secret) {
        (AUTHORIZATION, &self.value)
    }
}

impl std::fmt::Debug for AuthHeader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AuthHeader({} {})", self.profile, self.fingerprint)
    }
}

// The cases only the crate can build: an `Endpoints` that names a host of its own (plan §4.8).
#[cfg(test)]
#[path = "door_tests.rs"]
mod tests;
