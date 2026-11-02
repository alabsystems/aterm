// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded, TLS-verified mail wire. No subprocess or external mail application.

use crate::{Endpoint, Security};
use aterm_http::stream::{AlwaysAuthorized, Connect, Deadline, Stream, TcpConnector};
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::Arc;
use std::time::Duration;

pub(crate) const MAX_LINE: usize = 64 * 1024;
pub(crate) const MAX_MESSAGE: usize = 32 * 1024 * 1024;

pub(crate) struct Wire {
    pub(crate) stream: BufReader<Stream>,
    endpoint: Endpoint,
    deadline: Deadline,
}

impl Wire {
    #[cfg(test)]
    pub(crate) fn for_test(tcp: std::net::TcpStream) -> Self {
        let deadline = Deadline::after(Duration::from_secs(60));
        Self {
            stream: BufReader::new(Stream::plain(tcp, Arc::new(AlwaysAuthorized), deadline)),
            endpoint: Endpoint {
                host: "localhost".into(),
                port: 1,
                security: Security::Tls,
            },
            deadline,
        }
    }
    pub(crate) fn connect(endpoint: &Endpoint) -> Result<Self, String> {
        if endpoint.host.is_empty()
            || endpoint.host.chars().any(char::is_control)
            || endpoint.port == 0
        {
            return Err("Invalid mail server address".into());
        }
        let deadline = Deadline::after(Duration::from_secs(60)).with_idle(Duration::from_secs(20));
        let tcp = TcpConnector
            .connect(&endpoint.host, endpoint.port, deadline)
            .map_err(|e| e.to_string())?;
        let stream = match endpoint.security {
            Security::Tls => Stream::start_tls(
                tcp,
                aterm_http::tls::client_config(&aterm_http::Trust::PlatformVerifier)?,
                &endpoint.host,
                Arc::new(AlwaysAuthorized),
                deadline,
            ),
            Security::StartTls => Ok(Stream::plain(tcp, Arc::new(AlwaysAuthorized), deadline)),
        }
        .map_err(|e| e.to_string())?;
        Ok(Self {
            stream: BufReader::new(stream),
            endpoint: endpoint.clone(),
            deadline,
        })
    }
    pub(crate) fn upgrade(self) -> Result<Self, String> {
        if !self.stream.buffer().is_empty() {
            return Err("Unexpected bytes before TLS handshake".into());
        }
        let tcp = self
            .stream
            .into_inner()
            .into_tcp()
            .ok_or("Connection is already encrypted")?;
        let stream = Stream::start_tls(
            tcp,
            aterm_http::tls::client_config(&aterm_http::Trust::PlatformVerifier)?,
            &self.endpoint.host,
            Arc::new(AlwaysAuthorized),
            self.deadline,
        )
        .map_err(|e| e.to_string())?;
        Ok(Self {
            stream: BufReader::new(stream),
            endpoint: self.endpoint,
            deadline: self.deadline,
        })
    }
    pub(crate) fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.stream
            .get_mut()
            .write_all(bytes)
            .and_then(|_| self.stream.get_mut().flush())
            .map_err(|e| e.to_string())
    }
    pub(crate) fn line(&mut self) -> Result<Vec<u8>, String> {
        bounded_line(&mut self.stream)
    }
    pub(crate) fn literal(&mut self, len: usize) -> Result<Vec<u8>, String> {
        if len > MAX_MESSAGE {
            return Err("Message exceeds the 32 MiB limit".into());
        }
        let mut bytes = vec![0; len];
        self.stream
            .read_exact(&mut bytes)
            .map_err(|e| e.to_string())?;
        Ok(bytes)
    }
}

pub(crate) fn bounded_line(reader: &mut impl BufRead) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    let read = reader
        .take((MAX_LINE + 1) as u64)
        .read_until(b'\n', &mut bytes)
        .map_err(|e| e.to_string())?;
    if read == 0 {
        return Err("Mail server closed the connection".into());
    }
    if bytes.len() > MAX_LINE {
        return Err("Mail server line exceeds 64 KiB".into());
    }
    if !bytes.ends_with(b"\r\n") {
        return Err("Mail server returned an incomplete line".into());
    }
    Ok(bytes)
}

pub(crate) fn quoted(value: &str) -> Result<String, String> {
    if value.chars().any(char::is_control) {
        return Err("Control character in mail protocol argument".into());
    }
    Ok(format!(
        "\"{}\"",
        value.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_lines_and_arguments_refuse_injection() {
        assert!(bounded_line(&mut &b"* OK\n"[..]).is_err());
        assert!(bounded_line(&mut &vec![b'x'; MAX_LINE + 1][..]).is_err());
        assert!(quoted("INBOX\r\nLOGOUT").is_err());
        assert_eq!(quoted("a\\b\"c").unwrap(), "\"a\\\\b\\\"c\"");
    }
}
