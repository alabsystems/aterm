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
        let (reader, writer) = aterm_uds::kept_out(io::pipe)?;
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
mod tests {
    use super::*;
    use std::io::Write as _;

    fn assert_closed(fd: libc::c_int) {
        assert_eq!(unsafe { libc::fcntl(fd, libc::F_GETFD) }, -1);
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EBADF));
    }

    fn low_descriptors() -> usize {
        (0..256)
            .filter(|fd| unsafe { libc::fcntl(*fd, libc::F_GETFD) } >= 0)
            .count()
    }

    #[test]
    fn closed_capture_observations_keep_all_128_prefixes_without_reader_descriptors() {
        if !aterm_uds::test_binary::isolated_lifecycle(
            "actor::owner::bootstrap_query::capture::tests::closed_capture_observations_keep_all_128_prefixes_without_reader_descriptors",
        ) {
            return;
        }
        // A complete actual pipe initializes the same descriptor path before
        // measurement. No OS limit, concurrency or selected-input owner changes.
        let (mut warm, writer) = Capture::pipe().unwrap();
        drop(writer);
        warm.poll().unwrap();
        drop(warm.into_observation());
        let before = low_descriptors();
        let mut observations = Vec::new();
        for index in 0..128_u8 {
            let (mut capture, mut writer) = Capture::pipe().unwrap();
            let fd = capture.reader.as_raw_fd();
            assert!(
                fd < 256,
                "this bounded native control measures the unchanged low table"
            );
            let bytes = [index, 0, 255, b'\n'];
            writer.write_all(&bytes).unwrap();
            drop(writer);
            capture.poll().unwrap();
            assert!(capture.complete());
            let observation = capture.into_observation();
            assert_closed(fd);
            assert_eq!(observation.prefix, bytes);
            assert_eq!(observation.bytes_seen, Some(bytes.len() as u64));
            assert!(observation.eof && !observation.failed);
            observations.push(observation);
            assert_eq!(low_descriptors(), before);
        }
        assert_eq!(observations.len(), 128);
        assert_eq!(low_descriptors(), before);
        for (index, observation) in observations.into_iter().enumerate() {
            assert_eq!(observation.prefix, [index as u8, 0, 255, b'\n']);
        }
    }

    #[test]
    fn actual_child_owning_wait_and_pipe_eof_preserve_bytes_after_reader_close() {
        if !aterm_uds::test_binary::isolated_lifecycle(
            "actor::owner::bootstrap_query::capture::tests::actual_child_owning_wait_and_pipe_eof_preserve_bytes_after_reader_close",
        ) {
            return;
        }
        // This is a real native owning-wait/output control, not an SDK Keeper,
        // NoForkTarget, paid Epoch borrower, Host completion or Ready fixture.
        let (mut capture, writer) = Capture::pipe().unwrap();
        let fd = capture.reader.as_raw_fd();
        let mut command = aterm_uds::spawnfd::command("/usr/bin/printf");
        command
            .env_clear()
            .arg(r"native\000\377")
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(aterm_uds::spawnfd::Stdio::from(writer))
            .stderr(aterm_uds::spawnfd::Stdio::null());
        let mut child = command.spawn().unwrap();
        drop(command);
        let actual = child.wait_with_rusage().unwrap();
        assert!(actual.status.success() && child.is_reaped());
        capture.poll().unwrap();
        assert!(capture.complete());
        let observation = capture.into_observation();
        assert_closed(fd);
        assert_eq!(observation.prefix, b"native\0\xff");
        assert_eq!(observation.bytes_seen, Some(8));
        assert!(observation.eof && !observation.failed);
        assert!(
            actual.status.success(),
            "actual wait status remains separate from FD cleanup"
        );
    }

    #[test]
    fn overflow_snapshot_preserves_seen_total_eof_and_failed_prefix() {
        let (mut capture, mut writer) = Capture::pipe().unwrap();
        let chunk = [0x67; wire::MAX_CHUNK];
        for _ in 0..wire::MAX_OUTPUT as usize / chunk.len() {
            writer.write_all(&chunk).unwrap();
            capture.poll().unwrap();
        }
        writer.write_all(b"x").unwrap();
        assert!(capture.poll().is_err());
        drop(writer);
        capture.poll().unwrap();
        let observation = capture.into_observation();
        assert_eq!(observation.bytes_seen, Some(wire::MAX_OUTPUT + 1));
        assert_eq!(observation.prefix, vec![0x67; wire::MAX_OUTPUT as usize]);
        assert!(observation.eof && observation.failed);
    }
}
