// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Added by the aterm project in 2026; see the repository NOTICE.

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
