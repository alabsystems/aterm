// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    PassVerdict, SeedMarkers, SeedRetire, Wake, classify_pass_exit, pass_said_detail,
    read_pass_stderr, read_seed_markers, seed_retire, seed_retire_event,
};

fn drive(script: &str) -> (SeedMarkers, bool, Vec<Wake>) {
    let (seen, code, posted) = drive_code(script);
    (seen, code == Some(0), posted)
}

fn drive_code(script: &str) -> (SeedMarkers, Option<i32>, Vec<Wake>) {
    let mut child = aterm_uds::spawnfd::command("/bin/sh")
        .arg("-c")
        .arg(script)
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("the fake atpkg child spawns");
    let mut posted = Vec::new();
    let seen = read_seed_markers(
        std::io::BufReader::new(child.stdout.take().expect("piped stdout")),
        |event| posted.push(event),
    );
    let code = child.wait().ok().and_then(|s| s.code());
    (seen, code, posted)
}

#[test]
fn an_announced_pass_that_exits_zero_is_not_a_failure() {
    let (seen, ok, posted) = drive(
        "echo 'atpkg: net-starting: installing 2 program(s) over the network: claude, codex \
         (~410 MB to download)'; exit 0",
    );
    assert!(seen.saw_start, "the announcement must be seen");
    assert!(!seen.saw_terminal, "this child prints no terminal marker");
    assert!(ok, "the child exited 0");
    assert!(
        matches!(posted.as_slice(), [Wake::PkgSeedStarted { .. }]),
        "the stream itself raises only the announcement: {posted:?}"
    );
    let verdict = seed_retire(seen, ok, true);
    assert_eq!(
        verdict,
        SeedRetire::Finished,
        "an announced pass that exited ZERO over a stocked store did not fail"
    );
    assert!(
        !matches!(
            seed_retire_event(verdict, ""),
            Some(Wake::PkgSeedFailed { .. })
        ),
        "the banner must not say the install failed"
    );
}

#[test]
fn a_child_that_dies_after_announcing_still_reports_a_failure() {
    let (seen, ok, _) = drive("echo 'atpkg: net-starting: installing 9 program(s)'; exit 7");
    assert!(seen.saw_start && !seen.saw_terminal);
    assert!(!ok);
    assert_eq!(seed_retire(seen, ok, true), SeedRetire::Failed);
    assert!(matches!(
        seed_retire_event(seed_retire(seen, ok, true), "atpkg: the store lock is held"),
        Some(Wake::PkgSeedFailed { .. })
    ));
}

#[test]
fn the_quiet_steady_state_raises_nothing() {
    let (seen, ok, posted) =
        drive("echo 'atpkg: the store already holds the ALab toolset — nothing to seed'; exit 0");
    assert!(!seen.saw_start && !seen.saw_terminal);
    assert!(ok);
    assert!(
        posted.is_empty(),
        "a plain sentence is not a marker: {posted:?}"
    );
    for stocked in [true, false] {
        assert_eq!(
            seed_retire(seen, ok, stocked),
            SeedRetire::Nothing,
            "no announcement, no card — whatever the store holds"
        );
        assert!(seed_retire_event(seed_retire(seen, ok, stocked), "").is_none());
    }
}

#[test]
fn the_positive_terminal_answers_the_announcement() {
    let (seen, ok, posted) = drive(
        "echo 'atpkg: net-starting: installing 2 program(s)'; \
         echo 'atpkg: seed-done: the pass finished; 12 ALab program(s) are installed'; exit 0",
    );
    assert!(seen.saw_start, "the announcement opened");
    assert!(
        seen.saw_terminal,
        "`seed-done:` is a TERMINAL — the absence the GUI used to read is gone"
    );
    assert!(matches!(
        posted.as_slice(),
        [Wake::PkgSeedStarted { .. }, Wake::PkgSeedDone { .. }]
    ));
    assert_eq!(seed_retire(seen, ok, true), SeedRetire::Nothing);

    let (seen, ok, posted) = drive(
        "echo 'atpkg: net-starting: installing 2 program(s) over the network: claude, codex'; \
         echo 'atpkg: net-installed: claude, codex'; exit 0",
    );
    assert!(
        seen.saw_start && seen.saw_terminal,
        "`net-installed:` is a TERMINAL too"
    );
    assert!(matches!(
        posted.as_slice(),
        [Wake::PkgSeedStarted { .. }, Wake::PkgSeed { .. }]
    ));
    assert_eq!(seed_retire(seen, ok, true), SeedRetire::Nothing);
}

#[test]
fn an_informational_row_does_not_answer_an_announcement() {
    let (seen, ok, posted) = drive(
        "echo 'atpkg: net-starting: installing 2 program(s) over the network: claude, codex'; \
         echo 'atpkg: managed-current: claude 2.1.280 (Anthropic latest)'; exit 0",
    );
    assert!(seen.saw_start);
    assert!(
        !seen.saw_terminal,
        "an R6 row says what the machine HAS, not how the pass ended"
    );
    assert_eq!(posted.len(), 2, "{posted:?}");
    assert_eq!(seed_retire(seen, ok, true), SeedRetire::Finished);
}

#[test]
fn a_contended_pass_is_not_a_failed_install() {
    let (seen, code, posted) = drive_code(&format!(
        "echo 'atpkg: seed-busy: another atpkg process holds the store lock \
         — that pass is doing this work and this one stood aside'; exit {}",
        atpkg::lock::CONTENDED_EXIT
    ));
    assert_eq!(
        code,
        Some(75),
        "the contended child exits atpkg's contention code"
    );
    assert!(
        seen.saw_terminal && seen.saw_busy && !seen.saw_start,
        "`seed-busy:` ENDS the pass, says so, and opens nothing: {seen:?}"
    );
    assert!(
        posted.is_empty(),
        "the line is the lane's to log, not an event: {posted:?}"
    );
    assert_eq!(
        classify_pass_exit(code, seen.saw_start, seen.saw_terminal),
        PassVerdict::Busy,
        "a contended pass is deferred, never a failed install"
    );
    let (bare, bare_code, bare_posted) =
        drive_code("echo 'atpkg: the prefix is not writable' >&2; exit 1");
    assert_eq!(bare_code, Some(1));
    assert!(
        bare_posted.is_empty() && bare == SeedMarkers::default(),
        "{bare:?}"
    );
    assert_eq!(
        classify_pass_exit(bare_code, bare.saw_start, bare.saw_terminal),
        PassVerdict::Refused,
        "a real refusal still reaches the card"
    );
}

#[test]
fn an_announced_pass_that_installs_nothing_says_so() {
    let (seen, ok, _) = drive("echo 'atpkg: net-starting: installing 2 program(s)'; exit 0");
    assert_eq!(seed_retire(seen, ok, false), SeedRetire::FinishedEmpty);
    assert!(matches!(
        seed_retire_event(SeedRetire::FinishedEmpty, ""),
        Some(Wake::PkgSeedNothing { detail }) if detail.is_empty()
    ));
    assert!(matches!(
        seed_retire_event(SeedRetire::FinishedEmpty, "  ay: no build for this Mac\n"),
        Some(Wake::PkgSeedNothing { detail }) if detail == "ay: no build for this Mac"
    ));
    assert!(matches!(
        seed_retire_event(SeedRetire::Finished, ""),
        Some(Wake::PkgSeedDone { detail }) if detail.is_empty()
    ));
}

#[test]
fn a_non_utf8_stdout_line_does_not_end_the_marker_reader() {
    let (seen, code, posted) = drive_code(
        "echo 'atpkg: net-starting: installing 2 program(s)'; \
         printf 'brew: \\377\\376 not utf-8\\n'; \
         echo 'atpkg: seed-done: the pass finished; 12 ALab program(s) are installed'; \
         exit 0",
    );
    assert_eq!(code, Some(0));
    assert!(
        seen.saw_start,
        "the announcement before the bad line: {seen:?}"
    );
    assert!(
        seen.saw_terminal,
        "the terminal AFTER one non-UTF-8 line must still be read: {seen:?} {posted:?}"
    );
    assert!(
        matches!(
            posted.as_slice(),
            [Wake::PkgSeedStarted { .. }, Wake::PkgSeedDone { .. }]
        ),
        "both markers posted, the raw line raised nothing: {posted:?}"
    );
}

#[test]
fn a_non_utf8_stderr_byte_does_not_drop_the_refusal() {
    let mut child = aterm_uds::spawnfd::command("/bin/sh")
        .arg("-c")
        .arg(
            "echo 'atpkg: the prefix is not writable: /x' >&2; \
             printf 'brew: \\377\\n' >&2; \
             echo 'atpkg: refusing to mutate the store without the lock' >&2; \
             exit 1",
        )
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::piped())
        .spawn()
        .expect("the fake atpkg child spawns");
    let (said, unread) = read_pass_stderr(child.stderr.take().expect("piped stderr"));
    assert!(!unread);
    assert_eq!(child.wait().ok().and_then(|s| s.code()), Some(1));
    assert!(
        said.contains("the prefix is not writable")
            && said.contains("refusing to mutate the store"),
        "every sentence around the raw byte is kept: {said:?}"
    );
    assert!(
        pass_said_detail(&said).contains("the prefix is not writable"),
        "the card quotes the refusal: {:?}",
        pass_said_detail(&said)
    );
}

#[test]
fn a_large_pass_stderr_is_drained_and_keeps_a_bounded_failure_tail() {
    use std::io::Read as _;
    let suffix = "\natpkg: refusing to mutate the store — final failure\n";
    let mut pipe = std::io::repeat(b'x')
        .take((super::PASS_STDERR_KEEP * 16) as u64)
        .chain(std::io::Cursor::new(suffix.as_bytes()));
    let (said, unread) = read_pass_stderr(&mut pipe);
    assert!(!unread);
    assert!(said.len() <= super::PASS_STDERR_KEEP + super::PASS_STDERR_OMITTED.len());
    assert!(said.ends_with(suffix), "the failure at the end survives");
    assert_eq!(
        pipe.read(&mut [0]).unwrap(),
        0,
        "the whole pipe was drained"
    );
}

#[test]
fn oversized_stdout_supplies_no_partial_marker_and_cannot_prove_a_clean_pass() {
    use std::io::Read as _;
    let start = format!("atpkg: {}", atpkg::cli::NET_STARTING_MARKER);
    let end = format!("\natpkg: {}complete", atpkg::cli::SEED_DONE_MARKER);
    let pipe = std::io::Cursor::new(start.into_bytes())
        .chain(std::io::repeat(b'x').take((super::PASS_STDOUT_LINE_MAX + 1) as u64))
        .chain(std::io::Cursor::new(end.into_bytes()));
    let mut pipe = std::io::BufReader::new(pipe);
    let mut posted = Vec::new();
    let seen = read_seed_markers(&mut pipe, |event| posted.push(event));
    assert!(
        !seen.saw_start,
        "an oversized frame supplies no announcement"
    );
    assert!(
        seen.saw_terminal,
        "the complete frame after it is still read"
    );
    assert!(matches!(posted.as_slice(), [Wake::PkgSeedDone { .. }]));
    assert_eq!(pipe.read(&mut [0]).unwrap(), 0);
    let run = super::PassRun {
        seen,
        code: Some(0),
        holder_advanced: false,
        said: String::new(),
        pass_seq_before: None,
    };
    assert!(
        !run.clean(),
        "discarded protocol data supplies no success proof"
    );
    assert_eq!(run.verdict(), PassVerdict::Refused);
    assert!(!run.why().is_empty());
}

#[test]
fn an_unterminated_stdout_marker_lost_to_io_error_cannot_prove_success() {
    struct BrokenAtEnd(std::io::Cursor<Vec<u8>>);
    impl std::io::Read for BrokenAtEnd {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = std::io::Read::read(&mut self.0, buf)?;
            if n == 0 {
                return Err(std::io::ErrorKind::Other.into());
            }
            Ok(n)
        }
    }
    let data = format!("atpkg: {}partial", atpkg::cli::SEED_DONE_MARKER);
    let mut posted = Vec::new();
    let seen = read_seed_markers(
        std::io::BufReader::new(BrokenAtEnd(std::io::Cursor::new(data.into_bytes()))),
        |event| posted.push(event),
    );
    assert!(
        posted.is_empty(),
        "an I/O error is not a complete final frame"
    );
    let run = super::PassRun {
        seen,
        code: Some(0),
        holder_advanced: false,
        said: String::new(),
        pass_seq_before: None,
    };
    assert!(!run.clean());
    assert_eq!(run.verdict(), PassVerdict::Refused);
}

#[test]
fn pass_frame_boundaries_preserve_exact_limits_and_recover_after_interruption() {
    for data in [b"abc".as_slice(), b"abc\n".as_slice()] {
        for split in 0..=data.len() {
            let mut frames = super::PassMarkerFrames::new(3);
            let mut got = Vec::new();
            frames.push(&data[..split], |raw| got.push(raw.to_vec()));
            frames.push(&data[split..], |raw| got.push(raw.to_vec()));
            assert!(!frames.finish(true, |raw| got.push(raw.to_vec())));
            assert_eq!(got, [b"abc".to_vec()]);
        }
    }

    struct InterruptedOnThird {
        data: std::io::Cursor<&'static [u8]>,
        reads: usize,
    }
    impl std::io::Read for InterruptedOnThird {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.reads += 1;
            if self.reads == 3 {
                return Err(std::io::ErrorKind::Interrupted.into());
            }
            std::io::Read::read(&mut self.data, buf)
        }
    }
    let input = std::io::BufReader::with_capacity(
        2,
        InterruptedOnThird {
            data: std::io::Cursor::new(b"atpkg: net-starting: ready\r\n"),
            reads: 0,
        },
    );
    let expected = read_seed_markers(
        std::io::Cursor::new(b"atpkg: net-starting: ready\r\n"),
        |_| {},
    );
    let seen = read_seed_markers(input, |_| {});
    assert_eq!(seen, expected);
    assert!(seen.saw_start);
    assert!(!seen.stdout_unread);
}

#[test]
fn incremental_pass_frames_discard_whole_oversize_and_unfinished_frames() {
    let data = b"abcd\nok\r\nend";
    for chunk_size in 1..=data.len() {
        let mut frames = super::PassMarkerFrames::new(3);
        let mut got = Vec::new();
        for chunk in data.chunks(chunk_size) {
            frames.push(chunk, |raw| got.push(raw.to_vec()));
            assert!(frames.raw.len() <= 3);
        }
        assert!(frames.finish(true, |raw| got.push(raw.to_vec())));
        assert_eq!(got, [b"ok\r".to_vec(), b"end".to_vec()]);
    }
    for eof in [false, true] {
        let mut frames = super::PassMarkerFrames::new(3);
        let mut got = Vec::new();
        frames.push(b"ok\nend", |raw| got.push(raw.to_vec()));
        assert_eq!(got, [b"ok".to_vec()], "a chunk boundary supplies no EOF");
        assert_eq!(frames.finish(eof, |raw| got.push(raw.to_vec())), !eof);
        assert_eq!(got.len(), if eof { 2 } else { 1 });
    }
}

#[test]
fn marker_events_and_utf8_do_not_depend_on_pipe_chunk_boundaries() {
    let data = format!(
        "atpkg: {}working 🙂\r\natpkg: {}complete",
        atpkg::cli::NET_STARTING_MARKER,
        atpkg::cli::SEED_DONE_MARKER
    );
    let mut expected = Vec::new();
    let expected_seen = read_seed_markers(std::io::Cursor::new(data.as_bytes()), |event| {
        expected.push(format!("{event:?}"));
    });
    assert!(expected_seen.saw_start && expected_seen.saw_terminal);
    for split in 0..=data.len() {
        let mut frames = super::PassMarkerFrames::new(super::PASS_STDOUT_LINE_MAX);
        let mut seen = SeedMarkers::default();
        let mut events = Vec::new();
        let mut post = |event| events.push(format!("{event:?}"));
        frames.push(&data.as_bytes()[..split], |raw| {
            super::record_seed_frame(raw, &mut seen, &mut post)
        });
        frames.push(&data.as_bytes()[split..], |raw| {
            super::record_seed_frame(raw, &mut seen, &mut post)
        });
        seen.stdout_unread = frames.finish(true, |raw| {
            super::record_seed_frame(raw, &mut seen, &mut post)
        });
        assert_eq!(seen, expected_seen, "split {split}");
        assert_eq!(events, expected, "split {split}");
    }
}

#[test]
fn unread_stdout_retains_a_bounded_cause_and_never_synthesizes_success() {
    let said = "🙂".repeat(3000) + " final diagnostic";
    let run = |seen| super::PassRun {
        seen,
        code: Some(0),
        holder_advanced: false,
        said: said.clone(),
        pass_seq_before: None,
    };
    let normal = run(SeedMarkers {
        saw_start: true,
        ..SeedMarkers::default()
    });
    let unread = run(SeedMarkers {
        saw_start: true,
        stdout_unread: true,
        ..SeedMarkers::default()
    });
    let why = unread.why();
    assert!(why.chars().count() <= 2000);
    assert!(why.ends_with("final diagnostic"));
    assert_ne!(
        why,
        normal.why(),
        "the protocol fault survives the long stderr tail"
    );
    let mut posted = Vec::new();
    super::report_pass_verdict(super::PassVerb::Seed, &unread, None, |event| {
        posted.push(event)
    });
    assert!(matches!(posted.as_slice(), [Wake::PkgSeedFailed { detail }] if detail == &why));
    for stocked in [false, true] {
        assert_eq!(seed_retire(unread.seen, true, stocked), SeedRetire::Failed);
    }
}

#[test]
fn pass_stderr_keeps_partial_diagnostics_after_interruption_and_read_failure() {
    struct InterruptedThenBroken {
        data: std::io::Cursor<&'static [u8]>,
        interrupted: bool,
    }
    impl std::io::Read for InterruptedThenBroken {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if !std::mem::replace(&mut self.interrupted, true) {
                return Err(std::io::ErrorKind::Interrupted.into());
            }
            let n = std::io::Read::read(&mut self.data, buf)?;
            if n == 0 {
                return Err(std::io::ErrorKind::Other.into());
            }
            Ok(n)
        }
    }
    let (said, unread) = read_pass_stderr(InterruptedThenBroken {
        data: std::io::Cursor::new(b"before\xffafter\n"),
        interrupted: false,
    });
    assert_eq!(said, "before\u{fffd}after\n");
    assert!(unread);
    for stdout_unread in [false, true] {
        let run = super::PassRun {
            seen: SeedMarkers {
                saw_start: true,
                stderr_unread: unread,
                stdout_unread,
                ..SeedMarkers::default()
            },
            code: Some(0),
            holder_advanced: false,
            said: said.clone(),
            pass_seq_before: None,
        };
        assert!(!run.clean());
        assert_eq!(run.verdict(), PassVerdict::Refused);
        assert_eq!(seed_retire(run.seen, true, true), SeedRetire::Failed);
        let why = run.why();
        assert!(why.contains("stderr"));
        assert!(why.contains("before\u{fffd}after"));
        let mut events = Vec::new();
        super::report_pass_verdict(super::PassVerb::Seed, &run, None, |event| {
            events.push(event)
        });
        assert!(matches!(events.as_slice(), [Wake::PkgSeedFailed { detail }] if detail == &why));
    }
}
