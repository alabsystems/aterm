// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One AKP1 frame on a connected stream, with its descriptor.
//!
//! Every read goes through `fdpass::recv_with_fds`, the header's first read
//! declaring room for ONE descriptor and every later read for none: a
//! descriptor riding any byte but a frame's first, or riding a kind that
//! carries none, is closed and the connection refused — never silently
//! dropped by a plain `read`, never left in the table unnamed.

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

/// Read one frame. `Ok(None)` is a clean EOF at a frame boundary.
///
/// # Errors
/// A malformed frame (`InvalidData`), EOF mid-frame (`UnexpectedEof`), a
/// descriptor where none may ride, or the socket's own errors (a read timeout
/// reads as `WouldBlock`).
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
        if r.bytes == 0 && r.fds.is_empty() {
            return if got == 0 {
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
///
/// # Errors
/// A frame that will not encode, a descriptor on a kind that carries none (or
/// none on one that does), or the socket's own errors.
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
        Some(fd) => fdpass::send_with_fds(stream, &bytes, &[fd])?,
        None => 0,
    };
    let mut s = stream;
    s.write_all(&bytes[sent..])?;
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::wire::{Birth, MasterHeader};
    use std::os::fd::AsFd as _;
    use std::os::unix::fs::MetadataExt as _;

    #[test]
    fn a_register_carries_its_descriptor_and_a_bye_none() {
        let (a, b) = CtlStream::pair().expect("pair");
        let file = std::fs::File::open("/dev/null").expect("open");
        let rdev = file.metadata().expect("stat").rdev();
        let frame = Frame::Register {
            header: MasterHeader {
                rdev,
                shell_pid: 1,
                shell_birth: Birth::default(),
                local_id: 2,
            },
            tag: b"t".to_vec(),
        };
        write_frame(&a, &frame, Some(file.as_fd())).expect("write");
        write_frame(&a, &Frame::Bye, None).expect("write");
        let got = read_frame(&b).expect("read").expect("a frame");
        assert_eq!(got.frame, frame);
        let fd = got.fd.expect("the descriptor rode it");
        let back = std::fs::File::from(fd);
        assert_eq!(
            back.metadata().expect("stat").rdev(),
            rdev,
            "the same device"
        );
        let bye = read_frame(&b).expect("read").expect("a frame");
        assert_eq!(bye.frame, Frame::Bye);
        assert!(bye.fd.is_none());
        drop(a);
        assert!(read_frame(&b).expect("eof").is_none(), "clean EOF");
    }

    #[test]
    fn a_descriptor_on_the_wrong_kind_is_refused() {
        let (a, b) = CtlStream::pair().expect("pair");
        assert!(
            write_frame(&a, &Frame::Bye, Some(a.as_fd())).is_err(),
            "the writer refuses"
        );
        // A hand-rolled BYE header carrying a descriptor.
        let bytes = Frame::Bye.encode().expect("encode");
        fdpass::send_with_fds(&a, &bytes, &[a.as_fd()]).expect("send");
        let err = read_frame(&b).expect_err("refused");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }
}
