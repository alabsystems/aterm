// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! A scripted transport for unit tests: answers queued in order, every request recorded.

use std::collections::VecDeque;
use std::io::Cursor;
use std::sync::{Mutex, PoisonError};

use super::{Method, Phase, Transport, WireBody, WireError, WireKind, WireRequest, WireResponse};

/// One queued outcome.
pub enum Scripted {
    /// An answer.
    Answer {
        /// Its status.
        status: u16,
        /// Its headers, in order.
        headers: Vec<(String, String)>,
        /// Its body.
        body: Vec<u8>,
    },
    /// A failed exchange.
    Fail(WireError),
}

/// A request as the transport saw it. The credential is reduced to its profile and fingerprint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeenRequest {
    /// The method.
    pub method: Method,
    /// The URL.
    pub url: String,
    /// Every header but the credential.
    pub headers: Vec<(String, String)>,
    /// The credential's profile, when one was carried.
    pub auth_profile: Option<String>,
    /// The credential's fingerprint, when one was carried.
    pub auth_fingerprint: Option<String>,
    /// The body, read whole.
    pub body: Vec<u8>,
}

/// A transport that answers from a queue.
#[derive(Default)]
pub struct ScriptedTransport {
    queue: Mutex<VecDeque<Scripted>>,
    seen: Mutex<Vec<SeenRequest>>,
}

impl ScriptedTransport {
    /// An empty queue.
    #[must_use]
    pub fn new() -> ScriptedTransport {
        ScriptedTransport::default()
    }

    /// Queue an outcome.
    pub fn push(&self, outcome: Scripted) {
        self.queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push_back(outcome);
    }

    /// Queue an answer.
    pub fn answer(&self, status: u16, headers: &[(&str, &str)], body: &[u8]) {
        self.push(Scripted::Answer {
            status,
            headers: headers
                .iter()
                .map(|(n, v)| ((*n).to_string(), (*v).to_string()))
                .collect(),
            body: body.to_vec(),
        });
    }

    /// Every request seen, in order.
    #[must_use]
    pub fn requests(&self) -> Vec<SeenRequest> {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Outcomes still queued.
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }
}

fn local(detail: &str) -> WireError {
    WireError {
        phase: Phase::NotSent,
        kind: WireKind::Local,
        detail: detail.to_string(),
    }
}

impl Transport for ScriptedTransport {
    fn round_trip(&self, req: WireRequest<'_>) -> Result<WireResponse, WireError> {
        let body = match req.body {
            WireBody::Empty => Vec::new(),
            WireBody::Bytes(bytes) => bytes.to_vec(),
            WireBody::Reader { reader, .. } => {
                let mut bytes = Vec::new();
                reader
                    .read_to_end(&mut bytes)
                    .map_err(|e| local(&format!("scripted transport: the body: {e}")))?;
                bytes
            }
        };
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(SeenRequest {
                method: req.method,
                url: req.url,
                headers: req.headers,
                auth_profile: req.auth.map(|a| a.profile().to_string()),
                auth_fingerprint: req.auth.map(|a| a.fingerprint().to_string()),
                body,
            });
        let next = self
            .queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front();
        match next {
            None => Err(local("scripted transport: no answer queued")),
            Some(Scripted::Fail(error)) => Err(error),
            Some(Scripted::Answer {
                status,
                headers,
                body,
            }) => Ok(WireResponse {
                status,
                headers,
                body: Box::new(Cursor::new(body)),
            }),
        }
    }
}

impl super::sealed::Sealed for ScriptedTransport {}
