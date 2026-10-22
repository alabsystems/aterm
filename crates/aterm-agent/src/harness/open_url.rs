// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-open-url <url>`: THE OPENER A RENEWAL'S SIGN-IN RUNS AS `BROWSER`
//! (`docs/DESIGN-claude-accounts-v2-2026-10-15.md` §5.3 step 2): the one
//! binary answers to it as an argv0 alias, so the variable names ONE
//! executable, as Claude Code needs (READ in the 2.1.300 bundle: the opener
//! is spawned with no shell, `BROWSER || "open"` and the URL as its one
//! argument).
//!
//! It opens the URL in the browser profile that holds the account's
//! claude.ai session: [`PROFILE_ENV`] names the profile by its DISPLAY name
//! (`Claude m7-a`, what Chrome shows), Chrome's own `Local State` maps that
//! name to the profile's directory ([`profile_dir_of`]), and `open -na
//! "Google Chrome" --args --profile-directory=<dir> <url>` opens it
//! ([`open_command`]).
//!
//! WITH NO PROFILE NAMED — what a renewal over SSH hands it — it opens
//! NOTHING (exit 1): the owner's browser is on the machine they are at, and
//! Claude Code calls its opener under `SSH_CONNECTION` too (MEASURED on
//! 2.1.300; with `BROWSER` unset that is `open`, a browser on this machine's
//! screen).
//!
//! It NEVER LOGS OR PRINTS THE URL (it names the account's address as its
//! `login_hint`). The manual profile path takes only an `https://` URL.
//! An explicitly configured mailbox can instead complete one same-origin
//! Claude authorization flow, with no provider cookies at its exact local
//! callback. Unknown origins or flows fall back to the printed URL; this is
//! not a browser or a general cookie jar. ANY failure — no profile named, Chrome's state
//! unreadable, no such profile, a non-macOS machine, `open` refused — is exit
//! 1 with one line on stderr that names no URL, and Claude Code then prints
//! the URL itself (`If the browser didn't open, visit: …`) for the owner to
//! open by hand.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use aterm_json::Value;

/// The argv0 name the one binary answers as the opener, and the name of the
/// link a renewal makes to it (`<accounts store>/bin/aterm-open-url`).
pub const OPENER: &str = "aterm-open-url";

/// The variable a renewal's flow carries the profile's display name in.
pub const PROFILE_ENV: &str = "ATERM_OPEN_URL_PROFILE";

/// The variable naming a directory of incoming verification / magic-link
/// emails (`<accounts store>/mailbox` or test mailbox) for headless OAuth +
/// email-link authorization.
pub const MAILBOX_ENV: &str = "ATERM_OPEN_URL_MAILBOX";

/// The variable naming the target account email address for headless OAuth +
/// email-link authorization (falls back to `login_hint` in the authorize URL).
pub const EMAIL_ENV: &str = "ATERM_OPEN_URL_EMAIL";

/// Default subdirectory under `<accounts store>` checked for incoming
/// verification / magic-link emails when [`MAILBOX_ENV`] is unset.
pub const DEFAULT_MAILBOX_DIR: &str = "mailbox";

/// The most bytes of Chrome's `Local State` that are read.
pub const LOCAL_STATE_MAX: u64 = 8 * 1024 * 1024;

/// Maximum bytes read from a single incoming email message file.
const EMAIL_FILE_MAX: u64 = 512 * 1024;

/// Default timeout for headless OAuth + email magic-link authorization.
pub const AUTHORIZE_TIMEOUT: Duration = Duration::from_secs(30);

/// Chrome's `Local State` under `home` (macOS).
#[must_use]
pub fn local_state_path(home: &Path) -> PathBuf {
    home.join("Library")
        .join("Application Support")
        .join("Google")
        .join("Chrome")
        .join("Local State")
}

/// THE PROFILE DIRECTORY whose display name is `name` in Chrome's `Local
/// State` text (`profile.info_cache.<dir>.name`), compared exactly. `None`
/// for text that is not that document, no such profile, two profiles of the
/// one name (which one the owner meant is not known), and a directory name
/// that is not one (`Default`, `Profile 3`: no path separator, no leading
/// `-`, no control character).
#[must_use]
pub fn profile_dir_of(local_state: &str, name: &str) -> Option<String> {
    let doc: Value = aterm_json::from_str(local_state).ok()?;
    let cache = doc.get("profile")?.get("info_cache")?.as_object()?;
    let mut named = cache
        .iter()
        .filter(|(_, profile)| profile.get("name").and_then(Value::as_str) == Some(name))
        .map(|(dir, _)| dir);
    let dir = named.next()?;
    if named.next().is_some() {
        return None;
    }
    let plain = !dir.is_empty()
        && !dir.starts_with('-')
        && !dir.contains(['/', '\\'])
        && !dir.chars().any(char::is_control);
    plain.then(|| dir.clone())
}

/// `open -na "Google Chrome" --args --profile-directory=<dir> <url>`: the
/// URL in that profile, a new window if Chrome runs already. Built by the
/// workspace's own command, which hands the child its stdio and nothing else.
#[must_use]
pub fn open_command(dir: &str, url: &str) -> aterm_uds::spawnfd::Command {
    let mut cmd = aterm_uds::spawnfd::command("open");
    cmd.args(["-na", "Google Chrome", "--args"])
        .arg(format!("--profile-directory={dir}"))
        .arg(url);
    cmd
}

/// Why `url` is not opened, or `None`: only an `https://` URL with no
/// whitespace or control character in it — what Claude Code's sign-in hands
/// over, and nothing an opener should take that is not.
#[must_use]
pub fn url_refusal(url: &str) -> Option<&'static str> {
    if !url.starts_with("https://") {
        Some("it opens https:// URLs only")
    } else if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        Some("the URL holds a space or a control character")
    } else {
        None
    }
}

/// The opener's work, the machine's reads injected: `profile` is
/// [`PROFILE_ENV`]'s value, `local_state` reads Chrome's state, `run` starts
/// the command and answers whether it exited 0. Answers the exit code and,
/// for a failure, the one line said (never the URL).
pub fn open_with(
    args: &[String],
    profile: Option<&str>,
    local_state: &dyn Fn() -> Option<String>,
    run: &mut dyn FnMut(aterm_uds::spawnfd::Command) -> bool,
) -> (u8, Option<String>) {
    let [url] = args else {
        return (2, Some(format!("{OPENER}: takes one URL")));
    };
    if let Some(why) = url_refusal(url) {
        return (1, Some(format!("{OPENER}: not opened: {why}")));
    }
    let Some(profile) = profile.filter(|p| !p.trim().is_empty()) else {
        return (
            1,
            Some(format!("{OPENER}: no profile named ({PROFILE_ENV})")),
        );
    };
    if !cfg!(target_os = "macos") {
        return (
            1,
            Some(format!("{OPENER}: opens a Chrome profile on macOS only")),
        );
    }
    let Some(text) = local_state() else {
        return (
            1,
            Some(format!("{OPENER}: Chrome's profile list cannot be read")),
        );
    };
    let Some(dir) = profile_dir_of(&text, profile) else {
        return (
            1,
            Some(format!(
                "{OPENER}: Chrome has no one profile named {}",
                super::one_line(profile, 64)
            )),
        );
    };
    if run(open_command(&dir, url)) {
        (0, None)
    } else {
        (1, Some(format!("{OPENER}: `open` did not open Chrome")))
    }
}

/// Percent-decode a URL query component (`+` -> space, `%XX` -> byte).
#[must_use]
pub fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'+' {
            out.push(b' ');
            i += 1;
        } else if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (
                (bytes[i + 1] as char).to_digit(16),
                (bytes[i + 2] as char).to_digit(16),
            )
        {
            out.push(((hi << 4) | lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Percent-encode a form field component for `application/x-www-form-urlencoded`.
#[must_use]
pub fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for b in input.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            use std::fmt::Write as _;
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// Extract a decoded query parameter from `url`.
#[must_use]
pub fn extract_query_param(url: &str, key: &str) -> Option<String> {
    let (_, query) = url.split_once('?')?;
    let query = query.split('#').next().unwrap_or(query);
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=')
            && k == key
        {
            return Some(percent_decode(v));
        }
    }
    None
}

/// Extract the hinted email (`login_hint` or `email`) from an OAuth authorize URL.
#[must_use]
pub fn extract_login_hint(authorize_url: &str) -> Option<String> {
    extract_query_param(authorize_url, "login_hint")
        .or_else(|| extract_query_param(authorize_url, "email"))
        .filter(|s| !s.trim().is_empty())
}

/// Decode RFC 2045 Quoted-Printable soft line breaks (`=\r\n`, `=\n`) and `=XX`
/// hex escapes so wrapped magic-link URLs in `.eml` files are reconstructed.
#[must_use]
pub fn decode_quoted_printable(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'=' {
            if i + 2 < bytes.len() && bytes[i + 1] == b'\r' && bytes[i + 2] == b'\n' {
                i += 3;
                continue;
            }
            if i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
                i += 2;
                continue;
            }
            if i + 2 < bytes.len()
                && let (Some(hi), Some(lo)) = (
                    (bytes[i + 1] as char).to_digit(16),
                    (bytes[i + 2] as char).to_digit(16),
                )
            {
                out.push(((hi << 4) | lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn decode_html_amp(input: &str) -> String {
    input
        .replace("&amp;", "&")
        .replace("&#38;", "&")
        .replace("&#x26;", "&")
        .replace("&#61;", "=")
}

fn is_supported_auth_url(url: &str) -> bool {
    !url.chars().any(|c| c.is_whitespace() || c.is_control())
        && (url.starts_with("https://")
            || url.starts_with("http://127.0.0.1:")
            || url.starts_with("http://localhost:"))
}

fn email_matches_target(raw_email: &str, target: &str) -> bool {
    let target_trimmed = target.trim();
    if target_trimmed.is_empty() {
        return true;
    }
    if let Ok(doc) = aterm_json::from_str::<Value>(raw_email)
        && let Some(to) = doc.get("to").and_then(Value::as_str)
    {
        return to
            .to_ascii_lowercase()
            .contains(&target_trimmed.to_ascii_lowercase());
    }
    let head = raw_email
        .split_once("\r\n\r\n")
        .or_else(|| raw_email.split_once("\n\n"))
        .map_or(raw_email, |(h, _)| h);
    let mut saw_to_header = false;
    for line in head.lines() {
        if let Some((k, v)) = line.split_once(':')
            && k.trim().eq_ignore_ascii_case("to")
        {
            saw_to_header = true;
            if v.to_ascii_lowercase()
                .contains(&target_trimmed.to_ascii_lowercase())
            {
                return true;
            }
        }
    }
    if saw_to_header {
        return false;
    }
    raw_email
        .to_ascii_lowercase()
        .contains(&target_trimmed.to_ascii_lowercase())
}

/// Extract a verification / magic-link URL from an incoming email message
/// (`.eml` RFC 5322, HTML, plain text, or JSON) addressed to `target_email`.
#[must_use]
pub fn extract_magic_link_from_email(
    raw_email: &str,
    target_email: Option<&str>,
) -> Option<String> {
    if let Some(target) = target_email
        && !email_matches_target(raw_email, target)
    {
        return None;
    }
    if let Ok(doc) = aterm_json::from_str::<Value>(raw_email) {
        for key in ["magic_url", "verify_url", "login_url", "url"] {
            if let Some(u) = doc.get(key).and_then(Value::as_str)
                && is_supported_auth_url(u)
            {
                return Some(u.to_owned());
            }
        }
    }
    let decoded = decode_html_amp(&decode_quoted_printable(raw_email));
    let mut urls = Vec::new();
    for scheme in ["https://", "http://127.0.0.1:", "http://localhost:"] {
        let mut rest = decoded.as_str();
        while let Some(pos) = rest.find(scheme) {
            let after = &rest[pos..];
            let end = after
                .find(|c: char| {
                    c.is_whitespace()
                        || c.is_control()
                        || matches!(c, '"' | '\'' | '<' | '>' | ')' | ']')
                })
                .unwrap_or(after.len());
            let candidate = after[..end].trim_end_matches(['.', ',', ';']);
            if is_supported_auth_url(candidate) && !urls.iter().any(|u| u == candidate) {
                urls.push(candidate.to_owned());
            }
            rest = &after[end..];
        }
    }
    urls.iter()
        .find(|u| {
            let lower = u.to_ascii_lowercase();
            !lower.contains("unsubscribe")
                && (lower.contains("magic")
                    || lower.contains("verify")
                    || lower.contains("login")
                    || lower.contains("token=")
                    || lower.contains("auth")
                    || lower.contains("callback"))
        })
        .or_else(|| urls.first())
        .cloned()
}

fn resolve_relative_url(base_url: &str, target: &str) -> String {
    let target = decode_html_amp(target.trim());
    if target.starts_with("https://") || target.starts_with("http://") {
        return target;
    }
    let Some((scheme, rest)) = base_url.split_once("://") else {
        return target;
    };
    if target.starts_with("//") {
        return format!("{scheme}:{target}");
    }
    let authority = rest.split('/').next().unwrap_or(rest);
    if target.starts_with('/') {
        format!("{scheme}://{authority}{target}")
    } else {
        format!("{scheme}://{authority}/{target}")
    }
}

fn extract_attr(tag: &str, attr_name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let needle = format!("{attr_name}=");
    let mut search_from = 0;
    while let Some(rel) = lower[search_from..].find(&needle) {
        let idx = search_from + rel;
        let prev_ok = idx == 0
            || tag.as_bytes()[idx - 1].is_ascii_whitespace()
            || matches!(tag.as_bytes()[idx - 1], b'"' | b'\'');
        let val_start = idx + needle.len();
        search_from = val_start;
        if !prev_ok || val_start >= tag.len() {
            continue;
        }
        let rest = &tag[val_start..];
        if let Some(stripped) = rest.strip_prefix('"') {
            if let Some(end) = stripped.find('"') {
                return Some(decode_html_amp(&stripped[..end]));
            }
        } else if let Some(stripped) = rest.strip_prefix('\'') {
            if let Some(end) = stripped.find('\'') {
                return Some(decode_html_amp(&stripped[..end]));
            }
        } else {
            let end = rest
                .find(|c: char| c.is_ascii_whitespace() || c == '>')
                .unwrap_or(rest.len());
            return Some(decode_html_amp(&rest[..end]));
        }
    }
    None
}

/// Parse an OAuth consent / authorization page (after clicking the email magic
/// link) into `(HTTP method, resolved action URL, optional form body)`.
#[must_use]
pub fn parse_html_approval_action(
    html: &str,
    base_url: &str,
) -> Option<(String, String, Option<String>)> {
    if let Ok(doc) = aterm_json::from_str::<Value>(html)
        && let Some(approve) = doc.get("approve_url").and_then(Value::as_str)
    {
        return Some((
            "POST".to_owned(),
            resolve_relative_url(base_url, approve),
            None,
        ));
    }
    let lower = html.to_ascii_lowercase();
    if let Some(form_start) = lower.find("<form") {
        let form_slice = &html[form_start..];
        let form_end = lower[form_start..]
            .find("</form>")
            .map_or(form_slice.len(), |e| e + 7);
        let form_html = &form_slice[..form_end];
        let tag_end = form_html.find('>')?;
        let open_tag = &form_html[..=tag_end];
        let action = extract_attr(open_tag, "action").map_or_else(
            || base_url.to_owned(),
            |a| resolve_relative_url(base_url, &a),
        );
        let method = extract_attr(open_tag, "method")
            .unwrap_or_else(|| "GET".to_owned())
            .to_ascii_uppercase();
        let mut fields = Vec::new();
        let mut scan = form_html;
        while let Some(in_pos) = scan.to_ascii_lowercase().find("<input") {
            let after_in = &scan[in_pos..];
            let Some(end_in) = after_in.find('>') else {
                break;
            };
            let input_tag = &after_in[..=end_in];
            if let Some(name) = extract_attr(input_tag, "name") {
                let value = extract_attr(input_tag, "value").unwrap_or_default();
                fields.push(format!(
                    "{}={}",
                    percent_encode(&name),
                    percent_encode(&value)
                ));
            }
            scan = &after_in[end_in + 1..];
        }
        let encoded = fields.join("&");
        if method == "POST" {
            return Some(("POST".to_owned(), action, Some(encoded)));
        }
        let full_url = if encoded.is_empty() {
            action
        } else if action.contains('?') {
            format!("{action}&{encoded}")
        } else {
            format!("{action}?{encoded}")
        };
        return Some(("GET".to_owned(), full_url, None));
    }
    let mut scan = html;
    while let Some(a_pos) = scan.to_ascii_lowercase().find("<a ") {
        let after_a = &scan[a_pos..];
        let Some(end_a) = after_a.find('>') else {
            break;
        };
        let a_tag = &after_a[..=end_a];
        if let Some(href) = extract_attr(a_tag, "href") {
            let resolved = resolve_relative_url(base_url, &href);
            let r_low = resolved.to_ascii_lowercase();
            if r_low.contains("approve")
                || r_low.contains("authorize")
                || r_low.contains("callback")
            {
                return Some(("GET".to_owned(), resolved, None));
            }
        }
        scan = &after_a[end_a + 1..];
    }
    None
}

#[derive(Debug)]
struct HttpResponse {
    status: u16,
    location: Option<String>,
    body: String,
}

fn parse_raw_http_response(
    raw: &str,
    cookies: &mut BTreeMap<String, String>,
) -> Option<HttpResponse> {
    let mut current = raw;
    while let Some((head, rest)) = current
        .split_once("\r\n\r\n")
        .or_else(|| current.split_once("\n\n"))
    {
        let first_line = head.lines().next()?;
        let mut parts = first_line.split_ascii_whitespace();
        let proto = parts.next()?;
        let code_str = parts.next()?;
        let status: u16 = code_str.parse().ok()?;
        if proto.starts_with("HTTP/") && status == 100 && rest.starts_with("HTTP/") {
            current = rest;
            continue;
        }
        let mut location = None;
        for line in head.lines().skip(1) {
            let Some((name, val)) = line.split_once(':') else {
                continue;
            };
            let val = val.trim();
            if name.eq_ignore_ascii_case("location") {
                location = Some(val.to_owned());
            } else if name.eq_ignore_ascii_case("set-cookie") {
                let pair = val.split(';').next().unwrap_or(val).trim();
                if let Some((ck, cv)) = pair.split_once('=') {
                    cookies.insert(ck.trim().to_owned(), cv.trim().to_owned());
                }
            }
        }
        return Some(HttpResponse {
            status,
            location,
            body: rest.to_owned(),
        });
    }
    None
}

fn loopback_http_exchange(
    method: &str,
    url: &str,
    cookies: &mut BTreeMap<String, String>,
    form_body: Option<&str>,
) -> Result<HttpResponse, &'static str> {
    let rest = url
        .strip_prefix("http://")
        .ok_or("loopback client takes http:// only")?;
    let (authority, path_and_query) = match rest.find('/') {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None => (rest, "/"),
    };
    let (_, port_str) = authority
        .rsplit_once(':')
        .ok_or("loopback URL is missing a port")?;
    let port: u16 = port_str
        .parse()
        .map_err(|_| "loopback URL port is invalid")?;
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5))
        .map_err(|_| "could not connect to local OAuth / callback server")?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));

    let mut req =
        format!("{method} {path_and_query} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n");
    if !cookies.is_empty() {
        let cookie_hdr: Vec<String> = cookies.iter().map(|(k, v)| format!("{k}={v}")).collect();
        req.push_str(&format!("Cookie: {}\r\n", cookie_hdr.join("; ")));
    }
    if let Some(body) = form_body {
        req.push_str(&format!(
            "Content-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        ));
    } else {
        req.push_str("\r\n");
    }
    stream
        .write_all(req.as_bytes())
        .and_then(|()| stream.flush())
        .map_err(|_| "failed writing HTTP request")?;
    let mut buf = Vec::new();
    stream
        .take(512 * 1024)
        .read_to_end(&mut buf)
        .map_err(|_| "failed reading HTTP response")?;
    let raw = String::from_utf8_lossy(&buf);
    parse_raw_http_response(&raw, cookies).ok_or("malformed HTTP response")
}

fn https_curl_exchange(
    method: &str,
    url: &str,
    cookies: &mut BTreeMap<String, String>,
    form_body: Option<&str>,
) -> Result<HttpResponse, &'static str> {
    let mut cmd = aterm_uds::spawnfd::command("curl");
    // Ignore curlrc before any other option: it must not add redirects or
    // another destination behind the flow's per-request origin admission.
    cmd.args([
        "--disable",
        "--globoff",
        "--proto",
        "=https",
        "-sS",
        "-i",
        "--max-time",
        "15",
        "-X",
        method,
    ]);
    if !cookies.is_empty() {
        let cookie_hdr: Vec<String> = cookies.iter().map(|(k, v)| format!("{k}={v}")).collect();
        cmd.args(["-b", &cookie_hdr.join("; ")]);
    }
    if let Some(body) = form_body {
        cmd.args(["--data", body]);
    }
    cmd.arg(url);
    let out = cmd.output().map_err(|_| "failed running curl")?;
    if !out.status.success() {
        return Err("HTTPS request failed");
    }
    let raw = String::from_utf8_lossy(&out.stdout);
    parse_raw_http_response(&raw, cookies).ok_or("malformed HTTPS response")
}

fn http_exchange(
    method: &str,
    url: &str,
    cookies: &mut BTreeMap<String, String>,
    form_body: Option<&str>,
) -> Result<HttpResponse, &'static str> {
    if url.starts_with("http://127.0.0.1:") || url.starts_with("http://localhost:") {
        loopback_http_exchange(method, url, cookies, form_body)
    } else if url.starts_with("https://") {
        https_curl_exchange(method, url, cookies, form_body)
    } else {
        Err("unsupported URL scheme")
    }
}

/// Deliberately narrow admission, not a URL or cookie standard. This is
/// the Claude origin the existing opener contract names. Any other
/// origin (including a new vendor login origin) needs the manual browser.
fn provider_url(url: &str) -> bool {
    url.starts_with("https://claude.ai/")
        && !url.chars().any(|c| c.is_whitespace() || c.is_control())
        && !url.contains(['\\', '#'])
}

fn loopback_parts(url: &str) -> Option<(u16, &str, Option<&str>)> {
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) || url.contains(['\\', '#']) {
        return None;
    }
    let rest = url
        .strip_prefix("http://127.0.0.1:")
        .or_else(|| url.strip_prefix("http://localhost:"))?;
    let (port, path) = rest.split_once('/')?;
    let port = port.parse::<u16>().ok().filter(|port| *port > 0)?;
    let (path, query) = path
        .split_once('?')
        .map_or((path, None), |(path, query)| (path, Some(query)));
    Some((port, path, query))
}

fn one_query_param(url: &str, key: &str) -> Option<String> {
    let (_, query) = url.split_once('?')?;
    let mut values = query.split('&').filter_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (percent_decode(name) == key).then(|| percent_decode(value))
    });
    let value = values.next().filter(|value| !value.is_empty())?;
    values.next().is_none().then_some(value)
}

/// Only the callback endpoint and state actually named by this authorize
/// request can leave the provider origin. Its request gets no cookies or
/// consent form, and its response cannot add provider cookies.
struct MailboxFlow {
    callback_port: u16,
    callback_path: String,
    state: String,
}

impl MailboxFlow {
    fn new(authorize_url: &str) -> Result<Self, &'static str> {
        if !provider_url(authorize_url)
            || authorize_url.split('?').next() != Some("https://claude.ai/oauth/authorize")
        {
            return Err("this authorize origin needs the manual browser");
        }
        let callback = one_query_param(authorize_url, "redirect_uri")
            .ok_or("the authorize URL has no single local callback")?;
        let (port, path, query) =
            loopback_parts(&callback).ok_or("the authorize callback is not a local endpoint")?;
        if query.is_some() {
            return Err("the authorize callback must name an endpoint without a query");
        }
        Ok(Self {
            callback_port: port,
            callback_path: path.to_owned(),
            state: one_query_param(authorize_url, "state")
                .ok_or("the authorize URL has no single state")?,
        })
    }

    fn is_callback(&self, url: &str) -> bool {
        loopback_parts(url).is_some_and(|(port, path, _)| {
            port == self.callback_port
                && path == self.callback_path
                && one_query_param(url, "state").as_deref() == Some(self.state.as_str())
                && one_query_param(url, "code").is_some()
        })
    }
}

enum StepOutcome {
    CallbackDelivered,
    Page { url: String, body: String },
}

fn follow_until_page_or_callback(
    flow: &MailboxFlow,
    exchange: &mut impl FnMut(
        &str,
        &str,
        &mut BTreeMap<String, String>,
        Option<&str>,
    ) -> Result<HttpResponse, &'static str>,
    initial_method: &str,
    initial_url: &str,
    cookies: &mut BTreeMap<String, String>,
    initial_body: Option<&str>,
) -> Result<StepOutcome, &'static str> {
    let mut method = initial_method.to_owned();
    let mut current_url = initial_url.to_owned();
    let mut body = initial_body.map(str::to_owned);
    for _ in 0..8 {
        if flow.is_callback(&current_url) {
            let cb_resp = exchange("GET", &current_url, &mut BTreeMap::new(), None)?;
            if (200..400).contains(&cb_resp.status) {
                return Ok(StepOutcome::CallbackDelivered);
            }
            return Err("the local OAuth callback listener rejected the authorization code");
        }
        if !provider_url(&current_url) {
            return Err("this OAuth destination needs the manual browser");
        }
        let resp = exchange(&method, &current_url, cookies, body.as_deref())?;
        if matches!(resp.status, 301 | 302 | 303 | 307 | 308) {
            let Some(loc) = resp.location else {
                return Err("redirect response omitted Location header");
            };
            current_url = resolve_relative_url(&current_url, &loc);
            method = "GET".to_owned();
            body = None;
            continue;
        }
        if (200..300).contains(&resp.status) {
            return Ok(StepOutcome::Page {
                url: current_url,
                body: resp.body,
            });
        }
        return Err("the OAuth server returned an error status");
    }
    Err("too many OAuth redirects")
}

fn poll_mailbox_for_magic_link(
    mailbox_dir: &Path,
    target_email: Option<&str>,
    timeout: Duration,
) -> Result<String, &'static str> {
    let deadline = Instant::now() + timeout;
    let mut tried = BTreeSet::new();
    loop {
        if let Ok(entries) = std::fs::read_dir(mailbox_dir) {
            let mut candidates: Vec<(std::time::SystemTime, PathBuf)> = entries
                .filter_map(Result::ok)
                .filter_map(|e| {
                    let path = e.path();
                    let name = e.file_name();
                    let name_str = name.to_string_lossy();
                    if name_str.starts_with('.') || name_str.ends_with(".clicked") {
                        return None;
                    }
                    let clicked_marker = mailbox_dir.join(format!("{name_str}.clicked"));
                    if clicked_marker.exists() || tried.contains(&path) {
                        return None;
                    }
                    let meta = e.metadata().ok()?;
                    if !meta.is_file() {
                        return None;
                    }
                    let mtime = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                    Some((mtime, path))
                })
                .collect();
            candidates.sort_by_key(|a| std::cmp::Reverse(a.0));
            for (_, path) in candidates {
                tried.insert(path.clone());
                let mut raw = String::new();
                let Ok(file) = std::fs::File::open(&path) else {
                    continue;
                };
                if file.take(EMAIL_FILE_MAX).read_to_string(&mut raw).is_err() {
                    continue;
                }
                if let Some(magic_url) = extract_magic_link_from_email(&raw, target_email) {
                    if let Some(name) = path.file_name() {
                        let marker =
                            mailbox_dir.join(format!("{}.clicked", name.to_string_lossy()));
                        let _ = std::fs::write(marker, b"1\n");
                    }
                    return Ok(magic_url);
                }
            }
        }
        if Instant::now() >= deadline {
            return Err("timed out waiting for magic-link email in mailbox");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Complete the OAuth + email magic-link authorization flow end-to-end:
/// 1. Visits `authorize_url` (`GET`), capturing session cookies and checking
///    whether an already-authenticated session redirects straight to `redirect_uri`.
/// 2. Polls `mailbox_dir` for an incoming verification / magic-link email
///    addressed to `target_email` (or `login_hint` from `authorize_url`).
/// 3. Clicks (fetches) the magic link URL from the email.
/// 4. Submits the OAuth consent / approval form (`POST` or `GET`) and delivers
///    the resulting authorization `code` and `state` to `claude`'s localhost
///    callback listener.
///
/// # Errors
///
/// Returns a static diagnostic string (never containing any URL or email
/// address) if any stage of the flow fails or times out.
pub fn authorize_with_mailbox(
    authorize_url: &str,
    mailbox_dir: &Path,
    target_email: Option<&str>,
    timeout: Duration,
) -> Result<(), &'static str> {
    authorize_using(
        authorize_url,
        target_email,
        |email| poll_mailbox_for_magic_link(mailbox_dir, email, timeout),
        &mut http_exchange,
    )
}

/// The real flow with its two effects passed as capabilities. Tests replace
/// both, so exercising the complete admission/redirect/form path reads no
/// mailbox and starts no curl, callback listener or browser.
fn authorize_using(
    authorize_url: &str,
    target_email: Option<&str>,
    magic_link: impl FnOnce(Option<&str>) -> Result<String, &'static str>,
    exchange: &mut impl FnMut(
        &str,
        &str,
        &mut BTreeMap<String, String>,
        Option<&str>,
    ) -> Result<HttpResponse, &'static str>,
) -> Result<(), &'static str> {
    let flow = MailboxFlow::new(authorize_url)?;
    let email = target_email
        .filter(|e| !e.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| extract_login_hint(authorize_url));
    let mut cookies = BTreeMap::new();

    match follow_until_page_or_callback(&flow, exchange, "GET", authorize_url, &mut cookies, None)?
    {
        StepOutcome::CallbackDelivered => return Ok(()),
        StepOutcome::Page { .. } => {}
    }

    let magic_url = magic_link(email.as_deref())?;

    let consent_page = match follow_until_page_or_callback(
        &flow,
        exchange,
        "GET",
        &magic_url,
        &mut cookies,
        None,
    )? {
        StepOutcome::CallbackDelivered => return Ok(()),
        StepOutcome::Page { url, body } => (url, body),
    };

    let (method, action_url, form_body) =
        parse_html_approval_action(&consent_page.1, &consent_page.0)
            .ok_or("magic-link response contained no OAuth approval action")?;

    match follow_until_page_or_callback(
        &flow,
        exchange,
        &method,
        &action_url,
        &mut cookies,
        form_body.as_deref(),
    )? {
        StepOutcome::CallbackDelivered => Ok(()),
        StepOutcome::Page { .. } => Err("OAuth approval did not redirect to callback listener"),
    }
}

/// `aterm-open-url <url>` (the argv0 alias, `crates/aterm`'s router): when
/// [`MAILBOX_ENV`] names a mailbox directory, drives the automated OAuth +
/// email magic-link authorization flow ([`authorize_with_mailbox`]); otherwise
/// opens the URL in [`PROFILE_ENV`]'s Chrome profile ([`open_with`]).
#[must_use]
pub fn main_entry(args: Vec<OsString>) -> ExitCode {
    let args: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    if let [url] = args.as_slice()
        && let Some(mailbox) = std::env::var_os(MAILBOX_ENV).filter(|v| !v.is_empty())
    {
        let email = std::env::var(EMAIL_ENV).ok();
        match authorize_with_mailbox(
            url,
            Path::new(&mailbox),
            email.as_deref(),
            AUTHORIZE_TIMEOUT,
        ) {
            Ok(()) => return ExitCode::from(0),
            Err(why) => {
                eprintln!("{OPENER}: not authorized: {why}");
                return ExitCode::from(1);
            }
        }
    }
    let profile = std::env::var(PROFILE_ENV).ok();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let local_state = || {
        use std::io::Read as _;
        let path = local_state_path(home.as_deref()?);
        let mut text = String::new();
        std::fs::File::open(path)
            .ok()?
            .take(LOCAL_STATE_MAX)
            .read_to_string(&mut text)
            .ok()?;
        Some(text)
    };
    let mut run = |mut cmd: aterm_uds::spawnfd::Command| cmd.status().is_ok_and(|s| s.success());
    let (code, said) = open_with(&args, profile.as_deref(), &local_state, &mut run);
    if let Some(said) = said {
        eprintln!("{said}");
    }
    ExitCode::from(code)
}

#[cfg(test)]
#[path = "open_url_tests.rs"]
mod tests;
