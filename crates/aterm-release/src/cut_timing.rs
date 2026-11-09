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

thread_local! {
    static STACK: RefCell<Vec<(u64, Weak<Mutex<Cost>>)>> = const { RefCell::new(Vec::new()) };
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
            stack.push((id, Arc::downgrade(&children)));
            parent
        });
        Self {
            name: name.to_owned(),
            start: Instant::now(),
            at: now(),
            cpu: aterm_sysprobe::jobs::counters_of(std::process::id()),
            children,
            parent_id: parent.as_ref().map(|(id, _)| *id),
            parent: parent.map(|(_, cost)| cost),
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
            timer.parent_id = parent.as_ref().map(|(id, _)| *id);
            timer.parent = parent.as_ref().map(|(_, cost)| cost.clone());
            timer.cpu = None;
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
        use std::io::Write as _;
        let mut timer = Self::new(name);
        timer.log = Some(log.to_owned());
        let event = aterm_json::json!({
            "schema": 1, "event": "cut-gate-started", "gate": name,
            "process_id": std::process::id(), "gate_id": timer.id, "parent_gate_id": timer.parent_id,
            "process_image": image_id(),
            "started_at_unix_ms": timer.at,
            "self_cpu_ns": timer.cpu.as_ref().map(|c| c.cpu_ns),
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
        STACK.with_borrow_mut(|stack| stack.retain(|(id, _)| *id != self.id));
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
                let _ = writeln!(log, "release-event: {event}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arrival_requires_the_execing_pid() {
        assert!(is_exec_receiver(Some("42"), 42));
        assert!(!is_exec_receiver(Some("43"), 42));
        assert!(!is_exec_receiver(Some("invalid"), 42));
        assert!(!is_exec_receiver(None, 42));
    }

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

    /// NEGATIVE CONTROL: a plain worker-local Timer loses its parent and
    /// reports process-wide CPU as though that worker alone consumed it.
    #[test]
    fn parallel_gates_keep_parent_identity_without_double_counting_cpu() {
        let root = Timer::new("parent");
        let worker_timer = Timer::parallel_parent();
        std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let timer = worker_timer("worker");
                    assert_eq!(timer.parent_id, Some(root.id));
                    assert!(timer.cpu.is_none());
                })
                .join()
                .unwrap();
        });
        let children = *root.children.lock().unwrap();
        assert!(children.parallel);
        assert!(children.own.is_none());
        assert!(children.child.is_none());
    }

    #[test]
    fn exclusive_cost_keeps_between_gate_work_without_counting_nested_work_twice() {
        let total = Cost {
            wall: 20.0,
            own: Some(8.0),
            child: Some(12.0),
            parallel: false,
        };
        let phase = Cost {
            wall: 10.0,
            own: Some(4.0),
            child: Some(9.0),
            parallel: false,
        };
        let nested = Cost {
            wall: 3.0,
            own: Some(1.0),
            child: Some(2.0),
            parallel: false,
        };
        let mut sum = total.excluding(phase);
        sum.add(phase.excluding(nested));
        sum.add(nested);
        assert_eq!(
            (sum.wall, sum.own, sum.child),
            (total.wall, total.own, total.child)
        );
        let unknown = total.excluding(Cost {
            child: None,
            ..phase
        });
        assert_eq!(unknown.child, None);
        assert_eq!(phase.excluding(total).child, None);
        let root = Timer::new("root");
        assert_eq!(root.parent_id, None);
        let child = Timer::new("nested");
        assert_eq!(child.parent_id, Some(root.id));
        assert_ne!(child.id, root.id);
        drop(child);
        assert!(root.children.lock().unwrap().wall > 0.0);
        assert_eq!(STACK.with_borrow(Vec::len), 1);
        drop(root);
        assert!(STACK.with_borrow(Vec::is_empty));
    }

    /// NEGATIVE CONTROL: merging the log sink without the cost/identity fields
    /// loses the exclusive accounting, while keeping only stdout loses the log.
    #[test]
    fn logged_timing_keeps_the_same_identity_and_exclusive_cost() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let log = dir.path().join("phase.log");
        let root = Timer::new("root");
        let timer = Timer::start(&log, "logged");
        let id = timer.id;
        drop(timer);
        let events: Vec<aterm_json::Value> = std::fs::read_to_string(&log)
            .unwrap()
            .lines()
            .map(|line| {
                aterm_json::from_str(line.strip_prefix("release-event: ").unwrap()).unwrap()
            })
            .collect();
        assert_eq!(events.len(), 2);
        for event in &events {
            assert_eq!(event["gate_id"].as_u64(), Some(id));
            assert_eq!(event["process_image"].as_str(), Some(image_id()));
            assert_eq!(event["parent_gate_id"].as_u64(), Some(root.id));
            assert_eq!(
                event["process_id"].as_u64(),
                Some(u64::from(std::process::id()))
            );
        }
        assert_eq!(events[0]["event"].as_str(), Some("cut-gate-started"));
        assert_eq!(events[1]["event"].as_str(), Some("cut-gate-finished"));
        assert!(events[1]["exclusive"]["execution_s"].as_f64().is_some());
    }
}
