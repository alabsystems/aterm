// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The supervisor's persistent transport: [`RelayCtl`], the [`Ctl`] seam over one persistent
//! control connection ([`RelayClient`] underneath). The other [`Ctl`] is the
//! `aterm-ctl`-per-request [`crate::CtlClient`].
//!
//! A held socket avoids forking `aterm` per request, and an in-GUI host has no binary to fork.
//! [`RelayCtl`] renders every reply as `aterm-ctl` prints it
//! ([`aterm_types::control_verbs::framing_of`], `ERR …` on stderr at exit 1, timeouts at exit
//! 124, counted rows on stdout), so every judgment in the loop ([`CtlReply::lost`], `timed_out`,
//! `unknown_form`, `skipped`) reads the same either way. A lost connection is reported in
//! `aterm-ctl`'s words, and the next request dials again, re-resolving the socket when none was
//! named (following the `latest` alias after a self-update's handoff).

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use aterm_types::control_verbs::{Framing, artifact_reply_requires_ack, framing_of};
use aterm_uds::CtlStream;

use super::run::{Ctl, CtlReply, Interrupter};
use crate::RelayClient;

/// How long a request that names no `timeout` may wait for its answer.
const DEFAULT_REPLY_WAIT: Duration = Duration::from_secs(60);
/// What an `await … timeout <ms>` may take past its own bound before the
/// connection is judged hung.
const AWAIT_MARGIN: Duration = Duration::from_secs(5);

/// Where [`RelayCtl`] dials: a named socket, or the one `aterm-ctl` would
/// resolve (re-resolved at every dial).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    /// This socket, always (`--socket`, the host's own).
    Socket(String),
    /// Whatever `aterm_ctl::resolve_sock_for(self_sid)` names at the dial.
    Resolved { self_sid: Option<String> },
}

/// One persistent, authenticated control connection behind the [`Ctl`] seam.
pub struct RelayCtl {
    endpoint: Endpoint,
    /// A token given up front (the host's own); `None` reads the one beside the
    /// socket at every dial, once that dial's connect succeeded.
    token: Option<String>,
    conn: Option<RelayClient<CtlStream>>,
    /// A second handle on the live connection, for the interrupter.
    shut: Arc<Mutex<Option<CtlStream>>>,
    /// Set by the interrupter: every request from then on fails.
    cut: Arc<AtomicBool>,
    /// The token given up front is one a listener may refuse (the in-GUI
    /// harness's in-memory token, which only the process that minted it
    /// knows): on `ERR auth` the transport drops it for good and reads the
    /// one beside the socket, once ([`Self::preferring`]).
    beside_on_refusal: bool,
}

impl RelayCtl {
    /// A transport that dials `endpoint` on its first request.
    pub fn new(endpoint: Endpoint, token: Option<String>) -> Self {
        Self {
            endpoint,
            token,
            conn: None,
            shut: Arc::default(),
            cut: Arc::default(),
            beside_on_refusal: false,
        }
    }

    /// A transport that presents `token` first and, should the listener
    /// answer `ERR auth` (a seamless update's successor rebound the socket,
    /// and does not know a token this process held only in memory), the
    /// token beside the socket from then on: the request is sent again on
    /// that connection, once — the refused one ran nothing (design §2.5,
    /// phase 3A ruling R10).
    pub fn preferring(endpoint: Endpoint, token: String) -> Self {
        Self {
            beside_on_refusal: true,
            ..Self::new(endpoint, Some(token))
        }
    }

    /// Dial now, so a caller can fall back when nothing answers.
    ///
    /// # Errors
    /// The socket could not be resolved, reached, or its token read.
    pub fn connect(&mut self) -> Result<(), String> {
        self.dial().map(|_| ())
    }

    fn sock(&self) -> Result<String, String> {
        match &self.endpoint {
            Endpoint::Socket(s) => Ok(s.clone()),
            Endpoint::Resolved { self_sid } => aterm_ctl::resolve_sock_for(self_sid.as_deref())
                .map_err(|e| format!("cannot resolve the control socket: {e}")),
        }
    }

    /// The live connection, dialled when there is none.
    fn dial(&mut self) -> Result<&mut RelayClient<CtlStream>, String> {
        if self.conn.is_none() {
            let sock = self.sock()?;
            // The token beside the socket is read after this dial's connect succeeded, so it is
            // the listener's that took it, never one for a socket a successor has since rebound.
            let client = match &self.token {
                Some(t) => RelayClient::connect_local(&sock, t.trim()),
                None => RelayClient::connect_local_beside(&sock),
            }
            .map_err(|e| format!("connect {sock}: {e}"))?;
            *self.shut.lock().unwrap_or_else(PoisonError::into_inner) =
                client.transport().try_clone().ok();
            self.conn = Some(client);
        }
        self.conn
            .as_mut()
            .ok_or_else(|| "no control connection".to_string())
    }

    fn drop_conn(&mut self) {
        self.conn = None;
        *self.shut.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}

/// A failure the client itself reports, as `aterm-ctl` prints it.
fn client_failure(text: &str) -> CtlReply {
    CtlReply {
        code: 1,
        stdout: String::new(),
        stderr: format!("aterm-ctl: {text}\n"),
    }
}

/// An I/O error on the connection, in the words [`CtlReply::lost`] knows.
fn io_words(e: &io::Error) -> String {
    match e.kind() {
        io::ErrorKind::UnexpectedEof => {
            "server closed the connection without responding".to_string()
        }
        io::ErrorKind::BrokenPipe => "Broken pipe".to_string(),
        io::ErrorKind::ConnectionReset => "Connection reset by peer".to_string(),
        io::ErrorKind::ConnectionRefused => "Connection refused".to_string(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => {
            "timed out waiting for the server".to_string()
        }
        _ => e.to_string(),
    }
}

/// How long the answer to `args` may take: an `await`'s own `timeout <ms>`
/// (or `timeout=<ms>`) plus a margin, else the default.
fn reply_wait(args: &[&str]) -> Duration {
    let ms = args.iter().enumerate().find_map(|(i, a)| {
        if *a == "timeout" {
            args.get(i + 1).and_then(|n| n.parse::<u64>().ok())
        } else {
            a.strip_prefix("timeout=").and_then(|n| n.parse().ok())
        }
    });
    match ms {
        Some(ms) => Duration::from_millis(ms) + AWAIT_MARGIN,
        None => DEFAULT_REPLY_WAIT,
    }
}

/// Whether a status line is a server-reported TIMEOUT (`aterm-ctl`'s exit
/// 124): a bare `OK timeout`, or a verdict carrying `status=timeout`.
fn is_timeout(status: &str) -> bool {
    let mut it = status.split_whitespace();
    it.next() == Some("OK") && (it.clone().eq(["timeout"]) || it.any(|w| w == "status=timeout"))
}

/// The body as text: valid UTF-8 (every counted reply) is taken as it is,
/// anything else read lossily, as `aterm-ctl` prints it.
fn lossy(body: Vec<u8>) -> String {
    String::from_utf8(body).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

/// A reply rendered as `aterm-ctl` renders it: `header` the status line, `body` the counted rows
/// (`Lines`) or raw bytes (`Bytes`). The supervisor's tests build their scripted replies through
/// it, so a reply shape no transport produces cannot pass for one.
pub(super) fn render(
    verb: &str,
    framing: Framing,
    header: &str,
    body: impl Into<Vec<u8>>,
) -> CtlReply {
    if !crate::is_ok_reply(header) {
        return client_failure(header);
    }
    let body = body.into();
    let code = if is_timeout(header) { 124 } else { 0 };
    let tail = header.strip_prefix("OK").unwrap_or("").trim_start();
    let mut stderr = String::new();
    let stdout = match framing {
        Framing::Lines => {
            // `search`'s ` incomplete` marker: the note `aterm-ctl` prints.
            if verb == "search"
                && let Some(note) = aterm_ctl::search_incomplete_note(tail)
            {
                stderr.push_str(&format!("aterm-ctl: {note}\n"));
            }
            if tail.split_whitespace().nth(1) == Some("turn") {
                let verdict: Vec<&str> = tail.split_whitespace().skip(1).collect();
                stderr.push_str(&format!("aterm-ctl: {}\n", verdict.join(" ")));
            }
            let empty = tail.split_whitespace().next() == Some("0");
            if verb == "inbox" || verb == "offscreen" || verb == "search" {
                stderr.push_str(&format!("aterm-ctl: {header}\n"));
            } else if empty {
                stderr.push_str(&format!("aterm-ctl: {header} ({verb}: no results)\n"));
            }
            lossy(body)
        }
        Framing::Bytes => {
            if verb == "inbox" {
                stderr.push_str(&format!("aterm-ctl: {header}\n"));
            } else if body.is_empty() {
                stderr.push_str(&format!("aterm-ctl: {header} ({verb}: empty body)\n"));
            }
            lossy(body)
        }
        _ => format!("{header}\n"),
    };
    CtlReply {
        code,
        stdout,
        stderr,
    }
}

impl Ctl for RelayCtl {
    fn call(&mut self, args: &[&str]) -> Result<CtlReply, String> {
        if self.cut.load(Ordering::SeqCst) {
            return Err("the supervisor was stopped".to_string());
        }
        let verb = args
            .iter()
            .find(|a| !a.starts_with('@'))
            .copied()
            .unwrap_or("");
        let line = args.join(" ");
        let framing = framing_of(verb, &line);
        if matches!(framing, Framing::Push) || artifact_reply_requires_ack(verb, &line) {
            return Ok(client_failure(&format!(
                "{verb}: not served over the supervisor's persistent connection"
            )));
        }
        let wait = reply_wait(args);
        let conn = match self.dial() {
            Ok(c) => c,
            Err(e) => return Ok(client_failure(&e)),
        };
        let _ = conn.transport().set_read_timeout(Some(wait));
        let got = match framing {
            Framing::Lines => conn
                .request_counted(&line)
                .map(|(h, b)| (h, b.into_bytes())),
            Framing::Bytes => conn.request_bytes(&line),
            _ => conn.request_line(&line).map(|h| (h, Vec::new())),
        };
        match got {
            Ok((header, _)) if header == "ERR auth" && self.beside_on_refusal => {
                // The token given up front was refused: from now on, the
                // one beside the socket. The listener closes a refused
                // connection, and ran nothing on it.
                self.beside_on_refusal = false;
                self.token = None;
                self.drop_conn();
                self.call(args)
            }
            Ok((header, body)) => Ok(render(verb, framing, &header, body)),
            Err(e) if e.kind() == io::ErrorKind::InvalidInput => Ok(client_failure(&e.to_string())),
            Err(e) => {
                // A request that failed mid-exchange leaves the stream at an
                // unknown place: the next request dials afresh.
                self.drop_conn();
                if self.cut.load(Ordering::SeqCst) {
                    return Err("the supervisor was stopped".to_string());
                }
                Ok(client_failure(&io_words(&e)))
            }
        }
    }

    fn interrupter(&self) -> Option<Interrupter> {
        let cut = Arc::clone(&self.cut);
        let shut = Arc::clone(&self.shut);
        Some(Box::new(move || {
            cut.store(true, Ordering::SeqCst);
            // Each half on its own ([`aterm_uds::hangup::hang_up`]): Darwin refuses a both-ways
            // `shutdown` of a socket whose server half-closed toward us, shutting nothing.
            if let Some(s) = shut.lock().unwrap_or_else(PoisonError::into_inner).as_ref() {
                aterm_uds::hangup::hang_up(s);
            }
        }))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;

    /// A server on a scratch socket: it takes the `AUTH` line, then answers
    /// each request line with the next scripted reply (verbatim bytes),
    /// recording the lines it read; `None` closes the connection unanswered.
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
                let Ok((stream, _)) = listener.accept() else {
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

    /// R10 (phase 3A): a token given up front that the listener refuses
    /// (`ERR auth`: an update's successor rebound the socket and never knew
    /// a token held only in memory) is dropped for the one beside the socket,
    /// and the request goes out again on that connection, once; every later
    /// request uses it. NEGATIVE CONTROL: a transport built with `new` keeps
    /// the refusal.
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
                let Ok((stream, _)) = listener.accept() else {
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
        // Closed, so the scratch server takes the next connection.
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

    /// Every reply shape the loop reads comes out as `aterm-ctl` prints it.
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
        // `search`'s ` incomplete` marker: the hits on stdout, `aterm-ctl`'s
        // note for it on stderr, then the header (the report reads both).
        let search = c.call(&["@s-1", "search", "^›", "regex"]).expect("search");
        assert_eq!(
            (search.stdout.as_str(), search.stderr.as_str()),
            (
                "10 0 1\n",
                "aterm-ctl: older history not searched (search_history_lines)\naterm-ctl: OK \
                 1 incomplete older\n"
            )
        );
        drop(c);
        let seen = h.join().expect("server");
        assert_eq!(seen[0], "AUTH");
        assert_eq!(seen[1], "@s-1 text --json tail=40");
        assert_eq!(seen.len(), 10, "one connection, one AUTH: {seen:?}");
    }

    /// A connection that closes unanswered is `lost` in `aterm-ctl`'s words,
    /// and the next request dials again (a second AUTH) and is served.
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

    /// Negative control: a socket nothing listens on is `lost` too (the
    /// `connect <path>: …` words), never a server answer.
    #[test]
    fn an_absent_socket_is_lost() {
        let mut c = RelayCtl::new(
            Endpoint::Socket("/nonexistent/aterm-relayctl.sock".to_string()),
            Some("tok".to_string()),
        );
        let r = c.call(&["meta"]).expect("reply");
        assert!(r.lost() && !r.ok(), "{r:?}");
    }

    /// The interrupter cuts a parked request short and every one after it.
    #[test]
    fn the_interrupter_ends_a_parked_request() {
        let dir = std::env::temp_dir().join(format!("aterm-relayctl-cut-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let sock = dir.join("a.sock");
        let listener = UnixListener::bind(&sock).expect("bind");
        // Accept and never answer.
        let h = std::thread::spawn(move || listener.accept().map(|(s, _)| s));
        let mut c = RelayCtl::new(
            Endpoint::Socket(sock.to_string_lossy().into_owned()),
            Some("tok".to_string()),
        );
        c.connect().expect("dial");
        let cut = c.interrupter().expect("interrupter");
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            cut();
        });
        // The await asks for five minutes; the bound is a one-minute hang detector.
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
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The cut hangs up a connection its server half-closed (a relay passes its child's EOF on
    /// so) while the transport still holds it: the server reads its end-of-file at the cut. A
    /// both-ways `shutdown` fails here on Darwin (`ENOTCONN`) and shuts nothing.
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
        let (server, _) = listener.accept().expect("accept");
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
}
