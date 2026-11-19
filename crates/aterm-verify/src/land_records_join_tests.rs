// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE JOIN, verify's half: a landing run names the session the hosting instance placed it in,
//! never one a variable of its environment names; with nobody to place it, it belongs to its
//! computer; and the four fields are signed with the rest of the record. The instance's half
//! (the process's lineage decides, a legacy `s-` session as a minted one, a program worker's
//! run credited to the session that asked) is `aterm-gui`'s `control_hosted_tests.rs`.

use super::*;
use crate::hosting::tests::Recording;
use crate::hosting::{Hosted, Unhosted};
use crate::land_record::tests::{NOW, signer};
use crate::land_run::tests::record;
use crate::land_run::{Entry, Hosting};
use crate::runs::ToInstance;
use std::io::{BufRead as _, Write as _};
use std::path::PathBuf;

/// The session the landing's process really runs in, in the legacy form a session opened before
/// the minted one still holds.
const TRUE: &str = "s-f56dadda2d012b94f138";
/// Another session of the same instance: the one a forged variable names.
const FORGED: &str = "t3xcj80edmk";
/// A worker session.
const WORKER: &str = "tqn5r6e0148";
/// The instance token of the stand-in.
const TOKEN: &str = "cafef00d";

/// A stand-in aterm instance: a scratch rendezvous whose graph names one socket for each of the
/// sessions it hosts, the token beside it, and the socket, bound.
struct Instance {
    rendezvous: aterm_tempfile::TempDir,
    listener: std::os::unix::net::UnixListener,
}

impl Instance {
    fn hosting(sids: &[&str]) -> Self {
        let rendezvous = aterm_tempfile::tempdir().expect("tempdir");
        let graph = rendezvous.path().join("graph");
        std::fs::create_dir(&graph).expect("graph");
        let sock = rendezvous.path().join("fake.sock");
        for sid in sids {
            std::fs::write(graph.join(sid), format!("sock {}\n", sock.display())).expect("entry");
        }
        let token = rendezvous
            .path()
            .join(aterm_types::control_socket::token_name_for_sock(
                "fake.sock",
            ));
        std::fs::write(token, format!("{TOKEN}\n")).expect("token");
        let listener = std::os::unix::net::UnixListener::bind(&sock).expect("bind");
        Self {
            rendezvous,
            listener,
        }
    }

    fn dir(&self) -> Option<PathBuf> {
        Some(self.rendezvous.path().to_path_buf())
    }

    /// Takes one connection, reads its `AUTH` line and its one request, answers `reply`: the
    /// two lines it heard.
    fn serve(&self, reply: &str) -> Vec<String> {
        let must = aterm_spec::wait::MUST_HAPPEN;
        let (conn, _) = aterm_uds::accept(
            &self.listener,
            aterm_uds::AcceptWait::Until(Instant::now() + must),
            aterm_uds::AcceptedMode::Blocking,
        )
        .expect("the post's connection");
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
    }

    /// What `ask` got while the instance answered its one post with `reply`, and the lines the
    /// instance heard.
    fn answering<T>(&self, reply: &str, ask: impl FnOnce() -> T) -> (T, Vec<String>) {
        std::thread::scope(|scope| {
            let served = scope.spawn(|| self.serve(reply));
            let got = ask();
            (got, served.join().expect("the instance"))
        })
    }
}

/// The checkout every run here lands.
fn root() -> &'static Path {
    Path::new("/work/aterm-feature")
}

/// A run of the landing at [`root`] on host `mac`, on branch `feature/join`, posted to `host`.
fn run_on(host: &dyn crate::hosting::Host) -> land_run::Run {
    run_of(root(), "mac", None, Some("feature/join"), host)
}

/// A process cannot claim a session by setting a variable. The landing's shell says, in both
/// `ATERM_SESSION_ID` and `ATERM_PARENT_SESSION_ID`, that it is session [`FORGED`]; the
/// instance, which hosts that session too, placed the process in [`TRUE`]. The run is
/// [`TRUE`]'s: the request the landing sent names no session at all (the variable only found
/// the instance to ask), the record's old `session` field is not written, and the forged id is
/// in no byte of the signed record.
///
/// NEGATIVE CONTROL: `ATERM_SESSION_ID` naming [`FORGED`] with nothing else finds no instance,
/// so nobody is asked and the run names no session, though a session of that id exists.
#[test]
fn join_a_forged_session_variable_does_not_move_a_run() {
    let instance = Instance::hosting(&[TRUE, FORGED]);
    let forged = |key: &str| {
        matches!(key, "ATERM_SESSION_ID" | "ATERM_PARENT_SESSION_ID").then(|| FORGED.to_owned())
    };
    let host = ToInstance::of(&forged, instance.dir());
    let (run, heard) =
        instance.answering(&format!("OK hosted={TRUE} worker=-\n"), || run_on(&host));
    assert_eq!(
        heard,
        [
            format!("AUTH {TOKEN}\n"),
            "hosted start kind=land\n".to_owned()
        ],
        "the request names no session"
    );
    assert_eq!(
        run.hosting(),
        Some(Hosting {
            hosted: TRUE,
            worker: None
        })
    );
    assert_eq!(run.session, None, "the shell's id is not recorded");
    let signed = land_run::Record {
        run,
        ..record(Kind::End, 0)
    }
    .sign(Some(&signer(1)))
    .expect("it signs");
    let text = String::from_utf8(signed.bytes).expect("UTF-8");
    assert!(text.contains(&format!("hosted = \"{TRUE}\"")), "{text}");
    assert!(!text.contains(FORGED), "{text}");

    let shell_only = |key: &str| (key == "ATERM_SESSION_ID").then(|| FORGED.to_owned());
    let alone = run_on(&ToInstance::of(&shell_only, instance.dir()));
    assert_eq!(alone.hosting(), None);
    assert_eq!(alone.session, None);
}

/// A run with no hosting session belongs to its computer and to no session, and the landing
/// goes on: with nobody to ask (no session to find an instance by, no instance hosting it, a
/// socket nothing answers at, or no host at all), with an instance that placed the process in
/// no session (a LaunchAgent, a bare ssh), and with one that does not know the verb (an earlier
/// aterm), the run names its host and node, its checkout and its branch, and no session; its
/// record signs and reads, and holds neither field. Its end is posted to nobody without
/// failing. A record made before the fields reads the same way: no hosting session known,
/// whatever shell id its `session` holds.
///
/// NEGATIVE CONTROL: the same run, with an instance that placed it, names that session.
#[test]
fn join_a_run_with_no_hosting_session_belongs_to_its_computer() {
    let instance = Instance::hosting(&[TRUE]);
    let named = |sid: &str| ToInstance {
        sid: Some(sid.to_owned()),
        dir: instance.dir(),
    };
    let gone = aterm_tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(gone.path().join("graph")).expect("graph");
    std::fs::write(
        gone.path().join("graph").join(TRUE),
        format!("sock {}\n", gone.path().join("gone.sock").display()),
    )
    .expect("entry");
    let me = signer(3);
    let node = Some(me.node().to_owned());
    let mut unhosted: Vec<(String, land_run::Run)> = Vec::new();
    let nobody: [(&str, Box<dyn crate::hosting::Host>); 4] = [
        ("no host", Box::new(Unhosted)),
        (
            "no session names an instance",
            Box::new(ToInstance::default()),
        ),
        ("no instance hosts the session", Box::new(named(FORGED))),
        (
            "nothing answers at the socket",
            Box::new(ToInstance {
                sid: Some(TRUE.to_owned()),
                dir: Some(gone.path().to_path_buf()),
            }),
        ),
    ];
    for (case, host) in &nobody {
        let run = run_of(root(), "mac", node.clone(), Some("feature/join"), &**host);
        host.end(crate::hosting::LAND, Some(exit::PASS));
        unhosted.push(((*case).to_owned(), run));
    }
    for reply in [
        "OK hosted=- worker=-\n",
        "ERR unknown verb: hosted\n",
        "OK hosted=tab-1 worker=-\n",
    ] {
        let (run, _) = instance.answering(reply, || {
            run_of(
                root(),
                "mac",
                node.clone(),
                Some("feature/join"),
                &named(TRUE),
            )
        });
        unhosted.push((reply.trim().to_owned(), run));
    }
    for (case, run) in unhosted {
        assert_eq!(run.hosting(), None, "{case}");
        assert_eq!(
            (run.host.as_str(), &run.node, run.session.as_deref()),
            ("mac", &node, None),
            "{case}"
        );
        assert_eq!(
            (run.checkout.as_deref(), run.branch.as_deref()),
            (Some("/work/aterm-feature"), Some("feature/join")),
            "{case}"
        );
        let signed = land_run::Record {
            run: land_run::Run {
                id: crate::land_run::tests::RUN.to_owned(),
                ..run
            },
            ..record(Kind::End, 0)
        }
        .sign(Some(&me))
        .expect("it signs");
        let text = String::from_utf8(signed.bytes.clone()).expect("UTF-8");
        assert!(
            !text.contains("hosted") && !text.contains("worker"),
            "{case}: {text}"
        );
        let read = Entry::read(&signed.path, &signed.bytes, Origin::Journal)
            .body
            .unwrap_or_else(|why| panic!("{case}: {why}"));
        assert_eq!(read.record.run.hosting(), None, "{case}");
        assert_eq!(read.signature.map(|sig| sig.key), Some(me.key()), "{case}");
    }

    // A record from before the join: `session` names the shell, and nothing names a session.
    let before = record(Kind::End, 0);
    assert_eq!(before.run.session.as_deref(), Some("tab-1"));
    let signed = before.sign(Some(&me)).expect("it signs");
    let read = Entry::read(&signed.path, &signed.bytes, Origin::Journal)
        .body
        .expect("an earlier record reads");
    assert_eq!(read.record.run.hosting(), None);
    assert_eq!(read.record.at, NOW);

    let (placed, _) = instance.answering(&format!("OK hosted={TRUE} worker=-\n"), || {
        run_of(
            root(),
            "mac",
            node.clone(),
            Some("feature/join"),
            &named(TRUE),
        )
    });
    assert_eq!(placed.hosting().map(|hosting| hosting.hosted), Some(TRUE));
}

/// The run record signs the session it belongs to, its worker, its checkout and its branch: a
/// record naming all four (a `verify land --tab` landing, credited to the legacy session that
/// asked, run by a minted worker session) reads back as made under its signer's key, and a
/// change to any one of the four after the signature is refused. A reader also refuses, signed
/// or not, a hosting session or a worker that is no session id, a worker with no session it
/// worked for or that is that session, and a checkout or a branch no record holds. The run's
/// end is posted where its start was.
///
/// NEGATIVE CONTROL: the untouched bytes read, so each refusal is the change's.
#[test]
fn join_the_run_record_signs_hosted_checkout_branch_and_worker() {
    let me = signer(1);
    let host = Recording {
        placed: Hosted {
            hosted: Some(TRUE.to_owned()),
            worker: Some(WORKER.to_owned()),
        },
        ..Recording::default()
    };
    let run = land_run::Run {
        id: crate::land_run::tests::RUN.to_owned(),
        ..run_on(&host)
    };
    assert_eq!(
        run.hosting(),
        Some(Hosting {
            hosted: TRUE,
            worker: Some(WORKER)
        })
    );
    let made = land_run::Record {
        run: run.clone(),
        ..record(Kind::End, 0)
    };
    let signed = made.sign(Some(&me)).expect("it signs");
    let text = String::from_utf8(signed.bytes.clone()).expect("UTF-8");
    let fields = [
        format!("hosted = \"{TRUE}\""),
        format!("worker = \"{WORKER}\""),
        "checkout = \"/work/aterm-feature\"".to_owned(),
        "branch = \"feature/join\"".to_owned(),
    ];
    let (body, signature) = text.split_once("[signature]").expect("a signature");
    for field in &fields {
        assert!(body.contains(field), "{field} is signed: {text}");
        assert!(!signature.contains(field), "{text}");
    }
    let read = Entry::read(&signed.path, &signed.bytes, Origin::Journal)
        .body
        .expect("it reads");
    assert_eq!(read.record, made);
    assert_eq!(read.signature.map(|sig| sig.key), Some(me.key()));

    let changes = [
        (fields[0].as_str(), format!("hosted = \"{FORGED}\"")),
        (fields[1].as_str(), "worker = \"t0000000000\"".to_owned()),
        (
            fields[2].as_str(),
            "checkout = \"/work/another\"".to_owned(),
        ),
        (fields[3].as_str(), "branch = \"main\"".to_owned()),
    ];
    for (field, changed) in changes {
        let tampered = text.replace(field, &changed);
        assert_ne!(tampered, text, "{field}");
        let refused = Entry::read(&signed.path, tampered.as_bytes(), Origin::Journal)
            .body
            .expect_err(&changed);
        assert!(refused.contains("signature"), "{changed}: {refused}");
    }

    let with = |change: &dyn Fn(&mut land_run::Run)| {
        let mut run = run.clone();
        change(&mut run);
        land_run::Record {
            run,
            ..record(Kind::End, 0)
        }
    };
    let refusals: [(&dyn Fn(&mut land_run::Run), &str); 7] = [
        (
            &|run| run.hosted = Some("tab-1".to_owned()),
            "no session id",
        ),
        (
            &|run| run.worker = Some("S-F56DADDA2D012B94F138".to_owned()),
            "no session id",
        ),
        (&|run| run.hosted = None, "names no other session"),
        (
            &|run| run.worker = Some(TRUE.to_owned()),
            "names no other session",
        ),
        (
            &|run| run.checkout = Some("/work/a\u{7}b".to_owned()),
            "not one a record holds",
        ),
        (
            &|run| run.checkout = Some("x".repeat(land_run::WHY_BYTES + 1)),
            "not one a record holds",
        ),
        (
            &|run| run.branch = Some(String::new()),
            "not one a record holds",
        ),
    ];
    for (change, why) in refusals {
        let bad = with(change);
        for signer in [Some(&me as &dyn Signer), None] {
            let signed = bad.sign(signer).expect("it encodes");
            let refused = Entry::read(&signed.path, &signed.bytes, Origin::Spool)
                .body
                .expect_err(why);
            assert!(refused.contains(why), "{:?}: {refused}", bad.run);
        }
    }

    // The run's end goes where its start went: the host that placed it.
    let scratch = aterm_tempfile::tempdir().expect("tempdir");
    let git_here = |dir: &Path| {
        let mut cmd = aterm_uds::fixture_git::git();
        cmd.current_dir(dir);
        cmd
    };
    let host = std::sync::Arc::new(host);
    let mut records = Records {
        append: Box::new(|_, _, _| Ok(())),
        spool: Box::new(Unavailable("no spool in this test".to_owned())),
        signer: None,
        host: "mac".to_owned(),
        now: Box::new(|| NOW),
        run,
        hosting: Box::new(Shared(host.clone())),
        interrupt: Box::new(|| None),
        tally: Tally::default(),
        lock: None,
    };
    let _ = finish(
        &Git {
            root: scratch.path(),
            make: &git_here,
        },
        &mut records,
        &mut |_| {},
        exit::FAILED,
    );
    assert_eq!(
        host.posted(),
        [
            "hosted start kind=land".to_owned(),
            format!("hosted end kind=land exit={}", exit::FAILED)
        ]
    );
}

/// A host a test also reads after the records took it.
#[derive(Debug)]
struct Shared(std::sync::Arc<Recording>);

impl crate::hosting::Host for Shared {
    fn start(&self, kind: &str) -> Hosted {
        self.0.start(kind)
    }
    fn end(&self, kind: &str, exit: Option<i32>) {
        self.0.end(kind, exit);
    }
}
