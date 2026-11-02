// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Canonical Claude OAuth profile verification. Native `auth status` alone is
//! local metadata; this module requires the provider to accept the exact token.

use super::super::super::{accounts, profile};
use aterm_http::{Client, Method, ProxyMode, Trust};
use aterm_json::Value;
use aterm_uds::secret::Secret;
use std::time::Duration;

const PROFILE_URL: &str = "https://api.anthropic.com/api/oauth/profile";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaudeProfile {
    pub email: String,
    pub principal: [u8; 32],
}

fn profile_value(value: &Value) -> Result<ClaudeProfile, String> {
    let account = &value["account"];
    let uuid = account["uuid"]
        .as_str()
        .filter(|id| !id.is_empty() && id.len() <= 1024 && !id.chars().any(char::is_control))
        .ok_or("provider profile has no account identity")?;
    let email = account["email"]
        .as_str()
        .filter(|email| accounts::email_refusal(email).is_none())
        .ok_or("provider profile has no valid email")?;
    if value["organization"]["uuid"]
        .as_str()
        .is_none_or(|id| id.is_empty() || id.len() > 1024 || id.chars().any(char::is_control))
    {
        return Err("provider profile has no organization identity".into());
    }
    Ok(ClaudeProfile {
        email: email.to_owned(),
        principal: profile::principal_id(uuid).ok_or("invalid provider account identity")?,
    })
}

/// One bounded request to the vendor's canonical OAuth profile endpoint.
/// Redirects are not followed by `aterm-http`.
pub fn read_claude_profile(token: &str, wait: Duration) -> Result<ClaudeProfile, String> {
    if wait.is_zero() || !token_valid(token) {
        return Err("invalid OAuth profile request".into());
    }
    let mut header = b"Bearer ".to_vec();
    header.extend_from_slice(token.as_bytes());
    let response = Client::new(Trust::PlatformVerifier, ProxyMode::Environment, wait)
        .request(Method::Get, PROFILE_URL)
        .secret_header("Authorization", Secret::from_vec(header))
        .header("User-Agent", "claude-cli/aterm")
        .header("Content-Type", "application/json")
        .header("Cache-Control", "no-cache")
        .limit(256 * 1024)
        .backstop(wait)
        .send(&[])
        .map_err(|_| "provider profile request did not complete")?;
    if response.status() != 200 {
        return Err("provider refused the staged OAuth token".into());
    }
    let value = aterm_json::from_slice(response.body())
        .map_err(|_| "provider profile response is unreadable")?;
    profile_value(&value)
}

fn token_valid(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= 64 * 1024
        && token.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
}

fn access_token(bytes: &[u8], now: i64) -> Result<Secret, String> {
    if bytes.len() > 2 * 1024 * 1024 || now <= 0 {
        return Err("invalid staged credential size or observation time".into());
    }
    let value: Value =
        aterm_json::from_slice(bytes).map_err(|_| "staged Claude credential is unreadable")?;
    let token = (|| {
        let oauth = &value["claudeAiOauth"];
        let expiry = oauth["expiresAt"]
            .as_u64()
            .ok_or("staged Claude token has no expiry")?;
        if expiry <= (now as u64).saturating_mul(1000) {
            return Err("staged Claude token is expired".into());
        }
        let token = oauth["accessToken"]
            .as_str()
            .filter(|token| token_valid(token))
            .ok_or("staged Claude OAuth token is invalid")?;
        Ok(Secret::new(token.as_bytes()))
    })();
    super::wipe_json(value);
    token
}

pub(super) fn verify_credentials(
    bytes: &[u8],
    email: &str,
    now: i64,
    read: impl FnOnce(&str) -> Result<ClaudeProfile, String>,
) -> Result<ClaudeProfile, String> {
    let token = access_token(bytes, now)?;
    let token = std::str::from_utf8(token.expose()).map_err(|_| "staged token is not text")?;
    let profile = read(token)?;
    if !accounts::email_matches(&profile.email, email) {
        return Err("provider profile names a different approved account".into());
    }
    Ok(profile)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_json::json;
    use std::cell::Cell;

    #[test]
    fn shared_claude_requires_fresh_provider_identity_not_local_status_metadata() {
        let bytes = aterm_json::to_vec(
            &json!({"claudeAiOauth":{"accessToken":"synthetic", "expiresAt":4102444800000_u64}}),
        )
        .unwrap();
        assert!(
            verify_credentials(&bytes, "approved@example.test", 1_800_000_000, |_| Err(
                "rejected by provider".into()
            ))
            .is_err()
        );
        let wrong = profile_value(&json!({"account":{"uuid":"id", "email":"other@example.test"},"organization":{"uuid":"org"}})).unwrap();
        assert!(
            profile_value(&json!({"account":{"uuid":"id", "email":"approved@example.test"},"organization":{"uuid":"org\n"}})).is_err()
        );
        assert!(
            verify_credentials(&bytes, "approved@example.test", 1_800_000_000, |_| Ok(
                wrong
            ))
            .is_err()
        );
        let valid = profile_value(&json!({"account":{"uuid":"id", "email":"approved@example.test"},"organization":{"uuid":"org"}})).unwrap();
        let got = verify_credentials(&bytes, "APPROVED@example.test", 1_800_000_000, |_| {
            Ok(valid)
        })
        .unwrap();
        assert_eq!(got.principal, profile::principal_id("id").unwrap());
    }

    #[test]
    fn shared_claude_missing_expired_or_header_invalid_token_never_reaches_provider() {
        for oauth in [
            json!({}),
            json!({"accessToken":"synthetic", "expiresAt":1}),
            json!({"accessToken":"synthetic\r\nheader", "expiresAt":4102444800000_u64}),
        ] {
            let called = Cell::new(false);
            let bytes = aterm_json::to_vec(&json!({"claudeAiOauth":oauth})).unwrap();
            assert!(
                verify_credentials(&bytes, "approved@example.test", 1_800_000_000, |_| {
                    called.set(true);
                    Err("must not execute".into())
                })
                .is_err()
            );
            assert!(!called.get());
        }
    }
}
