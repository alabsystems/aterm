// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Scripted machine for testing the stuck-process engine ([`World::tick`], [`Jobs::plan`],
//! [`Jobs::absorb`]).

#![allow(dead_code, reason = "each test binary mounting this uses part of it")]

use std::collections::{BTreeMap, BTreeSet, HashMap};

use aterm_vitals::jobs::{
    Block, CallHint, Consumer, Fault, Fd, FdKind, GateWord, InputGate, JobCounters, JobProc,
    JobTab, JobTick, Jobs, JobsView, LanesSeen, Machine, NamedWait, ProcState, SweepProc, TermWord,
    ThreadState, TtyMode,
};

/// The world's clock at the start, ms (any value; the engine reads deltas).
pub const T0: u64 = 5_000_000;
/// aterm's own user.
pub const UID: u32 = 501;

/// One scripted process.
#[derive(Clone, Debug)]
pub struct Proc {
    pub pid: u32,
    pub ppid: u32,
    pub pgid: u32,
    pub sid: u32,
    pub name: String,
    /// Its executable's file name, where the reader gives one.
    pub exe: Option<String>,
    pub state: ProcState,
    /// Its controlling terminal is the tab's.
    pub tty: bool,
    pub uid: u32,
    pub start: u64,
    /// CPU, ns a second.
    pub cpu_per_s: u64,
    /// Reaped children's CPU, ns a second.
    pub child_cpu_per_s: u64,
    /// System calls a second.
    pub syscalls_per_s: u64,
    /// Wakeups a second.
    pub wakeups_per_s: u64,
    /// Logical writes, bytes a second.
    pub writes_per_s: u64,
    /// Page-ins a second.
    pub pageins_per_s: u64,
    pub counters: JobCounters,
    /// Counters refused (another user, or an injected EPERM).
    pub refused: bool,
    /// Descriptors besides the output file.
    pub fds: Vec<Fd>,
    /// Descriptors 1 and 2 on this output file `(dev, ino)`.
    pub out: Option<(u64, u64)>,
    pub wchan: Option<String>,
    /// A thread in an uninterruptible wait.
    pub d_state: bool,
    /// Its one thread never ran past 20 s: `wait` says what keeps it.
    pub held: bool,
    /// What keeps a `held` process from starting: macOS's program check unless set.
    pub wait: aterm_vitals::jobs::StartWait,
    /// Which program the pid runs (the host reads its pid version, which moves on exec).
    pub exec: u64,
    /// A fork that never ran a program of its own (zsh's `zpty` worker).
    pub forked: bool,
    /// Started under aterm though no tab's tree holds it (its resource coalition).
    pub aterm: bool,
}

impl Proc {
    /// A sleeping process in its own group and the given session.
    pub fn new(pid: u32, ppid: u32, name: &str) -> Self {
        Self {
            pid,
            ppid,
            pgid: pid,
            sid: 0,
            name: name.to_owned(),
            exe: None,
            state: ProcState::Sleeping,
            tty: false,
            uid: UID,
            start: u64::from(pid) * 1000 + 7,
            cpu_per_s: 0,
            child_cpu_per_s: 0,
            syscalls_per_s: 0,
            wakeups_per_s: 0,
            writes_per_s: 0,
            pageins_per_s: 0,
            counters: JobCounters {
                cpu_ns: 1_000_000,
                child_cpu_ns: Some(0),
                syscalls: Some(100),
                wakeups: Some(10),
                ctx_switches: Some(10),
                disk_read: Some(0),
                disk_written: Some(0),
                logical_writes: Some(0),
                runnable_ns: Some(0),
                pageins: Some(0),
            },
            refused: false,
            fds: Vec::new(),
            out: None,
            wchan: None,
            d_state: false,
            held: false,
            wait: aterm_vitals::jobs::StartWait::ProgramCheck,
            exec: 1,
            forked: false,
            aterm: false,
        }
    }
}

/// A shared output file: its size, how fast it grows and what it appends.
#[derive(Clone, Copy, Debug)]
pub struct OutFile {
    pub size: u64,
    /// Bytes a second.
    pub per_s: u64,
    /// Appended line-ending state.
    pub lf: Option<bool>,
}

/// One scripted tab.
#[derive(Clone, Debug)]
pub struct Tab {
    pub tab: JobTab,
    /// The terminal's foreground group.
    pub tpgid: u32,
}

/// The scripted machine.
pub struct World {
    pub at: u64,
    pub procs: BTreeMap<u32, Proc>,
    pub files: HashMap<(u64, u64), OutFile>,
    pub tabs: Vec<Tab>,
    pub machine: Machine,
    pub lanes: LanesSeen,
    pub jobs: Jobs,
    pub limit: Option<u64>,
    /// Named pids a scenario keeps.
    pub vars: HashMap<&'static str, u32>,
    /// How long after its clock a tick reads its processes (the shells' terminal groups).
    pub read_lag_ms: u64,
    /// Pids a tick's sweep lists that exit before it reads them: they have no reading.
    pub unread: BTreeSet<u32>,
    /// The group in front of each terminal session nested in a tab's, by its leader.
    pub fronts: HashMap<u32, u32>,
    /// The word of each nested session's terminal aterm's user may open, by its leader.
    pub terms: HashMap<u32, TermWord>,
    looked: HashMap<(u32, u64, u64), u64>,
}

impl Default for World {
    fn default() -> Self {
        Self::new()
    }
}

impl World {
    /// Creates a world with aterm (pid 100) and one shell tab (pid 200).
    pub fn new() -> Self {
        let mut w = Self {
            at: T0,
            procs: BTreeMap::new(),
            files: HashMap::new(),
            tabs: Vec::new(),
            machine: Machine::default(),
            lanes: LanesSeen::default(),
            jobs: Jobs::new(),
            read_lag_ms: 0,
            unread: BTreeSet::new(),
            fronts: HashMap::new(),
            terms: HashMap::new(),
            limit: Some(5333),
            vars: HashMap::new(),
            looked: HashMap::new(),
        };
        let mut aterm = Proc::new(100, 1, "aterm");
        aterm.sid = 100;
        w.add(aterm);
        w.shell_tab(1, 1, 200);
        w
    }

    /// The same world whose engine carries an injected defect.
    pub fn with_fault(mut self, f: Fault) -> Self {
        self.jobs = Jobs::new().with_fault(f);
        self
    }

    /// A tab whose shell `zsh` (`shell` pid) sits at a prompt.
    pub fn shell_tab(&mut self, key: u64, number: u32, shell: u32) {
        let mut sh = Proc::new(shell, 100, "zsh");
        sh.sid = shell;
        sh.tty = true;
        self.add(sh);
        self.tabs.push(Tab {
            tab: JobTab {
                key,
                number,
                shell_pid: shell,
                tty: Some(TtyMode {
                    canonical: true,
                    echo: true,
                    stamped_ms: self.at,
                }),
                input: InputGate {
                    word: GateWord::Clear,
                    bytes: 0,
                    wait_ms: 0,
                },
                ..JobTab::default()
            },
            tpgid: shell,
        });
    }

    pub fn add(&mut self, p: Proc) {
        self.procs.insert(p.pid, p);
    }

    pub fn p(&mut self, pid: u32) -> &mut Proc {
        self.procs.get_mut(&pid).expect("a scripted process")
    }

    pub fn tab(&mut self, key: u64) -> &mut Tab {
        self.tabs
            .iter_mut()
            .find(|t| t.tab.key == key)
            .expect("a scripted tab")
    }

    /// Runs `p` as the foreground job of tab `key` under OSC 133 block `id`; the block it
    /// replaces is the tab's ended one, closed, as the host's feed keeps it.
    pub fn run_fg(&mut self, key: u64, id: u64, command: &str, mut p: Proc) {
        let shell = self.tab(key).tab.shell_pid;
        p.ppid = shell;
        p.sid = shell;
        p.tty = true;
        let pgid = p.pgid;
        self.add(p);
        let at = self.at;
        let t = self.tab(key);
        if let Some(mut b) = t.tab.block.take() {
            b.open = false;
            t.tab.ended = Some(b);
        }
        t.tab.block = Some(Block {
            id,
            started_ms: at,
            open: true,
            command: command.to_owned(),
            status: None,
            stopped: false,
        });
        t.tpgid = pgid;
    }

    /// A process in tab `key`'s session and terminal, not in front.
    pub fn run_bg(&mut self, key: u64, mut p: Proc) {
        let shell = self.tab(key).tab.shell_pid;
        p.ppid = shell;
        p.sid = shell;
        p.tty = true;
        self.add(p);
    }

    /// `D` for the open block: the shell has the terminal again.
    pub fn close_block(&mut self, key: u64, status: i32) {
        let t = self.tab(key);
        if let Some(b) = t.tab.block.as_mut() {
            b.open = false;
            b.status = Some(status);
        }
        t.tpgid = t.tab.shell_pid;
    }

    /// Ctrl-Z: the open block ends in a stop (`D` with 128 plus the host's SIGTSTP, which the
    /// host decodes), and the shell has the terminal again.
    pub fn stop_block(&mut self, key: u64) {
        self.close_block(key, 148);
        if let Some(b) = self.tab(key).tab.block.as_mut() {
            b.stopped = true;
        }
    }

    /// Starts an agent (`claude`, `agent` pid) in front of tab `key`.
    pub fn agent(&mut self, key: u64, agent: u32) {
        if self.procs.contains_key(&agent) {
            return;
        }
        let mut a = Proc::new(agent, 0, "claude");
        a.cpu_per_s = 20_000_000;
        self.run_fg(key, 1, "claude", a);
    }

    /// Starts a detached tool call (`leader`) under `agent` with `child` running the command.
    pub fn call(
        &mut self,
        key: u64,
        agent: u32,
        leader: u32,
        mut child: Proc,
        hint: Option<CallHint>,
    ) {
        self.agent(key, agent);
        let file = (1, u64::from(leader) + 90_000);
        self.files.insert(
            file,
            OutFile {
                size: 0,
                per_s: 0,
                lf: Some(true),
            },
        );
        let mut zsh = Proc::new(leader, agent, "zsh");
        zsh.sid = leader;
        zsh.out = Some(file);
        self.add(zsh);
        child.ppid = leader;
        child.pgid = leader;
        child.sid = leader;
        child.out = Some(file);
        self.add(child);
        if let Some(h) = hint {
            self.tab(key).tab.calls.push(h);
        }
    }

    /// The call's hint, replaced.
    pub fn hint(&mut self, key: u64, h: CallHint) {
        let t = self.tab(key);
        t.tab.calls.retain(|c| c.leader != h.leader);
        t.tab.calls.push(h);
    }

    /// The output file of call `leader`.
    pub fn call_file(&mut self, leader: u32) -> &mut OutFile {
        self.files
            .get_mut(&(1, u64::from(leader) + 90_000))
            .expect("a call's output file")
    }

    /// Tab `key`'s program printed a line now: any running process of the tab may have written it.
    pub fn line(&mut self, key: u64) {
        let writers = self.running_in(key);
        self.line_from(key, &writers);
    }

    /// Tab `key` printed a line now, written by `writers` (each made the write's system call; none
    /// for the line editor's own redraw while the shell is no member).
    pub fn line_from(&mut self, key: u64, writers: &[u32]) {
        self.wrote(writers);
        let at = self.at;
        let t = self.tab(key);
        t.tab.line_output_ms = Some(at);
        t.tab.output_ms = Some(at);
        t.tab.output_bytes += 40;
    }

    /// The running processes of tab `key`: its shell and what runs below it.
    pub fn running_in(&mut self, key: u64) -> Vec<u32> {
        let shell = self.tab(key).tab.shell_pid;
        self.procs
            .values()
            .filter(|p| !matches!(p.state, ProcState::Stopped | ProcState::Zombie))
            .filter(|p| {
                let mut cur = p.pid;
                for _ in 0..self.procs.len() {
                    if cur == shell {
                        return true;
                    }
                    match self.procs.get(&cur) {
                        Some(q) if q.ppid != cur => cur = q.ppid,
                        _ => return false,
                    }
                }
                false
            })
            .map(|p| p.pid)
            .collect()
    }

    fn wrote(&mut self, writers: &[u32]) {
        for w in writers {
            if let Some(s) = self
                .procs
                .get_mut(w)
                .and_then(|p| p.counters.syscalls.as_mut())
            {
                *s += 1;
            }
        }
    }

    /// The person typed into tab `key` now, and `reader` read the keys (a process in a session
    /// nested in the tab's, its relay passing them on).
    pub fn typed(&mut self, key: u64, reader: u32) {
        self.wrote(&[reader]);
        let at = self.at;
        self.tab(key).tab.typed_ms = Some(at);
    }

    /// Tab `key`'s shell wrote one of its own OSC 133 marks now.
    pub fn mark(&mut self, key: u64) {
        let at = self.at;
        self.tab(key).tab.shell_ms = Some(at);
    }

    /// Tab `key`'s program redrew with a CR and no line feed now.
    pub fn cr(&mut self, key: u64) {
        let writers = self.running_in(key);
        self.wrote(&writers);
        let at = self.at;
        let t = self.tab(key);
        t.tab.output_ms = Some(at);
        t.tab.output_bytes += 12;
    }

    /// The terminal mode word, stamped now.
    pub fn tty_mode(&mut self, key: u64, canonical: bool, echo: bool) {
        let at = self.at;
        self.tab(key).tab.tty = Some(TtyMode {
            canonical,
            echo,
            stamped_ms: at,
        });
    }

    /// The unread-input gate's reading.
    pub fn gate(&mut self, key: u64, word: GateWord, bytes: u64, wait_ms: u64) {
        self.tab(key).tab.input = InputGate {
            word,
            bytes,
            wait_ms,
        };
    }

    /// A Tier B naming handed this tick.
    pub fn name(&mut self, key: u64, n: NamedWait) {
        self.tab(key).tab.named.push(n);
    }

    /// Advance the clock by `dt_ms` and take one tick as the host does.
    pub fn tick(&mut self, dt_ms: u64) -> JobsView {
        self.advance(dt_ms);
        self.observe()
    }

    /// Advance the clock and every counter by `dt_ms`.
    pub fn advance(&mut self, dt_ms: u64) {
        self.at += dt_ms;
        for p in self.procs.values_mut() {
            if matches!(p.state, ProcState::Stopped | ProcState::Zombie) {
                continue;
            }
            let c = &mut p.counters;
            c.cpu_ns += p.cpu_per_s * dt_ms / 1000;
            *c.child_cpu_ns.get_or_insert(0) += p.child_cpu_per_s * dt_ms / 1000;
            if let Some(s) = c.syscalls.as_mut() {
                *s += p.syscalls_per_s * dt_ms / 1000;
            }
            *c.wakeups.get_or_insert(0) += p.wakeups_per_s * dt_ms / 1000;
            *c.logical_writes.get_or_insert(0) += p.writes_per_s * dt_ms / 1000;
            *c.pageins.get_or_insert(0) += p.pageins_per_s * dt_ms / 1000;
        }
        for f in self.files.values_mut() {
            f.size += f.per_s * dt_ms / 1000;
        }
    }

    /// Runs one sweep-plan-read-absorb tick at the current clock.
    pub fn observe(&mut self) -> JobsView {
        let sweep: Vec<SweepProc> = self.procs.values().map(sweep_of).collect();
        let tabs: Vec<JobTab> = self.tabs.iter().map(|t| t.tab.clone()).collect();
        let wants = self.jobs.plan(&sweep, &tabs);
        let mut read = Vec::new();
        for w in wants {
            let Some(p) = self
                .procs
                .get(&w.pid)
                .filter(|_| !self.unread.contains(&w.pid))
                .cloned()
            else {
                continue;
            };
            read.push(self.facts_of(&p, w.fds));
        }
        let tick = JobTick {
            at_ms: self.at,
            read_ms: self.at + self.read_lag_ms,
            self_uid: Some(UID),
            sweep,
            procs: read,
            tabs,
            machine: self.machine,
            lanes: self.lanes,
            process_limit: self.limit,
            held: self.held(),
        };
        let view = self.jobs.absorb(&tick);
        for t in &mut self.tabs {
            t.tab.named.clear();
        }
        view
    }

    fn facts_of(&mut self, p: &Proc, want_fds: bool) -> JobProc {
        let zombie = p.state == ProcState::Zombie;
        let tpgid = p
            .tty
            .then(|| {
                self.tabs
                    .iter()
                    .find(|t| self.session_of(p.pid) == Some(t.tab.shell_pid))
                    .map(|t| t.tpgid)
                    .or_else(|| {
                        self.session_of(p.pid)
                            .and_then(|s| self.fronts.get(&s).copied())
                    })
            })
            .flatten();
        let mut fds = Vec::new();
        if want_fds && !zombie {
            fds.extend(p.fds.iter().cloned());
            if let Some(file) = p.out {
                let f = self.files[&file];
                let key = (p.pid, file.0, file.1);
                let last = self.looked.insert(key, f.size);
                let lf_tail = match last {
                    Some(l) if f.size > l => f.lf,
                    _ => None,
                };
                for fd in [1, 2] {
                    fds.push(Fd {
                        fd,
                        kind: FdKind::File {
                            regular: true,
                            size: f.size,
                            dev: file.0,
                            ino: file.1,
                            lf_tail,
                            fresh_dylib: false,
                        },
                    });
                }
            }
        }
        JobProc {
            pid: p.pid,
            ppid: p.ppid,
            pgid: p.pgid,
            sid: (!zombie).then_some(p.sid),
            start: Some(p.start),
            uid: Some(p.uid),
            name: p.name.clone(),
            exe: p.exe.clone().filter(|_| !zombie),
            state: if p.d_state {
                ProcState::Uninterruptible
            } else {
                p.state
            },
            tty: (p.tty && !zombie).then_some(0x1000),
            tpgid,
            term: self
                .terms
                .get(&p.pid)
                .copied()
                .filter(|_| p.sid == p.pid && p.tty && !zombie),
            counters: (!p.refused && !zombie).then_some(p.counters),
            threads: (!zombie).then(|| {
                vec![ThreadState {
                    state: if p.d_state {
                        ProcState::Uninterruptible
                    } else {
                        p.state
                    },
                    wchan: p.wchan.clone(),
                }]
            }),
            wchan: p.wchan.clone(),
            fds: (want_fds && !zombie).then_some(fds),
            program_check: p.held
                && p.wait == aterm_vitals::jobs::StartWait::ProgramCheck
                && !zombie,
        }
    }

    /// Processes owned by `UID` that have not started, each with what keeps it; one is aterm's
    /// when it descends from pid 100 or is a coalition member.
    fn held(&self) -> Vec<aterm_vitals::jobs::HeldProc> {
        self.procs
            .values()
            .filter(|p| p.held && p.uid == UID && p.state != ProcState::Zombie)
            .map(|p| aterm_vitals::jobs::HeldProc {
                pid: p.pid,
                exec: p.exec,
                name: p.exe.clone().unwrap_or_else(|| p.name.clone()),
                aterm: p.aterm || self.descends_from(p.pid, 100),
                wait: p.wait,
            })
            .collect()
    }

    fn descends_from(&self, pid: u32, root: u32) -> bool {
        let mut cur = pid;
        for _ in 0..64 {
            let Some(p) = self.procs.get(&cur) else {
                return false;
            };
            if p.ppid == root {
                return true;
            }
            if p.ppid <= 1 || p.ppid == cur {
                return false;
            }
            cur = p.ppid;
        }
        false
    }

    fn session_of(&self, pid: u32) -> Option<u32> {
        let p = self.procs.get(&pid)?;
        if p.sid != 0 {
            return Some(p.sid);
        }
        None
    }

    /// Kill `pid` (it is reaped at once, or a zombie when `zombie`).
    pub fn exit(&mut self, pid: u32, zombie: bool) {
        if zombie {
            if let Some(p) = self.procs.get_mut(&pid) {
                p.state = ProcState::Zombie;
            }
        } else {
            self.procs.remove(&pid);
        }
    }
}

fn sweep_of(p: &Proc) -> SweepProc {
    let zombie = p.state == ProcState::Zombie;
    SweepProc {
        pid: p.pid,
        ppid: p.ppid,
        pgid: p.pgid,
        uid: Some(p.uid),
        name: p.name.clone(),
        state: if p.d_state {
            ProcState::Uninterruptible
        } else {
            p.state
        },
        session_leader: (!zombie).then_some(p.sid == p.pid),
        has_tty: (!zombie).then_some(p.tty),
        exec: (!zombie).then_some(!p.forked),
    }
}

/// A hint for call `leader`.
pub fn hint(
    leader: u32,
    consumer: Consumer,
    task: Option<&str>,
    promoted_ms: Option<u64>,
) -> CallHint {
    CallHint {
        leader,
        consumer,
        promoted_ms,
        task: task.map(str::to_owned),
        command: None,
    }
}
