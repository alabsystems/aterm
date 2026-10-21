// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A scripted machine for the stuck-process engine: processes with counter
//! rates, descriptors and an output file, tabs with their terminal stamps,
//! and a clock the test steps. Each [`World::tick`] does what the host does:
//! sweep, [`Jobs::plan`], read what the plan asked for (descriptors only when
//! asked), [`Jobs::absorb`]. No clock, no OS.

#![allow(dead_code, reason = "each test binary mounting this uses part of it")]

use std::collections::{BTreeMap, HashMap};

use aterm_vitals::jobs::{
    Block, CallHint, Consumer, Fault, Fd, FdKind, GateWord, InputGate, JobCounters, JobProc,
    JobTab, JobTick, Jobs, JobsView, LanesSeen, Machine, NamedWait, ProcState, SweepProc,
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
    /// fds 1 and 2 on this output file: (dev, ino).
    pub out: Option<(u64, u64)>,
    pub wchan: Option<String>,
    /// A thread in an uninterruptible wait.
    pub d_state: bool,
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
        }
    }
}

/// A shared output file: its size, how fast it grows and what it appends.
#[derive(Clone, Copy, Debug)]
pub struct OutFile {
    pub size: u64,
    /// Bytes a second.
    pub per_s: u64,
    /// What the appended bytes hold: `Some(true)` a line feed, `Some(false)`
    /// only CRs, `None` a file the reader may not open.
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
    /// The size each (pid, file) was last looked at, for the tail check.
    looked: HashMap<(u32, u64, u64), u64>,
}

impl Default for World {
    fn default() -> Self {
        Self::new()
    }
}

impl World {
    /// aterm (pid 100) and one tab (key 1, `tab1`) whose shell is `zsh`
    /// pid 200 at a prompt.
    pub fn new() -> Self {
        let mut w = Self {
            at: T0,
            procs: BTreeMap::new(),
            files: HashMap::new(),
            tabs: Vec::new(),
            machine: Machine::default(),
            lanes: LanesSeen::default(),
            jobs: Jobs::new(),
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

    /// Run `p` as the foreground job of tab `key` under OSC 133 block `id`
    /// (`C` now).
    pub fn run_fg(&mut self, key: u64, id: u64, command: &str, mut p: Proc) {
        let shell = self.tab(key).tab.shell_pid;
        p.ppid = shell;
        p.sid = shell;
        p.tty = true;
        let pgid = p.pgid;
        self.add(p);
        let at = self.at;
        let t = self.tab(key);
        t.tab.block = Some(Block {
            id,
            started_ms: at,
            open: true,
            command: command.to_owned(),
            status: None,
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

    /// An agent (`claude`, `agent` pid) running in front of tab `key`
    /// (block 1), printing nothing yet.
    pub fn agent(&mut self, key: u64, agent: u32) {
        if self.procs.contains_key(&agent) {
            return;
        }
        let mut a = Proc::new(agent, 0, "claude");
        a.cpu_per_s = 20_000_000;
        self.run_fg(key, 1, "claude", a);
    }

    /// A detached call: `zsh -c …` (`leader`, its own session, no terminal)
    /// under the agent, with `child` running the command, fds 1 and 2 on the
    /// call's output file. Returns nothing; the call's id is `a<leader>`.
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

    /// Tab `key`'s program printed a line now.
    pub fn line(&mut self, key: u64) {
        let at = self.at;
        let t = self.tab(key);
        t.tab.line_output_ms = Some(at);
        t.tab.output_ms = Some(at);
        t.tab.output_bytes += 40;
    }

    /// Tab `key`'s program redrew with a CR and no line feed now.
    pub fn cr(&mut self, key: u64) {
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
            *c.syscalls.get_or_insert(0) += p.syscalls_per_s * dt_ms / 1000;
            *c.wakeups.get_or_insert(0) += p.wakeups_per_s * dt_ms / 1000;
            *c.logical_writes.get_or_insert(0) += p.writes_per_s * dt_ms / 1000;
            *c.pageins.get_or_insert(0) += p.pageins_per_s * dt_ms / 1000;
        }
        for f in self.files.values_mut() {
            f.size += f.per_s * dt_ms / 1000;
        }
    }

    /// Take one tick at the current clock as the host does: sweep, plan,
    /// read what the plan asked for, absorb.
    pub fn observe(&mut self) -> JobsView {
        let sweep: Vec<SweepProc> = self.procs.values().map(sweep_of).collect();
        let tabs: Vec<JobTab> = self.tabs.iter().map(|t| t.tab.clone()).collect();
        let wants = self.jobs.plan(&sweep, &tabs);
        let mut read = Vec::new();
        for w in wants {
            let Some(p) = self.procs.get(&w.pid).cloned() else {
                continue;
            };
            read.push(self.facts_of(&p, w.fds));
        }
        let tick = JobTick {
            at_ms: self.at,
            self_uid: Some(UID),
            sweep,
            procs: read,
            tabs,
            machine: self.machine,
            lanes: self.lanes,
            process_limit: self.limit,
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
        }
    }

    /// The session a process's terminal belongs to (the shell it runs under).
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
