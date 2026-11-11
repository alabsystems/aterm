// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded MIME-to-text projection. Message HTML is never executed or fetched.

use crate::Message;

pub fn parse(bytes: &[u8]) -> Result<Message, String> {
    if bytes.len() > crate::wire::MAX_MESSAGE {
        return Err("Message exceeds size limit".into());
    }
    let (headers, content) = split(bytes);
    let from = header(&headers, "from");
    let to = header(&headers, "to");
    let subject = header(&headers, "subject");
    let date = header(&headers, "date");
    Ok(Message {
        id: String::new(),
        from,
        to,
        subject,
        date,
        unread: false,
        text: body_with_split(&headers, content, 0)?,
    })
}

fn canonical_header(name: &str) -> Option<&'static str> {
    [
        "from",
        "to",
        "subject",
        "date",
        "content-type",
        "content-disposition",
        "content-transfer-encoding",
    ]
    .into_iter()
    .find(|wanted| name.eq_ignore_ascii_case(wanted))
}

fn split(bytes: &[u8]) -> (Vec<(&'static str, String)>, &[u8]) {
    let (raw, body) = if let Some(body) = bytes.strip_prefix(b"\r\n") {
        (&[][..], body)
    } else if let Some(body) = bytes.strip_prefix(b"\n") {
        (&[][..], body)
    } else if let Some(at) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
        (&bytes[..at], &bytes[at + 4..])
    } else if let Some(at) = bytes.windows(2).position(|b| b == b"\n\n") {
        (&bytes[..at], &bytes[at + 2..])
    } else {
        (bytes, &[][..])
    };
    let mut headers: Vec<(&'static str, String)> = Vec::new();
    let mut keep_last = false;
    for line in String::from_utf8_lossy(raw).lines() {
        if line.starts_with([' ', '\t']) {
            if keep_last && let Some((_, value)) = headers.last_mut() {
                value.push(' ');
                value.push_str(line.trim());
            }
        } else if let Some((name, value)) = line.split_once(':') {
            if let Some(canonical) = canonical_header(name) {
                keep_last = true;
                headers.push((canonical, value.trim().to_owned()));
            } else {
                keep_last = false;
            }
        }
    }
    (headers, body)
}

fn raw_header<'a>(headers: &'a [(&'static str, String)], name: &str) -> &'a str {
    headers
        .iter()
        .find(|(key, _)| *key == name)
        .map_or("", |(_, value)| value)
}

fn header(headers: &[(&'static str, String)], name: &str) -> String {
    words(raw_header(headers, name))
}

fn media_type(value: &str) -> &str {
    value
        .split(';')
        .next()
        .unwrap_or("")
        .split('(')
        .next()
        .unwrap_or("")
        .trim()
}

fn parameter<'a>(value: &'a str, key: &str) -> Option<&'a str> {
    value.split(';').skip(1).find_map(|part| {
        let (name, raw) = part.trim().split_once('=')?;
        if !name.trim().eq_ignore_ascii_case(key) {
            return None;
        }
        let raw = raw.trim();
        if let Some(quoted) = raw.strip_prefix('"') {
            quoted.split_once('"').map(|(inner, _)| inner)
        } else {
            Some(
                raw.split([' ', '\t', '('])
                    .next()
                    .unwrap_or("")
                    .trim_matches('"'),
            )
        }
    })
}

fn body(bytes: &[u8], depth: usize) -> Result<String, String> {
    let (headers, content) = split(bytes);
    body_with_split(&headers, content, depth)
}

fn body_with_split(
    headers: &[(&'static str, String)],
    content: &[u8],
    depth: usize,
) -> Result<String, String> {
    if depth >= 16 {
        return Err("MIME nesting exceeds limit".into());
    }
    let kind = raw_header(headers, "content-type");
    let media = media_type(kind);
    if media_type(raw_header(headers, "content-disposition")).eq_ignore_ascii_case("attachment") {
        return Ok(String::new());
    }
    if media
        .get(..10)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("multipart/"))
    {
        let is_alternative = media.eq_ignore_ascii_case("multipart/alternative");
        let is_digest = media.eq_ignore_ascii_case("multipart/digest");
        let boundary = parameter(kind, "boundary").ok_or("Multipart message has no boundary")?;
        if boundary.is_empty() || boundary.len() > 200 || boundary.chars().any(char::is_control) {
            return Err("Invalid MIME boundary".into());
        }
        let mut close_buf = [0u8; 204];
        let close_len = boundary.len() + 4;
        close_buf[..2].copy_from_slice(b"--");
        close_buf[2..2 + boundary.len()].copy_from_slice(boundary.as_bytes());
        close_buf[2 + boundary.len()..close_len].copy_from_slice(b"--");
        let close_bytes = &close_buf[..close_len];
        let delimiter_bytes = &close_bytes[..close_len - 2];
        let mut parts = Vec::new();
        let mut start = None;
        let mut offset = 0usize;
        for line in content.split_inclusive(|b| *b == b'\n') {
            let mut trimmed = line;
            while let Some((&last, head)) = trimmed.split_last() {
                if matches!(last, b'\r' | b'\n' | b' ' | b'\t') {
                    trimmed = head;
                } else {
                    break;
                }
            }
            if trimmed == delimiter_bytes || trimmed == close_bytes {
                if let Some(at) = start.take() {
                    let raw_part = &content[at..offset];
                    let part = raw_part
                        .strip_suffix(b"\r\n")
                        .or_else(|| raw_part.strip_suffix(b"\n"))
                        .unwrap_or(raw_part);
                    parts.push(part);
                }
                if trimmed == close_bytes {
                    break;
                }
                start = Some(offset + line.len());
            }
            offset += line.len();
        }
        if let Some(at) = start {
            parts.push(&content[at..]);
        }
        let mut fallback = String::new();
        for part in parts.into_iter().take(256) {
            let (part_headers, part_content) = split(part);
            let part_media = media_type(raw_header(&part_headers, "content-type"));
            let is_plain_or_untyped =
                part_media.is_empty() || part_media.eq_ignore_ascii_case("text/plain");
            let text = if is_digest && part_media.is_empty() {
                // RFC 2046 §5.1.5: untyped parts in `multipart/digest` are `message/rfc822`,
                // whose inner payload is another MIME message after the part's own headers.
                body(part_content, depth + 1)?
            } else {
                body_with_split(&part_headers, part_content, depth + 1)?
            };
            if is_alternative && is_plain_or_untyped && !text.is_empty() {
                return Ok(text);
            }
            if !text.is_empty() {
                if !fallback.is_empty() {
                    fallback.push('\n');
                }
                fallback.push_str(&text);
            }
        }
        return Ok(fallback);
    }
    if !media.is_empty()
        && !media
            .get(..5)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("text/"))
    {
        return Ok(String::new());
    }
    let transfer = media_type(raw_header(headers, "content-transfer-encoding")).trim_matches('"');
    let decoded = if transfer.eq_ignore_ascii_case("base64") {
        let compact: std::borrow::Cow<'_, [u8]> = if content.iter().any(u8::is_ascii_whitespace) {
            let mut buf = Vec::with_capacity(content.len());
            buf.extend(content.iter().copied().filter(|b| !b.is_ascii_whitespace()));
            std::borrow::Cow::Owned(buf)
        } else {
            std::borrow::Cow::Borrowed(content)
        };
        std::borrow::Cow::Owned(
            aterm_codec::base64::decode(
                std::str::from_utf8(&compact).map_err(|_| "Invalid MIME base64")?,
            )
            .map_err(|_| "Invalid MIME base64")?,
        )
    } else if transfer.eq_ignore_ascii_case("quoted-printable") {
        std::borrow::Cow::Owned(quoted_printable(content, false))
    } else {
        std::borrow::Cow::Borrowed(content)
    };
    let text = charset(decoded, parameter(kind, "charset").unwrap_or("utf-8"));
    Ok(if media.eq_ignore_ascii_case("text/html") {
        html_text(&text)
    } else {
        text
    })
}

fn charset(bytes: std::borrow::Cow<'_, [u8]>, name: &str) -> String {
    let base = name.split('*').next().unwrap_or(name);
    if base.eq_ignore_ascii_case("iso-8859-1") || base.eq_ignore_ascii_case("latin1") {
        bytes.iter().map(|b| char::from(*b)).collect()
    } else {
        match bytes {
            std::borrow::Cow::Owned(vec) => String::from_utf8(vec)
                .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned()),
            std::borrow::Cow::Borrowed(slice) => String::from_utf8_lossy(slice).into_owned(),
        }
    }
}

fn push_charset(output: &mut String, bytes: Vec<u8>, name: &str) {
    let base = name.split('*').next().unwrap_or(name);
    if base.eq_ignore_ascii_case("iso-8859-1") || base.eq_ignore_ascii_case("latin1") {
        output.extend(bytes.into_iter().map(char::from));
    } else if output.is_empty() {
        *output = match String::from_utf8(bytes) {
            Ok(s) => s,
            Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
        };
    } else {
        output.push_str(&String::from_utf8_lossy(&bytes));
    }
}

fn quoted_printable(bytes: &[u8], word: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'=' {
            if bytes.get(at + 1..at + 3) == Some(b"\r\n") {
                at += 3;
                continue;
            }
            if bytes.get(at + 1) == Some(&b'\n') {
                at += 2;
                continue;
            }
            if let Some(pair) = bytes.get(at + 1..at + 3)
                && let (Some(hi), Some(lo)) = (
                    char::from(pair[0]).to_digit(16),
                    char::from(pair[1]).to_digit(16),
                )
            {
                out.push((hi * 16 + lo) as u8);
                at += 3;
                continue;
            }
        }
        out.push(if word && bytes[at] == b'_' {
            b' '
        } else {
            bytes[at]
        });
        at += 1;
    }
    out
}

fn parse_encoded_word(after_open: &str) -> Option<(&str, Vec<u8>, &str)> {
    let (encoding, rest) = after_open.split_once('?')?;
    if encoding.is_empty() || encoding.as_bytes().iter().any(u8::is_ascii_whitespace) {
        return None;
    }
    let (mode, rest) = rest.split_once('?')?;
    let (data, remaining) = rest.split_once("?=")?;
    if data.bytes().any(|b| b.is_ascii_whitespace() || b == b'?') {
        return None;
    }
    let decoded = if mode.eq_ignore_ascii_case("b") {
        aterm_codec::base64::decode(data).ok()?
    } else if mode.eq_ignore_ascii_case("q") {
        quoted_printable(data.as_bytes(), true)
    } else {
        return None;
    };
    let base = encoding.split('*').next().unwrap_or(encoding);
    Some((base, decoded, remaining))
}

pub fn words(mut input: &str) -> String {
    let Some(mut first_start) = input.find("=?") else {
        return input.to_owned();
    };
    let mut output = String::with_capacity(input.len());
    let mut pending: Option<(&str, Vec<u8>)> = None;
    let flush = |output: &mut String, pending: &mut Option<(&str, Vec<u8>)>| {
        if let Some((enc, bytes)) = pending.take() {
            push_charset(output, bytes, enc);
        }
    };
    let mut predecoded: Option<(&str, Vec<u8>, &str)> = None;
    loop {
        let (enc, decoded, remaining) = if let Some(next) = predecoded.take() {
            next
        } else {
            let start = first_start;
            if start > 0 {
                flush(&mut output, &mut pending);
                output.push_str(&input[..start]);
            }
            let rest = &input[start + 2..];
            let Some(parsed) = parse_encoded_word(rest) else {
                flush(&mut output, &mut pending);
                output.push_str("=?");
                input = rest;
                let Some(next_start) = input.find("=?") else {
                    break;
                };
                first_start = next_start;
                continue;
            };
            parsed
        };
        match &mut pending {
            Some((prev_enc, bytes)) if prev_enc.eq_ignore_ascii_case(enc) => {
                bytes.extend_from_slice(&decoded);
            }
            _ => {
                flush(&mut output, &mut pending);
                pending = Some((enc, decoded));
            }
        }
        let trimmed = remaining.trim_start_matches([' ', '\t', '\r', '\n']);
        if let Some(after_open) = trimmed.strip_prefix("=?")
            && let Some(next_parsed) = parse_encoded_word(after_open)
        {
            predecoded = Some(next_parsed);
            input = trimmed;
            continue;
        }
        flush(&mut output, &mut pending);
        input = remaining;
        let Some(next_start) = input.find("=?") else {
            break;
        };
        first_start = next_start;
    }
    flush(&mut output, &mut pending);
    output.push_str(input);
    output
}

pub fn html_text(html: &str) -> String {
    let mut output = String::with_capacity(html.len());
    let mut hidden: Vec<&'static str> = Vec::new();
    let mut cursor = html;
    while !cursor.is_empty() {
        if let Some(after_comment_open) = cursor.strip_prefix("<!--") {
            cursor = after_comment_open
                .split_once("-->")
                .map_or("", |(_, rest)| rest);
            continue;
        }
        if let Some(after_lt) = cursor.strip_prefix('<')
            && let Some((raw_tag, rest)) = after_lt.split_once('>')
        {
            cursor = rest;
            let trimmed_tag = raw_tag.trim();
            let self_closing = trimmed_tag.ends_with('/');
            let token = trimmed_tag
                .trim_end_matches('/')
                .split_whitespace()
                .next()
                .unwrap_or("");
            if !self_closing {
                if token.eq_ignore_ascii_case("script") {
                    hidden.push("script");
                    continue;
                } else if token.eq_ignore_ascii_case("style") {
                    hidden.push("style");
                    continue;
                } else if token.eq_ignore_ascii_case("head") {
                    hidden.push("head");
                    continue;
                }
            }
            if let Some(closing) = token.strip_prefix('/')
                && (closing.eq_ignore_ascii_case("script")
                    || closing.eq_ignore_ascii_case("style")
                    || closing.eq_ignore_ascii_case("head"))
            {
                if let Some(pos) = hidden
                    .iter()
                    .rposition(|open| open.eq_ignore_ascii_case(closing))
                {
                    hidden.truncate(pos);
                }
                continue;
            }
            if hidden.is_empty()
                && (token.eq_ignore_ascii_case("br")
                    || token.eq_ignore_ascii_case("/p")
                    || token.eq_ignore_ascii_case("/div")
                    || token.eq_ignore_ascii_case("/li")
                    || token.eq_ignore_ascii_case("/tr"))
            {
                output.push('\n');
            }
            continue;
        }
        let span = if cursor.starts_with('<') {
            1
        } else {
            cursor.find('<').unwrap_or(cursor.len())
        };
        if hidden.is_empty() {
            output.push_str(&cursor[..span]);
        }
        cursor = &cursor[span..];
    }
    let decoded = if output.contains('&') {
        let mut decoded = String::with_capacity(output.len());
        let mut rest = output.as_str();
        while let Some(start) = rest.find('&') {
            decoded.push_str(&rest[..start]);
            rest = &rest[start..];
            let entity = rest.get(1..).and_then(|tail| {
                tail.bytes()
                    .take(33)
                    .position(|byte| byte == b';')
                    .map(|end| &tail[..end])
            });
            if let Some((entity, ch)) =
                entity.and_then(|entity| html_entity(entity).map(|ch| (entity, ch)))
            {
                decoded.push(ch);
                rest = &rest[entity.len() + 2..];
            } else {
                decoded.push('&');
                rest = &rest[1..];
            }
        }
        decoded.push_str(rest);
        decoded
    } else {
        output
    };
    let mut text = String::with_capacity(decoded.len());
    for line in decoded.lines() {
        let mut line_started = false;
        let mut need_space = false;
        for ch in line.chars() {
            if matches!(ch, '\u{ad}' | '\u{34f}' | '\u{200b}' | '\u{feff}') {
                continue;
            }
            if ch.is_whitespace() {
                if line_started {
                    need_space = true;
                }
            } else {
                if !line_started {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    line_started = true;
                } else if need_space {
                    text.push(' ');
                }
                need_space = false;
                text.push(ch);
            }
        }
    }
    if decoded.ends_with('\n') && !text.is_empty() {
        text.push('\n');
    }
    text
}

fn html_entity(entity: &str) -> Option<char> {
    match entity {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some(' '),
        "shy" => Some('\u{ad}'),
        "ndash" => Some('–'),
        "mdash" => Some('—'),
        "lsquo" => Some('‘'),
        "rsquo" => Some('’'),
        "ldquo" => Some('“'),
        "rdquo" => Some('”'),
        "hellip" => Some('…'),
        "copy" => Some('©'),
        "reg" => Some('®'),
        _ => {
            let (digits, radix) = if let Some(digits) = entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
            {
                (digits, 16)
            } else {
                (entity.strip_prefix('#')?, 10)
            };
            if digits.is_empty()
                || !digits.bytes().all(|b| {
                    if radix == 16 {
                        b.is_ascii_hexdigit()
                    } else {
                        b.is_ascii_digit()
                    }
                })
            {
                return None;
            }
            u32::from_str_radix(digits, radix)
                .ok()
                .and_then(char::from_u32)
                .filter(|ch| *ch != '\0')
        }
    }
}

#[cfg(test)]
#[path = "mime_tests.rs"]
mod tests;
