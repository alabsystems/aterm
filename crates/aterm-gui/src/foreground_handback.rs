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
    pub(crate) const fn begin(&mut self) {
        self.cut = 0;
        self.clean = 0;
        self.parked = None;
        self.edge = None;
    }

    /// The park after the last pre-park sample ended without the watch: that
    /// sample's offset is clean.
    const fn settle(&mut self) {
        if let Some(parked) = self.parked.take() {
            self.clean = parked;
        }
    }

    /// Whether this batch already carries its one edge (a process event heard now waits for the next batch, or goes byte-free after this one).
    pub(crate) const fn carries_edge(&self) -> bool {
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
    pub(crate) const fn take_edge(&mut self) -> Option<FgEdge> {
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
    pub(crate) const fn holder(&self) -> i32 {
        self.running
    }
}

/// Which foreground group armed each evidence bit in force: the parse stage's half of the
/// handback rule, which restores the modes only once a group that armed an input-hijacking bit
/// still in force is gone.
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

    /// The death edge the gather never sampled: the session's shell (`shell`) holds this batch,
    /// and a group that armed an input-hijacking bit still in force has no member left, though no
    /// edge from it was cut. A job `fg` resumed that writes nothing has its hold read only if a
    /// sample lands between the shell's `tcsetpgrp` and the job's death; a reader slower than
    /// that sees the shell before and after. The edge from that group into the shell, at the
    /// batch's first byte. `gone` is the bystander rule (no member of the group survives), so a
    /// stopped or `bg`'d group, even one whose leader exited, keeps its modes; the handback at
    /// this edge judges `from` by that rule too ([`Self::probe_edge`]'s `from_role`), or a leader
    /// pid the system gave to a process outside the dead group would keep its modes armed and
    /// cut this edge again on every batch the shell writes.
    pub(crate) fn missed_reclaim(
        &self,
        holder: i32,
        shell: i32,
        gone: impl FnMut(i32) -> bool,
    ) -> Option<FgEdge> {
        if shell <= 0 || holder != shell {
            return None;
        }
        self.orphaned_by(holder, gone)
            .map(|owner| FgEdge::group(0, owner, holder))
    }

    /// Probe, before the term lock is taken, every group the handback at the edge `from → to` can
    /// ask about: `from` by `from_role` (the [`FgRole::Holder`] rule for a sampled edge, the
    /// [`FgRole::Bystander`] rule [`Self::missed_reclaim`] found it gone by for a missed one), every
    /// other suspect as a bystander.
    pub(crate) fn probe_edge(
        &self,
        from: i32,
        from_role: FgRole,
        to: i32,
        mut gone: impl FnMut(i32, FgRole) -> bool,
    ) -> FgVerdicts {
        let mut v = FgVerdicts::default();
        for pgid in core::iter::once(from).chain(self.suspects(to)) {
            if pgid <= 0 || pgid == to || v.asked().any(|(p, _)| p == pgid) {
                continue;
            }
            let role = if pgid == from {
                from_role
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
        // SAFETY: signal 0 performs only the existence and permission checks
        // and delivers nothing; `pgid > 0` names one process, never a group.
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
        // SAFETY: signal 0 performs only the existence and permission checks
        // and delivers nothing; `-pgid` with `pgid > 1` names that one group.
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
        aterm_uds::process::read_proc_text(std::path::Path::new(&format!("/proc/{pid}/stat")))
            .is_ok_and(|stat| proc_stat_is_stopped_in(&stat, pgid))
    };
    let sid = if master >= 0 {
        // SAFETY: TIOCGSID reads the session id of the terminal `master`
        // names (a pty master answers for its slave); an fd that is not a
        // terminal answers -1 and writes nothing.
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
            .filter(|pid| !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()))
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
        if let Ok(list) = aterm_uds::process::read_proc_text(&task.path().join("children")) {
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
        .filter(|pid| !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()))
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

/// macOS: list the group ([`darwin_group_members`]), then read each member's
/// status ([`darwin_member_status`], `PROC_PIDT_SHORTBSDINFO`).
#[cfg(target_os = "macos")]
fn darwin_group_has_stopped_member(pgid: i32) -> bool {
    /// `SSTOP` from `<sys/proc.h>`.
    const SSTOP: u32 = aterm_uds::process::BsdShortInfo::SSTOP;
    darwin_group_members(pgid).is_some_and(|pids| {
        pids.into_iter()
            .any(|pid| darwin_member_status(pid) == Some((SSTOP, pgid)))
    })
}

/// Cap on the members [`darwin_group_members`] lists for one group.
#[cfg(target_os = "macos")]
pub(crate) const DARWIN_GROUP_MEMBERS_MAX: usize = 4096;

/// macOS: the positive pids of group `pgid` (`proc_listpgrppids`), the first
/// [`DARWIN_GROUP_MEMBERS_MAX`] of a group that has that many or more. Empty both for a group with
/// no member and for a listing libproc refused, which it answers with no pid; `None` only for a
/// negative answer.
#[cfg(target_os = "macos")]
pub(crate) fn darwin_group_members(pgid: i32) -> Option<Vec<libc::pid_t>> {
    // SAFETY (declaration): libproc in libSystem exports this with exactly this signature
    // (`<libproc.h>`): a pgid, a buffer of `buffersize` bytes, and the number of pids written (0
    // also when it refuses the listing).
    unsafe extern "C" {
        fn proc_listpgrppids(
            pgrpid: libc::pid_t,
            buffer: *mut libc::c_void,
            buffersize: libc::c_int,
        ) -> libc::c_int;
    }
    let mut capacity = 64_usize;
    let pids = loop {
        let mut pids: Vec<libc::pid_t> = vec![0; capacity];
        let bytes = libc::c_int::try_from(capacity * std::mem::size_of::<libc::pid_t>()).ok()?;
        // SAFETY: `pids` holds exactly `bytes` bytes and outlives the call, which writes at most that many and answers how many pids it wrote.
        let n = unsafe { proc_listpgrppids(pgid, pids.as_mut_ptr().cast(), bytes) };
        let n = usize::try_from(n).ok()?;
        if n < capacity || capacity >= DARWIN_GROUP_MEMBERS_MAX {
            pids.truncate(n.min(capacity));
            break pids;
        }
        capacity *= 4;
    };
    Some(pids.into_iter().filter(|pid| *pid > 0).collect())
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
    tl.record("modes-restored");
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

#[cfg(test)]
#[path = "foreground_handback_tests.rs"]
mod tests;
