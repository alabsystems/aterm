// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Cut gate accounting. CPU is the same reaped-child scope as step-child-finished;
//! self CPU is separate so hashing in the cutter is visible too. Nested gates are
//! inclusive. Serial exclusive rows subtract immediate children. Concurrent
//! gates retain parent identity and wall intervals; their per-worker CPU and
//! the parent's exclusive costs are unknown because process counters overlap.

use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub(crate) const EXEC_PID_ENV: &str = "ATERM_RELEASE_EXEC_PID";

fn is_exec_receiver(value: Option<&str>, pid: u32) -> bool {
    value.and_then(|value| value.parse::<u32>().ok()) == Some(pid)
}

/// Exec preserves the PID and CPU counters; ordinary children must not report arrival.
pub(crate) fn image_arrived() {
    if !is_exec_receiver(
        std::env::var(EXEC_PID_ENV).ok().as_deref(),
        std::process::id(),
    ) {
        return;
    }
    let event = aterm_json::json!({
        "schema": 1, "event": "cutter-image-arrived", "process_id": std::process::id(),
        "at_unix_ms": now(),
        "self_cpu_ns": aterm_sysprobe::jobs::counters_of(std::process::id()).map(|c| c.cpu_ns),
    });
    eprintln!("release-event: {event}");
}

#[derive(Clone, Copy)]
struct Cost {
    wall: f64,
    own: Option<f64>,
    child: Option<f64>,
    parallel: bool,
}

impl Default for Cost {
    fn default() -> Self {
        Self {
            wall: 0.0,
            own: Some(0.0),
            child: Some(0.0),
            parallel: false,
        }
    }
}

impl Cost {
    fn add(&mut self, other: Self) {
        self.wall += other.wall;
        self.own = self.own.zip(other.own).map(|(a, b)| a + b);
        self.child = self.child.zip(other.child).map(|(a, b)| a + b);
        self.parallel |= other.parallel;
    }

    fn excluding(self, children: Self) -> Self {
        let subtract = |a: Option<f64>, b: Option<f64>| {
            a.zip(b)
                .and_then(|(a, b)| (a + 1e-9 >= b).then_some((a - b).max(0.0)))
        };
        Self {
            wall: (self.wall - children.wall).max(0.0),
            own: subtract(self.own, children.own),
            child: subtract(self.child, children.child),
            parallel: self.parallel || children.parallel,
        }
    }
}

#[derive(Clone)]
struct Frame {
    id: u64,
    children: Weak<Mutex<Cost>>,
    concurrent: bool,
}

thread_local! {
    static STACK: RefCell<Vec<Frame>> = const { RefCell::new(Vec::new()) };
}
static NEXT: AtomicU64 = AtomicU64::new(1);

// A native cutter handoff execs under the same PID and resets static counters.
// Keep that new image's sequence separate when its records join the old log.
fn image_id() -> &'static str {
    static IMAGE: OnceLock<String> = OnceLock::new();
    IMAGE.get_or_init(|| format!("{}:{:?}", std::process::id(), Instant::now()))
}

pub(crate) fn measure<T>(name: &str, run: impl FnOnce() -> T) -> T {
    let timer = Timer::new(name);
    let result = run();
    drop(timer);
    result
}

/// Join independent gates before the caller may consume either verdict. Even a
/// refusal joins the other gate: no gate outlives its preflight or public step.
pub(crate) fn independent<A: Send, B: Send>(
    left: (&str, impl FnOnce() -> A + Send),
    right: (&str, impl FnOnce() -> B + Send),
) -> (A, B) {
    independent_logged(None, left, right)
}

pub(crate) fn independent_logged<A: Send, B: Send>(
    log: Option<&std::path::Path>,
    left: (&str, impl FnOnce() -> A + Send),
    right: (&str, impl FnOnce() -> B + Send),
) -> (A, B) {
    let timer = Timer::parallel_parent();
    #[cfg(test)]
    let fixture = crate::github::Fixture::current();
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            #[cfg(test)]
            let _route = fixture.map(crate::github::Fixture::enter);
            let _gate = timer(left.0).with_log(log);
            left.1()
        });
        let result = {
            let _gate = timer(right.0).with_log(log);
            right.1()
        };
        (
            worker
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
            result,
        )
    })
}

pub(crate) struct Timer {
    name: String,
    start: Instant,
    at: u64,
    cpu: Option<aterm_sysprobe::jobs::Counters>,
    children: Arc<Mutex<Cost>>,
    parent: Option<Weak<Mutex<Cost>>>,
    id: u64,
    parent_id: Option<u64>,
    log: Option<std::path::PathBuf>,
}

impl Timer {
    pub(crate) fn new(name: &str) -> Self {
        let children = Arc::new(Mutex::new(Cost::default()));
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let parent = STACK.with_borrow_mut(|stack| {
            let parent = stack.last().cloned();
            stack.push(Frame {
                id,
                children: Arc::downgrade(&children),
                concurrent: parent.as_ref().is_some_and(|frame| frame.concurrent),
            });
            parent
        });
        let parallel = parent.as_ref().is_some_and(|frame| frame.concurrent);
        if parallel {
            *children.lock().unwrap() = Cost {
                own: None,
                child: None,
                parallel: true,
                ..Cost::default()
            };
        }
        Self {
            name: name.to_owned(),
            start: Instant::now(),
            at: now(),
            cpu: (!parallel)
                .then(|| aterm_sysprobe::jobs::counters_of(std::process::id()))
                .flatten(),
            children,
            parent_id: parent.as_ref().map(|frame| frame.id),
            parent: parent.map(|frame| frame.children),
            id,
            log: None,
        }
    }

    /// Carry gate identity across a worker boundary. Process CPU counters cannot
    /// attribute concurrent requests, so their CPU and the parent's exclusive
    /// accounting are explicitly unknown rather than counted more than once.
    pub(crate) fn parallel_parent() -> impl Fn(&str) -> Timer + Send + Sync {
        let parent = STACK.with_borrow(|stack| stack.last().cloned());
        move |name| {
            let mut timer = Timer::new(name);
            timer.parent_id = parent.as_ref().map(|frame| frame.id);
            timer.parent = parent.as_ref().map(|frame| frame.children.clone());
            timer.cpu = None;
            STACK.with_borrow_mut(|stack| stack.last_mut().unwrap().concurrent = true);
            *timer.children.lock().unwrap() = Cost {
                own: None,
                child: None,
                parallel: true,
                ..Cost::default()
            };
            if let Some(cost) = timer.parent.as_ref().and_then(Weak::upgrade) {
                let mut cost = cost.lock().unwrap();
                cost.own = None;
                cost.child = None;
                cost.parallel = true;
            }
            timer
        }
    }

    /// Keep the same event in the operation's log, including when an error unwinds it.
    pub(crate) fn start(log: &std::path::Path, name: &str) -> Self {
        Self::new(name).with_log(Some(log))
    }

    pub(crate) fn with_log(mut self, log: Option<&std::path::Path>) -> Self {
        use std::io::Write as _;
        let Some(log) = log else {
            return self;
        };
        self.log = Some(log.to_owned());
        let timer = &self;
        let event = aterm_json::json!({
            "schema": 1, "event": "cut-gate-started", "gate": timer.name,
            "process_id": std::process::id(), "gate_id": timer.id, "parent_gate_id": timer.parent_id,
            "process_image": image_id(),
            "started_at_unix_ms": timer.at,
            "self_cpu_ns": timer.cpu.as_ref().map(|c| c.cpu_ns),
        });
        if let Ok(mut sink) = crate::durable::open_append(log, true) {
            let _ = sink.write_all(format!("release-event: {event}\n").as_bytes());
        }
        self
    }
}

fn now() -> u64 {
    epoch_ms().unwrap_or(0)
}

/// The wall-clock timestamp, unknown before the epoch or beyond its millisecond range.
pub(crate) fn epoch_ms() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
}

fn delta(before: Option<u64>, after: Option<u64>) -> aterm_json::Value {
    before
        .zip(after)
        .and_then(|(b, a)| a.checked_sub(b))
        .map_or(aterm_json::Value::Null, |ns| (ns as f64 / 1e9).into())
}

impl Drop for Timer {
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
        let cost = Cost {
            wall,
            own: own.as_f64(),
            child: child.as_f64(),
            parallel: self.children.lock().unwrap().parallel,
        };
        let children = *self.children.lock().unwrap();
        let exclusive = cost.excluding(children);
        STACK.with_borrow_mut(|stack| stack.retain(|frame| frame.id != self.id));
        if let Some(parent) = self.parent.as_ref().and_then(Weak::upgrade) {
            parent.lock().unwrap().add(cost);
        }
        println!(
            "  timing     {}: wall {:.3}s · self CPU {}s · child CPU {}s (inclusive)",
            self.name, wall, own, child
        );
        let event = aterm_json::json!({
            "schema": 1, "event": "cut-gate-finished", "gate": self.name,
            "process_id": std::process::id(), "gate_id": self.id, "parent_gate_id": self.parent_id,
            "process_image": image_id(),
            "started_at_unix_ms": self.at, "ended_at_unix_ms": now(),
            "execution_s": wall, "self_cpu_s": own,
            "child_cpu": {"scope": "reaped-child-and-waited-descendants", "observed_s": child},
            "inclusive": true,
            "exclusive": {"execution_s": (!exclusive.parallel).then_some(exclusive.wall), "self_cpu_s": exclusive.own, "child_cpu_s": exclusive.child}
        });
        println!("release-event: {event}");
        if let Some(path) = &self.log {
            use std::io::Write as _;
            if let Ok(mut log) = crate::durable::open_append(path, true) {
                let _ = log.write_all(format!("release-event: {event}\n").as_bytes());
            }
        }
    }
}

#[cfg(test)]
#[path = "cut_timing_tests.rs"]
mod tests;
