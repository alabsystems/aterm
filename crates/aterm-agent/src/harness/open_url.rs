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
pub(super) const EMAIL_FILE_MAX: u64 = 512 * 1024;

/// Default timeout for headless OAuth + email magic-link authorization.
pub const AUTHORIZE_TIMEOUT: Duration = Duration::from_secs(90);

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
    let mut cmd = aterm_uds::spawnfd::command(aterm_uds::os_path!("open"));
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
            && percent_decode(k) == key
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

pub(super) fn split_header_body(s: &str) -> Option<(&str, &str)> {
    match (s.find("\r\n\r\n"), s.find("\n\n")) {
        (Some(crlf), Some(lf)) if crlf <= lf => Some((&s[..crlf], &s[crlf + 4..])),
        (Some(_), Some(lf)) => Some((&s[..lf], &s[lf + 2..])),
        (Some(crlf), None) => Some((&s[..crlf], &s[crlf + 4..])),
        (None, Some(lf)) => Some((&s[..lf], &s[lf + 2..])),
        (None, None) => None,
    }
}

pub(super) fn is_mime_boundary_line(line: &str) -> bool {
    let trimmed = line.trim_end_matches(['\r', '\n', ' ', '\t']);
    let Some(after_dashes) = trimmed.strip_prefix("--") else {
        return false;
    };
    if after_dashes.is_empty() || after_dashes.len() > 72 || after_dashes.bytes().all(|b| b == b'-')
    {
        return false;
    }
    after_dashes.bytes().all(|b| {
        b.is_ascii_alphanumeric()
            || matches!(
                b,
                b'\'' | b'(' | b')' | b'+' | b'_' | b',' | b'-' | b'.' | b'/' | b'=' | b'?'
            )
    })
}

pub(super) fn is_top_level_base64_email(raw_email: &str) -> bool {
    let Some((head, _)) = split_header_body(raw_email) else {
        return false;
    };
    for line in unfold_rfc5322_headers(head) {
        if let Some((k, v)) = line.split_once(':')
            && k.trim().eq_ignore_ascii_case("content-transfer-encoding")
        {
            return v
                .trim()
                .trim_matches(['"', '\''])
                .to_ascii_lowercase()
                .starts_with("base64");
        }
    }
    false
}

fn part_opening_boundary(before_header: &str) -> Option<&str> {
    for line in before_header.lines().rev() {
        let trimmed = line.trim_end_matches(['\r', '\n', ' ', '\t']);
        if trimmed.is_empty() {
            return None;
        }
        if is_mime_boundary_line(trimmed) {
            return Some(trimmed);
        }
    }
    None
}

fn is_part_end_boundary(line: &str, open_boundary: Option<&str>) -> bool {
    let trimmed = line.trim_end_matches(['\r', '\n', ' ', '\t']);
    if let Some(bound) = open_boundary {
        trimmed == bound || trimmed.strip_prefix(bound) == Some("--")
    } else {
        is_mime_boundary_line(trimmed)
    }
}

pub(super) fn is_quoted_printable_email(raw_email: &str) -> bool {
    let (head, body) = split_header_body(raw_email).unwrap_or(("", raw_email));
    for line in unfold_rfc5322_headers(head) {
        if let Some((k, v)) = line.split_once(':')
            && k.trim().eq_ignore_ascii_case("content-transfer-encoding")
        {
            return v
                .trim()
                .trim_matches('"')
                .to_ascii_lowercase()
                .starts_with("quoted-printable");
        }
    }
    if find_ascii_ci(body, "content-transfer-encoding").is_some() {
        return false;
    }
    if body.contains("=\r\n")
        || find_ascii_ci(body, "=3d\"").is_some()
        || find_ascii_ci(body, "=3d'").is_some()
    {
        return true;
    }
    if !head.is_empty() || find_ascii_ci(body, "=3d").is_none() {
        return false;
    }
    let bytes = body.as_bytes();
    let mut i = 0usize;
    let mut saw_qp_eq = false;
    while i < bytes.len() {
        if bytes[i] != b'=' {
            i += 1;
            continue;
        }
        if bytes.get(i + 1..i + 3) == Some(b"\r\n") {
            i += 3;
            continue;
        }
        if bytes.get(i + 1) == Some(&b'\n') {
            i += 2;
            continue;
        }
        let Some(&[hi, lo]) = bytes.get(i + 1..i + 3) else {
            return false;
        };
        if !hi.is_ascii_hexdigit() || !lo.is_ascii_hexdigit() {
            return false;
        }
        if hi == b'3' && matches!(lo, b'D' | b'd') {
            let hex_run = bytes[i + 1..]
                .iter()
                .take_while(|b| b.is_ascii_hexdigit())
                .count();
            let double_qp = bytes
                .get(i + 3..i + 5)
                .is_some_and(|pair| pair.eq_ignore_ascii_case(b"3d"));
            if double_qp || hex_run < 6 {
                saw_qp_eq = true;
            }
        }
        i += 3;
    }
    saw_qp_eq
}

pub(super) fn decode_html_amp(input: &str) -> String {
    if !input.contains('&') {
        return input.to_owned();
    }
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after_amp = &rest[amp + 1..];
        let mut decoded_char = None;
        let mut consumed = 0usize;
        let scan_len = after_amp.len().min(12);
        if let Some(semi) = after_amp.as_bytes()[..scan_len]
            .iter()
            .position(|&b| b == b';')
        {
            let entity = &after_amp[..semi];
            if entity.eq_ignore_ascii_case("amp") {
                decoded_char = Some('&');
            } else if entity.eq_ignore_ascii_case("lt") {
                decoded_char = Some('<');
            } else if entity.eq_ignore_ascii_case("gt") {
                decoded_char = Some('>');
            } else if entity.eq_ignore_ascii_case("quot") {
                decoded_char = Some('"');
            } else if entity.eq_ignore_ascii_case("apos") {
                decoded_char = Some('\'');
            } else if entity.eq_ignore_ascii_case("nbsp") {
                decoded_char = Some(' ');
            } else if let Some(num) = entity.strip_prefix('#') {
                let code =
                    if let Some(hex) = num.strip_prefix('x').or_else(|| num.strip_prefix('X')) {
                        u32::from_str_radix(hex, 16).ok()
                    } else {
                        num.parse::<u32>().ok()
                    };
                if let Some(c) = code {
                    if c == 160 {
                        decoded_char = Some(' ');
                    } else if (0x20..=0x7E).contains(&c) {
                        decoded_char = Some((c as u8) as char);
                    }
                }
            }
            if decoded_char.is_some() {
                consumed = semi + 1;
            }
        }
        if let Some(ch) = decoded_char {
            out.push(ch);
            rest = &after_amp[consumed..];
        } else {
            out.push('&');
            rest = after_amp;
        }
    }
    out.push_str(rest);
    out
}

pub(super) fn strip_html_comments(input: &str) -> String {
    if !input.contains("<!--") {
        return input.to_owned();
    }
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        let after_open = &rest[start + 4..];
        if let Some(end) = after_open.find("-->") {
            rest = &after_open[end + 3..];
        } else {
            rest = "";
            break;
        }
    }
    out.push_str(rest);
    out
}

pub(super) fn find_tag_close(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    if bytes.first() != Some(&b'<') {
        return s.find('>');
    }
    let mut pos = 1usize;
    while pos < bytes.len()
        && !bytes[pos].is_ascii_whitespace()
        && !matches!(bytes[pos], b'>' | b'/')
    {
        pos += 1;
    }
    while pos < bytes.len() {
        while pos < bytes.len() && (bytes[pos].is_ascii_whitespace() || bytes[pos] == b'/') {
            pos += 1;
        }
        if pos >= bytes.len() {
            break;
        }
        if bytes[pos] == b'>' {
            return Some(pos);
        }
        let start = pos;
        while pos < bytes.len()
            && !bytes[pos].is_ascii_whitespace()
            && !matches!(bytes[pos], b'=' | b'>' | b'/')
        {
            pos += 1;
        }
        if start == pos {
            pos += 1;
            continue;
        }
        while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }
        if bytes.get(pos) == Some(&b'=') {
            pos += 1;
            while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
                pos += 1;
            }
            match bytes.get(pos).copied() {
                Some(q @ (b'"' | b'\'')) => {
                    let Some(rel_end) = bytes[pos + 1..].iter().position(|&b| b == q) else {
                        break;
                    };
                    pos = pos + 1 + rel_end + 1;
                }
                Some(_) => {
                    while pos < bytes.len()
                        && !bytes[pos].is_ascii_whitespace()
                        && bytes[pos] != b'>'
                    {
                        pos += 1;
                    }
                }
                None => break,
            }
        }
    }
    s.find('>')
}

fn find_ascii_ci(haystack: &str, needle: &str) -> Option<usize> {
    let h = haystack.as_bytes();
    let n = needle.as_bytes();
    if n.is_empty() {
        return Some(0);
    }
    h.windows(n.len()).position(|w| w.eq_ignore_ascii_case(n))
}

pub(super) fn extract_iframe_srcdoc_fragments(input: &str) -> String {
    let uncommented = strip_html_comments(input);
    let input = uncommented.as_str();
    let lower = input.to_ascii_lowercase();
    let mut fragments = Vec::new();
    let mut search_from = 0usize;
    while let Some(rel) = lower[search_from..].find("srcdoc") {
        let after_name = search_from + rel + "srcdoc".len();
        let rest = &input[after_name..];
        let trimmed = rest.trim_start_matches([' ', '\t', '\r', '\n']);
        let Some(after_eq) = trimmed.strip_prefix('=') else {
            search_from = after_name;
            continue;
        };
        let after_eq = after_eq.trim_start_matches([' ', '\t', '\r', '\n']);
        let Some(quote) = after_eq.chars().next().filter(|&c| c == '"' || c == '\'') else {
            search_from = after_name;
            continue;
        };
        let value_start = &after_eq[quote.len_utf8()..];
        let Some(end_idx) = value_start.find(quote) else {
            break;
        };
        let raw_srcdoc = &value_start[..end_idx];
        let unescaped = decode_html_amp(&decode_html_amp(raw_srcdoc));
        fragments.push(strip_html_comments(&unescaped));
        let consumed = input.len() - value_start.len() + end_idx + quote.len_utf8();
        search_from = consumed;
    }
    fragments.reverse();
    fragments.join("\n")
}

fn decode_base64_bytes(input: &str) -> Option<Vec<u8>> {
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
    (!out.is_empty()).then_some(out)
}

fn decode_base64_block(input: &str) -> Option<String> {
    decode_base64_bytes(input).map(|out| String::from_utf8_lossy(&out).into_owned())
}

pub(super) fn encode_base64(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        out.push(TABLE[(b0 >> 2) as usize] as char);
        out.push(TABLE[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(b2 & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

fn decode_rfc2047_word(word: &str) -> Option<(String, &str)> {
    let after_start = word.strip_prefix("=?")?;
    let (_charset, after_cs) = after_start.split_once('?')?;
    let (enc, after_enc) = after_cs.split_once('?')?;
    let (payload, tail) = after_enc.split_once("?=")?;
    let decoded = if enc.eq_ignore_ascii_case("b") {
        decode_base64_block(payload)?
    } else if enc.eq_ignore_ascii_case("q") {
        decode_quoted_printable(&payload.replace('_', " "))
    } else {
        return None;
    };
    Some((decoded, tail))
}

fn is_encoded_word_separator(mut text: &str) -> bool {
    while !text.is_empty() {
        text = text.trim_start_matches([' ', '\t']);
        if text.is_empty() {
            return true;
        }
        // Accept LF-only folding as the header reader does, but require folding whitespace.
        let Some(tail) = text
            .strip_prefix("\r\n")
            .or_else(|| text.strip_prefix('\n'))
        else {
            return false;
        };
        if !tail.starts_with(' ') && !tail.starts_with('\t') {
            return false;
        }
        text = tail;
    }
    true
}

pub(super) fn decode_rfc2047_words(input: &str) -> String {
    if !input.contains("=?") {
        return input.to_owned();
    }
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    let mut previous_decoded = false;
    while let Some(start) = rest.find("=?") {
        if let Some((decoded, tail)) = decode_rfc2047_word(&rest[start..]) {
            let between = &rest[..start];
            if !previous_decoded || !is_encoded_word_separator(between) {
                out.push_str(between);
            }
            out.push_str(&decoded);
            rest = tail;
            previous_decoded = true;
        } else {
            out.push_str(&rest[..start + 2]);
            rest = &rest[start + 2..];
            previous_decoded = false;
        }
    }
    out.push_str(rest);
    out
}

pub(super) fn extract_rfc5322_subject(raw_email: &str) -> Option<String> {
    let (head, _) = split_header_body(raw_email)?;
    for line in unfold_rfc5322_headers(head) {
        if let Some((k, v)) = line.split_once(':')
            && k.trim().eq_ignore_ascii_case("subject")
        {
            let decoded = decode_rfc2047_words(v.trim());
            if !decoded.trim().is_empty() {
                return Some(decoded);
            }
        }
    }
    None
}

pub(super) fn decode_mime_base64_parts(raw_email: &str) -> String {
    let mut decoded_parts = String::new();
    let mut search_from = 0usize;
    while let Some(rel) = find_ascii_ci(&raw_email[search_from..], "content-transfer-encoding") {
        let pos = search_from + rel;
        let from_hdr = &raw_email[pos..];
        let line_end = from_hdr.find('\n').unwrap_or(from_hdr.len());
        let hdr_line = &from_hdr[..line_end];
        let after_line = &from_hdr[line_end..];
        let valid_line_start = pos == 0 || matches!(raw_email.as_bytes()[pos - 1], b'\n' | b'\r');
        let (hdr_name, first_val) = hdr_line.split_once(':').unwrap_or(("", ""));
        if !valid_line_start
            || !hdr_name
                .trim()
                .eq_ignore_ascii_case("content-transfer-encoding")
        {
            if after_line.is_empty() {
                break;
            }
            search_from = pos + line_end + 1;
            continue;
        }
        let mut encoding_val = first_val.trim();
        if encoding_val.is_empty()
            && let Some(cont) = after_line.strip_prefix('\n')
            && cont.starts_with([' ', '\t'])
        {
            let cont_end = cont.find('\n').unwrap_or(cont.len());
            encoding_val = cont[..cont_end].trim();
        }
        let is_b64 = encoding_val
            .trim_matches('"')
            .to_ascii_lowercase()
            .starts_with("base64");
        let open_bound = part_opening_boundary(&raw_email[..pos]);
        if is_b64
            && let Some((hdr_rest, body_start)) = split_header_body(from_hdr)
            && !hdr_rest
                .lines()
                .any(|l| is_part_end_boundary(l, open_bound))
        {
            let mut body_end = body_start.len();
            let mut offset = 0usize;
            for line in body_start.split_inclusive('\n') {
                if is_part_end_boundary(line, open_bound) {
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
        search_from = pos + line_end + 1;
    }
    decoded_parts
}

pub(super) fn decode_qp_mime_parts(input: &str) -> String {
    if find_ascii_ci(input, "quoted-printable").is_none() {
        return input.to_owned();
    }
    let mut out = String::with_capacity(input.len());
    let mut cursor = 0usize;
    let mut search_from = 0usize;
    while let Some(rel) = find_ascii_ci(&input[search_from..], "content-transfer-encoding") {
        let pos = search_from + rel;
        let from_hdr = &input[pos..];
        let line_end = from_hdr.find('\n').unwrap_or(from_hdr.len());
        let hdr_line = &from_hdr[..line_end];
        let after_line = &from_hdr[line_end..];
        let valid_line_start = pos == 0 || matches!(input.as_bytes()[pos - 1], b'\n' | b'\r');
        let (hdr_name, first_val) = hdr_line.split_once(':').unwrap_or(("", ""));
        if !valid_line_start
            || !hdr_name
                .trim()
                .eq_ignore_ascii_case("content-transfer-encoding")
        {
            if after_line.is_empty() {
                break;
            }
            search_from = pos + line_end + 1;
            continue;
        }
        let mut encoding_val = first_val.trim();
        if encoding_val.is_empty()
            && let Some(cont) = after_line.strip_prefix('\n')
            && cont.starts_with([' ', '\t'])
        {
            let cont_end = cont.find('\n').unwrap_or(cont.len());
            encoding_val = cont[..cont_end].trim();
        }
        let is_qp = encoding_val
            .trim_matches('"')
            .to_ascii_lowercase()
            .starts_with("quoted-printable");
        let open_bound = part_opening_boundary(&input[..pos]);
        if is_qp
            && let Some((hdr_rest, body_start)) = split_header_body(from_hdr)
            && !hdr_rest
                .lines()
                .any(|l| is_part_end_boundary(l, open_bound))
        {
            let body_offset = pos + (from_hdr.len() - body_start.len());
            let mut body_end = body_start.len();
            let mut offset = 0usize;
            for line in body_start.split_inclusive('\n') {
                if is_part_end_boundary(line, open_bound) {
                    body_end = offset;
                    break;
                }
                offset += line.len();
            }
            out.push_str(&input[cursor..body_offset]);
            out.push_str(&decode_quoted_printable(&body_start[..body_end]));
            cursor = body_offset + body_end;
            search_from = cursor;
            continue;
        }
        if after_line.is_empty() {
            break;
        }
        search_from = pos + line_end + 1;
    }
    out.push_str(&input[cursor..]);
    out
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
        Value::Object(map) => {
            let mut mailbox_part: Option<&str> = None;
            let mut domain_part: Option<&str> = None;
            for (k, v) in map {
                if let Some(s) = v.as_str() {
                    match k.to_ascii_lowercase().as_str() {
                        "mailbox" | "local" | "localpart" | "local_part" | "user" => {
                            mailbox_part = Some(s.trim());
                        }
                        "domain" | "host" => {
                            domain_part = Some(s.trim());
                        }
                        _ => {}
                    }
                }
            }
            if let (Some(mb), Some(dom)) = (mailbox_part, domain_part)
                && !mb.is_empty()
                && !dom.is_empty()
                && contains_matching_email(&format!("{mb}@{dom}"), target)
            {
                return true;
            }
            map.iter().any(|(k, v)| {
                matches!(
                    k.to_ascii_lowercase().as_str(),
                    "email"
                        | "address"
                        | "emailaddress"
                        | "email_address"
                        | "to"
                        | "tofull"
                        | "to_full"
                        | "torecipients"
                        | "to_recipients"
                        | "recipients"
                        | "recipient"
                        | "delivered_to"
                        | "delivered-to"
                        | "x-original-to"
                        | "envelope-to"
                        | "targets"
                        | "value"
                ) && json_value_matches_email(v, target)
            })
        }
        _ => false,
    }
}

fn is_json_recipient_key(key_lower: &str) -> bool {
    matches!(
        key_lower,
        "to" | "tofull"
            | "to_full"
            | "torecipients"
            | "to_recipients"
            | "recipients"
            | "recipient"
            | "delivered_to"
            | "delivered-to"
            | "x-delivered-to"
            | "x_delivered_to"
            | "x-original-to"
            | "x_original_to"
            | "original-recipient"
            | "original_recipient"
            | "envelope-to"
            | "envelope_to"
            | "x-envelope-to"
            | "x_envelope_to"
            | "x-forwarded-to"
            | "x_forwarded_to"
            | "resent-to"
            | "resent_to"
            | "cc"
            | "ccfull"
            | "cc_full"
            | "ccrecipients"
            | "cc_recipients"
            | "bcc"
            | "bccfull"
            | "bcc_full"
            | "bccrecipients"
            | "bcc_recipients"
            | "rcpt_to"
            | "rcpt-to"
    )
}

fn json_value_contains_email(val: &Value) -> bool {
    match val {
        Value::String(s) => s.contains('@'),
        Value::Array(arr) => arr.iter().any(json_value_contains_email),
        Value::Object(map) => {
            let has_split = map.keys().any(|k| {
                matches!(
                    k.to_ascii_lowercase().as_str(),
                    "mailbox" | "local" | "localpart" | "local_part" | "user"
                )
            }) && map
                .keys()
                .any(|k| matches!(k.to_ascii_lowercase().as_str(), "domain" | "host"));
            has_split || map.values().any(json_value_contains_email)
        }
        _ => false,
    }
}

fn json_value_is_nonempty(val: &Value) -> bool {
    match val {
        Value::Null => false,
        Value::String(s) => !s.trim().is_empty() && json_value_contains_email(val),
        Value::Array(arr) => !arr.is_empty() && json_value_contains_email(val),
        Value::Object(map) => !map.is_empty() && json_value_contains_email(val),
        _ => false,
    }
}

pub(super) fn check_json_recipients(
    obj: &aterm_json::Map,
    target: &str,
    depth: usize,
) -> (bool, bool) {
    if depth > 6 {
        return (false, false);
    }
    let mut saw_recipient = false;
    let mut matched = false;
    for (k, val) in obj {
        let key = k.to_ascii_lowercase();
        if is_json_recipient_key(&key) && json_value_is_nonempty(val) {
            saw_recipient = true;
            if json_value_matches_email(val, target) {
                matched = true;
            }
        } else if matches!(key.as_str(), "headers" | "header") {
            if let Some(hdr_map) = val.as_object() {
                let (s, m) = check_json_recipients(hdr_map, target, depth + 1);
                saw_recipient |= s;
                matched |= m;
            } else if let Some(hdr_arr) = val.as_array() {
                for item in hdr_arr {
                    let Some(item_obj) = item.as_object() else {
                        continue;
                    };
                    let hdr_name = item_obj
                        .iter()
                        .find_map(|(hk, hv)| {
                            matches!(hk.to_ascii_lowercase().as_str(), "name" | "key" | "header")
                                .then(|| hv.as_str())
                                .flatten()
                        })
                        .unwrap_or("");
                    if is_json_recipient_key(&hdr_name.trim().to_ascii_lowercase())
                        && let Some((_, hv)) = item_obj.iter().find(|(hk, _)| {
                            matches!(hk.to_ascii_lowercase().as_str(), "value" | "values" | "val")
                        })
                        && json_value_is_nonempty(hv)
                    {
                        saw_recipient = true;
                        if json_value_matches_email(hv, target) {
                            matched = true;
                        }
                    }
                }
            }
        } else if matches!(
            key.as_str(),
            "envelope" | "content" | "payload" | "data" | "message" | "mail"
        ) && let Some(sub_obj) = val.as_object()
        {
            let (s, m) = check_json_recipients(sub_obj, target, depth + 1);
            saw_recipient |= s;
            matched |= m;
        }
    }
    (saw_recipient, matched)
}

pub(super) fn email_matches_target(raw_email: &str, target: &str) -> bool {
    let target_trimmed = target.trim();
    if target_trimmed.is_empty() {
        return true;
    }
    if let Ok(doc) = aterm_json::from_str::<Value>(raw_email)
        && let Some(obj) = doc.as_object()
    {
        let (saw_json_recipient, matched) = check_json_recipients(obj, target_trimmed, 0);
        if matched {
            return true;
        }
        if saw_json_recipient {
            return false;
        }
    }
    let head = split_header_body(raw_email).map_or(raw_email, |(h, _)| h);
    let mut saw_recipient_header = false;
    for line in unfold_rfc5322_headers(head) {
        if let Some((k, v)) = line.split_once(':') {
            let key = k.trim().to_ascii_lowercase();
            if is_json_recipient_key(&key) {
                let decoded_v = decode_rfc2047_words(v);
                if decoded_v.contains('@') {
                    saw_recipient_header = true;
                }
                if contains_matching_email(&decoded_v, target_trimmed) {
                    return true;
                }
            } else if key == "received" && v.to_ascii_lowercase().contains("for ") {
                let decoded_v = decode_rfc2047_words(v);
                if contains_matching_email(&decoded_v, target_trimmed) {
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
    let (without_query, has_query) = lower
        .split_once('?')
        .map_or((lower.as_str(), false), |(p, q)| (p, !q.is_empty()));
    if has_negative_url_keyword(&lower)
        || lower.contains("email-preferences")
        || [
            ".png", ".jpg", ".jpeg", ".gif", ".svg", ".ico", ".css", ".js", ".woff", ".woff2",
            ".ttf", ".eot", ".otf", ".webp", ".avif",
        ]
        .iter()
        .any(|ext| without_query.ends_with(ext))
    {
        return true;
    }
    let after_scheme = without_query
        .strip_prefix("https://")
        .or_else(|| without_query.strip_prefix("http://"))
        .unwrap_or(without_query);
    let (host_port, raw_path) = after_scheme.split_once('/').unwrap_or((after_scheme, ""));
    let host = host_port.split(':').next().unwrap_or(host_port);
    if matches!(
        host,
        "fonts.googleapis.com"
            | "fonts.gstatic.com"
            | "www.w3.org"
            | "w3.org"
            | "schema.org"
            | "anthropic.com"
            | "www.anthropic.com"
            | "support.anthropic.com"
            | "status.anthropic.com"
            | "docs.anthropic.com"
    ) {
        return true;
    }
    let trimmed_path = raw_path.trim_matches('/');
    if !has_query
        && (trimmed_path.is_empty()
            || matches!(
                trimmed_path,
                "login"
                    | "signin"
                    | "sign-in"
                    | "auth"
                    | "oauth"
                    | "welcome"
                    | "home"
                    | "app"
                    | "settings"
                    | "account"
                    | "chats"
                    | "new"
            ))
    {
        return true;
    }
    let first_seg = trimmed_path.split('/').next().unwrap_or("");
    matches!(
        first_seg,
        "privacy"
            | "privacy-policy"
            | "terms"
            | "terms-of-service"
            | "legal"
            | "support"
            | "help"
            | "faq"
            | "docs"
            | "status"
            | "security"
            | "about"
            | "careers"
            | "pricing"
            | "blog"
            | "contact"
            | "download"
            | "downloads"
            | "product"
            | "news"
            | "research"
            | "company"
            | "customers"
    )
}

fn json_headers_transfer_encoding(obj: &aterm_json::Map) -> (bool, bool) {
    let mut is_b64 = false;
    let mut is_qp = false;
    let mut inspect_str = |s: &str| {
        let lower = s.trim().to_ascii_lowercase();
        if lower.starts_with("base64") {
            is_b64 = true;
        } else if lower.contains("quoted-printable") {
            is_qp = true;
        }
    };
    let mut inspect_val = |v: &Value| match v {
        Value::String(s) => inspect_str(s),
        Value::Array(arr) => {
            for item in arr {
                if let Some(s) = item.as_str() {
                    inspect_str(s);
                }
            }
        }
        _ => {}
    };
    for (k, v) in obj {
        let key = k.to_ascii_lowercase();
        if matches!(
            key.as_str(),
            "content-transfer-encoding" | "content_transfer_encoding" | "encoding"
        ) {
            inspect_val(v);
        } else if matches!(key.as_str(), "headers" | "header") {
            if let Some(hdr_map) = v.as_object() {
                for (hk, hv) in hdr_map {
                    if hk.eq_ignore_ascii_case("content-transfer-encoding") {
                        inspect_val(hv);
                    }
                }
            } else if let Some(hdr_arr) = v.as_array() {
                for item in hdr_arr {
                    if let Some(item_obj) = item.as_object() {
                        let is_cte = item_obj.iter().any(|(hk, hv)| {
                            matches!(hk.to_ascii_lowercase().as_str(), "name" | "key")
                                && hv.as_str().is_some_and(|s| {
                                    s.trim().eq_ignore_ascii_case("content-transfer-encoding")
                                })
                        });
                        if is_cte {
                            for (hk, hv) in item_obj {
                                if matches!(hk.to_ascii_lowercase().as_str(), "value" | "values") {
                                    inspect_val(hv);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    (is_b64, is_qp)
}

fn append_decoded_json_body_str(s: &str, is_b64: bool, is_qp: bool, extra_text: &mut String) {
    let rfc2047_cow = decode_rfc2047_words(s);
    let s = rfc2047_cow.as_str();
    if is_b64 && let Some(decoded) = decode_base64_block(s) {
        extra_text.push('\n');
        if is_quoted_printable_email(&decoded) {
            extra_text.push_str(&decode_quoted_printable(&decoded));
        } else {
            extra_text.push_str(&decode_qp_mime_parts(&decoded));
        }
        return;
    }
    extra_text.push('\n');
    if is_qp || is_quoted_printable_email(s) {
        let decoded = decode_quoted_printable(s);
        extra_text.push_str(&decoded);
        extra_text.push_str(&decode_mime_base64_parts(&decoded));
    } else {
        let decoded = decode_qp_mime_parts(s);
        extra_text.push_str(&decoded);
        extra_text.push_str(&decode_mime_base64_parts(&decoded));
    }
}

pub(super) fn extract_json_email_fields(
    doc: &Value,
    explicit_urls: &mut Vec<String>,
    explicit_otps: &mut Vec<String>,
    extra_text: &mut String,
) {
    extract_json_email_fields_inner(doc, explicit_urls, explicit_otps, extra_text, 0);
}

fn extract_json_email_fields_inner(
    val: &Value,
    explicit_urls: &mut Vec<String>,
    explicit_otps: &mut Vec<String>,
    extra_text: &mut String,
    depth: usize,
) {
    if depth > 8 {
        return;
    }
    match val {
        Value::Array(items) => {
            for item in items {
                extract_json_email_fields_inner(
                    item,
                    explicit_urls,
                    explicit_otps,
                    extra_text,
                    depth + 1,
                );
            }
        }
        Value::Object(obj) => {
            let (is_b64, is_qp) = json_headers_transfer_encoding(obj);
            for (k, v) in obj {
                let key = k.to_ascii_lowercase();
                if matches!(
                    key.as_str(),
                    "magic_url"
                        | "verify_url"
                        | "login_url"
                        | "url"
                        | "confirmation_url"
                        | "action_url"
                ) && let Some(u) = v.as_str()
                    && is_supported_auth_url(u)
                    && !is_non_auth_link(u)
                    && (key != "url" || !is_json_message_self_url(u))
                    && !explicit_urls.iter().any(|existing| existing == u)
                {
                    explicit_urls.push(u.to_owned());
                }
                if matches!(
                    key.as_str(),
                    "otp" | "code" | "verification_code" | "login_code"
                ) {
                    let parsed_otp = if let Some(c) = v.as_str() {
                        let trimmed = c.trim();
                        if trimmed.len() == 6 && trimmed.bytes().all(|b| b.is_ascii_digit()) {
                            Some(trimmed.to_owned())
                        } else if let Some((a, b)) = trimmed.split_once('-')
                            && a.len() == 3
                            && b.len() == 3
                            && a.bytes().all(|c| c.is_ascii_digit())
                            && b.bytes().all(|c| c.is_ascii_digit())
                        {
                            Some(format!("{a}{b}"))
                        } else {
                            None
                        }
                    } else if key != "code"
                        && let Some(n) = v.as_u64()
                        && (100_000..=999_999).contains(&n)
                    {
                        Some(n.to_string())
                    } else {
                        None
                    };
                    if let Some(code_str) = parsed_otp
                        && !explicit_otps.iter().any(|existing| existing == &code_str)
                    {
                        explicit_otps.push(code_str);
                    }
                }
                if matches!(
                    key.as_str(),
                    "body"
                        | "html"
                        | "text"
                        | "plain"
                        | "textbody"
                        | "htmlbody"
                        | "text_body"
                        | "html_body"
                        | "raw"
                        | "snippet"
                        | "bodypreview"
                        | "body_preview"
                        | "subject"
                        | "content"
                ) {
                    if let Some(s) = v.as_str() {
                        append_decoded_json_body_str(s, is_b64, is_qp, extra_text);
                    } else if key == "subject"
                        && let Some(arr) = v.as_array()
                    {
                        for item in arr {
                            if let Some(s) = item.as_str() {
                                append_decoded_json_body_str(s, is_b64, is_qp, extra_text);
                            }
                        }
                    } else if v.is_object() || v.is_array() {
                        extract_json_email_fields_inner(
                            v,
                            explicit_urls,
                            explicit_otps,
                            extra_text,
                            depth + 1,
                        );
                    }
                } else if matches!(key.as_str(), "headers" | "header") {
                    if let Some(hdr_map) = v.as_object() {
                        for (hk, hv) in hdr_map {
                            if hk.eq_ignore_ascii_case("subject") {
                                if let Some(s) = hv.as_str() {
                                    append_decoded_json_body_str(s, false, false, extra_text);
                                } else if let Some(arr) = hv.as_array() {
                                    for item in arr {
                                        if let Some(s) = item.as_str() {
                                            append_decoded_json_body_str(
                                                s, false, false, extra_text,
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    } else if let Some(hdr_arr) = v.as_array() {
                        for item in hdr_arr {
                            if let Some(item_obj) = item.as_object() {
                                let is_subj = item_obj.iter().any(|(hk, hv)| {
                                    matches!(
                                        hk.to_ascii_lowercase().as_str(),
                                        "name" | "key" | "header"
                                    ) && hv
                                        .as_str()
                                        .is_some_and(|s| s.trim().eq_ignore_ascii_case("subject"))
                                });
                                if is_subj {
                                    for (hk, hv) in item_obj {
                                        if matches!(
                                            hk.to_ascii_lowercase().as_str(),
                                            "value" | "values" | "val"
                                        ) && let Some(s) = hv.as_str()
                                        {
                                            append_decoded_json_body_str(
                                                s, false, false, extra_text,
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                } else if key == "data" {
                    if let Some(s) = v.as_str() {
                        if s.contains("://") || s.contains('<') || s.contains(' ') {
                            append_decoded_json_body_str(s, is_b64, is_qp, extra_text);
                        } else if s.len() >= 8
                            && let Some(decoded) = decode_base64_block(s)
                            && decoded.bytes().all(|b| {
                                !b.is_ascii_control() || matches!(b, b'\r' | b'\n' | b'\t')
                            })
                        {
                            append_decoded_json_body_str(&decoded, false, is_qp, extra_text);
                        }
                    } else if v.is_object() || v.is_array() {
                        extract_json_email_fields_inner(
                            v,
                            explicit_urls,
                            explicit_otps,
                            extra_text,
                            depth + 1,
                        );
                    }
                } else if matches!(key.as_str(), "bodyvalues" | "body_values")
                    && let Some(bv_map) = v.as_object()
                {
                    for part in bv_map.values() {
                        if let Some(part_obj) = part.as_object() {
                            for (pk, pv) in part_obj {
                                if matches!(
                                    pk.to_ascii_lowercase().as_str(),
                                    "value" | "body" | "text" | "html"
                                ) && let Some(s) = pv.as_str()
                                {
                                    append_decoded_json_body_str(s, is_b64, is_qp, extra_text);
                                }
                            }
                        } else if let Some(s) = part.as_str() {
                            append_decoded_json_body_str(s, is_b64, is_qp, extra_text);
                        }
                    }
                } else if matches!(
                    key.as_str(),
                    "message" | "mail" | "payload" | "mime" | "parts" | "attributes"
                ) && (v.is_object() || v.is_array())
                {
                    extract_json_email_fields_inner(
                        v,
                        explicit_urls,
                        explicit_otps,
                        extra_text,
                        depth + 1,
                    );
                }
            }
        }
        _ => {}
    }
}

fn is_json_message_self_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    let has_auth_kw = [
        "magic",
        "verify",
        "token=",
        "callback",
        "authorize",
        "approve",
        "confirm",
    ]
    .iter()
    .any(|kw| lower.contains(kw));
    !has_auth_kw
        && (lower.contains("/api/")
            || lower.contains("/messages/")
            || lower.contains("/message/")
            || lower.contains("/emails/"))
}

fn percent_decode_uri(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
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

fn decode_proofpoint_v2(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'_' {
            out.push(b'/');
            i += 1;
        } else if bytes[i] == b'-'
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

fn decode_proofpoint_v3(inner: &str, after_close: &str) -> String {
    let mut out = if inner
        .get(..8)
        .is_some_and(|p| p.eq_ignore_ascii_case("https%3a"))
        || inner
            .get(..7)
            .is_some_and(|p| p.eq_ignore_ascii_case("http%3a"))
    {
        percent_decode_uri(inner)
    } else {
        inner.to_owned()
    };
    if out.contains('*')
        && let Some(after_semi) = after_close.strip_prefix(';')
        && let Some((enc_b64, _)) = after_semi.split_once('!')
        && !enc_b64.is_empty()
        && let Some(repl_bytes) = decode_base64_bytes(enc_b64)
    {
        let mut replaced = Vec::with_capacity(out.len());
        let mut repl_iter = repl_bytes.into_iter();
        for b in out.bytes() {
            if b == b'*' {
                replaced.push(repl_iter.next().unwrap_or(b'*'));
            } else {
                replaced.push(b);
            }
        }
        out = String::from_utf8_lossy(&replaced).into_owned();
    }
    out
}

fn peel_tracked_url_once(url: &str) -> Option<String> {
    if provider_url(url)
        || url.starts_with("http://127.0.0.1:")
        || url.starts_with("http://localhost:")
    {
        return None;
    }
    if let Some((_, after_v3)) = url.split_once("/v3/__")
        && let Some((inner, after_close)) = after_v3.split_once("__")
    {
        let decoded = decode_proofpoint_v3(inner, after_close);
        let cleaned = decoded.trim_end_matches(['.', ',', ';']);
        if cleaned.starts_with("https://") || cleaned.starts_with("http://") {
            return Some(cleaned.to_owned());
        }
    }
    if let Some((_, after_l0)) = url.split_once("/L0/") {
        let encoded = after_l0.split(['/', '?']).next().unwrap_or(after_l0);
        let decoded = percent_decode_uri(encoded);
        let cleaned = decoded.trim_end_matches(['.', ',', ';']);
        if cleaned.starts_with("https://") || cleaned.starts_with("http://") {
            return Some(cleaned.to_owned());
        }
    }
    let (_, query) = url.split_once('?')?;
    let query = query.split('#').next().unwrap_or(query);
    let lower_url = url.to_ascii_lowercase();
    for pair in query.split('&') {
        let Some((k, v)) = pair.split_once('=') else {
            continue;
        };
        let key_lower = percent_decode_uri(k).to_ascii_lowercase();
        if !matches!(
            key_lower.as_str(),
            "url"
                | "u"
                | "target"
                | "redirect"
                | "redirect_url"
                | "dest"
                | "destination"
                | "link"
                | "goto"
                | "q"
        ) {
            continue;
        }
        let decoded = if key_lower == "u"
            && (lower_url.contains("urldefense")
                || v.starts_with("https-3A__")
                || v.starts_with("http-3A__"))
        {
            decode_proofpoint_v2(v)
        } else {
            percent_decode_uri(v)
        };
        let cleaned = decoded.trim_end_matches(['.', ',', ';']);
        if cleaned.starts_with("https://") || cleaned.starts_with("http://") {
            return Some(cleaned.to_owned());
        }
    }
    None
}

enum TrackedUrl {
    NotTracked,
    Unwrapped(String),
    RejectedTarget,
}

fn classify_tracked_magic_url(url: &str) -> TrackedUrl {
    let Some(mut current) = peel_tracked_url_once(url) else {
        return TrackedUrl::NotTracked;
    };
    for _ in 0..3 {
        if let Some(next) = peel_tracked_url_once(&current) {
            current = next;
        } else {
            break;
        }
    }
    if is_supported_auth_url(&current) && !is_non_auth_link(&current) {
        TrackedUrl::Unwrapped(current)
    } else {
        TrackedUrl::RejectedTarget
    }
}

fn is_negative_email_anchor(text_lower: &str) -> bool {
    let trimmed = text_lower.trim();
    !trimmed.is_empty()
        && [
            "unsubscribe",
            "opt out",
            "opt-out",
            "email preferences",
            "manage preferences",
            "notification settings",
            "privacy",
            "terms",
            "legal",
            "help center",
            "support",
            "contact us",
            "view in browser",
            "view online",
            "view email",
            "read in browser",
            "cancel",
            "deny",
            "revoke",
            "reject",
            "decline",
            "not you",
            "wasn't you",
            "wasnt you",
            "report ",
            "lock account",
            "careers",
            "pricing",
            "blog",
        ]
        .iter()
        .any(|kw| trimmed.contains(kw))
}

fn is_positive_email_anchor(text_lower: &str) -> bool {
    let trimmed = text_lower.trim();
    !trimmed.is_empty()
        && !is_negative_email_anchor(trimmed)
        && [
            "sign in",
            "sign-in",
            "signin",
            "log in",
            "log-in",
            "login",
            "verify",
            "confirm",
            "magic link",
            "authenticate",
            "approve",
            "authorize",
            "complete sign",
            "continue to claude",
            "open claude",
        ]
        .iter()
        .any(|kw| trimmed.contains(kw))
}

fn classify_email_html_anchors(html: &str) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut positive = BTreeSet::new();
    let mut negative = BTreeSet::new();
    let mut scan = html;
    while let Some(a_pos) = find_ascii_ci(scan, "<a") {
        let after_a = &scan[a_pos..];
        if after_a.len() > 2
            && !after_a.as_bytes()[2].is_ascii_whitespace()
            && !matches!(after_a.as_bytes()[2], b'>' | b'/')
        {
            scan = &after_a[2..];
            continue;
        }
        let Some(end_a) = find_tag_close(after_a) else {
            break;
        };
        let a_tag = &after_a[..=end_a];
        let after_open = &after_a[end_a + 1..];
        let self_closing = a_tag.trim_end_matches('>').trim_end().ends_with('/');
        let end_anchor = if self_closing {
            0
        } else {
            find_ascii_ci(after_open, "</a>").unwrap_or(0)
        };
        let inner_raw = &after_open[..end_anchor];
        let mut label = strip_inline_html(inner_raw);
        for extra in [
            extract_attr(a_tag, "aria-label"),
            extract_attr(a_tag, "title"),
        ]
        .into_iter()
        .flatten()
        {
            if !extra.is_empty() {
                label.push(' ');
                label.push_str(&extra);
            }
        }
        if let Some(href) = extract_attr(a_tag, "href") {
            let cleaned = href.trim().trim_end_matches(['.', ',', ';']);
            if !cleaned.is_empty() {
                let label_low = label.to_ascii_lowercase();
                if is_positive_email_anchor(&label_low) {
                    positive.insert(cleaned.to_owned());
                    if let TrackedUrl::Unwrapped(u) = classify_tracked_magic_url(cleaned) {
                        positive.insert(u);
                    }
                } else if is_negative_email_anchor(&label_low) {
                    negative.insert(cleaned.to_owned());
                    if let TrackedUrl::Unwrapped(u) = classify_tracked_magic_url(cleaned) {
                        negative.insert(u);
                    }
                }
            }
        }
        scan = after_open;
    }
    (positive, negative)
}

pub(super) fn extract_ranked_magic_links_from_email(
    raw_email: &str,
    target_email: Option<&str>,
) -> (Vec<String>, Vec<String>) {
    if let Some(target) = target_email
        && !email_matches_target(raw_email, target)
    {
        return (Vec::new(), Vec::new());
    }
    let mut extra_text = decode_mime_base64_parts(raw_email);
    if !extra_text.is_empty() {
        extra_text = decode_qp_mime_parts(&extra_text);
    }
    let mut explicit_urls = Vec::new();
    let mut ignored_otps = Vec::new();
    let mut is_json_with_body = false;
    if let Ok(doc) = aterm_json::from_str::<Value>(raw_email) {
        extract_json_email_fields(&doc, &mut explicit_urls, &mut ignored_otps, &mut extra_text);
        if doc.is_object() {
            is_json_with_body = true;
        }
    }
    let qp_decoded = if is_json_with_body || is_top_level_base64_email(raw_email) {
        String::new()
    } else if is_quoted_printable_email(raw_email) {
        decode_quoted_printable(raw_email)
    } else {
        decode_qp_mime_parts(raw_email)
    };
    if let Some(subj) = extract_rfc5322_subject(raw_email) {
        extra_text.push('\n');
        extra_text.push_str(&subj);
    }
    let srcdoc_extra = extract_iframe_srcdoc_fragments(&format!("{qp_decoded}\n{extra_text}"));
    let combined = strip_html_comments(&format!("{srcdoc_extra}\n{extra_text}\n{qp_decoded}"));
    let decoded = decode_html_amp(&combined);
    let (mut positive_anchors, negative_anchors) = classify_email_html_anchors(&decoded);
    let mut urls = Vec::new();
    for explicit in explicit_urls {
        match classify_tracked_magic_url(&explicit) {
            TrackedUrl::RejectedTarget => continue,
            TrackedUrl::Unwrapped(unwrapped) => {
                positive_anchors.insert(unwrapped.clone());
                if !urls.iter().any(|u| u == &unwrapped) {
                    urls.push(unwrapped);
                }
            }
            TrackedUrl::NotTracked => {}
        }
        positive_anchors.insert(explicit.clone());
        if !urls.iter().any(|u| u == &explicit) {
            urls.push(explicit);
        }
    }
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
            let rejected_by_anchor =
                negative_anchors.contains(candidate) && !positive_anchors.contains(candidate);
            if !rejected_by_anchor
                && is_supported_auth_url(candidate)
                && !is_non_auth_link(candidate)
            {
                match classify_tracked_magic_url(candidate) {
                    TrackedUrl::RejectedTarget => {}
                    TrackedUrl::Unwrapped(unwrapped) => {
                        let unwrapped_rejected = negative_anchors.contains(&unwrapped)
                            && !positive_anchors.contains(&unwrapped);
                        if !unwrapped_rejected {
                            if !urls.iter().any(|u| u == &unwrapped) {
                                urls.push(unwrapped);
                            }
                            if !urls.iter().any(|u| u == candidate) {
                                urls.push(candidate.to_owned());
                            }
                        }
                    }
                    TrackedUrl::NotTracked => {
                        if !urls.iter().any(|u| u == candidate) {
                            urls.push(candidate.to_owned());
                        }
                    }
                }
            }
            rest = &after[end..];
        }
    }
    let looks_like_magic_token = |u: &str| {
        let lower = u.to_ascii_lowercase();
        lower.contains("magic")
            || lower.contains("verify")
            || lower.contains("confirm")
            || lower.contains("callback")
            || lower.contains("token=")
            || lower.contains("code=")
            || lower.contains("ticket=")
            || lower.contains("stytch")
            || lower.contains("authenticate")
    };
    let looks_like_auth = |u: &str| {
        let lower = u.to_ascii_lowercase();
        looks_like_magic_token(u)
            || lower.contains("login")
            || lower.contains("auth")
            || positive_anchors.contains(u)
    };
    let mut auth_ranked = Vec::with_capacity(urls.len());
    let mut push_auth = |u: &String| {
        if !auth_ranked.iter().any(|existing| existing == u) {
            auth_ranked.push(u.clone());
        }
    };
    for u in urls
        .iter()
        .filter(|u| provider_url(u) && looks_like_magic_token(u))
    {
        push_auth(u);
    }
    for u in urls
        .iter()
        .filter(|u| provider_url(u) && looks_like_auth(u))
    {
        push_auth(u);
    }
    for u in urls.iter().filter(|u| looks_like_magic_token(u)) {
        push_auth(u);
    }
    for u in urls.iter().filter(|u| looks_like_auth(u)) {
        push_auth(u);
    }
    let mut fallback_ranked = Vec::new();
    let mut push_fallback = |u: &String| {
        if !auth_ranked.iter().any(|existing| existing == u)
            && !fallback_ranked.iter().any(|existing| existing == u)
        {
            fallback_ranked.push(u.clone());
        }
    };
    for u in urls.iter().filter(|u| provider_url(u)) {
        push_fallback(u);
    }
    for u in &urls {
        push_fallback(u);
    }
    (auth_ranked, fallback_ranked)
}

/// Extracts all candidate verification or magic-link URLs from `raw_email` addressed to
/// `target_email` in priority order.
#[must_use]
pub fn extract_magic_links_from_email(raw_email: &str, target_email: Option<&str>) -> Vec<String> {
    let (mut auth_links, fallback_links) =
        extract_ranked_magic_links_from_email(raw_email, target_email);
    auth_links.extend(fallback_links);
    auth_links
}

/// Extracts a verification or magic-link URL from an incoming email addressed to `target_email`.
#[must_use]
pub fn extract_magic_link_from_email(
    raw_email: &str,
    target_email: Option<&str>,
) -> Option<String> {
    extract_magic_links_from_email(raw_email, target_email)
        .into_iter()
        .next()
}

fn normalize_dot_segments(path_and_query: &str) -> String {
    let (raw_path, query) = path_and_query
        .split_once('?')
        .map_or((path_and_query, None), |(p, q)| (p, Some(q)));
    if !raw_path.contains("/.") {
        return path_and_query.to_owned();
    }
    let trailing_slash = raw_path.len() > 1
        && (raw_path.ends_with('/') || raw_path.ends_with("/.") || raw_path.ends_with("/.."));
    let mut stack: Vec<&str> = Vec::new();
    for seg in raw_path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                stack.pop();
            }
            s => stack.push(s),
        }
    }
    let normalized_path = if stack.is_empty() {
        "/".to_owned()
    } else if trailing_slash {
        format!("/{}/", stack.join("/"))
    } else {
        format!("/{}", stack.join("/"))
    };
    match query {
        Some(q) => format!("{normalized_path}?{q}"),
        None => normalized_path,
    }
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
    let (authority, tail) = rest
        .find(['/', '?', '#'])
        .map_or((rest, ""), |idx| rest.split_at(idx));
    let base_path = tail.find(['?', '#']).map_or(tail, |idx| &tail[..idx]);
    let effective_path = if base_path.is_empty() { "/" } else { base_path };
    if target.is_empty() {
        return base_url.split('#').next().unwrap_or(base_url).to_owned();
    }
    let target = target.split('#').next().unwrap_or(&target);
    if target.starts_with('?') {
        return format!("{scheme}://{authority}{effective_path}{target}");
    }
    let combined = if target.starts_with('/') {
        normalize_dot_segments(target)
    } else {
        let dir = effective_path
            .rfind('/')
            .map_or("/", |idx| &effective_path[..=idx]);
        normalize_dot_segments(&format!("{dir}{target}"))
    };
    format!("{scheme}://{authority}{combined}")
}

/// Find a real attribute; `Some(None)` is a bare boolean attribute.
fn html_attr<'a>(tag: &'a str, attr_name: &str) -> Option<Option<&'a str>> {
    let bytes = tag.as_bytes();
    // All callers supply an opening tag. Skip its name before reading attributes.
    let mut pos = bytes
        .iter()
        .position(|&b| b.is_ascii_whitespace() || matches!(b, b'>' | b'/'))?;
    while pos < bytes.len() {
        while pos < bytes.len() && (bytes[pos].is_ascii_whitespace() || bytes[pos] == b'/') {
            pos += 1;
        }
        if pos == bytes.len() || bytes[pos] == b'>' {
            break;
        }
        let start = pos;
        while pos < bytes.len()
            && !bytes[pos].is_ascii_whitespace()
            && !matches!(bytes[pos], b'=' | b'>' | b'/')
        {
            pos += 1;
        }
        if start == pos {
            pos += 1;
            continue;
        }
        let name = &tag[start..pos];
        while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }
        let value = if bytes.get(pos) == Some(&b'=') {
            pos += 1;
            while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
                pos += 1;
            }
            let first = *bytes.get(pos)?;
            let start = pos;
            if matches!(first, b'"' | b'\'') {
                pos += 1;
                let end = pos + bytes[pos..].iter().position(|b| *b == first)?;
                let value = &tag[pos..end];
                pos = end + 1;
                Some(value)
            } else {
                while pos < bytes.len() && !bytes[pos].is_ascii_whitespace() && bytes[pos] != b'>' {
                    pos += 1;
                }
                Some(&tag[start..pos])
            }
        } else {
            None
        };
        if name.eq_ignore_ascii_case(attr_name) {
            return Some(value);
        }
    }
    None
}

fn extract_attr(tag: &str, attr_name: &str) -> Option<String> {
    html_attr(tag, attr_name).flatten().map(decode_html_amp)
}

fn has_html_attr(tag: &str, attr_name: &str) -> bool {
    html_attr(tag, attr_name).is_some()
}

fn is_negative_form_token(s: &str) -> bool {
    let lower = s.trim().to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "deny"
            | "cancel"
            | "reject"
            | "decline"
            | "no"
            | "false"
            | "logout"
            | "signout"
            | "sign-out"
            | "sign out"
            | "log out"
            | "abort"
            | "back"
            | "dismiss"
            | "close"
            | "never"
            | "disagree"
            | "refuse"
            | "revoke"
    ) || [
        "do not ",
        "don't ",
        "dont ",
        "not allow",
        "not authorize",
        "not approve",
        "not accept",
        "not you",
        "deny ",
        "cancel ",
        "reject ",
        "decline ",
        "abort ",
        "revoke ",
        "refuse ",
        "disagree",
        "switch account",
        "another account",
        "different account",
    ]
    .iter()
    .any(|bad| lower.contains(bad))
}

fn has_negative_url_keyword(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    [
        "logout",
        "signout",
        "sign-out",
        "deny",
        "cancel",
        "reject",
        "decline",
        "revoke",
        "/search",
        "unsubscribe",
        "access_denied",
        "?error=",
        "&error=",
    ]
    .iter()
    .any(|bad| lower.contains(bad))
}

fn has_positive_approval_keyword(s: &str) -> bool {
    if is_negative_form_token(s) {
        return false;
    }
    let lower = s.to_ascii_lowercase();
    [
        "approve",
        "authorize",
        "consent",
        "allow",
        "accept",
        "confirm",
        "continue",
        "grant",
        "yes",
    ]
    .iter()
    .any(|kw| lower.contains(kw))
}

fn strip_inline_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(lt) = rest.find('<') {
        out.push_str(&rest[..lt]);
        let from_lt = &rest[lt..];
        if let Some(gt) = find_tag_close(from_lt) {
            rest = &from_lt[gt + 1..];
        } else {
            break;
        }
    }
    out.push_str(rest);
    decode_html_amp(out.trim())
}

fn parse_json_approval_action(
    doc: &Value,
    base_url: &str,
) -> Option<(String, String, Option<String>)> {
    let candidates = [Some(doc), doc.get("data"), doc.get("result")];
    for obj in candidates.into_iter().flatten() {
        let method_override = obj
            .get("method")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(str::to_ascii_uppercase);
        for key in [
            "approve_url",
            "approval_url",
            "authorize_url",
            "consent_url",
        ] {
            if let Some(target) = obj
                .get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                let resolved = resolve_relative_url(base_url, target);
                if !has_negative_url_keyword(&resolved) {
                    let method = method_override.clone().unwrap_or_else(|| "POST".to_owned());
                    return Some((method, resolved, None));
                }
            }
        }
        for key in [
            "redirect_url",
            "redirect_uri",
            "redirect_to",
            "callback_url",
            "continue_url",
            "return_to",
            "next_url",
            "location",
        ] {
            if let Some(target) = obj
                .get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                let resolved = resolve_relative_url(base_url, target);
                if !has_negative_url_keyword(&resolved) {
                    let method = method_override.unwrap_or_else(|| "GET".to_owned());
                    return Some((method, resolved, None));
                }
            }
        }
    }
    None
}

fn extract_meta_refresh_url(html: &str, base_url: &str) -> Option<String> {
    let mut scan = html;
    while let Some(meta_pos) = find_ascii_ci(scan, "<meta") {
        let after_meta = &scan[meta_pos..];
        if after_meta.len() > 5
            && !after_meta.as_bytes()[5].is_ascii_whitespace()
            && !matches!(after_meta.as_bytes()[5], b'>' | b'/')
        {
            scan = &after_meta[5..];
            continue;
        }
        let Some(end_meta) = find_tag_close(after_meta) else {
            break;
        };
        let meta_tag = &after_meta[..=end_meta];
        if extract_attr(meta_tag, "http-equiv")
            .is_some_and(|v| v.trim().eq_ignore_ascii_case("refresh"))
            && let Some(content) = extract_attr(meta_tag, "content")
        {
            let after_semi = content
                .split_once(';')
                .map_or(content.as_str(), |(_, tail)| tail)
                .trim();
            let raw_target = if after_semi
                .get(..3)
                .is_some_and(|p| p.eq_ignore_ascii_case("url"))
            {
                after_semi[3..]
                    .trim_start()
                    .strip_prefix('=')
                    .map(|v| v.trim().trim_matches(['"', '\'']).trim())
            } else {
                None
            };
            if let Some(raw_target) = raw_target.filter(|s| !s.is_empty()) {
                let low = raw_target.to_ascii_lowercase();
                if !low.starts_with("javascript:")
                    && !low.starts_with("mailto:")
                    && !raw_target.starts_with('#')
                {
                    let resolved = resolve_relative_url(base_url, raw_target);
                    if !has_negative_url_keyword(&resolved) {
                        return Some(resolved);
                    }
                }
            }
        }
        scan = &after_meta[end_meta + 1..];
    }
    None
}

struct SubmitChoice {
    score: u8,
    name: Option<String>,
    value: String,
    formaction: Option<String>,
    formmethod: Option<String>,
}

/// Parses an OAuth consent page into `(HTTP method, resolved action URL, optional form body)`.
#[must_use]
pub fn parse_html_approval_action(
    html: &str,
    base_url: &str,
) -> Option<(String, String, Option<String>)> {
    if let Ok(doc) = aterm_json::from_str::<Value>(html)
        && let Some(parsed) = parse_json_approval_action(&doc, base_url)
    {
        return Some(parsed);
    }
    let uncommented = strip_html_comments(html);
    let html = uncommented.as_str();
    let mut best_form: Option<(u8, String, String, Option<String>)> = None;
    let mut search_pos = 0;
    while let Some(rel_start) = find_ascii_ci(&html[search_pos..], "<form") {
        let form_start = search_pos + rel_start;
        let after_name = form_start + 5;
        if after_name < html.len()
            && !html.as_bytes()[after_name].is_ascii_whitespace()
            && !matches!(html.as_bytes()[after_name], b'>' | b'/')
        {
            search_pos = after_name;
            continue;
        }
        let form_slice = &html[form_start..];
        let form_end =
            find_ascii_ci(form_slice, "</form>").map_or(form_slice.len(), |e| e + "</form>".len());
        search_pos = form_start + form_end;
        let form_html = &form_slice[..form_end];
        let Some(tag_end) = find_tag_close(form_html) else {
            continue;
        };
        let open_tag = &form_html[..=tag_end];
        let raw_action = extract_attr(open_tag, "action").unwrap_or_default();
        let mut action = resolve_relative_url(base_url, &raw_action);
        if has_negative_url_keyword(&action) {
            continue;
        }
        let mut method = extract_attr(open_tag, "method")
            .unwrap_or_else(|| "GET".to_owned())
            .to_ascii_uppercase();
        let mut fields = Vec::new();
        let mut saw_submit = false;
        let mut chosen_submit: Option<SubmitChoice> = None;
        let mut scan = &form_html[tag_end + 1..];
        while !scan.is_empty() {
            let next_in = find_ascii_ci(scan, "<input");
            let next_btn = find_ascii_ci(scan, "<button");
            let (ctrl_pos, is_button, tag_len) = match (next_in, next_btn) {
                (Some(i), Some(b)) if i <= b => (i, false, 6),
                (Some(_), Some(b)) => (b, true, 7),
                (Some(i), None) => (i, false, 6),
                (None, Some(b)) => (b, true, 7),
                (None, None) => break,
            };
            let after_ctrl = &scan[ctrl_pos..];
            if after_ctrl.len() > tag_len
                && !after_ctrl.as_bytes()[tag_len].is_ascii_whitespace()
                && !matches!(after_ctrl.as_bytes()[tag_len], b'>' | b'/')
            {
                scan = &after_ctrl[tag_len..];
                continue;
            }
            let Some(end_ctrl) = find_tag_close(after_ctrl) else {
                break;
            };
            let ctrl_tag = &after_ctrl[..=end_ctrl];
            let after_open = &after_ctrl[end_ctrl + 1..];
            let self_closing = ctrl_tag.trim_end_matches('>').trim_end().ends_with('/');
            let button_text = if is_button && !self_closing {
                let end_btn = find_ascii_ci(after_open, "</button>").unwrap_or(0);
                strip_inline_html(&after_open[..end_btn])
            } else {
                String::new()
            };
            scan = after_open;
            if has_html_attr(ctrl_tag, "disabled") {
                continue;
            }
            let ctrl_type = extract_attr(ctrl_tag, "type")
                .unwrap_or_else(|| {
                    if is_button {
                        "submit".to_owned()
                    } else {
                        "text".to_owned()
                    }
                })
                .to_ascii_lowercase();
            if matches!(ctrl_type.as_str(), "reset" | "button") {
                continue;
            }
            if matches!(ctrl_type.as_str(), "checkbox" | "radio")
                && !has_html_attr(ctrl_tag, "checked")
            {
                continue;
            }
            let is_submit = is_button || matches!(ctrl_type.as_str(), "submit" | "image");
            if is_submit {
                saw_submit = true;
                let name = extract_attr(ctrl_tag, "name");
                let value = extract_attr(ctrl_tag, "value").unwrap_or_default();
                let label_attr = extract_attr(ctrl_tag, "aria-label")
                    .or_else(|| extract_attr(ctrl_tag, "title"))
                    .or_else(|| extract_attr(ctrl_tag, "alt"))
                    .unwrap_or_default();
                let formaction = extract_attr(ctrl_tag, "formaction")
                    .map(|fa| resolve_relative_url(base_url, &fa));
                let formmethod =
                    extract_attr(ctrl_tag, "formmethod").map(|fm| fm.to_ascii_uppercase());
                let is_neg = name.as_deref().is_some_and(is_negative_form_token)
                    || is_negative_form_token(&value)
                    || (!button_text.is_empty() && is_negative_form_token(&button_text))
                    || (!label_attr.is_empty() && is_negative_form_token(&label_attr))
                    || formaction.as_deref().is_some_and(has_negative_url_keyword);
                if is_neg {
                    continue;
                }
                let is_pos = name.as_deref().is_some_and(has_positive_approval_keyword)
                    || has_positive_approval_keyword(&value)
                    || has_positive_approval_keyword(&button_text)
                    || has_positive_approval_keyword(&label_attr)
                    || formaction
                        .as_deref()
                        .is_some_and(has_positive_approval_keyword);
                let btn_score = if is_pos { 2 } else { 1 };
                if chosen_submit
                    .as_ref()
                    .is_none_or(|prev| btn_score > prev.score)
                {
                    chosen_submit = Some(SubmitChoice {
                        score: btn_score,
                        name,
                        value,
                        formaction,
                        formmethod,
                    });
                }
                continue;
            }
            if let Some(name) = extract_attr(ctrl_tag, "name") {
                let value = extract_attr(ctrl_tag, "value").unwrap_or_default();
                fields.push((
                    name.clone(),
                    format!("{}={}", percent_encode(&name), percent_encode(&value)),
                ));
            }
        }
        if saw_submit && chosen_submit.is_none() {
            continue;
        }
        let mut submit_score = 1u8;
        if let Some(sub) = chosen_submit {
            submit_score = sub.score;
            if let Some(fa) = sub.formaction {
                action = fa;
            }
            if let Some(fm) = sub.formmethod {
                method = fm;
            }
            if let Some(name) = sub.name {
                fields.push((
                    name.clone(),
                    format!("{}={}", percent_encode(&name), percent_encode(&sub.value)),
                ));
            }
        }
        let encoded: String = fields
            .iter()
            .map(|(_, kv)| kv.as_str())
            .collect::<Vec<_>>()
            .join("&");
        let candidate = if method == "POST" {
            ("POST".to_owned(), action.clone(), Some(encoded))
        } else {
            let full_url = if encoded.is_empty() {
                action.clone()
            } else if let Some((base_path, existing_q)) = action.split_once('?') {
                let kept_q: Vec<&str> = existing_q
                    .split('&')
                    .filter(|pair| {
                        let k = pair.split('=').next().unwrap_or("");
                        let decoded_k = percent_decode(k);
                        !k.is_empty() && !fields.iter().any(|(fk, _)| fk == &decoded_k)
                    })
                    .collect();
                if kept_q.is_empty() {
                    format!("{base_path}?{encoded}")
                } else {
                    format!("{base_path}?{}&{encoded}", kept_q.join("&"))
                }
            } else {
                format!("{action}?{encoded}")
            };
            ("GET".to_owned(), full_url, None)
        };
        let score = if submit_score == 2 {
            3
        } else if has_positive_approval_keyword(&action) || has_positive_approval_keyword(form_html)
        {
            2
        } else {
            1
        };
        if best_form
            .as_ref()
            .is_none_or(|(prev_score, ..)| score > *prev_score)
        {
            best_form = Some((score, candidate.0, candidate.1, candidate.2));
        }
    }
    if let Some((_, m, a, b)) = best_form {
        return Some((m, a, b));
    }
    let mut scan = html;
    while let Some(a_pos) = find_ascii_ci(scan, "<a") {
        let after_a = &scan[a_pos..];
        if after_a.len() > 2
            && !after_a.as_bytes()[2].is_ascii_whitespace()
            && !matches!(after_a.as_bytes()[2], b'>' | b'/')
        {
            scan = &after_a[2..];
            continue;
        }
        let Some(end_a) = find_tag_close(after_a) else {
            break;
        };
        let a_tag = &after_a[..=end_a];
        let after_open = &after_a[end_a + 1..];
        let self_closing = a_tag.trim_end_matches('>').trim_end().ends_with('/');
        let end_anchor = if self_closing {
            0
        } else {
            find_ascii_ci(after_open, "</a>").unwrap_or(0)
        };
        let mut anchor_text = strip_inline_html(&after_open[..end_anchor]);
        if anchor_text.is_empty() {
            anchor_text = extract_attr(a_tag, "aria-label")
                .or_else(|| extract_attr(a_tag, "title"))
                .unwrap_or_default();
        }
        if let Some(href) = extract_attr(a_tag, "href") {
            let href_trimmed = href.trim();
            if !href_trimmed.is_empty()
                && !href_trimmed.starts_with('#')
                && !href_trimmed.to_ascii_lowercase().starts_with("javascript:")
                && !href_trimmed.to_ascii_lowercase().starts_with("mailto:")
            {
                let resolved = resolve_relative_url(base_url, href_trimmed);
                let r_low = resolved.to_ascii_lowercase();
                let is_neg = has_negative_url_keyword(&r_low)
                    || (!anchor_text.is_empty() && is_negative_form_token(&anchor_text));
                if !is_neg
                    && (r_low.contains("approve")
                        || r_low.contains("authorize")
                        || r_low.contains("callback")
                        || r_low.contains("consent")
                        || r_low.contains("allow")
                        || r_low.contains("accept")
                        || r_low.contains("confirm")
                        || has_positive_approval_keyword(&anchor_text))
                {
                    return Some(("GET".to_owned(), resolved, None));
                }
            }
        }
        scan = after_open;
    }
    if let Some(refresh_url) = extract_meta_refresh_url(html, base_url) {
        return Some(("GET".to_owned(), refresh_url, None));
    }
    None
}

#[derive(Debug)]
pub(super) struct HttpResponse {
    pub(super) status: u16,
    pub(super) location: Option<String>,
    pub(super) body: String,
}

fn try_decode_chunked_body(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut pos = 0usize;
    let mut out = Vec::with_capacity(bytes.len());
    let mut saw_chunk = false;
    loop {
        let rest = bytes.get(pos..)?;
        let (line_end, eol_len) = if let Some(idx) = rest.windows(2).position(|w| w == b"\r\n") {
            (idx, 2usize)
        } else {
            (rest.iter().position(|&b| b == b'\n')?, 1usize)
        };
        let size_line = std::str::from_utf8(&rest[..line_end]).ok()?;
        let hex_part = size_line.split(';').next().unwrap_or("").trim();
        if hex_part.is_empty() || !hex_part.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let chunk_len = usize::from_str_radix(hex_part, 16).ok()?;
        pos += line_end + eol_len;
        if chunk_len == 0 {
            let valid_empty = matches!(bytes.get(pos..), Some(b"\r\n" | b"\n" | b""));
            return (saw_chunk || valid_empty).then(|| String::from_utf8_lossy(&out).into_owned());
        }
        let chunk_end = pos.checked_add(chunk_len)?;
        let chunk_bytes = bytes.get(pos..chunk_end)?;
        out.extend_from_slice(chunk_bytes);
        saw_chunk = true;
        pos = chunk_end;
        if bytes.get(pos..pos + 2) == Some(b"\r\n") {
            pos += 2;
        } else if bytes.get(pos) == Some(&b'\n') {
            pos += 1;
        } else {
            return None;
        }
    }
}

pub(super) fn parse_raw_http_response(
    raw: &str,
    cookies: &mut BTreeMap<String, String>,
) -> Option<HttpResponse> {
    let mut current = raw;
    while let Some((head, rest)) = split_header_body(current) {
        let first_line = head.lines().next()?;
        let mut parts = first_line.split_ascii_whitespace();
        let proto = parts.next()?;
        let code_str = parts.next()?;
        let status: u16 = code_str.parse().ok()?;
        if proto.starts_with("HTTP/")
            && ((100..200).contains(&status)
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
        let mut is_chunked = false;
        for line in head.lines().skip(1) {
            let Some((name, val)) = line.split_once(':') else {
                continue;
            };
            let val = val.trim();
            if name.eq_ignore_ascii_case("location") {
                location = Some(val.to_owned());
            } else if name.eq_ignore_ascii_case("transfer-encoding")
                && val.to_ascii_lowercase().contains("chunked")
            {
                is_chunked = true;
            } else if name.eq_ignore_ascii_case("set-cookie") {
                let mut segments = val.split(';');
                let pair = segments.next().unwrap_or(val).trim();
                let expired = segments.any(|attr| {
                    let Some((ak, av)) = attr.split_once('=') else {
                        return false;
                    };
                    let ak = ak.trim();
                    let av = av.trim().trim_matches('"');
                    (ak.eq_ignore_ascii_case("max-age") && (av == "0" || av.starts_with('-')))
                        || (ak.eq_ignore_ascii_case("expires") && av.contains("1970"))
                });
                if let Some((ck, cv)) = pair.split_once('=') {
                    let ck = ck.trim();
                    let cv = cv.trim();
                    if cv.is_empty() || expired {
                        cookies.remove(ck);
                    } else if !ck.is_empty() {
                        cookies.insert(ck.to_owned(), cv.to_owned());
                    }
                }
            }
        }
        let body = if is_chunked {
            try_decode_chunked_body(rest).unwrap_or_else(|| rest.to_owned())
        } else {
            rest.to_owned()
        };
        return Some(HttpResponse {
            status,
            location,
            body,
        });
    }
    None
}

pub(super) fn loopback_http_exchange(
    method: &str,
    url: &str,
    cookies: &mut BTreeMap<String, String>,
    form_body: Option<&str>,
) -> Result<HttpResponse, &'static str> {
    let rest = url
        .strip_prefix("http://")
        .ok_or("loopback client takes http:// only")?;
    let (authority, tail) = match rest.find(['/', '?']) {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None => (rest, "/"),
    };
    let path_and_query = if tail.starts_with('/') {
        tail.to_owned()
    } else {
        format!("/{tail}")
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

fn curl_config(options: &[(&'static str, &str)]) -> std::io::Result<String> {
    if options
        .iter()
        .any(|(name, value)| name.contains('\0') || value.contains('\0'))
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "curl config contains a NUL",
        ));
    }
    let mut text = String::new();
    for (name, value) in options {
        text.push_str(name);
        text.push_str(" = \"");
        for c in value.chars() {
            match c {
                '\\' => text.push_str("\\\\"),
                '"' => text.push_str("\\\""),
                '\n' => text.push_str("\\n"),
                '\r' => text.push_str("\\r"),
                '\t' => text.push_str("\\t"),
                '\u{b}' => text.push_str("\\v"),
                c => text.push(c),
            }
        }
        text.push_str("\"\n");
    }
    Ok(text)
}

fn curl_command(
    program: impl AsRef<std::ffi::OsStr>,
    args: &[&str],
    options: &[(&'static str, &str)],
) -> std::io::Result<(aterm_uds::spawnfd::Command, String)> {
    use aterm_uds::spawnfd::Stdio;

    let input = curl_config(options)?;
    let mut cmd = aterm_uds::spawnfd::command(program);
    cmd.args(args)
        .args(["--config", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    Ok((cmd, input))
}

/// Pass fixed curl options containing credentials or URLs through stdin, preserving the
/// caller's environment and flags. Callers put `--disable` first to suppress `.curlrc`.
/// # Errors
/// A config NUL, child or writer startup failure, failed pipe I/O or wait, or a writer panic.
pub(super) fn curl_output(
    args: &[&str],
    options: &[(&'static str, &str)],
) -> std::io::Result<std::process::Output> {
    curl_output_with(aterm_uds::os_path!("curl"), args, options)
}

fn curl_output_with(
    program: impl AsRef<std::ffi::OsStr>,
    args: &[&str],
    options: &[(&'static str, &str)],
) -> std::io::Result<std::process::Output> {
    let (mut cmd, input) = curl_command(program, args, options)?;
    let mut child = cmd.spawn()?;
    let Some(mut stdin) = child.stdin.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(std::io::Error::other("curl config stdin is unavailable"));
    };
    std::thread::scope(|scope| {
        let writer = std::thread::Builder::new()
            .name("aterm-curl-config".into())
            .spawn_scoped(scope, move || stdin.write_all(input.as_bytes()));
        let writer = match writer {
            Ok(writer) => writer,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        // Drain both output pipes while config is written, so a large form and a
        // diagnostic cannot fill opposite pipes. Close stdin and join before returning.
        let output = child.wait_with_output();
        writer
            .join()
            .map_err(|_| std::io::Error::other("curl config writer panicked"))??;
        output
    })
}

fn https_curl_exchange(
    method: &str,
    url: &str,
    cookies: &mut BTreeMap<String, String>,
    form_body: Option<&str>,
) -> Result<HttpResponse, &'static str> {
    let cookie_hdr = cookies
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("; ");
    let mut options = vec![("url", url)];
    if !cookies.is_empty() {
        options.push(("cookie", cookie_hdr.as_str()));
    }
    if let Some(body) = form_body {
        options.push(("data-raw", body));
    }
    let out = curl_output(
        &[
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
        ],
        &options,
    )
    .map_err(|_| "failed running curl")?;
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

fn provider_url(url: &str) -> bool {
    (url.starts_with("https://claude.ai/") || url.starts_with("https://claude.com/"))
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
    let (port, tail) = match rest.find(['/', '?']) {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None => (rest, ""),
    };
    let port = port.parse::<u16>().ok().filter(|port| *port > 0)?;
    let (raw_path, query) = tail
        .split_once('?')
        .map_or((tail, None), |(path, query)| (path, Some(query)));
    let path = raw_path.strip_prefix('/').unwrap_or(raw_path);
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

/// The pages a sign-in flow may visit ([`MailboxFlow::pinned`]): Claude Code's authorize
/// endpoints, each with the one origin a flow it starts stays on ([`CLAUDE_PIN`]), or a vendor
/// route's pinned hosts, any `https` page on them (`connector::Route::hosts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pin {
    /// Each authorize endpoint and the one origin a flow it starts may visit.
    Endpoints(&'static [(&'static str, &'static str)]),
    /// Any `https` page on these hosts ([`super::login_drive::pinned`]).
    Hosts(&'static [&'static str]),
}

/// Claude Code's pin: claude.ai's and claude.com's authorize endpoints, each flow kept on the
/// origin it started on.
pub const CLAUDE_PIN: Pin = Pin::Endpoints(&[
    ("https://claude.ai/oauth/authorize", "https://claude.ai/"),
    (
        "https://claude.com/cai/oauth/authorize",
        "https://claude.com/",
    ),
]);

impl Pin {
    /// Whether a page of a flow under this pin may be `url`.
    #[must_use]
    pub fn admits(&self, url: &str) -> bool {
        match self {
            Pin::Endpoints(endpoints) => {
                provider_url(url) && endpoints.iter().any(|(_, origin)| url.starts_with(origin))
            }
            Pin::Hosts(hosts) => hosted_url(url, hosts),
        }
    }
}

/// `url` is `https` on one of `hosts`, with no fragment.
fn hosted_url(url: &str, hosts: &[&str]) -> bool {
    super::login_drive::pinned(url, hosts) && !url.contains('#')
}

/// The pages one flow may visit, fixed when it starts ([`MailboxFlow::pinned`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Origin {
    /// One origin of [`CLAUDE_PIN`]'s.
    Prefix(&'static str),
    /// A vendor route's hosts.
    Hosts(&'static [&'static str]),
}

/// Only the callback endpoint and state actually named by this authorize
/// request can leave the provider origin.
pub(super) struct MailboxFlow {
    origin: Origin,
    callback_port: u16,
    callback_path: String,
    state: String,
}

impl MailboxFlow {
    pub(super) fn new(authorize_url: &str) -> Result<Self, &'static str> {
        Self::pinned(authorize_url, CLAUDE_PIN)
    }

    /// The flow `authorize_url` starts under `pin`: refused unless the pin admits it.
    pub(super) fn pinned(authorize_url: &str, pin: Pin) -> Result<Self, &'static str> {
        let origin = match pin {
            Pin::Endpoints(endpoints) => {
                let base = authorize_url.split('?').next();
                endpoints
                    .iter()
                    .find(|(endpoint, _)| Some(*endpoint) == base && provider_url(authorize_url))
                    .map(|(_, origin)| Origin::Prefix(origin))
                    .ok_or("this authorize origin needs the manual browser")?
            }
            Pin::Hosts(hosts) if hosted_url(authorize_url, hosts) => Origin::Hosts(hosts),
            Pin::Hosts(_) => return Err("this authorize origin needs the manual browser"),
        };
        let callback = one_query_param(authorize_url, "redirect_uri")
            .ok_or("the authorize URL has no single local callback")?;
        let (port, path, query) =
            loopback_parts(&callback).ok_or("the authorize callback is not a local endpoint")?;
        if query.is_some() {
            return Err("the authorize callback must name an endpoint without a query");
        }
        Ok(Self {
            origin,
            callback_port: port,
            callback_path: path.to_owned(),
            state: one_query_param(authorize_url, "state")
                .ok_or("the authorize URL has no single state")?,
        })
    }

    pub(super) fn admits_provider_url(&self, url: &str) -> bool {
        match self.origin {
            Origin::Prefix(prefix) => url.starts_with(prefix) && provider_url(url),
            Origin::Hosts(hosts) => hosted_url(url, hosts),
        }
    }

    pub(super) fn is_callback(&self, url: &str) -> bool {
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
        if !flow.admits_provider_url(&current_url) {
            return Err("this OAuth destination needs the manual browser");
        }
        let resp = exchange(&method, &current_url, cookies, body.as_deref())?;
        if matches!(resp.status, 301..=303 | 307 | 308) {
            let Some(loc) = resp.location else {
                return Err("redirect response omitted Location header");
            };
            current_url = resolve_relative_url(&current_url, &loc);
            if matches!(resp.status, 301..=303) {
                method = "GET".to_owned();
                body = None;
            }
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

#[cfg(test)]
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

fn page_needs_browser_before_email(html: &str) -> bool {
    let uncommented = strip_html_comments(html);
    let mut scan = uncommented.as_str();
    while let Some(pos) = find_ascii_ci(scan, "<input") {
        let after = &scan[pos..];
        if after.len() > 6
            && !after.as_bytes()[6].is_ascii_whitespace()
            && !matches!(after.as_bytes()[6], b'>' | b'/')
        {
            scan = &after[6..];
            continue;
        }
        let Some(end) = find_tag_close(after) else {
            break;
        };
        let tag = &after[..=end];
        scan = &after[end + 1..];
        if has_html_attr(tag, "disabled") || has_html_attr(tag, "hidden") {
            continue;
        }
        let input_type = extract_attr(tag, "type")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        if input_type == "hidden" {
            continue;
        }
        if input_type == "email" {
            return true;
        }
        if extract_attr(tag, "name").is_some_and(|n| n.trim().eq_ignore_ascii_case("email")) {
            return true;
        }
        if extract_attr(tag, "autocomplete").is_some_and(|ac| {
            let ac_low = ac.trim().to_ascii_lowercase();
            ac_low == "email" || ac_low == "username" || ac_low.ends_with(" email")
        }) {
            return true;
        }
    }
    let lower = uncommented.to_ascii_lowercase();
    let has_email_sent_notice = lower.contains("check your email")
        || lower.contains("magic link")
        || lower.contains("sent a link")
        || lower.contains("verification code");
    lower.contains("<script") && !has_email_sent_notice
}

#[cfg(all(test, unix))]
fn poll_http_mailbox_credential(
    mailbox_cfg: &super::headless_oauth::MailboxConfig,
    email: Option<&str>,
    timeout: Duration,
    pending: &mut Option<super::headless_oauth::MailboxCredential>,
) -> Result<String, &'static str> {
    let mut seen_otps = BTreeSet::new();
    poll_http_mailbox_credential_with_seen_otps(
        mailbox_cfg,
        email,
        timeout,
        pending,
        &mut seen_otps,
    )
}

fn poll_http_mailbox_credential_with_seen_otps(
    mailbox_cfg: &super::headless_oauth::MailboxConfig,
    email: Option<&str>,
    timeout: Duration,
    pending: &mut Option<super::headless_oauth::MailboxCredential>,
    seen_otps: &mut BTreeSet<String>,
) -> Result<String, &'static str> {
    use super::headless_oauth::MailboxCredential;

    let credential = super::headless_oauth::poll_mailbox_config_for_credential_with_seen_otps(
        mailbox_cfg,
        email,
        timeout,
        seen_otps,
    )?;
    *pending = Some(credential.clone());
    match credential {
        MailboxCredential::MagicLink(url) => Ok(url),
        MailboxCredential::Otp(_) => {
            Err("verification email contained a 6-digit OTP requiring headless browser")
        }
    }
}

/// Completes the OAuth and email magic-link/OTP authorization flow using `mailbox_cfg`.
///
/// # Errors
///
/// Returns a static diagnostic string (without URLs or emails) on failure or timeout.
pub fn authorize_with_mailbox_config(
    authorize_url: &str,
    mailbox_cfg: &super::headless_oauth::MailboxConfig,
    store_dir: Option<&Path>,
    target_email: Option<&str>,
    timeout: Duration,
) -> Result<(), &'static str> {
    let force_headless = std::env::var(super::headless_oauth::HEADLESS_ENV)
        .is_ok_and(|v| matches!(v.trim(), "1" | "true" | "yes"));
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let has_headless_browser =
        super::headless_oauth::find_headless_browser(home.as_deref()).is_some();
    let t0 = Instant::now();
    let resolved_email = target_email
        .filter(|e| !e.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| extract_login_hint(authorize_url));
    let has_non_webmail = mailbox_cfg.dir.is_some()
        || mailbox_cfg
            .effective_imap_url(resolved_email.as_deref())
            .is_some()
        || mailbox_cfg.http_url.is_some();
    if !force_headless && has_non_webmail {
        let mut pending_credential = None;
        let mut seen_otps = BTreeSet::new();
        let mut attempt = 0usize;
        let first_res = authorize_using_with_page(
            authorize_url,
            target_email,
            |email, initial_body| {
                let poll_timeout = if attempt > 0 {
                    Duration::ZERO
                } else if has_headless_browser && page_needs_browser_before_email(initial_body) {
                    timeout
                        .saturating_sub(t0.elapsed())
                        .min(Duration::from_millis(200))
                } else {
                    timeout.saturating_sub(t0.elapsed())
                };
                attempt += 1;
                poll_http_mailbox_credential_with_seen_otps(
                    mailbox_cfg,
                    email,
                    poll_timeout,
                    &mut pending_credential,
                    &mut seen_otps,
                )
            },
            &mut http_exchange,
        );
        match first_res {
            Ok(()) => return Ok(()),
            Err(first_err) => {
                if matches!(
                    first_err,
                    "this OAuth destination needs the manual browser"
                        | "the local OAuth callback listener rejected the authorization code"
                ) || !has_headless_browser
                {
                    return Err(first_err);
                }
                let remaining = timeout.saturating_sub(t0.elapsed());
                if remaining.is_zero() {
                    return Err(first_err);
                }
                return super::headless_oauth::authorize_with_headless_browser_config_and_credential(
                    authorize_url,
                    store_dir,
                    Some(mailbox_cfg),
                    target_email,
                    remaining,
                    pending_credential,
                )
                .map_err(|cdp_err| {
                    if cdp_err.starts_with("no headless browser")
                        || cdp_err == "could not spawn headless browser"
                    {
                        first_err
                    } else {
                        cdp_err
                    }
                });
            }
        }
    }
    super::headless_oauth::authorize_with_headless_browser_config(
        authorize_url,
        store_dir,
        Some(mailbox_cfg),
        target_email,
        timeout,
    )
}

/// Completes the OAuth and email magic-link/OTP authorization flow using `mailbox_dir`.
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
    let store_dir = discover_store_dir();
    let cfg = super::headless_oauth::MailboxConfig::from_dir(mailbox_dir);
    authorize_with_mailbox_config(
        authorize_url,
        &cfg,
        store_dir.as_deref(),
        target_email,
        timeout,
    )
}

/// The real flow with its two effects passed as capabilities.
#[cfg(test)]
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
    let mut magic_link = Some(magic_link);
    authorize_using_with_page(
        authorize_url,
        target_email,
        |email, _| magic_link.take().ok_or("no more candidate magic links")?(email),
        exchange,
    )
}

fn authorize_using_with_page(
    authorize_url: &str,
    target_email: Option<&str>,
    mut magic_link: impl FnMut(Option<&str>, &str) -> Result<String, &'static str>,
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

    let initial_body = match follow_until_page_or_callback(
        &flow,
        exchange,
        "GET",
        authorize_url,
        &mut cookies,
        None,
    )? {
        StepOutcome::CallbackDelivered => return Ok(()),
        StepOutcome::Page { body, .. } => body,
    };

    let mut last_err = "magic-link response contained no OAuth approval action";
    for attempt in 0..3 {
        let magic_url = match magic_link(email.as_deref(), &initial_body) {
            Ok(url) => url,
            Err(err) => return Err(if attempt == 0 { err } else { last_err }),
        };

        let mut page = match follow_until_page_or_callback(
            &flow,
            exchange,
            "GET",
            &magic_url,
            &mut cookies,
            None,
        ) {
            Ok(StepOutcome::CallbackDelivered) => return Ok(()),
            Ok(StepOutcome::Page { url, body }) => (url, body),
            Err(err) => {
                if err == "the local OAuth callback listener rejected the authorization code" {
                    return Err(err);
                }
                last_err = err;
                continue;
            }
        };

        let mut step_failed = false;
        for step in 0..4 {
            let Some((method, action_url, form_body)) =
                parse_html_approval_action(&page.1, &page.0)
            else {
                last_err = if step == 0 {
                    "magic-link response contained no OAuth approval action"
                } else {
                    "OAuth approval did not redirect to callback listener"
                };
                step_failed = true;
                break;
            };
            match follow_until_page_or_callback(
                &flow,
                exchange,
                &method,
                &action_url,
                &mut cookies,
                form_body.as_deref(),
            ) {
                Ok(StepOutcome::CallbackDelivered) => return Ok(()),
                Ok(StepOutcome::Page { url, body }) => {
                    page = (url, body);
                }
                Err(err) => {
                    if err == "the local OAuth callback listener rejected the authorization code" {
                        return Err(err);
                    }
                    last_err = err;
                    step_failed = true;
                    break;
                }
            }
        }
        if !step_failed {
            last_err = "OAuth approval did not redirect to callback listener";
        }
    }
    Err(last_err)
}

/// The variable naming, to an opener or headless run, the pool's CLI (`connector::Cli::id`):
/// read ahead of Claude Code's home to find the account ([`account_label_of`],
/// [`store_dir_of`]).
pub const ACCOUNT_CLI_ENV: &str = "ATERM_ACCOUNT_CLI";

/// The variable naming, to an opener or headless run, the row's label ([`account_label_of`]).
pub const ACCOUNT_LABEL_ENV: &str = "ATERM_ACCOUNT_LABEL";

/// The account's label as `var` names it: [`ACCOUNT_LABEL_ENV`] where it holds a label a roster
/// could carry, else the name of Claude Code's home (`<store>/logins/<label>[+<seq>]`) unless
/// [`ACCOUNT_CLI_ENV`] names another CLI.
#[must_use]
pub fn account_label_of(var: &dyn Fn(&str) -> Option<OsString>) -> Option<String> {
    if let Some(label) = var(ACCOUNT_LABEL_ENV)
        .and_then(|v| v.into_string().ok())
        .map(|v| v.trim().to_owned())
        .filter(|v| super::accounts::admissible_label(v))
    {
        return Some(label);
    }
    if names_another_cli(var) {
        return None;
    }
    let dir = var(super::accounts::HOME_ENV)?;
    let name = Path::new(&dir).file_name()?.to_str()?;
    let label = name.rsplit_once('+').map_or(name, |(l, _)| l).trim();
    (!label.is_empty()).then(|| label.to_owned())
}

/// Whether [`ACCOUNT_CLI_ENV`] names a CLI other than Claude Code.
fn names_another_cli(var: &dyn Fn(&str) -> Option<OsString>) -> bool {
    var(ACCOUNT_CLI_ENV)
        .and_then(|v| v.into_string().ok())
        .and_then(|id| super::accounts::Cli::parse(id.trim()))
        .is_some_and(|cli| cli != super::accounts::Cli::Claude)
}

/// The accounts store an opener run finds: Claude Code's home's store (`<store>/logins/<label>`)
/// unless [`ACCOUNT_CLI_ENV`] names another CLI, else the store the opener's link sits in
/// (`<store>/bin/aterm-open-url`, `arg0`).
#[must_use]
pub fn store_dir_of(
    var: &dyn Fn(&str) -> Option<OsString>,
    arg0: Option<&Path>,
) -> Option<PathBuf> {
    if !names_another_cli(var)
        && let Some(claude_dir) = var(super::accounts::HOME_ENV)
    {
        let p = Path::new(&claude_dir);
        if p.parent()
            .and_then(Path::file_name)
            .is_some_and(|n| n == "logins")
            && let Some(store_dir) = p.parent().and_then(Path::parent)
            && store_dir.is_dir()
        {
            return Some(store_dir.to_path_buf());
        }
    }
    let store_dir = arg0?.parent().and_then(Path::parent)?;
    store_dir.is_dir().then(|| store_dir.to_path_buf())
}

fn discover_store_dir() -> Option<PathBuf> {
    let arg0 = std::env::args_os().next().map(PathBuf::from);
    store_dir_of(&|name| std::env::var_os(name), arg0.as_deref())
}

/// Entry point for the `aterm-open-url` argv0 alias.
#[must_use]
pub fn main_entry(args: Vec<OsString>) -> ExitCode {
    let args: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let profile = std::env::var(PROFILE_ENV).ok();
    let email = std::env::var(EMAIL_ENV).ok();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let store_dir = discover_store_dir();
    if let [url] = args.as_slice() {
        let force_headless = std::env::var(super::headless_oauth::HEADLESS_ENV)
            .is_ok_and(|v| matches!(v.trim(), "1" | "true" | "yes"));
        let has_mailbox_env = std::env::var_os(MAILBOX_ENV).is_some_and(|v| !v.is_empty());
        let resolved_email = email
            .as_deref()
            .filter(|e| !e.trim().is_empty())
            .map(str::to_owned)
            .or_else(|| extract_login_hint(url));
        let label = super::headless_oauth::infer_account_label_from_env();
        let mailbox_cfg =
            super::headless_oauth::discover_mailbox_config(home.as_deref(), store_dir.as_deref())
                .map(|mut cfg| {
                    cfg.alias_label_to_email(label.as_deref(), resolved_email.as_deref());
                    cfg
                })
                .filter(|cfg| has_mailbox_env || cfg.is_configured_for(resolved_email.as_deref()));
        if let Some(cfg) = mailbox_cfg {
            match authorize_with_mailbox_config(
                url,
                &cfg,
                store_dir.as_deref(),
                resolved_email.as_deref(),
                AUTHORIZE_TIMEOUT,
            ) {
                Ok(()) => return ExitCode::from(0),
                Err(why) => {
                    if force_headless || profile.as_deref().is_none_or(|p| p.trim().is_empty()) {
                        eprintln!("{OPENER}: not authorized: {why}");
                        return ExitCode::from(1);
                    }
                }
            }
        } else if force_headless || profile.as_deref().is_none_or(|p| p.trim().is_empty()) {
            let has_saved_profile = store_dir.as_deref().is_some_and(|s| {
                super::headless_oauth::has_browser_profile(
                    s,
                    label.as_deref().unwrap_or("default"),
                    resolved_email.as_deref(),
                )
            });
            if force_headless || has_saved_profile {
                match super::headless_oauth::authorize_with_headless_browser_config(
                    url,
                    store_dir.as_deref(),
                    None,
                    resolved_email.as_deref(),
                    AUTHORIZE_TIMEOUT,
                ) {
                    Ok(()) => return ExitCode::from(0),
                    Err(why) => {
                        eprintln!("{OPENER}: not authorized: {why}");
                        return ExitCode::from(1);
                    }
                }
            }
        }
    }
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
