// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::test_socket::read_to_end_of_file;
use crate::test_socket::{hang_up, join_after_hang_up};
use std::io::{BufRead, BufReader};

fn unknown_host() -> ForwardHost {
    ForwardHost {
        sock: String::new(),
        token: String::new(),
        pid: None,
        listener: None,
    }
}

fn token_beside(sock: &Path, token: &str) {
    let name = aterm_types::control_socket::token_name_for_sock(
        &sock.file_name().expect("a socket file").to_string_lossy(),
    );
    std::fs::write(sock.with_file_name(name), format!("{token}\n")).expect("the host's token");
}

fn hop_to(sock: &Path, token: &str) -> Hop {
    token_beside(sock, token);
    connect_hop(&sock.to_string_lossy())
        .unwrap_or_else(|e| panic!("{}: no hop: {e:?}", sock.display()))
}

#[test]
fn published_sock_spelling_drops_only_the_verbatim_drive_prefix() {
    assert_eq!(
        published_sock_spelling(r"\\?\C:\Users\u\AppData\Local\Temp\aterm\aterm-1.sock"),
        r"C:\Users\u\AppData\Local\Temp\aterm\aterm-1.sock"
    );
    assert_eq!(
        published_sock_spelling(r"C:\Users\u\aterm-1.sock"),
        r"C:\Users\u\aterm-1.sock"
    );
    assert_eq!(
        published_sock_spelling(r"\\?\UNC\srv\share\aterm.sock"),
        r"\\?\UNC\srv\share\aterm.sock"
    );
    assert_eq!(
        published_sock_spelling("/private/var/folders/x/aterm-1.sock"),
        "/private/var/folders/x/aterm-1.sock"
    );
}

#[test]
fn explicit_socket_instance_resolvable_from_default_dir_entry() {
    let default_dir = std::env::temp_dir().join(format!("aterm-defaultdir-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&default_dir);
    let sid = SessionId::generate();
    let nonce = LaunchNonce::generate();
    let explicit_sock = "/some/explicit/place/myapp-control.sock";

    write_graph_entry(&default_dir, &sid, explicit_sock, &nonce);

    let (sock, got_nonce) =
        read_graph_entry(&default_dir, sid_of(&sid)).expect("entry in default dir");
    assert_eq!(
        sock, explicit_sock,
        "the default-dir entry names the explicit out-of-dir socket verbatim"
    );
    assert!(got_nonce.ct_eq(&nonce));
    let body = std::fs::read_to_string(default_dir.join("graph").join(sid.as_str())).expect("body");
    assert_eq!(
        aterm_types::control_socket::graph_entry_sock(&body).as_deref(),
        Some(explicit_sock)
    );
    assert_eq!(
        aterm_types::control_socket::graph_entry_pid(&body),
        Some(std::process::id())
    );
    let _ = std::fs::remove_dir_all(&default_dir);
}

/// NEGATIVE CONTROL: a missing default dir or a `sock_dir` matching the default dir (literally or via symlink) yields `None`, never a double-write.
#[test]
fn mirror_dir_only_for_out_of_default_instances() {
    let _guard = self_sock_test_guard();
    let def = crate::test_scratch::short_dir("default rendezvous dir");
    let other = crate::test_scratch::short_dir("out-of-default dir");
    assert_eq!(
        rendezvous_mirror_dir_with(other.path(), None),
        None,
        "an absent default dir yields no mirror target"
    );
    set_mirror_dir_override(Some(def.path().to_path_buf()));
    assert!(
        rendezvous_mirror_dir(def.path()).is_none(),
        "a default instance must not double-write"
    );
    #[cfg(unix)]
    {
        let alias = other.path().join("default-alias");
        std::os::unix::fs::symlink(def.path(), &alias).expect("create symlink alias");
        assert!(
            rendezvous_mirror_dir(&alias).is_none(),
            "a symlinked alias of the default dir must not double-write"
        );
    }
    assert_eq!(
        rendezvous_mirror_dir(other.path()).as_deref(),
        Some(def.path()),
        "an out-of-dir instance mirrors into the default rendezvous dir"
    );
    set_mirror_dir_override(None);
}

#[test]
fn parse_graph_entry_rejects_malformed() {
    assert!(
        parse_graph_entry("sock /a/b.sock\nnonce deadbeef\n").is_none(),
        "short nonce"
    );
    assert!(
        parse_graph_entry(
            "nonce {}\n"
                .replace("{}", &LaunchNonce::generate().to_hex())
                .as_str()
        )
        .is_none(),
        "no sock"
    );
    let good = format!("sock /x.sock\nnonce {}\n", LaunchNonce::generate().to_hex());
    assert!(parse_graph_entry(&good).is_some());
}

#[test]
fn an_id_that_is_no_sid_is_never_joined_into_a_path() {
    let dir = crate::test_scratch::short_dir("the graph dir");
    let nonce = LaunchNonce::generate();
    let minted = SessionId::generate();
    write_graph_entry(dir.path(), &minted, "/x/aterm-1.sock", &nonce);
    assert!(
        dir.path().join("graph").join(minted.as_str()).is_file(),
        "a minted id's entry is written"
    );
    for id in ["../x", "*", "s-0A1B", "s-"] {
        let id = SessionId::new(id);
        write_graph_entry(dir.path(), &id, "/x/aterm-1.sock", &nonce);
        assert_eq!(graph_entry_host_pid(dir.path(), &id), None, "{id:?}");
        assert!(
            write_edge_tokens(dir.path(), &id, "aa", "bb", "cc").is_none(),
            "{id:?}"
        );
    }
    assert!(!dir.path().join("x").exists(), "`../x` left graph/");
    let mut names: Vec<_> = std::fs::read_dir(dir.path().join("graph"))
        .expect("graph/")
        .map(|e| e.expect("entry").file_name())
        .collect();
    names.sort();
    assert_eq!(names, [std::ffi::OsString::from(minted.as_str())]);
}

#[cfg(unix)]
#[test]
fn graph_and_edge_sidecars_reject_symlinks_and_oversized_entries() {
    let dir = crate::test_scratch::short_dir("graph-sidecar-hygiene");
    let nonce = LaunchNonce::generate();
    let minted = SessionId::generate();
    let _ = std::fs::create_dir_all(dir.path().join("graph"));
    let target = dir.path().join("target.txt");
    std::fs::write(
        &target,
        format!("sock /x/aterm-1.sock\nnonce {}\npid 1\n", nonce.to_hex()),
    )
    .expect("target");
    let entry = dir.path().join("graph").join(minted.as_str());
    std::os::unix::fs::symlink(&target, &entry).expect("symlink");
    assert!(read_graph_entry(dir.path(), sid_of(&minted)).is_none());
    assert_eq!(graph_entry_host_pid(dir.path(), &minted), None);
    write_graph_entry(dir.path(), &minted, "/x/aterm-2.sock", &nonce);
    assert!(
        std::fs::read_to_string(&target)
            .expect("target intact")
            .contains("/x/aterm-1.sock")
    );
    std::fs::remove_file(&entry).expect("remove symlink");
    std::fs::write(&entry, "x".repeat(MAX_PRIVATE_TEXT_BYTES as usize + 1)).expect("oversized");
    assert!(read_private_text(&entry).is_err());
}

#[cfg(unix)]
#[test]
fn required_proxy_accepts_a_scheduled_host() {
    for age in [
        std::time::Duration::from_secs(11),
        std::time::Duration::from_secs(61),
    ] {
        let expired = age >= aterm_spec::wait::MUST_HAPPEN;
        let dir = crate::test_scratch::short_dir("scheduled-hop");
        let sock = dir.path().join("host.sock");
        let listener = aterm_uds::CtlListener::bind(&sock).unwrap();
        let (ready, listening) = std::sync::mpsc::channel();
        let host = std::thread::spawn(move || -> std::io::Result<Vec<String>> {
            let (stream, _) = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )?;
            stream.set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))?;
            stream.set_write_timeout(Some(aterm_spec::wait::MUST_HAPPEN))?;
            ready.send(()).unwrap();
            let mut reader = BufReader::new(&stream);
            let mut probe = String::new();
            reader.read_line(&mut probe)?;
            if probe != "TOKEN tok version\n" {
                return Err(std::io::Error::other("wrong native probe"));
            }
            if expired {
                let mut rest = String::new();
                reader.read_to_string(&mut rest)?;
                return Ok(vec![probe, rest]);
            }
            (&stream).write_all(b"OK version=host\n")?;
            let mut verb = String::new();
            reader.read_line(&mut verb)?;
            Ok(vec![probe, verb])
        });
        let (_application, client) = CtlStream::pair().unwrap();
        let mut relayed = false;
        let result = deliver_then_relay_with_clock(
            hop_to(&sock, "tok"),
            "status",
            &client,
            &[],
            FORWARD_HANG,
            |_, _, _| {
                relayed = true;
                Ok(())
            },
            || {
                listening
                    .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                    .unwrap();
                std::time::Instant::now() - age
            },
        );
        let heard = host.join().unwrap();
        if expired {
            assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::TimedOut);
            assert_eq!(heard.unwrap(), ["TOKEN tok version\n", ""]);
            assert!(
                !relayed,
                "an expired proof sends no command and starts no relay"
            );
        } else {
            result.expect("a scheduled valid host still accepts the forward");
            assert_eq!(heard.unwrap(), ["TOKEN tok version\n", "status\n"]);
            assert!(relayed);
        }
    }
}

#[cfg(unix)]
#[test]
fn required_proxy_keeps_a_scheduled_host_present() {
    for age in [
        std::time::Duration::from_secs(3),
        std::time::Duration::from_secs(61),
    ] {
        let expired = age >= aterm_spec::wait::MUST_HAPPEN;
        let dir = crate::test_scratch::short_dir("scheduled-host-look");
        let sock = dir.path().join("host.sock");
        let listener = aterm_uds::CtlListener::bind(&sock).unwrap();
        let identity = aterm_uds::listener::listener_at(&sock);
        let (ready, listening) = std::sync::mpsc::channel();
        let host = std::thread::spawn(move || -> std::io::Result<String> {
            let (stream, _) = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )?;
            stream.set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))?;
            stream.set_write_timeout(Some(aterm_spec::wait::MUST_HAPPEN))?;
            ready.send(()).unwrap();
            let mut reader = BufReader::new(&stream);
            let mut probe = String::new();
            reader.read_line(&mut probe)?;
            if probe != "TOKEN tok version\n" {
                return Err(std::io::Error::other("wrong native probe"));
            }
            if !expired {
                (&stream).write_all(b"OK version=host\n")?;
            }
            let mut rest = String::new();
            reader.read_to_string(&mut rest)?;
            if !rest.is_empty() {
                return Err(std::io::Error::other("a look sent a command"));
            }
            Ok(probe)
        });
        let look = ForwardHost {
            sock: sock.to_str().unwrap().to_owned(),
            token: "tok".into(),
            pid: Some(std::process::id()),
            listener: identity,
        };
        let gone = look.gone_line_with_clock(|| {
            listening
                .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                .unwrap();
            std::time::Instant::now() - age
        });
        let heard = host.join().unwrap();
        if expired {
            assert_eq!(
                gone,
                Some(aterm_types::control_verbs::host_gone_line(
                    std::process::id(),
                    sock.to_str().unwrap()
                ))
            );
        } else {
            assert_eq!(
                gone, None,
                "a scheduling pause is not proof that the native host went away"
            );
        }
        assert_eq!(heard.unwrap(), "TOKEN tok version\n");
    }
}

#[test]
fn a_child_that_accepts_nothing_fails_the_forward_fast() {
    let dir = crate::test_scratch::short_dir("a hop's socket");
    let sock = dir.path().join("wedged.sock");
    let _listener = aterm_uds::CtlListener::bind(&sock).expect("bind, never accept");
    let (_client_app, client_relay) = CtlStream::pair().expect("pair");
    let started = std::time::Instant::now();
    let mut relayed = false;
    let result = deliver_then_relay(
        hop_to(&sock, "tok-hex"),
        "text",
        &client_relay,
        &[],
        std::time::Duration::from_millis(400),
        |_, _, _| {
            relayed = true;
            Ok(())
        },
    );
    let error = result.expect_err("a child that accepts nothing is an error");
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert!(
        error.to_string().contains("accepted no connection"),
        "{error}"
    );
    assert!(!relayed, "nothing is relayed to a child that never served");
    assert!(
        started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
        "the probe bound held: {:?}",
        started.elapsed()
    );
}

#[test]
fn a_child_that_refuses_the_probe_never_receives_the_verb() {
    let dir = crate::test_scratch::short_dir("a hop's socket");
    let sock = dir.path().join("refuse.sock");
    let listener = aterm_uds::CtlListener::bind(&sock).expect("bind");
    let child = std::thread::spawn(move || {
        let (mut conn, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        )
        .expect("accept");
        let mut rdr = BufReader::new(conn.try_clone().unwrap());
        let mut first = String::new();
        rdr.read_line(&mut first).unwrap();
        conn.write_all(b"ERR auth\n").unwrap();
        let mut rest = String::new();
        let _ = rdr.read_to_string(&mut rest);
        rest
    });
    let (_client_app, client_relay) = CtlStream::pair().expect("pair");
    let result = deliver_then_relay(
        hop_to(&sock, "stale"),
        "key enter",
        &client_relay,
        &[],
        std::time::Duration::from_secs(5),
        |_, _, _| Ok(()),
    );
    assert!(result.is_err(), "a refused probe is a failed forward");
    assert_eq!(child.join().unwrap(), "", "the verb was never sent");
}

#[test]
fn a_probe_surplus_is_refused_before_delivery() {
    for surplus in [true, false] {
        let dir = crate::test_scratch::short_dir("a hop's socket");
        let sock = dir.path().join("surplus.sock");
        let listener = aterm_uds::CtlListener::bind(&sock).expect("bind");
        let host = std::thread::spawn(move || {
            let (conn, _) = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )
            .expect("accept");
            let mut rdr = BufReader::new(conn.try_clone().unwrap());
            let mut probe = String::new();
            rdr.read_line(&mut probe).unwrap();
            let answer: &[u8] = if surplus {
                b"OK version=host\nOK unasked\n"
            } else {
                b"OK version=host\n"
            };
            (&conn).write_all(answer).unwrap();
            let mut verb = String::new();
            let _ = rdr.read_line(&mut verb);
            verb
        });
        let (_client_app, client_relay) = CtlStream::pair().expect("pair");
        let mut relayed = false;
        let result = deliver_then_relay(
            hop_to(&sock, "tok"),
            "text",
            &client_relay,
            &[],
            aterm_spec::wait::MUST_HAPPEN,
            |_, _, _| {
                relayed = true;
                Ok(())
            },
        );
        let heard = host.join().unwrap();
        if surplus {
            let error = result.expect_err("a surplus fails the forward");
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidData, "{error}");
            assert_eq!(heard, "", "the verb was never sent");
            assert!(!relayed, "nothing is relayed past a surplus");
        } else {
            result.expect("a probe answered alone is a forward");
            assert_eq!(heard, "text\n", "the verb follows the probe");
            assert!(relayed, "the relay starts once the verb is sent");
        }
    }
}

/// A saturated host still answers: a listener with no lane free writes its busy
/// line and closes without reading, so the probe's write fails (`EPIPE`) and the
/// line left behind is the answer [`ForwardHost::serves`] takes as proof the
/// host is alive.
#[cfg(unix)]
#[test]
fn a_busy_line_left_behind_answers_a_probe_whose_write_failed() {
    let dir = crate::test_scratch::short_dir("a hop's socket");
    let sock = dir.path().join("busy.sock");
    let listener = aterm_uds::CtlListener::bind(&sock).expect("bind");
    let look = CtlStream::connect(&sock).expect("connect");
    let (conn, _) = aterm_uds::accept(
        &listener,
        aterm_uds::AcceptWait::Forever,
        aterm_uds::AcceptedMode::Blocking,
    )
    .expect("accept");
    (&conn)
        .write_all(b"ERR control server busy; retry\n")
        .unwrap();
    drop(conn);
    let answer = probe_answer(
        &look,
        "tok",
        std::time::Duration::from_secs(60),
        std::time::Instant::now,
    );
    assert_eq!(
        answer.expect("the line left behind"),
        "ERR control server busy; retry"
    );
}

#[test]
fn a_host_that_goes_away_under_a_forward_leaves_a_typed_line() {
    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Host {
        Gone,
        Reopened,
        ServesOn,
    }
    for case in [Host::Gone, Host::Reopened, Host::ServesOn] {
        let dir = crate::test_scratch::short_dir("a hop's socket");
        let sock = dir.path().join("host.sock");
        let sock_s = sock.to_string_lossy().into_owned();
        let listener = aterm_uds::CtlListener::bind(&sock).expect("bind host");
        let (fresh_path, path) = (dir.path().join("fresh.sock"), sock.clone());
        // What the host thread leaves alive until it is joined, after the relay ends.
        #[cfg(unix)]
        type GoneSocket = std::os::fd::OwnedFd;
        #[cfg(not(unix))]
        type GoneSocket = ();
        let host = std::thread::spawn(move || -> Option<GoneSocket> {
            let (conn, _) = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )
            .expect("accept");
            let mut rdr = BufReader::new(conn.try_clone().unwrap());
            let mut probe = String::new();
            rdr.read_line(&mut probe).unwrap();
            (&conn).write_all(b"OK version=host\n").unwrap();
            let mut verb = String::new();
            rdr.read_line(&mut verb).unwrap();
            let listener = match case {
                Host::Gone => {
                    // A gone host leaves a socket file nothing listens on. A dropped listener
                    // alone is not one: a child another test spawned can hold a copy of its
                    // descriptor until its exec, and the relay's gone check would then connect
                    // into that copy's backlog and wait FORWARD_HANG, as long as this test's
                    // hang detector, for a probe answer nobody sends. The path refuses before
                    // the connection drops, since that drop is what starts the relay's check.
                    #[cfg(unix)]
                    let gone = Some(crate::test_socket::never_listening_socket(&path));
                    #[cfg(not(unix))]
                    let gone = None;
                    drop(listener);
                    drop(conn);
                    return gone;
                }
                Host::Reopened => {
                    let fresh = aterm_uds::CtlListener::bind(&fresh_path).expect("rebind");
                    std::fs::rename(&fresh_path, &path).expect("over the path");
                    drop(listener);
                    let _ = conn.shutdown(std::net::Shutdown::Both);
                    fresh
                }
                Host::ServesOn => listener,
            };
            drop(rdr);
            drop(conn);
            loop {
                let (conn, _) = aterm_uds::accept(
                    &listener,
                    aterm_uds::AcceptWait::Forever,
                    aterm_uds::AcceptedMode::Blocking,
                )
                .expect("accept a look");
                let mut line = String::new();
                let _ = BufReader::new(&conn).read_line(&mut line);
                if line.starts_with("stop") {
                    return None;
                }
                if line.ends_with("version\n") {
                    let _ = (&conn).write_all(b"OK version=host\n");
                }
            }
        });
        let (client_app, client_relay) = CtlStream::pair().expect("pair");
        let hop = hop_to(&sock, "tok-hex");
        let relay = std::thread::spawn(move || relay_hop(hop, "turn hi", &client_relay, &[]));
        let told = String::from_utf8(read_to_end_of_file(&client_app, |read| {
            format!(
                "{case:?}: the relay's EOF, after {:?}",
                String::from_utf8_lossy(read)
            )
        }))
        .expect("the relay passes text");
        drop(client_app);
        relay
            .join()
            .unwrap()
            .expect("a delivered verb is never a failed forward");
        if case != Host::Gone
            && let Ok(conn) = CtlStream::connect(&sock)
        {
            let _ = (&conn).write_all(b"stop\n");
        }
        if case == Host::ServesOn || cfg!(windows) {
            assert_eq!(told, "", "a host that serves on ended it on purpose");
        } else {
            assert_eq!(
                told,
                format!("ERR gone pid={} sock={sock_s}\n", std::process::id()),
                "{case:?}: the host's going is said before the EOF"
            );
        }
        host.join().unwrap();
    }
}

#[test]
fn relay_hop_pipes_handshake_and_response() {
    let dir_guard = crate::test_scratch::short_dir("the child's socket");
    let sock = dir_guard.path().join("child.sock");
    let listener = aterm_uds::CtlListener::bind(&sock).expect("bind child");

    let child = std::thread::spawn(move || {
        let (mut conn, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        )
        .expect("accept");
        let mut rdr = BufReader::new(conn.try_clone().unwrap());
        let mut first = String::new();
        rdr.read_line(&mut first).unwrap();
        assert_eq!(first, "TOKEN tok-hex version\n", "handshake + probe");
        conn.write_all(b"OK version=child\n").unwrap();
        let mut verb = String::new();
        rdr.read_line(&mut verb).unwrap();
        assert_eq!(verb, "screen\n", "the verb follows the probe");
        conn.write_all(b"OK 1\n{\"k\":1}\n").unwrap();
        conn.write_all(&[0xffu8, b'\n']).unwrap();
        conn.flush().unwrap();
        let mut more = String::new();
        let _ = rdr.read_line(&mut more);
        let _ = conn.write_all(more.as_bytes());
        let _ = conn.shutdown(std::net::Shutdown::Both);
    });

    let (client_app, client_relay) = CtlStream::pair().expect("pair");
    let hop = hop_to(&sock, "tok-hex");
    let relay = std::thread::spawn(move || {
        relay_hop(hop, "screen", &client_relay, &[]).expect("relay");
    });

    let mut app_rdr = BufReader::new(client_app.try_clone().unwrap());
    let mut status = String::new();
    app_rdr.read_line(&mut status).unwrap();
    assert_eq!(status, "OK 1\n");
    let mut body = String::new();
    app_rdr.read_line(&mut body).unwrap();
    assert_eq!(body, "{\"k\":1}\n");
    let mut raw = Vec::new();
    let mut byte = [0u8; 2];
    std::io::Read::read_exact(&mut app_rdr, &mut byte).unwrap();
    raw.extend_from_slice(&byte);
    assert_eq!(raw, vec![0xff, b'\n']);

    (&client_app).write_all(b"ping\n").unwrap();
    (&client_app).flush().unwrap();
    let mut echo = String::new();
    app_rdr.read_line(&mut echo).unwrap();
    assert_eq!(echo, "ping\n");

    drop(app_rdr);
    hang_up(client_app);
    join_after_hang_up(relay, "the relay to end on its client's hang-up");
    join_after_hang_up(child, "the stand-in child to finish serving");
}

#[test]
fn one_proxy_hop_preserves_guard_until_original_client_ack() {
    struct Guard(std::sync::Arc<std::sync::atomic::AtomicBool>);
    impl Drop for Guard {
        fn drop(&mut self) {
            self.0.store(false, std::sync::atomic::Ordering::Release);
        }
    }

    let alive = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let (mut original, relay_client) = CtlStream::pair().unwrap();
    let (relay_child, mut child) = CtlStream::pair().unwrap();
    original
        .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
        .unwrap();

    let relay = std::thread::spawn(move || {
        relay_bidirectional(&relay_client, &relay_child, unknown_host()).unwrap()
    });
    let child_alive = std::sync::Arc::clone(&alive);
    let service = std::thread::spawn(move || {
        let _guard = Guard(child_alive);
        let mut reader = BufReader::new(child.try_clone().unwrap());
        let mut request = String::new();
        reader.read_line(&mut request).unwrap();
        assert_eq!(request, "image shot.png\n");
        child.write_all(b"OK 1 1 /remote/shot.png\n").unwrap();
        child
            .write_all(b"ACK-CHALLENGE 00112233445566778899aabbccddeeff\n")
            .unwrap();
        child.flush().unwrap();

        let mut ack = String::new();
        reader.read_line(&mut ack).unwrap();
        assert_eq!(ack.trim_end(), "ACK 00112233445566778899aabbccddeeff");
        let _ = child.shutdown(std::net::Shutdown::Both);
    });

    original.write_all(b"image shot.png\n").unwrap();
    original.flush().unwrap();
    let mut reader = BufReader::new(original.try_clone().unwrap());
    let mut response = String::new();
    reader.read_line(&mut response).unwrap();
    assert_eq!(response, "OK 1 1 /remote/shot.png\n");
    let mut challenge = String::new();
    reader.read_line(&mut challenge).unwrap();
    assert_eq!(
        challenge,
        "ACK-CHALLENGE 00112233445566778899aabbccddeeff\n"
    );
    assert!(
        alive.load(std::sync::atomic::Ordering::Acquire),
        "relay buffering/flushing the response cannot release the child guard"
    );

    original
        .write_all(
            format!(
                "{}{}\n",
                aterm_types::control_verbs::ARTIFACT_REPLY_ACK_PREFIX,
                "00112233445566778899aabbccddeeff"
            )
            .as_bytes(),
        )
        .unwrap();
    original.flush().unwrap();
    join_after_hang_up(service, "the child to read the ACK and release its guard");
    assert!(
        !alive.load(std::sync::atomic::Ordering::Acquire),
        "the original client's ACK crosses the proxy before guard release"
    );
    drop(reader);
    hang_up(original);
    join_after_hang_up(relay, "the relay to end on its client's hang-up");
}

/// Which of `relay_bidirectional`'s two error arms a teardown case drives.
#[cfg(unix)]
#[derive(Clone, Copy, Debug)]
enum RelayFault {
    /// The child-bound socket loses its send half: the main thread's
    /// client-to-child write fails the moment the original client sends a
    /// request, while the worker is parked reading a child that never speaks.
    ChildBound,
    /// The client-bound socket loses its send half: the worker's
    /// child-to-client write fails the moment the child answers, while the
    /// main thread is parked reading an original client that never speaks.
    ClientBound,
}

/// Tear the real relay down under `fault` while both peers stay open and
/// otherwise silent, and project what it left onto `RelayTeardown`'s
/// variables.
#[cfg(unix)]
fn relay_teardown_under(
    fault: RelayFault,
) -> (std::collections::BTreeMap<&'static str, i64>, bool) {
    let (original, relay_client) = CtlStream::pair().expect("client pair");
    let (relay_child, child_peer) = CtlStream::pair().expect("child pair");
    let client_probe = relay_client.try_clone().expect("clone");
    let child_probe = relay_child.try_clone().expect("clone");
    let faulted = match fault {
        RelayFault::ChildBound => &relay_child,
        RelayFault::ClientBound => &relay_client,
    };
    faulted
        .shutdown(std::net::Shutdown::Write)
        .expect("inject the send fault");
    let (returned_tx, returned_rx) = std::sync::mpsc::channel();
    let relay = std::thread::spawn(move || {
        let result = relay_bidirectional(&relay_client, &relay_child, unknown_host());
        let _ = returned_tx.send(());
        result
    });

    let (mut speaker, said) = match fault {
        RelayFault::ChildBound => (&original, b"screen\n".as_slice()),
        RelayFault::ClientBound => (&child_peer, b"OK reply\n".as_slice()),
    };
    speaker.write_all(said).expect("speak");
    let done = read_to_end_of_file(speaker, |read| {
        format!(
            "{fault:?}: the speaker's end-of-file, after {:?}",
            String::from_utf8_lossy(read)
        )
    })
    .is_empty();
    let returned = returned_rx
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .is_ok();
    let read_open = |probe: &CtlStream| {
        probe.set_nonblocking(true).expect("non-blocking probe");
        let mut byte = [0u8; 1];
        match (&*probe).read(&mut byte) {
            Ok(0) => 0,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => 1,
            other => panic!("{fault:?}: unexpected probe read: {other:?}"),
        }
    };
    let observed = std::collections::BTreeMap::from([
        ("child_read_open", read_open(&child_probe)),
        ("client_read_open", read_open(&client_probe)),
        ("done", i64::from(done)),
    ]);
    hang_up(child_peer);
    hang_up(original);
    let _ = join_after_hang_up(relay, "the relay to end once both peers hung up");
    (observed, returned)
}

#[test]
#[cfg(unix)]
fn relay_teardown_conforms_to_readers_unblock_after_teardown() {
    use aterm_spec::verify::validate_transition_tiered;
    let model = aterm_models_session::relay_teardown_model();
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    for fault in [RelayFault::ChildBound, RelayFault::ClientBound] {
        let (observed, returned) = relay_teardown_under(fault);
        let (ok, why) = validate_transition_tiered(
            &model,
            &[],
            &model.init_state(),
            &observed,
            Some("Teardown"),
            "RelayTeardown(relay_bidirectional)",
        );
        assert!(
            ok,
            "{fault:?}: the relay tore down to {observed:?}, not the model's Teardown \
             (a read half a pump could still park on) — {why}"
        );
        assert!(
            returned,
            "{fault:?}: the relay did not return: a pump is still parked"
        );
        let (admitted, _) = validate_transition_tiered(
            &buggy,
            &[],
            &buggy.init_state(),
            &observed,
            Some("Teardown"),
            "RelayTeardown(Buggy=1)",
        );
        assert!(
            !admitted,
            "{fault:?}: the shutdown(Write)-only mutant admitted the real teardown, so \
             this conformance cannot tell the two apart"
        );
    }
}

#[test]
#[cfg(unix)]
fn the_host_pump_ends_on_the_stop_flag_without_a_wake() {
    let (host, _silent_host_peer) = CtlStream::pair().expect("host pair");
    let (client, _client_peer) = CtlStream::pair().expect("client pair");
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stopped = std::sync::Arc::clone(&stop);
    let (waiting_tx, waiting_rx) = std::sync::mpsc::channel();
    let (ended_tx, ended_rx) = std::sync::mpsc::channel();
    let pump = std::thread::spawn(move || {
        let ready = || {
            let _ = waiting_tx.send(());
            readable_unless_stopped(&host, &stopped)
        };
        let leg = copy_host_to_client(&mut &host, &mut &client, ready);
        let _ = ended_tx.send(matches!(leg, HostLeg::HostEnded));
    });
    waiting_rx
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the pump reaches its wait on the silent host");
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    let ended = ended_rx
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the pump ends on the stop flag with no wake from any shutdown");
    assert!(ended, "a stopped pump reads as the host's side ending");
    pump.join().expect("the pump");
}

/// What the socket `end` names holds for its reader, peeked without blocking
/// or consuming: the bytes queued (up to 64), `Ok(0)` at its end of file, and
/// `WouldBlock` for an open socket with nothing queued.
#[cfg(unix)]
fn peek_now(end: &CtlStream) -> std::io::Result<usize> {
    use std::os::fd::AsRawFd as _;
    let mut buf = [0u8; 64];
    // SAFETY: `end` is borrowed for the call, `buf` is writable, and `MSG_PEEK | MSG_DONTWAIT` does not block.
    let n = unsafe {
        libc::recv(
            end.as_raw_fd(),
            buf.as_mut_ptr().cast(),
            buf.len(),
            libc::MSG_PEEK | libc::MSG_DONTWAIT,
        )
    };
    usize::try_from(n).map_err(|_| std::io::Error::last_os_error())
}

#[test]
#[cfg(unix)]
fn a_child_that_half_closed_reads_eof_when_the_relay_ends_though_a_copy_survives() {
    let (original, relay_client) = CtlStream::pair().expect("client pair");
    let (relay_child, child) = CtlStream::pair().expect("child pair");
    let forked_copy = relay_child
        .try_clone()
        .expect("a surviving copy of the relay's end");
    let relay = std::thread::spawn(move || {
        relay_bidirectional(&relay_client, &relay_child, unknown_host())
    });

    (&child)
        .write_all(b"OK answer\n")
        .expect("the child answers");
    child
        .shutdown(std::net::Shutdown::Write)
        .expect("the child half-closes");
    let answer = read_to_end_of_file(&original, |read| {
        format!(
            "the answer, then the relay's half-close: {:?} read; the relay's end of \
             the child {}; the relay {}",
            String::from_utf8_lossy(read),
            match peek_now(&forked_copy) {
                Ok(0) => "had nothing left to read".to_string(),
                Ok(n) => format!("still held {n} bytes the relay never read"),
                Err(e) => format!("could not be peeked: {e}"),
            },
            if relay.is_finished() {
                "had returned"
            } else {
                "was still running"
            },
        )
    });
    assert_eq!(String::from_utf8_lossy(&answer), "OK answer\n");
    original
        .shutdown(std::net::Shutdown::Write)
        .expect("the client is done");
    join_after_hang_up(relay, "the relay to end once both sides finished").expect("the relay");

    child.set_nonblocking(true).expect("non-blocking");
    assert_eq!(
        (&child).read(&mut [0u8; 16]).map_err(|e| e.kind()),
        Ok(0),
        "the child read no end-of-file from a relay that ended while a copy of its end lived"
    );
    drop(forked_copy);
}

#[test]
fn edge_tokens_file_is_0600_and_read_is_repeatable() {
    let dir = std::env::temp_dir().join(format!("aterm-edges-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let sid = SessionId::generate();
    let (r, w, s) = ("aa".repeat(32), "bb".repeat(32), "cc".repeat(32));
    let file = write_edge_tokens(&dir, &sid, &r, &w, &s).expect("write");
    let path = file.path().to_string_lossy().into_owned();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "edge-token file must be 0600, got {mode:o}");
    }
    let first = read_edge_tokens(&path);
    let second = read_edge_tokens(&path);
    assert_eq!(first, Some((r.clone(), w.clone(), s.clone())), "first read");
    assert_eq!(
        second,
        Some((r.clone(), w.clone(), s.clone())),
        "second read still succeeds (persists)"
    );
    #[cfg(unix)]
    {
        assert!(
            write_edge_tokens(&dir, &sid, "11", "22", "33").is_none(),
            "a locked edge file cannot be overwritten"
        );
        assert_eq!(
            read_edge_tokens(&path),
            Some((r, w, s)),
            "a refused overwrite neither truncates nor unlinks the live owner's file"
        );
    }
    drop(ChildProxy::new(None));
    assert!(read_edge_tokens(&path).is_some(), "only its own file");
    drop(ChildProxy::new(Some(file)));
    assert!(
        read_edge_tokens(&path).is_none(),
        "removed by owning parent"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A dead parent's edge file is swept, a live one's never. A parent holds
/// its file's owner lock for the child's life; the sweep removes a file
/// whose lock is free (here, its handle dropped without the clean retire's
/// removal, as a crash leaves it).
#[cfg(unix)]
#[test]
fn a_dead_parents_edge_file_is_swept_and_a_live_ones_never() {
    let dir = std::env::temp_dir().join(format!("aterm-edges-sweep-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let hex = |c: &str| c.repeat(32);
    let live = write_edge_tokens(
        &dir,
        &SessionId::generate(),
        &hex("aa"),
        &hex("bb"),
        &hex("cc"),
    )
    .expect("live");
    let crashed = write_edge_tokens(
        &dir,
        &SessionId::generate(),
        &hex("dd"),
        &hex("ee"),
        &hex("ff"),
    )
    .expect("crashed");
    let crashed_path = crashed.path().to_path_buf();
    let forked = crate::parked_fork::ParkedChild::fork();
    drop(crashed);
    let edges = dir.join("edges");
    let empty = edges.join(SessionId::generate().as_str());
    std::fs::write(&empty, b"").unwrap();
    let next = write_edge_tokens(
        &dir,
        &SessionId::generate(),
        &hex("11"),
        &hex("22"),
        &hex("33"),
    )
    .expect("next");
    assert!(crashed_path.exists(), "a provisioning sweeps nothing");
    sweep_dead_edge_files(&edges);
    assert!(
        forked.still_parked(),
        "the sweep ran before the fork's exec"
    );
    forked.release();
    assert!(!crashed_path.exists(), "the dead parent's file is swept");
    assert!(live.path().exists(), "a live parent's file is never swept");
    assert!(empty.exists(), "an empty file may be mid-write: left");
    assert!(next.path().exists());
    drop((live, next));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn publish_session_lifecycle_follows_bind_and_close() {
    let _guard = self_sock_test_guard();
    let dir = std::env::temp_dir().join(format!("aterm-pub-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let mirror = std::env::temp_dir().join(format!("aterm-pub-mirror-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&mirror);
    set_mirror_dir_override(Some(mirror.clone()));
    let real_default = aterm_uds::control_socket_dir();
    let sid = SessionId::generate();
    let nonce = LaunchNonce::generate();

    clear_self_sock();
    publish_session(&sid, &nonce);
    assert!(
        read_graph_entry(&dir, sid_of(&sid)).is_none(),
        "no entry pre-bind"
    );

    let sock = dir.join("aterm-88001.sock").to_string_lossy().into_owned();
    let canon_sock = published_sock_spelling(
        &std::fs::canonicalize(&dir)
            .unwrap()
            .join("aterm-88001.sock")
            .to_string_lossy(),
    );
    set_self_sock(&dir, &sock);
    publish_session(&sid, &nonce);
    assert!(
        flush_publications(aterm_spec::wait::MUST_HAPPEN),
        "the write lands"
    );
    let (got_sock, got_nonce) = read_graph_entry(&dir, sid_of(&sid)).expect("published");
    assert_eq!(got_sock, canon_sock);
    assert!(got_nonce.ct_eq(&nonce));

    assert!(
        read_graph_entry(&mirror, sid_of(&sid)).is_some(),
        "mirror lands in the scratch override dir"
    );
    if let Some(real) = real_default.as_ref() {
        assert!(
            read_graph_entry(real, sid_of(&sid)).is_none(),
            "the mirror must NEVER write into the user's real control dir"
        );
    }

    unpublish_session(&sid);
    assert!(
        flush_publications(aterm_spec::wait::MUST_HAPPEN),
        "the retire lands"
    );
    assert!(
        read_graph_entry(&dir, sid_of(&sid)).is_none(),
        "retired on close"
    );
    assert!(
        read_graph_entry(&mirror, sid_of(&sid)).is_none(),
        "mirror entry retired too"
    );
    if let Some(real) = real_default.as_ref() {
        assert!(
            read_graph_entry(real, sid_of(&sid)).is_none(),
            "still nothing in the real control dir after retire"
        );
    }
    clear_self_sock();
    set_mirror_dir_override(None);
    let _ = std::fs::remove_dir_all(&mirror);

    std::fs::write(dir.join("aterm-88001.token"), "  feed1234\n").unwrap();
    assert_eq!(read_sibling_token(&sock).as_deref(), Some("feed1234"));
    std::fs::write(dir.join("aterm-88001.token"), "\n").unwrap();
    assert_eq!(read_sibling_token(&sock), None);
    let other = dir.join("aterm-88002.sock").to_string_lossy().into_owned();
    assert_eq!(read_sibling_token(&other), None);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_session_publication_is_queued_off_the_caller_and_kept_in_order() {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            clear_self_sock();
            let _ = flush_publications(aterm_spec::wait::MUST_HAPPEN);
            set_mirror_dir_override(None);
        }
    }
    let _guard = self_sock_test_guard();
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    let (dir, mirror) = (scratch.path().join("own"), scratch.path().join("mirror"));
    for d in [&dir, &mirror] {
        std::fs::create_dir_all(d).expect("scratch dir");
    }
    set_mirror_dir_override(Some(mirror.clone()));
    let _reset = Reset;
    set_self_sock(&dir, &dir.join("aterm-88003.sock").to_string_lossy());

    let (held, release) = std::sync::mpsc::sync_channel(0);
    queue_publication(Publication::Flush(held));
    let (kept, closed) = (SessionId::generate(), SessionId::generate());
    publish_session(&kept, &LaunchNonce::generate());
    publish_session(&closed, &LaunchNonce::generate());
    unpublish_session(&closed);
    assert!(
        read_graph_entry(&dir, sid_of(&kept)).is_none(),
        "publish_session wrote in the caller's turn"
    );
    let stranger = SessionId::generate();
    let elsewhere = {
        let stranger = stranger.clone();
        std::thread::spawn(move || {
            publish_session(&stranger, &LaunchNonce::generate());
            (
                self_sock_path(),
                flush_publications(std::time::Duration::ZERO),
            )
        })
        .join()
        .expect("another test's thread")
    };
    assert_eq!(
        elsewhere,
        (None, true),
        "another test read this one's socket or waited behind its queue"
    );
    release
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the publication thread reached the held write");
    assert!(
        flush_publications(aterm_spec::wait::MUST_HAPPEN),
        "the queue drained"
    );
    assert!(
        read_graph_entry(&dir, sid_of(&kept)).is_some(),
        "the publish landed"
    );
    assert!(
        read_graph_entry(&mirror, sid_of(&kept)).is_some(),
        "and its mirror"
    );
    assert!(
        read_graph_entry(&dir, sid_of(&closed)).is_none()
            && read_graph_entry(&mirror, sid_of(&closed)).is_none(),
        "a session retired behind its publish leaves no entry"
    );
    assert!(
        read_graph_entry(&dir, sid_of(&stranger)).is_none(),
        "another test's session was published into this one's dir"
    );
}

/// A sibling's token file is read the way every control client reads its
/// own: one planted as a symlink is refused rather than followed, and one
/// larger than any token is refused rather than read whole.
#[cfg(unix)]
#[test]
fn a_symlinked_or_oversized_sibling_token_is_refused() {
    let dir = std::env::temp_dir().join(format!("aterm-sibtok-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    let sock = dir.join("aterm-88003.sock").to_string_lossy().into_owned();
    let token = dir.join("aterm-88003.token");
    let elsewhere = dir.join("elsewhere");
    std::fs::write(&elsewhere, "feed1234\n").expect("a token-shaped file");
    std::os::unix::fs::symlink(&elsewhere, &token).expect("plant");
    assert_eq!(read_sibling_token(&sock), None, "a symlink is followed");
    std::fs::remove_file(&token).expect("unplant");
    std::fs::write(&token, "f".repeat(64 * 1024)).expect("oversized");
    assert_eq!(read_sibling_token(&sock), None, "an oversized file is read");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_quit_retires_only_its_own_graph_entries() {
    let dir = crate::test_scratch::short_dir("a hop's socket");
    let ours = dir.path().join("aterm-cand.sock");
    let theirs = dir.path().join("aterm-parent.sock");
    let (mine, parents) = (SessionId::generate(), SessionId::generate());
    write_graph_entry(
        dir.path(),
        &mine,
        ours.to_str().unwrap(),
        &LaunchNonce::generate(),
    );
    write_graph_entry(
        dir.path(),
        &parents,
        theirs.to_str().unwrap(),
        &LaunchNonce::generate(),
    );
    for sid in [&mine, &parents] {
        remove_own_graph_entry(dir.path(), sid, ours.to_str().unwrap());
    }
    let exists = |sid: &SessionId| entry_path(dir.path(), sid).is_some_and(|p| p.exists());
    assert!(!exists(&mine), "its own entry goes");
    assert!(exists(&parents), "the parent's entry stays with the parent");
}
