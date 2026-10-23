// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The supervisor's persistent transport: [`RelayCtl`], the [`Ctl`] seam over persistent control connections (a [`RelayClient`] on each).

use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use aterm_types::control_request::Sid;
use aterm_types::control_verbs::{Framing, artifact_reply_requires_ack, framing_of};
use aterm_uds::CtlStream;

use super::run::{Ctl, CtlReply, Interrupter};
use crate::RelayClient;

/// How long a request that names no `timeout` may wait for its answer.
const DEFAULT_REPLY_WAIT: Duration = Duration::from_secs(60);
/// What an `await … timeout <ms>` may take past its own bound before the
/// connection is judged hung.
const AWAIT_MARGIN: Duration = Duration::from_secs(5);

/// Where [`RelayCtl`] sends what no session's host answers: a named socket, or the one `aterm-ctl` would resolve (re-resolved at every dial).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    /// This socket (`--socket`, the host's own).
    Socket(String),
    /// Whatever `aterm_ctl::resolve_sock_for(self_sid)` names at the dial.
    Resolved { self_sid: Option<String> },
}

/// Persistent, authenticated control connections behind the [`Ctl`] seam, one per instance that answers its requests.
pub struct RelayCtl {
    endpoint: Endpoint,
    /// A token given up front (the host's own) for the endpoint's socket;
    /// `None` reads the one beside the socket at every dial, once that dial's
    /// connect succeeded, as every dial to a session's host does.
    token: Option<String>,
    /// The rendezvous dir whose graph entries name each session's host
    /// ([`aterm_uds::control_socket_dir`]); `None` sends every request to the
    /// endpoint.
    rendezvous: Option<PathBuf>,
    /// The socket [`Endpoint::Resolved`] named at its last resolution, which
    /// stands while a connection dialled there lives.
    resolved: Option<String>,
    conns: Vec<Conn>,
    next_id: u64,
    /// A second handle on each live connection, by [`Conn::id`], for the
    /// interrupter.
    shut: Arc<Mutex<Vec<(u64, CtlStream)>>>,
    /// Set by the interrupter: every request from then on fails.
    cut: Arc<AtomicBool>,
    /// The token given up front is one a listener may refuse (the in-GUI
    /// harness's in-memory token, which only the process that minted it
    /// knows): on `ERR auth` the transport drops it for good and reads the
    /// one beside the socket, once ([`Self::preferring`]).
    beside_on_refusal: bool,
}

/// One connection [`RelayCtl`] holds.
struct Conn {
    id: u64,
    /// The socket it was dialled at.
    sock: String,
    carries: Carries,
    client: RelayClient<CtlStream>,
}

/// Which requests ride a [`Conn`].
enum Carries {
    /// Those the instance at its socket answers itself: requests naming no
    /// session, and those naming one of `hosted`, the sessions whose graph
    /// entries named that socket.
    Own { hosted: Vec<String> },
    /// Only requests about this session, which the endpoint was not found to
    /// host and may relay: after it does, the connection is the host's.
    Relayed(String),
}

impl Carries {
    fn hosts(&self, sid: &str) -> bool {
        matches!(self, Carries::Own { hosted } if hosted.iter().any(|h| h == sid))
    }
}

/// Which connection a request rides ([`RelayCtl::conn_for`]).
enum Route<'a> {
    /// It names no session and is aimed at none: the endpoint answers it.
    Endpoint,
    /// It names this session ([`aterm_ctl::target_sid`]).
    Names(Sid<'a>),
    /// It names none, but asks the instance hosting this one
    /// ([`Ctl::call_at_host_of`]).
    HostOf(Sid<'a>),
}

/// Why a dial gave no connection.
enum DialError {
    /// Nothing listens at the socket (`ENOENT`, `ECONNREFUSED`).
    NoHost(String),
    /// Anything else, the request's failure.
    Failed(String),
}

impl DialError {
    fn words(self) -> String {
        match self {
            DialError::NoHost(w) | DialError::Failed(w) => w,
        }
    }
}

impl RelayCtl {
    /// A transport that dials on its first request, reading each session's host from the rendezvous dir.
    pub fn new(endpoint: Endpoint, token: Option<String>) -> Self {
        Self {
            endpoint,
            token,
            rendezvous: aterm_uds::control_socket_dir(),
            resolved: None,
            conns: Vec::new(),
            next_id: 0,
            shut: Arc::default(),
            cut: Arc::default(),
            beside_on_refusal: false,
        }
    }

    /// A transport that presents `token` first and, should the listener answer `ERR auth` (a seamless update's successor rebound the socket, and does not know a token this process held only in memory), the token beside the socket from then on: the request is sent again on that connection, once — the refused one ran nothing (design §2.5).
    pub fn preferring(endpoint: Endpoint, token: String) -> Self {
        Self {
            beside_on_refusal: true,
            ..Self::new(endpoint, Some(token))
        }
    }

    /// [`Self::new`]'s transport reading the session graph in `dir` instead
    /// (the tests' scratch rendezvous dirs).
    #[cfg(all(test, unix))]
    fn in_rendezvous(mut self, dir: &std::path::Path) -> Self {
        self.rendezvous = Some(dir.to_path_buf());
        self
    }

    /// Dial the endpoint now rather than at the first request that rides its connection, so a socket that cannot be reached is told here.
    /// # Errors
    /// The socket could not be resolved, reached, or its token read.
    pub fn connect(&mut self) -> Result<(), String> {
        self.conn_for(Route::Endpoint).map(|_| ())
    }

    /// The endpoint's socket: the named one, or the one resolved for a live
    /// connection, else resolved afresh.
    fn endpoint_sock(&mut self) -> Result<String, String> {
        let self_sid = match &self.endpoint {
            Endpoint::Socket(s) => return Ok(s.clone()),
            Endpoint::Resolved { self_sid } => self_sid,
        };
        if let Some(s) = &self.resolved
            && self.conns.iter().any(|c| &c.sock == s)
        {
            return Ok(s.clone());
        }
        let s = aterm_ctl::resolve_sock_for(self_sid.as_deref())
            .map_err(|e| format!("cannot resolve the control socket: {e}"))?;
        self.resolved = Some(s.clone());
        Ok(s)
    }

    /// Whether `sock` is the endpoint's, the one socket the given token is for.
    fn is_endpoint(&self, sock: &str) -> bool {
        match &self.endpoint {
            Endpoint::Socket(s) => s == sock,
            Endpoint::Resolved { .. } => self.resolved.as_deref() == Some(sock),
        }
    }

    /// A connection to `sock`, authenticated: with the given token on the
    /// endpoint's socket, else with the token beside `sock`, read once this
    /// dial's connect succeeded, so it is the listener's that took it — never
    /// one read for a socket an update's successor has since rebound.
    fn dial(&self, sock: &str) -> Result<RelayClient<CtlStream>, DialError> {
        let connect_failed = |e: &io::Error| {
            let words = format!("connect {sock}: {e}");
            match e.kind() {
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => {
                    DialError::NoHost(words)
                }
                _ => DialError::Failed(words),
            }
        };
        let given = self.token.as_deref().filter(|_| self.is_endpoint(sock));
        let (stream, token) = match given {
            Some(t) => (
                CtlStream::connect(sock).map_err(|e| connect_failed(&e))?,
                t.trim().to_string(),
            ),
            None => {
                let (stream, token) =
                    aterm_ctl::connect_then_read_token(sock).map_err(|e| connect_failed(&e))?;
                let token =
                    token.map_err(|e| DialError::Failed(format!("connect {sock}: token: {e}")))?;
                (stream, token)
            }
        };
        RelayClient::authenticated(stream, &token)
            .map_err(|e| DialError::Failed(format!("connect {sock}: {e}")))
    }

    /// Hold `client`, dialled at `sock`, for the requests `carries` names.
    fn hold(&mut self, sock: String, carries: Carries, client: RelayClient<CtlStream>) -> usize {
        let id = self.next_id;
        self.next_id += 1;
        if let Ok(handle) = client.transport().try_clone() {
            self.shut
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((id, handle));
        }
        self.conns.push(Conn {
            id,
            sock,
            carries,
            client,
        });
        self.conns.len() - 1
    }

    /// The endpoint's own connection, dialled when there is none.
    fn endpoint_conn(&mut self) -> Result<usize, String> {
        let sock = self.endpoint_sock()?;
        let own = |c: &Conn| c.sock == sock && matches!(c.carries, Carries::Own { .. });
        if let Some(at) = self.conns.iter().position(own) {
            return Ok(at);
        }
        let client = self.dial(&sock).map_err(DialError::words)?;
        Ok(self.hold(sock, Carries::Own { hosted: Vec::new() }, client))
    }

    /// The index of the connection `route` rides, dialled when none does.
    fn conn_for(&mut self, route: Route<'_>) -> Result<usize, String> {
        let (sid, named) = match route {
            Route::Endpoint => return self.endpoint_conn(),
            Route::Names(sid) => (sid, true),
            Route::HostOf(sid) => (sid, false),
        };
        let s = sid.as_str();
        if let Some(at) = self.conns.iter().position(|c| c.carries.hosts(s)) {
            return Ok(at);
        }
        let relayed = |c: &Conn| matches!(&c.carries, Carries::Relayed(r) if r == s);
        if let Some(at) = self.conns.iter().position(relayed) {
            return Ok(at);
        }
        if let Some(host) = self
            .rendezvous
            .as_deref()
            .and_then(|dir| aterm_ctl::hosting_sock(dir, sid))
        {
            let own = |c: &Conn| c.sock == host && matches!(c.carries, Carries::Own { .. });
            if let Some(at) = self.conns.iter().position(own) {
                if let Carries::Own { hosted } = &mut self.conns[at].carries {
                    hosted.push(s.to_string());
                }
                return Ok(at);
            }
            match self.dial(&host) {
                Ok(client) => {
                    let hosted = vec![s.to_string()];
                    return Ok(self.hold(host, Carries::Own { hosted }, client));
                }
                Err(DialError::NoHost(_)) => {}
                Err(DialError::Failed(why)) => return Err(why),
            }
        }
        if !named {
            return self.endpoint_conn();
        }
        let sock = self.endpoint_sock()?;
        let client = self.dial(&sock).map_err(DialError::words)?;
        Ok(self.hold(sock, Carries::Relayed(s.to_string()), client))
    }

    /// Let the connection at `at` go: a request that failed mid-exchange left
    /// its stream at an unknown place, and the routes it carried are decided
    /// again by the next request.
    fn drop_conn(&mut self, at: usize) {
        let conn = self.conns.swap_remove(at);
        self.shut
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(id, _)| *id != conn.id);
    }

    /// One request: on the connection its route names ([`Self::conn_for`]),
    /// rendered as `aterm-ctl` prints the reply.
    fn exchange(&mut self, args: &[&str], host_of: Option<&str>) -> Result<CtlReply, String> {
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
        let route = match (aterm_ctl::target_sid(&line), host_of.and_then(Sid::parse)) {
            (Some(sid), _) => Route::Names(sid),
            (None, Some(sid)) => Route::HostOf(sid),
            (None, None) => Route::Endpoint,
        };
        let wait = reply_wait(args);
        let at = match self.conn_for(route) {
            Ok(at) => at,
            Err(e) => return Ok(client_failure(&e)),
        };
        if self.cut.load(Ordering::SeqCst) {
            return Err("the supervisor was stopped".to_string());
        }
        let given_token = self.token.is_some() && self.is_endpoint(&self.conns[at].sock);
        let conn = &mut self.conns[at].client;
        let _ = conn.transport().set_read_timeout(Some(wait));
        let got = match framing {
            Framing::Lines => conn
                .request_counted(&line)
                .map(|(h, b)| (h, b.into_bytes())),
            Framing::Bytes => conn.request_bytes(&line),
            _ => conn.request_line(&line).map(|h| (h, Vec::new())),
        };
        match got {
            Ok((header, _)) if header == "ERR auth" && self.beside_on_refusal && given_token => {
                self.beside_on_refusal = false;
                self.token = None;
                self.drop_conn(at);
                self.exchange(args, host_of)
            }
            Ok((header, body)) => Ok(render(verb, framing, &header, body)),
            Err(e) if e.kind() == io::ErrorKind::InvalidInput => Ok(client_failure(&e.to_string())),
            Err(e) => {
                self.drop_conn(at);
                if self.cut.load(Ordering::SeqCst) {
                    return Err("the supervisor was stopped".to_string());
                }
                Ok(client_failure(&io_words(&e)))
            }
        }
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

/// A reply rendered as `aterm-ctl` renders it: `header` the status line, `body` the counted rows (`Lines`) or raw bytes (`Bytes`).
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
            let turn = tail.split_whitespace().nth(1) == Some("turn");
            if turn {
                let verdict: Vec<&str> = tail.split_whitespace().skip(1).collect();
                stderr.push_str(&format!("aterm-ctl: {}\n", verdict.join(" ")));
            }
            let empty = tail.split_whitespace().next() == Some("0");
            if verb == "inbox" || verb == "offscreen" || verb == "search" {
                stderr.push_str(&format!("aterm-ctl: {header}\n"));
            } else if empty {
                stderr.push_str(&format!("aterm-ctl: {header} ({verb}: no results)\n"));
            } else if !turn && let Some(note) = aterm_ctl::frame_open_note(tail) {
                // `text`'s `frame=open`, whose header is not shown: the note `aterm-ctl` prints.
                stderr.push_str(&format!("aterm-ctl: {note}\n"));
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
        self.exchange(args, None)
    }

    fn call_at_host_of(&mut self, sid: &str, args: &[&str]) -> Result<CtlReply, String> {
        self.exchange(args, Some(sid))
    }

    fn interrupter(&self) -> Option<Interrupter> {
        let cut = Arc::clone(&self.cut);
        let shut = Arc::clone(&self.shut);
        Some(Box::new(move || {
            cut.store(true, Ordering::SeqCst);
            for (_, s) in shut.lock().unwrap_or_else(PoisonError::into_inner).iter() {
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
    /// request uses it.
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
        // A read of a frame the program had not finished: the rows on stdout, `aterm-ctl`'s
        // note for its `frame=open` on stderr.
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
    /// so) while the transport still holds it: the server reads its end-of-file at the cut.
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

    /// How a stand-in instance answers ([`instance`]).
    struct Stand {
        /// Who answers: each request `OK <name>`, a `who` a roster of the one
        /// row `<name>`.
        name: &'static str,
        /// A session the instance relays, as a server forwards one it does not
        /// host: the rest of a connection that carried a request naming it is
        /// the host's, which answers as `<sid>-host`.
        relays: Option<&'static str>,
        /// How many requests a connection is answered before the instance
        /// hangs it up.
        answers: usize,
    }

    /// A stand-in instance on a socket of a scratch rendezvous dir.
    struct Instance {
        sock: String,
        /// Each connection's lines, its `AUTH` line first, in accept order.
        seen: Arc<Mutex<Vec<Vec<String>>>>,
        /// A connection's number as it closes.
        ended: std::sync::mpsc::Receiver<usize>,
        stop: Arc<AtomicBool>,
        accept: std::thread::JoinHandle<()>,
    }

    impl Instance {
        /// Stop accepting, wait for every connection to close, and give back
        /// the lines each carried.
        fn finish(self) -> Vec<Vec<String>> {
            self.stop.store(true, Ordering::SeqCst);
            drop(std::os::unix::net::UnixStream::connect(&self.sock));
            self.accept.join().expect("accept loop");
            let seen = self.seen.lock().expect("seen");
            seen.clone()
        }
    }

    /// Serve `stand` on `sock`, each connection on its own thread, as a
    /// server parks every connection a client holds.
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
                for stream in listener.incoming() {
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

    /// Publish `sid`'s graph entry in the rendezvous dir `dir`, naming `sock`.
    fn publish(dir: &std::path::Path, sid: &str, sock: &str) {
        let graph = dir.join("graph");
        std::fs::create_dir_all(&graph).expect("graph dir");
        std::fs::write(graph.join(sid), format!("sock {sock}\nnonce 00\npid 1\n"))
            .expect("graph entry");
    }

    fn stdout(reply: Result<CtlReply, String>) -> String {
        reply.expect("reply").stdout
    }

    /// A WORKER ON ANOTHER INSTANCE GETS ITS OWN CONNECTION: a request naming
    /// the session goes to the instance its graph entry names, on that
    /// instance's one connection, authenticated with the token beside its
    /// socket (the given token is the endpoint's alone), and so does a request
    /// aimed at the session's host ([`Ctl::call_at_host_of`]).
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

    /// A SESSION NO HOST ANSWERS FOR RIDES A CONNECTION OF ITS OWN to the
    /// endpoint, which may relay it: a graph entry naming a socket nothing
    /// listens on (a crashed host), one naming a removed socket, and no entry.
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

    /// A HOST THAT HANGS UP IS FOUND AGAIN BY ITS GRAPH ENTRY: the request it
    /// dropped is lost, in `aterm-ctl`'s words, and the next one reads the
    /// entry again and reaches the successor it names now (a self-update's),
    /// with that successor's token.
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
        let mut c =
            RelayCtl::new(Endpoint::Socket(endpoint.sock.clone()), None).in_rendezvous(&dir);
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

    /// THE CUT HANGS UP EVERY CONNECTION the transport holds, the endpoint's
    /// and each host's, while it still holds them, and every request after it
    /// fails, whichever connection it would ride.
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
}
