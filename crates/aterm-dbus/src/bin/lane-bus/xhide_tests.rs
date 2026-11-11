// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn setup_request() -> Vec<u8> {
    let mut s = vec![b'l', 0, 11, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    s[6] = 3;
    s.extend_from_slice(b"abc\0");
    s
}

fn query(name: &str) -> Vec<u8> {
    let mut q = vec![QUERY_EXTENSION, 0, 0, 0];
    q.extend_from_slice(&u16::try_from(name.len()).unwrap().to_le_bytes());
    q.extend_from_slice(&[0, 0]);
    q.extend_from_slice(name.as_bytes());
    q.resize(pad4(q.len()), 0);
    let words = u16::try_from(q.len() / 4).unwrap();
    q[2..4].copy_from_slice(&words.to_le_bytes());
    q
}

fn reply(seq: u16) -> Vec<u8> {
    let mut r = vec![0u8; 32];
    r[0] = REPLY;
    r[2..4].copy_from_slice(&seq.to_le_bytes());
    r[8..12].copy_from_slice(&[1, 131, 80, 150]);
    r
}

/// Feed `stream` through `f` whole, as the relay cuts it.
fn through(f: &mut dyn Filter, mut stream: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(n) = f.len(stream).unwrap() {
        let mut m = stream[..n].to_vec();
        if f.pass(&mut m) {
            out.extend_from_slice(&m);
        }
        stream = &stream[n..];
    }
    assert!(stream.is_empty(), "{} bytes left", stream.len());
    out
}

#[test]
fn only_the_hidden_extension_is_answered_absent() {
    let asked = Asked::default();
    let mut req = Requests {
        setup: false,
        seq: 0,
        hide: vec!["XInputExtension".into()],
        asked: Arc::clone(&asked),
        xi1: None,
    };
    let mut stream = setup_request();
    stream.extend(query("RANDR"));
    stream.extend(query("XInputExtension"));
    stream.extend([127, 0, 0, 0, 3, 0, 0, 0, 9, 9, 9, 9]);
    stream.extend(query("XKEYBOARD"));
    assert_eq!(
        through(&mut req, &stream),
        stream,
        "requests pass unchanged"
    );
    assert_eq!(*asked.lock().unwrap(), [(2, "XInputExtension".to_string())]);

    let mut rep = Replies {
        setup: false,
        asked: Arc::clone(&asked),
        xi1: None,
    };
    let mut setup = vec![1, 0, 11, 0, 0, 0, 2, 0];
    setup.extend([7u8; 8]);
    let mut stream = setup.clone();
    for seq in 1..=4 {
        stream.extend(reply(seq));
    }
    let mut xge = vec![GENERIC_EVENT; 1];
    xge.resize(32, 0);
    xge[4] = 2;
    xge.extend([5u8; 8]);
    stream.extend(&xge);
    stream.extend([2u8; 32]);
    let out = through(&mut rep, &stream);
    let mut want = setup;
    for seq in 1..=4 {
        let mut r = reply(seq);
        if seq == 2 {
            r[8..12].fill(0);
        }
        want.extend(r);
    }
    want.extend(&xge);
    want.extend([2u8; 32]);
    assert_eq!(out, want);
    assert!(asked.lock().unwrap().is_empty(), "the query was answered");
}

/// Under `XInput2`: XInputExtension is answered present, its opcode
/// kept, and only its `XIQueryVersion` is answered with a `Request`
/// error; a reply to another of its requests passes.
#[test]
fn xinput2_alone_is_hidden_as_an_xinput_1_server_lacks_it() {
    let asked = Asked::default();
    let xi1 = SharedXi1::default();
    let mut req = Requests {
        setup: false,
        seq: 0,
        hide: vec![XINPUT2.into()],
        asked: Arc::clone(&asked),
        xi1: Some(Arc::clone(&xi1)),
    };
    let mut rep = Replies {
        setup: false,
        asked: Arc::clone(&asked),
        xi1: Some(Arc::clone(&xi1)),
    };
    let mut setup = vec![1, 0, 11, 0, 0, 0, 2, 0];
    setup.extend([7u8; 8]);
    through(&mut req, &setup_request());
    through(&mut rep, &setup);
    through(&mut req, &query(XINPUT));
    assert_eq!(
        through(&mut rep, &reply(1)),
        reply(1),
        "XInputExtension stays present"
    );
    let xi_query_version = [131, XI_QUERY_VERSION, 2, 0, 2, 0, 3, 0];
    let xi_get_client_pointer = [131, 45, 2, 0, 0, 0, 0, 0];
    let core = [XI_QUERY_VERSION, 0, 2, 0, 0, 0, 0, 0];
    for r in [&xi_query_version[..], &xi_get_client_pointer, &core] {
        assert_eq!(through(&mut req, r), r, "requests pass unchanged");
    }
    let mut error = vec![0u8; 32];
    error[1] = BAD_REQUEST;
    error[2] = 2;
    error[8] = XI_QUERY_VERSION;
    error[10] = 131;
    assert_eq!(through(&mut rep, &reply(2)), error);
    assert_eq!(through(&mut rep, &reply(3)), reply(3));
    assert_eq!(through(&mut rep, &reply(4)), reply(4));
    assert!(asked.lock().unwrap().is_empty(), "nothing answered absent");
    assert!(xi1.lock().unwrap().versions.is_empty());
}

#[test]
fn a_big_endian_client_and_a_zero_length_request_are_refused() {
    let mut req = Requests {
        setup: false,
        seq: 0,
        hide: Vec::new(),
        asked: Asked::default(),
        xi1: None,
    };
    let mut b = setup_request();
    b[0] = b'B';
    assert!(req.len(&b).is_err());
    req.setup = true;
    assert!(req.len(&[1, 0, 0, 0, 0, 0, 0, 0]).is_err());
    assert_eq!(socket(":93").unwrap(), Path::new("/tmp/.X11-unix/X93"));
    assert!(socket("host:0").is_err());
}
