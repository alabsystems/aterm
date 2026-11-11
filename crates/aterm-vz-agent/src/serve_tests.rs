// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::sync::atomic::AtomicUsize;
use std::time::Duration;

const HANG: Duration = Duration::from_secs(60);

/// A child pipe whose first write blocks until the test releases it.
struct HeldWriter {
    entered: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
    first: bool,
    fail: bool,
    written: Arc<AtomicUsize>,
}

impl Write for HeldWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.first {
            self.first = false;
            let _ = self.entered.send(());
            self.release.recv().map_err(|_| io::ErrorKind::BrokenPipe)?;
        }
        if self.fail {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        self.written.fetch_add(bytes.len(), Ordering::SeqCst);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct HeldInput {
    queue: Option<InputQueue>,
    release: Option<mpsc::Sender<()>>,
    done: mpsc::Receiver<()>,
    worker: Option<thread::JoinHandle<()>>,
}

impl HeldInput {
    fn finish(&mut self) -> bool {
        drop(self.queue.take());
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
        if self.worker.is_none() {
            return true;
        }
        if matches!(
            self.done.recv_timeout(HANG),
            Err(mpsc::RecvTimeoutError::Timeout)
        ) {
            drop(self.worker.take());
            return false;
        }
        self.worker.take().expect("the worker").join().is_ok()
    }
}

impl Drop for HeldInput {
    fn drop(&mut self) {
        // Release on every assertion failure too; do not strand the writer on its gate.
        if !self.finish() {
            eprintln!("hung: the released stdin worker did not finish within {HANG:?}");
        }
    }
}

#[test]
fn a_stalled_input_writer_keeps_both_pending_limits_until_the_write_ends() {
    for (size, chunks, fail) in [
        (
            wire::MAX_FRAME,
            MAX_PENDING_STDIN_BYTES / wire::MAX_FRAME,
            false,
        ),
        (1, MAX_PENDING_STDIN_CHUNKS, false),
        (
            wire::MAX_FRAME,
            MAX_PENDING_STDIN_BYTES / wire::MAX_FRAME,
            true,
        ),
    ] {
        let budget = InputBudget::default();
        let written = Arc::new(AtomicUsize::new(0));
        let (sender, input) = mpsc::channel();
        let (entered, writing) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let (done, ended) = mpsc::channel();
        let pipe = HeldWriter {
            entered,
            release: released,
            first: true,
            fail,
            written: Arc::clone(&written),
        };
        let worker = thread::spawn(move || {
            forward_input(pipe, input);
            let _ = done.send(());
        });
        let mut held = HeldInput {
            queue: Some(InputQueue {
                sender,
                budget: budget.clone(),
            }),
            release: Some(release),
            done: ended,
            worker: Some(worker),
        };
        let queue = held.queue.as_ref().unwrap();
        assert!(queue.push(vec![7; size]).is_ok());
        writing
            .recv_timeout(HANG)
            .expect("hung: the worker never entered its write");
        for _ in 1..chunks {
            assert!(queue.push(vec![7; size]).is_ok());
        }
        for _ in 0..3 {
            assert!(
                queue.push(Vec::new()).is_ok(),
                "empty input uses no queue entry"
            );
        }
        assert!(
            queue.push(vec![1]).is_err(),
            "the worker-held chunk stays charged"
        );
        {
            let pending = budget.0.lock().unwrap();
            assert_eq!((pending.bytes, pending.chunks), (size * chunks, chunks));
        }
        assert!(held.finish(), "hung: the released worker did not drain");
        let pending = budget.0.lock().unwrap();
        assert_eq!(
            (pending.bytes, pending.chunks),
            (0, 0),
            "all charges released"
        );
        assert_eq!(
            written.load(Ordering::SeqCst),
            if fail { 0 } else { size * chunks }
        );
    }
}
