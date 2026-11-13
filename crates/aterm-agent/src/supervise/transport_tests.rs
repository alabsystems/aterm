// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;

fn serve(
    tag: &str,
    replies: Vec<Option<&'static str>>,
) -> (String, std::thread::JoinHandle<Vec<String>>) {
    let dir = crate::supervise::test_scratch_path("relayctl", tag);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let sock = dir.join("a.sock");
    let listener = UnixListener::bind(&sock).expect("bind");
    let path = sock.to_string_lossy().into_owned();
    let h = std::thread::spawn(move || {
        let mut seen = Vec::new();
        let mut replies = replies.into_iter().peekable();
        'conns: loop {
            let Ok((stream, _)) = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            ) else {
                break;
            };
            let mut w = stream.try_clone().expect("clone");
            let mut r = BufReader::new(stream);
            loop {
                let mut line = String::new();
                if r.read_line(&mut line).unwrap_or(0) == 0 {
                    if replies.peek().is_none() {
                        break 'conns;
                    }
                    continue 'conns;
                }
                let line = line.trim_end().to_string();
                if line.starts_with("AUTH ") {
                    seen.push("AUTH".to_string());
                    continue;
                }
                seen.push(line);
                match replies.next() {
                    Some(Some(reply)) => {
                        let _ = w.write_all(reply.as_bytes());
                    }
                    Some(None) => continue 'conns,
                    None => break 'conns,
                }
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
        seen
    });
    (path, h)
}

#[test]
fn a_refused_preferred_token_falls_back_to_the_one_beside_the_socket() {
    let dir = crate::supervise::test_scratch_path("relayctl", "prefer");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let sock = dir.join("a.sock");
    let names = aterm_types::control_socket::token_names_for_sock("a.sock");
    std::fs::write(dir.join(&names[0]), "beside-tok\n").expect("token");
    let listener = UnixListener::bind(&sock).expect("bind");
    let path = sock.to_string_lossy().into_owned();
    let h = std::thread::spawn(move || {
        let mut seen: Vec<(String, String)> = Vec::new();
        for _ in 0..3 {
            let Ok((stream, _)) = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            ) else {
                break;
            };
            let mut w = stream.try_clone().expect("clone");
            let mut r = BufReader::new(stream);
            let mut auth = String::new();
            let _ = r.read_line(&mut auth);
            let token = auth.trim_end().trim_start_matches("AUTH ").to_string();
            loop {
                let mut line = String::new();
                if r.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                seen.push((token.clone(), line.trim_end().to_string()));
                if token != "beside-tok" {
                    let _ = w.write_all(b"ERR auth\n");
                    break;
                }
                let _ = w.write_all(b"OK\n");
            }
        }
        seen
    });
    let mut c = RelayCtl::preferring(Endpoint::Socket(path.clone()), "memory-tok".to_string());
    let first = c.call(&["@s-1", "send", "x"]).expect("send");
    assert_eq!(
        (first.code, first.stdout.as_str()),
        (0, "OK\n"),
        "{first:?}"
    );
    let second = c.call(&["@s-1", "send", "y"]).expect("send");
    assert_eq!(second.code, 0, "{second:?}");
    drop(c);
    let mut plain = RelayCtl::new(Endpoint::Socket(path), Some("memory-tok".to_string()));
    let refused = plain.call(&["@s-1", "send", "z"]).expect("send");
    assert!(
        refused.code == 1 && refused.stderr.contains("ERR auth"),
        "{refused:?}"
    );
    drop(plain);
    let seen = h.join().expect("server");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        seen,
        vec![
            ("memory-tok".to_string(), "@s-1 send x".to_string()),
            ("beside-tok".to_string(), "@s-1 send x".to_string()),
            ("beside-tok".to_string(), "@s-1 send y".to_string()),
            ("memory-tok".to_string(), "@s-1 send z".to_string()),
        ]
    );
}

#[test]
fn replies_render_as_aterm_ctl_prints_them() {
    let (sock, h) = serve(
        "render",
        vec![
            Some("OK 2\n{\"rows\":[]}\nsecond\n"),
            Some("OK timeout\n"),
            Some("ERR halted reason=owner origin=local\n"),
            Some("OK skipped seq=7\n"),
            Some("OK 1 turn submitted=1 status=settled seq=9\nrow\n"),
            Some("OK 5\nhello"),
            Some("OK 1 incomplete capped\n10 0 1\n"),
            Some("OK title=- attention=-\n"),
            Some("OK 1 incomplete older\n10 0 1\n"),
            Some("OK 1 frame=open\nrow\n"),
        ],
    );
    let mut c = RelayCtl::new(Endpoint::Socket(sock), Some("tok".to_string()));
    let text = c
        .call(&["@s-1", "text", "--json", "tail=40"])
        .expect("text");
    assert_eq!(
        (text.code, text.stdout.as_str()),
        (0, "{\"rows\":[]}\nsecond\n")
    );
    let wait = c
        .call(&["@s-1", "await", "gone", "x", "timeout", "20"])
        .expect("await");
    assert!(
        wait.timed_out() && wait.stdout == "OK timeout\n",
        "{wait:?}"
    );
    let halted = c.call(&["@s-1", "key", "if=^x$", "1"]).expect("key");
    assert!(halted.is_err("halted") && !halted.lost(), "{halted:?}");
    let skipped = c.call(&["@s-1", "key", "if=^x$", "1"]).expect("key");
    assert!(skipped.skipped() && skipped.seq() == Some(7), "{skipped:?}");
    let turn = c
        .call(&["@s-1", "turn", "idle=600", "hello"])
        .expect("turn");
    assert_eq!(turn.stdout, "row\n");
    assert!(turn.submitted() && turn.turn_seq() == Some(9), "{turn:?}");
    let body = c.call(&["@s-1", "inbox", "get", "3"]).expect("inbox get");
    assert_eq!(body.stdout, "hello");
    let search = c.call(&["@s-1", "search", "x"]).expect("search");
    assert_eq!(search.stdout, "10 0 1\n");
    assert_eq!(
        search.stderr,
        "aterm-ctl: results stop at 100000 matches\naterm-ctl: OK 1 incomplete capped\n"
    );
    let meta = c.call(&["@s-1", "meta"]).expect("meta");
    assert_eq!(meta.stdout, "OK title=- attention=-\n");
    let search = c.call(&["@s-1", "search", "^›", "regex"]).expect("search");
    assert_eq!(
        (search.stdout.as_str(), search.stderr.as_str()),
        (
            "10 0 1\n",
            "aterm-ctl: older history not searched (search_history_lines)\naterm-ctl: OK \
             1 incomplete older\n"
        )
    );
    let text = c.call(&["@s-1", "text"]).expect("text");
    assert_eq!(
        (text.stdout.as_str(), text.stderr.as_str()),
        (
            "row\n",
            format!("aterm-ctl: {}\n", aterm_ctl::FRAME_OPEN_NOTE).as_str()
        )
    );
    drop(c);
    let seen = h.join().expect("server");
    assert_eq!(seen[0], "AUTH");
    assert_eq!(seen[1], "@s-1 text --json tail=40");
    assert_eq!(seen.len(), 11, "one connection, one AUTH: {seen:?}");
}

#[test]
fn a_dropped_connection_is_lost_and_the_next_request_dials_again() {
    let (sock, h) = serve("drop", vec![None, Some("OK 0\n")]);
    let mut c = RelayCtl::new(Endpoint::Socket(sock), Some("tok".to_string()));
    let r = c.call(&["text", "--json"]).expect("reply");
    assert!(r.lost(), "{r:?}");
    let r = c.call(&["text", "--json"]).expect("reply");
    assert!(r.ok(), "{r:?}");
    drop(c);
    let seen = h.join().expect("server");
    assert_eq!(
        seen.iter().filter(|l| *l == "AUTH").count(),
        2,
        "redialled: {seen:?}"
    );
}

#[test]
fn an_absent_socket_is_lost() {
    let mut c = RelayCtl::new(
        Endpoint::Socket("/nonexistent/aterm-relayctl.sock".to_string()),
        Some("tok".to_string()),
    );
    let r = c.call(&["meta"]).expect("reply");
    assert!(r.lost() && !r.ok(), "{r:?}");
}

#[test]
fn the_interrupter_ends_a_parked_request() {
    let dir = crate::supervise::ScratchDir::new("relayctl", "cut-parked");
    let sock = dir.join("a.sock");
    let listener = UnixListener::bind(&sock).expect("bind");
    let (parked_tx, parked_rx) = std::sync::mpsc::channel::<()>();
    // The server says when the await reached it, and keeps its end open until the join.
    let h = std::thread::spawn(move || -> std::io::Result<_> {
        let (s, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        )?;
        let mut r = BufReader::new(s.try_clone()?);
        let mut line = String::new();
        loop {
            line.clear();
            if r.read_line(&mut line)? == 0 {
                return Ok(s);
            }
            if line.starts_with("await inbox") {
                let _ = parked_tx.send(());
                return Ok(s);
            }
        }
    });
    let mut c = RelayCtl::new(
        Endpoint::Socket(sock.to_string_lossy().into_owned()),
        Some("tok".to_string()),
    );
    c.connect().expect("dial");
    let cut = c.interrupter().expect("interrupter");
    let t = std::thread::spawn(move || {
        parked_rx
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the await reached the server");
        cut();
    });
    let started = std::time::Instant::now();
    let r = c.call(&["await", "inbox", "since=0", "timeout", "300000"]);
    assert!(r.is_err(), "{r:?}");
    assert!(
        started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
        "before the await's own timeout"
    );
    assert!(
        c.call(&["meta"]).is_err(),
        "every request after the cut fails"
    );
    t.join().expect("cutter");
    drop(h.join());
}

#[test]
fn the_interrupter_hangs_up_a_connection_its_server_half_closed() {
    use std::io::Read as _;

    let dir = crate::supervise::ScratchDir::new("relayctl", "cut-half");
    let sock = dir.join("a.sock");
    let listener = UnixListener::bind(&sock).expect("bind");
    let mut c = RelayCtl::new(
        Endpoint::Socket(sock.to_string_lossy().into_owned()),
        Some("tok".to_string()),
    );
    c.connect().expect("dial");
    let (server, _) = aterm_uds::accept(
        &listener,
        aterm_uds::AcceptWait::Forever,
        aterm_uds::AcceptedMode::Blocking,
    )
    .expect("accept");
    server
        .shutdown(std::net::Shutdown::Write)
        .expect("the server half-closes toward the client");
    let cut = c.interrupter().expect("interrupter");
    cut();

    server.set_nonblocking(true).expect("non-blocking");
    let mut heard = Vec::new();
    let end = (&server).read_to_end(&mut heard).map_err(|e| e.kind());
    assert_eq!(String::from_utf8_lossy(&heard), "AUTH tok\n");
    assert!(
        end.is_ok(),
        "the server read no end-of-file from a cut connection the transport still held: \
         {end:?}"
    );
    drop(c);
}

struct Stand {
    name: &'static str,
    relays: Option<&'static str>,
    answers: usize,
}

struct Instance {
    sock: String,
    seen: Arc<Mutex<Vec<Vec<String>>>>,
    ended: std::sync::mpsc::Receiver<usize>,
    stop: Arc<AtomicBool>,
    accept: std::thread::JoinHandle<()>,
}

impl Instance {
    fn finish(self) -> Vec<Vec<String>> {
        self.stop.store(true, Ordering::SeqCst);
        drop(std::os::unix::net::UnixStream::connect(&self.sock));
        self.accept.join().expect("accept loop");
        let seen = self.seen.lock().expect("seen");
        seen.clone()
    }
}

fn instance(sock: std::path::PathBuf, stand: Stand) -> Instance {
    let listener = UnixListener::bind(&sock).expect("bind");
    let seen: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
    let stop: Arc<AtomicBool> = Arc::default();
    let (tx, ended) = std::sync::mpsc::channel();
    let stand = Arc::new(stand);
    let accept = {
        let (seen, stop) = (Arc::clone(&seen), Arc::clone(&stop));
        std::thread::spawn(move || {
            let mut serving = Vec::new();
            loop {
                let stream = aterm_uds::accept(
                    &listener,
                    aterm_uds::AcceptWait::Forever,
                    aterm_uds::AcceptedMode::Blocking,
                )
                .map(|(stream, _)| stream);
                let Ok(stream) = stream else { break };
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                let n = {
                    let mut seen = seen.lock().expect("seen");
                    seen.push(Vec::new());
                    seen.len() - 1
                };
                let (seen, stand, tx) = (Arc::clone(&seen), Arc::clone(&stand), tx.clone());
                serving.push(std::thread::spawn(move || {
                    serve_connection(stream, n, &stand, &seen);
                    let _ = tx.send(n);
                }));
            }
            for conn in serving {
                conn.join().expect("connection");
            }
        })
    };
    Instance {
        sock: sock.to_string_lossy().into_owned(),
        seen,
        ended,
        stop,
        accept,
    }
}

fn serve_connection(
    stream: std::os::unix::net::UnixStream,
    n: usize,
    stand: &Stand,
    seen: &Mutex<Vec<Vec<String>>>,
) {
    let mut w = stream.try_clone().expect("clone");
    let (mut answered, mut relayed) = (0, false);
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else { break };
        seen.lock().expect("seen")[n].push(line.clone());
        if line.starts_with("AUTH ") {
            continue;
        }
        if answered == stand.answers {
            break;
        }
        answered += 1;
        let names = aterm_ctl::target_sid(&line).map(|sid| sid.as_str().to_string());
        relayed |= stand.relays.is_some() && names.as_deref() == stand.relays;
        let by = match stand.relays {
            Some(sid) if relayed => format!("{sid}-host"),
            _ => stand.name.to_string(),
        };
        let reply = if line.split_whitespace().any(|w| w == "who") {
            format!("OK 1\n{by}\n")
        } else {
            format!("OK {by}\n")
        };
        if w.write_all(reply.as_bytes()).is_err() {
            break;
        }
    }
}

fn publish(dir: &std::path::Path, sid: &str, sock: &str) {
    let graph = dir.join("graph");
    std::fs::create_dir_all(&graph).expect("graph dir");
    std::fs::write(graph.join(sid), format!("sock {sock}\nnonce 00\npid 1\n"))
        .expect("graph entry");
}

fn stdout(reply: Result<CtlReply, String>) -> String {
    reply.expect("reply").stdout
}

#[test]
fn a_worker_on_another_instance_gets_its_own_connection() {
    let dir = crate::supervise::ScratchDir::new("relayctl", "hosts");
    let endpoint = instance(
        dir.join("aterm-1.sock"),
        Stand {
            name: "endpoint",
            relays: Some("s-ab"),
            answers: usize::MAX,
        },
    );
    let host = instance(
        dir.join("aterm-2.sock"),
        Stand {
            name: "host",
            relays: None,
            answers: usize::MAX,
        },
    );
    std::fs::write(dir.join("aterm-2.token"), "hosttok\n").expect("token");
    publish(&dir, "s-ab", &host.sock);
    let mut c = RelayCtl::new(
        Endpoint::Socket(endpoint.sock.clone()),
        Some("tok".to_string()),
    )
    .in_rendezvous(&dir);
    assert_eq!(stdout(c.call(&["@s-ab", "meta"])), "OK host\n");
    assert_eq!(stdout(c.call(&["meta"])), "OK endpoint\n");
    assert_eq!(stdout(c.call_at_host_of("s-ab", &["who"])), "host\n");
    assert_eq!(stdout(c.call(&["@s-ab", "meta"])), "OK host\n");
    assert_eq!(stdout(c.call(&["who"])), "endpoint\n");
    drop(c);
    assert_eq!(endpoint.finish(), [vec!["AUTH tok", "meta", "who"]]);
    assert_eq!(
        host.finish(),
        [vec!["AUTH hosttok", "@s-ab meta", "who", "@s-ab meta"]]
    );
}

/// A session the endpoint hosts, its graph entry naming the endpoint's socket by another
/// spelling (the canonical one a graph entry is published under, the endpoint given through a
/// symlinked folder as `--control-sock` takes it), rides the endpoint's own connection under
/// the endpoint's token, never a second connection under the token beside the socket.
#[test]
fn a_session_the_endpoint_hosts_under_another_spelling_rides_the_endpoints_token() {
    let dir = crate::supervise::ScratchDir::new("relayctl", "spelling");
    let real = dir.join("real");
    std::fs::create_dir_all(&real).expect("the folder");
    let alias = dir.join("alias");
    std::os::unix::fs::symlink(&real, &alias).expect("a symlinked spelling");
    let endpoint = instance(
        real.join("aterm-1.sock"),
        Stand {
            name: "endpoint",
            relays: None,
            answers: usize::MAX,
        },
    );
    std::fs::write(real.join("aterm-1.token"), "besidetok\n").expect("token");
    publish(&real, "s-ab", &endpoint.sock);
    let given = alias.join("aterm-1.sock").to_string_lossy().into_owned();
    let mut c =
        RelayCtl::new(Endpoint::Socket(given), Some("tok".to_string())).in_rendezvous(&real);
    assert_eq!(stdout(c.call(&["@s-ab", "meta"])), "OK endpoint\n");
    assert_eq!(stdout(c.call(&["meta"])), "OK endpoint\n");
    drop(c);
    assert_eq!(endpoint.finish(), [vec!["AUTH tok", "@s-ab meta", "meta"]]);
}

#[test]
fn a_session_no_host_answers_for_rides_a_connection_of_its_own() {
    let dir = crate::supervise::ScratchDir::new("relayctl", "nohost");
    let endpoint = instance(
        dir.join("aterm-1.sock"),
        Stand {
            name: "endpoint",
            relays: Some("s-cd"),
            answers: usize::MAX,
        },
    );
    let crashed = dir.join("aterm-3.sock");
    drop(UnixListener::bind(&crashed).expect("bind"));
    publish(&dir, "s-cd", &crashed.to_string_lossy());
    publish(&dir, "s-ce", &dir.join("aterm-4.sock").to_string_lossy());
    let mut c = RelayCtl::new(
        Endpoint::Socket(endpoint.sock.clone()),
        Some("tok".to_string()),
    )
    .in_rendezvous(&dir);
    assert_eq!(stdout(c.call(&["@s-cd", "meta"])), "OK s-cd-host\n");
    assert_eq!(stdout(c.call(&["meta"])), "OK endpoint\n");
    assert_eq!(stdout(c.call(&["@s-cd", "meta"])), "OK s-cd-host\n");
    assert_eq!(stdout(c.call_at_host_of("s-cd", &["who"])), "s-cd-host\n");
    assert_eq!(stdout(c.call(&["@s-ce", "meta"])), "OK endpoint\n");
    assert_eq!(stdout(c.call(&["@s-cf", "meta"])), "OK endpoint\n");
    drop(c);
    assert_eq!(
        endpoint.finish(),
        [
            vec!["AUTH tok", "@s-cd meta", "@s-cd meta", "who"],
            vec!["AUTH tok", "meta"],
            vec!["AUTH tok", "@s-ce meta"],
            vec!["AUTH tok", "@s-cf meta"],
        ]
    );
}

#[test]
fn a_host_that_hangs_up_is_found_again_by_its_graph_entry() {
    let dir = crate::supervise::ScratchDir::new("relayctl", "successor");
    let stand = |name, answers| Stand {
        name,
        relays: None,
        answers,
    };
    let endpoint = instance(dir.join("aterm-1.sock"), stand("endpoint", usize::MAX));
    let old = instance(dir.join("aterm-2.sock"), stand("old", 1));
    let new = instance(dir.join("aterm-5.sock"), stand("new", usize::MAX));
    std::fs::write(dir.join("aterm-2.token"), "oldtok\n").expect("token");
    std::fs::write(dir.join("aterm-5.token"), "newtok\n").expect("token");
    publish(&dir, "s-ab", &old.sock);
    let mut c = RelayCtl::new(Endpoint::Socket(endpoint.sock.clone()), None).in_rendezvous(&dir);
    assert_eq!(stdout(c.call(&["@s-ab", "meta"])), "OK old\n");
    publish(&dir, "s-ab", &new.sock);
    let dropped = c.call(&["@s-ab", "meta"]).expect("reply");
    assert!(dropped.lost(), "{dropped:?}");
    assert_eq!(stdout(c.call(&["@s-ab", "meta"])), "OK new\n");
    drop(c);
    assert_eq!(endpoint.finish(), Vec::<Vec<String>>::new());
    assert_eq!(
        old.finish(),
        [vec!["AUTH oldtok", "@s-ab meta", "@s-ab meta"]]
    );
    assert_eq!(new.finish(), [vec!["AUTH newtok", "@s-ab meta"]]);
}

#[test]
fn the_interrupter_hangs_up_every_connection() {
    let dir = crate::supervise::ScratchDir::new("relayctl", "cut-all");
    let stand = |name| Stand {
        name,
        relays: None,
        answers: usize::MAX,
    };
    let endpoint = instance(dir.join("aterm-1.sock"), stand("endpoint"));
    let host = instance(dir.join("aterm-2.sock"), stand("host"));
    std::fs::write(dir.join("aterm-2.token"), "hosttok\n").expect("token");
    publish(&dir, "s-ab", &host.sock);
    let mut c = RelayCtl::new(
        Endpoint::Socket(endpoint.sock.clone()),
        Some("tok".to_string()),
    )
    .in_rendezvous(&dir);
    assert_eq!(stdout(c.call(&["meta"])), "OK endpoint\n");
    assert_eq!(stdout(c.call(&["@s-ab", "meta"])), "OK host\n");
    let cut = c.interrupter().expect("interrupter");
    cut();
    for (name, at) in [("endpoint", &endpoint), ("host", &host)] {
        at.ended
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .unwrap_or_else(|e| panic!("the {name} read no end of the cut connection: {e}"));
    }
    assert!(c.call(&["meta"]).is_err(), "after the cut");
    assert!(c.call(&["@s-ab", "meta"]).is_err(), "after the cut");
    drop(c);
    assert_eq!(endpoint.finish(), [vec!["AUTH tok", "meta"]]);
    assert_eq!(host.finish(), [vec!["AUTH hosttok", "@s-ab meta"]]);
}

#[test]
fn the_reply_wait_follows_an_awaits_own_timeout() {
    assert_eq!(
        reply_wait(&["await", "seq", "4", "timeout", "20000"]),
        Duration::from_millis(20_000) + AWAIT_MARGIN
    );
    assert_eq!(
        reply_wait(&["await", "inbox", "timeout=300"]),
        Duration::from_millis(300) + AWAIT_MARGIN
    );
    assert_eq!(reply_wait(&["meta"]), DEFAULT_REPLY_WAIT);
}
