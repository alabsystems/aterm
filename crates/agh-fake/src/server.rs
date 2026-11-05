// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Listener pool and single-request HTTP connection handler (`Connection: close`).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::routes::{self, Answer, Outcome, Request};
use crate::state::State;
use crate::{FakeOrigin, Ports};

/// How long a connection may sit unread: a hang detector, never a latency budget.
const READ_TIMEOUT: Duration = Duration::from_secs(60);
/// The longest request head read.
const HEAD_MAX: usize = 64 * 1024;
/// The largest request body read.
const BODY_MAX: u64 = 16 * 1024 * 1024;
/// How long the accept loop parks after a failed accept: a back-off, never a latency budget.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(10);

/// Bind the four listeners, put their ports in `state`, and start each accept thread.
pub(crate) fn serve(
    state: &Arc<Mutex<State>>,
    stop: &Arc<AtomicBool>,
) -> (Ports, Vec<JoinHandle<()>>) {
    let origins = [
        FakeOrigin::Api,
        FakeOrigin::Uploads,
        FakeOrigin::Web,
        FakeOrigin::Cdn,
    ];
    let listeners = origins.map(|_| {
        TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .unwrap_or_else(|e| panic!("agh-fake cannot bind a loopback port: {e}"))
    });
    let [api, uploads, web, cdn] = listeners.each_ref().map(|l| {
        l.local_addr()
            .unwrap_or_else(|e| panic!("agh-fake cannot read its port: {e}"))
            .port()
    });
    let ports = Ports {
        api,
        uploads,
        web,
        cdn,
    };
    state.lock().unwrap_or_else(PoisonError::into_inner).ports = ports;
    let accepts = listeners
        .into_iter()
        .zip(origins)
        .map(|(listener, origin)| {
            let state = state.clone();
            let stop = stop.clone();
            std::thread::spawn(move || accept(&listener, origin, &state, &stop))
        })
        .collect();
    (ports, accepts)
}

/// Connect once to each port, so each accept loop sees the stop flag.
pub(crate) fn wake(ports: Ports) {
    for port in [ports.api, ports.uploads, ports.web, ports.cdn] {
        let _ = TcpStream::connect((Ipv4Addr::LOCALHOST, port));
    }
}

/// One listener's accept loop. A failed accept (`EMFILE` in a crowded test run) would fail
/// again at once, so the loop backs off before the next one rather than spin a core; the
/// back-off is a park that `Drop` cuts short, never a clock read or a latency budget. A
/// connection whose thread cannot be made is dropped, so its client sees a close and the
/// origin keeps serving.
fn accept(
    listener: &TcpListener,
    origin: FakeOrigin,
    state: &Arc<Mutex<State>>,
    stop: &AtomicBool,
) {
    for stream in listener.incoming() {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let Ok(stream) = stream else {
            std::thread::park_timeout(ACCEPT_BACKOFF);
            continue;
        };
        let state = state.clone();
        let _ = std::thread::Builder::new().spawn(move || serve_one(&stream, origin, &state));
    }
}

/// A request's head: the request line's method and target, and the headers.
struct Head {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    /// No request target, a header line without `:`, a head cut short, or one over
    /// `HEAD_MAX`.
    malformed: bool,
}

/// Read one request head as bytes, decoded lossily; `None` when the peer sent no byte (a
/// wake connection). A head the fake cannot parse is read as far as it goes and marked
/// `malformed`, so it is still logged and answered. Every line, the request line included,
/// counts toward `HEAD_MAX`, and a read that fails after some bytes is a head cut short.
fn read_head(reader: &mut BufReader<&TcpStream>) -> Option<Head> {
    let mut raw = Vec::with_capacity(256);
    let first = u64::try_from(HEAD_MAX + 1).unwrap_or(u64::MAX);
    let ended = reader.by_ref().take(first).read_until(b'\n', &mut raw);
    if raw.is_empty() {
        return None;
    }
    let mut read = raw.len();
    let cut = ended.is_err() || !raw.ends_with(b"\n");
    let (method, target) = {
        let line = String::from_utf8_lossy(&raw);
        let mut parts = line.split_ascii_whitespace();
        (
            parts.next().unwrap_or("").to_string(),
            parts.next().unwrap_or("").to_string(),
        )
    };
    let mut malformed = target.is_empty() || cut || read > HEAD_MAX;
    let mut headers = Vec::new();
    while !malformed {
        raw.clear();
        let room = u64::try_from(HEAD_MAX + 1 - read).unwrap_or(0);
        let n = reader
            .by_ref()
            .take(room)
            .read_until(b'\n', &mut raw)
            .unwrap_or(0);
        read += n;
        if n == 0 || read > HEAD_MAX || !raw.ends_with(b"\n") {
            malformed = true;
            break;
        }
        let line = String::from_utf8_lossy(&raw);
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        match line.split_once(':') {
            Some((name, value)) => {
                headers.push((name.trim().to_string(), value.trim().to_string()));
            }
            None => malformed = true,
        }
    }
    Some(Head {
        method,
        target,
        headers,
        malformed,
    })
}

/// Read one request, decide its outcome under the lock, and carry it out.
fn serve_one(stream: &TcpStream, origin: FakeOrigin, state: &Mutex<State>) {
    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
    let mut reader = BufReader::new(stream);
    let Some(head) = read_head(&mut reader) else {
        return;
    };
    let header = |name: &str| crate::header_in(&head.headers, name);
    let chunked = header("transfer-encoding").is_some_and(|v| {
        v.as_bytes()
            .windows(7)
            .any(|w| w.eq_ignore_ascii_case(b"chunked"))
    });
    let mut body = Vec::new();
    let (mut cut, mut over) = (false, false);
    // What is left to drop once answered: the rest of a body over the bound, so its 413
    // reaches a client that writes the whole body before it reads; `BODY_MAX` otherwise.
    let mut left = BODY_MAX;
    if !chunked && !head.malformed {
        let declared = header("content-length")
            .and_then(crate::parse_uint::<u64>)
            .unwrap_or(0);
        let bound = declared.min(BODY_MAX);
        if let Ok(cap) = usize::try_from(bound.min(64 * 1024)) {
            body.reserve(cap);
        }
        let _ = (&mut reader).take(bound).read_to_end(&mut body);
        cut = u64::try_from(body.len()).unwrap_or(u64::MAX) < bound;
        over = !cut && declared > BODY_MAX;
        if over {
            left = declared.saturating_sub(bound);
        }
    }
    let req = Request {
        origin,
        method: head.method,
        target: head.target,
        headers: head.headers,
        body,
        chunked,
        malformed: head.malformed,
        cut,
        over,
    };
    let outcome = routes::handle(
        &mut state.lock().unwrap_or_else(PoisonError::into_inner),
        &req,
    );
    match outcome {
        Outcome::Answer(answer) => write(stream, &req, &answer, None),
        Outcome::Close => {}
        Outcome::Hang(wait) => {
            hold(stream, &mut reader, wait, state);
            return;
        }
        Outcome::Truncate(answer, at) => write(stream, &req, &answer, Some(at)),
    }
    let _ = stream.shutdown(Shutdown::Write);
    drain(&mut reader, left);
}

/// Hold the connection `wait`, reading and dropping what the client sends, until the client
/// hangs up or [`crate::FakeGithub::end_hangs`] ends it, looking for that every `HANG_LOOK`.
fn hold(
    stream: &TcpStream,
    reader: &mut BufReader<&TcpStream>,
    wait: Duration,
    state: &Mutex<State>,
) {
    let began = std::time::Instant::now();
    let mut sink = [0_u8; 4096];
    loop {
        let left = wait.saturating_sub(began.elapsed());
        if left.is_zero()
            || state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .hangs_ended
        {
            return;
        }
        let _ = stream.set_read_timeout(Some(left.min(HANG_LOOK)));
        match reader.read(&mut sink) {
            Ok(0) => return,
            Ok(_) => {}
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => return,
        }
    }
}

/// How often a held connection looks whether its hang was ended.
const HANG_LOOK: Duration = Duration::from_millis(20);

/// Read and drop what the client sends, keeping none of it, until it hangs up, a read times
/// out, or `left` bytes have come.
fn drain(reader: &mut BufReader<&TcpStream>, left: u64) {
    let _ = std::io::copy(&mut reader.by_ref().take(left), &mut std::io::sink());
}

/// Write `answer`: no body on a HEAD or a 304, and no `Content-Length` on a 304; with `cut`,
/// only that many bytes of the body after the full head. The fake writes `Connection` and
/// `Content-Length` itself, so an answer's own header of either name is dropped.
fn write(stream: &TcpStream, req: &Request, answer: &Answer, cut: Option<usize>) {
    use std::fmt::Write as _;
    let status = answer.status;
    let mut out = String::with_capacity(192);
    let _ = write!(
        out,
        "HTTP/1.1 {status} {}\r\nConnection: close\r\n",
        reason(status)
    );
    if status != 304 {
        let _ = write!(out, "Content-Length: {}\r\n", answer.body.len());
    }
    for (name, value) in &answer.headers {
        let own =
            name.eq_ignore_ascii_case("content-length") || name.eq_ignore_ascii_case("connection");
        if !own {
            let _ = write!(out, "{name}: {value}\r\n");
        }
    }
    out.push_str("\r\n");
    let body: &[u8] = if req.method == "HEAD" || status == 304 {
        &[]
    } else {
        &answer.body[..cut.unwrap_or(answer.body.len()).min(answer.body.len())]
    };
    let mut writer = stream;
    let _ = writer
        .write_all(out.as_bytes())
        .and_then(|()| writer.write_all(body))
        .and_then(|()| writer.flush());
}

/// A status's reason phrase.
pub(crate) const fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        304 => "Not Modified",
        307 => "Temporary Redirect",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Content Too Large",
        422 => "Unprocessable Entity",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "Status",
    }
}
