// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One non-streaming Google text generation request per durable request-count
//! admission. No monetary ceiling, provider billing receipt or native CLI proxy
//! is implied. The provider key stays inside the hosting process.

use super::{
    Broker,
    host::{Host, Limits},
    transport::AdapterReply,
};
use crate::resources::{BoundRequest, Observation, Outcome};
use aterm_http::{Body, Client, Guard, ProxyMode, Trust};
use aterm_uds::secret::Secret;
use serde::de::{self, MapAccess, Visitor};
use std::{
    fmt,
    io::{self, Read},
    path::Path,
    sync::Arc,
    time::Instant,
};
use zeroize::Zeroizing;

mod client;
pub use client::{request_account_until, request_until};

#[cfg(test)]
pub(crate) use client::test_request_until;

/// Versioned request-count contract, including the pinned destination and
/// reconstructed JSON fields. Select this exact name in the signed billing
/// policy and broker grant.
pub const ADAPTER: &str = "google-text-request-v1";
const ORIGIN: &str = "https://generativelanguage.googleapis.com";
const INPUT_MAX: usize = 64 * 1024;
const PROMPT_MAX: usize = 60 * 1024;
const RESPONSE_MAX: usize = 1024 * 1024 - 2;

struct Input {
    model: String,
    prompt: Secret,
    max_output_tokens: u64,
}

struct Prepared {
    path: String,
    body: Secret,
}

#[derive(Debug)]
struct Until(Instant);

impl Guard for Until {
    fn is_authorized(&self) -> bool {
        Instant::now() < self.0
    }
}

fn refused() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "invalid Google text request")
}

/// Create a local host for the closed Google text contract. It validates the
/// request after capability authentication and before any durable charge.
/// Each admitted effect retains one request unit even on rejection or failure.
/// # Errors
/// The local endpoint's privacy, ownership or socket creation errors.
pub fn bind(dir: &Path, broker: Broker, limits: Limits) -> io::Result<Host> {
    bind_at(dir, broker, limits, ORIGIN.to_owned())
}

fn bind_at(dir: &Path, broker: Broker, limits: Limits, origin: String) -> io::Result<Host> {
    Host::bind_prepared(
        dir,
        broker,
        limits,
        prepare,
        move |prepared, request, credential, deadline| {
            perform(prepared, request, credential, deadline, &origin)
        },
    )
}

fn prepare(adapter: &str, bytes: &[u8], deadline: Instant) -> io::Result<Prepared> {
    if adapter != ADAPTER || bytes.len() > INPUT_MAX || Instant::now() >= deadline {
        return Err(refused());
    }
    let input: Input = aterm_json::from_slice(bytes).map_err(|_| refused())?;
    let model = &input.model;
    if !model.starts_with("gemini-")
        || model.len() <= 7
        || model.len() > 96
        || !model
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'.'))
        || model.contains("..")
        || input.prompt.is_empty()
        || input.prompt.len() > PROMPT_MAX
        || !(1..=8192).contains(&input.max_output_tokens)
    {
        return Err(refused());
    }
    let prompt = std::str::from_utf8(input.prompt.expose()).map_err(|_| refused())?;
    let escaped = Zeroizing::new(aterm_json::quoted_string(prompt).map_err(|_| refused())?);
    let mut body = Zeroizing::new(Vec::with_capacity(escaped.len() + 200));
    body.extend_from_slice(br#"{"contents":[{"role":"user","parts":[{"text":"#);
    body.extend_from_slice(&escaped);
    body.extend_from_slice(br#"}]}],"generationConfig":{"candidateCount":1,"maxOutputTokens":"#);
    body.extend_from_slice(input.max_output_tokens.to_string().as_bytes());
    body.extend_from_slice(br#", "responseModalities":["TEXT"]}}"#);
    if Instant::now() >= deadline {
        return Err(refused());
    }
    Ok(Prepared {
        path: format!("/v1beta/models/{model}:generateContent"),
        body: Secret::new(&body),
    })
}

fn perform(
    prepared: Prepared,
    request: &BoundRequest,
    credential: &[u8],
    deadline: Instant,
    origin: &str,
) -> Option<AdapterReply> {
    if request.binding().adapter != ADAPTER || request.cost() != 1 {
        return None;
    }
    let key = aterm_secrets::api_key::decode(credential, "google").ok()?;
    let wait = deadline
        .checked_duration_since(Instant::now())
        .filter(|n| !n.is_zero())?;
    let client = Client::new(Trust::PlatformVerifier, ProxyMode::Direct, wait);
    let response = client
        .post(&format!("{origin}{}", prepared.path))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .secret_header("x-goog-api-key", Secret::new(key.expose()))
        .limit(RESPONSE_MAX)
        .backstop(wait)
        .guard(Arc::new(Until(deadline)))
        .exchange(Body::Bytes(prepared.body.expose()))
        .ok()?;
    let status = response.status();
    let mut body = Zeroizing::new(Vec::with_capacity(RESPONSE_MAX + 1));
    response
        .into_body()
        .take((RESPONSE_MAX + 1) as u64)
        .read_to_end(&mut body)
        .ok()?;
    if body.len() > RESPONSE_MAX {
        return None;
    }
    // A private commitment to the observed response, not a provider-signed
    // billing receipt and never a credential revision identity.
    let evidence = aterm_digest::hmac_sha256_parts(
        key.expose(),
        &[
            b"aterm/google-text-response/1\0",
            &status.to_be_bytes(),
            &body,
        ],
    );
    let observation = Observation::new(
        if (200..300).contains(&status) {
            Outcome::Completed
        } else {
            Outcome::Rejected
        },
        evidence,
    )
    .ok()?;
    let mut reply = Vec::with_capacity(body.len() + 2);
    reply.extend_from_slice(&status.to_be_bytes());
    reply.extend_from_slice(&body);
    AdapterReply::from_secret(Secret::from_vec(reply), Some(observation)).ok()
}

impl<'de> serde::Deserialize<'de> for Input {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct InputVisitor;
        impl<'de> Visitor<'de> for InputVisitor {
            type Value = Input;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a closed Google text request")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Input, M::Error> {
                let (mut model, mut prompt, mut max_output_tokens) = (None, None, None);
                while let Some(field) = map.next_key::<String>()? {
                    match field.as_str() {
                        "model" if model.is_none() => model = Some(map.next_value::<String>()?),
                        "prompt" if prompt.is_none() => {
                            prompt =
                                Some(Secret::from_vec(map.next_value::<String>()?.into_bytes()))
                        }
                        "max_output_tokens" if max_output_tokens.is_none() => {
                            max_output_tokens = Some(map.next_value::<u64>()?)
                        }
                        _ => return Err(de::Error::custom("unknown or duplicate field")),
                    }
                }
                Ok(Input {
                    model: model.ok_or_else(|| de::Error::custom("model"))?,
                    prompt: prompt.ok_or_else(|| de::Error::custom("prompt"))?,
                    max_output_tokens: max_output_tokens
                        .ok_or_else(|| de::Error::custom("max_output_tokens"))?,
                })
            }
        }
        deserializer.deserialize_map(InputVisitor)
    }
}

/// Read the status prefix of this adapter's reply without copying its private
/// provider body. Status is a received HTTP response, not proof of billing.
/// # Errors
/// Missing/invalid status prefix.
pub fn response(body: &[u8]) -> io::Result<(u16, &[u8])> {
    let status = u16::from_be_bytes(
        body.get(..2)
            .ok_or_else(refused)?
            .try_into()
            .map_err(|_| refused())?,
    );
    if !(100..=599).contains(&status) {
        return Err(refused());
    }
    Ok((status, &body[2..]))
}

#[cfg(test)]
pub(crate) fn test_bind_at(
    dir: &Path,
    broker: Broker,
    limits: Limits,
    origin: String,
) -> io::Result<Host> {
    bind_at(dir, broker, limits, origin)
}

#[cfg(test)]
mod tests;
