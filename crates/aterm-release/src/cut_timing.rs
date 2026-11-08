// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Cut gate accounting. CPU is the same reaped-child scope as step-child-finished;
//! self CPU is separate so hashing in the cutter is visible too. Nested gates are
//! inclusive and must not be added to their parent.

use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub(crate) fn measure<T>(name: &str, run: impl FnOnce() -> T) -> T {
    let timer = Timer::new(name);
    let result = run();
    drop(timer);
    result
}

pub(crate) struct Timer<'a> {
    name: &'a str,
    start: Instant,
    at: u64,
    cpu: Option<aterm_sysprobe::jobs::Counters>,
    log: Option<std::path::PathBuf>,
}

impl<'a> Timer<'a> {
    pub(crate) fn new(name: &'a str) -> Self {
        Self {
            name,
            start: Instant::now(),
            at: now(),
            cpu: aterm_sysprobe::jobs::counters_of(std::process::id()),
            log: None,
        }
    }

    /// Keep the same event in the operation's log, including when an error unwinds it.
    pub(crate) fn start(log: &std::path::Path, name: &'a str) -> Self {
        use std::io::Write as _;
        let mut timer = Self::new(name);
        timer.log = Some(log.to_owned());
        let event = aterm_json::json!({
            "schema": 1, "event": "cut-gate-started", "gate": name,
            "started_at_unix_ms": timer.at,
        });
        if let Ok(mut sink) = crate::durable::open_append(log, true) {
            let _ = writeln!(sink, "release-event: {event}");
        }
        timer
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn delta(before: Option<u64>, after: Option<u64>) -> aterm_json::Value {
    before
        .zip(after)
        .and_then(|(b, a)| a.checked_sub(b))
        .map_or(aterm_json::Value::Null, |ns| (ns as f64 / 1e9).into())
}

impl Drop for Timer<'_> {
    fn drop(&mut self) {
        let after = aterm_sysprobe::jobs::counters_of(std::process::id());
        let child = delta(
            self.cpu.as_ref().and_then(|c| c.child_cpu_ns),
            after.as_ref().and_then(|c| c.child_cpu_ns),
        );
        let own = delta(
            self.cpu.as_ref().map(|c| c.cpu_ns),
            after.as_ref().map(|c| c.cpu_ns),
        );
        let wall = self.start.elapsed().as_secs_f64();
        println!(
            "  timing     {}: wall {:.3}s · self CPU {}s · child CPU {}s (inclusive)",
            self.name, wall, own, child
        );
        let event = aterm_json::json!({
            "schema": 1, "event": "cut-gate-finished", "gate": self.name,
            "started_at_unix_ms": self.at, "ended_at_unix_ms": now(),
            "execution_s": wall, "self_cpu_s": own,
            "child_cpu": {"scope": "reaped-child-and-waited-descendants", "observed_s": child},
            "inclusive": true
        });
        println!("release-event: {event}");
        if let Some(path) = &self.log {
            use std::io::Write as _;
            if let Ok(mut log) = crate::durable::open_append(path, true) {
                let _ = writeln!(log, "release-event: {event}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_or_regressed_counters_are_not_zero_cpu() {
        assert!(delta(None, Some(3)).is_null());
        assert!(delta(Some(4), Some(3)).is_null());
        assert_eq!(
            delta(Some(1_000_000_000), Some(3_000_000_000)).as_f64(),
            Some(2.0)
        );
        assert_eq!(measure("return-value", || 7), 7);
    }
}
