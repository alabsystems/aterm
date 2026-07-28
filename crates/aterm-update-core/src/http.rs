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
    // caller in this crate passes a fixed option list of <= 13 items, so the clamp
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

/// GET a GitHub API JSON resource, returning the raw body bytes. Distinguishes an
/// authentication failure (401/403 — expired/revoked/insufficient token, or no token
/// against a private repo) from a rate limit and from a transient error, so the
/// caller can act on the difference instead of collapsing it into one string. We
/// append the HTTP status via `-w` and DON'T pass `-f` (we want the code even on 4xx).
// Skip: response-text handling — from_utf8_lossy over curl output (display/
// classification only; the byte-exact BODY is returned untouched as Vec<u8>)
// and the trailing-status split arithmetic, whose bounds ride the lossy
// Cow (unmodeled). Every malformed shape returns Err (fail-closed).
// Audited (update-atpkg); droppable with the byte-exact contract lane.
#[cfg_attr(trust_verify, trust::skip)]
pub fn api_get_classified(url: &str, token: Option<&str>) -> Result<Vec<u8>, HttpError> {
    let out = curl_fetch(
        &[
            "-sS",
            "--retry",
            "2",
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
        ],
        url,
        token,
    )
    .map_err(HttpError::Transport)?;
    if !out.status.success() {
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
    // GitHub signals rate limiting with 429, or a 403 whose body mentions a (primary
    // or secondary) rate limit. That is TRANSIENT — the credential (or its absence) is
    // not the problem — so it must not be reported as an auth failure ("rotate the
    // token"), and `--retry` doesn't cover 403 anyway; we surface it as
    // back-off-and-retry-next-cycle (F11). It is the ROUTINE outcome on the anonymous
    // lane, whose budget is ~60 requests/hour per IP.
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
    match numeric {
        401 | 403 => Err(HttpError::Unauthorized { code: numeric }),
        404 => Err(HttpError::NotFound {
            url: url.to_string(),
        }),
        other => Err(HttpError::Status {
            code: other,
            url: url.to_string(),
        }),
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

/// Download a SMALL asset's bytes (e.g. a manifest) into memory, size-capped at
/// `max_filesize` bytes so a rogue/oversized asset can't be buffered whole. The cap
/// is caller-supplied (the meaning of "small" is artifact-specific); curl aborts
/// before reading past it.
// Skip: same audited display-lossy Err-path class as `api_get`.
#[cfg_attr(trust_verify, trust::skip)]
pub fn download_bytes(
    asset_url: &str,
    token: Option<&str>,
    max_filesize: u64,
) -> Result<Vec<u8>, String> {
    require_https_url(asset_url)?;
    let cap = max_filesize.to_string();
    // NOTE: no `-w "\n%{http_code}"` here (and none in `download_to`). These carry
    // `-f`, so curl's exit status already reports a non-2xx, and appending the status
    // to stdout would CORRUPT the downloaded bytes — including the appcast the
    // Ed25519 signature covers. Asset downloads therefore stay unclassified; every
    // public/private verdict is taken from the releases LIST, which always runs first.
    let out = curl_fetch(
        &[
            "-fsSL",
            // Redirects (GitHub's 302 to object storage) may only land on https —
            // `-L` alone would also follow http/ftp(s), a MITM downgrade vector.
            "--proto-redir",
            "=https",
            "--retry",
            "2",
            "--max-time",
            "60",
            "--max-filesize",
            &cap,
            "-H",
            "Accept: application/octet-stream",
            // The `--` end-of-options guard for the server-controlled asset URL is
            // appended by `curl_argv` (AFTER the auth channel — see its invariants);
            // `require_https_url` above closes the scheme-injection vector.
        ],
        asset_url,
        token,
    )?;
    if !out.status.success() {
        return Err(format!(
            "curl asset download failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out.stdout)
}

/// Download an asset (e.g. a DMG) to a file, following the storage redirect.
/// Bounded at `max_filesize` bytes (caller-supplied) so an attacker-controlled or
/// mis-pointed release asset can't fill the disk — curl aborts before writing past
/// it.
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
    let out = curl_fetch(
        &[
            "-fSL",
            // https-only redirects — see `download_bytes`.
            "--proto-redir",
            "=https",
            "--retry",
            "2",
            "--max-time",
            "600",
            "--max-filesize",
            &cap,
            "-H",
            "Accept: application/octet-stream",
            "-o",
            dest_s,
            // The `--` guard before the server-controlled asset URL is appended by
            // `curl_argv` (see `download_bytes`); `require_https_url` rejects
            // non-https schemes.
        ],
        asset_url,
        token,
    )?;
    if !out.status.success() {
        return Err(format!(
            "curl download failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{HttpError, curl_argv, curl_bin, curl_fetch, curl_prepared, token_config_safe};
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
            .err()
            .expect("an empty token must be refused before spawning curl");
        assert!(err.contains("empty"), "{err}");
        assert!(
            err.contains("anonymously"),
            "the remedy (request anonymously) must be named: {err}"
        );
        // The injection guard still fires ahead of any spawn, too.
        let err = curl_fetch(&["-sS"], "https://api.github.com/repos/o/r", Some("a\"b"))
            .err()
            .expect("an injection-shaped token must be refused");
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
}
