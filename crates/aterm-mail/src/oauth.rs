// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Google OAuth with PKCE. Client registration belongs to aterm, not another mail app.

use crate::Secret;

pub struct Pending {
    pub state: String,
    pub verifier: Secret,
    pub redirect_uri: String,
    pub authorize_url: String,
}

pub struct Tokens {
    pub access_token: Secret,
    pub refresh_token: Option<Secret>,
    pub expires_in: u64,
}

fn random() -> Result<String, String> {
    let mut bytes = [0; 32];
    aterm_uds::rand::fill(&mut bytes).map_err(|e| e.to_string())?;
    aterm_codec::base64::encode(&bytes)
        .map(|s| s.trim_end_matches('=').replace('+', "-").replace('/', "_"))
        .map_err(|e| e.to_string())
}

pub fn encode(value: &str) -> String {
    let mut result = String::new();
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            result.push(char::from(b));
        } else {
            use std::fmt::Write;
            let _ = write!(result, "%{b:02X}");
        }
    }
    result
}

pub fn begin(client_id: &str, port: u16, login_hint: Option<&str>) -> Result<Pending, String> {
    if client_id.is_empty() || port == 0 {
        return Err("Google desktop OAuth client is not configured".into());
    }
    let verifier = random()?;
    let digest = ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes());
    let challenge = aterm_codec::base64::encode(digest.as_ref())
        .map_err(|e| e.to_string())?
        .trim_end_matches('=')
        .replace('+', "-")
        .replace('/', "_");
    let state = random()?;
    let redirect_uri = format!("http://127.0.0.1:{port}/oauth/callback");
    let mut authorize_url = format!(
        "https://accounts.google.com/o/oauth2/v2/auth?client_id={}&redirect_uri={}&response_type=code&scope={}&state={}&code_challenge={}&code_challenge_method=S256&access_type=offline&prompt=consent%20select_account",
        encode(client_id),
        encode(&redirect_uri),
        encode("https://mail.google.com/"),
        encode(&state),
        encode(&challenge)
    );
    if let Some(hint) = login_hint {
        authorize_url.push_str(&format!("&login_hint={}", encode(hint)));
    }
    Ok(Pending {
        state,
        verifier: Secret::new(verifier),
        redirect_uri,
        authorize_url,
    })
}

pub fn exchange(
    client_id: &str,
    client_secret: Option<&str>,
    pending: &Pending,
    state: &str,
    code: &str,
) -> Result<Tokens, String> {
    if !aterm_digest::ct_eq(state.as_bytes(), pending.state.as_bytes()) {
        return Err("Google sign-in state did not match".into());
    }
    let mut fields = vec![
        ("client_id", client_id),
        ("code", code),
        ("code_verifier", pending.verifier.expose()),
        ("redirect_uri", &pending.redirect_uri),
        ("grant_type", "authorization_code"),
    ];
    if let Some(secret) = client_secret {
        fields.push(("client_secret", secret));
    }
    request(fields)
}

pub fn refresh(
    client_id: &str,
    client_secret: Option<&str>,
    refresh_token: &Secret,
) -> Result<Tokens, String> {
    let mut fields = vec![
        ("client_id", client_id),
        ("refresh_token", refresh_token.expose()),
        ("grant_type", "refresh_token"),
    ];
    if let Some(secret) = client_secret {
        fields.push(("client_secret", secret));
    }
    request(fields)
}

fn request(fields: Vec<(&str, &str)>) -> Result<Tokens, String> {
    let body = fields
        .into_iter()
        .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    let client = aterm_http::Client::new(
        aterm_http::Trust::PlatformVerifier,
        aterm_http::ProxyMode::Environment,
        std::time::Duration::from_secs(30),
    );
    let response = client
        .post("https://oauth2.googleapis.com/token")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .limit(64 * 1024)
        .send(body.as_bytes())
        .map_err(|e| e.to_string())?;
    if response.status() != 200 {
        return Err(format!(
            "Google sign-in failed (HTTP {})",
            response.status()
        ));
    }
    let value: aterm_json::Value =
        aterm_json::from_slice(response.body()).map_err(|_| "Invalid Google token response")?;
    Ok(Tokens {
        access_token: Secret::new(
            value["access_token"]
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or("Google returned no access token")?
                .into(),
        ),
        refresh_token: value["refresh_token"]
            .as_str()
            .map(|s| Secret::new(s.into())),
        expires_in: value["expires_in"].as_u64().unwrap_or(3600),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pkce_is_fresh_and_callback_rejects_wrong_state_before_network() {
        let first = begin("client", 1234, Some("me@gmail.com")).unwrap();
        let second = begin("client", 1234, None).unwrap();
        assert_ne!(first.state, second.state);
        assert!(first.authorize_url.contains("code_challenge_method=S256"));
        assert!(exchange("client", None, &first, "wrong", "unused").is_err());
    }
}
