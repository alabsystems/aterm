// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_uds::{AcceptWait, AcceptedMode};

/// `ssh` runs the proxy with `/bin/sh -c exec …` after expanding its own `%` tokens: a path
/// with a blank, a quote or a `%` reaches the shell as one word and unexpanded.
#[test]
fn the_proxy_command_quotes_the_executable_for_the_shell_and_ssh() {
    assert_eq!(
        command(
            Path::new("/Applications/aterm (dev).app/Contents/MacOS/aterm"),
            Duration::from_secs(5)
        ),
        "'/Applications/aterm (dev).app/Contents/MacOS/aterm' lan fdpass %h %p 5"
    );
    assert_eq!(
        command(Path::new("/x/it's 100%/aterm"), Duration::from_millis(10)),
        "'/x/it'\\''s 100%%/aterm' lan fdpass %h %p 1"
    );
    let all: Vec<String> = options(Duration::from_secs(15))
        .iter()
        .map(|o| o.to_string_lossy().into_owned())
        .collect();
    assert_eq!(all[0], "-o");
    assert!(all[1].starts_with("ProxyCommand='"), "{all:?}");
    assert!(all[1].ends_with("' lan fdpass %h %p 15"), "{all:?}");
    assert_eq!(all[2..], ["-o", "ProxyUseFdpass=yes"]);
}

/// A connect is passed to `ssh` as `ProxyUseFdpass` takes it: one byte and the connected
/// descriptor, which reaches the listener. A destination nobody listens on fails in the
/// words `ssh` uses, so the controller's reading of `ssh` holds.
#[test]
fn a_connection_is_passed_on_and_a_failure_is_worded_as_ssh_words_it() {
    use std::io::{Read as _, Write as _};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (ours, theirs) = aterm_uds::CtlStream::pair().unwrap();
    fdpass("127.0.0.1", port, Duration::from_secs(60), &ours).unwrap();
    let mut byte = [0u8; 1];
    let got = aterm_uds::fdpass::recv_with_fds(&theirs, &mut byte, 1).unwrap();
    assert_eq!((got.bytes, byte), (1, [0]));
    let mut passed = std::net::TcpStream::from(got.fds.into_iter().next().expect("a socket"));
    let (mut accepted, _) =
        aterm_uds::accept(&listener, AcceptWait::Forever, AcceptedMode::Blocking).unwrap();
    passed.write_all(b"SSH-2.0-x\r\n").unwrap();
    let mut heard = [0u8; 11];
    accepted.read_exact(&mut heard).unwrap();
    assert_eq!(&heard, b"SSH-2.0-x\r\n");

    // Nobody listens on the port the passed connection holds, and while it holds it no other
    // test can bind it, as a sibling running alongside could take the listener's own port
    // once dropped.
    let unheard = passed.local_addr().unwrap().port();
    let why = fdpass("127.0.0.1", unheard, Duration::from_secs(60), &ours).unwrap_err();
    assert_eq!(
        why,
        format!("ssh: connect to host 127.0.0.1 port {unheard}: Connection refused")
    );
    assert!(why.contains(super::super::ssh::CONNECTION_REFUSED), "{why}");
}

#[test]
fn a_failure_without_an_errno_reads_as_ssh_words_it() {
    assert_eq!(
        strerror(&io::Error::from(io::ErrorKind::TimedOut)),
        "Operation timed out"
    );
    assert_eq!(
        strerror(&io::Error::from_raw_os_error(libc::EHOSTUNREACH)),
        "No route to host"
    );
}

/// The arguments `ProxyCommand` passes are checked before anything is dialled.
#[test]
fn arguments_that_do_not_parse_are_refused() {
    assert!(
        serve("h", "port", "5")
            .unwrap_err()
            .starts_with(BROKER_SAID)
    );
    assert!(serve("h", "22", "0").unwrap_err().starts_with(BROKER_SAID));
}
