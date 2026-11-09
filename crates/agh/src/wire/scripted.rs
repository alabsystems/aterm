// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! A scripted transport for unit tests: answers queued in order, every request recorded.

use std::collections::VecDeque;
use std::io::{Cursor, Read};
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
    /// An answered head whose body yields `body` and then fails while it is read.
    BodyFault {
        /// Its status.
        status: u16,
        /// Its headers, in order.
        headers: Vec<(String, String)>,
        /// Bytes yielded before the failure.
        body: Vec<u8>,
        /// The wire class the body read must produce.
        kind: WireKind,
    },
    /// A failed exchange.
    Fail(WireError),
}

/// A request as the transport saw it.
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
    /// Each request seen, with its permit's profile or `None`, pushed as one pair so that
    /// concurrent round trips never pair a request with another's permit.
    seen: Mutex<Vec<(SeenRequest, Option<String>)>>,
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

    /// Queue an answered head whose body yields `body` and then fails as `kind`.
    pub fn answer_body_fault(
        &self,
        status: u16,
        headers: &[(&str, &str)],
        body: &[u8],
        kind: WireKind,
    ) {
        self.push(Scripted::BodyFault {
            status,
            headers: headers
                .iter()
                .map(|(n, v)| ((*n).to_string(), (*v).to_string()))
                .collect(),
            body: body.to_vec(),
            kind,
        });
    }

    /// Every request seen, in order.
    #[must_use]
    pub fn requests(&self) -> Vec<SeenRequest> {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(request, _)| request.clone())
            .collect()
    }

    /// The permit's profile beside each request seen, in order: one entry per request, the
    /// profile or `None`.
    #[must_use]
    pub fn permits(&self) -> Vec<Option<String>> {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(_, permit)| permit.clone())
            .collect()
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

struct FaultBody {
    before: Cursor<Vec<u8>>,
    kind: WireKind,
    failed: bool,
}

impl Read for FaultBody {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let read = self.before.read(buf)?;
        if read != 0 || buf.is_empty() {
            return Ok(read);
        }
        if self.failed {
            return Ok(0);
        }
        self.failed = true;
        let kind = match self.kind {
            WireKind::Timeout | WireKind::Idle => std::io::ErrorKind::TimedOut,
            WireKind::Reset => std::io::ErrorKind::ConnectionReset,
            WireKind::Truncated => std::io::ErrorKind::UnexpectedEof,
            _ => std::io::ErrorKind::Other,
        };
        Err(std::io::Error::new(
            kind,
            format!("scripted {} body failure", self.kind.slug()),
        ))
    }
}

impl Transport for ScriptedTransport {
    fn round_trip(&self, req: WireRequest<'_>) -> Result<WireResponse, WireError> {
        let body = match req.body {
            WireBody::Empty => Vec::new(),
            WireBody::Bytes(bytes) => bytes.to_vec(),
            WireBody::Reader { len, reader } => {
                use std::io::Read as _;
                let mut bytes = Vec::new();
                std::io::Read::take(reader, len.saturating_add(1))
                    .read_to_end(&mut bytes)
                    .map_err(|e| local(&format!("scripted transport: the body: {e}")))?;
                bytes
            }
        };
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((
                SeenRequest {
                    method: req.method,
                    url: req.url,
                    headers: req.headers,
                    auth_profile: req.auth.map(|a| a.profile().to_string()),
                    auth_fingerprint: req.auth.map(|a| a.fingerprint().to_string()),
                    body,
                },
                req.permit.map(|permit| permit.profile().to_string()),
            ));
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
            Some(Scripted::BodyFault {
                status,
                headers,
                body,
                kind,
            }) => Ok(WireResponse {
                status,
                headers,
                body: Box::new(FaultBody {
                    before: Cursor::new(body),
                    kind,
                    failed: false,
                }),
            }),
        }
    }
}

impl super::sealed::Sealed for ScriptedTransport {}
