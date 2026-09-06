// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The network layer: `curl` calls to GitHub — the Releases API when a token is
//! available, and the unmetered web host (`github.com/…/releases/…`) with no
//! credential at all. Artifact-agnostic — these fetch arbitrary API JSON, asset bytes
//! and redirect headers; the consuming crate decides what the bytes mean.
//!
//! Two HOSTS, MEASURED 2026-09-02/03 against the shipped channel. `api.github.com` is
//! metered (~60 requests/hour per IP anonymously, 5000 with a token) and is used ONLY on
//! the token lane: the releases LIST and the asset API URL (`…/releases/assets/<id>`,
//! `Accept: application/octet-stream`, the credential on stdin; curl `-L` follows the 302
//! to storage and drops the `Authorization` header on the cross-host redirect by default).
//! `github.com` is not metered at all: for a PUBLIC repo
//! `https://github.com/{owner}/{repo}/releases/download/{tag}/{name}` answers 200
//! anonymously via a 302 to `release-assets.githubusercontent.com` with no
//! `x-ratelimit-*` headers, `…/releases/latest/download/{name}` answers a 302 whose
//! `Location` names the newest published release's tag ([`head_no_redirect`]), a missing
//! asset answers 404, and a PRIVATE repo answers 404 on both URL shapes (no credential is
//! accepted there — and none is ever sent: [`refuse_credential_off_api`]). This module
//! classifies the answer PER HOST — a 403 is a rate limit on the API host and a blocked
//! host on the web one.
//!
//! # Why the token is OPTIONAL
//!
//! A private repo cannot be read without one, but a PUBLIC one can — and aterm's
//! shipped update channel is public. Making the token mandatory here is what made a
//! freshly installed Mac refuse to even ask: the caller returned before any network
//! call, so a repo it could have read anonymously looked like "no updates, forever".
//! `token: Option<&str>` splits the two lanes explicitly:
//!
//! * `Some(t)` — byte-for-byte the historical path. The token is fed to curl through
//!   STDIN ([`curl_fetch`], `curl --config -`), NEVER on argv, so it is not exposed
//!   to same-user processes via `ps`.
//! * `None` — the `--config -` channel is omitted from the argv entirely (not passed
//!   an empty config: an absent option cannot be mis-parsed) and stdin is
//!   `/dev/null`, so curl can never block on an EOF nobody will send. Every OTHER
//!   hardening is unchanged — `-q` first, the scrubbed config-dir env, the `--`
//!   end-of-options marker — because those defend against a hostile curlrc and a
//!   server-controlled URL, which have nothing to do with authentication.
//!
//! An anonymous API caller is rate-limited to ~60 requests/hour PER IP (5000/hour with
//! a token), so [`HttpError`] classifies that state separately: a rate limit is not an
//! auth failure and must not be reported as one. The web lane never meets it — the only
//! throttle `github.com` has is a 429 of its own.

use std::path::Path;
use std::process::Command;

/// A classified GitHub API failure. [`api_get`] flattens this to the historical
/// `String`; [`api_get_classified`] hands it over intact so a caller can tell
/// "you need a credential" from "slow down" from "the network is down" — a
/// distinction the token-optional updater has to make on EVERY check, since the
/// same 404 means "private repo, no token" and "repo does not exist".
///
/// [`std::fmt::Display`] reproduces the historical message for each arm verbatim, so
/// no log line, status string, or test wording changes with the classification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpError {
    /// curl itself failed (exit != 0): DNS, TLS, timeout, or a refused spawn.
    Transport(String),
    /// HTTP 429, or a 403 whose body names a (primary or secondary) rate limit.
    /// TRANSIENT: the credential — or the lack of one — is not the problem.
    RateLimited {
        code: u16,
        url: String,
        /// Whether the request carried a token. Anonymous rate limits are ordinary
        /// (~60/hour per IP) and need different advice than an authenticated one.
        authenticated: bool,
    },
    /// HTTP 401, or a 403 that is NOT a rate limit: the credential is missing,
    /// expired, revoked, or lacks access.
    Unauthorized { code: u16 },
    /// HTTP 404. GitHub deliberately returns this both for a private repo the caller
    /// cannot see AND for a repo that does not exist — the two are indistinguishable
    /// over the API, so the classification stops here and the caller must say so.
    NotFound { url: String },
    /// Any other non-2xx status.
    Status { code: u16, url: String },
    /// The `-w`-appended status trailer was not a number: a proxy/portal mangled the
    /// response. Carries the whole historical message.
    Malformed(String),
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(message) | Self::Malformed(message) => f.write_str(message),
            Self::RateLimited {
                code,
                url,
                authenticated: true,
            } => write!(
                f,
                "GitHub rate limit hit (HTTP {code}) for {url}; transient (the token is \
                 valid) — backing off, will retry on the next check"
            ),
            // The authenticated wording ("the token is valid") would be a lie for a
            // credential-less API caller (atpkg's index listing when no pointer
            // resolves — the app updater never calls the API without a token), where
            // the ~60/hour per-IP budget is the whole story, including for several
            // machines behind one NAT.
            Self::RateLimited {
                code,
                url,
                authenticated: false,
            } => write!(
                f,
                "GitHub rate limit hit (HTTP {code}) for {url}; the unauthenticated API \
                 allows ~60 requests/hour per IP address — backing off, will retry on the \
                 next check"
            ),
            Self::Unauthorized { code } => write!(
                f,
                "GitHub auth failed (HTTP {code}): the update token is missing required \
                 access, expired, or was revoked — rotate it (see docs/RELEASING.md)"
            ),
            Self::NotFound { url } => write!(
                f,
                "GitHub returned HTTP 404 for {url} (repo/releases not found, or the token \
                 lacks access to this private repo)"
            ),
            Self::Status { code, url } => {
                write!(f, "GitHub API returned HTTP {code} for {url}")
            }
        }
    }
}

/// Whether `token` is safe to interpolate into a curl config-file line. The token is
/// delivered as `header = "Authorization: Bearer <token>"` on curl's `--config -`
/// stdin; a token carrying a quote, backslash, or newline could close the quoted
/// value and inject additional curl directives (e.g. `insecure` to disable TLS, or
/// `proxy = http://attacker/` to exfiltrate). A real GitHub PAT is `[A-Za-z0-9_.-]+`,
/// so we reject any control character, quote, or backslash — defense in depth even
/// though the token comes from a trusted source (env / keychain / 0600 file).
fn token_config_safe(token: &str) -> bool {
    !token
        .bytes()
        .any(|b| b < 0x20 || b == 0x7f || b == b'"' || b == b'\\')
}

/// Build curl's FULL parameter list (everything after the binary name): `-q` first,
/// the caller's options, the fixed User-Agent, the `--config -` stdin auth channel
/// (only when `authenticated`), and LAST `--` + the URL. Pure and separate from the
/// spawn so the ordering — the security- AND correctness-critical part — is
/// unit-testable.
///
/// ORDERING INVARIANTS (each guards against a real failure):
/// * `-q` is the very first parameter (see [`curl_fetch`] — booby-trapped curlrc).
/// * `--config -` comes BEFORE the `--` end-of-options marker: everything after
///   `--` is a URL to curl, so a misplaced marker silently DISABLES authentication
///   and turns the remaining options into bogus URLs. Exactly that shipped in
///   v0.5.10/v0.5.11 (the caller-side `--` in `download_bytes`/`download_to`
///   preceded the appended auth args): every private-repo asset download failed
///   404-unauthenticated, bricking auto-update on those builds.
/// * `--` immediately precedes the URL: the asset URL from the releases JSON is the
///   one fully server-controlled string that reaches curl, so a leading-dash value
///   (`-K/tmp/evil`) must parse as a URL, never as an option.
/// * When `authenticated` is false the auth channel is OMITTED, not emptied: there
///   is no `--config` at all, so an anonymous request carries no `Authorization`
///   header and cannot be turned into one by a mangled config stream.
///
/// The `curl` binary to spawn. On unix curl lives at the well-known absolute path
/// `/usr/bin/curl`, which we spawn verbatim so a `PATH`-injected shim can never be
/// run in its place. Windows has no fixed install location but ships curl since
/// Win10 1803 as `curl.exe` on `PATH`, so we resolve it by name there (there is no
/// trusted absolute path to pin, and `Command::new` does NOT consult PowerShell's
/// `curl`→`Invoke-WebRequest` alias — only real executables on `PATH`). The argv
/// assembled by [`curl_argv`] is identical on every platform.
#[cfg(not(windows))]
fn curl_bin() -> &'static str {
    "/usr/bin/curl"
}

#[cfg(windows)]
fn curl_bin() -> &'static str {
    "curl.exe"
}

// Skip: Vec growth (`extend`) — the audited-alloc class; capacity is
// clamped (see below) and the argv is bounded by the caller's fixed flag
// sets. Droppable when the T3 collect/extend layer lands.
#[cfg_attr(trust_verify, trust::skip)]
fn curl_argv(args: &[&str], url: &str, authenticated: bool) -> Vec<String> {
    // The capacity is a pre-size HINT only; clamp it so the `+ 7` and the resulting
    // allocation size are provably panic-free for any abstract `args` (the verifier
    // refutes the unclamped form with a huge unconstrained slice length). Every
    // caller in this crate passes a fixed option list of <= 19 items, so the clamp
    // never binds on a real path — and even if it ever did, `Vec` growth in `extend`
    // /`push` keeps the returned contents identical.
    let mut v = Vec::with_capacity(args.len().min(32) + 7);
    v.push("-q".to_string());
    v.extend(args.iter().map(|s| (*s).to_string()));
    v.extend(
        ["-H", "User-Agent: aterm-update"]
            .iter()
            .map(|s| (*s).to_string()),
    );
    if authenticated {
        v.extend(["--config", "-"].iter().map(|s| (*s).to_string()));
    }
    v.push("--".to_string());
    v.push(url.to_string());
    v
}

/// Validate the credential and build the EXACT process [`curl_fetch`] spawns, minus
/// only the stdio wiring and the stdin write.
///
/// This is the testable seam for C3. The guarantee "the token never reaches argv" has
/// to be checked where the process is actually assembled: asserting it on
/// [`curl_argv`] alone is weaker than it looks, because `curl_argv` is never GIVEN the
/// token and so cannot leak it no matter what. A `.arg("-H").arg(format!(
/// "Authorization: Bearer {t}"))` appended in the spawn path — the tempting way to
/// "simplify away" the stdin channel — is invisible to such a test (verified by
/// mutation: the whole suite stayed green). Routing the token through here, and
/// asserting on the resulting argv, closes that.
///
/// Takes the token so that a future change which DOES put it on argv is observable;
/// it must only ever be used to decide `authenticated` and to validate.
fn curl_prepared(args: &[&str], url: &str, token: Option<&str>) -> Result<Command, String> {
    if let Some(token) = token {
        // Fail CLOSED on an empty token rather than emitting a bare `Bearer `: a
        // caller that reached here with `Some("")` has a bug, and a header GitHub
        // reads as a malformed credential is worse than an honest anonymous request
        // (which the `None` lane exists to make).
        if token.is_empty() {
            return Err(
                "update token is empty — refusing to send a bare `Authorization: Bearer` \
                 header (pass no token to request anonymously)"
                    .to_string(),
            );
        }
        if !token_config_safe(token) {
            return Err(
                "update token contains illegal characters (control/quote/backslash) — refusing \
                 to build the curl config line"
                    .to_string(),
            );
        }
    }
    Ok(curl_command(args, url, token.is_some()))
}

/// Build the argv-and-env part of the curl process. See [`curl_prepared`], the seam
/// callers and tests go through.
fn curl_command(args: &[&str], url: &str, authenticated: bool) -> Command {
    let mut command = Command::new(curl_bin());
    command
        // `-q` MUST be first (curl_argv puts it first): curl reads the default
        // ~/.curlrc (or $CURL_HOME / $XDG_CONFIG_HOME) EVEN when `--config` is
        // given, unless `-q` is the very first parameter. A hostile/booby-trapped
        // curlrc could add `insecure` + `proxy = http://attacker/` and exfiltrate
        // the Bearer token we plumb in `curl_fetch`. `-q` disables ONLY the default
        // config file — it does NOT disable the explicit `--config -` stdin that
        // carries the Authorization header, so token delivery is unaffected. Also
        // scrub the config-dir env vars so the default-config lookup cannot be
        // redirected. BOTH lanes keep this: a hostile curlrc is an ambient-environment
        // threat, orthogonal to whether this particular request carries a credential.
        .args(curl_argv(args, url, authenticated))
        .env_remove("CURL_HOME")
        .env_remove("XDG_CONFIG_HOME");
    command
}

/// Run curl against `url` with extra `args`, on one of two lanes.
///
/// `Some(token)` feeds the secret `Authorization` header through STDIN
/// (`curl --config -`) so the token NEVER appears in argv — argv is world-visible to
/// same-user processes via `ps`. `None` omits the auth channel altogether (public
/// channel / no credential provisioned) and gives curl `/dev/null` for stdin, so it
/// cannot block waiting for a config stream that will never be written or closed.
///
/// The URL is passed separately so [`curl_argv`] can place the `--` end-of-options
/// marker directly before it, AFTER every option including the auth channel (callers
/// must NOT put `--` in `args` — that is the v0.5.10 auto-update-bricking
/// regression). Returns the completed process output.
// Skip: same audited display-lossy Err-path class as `api_get`.
#[cfg_attr(trust_verify, trust::skip)]
fn curl_fetch(
    args: &[&str],
    url: &str,
    token: Option<&str>,
) -> Result<std::process::Output, String> {
    use std::io::Write;
    use std::process::Stdio;
    let mut command = curl_prepared(args, url, token)?;
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let Some(token) = token else {
        return command
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("spawn curl: {e}"));
    };
    let mut child = command
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn curl: {e}"))?;
    {
        let mut stdin = child.stdin.take().ok_or("curl stdin unavailable")?;
        stdin
            .write_all(format!("header = \"Authorization: Bearer {token}\"\n").as_bytes())
            .map_err(|e| format!("write curl config: {e}"))?;
    } // drop stdin → EOF so curl proceeds
    child
        .wait_with_output()
        .map_err(|e| format!("curl wait: {e}"))
}

/// [`api_get_classified`], flattened to the historical `String` error. Every message
/// is byte-identical to what this function produced before the classification split
/// (see [`HttpError`]'s `Display`), so existing callers, logs and status text are
/// unchanged.
pub fn api_get(url: &str, token: Option<&str>) -> Result<Vec<u8>, String> {
    api_get_classified(url, token).map_err(|e| e.to_string())
}

/// How many times a request whose BODY is captured from curl's stdout is attempted,
/// in-process. Matches the budget curl's own `--retry 2` used to spend here (one try
/// plus two retries), so the worst-case wall time is unchanged in magnitude.
const CURL_ATTEMPTS: u32 = 3;

/// Whether an HTTP status is worth another in-process attempt: the transient
/// server-side set curl itself calls retryable (`man curl`, `--retry`).
///
/// 429 — and the rate-limited 403 — are deliberately ABSENT. Classification here is
/// code-only, so a retry buys nothing but a second request against a budget that is
/// already exhausted, and hammering GitHub's secondary limit without honouring
/// `Retry-After` is strictly worse than the back-off-and-retry-on-the-next-cycle this
/// layer already documents. Everything else (2xx, 401/403/404, a mangled trailer) is a
/// verdict rather than a blip and is returned on the first attempt.
fn transient_api_status(code: &str) -> bool {
    matches!(code, "408" | "500" | "502" | "503" | "504")
}

/// The fixed option list for [`api_get_classified`], extracted so the flag set itself
/// is assertable in a unit test.
///
/// It carries NO `--retry`, and that omission is load-bearing. The body is captured
/// from curl's STDOUT, and curl truncates only a FILE sink between attempts (a pipe has
/// no filename to `ftruncate`), so a curl-level retry CONCATENATES the failed attempt's
/// error document in front of the good one while `-w` writes the status trailer exactly
/// once — the result parses as a healthy 200 whose JSON then fails with "trailing
/// characters", i.e. a blip curl HAD recovered from is reported as a broken publisher.
/// Reproduced against curl 8.7.1. [`api_get_classified`] retries the whole subprocess
/// instead: a fresh pipe per attempt, so no failed attempt's bytes can survive.
fn api_get_args() -> [&'static str; 11] {
    [
        "-sS",
        "--max-time",
        "30",
        // Bound the buffered-in-memory API response. GitHub API JSON (a releases
        // list / a manifest) is small; 16 MiB is generous headroom while stopping
        // a rogue/oversized response from being read whole into memory, matching
        // the caps download_bytes/download_to already carry.
        "--max-filesize",
        "16777216",
        "-H",
        "Accept: application/vnd.github+json",
        "-H",
        "X-GitHub-Api-Version: 2022-11-28",
        "-w",
        "\n%{http_code}",
    ]
}

/// GET a GitHub API JSON resource, returning the raw body bytes. Distinguishes an
/// authentication failure (401/403 — expired/revoked/insufficient token, or no token
/// against a private repo) from a rate limit and from a transient error, so the
/// caller can act on the difference instead of collapsing it into one string. We
/// append the HTTP status via `-w` and DON'T pass `-f` (we want the code even on 4xx).
///
/// A transport failure or a transient server status is retried up to three times HERE
/// rather than by curl, because each attempt must start from a fresh pipe: curl
/// truncates only a FILE sink between retries, so a curl-level retry CONCATENATES the
/// failed attempt's error document in front of the good body under one `-w` status
/// trailer, and the whole thing then fails JSON parsing as a "broken publisher".
/// See `api_get_args`.
// Skip: response-text handling — from_utf8_lossy over curl output (display/
// classification only; the byte-exact BODY is returned untouched as Vec<u8>)
// and the trailing-status split arithmetic, whose bounds ride the lossy
// Cow (unmodeled). Every malformed shape returns Err (fail-closed).
// Audited (update-atpkg); droppable with the byte-exact contract lane.
#[cfg_attr(trust_verify, trust::skip)]
pub fn api_get_classified(url: &str, token: Option<&str>) -> Result<Vec<u8>, HttpError> {
    // No header sink, therefore an argv that is EXACTLY `api_get_args()` (asserted in
    // `the_plain_lane_argv_is_unchanged`). Every existing caller's bytes, errors,
    // retries and wording are the historical ones.
    api_get_with_headers(url, token, None)
}

/// [`api_get_args`] plus the response-header dump, when the caller wants one.
///
/// Kept as its own function so a unit test can assert BOTH sides: with no sink the list
/// is byte-identical to the historical `api_get_args()`, and with one the flag pair
/// appears exactly once and still BEFORE the `--` end-of-options marker `curl_argv`
/// appends (a caller-side `--` is the v0.5.10 auto-update-bricking regression).
fn api_get_args_dumping(header_dump: Option<&str>) -> Vec<&str> {
    let mut args: Vec<&str> = api_get_args().to_vec();
    if let Some(dump) = header_dump {
        // `--dump-header`, not `-D -`: the body is captured from stdout and the status
        // trailer is appended to it, so response headers must land in a FILE or they
        // would corrupt both. The sink is a caller-owned path inside its own `0700`
        // directory.
        args.push("--dump-header");
        args.push(dump);
    }
    args
}

/// The `x-ratelimit-*` block of a GitHub API response, read back from a curl
/// `--dump-header` capture.
///
/// Every field is optional because every field is server-supplied: a proxy may strip
/// any of them, and a consumer that needs one must treat its absence as "unknown", never
/// as "plenty". `reset` is a unix epoch, already clamped by the parser.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RateLimitHeaders {
    /// `x-ratelimit-limit`: the hourly allowance (~60 anonymous, 5000 with a token).
    pub limit: Option<u32>,
    /// `x-ratelimit-remaining`: what is left in the current window.
    pub remaining: Option<u32>,
    /// `x-ratelimit-used`: what this window has already spent.
    pub used: Option<u32>,
    /// `x-ratelimit-reset`: when the window renews, unix seconds — clamped to
    /// `now + 3600`, because the window is an hour and a value past that is a lie
    /// (a skewed clock, a mangled header) that would otherwise hold a machine off
    /// GitHub indefinitely.
    pub reset: Option<u64>,
}

/// How far past `now` a server-supplied reset epoch is believed. GitHub's window is an
/// hour, so nothing honest can be further out.
const RATE_LIMIT_RESET_HORIZON_SECS: u64 = 3600;

/// Parse the rate-limit headers out of a header dump's LAST block (the response whose
/// body was kept — a redirect chain writes one block per hop, and a block boundary is
/// an `HTTP/` status line). Names are matched case-insensitively (GitHub emits them
/// lowercase; a proxy may not); a non-numeric value is treated as absent, never as 0 or
/// as a guess. `None` when the last block carries none of the four.
///
/// `now` is injected so the clamp is testable without a clock.
#[must_use]
pub fn parse_rate_limit_headers(text: &str, now: u64) -> Option<RateLimitHeaders> {
    let mut found = RateLimitHeaders::default();
    let mut any = false;
    for line in text.lines() {
        // A new hop: everything read so far belonged to a response we did not keep.
        if line.starts_with("HTTP/") {
            found = RateLimitHeaders::default();
            any = false;
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        let value = value.trim();
        if name.eq_ignore_ascii_case("x-ratelimit-limit") {
            found.limit = value.parse().ok();
            any |= found.limit.is_some();
        } else if name.eq_ignore_ascii_case("x-ratelimit-remaining") {
            found.remaining = value.parse().ok();
            any |= found.remaining.is_some();
        } else if name.eq_ignore_ascii_case("x-ratelimit-used") {
            found.used = value.parse().ok();
            any |= found.used.is_some();
        } else if name.eq_ignore_ascii_case("x-ratelimit-reset") {
            found.reset = value
                .parse::<u64>()
                .ok()
                .map(|reset| reset.min(now.saturating_add(RATE_LIMIT_RESET_HORIZON_SECS)));
            any |= found.reset.is_some();
        }
    }
    any.then_some(found)
}

/// [`parse_rate_limit_headers`] over the file curl dumped headers into, against the
/// wall clock. Absent or unreadable file ⇒ `None`.
#[must_use]
pub fn rate_limit_from_header_dump(path: &Path) -> Option<RateLimitHeaders> {
    let raw = std::fs::read(path).ok()?;
    parse_rate_limit_headers(&String::from_utf8_lossy(&raw), unix_now_secs())
}

/// Unix seconds now, `0` on a clock before the epoch (which only makes the reset clamp
/// tighter — the safe direction).
fn unix_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// [`api_get_classified`], additionally dumping the response HEADERS into
/// `header_sink` so the caller can read the `x-ratelimit-*` block back
/// ([`rate_limit_from_header_dump`]) — the token lane's evidence for holding a check
/// until the server's own reset instead of guessing.
///
/// The sink is unlinked before every attempt: a reset epoch a later hold rides on must
/// describe THIS response, never a previous one's. `None` spawns the exact
/// historical argv and touches no file.
// Skip: same audited display-lossy Err-path class as `api_get`.
#[cfg_attr(trust_verify, trust::skip)]
pub fn api_get_with_headers(
    url: &str,
    token: Option<&str>,
    header_sink: Option<&Path>,
) -> Result<Vec<u8>, HttpError> {
    let sink = header_sink.and_then(|p| p.to_str());
    let args = api_get_args_dumping(sink);
    // Bounded: `last` is true on attempt `CURL_ATTEMPTS`, and every branch returns
    // there, so the loop cannot run more than `CURL_ATTEMPTS` times.
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        if attempt > 1 {
            // curl's own inter-retry backoff, preserved: 1 s, then 2 s.
            std::thread::sleep(std::time::Duration::from_secs(1 << (attempt - 2)));
        }
        let last = attempt >= CURL_ATTEMPTS;
        if let Some(sink) = header_sink {
            // Never let a PREVIOUS response's headers be read as THIS one's. curl
            // truncates the dump file on open, so this is belt-and-suspenders — but a
            // reset epoch read out of a response we did not receive is the one way a
            // hold could be pointed at the wrong window. A failure to remove is
            // harmless (worst case: no headers, i.e. no hold, the historical back-off).
            let _ = std::fs::remove_file(sink);
        }
        // The token is passed in unchanged on every attempt — never re-read or
        // re-validated per attempt, so a rotation mid-loop cannot split the lanes.
        let out = curl_fetch(&args, url, token).map_err(HttpError::Transport)?;
        if !out.status.success() {
            if !last {
                continue;
            }
            // Transport-level failure (curl exit != 0): DNS, TLS, timeout, etc.
            return Err(HttpError::Transport(format!(
                "curl GET {} failed ({}): {}",
                url,
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        // Split the trailing "\n<http_code>" we appended via -w.
        let stdout = out.stdout;
        let text = String::from_utf8_lossy(&stdout);
        let (body, code) = match text.rfind('\n') {
            Some(i) => (&text[..i], text[i + 1..].trim()),
            None => ("", text.trim()),
        };
        if code.starts_with('2') {
            return Ok(body.as_bytes().to_vec());
        }
        if !last && transient_api_status(code) {
            // Discard this attempt's bytes ENTIRELY — that discarding is the whole
            // point of retrying out here instead of inside curl.
            continue;
        }
        // GitHub signals rate limiting with 429, or a 403 whose body mentions a
        // (primary or secondary) rate limit. That is TRANSIENT — the credential (or its
        // absence) is not the problem — so it must not be reported as an auth failure
        // ("rotate the token"), and retrying it here would only spend a budget that is
        // already gone; we surface it as back-off-and-retry-next-cycle (F11).
        let rate_limited =
            code == "429" || (code == "403" && body.to_ascii_lowercase().contains("rate limit"));
        let Ok(numeric) = code.parse::<u16>() else {
            // A non-numeric trailer means something mangled the response (captive
            // portal / proxy). Fail closed with the historical wording.
            return Err(HttpError::Malformed(format!(
                "GitHub API returned HTTP {code} for {url}"
            )));
        };
        if rate_limited {
            return Err(HttpError::RateLimited {
                code: numeric,
                url: url.to_string(),
                authenticated: token.is_some(),
            });
        }
        return match numeric {
            401 | 403 => Err(HttpError::Unauthorized { code: numeric }),
            404 => Err(HttpError::NotFound {
                url: url.to_string(),
            }),
            other => Err(HttpError::Status {
                code: other,
                url: url.to_string(),
            }),
        };
    }
}

/// What a redirect-refusing HEAD came back with: the status the web host answered
/// with, and its `Location` header when it sent one.
///
/// Deliberately NOT classified here. A 302 is the evergreen pointer's ordinary answer
/// (`Location` names the newest release), a 404 is "no published release" on this host
/// and a 5xx/429 is weather — but WHICH of those is a verdict is the caller's business
/// ([`crate::pointer`]), and this layer only reports what the wire said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadAnswer {
    /// The HTTP status of the FIRST hop (no redirect is followed).
    pub code: u16,
    /// The `Location` header of that hop, trimmed, if present. Server-controlled text:
    /// the caller validates it under a strict predicate before deriving anything.
    pub location: Option<String>,
}

/// The option list for [`head_no_redirect`], extracted so the flag set is assertable
/// in a unit test.
///
/// `-I` asks for headers only; `--max-redirs 0` makes curl STOP at the first hop and
/// report it (with `-L` absent curl would not follow anyway — the flag pins the intent
/// against a later "helpful" `-L`); the headers land on stdout, followed by the `-w`
/// status trailer, so no file sink is needed. No `-f`: a 404 is an answer this lane
/// reads, not a failure. No `--retry`: the stdout capture is retried per process like
/// the API lane's (a concatenated hop would be parsed as one).
fn head_args() -> [&'static str; 8] {
    [
        "-sS",
        "-I",
        "--max-redirs",
        "0",
        "--max-time",
        "30",
        "-w",
        "\n%{http_code}",
    ]
}

/// The status and `Location` of ONE hop of `url`, anonymously, without following it.
///
/// This is the whole cost of a steady-state check on the web lane: one HEAD to
/// `…/releases/latest/download/<name>`, whose 302 names the newest published release's
/// tag. No credential is ever attached (the argument is not even accepted — `github.com`
/// reads no `Authorization` header and must never be shown one), the scheme must be
/// `https`, and a transport failure or a transient 5xx is retried in-process exactly as
/// [`api_get_classified`] retries, for the same fresh-pipe reason.
// Skip: same audited display-lossy Err-path class as `api_get`.
#[cfg_attr(trust_verify, trust::skip)]
pub fn head_no_redirect(url: &str) -> Result<HeadAnswer, HttpError> {
    require_https_url(url).map_err(HttpError::Transport)?;
    let args = head_args();
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        if attempt > 1 {
            std::thread::sleep(std::time::Duration::from_secs(1 << (attempt - 2)));
        }
        let last = attempt >= CURL_ATTEMPTS;
        let out = curl_fetch(&args, url, None).map_err(HttpError::Transport)?;
        if !out.status.success() {
            if !last {
                continue;
            }
            return Err(HttpError::Transport(format!(
                "curl HEAD {} failed ({}): {}",
                url,
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let (headers, code) = match text.rfind('\n') {
            Some(i) => (&text[..i], text[i + 1..].trim()),
            None => ("", text.trim()),
        };
        if !last && transient_api_status(code) {
            continue;
        }
        let Ok(numeric) = code.parse::<u16>() else {
            return Err(HttpError::Malformed(format!(
                "the release host answered HTTP {code} to HEAD {url}"
            )));
        };
        return Ok(HeadAnswer {
            code: numeric,
            location: location_header(headers),
        });
    }
}

/// The LAST `Location:` header in a header capture, trimmed. (A single hop is captured
/// — `--max-redirs 0` — but the scan is written for the last block regardless, the same
/// way the rate-limit reader is.) `None` when absent or empty.
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

/// Reject any asset URL that is not plain `https://…`. The asset URL comes from the
/// releases JSON's `assets[].url` — the one fully server-controlled string that
/// reaches curl — so, like the API host and the token charset elsewhere in this
/// crate, it must be validated: a `file://` / `ftp://` value would let a hostile or
/// MITM'd response turn the downloader into a local-file / SSRF read. Combined with a
/// literal `--` before the URL in the argv (so a `-K…`-style value can't be parsed as
/// a curl option), this keeps the one untrusted curl input inert.
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
/// Like [`api_get_args`] it carries NO `--retry`: these bytes are captured from curl's
/// stdout, which curl does not truncate between attempts. `-f` makes the concatenation
/// window much narrower than the API lane's (a 5xx writes zero body bytes before the
/// retry fires), but a `--max-time` that expires after partial bytes still lands two
/// attempts' fragments in one buffer — and the buffer is exactly what the Ed25519
/// check reads. [`download_bytes`] retries the subprocess instead.
///
/// Refuse to pair a credential with any host but `api.github.com` — STRUCTURALLY, at
/// the one place every asset download passes through, rather than by each caller's
/// discipline. A GitHub token belongs on GitHub's API and nowhere else: the release
/// download host (`github.com/…/releases/download`) 302s to object storage and reads no
/// `Authorization` header, a vendor host is not GitHub at all, and a caller that reached
/// here with `Some(token)` and a web URL has a bug that would otherwise leak the secret
/// silently. `require_https_url` is the scheme gate; this is the host gate.
fn refuse_credential_off_api(asset_url: &str, token: Option<&str>) -> Result<(), String> {
    if token.is_some() && !crate::cdn::is_api_host(asset_url) {
        return Err(format!(
            "refusing to send a credential to a non-API host: {asset_url}"
        ));
    }
    Ok(())
}

/// NOTE: no `-w "\n%{http_code}"` here (and none in [`download_to_args`]). These carry
/// `-f`, so curl's exit status already reports a non-2xx, and appending the status to
/// stdout would CORRUPT the downloaded bytes — including the appcast the Ed25519
/// signature covers. Asset downloads therefore stay unclassified; every public/private
/// verdict is taken from the discovery step that always runs first — the evergreen
/// pointer's HEAD on the web lane ([`head_no_redirect`]), the releases LIST on the
/// token lane.
fn download_bytes_args(cap: &str) -> [&str; 9] {
    [
        "-fsSL",
        // Redirects (GitHub's 302 to object storage) may only land on https —
        // `-L` alone would also follow http/ftp(s), a MITM downgrade vector.
        "--proto-redir",
        "=https",
        "--max-time",
        "60",
        "--max-filesize",
        cap,
        "-H",
        "Accept: application/octet-stream",
        // The `--` end-of-options guard for the server-controlled asset URL is
        // appended by `curl_argv` (AFTER the auth channel — see its invariants);
        // `require_https_url` closes the scheme-injection vector.
    ]
}

/// Download a SMALL asset's bytes (e.g. a manifest) into memory, size-capped at
/// `max_filesize` bytes so a rogue/oversized asset can't be buffered whole. The cap
/// is caller-supplied (the meaning of "small" is artifact-specific); curl aborts
/// before reading past it.
///
/// A failed attempt is retried up to three times here rather than by curl, so the
/// returned buffer always holds exactly ONE attempt's bytes — the Ed25519 check reads
/// that buffer, and curl does not truncate a pipe between its own retries. See
/// `download_bytes_args`.
// Skip: same audited display-lossy Err-path class as `api_get`.
#[cfg_attr(trust_verify, trust::skip)]
pub fn download_bytes(
    asset_url: &str,
    token: Option<&str>,
    max_filesize: u64,
) -> Result<Vec<u8>, String> {
    require_https_url(asset_url)?;
    refuse_credential_off_api(asset_url, token)?;
    let cap = max_filesize.to_string();
    // Bounded exactly as `api_get_classified`'s loop is: the final attempt returns on
    // both arms.
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        if attempt > 1 {
            // curl's own inter-retry backoff, preserved: 1 s, then 2 s.
            std::thread::sleep(std::time::Duration::from_secs(1 << (attempt - 2)));
        }
        let out = curl_fetch(&download_bytes_args(&cap), asset_url, token)?;
        if out.status.success() {
            return Ok(out.stdout);
        }
        // With `-f` every failure — HTTP error, timeout, DNS — is a non-zero exit.
        let stderr = String::from_utf8_lossy(&out.stderr);
        // A RATE LIMIT is not a broken download. `-f` folds "429" / "403 rate limit"
        // into exit 22 with the status in curl's own message; name it, so the check
        // lane can take its deferred, no-ledger path instead of booking a
        // `pipeline` failure (2026-08-19 audit). On the API asset endpoint the only
        // 403 a client ever meets is the rate limit (a private asset answers 404),
        // and with a token an auth failure has already been classified by the
        // releases list. On the WEB host a 403 is a blocked host and a 404 a
        // missing/private asset — both verdicts, named per host by
        // `classify_asset_failure`.
        if let Some(verdict) = classify_asset_failure(asset_url, &stderr) {
            return Err(verdict);
        }
        if attempt >= CURL_ATTEMPTS {
            return Err(format!(
                "curl asset download failed ({}): {}",
                out.status,
                stderr.trim()
            ));
        }
    }
}

/// The marker [`download_bytes`] puts in front of a rate-limited asset fetch, so a
/// caller holding only the error string can classify it ([`download_error_is_rate_limit`]).
const RATE_LIMIT_ERROR_PREFIX: &str = "rate limited (HTTP ";

/// Whether a [`download_bytes`] / [`download_to`] error describes a GitHub rate limit
/// (HTTP 429 on either host, 403 on the API host) rather than a broken download.
#[must_use]
pub fn download_error_is_rate_limit(error: &str) -> bool {
    error.contains(RATE_LIMIT_ERROR_PREFIX)
}

/// Whether an HTTP status on `url` is GitHub telling this client to slow down. PER HOST:
/// a 429 is that on every host; a 403 is that ONLY on `api.github.com`, whose asset
/// endpoint never answers 403 for any other reason to an anonymous public-channel
/// client. On `github.com` a 403 is a blocked host or a filtering proxy (an unpublished,
/// draft or private asset answers 404 there — measured 2026-09-02), and calling it a
/// rate limit would make the check lane defer, three times, on a host that is not
/// going to change its mind.
fn rate_limit_shaped(url: &str, code: u16) -> bool {
    code == 429 || (code == 403 && crate::cdn::is_api_host(url))
}

/// The VERDICT a `-f` failure carries, if it is one: a rate limit on either host, or a
/// web-host 403/404 — both of which are answers about THIS asset on THIS host, not
/// blips, so the caller returns them on the first attempt instead of retrying. `None`
/// is the historical path (retry, then the generic curl message).
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

/// GitHub's per-release-asset ceiling, and THE one number both sides of the
/// update channel must agree on: the client's container download cap
/// (`aterm-update` github.rs) and the cutter's publish-size guard
/// (`aterm-release` `UPDATER_MAX_DMG_BYTES`) must be this constant, not private
/// copies. They drifted once — 2026-08-02 raised the cutter's bound to 2 GiB
/// for the batteries-included DMGs, the client's container site kept 512 MiB,
/// and every 0.15.0 install accepted the v0.17.0 manifest and then could never
/// fetch its 775 MB payload (curl exit 56, "Maximum file size exceeded"),
/// which reads as a network failure and never escalates.
pub const RELEASE_ASSET_DOWNLOAD_BOUND: u64 = 2_147_483_648;

/// The wall-clock backstop for a file-sink asset download, DERIVED from the size cap
/// instead of fixed.
///
/// A ceiling decoupled from the payload is what strands a big container on a slow link.
/// The fixed 600 s this replaces demanded 1.3 MB/s (~10 Mbit/s) sustained to move the
/// shipped 775 MB batteries-included container, and 13 MB/s to move atpkg's 8 GiB
/// `ARTIFACT_CAP` — and since nothing resumes (the caller deletes the `.part` on
/// failure), such a machine died at the SAME wall on every single cycle and could never
/// update at all, while the operator-facing notification blamed a "broken update
/// pipeline". The 600 came in with the original extraction and was never revisited when
/// the size bound was raised to 2 GiB: the same coupled-constant miss that once shipped
/// a 512 MiB client cap against 775 MB containers.
///
/// The floor rate is a deliberately slow 64 KiB/s so this stays a BACKSTOP, never a
/// second stall detector — `--speed-limit`/`--speed-time` are what express "stalled",
/// and this only bounds a transfer that trickles forever. Never below the historical
/// 600 s, never above 6 h.
fn download_max_time_secs(max_filesize: u64) -> u64 {
    (max_filesize / 65_536).clamp(600, 21_600)
}

/// The option list for [`download_to`], extracted so the flag set is assertable in a
/// unit test.
///
/// This is the ONE lane that keeps curl's own `--retry`: the sink is a file (`-o`), and
/// curl DOES truncate a file sink between attempts (verified), so no failed attempt's
/// bytes can survive into `dest` the way they survive on a pipe — see
/// [`api_get_args`].
fn download_to_args<'a>(cap: &'a str, max_time: &'a str, dest: &'a str) -> [&'a str; 19] {
    [
        // `-s` matters as much as `-S` here, and its absence was load-bearing: without
        // it curl writes its PROGRESS METER to stderr, and this lane's stderr is
        // captured into the failure message. A machine that failed this download 95
        // consecutive times logged 95 walls of `--:--:--   0` with the actual cause
        // buried at the end of each one, unreadable
        // (docs/AUDIT-nux-first-open-toolchain-2026-08-31.md). `-S` keeps real errors,
        // which is the pairing `download_bytes_args` already uses.
        "-fsSL",
        // https-only redirects — see `download_bytes_args`.
        "--proto-redir",
        "=https",
        "--retry",
        "2",
        // Bound the CONNECT, not the transfer — a black-holed TCP/TLS setup must
        // still fail fast.
        "--connect-timeout",
        "30",
        // Abort only on a REAL stall: under 4 KiB/s for 120 s. This — not the wall
        // clock — is what tells a dead link from a merely slow one, and it exits 28
        // just like a `--max-time` expiry, so the error text and the `pipeline`-class
        // health accounting are unchanged for genuinely dead links.
        "--speed-limit",
        "4096",
        "--speed-time",
        "120",
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
    ]
}

/// Download an asset (e.g. a DMG) to a file, following the storage redirect.
/// Bounded at `max_filesize` bytes (caller-supplied) so an attacker-controlled or
/// mis-pointed release asset can't fill the disk — curl aborts before writing past
/// it.
///
/// The TIME bound is derived from that same cap (`download_max_time_secs`) and paired
/// with a stall detector, so a slow link finishes instead of dying at a fixed wall it
/// can never beat.
///
/// THIS lane does not resume. That used to be argued as an absolute — "a ranged request
/// would make `--max-filesize` bound only the REMAINING range rather than total bytes
/// written" — and the accounting half of that is real, but it is arithmetic, not a
/// barrier: [`download_to_resumable`] subtracts the offset from the cap and keeps the
/// total bound exactly. What remains true is that resuming needs a `.part` lifecycle the
/// CALLER owns, and this lane's caller (the app-container download, 26–29 MB, whose
/// scratch dir is swept wholesale before every attempt by design) neither has one nor
/// has much to gain. The 630 MB toolchain artifact does, and uses the resumable form.
// Skip: same audited display-lossy Err-path class as `api_get`.
#[cfg_attr(trust_verify, trust::skip)]
pub fn download_to(
    asset_url: &str,
    token: Option<&str>,
    dest: &Path,
    max_filesize: u64,
) -> Result<(), String> {
    require_https_url(asset_url)?;
    refuse_credential_off_api(asset_url, token)?;
    let dest_s = dest.to_str().ok_or("non-UTF-8 destination path")?;
    let cap = max_filesize.to_string();
    // Both must outlive the argv array, which borrows them as `&str`.
    let max_time = download_max_time_secs(max_filesize).to_string();
    let out = curl_fetch(&download_to_args(&cap, &max_time, dest_s), asset_url, token)?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        // Same per-host classification as `download_bytes`: a rate limit is named so
        // the check lane can defer instead of booking a broken pipeline; a web-host
        // 403/404 is named for what it is.
        if let Some(verdict) = classify_asset_failure(asset_url, &stderr) {
            return Err(verdict);
        }
        return Err(format!(
            "curl download failed ({}): {}",
            out.status,
            stderr.trim()
        ));
    }
    Ok(())
}

/// The `.part` sibling a resumable download writes into: the destination file name with
/// `.part` APPENDED — never `Path::with_extension`, which would turn
/// `trust-5520.tar.zst` into `trust-5520.tar.part` and collide across builds.
///
/// The name therefore still carries the asset name (which carries the build number), so
/// a partial can never be confused with another build's.
fn part_path(dest: &Path) -> Option<std::path::PathBuf> {
    let name = dest.file_name()?;
    let mut part = name.to_os_string();
    part.push(".part");
    Some(dest.with_file_name(part))
}

/// How a resumable attempt should be issued, given what is already on disk.
///
/// Kept pure so the ONE thing that must not be got wrong — the size accounting — is
/// assertable without a network: `--max-filesize` is compared against the response's
/// `Content-Length`, and a ranged response carries only the REMAINDER, so the cap handed
/// to curl must be `max_filesize - offset` for the TOTAL bytes written to stay bounded by
/// `max_filesize`. That total accounting is the anti-disk-fill guard `download_to`'s doc
/// names as the reason resume was left out; subtracting the offset is what restores it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ResumePlan {
    /// Byte offset to continue from. `0` means "no range at all" — a fresh transfer,
    /// with no `--continue-at` on the argv.
    offset: u64,
    /// What to pass as `--max-filesize`: the REMAINING allowance.
    remaining_cap: u64,
    /// Whether the existing prefix must be discarded first (it is already at or past the
    /// total cap, so it can never become a valid artifact — and it would leave a
    /// zero/negative allowance).
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

/// Whether a FAILED attempt's prefix is worth keeping: only if this attempt actually
/// moved the file forward.
///
/// This is the anti-wedge rule. A prefix that cannot be extended — the upstream object
/// was clobbered and is now shorter (curl 416), a server that refuses ranges (curl 33), a
/// corrupt local file — would otherwise be retried from the same dead offset forever, six
/// hours apart, for the life of the machine. One attempt that makes no progress discards
/// it and the next starts clean, which is exactly today's cost and no worse.
fn keep_partial(before: u64, after: u64) -> bool {
    after > before
}

/// Whether a FAILED attempt died on the RANGE itself — a server (or upstream object)
/// that refused to serve the requested offset — as opposed to a transport or HTTP
/// failure that would recur from offset 0 too.
///
/// Two spellings reach us: curl exit 33 (`CURLE_RANGE_ERROR`, the server cannot or will
/// not resume) and an HTTP 416 surfaced through `--fail` as exit 22 (the offset is past
/// what the upstream object now holds — it shrank or was clobbered). Both mean the
/// SAME prefix retried at the SAME offset can never succeed, and both are exactly the
/// cases a fresh attempt from 0 can: that is what makes the in-call fresh retry in
/// [`download_to_resumable`] worthwhile for these and pointless for anything else.
fn range_refused(exit_code: Option<i32>, stderr: &str) -> bool {
    exit_code == Some(33) || curl_http_error_code(stderr) == Some(416)
}

/// [`download_to_args`] plus the resume flags. `offset == 0` adds NOTHING — a fresh
/// transfer must go out as the byte-for-byte historical request, with no `Range` header
/// for a server to mishandle.
fn download_resume_args<'a>(
    cap: &'a str,
    max_time: &'a str,
    dest: &'a str,
    offset: Option<&'a str>,
) -> Vec<&'a str> {
    let mut args: Vec<&str> = download_to_args(cap, max_time, dest).to_vec();
    if let Some(offset) = offset {
        // An EXPLICIT offset, not `-C -`: `-` asks curl to size the local file itself,
        // which is a second source of truth for a number we already hold and have already
        // used to compute the cap above.
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
/// lanes are deliberately untouched (byte-for-byte historical argv).
const HTTPS_ONLY_ARGS: [&str; 2] = ["--proto", "=https"];

/// [`download_resume_args`] with the initial-request scheme pinned to `https` as well.
fn download_resume_args_https_only<'a>(
    cap: &'a str,
    max_time: &'a str,
    dest: &'a str,
    offset: Option<&'a str>,
) -> Vec<&'a str> {
    let mut args: Vec<&str> = HTTPS_ONLY_ARGS.to_vec();
    args.extend(download_resume_args(cap, max_time, dest, offset));
    args
}

/// Download an asset to `dest`, RESUMABLY: bytes land in a sibling `<dest>.part` that
/// SURVIVES a failed attempt, and the next call continues from where the last one
/// stopped. On success the part is renamed onto `dest`, so `dest` only ever exists
/// complete.
///
/// # Why this exists
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
/// failed attempt that made no progress discards its prefix ([`keep_partial`]) so a dead
/// offset can never wedge the lane.
///
/// # A range-refused resume retries fresh ONCE, in-call
///
/// [`keep_partial`] already guaranteed a range-refusing server could not WEDGE the lane
/// — the dead prefix was discarded and the NEXT call started clean. But "next call" was
/// a whole failed pass away (six hours, or one spurious failed row in a progress
/// surface). When the failure names the range itself ([`range_refused`]: curl 33, or a
/// 416 because the upstream object shrank), this call now discards the `.part` and
/// retries from offset 0 immediately — at most once per call, with the cap recomputed
/// from the FULL `max_filesize` and a fresh curl process (hence a fresh wall clock) for
/// the fresh attempt. Any other failure keeps today's semantics exactly, and the sha256
/// gate downstream is untouched either way.
// Skip: same audited display-lossy Err-path class as `api_get`.
#[cfg_attr(trust_verify, trust::skip)]
pub fn download_to_resumable(
    asset_url: &str,
    token: Option<&str>,
    dest: &Path,
    max_filesize: u64,
) -> Result<(), String> {
    download_to_resumable_with(asset_url, token, dest, max_filesize, false)
}

/// [`download_to_resumable`] for a VENDOR-hosted asset: identical transfer, `.part`
/// lifecycle and cap arithmetic, with curl's own scheme pin (`--proto =https`) added
/// beside the redirect pin so neither the first hop nor any redirect can leave https.
///
/// Must be called with `token = None` — a vendor host is never GitHub, and the GitHub
/// credential must not be presented to it. The transport ENFORCES that
/// (`refuse_credential_off_api`): a credential paired with any non-API host is refused
/// before curl is spawned.
// Skip: same audited display-lossy Err-path class as `api_get`.
#[cfg_attr(trust_verify, trust::skip)]
pub fn download_to_resumable_https_only(
    asset_url: &str,
    token: Option<&str>,
    dest: &Path,
    max_filesize: u64,
) -> Result<(), String> {
    download_to_resumable_with(asset_url, token, dest, max_filesize, true)
}

/// The shared body of [`download_to_resumable`] / [`download_to_resumable_https_only`];
/// `https_only` selects the argv builder and nothing else.
// Skip: same audited display-lossy Err-path class as `api_get`.
#[cfg_attr(trust_verify, trust::skip)]
fn download_to_resumable_with(
    asset_url: &str,
    token: Option<&str>,
    dest: &Path,
    max_filesize: u64,
    https_only: bool,
) -> Result<(), String> {
    require_https_url(asset_url)?;
    refuse_credential_off_api(asset_url, token)?;
    let Some(part) = part_path(dest) else {
        return Err("destination has no file name".to_string());
    };
    let part_s = part.to_str().ok_or("non-UTF-8 destination path")?;
    // At most ONE fresh retry per call: the loop runs a second iteration only through
    // the range-refused arm below, which sets this flag and deletes the `.part` — so
    // the second iteration is provably a fresh (offset-0) attempt and provably the last.
    let mut retried_fresh = false;
    loop {
        let existing = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
        let plan = resume_plan(existing, max_filesize);
        let existing = if plan.discard {
            let _ = std::fs::remove_file(&part);
            0
        } else {
            existing
        };
        // Both must outlive the argv, which borrows them as `&str`.
        let cap = plan.remaining_cap.to_string();
        // The wall clock stays derived from the FULL cap, not the remainder: it is a
        // backstop against a transfer that trickles forever, and shrinking it for a
        // resumed attempt would re-introduce the fixed-wall stranding it exists to
        // prevent. Recomputed per attempt so the fresh-retry iteration hands its curl
        // child a full, rebuilt budget rather than whatever the refused attempt left.
        let max_time = download_max_time_secs(max_filesize).to_string();
        let offset_text = plan.offset.to_string();
        // `None` at offset 0: a fresh transfer must carry no range at all.
        let offset = (plan.offset > 0).then_some(offset_text.as_str());
        let args = if https_only {
            download_resume_args_https_only(&cap, &max_time, part_s, offset)
        } else {
            download_resume_args(&cap, &max_time, part_s, offset)
        };
        let out = curl_fetch(&args, asset_url, token)?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            // A RESUMED attempt the server refused at the range itself: discard the
            // prefix (it is exactly the thing being refused) and go around once from
            // offset 0. Only a ranged attempt can take this arm — a fresh attempt
            // carries no range for a server to refuse — so termination holds even if
            // curl exit 33 ever appeared on a rangeless transfer.
            if plan.offset > 0 && !retried_fresh && range_refused(out.status.code(), &stderr) {
                let _ = std::fs::remove_file(&part);
                retried_fresh = true;
                continue;
            }
            let after = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
            if !keep_partial(existing, after) {
                let _ = std::fs::remove_file(&part);
            }
            // Same per-host classification as `download_to`: a rate limit is named so
            // the check lane can defer instead of booking a broken pipeline.
            if let Some(verdict) = classify_asset_failure(asset_url, &stderr) {
                return Err(verdict);
            }
            return Err(format!(
                "curl download failed ({}): {}",
                out.status,
                stderr.trim()
            ));
        }
        // ONLY a curl success promotes the part. `dest` therefore never holds a prefix,
        // and every existing caller's "the file at `dest` is the whole asset" assumption
        // is untouched.
        return std::fs::rename(&part, dest).map_err(|e| {
            let _ = std::fs::remove_file(&part);
            format!("finalize download: {e}")
        });
    }
}

#[cfg(test)]
mod tests {
    const API_ASSET: &str = "https://api.github.com/repos/alabsystems/aterm/releases/assets/1";
    const WEB_ASSET: &str =
        "https://github.com/alabsystems/aterm/releases/download/v0.74.0/aterm-machines.toml";

    fn curl_err(code: u16) -> String {
        format!("curl: (22) The requested URL returned error: {code}")
    }

    /// The classification is PER HOST. On the API host a 403 is the rate limit (the
    /// only 403 an anonymous public-channel client meets there); on the web host a 403
    /// is a blocked host, a 404 a missing/private asset, and only a 429 is GitHub's
    /// throttle. A transport failure is none of these on either host.
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

        // API host: 403 and 429 are the rate limit; 404 is the historical retry path.
        for code in [403, 429] {
            let verdict =
                super::classify_asset_failure(API_ASSET, &curl_err(code)).expect("a verdict");
            assert!(super::download_error_is_rate_limit(&verdict), "{verdict}");
        }
        assert_eq!(
            super::classify_asset_failure(API_ASSET, &curl_err(404)),
            None
        );

        // Web host: 429 is a rate limit; 403 is forbidden; 404 is not found; neither of
        // the last two is rate-limit shaped, so the check lane books rather than defers.
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

    /// The LAST block wins (a redirect chain writes one per hop), names match
    /// case-insensitively, a non-numeric value is absent, and the reset is clamped to
    /// one hour past `now` — the window is an hour, so anything further is a lie.
    #[test]
    fn rate_limit_headers_are_read_from_the_last_block_and_clamped() {
        let now = 1_788_390_000;
        let dump = "HTTP/2 302
x-ratelimit-limit: 60
x-ratelimit-remaining: 59
                    x-ratelimit-used: 1
x-ratelimit-reset: 1788390100

                    HTTP/2 403
X-RateLimit-Limit: 60
X-RateLimit-Remaining: 1
                    X-RateLimit-Used: 23
X-RateLimit-Reset: 1788392970

";
        let h = super::parse_rate_limit_headers(dump, now).expect("headers present");
        assert_eq!(
            h,
            super::RateLimitHeaders {
                limit: Some(60),
                remaining: Some(1),
                used: Some(23),
                reset: Some(1_788_392_970),
            }
        );
        // A reset beyond the horizon is clamped to now + 3600.
        let far = "HTTP/2 403
x-ratelimit-remaining: 0
x-ratelimit-reset: 9999999999
";
        let h = super::parse_rate_limit_headers(far, now).unwrap();
        assert_eq!(h.reset, Some(now + 3600));
        assert_eq!(h.remaining, Some(0));
        // A later hop WITHOUT the headers yields none — the kept response had none.
        let stripped = "HTTP/2 302
x-ratelimit-remaining: 40

HTTP/2 200
                        content-type: text/plain
";
        assert_eq!(super::parse_rate_limit_headers(stripped, now), None);
        // Non-numeric values are absent, not zero.
        let junk = "HTTP/2 200
x-ratelimit-remaining: lots
x-ratelimit-limit: 60
";
        let h = super::parse_rate_limit_headers(junk, now).unwrap();
        assert_eq!(h.remaining, None);
        assert_eq!(h.limit, Some(60));
        assert_eq!(super::parse_rate_limit_headers("", now), None);
    }

    /// The web lane is the SAME request as the API lane with a different URL: same
    /// flags, same header, same `--` guard, no auth channel. Nothing about the transfer
    /// changes with the host — only where the bytes come from.
    #[test]
    fn the_asset_argv_for_a_web_url_differs_from_the_api_argv_only_in_the_url() {
        let args = super::download_bytes_args("5000000");
        let api = super::curl_argv(&args, API_ASSET, false);
        let web = super::curl_argv(&args, WEB_ASSET, false);
        assert_eq!(api.len(), web.len());
        let differing: Vec<(usize, &String, &String)> = api
            .iter()
            .zip(web.iter())
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .map(|(i, (a, b))| (i, a, b))
            .collect();
        assert_eq!(differing.len(), 1, "{differing:?}");
        assert_eq!(
            differing[0].0,
            api.len() - 1,
            "only the URL, and it is last"
        );
        assert!(
            !web.iter().any(|a| a == "--config"),
            "no credential channel on the web lane"
        );
    }

    use super::{
        HttpError, RELEASE_ASSET_DOWNLOAD_BOUND, api_get_args, api_get_args_dumping, curl_argv,
        curl_bin, curl_fetch, curl_prepared, download_bytes_args, download_max_time_secs,
        download_resume_args, download_resume_args_https_only, download_to_args,
        download_to_resumable, download_to_resumable_https_only, head_args, keep_partial,
        location_header, part_path, range_refused, resume_plan, token_config_safe,
        transient_api_status,
    };
    use std::process::Command;

    /// The curl binary is platform-selected: a pinned absolute path on unix (no
    /// `PATH`-shim substitution), and the on-`PATH` `curl.exe` Windows ships since
    /// Win10 1803. Guards against a regression back to the hardcoded `/usr/bin/curl`
    /// that made every update request fail to spawn on Windows.
    #[test]
    fn curl_binary_is_platform_selected() {
        #[cfg(windows)]
        assert_eq!(curl_bin(), "curl.exe");
        #[cfg(not(windows))]
        assert_eq!(curl_bin(), "/usr/bin/curl");
    }

    /// The v0.5.10/v0.5.11 auto-update-bricking regression: the `--` end-of-options
    /// marker must come AFTER every option — in particular after the `--config -`
    /// stdin auth channel — and immediately BEFORE the URL. When callers placed `--`
    /// themselves (before `curl_auth` appended the auth args), every option after it
    /// was parsed as a URL: the real request went out UNAUTHENTICATED (404 on the
    /// private repo) and asset downloads failed on every client, permanently.
    #[test]
    fn argv_orders_auth_before_end_of_options_and_url_last() {
        let v = curl_argv(
            &[
                "-fsSL",
                "--max-filesize",
                "5000000",
                "-H",
                "Accept: application/octet-stream",
            ],
            "https://api.github.com/repos/o/r/releases/assets/1",
            true,
        );
        assert_eq!(
            v[0], "-q",
            "-q must be the very first parameter (curlrc defense)"
        );
        let dashdash = v.iter().position(|a| a == "--").expect("`--` present");
        let config = v
            .iter()
            .position(|a| a == "--config")
            .expect("--config present");
        assert_eq!(v[config + 1], "-", "auth config is read from stdin");
        assert!(
            config < dashdash,
            "the auth channel must be an OPTION (before `--`), not a URL: {v:?}"
        );
        assert_eq!(
            dashdash,
            v.len() - 2,
            "`--` must immediately precede the URL and nothing else: {v:?}"
        );
        assert_eq!(
            v[v.len() - 1],
            "https://api.github.com/repos/o/r/releases/assets/1"
        );
        assert_eq!(
            v.iter().filter(|a| *a == "--").count(),
            1,
            "exactly one end-of-options marker (callers must not add their own): {v:?}"
        );
    }

    /// A hostile releases JSON pointing an asset at a leading-dash "URL" must land
    /// after `--` so curl treats it as a URL (then fails DNS), never as an option.
    #[test]
    fn leading_dash_url_stays_inert() {
        for authenticated in [true, false] {
            let v = curl_argv(&["-fsSL"], "-K/tmp/evil", authenticated);
            let dashdash = v.iter().position(|a| a == "--").unwrap();
            assert_eq!(v[dashdash + 1], "-K/tmp/evil");
            assert_eq!(dashdash + 2, v.len());
        }
    }

    /// THE token-optional argv contract. An anonymous request must carry NO auth
    /// channel at all — not an empty one — while keeping every ordering invariant
    /// the authenticated lane relies on. If `--config` ever leaks back into this
    /// argv, curl blocks reading a stdin nobody writes and the check hangs forever.
    #[test]
    fn argv_omits_the_auth_channel_when_unauthenticated() {
        let v = curl_argv(
            &["-sS", "-H", "Accept: application/vnd.github+json"],
            "https://api.github.com/repos/o/r/releases?per_page=100&page=1",
            false,
        );
        assert!(
            !v.iter().any(|a| a == "--config"),
            "an anonymous request must not open the auth config channel: {v:?}"
        );
        assert!(
            !v.iter()
                .any(|a| a.contains("Authorization") || a.contains("Bearer")),
            "an anonymous request must carry no credential material: {v:?}"
        );
        // The three invariants that are NOT about authentication still hold.
        assert_eq!(
            v[0], "-q",
            "the curlrc defense is orthogonal to auth and must survive: {v:?}"
        );
        assert!(
            v.iter().any(|a| a == "User-Agent: aterm-update"),
            "the fixed User-Agent still identifies us: {v:?}"
        );
        let dashdash = v.iter().position(|a| a == "--").expect("`--` present");
        assert_eq!(
            v.iter().filter(|a| *a == "--").count(),
            1,
            "exactly one end-of-options marker: {v:?}"
        );
        assert_eq!(
            dashdash,
            v.len() - 2,
            "`--` must immediately precede the URL and nothing else: {v:?}"
        );
        assert_eq!(
            v[v.len() - 1],
            "https://api.github.com/repos/o/r/releases?per_page=100&page=1"
        );
    }

    /// The property C3 exists to protect: even WITH a token, nothing on argv is
    /// derived from it — the credential only ever travels over the `--config -`
    /// stdin channel. argv is world-readable to same-user processes via `ps`.
    #[test]
    fn a_present_token_never_reaches_argv() {
        const SECRET: &str = concat!("gh", "p_TOPSECRETtokenvalue0123456789ABCD");
        let v = curl_argv(&["-sS"], "https://api.github.com/repos/o/r/releases", true);
        assert!(
            !v.iter().any(|a| a.contains(SECRET)),
            "curl_argv must not be able to carry a token at all: {v:?}"
        );
        // …structurally: the argv builder is not even given the token, and the only
        // thing that changes with `authenticated` is the stdin channel opener.
        let anon = curl_argv(&["-sS"], "https://api.github.com/repos/o/r/releases", false);
        let removed: Vec<_> = v.iter().filter(|a| !anon.contains(a)).collect();
        assert_eq!(
            removed,
            ["--config", "-"].iter().collect::<Vec<_>>(),
            "the ONLY authenticated-lane difference is the stdin config channel: {v:?}"
        );
    }

    /// C3, enforced where the process is ACTUALLY built and WITH a real secret in hand.
    ///
    /// `a_present_token_never_reaches_argv` checks `curl_argv`, which is never given
    /// the token and therefore cannot leak it however it is written — a weaker property
    /// than it looks. `curl_prepared` DOES receive the credential, so this test can
    /// assert the thing that actually matters: a live token went in, and nothing
    /// derived from it came out on argv. Verified non-vacuous by mutation (appending
    /// `-H "Authorization: Bearer {t}"` in the spawn path fails exactly here).
    #[test]
    fn the_spawned_command_carries_no_credential_on_either_lane() {
        const SECRET: &str = concat!("gh", "p_TOPSECRETtokenvalue0123456789ABCD");
        const URL: &str = "https://api.github.com/repos/o/r/releases";
        for token in [Some(SECRET), None] {
            let argv = argv_of(&curl_prepared(&["-sS"], URL, token).expect("valid token"));
            assert!(
                !argv.iter().any(|a| a.contains(SECRET)),
                "the token must never appear on argv (token={token:?}): {argv:?}"
            );
            // Nor may the header it would ride in, under any spelling.
            assert!(
                !argv.iter().any(|a| {
                    let a = a.to_ascii_lowercase();
                    a.contains("authorization") || a.contains("bearer")
                }),
                "no Authorization/Bearer header may appear on argv: {argv:?}"
            );
            // …and the argv is EXACTLY curl_argv's list, so nothing was appended
            // after the `--` end-of-options marker either.
            assert_eq!(
                argv,
                curl_argv(&["-sS"], URL, token.is_some()),
                "curl_fetch must spawn curl_argv's list verbatim (token={token:?})"
            );
        }
    }

    /// The spawned program + argv, as owned strings.
    fn argv_of(command: &Command) -> Vec<String> {
        command
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    /// Fail closed on a caller bug rather than emitting a bare `Bearer ` header:
    /// an empty token is not "anonymous", it is a mistake, and GitHub answers a
    /// malformed credential with a 401 that would look like a revoked token.
    #[test]
    fn empty_token_is_refused() {
        let err = curl_fetch(&["-sS"], "https://api.github.com/repos/o/r", Some(""))
            .expect_err("an empty token must be refused before spawning curl");
        assert!(err.contains("empty"), "{err}");
        assert!(
            err.contains("anonymously"),
            "the remedy (request anonymously) must be named: {err}"
        );
        // The injection guard still fires ahead of any spawn, too.
        let err = curl_fetch(&["-sS"], "https://api.github.com/repos/o/r", Some("a\"b"))
            .expect_err("an injection-shaped token must be refused");
        assert!(err.contains("illegal characters"), "{err}");
    }

    /// Classification must not change any operator-visible wording: each arm's
    /// `Display` is the string this layer returned before the split. The one
    /// deliberate exception is the ANONYMOUS rate limit, where the old text
    /// ("the token is valid") would be a lie.
    #[test]
    fn classified_errors_render_the_historical_wording() {
        let url = "https://api.github.com/repos/o/r/releases";
        assert_eq!(
            HttpError::RateLimited {
                code: 403,
                url: url.into(),
                authenticated: true
            }
            .to_string(),
            format!(
                "GitHub rate limit hit (HTTP 403) for {url}; transient (the token is \
                 valid) — backing off, will retry on the next check"
            )
        );
        let anon = HttpError::RateLimited {
            code: 429,
            url: url.into(),
            authenticated: false,
        }
        .to_string();
        assert!(
            anon.contains("~60 requests/hour per IP") && !anon.contains("the token is valid"),
            "an anonymous rate limit must not claim a token is involved: {anon}"
        );
        assert_eq!(
            HttpError::Unauthorized { code: 401 }.to_string(),
            "GitHub auth failed (HTTP 401): the update token is missing required \
             access, expired, or was revoked — rotate it (see docs/RELEASING.md)"
        );
        assert_eq!(
            HttpError::NotFound { url: url.into() }.to_string(),
            format!(
                "GitHub returned HTTP 404 for {url} (repo/releases not found, or the token \
                 lacks access to this private repo)"
            )
        );
        assert_eq!(
            HttpError::Status {
                code: 500,
                url: url.into()
            }
            .to_string(),
            format!("GitHub API returned HTTP 500 for {url}")
        );
        assert_eq!(
            HttpError::Transport("curl GET x failed (exit 6): dns".into()).to_string(),
            "curl GET x failed (exit 6): dns"
        );
    }

    #[test]
    fn well_formed_tokens_are_accepted() {
        for t in [
            concat!("gh", "p_ABCdef0123456789ABCdef0123456789ABCd"),
            concat!("github", "_pat_11ABC_def.ghi-jkl"),
            "classic-40-hex-abcdef0123456789abcdef0123456789abcdef01",
        ] {
            assert!(token_config_safe(t), "real token rejected: {t:?}");
        }
    }

    #[test]
    fn injection_shaped_tokens_are_rejected() {
        // Each of these could break out of `header = "...: Bearer <t>"` and inject a
        // curl directive (a quote to close the value, a newline to add a line, a
        // backslash to escape, or a control char).
        for t in [
            "x\"\ninsecure",           // close the quote, add `insecure` (disable TLS)
            "x\nproxy = http://evil/", // newline → new directive
            "x\"y",                    // stray quote
            "x\\y",                    // backslash escape
            "x\ty",                    // control char (tab)
            "x\r\nfoo",                // CRLF
        ] {
            assert!(!token_config_safe(t), "injection token accepted: {t:?}");
        }
    }

    /// The bound is GitHub's own per-asset ceiling, so the only thing that can
    /// strand a payload is GitHub refusing to host it. A private lower copy is
    /// how 0.15.0 shipped a 512 MiB client cap against 775 MB containers:
    /// every install accepted the manifest and then failed the download every
    /// interval, as a "network" failure that never escalates.
    #[test]
    fn release_asset_bound_is_githubs_ceiling_and_covers_batteries_included() {
        assert_eq!(RELEASE_ASSET_DOWNLOAD_BOUND, 2 * 1024 * 1024 * 1024);
        // A claim about a constant belongs at COMPILE time; a runtime assert
        // over constants can never fail a run that compiled.
        const { assert!(RELEASE_ASSET_DOWNLOAD_BOUND > 800_000_000) };
    }

    /// The download ceiling must scale with the payload it is supposed to admit. A
    /// FIXED 600 s is decoupled from `--max-filesize` in exactly the way the 512 MiB
    /// client cap was decoupled from the 775 MB container: the shipped batteries
    /// container needed 1.3 MB/s sustained to beat it and atpkg's 8 GiB bundles needed
    /// 13 MB/s, nothing resumes, so a slower machine died at the SAME wall every cycle
    /// and could never update at all.
    #[test]
    fn the_download_ceiling_is_derived_from_the_size_cap() {
        // A release-sized asset gets hours, not ten minutes (64 KiB/s floor rate).
        assert_eq!(download_max_time_secs(775_000_000), 11_825);
        // The 2 GiB release bound and atpkg's 8 GiB ARTIFACT_CAP both take the 6 h clamp.
        assert_eq!(download_max_time_secs(RELEASE_ASSET_DOWNLOAD_BOUND), 21_600);
        assert_eq!(download_max_time_secs(8 << 30), 21_600);
        // …and a small cap never drops BELOW the historical wall.
        assert_eq!(download_max_time_secs(1024), 600);
        assert_eq!(download_max_time_secs(0), 600);
    }

    /// Invariant (b), pinned at the TRANSPORT: a credential paired with any host but
    /// `api.github.com` is refused before curl is spawned, on every download entry point
    /// — the byte lane, the file-sink lane, both resumable lanes. The anonymous web
    /// fetch and the authenticated API fetch are the only two pairings that pass the
    /// gate (asserted on the gate itself, so no network request is made here).
    #[test]
    fn a_credential_is_refused_for_any_non_api_host() {
        const SECRET: &str = concat!("gh", "p_TOPSECRETtokenvalue0123456789ABCD");
        const VENDOR: &str = "https://vendor.example/toolchain/trust-5520.tar.zst";
        assert!(super::refuse_credential_off_api(API_ASSET, Some(SECRET)).is_ok());
        assert!(super::refuse_credential_off_api(WEB_ASSET, None).is_ok());
        assert!(super::refuse_credential_off_api(VENDOR, None).is_ok());
        for url in [WEB_ASSET, VENDOR, "https://api.github.com.evil.example/x"] {
            let refused = super::refuse_credential_off_api(url, Some(SECRET)).unwrap_err();
            assert!(refused.contains("non-API host"), "{refused}");
            assert!(
                !refused.contains(SECRET),
                "never echo the secret: {refused}"
            );
        }
        let dest = std::env::temp_dir().join("aterm-http-credential-gate");
        let refusals = [
            super::download_bytes(WEB_ASSET, Some(SECRET), 1024).unwrap_err(),
            super::download_to(WEB_ASSET, Some(SECRET), &dest, 1024).unwrap_err(),
            download_to_resumable(WEB_ASSET, Some(SECRET), &dest, 1024).unwrap_err(),
            download_to_resumable_https_only(VENDOR, Some(SECRET), &dest, 1024).unwrap_err(),
        ];
        for refused in refusals {
            assert!(
                refused.contains("non-API host"),
                "the gate must be the refusal, not a network error: {refused}"
            );
        }
        assert!(!dest.exists(), "nothing was spawned, nothing was written");
    }

    /// "No stall" must not be spelled as "no bound". The asset download bounds the
    /// CONNECT and the STALL, and keeps a wall clock that is derived rather than fixed.
    #[test]
    fn the_asset_download_bounds_the_stall_not_the_transfer() {
        let cap = RELEASE_ASSET_DOWNLOAD_BOUND.to_string();
        let max_time = download_max_time_secs(RELEASE_ASSET_DOWNLOAD_BOUND).to_string();
        let args = download_to_args(&cap, &max_time, "/tmp/aterm.dmg.part");
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
        assert_eq!(value_of("--speed-time"), "120");
        // The size cap and the sink are untouched by the timing change.
        assert_eq!(value_of("--max-filesize"), cap);
        assert_eq!(value_of("-o"), "/tmp/aterm.dmg.part");
        // Callers must never place `--` themselves — curl_argv appends it after the
        // auth channel (the v0.5.10 bricking regression).
        assert!(!args.contains(&"--"), "no caller-side `--`: {args:?}");
    }

    /// Only the FILE-sink lane may use curl's own `--retry`. On a pipe curl cannot
    /// truncate what a failed attempt already wrote, so a retried API GET returns the
    /// error document CONCATENATED in front of the good body under a single `-w`
    /// status trailer — a 200 whose JSON then fails with "trailing characters",
    /// blaming the publisher for a blip curl had recovered from. Those two lanes retry
    /// the subprocess instead; `download_to` writes to `-o`, which curl DOES truncate.
    #[test]
    fn only_the_file_sink_lane_uses_curls_own_retry() {
        assert!(
            !api_get_args().contains(&"--retry"),
            "a stdout-captured GET must not let curl retry: {:?}",
            api_get_args()
        );
        let cap = "16777216";
        assert!(
            !download_bytes_args(cap).contains(&"--retry"),
            "the buffer the Ed25519 check reads must hold ONE attempt's bytes: {:?}",
            download_bytes_args(cap)
        );
        assert!(
            download_to_args(cap, "600", "/tmp/x").contains(&"--retry"),
            "the -o lane keeps curl's retry — a file sink is truncated between attempts"
        );
    }

    /// The in-process retry decision: the transient server-side set curl itself
    /// retries, and nothing else. 429 is the deliberate exclusion — the classification
    /// is code-only, so retrying spends an exhausted budget and hammers GitHub's
    /// secondary limit instead of backing off to the next cycle.
    #[test]
    fn only_transient_server_statuses_are_retried_in_process() {
        for code in ["408", "500", "502", "503", "504"] {
            assert!(transient_api_status(code), "{code} is transient");
        }
        for code in ["200", "204", "301", "401", "403", "404", "429", "418", ""] {
            assert!(
                !transient_api_status(code),
                "{code} is a verdict, not a blip — retrying it is wrong"
            );
        }
    }
    /// The historical lane must be BYTE-IDENTICAL. `api_get_classified` delegates to the
    /// header-dumping form, so the one thing that could regress every existing caller is
    /// the argv growing a flag; with no sink it must be exactly the list it always was.
    #[test]
    fn the_plain_lane_argv_is_unchanged() {
        assert_eq!(
            api_get_args_dumping(None),
            api_get_args().to_vec(),
            "a plain GET must spawn the historical option list, unchanged"
        );
    }

    /// …and a header-dumping one adds EXACTLY one flag pair, in front of the `--` marker
    /// `curl_argv` appends (callers must never place their own — the v0.5.10 bricking
    /// regression).
    #[test]
    fn a_header_dump_adds_exactly_the_sink() {
        let args = api_get_args_dumping(Some("/tmp/aterm-updates/list.headers"));
        let base = api_get_args().len();
        assert_eq!(
            args.len(),
            base + 2,
            "one flag pair and nothing else: {args:?}"
        );
        assert_eq!(args[base], "--dump-header");
        assert_eq!(args[base + 1], "/tmp/aterm-updates/list.headers");
        assert!(
            !args.contains(&"--"),
            "no caller-side end-of-options marker: {args:?}"
        );
        // The base list survives verbatim underneath.
        assert_eq!(&args[..base], &api_get_args()[..]);
        // No conditional-request machinery survives: a 304 can never be produced
        // because nothing is ever asked conditionally.
        assert!(!args.iter().any(|a| a.starts_with("If-None-Match")));
    }

    /// THE web-lane steady-state request: headers only, ONE hop, no credential channel,
    /// no `-f` (a 404 is an answer), no `-L` (the redirect is the answer), and the `--`
    /// guard still last before the URL.
    #[test]
    fn the_head_argv_stops_at_the_first_hop_and_carries_no_credential() {
        let args = head_args();
        assert!(args.contains(&"-I"), "{args:?}");
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
            false,
        );
        assert_eq!(v[0], "-q");
        assert!(!v.iter().any(|a| a == "--config"));
        let dashdash = v.iter().position(|a| a == "--").expect("`--` present");
        assert_eq!(dashdash, v.len() - 2);
        // The scheme gate runs before any spawn.
        let err = super::head_no_redirect("http://github.com/x").unwrap_err();
        assert!(err.to_string().contains("non-https"), "{err}");
    }

    /// The `Location` reader takes the LAST block's value (the hop we kept), matches
    /// the name case-insensitively, trims, and yields nothing for an absent or empty
    /// header — the pointer module then refuses rather than guesses.
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

    // -----------------------------------------------------------------------------
    // RESUMABLE ARTIFACT DOWNLOAD (aup-3)
    //
    // The win is measured in BYTES RE-FETCHED PER FAILED ATTEMPT, which needs a real
    // transfer to observe; `resume_cost_over_the_network` below is that measurement and
    // runs when `ATERM_RESUME_TEST_URL` names a real https asset. Everything a network
    // cannot be asked about — the size accounting, the request shape, the `.part` state
    // machine — is pinned here, unconditionally.
    // -----------------------------------------------------------------------------

    /// The part sibling must APPEND, never replace an extension: `with_extension` turns
    /// `trust-5520.tar.zst` into `trust-5520.tar.part`, which collides across builds and
    /// would let one build resume another's prefix into a digest failure.
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

    /// THE accounting invariant: `--max-filesize` bounds the response body, and a ranged
    /// response carries only the remainder — so `offset + cap` must always equal the
    /// caller's TOTAL bound. That is the anti-disk-fill guard the no-resume doc was
    /// protecting, restored by arithmetic instead of by refusing to resume.
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
        // A prefix at or past the total bound can never become a valid artifact, and
        // would leave a zero allowance: discard and start clean.
        for existing in [TOTAL, TOTAL + 1] {
            let plan = resume_plan(existing, TOTAL);
            assert!(plan.discard, "existing={existing}");
            assert_eq!(plan.offset, 0);
            assert_eq!(plan.remaining_cap, TOTAL);
        }
    }

    /// A FRESH transfer must be the byte-for-byte historical request — no `--continue-at`,
    /// no `Range` for a server to mishandle — and a resumed one adds exactly the offset.
    #[test]
    fn only_a_resumed_attempt_carries_a_range() {
        let fresh = download_resume_args("100", "600", "/s/a.tar.zst.part", None);
        assert_eq!(
            fresh,
            download_to_args("100", "600", "/s/a.tar.zst.part").to_vec(),
            "a fresh attempt must spawn the historical option list, unchanged"
        );
        let resumed = download_resume_args("60", "600", "/s/a.tar.zst.part", Some("40"));
        assert_eq!(resumed.len(), fresh.len() + 2);
        assert_eq!(resumed[fresh.len()], "--continue-at");
        assert_eq!(
            resumed[fresh.len() + 1],
            "40",
            "an EXPLICIT offset, never `-`"
        );
        // The sink is the PART, never the destination: `dest` only ever exists complete.
        let sink = resumed[resumed.iter().position(|a| *a == "-o").unwrap() + 1];
        assert!(sink.ends_with(".part"), "{resumed:?}");
        // The stall detector and the derived wall clock are untouched by resuming.
        for flag in [
            "--speed-limit",
            "--speed-time",
            "--connect-timeout",
            "--retry",
        ] {
            assert!(resumed.contains(&flag), "{flag} must survive: {resumed:?}");
        }
        assert!(
            !resumed.contains(&"--"),
            "no caller-side end-of-options marker"
        );
    }

    /// The VENDOR lane pins the scheme on BOTH hops: `--proto =https` for the initial
    /// request and `--proto-redir =https` for every redirect — and is otherwise the
    /// historical resumable argv, byte for byte (the release lanes keep theirs unchanged).
    #[test]
    fn the_vendor_lane_pins_https_on_the_first_hop_and_every_redirect() {
        let plain = download_resume_args("100", "600", "/s/claude.part", None);
        let pinned = download_resume_args_https_only("100", "600", "/s/claude.part", None);
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
        // The release lane is untouched: no `--proto` on its own.
        assert!(!plain.contains(&"--proto"), "{plain:?}");
        // Resume flags ride behind the pin exactly as before.
        let resumed = download_resume_args_https_only("60", "600", "/s/claude.part", Some("40"));
        assert_eq!(resumed.len(), pinned.len() + 2);
        assert_eq!(&resumed[resumed.len() - 2..], ["--continue-at", "40"]);
        // And the scheme gate still refuses a non-https URL before spawning anything.
        let dir = std::env::temp_dir().join(format!("aterm-vendor-lane-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let err =
            download_to_resumable_https_only("http://example.invalid/x", None, &dir.join("x"), 10)
                .unwrap_err();
        assert!(err.contains("non-https"), "{err}");
        let err =
            download_to_resumable("ftp://example.invalid/x", None, &dir.join("x"), 10).unwrap_err();
        assert!(err.contains("non-https"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The anti-wedge rule: a failed attempt keeps its prefix only if it MOVED. A 416
    /// (the upstream object shrank), a curl 33 (server refuses ranges) or a corrupt local
    /// file would otherwise be retried from the same dead offset forever.
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

    /// The in-call fresh-retry trigger: exactly the failures that name the RANGE (curl
    /// exit 33; HTTP 416 through `--fail`'s exit 22) qualify, and nothing else — a
    /// stalled transfer, a 404, or a rate limit would fail from offset 0 too, so
    /// retrying them fresh would only double the cost of an already-failed attempt.
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

    /// A refused URL fails before anything is created, and leaves no part behind — the
    /// scheme guard runs first on this lane too.
    #[test]
    fn the_resumable_lane_keeps_the_scheme_guard() {
        let dir = std::env::temp_dir().join(format!("aterm-resume-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("a.tar.zst");
        let err = download_to_resumable("file:///etc/passwd", None, &dest, 1 << 20)
            .expect_err("a non-https asset URL must be refused");
        assert!(err.contains("non-https"), "{err}");
        assert!(!dest.exists() && !dir.join("a.tar.zst.part").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// THE MEASUREMENT — bytes actually re-fetched on a retry.
    ///
    /// Needs a real https asset, so it is env-gated rather than skipped silently:
    ///
    /// ```text
    ///   ATERM_RESUME_TEST_URL=https://…/some-release-asset \
    ///     cargo test -p aterm-update-core resume_cost_over_the_network -- --nocapture --ignored
    ///   -> {"total_bytes":N,"seeded_prefix":N/2,"bytes_fetched_on_retry":~N/2,"ratio":~0.5}
    /// ```
    ///
    /// Two-sided reach guards: the asset must be big enough for a half to be meaningful,
    /// the seeded prefix must be a real prefix of it (the run downloads the whole thing
    /// once first, so the resumed file is compared against the whole one — a resume that
    /// produced DIFFERENT bytes fails here), and the retry must fetch strictly less than
    /// the whole asset or the saving is imaginary.
    ///
    /// # The saving is OBSERVED, not asserted
    ///
    /// `total - seeded` is arithmetic: it is what we ASKED for, and a server that ignored
    /// the range and re-sent everything would produce the same number while saving
    /// nothing. So the run does it twice. The second pass seeds a prefix of the RIGHT
    /// LENGTH but the WRONG BYTES; if the remainder alone came over the wire, the result
    /// must still carry that poison, and if the whole object was re-sent it cannot. The
    /// two passes together bracket the answer: pass one proves a resume reconstructs the
    /// artifact exactly, pass two proves the prefix was genuinely not transferred.
    #[test]
    #[ignore = "needs ATERM_RESUME_TEST_URL to name a real https release asset"]
    fn resume_cost_over_the_network() {
        let Ok(url) = std::env::var("ATERM_RESUME_TEST_URL") else {
            panic!("set ATERM_RESUME_TEST_URL to a real https asset URL");
        };
        let dir = std::env::temp_dir().join(format!("aterm-resume-net-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let whole = dir.join("whole.bin");
        super::download_to(&url, None, &whole, RELEASE_ASSET_DOWNLOAD_BOUND).expect("baseline");
        let reference = std::fs::read(&whole).expect("baseline bytes");
        let total = reference.len() as u64;
        assert!(
            total > 1 << 20,
            "reach guard: a {total}-byte asset is too small to price a resume"
        );

        // Seed a genuine half-prefix and resume onto it.
        let dest = dir.join("resumed.bin");
        let part = part_path(&dest).unwrap();
        let seeded = total / 2;
        std::fs::write(&part, &reference[..seeded as usize]).unwrap();
        download_to_resumable(&url, None, &dest, RELEASE_ASSET_DOWNLOAD_BOUND).expect("resume");
        let resumed = std::fs::read(&dest).expect("resumed bytes");

        assert_eq!(
            resumed, reference,
            "a resumed download must reconstruct the SAME bytes"
        );
        let fetched = total - seeded;
        println!(
            "{{\"total_bytes\":{total},\"seeded_prefix\":{seeded},\
             \"bytes_fetched_on_retry\":{fetched},\"ratio\":{:.3}}}",
            fetched as f64 / total as f64
        );
        assert!(
            fetched < total,
            "the retry must not re-fetch the whole artifact"
        );

        // …and the observation. Same offset, POISONED prefix.
        let poisoned_dest = dir.join("poisoned.bin");
        let poisoned_part = part_path(&poisoned_dest).unwrap();
        let mut poison = reference[..seeded as usize].to_vec();
        for b in poison.iter_mut() {
            *b = !*b;
        }
        std::fs::write(&poisoned_part, &poison).unwrap();
        download_to_resumable(&url, None, &poisoned_dest, RELEASE_ASSET_DOWNLOAD_BOUND)
            .expect("resume onto a poisoned prefix");
        let got = std::fs::read(&poisoned_dest).expect("poisoned bytes");
        assert_eq!(got.len(), reference.len(), "the total length is unchanged");
        assert_eq!(
            &got[seeded as usize..],
            &reference[seeded as usize..],
            "the REMAINDER really was transferred"
        );
        assert_eq!(
            &got[..seeded as usize],
            &poison[..],
            "the prefix was NOT re-fetched — the bytes we planted survived, which is the \
             saving, observed rather than computed"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
