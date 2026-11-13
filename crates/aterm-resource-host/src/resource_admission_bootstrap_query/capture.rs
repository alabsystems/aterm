// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Same original native pipes, bounded retained prefixes, and physical EOF.
//! A reader does not wait/reap a child and its EOF does not retire a resource.

use super::{refused, wire};
use std::{
    io::{self, Read as _},
    os::fd::AsRawFd as _,
};

pub(super) struct Capture {
    reader: io::PipeReader,
    prefix: Vec<u8>,
    seen: Option<u64>,
    eof: bool,
    failed: bool,
}

/// Captured DATA after the actual owner closes this query. It is not a
/// stream/child/native-closure capability and has no descriptor.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::actor::owner) struct CaptureObservation {
    pub(in crate::actor::owner) prefix: Vec<u8>,
    pub(in crate::actor::owner) bytes_seen: Option<u64>,
    pub(in crate::actor::owner) eof: bool,
    pub(in crate::actor::owner) failed: bool,
}
impl Capture {
    pub(super) fn pipe() -> io::Result<(Self, io::PipeWriter)> {
        let (reader, writer) = aterm_uds::pipe()?;
        // This description is still private and is never inherited by the
        // child. Changing its flags cannot change the target's write OFD.
        let flags = unsafe { libc::fcntl(reader.as_raw_fd(), libc::F_GETFL) };
        if flags < 0
            || unsafe { libc::fcntl(reader.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) }
                < 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok((
            Self {
                reader,
                prefix: Vec::new(),
                seen: Some(0),
                eof: false,
                failed: false,
            },
            writer,
        ))
    }

    /// Bound one actor turn independently of how much a failed child writes.
    /// After overflow/error retain the original prefix and continue draining
    /// on later turns; no truncated output can become a positive transcript.
    pub(super) fn poll(&mut self) -> io::Result<()> {
        if self.eof {
            return Ok(());
        }
        let mut bytes = [0; wire::MAX_CHUNK];
        for _ in 0..16 {
            match self.reader.read(&mut bytes) {
                Ok(0) => {
                    self.eof = true;
                    return Ok(());
                }
                Ok(count) => {
                    self.seen = self.seen.and_then(|seen| seen.checked_add(count as u64));
                    if self.seen.is_none() {
                        self.failed = true;
                    }
                    let remaining = (wire::MAX_OUTPUT as usize).saturating_sub(self.prefix.len());
                    self.prefix
                        .extend_from_slice(&bytes[..count.min(remaining)]);
                    if count > remaining {
                        self.failed = true;
                        return Err(refused("Bootstrap query output exceeds its closed bound"));
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(error) => {
                    self.failed = true;
                    return Err(error);
                }
            }
        }
        Ok(())
    }
    pub(super) fn complete(&self) -> bool {
        self.eof && !self.failed
    }
    pub(super) fn at_eof(&self) -> bool {
        self.eof
    }
    #[cfg(test)]
    pub(super) fn prefix(&self) -> &[u8] {
        &self.prefix
    }
    pub(super) fn into_observation(self) -> CaptureObservation {
        let Self {
            reader,
            prefix,
            seen,
            eof,
            failed,
        } = self;
        drop(reader);
        CaptureObservation {
            prefix,
            bytes_seen: seen,
            eof,
            failed,
        }
    }
    pub(super) fn append_frames(
        &self,
        stream: wire::Stream,
        request: &wire::Request,
        digest: [u8; 32],
        frames: &mut Vec<Vec<u8>>,
    ) -> io::Result<()> {
        if !self.complete() {
            return Err(refused(
                "Bootstrap query lacks complete original stream EOF",
            ));
        }
        let mut offset = 0;
        for bytes in self.prefix.chunks(wire::MAX_CHUNK) {
            frames.push(
                wire::Event::Output {
                    stream,
                    offset,
                    bytes: bytes.to_vec(),
                }
                .encode(request, digest)?,
            );
            offset += bytes.len() as u64;
        }
        frames.push(
            wire::Event::Eof {
                stream,
                bytes: offset,
            }
            .encode(request, digest)?,
        );
        Ok(())
    }
}

#[cfg(test)]
#[path = "capture_tests.rs"]
mod tests;
