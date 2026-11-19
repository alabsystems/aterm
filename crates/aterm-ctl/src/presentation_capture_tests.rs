// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn words(text: &[&str]) -> Vec<String> {
    text.iter().map(|word| (*word).to_string()).collect()
}

#[test]
fn a_presentation_command_keeps_literal_photo_arguments_out_of_the_control_request() {
    let mut request = words(&[
        "image",
        "--presentation-lease",
        "held.png",
        "--producer-pid",
        "42",
        "--",
        "/usr/bin/env",
        "DISPLAY=:1",
        "a photo; no shell",
    ]);
    let plan = Plan::take(&mut request).unwrap().unwrap();
    assert_eq!(
        request,
        words(&["image", "--presentation-lease", "held.png"])
    );
    assert_eq!(
        plan.argv,
        words(&["/usr/bin/env", "DISPLAY=:1", "a photo; no shell"])
    );
    assert_eq!(plan.producers, [42]);
    let mut ordinary = words(&["image", "plain", "shot.png"]);
    assert_eq!(Plan::take(&mut ordinary).unwrap(), None);
    assert_eq!(ordinary, words(&["image", "plain", "shot.png"]));
    let mut literal = words(&["sudo", "echo", "--presentation-lease"]);
    assert_eq!(Plan::take(&mut literal).unwrap(), None);
    for bad in [
        words(&["image", "--presentation-lease", "held.png"]),
        words(&["image", "--presentation-lease", "held.png", "--"]),
        words(&[
            "image",
            "--presentation-lease",
            "held.png",
            "--producer-pid",
            "0",
            "--",
            "photo",
        ]),
        words(&[
            "image",
            "--presentation-lease",
            "../held.png",
            "--",
            "photo",
        ]),
        words(&[
            "image",
            "--presentation-lease",
            "held.png",
            "--presentation-lease",
            "--",
            "photo",
        ]),
        words(&[
            "@t0000000001",
            "image",
            "--presentation-lease",
            "held.png",
            "--",
            "photo",
        ]),
    ] {
        assert!(Plan::take(&mut bad.clone()).is_err(), "{bad:?}");
    }
}

#[test]
fn a_capture_receipt_names_one_exact_material_presentation_and_canonical_private_path() {
    let frame = "window=0 serial=2 surface=3 width=744 height=528";
    let image = Receipt::parse(
        &format!("OK presentation-image {frame} path=/private/a%20b.png"),
        "OK presentation-image ",
        true,
    )
    .unwrap();
    let end = Receipt::parse(
        &format!("OK presentation-released {frame}"),
        "OK presentation-released ",
        false,
    )
    .unwrap();
    assert_eq!(image.identity, end.identity);
    assert_eq!(image.path, Some(PathBuf::from("/private/a b.png")));
    assert_eq!(image.fields(), frame);
    for bad in [
        format!("OK presentation-image {frame} path=relative.png"),
        format!("OK presentation-image {frame} path=/a%GG.png"),
        format!("OK presentation-image {frame} path=/a.png width=744"),
        format!(
            "OK presentation-image {} path=/a.png",
            frame.replace("surface=3", "surface=0")
        ),
        format!(
            "OK presentation-image {} path=/a.png",
            frame.replace("serial=2 ", "")
        ),
        format!("OK presentation-released {frame} path=/a.png"),
    ] {
        assert!(
            Receipt::parse(&bad, "OK presentation-image ", true).is_err(),
            "{bad}"
        );
    }
}

#[test]
fn a_native_photo_finishes_before_ack_and_errors_close_its_capture_connection() {
    for end in ["ok", "photo-error", "surface-changed"] {
        let scratch = crate::test_scratch_dir("presentation-photo");
        aterm_uds::private_file::write_private(scratch.path(), "client.png", b"owned PNG bytes")
            .unwrap();
        let image = scratch.path().join("client.png");
        let photo = scratch.path().join("photo-finished");
        let plan = if end == "photo-error" {
            Plan {
                argv: words(&["/usr/bin/false"]),
                producers: Vec::new(),
            }
        } else {
            Plan {
                argv: vec![
                    "/usr/bin/touch".into(),
                    photo.to_string_lossy().into_owned(),
                ],
                producers: Vec::new(),
            }
        };
        let (client, mut server) = CtlStream::pair().unwrap();
        let photo_at_ack = photo.clone();
        let serving = std::thread::spawn(move || {
            let fields = "window=0 serial=2 surface=3 width=744 height=528";
            writeln!(
                server,
                "OK presentation-image {fields} path={}",
                aterm_codec::pct::encode(&image.to_string_lossy())
            )
            .unwrap();
            let nonce = "0123456789abcdef0123456789abcdef";
            writeln!(
                server,
                "{}{nonce}",
                aterm_types::control_verbs::ARTIFACT_REPLY_CHALLENGE_PREFIX
            )
            .unwrap();
            server.flush().unwrap();
            let mut ack = String::new();
            let count = std::io::BufReader::new(&server)
                .read_line(&mut ack)
                .unwrap();
            if end == "photo-error" {
                assert_eq!(count, 0, "an unsuccessful photo must close without ACK");
            } else {
                assert!(
                    photo_at_ack.exists(),
                    "the actual photograph process must finish first"
                );
                assert_eq!(
                    ack,
                    format!(
                        "{}{nonce}\n",
                        aterm_types::control_verbs::ARTIFACT_REPLY_ACK_PREFIX
                    )
                );
                let fields = if end == "surface-changed" {
                    fields.replace("surface=3", "surface=4")
                } else {
                    fields.to_string()
                };
                writeln!(server, "OK presentation-released {fields}").unwrap();
                server.flush().unwrap();
            }
        });
        let result = capture_on(client, &plan, None);
        serving.join().unwrap();
        if end == "ok" {
            let (held, released, hash) = result.unwrap();
            assert_eq!(held.identity, released.identity);
            assert_eq!(
                hash,
                aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(b"owned PNG bytes"))
            );
        } else {
            assert!(result.is_err(), "{end}");
        }
    }
}

#[test]
fn an_expired_capture_budget_starts_no_photo_and_native_progress_cannot_extend_it() {
    let scratch = crate::test_scratch_dir("presentation-expired");
    let target = scratch.path().join("must-not-be-created");
    let plan = Plan {
        argv: vec![
            "/usr/bin/touch".into(),
            target.to_string_lossy().into_owned(),
        ],
        producers: vec![std::process::id()],
    };
    let expired = Some(Instant::now());
    assert_eq!(
        photograph(&plan, expired).unwrap_err().kind(),
        io::ErrorKind::TimedOut
    );
    assert!(
        !target.exists(),
        "an expired operation cannot dispatch native work"
    );

    let (client, _server) = CtlStream::pair().unwrap();
    assert_eq!(
        capture_on(client, &plan, expired).unwrap_err().kind(),
        io::ErrorKind::TimedOut
    );
    assert!(!target.exists());

    let mut ready = std::io::Cursor::new(b"OK ready\n");
    let mut progress = Progress::new(&[std::process::id()]);
    progress.check().unwrap();
    assert_eq!(
        line(&mut ready, &mut progress, expired).unwrap_err().kind(),
        io::ErrorKind::TimedOut,
        "even a buffered answer and a freshly observed producer cannot replenish the deadline"
    );
    assert_eq!(
        ready.position(),
        0,
        "the expired operation reads no more payload"
    );
    assert_eq!(
        line(&mut ready, &mut progress, None).unwrap(),
        "OK ready",
        "--timeout 0 preserves the native-progress-only wait"
    );
}

struct PartialWriter {
    clock: std::rc::Rc<std::cell::Cell<Instant>>,
    step: Duration,
    written: Vec<u8>,
    flushed: usize,
    flush_at: Option<Instant>,
    error: Option<io::ErrorKind>,
}

impl Write for PartialWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.clock.set(self.clock.get() + self.step);
        if let Some(error) = self.error {
            return Err(error.into());
        }
        let count = bytes.len().min(1);
        self.written.extend_from_slice(&bytes[..count]);
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flushed += 1;
        if let Some(at) = self.flush_at {
            self.clock.set(at);
        }
        Ok(())
    }
}

#[test]
fn partial_request_and_ack_writes_share_one_absolute_budget_through_flush() {
    for bytes in [b"image held.png\n".as_slice(), b"ACK nonce\n".as_slice()] {
        let start = Instant::now();
        let until = start + Duration::from_secs(10);
        let clock = std::rc::Rc::new(std::cell::Cell::new(start));
        let mut writer = PartialWriter {
            clock: clock.clone(),
            step: Duration::from_secs(4),
            written: Vec::new(),
            flushed: 0,
            flush_at: None,
            error: None,
        };
        let mut armed = Vec::new();
        let error = write_unbuffered_until(
            &mut writer,
            bytes,
            Some(until),
            &mut || clock.get(),
            &mut |left| {
                armed.push(left);
                Ok(())
            },
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(
            writer.written,
            bytes[..3],
            "no write after the third crosses the deadline"
        );
        assert_eq!(writer.flushed, 0);
        assert_eq!(
            armed,
            [
                Some(Duration::from_secs(10)),
                Some(Duration::from_secs(6)),
                Some(Duration::from_secs(2))
            ]
        );
    }

    let start = Instant::now();
    let until = start + Duration::from_secs(10);
    let clock = std::rc::Rc::new(std::cell::Cell::new(start));
    let mut writer = PartialWriter {
        clock: clock.clone(),
        step: Duration::from_secs(1),
        written: Vec::new(),
        flushed: 0,
        flush_at: Some(until),
        error: None,
    };
    let mut armed = Vec::new();
    let error = write_unbuffered_until(
        &mut writer,
        b"ACK",
        Some(until),
        &mut || clock.get(),
        &mut |left| {
            armed.push(left);
            Ok(())
        },
    )
    .unwrap_err();
    assert_eq!(writer.written, b"ACK");
    assert_eq!(writer.flushed, 1);
    assert_eq!(armed.last(), Some(&Some(Duration::from_secs(8))));
    assert_eq!(
        error.kind(),
        io::ErrorKind::TimedOut,
        "a late successful flush is no completion receipt"
    );
}

#[test]
fn capture_writes_refuse_expired_admission_and_preserve_native_errors_or_no_deadline() {
    let start = Instant::now();
    let mut expired = Vec::new();
    let error = write_unbuffered_until(
        &mut expired,
        b"request",
        Some(start),
        &mut || start,
        &mut |_| panic!("an expired write must not arm native I/O"),
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(expired.is_empty());

    let clock = std::rc::Rc::new(std::cell::Cell::new(start));
    let mut writer = PartialWriter {
        clock: clock.clone(),
        step: Duration::from_secs(20),
        written: Vec::new(),
        flushed: 0,
        flush_at: None,
        error: Some(io::ErrorKind::BrokenPipe),
    };
    let error = write_unbuffered_until(
        &mut writer,
        b"request",
        Some(start + Duration::from_secs(10)),
        &mut || clock.get(),
        &mut |_| Ok(()),
    )
    .unwrap_err();
    assert_eq!(
        error.kind(),
        io::ErrorKind::BrokenPipe,
        "native delivery classification is retained"
    );
    writer.error = None;
    write_unbuffered_until(
        &mut writer,
        b"ACK",
        None,
        &mut || clock.get(),
        &mut |left| {
            assert_eq!(left, None, "--timeout 0 arms no new wall budget");
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(writer.written, b"ACK");
    assert_eq!(writer.flushed, 1);
}

#[test]
fn a_delivered_ack_does_not_rearm_its_unbuffered_flush_after_peer_closure() {
    struct Answered {
        detached: std::rc::Rc<std::cell::Cell<bool>>,
        written: Vec<u8>,
        flushed: usize,
    }
    impl Write for Answered {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.written.extend_from_slice(bytes);
            // The peer can consume the final ACK and close before write returns.
            self.detached.set(true);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushed += 1;
            Ok(())
        }
    }

    let now = Instant::now();
    let detached = std::rc::Rc::new(std::cell::Cell::new(false));
    let mut writer = Answered {
        detached: detached.clone(),
        written: Vec::new(),
        flushed: 0,
    };
    let mut armed = Vec::new();
    write_unbuffered_until(
        &mut writer,
        b"ACK nonce\n",
        Some(now + Duration::from_secs(10)),
        &mut || now,
        &mut |left| {
            if detached.get() {
                return Err(io::Error::from_raw_os_error(22));
            }
            armed.push(left);
            Ok(())
        },
    )
    .unwrap();
    assert!(detached.get());
    assert_eq!(writer.written, b"ACK nonce\n");
    assert_eq!(writer.flushed, 1);
    assert_eq!(armed, [Some(Duration::from_secs(10))]);
}

#[test]
fn a_closed_presentation_peer_keeps_queued_release_bytes_and_refuses_a_cut_frame() {
    for buffered in [false, true] {
        for complete in [false, true] {
            let (client, mut server) = CtlStream::pair().unwrap();
            let frame = "OK presentation-released window=0 serial=2 surface=3 width=744 height=528";
            write!(server, "{frame}{}", if complete { "\n" } else { "" }).unwrap();
            server.flush().unwrap();
            let capacity = if buffered { 8192 } else { 4 };
            let mut reader = std::io::BufReader::with_capacity(
                capacity,
                CaptureRead {
                    stream: &client,
                    deadline: None,
                },
            );
            if buffered {
                assert!(!reader.fill_buf().unwrap().is_empty());
            }
            server.shutdown(std::net::Shutdown::Both).unwrap();
            drop(server);
            let last = Instant::now().checked_sub(HANG + TICK).unwrap();
            let expired_progress = || Progress {
                roots: Vec::new(),
                last,
                read: None,
            };
            assert_eq!(
                expired_progress().check().unwrap_err().kind(),
                io::ErrorKind::TimedOut
            );
            let mut stopped_progress = expired_progress();
            let result = line(&mut reader, &mut stopped_progress, None);
            if complete {
                assert_eq!(
                    result.unwrap(),
                    frame,
                    "buffered={buffered}: the closed peer's frame is whole"
                );
            } else {
                assert_eq!(
                    result.unwrap_err().kind(),
                    io::ErrorKind::UnexpectedEof,
                    "buffered={buffered}: queued bytes do not excuse missing framing"
                );
            }
        }
    }
}

#[test]
fn an_unrelated_native_read_error_is_not_accepted_as_peer_closure() {
    struct Refused;
    impl Read for Refused {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unrelated native refusal",
            ))
        }
    }
    let mut reader = std::io::BufReader::new(Refused);
    let error = line(&mut reader, &mut Progress::new(&[]), None).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(error.to_string(), "unrelated native refusal");
}
