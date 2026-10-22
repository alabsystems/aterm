// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The network layer: anonymous `curl` calls to GitHub's unmetered web host
//! (`github.com/…/releases/…`). No credential is ever sent. Artifact-agnostic: the consuming
//! crate decides what the bytes mean.
//!
//! For a public repo, `…/releases/download/{tag}/{name}` answers 200 via a 302 to
//! `release-assets.githubusercontent.com` with no `x-ratelimit-*` headers,
//! `…/releases/latest/download/{name}` answers a 302 naming the newest tag
//! ([`head_no_redirect`]), and a missing asset or a private repo answers 404. The metered
//! `api.github.com` is on no lane, but failures are still classified per host: a 403 is a
//! rate limit on the API host and a blocked host on the web one, whose only throttle is 429.
//!
//! Every request is hardened against a hostile curlrc and a server-controlled URL: `-q`
//! first, the scrubbed config-dir env, the `--` end-of-options marker, `/dev/null` stdin.
//!
//! A second lane fetches for the vendor-direct agents ([`vendor_get`],
//! [`vendor_content_length`], [`vendor_download_to`]): anonymous, https on every hop,
//! byte-capped, with curl's CA-trust overrides dropped, and the only lane that asks
//! conditionally.

use aterm_uds::spawnfd::Command;
use std::path::Path;

/// A classified transport failure. The web lane ([`head_no_redirect`]) reports what the
/// wire said or that it could not be read; the vendor lane ([`vendor_get`],
/// [`vendor_content_length`], [`vendor_download_to`]) adds its own verdicts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpError {
    /// curl itself failed (exit != 0): DNS, TLS, timeout, or a refused spawn.
    Transport(String),
    /// The `-w`-appended status trailer was not a number: a proxy/portal mangled the
    /// response. Carries the whole message.
    Malformed(String),
    /// A status the vendor lane ([`vendor_get`], [`vendor_content_length`],
    /// [`vendor_download_to`]) does not accept.
    VendorStatus { code: u16, url: String },
    /// A vendor-lane verdict about the request or the document, returned on the first
    /// attempt: a URL, cap or ETag refused before spawning, a response over its cap, or a
    /// curl refusal that recurs on every attempt (a hop off https, too many redirects, a
    /// certificate that does not verify). Never `Transport`, which callers read as offline.
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
/// Windows ships `curl.exe` on `PATH` (since Win10 1803) with no fixed location to pin, and
/// `Command::new` finds only real executables, never PowerShell's `curl` alias.
#[cfg(not(windows))]
const CURL_BIN: &str = "/usr/bin/curl";

#[cfg(windows)]
const CURL_BIN: &str = "curl.exe";

/// `ATERM_DEV_CURL` — a DEVELOPMENT SEAM (`aterm_types::dev_seam!`): the program a
/// development build runs in place of [`CURL_BIN`], so a test can count the curl processes
/// a lane starts and script what each answers (atpkg's first-round link probe). A shipped
/// binary does not read it, and a child shell never inherits it
/// (`aterm_types::domain::ENV_DENY_VARS`).
pub const CURL_SEAM: &str = "ATERM_DEV_CURL";

/// The curl every lane spawns: [`CURL_BIN`], or a development build's [`CURL_SEAM`].
fn curl_bin() -> std::ffi::OsString {
    aterm_types::dev_seam!(CURL_SEAM)
        .filter(|seam| !seam.is_empty())
        .unwrap_or_else(|| CURL_BIN.into())
}

/// Build curl's full parameter list (everything after the binary name): `-q` first, the
/// caller's options, the fixed User-Agent, and last `--` and the URL. Ordering invariants:
///
/// * `-q` is the very first parameter (see [`curl_command`]).
/// * `--` comes after every option: everything after it is a URL to curl, so a misplaced
///   marker turns the remaining options into bogus URLs.
/// * `--` immediately precedes the URL, the one server-influenced string, so a leading-dash
///   value (`-K/tmp/evil`) parses as a URL, never an option.
// Skip: Vec growth (`extend`); drop this once Trust proves generic `extend`/`collect` total.
#[cfg_attr(trust_verify, trust::skip)]
fn curl_argv(args: &[&str], url: &str) -> Vec<String> {
    // A pre-size hint only, clamped so `+ 7` and the allocation are provably panic-free for
    // any abstract `args` (callers pass fixed lists of at most 19 items).
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

/// Build the argv-and-env part of the curl process for every lane; [`curl_fetch`] adds the
/// stdio. [`aterm_uds::spawnfd::command`] gives curl.exe a windowless console when the
/// caller (the window) has none.
fn curl_command(args: &[&str], url: &str) -> Command {
    let mut command = aterm_uds::spawnfd::command(curl_bin());
    command
        // `-q` first (curl_argv puts it there): otherwise curl reads ~/.curlrc (or
        // $CURL_HOME / $XDG_CONFIG_HOME), where a hostile `insecure` + `proxy = …` could read
        // or rewrite every byte. The config-dir variables are scrubbed too.
        .args(curl_argv(args, url))
        .env_remove("CURL_HOME")
        .env_remove("XDG_CONFIG_HOME");
    command
}

/// Run curl against `url` with extra `args`, anonymously, with `/dev/null` for stdin. Callers
/// never put `--` in `args`: [`curl_argv`] places it before the URL, after every option.
// Skip: same audited display-lossy Err-path class as `head_no_redirect_with`.
#[cfg_attr(trust_verify, trust::skip)]
fn curl_fetch(args: &[&str], url: &str) -> Result<std::process::Output, String> {
    use aterm_uds::spawnfd::Stdio;
    curl_command(args, url)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("spawn curl: {e}"))
}

/// How many times a request whose body is captured from curl's stdout is attempted (one try
/// plus two retries, as `--retry 2` would spend).
///
/// The retry is the subprocess, never curl's `--retry`, on every stdout-captured lane: curl
/// truncates only a file sink between attempts, so on a pipe a curl-level retry concatenates
/// the failed attempt's bytes before the good ones while `-w` writes the status trailer once,
/// and a recovered blip reads as a malformed answer (curl 8.7.1).
const CURL_ATTEMPTS: u32 = 3;

/// How long this process holds a link down once a request spent every attempt reaching no
/// host ([`attempt_again`]): a pass's lanes ask within seconds of one another, and a verdict
/// older than this is asked afresh, retries and all.
const LINK_DOWN_HOLDS: std::time::Duration = std::time::Duration::from_secs(30);

/// When a request in this process last spent its attempts on a link that reached no host,
/// until a request reaches one: ONE offline verdict for every lane of the process, so a
/// second request on a dead link — the set completion's resolve after the pass's own, on an
/// offline first launch — is answered by its first attempt instead of paying the same backoff
/// again ([`attempt_again`]).
static LINK_DOWN_SINCE: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);

/// What an attempt says of the link ([`link_after`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Link {
    /// It reached no host, with nothing left to try: the link is down.
    Down,
    /// A host answered, whatever it said: the link is up.
    Reached,
    /// Neither, or not yet.
    Unknown,
}

/// How far an attempt's connection got, as its `-w` trailer's `%{time_appconnect}` says
/// ([`handshake_in`]): every lane is https, so a TLS handshake that finished is a host that
/// answered, and none is a connection that never got past the name, the route or a proxy's
/// tunnel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Handshake {
    /// The TLS handshake finished.
    Done,
    /// It never did.
    NotDone,
    /// The run carries no trailer that says (a byte lane, whose stdout is the body).
    Unknown,
}

/// What the LAST token of a `-w` trailer — `%{time_appconnect}`, seconds — says of the
/// handshake: zero is none, a positive time one that finished, anything else unknown (a
/// trailer that ends on another field).
fn handshake_in(trailer: &str) -> Handshake {
    match trailer
        .split_whitespace()
        .last()
        .and_then(|t| t.parse::<f64>().ok())
    {
        Some(secs) if secs > 0.0 => Handshake::Done,
        Some(0.0) => Handshake::NotDone,
        _ => Handshake::Unknown,
    }
}

/// Whether curl's `exit` says it reached no host at all: the proxy or the host name did not
/// resolve (5, 6), or no route led to it, or nothing answered the connect (7) — or the
/// connect bound expired before any TLS handshake finished (28, [`Handshake::NotDone`]): a
/// name that never resolves, a dropped SYN, a proxy that never opens the tunnel. A second
/// try a moment later meets the same; a transfer that timed out after its handshake, a
/// broken transfer or a refused handshake does not say so, and keeps its honest retries.
fn reached_no_host(exit: Option<i32>, handshake: Handshake) -> bool {
    matches!(exit, Some(5..=7)) || (exit == Some(28) && handshake == Handshake::NotDone)
}

/// What attempt `attempt` of a request's `attempts`, ended with curl's `exit` after
/// `handshake`, says of the link, and whether another attempt is worth its backoff —
/// `known_down` when this process already holds the link down. Pure: [`attempt_again`]
/// keeps the verdict. An attempt that reached no host retries only while the link is not
/// known down, and holds it down once a request has spent every one of its retries on it (a
/// one-attempt hint never says so on its own); one a host answered (a status, `-f`'s exit
/// 22) brings it back up; every other failure retries as it always did.
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

/// [`link_after`] over this process's verdict ([`LINK_DOWN_SINCE`]), which it updates: whether
/// a request makes another attempt after attempt `attempt` of its `attempts` ended with
/// `exit` after `handshake`.
// Skip: a process-global verdict behind a std Mutex and the monotonic clock (absent std
// bodies); the decision itself is the pure `link_after`.
#[cfg_attr(trust_verify, trust::skip)]
fn attempt_again(exit: Option<i32>, handshake: Handshake, attempt: u32, attempts: u32) -> bool {
    let mut since = LINK_DOWN_SINCE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let known_down = since.is_some_and(|at| at.elapsed() <= LINK_DOWN_HOLDS);
    let (link, again) = link_after(exit, handshake, attempt, attempts, known_down);
    match link {
        Link::Down => *since = Some(std::time::Instant::now()),
        Link::Reached => *since = None,
        Link::Unknown => {}
    }
    again
}

/// Hold the link down from now ([`LINK_DOWN_SINCE`]), for a caller that saw every request of
/// a round it asked at once reach no host on its first attempt ([`head_no_redirect_first`]):
/// one verdict for the whole round, so none of them, and no request after them, backs off
/// and retries a link that is not there. Any host's answer clears it, as ever.
// Skip: the same process-global verdict as `attempt_again`.
#[cfg_attr(trust_verify, trust::skip)]
pub fn note_link_down() {
    *LINK_DOWN_SINCE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(std::time::Instant::now());
}

/// The pause before attempt `attempt` of a request: curl's own `--retry` backoff, kept
/// by every lane that retries in-process — nothing before the first attempt, 1 s before
/// the second, 2 s before the third.
///
/// A development build under the fast-timers seam
/// ([`aterm_types::domain::ENV_DEV_FAST_TIMERS`], `dev_seam!`) waits a thousandth of it,
/// so a test that counts a request's attempts — a child it starts with the seam set —
/// does not sit out the seconds between them. A shipped build never reads the seam.
fn backoff_before(attempt: u32) -> std::time::Duration {
    let production = match attempt {
        0 | 1 => return std::time::Duration::ZERO,
        n => std::time::Duration::from_secs(1 << (n - 2)),
    };
    if aterm_types::dev_seam!(aterm_types::domain::ENV_DEV_FAST_TIMERS).is_some() {
        production / 1000
    } else {
        production
    }
}

/// Sleep out [`backoff_before`] `attempt`.
fn back_off_before(attempt: u32) {
    let pause = backoff_before(attempt);
    if !pause.is_zero() {
        std::thread::sleep(pause);
    }
}

/// Whether an HTTP status is worth another in-process attempt: the transient server-side set
/// curl calls retryable (`--retry`), minus 429, where a retry without honouring `Retry-After`
/// only hits a throttle already refusing; every caller backs off to its next cycle instead.
fn transient_status(code: &str) -> bool {
    matches!(code, "408" | "500" | "502" | "503" | "504")
}

/// What a redirect-refusing HEAD came back with: the status and the `Location` header, if
/// any. Unclassified: which answer is a verdict is the caller's business ([`crate::pointer`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadAnswer {
    /// The HTTP status of the first hop (no redirect is followed).
    pub code: u16,
    /// The `Location` header of that hop, trimmed, if present. Server-controlled text:
    /// the caller validates it under a strict predicate before deriving anything.
    pub location: Option<String>,
}

/// The option list for [`head_no_redirect`]. `-I` asks for headers only; `--max-redirs 0`
/// stops at the first hop (pinning the intent against a later `-L`); headers and the `-w`
/// status trailer land on stdout. No `-f`: a 404 is an answer here. No `--retry`: see
/// [`CURL_ATTEMPTS`].
fn head_args() -> [&'static str; 10] {
    head_args_with_timeout("30")
}

fn head_args_with_timeout(timeout_secs: &'static str) -> [&'static str; 10] {
    [
        "-sS",
        "-I",
        "--max-redirs",
        "0",
        // A black-holed TCP/TLS setup fails at the connect bound, not at the wall clock.
        "--connect-timeout",
        SMALL_CONNECT_SECS,
        "--max-time",
        timeout_secs,
        // The status, then how far the connection got ([`handshake_in`]).
        "-w",
        "\n%{http_code} %{time_appconnect}",
    ]
}

/// The connect bound (DNS, TCP and TLS) of a HEAD or a small-document GET, in seconds.
/// Without it a dropped SYN or a half-up VPN holds each attempt to its whole `--max-time`, so
/// a pass's first request spends 93 s (three 30 s attempts plus the backoff) before it calls
/// the machine offline; bounded, it takes 18 s. A host that answers connects in well under a
/// second, and each attempt keeps the full wall clock for its transfer.
const SMALL_CONNECT_SECS: &str = "5";

/// The status and `Location` of one hop of `url`, anonymously, without following it: the
/// whole cost of a steady-state check on the web lane. No credential is accepted (`github.com`
/// must never be shown one), the scheme must be `https`, and a transport failure or transient
/// 5xx is retried with a fresh pipe per attempt ([`CURL_ATTEMPTS`]).
// Skip: same audited display-lossy Err-path class as `head_no_redirect_with`.
#[cfg_attr(trust_verify, trust::skip)]
pub fn head_no_redirect(url: &str) -> Result<HeadAnswer, HttpError> {
    head_no_redirect_with(url, &head_args(), CURL_ATTEMPTS)
}

/// A HEAD for a background discovery hint: one try with a five-second deadline, since the
/// full signed update has its own retries.
// Skip: same audited display-lossy Err-path class as `head_no_redirect`.
#[cfg_attr(trust_verify, trust::skip)]
pub fn head_no_redirect_quick(url: &str) -> Result<HeadAnswer, HttpError> {
    head_no_redirect_with(url, &head_args_with_timeout("5"), 1)
}

// Skip: response-text handling — from_utf8_lossy over curl output (classification
// only) and the trailing-status split arithmetic, whose bounds ride the lossy Cow
// (unmodeled), in `head_attempt` below. Every malformed shape returns Err (fail-closed).
// Shared by the normal request, the short hint and a round's first attempts so all use one
// audited response parser; every other `Skip: … display-lossy Err-path class` note in
// this module names this one.
#[cfg_attr(trust_verify, trust::skip)]
fn head_no_redirect_with(url: &str, args: &[&str], attempts: u32) -> Result<HeadAnswer, HttpError> {
    require_https_url(url).map_err(HttpError::Transport)?;
    head_attempts(url, args, 1, attempts)
}

/// What the first attempt of a [`head_no_redirect`] came to, apart from its retries
/// ([`head_no_redirect_again`]), so a caller asking several HEADs at once reads the link
/// from every first attempt before any of them backs off.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FirstHead {
    /// Settled on that attempt: the answer, or a failure no retry changes (a link this
    /// process already holds down among them).
    Settled(Result<HeadAnswer, HttpError>),
    /// Another attempt may change it. `answer` is what it says if none follows; `unreached`
    /// when it reached no host ([`reached_no_host`]) — what a round reads its link from
    /// ([`note_link_down`]).
    Again {
        /// The attempt's own answer: a transient status, or the transport failure.
        answer: Result<HeadAnswer, HttpError>,
        /// It reached no host.
        unreached: bool,
    },
}

/// [`head_no_redirect`]'s first attempt alone.
// Skip: same audited display-lossy Err-path class as `head_no_redirect_with`.
#[cfg_attr(trust_verify, trust::skip)]
pub fn head_no_redirect_first(url: &str) -> FirstHead {
    match require_https_url(url) {
        Ok(()) => head_attempt(url, &head_args(), 1, CURL_ATTEMPTS),
        Err(e) => FirstHead::Settled(Err(HttpError::Transport(e))),
    }
}

/// [`head_no_redirect`]'s attempts after the first, each after its backoff: the rest of a
/// HEAD whose first attempt said [`FirstHead::Again`].
// Skip: same audited display-lossy Err-path class as `head_no_redirect_with`.
#[cfg_attr(trust_verify, trust::skip)]
pub fn head_no_redirect_again(url: &str) -> Result<HeadAnswer, HttpError> {
    require_https_url(url).map_err(HttpError::Transport)?;
    head_attempts(url, &head_args(), 2, CURL_ATTEMPTS)
}

/// Attempts `from..=attempts` of a HEAD of `url`, each after its backoff; the last settles.
// Skip: same audited display-lossy Err-path class as `head_no_redirect_with`.
#[cfg_attr(trust_verify, trust::skip)]
fn head_attempts(
    url: &str,
    args: &[&str],
    from: u32,
    attempts: u32,
) -> Result<HeadAnswer, HttpError> {
    let mut attempt = from;
    loop {
        back_off_before(attempt);
        match head_attempt(url, args, attempt, attempts) {
            FirstHead::Settled(answer) => return answer,
            FirstHead::Again { .. } => attempt += 1,
        }
    }
}

/// Attempt `attempt` of a HEAD's `attempts`: settled, or worth another. Never
/// [`FirstHead::Again`] on the last.
// Skip: same audited display-lossy Err-path class as `head_no_redirect_with`.
#[cfg_attr(trust_verify, trust::skip)]
fn head_attempt(url: &str, args: &[&str], attempt: u32, attempts: u32) -> FirstHead {
    let last = attempt >= attempts;
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
    let again = attempt_again(exit, handshake, attempt, attempts);
    if !out.status.success() {
        let answer = Err(HttpError::Transport(format!(
            "curl HEAD {} failed ({}): {}",
            url,
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )));
        return if again {
            FirstHead::Again {
                answer,
                unreached: reached_no_host(exit, handshake),
            }
        } else {
            FirstHead::Settled(answer)
        };
    }
    let code = trailer.split_whitespace().next().unwrap_or("");
    let Ok(numeric) = code.parse::<u16>() else {
        return FirstHead::Settled(Err(HttpError::Malformed(format!(
            "the release host answered HTTP {code} to HEAD {url}"
        ))));
    };
    let answer = Ok(HeadAnswer {
        code: numeric,
        location: location_header(headers),
    });
    if !last && transient_status(code) {
        FirstHead::Again {
            answer,
            unreached: false,
        }
    } else {
        FirstHead::Settled(answer)
    }
}

/// The last `Location:` header in a header capture, trimmed; `None` when absent or empty.
fn location_header(headers: &str) -> Option<String> {
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

/// Reject any asset URL that is not plain `https://…`: a `file://` or `ftp://` value would
/// turn the downloader into a local-file or SSRF read. With the `--` before the URL, this
/// keeps the one server-influenced curl input inert.
fn require_https_url(url: &str) -> Result<(), String> {
    if url.starts_with("https://") {
        Ok(())
    } else {
        Err(format!("refusing asset URL with non-https scheme: {url}"))
    }
}

/// The option list for [`download_bytes`], extracted so the flag set is assertable in
/// a unit test.
///
/// It carries NO `--retry`: these bytes are captured from curl's stdout, which curl
/// does not truncate between attempts ([`CURL_ATTEMPTS`]). `-f` narrows the
/// concatenation window (a 5xx writes zero body bytes before the retry fires), but a
/// `--max-time` that expires after partial bytes still lands two attempts' fragments in
/// one buffer — and the buffer is exactly what the Ed25519 check reads.
/// [`download_bytes`] retries the subprocess instead.
///
/// NOTE: no `-w "\n%{http_code}"` here (and none in [`download_resume_args`]). These carry
/// `-f`, so curl's exit status already reports a non-2xx, and appending the status to
/// stdout would CORRUPT the downloaded bytes — including the appcast the Ed25519
/// signature covers. Asset downloads therefore stay unclassified; every public/private
/// verdict is taken from the discovery step that always runs first — the evergreen
/// pointer's HEAD ([`head_no_redirect`]).
fn download_bytes_args(cap: &str) -> [&str; 11] {
    [
        "-fsSL",
        // Redirects (GitHub's 302 to object storage) may land only on https: `-L` alone
        // would also follow http/ftp, a MITM downgrade.
        "--proto-redir",
        "=https",
        "--connect-timeout",
        SMALL_CONNECT_SECS,
        "--max-time",
        "60",
        "--max-filesize",
        cap,
        "-H",
        "Accept: application/octet-stream",
        // `curl_argv` appends `--` before the URL; `require_https_url` checks its scheme.
    ]
}

/// Download a small asset's bytes (e.g. a manifest) into memory, capped at `max_filesize`
/// (curl aborts before reading past it). Retried here, not by curl, so the buffer the Ed25519
/// check reads always holds exactly one attempt's bytes.
// Skip: same audited display-lossy Err-path class as `head_no_redirect_with`.
#[cfg_attr(trust_verify, trust::skip)]
pub fn download_bytes(asset_url: &str, max_filesize: u64) -> Result<Vec<u8>, String> {
    require_https_url(asset_url)?;
    let cap = max_filesize.to_string();
    // Bounded: the final attempt returns on both arms.
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        back_off_before(attempt);
        let out = curl_fetch(&download_bytes_args(&cap), asset_url)?;
        // Its stdout is the body: no trailer says how far a failed connection got.
        let again = attempt_again(
            out.status.code(),
            Handshake::Unknown,
            attempt,
            CURL_ATTEMPTS,
        );
        if out.status.success() {
            return Ok(out.stdout);
        }
        // With `-f` every failure (HTTP error, timeout, DNS) is a non-zero exit.
        let stderr = String::from_utf8_lossy(&out.stderr);
        // `-f` folds a 429 or 403 into exit 22 with the status in curl's message; a rate
        // limit is named so the check lane defers instead of booking a `pipeline` failure,
        // and a web-host 403/404 is a verdict (`classify_asset_failure`).
        if let Some(verdict) = classify_asset_failure(asset_url, &stderr) {
            return Err(verdict);
        }
        if !again {
            return Err(format!(
                "curl asset download failed ({}): {}",
                out.status,
                stderr.trim()
            ));
        }
    }
}

/// The marker in a rate-limited asset fetch's error ([`download_error_is_rate_limit`]).
const RATE_LIMIT_ERROR_PREFIX: &str = "rate limited (HTTP ";

/// Whether a [`download_bytes`] / [`download_to`] error describes a GitHub rate limit
/// (HTTP 429 on either host, 403 on the API host) rather than a broken download.
#[must_use]
pub fn download_error_is_rate_limit(error: &str) -> bool {
    error.contains(RATE_LIMIT_ERROR_PREFIX)
}

/// The prefix of a web-host 404 error (`classify_asset_failure`), so a caller holding only the
/// string can tell a missing asset from a broken download.
const NOT_FOUND_ERROR_PREFIX: &str = "HTTP 404";

/// Whether a [`download_bytes`] / [`download_to`] error is the web host's 404 verdict: the
/// release exists but does not carry the asset, as a source-only channel head (a `vX.Y.0`
/// published before the app cut attached its appcast). The check lane books no `pipeline`
/// failure for it.
#[must_use]
pub fn download_error_is_not_found(error: &str) -> bool {
    error.starts_with(NOT_FOUND_ERROR_PREFIX)
}

/// Whether a [`download_to`] error is this machine failing to store the bytes it was served
/// (a full disk, a quota): curl's write error, exit 23, or exit 56 with the same words when
/// the write fails mid-receive (`Failure writing output to destination`, macOS curl 8.7.1).
/// The host answered, so the check lane books no pipeline or host failure for it.
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

/// Whether an HTTP status on `url` is GitHub telling this client to slow down, per host: 429
/// anywhere, 403 only on `api.github.com`. On `github.com` a 403 is a blocked host or
/// filtering proxy (an unpublished or private asset answers 404), and calling it a rate limit
/// would make the check lane defer on a host that will not change its mind.
fn rate_limit_shaped(url: &str, code: u16) -> bool {
    code == 429 || (code == 403 && crate::cdn::is_api_host(url))
}

/// The verdict a `-f` failure carries, if any: a rate limit on either host, or a web-host
/// 403/404. These are answers about this asset on this host, so the caller returns them on
/// the first attempt; `None` retries, then reports the curl message.
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

/// The HTTP status curl reports for a `-f` failure ("The requested URL returned
/// error: 429" — the exit is 22 for every 4xx/5xx, so the code lives in the text).
#[must_use]
fn curl_http_error_code(stderr: &str) -> Option<u16> {
    let idx = stderr.find("returned error: ")?;
    let rest = &stderr[idx + "returned error: ".len()..];
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// GitHub's per-release-asset ceiling, and the one number both the client's container
/// download cap (`aterm-update`) and the cutter's publish-size guard (`aterm-release`'s
/// `UPDATER_MAX_DMG_BYTES`) must use: a client cap below the cutter's would accept a manifest
/// whose payload it can never fetch (curl exit 56), read as a network failure that never
/// escalates.
pub const RELEASE_ASSET_DOWNLOAD_BOUND: u64 = 2_147_483_648;

// The bound must admit the ~775 MB batteries-included container (checked at compile time,
// in test builds).
#[cfg(test)]
const _: () = assert!(RELEASE_ASSET_DOWNLOAD_BOUND > 800_000_000);

/// The wall-clock backstop for a file-sink download, derived from the size cap: a fixed
/// 600 s would demand ~10 Mbit/s for a 775 MB container and fail a slower machine at the same
/// wall every cycle. The floor rate is a slow 64 KiB/s so this stays a backstop for a
/// transfer that trickles forever, not a stall detector (`--speed-limit` is that). Clamped to
/// 600 s..6 h.
fn download_max_time_secs(max_filesize: u64) -> u64 {
    (max_filesize / 65_536).clamp(600, 21_600)
}

/// Download an asset (e.g. a DMG) to a file, following the storage redirect, capped at
/// `max_filesize` bytes so a hostile or mis-pointed asset cannot fill the disk. The time
/// bound derives from the same cap (`download_max_time_secs`), paired with a stall detector.
///
/// The TIME bound is derived from that same cap (`download_max_time_secs`) and paired
/// with a stall detector, so a slow link finishes instead of dying at a fixed wall it
/// can never beat.
///
/// A stalled or cut attempt continues IN THIS CALL exactly as [`download_to_resumable`]'s
/// does — the same option list, stall bounds and retries — but into `dest` itself, and every
/// call begins afresh, cutting `dest` to nothing in place first: this lane's callers (the
/// app-container download, 26–29 MB, whose scratch dir is swept wholesale before every
/// attempt by design and whose progress row polls this very file; the Linux updater's
/// private temp file; the publisher's probe copy) own the file and remove it after a
/// failure, so nothing resumes across calls, and bytes that stood there before the call are
/// never continued. Before, a silent drop held this lane for three 120 s stalls on one dead
/// HTTP/2 connection (curl's own `--retry` reused it) and then failed the check.
// Skip: same audited display-lossy Err-path class as `head_no_redirect_with`.
#[cfg_attr(trust_verify, trust::skip)]
pub fn download_to(asset_url: &str, dest: &Path, max_filesize: u64) -> Result<(), String> {
    download_to_resumable_with(
        asset_url,
        dest,
        max_filesize,
        ResumeLane::Release,
        Sink::Dest,
    )
    .map_err(|e| e.to_string())
}

/// The `.part` sibling a resumable download writes into: the destination file name with
/// `.part` appended, never `Path::with_extension` (which would turn `trust-5520.tar.zst` into
/// `trust-5520.tar.part`), so it still names the asset and its build.
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
    /// What to pass as `--max-filesize`: the remaining allowance.
    remaining_cap: u64,
    /// Whether the existing prefix must be discarded first: it is at or past the total cap,
    /// so it can never become a valid artifact.
    discard: bool,
}

fn resume_plan(existing: u64, max_filesize: u64) -> ResumePlan {
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

/// Whether a failed attempt's prefix is worth keeping: only if the attempt moved the file
/// forward. The anti-wedge rule: a prefix that cannot be extended (a clobbered, now shorter
/// upstream object, curl 416; a server refusing ranges, curl 33; a corrupt local file) would
/// otherwise be retried from the same dead offset forever.
fn keep_partial(before: u64, after: u64) -> bool {
    after > before
}

/// Whether a failed attempt's prefix survives: [`keep_partial`]'s anti-wedge rule, except
/// after a web-host 403/404. That is the host saying it does not serve this URL (a blocked
/// host, a filtering proxy, an asset the release does not carry, a private repo); `--fail`
/// wrote no body and no range was consulted, so discarding the prefix protects nothing and
/// costs bytes when another attempt resumes it. Every other failure, including every status
/// on the API host, keeps the anti-wedge rule.
fn keep_partial_after_failure(url: &str, stderr: &str, before: u64, after: u64) -> bool {
    if keep_partial(before, after) {
        return true;
    }
    !crate::cdn::is_api_host(url) && matches!(curl_http_error_code(stderr), Some(403 | 404))
}

/// Whether a failed attempt died on the range itself: curl exit 33 (`CURLE_RANGE_ERROR`, the
/// server will not resume) or an HTTP 416 through `--fail` (the upstream object shrank or was
/// clobbered). The same prefix at the same offset can never succeed, but a fresh attempt from
/// 0 can, which is what makes the in-call fresh retry in [`download_to_resumable`] worth it.
fn range_refused(exit_code: Option<i32>, stderr: &str) -> bool {
    exit_code == Some(33) || curl_http_error_code(stderr) == Some(416)
}

/// How long a resumable attempt may run under 4 KiB/s before curl calls it stalled
/// (`--speed-time`), chosen per attempt from how the one before it went
/// ([`StallBound::after`]).
///
/// The bound is what a silent drop costs — a Wi-Fi switch, a NAT timeout or a sleeping peer
/// sends no reset, so nothing but this ends the attempt — and every later ALab download
/// waits behind it, since they take the link in turns. A stall resumes from the `.part`
/// ([`download_to_resumable`]), so ending one early costs a reconnect, never a byte.
/// Measured with curl 8.7.1 against a tunnel held open with no bytes: curl reports the
/// stall about six seconds after `--speed-time` (its speed is a five-second average, which
/// has to drain first) — 9.0 s at 3, 16.1 s at 10, 21.1 s at 15 — and at 64 kB/s a link
/// survived any silence up to 12 s at 8 and tripped at 13.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StallBound {
    /// 8 s: the stall is reported about 14 s after the last byte, where the historical 120 s
    /// took 126 s. A call's first attempt, and every attempt after one that beat 4 KiB/s.
    Quick,
    /// The historical 120 s, after an attempt that never beat 4 KiB/s on average: a proxy
    /// that holds or trickles a body while it scans it is not a dead link, and the quick
    /// bound would end every attempt it makes before its first real byte.
    Patient,
}

impl StallBound {
    /// The `--speed-time` value.
    fn secs(self) -> &'static str {
        match self {
            Self::Quick => "8",
            Self::Patient => "120",
        }
    }

    /// The bound for the attempt after one that moved `moved` bytes in `took`: quick while
    /// the attempt beat curl's 4 KiB/s floor on average, stalled minutes included; patient
    /// once it did not — a body held back by a scanning proxy, a trickle, or a link that
    /// moved nothing.
    fn after(moved: u64, took: std::time::Duration) -> Self {
        if u128::from(moved) * 1000 >= 4096 * took.as_millis() {
            Self::Quick
        } else {
            Self::Patient
        }
    }
}

/// curl's TCP keepalive for a resumable attempt (`--keepalive-time`, seconds idle before the
/// first probe and between probes). Its default is 60 s, and a reset that reached the host
/// while curl sat in a read was reported only at the first probe — 60 s after the cut, in
/// 3 of 24 dropped-transfer replays. A probe is sent only on a connection idle this long,
/// so a transfer that is moving sends none. Two seconds, not five: through a proxy that cut
/// with a reset, plain curl learned of the cut late in 6 of 12 tries at 5 (5.7–6.0 s after
/// it) and in 2 of 12 at 2 (2.6 s); every other try saw it in 0.6 s. Two keeps a dead peer's
/// keepalive verdict (the first probe plus macOS's eight unanswered ones, 18 s) past the
/// quick stall report (about 14 s), so a keepalive never ends an attempt the stall bound would
/// have let run.
const RESUME_KEEPALIVE_SECS: &str = "2";

/// The option list for one attempt of a FILE download ([`download_to_resumable`],
/// [`download_to`]), extracted so the flag set is assertable in a unit test. Three of its
/// flags differ from the list this lane carried before, each because a stalled attempt now
/// continues from the bytes it wrote and so costs a reconnect, never those bytes:
///
/// * no `--retry`. curl truncates its `-o` file to where the attempt began before it retries
///   (measured, curl 8.7.1: a stall at 1.5 of 2 MB went back for byte 0), so its own retry
///   threw away everything a stalled attempt had moved — 700 MB at 95 % of a fresh `trust`.
///   Over HTTP/2 it also retried on the SAME connection: after a silent drop all three
///   attempts stalled on the one dead tunnel (371 s in a first-launch replay) and left the
///   `.part` empty. [`download_to_resumable`] makes the next attempt itself, a new curl and a
///   new connection, from the `.part`.
/// * `--speed-time` is `stall` ([`StallBound`]) rather than 120 s.
/// * `--keepalive-time` is [`RESUME_KEEPALIVE_SECS`] rather than curl's 60 s.
///
/// `offset == None` adds no range — a fresh transfer carries no `Range` header for a server
/// to mishandle.
///
/// NOTE: no `-w` trailer: the body goes to `-o`, and curl's exit status already reports a
/// non-2xx under `-f` (see `download_bytes_args`).
fn download_resume_args<'a>(
    cap: &'a str,
    max_time: &'a str,
    dest: &'a str,
    offset: Option<&'a str>,
    stall: StallBound,
) -> Vec<&'a str> {
    let mut args: Vec<&str> = vec![
        // `-s`: without it curl writes its PROGRESS METER to stderr, which this lane
        // captures into the failure message, burying the cause. `-S` keeps real
        // errors, the pairing `download_bytes_args` uses.
        "-fsSL",
        // https-only redirects, as in `download_bytes_args`.
        "--proto-redir",
        "=https",
        // Bound the CONNECT, not the transfer — a black-holed TCP/TLS setup must
        // still fail fast.
        "--connect-timeout",
        "30",
        // A stall is under 4 KiB/s for `stall` — this, not the wall clock, is what tells a
        // dead link from a merely slow one, and it exits 28 just like a `--max-time`
        // expiry, so the error text and the `pipeline`-class health accounting are
        // unchanged for genuinely dead links.
        "--speed-limit",
        "4096",
        "--speed-time",
        stall.secs(),
        "--keepalive-time",
        RESUME_KEEPALIVE_SECS,
        "--max-time",
        max_time,
        "--max-filesize",
        cap,
        "-H",
        "Accept: application/octet-stream",
        "-o",
        dest,
        // The `--` guard before the server-controlled asset URL is appended by
        // `curl_argv` (see `download_bytes_args`); `require_https_url` rejects
        // non-https schemes.
    ];
    if let Some(offset) = offset {
        // An explicit offset, not `-C -`, which would have curl size the local file itself:
        // a second source of truth for the number the cap was computed from.
        args.push("--continue-at");
        args.push(offset);
    }
    args
}

/// The `https`-only pin for the VENDOR lane, prepended to [`download_resume_args`].
///
/// `--proto-redir =https` (already on every lane) constrains where a REDIRECT may land;
/// `--proto =https` constrains the INITIAL request too. `require_https_url` checks the
/// URL's spelling, and this makes curl enforce the same rule on the wire — a second,
/// independent gate for the one lane whose URL is a vendor's, not GitHub's. The release
/// lanes are deliberately untouched (the plain argv).
const HTTPS_ONLY_ARGS: [&str; 2] = ["--proto", "=https"];

/// [`download_resume_args`] with the initial-request scheme pinned to `https` as well.
fn download_resume_args_https_only<'a>(
    cap: &'a str,
    max_time: &'a str,
    dest: &'a str,
    offset: Option<&'a str>,
    stall: StallBound,
) -> Vec<&'a str> {
    let mut args: Vec<&str> = HTTPS_ONLY_ARGS.to_vec();
    args.extend(download_resume_args(cap, max_time, dest, offset, stall));
    args
}

/// What a resumable download does after a failed attempt that carried no verdict about the
/// URL ([`next_attempt`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NextAttempt {
    /// Continue from the `.part` at once: the attempt moved it.
    Resume,
    /// The same offset again after curl's own backoff: the attempt moved nothing, and its
    /// failure is one curl's `--retry` retried.
    Retry,
    /// Return the failure.
    Stop,
}

/// [`NextAttempt`] for an attempt that ended with curl's `exit` and `stderr`, having moved
/// the `.part` or not ([`keep_partial`]).
///
/// An attempt that MOVED it resumes whatever ended it — a stall, a reset, a closed
/// connection — except a failure no reconnect changes: the size cap, or this machine
/// failing to store the bytes ([`download_error_is_local_write`]). One that moved nothing
/// retries only what curl's own `--retry` retried here before this lane took the retry
/// over: a timeout (28), a name that did not resolve (5, 6) and a transient status
/// ([`transient_status`]; 429 is a verdict the caller reads first, as on every lane).
fn next_attempt(exit: Option<i32>, stderr: &str, moved: bool) -> NextAttempt {
    if filesize_exceeded(exit, stderr) || download_error_is_local_write(stderr) {
        NextAttempt::Stop
    } else if moved {
        NextAttempt::Resume
    } else if matches!(exit, Some(5 | 6 | 28))
        || curl_http_error_code(stderr).is_some_and(|code| transient_status(&code.to_string()))
    {
        NextAttempt::Retry
    } else {
        NextAttempt::Stop
    }
}

/// How many times one download continues its file after an attempt that moved less than
/// [`RESUME_EARNED_AT`] ([`resume_allowed`]): a host that cuts every attempt after a few
/// bytes is never asked in a loop.
const SHORT_RESUMES_PER_CALL: u32 = 16;

/// What an attempt must move for the resume after it to spend none of
/// [`SHORT_RESUMES_PER_CALL`] ([`resume_allowed`]). A link that keeps moving between its
/// hiccups — 64 kB/s with a silence past the quick stall bound every minute, a Wi-Fi that
/// drops every few minutes — keeps continuing, where a flat count of sixteen failed the
/// install on its seventeenth hiccup (a 745 MB archive at 64 kB/s takes over three hours, and
/// the stall bound it replaced never tripped on a silence under 120 s). 1 MiB is 16 s at the
/// 64 kB/s average the derived wall clock assumes ([`download_max_time_secs`]).
const RESUME_EARNED_AT: u64 = 1 << 20;

/// Whether a download continues its file once more ([`NextAttempt::Resume`]) after an
/// attempt that moved `moved` bytes, having resumed `short` times after attempts that moved
/// less than [`RESUME_EARNED_AT`] and `all` times in all, toward a cap of `cap` bytes. A
/// short attempt spends one of [`SHORT_RESUMES_PER_CALL`]; every resume spends one of
/// `SHORT_RESUMES_PER_CALL + cap / RESUME_EARNED_AT`, which bounds the call however the bytes
/// fall.
fn resume_allowed(moved: u64, short: u32, all: u32, cap: u64) -> bool {
    let budget = u64::from(SHORT_RESUMES_PER_CALL).saturating_add(cap / RESUME_EARNED_AT);
    u64::from(all) < budget && (moved >= RESUME_EARNED_AT || short < SHORT_RESUMES_PER_CALL)
}

/// Empty the file a download's attempts write, so the next attempt begins at byte 0: the
/// `.part` is removed, and `dest` ([`Sink::Dest`]) — its caller's file, made with the caller's
/// mode and flags (the Linux updater creates it `0600` with `O_NOFOLLOW`) — is cut to nothing
/// in place.
fn clear_sink(file: &Path, sink: Sink) {
    match sink {
        Sink::Part => {
            let _ = std::fs::remove_file(file);
        }
        Sink::Dest => {
            if let Ok(open) = std::fs::OpenOptions::new().write(true).open(file) {
                let _ = open.set_len(0);
            }
        }
    }
}

/// Download an asset to `dest`, RESUMABLY: bytes land in a sibling `<dest>.part` that
/// SURVIVES a failed attempt, and the next call continues from where the last one
/// stopped. On success the part is renamed onto `dest`, so `dest` only ever exists
/// complete.
///
/// A resumed body is not trusted for being resumed: the caller's sha256 gate over the
/// complete file (`atpkg`'s `verify_and_stage`) still rejects a clobbered upstream prefix, a
/// mis-resumed range or a server that ignored the range. The total cap is preserved by
/// subtracting the offset ([`resume_plan`]), and a failed attempt that made no progress
/// discards its prefix ([`keep_partial`]).
///
/// Without it, the cost of a transient stall is O(artifact size) × attempts. The
/// scaling variable is the signed artifact size, and the dominant shipped toolchain
/// member is 629,817,785 B — ~8.4 minutes at 10 Mbit/s. curl's stall detector fires at
/// under 4 KiB/s for 120 s, so a single Wi-Fi hiccup at 95 % discarded ~600 MB and the
/// next pass started at zero. That is the same failure shape `download_max_time_secs`
/// was written to fix, left half-fixed: the wall clock scales with the payload, but the
/// retry did not.
///
/// # Correctness is the signed digest's job, exactly as before
///
/// A resumed body is not trusted for being resumed. The caller's `sha256` gate over the
/// COMPLETE file (`atpkg`'s `verify_and_stage`, step 1) runs unchanged, so a prefix from
/// a clobbered upstream object, a mis-resumed range, or a server that ignored the range
/// and appended a whole second copy all fail there and are discarded — which costs
/// exactly what a failed download costs today, not a new failure mode. What resume can
/// never do is make a WRONG artifact acceptable.
///
/// The total-bytes cap is preserved by subtracting the offset ([`resume_plan`]); a
/// failed call that made no progress discards its prefix ([`keep_partial`]) so a dead
/// offset can never wedge the lane.
///
/// # A stalled or cut attempt resumes in-call, at once
///
/// A connection that dies silently — a Wi-Fi switch, a NAT timeout, a sleeping peer — ends
/// only at curl's stall bound, and every later ALab download waits behind it. So an
/// attempt is held to a quick bound unless the one before it never beat 4 KiB/s
/// ([`StallBound`]; a silent drop is reported about 14 s after the last byte, where the
/// historical bound took 126 s), probes an idle connection every 2 s, and an attempt that moved the `.part`
/// continues from it in this call, at once ([`next_attempt`]) — for as long as each attempt
/// moves at least [`RESUME_EARNED_AT`], [`SHORT_RESUMES_PER_CALL`] times after attempts that
/// moved less ([`resume_allowed`]). Once the call's elapsed time reaches the derived limit,
/// no further progressed resume is scheduled. An attempt already started still gets the full
/// per-attempt `--max-time`; the no-progress retries and the one range-refused fresh retry keep
/// their separate budgets. The elapsed check is not a deadline for the whole call. A slow link
/// loses nothing to the quick bound: a stall it trips costs one reconnect. curl's own `--retry` is gone
/// from this lane ([`download_resume_args`]): it went back to where the attempt began. An
/// attempt that moved nothing is retried as curl's `--retry` retried it, twice, 1 s and
/// 2 s apart, under the patient bound.
///
/// # A range-refused resume retries fresh ONCE, in-call
///
/// [`keep_partial`] alone would leave the clean restart to the NEXT call, a whole failed
/// pass away. When the failure names the range itself ([`range_refused`]: curl 33, or a
/// 416 because the upstream object shrank), this call discards the `.part` and retries
/// from offset 0 immediately — at most once per call, with the cap recomputed from the
/// FULL `max_filesize` and a fresh curl process (hence a fresh wall clock). Any other
/// failure keeps the plain rule, and the sha256 gate downstream is untouched either way.
///
/// # A verdict about the URL does not discard the prefix
///
/// A web-host 403/404 is the host answering about the URL, not about the bytes, so the
/// prefix is LEFT for whatever attempt resumes it ([`keep_partial_after_failure`]).
// Skip: same audited display-lossy Err-path class as `head_no_redirect_with`.
#[cfg_attr(trust_verify, trust::skip)]
pub fn download_to_resumable(
    asset_url: &str,
    dest: &Path,
    max_filesize: u64,
) -> Result<(), String> {
    download_to_resumable_with(
        asset_url,
        dest,
        max_filesize,
        ResumeLane::Release,
        Sink::Part,
    )
    .map_err(|e| e.to_string())
}

/// [`download_to_resumable`] for a vendor-hosted asset, with `--proto =https` beside the
/// redirect pin so neither the first hop nor any redirect can leave https.
// Skip: same audited display-lossy Err-path class as `head_no_redirect_with`.
#[cfg_attr(trust_verify, trust::skip)]
pub fn download_to_resumable_https_only(
    asset_url: &str,
    dest: &Path,
    max_filesize: u64,
) -> Result<(), String> {
    download_to_resumable_with(
        asset_url,
        dest,
        max_filesize,
        ResumeLane::HttpsOnly,
        Sink::Part,
    )
    .map_err(|e| e.to_string())
}

/// Which request and child a resumable download runs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ResumeLane {
    /// The release lanes: the plain argv and child.
    Release,
    /// A signed index row's vendor URL: `--proto =https` added to the argv.
    HttpsOnly,
    /// A vendor-direct payload: [`ResumeLane::HttpsOnly`]'s argv, run as
    /// [`vendor_command`], with [`vendor_download_verdict`] classifying a failure.
    VendorDirect,
}

/// Where a file download's bytes land ([`download_to_resumable_with`]).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Sink {
    /// `<dest>.part`, kept across calls for the next one to continue and renamed onto `dest`
    /// on success: the resumable lanes.
    Part,
    /// `dest` itself, cut to nothing in place as every call begins ([`clear_sink`]) and left
    /// as it stands after a failure for its caller to remove: [`download_to`].
    Dest,
}

/// The shared body of [`download_to_resumable`], [`download_to_resumable_https_only`],
/// [`vendor_download_to`] and [`download_to`]; `lane` selects the argv, the child and the
/// verdicts, and `sink` the file. Every error but a vendor-direct verdict is
/// [`HttpError::Transport`], whose text is the historical message.
// Skip: same audited display-lossy Err-path class as `head_no_redirect_with`.
#[cfg_attr(trust_verify, trust::skip)]
fn download_to_resumable_with(
    asset_url: &str,
    dest: &Path,
    max_filesize: u64,
    lane: ResumeLane,
    sink: Sink,
) -> Result<(), HttpError> {
    require_https_url(asset_url).map_err(HttpError::Transport)?;
    // The file the attempts write: the `.part`, or `dest` itself.
    let part = match sink {
        Sink::Part => part_path(dest)
            .ok_or_else(|| HttpError::Transport("destination has no file name".to_string()))?,
        Sink::Dest => dest.to_path_buf(),
    };
    let part_s = part
        .to_str()
        .ok_or_else(|| HttpError::Transport("non-UTF-8 destination path".to_string()))?;
    // `dest` is begun afresh by each call: cut it to nothing now, so bytes that stood there
    // before this call never read as an attempt's progress (an attempt that fails before its
    // first byte — a 503 under `-f` — leaves the file as it found it).
    if sink == Sink::Dest {
        clear_sink(&part, sink);
    }
    // TERMINATION: the loop goes around only through three arms, each bounded. The
    // range-refused arm runs at most ONCE per call (it sets this flag and empties the file,
    // so the iteration after it is a fresh, offset-0 attempt). The resume arm runs at most
    // `SHORT_RESUMES_PER_CALL + cap / RESUME_EARNED_AT` times (`resume_allowed`), and the
    // retry arm at most `CURL_ATTEMPTS - 1` times in a row (its count restarts only after
    // one of the other two).
    let mut retried_fresh = false;
    let mut resumes: u32 = 0;
    let mut short_resumes: u32 = 0;
    // Attempts in a row that moved nothing, the one just made included.
    let mut idle: u32 = 0;
    let mut stall = StallBound::Quick;
    // What the `.part` held when this call began, or when its fresh retry discarded it:
    // the anti-wedge rule asks whether the CALL moved it, whichever attempt did.
    let mut call_began: Option<u64> = None;
    let started = std::time::Instant::now();
    loop {
        let existing = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
        let plan = resume_plan(existing, max_filesize);
        let existing = if plan.discard {
            clear_sink(&part, sink);
            0
        } else {
            existing
        };
        let began = *call_began.get_or_insert(existing);
        // Both must outlive the argv, which borrows them as `&str`.
        let cap = plan.remaining_cap.to_string();
        // The wall clock derives from the full cap, not the remainder (it is a backstop
        // against a trickle), recomputed so a fresh retry gets a full budget.
        let max_time = download_max_time_secs(max_filesize).to_string();
        let offset_text = plan.offset.to_string();
        // `None` at offset 0: a fresh transfer carries no range.
        let offset = (plan.offset > 0).then_some(offset_text.as_str());
        let args = if lane == ResumeLane::Release {
            download_resume_args(&cap, &max_time, part_s, offset, stall)
        } else {
            download_resume_args_https_only(&cap, &max_time, part_s, offset, stall)
        };
        let attempt_began = std::time::Instant::now();
        let out = if lane == ResumeLane::VendorDirect {
            vendor_fetch(&args, asset_url)
        } else {
            curl_fetch(&args, asset_url)
        }
        .map_err(HttpError::Transport)?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            // A resumed attempt refused at the range itself: discard the prefix and go
            // around once from offset 0. Only a ranged attempt takes this arm, so the loop
            // terminates.
            if plan.offset > 0 && !retried_fresh && range_refused(out.status.code(), &stderr) {
                clear_sink(&part, sink);
                retried_fresh = true;
                call_began = Some(0);
                idle = 0;
                continue;
            }
            let after = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
            // A verdict about the URL or the document ends the call whatever the attempt
            // moved: the vendor lane's own first, then the per-host classification
            // `download_to` makes (a rate limit is named so the check lane can defer
            // instead of booking a broken pipeline).
            let verdict = if lane == ResumeLane::VendorDirect {
                vendor_download_verdict(out.status.code(), &stderr, asset_url, max_filesize)
            } else {
                None
            }
            .or_else(|| classify_asset_failure(asset_url, &stderr).map(HttpError::Transport));
            if verdict.is_none() {
                let moved = keep_partial(existing, after);
                let moved_bytes = after.saturating_sub(existing);
                // The next attempt's stall bound, from how this one went.
                stall = StallBound::after(moved_bytes, attempt_began.elapsed());
                match next_attempt(out.status.code(), &stderr, moved) {
                    // A stall or a cut after bytes moved: schedule another resume only while
                    // the resume budget and elapsed-time check allow. The check does not
                    // shorten an attempt already started: it gets the full `--max-time`.
                    NextAttempt::Resume
                        if resume_allowed(moved_bytes, short_resumes, resumes, max_filesize)
                            && started.elapsed().as_secs()
                                < download_max_time_secs(max_filesize) =>
                    {
                        resumes += 1;
                        if moved_bytes < RESUME_EARNED_AT {
                            short_resumes += 1;
                        }
                        idle = 0;
                        continue;
                    }
                    NextAttempt::Retry => {
                        idle += 1;
                        // Handshake unknown: the `-o` lanes carry no trailer. A name that
                        // did not resolve keeps its retries only while the link is not
                        // already known down.
                        if attempt_again(out.status.code(), Handshake::Unknown, idle, CURL_ATTEMPTS)
                        {
                            // `idle` attempts in a row moved nothing: the next is attempt
                            // `idle + 1` of the run, after its backoff.
                            back_off_before(idle + 1);
                            continue;
                        }
                    }
                    NextAttempt::Resume | NextAttempt::Stop => {}
                }
            }
            // The anti-wedge discard over the whole CALL, EXCEPT when the failure was an
            // answer about the URL (a web-host 403/404): that prefix is still a valid
            // prefix for the other host serving the same signed object, and the caller
            // tries it next. `dest` is its caller's to remove.
            if sink == Sink::Part && !keep_partial_after_failure(asset_url, &stderr, began, after) {
                let _ = std::fs::remove_file(&part);
            }
            if let Some(verdict) = verdict {
                return Err(verdict);
            }
            return Err(HttpError::Transport(format!(
                "curl download failed ({}): {}",
                out.status,
                stderr.trim()
            )));
        }
        if sink == Sink::Dest {
            return Ok(());
        }
        // Only a curl success promotes the part, so `dest` never holds a prefix.
        return std::fs::rename(&part, dest).map_err(|e| {
            let _ = std::fs::remove_file(&part);
            HttpError::Transport(format!("finalize download: {e}"))
        });
    }
}

// The vendor document lane (vendor-direct agents).

/// What [`vendor_get`] came back with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VendorResponse {
    /// HTTP 200: the whole body (at most the cap), the response's ETag when it carried a
    /// well-formed one, and the URL curl ended on after redirects.
    Body {
        bytes: Vec<u8>,
        etag: Option<String>,
        effective_url: String,
    },
    /// HTTP 304 to a conditional request: unchanged. `etag` is the validator to keep: the
    /// 304's own ETag, or the one sent when the 304 carried none (RFC 9110 says only
    /// SHOULD), so the next poll stays conditional.
    NotModified { etag: String },
}

/// The curl child environment a vendor request drops: the curlrc redirections, plus
/// everything that changes which CAs curl trusts, including the TLS backend switch
/// (`CURL_SSL_BACKEND=secure-transport` makes a multi-backend curl trust the keychain's
/// user-added roots). A vendor document's TLS session is the evidence for its digest, so
/// nothing ambient may vouch for it.
const VENDOR_ENV_SCRUB: [&str; 6] = [
    "CURL_HOME",
    "XDG_CONFIG_HOME",
    "CURL_CA_BUNDLE",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "CURL_SSL_BACKEND",
];

/// How many redirects a vendor request may follow (real chains are one hop or none; curl's
/// default is 50).
const VENDOR_MAX_REDIRS: &str = "5";

/// The longest ETag this lane stores or sends back.
const ETAG_MAX: usize = 256;

/// The smallest `--max-filesize` a vendor GET hands curl. curl applies it to every hop's
/// Content-Length, redirects included (a 150-byte 302 body under a 64-byte cap is exit 63),
/// so curl's cap is a backstop and the exact bound is [`read_capped`]'s, on the final body.
const VENDOR_CURL_CAP_FLOOR: u64 = 16 * 1024;

/// The `--max-filesize` for a vendor GET whose document cap is `cap`.
fn vendor_curl_cap(cap: u64) -> u64 {
    cap.max(VENDOR_CURL_CAP_FLOOR)
}

/// Whether a failed curl run hit `--max-filesize`: exit 63 when the size is known up front,
/// exit 56 with this message when crossed mid-transfer (curl 8.7.1). A verdict, never retried.
fn filesize_exceeded(exit: Option<i32>, stderr: &str) -> bool {
    exit == Some(63) || stderr.contains("Maximum file size exceeded")
}

/// The curl exits that recur on every attempt and are about the host, not the network: 1 (a
/// hop that is not https), 47 (too many redirects), 51/60 (a certificate that does not
/// verify). A verdict, never retried and never reported as a transport failure.
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

/// Whether `etag` may ride an `If-None-Match` header: 1..=[`ETAG_MAX`] bytes of visible
/// ASCII (RFC 9110 `entity-tag` has no space or control byte), so a stored value can
/// never inject a header line.
fn etag_ok(etag: &str) -> bool {
    !etag.is_empty() && etag.len() <= ETAG_MAX && etag.bytes().all(|b| (0x21..=0x7e).contains(&b))
}

/// The value of header `name` in the last block of a header capture (a redirect chain writes
/// one block per hop), trimmed; `None` when absent, empty, or given twice with different
/// values in that block.
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

/// The response ETag from a vendor header capture, only when [`etag_ok`] admits it.
fn etag_header(headers: &str) -> Option<String> {
    last_block_header(headers, "etag").filter(|e| etag_ok(e))
}

/// The final hop's `Content-Length`: all ASCII digits, and never zero — the value is used
/// as a `--max-filesize` cap, where curl reads `0` as "no limit".
fn content_length_header(headers: &str) -> Option<u64> {
    let value = last_block_header(headers, "content-length")?;
    if !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    value.parse::<u64>().ok().filter(|n| *n > 0)
}

/// Split the vendor GET's `-w` trailer (`<http_code> <url_effective> <time_appconnect>`, the
/// last read by [`handshake_in`]). `None` unless the code is numeric, the URL is https with
/// no whitespace or control byte, and anything after it is the handshake's time.
fn vendor_trailer(stdout: &str) -> Option<(u16, String)> {
    let (code, rest) = stdout.trim().split_once(' ')?;
    let url = match rest.rsplit_once(' ') {
        Some((url, secs)) if secs.parse::<f64>().is_ok() => url,
        _ => rest,
    };
    let code = code.parse::<u16>().ok()?;
    if !url.starts_with("https://") || url.bytes().any(|b| b <= b' ' || b == 0x7f) {
        return None;
    }
    Some((code, url.to_string()))
}

/// One finished vendor-lane curl run: its exit code (`None` when a signal ended it),
/// stdout and stderr.
struct CurlRun<'a> {
    exit: Option<i32>,
    stdout: &'a str,
    stderr: &'a str,
}

impl CurlRun<'_> {
    /// The transport failure a run that exhausted its attempts reports.
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
    /// Another attempt may cure it. Never answered when the attempt was the last.
    Retry,
    /// The answer, or a verdict no further attempt changes.
    Done(Result<T, HttpError>),
}

/// One vendor GET attempt's decision, from its curl run and this attempt's header dump;
/// `body` reads the capped body, called only for a 200. `sent_etag` is the `If-None-Match`
/// the request carried: a 304 to an unconditional request is a protocol violation.
fn vendor_get_step(
    run: &CurlRun<'_>,
    headers: &str,
    last: bool,
    url: &str,
    cap: u64,
    sent_etag: Option<&str>,
    body: impl FnOnce() -> Result<Vec<u8>, HttpError>,
) -> Step<VendorResponse> {
    if run.exit != Some(0) {
        if filesize_exceeded(run.exit, run.stderr) {
            return Step::Done(Err(HttpError::VendorRefused(format!(
                "vendor document {url} exceeds its {cap}-byte cap"
            ))));
        }
        if let Some(refusal) = vendor_curl_refusal(run.exit, run.stderr, url) {
            return Step::Done(Err(refusal));
        }
        return if last {
            Step::Done(Err(run.failure("GET", url)))
        } else {
            Step::Retry
        };
    }
    let Some((code, effective_url)) = vendor_trailer(run.stdout) else {
        return Step::Done(Err(HttpError::Malformed(format!(
            "the vendor host answered an unreadable status for {url}: {}",
            run.stdout.trim()
        ))));
    };
    if !last && transient_status(&code.to_string()) {
        return Step::Retry;
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

/// One vendor HEAD attempt's decision. stdout is every hop's headers, then a newline and
/// the final status (the `-w` trailer); only the last hop's `Content-Length` counts.
fn vendor_head_step(run: &CurlRun<'_>, last: bool, url: &str) -> Step<u64> {
    if run.exit != Some(0) {
        if let Some(refusal) = vendor_curl_refusal(run.exit, run.stderr, url) {
            return Step::Done(Err(refusal));
        }
        return if last {
            Step::Done(Err(run.failure("HEAD", url)))
        } else {
            Step::Retry
        };
    }
    let (headers, trailer) = match run.stdout.rfind('\n') {
        Some(i) => (&run.stdout[..i], run.stdout[i + 1..].trim()),
        None => ("", run.stdout.trim()),
    };
    let code = trailer.split_whitespace().next().unwrap_or("");
    if !last && transient_status(code) {
        return Step::Retry;
    }
    let Ok(code) = code.parse::<u16>() else {
        return Step::Done(Err(HttpError::Malformed(format!(
            "the vendor host answered HTTP {code} to HEAD {url}"
        ))));
    };
    if code != 200 {
        return Step::Done(Err(HttpError::VendorStatus {
            code,
            url: url.to_string(),
        }));
    }
    Step::Done(content_length_header(headers).ok_or_else(|| {
        HttpError::Malformed(format!(
            "the vendor host sent no usable Content-Length for {url}"
        ))
    }))
}

/// The verdict a failed vendor-direct payload run carries, when it is one: over the cap,
/// a [`vendor_curl_refusal`], or a `-f` status (exit 22) that no retry changes — anything
/// but 408, 429 and 5xx. `None` leaves it a transport failure.
fn vendor_download_verdict(
    exit: Option<i32>,
    stderr: &str,
    url: &str,
    cap: u64,
) -> Option<HttpError> {
    if filesize_exceeded(exit, stderr) {
        return Some(HttpError::VendorRefused(format!(
            "vendor payload {url} exceeds its {cap}-byte cap"
        )));
    }
    if let Some(refusal) = vendor_curl_refusal(exit, stderr, url) {
        return Some(refusal);
    }
    let code = curl_http_error_code(stderr).filter(|_| exit == Some(22))?;
    (!matches!(code, 408 | 429 | 500..=599)).then(|| HttpError::VendorStatus {
        code,
        url: url.to_string(),
    })
}

/// A vendor GET's time budget: curl's connect and whole-transfer bounds (seconds), and how
/// many attempts a network failure gets.
#[derive(Clone, Copy, Debug)]
struct VendorBounds {
    connect_secs: &'static str,
    max_secs: &'static str,
    attempts: u32,
}

/// A document the lane needs: its whole transfer patient, and retried. Its connect is bounded
/// as the web lane's small requests are ([`SMALL_CONNECT_SECS`]): a vendor host that answers
/// connects in well under a second, and a name that never resolved held each of a pass's
/// document reads for 93 s (three 30 s connects and the sleeps between them) — on a first
/// run, the whole announcement behind one of them. Bounded, three attempts take 18 s, and a
/// connect that never finished holds the link down for the rest ([`reached_no_host`]).
const LANE_BOUNDS: VendorBounds = VendorBounds {
    connect_secs: SMALL_CONNECT_SECS,
    max_secs: "60",
    attempts: CURL_ATTEMPTS,
};

/// A head read as a hint ([`vendor_get_hint`]): one short attempt, because the caller asks
/// again on its own cadence.
const HINT_BOUNDS: VendorBounds = VendorBounds {
    connect_secs: "5",
    max_secs: "15",
    attempts: 1,
};

/// The option list for [`vendor_get`]. https on the first hop and every redirect, a bounded
/// redirect count, `bounds`' connect and wall-clock limits, curl's own size cap, the body
/// and the header dump into the caller's private scratch files, and the status + effective
/// URL on stdout. No `-f` (a 304 is an answer) and no `--retry` (the subprocess is
/// retried). The `If-None-Match` line is appended only when `condition` is given.
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

/// The option list for [`vendor_content_length`]: a HEAD that follows redirects under the
/// same https pins and bounds as [`vendor_get_args`]; every hop's headers land on stdout,
/// followed by the final status.
fn vendor_head_args() -> [&'static str; 15] {
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

/// The vendor-lane curl process: the anonymous argv (`-q` first, `--` before the URL,
/// never a credential channel) with [`VENDOR_ENV_SCRUB`] removed from its environment.
fn vendor_command(args: &[&str], url: &str) -> Command {
    let mut command = curl_command(args, url);
    for var in VENDOR_ENV_SCRUB {
        command.env_remove(var);
    }
    command
}

/// Spawn [`vendor_command`] with no stdin and wait for it.
// Skip: same audited display-lossy Err-path class as `head_no_redirect_with`.
#[cfg_attr(trust_verify, trust::skip)]
fn vendor_fetch(args: &[&str], url: &str) -> Result<std::process::Output, String> {
    use aterm_uds::spawnfd::Stdio;
    vendor_command(args, url)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("spawn curl: {e}"))
}

/// A fresh private directory for one vendor request's body and header dump, removed on
/// drop. `create_dir` on a new name (never an existing path) at mode `0700` on unix, so no
/// other user can pre-plant or read what curl writes there.
struct VendorScratch(std::path::PathBuf);

impl VendorScratch {
    // Skip: same audited display-lossy Err-path class as `head_no_redirect_with`.
    #[cfg_attr(trust_verify, trust::skip)]
    fn new() -> Result<Self, String> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        #[cfg(unix)]
        let builder = {
            let mut builder = std::fs::DirBuilder::new();
            std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
            builder
        };
        #[cfg(not(unix))]
        let builder = std::fs::DirBuilder::new();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        for _ in 0..8 {
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir()
                .join(format!("aterm-vendor-{}-{n}-{nanos}", std::process::id()));
            match builder.create(&dir) {
                Ok(()) => return Ok(Self(dir)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(format!("create vendor scratch dir: {e}")),
            }
        }
        Err("create vendor scratch dir: no free name".to_string())
    }
}

impl Drop for VendorScratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Read at most `cap` bytes of `path`, the body fetched from `url`; more is a verdict
/// about the document. A missing file is an empty body (curl creates no `-o` file for a
/// zero-length response).
fn read_capped(path: &Path, cap: u64, url: &str) -> Result<Vec<u8>, HttpError> {
    use std::io::Read;
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(HttpError::Transport(format!("read vendor body: {e}"))),
    };
    let mut bytes = Vec::new();
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

/// GET a small vendor document (a release head, a manifest, a signature, a SHA256SUMS)
/// anonymously: https on every hop, at most `cap` bytes of final body, bounded in time, and
/// with the CA-trust overrides dropped. `if_none_match` makes the request conditional (a 304
/// is [`VendorResponse::NotModified`]). The caller owns the URL policy; this layer enforces
/// only the scheme and returns the effective URL for the caller to check.
///
/// # Errors
/// [`HttpError::VendorRefused`] for a non-https URL, a zero cap, or an `if_none_match`
/// that is not a visible-ASCII ETag (before any spawn), a document over its cap, or a
/// [`vendor_curl_refusal`]; [`HttpError::VendorStatus`] for any status but 200 and a
/// conditional 304; [`HttpError::Malformed`] for an unreadable trailer; and
/// [`HttpError::Transport`] only when the network failed all three attempts.
pub fn vendor_get(
    url: &str,
    cap: u64,
    if_none_match: Option<&str>,
) -> Result<VendorResponse, HttpError> {
    vendor_get_within(url, cap, if_none_match, LANE_BOUNDS)
}

/// [`vendor_get`] for a head read only as a hint (the window's head watch): the same
/// pins, cap, anonymity and environment, but one attempt of at most 15 s, so a network
/// that drops packets costs seconds, not minutes.
///
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
// Skip: same audited display-lossy Err-path class as `head_no_redirect_with`.
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
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        back_off_before(attempt);
        // A previous attempt's body or headers must never be read as this one's.
        let _ = std::fs::remove_file(&body);
        let _ = std::fs::remove_file(&headers);
        let out = vendor_fetch(&args, url).map_err(HttpError::Transport)?;
        let dump = std::fs::read(&headers).unwrap_or_default();
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
            !attempt_again(run.exit, handshake_in(run.stdout), attempt, bounds.attempts),
            url,
            cap,
            if_none_match,
            || read_capped(&body, cap, url),
        );
        if let Step::Done(result) = step {
            return result;
        }
    }
}

/// The final `Content-Length` of `url` after redirects, from an anonymous HEAD under the
/// vendor lane's https pins, bounds and environment — the exact byte cap for a payload no
/// signed document sizes. Never zero (curl reads a zero cap as unlimited).
///
/// # Errors
/// [`HttpError::VendorRefused`] for a non-https URL (before any spawn) or a
/// [`vendor_curl_refusal`]; [`HttpError::VendorStatus`] for a final status other than
/// 200; [`HttpError::Malformed`] for a final hop with no single positive numeric
/// `Content-Length`; [`HttpError::Transport`] only when the network failed all three
/// attempts.
// Skip: same audited display-lossy Err-path class as `head_no_redirect_with`.
#[cfg_attr(trust_verify, trust::skip)]
pub fn vendor_content_length(url: &str) -> Result<u64, HttpError> {
    require_https_url(url).map_err(HttpError::VendorRefused)?;
    let args = vendor_head_args();
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        back_off_before(attempt);
        let out = vendor_fetch(&args, url).map_err(HttpError::Transport)?;
        let (stdout, stderr) = (
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        let run = CurlRun {
            exit: out.status.code(),
            stdout: &stdout,
            stderr: &stderr,
        };
        let last = !attempt_again(run.exit, handshake_in(run.stdout), attempt, CURL_ATTEMPTS);
        if let Step::Done(result) = vendor_head_step(&run, last, url) {
            return result;
        }
    }
}

/// Download a vendor-direct payload to `dest`, capped at exactly `cap` bytes: the resumable
/// transfer of [`download_to_resumable_https_only`] run as a vendor-lane child, with
/// [`VENDOR_ENV_SCRUB`] dropped from its environment.
///
/// # Errors
/// [`HttpError::VendorRefused`] for a non-https URL or a zero cap (before any spawn), a
/// payload over its cap, or a [`vendor_curl_refusal`]; [`HttpError::VendorStatus`] for a
/// status no retry changes (anything but 408, 429 and 5xx); [`HttpError::Transport`] for
/// everything else, the network included.
// Skip: same audited display-lossy Err-path class as `head_no_redirect_with`.
#[cfg_attr(trust_verify, trust::skip)]
pub fn vendor_download_to(url: &str, dest: &Path, cap: u64) -> Result<(), HttpError> {
    require_https_url(url).map_err(HttpError::VendorRefused)?;
    if cap == 0 {
        return Err(HttpError::VendorRefused(format!(
            "refusing a zero byte cap for {url}"
        )));
    }
    download_to_resumable_with(url, dest, cap, ResumeLane::VendorDirect, Sink::Part)
}

#[cfg(test)]
mod tests {
    const API_ASSET: &str = "https://api.github.com/repos/alabsystems/aterm/releases/assets/1";
    const WEB_ASSET: &str =
        "https://github.com/alabsystems/aterm/releases/download/v0.74.0/aterm-machines.toml";

    fn curl_err(code: u16) -> String {
        format!("curl: (22) The requested URL returned error: {code}")
    }

    /// Per host: on the API host a 403 is the rate limit; on the web host a 403 is a blocked
    /// host, a 404 a missing asset, and only 429 a throttle. A transport failure is none.
    #[test]
    fn a_rate_limited_asset_fetch_is_named_and_a_broken_one_is_not() {
        assert_eq!(super::curl_http_error_code(&curl_err(429)), Some(429));
        assert_eq!(
            super::curl_http_error_code(
                "curl: (22) The requested URL returned error: 403 rate limit exceeded"
            ),
            Some(403)
        );
        assert_eq!(super::curl_http_error_code("curl: (56) Recv failure"), None);
        assert!(super::download_error_is_rate_limit(&format!(
            "{}429) fetching asset",
            super::RATE_LIMIT_ERROR_PREFIX
        )));
        assert!(!super::download_error_is_rate_limit(
            "curl asset download failed (exit status: 22): 404"
        ));

        // API host: 403 and 429 are the rate limit; 404 takes the retry path.
        for code in [403, 429] {
            let verdict =
                super::classify_asset_failure(API_ASSET, &curl_err(code)).expect("a verdict");
            assert!(super::download_error_is_rate_limit(&verdict), "{verdict}");
        }
        assert_eq!(
            super::classify_asset_failure(API_ASSET, &curl_err(404)),
            None
        );

        // Web host: 429 is a rate limit; 403 and 404 are verdicts the check lane books.
        let throttled = super::classify_asset_failure(WEB_ASSET, &curl_err(429)).unwrap();
        assert!(super::download_error_is_rate_limit(&throttled));
        let forbidden = super::classify_asset_failure(WEB_ASSET, &curl_err(403)).unwrap();
        assert!(
            forbidden.contains("blocked host or proxy") && forbidden.contains(WEB_ASSET),
            "{forbidden}"
        );
        assert!(!super::download_error_is_rate_limit(&forbidden));
        let missing = super::classify_asset_failure(WEB_ASSET, &curl_err(404)).unwrap();
        assert!(
            missing.contains("does not carry this asset") && missing.contains(WEB_ASSET),
            "{missing}"
        );
        assert!(!super::download_error_is_rate_limit(&missing));
        // …and only the 404 is the not-found verdict the source-only-head path keys on.
        assert!(super::download_error_is_not_found(&missing));
        assert!(!super::download_error_is_not_found(&forbidden));
        assert!(!super::download_error_is_not_found(&throttled));
        assert!(!super::download_error_is_not_found(
            "curl asset download failed (exit status: 22): 404"
        ));
        // A 5xx on the web host is still the retry path, not a verdict.
        assert_eq!(
            super::classify_asset_failure(WEB_ASSET, &curl_err(502)),
            None
        );
        assert_eq!(
            super::classify_asset_failure(WEB_ASSET, "curl: (56) Recv failure"),
            None
        );
    }

    use super::{Handshake, Link, link_after};
    use super::{
        HttpError, LANE_BOUNDS, NextAttempt, RELEASE_ASSET_DOWNLOAD_BOUND, StallBound, curl_argv,
        download_bytes_args, download_max_time_secs, download_resume_args,
        download_resume_args_https_only, download_to_resumable, download_to_resumable_https_only,
        head_args, keep_partial, keep_partial_after_failure, location_header, next_attempt,
        part_path, range_refused, resume_allowed, resume_plan, transient_status, vendor_get_args,
    };
    use aterm_uds::spawnfd::Command;

    /// `-q` leads, and `--` comes after every option and immediately before the URL.
    #[test]
    fn argv_puts_q_first_and_end_of_options_right_before_the_url() {
        const URL: &str = "https://github.com/o/r/releases/download/v1.0.0/a.toml";
        let v = curl_argv(
            &[
                "-fsSL",
                "--max-filesize",
                "5000000",
                "-H",
                "Accept: application/octet-stream",
            ],
            URL,
        );
        assert_eq!(
            v[0], "-q",
            "-q must be the very first parameter (curlrc defense)"
        );
        assert!(
            v.iter().any(|a| a == "User-Agent: aterm-update"),
            "the fixed User-Agent still identifies us: {v:?}"
        );
        let dashdash = v.iter().position(|a| a == "--").expect("`--` present");
        assert_eq!(
            dashdash,
            v.len() - 2,
            "`--` must immediately precede the URL and nothing else: {v:?}"
        );
        assert_eq!(v[v.len() - 1], URL);
        assert_eq!(
            v.iter().filter(|a| *a == "--").count(),
            1,
            "exactly one end-of-options marker (callers must not add their own): {v:?}"
        );
    }

    /// The spawned program + argv, as owned strings.
    fn argv_of(command: &Command) -> Vec<String> {
        command
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    /// The download ceiling scales with the size cap (64 KiB/s floor), clamped to 600 s..6 h.
    #[test]
    fn the_download_ceiling_is_derived_from_the_size_cap() {
        // A release-sized asset gets hours, not ten minutes (64 KiB/s floor rate).
        assert_eq!(download_max_time_secs(775_000_000), 11_825);
        // The 2 GiB release bound and atpkg's 8 GiB ARTIFACT_CAP both take the 6 h clamp.
        assert_eq!(download_max_time_secs(RELEASE_ASSET_DOWNLOAD_BOUND), 21_600);
        assert_eq!(download_max_time_secs(8 << 30), 21_600);
        // …and a small cap never drops below 600 s.
        assert_eq!(download_max_time_secs(1024), 600);
        assert_eq!(download_max_time_secs(0), 600);
    }

    /// The asset download bounds the connect and the stall, with a derived wall clock.
    #[test]
    fn the_asset_download_bounds_the_stall_not_the_transfer() {
        let cap = RELEASE_ASSET_DOWNLOAD_BOUND.to_string();
        let max_time = download_max_time_secs(RELEASE_ASSET_DOWNLOAD_BOUND).to_string();
        let args = download_resume_args(
            &cap,
            &max_time,
            "/tmp/aterm.dmg.part",
            None,
            StallBound::Quick,
        );
        let value_of = |flag: &str| {
            let i = args
                .iter()
                .position(|a| *a == flag)
                .unwrap_or_else(|| panic!("{flag} present in {args:?}"));
            args[i + 1]
        };
        assert_eq!(
            value_of("--max-time"),
            "21600",
            "the wall clock must be the derived ceiling, never the fixed 600 s: {args:?}"
        );
        assert_eq!(value_of("--connect-timeout"), "30");
        assert_eq!(value_of("--speed-limit"), "4096");
        assert_eq!(value_of("--speed-time"), StallBound::Quick.secs());
        // The size cap and the sink are untouched by the timing change.
        assert_eq!(value_of("--max-filesize"), cap);
        assert_eq!(value_of("-o"), "/tmp/aterm.dmg.part");
        // Callers never place `--` themselves; `curl_argv` appends it.
        assert!(!args.contains(&"--"), "no caller-side `--`: {args:?}");
    }

    /// NO lane lets curl retry. On a pipe curl cannot truncate what a failed attempt already
    /// wrote, so a retried capture returns the failed attempt's bytes CONCATENATED in front
    /// of the good ones under a single `-w` status trailer — a blip curl had recovered from,
    /// read as a malformed answer. On a file it truncates back to where the attempt began, so
    /// a retried stall throws its bytes away (and over HTTP/2 retries on the same dead
    /// connection). Every lane retries the subprocess instead.
    #[test]
    fn no_lane_lets_curl_retry() {
        assert!(
            !head_args().contains(&"--retry"),
            "a stdout-captured HEAD must not let curl retry: {:?}",
            head_args()
        );
        let cap = "16777216";
        assert!(
            !download_bytes_args(cap).contains(&"--retry"),
            "the buffer the Ed25519 check reads must hold ONE attempt's bytes: {:?}",
            download_bytes_args(cap)
        );
        for stall in [StallBound::Quick, StallBound::Patient] {
            let file = download_resume_args(cap, "600", "/tmp/x", None, stall);
            assert!(
                !file.contains(&"--retry"),
                "a file lane continues its bytes itself: {file:?}"
            );
        }
    }

    /// Only the transient server-side set is retried in process; 429 is excluded.
    #[test]
    fn only_transient_server_statuses_are_retried_in_process() {
        for code in ["408", "500", "502", "503", "504"] {
            assert!(transient_status(code), "{code} is transient");
        }
        for code in ["200", "204", "301", "401", "403", "404", "429", "418", ""] {
            assert!(
                !transient_status(code),
                "{code} is a verdict, not a blip — retrying it is wrong"
            );
        }
    }
    /// ONE OFFLINE VERDICT, NOT ONE PER REQUEST. A request that reaches no host (the name or
    /// the proxy does not resolve, nothing answers the connect) retries with its backoff while
    /// the link is not known down, and holds it down once its last attempt fails the same way
    /// (a one-attempt hint never holds it down on its own word); a request after it on the
    /// dead link stops at its first such failure. A timeout or a
    /// broken transfer says nothing of the link and keeps every retry, known down or not, and
    /// any host's answer brings the link back up.
    #[test]
    fn a_link_known_down_is_not_paid_for_twice() {
        for no_host in [5, 6, 7] {
            let exit = Some(no_host);
            // A request with retries: they go on until the last, which holds the link down.
            assert_eq!(
                link_after(exit, Handshake::Unknown, 1, 3, false),
                (Link::Unknown, true),
                "{no_host}"
            );
            assert_eq!(
                link_after(exit, Handshake::Unknown, 3, 3, false),
                (Link::Down, false),
                "{no_host}"
            );
            // On a link already known down, its first such failure is its last.
            assert_eq!(
                link_after(exit, Handshake::Unknown, 1, 3, true),
                (Link::Down, false),
                "{no_host}"
            );
            // A one-attempt hint consults the verdict and never makes one on its own.
            assert_eq!(
                link_after(exit, Handshake::Unknown, 1, 1, false),
                (Link::Unknown, false),
                "{no_host}"
            );
        }
        for transient in [Some(28), Some(35), Some(52), Some(56), None] {
            for known_down in [false, true] {
                assert_eq!(
                    link_after(transient, Handshake::Unknown, 1, 3, known_down),
                    (Link::Unknown, true),
                    "{transient:?}"
                );
                assert_eq!(
                    link_after(transient, Handshake::Unknown, 3, 3, known_down),
                    (Link::Unknown, false),
                    "{transient:?}"
                );
            }
        }
        for answered in [0, 22] {
            assert_eq!(
                link_after(Some(answered), Handshake::Done, 1, 3, true),
                (Link::Reached, true)
            );
            assert_eq!(
                link_after(Some(answered), Handshake::Done, 1, 1, true),
                (Link::Reached, false)
            );
        }
    }

    /// A connect bound that expired before any TLS handshake finished reached no host, as a
    /// name that does not resolve does: its request retries only while the link is not known
    /// down, and holds it down once it spent its retries. A timeout after the handshake, or
    /// one no trailer explains, is a transfer's, and keeps every retry.
    #[test]
    fn a_connect_that_never_finished_reached_no_host() {
        use super::handshake_in;
        assert_eq!(handshake_in("000 0.000000"), Handshake::NotDone);
        assert_eq!(handshake_in("000 https://x/ 0.000000"), Handshake::NotDone);
        assert_eq!(handshake_in("200 0.130092"), Handshake::Done);
        assert_eq!(handshake_in("200 https://x/"), Handshake::Unknown);
        assert_eq!(handshake_in(""), Handshake::Unknown);
        let stalled = Some(28);
        assert_eq!(
            link_after(stalled, Handshake::NotDone, 1, 3, false),
            (Link::Unknown, true)
        );
        assert_eq!(
            link_after(stalled, Handshake::NotDone, 3, 3, false),
            (Link::Down, false)
        );
        assert_eq!(
            link_after(stalled, Handshake::NotDone, 1, 3, true),
            (Link::Down, false),
            "on a link known down, a connect that never finished is not paid for twice"
        );
        for transfer in [Handshake::Done, Handshake::Unknown] {
            assert_eq!(
                link_after(stalled, transfer, 1, 3, true),
                (Link::Unknown, true)
            );
            assert_eq!(
                link_after(stalled, transfer, 3, 3, false),
                (Link::Unknown, false)
            );
        }
    }

    /// Conditional requests exist ONLY on the vendor lane: no GitHub lane's option list
    /// carries an `If-None-Match`.
    #[test]
    fn only_the_vendor_lane_asks_conditionally() {
        let cap = "1024";
        let lanes: [Vec<&str>; 4] = [
            head_args().to_vec(),
            download_bytes_args(cap).to_vec(),
            download_resume_args(cap, "600", "/tmp/x", None, StallBound::Quick),
            download_resume_args_https_only(
                cap,
                "600",
                "/tmp/x.part",
                Some("4"),
                StallBound::Quick,
            ),
        ];
        for lane in &lanes {
            assert!(
                !lane.iter().any(|a| a.contains("If-None-Match")),
                "{lane:?}"
            );
        }
        let conditional = vendor_get_args(
            LANE_BOUNDS,
            cap,
            "/s/body",
            "/s/headers",
            Some("If-None-Match: \"x\""),
        );
        assert_eq!(
            conditional
                .iter()
                .filter(|a| a.starts_with("If-None-Match"))
                .count(),
            1
        );
        let plain = vendor_get_args(LANE_BOUNDS, cap, "/s/body", "/s/headers", None);
        assert!(!plain.iter().any(|a| a.contains("If-None-Match")));
        assert_eq!(conditional.len(), plain.len() + 2, "exactly one `-H` pair");
    }

    /// The vendor GET pins https on every hop, bounds redirects and time, caps the size, and
    /// carries no credential channel, `-f` or `--retry`; `--` comes last before the URL.
    #[test]
    fn the_vendor_get_argv_pins_https_bounds_and_carries_no_credential() {
        const URL: &str = "https://downloads.claude.ai/claude-code-releases/latest";
        let args = vendor_get_args(
            LANE_BOUNDS,
            "64",
            "/s/body",
            "/s/headers",
            Some("If-None-Match: \"e\""),
        );
        let value_of = |flag: &str| {
            let i = args
                .iter()
                .position(|a| *a == flag)
                .unwrap_or_else(|| panic!("{flag} present in {args:?}"));
            args[i + 1]
        };
        assert_eq!(value_of("--proto"), "=https");
        assert_eq!(value_of("--proto-redir"), "=https");
        assert_eq!(value_of("--max-redirs"), "5");
        assert_eq!(value_of("--connect-timeout"), "5");
        assert_eq!(value_of("--max-time"), "60");
        assert_eq!(value_of("--max-filesize"), "64");
        assert_eq!(value_of("-o"), "/s/body");
        assert_eq!(value_of("--dump-header"), "/s/headers");
        assert_eq!(
            value_of("-w"),
            "%{http_code} %{url_effective} %{time_appconnect}"
        );
        assert!(args.contains(&"-L"));
        for absent in [
            "-f", "--fail", "--retry", "--config", "-K", "--netrc", "-u", "--",
        ] {
            assert!(
                !args.contains(&absent),
                "{absent} must not be on the vendor lane"
            );
        }
        let v = argv_of(&super::vendor_command(&args, URL));
        assert_eq!(v[0], "-q", "the curlrc defense comes first");
        assert_eq!(v.iter().filter(|a| *a == "--").count(), 1);
        assert_eq!(v[v.len() - 2], "--");
        assert_eq!(v[v.len() - 1], URL);
        assert!(
            !v.iter().any(|a| {
                let a = a.to_ascii_lowercase();
                a.contains("authorization") || a.contains("bearer")
            }),
            "{v:?}"
        );
    }

    /// The vendor HEAD follows redirects under the same pins and bounds, reads headers
    /// only, and ends on the status trailer.
    #[test]
    fn the_vendor_head_argv_follows_redirects_under_the_same_pins() {
        let args = super::vendor_head_args();
        for flag in ["-I", "-L"] {
            assert!(args.contains(&flag), "{flag}: {args:?}");
        }
        for (flag, value) in [
            ("--proto", "=https"),
            ("--proto-redir", "=https"),
            ("--max-redirs", "5"),
            ("--connect-timeout", "5"),
            ("--max-time", "60"),
            ("-w", "\n%{http_code} %{time_appconnect}"),
        ] {
            let i = args.iter().position(|a| *a == flag).expect(flag);
            assert_eq!(args[i + 1], value, "{flag}");
        }
        for absent in ["-f", "--fail", "--retry", "--config", "--", "-o"] {
            assert!(!args.contains(&absent), "{absent}: {args:?}");
        }
    }

    /// The vendor child drops the CA-trust overrides and the TLS backend switch beside the
    /// curlrc redirections; the GitHub lanes keep theirs.
    #[test]
    fn the_vendor_child_env_drops_the_ca_overrides() {
        let removed = |command: &Command| -> Vec<String> {
            command
                .get_envs()
                .filter(|(_, value)| value.is_none())
                .map(|(key, _)| key.to_string_lossy().into_owned())
                .collect()
        };
        let vendor = removed(&super::vendor_command(
            &["-sS"],
            "https://releases.openai.com/x",
        ));
        for var in [
            "CURL_HOME",
            "XDG_CONFIG_HOME",
            "CURL_CA_BUNDLE",
            "SSL_CERT_FILE",
            "SSL_CERT_DIR",
            "CURL_SSL_BACKEND",
        ] {
            assert!(
                vendor.iter().any(|k| k == var),
                "{var} scrubbed: {vendor:?}"
            );
        }
        let github = removed(&super::curl_command(&["-sS"], WEB_ASSET));
        assert!(!github.iter().any(|k| k == "SSL_CERT_FILE"), "{github:?}");
    }

    /// An ETag is sendable only as visible ASCII, so a stored value can never inject a
    /// header line; the reader takes the last hop's, case-insensitively, and drops one it
    /// could not send back.
    #[test]
    fn etags_are_read_from_the_last_hop_and_only_well_formed_ones_survive() {
        for good in [
            "\"5dc26bfe87ab1b83497e1f21c94fb98d\"",
            "W/\"x\"",
            "\"0x8DF18E1A1B206AB\"",
        ] {
            assert!(super::etag_ok(good), "{good}");
        }
        let long = "x".repeat(super::ETAG_MAX + 1);
        for bad in [
            "",
            "\"a b\"",
            "\"a\"\r\nX-Evil: 1",
            "\"a\"\n",
            "\"a\t\"",
            long.as_str(),
        ] {
            assert!(!super::etag_ok(bad), "{bad:?}");
        }
        let dump = "HTTP/2 302\r\netag: \"first\"\r\nlocation: https://b/\r\n\r\n\
                    HTTP/2 200\r\nETag: \"second\"\r\ncontent-length: 7\r\n\r\n";
        assert_eq!(super::etag_header(dump).as_deref(), Some("\"second\""));
        assert_eq!(
            super::etag_header("HTTP/2 302\r\netag: \"first\"\r\n\r\nHTTP/2 200\r\n\r\n"),
            None,
            "the kept hop carried none"
        );
        assert_eq!(super::etag_header("HTTP/2 200\r\netag: \"a b\"\r\n"), None);
        assert_eq!(
            super::etag_header("HTTP/2 304\r\netag: \"e\"\r\n\r\n").as_deref(),
            Some("\"e\"")
        );
    }

    /// The final hop's Content-Length, only as one positive decimal: a zero would reach
    /// curl as an unlimited `--max-filesize`, and two different values are ambiguous.
    #[test]
    fn the_content_length_is_the_final_hops_single_positive_decimal() {
        let chain = "HTTP/2 302\r\ncontent-length: 0\r\nlocation: https://x/\r\n\r\n\
                     HTTP/2 200\r\nContent-Length: 127316185\r\n\r\n";
        assert_eq!(super::content_length_header(chain), Some(127_316_185));
        for bad in [
            "HTTP/2 200\r\ncontent-length: 0\r\n",
            "HTTP/2 200\r\ncontent-length: -5\r\n",
            "HTTP/2 200\r\ncontent-length: 12, 12\r\n",
            "HTTP/2 200\r\ncontent-length: 1e3\r\n",
            "HTTP/2 200\r\ncontent-length: 5\r\ncontent-length: 6\r\n",
            "HTTP/2 200\r\ncontent-type: text/plain\r\n",
            "HTTP/2 200\r\ncontent-length: 99999999999999999999999\r\n",
            "",
        ] {
            assert_eq!(super::content_length_header(bad), None, "{bad:?}");
        }
        assert_eq!(
            super::content_length_header(
                "HTTP/2 200\r\ncontent-length: 5\r\ncontent-length: 5\r\n"
            ),
            Some(5),
            "a repeated identical value is one value"
        );
    }

    /// The `-w` trailer: a numeric code and an https effective URL, or nothing.
    #[test]
    fn the_vendor_trailer_yields_a_code_and_an_https_effective_url() {
        assert_eq!(
            super::vendor_trailer("200 https://release-assets.githubusercontent.com/a?b=c"),
            Some((
                200,
                "https://release-assets.githubusercontent.com/a?b=c".to_string()
            ))
        );
        assert_eq!(
            super::vendor_trailer("304 https://downloads.claude.ai/claude-code-releases/latest\n")
                .map(|t| t.0),
            Some(304)
        );
        // The handshake's time after the URL ([`super::handshake_in`]) is not part of it.
        assert_eq!(
            super::vendor_trailer("200 https://x/y 0.130092\n"),
            Some((200, "https://x/y".to_string()))
        );
        for bad in [
            "",
            "200",
            "abc https://x/",
            "200 http://x/",
            "200 https://x/ y",
            "200 file:///etc/passwd",
        ] {
            assert_eq!(super::vendor_trailer(bad), None, "{bad:?}");
        }
    }

    /// A finished run for the step tests: exit 0 unless given, empty stderr.
    fn run<'a>(exit: Option<i32>, stdout: &'a str, stderr: &'a str) -> super::CurlRun<'a> {
        super::CurlRun {
            exit,
            stdout,
            stderr,
        }
    }

    /// The body reader a step must not call.
    fn no_body() -> Result<Vec<u8>, HttpError> {
        panic!("the body is read only for a 200")
    }

    /// 200 keeps the body with the last hop's ETag and the effective URL; a conditional 304
    /// keeps a validator (its own ETag, else the one sent); an unconditional 304 and every
    /// other status are the host's refusal.
    #[test]
    fn a_vendor_get_attempt_answers_body_not_modified_or_the_status() {
        use super::{Step, VendorResponse, vendor_get_step};
        const URL: &str = "https://releases.openai.com/codex/channels/latest";
        let ok = run(
            Some(0),
            "200 https://releases.openai.com/codex/channels/latest",
            "",
        );
        let dump = "HTTP/2 200\r\netag: \"new\"\r\ncontent-length: 7\r\n\r\n";
        for sent in [None, Some("\"old\"")] {
            assert_eq!(
                vendor_get_step(&ok, dump, false, URL, 64, sent, || Ok(b"{}".to_vec())),
                Step::Done(Ok(VendorResponse::Body {
                    bytes: b"{}".to_vec(),
                    etag: Some("\"new\"".into()),
                    effective_url: URL.into(),
                }))
            );
        }
        let not_modified = run(
            Some(0),
            "304 https://releases.openai.com/codex/channels/latest",
            "",
        );
        assert_eq!(
            vendor_get_step(
                &not_modified,
                "HTTP/2 304\r\netag: \"e2\"\r\n\r\n",
                false,
                URL,
                64,
                Some("\"e\""),
                no_body
            ),
            Step::Done(Ok(VendorResponse::NotModified {
                etag: "\"e2\"".into()
            })),
            "the 304's own ETag is the validator to keep"
        );
        assert_eq!(
            vendor_get_step(
                &not_modified,
                "HTTP/2 304\r\n\r\n",
                false,
                URL,
                64,
                Some("\"e\""),
                no_body
            ),
            Step::Done(Ok(VendorResponse::NotModified {
                etag: "\"e\"".into()
            })),
            "a 304 without an ETag keeps the one sent"
        );
        for (stdout, sent) in [
            (
                "304 https://releases.openai.com/codex/channels/latest",
                None,
            ),
            (
                "404 https://releases.openai.com/codex/channels/latest",
                Some("\"e\""),
            ),
            (
                "429 https://releases.openai.com/codex/channels/latest",
                None,
            ),
            (
                "206 https://releases.openai.com/codex/channels/latest",
                None,
            ),
            (
                "503 https://releases.openai.com/codex/channels/latest",
                None,
            ),
        ] {
            let code: u16 = stdout[..3].parse().unwrap();
            let Step::Done(Err(err)) =
                vendor_get_step(&run(Some(0), stdout, ""), "", true, URL, 64, sent, no_body)
            else {
                panic!("{stdout}: a final status is an error");
            };
            assert_eq!(
                err,
                HttpError::VendorStatus {
                    code,
                    url: URL.into()
                }
            );
            let text = err.to_string();
            assert!(
                text.contains("vendor host") && !text.contains("GitHub"),
                "{text}"
            );
        }
        let mangled = run(Some(0), "<html>portal</html>", "");
        assert!(matches!(
            vendor_get_step(&mangled, "", false, URL, 64, None, no_body),
            Step::Done(Err(HttpError::Malformed(_)))
        ));
        // The capped read's own verdict passes through untouched.
        let over = || Err(HttpError::VendorRefused("over".into()));
        assert_eq!(
            vendor_get_step(&ok, dump, false, URL, 64, None, over),
            Step::Done(Err(HttpError::VendorRefused("over".into())))
        );
    }

    /// Only the network is retried, and only while attempts remain: a transient status or
    /// a transport exit retries, the last attempt reports it as `Transport`, and a size
    /// verdict or a curl refusal is final on the first attempt and never `Transport`.
    #[test]
    fn a_vendor_get_attempt_retries_only_the_network() {
        use super::{Step, vendor_get_step};
        const URL: &str = "https://downloads.claude.ai/claude-code-releases/latest";
        let dns = run(Some(6), "", "curl: (6) Could not resolve host");
        let busy = run(
            Some(0),
            "503 https://downloads.claude.ai/claude-code-releases/latest",
            "",
        );
        for transient in [&dns, &busy] {
            assert_eq!(
                vendor_get_step(transient, "", false, URL, 64, None, no_body),
                Step::Retry
            );
        }
        assert!(matches!(
            vendor_get_step(&dns, "", true, URL, 64, None, no_body),
            Step::Done(Err(HttpError::Transport(ref m))) if m.contains("exit 6") && m.contains("resolve")
        ));
        for (exit, stderr) in [
            (Some(63), "curl: (63) Maximum file size exceeded"),
            (Some(56), "curl: (56) Maximum file size exceeded"),
            (
                Some(1),
                "curl: (1) Protocol \"http\" disabled (in redirect)",
            ),
            (Some(47), "curl: (47) Maximum (5) redirects followed"),
            (Some(60), "curl: (60) SSL certificate problem"),
            (
                Some(51),
                "curl: (51) SSL: no alternative certificate subject name",
            ),
        ] {
            let Step::Done(Err(HttpError::VendorRefused(message))) =
                vendor_get_step(&run(exit, "", stderr), "", false, URL, 64, None, no_body)
            else {
                panic!("{exit:?} is a verdict on the first attempt");
            };
            assert!(message.contains(URL), "{message}");
        }
        // A last attempt never answers Retry, so the loop always ends.
        for last_run in [&dns, &busy, &run(None, "", "")] {
            assert_ne!(
                vendor_get_step(last_run, "", true, URL, 64, None, no_body),
                Step::Retry
            );
        }
    }

    /// One HEAD attempt: the trailer is split off the last line, only the final hop's
    /// Content-Length counts, the network is retried while attempts remain, and a curl
    /// refusal is final.
    #[test]
    fn a_vendor_head_attempt_reads_the_final_hop_under_the_trailer() {
        use super::{Step, vendor_head_step};
        const URL: &str = "https://github.com/openai/codex/releases/download/rust-v0.156.0/\
                           codex-package_SHA256SUMS";
        let chain = "HTTP/2 302\r\nlocation: https://release-assets.githubusercontent.com/x\r\n\
                     content-length: 0\r\n\r\nHTTP/2 200\r\ncontent-length: 1631\r\n\r\n\n200";
        assert_eq!(
            vendor_head_step(&run(Some(0), chain, ""), false, URL),
            Step::Done(Ok(1631))
        );
        let gone = "HTTP/2 302\r\ncontent-length: 5\r\n\r\nHTTP/2 404\r\n\r\n\n404";
        assert_eq!(
            vendor_head_step(&run(Some(0), gone, ""), false, URL),
            Step::Done(Err(HttpError::VendorStatus {
                code: 404,
                url: URL.into()
            }))
        );
        let no_length = "HTTP/2 200\r\ncontent-type: text/plain\r\n\r\n\n200";
        assert!(matches!(
            vendor_head_step(&run(Some(0), no_length, ""), false, URL),
            Step::Done(Err(HttpError::Malformed(_)))
        ));
        assert!(matches!(
            vendor_head_step(&run(Some(0), "HTTP/2 200\r\n\r\n\nabc", ""), false, URL),
            Step::Done(Err(HttpError::Malformed(_)))
        ));
        let busy = "HTTP/2 503\r\n\r\n\n503";
        assert_eq!(
            vendor_head_step(&run(Some(0), busy, ""), false, URL),
            Step::Retry
        );
        assert_eq!(
            vendor_head_step(&run(Some(0), busy, ""), true, URL),
            Step::Done(Err(HttpError::VendorStatus {
                code: 503,
                url: URL.into()
            }))
        );
        let dns = run(Some(6), "", "curl: (6) Could not resolve host");
        assert_eq!(vendor_head_step(&dns, false, URL), Step::Retry);
        assert!(matches!(
            vendor_head_step(&dns, true, URL),
            Step::Done(Err(HttpError::Transport(_)))
        ));
        assert!(matches!(
            vendor_head_step(&run(Some(60), "", "curl: (60) SSL"), false, URL),
            Step::Done(Err(HttpError::VendorRefused(_)))
        ));
    }

    /// A payload failure is a verdict when no retry changes it — over the cap, a curl
    /// refusal, a `-f` status other than 408/429/5xx — and a transport failure otherwise.
    #[test]
    fn a_vendor_payload_failure_is_a_verdict_only_when_it_recurs() {
        use super::vendor_download_verdict as verdict;
        const URL: &str =
            "https://downloads.claude.ai/claude-code-releases/2.1.280/darwin-arm64/claude";
        let http = |code: u16| format!("curl: (22) The requested URL returned error: {code}");
        assert!(matches!(
            verdict(Some(63), "curl: (63) Maximum file size exceeded", URL, 9),
            Some(HttpError::VendorRefused(ref m)) if m.contains("9-byte cap")
        ));
        assert!(matches!(
            verdict(Some(1), "", URL, 9),
            Some(HttpError::VendorRefused(_))
        ));
        for code in [403, 404, 410] {
            assert_eq!(
                verdict(Some(22), &http(code), URL, 9),
                Some(HttpError::VendorStatus {
                    code,
                    url: URL.into()
                })
            );
        }
        for code in [408, 429, 500, 503] {
            assert_eq!(verdict(Some(22), &http(code), URL, 9), None, "{code}");
        }
        assert_eq!(
            verdict(Some(28), "curl: (28) Operation timed out", URL, 9),
            None
        );
        assert_eq!(verdict(None, "", URL, 9), None);
    }

    /// A non-https URL, a zero cap and an injecting If-None-Match are refused before spawning
    /// as verdicts, never `Transport` (which callers read as offline).
    #[test]
    fn the_vendor_lane_refuses_before_spawning() {
        const URL: &str = "https://downloads.claude.ai/claude-code-releases/latest";
        let dest = std::env::temp_dir().join("aterm-vendor-refused-never-written");
        let refused = |result: Result<(), HttpError>, needle: &str| match result {
            Err(HttpError::VendorRefused(m)) => assert!(m.contains(needle), "{m}"),
            other => panic!("{needle}: {other:?}"),
        };
        for url in [
            "http://downloads.claude.ai/x",
            "file:///etc/passwd",
            "-K/tmp/evil",
        ] {
            refused(super::vendor_get(url, 64, None).map(drop), "non-https");
            refused(super::vendor_content_length(url).map(drop), "non-https");
            refused(super::vendor_download_to(url, &dest, 64), "non-https");
        }
        refused(super::vendor_get(URL, 0, None).map(drop), "zero byte cap");
        refused(super::vendor_download_to(URL, &dest, 0), "zero byte cap");
        refused(
            super::vendor_get(URL, 64, Some("\"a\"\r\nX-Evil: 1")).map(drop),
            "If-None-Match",
        );
        assert!(!dest.exists(), "a refusal writes nothing");
    }

    /// Both of curl's spellings of "over the cap" are a verdict, and nothing else is.
    #[test]
    fn only_the_size_cap_is_read_as_a_size_verdict() {
        assert!(super::filesize_exceeded(Some(63), ""));
        assert!(super::filesize_exceeded(
            Some(56),
            "curl: (56) Maximum file size exceeded"
        ));
        assert!(!super::filesize_exceeded(
            Some(56),
            "curl: (56) Recv failure"
        ));
        assert!(!super::filesize_exceeded(
            Some(28),
            "curl: (28) Operation timed out"
        ));
        assert!(!super::filesize_exceeded(None, ""));
    }

    /// The body bound is exact: `cap` bytes are kept, `cap + 1` refused, and a missing
    /// file (curl writes none for an empty body) is an empty document.
    #[test]
    fn the_vendor_body_read_is_capped_exactly() {
        let scratch = super::VendorScratch::new().expect("scratch dir");
        let path = scratch.0.join("body");
        std::fs::write(&path, b"2.1.280\n").unwrap();
        assert_eq!(super::read_capped(&path, 8, "u").unwrap(), b"2.1.280\n");
        let Err(HttpError::VendorRefused(err)) = super::read_capped(&path, 7, "u") else {
            panic!("one byte over the cap is a verdict about the document");
        };
        assert!(err.contains("7-byte cap"), "{err}");
        assert!(
            super::read_capped(&scratch.0.join("absent"), 8, "u")
                .unwrap()
                .is_empty()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&scratch.0).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700, "the scratch dir is private");
        }
        let dir = scratch.0.clone();
        drop(scratch);
        assert!(!dir.exists(), "the scratch dir is removed on drop");
    }

    /// The web-lane steady-state request: headers only, one hop, no `-f`, `-L` or credential
    /// channel, and `--` last before the URL.
    #[test]
    fn the_head_argv_stops_at_the_first_hop_and_carries_no_credential() {
        let args = head_args();
        assert!(args.contains(&"-I"), "{args:?}");
        let timeout = args.iter().position(|a| *a == "--max-time").unwrap();
        assert_eq!(args[timeout + 1], "30");
        let quick = super::head_args_with_timeout("5");
        assert_eq!(quick[timeout + 1], "5");
        assert_eq!(&quick[..timeout + 1], &args[..timeout + 1]);
        assert_eq!(&quick[timeout + 2..], &args[timeout + 2..]);
        let i = args
            .iter()
            .position(|a| *a == "--max-redirs")
            .expect("redirects are refused explicitly");
        assert_eq!(args[i + 1], "0");
        for absent in ["-L", "--location", "-f", "--fail", "--retry", "--config"] {
            assert!(
                !args.contains(&absent),
                "{absent} must not be on the HEAD lane"
            );
        }
        let v = curl_argv(
            &args,
            "https://github.com/alabsystems/aterm/releases/latest/download/aterm-appcast.toml",
        );
        assert_eq!(v[0], "-q");
        let dashdash = v.iter().position(|a| a == "--").expect("`--` present");
        assert_eq!(dashdash, v.len() - 2);
        // The scheme gate runs before any spawn.
        let err = super::head_no_redirect("http://github.com/x").unwrap_err();
        assert!(err.to_string().contains("non-https"), "{err}");
        let err = super::head_no_redirect_quick("http://github.com/x").unwrap_err();
        assert!(err.to_string().contains("non-https"), "{err}");
    }

    /// The HEAD lanes and the small-document GET bound the connect apart from the wall clock,
    /// as an option ahead of the `--` that guards the URL.
    #[test]
    fn the_small_lanes_bound_the_connect_ahead_of_the_url_guard() {
        let small = super::download_bytes_args("5000000");
        let quick = super::head_args_with_timeout("5");
        let head = head_args();
        for lane in [&head[..], &quick[..], &small[..]] {
            let v = curl_argv(lane, WEB_ASSET);
            let bound = v
                .iter()
                .position(|a| a == "--connect-timeout")
                .unwrap_or_else(|| panic!("no connect bound: {v:?}"));
            assert_eq!(v[bound + 1], "5", "{v:?}");
            let dashdash = v.iter().position(|a| a == "--").expect("`--` present");
            assert!(bound < dashdash, "{v:?}");
        }
    }

    /// The `Location` reader takes the last block's value, case-insensitively and trimmed,
    /// and nothing for an absent or empty header.
    #[test]
    fn the_location_header_is_read_from_the_kept_hop() {
        assert_eq!(
            location_header(
                "HTTP/2 302 \r\nlocation: https://github.com/o/r/releases/download/v1.2.3/a\r\n\r\n"
            )
            .as_deref(),
            Some("https://github.com/o/r/releases/download/v1.2.3/a")
        );
        assert_eq!(
            location_header("HTTP/1.1 301\r\nLocation: https://first/\r\n\r\nHTTP/2 302\r\nLOCATION:   https://second/  \r\n")
                .as_deref(),
            Some("https://second/")
        );
        assert_eq!(
            location_header("HTTP/2 404\r\ncontent-type: text/plain\r\n"),
            None
        );
        assert_eq!(location_header("HTTP/2 302\r\nlocation:\r\n"), None);
        assert_eq!(location_header(""), None);
    }

    // Resumable artifact download: the size accounting, the request shape and the `.part`
    // state machine, which need no network.

    /// The part sibling appends `.part`, keeping the build-bearing name.
    #[test]
    fn the_part_sibling_appends_and_keeps_the_build_bearing_name() {
        let p = part_path(std::path::Path::new("/s/trust-5520.tar.zst")).unwrap();
        assert_eq!(p, std::path::Path::new("/s/trust-5520.tar.zst.part"));
        assert!(
            p.to_string_lossy().contains("5520"),
            "the part name must still carry the build number: {p:?}"
        );
        assert_eq!(part_path(std::path::Path::new("/")), None);
    }

    /// `offset + remaining_cap` always equals the caller's total bound, the anti-disk-fill
    /// guard.
    #[test]
    fn the_total_byte_bound_survives_every_resume_offset() {
        const TOTAL: u64 = 8 << 30;
        for existing in [0u64, 1, 4096, 629_817_785, TOTAL - 1] {
            let plan = resume_plan(existing, TOTAL);
            assert!(!plan.discard);
            assert_eq!(plan.offset, existing);
            assert_eq!(
                plan.offset + plan.remaining_cap,
                TOTAL,
                "offset + remaining cap must equal the total bound (existing={existing})"
            );
        }
        // A prefix at or past the total bound is discarded.
        for existing in [TOTAL, TOTAL + 1] {
            let plan = resume_plan(existing, TOTAL);
            assert!(plan.discard, "existing={existing}");
            assert_eq!(plan.offset, 0);
            assert_eq!(plan.remaining_cap, TOTAL);
        }
    }

    /// A FRESH transfer carries no `--continue-at` — no `Range` for a server to mishandle —
    /// and a resumed one adds exactly the offset.
    #[test]
    fn only_a_resumed_attempt_carries_a_range() {
        let quick = StallBound::Quick;
        let fresh = download_resume_args("100", "600", "/s/a.tar.zst.part", None, quick);
        assert!(!fresh.contains(&"--continue-at"), "{fresh:?}");
        let resumed = download_resume_args("60", "600", "/s/a.tar.zst.part", Some("40"), quick);
        assert_eq!(resumed.len(), fresh.len() + 2);
        assert_eq!(resumed[fresh.len()], "--continue-at");
        assert_eq!(
            resumed[fresh.len() + 1],
            "40",
            "an EXPLICIT offset, never `-`"
        );
        // The sink is the part, never the destination.
        let sink = resumed[resumed.iter().position(|a| *a == "-o").unwrap() + 1];
        assert!(sink.ends_with(".part"), "{resumed:?}");
        // Resuming leaves the stall detector and the retry alone.
        for flag in [
            "--speed-limit",
            "--speed-time",
            "--keepalive-time",
            "--connect-timeout",
            "--max-time",
        ] {
            assert!(resumed.contains(&flag), "{flag} must survive: {resumed:?}");
        }
        assert!(
            !resumed.contains(&"--"),
            "no caller-side end-of-options marker"
        );
    }

    /// THE FILE LANES' FLAG SET (the resumable lanes and the app container's). A silent drop
    /// ends at curl's stall bound and keepalive, and every later ALab download waits behind
    /// it: an attempt is held to 4 KiB/s for 8 s (patient 120 s only after one that never beat
    /// 4 KiB/s), probes an idle connection every 2 s, and never lets curl retry — curl's
    /// retry truncates the file back to where the attempt began.
    #[test]
    fn a_resumable_attempt_is_held_to_a_quick_stall_and_never_lets_curl_retry() {
        let value_of = |args: &[&str], flag: &str| -> String {
            let i = args
                .iter()
                .position(|a| *a == flag)
                .unwrap_or_else(|| panic!("{flag} present in {args:?}"));
            args[i + 1].to_string()
        };
        for stall in [StallBound::Quick, StallBound::Patient] {
            for args in [
                download_resume_args("100", "600", "/s/a.part", Some("40"), stall),
                download_resume_args_https_only("100", "600", "/s/a.part", None, stall),
            ] {
                assert!(!args.contains(&"--retry"), "{args:?}");
                assert_eq!(value_of(&args, "--speed-limit"), "4096");
                assert_eq!(value_of(&args, "--keepalive-time"), "2");
                assert_eq!(value_of(&args, "--connect-timeout"), "30");
                assert_eq!(value_of(&args, "--max-time"), "600");
                assert_eq!(value_of(&args, "--max-filesize"), "100");
                assert_eq!(value_of(&args, "--proto-redir"), "=https");
                assert_eq!(
                    value_of(&args, "--speed-time"),
                    if stall == StallBound::Quick {
                        "8"
                    } else {
                        "120"
                    }
                );
            }
        }
    }

    /// The quick bound is earned by a transfer that beat 4 KiB/s, stalled time included; one
    /// that never did — a body a scanning proxy holds back, a trickle, nothing at all — gets
    /// the historical 120 s next.
    #[test]
    fn only_an_attempt_that_beat_the_speed_floor_keeps_the_quick_stall_bound() {
        let secs = std::time::Duration::from_secs;
        // A silent drop: 30 MB in a second, then 14 s of nothing.
        assert_eq!(StallBound::after(30_000_000, secs(15)), StallBound::Quick);
        // 64 kB/s for a minute, then a 13 s silence that tripped the bound.
        assert_eq!(StallBound::after(3_840_000, secs(73)), StallBound::Quick);
        assert_eq!(StallBound::after(4096 * 15, secs(15)), StallBound::Quick);
        // A held body, a trickle, a link that moved nothing.
        assert_eq!(StallBound::after(0, secs(14)), StallBound::Patient);
        assert_eq!(
            StallBound::after(4096 * 15 - 1, secs(15)),
            StallBound::Patient
        );
        assert_eq!(StallBound::after(1024 * 14, secs(14)), StallBound::Patient);
    }

    /// WHAT A FAILED ATTEMPT LEADS TO. One that moved the `.part` resumes from it at once,
    /// whatever ended it, but for the cap and a full disk; one that moved nothing retries only
    /// the failures curl's own `--retry` retried (a timeout, an unresolved name, a transient
    /// status), and everything else is returned.
    #[test]
    fn a_failed_attempt_resumes_if_it_moved_and_retries_only_curls_set_if_not() {
        use NextAttempt::{Resume, Retry, Stop};
        let stall = "curl: (28) Operation too slow. Less than 4096 bytes/sec transferred";
        let reset = "curl: (56) Recv failure: Connection reset by peer";
        let closed = "curl: (18) transfer closed with 123 bytes remaining to read";
        for (exit, stderr) in [(28, stall), (56, reset), (18, closed)] {
            assert_eq!(next_attempt(Some(exit), stderr, true), Resume, "{stderr}");
        }
        // NEGATIVE CONTROL: the same failures that moved nothing are not resumes.
        assert_eq!(next_attempt(Some(28), stall, false), Retry);
        assert_eq!(next_attempt(Some(56), reset, false), Stop);
        assert_eq!(next_attempt(Some(18), closed, false), Stop);
        // No reconnect changes the cap or a full disk, moved or not.
        let full = "curl: (23) Failure writing output to destination";
        let over = "curl: (63) Maximum file size exceeded";
        for moved in [true, false] {
            assert_eq!(next_attempt(Some(23), full, moved), Stop);
            assert_eq!(next_attempt(Some(63), over, moved), Stop);
        }
        // Moved nothing: curl's retry set, and only it.
        assert_eq!(
            next_attempt(Some(6), "curl: (6) Could not resolve host", false),
            Retry
        );
        assert_eq!(
            next_attempt(Some(5), "curl: (5) Could not resolve proxy", false),
            Retry
        );
        assert_eq!(next_attempt(Some(22), &curl_err(503), false), Retry);
        assert_eq!(next_attempt(Some(22), &curl_err(408), false), Retry);
        assert_eq!(
            next_attempt(Some(7), "curl: (7) Failed to connect", false),
            Stop
        );
        assert_eq!(next_attempt(Some(22), &curl_err(416), false), Stop);
        assert_eq!(
            next_attempt(Some(35), "curl: (35) SSL connect error", false),
            Stop
        );
    }

    /// THE RESUME BUDGET'S BOUNDS. Sixteen resumes after attempts that moved under 1 MiB, not
    /// seventeen; an attempt that moved 1 MiB or more resumes past them; and every resume
    /// counts toward `16 + cap / 1 MiB`, so the call ends however the bytes fall.
    #[test]
    fn a_resume_is_free_of_the_short_budget_only_after_an_attempt_that_moved_a_mebibyte() {
        let mib: u64 = 1 << 20;
        let cap: u64 = 745_000_000;
        assert!(resume_allowed(1000, 15, 15, cap));
        assert!(!resume_allowed(1000, 16, 16, cap));
        assert!(resume_allowed(mib, 16, 300, cap));
        assert!(!resume_allowed(mib - 1, 16, 300, cap));
        let all = 16 + u32::try_from(cap / mib).unwrap();
        assert!(resume_allowed(mib, 0, all - 1, cap));
        assert!(!resume_allowed(mib, 0, all, cap));
        // A payload under 1 MiB keeps the sixteen, and no more.
        assert!(resume_allowed(mib, 15, 15, 4096));
        assert!(!resume_allowed(mib, 16, 16, 4096));
    }

    /// The directory [`probe_a_transfer_over_real_curl`] downloads into, set only for the
    /// children [`start_transfer`] starts.
    #[cfg(unix)]
    const RESUME_PROBE_ENV: &str = "ATERM_TEST_RESUME_PROBE_DIR";
    /// Which file lane that probe downloads with: `part` ([`download_to_resumable`]) or
    /// `dest` ([`super::download_to`]).
    #[cfg(unix)]
    const RESUME_PROBE_LANE: &str = "ATERM_TEST_RESUME_PROBE_LANE";
    /// The size of the payload that probe downloads, which is also its cap.
    #[cfg(unix)]
    const RESUME_PROBE_SIZE: &str = "ATERM_TEST_RESUME_PROBE_SIZE";
    /// That probe's name, as `--exact` wants it.
    #[cfg(unix)]
    const RESUME_PROBE: &str = "http::tests::probe_a_transfer_over_real_curl";

    /// The probe's payload of `size` bytes, the same for the server and the check.
    #[cfg(unix)]
    fn payload(size: usize) -> Vec<u8> {
        (0..size).map(|i| (i * 31 % 251) as u8).collect()
    }

    /// The child's half: one download over a PRODUCTION file lane — its argv, its retries,
    /// its file — whose curl is the real one behind the parent's wrapper
    /// ([`super::CURL_SEAM`]), pointed at the parent's server. Nothing without the parent's
    /// directory.
    #[cfg(unix)]
    #[test]
    fn probe_a_transfer_over_real_curl() {
        let Some(dir) = std::env::var_os(RESUME_PROBE_ENV) else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        let size: u64 = std::env::var(RESUME_PROBE_SIZE).unwrap().parse().unwrap();
        let url = "https://github.com/alabsystems/aterm/releases/download/atpkg-a-1/a-1.bin";
        let dest = dir.join("a-1.bin");
        let got = if std::env::var_os(RESUME_PROBE_LANE).is_some_and(|lane| lane == "dest") {
            super::download_to(url, &dest, size)
        } else {
            download_to_resumable(url, &dest, size)
        };
        let said = match got {
            Ok(()) => "ok".to_string(),
            Err(e) => format!("err {e}"),
        };
        // Publish a complete result before waiting for the owning parent to stop
        // this stand-in. It must not exit/reap before the parent kills its group.
        let pending = dir.join("said.pending");
        std::fs::write(&pending, said).unwrap();
        std::fs::rename(pending, dir.join("said")).unwrap();
        loop {
            std::thread::park();
        }
    }

    /// What the test server answers one request with ([`start_transfer`]).
    #[cfg(all(unix, any(debug_assertions, feature = "dev-seams")))]
    #[derive(Clone, Copy)]
    enum Answer {
        /// `503`, no body.
        Busy,
        /// The payload from the requested offset (`200` from byte 0, `206` past it),
        /// announcing all of the rest but sending at most `send` bytes of it; then, when
        /// `hold`, nothing at all — open, no byte, no FIN, no reset — until the client
        /// closes, else a close.
        Body { send: usize, hold: bool },
    }

    /// The transfer's one server thread. Shutdown closes its current connection before
    /// joining, including while a request or a deliberately held body is being read.
    #[cfg(all(unix, any(debug_assertions, feature = "dev-seams")))]
    struct TransferServer {
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
        current: std::sync::Arc<std::sync::Mutex<Option<std::net::TcpStream>>>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    #[cfg(all(unix, any(debug_assertions, feature = "dev-seams")))]
    impl Drop for TransferServer {
        fn drop(&mut self) {
            self.stop.store(true, std::sync::atomic::Ordering::Release);
            let current = self
                .current
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(conn) = current.as_ref() {
                let _ = conn.shutdown(std::net::Shutdown::Both);
            }
            drop(current);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    /// One transfer in flight: a server answering its `n`th request with `script(n)`, and a
    /// child running the probe on `lane` against it.
    #[cfg(all(unix, any(debug_assertions, feature = "dev-seams")))]
    struct Transfer {
        lane: &'static str,
        /// The payload's size, which is also the probe's cap.
        size: usize,
        child: Option<aterm_uds::spawnfd::Child>,
        ranges: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        _server: TransferServer,
        dir: aterm_tempfile::TempDir,
    }

    #[cfg(all(unix, any(debug_assertions, feature = "dev-seams")))]
    impl Transfer {
        /// End the owned group before the one reap. No numeric signal follows a
        /// reap. This handle has never waited: its unreaped pid reserves the group
        /// identity even if the probe failed before publishing its completion.
        fn reap_probe(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
            if self.child.is_none() {
                return Ok(None);
            }
            let Some(mut child) = self.child.take() else {
                return Ok(None);
            };
            let pid = i32::try_from(child.id()).map_err(std::io::Error::other)?;
            if pid <= 1 {
                return Err(std::io::Error::other("the probe must lead its own group"));
            }
            // SAFETY: process_group(0) made this exclusively held, still-unreaped
            // child its group leader; no wait above released its pid for reuse.
            let killed = unsafe { libc::kill(-pid, libc::SIGKILL) };
            let group_error = (killed != 0).then(std::io::Error::last_os_error);
            let _ = child.kill();
            let status = child.wait()?;
            if let Some(error) = group_error
                && error.raw_os_error() != Some(libc::ESRCH)
            {
                return Err(error);
            }
            Ok(Some(status))
        }
    }

    #[cfg(all(unix, any(debug_assertions, feature = "dev-seams")))]
    impl Drop for Transfer {
        fn drop(&mut self) {
            let _ = self.reap_probe();
            // Fields then stop/join the server before the private directory drops.
        }
    }

    /// What a fresh attempt's quick stall bound (`--speed-time`) is cut to behind
    /// [`start_transfer`]'s curl. curl's speed is a moving average over its last five seconds,
    /// so an attempt notices a silence about `--speed-time` after its average falls under the
    /// floor; two seconds leaves a fresh attempt's first byte a hundred times the loopback
    /// round trip, on a loaded machine too.
    #[cfg(all(unix, any(debug_assertions, feature = "dev-seams")))]
    const FRESH_STALL_SECS: u32 = 2;

    /// Start a [`Transfer`] of a `size`-byte payload on `lane`, with `stale` already standing
    /// at the destination when given.
    #[cfg(all(unix, any(debug_assertions, feature = "dev-seams")))]
    fn start_transfer(
        lane: &'static str,
        size: usize,
        stale: Option<&[u8]>,
        script: fn(usize) -> Answer,
    ) -> Transfer {
        use std::io::{Read as _, Write as _};
        use std::os::unix::fs::PermissionsExt as _;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let ranges = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let seen = std::sync::Arc::clone(&ranges);
        listener.set_nonblocking(true).unwrap();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let current = std::sync::Arc::new(std::sync::Mutex::new(None));
        let stopping = std::sync::Arc::clone(&stop);
        let serving = std::sync::Arc::clone(&current);
        // Every request is answered from this one payload: built per request, a 20 MiB body
        // cut twenty times cost seconds of unoptimized generation before curl saw a byte.
        let body = payload(size);
        let thread = std::thread::spawn(move || {
            let mut n = 0;
            while !stopping.load(std::sync::atomic::Ordering::Acquire) {
                let mut conn = match listener.accept() {
                    Ok((conn, _)) => conn,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                        continue;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => return,
                };
                conn.set_nonblocking(false).unwrap();
                {
                    let mut serving = serving.lock().unwrap();
                    *serving = Some(conn.try_clone().unwrap());
                    if stopping.load(std::sync::atomic::Ordering::Acquire) {
                        let _ = conn.shutdown(std::net::Shutdown::Both);
                        break;
                    }
                }
                let seen = std::sync::Arc::clone(&seen);
                let body = &body;
                (move || {
                    let mut head = Vec::new();
                    let mut buf = [0u8; 4096];
                    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                        match conn.read(&mut buf) {
                            Ok(0) | Err(_) => return,
                            Ok(k) => head.extend_from_slice(&buf[..k]),
                        }
                    }
                    let head = String::from_utf8_lossy(&head).to_ascii_lowercase();
                    let range = head
                        .lines()
                        .find_map(|l| l.strip_prefix("range: ").map(str::to_string))
                        .unwrap_or_else(|| "none".to_string());
                    seen.lock().unwrap().push(range.clone());
                    let Answer::Body { send, hold } = script(n) else {
                        let _ = conn.write_all(
                            b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\
                              Connection: close\r\n\r\n",
                        );
                        return;
                    };
                    let from = range
                        .strip_prefix("bytes=")
                        .and_then(|r| r.trim_end_matches('-').parse::<usize>().ok())
                        .unwrap_or(0)
                        .min(size);
                    let reply = if from == 0 {
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {size}\r\nConnection: close\r\n\r\n"
                        )
                    } else {
                        format!(
                            "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\n\
                             Content-Range: bytes {from}-{}/{size}\r\nConnection: close\r\n\r\n",
                            size - from,
                            size - 1
                        )
                    };
                    let _ = conn.write_all(reply.as_bytes());
                    let _ = conn.write_all(&body[from..size.min(from.saturating_add(send))]);
                    if hold {
                        while matches!(conn.read(&mut buf), Ok(k) if k > 0) {}
                    }
                })();
                serving.lock().unwrap().take();
                n += 1;
            }
        });
        let server = TransferServer {
            stop,
            current,
            thread: Some(thread),
        };
        let dir = aterm_tempfile::tempdir().unwrap();
        if let Some(stale) = stale {
            std::fs::write(dir.path().join("a-1.bin"), stale).unwrap();
        }
        let wrapper = dir.path().join("curl");
        // The real curl, every option as the lane wrote it, the URL swapped for the server's
        // — but for a fresh attempt's quick stall bound, cut to `FRESH_STALL_SECS`, so a
        // silent hold is noticed in seconds. Only the quick bound of an attempt from byte 0:
        // an attempt that continues a file, and a patient bound, run as the lane wrote them.
        let quick = StallBound::Quick.secs();
        std::fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nunset http_proxy HTTP_PROXY https_proxy HTTPS_PROXY all_proxy ALL_PROXY\n\
                 fresh=1\nfor a do [ \"$a\" = --continue-at ] && fresh=0; done\n\
                 n=$#; i=0; prev=\nfor a do\n  i=$((i+1))\n  [ $i -eq 1 ] && set --\n  \
                 if [ $i -eq $n ]; then set -- \"$@\" http://127.0.0.1:{port}/a-1.bin; \
                 elif [ $fresh = 1 ] && [ \"$prev\" = --speed-time ] && [ \"$a\" = {quick} ]; \
                 then set -- \"$@\" {FRESH_STALL_SECS}; \
                 else set -- \"$@\" \"$a\"; fi\n  prev=$a\ndone\nexec /usr/bin/curl \"$@\"\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
        let child = aterm_uds::spawnfd::command(std::env::current_exe().unwrap())
            .args(["--exact", "--nocapture", RESUME_PROBE])
            .process_group(0)
            .env(RESUME_PROBE_ENV, dir.path())
            .env(RESUME_PROBE_LANE, lane)
            .env(RESUME_PROBE_SIZE, size.to_string())
            .env(super::CURL_SEAM, &wrapper)
            // A retry's backoff at a thousandth (`backoff_before`): what these transfers
            // pin is which requests are made, never the seconds between them.
            .env(aterm_types::domain::ENV_DEV_FAST_TIMERS, "1")
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .stderr(aterm_uds::spawnfd::Stdio::null())
            .spawn()
            .unwrap();
        Transfer {
            lane,
            size,
            child: Some(child),
            ranges,
            _server: server,
            dir,
        }
    }

    /// Wait for `run`'s probe to finish and return what its download said: `ok`, or `err`
    /// and the error.
    ///
    /// The wait is a one-minute hang detector, never a latency budget, and it times a STEP,
    /// not the whole transfer: it fires only after a minute in which the transfer moved not
    /// at all — no new request reached the server and no byte reached its file. A transfer
    /// of twenty attempts, each a fresh curl, went past one minute in all under a loaded
    /// workspace run while every attempt moved (and passed alone), so a bound on the whole
    /// call read a slow machine as a hang.
    ///
    /// Progress alone could feed that detector forever, so it also fails the call the moment
    /// the server has been asked more often than the lane's loop may ask it: every resume
    /// `resume_allowed` grants, the first attempt and the one fresh retry after a refused
    /// range, each after at most `CURL_ATTEMPTS - 1` retries in a row (the TERMINATION note in
    /// `download_to_resumable_with`). A lane that asks in a loop fails here at once, not never.
    #[cfg(all(unix, any(debug_assertions, feature = "dev-seams")))]
    fn finished(run: &mut Transfer) -> String {
        const HANG_DETECTOR: std::time::Duration = std::time::Duration::from_secs(60);
        let resumes = u64::from(super::SHORT_RESUMES_PER_CALL)
            + u64::try_from(run.size).unwrap() / super::RESUME_EARNED_AT;
        let ceiling = usize::try_from((resumes + 2) * u64::from(super::CURL_ATTEMPTS)).unwrap();
        let dir = run.dir.path().to_path_buf();
        let moved = |run: &Transfer| {
            let size = |name: &str| std::fs::metadata(dir.join(name)).map_or(0, |m| m.len());
            (
                run.ranges.lock().unwrap().len(),
                size("a-1.bin.part"),
                size("a-1.bin"),
            )
        };
        let mut last = moved(run);
        let mut deadline = std::time::Instant::now() + HANG_DETECTOR;
        let said = loop {
            match std::fs::read_to_string(dir.join("said")) {
                Ok(said) => break said,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => panic!("{}: reading the probe's completion: {error}", run.lane),
            }
            let now = moved(run);
            if now.0 > ceiling {
                panic!(
                    "{}: the server was asked {} times, past the {ceiling} the lane's loop \
                     allows one call; requests: {:?}",
                    run.lane,
                    now.0,
                    run.ranges.lock().unwrap()
                );
            }
            if now != last {
                last = now;
                deadline = std::time::Instant::now() + HANG_DETECTOR;
            } else if std::time::Instant::now() > deadline {
                panic!(
                    "{}: the transfer moved nothing for {}s and never finished; requests: {:?}",
                    run.lane,
                    HANG_DETECTOR.as_secs(),
                    run.ranges.lock().unwrap()
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        use std::os::unix::process::ExitStatusExt as _;
        let status = run.reap_probe().unwrap().expect("the probe is still owned");
        assert_eq!(
            status.signal(),
            Some(libc::SIGKILL),
            "{}: {status}",
            run.lane
        );
        said
    }

    /// A SILENT DROP, OVER REAL CURL, on both file lanes at once: the resumable `.part` lane
    /// (`atpkg`'s archives) and the app container's (`download_to`). The server's first
    /// connection moves the first 2000 bytes of 4 MiB and then goes silent — open, no byte, no
    /// FIN, no reset: a Wi-Fi switch, a NAT timeout, a sleeping peer. Each lane must notice
    /// within its quick stall bound (cut to [`FRESH_STALL_SECS`] by the server's curl) and
    /// continue its file in the same call, so its server sees exactly two requests, the second
    /// for `bytes=2000-`, and the file is whole. The bytes before the silence are under curl's
    /// 4 KiB/s floor, so its five-second average has no burst to drain before the bound
    /// counts. On the lanes before this, curl waited 120 s for the stall and then retried from
    /// byte 0: the hang detector fires, and a second request carries no range.
    #[cfg(all(unix, any(debug_assertions, feature = "dev-seams")))]
    #[test]
    fn a_silently_held_transfer_continues_within_the_call() {
        const SIZE: usize = 4 << 20;
        const HELD_AT: usize = 2000;
        let script = |n| Answer::Body {
            send: if n == 0 { HELD_AT } else { usize::MAX },
            hold: n == 0,
        };
        let mut runs = [
            start_transfer("part", SIZE, None, script),
            start_transfer("dest", SIZE, None, script),
        ];
        for run in &mut runs {
            let lane = run.lane;
            assert_eq!(finished(run), "ok", "{lane}");
            assert_eq!(
                *run.ranges.lock().unwrap(),
                ["none".to_string(), format!("bytes={HELD_AT}-")],
                "{lane}: one fresh request, then one continuing the file"
            );
            let dir = run.dir.path();
            assert!(
                std::fs::read(dir.join("a-1.bin")).unwrap() == payload(SIZE),
                "{lane}: the continued file is the whole payload, byte for byte"
            );
            assert!(!dir.join("a-1.bin.part").exists(), "{lane}");
        }
    }

    /// THE RESUME BUDGET, OVER REAL CURL. A link that keeps moving — every connection cut
    /// after a little more than 1 MiB, twenty cuts in a 20 MiB payload — is continued to the
    /// end in one call: a flat budget of sixteen failed it on the seventeenth cut, a slow
    /// link's install lost to hiccups the 120 s bound before never even noticed. NEGATIVE
    /// CONTROL, the same budget's other half: a host that cuts every connection after 1000
    /// bytes is asked exactly seventeen times (one request and sixteen resumes) and the call
    /// fails, keeping the `.part` it moved.
    #[cfg(all(unix, any(debug_assertions, feature = "dev-seams")))]
    #[test]
    fn a_link_that_keeps_moving_is_continued_and_one_that_cuts_short_is_not_asked_in_a_loop() {
        const MOVING: usize = 20 << 20;
        const STEP: usize = (1 << 20) + 1024;
        const SHORT: usize = 4 << 20;
        let mut moving = start_transfer("part", MOVING, None, |_| Answer::Body {
            send: STEP,
            hold: false,
        });
        let mut short = start_transfer("part", SHORT, None, |_| Answer::Body {
            send: 1000,
            hold: false,
        });
        assert_eq!(finished(&mut moving), "ok");
        let asked = moving.ranges.lock().unwrap().clone();
        assert_eq!(asked.len(), MOVING.div_ceil(STEP), "{asked:?}");
        assert_eq!(asked[19], format!("bytes={}-", 19 * STEP));
        assert!(std::fs::read(moving.dir.path().join("a-1.bin")).unwrap() == payload(MOVING));

        let said = finished(&mut short);
        assert!(said.starts_with("err curl download failed"), "{said}");
        let asked = short.ranges.lock().unwrap().clone();
        assert_eq!(asked.len(), 17, "{asked:?}");
        assert_eq!(
            std::fs::read(short.dir.path().join("a-1.bin.part")).unwrap(),
            payload(17 * 1000)
        );
    }

    /// A FILE ALREADY AT `download_to`'s DESTINATION IS NOT THIS CALL'S PROGRESS. Its first
    /// attempt is answered 503 — under `-f` curl writes nothing, so the 2 MiB that stood at
    /// `dest` before the call are still there — and the lane must retry from byte 0 as for
    /// any transient status, never read those bytes as an attempt that moved and continue
    /// them at `bytes=2097152-`: that file would be 2 MiB of something else in front of the
    /// payload's tail.
    #[cfg(all(unix, any(debug_assertions, feature = "dev-seams")))]
    #[test]
    fn bytes_already_at_the_destination_are_never_continued() {
        const SIZE: usize = 4 << 20;
        let stale = vec![0xEE_u8; 2 << 20];
        let mut run = start_transfer("dest", SIZE, Some(&stale), |n| {
            if n == 0 {
                Answer::Busy
            } else {
                Answer::Body {
                    send: usize::MAX,
                    hold: false,
                }
            }
        });
        assert_eq!(finished(&mut run), "ok");
        assert_eq!(*run.ranges.lock().unwrap(), ["none", "none"]);
        assert!(std::fs::read(run.dir.path().join("a-1.bin")).unwrap() == payload(SIZE));
    }

    /// The VENDOR lane pins the scheme on BOTH hops: `--proto =https` for the initial
    /// request and `--proto-redir =https` for every redirect — and is otherwise the
    /// plain resumable argv, byte for byte (the release lanes keep theirs unchanged).
    #[test]
    fn the_vendor_lane_pins_https_on_the_first_hop_and_every_redirect() {
        let quick = StallBound::Quick;
        let plain = download_resume_args("100", "600", "/s/claude.part", None, quick);
        let pinned = download_resume_args_https_only("100", "600", "/s/claude.part", None, quick);
        assert_eq!(&pinned[..2], ["--proto", "=https"]);
        assert_eq!(
            &pinned[2..],
            &plain[..],
            "everything after the pin is the plain argv"
        );
        let redir = pinned
            .iter()
            .position(|a| *a == "--proto-redir")
            .expect("redirect pin present");
        assert_eq!(pinned[redir + 1], "=https");
        // The release lane carries no `--proto`.
        assert!(!plain.contains(&"--proto"), "{plain:?}");
        // Resume flags ride behind the pin exactly as before.
        let resumed =
            download_resume_args_https_only("60", "600", "/s/claude.part", Some("40"), quick);
        assert_eq!(resumed.len(), pinned.len() + 2);
        assert_eq!(&resumed[resumed.len() - 2..], ["--continue-at", "40"]);
        // The scheme gate refuses a non-https URL before spawning.
        let dir = std::env::temp_dir().join(format!("aterm-vendor-lane-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let err = download_to_resumable_https_only("http://example.invalid/x", &dir.join("x"), 10)
            .unwrap_err();
        assert!(err.contains("non-https"), "{err}");
        let err = download_to_resumable("ftp://example.invalid/x", &dir.join("x"), 10).unwrap_err();
        assert!(err.contains("non-https"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A failed attempt keeps its prefix only if it moved.
    #[test]
    fn a_failed_attempt_that_made_no_progress_discards_its_prefix() {
        assert!(keep_partial(0, 1));
        assert!(keep_partial(600_000_000, 629_000_000));
        assert!(!keep_partial(0, 0), "nothing arrived");
        assert!(!keep_partial(600_000_000, 600_000_000), "a dead offset");
        assert!(
            !keep_partial(600_000_000, 4),
            "a truncated/clobbered prefix"
        );
    }

    /// …except after a web-host 403/404, a verdict about the URL, which spares the prefix.
    #[test]
    fn a_web_host_url_verdict_spares_the_prefix_the_other_lane_resumes() {
        // What a stalled API-lane attempt left in `<dest>.part`.
        const STALLED: u64 = 500_000_000;
        const WEB: &str = "https://github.com/alabsystems/orc-private/releases/download/\
                           atpkg-orc-77/orc-77.tar.zst";
        const API: &str = "https://api.github.com/repos/alabsystems/orc-private/releases/assets/1";

        // Non-vacuity: the progress rule alone discards it.
        assert!(!keep_partial(STALLED, STALLED));
        for code in [403, 404] {
            assert!(
                keep_partial_after_failure(WEB, &curl_err(code), STALLED, STALLED),
                "a web-host {code} answers about the URL, not about the prefix"
            );
        }

        // Everything else keeps the anti-wedge rule exactly.
        assert!(
            !keep_partial_after_failure(WEB, &curl_err(416), STALLED, STALLED),
            "416: the prefix IS what was refused"
        );
        assert!(
            !keep_partial_after_failure(WEB, &curl_err(429), STALLED, STALLED),
            "a rate limit is the same URL again in a minute, not the other lane"
        );
        assert!(
            !keep_partial_after_failure(WEB, &curl_err(502), STALLED, STALLED),
            "a 5xx is the retry path, not a verdict"
        );
        assert!(
            !keep_partial_after_failure(WEB, "curl: (28) Operation too slow", STALLED, STALLED),
            "a stall that moved nothing still discards"
        );
        assert!(
            !keep_partial_after_failure(API, &curl_err(404), STALLED, STALLED),
            "an API-host 404 is the historical retry path, not a URL verdict"
        );
        // …and an attempt that moved keeps its prefix on any host.
        for url in [WEB, API] {
            assert!(keep_partial_after_failure(
                url,
                &curl_err(404),
                STALLED,
                STALLED + 1
            ));
        }
    }

    /// Only a failure naming the range (curl 33; a 416 through `--fail`) earns the in-call
    /// fresh retry; anything else would fail from offset 0 too.
    #[test]
    fn only_a_range_refusal_earns_the_in_call_fresh_retry() {
        assert!(
            range_refused(Some(33), ""),
            "curl 33: server refuses ranges"
        );
        assert!(
            range_refused(Some(22), "curl: (22) The requested URL returned error: 416"),
            "416: the offset is past what the upstream object now holds"
        );
        assert!(
            !range_refused(Some(28), "curl: (28) Operation too slow"),
            "a stall recurs from offset 0 too"
        );
        assert!(
            !range_refused(Some(22), "curl: (22) The requested URL returned error: 404"),
            "a missing asset is not a range problem"
        );
        assert!(
            !range_refused(Some(22), "curl: (22) The requested URL returned error: 429"),
            "a rate limit must reach the rate-limit classifier, not a retry"
        );
        assert!(
            !range_refused(None, ""),
            "a signal-killed curl proves nothing"
        );
    }
}
