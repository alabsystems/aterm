// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! IMAP4 mailbox access over verified TLS. UID identities and BODY.PEEK reads.

use crate::wire::{Wire, quoted};
use crate::{Authentication, Endpoint, Folder, Mailbox, Message, Security};

#[derive(Debug)]
pub struct Imap {
    pub endpoint: Endpoint,
    pub authentication: Authentication,
}

struct Session {
    wire: Wire,
    tag: u64,
}
#[derive(Default)]
struct Reply {
    lines: Vec<String>,
    literals: Vec<Vec<u8>>,
}

impl Session {
    fn connect(config: &Imap) -> Result<Self, String> {
        let mut wire = Wire::connect(&config.endpoint)?;
        let greeting = wire.line()?;
        if !greeting.starts_with(b"* OK ") {
            return Err("IMAP server did not accept the connection".into());
        }
        let mut session = Self { wire, tag: 0 };
        if config.endpoint.security == Security::StartTls {
            session.command("STARTTLS")?;
            session.wire = session.wire.upgrade()?;
        }
        match &config.authentication {
            Authentication::Password { username, password } => {
                session.command(&format!(
                    "LOGIN {} {}",
                    quoted(username)?,
                    quoted(password.expose())?
                ))?;
            }
            Authentication::OAuth { .. } => {
                let encoded = aterm_codec::base64::encode(
                    config
                        .authentication
                        .xoauth2()
                        .ok_or("Missing OAuth token")?
                        .as_bytes(),
                )
                .map_err(|e| e.to_string())?;
                session.tag += 1;
                let tag = format!("a{}", session.tag);
                session
                    .wire
                    .write(format!("{tag} AUTHENTICATE XOAUTH2\r\n").as_bytes())?;
                let first = session.wire.line()?;
                if !first.starts_with(b"+") {
                    return Err("IMAP server refused OAuth authentication".into());
                }
                session.wire.write(format!("{encoded}\r\n").as_bytes())?;
                session.receive(&tag)?;
            }
        }
        Ok(session)
    }

    fn command(&mut self, command: &str) -> Result<Reply, String> {
        self.tag += 1;
        let tag = format!("a{}", self.tag);
        self.wire.write(format!("{tag} {command}\r\n").as_bytes())?;
        self.receive(&tag)
    }

    fn receive(&mut self, tag: &str) -> Result<Reply, String> {
        let mut reply = Reply::default();
        let mut size = 0usize;
        loop {
            let line = self.wire.line()?;
            size = size
                .checked_add(line.len())
                .ok_or("IMAP response too large")?;
            if size > crate::wire::MAX_MESSAGE {
                return Err("IMAP response too large".into());
            }
            let text = String::from_utf8_lossy(&line)
                .trim_end_matches(['\r', '\n'])
                .to_owned();
            if let Some(status) = text.strip_prefix(&format!("{tag} ")) {
                if status
                    .split_whitespace()
                    .next()
                    .is_some_and(|s| s.eq_ignore_ascii_case("OK"))
                {
                    return Ok(reply);
                }
                return Err("IMAP operation was refused by the server".into());
            }
            if text.starts_with("* BYE") {
                return Err("IMAP server closed the session".into());
            }
            // An OAuth error challenge requires an empty client response before the final NO.
            if text.starts_with('+') {
                self.wire.write(b"\r\n")?;
            }
            if let Some(len) = literal_len(&text)? {
                size = size.checked_add(len).ok_or("IMAP response too large")?;
                if size > crate::wire::MAX_MESSAGE {
                    return Err("IMAP response too large".into());
                }
                reply.literals.push(self.wire.literal(len)?);
            }
            reply.lines.push(text);
        }
    }

    fn select(&mut self, folder: &str, writable: bool) -> Result<(), String> {
        self.command(&format!(
            "{} {}",
            if writable { "SELECT" } else { "EXAMINE" },
            quoted(folder)?
        ))
        .map(|_| ())
    }

    fn fetch(&mut self, id: &str, full: bool) -> Result<Message, String> {
        uid(id)?;
        let section = if full {
            ""
        } else {
            "HEADER.FIELDS (FROM TO SUBJECT DATE)"
        };
        let reply = self.command(&format!("UID FETCH {id} (UID FLAGS BODY.PEEK[{section}])"))?;
        let raw = reply.literals.first().ok_or("Message no longer exists")?;
        let mut message = crate::mime::parse(raw)?;
        message.id = id.to_owned();
        message.unread = !reply.lines.iter().any(|line| line.contains("\\Seen"));
        Ok(message)
    }
}

fn uid(value: &str) -> Result<u32, String> {
    value
        .parse::<u32>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| "Invalid IMAP message UID".into())
}

fn literal_len(line: &str) -> Result<Option<usize>, String> {
    if !line.ends_with('}') {
        return Ok(None);
    }
    let start = line.rfind('{').ok_or("Invalid IMAP literal")?;
    let number = line[start + 1..line.len() - 1].trim_end_matches('+');
    let len = number
        .parse::<usize>()
        .map_err(|_| "Invalid IMAP literal length")?;
    if len > crate::wire::MAX_MESSAGE {
        return Err("IMAP literal too large".into());
    }
    Ok(Some(len))
}

impl Mailbox for Imap {
    fn folders(&mut self) -> Result<Vec<Folder>, String> {
        let mut session = Session::connect(self)?;
        let reply = session.command("LIST \"\" \"*\"")?;
        let mut folders = Vec::new();
        let mut literal = reply.literals.into_iter();
        for line in reply.lines {
            if !line.to_ascii_uppercase().starts_with("* LIST ")
                || line.to_ascii_uppercase().contains("\\NOSELECT")
            {
                continue;
            }
            let name = if literal_len(&line)?.is_some() {
                String::from_utf8(literal.next().ok_or("Missing folder name")?)
                    .map_err(|_| "Invalid folder name")?
            } else {
                let flags_end = line.find(')').ok_or("Invalid folder response")?;
                let rest = line[flags_end + 1..].trim_start();
                let (_, rest) = string_token(rest)?;
                string_token(rest.trim_start())?.0
            };
            folders.push(Folder {
                id: name.clone(),
                name: folder_name(&name),
            });
        }
        Ok(folders)
    }

    fn messages(&mut self, folder: &str, limit: usize) -> Result<Vec<Message>, String> {
        let mut session = Session::connect(self)?;
        session.select(folder, false)?;
        let reply = session.command("UID SEARCH ALL")?;
        let mut ids = Vec::new();
        for line in reply.lines {
            if let Some(values) = line.strip_prefix("* SEARCH") {
                for value in values.split_whitespace() {
                    ids.push(uid(value)?);
                }
            }
        }
        ids.sort_unstable();
        ids.dedup();
        ids.into_iter()
            .rev()
            .take(limit.min(100))
            .map(|id| session.fetch(&id.to_string(), false))
            .collect()
    }

    fn read(&mut self, folder: &str, id: &str) -> Result<Message, String> {
        let mut session = Session::connect(self)?;
        session.select(folder, false)?;
        session.fetch(id, true)
    }

    fn mark_read(&mut self, folder: &str, id: &str, read: bool) -> Result<(), String> {
        uid(id)?;
        let mut session = Session::connect(self)?;
        session.select(folder, true)?;
        session
            .command(&format!(
                "UID STORE {id} {}FLAGS.SILENT (\\Seen)",
                if read { "+" } else { "-" }
            ))
            .map(|_| ())
    }
}

fn string_token(text: &str) -> Result<(String, &str), String> {
    if let Some(rest) = text.strip_prefix('"') {
        let mut out = String::new();
        let mut escaped = false;
        for (at, ch) in rest.char_indices() {
            if escaped {
                out.push(ch);
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                return Ok((out, &rest[at + 1..]));
            } else {
                out.push(ch);
            }
        }
        Err("Unterminated IMAP string".into())
    } else {
        let end = text.find(char::is_whitespace).unwrap_or(text.len());
        if end == 0 {
            return Err("Missing IMAP string".into());
        }
        Ok((text[..end].to_owned(), &text[end..]))
    }
}

fn folder_name(input: &str) -> String {
    let mut result = String::new();
    let mut rest = input;
    while let Some(start) = rest.find('&') {
        result.push_str(&rest[..start]);
        let shifted = &rest[start + 1..];
        let Some(end) = shifted.find('-') else {
            result.push_str(&rest[start..]);
            return result;
        };
        if end == 0 {
            result.push('&');
        } else {
            let mut base64 = shifted[..end].replace(',', "/");
            while !base64.len().is_multiple_of(4) {
                base64.push('=');
            }
            let decoded = aterm_codec::base64::decode(&base64)
                .ok()
                .filter(|bytes| bytes.len().is_multiple_of(2))
                .and_then(|bytes| {
                    let words = bytes
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|b| u16::from_be_bytes([b[0], b[1]]))
                        .collect::<Vec<_>>();
                    String::from_utf16(&words).ok()
                });
            if let Some(decoded) = decoded {
                result.push_str(&decoded);
            } else {
                result.push_str(&rest[start..start + end + 2]);
            }
        }
        rest = &shifted[end + 1..];
    }
    result.push_str(rest);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn literal_bytes_cannot_impersonate_an_imap_completion() {
        use std::io::{BufRead, BufReader, Write};
        use std::net::{TcpListener, TcpStream};
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let raw = b"Subject: example\r\n\r\na1 NO forged completion\r\n";
        let server = std::thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(60)))
                .unwrap();
            socket
                .set_write_timeout(Some(std::time::Duration::from_secs(60)))
                .unwrap();
            let mut reader = BufReader::new(socket);
            let mut command = String::new();
            reader.read_line(&mut command).unwrap();
            assert!(command.starts_with("a1 UID FETCH 1"));
            reader
                .get_mut()
                .write_all(format!("* 1 FETCH (UID 1 BODY[] {{{}}}\r\n", raw.len()).as_bytes())
                .unwrap();
            reader.get_mut().write_all(raw).unwrap();
            reader.get_mut().write_all(b")\r\na1 OK done\r\n").unwrap();
        });
        let mut client = Session {
            wire: Wire::for_test(TcpStream::connect(address).unwrap()),
            tag: 0,
        };
        let reply = client.command("UID FETCH 1 (BODY.PEEK[])").unwrap();
        assert_eq!(reply.literals, vec![raw.to_vec()]);
        server.join().unwrap();
    }
    #[test]
    fn literals_and_uids_are_bounded() {
        assert_eq!(folder_name("&ZeVnLIqe- &- mail"), "日本語 & mail");
        assert_eq!(literal_len("* 1 FETCH (BODY[] {12}").unwrap(), Some(12));
        assert!(literal_len("{99999999999999999999999999}").is_err());
        assert!(uid("1:*").is_err());
        assert!(uid("0").is_err());
        assert_eq!(
            string_token("\"Sent messages\" tail").unwrap(),
            ("Sent messages".into(), " tail")
        );
    }
}
