// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One AKP1 frame on a connected stream, with its descriptor.

use std::io::{self, Write as _};
use std::os::fd::{BorrowedFd, OwnedFd};

use aterm_uds::CtlStream;
use aterm_uds::fdpass;

use crate::wire::{Frame, HEADER_LEN, WireError, decode_header};

/// A frame read off the wire, and the descriptor that rode it.
#[derive(Debug)]
pub struct Incoming {
    pub frame: Frame,
    pub fd: Option<OwnedFd>,
}

fn wire(e: WireError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e.to_string())
}

/// Read one frame.
/// # Errors
/// A malformed frame (`InvalidData`), EOF mid-frame (`UnexpectedEof`), a descriptor where none may
/// ride, or the socket's own errors (a read timeout reads as `WouldBlock`).
pub fn read_frame(stream: &CtlStream) -> io::Result<Option<Incoming>> {
    let mut hdr = [0u8; HEADER_LEN];
    let mut got = 0usize;
    let mut fd: Option<OwnedFd> = None;
    while got < HEADER_LEN {
        let max = usize::from(got == 0);
        let r = match fdpass::recv_with_fds(stream, &mut hdr[got..], max) {
            Ok(r) => r,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if r.bytes == 0 {
            return if got == 0 && r.fds.is_empty() {
                Ok(None)
            } else {
                Err(io::ErrorKind::UnexpectedEof.into())
            };
        }
        if let Some(f) = r.fds.into_iter().next() {
            fd = Some(f);
        }
        got += r.bytes;
    }
    let header = decode_header(&hdr).map_err(wire)?;
    if fd.is_some() && !header.kind.carries_fd() {
        return Err(wire(WireError::FdFlag(header.kind)));
    }
    let mut body = vec![0u8; header.len];
    let mut at = 0usize;
    while at < body.len() {
        let r = match fdpass::recv_with_fds(stream, &mut body[at..], 0) {
            Ok(r) => r,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if r.bytes == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        at += r.bytes;
    }
    let frame = Frame::decode(&header, &body).map_err(wire)?;
    Ok(Some(Incoming { frame, fd }))
}

/// Write one frame, with `fd` riding its first byte when the kind carries one.
/// # Errors
/// A frame that will not encode, a descriptor on a kind that carries none (or none on one that
/// does), or the socket's own errors.
pub fn write_frame(
    stream: &CtlStream,
    frame: &Frame,
    fd: Option<BorrowedFd<'_>>,
) -> io::Result<()> {
    let bytes = frame.encode().map_err(wire)?;
    if fd.is_some() != frame.kind().carries_fd() {
        return Err(wire(WireError::FdFlag(frame.kind())));
    }
    let sent = match fd {
        Some(fd) => loop {
            match fdpass::send_with_fds(stream, &bytes, &[fd]) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(n) => break n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        },
        None => 0,
    };
    let mut s = stream;
    s.write_all(&bytes[sent..])?;
    Ok(())
}

#[cfg(all(test, unix))]
#[path = "frameio_tests.rs"]
mod tests;
