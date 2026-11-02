// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Leftover processes (`sys left`): the pure judge of one candidate's kernel facts
//! (`docs/DESIGN-leftover-processes-2026-10-16.md` §4). The host reads the facts
//! (`aterm_uds::left`) and the run records (`aterm_uds::runs`); this decides which witness ties a
//! process to a session or a run, which tier that proof reaches, and the words its row prints.
//! Nothing here reads the kernel, the clock or a file. Names are never proof: no rule reads a
//! process's name, and a row prints its executable's word only.

use crate::jobs::{JobKind, JobsView, Verdict, WaitName};

/// Whether a process launchd or init adopted, judged by its launcher's pid version where the
/// kernel keeps one (Apple: launchd is its parent and its launcher's version is not `launchd`'s,
/// launchd's own; unread, nothing is orphaned) and otherwise (Linux) by its parent: init adopted
/// it. Linux keeps no original parent, so a parent older than a run that adopted its member (a
/// subreaper) is read only for a run's groups and id, by that run's own verdict
/// (`aterm_uds::runs::Judge`). The one rule: `aterm_uds::process::Lineage::orphaned` at a floor
/// of 0 is this.
#[must_use]
pub fn orphaned(parent: u32, launcher_version: Option<i32>, launchd: Option<i32>) -> bool {
    match launcher_version {
        Some(version) => parent == 1 && launchd.is_some_and(|v| v != version),
        None => parent == 1,
    }
}

/// Started by launchd itself: launchd is its parent, and its launcher's pid version is launchd's.
/// Such a process is never an orphan, and never a leftover.
#[must_use]
pub fn launchd_started(parent: u32, launcher_version: Option<i32>, launchd: Option<i32>) -> bool {
    parent == 1 && launchd.is_some_and(|v| launcher_version == Some(v))
}

/// A row's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum State {
    /// Held at macOS's program check.
    ProgramCheck,
    /// A live job's process waiting behind one that is held.
    Waiting,
    /// Launchd adopted it, and its owner lives or is unknown.
    Orphaned,
    /// It outlived its owner: a closed tab, or a dead verify run.
    Left,
}

impl State {
    /// The `state=` word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::ProgramCheck => "program-check",
            Self::Waiting => "waiting",
            Self::Orphaned => "orphaned",
            Self::Left => "left",
        }
    }
}

/// Who may end a row (§4.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Tier {
    /// Nobody: it must not be ended from here.
    Never,
    /// Nobody: words and facts, no token.
    Name,
    /// An explicit end of its token.
    Offer,
    /// The next verify run ends it at its start (its record's own A1–A7 verdict).
    Auto,
}

impl Tier {
    /// The `tier=` word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Never => "never",
            Self::Name => "name",
            Self::Offer => "offer",
            Self::Auto => "auto",
        }
    }
}

/// Why a row is listed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Why {
    /// A live descendant, or a live run's or session's own.
    Live,
    /// Adopted while its session lives, or tied to no session or run.
    Orphaned,
    /// Its tab closed, and it was born while the tab lived.
    OwnerGone,
    /// Its verify run's launcher is dead.
    LauncherDead,
    /// It holds a listening socket, or leads its own group with no tty outside every root.
    Daemon,
}

impl Why {
    /// The `why=` word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Orphaned => "orphaned",
            Self::OwnerGone => "owner-gone",
            Self::LauncherDead => "launcher-dead",
            Self::Daemon => "daemon",
        }
    }
}

/// One witness or corroboration that held (§4.1), in the order `proof=` prints them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Proof {
    /// W-group: born into a run's open stage group.
    Group,
    /// W-run: its environment carries a run's id.
    Run,
    /// W-session: its environment, or a group-mate's, names the session.
    Env,
    /// W-writer: an aterm instance proven the writer of its graph entry.
    Writer,
    /// C-tree: its executable is under the run's roots.
    Tree,
    /// C-time: born inside the witness's window.
    Time,
    /// C-coalition: started under this aterm launch (words only).
    Coalition,
    /// W-chain: its live parent chain reaches a live tab's shell (words only).
    Chain,
}

impl Proof {
    /// The word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Group => "group",
            Self::Run => "run",
            Self::Env => "env",
            Self::Writer => "writer",
            Self::Tree => "tree",
            Self::Time => "time",
            Self::Coalition => "coalition",
            Self::Chain => "chain",
        }
    }
}

/// A candidate's lineage (flavors 17 and 18, size-checked; Linux `/proc`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Lin {
    /// Its unique id (`p_uniqueid`; start ticks on Linux).
    pub uniqueid: u64,
    /// Its parent now: `1` once launchd or init adopted it.
    pub parent: u32,
    /// Its process group.
    pub pgid: u32,
    /// Its launcher's pid version, kept through adoption; `None` where the kernel keeps none.
    pub launcher_version: Option<i32>,
    /// Its session's controlling terminal.
    pub tty: Option<u32>,
}

/// The marker variables of one environment: the session it names and the run id it carries.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Marks {
    /// The kernel handed back no environment at all: proof of nothing, either way.
    pub withheld: bool,
    /// `ATERM_PARENT_SESSION_ID`.
    pub session: Option<String>,
    /// `ATERM_VERIFY_RUN`.
    pub run: Option<String>,
}

/// How one verify run's record ties a candidate, as the host read it against that record.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunTie {
    /// The record's id (`<pid>-<birth>`).
    pub id: String,
    /// W-group: born into one of its open groups whose leader check passed, inside the window.
    pub group: bool,
    /// W-run: carries its id, born inside its window.
    pub run: bool,
    /// C-tree: its executable is under the record's roots.
    pub tree: bool,
    /// The run's launcher reads dead on a direct read.
    pub dead: bool,
    /// The record's own A1–A7 verdict is proven: the next verify run ends it.
    pub auto: bool,
}

/// Held at macOS's program check, as the jobs engine names it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Held {
    /// Since the first reading that saw it held, ms.
    pub check_ms: u64,
    /// The tab of the job it is a member of, or the tab a session-mate ties it to.
    pub tab: Option<u64>,
}

/// A live job's process waiting behind a held one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Waiting {
    /// The held process it waits on.
    pub peer: Option<u32>,
    /// Its job's tab.
    pub tab: u64,
}

/// One candidate's facts, read by the host.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    /// Its pid.
    pub pid: u32,
    /// Its parent as the sweep read it.
    pub ppid: u32,
    /// Its lineage; `None` where flavor 17 or 18 could not be read or failed its size check.
    pub lineage: Option<Lin>,
    /// Its identity token (`<pid>@<birth>:<idversion>`), read with the lineage.
    pub token: Option<String>,
    /// Its executable's path; `None` when gone, deleted or unread.
    pub exe: Option<String>,
    /// Its executable is under some record's roots.
    pub exe_under_root: bool,
    /// Its fork age, ms.
    pub started_ms: Option<u64>,
    /// Its CPU time, ms.
    pub cpu_ms: Option<u64>,
    /// Whether its one thread ran (`aterm_uds::process::thread_ran`), or a stand-in holds it
    /// (`Some(false)`); `None` when unread.
    pub thread_ran: Option<bool>,
    /// Held at macOS's program check, as the jobs engine names it.
    pub held: Option<Held>,
    /// Waiting behind a held process.
    pub waiting: Option<Waiting>,
    /// Its own marker variables; never read for a process whose thread has not run.
    pub marks: Option<Marks>,
    /// A group-mate's whose thread ran, read only when its own are withheld (a platform binary).
    pub mate_marks: Option<Marks>,
    /// Whether it holds a listening socket; `None` when the listener table did not read, or its
    /// descriptors could not be matched against it.
    pub listener: Option<bool>,
    /// Whether a sandbox profile holds it (a lab jail's, an agent's sandboxed tool command's);
    /// `None` when the check failed.
    pub sandboxed: Option<bool>,
    /// Whether it is an aterm program launchd keeps by design while what it serves runs: a
    /// tether's watcher or a build's claim holder, known by its executable and verb as doctor's
    /// census knows them. `Some(false)` for any other image; `None` for an aterm image whose verb
    /// could not be read.
    pub by_design: Option<bool>,
    /// Its executable is an aterm program's image.
    pub aterm: bool,
    /// An aterm instance proven the writer of its graph entry.
    pub writer: bool,
    /// It shares this aterm launch's coalition.
    pub coalition: bool,
    /// The live tab whose shell its live parent chain reaches.
    pub chain: Option<u64>,
    /// How each record of this boot ties it.
    pub runs: Vec<RunTie>,
    /// A live record claims it (it is a live run's launcher, in its open group, or carries its
    /// id; unread or withheld markers claim it while a live run has an id).
    pub claimed: bool,
    /// Whether a live process holds the master of its tty; `None` when unread.
    pub tty_master_held: Option<bool>,
}

/// A live tab the reading saw.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LiveTab {
    /// The host's key for its session.
    pub key: u64,
    /// Its session id.
    pub sid: String,
    /// Its shell's pid.
    pub shell_pid: u32,
    /// Its shell's unique id, read once; `None` until read.
    pub shell_uniqueid: Option<u64>,
    /// Its foreground job's process group, where the jobs engine names one.
    pub fg_pgid: Option<u32>,
    /// The lowest unique id among the agent calls seen in it: its agent had started by then.
    pub agent_since: Option<u64>,
}

/// A tab this instance saw close: its shell and the horizon its window closes at (§3.2). Kept in
/// memory only.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClosedTab {
    /// The host's key for its session.
    pub key: u64,
    /// Its session id.
    pub sid: String,
    /// Its label (`Tab 2`).
    pub label: String,
    /// Its tab number in its window, where it had one.
    pub number: Option<usize>,
    /// Its shell's unique id: the window opens after it.
    pub shell_uniqueid: u64,
    /// The highest unique id of this uid's table at the reading that missed the tab: the window
    /// closes there, inclusive.
    pub horizon: u64,
    /// When that reading was, unix ms.
    pub closed_ms: u64,
    /// The lowest unique id among the agent calls seen in it while it lived.
    pub agent_since: Option<u64>,
    /// The processes it left that the newest reading offers a token for (the rows `end` takes):
    /// a daemon, a held program and a NEVER row it left are listed, never counted.
    pub left: usize,
}

/// What every candidate of a reading is judged against.
#[derive(Clone, Copy, Debug)]
pub struct Ctx<'a> {
    /// launchd's pid version.
    pub launchd: Option<i32>,
    /// The live tabs.
    pub live: &'a [LiveTab],
    /// The tabs this instance saw close.
    pub closed: &'a [ClosedTab],
    /// Pids never ended from here (this instance, its ancestors, its keeper).
    pub spared: &'a [u32],
    /// Pids an agent's call of a live session left, with that session's id ([`tellers`]).
    pub tellers: &'a [(u32, String)],
}

/// Where a row came from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Source {
    /// Nothing ties it.
    #[default]
    Unknown,
    /// A live tab, by its session key.
    Tab(u64),
    /// A closed tab, by its session key.
    Closed(u64),
    /// A verify run, by its record's id.
    Run(String),
}

/// One `proc` row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeftRow {
    /// Its pid.
    pub pid: u32,
    /// Its state.
    pub state: State,
    /// For `waiting`: the held process it waits on.
    pub peer: Option<u32>,
    /// Held since the first reading that saw it held, ms.
    pub check_ms: Option<u64>,
    /// Its fork age, ms.
    pub started_ms: Option<u64>,
    /// Its CPU time, ms.
    pub cpu_ms: Option<u64>,
    /// The executable judged, which an end compares for a tier with no C-tree.
    pub exe_judged: Option<String>,
    /// Who may end it.
    pub tier: Tier,
    /// Why it is listed.
    pub why: Why,
    /// The witnesses and corroborations that held, whatever the tier.
    pub proof: Vec<Proof>,
    /// It holds a listening socket.
    pub listen: bool,
    /// Where it came from.
    pub from: Source,
    /// The live session whose agent's call left it, which may be told of it.
    pub teller: Option<String>,
    /// Its token, for a tier an end takes (`offer`, `auto`); `None` otherwise.
    pub token: Option<String>,
    /// It is in a live session's foreground group: that is `signal`'s to end, never `end`'s.
    pub foreground: bool,
}

impl LeftRow {
    /// Its executable's word: the judged path's file name when it matches
    /// `^[A-Za-z0-9._+-]{1,32}$`, else `-`.
    #[must_use]
    pub fn exe_word(&self) -> &str {
        self.exe_judged
            .as_deref()
            .and_then(|p| std::path::Path::new(p).file_name())
            .and_then(|n| n.to_str())
            .map_or("-", crate::sys::exe_word)
    }
}

/// One run record of this boot.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunRow {
    /// Its id.
    pub id: String,
    /// Its launcher reads dead.
    pub dead: bool,
    /// Since a reader first found it dead, ms.
    pub dead_ms: Option<u64>,
    /// The session it rode in.
    pub sid: Option<String>,
    /// Its roots, the worktree first.
    pub roots: Vec<String>,
    /// The rows it ties.
    pub left: usize,
    /// The next verify run ends a row of it.
    pub next_verify: bool,
}

/// One recorded dangling link.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LinkRow {
    /// The root it is under, 1-based.
    pub root: usize,
    /// What kind of link (`target`).
    pub kind: String,
    /// Who saw it (`disk`).
    pub seen: String,
}

/// What one reading says was left.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LeftView {
    /// The `proc` rows, by pid.
    pub rows: Vec<LeftRow>,
    /// The run records of this boot.
    pub runs: Vec<RunRow>,
    /// The recorded dangling links.
    pub links: Vec<LinkRow>,
    /// The tabs this instance saw close, oldest first.
    pub closed: Vec<ClosedTab>,
    /// The live tabs the reading judged against: what `end` judges a row again against.
    pub live: Vec<LiveTab>,
    /// The sweep listed every process (it stopped short of its cap).
    pub complete: bool,
    /// The reading stopped at its bound of new candidates: some rows wait for a later one.
    pub bounded: bool,
}

/// The session W-session ties a candidate to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tie {
    /// The host's key for that session.
    pub key: u64,
    /// Its tab closed: it is in this instance's closed-tab records.
    pub closed: bool,
    /// The candidate was born at or after the first agent call seen in that tab, so after its
    /// agent started.
    pub after_agent: bool,
}

/// The session a candidate's environment, or a group-mate's, names, while that session lived
/// (W-session): orphaned ([`orphaned`]), born after the shell, and at or before the horizon once
/// the tab closed. The first environment that is not withheld speaks, its
/// own before a mate's; a withheld one is no proof either way, and an empty id names nothing.
#[must_use]
pub fn session_of(f: &Facts, ctx: &Ctx<'_>) -> Option<Tie> {
    let lin = f.lineage.as_ref()?;
    let named = [f.marks.as_ref(), f.mate_marks.as_ref()]
        .into_iter()
        .flatten()
        .find(|m| !m.withheld)?
        .session
        .as_deref()
        .filter(|sid| !sid.is_empty())?;
    let in_window = |shell: u64, horizon: Option<u64>| {
        lin.uniqueid > shell
            && horizon.is_none_or(|h| lin.uniqueid <= h)
            && orphaned(lin.parent, lin.launcher_version, ctx.launchd)
    };
    let after = |since: Option<u64>| since.is_some_and(|agent| lin.uniqueid >= agent);
    if let Some(t) = ctx.live.iter().find(|t| t.sid == named) {
        return t
            .shell_uniqueid
            .filter(|&shell| in_window(shell, None))
            .map(|_| Tie {
                key: t.key,
                closed: false,
                after_agent: after(t.agent_since),
            });
    }
    ctx.closed
        .iter()
        .rev()
        .find(|t| t.sid == named && in_window(t.shell_uniqueid, Some(t.horizon)))
        .map(|t| Tie {
            key: t.key,
            closed: true,
            after_agent: after(t.agent_since),
        })
}

/// The pids an agent's call of a live session left behind, each with that session's id: the
/// leader of each live orphan unit the jobs engine made from an agent call (`JobRow::orphan_of`)
/// whose tab is one of `live`. A person's `nohup` survivor, an orphan of a shell job, has none.
#[must_use]
pub fn tellers(view: &JobsView, live: &[LiveTab]) -> Vec<(u32, String)> {
    view.rows
        .iter()
        .filter(|r| {
            r.kind == JobKind::Orphan
                && r.orphan_of == Some(JobKind::AgentCall)
                && r.verdict != Verdict::Exited
        })
        .filter_map(|r| {
            let tab = live.iter().find(|t| t.key == r.tab)?;
            Some((r.leader.0, tab.sid.clone()))
        })
        .collect()
}

/// The foreground job's process group of each live tab, as the jobs view names it.
#[must_use]
pub fn foreground_groups(view: &JobsView) -> Vec<(u64, u32)> {
    view.rows
        .iter()
        .filter(|r| r.kind == JobKind::Shell && r.verdict != Verdict::Exited)
        .map(|r| (r.tab, r.pgid))
        .collect()
}

/// The programs macOS's program check holds, as the jobs view names them, each since its first
/// sighting and with the tab of its job or the one a session-mate ties it to. `age_ms` is the
/// view's age at the reading that reads it (`Snapshot::jobs_age_ms`): a reading the engine did
/// not run on carries the last pass's view, whose `for_ms` are as of that pass, so each is told
/// on by it to the reading's time.
#[must_use]
pub fn held_of(view: &JobsView, age_ms: u64) -> Vec<(u32, Held)> {
    view.findings
        .program_check
        .iter()
        .map(|n| {
            (
                n.pid,
                Held {
                    check_ms: n.for_ms.saturating_add(age_ms),
                    tab: n.tab,
                },
            )
        })
        .collect()
}

/// The live jobs' processes that wait behind a held one: the leader of a job whose wait is the
/// program check of another of its members, and the process whose pipe's peer is held.
#[must_use]
pub fn waiting_of(view: &JobsView) -> Vec<(u32, Waiting)> {
    let held: Vec<u32> = view.findings.program_check.iter().map(|n| n.pid).collect();
    view.rows
        .iter()
        .filter(|r| r.verdict != Verdict::Exited)
        .filter_map(|r| {
            let w = r.wait.as_ref()?;
            let (pid, peer) = if w.name == WaitName::ProgramCheck && r.leader.0 != w.leaf {
                (r.leader.0, w.leaf)
            } else {
                (w.leaf, w.peer.filter(|p| held.contains(p))?)
            };
            (!held.contains(&pid)).then_some((
                pid,
                Waiting {
                    peer: Some(peer),
                    tab: r.tab,
                },
            ))
        })
        .collect()
}

/// Whether `view` offers a row whose owner is gone (a closed tab's, a dead run's): such a view is
/// read again within the jobs engine's slow cadence, so the row's next reading comes without an
/// edge.
#[must_use]
pub fn holds_an_offer(view: &LeftView) -> bool {
    view.rows.iter().any(|r| {
        matches!(r.tier, Tier::Offer | Tier::Auto)
            && matches!(r.from, Source::Closed(_) | Source::Run(_))
    })
}

/// Judges one candidate: `None` when it is no leftover (a live process under a live owner that
/// is neither held nor waiting behind one) or launchd started it. The rules, in order:
/// NEVER (a live run's member, a program launchd keeps by design, a spared pid, a live
/// session's foreground group, a tty whose master a live process holds, an instance whose
/// launcher lives); an unreadable lineage is NAME; a held program is NAME with no token (O4);
/// then the witnesses (W-group or W-run with C-tree, W-session, W-writer), the owner gone (O2)
/// and the daemon shape (O3) decide OFFER, and the record's own A1–A7 verdict AUTO. A read that
/// did not answer proves nothing, so a row whose listener table, sandbox check or aterm verb did
/// not read is offered no token, and neither is one a sandbox profile holds: a lab's jail member
/// is told from an agent's sandboxed command by nothing read here, and either is named. An aterm
/// program W-session alone ties is named too: a window is a person's to close. Every witness that
/// held is in `proof`, whatever the tier.
#[must_use]
pub fn judge(f: &Facts, ctx: &Ctx<'_>) -> Option<LeftRow> {
    let lin = f.lineage.as_ref();
    if lin.is_some_and(|l| launchd_started(l.parent, l.launcher_version, ctx.launchd)) {
        return None;
    }
    let adopted = lin.map_or(f.ppid == 1, |l| {
        orphaned(l.parent, l.launcher_version, ctx.launchd)
    });
    let held = f.held.is_some() || f.thread_ran != Some(true);
    let session = session_of(f, ctx);
    let writer = f.writer && adopted;
    let ties: Vec<&RunTie> = f.runs.iter().filter(|t| t.group || t.run).collect();
    let mut proof = Vec::new();
    for t in &ties {
        if t.group {
            proof.push(Proof::Group);
        }
        if t.run {
            proof.push(Proof::Run);
        }
        if t.tree {
            proof.push(Proof::Tree);
        }
        proof.push(Proof::Time);
    }
    if session.is_some() {
        proof.extend([Proof::Env, Proof::Time]);
    }
    if writer {
        proof.push(Proof::Writer);
    }
    if f.coalition {
        proof.push(Proof::Coalition);
    }
    if f.chain.is_some() {
        proof.push(Proof::Chain);
    }
    proof.sort_unstable();
    proof.dedup();

    // The row's run: the one whose verdict made it AUTO, else the first dead one that ties it.
    let dead_tie = ties
        .iter()
        .copied()
        .find(|t| t.auto)
        .or_else(|| ties.iter().copied().find(|t| t.dead));
    // A dead run's member its witness ties that runs its build: offered however it is shaped
    // (O3), since the run that made it is gone.
    let run_proof = ties.iter().any(|t| t.tree && t.dead);
    // Daemon-shaped (§4.2): it holds a listening socket, unless it is an agent's own (W-session
    // ties it to a session whose agent had started before it was born: a dev server) and no aterm
    // program's, whose listener is a window's or an instance's control socket or a broker's; or it
    // leads its own group with no tty, its executable outside every recorded root (`trustd`,
    // `ssh-agent`). A listener table that did not read rules no listener out.
    let agents = session.is_some_and(|t| t.after_agent) && !f.aterm;
    let listens = !agents && f.listener == Some(true);
    let listen_unread = !agents && f.listener.is_none();
    let leads = lin.is_some_and(|l| l.pgid == f.pid && l.tty.is_none() && !f.exe_under_root);
    let daemon = listens || leads;
    let closed = session.is_some_and(|t| t.closed);
    let owner_gone = dead_tie.is_some() || closed;
    let foreground = lin.is_some_and(|l| {
        ctx.live
            .iter()
            .any(|t| t.fg_pgid.is_some_and(|g| g == l.pgid))
    });
    let never = f.claimed
        || f.by_design == Some(true)
        || ctx.spared.contains(&f.pid)
        || foreground
        || f.tty_master_held == Some(true)
        || (f.writer && !adopted);
    if !(held || f.waiting.is_some() || adopted || owner_gone) {
        return None;
    }
    let why = if never {
        Why::Live
    } else if daemon && !run_proof {
        Why::Daemon
    } else if dead_tie.is_some() {
        Why::LauncherDead
    } else if closed {
        Why::OwnerGone
    } else if session.is_some() || writer || adopted {
        Why::Orphaned
    } else {
        Why::Live
    };
    let tty_free = lin.is_some_and(|l| l.tty.is_none()) || f.tty_master_held == Some(false);
    let read = f.sandboxed == Some(false) && f.by_design.is_some();
    // W-session alone ties no aterm program to a token: a window is a person's to close, and
    // nothing read here tells one from aterm's other programs. An executable that did not read
    // is no proof it is not one, and leaves `end` nothing to hold the row to.
    let session_offer = session.is_some() && f.exe.is_some() && !f.aterm;
    let offered = (run_proof || session_offer || writer)
        && (run_proof || !(daemon || listen_unread))
        && tty_free
        && read;
    let tier = if never {
        Tier::Never
    } else if lin.is_none() || held || f.token.is_none() {
        Tier::Name
    } else if ties.iter().any(|t| t.auto) {
        Tier::Auto
    } else if offered {
        Tier::Offer
    } else {
        Tier::Name
    };
    let state = if f.held.is_some() {
        State::ProgramCheck
    } else if f.waiting.is_some() {
        State::Waiting
    } else if owner_gone {
        State::Left
    } else {
        State::Orphaned
    };
    let from = match (session, dead_tie.or(ties.first().copied())) {
        (Some(t), _) if t.closed => Source::Closed(t.key),
        (_, Some(t)) if t.dead => Source::Run(t.id.clone()),
        (Some(t), _) => Source::Tab(t.key),
        (None, Some(t)) => Source::Run(t.id.clone()),
        (None, None) => f
            .chain
            .or(f.waiting.map(|w| w.tab))
            .or(f.held.and_then(|h| h.tab))
            .map_or(Source::Unknown, Source::Tab),
    };
    Some(LeftRow {
        pid: f.pid,
        state,
        peer: f.waiting.and_then(|w| w.peer),
        check_ms: f.held.map(|h| h.check_ms),
        started_ms: f.started_ms,
        cpu_ms: f.cpu_ms,
        exe_judged: f.exe.clone(),
        tier,
        why,
        proof,
        listen: f.listener == Some(true),
        from,
        teller: ctx
            .tellers
            .iter()
            .find(|(pid, _)| *pid == f.pid)
            .map(|(_, sid)| sid.clone()),
        token: matches!(tier, Tier::Offer | Tier::Auto)
            .then(|| f.token.clone())
            .flatten(),
        foreground,
    })
}

#[cfg(test)]
#[path = "left_tests.rs"]
mod tests;
