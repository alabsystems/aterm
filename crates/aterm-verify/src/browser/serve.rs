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
            for stream in listener.incoming() {
                if stop2.load(Ordering::SeqCst) {
                    return;
                }
                let Ok(stream) = stream else { continue };
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
            .then(|| value.trim().parse().ok())
            .flatten()
    })
}

fn request_head<R: Read>(reader: &mut BufReader<Take<R>>) -> std::io::Result<String> {
    let mut head = String::new();
    loop {
        let start = head.len();
        let n = reader.read_line(&mut head)?;
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
    match file.and_then(|p| std::fs::read(&p).ok().map(|b| (p, b))) {
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
mod tests {
    use super::*;

    #[test]
    fn request_heads_are_bounded_before_their_newline() {
        let prefix = "GET /lane.mjs HTTP/1.1\r\nX: ";
        let head = format!(
            "{prefix}{}\r\n\r\n",
            "x".repeat(MAX_HEAD - prefix.len() - 4)
        );
        assert_eq!(head.len(), MAX_HEAD);
        let bounded = |input| BufReader::new(Read::take(input, MAX_HEAD as u64 + 1));
        assert_eq!(request_head(&mut bounded(head.as_bytes())).unwrap(), head);
        let oversized = "x".repeat(MAX_HEAD * 2);
        let mut input = std::io::Cursor::new(oversized);
        assert_eq!(
            request_head(&mut BufReader::new((&mut input).take(MAX_HEAD as u64 + 1)))
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidData
        );
        assert_eq!(input.position(), MAX_HEAD as u64 + 1);
    }

    #[test]
    fn a_post_body_is_its_content_length_and_no_more() {
        let body = "x".repeat(20_000);
        let request = format!(
            "POST /answer/gpu HTTP/1.1\r\ncontent-length: {}\r\n\r\n{body}trailing",
            body.len()
        );
        let mut reader = BufReader::new(Read::take(request.as_bytes(), MAX_HEAD as u64 + 1));
        let head = request_head(&mut reader).unwrap();
        assert_eq!(content_length(&head), Some(body.len()));
        assert_eq!(request_body(&head, &mut reader), Ok(body.into_bytes()));
        let mut rest = String::new();
        reader.get_mut().set_limit(u64::MAX);
        reader.read_to_string(&mut rest).unwrap();
        assert_eq!(rest, "trailing");
        let refused = |head: &str| {
            let mut reader = BufReader::new(Read::take(&b""[..], 0));
            request_body(head, &mut reader)
        };
        assert_eq!(
            refused("POST /answer/gpu HTTP/1.1\r\n\r\n"),
            Err("411 Length Required")
        );
        assert_eq!(
            refused(&format!(
                "POST / HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
                MAX_ANSWER + 1
            )),
            Err("413 Content Too Large")
        );
        assert_eq!(
            refused("POST / HTTP/1.1\r\nContent-Length: 4\r\n\r\n"),
            Err("400 Bad Request")
        );
        assert_eq!(answer_kind("/answer/worker"), Some("worker"));
        for bad in [
            "/answer/",
            "/answer/../x",
            "/answer/GPU",
            "/answers/gpu",
            "/cpu.html",
        ] {
            assert_eq!(answer_kind(bad), None, "{bad}");
        }
    }

    #[test]
    fn only_plain_paths_under_the_root_resolve() {
        let root = Path::new("/site");
        assert_eq!(
            resolve(root, "/gpu/aterm_gpu_web.js?v=1#x"),
            Some(PathBuf::from("/site/gpu/aterm_gpu_web.js"))
        );
        assert_eq!(
            resolve(root, "/q/quote%27%60%24%7Bx%7D%23%3F%25_bg.wasm?copy=1"),
            Some(PathBuf::from("/site/q/quote'`${x}#?%_bg.wasm"))
        );
        for bad in [
            "/",
            "",
            "gpu.html",
            "/../etc/passwd",
            "/a/../../b",
            "/%2e%2e/x",
            "/%2E/x",
            "/a%2Fb",
            "/a%5Cb",
            "/a%00b",
            "/a%zzb",
            "/a%2",
            "/a%ff",
            "/a//b",
            "/a\\b",
            "/./a",
        ] {
            assert_eq!(resolve(root, bad), None, "{bad}");
        }
    }

    #[test]
    fn modules_and_wasm_get_the_types_a_page_needs() {
        assert_eq!(
            content_type(Path::new("a.mjs")),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(
            content_type(Path::new("a.js")),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(content_type(Path::new("a_bg.wasm")), "application/wasm");
        assert_eq!(
            content_type(Path::new("cpu.html")),
            "text/html; charset=utf-8"
        );
        assert_eq!(content_type(Path::new("x")), "application/octet-stream");
    }

    fn request(origin: &str, head: &str) -> String {
        let addr = origin.trim_start_matches("http://");
        let mut s = TcpStream::connect(addr).expect("connect");
        s.set_read_timeout(Some(crate::browser::cdp::HANG_DETECTOR))
            .expect("timeout");
        s.set_write_timeout(Some(crate::browser::cdp::HANG_DETECTOR))
            .expect("write timeout");
        s.write_all(head.as_bytes()).expect("send");
        s.shutdown(std::net::Shutdown::Write)
            .expect("finish request");
        let mut reader = BufReader::new(s);
        let mut out = String::new();
        let mut length = None;
        loop {
            let mut line = String::new();
            assert_ne!(reader.read_line(&mut line).expect("response header"), 0);
            if let Some(value) = line.strip_prefix("Content-Length: ") {
                length = Some(value.trim().parse::<usize>().expect("content length"));
            }
            out.push_str(&line);
            if line == "\r\n" {
                break;
            }
        }
        let mut body = vec![0; length.expect("counted response")];
        reader.read_exact(&mut body).expect("response body");
        out.push_str(std::str::from_utf8(&body).expect("text fixture"));
        out
    }

    fn get(origin: &str, target: &str) -> String {
        request(
            origin,
            &format!("GET {target} HTTP/1.1\r\nHost: localhost\r\n\r\n"),
        )
    }

    #[test]
    fn the_server_answers_files_and_refuses_the_rest() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("atv-serve-")
            .tempdir()
            .expect("scratch");
        std::fs::write(dir.path().join("lane.mjs"), "export {}\n").expect("write");
        let server = Server::start(dir.path()).expect("start");
        let ok = get(&server.origin(), "/lane.mjs");
        assert!(ok.starts_with("HTTP/1.1 200 OK\r\n"), "{ok}");
        assert!(
            ok.contains("Content-Type: text/javascript; charset=utf-8\r\n"),
            "{ok}"
        );
        assert!(ok.ends_with("\r\n\r\nexport {}\n"), "{ok}");
        assert!(
            !ok.contains("Cross-Origin-"),
            "the lane's pages stay as they were: {ok}"
        );
        let isolated = Server::start_isolated(dir.path()).expect("start");
        let iso = get(&isolated.origin(), "/lane.mjs");
        assert!(
            iso.contains(ISOLATED) && iso.ends_with("\r\n\r\nexport {}\n"),
            "{iso}"
        );
        assert!(get(&server.origin(), "/missing.js").starts_with("HTTP/1.1 404"));
        assert!(get(&server.origin(), "/../lane.mjs").starts_with("HTTP/1.1 404"));
        let oversized =
            |prefix: &str| format!("{prefix}{}", "x".repeat(MAX_HEAD + 1 - prefix.len()));
        for invalid in [
            oversized("GET /lane.mjs "),
            oversized("GET /lane.mjs HTTP/1.1\r\nX: "),
            "GET /lane.mjs HTTP/1.1\r\nHost: localhost\r\n".to_string(),
        ] {
            let refused = request(&server.origin(), &invalid);
            assert!(
                refused.starts_with("HTTP/1.1 400"),
                "bad request admitted: {refused}"
            );
        }
        assert!(get(&server.origin(), "/lane.mjs").ends_with("\r\n\r\nexport {}\n"));
        let post = |kind: &str, body: &str| {
            request(
                &server.origin(),
                &format!(
                    "POST /answer/{kind} HTTP/1.1\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                ),
            )
        };
        assert!(post("gpu", "{\"g\":1}").starts_with("HTTP/1.1 204"));
        assert!(post("cpu", "{\"c\":1}").starts_with("HTTP/1.1 204"));
        assert!(post("Cpu", "{}").starts_with("HTTP/1.1 405"));
        let mut server = server;
        assert_eq!(
            server.answer("cpu", || true).as_deref(),
            Ok(&b"{\"c\":1}"[..])
        );
        assert_eq!(
            server.answer("gpu", || true).as_deref(),
            Ok(&b"{\"g\":1}"[..])
        );
        assert_eq!(
            server.answer("worker", || false),
            Err("the browser ended before the worker page answered".to_string())
        );
        assert!(server.served() >= 1);
        drop(server);
    }

    /// A WAIT IS HUNG A MINUTE AFTER ITS START OR ITS PAGE'S LAST BEAT, WHICHEVER IS LATER: a beat
    /// after the start moves the end out; one from before it (an earlier page's) does not, and
    /// the words say which minute ran out.
    #[test]
    fn a_wait_ends_a_minute_after_its_start_or_its_last_beat() {
        let t0 = Instant::now();
        let at = |secs: u64| t0 + Duration::from_secs(secs);
        let what = "answer from the gpu page";
        assert_eq!(hang_at(t0, None), at(60));
        assert_eq!(hang_at(t0, Some(at(45))), at(105));

        assert_eq!(wait_step(t0, None, t0, what), Ok(BEAT_POLL));
        assert_eq!(
            wait_step(t0, None, at(60) - Duration::from_millis(50), what),
            Ok(Duration::from_millis(50)),
            "the last look waits only what is left"
        );
        let unbeaten = Err(hang_words(what, false));
        assert_eq!(wait_step(t0, None, at(60), what), unbeaten);

        let beat = Some(at(45));
        assert_eq!(wait_step(t0, beat, at(100), what), Ok(BEAT_POLL));
        let beaten = wait_step(t0, beat, at(105), what);
        assert_eq!(beaten, Err(hang_words(what, true)));
        assert_ne!(beaten, unbeaten, "the words say a beat came");

        let before = t0
            .checked_sub(Duration::from_secs(5))
            .expect("a clock five seconds old");
        assert_eq!(
            wait_step(t0, Some(before), at(60), what),
            unbeaten,
            "NEGATIVE CONTROL: an earlier page's beat moves nothing"
        );
    }

    /// A PAGE'S BEAT IS HEARD AND NOT SERVED: `POST /beat` answers 204 and moves the beat clock,
    /// and is not counted among the requests served; a GET of it is no file, and a POST anywhere
    /// else moves nothing.
    #[test]
    fn a_beat_moves_the_clock_and_is_not_a_request_served() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("atv-beat-")
            .tempdir()
            .expect("scratch");
        let server = Server::start(dir.path()).expect("start");
        assert_eq!(server.beats().last(), None, "CONTROL: no beat yet");
        let post = |target: &str| {
            request(
                &server.origin(),
                &format!("POST {target} HTTP/1.1\r\nContent-Length: 0\r\n\r\n"),
            )
        };
        let before = Instant::now();
        assert!(post(BEAT).starts_with("HTTP/1.1 204"));
        let first = server.beats().last().expect("the beat");
        assert!(first >= before);
        assert!(post(BEAT).starts_with("HTTP/1.1 204"));
        assert!(server.beats().last().expect("the beat") > first);
        assert_eq!(server.served(), 0, "a beat is not a request served");
        let last = server.beats().last();
        assert!(
            post("/beats").starts_with("HTTP/1.1 405"),
            "NEGATIVE CONTROL"
        );
        assert!(get(&server.origin(), BEAT).starts_with("HTTP/1.1 404"));
        assert_eq!(server.beats().last(), last, "another request moves nothing");
    }
}
