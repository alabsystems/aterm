// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A loopback HTTP server for the lane's site directory: GET only, one
//! connection per request (`Connection: close`), each on its own thread. It
//! serves the generated site's files through plain relative paths, with the
//! content types a page loading ES modules and wasm needs (a module script
//! must be JavaScript, and `WebAssembly.instantiateStreaming` wants
//! `application/wasm`).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

/// The longest accepted request head; one extra byte detects overflow.
const MAX_HEAD: usize = 16 << 10;

/// A running server; dropping it stops accepting requests.
pub(crate) struct Server {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    served: Arc<AtomicUsize>,
    accept: Option<JoinHandle<()>>,
}

impl Server {
    /// Serve `root` on 127.0.0.1 at a port the kernel picks.
    pub(crate) fn start(root: &Path) -> Result<Self, String> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .map_err(|e| format!("could not listen on loopback: {e}"))?;
        let addr = listener
            .local_addr()
            .map_err(|e| format!("could not read the listening address: {e}"))?;
        let stop = Arc::new(AtomicBool::new(false));
        let served = Arc::new(AtomicUsize::new(0));
        let (root, stop2, served2) = (root.to_path_buf(), stop.clone(), served.clone());
        let accept = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if stop2.load(Ordering::SeqCst) {
                    return;
                }
                let Ok(stream) = stream else { continue };
                let (root, served) = (root.clone(), served2.clone());
                std::thread::spawn(move || {
                    if answer(stream, &root).is_ok() {
                        served.fetch_add(1, Ordering::SeqCst);
                    }
                });
            }
        });
        Ok(Self {
            addr,
            stop,
            served,
            accept: Some(accept),
        })
    }

    /// `http://127.0.0.1:<port>`.
    pub(crate) fn origin(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Requests answered so far.
    pub(crate) fn served(&self) -> usize {
        self.served.load(Ordering::SeqCst)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the accept loop so it sees the flag.
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_secs(1));
        if let Some(t) = self.accept.take() {
            let _ = t.join();
        }
    }
}

/// The content type of a file the site serves.
pub(crate) fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("wasm") => "application/wasm",
        Some("json") => "application/json",
        Some("ttf") => "font/ttf",
        _ => "application/octet-stream",
    }
}

/// The file under `root` a request target names: its path, the query and
/// fragment dropped. `None` for anything not a plain relative path: `..`, a
/// percent escape, a backslash, an empty name.
pub(crate) fn resolve(root: &Path, target: &str) -> Option<PathBuf> {
    let path = target.split(['?', '#']).next()?;
    let rel = path.strip_prefix('/')?;
    if rel.is_empty() || rel.contains(['%', '\\']) {
        return None;
    }
    let rel = Path::new(rel);
    if !rel.components().all(|c| matches!(c, Component::Normal(_))) {
        return None;
    }
    Some(root.join(rel))
}

/// A complete request head, bounded even when a line has no newline.
fn request_head(input: impl Read) -> std::io::Result<String> {
    let mut reader = BufReader::new(input.take(MAX_HEAD as u64 + 1));
    let mut head = String::new();
    loop {
        let start = head.len();
        let n = reader.read_line(&mut head)?;
        if head.len() > MAX_HEAD {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "request head too large",
            ));
        }
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "incomplete request head",
            ));
        }
        if matches!(&head[start..], "\r\n" | "\n") {
            return Ok(head);
        }
    }
}

fn empty_response(out: &mut TcpStream, status: &str) -> std::io::Result<()> {
    write!(
        out,
        "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )
}

/// Read one request from `stream` and answer it.
fn answer(mut stream: TcpStream, root: &Path) -> std::io::Result<()> {
    stream.set_read_timeout(Some(crate::browser::cdp::HANG_DETECTOR))?;
    stream.set_write_timeout(Some(crate::browser::cdp::HANG_DETECTOR))?;
    let head = match request_head(&stream) {
        Ok(head) => head,
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::InvalidData | std::io::ErrorKind::UnexpectedEof
            ) =>
        {
            return empty_response(&mut stream, "400 Bad Request");
        }
        Err(e) => return Err(e),
    };
    let mut words = head.lines().next().unwrap_or_default().split_whitespace();
    let (method, target) = (words.next().unwrap_or(""), words.next().unwrap_or(""));
    let file = (method == "GET")
        .then(|| resolve(root, target))
        .flatten()
        .filter(|p| p.is_file());
    let mut out = stream;
    match file.and_then(|p| std::fs::read(&p).ok().map(|b| (p, b))) {
        Some((path, body)) => {
            write!(
                out,
                "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\n\
                 Cache-Control: no-store\r\nConnection: close\r\n\r\n",
                content_type(&path),
                body.len()
            )?;
            out.write_all(&body)?;
        }
        None => {
            let status = if method == "GET" {
                "404 Not Found"
            } else {
                "405 Method Not Allowed"
            };
            empty_response(&mut out, status)?;
        }
    }
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_heads_are_bounded_before_their_newline() {
        let prefix = "GET /lane.mjs HTTP/1.1\r\nX: ";
        let head = format!(
            "{prefix}{}\r\n\r\n",
            "x".repeat(MAX_HEAD - prefix.len() - 4)
        );
        assert_eq!(head.len(), MAX_HEAD);
        assert_eq!(request_head(head.as_bytes()).unwrap(), head);
        let oversized = "x".repeat(MAX_HEAD * 2);
        let mut input = std::io::Cursor::new(oversized);
        assert_eq!(
            request_head(&mut input).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
        assert_eq!(input.position(), MAX_HEAD as u64 + 1);
    }

    #[test]
    fn only_plain_paths_under_the_root_resolve() {
        let root = Path::new("/site");
        assert_eq!(
            resolve(root, "/gpu/aterm_gpu_web.js?v=1#x"),
            Some(PathBuf::from("/site/gpu/aterm_gpu_web.js"))
        );
        for bad in [
            "/",
            "",
            "gpu.html",
            "/../etc/passwd",
            "/a/../../b",
            "/%2e%2e/x",
            "/a\\b",
            "/./a",
        ] {
            assert_eq!(resolve(root, bad), None, "{bad}");
        }
    }

    #[test]
    fn modules_and_wasm_get_the_types_a_page_needs() {
        assert_eq!(
            content_type(Path::new("a.mjs")),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(
            content_type(Path::new("a.js")),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(content_type(Path::new("a_bg.wasm")), "application/wasm");
        assert_eq!(
            content_type(Path::new("cpu.html")),
            "text/html; charset=utf-8"
        );
        assert_eq!(content_type(Path::new("x")), "application/octet-stream");
    }

    fn request(origin: &str, head: &str) -> String {
        let addr = origin.trim_start_matches("http://");
        let mut s = TcpStream::connect(addr).expect("connect");
        s.set_read_timeout(Some(crate::browser::cdp::HANG_DETECTOR))
            .expect("timeout");
        s.set_write_timeout(Some(crate::browser::cdp::HANG_DETECTOR))
            .expect("write timeout");
        s.write_all(head.as_bytes()).expect("send");
        s.shutdown(std::net::Shutdown::Write)
            .expect("finish request");
        let mut reader = BufReader::new(s);
        let mut out = String::new();
        let mut length = None;
        loop {
            let mut line = String::new();
            assert_ne!(reader.read_line(&mut line).expect("response header"), 0);
            if let Some(value) = line.strip_prefix("Content-Length: ") {
                length = Some(value.trim().parse::<usize>().expect("content length"));
            }
            out.push_str(&line);
            if line == "\r\n" {
                break;
            }
        }
        // A refused request may leave unread input and close with a reset.
        // The counted response is complete without waiting for connection EOF.
        let mut body = vec![0; length.expect("counted response")];
        reader.read_exact(&mut body).expect("response body");
        out.push_str(std::str::from_utf8(&body).expect("text fixture"));
        out
    }

    fn get(origin: &str, target: &str) -> String {
        request(
            origin,
            &format!("GET {target} HTTP/1.1\r\nHost: localhost\r\n\r\n"),
        )
    }

    /// A real loopback round trip: a file comes back whole with its type, a
    /// missing one and an escape are 404s, and dropping the server stops it.
    #[test]
    fn the_server_answers_files_and_refuses_the_rest() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("atv-serve-")
            .tempdir()
            .expect("scratch");
        std::fs::write(dir.path().join("lane.mjs"), "export {}\n").expect("write");
        let server = Server::start(dir.path()).expect("start");
        let ok = get(&server.origin(), "/lane.mjs");
        assert!(ok.starts_with("HTTP/1.1 200 OK\r\n"), "{ok}");
        assert!(
            ok.contains("Content-Type: text/javascript; charset=utf-8\r\n"),
            "{ok}"
        );
        assert!(ok.ends_with("\r\n\r\nexport {}\n"), "{ok}");
        assert!(get(&server.origin(), "/missing.js").starts_with("HTTP/1.1 404"));
        assert!(get(&server.origin(), "/../lane.mjs").starts_with("HTTP/1.1 404"));
        // End oversized requests at the overflow probe. Larger unread bodies
        // may make the OS reset the connection before a refusal is received;
        // the reader-bound test above covers that longer input independently.
        let oversized =
            |prefix: &str| format!("{prefix}{}", "x".repeat(MAX_HEAD + 1 - prefix.len()));
        for invalid in [
            oversized("GET /lane.mjs "),
            oversized("GET /lane.mjs HTTP/1.1\r\nX: "),
            "GET /lane.mjs HTTP/1.1\r\nHost: localhost\r\n".to_string(),
        ] {
            let refused = request(&server.origin(), &invalid);
            assert!(
                refused.starts_with("HTTP/1.1 400"),
                "bad request admitted: {refused}"
            );
        }
        assert!(get(&server.origin(), "/lane.mjs").ends_with("\r\n\r\nexport {}\n"));
        assert!(server.served() >= 1);
        // Returns once the accept loop has ended: a drop that hung would hang
        // the test.
        drop(server);
    }
}
