// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A monotonic clock advanced by an isolated test in another process. Callers obtain the
//! optional directory through `dev_seam!`, so shipped callers always use the native clock.
//! A reading is acknowledged only after the caller has judged the property under test.
//! `<key>.ready` announces a read; an optional `<key>.hold` barrier holds that read until the
//! fixture removes it. Both barriers use a one-minute hang detector.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// A native clock plus a monotonic millisecond offset, scoped to one fixture and operation.
pub struct Clock {
    path: Option<PathBuf>,
    offset: AtomicU64,
}

impl Clock {
    /// Use `directory/key` for externally advanced time, or the native clock without a directory.
    pub fn new(directory: Option<std::ffi::OsString>, key: &str) -> Self {
        Self {
            path: directory.map(|dir| PathBuf::from(dir).join(key)),
            offset: AtomicU64::new(0),
        }
    }

    /// Read time. Missing or partially written input keeps the last offset; time never retreats.
    ///
    /// # Panics
    /// Panics when the fixture leaves its read barrier held for a minute.
    pub fn read(&self) -> Reading<'_> {
        if let Some(path) = &self.path {
            let _ = std::fs::write(path.with_extension("ready"), b"");
            let hang = Instant::now() + Duration::from_secs(60);
            while path.with_extension("hold").exists() {
                assert!(
                    Instant::now() < hang,
                    "test clock held past its hang detector: {path:?}"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        let real = Instant::now();
        let offset = self
            .path
            .as_ref()
            .and_then(|path| {
                std::fs::read_to_string(path)
                    .ok()?
                    .trim()
                    .parse::<u64>()
                    .ok()
            })
            .filter(|&offset| real.checked_add(Duration::from_millis(offset)).is_some())
            .unwrap_or(0);
        let offset = self.offset.fetch_max(offset, Ordering::SeqCst).max(offset);
        Reading {
            clock: self,
            now: real
                .checked_add(Duration::from_millis(offset))
                .unwrap_or(real),
            offset,
        }
    }

    /// Whether this operation has an external clock.
    pub fn advanced(&self) -> bool {
        self.path.is_some()
    }
}

/// One reading, and the receipt that its consumer has finished judging it.
pub struct Reading<'a> {
    clock: &'a Clock,
    /// Native monotonic time plus the fixture's last accepted offset.
    pub now: Instant,
    offset: u64,
}

impl Reading<'_> {
    /// Record that the consumer judged this reading. The fixture waits for this receipt before
    /// drawing its next event; a clock read alone is not proof the decision used it.
    pub fn observed(self) {
        if let Some(path) = &self.clock.path {
            let _ = std::fs::write(path.with_extension("seen"), self.offset.to_string());
        }
    }

    /// A deadline on this clock expressed on the native wait clock. Advanced clocks recheck
    /// within 20 ms so a change needs no unrelated terminal event to wake the consumer.
    pub fn wait_until(&self, until: Instant) -> Instant {
        if !self.clock.advanced() {
            return until;
        }
        let left = until.saturating_duration_since(self.now);
        Instant::now() + left.min(Duration::from_millis(20))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_advances_monotonically_and_only_a_judged_reading_gets_a_receipt() {
        let root = crate::test_scratch_dir("clock");
        let clock = Clock::new(Some(root.path().as_os_str().to_owned()), "operation");
        let path = root.path().join("operation");
        let before = clock.read().now;
        std::fs::write(&path, "32000").unwrap();
        let after = clock.read();
        assert!(after.now.duration_since(before) >= Duration::from_secs(32));
        assert!(!path.with_extension("seen").exists());
        after.observed();
        assert_eq!(
            std::fs::read_to_string(path.with_extension("seen")).unwrap(),
            "32000"
        );
        for value in ["0", "partial"] {
            std::fs::write(&path, value).unwrap();
            assert!(clock.read().now >= before + Duration::from_secs(32));
        }
        let native = Clock::new(None, "unused");
        let reading = native.read();
        let until = reading.now + Duration::from_secs(60);
        assert_eq!(reading.wait_until(until), until);
    }

    #[test]
    fn a_held_read_takes_the_time_written_before_its_release() {
        let root = crate::test_scratch_dir("clock");
        let path = root.path().join("operation");
        std::fs::write(path.with_extension("hold"), "").unwrap();
        let clock = Clock::new(Some(root.path().as_os_str().to_owned()), "operation");
        let (sent, heard) = std::sync::mpsc::channel();
        let before = Instant::now();
        let reader = std::thread::spawn(move || {
            let reading = clock.read();
            sent.send(reading.now).unwrap();
            reading.observed();
        });
        aterm_spec::wait::until_within(
            Duration::from_secs(60),
            "the read reached its barrier",
            || path.with_extension("ready").exists().then_some(()),
        );
        assert_eq!(heard.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty));
        std::fs::write(&path, "25000").unwrap();
        std::fs::remove_file(path.with_extension("hold")).unwrap();
        assert!(
            heard.recv_timeout(Duration::from_secs(60)).unwrap()
                >= before + Duration::from_secs(25)
        );
        reader.join().unwrap();
        assert_eq!(
            std::fs::read_to_string(path.with_extension("seen")).unwrap(),
            "25000"
        );
    }
}
