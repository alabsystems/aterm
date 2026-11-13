// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Canonical Claude OAuth profile verification. Native `auth status` alone is
//! local metadata; this module requires the provider to accept the exact token.

use super::super::super::{accounts, profile};
use super::ObservationError;
use aterm_http::{Client, Method, ProxyMode, Trust};
use aterm_json::Value;
use aterm_uds::secret::Secret;
use std::time::{Duration, Instant};

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
    read_started(
        token,
        wait,
        Instant::now(),
        PROFILE_URL,
        ProxyMode::Environment,
    )
}

fn read_started(
    token: &str,
    wait: Duration,
    started: Instant,
    endpoint: &str,
    proxy: ProxyMode,
) -> Result<ClaudeProfile, ObservationError> {
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
    let by = started + wait;
    let left = by.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(ObservationError::Unavailable(
            "OAuth profile deadline expired".into(),
        ));
    }
    let response = Client::new(Trust::PlatformVerifier, proxy, left)
        .request(Method::Get, endpoint)
        .secret_header("Authorization", Secret::from_vec(header))
        .header("User-Agent", "claude-cli/aterm")
        .header("Content-Type", "application/json")
        .header("Cache-Control", "no-cache")
        .limit(256 * 1024)
        .backstop(by.saturating_duration_since(Instant::now()))
        .send(&[])
        .map_err(|_| {
            ObservationError::Unavailable("provider profile request did not complete".into())
        })?;
    profile_response(response.status(), response.body())
}

#[cfg(test)]
#[path = "cli_shared_claude/native_wait_tests.rs"]
mod native_wait_tests;

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
#[path = "cli_shared_claude_tests.rs"]
mod tests;
