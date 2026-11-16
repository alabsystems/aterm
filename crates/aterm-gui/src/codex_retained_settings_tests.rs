// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
use super::*;
use std::os::unix::ffi::OsStrExt as _;

#[test]
fn settings_frame_preserves_raw_values_and_refuses_truncation_or_trailing_bytes() {
    let frame = b"ALABSET1\0\0\0\x17\0\0\0\x01\0\0\0\x01x\0\0\0\x01\0\0\0\x01K\0\0\0\x01\xff";
    let settings = Settings::read(&mut frame.as_slice()).unwrap();
    assert_eq!(settings.arguments, ["x"]);
    assert_eq!(settings.environment[0].0.as_bytes(), b"K");
    assert_eq!(settings.environment[0].1.as_bytes(), b"\xff");
    for len in 0..frame.len() {
        assert!(Settings::read(&mut &frame[..len]).is_err());
    }
    let mut trailing = frame.to_vec();
    trailing.push(1);
    assert!(Settings::read(&mut trailing.as_slice()).is_err());
    let mut invalid_key = frame.to_vec();
    invalid_key[29] = b'=';
    assert!(Settings::read(&mut invalid_key.as_slice()).is_err());
}

#[test]
fn settings_frame_preserves_non_utf8_and_empty_arguments() {
    let frame = b"ALABSET1\0\0\0\x1c\0\0\0\x02\0\0\0\x0c/workspace/\xff\0\0\0\0\0\0\0\0";
    let settings = Settings::read(&mut frame.as_slice()).unwrap();
    assert_eq!(settings.arguments[0].as_bytes(), b"/workspace/\xff");
    assert!(settings.arguments[1].is_empty());
    assert!(settings.environment.is_empty());
}

#[test]
fn settings_frame_refuses_invalid_counts_duplicate_keys_and_nul_values() {
    fn frame(body: &[u8]) -> Vec<u8> {
        let mut frame = b"ALABSET1".to_vec();
        frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
        frame.extend_from_slice(body);
        frame
    }
    for body in [
        b"\xff\xff\xff\xff\0\0\0\0".as_slice(),
        b"\0\0\0\0\xff\xff\xff\xff".as_slice(),
        b"\0\0\0\0\0\0\0\x02\0\0\0\x01K\0\0\0\x01a\0\0\0\x01K\0\0\0\x01b".as_slice(),
        b"\0\0\0\0\0\0\0\x01\0\0\0\x01K\0\0\0\x01\0".as_slice(),
        b"\0\0\0\0\0\0\0\x01\0\0\0\0\0\0\0\0".as_slice(),
    ] {
        assert!(Settings::read(&mut frame(body).as_slice()).is_err());
    }
    let mut oversized = b"ALABSET1".to_vec();
    oversized.extend_from_slice(&((LIMIT + 1) as u32).to_be_bytes());
    assert!(Settings::read(&mut oversized.as_slice()).is_err());
}

#[test]
fn settings_socket_drains_the_complete_frame_after_the_writer_closed() {
    use std::io::Write as _;
    let (mut writer, reader) = aterm_uds::kept_out(aterm_uds::CtlStream::pair).unwrap();
    let frame = b"ALABSET1\0\0\0\x08\0\0\0\0\0\0\0\0";
    writer.write_all(frame).unwrap();
    drop(writer); // The actual peer has closed before admission starts reading.
    let settings = Settings::read_socket(
        &reader,
        std::time::Instant::now() + std::time::Duration::from_secs(10),
    )
    .unwrap();
    assert!(settings.arguments.is_empty());
    assert!(settings.environment.is_empty());
}

#[test]
fn expired_settings_deadline_cannot_accept_even_a_buffered_frame() {
    use std::io::Write as _;
    let (mut writer, reader) = aterm_uds::kept_out(aterm_uds::CtlStream::pair).unwrap();
    writer
        .write_all(b"ALABSET1\0\0\0\x08\0\0\0\0\0\0\0\0")
        .unwrap();
    drop(writer);
    assert_eq!(
        Settings::read_socket(&reader, std::time::Instant::now())
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::TimedOut
    );
}
