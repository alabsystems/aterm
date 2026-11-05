// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE WINDOW'S ACTION TELEMETRY (Continuum TM-0, `aterm_telemetry`): one row for each action
//! this aterm watches, from the readings it already takes, and the `telemetry` verb that reads
//! them back.
//!
//! * A tab's command (`kind=command`): the status sweep's block read ([`commands_seen`]) closes
//!   each OSC 133 block once. Its wall time is the shell's own C and D stamps; its CPU is what
//!   the shell and the processes it reaped spent between the reader seeing C and seeing D
//!   ([`CmdMeter::slice`], one `proc_pid_rusage` of the shell at each edge); its peak memory and
//!   a `stalled` stop are the jobs engine's readings of the block's job ([`jobs_passed`]).
//! * An agent's tool call (`kind=agent-call`): the jobs engine's `agent-call` jobs as they end
//!   ([`jobs_passed`]), every figure sampled at its passes.
//! * A worker spawn (`kind=spawn`): the `spawn` verb's answer ([`spawned`]).
//!
//! Nothing here samples anything: every figure is a reading another part of the window takes
//! anyway, and a figure no reading covers is written unknown with the source that would supply
//! it. Rows go to `aterm_telemetry`'s writer thread, so no session waits on the disk.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Mutex, OnceLock, PoisonError};

use aterm_core::terminal::Terminal;
use aterm_telemetry::{Digest, Fig, Kind, Meter, Outcome, Row, Stop};

/// What a figure no reading covers names as its source.
pub(crate) mod source {
    /// A command's CPU: the shell's rusage at its C and D, which the reader did not see both of.
    pub(crate) const SHELL_RUSAGE: &str = "shell-rusage";
    /// Peak memory and an agent call's CPU: the jobs engine's passes, none of which read it.
    pub(crate) const JOBS: &str = "jobs";
    /// Wall time: the shell's OSC 133 stamps.
    pub(crate) const OSC133: &str = "osc133";
    /// Admission wait: the claim's own row in aterm-capacity's ledger.
    pub(crate) const CAPACITY: &str = "aterm-capacity";
    /// A build's cache tier: the build tool's own action records.
    pub(crate) const TARGO: &str = "targo";
}

/// This process's host: its name and facts' digest, once the writer started.
static HOST: OnceLock<Option<(String, Digest)>> = OnceLock::new();

/// Start this process's writer, once: on the log under the state directory, with the host's facts
/// read now. A unit-test build starts none, so no test writes into a real state directory.
pub(crate) fn start() {
    if cfg!(test) {
        return;
    }
    HOST.get_or_init(|| {
        aterm_telemetry::start_default().map(|facts| {
            let digest = facts.digest();
            (facts.host, digest)
        })
    });
}

fn host() -> Option<(&'static str, Digest)> {
    HOST.get()
        .and_then(Option::as_ref)
        .map(|(name, digest)| (name.as_str(), *digest))
}

fn emit(row: Row) {
    aterm_telemetry::emit(row);
}

/// The most blocks a [`CmdMeter`] keeps readings for.
const METER_KEEP: usize = 64;

/// A session's command edges as its PTY reader saw them: the shell's CPU (its own and its reaped
/// children's) and its children when a block began executing and when it stopped. Written by the
/// reader under the terminal lock ([`Self::slice`]), taken by the status sweep once the block's
/// row goes out.
///
/// The shell's reaped-children CPU counts every child it reaped in the window, a background job
/// of an earlier command's among them. So a block's CPU is kept only when no job that ran beside
/// it was reaped in its window: a child of the shell's at the prompt before the block (its
/// previous D) that still ran at the block's C and was gone at its D took its CPU into the
/// window, and the block's CPU is then unknown, never another job's charged to it. A prompt
/// helper that lives on (in both lists) costs nothing; a child of the block's own (born after
/// the prompt) is its own. A job the block left running spends its CPU after the block, which
/// its row does not hold.
#[derive(Debug, Default)]
pub(crate) struct CmdMeter(Mutex<MeterState>);

#[derive(Debug, Default)]
struct MeterState {
    /// Block id → its start.
    started: BTreeMap<u64, Start>,
    /// Block id → its CPU, ns; `None` where it cannot be told from another job's.
    ended: BTreeMap<u64, Option<u64>>,
    /// The shell's children when the newest block stopped: at the prompt, the jobs it kept.
    prompt: Option<Vec<u32>>,
    /// The newest block a command row went out for.
    reported: Option<u64>,
}

/// A block's start: the shell's CPU, ns, and the jobs that ran beside it (the prompt's children
/// still there at its C; `None` where either list was not read).
#[derive(Debug)]
struct Start {
    cpu: u64,
    beside: Option<Vec<u32>>,
}

/// One reading at an edge: the shell's CPU with its reaped children's, ns, and its children,
/// sorted (`None` where the kernel did not list them).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Edge {
    pub(crate) cpu: u64,
    pub(crate) children: Option<Vec<u32>>,
}

/// The terminal's current block and whether it is executing, read before a slice is parsed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct BlockMark(Option<(u64, bool)>);

/// [`BlockMark`] of `t` now.
pub(crate) fn mark(t: &Terminal) -> BlockMark {
    BlockMark(
        t.current_block()
            .map(|b| (b.id, b.state == aterm_types::BlockState::Executing)),
    )
}

impl BlockMark {
    fn executing(self) -> Option<u64> {
        self.0.filter(|(_, runs)| *runs).map(|(id, _)| id)
    }
}

fn keep_newest<V>(map: &mut BTreeMap<u64, V>) {
    while map.len() > METER_KEEP {
        map.pop_first();
    }
}

impl CmdMeter {
    /// After a slice: where the block executing changed since `before`, read the shell `shell`
    /// once ([`Edge`]) and note it as the end of the block that stopped and the start of the one
    /// that began. A block whose C and D came in one slice gets no reading: its CPU stays unknown
    /// rather than read as none.
    pub(crate) fn slice(&self, before: BlockMark, t: &Terminal, shell: i32) {
        self.slice_with(before, mark(t), shell, read_shell);
    }

    fn slice_with(
        &self,
        before: BlockMark,
        after: BlockMark,
        shell: i32,
        read: impl FnOnce(i32) -> Option<Edge>,
    ) {
        let (was, now) = (before.executing(), after.executing());
        if was == now {
            return;
        }
        let Some(edge) = read(shell) else {
            return;
        };
        let mut st = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(id) = was {
            if let Some(start) = st.started.remove(&id) {
                let alone = match (&start.beside, &edge.children) {
                    (Some(beside), Some(now)) => {
                        beside.iter().all(|pid| now.binary_search(pid).is_ok())
                    }
                    _ => false,
                };
                let cpu = edge.cpu.checked_sub(start.cpu).filter(|_| alone);
                st.ended.insert(id, cpu);
                keep_newest(&mut st.ended);
            }
            st.prompt.clone_from(&edge.children);
        }
        if let Some(id) = now {
            let beside = match (&st.prompt, &edge.children) {
                (Some(prompt), Some(at_c)) => Some(
                    prompt
                        .iter()
                        .copied()
                        .filter(|pid| at_c.binary_search(pid).is_ok())
                        .collect(),
                ),
                _ => None,
            };
            st.started.insert(
                id,
                Start {
                    cpu: edge.cpu,
                    beside,
                },
            );
            keep_newest(&mut st.started);
        }
    }

    /// The CPU block `id` spent, ns, once both its readings are in and no other job's is in it.
    fn take_cpu(&self, id: u64) -> Option<u64> {
        let mut st = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        st.started.remove(&id);
        st.ended.remove(&id).flatten()
    }

    /// Whether block `id` is past the newest one reported, and mark it reported.
    fn report(&self, id: u64) -> bool {
        let mut st = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if st.reported.is_some_and(|r| id <= r) {
            return false;
        }
        st.reported = Some(id);
        true
    }
}

fn read_shell(shell: i32) -> Option<Edge> {
    let pid = u32::try_from(shell).ok().filter(|p| *p > 1)?;
    Some(Edge {
        cpu: aterm_sysprobe::jobs::cpu_with_reaped_ns(pid)?,
        children: aterm_sysprobe::jobs::child_pids(pid),
    })
}

/// Each session's public id (`s-…`, what `aterm ctl @<sid>` and the survey name) by the window's
/// local key, as the status sweep last read it from the session store ([`note_sid`]): a row
/// names its session by the id a reader can join it on. The newest [`SIDS_KEEP`].
static SIDS: Mutex<BTreeMap<u64, String>> = Mutex::new(BTreeMap::new());

const SIDS_KEEP: usize = 512;

/// The status sweep read session `local`'s public id `sid`.
pub(crate) fn note_sid(local: u64, sid: &str) {
    let mut sids = SIDS.lock().unwrap_or_else(PoisonError::into_inner);
    if sids.get(&local).is_some_and(|s| s == sid) {
        return;
    }
    sids.insert(local, sid.to_owned());
    while sids.len() > SIDS_KEEP {
        sids.pop_first();
    }
}

/// Session `local`'s public id, once the sweep noted it.
fn sid_of(local: u64) -> Option<String> {
    SIDS.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&local)
        .cloned()
}

/// The jobs engine's readings of each block's foreground job: (session, block) → (peak bytes,
/// judged stuck at its last pass), the newest [`BLOCK_JOBS_KEEP`].
static BLOCK_JOBS: Mutex<BlockJobs> = Mutex::new(BlockJobs {
    map: BTreeMap::new(),
    order: VecDeque::new(),
});

const BLOCK_JOBS_KEEP: usize = 512;

struct BlockJobs {
    map: BTreeMap<(u64, u64), (Option<u64>, bool)>,
    order: VecDeque<(u64, u64)>,
}

fn take_block_job(session: u64, block: u64) -> Option<(Option<u64>, bool)> {
    let mut jobs = BLOCK_JOBS.lock().unwrap_or_else(PoisonError::into_inner);
    let got = jobs.map.remove(&(session, block));
    if got.is_some() {
        jobs.order.retain(|k| *k != (session, block));
    }
    got
}

/// After a jobs pass at host clock `at_ms`: keep what it read of each block's job for that block's
/// command row, and write a row for each agent call that ended.
pub(crate) fn jobs_passed(jobs: &mut aterm_vitals::jobs::Jobs, at_ms: u64) {
    {
        let mut kept = BLOCK_JOBS.lock().unwrap_or_else(PoisonError::into_inner);
        for m in jobs.block_meters() {
            let key = (m.tab, m.block);
            if kept.map.insert(key, (m.peak_bytes, m.stuck)).is_none() {
                kept.order.push_back(key);
            }
        }
        while kept.order.len() > BLOCK_JOBS_KEEP {
            if let Some(old) = kept.order.pop_front() {
                kept.map.remove(&old);
            }
        }
    }
    let ended = jobs.take_ended();
    let Some((host_name, facts)) = host() else {
        return;
    };
    let wall_now = aterm_telemetry::now_ms();
    for e in ended {
        if let Some(row) = agent_call_row(&e, at_ms, wall_now, host_name, facts) {
            emit(row);
        }
    }
}

fn agent_call_row(
    e: &aterm_vitals::jobs::Ended,
    host_at_ms: u64,
    wall_now: u64,
    host_name: &str,
    facts: Digest,
) -> Option<Row> {
    if e.kind != aterm_vitals::jobs::JobKind::AgentCall {
        return None;
    }
    let to_wall = |ms: u64| wall_now.saturating_sub(host_at_ms.saturating_sub(ms));
    let what = if e.command.trim().is_empty() {
        e.name.as_str()
    } else {
        e.command.as_str()
    };
    let (outcome, stop, code) = match (e.exit, e.stuck) {
        (_, true) => (Outcome::Unknown, Stop::Stalled, e.exit),
        (Some(0), false) => (Outcome::Ok, Stop::Exit, Some(0)),
        (Some(c), false) => (Outcome::Fail, Stop::Exit, Some(c)),
        (None, false) => (Outcome::Unknown, Stop::Unknown, None),
    };
    Some(Row {
        at_ms: to_wall(e.ended_ms),
        kind: Kind::AgentCall,
        key: Some(Digest::key(&Kind::AgentCall, what, None)),
        dir: None,
        // The name the kernel showed its job running, never a word of the call's line.
        label: aterm_telemetry::row::label(&e.name),
        sid: sid_of(e.tab),
        host: host_name.to_owned(),
        facts,
        wall_ms: Fig::Known(e.ended_ms.saturating_sub(e.opened_ms)),
        cpu_ms: e.cpu_ns.map_or_else(
            || Fig::unknown(source::JOBS),
            |ns| Fig::Known(ns / 1_000_000),
        ),
        peak_bytes: e
            .peak_bytes
            .map_or_else(|| Fig::unknown(source::JOBS), Fig::Known),
        wait_ms: Fig::unknown(source::CAPACITY),
        cache: Fig::unknown(source::TARGO),
        outcome,
        stop,
        code,
        sig: None,
        meter: Meter::Sampled,
    })
}

/// The status sweep's block read of session `session` (under its terminal guard `term`): a row for
/// each block that stopped executing since the last one reported.
pub(crate) fn commands_seen(
    meter: &CmdMeter,
    session: u64,
    term: &Terminal,
    read: &crate::session_log::BlockRead,
) {
    use crate::session_log::BlockEnd;
    let Some((host_name, facts)) = host() else {
        return;
    };
    let sid = sid_of(session);
    for fact in &read.facts {
        if matches!(fact.end, BlockEnd::Running) || !meter.report(fact.id) {
            continue;
        }
        let block = term.block_by_id(fact.id);
        let line = block
            .and_then(|b| b.commandline.as_deref())
            .map(str::trim)
            .unwrap_or("");
        let dir = block.and_then(|b| b.working_directory.as_deref());
        let Some(blk) = block_facts(fact, line, dir) else {
            continue;
        };
        emit(command_row(
            &blk,
            meter.take_cpu(fact.id),
            take_block_job(session, fact.id),
            (sid.as_deref(), host_name, facts),
        ));
    }
}

/// What a closed block's `fact` says of its command, its line `line` run in `dir`; `None` while
/// it runs. Its program is the block's detail ([`crate::session_status::command_detail`]): a
/// shell builtin or keyword from the reader's fixed list, or a program the kernel saw run in its
/// window (the command's own, or the wrapper or interpreter that ran it); never a word only
/// typed, since a line's first word may be a password typed at a prompt that read no echo.
fn block_facts<'a>(
    fact: &'a crate::session_log::BlockFact,
    line: &'a str,
    dir: Option<&'a str>,
) -> Option<BlockFacts<'a>> {
    use crate::session_log::BlockEnd;
    let (exit, ended_ms) = match fact.end {
        BlockEnd::Running => return None,
        BlockEnd::Done(done) => (Some(done.exit_code), done.ended_ms),
        BlockEnd::Unended { ended_ms } => (None, ended_ms),
    };
    let program = fact
        .detail
        .as_deref()
        .and_then(|d| d.split_whitespace().next());
    Some(BlockFacts {
        line,
        dir,
        program,
        started_ms: fact.started_ms,
        ended_ms,
        exit,
    })
}

/// What a closed block says of its command.
struct BlockFacts<'a> {
    line: &'a str,
    dir: Option<&'a str>,
    program: Option<&'a str>,
    started_ms: Option<u64>,
    ended_ms: Option<u64>,
    /// `None`: the block closed with no D (its status unknown); `Some(status)` with D.
    exit: Option<Option<i32>>,
}

fn command_row(
    b: &BlockFacts<'_>,
    cpu_ns: Option<u64>,
    job: Option<(Option<u64>, bool)>,
    (sid, host_name, facts): (Option<&str>, &str, Digest),
) -> Row {
    let status = b.exit.flatten();
    let (mut stop, code, sig) = Stop::of_shell_status(status);
    let stuck = job.is_some_and(|(_, stuck)| stuck);
    if stuck {
        stop = Stop::Stalled;
    }
    let outcome = match status {
        Some(0) => Outcome::Ok,
        Some(_) => Outcome::Fail,
        None => Outcome::Unknown,
    };
    let at_ms = b
        .ended_ms
        .or(b.started_ms)
        .unwrap_or_else(aterm_telemetry::now_ms);
    Row {
        at_ms,
        kind: Kind::Command,
        // A line no read confirmed a program for gets no key: its words may be a secret typed at
        // a prompt, which an unsalted digest would let a reader confirm by guessing.
        key: b
            .program
            .map(|_| Digest::key(&Kind::Command, b.line, b.dir)),
        dir: b.dir.map(Digest::dir),
        label: aterm_telemetry::row::label(b.program.unwrap_or("")),
        sid: sid.map(str::to_owned),
        host: host_name.to_owned(),
        facts,
        wall_ms: match (b.started_ms, b.ended_ms) {
            (Some(s), Some(e)) => Fig::Known(e.saturating_sub(s)),
            _ => Fig::unknown(source::OSC133),
        },
        cpu_ms: cpu_ns.map_or_else(
            || Fig::unknown(source::SHELL_RUSAGE),
            |ns| Fig::Known(ns / 1_000_000),
        ),
        peak_bytes: job
            .and_then(|(peak, _)| peak)
            .map_or_else(|| Fig::unknown(source::JOBS), Fig::Known),
        wait_ms: Fig::unknown(source::CAPACITY),
        cache: Fig::unknown(source::TARGO),
        outcome,
        stop,
        code,
        sig,
        meter: Meter::Exact,
    }
}

/// The `spawn` verb answered `reply` for a worker of `agent` (`-` for a shell) in `cwd` after
/// `wall_ms`: a `kind=spawn` row.
pub(crate) fn spawned(agent: &str, cwd: Option<&str>, wall_ms: u64, reply: &str) {
    let Some((host_name, facts)) = host() else {
        return;
    };
    emit(spawn_row(agent, cwd, wall_ms, reply, host_name, facts));
}

fn spawn_row(
    agent: &str,
    cwd: Option<&str>,
    wall_ms: u64,
    reply: &str,
    host_name: &str,
    facts: Digest,
) -> Row {
    let ok = reply.starts_with("OK");
    Row {
        at_ms: aterm_telemetry::now_ms(),
        kind: Kind::Spawn,
        key: Some(Digest::key(&Kind::Spawn, agent, cwd)),
        dir: cwd.map(Digest::dir),
        label: aterm_telemetry::row::label(agent),
        sid: None,
        host: host_name.to_owned(),
        facts,
        wall_ms: Fig::Known(wall_ms),
        cpu_ms: Fig::Na,
        peak_bytes: Fig::Na,
        wait_ms: Fig::unknown(source::CAPACITY),
        cache: Fig::Na,
        outcome: if ok { Outcome::Ok } else { Outcome::Refused },
        stop: if ok { Stop::Exit } else { Stop::Unknown },
        code: None,
        sig: None,
        meter: Meter::Stamped,
    }
}

/// The `telemetry` verb: `telemetry [rows] [--since <t>] [--kind <k>] [--limit <n>]`, its JSON
/// form when `json`.
pub(crate) fn cmd(rest: &str, json: bool) -> String {
    let Some(dir) = aterm_telemetry::log::default_dir() else {
        return "ERR telemetry: this aterm has no state directory\n".to_owned();
    };
    cmd_in(&dir, rest, json, aterm_telemetry::now_ms())
}

const USAGE: &str = "ERR usage: telemetry [rows] [--since <n>s|m|h|d|<unix ms>] [--kind <kind>] [--limit <n>] [--json]\n";

fn cmd_in(dir: &std::path::Path, rest: &str, json: bool, now_ms: u64) -> String {
    let mut rows = false;
    let mut since = None;
    let mut kind: Option<String> = None;
    let mut limit = 50usize;
    let mut words = rest.split_whitespace();
    while let Some(w) = words.next() {
        let (key, inline) = match w.split_once('=') {
            Some((k, v)) => (k.trim_start_matches('-'), Some(v.to_owned())),
            None => (w.trim_start_matches('-'), None),
        };
        let mut value = || inline.clone().or_else(|| words.next().map(str::to_owned));
        match key {
            "rows" if inline.is_none() => rows = true,
            "summary" if inline.is_none() => rows = false,
            "since" => {
                let Some(v) = value() else {
                    return USAGE.to_owned();
                };
                match aterm_telemetry::summary::parse_since(&v, now_ms) {
                    Ok(t) => since = Some(t),
                    Err(e) => return format!("ERR telemetry: {e}\n"),
                }
            }
            "kind" => match value() {
                Some(v) => kind = Some(v),
                None => return USAGE.to_owned(),
            },
            "limit" | "n" => match value().and_then(|v| v.parse().ok()) {
                Some(n) => limit = n,
                None => return USAGE.to_owned(),
            },
            _ => return USAGE.to_owned(),
        }
    }
    let files = aterm_telemetry::Limits::DEFAULT.files;
    let mut reading = match aterm_telemetry::log::read(dir, files, since) {
        Ok(r) => r,
        Err(e) => return format!("ERR telemetry: {e}\n"),
    };
    if let Some(k) = kind.as_deref() {
        reading.rows.retain(|r| r.kind.word() == k);
    }
    let shown = dir.display().to_string();
    if json {
        let body = aterm_telemetry::summary::render_json(&reading, since, &shown, rows);
        return aterm_control::wire::json_ok(&body);
    }
    let text = if rows {
        aterm_telemetry::summary::render_rows(&reading, since, &shown, None, limit)
    } else {
        aterm_telemetry::summary::render(&reading, since, &shown)
    };
    let n = text.lines().count();
    format!("OK {n}\n{text}")
}

#[cfg(test)]
#[path = "telemetry_host_tests.rs"]
mod tests;
