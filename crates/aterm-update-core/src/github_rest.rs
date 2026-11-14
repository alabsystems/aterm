// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! GitHub REST API client over [`crate::http`] (`curl`) with stdin-piped [`Token::new`],
//! [`RestError::Local`] preflight, [`RestError::Redirect`] policy ([`Client::asset_location`]),
//! and [`crate::github_pages`] pagination.

use std::io::Write as _;
use std::path::Path;
use std::sync::Arc;

use aterm_json::Value;
use aterm_types::http_retry::{Budget, Cause, Decision, Policy, Retry};
use aterm_uds::secret::Secret;
use aterm_uds::spawnfd::{Command, Stdio};

use crate::http::{VENDOR_ENV_SCRUB, VendorScratch, curl_command};

/// The REST API version every api and uploads request names.
pub const API_VERSION: &str = "2022-11-28";
/// The JSON media type.
pub const ACCEPT_JSON: &str = "application/vnd.github+json";
/// An asset's bytes, which the asset API answers with a redirect to the CDN.
pub const ACCEPT_BYTES: &str = "application/octet-stream";
/// The largest JSON answer read (a listing page of 100 releases is well under 1 MiB).
const JSON_CAP: u64 = 64 << 20;
/// How long a connect may take.
const CONNECT_SECS: &str = "30";
/// How long an api call may take, its transfer included.
const API_SECS: &str = "300";
/// A transfer slower than this many bytes per second for [`STALL_SECS`] is ended.
const STALL_BYTES: &str = "1";
/// See [`STALL_BYTES`].
const STALL_SECS: &str = "300";
/// The longest header dump read.
const HEADERS_CAP: u64 = 1 << 20;
/// The variables a GitHub token is exported in.
const TOKEN_ENV: [&str; 2] = ["GH_TOKEN", "GITHUB_TOKEN"];
/// Short partial transfers may earn this many immediate resumes. Beyond it, every resume must
/// have advanced by at least [`RESUME_EARNED_AT`].
const SHORT_RESUMES: u32 = 16;
/// Progress that earns another large-transfer attempt after [`SHORT_RESUMES`] short ones.
const RESUME_EARNED_AT: u64 = 1 << 20;

/// The origins a client talks to: the api and uploads origins carry the token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Origins {
    /// `https://api.github.com`.
    pub api: String,
    /// `https://uploads.github.com`.
    pub uploads: String,
    /// `https://github.com`.
    pub web: String,
    /// The asset CDN's origins.
    pub cdn: Vec<String>,
}

impl Origins {
    /// github.com's.
    #[must_use]
    pub fn github() -> Origins {
        Origins {
            api: "https://api.github.com".into(),
            uploads: "https://uploads.github.com".into(),
            web: "https://github.com".into(),
            cdn: vec![
                "https://release-assets.githubusercontent.com".into(),
                "https://objects.githubusercontent.com".into(),
            ],
        }
    }

    /// A loopback fixture's four origins (plain HTTP on 127.0.0.1), for tests.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn loopback(api: u16, uploads: u16, web: u16, cdn: u16) -> Origins {
        let at = |port: u16| format!("http://127.0.0.1:{port}");
        Origins {
            api: at(api),
            uploads: at(uploads),
            web: at(web),
            cdn: vec![at(cdn)],
        }
    }

    /// Whether every origin is https, so curl may be told to speak nothing else.
    fn https(&self) -> bool {
        [&self.api, &self.uploads, &self.web]
            .into_iter()
            .chain(&self.cdn)
            .all(|origin| origin.starts_with("https://"))
    }

    /// Whether `url` is on `origin`.
    fn on(origin: &str, url: &str) -> bool {
        url.strip_prefix(origin)
            .is_some_and(|rest| rest.starts_with('/'))
            && url.bytes().all(|b| b > b' ' && b != 0x7f && b != b'\\')
    }
}

/// A GitHub token: visible ASCII only, held as a [`Secret`].
pub struct Token(Secret);

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Token(<redacted>)")
    }
}

impl Token {
    /// `text` with surrounding ASCII whitespace dropped.
    /// # Errors
    /// An empty token, or one holding a byte outside visible ASCII (`!`..=`~`).
    pub fn new(text: &[u8]) -> Result<Token, String> {
        let token = text.trim_ascii();
        if token.is_empty() {
            return Err("the GitHub token is empty".into());
        }
        if !token.iter().all(|b| (0x21..=0x7e).contains(b)) {
            return Err("the GitHub token holds a byte that is not visible ASCII".into());
        }
        Ok(Token(Secret::new(token)))
    }

    /// The token in `path`: `None` when the file is absent or holds only whitespace.
    /// # Errors
    /// A file that cannot be read, is not a regular file, exceeds 16 KiB, or holds a token
    /// [`Token::new`] refuses.
    pub fn from_file(path: &Path) -> Result<Option<Token>, String> {
        const TOKEN_FILE_CAP: usize = 16 * 1024;
        use std::io::Read as _;

        let mut opts = std::fs::OpenOptions::new();
        opts.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC);
        }
        let file = match opts.open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(format!(
                    "cannot read the token file {}: {e}",
                    path.display()
                ));
            }
        };
        let meta = match file.metadata() {
            Ok(meta) if meta.is_file() => meta,
            Ok(_) => {
                return Err(format!(
                    "cannot read the token file {}: not a regular file",
                    path.display()
                ));
            }
            Err(e) => {
                return Err(format!(
                    "cannot read the token file {}: {e}",
                    path.display()
                ));
            }
        };
        if meta.len() > TOKEN_FILE_CAP as u64 {
            return Err(format!(
                "cannot read the token file {}: exceeds {TOKEN_FILE_CAP} bytes",
                path.display()
            ));
        }
        let mut bytes =
            Vec::with_capacity(usize::try_from(meta.len().saturating_add(1)).unwrap_or(0));
        if let Err(e) = file
            .take((TOKEN_FILE_CAP + 1) as u64)
            .read_to_end(&mut bytes)
        {
            aterm_uds::secret::scrub(&mut bytes);
            return Err(format!(
                "cannot read the token file {}: {e}",
                path.display()
            ));
        }
        let token = if bytes.len() > TOKEN_FILE_CAP {
            Err(format!(
                "cannot read the token file {}: exceeds {TOKEN_FILE_CAP} bytes",
                path.display()
            ))
        } else if bytes.trim_ascii().is_empty() {
            Ok(None)
        } else {
            Token::new(&bytes)
                .map(Some)
                .map_err(|why| format!("{why} ({})", path.display()))
        };
        aterm_uds::secret::scrub(&mut bytes);
        token
    }

    /// The token's bytes, for the one child no stdin reaches.
    #[must_use]
    pub const fn expose(&self) -> &[u8] {
        self.0.expose()
    }

    /// The header line curl reads on stdin (`-H @-`).
    fn header_line(&self) -> Secret {
        const AUTHORIZATION: &[u8] = b"Authorization: Bearer ";
        let mut line = Vec::with_capacity(AUTHORIZATION.len() + self.0.len() + 1);
        line.extend_from_slice(AUTHORIZATION);
        line.extend_from_slice(self.0.expose());
        line.push(b'\n');
        Secret::from_vec(line)
    }
}

/// How a client starts curl: [`Direct`] in production.
pub trait Spawn: Send + Sync {
    /// Run `command` to its end.
    /// # Errors
    /// The spawn's, marked by [`unstarted`] (the child never ran, so it sent nothing), or
    /// reading or waiting.
    fn run(
        &self,
        command: &mut Command,
        stdin: Option<&[u8]>,
    ) -> std::io::Result<std::process::Output>;
}

/// A [`Spawn::run`] error raised before the child started ([`unstarted`]).
#[derive(Debug)]
struct NotStarted(std::io::Error);

impl std::fmt::Display for NotStarted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for NotStarted {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

/// `e`, a failure to start a child, marked so [`never_started`] knows it: its kind and its
/// message are `e`'s.
#[must_use]
pub fn unstarted(e: std::io::Error) -> std::io::Error {
    std::io::Error::new(e.kind(), NotStarted(e))
}

/// Whether `e` is a [`Spawn::run`] failure raised before the child started ([`unstarted`]),
/// so nothing it would have sent went anywhere.
#[must_use]
pub fn never_started(e: &std::io::Error) -> bool {
    e.get_ref().is_some_and(|inner| inner.is::<NotStarted>())
}

/// The production [`Spawn`].
#[derive(Clone, Copy, Debug, Default)]
pub struct Direct;

impl Spawn for Direct {
    fn run(
        &self,
        command: &mut Command,
        stdin: Option<&[u8]>,
    ) -> std::io::Result<std::process::Output> {
        command
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(unstarted)?;
        if let (Some(bytes), Some(mut pipe)) = (stdin, child.stdin.take()) {
            let _ = pipe.write_all(bytes);
        }
        child.wait_with_output()
    }
}

/// An HTTP method.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    /// GET.
    Get,
    /// POST.
    Post,
    /// PATCH.
    Patch,
    /// DELETE.
    Delete,
}

impl Method {
    /// Its name on the wire (`GET`, `POST`, …).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
        }
    }
}

/// A request's body.
#[derive(Clone, Copy, Debug)]
pub enum Body<'a> {
    /// None.
    Empty,
    /// A JSON document (`--data-binary` from a private scratch file).
    Json(&'a [u8]),
    /// A file's bytes, streamed off disk (`--upload-file`, [`upload_path`]).
    File(&'a Path),
}

/// One request to the api or uploads origin.
#[derive(Clone, Copy, Debug)]
pub struct Request<'a> {
    /// The method.
    pub method: Method,
    /// The absolute URL, on the api or uploads origin.
    pub url: &'a str,
    /// The `Accept` media type.
    pub accept: &'a str,
    /// The body.
    pub body: Body<'a>,
    /// `--fail-with-body` ([`Exchange::exit`]).
    pub fail_with_body: bool,
}

impl<'a> Request<'a> {
    /// A JSON GET of `url`.
    #[must_use]
    pub const fn get(url: &'a str) -> Request<'a> {
        Request {
            method: Method::Get,
            url,
            accept: ACCEPT_JSON,
            body: Body::Empty,
            fail_with_body: false,
        }
    }
}

/// What one curl run came to, as curl reported it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exchange {
    /// curl's exit code; `None` when a signal ended it.
    pub exit: Option<i32>,
    /// curl's stderr.
    pub stderr: String,
    /// The HTTP status, when an answer came.
    pub status: Option<u16>,
    /// The answer's headers (its last header block), names as sent.
    pub headers: Vec<(String, String)>,
    /// The answer's body.
    pub body: Vec<u8>,
}

impl Exchange {
    /// The answer, whatever its status.
    /// # Errors
    /// [`RestError::Malformed`] for an answer over its cap; [`RestError::Transport`] when curl
    /// reached no answer.
    pub fn answer(self) -> Result<Answer, RestError> {
        let retry_after_secs = retry_after_header(&self.headers, "retry-after");
        if answer_exceeds_cap(self.exit, self.status, &self.stderr, self.body.len() as u64) {
            return Err(RestError::Malformed(format!(
                "GitHub's answer exceeds its {JSON_CAP}-byte cap"
            )));
        }
        match (self.exit, self.status) {
            (Some(0), Some(status)) | (Some(22), Some(status)) => Ok(Answer {
                status,
                headers: self.headers,
                body: self.body,
            }),
            (exit, status) => Err(RestError::Transport {
                exit,
                status,
                retry_after_secs,
                message: format!(
                    "curl reached no answer ({}): {}",
                    exit.map_or_else(|| "ended by a signal".to_string(), |c| format!("exit {c}")),
                    self.stderr.trim()
                ),
            }),
        }
    }
}

fn answer_exceeds_cap(exit: Option<i32>, status: Option<u16>, stderr: &str, body_len: u64) -> bool {
    !matches!(status, Some(400..=599))
        && (exit == Some(63)
            || body_len > JSON_CAP
            || stderr.contains("Maximum file size exceeded"))
}

/// An HTTP answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answer {
    /// The status.
    pub status: u16,
    /// The headers, names as sent.
    pub headers: Vec<(String, String)>,
    /// The body.
    pub body: Vec<u8>,
}

impl Answer {
    /// Header `name`'s value, ASCII case-insensitively (the first, when repeated).
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// A valid `Retry-After` as a delay in seconds. HTTP-date values are measured from now;
    /// malformed values are not a retry instruction for the bounded anonymous-read policy.
    #[must_use]
    pub fn retry_after_secs(&self) -> Option<u64> {
        retry_after_header(&self.headers, "retry-after")
    }

    /// GitHub's own words for this answer.
    #[must_use]
    pub fn message(&self) -> String {
        let Ok(doc) = aterm_json::from_slice::<Value>(&self.body) else {
            return format!("HTTP {}", self.status);
        };
        let message = doc
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let codes: Vec<String> = doc
            .get("errors")
            .and_then(Value::as_array)
            .map(|errors| {
                errors
                    .iter()
                    .filter_map(|e| {
                        let field = e.get("field").and_then(Value::as_str).unwrap_or("");
                        let code = e
                            .get("code")
                            .or_else(|| e.get("message"))
                            .and_then(Value::as_str)?;
                        Some(if field.is_empty() {
                            code.to_string()
                        } else {
                            format!("{field}: {code}")
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        match (message.is_empty(), codes.is_empty()) {
            (true, true) => format!("HTTP {}", self.status),
            (false, true) => message,
            (true, false) => codes.join("; "),
            (false, false) => format!("{message} ({})", codes.join("; ")),
        }
    }

    /// The answer as a call takes it: any 2xx.
    /// # Errors
    /// [`RestError::Redirect`] for a 3xx (never followed); [`RestError::Status`] for any other.
    pub fn success(self) -> Result<Answer, RestError> {
        match self.status {
            200..=299 => Ok(self),
            300..=399 => Err(RestError::Redirect(format!(
                "GitHub answered HTTP {} (a redirect, refused: {})",
                self.status,
                self.header("location")
                    .map_or(std::borrow::Cow::Borrowed("no Location"), origin_of)
            ))),
            status => {
                let retry_after_secs = self.retry_after_secs();
                Err(RestError::Status {
                    status,
                    retry_after_secs,
                    message: self.message(),
                })
            }
        }
    }

    /// The body as JSON.
    /// # Errors
    /// [`RestError::Malformed`] for a body that is not.
    pub fn json(&self) -> Result<Value, RestError> {
        aterm_json::from_slice(&self.body)
            .map_err(|_| RestError::Malformed("GitHub's answer is not JSON".into()))
    }
}

/// Header `name` as a `Retry-After` delay measured from `now_unix_secs`.
fn retry_after_header_at(
    headers: &[(String, String)],
    name: &str,
    now_unix_secs: Option<u64>,
) -> Option<u64> {
    headers
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
        .and_then(|value| aterm_types::http_retry::retry_after_delay_secs(value, now_unix_secs))
}

fn retry_after_header(headers: &[(String, String)], name: &str) -> Option<u64> {
    retry_after_header_at(headers, name, aterm_time::unix_secs())
}

/// The scheme and host/port of `url`, never its userinfo, path, or query (a CDN URL's signature).
fn origin_of(url: &str) -> std::borrow::Cow<'_, str> {
    if let Some((scheme, rest)) = url.split_once("://")
        && !scheme.is_empty()
        && scheme.as_bytes()[0].is_ascii_alphabetic()
        && scheme
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
    {
        let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
        if let Some((_, host)) = authority.rsplit_once('@') {
            return std::borrow::Cow::Owned(format!("{scheme}://{host}"));
        }
        return std::borrow::Cow::Borrowed(&url[..scheme.len() + 3 + authority.len()]);
    }
    if let Some(rest) = url.strip_prefix("//") {
        let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
        if let Some((_, host)) = authority.rsplit_once('@') {
            return std::borrow::Cow::Owned(format!("//{host}"));
        }
        return std::borrow::Cow::Borrowed(&url[..2 + authority.len()]);
    }
    std::borrow::Cow::Borrowed("a relative URL")
}

/// Why a call failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RestError {
    /// Refused here before curl ran.
    Local(String),
    /// curl reached no usable answer.
    Transport {
        /// curl's exit code, when a child ran and exited normally.
        exit: Option<i32>,
        /// The HTTP status curl observed before the transport failed, when it reported one.
        status: Option<u16>,
        /// A valid `Retry-After` response header, converted to seconds.
        retry_after_secs: Option<u64>,
        /// The diagnostic.
        message: String,
    },
    /// GitHub answered a status the call does not take, in its own words.
    Status {
        /// The status.
        status: u16,
        /// A valid `Retry-After` response header, converted to seconds.
        retry_after_secs: Option<u64>,
        /// GitHub's message ([`Answer::message`]).
        message: String,
    },
    /// A redirect refused.
    Redirect(String),
    /// An answer that is not what the call reads.
    Malformed(String),
}

impl RestError {
    /// The HTTP status GitHub answered, including one observed before a transport failure.
    #[must_use]
    pub const fn status(&self) -> Option<u16> {
        match self {
            Self::Status { status, .. } => Some(*status),
            Self::Transport { status, .. } => *status,
            _ => None,
        }
    }

    /// curl's exit code for a transport failure, when one ran to an exit.
    #[must_use]
    pub const fn curl_exit(&self) -> Option<i32> {
        match self {
            Self::Transport { exit, .. } => *exit,
            _ => None,
        }
    }

    /// A valid `Retry-After` response header, converted to seconds.
    #[must_use]
    pub const fn retry_after_secs(&self) -> Option<u64> {
        match self {
            Self::Status {
                retry_after_secs, ..
            }
            | Self::Transport {
                retry_after_secs, ..
            } => *retry_after_secs,
            _ => None,
        }
    }

    /// The shared anonymous-read policy's transient cause, when asking this idempotent read
    /// again can change the result. An answered 4xx takes precedence over curl's exit code.
    #[must_use]
    pub const fn retry_cause(&self) -> Option<Cause> {
        match self {
            Self::Status {
                status,
                retry_after_secs,
                ..
            } => Cause::from_http(*status, *retry_after_secs),
            Self::Transport {
                exit,
                status,
                retry_after_secs,
                ..
            } => Cause::from_curl(*exit, *status, *retry_after_secs),
            Self::Local(_) | Self::Redirect(_) | Self::Malformed(_) => None,
        }
    }

    /// Curl could not continue a partial response because the server declined the requested
    /// range. One fresh request can still complete the same idempotent read.
    fn resume_refused(&self) -> bool {
        self.curl_exit() == Some(33) && !self.status().is_some_and(|status| status >= 400)
    }

    /// Preserve this error's classification while naming the retry bound that was exhausted.
    fn after_bound(self, bound: &str) -> Self {
        let suffix = format!(" after {bound}");
        match self {
            Self::Transport {
                exit,
                status,
                retry_after_secs,
                mut message,
            } => {
                message.push_str(&suffix);
                Self::Transport {
                    exit,
                    status,
                    retry_after_secs,
                    message,
                }
            }
            Self::Status {
                status,
                retry_after_secs,
                mut message,
            } => {
                message.push_str(&suffix);
                Self::Status {
                    status,
                    retry_after_secs,
                    message,
                }
            }
            other => other,
        }
    }

    /// Whether GitHub answered 404.
    #[must_use]
    pub const fn is_not_found(&self) -> bool {
        matches!(self.status(), Some(404))
    }
}

impl std::fmt::Display for RestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Local(m) | Self::Redirect(m) | Self::Malformed(m) => f.write_str(m),
            Self::Transport { message, .. } => f.write_str(message),
            Self::Status {
                status, message, ..
            } => write!(f, "HTTP {status}: {message}"),
        }
    }
}

/// A request built and checked, not yet sent ([`Client::prepare`], [`Prepared::run`]).
pub struct Prepared {
    command: Command,
    stdin: Option<Secret>,
    scratch: VendorScratch,
    spawn: Arc<dyn Spawn>,
}

impl Prepared {
    /// curl's full argv, the program first: what a process listing would show.
    #[must_use]
    pub fn argv(&self) -> Vec<String> {
        std::iter::once(self.command.get_program())
            .chain(self.command.get_args())
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    /// Send it.
    /// # Errors
    /// [`RestError::Local`] when the spawn was interrupted before curl started ([`not_started`]);
    /// [`RestError::Transport`] when curl could not otherwise be started.
    pub fn run(mut self) -> Result<Exchange, RestError> {
        let out = self
            .spawn
            .run(&mut self.command, self.stdin.as_ref().map(Secret::expose))
            .map_err(not_started)?;
        let code = String::from_utf8_lossy(&out.stdout);
        let code = code.trim();
        let status = (!code.is_empty() && code.bytes().all(|b| b.is_ascii_digit()))
            .then(|| code.parse::<u16>().ok())
            .flatten()
            .filter(|status| *status != 0);
        let headers = read_capped(&self.scratch.0.join("headers"), HEADERS_CAP)
            .map(|dump| last_block(&String::from_utf8_lossy(&dump)))
            .unwrap_or_default();
        let body = read_capped(&self.scratch.0.join("body"), JSON_CAP).unwrap_or_default();
        Ok(Exchange {
            exit: out.status.code(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            status,
            headers,
            body,
        })
    }
}

/// A spawn that did not start curl ([`Spawn`], [`RestError::Local`], [`RestError::Transport`]).
fn not_started(e: std::io::Error) -> RestError {
    if e.kind() == std::io::ErrorKind::Interrupted {
        RestError::Local(format!("curl not started: {e}"))
    } else {
        RestError::Transport {
            exit: None,
            status: None,
            retry_after_secs: None,
            message: format!("cannot start curl: {e}"),
        }
    }
}

/// Record every admitted retry, then observe the shared policy's pause. Unit tests inject the
/// operation and do not spend wall time on the production backoff.
fn wait_before_retry(what: &str, retry: Retry, wait: bool) {
    crate::http::note_retry(format!("anonymous HTTPS read ({what}): {retry}"));
    #[cfg(test)]
    let _ = wait;
    #[cfg(not(test))]
    if wait {
        let pause = if aterm_types::dev_seam!(aterm_types::domain::ENV_DEV_FAST_TIMERS).is_some() {
            retry.pause / 1000
        } else {
            retry.pause
        };
        if !pause.is_zero() {
            std::thread::sleep(pause);
        }
    }
}

/// A regular file's length, or zero when no file exists yet.
fn staged_len(path: &Path) -> Result<u64, RestError> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() => Ok(meta.len()),
        Ok(_) => Err(RestError::Local(format!(
            "the staged GitHub asset {} is not a regular file",
            path.display()
        ))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(RestError::Local(format!(
            "inspect staged GitHub asset {}: {e}",
            path.display()
        ))),
    }
}

/// The finite number of progress-earned resumes for a transfer capped at `cap`: sixteen short
/// advances, then no more than one resume for each whole MiB the completed payload can hold.
const fn progress_resume_bound(cap: u64) -> u64 {
    (SHORT_RESUMES as u64).saturating_add(cap / RESUME_EARNED_AT)
}

const fn progress_earns_resume(moved: u64, short: u32, resumes: u64, cap: u64) -> bool {
    resumes < progress_resume_bound(cap) && (moved >= RESUME_EARNED_AT || short < SHORT_RESUMES)
}

/// At most `cap + 1` bytes of `path`; a file curl never wrote (no answer, or an empty one) is empty.
fn read_capped(path: &Path, cap: u64) -> std::io::Result<Vec<u8>> {
    use std::io::Read as _;
    let (file, meta) = match aterm_uds::regular_file::open(path) {
        Ok(opened) => opened,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut bytes =
        Vec::with_capacity(usize::try_from(meta.len().min(cap).saturating_add(1)).unwrap_or(0));
    file.take(cap.saturating_add(1)).read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// The headers of the last block of a header dump (`100 Continue` writes one before the answer's).
fn last_block(dump: &str) -> Vec<(String, String)> {
    let mut headers = Vec::new();
    for line in dump.lines() {
        let line = line.trim_end_matches('\r');
        if line.starts_with("HTTP/") {
            headers.clear();
            continue;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }
    }
    headers
}

/// The REST lane's curl for `url`, with `args` before it: [`curl_command`]'s `/usr/bin/curl`
/// (`-q` first, so no curlrc is read; `--` before the URL), never negotiating below TLS 1.2
/// (`--tlsv1.2`, the floor GitHub and Firebase hold; macOS's curl 8.7 would otherwise offer
/// TLS 1.0), with every variable that changes what curl reads or trusts ([`VENDOR_ENV_SCRUB`])
/// and the GitHub token's variables removed. The caller adds its stdio. Every GitHub request
/// here is one (the REST API, and the release-asset reader's download route, API route and
/// asset CDN), and so is each of the release's Firebase Hosting requests (`ship site`), whose
/// bearer token rides stdin the same way.
#[must_use]
pub fn curl(args: &[&str], url: &str) -> Command {
    let mut argv: Vec<&str> = Vec::with_capacity(args.len().saturating_add(1));
    argv.push("--tlsv1.2");
    argv.extend_from_slice(args);
    let mut command = curl_command(&argv, url);
    for var in VENDOR_ENV_SCRUB.iter().chain(&TOKEN_ENV) {
        command.env_remove(var);
    }
    command
}

/// `file` as `--upload-file` takes it.
fn upload_path(file: &Path) -> std::path::PathBuf {
    if file.is_absolute() {
        file.to_path_buf()
    } else {
        Path::new(".").join(file)
    }
}

/// GitHub's REST API as one identity: a token, or none (anonymous reads).
pub struct Client {
    origins: Origins,
    token: Option<Token>,
    spawn: Arc<dyn Spawn>,
    wait_for_retry: bool,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("origins", &self.origins)
            .field("token", &self.token.is_some())
            .finish_non_exhaustive()
    }
}

impl Client {
    /// A client of `origins` speaking as `token`, or anonymously.
    #[must_use]
    pub fn new(origins: Origins, token: Option<Token>) -> Client {
        Client {
            origins,
            token,
            spawn: Arc::new(Direct),
            wait_for_retry: true,
        }
    }

    /// The same client, starting curl through `spawn`.
    #[must_use]
    pub fn with_spawn(self, spawn: Arc<dyn Spawn>) -> Client {
        Client { spawn, ..self }
    }

    /// Test/dev seam: record retries but spend no wall time on their pauses.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn without_retry_wait(self) -> Client {
        Client {
            wait_for_retry: false,
            ..self
        }
    }

    /// The origins.
    #[must_use]
    pub const fn origins(&self) -> &Origins {
        &self.origins
    }

    /// Whether it speaks as a token.
    #[must_use]
    pub const fn has_token(&self) -> bool {
        self.token.is_some()
    }

    /// `path` (`/repos/…`) on the api origin.
    #[must_use]
    pub fn api(&self, path: &str) -> String {
        format!("{}{path}", self.origins.api)
    }

    /// `path` on the uploads origin.
    #[must_use]
    pub fn uploads(&self, path: &str) -> String {
        format!("{}{path}", self.origins.uploads)
    }

    /// Build and check `request`.
    /// # Errors
    /// [`RestError::Local`]: a URL off the api and uploads origins, a write without a token, a
    /// scratch directory that could not be made, a path that is not UTF-8, or a JSON body that
    /// could not be written.
    pub fn prepare(&self, request: &Request<'_>) -> Result<Prepared, RestError> {
        let url = request.url;
        if !Origins::on(&self.origins.api, url) && !Origins::on(&self.origins.uploads, url) {
            return Err(RestError::Local(format!(
                "refused a GitHub request off the api and uploads origins: {}",
                origin_of(url)
            )));
        }
        if request.method != Method::Get && self.token.is_none() {
            return Err(RestError::Local(format!(
                "refused a {} without a token: a GitHub write never goes anonymously",
                request.method.name()
            )));
        }
        let scratch = VendorScratch::labelled("github").map_err(RestError::Local)?;
        let path_arg = |path: &Path| -> Result<String, RestError> {
            path.to_str()
                .map(str::to_string)
                .ok_or_else(|| RestError::Local(format!("{} is not UTF-8", path.display())))
        };
        let headers = path_arg(&scratch.0.join("headers"))?;
        let body = path_arg(&scratch.0.join("body"))?;
        let accept = format!("Accept: {}", request.accept);
        let version = format!("X-GitHub-Api-Version: {API_VERSION}");
        let cap = JSON_CAP.to_string();
        let mut args: Vec<String> = [
            "-sS",
            "--globoff",
            "--connect-timeout",
            CONNECT_SECS,
            "-D",
            &headers,
            "-o",
            &body,
            "-w",
            "%{http_code}",
            "--max-filesize",
            &cap,
            "-H",
            &accept,
            "-H",
            &version,
        ]
        .iter()
        .map(|a| (*a).to_string())
        .collect();
        if self.origins.https() {
            args.extend(["--proto", "=https"].map(String::from));
        }
        if request.method != Method::Get {
            args.extend(["-X".to_string(), request.method.name().to_string()]);
        }
        match request.body {
            Body::Empty => args.extend(["--max-time", API_SECS].map(String::from)),
            Body::Json(json) => {
                let file = scratch.0.join("request.json");
                {
                    use std::io::Write as _;
                    let mut opts = std::fs::OpenOptions::new();
                    opts.write(true).create_new(true);
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::OpenOptionsExt as _;
                        opts.mode(0o600)
                            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
                    }
                    opts.open(&file)
                        .and_then(|mut f| f.write_all(json))
                        .map_err(|e| RestError::Local(format!("write the request body: {e}")))?;
                }
                args.extend([
                    "--max-time".to_string(),
                    API_SECS.to_string(),
                    "-H".to_string(),
                    "Content-Type: application/json".to_string(),
                    "--data-binary".to_string(),
                    format!("@{}", path_arg(&file)?),
                ]);
            }
            Body::File(file) => {
                args.extend([
                    "--speed-limit".to_string(),
                    STALL_BYTES.to_string(),
                    "--speed-time".to_string(),
                    STALL_SECS.to_string(),
                    "-H".to_string(),
                    "Content-Type: application/octet-stream".to_string(),
                    "-H".to_string(),
                    "Expect:".to_string(),
                    "--upload-file".to_string(),
                    path_arg(&upload_path(file))?,
                ]);
            }
        }
        if request.fail_with_body {
            args.push("--fail-with-body".to_string());
        }
        if self.token.is_some() {
            args.extend(["-H", "@-"].map(String::from));
        }
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        let command = curl(&argv, url);
        Ok(Prepared {
            command,
            stdin: self.token.as_ref().map(Token::header_line),
            scratch,
            spawn: Arc::clone(&self.spawn),
        })
    }

    /// Send `request`.
    /// # Errors
    /// As [`Client::prepare`] and [`Prepared::run`].
    pub fn exchange(&self, request: &Request<'_>) -> Result<Exchange, RestError> {
        self.prepare(request)?.run()
    }

    /// GET `path` on the api origin, whatever its status.
    /// # Errors
    /// As [`Client::exchange`] and [`Exchange::answer`].
    pub fn get(&self, path: &str) -> Result<Answer, RestError> {
        let url = self.api(path);
        self.exchange(&Request::get(&url))?.answer()
    }

    /// `method` `path` on the api origin with `json` as its body, whatever its status.
    /// # Errors
    /// As [`Client::exchange`] and [`Exchange::answer`].
    pub fn send(
        &self,
        method: Method,
        path: &str,
        json: Option<&Value>,
    ) -> Result<Answer, RestError> {
        let url = self.api(path);
        let bytes = json
            .map(aterm_json::to_vec)
            .transpose()
            .map_err(|e| RestError::Local(format!("serialize the request: {e}")))?;
        let request = Request {
            method,
            url: &url,
            accept: ACCEPT_JSON,
            body: bytes.as_deref().map_or(Body::Empty, Body::Json),
            fail_with_body: false,
        };
        self.exchange(&request)?.answer()
    }

    /// The JSON document at `path` (a 2xx).
    /// # Errors
    /// As [`Client::get`], [`Answer::success`] and [`Answer::json`].
    pub fn read(&self, path: &str) -> Result<Value, RestError> {
        self.get(path)?.success()?.json()
    }

    /// Every item of the listing at `base`, page by page ([`crate::github_pages::all`]).
    /// # Errors
    /// The first page's own failure as it came ([`RestError::Status`] for a 404 among them),
    /// else [`RestError::Malformed`] for an unreadable page or pages that disagree.
    pub fn pages(&self, base: &str, per_page: usize) -> Result<Vec<Value>, RestError> {
        let mut failed: Option<RestError> = None;
        let listed = crate::github_pages::all(&self.origins.api, base, per_page, |endpoint| {
            match self.get(endpoint).and_then(Answer::success) {
                Ok(answer) => {
                    crate::github_pages::page(answer.status, answer.headers, &answer.body)
                }
                Err(e) => {
                    let why = e.to_string();
                    failed = Some(e);
                    Err(why)
                }
            }
        });
        listed.map_err(|why| failed.take().unwrap_or(RestError::Malformed(why)))
    }

    /// POST `file`'s bytes to `url` (an asset upload on the uploads origin), whatever the status.
    /// # Errors
    /// As [`Client::exchange`] and [`Exchange::answer`].
    pub fn upload(&self, url: &str, file: &Path) -> Result<Answer, RestError> {
        let request = Request {
            method: Method::Post,
            url,
            accept: ACCEPT_JSON,
            body: Body::File(file),
            fail_with_body: false,
        };
        self.exchange(&request)?.answer()
    }

    /// Where the asset at `asset_path` (`/repos/o/r/releases/assets/{id}`) is served.
    /// # Errors
    /// As [`Client::exchange`] and [`Exchange::answer`]; [`RestError::Redirect`] for a `Location`
    /// off the CDN (never asked); [`RestError::Malformed`] for a 2xx or a redirect with no
    /// `Location`; [`RestError::Status`] for any other status.
    pub fn asset_location(&self, asset_path: &str) -> Result<String, RestError> {
        let url = self.api(asset_path);
        let request = Request {
            accept: ACCEPT_BYTES,
            ..Request::get(&url)
        };
        let answer = self.exchange(&request)?.answer()?;
        match answer.status {
            301..=303 | 307 | 308 => {
                let location = answer
                    .header("location")
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .ok_or_else(|| {
                        RestError::Malformed("the asset API redirected with no Location".into())
                    })?;
                if self
                    .origins
                    .cdn
                    .iter()
                    .any(|cdn| Origins::on(cdn, location))
                {
                    Ok(location.to_string())
                } else {
                    Err(RestError::Redirect(format!(
                        "the asset API redirected off the asset CDN, to {}; refused, never asked",
                        origin_of(location)
                    )))
                }
            }
            200..=299 => Err(RestError::Malformed(
                "the asset API answered the bytes itself, not a redirect to the asset CDN".into(),
            )),
            status => Err(RestError::Status {
                status,
                retry_after_secs: answer.retry_after_secs(),
                message: answer.message(),
            }),
        }
    }

    /// One status-reporting, redirect-refusing CDN transfer into `staged`. `offset` resumes a
    /// partial transfer and `remaining_cap` bounds only the response remainder, keeping the
    /// completed file within the caller's total cap.
    fn cdn_download_attempt(
        &self,
        url: &str,
        staged: &Path,
        headers: &Path,
        remaining_cap: u64,
        offset: u64,
    ) -> Result<(), RestError> {
        let path_arg = |path: &Path| {
            path.to_str()
                .map(str::to_string)
                .ok_or_else(|| RestError::Local(format!("{} is not UTF-8", path.display())))
        };
        let staged_arg = path_arg(staged)?;
        let headers_arg = path_arg(headers)?;
        let total_cap = remaining_cap.saturating_add(offset);
        let cap = remaining_cap.to_string();
        let offset = offset.to_string();
        let _ = std::fs::remove_file(headers);
        let mut args = vec![
            "-sS".to_string(),
            "--globoff".to_string(),
            "--fail".to_string(),
            "-L".to_string(),
            "--max-redirs".to_string(),
            "0".to_string(),
            "--connect-timeout".to_string(),
            CONNECT_SECS.to_string(),
            "--speed-limit".to_string(),
            STALL_BYTES.to_string(),
            "--speed-time".to_string(),
            STALL_SECS.to_string(),
            "-H".to_string(),
            "Accept: application/octet-stream".to_string(),
            "-D".to_string(),
            headers_arg,
            "-o".to_string(),
            staged_arg,
            "-w".to_string(),
            "%{http_code}".to_string(),
            "--max-filesize".to_string(),
            cap,
        ];
        if offset != "0" {
            args.extend(["--continue-at".to_string(), offset]);
        }
        if self.origins.https() {
            args.extend(["--proto", "=https", "--proto-redir", "=https"].map(String::from));
        }
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = self
            .spawn
            .run(&mut curl(&argv, url), None)
            .map_err(not_started)?;
        let status = String::from_utf8_lossy(&out.stdout);
        let status = status
            .trim()
            .parse::<u16>()
            .ok()
            .filter(|status| *status != 0);
        let headers = read_capped(headers, HEADERS_CAP)
            .map(|dump| last_block(&String::from_utf8_lossy(&dump)))
            .map_err(|e| RestError::Local(format!("read the asset CDN's headers: {e}")))?;
        let retry_after_secs = retry_after_header(&headers, "retry-after");
        if out.status.success() {
            return match status {
                Some(200..=299) => Ok(()),
                Some(status) => Err(RestError::Status {
                    status,
                    retry_after_secs,
                    message: format!("the asset CDN answered HTTP {status}"),
                }),
                None => Err(RestError::Malformed(
                    "the asset CDN answered no readable HTTP status".into(),
                )),
            };
        }
        Err(cdn_failure(
            out.status.code(),
            status,
            retry_after_secs,
            &String::from_utf8_lossy(&out.stderr),
            total_cap,
        ))
    }

    /// Download the asset at `asset_path` into `dest`, at most `cap` bytes. The API location
    /// and the anonymous CDN transfer both use the shared retry policy. Bytes stay in a private
    /// sibling staging directory and are atomically renamed onto `dest` only after a complete
    /// transfer. A transient failure after progress resumes at that byte; sixteen short advances
    /// plus one for every whole MiB under `cap` bound progress-earned attempts, while failures
    /// without progress use [`Policy::STANDARD`].
    /// # Errors
    /// As [`Client::asset_location`]; [`RestError::Local`] for a non-UTF-8 `dest`; then
    /// [`RestError::Malformed`] over the cap, [`RestError::Status`] for the CDN's HTTP error,
    /// [`RestError::Redirect`] for a CDN that redirects again, and [`RestError::Transport`]
    /// otherwise.
    pub fn download_asset(&self, asset_path: &str, dest: &Path, cap: u64) -> Result<(), RestError> {
        self.download_asset_via(
            asset_path,
            dest,
            cap,
            |location, staged, headers, remaining_cap, offset| {
                self.cdn_download_attempt(location, staged, headers, remaining_cap, offset)
            },
        )
    }

    /// [`Client::download_asset`] with its CDN transfer injected. The production method and the
    /// negative controls exercise this same state machine.
    fn download_asset_via(
        &self,
        asset_path: &str,
        dest: &Path,
        cap: u64,
        mut transfer: impl FnMut(&str, &Path, &Path, u64, u64) -> Result<(), RestError>,
    ) -> Result<(), RestError> {
        if cap == 0 {
            return Err(RestError::Local(
                "refused a zero-byte GitHub asset download cap".into(),
            ));
        }
        if dest.to_str().is_none() {
            return Err(RestError::Local(format!("{} is not UTF-8", dest.display())));
        }
        let parent = dest
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let scratch = aterm_tempfile::Builder::new()
            .prefix(".aterm-github-asset-")
            .tempdir_in(parent)
            .map_err(|e| {
                RestError::Local(format!(
                    "make a sibling staging directory for {}: {e}",
                    dest.display()
                ))
            })?;
        let staged = scratch.path().join("asset");
        let headers = scratch.path().join("headers");

        let mut idle_failures = 0_u32;
        let mut idle_budget = Budget::new(Policy::STANDARD);
        let mut progress_resumes = 0_u64;
        let mut short_resumes = 0_u32;
        let mut restarted_fresh = false;
        loop {
            let mut before = staged_len(&staged)?;
            if before >= cap {
                std::fs::remove_file(&staged).map_err(|e| {
                    RestError::Local(format!(
                        "reset full partial GitHub asset {}: {e}",
                        staged.display()
                    ))
                })?;
                before = 0;
            }
            let location = match self.asset_location(asset_path) {
                Ok(location) => location,
                Err(error) => {
                    let Some(cause) = error.retry_cause() else {
                        return Err(error);
                    };
                    idle_failures = idle_failures.saturating_add(1);
                    match idle_budget.after(idle_failures, cause) {
                        Decision::Retry(retry) => {
                            wait_before_retry("GitHub asset location", retry, self.wait_for_retry);
                            continue;
                        }
                        Decision::Exhausted => {
                            return Err(error.after_bound(&idle_budget.bound()));
                        }
                    }
                }
            };
            let remaining_cap = cap.saturating_sub(before);
            match transfer(&location, &staged, &headers, remaining_cap, before) {
                Ok(()) => {
                    let size = staged_len(&staged)?;
                    if size > cap {
                        return Err(RestError::Malformed(format!(
                            "the asset exceeds its {cap}-byte cap"
                        )));
                    }
                    return std::fs::rename(&staged, dest).map_err(|e| {
                        RestError::Local(format!(
                            "place downloaded GitHub asset at {}: {e}",
                            dest.display()
                        ))
                    });
                }
                Err(error) => {
                    let mut after = staged_len(&staged)?;
                    if before > 0 && !restarted_fresh && error.resume_refused() {
                        std::fs::remove_file(&staged).map_err(|e| {
                            RestError::Local(format!(
                                "restart a GitHub asset whose CDN refused Range: {e}"
                            ))
                        })?;
                        restarted_fresh = true;
                        idle_failures = 0;
                        idle_budget = Budget::new(Policy::STANDARD);
                        crate::http::note_retry(format!(
                            "anonymous HTTPS read (GitHub asset CDN): the CDN refused a Range \
                             resume after {before} bytes — retrying once from byte zero"
                        ));
                        continue;
                    }
                    if error.status().is_some_and(|status| status >= 400) && after > before {
                        if before == 0 {
                            let _ = std::fs::remove_file(&staged);
                        } else {
                            std::fs::OpenOptions::new()
                                .write(true)
                                .open(&staged)
                                .and_then(|file| file.set_len(before))
                                .map_err(|e| {
                                    RestError::Local(format!(
                                        "discard the asset CDN's error body: {e}"
                                    ))
                                })?;
                        }
                        after = before;
                    }
                    if after > cap {
                        return Err(RestError::Malformed(format!(
                            "the asset exceeds its {cap}-byte cap"
                        )));
                    }
                    let Some(cause) = error.retry_cause() else {
                        return Err(error);
                    };
                    let moved = after.saturating_sub(before);
                    if moved > 0 && after < cap {
                        if !progress_earns_resume(moved, short_resumes, progress_resumes, cap) {
                            let bound =
                                format!("{} progress-earned resumes", progress_resume_bound(cap));
                            return Err(error.after_bound(&bound));
                        }
                        progress_resumes = progress_resumes.saturating_add(1);
                        if moved < RESUME_EARNED_AT {
                            short_resumes = short_resumes.saturating_add(1);
                        }
                        crate::http::note_retry(format!(
                            "anonymous HTTPS read (GitHub asset CDN): attempt made {moved} bytes \
                             of progress, then failed ({cause}) — resuming ({progress_resumes}/{})",
                            progress_resume_bound(cap)
                        ));
                        idle_failures = 0;
                        idle_budget = Budget::new(Policy::STANDARD);
                        continue;
                    }
                    if after >= cap {
                        let _ = std::fs::remove_file(&staged);
                    }
                    idle_failures = idle_failures.saturating_add(1);
                    match idle_budget.after(idle_failures, cause) {
                        Decision::Retry(retry) => {
                            wait_before_retry("GitHub asset CDN", retry, self.wait_for_retry);
                        }
                        Decision::Exhausted => {
                            return Err(error.after_bound(&idle_budget.bound()));
                        }
                    }
                }
            }
        }
    }
}

/// What a failed CDN curl means.
fn cdn_failure(
    exit: Option<i32>,
    status: Option<u16>,
    retry_after_secs: Option<u64>,
    stderr: &str,
    cap: u64,
) -> RestError {
    let said = stderr.trim();
    let answered_status = status.or_else(|| {
        said.rsplit_once("returned error: ")
            .and_then(|(_, tail)| tail.get(..3))
            .filter(|code| code.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|code| code.parse().ok())
    });
    if let Some(status @ 400..=599) = answered_status {
        return RestError::Status {
            status,
            retry_after_secs,
            message: format!("the asset CDN refused it ({said})"),
        };
    }
    match exit {
        Some(47) => {
            RestError::Redirect(format!("the asset CDN redirected again; refused ({said})"))
        }
        Some(63) => RestError::Malformed(format!("the asset exceeds its {cap}-byte cap")),
        _ if said.contains("Maximum file size exceeded") => {
            RestError::Malformed(format!("the asset exceeds its {cap}-byte cap"))
        }
        Some(22) => RestError::Status {
            status: answered_status.unwrap_or(0),
            retry_after_secs,
            message: format!("the asset CDN refused it ({said})"),
        },
        _ => RestError::Transport {
            exit,
            status,
            retry_after_secs,
            message: format!(
                "the asset CDN download failed ({}): {said}",
                exit.map_or_else(|| "ended by a signal".to_string(), |c| format!("exit {c}"))
            ),
        },
    }
}

#[cfg(test)]
#[path = "github_rest_tests.rs"]
mod tests;
