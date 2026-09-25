// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test-only: the strain engine's invariants (design §10.14, rulings
//! 206–212) — FELT, HEAVY with its hysteresis, NAMED over the grouped
//! sweep, NOT OWN, the episode's state machine, the quiet periods, the record
//! cap, the closed words, and the level meter on the glass.

use std::collections::HashMap;

use super::*;
use crate::animate::{Anim, Look};
use crate::center::{EchoKind, MessageCenter, Outcome};
use crate::glass::Links;
use crate::log::MessageLog;
use crate::model::{Restatement, WallStamp};
use crate::text::{char_width, glass_title_fault, title_words};
use crate::{FILL_GLIDE, GLINT_PERIOD};

const CORES: u16 = 8;
const MEM_MIB: u32 = 16_384;
const SELF_PID: u32 = 50;
const OPEN: Gate = Gate {
    enabled: true,
    focused_on_screen: true,
};

fn cfg() -> StrainConfig {
    StrainConfig {
        services_noun: MACOS_SERVICES,
        glass: true,
        self_pid: SELF_PID,
    }
}

fn job_id() -> MessageId {
    MessageId::from_raw(77).unwrap()
}

#[derive(Clone)]
struct Proc {
    row: ProcRow,
    /// Milli-cores it burns while the world holds.
    mc: u32,
}

fn proc(pid: u32, ppid: u32, name: &str, mc: u32) -> Proc {
    Proc {
        row: ProcRow {
            pid,
            ppid,
            uid_is_ours: true,
            name: name.into(),
            bundle: None,
            cpu_ns: Some(0),
            footprint_kib: Some(20 * 1024),
        },
        mc,
    }
}

fn app(pid: u32, name: &str, mc: u32, gib: u64) -> Proc {
    let mut p = proc(pid, 1, name, mc);
    p.row.bundle = Some(format!("/Applications/{name}.app/Contents/MacOS/{name}"));
    p.row.footprint_kib = Some(gib * 1024 * 1024);
    p
}

/// Another user's process: the kernel refuses its time (EPERM).
fn eperm(pid: u32, name: &str) -> Proc {
    Proc {
        row: ProcRow {
            pid,
            ppid: 1,
            uid_is_ours: false,
            name: String::new(),
            bundle: Some(format!("/System/{name}")),
            cpu_ns: None,
            footprint_kib: None,
        },
        mc: 0,
    }
}

/// aterm, its two sessions' shells, and some of macOS.
fn base() -> Vec<Proc> {
    vec![
        proc(SELF_PID, 1, "aterm", 100),
        proc(100, SELF_PID, "zsh", 0),
        proc(200, SELF_PID, "zsh", 0),
        eperm(30, "WindowServer"),
        eperm(31, "mds_stores"),
    ]
}

/// `yes >/dev/null` twelve times under `shell`.
fn yes12(shell: u32) -> Vec<Proc> {
    (1..=12)
        .map(|i| proc(shell + i, shell, "yes", 660))
        .collect()
}

fn sessions() -> Vec<SessionRef> {
    vec![
        SessionRef {
            shell_pid: 100,
            program: "zsh".into(),
            tab: 1,
            receiving_keys: true,
            elsewhere: false,
        },
        SessionRef {
            shell_pid: 200,
            program: "yes".into(),
            tab: 2,
            receiving_keys: false,
            elsewhere: false,
        },
    ]
}

#[derive(Clone)]
struct World {
    busy_pm: u16,
    procs: Vec<Proc>,
    pressure: Option<MemoryLevel>,
    mem_used_pm: Option<u16>,
    swap_kib_s: u64,
    thermal: Option<Thermal>,
    low_power: Option<bool>,
    blind: bool,
}

impl World {
    fn calm() -> Self {
        Self {
            busy_pm: 250,
            procs: base(),
            pressure: Some(MemoryLevel::Normal),
            mem_used_pm: Some(600),
            swap_kib_s: 0,
            thermal: Some(Thermal::Nominal),
            low_power: Some(false),
            blind: false,
        }
    }

    fn loaded(extra: Vec<Proc>, busy_pm: u16) -> Self {
        let mut w = Self::calm();
        w.busy_pm = busy_pm;
        w.procs.extend(extra);
        w
    }
}

struct Sim {
    t: StrainTracker,
    base: Instant,
    ms: u64,
    busy: u64,
    total: u64,
    swap_kib_ms: u64,
    cpu: HashMap<u32, u64>,
    sessions: Vec<SessionRef>,
    job: Option<u32>,
    gate: Gate,
    auto: bool,
    outs: Vec<(u64, StrainOut)>,
}

impl Sim {
    fn new() -> Self {
        Self::with(cfg())
    }

    fn with(cfg: StrainConfig) -> Self {
        Self {
            t: StrainTracker::new(cfg),
            base: Instant::now(),
            ms: 0,
            busy: 0,
            total: 0,
            swap_kib_ms: 0,
            cpu: HashMap::new(),
            sessions: sessions(),
            job: None,
            gate: OPEN,
            auto: true,
            outs: Vec::new(),
        }
    }

    fn at(&self, ms: u64) -> Instant {
        self.base + Duration::from_millis(ms)
    }

    fn rows(&self, w: &World) -> Vec<ProcRow> {
        w.procs
            .iter()
            .map(|p| ProcRow {
                cpu_ns: p
                    .row
                    .cpu_ns
                    .map(|_| self.cpu.get(&p.row.pid).copied().unwrap_or(0)),
                ..p.row.clone()
            })
            .collect()
    }

    fn reading(&self, w: &World, now: Instant) -> Reading {
        if w.blind {
            return Reading {
                cores: CORES,
                mem_mib: MEM_MIB,
                ..Reading::empty(now)
            };
        }
        Reading {
            at: now,
            cores: CORES,
            mem_mib: MEM_MIB,
            busy_ticks: Some((self.busy, self.total)),
            pressure: w.pressure,
            mem_used_pm: w.mem_used_pm,
            mem_used_mib: w.mem_used_pm.map(|pm| u32::from(pm) * MEM_MIB / 1000),
            swap_pages: Some(self.swap_kib_ms / 1000 / 16),
            page_kib: 16,
            thermal: w.thermal,
            low_power: w.low_power,
            psi: None,
        }
    }

    /// One reading now (with a sweep when the tracker wants one).
    fn sample(&mut self, w: &World) -> StrainOut {
        let now = self.at(self.ms);
        if self.t.wants_scan() {
            let rows = self.rows(w);
            let jobs: Vec<JobRef<'_>> = self
                .job
                .map(|pid| JobRef {
                    pid,
                    id: job_id(),
                    title: "Installing ALab tools",
                })
                .into_iter()
                .collect();
            self.t.scanned(now, &rows, &self.sessions, &jobs);
        }
        let r = self.reading(w, now);
        let out = self.t.observe(&r, self.gate);
        if out != StrainOut::None {
            self.outs.push((self.ms, out.clone()));
        }
        out
    }

    /// Advance to `to` ms in 10 ms steps under `w`: a key every `key.0` ms
    /// with lag `key.1`; a reading whenever the tracker asks (when `auto`).
    fn run(&mut self, to: u64, w: &World, key: Option<(u64, u32)>) {
        while self.ms < to {
            self.ms += 10;
            let dt = 10;
            let cores = u64::from(CORES);
            self.total += cores * dt;
            self.busy += cores * dt * u64::from(w.busy_pm) / 1000;
            self.swap_kib_ms += w.swap_kib_s * dt;
            for p in &w.procs {
                *self.cpu.entry(p.row.pid).or_default() += u64::from(p.mc) * dt * 1000;
            }
            let now = self.at(self.ms);
            if let Some((every, lag)) = key
                && self.ms % every == 0
            {
                self.t.note_key(now, lag);
            }
            if self.auto
                && let Some(d) = self.t.next_sample(now, self.gate)
                && d <= now
            {
                self.sample(w);
            }
        }
    }

    fn posts(&self) -> Vec<(u64, Message)> {
        self.outs
            .iter()
            .filter_map(|(t, o)| match o {
                StrainOut::Post(m) => Some((*t, m.clone())),
                _ => None,
            })
            .collect()
    }

    fn folds(&self) -> Vec<(u64, Option<Message>)> {
        self.outs
            .iter()
            .filter_map(|(t, o)| match o {
                StrainOut::Fold { record } => Some((*t, record.clone())),
                _ => None,
            })
            .collect()
    }

    fn records(&self) -> Vec<Message> {
        self.outs
            .iter()
            .filter_map(|(_, o)| match o {
                StrainOut::Record(m) | StrainOut::Fold { record: Some(m) } => Some(m.clone()),
                _ => None,
            })
            .collect()
    }

    fn restates(&self) -> Vec<(u64, Message)> {
        self.outs
            .iter()
            .filter_map(|(t, o)| match o {
                StrainOut::Restate(m) => Some((*t, m.clone())),
                _ => None,
            })
            .collect()
    }
}

/// Typing: a key every 150 ms (6.7 a second, a steady typist).
const EVERY: u64 = 150;
/// A key under load.
const SLOW: u32 = 120;
/// A key on this m1 today: its measured p95.
const FAST: u32 = 23;

/// The machine as it was when the spec was written: `fileproviderd` at 1.3
/// cores, a load average of 4.4 (never an input), keys at their p95.
#[test]
fn todays_snapshot_raises_nothing() {
    let mut s = Sim::new();
    let mut w = World::loaded(vec![proc(400, 1, "fileproviderd", 1300)], 450);
    w.mem_used_pm = Some(700);
    s.run(120_000, &w, Some((EVERY, FAST)));
    assert!(s.outs.is_empty(), "{:?}", s.outs);
    assert_eq!(s.t.state_word(), "calm");
    assert_eq!(s.t.next_sample(s.at(s.ms), OPEN), None);
}

#[test]
fn a_lone_hitch_never_leaves_calm() {
    let mut s = Sim::new();
    s.run(3000, &World::calm(), Some((EVERY, FAST)));
    s.t.note_freeze(s.at(3000), 900, "gpu_present");
    s.run(6000, &World::calm(), Some((EVERY, FAST)));
    assert_eq!(s.t.state_word(), "calm");
    assert!(s.outs.is_empty());
    // ONE stall is one hitch however it is seen (review 2026-09-24): a 900
    // ms freeze with a key typed into it — the key's arrival-to-present
    // slice waited out that same turn — and two keys typed into one 600 ms
    // stall, freeze and all, never leave calm.
    s.run(10_000, &World::calm(), Some((EVERY, FAST)));
    s.t.note_freeze(s.at(10_000), 900, "window_event");
    s.t.note_key(s.at(10_010), 900);
    assert_eq!(
        s.t.state_word(),
        "calm",
        "a key inside a freeze is that freeze"
    );
    s.run(20_000, &World::calm(), Some((EVERY, FAST)));
    s.t.note_key(s.at(20_010), 600);
    s.t.note_key(s.at(20_010), 450);
    s.t.note_freeze(s.at(20_000), 600, "window_event");
    assert_eq!(s.t.state_word(), "calm", "two keys in one stall");
    assert!(s.outs.is_empty(), "{:?}", s.outs);
    // A short turn is no hitch at all; two long ones in the window are FELT.
    s.run(30_000, &World::calm(), Some((EVERY, FAST)));
    s.t.note_freeze(s.at(30_000), FREEZE_MS - 1, "x");
    s.t.note_freeze(s.at(30_500), FREEZE_MS - 1, "x");
    assert!(!s.t.felt(s.at(30_500)));
    s.t.note_freeze(s.at(31_000), FREEZE_MS, "x");
    assert_eq!(s.t.state_word(), "calm", "one hitch in the window");
    s.t.note_freeze(s.at(31_500), FREEZE_MS, "x");
    assert_eq!(s.t.state_word(), "suspect", "two stalls");
}

#[test]
fn calm_schedules_no_sample() {
    let t = StrainTracker::new(cfg());
    let now = Instant::now();
    assert_eq!(t.next_sample(now, OPEN), None);
    assert!(!t.wants_scan());
    assert_eq!(t.verdict(), None);
    // A suspect tracker behind a closed gate arms nothing either.
    let mut s = Sim::new();
    s.auto = false;
    s.run(4000, &World::calm(), Some((EVERY, SLOW)));
    assert_eq!(s.t.state_word(), "suspect");
    let now = s.at(s.ms);
    assert_eq!(s.t.next_sample(now, OPEN), Some(now), "the first at once");
    for gate in [
        Gate {
            enabled: false,
            focused_on_screen: true,
        },
        Gate {
            enabled: true,
            focused_on_screen: false,
        },
    ] {
        assert_eq!(s.t.next_sample(now, gate), None, "{gate:?}");
    }
}

#[test]
fn posts_after_onset_and_two_heavy_readings_not_before() {
    let mut s = Sim::new();
    let w = World::loaded(yes12(200), 1000);
    s.run(60_000, &w, Some((EVERY, SLOW)));
    let posts = s.posts();
    assert_eq!(posts.len(), 1, "{:?}", s.outs);
    let (at, msg) = &posts[0];
    assert_eq!(msg.title, "Typing slowed by yes in tab 2");
    // Suspect at the third slow key's window (6 keys, 0.9 s); the row no
    // sooner than STRAIN_ONSET after it.
    assert!(*at >= 900 + 5000, "{at}");
    assert!(
        *at <= 900 + 5000 + 100,
        "the reading lands on the onset: {at}"
    );
    let meter = msg.meter.as_ref().unwrap();
    assert!(meter.level && meter.fill_permille == Some(1000));
    assert_eq!(meter.load, Some(Load::Cpu));
    assert_eq!(meter.stats, "8.0 of 8 cores");
    assert_eq!(
        msg.hold,
        Hold::Live {
            stale_after: STALE_STRAIN
        }
    );
    assert_eq!(msg.key.as_deref(), Some(STRAIN_KEY));
    assert_eq!(msg.tag, tags::SYSTEM);
    assert!(!msg.excerpt, "no excerpt: every fact waits behind Details");
    assert!(
        msg.detail[0].starts_with("top: yes in tab 2 7.9 cores"),
        "{:?}",
        msg.detail
    );
    assert!(msg.actions.is_empty(), "no capsule (ruling 101)");

    // One heavy reading is not HEAVY: a two-second spike under slow typing
    // raises nothing.
    let mut s = Sim::new();
    s.run(4000, &World::calm(), Some((EVERY, SLOW)));
    let spike = World::loaded(yes12(200), 1000);
    let at = s.ms;
    s.run(at + 2000, &spike, Some((EVERY, SLOW)));
    s.run(30_000, &World::calm(), Some((EVERY, SLOW)));
    assert!(s.posts().is_empty(), "{:?}", s.outs);
}

/// A kind enters after two readings past its line and clears only after
/// three past its clear line; a reading between the lines resets the run.
#[test]
fn clears_only_after_three_readings() {
    let mut s = Sim::new();
    s.auto = false;
    let heavy = World::loaded(yes12(200), 1000);
    let calm = World::calm();
    let mid = World::loaded(yes12(200), 700);
    s.run(1000, &calm, Some((EVERY, SLOW)));
    let step = |s: &mut Sim, w: &World| {
        let to = s.ms + 2000;
        s.run(to, w, Some((EVERY, SLOW)));
        s.sample(w);
        s.t.verdict()
    };
    assert_eq!(step(&mut s, &heavy), None, "the baseline");
    assert_eq!(step(&mut s, &heavy), None, "one past the line");
    assert_eq!(step(&mut s, &heavy), Some(StrainKind::Cpu), "two");
    assert_eq!(step(&mut s, &calm), Some(StrainKind::Cpu), "one clear");
    assert_eq!(step(&mut s, &calm), Some(StrainKind::Cpu), "two clear");
    assert_eq!(
        step(&mut s, &mid),
        Some(StrainKind::Cpu),
        "between the lines resets"
    );
    assert_eq!(step(&mut s, &calm), Some(StrainKind::Cpu));
    assert_eq!(step(&mut s, &calm), Some(StrainKind::Cpu));
    assert_eq!(step(&mut s, &calm), None, "three in a row");
}

#[test]
fn missing_fields_never_imply_heavy() {
    let mut s = Sim::new();
    let mut w = World::loaded(yes12(200), 1000);
    w.blind = true;
    s.run(60_000, &w, Some((EVERY, SLOW)));
    assert_eq!(s.t.verdict(), None);
    assert!(s.posts().is_empty(), "{:?}", s.outs);
    // The episode ends as FELT with nothing heavy.
    s.run(80_000, &w, Some((EVERY, FAST)));
    let recs = s.records();
    assert_eq!(recs.len(), 1, "{:?}", s.outs);
    assert!(
        recs[0].title.starts_with("Typing slow for "),
        "{}",
        recs[0].title
    );
    assert!(recs[0].title.ends_with(", no heavy cause"));
}

/// Busy time aterm cannot read — another user's daemons — is `macOS
/// services`, never a daemon's name.
#[test]
fn eperm_time_is_services_never_a_daemon() {
    let mut s = Sim::new();
    let w = World::loaded(vec![], 950);
    s.run(30_000, &w, Some((EVERY, SLOW)));
    let posts = s.posts();
    assert_eq!(posts.len(), 1, "{:?}", s.outs);
    let msg = &posts[0].1;
    assert_eq!(msg.title, "Typing slowed by macOS services");
    let all = format!("{} {}", msg.title, msg.detail.join(" "));
    for daemon in ["mds", "WindowServer", "kernel"] {
        assert!(!all.contains(daemon), "{all}");
    }
    // Off macOS the noun is the platform's.
    let other = StrainConfig {
        services_noun: SYSTEM_SERVICES,
        ..cfg()
    };
    assert_eq!(
        title(StrainKind::Cpu, &Culprit::Services, &other),
        "Typing slowed by system services"
    );
}

#[test]
fn named_only_past_one_core_and_forty_percent() {
    let h = |c: Culprit, mc: u32| Held {
        culprit: c,
        mc,
        mib: 0,
    };
    let p = |n: &str| Culprit::Process(n.into());
    // 0.9 cores: under a core.
    assert_eq!(lead_cpu(&[h(p("a"), 900)], 1000), Some((p("a"), false)));
    // 1.2 cores of 7.2 busy: 17 %.
    assert_eq!(
        lead_cpu(&[h(p("a"), 1200), h(p("b"), 1000)], 7200),
        Some((p("a"), false))
    );
    // 3 of 6: half the busy time.
    assert_eq!(lead_cpu(&[h(p("a"), 3000)], 6000), Some((p("a"), true)));
    // Exactly at both lines.
    assert_eq!(lead_cpu(&[h(p("a"), 1000)], 2500), Some((p("a"), true)));
    // Memory: 20 % of the RAM.
    let m = |mib| Held {
        culprit: p("a"),
        mc: 0,
        mib,
    };
    assert_eq!(lead_mem(&[m(3000)], MEM_MIB).unwrap().1, false);
    assert_eq!(lead_mem(&[m(3300)], MEM_MIB).unwrap().1, true);

    // End to end: eight lone processes at 0.9 cores each fill the machine,
    // none is named, and the title names the resource.
    let mut s = Sim::new();
    let many: Vec<Proc> = (0..8)
        .map(|i| proc(500 + i, 1, &format!("worker{i}"), 950))
        .collect();
    s.run(30_000, &World::loaded(many, 990), Some((EVERY, SLOW)));
    let posts = s.posts();
    assert_eq!(posts.len(), 1, "{:?}", s.outs);
    assert_eq!(posts[0].1.title, "Typing slowed by CPU load");
    assert_eq!(
        posts[0].1.meter.as_ref().unwrap().load,
        None,
        "the load slot only beside a named culprit"
    );
}

#[test]
fn own_command_and_own_job_are_records() {
    // `yes` in the tab the person types into: their own command.
    let mut s = Sim::new();
    s.run(
        40_000,
        &World::loaded(yes12(100), 1000),
        Some((EVERY, SLOW)),
    );
    assert!(s.posts().is_empty(), "{:?}", s.outs);
    s.run(60_000, &World::calm(), Some((EVERY, FAST)));
    let recs = s.records();
    assert_eq!(recs.len(), 1, "{:?}", s.outs);
    assert!(
        recs[0]
            .title
            // What burns there, not the idle shell that holds the tab.
            .starts_with("Typing slowed by yes in tab 1 for "),
        "{}",
        recs[0].title
    );
    assert_eq!(recs[0].hold, Hold::LogOnly);
    assert!(
        recs[0]
            .detail
            .contains(&"not shown: your own command".to_string()),
        "{:?}",
        recs[0].detail
    );

    // An aterm job that already has a row: its load words explain it.
    let mut s = Sim::new();
    s.job = Some(300);
    let mut job = vec![proc(300, SELF_PID, "atpkg", 50)];
    job.extend((1..=8).map(|i| proc(300 + i, 300, "cc", 950)));
    s.run(40_000, &World::loaded(job, 1000), Some((EVERY, SLOW)));
    assert!(s.posts().is_empty(), "{:?}", s.outs);
    s.run(60_000, &World::calm(), Some((EVERY, FAST)));
    let recs = s.records();
    assert_eq!(recs.len(), 1, "{:?}", s.outs);
    assert!(
        recs[0]
            .detail
            .contains(&"not shown: explained by Installing ALab tools".to_string()),
        "{:?}",
        recs[0].detail
    );
    assert!(recs[0].title.starts_with("Typing slowed by CPU load for "));
}

#[test]
fn aterm_is_never_the_named_cause() {
    let mut s = Sim::new();
    let mut w = World::calm();
    w.busy_pm = 1000;
    w.procs[0].mc = 7900;
    s.run(40_000, &w, Some((EVERY, SLOW)));
    assert!(s.posts().is_empty(), "{:?}", s.outs);
    s.run(60_000, &World::calm(), Some((EVERY, FAST)));
    let recs = s.records();
    assert_eq!(recs.len(), 1, "{:?}", s.outs);
    // The record names aterm (a defect to fix) with its cores — never
    // `no heavy cause`, which would say nothing was heavy.
    assert!(
        recs[0].title.starts_with("Typing slowed by aterm for "),
        "{}",
        recs[0].title
    );
    assert!(
        recs[0].detail[0].starts_with("top: aterm 7."),
        "{:?}",
        recs[0].detail
    );
    assert!(
        recs[0]
            .detail
            .contains(&"not shown: aterm itself".to_string())
    );
    // An aterm bundle is aterm too, wherever its parent is.
    let mut helper = proc(900, 1, "aterm-helper", 0);
    helper.row.bundle = Some("/Applications/aterm.app/Contents/MacOS/aterm-helper".into());
    assert_eq!(
        group(&[helper.row.clone()], &[], &[], 0)[0].culprit,
        Culprit::Aterm
    );
    assert_eq!(
        title(StrainKind::Cpu, &Culprit::Aterm, &cfg()),
        "Typing slowed by CPU load"
    );
}

#[test]
fn felt_without_heavy_records_the_turn_owner() {
    let mut s = Sim::new();
    let w = World::calm();
    s.run(2000, &w, Some((EVERY, FAST)));
    s.t.note_freeze(s.at(2000), 480, "gpu_present");
    s.t.note_freeze(s.at(2700), 610, "session_restore");
    assert_eq!(s.t.state_word(), "suspect");
    s.run(8000, &w, Some((EVERY, SLOW)));
    s.run(30_000, &w, Some((EVERY, FAST)));
    assert_eq!(s.t.state_word(), "calm");
    let recs = s.records();
    assert_eq!(recs.len(), 1, "{:?}", s.outs);
    let r = &recs[0];
    assert!(r.title.starts_with("Typing slow for "), "{}", r.title);
    assert!(
        r.detail
            .contains(&"turn: session_restore 610 ms".to_string()),
        "{:?}",
        r.detail
    );
    assert!(r.detail.contains(&"not shown: no heavy cause".to_string()));
    assert!(r.detail.iter().any(|l| l.starts_with("typing: ")));
}

#[test]
fn culprit_swap_needs_two_scans() {
    let mut s = Sim::new();
    let yes = World::loaded(yes12(200), 1000);
    s.run(10_000, &yes, Some((EVERY, SLOW)));
    assert_eq!(s.posts().len(), 1);
    // Now Chrome takes the machine and `yes` stops.
    let chrome = World::loaded(vec![app(700, "Google Chrome", 7800, 2)], 1000);
    let start = s.ms;
    s.run(start + 4000, &chrome, Some((EVERY, SLOW)));
    // The first sweep after the change: Chrome leads once.
    let titles: Vec<String> = s.restates().iter().map(|(_, m)| m.title.clone()).collect();
    assert!(
        titles.iter().all(|t| t == "Typing slowed by yes in tab 2"),
        "{titles:?}"
    );
    s.run(start + 14_000, &chrome, Some((EVERY, SLOW)));
    let swapped: Vec<(u64, Message)> = s
        .restates()
        .into_iter()
        .filter(|(_, m)| m.title == "Typing slowed by Google Chrome")
        .collect();
    assert!(!swapped.is_empty(), "{:?}", s.outs);
    assert!(
        swapped[0].0 >= start + 6000,
        "two sweeps: {}",
        swapped[0].0 - start
    );
    // The sweep that straddled the change credited `yes`'s vanished time to
    // the services once; one lead is not a swap.
    assert!(
        s.restates()
            .iter()
            .all(|(_, m)| m.title != "Typing slowed by macOS services"),
        "{:?}",
        s.outs
    );
    assert_eq!(s.posts().len(), 1, "a swap is a restate in place");
}

#[test]
fn only_escalation_breaks_quiet() {
    let mut s = Sim::new();
    let yes = World::loaded(yes12(200), 1000);
    let calm = World::calm();
    s.run(20_000, &yes, Some((EVERY, SLOW)));
    s.run(40_000, &calm, Some((EVERY, FAST)));
    assert_eq!(s.posts().len(), 1);
    assert_eq!(s.folds().len(), 1);
    let fold = s.folds()[0].0;
    // The same cause a minute later: quiet (3 min any, 15 min same).
    s.run(fold + 60_000, &calm, Some((EVERY, FAST)));
    s.run(fold + 80_000, &yes, Some((EVERY, SLOW)));
    s.run(fold + 100_000, &calm, Some((EVERY, FAST)));
    assert_eq!(s.posts().len(), 1, "{:?}", s.outs);
    assert!(s.records().iter().any(|r| {
        r.detail
            .contains(&"not shown: quiet after last episode".to_string())
    }));
    // Another cause inside three minutes: quiet too.
    let chrome = World::loaded(vec![app(700, "Google Chrome", 7800, 2)], 1000);
    s.run(fold + 120_000, &chrome, Some((EVERY, SLOW)));
    s.run(fold + 140_000, &calm, Some((EVERY, FAST)));
    assert_eq!(s.posts().len(), 1, "{:?}", s.outs);
    // The same cause after five minutes: still quiet (fifteen for it).
    s.run(fold + 300_000, &calm, Some((EVERY, FAST)));
    s.run(fold + 320_000, &yes, Some((EVERY, SLOW)));
    s.run(fold + 340_000, &calm, Some((EVERY, FAST)));
    assert_eq!(s.posts().len(), 1, "{:?}", s.outs);
    // Another cause after five minutes: shown.
    s.run(fold + 360_000, &chrome, Some((EVERY, SLOW)));
    assert_eq!(s.posts().len(), 2, "{:?}", s.outs);
    assert_eq!(s.posts()[1].1.title, "Typing slowed by Google Chrome");
    s.run(fold + 380_000, &calm, Some((EVERY, FAST)));
    // An escalation breaks the quiet at once: memory pressure Critical.
    let start = s.ms;
    let mut full = World::loaded(vec![app(700, "Google Chrome", 500, 6)], 400);
    full.pressure = Some(MemoryLevel::Critical);
    full.mem_used_pm = Some(980);
    full.swap_kib_s = 60_000;
    s.run(start + 20_000, &full, Some((EVERY, SLOW)));
    let posts = s.posts();
    assert_eq!(posts.len(), 3, "{:?}", s.outs);
    let m = &posts[2].1;
    assert_eq!(m.title, "Typing slowed by Google Chrome");
    assert_eq!(m.severity, Severity::Warn, "critical: quit something");
    assert_eq!(m.meter.as_ref().unwrap().load, Some(Load::Memory));
    assert_eq!(m.meter.as_ref().unwrap().stats, "16 GB \u{b7} swap 59 MB/s");
    assert!(
        m.detail[0].starts_with("top: Google Chrome 6.0 GB"),
        "{:?}",
        m.detail
    );
}

#[test]
fn glass_cap_two_minutes_min_six_seconds() {
    // Never before six seconds: readings every second clear the kind in
    // three, and the row still stands until six.
    let mut s = Sim::new();
    let yes = World::loaded(yes12(200), 1000);
    s.run(10_000, &yes, Some((EVERY, SLOW)));
    let posted = s.posts()[0].0;
    s.auto = false;
    let calm = World::calm();
    for i in 1..=5 {
        s.run(posted + i * 1000, &calm, Some((EVERY, SLOW)));
        s.sample(&calm);
    }
    assert_eq!(s.t.verdict(), None, "cleared after three");
    assert!(s.folds().is_empty(), "{:?}", s.outs);
    s.run(posted + 6000, &calm, Some((EVERY, SLOW)));
    s.sample(&calm);
    assert_eq!(s.folds().len(), 1, "{:?}", s.outs);
    let rec = s.folds()[0].1.clone().unwrap();
    assert!(
        rec.detail.last().unwrap().starts_with("on glass 6 s"),
        "{:?}",
        rec.detail
    );

    // Never past two minutes, whatever the machine does.
    let mut s = Sim::new();
    s.run(300_000, &yes, Some((EVERY, SLOW)));
    let posted = s.posts()[0].0;
    let folded = s.folds()[0].0;
    let on = folded - posted;
    assert!((120_000..122_100).contains(&on), "{on}");
    let rec = s.folds()[0].1.clone().unwrap();
    assert_eq!(rec.detail.last().unwrap(), "on glass 2m 0s");
    assert!(
        rec.title
            .starts_with("Typing slowed by yes in tab 2 for 2m ")
    );
    // …and the quiet keeps it off after (the same cause for 15 min).
    assert_eq!(s.posts().len(), 1, "{:?}", s.outs);

    // No key for thirty seconds folds it too.
    let mut s = Sim::new();
    s.run(10_000, &yes, Some((EVERY, SLOW)));
    let last_key = 9_900;
    s.run(50_000, &yes, None);
    let idle = s.folds()[0].0 - last_key;
    assert!((30_000..32_100).contains(&idle), "{idle}");
}

/// The measured level on the glass: the whole row, no percent, no ETA, no
/// glint; it glides up AND down; it never completes (a resolve is refused,
/// every echo is a Vanish); and it is never carried.
#[test]
fn a_level_meter_glides_both_ways_and_never_completes() {
    let now = Instant::now();
    let ms = Duration::from_millis;
    let mut c = MessageCenter::new(MessageLog::empty(), now);
    let row = |pm: u16| {
        Message::new(
            tags::SYSTEM,
            Severity::Info,
            "Typing slowed by yes in tab 2",
        )
        .key(STRAIN_KEY)
        .hold(Hold::Live {
            stale_after: STALE_STRAIN,
        })
        .meter(Meter {
            load: Some(Load::Cpu),
            ..Meter::level(pm, "7.1 of 8 cores")
        })
        .no_excerpt()
    };
    let stamp = WallStamp { unix_ms: 1 };
    let id = c.post(row(900), stamp, now).id;
    c.commit_rows(now, 3);
    let p = c.presentation(120, &char_width, None, Links::Painted);
    let layout = &p.rows[0];
    assert_eq!(layout.meter, Some((0, 120, 900)), "the whole row");
    assert!(layout.pct.is_none(), "no percent word");
    assert!(layout.eta.is_none(), "no ETA");
    assert!(
        layout.elapsed.is_some(),
        "the clock: how long it has lasted"
    );
    assert_eq!(layout.load.map(|(_, w)| w), Some("CPU busy"));
    // No glint, ever: every frame over two glint periods is the plain bar.
    for f in 0..u64::try_from((2 * GLINT_PERIOD).as_millis() / 33).unwrap() {
        let m = c.motion(&p, now + ms(f * 33), Look::MOVING);
        assert!(
            matches!(m.rows[0].anim, Anim::Bar { glint_ms: None, .. }),
            "{:?}",
            m.rows[0].anim
        );
    }
    // Down, then up: each restate glides over FILL_GLIDE from the fill shown.
    let restate = |c: &mut MessageCenter, pm: u16, at: Instant| {
        c.restate(
            id,
            Restatement {
                meter: Some(row(pm).meter),
                ..Restatement::default()
            },
            at,
        )
    };
    let t1 = now + ms(2000);
    assert!(restate(&mut c, 400, t1));
    let mid = c.live(id).unwrap().shown_fill(t1 + FILL_GLIDE / 2).unwrap();
    assert!(400 < mid && mid < 900, "gliding down: {mid}");
    assert_eq!(c.live(id).unwrap().shown_fill(t1 + FILL_GLIDE), Some(400));
    let t2 = t1 + ms(2000);
    assert!(restate(&mut c, 800, t2));
    let mid = c.live(id).unwrap().shown_fill(t2 + FILL_GLIDE / 2).unwrap();
    assert!(400 < mid && mid < 800, "gliding up: {mid}");
    // The deadline asks frames for the glide only, then nothing but the
    // clock's next word: no glint travel is ever scheduled.
    let p = c.presentation(120, &char_width, None, Links::Painted);
    let rest = t2 + FILL_GLIDE + ms(100);
    let next = c.motion_deadline(&p, rest, Look::MOVING).unwrap();
    assert!(next >= rest + ms(800), "only the clock ticks at rest");
    // Never carried: the successor measures for itself.
    assert!(c.carried().live.is_empty());
    // Never completes.
    assert!(!c.resolve(id, Outcome::Ok, t2), "a level has no outcome");
    assert!(!c.resolve(id, Outcome::Warn, t2));
    assert!(c.live(id).is_some());
    assert!(c.withdraw_with(id, EchoKind::Complete, t2 + ms(500)));
    assert_eq!(c.echoes()[0].kind, EchoKind::Vanish, "Vanish only");
    // A struct literal cannot sneak one in either: a level needs a fill.
    let m = Meter {
        level: true,
        ..Meter::default()
    }
    .normalized();
    assert!(!m.level);
    let m = Meter {
        amount: Some(crate::model::Amount {
            series: 1,
            done: 1,
            total: 2,
            unit: crate::model::Unit::Bytes,
        }),
        ..Meter::level(10, "")
    }
    .normalized();
    assert!(m.level && m.amount.is_none(), "a gauge has no ETA");
}

#[test]
fn records_cap_at_six_an_hour() {
    let mut t = StrainTracker::new(cfg());
    let base = Instant::now();
    let rec = |i: u64| {
        Message::new(tags::SYSTEM, Severity::Info, format!("r{i}"))
            .key(STRAIN_KEY)
            .hold(Hold::LogOnly)
    };
    let mut kept = Vec::new();
    for i in 0..9u64 {
        let at = base + Duration::from_secs(i * 60);
        if let Some(m) = t.record(at, rec(i)) {
            kept.push(m.title);
        }
    }
    assert_eq!(kept, ["r0", "r1", "r2", "r3", "r4", "r5"]);
    // An hour after the first, one slot frees and the next says what it
    // swallowed.
    let at = base + Duration::from_secs(3600);
    assert_eq!(t.record(at, rec(9)).unwrap().title, "r9 (+3 more)");
    let at = base + Duration::from_secs(3660);
    assert_eq!(t.record(at, rec(10)).unwrap().title, "r10");

    // The critical push while calm: at most one per ten minutes.
    let mut t = StrainTracker::new(cfg());
    assert!(matches!(t.pushed_critical(base), StrainOut::Record(_)));
    assert_eq!(
        t.pushed_critical(base + Duration::from_secs(300)),
        StrainOut::None
    );
    let StrainOut::Record(m) = t.pushed_critical(base + Duration::from_secs(600)) else {
        panic!("a second record after ten minutes");
    };
    assert_eq!(m.title, "Memory pressure critical");
    assert_eq!(m.hold, Hold::LogOnly);
}

/// Every title the engine can say — every kind by every culprit, with the
/// hostile names a process can carry — passes the glass title form, stays
/// inside six words and 48 characters, never says blather, and every row
/// passes the level rule.
#[test]
fn every_title_passes_attention_and_the_title_form() {
    let names = [
        "yes",
        "cargo",
        "Google Chrome",
        "Microsoft Visual Studio Code Helper (Renderer)",
        "node.",
        "a: b; c",
        "100% cpu",
        "usage",
        "consider-this",
        "FYI",
        "may",
        "x\u{1b}[31m",
        "",
        "   ",
        "\u{2014}",
        "a \u{2014} b",
        &"w".repeat(80),
    ];
    let mut culprits = vec![
        Culprit::Services,
        Culprit::Resource,
        Culprit::Aterm,
        Culprit::OwnJob(job_id()),
    ];
    for f in [
        Family::FileSync,
        Family::ICloudSync,
        Family::PhotosAnalysis,
        Family::SpotlightIndexing,
        Family::FileIndexing,
        Family::SystemUpdate,
    ] {
        culprits.push(Culprit::Family(f));
    }
    for n in names {
        culprits.push(Culprit::App(n.into()));
        culprits.push(Culprit::Process(n.into()));
        for (tab, elsewhere) in [(2, false), (65_535, false), (1, true)] {
            culprits.push(Culprit::Session {
                program: n.into(),
                tab,
                elsewhere,
                receiving_keys: false,
            });
        }
    }
    for services_noun in [MACOS_SERVICES, SYSTEM_SERVICES] {
        let cfg = StrainConfig {
            services_noun,
            ..cfg()
        };
        for kind in StrainKind::ALL {
            for c in &culprits {
                let t = title(kind, c, &cfg);
                assert_eq!(glass_title_fault(&t), None, "{t:?}");
                assert!(title_words(&t) <= GLASS_TITLE_WORDS, "{t:?}");
                assert!(t.chars().count() <= GLASS_TITLE_CHARS, "{t:?}");
                assert!(!says_blather(&t), "{t:?}");
                assert!(t.starts_with("Typing slowed by "), "{t:?}");
                assert!(!t.ends_with(' '), "{t:?}");
            }
        }
    }
    // A name made only of blather or noise falls back to the resource.
    assert_eq!(
        title(StrainKind::Cpu, &Culprit::Process("usage".into()), &cfg()),
        "Typing slowed by CPU load"
    );
    // The records' titles say no blather either.
    for words in ["yes in tab 2", "CPU load", "macOS services"] {
        for secs in [0, 40, 192, 3700] {
            let t = format!(
                "{TITLE_HEAD}{words} for {}",
                span_words(Duration::from_secs(secs))
            );
            assert!(!says_blather(&t), "{t}");
        }
    }
    assert!(says_blather("Load average 4.4"));
    assert!(says_blather("CPU usage high"));
    assert!(says_blather("You may want to"));
    assert!(!says_blather("Typing slowed by CPU load"));

    // Every row a live episode posts passes the level rule and the host's
    // attention shape: live, a fill, not a confirmation.
    let mut s = Sim::new();
    s.run(
        30_000,
        &World::loaded(yes12(200), 1000),
        Some((EVERY, SLOW)),
    );
    for (_, m) in s.posts().iter().chain(s.restates().iter()) {
        assert_eq!(level_fault(m), None);
        assert_eq!(glass_title_fault(&m.title), None);
        assert!(matches!(m.hold, Hold::Live { .. }));
        assert!(m.meter.as_ref().unwrap().fill_permille.is_some());
        assert_ne!(m.severity, Severity::Success);
        assert!(m.detail.len() <= 5);
    }
    // A level anywhere else is refused.
    let off = Message::new(tags::UPDATE, Severity::Info, "t").meter(Meter::level(5, ""));
    assert_eq!(level_fault(&off), Some("a level off the strain row"));
    let keyed = off.clone().key("update.progress");
    assert_eq!(level_fault(&keyed), Some("a level off the strain row"));
    let system = Message::new(tags::SYSTEM, Severity::Info, "t")
        .meter(Meter::level(5, ""))
        .key("system.other");
    assert_eq!(level_fault(&system), Some("a level off the strain row"));
    assert_eq!(
        level_fault(&Message::new(tags::UPDATE, Severity::Info, "t")),
        None
    );
}

/// The accessible name is the title — it changes with the cause, never with
/// the numbers; the description carries the load words and the stats.
#[test]
fn the_accessible_name_ignores_the_numbers() {
    let mut s = Sim::new();
    let mut w = World::loaded(yes12(200), 1000);
    s.run(10_000, &w, Some((EVERY, SLOW)));
    w.busy_pm = 880;
    for p in &mut w.procs {
        if p.row.name == "yes" {
            p.mc = 580;
        }
    }
    s.run(20_000, &w, Some((EVERY, SLOW)));
    let posted = s.posts()[0].1.clone();
    let restated: Vec<Message> = s.restates().into_iter().map(|(_, m)| m).collect();
    assert!(
        restated
            .iter()
            .any(|m| m.meter.as_ref().unwrap().stats != posted.meter.as_ref().unwrap().stats),
        "the numbers moved"
    );
    let now = Instant::now();
    let mut c = MessageCenter::new(MessageLog::empty(), now);
    let id = c.post(posted.clone(), WallStamp { unix_ms: 1 }, now).id;
    c.commit_rows(now, 3);
    let name = |c: &MessageCenter| {
        c.presentation(120, &char_width, None, Links::Painted).rows[0].spoken("")
    };
    let before = name(&c);
    assert_eq!(before, "Typing slowed by yes in tab 2");
    let mut descriptions = vec![level_description(&posted)];
    for m in &restated {
        c.restate(
            id,
            Restatement {
                title: Some(m.title.clone()),
                meter: Some(m.meter.clone()),
                ..Restatement::default()
            },
            now + Duration::from_secs(1),
        );
        assert_eq!(name(&c), before, "the numbers never re-announce the row");
        descriptions.push(level_description(m));
    }
    assert!(
        descriptions[0].starts_with("CPU busy \u{b7} 8.0 of 8 cores"),
        "{descriptions:?}"
    );
    assert!(
        descriptions.iter().any(|d| d.contains("7.0 of 8 cores")),
        "{descriptions:?}"
    );
}

/// The live gate, synthetically: twelve `yes` in tab 2 while a person types
/// in tab 1. The row reaches the glass within about nine seconds of the
/// load's onset and folds within about twelve of its end, and a calm tracker
/// then arms nothing.
#[test]
fn a_synthetic_yes_x12_posts_within_nine_seconds_and_folds_within_twelve() {
    let mut s = Sim::new();
    let calm = World::calm();
    let yes = World::loaded(yes12(200), 1000);
    s.run(10_000, &calm, Some((EVERY, FAST)));
    assert!(s.outs.is_empty());
    s.run(70_000, &yes, Some((EVERY, SLOW)));
    let posts = s.posts();
    assert_eq!(posts.len(), 1, "{:?}", s.outs);
    let onset = 10_000;
    let lag = posts[0].0 - onset;
    assert!(lag <= 9_000, "posted {lag} ms after the onset");
    assert_eq!(posts[0].1.title, "Typing slowed by yes in tab 2");
    s.run(100_000, &calm, Some((EVERY, FAST)));
    let folds = s.folds();
    assert_eq!(folds.len(), 1, "{:?}", s.outs);
    let lag = folds[0].0 - 70_000;
    assert!(lag <= 12_000, "folded {lag} ms after the load ended");
    let rec = folds[0].1.clone().unwrap();
    assert_eq!(rec.hold, Hold::LogOnly);
    assert!(
        rec.title
            .starts_with("Typing slowed by yes in tab 2 for 1m "),
        "{}",
        rec.title
    );
    assert!(rec.detail.len() <= 5);
    assert!(rec.detail.iter().any(|l| l.starts_with("on glass ")));
    // Calm again: nothing armed.
    assert_eq!(s.t.state_word(), "calm");
    assert_eq!(s.t.next_sample(s.at(s.ms), OPEN), None);
    let arms_before = s.outs.len();
    s.run(130_000, &calm, None);
    assert_eq!(s.outs.len(), arms_before);
}

/// The switch off parks the tracker: an open row folds with its record,
/// and nothing is armed after.
#[test]
fn the_switch_off_parks_and_arms_nothing() {
    let mut s = Sim::new();
    let yes = World::loaded(yes12(200), 1000);
    s.run(10_000, &yes, Some((EVERY, SLOW)));
    assert_eq!(s.posts().len(), 1);
    s.gate.enabled = false;
    let now = s.at(s.ms);
    let r = s.reading(&yes, now);
    assert!(matches!(
        s.t.observe(&r, s.gate),
        StrainOut::Fold { record: Some(_) }
    ));
    assert_eq!(s.t.state_word(), "calm");
    assert_eq!(s.t.next_sample(now, OPEN), None);
}

/// With the glass off (the records-only rollout) an episode is a record.
#[test]
fn records_only_writes_the_episode_and_posts_nothing() {
    let mut s = Sim::with(StrainConfig {
        glass: false,
        ..cfg()
    });
    s.run(
        20_000,
        &World::loaded(yes12(200), 1000),
        Some((EVERY, SLOW)),
    );
    s.run(40_000, &World::calm(), Some((EVERY, FAST)));
    assert!(s.posts().is_empty() && s.folds().is_empty(), "{:?}", s.outs);
    let recs = s.records();
    assert_eq!(recs.len(), 1);
    assert!(
        recs[0]
            .detail
            .contains(&"not shown: records only".to_string())
    );
}

/// Grouping: a session by its shell, helpers into their app, a family only
/// for the person's own processes, another user's process as the services.
#[test]
fn grouping_follows_the_first_match() {
    let mut rows: Vec<ProcRow> = base().into_iter().map(|p| p.row).collect();
    let mut helper = app(701, "Google Chrome", 0, 1).row;
    helper.bundle = Some(
        "/Applications/Google Chrome.app/Contents/Frameworks/Helper.app/Contents/MacOS/Helper"
            .into(),
    );
    helper.cpu_ns = Some(5);
    let mut main = app(700, "Google Chrome", 0, 1).row;
    main.cpu_ns = Some(7);
    let mut fp = proc(400, 1, "fileproviderd", 0).row;
    fp.cpu_ns = Some(3);
    let mut theirs = proc(401, 1, "fileproviderd", 0).row;
    theirs.uid_is_ours = false;
    theirs.cpu_ns = Some(2);
    let mut deep = proc(209, 208, "cc", 0).row;
    deep.cpu_ns = Some(11);
    let mut mid = proc(208, 200, "make", 0).row;
    mid.cpu_ns = Some(1);
    rows.extend([helper, main, fp, theirs, deep, mid]);
    let g = group(&rows, &sessions(), &[], SELF_PID);
    let find = |c: &Culprit| g.iter().find(|x| x.culprit == *c).map(|x| x.cpu_ns);
    assert_eq!(find(&Culprit::App("Google Chrome".into())), Some(12));
    assert_eq!(find(&Culprit::Family(Family::FileSync)), Some(3));
    assert_eq!(find(&Culprit::Services), Some(2));
    // Tab 2's foreground `yes` is not what burns there: `cc` under `make`
    // is, and the session is named by it (ruling 207, amended live).
    assert_eq!(
        find(&Culprit::Session {
            program: "cc".into(),
            tab: 2,
            elsewhere: false,
            receiving_keys: false
        }),
        Some(12)
    );
    assert_eq!(g[0].cpu_ns, 12, "heaviest first");
    // The wire's `load=` stays the four words a script may declare.
    assert_eq!(Load::parse_wire("memory"), None);
    assert_eq!(Load::parse("memory"), Some(Load::Memory));
    assert_eq!(Load::Memory.words(), "memory full");
}

/// The live gate's own scenario (found live on this m1, 2026-09-24): twelve
/// `yes &` at a zsh prompt leave zsh as tab 2's FOREGROUND program, and the
/// band said `Typing slowed by zsh in tab 2` — an idle shell blamed. A
/// session is named by what is heavy under it: the foreground program when
/// the heaviest process is it or runs under it (`cargo`, not its `rustc`),
/// else the heaviest process itself (`yes`; a background build under a
/// foreground editor is `rustc`, never `vim`). Two panes of one tab that
/// come to say the same words are one group.
#[test]
fn a_session_is_named_by_what_is_heavy_under_it() {
    let at_prompt = |program: &str| {
        let mut s = sessions();
        s[1].program = program.into();
        s
    };
    let named = |rows: &[ProcRow], s: &[SessionRef]| -> Vec<(String, u64)> {
        group(rows, s, &[], SELF_PID)
            .into_iter()
            .filter_map(|g| match g.culprit {
                Culprit::Session {
                    program, tab: 2, ..
                } => Some((program, g.cpu_ns)),
                _ => None,
            })
            .collect()
    };
    let row = |pid: u32, ppid: u32, name: &str, ns: u64| {
        let mut r = proc(pid, ppid, name, 0).row;
        r.cpu_ns = Some(ns);
        r
    };
    let mut rows: Vec<ProcRow> = base().into_iter().map(|p| p.row).collect();
    rows[2].name = "-zsh".into();
    // `yes &` x12 at the prompt.
    let mut yes = rows.clone();
    yes.extend((1..=12).map(|i| row(200 + i, 200, "yes", 660)));
    assert_eq!(
        named(&yes, &at_prompt("zsh")),
        [("yes".to_string(), 12 * 660)]
    );
    // A foreground build: the program the person ran, not its compiler.
    let mut build = rows.clone();
    build.extend([row(210, 200, "cargo", 5), row(211, 210, "rustc", 900)]);
    assert_eq!(
        named(&build, &at_prompt("cargo")),
        [("cargo".to_string(), 905)]
    );
    // A background build under a foreground editor: the build.
    let mut bg = rows.clone();
    bg.extend([
        row(220, 200, "vim", 3),
        row(221, 200, "cargo", 5),
        row(222, 221, "rustc", 900),
    ]);
    assert_eq!(named(&bg, &at_prompt("vim")), [("rustc".to_string(), 908)]);
    // Only the shell measured: the published program is all there is.
    let mut idle = rows.clone();
    idle[2].cpu_ns = Some(40);
    assert_eq!(named(&idle, &at_prompt("zsh")), [("zsh".to_string(), 40)]);
    // Two panes of tab 2, each at a prompt with a `yes` under it: one group.
    let mut panes = at_prompt("zsh");
    panes.push(SessionRef {
        shell_pid: 300,
        program: "bash".into(),
        tab: 2,
        receiving_keys: false,
        elsewhere: false,
    });
    let mut two = rows.clone();
    two.extend([
        row(300, SELF_PID, "bash", 0),
        row(201, 200, "yes", 600),
        row(301, 300, "yes", 700),
    ]);
    assert_eq!(named(&two, &panes), [("yes".to_string(), 1300)]);

    // End to end: the tracker titles the prompt's load `yes in tab 2`.
    let mut s = Sim::new();
    s.sessions = at_prompt("zsh");
    let calm = World::calm();
    let yes = World::loaded(yes12(200), 1000);
    s.run(10_000, &calm, Some((EVERY, FAST)));
    s.run(40_000, &yes, Some((EVERY, SLOW)));
    let posts = s.posts();
    assert_eq!(posts.len(), 1, "{:?}", s.outs);
    assert_eq!(posts[0].1.title, "Typing slowed by yes in tab 2");
}

/// Found live (2026-09-24): twelve `yes` killed between two sweeps took
/// their window's time with them, the services' subtraction took it, and
/// the live row read `top: macOS services 7.5 cores` under `Typing slowed
/// by yes in tab 2`; the episode's record then carried the calm machine's
/// last sweep (`macOS services 0.6 cores · aterm 0.2`), which never named
/// what slowed the typing. A sweep that busy processes exited inside is a
/// baseline only, and a clearing machine's sweeps never replace a loaded
/// one's `top:` line.
#[test]
fn exits_never_hand_the_services_the_culprits_time() {
    let mut s = Sim::new();
    let calm = World::calm();
    let yes = World::loaded(yes12(200), 1000);
    s.run(10_000, &calm, Some((EVERY, FAST)));
    s.run(40_000, &yes, Some((EVERY, SLOW)));
    assert_eq!(s.posts().len(), 1, "{:?}", s.outs);
    // The load is killed; the machine stays busy for the rest of the
    // window (the reading lags the exits), then clears.
    let mut dying = World::calm();
    dying.busy_pm = 1000;
    s.run(43_000, &dying, Some((EVERY, SLOW)));
    s.run(80_000, &calm, Some((EVERY, FAST)));
    let folds = s.folds();
    assert_eq!(folds.len(), 1, "{:?}", s.outs);
    let top = |m: &Message| {
        m.detail
            .iter()
            .find(|l| l.starts_with("top: "))
            .cloned()
            .unwrap_or_default()
    };
    for (_, m) in s.restates().iter().chain(s.posts().iter()) {
        assert!(
            top(m).starts_with("top: yes in tab 2 "),
            "the live row's top line: {:?}",
            m.detail
        );
    }
    let rec = folds[0].1.clone().expect("the episode's record");
    assert!(
        top(&rec).starts_with("top: yes in tab 2 "),
        "the record says what loaded the machine: {:?}",
        rec.detail
    );
}

/// Review 2026-09-24: a build's compilers start and exit every sweep. The
/// exits' time is credited, at their last rate, to the group they belonged
/// to — never thrown away with the whole sweep (the build was never named:
/// `Typing slowed by CPU load`), never handed to the services. A steady
/// stream of one-core children under `cargo` in tab 2 names `cargo`.
#[test]
fn churning_children_still_name_their_session() {
    let mut s = Sim::new();
    s.sessions[1].program = "cargo".into();
    // Every second one `rustc` exits and a new one starts: eight run at a
    // time, each for eight seconds, under a steady `cargo`.
    let world = |sec: u64| {
        let mut procs = vec![proc(210, 200, "cargo", 20)];
        let first = u32::try_from(sec).unwrap();
        procs.extend((first..first + 8).map(|k| proc(1000 + k, 210, "rustc", 990)));
        World::loaded(procs, 1000)
    };
    // Typing turns slow first (the build is starting): the episode's early
    // sweeps see a calm machine and name nobody.
    s.run(4000, &World::calm(), Some((EVERY, SLOW)));
    for sec in 4..40 {
        s.run((sec + 1) * 1000, &world(sec), Some((EVERY, SLOW)));
    }
    let posts = s.posts();
    assert_eq!(posts.len(), 1, "{:?}", s.outs);
    assert_eq!(posts[0].1.title, "Typing slowed by cargo in tab 2");
    assert_eq!(posts[0].1.meter.as_ref().unwrap().load, Some(Load::Cpu));
    let mut rows: Vec<Message> = s.posts().into_iter().map(|(_, m)| m).collect();
    rows.extend(s.restates().into_iter().map(|(_, m)| m));
    for m in &rows {
        assert_eq!(m.title, "Typing slowed by cargo in tab 2", "{:?}", s.outs);
        assert!(
            m.detail[0].starts_with("top: cargo in tab 2 "),
            "the top line is the build, not the services: {:?}",
            m.detail
        );
    }
}

/// Review 2026-09-24: a leader whose share hovers at the NAMED line (1 core
/// and 40 % of the busy time) never flips the title between the culprit and
/// the resource scan by scan: while a row shows, NAMED changes like a
/// culprit does — after two scans that agree.
#[test]
fn the_named_flag_changes_only_after_two_scans() {
    let c = Culprit::Process("yes".into());
    let mut l = Leader::default();
    l.offer(Some((c.clone(), true)), false);
    assert_eq!(l.now, Some((c.clone(), true)));
    // Hovering: named, not, named, not — the row keeps its words.
    for named in [false, true, false, true, false] {
        l.offer(Some((c.clone(), named)), true);
        assert_eq!(l.now, Some((c.clone(), true)), "one scan is no change");
    }
    // Two scans that agree: the change is real.
    l.offer(Some((c.clone(), false)), true);
    assert_eq!(l.now, Some((c.clone(), false)));
    // And back the same way.
    l.offer(Some((c.clone(), true)), true);
    assert_eq!(l.now, Some((c.clone(), false)));
    l.offer(Some((c.clone(), true)), true);
    assert_eq!(l.now, Some((c.clone(), true)));
    // With no row showing it follows the scan at once.
    l.offer(Some((c.clone(), false)), false);
    assert_eq!(l.now, Some((c, false)));
}

/// Review 2026-09-24: a Suspect always outlives [`STRAIN_RECORD_AFTER`]
/// before it ends, so the record's line is how long typing was FELT slow: a
/// blip felt for under five seconds writes nothing (and spends none of the
/// cap); one felt longer writes its record.
#[test]
fn a_suspect_felt_under_five_seconds_writes_no_record() {
    let mut s = Sim::new();
    let calm = World::calm();
    s.run(1000, &calm, Some((EVERY, SLOW)));
    assert_eq!(s.t.state_word(), "suspect");
    s.run(2000, &calm, Some((EVERY, SLOW)));
    s.run(40_000, &calm, Some((EVERY, FAST)));
    assert_eq!(s.t.state_word(), "calm");
    assert!(s.records().is_empty(), "{:?}", s.outs);
    // Felt for eight seconds: one record.
    s.run(49_000, &calm, Some((EVERY, SLOW)));
    s.run(80_000, &calm, Some((EVERY, FAST)));
    let recs = s.records();
    assert_eq!(recs.len(), 1, "{:?}", s.outs);
    assert!(
        recs[0].title.starts_with("Typing slow for "),
        "{}",
        recs[0].title
    );
    assert!(!recs[0].title.contains("for 0 s"), "{}", recs[0].title);
}

/// Review 2026-09-24: memory stays heavy while pressure sits at Warn (it
/// clears only at Normal) after the swapping stops; a CPU kind that is
/// pegged and RISING still opens the row, and the row does not turn into
/// `low memory` a reading later.
#[test]
fn a_heavy_kind_no_longer_rising_never_blocks_a_rising_one() {
    let mut s = Sim::new();
    let mut pegged = World::loaded(yes12(200), 1000);
    pegged.pressure = Some(MemoryLevel::Warn);
    let mut swapping = pegged.clone();
    swapping.swap_kib_s = 8 * 1024;
    s.run(4000, &swapping, Some((EVERY, SLOW)));
    s.run(5000, &pegged, Some((EVERY, SLOW)));
    assert_eq!(
        s.t.verdict(),
        Some(StrainKind::Memory),
        "heavy, by hysteresis"
    );
    s.run(40_000, &pegged, Some((EVERY, SLOW)));
    let posts = s.posts();
    assert_eq!(posts.len(), 1, "{:?}", s.outs);
    assert_eq!(posts[0].1.title, "Typing slowed by yes in tab 2");
    for (_, m) in s.restates() {
        assert_eq!(m.title, "Typing slowed by yes in tab 2", "{:?}", s.outs);
    }
}

/// Review 2026-09-24: two panes of one tab both at a `zsh` prompt, `yes`
/// under pane B. The session is named from pane B's OWN shell — never by
/// looking a session up again by its words, which could find pane A and
/// walk past B's `zsh` to blame the idle shell.
#[test]
fn split_panes_name_the_pane_that_is_heavy() {
    let row = |pid: u32, ppid: u32, name: &str, ns: u64| {
        let mut r = proc(pid, ppid, name, 0).row;
        r.cpu_ns = Some(ns);
        r
    };
    let panes = vec![
        SessionRef {
            shell_pid: 300,
            program: "zsh".into(),
            tab: 2,
            receiving_keys: false,
            elsewhere: false,
        },
        SessionRef {
            shell_pid: 400,
            program: "zsh".into(),
            tab: 2,
            receiving_keys: false,
            elsewhere: false,
        },
    ];
    let rows = vec![
        row(300, SELF_PID, "zsh", 0),
        row(400, SELF_PID, "zsh", 1),
        row(401, 400, "yes", 990),
    ];
    let g = group(&rows, &panes, &[], SELF_PID);
    assert_eq!(
        g[0].culprit,
        Culprit::Session {
            program: "yes".into(),
            tab: 2,
            elsewhere: false,
            receiving_keys: false,
        },
        "{g:?}"
    );
    assert_eq!(g[0].cpu_ns, 991);
}

/// Review 2026-09-24: the window loses focus with the row open; the host
/// takes no readings (the idle law) and the row fades. When a reading comes
/// an hour later, the episode ends where sampling STOPPED: its record never
/// counts the hour away as time on the glass, and the quiet period starts
/// then too. [`StrainTracker::lapsed`] ends it the same way on any loop turn
/// once no reading has come for [`STALE_STRAIN`], so an unfocused tracker is
/// calm again without anything armed.
#[test]
fn a_gap_folds_where_sampling_stopped() {
    let yes = World::loaded(yes12(200), 1000);
    for via_lapse in [false, true] {
        let mut s = Sim::new();
        s.run(12_000, &yes, Some((EVERY, SLOW)));
        assert_eq!(s.t.state_word(), "open");
        let posted = s.posts()[0].0;
        let last = s.ms;
        s.auto = false;
        s.run(last + 3_600_000, &yes, None);
        let out = if via_lapse {
            assert_eq!(
                s.t.lapsed(s.at(last + 20_000)),
                StrainOut::None,
                "not stale"
            );
            s.t.lapsed(s.at(s.ms))
        } else {
            let r = s.reading(&yes, s.at(s.ms));
            s.t.observe(&r, OPEN)
        };
        let StrainOut::Fold { record: Some(rec) } = out else {
            panic!("a fold: {out:?}");
        };
        assert_eq!(s.t.state_word(), "calm");
        let glass = rec.detail.last().unwrap();
        let secs: u64 = glass
            .strip_prefix("on glass ")
            .and_then(|g| g.strip_suffix(" s"))
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("seconds on glass, not the hour away: {glass}"));
        assert!(
            secs <= (last - posted) / 1000 && secs < (STALE_STRAIN + STRAIN_SAMPLE_EVERY).as_secs(),
            "{glass}"
        );
        assert!(rec.title.ends_with(" s"), "{}", rec.title);
    }
}

/// Review 2026-09-24: an episode that ends as a Suspect after the window
/// lost focus counts its felt span to its last reading, never the time away.
#[test]
fn a_suspect_gap_ends_at_the_last_reading() {
    let mut s = Sim::new();
    s.sessions[1].receiving_keys = true;
    s.sessions[0].receiving_keys = false;
    // Their own command: a Suspect that is never shown.
    s.run(
        20_000,
        &World::loaded(yes12(200), 1000),
        Some((EVERY, SLOW)),
    );
    assert_eq!(s.t.state_word(), "suspect");
    let last = s.ms;
    s.auto = false;
    s.run(last + 3_600_000, &World::calm(), None);
    let out = s.t.lapsed(s.at(s.ms));
    let StrainOut::Record(rec) = out else {
        panic!("a record: {out:?}");
    };
    assert!(rec.title.ends_with(" s"), "not the hour: {}", rec.title);
    assert!(
        rec.detail
            .contains(&"not shown: your own command".to_string()),
        "{:?}",
        rec.detail
    );
    assert_eq!(s.t.state_word(), "calm");
}
