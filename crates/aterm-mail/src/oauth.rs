// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Google OAuth with PKCE. Client registration belongs to aterm, not another mail app.

use crate::Secret;

/// Hang detector for a required token response, bounded by its original parent.
const TOKEN_REQUEST_HANG: std::time::Duration = std::time::Duration::from_secs(60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    Transport,
    Temporary,
    Rejected,
    InvalidResponse,
    Storage,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub kind: ErrorKind,
}
impl Error {
    pub fn retryable(&self) -> bool {
        !matches!(self.kind, ErrorKind::Rejected)
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self.kind {
            ErrorKind::Transport => "Could not reach Google authentication",
            ErrorKind::Temporary => "Google authentication is temporarily unavailable",
            ErrorKind::Rejected => "Google rejected the mail authorization",
            ErrorKind::InvalidResponse => "Google returned an invalid mail authorization",
            ErrorKind::Storage => "Could not persist Google mail authorization",
        })
    }
}
impl From<Error> for String {
    fn from(error: Error) -> Self {
        error.to_string()
    }
}

#[path = "google_session.rs"]
pub mod session;

#[path = "oauth_enrollment.rs"]
pub mod enrollment;

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

fn url_safe_base64(input: &[u8]) -> Result<String, String> {
    let mut encoded = aterm_codec::base64::encode(input)
        .map_err(|e| e.to_string())?
        .into_bytes();
    while encoded.last() == Some(&b'=') {
        encoded.pop();
    }
    for b in &mut encoded {
        match *b {
            b'+' => *b = b'-',
            b'/' => *b = b'_',
            _ => {}
        }
    }
    String::from_utf8(encoded).map_err(|e| e.to_string())
}

fn random() -> Result<String, String> {
    let mut bytes = zeroize::Zeroizing::new([0u8; 32]);
    aterm_uds::rand::fill(&mut *bytes).map_err(|e| e.to_string())?;
    url_safe_base64(&*bytes)
}

fn encoded_len(value: &str) -> usize {
    value
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                1
            } else {
                3
            }
        })
        .sum()
}

pub(crate) fn encode_into(result: &mut String, value: &str) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            result.push(char::from(b));
        } else {
            result.push('%');
            result.push(char::from(HEX[usize::from(b >> 4)]));
            result.push(char::from(HEX[usize::from(b & 0x0f)]));
        }
    }
}

pub fn encode(value: &str) -> String {
    let mut result = String::with_capacity(encoded_len(value));
    encode_into(&mut result, value);
    result
}

pub fn begin(client_id: &str, port: u16, login_hint: Option<&str>) -> Result<Pending, String> {
    if client_id.is_empty() || port == 0 {
        return Err("Google desktop OAuth client is not configured".into());
    }
    let mut verifier = zeroize::Zeroizing::new(random()?);
    let digest = ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes());
    let challenge = url_safe_base64(digest.as_ref())?;
    let state = random()?;
    let redirect_uri = format!("http://127.0.0.1:{port}/oauth/callback");
    let mut authorize_url = String::with_capacity(
        192 + encoded_len(client_id)
            + encoded_len(&redirect_uri)
            + state.len()
            + challenge.len()
            + login_hint.map_or(0, |h| 12 + encoded_len(h)),
    );
    authorize_url.push_str("https://accounts.google.com/o/oauth2/v2/auth?client_id=");
    encode_into(&mut authorize_url, client_id);
    authorize_url.push_str("&redirect_uri=");
    encode_into(&mut authorize_url, &redirect_uri);
    authorize_url.push_str("&response_type=code&scope=");
    encode_into(&mut authorize_url, "https://mail.google.com/");
    authorize_url.push_str("&state=");
    encode_into(&mut authorize_url, &state);
    authorize_url.push_str("&code_challenge=");
    encode_into(&mut authorize_url, &challenge);
    authorize_url.push_str(
        "&code_challenge_method=S256&access_type=offline&prompt=consent%20select_account",
    );
    if let Some(hint) = login_hint {
        authorize_url.push_str("&login_hint=");
        encode_into(&mut authorize_url, hint);
    }
    Ok(Pending {
        state,
        verifier: Secret::new(std::mem::take(&mut *verifier)),
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
) -> Result<Tokens, Error> {
    exchange_until(
        client_id,
        client_secret,
        pending,
        state,
        code,
        std::time::Instant::now() + TOKEN_REQUEST_HANG,
    )
}

pub fn exchange_until(
    client_id: &str,
    client_secret: Option<&str>,
    pending: &Pending,
    state: &str,
    code: &str,
    end: std::time::Instant,
) -> Result<Tokens, Error> {
    if !aterm_digest::ct_eq(state.as_bytes(), pending.state.as_bytes()) {
        return Err(Error {
            kind: ErrorKind::Rejected,
        });
    }
    let fields = [
        ("client_id", client_id),
        ("code", code),
        ("code_verifier", pending.verifier.expose()),
        ("redirect_uri", &pending.redirect_uri),
        ("grant_type", "authorization_code"),
        ("client_secret", client_secret.unwrap_or("")),
    ];
    let fields = if client_secret.is_some() {
        &fields[..]
    } else {
        &fields[..5]
    };
    request_until(fields, end)
}

pub fn refresh(
    client_id: &str,
    client_secret: Option<&str>,
    refresh_token: &Secret,
) -> Result<Tokens, Error> {
    refresh_until(
        client_id,
        client_secret,
        refresh_token,
        std::time::Instant::now() + TOKEN_REQUEST_HANG,
    )
}

pub fn refresh_until(
    client_id: &str,
    client_secret: Option<&str>,
    refresh_token: &Secret,
    end: std::time::Instant,
) -> Result<Tokens, Error> {
    if client_id.is_empty() || refresh_token.expose().is_empty() {
        return Err(Error {
            kind: ErrorKind::Rejected,
        });
    }
    let fields = [
        ("client_id", client_id),
        ("refresh_token", refresh_token.expose()),
        ("grant_type", "refresh_token"),
        ("client_secret", client_secret.unwrap_or("")),
    ];
    let fields = if client_secret.is_some() {
        &fields[..]
    } else {
        &fields[..3]
    };
    request_until(fields, end)
}

fn request_until(fields: &[(&str, &str)], end: std::time::Instant) -> Result<Tokens, Error> {
    request_started(
        fields,
        end,
        std::time::Instant::now(),
        "https://oauth2.googleapis.com/token",
        aterm_http::ProxyMode::Environment,
    )
}

fn request_started(
    fields: &[(&str, &str)],
    end: std::time::Instant,
    started: std::time::Instant,
    endpoint: &str,
    proxy: aterm_http::ProxyMode,
) -> Result<Tokens, Error> {
    let by = end.min(started + TOKEN_REQUEST_HANG);
    let within = by.saturating_duration_since(std::time::Instant::now());
    if within.is_zero() {
        return Err(Error {
            kind: ErrorKind::Transport,
        });
    }
    let cap: usize = fields
        .iter()
        .enumerate()
        .map(|(i, (k, v))| usize::from(i > 0) + encoded_len(k) + 1 + encoded_len(v))
        .sum();
    let mut body = zeroize::Zeroizing::new(String::with_capacity(cap));
    for (i, &(k, v)) in fields.iter().enumerate() {
        if i > 0 {
            body.push('&');
        }
        encode_into(&mut body, k);
        body.push('=');
        encode_into(&mut body, v);
    }
    let client = aterm_http::Client::new(aterm_http::Trust::PlatformVerifier, proxy, within);
    let response = client
        .post(endpoint)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .limit(64 * 1024)
        .backstop(by.saturating_duration_since(std::time::Instant::now()))
        .send(body.as_bytes())
        .map_err(|_| Error {
            kind: ErrorKind::Transport,
        })?;
    decode_response(response.status(), response.body())
}

#[cfg(test)]
#[path = "oauth/native_wait_tests.rs"]
mod native_wait_tests;

fn decode_response(status: u16, body: &[u8]) -> Result<Tokens, Error> {
    if matches!(status, 407 | 408 | 425 | 429) || status >= 500 {
        return Err(Error {
            kind: ErrorKind::Temporary,
        });
    }
    if status != 200 {
        if let Ok(err_val) = aterm_json::from_slice::<aterm_json::Value>(body)
            && let Some(code) = err_val["error"].as_str()
            && {
                let code = code.trim();
                code.eq_ignore_ascii_case("temporarily_unavailable")
                    || code.eq_ignore_ascii_case("server_error")
                    || code.eq_ignore_ascii_case("slow_down")
            }
        {
            return Err(Error {
                kind: ErrorKind::Temporary,
            });
        }
        return Err(Error {
            kind: ErrorKind::Rejected,
        });
    }
    let invalid = || Error {
        kind: ErrorKind::InvalidResponse,
    };
    let value: aterm_json::Value = aterm_json::from_slice(body).map_err(|_| invalid())?;
    if value["token_type"]
        .as_str()
        .is_none_or(|kind| !kind.eq_ignore_ascii_case("Bearer"))
    {
        return Err(invalid());
    }
    if value.get("scope").is_some()
        && !value["scope"].is_null()
        && value["scope"].as_str().is_none()
    {
        return Err(invalid());
    }
    if let Some(scope) = value["scope"].as_str()
        && !scope
            .split_whitespace()
            .any(|scope| scope == "https://mail.google.com/")
    {
        return Err(Error {
            kind: ErrorKind::Rejected,
        });
    }
    let secret = |name: &str| -> Result<Secret, Error> {
        let raw = value[name]
            .as_str()
            .filter(|value| {
                !value.is_empty() && value.len() <= 8192 && !value.chars().any(char::is_control)
            })
            .ok_or_else(invalid)?;
        Ok(Secret::new(raw.into()))
    };
    let expires_in = value["expires_in"]
        .as_u64()
        .filter(|seconds| *seconds > 0 && *seconds <= 86_400)
        .ok_or_else(invalid)?;
    let refresh_token = match value.get("refresh_token") {
        None | Some(aterm_json::Value::Null) => None,
        Some(_) => Some(secret("refresh_token")?),
    };
    Ok(Tokens {
        access_token: secret("access_token")?,
        refresh_token,
        expires_in,
    })
}

#[cfg(test)]
#[path = "oauth_tests.rs"]
mod tests;
