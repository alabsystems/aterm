// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::io::Read as _;

use super::*;

#[test]
fn messages_and_their_descriptors_arrive_in_order_across_any_split() {
    let (a, b) = UnixStream::pair().unwrap();
    let (p1, mut p1_far) = UnixStream::pair().unwrap();
    let (p2, mut p2_far) = UnixStream::pair().unwrap();
    let sender = Channel::new(a);
    sender.send_reply(&Reply::Started, None).unwrap();
    sender
        .send_reply(&Reply::Connected { id: 1, port: 1024 }, Some(p1.as_fd()))
        .unwrap();
    sender
        .send_reply(&Reply::Booting("between".into()), None)
        .unwrap();
    sender
        .send_reply(&Reply::Connected { id: 2, port: 1024 }, Some(p2.as_fd()))
        .unwrap();
    drop(sender);
    drop((p1, p2));
    let mut r = Channel::new(b);
    assert!(matches!(
        r.recv_reply().unwrap(),
        Some((Reply::Started, None))
    ));
    let (reply, fd) = r.recv_reply().unwrap().unwrap();
    assert_eq!(reply, Reply::Connected { id: 1, port: 1024 });
    let mut first = UnixStream::from(fd.expect("first descriptor"));
    assert!(matches!(
        r.recv_reply().unwrap(),
        Some((Reply::Booting(_), None))
    ));
    let (reply, fd) = r.recv_reply().unwrap().unwrap();
    assert_eq!(reply, Reply::Connected { id: 2, port: 1024 });
    let mut second = UnixStream::from(fd.expect("second descriptor"));
    assert!(r.recv_reply().unwrap().is_none(), "EOF between messages");
    // Each descriptor is the one sent with its message, live in this process.
    first.write_all(b"one").unwrap();
    second.write_all(b"two").unwrap();
    let mut got = [0u8; 3];
    p1_far.read_exact(&mut got).unwrap();
    assert_eq!(&got, b"one");
    p2_far.read_exact(&mut got).unwrap();
    assert_eq!(&got, b"two");
}

#[test]
fn a_stream_cut_inside_a_message_is_truncated() {
    let (a, b) = UnixStream::pair().unwrap();
    let bytes = Request::Connect { port: 1024 }.encode().unwrap();
    (&a).write_all(&bytes[..bytes.len() - 1]).unwrap();
    drop(a);
    let mut r = Channel::new(b);
    assert!(matches!(r.recv_request(), Err(ProtoError::Truncated)));
}

#[test]
fn a_connected_without_its_descriptor_is_refused() {
    let (a, b) = UnixStream::pair().unwrap();
    let sender = Channel::new(a);
    assert!(matches!(
        sender.send_reply(&Reply::Connected { id: 9, port: 1 }, None),
        Err(ProtoError::NoDescriptor)
    ));
    let (extra, _extra_far) = UnixStream::pair().unwrap();
    assert!(matches!(
        sender.send_reply(&Reply::Started, Some(extra.as_fd())),
        Err(ProtoError::Malformed("unexpected descriptor for reply"))
    ));

    let (raw_a, raw_b) = UnixStream::pair().unwrap();
    (&raw_a)
        .write_all(&Reply::Connected { id: 9, port: 1 }.encode().unwrap())
        .unwrap();
    drop(raw_a);
    let mut r = Channel::new(raw_b);
    assert!(matches!(r.recv_reply(), Err(ProtoError::NoDescriptor)));

    let (req_a, req_b) = UnixStream::pair().unwrap();
    send(
        &req_a,
        &Request::State.encode().unwrap(),
        Some(extra.as_fd()),
    )
    .unwrap();
    drop((req_a, b));
    let mut req_r = Channel::new(req_b);
    assert!(matches!(
        req_r.recv_request(),
        Err(ProtoError::Malformed("unexpected descriptor on request"))
    ));

    let (rep_a, rep_b) = UnixStream::pair().unwrap();
    send(
        &rep_a,
        &Reply::Started.encode().unwrap(),
        Some(extra.as_fd()),
    )
    .unwrap();
    drop(rep_a);
    let mut rep_r = Channel::new(rep_b);
    assert!(matches!(
        rep_r.recv_reply(),
        Err(ProtoError::Malformed("unexpected descriptor on reply"))
    ));

    let (dup_a, dup_b) = UnixStream::pair().unwrap();
    let (extra2, _extra2_far) = UnixStream::pair().unwrap();
    aterm_uds::fdpass::send_with_fds(
        &dup_a,
        &Reply::Connected { id: 1, port: 1024 }.encode().unwrap(),
        &[extra.as_fd(), extra2.as_fd()],
    )
    .unwrap();
    drop((dup_a, extra, extra2));
    let mut dup_r = Channel::new(dup_b);
    assert!(matches!(
        dup_r.recv_reply(),
        Err(ProtoError::Malformed("unexpected descriptor on reply"))
    ));
}

#[test]
fn a_read_past_the_span_fails_rather_than_waits() {
    let (_a, b) = UnixStream::pair().unwrap();
    let mut r = Channel::new(b);
    r.set_span(Some(Duration::from_millis(20))).unwrap();
    match r.recv_reply() {
        Err(ProtoError::Io(e)) => assert!(
            matches!(
                e.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ),
            "{e}"
        ),
        other => panic!("{other:?}"),
    }
}
