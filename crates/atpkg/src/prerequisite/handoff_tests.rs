// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn custody_acknowledgement_requires_the_exact_byte_before_deadline() {
    for (reply, accepted) in [(Some(b'K'), true), (Some(b'X'), false), (None, false)] {
        let (mut parent, child) = aterm_uds::CtlStream::pair().unwrap();
        if let Some(reply) = reply {
            parent.write_all(&[reply]).unwrap();
        }
        drop(parent);
        assert_eq!(
            acknowledge(
                &child,
                std::time::Instant::now() + std::time::Duration::from_secs(1)
            )
            .is_ok(),
            accepted
        );
    }
    let (_parent, child) = aterm_uds::CtlStream::pair().unwrap();
    assert!(acknowledge(&child, std::time::Instant::now()).is_err());
}

#[test]
fn a_socket_owned_by_another_process_cannot_request_admission() {
    let directory = aterm_tempfile::Builder::new()
        .prefix("handoff-peer-")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .unwrap();
    let socket = directory.path().join("s");
    let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    // This listener belongs to this test process, not command()'s parent.
    // The refusal must precede configuration, package lookup or network work.
    let error = command(&[
        "--tool".into(),
        "codex=42".into(),
        "--socket".into(),
        socket.to_str().unwrap().into(),
    ])
    .unwrap_err();
    assert!(
        error.contains("not owned by the original parent"),
        "{error}"
    );
}

#[test]
fn request_requires_an_exact_signed_tool_and_an_absolute_socket() {
    let args = |values: &[&str]| values.iter().map(ToString::to_string).collect::<Vec<_>>();
    let (want, build, socket) =
        request(&args(&["--tool", "codex=42", "--socket", "/private/s"])).unwrap();
    assert_eq!(
        (want, build, socket),
        (
            super::super::Prerequisite::Codex,
            42,
            PathBuf::from("/private/s")
        )
    );
    for values in [
        vec!["--tool", "codex", "--socket", "/private/s"],
        vec!["--tool", "codex=0", "--socket", "/private/s"],
        vec!["--tool", "unmanaged=42", "--socket", "/private/s"],
        vec!["--tool", "codex=42", "--socket", "relative"],
        vec!["--tool", "codex=42", "--socket", "/private/s", "extra"],
    ] {
        assert!(request(&args(&values)).is_err(), "{values:?}");
    }
}
