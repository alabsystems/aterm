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
use crate::{DEFAULT_REPLY_WAIT, RelayClient};

const AWAIT_MARGIN: Duration = Duration::from_secs(5);

/// Whether `a` and `b` name one socket: their folders resolve to one folder, and their names
/// agree.
fn same_socket(a: &str, b: &str) -> bool {
    let canonical = |p: &str| {
        let p = std::path::Path::new(p);
        Some(
            std::fs::canonicalize(p.parent()?)
                .ok()?
                .join(p.file_name()?),
        )
    };
    canonical(a).is_some_and(|a| canonical(b) == Some(a))
}

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
    token: Option<String>,
    rendezvous: Option<PathBuf>,
    resolved: Option<String>,
    conns: Vec<Conn>,
    next_id: u64,
    shut: Arc<Mutex<Vec<(u64, CtlStream)>>>,
    cut: Arc<AtomicBool>,
    beside_on_refusal: bool,
}

struct Conn {
    id: u64,
    sock: String,
    carries: Carries,
    client: RelayClient<CtlStream>,
}

enum Carries {
    Own { hosted: Vec<String> },
    Relayed(String),
}

impl Carries {
    fn hosts(&self, sid: &str) -> bool {
        matches!(self, Carries::Own { hosted } if hosted.iter().any(|h| h == sid))
    }
}

enum Route<'a> {
    Endpoint,
    Names(Sid<'a>),
    HostOf(Sid<'a>),
}

enum DialError {
    NoHost(String),
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

    /// A transport that tries `token` first and falls back to the token beside the socket on `ERR auth`.
    pub fn preferring(endpoint: Endpoint, token: String) -> Self {
        Self {
            beside_on_refusal: true,
            ..Self::new(endpoint, Some(token))
        }
    }

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

    fn is_endpoint(&self, sock: &str) -> bool {
        match &self.endpoint {
            Endpoint::Socket(s) => s == sock,
            Endpoint::Resolved { .. } => self.resolved.as_deref() == Some(sock),
        }
    }

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
            Some(t) => {
                let stream = aterm_uds::kept_out(|| CtlStream::connect(sock))
                    .map_err(|e| connect_failed(&e))?;
                (stream, t.trim().to_string())
            }
            None => {
                let (stream, token) =
                    aterm_ctl::connect_then_read_token(sock).map_err(|e| connect_failed(&e))?;
                let token =
                    token.map_err(|e| DialError::Failed(format!("connect {sock}: token: {e}")))?;
                (stream, token)
            }
        };
        RelayClient::authenticated(stream, &token)
            .map(|client| client.bounded(DEFAULT_REPLY_WAIT))
            .map_err(|e| DialError::Failed(format!("connect {sock}: {e}")))
    }

    fn hold(&mut self, sock: String, carries: Carries, client: RelayClient<CtlStream>) -> usize {
        let id = self.next_id;
        self.next_id += 1;
        if let Ok(handle) = aterm_uds::kept_out(|| client.transport().try_clone()) {
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

    fn endpoint_conn(&mut self) -> Result<usize, String> {
        let sock = self.endpoint_sock()?;
        let own = |c: &Conn| c.sock == sock && matches!(c.carries, Carries::Own { .. });
        if let Some(at) = self.conns.iter().position(own) {
            return Ok(at);
        }
        let client = self.dial(&sock).map_err(DialError::words)?;
        Ok(self.hold(sock, Carries::Own { hosted: Vec::new() }, client))
    }

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
            .map(|host| self.endpoint_spelling(host))
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

    /// `host` in the endpoint's own spelling when the two name one socket (`/tmp/…` and
    /// `/private/tmp/…`: a graph entry is published canonical, a `--control-sock` is kept as
    /// given), so a session the endpoint hosts rides the endpoint's connection, under the token
    /// the endpoint was given; otherwise `host`.
    fn endpoint_spelling(&self, host: String) -> String {
        let own = match &self.endpoint {
            Endpoint::Socket(s) => Some(s.as_str()),
            Endpoint::Resolved { .. } => self.resolved.as_deref(),
        };
        match own {
            Some(own) if own != host && same_socket(own, &host) => own.to_string(),
            _ => host,
        }
    }

    fn drop_conn(&mut self, at: usize) {
        let conn = self.conns.swap_remove(at);
        self.shut
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(id, _)| *id != conn.id);
    }

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
        // reply_wait already includes the server's named wait; bounded adds it once.
        conn.bound_by(wait.saturating_sub(crate::named_timeout(args).unwrap_or_default()));
        let got = match framing {
            Framing::Lines => conn
                .request_counted(&line)
                .map(|(h, b)| (h, b.into_bytes())),
            Framing::Bytes => conn.request_bytes(&line),
            _ => conn.request_line(&line).map(|h| (h, Vec::new())),
        };
        let unread = conn.reply_unread();
        match got {
            Ok((header, _)) if header == "ERR auth" && self.beside_on_refusal && given_token => {
                self.beside_on_refusal = false;
                self.token = None;
                self.drop_conn(at);
                self.exchange(args, host_of)
            }
            Ok((header, body)) => Ok(render(verb, framing, &header, body)),
            Err(e) if e.kind() == io::ErrorKind::InvalidInput && !unread => {
                Ok(client_failure(&e.to_string()))
            }
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

fn client_failure(text: &str) -> CtlReply {
    CtlReply {
        code: 1,
        stdout: String::new(),
        stderr: format!("aterm-ctl: {text}\n"),
    }
}

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

fn reply_wait(args: &[&str]) -> Duration {
    crate::named_timeout(args).map_or(DEFAULT_REPLY_WAIT, |wait| wait + AWAIT_MARGIN)
}

fn is_timeout(status: &str) -> bool {
    let mut it = status.split_whitespace();
    it.next() == Some("OK") && (it.clone().eq(["timeout"]) || it.any(|w| w == "status=timeout"))
}

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
#[path = "transport_tests.rs"]
mod tests;
