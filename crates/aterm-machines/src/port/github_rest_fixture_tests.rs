// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_uds::{AcceptWait, AcceptedMode};
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// One request the fixture read.
#[derive(Debug, Clone)]
pub(crate) struct Seen {
    /// `GET /user/keys?per_page=100&page=1 HTTP/1.1`.
    pub(crate) line: String,
    /// Each header as sent, names lowercased.
    pub(crate) headers: Vec<(String, String)>,
    /// The body, by its `Content-Length`.
    pub(crate) body: Vec<u8>,
}

impl Seen {
    /// The first value of `name`.
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// The listener and what it saw.
pub(crate) struct Server {
    /// `http://127.0.0.1:<port>`.
    pub(crate) origin: String,
    seen: Arc<Mutex<Vec<Seen>>>,
    thread: Option<JoinHandle<()>>,
}

/// A complete JSON answer with `status`, `headers` and `body`.
pub(crate) fn json(status: u16, headers: &[(&str, &str)], body: &str) -> Vec<u8> {
    let mut out = format!(
        "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (name, value) in headers {
        out.push_str(&format!("{name}: {value}\r\n"));
    }
    out.push_str("\r\n");
    out.push_str(body);
    out.into_bytes()
}

/// A head that declares `body`'s length plus `missing`, then only `body`.
pub(crate) fn cut_short(body: &str, missing: usize) -> Vec<u8> {
    format!(
        "HTTP/1.1 200 Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len() + missing
    )
    .into_bytes()
}

impl Server {
    /// Serve `replies`, one connection each, in order; the listener stops after the last.
    pub(crate) fn start(replies: Vec<Vec<u8>>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        let thread = std::thread::spawn(move || {
            for reply in replies {
                let Ok((stream, _)) =
                    aterm_uds::accept(&listener, AcceptWait::Forever, AcceptedMode::Blocking)
                else {
                    return;
                };
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() || line.is_empty() {
                    return;
                }
                let mut headers = Vec::new();
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).is_err() || header.trim().is_empty() {
                        break;
                    }
                    if let Some((name, value)) = header.trim_end().split_once(':') {
                        headers.push((name.to_ascii_lowercase(), value.trim().to_owned()));
                    }
                }
                let length = headers
                    .iter()
                    .find(|(name, _)| name == "content-length")
                    .and_then(|(_, value)| crate::parse_uint::<usize>(value))
                    .unwrap_or(0);
                let mut body = vec![0; length];
                if reader.read_exact(&mut body).is_err() {
                    return;
                }
                record.lock().unwrap().push(Seen {
                    line: line.trim_end().to_owned(),
                    headers,
                    body,
                });
                let mut stream = reader.into_inner();
                let _ = stream.write_all(&reply);
                let _ = stream.flush();
            }
        });
        Self {
            origin,
            seen,
            thread: Some(thread),
        }
    }

    /// Every request read so far.
    pub(crate) fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::net::TcpStream::connect(self.origin.trim_start_matches("http://"));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
