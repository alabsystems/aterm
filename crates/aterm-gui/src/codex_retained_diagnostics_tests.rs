// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn report(bytes: &[u8]) -> Report {
    classify(bytes, false, Completion::Eof)
}

#[test]
fn fixed_runtime_forms_and_specific_precedence_are_closed() {
    for (bytes, expected) in [
        (&b"thread 'worker' panicked at library/core/src/panicking.rs:3:4:\nPRIVATE_PAYLOAD\n"[..], Kind::RustPanic),
        (&b"panic in a function that cannot unwind\nthread caused non-unwinding panic. aborting.\n"[..], Kind::NonunwindingPanic),
        (&b"panic in a destructor during cleanup\nthread caused non-unwinding panic. aborting.\n"[..], Kind::DoublePanic),
        (&b"thread panicked while processing panic. aborting.\n"[..], Kind::DoublePanic),
        (&b"fatal runtime error: stack overflow, aborting\n"[..], Kind::StackOverflow),
        (&b"memory allocation of 1234 bytes failed\n"[..], Kind::AllocationFailure),
        (&b"process(12) malloc: *** error for object PRIVATE_OBJECT: pointer being freed was not allocated\nprocess(12) malloc: *** set a breakpoint in malloc_error_break to debug\n"[..], Kind::MallocAbort),
        (&b"malloc: nano zone abandoned\n"[..], Kind::Unknown),
    ] {
        assert_eq!(report(bytes).kind, expected);
    }
}

#[test]
fn output_never_contains_payload_thread_path_or_backtrace() {
    let input = b"thread 'PRIVATE_THREAD' panicked at /PRIVATE_HOME/private.rs:53:7:\nPRIVATE_PAYLOAD\nstack backtrace:\nPRIVATE_BACKTRACE\n";
    let rendered = report(input).to_string();
    assert_eq!(report(input).kind, Kind::RustPanic);
    assert_eq!(report(input).location, None);
    assert_eq!(
        rendered,
        "stderr-kind=rust-panic capture=eof truncated=false site=unknown line=none column=none"
    );
    assert!(!rendered.contains("PRIVATE"));
    assert!(!format!("{:?}", report(input)).contains("PRIVATE"));
    assert_eq!(
        report(b"{\"message\":\"thread caused non-unwinding panic. aborting.\"}\n").kind,
        Kind::Unknown
    );
    assert_eq!(
        report(b"\"fatal runtime error: stack overflow, aborting\"\n").kind,
        Kind::Unknown
    );
}

#[test]
fn actual_std_thread_id_header_is_bounded_and_never_exported() {
    let input = b"thread 'PRIVATE_THREAD' (123456789) panicked at library/core/src/panicking.rs:3:4:\nPRIVATE_PAYLOAD\n";
    let result = report(input);
    assert_eq!(result.kind, Kind::RustPanic);
    assert_eq!(
        result.location,
        Some(Location {
            site: Site::CorePanicking,
            line: 3,
            column: 4
        })
    );
    assert!(!result.to_string().contains("123456789"));
    for identity in ["", "0", "-1", "+1", "0x12", "123456789012345678901"] {
        assert_eq!(
            report(
                format!("thread 'x' ({identity}) panicked at library/core/src/panicking.rs:3:4:\n")
                    .as_bytes()
            )
            .kind,
            Kind::Unknown
        );
    }
}

#[test]
fn allocator_requires_runtime_prefix_and_partial_lines_remain_unknown() {
    let error = "process(12,0x1234) malloc: *** error for object PRIVATE: invalid object\n";
    let breakpoint =
        "process(12,0x1234) malloc: *** set a breakpoint in malloc_error_break to debug\n";
    assert_eq!(
        report(format!("{error}{breakpoint}").as_bytes()).kind,
        Kind::MallocAbort
    );
    for prefix in ["INFO ", "log: ", "\"", "{", "/PRIVATE/"] {
        assert_eq!(
            report(format!("{prefix}{error}{prefix}{breakpoint}").as_bytes()).kind,
            Kind::Unknown
        );
    }
    let marker = b"fatal runtime error: stack overflow, aborting";
    for completion in [
        Completion::Open,
        Completion::ReadError,
        Completion::Unavailable,
    ] {
        assert_eq!(classify(marker, false, completion).kind, Kind::Unknown);
    }
    assert_eq!(
        classify(marker, false, Completion::Eof).kind,
        Kind::StackOverflow
    );
    assert_eq!(
        report(b"fatal runtime error: stack overflow, aborting PRIVATE_SUFFIX\n").kind,
        Kind::Unknown
    );
}

#[test]
fn only_exact_site_aliases_and_bounded_coordinates_escape() {
    let known = report(
        b"thread 'PRIVATE' panicked at app-server/src/message_processor.rs:1730:5:\nPRIVATE\n",
    );
    assert_eq!(
        known.location,
        Some(Location {
            site: Site::SourceMessageProcessor,
            line: 1730,
            column: 5
        })
    );
    assert!(!known.to_string().contains(".rs"));
    for path in [
        "/PRIVATE/app-server/src/message_processor.rs",
        "other/app-server/src/message_processor.rs",
        "app-server/src/../src/message_processor.rs",
        "app-server/src/message_processor.rs/PRIVATE",
        "app-server/src/message_processor.rѕ",
    ] {
        assert_eq!(
            report(format!("thread 'x' panicked at {path}:1:2:\n").as_bytes()).location,
            None
        );
    }
    for coordinates in [
        "0:1",
        "1:0",
        "01:2",
        "-1:2",
        "1:+2",
        "1000001:1",
        "1:1000001",
        "42949672960:1",
        "1:2:PRIVATE",
    ] {
        assert_eq!(
            report(
                format!("thread 'x' panicked at library/core/src/panicking.rs:{coordinates}:\n")
                    .as_bytes()
            )
            .location,
            None
        );
    }
    let first_unknown = report(b"thread 'x' panicked at /PRIVATE/file.rs:1:2:\nthread 'x' panicked at library/core/src/panicking.rs:3:4:\n");
    assert_eq!(first_unknown.location, None);
    assert_eq!(
        report(b"thread '\xff' panicked at /\xff/private.rs:1:2:\n").location,
        None
    );
}

#[test]
fn split_reads_preserve_bytes_and_eof_without_exposing_them() {
    let capture = Arc::new(Capture::new());
    let input =
        b"thread 'PRIVATE' panicked at library/core/src/panicking.rs:3:4:\nPRIVATE_PAYLOAD\n";
    let mut reader = Stderr::new(io::Cursor::new(input), capture.clone());
    let mut actual = Vec::new();
    let mut byte = [0; 1];
    while reader.read(&mut byte).unwrap() != 0 {
        actual.push(byte[0]);
    }
    assert_eq!(actual, input);
    drop(reader);
    let result = capture.report(Duration::ZERO);
    assert_eq!(result.kind, Kind::RustPanic);
    assert_eq!(result.completion, Completion::Eof);
    assert!(!result.to_string().contains("PRIVATE"));
}

#[test]
fn bounded_tail_ignores_its_truncated_first_line() {
    let capture = Capture::new();
    capture.append(&vec![b'x'; LIMIT]);
    capture.append(b"fatal runtime error: stack overflow, aborting\n");
    capture.finish(Completion::Eof);
    let result = capture.report(Duration::ZERO);
    assert!(result.truncated);
    assert_eq!(result.kind, Kind::Unknown);
    assert_eq!(capture.state.lock().unwrap().bytes.len(), LIMIT);
    let capture = Capture::new();
    capture.append(&vec![b'x'; LIMIT]);
    capture.append(b"\nfatal runtime error: stack overflow, aborting\n");
    capture.finish(Completion::Eof);
    assert_eq!(capture.report(Duration::ZERO).kind, Kind::StackOverflow);
}

#[test]
fn errors_drop_and_zero_length_reads_do_not_become_eof() {
    struct Failure;
    impl Read for Failure {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("PRIVATE_ERROR"))
        }
    }
    let capture = Arc::new(Capture::new());
    let mut reader = Stderr::new(Failure, capture.clone());
    assert_eq!(
        reader.read(&mut [0]).unwrap_err().kind(),
        io::ErrorKind::Other
    );
    drop(reader);
    assert_eq!(
        capture.report(Duration::ZERO).completion,
        Completion::ReadError
    );
    assert!(
        !capture
            .report(Duration::ZERO)
            .to_string()
            .contains("PRIVATE")
    );
    let capture = Arc::new(Capture::new());
    let mut reader = Stderr::new(io::Cursor::new(b""), capture.clone());
    assert_eq!(reader.read(&mut []).unwrap(), 0);
    assert_eq!(capture.report(Duration::ZERO).completion, Completion::Open);
    drop(reader);
    assert_eq!(
        capture.report(Duration::ZERO).completion,
        Completion::Unavailable
    );
}

#[test]
fn completion_wait_observes_real_drain_and_open_capture_stays_partial() {
    let capture = Arc::new(Capture::new());
    assert_eq!(capture.report(Duration::ZERO).completion, Completion::Open);
    let writer = capture.clone();
    let thread = std::thread::spawn(move || {
        let mut reader = Stderr::new(
            io::Cursor::new(b"fatal runtime error: stack overflow, aborting\n"),
            writer,
        );
        io::copy(&mut reader, &mut io::sink()).unwrap();
    });
    let result = capture.report(Duration::from_secs(10));
    thread.join().unwrap();
    assert_eq!(result.completion, Completion::Eof);
    assert_eq!(result.kind, Kind::StackOverflow);
}
