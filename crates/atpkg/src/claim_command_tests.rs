// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
use super::*;
use std::{
    io::{Read as _, Write as _},
    os::{
        fd::{AsRawFd as _, IntoRawFd as _},
        unix::{ffi::OsStringExt as _, net::UnixStream},
    },
};

fn argv(extra: &[&str]) -> Vec<OsString> {
    let mut args = [
        "--what",
        "train compile",
        "--key",
        "trust-train:compile",
        "--checkout",
        "/private/owned/source",
        "--rank",
        "release",
        "--cores",
        "6",
        "--status-fd",
        "9",
    ]
    .map(OsString::from)
    .to_vec();
    args.extend(extra.iter().map(OsString::from));
    args.extend(["--", "/bin/echo", "literal payload"].map(OsString::from));
    args
}

fn words(args: &[OsString]) -> ClaimWords {
    let Words::Exec(words) = parse_words(args).unwrap() else {
        panic!("expected exec")
    };
    words
}

#[test]
fn public_claim_wire_preserves_zero_memory_wait_and_arrival_semantics() {
    let base = words(&argv(&[]));
    assert_eq!(base.cores, 6);
    assert_eq!(base.rank, aterm_capacity::Rank::Release);
    assert_eq!(base.mem_bytes, None);
    assert_eq!(base.wait_ms, None);
    let zero = words(&argv(&["--mem-bytes", "0", "--wait-ms", "0"]));
    assert_eq!(zero.mem_bytes, Some(0));
    assert_eq!(zero.wait_ms, Some(0));
    let retry = words(&argv(&["--no-wait", "--since-ms", "1234", "--alone"]));
    assert!(retry.no_wait && retry.alone);
    assert_eq!(retry.since_ms, Some(1234));
    assert!(parse_words(&argv(&["--since-ms", "1234"])).is_err());
    assert!(parse_words(&argv(&["--no-wait", "--wait-ms", "0"])).is_err());
}

#[test]
fn capability_is_exact_and_payload_bytes_are_not_options_or_lossy_text() {
    assert_eq!(
        parse_words(&["--capabilities".into()]).unwrap(),
        Words::Capabilities
    );
    assert!(parse_words(&["--capabilities".into(), "--cores".into(), "1".into()]).is_err());
    let mut raw = argv(&[]);
    raw.pop();
    raw.push(OsString::from_vec(b"opaque-\xff".to_vec()));
    raw.push("--capabilities".into());
    raw.push("--help".into());
    let parsed = words(&raw);
    assert_eq!(parsed.command[1].as_bytes(), b"opaque-\xff");
    assert_eq!(
        parsed.command[2..],
        [OsString::from("--capabilities"), OsString::from("--help")]
    );
    let mut checkout = argv(&[]);
    checkout[5] = OsString::from_vec(b"/private/owned/\xff".to_vec());
    assert_eq!(
        words(&checkout).checkout.as_os_str().as_bytes(),
        b"/private/owned/\xff"
    );
}

#[test]
fn help_is_an_exact_command_not_a_label_or_payload_word() {
    assert!(help_command(&["--help".into()]));
    assert!(help_command(&["-h".into()]));
    let mut args = argv(&[]);
    args[1] = "--help".into();
    assert!(!help_command(&args));
    assert_eq!(words(&args).what, "--help");
    args[1] = "-h".into();
    assert!(!help_command(&args));
    assert_eq!(words(&args).what, "-h");
    args.push("--help".into());
    assert!(!help_command(&args));
    assert_eq!(words(&args).command.last().unwrap(), "--help");
}

#[test]
fn ambiguous_missing_and_invalid_claim_words_refuse_before_fd_ownership() {
    for extra in [
        vec!["--cores", "1"],
        vec!["--mem-bytes", "1", "--mem-bytes", "2"],
        vec!["--alone", "--alone"],
        vec!["--no-wait", "--no-wait"],
        vec!["--status-fd", "4"],
        vec!["--flock-fd", "9"],
        vec!["--unknown", "x"],
    ] {
        assert!(parse_words(&argv(&extra)).is_err(), "{extra:?}");
    }
    for (flag, values) in [
        ("--cores", vec!["0", "+1", "-1", "4294967296"]),
        ("--mem-bytes", vec!["-1", "18446744073709551616"]),
        ("--status-fd", vec!["0", "2", "2147483648"]),
        ("--rank", vec!["background", "Release"]),
    ] {
        for value in values {
            let mut args = argv(&[]);
            if let Some(at) = args.iter().position(|word| word == flag) {
                args[at + 1] = value.into();
            } else {
                args = argv(&[flag, value]);
            }
            assert!(parse_words(&args).is_err(), "{flag} {value}");
        }
    }
    for required in [
        "--what",
        "--key",
        "--checkout",
        "--rank",
        "--cores",
        "--status-fd",
    ] {
        let mut args = argv(&[]);
        let at = args.iter().position(|word| word == required).unwrap();
        args.drain(at..at + 2);
        assert!(parse_words(&args).is_err(), "missing {required}");
    }
    let mut no_program = argv(&[]);
    no_program.truncate(no_program.iter().position(|word| word == "--").unwrap() + 1);
    assert!(parse_words(&no_program).is_err());
    let mut not_text = argv(&[]);
    not_text[1] = OsString::from_vec(vec![0xff]);
    assert!(parse_words(&not_text).is_err());
}

fn inherited_for_test(fd: RawFd) {
    // SAFETY: tests own this live descriptor, under kept_out's spawn lock.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    assert!(flags >= 0);
    // SAFETY: change only this test-owned original's descriptor flags.
    assert_eq!(
        unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) },
        0
    );
}

#[test]
fn fd_validation_refuses_fresh_reused_closed_wrong_direction_and_wrong_types() {
    aterm_uds::kept_out(|| -> io::Result<std::io::PipeReader> {
        let (reader, writer) = aterm_uds::pipe()?;
        let file = aterm_tempfile::tempfile()?;
        let mut parsed = words(&argv(&[]));
        parsed.status_fd = writer.as_raw_fd();
        assert!(
            validate_fds(&parsed).is_err(),
            "new CLOEXEC FD is not inherited"
        );
        inherited_for_test(writer.as_raw_fd());
        validate_fds(&parsed)?;
        inherited_for_test(reader.as_raw_fd());
        parsed.status_fd = reader.as_raw_fd();
        assert!(
            validate_fds(&parsed).is_err(),
            "read end cannot carry status"
        );
        inherited_for_test(file.as_raw_fd());
        parsed.status_fd = file.as_raw_fd();
        assert!(
            validate_fds(&parsed).is_err(),
            "regular file cannot signal reader loss"
        );
        parsed.status_fd = writer.as_raw_fd();
        parsed.flock_fd = Some(reader.as_raw_fd());
        assert!(validate_fds(&parsed).is_err(), "flock needs a regular file");
        parsed.flock_fd = Some(file.as_raw_fd());
        validate_fds(&parsed)?;
        parsed.status_fd = i32::MAX;
        assert!(validate_fds(&parsed).is_err());
        Ok(reader)
    })
    .unwrap();
}

#[test]
fn successful_parse_takes_only_explicitly_transferred_originals_after_all_checks() {
    aterm_uds::kept_out(|| -> io::Result<UnixStream> {
        let (mut reader, writer) = UnixStream::pair()?;
        let lock = aterm_tempfile::tempfile()?;
        inherited_for_test(writer.as_raw_fd());
        inherited_for_test(lock.as_raw_fd());
        let status_fd = writer.into_raw_fd();
        let flock_fd = lock.into_raw_fd();
        let mut args = argv(&["--flock-fd", &flock_fd.to_string()]);
        let at = args.iter().position(|word| word == "--status-fd").unwrap();
        args[at + 1] = status_fd.to_string().into();
        let ClaimCommand::Exec(mut parsed) = parse(&args).unwrap() else {
            panic!("expected exec")
        };
        assert_eq!(parsed.status.as_raw_fd(), status_fd);
        assert_eq!(parsed.flock.as_ref().unwrap().as_raw_fd(), flock_fd);
        parsed.status.write_all(b"one")?;
        drop(parsed);
        reader.set_nonblocking(true)?;
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes)?;
        assert_eq!(
            bytes, b"one",
            "original status writer closed without a leaked duplicate"
        );
        Ok(reader)
    })
    .unwrap();
}
