// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Google account proof from the canonical OpenID Connect userinfo endpoint.

use aterm_http::{Client, Method, ProxyMode, Trust};
use aterm_json::Value;
use aterm_uds::secret::Secret;
use std::time::Duration;

const USERINFO: &str = "https://openidconnect.googleapis.com/v1/userinfo";

/// Provider-confirmed identity. Cached CLI metadata is never an input to this proof.
pub struct GoogleProfile {
    pub email: String,
    pub principal: [u8; 32],
}

fn profile(value: &Value, expected: &str) -> Result<GoogleProfile, &'static str> {
    let sub = value["sub"]
        .as_str()
        .filter(|s| {
            !s.is_empty() && s.len() <= 255 && s.bytes().all(|b| (0x21..=0x7e).contains(&b))
        })
        .ok_or("Google profile has no valid subject")?;
    let email = value["email"]
        .as_str()
        .ok_or("Google profile has no email")?;
    if value["email_verified"].as_bool() != Some(true)
        || super::accounts::email_refusal(email).is_some()
        || !email.eq_ignore_ascii_case(expected)
    {
        return Err("Google profile does not verify the configured account");
    }
    let mut digest = aterm_digest::Sha256::new();
    digest.update(b"aterm.google.subject.v1\0");
    digest.update(sub.as_bytes());
    Ok(GoogleProfile {
        email: email.to_owned(),
        principal: digest.finalize(),
    })
}

/// Accept only a successful canonical response with a verified, exact configured email.
fn response(status: u16, body: &[u8], expected: &str) -> Result<GoogleProfile, &'static str> {
    if status != 200 || body.len() > 64 * 1024 {
        return Err("Google refused the account verification request");
    }
    let value = aterm_json::from_slice(body).map_err(|_| "Google profile is unreadable")?;
    profile(&value, expected)
}

/// Bounded bearer request. Redirects are not followed; tokens never enter a URL or diagnostic.
pub fn read(token: &str, expected: &str, wait: Duration) -> Result<GoogleProfile, &'static str> {
    if wait.is_zero()
        || token.is_empty()
        || token.len() > 64 * 1024
        || !token.bytes().all(|b| (0x21..=0x7e).contains(&b))
    {
        return Err("Invalid Google account verification request");
    }
    let mut header = b"Bearer ".to_vec();
    header.extend_from_slice(token.as_bytes());
    let reply = Client::new(Trust::PlatformVerifier, ProxyMode::Environment, wait)
        .request(Method::Get, USERINFO)
        .secret_header("Authorization", Secret::from_vec(header))
        .header("Cache-Control", "no-cache")
        .limit(64 * 1024)
        .backstop(wait)
        .send(&[])
        .map_err(|_| "Google account verification did not complete")?;
    response(reply.status(), reply.body(), expected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_google_identity_requires_verified_exact_account() {
        let valid = br#"{"sub":"1234","email":"Owner+cli@gmail.com","email_verified":true}"#;
        let first = response(200, valid, "owner+cli@gmail.com").unwrap();
        assert!(response(200, valid, "owner@gmail.com").is_err());
        assert!(response(302, valid, "owner+cli@gmail.com").is_err());
        assert!(response(401, valid, "owner+cli@gmail.com").is_err());
        for invalid in [
            br#"{"sub":"1234","email":"owner+cli@gmail.com","email_verified":false}"#.as_slice(),
            br#"{"sub":"1234","email":"owner+cli@gmail.com","email_verified":"true"}"#,
            br#"{"sub":"","email":"owner+cli@gmail.com","email_verified":true}"#,
        ] {
            assert!(response(200, invalid, "owner+cli@gmail.com").is_err());
        }
        let changed = br#"{"sub":"5678","email":"owner+cli@gmail.com","email_verified":true}"#;
        assert_ne!(
            first.principal,
            response(200, changed, "owner+cli@gmail.com")
                .unwrap()
                .principal
        );
    }
}
