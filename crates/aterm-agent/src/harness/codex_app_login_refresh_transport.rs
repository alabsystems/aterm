// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Transport only for an already admitted, sole-owner native OAuth grant.
//! No file, legacy journal, publication or refresh delegation is entered here.

use super::*;
use crate::harness::auth_deadline::Deadline;
use aterm_secrets::SecretBytes;

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
    let Ok(left) = deadline.capped(EXCHANGE_WAIT) else {
        return Ok(RefreshExchange::NotSent);
    };
    if left < MIN_REFRESH_BUDGET {
        return Ok(RefreshExchange::NotSent);
    }
    let response = http_client(&authority.token_url, left)
        .request(Method::Post, &authority.token_url)
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
mod tests {
    use super::*;

    fn document(issuer: &str) -> Vec<u8> {
        aterm_json::to_vec(&json!({"auth_mode":"chatgpt","tokens":{
            "access_token":"test-old-access","refresh_token":"test-refresh-canary",
            "account_id":"test-account","oauth_provenance":{"issuer":issuer,"client_id":CLIENT_ID}
        }}))
        .unwrap()
    }

    #[test]
    fn absent_budget_invalid_authority_and_failed_arm_never_send() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let doc = document(&format!("http://{}", listener.local_addr().unwrap()));
        let armed = AtomicBool::new(false);
        let result = exchange_owned_refresh(&doc, Deadline::after(Duration::ZERO), || {
            armed.store(true, Ordering::SeqCst);
            Ok(())
        });
        assert!(result.is_err() || matches!(result, Ok(RefreshExchange::NotSent)));
        assert!(!armed.load(Ordering::SeqCst));
        assert!(
            exchange_owned_refresh(&doc, Deadline::after(Duration::from_secs(10)), || Err(
                "not durable".into()
            ))
            .is_err()
        );
        assert!(
            exchange_owned_refresh(
                &document("http://not-loopback.example"),
                Deadline::after(Duration::from_secs(10)),
                || panic!("invalid authority armed")
            )
            .is_err()
        );
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    fn response_case(status: u16, body: Option<&'static str>) -> RefreshExchange {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let issuer = format!("http://{}", listener.local_addr().unwrap());
        let armed = Arc::new(AtomicBool::new(false));
        let check = Arc::clone(&armed);
        let server = std::thread::spawn(move || {
            let until = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < until,
                            "refresh fixture accept did not happen"
                        );
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("refresh fixture accept failed: {error}"),
                }
            };
            assert!(check.load(Ordering::SeqCst), "request preceded durable arm");
            // BSD accepts may inherit the listener's nonblocking mode. The
            // fixture reads a complete request under its hang detector.
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
                .unwrap();
            stream
                .set_write_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
                .unwrap();
            let mut reader = std::io::BufReader::new(&stream);
            let mut size = None;
            loop {
                let mut line = String::new();
                std::io::BufRead::read_line(&mut reader, &mut line).unwrap();
                assert!(!line.is_empty(), "refresh request ended before its headers");
                if line == "\r\n" {
                    break;
                }
                if let Some((key, value)) = line.split_once(':')
                    && key.eq_ignore_ascii_case("content-length")
                {
                    size = Some(value.trim().parse::<usize>().unwrap());
                }
            }
            let mut sent = vec![0; size.unwrap()];
            reader.read_exact(&mut sent).unwrap();
            let sent: Value = aterm_json::from_slice(&sent).unwrap();
            assert_eq!(sent["grant_type"].as_str(), Some("refresh_token"));
            drop(reader);
            if let Some(body) = body {
                write!(stream, "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
                stream.flush().unwrap();
            }
        });
        let result = exchange_owned_refresh(
            &document(&issuer),
            Deadline::after(aterm_spec::wait::MUST_HAPPEN),
            || {
                armed.store(true, Ordering::SeqCst);
                Ok(())
            },
        )
        .unwrap();
        server.join().unwrap();
        result
    }

    #[test]
    fn completed_responses_are_owned_before_any_payload_interpretation() {
        for (status, body, refused) in [
            (200, "not-json-test-canary", false),
            (400, r#"{"error":"invalid_grant"}"#, true),
        ] {
            let RefreshExchange::Completed(received) = response_case(status, Some(body)) else {
                panic!("completed refresh response was lost");
            };
            assert_eq!(received.status(), status);
            assert_eq!(received.body(), body.as_bytes());
            assert!(received.received_at().is_some());
            assert_eq!(refresh_refused(received.status(), received.body()), refused);
            assert!(!format!("{received:?}").contains(body));
        }
    }

    #[test]
    fn a_sent_request_without_a_response_is_unknown_not_replayable() {
        assert!(matches!(response_case(200, None), RefreshExchange::Unknown));
    }
    fn identity_token(user: &str, workspace: &str) -> String {
        super::super::tests::make_jwt(&json!({"https://api.openai.com/auth": {
            "chatgpt_user_id":user, "chatgpt_account_id":workspace
        }}))
    }

    fn previous_document() -> Vec<u8> {
        let token = identity_token("same-user", "jwt-default");
        aterm_json::to_vec(
            &json!({"auth_mode":"chatgpt", "login_owner_id":"fresh-code-owner",
                "unknown_native_field":{"keep":[1,2]}, "tokens":{
                    "access_token":token, "id_token":token, "refresh_token":"old-refresh",
                    "account_id":"selected-workspace", "extra_native_token_field":true,
                    "oauth_provenance":{"issuer":DEFAULT_ISSUER,"client_id":CLIENT_ID}
            }}),
        )
        .unwrap()
    }

    #[test]
    fn owned_merge_preserves_selected_workspace_optional_tokens_and_native_metadata() {
        let previous = previous_document();
        let before: Value = aterm_json::from_slice(&previous).unwrap();
        let access = identity_token("same-user", "another-jwt-default");
        let body = aterm_json::to_vec(&json!({"access_token":access})).unwrap();
        let merged = merge_owned_refresh(&previous, 200, &body, 1_800_000_000).unwrap();
        let after: Value = aterm_json::from_slice(merged.as_slice()).unwrap();
        for key in ["auth_mode", "login_owner_id", "unknown_native_field"] {
            assert_eq!(before.get(key), after.get(key));
        }
        for key in [
            "account_id",
            "id_token",
            "refresh_token",
            "oauth_provenance",
            "extra_native_token_field",
        ] {
            assert_eq!(before["tokens"].get(key), after["tokens"].get(key));
        }
        assert_eq!(
            after["tokens"]["access_token"].as_str(),
            Some(access.as_str())
        );
        assert_eq!(after["last_refresh"].as_str(), Some("2027-01-15T08:00:00Z"));
        let body = aterm_json::to_vec(
            &json!({"access_token":access, "id_token":identity_token("same-user","changed-default"),
            "refresh_token":"rotated-refresh"}),
        )
        .unwrap();
        let rotated = merge_owned_refresh(&previous, 200, &body, 1_800_000_001).unwrap();
        let rotated: Value = aterm_json::from_slice(rotated.as_slice()).unwrap();
        assert_eq!(
            rotated["tokens"]["refresh_token"].as_str(),
            Some("rotated-refresh")
        );
        assert_eq!(
            rotated["tokens"]["account_id"].as_str(),
            Some("selected-workspace")
        );
    }

    #[test]
    fn owned_merge_refuses_changed_or_conflicting_users_malformed_and_oversize_documents() {
        let previous = previous_document();
        let other = identity_token("another-user", "selected-workspace");
        for body in [
            b"not-json".to_vec(),
            b"{}".to_vec(),
            br#"{"access_token":""}"#.to_vec(),
            aterm_json::to_vec(&json!({"access_token":other})).unwrap(),
            aterm_json::to_vec(&json!({"access_token":other,"id_token":other})).unwrap(),
            aterm_json::to_vec(&json!({"access_token":"x".repeat(16*1024)})).unwrap(),
        ] {
            assert!(merge_owned_refresh(&previous, 200, &body, 1_800_000_000).is_err());
        }
        assert!(merge_owned_refresh(&previous, 401, b"{}", 1_800_000_000).is_err());
        assert!(
            merge_owned_refresh(&previous, 200, br#"{"access_token":"opaque"}"#, u64::MAX).is_err()
        );
        assert!(
            merge_owned_refresh(&vec![b'x'; 16 * 1024 + 1], 200, b"{}", 1_800_000_000).is_err()
        );
    }
    #[test]
    fn owned_token_digest_matches_native_spent_token_correlation() {
        let doc = previous_document();
        assert_eq!(
            owned_refresh_token_digest(&doc).unwrap(),
            refresh_token_digest("old-refresh")
        );
        assert!(owned_refresh_token_digest(b"{}").is_err());
        assert!(owned_refresh_token_digest(&vec![b' '; 16 * 1024 + 1]).is_err());
    }
}
