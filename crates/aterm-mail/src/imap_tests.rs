// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_uds::{AcceptWait, AcceptedMode};
#[test]
fn a_persistent_reader_batches_headers_and_wakes_on_server_idle_events() {
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};
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
        let mut socket = BufReader::new(socket);
        let mut command = String::new();
        socket.read_line(&mut command).unwrap();
        assert_eq!(command, "a1 UID SEARCH ALL\r\n");
        socket
            .get_mut()
            .write_all(b"* SEARCH 1 2\r\na1 OK searched\r\n")
            .unwrap();
        command.clear();
        socket.read_line(&mut command).unwrap();
        assert!(command.starts_with("a2 UID FETCH 2,1 "));
        for id in [1, 2] {
            let headers = format!(
                "From: sender@example.test\r\nTo: me@example.test\r\nSubject: item {id}\r\n\r\n"
            );
            let prefix = if id == 1 {
                "UID 1 FLAGS () "
            } else {
                "FLAGS () "
            };
            let suffix = if id == 2 { " UID 2" } else { "" };
            socket.get_mut().write_all(format!("* {id} FETCH ({prefix}BODY[HEADER.FIELDS (FROM TO SUBJECT DATE)] {{{}}}\r\n{headers}{suffix})\r\n", headers.len()).as_bytes()).unwrap();
        }
        socket
            .get_mut()
            .write_all(b"* 3 EXISTS\r\na2 OK fetched\r\n")
            .unwrap();
        command.clear();
        socket.read_line(&mut command).unwrap();
        assert_eq!(command, "a3 IDLE\r\n");
        socket
            .get_mut()
            .write_all(b"* 3 EXISTS\r\n+ idling\r\n")
            .unwrap();
        command.clear();
        socket.read_line(&mut command).unwrap();
        assert_eq!(command, "DONE\r\n");
        socket
            .get_mut()
            .write_all(b"a3 OK idle complete\r\n")
            .unwrap();
        command.clear();
        socket.read_line(&mut command).unwrap();
        assert_eq!(command, "a4 IDLE\r\n");
        socket
            .get_mut()
            .write_all(b"* OK Still here\r\n+ idling\r\n* OK Keepalive\r\n")
            .unwrap();
        command.clear();
        socket.read_line(&mut command).unwrap();
        assert_eq!(command, "DONE\r\n");
        socket
            .get_mut()
            .write_all(b"a4 OK idle complete\r\n")
            .unwrap();
        command.clear();
        socket.read_line(&mut command).unwrap();
        assert_eq!(command, "a5 IDLE\r\n");
        socket.get_mut().write_all(b"+ idling\r\n").unwrap();
        command.clear();
        socket.read_line(&mut command).unwrap();
        assert_eq!(command, "DONE\r\n");
        socket
            .get_mut()
            .write_all(b"* 4 EXISTS\r\na5 OK idle complete\r\n")
            .unwrap();
        command.clear();
        socket.read_line(&mut command).unwrap();
        assert_eq!(command, "a6 UID FETCH 2 (UID FLAGS BODY.PEEK[])\r\n");
        socket.get_mut().write_all(b"* 1 FETCH (UID 1 BODY[] {5}\r\nwrong)\r\n* 2 FETCH (BODY[] {5}\r\nright UID 2)\r\na6 OK fetched\r\n").unwrap();
        command.clear();
        socket.read_line(&mut command).unwrap();
        assert_eq!(command, "a7 UID FETCH 3 (UID FLAGS BODY.PEEK[])\r\n");
        socket
            .get_mut()
            .write_all(b"* 1 FETCH (UID 1 BODY[] {5}\r\nwrong)\r\na7 OK fetched\r\n")
            .unwrap();
        command.clear();
        socket.read_line(&mut command).unwrap();
        assert_eq!(command, "a8 UID FETCH 4 (UID FLAGS BODY.PEEK[])\r\n");
        socket.get_mut().write_all(b"a8 OK absent\r\n").unwrap();
    });
    let mut reader = Reader {
        session: Session {
            wire: Wire::for_test(TcpStream::connect(address).unwrap()),
            tag: 0,
            changed: false,
        },
        idle: true,
    };
    let messages = reader.messages(100).unwrap();
    assert_eq!(
        messages.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        ["2", "1"]
    );
    assert!(reader.wait(std::time::Duration::from_secs(60)).unwrap());
    assert!(reader.wait(std::time::Duration::from_secs(60)).unwrap());
    assert!(!reader.wait(std::time::Duration::from_millis(20)).unwrap());
    assert!(reader.wait(std::time::Duration::from_millis(20)).unwrap());
    assert_eq!(reader.source("2").unwrap(), b"right");
    assert!(reader.source("3").is_err());
    assert_eq!(reader.read("0").unwrap_err(), ReadError::InvalidId);
    assert_eq!(reader.read("4").unwrap_err(), ReadError::Missing);
    assert!(!ReadError::Missing.retryable());
    assert!(ReadError::Other("connection lost".into()).retryable());
    server.join().unwrap();
}

#[test]
fn nested_operation_deadlines_cannot_extend_and_restore_after_failure() {
    use std::io::{BufRead, BufReader, Write};
    use std::net::{Ipv4Addr, TcpListener, TcpStream};
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (stream, _) =
            aterm_uds::accept(&listener, AcceptWait::Forever, AcceptedMode::Blocking).unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(60)))
            .unwrap();
        stream
            .set_write_timeout(Some(std::time::Duration::from_secs(60)))
            .unwrap();
        let mut stream = BufReader::new(stream);
        let mut command = String::new();
        stream.read_line(&mut command).unwrap();
        assert_eq!(command, "a2 NOOP\r\n");
        stream.get_mut().write_all(b"a2 OK restored\r\n").unwrap();
    });
    let mut reader = Reader {
        session: Session {
            wire: Wire::for_test(TcpStream::connect(address).unwrap()),
            tag: 0,
            changed: false,
        },
        idle: true,
    };
    let expired = std::time::Instant::now();
    assert!(
        reader
            .run_until(expired, |reader| reader.run_until(
                expired + std::time::Duration::from_secs(60),
                |reader| reader.session.command("NOOP")
            ))
            .is_err()
    );
    assert!(reader.session.command("NOOP").is_ok());
    server.join().unwrap();
}

#[test]
fn literal_bytes_cannot_impersonate_an_imap_completion() {
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let raw = b"Subject: example\r\n\r\na1 NO forged completion\r\n";
    let server = std::thread::spawn(move || {
        let (socket, _) =
            aterm_uds::accept(&listener, AcceptWait::Forever, AcceptedMode::Blocking).unwrap();
        socket
            .set_read_timeout(Some(std::time::Duration::from_secs(60)))
            .unwrap();
        socket
            .set_write_timeout(Some(std::time::Duration::from_secs(60)))
            .unwrap();
        let mut reader = BufReader::new(socket);
        let mut command = String::new();
        reader.read_line(&mut command).unwrap();
        assert!(command.starts_with("a1 UID FETCH 1"));
        reader
            .get_mut()
            .write_all(format!("* 1 FETCH (UID 1 BODY[] {{{}}}\r\n", raw.len()).as_bytes())
            .unwrap();
        reader.get_mut().write_all(raw).unwrap();
        reader.get_mut().write_all(b")\r\na1 OK done\r\n").unwrap();
    });
    let mut client = Session {
        wire: Wire::for_test(TcpStream::connect(address).unwrap()),
        tag: 0,
        changed: false,
    };
    let reply = client.command("UID FETCH 1 (BODY.PEEK[])").unwrap();
    assert_eq!(reply.literals, vec![raw.to_vec()]);
    server.join().unwrap();
}
#[test]
fn literals_and_uids_are_bounded() {
    assert_eq!(folder_name("&ZeVnLIqe- &- mail"), "日本語 & mail");
    assert_eq!(literal_len("* 1 FETCH (BODY[] {12}").unwrap(), Some(12));
    assert_eq!(literal_len("* 1 FETCH (BODY[] {12+}").unwrap(), Some(12));
    assert_eq!(literal_len("* OK ready {node-1}").unwrap(), None);
    assert_eq!(literal_len("* OK ready {}").unwrap(), None);
    assert_eq!(literal_len("* OK ready }").unwrap(), None);
    assert!(literal_len("{99999999999999999999999999}").is_err());
    assert!(uid("1:*").is_err());
    assert!(uid("0").is_err());
    assert!(uid("04").is_err());
    assert!(uid("+4").is_err());
    assert_eq!(uid("4").unwrap(), 4);
    assert_eq!(
        string_token("\"Sent messages\" tail").unwrap(),
        ("Sent messages".into(), " tail")
    );
}

#[test]
fn folder_literals_stay_aligned_across_noselect_and_inline_fetch_bodies_parse_cleanly() {
    use std::io::{BufRead, BufReader, Write};
    use std::net::{Ipv4Addr, TcpListener, TcpStream};

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
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
        let mut reader = BufReader::new(socket);
        let mut cmd = String::new();

        reader.read_line(&mut cmd).unwrap();
        assert_eq!(cmd, "a1 LIST \"\" \"*\"\r\n");
        reader
            .get_mut()
            .write_all(
                b"* OK backend {node-1}\r\n\
                  * LIST (\\Noselect \\HasChildren) \"/\" {7}\r\nArchive\r\n\
                  * LIST (\\NonExistent) \"/\" \"Ghost\"\r\n\
                  * LIST (\\HasNoChildren) \"/\" {12}\r\nArchive/2026\r\n\
                  * LIST (\\HasNoChildren) \"/\" \"Notes\\\\Noselect\"\r\n\
                  a1 OK listed\r\n",
            )
            .unwrap();

        cmd.clear();
        reader.read_line(&mut cmd).unwrap();
        assert_eq!(cmd, "a2 UID FETCH 4 (UID FLAGS BODY.PEEK[])\r\n");
        reader
            .get_mut()
            .write_all(
                b"* 9 FETCH (FLAGS (\\Seen))\r\n\
                  * 4 FETCH (UID 4 FLAGS (\\Seen) BODY[] \"\")\r\n\
                  a2 OK fetched\r\n",
            )
            .unwrap();

        cmd.clear();
        reader.read_line(&mut cmd).unwrap();
        assert_eq!(cmd, "a3 UID FETCH 5 (UID FLAGS BODY.PEEK[])\r\n");
        reader
            .get_mut()
            .write_all(
                b"* 5 FETCH (FLAGS () BODY[] \"Subject: \\\\Seen UID 999\" UID 5)\r\n\
                  a3 OK fetched\r\n",
            )
            .unwrap();

        cmd.clear();
        reader.read_line(&mut cmd).unwrap();
        assert_eq!(cmd, "a4 UID SEARCH ALL\r\n");
        reader
            .get_mut()
            .write_all(b"* SEARCH 6\r\na4 OK searched\r\n")
            .unwrap();

        cmd.clear();
        reader.read_line(&mut cmd).unwrap();
        assert_eq!(
            cmd,
            "a5 UID FETCH 6 (UID FLAGS BODY.PEEK[HEADER.FIELDS (FROM TO SUBJECT DATE)])\r\n"
        );
        reader
            .get_mut()
            .write_all(
                b"* 6 FETCH (UID 6 FLAGS () BODY[HEADER.FIELDS (FROM TO SUBJECT DATE)] NIL)\r\n\
                  a5 OK fetched\r\n",
            )
            .unwrap();
    });

    let mut reader = Reader {
        session: Session {
            wire: Wire::for_test(TcpStream::connect(address).unwrap()),
            tag: 0,
            changed: false,
        },
        idle: false,
    };
    let folders = reader.folders().unwrap();
    assert_eq!(
        folders.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(),
        ["Archive/2026", "Notes\\Noselect"]
    );

    let empty = reader.read("4").unwrap();
    assert_eq!(empty.id, "4");
    assert!(!empty.unread);
    assert_eq!(empty.text, "");

    let inline = reader.read("5").unwrap();
    assert_eq!(inline.id, "5");
    assert!(
        inline.unread,
        "quoted body text must not spoof \\Seen flag in attributes"
    );
    assert_eq!(inline.subject, "\\Seen UID 999");

    assert_eq!(reader.read("04").unwrap_err(), ReadError::InvalidId);
    assert_eq!(reader.read("+4").unwrap_err(), ReadError::InvalidId);

    let listed = reader.messages(10).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, "6");
    assert!(listed[0].unread);

    server.join().unwrap();
}

#[test]
fn transient_imap_refusals_and_xoauth2_pre_challenge_lines_do_not_falsely_reject_credentials() {
    use crate::Secret;
    use std::io::{BufRead, BufReader, Write};
    use std::net::{Ipv4Addr, TcpListener, TcpStream};

    let run_auth = |auth: Authentication, script: &[&[u8]], after_challenge: Option<&[&[u8]]>| {
        let script: Vec<Vec<u8>> = script.iter().map(|s| s.to_vec()).collect();
        let after_challenge: Option<Vec<Vec<u8>>> =
            after_challenge.map(|a| a.iter().map(|s| s.to_vec()).collect());
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
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
            let mut reader = BufReader::new(socket);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            for out in &script {
                reader.get_mut().write_all(out).unwrap();
            }
            if let Some(after) = &after_challenge {
                line.clear();
                reader.read_line(&mut line).unwrap();
                for out in after {
                    reader.get_mut().write_all(out).unwrap();
                }
                if after.iter().any(|l| l.starts_with(b"+")) {
                    line.clear();
                    reader.read_line(&mut line).unwrap();
                    reader
                        .get_mut()
                        .write_all(b"a1 NO [AUTHENTICATIONFAILED] Invalid credentials\r\n")
                        .unwrap();
                }
            }
        });
        let mut session = Session {
            wire: Wire::for_test(TcpStream::connect(address).unwrap()),
            tag: 0,
            changed: false,
        };
        let result = session.authenticate(&auth);
        server.join().unwrap();
        result
    };

    let pw = Authentication::Password {
        username: "me@gmail.com".into(),
        password: Secret::new("app-password".into()),
    };
    let oauth = Authentication::OAuth {
        username: "me@gmail.com".into(),
        access_token: Secret::new("token-123".into()),
    };

    // Transient server / connection-cap / protocol errors must stay `ConnectError::Other`,
    // never `ConnectError::Authentication` (which permanently retires the mailbox login).
    for transient in [
        &b"a1 NO [UNAVAILABLE] Temporary System Problem. Try again later\r\n"[..],
        &b"a1 NO [ALERT] Too many simultaneous connections. (Failure)\r\n"[..],
        &b"a1 NO [INUSE] Mailbox locked by another session\r\n"[..],
        &b"a1 BAD Command Argument Error\r\n"[..],
        &b"* bye Server shutting down for maintenance\r\n"[..],
    ] {
        let slice = [transient];
        assert!(
            matches!(
                run_auth(pw.clone(), &slice, None),
                Err(ConnectError::Other(_))
            ),
            "password transient line {:?} must not permanently reject credentials",
            String::from_utf8_lossy(transient)
        );
        assert!(
            matches!(
                run_auth(oauth.clone(), &slice, None),
                Err(ConnectError::Other(_))
            ),
            "oauth pre-challenge transient line {:?} must not permanently reject credentials",
            String::from_utf8_lossy(transient)
        );
    }

    // Untagged server lines before `+` in XOAUTH2 succeed when the token is accepted.
    assert!(
        run_auth(
            oauth.clone(),
            &[b"* CAPABILITY IMAP4rev1 AUTH=XOAUTH2\r\n", b"+ \r\n"],
            Some(&[b"a1 OK Authenticated\r\n"]),
        )
        .is_ok()
    );

    // Genuine credential rejections still return `ConnectError::Authentication`.
    assert!(matches!(
        run_auth(
            pw,
            &[b"a1 NO [AUTHENTICATIONFAILED] Invalid credentials (Failure)\r\n"],
            None,
        ),
        Err(ConnectError::Authentication)
    ));
    assert!(matches!(
        run_auth(
            oauth,
            &[b"+ \r\n"],
            Some(&[b"+ eyJzdGF0dXMiOiI0MDAifQ==\r\n"]),
        ),
        Err(ConnectError::Authentication)
    ));
}

#[test]
fn reader_run_until_bounds_waits_and_restores_wire_deadlines() {
    use std::io::{BufRead, BufReader, Write};
    use std::net::{Ipv4Addr, TcpListener, TcpStream};
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
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
        let mut reader = BufReader::new(socket);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line, "a1 LIST \"\" \"*\"\r\n");
        reader
            .get_mut()
            .write_all(b"* LIST (\\HasNoChildren) \"/\" \"INBOX\"\r\na1 OK LIST completed\r\n")
            .unwrap();
        line.clear();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line, "PING\r\n");
        reader.get_mut().write_all(b"PONG\r\n").unwrap();
    });

    let expired = std::time::Instant::now() - std::time::Duration::from_millis(1);
    let mut mail_reader = Reader {
        session: Session {
            wire: Wire::for_test(TcpStream::connect(address).unwrap()),
            tag: 0,
            changed: false,
        },
        idle: false,
    };

    // Expired `run_until` fails fast in polling fallback (`!idle`) instead of sleeping 2s.
    assert!(
        mail_reader
            .run_until(expired, |r| r.wait(std::time::Duration::from_secs(2)))
            .is_err()
    );

    // Expired `run_until` on an IDLE-capable reader fails fast before issuing `aN IDLE`,
    // leaving the wire synchronized for subsequent commands once `restore_limit` runs.
    mail_reader.idle = true;
    assert!(
        mail_reader
            .run_until(expired, |r| r.wait(std::time::Duration::from_secs(2)))
            .is_err()
    );
    let folders = mail_reader.folders().unwrap();
    assert_eq!(folders.len(), 1);
    assert_eq!(folders[0].id, "INBOX");

    // Even when an expired `run_until` clamps the active stream deadline, `restore_limit`
    // renews the stream deadline on exit so direct wire reads/writes succeed afterward.
    assert!(
        mail_reader
            .run_until(expired, |r| r.session.wire.line())
            .is_err()
    );
    mail_reader.session.wire.write(b"PING\r\n").unwrap();
    assert_eq!(mail_reader.session.wire.line().unwrap(), b"PONG\r\n");
    server.join().unwrap();
}

#[test]
fn try_select_read_only_clears_banner_change_and_classifies_refusals() {
    use std::io::{BufRead, BufReader, Write};
    use std::net::{Ipv4Addr, TcpListener, TcpStream};
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
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
        let mut reader = BufReader::new(socket);
        let mut line = String::new();

        reader.read_line(&mut line).unwrap();
        assert_eq!(line, "a1 EXAMINE \"Archive\"\r\n");
        reader
            .get_mut()
            .write_all(b"* 5 EXISTS\r\n* 0 RECENT\r\na1 OK [READ-ONLY] EXAMINE completed\r\n")
            .unwrap();

        line.clear();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line, "a2 EXAMINE \"Missing\"\r\n");
        reader
            .get_mut()
            .write_all(b"a2 NO [NONEXISTENT] Unknown Mailbox: Missing\r\n")
            .unwrap();

        line.clear();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line, "a3 EXAMINE \"Busy\"\r\n");
        reader
            .get_mut()
            .write_all(b"a3 NO [UNAVAILABLE] Temporary System Problem\r\n")
            .unwrap();
    });

    let mut mail_reader = Reader {
        session: Session {
            wire: Wire::for_test(TcpStream::connect(address).unwrap()),
            tag: 0,
            changed: false,
        },
        idle: true,
    };

    mail_reader.try_select_read_only("Archive").unwrap();
    assert!(
        !mail_reader.wait(std::time::Duration::ZERO).unwrap(),
        "EXAMINE banner `* 5 EXISTS` must not leave a phantom notification"
    );
    assert_eq!(
        mail_reader.try_select_read_only("Missing"),
        Err(SelectError::UnknownFolder)
    );
    assert!(
        mail_reader
            .try_select_read_only("Busy")
            .unwrap_err()
            .retryable()
    );
    assert_eq!(
        mail_reader.try_select_read_only("bad\nfolder"),
        Err(SelectError::UnknownFolder)
    );
    server.join().unwrap();
}

/// IMAP flag matching splits compact backslash-prefixed flags without confusing prefix-sharing
/// keywords, and Modified UTF-7 folder decoding refuses non-canonical or shifted ASCII inputs.
/// NEGATIVE CONTROL: substring `contains("\\seen")` treats `\SeenLater` or quoted `FLAGS (\Seen)`
/// as read, and lenient base64 decoding turns `&AOm-`, `&AOk=-`, or `&AC8-` into decoded text.
#[test]
fn flag_boundaries_and_canonical_modified_utf7_folder_names_hold() {
    assert!(flag_list_contains("\\Noselect\\HasChildren", "\\Noselect"));
    assert!(flag_list_contains("\\Answered\\Seen", "\\Seen"));
    assert!(!flag_list_contains("\\NoselectLater", "\\Noselect"));
    assert!(!flag_list_contains("\\SeenLater $Seen", "\\Seen"));

    assert!(attributes_have_seen(
        "* 1 FETCH (UID 1 FLAGS(\\Answered\\Seen) BODY[] {0})"
    ));
    assert!(!attributes_have_seen(
        "* 1 FETCH (UID 1 FLAGS (\\SeenLater) BODY[] {0})"
    ));
    assert!(!attributes_have_seen(
        "* 1 FETCH (UID 1 FLAGS () ENVELOPE (\"Mon\" \"FLAGS (\\\\Seen)\") BODY[] {0})"
    ));

    assert_eq!(folder_name("&AOk-"), "é");
    assert_eq!(folder_name("&AOm-"), "&AOm-");
    assert_eq!(folder_name("&AOk=-"), "&AOk=-");
    assert_eq!(folder_name("&ZeVn/Iqe-"), "&ZeVn/Iqe-");
    assert_eq!(folder_name("&AC8-"), "&AC8-");
}
