// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The vitals host's leftover pass (`sys left`, `docs/DESIGN-leftover-processes-2026-10-16.md`
//! §3.2, §5.3, §5.4): on the probe thread after the jobs pass, it reads each candidate's kernel
//! facts ([`aterm_uds::left`]), keeps each closed tab's two numbers (its shell's unique id and
//! the horizon of the reading that missed it), appends a dead run's `dead_seen` once, and judges
//! every row ([`aterm_vitals::left::judge`]). It ends nothing: it names and offers.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use aterm_sysprobe::procs::ProcessReading;
use aterm_uds::left::{Inert, Known, LeftKernel, Live, Reader, Row};
use aterm_uds::runs::Record;
use aterm_vitals::jobs::{JobKind, JobsView, Verdict};
use aterm_vitals::left::{self, ClosedTab, LeftRow, LeftView, LiveTab, RunRow, Source, Tier};

use crate::vitals_host::Request;

/// The most closed tabs kept, oldest dropped first, as the closed-session index keeps its own.
pub(crate) const CLOSED_KEPT: usize = 512;

/// How often the run records are read again with no change to their folder, ms.
const RECORDS_EVERY_MS: u64 = 60_000;

/// What the pass reads with (`kernel()` live, `inert()` in unit tests).
#[derive(Clone)]
pub(crate) struct LeftReaders {
    /// The kernel's reads.
    pub(crate) kernel: Arc<dyn LeftKernel + Send + Sync>,
    /// Where this instance's rendezvous keeps verify's run records; `None` reads none.
    pub(crate) records: Option<PathBuf>,
    /// This boot's id ([`aterm_uds::process::boot_id`]).
    pub(crate) boot: fn() -> Option<String>,
}

impl LeftReaders {
    /// The kernel's readers, with W-writer's read over this instance's rendezvous.
    pub(crate) fn kernel() -> Self {
        Self {
            kernel: Arc::new(Live {
                writer: |pid| {
                    aterm_uds::control_socket_dir()
                        .is_some_and(|dir| aterm_ctl::graph_writer(&dir, pid))
                },
            }),
            records: aterm_uds::control_socket_dir().map(|dir| {
                aterm_uds::runs::records_dir(&dir, aterm_uds::process::pid_namespace().as_deref())
            }),
            boot: aterm_uds::process::boot_id,
        }
    }

    /// The inert arm: nothing is read, and no record.
    pub(crate) fn inert() -> Self {
        Self {
            kernel: Arc::new(Inert),
            records: None,
            boot: || None,
        }
    }
}

/// A live tab the pass has seen, and what it keeps of it for the close.
#[derive(Clone, Debug, Default)]
struct Tab {
    sid: String,
    shell_pid: u32,
    /// Its shell's unique id, read the first reading the shell is seen.
    shell_uniqueid: Option<u64>,
    label: String,
    number: Option<usize>,
    /// The lowest unique id among the agent calls seen in it.
    agent_since: Option<u64>,
}

/// The run records last read, and when.
#[derive(Debug, Default)]
struct Records {
    list: Vec<(PathBuf, Record)>,
    folder_mtime: Option<SystemTime>,
    read_ms: Option<u64>,
}

/// The pass, on the probe thread.
pub(crate) struct LeftPass {
    readers: LeftReaders,
    reader: Reader,
    live: BTreeMap<u64, Tab>,
    closed: VecDeque<ClosedTab>,
    records: Records,
    /// Programs outside every job that have not started (held, translating, not started, or
    /// stopped or traced) that W-session ties to a live tab, by pid: the jobs pass names them by
    /// that tab ([`Self::ties`]).
    ties: BTreeMap<u32, u64>,
    view: Option<Arc<LeftView>>,
}

/// A sweep row of this uid in the reader's form.
fn row_of(p: &ProcessReading) -> Row {
    Row {
        pid: p.pid,
        ppid: p.ppid,
        pgid: p.pgid,
        start: p.start,
        cpu_ns: p.cpu_ns,
        birth_ms: p.birth_ms,
    }
}

/// The pids `jobs` names as not started for another reason than macOS's program check
/// (translating, on a volume not proven local, or stopped or traced): each is tied to a tab by the
/// same W-session rule as a held one, though only a held one reads held to the leftover reader.
fn unstarted_of(jobs: &JobsView) -> BTreeSet<u32> {
    let f = &jobs.findings;
    f.translating
        .iter()
        .chain(&f.not_started)
        .chain(f.stopped_unstarted.iter().map(|s| &s.named))
        .map(|n| n.pid)
        .collect()
}

impl LeftPass {
    pub(crate) fn new() -> Self {
        Self::with(if cfg!(test) {
            LeftReaders::inert()
        } else {
            LeftReaders::kernel()
        })
    }

    pub(crate) fn with(readers: LeftReaders) -> Self {
        Self {
            readers,
            reader: Reader::new(),
            live: BTreeMap::new(),
            closed: VecDeque::new(),
            records: Records::default(),
            ties: BTreeMap::new(),
            view: None,
        }
    }

    /// The last view, for a reading with no sweep.
    pub(crate) fn last(&self) -> Option<Arc<LeftView>> {
        self.view.clone()
    }

    /// The live tab a session-mate's environment ties each program that has not started to, by
    /// pid, as the last reading judged it (W-session: orphaned, born in that tab's window).
    pub(crate) fn ties(&self) -> BTreeMap<u32, u64> {
        self.ties.clone()
    }

    /// Reads the records again when their folder changed or a minute passed; a snapshot that
    /// does not read whole keeps the last one and is read again at the next reading.
    fn refresh_records(&mut self, at_ms: u64) {
        let Some(dir) = self.readers.records.as_deref() else {
            return;
        };
        let mtime = std::fs::metadata(dir).and_then(|m| m.modified()).ok();
        let due = self.records.folder_mtime != mtime
            || self
                .records
                .read_ms
                .is_none_or(|t| at_ms.saturating_sub(t) >= RECORDS_EVERY_MS);
        if !due {
            return;
        }
        if let Ok(list) = aterm_uds::runs::read_records(dir) {
            self.records.list = list;
            self.records.folder_mtime = mtime;
            self.records.read_ms = Some(at_ms);
        } else {
            self.records.read_ms = None;
        }
    }
}

impl LeftPass {
    /// One reading's pass over `sweep` (`None`: a machine-only reading, which keeps the last
    /// view), at `at_ms` (unix ms), with the jobs pass's view of the same reading, `jobs_age_ms`
    /// old then (the last pass's, where the engine did not run on this reading). `complete` says
    /// the sweep listed every process.
    pub(crate) fn tick(
        &mut self,
        at_ms: u64,
        sweep: Option<&[ProcessReading]>,
        complete: bool,
        req: &Request,
        jobs: Option<&JobsView>,
        jobs_age_ms: u64,
    ) -> Option<Arc<LeftView>> {
        let Some(sweep) = sweep else {
            return self.view.clone();
        };
        let uid = sweep
            .iter()
            .find(|p| p.pid == req.self_pid)
            .and_then(|p| p.uid);
        let rows: Vec<Row> = sweep
            .iter()
            .filter(|p| uid.is_some() && p.uid == uid)
            .filter(|p| p.state != aterm_sysprobe::jobs::RunState::Zombie)
            .map(row_of)
            .collect();
        self.refresh_records(at_ms);
        let held = jobs
            .map(|v| left::held_of(v, jobs_age_ms))
            .unwrap_or_default();
        let unstarted = jobs.map(unstarted_of).unwrap_or_default();
        let waiting = jobs.map(left::waiting_of).unwrap_or_default();
        let shells: Vec<(u64, u32)> = req
            .tabs
            .iter()
            .filter(|t| t.shell_pid > 0)
            .map(|t| (t.key, t.shell_pid))
            .collect();
        let boot = (self.readers.boot)();
        let known = Known {
            self_pid: req.self_pid,
            now_ms: at_ms,
            boot: boot.as_deref(),
            records: &self.records.list,
            held: &held,
            waiting: &waiting,
            shells: &shells,
        };
        let kernel = Arc::clone(&self.readers.kernel);
        let reading = self.reader.read(&*kernel, &rows, &known);
        self.see_tabs(&*kernel, sweep, req, jobs, reading.horizon, at_ms);
        self.append_dead_seen(&*kernel, req.self_pid, &reading.runs, at_ms);

        let fg: BTreeMap<u64, u32> = jobs
            .map(left::foreground_groups)
            .unwrap_or_default()
            .into_iter()
            .collect();
        let live: Vec<LiveTab> = self
            .live
            .iter()
            .map(|(&key, t)| LiveTab {
                key,
                sid: t.sid.clone(),
                shell_pid: t.shell_pid,
                shell_uniqueid: t.shell_uniqueid,
                fg_pgid: fg.get(&key).copied(),
                agent_since: t.agent_since,
            })
            .collect();
        let closed: Vec<ClosedTab> = self.closed.iter().cloned().collect();
        let tellers = jobs.map(|v| left::tellers(v, &live)).unwrap_or_default();
        let spared = [req.self_pid];
        let ctx = left::Ctx {
            launchd: reading.launchd,
            live: &live,
            closed: &closed,
            spared: &spared,
            tellers: &tellers,
        };
        let rows: Vec<LeftRow> = reading
            .facts
            .iter()
            .filter_map(|f| left::judge(f, &ctx))
            .collect();
        self.ties = reading
            .facts
            .iter()
            .filter(|f| f.held.is_some() || unstarted.contains(&f.pid))
            .filter_map(|f| {
                let tie = left::session_of(f, &ctx)?;
                (!tie.closed).then_some((f.pid, tie.key))
            })
            .collect();
        for tab in &mut self.closed {
            tab.left = rows
                .iter()
                .filter(|r| r.from == Source::Closed(tab.key) && r.token.is_some())
                .count();
        }
        let runs = reading
            .runs
            .iter()
            .map(|run| {
                let id = run.record.id.map(|i| i.text()).unwrap_or_default();
                let of_run = || {
                    rows.iter()
                        .filter(|r| matches!(&r.from, Source::Run(rid) if *rid == id))
                };
                RunRow {
                    id: id.clone(),
                    dead: run.dead,
                    dead_ms: run
                        .record
                        .dead_seen_ms
                        .filter(|_| run.dead)
                        .map(|ms| at_ms.saturating_sub(ms)),
                    sid: run.record.sid.clone(),
                    roots: run
                        .record
                        .roots
                        .iter()
                        .map(|r| r.display().to_string())
                        .collect(),
                    left: of_run().count(),
                    next_verify: of_run().any(|r| r.tier == Tier::Auto),
                }
            })
            .collect();
        self.view = Some(Arc::new(LeftView {
            rows,
            runs,
            links: Vec::new(),
            closed: self.closed.iter().cloned().collect(),
            live,
            complete,
            bounded: reading.bounded,
        }));
        self.view.clone()
    }

    /// Takes this reading's tabs: a tab first seen has its shell's unique id read once; a tab
    /// the last reading had and this one lacks closed, and keeps its shell's unique id and this
    /// reading's horizon, unless its shell was never read.
    fn see_tabs(
        &mut self,
        kernel: &dyn LeftKernel,
        sweep: &[ProcessReading],
        req: &Request,
        jobs: Option<&JobsView>,
        horizon: u64,
        at_ms: u64,
    ) {
        let start_of = |pid: u32| sweep.iter().find(|p| p.pid == pid).and_then(|p| p.start);
        let mut now = BTreeMap::new();
        for t in req.tabs.iter().filter(|t| t.shell_pid > 0) {
            let mut tab = self
                .live
                .remove(&t.key)
                .filter(|old| old.shell_pid == t.shell_pid)
                .unwrap_or_else(|| Tab {
                    shell_pid: t.shell_pid,
                    ..Tab::default()
                });
            if tab.shell_uniqueid.is_none() {
                tab.shell_uniqueid =
                    self.reader
                        .uniqueid(kernel, t.shell_pid, start_of(t.shell_pid));
            }
            if let Some((_, sid)) = req.session_ids.iter().find(|(k, _)| *k == t.key) {
                tab.sid.clone_from(sid);
            }
            tab.label.clone_from(&t.label);
            if let Some(p) = req.layout.iter().find(|p| p.key == t.key) {
                tab.number = Some(p.tab);
            }
            for call in jobs.into_iter().flat_map(|v| &v.rows).filter(|r| {
                r.tab == t.key && r.kind == JobKind::AgentCall && r.verdict != Verdict::Exited
            }) {
                let leader = call.leader.0;
                if let Some(id) = self.reader.uniqueid(kernel, leader, start_of(leader)) {
                    tab.agent_since = Some(tab.agent_since.map_or(id, |a| a.min(id)));
                }
            }
            now.insert(t.key, tab);
        }
        for (key, tab) in std::mem::replace(&mut self.live, now) {
            let Some(shell_uniqueid) = tab.shell_uniqueid else {
                continue;
            };
            self.closed.push_back(ClosedTab {
                key,
                sid: tab.sid,
                label: tab.label,
                number: tab.number,
                shell_uniqueid,
                horizon,
                closed_ms: at_ms,
                agent_since: tab.agent_since,
                left: 0,
            });
            while self.closed.len() > CLOSED_KEPT {
                self.closed.pop_front();
            }
        }
    }

    /// Appends the `dead_seen` line to each dead run's record that has none: the highest unique
    /// id of a table of this uid read after this reading found the launcher dead
    /// ([`aterm_uds::runs::table`]), never the sweep's, which was read before, so whatever the
    /// run started before it died is inside its window. A table that does not read whole writes
    /// none, and a later reading tries again. A record keeps its first such line; the record as
    /// read takes the line too, so the readings until the next read of the folder append none.
    fn append_dead_seen(
        &mut self,
        kernel: &dyn LeftKernel,
        self_pid: u32,
        runs: &[aterm_uds::left::RunRead],
        at_ms: u64,
    ) {
        let mut dead = runs
            .iter()
            .filter(|r| r.dead && r.record.dead_seen.is_none())
            .peekable();
        if dead.peek().is_none() {
            return;
        }
        let Some((_, horizon)) = i32::try_from(self_pid)
            .ok()
            .and_then(|me| aterm_uds::runs::table(kernel, me))
        else {
            return;
        };
        for run in dead {
            aterm_uds::runs::append_dead_seen(&run.path, horizon, at_ms);
            if let Some((_, rec)) = self
                .records
                .list
                .iter_mut()
                .find(|(path, _)| *path == run.path)
            {
                rec.dead_seen = Some(horizon);
                rec.dead_seen_ms = Some(at_ms);
            }
        }
    }
}

/// `pid` read again on fresh state and judged against the host's freshly captured live and
/// confirmed closed tabs: the `end` verb's re-proof before each signal. No cached jobs view
/// supplies foreground, held or waiting authority. Every kernel read is made anew
/// ([`aterm_uds::left::read_one`]), and the run records are read again from the rendezvous; none
/// read is no record, so a run's tie then proves nothing. `None` when the pid is gone, is no
/// candidate, or is this process.
pub(crate) fn reread(
    readers: &LeftReaders,
    live: &[LiveTab],
    closed: &[ClosedTab],
    self_pid: u32,
    pid: u32,
    now_ms: u64,
) -> Option<LeftView> {
    if pid == self_pid {
        return None;
    }
    let records = readers
        .records
        .as_deref()
        .and_then(|dir| aterm_uds::runs::read_records(dir).ok())
        .unwrap_or_default();
    let shells: Vec<(u64, u32)> = live
        .iter()
        .filter(|t| t.shell_pid > 0)
        .map(|t| (t.key, t.shell_pid))
        .collect();
    let boot = (readers.boot)();
    let known = Known {
        self_pid,
        now_ms,
        boot: boot.as_deref(),
        records: &records,
        held: &[],
        waiting: &[],
        shells: &shells,
    };
    let kernel = &*readers.kernel;
    let facts = aterm_uds::left::read_one(kernel, pid, &known)?;
    let spared = [self_pid];
    let ctx = left::Ctx {
        launchd: kernel.launchd_version(),
        live,
        closed,
        spared: &spared,
        tellers: &[],
    };
    let row = left::judge(&facts, &ctx)?;
    let runs = facts
        .runs
        .iter()
        .filter_map(|tie| {
            let (_, record) = records.iter().find(|(_, record)| {
                record.id.as_ref().is_some_and(|id| id.text() == tie.id)
                    && boot.as_deref() == Some(record.boot.as_str())
            })?;
            Some(RunRow {
                id: tie.id.clone(),
                dead: tie.dead,
                dead_ms: record
                    .dead_seen_ms
                    .filter(|_| tie.dead)
                    .map(|at| now_ms.saturating_sub(at)),
                sid: record.sid.clone(),
                roots: record
                    .roots
                    .iter()
                    .map(|r| r.display().to_string())
                    .collect(),
                left: usize::from(matches!(&row.from, Source::Run(id) if id == &tie.id)),
                next_verify: row.tier == Tier::Auto,
            })
        })
        .collect();
    Some(LeftView {
        rows: vec![row],
        runs,
        live: live.to_vec(),
        closed: closed.to_vec(),
        ..LeftView::default()
    })
}

#[cfg(test)]
#[path = "vitals_left_tests.rs"]
mod tests;
