// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pure injected-`Read` tests for strict stdout draining.

use super::*;
use std::io::Cursor;
use std::time::Duration;

const HANG: Duration = Duration::from_secs(60);

fn collect(stream: impl Read + Send + 'static) -> Result<Vec<u8>, CompleteOutputError> {
    drain_stdout(None, stream)
        .expect("the pure reader starts")
        .collect(&Grace::at(Instant::now() + HANG))
}

/// A complete prefix followed by one injected read error (or one interrupted read first).
struct Ending {
    prefix: Cursor<Vec<u8>>,
    error: Option<io::ErrorKind>,
    interrupt: bool,
}

impl Read for Ending {
    fn read(&mut self, into: &mut [u8]) -> io::Result<usize> {
        if std::mem::take(&mut self.interrupt) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let n = self.prefix.read(into)?;
        if n != 0 {
            return Ok(n);
        }
        match self.error.take() {
            Some(error) => Err(error.into()),
            None => Ok(0),
        }
    }
}

#[test]
fn complete_stdout_accepts_eof_through_the_inclusive_limit() {
    for bytes in [
        Vec::new(),
        b"complete\x00\xff".to_vec(),
        vec![b'x'; OUTPUT_KEEP],
    ] {
        assert_eq!(collect(Cursor::new(bytes.clone())).unwrap(), bytes);
    }
    assert_eq!(
        collect(Ending {
            prefix: Cursor::new(b"after an interrupted read".to_vec()),
            error: None,
            interrupt: true,
        })
        .unwrap(),
        b"after an interrupted read"
    );
}

#[test]
fn complete_stdout_refuses_a_read_error_without_publishing_its_prefix() {
    assert_eq!(
        collect(Cursor::new(b"ordinary".to_vec())).unwrap(),
        b"ordinary"
    );
    let error = collect(Ending {
        prefix: Cursor::new(b"apparently valid short prefix".to_vec()),
        error: Some(io::ErrorKind::PermissionDenied),
        interrupt: false,
    })
    .expect_err("a read failure is not an empty or short complete report");
    assert!(matches!(error, CompleteOutputError::StdoutRead(ref e)
        if e.kind() == io::ErrorKind::PermissionDenied));
}

#[test]
fn complete_stdout_refuses_every_discarded_byte_after_eof() {
    assert_eq!(
        collect(Cursor::new(vec![b'x'; OUTPUT_KEEP])).unwrap().len(),
        OUTPUT_KEEP
    );
    let error = collect(Cursor::new(vec![b'x'; OUTPUT_KEEP + 1]))
        .expect_err("one discarded byte prevents complete capture");
    assert!(matches!(error, CompleteOutputError::StdoutTooLarge { limit } if limit == OUTPUT_KEEP));
}

/// The test knows the reader is waiting for release before EOF.
struct GatedEof {
    prefix: Cursor<Vec<u8>>,
    entered: mpsc::SyncSender<()>,
    release: mpsc::Receiver<()>,
    dropped: mpsc::SyncSender<()>,
}

impl Read for GatedEof {
    fn read(&mut self, into: &mut [u8]) -> io::Result<usize> {
        let n = self.prefix.read(into)?;
        if n != 0 {
            return Ok(n);
        }
        self.entered
            .send(())
            .map_err(|_| io::ErrorKind::BrokenPipe)?;
        self.release.recv().map_err(|_| io::ErrorKind::BrokenPipe)?;
        Ok(0)
    }
}

impl Drop for GatedEof {
    fn drop(&mut self) {
        let _ = self.dropped.send(());
    }
}

fn gated() -> (
    GatedEof,
    mpsc::Receiver<()>,
    mpsc::Sender<()>,
    mpsc::Receiver<()>,
) {
    let (entered, observed) = mpsc::sync_channel(0);
    let (release, held) = mpsc::channel();
    let (dropped, gone) = mpsc::sync_channel(1);
    (
        GatedEof {
            prefix: Cursor::new(b"owned prefix".to_vec()),
            entered,
            release: held,
            dropped,
        },
        observed,
        release,
        gone,
    )
}

#[test]
fn complete_stdout_refuses_a_still_open_stream_and_accepts_proven_completion() {
    let (input, entered, release, dropped) = gated();
    let reader = drain_stdout(None, input).unwrap();
    entered
        .recv_timeout(HANG)
        .expect("the reader reached the EOF gate");
    let incomplete = reader.collect(&Grace::at(Instant::now()));
    release.send(()).expect("release this owned reader");
    dropped
        .recv_timeout(HANG)
        .expect("the incomplete reader released its input");
    assert!(matches!(
        incomplete,
        Err(CompleteOutputError::StdoutIncomplete)
    ));

    let (input, entered, release, dropped) = gated();
    let reader = drain_stdout(None, input).unwrap();
    entered
        .recv_timeout(HANG)
        .expect("the second reader reached the gate");
    release.send(()).expect("permit actual EOF");
    assert_eq!(
        reader.collect(&Grace::at(Instant::now() + HANG)).unwrap(),
        b"owned prefix"
    );
    dropped
        .recv_timeout(HANG)
        .expect("the completed reader released its input");
}

#[cfg(unix)]
#[test]
fn complete_stdout_reads_a_pipe_no_writer_holds_to_its_end() {
    use crate::deadline::tests::{Late, an_empty_group};
    use std::io::Write as _;
    let (reader, mut writer) = std::io::pipe().expect("pipe");
    writer.write_all(b"owned prefix").expect("write");
    drop(writer);
    let pipe = pipe_end(&reader);
    let (release, held) = mpsc::channel();
    let drain = drain_stdout(pipe, Late { held, reader }).expect("reader");
    let grace = Grace {
        by: Instant::now(),
        until: Instant::now() + HANG,
        group: Some(an_empty_group()),
    };
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        let _ = release.send(());
    });
    assert_eq!(drain.collect(&grace).unwrap(), b"owned prefix");
}

#[cfg(unix)]
#[test]
fn complete_stdout_reads_a_pipe_held_outside_the_childs_group_until_it_lets_go() {
    use crate::deadline::tests::{an_empty_group, lets_go};
    use std::io::Write as _;
    let (reader, mut writer) = std::io::pipe().expect("pipe");
    writer.write_all(b"one ").expect("write");
    let pipe = pipe_end(&reader);
    let drain = drain_stdout(pipe, reader).expect("reader");
    let grace = Grace {
        by: Instant::now(),
        until: Instant::now() + HANG,
        group: Some(an_empty_group()),
    };
    lets_go(writer, b"two");
    assert_eq!(drain.collect(&grace).unwrap(), b"one two");
}

/// A pipe held outside the exited child's group is read until it lets go while the kernel
/// still lists that group with no live member: Darwin answers a signal for a group left
/// only zombies with `EPERM`, and a loaded test binary saw a reaped `git`'s group answer so
/// past the grace while its stdout was still held (the repository probe read unread).
/// Negative control: with `EPERM` read as a live descendant, `collect` refuses at once.
#[cfg(target_vendor = "apple")]
#[test]
fn complete_stdout_reads_a_pipe_held_outside_a_group_left_only_a_zombie_until_it_lets_go() {
    use crate::deadline::tests::{a_group_left_only_a_zombie, lets_go};
    use std::io::Write as _;
    let zombie = a_group_left_only_a_zombie();
    let (reader, mut writer) = std::io::pipe().expect("pipe");
    writer.write_all(b"one ").expect("write");
    let pipe = pipe_end(&reader);
    let drain = drain_stdout(pipe, reader).expect("reader");
    let grace = Grace {
        by: Instant::now(),
        until: Instant::now() + HANG,
        group: Some(zombie.id()),
    };
    lets_go(writer, b"two");
    let read = drain.collect(&grace);
    drop(zombie);
    assert_eq!(read.unwrap(), b"one two");
}
