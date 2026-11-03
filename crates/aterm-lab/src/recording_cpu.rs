// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! External observations of an unchanged recording subject; never an acceptance verdict.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use aterm_json::{Value, json};
use aterm_uds::peer::{AuditToken, peer_audit_token};
use aterm_uds::process::{Identity, read_identity, read_process_path};

use crate::sys::ThreadCpu;
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
    "dropped_input",
    "dropped_events",
    "dropped_replies",
];

struct Args {
    pid: i32,
    aterm: PathBuf,
    sock: PathBuf,
    output: PathBuf,
    duration: Duration,
    interval: Duration,
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
            self.reset |= self.last.is_some_and(|last| value < last);
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
struct Threads(BTreeMap<u64, Thread>);

fn named(thread: &ThreadCpu) -> bool {
    thread.name.starts_with("aterm-pty-reader-") || thread.name == "aterm-temporal-writer"
}

impl Threads {
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
            for item in reading.iter().filter(|item| named(item)) {
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
    let socket = args.sock.to_str().ok_or("recording-cpu: non-UTF8 socket")?;
    let (mut stream, token) =
        aterm_ctl::connect_then_read_token(socket).map_err(|e| e.to_string())?;
    if !peer_matches(identity, peer_audit_token(&stream))
        || read_identity(args.pid) != Some(identity)
    {
        return Err(Fail::die(
            "recording-cpu: control peer does not match subject PID and exec version",
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

pub fn cmd(args: &[String]) -> Res {
    let args = parse(args)?;
    let identity = read_identity(args.pid).ok_or("recording-cpu: process identity unavailable")?;
    if read_process_path(args.pid).as_ref() != Some(&args.aterm) {
        return Err(Fail::die(
            "recording-cpu: subject executable differs from --aterm",
        ));
    }
    let mut control = connect(&args, identity)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&args.output)
        .map_err(|e| e.to_string())?;
    write_record(
        &mut file,
        &json!({
            "kind": "scope", "schema": 1, "identity": identity.token(),
            "image": args.aterm, "socket": args.sock,
            "duration_ms": args.duration.as_millis(), "interval_ms": args.interval.as_millis(),
            "cadence": "post-query sleep; actual observation timestamps retained, not fixed rate",
            "control_binding": "one persistent authenticated connection, kernel audit PID and exec version match subject identity",
            "scope": "external wall interval includes setup, idle, control polling and completion; not drain-only or stage CPU",
            "threads": "named live reader/writer TIDs only; absent/exited-before-first-sample threads unknown, not zero",
            "status_scope": "default pane of an isolated one-pane subject, not all panes' work",
            "queue": "maximum observed gauge is a sampled lower bound, never a continuous high-water mark",
            "verdict": "diagnostic only; no acceptance thresholds or verdicts",
        }),
    )?;
    let started = Instant::now();
    let mut threads = Threads::default();
    let mut process_cpu = Metric::default();
    let mut metrics: BTreeMap<&str, Metric> = METRICS
        .iter()
        .map(|key| (*key, Metric::default()))
        .collect();
    let mut count = 0;
    loop {
        crate::stop::check()?;
        let before = Instant::now();
        let same_before = read_identity(args.pid) == Some(identity)
            && read_process_path(args.pid).as_ref() == Some(&args.aterm);
        let reading = same_before
            .then(|| crate::sys::thread_cpu_ns(args.pid))
            .flatten();
        let total_cpu = same_before
            .then(|| crate::sys::process_cpu_ns(args.pid))
            .flatten();
        let cpu_read_end_us = started.elapsed().as_micros();
        let response = if same_before {
            control
                .request_line("temporal status")
                .map_err(|e| e.to_string())
        } else {
            Err("subject identity changed or unavailable".into())
        };
        let same_after = read_identity(args.pid) == Some(identity)
            && read_process_path(args.pid).as_ref() == Some(&args.aterm);
        let valid = same_before && same_after;
        let reading = valid.then_some(reading).flatten();
        let status = valid
            .then_some(response.as_ref().ok())
            .flatten()
            .filter(|reply| reply.starts_with("OK "))
            .cloned();
        process_cpu.observe(valid.then_some(total_cpu).flatten(), count == 0);
        threads.observe(reading.as_deref(), count == 0);
        for (key, metric) in &mut metrics {
            let value = status
                .as_deref()
                .and_then(|status| crate::screen::field(status, key))
                .and_then(|text| text.parse().ok());
            metric.observe(value, count == 0);
        }
        let raw: Option<Vec<Value>> = reading.as_ref().map(|items| {
            items
                .iter()
                .filter(|item| named(item))
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
                "control_reply": response.as_ref().ok(), "control_error": response.as_ref().err(),
            }),
        )?;
        count += 1;
        if count == 1 {
            println!("READY {}", identity.token());
            std::io::stdout().flush().map_err(|e| e.to_string())?;
        }
        if started.elapsed() >= args.duration || !valid {
            break;
        }
        std::thread::sleep(
            args.interval
                .min(args.duration.saturating_sub(started.elapsed())),
        );
    }
    let metrics: Vec<Value> = metrics
        .iter()
        .map(|(key, metric)| {
            let counter = matches!(
                *key,
                "witnessed" | "diverged" | "dropped_input" | "dropped_events" | "dropped_replies"
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
            "kind": "summary", "identity": identity.token(), "elapsed_us": started.elapsed().as_micros(),
            "observations": count, "threads": threads.report(), "status_metrics": metrics,
            "process_cpu_delta_ns": process_cpu.delta(),
            "process_cpu_missing_observations": process_cpu.missing,
            "process_cpu_counter_reset": process_cpu.reset,
        }),
    )?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_peer_requires_exact_pid_and_exec_version() {
        let identity = Identity {
            pid: 14,
            birth: aterm_uds::process::Birth(100),
            idversion: Some(3),
        };
        let token = |pid, version| {
            let mut words = [0; 8];
            words[5] = pid;
            words[7] = version;
            Some(AuditToken(words))
        };
        assert!(peer_matches(identity, token(14, 3)));
        assert!(!peer_matches(identity, token(15, 3)));
        assert!(!peer_matches(identity, token(14, 4)));
        assert!(!peer_matches(identity, None));
        assert!(!peer_matches(
            Identity {
                idversion: None,
                ..identity
            },
            token(14, 3)
        ));
    }

    #[test]
    fn cpu_requires_both_boundaries_and_monotone_uninterrupted_identity() {
        let mut threads = Threads::default();
        let sample = |ns| {
            vec![ThreadCpu {
                tid: 8,
                name: "aterm-temporal-writer".into(),
                ns,
            }]
        };
        threads.observe(Some(&sample(10)), true);
        threads.observe(Some(&sample(15)), false);
        assert_eq!(threads.0[&8].cpu.delta(), Some(5));
        threads.observe(None, false);
        threads.observe(Some(&sample(20)), false);
        assert_eq!(threads.0[&8].cpu.delta(), None);
        let mut late = Threads::default();
        late.observe(Some(&[]), true);
        late.observe(Some(&sample(20)), false);
        assert_eq!(late.0[&8].cpu.delta(), None);
        let mut reset = Metric::default();
        reset.observe(Some(20), true);
        reset.observe(Some(19), false);
        assert_eq!(reset.delta(), None);
    }

    #[test]
    fn missing_gauges_do_not_invent_zero_or_erase_observed_lower_bound() {
        let mut metric = Metric::default();
        metric.observe(None, true);
        assert_eq!(metric.max_observed, None);
        metric.observe(Some(12), false);
        metric.observe(Some(0), false);
        metric.observe(None, false);
        assert_eq!(metric.max_observed, Some(12));
        assert_eq!(metric.last, None);
        assert_eq!(metric.missing, 2);
        assert_eq!(metric.delta(), None);
    }

    #[test]
    fn exited_or_renamed_thread_never_looks_like_completed_zero_work() {
        let mut threads = Threads::default();
        threads.observe(
            Some(&[ThreadCpu {
                tid: 4,
                name: "aterm-pty-reader-0".into(),
                ns: 4,
            }]),
            true,
        );
        threads.observe(
            Some(&[ThreadCpu {
                tid: 4,
                name: "other".into(),
                ns: 9,
            }]),
            false,
        );
        assert_eq!(threads.0[&4].cpu.delta(), None);
        assert_eq!(threads.0[&4].cpu.last, None);
    }
}
