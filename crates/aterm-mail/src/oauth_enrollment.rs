// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//! One bounded Google authorization, with a loopback receiver bound before navigation.

use super::{Error, ErrorKind, Pending, Secret, Tokens};
use aterm_uds::{AcceptWait, AcceptedMode};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

fn error(kind: ErrorKind) -> Error {
    Error { kind }
}

pub struct Client {
    pub id: String,
    pub secret: Option<Secret>,
}

impl Client {
    /// Read Google's desktop-client JSON export. Endpoints always remain Google's.
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() > 65536 {
            return Err(error(ErrorKind::InvalidResponse));
        }
        let value: aterm_json::Value =
            aterm_json::from_slice(bytes).map_err(|_| error(ErrorKind::InvalidResponse))?;
        let installed = value
            .get("installed")
            .filter(|installed| installed.is_object())
            .ok_or(error(ErrorKind::InvalidResponse))?;
        let field = |name: &str| -> Result<String, Error> {
            installed[name]
                .as_str()
                .filter(|text| {
                    !text.is_empty() && text.len() <= 8192 && !text.chars().any(char::is_control)
                })
                .map(str::to_owned)
                .ok_or(error(ErrorKind::InvalidResponse))
        };
        Ok(Self {
            id: field("client_id")?,
            secret: installed
                .get("client_secret")
                .filter(|secret| !secret.is_null())
                .map(|_| field("client_secret").map(Secret::new))
                .transpose()?,
        })
    }
}

pub struct Enrollment {
    client: Client,
    pending: Pending,
    listener: TcpListener,
}

impl Enrollment {
    pub fn bind(client: Client, account: &str) -> Result<Self, Error> {
        let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
            .map_err(|_| error(ErrorKind::Transport))?;
        let port = listener
            .local_addr()
            .map_err(|_| error(ErrorKind::Transport))?
            .port();
        let pending = super::begin(&client.id, port, Some(account))
            .map_err(|_| error(ErrorKind::InvalidResponse))?;
        Ok(Self {
            client,
            pending,
            listener,
        })
    }

    /// The authorizer owns its browser lifetime and obeys the shared deadline.
    /// Callback state is consumed once; no tokens or callback URLs are logged.
    pub fn run(
        self,
        end: Instant,
        authorize: impl FnOnce(&str, Instant) -> Result<(), Error>,
    ) -> Result<(Client, Tokens), Error> {
        if Instant::now() >= end {
            return Err(error(ErrorKind::Transport));
        }
        let stop = AtomicBool::new(false);
        let code = std::thread::scope(|scope| {
            let receiver = std::thread::Builder::new()
                .name("gmail-oauth-callback".into())
                .spawn_scoped(scope, || self.receive(end, &stop))
                .map_err(|_| error(ErrorKind::Transport))?;
            let result = authorize(&self.pending.authorize_url, end);
            stop.store(true, Ordering::Release);
            let code = receiver.join().map_err(|_| error(ErrorKind::Transport))?;
            result?;
            code
        })?;
        let tokens = super::exchange_until(
            &self.client.id,
            self.client.secret.as_ref().map(Secret::expose),
            &self.pending,
            &self.pending.state,
            code.expose(),
            end,
        )?;
        Ok((self.client, tokens))
    }

    fn receive(&self, end: Instant, stop: &AtomicBool) -> Result<Secret, Error> {
        let mut completed: Option<Secret> = None;
        let port = self
            .listener
            .local_addr()
            .map_err(|_| error(ErrorKind::Transport))?
            .port();
        while Instant::now() < end {
            let stopping = stop.load(Ordering::Acquire);
            let accept_end = end.min(Instant::now() + Duration::from_millis(20));
            match aterm_uds::accept(
                &self.listener,
                AcceptWait::Until(accept_end),
                AcceptedMode::Blocking,
            ) {
                Ok((mut stream, peer)) if peer.ip().is_loopback() => {
                    let budget = end
                        .saturating_duration_since(Instant::now())
                        .min(Duration::from_secs(2));
                    if budget.is_zero() {
                        break;
                    }
                    bound_io(&stream, budget)?;
                    let result = request(&mut stream, Instant::now() + budget)
                        .and_then(|request| callback(&request, port, &self.pending.state))
                        .and_then(|code| {
                            if let Some(previous) = &completed
                                && code.as_ref().is_none_or(|code| {
                                    !aterm_digest::ct_eq(
                                        code.expose().as_bytes(),
                                        previous.expose().as_bytes(),
                                    )
                                })
                            {
                                return Err(error(ErrorKind::Rejected));
                            }
                            Ok(code)
                        });
                    let response = if result.is_ok() {
                        b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n".as_slice()
                    } else {
                        b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n".as_slice()
                    };
                    let left = end.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return Err(error(ErrorKind::Transport));
                    }
                    stream
                        .set_write_timeout(Some(left.min(Duration::from_secs(2))))
                        .map_err(|_| error(ErrorKind::Transport))?;
                    let _ = stream.write_all(response);
                    match result {
                        Ok(Some(code)) => completed = Some(code),
                        Ok(None) => return Err(error(ErrorKind::Rejected)),
                        Err(_) => continue,
                    }
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
                    if stopping {
                        break;
                    }
                }
                Err(_) => return Err(error(ErrorKind::Transport)),
            }
        }
        completed.ok_or(error(ErrorKind::Transport))
    }
}

/// Bound an accepted callback stream's reads and writes. [`aterm_uds::accept`] already makes its
/// blocking mode explicit before the browser's request can race the first read.
fn bound_io(stream: &TcpStream, budget: Duration) -> Result<(), Error> {
    stream
        .set_read_timeout(Some(budget))
        .map_err(|_| error(ErrorKind::Transport))?;
    stream
        .set_write_timeout(Some(budget))
        .map_err(|_| error(ErrorKind::Transport))
}

fn request(stream: &mut TcpStream, end: Instant) -> Result<zeroize::Zeroizing<String>, Error> {
    let mut bytes = zeroize::Zeroizing::new(Vec::with_capacity(512));
    let mut chunk = zeroize::Zeroizing::new([0u8; 512]);
    while bytes.len() < 16384 {
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(error(ErrorKind::Transport));
        }
        stream
            .set_read_timeout(Some(left))
            .map_err(|_| error(ErrorKind::Transport))?;
        let max_read = (16384 - bytes.len()).min(chunk.len());
        let read_len = match stream.read(&mut chunk[..max_read]) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(error(ErrorKind::InvalidResponse)),
        };
        let search_from = bytes.len().saturating_sub(3);
        bytes.extend_from_slice(&chunk[..read_len]);
        if let Some(rel) = bytes[search_from..]
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
        {
            bytes.truncate(search_from + rel + 4);
            let text = String::from_utf8(std::mem::take(&mut *bytes))
                .map_err(|_| error(ErrorKind::InvalidResponse))?;
            return Ok(zeroize::Zeroizing::new(text));
        }
    }
    Err(error(ErrorKind::InvalidResponse))
}

fn decode(text: &str) -> Result<String, Error> {
    if !text.as_bytes().iter().any(|&b| matches!(b, b'+' | b'%')) {
        if text.chars().any(char::is_control) {
            return Err(error(ErrorKind::InvalidResponse));
        }
        return Ok(text.to_owned());
    }
    let mut bytes = zeroize::Zeroizing::new(Vec::with_capacity(text.len()));
    let mut input = text.bytes();
    while let Some(byte) = input.next() {
        bytes.push(match byte {
            b'+' => b' ',
            b'%' => {
                let a = input.next().and_then(|b| char::from(b).to_digit(16));
                let b = input.next().and_then(|b| char::from(b).to_digit(16));
                (a.zip(b)
                    .map(|(a, b)| a * 16 + b)
                    .ok_or(error(ErrorKind::InvalidResponse))?) as u8
            }
            _ => byte,
        });
    }
    let mut text = zeroize::Zeroizing::new(
        String::from_utf8(std::mem::take(&mut *bytes))
            .map_err(|_| error(ErrorKind::InvalidResponse))?,
    );
    if text.chars().any(char::is_control) {
        return Err(error(ErrorKind::InvalidResponse));
    }
    Ok(std::mem::take(&mut *text))
}

fn callback(request: &str, port: u16, state: &str) -> Result<Option<Secret>, Error> {
    let invalid = || error(ErrorKind::InvalidResponse);
    let mut lines = request.split("\r\n");
    let mut first = lines.next().ok_or_else(invalid)?.split(' ');
    if first.next() != Some("GET") {
        return Err(invalid());
    }
    let target = first.next().ok_or_else(invalid)?;
    if first.next() != Some("HTTP/1.1") || first.next().is_some() {
        return Err(invalid());
    }
    let mut hosts = lines
        .filter_map(|line| line.split_once(':'))
        .filter(|(name, _)| name.eq_ignore_ascii_case("host"));
    let mut host_buf = [0u8; 20];
    let mut host_cur = std::io::Cursor::new(&mut host_buf[..]);
    let _ = write!(host_cur, "127.0.0.1:{port}");
    let host_len = host_cur.position() as usize;
    let expected_host = std::str::from_utf8(&host_buf[..host_len]).unwrap_or("");
    if hosts.next().map(|(_, value)| value.trim()) != Some(expected_host) || hosts.next().is_some()
    {
        return Err(invalid());
    }
    let query = target
        .strip_prefix("/oauth/callback?")
        .ok_or_else(invalid)?;
    let mut fields = std::collections::BTreeMap::new();
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').ok_or_else(invalid)?;
        if fields
            .insert(decode(key)?, Secret::new(decode(value)?))
            .is_some()
        {
            return Err(invalid());
        }
    }
    if !fields
        .get("state")
        .is_some_and(|got| aterm_digest::ct_eq(got.expose().as_bytes(), state.as_bytes()))
    {
        return Err(error(ErrorKind::Rejected));
    }
    match (fields.remove("code"), fields.remove("error")) {
        (Some(code), None) if !code.expose().is_empty() && code.expose().len() <= 8192 => {
            Ok(Some(code))
        }
        (None, Some(_)) => Ok(None),
        _ => Err(invalid()),
    }
}

#[cfg(test)]
#[path = "oauth_enrollment_tests.rs"]
mod tests;
