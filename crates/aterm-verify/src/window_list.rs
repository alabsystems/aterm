// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Developer Tools list as the aterm instance verify runs in reads it, for the run-start
//! line's `(<app> listed: …)` word: verify runs from a checkout, outside any `.app`, so its own
//! read is refused ([`atpkg::devtools::listed_word`]), and the instance's, made inside its bundle,
//! is asked through aterm ctl's own client path.

use std::path::Path;
use std::time::{Duration, Instant};

use atpkg::devtools::WindowList;

/// How long the instance has to answer.
pub const ASK_BOUND: Duration = Duration::from_secs(2);

/// What the instance hosting this process's session (`ATERM_PARENT_SESSION_ID`, found by its
/// discovery graph entry: [`aterm_ctl::hosting_sock`], never another instance) reads of the list,
/// asked once (`privacy --json`, as its owner) on a thread of its own that is waited for at most
/// [`ASK_BOUND`]; `None` with no session, no instance hosting it, or no whole answer in time.
#[must_use]
pub fn hosting_instance() -> Option<WindowList> {
    let sid = std::env::var(aterm_types::domain::ENV_PARENT_SESSION_ID)
        .ok()
        .filter(|sid| !sid.is_empty())?;
    let dir = aterm_uds::control_socket_dir()?;
    let (answer, answered) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("verify-devtools".to_owned())
        .spawn(move || {
            let _ = answer.send(ask(&dir, &sid, Instant::now() + ASK_BOUND));
        })
        .ok()?;
    answered.recv_timeout(ASK_BOUND).ok().flatten()
}

/// One `privacy --json` to the instance `dir`'s graph says hosts `sid`, connected as aterm ctl
/// connects ([`aterm_ctl::connect_then_read_token`]) and asked in its grammar
/// ([`aterm_ctl::exchange_lines`]: its token's `AUTH`, then the request, and the reply's
/// `OK <n>` and `n` lines, one JSON object), every write and read done by `deadline`.
fn ask(dir: &Path, sid: &str, deadline: Instant) -> Option<WindowList> {
    let sid = aterm_types::control_request::Sid::parse(sid)?;
    let sock = aterm_ctl::hosting_sock(dir, sid)?;
    let (stream, token) = aterm_ctl::connect_then_read_token(&sock).ok()?;
    let within = deadline.saturating_duration_since(Instant::now());
    let lines = aterm_ctl::exchange_lines(&stream, &token.ok()?, "privacy --json", within).ok()?;
    devtools_of(&lines.join("\n"))
}

/// `privacy --json`'s `devtools.subject` and `devtools.list`; `None` when either is missing or
/// not a string (an instance with no subject answers `null`).
#[must_use]
pub fn devtools_of(json: &str) -> Option<WindowList> {
    let value = aterm_json::from_str::<aterm_json::Value>(json).ok()?;
    let devtools = value.get("devtools")?;
    Some(WindowList {
        subject: devtools.get("subject")?.as_str()?.to_owned(),
        list: devtools.get("list")?.as_str()?.to_owned(),
    })
}

#[cfg(test)]
mod tests {
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
}
