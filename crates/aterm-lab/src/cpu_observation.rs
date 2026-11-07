// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared identity-bound CPU observations; neither recording nor idle mode awards a verdict.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use aterm_json::{Value, json};
use aterm_uds::peer::{AuditToken, peer_audit_token};
use aterm_uds::process::{Identity, read_identity, read_process_path};

use crate::band::Band;
use crate::sys::{ThreadCpu, ThreadCpuObservation};
use crate::{Fail, Res};

const USAGE: &str = "recording-cpu --pid PID --aterm PATH --sock PATH --output PATH --duration-ms N --interval-ms N";
const METRICS: &[&str] = &[
    "writer_in_flight",
    "writer_queued_bytes",
    "writer_urgent_pressure",
    "writer_urgent_drain",
    "writer_urgent_view",
    "pool_used",
    "live_events",
    "keyframes",
    "witnessed",
    "diverged",
    "pending",
    "unjudged_evicted",
    "dropped_input",
    "dropped_events",
    "dropped_replies",
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Mode {
    #[default]
    Recording,
    Idle {
        settle: Duration,
        band: Band,
    },
}

pub(crate) struct Args {
    pub(crate) pid: i32,
    pub(crate) aterm: PathBuf,
    pub(crate) sock: PathBuf,
    pub(crate) output: PathBuf,
    pub(crate) duration: Duration,
    pub(crate) interval: Duration,
    pub(crate) mode: Mode,
}

fn parse(args: &[String]) -> Result<Args, Fail> {
    let usage = || Fail::usage(USAGE);
    let mut values = BTreeMap::new();
    let (words, remainder) = args.as_chunks::<2>();
    for pair in words {
        if ![
            "--pid",
            "--aterm",
            "--sock",
            "--output",
            "--duration-ms",
            "--interval-ms",
        ]
        .contains(&pair[0].as_str())
            || values.insert(pair[0].as_str(), pair[1].as_str()).is_some()
        {
            return Err(usage());
        }
    }
    if !remainder.is_empty() || values.len() != 6 {
        return Err(usage());
    }
    let pid = values["--pid"].parse::<i32>().map_err(|_| usage())?;
    let duration = values["--duration-ms"]
        .parse::<u64>()
        .map_err(|_| usage())?;
    let interval = values["--interval-ms"]
        .parse::<u64>()
        .map_err(|_| usage())?;
    if pid <= 1
        || interval == 0
        || duration == 0
        || interval > duration
        || Instant::now()
            .checked_add(Duration::from_millis(duration))
            .is_none()
    {
        return Err(usage());
    }
    Ok(Args {
        pid,
        aterm: std::fs::canonicalize(values["--aterm"]).map_err(|e| e.to_string())?,
        sock: PathBuf::from(values["--sock"]),
        output: PathBuf::from(values["--output"]),
        duration: Duration::from_millis(duration),
        interval: Duration::from_millis(interval),
        mode: Mode::Recording,
    })
}

#[derive(Default)]
struct Metric {
    first: Option<u64>,
    last: Option<u64>,
    max_observed: Option<u64>,
    missing: usize,
    reset: bool,
}

impl Metric {
    fn observe(&mut self, value: Option<u64>, initial: bool) {
        if initial {
            self.first = value;
        }
        if let Some(value) = value {
            // Missing reads cannot erase an observed decrease across the gap.
            self.reset |= self.max_observed.is_some_and(|last| value < last);
            self.max_observed = Some(self.max_observed.map_or(value, |old| old.max(value)));
        } else {
            self.missing += 1;
        }
        self.last = value;
    }

    fn delta(&self) -> Option<u64> {
        if self.missing != 0 || self.reset {
            return None;
        }
        self.last?.checked_sub(self.first?)
    }
}

struct Thread {
    name: String,
    cpu: Metric,
}

#[derive(Default)]
struct Threads(BTreeMap<u64, Thread>, Mode);

fn named(thread: &ThreadCpu, mode: Mode) -> bool {
    matches!(mode, Mode::Idle { .. })
        || thread.name.starts_with("aterm-pty-reader-")
        || thread.name == "aterm-temporal-writer"
}

impl Threads {
    fn new(mode: Mode) -> Self {
        Self(BTreeMap::new(), mode)
    }

    fn observe_snapshot(&mut self, snapshot: Option<&ThreadCpuObservation>, initial: bool) -> bool {
        let reading = snapshot
            .filter(|s| s.complete())
            .map(|s| s.threads.as_slice());
        self.observe(reading, initial);
        reading.is_some()
    }

    fn observe(&mut self, reading: Option<&[ThreadCpu]>, initial: bool) {
        for (tid, thread) in &mut self.0 {
            let now = reading.and_then(|items| {
                items
                    .iter()
                    .find(|item| item.tid == *tid && item.name == thread.name)
            });
            thread.cpu.observe(now.map(|item| item.ns), initial);
        }
        if let Some(reading) = reading {
            for item in reading.iter().filter(|item| named(item, self.1)) {
                self.0.entry(item.tid).or_insert_with(|| {
                    let mut cpu = Metric::default();
                    cpu.observe(Some(item.ns), initial);
                    Thread {
                        name: item.name.clone(),
                        cpu,
                    }
                });
            }
        }
    }

    fn report(&self) -> Vec<Value> {
        self.0
            .iter()
            .map(|(tid, thread)| {
                json!({
                    "tid": tid, "name": thread.name,
                    "first_cpu_ns": thread.cpu.first, "last_cpu_ns": thread.cpu.last,
                    "cpu_delta_ns": thread.cpu.delta(), "missing_observations": thread.cpu.missing,
                    "counter_reset": thread.cpu.reset,
                })
            })
            .collect()
    }
}

fn write_record(file: &mut std::fs::File, value: &Value) -> Result<(), Fail> {
    writeln!(file, "{value}").map_err(|e| Fail::die(e.to_string()))?;
    file.flush().map_err(|e| Fail::die(e.to_string()))
}

fn peer_matches(identity: Identity, peer: Option<AuditToken>) -> bool {
    peer.is_some_and(|peer| {
        i32::try_from(peer.pid()).ok() == Some(identity.pid)
            && i32::try_from(peer.pid_version()).ok() == identity.idversion
    }) && identity.idversion.is_some()
}

fn connect(
    args: &Args,
    identity: Identity,
) -> Result<aterm_agent::RelayClient<aterm_uds::CtlStream>, Fail> {
    let socket = args
        .sock
        .to_str()
        .ok_or("CPU observation: non-UTF8 socket")?;
    let (mut stream, token) =
        aterm_ctl::connect_then_read_token(socket).map_err(|e| e.to_string())?;
    if !peer_matches(identity, peer_audit_token(&stream))
        || read_identity(args.pid) != Some(identity)
    {
        return Err(Fail::die(
            "CPU observation: control peer does not match subject PID and exec version",
        ));
    }
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    // The existing bounded token reader authenticates this already peer-pinned connection.
    // Never include the token or the auth line in diagnostic output.
    writeln!(stream, "AUTH {}", token.map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    Ok(aterm_agent::RelayClient::new(stream).bounded(Duration::from_secs(2)))
}

fn boundary(
    args: &Args,
    identity: Identity,
    control: &mut aterm_agent::RelayClient<aterm_uds::CtlStream>,
    file: &mut std::fs::File,
    boundary: &str,
) -> Result<(), Fail> {
    let before = read_identity(args.pid) == Some(identity)
        && read_process_path(args.pid).as_ref() == Some(&args.aterm);
    let reply = if before {
        control.request_line("metrics").map_err(|e| e.to_string())
    } else {
        Err("subject identity changed or unavailable".to_owned())
    };
    let valid = before
        && read_identity(args.pid) == Some(identity)
        && read_process_path(args.pid).as_ref() == Some(&args.aterm);
    write_record(
        file,
        &json!({
            "kind": "metrics_boundary", "boundary": boundary, "identity_valid": valid,
            "reply": reply.as_ref().ok(), "error": reply.as_ref().err(),
        }),
    )?;
    if !valid || !reply.is_ok_and(|line| line.starts_with("OK ")) {
        return Err(Fail::die(
            "CPU observation: metrics boundary unavailable or identity changed",
        ));
    }
    Ok(())
}

pub fn cmd(args: &[String]) -> Res {
    crate::stop::install();
    observe(parse(args)?)
}

pub(crate) fn observe(args: Args) -> Res {
    let identity =
        read_identity(args.pid).ok_or("CPU observation: process identity unavailable")?;
    if read_process_path(args.pid).as_ref() != Some(&args.aterm) {
        return Err(Fail::die(
            "CPU observation: subject executable differs from --aterm",
        ));
    }
    let mut control = connect(&args, identity)?;
    let mut file =
        crate::util::create_new_regular_file(&args.output, 0o600).map_err(|e| e.to_string())?;
    write_record(
        &mut file,
        &json!({
            "kind": "scope", "schema": 1, "identity": identity.token(),
            "mode": if args.mode == Mode::Recording { "recording" } else { "idle" },
            "idle_setup": match args.mode {
                Mode::Recording => None,
                Mode::Idle { settle, band } => Some(json!({
                    "settle_ms": settle.as_millis(), "band": band.name(),
                    "headless": true, "program": ["/bin/cat"],
                    "configuration": "standard private lab; update/package/harness automation off",
                })),
            },
            "image": args.aterm, "socket": args.sock,
            "duration_ms": args.duration.as_millis(), "interval_ms": args.interval.as_millis(),
            "cadence": "post-read/query sleep; actual observation timestamps retained, not fixed rate",
            "control_binding": "one persistent authenticated connection, kernel audit PID and exec version match subject identity",
            "scope": if args.mode == Mode::Recording { "external wall interval includes setup, idle, control polling and completion; not drain-only or stage CPU" } else { "CPU deltas span observed boundaries; metrics queries outside, no video or periodic status query" },
            "threads": if args.mode == Mode::Recording { "named live reader/writer TIDs only; absent/exited-before-first-sample threads unknown, not zero" } else { "all observed live TIDs; late, missing, renamed or reset counters have no exact full-window delta" },
            "status_scope": if args.mode == Mode::Recording { "default pane of an isolated one-pane subject, not all panes' work" } else { "metrics boundaries only; no periodic status query" },
            "host_state": "not measured by this command; no quiet-host guarantee",
            "coverage": "incomplete snapshots invalidate exact thread deltas; partial raw counters retained; unobserved lifetimes remain unknown",
            "queue": "maximum observed gauge is a sampled lower bound, never a continuous high-water mark",
            "verdict": "diagnostic only; no acceptance thresholds or verdicts",
        }),
    )?;
    let started = Instant::now();
    if matches!(args.mode, Mode::Idle { .. }) {
        boundary(&args, identity, &mut control, &mut file, "before")?;
    }
    let mut threads = Threads::new(args.mode);
    let mut process_cpu = Metric::default();
    let fields = if args.mode == Mode::Recording {
        METRICS
    } else {
        &[]
    };
    let mut metrics: BTreeMap<&str, Metric> =
        fields.iter().map(|key| (*key, Metric::default())).collect();
    let mut count = 0;
    let mut incomplete_thread_observations = 0;
    loop {
        crate::stop::check()?;
        let before = Instant::now();
        let same_before = read_identity(args.pid) == Some(identity)
            && read_process_path(args.pid).as_ref() == Some(&args.aterm);
        let snapshot = same_before
            .then(|| crate::sys::thread_cpu_observation(args.pid))
            .flatten();
        let total_cpu = same_before
            .then(|| crate::sys::process_cpu_ns(args.pid))
            .flatten();
        let cpu_read_end_us = started.elapsed().as_micros();
        let response = if same_before && args.mode == Mode::Recording {
            Some(
                control
                    .request_line("temporal status")
                    .map_err(|e| e.to_string()),
            )
        } else {
            None
        };
        let same_after = read_identity(args.pid) == Some(identity)
            && read_process_path(args.pid).as_ref() == Some(&args.aterm);
        let valid = same_before && same_after;
        let snapshot = valid.then_some(snapshot).flatten();
        let status = valid
            .then(|| response.as_ref().and_then(|r| r.as_ref().ok()))
            .flatten()
            .filter(|reply| reply.starts_with("OK "))
            .cloned();
        let control_error = if !same_before {
            Some("subject identity changed or unavailable".to_owned())
        } else {
            response.as_ref().and_then(|r| r.as_ref().err()).cloned()
        };
        process_cpu.observe(valid.then_some(total_cpu).flatten(), count == 0);
        let complete = threads.observe_snapshot(snapshot.as_ref(), count == 0);
        incomplete_thread_observations += usize::from(!complete);
        for (key, metric) in &mut metrics {
            let value = status
                .as_deref()
                .and_then(|status| crate::screen::field(status, key))
                .and_then(|text| text.parse().ok());
            metric.observe(value, count == 0);
        }
        let raw: Option<Vec<Value>> = snapshot.as_ref().map(|r| {
            r.threads
                .iter()
                .filter(|item| named(item, args.mode))
                .map(|item| json!({"tid": item.tid, "name": item.name, "cpu_ns": item.ns}))
                .collect()
        });
        write_record(
            &mut file,
            &json!({
                "kind": "observation", "index": count, "start_us": before.duration_since(started).as_micros(),
                "end_us": started.elapsed().as_micros(), "identity_valid": valid,
                "cpu_read_end_us": cpu_read_end_us,
                "process_cpu_ns": valid.then_some(total_cpu).flatten(),
                "threads": raw, "status": status,
                "thread_snapshot_complete": snapshot.as_ref().is_some_and(|r| r.complete()),
                "thread_list_complete": snapshot.as_ref().map(|r| r.list_complete),
                "unreadable_tids": snapshot.as_ref().map(|r| &r.missing_tids),
                "control_reply": response.as_ref().and_then(|r| r.as_ref().ok()), "control_error": control_error,
            }),
        )?;
        count += 1;
        if count == 1 {
            let mut stdout = std::io::stdout().lock();
            writeln!(stdout, "READY {}", identity.token()).map_err(|e| e.to_string())?;
            stdout.flush().map_err(|e| e.to_string())?;
        }
        if started.elapsed() >= args.duration || !valid {
            break;
        }
        crate::stop::sleep(
            args.interval
                .min(args.duration.saturating_sub(started.elapsed())),
        )?;
    }
    let elapsed_us = started.elapsed().as_micros();
    if matches!(args.mode, Mode::Idle { .. }) {
        boundary(&args, identity, &mut control, &mut file, "after")?;
    }
    let metrics: Vec<Value> = metrics
        .iter()
        .map(|(key, metric)| {
            let counter = matches!(
                *key,
                "witnessed"
                    | "diverged"
                    | "unjudged_evicted"
                    | "dropped_input"
                    | "dropped_events"
                    | "dropped_replies"
            );
            json!({"field": key, "kind": if counter {"counter"} else {"gauge"},
            "first": metric.first, "last": metric.last, "max_observed": metric.max_observed,
            "delta": if counter {metric.delta()} else {None},
            "missing_observations": metric.missing, "decreased": metric.reset})
        })
        .collect();
    write_record(
        &mut file,
        &json!({
            "kind": "summary", "identity": identity.token(), "elapsed_us": elapsed_us,
            "observations": count, "threads": threads.report(), "status_metrics": metrics,
            "process_cpu_delta_ns": process_cpu.delta(),
            "process_cpu_missing_observations": process_cpu.missing,
            "process_cpu_counter_reset": process_cpu.reset,
            "incomplete_thread_observations": incomplete_thread_observations,
        }),
    )?;
    Ok(0)
}

#[cfg(test)]
#[path = "cpu_observation_tests.rs"]
mod tests;
