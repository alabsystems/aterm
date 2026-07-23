// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The network layer: authenticated `curl` calls to the PRIVATE GitHub Releases
//! API. Artifact-agnostic — these fetch arbitrary API JSON and asset bytes; the
//! consuming crate decides what the bytes mean.
//!
//! Private repos require the API (the `releases/latest/download/…` browser
//! shortcut needs web auth), so we authenticate every request with the per-machine
//! token (see [`crate::token`]) and download asset bytes via the asset API URL
//! with `Accept: application/octet-stream` (curl `-L` follows the 302 to storage
//! and drops the `Authorization` header on the cross-host redirect by default).
//!
//! The token is fed to curl through STDIN ([`curl_auth`], `curl --config -`), never
//! on argv, so it is not exposed to same-user processes via `ps`.

use std::path::Path;
use std::process::Command;

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
/// the caller's options, the fixed User-Agent, the `--config -` stdin auth channel,
/// and LAST `--` + the URL. Pure and separate from the spawn so the ordering — the
/// security- AND correctness-critical part — is unit-testable.
///
/// ORDERING INVARIANTS (each guards against a real failure):
/// * `-q` is the very first parameter (see [`curl_auth`] — booby-trapped curlrc).
/// * `--config -` comes BEFORE the `--` end-of-options marker: everything after
///   `--` is a URL to curl, so a misplaced marker silently DISABLES authentication
///   and turns the remaining options into bogus URLs. Exactly that shipped in
///   v0.5.10/v0.5.11 (the caller-side `--` in `download_bytes`/`download_to`
///   preceded the appended auth args): every private-repo asset download failed
///   404-unauthenticated, bricking auto-update on those builds.
/// * `--` immediately precedes the URL: the asset URL from the releases JSON is the
///   one fully server-controlled string that reaches curl, so a leading-dash value
///   (`-K/tmp/evil`) must parse as a URL, never as an option.
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
fn curl_argv(args: &[&str], url: &str) -> Vec<String> {
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
        ["-H", "User-Agent: aterm-update", "--config", "-", "--"]
            .iter()
            .map(|s| (*s).to_string()),
    );
    v.push(url.to_string());
    v
}

/// Run curl against `url` with extra `args`, feeding the secret `Authorization`
/// header through STDIN (`curl --config -`) so the token NEVER appears in argv —
/// argv is world-visible to same-user processes via `ps`. The URL is passed
/// separately so [`curl_argv`] can place the `--` end-of-options marker directly
/// before it, AFTER every option including the auth channel (callers must NOT put
/// `--` in `args` — that is the v0.5.10 auto-update-bricking regression). Returns
/// the completed process output.
// Skip: same audited display-lossy Err-path class as `api_get`.
#[cfg_attr(trust_verify, trust::skip)]
fn curl_auth(args: &[&str], url: &str, token: &str) -> Result<std::process::Output, String> {
    use std::io::Write;
    use std::process::Stdio;
    if !token_config_safe(token) {
        return Err(
            "update token contains illegal characters (control/quote/backslash) — refusing \
             to build the curl config line"
                .to_string(),
        );
    }
    let mut child = Command::new(curl_bin())
        // `-q` MUST be first (curl_argv puts it first): curl reads the default
        // ~/.curlrc (or $CURL_HOME / $XDG_CONFIG_HOME) EVEN when `--config` is
        // given, unless `-q` is the very first parameter. A hostile/booby-trapped
        // curlrc could add `insecure` + `proxy = http://attacker/` and exfiltrate
        // the Bearer token we plumb below. `-q` disables ONLY the default config
        // file — it does NOT disable the explicit `--config -` stdin that carries
        // the Authorization header, so token delivery is unaffected. Also scrub the
        // config-dir env vars so the default-config lookup cannot be redirected.
        .args(curl_argv(args, url))
        .env_remove("CURL_HOME")
        .env_remove("XDG_CONFIG_HOME")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
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

/// GET a GitHub API JSON resource, returning the raw body bytes. Distinguishes an
/// authentication failure (401/403 — expired/revoked/insufficient token) from a
/// transient error, so the status/log says something actionable. We append the
/// HTTP status via `-w` and DON'T pass `-f` (we want the code even on 4xx).
// Skip: response-text handling — from_utf8_lossy over curl output (display/
// classification only; the byte-exact BODY is returned untouched as Vec<u8>)
// and the trailing-status split arithmetic, whose bounds ride the lossy
// Cow (unmodeled). Every malformed shape returns Err (fail-closed).
// Audited (update-atpkg); droppable with the byte-exact contract lane.
#[cfg_attr(trust_verify, trust::skip)]
pub fn api_get(url: &str, token: &str) -> Result<Vec<u8>, String> {
    let out = curl_auth(
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
    )?;
    if !out.status.success() {
        // Transport-level failure (curl exit != 0): DNS, TLS, timeout, etc.
        return Err(format!(
            "curl GET {} failed ({}): {}",
            url,
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    // Split the trailing "\n<http_code>" we appended via -w.
    let stdout = out.stdout;
    let text = String::from_utf8_lossy(&stdout);
    let (body, code) = match text.rfind('\n') {
        Some(i) => (&text[..i], text[i + 1..].trim()),
        None => ("", text.trim()),
    };
    // GitHub signals rate limiting with 429, or a 403 whose body mentions a (primary
    // or secondary) rate limit. That is TRANSIENT — the token is fine — so it must not
    // be reported as an auth failure ("rotate the token"), and `--retry` doesn't cover
    // 403 anyway; we surface it as back-off-and-retry-next-cycle (F11).
    let rate_limited =
        code == "429" || (code == "403" && body.to_ascii_lowercase().contains("rate limit"));
    match code {
        c if c.starts_with('2') => Ok(body.as_bytes().to_vec()),
        _ if rate_limited => Err(format!(
            "GitHub rate limit hit (HTTP {code}) for {url}; transient (the token is \
             valid) — backing off, will retry on the next check"
        )),
        "401" | "403" => Err(format!(
            "GitHub auth failed (HTTP {code}): the update token is missing required \
             access, expired, or was revoked — rotate it (see docs/RELEASING.md)"
        )),
        "404" => Err(format!(
            "GitHub returned HTTP 404 for {url} (repo/releases not found, or the token \
             lacks access to this private repo)"
        )),
        other => Err(format!("GitHub API returned HTTP {other} for {url}")),
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
pub fn download_bytes(asset_url: &str, token: &str, max_filesize: u64) -> Result<Vec<u8>, String> {
    require_https_url(asset_url)?;
    let cap = max_filesize.to_string();
    let out = curl_auth(
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
    token: &str,
    dest: &Path,
    max_filesize: u64,
) -> Result<(), String> {
    require_https_url(asset_url)?;
    let dest_s = dest.to_str().ok_or("non-UTF-8 destination path")?;
    let cap = max_filesize.to_string();
    let out = curl_auth(
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
    use super::{curl_argv, curl_bin, token_config_safe};

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
        let v = curl_argv(&["-fsSL"], "-K/tmp/evil");
        let dashdash = v.iter().position(|a| a == "--").unwrap();
        assert_eq!(v[dashdash + 1], "-K/tmp/evil");
        assert_eq!(dashdash + 2, v.len());
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
