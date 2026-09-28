// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE FOREGROUND CUT (2026-09-25): where in the PTY byte stream the
//! foreground process group changed, so the parse thread can hand the terminal
//! back to its host exactly there.
//!
//! The incident: a Claude Code killed while it held the terminal left the alt
//! screen, kitty flags, modifyOtherKeys, mouse tracking, focus reporting, 2026
//! and a hidden cursor armed under the shell that reclaimed it. The tab looked
//! crashed — every mouse move typed `ESC[<32;…M` at the prompt, and zle rang
//! the bell at each CSI-u chord. The engine half of the fix is
//! [`aterm_core::terminal::Terminal::foreground_handback`]; this module is the
//! host half's pure core: the gather thread samples `tcgetpgrp(master)` at
//! every dry-gap park and at every batch end, and [`FgCutter`] turns those
//! samples into at most one [`FgEdge`] per batch.
//!
//! # Why the cut is sound
//!
//! Every sample is taken AFTER the reads it covers, the tty output queue is
//! one FIFO, `tcgetpgrp` reports the group at the moment it is called, and a
//! shell reaps its job and `tcsetpgrp`s itself BEFORE writing its next byte
//! (measured for zsh 5.9 and bash 3.2; fish re-arms its modes only when it next
//! reads input). So if `s` is the last sample that still showed the old group,
//! every byte the NEW holder wrote sits at or after `s`'s offset — the cut. The
//! handback therefore lands before zsh's own `ESC[?2004h`, never after it: a
//! UI-side sweep could not promise that, which is why the handback is ordered
//! in the stream. The derived model is `ForegroundHandback`
//! (`aterm-spec/src/derive/models_foreground_handback.rs`).
//!
//! # Only ORPHANED modes are handed back (the 2026-09-25 review)
//!
//! A foreground change is not a death. Ctrl-Z stops a job and the shell takes
//! the terminal; `gdb -tui` gives the terminal to its inferior on `run` and
//! takes it back at a breakpoint; ssh's `~^Z` suspends a remote vim. Handing
//! back at every change stripped those programs' modes while they still ran,
//! and nothing re-armed them after `fg` or `continue`. So [`FgOwners`]
//! attributes each evidence bit ([`aterm_core::terminal::program_evidence`])
//! to the foreground group whose bytes set it, and the parse stage restores
//! only when one of those groups is GONE ([`group_gone`]): its leader pid
//! answers `ESRCH` ([`leader_gone`]) and no surviving member is stopped
//! ([`group_has_stopped_member`]). The LEADER, not the group: the incident's
//! SIGKILLed Claude Code left same-group children (MCP servers, tool
//! processes) alive, and a group check would have kept the incident. But not
//! the leader ALONE: a pipeline's first stage is its leader, so in
//! `producer | tui` the leader is gone as soon as the producer exits, and
//! Ctrl-Z on the still-running TUI read as a death (the review's live probe,
//! `sleep 0 | sh -c …`). A STOPPED member proves the job lives and will be
//! resumed; the incident's MCP children run, and none is stopped. That rule
//! is for the group that lost the terminal at the edge ([`FgRole::Holder`]),
//! which the incident's killed program always is at its death edge. Any
//! OTHER owner of an input bit ([`FgRole::Bystander`]) counts as gone only
//! when no member of its group survives ([`group_empty`], the third
//! 2026-09-25 review): the stopped leaderless pipeline, `bg`'d, runs on with
//! no member stopped, and the holder rule stripped its modes at the next
//! command the shell ran.
//! Attribution is what keeps `gdb -tui` whole when its inferior exits: the
//! inferior is gone, but the alt screen is gdb's, and gdb lives. A holder that
//! is gone with only a torn escape sequence to its name still gets the `CAN`.
//!
//! # Only a gone group that hijacked INPUT orphans the terminal
//!
//! Gone is not killed: a one-shot command that exits cleanly is gone too, and
//! nothing tells the two apart. The second 2026-09-25 review found `tput
//! civis`, `tput smcup` and `/usr/bin/printf '\e[?1000h'` all reverted the
//! moment the command exited, and `tput smcup; cmd` drawing `cmd` on the main
//! screen. The incident's harm is the input-hijacking modes
//! ([`program_evidence::INPUT`](aterm_core::terminal::program_evidence::INPUT)),
//! so a gone group orphans the terminal only when it owns one of those;
//! display bits alone (alt screen, VT52, hidden cursor, 2026) are kept, and
//! an input owner's death still restores every mode.
//!
//! # Liveness is decided outside the term lock
//!
//! At an edge the parse stage probes, BEFORE it takes the term lock for the
//! slice that starts at the cut, each group the handback can ask about, once
//! ([`FgOwners::probe_edge`]); the locked half reads the verdicts. The probe
//! runs at nearly every edge — after any finished command the old leader is
//! reaped — and on Linux it walks processes, so the review measured it as a
//! lock-path cost. That walk is bounded by the session's own process tree
//! ([`group_has_stopped_member`]).
//!
//! # The holder survives a reader restart
//!
//! The overlap handoff parks every reader and either resumes it or hands the
//! session to the next process; a job can die, and its shell reclaim and draw
//! its prompt, in between. A new reader that seeded its holder from a fresh
//! probe saw the shell as holder from its first sample — no edge, no handback,
//! the incident exactly. So the gather keeps the last holder it saw on the
//! session (`Session::fg_holder`), the handoff carries it on the session's
//! manifest record (`SessionRecord::fg_holder`) whatever rung the screen
//! crosses at, and a new reader's cutter starts from it: a first sample that
//! differs is an edge at offset 0, which is the handoff boundary. (The first
//! cut carried it in the control-carry sidecar, which a Sanitized or Repaint
//! session, a repainted one and one past the aggregate sidecar budget all go
//! without, while still restoring their modes.)
//!
//! Residuals (documented there and in the plan): old-group bytes still unread
//! at the change are parsed after the cut (bounded by one tty queue, needs
//! backpressure); a reap+reclaim inside one µs spin window; a job whose whole
//! life falls between two samples; no foreground change at all (ssh, tmux,
//! `-e`); a stopped job killed while stopped is handed back at the NEXT
//! foreground change, not at its death (nothing samples between); a job
//! whose leader exited and whose RUNNING non-leader hands the terminal to its
//! own child (`cat f | gdb -tui`, then `run`) counts as orphaned, because no
//! member is stopped (a stopped one, Ctrl-Z on `producer | tui`, is not);
//! a bystander ([`FgRole::Bystander`]) whose leader alone was killed while
//! other members of its group run on keeps its modes until the last of them
//! is gone (`kill %1` signals the whole group, so a job killed from the shell
//! is not this); on macOS a member's status is read with
//! `PROC_PIDT_SHORTBSDINFO`, which answers for another user's process (a
//! stopped `producer | sudo tui`), where `PROC_PIDTBSDINFO` is refused;
//! nested programs' modes cannot be told apart (a TUI inferior under
//! `gdb -tui` that dies takes gdb's modes with it, and so does one that arms
//! gdb's mode again); a restarted reader attributes the modes already in
//! force to the carried holder, until a program arms one again; a job whose
//! bytes were all parsed as the shell's (the whole life of `printf
//! '\e[?1000h'` between two samples, or read after the reclaim) loses its
//! own handback — only its own: the next program that arms that mode again
//! owns it ([`FgOwners`], 2026-09-27); a mode the shell arms again over a
//! stopped job's is the shell's from then on; a one-shot
//! that arms an INPUT mode on purpose and exits is handed back, and a program
//! killed with only DISPLAY modes armed (a SIGKILLed `less`) is not; on Linux
//! a stopped member outside the session leader's process tree (reparented
//! away) is not seen.

/// One foreground change inside a gathered batch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FgEdge {
    /// Byte offset in the batch at which the handback runs: after every byte
    /// the old group wrote, before any byte the new group wrote.
    pub(crate) at: usize,
    /// The foreground group that lost the terminal.
    pub(crate) from: i32,
    /// The foreground group that holds it now.
    pub(crate) to: i32,
}

/// Turns the gather's foreground samples into at most one [`FgEdge`] per batch.
///
/// `running` is the last valid sample (`0` = none yet); `cut` is the offset in
/// the current batch at which a sample last equalled `running`.
// Only the unix gather samples a foreground group (ConPTY has none), so on
// Windows the cutter is compiled but never constructed.
#[derive(Debug)]
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) struct FgCutter {
    running: i32,
    cut: usize,
    edge: Option<FgEdge>,
}

#[cfg_attr(not(unix), allow(dead_code))]
impl FgCutter {
    /// A cutter whose holder is the reader's first sample. A reader attached
    /// while a job holds the terminal (an adopted session) starts with the JOB
    /// as holder, so the job's death is still an edge. `first <= 0` (no tty,
    /// ConPTY) means "no holder yet": the first valid sample becomes it.
    pub(crate) fn new(first: i32) -> Self {
        Self {
            running: first.max(0),
            cut: 0,
            edge: None,
        }
    }

    /// Start a new batch: nothing read yet, no edge.
    pub(crate) fn begin(&mut self) {
        self.cut = 0;
        self.edge = None;
    }

    /// A sample `fg` taken after `filled` bytes of this batch were read.
    /// Returns `true` when the batch must be delivered NOW (it carries an
    /// edge): bytes read after this point belong to the new holder and must not
    /// join the batch the edge cuts — the next batch starts at the new holder.
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "foreground_handback_ownership",
            action = "Sample",
            project = "spawn::foreground_handback_conformance::project_ownership"
        )
    )]
    pub(crate) fn sample(&mut self, fg: i32, filled: usize) -> bool {
        if self.edge.is_some() {
            return true;
        }
        if fg <= 0 {
            return false; // no information (tcgetpgrp failed, no foreground group)
        }
        if self.running <= 0 || fg == self.running {
            self.running = fg;
            self.cut = filled;
            return false;
        }
        self.edge = Some(FgEdge {
            at: self.cut,
            from: self.running,
            to: fg,
        });
        self.running = fg;
        true
    }

    /// The batch-end sample (taken only when no park sample already found an
    /// edge) and this batch's edge, if any.
    pub(crate) fn finish(&mut self, fg: impl FnOnce() -> i32, filled: usize) -> Option<FgEdge> {
        if self.edge.is_none() {
            self.sample(fg(), filled);
        }
        self.edge.take()
    }

    /// The current holder (`0` = none seen yet): after [`Self::finish`], the
    /// group whose bytes end the batch — `edge.to` when it carries an edge.
    /// The gather stores it on the session so a restarted reader starts from it.
    pub(crate) fn holder(&self) -> i32 {
        self.running
    }
}

/// Which foreground group armed each evidence bit that is in force — the
/// parse stage's half of "only orphaned modes are handed back" (module docs).
///
/// [`Self::observe`] is called after each run of bytes one holder wrote, with
/// the engine's [`program_evidence`](aterm_core::terminal::Terminal::program_evidence)
/// reading and the bits whose setters those bytes carried
/// ([`take_evidence_asserted`](aterm_core::terminal::Terminal::take_evidence_asserted)):
/// a bit that came on, or that the holder's bytes armed again, is that
/// holder's; a bit that went off belongs to nobody; any other bit that stayed
/// on keeps its owner. Bits already in force at the first observation (an
/// adopted screen, a restarted reader) are the first holder's. `0` is
/// "unknown" (no probe answer) and is never reported gone.
///
/// Why a re-arm moves the owner (2026-09-27, the lane at load 59-65): a
/// starved gather read a one-shot `/usr/bin/printf '\e[?1000h'` only after
/// zsh had taken the terminal back, so its mouse bit was the SHELL's. Losing
/// that one handback is residual R1/R3. But when only a bit that CAME ON moved
/// its owner, every later job that armed mouse tracking over it (`?1003h`: the
/// bit stays on) never owned it, the shell — the new holder at every death
/// edge, and alive — was its only owner, and no later death in that session
/// was handed back again (ten lane rows). zsh's builtin `printf` reached it
/// with no load. The derived model is `ForegroundHandbackOwnership`.
#[derive(Debug, Default)]
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) struct FgOwners {
    owner: [i32; aterm_core::terminal::program_evidence::COUNT],
    last: u16,
}

#[cfg_attr(not(unix), allow(dead_code))]
impl FgOwners {
    /// `holder`'s bytes were just parsed; the engine's evidence is now
    /// `evidence`, and those bytes carried the setters of `asserted`.
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "foreground_handback_ownership",
            action = "Arm",
            project = "spawn::foreground_handback_conformance::project_ownership"
        )
    )]
    #[cfg_attr(
        test,
        aterm_spec::refines(
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

    /// Whether `owner` armed an INPUT-HIJACKING bit that is still in force
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
    /// owners excepted), each once, in bit order. At most
    /// [`program_evidence::COUNT`](aterm_core::terminal::program_evidence::COUNT).
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

    /// The first group that armed an INPUT-HIJACKING bit still in force and
    /// is gone now — `to` (the new holder) and unknown owners excepted. `gone`
    /// is asked at most once per distinct owner.
    ///
    /// A gone group that owns only DISPLAY bits (alt screen, VT52, hidden
    /// cursor, 2026) orphans nothing (the second 2026-09-25 review): that is
    /// a one-shot `tput smcup` or `tput civis` that exited cleanly, and the
    /// `tput smcup; cmd; tput rmcup` wrapper depends on the alt screen
    /// outliving `tput`. A gone group that owns an input bit takes its
    /// display bits with it — the handback restores every mode.
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "foreground_handback_ownership",
            action = "Reclaim",
            project = "spawn::foreground_handback_conformance::project_ownership"
        )
    )]
    pub(crate) fn orphaned_by(&self, to: i32, mut gone: impl FnMut(i32) -> bool) -> Option<i32> {
        self.suspects(to).find(|&owner| gone(owner))
    }

    /// Probe, BEFORE the term lock is taken, every group the handback at the
    /// edge `from → to` can ask about: `from` itself (the torn-sequence `CAN`
    /// asks whether it is gone, and the cut's own [`Self::observe`] can only
    /// attribute a new bit to `from`) and every [`Self::orphaned_by`]
    /// suspect. Each distinct group is probed once, `from` as the
    /// [`FgRole::Holder`] and every other suspect as a [`FgRole::Bystander`]
    /// (the rules differ: [`group_gone`]).
    ///
    /// Why before the lock (the second 2026-09-25 review): the probe is a
    /// `kill(pgid, 0)` and, for a gone leader, a member listing — one
    /// `proc_listpgrppids` on macOS (6.7 µs measured with 561 processes), a
    /// walk of the session's process tree on Linux. It runs at nearly every
    /// foreground edge: after ANY finished command the old holder's leader is
    /// reaped, so the `CAN` check asks about it. Asked under the term lock, the
    /// render and input paths waited on it.
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
            // `suspects` is at most COUNT distinct owners, plus `from`.
            if let Some(slot) = v.asked.get_mut(v.n) {
                *slot = (pgid, gone(pgid, role));
                v.n += 1;
            }
        }
        v
    }
}

/// The liveness verdicts [`FgOwners::probe_edge`] took before the term lock,
/// read under it. A group that was not probed reads as alive: when in doubt
/// the modes stay, which is how the terminal behaved before the handback.
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

    /// How many groups were probed (the tests' once-each check).
    #[cfg(test)]
    pub(crate) fn probed(&self) -> Vec<i32> {
        self.asked().map(|(p, _)| p).collect()
    }
}

/// Which side of a foreground edge a probed group is on: the liveness rule
/// differs ([`group_gone`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) enum FgRole {
    /// The group that lost the terminal at this edge (`from`): gone when its
    /// LEADER is gone and no member is stopped ([`group_orphaned`]). The
    /// incident's killed holder is always this at its death edge.
    Holder,
    /// Any other owner of an input bit (neither `from` nor `to`): gone only
    /// when NO member of its group survives ([`group_empty`]). Its leader
    /// alone proves nothing: a `bg`'d pipeline whose first stage exited runs
    /// on leaderless, with no member stopped.
    Bystander,
}

/// The liveness probe's shape (`SessionFactory::fg_gone`): `(master, pgid,
/// role)`, shipping [`group_gone`]. A plain `fn` so the reader threads copy it.
pub(crate) type FgGone = fn(i32, i32, FgRole) -> bool;

/// The leader half of the shipping liveness probe ([`group_gone`]): whether
/// process group `pgid`'s LEADER no longer exists — `kill(pgid, 0)` fails with
/// `ESRCH`. A zombie leader (not yet reaped), a live one, a pid owned by
/// another user (`EPERM`) and `pgid <= 0` all answer `false`: when in doubt
/// the modes stay, which is how the terminal behaved before the handback.
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

/// THE ORPHAN RULE, pure: a group's modes are orphaned when its LEADER is
/// gone and no surviving member of the group is STOPPED. `member_stopped` is
/// asked only when the leader is gone.
///
/// Why the second half (the 2026-09-25 review's live probe): zsh and bash make
/// a pipeline's FIRST stage its group leader. In `sleep 0 | sh -c 'printf
/// "\e[?1049h\e[?1003h"; sleep 30'` the `sleep 0` leader is reaped at once,
/// so `kill(pgid, 0)` answers `ESRCH` for a job that is alive. Ctrl-Z then
/// read as a death: the stop edge stripped the alt screen and mouse, and `fg`
/// resumed the job without them. A stopped member is proof the job lives and
/// will be resumed, so its modes stay. The incident still qualifies: the
/// SIGKILLed Claude Code's same-group MCP children keep RUNNING, and none of
/// them is stopped.
pub(crate) fn group_orphaned(leader_gone: bool, member_stopped: impl FnOnce() -> bool) -> bool {
    leader_gone && !member_stopped()
}

/// THE LIVENESS RULE, pure, by [`FgRole`]: the edge's [`FgRole::Holder`]
/// is gone by [`group_orphaned`] (leader gone, no member stopped); a
/// [`FgRole::Bystander`] only when its whole group is (`group_empty`). Each
/// fact is asked only when its rule needs it.
///
/// Why a bystander needs the whole group (the third 2026-09-25 review): a
/// pipeline whose first stage (its leader) exited, stopped by Ctrl-Z (kept:
/// a member is stopped) and then `bg`'d, runs on in the background with its
/// leader gone and no member stopped. It still owns its input bits, so it is
/// a suspect at every later edge (shell → `ls`), and the holder rule judged
/// it gone there: its modes were handed back while it ran, and `fg` resumed
/// it without them. A background group cannot be the incident (the incident's
/// program is killed while it HOLDS the terminal, so it is the edge's
/// `from`), and a job killed while stopped or in the background (`kill %1`
/// signals its whole group) is still handed back at the next edge.
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

/// The shipping liveness probe (`SessionFactory::fg_gone`): [`owner_gone`]
/// over the real process table, with [`leader_gone`],
/// [`group_has_stopped_member`] and [`group_empty`]. `master` is the
/// session's PTY master, which the Linux member walk reads the session from.
pub(crate) fn group_gone(master: i32, pgid: i32, role: FgRole) -> bool {
    owner_gone(
        role,
        || leader_gone(pgid),
        || group_has_stopped_member(master, pgid),
        || group_empty(pgid),
    )
}

/// Whether NO process of group `pgid` exists: `kill(-pgid, 0)` fails with
/// `ESRCH`. A group with any member (running, stopped, or another user's:
/// `EPERM`) answers `false`, and so does `pgid <= 1`: `kill(-1, …)` is the
/// broadcast to every process, not group 1, and no job runs in init's group.
/// When in doubt the modes stay. One syscall on every unix, no walk.
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

/// Whether any live process of group `pgid` is STOPPED (job-control stop or a
/// trace stop). Asked only when the group's leader is gone, and only from
/// [`FgOwners::probe_edge`], which runs OUTSIDE the term lock.
///
/// macOS lists the group itself (`proc_listpgrppids`, one syscall plus one
/// `proc_pidinfo` per member). Linux has no group listing, so it walks the
/// process tree under the terminal's SESSION LEADER (`tcgetsid(master)`, the
/// shell): a job's stages are the shell's children, and Ctrl-Z stops every
/// member of the group, so a stopped job has a stopped member there. That
/// walk is bounded by the session's own processes, not the machine's (the
/// second 2026-09-25 review: the first cut walked all of `/proc`, one
/// `open`+`read` per process in the system, at nearly every foreground edge).
/// Only a kernel without `/proc/<pid>/task/<tid>/children`
/// (`CONFIG_PROC_CHILDREN`), or an unknown session, falls back to that full
/// walk.
///
/// A table that cannot be read answers `false`, which leaves the decision to
/// the leader alone, as before this check: the incident (a dead program's
/// modes under the prompt) is the failure that must not come back, and a
/// stopped pipeline losing its modes is the lesser one.
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
            .and_then(|pid| pid.parse::<i32>().ok())
            .is_some_and(stopped)
    })
}

/// Linux: the children of every thread of `pid`
/// (`/proc/<pid>/task/<tid>/children`). `None` when no thread's list can be
/// read — the process is gone, or the kernel lacks `CONFIG_PROC_CHILDREN`.
#[cfg(target_os = "linux")]
fn linux_children(pid: i32) -> Option<Vec<i32>> {
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
/// visiting at most `cap` processes. `None` when `root`'s own children cannot
/// be listed (the caller falls back); a descendant whose list cannot be read
/// has exited and counts as childless.
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
/// status ([`darwin_member_status`], `PROC_PIDT_SHORTBSDINFO`). Both answer
/// for another user's processes.
#[cfg(target_os = "macos")]
fn darwin_group_has_stopped_member(pgid: i32) -> bool {
    // SAFETY (declaration): exported by libproc in libSystem on every
    // supported macOS (`<libproc.h>`) with exactly this signature: a pgid, a
    // buffer of `buffersize` bytes, and the number of pids written (or -1).
    // Declared here rather than in the first-party `libc` shim, which is
    // generated and conformance-checked, never hand-edited.
    unsafe extern "C" {
        fn proc_listpgrppids(
            pgrpid: libc::pid_t,
            buffer: *mut libc::c_void,
            buffersize: libc::c_int,
        ) -> libc::c_int;
    }
    /// `SSTOP` from `<sys/proc.h>` (`SIDL 1, SRUN 2, SSLEEP 3, SSTOP 4, SZOMB
    /// 5`; the shim carries `SZOMB` alone). Pinned by
    /// `foreground_handback_a_stopped_member_of_a_leaderless_group_is_seen`,
    /// which stops a real process and reads it back.
    const SSTOP: u32 = 4;
    /// A foreground job with more members than this is not one a terminal
    /// runs; past it the listing is read as far as it goes.
    const MAX_MEMBERS: usize = 4096;
    let mut capacity = 64_usize;
    let pids = loop {
        let mut pids: Vec<libc::pid_t> = vec![0; capacity];
        let Ok(bytes) = libc::c_int::try_from(capacity * std::mem::size_of::<libc::pid_t>()) else {
            return false;
        };
        // SAFETY: `pids` holds exactly `bytes` bytes and outlives the call,
        // which writes at most that many and answers how many pids it wrote.
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

/// `PROC_PIDT_SHORTBSDINFO` (`<sys/proc_info.h>`). Declared here, as
/// `aterm-sysprobe` declares its own private copy, rather than in the
/// first-party `libc` shim, which is generated and conformance-checked and
/// whose reference `libc` does not export it.
#[cfg(target_os = "macos")]
const PROC_PIDT_SHORTBSDINFO: libc::c_int = 13;

/// `struct proc_bsdshortinfo` (`<sys/proc_info.h>`), 64 bytes.
#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Clone, Copy)]
struct ProcBsdShortInfo {
    pbsi_pid: u32,
    pbsi_ppid: u32,
    pbsi_pgid: u32,
    pbsi_status: u32,
    pbsi_comm: [u8; 16],
    pbsi_flags: u32,
    pbsi_uid: u32,
    pbsi_gid: u32,
    pbsi_ruid: u32,
    pbsi_rgid: u32,
    pbsi_svuid: u32,
    pbsi_svgid: u32,
    pbsi_rfu: u32,
}

#[cfg(target_os = "macos")]
const _: () = assert!(std::mem::size_of::<ProcBsdShortInfo>() == 64);

/// macOS: one process's `(p_stat, pgid)`, `None` when it cannot be read (it
/// exited mid-walk).
///
/// `PROC_PIDT_SHORTBSDINFO`, not `PROC_PIDTBSDINFO`: the long record is
/// refused (`EPERM`) for a pid another user owns, so a stopped member of a
/// `producer | sudo tui` pipeline read as "not stopped" and the stopped job
/// was judged orphaned (the third 2026-09-25 review, measured against
/// launchd: the long flavor answered 0 with `EPERM`, the short one 64 of 64
/// bytes). The short record carries no same-user check, and its
/// `pbsi_status`/`pbsi_pgid` are all this needs. Pinned by
/// `foreground_handback_another_users_member_status_reads_back`.
#[cfg(target_os = "macos")]
fn darwin_member_status(pid: i32) -> Option<(u32, i32)> {
    const SIZE: libc::c_int = std::mem::size_of::<ProcBsdShortInfo>() as libc::c_int;
    let mut info = std::mem::MaybeUninit::<ProcBsdShortInfo>::uninit();
    // SAFETY: `info` points at `SIZE` writable bytes of exactly the
    // structure PROC_PIDT_SHORTBSDINFO fills (size asserted above); libproc
    // returns the bytes written.
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            PROC_PIDT_SHORTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            SIZE,
        )
    };
    if read != SIZE {
        return None;
    }
    // SAFETY: the exact-size success above initialized the whole record.
    let info = unsafe { info.assume_init() };
    Some((info.pbsi_status, i32::try_from(info.pbsi_pgid).ok()?))
}

/// Linux: whether one `/proc/<pid>/stat` line is a STOPPED (`T`, or `t` for a
/// trace stop) member of group `pgid`. The fields after the command's closing
/// parenthesis are `state ppid pgrp …`; the command itself may hold spaces and
/// parentheses, so the split is at the LAST `)`.
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

/// The byte taps' view of a batch with a handback: `buf[..at] ++ handback ++
/// buf[at..]`, one allocation. The cast and `bytes` subscribers then carry the
/// synthesized bytes at the same position the engine processed them, so a cast
/// replayed through a fresh engine reaches the live state.
pub(crate) fn splice_at(buf: &[u8], at: usize, handback: &[u8]) -> Vec<u8> {
    let at = at.min(buf.len());
    let mut out = Vec::with_capacity(buf.len() + handback.len());
    out.extend_from_slice(&buf[..at]);
    out.extend_from_slice(handback);
    out.extend_from_slice(&buf[at..]);
    out
}

/// The `modes-restored` timeline payload:
/// `from=<pgid> to=<pgid> program=<basename|-> reverted=<csv> bytes=<n>`.
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

#[cfg(test)]
mod tests {
    use super::{
        FgCutter, FgEdge, FgOwners, FgRole, TREE_CAP, group_orphaned, leader_gone, parse_children,
        proc_stat_is_stopped_in, restored_payload, splice_at, tree_any,
    };
    use aterm_core::terminal::program_evidence::{
        ALT_SCREEN, COUNT, CURSOR_HIDDEN, FOCUS, INPUT, KITTY, MOUSE, SYNC, VT52,
    };

    const SHELL: i32 = 100;
    const JOB: i32 = 200;

    #[test]
    fn foreground_handback_cutter_starts_from_an_invalid_or_a_valid_sample() {
        // No holder yet: the first valid sample becomes it, with no edge.
        let mut c = FgCutter::new(-1);
        c.begin();
        assert!(!c.sample(JOB, 10));
        assert_eq!(c.finish(|| JOB, 20), None);
        c.begin();
        assert_eq!(
            c.finish(|| SHELL, 5),
            Some(FgEdge {
                at: 0,
                from: JOB,
                to: SHELL
            })
        );

        // A valid first sample is the holder (the adopted-session case).
        let mut c = FgCutter::new(JOB);
        c.begin();
        assert_eq!(
            c.finish(|| SHELL, 7),
            Some(FgEdge {
                at: 0,
                from: JOB,
                to: SHELL
            })
        );
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
            Some(FgEdge {
                at: 4096,
                from: SHELL,
                to: JOB
            })
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
            Some(FgEdge {
                at: 300,
                from: JOB,
                to: SHELL
            })
        );
        // The next batch starts from the new holder with a reset cut.
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
        assert_eq!(
            c.finish(|| SHELL, 30),
            Some(FgEdge {
                at: 10,
                from: SHELL,
                to: JOB
            })
        );
        // The later flip back is the NEXT batch's edge, seen from its first sample.
        c.begin();
        assert_eq!(
            c.finish(|| SHELL, 5),
            Some(FgEdge {
                at: 0,
                from: JOB,
                to: SHELL
            })
        );
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
        // A reader seeded with a carried holder (not a fresh probe) sees the
        // shell that reclaimed the terminal while it was parked as an edge at
        // offset 0 — the handoff boundary.
        let mut c = FgCutter::new(JOB);
        c.begin();
        assert!(c.sample(SHELL, 0));
        assert_eq!(
            c.finish(|| SHELL, 40),
            Some(FgEdge {
                at: 0,
                from: JOB,
                to: SHELL
            })
        );
    }

    #[test]
    fn foreground_handback_owners_attribute_each_bit_to_its_setter() {
        const GDB: i32 = 300;
        const INFERIOR: i32 = 400;
        let dead = |pids: &'static [i32]| move |p: i32| pids.contains(&p);

        // gdb -tui arms the alt screen and mouse; its inferior writes text
        // and exits.
        let mut o = FgOwners::default();
        o.observe(SHELL, 0, 0);
        o.observe(GDB, ALT_SCREEN | MOUSE, 0);
        o.observe(INFERIOR, ALT_SCREEN | MOUSE, 0);
        assert_eq!(
            o.orphaned_by(GDB, dead(&[INFERIOR])),
            None,
            "the alt screen and mouse are gdb's, and gdb lives"
        );
        // The inferior arms focus reports and dies with them on: orphaned.
        o.observe(INFERIOR, ALT_SCREEN | MOUSE | FOCUS, 0);
        assert_eq!(o.orphaned_by(GDB, dead(&[INFERIOR])), Some(INFERIOR));
        // It turns them off first: nothing of its own is left.
        o.observe(INFERIOR, ALT_SCREEN | MOUSE, 0);
        assert_eq!(o.orphaned_by(GDB, dead(&[INFERIOR])), None);
        // gdb is killed: its modes are orphaned.
        assert_eq!(o.orphaned_by(SHELL, dead(&[GDB, INFERIOR])), Some(GDB));

        // The new holder's own bits never count, whatever `gone` says.
        let mut o = FgOwners::default();
        o.observe(JOB, MOUSE, 0);
        assert_eq!(o.orphaned_by(JOB, |_| true), None);
        assert_eq!(o.orphaned_by(SHELL, |_| true), Some(JOB));
        // Every bit cleared: nobody owns anything.
        o.observe(JOB, 0, 0);
        assert_eq!(o.orphaned_by(SHELL, |_| true), None);

        // Bits already in force at the first observation are its holder's
        // (an adopted screen, a restarted reader).
        let mut o = FgOwners::default();
        o.observe(JOB, ALT_SCREEN | MOUSE, 0);
        assert_eq!(o.orphaned_by(SHELL, dead(&[JOB])), Some(JOB));
        // An unknown holder (no probe answer) is never reported gone.
        let mut o = FgOwners::default();
        o.observe(0, MOUSE, 0);
        assert_eq!(o.orphaned_by(SHELL, |_| true), None);
        o.observe(-1, MOUSE | ALT_SCREEN, 0);
        assert_eq!(o.orphaned_by(SHELL, |_| true), None);
    }

    /// The 2026-09-27 lane at load 59-65: a one-shot's `?1000h` parsed as the
    /// SHELL's bytes (the reader never saw the one-shot hold the terminal),
    /// then a job that arms mouse tracking over it and dies. RED on the
    /// replaced rule (a bit that stayed on kept its owner): the shell stayed
    /// the owner, and at every death edge the shell is `to` and alive.
    #[test]
    fn foreground_handback_owners_give_a_re_armed_bit_to_its_re_armer() {
        let dead = |pids: &'static [i32]| move |p: i32| pids.contains(&p);
        let mut o = FgOwners::default();
        o.observe(SHELL, MOUSE, MOUSE);
        assert_eq!(o.orphaned_by(SHELL, |_| true), None, "the shell's own");
        // The job's `?1003h`: the evidence stays MOUSE, the assertion moves it.
        o.observe(JOB, MOUSE, MOUSE);
        assert_eq!(o.orphaned_by(SHELL, dead(&[JOB])), Some(JOB));
        // A kitty push over the shell's kitty flags: the same.
        let mut o = FgOwners::default();
        o.observe(SHELL, KITTY, KITTY);
        o.observe(JOB, KITTY | ALT_SCREEN, KITTY | ALT_SCREEN);
        assert_eq!(o.orphaned_by(SHELL, dead(&[JOB])), Some(JOB));
        // An asserted bit that is not in force (armed and cleared in one
        // slice) is nobody's.
        let mut o = FgOwners::default();
        o.observe(JOB, 0, MOUSE);
        assert_eq!(o.orphaned_by(SHELL, |_| true), None);
        // The shell re-arming a bit over a stopped job's takes it: the value in
        // force is the shell's (fish's kitty push at its prompt), and the
        // job's later death does not strip it.
        let mut o = FgOwners::default();
        o.observe(JOB, KITTY, KITTY);
        o.observe(SHELL, KITTY, KITTY);
        assert_eq!(o.orphaned_by(SHELL, dead(&[JOB])), None);

        // NEGATIVE CONTROL, and the residual: a job that never re-arms the
        // shell's bit does not own it — its death hands back nothing of it.
        let mut o = FgOwners::default();
        o.observe(SHELL, MOUSE, MOUSE);
        o.observe(JOB, MOUSE | ALT_SCREEN, ALT_SCREEN);
        assert_eq!(o.orphaned_by(SHELL, dead(&[JOB])), None);
    }

    /// The second 2026-09-25 review's live probe: `tput civis`, `tput smcup`
    /// and the `tput smcup; cmd` wrapper were reverted the moment `tput`
    /// exited. A gone group that owns only DISPLAY bits orphans nothing; one
    /// that owns an input-hijacking bit orphans all of its bits. RED on the
    /// reviewed rule (every owner of any bit counted).
    #[test]
    fn foreground_handback_a_display_only_one_shot_orphans_nothing() {
        const TPUT: i32 = 500;
        const CMD: i32 = 600;
        assert_eq!(
            INPUT & (ALT_SCREEN | VT52 | CURSOR_HIDDEN | SYNC),
            0,
            "the four display bits are not input evidence"
        );
        assert_eq!(
            INPUT.count_ones() as usize + 4,
            COUNT,
            "every other bit hijacks input"
        );
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
        // `tput smcup; cmd`: the edge is tput → cmd, and the alt screen stays.
        let mut o = FgOwners::default();
        o.observe(SHELL, 0, 0);
        o.observe(TPUT, ALT_SCREEN, 0);
        assert_eq!(o.orphaned_by(CMD, |p| p == TPUT), None);
        // `cmd` arms mouse tracking and is killed: it is orphaned, and the
        // handback (which restores every mode) takes tput's alt screen too.
        o.observe(CMD, ALT_SCREEN | MOUSE, 0);
        assert_eq!(o.orphaned_by(SHELL, |p| p == TPUT || p == CMD), Some(CMD));
        // A one-shot that arms an input bit on purpose is handed back: the
        // documented residual (`/usr/bin/printf '\e[?1000h'`).
        let mut o = FgOwners::default();
        o.observe(TPUT, MOUSE, 0);
        assert_eq!(o.orphaned_by(SHELL, |_| true), Some(TPUT));
        // The incident: an input owner with display bits of its own.
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

    /// The pre-lock probe (the second 2026-09-25 review): every group the
    /// locked handback can ask about is probed once, `from` included even
    /// when it also owns bits (the first cut probed a gone, stopped `from`
    /// twice), the new holder and display-only owners never, and a group
    /// that was not probed reads as alive.
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
        // The locked half reads the verdicts and asks nothing new.
        assert_eq!(o.orphaned_by(SHELL, |p| v.gone(p)), Some(JOB));

        // `from` owns nothing; an older holder owns an input bit (a bit that
        // stays on keeps its owner), and the new holder owns one of its own.
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
        // Invalid groups are never probed.
        let v = FgOwners::default().probe_edge(0, SHELL, |_, _| panic!("no group"));
        assert!(v.probed().is_empty());
    }

    /// The third 2026-09-25 review's L2: a pipeline whose first stage (its
    /// leader) exited is stopped (kept), then `bg`'d: it runs in the
    /// background, leaderless, no member stopped, and still owns its input
    /// bits. At the next edge (shell → `ls`) it is a BYSTANDER, gone only when
    /// no member of its group survives. RED on the holder rule for every
    /// suspect (the reviewed probe), which handed its modes back while it ran.
    #[test]
    fn foreground_handback_a_backgrounded_leaderless_job_is_not_a_death() {
        use super::{FgRole, owner_gone};
        const LS: i32 = 700;
        // The scripted table: JOB's leader is gone, nothing is stopped, and
        // the TUI after the leader still runs (the group is not empty).
        let (mut members_left, stopped) = (true, false);
        let mut o = FgOwners::default();
        o.observe(SHELL, 0, 0);
        o.observe(JOB, MOUSE | ALT_SCREEN, 0);
        o.observe(SHELL, MOUSE | ALT_SCREEN, 0);
        let shipping = |p: i32, role: FgRole, members_left: bool| {
            owner_gone(role, || p == JOB, || stopped, || !members_left)
        };
        let mut roles = Vec::new();
        let v = o.probe_edge(SHELL, LS, |p, role| {
            roles.push((p, role));
            shipping(p, role, members_left)
        });
        assert_eq!(
            roles,
            vec![(SHELL, FgRole::Holder), (JOB, FgRole::Bystander)]
        );
        assert!(!v.gone(JOB), "the backgrounded job runs on");
        assert_eq!(o.orphaned_by(LS, |p| v.gone(p)), None, "its modes stay");
        // The same job as the edge's HOLDER (`fg`, then its last member is
        // killed) is judged by the leader rule, as the incident must be.
        assert!(shipping(JOB, FgRole::Holder, true));
        // `kill %1` in the background empties the group: the next edge hands
        // it back.
        members_left = false;
        let v = o.probe_edge(SHELL, LS, |p, role| shipping(p, role, members_left));
        assert_eq!(o.orphaned_by(LS, |p| v.gone(p)), Some(JOB));
    }

    /// The Linux member walk is bounded by the session's own process tree:
    /// breadth first from the session leader, at most `TREE_CAP` processes,
    /// and `None` (fall back) only when the leader's own children cannot be
    /// listed.
    #[test]
    fn foreground_handback_session_tree_walk_is_bounded() {
        assert_eq!(
            parse_children("12 34 0 x -5 56\n").collect::<Vec<_>>(),
            vec![12, 34, 56]
        );
        assert_eq!(parse_children("").count(), 0);
        // shell 10 → job stages 11, 12; 12 → its own child 13.
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
        // A descendant that exited mid-walk is childless, not a fallback.
        let gappy = |pid: i32| (pid == 10).then(|| vec![11, 12]);
        assert_eq!(tree_any(10, gappy, |p| p == 12, TREE_CAP), Some(true));
        // The cap: an endless tree is visited at most `cap` times.
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
        // pid 1 exists and is not ours: EPERM is alive, not gone.
        assert!(!leader_gone(1));
        // A reaped child's pid is gone.
        let mut child = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("exit 0")
            .spawn()
            .expect("spawn sh");
        let pid = child.id() as i32;
        child.wait().expect("reap sh");
        assert!(leader_gone(pid), "a reaped leader is gone");
    }

    /// The orphan rule (the 2026-09-25 review's pipeline probe): a gone leader
    /// with a stopped member is a stopped job, not a dead one; a gone leader
    /// with only running members is the incident; a live leader is alive and
    /// the member listing is never asked for.
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
        // A command name with spaces and parentheses: split at the LAST `)`.
        assert!(proc_stat_is_stopped_in(
            "4321 (a) T (b) T 4000 4300 4000",
            4300
        ));
        assert!(!proc_stat_is_stopped_in("garbage", 4300));
        assert!(!proc_stat_is_stopped_in("4321 (sh) T", 4300));
    }

    /// The real process table: a group whose leader exited while a member
    /// lives, the review's `sleep 0 | sh -c …` shape. As the edge's HOLDER,
    /// with the member running the group is gone (the incident's shape); with
    /// the member STOPPED it is not (Ctrl-Z); resumed, it is gone again. RED
    /// on the reviewed probe (`leader_gone` alone), which calls the stopped
    /// group gone. As a BYSTANDER (`bg`) it is gone only once its last member
    /// is: RED on the holder rule for every suspect (the third review's L2).
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn foreground_handback_a_stopped_member_of_a_leaderless_group_is_seen() {
        use super::FgRole::{Bystander, Holder};
        use super::{group_empty, group_gone, group_has_stopped_member};
        use std::os::unix::process::CommandExt as _;
        use std::time::{Duration, Instant};

        /// Kills and reaps what it holds, however the test ends.
        struct Reaped(Vec<std::process::Child>);
        impl Drop for Reaped {
            fn drop(&mut self) {
                for child in &mut self.0 {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
        }
        fn until(what: &str, mut done: impl FnMut() -> bool) {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !done() {
                assert!(Instant::now() < deadline, "timed out waiting for {what}");
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        let leader = std::process::Command::new("/bin/sleep")
            .arg("60")
            .process_group(0)
            .spawn()
            .expect("spawn the leader");
        let pgid = i32::try_from(leader.id()).expect("a pid");
        let mut held = Reaped(vec![leader]);
        let member = std::process::Command::new("/bin/sleep")
            .arg("60")
            .process_group(pgid)
            .spawn()
            .expect("spawn the member into the leader's group");
        let member_pid = i32::try_from(member.id()).expect("a pid");
        held.0.push(member);

        assert!(!group_has_stopped_member(-1, pgid), "both run");
        assert!(!group_gone(-1, pgid, Holder), "the leader lives");
        assert!(!group_gone(-1, pgid, Bystander) && !group_empty(pgid));

        // The first stage exits and is reaped: the leader is gone.
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

        // Ctrl-Z reaches the member: the job is stopped, not dead.
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

        // `fg`: it runs again, and with its leader gone it counts as gone.
        // SAFETY: signals this test's own child by its pid.
        assert_eq!(unsafe { libc::kill(member_pid, libc::SIGCONT) }, 0);
        until("the member reads as running", || {
            !group_has_stopped_member(-1, pgid)
        });
        assert!(group_gone(-1, pgid, Holder));
        assert!(!group_gone(-1, pgid, Bystander));

        // The last member dies and is reaped: the group is empty, gone for
        // either role.
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

    /// A member owned by ANOTHER user (a stopped `producer | sudo tui`) must
    /// read back: `PROC_PIDTBSDINFO` refuses a pid that is not the caller's
    /// (`EPERM`), so a stopped root member read as "not stopped" and a
    /// leaderless stopped pipeline was judged orphaned. pid 1 (launchd, root)
    /// is the other user here; run as root this test cannot tell.
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
        let edge = FgEdge {
            at: 3,
            from: JOB,
            to: SHELL,
        };
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
