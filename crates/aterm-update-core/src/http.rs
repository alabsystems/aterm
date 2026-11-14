// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Outbound HTTP requests for updater checks and downloads.

use aterm_uds::spawnfd::Command;
use std::path::Path;

/// A classified transport failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpError {
    /// Curl itself failed (exit != 0).
    Transport(String),
    /// The `-w`-appended status trailer was not a number.
    Malformed(String),
    /// An HTTP status the vendor lane does not accept.
    VendorStatus { code: u16, url: String },
    /// A vendor-lane verdict about the request or the document, returned on the first attempt.
    VendorRefused(String),
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(message) | Self::Malformed(message) | Self::VendorRefused(message) => {
                f.write_str(message)
            }
            Self::VendorStatus { code, url } => {
                write!(f, "the vendor host answered HTTP {code} for {url}")
            }
        }
    }
}

/// The `curl` binary: the absolute `/usr/bin/curl` on unix, so a `PATH` shim cannot stand in.
#[cfg(not(windows))]
pub(crate) const CURL_BIN: &str = "/usr/bin/curl";

#[cfg(windows)]
pub(crate) const CURL_BIN: &str = "curl.exe";

/// `ATERM_DEV_CURL`.
pub const CURL_SEAM: &str = "ATERM_DEV_CURL";

/// The curl every lane spawns: [`CURL_BIN`], or a development build's [`CURL_SEAM`].
fn curl_bin() -> std::ffi::OsString {
    aterm_types::dev_seam!(CURL_SEAM)
        .filter(|seam| !seam.is_empty())
        .unwrap_or_else(|| CURL_BIN.into())
}

/// Builds curl's parameter list with `-q`, caller options, User-Agent, and `--` before `url`.
#[cfg_attr(trust_verify, trust::skip)]
fn curl_argv(args: &[&str], url: &str) -> Vec<String> {
    let mut v = Vec::with_capacity(args.len().min(32) + 7);
    v.push("-q".to_string());
    v.extend(args.iter().map(|s| (*s).to_string()));
    v.extend(
        ["-H", "User-Agent: aterm-update"]
            .iter()
            .map(|s| (*s).to_string()),
    );
    v.push("--".to_string());
    v.push(url.to_string());
    v
}

/// Builds the argv and environment of the curl process for `url`.
pub(crate) fn curl_command(args: &[&str], url: &str) -> Command {
    let mut command = aterm_uds::spawnfd::command(curl_bin());
    command
        .args(curl_argv(args, url))
        .env_remove("CURL_HOME")
        .env_remove("XDG_CONFIG_HOME");
    command
}

/// Runs curl anonymously against `url` with `args` and `/dev/null` for stdin.
#[cfg_attr(trust_verify, trust::skip)]
fn curl_fetch(args: &[&str], url: &str) -> Result<std::process::Output, String> {
    use aterm_uds::spawnfd::Stdio;
    let mut cmd = curl_command(args, url);
    cmd.stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());
    crate::run_operation_command(&mut cmd).map_err(|e| format!("spawn curl: {e}"))
}

/// Runs one prepared anonymous curl attempt through `spawn`.
///
/// The shared downloader still owns the command, response classification, retry budget, and
/// progress accounting. This seam lets an embedding operation retain its child-lifetime contract
/// and lets tests inject transport answers without replacing the policy.
#[cfg_attr(trust_verify, trust::skip)]
fn curl_fetch_via(
    args: &[&str],
    url: &str,
    spawn: &dyn crate::github_rest::Spawn,
) -> Result<std::process::Output, String> {
    let mut cmd = curl_command(args, url);
    spawn
        .run(&mut cmd, None)
        .map_err(|e| format!("spawn curl: {e}"))
}

/// Number of attempts admitted by the shared anonymous-read policy.
#[cfg(all(test, unix))]
const READ_ATTEMPTS: u32 = aterm_types::http_retry::Policy::STANDARD.attempts();

/// Curl writes this response metadata to stderr after its own diagnostic. Keeping it off stdout
/// leaves small response bodies byte-exact, and captures the status even when macOS curl 8.7.1
/// exits 56 for an HTTP/2 5xx instead of 22.
const CURL_META_PREFIX: &str = "aterm-http-meta:";
const CURL_META_WRITE_OUT: &str = "%{stderr}\naterm-http-meta:%{http_code}\t%header{retry-after}\n";

/// Timestamp when a request in this process last spent its attempts on an unreachable link.
static LINK_DOWN_SINCE: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);

/// What an attempt says of the link ([`link_after`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(test)]
enum Link {
    /// It reached no host, with nothing left to try.
    Down,
    /// A host answered, whatever it said.
    Reached,
    /// Indeterminate reachability.
    Unknown,
}

/// TLS handshake progress reported by a `-w` trailer's `%{time_appconnect}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Handshake {
    /// The TLS handshake finished.
    Done,
    /// The TLS handshake did not finish.
    NotDone,
    /// The run carries no trailer that says (a byte lane, whose stdout is the body).
    Unknown,
}

/// Parses handshake progress from the last token (`%{time_appconnect}`) of `trailer`.
pub(crate) fn handshake_in(trailer: &str) -> Handshake {
    match trailer
        .split_whitespace()
        .last()
        .filter(|t| !t.starts_with('+'))
        .and_then(|t| t.parse::<f64>().ok())
    {
        Some(secs) if secs > 0.0 => Handshake::Done,
        Some(0.0) => Handshake::NotDone,
        _ => Handshake::Unknown,
    }
}

/// Returns whether curl's `exit` and `handshake` indicate that no host was reached.
pub(crate) fn reached_no_host(exit: Option<i32>, handshake: Handshake) -> bool {
    matches!(exit, Some(5..=7)) || (exit == Some(28) && handshake == Handshake::NotDone)
}

/// Classifies link reachability and whether another attempt should run.
#[cfg(test)]
fn link_after(
    exit: Option<i32>,
    handshake: Handshake,
    attempt: u32,
    attempts: u32,
    known_down: bool,
) -> (Link, bool) {
    let last = attempt >= attempts;
    if reached_no_host(exit, handshake) {
        let down = known_down || (last && attempts > 1);
        (
            if down { Link::Down } else { Link::Unknown },
            !last && !known_down,
        )
    } else if matches!(exit, Some(0 | 22)) {
        (Link::Reached, !last)
    } else {
        (Link::Unknown, !last)
    }
}

/// Hold the link down from now ([`LINK_DOWN_SINCE`]).
#[cfg_attr(trust_verify, trust::skip)]
pub fn note_link_down() {
    *LINK_DOWN_SINCE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(std::time::Instant::now());
}

/// Make an admitted anonymous-read retry visible even when the embedding process has not
/// installed a logger. Cutter and verifier are standalone tools, so their stderr is the one
/// observation channel every production invocation owns.
pub(crate) fn note_retry(message: impl std::fmt::Display) {
    let message = message.to_string();
    let _ = write_retry_line(&mut std::io::stderr().lock(), &message);
    aterm_log::warn!("{message}");
}

fn write_retry_line(writer: &mut impl std::io::Write, message: &str) -> std::io::Result<()> {
    writeln!(writer, "{message}")
}

/// Log and wait for one retry admitted by the shared anonymous-read policy.
fn wait_before_retry(retry: aterm_types::http_retry::Retry) {
    wait_before_retry_via(retry, true);
}

/// The retry wait, with the clock suppressed beside an injected child transport.
fn wait_before_retry_via(retry: aterm_types::http_retry::Retry, wait: bool) {
    note_retry(format!("anonymous HTTPS read: {retry}"));
    let pause = if aterm_types::dev_seam!(aterm_types::domain::ENV_DEV_FAST_TIMERS).is_some() {
        retry.pause / 1000
    } else {
        retry.pause
    };
    if wait && !pause.is_zero() {
        std::thread::sleep(pause);
    }
}

/// HTTP status and optional `Location` header from a redirect-refusing HEAD request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadAnswer {
    /// The HTTP status of the first hop (no redirect is followed).
    pub code: u16,
    /// The `Location` header of that hop, trimmed, if present.
    pub location: Option<String>,
}

/// Option list for [`head_no_redirect`].
const fn head_args() -> [&'static str; 10] {
    head_args_with_timeout("30")
}

const fn head_args_with_timeout(timeout_secs: &str) -> [&str; 10] {
    [
        "-sS",
        "-I",
        "--max-redirs",
        "0",
        "--connect-timeout",
        SMALL_CONNECT_SECS,
        "--max-time",
        timeout_secs,
        "-w",
        "\n%{http_code} %{time_appconnect}",
    ]
}

/// The connect bound (DNS, TCP and TLS) of a HEAD or a small-document GET, in seconds.
const SMALL_CONNECT_SECS: &str = "5";

/// The status and `Location` of one hop of `url`, anonymously, without following it.
#[cfg_attr(trust_verify, trust::skip)]
pub fn head_no_redirect(url: &str) -> Result<HeadAnswer, HttpError> {
    head_no_redirect_with(url, &head_args(), aterm_types::http_retry::Policy::STANDARD)
}

/// A HEAD for a background discovery hint.
#[cfg_attr(trust_verify, trust::skip)]
pub fn head_no_redirect_quick(url: &str) -> Result<HeadAnswer, HttpError> {
    head_no_redirect_with(
        url,
        &head_args_with_timeout("5"),
        aterm_types::http_retry::Policy::NONE,
    )
}

/// One optional HEAD, with no retry and only the time remaining until `deadline`.
/// An expired deadline refuses before spawning curl. Multiple probes can share this deadline.
/// # Errors
/// A non-HTTPS URL, expired deadline, transport failure or exhausted one-attempt policy.
#[cfg_attr(trust_verify, trust::skip)]
pub fn head_no_redirect_until(
    url: &str,
    deadline: std::time::Instant,
) -> Result<HeadAnswer, HttpError> {
    require_https_url(url).map_err(HttpError::Transport)?;
    head_until(url, deadline)
}

fn head_remaining(
    deadline: std::time::Instant,
    now: std::time::Instant,
) -> Result<String, HttpError> {
    // curl treats zero as unlimited. Round down to milliseconds and refuse a sub-ms remainder.
    let millis = deadline.saturating_duration_since(now).as_millis();
    if millis == 0 {
        return Err(HttpError::Transport(
            "optional HEAD deadline exhausted".into(),
        ));
    }
    Ok(format!("{}.{:03}", millis / 1000, millis % 1000))
}

#[cfg_attr(trust_verify, trust::skip)]
fn head_until(url: &str, deadline: std::time::Instant) -> Result<HeadAnswer, HttpError> {
    let remaining = head_remaining(deadline, std::time::Instant::now())?;
    head_attempts(
        url,
        &head_args_with_timeout(&remaining),
        1,
        aterm_types::http_retry::Budget::new(aterm_types::http_retry::Policy::NONE),
    )
}

#[cfg_attr(trust_verify, trust::skip)]
fn head_no_redirect_with(
    url: &str,
    args: &[&str],
    policy: aterm_types::http_retry::Policy,
) -> Result<HeadAnswer, HttpError> {
    require_https_url(url).map_err(HttpError::Transport)?;
    head_attempts(url, args, 1, aterm_types::http_retry::Budget::new(policy))
}

/// Outcome of the first attempt of [`head_no_redirect`], separated from its retries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FirstHead {
    /// Settled on that attempt.
    Settled(Result<HeadAnswer, HttpError>),
    /// Another attempt may change it.
    Again {
        /// The attempt's own answer.
        answer: Result<HeadAnswer, HttpError>,
        /// It reached no host.
        unreached: bool,
        /// Why the shared policy permits another attempt.
        cause: aterm_types::http_retry::Cause,
    },
}

/// [`head_no_redirect`]'s first attempt alone.
#[cfg_attr(trust_verify, trust::skip)]
pub fn head_no_redirect_first(url: &str) -> FirstHead {
    match require_https_url(url) {
        Ok(()) => head_attempt(url, &head_args()),
        Err(e) => FirstHead::Settled(Err(HttpError::Transport(e))),
    }
}

/// [`head_no_redirect`]'s attempts after the first, each after its backoff.
#[cfg_attr(trust_verify, trust::skip)]
pub fn head_no_redirect_again(
    url: &str,
    first_cause: aterm_types::http_retry::Cause,
) -> Result<HeadAnswer, HttpError> {
    require_https_url(url).map_err(HttpError::Transport)?;
    let mut budget =
        aterm_types::http_retry::Budget::new(aterm_types::http_retry::Policy::STANDARD);
    match budget.after(1, first_cause) {
        aterm_types::http_retry::Decision::Retry(retry) => wait_before_retry(retry),
        aterm_types::http_retry::Decision::Exhausted => {
            return Err(head_exhausted(url, budget.bound(), None));
        }
    }
    head_attempts(url, &head_args(), 2, budget)
}

/// Attempts a HEAD of `url` until it settles or the shared budget is exhausted.
#[cfg_attr(trust_verify, trust::skip)]
fn head_attempts(
    url: &str,
    args: &[&str],
    from: u32,
    mut budget: aterm_types::http_retry::Budget,
) -> Result<HeadAnswer, HttpError> {
    let mut attempt = from;
    loop {
        match head_attempt(url, args) {
            FirstHead::Settled(answer) => return answer,
            FirstHead::Again { answer, cause, .. } => match budget.after(attempt, cause) {
                aterm_types::http_retry::Decision::Retry(retry) => {
                    wait_before_retry(retry);
                    attempt = attempt.saturating_add(1);
                }
                aterm_types::http_retry::Decision::Exhausted => {
                    return Err(head_exhausted(url, budget.bound(), Some(&answer)));
                }
            },
        }
    }
}

fn head_exhausted(
    url: &str,
    bound: String,
    answer: Option<&Result<HeadAnswer, HttpError>>,
) -> HttpError {
    let last = match answer {
        Some(Ok(answer)) => format!("last answer HTTP {}", answer.code),
        Some(Err(error)) => format!("last failure: {error}"),
        None => "the server-requested delay was outside the bound".to_string(),
    };
    HttpError::Transport(format!(
        "anonymous HEAD {url} stopped after {bound}: {last}"
    ))
}

/// Runs one HEAD request and classifies its answer through the shared policy.
#[cfg_attr(trust_verify, trust::skip)]
fn head_attempt(url: &str, args: &[&str]) -> FirstHead {
    let out = match curl_fetch(args, url) {
        Ok(out) => out,
        Err(e) => return FirstHead::Settled(Err(HttpError::Transport(e))),
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let (headers, trailer) = match text.rfind('\n') {
        Some(i) => (&text[..i], text[i + 1..].trim()),
        None => ("", text.trim()),
    };
    let handshake = handshake_in(trailer);
    let exit = out.status.code();
    let code = trailer.split_whitespace().next().unwrap_or("");
    let numeric = (!code.is_empty() && code.bytes().all(|b| b.is_ascii_digit()))
        .then(|| code.parse::<u16>().ok())
        .flatten()
        .filter(|status| *status != 0);
    let retry_after = last_block_header(headers, "retry-after").and_then(|value| {
        aterm_types::http_retry::retry_after_delay_secs(&value, aterm_time::unix_secs())
    });
    let cause = aterm_types::http_retry::Cause::from_curl(exit, numeric, retry_after);
    let answer = if !out.status.success() {
        Err(HttpError::Transport(format!(
            "curl HEAD {} failed ({}): {}",
            url,
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    } else if let Some(numeric) = numeric {
        Ok(HeadAnswer {
            code: numeric,
            location: location_header(headers),
        })
    } else {
        Err(HttpError::Malformed(format!(
            "the release host answered HTTP {code} to HEAD {url}"
        )))
    };
    if let Some(cause) = cause {
        FirstHead::Again {
            answer,
            // Timeouts are explicitly transient even when the TLS handshake never finished.
            // DNS/proxy/connect failures are not classified as retryable and settle above.
            unreached: reached_no_host(exit, handshake)
                && !matches!(cause, aterm_types::http_retry::Cause::Timeout),
            cause,
        }
    } else {
        FirstHead::Settled(answer)
    }
}

/// The last `Location:` header in a header capture, trimmed; `None` when absent or empty.
pub(crate) fn location_header(headers: &str) -> Option<String> {
    let mut found: Option<String> = None;
    for line in headers.lines() {
        if line.starts_with("HTTP/") {
            found = None;
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case("location") {
            let value = value.trim();
            found = (!value.is_empty()).then(|| value.to_string());
        }
    }
    found
}

/// Rejects any asset URL that does not use the `https://` scheme.
fn require_https_url(url: &str) -> Result<(), String> {
    if url.starts_with("https://") {
        Ok(())
    } else {
        Err(format!("refusing asset URL with non-https scheme: {url}"))
    }
}

/// Option list for [`download_bytes`].
fn download_bytes_args<'a>(
    cap: &'a str,
    accept_header: &'a str,
    extra_header: Option<&'a str>,
) -> Vec<&'a str> {
    let mut args = vec![
        "-fsSL",
        "--proto-redir",
        "=https",
        "--connect-timeout",
        SMALL_CONNECT_SECS,
        "--max-time",
        "60",
        "--max-filesize",
        cap,
        "-H",
        accept_header,
        "--write-out",
        CURL_META_WRITE_OUT,
    ];
    if let Some(header) = extra_header {
        args.extend(["-H", header]);
    }
    args
}

/// Download a small asset's bytes (e.g. a manifest) into memory, capped at `max_filesize`.
#[cfg_attr(trust_verify, trust::skip)]
pub fn download_bytes(asset_url: &str, max_filesize: u64) -> Result<Vec<u8>, String> {
    download_bytes_with_headers(
        asset_url,
        max_filesize,
        "application/octet-stream",
        None,
        None,
    )
}

/// Download a small asset with an explicit HTTP `Accept` media type.
///
/// This is the shared anonymous-read path for GitHub API documents as well as release assets.
/// Header control bytes are refused before curl is spawned.
#[cfg_attr(trust_verify, trust::skip)]
pub fn download_bytes_with_accept(
    asset_url: &str,
    max_filesize: u64,
    accept: &str,
) -> Result<Vec<u8>, String> {
    download_bytes_with_headers(asset_url, max_filesize, accept, None, None)
}

/// Download a small asset while requiring revalidation instead of a cached intermediary answer.
#[cfg_attr(trust_verify, trust::skip)]
pub fn download_bytes_uncached(asset_url: &str, max_filesize: u64) -> Result<Vec<u8>, String> {
    download_bytes_with_headers(
        asset_url,
        max_filesize,
        "application/octet-stream",
        Some("Cache-Control: no-cache"),
        None,
    )
}

/// [`download_bytes_uncached`] with each prepared curl command run by `spawn`.
///
/// This retains an embedding operation's child cancellation and environment policy while the
/// shared anonymous-read transport retains the retry policy and verdicts.
#[cfg_attr(trust_verify, trust::skip)]
pub fn download_bytes_uncached_via(
    asset_url: &str,
    max_filesize: u64,
    spawn: &dyn crate::github_rest::Spawn,
) -> Result<Vec<u8>, String> {
    download_bytes_with_headers(
        asset_url,
        max_filesize,
        "application/octet-stream",
        Some("Cache-Control: no-cache"),
        Some(spawn),
    )
}

#[cfg_attr(trust_verify, trust::skip)]
fn download_bytes_with_headers(
    asset_url: &str,
    max_filesize: u64,
    accept: &str,
    extra_header: Option<&str>,
    spawn: Option<&dyn crate::github_rest::Spawn>,
) -> Result<Vec<u8>, String> {
    require_https_url(asset_url)?;
    if accept.is_empty() || !accept.is_ascii() || accept.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err("refusing invalid HTTP Accept media type".to_string());
    }
    let cap = max_filesize.to_string();
    let accept_header = format!("Accept: {accept}");
    let mut attempt: u32 = 1;
    let mut budget =
        aterm_types::http_retry::Budget::new(aterm_types::http_retry::Policy::STANDARD);
    loop {
        let args = download_bytes_args(&cap, &accept_header, extra_header);
        let out = match spawn {
            Some(spawn) => curl_fetch_via(&args, asset_url, spawn),
            None => curl_fetch(&args, asset_url),
        }?;
        if out.status.success() {
            return Ok(out.stdout);
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        if let Some(cause) = curl_retry_cause(out.status.code(), &stderr) {
            match budget.after(attempt, cause) {
                aterm_types::http_retry::Decision::Retry(retry) => {
                    wait_before_retry(retry);
                    attempt = attempt.saturating_add(1);
                    continue;
                }
                aterm_types::http_retry::Decision::Exhausted => {
                    return Err(format!(
                        "curl asset download failed after {}: {}",
                        budget.bound(),
                        curl_diagnostic(&stderr)
                    ));
                }
            }
        }
        if let Some(verdict) = classify_asset_failure(asset_url, &stderr) {
            return Err(verdict);
        }
        return Err(format!(
            "curl asset download failed ({}): {}",
            out.status,
            curl_diagnostic(&stderr)
        ));
    }
}

/// The marker in a rate-limited asset fetch's error ([`classify_asset_failure`]).
const RATE_LIMIT_ERROR_PREFIX: &str = "rate limited (HTTP ";

/// Prefix of a web-host 404 error (`classify_asset_failure`).
const NOT_FOUND_ERROR_PREFIX: &str = "HTTP 404";

/// Whether a [`download_bytes`] error is the web host's 404 verdict.
#[must_use]
pub fn download_error_is_not_found(error: &str) -> bool {
    error.starts_with(NOT_FOUND_ERROR_PREFIX)
}

/// Whether a download's error is this machine failing to store the bytes it was served.
#[must_use]
pub fn download_error_is_local_write(error: &str) -> bool {
    [
        "curl: (23)",
        "Failure writing output to destination",
        "Failed writing body",
    ]
    .iter()
    .any(|words| error.contains(words))
}

/// Returns whether HTTP status `code` on `url` indicates a GitHub rate limit.
fn rate_limit_shaped(url: &str, code: u16) -> bool {
    code == 429 || (code == 403 && crate::cdn::is_api_host(url))
}

/// Classifies a `-f` curl failure into a rate-limit or web-host 403/404 verdict.
fn classify_asset_failure(url: &str, stderr: &str) -> Option<String> {
    let code = curl_http_error_code(stderr)?;
    if rate_limit_shaped(url, code) {
        return Some(format!("{RATE_LIMIT_ERROR_PREFIX}{code}) fetching asset"));
    }
    if crate::cdn::is_api_host(url) {
        return None;
    }
    match code {
        403 => Some(format!(
            "HTTP 403 from the release download host (a blocked host or proxy, not \
             GitHub's rate limit): {url}"
        )),
        404 => Some(format!(
            "HTTP 404: the release does not carry this asset (unpublished, draft, or \
             private): {url}"
        )),
        _ => None,
    }
}

/// Curl's final HTTP status and `Retry-After` delay, from [`CURL_META_WRITE_OUT`].
fn curl_http_meta(stderr: &str) -> (Option<u16>, Option<u64>) {
    let Some(line) = stderr
        .lines()
        .rev()
        .find(|line| line.starts_with(CURL_META_PREFIX))
    else {
        return (None, None);
    };
    let fields = line[CURL_META_PREFIX.len()..].split_once('\t');
    let (status, retry_after) = fields.unwrap_or((line, ""));
    let status = status.parse::<u16>().ok().filter(|status| *status != 0);
    let retry_after =
        aterm_types::http_retry::retry_after_delay_secs(retry_after, aterm_time::unix_secs());
    (status, retry_after)
}

/// The diagnostic without curl's machine-readable trailer.
fn curl_diagnostic(stderr: &str) -> String {
    stderr
        .lines()
        .filter(|line| !line.starts_with(CURL_META_PREFIX))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// Classify one curl failure through the shared anonymous-read policy.
#[must_use]
pub fn curl_retry_cause(exit: Option<i32>, stderr: &str) -> Option<aterm_types::http_retry::Cause> {
    let (written_status, retry_after) = curl_http_meta(stderr);
    let status = written_status.or_else(|| curl_http_error_code_text(stderr));
    aterm_types::http_retry::Cause::from_curl(exit, status, retry_after)
}

/// Extracts the HTTP status code curl reports for a `-f` failure.
#[must_use]
fn curl_http_error_code(stderr: &str) -> Option<u16> {
    curl_http_meta(stderr)
        .0
        .or_else(|| curl_http_error_code_text(stderr))
}

fn curl_http_error_code_text(stderr: &str) -> Option<u16> {
    let idx = stderr.find("returned error: ")?;
    let rest = &stderr[idx + "returned error: ".len()..];
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// GitHub's per-release-asset ceiling.
pub const RELEASE_ASSET_DOWNLOAD_BOUND: u64 = 2_147_483_648;

#[cfg(test)]
const _: () = assert!(RELEASE_ASSET_DOWNLOAD_BOUND > 800_000_000);

fn part_path(dest: &Path) -> Option<std::path::PathBuf> {
    let name = dest.file_name()?;
    let mut part = name.to_os_string();
    part.push(".part");
    Some(dest.with_file_name(part))
}

/// How a resumable attempt should be issued, given what is already on disk. Pure, so the size
/// accounting is assertable: `--max-filesize` is compared against the response's
/// `Content-Length`, which for a ranged response is only the remainder, so curl gets
/// `max_filesize - offset` and the total written stays bounded by `max_filesize`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ResumePlan {
    /// Byte offset to continue from; `0` is a fresh transfer with no `--continue-at`.
    offset: u64,
    /// What to pass as `--max-filesize`.
    remaining_cap: u64,
    /// Whether the existing prefix must be discarded first.
    discard: bool,
}

const fn resume_plan(existing: u64, max_filesize: u64) -> ResumePlan {
    if existing >= max_filesize {
        return ResumePlan {
            offset: 0,
            remaining_cap: max_filesize,
            discard: true,
        };
    }
    ResumePlan {
        offset: existing,
        remaining_cap: max_filesize.saturating_sub(existing),
        discard: false,
    }
}

const fn keep_partial(before: u64, after: u64) -> bool {
    after > before
}

fn keep_partial_after_failure(url: &str, stderr: &str, before: u64, after: u64) -> bool {
    if keep_partial(before, after) {
        return true;
    }
    !crate::cdn::is_api_host(url) && matches!(curl_http_error_code(stderr), Some(403 | 404))
}

fn range_refused(exit_code: Option<i32>, stderr: &str) -> bool {
    exit_code == Some(33) && !curl_http_error_code(stderr).is_some_and(|status| status >= 400)
}

/// How long a resumable attempt may run under 4 KiB/s before curl calls it stalled
/// (`--speed-time`), chosen per attempt from how the one before it went
/// ([`StallBound::after`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StallBound {
    /// 8 s: the stall is reported about 14 s after the last byte.
    Quick,
    /// The historical 120 s, after an attempt that never beat 4 KiB/s on average.
    Patient,
}

impl StallBound {
    const fn secs(self) -> &'static str {
        match self {
            Self::Quick => "8",
            Self::Patient => "120",
        }
    }

    fn after(moved: u64, took: std::time::Duration) -> Self {
        if u128::from(moved) * 1000 >= 4096 * took.as_millis() {
            Self::Quick
        } else {
            Self::Patient
        }
    }
}

const RESUME_KEEPALIVE_SECS: &str = "2";

fn download_resume_args<'a>(
    cap: &'a str,
    dest: &'a str,
    offset: Option<&'a str>,
    stall: StallBound,
) -> Vec<&'a str> {
    let mut args: Vec<&str> = vec![
        "-fsSL",
        "--proto-redir",
        "=https",
        "--connect-timeout",
        "30",
        "--speed-limit",
        "4096",
        "--speed-time",
        stall.secs(),
        "--keepalive-time",
        RESUME_KEEPALIVE_SECS,
        "--max-filesize",
        cap,
        "-H",
        "Accept: application/octet-stream",
        "--write-out",
        CURL_META_WRITE_OUT,
        "-o",
        dest,
    ];
    if let Some(offset) = offset {
        args.push("--continue-at");
        args.push(offset);
    }
    args
}

/// Signed/vendor HTTPS downloads also retain verify provision's TLS 1.2 floor.
const HTTPS_ONLY_ARGS: [&str; 3] = ["--proto", "=https", "--tlsv1.2"];

fn download_resume_args_https_only<'a>(
    cap: &'a str,
    dest: &'a str,
    offset: Option<&'a str>,
    stall: StallBound,
) -> Vec<&'a str> {
    let mut args: Vec<&str> = HTTPS_ONLY_ARGS.to_vec();
    args.extend(download_resume_args(cap, dest, offset, stall));
    args
}

/// What a resumable download does after a failed attempt that carried no verdict about the
/// URL ([`next_attempt`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NextAttempt {
    /// Continue from the `.part`; the completed prefix earned another attempt.
    Resume(aterm_types::http_retry::Cause),
    /// The same offset again after curl's own backoff.
    Retry(aterm_types::http_retry::Cause),
    /// Return the failure.
    Stop,
}

fn next_attempt(exit: Option<i32>, stderr: &str, moved: bool) -> NextAttempt {
    if download_error_is_local_write(stderr) {
        return NextAttempt::Stop;
    }
    if curl_http_error_code(stderr).is_some_and(|status| status >= 400) {
        return match curl_retry_cause(exit, stderr) {
            Some(cause) if moved => NextAttempt::Resume(cause),
            Some(cause) => NextAttempt::Retry(cause),
            None => NextAttempt::Stop,
        };
    }
    if filesize_exceeded(exit, stderr) {
        NextAttempt::Stop
    } else if let Some(cause) = curl_retry_cause(exit, stderr) {
        if moved {
            NextAttempt::Resume(cause)
        } else {
            NextAttempt::Retry(cause)
        }
    } else {
        NextAttempt::Stop
    }
}

const SHORT_RESUMES_PER_CALL: u32 = 16;

const RESUME_EARNED_AT: u64 = 1 << 20;

fn resume_allowed(moved: u64, short: u32, all: u32, cap: u64) -> bool {
    let budget = resume_budget(cap);
    u64::from(all) < budget && (moved >= RESUME_EARNED_AT || short < SHORT_RESUMES_PER_CALL)
}

const fn resume_budget(cap: u64) -> u64 {
    (SHORT_RESUMES_PER_CALL as u64).saturating_add(cap / RESUME_EARNED_AT)
}

/// What a stopped release read answers ([`crate::release_asset::AssetError::Stopped`]).
pub const DOWNLOAD_STOPPED: &str = "download stopped: another source delivered the bytes first";

/// Download a vendor-hosted asset to `dest`, resumably, through `<dest>.part`.
#[cfg_attr(trust_verify, trust::skip)]
pub fn download_to_resumable_https_only(
    asset_url: &str,
    dest: &Path,
    max_filesize: u64,
) -> Result<(), String> {
    download_to_resumable_with(asset_url, dest, max_filesize, ResumeLane::HttpsOnly, None)
        .map_err(|e| e.to_string())
}

/// [`download_to_resumable_https_only`] with each prepared curl command run by `spawn`.
///
/// The shared downloader retains HTTPS enforcement, progress accounting, response
/// classification, and its one bounded retry budget; `spawn` controls only child execution.
#[cfg_attr(trust_verify, trust::skip)]
pub fn download_to_resumable_https_only_via(
    asset_url: &str,
    dest: &Path,
    max_filesize: u64,
    spawn: &dyn crate::github_rest::Spawn,
) -> Result<(), String> {
    download_to_resumable_with(
        asset_url,
        dest,
        max_filesize,
        ResumeLane::HttpsOnly,
        Some(spawn),
    )
    .map_err(|e| e.to_string())
}

/// Which request and child a resumable download runs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ResumeLane {
    /// A signed index row's vendor URL.
    HttpsOnly,
    /// A vendor-direct payload.
    VendorDirect,
}

/// The shared body of [`download_to_resumable_https_only`] and [`vendor_download_to`]: `lane`
/// selects the child and the verdicts. The bytes land in `<dest>.part`, kept across calls for
/// the next one to continue and renamed onto `dest` once whole; `spawn`, when given, runs each
/// prepared curl.
#[cfg_attr(trust_verify, trust::skip)]
fn download_to_resumable_with(
    asset_url: &str,
    dest: &Path,
    max_filesize: u64,
    lane: ResumeLane,
    spawn: Option<&dyn crate::github_rest::Spawn>,
) -> Result<(), HttpError> {
    require_https_url(asset_url).map_err(HttpError::Transport)?;
    let part = part_path(dest)
        .ok_or_else(|| HttpError::Transport("destination has no file name".to_string()))?;
    let part_s = part
        .to_str()
        .ok_or_else(|| HttpError::Transport("non-UTF-8 destination path".to_string()))?;
    let mut retried_fresh = false;
    let mut resumes: u32 = 0;
    let mut short_resumes: u32 = 0;
    let mut idle_failures: u32 = 0;
    let mut idle_budget =
        aterm_types::http_retry::Budget::new(aterm_types::http_retry::Policy::STANDARD);
    let mut stall = StallBound::Quick;
    let mut call_began: Option<u64> = None;
    loop {
        let existing = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
        let plan = resume_plan(existing, max_filesize);
        let existing = if plan.discard {
            let _ = std::fs::remove_file(&part);
            0
        } else {
            existing
        };
        let began = *call_began.get_or_insert(existing);
        let cap = plan.remaining_cap.to_string();
        let offset_text = plan.offset.to_string();
        let offset = (plan.offset > 0).then_some(offset_text.as_str());
        let args = download_resume_args_https_only(&cap, part_s, offset, stall);
        let attempt_began = std::time::Instant::now();
        let out = match spawn {
            _ if lane == ResumeLane::VendorDirect => vendor_fetch(&args, asset_url),
            Some(spawn) => curl_fetch_via(&args, asset_url, spawn),
            None => curl_fetch(&args, asset_url),
        }
        .map_err(HttpError::Transport)?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            if plan.offset > 0 && !retried_fresh && range_refused(out.status.code(), &stderr) {
                note_retry(format!(
                    "anonymous HTTPS read: the server refused a Range resume after {} bytes — \
                     retrying once from byte zero",
                    plan.offset
                ));
                let _ = std::fs::remove_file(&part);
                retried_fresh = true;
                call_began = Some(0);
                idle_failures = 0;
                idle_budget =
                    aterm_types::http_retry::Budget::new(aterm_types::http_retry::Policy::STANDARD);
                continue;
            }
            let after = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
            let cause = curl_retry_cause(out.status.code(), &stderr);
            let verdict = if lane == ResumeLane::VendorDirect {
                vendor_download_verdict(out.status.code(), &stderr, asset_url, max_filesize)
            } else {
                None
            }
            .or_else(|| {
                cause
                    .is_none()
                    .then(|| classify_asset_failure(asset_url, &stderr))
                    .flatten()
                    .map(HttpError::Transport)
            });
            if verdict.is_none() {
                let moved = keep_partial(existing, after);
                let moved_bytes = after.saturating_sub(existing);
                stall = StallBound::after(moved_bytes, attempt_began.elapsed());
                let mut exhausted_bound: Option<String> = None;
                match next_attempt(out.status.code(), &stderr, moved) {
                    NextAttempt::Resume(cause) => {
                        if resume_allowed(moved_bytes, short_resumes, resumes, max_filesize) {
                            note_retry(format!(
                                "anonymous HTTPS read: attempt made {moved_bytes} bytes of \
                                 progress, then failed ({cause}) — resuming ({}/{})",
                                resumes + 1,
                                resume_budget(max_filesize)
                            ));
                            resumes += 1;
                            if moved_bytes < RESUME_EARNED_AT {
                                short_resumes += 1;
                            }
                            idle_failures = 0;
                            idle_budget = aterm_types::http_retry::Budget::new(
                                aterm_types::http_retry::Policy::STANDARD,
                            );
                            continue;
                        }
                        exhausted_bound = Some(format!(
                            "the {} progress-earned resume bound",
                            resume_budget(max_filesize)
                        ));
                    }
                    NextAttempt::Retry(cause) => {
                        idle_failures = idle_failures.saturating_add(1);
                        match idle_budget.after(idle_failures, cause) {
                            aterm_types::http_retry::Decision::Retry(retry) => {
                                wait_before_retry_via(retry, spawn.is_none());
                                continue;
                            }
                            aterm_types::http_retry::Decision::Exhausted => {
                                exhausted_bound = Some(idle_budget.bound());
                            }
                        }
                    }
                    NextAttempt::Stop => {}
                }
                if !keep_partial_after_failure(asset_url, &stderr, began, after) {
                    let _ = std::fs::remove_file(&part);
                }
                let bound = exhausted_bound
                    .map(|bound| format!(" after {bound}"))
                    .unwrap_or_default();
                return Err(HttpError::Transport(format!(
                    "curl download failed{bound} ({}): {}",
                    out.status,
                    curl_diagnostic(&stderr)
                )));
            }
            if !keep_partial_after_failure(asset_url, &stderr, began, after) {
                let _ = std::fs::remove_file(&part);
            }
            if let Some(verdict) = verdict {
                return Err(verdict);
            }
        }
        return std::fs::rename(&part, dest).map_err(|e| {
            let _ = std::fs::remove_file(&part);
            HttpError::Transport(format!("finalize download: {e}"))
        });
    }
}

/// What [`vendor_get`] came back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VendorResponse {
    /// HTTP 200: the whole body (at most the cap).
    Body {
        bytes: Vec<u8>,
        etag: Option<String>,
        effective_url: String,
    },
    /// HTTP 304 to a conditional request.
    NotModified { etag: String },
}

/// The curl child environment a vendor request drops: the curlrc redirections, plus
/// everything that changes which CAs curl trusts, including the TLS backend switch
/// (`CURL_SSL_BACKEND=secure-transport` makes a multi-backend curl trust the keychain's
/// user-added roots).
pub(crate) const VENDOR_ENV_SCRUB: [&str; 6] = [
    "CURL_HOME",
    "XDG_CONFIG_HOME",
    "CURL_CA_BUNDLE",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "CURL_SSL_BACKEND",
];

const VENDOR_MAX_REDIRS: &str = "5";

const ETAG_MAX: usize = 256;

const VENDOR_CURL_CAP_FLOOR: u64 = 16 * 1024;

fn vendor_curl_cap(cap: u64) -> u64 {
    cap.max(VENDOR_CURL_CAP_FLOOR)
}

fn filesize_exceeded(exit: Option<i32>, stderr: &str) -> bool {
    exit == Some(63) || stderr.contains("Maximum file size exceeded")
}

fn vendor_curl_refusal(exit: Option<i32>, stderr: &str, url: &str) -> Option<HttpError> {
    let why = match exit? {
        1 => "a hop that is not https",
        47 => "too many redirects",
        51 | 60 => "a TLS certificate that does not verify",
        _ => return None,
    };
    Some(HttpError::VendorRefused(format!(
        "curl refused {url}: {why} ({})",
        stderr.trim()
    )))
}

fn etag_ok(etag: &str) -> bool {
    !etag.is_empty() && etag.len() <= ETAG_MAX && etag.bytes().all(|b| (0x21..=0x7e).contains(&b))
}

fn last_block_header(headers: &str, name: &str) -> Option<String> {
    let mut found: Option<String> = None;
    let mut conflict = false;
    for line in headers.lines() {
        if line.starts_with("HTTP/") {
            found = None;
            conflict = false;
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if !key.trim().eq_ignore_ascii_case(name) {
            continue;
        }
        let value = value.trim();
        match &found {
            Some(previous) if previous != value => conflict = true,
            _ => found = (!value.is_empty()).then(|| value.to_string()),
        }
    }
    if conflict { None } else { found }
}

fn etag_header(headers: &str) -> Option<String> {
    last_block_header(headers, "etag").filter(|e| etag_ok(e))
}

fn content_length_header(headers: &str) -> Option<u64> {
    let value = last_block_header(headers, "content-length")?;
    if !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    value.parse::<u64>().ok().filter(|n| *n > 0)
}

fn vendor_trailer(stdout: &str) -> Option<(u16, String)> {
    let (code, rest) = stdout.trim().split_once(' ')?;
    let url = match rest.rsplit_once(' ') {
        Some((url, secs)) if !secs.starts_with('+') && secs.parse::<f64>().is_ok() => url,
        _ => rest,
    };
    if code.is_empty() || !code.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let code = code.parse::<u16>().ok()?;
    if !url.starts_with("https://") || url.bytes().any(|b| b <= b' ' || b == 0x7f) {
        return None;
    }
    Some((code, url.to_string()))
}

struct CurlRun<'a> {
    exit: Option<i32>,
    stdout: &'a str,
    stderr: &'a str,
}

impl CurlRun<'_> {
    fn failure(&self, verb: &str, url: &str) -> HttpError {
        let exit = self
            .exit
            .map_or_else(|| "ended by a signal".to_string(), |c| format!("exit {c}"));
        HttpError::Transport(format!(
            "curl {verb} {url} failed ({exit}): {}",
            self.stderr.trim()
        ))
    }
}

/// What one vendor-lane attempt decided.
#[derive(Debug, PartialEq, Eq)]
enum Step<T> {
    /// Another attempt may cure it; `exhausted` is the refusal when the shared bound is spent.
    Retry {
        cause: aterm_types::http_retry::Cause,
        exhausted: HttpError,
    },
    /// The answer, or a verdict no further attempt changes.
    Done(Result<T, HttpError>),
}

fn admit_vendor_retry(
    attempt: u32,
    budget: &mut aterm_types::http_retry::Budget,
    cause: aterm_types::http_retry::Cause,
    exhausted: HttpError,
) -> Result<(), HttpError> {
    admit_vendor_retry_via(attempt, budget, cause, exhausted, wait_before_retry)
}

fn admit_vendor_retry_via(
    attempt: u32,
    budget: &mut aterm_types::http_retry::Budget,
    cause: aterm_types::http_retry::Cause,
    exhausted: HttpError,
    mut on_retry: impl FnMut(aterm_types::http_retry::Retry),
) -> Result<(), HttpError> {
    match budget.after(attempt, cause) {
        aterm_types::http_retry::Decision::Retry(retry) => {
            on_retry(retry);
            Ok(())
        }
        aterm_types::http_retry::Decision::Exhausted => Err(HttpError::Transport(format!(
            "{exhausted} after {}",
            budget.bound()
        ))),
    }
}

fn vendor_get_step(
    run: &CurlRun<'_>,
    headers: &str,
    url: &str,
    cap: u64,
    sent_etag: Option<&str>,
    body: impl FnOnce() -> Result<Vec<u8>, HttpError>,
) -> Step<VendorResponse> {
    let trailer = vendor_trailer(run.stdout);
    let status = trailer.as_ref().map(|(code, _)| *code);
    let retry_after = last_block_header(headers, "retry-after").and_then(|value| {
        aterm_types::http_retry::retry_after_delay_secs(&value, aterm_time::unix_secs())
    });
    if run.exit != Some(0) {
        let exhausted = run.failure("GET", url);
        if let Some(code @ 400..=599) = status {
            if let Some(cause) = aterm_types::http_retry::Cause::from_http(code, retry_after) {
                return Step::Retry { cause, exhausted };
            }
            return Step::Done(Err(HttpError::VendorStatus {
                code,
                url: url.to_string(),
            }));
        }
        if filesize_exceeded(run.exit, run.stderr) {
            return Step::Done(Err(HttpError::VendorRefused(format!(
                "vendor document {url} exceeds its {cap}-byte cap"
            ))));
        }
        if let Some(refusal) = vendor_curl_refusal(run.exit, run.stderr, url) {
            return Step::Done(Err(refusal));
        }
        return match aterm_types::http_retry::Cause::from_curl(run.exit, status, retry_after) {
            Some(cause) => Step::Retry { cause, exhausted },
            None => Step::Done(Err(exhausted)),
        };
    }
    let Some((code, effective_url)) = trailer else {
        return Step::Done(Err(HttpError::Malformed(format!(
            "the vendor host answered an unreadable status for {url}: {}",
            run.stdout.trim()
        ))));
    };
    if let Some(cause) = aterm_types::http_retry::Cause::from_http(code, retry_after) {
        return Step::Retry {
            cause,
            exhausted: HttpError::VendorStatus {
                code,
                url: url.to_string(),
            },
        };
    }
    let etag = etag_header(headers);
    Step::Done(match (code, sent_etag) {
        (200, _) => body().map(|bytes| VendorResponse::Body {
            bytes,
            etag,
            effective_url,
        }),
        (304, Some(sent)) => Ok(VendorResponse::NotModified {
            etag: etag.unwrap_or_else(|| sent.to_string()),
        }),
        _ => Err(HttpError::VendorStatus {
            code,
            url: url.to_string(),
        }),
    })
}

/// A HEAD attempt: `Ok(None)` is a final 200 that names no single positive `Content-Length`
/// (npm's registry answers its tarballs so), which [`vendor_range_step`] sizes instead.
fn vendor_head_step(run: &CurlRun<'_>, url: &str) -> Step<Option<u64>> {
    let (headers, trailer) = match run.stdout.rfind('\n') {
        Some(i) => (&run.stdout[..i], run.stdout[i + 1..].trim()),
        None => ("", run.stdout.trim()),
    };
    let code_text = trailer.split_whitespace().next().unwrap_or("");
    let code = (!code_text.is_empty() && code_text.bytes().all(|b| b.is_ascii_digit()))
        .then(|| code_text.parse::<u16>().ok())
        .flatten()
        .filter(|status| *status != 0);
    let retry_after = last_block_header(headers, "retry-after").and_then(|value| {
        aterm_types::http_retry::retry_after_delay_secs(&value, aterm_time::unix_secs())
    });
    if run.exit != Some(0) {
        let exhausted = run.failure("HEAD", url);
        if let Some(code @ 400..=599) = code {
            if let Some(cause) = aterm_types::http_retry::Cause::from_http(code, retry_after) {
                return Step::Retry { cause, exhausted };
            }
            return Step::Done(Err(HttpError::VendorStatus {
                code,
                url: url.to_string(),
            }));
        }
        if let Some(refusal) = vendor_curl_refusal(run.exit, run.stderr, url) {
            return Step::Done(Err(refusal));
        }
        return match aterm_types::http_retry::Cause::from_curl(run.exit, code, retry_after) {
            Some(cause) => Step::Retry { cause, exhausted },
            None => Step::Done(Err(exhausted)),
        };
    }
    let Some(code) = code else {
        return Step::Done(Err(HttpError::Malformed(format!(
            "the vendor host answered HTTP {code_text} to HEAD {url}"
        ))));
    };
    if let Some(cause) = aterm_types::http_retry::Cause::from_http(code, retry_after) {
        return Step::Retry {
            cause,
            exhausted: HttpError::VendorStatus {
                code,
                url: url.to_string(),
            },
        };
    }
    if code != 200 {
        return Step::Done(Err(HttpError::VendorStatus {
            code,
            url: url.to_string(),
        }));
    }
    Step::Done(Ok(content_length_header(headers)))
}

/// The total of the final hop's `Content-Range: bytes 0-0/<total>`: one positive decimal.
fn content_range_total(headers: &str) -> Option<u64> {
    let value = last_block_header(headers, "content-range")?;
    let total = value.strip_prefix("bytes 0-0/")?;
    if total.is_empty() || !total.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    total.parse::<u64>().ok().filter(|n| *n > 0)
}

/// A one-byte ranged GET's attempt (`Range: bytes=0-0`): the total its final 206 names.
fn vendor_range_step(run: &CurlRun<'_>, url: &str) -> Step<u64> {
    let no_size = || {
        HttpError::Malformed(format!(
            "the vendor host sent no usable Content-Length for {url}, and no \
             `Content-Range: bytes 0-0/<size>` to a one-byte range"
        ))
    };
    let (headers, trailer) = match run.stdout.rfind('\n') {
        Some(i) => (&run.stdout[..i], run.stdout[i + 1..].trim()),
        None => ("", run.stdout.trim()),
    };
    let code_text = trailer.split_whitespace().next().unwrap_or("");
    let code = (!code_text.is_empty() && code_text.bytes().all(|b| b.is_ascii_digit()))
        .then(|| code_text.parse::<u16>().ok())
        .flatten()
        .filter(|status| *status != 0);
    let retry_after = last_block_header(headers, "retry-after").and_then(|value| {
        aterm_types::http_retry::retry_after_delay_secs(&value, aterm_time::unix_secs())
    });
    if run.exit != Some(0) {
        let exhausted = run.failure("GET", url);
        if let Some(code @ 400..=599) = code {
            if let Some(cause) = aterm_types::http_retry::Cause::from_http(code, retry_after) {
                return Step::Retry { cause, exhausted };
            }
            return Step::Done(Err(HttpError::VendorStatus {
                code,
                url: url.to_string(),
            }));
        }
        if filesize_exceeded(run.exit, run.stderr) {
            return Step::Done(Err(no_size()));
        }
        if let Some(refusal) = vendor_curl_refusal(run.exit, run.stderr, url) {
            return Step::Done(Err(refusal));
        }
        return match aterm_types::http_retry::Cause::from_curl(run.exit, code, retry_after) {
            Some(cause) => Step::Retry { cause, exhausted },
            None => Step::Done(Err(exhausted)),
        };
    }
    if let Some(code) = code
        && let Some(cause) = aterm_types::http_retry::Cause::from_http(code, retry_after)
    {
        return Step::Retry {
            cause,
            exhausted: HttpError::VendorStatus {
                code,
                url: url.to_string(),
            },
        };
    }
    match code {
        Some(206) => Step::Done(content_range_total(headers).ok_or_else(no_size)),
        Some(200) => Step::Done(Err(no_size())),
        Some(code) => Step::Done(Err(HttpError::VendorStatus {
            code,
            url: url.to_string(),
        })),
        None => Step::Done(Err(HttpError::Malformed(format!(
            "the vendor host answered HTTP {code_text} to a ranged GET of {url}"
        )))),
    }
}

fn vendor_download_verdict(
    exit: Option<i32>,
    stderr: &str,
    url: &str,
    cap: u64,
) -> Option<HttpError> {
    if let Some(code @ 400..=599) = curl_http_error_code(stderr) {
        if curl_retry_cause(exit, stderr).is_some() {
            return None;
        }
        return Some(HttpError::VendorStatus {
            code,
            url: url.to_string(),
        });
    }
    if filesize_exceeded(exit, stderr) {
        return Some(HttpError::VendorRefused(format!(
            "vendor payload {url} exceeds its {cap}-byte cap"
        )));
    }
    if let Some(refusal) = vendor_curl_refusal(exit, stderr, url) {
        return Some(refusal);
    }
    None
}

/// A vendor GET's time budget: curl's connect and whole-transfer bounds (seconds), and how
/// many attempts a network failure gets.
#[derive(Clone, Copy, Debug)]
struct VendorBounds {
    connect_secs: &'static str,
    max_secs: &'static str,
    policy: aterm_types::http_retry::Policy,
}

const LANE_BOUNDS: VendorBounds = VendorBounds {
    connect_secs: SMALL_CONNECT_SECS,
    max_secs: "60",
    policy: aterm_types::http_retry::Policy::STANDARD,
};

const HINT_BOUNDS: VendorBounds = VendorBounds {
    connect_secs: "5",
    max_secs: "15",
    policy: aterm_types::http_retry::Policy::NONE,
};

fn vendor_get_args<'a>(
    bounds: VendorBounds,
    cap: &'a str,
    body: &'a str,
    headers: &'a str,
    condition: Option<&'a str>,
) -> Vec<&'a str> {
    let mut args = vec![
        "-sS",
        "--proto",
        "=https",
        "--proto-redir",
        "=https",
        "-L",
        "--max-redirs",
        VENDOR_MAX_REDIRS,
        "--connect-timeout",
        bounds.connect_secs,
        "--max-time",
        bounds.max_secs,
        "--max-filesize",
        cap,
        "-o",
        body,
        "--dump-header",
        headers,
        "-w",
        "%{http_code} %{url_effective} %{time_appconnect}",
    ];
    if let Some(line) = condition {
        args.push("-H");
        args.push(line);
    }
    args
}

const fn vendor_head_args() -> [&'static str; 15] {
    [
        "-sS",
        "-I",
        "--proto",
        "=https",
        "--proto-redir",
        "=https",
        "-L",
        "--max-redirs",
        VENDOR_MAX_REDIRS,
        "--connect-timeout",
        SMALL_CONNECT_SECS,
        "--max-time",
        "60",
        "-w",
        "\n%{http_code} %{time_appconnect}",
    ]
}

/// A GET of `url`'s first byte, its headers on stdout and that byte into `body`, refused
/// past one byte: what sizes a payload whose HEAD names no length.
fn vendor_range_args(body: &str) -> [&str; 22] {
    [
        "-sS",
        "-r",
        "0-0",
        "--max-filesize",
        "1",
        "-o",
        body,
        "-D",
        "-",
        "--proto",
        "=https",
        "--proto-redir",
        "=https",
        "-L",
        "--max-redirs",
        VENDOR_MAX_REDIRS,
        "--connect-timeout",
        SMALL_CONNECT_SECS,
        "--max-time",
        "60",
        "-w",
        "\n%{http_code} %{time_appconnect}",
    ]
}

fn vendor_command(args: &[&str], url: &str) -> Command {
    let mut command = curl_command(args, url);
    for var in VENDOR_ENV_SCRUB {
        command.env_remove(var);
    }
    command
}

/// Spawn [`vendor_command`] with no stdin and wait for it.
#[cfg_attr(trust_verify, trust::skip)]
fn vendor_fetch(args: &[&str], url: &str) -> Result<std::process::Output, String> {
    use aterm_uds::spawnfd::Stdio;
    let mut cmd = vendor_command(args, url);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    crate::run_operation_command(&mut cmd).map_err(|e| format!("spawn curl: {e}"))
}

/// A fresh private directory for one vendor request's body and header dump, removed on
/// drop.
pub(crate) struct VendorScratch(pub(crate) std::path::PathBuf);

impl VendorScratch {
    #[cfg_attr(trust_verify, trust::skip)]
    fn new() -> Result<Self, String> {
        Self::labelled("vendor")
    }

    /// [`Self::new`] named `aterm-{label}-…` (the REST lane's is `github`).
    #[cfg_attr(trust_verify, trust::skip)]
    pub(crate) fn labelled(label: &str) -> Result<Self, String> {
        aterm_tempfile::Builder::new()
            .prefix(&format!("aterm-{label}-"))
            .suffix(".noindex")
            .tempdir()
            .map(|directory| Self(directory.keep()))
            .map_err(|error| format!("create {label} scratch dir: {error}"))
    }
}

impl Drop for VendorScratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn read_capped(path: &Path, cap: u64, url: &str) -> Result<Vec<u8>, HttpError> {
    use std::io::Read;
    let (file, meta) = match aterm_uds::regular_file::open(path) {
        Ok(opened) => opened,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(HttpError::Transport(format!("read vendor body: {e}"))),
    };
    if meta.len() > cap {
        return Err(HttpError::VendorRefused(format!(
            "vendor document {url} exceeds its {cap}-byte cap"
        )));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len().saturating_add(1)).unwrap_or(0));
    file.take(cap.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| HttpError::Transport(format!("read vendor body: {e}")))?;
    if bytes.len() as u64 > cap {
        return Err(HttpError::VendorRefused(format!(
            "vendor document {url} exceeds its {cap}-byte cap"
        )));
    }
    Ok(bytes)
}

/// Get a small vendor document (a release head, a manifest, a signature, a SHA256SUMS) anonymously.
/// # Errors
/// [`HttpError::VendorRefused`] for a non-https URL, a zero cap, or an `if_none_match`
/// that is not a visible-ASCII ETag (before any spawn), a document over its cap, or a
/// [`vendor_curl_refusal`]; [`HttpError::VendorStatus`] for any status but 200 and a
/// conditional 304; [`HttpError::Malformed`] for an unreadable trailer; and
/// [`HttpError::Transport`] for everything else, a network that failed every attempt
/// included.
pub fn vendor_get(
    url: &str,
    cap: u64,
    if_none_match: Option<&str>,
) -> Result<VendorResponse, HttpError> {
    vendor_get_within(url, cap, if_none_match, LANE_BOUNDS)
}

/// [`vendor_get`] for a head read only as a hint (the window's head watch).
/// # Errors
/// As [`vendor_get`]; a network failure is [`HttpError::Transport`] after the one attempt.
pub fn vendor_get_hint(
    url: &str,
    cap: u64,
    if_none_match: Option<&str>,
) -> Result<VendorResponse, HttpError> {
    vendor_get_within(url, cap, if_none_match, HINT_BOUNDS)
}

/// [`vendor_get`] under `bounds`.
#[cfg_attr(trust_verify, trust::skip)]
fn vendor_get_within(
    url: &str,
    cap: u64,
    if_none_match: Option<&str>,
    bounds: VendorBounds,
) -> Result<VendorResponse, HttpError> {
    require_https_url(url).map_err(HttpError::VendorRefused)?;
    if cap == 0 {
        return Err(HttpError::VendorRefused(format!(
            "refusing a zero byte cap for {url}"
        )));
    }
    if let Some(etag) = if_none_match
        && !etag_ok(etag)
    {
        return Err(HttpError::VendorRefused(format!(
            "refusing a malformed If-None-Match value for {url}"
        )));
    }
    let condition = if_none_match.map(|etag| format!("If-None-Match: {etag}"));
    let scratch = VendorScratch::new().map_err(HttpError::Transport)?;
    let body = scratch.0.join("body");
    let headers = scratch.0.join("headers");
    let (Some(body_s), Some(headers_s)) = (body.to_str(), headers.to_str()) else {
        return Err(HttpError::Transport(
            "non-UTF-8 temporary directory".to_string(),
        ));
    };
    let curl_cap = vendor_curl_cap(cap).to_string();
    let args = vendor_get_args(bounds, &curl_cap, body_s, headers_s, condition.as_deref());
    let mut attempt: u32 = 1;
    let mut budget = aterm_types::http_retry::Budget::new(bounds.policy);
    loop {
        let _ = std::fs::remove_file(&body);
        let _ = std::fs::remove_file(&headers);
        let out = vendor_fetch(&args, url).map_err(HttpError::Transport)?;
        let dump = read_capped(&headers, 64 * 1024, url).unwrap_or_default();
        let (stdout, stderr) = (
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        let run = CurlRun {
            exit: out.status.code(),
            stdout: &stdout,
            stderr: &stderr,
        };
        let step = vendor_get_step(
            &run,
            &String::from_utf8_lossy(&dump),
            url,
            cap,
            if_none_match,
            || read_capped(&body, cap, url),
        );
        match step {
            Step::Retry { cause, exhausted } => {
                admit_vendor_retry(attempt, &mut budget, cause, exhausted)?;
                attempt = attempt.saturating_add(1);
            }
            Step::Done(result) => return result,
        }
    }
}

/// The final size of `url` after redirects: its `Content-Length` to a HEAD, or, when the
/// final 200 names none, the total of a one-byte ranged GET's `Content-Range`.
/// # Errors
/// [`HttpError::VendorRefused`] for a non-https URL (before any spawn) or a
/// [`vendor_curl_refusal`]; [`HttpError::VendorStatus`] for a final status other than 200
/// (206 to the ranged GET); [`HttpError::Malformed`] for an unreadable status, or a final
/// hop naming neither a single positive numeric `Content-Length` nor a
/// `Content-Range: bytes 0-0/<size>`; [`HttpError::Transport`] for everything else, a
/// network that failed every attempt included.
#[cfg_attr(trust_verify, trust::skip)]
pub fn vendor_content_length(url: &str) -> Result<u64, HttpError> {
    require_https_url(url).map_err(HttpError::VendorRefused)?;
    let head_args = vendor_head_args();
    let scratch = VendorScratch::new().map_err(HttpError::Transport)?;
    let body = scratch.0.join("body");
    let Some(body_s) = body.to_str() else {
        return Err(HttpError::Transport(
            "non-UTF-8 temporary directory".to_string(),
        ));
    };
    let range_args = vendor_range_args(body_s);
    vendor_content_length_steps(
        url,
        || {
            let out = match vendor_fetch(&head_args, url) {
                Ok(out) => out,
                Err(error) => return Step::Done(Err(HttpError::Transport(error))),
            };
            let (stdout, stderr) = (
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr),
            );
            vendor_head_step(
                &CurlRun {
                    exit: out.status.code(),
                    stdout: &stdout,
                    stderr: &stderr,
                },
                url,
            )
        },
        || {
            let _ = std::fs::remove_file(&body);
            let out = match vendor_fetch(&range_args, url) {
                Ok(out) => out,
                Err(error) => return Step::Done(Err(HttpError::Transport(error))),
            };
            let (stdout, stderr) = (
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr),
            );
            vendor_range_step(
                &CurlRun {
                    exit: out.status.code(),
                    stdout: &stdout,
                    stderr: &stderr,
                },
                url,
            )
        },
        wait_before_retry,
    )
}

/// One logical size read across its HEAD and optional one-byte range phases. Successful phase
/// transitions consume an attempt too: the whole read, not each transport shape, owns the shared
/// nineteen-attempt / 200-second pause budget.
fn vendor_content_length_steps(
    url: &str,
    mut head: impl FnMut() -> Step<Option<u64>>,
    mut range: impl FnMut() -> Step<u64>,
    mut on_retry: impl FnMut(aterm_types::http_retry::Retry),
) -> Result<u64, HttpError> {
    let policy = aterm_types::http_retry::Policy::STANDARD;
    let mut budget = aterm_types::http_retry::Budget::new(policy);
    let mut attempt: u32 = 1;
    loop {
        match head() {
            Step::Retry { cause, exhausted } => {
                admit_vendor_retry_via(attempt, &mut budget, cause, exhausted, &mut on_retry)?;
                attempt = attempt.saturating_add(1);
            }
            Step::Done(Ok(Some(size))) => return Ok(size),
            Step::Done(Ok(None)) => break,
            Step::Done(Err(error)) => return Err(error),
        }
    }

    attempt = attempt.saturating_add(1);
    if attempt > policy.attempts() {
        return Err(HttpError::Transport(format!(
            "vendor size read for {url} needs its ranged GET after {}",
            budget.bound()
        )));
    }
    loop {
        match range() {
            Step::Retry { cause, exhausted } => {
                admit_vendor_retry_via(attempt, &mut budget, cause, exhausted, &mut on_retry)?;
                attempt = attempt.saturating_add(1);
            }
            Step::Done(result) => return result,
        }
    }
}

/// Download a vendor-direct payload to `dest`, capped at exactly `cap` bytes.
/// # Errors
/// [`HttpError::VendorRefused`] for a non-https URL or a zero cap (before any spawn), a
/// payload over its cap, or a [`vendor_curl_refusal`]; [`HttpError::VendorStatus`] for a
/// status no retry changes (anything but 5xx, or 429 with `Retry-After`); [`HttpError::Transport`] for
/// everything else, the network included.
#[cfg_attr(trust_verify, trust::skip)]
pub fn vendor_download_to(url: &str, dest: &Path, cap: u64) -> Result<(), HttpError> {
    require_https_url(url).map_err(HttpError::VendorRefused)?;
    if cap == 0 {
        return Err(HttpError::VendorRefused(format!(
            "refusing a zero byte cap for {url}"
        )));
    }
    download_to_resumable_with(url, dest, cap, ResumeLane::VendorDirect, None)
}

#[cfg(test)]
#[path = "http_tests.rs"]
mod tests;
