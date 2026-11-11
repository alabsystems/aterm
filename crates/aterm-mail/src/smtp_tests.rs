// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_uds::{AcceptWait, AcceptedMode};
#[test]
fn smtp_submission_and_uncertain_acknowledgement() {
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};
    for acknowledge in [true, false] {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (socket, _) =
                aterm_uds::accept(&listener, AcceptWait::Forever, AcceptedMode::Blocking).unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(60)))
                .unwrap();
            socket
                .set_write_timeout(Some(std::time::Duration::from_secs(60)))
                .unwrap();
            let mut io = BufReader::new(socket);
            io.get_mut().write_all(b"220 fixture\r\n").unwrap();
            for (prefix, response) in [
                ("EHLO ", "250 fixture\r\n"),
                ("AUTH PLAIN ", "235 authenticated\r\n"),
                ("MAIL FROM:", "250 accepted\r\n"),
                ("RCPT TO:", "250 accepted\r\n"),
                ("RCPT TO:", "250 accepted\r\n"),
                ("DATA", "354 continue\r\n"),
            ] {
                let mut line = String::new();
                io.read_line(&mut line).unwrap();
                assert!(line.starts_with(prefix));
                io.get_mut().write_all(response.as_bytes()).unwrap();
            }
            let mut data = String::new();
            loop {
                let mut line = String::new();
                assert!(io.read_line(&mut line).unwrap() > 0);
                if line == ".\r\n" {
                    break;
                }
                data.push_str(&line);
            }
            assert!(!data.contains("hidden@example.test"));
            assert!(data.contains("Message-ID: <"));
            if acknowledge {
                io.get_mut().write_all(b"250 queued\r\n").unwrap();
            }
        });
        let socket = TcpStream::connect(address).unwrap();
        let auth = Authentication::Password {
            username: "me@example.test".into(),
            password: crate::Secret::new("test-only".into()),
        };
        let draft = Draft {
            from: "me@example.test".into(),
            to: vec!["you@example.test".into()],
            bcc: vec!["hidden@example.test".into()],
            subject: "Hello".into(),
            text: "Hello".into(),
            ..Draft::default()
        };
        let result = submit(
            Wire::for_test(socket),
            Security::Tls,
            &auth,
            &encode(&draft).unwrap(),
            &draft,
        );
        if acknowledge {
            assert!(result.is_ok());
        } else {
            assert!(result.unwrap_err().contains("Check Sent"));
        }
        server.join().unwrap();
    }
}
#[test]
fn dates_are_utc_rfc5322() {
    assert_eq!(mail_date(0).unwrap(), "Thu, 01 Jan 1970 00:00:00 +0000");
}
#[test]
fn bcc_is_only_an_envelope_recipient_and_headers_cannot_be_injected() {
    let mut draft = Draft {
        from: "me@example.test".into(),
        to: vec!["you@example.test".into()],
        bcc: vec!["hidden@example.test".into()],
        subject: "Hello".into(),
        text: ".\nHello".into(),
        ..Draft::default()
    };
    let encoded = String::from_utf8(encode(&draft).unwrap()).unwrap();
    assert!(!encoded.contains("hidden@example"));
    assert!(!encoded.contains("\r\n.\r\n"));
    draft.subject = "Hello\r\nBcc: bad@example.test".into();
    assert!(encode(&draft).is_err());

    // Bcc-only / Cc-only drafts omit an empty `To:` header rather than emitting invalid `To: \r\n`.
    let bcc_only = Draft {
        from: "me@example.test".into(),
        bcc: vec!["hidden@example.test".into()],
        subject: "Notice".into(),
        text: "Body".into(),
        ..Draft::default()
    };
    let bcc_encoded = String::from_utf8(encode(&bcc_only).unwrap()).unwrap();
    assert!(!bcc_encoded.contains("\r\nTo:"));
    assert!(!bcc_encoded.contains("hidden@example"));
}

#[test]
fn smtp_two_step_oauth_bare_reply_codes_and_oauth_error_challenge() {
    use std::io::{BufRead, BufReader, Write};
    use std::net::{Ipv4Addr, TcpListener, TcpStream};

    let long_token = "t".repeat(600);
    let auth = Authentication::OAuth {
        username: "me@example.test".into(),
        access_token: crate::Secret::new(long_token),
    };
    let draft = Draft {
        from: "me@example.test".into(),
        bcc: vec!["hidden@example.test".into()],
        subject: "Hello".into(),
        text: "Body".into(),
        ..Draft::default()
    };

    // Long OAuth2 tokens must use two-step `AUTH XOAUTH2` (RFC 4954 §4 <= 512-byte command line),
    // and bare `250\r\n` replies without trailing space/text must succeed (RFC 5321 §4.2).
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (socket, _) =
            aterm_uds::accept(&listener, AcceptWait::Forever, AcceptedMode::Blocking).unwrap();
        socket
            .set_read_timeout(Some(std::time::Duration::from_secs(60)))
            .unwrap();
        let mut io = BufReader::new(socket);
        io.get_mut().write_all(b"220\r\n").unwrap();
        let mut line = String::new();
        io.read_line(&mut line).unwrap();
        assert_eq!(line, "EHLO aterm\r\n");
        io.get_mut()
            .write_all(b"250-smtp.example.test\r\n250\r\n")
            .unwrap();
        line.clear();
        io.read_line(&mut line).unwrap();
        assert_eq!(
            line, "AUTH XOAUTH2\r\n",
            "oversized initial-response must split across 334 continuation"
        );
        io.get_mut().write_all(b"334 \r\n").unwrap();
        line.clear();
        io.read_line(&mut line).unwrap();
        assert!(line.len() > 600);
        io.get_mut().write_all(b"235\r\n").unwrap();
        for response in [b"250\r\n".as_slice(), b"250\r\n", b"354\r\n"] {
            line.clear();
            io.read_line(&mut line).unwrap();
            io.get_mut().write_all(response).unwrap();
        }
        loop {
            line.clear();
            assert!(io.read_line(&mut line).unwrap() > 0);
            if line == ".\r\n" {
                break;
            }
        }
        io.get_mut().write_all(b"250\r\n").unwrap();
    });
    let socket = TcpStream::connect(address).unwrap();
    submit(
        Wire::for_test(socket),
        Security::Tls,
        &auth,
        &encode(&draft).unwrap(),
        &draft,
    )
    .unwrap();
    server.join().unwrap();

    // A `334` OAuth error challenge sends empty `\r\n` to surface the server's real `535` code.
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (socket, _) =
            aterm_uds::accept(&listener, AcceptWait::Forever, AcceptedMode::Blocking).unwrap();
        let mut io = BufReader::new(socket);
        io.get_mut().write_all(b"220 fixture\r\n").unwrap();
        let mut line = String::new();
        io.read_line(&mut line).unwrap();
        io.get_mut().write_all(b"250 fixture\r\n").unwrap();
        line.clear();
        io.read_line(&mut line).unwrap();
        io.get_mut()
            .write_all(b"334 eyJzdGF0dXMiOiI0MDAifQ==\r\n")
            .unwrap();
        line.clear();
        io.read_line(&mut line).unwrap();
        assert_eq!(line, "\r\n");
        io.get_mut()
            .write_all(b"535 5.7.8 Username and Password not accepted\r\n")
            .unwrap();
    });
    let short_oauth = Authentication::OAuth {
        username: "me@example.test".into(),
        access_token: crate::Secret::new("bad".into()),
    };
    let err = submit(
        Wire::for_test(TcpStream::connect(address).unwrap()),
        Security::Tls,
        &short_oauth,
        &encode(&draft).unwrap(),
        &draft,
    )
    .unwrap_err();
    assert!(err.contains("SMTP 535"), "{err}");
    server.join().unwrap();
}
