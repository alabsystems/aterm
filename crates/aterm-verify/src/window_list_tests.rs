// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The reading is the `devtools` object's `subject` and `list`; an instance with no subject
/// (`null`), a reply with no `devtools` object, or one that is no JSON gives none.
#[test]
fn the_reading_is_the_devtools_subject_and_list() {
    assert_eq!(
        devtools_of(
            r#"{"schema":1,"devtools":{"state":"allowed","subject":"com.aterm.aterm","list":"on","list_probe":"read"}}"#
        ),
        Some(WindowList {
            subject: "com.aterm.aterm".to_owned(),
            list: "on".to_owned(),
        })
    );
    assert_eq!(
        devtools_of(r#"{"devtools":{"subject":null,"list":"unknown"}}"#),
        None
    );
    assert_eq!(devtools_of(r#"{"schema":1}"#), None);
    assert_eq!(devtools_of("OK 1"), None);
}

/// The sid a test's rendezvous names.
#[cfg(unix)]
const SID: &str = "s-5e55";

/// A rendezvous whose graph names `SID`'s instance: a socket served once, on a thread of its
/// own, by an instance that reads the two request lines it is sent, answers `reply`, and gives
/// back what it heard.
#[cfg(unix)]
fn instance(
    reply: &'static str,
) -> (
    aterm_tempfile::TempDir,
    std::thread::JoinHandle<Vec<String>>,
) {
    use std::io::{BufRead as _, Write as _};
    let rendezvous = aterm_tempfile::tempdir().expect("tempdir");
    let graph = rendezvous.path().join("graph");
    std::fs::create_dir(&graph).expect("graph");
    let sock = rendezvous.path().join("fake.sock");
    std::fs::write(graph.join(SID), format!("sock {}\n", sock.display())).expect("entry");
    let token = rendezvous
        .path()
        .join(aterm_types::control_socket::token_name_for_sock(
            "fake.sock",
        ));
    std::fs::write(&token, "cafef00d\n").expect("token");
    let listener = std::os::unix::net::UnixListener::bind(&sock).expect("bind");
    let served = std::thread::spawn(move || {
        let must = aterm_spec::wait::MUST_HAPPEN;
        let (conn, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Until(Instant::now() + must),
            aterm_uds::AcceptedMode::Blocking,
        )
        .expect("the ask's connection");
        conn.set_read_timeout(Some(must)).expect("a hang detector");
        let mut reader = std::io::BufReader::new(conn.try_clone().expect("clone"));
        let mut heard = Vec::new();
        for _ in 0..2 {
            let mut line = String::new();
            reader.read_line(&mut line).expect("a request line");
            heard.push(line);
        }
        (&conn).write_all(reply.as_bytes()).expect("reply");
        heard
    });
    (rendezvous, served)
}

/// The instance hosting the session, as its graph entry names it, is asked `privacy --json`
/// after its token's `AUTH` on one connection, and its reply is read in aterm ctl's grammar:
/// `OK <n>` and that many lines, the object read whole whether it comes on one line or
/// several; a busy instance, a refused token, another `ERR` reply, or a status line not so
/// framed gives none. NEGATIVE CONTROL: take only an `OK 1` status and its one line, as a
/// hand-made reader did, and the object on two lines gives none.
#[cfg(unix)]
#[test]
fn the_hosting_instance_is_asked_privacy_and_read_in_ctls_grammar() {
    let on = WindowList {
        subject: "com.aterm.aterm".to_owned(),
        list: "on".to_owned(),
    };
    for (reply, want) in [
        (
            "OK 1\n{\"devtools\":{\"subject\":\"com.aterm.aterm\",\"list\":\"on\"}}\n",
            Some(&on),
        ),
        (
            "OK 2\n{\"devtools\":{\"subject\":\"com.aterm.aterm\",\n\"list\":\"on\"}}\n",
            Some(&on),
        ),
        (aterm_types::control_verbs::CONTROL_BUSY_LINE, None),
        ("ERR auth\n", None),
        ("ERR unknown verb\n", None),
        ("{\"devtools\":{}}\n", None),
    ] {
        let (rendezvous, served) = instance(reply);
        let got = ask(
            rendezvous.path(),
            SID,
            Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        );
        assert_eq!(got.as_ref(), want, "{reply:?}");
        assert_eq!(
            served.join().expect("the instance"),
            ["AUTH cafef00d\n", "privacy --json\n"],
            "{reply:?}"
        );
    }
}
