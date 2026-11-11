// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn shares() -> Vec<ShareEntry> {
    vec![
        ShareEntry {
            name: "repo".into(),
            path: "/src/aterm".into(),
        },
        ShareEntry {
            name: "git".into(),
            path: "/src/aterm/.git".into(),
        },
    ]
}

fn plan() -> Plan {
    Plan {
        name: "aterm-linux".into(),
        cpus: 8,
        memory_bytes: 16 << 30,
        kernel: "/k/Image".into(),
        initrd: "/k/initrd".into(),
        command_line: crate::plan::COMMAND_LINE.into(),
        disks: vec![
            Disk {
                path: "/live.img".into(),
                read_only: false,
            },
            Disk {
                path: "/seed.iso".into(),
                read_only: true,
            },
        ],
        mac: crate::plan::mac_for("aterm-linux"),
        boot_share: "/agent/x".into(),
        shares: shares(),
        rosetta: true,
        console_log: "/run/console.log".into(),
        agent_sha256: "cd".repeat(32),
    }
}

fn hello() -> Hello {
    Hello {
        proto: aterm_vz_agent::wire::PROTO,
        key: [0; aterm_vz_agent::key::LEN],
        agent_sha256: [7; 32],
        boot_id: "b00t".into(),
        kernel: "6.8.0-142-generic".into(),
        uptime_ms: 1830,
    }
}

fn requests() -> Vec<Request> {
    vec![
        Request::Attach(Attach {
            proto: PROTO,
            helper_sha256: "ab".repeat(32),
            agent_sha256: "cd".repeat(32),
            shares: shares(),
            keep: true,
        }),
        Request::Boot(Box::new(plan())),
        Request::Connect { port: 1024 },
        Request::Release { id: u64::MAX },
        Request::Share(Vec::new()),
        Request::Keep(false),
        Request::State,
        Request::Stop { grace_ms: 60_000 },
        Request::Detach,
    ]
}

fn replies() -> Vec<Reply> {
    vec![
        Reply::Attached {
            phase: Phase::Ready,
            keep: true,
            swapped: false,
        },
        Reply::Refused("another helper".into()),
        Reply::Started,
        Reply::Booting("systemd-networkd-wait-online.service (12 s)".into()),
        Reply::Ready(hello()),
        Reply::Connected { id: 3, port: 1024 },
        Reply::Done,
        Reply::State(Box::new(StateReport {
            pid: 4242,
            uniq: 99,
            helper_sha256: "ab".repeat(32),
            proto: PROTO,
            live_disk: "/vz/live/aterm-linux.img".into(),
            phase: Phase::Booting,
            keep: false,
            sessions: 2,
            boot_id: String::new(),
            agent_sha256: String::new(),
            failed_units: vec!["fwupd.service".into()],
            beat_ms_ago: Some(4_000),
            free_bytes: None,
            console_tail: "[  OK  ] Reached target local-fs.target.\n".into(),
        })),
        Reply::Stopping,
        Reply::Lost {
            why: "the guest stopped answering (no heartbeat for 60 s); VM stopped".into(),
            tail: "…".into(),
        },
        Reply::Failed {
            error: "VZErrorDomain 2: The storage device attachment is invalid.".into(),
            tail: String::new(),
        },
        Reply::Error("no session".into()),
    ]
}

fn one(bytes: &[u8]) -> (u8, &[u8]) {
    let (kind, payload, used) = split(bytes).unwrap().expect("whole");
    assert_eq!(used, bytes.len());
    (kind, payload)
}

#[test]
fn every_request_and_reply_round_trips() {
    for r in requests() {
        let bytes = r.encode().unwrap();
        let (kind, payload) = one(&bytes);
        assert_eq!(Request::decode(kind, payload).unwrap(), r);
    }
    for r in replies() {
        let bytes = r.encode().unwrap();
        let (kind, payload) = one(&bytes);
        assert_eq!(Reply::decode(kind, payload).unwrap(), r);
        assert_eq!(
            Reply::carries_descriptor(kind),
            matches!(r, Reply::Connected { .. })
        );
    }
}

#[test]
fn the_frozen_kinds_never_move() {
    let attach = requests()[0].encode().unwrap();
    assert_eq!(attach[0], 0x01);
    assert_eq!(
        &attach[HEADER..HEADER + 4],
        &PROTO.to_le_bytes(),
        "proto leads"
    );
    assert_eq!(Request::Stop { grace_ms: 1 }.encode().unwrap()[0], 0x02);
    assert_eq!(Request::State.encode().unwrap()[0], 0x03);
    assert_eq!(Reply::Refused(String::new()).encode().unwrap()[0], 0x81);
    assert_eq!(Reply::Stopping.encode().unwrap()[0], 0x82);
}

#[test]
fn another_versions_attach_and_stop_still_read() {
    // A later version's Attach: its proto, then fields this version does not know.
    let mut payload = (PROTO + 1).to_le_bytes().to_vec();
    payload.extend_from_slice(&[0xff; 9]);
    assert!(matches!(
        Request::decode(0x01, &payload),
        Ok(Request::Attach(Attach { proto, .. })) if proto == PROTO + 1
    ));
    // Its Stop, with a field after the grace.
    let mut payload = 500u64.to_le_bytes().to_vec();
    payload.push(1);
    assert_eq!(
        Request::decode(0x02, &payload).unwrap(),
        Request::Stop { grace_ms: 500 }
    );
    assert_eq!(Request::decode(0x03, &[1, 2, 3]).unwrap(), Request::State);
}

#[test]
fn a_split_waits_for_the_whole_message_and_refuses_an_oversized_one() {
    let bytes = Request::Connect { port: 1024 }.encode().unwrap();
    for cut in 0..bytes.len() {
        assert!(split(&bytes[..cut]).unwrap().is_none(), "cut at {cut}");
    }
    let mut two = bytes.clone();
    two.extend_from_slice(&Request::Detach.encode().unwrap());
    let (_, _, used) = split(&two).unwrap().unwrap();
    assert_eq!(used, bytes.len());
    let mut huge = vec![0x11];
    huge.extend_from_slice(&((MAX_MESSAGE as u32) + 1).to_le_bytes());
    assert!(matches!(split(&huge), Err(ProtoError::TooLarge(_))));
    assert!(matches!(
        Reply::Booting("x".repeat(MAX_MESSAGE)).encode(),
        Err(ProtoError::TooLarge(_))
    ));
}

#[test]
fn unknown_kinds_and_malformed_payloads_are_named() {
    assert!(matches!(
        Request::decode(0x7f, &[]),
        Err(ProtoError::UnknownKind(0x7f))
    ));
    assert!(matches!(
        Reply::decode(0x00, &[]),
        Err(ProtoError::UnknownKind(0x00))
    ));
    assert!(matches!(
        Request::decode(0x14, &[2]),
        Err(ProtoError::Malformed(_))
    ));
    assert!(matches!(
        Request::decode(0x11, &[0, 4, 0, 0, 9]),
        Err(ProtoError::Malformed(_))
    ));
    assert!(matches!(
        Reply::decode(0x90, &[9, 0, 0]),
        Err(ProtoError::Malformed(_))
    ));
    assert!(matches!(
        Request::decode(0x13, &[0xff, 0xff, 0xff, 0x7f]),
        Err(ProtoError::Malformed(_))
    ));
}
