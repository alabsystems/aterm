// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Private smart HTTP backed by real Git objects and services, independent of FakeGithub.
//! Requests use bounded Content-Length framing (no chunked transfer); Git CGI input,
//! output and execution are bounded. All repositories are owned mirrors of caller fixtures.

use std::io::{self, BufReader};
use std::net::{Ipv4Addr, Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::routes::{Answer, Request};
use crate::{FakeOrigin, header_in, server};

const LIMIT: usize = 16 * 1024 * 1024;
const HANG: Duration = Duration::from_secs(60);

/// The Git service requested, excluding arbitrary targets or queries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Service {
    /// Ref discovery or object download.
    Upload,
    /// Ref discovery or object/ref upload.
    Receive,
}

/// Identity recognized by the fixture's private authentication gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Identity {
    /// No credential.
    Anonymous,
    /// A malformed, duplicated or unknown credential.
    Unknown,
    /// The pull-only fixture reader.
    Reader,
    /// The fixture writer, which can also read.
    Writer,
}

/// Allowlisted evidence, containing no token, raw header, request body or CGI environment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observation {
    /// Whether this was a POST; accepted discovery requests are GETs.
    pub post: bool,
    /// Accepted Git service, or none for an unknown path/method.
    pub service: Option<Service>,
    /// Recognized identity.
    pub identity: Identity,
    /// HTTP status produced before any deliberately lost response.
    pub status: u16,
    /// Whether the CGI adapter was called; this alone is not an exec/effect proof.
    pub backend_attempted: bool,
    /// Whether this response was deliberately dropped after CGI completion.
    pub response_dropped: bool,
}

struct State {
    root: PathBuf,
    slug: String,
    reader: String,
    writer: String,
    stop: AtomicBool,
    lose_receive: AtomicBool,
    active: Mutex<Option<TcpStream>>,
    seen: Mutex<Vec<Observation>>,
}

/// One serialized loopback server and its owned object repository. Drop cancels its
/// active socket/CGI work and joins the worker before removing the repository.
pub struct GitHttpBackend {
    state: Arc<State>,
    port: u16,
    worker: Option<JoinHandle<()>>,
    repository: PathBuf,
    version: String,
    _root: aterm_tempfile::TempDir,
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn git(root: &Path) -> aterm_uds::spawnfd::Command {
    let mut command = aterm_uds::fixture_git::git();
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join("home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .current_dir(root);
    command
}

fn setup(command: &mut aterm_uds::spawnfd::Command) -> io::Result<Vec<u8>> {
    let out = aterm_uds::deadline::output_with_input_until(
        command,
        aterm_uds::secret::Secret::new(b""),
        Instant::now() + HANG,
        LIMIT,
    )?;
    if !out.status.success() {
        return Err(io::Error::other("Git fixture setup failed"));
    }
    Ok(out.stdout)
}

impl GitHttpBackend {
    /// Mirrors a caller's fixture repository under a fresh private root and binds loopback.
    /// The slug has exactly two plain ASCII path segments. Tokens must be nonempty/distinct.
    /// The mirror has no caller hooks/config and disables automatic maintenance.
    /// # Errors
    /// Invalid slug/tokens, scratch, Git clone/config, listener or worker creation failure.
    pub fn start(slug: &str, source: &Path, reader: &str, writer: &str) -> io::Result<Self> {
        let segments: Vec<_> = slug.split('/').collect();
        if segments.len() != 2
            || segments.iter().any(|s| {
                s.is_empty()
                    || *s == "."
                    || *s == ".."
                    || !s
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
            })
        {
            return Err(invalid("expected a two-segment fixture slug"));
        }
        if reader.is_empty() || writer.is_empty() || reader == writer {
            return Err(invalid(
                "fixture reader and writer must be distinct nonempty tokens",
            ));
        }
        let root = aterm_tempfile::Builder::new()
            .prefix("git-http-")
            .suffix(".noindex")
            .tempdir()?;
        std::fs::create_dir(root.path().join("home"))?;
        let source = std::fs::canonicalize(source)?;
        let repository = root.path().join(format!("{slug}.git"));
        std::fs::create_dir_all(
            repository
                .parent()
                .ok_or_else(|| invalid("no repository parent"))?,
        )?;
        setup(
            git(root.path())
                .args(["clone", "--quiet", "--mirror", "--local", "--"])
                .arg(source)
                .arg(&repository),
        )?;
        for (key, value) in [
            ("http.receivepack", "true"),
            ("gc.auto", "0"),
            ("maintenance.auto", "false"),
        ] {
            setup(
                git(root.path())
                    .arg("-C")
                    .arg(&repository)
                    .args(["config", key, value]),
            )?;
        }
        let version = String::from_utf8(setup(git(root.path()).arg("--version"))?)
            .map_err(|_| invalid("Git version is not UTF-8"))?
            .trim()
            .to_owned();
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let port = listener.local_addr()?.port();
        let state = Arc::new(State {
            root: root.path().to_owned(),
            slug: slug.to_owned(),
            reader: reader.to_owned(),
            writer: writer.to_owned(),
            stop: AtomicBool::new(false),
            lose_receive: AtomicBool::new(false),
            active: Mutex::new(None),
            seen: Mutex::new(Vec::new()),
        });
        let owned = Arc::clone(&state);
        let worker = std::thread::Builder::new()
            .name("git-http-fixture".into())
            .spawn(move || {
                loop {
                    if owned.stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok((stream, _)) = aterm_uds::accept(
                        &listener,
                        aterm_uds::AcceptWait::Forever,
                        aterm_uds::AcceptedMode::Blocking,
                    ) else {
                        break;
                    };
                    // Register under the same lock Drop takes, then recheck cancellation.
                    let Ok(active) = stream.try_clone() else {
                        continue;
                    };
                    *owned.active.lock().unwrap_or_else(PoisonError::into_inner) = Some(active);
                    if !owned.stop.load(Ordering::SeqCst) {
                        serve(&stream, &owned);
                    }
                    *owned.active.lock().unwrap_or_else(PoisonError::into_inner) = None;
                }
            })?;
        Ok(Self {
            state,
            port,
            worker: Some(worker),
            repository,
            version,
            _root: root,
        })
    }

    /// Bound web port, composable with the separate FakeGithub API origin.
    #[must_use]
    pub const fn port(&self) -> u16 {
        self.port
    }

    /// Owned mirror for ref/object assertions and deliberate fixture-state changes.
    #[must_use]
    pub fn repository(&self) -> &Path {
        &self.repository
    }

    /// Version of the backend Git executable under its isolated environment.
    #[must_use]
    pub fn git_version(&self) -> &str {
        &self.version
    }

    /// All requests in order; no evidence is cleared between producers and consumers.
    #[must_use]
    pub fn seen(&self) -> Vec<Observation> {
        self.state
            .seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Lose the next authenticated receive POST's successful HTTP response after Git
    /// completes. Ref state, not this fault flag, determines whether a write actually landed.
    pub fn lose_next_receive_response(&self) {
        self.state.lose_receive.store(true, Ordering::SeqCst);
    }
}

impl Drop for GitHttpBackend {
    fn drop(&mut self) {
        self.state.stop.store(true, Ordering::SeqCst);
        if let Some(stream) = self
            .state
            .active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            let _ = stream.shutdown(Shutdown::Both);
        }
        let _ = TcpStream::connect((Ipv4Addr::LOCALHOST, self.port));
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn identity(req: &Request, state: &State) -> Identity {
    let credentials: Vec<_> = req
        .headers
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case("authorization"))
        .collect();
    if credentials.is_empty() {
        return Identity::Anonymous;
    }
    if credentials.len() != 1 {
        return Identity::Unknown;
    }
    let Some((scheme, encoded)) = credentials[0].1.split_once(' ') else {
        return Identity::Unknown;
    };
    if !scheme.eq_ignore_ascii_case("basic") {
        return Identity::Unknown;
    }
    let Ok(raw) = aterm_codec::base64::decode(encoded) else {
        return Identity::Unknown;
    };
    let Ok(text) = std::str::from_utf8(&raw) else {
        return Identity::Unknown;
    };
    let Some(token) = text.strip_prefix("x-access-token:") else {
        return Identity::Unknown;
    };
    if token == state.writer {
        Identity::Writer
    } else if token == state.reader {
        Identity::Reader
    } else {
        Identity::Unknown
    }
}

fn endpoint(req: &Request, state: &State) -> Option<(Service, &'static str, &'static str)> {
    let prefix = format!("/{}.git/", state.slug);
    match (req.method.as_str(), req.target.strip_prefix(&prefix)?) {
        ("GET", "info/refs?service=git-upload-pack") => {
            Some((Service::Upload, "info/refs", "service=git-upload-pack"))
        }
        ("GET", "info/refs?service=git-receive-pack") => {
            Some((Service::Receive, "info/refs", "service=git-receive-pack"))
        }
        ("POST", "git-upload-pack") => Some((Service::Upload, "git-upload-pack", "")),
        ("POST", "git-receive-pack") => Some((Service::Receive, "git-receive-pack", "")),
        _ => None,
    }
}

fn plain(status: u16) -> Answer {
    Answer {
        status,
        headers: vec![("Content-Type".into(), "text/plain".into())],
        body: Vec::new(),
    }
}

fn serve(stream: &TcpStream, state: &State) {
    let _ = stream.set_read_timeout(Some(HANG));
    let _ = stream.set_write_timeout(Some(HANG));
    let mut reader = BufReader::new(stream);
    let Some((req, _)) = server::read_request(&mut reader, FakeOrigin::Web) else {
        return;
    };
    let who = identity(&req, state);
    let route = endpoint(&req, state);
    let mut started = false;
    let header = |name: &str| header_in(&req.headers, name);
    let framing = !req.malformed
        && !req.cut
        && !req.chunked
        && !req.over
        && header("transfer-encoding").is_none()
        && req
            .headers
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
            .count()
            <= 1
        && header("content-length")
            .is_none_or(|n| crate::parse_uint::<usize>(n) == Some(req.body.len()));
    let answer = if !framing {
        plain(if req.over { 413 } else { 400 })
    } else if let Some((service, path, query)) = route {
        let allowed =
            who == Identity::Writer || (who == Identity::Reader && service == Service::Upload);
        if !allowed {
            let mut a = plain(401);
            a.headers
                .push(("WWW-Authenticate".into(), "Basic realm=\"fixture\"".into()));
            a
        } else {
            let expected = match service {
                Service::Upload => "application/x-git-upload-pack-request",
                Service::Receive => "application/x-git-receive-pack-request",
            };
            if req.method == "POST" && header("content-type") != Some(expected) {
                plain(400)
            } else {
                started = true;
                backend(state, &req, who, path, query).unwrap_or_else(|_| plain(502))
            }
        }
    } else {
        plain(404)
    };
    let dropped = started
        && req.method == "POST"
        && route.is_some_and(|(s, _, _)| s == Service::Receive)
        && answer.status == 200
        && state.lose_receive.swap(false, Ordering::SeqCst);
    state
        .seen
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(Observation {
            post: req.method == "POST",
            service: route.map(|r| r.0),
            identity: who,
            status: answer.status,
            backend_attempted: started,
            response_dropped: dropped,
        });
    if !dropped {
        server::write(stream, &req, &answer, None);
    }
    let _ = stream.shutdown(Shutdown::Both);
}

fn backend(
    state: &State,
    req: &Request,
    who: Identity,
    path: &str,
    query: &str,
) -> io::Result<Answer> {
    let mut command = git(&state.root);
    command
        .arg("http-backend")
        .env("GIT_PROJECT_ROOT", &state.root)
        .env("GIT_HTTP_EXPORT_ALL", "1")
        .env("REQUEST_METHOD", &req.method)
        .env("SERVER_PROTOCOL", "HTTP/1.1")
        .env("PATH_INFO", format!("/{}.git/{path}", state.slug))
        .env("QUERY_STRING", query)
        .env("CONTENT_LENGTH", req.body.len().to_string())
        .env(
            "CONTENT_TYPE",
            header_in(&req.headers, "content-type").unwrap_or(""),
        )
        .env(
            "REMOTE_USER",
            if who == Identity::Writer {
                "writer"
            } else {
                "reader"
            },
        )
        .env("REMOTE_ADDR", "127.0.0.1");
    if let Some(protocol) = header_in(&req.headers, "git-protocol") {
        if !matches!(protocol, "version=1" | "version=2") {
            return Err(invalid("unsupported Git protocol"));
        }
        command.env("HTTP_GIT_PROTOCOL", protocol);
    }
    let out = aterm_uds::deadline::output_with_input_until_watching(
        &mut command,
        aterm_uds::secret::Secret::new(&req.body),
        Instant::now() + HANG,
        LIMIT,
        || state.stop.load(Ordering::SeqCst),
    )?
    .ok_or_else(|| io::Error::new(io::ErrorKind::Interrupted, "fixture stopped"))?;
    if !out.status.success() {
        return Err(io::Error::other("Git backend failed"));
    }
    parse_cgi(&out.stdout)
}

fn parse_cgi(bytes: &[u8]) -> io::Result<Answer> {
    let (end, separator) = bytes
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|n| (n, 4))
        .or_else(|| bytes.windows(2).position(|w| w == b"\n\n").map(|n| (n, 2)))
        .ok_or_else(|| invalid("CGI response has no header terminator"))?;
    if end > 64 * 1024 {
        return Err(invalid("CGI header exceeds bound"));
    }
    let text =
        std::str::from_utf8(&bytes[..end]).map_err(|_| invalid("CGI headers are not UTF-8"))?;
    let mut status = None;
    let mut headers = Vec::new();
    for line in text.lines() {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| invalid("invalid CGI header"))?;
        let value = value.trim();
        if value.bytes().any(|b| b < 32 || b == 127) {
            return Err(invalid("CGI header has a control byte"));
        }
        if name.eq_ignore_ascii_case("status") {
            let number = value.split_once(' ').map_or(value, |(n, _)| n);
            let code = crate::parse_uint::<u16>(number)
                .filter(|n| (200..600).contains(n))
                .ok_or_else(|| invalid("invalid CGI status"))?;
            if status.replace(code).is_some() {
                return Err(invalid("duplicate CGI status"));
            }
        } else if ["content-type", "cache-control", "expires", "pragma"]
            .iter()
            .any(|n| name.eq_ignore_ascii_case(n))
        {
            headers.push((name.to_owned(), value.to_owned()));
        } else {
            return Err(invalid("unexpected CGI header"));
        }
    }
    Ok(Answer {
        status: status.unwrap_or(200),
        headers,
        body: bytes[end + separator..].to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::parse_cgi;

    #[test]
    fn cgi_binary_body_is_exact_and_framing_headers_are_refused() {
        let answer = parse_cgi(
            b"Status: 403 Forbidden\r\nContent-Type: application/octet-stream\r\n\r\n\0\xff",
        )
        .unwrap();
        assert_eq!(answer.status, 403);
        assert_eq!(answer.body, [0, 255]);
        for malformed in [
            b"Content-Length: 9\n\n".as_slice(),
            b"Status: 200 OK\nStatus: 201 Created\n\n",
            b"Content-Type: a\x01b\n\n",
            b"no terminator",
        ] {
            assert!(parse_cgi(malformed).is_err());
        }
    }
}
