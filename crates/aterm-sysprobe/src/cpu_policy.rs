// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded read-only CPU-policy observations. No policy setter or command runner.
//!
//! The selectors are XNU xnu-12377.1.9 bsd/sys/resource_private.h:143-153.
//! This published ABI predates some running kernels: errors and unexpected values stay errors.

use aterm_json::{Value, json};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The only privileged collector role. It accepts no operands.
pub const COLLECTOR_VERB: &str = "__collect-cpu-policy";
/// This includes every selector for the initial census, never a prompt per PID.
pub const COLLECT_FOR: Duration = Duration::from_secs(10);
/// Fixed collector exited because its intrinsic whole-role deadline expired.
pub const DEADLINE_EXIT: i32 = 124;
/// Fixed collector exited because the parent or its input pipe disappeared.
pub const PARENT_LOSS_EXIT: i32 = 125;
const MAX_PIDS: usize = 4096;
const SELECTORS: [(i32, &str); 3] = [(7, "game_mode"), (8, "carplay"), (9, "runaway_mitigation")];

/// A PID generation, independent of executable names or argument strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity {
    /// Positive kernel process ID.
    pub pid: u32,
    /// Kernel birth time, Unix seconds.
    pub start_s: u64,
    /// Microseconds within the birth second.
    pub start_us: u32,
}

/// A missing or unread value cannot represent an off flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flag {
    /// A documented OFF/ON return with errno zero.
    Value(bool),
    /// An unsuccessful getter, retaining its literal return and errno.
    Error {
        /// Literal return value.
        value: i32,
        /// Nonzero native error.
        errno: i32,
    },
    /// Errno was zero but the value was outside OFF/ON.
    Unexpected(i32),
    /// No observation was attempted.
    NotRead,
}

impl Flag {
    fn json(&self) -> Value {
        match self {
            Self::Value(on) => json!({"value": u8::from(*on)}),
            Self::Error { value, errno } => {
                json!({"error": if *errno == 1 || *errno == 13 {"permission_denied"} else {"os_error"}, "errno": errno, "raw": value})
            }
            Self::Unexpected(value) => json!({"error": "unexpected_value", "raw": value}),
            Self::NotRead => json!({"error": "not_read"}),
        }
    }
}

/// An identity observation around one process's getter bundle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IdentityCheck {
    /// The complete PID generation still matches.
    Same,
    /// This PID now names a different generation.
    Changed,
    /// The kernel reported that the PID no longer exists.
    Exited,
    /// Identity could not be observed; absence was not established.
    Unread {
        /// Native errno when the reader supplied one.
        errno: Option<i32>,
    },
    /// No observation was attempted.
    NotRead,
}

impl IdentityCheck {
    fn json(&self) -> Value {
        match self {
            Self::Same => json!({"status":"same_generation"}),
            Self::Changed => json!({"error":"pid_generation_changed"}),
            Self::Exited => json!({"error":"process_exited"}),
            Self::Unread { errno: Some(errno) } => {
                json!({"error":"identity_unread", "errno":errno})
            }
            Self::Unread { errno: None } => {
                json!({"error":"identity_unread", "errno_unavailable":true})
            }
            Self::NotRead => json!({"error":"not_read"}),
        }
    }
}

/// Literal getter results and both identity observations for one initial PID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// Initial census generation.
    pub identity: Identity,
    /// Game Mode, CarPlay and runaway mitigation, in that order.
    pub flags: [Flag; 3],
    /// Identity immediately before the getters.
    pub before: IdentityCheck,
    /// Identity after the getters, or not-read if the initial check failed.
    pub after: IdentityCheck,
}

/// One non-atomic census window, never a machine-wide all-clear.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capture {
    /// Actual collector generation.
    pub collector: Identity,
    /// Collector's parent when collection began.
    pub parent_pid: u32,
    /// Actual caller credential; no requested-privilege substitute.
    pub effective_uid: u32,
    /// Wall-clock collection start.
    pub began_unix_ms: u64,
    /// Wall-clock collection end.
    pub ended_unix_ms: u64,
    /// Truncated monotonic elapsed milliseconds, for display only.
    pub elapsed_ms: u64,
    /// Unrounded monotonic elapsed time used for completeness.
    pub elapsed_ns: u64,
    /// PID zero means the caller to getpriority, so kernel_task is not a userspace target.
    pub excluded_pid_zero: usize,
    /// Initial positive-PID population before the explicit size bound.
    pub initial_processes: usize,
    /// Initial population exceeded the fixed bound.
    pub truncated: bool,
    /// Parent loss or a cancellation request interrupted collection.
    pub cancelled: bool,
    /// Collection exceeded its monotonic budget.
    pub timed_out: bool,
    /// Whole-census refusal or unavailability, distinct from an all-off scan.
    pub error: Option<String>,
    /// Every attempted PID, including identity and getter failures.
    pub rows: Vec<Row>,
}

impl Capture {
    /// All initial identities were read, within the window, as root.
    pub fn complete(&self) -> bool {
        self.collector.pid > 0
            && self.collector.start_s > 0
            && self.effective_uid == 0
            && self.initial_processes > 0
            && self.elapsed_ns <= COLLECT_FOR.as_nanos() as u64
            && !self.truncated
            && !self.cancelled
            && !self.timed_out
            && self.error.is_none()
            && self.rows.len() == self.initial_processes
            && self.rows.iter().all(|r| {
                r.before == IdentityCheck::Same
                    && r.after == IdentityCheck::Same
                    && r.flags.iter().all(|f| matches!(f, Flag::Value(_)))
            })
    }

    /// The full result, including individual coverage gaps and the actual caller's privilege.
    pub fn json(&self) -> Value {
        let stable = |r: &&Row| r.before == IdentityCheck::Same && r.after == IdentityCheck::Same;
        let on = self
            .rows
            .iter()
            .filter(stable)
            .flat_map(|r| &r.flags)
            .filter(|f| **f == Flag::Value(true))
            .count();
        let checked = self
            .rows
            .iter()
            .filter(stable)
            .filter(|r| r.flags.iter().all(|f| matches!(f, Flag::Value(_))))
            .count();
        let errors = self
            .rows
            .iter()
            .flat_map(|r| &r.flags)
            .filter(|f| matches!(f, Flag::Error { .. } | Flag::Unexpected(_)))
            .count();
        let unread = self
            .rows
            .iter()
            .flat_map(|r| &r.flags)
            .filter(|f| !matches!(f, Flag::Value(_)))
            .count();
        let identity_gaps = self
            .rows
            .iter()
            .filter(|r| r.before != IdentityCheck::Same || r.after != IdentityCheck::Same)
            .count();
        json!({
            "schema": 1, "operation": "cpu_policy", "source": "native_getpriority",
            "collector_pid": self.collector.pid, "collector_start_s": self.collector.start_s.to_string(),
            "collector_start_us": self.collector.start_us, "parent_pid": self.parent_pid,
            "effective_uid": self.effective_uid, "began_unix_ms": self.began_unix_ms,
            "ended_unix_ms": self.ended_unix_ms, "elapsed_ms": self.elapsed_ms,
            "elapsed_ns": self.elapsed_ns, "excluded_pid_zero": self.excluded_pid_zero,
            "population": "initial_userspace_pids_greater_than_zero",
            "initial_processes": self.initial_processes, "attempted_processes": self.rows.len(), "checked_processes": checked,
            "complete_initial_census": self.complete(), "truncated": self.truncated,
            "cancelled": self.cancelled, "timed_out": self.timed_out,
            "status": self.error.as_deref().unwrap_or("observed"), "active_flags_observed": on, "getter_errors": errors, "unread_selectors": unread, "identity_gaps": identity_gaps,
            "interpretation": "observations during this window; cause not established",
            "rows": self.rows.iter().map(|row| {
                let mut obj = aterm_json::Map::new();
                obj.insert("pid".into(), json!(row.identity.pid));
                obj.insert("start_s".into(), json!(row.identity.start_s.to_string()));
                obj.insert("start_us".into(), json!(row.identity.start_us));
                obj.insert("identity_before".into(), row.before.json());
                obj.insert("identity_after".into(), row.after.json());
                for ((_, name), flag) in SELECTORS.iter().zip(&row.flags) {
                    obj.insert((*name).into(), flag.json());
                }
                Value::Object(obj)
            }).collect::<Vec<_>>()
        })
    }
}

/// Decode the owned-pipe result without converting missing/error coverage to off flags.
/// Counts and the complete flag must agree with the actual serialized rows.
pub fn decode_capture(value: &Value) -> Option<Capture> {
    fn number(v: &Value, key: &str) -> Option<u32> {
        u32::try_from(v[key].as_u64()?).ok()
    }
    fn identity(v: &Value, pid: &str, sec: &str, usec: &str) -> Option<Identity> {
        let id = Identity {
            pid: number(v, pid)?,
            start_s: v[sec].as_str()?.parse().ok()?,
            start_us: number(v, usec)?,
        };
        (id.pid > 0 && id.start_s > 0 && id.start_us < 1_000_000).then_some(id)
    }
    fn check(v: &Value) -> Option<IdentityCheck> {
        match (v["status"].as_str(), v["error"].as_str()) {
            (Some("same_generation"), None) => Some(IdentityCheck::Same),
            (None, Some("pid_generation_changed")) => Some(IdentityCheck::Changed),
            (None, Some("process_exited")) => Some(IdentityCheck::Exited),
            (None, Some("not_read")) => Some(IdentityCheck::NotRead),
            (None, Some("identity_unread")) => {
                let errno = if v["errno_unavailable"].as_bool() == Some(true) {
                    None
                } else {
                    Some(i32::try_from(v["errno"].as_i64()?).ok()?)
                };
                Some(IdentityCheck::Unread { errno })
            }
            _ => None,
        }
    }
    fn flag(v: &Value) -> Option<Flag> {
        if v["error"].is_null() {
            return match v["value"].as_u64()? {
                0 => Some(Flag::Value(false)),
                1 => Some(Flag::Value(true)),
                _ => None,
            };
        }
        match v["error"].as_str()? {
            "not_read" => Some(Flag::NotRead),
            "unexpected_value" => {
                let raw = i32::try_from(v["raw"].as_i64()?).ok()?;
                (!matches!(raw, 0 | 1)).then_some(Flag::Unexpected(raw))
            }
            "permission_denied" | "os_error" => {
                let errno = i32::try_from(v["errno"].as_i64()?).ok()?;
                let raw = i32::try_from(v["raw"].as_i64()?).ok()?;
                if errno == 0
                    || (matches!(errno, 1 | 13)) != (v["error"].as_str()? == "permission_denied")
                {
                    return None;
                }
                Some(Flag::Error { value: raw, errno })
            }
            _ => None,
        }
    }
    if value["schema"].as_u64() != Some(1)
        || value["operation"].as_str() != Some("cpu_policy")
        || value["source"].as_str() != Some("native_getpriority")
        || value["population"].as_str() != Some("initial_userspace_pids_greater_than_zero")
    {
        return None;
    }
    let raw_rows = value["rows"].as_array()?;
    if raw_rows.len() > MAX_PIDS {
        return None;
    }
    let mut rows = Vec::with_capacity(raw_rows.len());
    let mut seen = std::collections::BTreeSet::new();
    for raw in raw_rows {
        let id = identity(raw, "pid", "start_s", "start_us")?;
        if !seen.insert(id.pid) {
            return None;
        }
        rows.push(Row {
            identity: id,
            before: check(&raw["identity_before"])?,
            after: check(&raw["identity_after"])?,
            flags: [
                flag(&raw[SELECTORS[0].1])?,
                flag(&raw[SELECTORS[1].1])?,
                flag(&raw[SELECTORS[2].1])?,
            ],
        });
    }
    let status = value["status"].as_str()?;
    let capture = Capture {
        collector: identity(
            value,
            "collector_pid",
            "collector_start_s",
            "collector_start_us",
        )?,
        parent_pid: number(value, "parent_pid")?,
        effective_uid: number(value, "effective_uid")?,
        began_unix_ms: value["began_unix_ms"].as_u64()?,
        ended_unix_ms: value["ended_unix_ms"].as_u64()?,
        elapsed_ms: value["elapsed_ms"].as_u64()?,
        elapsed_ns: value["elapsed_ns"].as_u64()?,
        excluded_pid_zero: usize::try_from(value["excluded_pid_zero"].as_u64()?).ok()?,
        initial_processes: usize::try_from(value["initial_processes"].as_u64()?).ok()?,
        truncated: value["truncated"].as_bool()?,
        cancelled: value["cancelled"].as_bool()?,
        timed_out: value["timed_out"].as_bool()?,
        error: if status == "observed" {
            None
        } else {
            Some(status.to_owned())
        },
        rows,
    };
    let encoded = capture.json();
    for key in [
        "checked_processes",
        "attempted_processes",
        "complete_initial_census",
        "active_flags_observed",
        "getter_errors",
        "unread_selectors",
        "identity_gaps",
    ] {
        if encoded[key] != value[key] {
            return None;
        }
    }
    if capture.elapsed_ms != capture.elapsed_ns / 1_000_000
        || capture.rows.len() > capture.initial_processes
        || capture.began_unix_ms == 0
        || capture.ended_unix_ms < capture.began_unix_ms
    {
        return None;
    }
    Some(capture)
}

fn wall_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

/// Raw kernel result decoding; a successful getter has only the documented 0/1 values.
#[cfg(any(target_os = "macos", test))]
fn flag(value: i32, errno: i32) -> Flag {
    if errno != 0 {
        Flag::Error { value, errno }
    } else {
        match value {
            0 => Flag::Value(false),
            1 => Flag::Value(true),
            _ => Flag::Unexpected(value),
        }
    }
}

fn identity(r: &crate::kproc::Record) -> Identity {
    Identity {
        pid: r.pid,
        start_s: r.start_s,
        start_us: r.start_us,
    }
}

#[cfg(any(target_os = "macos", test))]
fn check_identity(expected: Identity, read: std::io::Result<Option<Identity>>) -> IdentityCheck {
    match read {
        Ok(Some(actual)) if actual == expected => IdentityCheck::Same,
        Ok(Some(_)) => IdentityCheck::Changed,
        Ok(None) => IdentityCheck::Exited,
        Err(error) => IdentityCheck::Unread {
            errno: error.raw_os_error(),
        },
    }
}

/// Keep identity checks separate from raw getter results, including errors.
#[cfg(any(target_os = "macos", test))]
fn bundle(
    expected: Identity,
    mut read_identity: impl FnMut(u32) -> std::io::Result<Option<Identity>>,
    mut get: impl FnMut(i32, u32) -> (i32, i32),
) -> Row {
    let mut row = Row {
        identity: expected,
        flags: [Flag::NotRead; 3],
        before: check_identity(expected, read_identity(expected.pid)),
        after: IdentityCheck::NotRead,
    };
    if row.before != IdentityCheck::Same {
        return row;
    }
    for (at, (selector, _)) in SELECTORS.iter().enumerate() {
        let (value, errno) = get(*selector, expected.pid);
        row.flags[at] = flag(value, errno);
    }
    row.after = check_identity(expected, read_identity(expected.pid));
    row
}

/// PID 0 is the getter's caller-relative spelling, never a kernel-task observation.
#[cfg(any(target_os = "macos", test))]
fn remove_pid_zero(records: &mut Vec<crate::kproc::Record>) -> usize {
    let before = records.len();
    records.retain(|r| r.pid != 0);
    before - records.len()
}

/// Collect using this process's real credentials. The caller controls only cancellation.
pub fn collect(mut cancelled: impl FnMut() -> bool) -> Capture {
    let began = Instant::now();
    let pid = std::process::id();
    let mut capture = Capture {
        collector: crate::kproc::record(pid)
            .as_ref()
            .map(identity)
            .unwrap_or(Identity {
                pid,
                start_s: 0,
                start_us: 0,
            }),
        parent_pid: 0,
        effective_uid: u32::MAX,
        began_unix_ms: wall_ms(),
        ended_unix_ms: 0,
        elapsed_ms: 0,
        elapsed_ns: 0,
        excluded_pid_zero: 0,
        initial_processes: 0,
        truncated: false,
        cancelled: false,
        timed_out: false,
        error: None,
        rows: Vec::new(),
    };
    #[cfg(target_os = "macos")]
    {
        // SAFETY: these getters take no pointers and modify no process policy.
        capture.effective_uid = unsafe { libc::geteuid() };
        capture.parent_pid = unsafe { libc::getppid() }.cast_unsigned();
        if capture.effective_uid != 0 {
            capture.error = Some("permission_denied:root_collection_required".into());
        } else if let Some(mut records) = crate::kproc::records() {
            capture.excluded_pid_zero = remove_pid_zero(&mut records);
            records.sort_by_key(|r| r.pid);
            capture.initial_processes = records.len();
            capture.truncated = records.len() > MAX_PIDS;
            for r in records.into_iter().take(MAX_PIDS) {
                if cancelled() {
                    capture.cancelled = true;
                    break;
                }
                if began.elapsed() >= COLLECT_FOR {
                    capture.timed_out = true;
                    break;
                }
                capture.rows.push(bundle(
                    identity(&r),
                    |pid| crate::kproc::record_read(pid).map(|r| r.as_ref().map(identity)),
                    |selector, pid| {
                        // SAFETY: errno is thread-local. These selectors only read policy on a PID.
                        unsafe {
                            *libc::__error() = 0;
                            let result = libc::getpriority(selector, pid);
                            (result, *libc::__error())
                        }
                    },
                ));
            }
        } else {
            capture.error = Some("process_census_unavailable".into());
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = &mut cancelled;
        capture.error = Some("unsupported_platform".into());
    }
    capture.ended_unix_ms = wall_ms();
    let elapsed = began.elapsed();
    capture.elapsed_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
    capture.elapsed_ns = u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX);
    capture.timed_out |= elapsed > COLLECT_FOR;
    capture
}

/// The collector watchdog cannot wait on buffered I/O or process-exit hooks.
fn watchdog_exit(code: i32) -> ! {
    #[cfg(target_os = "macos")]
    // SAFETY: this terminates only the fixed collector process. The kernel closes
    // its owned descriptors; it has no subprocesses, output files or policy changes.
    unsafe {
        libc::_exit(code)
    }
    #[cfg(not(target_os = "macos"))]
    std::process::exit(code)
}

/// Validate fixed dispatch before any GUI or per-user state is initialized.
pub fn collector_entry(args: &[std::ffi::OsString]) -> u8 {
    use std::io::{Read, Write};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    if !args.is_empty() {
        eprintln!("CPU policy collection takes no operands");
        return 2;
    }
    #[cfg(target_os = "macos")]
    let parent = unsafe { libc::getppid() };
    let role_started = Instant::now();
    let closed = Arc::new(AtomicBool::new(false));
    // This watchdog covers blocking sysctl/getpriority calls and pipe writes too.
    // Only this fixed collector role owns it; ordinary sysprobe readers get no timer.
    let watchdog_closed = Arc::clone(&closed);
    if std::thread::Builder::new()
        .name("cpu-policy-deadline".into())
        .spawn(move || {
            loop {
                #[cfg(target_os = "macos")]
                let parent_gone = unsafe { libc::getppid() } != parent;
                #[cfg(not(target_os = "macos"))]
                let parent_gone = false;
                if watchdog_closed.load(Ordering::Acquire) || parent_gone {
                    watchdog_exit(PARENT_LOSS_EXIT);
                }
                if role_started.elapsed() >= COLLECT_FOR + Duration::from_secs(5) {
                    watchdog_exit(DEADLINE_EXIT);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        })
        .is_err()
    {
        return 2;
    }
    let me = match crate::kproc::record(std::process::id()) {
        Some(me) => me,
        None => return 2,
    };
    #[cfg(target_os = "macos")]
    let effective_uid = unsafe { libc::geteuid() };
    #[cfg(not(target_os = "macos"))]
    let effective_uid = u32::MAX;
    let hello = json!({"schema": 1, "phase": "collector_started", "pid": me.pid,
        "start_s": me.start_s.to_string(), "start_us": me.start_us, "effective_uid": effective_uid,
        "parent_pid": me.ppid});
    let Ok(hello) = aterm_json::to_string(&hello) else {
        return 2;
    };
    let mut output = std::io::stdout().lock();
    if writeln!(output, "{hello}")
        .and_then(|()| output.flush())
        .is_err()
    {
        return 1;
    }
    drop(output);
    let reader_closed = Arc::clone(&closed);
    let (ack_tx, ack_rx) = std::sync::mpsc::sync_channel(1);
    let monitor = std::thread::Builder::new()
        .name("cpu-policy-parent".into())
        .spawn(move || {
            let mut byte = [0];
            let ack = std::io::stdin().read_exact(&mut byte).is_ok() && byte[0] == 1;
            let _ = ack_tx.send(ack);
            if ack {
                let _ = std::io::stdin().read(&mut byte);
            }
            reader_closed.store(true, Ordering::Release);
        });
    if monitor.is_err() {
        return 2;
    }
    // No privileged observation until our parent has bound this exact generation.
    // A dead/unresponsive parent cannot leave this helper waiting indefinitely.
    if ack_rx.recv_timeout(Duration::from_secs(5)) != Ok(true) {
        return 2;
    }
    let capture = collect(|| {
        #[cfg(target_os = "macos")]
        if unsafe { libc::getppid() } != parent {
            return true;
        }
        closed.load(Ordering::Acquire)
    });
    let result = aterm_json::to_string(&capture.json())
        .ok()
        .and_then(|text| {
            let mut output = std::io::stdout().lock();
            writeln!(output, "{text}")
                .and_then(|()| output.flush())
                .ok()
        });
    if result.is_none() {
        1
    } else if capture.complete() {
        0
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn id() -> Identity {
        Identity {
            pid: 7,
            start_s: 100,
            start_us: 5,
        }
    }

    fn capture() -> Capture {
        Capture {
            collector: id(),
            parent_pid: 6,
            effective_uid: 0,
            began_unix_ms: 1000,
            ended_unix_ms: 1001,
            elapsed_ms: 1,
            elapsed_ns: 1_000_000,
            excluded_pid_zero: 1,
            initial_processes: 1,
            truncated: false,
            cancelled: false,
            timed_out: false,
            error: None,
            rows: vec![Row {
                identity: id(),
                before: IdentityCheck::Same,
                after: IdentityCheck::Same,
                flags: [Flag::Value(false); 3],
            }],
        }
    }

    #[test]
    fn cpu_policy_codec_binds_actual_rows_counts_and_error_values() {
        let good = capture();
        let encoded = good.json();
        assert_eq!(decode_capture(&encoded), Some(good.clone()));
        for field in [
            "identity_before",
            "identity_after",
            "game_mode",
            "carplay",
            "runaway_mitigation",
            "start_s",
        ] {
            let mut bad = encoded.clone();
            let rows = bad.as_object_mut().unwrap().get_mut("rows").unwrap();
            let _ = rows.as_array_mut().unwrap()[0]
                .as_object_mut()
                .unwrap()
                .remove(field);
            assert!(decode_capture(&bad).is_none(), "missing {field}");
        }
        let mut bad = encoded.clone();
        bad.as_object_mut()
            .unwrap()
            .get_mut("rows")
            .unwrap()
            .as_array_mut()
            .unwrap()[0] = json!({});
        assert!(decode_capture(&bad).is_none());
        let mut bad = encoded.clone();
        bad.as_object_mut()
            .unwrap()
            .get_mut("rows")
            .unwrap()
            .as_array_mut()
            .unwrap()
            .push(encoded["rows"][0].clone());
        for key in [
            "checked_processes",
            "attempted_processes",
            "initial_processes",
        ] {
            bad.as_object_mut().unwrap().insert(key.into(), json!(2));
        }
        assert!(decode_capture(&bad).is_none(), "duplicate PID");
        let mut partial = good.clone();
        partial.rows[0].flags[0] = Flag::Error {
            value: -1,
            errno: 13,
        };
        partial.rows[0].after = IdentityCheck::Unread { errno: Some(5) };
        assert_eq!(decode_capture(&partial.json()), Some(partial.clone()));
        assert!(!partial.complete());
        let mut forged = partial.json();
        forged
            .as_object_mut()
            .unwrap()
            .insert("complete_initial_census".into(), json!(true));
        assert!(decode_capture(&forged).is_none());
        let mut forged = partial.json();
        forged
            .as_object_mut()
            .unwrap()
            .insert("getter_errors".into(), json!(0));
        assert!(decode_capture(&forged).is_none());
        let mut unprivileged = good;
        unprivileged.effective_uid = 501;
        assert!(!decode_capture(&unprivileged.json()).unwrap().complete());
    }

    #[test]
    fn unexpected_and_denied_getters_never_become_off() {
        assert_eq!(
            flag(-1, 1),
            Flag::Error {
                value: -1,
                errno: 1
            }
        );
        assert_eq!(flag(-1, 0), Flag::Unexpected(-1));
        assert_eq!(flag(2, 0), Flag::Unexpected(2));
        assert_eq!(flag(0, 0), Flag::Value(false));
        assert_eq!(flag(1, 0), Flag::Value(true));
    }

    #[test]
    fn pid_reuse_exit_and_unread_identity_are_distinct_and_keep_raw_getters() {
        let mut n = 0;
        let row = bundle(
            id(),
            |_| {
                n += 1;
                Ok((n == 1).then_some(id()))
            },
            |_, _| (-1, 13),
        );
        assert_eq!(row.after, IdentityCheck::Exited);
        assert!(row.flags.iter().all(|f| *f
            == Flag::Error {
                value: -1,
                errno: 13
            }));
        let mut n = 0;
        let row = bundle(
            id(),
            |_| {
                n += 1;
                Ok(Some(Identity {
                    start_s: if n == 1 { 100 } else { 101 },
                    ..id()
                }))
            },
            |_, _| (0, 0),
        );
        assert_eq!(row.after, IdentityCheck::Changed);
        let row = bundle(
            id(),
            |_| Err(std::io::Error::from_raw_os_error(13)),
            |_, _| panic!("unread identity cannot run getters"),
        );
        assert_eq!(row.before, IdentityCheck::Unread { errno: Some(13) });
        assert_eq!(row.flags, [Flag::NotRead; 3]);
    }

    #[test]
    fn cpu_policy_census_never_passes_pid_zero_to_a_getter() {
        let make = |pid| crate::kproc::Record {
            pid,
            ppid: 0,
            pgid: 0,
            tpgid: 0,
            tty: None,
            uid: 0,
            comm: String::new(),
            start_s: 100,
            start_us: 0,
            zombie: false,
        };
        let mut records = vec![make(0), make(1), make(42)];
        assert_eq!(remove_pid_zero(&mut records), 1);
        let mut got = Vec::new();
        for r in records {
            let expected = identity(&r);
            bundle(
                expected,
                |_| Ok(Some(expected)),
                |_, pid| {
                    got.push(pid);
                    (0, 0)
                },
            );
        }
        assert_eq!(got, [1, 1, 1, 42, 42, 42]);
    }

    #[test]
    fn nonroot_and_partial_censuses_cannot_claim_complete() {
        let mut capture = Capture {
            collector: id(),
            parent_pid: 1,
            effective_uid: 0,
            began_unix_ms: 1,
            ended_unix_ms: 2,
            elapsed_ms: 1,
            elapsed_ns: 1_000_000,
            excluded_pid_zero: 0,
            initial_processes: 1,
            truncated: false,
            cancelled: false,
            timed_out: false,
            error: None,
            rows: vec![Row {
                identity: id(),
                flags: [Flag::Value(false), Flag::Value(false), Flag::Value(false)],
                before: IdentityCheck::Same,
                after: IdentityCheck::Same,
            }],
        };
        assert!(capture.complete());
        capture.effective_uid = 501;
        assert!(!capture.complete());
        capture.effective_uid = 0;
        capture.truncated = true;
        assert!(!capture.complete());
        capture.truncated = false;
        capture.rows[0].flags[1] = Flag::Error {
            value: -1,
            errno: 1,
        };
        assert!(!capture.complete());
        capture.rows[0].flags[1] = Flag::Value(false);
        capture.cancelled = true;
        assert!(!capture.complete());
        capture.cancelled = false;
        capture.timed_out = true;
        assert!(!capture.complete());
        capture.timed_out = false;
        capture.elapsed_ns = COLLECT_FOR.as_nanos() as u64 + 1;
        assert!(!capture.complete());
    }
}
