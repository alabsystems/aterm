// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pure injected-Read controls of the actual strict drain; no child or host file is opened.

use super::*;
use std::io::Cursor;
use std::time::Duration;

const HANG: Duration = Duration::from_secs(60);

fn collect(stream: impl Read + Send + 'static) -> Result<Vec<u8>, CompleteOutputError> {
    drain_stdout(stream)
        .expect("the pure reader starts")
        .collect(Instant::now() + HANG)
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

/// The test knows the reader is waiting for release before EOF; no scheduling duration
/// stands in for completion. Drop reports that the reader's owned input was released.
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
    let reader = drain_stdout(input).unwrap();
    entered
        .recv_timeout(HANG)
        .expect("the reader reached the EOF gate");
    // The empty release channel proves no EOF is possible, regardless of scheduling.
    let incomplete = reader.collect(Instant::now());
    release.send(()).expect("release this owned reader");
    dropped
        .recv_timeout(HANG)
        .expect("the incomplete reader released its input");
    assert!(matches!(
        incomplete,
        Err(CompleteOutputError::StdoutIncomplete)
    ));

    let (input, entered, release, dropped) = gated();
    let reader = drain_stdout(input).unwrap();
    entered
        .recv_timeout(HANG)
        .expect("the second reader reached the gate");
    release.send(()).expect("permit actual EOF");
    assert_eq!(
        reader.collect(Instant::now() + HANG).unwrap(),
        b"owned prefix"
    );
    dropped
        .recv_timeout(HANG)
        .expect("the completed reader released its input");
}
