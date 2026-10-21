// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `await sys` (§13.8): the grammar and its evaluation on one snapshot.
//!
//! ```text
//! await sys <predicate> [for=<dur>] [scope=tab|aterm] [timeout=<ms>]
//! <predicate> := quiet [<term> …] | <term> [and <term> …]
//! <term>      := <metric><op><value> | <metric>=<word>[,<word>…]
//!              | (job|task)=<id> <verdict>[,<verdict>…] | (job|task)=<id> class=<class>[,…]
//! <op>        := < | <= | > | >= | = | !=
//! ```
//!
//! The machine metrics this crate measures: `cpu` (busy, `%` of the cores or
//! `c` cores-worth), `cpu.l<n>` / `cpu.<cluster>` (one performance level's
//! busy `%`), `aterm` and `tab` (cores-worth), `wait` (cores-worth of threads
//! waiting for a core, own processes), `gpu` (`%`), `mem` (pressure, ordered
//! `normal < warn < critical`), `swap` (bytes/s in plus out), `decompress`
//! (per second), `thrash` (`yes`/`no`), `tab.mem` and `aterm.mem` (footprint
//! bytes), `throttle` (ordered `none < light < heavy < severe`).
//!
//! `free` and `disk=` are the volume wait (`aterm-gui`'s `control_disk`): a
//! predicate of only those routes there whole ([`Parsed::Volume`]); one that
//! mixes them with machine metrics is refused, since the two waits park on
//! different boards. `eta`, `growth` and `walk` are design P5's: they wait
//! on the folder walker's growth, which needs its change feed and walk cache
//! (§19; the walker `sys disk` reads re-walks on demand only), so each
//! answers `ERR unavailable <metric> why=not_sampled` at arm time rather
//! than parking forever.
//!
//! The stuck engine's terms read the view the host hangs on each snapshot
//! (`Snapshot::jobs`): `jobs.stuck`, `jobs.quiet` and `jobs.running` count the
//! target tab's jobs; `job=<id>` (or `task=<id>`, the agent's task id) names
//! one of the tab's jobs and waits for one of its verdicts (`done` is
//! `exited`) or, with `class=`, one of its classes; the two word sets never
//! mix (§11.7), and a class word given bare is refused as one (`ERR usage:
//! running is a class`). A job the engine has not seen yet reads as warming;
//! one that left the view after it exited (a minute on) is read from the
//! event ring ([`EndedJob`]). An exited job's exit meets `done`, and any other
//! wait on it, in the view or the ring, is `ERR gone job=<id>` (§13.7) at
//! once rather than parked to the timeout, as is a job neither the view nor
//! the ring knows once the engine has looked again ([`Outcome::absent`], the
//! host's call). `scope=aterm` (Owner-only, the access arm's) widens the `jobs.*`
//! counts and the job terms from the target's tab to every tab.
//!
//! `quiet` is `cpu<25% and wait<0.25 and gpu<10% and throttle=none and
//! swap<1MB/s`; an explicit term replaces the preset's term for its metric,
//! and a preset term this host cannot measure drops out, reported as
//! `skipped=<metric>:<why>`. An explicit term this host cannot measure is
//! refused (`ERR unavailable <metric> why=<word>`).
//!
//! LEVEL-TRIGGERED AND LATCHED: the host re-evaluates on every new snapshot;
//! `for=<dur>` needs the predicate to hold on consecutive snapshots spanning
//! `dur` by their stamps ([`Latch`]).

use crate::jobs::{Class, JobRow, Verdict};
use crate::model::{Snapshot, Throttle};

/// The usage line.
pub const USAGE: &str = "ERR usage: await sys <predicate> [for=<dur>] [scope=tab|aterm] \
                         [timeout=<ms>]; \
                         <predicate> := quiet [<term> …] | <term> [and <term> …]; \
                         <term> := <metric><op><value> with <op> < <= > >= = != and <metric> \
                         cpu cpu.l<n> aterm tab wait gpu mem swap decompress thrash tab.mem \
                         aterm.mem throttle jobs.stuck jobs.quiet jobs.running; or \
                         (job|task)=<id> <verdict>[,…]|class=<class>[,…]; or \
                         free>=<size>|disk=<word> (with vol=, alone)\n";

/// The refusal of a predicate that mixes the volume wait with the machine's.
pub const MIXED: &str = "ERR usage: await sys: free and disk= wait on their volume alone; \
                         wait for the machine terms in a second await\n";

/// `timeout=` when not given, ms (the `await` family's default).
pub const TIMEOUT_DEFAULT_MS: u64 = 30_000;
/// …and at most.
pub const TIMEOUT_MAX_MS: u64 = 600_000;

/// What a term compares.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Metric {
    /// Machine busy: `%` of the cores, or `c` cores-worth.
    Cpu,
    /// One performance level's busy `%`: by index (`l0` the fastest) or by
    /// its cluster's name; `label` is what the caller typed after `cpu.`.
    CpuLevel {
        /// `l<n>`'s index.
        index: Option<usize>,
        /// The word typed (`l0`, `super`).
        label: String,
    },
    /// aterm's cores-worth.
    Aterm,
    /// The target tab's cores-worth.
    Tab,
    /// Cores-worth of threads waiting for a core (own processes).
    Wait,
    /// GPU device busy `%`.
    Gpu,
    /// Memory pressure.
    Mem,
    /// Swap traffic, bytes/s.
    Swap,
    /// Decompressions per second.
    Decompress,
    /// Thrashing, `yes`/`no`.
    Thrash,
    /// The target tab's footprint, bytes.
    TabMem,
    /// aterm's footprint, bytes.
    AtermMem,
    /// The thermal level.
    Throttle,
    /// A count of the target tab's jobs (`jobs.stuck`, `jobs.quiet`,
    /// `jobs.running`).
    Jobs(JobCount),
    /// One job, by its id or its agent task id.
    Job(JobRef),
}

/// Which count a `jobs.<word>` term reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobCount {
    /// Jobs whose verdict is `stuck`.
    Stuck,
    /// Jobs whose verdict is `quiet`.
    Quiet,
    /// Live jobs whose class is `running`.
    Running,
}

impl JobCount {
    fn word(self) -> &'static str {
        match self {
            Self::Stuck => "stuck",
            Self::Quiet => "quiet",
            Self::Running => "running",
        }
    }

    fn counts(self, r: &JobRow) -> bool {
        match self {
            Self::Stuck => r.verdict == Verdict::Stuck,
            Self::Quiet => r.verdict == Verdict::Quiet,
            Self::Running => r.class == Class::Running && r.verdict != Verdict::Exited,
        }
    }
}

/// How a `job=`/`task=` term names its job.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobRef {
    /// The row's id (`a40977`).
    Id(String),
    /// The agent's task id for it.
    Task(String),
}

impl JobRef {
    fn matches(&self, r: &JobRow) -> bool {
        self.names(&r.id, r.task.as_deref())
    }

    fn names(&self, id: &str, task: Option<&str>) -> bool {
        match self {
            Self::Id(want) => want == id,
            Self::Task(t) => task == Some(t.as_str()),
        }
    }

    /// `job=<id>` or `task=<id>`, as a reply says it.
    #[must_use]
    pub fn word(&self) -> String {
        match self {
            Self::Id(id) => format!("job={id}"),
            Self::Task(t) => format!("task={t}"),
        }
    }
}

/// A job or task id: `[A-Za-z0-9_-]{1,32}`.
fn job_id(text: &str) -> Option<String> {
    ((1..=32).contains(&text.len())
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'))
    .then(|| text.to_owned())
}

impl Metric {
    /// Its name as the reply prints it.
    #[must_use]
    pub fn name(&self) -> String {
        match self {
            Self::Cpu => "cpu".into(),
            Self::CpuLevel { label, .. } => format!("cpu.{label}"),
            Self::Aterm => "aterm".into(),
            Self::Tab => "tab".into(),
            Self::Wait => "wait".into(),
            Self::Gpu => "gpu".into(),
            Self::Mem => "mem".into(),
            Self::Swap => "swap".into(),
            Self::Decompress => "decompress".into(),
            Self::Thrash => "thrash".into(),
            Self::TabMem => "tab.mem".into(),
            Self::AtermMem => "aterm.mem".into(),
            Self::Throttle => "throttle".into(),
            Self::Jobs(c) => format!("jobs.{}", c.word()),
            Self::Job(_) => "job".into(),
        }
    }

    /// Whether two terms are on the same metric (an explicit term replaces
    /// the preset's).
    fn same(&self, other: &Self) -> bool {
        self.name() == other.name()
    }

    fn kind(&self) -> Kind {
        match self {
            Self::Cpu => Kind::CpuPct,
            Self::CpuLevel { .. } | Self::Gpu => Kind::Pct,
            Self::Aterm | Self::Tab | Self::Wait => Kind::Cores,
            Self::Mem | Self::Throttle => Kind::Ordered,
            Self::Swap => Kind::Bps,
            Self::Decompress => Kind::PerS,
            Self::Thrash => Kind::YesNo,
            Self::TabMem | Self::AtermMem => Kind::Bytes,
            Self::Jobs(_) => Kind::Count,
            Self::Job(_) => Kind::Job,
        }
    }

    fn levels(&self) -> &'static [&'static str] {
        match self {
            Self::Mem => &["normal", "warn", "critical"],
            Self::Throttle => &["none", "light", "heavy", "severe"],
            Self::Thrash => &["no", "yes"],
            _ => &[],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    CpuPct,
    Pct,
    Cores,
    Ordered,
    YesNo,
    Bps,
    PerS,
    Bytes,
    Count,
    Job,
}

/// A comparison.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
    /// `=`
    Eq,
    /// `!=`
    Ne,
}

impl Op {
    fn split(text: &str) -> Option<(Self, &str)> {
        for (w, op) in [
            ("<=", Self::Le),
            (">=", Self::Ge),
            ("!=", Self::Ne),
            ("<", Self::Lt),
            (">", Self::Gt),
            ("=", Self::Eq),
        ] {
            if let Some(rest) = text.strip_prefix(w) {
                return Some((op, rest));
            }
        }
        None
    }

    fn holds<T: PartialOrd>(self, a: T, b: T) -> bool {
        match self {
            Self::Lt => a < b,
            Self::Le => a <= b,
            Self::Gt => a > b,
            Self::Ge => a >= b,
            Self::Eq => a == b,
            Self::Ne => a != b,
        }
    }
}

/// What a term wants.
#[derive(Clone, Debug, PartialEq)]
pub enum Want {
    /// A number in the metric's base unit (percent, cores, bytes, bytes/s,
    /// per second); `cores` marks a `cpu` value given in `c`.
    Num {
        /// The comparison.
        op: Op,
        /// The value.
        v: f64,
        /// A `cpu` value in cores-worth rather than percent.
        cores: bool,
    },
    /// Levels: `=`/`!=` take a set, the order comparisons one level.
    Levels {
        /// The comparison.
        op: Op,
        /// Ranks in the metric's order.
        ranks: Vec<usize>,
    },
    /// A job's verdict, one of these.
    Verdicts(Vec<Verdict>),
    /// A job's class, one of these.
    Classes(Vec<Class>),
}

/// One term.
#[derive(Clone, Debug, PartialEq)]
pub struct Term {
    /// The metric.
    pub metric: Metric,
    /// What it must be.
    pub want: Want,
    /// From the `quiet` preset (drops out where unmeasurable).
    pub preset: bool,
}

/// A parsed machine wait.
#[derive(Clone, Debug, PartialEq)]
pub struct AwaitSys {
    /// The terms, all of which must hold.
    pub terms: Vec<Term>,
    /// `for=`, ms (0 without it).
    pub for_ms: u64,
    /// `timeout=`, ms.
    pub timeout_ms: u64,
    /// `scope=aterm`: the job terms read every tab, not the target's
    /// (Owner-only).
    pub scope_aterm: bool,
}

/// A job that left the engine's view after it exited, as the host's event
/// ring remembers its `exited` event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EndedJob {
    /// Its tab's key.
    pub tab: u64,
    /// Its id.
    pub id: String,
    /// The agent's task id, for an agent's call.
    pub task: Option<String>,
    /// Its exit status, when known.
    pub exit: Option<i32>,
}

impl EndedJob {
    /// The ended job an event says, when it is a job's `exited`.
    #[must_use]
    pub fn of(e: &crate::events::Event) -> Option<Self> {
        match &e.kind {
            crate::events::EventKind::Job {
                tab,
                id,
                to: Verdict::Exited,
                task,
                exit,
                ..
            } => Some(Self {
                tab: *tab,
                id: id.clone(),
                task: task.clone(),
                exit: *exit,
            }),
            _ => None,
        }
    }
}

/// What `await sys …` is.
#[derive(Clone, Debug, PartialEq)]
pub enum Parsed {
    /// A wait on the machine's metrics.
    Machine(AwaitSys),
    /// The volume wait (`free`/`disk=` alone): handed whole to its parser.
    Volume,
}

/// A decimal number.
fn number(text: &str) -> Option<(f64, &str)> {
    let end = text
        .bytes()
        .position(|b| !(b.is_ascii_digit() || b == b'.'))
        .unwrap_or(text.len());
    let (n, rest) = text.split_at(end);
    if n.is_empty() || n.starts_with('.') || n.ends_with('.') || n.matches('.').count() > 1 {
        return None;
    }
    Some((n.parse().ok()?, rest))
}

/// A size unit's bytes.
fn size_unit(unit: &str) -> Option<f64> {
    Some(match unit {
        "B" => 1.0,
        "KB" => 1e3,
        "MB" => 1e6,
        "GB" => 1e9,
        "TB" => 1e12,
        "KiB" => 1024.0,
        "MiB" => 1_048_576.0,
        "GiB" => 1_073_741_824.0,
        "TiB" => 1_099_511_627_776.0,
        _ => return None,
    })
}

/// A duration: `<n>(ms|s|m|h)`, ms.
fn duration_ms(text: &str) -> Option<u64> {
    let (n, unit) = number(text)?;
    let scale = match unit {
        "ms" => 1.0,
        "s" => 1e3,
        "m" => 60e3,
        "h" => 3_600e3,
        _ => return None,
    };
    let ms = n * scale;
    (ms.is_finite() && (0.0..=86_400e3).contains(&ms)).then(|| ms.round() as u64)
}

fn want(metric: &Metric, op: Op, value: &str) -> Result<Want, String> {
    let usage = || USAGE.to_owned();
    match metric.kind() {
        Kind::Ordered | Kind::YesNo => {
            let levels = metric.levels();
            let ranks: Option<Vec<usize>> = value
                .split(',')
                .map(|w| levels.iter().position(|l| *l == w))
                .collect();
            let ranks = ranks.filter(|r| !r.is_empty()).ok_or_else(usage)?;
            let ordered_ok = matches!(op, Op::Eq | Op::Ne)
                || (metric.kind() == Kind::Ordered && ranks.len() == 1);
            if !ordered_ok {
                return Err(usage());
            }
            Ok(Want::Levels { op, ranks })
        }
        kind => {
            let (n, unit) = number(value).ok_or_else(usage)?;
            let (v, cores) = match (kind, unit) {
                (Kind::CpuPct, "%") | (Kind::Pct, "%") => (n, false),
                (Kind::CpuPct, "c") => (n, true),
                (Kind::Count, "") => (n, false),
                (Kind::Cores, "c" | "") | (Kind::PerS, "" | "/s") => (n, false),
                (Kind::Bytes, u) => (n * size_unit(u).ok_or_else(usage)?, false),
                (Kind::Bps, u) => (
                    n * size_unit(u.strip_suffix("/s").unwrap_or(u)).ok_or_else(usage)?,
                    false,
                ),
                _ => return Err(usage()),
            };
            Ok(Want::Num { op, v, cores })
        }
    }
}

/// The reply refusing a metric this build does not measure.
fn not_sampled(metric: &str) -> String {
    format!("ERR unavailable {metric} why=not_sampled\n")
}

/// One `<metric><op><value>` token.
enum Tok {
    Term(Term),
    Volume,
    /// `job=<id>` or `task=<id>`: its words are the next token.
    Job(JobRef),
}

/// The words after `job=<id>`: verdicts (`done` is `exited`), or `class=`
/// and classes. A class word given bare is refused as one (§13.7): the two
/// word sets never mix.
fn job_want(words: &str) -> Result<Want, String> {
    if let Some(classes) = words.strip_prefix("class=") {
        let cs: Option<Vec<Class>> = classes.split(',').map(Class::from_word).collect();
        return cs
            .filter(|c| !c.is_empty())
            .map(Want::Classes)
            .ok_or_else(|| USAGE.to_owned());
    }
    let mut vs = Vec::new();
    for w in words.split(',') {
        match w {
            "done" => vs.push(Verdict::Exited),
            w => match Verdict::from_word(w) {
                Some(v) => vs.push(v),
                None if Class::from_word(w).is_some() => {
                    return Err(format!(
                        "ERR usage: {w} is a class; say class={w} (verdicts go bare)\n"
                    ));
                }
                None => return Err(USAGE.to_owned()),
            },
        }
    }
    if vs.is_empty() {
        return Err(USAGE.to_owned());
    }
    Ok(Want::Verdicts(vs))
}

fn term(tok: &str) -> Result<Tok, String> {
    let name_len = tok
        .bytes()
        .position(|b| !(b.is_ascii_alphanumeric() || b == b'.' || b == b'_'))
        .unwrap_or(tok.len());
    let (name, rest) = tok.split_at(name_len);
    let Some((op, value)) = Op::split(rest) else {
        return Err(USAGE.to_owned());
    };
    let metric = match name {
        "free" | "disk" => return Ok(Tok::Volume),
        "eta" | "growth" | "walk" => return Err(not_sampled(name)),
        "job" | "task" => {
            let id = job_id(value).filter(|_| op == Op::Eq).ok_or(USAGE)?;
            return Ok(Tok::Job(if name == "job" {
                JobRef::Id(id)
            } else {
                JobRef::Task(id)
            }));
        }
        "jobs.stuck" => Metric::Jobs(JobCount::Stuck),
        "jobs.quiet" => Metric::Jobs(JobCount::Quiet),
        "jobs.running" => Metric::Jobs(JobCount::Running),
        "cpu" => Metric::Cpu,
        "aterm" => Metric::Aterm,
        "tab" => Metric::Tab,
        "wait" => Metric::Wait,
        "gpu" => Metric::Gpu,
        "mem" => Metric::Mem,
        "swap" => Metric::Swap,
        "decompress" => Metric::Decompress,
        "thrash" => Metric::Thrash,
        "tab.mem" => Metric::TabMem,
        "aterm.mem" => Metric::AtermMem,
        "throttle" => Metric::Throttle,
        n => match n.strip_prefix("cpu.") {
            Some(label)
                if !label.is_empty()
                    && label.len() <= 24
                    && label
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_') =>
            {
                let index = label
                    .strip_prefix('l')
                    .and_then(|d| d.parse::<usize>().ok())
                    .filter(|i| *i < 8);
                Metric::CpuLevel {
                    index,
                    label: label.to_ascii_lowercase(),
                }
            }
            _ => return Err(USAGE.to_owned()),
        },
    };
    let want = want(&metric, op, value)?;
    Ok(Tok::Term(Term {
        metric,
        want,
        preset: false,
    }))
}

/// The `quiet` preset's terms.
#[must_use]
pub fn quiet_terms() -> Vec<Term> {
    let num = |metric, op, v| Term {
        metric,
        want: Want::Num {
            op,
            v,
            cores: false,
        },
        preset: true,
    };
    vec![
        num(Metric::Cpu, Op::Lt, 25.0),
        num(Metric::Wait, Op::Lt, 0.25),
        num(Metric::Gpu, Op::Lt, 10.0),
        Term {
            metric: Metric::Throttle,
            want: Want::Levels {
                op: Op::Eq,
                ranks: vec![0],
            },
            preset: true,
        },
        num(Metric::Swap, Op::Lt, 1e6),
    ]
}

impl AwaitSys {
    /// Parse the arguments after `await` (`sys <predicate> …`).
    ///
    /// # Errors
    ///
    /// The reply refusing it: [`USAGE`], [`MIXED`], or `ERR unavailable
    /// <metric> why=not_sampled` for a metric this build does not measure.
    pub fn parse(rest: &str) -> Result<Parsed, String> {
        let mut toks = rest.split_whitespace().peekable();
        if toks.next() != Some("sys") {
            return Err(USAGE.to_owned());
        }
        let mut terms: Vec<Term> = Vec::new();
        let mut volume = false;
        let mut vol_given = false;
        let mut quiet = false;
        let mut for_ms = 0u64;
        let mut timeout_ms = TIMEOUT_DEFAULT_MS;
        let mut scope_aterm = false;
        // `and` joins two terms; after `quiet` terms follow with or without
        // it. `after_and`: the last word was `and`; `first`: no term or
        // `quiet` yet.
        let mut after_and = false;
        let mut first = true;
        while let Some(tok) = toks.next() {
            if let Some(v) = tok.strip_prefix("timeout=") {
                timeout_ms = v.parse::<u64>().map_err(|_| USAGE)?.min(TIMEOUT_MAX_MS);
                continue;
            }
            if tok == "timeout" {
                let v = toks.next().ok_or(USAGE)?;
                timeout_ms = v.parse::<u64>().map_err(|_| USAGE)?.min(TIMEOUT_MAX_MS);
                continue;
            }
            if let Some(v) = tok.strip_prefix("for=") {
                for_ms = duration_ms(v).ok_or(USAGE)?;
                continue;
            }
            if let Some(v) = tok.strip_prefix("scope=") {
                scope_aterm = match v {
                    "tab" => false,
                    "aterm" => true,
                    _ => return Err(USAGE.to_owned()),
                };
                continue;
            }
            if tok.starts_with("vol=") {
                vol_given = true;
                continue;
            }
            if tok == "quiet" {
                if !first {
                    return Err(USAGE.to_owned());
                }
                quiet = true;
                first = false;
                continue;
            }
            if tok == "and" {
                if after_and || first {
                    return Err(USAGE.to_owned());
                }
                after_and = true;
                continue;
            }
            if !first && !after_and && !quiet {
                return Err(USAGE.to_owned());
            }
            first = false;
            after_and = false;
            match term(tok)? {
                Tok::Volume => volume = true,
                Tok::Term(t) => terms.push(t),
                Tok::Job(job) => {
                    let want = job_want(toks.next().ok_or(USAGE)?)?;
                    terms.push(Term {
                        metric: Metric::Job(job),
                        want,
                        preset: false,
                    });
                }
            }
        }
        if after_and {
            return Err(USAGE.to_owned());
        }
        if volume {
            return if terms.is_empty() && !quiet {
                Ok(Parsed::Volume)
            } else {
                Err(MIXED.to_owned())
            };
        }
        if vol_given || (terms.is_empty() && !quiet) {
            return Err(USAGE.to_owned());
        }
        if quiet {
            let explicit = std::mem::take(&mut terms);
            terms = quiet_terms()
                .into_iter()
                .filter(|p| !explicit.iter().any(|e| e.metric.same(&p.metric)))
                .collect();
            terms.extend(explicit);
        }
        Ok(Parsed::Machine(Self {
            terms,
            for_ms,
            timeout_ms,
            scope_aterm,
        }))
    }
}

/// One metric's reading on a snapshot.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Reading {
    /// A number in the metric's base unit.
    Num(f64),
    /// A level's rank.
    Rank(usize),
    /// Not known yet (the first sample has no interval).
    Warming,
    /// Not measurable here.
    Unavailable(&'static str),
}

/// The target's sessions, for `tab` and `tab.mem`. A session the snapshot
/// does not hold yet (the layout reached the probe after it read) is warming:
/// the target's exit, not its absence from one sample, ends the wait.
fn tab_sum(
    s: &Snapshot,
    keys: &[u64],
    f: impl Fn(&crate::model::TabView) -> Option<f64>,
) -> Reading {
    if s.aterm.is_none() {
        return Reading::Warming;
    }
    let mut found = false;
    let mut total = None;
    for k in keys {
        if let Some(t) = s.tab(*k) {
            found = true;
            if let Some(v) = f(t) {
                *total.get_or_insert(0.0) += v;
            }
        }
    }
    match (found, total) {
        (_, Some(v)) => Reading::Num(v),
        (true, None) => Reading::Unavailable("refused"),
        (false, None) => Reading::Warming,
    }
}

fn read(metric: &Metric, s: &Snapshot, tab_keys: &[u64], cores_want: bool) -> Reading {
    match metric {
        Metric::Cpu => match s.cpu.as_ref() {
            Some(c) if cores_want => Reading::Num(f64::from(c.busy.raw.max(0.0))),
            Some(c) => {
                Reading::Num(f64::from(c.busy.raw.max(0.0)) / (c.total().max(1) as f64) * 100.0)
            }
            None => Reading::Warming,
        },
        Metric::CpuLevel { index, label } => {
            let Some(cpu) = s.cpu.as_ref() else {
                return Reading::Warming;
            };
            let found = match index {
                Some(i) => cpu.clusters.get(*i),
                None => cpu.clusters.iter().find(|c| {
                    c.name.eq_ignore_ascii_case(label) || c.short.eq_ignore_ascii_case(label)
                }),
            };
            match found {
                Some(c) if !c.cores.is_empty() => {
                    let busy: f32 = c.cores.iter().map(|&i| cpu.cores[i].load.raw).sum();
                    Reading::Num(f64::from(busy.max(0.0)) / c.cores.len() as f64 * 100.0)
                }
                _ => Reading::Unavailable("unsupported"),
            }
        }
        Metric::Aterm => s.aterm.as_ref().map_or(Reading::Warming, |a| {
            Reading::Num(f64::from(a.cores.raw.max(0.0)))
        }),
        Metric::AtermMem => match s.aterm.as_ref() {
            None => Reading::Warming,
            Some(a) => a
                .footprint
                .map_or(Reading::Unavailable("refused"), |b| Reading::Num(b as f64)),
        },
        Metric::Tab => tab_sum(s, tab_keys, |t| Some(f64::from(t.scope.cores.raw.max(0.0)))),
        Metric::TabMem => tab_sum(s, tab_keys, |t| t.scope.footprint.map(|b| b as f64)),
        Metric::Wait => match s.global.wait {
            Some(w) => Reading::Num(f64::from(w)),
            None if s.procs_window_ns.is_none() => Reading::Warming,
            None => Reading::Unavailable("not_sampled"),
        },
        Metric::Gpu => s
            .gpu
            .as_ref()
            .map_or(Reading::Unavailable("unsupported"), |g| {
                Reading::Num(f64::from(g.load.raw.max(0.0)) * 100.0)
            }),
        Metric::Mem => match s.mem.as_ref().map(|m| m.sample.pressure) {
            Some(p) => p.rank().map_or(Reading::Unavailable("unsupported"), |r| {
                Reading::Rank(r.into())
            }),
            None => Reading::Unavailable("unsupported"),
        },
        Metric::Swap | Metric::Decompress | Metric::Thrash => {
            let Some(m) = s.mem.as_ref() else {
                return Reading::Unavailable("unsupported");
            };
            if m.sample.counters.is_none() {
                return Reading::Unavailable("not_sampled");
            }
            let Some(r) = m.rates else {
                return Reading::Warming;
            };
            match metric {
                Metric::Swap => Reading::Num(r.swap_bps()),
                Metric::Decompress => Reading::Num(r.decompress_ps),
                _ => match m.thrashing() {
                    Some(t) => Reading::Rank(usize::from(t)),
                    None => Reading::Unavailable("unsupported"),
                },
            }
        }
        Metric::Jobs(c) => match s.jobs.as_deref() {
            Some(v) => Reading::Num(
                crate::sys::jobs_view::rows_in(v, tab_keys)
                    .iter()
                    .filter(|r| c.counts(r))
                    .count() as f64,
            ),
            None => jobs_missing(s),
        },
        Metric::Job(_) => Reading::Unavailable("not_sampled"),
        Metric::Throttle => s.thermal.map_or(Reading::Unavailable("unsupported"), |t| {
            Reading::Rank(match t.level {
                Throttle::None => 0,
                Throttle::Light => 1,
                Throttle::Heavy => 2,
                Throttle::Severe => 3,
            })
        }),
    }
}

/// A snapshot with no jobs view: the first sample is warming; after it, the
/// host reads no jobs here.
fn jobs_missing(s: &Snapshot) -> Reading {
    match crate::sys::jobs_view::missing_why(s) {
        "warming" => Reading::Warming,
        why => Reading::Unavailable(why),
    }
}

/// The target tab's job `job` names, as a term reads it.
enum JobRead<'a> {
    Row(&'a JobRow),
    /// It left the view after it exited; the ring remembers it.
    Ended(&'a EndedJob),
    /// The view does not hold it and the ring does not remember it: not
    /// read yet, or gone (the host tells them apart).
    Absent,
    Warming,
    Unavailable(&'static str),
}

fn job_read<'a>(
    job: &JobRef,
    s: &'a Snapshot,
    tab_keys: &[u64],
    ended: &'a [EndedJob],
) -> JobRead<'a> {
    let Some(v) = s.jobs.as_deref() else {
        return match jobs_missing(s) {
            Reading::Unavailable(why) => JobRead::Unavailable(why),
            _ => JobRead::Warming,
        };
    };
    if let Some(r) = crate::sys::jobs_view::rows_in(v, tab_keys)
        .into_iter()
        .find(|r| job.matches(r))
    {
        return JobRead::Row(r);
    }
    ended
        .iter()
        .rev()
        .find(|e| tab_keys.contains(&e.tab) && job.names(&e.id, e.task.as_deref()))
        .map_or(JobRead::Absent, JobRead::Ended)
}

/// `job=<id> verdict=<v> class=<c> [quiet_ms=] [phi=] [exit=]`, or
/// `job=<ref> verdict=warming` before the engine has seen it.
fn job_words(job: &JobRef, r: Option<&JobRow>) -> String {
    let Some(r) = r else {
        return match job {
            JobRef::Id(id) => format!("job={id} verdict=warming"),
            JobRef::Task(t) => format!("task={t} verdict=warming"),
        };
    };
    let mut w = format!(
        "job={} verdict={} class={}",
        r.id,
        r.verdict.word(),
        r.class.word()
    );
    if let Some(wait) = &r.wait {
        w.push_str(&format!(" wait={}", wait.name.word()));
    }
    if r.verdict != Verdict::Exited {
        if let Some(q) = r.quiet_ms {
            w.push_str(&format!(" quiet_ms={q}"));
        }
        if let Some(p) = r.phi_milli.filter(|p| *p > 0) {
            w.push_str(&format!(" phi={}", crate::jobs::wire::phi_text(p)));
        }
    }
    if let Some(e) = r.exit.filter(|_| r.verdict == Verdict::Exited) {
        w.push_str(&format!(" exit={e}"));
    }
    w
}

/// A byte count as the reply prints it: `0B`, `310MB`, `1.2GB`.
#[must_use]
pub fn size_word(b: f64) -> String {
    let b = b.max(0.0);
    let (v, unit) = if b >= 1e12 {
        (b / 1e12, "TB")
    } else if b >= 1e9 {
        (b / 1e9, "GB")
    } else if b >= 1e6 {
        (b / 1e6, "MB")
    } else if b >= 1e3 {
        (b / 1e3, "KB")
    } else {
        return format!("{b:.0}B");
    };
    if v >= 100.0 {
        format!("{v:.0}{unit}")
    } else {
        format!("{v:.1}{unit}")
    }
}

fn value_word(metric: &Metric, r: Reading, cores_want: bool) -> String {
    match r {
        Reading::Warming => "warming".into(),
        Reading::Unavailable(w) => format!("unavailable({w})"),
        Reading::Rank(i) => metric.levels().get(i).copied().unwrap_or("-").into(),
        Reading::Num(v) => match metric.kind() {
            Kind::CpuPct if cores_want => format!("{v:.2}c"),
            Kind::CpuPct | Kind::Pct => format!("{:.0}%", v.round()),
            Kind::Cores if *metric == Metric::Wait => format!("{v:.2}"),
            Kind::Cores => format!("{v:.2}c"),
            Kind::Bps => format!("{}/s", size_word(v)),
            Kind::PerS => format!("{:.0}/s", v.round()),
            Kind::Bytes => size_word(v),
            Kind::Count => format!("{v:.0}"),
            Kind::Ordered | Kind::YesNo | Kind::Job => "-".into(),
        },
    }
}

/// One evaluation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    /// Every kept term holds on this snapshot.
    pub holds: bool,
    /// `<metric>=<value>` for every kept term, in order.
    pub values: Vec<String>,
    /// `<metric>:<why>` for each preset term that dropped out.
    pub skipped: Vec<String>,
    /// The refusal of an explicit term this host cannot measure, or of a
    /// wait on a job that ended without meeting it (`ERR gone job=<id>`).
    pub refused: Option<String>,
    /// The job terms (`job=<id>`, `task=<id>`) whose job neither the view
    /// nor the ring holds: not read yet, or gone; the host says `ERR gone`
    /// once the engine has looked again and still not found it.
    pub absent: Vec<String>,
}

fn holds(want: &Want, r: Reading) -> Option<bool> {
    match (want, r) {
        (Want::Num { op, v, .. }, Reading::Num(x)) => Some(op.holds(x, *v)),
        (Want::Levels { op, ranks }, Reading::Rank(x)) => Some(match op {
            Op::Eq => ranks.contains(&x),
            Op::Ne => !ranks.contains(&x),
            other => other.holds(x, ranks[0]),
        }),
        _ => None,
    }
}

/// Evaluate `wait` on `s`, the target's sessions being `tab_keys`.
#[must_use]
pub fn evaluate(wait: &AwaitSys, s: &Snapshot, tab_keys: &[u64]) -> Outcome {
    evaluate_with(wait, s, tab_keys, &[])
}

/// [`evaluate`], with the jobs that left the view after they exited
/// (`ended`, from the host's event ring).
#[must_use]
pub fn evaluate_with(
    wait: &AwaitSys,
    s: &Snapshot,
    tab_keys: &[u64],
    ended: &[EndedJob],
) -> Outcome {
    let mut out = Outcome {
        holds: true,
        values: Vec::new(),
        skipped: Vec::new(),
        refused: None,
        absent: Vec::new(),
    };
    for t in &wait.terms {
        if let Metric::Job(job) = &t.metric {
            match job_read(job, s, tab_keys, ended) {
                JobRead::Unavailable(why) => {
                    out.refused = Some(format!("ERR unavailable job why={why}\n"));
                    out.holds = false;
                    return out;
                }
                JobRead::Warming => {
                    out.values.push(job_words(job, None));
                    out.holds = false;
                }
                JobRead::Absent => {
                    out.values.push(job_words(job, None));
                    out.absent.push(job.word());
                    out.holds = false;
                }
                JobRead::Ended(e) => {
                    let met = match &t.want {
                        Want::Verdicts(vs) => vs.contains(&Verdict::Exited),
                        Want::Classes(cs) => cs.contains(&Class::Exited),
                        _ => false,
                    };
                    if !met {
                        // It ended without meeting the wait, and never will.
                        out.refused = Some(format!("ERR gone job={}\n", e.id));
                        out.holds = false;
                        return out;
                    }
                    let mut w = format!("job={} verdict=exited class=exited", e.id);
                    if let Some(code) = e.exit {
                        w.push_str(&format!(" exit={code}"));
                    }
                    out.values.push(w);
                }
                JobRead::Row(r) => {
                    out.values.push(job_words(job, Some(r)));
                    let met = match &t.want {
                        Want::Verdicts(vs) => vs.contains(&r.verdict),
                        Want::Classes(cs) => cs.contains(&r.class),
                        _ => false,
                    };
                    if !met && r.verdict == Verdict::Exited {
                        // It ended without meeting the wait, and never will.
                        out.refused = Some(format!("ERR gone job={}\n", r.id));
                        out.holds = false;
                        return out;
                    }
                    if !met {
                        out.holds = false;
                    }
                }
            }
            continue;
        }
        let cores_want = matches!(t.want, Want::Num { cores: true, .. });
        let r = read(&t.metric, s, tab_keys, cores_want);
        if let Reading::Unavailable(why) = r {
            if t.preset {
                out.skipped.push(format!("{}:{why}", t.metric.name()));
                continue;
            }
            out.refused = Some(format!("ERR unavailable {} why={why}\n", t.metric.name()));
            out.holds = false;
            return out;
        }
        out.values.push(format!(
            "{}={}",
            t.metric.name(),
            value_word(&t.metric, r, cores_want)
        ));
        if holds(&t.want, r) != Some(true) {
            out.holds = false;
        }
    }
    out
}

/// `for=`: holds over consecutive snapshots spanning the duration, timed by
/// their stamps.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Latch {
    since_ns: Option<u64>,
    last_ns: Option<u64>,
}

impl Latch {
    /// Feed one snapshot's verdict; returns how long it has held, ms, once
    /// that spans `for_ms` (at once when `for_ms` is 0). A snapshot already
    /// seen (the same stamp) changes nothing.
    pub fn step(&mut self, holds: bool, at_ns: u64, for_ms: u64) -> Option<u64> {
        if self.last_ns == Some(at_ns) {
            let since = self.since_ns?;
            let held = at_ns.saturating_sub(since) / 1_000_000;
            return (held >= for_ms).then_some(held);
        }
        self.last_ns = Some(at_ns);
        if !holds {
            self.since_ns = None;
            return None;
        }
        let since = *self.since_ns.get_or_insert(at_ns);
        let held = at_ns.saturating_sub(since) / 1_000_000;
        (held >= for_ms).then_some(held)
    }
}

/// `OK sys <values> [skipped=…] held_ms=<n> as_of=<hh:mm:ss> next=<cursor>`.
#[must_use]
pub fn met_reply(o: &Outcome, held_ms: u64, as_of_unix_ms: u64, cursor: &str) -> String {
    let mut r = String::from("OK sys");
    for v in &o.values {
        r.push(' ');
        r.push_str(v);
    }
    if !o.skipped.is_empty() {
        r.push_str(&format!(" skipped={}", o.skipped.join(",")));
    }
    r.push_str(&format!(
        " held_ms={held_ms} as_of={} next={cursor}\n",
        super::clock_s(as_of_unix_ms)
    ));
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        ClusterView, CoreView, CpuView, GpuView, MemCounters, MemRates, MemSample, MemView,
        Pressure, ScopeView, TabView, Thermal,
    };
    use crate::trend::TrendView;

    fn machine(s: &str) -> AwaitSys {
        match AwaitSys::parse(s).unwrap() {
            Parsed::Machine(w) => w,
            Parsed::Volume => panic!("{s} routed to the volume"),
        }
    }

    fn snap(busy_per_core: f32) -> Snapshot {
        let cores: Vec<CoreView> = (0..10)
            .map(|_| CoreView {
                load: TrendView::flat(busy_per_core),
            })
            .collect();
        Snapshot {
            at_ns: 5_000_000_000,
            seq: 3,
            cpu: Some(CpuView {
                busy: TrendView::flat(busy_per_core * 10.0),
                clusters: vec![
                    ClusterView {
                        name: "Super".into(),
                        short: "S".into(),
                        cores: (0..4).collect(),
                    },
                    ClusterView {
                        name: "Efficiency".into(),
                        short: "E".into(),
                        cores: (4..10).collect(),
                    },
                ],
                cores,
                history: Vec::new(),
                core_history: Vec::new(),
            }),
            gpu: Some(GpuView {
                load: TrendView::flat(0.02),
                ..GpuView::default()
            }),
            mem: Some(MemView {
                sample: MemSample {
                    total: 16 << 30,
                    pressure: Pressure::Normal,
                    counters: Some(MemCounters::default()),
                    ..MemSample::default()
                },
                rates: Some(MemRates::default()),
                ..MemView::default()
            }),
            global: ScopeView {
                wait: Some(0.05),
                ..ScopeView::default()
            },
            aterm: Some(ScopeView {
                cores: TrendView::flat(1.5),
                footprint: Some(800 << 20),
                ..ScopeView::default()
            }),
            tabs: vec![TabView {
                key: 7,
                scope: ScopeView {
                    cores: TrendView::flat(0.12),
                    footprint: Some(310_000_000),
                    ..ScopeView::default()
                },
                ..TabView::default()
            }],
            procs_window_ns: Some(1_000_000_000),
            thermal: Some(Thermal::default()),
            ..Snapshot::default()
        }
    }

    #[test]
    fn the_grammar_takes_every_metric_with_its_unit() {
        for ok in [
            "sys cpu<25%",
            "sys cpu<2.5c",
            "sys cpu.l0>50%",
            "sys cpu.super<=80%",
            "sys aterm<1c",
            "sys tab<0.5c for=10s",
            "sys wait<0.25",
            "sys gpu<10%",
            "sys mem=normal",
            "sys mem<critical",
            "sys mem=normal,warn",
            "sys mem!=critical and swap<1MB/s for=30s timeout=300000",
            "sys swap<1MiB",
            "sys decompress<100/s",
            "sys thrash=no",
            "sys tab.mem<2GB",
            "sys aterm.mem<4GiB",
            "sys throttle=none",
            "sys throttle<heavy",
            "sys quiet",
            "sys quiet for=20s timeout=600000",
            "sys quiet cpu<30%",
            "sys quiet and mem=normal",
            "sys timeout 5000 cpu<50%",
        ] {
            assert!(
                matches!(AwaitSys::parse(ok), Ok(Parsed::Machine(_))),
                "{ok}"
            );
        }
    }

    #[test]
    fn refusals_name_what_is_wrong() {
        for bad in [
            "sys",
            "cpu<25%",
            "sys cpu<25",
            "sys cpu<25MB",
            "sys gpu<1c",
            "sys tab.mem<2",
            "sys swap<1",
            "sys mem<normal,warn",
            "sys mem=calm",
            "sys thrash<yes",
            "sys bogus<1",
            "sys cpu<25% gpu<10%",
            "sys cpu<25% and",
            "sys and cpu<25%",
            "sys cpu<25% for=10",
            "sys cpu<25% vol=/",
            "sys cpu<25% quiet",
            "sys cpu<25% scope=window",
            "sys cpu.<1%",
        ] {
            assert_eq!(AwaitSys::parse(bad), Err(USAGE.to_owned()), "{bad}");
        }
        assert_eq!(
            AwaitSys::parse("sys free>=10GB and cpu<5%"),
            Err(MIXED.to_owned())
        );
        assert_eq!(
            AwaitSys::parse("sys quiet free>=10GB"),
            Err(MIXED.to_owned())
        );
        for (t, metric) in [
            ("sys growth<1MB/s for=30s", "growth"),
            ("sys walk=complete", "walk"),
            ("sys eta>1h", "eta"),
        ] {
            assert_eq!(
                AwaitSys::parse(t),
                Err(format!("ERR unavailable {metric} why=not_sampled\n")),
                "{t}"
            );
        }
    }

    #[test]
    fn job_terms_parse_as_verdicts_or_classes_never_both() {
        let w = machine("sys job=a40977 done,stuck timeout=600000");
        assert_eq!(w.terms[0].metric, Metric::Job(JobRef::Id("a40977".into())));
        assert_eq!(
            w.terms[0].want,
            Want::Verdicts(vec![Verdict::Exited, Verdict::Stuck])
        );
        let w = machine("sys task=b4u3h4kc3 class=waiting,exited and jobs.stuck=0");
        assert_eq!(
            w.terms[0].metric,
            Metric::Job(JobRef::Task("b4u3h4kc3".into()))
        );
        assert_eq!(
            w.terms[0].want,
            Want::Classes(vec![Class::Waiting, Class::Exited])
        );
        assert_eq!(w.terms[1].metric, Metric::Jobs(JobCount::Stuck));
        for bad in [
            "sys job=a1",
            "sys job=a1 bogus",
            "sys job=a1 class=stuck",
            "sys job=a1 done,class=running",
            "sys job>a1 done",
            "sys job=a/1 done",
            "sys job=a1 done jobs.stuck=0",
            "sys jobs.stuck>1c",
            "sys jobs.lost=0",
        ] {
            assert_eq!(AwaitSys::parse(bad), Err(USAGE.to_owned()), "{bad}");
        }
        // A class word given bare is named as one (§13.7).
        assert_eq!(
            AwaitSys::parse("sys job=a1 running"),
            Err("ERR usage: running is a class; say class=running (verdicts go bare)\n".to_owned())
        );
        assert_eq!(
            AwaitSys::parse("sys job=a1 done,waiting"),
            Err("ERR usage: waiting is a class; say class=waiting (verdicts go bare)\n".to_owned())
        );
        // scope= is read, not dropped: aterm widens the job terms.
        assert!(!machine("sys jobs.stuck=0").scope_aterm);
        assert!(!machine("sys jobs.stuck=0 scope=tab").scope_aterm);
        assert!(machine("sys jobs.stuck=0 scope=aterm").scope_aterm);
        assert_eq!(
            AwaitSys::parse("sys jobs.stuck=0 scope=machine"),
            Err(USAGE.to_owned())
        );
    }

    #[test]
    fn a_job_that_left_the_view_is_read_from_the_ring() {
        use crate::jobs::{Demand, Findings, JobsView};
        use crate::sys::jobs_view::fixtures::row;
        let mut s = snap(0.1);
        s.procs_window_ns = Some(2_000_000_000);
        s.jobs = Some(std::sync::Arc::new(JobsView {
            rows: vec![row("a44120", 7, Class::Running, Verdict::Fine)],
            findings: Findings::default(),
            demand: Demand::Fast,
            lanes_off: Vec::new(),
        }));
        let ended = [
            EndedJob {
                tab: 7,
                id: "a1".into(),
                task: Some("tz9k4m2qa".into()),
                exit: Some(3),
            },
            EndedJob {
                tab: 9,
                id: "a9".into(),
                task: None,
                exit: Some(0),
            },
        ];
        // Its exit meets done, by id or by the agent's task id.
        let o = evaluate_with(&machine("sys job=a1 done"), &s, &[7], &ended);
        assert!(o.holds, "{o:?}");
        assert_eq!(o.values, ["job=a1 verdict=exited class=exited exit=3"]);
        let o = evaluate_with(
            &machine("sys task=tz9k4m2qa class=exited"),
            &s,
            &[7],
            &ended,
        );
        assert!(o.holds, "{o:?}");
        // A wait it can no longer meet is refused at once, not parked.
        let o = evaluate_with(&machine("sys job=a1 stuck"), &s, &[7], &ended);
        assert!(!o.holds);
        assert_eq!(o.refused.as_deref(), Some("ERR gone job=a1\n"));
        // NEGATIVE CONTROL: another tab's ended job is not this tab's, and
        // a job nobody knows is absent (the host's grace decides gone).
        let o = evaluate_with(&machine("sys job=a9 done"), &s, &[7], &ended);
        assert!(!o.holds && o.refused.is_none(), "{o:?}");
        assert_eq!(o.absent, ["job=a9"]);
        // …unless the wait is aterm-wide.
        let o = evaluate_with(&machine("sys job=a9 done scope=aterm"), &s, &[7, 9], &ended);
        assert!(o.holds, "{o:?}");
        // An exited row still in the view: its exit meets done, and a wait it
        // can no longer meet is gone at once.
        let mut s2 = s.clone();
        s2.jobs = Some(std::sync::Arc::new(JobsView {
            rows: vec![row("a5", 7, Class::Exited, Verdict::Exited)],
            findings: Findings::default(),
            demand: Demand::Fast,
            lanes_off: Vec::new(),
        }));
        assert!(evaluate(&machine("sys job=a5 done"), &s2, &[7]).holds);
        assert_eq!(
            evaluate(&machine("sys job=a5 stuck"), &s2, &[7])
                .refused
                .as_deref(),
            Some("ERR gone job=a5\n")
        );
        // A row still in the view wins over the ring.
        let o = evaluate_with(&machine("sys job=a44120 class=running"), &s, &[7], &ended);
        assert!(o.holds && o.absent.is_empty(), "{o:?}");
    }

    #[test]
    fn job_terms_read_the_engines_rows_of_the_target_tab_only() {
        use crate::jobs::{Demand, Findings, JobsView};
        use crate::sys::jobs_view::fixtures::{row, stuck};
        let mut s = snap(0.1);
        s.procs_window_ns = Some(2_000_000_000);
        s.jobs = Some(std::sync::Arc::new(JobsView {
            rows: vec![
                stuck("a40977", 7),
                row("a44120", 7, Class::Running, Verdict::Fine),
                stuck("a9", 9),
            ],
            findings: Findings::default(),
            demand: Demand::Fast,
            lanes_off: Vec::new(),
        }));
        let o = evaluate(&machine("sys jobs.stuck=1 and jobs.running>=1"), &s, &[7]);
        assert!(o.holds, "{o:?}");
        assert_eq!(o.values, ["jobs.stuck=1", "jobs.running=1"]);
        let o = evaluate(&machine("sys job=a40977 done,stuck"), &s, &[7]);
        assert!(o.holds, "{o:?}");
        assert_eq!(
            o.values,
            ["job=a40977 verdict=stuck class=waiting wait=tcp quiet_ms=1152000 phi=8.34"]
        );
        // NEGATIVE CONTROL: the class words do not hold for a verdict, and
        // another tab's job is not this tab's (it reads as not seen yet).
        let o = evaluate(&machine("sys job=a40977 class=running"), &s, &[7]);
        assert!(!o.holds);
        let o = evaluate(&machine("sys job=a9 stuck"), &s, &[7]);
        assert!(!o.holds);
        assert_eq!(o.values, ["job=a9 verdict=warming"]);
        let o = evaluate(&machine("sys task=bq7f2k1za class=running"), &s, &[7]);
        assert!(
            !o.holds,
            "the first row by id carrying the task is the stuck one"
        );
        // A host that reads no jobs refuses the terms at arm.
        s.jobs = None;
        let o = evaluate(&machine("sys jobs.stuck=0"), &s, &[7]);
        assert_eq!(
            o.refused.as_deref(),
            Some("ERR unavailable jobs.stuck why=not_sampled\n")
        );
        let o = evaluate(&machine("sys job=a40977 done"), &s, &[7]);
        assert_eq!(
            o.refused.as_deref(),
            Some("ERR unavailable job why=not_sampled\n")
        );
        // …and the first sample is only warming.
        s.procs_window_ns = None;
        let o = evaluate(&machine("sys jobs.stuck=0"), &s, &[7]);
        assert!(o.refused.is_none() && !o.holds, "{o:?}");
    }

    #[test]
    fn the_volume_wait_routes_whole() {
        for v in [
            "sys free>=10GB",
            "sys free>room vol=/System/Volumes/Data",
            "sys disk=ok,low and free>1GB timeout=5000",
        ] {
            assert_eq!(AwaitSys::parse(v), Ok(Parsed::Volume), "{v}");
        }
    }

    #[test]
    fn an_explicit_term_replaces_the_presets_and_timeouts_clamp() {
        let w = machine("sys quiet cpu<30% timeout=9999999");
        assert_eq!(w.timeout_ms, TIMEOUT_MAX_MS);
        let cpu: Vec<&Term> = w.terms.iter().filter(|t| t.metric == Metric::Cpu).collect();
        assert_eq!(cpu.len(), 1);
        assert!(!cpu[0].preset);
        assert_eq!(w.terms.len(), 5);
        assert_eq!(machine("sys tab<1c for=1.5m").for_ms, 90_000);
        assert_eq!(machine("sys tab<1c").timeout_ms, TIMEOUT_DEFAULT_MS);
    }

    #[test]
    fn evaluation_reads_every_metric_and_prints_its_value() {
        let s = snap(0.1);
        let o = evaluate(
            &machine("sys cpu<25% and wait<0.25 and gpu<10% and mem=normal and tab<0.5c"),
            &s,
            &[7],
        );
        assert!(o.holds, "{o:?}");
        assert_eq!(
            o.values,
            ["cpu=10%", "wait=0.05", "gpu=2%", "mem=normal", "tab=0.12c"]
        );
        let o = evaluate(
            &machine("sys cpu.super>5% and cpu.l1>5% and cpu<0.5c"),
            &s,
            &[7],
        );
        assert_eq!(o.values, ["cpu.super=10%", "cpu.l1=10%", "cpu=1.00c"]);
        assert!(!o.holds, "1.0c is not below 0.5c");
        let o = evaluate(&machine("sys tab.mem<1GB and aterm.mem>500MB"), &s, &[7]);
        assert!(o.holds);
        assert_eq!(o.values, ["tab.mem=310MB", "aterm.mem=839MB"]);
        let o = evaluate(&machine("sys quiet"), &s, &[7]);
        assert!(o.holds, "{o:?}");
        assert_eq!(
            o.values,
            [
                "cpu=10%",
                "wait=0.05",
                "gpu=2%",
                "throttle=none",
                "swap=0B/s"
            ]
        );
        assert!(!evaluate(&machine("sys quiet"), &snap(0.5), &[7]).holds);
    }

    #[test]
    fn an_unmeasurable_preset_term_drops_out_and_an_explicit_one_refuses() {
        let mut s = snap(0.1);
        s.gpu = None;
        s.thermal = None;
        let o = evaluate(&machine("sys quiet"), &s, &[7]);
        assert!(o.holds);
        assert_eq!(o.skipped, ["gpu:unsupported", "throttle:unsupported"]);
        assert_eq!(
            met_reply(&o, 20_007, 1_792_185_697_412, "2b11.310"),
            "OK sys cpu=10% wait=0.05 swap=0B/s skipped=gpu:unsupported,throttle:unsupported \
             held_ms=20007 as_of=21:21:37 next=2b11.310\n"
        );
        let o = evaluate(&machine("sys gpu<10%"), &s, &[7]);
        assert_eq!(
            o.refused.as_deref(),
            Some("ERR unavailable gpu why=unsupported\n")
        );
        assert!(!o.holds);
    }

    #[test]
    fn a_warming_metric_parks_rather_than_refusing() {
        let mut s = snap(0.1);
        s.cpu = None;
        s.mem.as_mut().unwrap().rates = None;
        let o = evaluate(&machine("sys cpu<25% and swap<1MB/s"), &s, &[7]);
        assert!(!o.holds);
        assert!(o.refused.is_none());
        assert_eq!(o.values, ["cpu=warming", "swap=warming"]);
    }

    #[test]
    fn ordered_levels_compare_by_rank() {
        let mut s = snap(0.1);
        s.mem.as_mut().unwrap().sample.pressure = Pressure::Warn;
        s.thermal = Some(Thermal {
            level: Throttle::Light,
            raw: 1,
            low_power: None,
            exact: true,
        });
        assert!(evaluate(&machine("sys mem<critical"), &s, &[]).holds);
        assert!(!evaluate(&machine("sys mem=normal"), &s, &[]).holds);
        assert!(evaluate(&machine("sys mem=normal,warn"), &s, &[]).holds);
        assert!(evaluate(&machine("sys mem!=critical"), &s, &[]).holds);
        assert!(evaluate(&machine("sys throttle<heavy"), &s, &[]).holds);
        assert!(!evaluate(&machine("sys throttle=none"), &s, &[]).holds);
        assert!(evaluate(&machine("sys thrash=no"), &s, &[]).holds);
    }

    #[test]
    fn a_tab_the_snapshot_does_not_hold_yet_is_warming() {
        let o = evaluate(&machine("sys tab<1c"), &snap(0.1), &[99]);
        assert!(o.refused.is_none());
        assert!(!o.holds);
        assert_eq!(o.values, ["tab=warming"]);
        let mut s = snap(0.1);
        s.tabs[0].scope.footprint = None;
        let o = evaluate(&machine("sys tab.mem<1GB"), &s, &[7]);
        assert_eq!(
            o.refused.as_deref(),
            Some("ERR unavailable tab.mem why=refused\n")
        );
    }

    #[test]
    fn for_holds_over_consecutive_snapshots_by_their_stamps() {
        let mut l = Latch::default();
        let s = 1_000_000_000u64;
        assert_eq!(l.step(true, s, 2_000), None);
        assert_eq!(l.step(true, s, 2_000), None, "the same snapshot again");
        assert_eq!(l.step(true, 2 * s, 2_000), None);
        assert_eq!(l.step(true, 3 * s, 2_000), Some(2_000));
        let mut l = Latch::default();
        assert_eq!(l.step(true, s, 2_000), None);
        assert_eq!(
            l.step(false, 2 * s, 2_000),
            None,
            "a break restarts the hold"
        );
        assert_eq!(l.step(true, 3 * s, 2_000), None);
        assert_eq!(l.step(true, 5 * s, 2_000), Some(2_000));
        assert_eq!(Latch::default().step(true, s, 0), Some(0));
    }

    #[test]
    fn sizes_print_compactly() {
        assert_eq!(size_word(0.0), "0B");
        assert_eq!(size_word(1_234.0), "1.2KB");
        assert_eq!(size_word(310e6), "310MB");
        assert_eq!(size_word(7.1e6), "7.1MB");
    }
}
