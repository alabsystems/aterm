// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Profile-aware URL opener and headless mailbox OAuth flow for `aterm-open-url`.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use aterm_json::Value;

/// Executable alias name for the URL opener.
pub const OPENER: &str = "aterm-open-url";

/// The variable a renewal's flow carries the profile's display name in.
pub const PROFILE_ENV: &str = "ATERM_OPEN_URL_PROFILE";

/// Environment variable naming the mailbox directory for headless OAuth authorization.
pub const MAILBOX_ENV: &str = "ATERM_OPEN_URL_MAILBOX";

/// Environment variable naming the target account email for headless OAuth authorization.
pub const EMAIL_ENV: &str = "ATERM_OPEN_URL_EMAIL";

/// Default mailbox subdirectory under `<accounts store>` when [`MAILBOX_ENV`] is unset.
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

/// Resolves a Chrome profile directory name by exact display name in `Local State`.
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

/// Builds the `open -na "Google Chrome"` command for a profile directory and URL.
#[must_use]
pub fn open_command(dir: &str, url: &str) -> aterm_uds::spawnfd::Command {
    let mut cmd = aterm_uds::spawnfd::command("open");
    cmd.args(["-na", "Google Chrome", "--args"])
        .arg(format!("--profile-directory={dir}"))
        .arg(url);
    cmd
}

/// Returns a refusal reason if `url` is not a valid `https://` URL without control chars.
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

/// Runs the opener with injected profile, `Local State` reader, and command runner.
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

/// Decodes RFC 2045 Quoted-Printable soft line breaks and `=XX` hex escapes.
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

fn decode_base64_block(input: &str) -> Option<String> {
    let sextet = |b: u8| -> Option<u8> {
        match b {
            b'A'..=b'Z' => Some(b - b'A'),
            b'a'..=b'z' => Some(b - b'a' + 26),
            b'0'..=b'9' => Some(b - b'0' + 52),
            b'+' | b'-' => Some(62),
            b'/' | b'_' => Some(63),
            _ => None,
        }
    };
    let mut out = Vec::new();
    let mut group = [0u8; 4];
    let mut len = 0usize;
    for &b in input.as_bytes() {
        if b.is_ascii_whitespace() {
            continue;
        }
        if b == b'=' {
            break;
        }
        let val = sextet(b)?;
        group[len] = val;
        len += 1;
        if len == 4 {
            out.push((group[0] << 2) | (group[1] >> 4));
            out.push((group[1] << 4) | (group[2] >> 2));
            out.push((group[2] << 6) | group[3]);
            len = 0;
        }
    }
    if len == 2 {
        out.push((group[0] << 2) | (group[1] >> 4));
    } else if len == 3 {
        out.push((group[0] << 2) | (group[1] >> 4));
        out.push((group[1] << 4) | (group[2] >> 2));
    } else if len == 1 {
        return None;
    }
    (!out.is_empty()).then(|| String::from_utf8_lossy(&out).into_owned())
}

fn decode_mime_base64_parts(raw_email: &str) -> String {
    let mut decoded_parts = String::new();
    let mut search = raw_email;
    while let Some(pos) = search
        .to_ascii_lowercase()
        .find("content-transfer-encoding")
    {
        let from_hdr = &search[pos..];
        let line_end = from_hdr.find('\n').unwrap_or(from_hdr.len());
        let hdr_line = &from_hdr[..line_end];
        let is_b64 = hdr_line
            .split_once(':')
            .is_some_and(|(_, v)| v.trim().to_ascii_lowercase().starts_with("base64"));
        let after_line = &from_hdr[line_end..];
        if is_b64
            && let Some((_, body_start)) = from_hdr
                .split_once("\r\n\r\n")
                .or_else(|| from_hdr.split_once("\n\n"))
        {
            let mut body_end = body_start.len();
            let mut offset = 0usize;
            for line in body_start.split_inclusive('\n') {
                if line.starts_with("--") {
                    body_end = offset;
                    break;
                }
                offset += line.len();
            }
            if let Some(decoded) = decode_base64_block(&body_start[..body_end]) {
                decoded_parts.push('\n');
                decoded_parts.push_str(&decoded);
            }
        }
        if after_line.is_empty() {
            break;
        }
        search = &after_line[1..];
    }
    decoded_parts
}

fn is_supported_auth_url(url: &str) -> bool {
    !url.chars().any(|c| c.is_whitespace() || c.is_control())
        && (url.starts_with("https://")
            || url.starts_with("http://127.0.0.1:")
            || url.starts_with("http://localhost:"))
}

fn contains_matching_email(text: &str, target: &str) -> bool {
    text.split(|c: char| {
        c.is_whitespace()
            || matches!(
                c,
                '<' | '>' | ',' | ';' | '"' | '\'' | '(' | ')' | '[' | ']' | ':'
            )
    })
    .filter(|tok| tok.contains('@'))
    .any(|tok| {
        let cleaned = tok.trim_matches('.');
        super::accounts::email_matches(cleaned, target)
    })
}

fn unfold_rfc5322_headers(head: &str) -> Vec<String> {
    let mut unfolded: Vec<String> = Vec::new();
    for line in head.lines() {
        if (line.starts_with(' ') || line.starts_with('\t'))
            && let Some(prev) = unfolded.last_mut()
        {
            prev.push(' ');
            prev.push_str(line.trim());
        } else {
            unfolded.push(line.to_owned());
        }
    }
    unfolded
}

fn json_value_matches_email(val: &Value, target: &str) -> bool {
    match val {
        Value::String(s) => contains_matching_email(s, target),
        Value::Array(items) => items
            .iter()
            .any(|item| json_value_matches_email(item, target)),
        Value::Object(map) => ["email", "address"]
            .into_iter()
            .filter_map(|k| map.get(k))
            .any(|v| json_value_matches_email(v, target)),
        _ => false,
    }
}

fn email_matches_target(raw_email: &str, target: &str) -> bool {
    let target_trimmed = target.trim();
    if target_trimmed.is_empty() {
        return true;
    }
    if let Ok(doc) = aterm_json::from_str::<Value>(raw_email) {
        let mut saw_json_recipient = false;
        for key in ["to", "recipients", "recipient", "delivered_to"] {
            if let Some(val) = doc.get(key) {
                saw_json_recipient = true;
                if json_value_matches_email(val, target_trimmed) {
                    return true;
                }
            }
        }
        if saw_json_recipient {
            return false;
        }
    }
    let head = raw_email
        .split_once("\r\n\r\n")
        .or_else(|| raw_email.split_once("\n\n"))
        .map_or(raw_email, |(h, _)| h);
    let mut saw_recipient_header = false;
    for line in unfold_rfc5322_headers(head) {
        if let Some((k, v)) = line.split_once(':') {
            let key = k.trim();
            if key.eq_ignore_ascii_case("to")
                || key.eq_ignore_ascii_case("delivered-to")
                || key.eq_ignore_ascii_case("x-original-to")
                || key.eq_ignore_ascii_case("envelope-to")
            {
                saw_recipient_header = true;
                if contains_matching_email(v, target_trimmed) {
                    return true;
                }
            }
        }
    }
    if saw_recipient_header {
        return false;
    }
    contains_matching_email(raw_email, target_trimmed)
}

fn is_non_auth_link(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    let path = lower.split('?').next().unwrap_or(&lower);
    lower.contains("unsubscribe")
        || lower.contains("email-preferences")
        || path.ends_with(".png")
        || path.ends_with(".jpg")
        || path.ends_with(".jpeg")
        || path.ends_with(".gif")
        || path.ends_with(".svg")
        || path.ends_with(".ico")
        || path.ends_with(".css")
        || path.ends_with(".js")
}

/// Extracts a verification or magic-link URL from an incoming email addressed to `target_email`.
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
    let mut extra_text = decode_mime_base64_parts(raw_email);
    if let Ok(doc) = aterm_json::from_str::<Value>(raw_email) {
        for key in ["magic_url", "verify_url", "login_url", "url"] {
            if let Some(u) = doc.get(key).and_then(Value::as_str)
                && is_supported_auth_url(u)
                && !is_non_auth_link(u)
            {
                return Some(u.to_owned());
            }
        }
        for key in ["body", "html", "text", "raw"] {
            if let Some(s) = doc.get(key).and_then(Value::as_str) {
                extra_text.push('\n');
                extra_text.push_str(s);
                extra_text.push_str(&decode_mime_base64_parts(s));
            }
        }
    }
    let qp_decoded = decode_quoted_printable(raw_email);
    let combined = if extra_text.is_empty() {
        qp_decoded
    } else {
        format!("{qp_decoded}\n{extra_text}")
    };
    let decoded = decode_html_amp(&combined);
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
            if is_supported_auth_url(candidate)
                && !is_non_auth_link(candidate)
                && !urls.iter().any(|u| u == candidate)
            {
                urls.push(candidate.to_owned());
            }
            rest = &after[end..];
        }
    }
    let looks_like_auth = |u: &str| {
        let lower = u.to_ascii_lowercase();
        lower.contains("magic")
            || lower.contains("verify")
            || lower.contains("login")
            || lower.contains("token=")
            || lower.contains("auth")
            || lower.contains("callback")
    };
    urls.iter()
        .find(|u| provider_url(u) && looks_like_auth(u))
        .or_else(|| urls.iter().find(|u| looks_like_auth(u)))
        .or_else(|| urls.iter().find(|u| provider_url(u)))
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

/// Parses an OAuth consent page into `(HTTP method, resolved action URL, optional form body)`.
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
pub(super) struct HttpResponse {
    pub(super) status: u16,
    pub(super) location: Option<String>,
    pub(super) body: String,
}

pub(super) fn parse_raw_http_response(
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
        if proto.starts_with("HTTP/")
            && (status == 100
                || (status == 200
                    && first_line
                        .to_ascii_lowercase()
                        .contains("connection established")))
            && rest.starts_with("HTTP/")
        {
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
        cmd.args(["--data-raw", body]);
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
/// the Claude origin the existing opener contract names.
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
/// request can leave the provider origin.
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

pub(super) fn poll_mailbox_for_magic_link(
    mailbox_dir: &Path,
    target_email: Option<&str>,
    timeout: Duration,
) -> Result<String, &'static str> {
    let deadline = Instant::now() + timeout;
    let mut tried = BTreeSet::new();
    loop {
        if let Ok(entries) = std::fs::read_dir(mailbox_dir) {
            let mut candidates: Vec<(std::time::SystemTime, u64, PathBuf)> = entries
                .filter_map(Result::ok)
                .filter_map(|e| {
                    let path = e.path();
                    let name = e.file_name();
                    let name_str = name.to_string_lossy();
                    if name_str.starts_with('.') || name_str.ends_with(".clicked") {
                        return None;
                    }
                    let clicked_marker = mailbox_dir.join(format!("{name_str}.clicked"));
                    if clicked_marker.exists() {
                        return None;
                    }
                    let meta = e.metadata().ok()?;
                    if !meta.is_file() || meta.len() == 0 {
                        return None;
                    }
                    let mtime = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                    if tried.contains(&(path.clone(), meta.len(), mtime)) {
                        return None;
                    }
                    Some((mtime, meta.len(), path))
                })
                .collect();
            candidates.sort_by_key(|a| std::cmp::Reverse(a.0));
            for (mtime, len, path) in candidates {
                tried.insert((path.clone(), len, mtime));
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

/// Completes the OAuth and email magic-link authorization flow end-to-end.
///
/// # Errors
///
/// Returns a static diagnostic string (without URLs or emails) on failure or timeout.
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

/// The real flow with its two effects passed as capabilities.
pub(super) fn authorize_using(
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

    let mut page = match follow_until_page_or_callback(
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

    for step in 0..4 {
        let Some((method, action_url, form_body)) = parse_html_approval_action(&page.1, &page.0)
        else {
            return Err(if step == 0 {
                "magic-link response contained no OAuth approval action"
            } else {
                "OAuth approval did not redirect to callback listener"
            });
        };
        match follow_until_page_or_callback(
            &flow,
            exchange,
            &method,
            &action_url,
            &mut cookies,
            form_body.as_deref(),
        )? {
            StepOutcome::CallbackDelivered => return Ok(()),
            StepOutcome::Page { url, body } => {
                page = (url, body);
            }
        }
    }
    Err("OAuth approval did not redirect to callback listener")
}

fn discover_mailbox_dir() -> Option<PathBuf> {
    if let Some(mailbox) = std::env::var_os(MAILBOX_ENV).filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(mailbox));
    }
    if std::env::var_os(PROFILE_ENV).is_some_and(|v| !v.is_empty()) {
        return None;
    }
    if let Some(arg0) = std::env::args_os().next()
        && let Some(store_dir) = Path::new(&arg0).parent().and_then(Path::parent)
    {
        let store_mailbox = store_dir.join(DEFAULT_MAILBOX_DIR);
        if store_mailbox.is_dir() {
            return Some(store_mailbox);
        }
    }
    None
}

/// Entry point for the `aterm-open-url` argv0 alias.
#[must_use]
pub fn main_entry(args: Vec<OsString>) -> ExitCode {
    let args: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let profile = std::env::var(PROFILE_ENV).ok();
    if let [url] = args.as_slice()
        && let Some(mailbox) = discover_mailbox_dir()
    {
        let email = std::env::var(EMAIL_ENV).ok();
        match authorize_with_mailbox(url, &mailbox, email.as_deref(), AUTHORIZE_TIMEOUT) {
            Ok(()) => return ExitCode::from(0),
            Err(why) => {
                if profile.as_deref().is_none_or(|p| p.trim().is_empty()) {
                    eprintln!("{OPENER}: not authorized: {why}");
                    return ExitCode::from(1);
                }
            }
        }
    }
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
