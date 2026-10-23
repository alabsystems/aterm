// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The foreground cut: where in the PTY byte stream the foreground process group changed, so the parse thread can hand the terminal back to its host exactly there.

/// One foreground change inside a gathered batch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FgEdge {
    /// Byte offset in the batch at which the handback runs and the old holder's run ends.
    pub(crate) at: usize,
    /// Where the new holder's run begins, at or after `at`: `at` for a sampled edge.
    pub(crate) lay: usize,
    /// The foreground group that lost the terminal.
    pub(crate) from: i32,
    /// The foreground group that holds it now.
    pub(crate) to: i32,
    /// How the edge was found.
    pub(crate) kind: crate::holder_image::EdgeKind,
}

impl FgEdge {
    /// A sampled group change cut at `at`.
    pub(crate) const fn group(at: usize, from: i32, to: i32) -> Self {
        Self {
            at,
            lay: at,
            from,
            to,
            kind: crate::holder_image::EdgeKind::Group,
        }
    }
}

/// One process event as the gather heard it (`aterm_uds::procwatch`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProcSeen {
    /// The process that replaced its image.
    pub(crate) pid: i32,
    /// Its group (`0`: gone before it was read).
    pub(crate) pgid: i32,
    /// `tcgetpgrp(master)` at the event.
    pub(crate) fg: i32,
}

/// Turns the gather's foreground samples into at most one [`FgEdge`] per batch.
#[derive(Debug)]
#[cfg_attr(test, derive(Clone))]
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) struct FgCutter {
    running: i32,
    cut: usize,
    clean: usize,
    parked: Option<usize>,
    edge: Option<FgEdge>,
}

#[cfg_attr(not(unix), allow(dead_code))]
impl FgCutter {
    /// A cutter whose holder is the reader's first sample, so a job holding the terminal when the reader attached (an adopted session) still has its death as an edge.
    pub(crate) fn new(first: i32) -> Self {
        Self {
            running: first.max(0),
            cut: 0,
            clean: 0,
            parked: None,
            edge: None,
        }
    }

    /// Start a new batch: nothing read yet, no edge.
    pub(crate) fn begin(&mut self) {
        self.cut = 0;
        self.clean = 0;
        self.parked = None;
        self.edge = None;
    }

    /// The park after the last pre-park sample ended without the watch: that
    /// sample's offset is clean.
    fn settle(&mut self) {
        if let Some(parked) = self.parked.take() {
            self.clean = parked;
        }
    }

    /// Whether this batch already carries its one edge (a process event heard now waits for the next batch, or goes byte-free after this one).
    pub(crate) fn carries_edge(&self) -> bool {
        self.edge.is_some()
    }

    /// A sample `fg` taken after `filled` bytes of this batch were read.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "foreground_handback_ownership",
            action = "Sample",
            project = "spawn::foreground_handback_conformance::project_ownership"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "foreground_handback",
            action = "Park",
            project = "foreground_handback::exec_conformance::project_exec"
        )
    )]
    pub(crate) fn sample(&mut self, fg: i32, filled: usize) -> bool {
        self.settle();
        if self.edge.is_some() {
            return true;
        }
        self.parked = Some(filled);
        if fg <= 0 {
            // no information (tcgetpgrp failed, no foreground group)
            return false;
        }
        if self.running <= 0 || fg == self.running {
            self.running = fg;
            self.cut = filled;
            return false;
        }
        self.parked = None;
        self.edge = Some(FgEdge::group(self.cut, self.running, fg));
        self.running = fg;
        true
    }

    /// Which edge a process event would be, before any image is read.
    pub(crate) fn proc_kind(&self, ev: &ProcSeen) -> Option<crate::holder_image::EdgeKind> {
        use crate::holder_image::EdgeKind;
        if self.running <= 0 || ev.pgid <= 0 || ev.fg <= 0 {
            return None;
        }
        if ev.fg == ev.pgid && ev.fg != self.running {
            return Some(EdgeKind::Group);
        }
        (ev.fg == self.running && ev.pid == self.running).then_some(EdgeKind::Exec)
    }

    /// A process event heard after `filled` bytes of this batch were read: at a park the watch ended, or before the batch's first byte.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "foreground_handback",
            action = "ObserveExec",
            project = "foreground_handback::exec_conformance::project_exec"
        )
    )]
    pub(crate) fn proc_seen(
        &mut self,
        ev: ProcSeen,
        filled: usize,
        relevant: impl FnOnce(crate::holder_image::EdgeKind, i32) -> bool,
    ) -> bool {
        use crate::holder_image::EdgeKind;
        self.parked = None;
        if self.edge.is_some() {
            return true;
        }
        let Some(kind) = self.proc_kind(&ev) else {
            return false;
        };
        let to = match kind {
            EdgeKind::Group => ev.fg,
            EdgeKind::Exec => self.running,
        };
        if !relevant(kind, to) {
            return false;
        }
        let at = match kind {
            EdgeKind::Group => self.cut,
            EdgeKind::Exec => self.clean,
        };
        self.edge = Some(FgEdge {
            at,
            lay: filled.max(at),
            from: self.running,
            to,
            kind,
        });
        self.running = to;
        true
    }

    /// The watch was drained after `filled` bytes and none of its events was an edge.
    pub(crate) fn proc_drained(&mut self, filled: usize) {
        if self.edge.is_none() {
            self.clean = self.clean.max(filled);
        }
    }

    /// This batch's edge, if any, with no batch-end sample.
    pub(crate) fn take_edge(&mut self) -> Option<FgEdge> {
        self.parked = None;
        self.edge.take()
    }

    /// The batch-end sample (taken only when no park sample already found an edge) and this batch's edge, if any.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "foreground_handback",
            action = "Deliver",
            project = "foreground_handback::exec_conformance::project_exec"
        )
    )]
    pub(crate) fn finish(&mut self, fg: impl FnOnce() -> i32, filled: usize) -> Option<FgEdge> {
        if self.edge.is_none() {
            self.sample(fg(), filled);
        }
        self.parked = None;
        self.edge.take()
    }

    /// The current holder (`0` = none seen yet): after [`Self::finish`], the group whose bytes end the batch.
    pub(crate) fn holder(&self) -> i32 {
        self.running
    }
}

/// Which foreground group armed each evidence bit in force: the parse stage's half of the handback rule (module doc).
#[derive(Debug, Default)]
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) struct FgOwners {
    owner: [i32; aterm_core::terminal::program_evidence::COUNT],
    last: u16,
}

#[cfg_attr(not(unix), allow(dead_code))]
impl FgOwners {
    /// `holder`'s bytes were just parsed; the engine's evidence is now `evidence`, and those bytes carried the setters of `asserted`.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "foreground_handback_ownership",
            action = "Arm",
            project = "spawn::foreground_handback_conformance::project_ownership"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "foreground_handback_ownership",
            action = "ShellArm",
            project = "spawn::foreground_handback_conformance::project_ownership"
        )
    )]
    pub(crate) fn observe(&mut self, holder: i32, evidence: u16, asserted: u16) {
        let taken = evidence & (!self.last | asserted);
        for (bit, owner) in self.owner.iter_mut().enumerate() {
            let mask = 1u16 << bit;
            if taken & mask != 0 {
                *owner = holder.max(0);
            } else if evidence & mask == 0 {
                *owner = 0;
            }
        }
        self.last = evidence;
    }

    /// Whether `owner` armed an input-hijacking bit that is still in force
    /// ([`program_evidence::INPUT`](aterm_core::terminal::program_evidence::INPUT)).
    fn owns_input(&self, owner: i32) -> bool {
        use aterm_core::terminal::program_evidence::INPUT;
        self.owner.iter().enumerate().any(|(bit, &o)| {
            let mask = 1u16 << bit;
            o == owner && self.last & mask & INPUT != 0
        })
    }

    /// The groups whose liveness [`Self::orphaned_by`] may need: every
    /// owner of an input-hijacking bit still in force (`to` and unknown
    /// owners excepted), each once, in bit order.
    fn suspects(&self, to: i32) -> impl Iterator<Item = i32> {
        let mut out = [0i32; aterm_core::terminal::program_evidence::COUNT];
        let mut n = 0;
        for (bit, &owner) in self.owner.iter().enumerate() {
            if self.last & (1u16 << bit) == 0
                || owner <= 0
                || owner == to
                || out[..n].contains(&owner)
                || !self.owns_input(owner)
            {
                continue;
            }
            out[n] = owner;
            n += 1;
        }
        out.into_iter().take(n)
    }

    /// The first group that armed an input-hijacking bit still in force and is gone now (`to` and unknown owners excepted).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "foreground_handback_ownership",
            action = "Reclaim",
            project = "spawn::foreground_handback_conformance::project_ownership"
        )
    )]
    pub(crate) fn orphaned_by(&self, to: i32, mut gone: impl FnMut(i32) -> bool) -> Option<i32> {
        self.suspects(to).find(|&owner| gone(owner))
    }

    /// Probe, before the term lock is taken, every group the handback at the edge `from → to` can ask about.
    pub(crate) fn probe_edge(
        &self,
        from: i32,
        to: i32,
        mut gone: impl FnMut(i32, FgRole) -> bool,
    ) -> FgVerdicts {
        let mut v = FgVerdicts::default();
        for pgid in core::iter::once(from).chain(self.suspects(to)) {
            if pgid <= 0 || pgid == to || v.asked().any(|(p, _)| p == pgid) {
                continue;
            }
            let role = if pgid == from {
                FgRole::Holder
            } else {
                FgRole::Bystander
            };
            if let Some(slot) = v.asked.get_mut(v.n) {
                *slot = (pgid, gone(pgid, role));
                v.n += 1;
            }
        }
        v
    }
}

/// The liveness verdicts [`FgOwners::probe_edge`] took before the term lock, read under it.
#[derive(Clone, Copy, Debug)]
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) struct FgVerdicts {
    asked: [(i32, bool); aterm_core::terminal::program_evidence::COUNT + 1],
    n: usize,
}

impl Default for FgVerdicts {
    fn default() -> Self {
        Self {
            asked: [(0, false); aterm_core::terminal::program_evidence::COUNT + 1],
            n: 0,
        }
    }
}

#[cfg_attr(not(unix), allow(dead_code))]
impl FgVerdicts {
    fn asked(&self) -> impl Iterator<Item = (i32, bool)> + '_ {
        self.asked[..self.n].iter().copied()
    }

    /// Whether `pgid` was probed and found gone.
    pub(crate) fn gone(&self, pgid: i32) -> bool {
        self.asked().any(|(p, g)| p == pgid && g)
    }

    /// The groups probed, in order (the tests' once-each check).
    #[cfg(test)]
    pub(crate) fn probed(&self) -> Vec<i32> {
        self.asked().map(|(p, _)| p).collect()
    }
}

/// Which side of a foreground edge a probed group is on: the liveness rule differs ([`group_gone`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) enum FgRole {
    /// The group that lost the terminal at this edge (`from`).
    Holder,
    /// Any other owner of an input bit: gone only when no member of its group survives ([`group_empty`]).
    Bystander,
}

/// The liveness probe's shape (`SessionFactory::fg_gone`): `(master, pgid, role)`, shipping [`group_gone`].
pub(crate) type FgGone = fn(i32, i32, FgRole) -> bool;

/// The leader half of the shipping liveness probe ([`group_gone`]).
pub(crate) fn leader_gone(pgid: i32) -> bool {
    #[cfg(unix)]
    {
        if pgid <= 0 {
            return false;
        }
        // SAFETY: signal 0 performs only the existence and permission checks and delivers nothing.
        let rc = unsafe { libc::kill(pgid, 0) };
        rc == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    }
    #[cfg(not(unix))]
    {
        let _ = pgid;
        false
    }
}

/// The orphan rule, pure: a group's modes are orphaned when its leader is gone and no surviving member is stopped.
pub(crate) fn group_orphaned(leader_gone: bool, member_stopped: impl FnOnce() -> bool) -> bool {
    leader_gone && !member_stopped()
}

/// The liveness rule, pure, by [`FgRole`].
pub(crate) fn owner_gone(
    role: FgRole,
    leader_gone: impl FnOnce() -> bool,
    member_stopped: impl FnOnce() -> bool,
    group_empty: impl FnOnce() -> bool,
) -> bool {
    match role {
        FgRole::Holder => group_orphaned(leader_gone(), member_stopped),
        FgRole::Bystander => group_empty(),
    }
}

/// The shipping liveness probe (`SessionFactory::fg_gone`).
pub(crate) fn group_gone(master: i32, pgid: i32, role: FgRole) -> bool {
    owner_gone(
        role,
        || leader_gone(pgid),
        || group_has_stopped_member(master, pgid),
        || group_empty(pgid),
    )
}

/// Whether no process of group `pgid` exists: `kill(-pgid, 0)` fails with `ESRCH`.
pub(crate) fn group_empty(pgid: i32) -> bool {
    #[cfg(unix)]
    {
        if pgid <= 1 {
            return false;
        }
        // SAFETY: signal 0 performs only the existence and permission checks and delivers nothing.
        let rc = unsafe { libc::kill(-pgid, 0) };
        rc == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    }
    #[cfg(not(unix))]
    {
        let _ = pgid;
        false
    }
}

/// Whether any live process of group `pgid` is stopped (job-control or trace stop).
pub(crate) fn group_has_stopped_member(master: i32, pgid: i32) -> bool {
    if pgid <= 0 {
        return false;
    }
    #[cfg(target_os = "macos")]
    {
        let _ = master;
        darwin_group_has_stopped_member(pgid)
    }
    #[cfg(target_os = "linux")]
    {
        linux_group_has_stopped_member(master, pgid)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = master;
        false
    }
}

/// Linux: [`group_has_stopped_member`]'s session-bounded walk, with the
/// whole-table walk as the fallback.
#[cfg(target_os = "linux")]
fn linux_group_has_stopped_member(master: i32, pgid: i32) -> bool {
    let stopped = |pid: i32| {
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .is_ok_and(|stat| proc_stat_is_stopped_in(&stat, pgid))
    };
    let sid = if master >= 0 {
        // SAFETY: TIOCGSID reads the session id of the terminal `master` names (a pty master answers for its slave).
        unsafe { libc::tcgetsid(master) }
    } else {
        -1
    };
    if sid > 0
        && let Some(found) = tree_any(sid, linux_children, stopped, TREE_CAP)
    {
        return found;
    }
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return false;
    };
    dir.flatten().any(|entry| {
        entry
            .file_name()
            .to_str()
            .and_then(|pid| pid.parse::<i32>().ok())
            .is_some_and(stopped)
    })
}

/// Linux: the children of every thread of `pid` (`/proc/<pid>/task/<tid>/children`).
#[cfg(target_os = "linux")]
pub(crate) fn linux_children(pid: i32) -> Option<Vec<i32>> {
    let tasks = std::fs::read_dir(format!("/proc/{pid}/task")).ok()?;
    let mut out = Vec::new();
    let mut read_any = false;
    for task in tasks.flatten() {
        if let Ok(list) = std::fs::read_to_string(task.path().join("children")) {
            read_any = true;
            out.extend(parse_children(&list));
        }
    }
    read_any.then_some(out)
}

/// How many processes the session walk visits at most: a session with more
/// than this is not one a terminal tab runs, and past it the walk answers
/// what it found so far.
#[cfg(any(target_os = "linux", test))]
const TREE_CAP: usize = 4096;

/// One `/proc/<pid>/task/<tid>/children` file: space-separated pids.
#[cfg(any(target_os = "linux", test))]
fn parse_children(list: &str) -> impl Iterator<Item = i32> + '_ {
    list.split_ascii_whitespace()
        .filter_map(|pid| pid.parse::<i32>().ok())
        .filter(|pid| *pid > 0)
}

/// Whether `hit` holds for `root` or any of its descendants, breadth first,
/// visiting at most `cap` processes.
#[cfg(any(target_os = "linux", test))]
fn tree_any(
    root: i32,
    mut children: impl FnMut(i32) -> Option<Vec<i32>>,
    mut hit: impl FnMut(i32) -> bool,
    cap: usize,
) -> Option<bool> {
    let mut queue = std::collections::VecDeque::from(children(root)?);
    if hit(root) {
        return Some(true);
    }
    let mut visited = 1_usize;
    while let Some(pid) = queue.pop_front() {
        if visited >= cap {
            break;
        }
        visited += 1;
        if hit(pid) {
            return Some(true);
        }
        queue.extend(children(pid).unwrap_or_default());
    }
    Some(false)
}

/// macOS: list the group (`proc_listpgrppids`), then read each member's
/// status ([`darwin_member_status`], `PROC_PIDT_SHORTBSDINFO`).
#[cfg(target_os = "macos")]
fn darwin_group_has_stopped_member(pgid: i32) -> bool {
    unsafe extern "C" {
        fn proc_listpgrppids(
            pgrpid: libc::pid_t,
            buffer: *mut libc::c_void,
            buffersize: libc::c_int,
        ) -> libc::c_int;
    }
    /// `SSTOP` from `<sys/proc.h>`.
    const SSTOP: u32 = aterm_uds::process::BsdShortInfo::SSTOP;
    /// A foreground job with more members than this is not one a terminal
    /// runs; past it the listing is read as far as it goes.
    const MAX_MEMBERS: usize = 4096;
    let mut capacity = 64_usize;
    let pids = loop {
        let mut pids: Vec<libc::pid_t> = vec![0; capacity];
        let Ok(bytes) = libc::c_int::try_from(capacity * std::mem::size_of::<libc::pid_t>()) else {
            return false;
        };
        // SAFETY: `pids` holds exactly `bytes` bytes and outlives the call, which writes at most that many and answers how many pids it wrote.
        let n = unsafe { proc_listpgrppids(pgid, pids.as_mut_ptr().cast(), bytes) };
        let Ok(n) = usize::try_from(n) else {
            return false;
        };
        if n < capacity || capacity >= MAX_MEMBERS {
            pids.truncate(n.min(capacity));
            break pids;
        }
        capacity *= 4;
    };
    pids.into_iter()
        .filter(|pid| *pid > 0)
        .any(|pid| darwin_member_status(pid) == Some((SSTOP, pgid)))
}

/// macOS: one process's `(p_stat, pgid)`, `None` when it cannot be read (it exited).
#[cfg(target_os = "macos")]
fn darwin_member_status(pid: i32) -> Option<(u32, i32)> {
    let info = aterm_uds::process::read_short_info(pid)?;
    Some((info.pbsi_status, i32::try_from(info.pbsi_pgid).ok()?))
}

/// Linux: whether one `/proc/<pid>/stat` line is a stopped (`T`, or `t` for a trace stop)
/// member of group `pgid`.
#[cfg(any(target_os = "linux", test))]
fn proc_stat_is_stopped_in(stat: &str, pgid: i32) -> bool {
    let Some((_, rest)) = stat.rsplit_once(')') else {
        return false;
    };
    let mut fields = rest.split_whitespace();
    let state = fields.next();
    let _ppid = fields.next();
    let pgrp = fields.next().and_then(|f| f.parse::<i32>().ok());
    matches!(state, Some("T" | "t")) && pgrp == Some(pgid)
}

/// The byte taps' view of a batch with a handback: `buf[..at] ++ handback ++ buf[at..]`, one allocation.
pub(crate) fn splice_at(buf: &[u8], at: usize, handback: &[u8]) -> Vec<u8> {
    let at = at.min(buf.len());
    let mut out = Vec::with_capacity(buf.len() + handback.len());
    out.extend_from_slice(&buf[..at]);
    out.extend_from_slice(handback);
    out.extend_from_slice(&buf[at..]);
    out
}

/// Record a handback that put `reverted` modes back with `bytes` bytes at `edge`.
pub(crate) fn record_restored(
    tl: &mut crate::session_timeline::SessionTimeline,
    edge: FgEdge,
    reverted: &[&'static str],
    bytes: usize,
) -> String {
    let program = tl.program_of(edge.from).map(str::to_string);
    let payload = restored_payload(edge, program.as_deref(), reverted, bytes);
    tl.record("modes-restored", payload.clone());
    let at = tl.slog().now();
    tl.slog_mut().observe_modes(
        crate::session_log::observed::ModesFrom::Handback {
            from: edge.from,
            to: edge.to,
            program: program.as_deref(),
        },
        reverted,
        bytes,
        aterm_slog::Actor::Nobody,
        at,
    );
    payload
}

/// The `modes-restored` timeline payload: `from=<pgid> to=<pgid> program=<basename|-> reverted=<csv> bytes=<n>`.
pub(crate) fn restored_payload(
    edge: FgEdge,
    program: Option<&str>,
    reverted: &[&'static str],
    bytes: usize,
) -> String {
    let program = program.map_or_else(|| "-".to_string(), crate::control::pct_encode);
    format!(
        "from={} to={} program={program} reverted={} bytes={bytes}",
        edge.from,
        edge.to,
        reverted.join(",")
    )
}

/// Tier-1 bind for `ForegroundHandback`'s `Exec` and `ObserveExec`: the
/// cutter through every reachable state of the model.
#[cfg(test)]
#[path = "foreground_handback_exec_conformance.rs"]
mod exec_conformance;

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::leader_gone;
    use super::{
        FgCutter, FgEdge, FgOwners, FgRole, ProcSeen, TREE_CAP, group_orphaned, parse_children,
        proc_stat_is_stopped_in, restored_payload, splice_at, tree_any,
    };
    use crate::holder_image::EdgeKind;
    use aterm_core::terminal::program_evidence::{
        ALT_SCREEN, CURSOR_HIDDEN, FOCUS, KITTY, MOUSE, SYNC, VT52,
    };

    const SHELL: i32 = 100;
    const JOB: i32 = 200;

    #[test]
    fn foreground_handback_cutter_starts_from_an_invalid_or_a_valid_sample() {
        let mut c = FgCutter::new(-1);
        c.begin();
        assert!(!c.sample(JOB, 10));
        assert_eq!(c.finish(|| JOB, 20), None);
        c.begin();
        assert_eq!(c.finish(|| SHELL, 5), Some(FgEdge::group(0, JOB, SHELL)));

        let mut c = FgCutter::new(JOB);
        c.begin();
        assert_eq!(c.finish(|| SHELL, 7), Some(FgEdge::group(0, JOB, SHELL)));
    }

    #[test]
    fn foreground_handback_cutter_moves_the_cut_and_cuts_at_the_last_same_sample() {
        let mut c = FgCutter::new(SHELL);
        c.begin();
        assert!(!c.sample(SHELL, 1024), "unchanged: keep gathering");
        assert!(!c.sample(SHELL, 4096), "unchanged: the cut moves forward");
        assert!(c.sample(JOB, 6000), "changed at a park: deliver now");
        assert_eq!(
            c.finish(
                || panic!("an edge already exists: no batch-end probe"),
                6000
            ),
            Some(FgEdge::group(4096, SHELL, JOB))
        );
    }

    #[test]
    fn foreground_handback_cutter_cuts_at_batch_end_and_ignores_invalid_samples() {
        let mut c = FgCutter::new(JOB);
        c.begin();
        assert!(!c.sample(-1, 100), "tcgetpgrp failed: no information");
        assert!(!c.sample(0, 200), "no foreground group: no information");
        assert!(!c.sample(JOB, 300));
        assert_eq!(
            c.finish(|| SHELL, 900),
            Some(FgEdge::group(300, JOB, SHELL))
        );
        c.begin();
        assert!(!c.sample(SHELL, 50));
        assert_eq!(c.finish(|| SHELL, 80), None);
    }

    #[test]
    fn foreground_handback_cutter_reports_at_most_one_edge_per_batch() {
        let mut c = FgCutter::new(SHELL);
        c.begin();
        assert!(!c.sample(SHELL, 10));
        assert!(c.sample(JOB, 20));
        assert!(c.sample(SHELL, 30), "still delivering: the edge is taken");
        assert_eq!(c.finish(|| SHELL, 30), Some(FgEdge::group(10, SHELL, JOB)));
        c.begin();
        assert_eq!(c.finish(|| SHELL, 5), Some(FgEdge::group(0, JOB, SHELL)));
    }

    #[test]
    fn foreground_handback_cutter_begin_resets_the_cut() {
        let mut c = FgCutter::new(SHELL);
        c.begin();
        assert!(!c.sample(SHELL, 5000));
        assert_eq!(c.finish(|| SHELL, 6000), None);
        c.begin();
        assert!(c.sample(JOB, 100));
        assert_eq!(
            c.finish(|| JOB, 100).map(|e| e.at),
            Some(0),
            "the previous batch's offset must not leak into this one"
        );
    }

    #[test]
    fn foreground_handback_cutter_reports_its_holder() {
        let mut c = FgCutter::new(-1);
        assert_eq!(c.holder(), 0, "no holder yet");
        c.begin();
        assert_eq!(c.finish(|| SHELL, 3), None);
        assert_eq!(c.holder(), SHELL);
        c.begin();
        assert!(c.finish(|| JOB, 3).is_some());
        assert_eq!(c.holder(), JOB, "after an edge the holder is edge.to");
        let mut c = FgCutter::new(JOB);
        c.begin();
        assert!(c.sample(SHELL, 0));
        assert_eq!(c.finish(|| SHELL, 40), Some(FgEdge::group(0, JOB, SHELL)));
    }

    /// An `exec` heard at a park (card engine §3.3).
    fn exec_of(pid: i32, pgid: i32, fg: i32) -> ProcSeen {
        ProcSeen { pid, pgid, fg }
    }

    /// An Exec edge (the running leader replaced its image) cuts at the last
    /// clean offset, not at the poll that heard it, and lays the new run at
    /// that poll: the bytes between are no holder's.
    #[test]
    fn foreground_handback_cutter_cuts_an_exec_at_the_last_clean_poll() {
        let mut c = FgCutter::new(JOB);
        c.begin();
        assert!(!c.sample(JOB, 100));
        assert!(!c.sample(JOB, 200));
        let mut asked = None;
        assert!(c.proc_seen(exec_of(JOB, JOB, JOB), 200, |kind, to| {
            asked = Some((kind, to));
            true
        }));
        assert_eq!(asked, Some((EdgeKind::Exec, JOB)));
        let edge = c.finish(|| JOB, 200).expect("the exec edge");
        assert_eq!(
            (edge.at, edge.lay, edge.from, edge.to, edge.kind),
            (100, 200, JOB, JOB, EdgeKind::Exec),
            "cut at the last clean poll, laid at the one that heard it"
        );
        assert_eq!(c.holder(), JOB);

        c.begin();
        assert!(!c.sample(JOB, 100));
        assert!(!c.proc_seen(exec_of(JOB, JOB, JOB), 100, |_, _| false));
        c.proc_drained(100);
        assert!(!c.sample(JOB, 300));
        assert!(c.proc_seen(exec_of(JOB, JOB, JOB), 300, |_, _| true));
        let edge = c.finish(|| JOB, 300).expect("the exec edge");
        assert_eq!((edge.at, edge.lay), (100, 300));

        c.begin();
        assert!(c.proc_seen(exec_of(JOB, JOB, JOB), 0, |_, _| true));
        let edge = c.finish(|| JOB, 0).expect("the exec edge");
        assert_eq!((edge.at, edge.lay), (0, 0));
    }

    /// A Group edge (the `exec`ed process leads the group now in front) cuts
    /// where a sampled one would: the last sample that showed the old group.
    #[test]
    fn foreground_handback_cutter_cuts_a_kqueue_group_edge_at_the_last_same_sample() {
        let mut c = FgCutter::new(SHELL);
        c.begin();
        assert!(!c.sample(SHELL, 40));
        assert!(!c.sample(-1, 90));
        assert!(c.proc_seen(exec_of(JOB, JOB, JOB), 90, |kind, to| {
            assert_eq!((kind, to), (EdgeKind::Group, JOB));
            true
        }));
        assert!(c.carries_edge());
        let edge = c.finish(|| JOB, 90).expect("the group edge");
        assert_eq!(
            (edge.at, edge.lay, edge.from, edge.to, edge.kind),
            (40, 90, SHELL, JOB, EdgeKind::Group)
        );
        assert_eq!(c.holder(), JOB);
    }

    /// Only an `exec` that enters or leaves the terminal's holder, and that the
    /// gather calls card-relevant, is an edge; the rest fold into the sampled
    /// cut, and a batch keeps at most one edge.
    #[test]
    fn foreground_handback_cutter_folds_every_other_process_event() {
        const MEMBER: i32 = 201;
        const BG: i32 = 300;
        let mut c = FgCutter::new(JOB);
        c.begin();
        let never = |_: EdgeKind, _: i32| -> bool { panic!("no candidate") };
        assert!(!c.proc_seen(exec_of(MEMBER, JOB, JOB), 0, never));
        assert!(!c.proc_seen(exec_of(BG, BG, JOB), 0, never));
        assert!(!c.proc_seen(exec_of(JOB, 0, JOB), 0, never));
        assert!(!c.proc_seen(exec_of(JOB, JOB, -1), 0, never));
        assert!(!c.proc_seen(exec_of(JOB, JOB, JOB), 0, |_, _| false));
        assert!(!c.carries_edge());
        assert_eq!(c.finish(|| JOB, 10), None);
        let mut c = FgCutter::new(-1);
        c.begin();
        assert!(!c.proc_seen(exec_of(JOB, JOB, JOB), 0, never));

        let mut c = FgCutter::new(SHELL);
        c.begin();
        assert!(c.proc_seen(exec_of(JOB, JOB, JOB), 0, |_, _| true));
        assert!(c.proc_seen(exec_of(JOB, JOB, JOB), 5, |_, _| true));
        assert_eq!(c.finish(|| JOB, 5).map(|e| e.kind), Some(EdgeKind::Group));
    }

    #[test]
    fn foreground_handback_owners_attribute_each_bit_to_its_setter() {
        const GDB: i32 = 300;
        const INFERIOR: i32 = 400;
        let dead = |pids: &'static [i32]| move |p: i32| pids.contains(&p);

        let mut o = FgOwners::default();
        o.observe(SHELL, 0, 0);
        o.observe(GDB, ALT_SCREEN | MOUSE, 0);
        o.observe(INFERIOR, ALT_SCREEN | MOUSE, 0);
        assert_eq!(
            o.orphaned_by(GDB, dead(&[INFERIOR])),
            None,
            "the alt screen and mouse are gdb's, and gdb lives"
        );
        o.observe(INFERIOR, ALT_SCREEN | MOUSE | FOCUS, 0);
        assert_eq!(o.orphaned_by(GDB, dead(&[INFERIOR])), Some(INFERIOR));
        o.observe(INFERIOR, ALT_SCREEN | MOUSE, 0);
        assert_eq!(o.orphaned_by(GDB, dead(&[INFERIOR])), None);
        assert_eq!(o.orphaned_by(SHELL, dead(&[GDB, INFERIOR])), Some(GDB));

        let mut o = FgOwners::default();
        o.observe(JOB, MOUSE, 0);
        assert_eq!(o.orphaned_by(JOB, |_| true), None);
        assert_eq!(o.orphaned_by(SHELL, |_| true), Some(JOB));
        o.observe(JOB, 0, 0);
        assert_eq!(o.orphaned_by(SHELL, |_| true), None);

        let mut o = FgOwners::default();
        o.observe(JOB, ALT_SCREEN | MOUSE, 0);
        assert_eq!(o.orphaned_by(SHELL, dead(&[JOB])), Some(JOB));
        let mut o = FgOwners::default();
        o.observe(0, MOUSE, 0);
        assert_eq!(o.orphaned_by(SHELL, |_| true), None);
        o.observe(-1, MOUSE | ALT_SCREEN, 0);
        assert_eq!(o.orphaned_by(SHELL, |_| true), None);
    }

    /// A bit re-armed over the shell's (a one-shot's `?1000h` parsed as the shell's, then a
    /// job's `?1003h`) moves to the re-armer, so the job's death hands it back.
    #[test]
    fn foreground_handback_owners_give_a_re_armed_bit_to_its_re_armer() {
        let dead = |pids: &'static [i32]| move |p: i32| pids.contains(&p);
        let mut o = FgOwners::default();
        o.observe(SHELL, MOUSE, MOUSE);
        assert_eq!(o.orphaned_by(SHELL, |_| true), None, "the shell's own");
        o.observe(JOB, MOUSE, MOUSE);
        assert_eq!(o.orphaned_by(SHELL, dead(&[JOB])), Some(JOB));
        let mut o = FgOwners::default();
        o.observe(SHELL, KITTY, KITTY);
        o.observe(JOB, KITTY | ALT_SCREEN, KITTY | ALT_SCREEN);
        assert_eq!(o.orphaned_by(SHELL, dead(&[JOB])), Some(JOB));
        let mut o = FgOwners::default();
        o.observe(JOB, 0, MOUSE);
        assert_eq!(o.orphaned_by(SHELL, |_| true), None);
        let mut o = FgOwners::default();
        o.observe(JOB, KITTY, KITTY);
        o.observe(SHELL, KITTY, KITTY);
        assert_eq!(o.orphaned_by(SHELL, dead(&[JOB])), None);

        let mut o = FgOwners::default();
        o.observe(SHELL, MOUSE, MOUSE);
        o.observe(JOB, MOUSE | ALT_SCREEN, ALT_SCREEN);
        assert_eq!(o.orphaned_by(SHELL, dead(&[JOB])), None);
    }

    /// A gone group owning only display bits (`tput civis`, `tput smcup`, `tput smcup; cmd`)
    /// orphans nothing; one owning an input bit orphans all of its bits.
    #[test]
    fn foreground_handback_a_display_only_one_shot_orphans_nothing() {
        const TPUT: i32 = 500;
        const CMD: i32 = 600;
        for display in [
            ALT_SCREEN,
            CURSOR_HIDDEN,
            SYNC,
            VT52,
            ALT_SCREEN | CURSOR_HIDDEN,
        ] {
            let mut o = FgOwners::default();
            o.observe(SHELL, 0, 0);
            o.observe(TPUT, display, 0);
            assert_eq!(
                o.orphaned_by(SHELL, |_| true),
                None,
                "a one-shot's display bits {display:#x} are kept"
            );
        }
        let mut o = FgOwners::default();
        o.observe(SHELL, 0, 0);
        o.observe(TPUT, ALT_SCREEN, 0);
        assert_eq!(o.orphaned_by(CMD, |p| p == TPUT), None);
        o.observe(CMD, ALT_SCREEN | MOUSE, 0);
        assert_eq!(o.orphaned_by(SHELL, |p| p == TPUT || p == CMD), Some(CMD));
        let mut o = FgOwners::default();
        o.observe(TPUT, MOUSE, 0);
        assert_eq!(o.orphaned_by(SHELL, |_| true), Some(TPUT));
        let mut o = FgOwners::default();
        o.observe(JOB, ALT_SCREEN | CURSOR_HIDDEN | SYNC | KITTY | MOUSE, 0);
        assert_eq!(o.orphaned_by(SHELL, |_| true), Some(JOB));
    }

    #[test]
    fn foreground_handback_owners_ask_about_each_owner_once() {
        let mut o = FgOwners::default();
        o.observe(JOB, ALT_SCREEN | MOUSE | CURSOR_HIDDEN | KITTY, 0);
        let mut asked = Vec::new();
        assert_eq!(
            o.orphaned_by(SHELL, |p| {
                asked.push(p);
                false
            }),
            None
        );
        assert_eq!(asked, vec![JOB]);
    }

    /// The pre-lock probe asks each group the locked handback can ask about once (`from`
    /// included even when it also owns bits), never the new holder or a display-only owner;
    /// a group not probed reads as alive.
    #[test]
    fn foreground_handback_probe_edge_asks_each_group_once_before_the_lock() {
        const TPUT: i32 = 500;
        let mut o = FgOwners::default();
        o.observe(TPUT, ALT_SCREEN, 0);
        o.observe(JOB, ALT_SCREEN | MOUSE | KITTY, 0);
        let mut asked = Vec::new();
        let v = o.probe_edge(JOB, SHELL, |p, role| {
            asked.push((p, role));
            true
        });
        assert_eq!(
            asked,
            vec![(JOB, FgRole::Holder)],
            "from once, as the holder; tput owns display bits only"
        );
        assert_eq!(v.probed(), vec![JOB]);
        assert!(v.gone(JOB));
        assert!(!v.gone(TPUT) && !v.gone(SHELL), "not probed: alive");
        assert_eq!(o.orphaned_by(SHELL, |p| v.gone(p)), Some(JOB));

        let mut o = FgOwners::default();
        o.observe(JOB, MOUSE, 0);
        o.observe(TPUT, MOUSE | FOCUS, 0);
        let mut asked = Vec::new();
        let v = o.probe_edge(SHELL, TPUT, |p, _| {
            asked.push(p);
            p == JOB
        });
        assert_eq!(asked, vec![SHELL, JOB], "`to` (TPUT) is never probed");
        assert!(!v.gone(SHELL) && v.gone(JOB));
        assert_eq!(o.orphaned_by(TPUT, |p| v.gone(p)), Some(JOB));
        let v = FgOwners::default().probe_edge(0, SHELL, |_, _| panic!("no group"));
        assert!(v.probed().is_empty());
    }

    /// The Linux member walk: breadth first from the session leader, at most `TREE_CAP`
    /// processes, `None` (fall back) only when the leader's own children cannot be listed.
    #[test]
    fn foreground_handback_session_tree_walk_is_bounded() {
        assert_eq!(
            parse_children("12 34 0 x -5 56\n").collect::<Vec<_>>(),
            vec![12, 34, 56]
        );
        assert_eq!(parse_children("").count(), 0);
        let tree = |pid: i32| match pid {
            10 => Some(vec![11, 12]),
            12 => Some(vec![13]),
            11 | 13 => Some(vec![]),
            _ => None,
        };
        assert_eq!(tree_any(10, tree, |p| p == 13, TREE_CAP), Some(true));
        assert_eq!(tree_any(10, tree, |_| false, TREE_CAP), Some(false));
        assert_eq!(
            tree_any(10, tree, |p| p == 10, TREE_CAP),
            Some(true),
            "the root too"
        );
        assert_eq!(
            tree_any(99, tree, |_| true, TREE_CAP),
            None,
            "no listing: fall back"
        );
        let gappy = |pid: i32| (pid == 10).then(|| vec![11, 12]);
        assert_eq!(tree_any(10, gappy, |p| p == 12, TREE_CAP), Some(true));
        let mut visits = 0;
        let endless = |pid: i32| Some(vec![pid + 1, pid + 2]);
        assert_eq!(
            tree_any(
                1,
                endless,
                |_| {
                    visits += 1;
                    false
                },
                50
            ),
            Some(false)
        );
        assert_eq!(visits, 50);
    }

    #[cfg(unix)]
    #[test]
    fn foreground_handback_leader_gone_is_esrch_only() {
        assert!(
            !leader_gone(std::process::id() as i32),
            "this process lives"
        );
        assert!(!leader_gone(0) && !leader_gone(-1), "no pid, no verdict");
        assert!(!leader_gone(1));
        let mut child = aterm_uds::spawnfd::command("/bin/sh")
            .arg("-c")
            .arg("exit 0")
            .spawn()
            .expect("spawn sh");
        let pid = child.id() as i32;
        child.wait().expect("reap sh");
        assert!(leader_gone(pid), "a reaped leader is gone");
    }

    /// The orphan rule: a gone leader with a stopped member is a stopped job, with only running
    /// members a killed one; a live leader never asks for the member listing.
    #[test]
    fn foreground_handback_a_gone_leader_with_a_stopped_member_is_not_orphaned() {
        assert!(!group_orphaned(true, || true), "Ctrl-Z on `producer | tui`");
        assert!(
            group_orphaned(true, || false),
            "the incident: MCP children run"
        );
        assert!(
            !group_orphaned(false, || panic!("a live leader needs no listing")),
            "a live leader"
        );
    }

    #[test]
    fn foreground_handback_proc_stat_reads_state_and_group() {
        assert!(proc_stat_is_stopped_in(
            "4321 (sh) T 4000 4300 4000 34816 4000 0",
            4300
        ));
        assert!(
            proc_stat_is_stopped_in("4321 (sh) t 4000 4300 4000", 4300),
            "a trace stop is a stop"
        );
        assert!(!proc_stat_is_stopped_in("4321 (sh) S 4000 4300 4000", 4300));
        assert!(
            !proc_stat_is_stopped_in("4321 (sh) T 4000 4301 4000", 4300),
            "another group's"
        );
        assert!(proc_stat_is_stopped_in(
            "4321 (a) T (b) T 4000 4300 4000",
            4300
        ));
        assert!(!proc_stat_is_stopped_in("garbage", 4300));
        assert!(!proc_stat_is_stopped_in("4321 (sh) T", 4300));
    }

    /// The real process table, a group whose leader exited while a member lives: as the
    /// holder it is gone while the member runs, not while it is stopped, and gone again once
    /// resumed; as a bystander only once its last member is.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn foreground_handback_a_stopped_member_of_a_leaderless_group_is_seen() {
        use super::FgRole::{Bystander, Holder};
        use super::{group_empty, group_gone, group_has_stopped_member};
        use std::time::Duration;

        /// Kills and reaps what it holds, however the test ends.
        struct Reaped(Vec<aterm_uds::spawnfd::Child>);
        impl Drop for Reaped {
            fn drop(&mut self) {
                for child in &mut self.0 {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
        }
        fn until(what: &str, mut done: impl FnMut() -> bool) {
            let seen = aterm_spec::wait::within(
                aterm_spec::wait::MUST_HAPPEN,
                Duration::from_millis(5),
                || done().then_some(()),
            );
            assert!(seen.is_some(), "timed out waiting for {what}");
        }

        let leader = aterm_uds::spawnfd::command("/bin/sleep")
            .arg("60")
            .process_group(0)
            .spawn()
            .expect("spawn the leader");
        let pgid = i32::try_from(leader.id()).expect("a pid");
        let mut held = Reaped(vec![leader]);
        let member = aterm_uds::spawnfd::command("/bin/sleep")
            .arg("60")
            .process_group(pgid)
            .spawn()
            .expect("spawn the member into the leader's group");
        let member_pid = i32::try_from(member.id()).expect("a pid");
        held.0.push(member);

        assert!(!group_has_stopped_member(-1, pgid), "both run");
        assert!(!group_gone(-1, pgid, Holder), "the leader lives");
        assert!(!group_gone(-1, pgid, Bystander) && !group_empty(pgid));

        let mut leader = held.0.remove(0);
        leader.kill().expect("kill the leader");
        leader.wait().expect("reap the leader");
        assert!(leader_gone(pgid));
        assert!(
            group_gone(-1, pgid, Holder),
            "a running member: the incident's shape"
        );
        assert!(
            !group_gone(-1, pgid, Bystander),
            "a leaderless job running in the background is not gone"
        );

        // SAFETY: signals this test's own child by its pid.
        assert_eq!(unsafe { libc::kill(member_pid, libc::SIGSTOP) }, 0);
        until("the member reads as stopped", || {
            group_has_stopped_member(-1, pgid)
        });
        assert!(
            !group_gone(-1, pgid, Holder),
            "a stopped job keeps its modes"
        );
        assert!(!group_gone(-1, pgid, Bystander));

        // SAFETY: signals this test's own child by its pid.
        assert_eq!(unsafe { libc::kill(member_pid, libc::SIGCONT) }, 0);
        until("the member reads as running", || {
            !group_has_stopped_member(-1, pgid)
        });
        assert!(group_gone(-1, pgid, Holder));
        assert!(!group_gone(-1, pgid, Bystander));

        let mut member = held.0.remove(0);
        member.kill().expect("kill the member");
        member.wait().expect("reap the member");
        assert!(group_empty(pgid));
        assert!(group_gone(-1, pgid, Holder) && group_gone(-1, pgid, Bystander));
        assert!(!group_has_stopped_member(-1, 0) && !group_has_stopped_member(-1, -1));
        assert!(
            !group_empty(0) && !group_empty(-1) && !group_empty(1),
            "no group, or the broadcast pid: no verdict"
        );
    }

    /// A member owned by another user reads back (`PROC_PIDTBSDINFO` would refuse it with
    /// `EPERM`).
    #[cfg(target_os = "macos")]
    #[test]
    fn foreground_handback_another_users_member_status_reads_back() {
        let (status, pgid) =
            super::darwin_member_status(1).expect("launchd's status reads across users");
        assert_eq!(pgid, 1, "launchd leads its own group");
        assert!((1..=5).contains(&status), "a p_stat value: {status}");
        let own = i32::try_from(std::process::id()).expect("a pid");
        // SAFETY: getpgrp takes no arguments and cannot fail.
        let own_pgid = unsafe { libc::getpgrp() };
        let (status, pgid) = super::darwin_member_status(own).expect("this process");
        assert_eq!(pgid, own_pgid);
        assert!(matches!(status, 2 | 3), "this process runs: {status}");
    }

    #[test]
    fn foreground_handback_splice_and_payload() {
        assert_eq!(splice_at(b"abcd", 2, b"XY"), b"abXYcd");
        assert_eq!(splice_at(b"abcd", 4, b"XY"), b"abcdXY");
        assert_eq!(splice_at(b"abcd", 0, b"XY"), b"XYabcd");
        let edge = FgEdge::group(3, JOB, SHELL);
        assert_eq!(
            restored_payload(edge, Some("claude"), &["alt", "kitty"], 42),
            "from=200 to=100 program=claude reverted=alt,kitty bytes=42"
        );
        assert_eq!(
            restored_payload(edge, None, &["parser"], 1),
            "from=200 to=100 program=- reverted=parser bytes=1"
        );
    }
}

/// `foreground_handback_ownership` environment actions, each waived where its Tier-1
/// (`foreground_handback_conformance`) binds it.
#[cfg(all(any(test, debug_assertions), unix))]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "foreground_handback_ownership",
    action = "Launch",
    reason = "The shell's `tcsetpgrp` giving a job the terminal: an environment step. The \
              Tier-1 rows move the scripted foreground probe exactly there."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "foreground_handback_ownership",
    action = "Die",
    reason = "A process's death: an environment step. The Tier-1 rows add the job to the \
              scripted liveness probe's dead set exactly there."
)]
#[allow(
    dead_code,
    reason = "carrier for the `spec_unmodeled` waivers above; nothing calls it. An allow, \
              not an expect: rustc does not lint a fn the declarative anchors rebuild"
)]
fn ownership_scope_waivers() {}

/// `foreground_handback` actions no one function of the host performs, each
/// waived where its Tier-1 binds it.
#[cfg(all(any(test, debug_assertions), unix))]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "foreground_handback",
    action = "Launch",
    reason = "zle's `2004l` and the shell's `tcsetpgrp` giving a job the terminal: an \
              environment step. The PTY Tier-1 writes the bytes and moves the scripted \
              foreground probe exactly there; the exec rig moves its scripted `tcgetpgrp`."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "foreground_handback",
    action = "OneShot",
    reason = "A display-only one-shot (`tput smcup`) given the terminal: an environment step, \
              scripted by the PTY Tier-1's one-shot schedule."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "foreground_handback",
    action = "Adopt",
    reason = "A reader attached while a job holds the terminal: the PTY Tier-1's adopted \
              schedule feeds the job's screen first and seeds the probe with the job."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "foreground_handback",
    action = "JobWrite",
    reason = "The job's write to the slave: an environment step the Tier-1 rigs perform."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "foreground_handback",
    action = "Die",
    reason = "The job's death: the PTY Tier-1 adds it to the scripted liveness probe's dead set."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "foreground_handback",
    action = "Stop",
    reason = "Ctrl-Z stopping the job: an environment step of the STOP_FG schedule."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "foreground_handback",
    action = "Reclaim",
    reason = "The shell's `tcsetpgrp` taking the terminal back: the Tier-1 rigs move the \
              scripted foreground exactly there."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "foreground_handback",
    action = "ShellWrite",
    reason = "The shell's post-reclaim output: an environment step the PTY Tier-1 writes."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "foreground_handback",
    action = "Resume",
    reason = "`fg` giving the stopped job the terminal back: an environment step of the \
              STOP_FG schedule."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "foreground_handback",
    action = "Exec",
    reason = "The holder's `execve` and its `NOTE_EXEC`: an environment step. The exec rig \
              marks the job's later bytes the new image's and the watch pending there; \
              `ProcWatch`'s own tests drive a real `exec` through the kernel."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "foreground_handback",
    action = "ReadJob",
    reason = "The gather's `read(2)` of the master inside `aterm_pty::drain_more_nonblocking`, \
              whose polls name the process watch before the master \
              (`the_process_watch_is_heard_before_the_master_in_every_poll`)."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "foreground_handback",
    action = "ReadShell",
    reason = "The same `read(2)` as `ReadJob`, of the shell's bytes behind the job's."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "foreground_handback",
    action = "Parse",
    reason = "The parse stage's slice and handback at the cut (`spawn::ingest` over \
              `Terminal::foreground_handback`): the PTY Tier-1 projects its effect (`leak`, \
              `paste`, `prompt`) after every schedule step."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "foreground_handback",
    action = "Sweep",
    reason = "The UI thread's status sweep, which touches no mode: no schedule needs it to run."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "foreground_handback",
    action = "Restart",
    reason = "`park_reader` then `attach_reader_inner` seeding a new cutter with the carried \
              holder and a new process watch: the PTY Tier-1's PARKED schedule, and the exec \
              rig's new cutter with a quiet watch."
)]
#[allow(
    dead_code,
    reason = "carrier for the `spec_unmodeled` waivers above; nothing calls it. An allow, \
              not an expect: rustc does not lint a fn the declarative anchors rebuild"
)]
fn handback_scope_waivers() {}
