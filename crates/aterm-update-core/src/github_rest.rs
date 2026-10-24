// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! GitHub REST API client over [`crate::http`] (`curl`) with stdin-piped [`Token::new`],
//! [`RestError::Local`] preflight, [`RestError::Redirect`] policy ([`Client::asset_location`]),
//! and [`crate::github_pages`] pagination.

use std::io::Write as _;
use std::path::Path;
use std::sync::Arc;

use aterm_json::Value;
use aterm_uds::secret::Secret;
use aterm_uds::spawnfd::{Child, Command, Stdio};

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
    /// A file that cannot be read, or one whose token [`Token::new`] refuses.
    pub fn from_file(path: &Path) -> Result<Option<Token>, String> {
        let mut bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(format!(
                    "cannot read the token file {}: {e}",
                    path.display()
                ));
            }
        };
        let token = if bytes.trim_ascii().is_empty() {
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
        if self.exit == Some(63) || self.stderr.contains("Maximum file size exceeded") {
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
            (exit, _) => Err(RestError::Transport(format!(
                "curl reached no answer ({}): {}",
                exit.map_or_else(|| "ended by a signal".to_string(), |c| format!("exit {c}")),
                self.stderr.trim()
            ))),
        }
    }
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
                self.header("location").map_or("no Location", origin_of)
            ))),
            status => Err(RestError::Status {
                status,
                message: self.message(),
            }),
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

/// The scheme and authority of `url`, never its path or query (a CDN URL's signature).
fn origin_of(url: &str) -> &str {
    let after_scheme = url.find("://").map_or(0, |i| i + 3);
    let end = url[after_scheme..]
        .find(['/', '?', '#'])
        .map_or(url.len(), |i| after_scheme + i);
    &url[..end]
}

/// Why a call failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RestError {
    /// Refused here before curl ran.
    Local(String),
    /// curl reached no answer.
    Transport(String),
    /// GitHub answered a status the call does not take, in its own words.
    Status {
        /// The status.
        status: u16,
        /// GitHub's message ([`Answer::message`]).
        message: String,
    },
    /// A redirect refused.
    Redirect(String),
    /// An answer that is not what the call reads.
    Malformed(String),
}

impl RestError {
    /// The HTTP status GitHub answered, for [`RestError::Status`].
    #[must_use]
    pub const fn status(&self) -> Option<u16> {
        match self {
            Self::Status { status, .. } => Some(*status),
            _ => None,
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
            Self::Local(m) | Self::Transport(m) | Self::Redirect(m) | Self::Malformed(m) => {
                f.write_str(m)
            }
            Self::Status { status, message } => write!(f, "HTTP {status}: {message}"),
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
        let status = code
            .trim()
            .parse::<u16>()
            .ok()
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
        RestError::Transport(format!("cannot start curl: {e}"))
    }
}

/// At most `cap` bytes of `path`; a file curl never wrote (no answer, or an empty one) is empty.
fn read_capped(path: &Path, cap: u64) -> std::io::Result<Vec<u8>> {
    use std::io::Read as _;
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut bytes = Vec::new();
    file.take(cap).read_to_end(&mut bytes)?;
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
/// (`-q` first, so no curlrc is read; `--` before the URL), with every variable that changes
/// what curl reads or trusts ([`VENDOR_ENV_SCRUB`]) and the GitHub token's variables removed.
/// The caller adds its stdio. Every GitHub request here is one, and so is each of the release's
/// Firebase Hosting requests (`ship site`), whose bearer token rides stdin the same way.
#[must_use]
pub fn curl(args: &[&str], url: &str) -> Command {
    let mut command = curl_command(args, url);
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
        }
    }

    /// The same client, starting curl through `spawn`.
    #[must_use]
    pub fn with_spawn(self, spawn: Arc<dyn Spawn>) -> Client {
        Client { spawn, ..self }
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
                std::fs::write(&file, json)
                    .map_err(|e| RestError::Local(format!("write the request body: {e}")))?;
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
                message: answer.message(),
            }),
        }
    }

    /// The anonymous CDN curl for `url`.
    fn cdn_command(&self, url: &str, to: Option<(&str, u64)>) -> Command {
        let cap = to.map(|(_, cap)| cap.to_string());
        let mut args = vec![
            "-sS",
            "--globoff",
            "--fail",
            "-L",
            "--max-redirs",
            "0",
            "--connect-timeout",
            CONNECT_SECS,
            "--speed-limit",
            STALL_BYTES,
            "--speed-time",
            STALL_SECS,
            "-H",
            "Accept: application/octet-stream",
        ];
        if self.origins.https() {
            args.extend(["--proto", "=https", "--proto-redir", "=https"]);
        }
        if let (Some((dest, _)), Some(cap)) = (to, cap.as_deref()) {
            args.extend(["-o", dest, "--max-filesize", cap]);
        }
        curl(&args, url)
    }

    /// The bytes of the asset at `asset_path`, streaming.
    /// # Errors
    /// As [`Client::asset_location`]; [`RestError::Transport`] when curl could not be started.
    pub fn open_asset(&self, asset_path: &str) -> Result<Child, RestError> {
        let location = self.asset_location(asset_path)?;
        self.cdn_command(&location, None)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| RestError::Transport(format!("cannot start curl: {e}")))
    }

    /// Download the asset at `asset_path` into `dest`, at most `cap` bytes.
    /// # Errors
    /// As [`Client::asset_location`]; [`RestError::Local`] for a non-UTF-8 `dest`; then
    /// [`RestError::Malformed`] over the cap, [`RestError::Status`] for the CDN's HTTP error,
    /// [`RestError::Redirect`] for a CDN that redirects again, and [`RestError::Transport`]
    /// otherwise.
    pub fn download_asset(&self, asset_path: &str, dest: &Path, cap: u64) -> Result<(), RestError> {
        let location = self.asset_location(asset_path)?;
        let dest_arg = dest
            .to_str()
            .ok_or_else(|| RestError::Local(format!("{} is not UTF-8", dest.display())))?;
        let out = self
            .spawn
            .run(
                &mut self.cdn_command(&location, Some((dest_arg, cap))),
                None,
            )
            .map_err(|e| RestError::Transport(format!("cannot start curl: {e}")));
        let failure = match out {
            Err(e) => e,
            Ok(out) if out.status.success() => return Ok(()),
            Ok(out) => cdn_failure(
                out.status.code(),
                &String::from_utf8_lossy(&out.stderr),
                cap,
            ),
        };
        let _ = std::fs::remove_file(dest);
        Err(failure)
    }
}

/// What a failed CDN curl means.
fn cdn_failure(exit: Option<i32>, stderr: &str, cap: u64) -> RestError {
    let said = stderr.trim();
    match exit {
        Some(63) => RestError::Malformed(format!("the asset exceeds its {cap}-byte cap")),
        _ if said.contains("Maximum file size exceeded") => {
            RestError::Malformed(format!("the asset exceeds its {cap}-byte cap"))
        }
        Some(22) => RestError::Status {
            status: said
                .rsplit_once("returned error: ")
                .and_then(|(_, tail)| tail.get(..3))
                .and_then(|code| code.parse().ok())
                .unwrap_or(0),
            message: format!("the asset CDN refused it ({said})"),
        },
        Some(47) => {
            RestError::Redirect(format!("the asset CDN redirected again; refused ({said})"))
        }
        _ => RestError::Transport(format!(
            "the asset CDN download failed ({}): {said}",
            exit.map_or_else(|| "ended by a signal".to_string(), |c| format!("exit {c}"))
        )),
    }
}

#[cfg(test)]
#[path = "github_rest_tests.rs"]
mod tests;
