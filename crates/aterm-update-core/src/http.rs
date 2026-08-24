// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The network layer: `curl` calls to the GitHub Releases API, authenticated when a
//! token is available and ANONYMOUS when one is not. Artifact-agnostic — these fetch
//! arbitrary API JSON and asset bytes; the consuming crate decides what the bytes
//! mean.
//!
//! Both repo shapes require the API (the `releases/latest/download/…` browser
//! shortcut needs web auth even for public repos), and asset bytes are downloaded via
//! the asset API URL with `Accept: application/octet-stream` (curl `-L` follows the
//! 302 to storage and drops the `Authorization` header on the cross-host redirect by
//! default).
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
//! An anonymous caller is rate-limited to ~60 requests/hour PER IP (5000/hour with a
//! token), so [`HttpError`] classifies that state separately: a rate limit is not an
//! auth failure and must not be reported as one.

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
            // The authenticated wording ("the token is valid") would be a lie on the
            // anonymous lane, where there IS no token and the ~60/hour per-IP budget
            // is the whole story — including for several machines behind one NAT.
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
    // The unconditional lane, spelled as the degenerate conditional one: no validator,
    // no header sink, therefore an argv that is EXACTLY `api_get_args()` (asserted in
    // `the_unconditional_lane_argv_is_unchanged`). Every existing caller's bytes,
    // errors, retries and wording are the historical ones.
    match api_get_conditional(url, token, None, None)? {
        ApiResponse::Body { bytes, .. } => Ok(bytes),
        // Unreachable: a 304 is only ever honoured when we SENT a validator, and this
        // lane never does. Fail closed on a proxy that invents one rather than handing
        // the caller an empty body it would parse as an empty release list.
        ApiResponse::NotModified => Err(HttpError::Malformed(format!(
            "GitHub API returned HTTP 304 for {url} without a conditional request"
        ))),
    }
}

/// What a conditional API GET came back with.
///
/// The 304 arm is the whole point: it is the SERVER asserting that the representation
/// the caller already holds is current, which is the only kind of freshness this layer
/// will ever act on — there is no TTL, no heuristic expiry, and no offline path that
/// serves a cached answer without a fresh 304 for it on THIS request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiResponse {
    /// HTTP 304: unchanged since the validator the caller supplied. No body was
    /// transferred and none is returned.
    NotModified,
    /// A fresh 2xx body, plus the response's `ETag` when the server offered one and it
    /// is safe to echo back ([`validator_safe`]).
    Body {
        bytes: Vec<u8>,
        etag: Option<String>,
    },
}

/// Whether a server-supplied `ETag` may be echoed back on a later request.
///
/// This value is fully SERVER-CONTROLLED and, unlike a response body, it goes back out
/// on curl's ARGV as an `If-None-Match:` header value — so it gets the same treatment
/// the token and the asset URL get elsewhere in this module. The grammar (RFC 9110
/// §8.8.3) is an optional `W/` prefix then a quoted string of visible ASCII; anything
/// carrying a control character, a space, a newline, or an interior quote is refused,
/// which makes header injection through this channel structurally impossible.
///
/// Refusal is not a failure: an unusable validator simply means the next request goes
/// out unconditionally, i.e. exactly what the caller did before this existed.
#[must_use]
pub fn validator_safe(validator: &str) -> bool {
    let body = validator.strip_prefix("W/").unwrap_or(validator);
    if validator.len() > 128 || body.len() < 2 {
        return false;
    }
    if !(body.starts_with('"') && body.ends_with('"')) {
        return false;
    }
    // Every byte visible ASCII (no CTL, no space, no DEL) — checked over the whole
    // value, prefix included.
    if !validator.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
        return false;
    }
    // …and no interior quote, which would close the header value early.
    body.get(1..body.len().saturating_sub(1))
        .is_some_and(|inner| !inner.contains('"'))
}

/// Whether an HTTP status is the "you already have it" answer — and ONLY when we asked.
///
/// The `sent_validator` half is load-bearing, not defensive dressing: a captive portal
/// or a broken proxy that answers 304 to an UNCONDITIONAL request would otherwise be
/// telling this client "nothing changed" about a resource it never described, which is
/// precisely the shape of "a stale cache hides a real update". Without a validator on
/// the wire there is nothing for a 304 to be relative to, so it is a malformed answer
/// and falls through to the fail-closed classification below.
fn is_not_modified(code: &str, sent_validator: bool) -> bool {
    code == "304" && sent_validator
}

/// The extra curl options a conditional GET adds to [`api_get_args`], in order.
///
/// Kept as its own function so a unit test can assert BOTH sides: with neither a
/// validator nor a sink the list is byte-identical to the historical `api_get_args()`,
/// and with them the two flags appear exactly once each and still BEFORE the `--`
/// end-of-options marker `curl_argv` appends (a caller-side `--` is the v0.5.10
/// auto-update-bricking regression).
fn conditional_args<'a>(
    inm_header: Option<&'a str>,
    header_dump: Option<&'a str>,
) -> Vec<&'a str> {
    let mut args: Vec<&str> = api_get_args().to_vec();
    if let Some(header) = inm_header {
        args.push("-H");
        args.push(header);
    }
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

/// The last `ETag` in a curl `--dump-header` capture, if it is safe to echo back.
///
/// LAST rather than first: a redirect chain writes one header block per hop, and the
/// validator that matters is the one belonging to the response whose body we kept.
/// A missing, malformed or unsafe value yields `None`, which degrades the next check to
/// an unconditional GET — never to a wrong answer.
fn etag_from_header_dump(path: &Path) -> Option<String> {
    let raw = std::fs::read(path).ok()?;
    let text = String::from_utf8_lossy(&raw);
    let mut found: Option<String> = None;
    for line in text.lines() {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case("etag") {
            found = Some(value.trim().to_string());
        }
    }
    found.filter(|e| validator_safe(e))
}

/// [`api_get_classified`], made CONDITIONAL: when `validator` is a stored `ETag`, the
/// request carries `If-None-Match` and the server may answer 304 with no body at all.
///
/// # Why this exists
///
/// The app updater's check re-downloaded and re-parsed the ENTIRE GitHub release
/// history to learn one tag, every 75 s, forever. MEASURED 2026-08-20 against the real
/// channel (`alabsystems/aterm`, anonymous lane): page 1 = **594,708 bytes** for 42
/// releases / 200 assets — ~14.2 KB per release, because each asset object embeds a full
/// uploader user block — i.e. ~28.5 MB/hour ≈ 685 MB/day per running instance, growing
/// with every cut and by a whole page per 100. The same probe with `If-None-Match`
/// returned **HTTP 304 with `size_download=0`**: no body, nothing to parse.
///
/// The saving is BYTES and CPU, not requests. The same probe showed the 304 still
/// consuming one unit of `x-ratelimit-used`, so the request budget the cadence constant
/// was chosen against is unchanged — do not claim otherwise.
///
/// # Freshness is the SERVER's word, never ours
///
/// There is no TTL and no offline reuse. `NotModified` can only be returned when this
/// very request carried the caller's validator and this very response said 304
/// ([`is_not_modified`]). A server that ignores `If-None-Match`, a proxy that strips
/// the header, a validator we refuse as unsafe, an absent memo — every one of those
/// lands on a 200 and therefore on the byte-for-byte historical path. The failure
/// direction is "no saving", never "stale answer".
///
/// `header_sink` is where curl dumps the response headers so the `ETag` can be read
/// back; pass `None` (and no validator) for the unconditional lane, which then spawns
/// the exact historical argv.
// Skip: same audited display-lossy Err-path class as `api_get`.
#[cfg_attr(trust_verify, trust::skip)]
pub fn api_get_conditional(
    url: &str,
    token: Option<&str>,
    validator: Option<&str>,
    header_sink: Option<&Path>,
) -> Result<ApiResponse, HttpError> {
    // An unusable validator is dropped HERE, so everything below sees a single truth
    // about whether a conditional request went out.
    let validator = validator.filter(|v| validator_safe(v));
    let inm = validator.map(|v| {
        let mut header = String::from("If-None-Match: ");
        header.push_str(v);
        header
    });
    let sink = header_sink.and_then(|p| p.to_str());
    let args = conditional_args(inm.as_deref(), sink);
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
            // truncates the dump file on open, so this is belt-and-suspenders — but the
            // validator it yields is what a later 304 is relative to, and a validator
            // describing a response we did not receive is the one way a conditional
            // request could be told "unchanged" about the wrong bytes. Owning the
            // invariant here costs one `unlink` per request; a failure to remove is
            // harmless (worst case: no validator, i.e. an unconditional next check).
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
            return Ok(ApiResponse::Body {
                bytes: body.as_bytes().to_vec(),
                // Only read back when we asked for a dump; a caller on the
                // unconditional lane spawns no `--dump-header` and touches no file.
                etag: header_sink.and_then(etag_from_header_dump),
            });
        }
        // The steady state this function exists for: no body was transferred, no JSON
        // will be parsed, and the caller keeps what it already had. Placed AFTER the
        // 2xx arm and BEFORE every failure arm, and gated on having actually sent a
        // validator (see [`is_not_modified`]).
        if is_not_modified(code, validator.is_some()) {
            return Ok(ApiResponse::NotModified);
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
        // already gone; we surface it as back-off-and-retry-next-cycle (F11). It is the
        // ROUTINE outcome on the anonymous lane, whose budget is ~60 requests/hour per
        // IP.
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
/// NOTE: no `-w "\n%{http_code}"` here (and none in [`download_to_args`]). These carry
/// `-f`, so curl's exit status already reports a non-2xx, and appending the status to
/// stdout would CORRUPT the downloaded bytes — including the appcast the Ed25519
/// signature covers. Asset downloads therefore stay unclassified; every public/private
/// verdict is taken from the releases LIST, which always runs first.
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
        // `pipeline` failure and — three checks later on a saturated anonymous IP —
        // the loud "download pipeline is likely broken" notice (2026-08-19 audit).
        // On the asset endpoint the only 403 an anonymous public-channel client
        // ever meets is the rate limit (a private asset answers 404), and with a
        // token an auth failure has already been classified by the releases list.
        if let Some(code) = curl_http_error_code(&stderr)
            && (code == 429 || code == 403)
        {
            return Err(format!("{RATE_LIMIT_ERROR_PREFIX}{code}) fetching asset"));
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

/// Whether a [`download_bytes`] error describes a GitHub rate limit (HTTP 429/403 on
/// the asset endpoint) rather than a broken download.
#[must_use]
pub fn download_error_is_rate_limit(error: &str) -> bool {
    error.contains(RATE_LIMIT_ERROR_PREFIX)
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
        "-fSL",
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
    let dest_s = dest.to_str().ok_or("non-UTF-8 destination path")?;
    let cap = max_filesize.to_string();
    // Both must outlive the argv array, which borrows them as `&str`.
    let max_time = download_max_time_secs(max_filesize).to_string();
    let out = curl_fetch(&download_to_args(&cap, &max_time, dest_s), asset_url, token)?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        // Same classification as `download_bytes`: a 429/403 on the asset lane is
        // named as a rate limit so the check lane can defer instead of booking a
        // broken pipeline (the caller bounds how long that classification is trusted).
        if let Some(code) = curl_http_error_code(&stderr)
            && (code == 429 || code == 403)
        {
            return Err(format!("{RATE_LIMIT_ERROR_PREFIX}{code}) fetching asset"));
        }
        return Err(format!("curl download failed ({}): {}", out.status, stderr.trim()));
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
    require_https_url(asset_url)?;
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
        let out = curl_fetch(
            &download_resume_args(&cap, &max_time, part_s, offset),
            asset_url,
            token,
        )?;
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
            // Same classification as `download_to`: a 429/403 on the asset lane is named
            // as a rate limit so the check lane can defer instead of booking a broken
            // pipeline.
            if let Some(code) = curl_http_error_code(&stderr)
                && (code == 429 || code == 403)
            {
                return Err(format!("{RATE_LIMIT_ERROR_PREFIX}{code}) fetching asset"));
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
    #[test]
    fn a_rate_limited_asset_fetch_is_named_and_a_broken_one_is_not() {
        assert_eq!(
            super::curl_http_error_code("curl: (22) The requested URL returned error: 429"),
            Some(429)
        );
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
    }

    use super::{
        HttpError, RELEASE_ASSET_DOWNLOAD_BOUND, api_get_args, conditional_args, curl_argv,
        curl_bin, curl_fetch, curl_prepared, download_bytes_args, download_max_time_secs,
        download_resume_args, download_to_args, download_to_resumable, etag_from_header_dump,
        is_not_modified, keep_partial, part_path, range_refused, resume_plan, token_config_safe,
        transient_api_status, validator_safe,
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
    /// The historical lane must be BYTE-IDENTICAL. `api_get_classified` now delegates to
    /// the conditional form, so the one thing that could regress every existing caller is
    /// the argv growing a flag; with no validator and no sink it must be exactly the list
    /// it always was.
    #[test]
    fn the_unconditional_lane_argv_is_unchanged() {
        assert_eq!(
            conditional_args(None, None),
            api_get_args().to_vec(),
            "an unconditional GET must spawn the historical option list, unchanged"
        );
    }

    /// …and a conditional one adds EXACTLY two flag pairs, in front of the `--` marker
    /// `curl_argv` appends (callers must never place their own — the v0.5.10 bricking
    /// regression).
    #[test]
    fn a_conditional_request_adds_exactly_the_validator_and_the_sink() {
        let args = conditional_args(
            Some("If-None-Match: W/\"deadbeef\""),
            Some("/tmp/aterm-updates/catalog.headers"),
        );
        let base = api_get_args().len();
        assert_eq!(args.len(), base + 4, "two flag pairs and nothing else: {args:?}");
        assert_eq!(args[base], "-H");
        assert_eq!(args[base + 1], "If-None-Match: W/\"deadbeef\"");
        assert_eq!(args[base + 2], "--dump-header");
        assert_eq!(args[base + 3], "/tmp/aterm-updates/catalog.headers");
        assert!(!args.contains(&"--"), "no caller-side end-of-options marker: {args:?}");
        // The base list survives verbatim underneath.
        assert_eq!(&args[..base], &api_get_args()[..]);
        // Each half is independently optional.
        assert_eq!(conditional_args(Some("If-None-Match: \"x\""), None).len(), base + 2);
        assert_eq!(conditional_args(None, Some("/tmp/h")).len(), base + 2);
    }

    /// A 304 may ONLY be believed when this request carried a validator. A captive
    /// portal or proxy answering 304 to an unconditional GET would otherwise be telling
    /// the updater "nothing changed" about a resource it never described — the exact
    /// shape of "a stale cache hides a real update".
    #[test]
    fn only_a_request_that_sent_a_validator_may_be_told_nothing_changed() {
        assert!(is_not_modified("304", true));
        assert!(!is_not_modified("304", false), "unsolicited 304 must not be honoured");
        for code in ["200", "301", "403", "404", "500", "3040", "", "30"] {
            assert!(!is_not_modified(code, true), "{code} is not a 304");
        }
    }

    /// The validator goes back out on ARGV as a header value, so it gets the token's
    /// treatment: a strict grammar, and refusal degrades to an unconditional request
    /// rather than to anything unsafe.
    #[test]
    fn only_well_formed_validators_are_echoed_back() {
        for good in [
            "\"6f1c8b1e5f0a\"",
            "W/\"6f1c8b1e5f0a\"",
            "W/\"gzip-4d2-8ab\"",
        ] {
            assert!(validator_safe(good), "real ETag rejected: {good:?}");
        }
        for bad in [
            "",                        // absent
            "6f1c8b1e",                // unquoted
            "\"a",                     // unterminated
            "\"a\"\r\nX-Evil: 1",      // CRLF header injection
            "\"a\nb\"",                // newline
            "\"a b\"",                 // space (would split the header)
            "\"a\"b\"",                // interior quote closes the value early
            "\"a\tb\"",                // control character
            "W/",                      // prefix only
        ] {
            assert!(!validator_safe(bad), "injection-shaped validator accepted: {bad:?}");
        }
        // Absurdly long values are refused too (bounded argv).
        let long = format!("\"{}\"", "a".repeat(200));
        assert!(!validator_safe(&long));
    }

    /// The header dump is parsed for the LAST `ETag` (a redirect chain writes one block
    /// per hop) and an unsafe value is dropped rather than echoed.
    #[test]
    fn the_last_safe_etag_is_read_back_from_the_dump() {
        let dir = std::env::temp_dir().join(format!("aterm-http-etag-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("headers");

        std::fs::write(
            &p,
            "HTTP/2 301\r\nETag: \"first-hop\"\r\n\r\nHTTP/2 200\r\netag: W/\"final\"\r\n\
             Content-Type: application/json\r\n\r\n",
        )
        .unwrap();
        assert_eq!(
            etag_from_header_dump(&p).as_deref(),
            Some("W/\"final\""),
            "the LAST block's validator is the one describing the body we kept"
        );

        std::fs::write(&p, "HTTP/2 200\r\nContent-Type: application/json\r\n\r\n").unwrap();
        assert_eq!(etag_from_header_dump(&p), None, "no ETag ⇒ no conditional next time");

        std::fs::write(&p, "HTTP/2 200\r\nETag: not-quoted\r\n\r\n").unwrap();
        assert_eq!(etag_from_header_dump(&p), None, "an unsafe validator is dropped");

        assert_eq!(etag_from_header_dump(&dir.join("absent")), None);
        let _ = std::fs::remove_dir_all(&dir);
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
        assert_eq!(resumed[fresh.len() + 1], "40", "an EXPLICIT offset, never `-`");
        // The sink is the PART, never the destination: `dest` only ever exists complete.
        let sink = resumed[resumed.iter().position(|a| *a == "-o").unwrap() + 1];
        assert!(sink.ends_with(".part"), "{resumed:?}");
        // The stall detector and the derived wall clock are untouched by resuming.
        for flag in ["--speed-limit", "--speed-time", "--connect-timeout", "--retry"] {
            assert!(resumed.contains(&flag), "{flag} must survive: {resumed:?}");
        }
        assert!(!resumed.contains(&"--"), "no caller-side end-of-options marker");
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
        assert!(!keep_partial(600_000_000, 4), "a truncated/clobbered prefix");
    }

    /// The in-call fresh-retry trigger: exactly the failures that name the RANGE (curl
    /// exit 33; HTTP 416 through `--fail`'s exit 22) qualify, and nothing else — a
    /// stalled transfer, a 404, or a rate limit would fail from offset 0 too, so
    /// retrying them fresh would only double the cost of an already-failed attempt.
    #[test]
    fn only_a_range_refusal_earns_the_in_call_fresh_retry() {
        assert!(range_refused(Some(33), ""), "curl 33: server refuses ranges");
        assert!(
            range_refused(
                Some(22),
                "curl: (22) The requested URL returned error: 416"
            ),
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
        assert!(!range_refused(None, ""), "a signal-killed curl proves nothing");
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
