// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded MIME-to-text projection. Message HTML is never executed or fetched.

use crate::Message;

pub fn parse(bytes: &[u8]) -> Result<Message, String> {
    if bytes.len() > crate::wire::MAX_MESSAGE {
        return Err("Message exceeds size limit".into());
    }
    let (headers, _) = split(bytes);
    Ok(Message {
        id: String::new(),
        from: header(&headers, "from"),
        to: header(&headers, "to"),
        subject: header(&headers, "subject"),
        date: header(&headers, "date"),
        unread: false,
        text: body(bytes, 0)?,
    })
}

fn split(bytes: &[u8]) -> (Vec<(String, String)>, &[u8]) {
    let (raw, body) = if let Some(at) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
        (&bytes[..at], &bytes[at + 4..])
    } else if let Some(at) = bytes.windows(2).position(|b| b == b"\n\n") {
        (&bytes[..at], &bytes[at + 2..])
    } else {
        (bytes, &[][..])
    };
    let mut headers: Vec<(String, String)> = Vec::new();
    for line in String::from_utf8_lossy(raw).lines() {
        if line.starts_with([' ', '\t']) {
            if let Some((_, value)) = headers.last_mut() {
                value.push(' ');
                value.push_str(line.trim());
            }
        } else if let Some((name, value)) = line.split_once(':') {
            headers.push((name.to_ascii_lowercase(), value.trim().to_owned()));
        }
    }
    (headers, body)
}

fn raw_header<'a>(headers: &'a [(String, String)], name: &str) -> &'a str {
    headers
        .iter()
        .find(|(key, _)| key == name)
        .map_or("", |(_, value)| value)
}

fn header(headers: &[(String, String)], name: &str) -> String {
    words(raw_header(headers, name))
}

fn parameter<'a>(value: &'a str, key: &str) -> Option<&'a str> {
    value.split(';').skip(1).find_map(|part| {
        let (name, value) = part.trim().split_once('=')?;
        name.trim()
            .eq_ignore_ascii_case(key)
            .then(|| value.trim().trim_matches('"'))
    })
}

fn body(bytes: &[u8], depth: usize) -> Result<String, String> {
    if depth >= 16 {
        return Err("MIME nesting exceeds limit".into());
    }
    let (headers, content) = split(bytes);
    let kind = raw_header(&headers, "content-type");
    if raw_header(&headers, "content-disposition")
        .to_ascii_lowercase()
        .starts_with("attachment")
    {
        return Ok(String::new());
    }
    if kind.to_ascii_lowercase().starts_with("multipart/") {
        let boundary = parameter(kind, "boundary").ok_or("Multipart message has no boundary")?;
        if boundary.is_empty() || boundary.len() > 200 || boundary.chars().any(char::is_control) {
            return Err("Invalid MIME boundary".into());
        }
        let delimiter = format!("--{boundary}");
        let mut parts = Vec::new();
        let mut start = None;
        let mut offset = 0usize;
        for line in content.split_inclusive(|b| *b == b'\n') {
            let line_text = String::from_utf8_lossy(line);
            let trimmed = line_text.trim_end_matches(['\r', '\n', ' ', '\t']);
            if trimmed == delimiter || trimmed == format!("{delimiter}--") {
                if let Some(at) = start.take() {
                    parts.push(&content[at..offset]);
                }
                if trimmed.ends_with("--") && trimmed != delimiter {
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
            let (part_headers, _) = split(part);
            let text = body(part, depth + 1)?;
            if raw_header(&part_headers, "content-type")
                .to_ascii_lowercase()
                .starts_with("text/plain")
                && !text.is_empty()
            {
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
    if !kind.is_empty() && !kind.to_ascii_lowercase().starts_with("text/") {
        return Ok(String::new());
    }
    let transfer = raw_header(&headers, "content-transfer-encoding");
    let decoded = if transfer.eq_ignore_ascii_case("base64") {
        let compact: Vec<u8> = content
            .iter()
            .copied()
            .filter(|b| !b.is_ascii_whitespace())
            .collect();
        aterm_codec::base64::decode(
            std::str::from_utf8(&compact).map_err(|_| "Invalid MIME base64")?,
        )
        .map_err(|_| "Invalid MIME base64")?
    } else if transfer.eq_ignore_ascii_case("quoted-printable") {
        quoted_printable(content, false)
    } else {
        content.to_vec()
    };
    let text = charset(&decoded, parameter(kind, "charset").unwrap_or("utf-8"));
    Ok(if kind.to_ascii_lowercase().starts_with("text/html") {
        html_text(&text)
    } else {
        text
    })
}

fn charset(bytes: &[u8], name: &str) -> String {
    if name.eq_ignore_ascii_case("iso-8859-1") {
        bytes.iter().map(|b| char::from(*b)).collect()
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

fn quoted_printable(bytes: &[u8], word: bool) -> Vec<u8> {
    let mut out = Vec::new();
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

pub fn words(mut input: &str) -> String {
    let mut output = String::new();
    while let Some(start) = input.find("=?") {
        output.push_str(&input[..start]);
        let rest = &input[start + 2..];
        let Some((encoding, rest)) = rest.split_once('?') else {
            output.push_str(&input[start..]);
            return output;
        };
        let Some((mode, rest)) = rest.split_once('?') else {
            output.push_str(&input[start..]);
            return output;
        };
        let Some((data, remaining)) = rest.split_once("?=") else {
            output.push_str(&input[start..]);
            return output;
        };
        let decoded = if mode.eq_ignore_ascii_case("b") {
            aterm_codec::base64::decode(data).ok()
        } else if mode.eq_ignore_ascii_case("q") {
            Some(quoted_printable(data.as_bytes(), true))
        } else {
            None
        };
        if let Some(decoded) = decoded {
            output.push_str(&charset(&decoded, encoding));
        } else {
            output.push_str(&input[start..input.len() - remaining.len()]);
        }
        input = if remaining.trim_start().starts_with("=?") {
            remaining.trim_start()
        } else {
            remaining
        };
    }
    output.push_str(input);
    output
}

pub fn html_text(html: &str) -> String {
    let mut output = String::new();
    let mut in_tag = false;
    let mut tag = String::new();
    let mut hidden = None;
    for ch in html.chars() {
        if ch == '<' {
            in_tag = true;
            tag.clear();
        } else if ch == '>' && in_tag {
            in_tag = false;
            let token = tag
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            match token.as_str() {
                "script" | "style" | "head" => hidden = Some(token.clone()),
                "/script" | "/style" | "/head"
                    if hidden.as_deref() == Some(token.trim_start_matches('/')) =>
                {
                    hidden = None
                }
                "br" | "br/" | "/p" | "/div" | "/li" | "/tr" if hidden.is_none() => {
                    output.push('\n')
                }
                _ => {}
            }
        } else if in_tag {
            tag.push(ch);
        } else if hidden.is_none() {
            output.push(ch);
        }
    }
    output
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prefers_plain_part_and_decodes_headers_without_loading_html() {
        let raw = b"Subject: =?UTF-8?Q?Hello_=E2=9C=93?=\r\nContent-Type: multipart/alternative; boundary=x\r\n\r\n--x\r\nContent-Type: text/html\r\n\r\n<script>bad()</script><p>html</p>\r\n--x\r\nContent-Type: text/plain\r\nContent-Transfer-Encoding: base64\r\n\r\ncGxhaW4=\r\n--x--\r\n";
        let message = parse(raw).unwrap();
        assert_eq!(message.subject, "Hello ✓");
        assert_eq!(message.text, "plain");
        assert_eq!(
            html_text("<script>secret</script><p>hello &amp; goodbye</p>"),
            "hello & goodbye\n"
        );
    }
}
