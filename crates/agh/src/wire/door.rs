// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Credential authorization header gate for GitHub API and uploads origins.

use aterm_uds::secret::Secret;

use crate::identity::Credential;
use crate::wire::{Endpoints, Origin};

/// The header's name.
pub(crate) const AUTHORIZATION: &str = "Authorization";
/// The only hosts a credential is ever sent to, over `https` on port 443.
const CREDENTIAL_HOSTS: [&str; 2] = ["api.github.com", "uploads.github.com"];

/// Whether `name` is the credential header's name, ASCII case-insensitively.
#[must_use]
pub const fn is_authorization(name: &str) -> bool {
    name.eq_ignore_ascii_case(AUTHORIZATION)
}

/// A built credential header.
pub struct AuthHeader {
    value: Secret,
    profile: String,
    fingerprint: String,
    origin: Origin,
}

/// Condition 2: GitHub's own API hosts over TLS, or, in a test or debug build, `http` on
/// `127.0.0.1`.
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
    /// `Some` only when `origin` equals `endpoints.api()` or `endpoints.uploads()` and passes `credential_origin`.
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

    /// `for_origin`, for the integration tests.
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

    /// The origin it was built for.
    #[must_use]
    pub const fn origin(&self) -> &Origin {
        &self.origin
    }

    /// The header's name and value, for `HttpTransport` and `ScriptedTransport` alone.
    pub(crate) const fn parts(&self) -> (&'static str, &Secret) {
        (AUTHORIZATION, &self.value)
    }
}

impl std::fmt::Debug for AuthHeader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AuthHeader({} {})", self.profile, self.fingerprint)
    }
}

#[cfg(test)]
#[path = "door_tests.rs"]
mod tests;
