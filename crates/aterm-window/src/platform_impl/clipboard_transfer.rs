// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Added by the aterm project in 2026; see the repository NOTICE.

//! Nonblocking clipboard pipe transfers and their completion policy.

use std::io::{Read, Write};
use std::time::{Duration, Instant};

/// The hang detector for a required clipboard transfer.
pub(super) const TRANSFER_HANG: Duration = Duration::from_secs(60);

/// Bound memory even when a foreign source never finishes its text.
const MAX_PASTE_BYTES: usize = 16 << 20;

/// Read a nonblocking pipe to EOF, or to the explicit text size limit.
pub(super) fn read(pipe: impl Read) -> Option<String> {
    read_after(pipe, Duration::ZERO)
}

fn read_after(pipe: impl Read, elapsed: Duration) -> Option<String> {
    let started = Instant::now();
    read_with_elapsed(pipe, || elapsed.saturating_add(started.elapsed()))
}

fn read_with_elapsed(mut pipe: impl Read, mut elapsed: impl FnMut() -> Duration) -> Option<String> {
    let mut bytes = Vec::new();
    let mut truncated = false;
    let mut buf = [0u8; 8192];
    loop {
        if elapsed() >= TRANSFER_HANG {
            return None;
        }
        match pipe.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                bytes.extend_from_slice(&buf[..n]);
                if bytes.len() >= MAX_PASTE_BYTES {
                    bytes.truncate(MAX_PASTE_BYTES);
                    truncated = true;
                    break;
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(2));
            },
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {},
            Err(_) => return None,
        }
    }
    decode_paste_bytes(bytes, truncated)
}

/// Send the entire selection on a nonblocking pipe.
///
/// # Errors
/// A hung or broken transfer, a zero-byte write, or a flush failure.
pub(super) fn write(pipe: impl Write, bytes: &[u8]) -> std::io::Result<()> {
    write_after(pipe, bytes, Duration::ZERO)
}

fn write_after(mut pipe: impl Write, bytes: &[u8], elapsed: Duration) -> std::io::Result<()> {
    let started = Instant::now();
    let mut at = 0;
    while at < bytes.len() {
        if elapsed.saturating_add(started.elapsed()) >= TRANSFER_HANG {
            return Err(std::io::ErrorKind::TimedOut.into());
        }
        match pipe.write(&bytes[at..]) {
            Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
            Ok(n) => at += n,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(2));
            },
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {},
            Err(error) => return Err(error),
        }
    }
    pipe.flush()
}

fn decode_paste_bytes(bytes: Vec<u8>, truncated: bool) -> Option<String> {
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(error) if truncated && error.utf8_error().error_len().is_none() => {
            let valid_len = error.utf8_error().valid_up_to();
            let mut bytes = error.into_bytes();
            bytes.truncate(valid_len);
            String::from_utf8(bytes).ok()?
        },
        Err(_) => return None,
    };
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[cfg(unix)]
    fn a_scheduled_foreign_source_supplies_the_whole_selection() {
        let (mut source, mut destination) = std::os::unix::net::UnixStream::pair().unwrap();
        source.set_nonblocking(true).unwrap();
        destination.write_all(b"before after").unwrap();
        drop(destination);
        let actual = read_after(&mut source, Duration::from_secs(4));
        assert_eq!(actual.as_deref(), Some("before after"));
    }

    #[test]
    #[cfg(unix)]
    fn an_unfinished_native_transfer_never_returns_a_valid_prefix_as_complete() {
        struct AfterRead<'a> {
            pipe: &'a mut std::os::unix::net::UnixStream,
            started: &'a std::cell::Cell<Instant>,
        }
        impl Read for AfterRead<'_> {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                let result = self.pipe.read(buffer);
                self.started.set(Instant::now() - Duration::from_secs(61));
                result
            }
        }
        let (mut source, mut destination) = std::os::unix::net::UnixStream::pair().unwrap();
        source.set_nonblocking(true).unwrap();
        destination.write_all(b"prefix").unwrap();
        let started = std::cell::Cell::new(Instant::now() - Duration::from_secs(4));
        let actual = read_with_elapsed(AfterRead { pipe: &mut source, started: &started }, || {
            started.get().elapsed()
        });
        assert_eq!(actual, None);
        destination.write_all(b"tail").unwrap();
        let mut tail = [0; 4];
        source.read_exact(&mut tail).unwrap();
        assert_eq!(&tail, b"tail");

        assert_eq!(read_after(&mut source, Duration::from_secs(61)), None);
    }

    #[test]
    fn an_io_failure_cannot_certify_the_prefix_read_before_it() {
        let mut first = true;
        let mut reader = std::io::Cursor::new(b"prefix");
        struct Broken<'a> {
            first: &'a mut bool,
            reader: &'a mut std::io::Cursor<&'static [u8; 6]>,
        }
        impl Read for Broken<'_> {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                if std::mem::take(self.first) {
                    self.reader.read(buffer)
                } else {
                    Err(std::io::ErrorKind::BrokenPipe.into())
                }
            }
        }
        assert_eq!(read(Broken { first: &mut first, reader: &mut reader }), None);
    }

    #[test]
    fn only_the_explicit_size_cap_allows_an_incomplete_final_codepoint() {
        assert_eq!(decode_paste_bytes(b"hello\xE4\xB8".to_vec(), true).as_deref(), Some("hello"));
        assert_eq!(decode_paste_bytes(b"hello\xE4\xB8".to_vec(), false), None);
        assert_eq!(decode_paste_bytes(b"he\xFFllo\xE4".to_vec(), true), None);
        assert_eq!(decode_paste_bytes(b"\xE4\xB8".to_vec(), true), None);
        let mut capped = vec![b'a'; MAX_PASTE_BYTES - 1];
        capped.extend_from_slice("中tail".as_bytes());
        let text = read(std::io::Cursor::new(capped)).unwrap();
        assert_eq!(text.len(), MAX_PASTE_BYTES - 1);
        assert!(text.bytes().all(|byte| byte == b'a'));
    }

    #[test]
    #[cfg(unix)]
    fn a_scheduled_requester_receives_the_entire_native_selection() {
        let (mut source, mut destination) = std::os::unix::net::UnixStream::pair().unwrap();
        source.set_nonblocking(true).unwrap();
        destination.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
        let receiver = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            destination.read_to_end(&mut bytes).unwrap();
            bytes
        });
        let bytes = vec![0x52; 2 << 20];
        let actual = write_after(&mut source, &bytes, Duration::from_secs(4));
        drop(source);
        let received = receiver.join().unwrap();
        assert_eq!(received, bytes);
        actual.unwrap();
    }

    #[test]
    fn only_completed_output_may_report_success() {
        let mut completed = Vec::new();
        write(&mut completed, b"before ").unwrap();
        write_after(&mut completed, b"after", Duration::from_secs(4)).unwrap();
        assert_eq!(completed, b"before after");
        let mut destination = Vec::new();
        assert_eq!(
            write_after(&mut destination, b"not sent", Duration::from_secs(61)).unwrap_err().kind(),
            std::io::ErrorKind::TimedOut
        );
        assert!(destination.is_empty());
    }
}
