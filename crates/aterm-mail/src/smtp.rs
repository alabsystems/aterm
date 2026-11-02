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
    let encoded =
        aterm_codec::base64::encode(credentials.ok_or("Missing mail credentials")?.as_bytes())
            .map_err(|e| e.to_string())?;
    command(&mut wire, &format!("AUTH {mechanism} {encoded}"), 235)?;
    command(&mut wire, &format!("MAIL FROM:<{}>", draft.from), 250)?;
    for recipient in draft.to.iter().chain(&draft.cc).chain(&draft.bcc) {
        command(&mut wire, &format!("RCPT TO:<{recipient}>"), 250)?;
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

fn command(wire: &mut Wire, text: &str, expected: u16) -> Result<(), String> {
    wire.write(format!("{text}\r\n").as_bytes())?;
    expect(wire, expected)
}

fn expect(wire: &mut Wire, expected: u16) -> Result<(), String> {
    for _ in 0..100 {
        let line = wire.line()?;
        if line.len() < 5 || !line[..3].iter().all(u8::is_ascii_digit) {
            return Err("Invalid SMTP response".into());
        }
        let code =
            (line[0] - b'0') as u16 * 100 + (line[1] - b'0') as u16 * 10 + (line[2] - b'0') as u16;
        if code != expected && !(expected == 250 && code == 251) {
            return Err(format!("Mail server refused operation (SMTP {code})"));
        }
        match line[3] {
            b' ' => return Ok(()),
            b'-' => {}
            _ => return Err("Invalid SMTP response".into()),
        }
    }
    Err("SMTP response exceeded line limit".into())
}

/// Encode a plain UTF-8 message with base64 transfer encoding and no Bcc header.
pub fn encode(draft: &Draft) -> Result<Vec<u8>, String> {
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
    let mut subject = String::new();
    let mut part = String::new();
    for ch in draft.subject.chars() {
        if part.len() + ch.len_utf8() > 36 {
            if !subject.is_empty() {
                subject.push_str("\r\n ");
            }
            subject.push_str(&format!(
                "=?UTF-8?B?{}?=",
                aterm_codec::base64::encode(part.as_bytes()).map_err(|e| e.to_string())?
            ));
            part.clear();
        }
        part.push(ch);
    }
    if !part.is_empty() {
        if !subject.is_empty() {
            subject.push_str("\r\n ");
        }
        subject.push_str(&format!(
            "=?UTF-8?B?{}?=",
            aterm_codec::base64::encode(part.as_bytes()).map_err(|e| e.to_string())?
        ));
    }
    let normalized = draft
        .text
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\n', "\r\n");
    let body = aterm_codec::base64::encode(normalized.as_bytes()).map_err(|e| e.to_string())?;
    let now = aterm_time::unix_secs().ok_or("System clock is unavailable")?;
    let date = mail_date(now)?;
    let mut identifier = [0u8; 16];
    aterm_uds::rand::fill(&mut identifier).map_err(|_| "Could not generate message identity")?;
    let identifier = aterm_codec::base64::encode(&identifier)
        .map_err(|e| e.to_string())?
        .trim_end_matches('=')
        .replace('+', "-")
        .replace('/', "_");
    let domain = draft.from.split_once('@').ok_or("Invalid sender")?.1;
    let mut message = format!(
        "Date: {date}\r\nMessage-ID: <{identifier}@{domain}>\r\nFrom: {}\r\nTo: {}\r\n",
        draft.from,
        draft.to.join(", ")
    );
    if !draft.cc.is_empty() {
        message.push_str(&format!("Cc: {}\r\n", draft.cc.join(", ")));
    }
    message.push_str(&format!("Subject: {subject}\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=UTF-8\r\nContent-Transfer-Encoding: base64\r\n\r\n"));
    for line in body.as_bytes().chunks(76) {
        message.push_str(std::str::from_utf8(line).map_err(|_| "Invalid encoded body")?);
        message.push_str("\r\n");
    }
    if body.is_empty() {
        message.push_str("\r\n");
    }
    Ok(message.into_bytes())
}

fn mail_date(seconds: u64) -> Result<String, String> {
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
    Ok(format!(
        "{}, {} {} {} {} +0000",
        days[((seconds / 86400 + 4) % 7) as usize],
        &iso[8..10],
        months[month],
        &iso[..4],
        &iso[11..19]
    ))
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
    }
}
