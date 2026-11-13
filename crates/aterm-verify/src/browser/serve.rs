// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Loopback HTTP server for browser lane site files, [`Server::answer`] POSTs and the pages'
//! beats ([`Beats`]).

use std::io::{BufRead, BufReader, Read, Take, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::browser::cdp::HANG_DETECTOR;

const MAX_HEAD: usize = 16 << 10;

const MAX_ANSWER: usize = 64 << 20;

/// The target a page's beat POSTs to.
const BEAT: &str = "/beat";

/// When a page last said it is still at work: `lane.mjs` POSTs [`BEAT`] at most once a second
/// from each step and each frame it waits for, so a page that is slow (SwiftShader's readbacks
/// on a busy machine) is told from one that stopped ([`hang_at`]).
#[derive(Clone, Debug, Default)]
pub(crate) struct Beats(Arc<Mutex<Option<Instant>>>);

impl Beats {
    fn beat(&self) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(Instant::now());
    }

    /// The last beat, if any came.
    pub(crate) fn last(&self) -> Option<Instant> {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// When a wait that began at `t0` calls its page hung: [`HANG_DETECTOR`] after `t0`, or after the
/// page's `last` beat where that came later. A minute with no answer and no beat is a hang; a
/// page that keeps beating is waited for however long its work takes.
pub(crate) fn hang_at(t0: Instant, last: Option<Instant>) -> Instant {
    last.map_or(t0, |beat| beat.max(t0)) + HANG_DETECTOR
}

/// The words a wait that `beaten` (a beat came after it began) or not ends with: what it went
/// without for [`HANG_DETECTOR`].
pub(crate) fn hang_words(what: &str, beaten: bool) -> String {
    let since = if beaten {
        " of the page's last beat"
    } else {
        ""
    };
    format!(
        "no {what} within {} s{since} (a hang detector, not a budget)",
        HANG_DETECTOR.as_secs()
    )
}

/// How often a wait looks again for a beat that moved its end.
const BEAT_POLL: Duration = Duration::from_millis(200);

/// One look, at `now`, of a wait for `what` that began at `t0`, its page's last beat `last`: how
/// long to wait for the next message (at most [`BEAT_POLL`], so a later beat is seen), or, once
/// [`hang_at`] has passed, the words the wait ends with ([`hang_words`]).
pub(crate) fn wait_step(
    t0: Instant,
    last: Option<Instant>,
    now: Instant,
    what: &str,
) -> Result<Duration, String> {
    let last = last.filter(|beat| *beat > t0);
    let left = hang_at(t0, last).saturating_duration_since(now);
    if left.is_zero() {
        return Err(hang_words(what, last.is_some()));
    }
    Ok(left.min(BEAT_POLL))
}

/// A running server.
pub(crate) struct Server {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    served: Arc<AtomicUsize>,
    accept: Option<JoinHandle<()>>,
    answers: Receiver<(String, Vec<u8>)>,
    /// Answers that came before the one being waited for, oldest first.
    early: Vec<(String, Vec<u8>)>,
    beats: Beats,
}

impl Server {
    /// Serve `root` on 127.0.0.1 at a port the kernel picks.
    pub(crate) fn start(root: &Path) -> Result<Self, String> {
        Self::serve(root, false)
    }

    /// [`Server::start`] with COOP/COEP cross-origin isolation headers.
    pub(crate) fn start_isolated(root: &Path) -> Result<Self, String> {
        Self::serve(root, true)
    }

    fn serve(root: &Path, isolated: bool) -> Result<Self, String> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .map_err(|e| format!("could not listen on loopback: {e}"))?;
        let addr = listener
            .local_addr()
            .map_err(|e| format!("could not read the listening address: {e}"))?;
        let stop = Arc::new(AtomicBool::new(false));
        let served = Arc::new(AtomicUsize::new(0));
        let (posted, answers) = channel();
        let beats = Beats::default();
        let (root, stop2, served2, beats2) = (
            root.to_path_buf(),
            stop.clone(),
            served.clone(),
            beats.clone(),
        );
        let accept = std::thread::spawn(move || {
            loop {
                if stop2.load(Ordering::SeqCst) {
                    return;
                }
                let Ok((stream, _)) = aterm_uds::accept(
                    &listener,
                    aterm_uds::AcceptWait::Forever,
                    aterm_uds::AcceptedMode::Blocking,
                ) else {
                    continue;
                };
                let (root, served, posted, beats) = (
                    root.clone(),
                    served2.clone(),
                    posted.clone(),
                    beats2.clone(),
                );
                std::thread::spawn(move || {
                    if answer(stream, &root, &posted, &beats, isolated).is_ok_and(|counted| counted)
                    {
                        served.fetch_add(1, Ordering::SeqCst);
                    }
                });
            }
        });
        Ok(Self {
            addr,
            stop,
            served,
            accept: Some(accept),
            answers,
            early: Vec::new(),
            beats,
        })
    }

    /// The pages' beats, for a wait over another channel ([`crate::browser::cdp::Cdp`]).
    pub(crate) fn beats(&self) -> &Beats {
        &self.beats
    }

    /// The body a page POSTed to `/answer/<kind>`, waited for until it comes or a minute passes
    /// with no answer and no beat ([`hang_at`]).
    pub(crate) fn answer(
        &mut self,
        kind: &str,
        mut alive: impl FnMut() -> bool,
    ) -> Result<Vec<u8>, String> {
        if let Some(i) = self.early.iter().position(|(k, _)| k == kind) {
            return Ok(self.early.remove(i).1);
        }
        let t0 = Instant::now();
        let what = format!("answer from the {kind} page");
        loop {
            let wait = wait_step(t0, self.beats.last(), Instant::now(), &what)?;
            match self.answers.recv_timeout(wait) {
                Ok((k, body)) if k == kind => return Ok(body),
                Ok(other) => self.early.push(other),
                Err(RecvTimeoutError::Timeout) if !alive() => {
                    return Err(format!("the browser ended before the {kind} page answered"));
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(format!(
                        "the server stopped before the {kind} page answered"
                    ));
                }
            }
        }
    }

    /// `http://127.0.0.1:<port>`.
    pub(crate) fn origin(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Requests answered so far, the pages' beats not counted.
    pub(crate) fn served(&self) -> usize {
        self.served.load(Ordering::SeqCst)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_secs(1));
        if let Some(t) = self.accept.take() {
            let _ = t.join();
        }
    }
}

/// The content type of a file the site serves.
pub(crate) fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("wasm") => "application/wasm",
        Some("json") => "application/json",
        Some("ttf") => "font/ttf",
        _ => "application/octet-stream",
    }
}

/// The file under `root` a request target names, each `/`-separated segment percent-decoded;
/// `None` for anything not a plain relative path once decoded.
pub(crate) fn resolve(root: &Path, target: &str) -> Option<PathBuf> {
    let path = target.split(['?', '#']).next()?;
    let rel = path.strip_prefix('/')?;
    let mut out = root.to_path_buf();
    for raw in rel.split('/') {
        let segment = percent_decoded(raw)?;
        if segment.contains(['/', '\\', '\0']) {
            return None;
        }
        let mut parts = Path::new(&segment).components();
        match (parts.next(), parts.next()) {
            (Some(Component::Normal(_)), None) => out.push(&segment),
            _ => return None,
        }
    }
    Some(out)
}

fn percent_decoded(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The kind a POST target names.
pub(crate) fn answer_kind(target: &str) -> Option<&str> {
    target
        .strip_prefix("/answer/")
        .filter(|k| !k.is_empty() && k.bytes().all(|b| b.is_ascii_lowercase()))
}

fn content_length(head: &str) -> Option<usize> {
    head.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("content-length")
            .then(|| {
                let v = value.trim();
                v.bytes()
                    .all(|b| b.is_ascii_digit())
                    .then(|| v.parse().ok())
                    .flatten()
            })
            .flatten()
    })
}

fn request_head<R: Read>(reader: &mut BufReader<Take<R>>) -> std::io::Result<String> {
    let mut head = String::new();
    loop {
        let start = head.len();
        let rem = MAX_HEAD.saturating_sub(start).saturating_add(1) as u64;
        let n = (&mut *reader).take(rem).read_line(&mut head)?;
        if head.len() > MAX_HEAD {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "request head too large",
            ));
        }
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "incomplete request head",
            ));
        }
        if matches!(&head[start..], "\r\n" | "\n") {
            return Ok(head);
        }
    }
}

fn empty_response(out: &mut TcpStream, status: &str) -> std::io::Result<()> {
    write!(
        out,
        "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )
}

fn request_body<R: Read>(
    head: &str,
    reader: &mut BufReader<Take<R>>,
) -> Result<Vec<u8>, &'static str> {
    let Some(len) = content_length(head) else {
        return Err("411 Length Required");
    };
    if len > MAX_ANSWER {
        return Err("413 Content Too Large");
    }
    let buffered = reader.buffer().len();
    reader
        .get_mut()
        .set_limit(len.saturating_sub(buffered) as u64);
    let mut body = vec![0; len];
    reader
        .read_exact(&mut body)
        .map_err(|_| "400 Bad Request")?;
    Ok(body)
}

const ISOLATED: &str = "Cross-Origin-Opener-Policy: same-origin\r\n\
                        Cross-Origin-Embedder-Policy: require-corp\r\n";

/// Answer one request: a site file, a posted answer, or a beat. Whether it counts as served (a
/// beat does not).
fn answer(
    mut stream: TcpStream,
    root: &Path,
    posted: &Sender<(String, Vec<u8>)>,
    beats: &Beats,
    isolated: bool,
) -> std::io::Result<bool> {
    stream.set_read_timeout(Some(HANG_DETECTOR))?;
    stream.set_write_timeout(Some(HANG_DETECTOR))?;
    let mut reader = BufReader::new((&stream).take(MAX_HEAD as u64 + 1));
    let head = match request_head(&mut reader) {
        Ok(head) => head,
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::InvalidData | std::io::ErrorKind::UnexpectedEof
            ) =>
        {
            return empty_response(&mut stream, "400 Bad Request").map(|()| true);
        }
        Err(e) => return Err(e),
    };
    let mut words = head.lines().next().unwrap_or_default().split_whitespace();
    let (method, target) = (words.next().unwrap_or(""), words.next().unwrap_or(""));
    if let Some(kind) = answer_kind(target).filter(|_| method == "POST") {
        let status = match request_body(&head, &mut reader) {
            Ok(body) => {
                let _ = posted.send((kind.to_string(), body));
                "204 No Content"
            }
            Err(status) => status,
        };
        drop(reader);
        empty_response(&mut stream, status)?;
        stream.flush()?;
        return Ok(true);
    }
    if method == "POST" && target == BEAT {
        beats.beat();
        drop(reader);
        empty_response(&mut stream, "204 No Content")?;
        stream.flush()?;
        return Ok(false);
    }
    drop(reader);
    let file = (method == "GET")
        .then(|| resolve(root, target))
        .flatten()
        .filter(|p| p.is_file());
    let mut out = stream;
    match file.and_then(|p| crate::read_regular(&p).ok().map(|b| (p, b))) {
        Some((path, body)) => {
            write!(
                out,
                "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\n\
                 Cache-Control: no-store\r\n{}Connection: close\r\n\r\n",
                content_type(&path),
                body.len(),
                if isolated { ISOLATED } else { "" }
            )?;
            out.write_all(&body)?;
        }
        None => {
            let status = if method == "GET" {
                "404 Not Found"
            } else {
                "405 Method Not Allowed"
            };
            empty_response(&mut out, status)?;
        }
    }
    out.flush()?;
    Ok(true)
}

#[cfg(test)]
#[path = "serve_tests.rs"]
mod tests;
