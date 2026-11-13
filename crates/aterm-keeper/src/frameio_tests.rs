// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
    write_frame(&a, &Frame::Bye { keep: Vec::new() }, None).expect("write");
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
    assert_eq!(bye.frame, Frame::Bye { keep: Vec::new() });
    assert!(bye.fd.is_none());
    drop(a);
    assert!(read_frame(&b).expect("eof").is_none(), "clean EOF");
}

#[test]
fn a_descriptor_on_the_wrong_kind_is_refused() {
    let (a, b) = CtlStream::pair().expect("pair");
    assert!(
        write_frame(&a, &Frame::Bye { keep: Vec::new() }, Some(a.as_fd())).is_err(),
        "the writer refuses"
    );
    let bytes = Frame::Bye { keep: Vec::new() }.encode().expect("encode");
    fdpass::send_with_fds(&a, &bytes, &[a.as_fd()]).expect("send");
    let err = read_frame(&b).expect_err("refused");
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn truncated_header_returns_unexpected_eof() {
    let (a, b) = CtlStream::pair().expect("pair");
    let bytes = Frame::Bye { keep: Vec::new() }.encode().expect("encode");
    fdpass::send_with_fds(&a, &bytes[..3], &[]).expect("partial header");
    drop(a);
    let err = read_frame(&b).expect_err("truncated header must fail");
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
}
