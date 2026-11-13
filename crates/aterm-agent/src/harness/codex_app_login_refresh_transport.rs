// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Transport only for an already admitted, sole-owner native OAuth grant.
//! No file, legacy journal, publication or refresh delegation is entered here.

use super::*;
use crate::harness::auth_deadline::Deadline;
use aterm_secrets::SecretBytes;

#[cfg(test)]
thread_local! {
    static FIXTURE_ENDPOINT: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Redirect only the final send after real document/issuer/client admission.
/// This is a local transport fixture, never production issuer authority.
#[cfg(all(test, unix))]
pub(in crate::harness) fn with_refresh_fixture_endpoint<R>(
    endpoint: &str,
    run: impl FnOnce() -> R,
) -> R {
    let address = endpoint
        .strip_prefix("http://")
        .unwrap()
        .strip_suffix("/oauth/token")
        .unwrap();
    let address: std::net::SocketAddr = address.parse().unwrap();
    assert!(address.ip().is_loopback());
    struct Reset(Option<String>);
    impl Drop for Reset {
        fn drop(&mut self) {
            FIXTURE_ENDPOINT.with(|slot| *slot.borrow_mut() = self.0.take());
        }
    }
    let prior = FIXTURE_ENDPOINT.with(|slot| slot.replace(Some(endpoint.to_owned())));
    let _reset = Reset(prior);
    run()
}

pub(in crate::harness) struct ReceivedRefresh {
    status: u16,
    body: SecretBytes,
    received_at: Option<u64>,
}

impl ReceivedRefresh {
    pub(in crate::harness) fn status(&self) -> u16 {
        self.status
    }
    pub(in crate::harness) fn body(&self) -> &[u8] {
        self.body.as_slice()
    }
    pub(in crate::harness) fn received_at(&self) -> Option<u64> {
        self.received_at
    }
}

impl std::fmt::Debug for ReceivedRefresh {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ReceivedRefresh(redacted)")
    }
}

#[must_use = "retain and encrypt completed OAuth responses before interpreting or publishing them"]
pub(in crate::harness) enum RefreshExchange {
    NotSent,
    Unknown,
    Completed(ReceivedRefresh),
}

impl std::fmt::Debug for RefreshExchange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotSent => "RefreshExchange(not sent)",
            Self::Unknown => "RefreshExchange(uncertain)",
            Self::Completed(_) => "RefreshExchange(completed, redacted)",
        })
    }
}

/// All errors occur before a send. After calling the transport, uncertainty is
/// represented explicitly; even malformed HTTP-200 bodies retain owned custody.
pub(in crate::harness) fn exchange_owned_refresh(
    document: &[u8],
    deadline: Deadline<'_>,
    before_send: impl FnOnce() -> Result<(), String>,
) -> Result<RefreshExchange, String> {
    exchange_owned_refresh_with_clock(document, deadline, before_send, Instant::now)
}

fn exchange_owned_refresh_with_clock(
    document: &[u8],
    deadline: Deadline<'_>,
    before_send: impl FnOnce() -> Result<(), String>,
    request_started: impl FnOnce() -> Instant,
) -> Result<RefreshExchange, String> {
    if document.is_empty() || document.len() > 16 * 1024 {
        return Err("managed refresh document unavailable".into());
    }
    let value =
        aterm_json::from_slice(document).map_err(|_| "managed refresh document unavailable")?;
    let stored = stored_auth_from_doc(value).ok_or("managed refresh document unavailable")?;
    let authority = refresh_authority(&stored, &format!("{DEFAULT_ISSUER}/oauth/token"))
        .ok_or("managed refresh authority unavailable")?;
    let token = stored
        .refresh_token
        .as_deref()
        .filter(|token| !token.is_empty())
        .ok_or("managed refresh token unavailable")?;
    let body = SecretBytes::new(
        aterm_json::to_vec(&json!({
            "client_id": authority.client_id,
            "grant_type": "refresh_token",
            "refresh_token": token,
        }))
        .map_err(|_| "managed refresh request unavailable")?,
    );
    if deadline
        .capped(EXCHANGE_WAIT)
        .map_err(|_| "managed refresh deadline expired")?
        < MIN_REFRESH_BUDGET
    {
        return Ok(RefreshExchange::NotSent);
    }
    before_send()?;
    let Ok(left) = deadline.remaining() else {
        return Ok(RefreshExchange::NotSent);
    };
    let left = exchange_wait(left, request_started());
    if left < MIN_REFRESH_BUDGET {
        return Ok(RefreshExchange::NotSent);
    }
    #[cfg(test)]
    let fixture_endpoint = FIXTURE_ENDPOINT.with(|slot| slot.borrow().clone());
    #[cfg(test)]
    let token_url = fixture_endpoint.as_deref().unwrap_or(&authority.token_url);
    #[cfg(not(test))]
    let token_url = authority.token_url.as_str();
    let response = http_client(token_url, left)
        .request(Method::Post, token_url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .header("User-Agent", "codex-cli")
        .header("originator", ORIGINATOR)
        .limit(MESSAGE_MAX)
        .backstop(left)
        .send_phased(body.as_slice());
    Ok(match response {
        Err(failed) if failed.phase == aterm_http::Phase::NotSent => RefreshExchange::NotSent,
        Err(_) => RefreshExchange::Unknown,
        Ok(response) => RefreshExchange::Completed(ReceivedRefresh {
            status: response.status(),
            body: SecretBytes::new(response.body().to_vec()),
            received_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|elapsed| elapsed.as_secs()),
        }),
    })
}

/// Native spent-token correlation, never a credential or ownership capability.
pub(in crate::harness) fn owned_refresh_token_digest(document: &[u8]) -> Result<String, String> {
    if document.is_empty() || document.len() > 16 * 1024 {
        return Err("managed refresh document unavailable".into());
    }
    let value =
        aterm_json::from_slice(document).map_err(|_| "managed refresh document unavailable")?;
    let stored = stored_auth_from_doc(value).ok_or("managed refresh document unavailable")?;
    let token = stored
        .refresh_token
        .as_deref()
        .filter(|token| !token.is_empty())
        .ok_or("managed refresh token unavailable")?;
    Ok(refresh_token_digest(token))
}

/// Whether a response the caller's encrypted journal retained explicitly refuses its refresh
/// token for good. Unlike the live legacy exchange ([`refresh_refused`]), a bare `401` is not
/// enough here: the retained answer may be a gateway challenge rather than the token issuer's
/// classification. Any other answer [`merge_owned_refresh`] cannot use remains uncertain.
pub(in crate::harness) fn owned_refresh_refused(status: u16, body: &[u8]) -> bool {
    status != 200 && body.len() <= MESSAGE_MAX && explicit_refresh_refusal(status, body)
}

/// When an explicitly refused owned refresh has outlived every permitted publication wait.
/// The exact canonical document is checked by the caller while it holds native Auth custody.
pub(in crate::harness) fn owned_refresh_refusal_settled_at(
    status: u16,
    body: &[u8],
    received_at: Option<u64>,
    now: i64,
) -> Option<i64> {
    let received_at = received_at?;
    let settled_at = received_at.checked_add(ROTATION_HANG.as_secs())?;
    let settled_at = i64::try_from(settled_at).ok()?;
    (owned_refresh_refused(status, body) && now >= settled_at).then_some(settled_at)
}

/// Interpret only a response already retained by the caller's encrypted journal.
/// This pure merge grants no publication or refresh authority.
pub(in crate::harness) fn merge_owned_refresh(
    previous: &[u8],
    status: u16,
    body: &[u8],
    received_at: u64,
) -> Result<SecretBytes, String> {
    const MAX_DOCUMENT: usize = 16 * 1024;
    let refused = || "managed refresh response is unavailable".to_owned();
    if status != 200
        || previous.is_empty()
        || previous.len() > MAX_DOCUMENT
        || body.is_empty()
        || body.len() > MESSAGE_MAX
    {
        return Err(refused());
    }
    let previous: Value = aterm_json::from_slice(previous).map_err(|_| refused())?;
    let stored = stored_auth_from_doc(previous).ok_or_else(refused)?;
    let identity = stored_identity(&stored).ok_or_else(refused)?;
    if stored.refresh_token.is_none() {
        return Err(refused());
    }
    let response: Value = aterm_json::from_slice(body).map_err(|_| refused())?;
    let text = |key: &str| {
        response
            .get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
    };
    let access = text("access_token").ok_or_else(refused)?;
    let at = i64::try_from(received_at).map_err(|_| refused())?;
    let stamp = aterm_types::rfc3339::format_rfc3339(received_at);
    if aterm_types::rfc3339::parse_utc_fractional(&stamp) != Some(at) {
        return Err(refused());
    }
    let tokens = RefreshedTokens {
        access_token: access.to_owned(),
        id_token: text("id_token").map(str::to_owned),
        refresh_token: text("refresh_token").map(str::to_owned),
        // The selected workspace is native policy, not a JWT default.
        account_id: stored.account_id.clone(),
        last_refresh: stamp,
    };
    let merged = tokens.merged_into(stored.doc).ok_or_else(refused)?;
    let candidate = stored_auth_from_doc(merged).ok_or_else(refused)?;
    if stored_identity(&candidate).as_ref() != Some(&identity) {
        return Err(refused());
    }
    let mut document = aterm_json::to_vec(&candidate.doc).map_err(|_| refused())?;
    document.push(b'\n');
    if document.len() > MAX_DOCUMENT {
        return Err(refused());
    }
    Ok(SecretBytes::new(document))
}

#[cfg(test)]
#[path = "codex_app_login_refresh_transport_tests.rs"]
mod tests;
