// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! IMAP4 mailbox access over verified TLS. UID identities and BODY.PEEK reads.

use crate::wire::{Wire, quoted};
use crate::{Authentication, Endpoint, Folder, Mailbox, Message, Security};

#[derive(Clone, Debug)]
pub struct Imap {
    pub endpoint: Endpoint,
    pub authentication: Authentication,
}

/// Authentication rejection is permanent until credentials change; transport failures are not.
#[derive(Debug)]
pub enum ConnectError {
    Authentication,
    Other(String),
}
impl From<String> for ConnectError {
    fn from(error: String) -> Self {
        Self::Other(error)
    }
}
impl From<&str> for ConnectError {
    fn from(error: &str) -> Self {
        Self::Other(error.into())
    }
}
impl From<ConnectError> for String {
    fn from(error: ConnectError) -> Self {
        match error {
            ConnectError::Authentication => "IMAP authentication was refused".into(),
            ConnectError::Other(error) => error,
        }
    }
}
/// A read rejected locally, absent remotely, or interrupted by a transport/protocol failure.
#[derive(Debug, PartialEq, Eq)]
pub enum ReadError {
    InvalidId,
    Missing,
    Other(String),
}
impl ReadError {
    pub fn retryable(&self) -> bool {
        matches!(self, Self::Other(_))
    }
}
impl From<String> for ReadError {
    fn from(error: String) -> Self {
        Self::Other(error)
    }
}
impl From<ReadError> for String {
    fn from(error: ReadError) -> Self {
        match error {
            ReadError::InvalidId => "Invalid IMAP message UID".into(),
            ReadError::Missing => "Message no longer exists".into(),
            ReadError::Other(message) => message,
        }
    }
}

/// A read-only folder selection rejected for an invalid/unknown folder or interrupted by a wire failure.
#[derive(Debug, PartialEq, Eq)]
pub enum SelectError {
    UnknownFolder,
    Other(String),
}
impl SelectError {
    pub fn retryable(&self) -> bool {
        matches!(self, Self::Other(_))
    }
}
impl From<SelectError> for String {
    fn from(error: SelectError) -> Self {
        match error {
            SelectError::UnknownFolder => "Unknown mail folder".into(),
            SelectError::Other(message) => message,
        }
    }
}

enum ReceiveError {
    Status(String),
    Other(String),
}

impl From<String> for ReceiveError {
    fn from(error: String) -> Self {
        Self::Other(error)
    }
}

impl From<&str> for ReceiveError {
    fn from(error: &str) -> Self {
        Self::Other(error.into())
    }
}

fn transient_no_status(status: &str) -> bool {
    let bytes = status.as_bytes();
    [
        b"[unavailable]".as_slice(),
        b"[inuse]",
        b"[serverbug]",
        b"[limit]",
        b"[overquota]",
        b"[clientbug]",
        b"too many simultaneous connections",
        b"maximum number of connections",
        b"temporary system problem",
        b"temporarily unavailable",
        b"server busy",
        b"try again later",
    ]
    .iter()
    .any(|needle| {
        bytes
            .windows(needle.len())
            .any(|w| w.eq_ignore_ascii_case(needle))
    })
}

fn auth_status_error(status: &str) -> ConnectError {
    let mut words = status.split_whitespace();
    let Some(kind) = words.next() else {
        return ConnectError::Other("IMAP operation was refused by the server".into());
    };
    if !kind.eq_ignore_ascii_case("NO") {
        return ConnectError::Other("IMAP operation was refused by the server".into());
    }
    if transient_no_status(status) {
        ConnectError::Other("IMAP authentication temporarily unavailable".into())
    } else {
        ConnectError::Authentication
    }
}

fn select_status_error(status: &str) -> SelectError {
    let mut words = status.split_whitespace();
    let Some(kind) = words.next() else {
        return SelectError::Other("IMAP operation was refused by the server".into());
    };
    if !kind.eq_ignore_ascii_case("NO") || transient_no_status(status) {
        SelectError::Other("IMAP operation was refused by the server".into())
    } else {
        SelectError::UnknownFolder
    }
}

/// An authenticated, read-only folder session reused across reads and notifications.
pub struct Reader {
    session: Session,
    idle: bool,
}

impl Imap {
    pub fn open(&self, folder: &str) -> Result<Reader, ConnectError> {
        self.open_by(folder, None)
    }

    /// Open a reader whose connection and every subsequent operation share one deadline.
    pub fn open_until(
        &self,
        folder: &str,
        end: std::time::Instant,
    ) -> Result<Reader, ConnectError> {
        self.open_by(folder, Some(end))
    }

    /// Bound connection setup while retaining the authenticated session for future operations.
    pub fn open_with_connect_timeout(
        &self,
        folder: &str,
        within: std::time::Duration,
    ) -> Result<Reader, ConnectError> {
        let end = std::time::Instant::now()
            .checked_add(within)
            .ok_or("Invalid mail connection timeout")?;
        let mut reader = self.open_until(folder, end)?;
        reader.session.wire.release_overall_deadline();
        Ok(reader)
    }

    fn open_by(
        &self,
        folder: &str,
        end: Option<std::time::Instant>,
    ) -> Result<Reader, ConnectError> {
        let mut session = Session::connect_until(self, end)?;
        let capabilities = session.command("CAPABILITY")?;
        let idle = capabilities
            .lines
            .iter()
            .filter(|line| {
                line.get(..13)
                    .is_some_and(|p| p.eq_ignore_ascii_case("* CAPABILITY "))
            })
            .flat_map(|line| line.split_whitespace())
            .any(|word| word.eq_ignore_ascii_case("IDLE"));
        session.select(folder, false)?;
        session.changed = false;
        Ok(Reader { session, idle })
    }
}

impl Reader {
    /// Bound one operation without replacing an existing shorter session deadline.
    pub fn run_until<T>(
        &mut self,
        end: std::time::Instant,
        operation: impl FnOnce(&mut Self) -> T,
    ) -> T {
        let previous = self.session.wire.limit_until(end);
        let result = operation(self);
        self.session.wire.restore_limit(previous);
        result
    }

    /// Change the selected folder without obtaining a writable mailbox session.
    pub fn select_read_only(&mut self, folder: &str) -> Result<(), String> {
        self.try_select_read_only(folder).map_err(String::from)
    }

    /// Change the selected folder while distinguishing nonexistent folders from wire failures.
    pub fn try_select_read_only(&mut self, folder: &str) -> Result<(), SelectError> {
        self.session.try_select(folder, false)?;
        self.session.changed = false;
        Ok(())
    }

    /// List selectable folders through the existing authenticated connection.
    pub fn folders(&mut self) -> Result<Vec<Folder>, String> {
        let reply = self.session.command("LIST \"\" \"*\"")?;
        let mut folders = Vec::new();
        let mut literal = reply.literals.into_iter();
        for line in reply.lines {
            let line_literal = if literal_len(&line)?.is_some() {
                Some(literal.next().ok_or("Missing folder name")?)
            } else {
                None
            };
            let Some(after_list) = line
                .get(..7)
                .filter(|prefix| prefix.eq_ignore_ascii_case("* LIST "))
                .map(|_| line[7..].trim_start())
            else {
                continue;
            };
            let rest = after_list
                .strip_prefix('(')
                .ok_or("Invalid folder response")?;
            let flags_end = rest.find(')').ok_or("Invalid folder response")?;
            let flags = &rest[..flags_end];
            if flag_list_contains(flags, "\\Noselect") || flag_list_contains(flags, "\\NonExistent")
            {
                continue;
            }
            let rest = rest[flags_end + 1..].trim_start();
            let (_, rest) = string_token(rest)?;
            let name = if let Some(bytes) = line_literal {
                String::from_utf8(bytes).map_err(|_| "Invalid folder name")?
            } else {
                string_token(rest.trim_start())?.0.into_owned()
            };
            let display_name = folder_name(&name);
            folders.push(Folder {
                id: name,
                name: display_name,
            });
        }
        Ok(folders)
    }

    /// Whether this folder session supports server-driven IDLE notifications.
    pub fn supports_idle(&self) -> bool {
        self.idle
    }

    /// Cancel a blocked read or IDLE wait from another thread without exposing the socket.
    pub fn interruptor(&self) -> Result<aterm_http::stream::Interrupt, String> {
        self.session
            .wire
            .stream
            .get_ref()
            .interruptor()
            .map_err(|error| error.to_string())
    }

    pub fn messages(&mut self, limit: usize) -> Result<Vec<Message>, String> {
        let reply = self.session.command("UID SEARCH ALL")?;
        let mut ids = Vec::new();
        for line in reply.lines {
            let mut words = line.split_whitespace();
            if words.next() == Some("*")
                && words
                    .next()
                    .is_some_and(|word| word.eq_ignore_ascii_case("SEARCH"))
            {
                for value in words {
                    ids.push(uid(value)?);
                }
            }
        }
        ids.sort_unstable();
        ids.dedup();
        let wanted: Vec<_> = ids.into_iter().rev().take(limit.min(100)).collect();
        if wanted.is_empty() {
            return Ok(Vec::new());
        }
        let mut cmd = String::with_capacity(72 + wanted.len().saturating_mul(8));
        cmd.push_str("UID FETCH ");
        for (i, id) in wanted.iter().enumerate() {
            use std::fmt::Write as _;
            if i > 0 {
                cmd.push(',');
            }
            let _ = write!(cmd, "{id}");
        }
        cmd.push_str(" (UID FLAGS BODY.PEEK[HEADER.FIELDS (FROM TO SUBJECT DATE)])");
        let reply = self.session.command(&cmd)?;
        let mut parts = fetch_parts(reply)?;
        if parts
            .iter()
            .any(|part| wanted.binary_search_by(|probe| part.id.cmp(probe)).is_err())
        {
            return Err("Unexpected message UID".into());
        }
        parts.sort_unstable_by_key(|part| std::cmp::Reverse(part.id));
        let mut messages = Vec::with_capacity(parts.len());
        for part in parts {
            let mut message = crate::mime::parse(&part.bytes)?;
            message.id = part.id.to_string();
            message.unread = !attributes_have_seen(&part.attributes);
            messages.push(message);
        }
        Ok(messages)
    }

    /// Read a message and its flags without altering its seen state.
    pub fn read(&mut self, id: &str) -> Result<Message, ReadError> {
        self.session.fetch(id)
    }

    pub fn source(&mut self, id: &str) -> Result<Vec<u8>, String> {
        self.session
            .source(id)
            .map(|part| part.bytes)
            .map_err(String::from)
    }

    /// Wait for server notifications, retaining updates received during preceding reads.
    /// A server without IDLE uses a short polling fallback.
    pub fn wait(&mut self, within: std::time::Duration) -> Result<bool, String> {
        if std::mem::take(&mut self.session.changed) {
            return Ok(true);
        }
        if within.is_zero() {
            return Ok(false);
        }
        if !self.idle {
            let sleep_for = match self.session.wire.remaining()? {
                Some(left) => within.min(left).min(std::time::Duration::from_secs(2)),
                None => within.min(std::time::Duration::from_secs(2)),
            };
            std::thread::sleep(sleep_for);
            return Ok(true);
        }
        self.session.wire.remaining()?;
        self.session.wire.renew(std::time::Duration::from_secs(60));
        let tag = self.session.next_tag();
        self.session.write_command(tag.as_str(), "IDLE")?;
        let mut notified = false;
        let mut continued = false;
        for _ in 0..256 {
            let ready = self.session.wire.line()?;
            if ready.starts_with(b"+") {
                continued = true;
                break;
            }
            if !ready.starts_with(b"* ") || ready.starts_with(b"* BYE") {
                return Err("IMAP server refused IDLE".into());
            }
            notified |= mailbox_change_bytes(&ready, true);
        }
        if !continued {
            return Err("IMAP IDLE continuation exceeded limit".into());
        }
        self.session.wire.renew(within);
        if !notified {
            for _ in 0..256 {
                let Some(line) = self.session.wire.idle_line()? else {
                    break;
                };
                if line
                    .get(..5)
                    .is_some_and(|p| p.eq_ignore_ascii_case(b"* BYE"))
                {
                    return Err("IMAP server closed its idle session".into());
                }
                if mailbox_change_bytes(&line, true) {
                    notified = true;
                    break;
                }
            }
        }
        self.session.wire.renew(std::time::Duration::from_secs(60));
        self.session.wire.write(b"DONE\r\n")?;
        let final_reply = self.session.receive(tag.as_str())?;
        let changed = std::mem::take(&mut self.session.changed);
        Ok(changed
            || notified
            || final_reply
                .lines
                .iter()
                .any(|line| mailbox_change(line, true)))
    }
}

struct TagBuf {
    bytes: [u8; 21],
    len: usize,
}

impl TagBuf {
    fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..self.len]).unwrap_or("")
    }
}

struct Session {
    wire: Wire,
    tag: u64,
    changed: bool,
}
#[derive(Default)]
struct Reply {
    lines: Vec<String>,
    literals: Vec<Vec<u8>>,
}

impl Session {
    fn next_tag(&mut self) -> TagBuf {
        self.tag += 1;
        let mut bytes = [0u8; 21];
        bytes[0] = b'a';
        let mut cursor = std::io::Cursor::new(&mut bytes[1..]);
        let _ = std::io::Write::write_fmt(&mut cursor, format_args!("{}", self.tag));
        let len = 1 + cursor.position() as usize;
        TagBuf { bytes, len }
    }

    fn write_command(&mut self, tag: &str, command: &str) -> Result<(), String> {
        let total = tag.len() + 1 + command.len() + 2;
        if total <= 512 {
            let mut buf = [0u8; 512];
            buf[..tag.len()].copy_from_slice(tag.as_bytes());
            buf[tag.len()] = b' ';
            let cmd_end = tag.len() + 1 + command.len();
            buf[tag.len() + 1..cmd_end].copy_from_slice(command.as_bytes());
            buf[cmd_end..total].copy_from_slice(b"\r\n");
            self.wire.write(&buf[..total])
        } else {
            let mut buf = Vec::with_capacity(total);
            buf.extend_from_slice(tag.as_bytes());
            buf.push(b' ');
            buf.extend_from_slice(command.as_bytes());
            buf.extend_from_slice(b"\r\n");
            self.wire.write(&buf)
        }
    }

    fn connect(config: &Imap) -> Result<Self, ConnectError> {
        Self::connect_until(config, None)
    }

    fn connect_until(config: &Imap, end: Option<std::time::Instant>) -> Result<Self, ConnectError> {
        let mut wire = Wire::connect_until(&config.endpoint, end)?;
        let greeting = wire.line()?;
        if greeting.len() < 5 || !greeting[..5].eq_ignore_ascii_case(b"* OK ") {
            return Err("IMAP server did not accept the connection".into());
        }
        let mut session = Self {
            wire,
            tag: 0,
            changed: false,
        };
        if config.endpoint.security == Security::StartTls {
            session.command("STARTTLS")?;
            session.wire = session.wire.upgrade()?;
        }
        session.authenticate(&config.authentication)?;
        Ok(session)
    }

    fn authenticate(&mut self, authentication: &Authentication) -> Result<(), ConnectError> {
        match authentication {
            Authentication::Password { username, password } => {
                self.wire.renew(std::time::Duration::from_secs(60));
                let tag_buf = self.next_tag();
                let tag = tag_buf.as_str();
                let quoted_user = quoted(username)?;
                let quoted_pw = zeroize::Zeroizing::new(quoted(password.expose())?);
                let cap = tag.len() + 7 + quoted_user.len() + 1 + quoted_pw.len() + 2;
                let mut login = zeroize::Zeroizing::new(String::with_capacity(cap));
                login.push_str(tag);
                login.push_str(" LOGIN ");
                login.push_str(&quoted_user);
                login.push(' ');
                login.push_str(&quoted_pw);
                login.push_str("\r\n");
                self.wire.write(login.as_bytes())?;
                self.receive_status(tag).map_err(|error| match error {
                    ReceiveError::Status(status) => auth_status_error(&status),
                    ReceiveError::Other(message) => ConnectError::Other(message),
                })?;
            }
            Authentication::OAuth { .. } => {
                let raw_xoauth2 = authentication.xoauth2().ok_or("Missing OAuth token")?;
                let encoded = zeroize::Zeroizing::new(
                    aterm_codec::base64::encode(raw_xoauth2.as_bytes())
                        .map_err(|e| e.to_string())?,
                );
                drop(raw_xoauth2);
                self.wire.renew(std::time::Duration::from_secs(60));
                let tag_buf = self.next_tag();
                let tag = tag_buf.as_str();
                self.write_command(tag, "AUTHENTICATE XOAUTH2")?;
                let mut challenged = false;
                for _ in 0..256 {
                    let line = self.wire.line()?;
                    if line.starts_with(b"+") {
                        challenged = true;
                        break;
                    }
                    let mut trimmed = line.as_slice();
                    while let Some((&last, rest)) = trimmed.split_last() {
                        if matches!(last, b'\r' | b'\n') {
                            trimmed = rest;
                        } else {
                            break;
                        }
                    }
                    if let Some(status) = trimmed
                        .strip_prefix(tag.as_bytes())
                        .and_then(|rest| rest.strip_prefix(b" "))
                    {
                        let status = String::from_utf8_lossy(status);
                        if status
                            .split_whitespace()
                            .next()
                            .is_some_and(|s| s.eq_ignore_ascii_case("OK"))
                        {
                            return Ok(());
                        }
                        return Err(auth_status_error(&status));
                    }
                    if trimmed
                        .get(..5)
                        .is_some_and(|p| p.eq_ignore_ascii_case(b"* BYE"))
                    {
                        return Err(ConnectError::Other("IMAP server closed the session".into()));
                    }
                    if !trimmed.starts_with(b"* ") {
                        return Err(ConnectError::Other(
                            "Invalid IMAP authentication challenge".into(),
                        ));
                    }
                }
                if !challenged {
                    return Err(ConnectError::Other(
                        "IMAP authentication continuation exceeded limit".into(),
                    ));
                }
                let mut line = zeroize::Zeroizing::new(Vec::with_capacity(encoded.len() + 2));
                line.extend_from_slice(encoded.as_bytes());
                line.extend_from_slice(b"\r\n");
                self.wire.write(&line)?;
                self.receive_status(tag).map_err(|error| match error {
                    ReceiveError::Status(status) => auth_status_error(&status),
                    ReceiveError::Other(message) => ConnectError::Other(message),
                })?;
            }
        }
        Ok(())
    }

    fn command(&mut self, command: &str) -> Result<Reply, String> {
        self.wire.renew(std::time::Duration::from_secs(60));
        let tag = self.next_tag();
        self.write_command(tag.as_str(), command)?;
        self.receive(tag.as_str())
    }

    fn receive(&mut self, tag: &str) -> Result<Reply, String> {
        self.receive_status(tag).map_err(|error| match error {
            ReceiveError::Status(_) => "IMAP operation was refused by the server".into(),
            ReceiveError::Other(message) => message,
        })
    }

    fn receive_status(&mut self, tag: &str) -> Result<Reply, ReceiveError> {
        let mut reply = Reply::default();
        let mut size = 0usize;
        loop {
            let mut line = self.wire.line()?;
            size = size
                .checked_add(line.len())
                .ok_or("IMAP response too large")?;
            if size > crate::wire::MAX_MESSAGE {
                return Err("IMAP response too large".into());
            }
            while line.last().is_some_and(|b| matches!(*b, b'\r' | b'\n')) {
                line.pop();
            }
            let text = String::from_utf8(line)
                .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());
            if let Some(status) = text
                .strip_prefix(tag)
                .and_then(|rest| rest.strip_prefix(' '))
            {
                if status
                    .split_whitespace()
                    .next()
                    .is_some_and(|s| s.eq_ignore_ascii_case("OK"))
                {
                    return Ok(reply);
                }
                return Err(ReceiveError::Status(status.to_owned()));
            }
            self.changed |= mailbox_change(&text, false);
            if text
                .get(..5)
                .is_some_and(|p| p.eq_ignore_ascii_case("* BYE"))
            {
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
        self.try_select(folder, writable).map_err(String::from)
    }

    fn try_select(&mut self, folder: &str, writable: bool) -> Result<(), SelectError> {
        let quoted_folder = quoted(folder).map_err(|_| SelectError::UnknownFolder)?;
        self.wire.renew(std::time::Duration::from_secs(60));
        let tag_buf = self.next_tag();
        let tag = tag_buf.as_str();
        let verb = if writable { "SELECT" } else { "EXAMINE" };
        let total = tag.len() + 1 + verb.len() + 1 + quoted_folder.len() + 2;
        if total <= 512 {
            let mut buf = [0u8; 512];
            let mut at = 0;
            buf[at..at + tag.len()].copy_from_slice(tag.as_bytes());
            at += tag.len();
            buf[at] = b' ';
            at += 1;
            buf[at..at + verb.len()].copy_from_slice(verb.as_bytes());
            at += verb.len();
            buf[at] = b' ';
            at += 1;
            buf[at..at + quoted_folder.len()].copy_from_slice(quoted_folder.as_bytes());
            at += quoted_folder.len();
            buf[at..at + 2].copy_from_slice(b"\r\n");
            self.wire.write(&buf[..total]).map_err(SelectError::Other)?;
        } else {
            let mut buf = Vec::with_capacity(total);
            buf.extend_from_slice(tag.as_bytes());
            buf.push(b' ');
            buf.extend_from_slice(verb.as_bytes());
            buf.push(b' ');
            buf.extend_from_slice(quoted_folder.as_bytes());
            buf.extend_from_slice(b"\r\n");
            self.wire.write(&buf).map_err(SelectError::Other)?;
        }
        self.receive_status(tag)
            .map(|_| ())
            .map_err(|error| match error {
                ReceiveError::Status(status) => select_status_error(&status),
                ReceiveError::Other(message) => SelectError::Other(message),
            })
    }

    fn source(&mut self, id: &str) -> Result<FetchPart, ReadError> {
        let wanted = uid(id).map_err(|_| ReadError::InvalidId)?;
        let reply = self.command(&format!("UID FETCH {wanted} (UID FLAGS BODY.PEEK[])"))?;
        fetch_parts(reply)?
            .into_iter()
            .find(|part| {
                part.id == wanted
                    && part
                        .attributes
                        .as_bytes()
                        .windows(6)
                        .any(|w| w.eq_ignore_ascii_case(b"BODY[]"))
            })
            .ok_or(ReadError::Missing)
    }

    fn fetch(&mut self, id: &str) -> Result<Message, ReadError> {
        let part = self.source(id)?;
        let mut message = crate::mime::parse(&part.bytes)?;
        message.id = part.id.to_string();
        message.unread = !attributes_have_seen(&part.attributes);
        Ok(message)
    }
}

fn mailbox_change_bytes(line: &[u8], flags: bool) -> bool {
    let mut words = line
        .split(|b| b.is_ascii_whitespace())
        .filter(|w| !w.is_empty());
    words.next() == Some(b"*")
        && words
            .next()
            .and_then(|number| std::str::from_utf8(number).ok())
            .is_some_and(|number| number.parse::<u32>().is_ok())
        && words.next().is_some_and(|kind| {
            [b"EXISTS".as_slice(), b"EXPUNGE", b"RECENT"]
                .iter()
                .any(|name| kind.eq_ignore_ascii_case(name))
                || flags && kind.eq_ignore_ascii_case(b"FETCH")
        })
}

fn mailbox_change(line: &str, flags: bool) -> bool {
    mailbox_change_bytes(line.as_bytes(), flags)
}

struct FetchPart {
    id: u32,
    attributes: String,
    bytes: Vec<u8>,
}
fn fetch_parts(reply: Reply) -> Result<Vec<FetchPart>, String> {
    let mut literals = reply.literals.into_iter();
    let mut parts = Vec::new();
    for (index, line) in reply.lines.iter().enumerate() {
        let (attributes, bytes) = if literal_len(line)?.is_some() {
            if !line
                .as_bytes()
                .windows(8)
                .any(|w| w.eq_ignore_ascii_case(b" FETCH ("))
            {
                return Err("Unexpected message literal".into());
            }
            let mut attributes = line.clone();
            for tail in reply
                .lines
                .iter()
                .skip(index + 1)
                .take_while(|tail| !tail.starts_with("* "))
            {
                attributes.push(' ');
                attributes.push_str(tail);
            }
            let bytes = literals.next().ok_or("Missing message literal")?;
            (attributes, bytes)
        } else if let Some(inline) = extract_inline_fetch_body(line)? {
            inline
        } else {
            continue;
        };
        let mut words = attributes
            .split_whitespace()
            .map(|word| word.trim_matches(['(', ')']));
        let id = words
            .find(|word| word.eq_ignore_ascii_case("UID"))
            .and_then(|_| words.next())
            .ok_or("Missing message UID")?;
        let id = uid(id)?;
        parts.push(FetchPart {
            id,
            attributes,
            bytes,
        });
    }
    if literals.next().is_some() {
        return Err("Unidentified message literal".into());
    }
    Ok(parts)
}

fn fetch_list_open(line: &str) -> Option<usize> {
    let after_star = line.strip_prefix('*')?;
    let trimmed = after_star.trim_start();
    if trimmed.len() == after_star.len() {
        return None;
    }
    let digits_end = trimmed
        .find(|ch: char| !ch.is_ascii_digit())
        .unwrap_or(trimmed.len());
    if digits_end == 0 {
        return None;
    }
    trimmed[..digits_end].parse::<u32>().ok()?;
    let after_seq = &trimmed[digits_end..];
    let verb = after_seq.trim_start();
    if verb.len() == after_seq.len() {
        return None;
    }
    if !verb
        .get(..5)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("FETCH"))
    {
        return None;
    }
    let after_fetch = verb[5..].trim_start();
    if !after_fetch.starts_with('(') {
        return None;
    }
    Some(line.len() - after_fetch.len())
}

fn extract_inline_fetch_body(line: &str) -> Result<Option<(String, Vec<u8>)>, String> {
    let Some(open_paren) = fetch_list_open(line) else {
        return Ok(None);
    };
    let rest = &line[open_paren + 1..];
    let mut in_quotes = false;
    let mut escaped = false;
    let mut prev_delim = true;
    for (pos, ch) in rest.char_indices() {
        if in_quotes {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_quotes = false;
            }
            prev_delim = false;
            continue;
        }
        if ch == '"' {
            in_quotes = true;
            prev_delim = false;
            continue;
        }
        if prev_delim
            && rest
                .get(pos..pos + 5)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("BODY["))
        {
            let close_rel = rest[pos + 5..]
                .find(']')
                .ok_or("Unterminated IMAP BODY section")?;
            let section_end = open_paren + 1 + pos + 5 + close_rel + 1;
            let mut after_section = &line[section_end..];
            if let Some(after_lt) = after_section.strip_prefix('<') {
                let gt = after_lt.find('>').ok_or("Unterminated IMAP BODY origin")?;
                after_section = &after_lt[gt + 1..];
            }
            let value_part = after_section.trim_start();
            if value_part.starts_with('"') {
                let (decoded, tail) = string_token(value_part)?;
                let mut attributes = String::with_capacity(section_end + 1 + tail.len());
                attributes.push_str(&line[..section_end]);
                if !tail.is_empty() && !tail.starts_with([' ', ')']) {
                    attributes.push(' ');
                }
                attributes.push_str(tail);
                return Ok(Some((attributes, decoded.into_owned().into_bytes())));
            }
            if value_part
                .get(..3)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("NIL"))
                && value_part[3..]
                    .chars()
                    .next()
                    .is_none_or(|next| next == ')' || next.is_whitespace())
            {
                let tail = &value_part[3..];
                let mut attributes = String::with_capacity(section_end + tail.len());
                attributes.push_str(&line[..section_end]);
                attributes.push_str(tail);
                return Ok(Some((attributes, Vec::new())));
            }
            return Err("Invalid inline IMAP BODY value".into());
        }
        prev_delim = ch == '(' || ch == ')' || ch.is_whitespace();
    }
    Ok(None)
}

fn uid(value: &str) -> Result<u32, String> {
    if value.is_empty() || value.starts_with('0') || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err("Invalid IMAP message UID".into());
    }
    value
        .parse::<u32>()
        .map_err(|_| "Invalid IMAP message UID".into())
}

fn literal_len(line: &str) -> Result<Option<usize>, String> {
    let Some(before_close) = line.strip_suffix('}') else {
        return Ok(None);
    };
    let Some((_, inside)) = before_close.rsplit_once('{') else {
        return Ok(None);
    };
    let number = inside.strip_suffix('+').unwrap_or(inside);
    if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
        return Ok(None);
    }
    let len = number
        .parse::<usize>()
        .map_err(|_| "IMAP literal too large")?;
    if len > crate::wire::MAX_MESSAGE {
        return Err("IMAP literal too large".into());
    }
    Ok(Some(len))
}

impl Mailbox for Imap {
    fn folders(&mut self) -> Result<Vec<Folder>, String> {
        let session = Session::connect(self)?;
        Reader {
            session,
            idle: false,
        }
        .folders()
    }

    fn messages(&mut self, folder: &str, limit: usize) -> Result<Vec<Message>, String> {
        self.open(folder)?.messages(limit)
    }

    fn read(&mut self, folder: &str, id: &str) -> Result<Message, String> {
        let mut session = Session::connect(self)?;
        session.select(folder, false)?;
        session.fetch(id).map_err(String::from)
    }

    fn mark_read(&mut self, folder: &str, id: &str, read: bool) -> Result<(), String> {
        let wanted = uid(id)?;
        let mut session = Session::connect(self)?;
        session.select(folder, true)?;
        session
            .command(&format!(
                "UID STORE {wanted} {}FLAGS.SILENT (\\Seen)",
                if read { "+" } else { "-" }
            ))
            .map(|_| ())
    }
}

fn string_token(text: &str) -> Result<(std::borrow::Cow<'_, str>, &str), String> {
    if let Some(rest) = text.strip_prefix('"') {
        let mut out: Option<String> = None;
        let mut escaped = false;
        for (at, ch) in rest.char_indices() {
            if escaped {
                out.as_mut().expect("escape initialized out").push(ch);
                escaped = false;
            } else if ch == '\\' {
                out.get_or_insert_with(|| {
                    let mut buf = String::with_capacity(rest.len());
                    buf.push_str(&rest[..at]);
                    buf
                });
                escaped = true;
            } else if ch == '"' {
                let decoded = out.map_or(
                    std::borrow::Cow::Borrowed(&rest[..at]),
                    std::borrow::Cow::Owned,
                );
                return Ok((decoded, &rest[at + 1..]));
            } else if let Some(buf) = out.as_mut() {
                buf.push(ch);
            }
        }
        Err("Unterminated IMAP string".into())
    } else {
        let end = text.find(char::is_whitespace).unwrap_or(text.len());
        if end == 0 {
            return Err("Missing IMAP string".into());
        }
        Ok((std::borrow::Cow::Borrowed(&text[..end]), &text[end..]))
    }
}

fn flag_list_contains(flags: &str, wanted: &str) -> bool {
    for chunk in flags.split(|ch: char| ch.is_whitespace() || ch == '(' || ch == ')') {
        if chunk.is_empty() {
            continue;
        }
        let mut rest = chunk;
        while !rest.is_empty() {
            let first_len = rest.chars().next().map_or(0, char::len_utf8);
            let next_slash = rest[first_len..].find('\\').map(|pos| first_len + pos);
            let (flag, tail) = match next_slash {
                Some(pos) => (&rest[..pos], &rest[pos..]),
                None => (rest, ""),
            };
            if flag.eq_ignore_ascii_case(wanted) {
                return true;
            }
            rest = tail;
        }
    }
    false
}

fn attributes_have_seen(attributes: &str) -> bool {
    let body = match fetch_list_open(attributes) {
        Some(open_paren) => &attributes[open_paren + 1..],
        None => attributes,
    };
    let mut in_quotes = false;
    let mut escaped = false;
    let mut prev_delim = true;
    let mut cursor = 0;
    while cursor < body.len() {
        let rest = &body[cursor..];
        let Some(ch) = rest.chars().next() else {
            break;
        };
        if in_quotes {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_quotes = false;
            }
            prev_delim = false;
            cursor += ch.len_utf8();
            continue;
        }
        if ch == '"' {
            in_quotes = true;
            prev_delim = false;
            cursor += 1;
            continue;
        }
        if prev_delim
            && rest
                .get(..5)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("FLAGS"))
        {
            let after_flags = rest[5..].trim_start();
            if (after_flags.len() < rest.len() - 5 || after_flags.starts_with('('))
                && let Some(inside_open) = after_flags.strip_prefix('(')
                && let Some(close_rel) = inside_open.find(')')
            {
                if flag_list_contains(&inside_open[..close_rel], "\\Seen") {
                    return true;
                }
                cursor = body.len() - inside_open[close_rel + 1..].len();
                prev_delim = true;
                continue;
            }
        }
        prev_delim = ch == '(' || ch == ')' || ch.is_whitespace();
        cursor += ch.len_utf8();
    }
    false
}

fn folder_name(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
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
            let raw_shifted = &shifted[..end];
            let decoded = if raw_shifted.contains(['/', '=']) {
                None
            } else {
                let mut padded = Vec::with_capacity((raw_shifted.len() + 3) & !3);
                for b in raw_shifted.bytes() {
                    padded.push(if b == b',' { b'/' } else { b });
                }
                while !padded.len().is_multiple_of(4) {
                    padded.push(b'=');
                }
                aterm_codec::base64::decode_strict(&padded)
                    .ok()
                    .filter(|bytes| bytes.len().is_multiple_of(2))
                    .and_then(|bytes| {
                        char::decode_utf16(
                            bytes
                                .as_chunks::<2>()
                                .0
                                .iter()
                                .map(|b| u16::from_be_bytes(*b)),
                        )
                        .collect::<Result<String, _>>()
                        .ok()
                    })
                    .filter(|decoded| !decoded.chars().any(|ch| (' '..='~').contains(&ch)))
            };
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
#[path = "imap_tests.rs"]
mod tests;
