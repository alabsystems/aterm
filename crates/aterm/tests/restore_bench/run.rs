// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE RESTORE MEASUREMENT's run: a crashed window's tabs back, timed end to end on real
//! processes. Each round boots this build's keeper and a headless instance A in one scratch
//! world ([`crate::keeper_world`]), opens `n` sessions that each print the spec's history,
//! waits for every session's journal to have caught up and its region to be in the keeper's
//! custody, SIGKILLs A by its own pid, and boots B in the same world. B's own clocks say how
//! long its HELLO, its one-after-another admission of the offers, the placing of every
//! recovered session and their history imports took (`metrics` `startup_keeper_*`); the
//! bench's clock says when B first served a request past its recovery-count fence (which
//! refuses `aterm ctl` clients, not the window), when every session's `status` first read a
//! `restore=` and when every history import was in (its lines plus `history_lost=` back to A's
//! count, and B's own journal caught up, `journal_ring_lag=0`). Application-event waits are
//! one-minute hang detectors; the keeper's new code copy has the shared first-exec bound.
//! Nothing here asserts a time.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::{Duration, Instant};

use aterm_json::{Value, json};

use crate::keeper_world::{self, HANG, Proc};
use crate::launch_isolation;
use crate::live_ctl::{AWAIT_MS, Ctl, fields};
use crate::profile::{self, Profile};
use crate::spec::{Applied, Arm, LOOKALIKE_SEAM, Spec};
use crate::stats;

/// Both instances keep a window's desk (its layout, its journals, its restore pass) without a
/// window's wait for glass (`desk::Desk::LabHeadless`). A window's restore pass places the
/// sessions after the first once its first frame is on glass; this desk places them at once (as a
/// plain headless instance has since K1), so B's placement reading leaves out a window's first
/// present. The seam holds only under a private state root ([`STATE_SEAM`]).
const DESK_SEAM: &str = "ATERM_DEBUG_HEADLESS_RESTORE";

/// The world's own state root for both instances (`aterm_types::dirs::runs_under_private_state_root`).
const STATE_SEAM: &str = "ATERM_STATE_HOME";

/// The gap between two sweeps of B's sessions, and between two of the bench's requests while
/// B's fence refuses them. A sweep's own length (`sweep_max_ms`) is the resolution of the
/// polled readings; the served reading's is its window (`served_window_ms`).
const POLL: Duration = Duration::from_millis(2);

/// The progress lines the lane echoes.
const PROGRESS: &str = "restore bench:";

/// What a run hands its writer.
pub struct Report {
    pub json: Value,
    pub markdown: String,
}

/// One crash and recovery of `sessions` sessions.
#[derive(Debug, Clone, PartialEq)]
pub struct Round {
    pub sessions: u32,
    /// B's own clocks (`metrics`): its birth to Rust `main`, `main` to the answered HELLO, that
    /// HELLO to every offer admitted, the slowest one offer, that HELLO to every recovered
    /// session placed and to their history imports settled (`None` with nothing to import), each
    /// less the seam's own work, which is `seam_ms`.
    pub birth_to_main_ms: f64,
    pub main_to_hello_ms: f64,
    pub admit_ms: f64,
    pub admit_max_ms: f64,
    pub placed_ms: f64,
    pub settled_ms: Option<f64>,
    pub offers: u64,
    /// The copy/plant/refusal outcomes B observed, rather than the requested arm label.
    pub experiment: Applied,
    /// What the copy and lookalike arms' seam did in B's admission (`startup_keeper_seam_*`):
    /// the regions it copied, the plants that took, and the time both took.
    pub seam_copies: u64,
    pub seam_plants: u64,
    pub seam_ms: f64,
    /// The bench's clock, from just before B's spawn: B's ready signal, B serving past its
    /// recovery-count fence (late by at most `served_window_ms`), every session's `status`
    /// reading a `restore=`, every history import in.
    pub ready_ms: f64,
    pub served_ms: f64,
    pub served_window_ms: f64,
    pub restored_ms: f64,
    pub imported_ms: f64,
    /// The longest sweep of B's sessions: the two polled readings' resolution.
    pub sweep_max_ms: f64,
    /// The recovered sessions by `restore=` and by `restore_why=`.
    pub rungs: BTreeMap<String, u32>,
    pub whys: BTreeMap<String, u32>,
    /// Every recovered session's `history_lost=`, summed; A's scrollback lines and B's, summed.
    pub history_lost: u64,
    pub a_lines: u64,
    pub b_lines: u64,
    /// `vm.loadavg` just before B's spawn.
    pub load: String,
}

impl Round {
    /// B's birth to its answered HELLO: B's two clocks added.
    #[must_use]
    pub fn birth_to_hello_ms(&self) -> f64 {
        self.birth_to_main_ms + self.main_to_hello_ms
    }

    /// The answered HELLO to every `restore=` readable: the bench's reading less B's, so it also
    /// holds whatever passed between the bench's spawn call and B's birth.
    #[must_use]
    pub fn hello_to_restored_ms(&self) -> f64 {
        self.restored_ms - self.birth_to_hello_ms()
    }

    fn json(&self) -> Value {
        json!({
            "sessions": self.sessions,
            "offers": self.offers,
            "experiment": {
                "copies": self.experiment.copies,
                "plants": self.experiment.plants,
                "refused": self.experiment.refused,
            },
            "birth_to_main_ms": stats::round3(self.birth_to_main_ms),
            "main_to_hello_ms": stats::round3(self.main_to_hello_ms),
            "birth_to_hello_ms": stats::round3(self.birth_to_hello_ms()),
            "admit_ms": stats::round3(self.admit_ms),
            "admit_max_ms": stats::round3(self.admit_max_ms),
            "placed_ms": stats::round3(self.placed_ms),
            "settled_ms": self.settled_ms.map(stats::round3),
            "seam_copies": self.seam_copies,
            "seam_plants": self.seam_plants,
            "seam_ms": stats::round3(self.seam_ms),
            "spawn_to_ready_ms": stats::round3(self.ready_ms),
            "spawn_to_served_ms": stats::round3(self.served_ms),
            "served_window_ms": stats::round3(self.served_window_ms),
            "spawn_to_restored_ms": stats::round3(self.restored_ms),
            "hello_to_restored_ms": stats::round3(self.hello_to_restored_ms()),
            "spawn_to_imported_ms": stats::round3(self.imported_ms),
            "sweep_max_ms": stats::round3(self.sweep_max_ms),
            "rungs": self.rungs,
            "whys": self.whys,
            "history_lost": self.history_lost,
            "a_lines": self.a_lines,
            "b_lines": self.b_lines,
            "load": self.load,
        })
    }
}

/// One timing column: its JSON name, its heading, and the reading (`NaN` where a round has
/// none, which the column's summary leaves out).
type Column = (&'static str, &'static str, fn(&Round) -> f64);

/// The readings on B's own clocks (`metrics`).
const B_COLUMNS: [Column; 6] = [
    (
        "birth_to_hello_ms",
        "birth → HELLO answered",
        Round::birth_to_hello_ms,
    ),
    ("admit_ms", "HELLO → every offer admitted", |r| r.admit_ms),
    ("admit_max_ms", "slowest one offer", |r| r.admit_max_ms),
    ("placed_ms", "HELLO → every session placed", |r| {
        r.placed_ms
    }),
    ("settled_ms", "HELLO → every import settled", |r| {
        r.settled_ms.unwrap_or(f64::NAN)
    }),
    ("seam_ms", "the seam's own work", |r| r.seam_ms),
];

/// The readings on the bench's clock, from just before B's spawn.
const BENCH_COLUMNS: [Column; 7] = [
    ("spawn_to_ready_ms", "spawn → ready", |r| r.ready_ms),
    (
        "spawn_to_served_ms",
        "spawn → served past the fence",
        |r| r.served_ms,
    ),
    ("served_window_ms", "served's window", |r| {
        r.served_window_ms
    }),
    (
        "spawn_to_restored_ms",
        "spawn → every `restore=` read",
        |r| r.restored_ms,
    ),
    (
        "hello_to_restored_ms",
        "HELLO → every `restore=` read",
        Round::hello_to_restored_ms,
    ),
    ("spawn_to_imported_ms", "spawn → every import in", |r| {
        r.imported_ms
    }),
    ("sweep_max_ms", "longest sweep", |r| r.sweep_max_ms),
];

/// `read`'s values over `rounds`, the rounds with no such reading left out.
fn column(rounds: &[Round], read: fn(&Round) -> f64) -> Vec<f64> {
    rounds.iter().map(read).filter(|v| v.is_finite()).collect()
}

/// One table of `columns` over `sizes`: each reading's median and range, per session count.
fn timing_table(columns: &[Column], sizes: &[(u32, Vec<Round>)]) -> Vec<String> {
    let mut head = "| sessions |".to_string();
    let mut rule = "|---:|".to_string();
    for (_, heading, _) in columns {
        head.push_str(&format!(" {heading} |"));
        rule.push_str("---:|");
    }
    head.push_str(" rounds |");
    rule.push_str("---:|");
    let mut table = vec![head, rule];
    for (n, rounds) in sizes {
        let mut row = format!("| {n} |");
        for (_, _, read) in columns {
            row.push_str(&match stats::summarize(&column(rounds, *read)) {
                Some(s) => format!(" {:.1} ({:.1}–{:.1}) |", s.p50, s.min, s.max),
                None => " — |".to_string(),
            });
        }
        row.push_str(&format!(" {} |", rounds.len()));
        table.push(row);
    }
    table
}

/// Measure `spec`.
#[must_use]
pub fn measure(spec: &Spec) -> Report {
    let exe = launch_isolation::aterm_executable();
    let at_start = profile::identity(&exe).unwrap_or_else(|why| why);
    let started = Instant::now();
    let mut sizes = Vec::new();
    for &n in &spec.sessions {
        let rounds: Vec<Round> = (1..=spec.rounds)
            .map(|round| {
                eprintln!(
                    "{PROGRESS} {} arm, {n} session(s), round {round} of {}",
                    spec.arm.word(),
                    spec.rounds
                );
                let r = crash_and_restore(spec, n);
                eprintln!(
                    "{PROGRESS}   HELLO {:.1} ms after B's birth, offers admitted in {:.1} ms and \
                     placed at {:.1} ms (the seam's own work {:.1} ms off both), served at {:.1} \
                     ms, every restore= at {:.1} ms, every import at {:.1} ms (load {})",
                    r.birth_to_hello_ms(),
                    r.admit_ms,
                    r.placed_ms,
                    r.seam_ms,
                    r.served_ms,
                    r.restored_ms,
                    r.imported_ms,
                    r.load
                );
                r
            })
            .collect();
        sizes.push((n, rounds));
    }
    let at_end = profile::identity(&exe).unwrap_or_else(|why| why);
    let arm = Profile::of_binary(&exe);
    let json = json!({
        "schema": "aterm-restore-bench-v2",
        "spec": spec.words(),
        "arm": spec.arm.word(),
        "profile": arm.map_or("unknown", Profile::name),
        "build": arm.map_or("a binary in neither arm's build directory", Profile::describe),
        "aterm_binary": exe.to_string_lossy().into_owned(),
        "aterm_binary_identity": { "start": at_start, "end": at_end },
        "clocks": "B's own (`metrics` startup_keeper_*, process_birth_to_rust_main_ms; the \
                   admission, placement and import readings less the seam's own work, \
                   seam_ms) and the bench's monotonic clock from just before B's spawn (which \
                   holds the seam's work)",
        "elapsed_s": stats::round3(started.elapsed().as_secs_f64()),
        "sizes": sizes.iter().map(|(n, rounds)| size_json(*n, rounds)).collect::<Vec<_>>(),
    });
    Report {
        json,
        markdown: markdown(spec, &sizes),
    }
}

pub(crate) fn size_json(n: u32, rounds: &[Round]) -> Value {
    let summary: aterm_json::Map = B_COLUMNS
        .iter()
        .chain(&BENCH_COLUMNS)
        .map(|(key, _, read)| ((*key).to_string(), stats::to_json(&column(rounds, *read))))
        .collect();
    json!({
        "sessions": n,
        "summary": summary,
        "rounds": rounds.iter().map(Round::json).collect::<Vec<_>>(),
    })
}

/// The run's tables: per session count, each reading's median with its range, then what the
/// recoveries came back as.
#[must_use]
pub fn markdown(spec: &Spec, sizes: &[(u32, Vec<Round>)]) -> String {
    let mut md = vec![
        format!(
            "**Arm:** `{}` — {}. **Run:** {}.",
            spec.arm.word(),
            arm_note(spec),
            spec.words()
        ),
        String::new(),
        "Milliseconds, median (min–max) over the rounds. B's own clocks (`metrics` \
         `process_birth_to_rust_main_ms` and `startup_keeper_*`). The admission, placement and \
         import readings leave out the seam's own work (the copy and lookalike arms' copy and \
         plant of each region, its last column), which a really damaged region never pays. \
         Placement is the restore pass that places every recovered session, which a window runs \
         once its first frame is on glass and this lab desk runs at once; an import settles when \
         B's history import of every recovered session is done (`—` with none to import):"
            .to_string(),
        String::new(),
    ];
    md.extend(timing_table(&B_COLUMNS, sizes));
    md.push(String::new());
    md.push(
        "The bench's clock, from just before B's spawn, which holds the seam's own work (it runs \
         before B's control socket listens). Everything after ready waits on B's recovery-count \
         fence, which refuses `aterm ctl` requests until the keeper confirmed every adopted \
         session's new journal; B's window does not wait on it. Served is asked every 2 ms and is \
         late by at most its window (from the last refused request, or ready, to the served \
         answer); `restore=` and the imports are polled, a sweep of B's sessions every 2 ms, so \
         each is late by at most its longest sweep. HELLO → `restore=` is the bench's reading \
         less B's birth → HELLO."
            .to_string(),
    );
    md.push(String::new());
    md.extend(timing_table(&BENCH_COLUMNS, sizes));
    md.push(String::new());
    md.push(
        "| sessions | `restore=` | `restore_why=` | `history_lost=` | A's lines | B's lines | \
         seam copies / plants | observed copies / plants / refusals | load before B (1/5/15) |"
            .to_string(),
    );
    md.push("|---:|---|---|---:|---:|---:|---:|---:|---|".to_string());
    for (n, rounds) in sizes {
        for r in rounds {
            md.push(format!(
                "| {n} | {} | {} | {} | {} | {} | {} / {} | {} / {} / {} | {} |",
                counts(&r.rungs),
                counts(&r.whys),
                r.history_lost,
                r.a_lines,
                r.b_lines,
                r.seam_copies,
                r.seam_plants,
                r.experiment.copies,
                r.experiment.plants,
                r.experiment.refused,
                r.load
            ));
        }
    }
    md.join("\n")
}

fn arm_note(spec: &Spec) -> String {
    match spec.arm {
        Arm::Clean => "B restores each offered region as the keeper holds it (what ships)".into(),
        Arm::Copy => "B restores each offered region from a copy in its heap \
                      (`ATERM_DEBUG_CRASH_RESTORE_LOOKALIKE_KIB=0`), the copy's making off B's \
                      clocks: against the clean arm, what restoring from a heap copy rather than \
                      the mapped region changes"
            .into(),
        Arm::Lookalike => format!(
            "B restores each offered region from a copy with the look-alike tail planted \
             (`ATERM_DEBUG_CRASH_RESTORE_LOOKALIKE_KIB={}`: the newest history chunk damaged, up \
             to {} KiB of frame look-alikes past the ring's commit, stopping at its lap), the \
             copy and the plant off B's clocks: the damage the ring walk's resync bound was \
             built for, the costliest tail in an optimized build but not in a dev one, where a \
             random tail costs the walk more (`benches/resync_walk.rs`)",
            spec.lookalike_kib, spec.lookalike_kib
        ),
    }
}

fn counts(map: &BTreeMap<String, u32>) -> String {
    if map.is_empty() {
        return "—".to_string();
    }
    map.iter()
        .map(|(k, v)| format!("`{k}` {v}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Whether `arm` took effect in each of B's `offers`: the seam's copies and the plants that
/// took (B's `startup_keeper_seam_copies=` and `_plants=`, `None` in a build without the seams),
/// and for the lookalike arm the history each recovered session lost to the plant (`lost`, its
/// `history_lost=`). A round whose arm did not take is not that arm's, and the bench refuses it.
///
/// # Errors
/// What did not take.
pub(crate) fn arm_took(
    arm: Arm,
    offers: u64,
    copies: Option<u64>,
    plants: Option<u64>,
    lost: &[u64],
) -> Result<(), String> {
    let (want_copies, want_plants) = match arm {
        Arm::Clean => (0, 0),
        Arm::Copy => (offers, 0),
        Arm::Lookalike => (offers, offers),
    };
    let seam = (copies.unwrap_or(0), plants.unwrap_or(0));
    if arm != Arm::Clean && (copies.is_none() || plants.is_none()) {
        return Err(format!(
            "the {} arm needs B's seam, and B reports none: a build without debug assertions",
            arm.word()
        ));
    }
    if seam != (want_copies, want_plants) {
        return Err(format!(
            "the {} arm's seam copied {} and planted {} of B's {offers} offered region(s); it \
             should have copied {want_copies} and planted {want_plants}",
            arm.word(),
            seam.0,
            seam.1
        ));
    }
    if arm == Arm::Lookalike && (lost.len() as u64 != offers || lost.contains(&0)) {
        return Err(format!(
            "the lookalike arm's plant should cost each of B's {offers} recovered session(s) \
             history, and their `history_lost=` read {lost:?}"
        ));
    }
    Ok(())
}

/// How late the served reading may be: from the last request the fence refused (`last_refused`,
/// or B's `ready` signal with none refused) to the served `answered`.
pub(crate) fn served_window_of(
    ready: Instant,
    last_refused: Option<Instant>,
    answered: Instant,
) -> Duration {
    answered.saturating_duration_since(last_refused.unwrap_or(ready).max(ready))
}

/// One round: A with `n` sessions, A's crash, B's recovery.
fn crash_and_restore(spec: &Spec, n: u32) -> Round {
    let world = keeper_world::world(&format!("rb{n}"));
    let root = world.0.clone();
    let _keeper = keeper_world::keeper(&root);
    let state = root.join("state").to_string_lossy().into_owned();
    let desk = [(DESK_SEAM, "1"), (STATE_SEAM, state.as_str())];
    let (a, sock) = keeper_world::boot(&root, "a", &desk);
    let mut ctl = Ctl::connect(&sock);
    let a_lines = fill_a(&root, &mut ctl, n, spec.lines);
    drop(ctl);
    let a_pid = crash(a);
    keeper_world::await_keeper(&root, &format!("orphaned={n}"));
    let load = load_average();
    let seam = spec.arm.seam(spec.lookalike_kib);
    let mut seams = desk.to_vec();
    seams.extend(seam.as_deref().map(|value| (LOOKALIKE_SEAM, value)));
    let (b, sock) = keeper_world::boot(&root, &format!("after-{a_pid}"), &seams);
    let ready = Instant::now();
    let admitted = keeper_world::admitted_every(&sock, POLL);
    let served_window_ms = ms(served_window_of(
        ready,
        admitted.last_refused,
        admitted.answered,
    ));
    let mut ctl = admitted.ctl;
    let mut round = Round {
        sessions: n,
        birth_to_main_ms: 0.0,
        main_to_hello_ms: 0.0,
        admit_ms: 0.0,
        admit_max_ms: 0.0,
        placed_ms: 0.0,
        settled_ms: None,
        offers: 0,
        seam_copies: 0,
        seam_plants: 0,
        seam_ms: 0.0,
        ready_ms: ms(ready.saturating_duration_since(b.spawned)),
        served_ms: ms(admitted.answered.saturating_duration_since(b.spawned)),
        served_window_ms,
        experiment: Applied::default(),
        restored_ms: 0.0,
        imported_ms: 0.0,
        sweep_max_ms: 0.0,
        rungs: BTreeMap::new(),
        whys: BTreeMap::new(),
        history_lost: 0,
        a_lines: a_lines.values().sum(),
        b_lines: 0,
        load,
    };
    let lost = sweep_b(&mut ctl, &b, &a_lines, &mut round);
    let importing = round.rungs.keys().any(|rung| rung != "meta");
    let metrics = b_metrics(&mut ctl, importing);
    let reading = |key: &str| -> Option<f64> {
        fields(&metrics)
            .get(key)
            .filter(|v| **v != "none")
            .map(|v| {
                v.parse()
                    .unwrap_or_else(|_| panic!("B's `metrics` {key}={v} is no reading"))
            })
    };
    let needed = |key: &str| -> f64 {
        reading(key).unwrap_or_else(|| panic!("B's `metrics` has no {key} reading: {metrics}"))
    };
    let counter = |key: &str| {
        fields(&metrics)
            .get(key)
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or_else(|| panic!("B's `metrics` has no {key} count: {metrics}"))
    };
    round.offers = counter("startup_keeper_offers");
    assert_eq!(
        round.offers,
        u64::from(n),
        "B's HELLO was offered every session A left"
    );
    round.experiment = Applied {
        copies: counter("restore_experiment_copies"),
        plants: counter("restore_experiment_plants"),
        refused: counter("restore_experiment_refused"),
    };
    assert!(
        round.experiment.matches(spec.arm, u64::from(n)),
        "B did not apply the requested {:?} arm to all {n} offers: {:?}",
        spec.arm,
        round.experiment
    );
    round.birth_to_main_ms = needed("process_birth_to_rust_main_ms");
    round.main_to_hello_ms = needed("startup_keeper_hello_ms");
    round.admit_ms = needed("startup_keeper_admit_ms");
    round.admit_max_ms = needed("startup_keeper_admit_max_ms");
    round.placed_ms = needed("startup_keeper_placed_ms");
    round.settled_ms = reading("startup_keeper_imported_ms");
    let copies = reading("startup_keeper_seam_copies").map(|v| v as u64);
    let plants = reading("startup_keeper_seam_plants").map(|v| v as u64);
    round.seam_copies = copies.unwrap_or(0);
    round.seam_plants = plants.unwrap_or(0);
    round.seam_ms = reading("startup_keeper_seam_ms").unwrap_or(0.0);
    if let Err(why) = arm_took(spec.arm, round.offers, copies, plants, &lost) {
        panic!("{why}; B's metrics: {metrics}");
    }
    round
}

/// B's `metrics` once its boot recovery has placed every recovered session and, when it had
/// history to import (`importing`), once those imports settled: a must-happen wait, since both
/// come before the bench's sweep saw every session's import in.
fn b_metrics(ctl: &mut Ctl, importing: bool) -> String {
    let mut last = String::new();
    let seen = aterm_spec::wait::within(HANG, POLL, || {
        last = ctl.ok("metrics");
        let f = fields(&last);
        let set = |key| f.get(key).is_some_and(|v| *v != "none");
        (set("startup_keeper_placed_ms") && (!importing || set("startup_keeper_imported_ms")))
            .then_some(())
    });
    assert!(
        seen.is_some(),
        "B's `metrics` never read every session placed and imported: {last}"
    );
    last
}

/// A's `n` sessions, each printing `lines` lines of history, its journal caught up and its
/// region in the keeper's custody: every session's scrollback line count, by sid.
fn fill_a(root: &Path, ctl: &mut Ctl, n: u32, lines: u32) -> BTreeMap<String, u64> {
    for _ in 1..n {
        let reply = ctl.ok("spawn");
        assert!(
            reply.split_whitespace().any(|t| t.starts_with("s-")),
            "`spawn` names its sid: {reply}"
        );
    }
    let sids = sids(ctl);
    assert_eq!(sids.len(), n as usize, "A runs {n} sessions: {sids:?}");
    for sid in &sids {
        await_field(ctl, sid, "journal", "live");
        await_prompt(ctl, sid, None);
    }
    for (i, sid) in sids.iter().enumerate() {
        ctl.type_line_to(
            sid,
            &format!("seq -f 'line %g' 1 {lines}; echo ARMED-$((1000+{i}))"),
        );
    }
    let mut a_lines = BTreeMap::new();
    for (i, sid) in sids.iter().enumerate() {
        let marker = format!("ARMED-{}", 1000 + i);
        let reply = ctl.line(&format!("@{sid} await match {marker} timeout={AWAIT_MS}"));
        assert!(
            reply.starts_with("OK match "),
            "{sid}: `{marker}` never reached the screen: {reply}"
        );
        await_prompt(ctl, sid, Some(&marker));
        await_field(ctl, sid, "journal_tail", "0");
        await_field(ctl, sid, "journal_ring_lag", "0");
        a_lines.insert(sid.clone(), scrollback(ctl, sid));
    }
    keeper_world::await_keeper(root, &format!("claimed={n}"));
    keeper_world::await_keeper(root, &format!("journals={n}"));
    a_lines
}

/// A SIGKILLed by its own pid, never `pkill`, and reaped: its pid.
fn crash(a: Proc) -> u32 {
    let mut a = a;
    let pid = a.child.id();
    a.child.kill().expect("SIGKILL A");
    let status = a.child.wait().expect("reap A");
    assert!(
        !status.success(),
        "A died by its signal: {status:?} (pid {pid})"
    );
    pid
}

/// Sweep B's sessions until every one reads a `restore=` and every import is in, recording when
/// each first held, the longest sweep, and what the recoveries came back as: every recovered
/// session's `history_lost=`.
fn sweep_b(
    ctl: &mut Ctl,
    b: &Proc,
    a_lines: &BTreeMap<String, u64>,
    round: &mut Round,
) -> Vec<u64> {
    let n = a_lines.len();
    let mut restored = None;
    let mut imported: BTreeMap<String, Duration> = BTreeMap::new();
    let mut sweep_max = Duration::ZERO;
    let mut last: BTreeMap<String, (String, u64)> = BTreeMap::new();
    let mut last_seen = Vec::new();
    let done = aterm_spec::wait::within(HANG, POLL, || {
        let sweep = Instant::now();
        let mut read = BTreeSet::new();
        last_seen = sids(ctl);
        for sid in &last_seen {
            let Some(&expected) = a_lines.get(sid) else {
                continue;
            };
            let status = ctl.line(&format!("@{sid} status"));
            let f = fields(&status);
            let rung = f.get("restore").copied().unwrap_or("-");
            if !matches!(rung, "exact" | "screen" | "meta") {
                continue;
            }
            read.insert(sid.clone());
            let lines = if rung == "meta" {
                0
            } else {
                scrollback(ctl, sid)
            };
            if !imported.contains_key(sid) && import_in(&f, lines, expected) {
                imported.insert(sid.clone(), b.spawned.elapsed());
            }
            last.insert(sid.clone(), (status, lines));
        }
        let (all_restored, all_imported) = sweep_complete(a_lines, &last_seen, &read, &imported);
        sweep_max = sweep_max.max(sweep.elapsed());
        if restored.is_none() && all_restored {
            restored = Some(b.spawned.elapsed());
        }
        all_imported.then_some(())
    });
    assert!(
        done.is_some(),
        "B's {n} recovered sessions never all read a restore= and an import in ({} imported); \
         B's last session IDs: {last_seen:?}; their last statuses: {last:#?}",
        imported.len()
    );
    round.restored_ms = ms(restored.unwrap_or_default());
    round.imported_ms = imported.values().max().copied().map_or(0.0, ms);
    round.sweep_max_ms = ms(sweep_max);
    let mut lost = Vec::with_capacity(last.len());
    for (status, lines) in last.values() {
        let f = fields(status);
        let word = |key| f.get(key).copied().unwrap_or("?").to_string();
        *round.rungs.entry(word("restore")).or_default() += 1;
        *round.whys.entry(word("restore_why")).or_default() += 1;
        let session_lost = f
            .get("history_lost")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        round.history_lost += session_lost;
        lost.push(session_lost);
        round.b_lines += lines;
    }
    lost
}

/// A completed sweep observes exactly A's session IDs, each restored, then each imported.
/// Earlier import observations cannot hide a missing or replacement session in the current
/// sweep. Row order is immaterial; this lane does not judge the order of restored tabs.
pub(crate) fn sweep_complete(
    expected: &BTreeMap<String, u64>,
    current: &[String],
    restored: &BTreeSet<String>,
    imported: &BTreeMap<String, Duration>,
) -> (bool, bool) {
    let present: BTreeSet<_> = current.iter().collect();
    let all_restored = current.len() == expected.len()
        && present.into_iter().eq(expected.keys())
        && restored.iter().eq(expected.keys());
    let all_imported = all_restored && imported.keys().eq(expected.keys());
    (all_restored, all_imported)
}

/// Whether a recovered session's history import is in: a META rung brings none; a screen is in
/// once its scrollback plus what it counts lost reaches A's `expected` lines and B's own journal
/// has caught up with it.
pub(crate) fn import_in(status: &BTreeMap<&str, &str>, lines: u64, expected: u64) -> bool {
    match status.get("restore").copied() {
        Some("meta") => true,
        Some("exact" | "screen") => {
            let lost = status
                .get("history_lost")
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0);
            status.get("journal") == Some(&"live")
                && status.get("journal_ring_lag") == Some(&"0")
                && lines.saturating_add(lost) >= expected
        }
        _ => false,
    }
}

/// The instance's sessions, by sid (`sessions`' rows).
fn sids(ctl: &mut Ctl) -> Vec<String> {
    let (_, rows) = ctl.counted("sessions");
    rows.iter()
        .filter(|row| row.starts_with(|c: char| c.is_ascii_digit()))
        .filter_map(|row| row.split_whitespace().nth(1).map(str::to_string))
        .collect()
}

/// Session `sid`'s scrollback line count (`lines`).
fn scrollback(ctl: &mut Ctl, sid: &str) -> u64 {
    let reply = ctl.ok(&format!("@{sid} lines"));
    reply
        .split_whitespace()
        .nth(1)
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("`@{sid} lines` counts: {reply}"))
}

/// Wait until session `sid`'s `status` has `key=value`.
#[track_caller]
fn await_field(ctl: &mut Ctl, sid: &str, key: &str, value: &str) {
    let mut last = String::new();
    let seen = aterm_spec::wait::within(HANG, keeper_world::GAP, || {
        last = ctl.line(&format!("@{sid} status"));
        (fields(&last).get(key) == Some(&value)).then_some(())
    });
    assert!(seen.is_some(), "{sid}: `{key}={value}` never held: {last}");
}

/// Wait until session `sid`'s shell reads at a prompt: the last non-blank row below the last row
/// holding `after` (anywhere, with `None`) ends in the scratch world's `/bin/sh` prompt.
#[track_caller]
fn await_prompt(ctl: &mut Ctl, sid: &str, after: Option<&str>) {
    let mut last = Vec::new();
    let seen = aterm_spec::wait::within(HANG, keeper_world::GAP, || {
        last = ctl.counted(&format!("@{sid} text")).1;
        let from = match after {
            Some(marker) => last.iter().rposition(|row| row.contains(marker))? + 1,
            None => 0,
        };
        last[from..]
            .iter()
            .rfind(|row| !row.trim().is_empty())
            .filter(|row| row.trim_end().ends_with(['$', '#']))
            .map(|_| ())
    });
    assert!(
        seen.is_some(),
        "{sid}: the shell never drew its prompt (below {after:?}); the screen: {last:?}"
    );
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// `vm.loadavg`'s three figures, or `unavailable`.
fn load_average() -> String {
    aterm_uds::spawnfd::command(aterm_uds::os_path!("sysctl"))
        .args(["-n", "vm.loadavg"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| {
            String::from_utf8_lossy(&out.stdout)
                .trim()
                .trim_matches(|c| c == '{' || c == '}' || c == ' ')
                .to_string()
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unavailable".to_string())
}
