// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded nonblocking frames. EOF and matching bytes supply no native closure.
use super::{alive, refused};
use aterm_uds::CtlStream;
use std::{
    collections::VecDeque,
    io::{self, Read as _},
    os::fd::AsRawFd as _,
    time::Instant,
};
const MAX_FRAME: usize = 1024 * 1024;
pub(super) enum Peek {
    Waiting,
    Eof,
    Bytes,
}
pub(super) struct Socket {
    stream: CtlStream,
    incoming: Vec<u8>,
    length: Option<usize>,
    received: usize,
    output: VecDeque<Vec<u8>>,
    written: usize,
}
impl Socket {
    pub(super) fn new(stream: CtlStream) -> io::Result<Self> {
        stream.set_nonblocking(true)?;
        Ok(Self {
            stream,
            incoming: Vec::new(),
            length: None,
            received: 0,
            output: VecDeque::new(),
            written: 0,
        })
    }
    pub(super) fn push(&mut self, bytes: Vec<u8>) -> io::Result<()> {
        if bytes.is_empty() || bytes.len() > MAX_FRAME || self.output.len() >= 40 {
            return Err(refused("archive frame population exceeds closed bound"));
        }
        let mut framed = Vec::with_capacity(bytes.len() + 4);
        framed.extend((bytes.len() as u32).to_be_bytes());
        framed.extend(bytes);
        self.output.push_back(framed);
        Ok(())
    }
    pub(super) fn flush(&mut self, deadline: Instant) -> io::Result<bool> {
        for _ in 0..32 {
            alive(deadline)?;
            let Some(bytes) = self.output.front() else {
                return Ok(true);
            };
            match aterm_uds::fdpass::send_vectored(
                &self.stream,
                &[io::IoSlice::new(&bytes[self.written..])],
            ) {
                Ok(0) => return Err(refused("archive frame peer closed")),
                Ok(n) => {
                    self.written += n;
                    if self.written == bytes.len() {
                        self.output.pop_front();
                        self.written = 0;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(false),
                Err(e) => return Err(e),
            }
        }
        Ok(self.output.is_empty())
    }
    pub(super) fn receive(&mut self, deadline: Instant) -> io::Result<Option<Vec<u8>>> {
        for _ in 0..32 {
            alive(deadline)?;
            let goal = self.length.map_or(4, |n| n + 4);
            if self.incoming.len() == goal {
                if self.length.is_some() {
                    let frame = self.incoming.split_off(4);
                    self.incoming.clear();
                    self.length = None;
                    return Ok(Some(frame));
                }
                let n = u32::from_be_bytes(
                    self.incoming
                        .as_slice()
                        .try_into()
                        .expect("four frame bytes"),
                ) as usize;
                if n == 0 || n > MAX_FRAME {
                    return Err(refused("archive incoming frame exceeds bound"));
                }
                self.length = Some(n);
                continue;
            }
            let mut bytes = [0; 4096];
            let limit = bytes
                .len()
                .min(goal - self.incoming.len())
                .min((MAX_FRAME + 4).saturating_sub(self.received));
            if limit == 0 {
                return Err(refused("archive incoming transcript exceeds closed bound"));
            }
            match (&self.stream).read(&mut bytes[..limit]) {
                Ok(0) => return Err(refused("archive peer closed during frame")),
                Ok(n) => {
                    self.received += n;
                    self.incoming.extend_from_slice(&bytes[..n]);
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(None),
                Err(e) => return Err(e),
            }
        }
        Ok(None)
    }
    pub(super) fn peek(&self) -> io::Result<Peek> {
        Self::peek_original(&self.stream)
    }
    pub(super) fn peek_original(stream: &CtlStream) -> io::Result<Peek> {
        let mut byte = 0u8;
        // SAFETY: peek one byte through this retained socket, without changing
        // shared flags or consuming a later frame.
        let count = unsafe {
            libc::recv(
                stream.as_raw_fd(),
                (&raw mut byte).cast(),
                1,
                libc::MSG_PEEK | libc::MSG_DONTWAIT,
            )
        };
        match count {
            0 => Ok(Peek::Eof),
            1.. => Ok(Peek::Bytes),
            _ => {
                let e = io::Error::last_os_error();
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) {
                    Ok(Peek::Waiting)
                } else {
                    Err(e)
                }
            }
        }
    }
    pub(super) fn shutdown(&self, direction: std::net::Shutdown) -> io::Result<()> {
        self.stream.shutdown(direction)
    }
    /// Failed cleanup may discard only a bounded prefix through this SAME
    /// socket. It never parses an ACK/Run or supplies native retirement.
    /// Keep every physical read inside the original one-frame intake bound.
    pub(super) fn discard_to_eof(&mut self) -> io::Result<bool> {
        self.incoming.clear();
        self.length = None;
        for _ in 0..16 {
            let remaining = (MAX_FRAME + 4).saturating_sub(self.received);
            if remaining == 0 {
                return match self.peek()? {
                    Peek::Eof => Ok(true),
                    _ => Err(refused(
                        "failed archive control exceeds original intake bound",
                    )),
                };
            }
            let mut bytes = [0; 4096];
            let limit = bytes.len().min(remaining);
            match (&self.stream).read(&mut bytes[..limit]) {
                Ok(0) => return Ok(true),
                Ok(n) => self.received += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(false),
                Err(e) => return Err(e),
            }
        }
        Ok(false)
    }
}

pub(super) struct Delivery {
    socket: Socket,
    expected: Vec<u8>,
    write_closed: bool,
    acknowledged: bool,
}
impl Delivery {
    pub(super) fn new(
        stream: CtlStream,
        frames: Vec<Vec<u8>>,
        expected: Vec<u8>,
    ) -> io::Result<Self> {
        let mut socket = Socket::new(stream)?;
        for frame in frames {
            socket.push(frame)?;
        }
        Ok(Self {
            socket,
            expected,
            write_closed: false,
            acknowledged: false,
        })
    }
    pub(super) fn poll(&mut self, deadline: Instant) -> io::Result<bool> {
        alive(deadline)?;
        if !self.write_closed {
            if !matches!(self.socket.peek()?, Peek::Waiting) {
                return Err(refused(
                    "archive peer acknowledged or closed before terminal EOF",
                ));
            }
            if !self.socket.flush(deadline)? {
                return Ok(false);
            }
            self.socket.shutdown(std::net::Shutdown::Write)?;
            self.write_closed = true;
        }
        if !self.acknowledged {
            let Some(frame) = self.socket.receive(deadline)? else {
                return Ok(false);
            };
            if frame != self.expected {
                return Err(refused(
                    "archive ACK differs from exact original request and terminal",
                ));
            }
            self.acknowledged = true;
        }
        match self.socket.peek()? {
            Peek::Waiting => Ok(false),
            Peek::Eof => Ok(true),
            Peek::Bytes => Err(refused("archive ACK has trailing bytes")),
        }
    }
    pub(super) fn abort(&self) -> io::Result<()> {
        self.socket.shutdown(std::net::Shutdown::Both)
    }
}

#[cfg(test)]
#[path = "io/tests.rs"]
mod tests;
