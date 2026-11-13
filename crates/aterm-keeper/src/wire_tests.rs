// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn round(frame: Frame) {
    let bytes = frame.encode().expect("encode");
    let mut h = [0u8; HEADER_LEN];
    h.copy_from_slice(&bytes[..HEADER_LEN]);
    let header = decode_header(&h).expect("header");
    assert_eq!(header.len, bytes.len() - HEADER_LEN);
    assert_eq!(header.flags & FLAG_FD != 0, frame.kind().carries_fd());
    let back = Frame::decode(&header, &bytes[HEADER_LEN..]).expect("decode");
    assert_eq!(back, frame);
}

fn header() -> MasterHeader {
    MasterHeader {
        rdev: 0x0f00_000a,
        shell_pid: 4242,
        shell_birth: Birth {
            seconds: 1_790_000_000,
            micros: 123_456,
        },
        local_id: 7,
    }
}

#[test]
fn every_frame_round_trips() {
    round(Frame::Hello {
        proto: PROTO,
        class: PeerClass::App,
        caps: CAP_SENDS_BYE,
        build: "0.98.0+gdeadbeef".into(),
        marker: None,
    });
    round(Frame::Hello {
        proto: PROTO,
        class: PeerClass::App,
        caps: CAP_SENDS_BYE,
        build: "0.98.0+gdeadbeef".into(),
        marker: Some(MarkerRef {
            nanos: 1_790_000_000_123_456_789,
            dir: b"/Users//x/Library/Logs/aterm".to_vec(),
        }),
    });
    round(Frame::Welcome {
        proto: PROTO,
        offers: 3,
        version: "0.98.0".into(),
        identity: Some(IdentityPolicy::Designated),
        caps: Some(CAP_JOURNAL),
    });
    round(Frame::Welcome {
        proto: PROTO,
        offers: 0,
        version: "0.98.0".into(),
        identity: Some(IdentityPolicy::SameUid),
        caps: None,
    });
    round(Frame::Welcome {
        proto: PROTO,
        offers: 0,
        version: "0.98.0".into(),
        identity: None,
        caps: Some(0),
    });
    round(Frame::Welcome {
        proto: PROTO,
        offers: 0,
        version: "0.98.0".into(),
        identity: None,
        caps: None,
    });
    round(Frame::Register {
        header: header(),
        tag: b"sid=s-1".to_vec(),
    });
    round(Frame::Meta {
        rdev: 9,
        meta: vec![1; MAX_META],
    });
    round(Frame::Release { rdev: 9 });
    round(Frame::Pending { pid: 77 });
    round(Frame::Bye { keep: Vec::new() });
    round(Frame::Bye {
        keep: vec![9, u64::MAX],
    });
    round(Frame::Offer {
        header: header(),
        tag: vec![2; MAX_TAG],
        meta: vec![3; 10],
        journal: None,
        kept: None,
    });
    round(Frame::Offer {
        header: header(),
        tag: b"sid=s-1".to_vec(),
        meta: vec![3; MAX_META],
        journal: Some(JournalOffer {
            attempts: 1,
            companions: 1,
        }),
        kept: None,
    });
    round(Frame::Offer {
        header: header(),
        tag: b"sid=s-1".to_vec(),
        meta: Vec::new(),
        journal: Some(JournalOffer {
            attempts: 0,
            companions: 1,
        }),
        kept: Some(true),
    });
    round(Frame::Orphans { n: 2 });
    round(Frame::Status {
        text: "keeper=running".into(),
    });
    round(Frame::Status {
        text: String::new(),
    });
    round(Frame::RestartWhenSafe);
    round(Frame::Adopt);
    for code in [
        RefuseCode::RdevMismatch,
        RefuseCode::NotAJournal,
        RefuseCode::NoSuchRecord,
    ] {
        round(Frame::Refused { code, rdev: 5 });
    }
    round(Frame::Companion {
        rdev: 0x0f00_000a,
        kind: COMPANION_JOURNAL,
        len: 8 << 20,
    });
    round(Frame::CompanionOffer {
        rdev: 0x0f00_000a,
        kind: COMPANION_JOURNAL,
        len: u64::MAX,
    });
    round(Frame::CompanionRelease {
        rdev: 0x0f00_000a,
        kind: COMPANION_JOURNAL,
    });
    round(Frame::Sync { seq: u64::MAX });
}

/// Decode `bytes` (a whole frame) as a reader would.
fn decode_whole(bytes: &[u8]) -> Result<Frame, WireError> {
    let mut h = [0u8; HEADER_LEN];
    h.copy_from_slice(&bytes[..HEADER_LEN]);
    Frame::decode(&decode_header(&h)?, &bytes[HEADER_LEN..])
}

/// The crash-restore fields only grow the wire. A journal OFFER and a WELCOME stating caps
/// are the older frame plus trailing bytes, so an older window reads them as before (the
/// growth rule) and its strict offer loop still meets an OFFER; and the older frames, whose
/// bodies end before those fields, decode as stating none.
#[test]
fn the_journal_fields_only_grow_the_wire() {
    let offer = |journal| Frame::Offer {
        header: header(),
        tag: b"sid=s-1".to_vec(),
        meta: vec![7; 12],
        journal,
        kept: None,
    };
    let plain = offer(None);
    let journal = offer(Some(JournalOffer {
        attempts: 2,
        companions: 0,
    }));
    let (old, new) = (plain.encode().expect("old"), journal.encode().expect("new"));
    assert_eq!(&new[HEADER_LEN..new.len() - 2], &old[HEADER_LEN..]);
    assert_eq!(&new[new.len() - 2..], [2, 0]);
    assert_eq!(decode_whole(&old), Ok(plain));

    let welcome = |caps| Frame::Welcome {
        proto: PROTO,
        offers: 1,
        version: "0.97.0".into(),
        identity: Some(IdentityPolicy::Designated),
        caps,
    };
    let older = welcome(None);
    let stated = welcome(Some(CAP_JOURNAL));
    let (old, new) = (older.encode().expect("old"), stated.encode().expect("new"));
    assert_eq!(&new[HEADER_LEN..new.len() - 1], &old[HEADER_LEN..]);
    assert_eq!(new[new.len() - 1], CAP_JOURNAL);
    assert_eq!(decode_whole(&old), Ok(older));

    let mut cut = Frame::Offer {
        header: header(),
        tag: Vec::new(),
        meta: Vec::new(),
        journal: Some(JournalOffer {
            attempts: 0,
            companions: 1,
        }),
        kept: None,
    }
    .encode()
    .expect("encode");
    cut.pop();
    let len = u16::try_from(cut.len() - HEADER_LEN).expect("short");
    cut[6..8].copy_from_slice(&len.to_be_bytes());
    assert_eq!(decode_whole(&cut), Err(WireError::Short(Kind::Offer)));
}

/// A BYE's keep set only grows the wire: one that keeps nothing is the older, empty BYE, byte
/// for byte, and one that keeps is that frame plus a count and the rdevs, so an older keeper
/// reads it as a plain BYE (and closes what it names, which is why a window keeps only to a
/// keeper stating [`CAP_KEEPS`]); past [`MAX_KEEPS`] it is refused both ways.
#[test]
fn a_byes_keep_set_only_grows_the_wire() {
    let plain = Frame::Bye { keep: Vec::new() };
    let keeping = Frame::Bye { keep: vec![7, 9] };
    let (old, new) = (plain.encode().expect("old"), keeping.encode().expect("new"));
    assert_eq!(old.len(), HEADER_LEN, "the older BYE's empty body");
    assert_eq!(&new[HEADER_LEN..HEADER_LEN + 2], [0, 2]);
    assert_eq!(new.len(), HEADER_LEN + 2 + 16);
    assert_eq!(decode_whole(&old), Ok(plain));
    assert_eq!(decode_whole(&new), Ok(keeping));
    let over = Frame::Bye {
        keep: vec![1; MAX_KEEPS + 1],
    };
    assert_eq!(over.encode(), Err(WireError::Oversize(Kind::Bye)));
    let mut forged = Frame::Bye {
        keep: vec![1; MAX_KEEPS],
    }
    .encode()
    .expect("encode");
    let n = u16::try_from(MAX_KEEPS + 1).expect("small");
    forged[HEADER_LEN..HEADER_LEN + 2].copy_from_slice(&n.to_be_bytes());
    forged.extend_from_slice(&1u64.to_be_bytes());
    let len = u16::try_from(forged.len() - HEADER_LEN).expect("short");
    forged[6..8].copy_from_slice(&len.to_be_bytes());
    assert_eq!(decode_whole(&forged), Err(WireError::Oversize(Kind::Bye)));
}

/// An OFFER's kept flag only grows the wire: it is one byte after the journal fields, which
/// it brings as zeroes to a window that does not journal, so an older window, whose decoder
/// stops after the fields it knows, reads the offer as before, and an older keeper's OFFER,
/// ending before it, decodes as saying nothing of how the record outlived its window. A flag
/// bit this build does not know says nothing.
#[test]
fn an_offers_kept_flag_only_grows_the_wire() {
    let offer = |journal, kept| Frame::Offer {
        header: header(),
        tag: b"sid=s-1".to_vec(),
        meta: vec![7; 12],
        journal,
        kept,
    };
    let journal = Some(JournalOffer {
        attempts: 2,
        companions: 0,
    });
    let older = offer(journal, None).encode().expect("older");
    let kept = offer(journal, Some(true)).encode().expect("kept");
    assert_eq!(&kept[HEADER_LEN..kept.len() - 1], &older[HEADER_LEN..]);
    assert_eq!(kept[kept.len() - 1], OFFER_KEPT);
    assert_eq!(decode_whole(&older), Ok(offer(journal, None)));
    let unjournaled = offer(None, Some(false)).encode().expect("no journal");
    assert_eq!(
        decode_whole(&unjournaled),
        Ok(offer(Some(JournalOffer::default()), Some(false))),
        "zeroed journal fields before the flag to a window that takes none"
    );
    let mut newer = offer(journal, Some(false)).encode().expect("newer");
    let last = newer.len() - 1;
    newer[last] = 0x80;
    assert_eq!(
        decode_whole(&newer),
        Ok(offer(journal, Some(false))),
        "a newer keeper's flag bit is not this one"
    );
}

/// The companion kinds are numbered 14 to 16 and SYNC 17, never renumbered, and carry a
/// descriptor exactly where the plan says (COMPANION and COMPANION_OFFER, not
/// COMPANION_RELEASE or SYNC); a decoder from before them refuses them as unknown kinds, which
/// is why the keeper sends a companion only to a window that stated CAP_JOURNAL, and a window
/// sends SYNC only to a keeper that stated CAP_SYNC.
#[test]
fn the_companion_kinds_are_numbered_and_carry_their_descriptors() {
    for (byte, kind, fd) in [
        (14, Kind::Companion, true),
        (15, Kind::CompanionOffer, true),
        (16, Kind::CompanionRelease, false),
        (17, Kind::Sync, false),
    ] {
        assert_eq!(Kind::from_byte(byte), Some(kind));
        assert_eq!(kind as u8, byte);
        assert_eq!(kind.carries_fd(), fd, "{kind:?}");
    }
    assert_eq!(Kind::from_byte(18), None, "the next number is unassigned");
    for (cap, others) in [
        (CAP_JOURNAL, CAP_SENDS_BYE | CAP_LINK),
        (CAP_SYNC, CAP_SENDS_BYE | CAP_LINK | CAP_JOURNAL),
        (CAP_KEEPS, CAP_SENDS_BYE | CAP_LINK | CAP_JOURNAL | CAP_SYNC),
    ] {
        assert_eq!(cap & others, 0, "a bit of its own");
    }
    assert_eq!(
        Frame::CompanionRelease {
            rdev: 1,
            kind: COMPANION_JOURNAL
        }
        .encode()
        .expect("encode")
        .len(),
        HEADER_LEN + 9
    );
}

#[test]
fn headers_are_refused_before_the_body() {
    let mut h = encode_header(Kind::Bye, 0).expect("header");
    h[0] = b'X';
    assert_eq!(decode_header(&h), Err(WireError::BadMagic));
    let mut h = encode_header(Kind::Bye, 0).expect("header");
    h[4] = 200;
    assert_eq!(decode_header(&h), Err(WireError::UnknownKind(200)));
    let mut h = encode_header(Kind::Bye, 0).expect("header");
    h[6..8].copy_from_slice(&u16::try_from(MAX_BODY + 1).unwrap().to_be_bytes());
    assert_eq!(decode_header(&h), Err(WireError::TooLong(MAX_BODY + 1)));
    let mut h = encode_header(Kind::Bye, 0).expect("header");
    h[5] = FLAG_FD;
    assert_eq!(decode_header(&h), Err(WireError::FdFlag(Kind::Bye)));
    let mut h = encode_header(Kind::Register, 0).expect("header");
    h[5] = 0;
    assert_eq!(decode_header(&h), Err(WireError::FdFlag(Kind::Register)));
}

#[test]
fn oversize_parts_are_refused_both_ways() {
    assert_eq!(
        Frame::Register {
            header: header(),
            tag: vec![0; MAX_TAG + 1],
        }
        .encode(),
        Err(WireError::Oversize(Kind::Register))
    );
    let mut body = 9u64.to_be_bytes().to_vec();
    body.extend_from_slice(&u16::try_from(MAX_META + 1).unwrap().to_be_bytes());
    body.resize(body.len() + MAX_META + 1, 0);
    let h = Header {
        kind: Kind::Meta,
        flags: 0,
        len: body.len(),
    };
    assert_eq!(
        Frame::decode(&h, &body),
        Err(WireError::Oversize(Kind::Meta))
    );
}

/// The growth rule: a longer body than this build knows is read by its
/// known prefix; a shorter one is refused.
#[test]
fn frames_only_grow() {
    let mut bytes = Frame::Release { rdev: 11 }.encode().expect("encode");
    bytes.extend_from_slice(b"future");
    let body = &bytes[HEADER_LEN..];
    let h = Header {
        kind: Kind::Release,
        flags: 0,
        len: body.len(),
    };
    assert_eq!(Frame::decode(&h, body), Ok(Frame::Release { rdev: 11 }));
    let h = Header {
        kind: Kind::Release,
        flags: 0,
        len: 3,
    };
    assert_eq!(
        Frame::decode(&h, &[0, 0, 0]),
        Err(WireError::Short(Kind::Release))
    );
}

/// A HELLO from a window before the marker grew is read with none, and a
/// marker directory over its bound is refused.
#[test]
fn a_hello_without_a_marker_is_an_older_window_s() {
    let old = Frame::Hello {
        proto: PROTO,
        class: PeerClass::App,
        caps: CAP_SENDS_BYE,
        build: "0.97.0".into(),
        marker: None,
    }
    .encode()
    .expect("encode");
    let body = &old[HEADER_LEN..];
    assert_eq!(body.len(), 2 + 1 + 1 + 1 + "0.97.0".len());
    let mut h = [0u8; HEADER_LEN];
    h.copy_from_slice(&old[..HEADER_LEN]);
    let header = decode_header(&h).expect("header");
    assert!(matches!(
        Frame::decode(&header, body),
        Ok(Frame::Hello { marker: None, .. })
    ));
    assert_eq!(
        Frame::Hello {
            proto: PROTO,
            class: PeerClass::App,
            caps: 0,
            build: String::new(),
            marker: Some(MarkerRef {
                nanos: 1,
                dir: vec![b'/'; MAX_MARKER_DIR + 1],
            }),
        }
        .encode(),
        Err(WireError::Oversize(Kind::Hello))
    );
}

#[test]
fn an_unknown_peer_class_is_refused() {
    let mut body = PROTO.to_be_bytes().to_vec();
    body.extend_from_slice(&[9, 0, 0]);
    let h = Header {
        kind: Kind::Hello,
        flags: 0,
        len: body.len(),
    };
    assert_eq!(Frame::decode(&h, &body), Err(WireError::UnknownClass(9)));
}
