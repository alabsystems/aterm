// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The system Safari, driven through macOS's `safaridriver` (WebDriver). The driver only opens
//! the first page in its session's automation window; the pages post their answers back and open
//! the next as Firefox's and WebKit's do, and their answers are what is judged. Safari's window is
//! on the desktop and takes the focus, so its session runs on the quiet machine (in a verify run
//! the browser stage holds the run's, taken before the stage's memory) and is a named skip under
//! `--no-windows` ([`no_windows_skip`]). Cleanup addresses only the created WebDriver session;
//! the Safari application is shared with the person and is never terminated by this driver.

use std::ffi::OsString;
use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_json::{Value, json};
use aterm_uds::process::Identity;

use super::cdp::HANG_DETECTOR;

/// macOS's WebDriver for its own Safari.
pub(crate) const SAFARIDRIVER: &str = "/usr/bin/safaridriver";

/// Whether this host has a Safari: macOS's own browser.
pub(crate) const HERE: bool = cfg!(target_os = "macos");

/// The one-time fix for a Safari whose remote automation is off: `--enable` asks for an
/// administrator's password, so no agent and no check applies it.
pub(crate) const ENABLE_FIX: &str = "an administrator runs `sudo safaridriver --enable` once";

/// The most of an answer the client reads.
const MAX_ANSWER: usize = 16 << 20;

/// A command's wall deadline, already expired if its hang detector cannot be represented.
fn hang_deadline(now: Instant) -> Instant {
    now.checked_add(HANG_DETECTOR).unwrap_or(now)
}

/// The Safari the lane drives.
#[derive(Debug, Clone)]
pub(crate) struct Safari {
    /// Its WebDriver.
    pub driver: PathBuf,
}

/// The Safari to drive, or why there is none (a COULD NOT RUN sentence).
pub(crate) fn find() -> Result<Safari, String> {
    find_at(std::env::consts::OS, Path::new(SAFARIDRIVER))
}

/// [`find`] on `os`, its driver at `driver`.
fn find_at(os: &str, driver: &Path) -> Result<Safari, String> {
    if os != "macos" {
        return Err(format!(
            "no Safari: it is macOS's own browser, and this host is {os}"
        ));
    }
    if !driver.is_file() {
        return Err(format!(
            "no Safari: {} is not here, and macOS ships it",
            driver.display()
        ));
    }
    Ok(Safari {
        driver: driver.to_path_buf(),
    })
}

/// What `--no-windows` answers for the Safari row.
pub(crate) fn no_windows_skip() -> String {
    "Safari (--no-windows: Safari opens its pages in an automation window on the desktop and \
     takes the focus — fix: run without --no-windows when the desktop is free)"
        .to_string()
}

/// The driver's arguments: listen on `port`.
pub(crate) fn driver_args(port: u16) -> Vec<OsString> {
    vec!["--port".into(), port.to_string().into()]
}

/// A loopback port free now, for the driver to listen on: one the kernel picked, let go.
pub(crate) fn free_port() -> Result<u16, String> {
    TcpListener::bind(("127.0.0.1", 0))
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .map_err(|e| format!("no free loopback port for safaridriver: {e}"))
}

/// Wait until a WebDriver answers `GET /status` on `port` ([`is_status`]), or `ended` says how
/// the driver ended, or the hang detector passes. A port that answers otherwise is another
/// program's, which took it after [`free_port`] let it go: the session never talks to it.
pub(crate) fn listening(
    port: u16,
    mut ended: impl FnMut() -> Option<String>,
) -> Result<(), String> {
    let t0 = Instant::now();
    let deadline = hang_deadline(t0);
    loop {
        if let Some(status) = ended() {
            return Err(format!(
                "safaridriver exited ({status}) before it listened on port {port}"
            ));
        }
        if let Ok((status, body)) = command_before(port, "GET", "/status", None, deadline) {
            return if is_status(&body) {
                Ok(())
            } else {
                Err(format!(
                    "port {port} answers GET /status with HTTP {status}, not as a WebDriver: \
                     another program took it before safaridriver did"
                ))
            };
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "safaridriver did not listen on port {port} within {} s (a hang detector, not a \
                 budget)",
                HANG_DETECTOR.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Whether `body` is a WebDriver's answer to `GET /status`: its `value` says whether it is ready.
fn is_status(body: &Value) -> bool {
    body.pointer("/value/ready")
        .and_then(Value::as_bool)
        .is_some()
}

/// The New Session command's body: Safari on this Mac.
pub(crate) fn new_session() -> Value {
    json!({"capabilities": {"alwaysMatch": {"browserName": "safari", "platformName": "mac"}}})
}

/// A WebDriver error answer's words: `<error>: "<message>"`.
fn error_words(body: &Value) -> String {
    let value = body.get("value");
    let field = |k: &str| value.and_then(|v| v.get(k)).and_then(Value::as_str);
    format!(
        "{}: \"{}\"",
        field("error").unwrap_or("no error named"),
        field("message").unwrap_or_default()
    )
}

/// The session id a New Session answer carries, or why Safari gave none (a COULD NOT RUN
/// sentence): one whose remote automation is off names [`ENABLE_FIX`].
pub(crate) fn session_id(status: u16, body: &Value) -> Result<String, String> {
    let id = body
        .pointer("/value/sessionId")
        .and_then(Value::as_str)
        .filter(|_| status == 200);
    if let Some(id) = id {
        return Ok(id.to_string());
    }
    let said = format!(
        "safaridriver gave no session (HTTP {status}, {})",
        error_words(body)
    );
    let message = body
        .pointer("/value/message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if message.to_ascii_lowercase().contains("remote automation") {
        Err(format!(
            "{said}: Safari's remote automation is off (fix: {ENABLE_FIX})"
        ))
    } else {
        Err(said)
    }
}

/// One WebDriver command to the driver on `port`, answered within the hang detector: its HTTP
/// status and its JSON body (`null` when empty).
pub(crate) fn command(
    port: u16,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Result<(u16, Value), String> {
    let deadline = hang_deadline(Instant::now());
    command_before(port, method, path, body, deadline)
}

/// A command sharing its caller's deadline, as the driver's listening probes do.
fn command_before(
    port: u16,
    method: &str,
    path: &str,
    body: Option<&Value>,
    deadline: Instant,
) -> Result<(u16, Value), String> {
    let what = format!("safaridriver's answer to {method} {path}");
    let stream = TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        time_left(deadline, Instant::now(), &what)?,
    )
    .map_err(|e| format!("could not reach safaridriver on port {port}: {e}"))?;
    let body = match body {
        Some(v) => aterm_json::to_string(v)
            .map_err(|e| format!("could not encode {method} {path}: {e}"))?,
        None => String::new(),
    };
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json; \
         charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let raw = exchange(
        request.as_bytes(),
        deadline,
        Instant::now,
        |left, bytes| {
            stream.set_write_timeout(Some(left))?;
            (&stream).write(bytes)
        },
        |left, bytes| {
            stream.set_read_timeout(Some(left))?;
            (&stream).read(bytes)
        },
        &what,
    )?;
    let answer = parse_answer(&raw).map_err(|why| format!("{what}: {why}"))?;
    time_left(deadline, Instant::now(), &what)?;
    Ok(answer)
}

/// The remaining part of one command's wall deadline; each socket operation shares it.
fn time_left(deadline: Instant, now: Instant, what: &str) -> Result<Duration, String> {
    deadline
        .checked_duration_since(now)
        .filter(|left| !left.is_zero())
        .ok_or_else(|| super::serve::hang_words(what, false))
}

/// Send the request and read the answer through one wall deadline and a byte limit. The I/O
/// and clock are handed in so deterministic controls can make progress without renewing time.
fn exchange(
    mut request: &[u8],
    deadline: Instant,
    mut now: impl FnMut() -> Instant,
    mut write: impl FnMut(Duration, &[u8]) -> std::io::Result<usize>,
    mut read: impl FnMut(Duration, &mut [u8]) -> std::io::Result<usize>,
    what: &str,
) -> Result<Vec<u8>, String> {
    let failed = |e: std::io::Error| match e.kind() {
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => {
            super::serve::hang_words(what, false)
        }
        _ => format!("{what} broke off: {e}"),
    };
    while !request.is_empty() {
        let left = time_left(deadline, now(), what)?;
        match write(left, request) {
            Ok(0) => return Err(format!("{what}: the request write made no progress")),
            Ok(n) => request = &request[n..],
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(failed(e)),
        }
    }
    let mut raw = Vec::new();
    let mut buf = [0; 8192];
    loop {
        let left = time_left(deadline, now(), what)?;
        let take = (MAX_ANSWER + 1 - raw.len()).min(buf.len());
        match read(left, &mut buf[..take]) {
            Ok(0) => {
                time_left(deadline, now(), what)?;
                return Ok(raw);
            }
            Ok(n) => {
                if n > MAX_ANSWER - raw.len() {
                    return Err(format!("{what} exceeds the {MAX_ANSWER}-byte answer limit"));
                }
                raw.extend_from_slice(&buf[..n]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(failed(e)),
        }
    }
}

/// The status and JSON body of an HTTP/1.1 answer read to its close: its body is what
/// `Content-Length` says, its chunks, or the rest.
fn parse_answer(raw: &[u8]) -> Result<(u16, Value), String> {
    let end = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("it ends before its header does")?;
    let head = std::str::from_utf8(&raw[..end]).map_err(|_| "its header is not UTF-8")?;
    let rest = &raw[end + 4..];
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|l| l.split(' ').nth(1))
        .filter(|s| s.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or("its status line names no status")?;
    let header = |name: &str| {
        lines.clone().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.trim().eq_ignore_ascii_case(name).then(|| v.trim())
        })
    };
    let body = if header("transfer-encoding").is_some_and(|v| v.eq_ignore_ascii_case("chunked")) {
        dechunk(rest)?
    } else if let Some(n) = header("content-length") {
        let n: usize = n
            .bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| n.parse().ok())
            .flatten()
            .ok_or_else(|| format!("its Content-Length {n:?} is not a length"))?;
        rest.get(..n)
            .ok_or("it ends before its Content-Length")?
            .to_vec()
    } else {
        rest.to_vec()
    };
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok((status, Value::Null));
    }
    aterm_json::from_slice(&body)
        .map(|v| (status, v))
        .map_err(|e| format!("its body is not JSON: {e}"))
}

/// A chunked body's bytes.
fn dechunk(mut rest: &[u8]) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    loop {
        let line = rest
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or("a chunk has no size line")?;
        let size = std::str::from_utf8(&rest[..line])
            .ok()
            .and_then(|s| s.split(';').next())
            .map(str::trim)
            .filter(|s| s.bytes().all(|b| b.is_ascii_hexdigit()))
            .and_then(|s| usize::from_str_radix(s, 16).ok())
            .ok_or("a chunk's size is not hex")?;
        if size == 0 {
            return Ok(body);
        }
        let start = line.checked_add(2).ok_or("a chunk's start overflows")?;
        let end = start
            .checked_add(size)
            .ok_or("a chunk's length overflows")?;
        let next = end.checked_add(2).ok_or("a chunk's end overflows")?;
        let chunk = rest.get(start..end).ok_or("it ends inside a chunk")?;
        if rest.get(end..next) != Some(b"\r\n".as_slice()) {
            return Err("a chunk has no end".to_owned());
        }
        body.extend_from_slice(chunk);
        rest = rest.get(next..).ok_or("a chunk has no end")?;
    }
}

/// A WebDriver session on the driver at `port`, deleted explicitly or once on drop. That request
/// names only this automation session; it never terminates the shared Safari application.
pub(crate) struct Session {
    port: u16,
    id: String,
    closed: Option<Result<(), String>>,
}

impl Session {
    /// The session `id` on the driver at `port`.
    pub(crate) const fn new(port: u16, id: String) -> Self {
        Self {
            port,
            id,
            closed: None,
        }
    }

    /// Open `url` in the session's window, answered once its first page loaded.
    pub(crate) fn open(&self, url: &str) -> Result<(), String> {
        let path = format!("/session/{}/url", self.id);
        let (status, body) = command(self.port, "POST", &path, Some(&json!({"url": url})))?;
        if status == 200 {
            Ok(())
        } else {
            Err(format!(
                "safaridriver could not open {url} (HTTP {status}, {})",
                error_words(&body)
            ))
        }
    }

    /// Delete this session once, preserving the result so a refused cleanup is never later
    /// reported as successful or retried by Drop.
    pub(crate) fn close(&mut self) -> Result<(), String> {
        if let Some(result) = &self.closed {
            return result.clone();
        }
        self.closed = Some(Err(
            "the automation session's cleanup is unconfirmed".to_owned()
        ));
        let result = command(self.port, "DELETE", &format!("/session/{}", self.id), None).and_then(
            |(status, body)| {
                if status == 200 {
                    Ok(())
                } else {
                    Err(format!(
                        "safaridriver could not delete its automation session (HTTP {status}, {})",
                        error_words(&body)
                    ))
                }
            },
        );
        self.closed = Some(result.clone());
        result
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if self.closed.is_none()
            && let Err(why) = self.close()
        {
            eprintln!(
                "verify: {why}; its automation window may remain; the shared Safari application \
                 is left alone"
            );
        }
    }
}

/// Whether the user agent the pages report is Safari's: WebKit's, naming `Safari/`.
pub(crate) fn is_safari(user_agent: &str) -> bool {
    super::webkit::is_webkit(user_agent)
        && user_agent
            .split_whitespace()
            .any(|w| w.starts_with("Safari/"))
}

/// The session's product: `Safari <version>` from Safari's user agent; another browser's user
/// agent as it is.
pub(crate) fn product(user_agent: &str) -> String {
    if !is_safari(user_agent) {
        return user_agent.to_string();
    }
    user_agent
        .split_whitespace()
        .find_map(|w| w.strip_prefix("Version/"))
        .map_or_else(|| "Safari".to_string(), |v| format!("Safari {v}"))
}

/// Whether `exe` is a Safari's executable.
#[cfg(any(target_vendor = "apple", test))]
fn is_safari_app(exe: &Path) -> bool {
    exe.ends_with("Safari.app/Contents/MacOS/Safari")
}

/// This user's Safari processes now, or `None` where they could not be listed.
#[cfg(target_vendor = "apple")]
pub(crate) fn running() -> Option<Vec<Identity>> {
    let pids = aterm_uds::holders::uid_pids().ok()?;
    Some(
        pids.into_iter()
            .filter_map(|pid| {
                let pid = i32::try_from(pid).ok()?;
                let exe = aterm_uds::process::read_process_path(pid)?;
                is_safari_app(&exe)
                    .then(|| aterm_uds::process::read_identity(pid))
                    .flatten()
            })
            .collect(),
    )
}

/// See the Apple twin: no Safari runs here.
#[cfg(not(target_vendor = "apple"))]
pub(crate) fn running() -> Option<Vec<Identity>> {
    None
}

/// How the session's Safari ended: `safaris`, this user's Safaris once the session was made
/// ([`running`]), every one of them gone. `None` while one runs, or where none was listed.
#[cfg(target_vendor = "apple")]
pub(crate) fn quit(safaris: &[Identity]) -> Option<String> {
    let gone = !safaris.is_empty()
        && safaris
            .iter()
            .all(|s| aterm_uds::process::read_identity(s.pid) != Some(*s));
    gone.then(|| {
        let pids: Vec<String> = safaris.iter().map(|s| s.pid.to_string()).collect();
        format!("Safari (pid {}) quit", pids.join(", "))
    })
}

/// See the Apple twin: no Safari runs here ([`running`]).
#[cfg(not(target_vendor = "apple"))]
pub(crate) const fn quit(_: &[Identity]) -> Option<String> {
    None
}

#[cfg(test)]
#[path = "safari_tests.rs"]
mod tests;
