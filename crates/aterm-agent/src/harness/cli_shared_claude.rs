// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Canonical Claude OAuth profile verification. Native `auth status` alone is
//! local metadata; this module requires the provider to accept the exact token.

use super::super::super::{accounts, profile};
use super::ObservationError;
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
pub fn read_claude_profile(token: &str, wait: Duration) -> Result<ClaudeProfile, ObservationError> {
    if !token_valid(token) {
        return Err(ObservationError::RecoveryRequired(
            "invalid OAuth profile request".into(),
        ));
    }
    if wait.is_zero() {
        return Err(ObservationError::Unavailable(
            "OAuth profile deadline expired".into(),
        ));
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
        .map_err(|_| {
            ObservationError::Unavailable("provider profile request did not complete".into())
        })?;
    profile_response(response.status(), response.body())
}

fn profile_response(status: u16, body: &[u8]) -> Result<ClaudeProfile, ObservationError> {
    if status == 401 {
        return Err(ObservationError::RecoveryRequired(
            "provider rejected the staged OAuth token".into(),
        ));
    }
    if status != 200 {
        return Err(ObservationError::Unavailable(format!(
            "provider profile verification unavailable (HTTP {status})"
        )));
    }
    let value = aterm_json::from_slice(body).map_err(|_| {
        ObservationError::Unavailable("provider profile response is unreadable".into())
    })?;
    profile_value(&value).map_err(ObservationError::Unavailable)
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
    read: impl FnOnce(&str) -> Result<ClaudeProfile, ObservationError>,
) -> Result<ClaudeProfile, ObservationError> {
    let token = access_token(bytes, now).map_err(ObservationError::RecoveryRequired)?;
    let token = std::str::from_utf8(token.expose())
        .map_err(|_| ObservationError::RecoveryRequired("staged token is not text".into()))?;
    let profile = read(token)?;
    if !accounts::email_matches(&profile.email, email) {
        return Err(ObservationError::RecoveryRequired(
            "provider profile names a different approved account".into(),
        ));
    }
    Ok(profile)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_json::json;
    use std::cell::Cell;

    #[test]
    fn canonical_profile_outage_is_distinct_from_a_rejected_token() {
        let valid = br#"{"account":{"uuid":"id","email":"approved@example.test"},"organization":{"uuid":"org"}}"#;
        assert!(profile_response(200, valid).is_ok());
        assert!(matches!(
            profile_response(401, valid),
            Err(ObservationError::RecoveryRequired(_))
        ));
        for status in [201, 302, 400, 403, 404, 408, 429, 500, 502, 503] {
            assert!(
                matches!(
                    profile_response(status, valid),
                    Err(ObservationError::Unavailable(_))
                ),
                "status {status}"
            );
        }
        for body in [b"not json".as_slice(), b"{}".as_slice()] {
            assert!(matches!(
                profile_response(200, body),
                Err(ObservationError::Unavailable(_))
            ));
        }
        assert!(matches!(
            read_claude_profile("synthetic", Duration::ZERO),
            Err(ObservationError::Unavailable(_))
        ));
        assert!(matches!(
            read_claude_profile("bad\r\nheader", Duration::from_secs(1)),
            Err(ObservationError::RecoveryRequired(_))
        ));
    }

    #[test]
    fn shared_claude_requires_fresh_provider_identity_not_local_status_metadata() {
        let bytes = aterm_json::to_vec(
            &json!({"claudeAiOauth":{"accessToken":"synthetic", "expiresAt":4102444800000_u64}}),
        )
        .unwrap();
        assert!(
            verify_credentials(&bytes, "approved@example.test", 1_800_000_000, |_| Err(
                ObservationError::RecoveryRequired("rejected by provider".into())
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
                    Err(ObservationError::Unavailable("must not execute".into()))
                })
                .is_err()
            );
            assert!(!called.get());
        }
    }
}
