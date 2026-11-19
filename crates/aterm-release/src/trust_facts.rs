// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Trust verification lane fact recording, memory admission, ratchet judgment, and CLI verbs.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

#[path = "trust_facts/build_env.rs"]
mod build_env;
#[path = "trust_facts/build_files.rs"]
pub(super) mod build_files;

use aterm_digest::Sha256;

use crate::ledger::{Error, GitRunner, Result, git_original_ok, original_blob_at, rev_parse};
use crate::release::shell_word;

// Version 17 checked coverage before filing proof floors but did not retain proof grades.
// Earlier records lack the coverage evidence necessary for replay.
const FACTS_MAGIC_V17: &str = "aterm-trust facts 17";
pub const FACTS_MAGIC: &str = "aterm-trust facts 18";

pub const FACTS_END: &str = "end";

pub const FACTS_PREFIX: &str = "trust-facts-";

pub const PLAN_MAGIC: &str = "aterm-trust plan 2";

pub const RATCHET: &str = "crates/aterm-release/trust/floors.tsv";

pub const COSTS: &str = "crates/aterm-release/trust/costs.tsv";

/// Beside the floors: what the run that set each floor stopped ([`floor_run`],
/// [`backstops_rewritten`]).
pub const BACKSTOPS: &str = "crates/aterm-release/trust/backstops.tsv";

const FILES_OUTSIDE_TREE_KEY: [&str; 4] = [RATCHET, MEMORY, COSTS, BACKSTOPS];

const BACKSTOPS_HEADER: &str = "# What a backstop (a clock's, or the RSS reading's) stopped in the run that set each floor in floors.tsv. A crate's rows start with <crate>\t-\tcancelled\t<n>: that run's stops were kept, and the lane cancelled n of its proof checks. Each obligation a backstop stopped in it follows: <crate>\t<key>\t<stop>\t<obligation>. `ship trust run --update-ratchet` writes a crate's rows with its floor. The judge credits a starved run's shortfall only with the stops its floor's run did not have and the cancelled proof checks beyond that run's count, so what every run has is the code's. A crate with no rows was floored before they were kept: what its floor's run stopped is unknown, so every stop and cancelled check is credited.";

/// The head of a crate's [`BACKSTOPS`] rows, before the proof checks its floor's run cancelled.
const RUN_ROW: &str = "-\tcancelled\t";

// Earlier receipts did not establish that every declared package script had a matching run.
const KEY_SCHEMA: &str = "aterm-trust facts key 8";

pub const LANE_WHOLE_TREE: &str = "whole-tree";

fn short(sha: &str) -> &str {
    sha.get(..9).unwrap_or(sha)
}

/// Replaces tabs, carriage returns, and newlines in `s` with spaces for single-line records.
pub(crate) fn one_line(s: &str) -> String {
    if s.bytes().any(|b| matches!(b, b'\t' | b'\r' | b'\n')) {
        s.replace(['\t', '\r', '\n'], " ")
    } else {
        s.to_string()
    }
}

/// Borrows `s` when it is already a single tab-free line, or replaces tabs/CR/LF with spaces.
pub(crate) fn one_line_cow(s: &str) -> std::borrow::Cow<'_, str> {
    if s.bytes().any(|b| matches!(b, b'\t' | b'\r' | b'\n')) {
        std::borrow::Cow::Owned(s.replace(['\t', '\r', '\n'], " "))
    } else {
        std::borrow::Cow::Borrowed(s)
    }
}

/// Returns `s` unchanged when it is already a single tab-free line, or replaces tabs/CR/LF with spaces.
pub(crate) fn one_line_owned(s: String) -> String {
    if s.bytes().any(|b| matches!(b, b'\t' | b'\r' | b'\n')) {
        s.replace(['\t', '\r', '\n'], " ")
    } else {
        s
    }
}

/// The `--reason` of `trust run` or `trust judge` (`verb`): the words a lowered floor's row keeps
/// in its fifth column, so one with none, which would leave that column blank, is refused.
pub(crate) fn reason_arg(verb: &str, why: &str) -> std::result::Result<String, String> {
    if why.trim().is_empty() {
        return Err(format!(
            "trust {verb}: --reason says why a floor goes down, in its row's fifth column, so it needs words, got {why:?}"
        ));
    }
    Ok(why.to_string())
}

/// Encodes `bytes` as lowercase hexadecimal.
pub(crate) fn hex(bytes: &[u8]) -> String {
    aterm_codec::hex::encode_lower(bytes)
}

pub const CHECK_CANCELLED: &str = "proof validation cancelled by the caller";

/// Why a starved run stays undecided while its threads wait for a core, behind the host's work
/// or the prover's own other threads, and what the Trust toolchain would need to offer for the
/// lane to decide it then.
const WALL_CLOCKS: &str = "the prover's clocks count wall time, and only `[trust] backstop` in the workspace root manifest sets them, for every run in the checkout, so the lane can neither count CPU time nor scale them by the share its threads get; while its threads wait for a core, behind the host's work or the prover's own other threads, only a prover whose clocks count CPU time, or that takes a backstop for one run, decides it (asked of the Trust toolchain)";

/// How far apart two runs of one tree under one prover can prove, in hundredths of the count:
/// the prover is not reproducible. Measured at loads that starved neither run: aterm-link 841
/// and 853 (12, 1.4%), aterm-messages 1066 and 1070 (4), aterm-gpu 811 and 812 (1); none of
/// the obligations that moved was one a backstop stopped (`docs/DESIGN-adaptive-build-2026-10-24.md`,
/// TRUST-JUDGE).
pub const SPREAD_PER_CENT: u64 = 2;

/// The most a run may fall short of a floor of `f` its own prover recorded by that prover's
/// run-to-run spread alone: [`SPREAD_PER_CENT`] of `f`, rounded down, so a floor under 50 has
/// none and a small crate's one lost proof stays a regression.
#[must_use]
pub const fn spread(f: u64) -> u64 {
    f.saturating_mul(SPREAD_PER_CENT) / 100
}

/// What was collected from this check's native obligation-grade record.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GradeEvidence {
    /// A v17 record did not say whether a grade record existed.
    Legacy,
    /// Collection found no native record for the check.
    #[default]
    Absent,
    /// The complete native record supplied these counts, including a measured zero.
    Measured(crate::trust_credit::Tally),
}

impl GradeEvidence {
    fn write_field(self, out: &mut String) {
        match self {
            Self::Legacy => out.push_str("uncollected"),
            Self::Absent => out.push_str("absent"),
            Self::Measured(t) => {
                let _ = write!(out, "measured {} {} {}", t.proved, t.graded, t.calculus);
            }
        }
    }

    fn parse(field: &str) -> Option<Self> {
        match field {
            "uncollected" => Some(Self::Legacy),
            "absent" => Some(Self::Absent),
            _ => {
                let mut counts = field.strip_prefix("measured ")?.split(' ');
                let mut count = || {
                    let value = counts.next()?;
                    if value.len() > 1 && value.starts_with('0') {
                        return None;
                    }
                    parse_uint::<u64>(value)
                };
                let proved = count()?;
                let graded = count()?;
                let calculus = count()?;
                (counts.next().is_none() && calculus <= graded && graded <= proved).then_some(
                    Self::Measured(crate::trust_credit::Tally {
                        proved,
                        graded,
                        calculus,
                    }),
                )
            }
        }
    }
}

/// Raw verification metrics collected for a single crate's `targo trust check`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Measured {
    pub rc: String,
    pub transport: bool,
    pub summary: String,
    pub refused: u64,
    pub refused_why: String,
    pub lapsed: u64,
    pub cancelled: u64,
    pub budget_rows: u64,
    pub skipped: Option<u64>,
    pub ceiling: String,
    pub peak: String,
    pub launch: String,
    pub held: String,
    pub held_what: String,
    pub wall_budget: String,
    pub ended: String,
    pub path_refused: Vec<PathRefusal>,
    /// Obligations the prover's RSS backstop stopped (its `Stop causes:` line): what the
    /// machine's memory cost, as `lapsed` is what its clocks did.
    pub rss: u64,
    /// How the check ran ([`Pace`]); `None` when the lane recorded nothing.
    pub pace: Option<Pace>,
    /// What a backstop stopped, from the check's obligation record; `None` when it left none.
    pub backstops: Option<Vec<Backstop>>,
    /// What served trustd's socket after the trustd this run's keeper held went away while the
    /// check ran (the lane's `.trustd-lost` mark); empty when it stayed.
    pub trustd_lost: String,
    /// A driver refusal, the final no-measurement Result, or its failure and stop counts.
    pub could_not_run: String,
    pub grade: GradeEvidence,
}

/// How a check ran: its CPU and wall time, the host's mean one-minute load while it ran, the
/// host's cores, and how long its threads ran against how long they were runnable. The prover's
/// clocks are wall clocks, so a check whose threads waited for a core lapses more
/// ([`Pace::starved`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pace {
    pub cpu_ms: u64,
    pub wall_ms: u64,
    /// The mean one-minute load average, in hundredths; `None` when it could not be read.
    pub load: Option<u64>,
    pub cores: u64,
    /// Its processes' threads, as the lane's reads of them saw them; `None` when unread.
    pub threads: Option<Threads>,
}

/// How long a check's threads ran and how long they were runnable (running or waiting for a
/// core), summed over its processes: the work each of the prover's clocks timed got
/// `ran_ms / runnable_ms` of the time that clock counted, whatever kept its threads waiting,
/// the host's other work or the check's own threads beyond the cores. On a host with two kinds
/// of core, `cores` says which ran them ([`Threads::worked_ms`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Threads {
    pub ran_ms: u64,
    pub runnable_ms: u64,
    /// Where they ran, summed over the same processes; `None` where the host has one kind of
    /// core, or any of them was read without it.
    pub cores: Option<Cores>,
}

/// Of a check's threads' CPU time, what ran on the host's performance cores, and the
/// instructions they retired in all and on those cores (aterm-sysprobe's `CoreSplit`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cores {
    pub p_ms: u64,
    pub instructions: u64,
    pub p_instructions: u64,
}

impl Threads {
    /// THE MODEL (FONT-ECORE-SHARE): the time they ran, an efficiency core's counted at the speed
    /// the check's own instructions ran there against its performance cores'
    /// (`(e_instructions / e_ms) / (p_instructions / p_ms)`), so a millisecond on an efficiency
    /// core that retired half a performance core's instructions counts half. That is the time
    /// their instructions would have taken on its performance cores at the rate they ran there:
    /// `p_ms * instructions / p_instructions`, at most `ran_ms`. Live verifier threads on this
    /// M4 Pro ran 0.44 to 0.57 of the performance cores' rate on its efficiency cores
    /// (2026-10-27). The ratio is the run's own, so a performance core slowed itself (heat, the
    /// GPU's share of the memory) counts as full speed. With no split, or no time or
    /// instructions on a performance core to compare against, it is `ran_ms`.
    #[must_use]
    pub fn worked_ms(&self) -> u64 {
        match self.cores {
            Some(c) if c.p_ms > 0 && c.p_instructions > 0 => {
                let at_p =
                    u128::from(c.p_ms) * u128::from(c.instructions) / u128::from(c.p_instructions);
                u64::try_from(at_p).unwrap_or(u64::MAX).min(self.ran_ms)
            }
            _ => self.ran_ms,
        }
    }
}

impl Pace {
    /// `<cpu ms> <wall ms> <load in hundredths, or -> <cores>`, then `<ran ms> <runnable ms>`
    /// when its threads were read, then `<p-core ms> <instructions> <p-core instructions>` when
    /// where they ran was too ([`Threads::cores`]): the lane's `.pace` mark and the facts' field.
    #[must_use]
    pub fn text(&self) -> String {
        let mut t = String::new();
        let _ = write!(t, "{} {} ", self.cpu_ms, self.wall_ms);
        match self.load {
            Some(l) => {
                let _ = write!(t, "{l} {}", self.cores);
            }
            None => {
                let _ = write!(t, "- {}", self.cores);
            }
        }
        if let Some(th) = self.threads {
            let _ = write!(t, " {} {}", th.ran_ms, th.runnable_ms);
            if let Some(c) = th.cores {
                let _ = write!(t, " {} {} {}", c.p_ms, c.instructions, c.p_instructions);
            }
        }
        t
    }

    /// Reads [`Pace::text`].
    #[must_use]
    pub fn parse(s: &str) -> Option<Pace> {
        let mut parts = s.split(' ');
        let (cpu, wall, load, cores) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
        let threads = |ran: &str, runnable: &str, cores: Option<Cores>| {
            Some(Threads {
                ran_ms: parse_uint(ran)?,
                runnable_ms: parse_uint(runnable)?,
                cores,
            })
        };
        let rest: [Option<&str>; 6] = std::array::from_fn(|_| parts.next());
        let threads = match rest {
            [None, None, None, None, None, None] => None,
            [Some(ran), Some(runnable), None, None, None, None] => {
                Some(threads(ran, runnable, None)?)
            }
            [
                Some(ran),
                Some(runnable),
                Some(p),
                Some(all),
                Some(p_all),
                None,
            ] => {
                let split = Cores {
                    p_ms: parse_uint(p)?,
                    instructions: parse_uint(all)?,
                    p_instructions: parse_uint(p_all)?,
                };
                Some(threads(ran, runnable, Some(split))?)
            }
            _ => return None,
        };
        Some(Pace {
            cpu_ms: parse_uint(cpu)?,
            wall_ms: parse_uint(wall)?,
            load: match load {
                "-" => None,
                l => Some(parse_uint(l)?),
            },
            cores: parse_uint(cores)?,
            threads,
        })
    }

    /// Its threads' times, when a read found them runnable for any time.
    fn share(&self) -> Option<&Threads> {
        self.threads.as_ref().filter(|t| t.runnable_ms > 0)
    }

    /// Starved: its threads ran under 0.8 of the time they were runnable, an efficiency core's
    /// time counted at its speed ([`Threads::worked_ms`]); where they were not read, its CPU
    /// was under 0.8 of its wall time or the load was above the cores. The load counts the
    /// check's own threads and work that yields to them, so it stands in only for the share it
    /// cannot see.
    #[must_use]
    pub fn starved(&self) -> bool {
        if let Some(t) = self.share() {
            return t.worked_ms().saturating_mul(10) < t.runnable_ms.saturating_mul(8);
        }
        (self.wall_ms > 0 && self.cpu_ms.saturating_mul(10) < self.wall_ms.saturating_mul(8))
            || self
                .load
                .is_some_and(|l| l > self.cores.saturating_mul(100))
    }

    /// What a run on the same host needs to be decided ([`Pace::starved`]).
    #[must_use]
    pub fn unstarved(&self) -> String {
        if let Some(t) = self.share() {
            let speed = if t.cores.is_some() {
                ", counting an efficiency core's time at its speed"
            } else {
                ""
            };
            format!("threads that run at least 0.80 of the time they are runnable{speed}")
        } else {
            format!(
                "CPU/wall at least 0.80 and a mean load of at most {} (its cores)",
                self.cores
            )
        }
    }

    /// `CPU/wall 0.40, load 30.00 on 14 cores`, then `, threads ran 0.57 of their runnable
    /// time, 11.86 of them runnable on average` when they were read, with `(0.49 at a
    /// performance core's speed: 0.26 of it on efficiency cores, at 0.50 of that speed)` after
    /// the share where some of it ran on efficiency cores ([`Threads::worked_ms`]); an average
    /// above the cores says the check's own threads kept each other waiting.
    #[must_use]
    pub fn said(&self) -> String {
        let hundredths = |n: u64| format!("{}.{:02}", n / 100, n % 100);
        let ratio = |a: u64, b: u64| {
            a.saturating_mul(100)
                .saturating_add(b / 2)
                .checked_div(b)
                .map_or_else(|| "?".to_string(), hundredths)
        };
        let load = self.load.map_or_else(|| "unread".to_string(), hundredths);
        let mut said = format!(
            "CPU/wall {}, load {load} on {} cores",
            ratio(self.cpu_ms, self.wall_ms),
            self.cores
        );
        if let Some(t) = self.share() {
            let mut speed = String::new();
            if let Some(c) = t.cores.filter(|c| c.p_ms < t.ran_ms) {
                let e_ms = t.ran_ms - c.p_ms;
                let rate = if c.p_ms > 0 && c.p_instructions > 0 {
                    // (e_instructions / e_ms) / (p_instructions / p_ms), in hundredths.
                    let e = u128::from(c.instructions.saturating_sub(c.p_instructions))
                        * u128::from(c.p_ms);
                    let p = u128::from(c.p_instructions) * u128::from(e_ms);
                    let of = (e * 100 + p / 2) / p;
                    let of = hundredths(u64::try_from(of).unwrap_or(u64::MAX));
                    format!("at {of} of that speed")
                } else {
                    "counted at full speed, with no performance core's rate to compare".to_string()
                };
                speed = format!(
                    " ({} at a performance core's speed: {} of it on efficiency cores, {rate})",
                    ratio(t.worked_ms(), t.runnable_ms),
                    ratio(e_ms, t.ran_ms)
                );
            }
            let _ = write!(
                said,
                ", threads ran {} of their runnable time{speed}, {} of them runnable on average",
                ratio(t.ran_ms, t.runnable_ms),
                ratio(t.runnable_ms, self.wall_ms)
            );
        }
        said
    }
}

/// One obligation a backstop stopped ([`crate::trust_oblig::Obligation::backstopped`]).
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Backstop {
    /// [`crate::trust_oblig::Obligation::key`].
    pub key: String,
    /// `wall-backstop` or `rss-backstop`.
    pub stop: String,
    /// [`crate::trust_oblig::Obligation::said`].
    pub said: String,
}

impl Backstop {
    fn fields(&self) -> String {
        format!(
            "{}\t{}\t{}",
            one_line_cow(&self.key),
            one_line_cow(&self.stop),
            one_line_cow(&self.said)
        )
    }

    fn from_fields(v: &str) -> Option<Backstop> {
        let mut parts = v.splitn(3, '\t');
        let (key, stop, said) = (parts.next()?, parts.next()?, parts.next()?);
        (!key.is_empty() && crate::trust_oblig::is_backstop(stop)).then(|| Backstop {
            key: key.into(),
            stop: stop.into(),
            said: said.into(),
        })
    }

    /// The backstopped rows of a check's obligation record.
    fn of_record(rec: &crate::trust_oblig::Record) -> Vec<Backstop> {
        rec.rows
            .iter()
            .filter(|o| o.backstopped())
            .map(|o| Backstop {
                key: o.key(),
                stop: o.stop.clone(),
                said: o.said(),
            })
            .collect()
    }
}

/// Path rejected by `targo` for permissions or ownership.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PathRefusal {
    pub path: String,
    pub why: Refused,
}

/// Why `targo` refused a path.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Refused {
    Writable {
        owner: Option<u32>,
        mode: Option<u32>,
        link: Option<String>,
    },
    Foreign {
        owner: u32,
        me: u32,
    },
    Acl {
        me: u32,
    },
    Ancestor {
        authority: String,
    },
    Demand {
        demand: String,
    },
}

const REFUSED_MOVE: &str = "keep the lane's target root (--target-root) and the toolchain under directories owned by root or you and writable by their owner alone";

impl PathRefusal {
    fn said(&self, me: u32) -> String {
        match &self.why {
            Refused::Writable { owner, mode, link } => {
                let mode =
                    mode.map_or_else(|| "mode unnamed".to_string(), |m| format!("mode {m:o}"));
                let owner = owner
                    .filter(|o| *o != me)
                    .map_or_else(String::new, |o| format!(", owned by uid={o}"));
                match link {
                    Some(l) => format!(
                        "{mode}: group- or world-writable without the sticky bit{owner}, holding the launch symlink {l}"
                    ),
                    None => format!("{mode}: group- or world-writable{owner}"),
                }
            }
            Refused::Foreign { owner, me } => {
                format!("owned by uid={owner}, neither root nor you (uid={me})")
            }
            Refused::Acl { me } => {
                format!("another owns it, and an ACL lets you (uid={me}) write it")
            }
            Refused::Ancestor { authority } => {
                format!("foreign-owned or cross-user writable, by the {authority} check")
            }
            Refused::Demand { demand } => format!("trustd demands: {demand}"),
        }
    }

    fn remedy(&self, me: u32) -> String {
        let p = &self.path;
        let chmod = format!("chmod go-w {}", shell_word(p));
        match &self.why {
            Refused::Demand { demand } if !demand.starts_with("trustd executable") => {
                let chmod = if demand.contains("0700") {
                    format!("chmod 700 {}", shell_word(p))
                } else {
                    chmod
                };
                format!(
                    "if it is yours, run: {chmod}; if not, root must remove it, and the next check makes it afresh"
                )
            }
            Refused::Writable { mode: Some(m), .. } if m & 0o1000 != 0 => format!(
                "{p} is a shared sticky directory, which targo refuses all the same, so it must hold nothing targo checks: {REFUSED_MOVE}"
            ),
            Refused::Writable { owner: Some(o), .. } if *o == me => format!("run: {chmod}"),
            Refused::Writable { owner: Some(o), .. } => {
                format!("{p} is uid={o}'s, not yours to change, so it must move: {REFUSED_MOVE}")
            }
            Refused::Writable { owner: None, .. }
            | Refused::Ancestor { .. }
            | Refused::Demand { .. } => {
                format!("if it is yours, run: {chmod}; if not, it must move: {REFUSED_MOVE}")
            }
            Refused::Foreign { .. } => {
                format!("{p} is foreign-owned, so it must move: {REFUSED_MOVE}")
            }
            Refused::Acl { .. } => {
                format!("{p} is writable through an ACL, so it must move: {REFUSED_MOVE}")
            }
        }
    }
}

const REFUSED_COMPONENT: &str = "path component `";

const REFUSED_LINK: &str = " launch path symlink `";

const REFUSED_ANCESTOR: &str = " ancestor is foreign-owned or cross-user writable: ";

const TRUSTD_DEMANDS: [&str; 3] = [
    "socket parent must be ",
    "trustd runtime root must be ",
    "trustd executable must be ",
];

fn trustd_path(rest: &str) -> &str {
    let end = ["; ", "\\n", "\""]
        .iter()
        .filter_map(|t| rest.find(t))
        .min()
        .unwrap_or(rest.len());
    rest[..end].trim_end()
}

fn backticked(s: &str) -> Option<(&str, &str)> {
    let e = s.find('`')?;
    (e > 0).then(|| (&s[..e], &s[e + 1..]))
}

fn parse_uint<T: std::str::FromStr>(s: &str) -> Option<T> {
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}

fn octal_mode(s: &str) -> Option<u32> {
    let r = s.strip_prefix("0o")?;
    let n = r.bytes().take_while(|b| (b'0'..=b'7').contains(b)).count();
    (n > 0)
        .then(|| u32::from_str_radix(&r[..n], 8).ok())
        .flatten()
}

fn foreign_owner(rest: &str) -> Option<Refused> {
    const INVOKERS: [&str; 2] = [
        ", which is neither root nor the invoking identity (uid=",
        ", which is not permitted to own a redirection into the verified toolchain (invoking uid=",
    ];
    let r = rest.strip_prefix(" is owned by uid=")?;
    let owner = digits_at(r, 0);
    let after = &r[owner.len()..];
    let me = INVOKERS.iter().find_map(|w| after.strip_prefix(w))?;
    let (owner, me) = (owner.parse().ok()?, digits_at(me, 0).parse().ok()?);
    (owner != me).then_some(Refused::Foreign { owner, me })
}

fn without_ansi(line: &str) -> std::borrow::Cow<'_, str> {
    if !line.contains('\u{1b}') {
        return std::borrow::Cow::Borrowed(line);
    }
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        if chars.clone().next() == Some('[') {
            chars.next();
            for d in chars.by_ref() {
                if ('@'..='~').contains(&d) {
                    break;
                }
            }
        }
    }
    std::borrow::Cow::Owned(out)
}

fn path_refusals(line: &str) -> Vec<PathRefusal> {
    if !line.contains("writable") && !line.contains("owned by uid=") && !line.contains("must be ") {
        return Vec::new();
    }
    let line = without_ansi(line);
    let line: &str = &line;
    let mut out = Vec::new();
    for i in find_all(line, REFUSED_COMPONENT) {
        let Some((path, rest)) = backticked(&line[i + REFUSED_COMPONENT.len()..]) else {
            continue;
        };
        let why = if let Some(r) = rest.strip_prefix(" is group- or world-writable (uid=") {
            let owner = digits_at(r, 0);
            Some(Refused::Writable {
                owner: owner.parse().ok(),
                mode: r[owner.len()..]
                    .strip_prefix(", mode=")
                    .and_then(octal_mode),
                link: None,
            })
        } else if let Some(r) =
            rest.strip_prefix(" remains writable by Targo's effective identity (uid=")
        {
            digits_at(r, 0).parse().ok().map(|me| Refused::Acl { me })
        } else {
            foreign_owner(rest)
        };
        if let Some(why) = why {
            out.push(PathRefusal {
                path: path.to_string(),
                why,
            });
        }
    }
    for i in find_all(line, REFUSED_LINK) {
        let Some((link, rest)) = backticked(&line[i + REFUSED_LINK.len()..]) else {
            continue;
        };
        let before = &line[..i];
        let holder = before
            .rfind("` holding ")
            .filter(|h| !before[h + "` holding ".len()..].contains('`'))
            .and_then(|h| {
                let d = &before[..h];
                d.rfind("the directory `")
                    .map(|s| &d[s + "the directory `".len()..])
            })
            .filter(|d| !d.is_empty());
        let writable =
            rest.strip_prefix(" is group- or world-writable without the sticky bit (mode=");
        let why = match (holder, writable) {
            (Some(_), Some(r)) => Some(Refused::Writable {
                owner: None,
                mode: octal_mode(r),
                link: Some(link.to_string()),
            }),
            _ => foreign_owner(rest),
        };
        if let Some(why) = why {
            out.push(PathRefusal {
                path: holder.unwrap_or(link).to_string(),
                why,
            });
        }
    }
    for i in find_all(line, REFUSED_ANCESTOR) {
        let path = trustd_path(&line[i + REFUSED_ANCESTOR.len()..]);
        let before = &line[..i];
        let authority = before
            .rfind("trustd")
            .map(|t| &before[t..])
            .filter(|a| a.split(' ').count() <= 2)
            .unwrap_or("trustd");
        if !path.is_empty() {
            out.push(PathRefusal {
                path: path.to_string(),
                why: Refused::Ancestor {
                    authority: authority.to_string(),
                },
            });
        }
    }
    for subject in TRUSTD_DEMANDS {
        for i in find_all(line, subject) {
            let said = &line[i..];
            let Some(colon) = said.find(": ") else {
                continue;
            };
            let path = trustd_path(&said[colon + 2..]);
            if !path.is_empty() {
                out.push(PathRefusal {
                    path: path.to_string(),
                    why: Refused::Demand {
                        demand: said[..colon].to_string(),
                    },
                });
            }
        }
    }
    out
}

pub const COVERAGE_COMPLETE: &str = "eligible function bodies verified (complete)";

pub const COVERAGE_ACCOUNTED: &str = "eligible function bodies accounted for (";

pub const TRUSTD_REFUSED: &str =
    "could not establish the selected same-sysroot trustd memory authority";

pub const TRUSTD_DIRTY: &str = "crash epoch is not CLEAN";

/// The detailed diagnostic and the result line targo-trust uses for a non-measurement.
const TARGO_COULD_NOT_RUN: &str = "COULD NOT RUN: ";
const TARGO_NO_RESULT: &str = "Result: COULD NOT RUN (";

/// The longest line of a check's log the judge reads, its `\n` included: [`Measured::of_log`]
/// skips a longer one whole, through its real newline (`read_log_line`), so nothing in it is a
/// fact.
pub const LINE_MAX: u64 = 1024 * 1024;

/// How targo-trust says the coordinator refused a proof check memory.
const ADMISSION: &str = "memory admission failed";

/// How targo-trust says a proof check ran past its per-function budget.
const BUDGET: &str = "exceeded the per-function verification budget";

/// How targo-trust counts the proof checks a clock stopped.
const TIMED_OUT: &str = "timed_out=";

const CARGO_PROCESS_FAILED: &str = "process didn't exit successfully: `";
const FAILED_ROWS: &str = "Failed rows: ";

/// Every string but a path refusal's that [`Measured::of_log`] judges a line by: it reads
/// nothing in a line that holds none of them and names no path refusal (it skips such a line),
/// so the lane may leave that line out of a stored log ([`judged_line`]). Each predicate of
/// `of_log` needs one of these (its census figures by `memory census:`), but its path refusals,
/// which [`judged`] reads as `of_log` does (`path_refusals`), so a line that only passes their
/// quick gate (a rustc note that something "must be used") is no judged line; a predicate added
/// there adds its gate here.
pub const JUDGED: [&str; 20] = [
    "verification did not run",
    "transport:missing-json",
    TARGO_COULD_NOT_RUN,
    TARGO_NO_RESULT,
    TRUSTD_REFUSED,
    TRUSTD_DIRTY,
    "another trustd owns",
    "did not establish exact-identity readiness",
    "Summary:",
    FAILED_ROWS,
    "Coverage:",
    "Stop causes:",
    "[transport:",
    CARGO_PROCESS_FAILED,
    ADMISSION,
    BUDGET,
    CHECK_CANCELLED,
    TIMED_OUT,
    "memory census:",
    CEILING_FLAG,
];

/// Whether [`Measured::of_log`] reads anything in `line`, a line without its line ending: it
/// names one of [`JUDGED`], or a path refusal `of_log` reads (`path_refusals`).
#[must_use]
pub fn judged(line: &str) -> bool {
    JUDGED.iter().any(|needle| line.contains(needle)) || !path_refusals(line).is_empty()
}

/// Whether [`Measured::of_log`] reads anything in `line`, one line of a log as it reads one
/// (at most [`LINE_MAX`] bytes, its `\n` included): read as `of_log` reads it.
#[must_use]
pub fn judged_line(line: &[u8]) -> bool {
    (line.len() as u64) <= LINE_MAX
        && judged(String::from_utf8_lossy(line).trim_end_matches(['\n', '\r']))
}

/// What a stored log's name adds to the name the lane gives a check's log, by which its
/// sidecars are named (`<name@version>.log`): the lane keeps a check's log zstd-compressed
/// ([`stored_log`]).
pub const STORED_SUFFIX: &str = ".zst";

/// The stored, compressed log of the check whose log is named `log` (`<name@version>.log.zst`).
#[must_use]
pub fn stored_log(log: &Path) -> PathBuf {
    let mut name = log.as_os_str().to_os_string();
    name.push(STORED_SUFFIX);
    PathBuf::from(name)
}

/// Opens the stored log ([`stored_log`]) of the check whose log is named `log`, decompressed.
/// A stored log whose frame has not ended (its check still running, its lane ended before it
/// finished it, or its output's read failed) reads as far as it is, then fails
/// (`UnexpectedEof`), so no reader takes it for a whole log.
///
/// # Errors
///
/// Opening the stored log, naming it: `NotFound` where there is none; a symlink or anything
/// but a regular file is refused.
pub fn open_log(log: &Path) -> std::io::Result<impl BufRead> {
    let stored = stored_log(log);
    let file = crate::durable::open_regular_file(&stored)
        .map_err(|e| std::io::Error::new(e.kind(), format!("{}: {e}", stored.display())))?;
    Ok(BufReader::new(aterm_zstd::Decoder::new(BufReader::new(
        file,
    ))))
}

fn find_all<'a>(hay: &'a str, needle: &'a str) -> impl Iterator<Item = usize> + 'a {
    debug_assert!(!needle.is_empty());
    hay.match_indices(needle).map(|(i, _)| i)
}

fn rfind_all<'a>(hay: &'a str, needle: &'a str) -> impl Iterator<Item = usize> + 'a {
    debug_assert!(!needle.is_empty());
    hay.rmatch_indices(needle).map(|(i, _)| i)
}

fn digits_at(s: &str, i: usize) -> &str {
    let rest = s.get(i..).unwrap_or("");
    let n = rest.bytes().take_while(u8::is_ascii_digit).count();
    &rest[..n]
}

const fn digits_start_before(s: &str, end: usize) -> usize {
    let bytes = s.as_bytes();
    let mut i = end;
    while i > 0 && bytes[i - 1].is_ascii_digit() {
        i -= 1;
    }
    i
}

struct Summary<'a> {
    proved: u64,
    failed: Option<u64>,
    total: u64,
    outcome: &'a str,
}

impl Summary<'_> {
    fn unmeasured(&self) -> bool {
        matches!(self.outcome, "not-reproducible" | "could-not-run")
    }
}

fn summary_line(line: &str) -> Option<Summary<'_>> {
    let (counts, rest) = line.trim().strip_prefix("Summary: ")?.split_once(" (")?;
    let (total, suffix) = rest.split_once(" total)")?;
    let total = parse_uint(total)?;
    let mut fields = counts.splitn(8, ", ");
    let proved: u64 = parse_uint(fields.next()?.strip_suffix(" proved")?)?;
    // Compact summaries state only proved, or proved and failed. Each full writer
    // added buckets to a disjoint partition whose sum is the reported total.
    let labels: &[&str] = match fields.clone().count() {
        0 => &[],
        1 => &["failed"],
        3 => &["failed", "runtime-checked", "inconclusive"],
        5 => &[
            "failed",
            "runtime-checked",
            "assumed",
            "mandated",
            "inconclusive",
        ],
        6 => &[
            "failed",
            "runtime-checked",
            "assumed",
            "mandated",
            "contract-panic",
            "inconclusive",
        ],
        _ => return None,
    };
    let mut sum = proved;
    let mut failed = None;
    for (field, &label) in fields.zip(labels) {
        let (n, name) = field.split_once(' ')?;
        if name != label {
            return None;
        }
        let count = parse_uint(n)?;
        sum = sum.checked_add(count)?;
        if label == "failed" {
            failed = Some(count);
        }
    }
    if sum > total || (labels.len() > 1 && sum != total) {
        return None;
    }
    let outcome = if suffix.is_empty() {
        ""
    } else {
        // Only the determinism outcome affects the floor. Later clauses carry
        // the writer's profile and vector identifiers.
        let outcome = suffix.strip_prefix(';')?.split(';').next()?.trim();
        if !matches!(
            outcome,
            "measured" | "reproducible" | "not-reproducible" | "could-not-run"
        ) {
            return None;
        }
        outcome
    };
    Some(Summary {
        proved,
        failed,
        total,
        outcome,
    })
}

/// The optional line Targo prints just after its summary when some failures have no counterexample.
fn failure_breakdown(line: &str, expected: u64) -> Option<String> {
    let (counts, _) = line.strip_prefix(FAILED_ROWS)?.split_once(':')?;
    let (failed, unproved) = counts
        .strip_suffix(" of them unproved with NO counterexample")?
        .split_once(" failed, ")?;
    let failed = parse_uint::<u64>(failed)?;
    let unproved = parse_uint::<u64>(unproved)?;
    (failed == expected && (1..=failed).contains(&unproved)).then(|| {
        format!("{FAILED_ROWS}{failed} failed, {unproved} unproved without a counterexample")
    })
}

/// Extracts counts from a complete Trust summary with consistent outcome buckets.
#[must_use]
pub fn parse_summary(line: &str) -> (Option<u64>, Option<u64>) {
    summary_line(line).map_or((None, None), |s| (Some(s.proved), Some(s.total)))
}

fn census_skipped(line: &str) -> Option<u64> {
    let census = line.find("memory census:")? + "memory census:".len();
    rfind_all(line, " bodies skipped at the memory ceiling").find_map(|j| {
        let start = digits_start_before(line, j);
        (start < j && start > census)
            .then(|| line[start..j].parse().ok())
            .flatten()
    })
}

fn census_figure(line: &str, key: &str) -> Option<String> {
    let census = line.find("memory census:")? + "memory census:".len();
    rfind_all(line, key).find_map(|k| {
        if k < census {
            return None;
        }
        let at = k + key.len();
        let rest = &line[at..];
        let n = rest
            .bytes()
            .take_while(|b| b.is_ascii_digit() || *b == b'.')
            .count();
        let unit = rest.get(n..)?.strip_prefix(' ')?;
        let mut u = 0;
        let ub = unit.as_bytes();
        if ub.first().is_some_and(|b| b"KMGT".contains(b)) {
            u += 1;
        }
        if ub.get(u) == Some(&b'i') {
            u += 1;
        }
        (n > 0 && ub.get(u) == Some(&b'B')).then(|| format!("{} {}", &rest[..n], &unit[..=u]))
    })
}

fn transport_rows(line: &str) -> Vec<(String, String)> {
    const OPEN: &str = "[transport:";
    find_all(line, OPEN)
        .filter_map(|i| {
            let rest = &line[i + OPEN.len()..];
            let n = rest
                .bytes()
                .take_while(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
                .count();
            if n == 0 || rest.as_bytes().get(n) != Some(&b']') {
                return None;
            }
            let msg = &rest[n + 1..];
            let msg = msg.find(OPEN).map_or(msg, |j| &msg[..j]).trim();
            let msg = msg.strip_suffix("(targo-trust)").map_or(msg, str::trim_end);
            Some((rest[..n].to_string(), msg.to_string()))
        })
        .collect()
}

fn compiler_signal(line: &str) -> Option<String> {
    let line = line.trim_start();
    let report = line
        .strip_prefix("error: ")
        .unwrap_or(line)
        .strip_prefix(CARGO_PROCESS_FAILED)?;
    let (command, status) = report.rsplit_once("` (")?;
    let signal = status.strip_suffix(')')?.strip_prefix("signal: ")?;
    let (number, detail) = signal.split_once(", ").unwrap_or((signal, ""));
    let n = parse_uint::<u32>(number).filter(|&n| n != 0)?;
    let name = detail.split(':').next().unwrap_or_default();
    // Cargo prints the program verbatim and then joins arguments with spaces. Only a
    // command with no separator identifies the program's boundaries unambiguously.
    let child = if command.contains(' ') {
        "child process"
    } else {
        command.rsplit('/').next().filter(|s| !s.is_empty())?
    };
    Some(format!(
        "cargo: {child} ended by signal {n}{}",
        signal_said(name)
    ))
}

const fn signal_name(n: u32) -> &'static str {
    match n {
        1 => "SIGHUP",
        2 => "SIGINT",
        3 => "SIGQUIT",
        6 => "SIGABRT",
        9 => "SIGKILL",
        13 => "SIGPIPE",
        14 => "SIGALRM",
        15 => "SIGTERM",
        _ => "",
    }
}

fn signal_said(name: &str) -> String {
    if name.is_empty() {
        String::new()
    } else {
        format!(" ({name})")
    }
}

fn span(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, secs % 3600 / 60, secs % 60);
    let mut out = Vec::new();
    if h > 0 {
        out.push(format!("{h} h"));
    }
    if m > 0 {
        out.push(format!("{m} min"));
    }
    if s > 0 || out.is_empty() {
        out.push(format!("{s} s"));
    }
    out.join(" ")
}

/// The count for one cause in Trust's space-separated stop tally.
fn stop_count(tally: &str, cause: &str) -> u64 {
    tally
        .split_whitespace()
        .find_map(|w| parse_uint(w.strip_prefix(cause)?.strip_prefix('=')?))
        .unwrap_or(0)
}

fn coverage_complete(line: &str) -> bool {
    let Some(rest) = line.trim().strip_prefix("Coverage: ") else {
        return false;
    };
    let Some((counts, words)) = rest.split_once(' ') else {
        return false;
    };
    let Some((processed, eligible)) = counts.split_once('/') else {
        return false;
    };
    if processed != eligible {
        return false;
    }
    let Some(processed) = parse_uint::<u64>(processed) else {
        return false;
    };
    if words == COVERAGE_COMPLETE {
        return true;
    }
    // Test-harness builds print verified and excluded as the two parts of
    // processed. A matching headline alone cannot account for every body.
    let Some((verified, excluded)) = words
        .strip_prefix(COVERAGE_ACCOUNTED)
        .and_then(|s| s.strip_suffix(" excluded: test-harness) (complete)"))
        .and_then(|s| s.split_once(" verified, "))
    else {
        return false;
    };
    matches!((parse_uint::<u64>(verified), parse_uint::<u64>(excluded)),
        (Some(v), Some(e)) if e > 0 && v.checked_add(e) == Some(processed))
}

/// Summarizes `trustd` coordinator admission refusals by category and count.
#[must_use]
pub fn admission_cause(refusals: &[String]) -> String {
    let mut n = [0u64; 4];
    for r in refusals {
        let k = if r.contains("could not connect") || r.contains("could not send") {
            0
        } else if r.contains("before the deadline") {
            1
        } else if r.contains("declined the reservation")
            || r.contains("ERR busy")
            || r.contains("budget")
        {
            2
        } else {
            3
        };
        n[k] += 1;
    }
    [
        "trustd unreachable",
        "trustd did not answer in time",
        "trustd declined or was busy",
        "another refusal",
    ]
    .iter()
    .zip(n)
    .filter(|(_, c)| *c > 0)
    .map(|(kind, c)| format!("{kind}: {c}"))
    .collect::<Vec<_>>()
    .join(", ")
}

impl Measured {
    fn compiler_summary(&self) -> std::result::Result<Summary<'_>, String> {
        if !matches!(self.rc.trim(), "0" | "1") || self.summary.is_empty() {
            return Err(format!("GATE-BROKE(rc={})", self.rc));
        }
        summary_line(&self.summary).ok_or_else(|| "SUMMARY-UNPARSED".into())
    }

    /// Counts eligible for a proof floor. Targo can print counts before failing, and a
    /// refutation still exits 1 when its summary explicitly disclaims a measurement.
    /// # Errors
    /// Returns the gate status when the exit or summary cannot supply a proof count.
    pub(super) fn floor_counts(&self) -> std::result::Result<(u64, u64), String> {
        let s = self.compiler_summary()?;
        if s.unmeasured() {
            Err(format!("NOT-MEASURED(rc={},{})", self.rc, s.outcome))
        } else {
            Ok((s.proved, s.total))
        }
    }

    /// Parses a crate's `targo trust check` log and `.rc` exit-code file into [`Measured`] facts.
    /// # Errors
    /// A log read failed; even a complete-looking prefix cannot establish its final facts.
    pub fn of_log(log: impl BufRead, rc: &str) -> std::io::Result<Measured> {
        let mut m = Measured {
            rc: rc.to_string(),
            ..Measured::default()
        };
        let (mut listed, mut named): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
        let mut ceiling_bytes: Option<u64> = None;
        let mut peak_gib: Option<f64> = None;
        let (mut launch_refused, mut dirty, mut owned, mut unready) = (false, false, false, false);
        let mut cut: BTreeMap<String, String> = BTreeMap::new();
        let mut signalled = None;
        let mut coverage: Option<String> = None;
        let mut expected_failed = None;
        let mut failed_rows = None;
        let mut stop_tally = String::new();
        let mut report_reason = String::new();
        let mut refused_paths: BTreeSet<PathRefusal> = BTreeSet::new();
        let mut buf = Vec::new();
        let mut log = log;
        while read_log_line(&mut log, &mut buf)? {
            let line = String::from_utf8_lossy(&buf);
            let line = line.trim_end_matches(['\n', '\r']);
            if !judged(line) {
                continue;
            }
            if line.contains("verification did not run") || line.contains("transport:missing-json")
            {
                m.transport = true;
            }
            launch_refused |= line.contains(TRUSTD_REFUSED);
            dirty |= line.contains(TRUSTD_DIRTY);
            owned |= line.contains("another trustd owns");
            unready |= line.contains("did not establish exact-identity readiness");
            let lead = line.trim_start();
            if let Some(expected) = expected_failed.take() {
                failed_rows = failure_breakdown(lead, expected);
            }
            if lead.starts_with("Summary:") {
                m.summary = one_line(line);
                expected_failed = summary_line(&m.summary)
                    .filter(Summary::unmeasured)
                    .and_then(|s| s.failed);
                failed_rows = None;
                coverage = None;
                stop_tally.clear();
                report_reason.clear();
                m.rss = 0;
            }
            if lead.starts_with("Coverage:") {
                coverage = Some(one_line(lead));
            }
            if let Some(causes) = lead.strip_prefix("Stop causes:") {
                // Later clauses can list thousands of individual stops.
                let tally = causes.split([';', '—']).next().unwrap_or_default().trim();
                m.rss = stop_count(tally, "rss-backstop");
                stop_tally = one_line(tally);
            }
            if let Some(at) = line.find(TARGO_COULD_NOT_RUN) {
                let reason = if lead.starts_with("Stop causes:") {
                    &mut report_reason
                } else {
                    &mut m.could_not_run
                };
                if reason.is_empty() {
                    *reason = one_line(&line[at..]);
                }
            } else if report_reason.is_empty()
                && let Some(at) = line.find(TARGO_NO_RESULT)
            {
                report_reason = one_line(&line[at..]);
                if !stop_tally.is_empty() {
                    let _ = write!(report_reason, " (Stop causes: {stop_tally})");
                }
            }
            for (kind, msg) in transport_rows(line) {
                if kind != "missing-json" {
                    cut.entry(kind).or_insert(msg);
                }
            }
            if signalled.is_none() {
                signalled = compiler_signal(line);
            }
            refused_paths.extend(path_refusals(line));
            if let Some(a) = line.find(ADMISSION) {
                let said = line[a..].to_string();
                let last = line.rfind(ADMISSION).unwrap_or(a);
                if line
                    .find("[UNKNOWN]")
                    .is_some_and(|u| u + "[UNKNOWN]".len() <= last)
                {
                    listed.push(said);
                } else if listed.is_empty() {
                    named.push(said);
                }
            }
            for i in find_all(line, TIMED_OUT) {
                let d = digits_at(line, i + TIMED_OUT.len());
                m.lapsed = m.lapsed.saturating_add(d.parse().unwrap_or(0));
            }
            if line.contains(CHECK_CANCELLED) {
                m.cancelled = m.cancelled.saturating_add(1);
            }
            if lead
                .strip_prefix("[UNKNOWN]")
                .is_some_and(|rest| rest.contains(BUDGET))
            {
                m.budget_rows += 1;
            }
            if let Some(s) = census_skipped(line) {
                m.skipped = Some(m.skipped.unwrap_or(0).saturating_add(s));
            }
            if let Some(c) = census_figure(line, " ceiling ") {
                m.ceiling = c;
            }
            if let Some(p) = census_figure(line, " peak ") {
                // The largest peak, compared in GiB.
                if peak_gib.is_none_or(|was| gib_of(&p).is_some_and(|now| now > was)) {
                    peak_gib = gib_of(&p);
                    m.peak = p;
                }
            }
            for i in find_all(line, CEILING_FLAG) {
                let at = i + CEILING_FLAG.len();
                if matches!(line.as_bytes().get(at), Some(b'=' | b' ')) {
                    let d = digits_at(line, at + 1);
                    if let Ok(b) = d.parse() {
                        ceiling_bytes = Some(b);
                    }
                }
            }
        }
        if matches!(rc.trim(), "0" | "1")
            && (failed_rows.is_some() || !stop_tally.is_empty())
            && summary_line(&m.summary).is_some_and(|s| s.unmeasured())
        {
            let mut diagnostics: Vec<_> = failed_rows.into_iter().collect();
            if !stop_tally.is_empty() {
                let tally: String = stop_tally.chars().take(DIAGNOSTIC_CHARS).collect();
                diagnostics.push(format!("Stop causes: {tally}"));
            }
            m.could_not_run = diagnostics.join("; ");
        } else if m.could_not_run.is_empty() {
            m.could_not_run = report_reason;
        }
        let refusals = if listed.is_empty() { named } else { listed };
        m.refused = refusals.len() as u64;
        m.refused_why = admission_cause(&refusals);
        if m.ceiling.is_empty()
            && let Some(b) = ceiling_bytes
        {
            m.ceiling = format!("{b} B");
        }
        if m.summary.is_empty() && launch_refused {
            m.launch = if dirty {
                "dirty"
            } else if owned {
                "owned"
            } else if unready {
                "unready"
            } else {
                "other"
            }
            .into();
        }
        let mut ended: Vec<String> = cut
            .iter()
            .map(|(kind, msg)| {
                let msg = if msg.is_empty() {
                    "a row of that kind"
                } else {
                    msg.as_str()
                };
                format!("targo-trust: {msg} [transport:{kind}]")
            })
            .collect();
        ended.extend(signalled);
        if let Some(n) = parse_uint::<u32>(rc.trim())
            .filter(|c| (129..=192).contains(c))
            .map(|c| c - 128)
        {
            ended.push(format!(
                "the check itself ended by signal {n}{}",
                signal_said(signal_name(n))
            ));
        }
        if !m.summary.is_empty() {
            match &coverage {
                Some(c) if coverage_complete(c) => {}
                Some(c) => ended.push(format!("targo-trust's {c}")),
                None => ended.push(
                    "targo-trust printed no Coverage line, so nothing says the compiler covered the whole crate"
                        .into(),
                ),
            }
        }
        m.ended = one_line(&ended.join("; "));
        m.path_refused = refused_paths.into_iter().collect();
        Ok(m)
    }

    fn to_fields(&self) -> String {
        let mut out = String::with_capacity(128);
        let _ = write!(
            out,
            "{}\t{}\t{}\t{}\t{}\t{}\t",
            one_line_cow(&self.rc),
            u8::from(self.transport),
            self.refused,
            self.lapsed,
            self.cancelled,
            self.budget_rows,
        );
        match self.skipped {
            Some(n) => {
                let _ = write!(out, "{n}\t");
            }
            None => out.push_str("-\t"),
        }
        let _ = write!(
            out,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t",
            one_line_cow(&self.ceiling),
            one_line_cow(&self.peak),
            one_line_cow(&self.launch),
            one_line_cow(&self.held),
            one_line_cow(&self.held_what),
            one_line_cow(&self.refused_why),
            one_line_cow(&self.wall_budget),
            one_line_cow(&self.ended),
            self.rss,
        );
        match self.pace {
            Some(p) => {
                out.push_str(&p.text());
                out.push('\t');
            }
            None => out.push_str("-\t"),
        }
        match &self.backstops {
            Some(b) => {
                let _ = write!(out, "{}\t", b.len());
            }
            None => out.push_str("-\t"),
        }
        let _ = write!(
            out,
            "{}\t{}\t{}\t",
            one_line_cow(&self.trustd_lost),
            one_line_cow(&self.could_not_run),
            one_line_cow(&self.summary),
        );
        self.grade.write_field(&mut out);
        out
    }

    /// The fields of [`Measured::to_fields`], and how many `backstop` lines follow.
    fn from_fields(f: &[&str], has_grade: bool) -> Option<(Measured, Option<usize>)> {
        let (f, grade) = if has_grade {
            let (grade, fields) = f.split_last()?;
            (fields, GradeEvidence::parse(grade)?)
        } else {
            (f, GradeEvidence::Legacy)
        };
        let [
            rc,
            transport,
            refused,
            lapsed,
            cancelled,
            budget_rows,
            skipped,
            ceiling,
            peak,
            launch,
            held,
            held_what,
            refused_why,
            wall_budget,
            ended,
            rss,
            pace,
            backstops,
            trustd_lost,
            could_not_run,
            summary,
        ] = f
        else {
            return None;
        };
        let pace = match *pace {
            "-" => None,
            p => Some(Pace::parse(p)?),
        };
        let backstops: Option<usize> = match *backstops {
            "-" => None,
            n => Some(parse_uint(n)?),
        };
        if !matches!(*launch, "" | "dirty" | "owned" | "unready" | "other") {
            return None;
        }
        if !(wall_budget.is_empty()
            || *wall_budget == "?"
            || wall_budget.bytes().all(|b| b.is_ascii_digit()))
        {
            return None;
        }
        let m = Measured {
            rc: (*rc).to_string(),
            transport: match *transport {
                "0" => false,
                "1" => true,
                _ => return None,
            },
            refused: parse_uint(refused)?,
            lapsed: parse_uint(lapsed)?,
            cancelled: parse_uint(cancelled)?,
            budget_rows: parse_uint(budget_rows)?,
            skipped: match *skipped {
                "-" => None,
                n => Some(parse_uint(n)?),
            },
            ceiling: (*ceiling).to_string(),
            peak: (*peak).to_string(),
            launch: (*launch).to_string(),
            held: (*held).to_string(),
            held_what: (*held_what).to_string(),
            refused_why: (*refused_why).to_string(),
            wall_budget: (*wall_budget).to_string(),
            ended: (*ended).to_string(),
            summary: (*summary).to_string(),
            path_refused: Vec::new(),
            rss: parse_uint(rss)?,
            pace,
            // Allocate only for the rows that actually follow the declaration.
            backstops: backstops.map(|_| Vec::new()),
            trustd_lost: (*trustd_lost).to_string(),
            could_not_run: (*could_not_run).to_string(),
            grade,
        };
        Some((m, backstops))
    }
}

/// Read one bounded logical line, discarding an overlong diagnostic through its real newline.
/// A continuation chunk can never become an independent Summary or Coverage line.
fn read_log_line(log: &mut impl BufRead, buf: &mut Vec<u8>) -> std::io::Result<bool> {
    use std::io::Read as _;
    loop {
        buf.clear();
        if (&mut *log).take(LINE_MAX + 1).read_until(b'\n', buf)? == 0 {
            return Ok(false);
        }
        if buf.len() as u64 <= LINE_MAX {
            return Ok(true);
        }
        if buf.last() != Some(&b'\n') {
            log.skip_until(b'\n')?;
        }
    }
}

/// The last top-level Targo message retained in a completed check log. This
/// excerpt is for display only and does not participate in a proof verdict.
pub(super) fn last_targo_message(log: &Path) -> std::io::Result<Option<String>> {
    let mut log = open_log(log)?;
    let mut buf = Vec::new();
    let mut last = None;
    while read_log_line(&mut log, &mut buf)? {
        if let Some(message) = buf.trim_ascii_end().strip_prefix(b"targo trust: ") {
            let message = String::from_utf8_lossy(message);
            last = Some(diagnostic_excerpt(&message));
        }
    }
    Ok(last)
}

const DIAGNOSTIC_CHARS: usize = 512;

fn diagnostic_excerpt(text: &str) -> String {
    let mut chars = text.escape_debug();
    let mut excerpt: String = chars.by_ref().take(DIAGNOSTIC_CHARS).collect();
    if chars.next().is_some() {
        excerpt.push('…');
    }
    excerpt
}

/// Read an optional execution mark. A present but unreadable mark is unavailable input.
fn optional_receipt(path: &Path) -> std::io::Result<Option<String>> {
    optional_text(path).map(|text| text.map(|text| text.trim().to_string()))
}

/// Read optional text without changing its contents. Only a missing path is absence.
fn optional_text(path: &Path) -> std::io::Result<Option<String>> {
    match crate::durable::read_text(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(std::io::Error::new(
            e.kind(),
            format!("{}: {e}", path.display()),
        )),
    }
}

/// The required status uses the same canonical signed decimal as `trust_lane::Ended::code`.
fn execution_status(path: &Path) -> std::io::Result<String> {
    let text = crate::durable::read_text(path)
        .map_err(|e| std::io::Error::new(e.kind(), format!("{}: {e}", path.display())))?;
    let status = text.trim();
    if status
        .parse::<i32>()
        .ok()
        .is_none_or(|n| n.to_string() != status)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{}: no canonical compiler exit status", path.display()),
        ));
    }
    Ok(status.to_string())
}

const GIB: f64 = 1_073_741_824.0;

pub const MEMORY: &str = "crates/aterm-release/trust/memory.tsv";

const HOST_OBS_HEADER: &str = "# trust lane host memory observations: <name@version> <held|cap-bound> <run ceiling GiB> <verifier peak GiB> <proved> <floor> <prover> <when>, one row per library whose last check here ran out of the verifier's memory ceiling. While its version, floor and prover stand, a cap-bound row is refused up front on this host and a held one is gated; --try-refused runs a refused one again. Local to this machine, never committed.";

fn four(x: f64) -> f64 {
    format!("{x:.4}").parse().unwrap_or(x)
}

fn one(x: f64) -> String {
    format!("{x:.1}")
}

fn gib_of(figure: &str) -> Option<f64> {
    let (v, unit) = figure.split_once(' ')?;
    let v: f64 = gib(v)?;
    let f = match unit.as_bytes().first() {
        Some(b'K') => 1.0 / 1_048_576.0,
        Some(b'M') => 1.0 / 1024.0,
        Some(b'G') => 1.0,
        Some(b'T') => 1024.0,
        _ => 1.0 / GIB,
    };
    Some(four(v * f))
}

fn gib(s: &str) -> Option<f64> {
    let (int, frac) = s.split_once('.').unwrap_or((s, "0"));
    let digits = |p: &str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit());
    (digits(int) && digits(frac))
        .then(|| s.parse().ok())
        .flatten()
}

fn gib_le(a: &str, b: &str) -> bool {
    matches!((gib(a), gib(b)), (Some(a), Some(b)) if a <= b)
}

fn gib_max(a: &str, b: &str) -> String {
    let (x, y) = (gib(a).unwrap_or(0.0), gib(b).unwrap_or(0.0));
    one(if x >= y { x } else { y })
}
fn gib_min(a: &str, b: &str) -> String {
    let (x, y) = (gib(a).unwrap_or(0.0), gib(b).unwrap_or(0.0));
    one(if x <= y { x } else { y })
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn ram_for(x: f64) -> u64 {
    let r = 8.0 * x;
    let i = r.trunc();
    (if i < r - 1e-9 { i + 1.0 } else { i }) as u64
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn ram_above(x: f64) -> u64 {
    ((8.0 * x + 1e-9).trunc() as u64).saturating_add(1)
}

/// The `-Z` flag through which `targo trust` gives a verifier its memory ceiling: a prover
/// whose `trustc -Z help` lists it has one (321aaeda7, store build 9192), and one whose does
/// not has none (20565c325, store build 15698, which descends from the upstream deletion of
/// the ceiling).
pub const CEILING_FLAG: &str = "trust-verify-memory-ceiling-bytes";

/// Host RAM and unknown-process state sampled at the start of a verification run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Host {
    pub mem: u64,
    pub memtotal: bool,
    /// The prover's trustc takes no memory ceiling ([`CEILING_FLAG`]; the plan's
    /// `driver-ceiling no`): the RAM / 8 model refuses nothing, and a library is refused up
    /// front only when its recorded peak, with the margin its check claims, is not below the
    /// RAM ([`peak_seen`]). A record or plan from before the probe has the ceiling.
    pub uncapped: bool,
    /// The most memory that can ever be available here, bytes: the RAM less what is wired
    /// ([`aterm_capacity::Reading::reachable`]), which the launch gate's claims are held to; 0
    /// where unread, and in a facts record, which keeps it only in a refusal's row
    /// ([`peak_seen`]).
    pub reach: u64,
}

impl Host {
    /// The bound a claim must be below for the launch gate ever to admit it ([`peak_seen`]):
    /// [`Self::reach`] where read and below the RAM, else the RAM. The gate is held to it too
    /// (`trust_lane::Admission::held_to`), so it and the plan judge against one bound.
    pub(crate) const fn admits_below(&self) -> u64 {
        if self.reach > 0 && self.reach < self.mem {
            self.reach
        } else {
            self.mem
        }
    }

    #[allow(clippy::cast_precision_loss)]
    fn ram_gib(&self) -> String {
        let g = self.mem as f64 / GIB;
        if (g - g.trunc()).abs() < f64::EPSILON {
            format!("{g:.0}")
        } else {
            format!("{g:.1}")
        }
    }

    /// Formats the per-verifier memory ceiling (`RAM / 8` in GiB to one decimal place): none
    /// when the RAM is unread or the prover takes no ceiling ([`Host::uncapped`]).
    #[allow(clippy::cast_precision_loss)]
    #[must_use]
    pub fn ceiling(&self) -> Option<String> {
        (self.mem > 0 && !self.uncapped).then(|| one((self.mem / 8) as f64 / GIB))
    }

    fn desc(&self) -> String {
        match self.ceiling() {
            Some(c) => format!("{} GiB RAM, verifier ceiling {c} GiB", self.ram_gib()),
            None if self.mem > 0 => format!("{} GiB RAM, no verifier ceiling", self.ram_gib()),
            None => "memory unreadable".into(),
        }
    }

    fn share(&self) -> String {
        if self.mem > 0 {
            format!("{} GiB RAM / 8", self.ram_gib())
        } else {
            "targo trust: RAM / 8".into()
        }
    }

    fn ram_class(&self) -> Option<String> {
        (self.mem > 0).then(|| self.ram_gib())
    }

    fn ram_phrase(&self, sure: Option<u64>, may: Option<u64>) -> String {
        let of = if self.memtotal {
            "of RAM as the OS reports it (MemTotal)"
        } else {
            "of RAM"
        };
        let may = may.filter(|m| self.mem == 0 || !gib_le(&m.to_string(), &self.ram_gib()));
        match (sure, may) {
            (Some(s), _) => format!("{s} GiB {of} verifies it"),
            (None, Some(m)) => format!(
                "its floor records no ceiling, so no RAM is known to verify it ({m} GiB may)"
            ),
            (None, None) => "its floor records no ceiling, so no RAM is known to verify it".into(),
        }
    }
}

/// Single crate's row from the verification memory record (`trust-memory.txt`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemRow {
    pub f: String,
    pub ram: String,
    pub at: String,
    pub need: String,
    pub bound: String,
    pub when: String,
}

impl Default for MemRow {
    fn default() -> MemRow {
        MemRow {
            f: "-".into(),
            ram: "-".into(),
            at: "-".into(),
            need: "-".into(),
            bound: "-".into(),
            when: "-".into(),
        }
    }
}

impl MemRow {
    fn fields(&self) -> String {
        format!(
            "{}\t{}\t{}\t{}\t{}\t{}",
            one_line_cow(&self.f),
            one_line_cow(&self.ram),
            one_line_cow(&self.at),
            one_line_cow(&self.need),
            one_line_cow(&self.bound),
            one_line_cow(&self.when),
        )
    }

    fn from_fields(f: &[&str]) -> Option<MemRow> {
        let [f, ram, at, need, bound, when] = f else {
            return None;
        };
        Some(MemRow {
            f: (*f).into(),
            ram: (*ram).into(),
            at: (*at).into(),
            need: (*need).into(),
            bound: (*bound).into(),
            when: (*when).into(),
        })
    }
}

fn same_ram(a: &str, b: &str) -> bool {
    matches!((gib(a), gib(b)), (Some(a), Some(b)) if (a - b).abs() < 0.05)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reading {
    Here,
    Upper,
    Lower,
    Unknown,
}

impl Reading {
    #[allow(clippy::cast_precision_loss)]
    fn of(row: &MemRow, host: &Host) -> Reading {
        let here = host.mem as f64 / GIB;
        match gib(&row.ram) {
            Some(_) if host.mem > 0 && same_ram(&row.ram, &host.ram_gib()) => Reading::Here,
            Some(r) if host.mem > 0 && r > here => Reading::Upper,
            Some(_) if host.mem > 0 => Reading::Lower,
            _ => Reading::Unknown,
        }
    }
}

fn row_for<'a>(rows: &'a [MemRow], host: &Host) -> Option<&'a MemRow> {
    let ram = |r: &&MemRow| gib(&r.ram).unwrap_or(0.0);
    let by_ram = |a: &&MemRow, b: &&MemRow| ram(a).total_cmp(&ram(b));
    let read = |w: Reading| {
        rows.iter()
            .filter(move |r| r.ram != "-" && Reading::of(r, host) == w)
    };
    read(Reading::Here)
        .next()
        .or_else(|| read(Reading::Lower).max_by(by_ram))
        .or_else(|| read(Reading::Upper).min_by(by_ram))
        .or_else(|| read(Reading::Unknown).max_by(by_ram))
        .or_else(|| rows.first())
}

fn fits(row: &MemRow, host: &Host, c: &str) -> bool {
    row.bound == "="
        && match Reading::of(row, host) {
            Reading::Here => {
                matches!((gib(&row.need), gib(c)), (Some(n), Some(x)) if n < x)
                    || gib_le(&row.at, c)
            }
            Reading::Upper | Reading::Unknown => gib_le(&row.need, c),
            Reading::Lower => false,
        }
}

fn tab_fields(line: &str, n: usize) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::with_capacity(n);
    let mut rest = line.trim_matches('\t');
    while out.len() + 1 < n && !rest.is_empty() {
        let (f, r) = rest.split_once('\t').unwrap_or((rest, ""));
        out.push(f);
        rest = r.trim_start_matches('\t');
    }
    if !rest.is_empty() {
        out.push(rest);
    }
    out
}

/// Parses `trust-memory.txt` into a map of crate names to [`MemRow`]s and any format errors.
pub fn parse_memory(
    text: &str,
    name: &str,
) -> std::result::Result<BTreeMap<String, Vec<MemRow>>, String> {
    let gib_or_dash = |s: &str| s == "-" || gib(s).is_some();
    let mut out: BTreeMap<String, Vec<MemRow>> = BTreeMap::new();
    for line in text.lines() {
        let f = tab_fields(line, 7);
        let Some(&lib) = f.first().filter(|n| !n.starts_with('#')) else {
            continue;
        };
        let col = |i: usize| f.get(i).copied().unwrap_or("");
        let (fc, ram, at, need, bound) = (col(1), col(2), col(3), col(4), col(5));
        let bad = if !gib_or_dash(fc) {
            format!("the floor ceiling '{fc}'; it must be GiB (e.g. 16.0) or '-'")
        } else if !gib_or_dash(ram) {
            format!("the host RAM '{ram}'; it must be GiB (e.g. 24) or '-'")
        } else if !gib_or_dash(at) {
            format!("the run ceiling '{at}'; it must be GiB (e.g. 3.0) or '-'")
        } else if !gib_or_dash(need) {
            format!("the need '{need}'; it must be GiB (e.g. 1.4) or '-'")
        } else if !matches!(bound, "=" | ">" | "-") {
            format!(
                "the bound '{bound}'; it must be '=' (measured with room), '>' (it ran out, so more) or '-'"
            )
        } else if (need == "-") != (bound == "-") {
            format!(
                "the need '{need}' with the bound '{bound}'; a need takes a bound, and a bound a need"
            )
        } else if (need == "-") != (ram == "-") {
            format!(
                "the need '{need}' with the host RAM '{ram}'; a need names the RAM it was measured on, and a host RAM a need"
            )
        } else if need == "-" && at != "-" {
            format!("the run ceiling '{at}' with no need")
        } else {
            String::new()
        };
        if !bad.is_empty() {
            return Err(format!("{name} gives '{lib}' {bad}"));
        }
        let when = col(6);
        let row = MemRow {
            f: fc.into(),
            ram: ram.into(),
            at: at.into(),
            need: need.into(),
            bound: bound.into(),
            when: if when.is_empty() {
                "-".into()
            } else {
                when.into()
            },
        };
        let rows = out.entry(lib.to_string()).or_default();
        let clash = if let Some(first) = rows.first().filter(|r| r.f != row.f) {
            format!(
                "two floor ceilings, '{}' and '{fc}'; every row of a library names the ceiling its one floor was set at",
                first.f
            )
        } else if !rows.is_empty() && (ram == "-" || rows.iter().any(|r| r.ram == "-")) {
            "a row with no need beside another; a library records no need, or one per RAM it was measured on".to_string()
        } else if rows.iter().any(|r| same_ram(&r.ram, ram)) {
            format!("two rows measured on {ram} GiB of RAM; a library has one row per RAM")
        } else {
            String::new()
        };
        if !clash.is_empty() {
            return Err(format!("{name} gives '{lib}' {clash}"));
        }
        rows.push(row);
    }
    Ok(out)
}

/// Per-crate out-of-memory observation recorded on the current host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostObs {
    pub spec: String,
    pub outcome: String,
    pub ceil: String,
    pub peak: String,
    pub proved: String,
    pub floor: String,
    pub prover: String,
    pub when: String,
}

impl HostObs {
    fn seen(&self) -> Seen {
        Seen {
            outcome: self.outcome.clone(),
            ceil: self.ceil.clone(),
            peak: self.peak.clone(),
            when: self.when.clone(),
            prover: String::new(),
        }
    }
}

/// Host memory-exhaustion observation attached to an up-front memory [`Refusal`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seen {
    pub outcome: String,
    /// The ceiling it was held to, `-` for none, in one of two units: a `held` or `cap-bound`
    /// row's verifier ceiling in GiB (`6.0`, as [`HostObs::ceil`]), a [`PEAK`] row's reach in
    /// bytes ([`peak_seen`]).
    pub ceil: String,
    pub peak: String,
    pub when: String,
    /// The prover whose cost row an [`OTHER_PEAK`] or [`OTHER_LEAST`] refusal read (its
    /// `targo --version`), empty for any other ([`Peak::of`]).
    pub prover: String,
}

/// What a plan or a facts record says of a library no [`Seen`] refused.
const NO_SEEN: &str = "-\t-\t-\t-\t-";

impl Seen {
    fn fields(&self) -> String {
        let prover = if self.prover.is_empty() {
            "-"
        } else {
            &self.prover
        };
        format!(
            "{}\t{}\t{}\t{}\t{}",
            one_line_cow(&self.outcome),
            one_line_cow(&self.ceil),
            one_line_cow(&self.peak),
            one_line_cow(&self.when),
            one_line_cow(prover),
        )
    }

    fn from_fields(f: &[&str]) -> Option<Option<Seen>> {
        let [outcome, ceil, peak, when, prover] = f else {
            return None;
        };
        Some((*outcome != "-").then(|| Seen {
            outcome: (*outcome).into(),
            ceil: (*ceil).into(),
            peak: (*peak).into(),
            when: (*when).into(),
            prover: if *prover == "-" {
                String::new()
            } else {
                (*prover).into()
            },
        }))
    }
}

/// Parses valid [`HostObs`] entries from the host's local observation file.
#[must_use]
pub fn parse_host_obs(text: &str) -> Vec<HostObs> {
    text.lines()
        .filter_map(|line| {
            let f = tab_fields(line, 8);
            let col = |i: usize| f.get(i).copied().unwrap_or_default();
            let spec = col(0);
            let outcome = col(1);
            let ceil = col(2);
            let floor = col(5);
            (!spec.is_empty()
                && !spec.starts_with('#')
                && matches!(outcome, "held" | "cap-bound")
                && gib(ceil).is_some()
                && !floor.is_empty()
                && floor.bytes().all(|b| b.is_ascii_digit()))
            .then(|| HostObs {
                spec: spec.into(),
                outcome: outcome.into(),
                ceil: ceil.into(),
                peak: col(3).into(),
                proved: col(4).into(),
                floor: floor.into(),
                prover: col(6).into(),
                when: col(7).into(),
            })
        })
        .collect()
}

/// Reason this host refuses to run `targo trust check` on a crate up front.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub line: String,
    pub token: String,
    pub sure: Option<u64>,
}

/// Determines whether this host must refuse verifying a crate up front due to memory bounds.
pub(crate) fn memory_refusal(
    host: &Host,
    name: &str,
    has_floor: bool,
    row: &MemRow,
    obs: Option<&Seen>,
) -> Option<Refusal> {
    let c = host.ceiling()?;
    if !has_floor || (row.f != "-" && gib_le(&row.f, &c)) {
        return None;
    }
    if obs.is_some_and(|o| o.outcome == "held" && gib_le(&o.ceil, &c)) {
        return None;
    }
    if fits(row, host, &c) {
        return None;
    }
    let sure = gib(&row.f).map(ram_for);
    if let Some(o) = obs.filter(|o| o.outcome == "cap-bound" && gib_le(&c, &o.ceil)) {
        let peak = if o.peak == "-" { "0" } else { o.peak.as_str() };
        let low = gib_max(peak, &o.ceil);
        let may =
            (row.f == "-" || !gib_le(&row.f, &low)).then(|| ram_above(gib(&low).unwrap_or(0.0)));
        return Some(Refusal {
            line: format!(
                "{name} not verified here: needs more than {low} GiB, this host gives {c} GiB ({}); it ran out here on {}; {}",
                host.share(),
                o.when.get(..10).unwrap_or(&o.when),
                host.ram_phrase(sure, may)
            ),
            token: format!(">{low}"),
            sure,
        });
    }
    let reading = Reading::of(row, host);
    if row.bound == "=" && (reading == Reading::Here || !gib_le(&row.need, &c)) {
        let room = gib(&row.at).map(ram_for);
        let above = gib(&row.need).map(ram_above);
        let under = above.filter(|r| gib_le(&r.to_string(), &row.ram));
        let sure = [sure, room, under].into_iter().flatten().min();
        let may = above;
        let measured = if reading == Reading::Here {
            String::new()
        } else {
            format!(", measured on {} GiB of RAM", row.ram)
        };
        let tight = if gib_le(&row.need, &c) {
            ", no room above the need"
        } else {
            ""
        };
        let short = format!("this host gives {c} GiB ({}){tight}", host.share());
        return Some(Refusal {
            line: format!(
                "{name} not verified here: needs ~{} GiB{measured}, {short}; {}",
                row.need,
                host.ram_phrase(sure, may)
            ),
            token: format!("~{}", row.need),
            sure,
        });
    }
    if row.bound == ">" && gib_le(&c, &row.need) {
        let may = (row.f == "-" || !gib_le(&row.f, &row.need))
            .then(|| ram_above(gib(&row.need).unwrap_or(0.0)));
        return Some(Refusal {
            line: format!(
                "{name} not verified here: needs more than {} GiB, this host gives {c} GiB ({}); {}",
                row.need,
                host.share(),
                host.ram_phrase(sure, may)
            ),
            token: format!(">{}", row.need),
            sure,
        });
    }
    None
}

/// The [`Seen::outcome`] of a library refused because its last recorded check's peak, with
/// the margin its check claims, is not below what this host can ever have available
/// ([`peak_seen`]): its `peak` is that peak in bytes, its `when` the cost history's stamp. The
/// peak's cost row names no prover: it was recorded before rows named one, and is read under
/// any prover until one records its own.
pub const PEAK: &str = "peak";

/// A [`PEAK`] refusal whose cost row names the prover that runs: that prover's own peak.
pub const OWN_PEAK: &str = "own-peak";

/// A [`PEAK`] refusal whose row's check was cut short (over budget, interrupted, or ended
/// without a whole summary): its peak is the least the check needs.
pub const LEAST: &str = "least";

/// An [`OWN_PEAK`] refusal whose row's check was cut short, as [`LEAST`].
pub const OWN_LEAST: &str = "own-least";

/// A [`PEAK`] refusal whose cost row another prover recorded ([`Seen::prover`]): no check of
/// the library under the prover that runs has finished, and the other prover's peak of the
/// same library is the best estimate there is of what its check needs ([`Peak::of`]).
pub const OTHER_PEAK: &str = "other-peak";

/// An [`OTHER_PEAK`] refusal whose row's check was cut short, as [`LEAST`].
pub const OTHER_LEAST: &str = "other-least";

/// A library's last recorded check, as the cost history holds it for the prover that runs
/// ([`Plan::admit`]): a row that prover recorded, or one that names no prover; or, where no
/// check under that prover has finished and its own row's peak is not refused by itself,
/// another prover's row whose peak is larger ([`Peak::of`]), which sizes the claim and refuses
/// up front but decides nothing else.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Peak {
    /// Its largest process's peak RSS, in bytes.
    pub rss: u64,
    /// When it was recorded (RFC 3339).
    pub when: String,
    /// Whether its row names the prover that runs; one that names none predates rows that do.
    pub own: bool,
    /// Whether its check was cut short, so that `rss` is the least it needs.
    pub least: bool,
    /// The prover that recorded it (its `targo --version`) where that is another prover than
    /// the one that runs, empty otherwise. aterm-gui's check under 29ebb403d starts with no row
    /// of its own; claiming what an unmeasured library claims (16 GiB) let one under 20565c325
    /// reach a 33 GB footprint while the capacity queue held the other steps' claims for two
    /// hours on 2026-10-30, so another prover's peak of the same library stands until one of
    /// its own decides: a finished check, or a peak refused by itself.
    pub of: String,
}

/// A library whose last recorded check peaked at `maxrss` bytes (the cost history's largest
/// process, stamped `when`) so near this host's memory that its claim
/// ([`aterm_capacity::peak_claim`]: that peak, a fifth of it and at least 1 GiB more; one
/// library's recorded peaks moved by 14% between two runs on one prover, aterm-render's 13.2 GB
/// on 2026-10-22 and 15.0 GB on 2026-10-21) is not below what can ever be available
/// ([`Host::reach`], the RAM less what is wired, else the RAM): what is available never
/// reaches that, so the launch gate could never admit it
/// ([`aterm_capacity::never`]), and a peak above the RAM cannot fit at all. It is refused up
/// front whatever the prover's ceiling, its row's ceiling the reach it was held to (`-` where
/// that was the RAM), its outcome [`OWN_PEAK`] where the row names the prover that runs,
/// [`OTHER_PEAK`] where it names another ([`Peak::of`]) and [`PEAK`] where it names none
/// ([`OWN_LEAST`], [`OTHER_LEAST`] and [`LEAST`] where its check was cut short, so that the
/// peak is the least it needs). `None` when its claim is below that, or the RAM is unread.
#[must_use]
pub fn peak_seen(host: &Host, peak: &Peak) -> Option<Seen> {
    let (maxrss, when) = (peak.rss, peak.when.as_str());
    let below = host.admits_below();
    (host.mem > 0 && aterm_capacity::peak_claim(maxrss) >= below).then(|| Seen {
        outcome: match (peak.own, !peak.of.is_empty(), peak.least) {
            (_, true, false) => OTHER_PEAK,
            (_, true, true) => OTHER_LEAST,
            (true, false, false) => OWN_PEAK,
            (false, false, false) => PEAK,
            (true, false, true) => OWN_LEAST,
            (false, false, true) => LEAST,
        }
        .into(),
        ceil: if below < host.mem {
            below.to_string()
        } else {
            "-".into()
        },
        peak: maxrss.to_string(),
        when: if when.is_empty() {
            "-".into()
        } else {
            when.into()
        },
        prover: peak.of.clone(),
    })
}

/// The one line a [`PEAK`], [`OWN_PEAK`], [`OTHER_PEAK`], [`LEAST`], [`OWN_LEAST`] or
/// [`OTHER_LEAST`] refusal says: whose peak it is (this prover's, another named prover's with
/// none finished under this one, or a row's that names none), the peak, and where its check was
/// cut short that it is the least the check needs, its date and this host's RAM, and the claim
/// when the peak itself is below the RAM, with the memory that can ever be available where the
/// row holds it ([`peak_seen`]). Its token is the claim, the memory a host must be able to make
/// available to admit it. `None` for any other [`Seen`].
#[allow(clippy::cast_precision_loss)]
pub(crate) fn peak_refusal(host: &Host, name: &str, seen: &Seen) -> Option<Refusal> {
    let (own, least) = match seen.outcome.as_str() {
        PEAK | OTHER_PEAK => (false, false),
        OWN_PEAK => (true, false),
        LEAST | OTHER_LEAST => (false, true),
        OWN_LEAST => (true, true),
        _ => return None,
    };
    let peak: u64 = parse_uint(&seen.peak)?;
    let claim = aterm_capacity::peak_claim(peak);
    let gb = |b: u64| b as f64 / 1e9;
    let on = match seen.when.get(..10) {
        Some(day) if seen.when != "-" => format!(" on {day}"),
        _ => String::new(),
    };
    let check = if own {
        "its last check under this prover".to_string()
    } else if matches!(seen.outcome.as_str(), OTHER_PEAK | OTHER_LEAST) {
        format!(
            "no check of it under this prover has finished, and its last check under {}",
            seen.prover
        )
    } else {
        "its last check, on a cost row that names no prover,".to_string()
    };
    let peaked = if least {
        format!(
            "was cut short after reaching {:.2} GB{on}, the least it needs",
            gb(peak)
        )
    } else {
        format!("peaked at {:.2} GB{on}", gb(peak))
    };
    let ram = format!("{:.1} GB ({} GiB) of RAM", gb(host.mem), host.ram_gib());
    let reach = parse_uint::<u64>(&seen.ceil).filter(|r| *r < host.mem);
    let why = if peak > host.mem {
        format!("{check} {peaked}, above this host's {ram}")
    } else if let Some(reach) = reach {
        format!(
            "{check} {peaked}, and with its margin it claims {:.2} GB, not below the {:.1} GB of this host's {ram} that can ever be available, the rest being wired",
            gb(claim),
            gb(reach)
        )
    } else {
        format!(
            "{check} {peaked}, and with its margin it claims {:.2} GB, not below this host's {ram}, which the launch gate never has available",
            gb(claim)
        )
    };
    Some(Refusal {
        line: format!("{name} not verified here: {why}"),
        token: format!("~{}", one(claim as f64 / GIB)),
        sure: None,
    })
}

#[derive(Clone, Debug, Default)]
struct Census {
    peak: Option<String>,
    ceil: Option<String>,
    skipped: Option<u64>,
    budget: u64,
    exhausted: bool,
    atceil: bool,
}

impl Census {
    fn of(m: &Measured) -> Census {
        let (peak, ceil) = (gib_of(&m.peak), gib_of(&m.ceiling));
        let mut c = Census {
            peak: peak.map(one),
            ceil: ceil.map(one),
            skipped: m.skipped,
            budget: m.budget_rows,
            ..Census::default()
        };
        if m.skipped.is_some_and(|n| n > 0) {
            c.exhausted = true;
        } else if let (true, Some(p), Some(x)) = (m.budget_rows > 0, peak, ceil)
            && x <= p
        {
            if p <= x {
                c.atceil = true;
            } else {
                c.exhausted = true;
            }
        }
        c
    }
}

fn mem_learn(row: &MemRow, f: &str, c: &Census, today: &str, host: &Host) -> Option<MemRow> {
    let ram = host.ram_class().filter(|_| c.peak.is_some());
    let base = match &ram {
        Some(r) if Reading::of(row, host) != Reading::Here => MemRow {
            ram: r.clone(),
            ..MemRow::default()
        },
        _ => row.clone(),
    };
    let mut new = MemRow {
        f: f.to_string(),
        ..base.clone()
    };
    if let (Some(peak), Some(_)) = (&c.peak, &ram) {
        let at = c.ceil.clone().unwrap_or_else(|| "-".into());
        if !c.exhausted && !c.atceil {
            (new.at, new.need, new.bound, new.when) =
                (at, peak.clone(), "=".into(), today.to_string());
        } else {
            let cand = c
                .ceil
                .as_deref()
                .map_or_else(|| peak.clone(), |x| gib_max(peak, x));
            if base.need == "-" || !gib_le(&cand, &base.need) {
                (new.at, new.need, new.bound, new.when) = (at, cand, ">".into(), today.to_string());
            }
        }
    }
    let changed = new.f != row.f
        || !same_slot(&new, row)
        || new.at != row.at
        || new.need != row.need
        || new.bound != row.bound;
    changed.then_some(new)
}

fn same_slot(a: &MemRow, b: &MemRow) -> bool {
    (a.ram == "-" && b.ram == "-") || same_ram(&a.ram, &b.ram)
}

/// Rewrites `trust-memory.txt`, preserving header comments and sorting `rows` by crate name.
#[must_use]
pub fn memory_rewritten(text: &str, rows: &[(String, MemRow)]) -> String {
    let mut out: Vec<String> = text
        .lines()
        .filter(|l| l.is_empty() || l.starts_with('#'))
        .map(str::to_string)
        .collect();
    let line_of = |n: &str, r: &MemRow| format!("{}\t{}", one_line(n), r.fields());
    let mut data: Vec<(String, Option<MemRow>, String)> = text
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| {
            let f = tab_fields(l, 7);
            let name = f.first().copied().unwrap_or_default().to_string();
            let fields = f.get(1..).unwrap_or_default();
            (name, MemRow::from_fields(fields), l.to_string())
        })
        .collect();
    for (n, r) in rows {
        data.retain(|(dn, dr, _)| {
            dn != n
                || !dr
                    .as_ref()
                    .is_some_and(|d| same_slot(d, r) || (r.ram != "-" && d.ram == "-"))
        });
        data.push((n.clone(), Some(r.clone()), line_of(n, r)));
        for (dn, dr, line) in &mut data {
            if let Some(d) = dr.as_mut().filter(|d| *dn == *n && d.f != r.f) {
                d.f.clone_from(&r.f);
                *line = line_of(dn, d);
            }
        }
    }
    let ram_of = |r: &Option<MemRow>| r.as_ref().and_then(|r| gib(&r.ram)).unwrap_or(-1.0);
    data.sort_unstable_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| ram_of(&a.1).total_cmp(&ram_of(&b.1)))
            .then_with(|| a.2.cmp(&b.2))
    });
    out.extend(data.into_iter().map(|(_, _, l)| l));
    let mut s = out.join("\n");
    s.push('\n');
    s
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(
    clippy::large_enum_variant,
    reason = "`Gate` IS what the run measured (past the lint's 200-byte line since \
              `Measured::path_refused`, 2026-10-17); a run holds one Decision per selected \
              library, tens of them, so boxing it would put an allocation and a deref on \
              every match over a run's facts, in the lane, the judge and the comparison, \
              to save a few kilobytes"
)]
/// Per-crate verification plan decision (`Gate` or `Refuse(Refusal)`).
pub enum Decision {
    Gate(Measured),
    NoLib,
    Queued(String),
    Ambiguous,
    Memory(Option<Seen>),
}

impl Decision {
    /// Formats the decision for a `.plan` `crate` line.
    #[must_use]
    pub fn word(&self) -> String {
        match self {
            Decision::Gate(_) => "gate".into(),
            Decision::NoLib => "no-lib".into(),
            Decision::Queued(s) => format!("queued:{s}"),
            Decision::Ambiguous => "ambiguous".into(),
            Decision::Memory(_) => "memory".into(),
        }
    }

    fn from_word(word: &str) -> Option<Decision> {
        Some(match word {
            "gate" => Decision::Gate(Measured::default()),
            "no-lib" => Decision::NoLib,
            "ambiguous" => Decision::Ambiguous,
            "memory" => Decision::Memory(None),
            other => Decision::Queued(other.strip_prefix("queued:")?.to_string()),
        })
    }
}

/// Selected crate in a verification run, pairing [`Decision`], [`MemRow`], and [`Measured`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Crate {
    pub name: String,
    pub decision: Decision,
    pub memory: MemRow,
}

impl Crate {
    /// Constructs a [`Crate`] entry with no memory record row.
    #[must_use]
    pub fn new(name: &str, decision: Decision) -> Crate {
        Crate {
            name: name.into(),
            decision,
            memory: MemRow::default(),
        }
    }
}

/// Content-addressed key and input digests identifying a filed verification record.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Provenance {
    pub key: String,
    pub tree_sans_floors: String,
    pub prover: String,
    pub lane: String,
    pub prover_root: String,
    pub trustc: String,
    pub head: String,
    pub tree: String,
    pub host: String,
    pub load: String,
    pub jobs: String,
    pub date: String,
    pub remeasured: Vec<String>,
}

/// Complete unjudged facts from a single verification lane run.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    pub driver: String,
    pub driver_id: String,
    pub trustd: String,
    pub grants: String,
    pub trustd_seen: String,
    pub only: String,
    pub ambiguous: Vec<(String, String, String)>,
    pub crates: Vec<Crate>,
    pub host: Host,
    pub measured_at: String,
    pub provenance: Option<Provenance>,
}

impl Facts {
    /// Single-line prover driver version string for ratchet rows.
    #[must_use]
    pub fn driver_row(&self) -> String {
        let id = if self.driver_id.is_empty() {
            "unknown"
        } else {
            self.driver_id.as_str()
        };
        one_line(id)
    }

    /// Serializes [`Facts`] into the current text record.
    #[must_use]
    pub fn to_record(&self) -> String {
        let mut out = format!("{FACTS_MAGIC}\n");
        if let Some(p) = &self.provenance {
            for (k, v) in [
                ("key", &p.key),
                ("tree-sans-floors", &p.tree_sans_floors),
                ("prover", &p.prover),
                ("lane", &p.lane),
                ("prover-root", &p.prover_root),
                ("trustc", &p.trustc),
                ("head", &p.head),
                ("tree", &p.tree),
                ("host", &p.host),
                ("load", &p.load),
                ("jobs", &p.jobs),
                ("date", &p.date),
            ] {
                let _ = writeln!(out, "{k} {}", one_line(v));
            }
            for r in &p.remeasured {
                let _ = writeln!(out, "remeasured {}", one_line(r));
            }
        }
        for (k, v) in [
            ("driver", &self.driver),
            ("driver-id", &self.driver_id),
            ("trustd", &self.trustd),
            ("grants", &self.grants),
            ("trustd-seen", &self.trustd_seen),
            ("only", &self.only),
        ] {
            let _ = writeln!(out, "{k} {}", one_line(v));
        }
        let _ = writeln!(out, "host-mem {}", self.host.mem);
        let _ = writeln!(
            out,
            "host-mem-os {}",
            if self.host.memtotal { "MemTotal" } else { "-" }
        );
        let _ = writeln!(
            out,
            "driver-ceiling {}",
            if self.host.uncapped { "no" } else { "yes" }
        );
        let _ = writeln!(out, "measured-at {}", one_line(&self.measured_at));
        for (n, a, b) in &self.ambiguous {
            let _ = writeln!(
                out,
                "ambiguous {}\t{}\t{}",
                one_line(n),
                one_line(a),
                one_line(b)
            );
        }
        for c in &self.crates {
            let _ = write!(
                out,
                "crate {}\t{}\t{}",
                one_line(&c.name),
                one_line(&c.decision.word()),
                c.memory.fields()
            );
            match &c.decision {
                Decision::Gate(m) => {
                    out.push('\t');
                    out.push_str(&m.to_fields());
                }
                Decision::Memory(seen) => {
                    out.push('\t');
                    match seen {
                        Some(seen) => out.push_str(&seen.fields()),
                        None => out.push_str(NO_SEEN),
                    }
                }
                _ => {}
            }
            out.push('\n');
            if let Decision::Gate(Measured {
                backstops: Some(b), ..
            }) = &c.decision
            {
                for b in b {
                    let _ = writeln!(out, "backstop {}\t{}", one_line(&c.name), b.fields());
                }
            }
        }
        out.push_str(FACTS_END);
        out.push('\n');
        out
    }

    /// Reads current facts or v17 proof-floor facts that predate grade collection.
    #[must_use]
    pub fn parse(text: &str) -> Option<Facts> {
        let mut lines = text.lines();
        let has_grade = match lines.next()? {
            FACTS_MAGIC => true,
            FACTS_MAGIC_V17 => false,
            _ => return None,
        };
        let mut f = Facts::default();
        let mut p = Provenance::default();
        let mut has_provenance = false;
        let mut ended = false;
        let mut gates = BTreeSet::new();
        // The `backstop` lines the last `crate` line said follow it.
        let mut owed = 0usize;
        for line in lines {
            if ended {
                return None;
            }
            if let Some(v) = line.strip_prefix("backstop ") {
                let (name, b) = v.split_once('\t')?;
                let Some(Crate {
                    name: last,
                    decision:
                        Decision::Gate(Measured {
                            backstops: Some(list),
                            ..
                        }),
                    ..
                }) = f.crates.last_mut()
                else {
                    return None;
                };
                if owed == 0 || name != last {
                    return None;
                }
                list.push(Backstop::from_fields(b)?);
                owed -= 1;
                continue;
            }
            if owed != 0 {
                return None;
            }
            if line == FACTS_END {
                ended = true;
                continue;
            }
            let (k, v) = line.split_once(' ').unwrap_or((line, ""));
            let prov = |slot: &mut String| {
                *slot = v.to_string();
                true
            };
            has_provenance |= match k {
                "key" => prov(&mut p.key),
                "tree-sans-floors" => prov(&mut p.tree_sans_floors),
                "prover" => prov(&mut p.prover),
                "lane" => prov(&mut p.lane),
                "prover-root" => prov(&mut p.prover_root),
                "trustc" => prov(&mut p.trustc),
                "head" => prov(&mut p.head),
                "tree" => prov(&mut p.tree),
                "host" => prov(&mut p.host),
                "load" => prov(&mut p.load),
                "jobs" => prov(&mut p.jobs),
                "date" => prov(&mut p.date),
                "remeasured" => {
                    p.remeasured.push(v.to_string());
                    true
                }
                _ => false,
            };
            match k {
                "key" | "tree-sans-floors" | "prover" | "lane" | "prover-root" | "trustc"
                | "head" | "tree" | "host" | "load" | "jobs" | "date" | "remeasured" => {}
                "driver" => f.driver = v.to_string(),
                "driver-id" => f.driver_id = v.to_string(),
                "trustd" => f.trustd = v.to_string(),
                "grants" => f.grants = v.to_string(),
                "trustd-seen" => f.trustd_seen = v.to_string(),
                "only" => f.only = v.to_string(),
                "host-mem" => f.host.mem = parse_uint(v)?,
                "host-mem-os" => {
                    f.host.memtotal = match v {
                        "MemTotal" => true,
                        "-" => false,
                        _ => return None,
                    };
                }
                "driver-ceiling" => f.host.uncapped = ceiling_word(v)?,
                "measured-at" => f.measured_at = v.to_string(),
                // Older receipts captured this verdict against the original floors.
                "vanished" => {}
                "ambiguous" => {
                    let mut parts = v.split('\t');
                    let (Some(n), Some(a), Some(b), None) =
                        (parts.next(), parts.next(), parts.next(), parts.next())
                    else {
                        return None;
                    };
                    f.ambiguous.push((n.into(), a.into(), b.into()));
                }
                "crate" => {
                    let fields: Vec<&str> = v.split('\t').collect();
                    let (name, word) = (fields.first()?, fields.get(1)?);
                    let memory = MemRow::from_fields(fields.get(2..8)?)?;
                    let rest = &fields[8..];
                    let decision = match Decision::from_word(word)? {
                        Decision::Gate(_) => {
                            if !gates.insert(*name) {
                                return None;
                            }
                            let (m, n) = Measured::from_fields(rest, has_grade)?;
                            owed = n.unwrap_or(0);
                            Decision::Gate(m)
                        }
                        Decision::Memory(_) => Decision::Memory(Seen::from_fields(rest)?),
                        d if rest.is_empty() => d,
                        _ => return None,
                    };
                    f.crates.push(Crate {
                        name: (*name).to_string(),
                        decision,
                        memory,
                    });
                }
                _ => return None,
            }
        }
        if !ended || owed != 0 {
            return None;
        }
        f.provenance = has_provenance.then_some(p);
        Some(f)
    }
}

#[derive(Clone)]
struct Floor {
    proved: String,
    total: String,
    driver: String,
    why: String,
}

fn parse_floor_row(row: &str) -> Floor {
    let mut f = row.split('\t');
    let _ = f.next();
    let mut col = || {
        f.next()
            .map_or("", |s| s.trim_end_matches('\t'))
            .to_string()
    };
    Floor {
        proved: col(),
        total: col(),
        driver: col(),
        why: col(),
    }
}

fn floor_of(ratchet: Option<&str>, name: &str) -> Option<Floor> {
    let row = ratchet?
        .lines()
        .find(|l| l.split('\t').next() == Some(name))?;
    Some(parse_floor_row(row))
}

/// Each applicable crate has at most one row, with a proof count bounded by its total.
fn validate_floor_rows(
    text: &str,
    source: &str,
    uses: impl Fn(&str) -> bool,
) -> std::result::Result<(), String> {
    let mut seen = BTreeMap::new();
    for (line, row) in text.lines().enumerate() {
        if row.starts_with('#') || row.trim().is_empty() {
            continue;
        }
        let line = line + 1;
        let mut columns = row.split('\t');
        let name = columns.next().unwrap_or_default();
        if !uses(name) {
            continue;
        }
        if let Some(first) = seen.insert(name, line) {
            return Err(format!(
                "{source}: duplicate floor for {name} on lines {first} and {line}"
            ));
        }
        let mut count = |label| {
            let value = columns.next().unwrap_or_default();
            parse_uint::<u64>(value).ok_or_else(|| {
                format!("{source} line {line}: {name} {label} `{value}` is not a count")
            })
        };
        let proved = count("floor")?;
        let total = count("total")?;
        if proved > total {
            return Err(format!(
                "{source} line {line}: {name} floor {proved} exceeds total {total}"
            ));
        }
    }
    Ok(())
}

impl Facts {
    fn check_floor_rows(
        &self,
        ratchet: Option<&str>,
        source: &str,
        whole: bool,
    ) -> std::result::Result<(), String> {
        let selected = (!whole).then(|| {
            self.crates
                .iter()
                .map(|c| c.name.as_str())
                .collect::<BTreeSet<_>>()
        });
        validate_floor_rows(ratchet.unwrap_or_default(), source, |name| {
            selected.as_ref().is_none_or(|names| names.contains(name))
        })
    }
}

fn floors_index(ratchet: Option<&str>) -> BTreeMap<&str, Floor> {
    let mut out = BTreeMap::new();
    for row in ratchet.unwrap_or("").lines() {
        if let Some(name) = row.split('\t').next().filter(|n| !n.is_empty()) {
            out.entry(name).or_insert_with(|| parse_floor_row(row));
        }
    }
    out
}

fn refused_path_note(r: &PathRefusal, names: &[&str], me: u32) -> String {
    format!(
        "REFUSED PATH: {}: targo refused {} ({}), which says nothing about the code: {}",
        names.join(" "),
        r.path,
        r.said(me),
        r.remedy(me)
    )
}

/// Final verification verdict (`PASS`, `FAIL`, `COULD-NOT-RUN`, or `NARROWED`) from [`judge`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    pub report: String,
    pub tail: String,
    pub word: &'static str,
    pub exit: i32,
    pub total: String,
    pub not_verified: String,
    pub failed: Vec<String>,
    pub unverified: Vec<String>,
    pub memory: Vec<(String, String, Option<u64>)>,
    pub host: String,
    pub ratchet: Option<String>,
    /// The new text of [`BACKSTOPS`] when a floor was written.
    pub backstops: Option<String>,
    pub memory_rows: Vec<(String, MemRow)>,
    pub host_obs: Vec<(String, Option<[String; 5]>)>,
}

/// What the run that set a crate's floor stopped, as [`BACKSTOPS`] keeps it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FloorRun {
    /// The obligations a backstop stopped in it.
    pub stops: Vec<Backstop>,
    /// The proof checks the lane cancelled in it ([`Measured::cancelled`]).
    pub cancelled: u64,
}

/// Reads the stop history of the run that set this crate's floor.
/// A known baseline has one readable cancellation header and complete stop rows for the crate.
#[must_use]
pub fn floor_run(backstops: Option<&str>, name: &str) -> Option<FloorRun> {
    floor_run_lines(
        backstop_rows(backstops).filter_map(|(owner, rest)| (owner == name).then_some(rest)),
    )
}

fn backstop_rows(backstops: Option<&str>) -> impl Iterator<Item = (&str, &str)> {
    backstops
        .unwrap_or("")
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        // A truncated row can still identify its crate. Let the row parser see its empty fields.
        .map(|line| line.split_once('\t').unwrap_or((line, "")))
}

fn floor_run_lines<'a>(lines: impl Iterator<Item = &'a str>) -> Option<FloorRun> {
    let mut cancelled = None;
    let mut stops = Vec::new();
    for rest in lines {
        match rest.strip_prefix(RUN_ROW) {
            Some(n) if cancelled.is_none() => cancelled = Some(parse_uint(n)?),
            Some(_) => return None,
            None => stops.push(Backstop::from_fields(rest)?),
        }
    }
    cancelled.map(|cancelled| FloorRun { stops, cancelled })
}

fn backstops_index(backstops: Option<&str>) -> BTreeMap<&str, Vec<&str>> {
    let mut out: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (name, rest) in backstop_rows(backstops) {
        out.entry(name).or_default().push(rest);
    }
    out
}

/// `backstops` (the text of [`BACKSTOPS`]) with `name`'s rows replaced by `run`'s, its head row
/// first, sorted.
#[must_use]
pub fn backstops_rewritten(backstops: Option<&str>, name: &str, run: &FloorRun) -> String {
    let text = backstops.unwrap_or("");
    let mut out = String::new();
    for l in text.lines().take_while(|l| l.starts_with('#')) {
        out.push_str(l);
        out.push('\n');
    }
    if out.is_empty() {
        out = format!("{BACKSTOPS_HEADER}\n");
    }
    let name = one_line(name);
    let mut rows: Vec<String> = text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
        .filter(|l| l.split('\t').next() != Some(name.as_str()))
        .map(str::to_string)
        .chain([format!("{name}\t{RUN_ROW}{}", run.cancelled)])
        .chain(run.stops.iter().map(|b| format!("{name}\t{}", b.fields())))
        .collect();
    rows.sort_unstable();
    for r in rows {
        out.push_str(&r);
        out.push('\n');
    }
    out
}

/// What a starved run's shortfall may be put down to the machine for ([`credit`]): an
/// obligation a backstop stopped in it and not in the run that set its floor, and a proof check
/// the lane cancelled beyond that run's count. What the floor's run had too recurs at any load,
/// so it is the code's and earns nothing. When [`BACKSTOPS`] kept nothing of that run, every
/// stop and every cancelled check is credited, and none of them is known to be new.
struct Credit {
    /// The stops and the cancelled proof checks together.
    n: u64,
    /// This run's stops its floor's run did not have.
    stops: u64,
    /// Those stops, by name, when this run kept its obligation record.
    named: Option<Vec<Backstop>>,
    /// This run's stops its floor's run had too.
    recurring: u64,
    /// The proof checks the lane cancelled beyond its floor's run's count.
    cancelled: u64,
    /// The floor's run, as [`BACKSTOPS`] kept it.
    floor_run: Option<FloorRun>,
}

impl Credit {
    /// What it credits, in a note's words.
    fn said(&self) -> String {
        let mut parts = Vec::new();
        if self.stops > 0 {
            let new = if self.floor_run.is_some() {
                " its floor's run did not"
            } else {
                ""
            };
            parts.push(format!(
                "a backstop stopped {} obligation(s){new}",
                self.stops
            ));
        }
        if self.cancelled > 0 {
            parts.push(match &self.floor_run {
                Some(r) => format!(
                    "{} proof check(s) were cancelled beyond its floor's run's {}",
                    self.cancelled, r.cancelled
                ),
                None => format!("{} proof check(s) were cancelled", self.cancelled),
            });
        }
        let mut said = parts.join(" and ");
        if self.floor_run.is_none() {
            said.push_str(
                "; its floor's run kept no record of what it stopped, so whether these recur at any load is unknown",
            );
        }
        said
    }
}

fn credit(m: &Measured, floor_run: Option<FloorRun>) -> Credit {
    let (had, had_cancelled) = floor_run
        .as_ref()
        .map_or((&[][..], 0), |r| (r.stops.as_slice(), r.cancelled));
    let (stops, named, recurring) = match &m.backstops {
        Some(run) => {
            let mut owed = BTreeMap::<&str, usize>::new();
            for b in had {
                *owed.entry(b.key.as_str()).or_default() += 1;
            }
            let fresh: Vec<Backstop> = run
                .iter()
                .filter(|b| match owed.get_mut(b.key.as_str()) {
                    Some(n) if *n > 0 => {
                        *n -= 1;
                        false
                    }
                    _ => true,
                })
                .cloned()
                .collect();
            let recurring = run.len().saturating_sub(fresh.len()) as u64;
            (fresh.len() as u64, Some(fresh), recurring)
        }
        None => {
            let all = m.lapsed.saturating_add(m.rss);
            let stops = all.saturating_sub(had.len() as u64);
            (stops, None, all - stops)
        }
    };
    let cancelled = m.cancelled.saturating_sub(had_cancelled);
    Credit {
        n: stops.saturating_add(cancelled),
        stops,
        named,
        recurring,
        cancelled,
        floor_run,
    }
}

const LOST_SHOWN: usize = 12;

/// What a run lost that the machine cannot be credited with, and why the drop is the code's.
fn lost_note(name: &str, proved: u64, f: u64, c: &Credit, pace: Option<Pace>) -> String {
    let known = c.floor_run.is_some();
    let new = if known {
        " that its floor's run did not"
    } else {
        ""
    };
    let mut said = vec![match &c.named {
        None => format!(
            "a backstop stopped {} obligation(s){}, and the run kept no obligation record to name them",
            c.stops,
            if known {
                " beyond its floor's run's count"
            } else {
                ""
            }
        ),
        Some(v) if v.is_empty() => format!("a backstop stopped none{new}"),
        Some(v) => {
            let mut shown: Vec<String> = v
                .iter()
                .take(LOST_SHOWN)
                .map(|b| format!("{} [{}]", b.said, b.stop))
                .collect();
            if v.len() > LOST_SHOWN {
                shown.push(format!("and {} more", v.len() - LOST_SHOWN));
            }
            format!("a backstop stopped {}{new}: {}", v.len(), shown.join("; "))
        }
    }];
    if c.recurring > 0 {
        said.push(format!(
            "{} stop(s) its floor's run had too recur at any load",
            c.recurring
        ));
    }
    if !known {
        said.push(
            "its floor's run kept no record of what it stopped, so none of these is known to be new"
                .into(),
        );
    }
    let why = match pace {
        Some(p) if p.starved() => {
            said.push(match &c.floor_run {
                Some(r) => format!(
                    "{} proof check(s) were cancelled beyond its floor's run's {}",
                    c.cancelled, r.cancelled
                ),
                None => format!("{} proof check(s) were cancelled", c.cancelled),
            });
            format!(
                "the run was starved ({}), but all the machine can be credited with, {}, cannot cover the shortfall of {}, so the drop is the code's",
                p.said(),
                c.n,
                f.saturating_sub(proved)
            )
        }
        Some(p) => format!(
            "the run was not starved ({}), so the drop is the code's",
            p.said()
        ),
        None => "the run's pace was not recorded, so nothing is put down to the machine, and the drop is the code's".into(),
    };
    format!(
        "LOST: {name}: {proved} proved against its floor of {f}; {}; {why}",
        said.join("; ")
    )
}

fn measure_again(facts: &Facts, names: &[&str]) -> String {
    if facts.provenance.is_some() {
        return "targo --unverified ship trust run --remeasure".into();
    }
    format!(
        "targo --unverified ship trust run -p {}",
        names.join(" -p ")
    )
}

const fn trustd_probe_options() -> Option<&'static str> {
    if cfg!(target_os = "macos") {
        Some("-alx")
    } else if cfg!(target_os = "linux") {
        Some("-lx")
    } else {
        None
    }
}

fn trustd_remedy(verdict: &str, sock: &str, exe: &str) -> String {
    let never = "never signal it: a killed trustd records DIRTY and blocks every check until `--recover-after-crash` attests no solver survives";
    match verdict {
        "foreign" => {
            format!("re-run once it retires (it exits 300 s after its last grant); {never}")
        }
        "silent" => format!("re-run when the machine is quieter; {never}"),
        "dirty" => match trustd_probe_options() {
            Some(options) => format!(
                "trustd's crash epoch at {sock} is DIRTY, and targo starts no trustd until someone attests that no solver of the prior daemon survives: from a shell that sees every process on this host (pgrep sees only its own PID namespace, so never from inside a PID-namespaced sandbox), `pgrep {options} 'trustc|rustc|ay|trust-wp.*|targo.*'` checks the standard Trust process names. Only exit 1 means none of those names matched; an error is not a clear probe. Account for any other or renamed solvers this toolchain runs too. Once no prior solver survives, run `{} --recover-after-crash --confirm-no-solvers --socket {}` and re-run; while a process remains, wait for it to exit, and never kill another's build to get there. It is an attestation, never a way to take a live socket",
                shell_word(exe),
                shell_word(sock)
            ),
            None => format!(
                "trustd's crash epoch at {sock} is DIRTY; this host has no vetted process-listing recipe, so no recovery command is offered. Establish a complete host view and that no solver of the prior daemon survives before attesting; never signal trustd or kill another's build"
            ),
        },
        _ => "re-run once trustd admits it".into(),
    }
}

/// Ratchet update policy (`--update-ratchet`) passed to [`judge`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Update<'a> {
    pub on: bool,
    pub reason: Option<&'a str>,
}

impl Update<'_> {
    pub const NONE: Update<'static> = Update {
        on: false,
        reason: None,
    };
    pub const RAISE: Update<'static> = Update {
        on: true,
        reason: None,
    };
}

/// Judges measured [`Facts`] against `ratchet` floors and optional `--update-ratchet` rules;
/// `backstops` is the text of [`BACKSTOPS`], what each floor's run had stopped.
#[must_use]
pub fn judge(
    facts: &Facts,
    ratchet: Option<&str>,
    backstops: Option<&str>,
    ratchet_name: &str,
    memory_name: &str,
    update: Update<'_>,
) -> Verdict {
    judge_with_credit(
        facts,
        ratchet,
        backstops,
        (ratchet_name, memory_name, None),
        update,
        None,
        &[],
    )
}

/// Only checks in the current plan can be traced to this run's log directory.
fn check_log_hint(plan: Option<&Plan>, name: &str) -> String {
    if let Some(plan) = plan
        && plan.logdir.ends_with("logs")
        && let Some(root) = plan.logdir.parent()
        && let Some(spec) = plan.specs.get(name)
    {
        let root = if root.as_os_str().is_empty() {
            Path::new(".")
        } else {
            root
        };
        return format!(
            "read its log with `targo --unverified ship trust log {} --target-root {}`",
            shell_word(spec),
            shell_word(&root.to_string_lossy()),
        );
    }
    format!(
        "consult the {} log saved from the original run, if available. A later check can replace the copy in its target root",
        shell_word(name),
    )
}

const FAILED_SOURCE_LIMIT: usize = 4;

fn failed_source_note(
    name: &str,
    summary: &str,
    record: &crate::trust_oblig::Record,
) -> Option<String> {
    let summary = summary_line(summary)?;
    let expected = summary.failed?;
    if !summary.unmeasured()
        || expected == 0
        || summary.total != record.total()
        || summary.proved != record.proved()
    {
        return None;
    }
    let failed = record.rows.iter().filter(|r| r.status == "failed");
    let count = failed.clone().count();
    if count as u64 != expected {
        return None;
    }
    let mut note = format!("FAILED OBLIGATIONS: {name}");
    for row in failed.take(FAILED_SOURCE_LIMIT) {
        let function = diagnostic_excerpt(&row.function);
        if row.file == crate::trust_oblig::NO_LOCATION {
            let _ = write!(note, "\n  {function} (source location unavailable)");
        } else {
            let _ = write!(
                note,
                "\n  {function} at {}:{}",
                diagnostic_excerpt(&row.file),
                row.span
            );
        }
    }
    if count > FAILED_SOURCE_LIMIT {
        let _ = write!(note, "\n  and {} more", count - FAILED_SOURCE_LIMIT);
    }
    Some(note)
}

#[allow(clippy::too_many_lines)]
fn judge_with_credit(
    facts: &Facts,
    ratchet: Option<&str>,
    backstops: Option<&str>,
    (ratchet_name, memory_name, live_plan): (&str, &str, Option<&Plan>),
    update: Update<'_>,
    working_memory: Option<&BTreeMap<String, Vec<MemRow>>>,
    credit_failures: &[String],
) -> Verdict {
    if let Err(why) = facts.check_floor_rows(ratchet, ratchet_name, facts.only.is_empty()) {
        return Verdict {
            tail: format!("VERDICT: COULD NOT RUN — {why}"),
            word: "COULD-NOT-RUN",
            exit: 2,
            failed: vec![why],
            host: facts.host.desc(),
            total: "0/0".into(),
            not_verified: "none".into(),
            report: String::new(),
            unverified: Vec::new(),
            memory: Vec::new(),
            ratchet: None,
            backstops: None,
            memory_rows: Vec::new(),
            host_obs: Vec::new(),
        };
    }
    let driver_row = facts.driver_row();
    let host = facts.host;
    let today = facts.measured_at.get(..10).unwrap_or(&facts.measured_at);
    let sock = facts.trustd.as_str();
    let trustd_exe = Path::new(&facts.driver).parent().map_or_else(
        || "trustd".to_string(),
        |d| d.join("trustd").display().to_string(),
    );
    let mut rows: Vec<[String; 4]> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    let mut mem_notes: Vec<String> = Vec::new();
    let mut fail = false;
    let mut failed: Vec<String> = Vec::new();
    let dash = || "-".to_string();
    let narrowed = !facts.only.is_empty();
    if !narrowed {
        // A floors-only change reuses these measurements. Judge today's rows against
        // the full graph, including the libraries this particular retry did not run.
        let inventory = facts.crates.iter().map(|c| c.name.as_str());
        for v in crate::trust_lane::vanished(inventory, ratchet.unwrap_or_default()) {
            failed.push(format!("{v} FORK-VANISHED"));
            rows.push([v, "FORK-VANISHED".into(), dash(), dash()]);
            fail = true;
        }
    }
    for (n, a, b) in &facts.ambiguous {
        rows.push([
            n.clone(),
            format!("AMBIGUOUS-SPEC({a} vs {b})"),
            dash(),
            dash(),
        ]);
        failed.push(format!("{n} AMBIGUOUS-SPEC"));
        fail = true;
    }
    let (mut queued, mut gated) = (0usize, 0usize);
    let (mut queued_what, mut skipped_what) = (String::new(), String::new());
    let (mut mem_what, mut memory) = (String::new(), Vec::new());
    let (mut mem_ram, mut mem_ram_unknown) = (None::<u64>, false);
    // Those refused for a peak above this host's RAM, whose fitting RAM no ceiling says.
    let mut peak_n = 0usize;
    let mut note_ram = |sure: Option<u64>| match sure {
        None => mem_ram_unknown = true,
        Some(r) => mem_ram = Some(mem_ram.map_or(r, |m: u64| m.max(r))),
    };
    for c in &facts.crates {
        match &c.decision {
            Decision::Queued(_) => {
                queued += 1;
                let _ = write!(queued_what, " {}", c.name);
                let _ = write!(skipped_what, " {} (queued fork)", c.name);
            }
            Decision::Memory(seen) => {
                let _ = write!(mem_what, " {}", c.name);
                let _ = write!(skipped_what, " {} (memory)", c.name);
                let peak = seen.as_ref().and_then(|s| peak_refusal(&host, &c.name, s));
                let by_peak = peak.is_some();
                let r = peak
                    .or_else(|| memory_refusal(&host, &c.name, true, &c.memory, seen.as_ref()))
                    .unwrap_or_else(|| Refusal {
                        line: format!(
                            "{} not verified here: this host gives {} ({})",
                            c.name,
                            host.ceiling()
                                .map_or_else(|| "?".into(), |c| format!("{c} GiB")),
                            host.share()
                        ),
                        token: "?".into(),
                        sure: None,
                    });
                mem_notes.push(format!("MEMORY: {}", r.line));
                memory.push((c.name.clone(), r.token, r.sure));
                if by_peak {
                    peak_n += 1;
                } else {
                    note_ram(r.sure);
                }
            }
            Decision::Gate(_) => gated += 1,
            Decision::NoLib | Decision::Ambiguous => {}
        }
    }
    // Each crate has u64 counts. Their sum must still distinguish one open obligation.
    let (mut total_proved, mut total_all) = (0u128, 0u128);
    let mut capbound_n = 0usize;
    let mut unmeasured_n = 0usize;
    let (mut by_trustd, mut by_clock) = (Vec::<String>::new(), Vec::<String>::new());
    // Undecided within the prover's spread ([`spread`]), and those whose check trustd went
    // away under ([`Measured::trustd_lost`]).
    let (mut by_spread, mut by_lost) = (Vec::<String>::new(), Vec::<String>::new());
    // Checks targo-trust ended with its no-measurement exit code.
    let mut by_targo = Vec::<String>::new();
    // What a run of the crates in `by_clock` needs to be decided ([`Pace::unstarved`]).
    let mut clock_need: Option<String> = None;
    let (mut trustd_dirty, mut trustd_block) = (false, None::<(String, String)>);
    let initial_floors = floors_index(ratchet);
    let initial_backstops = backstops_index(backstops);
    let mut ratchet_now: Option<String> = ratchet.map(str::to_string);
    let mut backstops_now: Option<String> = backstops.map(str::to_string);
    let mut rewrote = false;
    // Whether `--reason` went into a row, and if not, what became of each library it was given
    // for: a reason that lowered nothing is said, never dropped.
    let (mut reason_kept, mut reason_fates) = (false, Vec::<(String, String)>::new());
    let mut memory_rows: Vec<(String, MemRow)> = Vec::new();
    let no_memory = MemRow::default();
    let mut host_obs: Vec<(String, Option<[String; 5]>)> = Vec::new();
    let mut refused_paths: BTreeMap<&PathRefusal, Vec<&str>> = BTreeMap::new();
    for c in &facts.crates {
        let name = c.name.as_str();
        let m = match &c.decision {
            Decision::Ambiguous => continue,
            Decision::NoLib => {
                rows.push([name.into(), "NO-LIB".into(), dash(), dash()]);
                continue;
            }
            Decision::Queued(s) => {
                rows.push([name.into(), format!("QUEUED({s})"), dash(), dash()]);
                continue;
            }
            Decision::Memory(_) => {
                rows.push([name.into(), "REFUSED(memory)".into(), dash(), dash()]);
                continue;
            }
            Decision::Gate(m) => m,
        };
        for r in &m.path_refused {
            refused_paths.entry(r).or_default().push(name);
        }
        if !m.wall_budget.is_empty() {
            let secs = parse_uint::<u64>(&m.wall_budget);
            let s = format!("OVER-BUDGET({}s)", m.wall_budget);
            failed.push(format!("{name} {s}"));
            rows.push([name.into(), s, dash(), dash()]);
            unmeasured_n += 1;
            let _ = write!(skipped_what, " {name} (over its wall budget)");
            notes.push(format!(
                "OVER BUDGET: {name}: the check was still verifying when its per-crate wall budget of {} ran out. Give it longer with --crate-budget <seconds>, or {}. Past its size cap the log keeps every line the judge reads and the check's last output",
                secs.map_or_else(
                    || "(unnamed)".to_string(),
                    |s| if s < 60 {
                        format!("{s} s")
                    } else {
                        format!("{s} s ({})", span(s))
                    }
                ),
                check_log_hint(live_plan, name),
            ));
            fail = true;
            continue;
        }
        if !m.launch.is_empty() {
            if m.launch == "dirty" {
                trustd_dirty = true;
            } else {
                match m.held.as_str() {
                    "foreign" | "silent" => {
                        trustd_block = Some((m.held.clone(), m.held_what.clone()));
                    }
                    "ours" => {
                        trustd_block = Some((
                            "silent".into(),
                            format!(
                                "{}, and still targo did not see it answer within the half second it gives",
                                m.held_what
                            ),
                        ));
                    }
                    _ => {}
                }
            }
            rows.push([name.into(), "NOT-VERIFIED(trustd)".into(), dash(), dash()]);
            by_trustd.push(name.to_string());
            let _ = write!(skipped_what, " {name} (trustd did not admit it)");
            let cause = match m.launch.as_str() {
                "dirty" => "trustd's crash epoch is DIRTY (its log carries the recovery command)",
                "owned" => {
                    "another trustd held the socket and did not prove itself this build's within targo's half second"
                }
                "unready" => "the trustd it started did not answer in time",
                _ => "no trustd of its build could serve it (its log says why)",
            };
            let held = match m.held.as_str() {
                "none" => "nothing",
                "ours" => "this build's trustd",
                "foreign" => "a trustd of another build",
                "silent" => "a trustd that did not answer in time",
                _ => "what no probe could tell",
            };
            notes.push(format!(
                "NOT VERIFIED: {name}: targo would not start its check — {cause}; the socket then held {held}. To see the check output, {}",
                check_log_hint(live_plan, name),
            ));
            continue;
        }
        // trustd went away while the check ran, and the check measured nothing: its transport
        // ended with trustd (trustc dies of SIGPIPE writing to it), which says nothing of the code.
        if !m.trustd_lost.is_empty() && (m.transport || !m.ended.is_empty() || m.summary.is_empty())
        {
            rows.push([name.into(), "NOT-VERIFIED(trustd)".into(), dash(), dash()]);
            by_lost.push(name.to_string());
            let _ = write!(skipped_what, " {name} (trustd went away)");
            let how = if m.ended.is_empty() {
                format!("it exited {} with nothing measured", m.rc)
            } else {
                format!("it exited {}: {}", m.rc, m.ended)
            };
            notes.push(format!(
                "NOT VERIFIED: {name}: trustd went away while its check ran, and {how} — the check lost its prover, which says nothing of the code; the socket then held: {}",
                m.trustd_lost
            ));
            continue;
        }
        // Exit 3 is the verifier's no-measurement result even when it printed counts.
        // A refused path keeps its failure and remedy below.
        if m.rc.trim() == "3" && m.path_refused.is_empty() {
            rows.push([name.into(), "NOT-VERIFIED(targo)".into(), dash(), dash()]);
            by_targo.push(name.to_string());
            let _ = write!(skipped_what, " {name} (targo-trust could not run it)");
            let mut note =
                format!("NOT VERIFIED: {name}: targo-trust exited 3 without a measurement");
            if !m.could_not_run.is_empty() {
                let _ = write!(note, ": {}", m.could_not_run);
            }
            let _ = write!(note, "; re-run it ({})", measure_again(facts, &[name]));
            notes.push(note);
            notes.push(format!(
                "CHECK LOG: {name}: {}",
                check_log_hint(live_plan, name)
            ));
            continue;
        }
        if m.transport {
            unmeasured_n += 1;
            let _ = write!(skipped_what, " {name} (transport failed)");
            let s = format!("NOT-VERIFIED(rc={})", m.rc);
            failed.push(format!("{name} {s}"));
            rows.push([name.into(), s, dash(), dash()]);
            fail = true;
            continue;
        }
        if !m.ended.is_empty() {
            let counted = match parse_summary(&m.summary) {
                (Some(p), Some(t)) => {
                    format!(
                        "its Summary, {p} proved of {t}, counts only what ran before it stopped"
                    )
                }
                _ if m.summary.is_empty() => "it left no Summary".to_string(),
                _ => "its Summary does not parse".to_string(),
            };
            let s = "NOT-VERIFIED(incomplete)".to_string();
            failed.push(format!("{name} {s}"));
            rows.push([name.into(), s, dash(), dash()]);
            unmeasured_n += 1;
            let _ = write!(skipped_what, " {name} (did not finish)");
            notes.push(format!(
                "NOT VERIFIED: {name}: its verification did not finish ({}) — {counted}, so nothing was decided about it; re-run it",
                m.ended
            ));
            fail = true;
            continue;
        }
        let (proved, all) = match m.floor_counts() {
            Ok(counts) => counts,
            Err(status) => {
                if status.starts_with("NOT-MEASURED(") {
                    let mut note = format!(
                        "NOT MEASURED: {name}: compiler diagnostic excluded from proof totals and floors — {}",
                        diagnostic_excerpt(m.summary.trim())
                    );
                    // Earlier v17 receipts may carry a driver refusal from a different
                    // report in this field. Only the final summary's counts belong here.
                    if m.could_not_run.starts_with("Stop causes: ")
                        || m.could_not_run.starts_with(FAILED_ROWS)
                    {
                        let _ = write!(note, " ({})", diagnostic_excerpt(&m.could_not_run));
                    }
                    notes.push(note);
                    if let Some(sources) = live_plan.and_then(|p| p.failed_sources.get(name)) {
                        notes.push(sources.clone());
                    }
                    notes.push(format!(
                        "CHECK LOG: {name}: {}",
                        check_log_hint(live_plan, name)
                    ));
                }
                failed.push(format!("{name} {status}"));
                let _ = write!(skipped_what, " {name} ({status})");
                rows.push([name.into(), status, dash(), dash()]);
                unmeasured_n += 1;
                fail = true;
                continue;
            }
        };
        let row = if rewrote {
            floor_of(ratchet_now.as_deref(), name)
        } else {
            initial_floors.get(name).cloned()
        };
        let floor: Option<u64> = match &row {
            None => None,
            Some(r) if r.proved.is_empty() => None,
            Some(r) => match parse_uint(&r.proved) {
                Some(n) => Some(n),
                None => {
                    failed.push(format!("{name} FLOOR-UNPARSED"));
                    rows.push([
                        name.into(),
                        "FLOOR-UNPARSED".into(),
                        proved.to_string(),
                        all.to_string(),
                    ]);
                    notes.push(format!(
                        "BAD FLOOR: {name}: its row in {ratchet_name} gives the floor `{}`, which is not a count — fix the row (the verify stage's up-only check names it too)",
                        r.proved
                    ));
                    fail = true;
                    continue;
                }
            },
        };
        let floor_driver = row.as_ref().map_or("", |r| r.driver.as_str());
        let floor_total = row.as_ref().map_or("", |r| r.total.as_str());
        let floor_why = row.as_ref().map_or("", |r| r.why.as_str());
        let own_floor = floor_driver.is_empty() || floor_driver == driver_row;
        // A floor removed from the ratchet can leave its companion backstop rows behind.
        let run_backstops = match floor {
            None => None,
            Some(_) if rewrote => floor_run(backstops_now.as_deref(), name),
            Some(_) => initial_backstops
                .get(name)
                .and_then(|lines| floor_run_lines(lines.iter().copied())),
        };
        let credit = credit(m, run_backstops);
        let starved = m.pace.filter(Pace::starved);
        // What the machine is credited with: nothing in a run that was not starved.
        let clock = if starved.is_some() { credit.n } else { 0 };
        // Why this run sets no floor, whatever it proved: a starved run's count stands only when
        // nothing in it is the machine's.
        let run_withhold = if let Some(p) = starved.filter(|_| credit.n > 0) {
            Some(format!(
                "the run was starved ({}), and {}, so a run that was not starved need not prove what it did; measure it again in a run with {}: {}",
                p.said(),
                credit.said(),
                p.unstarved(),
                measure_again(facts, &[name])
            ))
        } else if m.backstops.is_none() && m.lapsed.saturating_add(m.rss) > 0 {
            Some(format!(
                "a backstop stopped {} obligation(s), and the run kept no obligation record to name them beside its floor (the lane writes it beside the stored log <name@version>.log{} as <name@version>.log{}, or says why it could not in <name@version>.log.oblig.err); re-base it from a run that keeps one",
                m.lapsed.saturating_add(m.rss),
                STORED_SUFFIX,
                crate::trust_oblig::RECORD_SUFFIX,
            ))
        } else {
            None
        };
        let mem = &c.memory;
        // Admission and its verdict describe the measured run. An update is applied to
        // the working floor's current ceiling and the current row for this host's RAM.
        let current_mem = working_memory.map_or(mem, |rows| {
            rows.get(name)
                .and_then(|r| row_for(r, &host))
                .unwrap_or(&no_memory)
        });
        let cen = Census::of(m);
        let capbound = floor.is_some_and(|f| proved.saturating_add(credit.n) < f)
            && own_floor
            && cen.exhausted
            && mem.f != "-"
            && cen
                .ceil
                .as_deref()
                .is_some_and(|x| !gib_le(&mem.f, x) && !fits(mem, &host, x));
        if m.refused > 0 {
            let (refused, why) = (m.refused, &m.refused_why);
            match floor {
                Some(f) if proved >= f => notes.push(format!(
                    "ADMISSION REFUSED: {name}: {refused} obligation(s) refused memory admission ({why}), and still {proved} proved against its floor of {f} — the floor held; what was refused was not decided"
                )),
                None if !update.on => notes.push(format!(
                    "ADMISSION REFUSED: {name}: {refused} obligation(s) refused memory admission ({why}) — record its floor on a run trustd admits"
                )),
                Some(f) if proved.saturating_add(refused).saturating_add(clock) < f => {
                    let tail = if capbound {
                        ", in a run that ran out of its memory ceiling — CAP-BOUND"
                    } else {
                        " — the drop is the code's"
                    };
                    let and = match starved {
                        Some(p) => format!(
                            " and the {clock} its starved run ({}) can be credited with",
                            p.said()
                        ),
                        None => String::new(),
                    };
                    notes.push(format!(
                        "ADMISSION REFUSED: {name}: {refused} obligation(s) refused memory admission ({why}){and} cannot cover {proved} proved against its floor of {f}{tail}"
                    ));
                }
                _ => {
                    let against = floor
                        .map_or_else(|| "no floor".to_string(), |f| format!("its floor of {f}"));
                    rows.push([name.into(), "NOT-VERIFIED(trustd)".into(), dash(), dash()]);
                    by_trustd.push(name.to_string());
                    let _ = write!(skipped_what, " {name} (trustd did not admit it)");
                    notes.push(format!(
                        "NOT VERIFIED: {name}: {refused} obligation(s) refused memory admission ({why}) left {proved} proved against {against} — the shortfall is the machine's, so nothing was decided; measure it again once trustd admits its work: {}",
                        measure_again(facts, &[name])
                    ));
                    continue;
                }
            }
        }
        if let (true, Some(f), Some(x)) = (capbound, floor, cen.ceil.as_deref()) {
            rows.push([name.into(), format!("CAP-BOUND(<{f})"), dash(), dash()]);
            capbound_n += 1;
            let _ = write!(mem_what, " {name}");
            let _ = write!(skipped_what, " {name} (cap-bound)");
            let low = gib_max(cen.peak.as_deref().unwrap_or("0"), x);
            let sure = gib(&mem.f).map(ram_for);
            let may = (!gib_le(&mem.f, &low)).then(|| ram_above(gib(&low).unwrap_or(0.0)));
            mem_notes.push(format!(
                "MEMORY: {name} cap-bound: ran out at its {x} GiB ceiling ({} GiB RAM / 8): {proved} proved vs floor {f}, set at {} GiB; floor kept, refused up front next run; {}",
                ram_for(gib(x).unwrap_or(0.0)),
                mem.f,
                host.ram_phrase(sure, may)
            ));
            memory.push((name.to_string(), format!(">{low}"), sure));
            note_ram(sure);
            host_obs.push((
                name.into(),
                Some([
                    "cap-bound".into(),
                    x.to_string(),
                    cen.peak.clone().unwrap_or_else(dash),
                    proved.to_string(),
                    f.to_string(),
                ]),
            ));
            if update.on
                && let Some(r) = mem_learn(current_mem, &current_mem.f, &cen, today, &host)
            {
                memory_rows.push((name.into(), r));
            }
            continue;
        }
        host_obs.push((
            name.into(),
            match (cen.exhausted, floor) {
                (true, Some(f)) if proved >= f => Some([
                    "held".into(),
                    cen.ceil.clone().unwrap_or_else(dash),
                    cen.peak.clone().unwrap_or_else(dash),
                    proved.to_string(),
                    f.to_string(),
                ]),
                _ => None,
            },
        ));
        // What the prover's own run-to-run spread can take from a floor it recorded itself (a
        // floor another prover recorded compares provers, not runs).
        let allowance = floor.filter(|_| own_floor).map_or(0, spread);
        if let Some(f) = floor.filter(|f| {
            proved < *f && clock > 0 && proved.saturating_add(clock).saturating_add(allowance) >= *f
        }) && let Some(pace) = starved
        {
            rows.push([
                name.into(),
                format!("NOT-VERIFIED(clock,<{f},{clock})"),
                dash(),
                dash(),
            ]);
            by_clock.push(name.to_string());
            clock_need.get_or_insert_with(|| pace.unstarved());
            let _ = write!(skipped_what, " {name} (starved)");
            let rest = if proved.saturating_add(clock) < f {
                format!(
                    ", and the {} it leaves is within the {allowance} runs of one tree under this prover differ by",
                    f - proved - clock
                )
            } else {
                String::new()
            };
            notes.push(format!(
                "BUDGET LAPSED: {name}: {proved} proved, floor {f}; in a starved run ({}), {}{rest} — undecided; a run with {} decides it ({}); {WALL_CLOCKS}",
                pace.said(),
                credit.said(),
                pace.unstarved(),
                measure_again(facts, &[name])
            ));
            continue;
        }
        // A run that names no obligation it lost and falls short of its own prover's floor by
        // no more than that prover's spread cannot tell the spread from a lost proof: undecided,
        // never a regression. It names one when a backstop stopped what its floor's run did not,
        // or the lane cancelled more proof checks than there; where that run was not kept, no
        // stop can be called new, so it names none.
        if let Some(f) = floor.filter(|f| proved < *f && proved.saturating_add(allowance) >= *f)
            && (credit.n == 0 || credit.floor_run.is_none())
        {
            let short = f - proved;
            rows.push([
                name.into(),
                format!("NOT-VERIFIED(spread,<{f},{short})"),
                dash(),
                dash(),
            ]);
            by_spread.push(name.to_string());
            let _ = write!(skipped_what, " {name} (within the prover's spread)");
            let names = if credit.floor_run.is_some() {
                "no backstop stop its floor's run did not have, no proof check cancelled beyond that run's count"
            } else {
                "what its floor's run stopped was not kept, so no stop can be called new"
            };
            notes.push(format!(
                "WITHIN SPREAD: {name}: {proved} proved, floor {f}, {short} short: no more than the {allowance} ({SPREAD_PER_CENT}% of the floor) that runs of one tree under this prover differ by, and the run names no obligation it lost ({names}) — undecided; a run that meets the floor decides it ({})",
                measure_again(facts, &[name])
            ));
            continue;
        }
        total_proved += u128::from(proved);
        total_all += u128::from(all);
        let mut status = if proved == all {
            "CLEAN".to_string()
        } else {
            "open".to_string()
        };
        let mut bad = false;
        match floor {
            None if update.on && run_withhold.is_none() => status = "FLOOR-RECORDED".into(),
            None => {
                status = "NO-FLOOR".into();
                bad = true;
                // With --update-ratchet on, its RE-BASE WITHHELD note says why none was recorded.
                if !update.on {
                    notes.push(format!(
                        "NO FLOOR: {name}: gated with no floor in {ratchet_name} — record it: targo --unverified ship trust run -p {name} --update-ratchet, and commit the row"
                    ));
                }
            }
            Some(f) if proved < f => {
                status = format!("RATCHET-REGRESSION(<{f})");
                bad = true;
                if credit.stops.saturating_add(credit.recurring) > 0 || clock > 0 {
                    notes.push(lost_note(name, proved, f, &credit, m.pace));
                }
                // Under a floor of 50 the allowance rounds down to 0: no spread to be beyond.
                if own_floor && allowance > 0 && f - proved > allowance {
                    notes.push(format!(
                        "BEYOND SPREAD: {name}: {proved} proved against its floor of {f}, {} short: more than the {allowance} ({SPREAD_PER_CENT}% of the floor) that runs of one tree under this prover differ by",
                        f - proved
                    ));
                }
                if !own_floor {
                    notes.push(format!(
                        "DRIVER CHANGED: {name}: its floor of {f} was recorded under {floor_driver}; this run's prover is {driver_row}, and proved {proved}: fix what it no longer proves, or lower the row onto it with a reason (`targo --unverified ship trust run -p {name} --update-ratchet --reason '<why>'`, or `ship trust judge --update-ratchet --reason '<why>'` over a whole-tree run's facts)"
                    ));
                }
                let skipped = m.skipped.unwrap_or(0);
                let exhausted = m.budget_rows.saturating_add(skipped);
                if exhausted > 0 {
                    let skipped_note = if skipped > 0 {
                        format!(
                            "; {skipped} of them were bodies skipped at the ceiling, never verified"
                        )
                    } else {
                        String::new()
                    };
                    let ceiling = cen.ceil.as_deref().map_or_else(
                        || "not named in its log".to_string(),
                        |x| format!("{x} GiB"),
                    );
                    let run_c = cen.ceil.as_deref();
                    let under =
                        !cen.exhausted && !cen.atceil && cen.peak.is_some() && cen.ceil.is_some();
                    let mem_why = if mem.f == "-" {
                        format!(
                            "{memory_name} records no ceiling for its floor, so the drop cannot be called the host's; it still fails; record it on a host that holds the floor: targo --unverified ship trust run -p {name} --update-ratchet"
                        )
                    } else if !own_floor {
                        "its floor is another prover's, and a new prover's shortfall is not called the host's; it still fails".to_string()
                    } else if run_c.is_some_and(|x| gib_le(&mem.f, x)) {
                        format!(
                            "its floor was set at {} GiB, no more than this run's, so the drop is the code's",
                            mem.f
                        )
                    } else if let Some(x) = run_c.filter(|x| cen.exhausted && fits(mem, &host, x)) {
                        let on = if Reading::of(mem, &host) == Reading::Here {
                            "this host's RAM".to_string()
                        } else {
                            format!("{} GiB of RAM", mem.ram)
                        };
                        format!(
                            "{memory_name} records its need as ~{} GiB, measured with room on {on}, which fits this run's {x} GiB: it needed more than its record says, so the drop is the code's",
                            mem.need
                        )
                    } else if cen.exhausted {
                        format!(
                            "its floor was set at {} GiB, and this run's log names no ceiling to compare it with, so the drop cannot be called the host's; it still fails",
                            mem.f
                        )
                    } else if under {
                        "the drop is not the host's".to_string()
                    } else {
                        format!(
                            "its floor was set at {} GiB, and this run's log cannot show whether it ran out of memory, so the drop cannot be called the host's; it still fails",
                            mem.f
                        )
                    };
                    let what = if cen.exhausted {
                        format!(
                            "{exhausted} function(s) ran out of the verifier's memory ceiling ({ceiling}{skipped_note})"
                        )
                    } else if under {
                        format!(
                            "{exhausted} function(s) hit the verifier's budget (time or steps; peak {} under its {} ceiling)",
                            m.peak, m.ceiling
                        )
                    } else {
                        format!(
                            "{exhausted} function(s) hit the verifier's budget (ceiling {ceiling})"
                        )
                    };
                    notes.push(format!(
                        "MEMORY CEILING: {name}: {proved} proved against a floor of {f}; {what} — {mem_why}"
                    ));
                }
            }
            Some(_) => {}
        }
        if !floor_why.is_empty() && !floor_total.is_empty() && floor_driver == driver_row {
            match parse_uint::<u64>(floor_total) {
                Some(t) if all > t => {
                    if status == "open" || status == "CLEAN" {
                        status = format!("TOTAL-ABOVE-ROW(>{t})");
                    }
                    bad = true;
                    let set = floor.map_or_else(String::new, |f| format!("floor {f}, "));
                    let todo = if floor.is_some_and(|f| proved < f) {
                        format!(
                            " and proves {proved}, below the floor — fix the regression, or edit the row by hand (floor, total {all}, and the reason in its fifth column)"
                        )
                    } else {
                        format!(
                            " — re-measure it: targo --unverified ship trust run -p {name} --update-ratchet, and commit the row"
                        )
                    };
                    notes.push(format!(
                        "HAND-LOWERED: {name}: its row sets {set}total {t}; this run measures {all} obligations{todo}"
                    ));
                }
                Some(_) => {}
                None => {
                    status = "FLOOR-UNPARSED".into();
                    bad = true;
                    notes.push(format!(
                        "BAD FLOOR: {name}: its row in {ratchet_name} gives the total `{floor_total}`, which is not a count — fix the row"
                    ));
                }
            }
        }
        if bad {
            fail = true;
            failed.push(format!("{name} {status} {proved}/{all}"));
        }
        rows.push([name.into(), status, proved.to_string(), all.to_string()]);
        if update.on {
            let mut nf = current_mem.f.clone();
            let run_c = cen.ceil.clone();
            let lowers = floor.is_some_and(|f| proved < f);
            let reason = update.reason.filter(|_| lowers && !own_floor);
            let mut fate = if reason.is_some() {
                None
            } else if lowers {
                Some(format!(
                    "{proved} proved, below its floor of {}, which this prover recorded: a reason lowers only another prover's floor",
                    floor.unwrap_or(0)
                ))
            } else {
                Some(floor.map_or_else(
                    || format!("{proved} proved, its first floor, which needs no reason"),
                    |f| format!("{proved} proved, meeting its floor of {f}"),
                ))
            };
            if lowers && reason.is_none() {
                if !own_floor {
                    notes.push(format!(
                        "NOT LOWERED: {name}: {proved} proved stays below the floor of {} another prover recorded — --update-ratchet lowers no floor; it goes down onto this prover only with its reason: `targo --unverified ship trust run -p {name} --update-ratchet --reason '<why>'` (or `ship trust judge --update-ratchet --reason '<why>'` over a whole-tree run's facts)",
                        floor.unwrap_or(0)
                    ));
                }
            } else {
                let withhold = run_withhold.or_else(|| match (floor, run_c.as_deref()) {
                    (Some(f), _) if proved >= f || current_mem.f == "-" => None,
                    (None, _) => None,
                    // A prover whose trustc takes no ceiling ran out of none: the floor's
                    // ceiling cannot explain what it proves.
                    (Some(_), None) if host.uncapped => None,
                    (Some(_), None) => Some(format!(
                        "this run's log names no verifier ceiling (a seal from trust main has none), and its floor was set at {} GiB; re-base it from a run whose log names its ceiling",
                            current_mem.f
                    )),
                    (Some(_), Some(rc))
                        if !gib_le(&current_mem.f, rc)
                            && (cen.peak.is_none()
                                || cen.skipped.is_some_and(|n| n > 0)
                                || cen.budget > 0
                                || cen.exhausted) =>
                    {
                        let how = if cen.exhausted {
                            "this run ran out of it"
                        } else {
                            "nothing in its census shows it had room"
                        };
                        Some(format!(
                            "this host's {rc} GiB ceiling is below the {} GiB its floor was set at, and {how}; re-base it on a host with {} GiB",
                            current_mem.f,
                            ram_for(gib(&current_mem.f).unwrap_or(0.0))
                        ))
                    }
                    (Some(_), Some(_)) => None,
                });
                if let Some(w) = withhold {
                    notes.push(format!("RE-BASE WITHHELD: {name}: {w}"));
                    fate = Some(format!(
                        "{proved} proved, its re-base withheld (RE-BASE WITHHELD)"
                    ));
                } else {
                    reason_kept |= reason.is_some();
                    let mut lines: Vec<String> = ratchet_now
                        .as_deref()
                        .unwrap_or("")
                        .lines()
                        .filter(|l| l.split('\t').next() != Some(name))
                        .map(str::to_string)
                        .collect();
                    lines.push(match reason {
                        Some(why) => {
                            format!(
                                "{name}\t{proved}\t{all}\t{driver_row}\t{}",
                                one_line(why.trim())
                            )
                        }
                        None => format!("{name}\t{proved}\t{all}\t{driver_row}"),
                    });
                    lines.sort_unstable();
                    let mut text = lines.join("\n");
                    text.push('\n');
                    ratchet_now = Some(text);
                    // A run that kept no record had no stops, or its floor was withheld above.
                    backstops_now = Some(backstops_rewritten(
                        backstops_now.as_deref(),
                        name,
                        &FloorRun {
                            stops: m.backstops.clone().unwrap_or_default(),
                            cancelled: m.cancelled,
                        },
                    ));
                    rewrote = true;
                    let same = floor == Some(proved) && own_floor;
                    nf = match run_c {
                        Some(rc) if same && current_mem.f != "-" => gib_min(&current_mem.f, &rc),
                        Some(rc) => rc,
                        None if same => current_mem.f.clone(),
                        None => dash(),
                    };
                }
            }
            if let Some(r) = mem_learn(current_mem, &nf, &cen, today, &host) {
                memory_rows.push((name.into(), r));
            }
            if let Some(f) = fate.filter(|_| update.reason.is_some()) {
                reason_fates.push((name.into(), f));
            }
        }
    }
    if update.on && update.reason.is_some() && !reason_kept {
        let fates: Vec<String> = rows
            .iter()
            .map(|r| {
                reason_fates.iter().find(|(n, _)| *n == r[0]).map_or_else(
                    || format!("{}: {}, nothing to lower", r[0], r[1]),
                    |(n, f)| format!("{n}: {f}"),
                )
            })
            .collect();
        notes.push(format!(
            "REASON UNUSED: --reason lowered no floor, so no row keeps it: {}",
            fates.join("; ")
        ));
    }

    let unverified: Vec<String> = by_trustd
        .iter()
        .chain(&by_lost)
        .chain(&by_targo)
        .chain(&by_clock)
        .chain(&by_spread)
        .cloned()
        .collect();
    let me = aterm_uds::peer::our_uid();
    let refused_notes: Vec<String> = refused_paths
        .iter()
        .map(|(r, names)| refused_path_note(r, names, me))
        .collect();
    let memory_n = mem_what.split_whitespace().count();
    let gated_n = gated.saturating_sub(capbound_n + unverified.len() + unmeasured_n);
    let mut out = String::from("\n");
    let _ = writeln!(
        out,
        "{:<28} {:<26} {:>8} {:>8}",
        "CRATE", "STATUS", "PROVED", "TOTAL"
    );
    for r in &rows {
        let _ = writeln!(out, "{:<28} {:<26} {:>8} {:>8}", r[0], r[1], r[2], r[3]);
    }
    out.push('\n');
    let _ = writeln!(out, "TOTAL: {total_proved}/{total_all} proved");
    let id = if facts.driver_id.is_empty() {
        "no --version answer"
    } else {
        facts.driver_id.as_str()
    };
    let _ = writeln!(out, "DRIVER: {} ({id})", facts.driver);
    match host.ceiling() {
        Some(c) => {
            let _ = writeln!(
                out,
                "HOST: {} GiB RAM, verifier ceiling {c} GiB per library (targo trust: RAM / 8)",
                host.ram_gib()
            );
        }
        None if host.mem > 0 => {
            let _ = writeln!(
                out,
                "HOST: {} GiB RAM, no verifier ceiling (this prover's trustc takes none): refused up front only for a recorded peak whose claim is not below what it can ever have available",
                host.ram_gib()
            );
        }
        None => out.push_str("HOST: memory unreadable, nothing refused up front\n"),
    }
    if gated > 0 {
        match facts.grants.as_str() {
            "1" => {
                let _ = writeln!(out, "TRUSTD: held by this run's keeper ({sock})");
            }
            "" if !facts.trustd_seen.is_empty() => {
                let what = facts
                    .trustd_seen
                    .split_once('\t')
                    .map_or(facts.trustd_seen.as_str(), |(_, w)| w);
                let _ = writeln!(
                    out,
                    "TRUSTD: never held — the keeper holds only this build's trustd, and {what}"
                );
            }
            "" => {
                let _ = writeln!(
                    out,
                    "TRUSTD: never held — no trustd granted the keeper's request at {sock}"
                );
            }
            n => {
                let _ = writeln!(
                    out,
                    "TRUSTD: held by this run's keeper, {n} grants — trustd went away under a live grant and came back ({sock})"
                );
            }
        }
        let lost: Vec<&str> = facts
            .crates
            .iter()
            .filter(|c| matches!(&c.decision, Decision::Gate(m) if !m.trustd_lost.is_empty()))
            .map(|c| c.name.as_str())
            .collect();
        if !lost.is_empty() {
            let _ = writeln!(
                out,
                "TRUSTD: went away under this run's keeper while these ran: {}",
                lost.join(" ")
            );
        }
    }
    for n in refused_notes.iter().chain(&mem_notes).chain(&notes) {
        out.push_str(n);
        out.push('\n');
    }
    let skipped_shown = if skipped_what.is_empty() {
        " none"
    } else {
        skipped_what.as_str()
    };
    let _ = writeln!(
        out,
        "GATED: {gated_n} of {} selected; not verified:{skipped_shown}",
        facts.crates.len()
    );
    if queued > 0 {
        let _ = writeln!(
            out,
            "SCOPE: {queued} vendored fork(s) QUEUED, not gated, nothing claimed:{queued_what}"
        );
    }
    // A partial measurement cannot decide a calculus regression on its own.
    let credit_regressed =
        !credit_failures.is_empty() && (fail || (unverified.is_empty() && memory_n == 0));
    if credit_regressed {
        failed.extend_from_slice(credit_failures);
        fail = true;
    }
    let (word, exit, mut tail) = if fail {
        ("FAIL", 1, format!("VERDICT: FAIL — {}", failed.join("; ")))
    } else if !unverified.is_empty() {
        let mut said = Vec::new();
        if !by_trustd.is_empty() {
            let why = if trustd_dirty {
                trustd_remedy("dirty", sock, &trustd_exe)
            } else if let Some((verdict, what)) = &trustd_block {
                format!("{what}: {}", trustd_remedy(verdict, sock, &trustd_exe))
            } else {
                trustd_remedy("", sock, &trustd_exe)
            };
            said.push(format!(
                "trustd did not admit the work, so nothing was decided about: {}; {why}",
                by_trustd.join(" ")
            ));
        }
        if !by_lost.is_empty() {
            said.push(format!(
                "trustd went away while they ran, so nothing was decided about: {}; re-run them, and if targo then refuses for a DIRTY crash epoch, that refusal's log carries the recovery command (never signal a trustd)",
                by_lost.join(" ")
            ));
        }
        if !by_targo.is_empty() {
            said.push(format!(
                "targo-trust could not run them to a measurement (its exit 3), so nothing was decided about: {}; re-run them ({})",
                by_targo.join(" "),
                measure_again(
                    facts,
                    &by_targo.iter().map(String::as_str).collect::<Vec<_>>()
                )
            ));
        }
        if !by_clock.is_empty() {
            said.push(format!(
                "in a starved run, what the machine can be credited with (each BUDGET LAPSED note says what) could cover the shortfall, so nothing was decided about: {}; a run with {} decides them ({}); while their threads wait for a core, behind the host's work or the prover's own other threads, only a prover whose clocks count CPU time does (each BUDGET LAPSED note says why)",
                by_clock.join(" "),
                clock_need.as_deref().unwrap_or_default(),
                measure_again(facts, &by_clock.iter().map(String::as_str).collect::<Vec<_>>())
            ));
        }
        if !by_spread.is_empty() {
            said.push(format!(
                "within the prover's run-to-run spread of their floors ({SPREAD_PER_CENT}%), with no obligation named lost, so nothing was decided about: {}; a run that meets its floor decides them ({})",
                by_spread.join(" "),
                measure_again(
                    facts,
                    &by_spread.iter().map(String::as_str).collect::<Vec<_>>()
                )
            ));
        }
        (
            "COULD-NOT-RUN",
            2,
            format!("VERDICT: COULD NOT RUN — {}", said.join("; and ")),
        )
    } else if memory_n > 0 {
        let ceiling_n = memory_n.saturating_sub(peak_n);
        let by_ceiling = match (mem_ram, mem_ram_unknown) {
            _ if ceiling_n == 0 => None,
            (None, _) if ceiling_n == 1 => {
                Some("no RAM is known to verify it (its floor records no ceiling)".to_string())
            }
            (None, _) => Some(
                "no RAM is known to verify them (no floor of theirs records its ceiling)"
                    .to_string(),
            ),
            (Some(r), true) => Some(format!(
                "{r} GiB of RAM verifies those whose floor records its ceiling"
            )),
            (Some(r), false) if ceiling_n == 1 => Some(format!("{r} GiB of RAM verifies it")),
            (Some(r), false) => Some(format!("{r} GiB of RAM verifies them all")),
        };
        let by_peak = match peak_n {
            0 => None,
            1 if ceiling_n == 0 => Some(
                "it needs a host with more RAM than its recorded peak and its margin".to_string(),
            ),
            1 => Some(
                "the one refused for its peak needs a host with more RAM than that peak and its margin"
                    .to_string(),
            ),
            _ => Some(
                "each refused for its peak needs a host with more RAM than that peak and its margin"
                    .to_string(),
            ),
        };
        let ram_all = [by_ceiling, by_peak]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("; ");
        let fit = if gated_n > 0 {
            "; nothing that fit failed"
        } else {
            ""
        };
        (
            "NARROWED",
            4,
            format!(
                "VERDICT: NARROWED BY MEMORY — not verified on this host ({}):{mem_what}{fit}; {ram_all}",
                host.desc()
            ),
        )
    } else if total_all > 0 && total_proved == total_all {
        // The public judge may still refuse a lowered working floor. It emits the tail
        // only once that check and any floor writes have succeeded.
        let mut claim = if narrowed || queued > 0 {
            "VERDICT: 100% OF COUNTED OBLIGATIONS PROVED (partial run)".to_string()
        } else {
            "VERDICT: 100% MACHINE-PROVED (every workspace library + every vendored fork, advisory lane: every obligation proved)".to_string()
        };
        if narrowed {
            let _ = write!(claim, "\n     - -p restricted the run to: {}", facts.only);
        }
        ("PASS", 0, claim)
    } else {
        ("PASS", 0, String::new())
    };
    if !tail.is_empty() && !refused_paths.is_empty() {
        let said: Vec<String> = refused_paths
            .keys()
            .map(|r| {
                format!(
                    "targo refused {} ({}): {}",
                    r.path,
                    r.said(me),
                    r.remedy(me)
                )
            })
            .collect();
        let _ = write!(tail, " — REFUSED PATH: {}", said.join("; "));
    }
    let none_if_empty = |s: &str| {
        let s = s.trim();
        if s.is_empty() {
            "none".to_string()
        } else {
            s.to_string()
        }
    };
    Verdict {
        report: out,
        tail,
        word,
        exit,
        total: format!("{total_proved}/{total_all}"),
        not_verified: none_if_empty(&skipped_what),
        failed,
        unverified,
        memory,
        host: host.desc(),
        ratchet: rewrote.then_some(ratchet_now).flatten(),
        backstops: rewrote.then_some(backstops_now).flatten(),
        memory_rows,
        host_obs,
    }
}

/// Returns any crate floor in `after` that is lower than in `before`, or any removed crate.
#[must_use]
pub fn floors_lowered(
    measured: &str,
    judged: &str,
    gated: &dyn Fn(&str) -> bool,
    driver: Option<&str>,
) -> Vec<String> {
    struct Row<'a> {
        name: &'a str,
        proved: &'a str,
        total: &'a str,
        prover: &'a str,
        why: &'a str,
    }
    fn rows(text: &str) -> Vec<Row<'_>> {
        text.lines()
            .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
            .filter_map(|l| {
                let mut f = l.split('\t');
                let name = f.next()?;
                Some(Row {
                    name,
                    proved: f.next().unwrap_or(""),
                    total: f.next().unwrap_or(""),
                    prover: f.next().unwrap_or(""),
                    why: f.next().unwrap_or("").trim(),
                })
            })
            .collect()
    }
    let driver_shown = driver.unwrap_or("its prover gave no `--version` line");
    let (was_rows, now) = (rows(measured), rows(judged));
    let mut out = Vec::new();
    for r in &now {
        if parse_uint::<u64>(r.proved).is_none() {
            out.push(format!("{}: floor `{}` is not a count", r.name, r.proved));
        }
        if parse_uint::<u64>(r.total).is_none() {
            out.push(format!("{}: total `{}` is not a count", r.name, r.total));
        }
        if gated(r.name) && !was_rows.iter().any(|w| w.name == r.name) && driver != Some(r.prover) {
            out.push(format!(
                "{}: a new floor {} under `{}`, which is not the prover that measured the facts \
                 ({driver_shown})",
                r.name, r.proved, r.prover
            ));
        }
    }
    for was in &was_rows {
        let Some(was_n) = parse_uint::<u64>(was.proved) else {
            continue;
        };
        let Some(is) = now.iter().find(|r| r.name == was.name) else {
            if gated(was.name) {
                out.push(format!(
                    "{}: floor {was_n} removed while the facts still verify it",
                    was.name
                ));
            }
            continue;
        };
        let Some(is_n) = parse_uint::<u64>(is.proved) else {
            continue;
        };
        if is.prover != was.prover {
            if driver != Some(is.prover) {
                out.push(format!(
                    "{}: floor {was_n} -> {is_n} re-based onto `{}`, which is not the prover \
                     that measured the facts ({driver_shown})",
                    was.name, is.prover
                ));
            } else if is_n < was_n && (is.why.is_empty() || is.why == was.why) {
                out.push(format!(
                    "{}: floor {was_n} -> {is_n} re-based onto the measuring prover with no new \
                     reason in the row's fifth column: a new prover meets the floors it \
                     inherits, or its row says why it does not",
                    was.name
                ));
            }
            continue;
        }
        if is_n >= was_n {
            continue;
        }
        if is.why.is_empty() || is.why == was.why {
            out.push(format!(
                "{}: floor {was_n} -> {is_n} under the same prover ({}) and no new reason in the \
                 row's fifth column",
                was.name, was.prover
            ));
            continue;
        }
        let gone = match (parse_uint::<u64>(was.total), parse_uint::<u64>(is.total)) {
            (Some(w), Some(i)) => Some(w.saturating_sub(i)),
            _ => None,
        };
        if gone.is_none_or(|g| was_n - is_n > g) {
            out.push(format!(
                "{}: floor {was_n} -> {is_n} under the same prover ({}), but the total went {} -> \
                 {}: a reason covers only proved obligations that left with their code",
                was.name, was.prover, was.total, is.total
            ));
        }
    }
    out
}

/// Computes the SHA-256 digest of `rev`'s git tree excluding the floor files (ratchet, memory,
/// costs and backstops).
pub fn tree_sans_floors(git: &dyn GitRunner, rev: &str) -> Result<String> {
    tree_inputs(git, rev, |_| {})
}

fn tree_inputs(git: &dyn GitRunner, rev: &str, mut regular: impl FnMut(&Path)) -> Result<String> {
    use std::os::unix::ffi::OsStrExt as _;

    let out = git_original_ok(
        git,
        &["ls-tree", "-r", "-z", "--full-tree", rev],
        aterm_uds::deadline::MAX_COMPLETE_STDOUT_BYTES,
    )?;
    let mut h = Sha256::new();
    let mut entries = 0usize;
    for entry in out.stdout.split(|b| *b == 0).filter(|e| !e.is_empty()) {
        let tab = entry
            .iter()
            .position(|&b| b == b'\t')
            .ok_or_else(|| Error::new(format!("git listed an invalid tree entry for {rev}")))?;
        let (mode_and_oid, path) = (&entry[..tab], &entry[tab + 1..]);
        if FILES_OUTSIDE_TREE_KEY.iter().any(|f| path == f.as_bytes()) {
            continue;
        }
        h.update(entry);
        h.update([0u8]);
        if mode_and_oid.starts_with(b"100644 blob ") || mode_and_oid.starts_with(b"100755 blob ") {
            regular(Path::new(std::ffi::OsStr::from_bytes(path)));
        }
        entries += 1;
    }
    if entries == 0 {
        return Err(Error::new(format!("git lists no file in {rev}'s tree")));
    }
    Ok(hex(&h.finalize()))
}

/// Computes the content-addressed prover identity (`tree_root` SHA-256 and store root) for `bin`.
pub fn prover_identity(bin: &Path) -> std::result::Result<(String, PathBuf), String> {
    for tool in ["targo", "trustc"] {
        if !bin.join(tool).is_file() {
            return Err(format!(
                "{} holds no {tool} (a prover is a toolchain with targo and trustc side by side)",
                bin.display()
            ));
        }
    }
    let root = bin
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", bin.display()))?;
    let root = fs::canonicalize(root).map_err(|e| format!("{}: {e}", root.display()))?;
    let digest = atpkg::tree::tree_root(&root).map_err(|e| format!("{}: {e}", root.display()))?;
    Ok((digest, root))
}

/// Computes the SHA-256 storage key for `(tree_sans_floors, prover, lane)`.
#[must_use]
pub fn facts_key(tree_sans_floors: &str, prover: &str, lane: &str) -> String {
    let material = format!(
        "{KEY_SCHEMA}\ntree-sans-floors {tree_sans_floors}\nprover {prover}\nlane {lane}\n"
    );
    hex(&Sha256::digest(material.as_bytes()))
}

/// Whether a `driver-ceiling` word says the prover takes no memory ceiling
/// ([`Host::uncapped`]): `yes` it takes one, `no` it does not.
fn ceiling_word(v: &str) -> Option<bool> {
    match v {
        "yes" => Some(false),
        "no" => Some(true),
        _ => None,
    }
}

/// Parsed `.plan` file written at the start of a verification lane run.
#[derive(Clone, Debug, Default)]
pub struct Plan {
    pub facts: Facts,
    /// Diagnostics from the obligation records validated by this measurement.
    failed_sources: BTreeMap<String, String>,
    pub specs: BTreeMap<String, String>,
    pub logdir: PathBuf,
    pub ratchet: PathBuf,
    pub memory: PathBuf,
    pub host_obs: Option<PathBuf>,
    pub try_refused: bool,
    pub remeasure: bool,
    pub file: Option<FilePlan>,
}

/// Filing parameters (`store`, `key`, `tree_sans_floors`, `prover`, `commit`) inside a [`Plan`].
#[derive(Clone, Debug, Default)]
pub struct FilePlan {
    pub refused: String,
    pub store: PathBuf,
    pub commit: String,
    pub tree: String,
    pub prover: String,
    pub prover_bin: PathBuf,
    pub lane_env: LaneEnv,
    pub(super) packages: build_files::Packages,
    pub cargo_home: Option<PathBuf>,
    pub target_root: PathBuf,
    pub host: String,
    pub load: String,
    pub jobs: String,
    pub date: String,
}

/// Sanitized environment variables declared in a verification lane [`.plan`](Plan).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LaneEnv {
    pub pins: BTreeMap<String, String>,
    pub passed: BTreeSet<String>,
}

impl Plan {
    /// Parses a `.plan` file written by `trust-lane.sh`.
    pub fn parse(text: &str) -> std::result::Result<Plan, String> {
        let mut lines = text.lines();
        if lines.next() != Some(PLAN_MAGIC) {
            return Err(format!("not a lane plan (no `{PLAN_MAGIC}` line)"));
        }
        let mut p = Plan::default();
        let mut file = FilePlan::default();
        let mut files = false;
        let (mut mem_rows, mut refused) = (Vec::new(), Vec::new());
        for line in lines {
            let (k, v) = line.split_once(' ').unwrap_or((line, ""));
            let bad = || format!("plan line {line:?} is not one this judge reads");
            match k {
                "driver" => p.facts.driver = v.into(),
                "driver-id" => p.facts.driver_id = v.into(),
                "trustd" => p.facts.trustd = v.into(),
                "only" => p.facts.only = v.into(),
                "logdir" => p.logdir = v.into(),
                "ratchet" => p.ratchet = v.into(),
                "memory-file" => p.memory = v.into(),
                "host-obs" => p.host_obs = (!v.is_empty()).then(|| v.into()),
                "try-refused" => p.try_refused = v == "1",
                "remeasure" => p.remeasure = v == "1",
                "host-mem" => p.facts.host.mem = parse_uint(v).ok_or_else(bad)?,
                "host-mem-os" => p.facts.host.memtotal = v == "MemTotal",
                "host-reach" => p.facts.host.reach = parse_uint(v).ok_or_else(bad)?,
                "driver-ceiling" => p.facts.host.uncapped = ceiling_word(v).ok_or_else(bad)?,
                // The host's one-minute load as the run began and as it ended: provenance only.
                "load-start" | "load-end" => {}
                "now" => p.facts.measured_at = v.into(),
                "memory" => {
                    let f: Vec<&str> = v.split('\t').collect();
                    let row = MemRow::from_fields(f.get(1..).ok_or_else(bad)?).ok_or_else(bad)?;
                    mem_rows.push((f[0].to_string(), row));
                }
                "refused" => {
                    let f: Vec<&str> = v.split('\t').collect();
                    let seen = Seen::from_fields(f.get(1..).ok_or_else(bad)?).ok_or_else(bad)?;
                    refused.push((f[0].to_string(), seen));
                }
                // Older plans included missing names from their initial ratchet.
                "vanished" => {}
                "ambiguous" => {
                    let mut parts = v.split('\t');
                    let (Some(n), Some(a), Some(b), None) =
                        (parts.next(), parts.next(), parts.next(), parts.next())
                    else {
                        return Err(bad());
                    };
                    p.facts.ambiguous.push((n.into(), a.into(), b.into()));
                }
                "crate" => {
                    let mut parts = v.split('\t');
                    let (Some(name), Some(spec), Some(word), None) =
                        (parts.next(), parts.next(), parts.next(), parts.next())
                    else {
                        return Err(bad());
                    };
                    let decision = Decision::from_word(word).ok_or_else(bad)?;
                    p.specs.insert(name.into(), spec.into());
                    p.facts.crates.push(Crate::new(name, decision));
                }
                "file" => {
                    files = true;
                    file.refused = v.into();
                }
                "store" => file.store = v.into(),
                "commit" => file.commit = v.into(),
                "tree" => file.tree = v.into(),
                "prover" => file.prover = v.into(),
                "prover-bin" => file.prover_bin = v.into(),
                "lane-pin" => {
                    let (name, value) = v.split_once('=').ok_or_else(bad)?;
                    if name.is_empty() {
                        return Err(bad());
                    }
                    file.lane_env.pins.insert(name.into(), value.into());
                }
                "lane-passed" if !v.is_empty() && !v.contains(['=', ' ']) => {
                    file.lane_env.passed.insert(v.into());
                }
                "target-root" => file.target_root = v.into(),
                "cargo-home" => file.cargo_home = (!v.is_empty()).then(|| v.into()),
                "host" => file.host = v.into(),
                "load" => file.load = v.into(),
                "jobs" => file.jobs = v.into(),
                "date" => file.date = v.into(),
                _ => return Err(bad()),
            }
        }
        if files {
            p.file = Some(file);
        }
        for (name, row) in mem_rows {
            if let Some(c) = p.facts.crates.iter_mut().find(|c| c.name == name) {
                c.memory = row;
            }
        }
        for (name, seen) in refused {
            match p.facts.crates.iter_mut().find(|c| c.name == name) {
                Some(c) if matches!(c.decision, Decision::Gate(_)) => {
                    c.decision = Decision::Memory(seen);
                }
                _ => return Err(format!("plan refuses `{name}`, which it does not gate")),
            }
        }
        Ok(p)
    }

    /// Evaluates up-front memory admission for all crates and returns the rewritten plan text.
    /// `peaks` is each library's last recorded check from the cost history under the prover
    /// that runs ([`Peak`]). A peak whose claim is not below what can ever be available on this
    /// host (the RAM less what is wired, else the RAM) is refused whatever the prover's ceiling
    /// ([`peak_seen`]); the RAM / 8 model refuses only under a prover that takes a ceiling
    /// ([`Host::uncapped`]).
    pub fn admit(
        &self,
        peaks: &BTreeMap<String, Peak>,
    ) -> std::result::Result<(String, Vec<(String, String)>), String> {
        let shown = self.memory.display().to_string();
        let text = crate::durable::read_text(&self.memory)
            .map_err(|e| format!("memory record {shown}: {e}"))?;
        let record = parse_memory(&text, &shown)?;
        let ratchet = optional_text(&self.ratchet).map_err(|e| e.to_string())?;
        self.facts.check_floor_rows(
            ratchet.as_deref(),
            &self.ratchet.display().to_string(),
            self.remeasure || self.facts.only.is_empty(),
        )?;
        let obs = match &self.host_obs {
            Some(path) => optional_text(path).map_err(|e| e.to_string())?,
            None => None,
        };
        let obs = obs.as_deref().map(parse_host_obs).unwrap_or_default();
        let driver_row = self.facts.driver_row();
        let (mut lines, mut refused) = (String::new(), Vec::new());
        let none = MemRow::default();
        for c in &self.facts.crates {
            let found = record.get(&c.name);
            let row = found
                .and_then(|rows| row_for(rows, &self.facts.host))
                .unwrap_or(&none);
            if found.is_some() {
                let _ = writeln!(lines, "memory {}\t{}", one_line(&c.name), row.fields());
            }
            if !matches!(c.decision, Decision::Gate(_)) || self.try_refused {
                continue;
            }
            if let Some(seen) = peaks
                .get(&c.name)
                .and_then(|peak| peak_seen(&self.facts.host, peak))
                && let Some(r) = peak_refusal(&self.facts.host, &c.name, &seen)
            {
                let _ = writeln!(lines, "refused {}\t{}", one_line(&c.name), seen.fields());
                refused.push((c.name.clone(), r.line));
                continue;
            }
            let floor = floor_of(ratchet.as_deref(), &c.name)
                .map(|f| f.proved)
                .filter(|f| !f.is_empty());
            let spec = self
                .specs
                .get(&c.name)
                .map_or(c.name.as_str(), String::as_str);
            let seen = floor.as_deref().and_then(|f| {
                obs.iter()
                    .find(|o| o.spec == spec && o.floor == f && o.prover == driver_row)
                    .map(HostObs::seen)
            });
            if let Some(r) = memory_refusal(
                &self.facts.host,
                &c.name,
                floor.is_some(),
                row,
                seen.as_ref(),
            ) {
                let seen_fields = seen
                    .as_ref()
                    .map_or(std::borrow::Cow::Borrowed(NO_SEEN), |s| {
                        std::borrow::Cow::Owned(s.fields())
                    });
                let _ = writeln!(lines, "refused {}\t{seen_fields}", one_line(&c.name));
                refused.push((c.name.clone(), r.line));
            }
        }
        Ok((lines, refused))
    }

    /// Reads each gated crate's stored log ([`open_log`]) and its `.rc`, `.trustd`,
    /// `.trustd-lost`, `.budget`, `.pace` and obligation record
    /// ([`crate::trust_oblig::RECORD_SUFFIX`]) beside it into `self.facts`.
    /// # Errors
    /// A gated log or required status is unavailable, the status is malformed, or a present
    /// optional receipt could not be read or a pace or obligation record is malformed. No partial
    /// measurement is kept.
    pub fn measure(&mut self) -> std::io::Result<()> {
        self.measure_with(|path, rc| Measured::of_log(open_log(path)?, rc))
    }

    fn measure_with(
        &mut self,
        mut measure: impl FnMut(&Path, &str) -> std::io::Result<Measured>,
    ) -> std::io::Result<()> {
        let read = |p: PathBuf| optional_receipt(&p).map(Option::unwrap_or_default);
        let mut facts = self.facts.clone();
        let mut failed_sources = BTreeMap::new();
        facts.grants = read(self.logdir.join(".trustd-grants"))?;
        facts.trustd_seen = read(self.logdir.join(".trustd-seen"))?;
        for c in &mut facts.crates {
            if let Decision::Gate(m) = &mut c.decision {
                let spec = self
                    .specs
                    .get(&c.name)
                    .map_or(c.name.as_str(), String::as_str);
                let log = self.logdir.join(format!("{spec}.log"));
                let rc = execution_status(&self.logdir.join(format!("{spec}.log.rc")))?;
                *m = measure(&log, &rc).map_err(|e| {
                    std::io::Error::new(e.kind(), format!("{}: {e}", log.display()))
                })?;
                let probe = read(self.logdir.join(format!("{spec}.log.trustd")))?;
                if !m.launch.is_empty() {
                    let (held, what) = probe.split_once('\t').unwrap_or((probe.as_str(), ""));
                    (m.held, m.held_what) = (one_line(held), one_line(what));
                }
                let mark = self.logdir.join(format!("{spec}.log.budget"));
                if let Some(said) = optional_receipt(&mark)? {
                    let said = said.lines().next().unwrap_or("");
                    m.wall_budget = if !said.is_empty() && said.bytes().all(|b| b.is_ascii_digit())
                    {
                        said.to_string()
                    } else {
                        "?".into()
                    };
                }
                let pace = self.logdir.join(format!("{spec}.log.pace"));
                m.pace = optional_text(&pace)?
                    .map(|text| {
                        // A cut after four or six fields looks like a complete older format.
                        text.strip_suffix('\n')
                            .and_then(|line| Pace::parse(line.strip_suffix('\r').unwrap_or(line)))
                            .ok_or_else(|| {
                                std::io::Error::new(
                                    std::io::ErrorKind::InvalidData,
                                    format!(
                                        "{}: incomplete or malformed pace receipt",
                                        pace.display()
                                    ),
                                )
                            })
                    })
                    .transpose()?;
                m.trustd_lost =
                    one_line(&read(self.logdir.join(format!("{spec}.log.trustd-lost")))?);
                let record = self
                    .logdir
                    .join(format!("{spec}.log{}", crate::trust_oblig::RECORD_SUFFIX));
                m.backstops = optional_receipt(&record)?
                    .map(|t| {
                        crate::trust_oblig::Record::parse_for(
                            &t,
                            crate::trust_lane::package_identity(&c.name),
                        )
                        .map_err(|why| {
                            std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                format!("{}: {why}", record.display()),
                            )
                        })
                    })
                    .transpose()?
                    .map(|r| {
                        if let Some(note) = failed_source_note(&c.name, &m.summary, &r) {
                            failed_sources.insert(c.name.clone(), note);
                        }
                        Backstop::of_record(&r)
                    });
            }
        }
        self.facts = facts;
        self.failed_sources = failed_sources;
        Ok(())
    }
}

fn env_dep_refusal(name: &str, is_set: bool, lane: &LaneEnv) -> String {
    if lane.passed.contains(name) {
        format!("the lane passes {name} from the host, and no key covers its value")
    } else if lane.pins.contains_key(name) {
        format!(
            "the compile's read of {name} could not be attributed to the lane pin or a covered build script"
        )
    } else if !is_set {
        format!(
            "the lane cannot establish that {name} stays unset from the compiler unit and its build-script inputs"
        )
    } else {
        format!(
            "{name} was set, and the lane has no matching setting or build-script output whose declared inputs the key covers"
        )
    }
}

fn dep_info_check(
    target_root: &Path,
    lane: &LaneEnv,
    packages: &build_files::Packages,
    keyed_file: &dyn Fn(&str, &Path) -> bool,
) -> std::result::Result<(), String> {
    let io_error = |path: &Path, err: std::io::Error| {
        format!(
            "could not inspect dep-info inputs at {}: {err}",
            path.display()
        )
    };
    let mut stack = vec![target_root.to_path_buf()];
    let (mut found, mut named) = (0usize, None::<(PathBuf, &str)>);
    let mut consumers = Vec::new();
    while let Some(dir) = stack.pop() {
        let root = dir == target_root;
        let entries = fs::read_dir(&dir).map_err(|err| io_error(&dir, err))?;
        for entry in entries {
            let e = entry.map_err(|err| io_error(&dir, err))?;
            let file_name = e.file_name();
            if root
                && !file_name
                    .to_str()
                    .and_then(|s| s.strip_prefix('j'))
                    .is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
            {
                continue;
            }
            let p = e.path();
            let ft = e.file_type().map_err(|err| io_error(&p, err))?;
            if root && !ft.is_dir() {
                return Err(format!("{}: expected a real slot directory", p.display()));
            }
            if !p.extension().is_some_and(|x| x == "d") {
                if ft.is_dir() {
                    stack.push(p);
                }
                continue;
            }
            let text = crate::durable::read_file(&p).map_err(|err| io_error(&p, err))?;
            for floors in FILES_OUTSIDE_TREE_KEY {
                let file = floors.rsplit('/').next().unwrap_or(floors).as_bytes();
                if text.windows(file.len()).any(|w| w == file) {
                    named = Some((p.clone(), floors));
                }
            }
            let mut reads = Vec::new();
            for line in text.split(|b| *b == b'\n') {
                let Some(read) = line.strip_prefix(b"# env-dep:".as_slice()) else {
                    continue;
                };
                let read = std::str::from_utf8(read).map_err(|_| {
                    format!("{} contains an invalid UTF-8 env-dep record", p.display())
                })?;
                reads.push(read.trim_end_matches('\r').to_owned());
            }
            if !reads.is_empty() {
                consumers.push((p, reads));
            }
            found += 1;
        }
    }
    if let Some((p, floors)) = named {
        return Err(format!(
            "{} names {floors}: a compile read a floors file, so it is an input and a record keyed without it would be carried to other floors",
            p.display()
        ));
    }
    if found == 0 {
        return Err(format!(
            "no dep-info under {} was written by this run, so nothing shows which files its compiles read",
            target_root.display()
        ));
    }
    consumers.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    let mut refused = Vec::new();
    for (file, reads) in &consumers {
        let rows = reads.iter().map(|read| match read.split_once('=') {
            Some((name, value)) => (name, Some(value)),
            None => (read.as_str(), None),
        });
        for (name, is_set) in build_env::refused_reads(file, rows, lane, packages, keyed_file)? {
            refused.push((name, file, is_set));
        }
    }
    refused.sort_unstable();
    refused.dedup_by_key(|(name, _, _)| *name);
    if let Some(&(name, file, is_set)) = refused.first() {
        let why = env_dep_refusal(name, is_set, lane);
        let more = refused.len() - 1;
        let others = if more > 0 {
            format!(" (and {more} other variable(s) the same way)")
        } else {
            String::new()
        };
        return Err(format!(
            "{} records a compile-time read of {name}: {why}{others} — a record keyed without that value would be carried to builds that read another",
            file.display()
        ));
    }
    Ok(())
}

/// Finds ancestor `.cargo/config[.toml]` files outside `root` and under `cargo_home`.
pub(crate) fn outside_config(root: &Path, cargo_home: Option<&Path>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let names = ["config", "config.toml"];
    let mut at = root.parent();
    while let Some(dir) = at {
        for n in names {
            let p = dir.join(".cargo").join(n);
            if p.is_file() {
                out.push(p);
            }
        }
        at = dir.parent();
    }
    if let Some(home) = cargo_home {
        for n in names {
            let p = home.join(n);
            if p.is_file() && !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out
}

/// Output lines, updated memory records, and [`Verdict`] produced by [`conclude`].
#[derive(Debug)]
pub struct Concluded {
    pub verdict: Verdict,
    pub credit_line: String,
    /// The measured floors admit this judgment's persistent updates, including its cost seed.
    pub floors_accepted: bool,
    pub facts_line: Option<String>,
    pub said: Vec<String>,
}

/// Merges a `--remeasure` run's [`Facts`] (`again`) into a base run's [`Facts`] (`base`).
pub fn remeasured(base: &Facts, again: &Facts) -> std::result::Result<Facts, String> {
    let (Some(b), Some(a)) = (&base.provenance, &again.provenance) else {
        return Err("a record without its key cannot be completed".into());
    };
    if a.key != b.key
        || a.tree_sans_floors != b.tree_sans_floors
        || a.prover != b.prover
        || a.lane != b.lane
    {
        return Err(format!(
            "the stored record is key {}, and this run measured key {}: facts of two keys are never one record",
            short(&b.key),
            short(&a.key)
        ));
    }
    // Only the plan holds the transient reachable-memory sample. Filed decisions
    // already record any refusal it caused, and replay never reconstructs the sample.
    if again.host.mem != base.host.mem {
        return Err(format!(
            "the stored record was measured on a host with {} bytes of memory, and this run on one with {}",
            base.host.mem, again.host.mem
        ));
    }
    if again.host.memtotal != base.host.memtotal {
        return Err("the stored record and this run use different sources for host RAM (MemTotal or platform RAM)".into());
    }
    if again.host.uncapped != base.host.uncapped {
        return Err(format!(
            "the stored record used {}, and this run uses {}",
            base.host.desc(),
            again.host.desc(),
        ));
    }
    if again.driver_id != base.driver_id {
        return Err(format!(
            "the stored record's driver says `{}`, and this run's `{}`",
            base.driver_id, again.driver_id
        ));
    }
    let mut out = base.clone();
    let mut names = Vec::new();
    for c in &again.crates {
        let Some(slot) = out.crates.iter_mut().find(|o| o.name == c.name) else {
            return Err(format!(
                "the stored record selects no library named {}",
                c.name
            ));
        };
        *slot = c.clone();
        names.push(c.name.clone());
    }
    if !names.is_empty()
        && let Some(p) = out.provenance.as_mut()
    {
        // Older v17 writers used the sole word "nothing" for an empty round.
        // A colon cannot occur in a Cargo package or feature-row name.
        let selection = if names.len() == 1 && names[0] == "nothing" {
            "crate:nothing".into()
        } else {
            names.join(" ")
        };
        p.remeasured
            .push(format!("{} {} {selection}", a.date, a.head));
    }
    Ok(out)
}

/// A pre-grade receipt lacks an aggregate even if a later round collected some grades.
enum ReceiptCredit<'a> {
    Measured(crate::trust_credit::Credit),
    Uncollected(&'a str),
}

impl ReceiptCredit<'_> {
    fn measured(self) -> std::result::Result<crate::trust_credit::Credit, String> {
        match self {
            Self::Measured(credit) => Ok(credit),
            Self::Uncollected(name) => Err(format!(
                "{name} CREDIT-UNREADABLE: stored facts have no calculus grade evidence; run `targo --unverified ship trust run --remeasure` to collect its grades"
            )),
        }
    }

    fn replay(
        self,
        epoch: Option<&crate::trust_credit::Epoch>,
    ) -> std::result::Result<(Vec<String>, String), String> {
        if let Self::Uncollected(name) = self
            && epoch.is_none_or(|e| e.calculus_floor == 0)
        {
            return Ok((
                Vec::new(),
                format!("proved (calculus): unavailable; stored grades missing for {name}"),
            ));
        }
        let credit = self.measured()?;
        Ok((
            crate::trust_credit::judge(epoch, &credit),
            credit.line(epoch),
        ))
    }
}

/// Count the same gated checks whose measurements are judged, including retained rounds.
fn calculus_credit(facts: &Facts) -> std::result::Result<ReceiptCredit<'_>, String> {
    let mut credit = crate::trust_credit::Credit::default();
    let mut uncollected = None;
    for c in &facts.crates {
        let Decision::Gate(m) = &c.decision else {
            continue;
        };
        let record = match &m.grade {
            GradeEvidence::Legacy => {
                uncollected.get_or_insert(c.name.as_str());
                continue;
            }
            GradeEvidence::Absent => None,
            GradeEvidence::Measured(tally) => Some(Ok(*tally)),
        };
        credit.add(&c.name, record);
    }
    if !credit.refused.is_empty() {
        return Err(crate::trust_credit::judge(None, &credit).join("; "));
    }
    Ok(match uncollected {
        Some(name) => ReceiptCredit::Uncollected(name),
        None => ReceiptCredit::Measured(credit),
    })
}

/// Completes a remeasurement only when its original record can be folded and replaced.
/// A failure here precedes any judgment or floor update from the partial run.
fn complete_record(
    store: &Path,
    key: &str,
    again: &Facts,
) -> std::result::Result<(Facts, PathBuf, crate::trust_credit::Credit), String> {
    let writer = FactsWriter::lock(store, key)?;
    let text = optional_text(&writer.path)
        .map_err(|e| format!("could not read original whole-tree record: {e}"))?
        .ok_or_else(|| {
            format!(
                "original whole-tree record is missing: {}",
                writer.path.display()
            )
        })?;
    let base = parse_stored_facts(&writer.path, &text)?;
    let merged = remeasured(&base, again)?;
    let credit = calculus_credit(&merged)?.measured()?;
    let path = writer.write(&merged)?;
    Ok((merged, path, credit))
}

fn filing(
    git: &dyn GitRunner,
    repo: &Path,
    plan: &Plan,
    file: &FilePlan,
) -> std::result::Result<Provenance, String> {
    if !file.refused.is_empty() {
        return Err(file.refused.clone());
    }
    let (mut paths, mut names) = (BTreeSet::new(), Vec::new());
    for c in &plan.facts.crates {
        if let Decision::Gate(m) = &c.decision
            && !m.path_refused.is_empty()
        {
            paths.extend(m.path_refused.iter().map(|r| r.path.as_str()));
            names.push(c.name.as_str());
        }
    }
    if !names.is_empty() {
        return Err(format!(
            "targo refused a path on this host ({}) for {} (REFUSED PATH above), which says nothing about the code",
            paths.into_iter().collect::<Vec<_>>().join(", "),
            names.join(" ")
        ));
    }
    if file.prover.is_empty() {
        return Err("the prover could not be identified when the run started".into());
    }
    if file.lane_env.passed.is_empty() {
        return Err(
            "the lane's plan declares no environment of its own, so nothing shows its compiles ran in one"
                .into(),
        );
    }
    if let Some(c) = outside_config(repo, file.cargo_home.as_deref()).first() {
        return Err(format!(
            "cargo read {}, a config file outside the tree the key covers",
            c.display()
        ));
    }
    let (prover, root) = match prover_identity(&file.prover_bin) {
        Err(why) => return Err(format!("the prover cannot be identified now ({why})")),
        Ok((now, _)) if now != file.prover => {
            return Err(format!(
                "the prover changed during the run ({} -> {})",
                short(&file.prover),
                short(&now)
            ));
        }
        Ok(found) => found,
    };
    let gated = plan
        .facts
        .crates
        .iter()
        .any(|c| matches!(c.decision, Decision::Gate(_)));
    let check_inputs = gated || !plan.remeasure;
    let mut regular = BTreeSet::new();
    let tsr = tree_inputs(git, &file.commit, |path| {
        if check_inputs {
            regular.insert(path.to_owned());
        }
    })
    .map_err(|e| format!("git could not list the tree the key covers ({e})"))?;
    if check_inputs
        && let Err(why) = dep_info_check(
            &file.target_root,
            &file.lane_env,
            &file.packages,
            &|package, path| file.packages.covers(&regular, package, path),
        )
    {
        return Err(why);
    }
    Ok(Provenance {
        key: facts_key(&tsr, &prover, LANE_WHOLE_TREE),
        tree_sans_floors: tsr,
        prover,
        lane: LANE_WHOLE_TREE.into(),
        prover_root: root.display().to_string(),
        trustc: trustc_commit(&file.prover_bin),
        head: file.commit.clone(),
        tree: file.tree.clone(),
        host: file.host.clone(),
        load: file.load.clone(),
        jobs: file.jobs.clone(),
        date: file.date.clone(),
        remeasured: Vec::new(),
    })
}

/// [`BACKSTOPS`] beside a ratchet at `ratchet`.
#[must_use]
pub fn backstops_beside(ratchet: &Path) -> PathBuf {
    ratchet.with_file_name(BACKSTOPS.rsplit('/').next().unwrap_or(BACKSTOPS))
}

/// Holds an updating checkout's floor directory lock from the first policy read through
/// the final policy write. A live run already holds its target-root lock and only takes
/// the receipt-key lock inside this guard. Receipt-only operations never take this lock.
pub(super) struct FloorInputs<'a> {
    ratchet: Option<String>,
    backstops: Option<String>,
    memory: Option<BTreeMap<String, Vec<MemRow>>>,
    update: Update<'a>,
    _writer: Option<aterm_update_core::FileLock>,
}

impl<'a> FloorInputs<'a> {
    pub(super) fn read(ratchet: &Path, memory: &Path, update: Update<'a>) -> std::io::Result<Self> {
        use std::os::unix::fs::OpenOptionsExt as _;

        let writer = if update.on {
            let parent = ratchet
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            // Each policy file is replaced by rename. Its containing directory stays put.
            let held = aterm_uds::kept_out(|| {
                fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_DIRECTORY)
                    .open(parent)
            })
            .and_then(aterm_update_core::FileLock::lock_open)
            .map_err(|e| {
                std::io::Error::new(
                    e.kind(),
                    format!("could not lock {}: {e}", parent.display()),
                )
            })?;
            Some(held)
        } else {
            None
        };
        Ok(Self {
            ratchet: optional_text(ratchet)?,
            backstops: optional_text(&backstops_beside(ratchet))?,
            memory: if update.on {
                optional_text(memory)?
                    .map(|text| {
                        parse_memory(&text, &memory.display().to_string()).map_err(|why| {
                            std::io::Error::new(std::io::ErrorKind::InvalidData, why)
                        })
                    })
                    .transpose()?
            } else {
                None
            },
            update,
            _writer: writer,
        })
    }

    fn judge(
        &self,
        facts: &Facts,
        paths: (&str, &str, Option<&Plan>),
        credit_failures: &[String],
    ) -> Verdict {
        judge_with_credit(
            facts,
            self.ratchet.as_deref(),
            self.backstops.as_deref(),
            paths,
            if credit_failures.is_empty() {
                self.update
            } else {
                Update::NONE
            },
            self.memory.as_ref(),
            credit_failures,
        )
    }
}

/// Stages each supplied text in a private directory beside its target before moving any
/// file into place. The caller provides an existing parent for each write. A staging write
/// that fails leaves every target as it was, and cleanup touches only the directories this
/// pass acquired. Files are renamed individually; a later rename failure does not undo one
/// already placed.
///
/// # Errors
/// Names the file it could not stage, write or move; guards attempt to remove remaining owned
/// scratch.
pub(super) fn write_staged_text(files: &[(&Path, Option<&str>)]) -> std::io::Result<()> {
    let mut staged = Vec::new();
    for &(path, text) in files {
        let Some(text) = text else {
            continue;
        };
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let scratch = aterm_tempfile::Builder::new()
            .prefix(".trust-write-")
            .suffix(".noindex")
            .tempdir_in(parent)
            .map_err(|e| {
                std::io::Error::new(e.kind(), format!("could not stage {}: {e}", path.display()))
            })?;
        let file = scratch.path().join("data");
        crate::durable::write_fresh(&file, text).map_err(|e| {
            std::io::Error::new(
                e.kind(),
                format!(
                    "could not write {} (for {}): {e}",
                    file.display(),
                    path.display()
                ),
            )
        })?;
        staged.push((path, scratch, file));
    }
    for (path, _scratch, file) in staged {
        fs::rename(&file, path).map_err(|e| {
            std::io::Error::new(
                e.kind(),
                format!(
                    "could not move {} onto {}: {e}",
                    file.display(),
                    path.display()
                ),
            )
        })?;
    }
    Ok(())
}

/// Applies the up-only rule to the proposed floors. A rejected proposal leaves the
/// measurements available for replay but publishes no floor-related updates.
fn check_measured_floors(
    git: &dyn GitRunner,
    facts: &Facts,
    working: Option<&str>,
    verdict: &mut Verdict,
) -> std::result::Result<bool, String> {
    let lowered = lowered_since_measured(git, facts, verdict.ratchet.as_deref().or(working))?;
    if lowered.is_empty() {
        return Ok(true);
    }
    let mut failures = verdict.failed.clone();
    failures.push(format!(
        "the working floors lower {} floor(s) the ratchet's up-only rule refuses (LOWERED above, with the measurement commit for each)",
        lowered.len()
    ));
    verdict.word = "FAIL";
    verdict.exit = 1;
    verdict.tail = format!("VERDICT: FAIL — {}", failures.join("; "));
    for why in lowered {
        let row = format!("LOWERED: {why}");
        let _ = writeln!(verdict.report, "{row}");
        verdict.failed.push(row);
    }
    verdict.ratchet = None;
    verdict.backstops = None;
    verdict.memory_rows.clear();
    verdict.host_obs.clear();
    Ok(false)
}

/// Measures completed logs, files their facts, and judges the live run against its proof
/// floors and calculus credit. A credit regression is a verdict on the run; the valid
/// measurements remain available for replay against proof floors.
pub(super) fn conclude(
    git: &dyn GitRunner,
    repo: &Path,
    plan: &mut Plan,
    floors: &FloorInputs<'_>,
    grades: &BTreeMap<String, GradeEvidence>,
    epoch: Option<&crate::trust_credit::Epoch>,
) -> Result<Concluded> {
    plan.facts.check_floor_rows(
        floors.ratchet.as_deref(),
        &plan.ratchet.display().to_string(),
        plan.remeasure || plan.facts.only.is_empty(),
    )?;
    plan.measure().map_err(|e| {
        Error::new(format!(
            "compiler execution receipts could not be measured: {e}"
        ))
    })?;
    for c in &mut plan.facts.crates {
        if let Decision::Gate(m) = &mut c.decision {
            m.grade = grades.get(&c.name).copied().ok_or_else(|| {
                Error::new(format!(
                    "{} CREDIT-UNREADABLE: the live check supplied no grade observation",
                    c.name
                ))
            })?;
        }
    }
    let filed = plan
        .file
        .as_ref()
        .map(|file| (file, filing(git, repo, plan, file)));
    let mut facts = plan.facts.clone();
    if let Some((_, Ok(prov))) = &filed {
        facts.provenance = Some(prov.clone());
    }
    let (facts, completed_path, credit) = if plan.remeasure {
        let completion_error = |why: &str| {
            Error::new(format!(
                "--remeasure could not complete the stored facts: {why}"
            ))
        };
        let (file, provenance) = filed
            .as_ref()
            .ok_or_else(|| completion_error("no filing plan"))?;
        let prov = provenance.as_ref().map_err(|why| completion_error(why))?;
        let (merged, path, credit) = complete_record(&file.store, &prov.key, &facts)
            .map_err(|why| completion_error(&why))?;
        (merged, Some(path), credit)
    } else {
        let credit = calculus_credit(&facts)
            .and_then(ReceiptCredit::measured)
            .map_err(Error::new)?;
        (facts, None, credit)
    };
    let credit_failures = crate::trust_credit::judge(epoch, &credit);
    let backstops_path = backstops_beside(&plan.ratchet);
    let mut verdict = floors.judge(
        &facts,
        (
            &plan.ratchet.display().to_string(),
            &plan.memory.display().to_string(),
            Some(plan),
        ),
        &credit_failures,
    );
    let floors_accepted = (!plan.remeasure
        || check_measured_floors(git, &facts, floors.ratchet.as_deref(), &mut verdict)
            .map_err(Error::new)?)
        && credit_failures.is_empty();
    if plan.remeasure {
        let ran: BTreeSet<&str> = plan.facts.crates.iter().map(|c| c.name.as_str()).collect();
        verdict
            .memory_rows
            .retain(|(n, _)| ran.contains(n.as_str()));
        verdict.host_obs.retain(|(n, _)| ran.contains(n.as_str()));
    }
    write_staged_text(&[
        (&plan.ratchet, verdict.ratchet.as_deref()),
        (&backstops_path, verdict.backstops.as_deref()),
    ])
    .map_err(|e| Error::new(e.to_string()))?;
    // A partial proof verdict still reports the observed calculus shortfall.
    let mut said: Vec<String> = credit_failures
        .into_iter()
        .filter(|why| !verdict.failed.contains(why))
        .map(|why| format!("trust: {why}"))
        .collect();
    if !verdict.memory_rows.is_empty() {
        let written = crate::durable::read_text(&plan.memory)
            .map(|t| memory_rewritten(&t, &verdict.memory_rows))
            .and_then(|t| write_staged_text(&[(&plan.memory, Some(&t))]));
        if written.is_err() {
            for (name, _) in &verdict.memory_rows {
                said.push(format!(
                    "trust: {} NOT updated for {name} — the next --update-ratchet run writes it",
                    plan.memory.display()
                ));
            }
        }
    }
    if let Some(path) = &plan.host_obs
        && let Err(e) = write_host_obs(path, plan, &verdict)
    {
        said.push(format!(
            "trust: host memory observations NOT written ({}: {e}) — the next run refuses only by what it already holds; nothing was decided by it",
            path.display()
        ));
    }
    let none = |why: &str| Some(format!("FACTS: none — {why}"));
    let facts_line = match &filed {
        None => None,
        Some((_, Err(why))) => none(why),
        Some((file, Ok(prov))) => {
            match completed_path.map_or_else(|| file_record(&file.store, &facts), Ok) {
                Ok(path) => Some(format!(
                    "FACTS: {} (key {}, prover {}, trustc {}{}) — available for explicit verdicts against a matching commit's floors",
                    path.display(),
                    short(&prov.key),
                    short(&prov.prover),
                    short(&prov.trustc),
                    match (plan.remeasure, plan.facts.crates.is_empty()) {
                        (false, _) => String::new(),
                        (true, true) => "; nothing was left undecided".to_string(),
                        (true, false) => format!(
                            "; measured again: {}",
                            plan.facts
                                .crates
                                .iter()
                                .map(|c| c.name.as_str())
                                .collect::<Vec<_>>()
                                .join(" ")
                        ),
                    }
                )),
                Err(e) => Some(format!("FACTS: NOT FILED — {e}")),
            }
        }
    };
    Ok(Concluded {
        verdict,
        credit_line: credit.line(epoch),
        floors_accepted,
        facts_line,
        said,
    })
}

fn write_host_obs(path: &Path, plan: &Plan, verdict: &Verdict) -> std::io::Result<()> {
    if verdict.host_obs.is_empty() {
        return Ok(());
    }
    let spec = |name: &str| {
        plan.specs
            .get(name)
            .cloned()
            .unwrap_or_else(|| name.to_string())
    };
    let gone: BTreeSet<String> = verdict.host_obs.iter().map(|(n, _)| spec(n)).collect();
    let set: Vec<String> = verdict
        .host_obs
        .iter()
        .filter_map(|(n, row)| {
            let [outcome, ceil, peak, proved, floor] = row.as_ref()?;
            Some(format!(
                "{}\t{outcome}\t{ceil}\t{peak}\t{proved}\t{floor}\t{}\t{}",
                spec(n),
                plan.facts.driver_row(),
                plan.facts.measured_at
            ))
        })
        .collect();
    let old = optional_text(path)?;
    if set.is_empty() && old.is_none() {
        return Ok(());
    }
    let mut rows: Vec<String> = old
        .as_deref()
        .unwrap_or("")
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter(|l| !gone.contains(l.split('\t').next().unwrap_or("")))
        .map(str::to_string)
        .chain(set)
        .collect();
    rows.sort_unstable();
    let mut text = format!("{HOST_OBS_HEADER}\n");
    for r in rows {
        text.push_str(&r);
        text.push('\n');
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    write_staged_text(&[(path, Some(&text))])
}

/// Serializes receipt writers for one key across worktrees and target roots.
/// Readers use the atomically published file without taking the writer lock.
struct FactsWriter {
    path: PathBuf,
    _lock: aterm_update_core::FileLock,
}

impl FactsWriter {
    fn lock(store: &Path, key: &str) -> std::result::Result<Self, String> {
        fs::create_dir_all(store)
            .map_err(|e| format!("could not create {}: {e}", store.display()))?;
        let lock_path = store.join(format!(".{FACTS_PREFIX}{key}.lock"));
        let lock = aterm_update_core::FileLock::acquire(&lock_path)
            .map_err(|e| format!("could not lock {}: {e}", lock_path.display()))?;
        Ok(Self {
            path: store.join(format!("{FACTS_PREFIX}{key}")),
            _lock: lock,
        })
    }

    fn write(&self, facts: &Facts) -> std::result::Result<PathBuf, String> {
        write_staged_text(&[(&self.path, Some(&facts.to_record()))]).map_err(|e| e.to_string())?;
        Ok(self.path.clone())
    }
}

/// Atomically writes `facts` into `store` under its provenance key.
pub fn file_record(store: &Path, facts: &Facts) -> std::result::Result<PathBuf, String> {
    let key = facts
        .provenance
        .as_ref()
        .map(|p| p.key.as_str())
        .filter(|k| !k.is_empty())
        .ok_or("a record with no key cannot be filed")?;
    FactsWriter::lock(store, key)?.write(facts)
}

/// Extracts the `commit-hash` reported by `<bin>/trustc -vV`, or `"unknown"`.
#[must_use]
pub fn trustc_commit(bin: &Path) -> String {
    aterm_uds::spawnfd::command(bin.join("trustc"))
        .arg("-vV")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .find_map(|l| {
                    l.trim()
                        .strip_prefix("commit-hash:")
                        .map(str::trim)
                        .map(str::to_string)
                })
                .filter(|h| !h.is_empty())
        })
        .unwrap_or_else(|| "unknown".to_string())
}

/// Toolchain identity (`bin`, `id`, `commit`) used to look up stored verification facts.
#[derive(Clone, Debug)]
pub struct Prover {
    pub bin: PathBuf,
    pub identity: std::result::Result<String, String>,
    pub trustc: String,
}

impl Prover {
    /// Resolves a [`Prover`] from its toolchain `bin/` directory.
    #[must_use]
    pub fn at(bin: &Path) -> Prover {
        Prover {
            bin: bin.to_path_buf(),
            identity: prover_identity(bin).map(|(id, _)| id),
            trustc: trustc_commit(bin),
        }
    }

    /// Resolves a [`Prover`] from a `<bin dir> trustc <commit>` toolchain string.
    #[must_use]
    pub fn of_toolchain(toolchain: &str) -> Prover {
        let bin = toolchain
            .rsplit_once(" trustc ")
            .map_or(toolchain, |(dir, _)| dir);
        Prover::at(Path::new(bin))
    }

    fn shown(&self) -> String {
        match &self.identity {
            Ok(id) => format!("{} (trustc {})", short(id), short(&self.trustc)),
            Err(_) => format!("unidentified (trustc {})", short(&self.trustc)),
        }
    }
}

/// Reads the ratchet ([`RATCHET`]) as of `commit`, returning `Ok(None)` if absent.
pub fn ratchet_at(git: &dyn GitRunner, commit: &str) -> Result<Option<String>> {
    floors_at(git, commit, RATCHET)
}

/// Reads the floor file `path` as of `commit`, returning `Ok(None)` if absent.
fn floors_at(git: &dyn GitRunner, commit: &str, path: &str) -> Result<Option<String>> {
    original_blob_at(
        git,
        commit,
        path,
        aterm_uds::deadline::MAX_COMPLETE_STDOUT_BYTES,
    )?
    .map(|bytes| {
        String::from_utf8(bytes).map_err(|_| Error::new(format!("{commit}:{path} is not UTF-8")))
    })
    .transpose()
}

/// The credit ledger is part of the facts key. Every round therefore shares the
/// ledger at the selected immutable commit, including when its other floors differ.
pub(crate) fn working_credit_epoch(
    git: &dyn GitRunner,
    repo: &Path,
    measured: Option<&str>,
) -> std::result::Result<Option<crate::trust_credit::Epoch>, String> {
    use crate::trust_credit::{EPOCHS, current_epoch, parse_epochs};
    let epoch =
        current_epoch(repo).map_err(|why| format!("credit epoch CREDIT-UNREADABLE {why}"))?;
    if let Some(head) = measured {
        if let Some(text) = floors_at(git, head, EPOCHS)
            .map_err(|e| format!("credit epoch CREDIT-UNREADABLE {e}"))?
        {
            let epochs = parse_epochs(&text)
                .map_err(|e| format!("credit epoch CREDIT-UNREADABLE {head}:{e}"))?;
            let minimum = epochs
                .last()
                .expect("parse_epochs rejects an empty ledger")
                .calculus_floor;
            let working = epoch.as_ref().map_or(0, |e| e.calculus_floor);
            if working < minimum {
                return Err(format!(
                    "CREDIT-FLOOR-LOWERED: the working {EPOCHS} requires {working} calculus proofs, below the {minimum} required by {head}:{EPOCHS}"
                ));
            }
        }
    }
    Ok(epoch)
}

/// Which round supplied each crate's current facts. The writer appends rounds in
/// measurement order; a superseded or empty round supplies no current floor.
fn measurement_heads<'a>(
    facts: &'a Facts,
    p: &'a Provenance,
) -> std::result::Result<BTreeMap<&'a str, &'a str>, String> {
    let oid = |s: &str| matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit());
    if !oid(&p.head) {
        return Err("the facts name no full commit they were measured on".into());
    }
    let mut heads = BTreeMap::new();
    let mut twins = BTreeSet::new();
    for c in &facts.crates {
        let name = c.name.as_str();
        // Selection keeps an Ambiguous row for each version of a package with twins.
        if c.decision == Decision::Ambiguous && !twins.insert(name) {
            continue;
        }
        if heads.insert(name, p.head.as_str()).is_some() {
            return Err(format!("the facts repeat crate {name:?}"));
        }
    }
    for round in &p.remeasured {
        let mut fields = round.splitn(3, ' ');
        let date = fields.next().unwrap_or_default();
        let head = fields.next().unwrap_or_default();
        let selection = fields.next().unwrap_or_default();
        if aterm_types::rfc3339::parse_utc(date).is_none() || !oid(head) || selection.is_empty() {
            return Err(format!("invalid remeasurement provenance: {round:?}"));
        }
        if selection == "nothing" {
            if heads.contains_key("nothing") {
                return Err("the stored round cannot distinguish a no-op from a crate named nothing; measure the whole tree again".into());
            }
            continue;
        }
        let mut seen = BTreeSet::new();
        let names = if selection == "crate:nothing" {
            "nothing"
        } else {
            selection
        };
        for name in names.split(' ') {
            let slot = heads
                .get_mut(name)
                .ok_or_else(|| format!("a remeasurement names unknown crate {name:?}"))?;
            if !seen.insert(name) && !twins.contains(name) {
                return Err(format!("a remeasurement repeats crate {name:?}"));
            }
            *slot = head;
        }
    }
    Ok(heads)
}

/// Checks each working floor against the commit that produced its crate's current facts.
pub fn lowered_since_measured(
    git: &dyn GitRunner,
    facts: &Facts,
    judged: Option<&str>,
) -> std::result::Result<Vec<String>, String> {
    validate_floor_rows(judged.unwrap_or_default(), RATCHET, |_| true)?;
    let p = facts
        .provenance
        .as_ref()
        .ok_or("the facts name no commit they were measured on")?;
    let heads = measurement_heads(facts, p)?;
    let gated: BTreeSet<&str> = facts
        .crates
        .iter()
        .filter(|c| matches!(c.decision, Decision::Gate(_)))
        .map(|c| c.name.as_str())
        .collect();
    // A row outside the selected crates still obeys the original whole-tree rules.
    let head_for = |name: &str| heads.get(name).copied().unwrap_or(&p.head);
    let driver = (!facts.driver_id.is_empty()).then(|| facts.driver_row());
    // An empty group still matters: a gated crate's working floor may have been removed.
    let mut working: BTreeMap<&str, String> = gated
        .iter()
        .map(|name| (head_for(name), String::new()))
        .collect();
    for row in judged
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
    {
        let name = row.split('\t').next().unwrap_or_default();
        let _ = writeln!(working.entry(head_for(name)).or_default(), "{row}");
    }
    let mut out = Vec::new();
    for (head, after) in working {
        // Round heads are immutable commits of this input tree. A corrupt history can
        // otherwise substitute arbitrary floors from a different tree or a tree object.
        if rev_parse(git, &format!("{head}^{{commit}}")).map_err(|e| e.to_string())? != head
            || tree_sans_floors(git, head).map_err(|e| e.to_string())? != p.tree_sans_floors
        {
            return Err(format!(
                "measurement commit {head} does not identify the stored input tree"
            ));
        }
        let baseline = ratchet_at(git, head)
            .map_err(|e| format!("git cannot read {RATCHET} of {} ({e})", short(head)))?;
        validate_floor_rows(
            baseline.as_deref().unwrap_or_default(),
            &format!("{head}:{RATCHET}"),
            |name| head_for(name) == head,
        )?;
        let before = baseline
            .as_deref()
            .unwrap_or_default()
            .lines()
            .filter(|row| head_for(row.split('\t').next().unwrap_or_default()) == head)
            .collect::<Vec<_>>()
            .join("\n");
        out.extend(
            floors_lowered(
                &before,
                &after,
                &|name| gated.contains(name),
                driver.as_deref(),
            )
            .into_iter()
            .map(|issue| format!("{issue} (measured on {})", short(head))),
        );
    }
    Ok(out)
}

fn records_about(receipts: &Path, tsr: &str) -> Vec<(String, Facts)> {
    let mut out: Vec<(String, Facts)> = fs::read_dir(receipts)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let file_name = e.file_name();
            let name = file_name.to_str().filter(|n| n.starts_with(FACTS_PREFIX))?;
            let facts = Facts::parse(&crate::durable::read_text(&e.path()).ok()?)?;
            (facts.provenance.as_ref()?.tree_sans_floors == tsr).then(|| (name.to_string(), facts))
        })
        .collect();
    out.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    out
}

fn parse_stored_facts(path: &Path, text: &str) -> std::result::Result<Facts, String> {
    Facts::parse(text).ok_or_else(|| {
        let head = text.lines().next().unwrap_or("");
        let shown = path.display();
        if !matches!(head, FACTS_MAGIC | FACTS_MAGIC_V17) && head.starts_with("aterm-trust facts ") {
            format!(
                "`{shown}` was filed as `{head}`. This judge reads `{FACTS_MAGIC}` and `{FACTS_MAGIC_V17}`. Measure the tree again with targo --unverified ship trust run --whole-tree"
            )
        } else {
            format!("`{shown}` is not a complete facts record")
        }
    })
}

/// Looks up the whole-tree facts for `commit` and `prover` and checks their recorded identity.
pub fn facts_for(
    git: &dyn GitRunner,
    receipts: &Path,
    prover: &Prover,
    commit: &str,
) -> Result<std::result::Result<(Facts, String), String>> {
    let id = match &prover.identity {
        Ok(id) => id.clone(),
        Err(why) => {
            return Ok(Err(format!(
                "the requested prover ({}) cannot be identified: {why}",
                prover.bin.display()
            )));
        }
    };
    let tsr = tree_sans_floors(git, commit)?;
    let key = facts_key(&tsr, &id, LANE_WHOLE_TREE);
    let path = receipts.join(format!("{FACTS_PREFIX}{key}"));
    let shown = format!("{FACTS_PREFIX}{}", short(&key));
    let Some(text) = optional_text(&path).map_err(|e| Error::new(e.to_string()))? else {
        let others = records_about(receipts, &tsr);
        let mut why = format!(
            "no whole-tree Trust facts for its tree (every file but {RATCHET}, {MEMORY}, {COSTS} and {BACKSTOPS}) from the requested prover ({}, identity {}) (`{shown}`)",
            prover.bin.display(),
            short(&id)
        );
        for (n, f) in &others {
            if f.provenance.as_ref().is_some_and(|record| {
                record.prover == id && record.lane == LANE_WHOLE_TREE && record.key != key
            }) {
                let _ = write!(
                    why,
                    "; `{}` uses another key for this tree and prover. Measure it with targo --unverified ship trust run --whole-tree",
                    short_name(n)
                );
                continue;
            }
            let (prover, trustc, prover_root) = f
                .provenance
                .as_ref()
                .map_or(("", "", ""), |p| (&p.prover, &p.trustc, &p.prover_root));
            let _ = write!(
                why,
                "; `{}` holds facts for this tree from prover {} (trustc {}, at {prover_root})",
                short_name(n),
                short(prover),
                short(trustc),
            );
        }
        return Ok(Err(why));
    };
    let facts = match parse_stored_facts(&path, &text) {
        Ok(facts) => facts,
        Err(why) => return Ok(Err(why)),
    };
    let Some(p) = facts.provenance.as_ref() else {
        return Ok(Err(format!(
            "`{shown}` names no key: it is not a filed facts record"
        )));
    };
    if p.key != key || p.tree_sans_floors != tsr || p.prover != id || p.lane != LANE_WHOLE_TREE {
        return Ok(Err(format!(
            "`{shown}` is not the record its name says (its own lines name key {}, tree {}, prover {}, lane {})",
            short(&p.key),
            short(&p.tree_sans_floors),
            short(&p.prover),
            p.lane
        )));
    }
    Ok(Ok((facts, key)))
}

fn short_name(name: &str) -> String {
    name.strip_prefix(FACTS_PREFIX).map_or_else(
        || name.to_string(),
        |k| format!("{FACTS_PREFIX}{}", short(k)),
    )
}

/// Parsed `trust` CLI subcommand (`run`, `reduce`, `judge`, `credit` or `log`).
#[derive(Debug, PartialEq, Eq)]
pub enum TrustCmd {
    Run(crate::trust_lane::Config),
    Reduce(crate::trust_oblig::ReduceArgs),
    Judge {
        commit: String,
        prover: Option<PathBuf>,
        update: bool,
        reason: Option<String>,
    },
    /// `credit <record.ndjson>…`: the D27 gate's "proved (calculus)" counter over those
    /// `trust.obligations.v1` records, against the working credit epoch
    /// ([`crate::trust_credit`]).
    Credit(Vec<PathBuf>),
    /// `log <crate|name@version> [--target-root DIR]`: the log the lane kept of that library's
    /// check under the target root a run with that `--target-root` uses, decompressed
    /// ([`print_log`]).
    Log {
        lib: String,
        root: Option<PathBuf>,
    },
}

/// Parses CLI arguments for the `trust` subcommand.
pub fn parse_cmd<'a>(
    it: &mut impl Iterator<Item = &'a str>,
) -> std::result::Result<TrustCmd, String> {
    let verb = it
        .next()
        .ok_or("trust needs a verb: run, reduce, judge, credit or log")?;
    if verb == "run" {
        return crate::trust_lane::parse(it).map(TrustCmd::Run);
    }
    if verb == "reduce" {
        return crate::trust_oblig::parse_reduce(it).map(TrustCmd::Reduce);
    }
    if verb == "log" {
        let (mut lib, mut root) = (None, None);
        while let Some(arg) = it.next() {
            if arg == "--target-root" {
                let dir = it
                    .next()
                    .filter(|d| !d.is_empty() && !d.starts_with('-'))
                    .ok_or("trust log: --target-root needs a directory")?;
                if root.replace(PathBuf::from(dir)).is_some() {
                    return Err("trust log: --target-root given twice".into());
                }
            } else if arg.is_empty() || arg.starts_with('-') || lib.is_some() {
                return Err(format!("trust log: unexpected {arg:?}"));
            } else {
                lib = Some(arg.to_string());
            }
        }
        let lib = lib.ok_or("trust log names one library: <crate> or <name@version>")?;
        return Ok(TrustCmd::Log { lib, root });
    }
    if verb == "credit" {
        let records: Vec<PathBuf> = it.map(PathBuf::from).collect();
        if records.is_empty()
            || records
                .iter()
                .any(|r| r.as_os_str().to_string_lossy().starts_with('-'))
        {
            return Err(
                "trust credit takes the trust.obligations.v1 records to count, and no flag".into(),
            );
        }
        return Ok(TrustCmd::Credit(records));
    }
    if verb != "judge" {
        return Err(format!("unknown trust verb {verb:?}"));
    }
    let mut flags: BTreeMap<&str, Option<&str>> = BTreeMap::new();
    while let Some(flag) = it.next() {
        let value = if matches!(flag, "--commit" | "--prover" | "--reason") {
            let v = it.next().ok_or_else(|| format!("{flag} needs a value"))?;
            if v.is_empty() || v.starts_with('-') {
                return Err(format!("{flag} needs a value, got {v:?}"));
            }
            Some(v)
        } else if flag == "--update-ratchet" {
            None
        } else {
            return Err(format!("trust {verb}: unknown flag {flag:?}"));
        };
        if flags.insert(flag, value).is_some() {
            return Err(format!("{flag} given twice"));
        }
    }
    let value = |f: &str| flags.get(f).copied().flatten();
    let commit = value("--commit").unwrap_or("HEAD").to_string();
    let update = flags.contains_key("--update-ratchet");
    let reason = value("--reason")
        .map(|why| reason_arg(verb, why))
        .transpose()?;
    if reason.is_some() && !update {
        return Err(
            "trust judge: --reason says why --update-ratchet lowers a floor; give both".into(),
        );
    }
    Ok(TrustCmd::Judge {
        commit,
        prover: value("--prover").map(PathBuf::from),
        update,
        reason,
    })
}

fn asked_prover(prover: Option<PathBuf>) -> std::result::Result<Prover, String> {
    match prover {
        Some(bin) => Ok(Prover::at(&bin)),
        None => crate::gates::cut_toolchain()
            .map(|tc| Prover::of_toolchain(&tc))
            .map_err(|e| format!("cannot name the pinned release toolchain: {e}")),
    }
}

/// Lists unfinished or undecided checks and those missing historical grades.
/// An explicit memory retry also selects libraries refused by admission or held below
/// their proof floor by the compiler's memory ceiling.
pub fn pending_for(
    git: &dyn GitRunner,
    prover: &Prover,
    rev: &str,
    try_refused: bool,
) -> Result<std::result::Result<Vec<String>, String>> {
    let store = crate::gates::receipt_store(git)?;
    let head = rev_parse(git, &format!("{rev}^{{commit}}"))?;
    let (facts, _) = match facts_for(git, &store, prover, &head)? {
        Ok(found) => found,
        Err(why) => return Ok(Err(why)),
    };
    let ratchet = ratchet_at(git, &head)?;
    let backstops = floors_at(git, &head, BACKSTOPS)?;
    validate_floor_rows(
        ratchet.as_deref().unwrap_or_default(),
        &format!("{head}:{RATCHET}"),
        |_| true,
    )?;
    let verdict = judge(
        &facts,
        ratchet.as_deref(),
        backstops.as_deref(),
        RATCHET,
        MEMORY,
        Update::NONE,
    );
    let mut pending: BTreeSet<_> = verdict.unverified.into_iter().collect();
    if try_refused {
        pending.extend(verdict.memory.into_iter().map(|(name, _, _)| name));
    }
    for c in &facts.crates {
        if let Decision::Gate(m) = &c.decision
            && m.path_refused.is_empty()
            && (!m.wall_budget.is_empty()
                || !m.ended.is_empty()
                || m.transport
                || m.grade == GradeEvidence::Legacy
                || m.compiler_summary().is_ok_and(|s| s.unmeasured()))
        {
            pending.insert(c.name.clone());
        }
    }
    Ok(Ok(pending.into_iter().collect()))
}

/// Reduce retained evidence, returning `2` for refusal or a failed output writer.
/// A failed initial diagnostic leaves the output unpublished. A later stdout
/// failure leaves the completed record in place; it never replaces an output.
pub(crate) fn reduce_with_io(
    args: &crate::trust_oblig::ReduceArgs,
    out: &mut dyn std::io::Write,
    err: &mut dyn std::io::Write,
) -> i32 {
    if writeln!(
        err,
        "aterm-release trust reduce: parsing retained evidence; no proof claim; source root {}",
        one_line(&args.repo.display().to_string()),
    )
    .and_then(|()| err.flush())
    .is_err()
    {
        return 2;
    }
    match crate::trust_oblig::write_new_record(args) {
        Ok(record) => {
            if let Err(why) = writeln!(
                out,
                "{}: {} functions, {} obligations, {} proved; {}",
                record.package,
                record.functions,
                record.total(),
                record.proved(),
                args.out.display()
            )
            .and_then(|()| out.flush())
            {
                let _ = writeln!(err, "aterm-release trust reduce: stdout: {why}")
                    .and_then(|()| err.flush());
                return 2;
            }
            0
        }
        Err(why) => {
            let _ = writeln!(err, "aterm-release trust reduce: {why}").and_then(|()| err.flush());
            2
        }
    }
}

/// Prints to `out` the stored log in `logdir` of the one check `lib` names, decompressed
/// ([`open_log`]): `lib` is a `name@version` spec, or the name of a library whose log `logdir`
/// holds at one version. 0 printed, a stored log whose frame has not ended included (a check
/// still running, or one its lane was ended under): what its writer had stored, saying so on
/// `err`; 1 no log there for `lib` (or no `logdir`); 2 logs of more than one version (each
/// named on `err`), a `logdir` that could not be read, or a log that could not be.
pub(crate) fn print_log(
    logdir: &Path,
    lib: &str,
    out: &mut dyn std::io::Write,
    err: &mut dyn std::io::Write,
) -> i32 {
    let entries = match fs::read_dir(logdir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let _ = writeln!(err, "no log of {lib}: no {}", logdir.display());
            return 1;
        }
        Err(e) => {
            let _ = writeln!(err, "{}: {e}", logdir.display());
            return 2;
        }
    };
    let mut specs = BTreeSet::new();
    let mut nonregular = None;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                let _ = writeln!(err, "{}: {e}", logdir.display());
                return 2;
            }
        };
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let Some(spec) = name
            .strip_suffix(STORED_SUFFIX)
            .and_then(|n| n.strip_suffix(".log"))
        else {
            continue;
        };
        let Some((row, version)) = spec.split_once('@') else {
            continue;
        };
        if row.is_empty()
            || (lib != row && lib != spec)
            || !aterm_capacity::epoch_recipe::cargo_semver(version)
        {
            continue;
        }
        match entry.file_type() {
            Ok(kind) if kind.is_file() => {
                specs.insert(spec.to_string());
            }
            Ok(_) => {
                nonregular.get_or_insert_with(|| entry.path());
            }
            Err(e) => {
                let _ = writeln!(err, "{}: {e}", entry.path().display());
                return 2;
            }
        }
    }
    let Some(spec) = specs.first() else {
        if let Some(path) = nonregular {
            let _ = writeln!(err, "{} is not a regular compiler log", path.display());
            return 2;
        }
        let _ = writeln!(err, "no log of {lib} in {}", logdir.display());
        return 1;
    };
    if specs.len() > 1 {
        let all: Vec<&str> = specs.iter().map(String::as_str).collect();
        let _ = writeln!(
            err,
            "{lib} has logs at {}: name one as name@version",
            all.join(", ")
        );
        return 2;
    }
    let printed = open_log(&logdir.join(format!("{spec}.log")))
        .and_then(|mut log| std::io::copy(&mut log, out));
    match printed {
        Ok(_) => 0,
        // Its writer flushes what the check wrote while it waits for more, so this is the
        // check's output until a few seconds ago, but the last bytes it holds past its cap.
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
            let _ = writeln!(
                err,
                "{spec}: the stored log ends there, before its end: what its writer had stored so far (its check still running, its lane ended before it finished the log, or reading the check's output failed)"
            );
            0
        }
        Err(e) => {
            let _ = writeln!(err, "{spec}: {e}");
            2
        }
    }
}

/// Executes `cmd` against `repo` and returns its process exit code (`0`, `1`, or `2`).
pub fn run_cmd(repo: &Path, cmd: TrustCmd) -> i32 {
    let git = crate::ledger::GitCli::new(repo);
    match cmd {
        TrustCmd::Reduce(args) => reduce_with_io(
            &args,
            &mut std::io::stdout().lock(),
            &mut std::io::stderr().lock(),
        ),
        TrustCmd::Credit(records) => crate::trust_credit::run_cmd(repo, &records),
        TrustCmd::Log { lib, root } => match crate::trust_lane::log_dir(repo, root.as_deref()) {
            Ok(dir) => print_log(&dir, &lib, &mut std::io::stdout(), &mut std::io::stderr()),
            Err(why) => {
                eprintln!("aterm-release trust log: {why}");
                2
            }
        },
        TrustCmd::Run(mut cfg) => {
            cfg.repo = repo.to_path_buf();
            crate::trust_lane::run(&cfg, &mut std::io::stdout()).exit()
        }
        TrustCmd::Judge {
            commit,
            prover,
            update,
            reason,
        } => {
            let prover = match asked_prover(prover) {
                Ok(p) => p,
                Err(why) => {
                    eprintln!("aterm-release trust judge: {why}");
                    return 2;
                }
            };
            let found = crate::gates::receipt_store(&git).and_then(|store| {
                let head = rev_parse(&git, &format!("{commit}^{{commit}}"))?;
                Ok(facts_for(&git, &store, &prover, &head)?.map(|(facts, key)| (facts, key, head)))
            });
            let (facts, key, head) = match found {
                Ok(Ok(found)) => found,
                Ok(Err(why)) => {
                    println!("no facts to judge: {why}");
                    return 1;
                }
                Err(e) => {
                    eprintln!("aterm-release trust judge: {e}");
                    return 2;
                }
            };
            let path = repo.join(RATCHET);
            let backstops_path = repo.join(BACKSTOPS);
            let update = Update {
                on: update,
                reason: reason.as_deref(),
            };
            let mem = repo.join(MEMORY);
            let floors = match FloorInputs::read(&path, &mem, update) {
                Ok(inputs) => inputs,
                Err(e) => {
                    eprintln!(
                        "aterm-release trust judge: proof floor inputs could not be read: {e}"
                    );
                    return 2;
                }
            };
            let epoch = match working_credit_epoch(&git, repo, Some(&head)) {
                Ok(epoch) => epoch,
                Err(why) => {
                    eprintln!("aterm-release trust judge: {why}");
                    return 1;
                }
            };
            let (failures, credit_line) =
                match calculus_credit(&facts).and_then(|stored| stored.replay(epoch.as_ref())) {
                    Ok(result) => result,
                    Err(why) => {
                        eprintln!("aterm-release trust judge: {why}");
                        return 2;
                    }
                };
            let mut verdict = floors.judge(&facts, (RATCHET, MEMORY, None), &failures);
            if let Err(why) =
                check_measured_floors(&git, &facts, floors.ratchet.as_deref(), &mut verdict)
            {
                eprintln!("aterm-release trust judge: {why}");
                return 2;
            }
            print!("{}", verdict.report);
            println!("{credit_line}");
            for why in &failures {
                if !verdict.failed.contains(why) {
                    println!("trust: {why}");
                }
            }
            let p = facts.provenance.unwrap_or_default();
            println!(
                "FACTS: {FACTS_PREFIX}{} (whole-tree run on {}, prover {}), judged against the working {RATCHET}",
                short(&key),
                short(&p.head),
                prover.shown()
            );
            for round in &p.remeasured {
                println!("REMEASUREMENT: {round}");
            }
            if let Err(e) = write_staged_text(&[
                (&path, verdict.ratchet.as_deref()),
                (&backstops_path, verdict.backstops.as_deref()),
            ]) {
                eprintln!("aterm-release trust judge: {e}");
                return 2;
            }
            if verdict.ratchet.is_some() {
                println!("RATCHET: {RATCHET} rewritten from the stored facts");
            }
            if verdict.backstops.is_some() {
                println!("BACKSTOPS: {BACKSTOPS} rewritten from the stored facts");
            }
            if !verdict.memory_rows.is_empty() {
                let written = crate::durable::read_text(&mem)
                    .map(|t| memory_rewritten(&t, &verdict.memory_rows))
                    .map_err(|e| {
                        std::io::Error::new(
                            e.kind(),
                            format!("could not write {}: {e}", mem.display()),
                        )
                    })
                    .and_then(|t| write_staged_text(&[(&mem, Some(&t))]));
                if let Err(e) = written {
                    eprintln!("aterm-release trust judge: {e}");
                    return 2;
                }
                println!("MEMORY RECORD: {MEMORY} kept current from the stored facts");
            }
            if !verdict.tail.is_empty() {
                println!("{}", verdict.tail);
            }
            verdict.exit
        }
    }
}

#[cfg(test)]
#[path = "trust_facts_testing_tests.rs"]
pub(crate) mod testing;

#[cfg(test)]
#[path = "trust_facts_tests.rs"]
mod tests;
