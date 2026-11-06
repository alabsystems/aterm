// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Google OAuth with PKCE. Client registration belongs to aterm, not another mail app.

use crate::Secret;

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
        std::time::Instant::now() + std::time::Duration::from_secs(30),
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
        std::time::Instant::now() + std::time::Duration::from_secs(30),
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
    let within = end
        .saturating_duration_since(std::time::Instant::now())
        .min(std::time::Duration::from_secs(30));
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
    let client = aterm_http::Client::new(
        aterm_http::Trust::PlatformVerifier,
        aterm_http::ProxyMode::Environment,
        within,
    );
    let response = client
        .post("https://oauth2.googleapis.com/token")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .limit(64 * 1024)
        .send(body.as_bytes())
        .map_err(|_| Error {
            kind: ErrorKind::Transport,
        })?;
    decode_response(response.status(), response.body())
}

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
    #[test]
    fn token_responses_validate_scope_type_expiry_and_secrets() {
        let valid = br#"{"token_type":"Bearer","access_token":"access","expires_in":3600}"#;
        assert_eq!(
            decode_response(200, valid).unwrap().access_token.expose(),
            "access"
        );
        for body in [
            br#"{"token_type":"Bearer","access_token":"","expires_in":3600}"#.as_slice(),
            br#"{"token_type":"Basic","access_token":"access","expires_in":3600}"#,
            br#"{"token_type":"Bearer","access_token":"access","expires_in":0}"#,
            br#"{"token_type":"Bearer","access_token":"access","expires_in":3600,"scope":123}"#,
        ] {
            assert!(matches!(
                decode_response(200, body),
                Err(Error {
                    kind: ErrorKind::InvalidResponse
                })
            ));
        }
        assert!(matches!(decode_response(200, br#"{"token_type":"Bearer","access_token":"access","expires_in":3600,"scope":"profile"}"#), Err(Error { kind: ErrorKind::Rejected })));
        let with_nulls = br#"{"token_type":"Bearer","access_token":"access","expires_in":3600,"refresh_token":null,"scope":null}"#;
        let decoded_nulls = decode_response(200, with_nulls).unwrap();
        assert_eq!(decoded_nulls.access_token.expose(), "access");
        assert!(decoded_nulls.refresh_token.is_none());
        assert!(
            Error {
                kind: ErrorKind::InvalidResponse
            }
            .retryable()
        );
        for status in [407, 408, 425, 429, 500, 503] {
            assert!(
                decode_response(status, b"private")
                    .err()
                    .unwrap()
                    .retryable()
            );
        }
        assert!(
            decode_response(400, br#"{"error":"temporarily_unavailable"}"#)
                .err()
                .unwrap()
                .retryable()
        );
        let rejected = decode_response(400, b"sensitive-token").err().unwrap();
        assert!(!rejected.retryable());
        assert!(!format!("{rejected:?} {rejected}").contains("sensitive"));
    }

    #[test]
    fn expired_refresh_deadline_never_reaches_network() {
        let result = refresh_until(
            "client",
            None,
            &Secret::new("refresh".into()),
            std::time::Instant::now(),
        );
        assert!(matches!(
            result,
            Err(Error {
                kind: ErrorKind::Transport
            })
        ));
    }
}
