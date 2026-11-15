// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Resource observations from original native children. The private enrolled GUI lane reads
//! these facts; none of them grants execution, resource custody or authority over a PID.

#[cfg(test)]
#[path = "resource_observation_wire_tests.rs"]
mod wire_tests;

use aterm_types::resource_admission::{AccountingObservation, Binding};
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Mutex, MutexGuard, OnceLock};

/// A kernel process incarnation, using the process sweep's start unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessKey {
    /// Native PID.
    pub pid: u32,
    /// Native start counter.
    pub start: u64,
}

/// An original direct child of the native custodian and its authenticated submitting session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Original {
    /// Session authenticated by the GUI on submission.
    pub session: String,
    /// Original process incarnation.
    pub process: ProcessKey,
    /// Its own CPU at the observation, nanoseconds; absent means unread.
    pub cpu_ns: Option<u64>,
    /// Its already reaped descendants' CPU at the observation, nanoseconds.
    pub child_cpu_ns: Option<u64>,
    /// Kernel auto-reap policy read with these counters.
    pub reaps_unwaited: Option<bool>,
    /// Physical footprint from this native observation; absent means unread.
    pub footprint: Option<u64>,
    /// Largest physical footprint from this native observation.
    pub peak_footprint: Option<u64>,
}

/// Completed original children, retained as counters across GUI handoffs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionUsage {
    /// Authenticated submitting session.
    pub session: String,
    /// Child-inclusive kernel counter charge from completed original waits, nanoseconds.
    /// Auto-reaped descendants can be charged repeatedly; this is not an actual-CPU floor.
    pub reaped_cpu_ns: u64,
    /// Original owning waits whose CPU or process generation was unavailable, cumulatively.
    pub unread: u64,
}

/// A final observation taken from the original unreaped child before its owning wait.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Terminal {
    /// Increasing event cursor within this host incarnation.
    pub seq: u64,
    /// Authenticated submitting session.
    pub session: String,
    /// Original process incarnation, even after that PID has been reused.
    pub process: ProcessKey,
    /// Its actual OS user identity at the final read; absent means unread.
    pub uid: Option<u32>,
    /// Resource coalition at the final read, when the kernel still exposes it.
    pub coalition: Option<u64>,
    /// Exec generation accompanying this final process read, when available.
    pub version: Option<i32>,
    /// Final own CPU, nanoseconds, from the still-unreaped process.
    pub cpu_ns: Option<u64>,
    /// Final waited-descendant CPU, nanoseconds, from the still-unreaped process.
    pub child_cpu_ns: Option<u64>,
    /// Combined CPU from the original owning wait, nanoseconds; not an invented split.
    pub wait_cpu_ns: Option<u64>,
    /// Whether this original child let the kernel reap its children without an explicit wait.
    /// macOS can report such descendant CPU twice; the consumer must retain this evidence.
    pub reaps_unwaited: Option<bool>,
    /// Wall clock of the owning wait, milliseconds.
    pub at_unix_ms: u64,
    /// Cross-process monotonic cut after original exit confirmation and before its owning wait.
    pub exit_cut_ns: Option<u64>,
}

/// One observation made while no original child can be reaped or transferred to the completed
/// counters. The host child counter and the session reaped counters therefore share a boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observation {
    /// Original custodian incarnation. A new incarnation starts new counters.
    pub host: ProcessKey,
    /// Its raw reaped-children CPU at that boundary, nanoseconds.
    pub host_child_cpu_ns: Option<u64>,
    /// Wall clock at the start of the process read, milliseconds.
    pub at_unix_ms: u64,
    /// Every original child still awaiting its original wait.
    pub originals: Vec<Original>,
    /// Completed counters, including children born and reaped between GUI sweeps.
    pub sessions: Vec<SessionUsage>,
    /// Queued or retained native work, read from the actual execution owner.
    pub outstanding_sessions: Vec<String>,
    /// Oldest event still available; a gap from the requested cursor is explicit lost coverage.
    pub first_seq: u64,
    /// Cursor through the newest captured original wait.
    pub next_seq: u64,
    /// Captured waits newer than the requested cursor.
    pub terminals: Vec<Terminal>,
    /// Active originals whose process generation has not yet been read.
    pub unproven_active: u64,
    /// Cumulative original waits whose process generation could never be recovered.
    pub lost_origins: u64,
}

#[derive(Default)]
pub(crate) struct Ledger {
    channel_pending: BTreeMap<(String, [u8; 16]), u32>,
    owner_sessions: std::collections::BTreeSet<String>,
    originals: BTreeMap<u32, (ProcessKey, String)>,
    unproven: BTreeMap<u32, String>,
    lost_origins: u64,
    completed: BTreeMap<String, (u64, u64)>,
    next_seq: u64,
    terminals: VecDeque<Terminal>,
}

pub(crate) fn native_observation_guard() -> MutexGuard<'static, Ledger> {
    static LEDGER: OnceLock<Mutex<Ledger>> = OnceLock::new();
    let native_observation_ledger = LEDGER.get_or_init(|| Mutex::new(Ledger::default()));
    native_observation_ledger
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

/// Called only by the execution owner, with its actual original child, before its owning wait.
pub(crate) fn born(binding: &Binding, pid: u32) {
    let start = i32::try_from(pid)
        .ok()
        .and_then(aterm_uds::process::read_process_start)
        .map(|start| start.boot.0);
    native_observation_guard().born(binding, pid, start);
}

impl Ledger {
    pub(crate) fn queued(&mut self, binding: &Binding) {
        let count = self
            .channel_pending
            .entry((binding.session.clone(), binding.attempt))
            .or_default();
        *count = count.saturating_add(1);
    }

    pub(crate) fn unqueue(&mut self, binding: &Binding) {
        let key = (binding.session.clone(), binding.attempt);
        if let Some(count) = self.channel_pending.get_mut(&key) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.channel_pending.remove(&key);
            }
        }
    }

    pub(crate) fn owner_state(
        &mut self,
        processed: Option<&Binding>,
        sessions: impl IntoIterator<Item = String>,
    ) {
        self.owner_sessions = sessions.into_iter().collect();
        if let Some(binding) = processed {
            self.unqueue(binding);
        }
    }

    pub(crate) fn sessions_outstanding(&self) -> Vec<String> {
        self.owner_sessions
            .iter()
            .cloned()
            .chain(
                self.channel_pending
                    .keys()
                    .map(|(session, _)| session.clone()),
            )
            .chain(self.originals.values().map(|(_, session)| session.clone()))
            .chain(self.unproven.values().cloned())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    fn born(&mut self, binding: &Binding, pid: u32, start: Option<u64>) {
        if self.originals.get(&pid).is_some_and(|(key, session)| {
            session == &binding.session && start.is_none_or(|start| key.start == start)
        }) {
            return;
        }
        if self.originals.remove(&pid).is_some() {
            self.lost_origins = self.lost_origins.saturating_add(1);
        }
        if self
            .unproven
            .remove(&pid)
            .is_some_and(|session| session != binding.session)
        {
            self.lost_origins = self.lost_origins.saturating_add(1);
        }
        if let Some(start) = start {
            self.originals
                .insert(pid, (ProcessKey { pid, start }, binding.session.clone()));
        } else {
            self.unproven.insert(pid, binding.session.clone());
        }
    }
    fn recover(&mut self, processes: &[aterm_sysprobe::ProcessReading]) {
        // Every entry is still an original unreaped Child. The same mutex excludes the
        // only owning waits, so its PID cannot name a replacement while this read runs.
        for process in processes {
            if let Some(start) = process.start
                && let Some(session) = self.unproven.remove(&process.pid)
            {
                self.originals.insert(
                    process.pid,
                    (
                        ProcessKey {
                            pid: process.pid,
                            start,
                        },
                        session,
                    ),
                );
            }
        }
    }
    #[cfg(test)]
    pub(crate) fn next_cursor(&self) -> u64 {
        self.next_seq
    }
    #[cfg(test)]
    pub(crate) fn terminals_since(&self, since: u64) -> impl Iterator<Item = &Terminal> {
        self.terminals.iter().filter(move |t| t.seq > since)
    }
    /// Called under the same guard as the owning wait. Every original child transfers once.
    #[cfg(test)]
    pub(crate) fn reaped(
        &mut self,
        pid: u32,
        accounting: AccountingObservation,
        final_usage: Option<aterm_sysprobe::ProcessReading>,
    ) {
        self.reaped_at(pid, accounting, final_usage, None);
    }

    pub(crate) fn reaped_at(
        &mut self,
        pid: u32,
        accounting: AccountingObservation,
        final_usage: Option<aterm_sysprobe::ProcessReading>,
        exit_cut_ns: Option<u64>,
    ) {
        let known = self.originals.remove(&pid);
        let unknown = self.unproven.remove(&pid);
        let Some(session) = known
            .as_ref()
            .map(|(_, session)| session.clone())
            .or(unknown)
        else {
            return;
        };
        let counter = self.completed.entry(session.clone()).or_default();
        match accounting.cpu_times {
            Some(cpu) => {
                counter.0 = counter
                    .0
                    .saturating_add(cpu.user_ns.saturating_add(cpu.system_ns))
            }
            None => counter.1 = counter.1.saturating_add(1),
        }
        let process = known.map(|(process, _)| process).or_else(|| {
            final_usage
                .as_ref()
                .and_then(|p| p.start)
                .map(|start| ProcessKey { pid, start })
        });
        let Some(process) = process else {
            if accounting.cpu_times.is_some() {
                counter.1 = counter.1.saturating_add(1);
            }
            self.lost_origins = self.lost_origins.saturating_add(1);
            return;
        };
        let final_usage = final_usage.filter(|p| p.start == Some(process.start));
        self.next_seq = self.next_seq.saturating_add(1);
        let wait_cpu_ns = accounting
            .cpu_times
            .map(|cpu| cpu.user_ns.saturating_add(cpu.system_ns));
        self.terminals.push_back(Terminal {
            seq: self.next_seq,
            session: session.clone(),
            process,
            uid: final_usage.as_ref().and_then(|p| p.uid),
            coalition: final_usage.as_ref().and_then(|p| p.coalition),
            version: final_usage.as_ref().and_then(|p| p.version),
            cpu_ns: final_usage.as_ref().and_then(|p| p.cpu_ns),
            child_cpu_ns: final_usage.as_ref().and_then(|p| p.child_cpu_ns),
            wait_cpu_ns,
            reaps_unwaited: final_usage.as_ref().and_then(|p| p.reaps_unwaited),
            at_unix_ms: unix_ms().unwrap_or(0),
            exit_cut_ns,
        });
        while self.terminals.len() > 65536 {
            self.terminals.pop_front();
        }
    }
}

fn unix_ms() -> std::io::Result<u64> {
    u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(std::io::Error::other)?
            .as_millis(),
    )
    .map_err(std::io::Error::other)
}

pub(crate) fn snapshot(since: u64) -> std::io::Result<Observation> {
    let mut ledger = native_observation_guard();
    let pid = std::process::id();
    let at_unix_ms = unix_ms()?;
    #[cfg(target_os = "macos")]
    let sweep = aterm_sysprobe::Sweep {
        procs: owned_usage(&ledger, pid, aterm_sysprobe::procs::process_usage),
        truncated: false,
    };
    #[cfg(not(target_os = "macos"))]
    let sweep = aterm_sysprobe::processes()
        .ok_or_else(|| std::io::Error::other("native resource process counters unavailable"))?;
    ledger.recover(&sweep.procs);
    let host = sweep
        .procs
        .iter()
        .find(|p| p.pid == pid)
        .ok_or_else(|| std::io::Error::other("native host counter unavailable"))?;
    let host_key = ProcessKey {
        pid,
        start: host
            .start
            .ok_or_else(|| std::io::Error::other("native host start unavailable"))?,
    };
    let originals = ledger
        .originals
        .values()
        .map(|(key, session)| {
            let process = sweep
                .procs
                .iter()
                .find(|p| p.pid == key.pid && p.start == Some(key.start));
            Original {
                session: session.clone(),
                process: *key,
                cpu_ns: process.and_then(|p| p.cpu_ns),
                child_cpu_ns: process.and_then(|p| p.child_cpu_ns),
                reaps_unwaited: process.and_then(|p| p.reaps_unwaited),
                footprint: process.and_then(|p| p.footprint),
                peak_footprint: process.and_then(|p| p.peak_footprint),
            }
        })
        .collect();
    let sessions = ledger
        .completed
        .iter()
        .map(|(session, &(reaped_cpu_ns, unread))| SessionUsage {
            session: session.clone(),
            reaped_cpu_ns,
            unread,
        })
        .collect();
    let first_seq = ledger
        .terminals
        .front()
        .map_or(ledger.next_seq.saturating_add(1), |t| t.seq);
    let terminals = ledger
        .terminals
        .iter()
        .filter(|t| t.seq > since)
        .cloned()
        .collect();
    Ok(Observation {
        host: host_key,
        host_child_cpu_ns: host.child_cpu_ns,
        at_unix_ms,
        originals,
        sessions,
        outstanding_sessions: ledger.sessions_outstanding(),
        first_seq,
        next_seq: ledger.next_seq,
        terminals,
        unproven_active: ledger.unproven.len() as u64,
        lost_origins: ledger.lost_origins,
    })
}

#[cfg(any(target_os = "macos", test))]
fn owned_usage(
    ledger: &Ledger,
    host: u32,
    mut read: impl FnMut(u32) -> Option<aterm_sysprobe::ProcessReading>,
) -> Vec<aterm_sysprobe::ProcessReading> {
    // The GUI already sweeps the machine. Keep the reaper's exclusion short by reading only
    // this custodian and its actual unreaped originals, never another user's process table.
    let pids: std::collections::BTreeSet<_> = std::iter::once(host)
        .chain(ledger.originals.keys().copied())
        .chain(ledger.unproven.keys().copied())
        .collect();
    pids.into_iter().filter_map(&mut read).collect()
}

impl Observation {
    pub(crate) fn frames(&self) -> Vec<aterm_json::Value> {
        let mut frames = vec![
            aterm_json::json!({"version":1,"host_pid":self.host.pid,"host_start":self.host.start,
            "host_child_cpu_ns":self.host_child_cpu_ns,"at_unix_ms":self.at_unix_ms,"first_seq":self.first_seq,"next_seq":self.next_seq,
            "originals":self.originals.len(),"sessions":self.sessions.len(),"terminals":self.terminals.len(),
            "unproven_active":self.unproven_active,"lost_origins":self.lost_origins,"outstanding":self.outstanding_sessions.len()}),
        ];
        let mut rows = Vec::new();
        for r in &self.originals {
            rows.push(aterm_json::json!({"kind":"original","session":r.session,"pid":r.process.pid,"start":r.process.start,"cpu_ns":r.cpu_ns,"child_cpu_ns":r.child_cpu_ns,
            "reaps_unwaited":r.reaps_unwaited,"footprint":r.footprint,"peak_footprint":r.peak_footprint}));
        }
        for r in &self.sessions {
            rows.push(aterm_json::json!({"kind":"session","session":r.session,"reaped_cpu_ns":r.reaped_cpu_ns,"unread":r.unread}));
        }
        for session in &self.outstanding_sessions {
            rows.push(aterm_json::json!({"kind":"outstanding","session":session}));
        }
        for r in &self.terminals {
            rows.push(aterm_json::json!({"kind":"terminal","session":r.session,"pid":r.process.pid,"start":r.process.start,
            "uid":r.uid,"coalition":r.coalition,"exec_version":r.version,"cpu_ns":r.cpu_ns,"child_cpu_ns":r.child_cpu_ns,"wait_cpu_ns":r.wait_cpu_ns,"reaps_unwaited":r.reaps_unwaited,"seq":r.seq,"at_unix_ms":r.at_unix_ms,"exit_cut_ns":r.exit_cut_ns}));
        }
        frames.extend(
            rows.chunks(16)
                .map(|chunk| aterm_json::Value::Array(chunk.to_vec())),
        );
        frames.push(aterm_json::json!({"done":true}));
        frames
    }

    pub(crate) fn receive(
        since: u64,
        mut read: impl FnMut() -> std::io::Result<aterm_json::Value>,
    ) -> std::io::Result<Self> {
        let bad = || {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "invalid native resource observation",
            )
        };
        let number = |v: &aterm_json::Value| v.as_u64().ok_or_else(bad);
        let optional = |v: &aterm_json::Value| {
            if v.is_null() {
                Ok(None)
            } else {
                number(v).map(Some)
            }
        };
        let process = |v: &aterm_json::Value| {
            Ok::<_, std::io::Error>(ProcessKey {
                pid: u32::try_from(number(&v["pid"])?)
                    .ok()
                    .filter(|p| *p > 0)
                    .ok_or_else(bad)?,
                start: number(&v["start"])?,
            })
        };
        let header = read()?;
        if header["version"].as_u64() != Some(1) {
            return Err(bad());
        }
        let declared = (
            number(&header["originals"])?,
            number(&header["sessions"])?,
            number(&header["terminals"])?,
            number(&header["outstanding"])?,
        );
        let mut out = Self {
            host: ProcessKey {
                pid: u32::try_from(number(&header["host_pid"])?).map_err(|_| bad())?,
                start: number(&header["host_start"])?,
            },
            host_child_cpu_ns: optional(&header["host_child_cpu_ns"])?,
            at_unix_ms: number(&header["at_unix_ms"])?,
            first_seq: number(&header["first_seq"])?,
            next_seq: number(&header["next_seq"])?,
            originals: Vec::new(),
            sessions: Vec::new(),
            outstanding_sessions: Vec::new(),
            terminals: Vec::new(),
            unproven_active: number(&header["unproven_active"])?,
            lost_origins: number(&header["lost_origins"])?,
        };
        let mut count = 0usize;
        loop {
            let frame = read()?;
            if frame["done"].as_bool() == Some(true) {
                break;
            }
            let rows = frame
                .as_array()
                .filter(|rows| !rows.is_empty() && rows.len() <= 16)
                .ok_or_else(bad)?;
            count = count
                .checked_add(rows.len())
                .filter(|n| *n <= 1_000_000)
                .ok_or_else(bad)?;
            for r in rows {
                let session = r["session"]
                    .as_str()
                    .filter(|s| aterm_types::control_request::Sid::parse(s).is_some())
                    .ok_or_else(bad)?
                    .to_owned();
                match r["kind"].as_str() {
                    Some("original") => out.originals.push(Original {
                        session,
                        process: process(r)?,
                        cpu_ns: optional(&r["cpu_ns"])?,
                        child_cpu_ns: optional(&r["child_cpu_ns"])?,
                        reaps_unwaited: if r["reaps_unwaited"].is_null() {
                            None
                        } else {
                            Some(r["reaps_unwaited"].as_bool().ok_or_else(bad)?)
                        },
                        footprint: optional(&r["footprint"])?,
                        peak_footprint: optional(&r["peak_footprint"])?,
                    }),
                    Some("outstanding") => out.outstanding_sessions.push(session),
                    Some("session") => out.sessions.push(SessionUsage {
                        session,
                        reaped_cpu_ns: number(&r["reaped_cpu_ns"])?,
                        unread: number(&r["unread"])?,
                    }),
                    Some("terminal") => out.terminals.push(Terminal {
                        seq: number(&r["seq"])?,
                        session,
                        process: process(r)?,
                        uid: optional(&r["uid"])?
                            .map(|uid| u32::try_from(uid).map_err(|_| bad()))
                            .transpose()?,
                        coalition: optional(&r["coalition"])?,
                        version: if r["exec_version"].is_null() {
                            None
                        } else {
                            Some(
                                i32::try_from(r["exec_version"].as_i64().ok_or_else(bad)?)
                                    .map_err(|_| bad())?,
                            )
                        },
                        cpu_ns: optional(&r["cpu_ns"])?,
                        child_cpu_ns: optional(&r["child_cpu_ns"])?,
                        wait_cpu_ns: optional(&r["wait_cpu_ns"])?,
                        reaps_unwaited: if r["reaps_unwaited"].is_null() {
                            None
                        } else {
                            Some(r["reaps_unwaited"].as_bool().ok_or_else(bad)?)
                        },
                        at_unix_ms: number(&r["at_unix_ms"])?,
                        exit_cut_ns: optional(&r["exit_cut_ns"])?,
                    }),
                    _ => return Err(bad()),
                }
            }
        }
        let expected_first = since.saturating_add(1).max(out.first_seq);
        let expected_count = if since == u64::MAX || expected_first > out.next_seq {
            0
        } else {
            out.next_seq - expected_first + 1
        };
        let originals: std::collections::BTreeSet<_> =
            out.originals.iter().map(|o| o.process.pid).collect();
        let sessions: std::collections::BTreeSet<_> =
            out.sessions.iter().map(|s| &s.session).collect();
        let outstanding: std::collections::BTreeSet<_> = out.outstanding_sessions.iter().collect();
        if out.host.pid == 0
            || out.first_seq == 0
            || out.first_seq > out.next_seq.saturating_add(1)
            || declared
                != (
                    out.originals.len() as u64,
                    out.sessions.len() as u64,
                    out.terminals.len() as u64,
                    out.outstanding_sessions.len() as u64,
                )
            || originals.len() != out.originals.len()
            || sessions.len() != out.sessions.len()
            || outstanding.len() != out.outstanding_sessions.len()
            || out.terminals.len() as u64 != expected_count
            || out
                .terminals
                .first()
                .is_some_and(|t| t.seq != expected_first)
            || !out
                .terminals
                .windows(2)
                .all(|w| w[0].seq.checked_add(1) == Some(w[1].seq))
            || out
                .terminals
                .iter()
                .any(|t| t.seq < out.first_seq || t.seq > out.next_seq)
        {
            return Err(bad());
        }
        Ok(out)
    }
}

#[cfg(test)]
#[path = "resource_observation_tests.rs"]
mod tests;
