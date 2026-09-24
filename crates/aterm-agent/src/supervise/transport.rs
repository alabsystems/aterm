// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The supervisor's transports: the [`Ctl`] seam over ONE persistent control
//! connection ([`RelayCtl`], a [`RelayClient`] underneath), and
//! [`Transport`], which is that connection or — where it cannot be opened —
//! the `aterm-ctl`-per-request [`CtlClient`] the loop always had.
//!
//! **Why.** `CtlClient` forks the whole `aterm` binary once per request
//! (measured 4 ms p50 against 0.15 ms p50 on a held socket, audit INT-7), and
//! an in-GUI host (the next wave) has no binary to fork at all. The loop reads
//! a [`CtlReply`] — exit code, stdout, stderr — because that is what
//! `aterm-ctl` gives it, so [`RelayCtl`] renders every reply the way
//! `aterm-ctl` prints it: the same framing table
//! ([`aterm_types::control_verbs::framing_of`]), `ERR …` on stderr behind the
//! client's `aterm-ctl: ` prefix at exit 1, an `OK timeout` (or a `turn`
//! verdict carrying `status=timeout`) at exit 124, a counted reply's rows on
//! stdout with `turn`'s verdict and the `inbox`/`offscreen` header on stderr.
//! So every judgment in the loop — [`CtlReply::lost`], `timed_out`,
//! `unknown_form`, `skipped` — reads the same either way.
//!
//! **A lost connection** is reported in `aterm-ctl`'s own words (`server
//! closed the connection without responding`, `Broken pipe`, `connect
//! <path>: …`), so the loop rides it out exactly as before, and the next
//! request dials again — resolving the socket afresh when none was named, so
//! after a self-update's handoff it follows the `latest` alias to the new
//! instance, as `aterm-ctl` does per run.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use aterm_types::control_verbs::{Framing, artifact_reply_requires_ack, framing_of};
use aterm_uds::CtlStream;

use super::run::{Ctl, CtlReply, Interrupter};
use crate::{CtlClient, RelayClient};

/// How long a request that names no `timeout` may wait for its answer.
const DEFAULT_REPLY_WAIT: Duration = Duration::from_secs(60);
/// What an `await … timeout <ms>` may take past its own bound before the
/// connection is judged hung.
const AWAIT_MARGIN: Duration = Duration::from_secs(5);

/// Where [`RelayCtl`] dials: a named socket, or the one `aterm-ctl` would
/// resolve (re-resolved at every dial).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    /// This socket, always (`--socket`, `$ATERM_CONTROL_SOCK`, the host's own).
    Socket(String),
    /// Whatever `aterm_ctl::resolve_sock_for(self_sid)` names at the dial.
    Resolved { self_sid: Option<String> },
}

/// One persistent, authenticated control connection behind the [`Ctl`] seam.
pub struct RelayCtl {
    endpoint: Endpoint,
    /// A token given up front (`$ATERM_CONTROL_TOKEN`, the host's own);
    /// `None` reads the one beside the socket at every dial.
    token: Option<String>,
    conn: Option<RelayClient<CtlStream>>,
    /// A second handle on the live connection, for the interrupter.
    shut: Arc<Mutex<Option<CtlStream>>>,
    /// Set by the interrupter: every request from then on fails.
    cut: Arc<AtomicBool>,
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
            let token = match &self.token {
                Some(t) => t.clone(),
                None => aterm_ctl::read_token_beside(&sock)
                    .map_err(|e| format!("connect {sock}: token: {e}"))?,
            };
            let client = RelayClient::connect_local(&sock, token.trim())
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
    if it.next() != Some("OK") {
        return false;
    }
    let rest: Vec<&str> = it.collect();
    rest == ["timeout"] || rest.contains(&"status=timeout")
}

/// A reply rendered as `aterm-ctl` renders it: `header` is the status line,
/// `body` the counted rows (`Lines`) or raw bytes (`Bytes`).
fn render(verb: &str, framing: Framing, header: &str, body: &[u8]) -> CtlReply {
    if header != "OK" && !header.starts_with("OK ") {
        return client_failure(header);
    }
    let code = if is_timeout(header) { 124 } else { 0 };
    let tail = header.strip_prefix("OK").unwrap_or("").trim_start();
    let mut stderr = String::new();
    let stdout = match framing {
        Framing::Lines => {
            if tail.split_whitespace().nth(1) == Some("turn") {
                let verdict: Vec<&str> = tail.split_whitespace().skip(1).collect();
                stderr.push_str(&format!("aterm-ctl: {}\n", verdict.join(" ")));
            }
            let empty = tail.split_whitespace().next() == Some("0");
            if verb == "inbox" || verb == "offscreen" {
                stderr.push_str(&format!("aterm-ctl: {header}\n"));
            } else if empty {
                stderr.push_str(&format!("aterm-ctl: {header} ({verb}: no results)\n"));
            }
            String::from_utf8_lossy(body).into_owned()
        }
        Framing::Bytes => {
            if verb == "inbox" {
                stderr.push_str(&format!("aterm-ctl: {header}\n"));
            } else if body.is_empty() {
                stderr.push_str(&format!("aterm-ctl: {header} ({verb}: empty body)\n"));
            }
            String::from_utf8_lossy(body).into_owned()
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
            Ok((header, body)) => Ok(render(verb, framing, &header, &body)),
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
            if let Some(s) = shut.lock().unwrap_or_else(PoisonError::into_inner).as_ref() {
                let _ = s.shutdown(std::net::Shutdown::Both);
            }
        }))
    }
}

/// The supervisor's client: the persistent connection where it could be
/// opened, else one `aterm-ctl` per request.
pub enum Transport {
    Relay(RelayCtl),
    Shell(CtlClient),
}

impl Transport {
    /// [`RelayCtl`] on `endpoint` when it dials now, else `fallback`.
    pub fn open(endpoint: Endpoint, token: Option<String>, fallback: CtlClient) -> Self {
        let mut relay = RelayCtl::new(endpoint, token);
        match relay.connect() {
            Ok(()) => Transport::Relay(relay),
            Err(_) => Transport::Shell(fallback),
        }
    }

    /// Which transport this is, for a diagnostic.
    pub fn name(&self) -> &'static str {
        match self {
            Transport::Relay(_) => "relay",
            Transport::Shell(_) => "aterm-ctl",
        }
    }
}

impl Ctl for Transport {
    fn call(&mut self, args: &[&str]) -> Result<CtlReply, String> {
        match self {
            Transport::Relay(r) => r.call(args),
            Transport::Shell(c) => Ctl::call(c, args),
        }
    }
    fn interrupter(&self) -> Option<Interrupter> {
        match self {
            Transport::Relay(r) => r.interrupter(),
            Transport::Shell(c) => Ctl::interrupter(c),
        }
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
        let dir = std::env::temp_dir().join(format!("aterm-relayctl-{tag}-{}", std::process::id()));
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
                Some("OK title=- attention=-\n"),
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
        let meta = c.call(&["@s-1", "meta"]).expect("meta");
        assert_eq!(meta.stdout, "OK title=- attention=-\n");
        drop(c);
        let seen = h.join().expect("server");
        assert_eq!(seen[0], "AUTH");
        assert_eq!(seen[1], "@s-1 text --json tail=40");
        assert_eq!(seen.len(), 8, "one connection, one AUTH: {seen:?}");
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
        let started = std::time::Instant::now();
        let r = c.call(&["await", "inbox", "since=0", "timeout", "20000"]);
        assert!(r.is_err(), "{r:?}");
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(
            c.call(&["meta"]).is_err(),
            "every request after the cut fails"
        );
        t.join().expect("cutter");
        drop(h.join());
        let _ = std::fs::remove_dir_all(&dir);
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
