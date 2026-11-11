// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn call() -> Message {
    let mut m = Message::method_call(Some(":1.9"), "/a", Some("org.a.B"), "C").unwrap();
    m.serial = 7;
    m
}

fn refused(bytes: &[u8]) -> Error {
    Message::decode(bytes).expect_err("decoded")
}

#[test]
fn a_message_round_trips() {
    let m = call().with_body(vec![Value::from("x"), Value::U32(3)]);
    let bytes = m.encode().unwrap();
    assert_eq!(frame_len(&bytes).unwrap(), Some(bytes.len()));
    assert_eq!(frame_len(&bytes[..15]).unwrap(), None);
    assert_eq!(Message::decode(&bytes).unwrap(), m);
}

#[test]
fn encode_holds_the_specification() {
    let mut m = call();
    m.serial = 0;
    assert!(matches!(m.encode(), Err(Error::Invalid("serial 0"))));
    let mut m = call();
    m.member = None;
    assert!(matches!(m.encode(), Err(Error::Invalid(_))));
    let mut m = call();
    m.sender = Some("not a name".into());
    assert!(matches!(m.encode(), Err(Error::Invalid("sender"))));
    assert!(Message::method_call(None, "/a/", None, "C").is_err());
    assert!(Message::signal("/a", "noDots", "C").is_err());
    assert!(Message::error(&call(), "bad", "x").is_err());
}

#[test]
fn decode_refuses_a_broken_header() {
    let good = call().encode().unwrap();
    let mut bad = good.clone();
    bad[0] = b'x';
    assert!(matches!(
        refused(&bad),
        Error::Malformed {
            what: "unknown byte order",
            ..
        }
    ));
    let mut bad = good.clone();
    bad[3] = 2;
    assert!(matches!(
        refused(&bad),
        Error::Malformed {
            what: "protocol version other than 1",
            ..
        }
    ));
    let mut bad = good.clone();
    bad[1] = 9;
    assert!(matches!(refused(&bad), Error::Unsupported(_)));
    let mut bad = good.clone();
    bad[8..12].copy_from_slice(&[0; 4]);
    assert!(matches!(
        refused(&bad),
        Error::Malformed {
            what: "serial 0",
            ..
        }
    ));
    assert!(matches!(
        refused(&good[..good.len() - 1]),
        Error::Malformed { .. }
    ));
    let mut long = good.clone();
    long.push(0);
    assert!(matches!(
        refused(&long),
        Error::Malformed {
            what: "length disagrees with the header",
            ..
        }
    ));
}

#[test]
fn decode_refuses_repeated_mistyped_and_missing_fields() {
    let build = |fields: &[(u8, Value)], kind: Kind| {
        let mut w = Writer::new(Endian::Little, 0);
        w.buf.extend_from_slice(&[b'l', kind as u8, 0, 1]);
        w.u32(0).unwrap();
        w.u32(5).unwrap();
        w.u32(0).unwrap();
        w.align(8).unwrap();
        for (code, value) in fields {
            w.align(8).unwrap();
            w.buf.push(*code);
            w.value(&Value::variant(value.clone())).unwrap();
        }
        let len = w.buf.len() - 16;
        w.patch_u32(12, len as u32);
        w.align(8).unwrap();
        w.buf
    };
    let path = || Value::path("/a").unwrap();
    let ok = build(
        &[(PATH, path()), (MEMBER, "M".into()), (42, Value::U64(1))],
        Kind::MethodCall,
    );
    assert_eq!(
        Message::decode(&ok).unwrap().member.as_deref(),
        Some("M"),
        "an unknown field is ignored"
    );
    let twice = build(
        &[(PATH, path()), (MEMBER, "M".into()), (MEMBER, "N".into())],
        Kind::MethodCall,
    );
    assert!(matches!(
        refused(&twice),
        Error::Malformed {
            what: "header field repeated",
            ..
        }
    ));
    let mistyped = build(
        &[(PATH, "/a".into()), (MEMBER, "M".into())],
        Kind::MethodCall,
    );
    assert!(matches!(
        refused(&mistyped),
        Error::Malformed {
            what: "header field of the wrong type or form",
            ..
        }
    ));
    let bad_name = build(&[(PATH, path()), (MEMBER, "M.N".into())], Kind::MethodCall);
    assert!(matches!(refused(&bad_name), Error::Malformed { .. }));
    let no_member = build(&[(PATH, path())], Kind::MethodCall);
    assert!(matches!(
        refused(&no_member),
        Error::Malformed {
            what: "method call without a path or member",
            ..
        }
    ));
    let invalid = build(
        &[(REPLY_SERIAL, Value::U32(1)), (0, Value::U32(1))],
        Kind::MethodReturn,
    );
    assert!(matches!(
        refused(&invalid),
        Error::Malformed {
            what: "header field code 0 (INVALID)",
            ..
        }
    ));
    let no_reply_serial = build(&[], Kind::MethodReturn);
    assert!(matches!(refused(&no_reply_serial), Error::Malformed { .. }));
    let zero_reply = build(&[(REPLY_SERIAL, Value::U32(0))], Kind::MethodReturn);
    assert!(matches!(refused(&zero_reply), Error::Malformed { .. }));
    let fds = build(
        &[(REPLY_SERIAL, Value::U32(1)), (UNIX_FDS, Value::U32(1))],
        Kind::MethodReturn,
    );
    assert!(matches!(refused(&fds), Error::Unsupported(_)));
    let no_fds = build(
        &[(REPLY_SERIAL, Value::U32(1)), (UNIX_FDS, Value::U32(0))],
        Kind::MethodReturn,
    );
    assert!(Message::decode(&no_fds).is_ok());
    let mut body = build(
        &[
            (REPLY_SERIAL, Value::U32(1)),
            (SIGNATURE, Value::Sig(Signature::new("u").unwrap())),
        ],
        Kind::MethodReturn,
    );
    assert!(matches!(refused(&body), Error::Malformed { .. }));
    body[4] = 4;
    body.extend_from_slice(&[1, 0, 0, 0]);
    assert_eq!(Message::decode(&body).unwrap().body, vec![Value::U32(1)]);
}

#[test]
fn frame_len_refuses_lengths_past_the_limits() {
    let mut head = call().encode().unwrap();
    head[12..16].copy_from_slice(&(1u32 << 27).to_le_bytes());
    assert!(frame_len(&head).is_err());
    let mut head = call().encode().unwrap();
    head[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(frame_len(&head).is_err());
}
