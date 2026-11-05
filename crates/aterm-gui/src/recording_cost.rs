// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Opt-in, recorder-owned completed scopes. Nested durations are inclusive, never additive.
//! CPU includes boundary-probe overhead; wall excludes the CPU probes. Their difference is
//! not a measured wait. Polling fields have no common cut while producers are active.

use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

#[derive(Debug, Default)]
struct Sum {
    total: AtomicU64,
    unknown: AtomicU64,
    invalid: AtomicBool,
}

impl Sum {
    fn add(&self, value: Option<u64>) {
        if let Some(value) = value {
            if self
                .total
                .fetch_add(value, Ordering::Relaxed)
                .checked_add(value)
                .is_none()
            {
                self.invalid.store(true, Ordering::Relaxed);
            }
        } else {
            self.invalid.store(true, Ordering::Relaxed);
            self.unknown.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn value(&self) -> Option<u64> {
        (!self.invalid.load(Ordering::Relaxed)).then(|| self.total.load(Ordering::Relaxed))
    }
}

#[derive(Debug, Default)]
pub(crate) struct Counter {
    calls: Sum,
    work: Sum,
    wall: Sum,
    cpu: Sum,
}

impl Counter {
    /// The scopes completed so far, unless a count overflowed.
    #[cfg(test)]
    pub(crate) fn calls(&self) -> Option<u64> {
        self.calls.value()
    }

    /// The work the completed scopes counted, unless one counted none or the sum overflowed.
    #[cfg(test)]
    pub(crate) fn work(&self) -> Option<u64> {
        self.work.value()
    }

    fn finish(&self, from: Stamp, to: Stamp, work: Option<u64>) {
        self.calls.add(Some(1));
        self.work.add(work);
        self.wall.add(
            to.wall
                .checked_duration_since(from.wall)
                .and_then(|duration| u64::try_from(duration.as_nanos()).ok()),
        );
        self.cpu.add(
            (from.thread == to.thread)
                .then_some(())
                .and_then(|()| to.cpu?.checked_sub(from.cpu?)),
        );
    }

    fn fields(&self, out: &mut String, name: &str, unit: &str) {
        let word = |value: Option<u64>| value.map_or_else(|| "-".into(), |n| n.to_string());
        let _ = write!(
            out,
            " recording_cost_{name}_calls={} recording_cost_{name}_work={} \
             recording_cost_{name}_unit={unit} recording_cost_{name}_wall_ns={} \
             recording_cost_{name}_cpu_ns={} recording_cost_{name}_cpu_unknown_scopes={}",
            word(self.calls.value()),
            word(self.work.value()),
            word(self.wall.value()),
            word(self.cpu.value()),
            self.cpu.unknown.load(Ordering::Relaxed)
        );
    }
}

#[derive(Debug, Default)]
pub(crate) struct Costs {
    pub(crate) ingest: Counter,
    pub(crate) copy: Counter,
    pub(crate) reader_lock: Counter,
    pub(crate) parse: Counter,
    pub(crate) record: Counter,
    pub(crate) witness: Counter,
    /// The readers' rekey captures of the live engine (`capture_keyframe` in `SpineMark::rekey`), each one capture. A slice's rekey runs inside its `record` scope; a host step's (`SpineMark::flush_host`) runs outside it, as does the closing witness such a rekey sends.
    pub(crate) rekey: Counter,
    pub(crate) apply: Counter,
    /// The shared writer's idle judging passes, their work the slice bytes each folded.
    pub(crate) judge: Counter,
}

impl Costs {
    fn fields(&self) -> String {
        let mut out = String::from(
            " recording_cost_scope=recorder_lifetime_completed,inclusive,polling_nonatomic \
            recording_cost_clock_order=cpu,wall;wall,cpu \
            recording_cost_clock_scope=cpu_includes_boundary_probes,wall_excludes_them \
            recording_cost_reader_lock_scope=acquisition_only \
            recording_cost_parse_scope=launch_process_call \
            recording_cost_nesting=ingest(copy,reader_lock,parse,record(witness,rekey));writer(apply,judge)",
        );
        self.ingest.fields(&mut out, "ingest", "input_bytes");
        self.copy.fields(&mut out, "copy", "input_bytes");
        self.reader_lock.fields(&mut out, "reader_lock", "attempts");
        self.parse.fields(&mut out, "parse", "input_bytes");
        self.record.fields(&mut out, "record", "input_bytes");
        self.witness.fields(&mut out, "witness", "attempts");
        self.rekey.fields(&mut out, "rekey", "captures");
        self.apply.fields(&mut out, "apply", "messages");
        self.judge.fields(&mut out, "judge", "bytes");
        out
    }
}

/// Kept by the existing recorder spine, shared only with its readers and queues.
#[derive(Debug)]
pub(crate) struct Owner(Option<Arc<Costs>>);

impl Default for Owner {
    fn default() -> Self {
        static ON: OnceLock<bool> = OnceLock::new();
        let on = *ON.get_or_init(|| aterm_types::dev_seam!("ATERM_TRACE_RECORDING_COST").is_some());
        Self(on.then(|| Arc::new(Costs::default())))
    }
}

impl Owner {
    /// Costs kept whatever the opt-in reads, for a test that reads them.
    #[cfg(test)]
    pub(crate) fn on() -> Self {
        Self(Some(Arc::default()))
    }

    pub(crate) fn handle(&self) -> Option<Arc<Costs>> {
        self.0.clone()
    }

    pub(crate) fn get(&self) -> Option<&Costs> {
        self.0.as_deref()
    }

    pub(crate) fn fields(&self) -> String {
        self.get().map_or_else(String::new, Costs::fields)
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Stamp {
    wall: Instant,
    cpu: Option<u64>,
    thread: std::thread::ThreadId,
}

fn stamp(close: bool) -> Stamp {
    // As submit_split does, keep the CPU probes outside the stage's wall span.
    let before = close.then(Instant::now);
    let cpu = crate::metrics::calling_thread_cpu_ns();
    Stamp {
        wall: before.unwrap_or_else(Instant::now),
        cpu,
        thread: std::thread::current().id(),
    }
}

/// Drop on the same thread that opened it. A moved scope retains wall but unknown CPU.
pub(crate) struct Scope<'a, F: FnMut(bool) -> Stamp> {
    counter: &'a Counter,
    from: Stamp,
    work: Option<u64>,
    probe: F,
}

impl<F: FnMut(bool) -> Stamp> Scope<'_, F> {
    /// Count `work` for this scope instead of what it began with: for a scope whose work is known only once it ran.
    pub(crate) const fn set_work(&mut self, work: Option<u64>) {
        self.work = work;
    }
}

impl<F: FnMut(bool) -> Stamp> Drop for Scope<'_, F> {
    fn drop(&mut self) {
        let to = (self.probe)(true);
        self.counter.finish(self.from, to, self.work);
    }
}

fn begin_with<F: FnMut(bool) -> Stamp>(
    counter: Option<&Counter>,
    work: Option<u64>,
    mut probe: F,
) -> Option<Scope<'_, F>> {
    counter.map(|counter| Scope {
        counter,
        from: probe(false),
        work,
        probe,
    })
}

type ClockProbe = fn(bool) -> Stamp;

/// The absent opt-in handle performs neither a clock read nor a counter update.
pub(crate) fn begin(counter: Option<&Counter>, work: Option<u64>) -> Option<Scope<'_, ClockProbe>> {
    begin_with(counter, work, stamp as ClockProbe)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(cpu: Option<u64>) -> Stamp {
        Stamp {
            wall: Instant::now(),
            cpu,
            thread: std::thread::current().id(),
        }
    }

    #[test]
    fn disabled_scopes_never_probe() {
        let probes = std::cell::Cell::new(0);
        drop(begin_with(None, Some(7), |_| {
            probes.set(probes.get() + 1);
            reading(Some(1))
        }));
        assert_eq!(probes.get(), 0);
    }

    #[test]
    fn unavailable_or_reset_cpu_is_unknown_without_losing_work() {
        for (from, to) in [(None, Some(5)), (Some(5), None), (Some(5), Some(4))] {
            let counter = Counter::default();
            counter.finish(reading(from), reading(to), Some(7));
            assert_eq!(counter.cpu.value(), None);
            assert_eq!(counter.cpu.unknown.load(Ordering::Relaxed), 1);
            assert_eq!(counter.calls.value(), Some(1));
            assert_eq!(counter.work.value(), Some(7));
        }
        let counter = Counter::default();
        counter.finish(reading(Some(4)), reading(Some(4)), Some(0));
        assert_eq!(
            counter.cpu.value(),
            Some(0),
            "an observed unchanged counter is known"
        );
    }

    #[test]
    fn unread_scope_stays_unknown_after_later_known_cpu_and_overflow() {
        let counter = Counter::default();
        counter.finish(reading(None), reading(None), Some(1));
        counter.finish(reading(Some(1)), reading(Some(4)), Some(1));
        assert_eq!(counter.cpu.value(), None);
        let sum = Sum::default();
        sum.add(Some(u64::MAX));
        sum.add(Some(1));
        assert_eq!(sum.value(), None);
    }
}
