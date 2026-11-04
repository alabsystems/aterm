// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Strict records requests. File recovery uses the same frame reader, not this policy.

use super::*;
use crate::membership_log::store::framed::{FrameReadError, FrameReader};
use crate::membership_log::{MAX_RECORDS, MAX_RECORDS_STREAM, Record, RecordId};
use std::io::{Cursor, Read, Write};

pub(super) struct StagedRecords(Vec<Record>);

impl StagedRecords {
    pub(super) fn records(&self) -> &[Record] {
        &self.0
    }

    pub(super) fn into_records(self) -> Vec<Record> {
        self.0
    }

    /// A completed pull's selected immutable owners have already been verified.
    pub(super) fn from_verified(records: Vec<Record>) -> Self {
        Self(records)
    }
}

pub(super) fn known_record(shared: &Shared, name: &str, id: &RecordId) -> Option<Record> {
    // No file refresh here: acquisition must not change a cached slot.
    let state = shared.lock_server_state();
    let store = &state.records.get(name)?.store;
    store
        .log()
        .and_then(|log| log.get(id))
        .or_else(|| store.uncomplete(id))
        .cloned()
}

#[cfg(test)]
pub(super) fn from_bytes(
    shared: &Shared,
    name: &str,
    bytes: &[u8],
) -> Result<StagedRecords, Reply> {
    stage(shared, name, &mut Cursor::new(bytes), bytes.len())
}

pub(super) fn from_http(
    shared: &Shared,
    name: &str,
    stream: &mut TcpStream,
    request: Request,
    length: usize,
) -> Result<StagedRecords, Reply> {
    if request.expect_continue && request.early.len() < length {
        stream
            .write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
            .and_then(|()| stream.flush())
            .map_err(|_| Reply::text(400, "the connection closed"))?;
    }
    let mut body = HttpBody::new(stream, request.early, length, &shared.tuning);
    stage(shared, name, &mut body, length)
}

fn stage(
    shared: &Shared,
    name: &str,
    reader: &mut impl Read,
    length: usize,
) -> Result<StagedRecords, Reply> {
    stage_with(
        reader,
        length,
        |id| known_record(shared, name, id),
        || {
            #[cfg(test)]
            {
                let hook = shared
                    .verifying
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone();
                if let Some(hook) = hook {
                    hook();
                }
            }
        },
        || {
            #[cfg(test)]
            shared.post_verifies.fetch_add(1, Ordering::SeqCst);
        },
    )
}

fn stage_with(
    reader: &mut impl Read,
    length: usize,
    mut known: impl FnMut(&RecordId) -> Option<Record>,
    mut starting_verification: impl FnMut(),
    mut verifying: impl FnMut(),
) -> Result<StagedRecords, Reply> {
    if length > MAX_RECORDS_STREAM {
        return Err(Reply::text(400, "membership record stream is too large"));
    }
    if length == 0 {
        return Err(Reply::text(
            400,
            "a POST to /records needs at least one record",
        ));
    }
    let mut reader = reader.take(length as u64);
    let mut frames = FrameReader::new();
    let mut records = Vec::new();
    let mut raw_count = 0;
    let mut started = false;
    let failure = loop {
        if raw_count == MAX_RECORDS && reader.limit() > 0 {
            break Reply::text(400, "membership record stream exceeds record bound");
        }
        let frame = match frames.read(&mut reader) {
            Ok(None) if reader.limit() == 0 => return Ok(StagedRecords(records)),
            Ok(None) => return Err(Reply::text(400, "the body ended before its Content-Length")),
            Ok(Some(frame)) => frame,
            Err(FrameReadError::Read(error)) => return Err(read_failure(error)),
            Err(FrameReadError::Invalid(error)) => break Reply::text(400, error.to_string()),
        };
        raw_count += 1;
        let id = aterm_digest::Sha256::digest(&frame);
        if let Some(record) = known(&id) {
            records.push(record);
            continue;
        }
        if !started {
            starting_verification();
            started = true;
        }
        verifying();
        match Record::decode(&frame) {
            Ok(record) => records.push(record),
            Err(error) => break Reply::text(400, error.to_string()),
        }
    };
    drop(records);
    // Read the declared remainder before returning a semantic error. A short body or a
    // stalled connection keeps the precedence it had when bodies were collected whole.
    let mut drain = [0; 16 * 1024];
    loop {
        match reader.read(&mut drain) {
            Ok(0) if reader.limit() == 0 => return Err(failure),
            Ok(0) => return Err(Reply::text(400, "the body ended before its Content-Length")),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(read_failure(error)),
        }
    }
}

#[derive(Debug)]
struct BodyFailure {
    status: u16,
    reason: &'static str,
}

impl std::fmt::Display for BodyFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.reason)
    }
}

impl std::error::Error for BodyFailure {}

fn body_failure(status: u16, reason: &'static str) -> io::Error {
    io::Error::other(BodyFailure { status, reason })
}

fn read_failure(error: io::Error) -> Reply {
    match error
        .get_ref()
        .and_then(|e| e.downcast_ref::<BodyFailure>())
    {
        Some(failure) => Reply::text(failure.status, failure.reason),
        None => Reply::text(400, "the connection closed"),
    }
}

trait BodyStream: Read {
    fn body_now(&self) -> Instant {
        Instant::now()
    }
    fn read_wait(&mut self, wait: Duration) -> io::Result<()>;
}

impl BodyStream for TcpStream {
    fn read_wait(&mut self, wait: Duration) -> io::Result<()> {
        self.set_read_timeout(Some(wait))
    }
}

struct HttpBody<'a, S> {
    stream: &'a mut S,
    early: Cursor<Vec<u8>>,
    remaining: usize,
    started: Instant,
    deadline: Duration,
    stall: Duration,
}

impl<'a, S: BodyStream> HttpBody<'a, S> {
    fn new(stream: &'a mut S, early: Vec<u8>, length: usize, tuning: &Tuning) -> Self {
        Self {
            started: stream.body_now(),
            stream,
            early: Cursor::new(early),
            remaining: length,
            deadline: tuning.body_deadline,
            stall: tuning.body_stall,
        }
    }
}

impl<S: BodyStream> Read for HttpBody<'_, S> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 || bytes.is_empty() {
            return Ok(0);
        }
        let want = self.remaining.min(bytes.len()).min(64 * 1024);
        let early = self.early.read(&mut bytes[..want])?;
        if early > 0 {
            self.remaining -= early;
            return Ok(early);
        }
        let left = self
            .deadline
            .checked_sub(
                self.stream
                    .body_now()
                    .saturating_duration_since(self.started),
            )
            .filter(|left| !left.is_zero())
            .ok_or_else(|| body_failure(408, "the body took too long"))?;
        let wait = left.min(self.stall);
        self.stream
            .read_wait(wait)
            .map_err(|_| body_failure(400, "the connection closed"))?;
        match self.stream.read(&mut bytes[..want]) {
            Ok(0) => Err(body_failure(
                400,
                "the body ended before its Content-Length",
            )),
            Ok(n) => {
                self.remaining -= n;
                Ok(n)
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => Err(error),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                Err(body_failure(
                    408,
                    if wait == left {
                        "the body took too long"
                    } else {
                        "the body stalled"
                    },
                ))
            }
            Err(_) => Err(body_failure(400, "the connection closed")),
        }
    }
}

#[cfg(test)]
#[path = "records_input_tests.rs"]
mod tests;
