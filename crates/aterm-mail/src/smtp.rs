// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! SMTP submission with explicit TLS and a single attempt per send.

use crate::wire::Wire;
use crate::{Authentication, Draft, Endpoint, Security};

/// Submit once. A transport failure after DATA is not retried: delivery may have happened.
pub fn send(endpoint: &Endpoint, auth: &Authentication, draft: &Draft) -> Result<(), String> {
    let bytes = encode(draft)?;
    let wire = Wire::connect(endpoint)?;
    submit(wire, endpoint.security, auth, &bytes, draft)
}

fn submit(
    mut wire: Wire,
    security: Security,
    auth: &Authentication,
    bytes: &[u8],
    draft: &Draft,
) -> Result<(), String> {
    expect(&mut wire, 220)?;
    command(&mut wire, "EHLO aterm", 250)?;
    if security == Security::StartTls {
        command(&mut wire, "STARTTLS", 220)?;
        wire = wire.upgrade()?;
        command(&mut wire, "EHLO aterm", 250)?;
    }
    let (mechanism, credentials) = match auth {
        Authentication::Password { .. } => ("PLAIN", auth.plain()),
        Authentication::OAuth { .. } => ("XOAUTH2", auth.xoauth2()),
    };
    let credentials = credentials.ok_or("Missing mail credentials")?;
    let encoded = zeroize::Zeroizing::new(
        aterm_codec::base64::encode(credentials.as_bytes()).map_err(|e| e.to_string())?,
    );
    drop(credentials);
    authenticate(&mut wire, mechanism, &encoded)?;
    drop(encoded);
    let mut cmd = String::with_capacity(64);
    cmd.push_str("MAIL FROM:<");
    cmd.push_str(&draft.from);
    cmd.push('>');
    command(&mut wire, &cmd, 250)?;
    for recipient in draft.to.iter().chain(&draft.cc).chain(&draft.bcc) {
        cmd.clear();
        cmd.push_str("RCPT TO:<");
        cmd.push_str(recipient);
        cmd.push('>');
        command(&mut wire, &cmd, 250)?;
    }
    command(&mut wire, "DATA", 354)?;
    wire.write(bytes)
        .map_err(|_| "Send interrupted; delivery is uncertain. Check Sent before retrying.")?;
    wire.write(b".\r\n")
        .map_err(|_| "Send interrupted; delivery is uncertain. Check Sent before retrying.")?;
    expect(&mut wire, 250)
        .map_err(|_| "No delivery acknowledgement. Check Sent before retrying.".to_string())?;
    // Acceptance, not QUIT, is the delivery boundary.
    let _ = wire.write(b"QUIT\r\n");
    Ok(())
}

fn authenticate(wire: &mut Wire, mechanism: &str, encoded: &str) -> Result<(), String> {
    let inline_len = 5 + mechanism.len() + 1 + encoded.len();
    // RFC 4954 §4: when `AUTH <mech> <initial-response>\r\n` would exceed SMTP's 512-octet command
    // line limit (typical for OAuth2 bearer tokens), send `AUTH <mech>` first and transmit the
    // base64 response after the server's `334` SASL continuation.
    let mut code = if inline_len + 2 <= 512 {
        let mut line = zeroize::Zeroizing::new(Vec::with_capacity(inline_len + 2));
        line.extend_from_slice(b"AUTH ");
        line.extend_from_slice(mechanism.as_bytes());
        line.push(b' ');
        line.extend_from_slice(encoded.as_bytes());
        line.extend_from_slice(b"\r\n");
        wire.write(&line)?;
        read_code(wire)?
    } else {
        command(wire, &format!("AUTH {mechanism}"), 334)?;
        let mut line = zeroize::Zeroizing::new(Vec::with_capacity(encoded.len() + 2));
        line.extend_from_slice(encoded.as_bytes());
        line.extend_from_slice(b"\r\n");
        wire.write(&line)?;
        read_code(wire)?
    };
    if code == 334 {
        wire.write(b"\r\n")?;
        code = read_code(wire)?;
    }
    if code != 235 {
        return Err(format!("Mail server refused operation (SMTP {code})"));
    }
    Ok(())
}

fn command(wire: &mut Wire, text: &str, expected: u16) -> Result<(), String> {
    let bytes = text.as_bytes();
    if bytes.len() + 2 <= 512 {
        let mut buf = [0u8; 512];
        buf[..bytes.len()].copy_from_slice(bytes);
        buf[bytes.len()] = b'\r';
        buf[bytes.len() + 1] = b'\n';
        wire.write(&buf[..bytes.len() + 2])?;
    } else {
        wire.write(format!("{text}\r\n").as_bytes())?;
    }
    expect(wire, expected)
}

fn read_code(wire: &mut Wire) -> Result<u16, String> {
    let mut group_code: Option<u16> = None;
    for _ in 0..100 {
        let line = wire.line()?;
        if line.len() < 5 || !line.ends_with(b"\r\n") || !line[..3].iter().all(u8::is_ascii_digit) {
            return Err("Invalid SMTP response".into());
        }
        let code =
            (line[0] - b'0') as u16 * 100 + (line[1] - b'0') as u16 * 10 + (line[2] - b'0') as u16;
        if let Some(prev) = group_code
            && prev != code
        {
            return Err("Invalid SMTP response".into());
        }
        group_code = Some(code);
        match line[3] {
            b' ' => return Ok(code),
            b'\r' if line.len() == 5 => return Ok(code),
            b'-' => {}
            _ => return Err("Invalid SMTP response".into()),
        }
    }
    Err("SMTP response exceeded line limit".into())
}

fn expect(wire: &mut Wire, expected: u16) -> Result<(), String> {
    let code = read_code(wire)?;
    if code != expected && !(expected == 250 && code == 251) {
        return Err(format!("Mail server refused operation (SMTP {code})"));
    }
    Ok(())
}

pub(crate) fn validate_draft(draft: &Draft) -> Result<(), String> {
    fn address(value: &str) -> bool {
        value.is_ascii()
            && value.split_once('@').is_some_and(|(local, domain)| {
                !local.is_empty() && !domain.is_empty() && !domain.contains('@')
            })
            && !value
                .bytes()
                .any(|b| b <= 32 || b >= 127 || b"<>()\\,;\"".contains(&b))
    }
    if !address(&draft.from)
        || draft
            .to
            .iter()
            .chain(&draft.cc)
            .chain(&draft.bcc)
            .any(|a| !address(a))
    {
        return Err("Enter valid email addresses without display names".into());
    }
    if draft.to.is_empty() && draft.cc.is_empty() && draft.bcc.is_empty() {
        return Err("Add at least one recipient".into());
    }
    if draft.subject.chars().any(char::is_control) {
        return Err("Subject contains a control character".into());
    }
    if draft.subject.len() > 4096 {
        return Err("Subject exceeds 4096 bytes".into());
    }
    if draft.text.len() > 20 * 1024 * 1024 {
        return Err("Message exceeds 20 MiB".into());
    }
    Ok(())
}

/// Encode a plain UTF-8 message with base64 transfer encoding and no Bcc header.
pub fn encode(draft: &Draft) -> Result<Vec<u8>, String> {
    validate_draft(draft)?;
    let mut subject = String::new();
    let mut chunk_start = 0usize;
    let mut chunk_len = 0usize;
    for (idx, ch) in draft.subject.char_indices() {
        let ch_len = ch.len_utf8();
        if chunk_len + ch_len > 36 {
            if !subject.is_empty() {
                subject.push_str("\r\n ");
            }
            subject.push_str("=?UTF-8?B?");
            aterm_codec::base64::encode_into(
                &mut subject,
                &draft.subject.as_bytes()[chunk_start..idx],
            )
            .map_err(|e| e.to_string())?;
            subject.push_str("?=");
            chunk_start = idx;
            chunk_len = 0;
        }
        chunk_len += ch_len;
    }
    if chunk_start < draft.subject.len() {
        if !subject.is_empty() {
            subject.push_str("\r\n ");
        }
        subject.push_str("=?UTF-8?B?");
        aterm_codec::base64::encode_into(&mut subject, &draft.subject.as_bytes()[chunk_start..])
            .map_err(|e| e.to_string())?;
        subject.push_str("?=");
    }
    let raw_text = draft.text.as_bytes();
    let needs_crlf_norm = raw_text.iter().enumerate().any(|(i, &b)| {
        (b == b'\r' && raw_text.get(i + 1) != Some(&b'\n'))
            || (b == b'\n' && (i == 0 || raw_text[i - 1] != b'\r'))
    });
    let normalized: std::borrow::Cow<'_, [u8]> = if needs_crlf_norm {
        let mut buf = Vec::with_capacity(raw_text.len().saturating_add(raw_text.len() / 16));
        let mut i = 0;
        while i < raw_text.len() {
            match raw_text[i] {
                b'\r' => {
                    buf.extend_from_slice(b"\r\n");
                    if raw_text.get(i + 1) == Some(&b'\n') {
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                b'\n' => {
                    buf.extend_from_slice(b"\r\n");
                    i += 1;
                }
                b => {
                    buf.push(b);
                    i += 1;
                }
            }
        }
        std::borrow::Cow::Owned(buf)
    } else {
        std::borrow::Cow::Borrowed(raw_text)
    };
    let now = aterm_time::unix_secs().ok_or("System clock is unavailable")?;
    let mut identifier = [0u8; 16];
    aterm_uds::rand::fill(&mut identifier).map_err(|_| "Could not generate message identity")?;
    let domain = draft.from.split_once('@').ok_or("Invalid sender")?.1;
    let body_b64_len = normalized.len().div_ceil(3) * 4;
    let mut message =
        String::with_capacity(256 + subject.len() + body_b64_len + (body_b64_len / 76) * 2);
    use std::fmt::Write as _;
    message.push_str("Date: ");
    write_mail_date(&mut message, now)?;
    message.push_str("\r\nMessage-ID: <");
    aterm_codec::base64::encode_url_into(&mut message, &identifier).map_err(|e| e.to_string())?;
    let _ = write!(message, "@{domain}>\r\nFrom: {}\r\n", draft.from);
    if !draft.to.is_empty() {
        message.push_str("To: ");
        for (i, addr) in draft.to.iter().enumerate() {
            if i > 0 {
                message.push_str(", ");
            }
            message.push_str(addr);
        }
        message.push_str("\r\n");
    }
    if !draft.cc.is_empty() {
        message.push_str("Cc: ");
        for (i, addr) in draft.cc.iter().enumerate() {
            if i > 0 {
                message.push_str(", ");
            }
            message.push_str(addr);
        }
        message.push_str("\r\n");
    }
    let _ = write!(
        message,
        "Subject: {subject}\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=UTF-8\r\nContent-Transfer-Encoding: base64\r\n\r\n"
    );
    if normalized.is_empty() {
        message.push_str("\r\n");
    } else {
        for chunk in normalized.chunks(57) {
            aterm_codec::base64::encode_into(&mut message, chunk).map_err(|e| e.to_string())?;
            message.push_str("\r\n");
        }
    }
    Ok(message.into_bytes())
}

fn write_mail_date(out: &mut String, seconds: u64) -> Result<(), String> {
    let iso = aterm_types::rfc3339::format_rfc3339(seconds);
    if iso.len() != 20 || !iso.is_ascii() {
        return Err("System clock is outside supported mail dates".into());
    }
    let month = iso[5..7]
        .parse::<usize>()
        .ok()
        .and_then(|n| n.checked_sub(1))
        .filter(|n| *n < 12)
        .ok_or("Invalid calendar month")?;
    let months = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let days = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    use std::fmt::Write as _;
    let _ = write!(
        out,
        "{}, {} {} {} {} +0000",
        days[((seconds / 86400 + 4) % 7) as usize],
        &iso[8..10],
        months[month],
        &iso[..4],
        &iso[11..19]
    );
    Ok(())
}

#[cfg(test)]
fn mail_date(seconds: u64) -> Result<String, String> {
    let mut out = String::with_capacity(31);
    write_mail_date(&mut out, seconds)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn smtp_submission_and_uncertain_acknowledgement() {
        use std::io::{BufRead, BufReader, Write};
        use std::net::{TcpListener, TcpStream};
        for acknowledge in [true, false] {
            let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
            let address = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let (socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(std::time::Duration::from_secs(60)))
                    .unwrap();
                socket
                    .set_write_timeout(Some(std::time::Duration::from_secs(60)))
                    .unwrap();
                let mut io = BufReader::new(socket);
                io.get_mut().write_all(b"220 fixture\r\n").unwrap();
                for (prefix, response) in [
                    ("EHLO ", "250 fixture\r\n"),
                    ("AUTH PLAIN ", "235 authenticated\r\n"),
                    ("MAIL FROM:", "250 accepted\r\n"),
                    ("RCPT TO:", "250 accepted\r\n"),
                    ("RCPT TO:", "250 accepted\r\n"),
                    ("DATA", "354 continue\r\n"),
                ] {
                    let mut line = String::new();
                    io.read_line(&mut line).unwrap();
                    assert!(line.starts_with(prefix));
                    io.get_mut().write_all(response.as_bytes()).unwrap();
                }
                let mut data = String::new();
                loop {
                    let mut line = String::new();
                    assert!(io.read_line(&mut line).unwrap() > 0);
                    if line == ".\r\n" {
                        break;
                    }
                    data.push_str(&line);
                }
                assert!(!data.contains("hidden@example.test"));
                assert!(data.contains("Message-ID: <"));
                if acknowledge {
                    io.get_mut().write_all(b"250 queued\r\n").unwrap();
                }
            });
            let socket = TcpStream::connect(address).unwrap();
            let auth = Authentication::Password {
                username: "me@example.test".into(),
                password: crate::Secret::new("test-only".into()),
            };
            let draft = Draft {
                from: "me@example.test".into(),
                to: vec!["you@example.test".into()],
                bcc: vec!["hidden@example.test".into()],
                subject: "Hello".into(),
                text: "Hello".into(),
                ..Draft::default()
            };
            let result = submit(
                Wire::for_test(socket),
                Security::Tls,
                &auth,
                &encode(&draft).unwrap(),
                &draft,
            );
            if acknowledge {
                assert!(result.is_ok());
            } else {
                assert!(result.unwrap_err().contains("Check Sent"));
            }
            server.join().unwrap();
        }
    }
    #[test]
    fn dates_are_utc_rfc5322() {
        assert_eq!(mail_date(0).unwrap(), "Thu, 01 Jan 1970 00:00:00 +0000");
    }
    #[test]
    fn bcc_is_only_an_envelope_recipient_and_headers_cannot_be_injected() {
        let mut draft = Draft {
            from: "me@example.test".into(),
            to: vec!["you@example.test".into()],
            bcc: vec!["hidden@example.test".into()],
            subject: "Hello".into(),
            text: ".\nHello".into(),
            ..Draft::default()
        };
        let encoded = String::from_utf8(encode(&draft).unwrap()).unwrap();
        assert!(!encoded.contains("hidden@example"));
        assert!(!encoded.contains("\r\n.\r\n"));
        draft.subject = "Hello\r\nBcc: bad@example.test".into();
        assert!(encode(&draft).is_err());

        // Bcc-only / Cc-only drafts omit an empty `To:` header rather than emitting invalid `To: \r\n`.
        let bcc_only = Draft {
            from: "me@example.test".into(),
            bcc: vec!["hidden@example.test".into()],
            subject: "Notice".into(),
            text: "Body".into(),
            ..Draft::default()
        };
        let bcc_encoded = String::from_utf8(encode(&bcc_only).unwrap()).unwrap();
        assert!(!bcc_encoded.contains("\r\nTo:"));
        assert!(!bcc_encoded.contains("hidden@example"));
    }

    #[test]
    fn smtp_two_step_oauth_bare_reply_codes_and_oauth_error_challenge() {
        use std::io::{BufRead, BufReader, Write};
        use std::net::{Ipv4Addr, TcpListener, TcpStream};

        let long_token = "t".repeat(600);
        let auth = Authentication::OAuth {
            username: "me@example.test".into(),
            access_token: crate::Secret::new(long_token),
        };
        let draft = Draft {
            from: "me@example.test".into(),
            bcc: vec!["hidden@example.test".into()],
            subject: "Hello".into(),
            text: "Body".into(),
            ..Draft::default()
        };

        // Long OAuth2 tokens must use two-step `AUTH XOAUTH2` (RFC 4954 §4 <= 512-byte command line),
        // and bare `250\r\n` replies without trailing space/text must succeed (RFC 5321 §4.2).
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(60)))
                .unwrap();
            let mut io = BufReader::new(socket);
            io.get_mut().write_all(b"220\r\n").unwrap();
            let mut line = String::new();
            io.read_line(&mut line).unwrap();
            assert_eq!(line, "EHLO aterm\r\n");
            io.get_mut()
                .write_all(b"250-smtp.example.test\r\n250\r\n")
                .unwrap();
            line.clear();
            io.read_line(&mut line).unwrap();
            assert_eq!(
                line, "AUTH XOAUTH2\r\n",
                "oversized initial-response must split across 334 continuation"
            );
            io.get_mut().write_all(b"334 \r\n").unwrap();
            line.clear();
            io.read_line(&mut line).unwrap();
            assert!(line.len() > 600);
            io.get_mut().write_all(b"235\r\n").unwrap();
            for response in [b"250\r\n".as_slice(), b"250\r\n", b"354\r\n"] {
                line.clear();
                io.read_line(&mut line).unwrap();
                io.get_mut().write_all(response).unwrap();
            }
            loop {
                line.clear();
                assert!(io.read_line(&mut line).unwrap() > 0);
                if line == ".\r\n" {
                    break;
                }
            }
            io.get_mut().write_all(b"250\r\n").unwrap();
        });
        let socket = TcpStream::connect(address).unwrap();
        submit(
            Wire::for_test(socket),
            Security::Tls,
            &auth,
            &encode(&draft).unwrap(),
            &draft,
        )
        .unwrap();
        server.join().unwrap();

        // A `334` OAuth error challenge sends empty `\r\n` to surface the server's real `535` code.
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            let mut io = BufReader::new(socket);
            io.get_mut().write_all(b"220 fixture\r\n").unwrap();
            let mut line = String::new();
            io.read_line(&mut line).unwrap();
            io.get_mut().write_all(b"250 fixture\r\n").unwrap();
            line.clear();
            io.read_line(&mut line).unwrap();
            io.get_mut()
                .write_all(b"334 eyJzdGF0dXMiOiI0MDAifQ==\r\n")
                .unwrap();
            line.clear();
            io.read_line(&mut line).unwrap();
            assert_eq!(line, "\r\n");
            io.get_mut()
                .write_all(b"535 5.7.8 Username and Password not accepted\r\n")
                .unwrap();
        });
        let short_oauth = Authentication::OAuth {
            username: "me@example.test".into(),
            access_token: crate::Secret::new("bad".into()),
        };
        let err = submit(
            Wire::for_test(TcpStream::connect(address).unwrap()),
            Security::Tls,
            &short_oauth,
            &encode(&draft).unwrap(),
            &draft,
        )
        .unwrap_err();
        assert!(err.contains("SMTP 535"), "{err}");
        server.join().unwrap();
    }
}
