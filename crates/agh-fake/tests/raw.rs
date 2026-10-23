// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Minimal HTTP test client over `std::net::TcpStream`.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpStream};
use std::time::Duration;

/// The hang detector for an answer that must come.
pub const HANG: Duration = Duration::from_secs(60);

const CREDENTIAL: &str = "Authorization";

/// One request to send.
#[derive(Clone, Debug)]
pub struct Req {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

/// Builds a GET request with API origin headers.
pub fn api(target: &str) -> Req {
    Req {
        method: "GET".into(),
        target: target.into(),
        headers: vec![
            ("User-Agent".into(), "agh-fake-suite".into()),
            ("Accept".into(), "application/vnd.github+json".into()),
            ("X-GitHub-Api-Version".into(), "2022-11-28".into()),
        ],
        body: Vec::new(),
    }
}

/// A GET as agh sends one to the web or CDN origin: `User-Agent` and `Accept: */*` only.
pub fn plain(target: &str) -> Req {
    Req {
        method: "GET".into(),
        target: target.into(),
        headers: vec![
            ("User-Agent".into(), "agh-fake-suite".into()),
            ("Accept".into(), "*/*".into()),
        ],
        body: Vec::new(),
    }
}

impl Req {
    /// The same request with another method.
    #[must_use]
    pub fn method(mut self, method: &str) -> Req {
        self.method = method.into();
        self
    }

    /// The same request with `name` set to `value` (replacing any value it had).
    #[must_use]
    pub fn header(self, name: &str, value: &str) -> Req {
        let mut req = self.without(name);
        req.headers.push((name.into(), value.into()));
        req
    }

    /// The same request without header `name`.
    #[must_use]
    pub fn without(mut self, name: &str) -> Req {
        self.headers.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
        self
    }

    /// The same request carrying `token` under the `Bearer` scheme.
    #[must_use]
    pub fn token(self, token: &str) -> Req {
        self.scheme("Bearer", token)
    }

    /// The same request carrying `token` under `scheme`.
    #[must_use]
    pub fn scheme(self, scheme: &str, token: &str) -> Req {
        self.header(CREDENTIAL, &format!("{scheme} {token}"))
    }

    /// The same request with `body` and its `Content-Length`.
    #[must_use]
    pub fn body(mut self, body: &[u8]) -> Req {
        self.body = body.to_vec();
        let length = body.len().to_string();
        self.header("Content-Length", &length)
    }

    /// The same request with `body` sent after the head as it is, framed by no header.
    #[must_use]
    pub fn bare_body(mut self, body: &[u8]) -> Req {
        self.body = body.to_vec();
        self
    }

    /// Send it to `port` and read the answer to EOF.
    /// # Panics
    /// When the fake cannot be reached, or does not take the request or answer it within the
    /// hang detector.
    pub fn send(&self, port: u16) -> Reply {
        let mut head = format!(
            "{} {} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n",
            self.method, self.target
        );
        for (name, value) in &self.headers {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        head.push_str("\r\n");
        let mut bytes = head.into_bytes();
        bytes.extend_from_slice(&self.body);
        send_bytes(port, &bytes)
    }
}

/// Send `bytes` to `port` as they are, and read the answer to EOF.
/// # Panics
/// When the fake cannot be reached, or does not take the request or answer it within the
/// hang detector.
pub fn send_bytes(port: u16, bytes: &[u8]) -> Reply {
    let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).expect("connect to the fake");
    stream.set_read_timeout(Some(HANG)).expect("read timeout");
    stream.set_write_timeout(Some(HANG)).expect("write timeout");
    stream.write_all(bytes).expect("write the request");
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .expect("read the answer to EOF");
    Reply::parse(raw)
}

/// Sends `bytes` to `port`, closes the write side, and reads the answer to EOF.
/// # Panics
/// When the fake cannot be reached or does not answer in time.
pub fn send_cut(port: u16, bytes: &[u8]) -> Reply {
    let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).expect("connect to the fake");
    stream.set_read_timeout(Some(HANG)).expect("read timeout");
    stream.set_write_timeout(Some(HANG)).expect("write timeout");
    stream.write_all(bytes).expect("write the request");
    stream
        .shutdown(std::net::Shutdown::Write)
        .expect("close the write side");
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .expect("read the answer to EOF");
    Reply::parse(raw)
}

/// One answer, split.
#[derive(Clone, Debug)]
pub struct Reply {
    /// The status; 0 when the fake closed without a byte.
    pub status: u16,
    /// The headers, as sent.
    pub headers: Vec<(String, String)>,
    /// The bytes after the head.
    pub body: Vec<u8>,
    /// Every byte read.
    pub raw: Vec<u8>,
}

impl Reply {
    fn parse(raw: Vec<u8>) -> Reply {
        let Some(end) = raw.windows(4).position(|w| w == b"\r\n\r\n") else {
            return Reply {
                status: 0,
                headers: Vec::new(),
                body: Vec::new(),
                raw,
            };
        };
        let head = String::from_utf8_lossy(&raw[..end]).into_owned();
        let mut lines = head.split("\r\n");
        let status = lines
            .next()
            .and_then(|l| l.split(' ').nth(1))
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let headers = lines
            .filter_map(|l| l.split_once(':'))
            .map(|(n, v)| (n.trim().to_string(), v.trim().to_string()))
            .collect();
        Reply {
            status,
            headers,
            body: raw[end + 4..].to_vec(),
            raw,
        }
    }

    /// The value of header `name`, in any letter case.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// Every value of header `name`, in any letter case, in the order sent.
    pub fn all(&self, name: &str) -> Vec<&str> {
        self.headers
            .iter()
            .filter(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
            .collect()
    }

    /// The body as JSON.
    /// # Panics
    /// When the body is not JSON.
    pub fn json(&self) -> aterm_json::Value {
        aterm_json::from_slice(&self.body).expect("a JSON body")
    }

    /// The `Link` header's URL for `rel`.
    pub fn link(&self, rel: &str) -> Option<String> {
        let wanted = format!("rel=\"{rel}\"");
        self.header("link")?.split(", ").find_map(|part| {
            let (url, params) = part.split_once(';')?;
            (params.trim() == wanted).then(|| url.trim_matches(['<', '>']).to_string())
        })
    }
}

#[test]
fn a_reply_splits_at_the_head_end() {
    let reply = Reply::parse(b"HTTP/1.1 404 Not Found\r\nA: b\r\n\r\nxy".to_vec());
    assert_eq!(reply.status, 404);
    assert_eq!(reply.header("a"), Some("b"));
    assert_eq!(reply.body, b"xy");
    assert_eq!(Reply::parse(Vec::new()).status, 0);
}
