// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bidirectional unix-socket [`Filter`] relay for [`crate::xhide`] and [`crate::wlhide`].

use std::io::{self, Write as _};
use std::os::fd::{AsFd as _, OwnedFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;

use aterm_uds::fdpass::{self, MAX_FDS};

/// The longest message a filter may announce.
pub(crate) const MAX_MESSAGE: usize = 64 << 20;

/// What a direction's stream is cut into and how each message fares.
pub(crate) trait Filter: Send {
    /// Length of the message `buf` starts with once its header is in, or `Ok(None)` / `Err`.
    fn len(&mut self, buf: &[u8]) -> Result<Option<usize>, String>;

    /// One whole message, which the filter may rewrite in place.
    fn pass(&mut self, msg: &mut [u8]) -> bool;
}

/// One read: the bytes into `buf`, the descriptors that came with them onto `fds`.
fn recv(from: &UnixStream, buf: &mut [u8], fds: &mut Vec<OwnedFd>) -> io::Result<usize> {
    loop {
        match fdpass::recv_with_fds(from, buf, MAX_FDS) {
            Ok(r) => {
                fds.extend(r.fds);
                return Ok(r.bytes);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
}

/// Write all of `data`, the descriptors `fds` riding its first bytes.
fn send(to: &UnixStream, data: &[u8], fds: &mut Vec<OwnedFd>) -> io::Result<()> {
    let mut at = 0;
    while !fds.is_empty() && at < data.len() {
        let carried = fds.len().min(MAX_FDS);
        let borrowed: Vec<_> = fds[..carried].iter().map(|f| f.as_fd()).collect();
        let end = if fds.len() > carried {
            at + 1
        } else {
            data.len()
        };
        let n = match fdpass::send_with_fds(to, &data[at..end], &borrowed) {
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        drop(borrowed);
        fds.drain(..carried);
        at += n;
    }
    let mut w = to;
    w.write_all(&data[at..])
}

/// Carry `from`'s stream to `to` through `filter` until `from` ends, then end `to`'s write side.
pub(crate) fn pump(
    from: &UnixStream,
    to: &UnixStream,
    filter: &mut dyn Filter,
) -> Result<(), String> {
    let mut buf = vec![0u8; 1 << 16];
    let mut pending: Vec<u8> = Vec::new();
    let mut fds: Vec<OwnedFd> = Vec::new();
    loop {
        let n = recv(from, &mut buf, &mut fds).map_err(|e| format!("read: {e}"))?;
        if n == 0 {
            let _ = to.shutdown(std::net::Shutdown::Write);
            return if pending.is_empty() {
                Ok(())
            } else {
                Err(format!(
                    "the stream ended inside a message ({} bytes)",
                    pending.len()
                ))
            };
        }
        pending.extend_from_slice(&buf[..n]);
        let mut out = Vec::new();
        let mut at = 0;
        while let Some(len) = filter.len(&pending[at..])? {
            if len == 0 || len > MAX_MESSAGE {
                return Err(format!("a message of {len} bytes"));
            }
            if pending.len() - at < len {
                break;
            }
            let msg = &mut pending[at..at + len];
            if filter.pass(msg) {
                out.extend_from_slice(msg);
            }
            at += len;
        }
        pending.drain(..at);
        if !out.is_empty() {
            send(to, &out, &mut fds).map_err(|e| format!("write: {e}"))?;
        }
    }
}

/// Serve `listen` (a socket path, replaced if one is there) ([`pump`]).
pub(crate) fn serve<F, G>(
    listen: &Path,
    server: &Path,
    make: impl Fn() -> (F, G),
) -> Result<(), String>
where
    F: Filter + 'static,
    G: Filter + 'static,
{
    let _ = std::fs::remove_file(listen);
    let listener = UnixListener::bind(listen).map_err(|e| format!("{}: {e}", listen.display()))?;
    eprintln!(
        "lane-bus: relaying {} to {}",
        listen.display(),
        server.display()
    );
    for client in listener.incoming() {
        let client = match client {
            Ok(c) => c,
            Err(e) => {
                eprintln!("lane-bus: accept: {e}");
                continue;
            }
        };
        let upstream = match UnixStream::connect(server) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("lane-bus: {}: {e}", server.display());
                continue;
            }
        };
        let (mut up, mut down) = make();
        let (c2, u2) = match (client.try_clone(), upstream.try_clone()) {
            (Ok(c), Ok(u)) => (c, u),
            (Err(e), _) | (_, Err(e)) => {
                eprintln!("lane-bus: clone a socket: {e}");
                continue;
            }
        };
        std::thread::spawn(move || {
            if let Err(e) = pump(&client, &upstream, &mut up) {
                eprintln!("lane-bus: client to server: {e}");
            }
        });
        std::thread::spawn(move || {
            if let Err(e) = pump(&u2, &c2, &mut down) {
                eprintln!("lane-bus: server to client: {e}");
            }
            let _ = c2.shutdown(std::net::Shutdown::Both);
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cuts a stream into 4-byte messages and drops every one starting 0xff.
    struct Fours;

    impl Filter for Fours {
        fn len(&mut self, buf: &[u8]) -> Result<Option<usize>, String> {
            Ok((!buf.is_empty()).then_some(4))
        }

        fn pass(&mut self, msg: &mut [u8]) -> bool {
            msg[1] = msg[1].wrapping_add(1);
            msg[0] != 0xff
        }
    }

    #[test]
    fn a_stream_is_cut_into_messages_filtered_and_its_descriptors_carried() {
        let (mut a, b) = UnixStream::pair().unwrap();
        let (c, mut d) = UnixStream::pair().unwrap();
        let t = std::thread::spawn(move || pump(&b, &c, &mut Fours));
        let (e, mut f) = UnixStream::pair().unwrap();
        let mut fds = vec![OwnedFd::from(e)];
        send(&a, &[1, 1, 1], &mut fds).unwrap();
        use std::io::{Read as _, Write as _};
        a.write_all(&[1, 0xff, 1, 1, 1, 2, 3, 4]).unwrap();
        a.write_all(&[2, 2]).unwrap();
        drop(a);
        let mut got = Vec::new();
        let mut buf = [0u8; 64];
        let mut carried = Vec::new();
        loop {
            let n = recv(&d, &mut buf, &mut carried).unwrap();
            if n == 0 {
                break;
            }
            got.extend_from_slice(&buf[..n]);
        }
        assert_eq!(got, [1, 2, 1, 1, 2, 4, 4, 2]);
        assert!(
            t.join().unwrap().is_err(),
            "a stream ended inside a message"
        );
        assert_eq!(carried.len(), 1, "the descriptor came through");
        let mut through = UnixStream::from(carried.pop().unwrap());
        through.write_all(b"x").unwrap();
        let mut one = [0u8; 1];
        f.read_exact(&mut one).unwrap();
        assert_eq!(&one, b"x");
        let _ = d.flush();
    }
}
