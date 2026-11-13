// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn round(frame: &Frame) {
    let bytes = encode(frame).unwrap();
    assert_eq!(
        bytes.capacity(),
        bytes.len(),
        "encoded output retains only its wire size"
    );
    assert_eq!(&decode(&bytes[4..]).unwrap(), frame);
    let len = u32::from_be_bytes(bytes[..4].try_into().unwrap()) as usize;
    assert_eq!(len, bytes.len() - 4);
    let body = &bytes[4..];
    let parity = |body: &[u8]| {
        let owned = decode(body)
            .map(|_| ())
            .map_err(|error| (error.kind(), error.to_string()));
        let checked = decode_by::<false>(body)
            .map(|_| ())
            .map_err(|error| (error.kind(), error.to_string()));
        assert_eq!(checked, owned, "ownership must not change wire validation");
    };
    for end in 0..=body.len() {
        parity(&body[..end]);
    }
    for index in 0..body.len() {
        for replacement in [0, 0x80, 0xff] {
            let mut changed = body.to_vec();
            changed[index] = replacement;
            parity(&changed);
        }
    }
}

#[test]
fn every_frame_survives_the_wire() {
    let v6: SocketAddr = "[fe80::1%7]:7484".parse().unwrap();
    for frame in [
        Frame::Hello {
            protocol: 1,
            build: "0.155.0+gabc".into(),
        },
        Frame::Refused {
            words: "not aterm".into(),
        },
        Frame::Unchecked {
            words: "trustd did not answer".into(),
        },
        Frame::Request(Request::Connect {
            target: Target::Host("m11.local".into()),
            port: 22,
            within_ms: 5000,
            bound_if: Some(4),
            source: Some("192.168.1.2".parse().unwrap()),
            hop_limit: Some(1),
        }),
        Frame::Request(Request::Connect {
            target: Target::Addr(v6),
            port: 7484,
            within_ms: 1,
            bound_if: None,
            source: None,
            hop_limit: None,
        }),
        Frame::Request(Request::Udp {
            bind: "0.0.0.0:5353".parse().unwrap(),
            reuse: true,
            groups: vec![
                ("224.0.0.251".parse().unwrap(), Some(4)),
                ("ff02::fb".parse().unwrap(), None),
            ],
            multicast_if: Some(4),
            ttl: Some(255),
        }),
        Frame::Request(Request::Resolve {
            name: "m11.local".into(),
            within_ms: 3000,
        }),
        Frame::Request(Request::Browse {
            service: "_alab._tcp".into(),
            domain: String::new(),
        }),
        Frame::Request(Request::ResolveService {
            name: "m11".into(),
            service: "_alab._tcp".into(),
            domain: "local.".into(),
        }),
        Frame::Request(Request::Query {
            name: "m11.local.".into(),
            rrtype: 28,
        }),
        Frame::Request(Request::Register(Advert {
            name: "vault".into(),
            service: "_alab-vault._tcp".into(),
            domain: "local.".into(),
            port: 7100,
            txt: vec![b"v=1".to_vec(), Vec::new()],
            interface: Some(0),
            host: Some("vault-host.local.".into()),
            addresses: vec!["192.168.1.9".parse().unwrap(), "fe80::9".parse().unwrap()],
        })),
        Frame::Socket { reached: Some(v6) },
        Frame::Socket { reached: None },
        Frame::Addresses(vec!["10.0.0.1".parse().unwrap()]),
        Frame::Event(Event::Added(Instance {
            name: "m11".into(),
            service: "_alab._tcp.".into(),
            domain: "local.".into(),
            interface: 4,
        })),
        Frame::Event(Event::Removed(Instance {
            name: "m11".into(),
            service: "_alab._tcp.".into(),
            domain: "local.".into(),
            interface: 4,
        })),
        Frame::Event(Event::Resolved {
            host: "m11.local.".into(),
            port: 7484,
            txt: vec![b"a=b".to_vec()],
            interface: 4,
        }),
        Frame::Event(Event::Record {
            rrtype: 1,
            rdata: vec![192, 168, 1, 9],
            ttl: 120,
            interface: 4,
            added: true,
        }),
        Frame::Event(Event::Registered {
            name: "vault (2)".into(),
            service: "_alab-vault._tcp.".into(),
            domain: "local.".into(),
        }),
        Frame::Error {
            code: libc::EACCES,
            words: "not local network".into(),
        },
    ] {
        round(&frame);
    }
}

#[test]
fn every_length_is_bounded_before_it_is_believed() {
    // A text field claiming 4 GiB, in a five-byte body: refused, nothing allocated for it.
    let mut body = vec![REFUSED];
    body.extend_from_slice(&u32::MAX.to_be_bytes());
    assert_eq!(
        decode(&body).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    // A list claiming more items than bytes are left.
    let mut body = vec![ADDRESSES];
    body.extend_from_slice(&1000u32.to_be_bytes());
    assert_eq!(
        decode(&body).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    // A TXT string over 255 bytes, from either side.
    let advert = Advert {
        txt: vec![vec![b'x'; 256]],
        ..Advert::default()
    };
    assert_eq!(
        encode(&Frame::Request(Request::Register(advert)))
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    // Bytes past the last field, an unknown tag, an empty body.
    let mut extra = encode(&Frame::Refused { words: "x".into() }).unwrap();
    extra.push(0);
    assert!(decode(&extra[4..]).is_err());
    assert!(decode(&[200]).is_err());
    assert!(decode(&[]).is_err());
    // A whole frame over the bound is refused before it is sent.
    let huge = Frame::Error {
        code: 0,
        words: "x".repeat(MAX_TEXT + 1),
    };
    assert!(encode(&huge).is_err());
}

#[test]
fn a_reader_refuses_an_oversized_length_before_reading_its_body() {
    let (ours, theirs) = CtlStream::pair().unwrap();
    let claimed = u32::try_from(MAX_FRAME + 1).unwrap();
    (&theirs).write_all(&claimed.to_be_bytes()).unwrap();
    let mut reader = Reader::new(0);
    let deadline = Instant::now() + std::time::Duration::from_secs(60);
    let refused = reader.frame(&ours, Some(deadline)).unwrap_err();
    assert_eq!(refused.kind(), io::ErrorKind::InvalidData, "{refused}");
    assert!(refused.to_string().contains("bound"), "{refused}");
}

#[test]
fn coalesced_frames_survive_every_partial_tail_and_refuse_a_bad_next_length() {
    let frames = [
        Frame::Socket { reached: None },
        Frame::Hello {
            protocol: 7,
            build: "split".into(),
        },
        Frame::Refused {
            words: "end".into(),
        },
    ];
    let wire: Vec<_> = frames
        .iter()
        .flat_map(|frame| encode(frame).unwrap())
        .collect();
    for split in 0..=wire.len() {
        let mut reader = Reader::new(0);
        let mut found = Vec::new();
        reader.append(&wire[..split]);
        while let Some(frame) = reader.complete().unwrap() {
            found.push(frame);
        }
        reader.append(&wire[split..]);
        while let Some(frame) = reader.complete().unwrap() {
            found.push(frame);
        }
        assert_eq!(found, frames, "split at {split}");
        assert!(!reader.buffered());
    }
    let mut reader = Reader::new(0);
    reader.append(&encode(&frames[0]).unwrap());
    reader.append(&u32::try_from(MAX_FRAME + 1).unwrap().to_be_bytes());
    assert_eq!(reader.complete().unwrap(), Some(frames[0].clone()));
    assert_eq!(
        reader.complete().unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn a_consumed_frame_keeps_its_partial_successor_through_timeout_or_eof() {
    let first = Frame::Socket { reached: None };
    let second = Frame::Hello {
        protocol: 7,
        build: "tail".into(),
    };
    let encoded = encode(&second).unwrap();
    for finish in [false, true] {
        let (ours, mut theirs) = CtlStream::pair().unwrap();
        let mut prefix = encode(&first).unwrap();
        prefix.extend_from_slice(&encoded[..2]);
        theirs.write_all(&prefix).unwrap();
        let mut reader = Reader::new(0);
        let deadline = Some(Instant::now() + std::time::Duration::from_secs(60));
        assert_eq!(reader.frame(&ours, deadline).unwrap(), Some(first.clone()));
        if finish {
            assert_eq!(
                reader
                    .frame(&ours, Some(Instant::now()))
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::TimedOut
            );
            theirs.write_all(&encoded[2..]).unwrap();
            drop(theirs);
            assert_eq!(reader.frame(&ours, deadline).unwrap(), Some(second.clone()));
            assert!(!reader.buffered());
            assert_eq!(reader.frame(&ours, deadline).unwrap(), None);
        } else {
            drop(theirs);
            assert_eq!(
                reader.frame(&ours, deadline).unwrap_err().kind(),
                io::ErrorKind::UnexpectedEof
            );
            assert!(reader.buffered());
        }
    }
}

#[test]
fn a_large_successor_survives_compaction_and_multiple_receive_quanta() {
    let frames = [
        Frame::Socket { reached: None },
        Frame::Event(Event::Record {
            rrtype: 16,
            rdata: vec![0x5a; 65_535],
            ttl: 60,
            interface: 7,
            added: true,
        }),
    ];
    let wire: Vec<_> = frames
        .iter()
        .flat_map(|frame| encode(frame).unwrap())
        .collect();
    for quantum in [3, 16 * 1024, 65_536] {
        let mut reader = Reader::new(0);
        let mut found = Vec::new();
        for bytes in wire.chunks(quantum) {
            reader.append(bytes);
            while let Some(frame) = reader.complete().unwrap() {
                found.push(frame);
            }
        }
        assert_eq!(found, frames, "receive quantum {quantum}");
        assert!(!reader.buffered());
    }
}

#[test]
fn a_reader_joins_split_frames_takes_a_passed_descriptor_and_ends_cleanly() {
    let (ours, theirs) = CtlStream::pair().unwrap();
    let hello = encode(&Frame::Hello {
        protocol: 7,
        build: "b".into(),
    })
    .unwrap();
    (&theirs).write_all(&hello[..3]).unwrap();
    (&theirs).write_all(&hello[3..]).unwrap();
    let (kept, _other) = CtlStream::pair().unwrap();
    send_with_fd(&theirs, &Frame::Socket { reached: None }, kept.as_fd()).unwrap();
    drop(theirs);
    let mut reader = Reader::new(1);
    let deadline = Some(Instant::now() + std::time::Duration::from_secs(60));
    assert_eq!(
        reader.frame(&ours, deadline).unwrap(),
        Some(Frame::Hello {
            protocol: 7,
            build: "b".into()
        })
    );
    assert_eq!(
        reader.frame(&ours, deadline).unwrap(),
        Some(Frame::Socket { reached: None })
    );
    assert!(reader.take_fd().is_some());
    assert_eq!(reader.frame(&ours, deadline).unwrap(), None);
}

#[test]
fn a_pass_holds_the_descriptor_until_the_peer_took_it_in() {
    let deadline = Instant::now() + std::time::Duration::from_secs(60);
    let (ours, theirs) = CtlStream::pair().unwrap();
    let (kept, _other) = CtlStream::pair().unwrap();
    let passing = std::thread::spawn(move || {
        pass(
            &theirs,
            &Frame::Socket { reached: None },
            kept.as_fd(),
            deadline,
        )
    });
    let mut reader = Reader::new(1);
    assert_eq!(
        reader.frame(&ours, Some(deadline)).unwrap(),
        Some(Frame::Socket { reached: None })
    );
    assert!(reader.take_fd().is_some());
    assert!(!passing.is_finished(), "the pass waits for the peer's word");
    took(&ours).unwrap();
    passing.join().unwrap().unwrap();

    let (ours, theirs) = CtlStream::pair().unwrap();
    let (kept, _other) = CtlStream::pair().unwrap();
    let passing = std::thread::spawn(move || {
        pass(
            &theirs,
            &Frame::Socket { reached: None },
            kept.as_fd(),
            deadline,
        )
    });
    let mut reader = Reader::new(1);
    reader.frame(&ours, Some(deadline)).unwrap();
    drop(reader);
    drop(ours);
    assert_eq!(
        passing.join().unwrap().unwrap_err().kind(),
        io::ErrorKind::UnexpectedEof
    );
}

#[test]
fn a_reader_that_takes_no_descriptor_refuses_one() {
    let (ours, theirs) = CtlStream::pair().unwrap();
    let (kept, _other) = CtlStream::pair().unwrap();
    send_with_fd(&theirs, &Frame::Socket { reached: None }, kept.as_fd()).unwrap();
    let mut reader = Reader::new(0);
    let deadline = Some(Instant::now() + std::time::Duration::from_secs(60));
    assert_eq!(
        reader.frame(&ours, deadline).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}
