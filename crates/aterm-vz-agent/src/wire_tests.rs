// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn every_kind() -> Vec<Frame> {
    vec![
        Frame::Exec(Exec {
            argv: vec![b"cargo".to_vec(), b"test".to_vec(), Vec::new()],
            cwd: b"/Users//me/aterm".to_vec(),
            env: vec![
                (b"PATH".to_vec(), b"/usr/bin:/bin".to_vec()),
                (b"EMPTY".to_vec(), Vec::new()),
            ],
            stdin: Stdin::Pipe,
            merge: true,
            clock_unix_ns: 1_792_000_000_123_456_789,
        }),
        Frame::Exec(Exec {
            argv: vec![b"true".to_vec()],
            cwd: b"/".to_vec(),
            env: Vec::new(),
            stdin: Stdin::Null,
            merge: false,
            clock_unix_ns: 0,
        }),
        Frame::Mount(vec![
            Share {
                name: "repo".into(),
                path: b"/Users//me/aterm".to_vec(),
            },
            Share {
                name: "git".into(),
                path: b"/Users//me/aterm/.git".to_vec(),
            },
        ]),
        Frame::Mount(Vec::new()),
        Frame::Poweroff,
        Frame::Stdin(b"line\n".to_vec()),
        Frame::Stdin(Vec::new()),
        Frame::StdinEof,
        Frame::Kill(9),
        Frame::Out(vec![0, 255, 10]),
        Frame::Err(b"warning\n".to_vec()),
        Frame::Exit(Exit::Exited(0)),
        Frame::Exit(Exit::Exited(-1)),
        Frame::Exit(Exit::Signaled(15)),
        Frame::Exit(Exit::NotStarted("cargo: No such file or directory".into())),
        Frame::Hello(Hello {
            proto: PROTO,
            key: [0x5a; 32],
            agent_sha256: [0xab; 32],
            boot_id: "1b4c…".into(),
            kernel: "6.8.0-142-generic".into(),
            uptime_ms: 1830,
        }),
        Frame::Booting(vec![Job {
            id: 117,
            unit: "systemd-networkd-wait-online.service".into(),
            kind: "start".into(),
            state: "running".into(),
            elapsed_ms: 5000,
        }]),
        Frame::Booting(Vec::new()),
        Frame::Ready(Ready {
            state: "degraded".into(),
            failed_units: vec!["fwupd.service".into()],
        }),
        Frame::Beat { uptime_ms: 99 },
        Frame::Clock { unix_ns: u64::MAX },
    ]
}

#[test]
fn every_kind_round_trips_through_one_stream() {
    let frames = every_kind();
    let mut stream = Vec::new();
    for f in &frames {
        write_frame(&mut stream, f).unwrap();
    }
    let mut r = stream.as_slice();
    for f in &frames {
        assert_eq!(read_frame(&mut r).unwrap().as_ref(), Some(f));
    }
    assert!(
        read_frame(&mut r).unwrap().is_none(),
        "the end between frames is no frame"
    );
}

#[test]
fn a_hellos_debug_withholds_its_key() {
    let hello = Hello {
        proto: PROTO,
        key: [0x5a; 32],
        agent_sha256: [0xab; 32],
        boot_id: "b".into(),
        kernel: "k".into(),
        uptime_ms: 1,
    };
    let text = format!("{:?}", Frame::Hello(hello));
    assert!(text.contains("key: \"<withheld>\""), "{text}");
    assert!(
        !text.contains("90, 90"),
        "the key's bytes (0x5a) are not printed: {text}"
    );
    assert!(text.contains("171, 171"), "the agent's hash is: {text}");
}

#[test]
fn a_frame_over_the_limit_is_refused_both_ways() {
    assert!(Frame::Out(vec![0; MAX_FRAME]).encode().is_ok());
    assert!(matches!(
        Frame::Out(vec![0; MAX_FRAME + 1]).encode(),
        Err(WireError::TooLarge(n)) if n == (MAX_FRAME + 1) as u64
    ));
    // A header that claims more than the limit is refused before any payload is
    // read or allocated: none follows it here.
    let mut header = vec![OUT];
    header.extend_from_slice(&((MAX_FRAME as u32) + 1).to_le_bytes());
    assert!(matches!(
        read_frame(&mut header.as_slice()),
        Err(WireError::TooLarge(_))
    ));
    let mut huge = vec![OUT];
    huge.extend_from_slice(&u32::MAX.to_le_bytes());
    assert!(matches!(
        read_frame(&mut huge.as_slice()),
        Err(WireError::TooLarge(_))
    ));
}

#[test]
fn a_stream_cut_inside_a_frame_is_truncated_at_every_cut() {
    let whole = Frame::Exit(Exit::NotStarted("why".into()))
        .encode()
        .unwrap();
    for cut in 1..whole.len() {
        assert!(
            matches!(read_frame(&mut &whole[..cut]), Err(WireError::Truncated)),
            "cut at {cut}"
        );
    }
    assert!(read_frame(&mut &whole[..0]).unwrap().is_none());
}

#[test]
fn an_unknown_kind_is_named() {
    for kind in [0x00, 0x07, 0x13, 0x25, 0x7f, 0xff] {
        let e = read_frame(&mut [kind, 0, 0, 0, 0].as_slice()).unwrap_err();
        assert!(matches!(e, WireError::UnknownKind(k) if k == kind), "{e}");
        assert!(e.to_string().contains(&format!("0x{kind:02x}")), "{e}");
    }
}

#[test]
fn a_payload_that_is_not_its_kinds_is_malformed() {
    let malformed = |kind: u8, payload: &[u8]| {
        matches!(Frame::decode(kind, payload), Err(WireError::Malformed(_)))
    };
    // Short of a fixed field.
    assert!(malformed(KILL, &[1, 0]));
    assert!(malformed(BEAT, &[0; 7]));
    // Bytes after the last field.
    assert!(malformed(KILL, &[9, 0, 0, 0, 0]));
    assert!(malformed(POWEROFF, &[0]));
    // A string or a list longer than the payload, refused before it is allocated.
    assert!(malformed(EXIT, &[2, 0xff, 0xff, 0xff, 0x7f]));
    assert!(malformed(MOUNT, &[0xff, 0xff, 0xff, 0xff]));
    // A tag none of the variants has.
    assert!(malformed(EXIT, &[3, 0, 0, 0, 0]));
    let mut exec = Frame::Exec(Exec {
        argv: vec![b"x".to_vec()],
        cwd: b"/".to_vec(),
        env: Vec::new(),
        stdin: Stdin::Null,
        merge: false,
        clock_unix_ns: 0,
    })
    .encode()
    .unwrap();
    let stdin_at = exec.len() - 10;
    exec[stdin_at] = 2;
    assert!(malformed(EXEC, &exec[HEADER..]));
    exec[stdin_at] = 0;
    exec[stdin_at + 1] = 2;
    assert!(malformed(EXEC, &exec[HEADER..]));
    // Text that is not UTF-8.
    assert!(malformed(EXIT, &[2, 1, 0, 0, 0, 0xff]));
}
