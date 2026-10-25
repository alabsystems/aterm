// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Trust verification lane fact recording, memory admission, ratchet judgment, and CLI verbs.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use aterm_digest::Sha256;

use crate::ledger::{Error, GitRunner, Result, git_ok, rev_parse};
use crate::release::shell_word;

pub const FACTS_MAGIC: &str = "aterm-trust facts 8";

pub const FACTS_END: &str = "end";

pub const FACTS_PREFIX: &str = "trust-facts-";

pub const PLAN_MAGIC: &str = "aterm-trust plan 1";

pub const RATCHET: &str = "crates/aterm-release/trust/floors.tsv";

pub const COSTS: &str = "crates/aterm-release/trust/costs.tsv";

/// Beside the floors: what the run that set each floor stopped ([`floor_run`],
/// [`backstops_rewritten`]).
pub const BACKSTOPS: &str = "crates/aterm-release/trust/backstops.tsv";

const BACKSTOPS_HEADER: &str = "# What a backstop (a clock's, or the RSS reading's) stopped in the run that set each floor in floors.tsv. A crate's rows start with <crate>\t-\tcancelled\t<n>: that run's stops were kept, and the lane cancelled n of its proof checks. Each obligation a backstop stopped in it follows: <crate>\t<key>\t<stop>\t<obligation>. `ship trust run --update-ratchet` writes a crate's rows with its floor. The judge credits a starved run's shortfall only with the stops its floor's run did not have and the cancelled proof checks beyond that run's count, so what every run has is the code's. A crate with no rows was floored before they were kept: what its floor's run stopped is unknown, so every stop and cancelled check is credited.";

/// The head of a crate's [`BACKSTOPS`] rows, before the proof checks its floor's run cancelled.
const RUN_ROW: &str = "-\tcancelled\t";

const KEY_SCHEMA: &str = "aterm-trust facts key 5";

pub const LANE_WHOLE_TREE: &str = "whole-tree";

fn short(sha: &str) -> &str {
    sha.get(..9).unwrap_or(sha)
}

/// Replaces tabs, carriage returns, and newlines in `s` with spaces for single-line records.
pub(crate) fn one_line(s: &str) -> String {
    s.replace(['\t', '\r', '\n'], " ")
}

/// Encodes `bytes` as lowercase hexadecimal.
pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

pub const CHECK_CANCELLED: &str = "proof validation cancelled by the caller";

/// Raw verification metrics parsed from a single crate's `targo trust check` log.
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
}

/// How a check ran: its CPU and wall time, the host's mean one-minute load while it ran and
/// the host's cores. The prover's clocks are wall clocks, so a check that got too little CPU
/// lapses more ([`Pace::starved`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pace {
    pub cpu_ms: u64,
    pub wall_ms: u64,
    /// The mean one-minute load average, in hundredths; `None` when it could not be read.
    pub load: Option<u64>,
    pub cores: u64,
}

impl Pace {
    /// `<cpu ms> <wall ms> <load in hundredths, or -> <cores>`: the lane's `.pace` mark and the
    /// facts' field.
    #[must_use]
    pub fn text(&self) -> String {
        format!(
            "{} {} {} {}",
            self.cpu_ms,
            self.wall_ms,
            self.load.map_or_else(|| "-".into(), |l| l.to_string()),
            self.cores
        )
    }

    /// Reads [`Pace::text`].
    #[must_use]
    pub fn parse(s: &str) -> Option<Pace> {
        let [cpu, wall, load, cores] = s.split(' ').collect::<Vec<_>>()[..] else {
            return None;
        };
        Some(Pace {
            cpu_ms: cpu.parse().ok()?,
            wall_ms: wall.parse().ok()?,
            load: match load {
                "-" => None,
                l => Some(l.parse().ok()?),
            },
            cores: cores.parse().ok()?,
        })
    }

    /// Starved: its CPU was under 0.8 of its wall time, or the load was above the cores.
    #[must_use]
    pub fn starved(&self) -> bool {
        (self.wall_ms > 0 && self.cpu_ms.saturating_mul(10) < self.wall_ms.saturating_mul(8))
            || self
                .load
                .is_some_and(|l| l > self.cores.saturating_mul(100))
    }

    /// `CPU/wall 0.40, load 30.00 on 14 cores`.
    #[must_use]
    pub fn said(&self) -> String {
        let hundredths = |n: u64| format!("{}.{:02}", n / 100, n % 100);
        let ratio = self
            .cpu_ms
            .saturating_mul(100)
            .saturating_add(self.wall_ms / 2)
            .checked_div(self.wall_ms)
            .map_or_else(|| "?".to_string(), hundredths);
        let load = self.load.map_or_else(|| "unread".to_string(), hundredths);
        format!("CPU/wall {ratio}, load {load} on {} cores", self.cores)
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
            one_line(&self.key),
            one_line(&self.stop),
            one_line(&self.said)
        )
    }

    fn from_fields(v: &str) -> Option<Backstop> {
        let [key, stop, said] = v.splitn(3, '\t').collect::<Vec<_>>()[..] else {
            return None;
        };
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

fn octal_mode(s: &str) -> Option<u32> {
    let r = s.strip_prefix("0o")?;
    let n = r.bytes().take_while(|b| (b'0'..=b'7').contains(b)).count();
    u32::from_str_radix(&r[..n], 8).ok()
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

fn without_ansi(line: &str) -> String {
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
    out
}

fn path_refusals(line: &str) -> Vec<PathRefusal> {
    if !line.contains("writable") && !line.contains("owned by uid=") && !line.contains("must be ") {
        return Vec::new();
    }
    let line = without_ansi(line);
    let line = line.as_str();
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

fn find_all(hay: &str, needle: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(i) = hay.get(from..).and_then(|s| s.find(needle)) {
        out.push(from + i);
        from += i + needle.len().max(1);
    }
    out
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

/// Extracts `(proved, total)` counts from a `Summary: <n> proved ... (<n> total)` log line.
#[must_use]
pub fn parse_summary(line: &str) -> (Option<u64>, Option<u64>) {
    let proved = find_all(line, "Summary: ").into_iter().rev().find_map(|i| {
        let d = digits_at(line, i + "Summary: ".len());
        let after = i + "Summary: ".len() + d.len();
        (!d.is_empty() && line.get(after..).is_some_and(|r| r.starts_with(" proved")))
            .then(|| d.parse().ok())
            .flatten()
    });
    let total = find_all(line, "(").into_iter().rev().find_map(|i| {
        let d = digits_at(line, i + 1);
        let after = i + 1 + d.len();
        (!d.is_empty() && line.get(after..).is_some_and(|r| r.starts_with(" total)")))
            .then(|| d.parse().ok())
            .flatten()
    });
    (proved, total)
}

fn census_skipped(line: &str) -> Option<u64> {
    let census = line.find("memory census:")? + "memory census:".len();
    find_all(line, " bodies skipped at the memory ceiling")
        .into_iter()
        .rev()
        .find_map(|j| {
            let start = digits_start_before(line, j);
            (start < j && start > census)
                .then(|| line[start..j].parse().ok())
                .flatten()
        })
}

fn census_figure(line: &str, key: &str) -> Option<String> {
    let census = line.find("memory census:")? + "memory census:".len();
    find_all(line, key).into_iter().rev().find_map(|k| {
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
        .into_iter()
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

fn compiler_signal(line: &str) -> Option<(String, u32, String)> {
    const SAID: &str = "exit successfully: `";
    const SIGNAL: &str = "(signal: ";
    let rest = &line[line.find(SAID)? + SAID.len()..];
    let program = rest.split([' ', '`']).next().unwrap_or("");
    let program = program.rsplit('/').next().unwrap_or(program);
    let s = rest.rfind(SIGNAL)? + SIGNAL.len();
    let d = digits_at(rest, s);
    let n = d.parse().ok()?;
    let name = rest[s + d.len()..]
        .strip_prefix(", ")
        .and_then(|r| r.split([':', ')']).next())
        .unwrap_or("")
        .trim();
    Some((program.to_string(), n, name.to_string()))
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

/// The count a `Stop causes:` line gives `cause` in its first clause (`rss-backstop=715`), before
/// the clauses that name which lane's backstops fired.
fn stop_count(causes: &str, cause: &str) -> u64 {
    causes
        .split(';')
        .next()
        .unwrap_or("")
        .split_whitespace()
        .find_map(|w| w.strip_prefix(cause)?.strip_prefix('=')?.parse().ok())
        .unwrap_or(0)
}

fn coverage_complete(line: &str) -> bool {
    let Some(rest) = line.trim().strip_prefix("Coverage: ") else {
        return false;
    };
    let Some((counts, words)) = rest.split_once(' ') else {
        return false;
    };
    let whole = words == COVERAGE_COMPLETE
        || (words.starts_with(COVERAGE_ACCOUNTED) && words.ends_with(") (complete)"));
    whole
        && matches!(counts.split_once('/'), Some((p, e))
            if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) && p == e)
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
    /// Parses a crate's `targo trust check` log and `.rc` exit-code file into [`Measured`] facts.
    #[must_use]
    pub fn of_log(log: impl BufRead, rc: &str) -> Measured {
        const ADMISSION: &str = "memory admission failed";
        const BUDGET: &str = "exceeded the per-function verification budget";
        let mut m = Measured {
            rc: rc.to_string(),
            ..Measured::default()
        };
        let (mut listed, mut named): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
        let mut ceiling_bytes: Option<u64> = None;
        let mut peak_gib: Option<f64> = None;
        let (mut launch_refused, mut dirty, mut owned, mut unready) = (false, false, false, false);
        let mut cut: BTreeMap<String, String> = BTreeMap::new();
        let mut signalled: Option<(String, u32, String)> = None;
        let mut coverage: Option<String> = None;
        let mut refused_paths: BTreeSet<PathRefusal> = BTreeSet::new();
        let mut buf = Vec::new();
        let mut log = log;
        loop {
            buf.clear();
            match log.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let line = String::from_utf8_lossy(&buf);
            let line = line.trim_end_matches(['\n', '\r']);
            if line.contains("verification did not run") || line.contains("transport:missing-json")
            {
                m.transport = true;
            }
            launch_refused |= line.contains(TRUSTD_REFUSED);
            dirty |= line.contains(TRUSTD_DIRTY);
            owned |= line.contains("another trustd owns");
            unready |= line.contains("did not establish exact-identity readiness");
            let lead = line.trim_start();
            if lead.starts_with("Summary:") {
                m.summary = one_line(line);
            }
            if lead.starts_with("Coverage:") {
                coverage = Some(one_line(lead));
            }
            if let Some(causes) = lead.strip_prefix("Stop causes:") {
                m.rss = stop_count(causes, "rss-backstop");
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
                    listed.push(said.clone());
                }
                named.push(said);
            }
            for i in find_all(line, "timed_out=") {
                let d = digits_at(line, i + "timed_out=".len());
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
            for i in find_all(line, "trust-verify-memory-ceiling-bytes") {
                let at = i + "trust-verify-memory-ceiling-bytes".len();
                if matches!(line.as_bytes().get(at), Some(b'=' | b' ')) {
                    let d = digits_at(line, at + 1);
                    if let Ok(b) = d.parse() {
                        ceiling_bytes = Some(b);
                    }
                }
            }
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
        if let Some((program, n, name)) = &signalled {
            ended.push(format!(
                "cargo: {program} ended by signal {n}{}",
                signal_said(name)
            ));
        }
        if let Some(n) = rc
            .trim()
            .parse::<u32>()
            .ok()
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
        m
    }

    fn to_fields(&self) -> String {
        [
            one_line(&self.rc),
            u8::from(self.transport).to_string(),
            self.refused.to_string(),
            self.lapsed.to_string(),
            self.cancelled.to_string(),
            self.budget_rows.to_string(),
            self.skipped.map_or_else(|| "-".into(), |n| n.to_string()),
            one_line(&self.ceiling),
            one_line(&self.peak),
            one_line(&self.launch),
            one_line(&self.held),
            one_line(&self.held_what),
            one_line(&self.refused_why),
            one_line(&self.wall_budget),
            one_line(&self.ended),
            self.rss.to_string(),
            self.pace.map_or_else(|| "-".into(), |p| p.text()),
            self.backstops
                .as_ref()
                .map_or_else(|| "-".into(), |b| b.len().to_string()),
            one_line(&self.summary),
        ]
        .join("\t")
    }

    /// The fields of [`Measured::to_fields`], and how many `backstop` lines follow.
    fn from_fields(f: &[&str]) -> Option<(Measured, Option<usize>)> {
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
            n => Some(n.parse().ok()?),
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
            refused: refused.parse().ok()?,
            lapsed: lapsed.parse().ok()?,
            cancelled: cancelled.parse().ok()?,
            budget_rows: budget_rows.parse().ok()?,
            skipped: match *skipped {
                "-" => None,
                n => Some(n.parse().ok()?),
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
            rss: rss.parse().ok()?,
            pace,
            backstops: backstops.map(Vec::with_capacity),
        };
        Some((m, backstops))
    }
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
    let v: f64 = v.parse().ok()?;
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
    (8.0 * x + 1e-9).trunc() as u64 + 1
}

/// Host RAM and unknown-process state sampled at the start of a verification run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Host {
    pub mem: u64,
    pub memtotal: bool,
}

impl Host {
    #[allow(clippy::cast_precision_loss)]
    fn ram_gib(&self) -> String {
        let g = self.mem as f64 / GIB;
        if (g - g.trunc()).abs() < f64::EPSILON {
            format!("{g:.0}")
        } else {
            format!("{g:.1}")
        }
    }

    /// Formats the per-verifier memory ceiling (`RAM / 8` in GiB to one decimal place).
    #[allow(clippy::cast_precision_loss)]
    #[must_use]
    pub fn ceiling(&self) -> Option<String> {
        (self.mem > 0).then(|| one((self.mem / 8) as f64 / GIB))
    }

    fn desc(&self) -> String {
        match self.ceiling() {
            Some(c) => format!("{} GiB RAM, verifier ceiling {c} GiB", self.ram_gib()),
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
        [
            &self.f,
            &self.ram,
            &self.at,
            &self.need,
            &self.bound,
            &self.when,
        ]
        .map(|v| one_line(v))
        .join("\t")
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

fn tab_fields(line: &str, n: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut rest = line.trim_matches('\t');
    while out.len() + 1 < n && !rest.is_empty() {
        let (f, r) = rest.split_once('\t').unwrap_or((rest, ""));
        out.push(f.to_string());
        rest = r.trim_start_matches('\t');
    }
    if !rest.is_empty() {
        out.push(rest.to_string());
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
        let Some(lib) = f.first().filter(|n| !n.starts_with('#')) else {
            continue;
        };
        let col = |i: usize| f.get(i).map_or("", String::as_str);
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
        let rows = out.entry(lib.clone()).or_default();
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
        }
    }
}

/// Host memory-exhaustion observation attached to an up-front memory [`Refusal`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seen {
    pub outcome: String,
    pub ceil: String,
    pub peak: String,
    pub when: String,
}

impl Seen {
    fn fields(&self) -> String {
        [&self.outcome, &self.ceil, &self.peak, &self.when]
            .map(|v| one_line(v))
            .join("\t")
    }

    fn from_fields(f: &[&str]) -> Option<Option<Seen>> {
        let [outcome, ceil, peak, when] = f else {
            return None;
        };
        Some((*outcome != "-").then(|| Seen {
            outcome: (*outcome).into(),
            ceil: (*ceil).into(),
            peak: (*peak).into(),
            when: (*when).into(),
        }))
    }
}

/// Parses valid [`HostObs`] entries from the host's local observation file.
#[must_use]
pub fn parse_host_obs(text: &str) -> Vec<HostObs> {
    text.lines()
        .filter_map(|line| {
            let f = tab_fields(line, 8);
            let col = |i: usize| f.get(i).cloned().unwrap_or_default();
            let spec = col(0);
            (!spec.is_empty() && !spec.starts_with('#')).then_some(())?;
            let o = HostObs {
                spec,
                outcome: col(1),
                ceil: col(2),
                peak: col(3),
                proved: col(4),
                floor: col(5),
                prover: col(6),
                when: col(7),
            };
            (matches!(o.outcome.as_str(), "held" | "cap-bound")
                && gib(&o.ceil).is_some()
                && !o.floor.is_empty()
                && o.floor.bytes().all(|b| b.is_ascii_digit()))
            .then_some(o)
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
            let name = f.first().cloned().unwrap_or_default();
            let fields: Vec<&str> = f.iter().skip(1).map(String::as_str).collect();
            (name, MemRow::from_fields(&fields), l.to_string())
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
    let key = |(n, r, l): &(String, Option<MemRow>, String)| {
        let ram = r.as_ref().and_then(|r| gib(&r.ram)).unwrap_or(-1.0);
        (n.clone(), ram, l.clone())
    };
    data.sort_by(|a, b| {
        let (ka, kb) = (key(a), key(b));
        ka.0.cmp(&kb.0)
            .then(ka.1.total_cmp(&kb.1))
            .then(ka.2.cmp(&kb.2))
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
    pub vanished: Vec<String>,
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

    /// Serializes [`Facts`] into its canonical `aterm-trust-facts 1` text record.
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
                out.push_str(&format!("{k} {}\n", one_line(v)));
            }
            for r in &p.remeasured {
                out.push_str(&format!("remeasured {}\n", one_line(r)));
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
            out.push_str(&format!("{k} {}\n", one_line(v)));
        }
        out.push_str(&format!("host-mem {}\n", self.host.mem));
        out.push_str(&format!(
            "host-mem-os {}\n",
            if self.host.memtotal { "MemTotal" } else { "-" }
        ));
        out.push_str(&format!("measured-at {}\n", one_line(&self.measured_at)));
        for v in &self.vanished {
            out.push_str(&format!("vanished {}\n", one_line(v)));
        }
        for (n, a, b) in &self.ambiguous {
            out.push_str(&format!(
                "ambiguous {}\t{}\t{}\n",
                one_line(n),
                one_line(a),
                one_line(b)
            ));
        }
        for c in &self.crates {
            out.push_str(&format!(
                "crate {}\t{}\t{}",
                one_line(&c.name),
                one_line(&c.decision.word()),
                c.memory.fields()
            ));
            match &c.decision {
                Decision::Gate(m) => {
                    out.push('\t');
                    out.push_str(&m.to_fields());
                }
                Decision::Memory(seen) => {
                    out.push('\t');
                    out.push_str(
                        &seen
                            .as_ref()
                            .map_or_else(|| ["-"; 4].join("\t"), Seen::fields),
                    );
                }
                _ => {}
            }
            out.push('\n');
            if let Decision::Gate(Measured {
                backstops: Some(b), ..
            }) = &c.decision
            {
                for b in b {
                    out.push_str(&format!("backstop {}\t{}\n", one_line(&c.name), b.fields()));
                }
            }
        }
        out.push_str(FACTS_END);
        out.push('\n');
        out
    }

    /// Parses a canonical `aterm-trust-facts 1` text record back into [`Facts`].
    #[must_use]
    pub fn parse(text: &str) -> Option<Facts> {
        let mut lines = text.lines();
        if lines.next() != Some(FACTS_MAGIC) {
            return None;
        }
        let mut f = Facts::default();
        let mut p = Provenance::default();
        let mut has_provenance = false;
        let mut ended = false;
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
                "host-mem" => f.host.mem = v.parse().ok()?,
                "host-mem-os" => {
                    f.host.memtotal = match v {
                        "MemTotal" => true,
                        "-" => false,
                        _ => return None,
                    };
                }
                "measured-at" => f.measured_at = v.to_string(),
                "vanished" => f.vanished.push(v.to_string()),
                "ambiguous" => {
                    let [n, a, b] = v.split('\t').collect::<Vec<_>>()[..] else {
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
                            let (m, n) = Measured::from_fields(rest)?;
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

struct Floor {
    proved: String,
    total: String,
    driver: String,
    why: String,
}

fn floor_of(ratchet: Option<&str>, name: &str) -> Option<Floor> {
    let row = ratchet?
        .lines()
        .find(|l| l.split('\t').next() == Some(name))?;
    let f: Vec<&str> = row.split('\t').collect();
    let col = |i: usize| {
        f.get(i)
            .map_or("", |s| s.trim_end_matches('\t'))
            .to_string()
    };
    Some(Floor {
        proved: col(1),
        total: col(2),
        driver: col(3),
        why: col(4),
    })
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

/// What `backstops` (the text of [`BACKSTOPS`]) keeps of the run that set `name`'s floor;
/// `None` when it kept nothing of that run (no head row, or a head row that does not read), so
/// what that run stopped is unknown.
#[must_use]
pub fn floor_run(backstops: Option<&str>, name: &str) -> Option<FloorRun> {
    let mut cancelled = None;
    let mut stops = Vec::new();
    for rest in backstops
        .unwrap_or("")
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.strip_prefix(name)?.strip_prefix('\t'))
    {
        match rest.strip_prefix(RUN_ROW) {
            Some(n) => cancelled = Some(n.parse().ok()?),
            None => stops.extend(Backstop::from_fields(rest)),
        }
    }
    cancelled.map(|cancelled| FloorRun { stops, cancelled })
}

/// `backstops` (the text of [`BACKSTOPS`]) with `name`'s rows replaced by `run`'s, its head row
/// first, sorted.
#[must_use]
pub fn backstops_rewritten(backstops: Option<&str>, name: &str, run: &FloorRun) -> String {
    let text = backstops.unwrap_or("");
    let mut out: String = text
        .lines()
        .take_while(|l| l.starts_with('#'))
        .map(|l| format!("{l}\n"))
        .collect();
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
    rows.sort();
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
#[allow(clippy::too_many_lines)]
pub fn judge(
    facts: &Facts,
    ratchet: Option<&str>,
    backstops: Option<&str>,
    ratchet_name: &str,
    memory_name: &str,
    update: Update<'_>,
) -> Verdict {
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
    for v in &facts.vanished {
        rows.push([v.clone(), "FORK-VANISHED".into(), dash(), dash()]);
        failed.push(format!("{v} FORK-VANISHED"));
        fail = true;
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
    let narrowed = !facts.only.is_empty();
    let (mut queued, mut gated) = (0usize, 0usize);
    let (mut queued_what, mut skipped_what) = (String::new(), String::new());
    let (mut mem_what, mut memory) = (String::new(), Vec::new());
    let (mut mem_ram, mut mem_ram_unknown) = (None::<u64>, false);
    let mut note_ram = |sure: Option<u64>| match sure {
        None => mem_ram_unknown = true,
        Some(r) => mem_ram = Some(mem_ram.map_or(r, |m: u64| m.max(r))),
    };
    for c in &facts.crates {
        match &c.decision {
            Decision::Queued(_) => {
                queued += 1;
                queued_what.push_str(&format!(" {}", c.name));
                skipped_what.push_str(&format!(" {} (queued fork)", c.name));
            }
            Decision::Memory(seen) => {
                mem_what.push_str(&format!(" {}", c.name));
                skipped_what.push_str(&format!(" {} (memory)", c.name));
                let r = memory_refusal(&host, &c.name, true, &c.memory, seen.as_ref())
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
                note_ram(r.sure);
            }
            Decision::Gate(_) => gated += 1,
            Decision::NoLib | Decision::Ambiguous => {}
        }
    }
    let (mut total_proved, mut total_all, mut capbound_n) = (0u64, 0u64, 0usize);
    let mut unfinished_n = 0usize;
    let (mut by_trustd, mut by_clock) = (Vec::<String>::new(), Vec::<String>::new());
    let (mut trustd_dirty, mut trustd_block) = (false, None::<(String, String)>);
    let mut ratchet_now: Option<String> = ratchet.map(str::to_string);
    let mut backstops_now: Option<String> = backstops.map(str::to_string);
    let mut rewrote = false;
    let mut memory_rows: Vec<(String, MemRow)> = Vec::new();
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
            let secs = m.wall_budget.parse::<u64>().ok();
            let s = format!("OVER-BUDGET({}s)", m.wall_budget);
            failed.push(format!("{name} {s}"));
            rows.push([name.into(), s, dash(), dash()]);
            unfinished_n += 1;
            skipped_what.push_str(&format!(" {name} (over its wall budget)"));
            notes.push(format!(
                "OVER BUDGET: {name}: still verifying when its per-crate wall budget of {} ran out, so the lane ended its check there and nothing was decided about it — a verification that does not finish is not verified; give it longer with --crate-budget <seconds>, or find what holds it (its log, up to where it was ended)",
                secs.map_or_else(
                    || "(unnamed)".to_string(),
                    |s| if s < 60 {
                        format!("{s} s")
                    } else {
                        format!("{s} s ({})", span(s))
                    }
                )
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
            skipped_what.push_str(&format!(" {name} (trustd did not admit it)"));
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
                "NOT VERIFIED: {name}: targo would not start its check — {cause}; the socket then held {held}"
            ));
            continue;
        }
        if m.transport {
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
            unfinished_n += 1;
            skipped_what.push_str(&format!(" {name} (did not finish)"));
            notes.push(format!(
                "NOT VERIFIED: {name}: its verification did not finish ({}) — {counted}, so nothing was decided about it; re-run it",
                m.ended
            ));
            fail = true;
            continue;
        }
        if m.summary.is_empty() {
            let s = format!("GATE-BROKE(rc={})", m.rc);
            failed.push(format!("{name} {s}"));
            rows.push([name.into(), s, dash(), dash()]);
            fail = true;
            continue;
        }
        let (Some(proved), Some(all)) = parse_summary(&m.summary) else {
            failed.push(format!("{name} SUMMARY-UNPARSED"));
            rows.push([name.into(), "SUMMARY-UNPARSED".into(), dash(), dash()]);
            fail = true;
            continue;
        };
        let row = floor_of(ratchet_now.as_deref(), name);
        let floor: Option<u64> = match &row {
            None => None,
            Some(r) if r.proved.is_empty() => None,
            Some(r) => match r.proved.parse() {
                Ok(n) => Some(n),
                Err(_) => {
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
        let credit = credit(m, floor_run(backstops_now.as_deref(), name));
        let starved = m.pace.filter(Pace::starved);
        // What the machine is credited with: nothing in a run that was not starved.
        let clock = if starved.is_some() { credit.n } else { 0 };
        // Why this run sets no floor, whatever it proved: a starved run's count stands only when
        // nothing in it is the machine's.
        let run_withhold = if let Some(p) = starved.filter(|_| credit.n > 0) {
            Some(format!(
                "the run was starved ({}), and {}, so a run that was not starved need not prove what it did; measure it again when the machine is quieter: {}",
                p.said(),
                credit.said(),
                measure_again(facts, &[name])
            ))
        } else if m.backstops.is_none() && m.lapsed.saturating_add(m.rss) > 0 {
            Some(format!(
                "a backstop stopped {} obligation(s), and the run kept no obligation record to name them beside its floor (the lane writes it beside the log as <name@version>.log{}, or says why it could not in <name@version>.log.oblig.err); re-base it from a run that keeps one",
                m.lapsed.saturating_add(m.rss),
                crate::trust_oblig::RECORD_SUFFIX,
            ))
        } else {
            None
        };
        let mem = &c.memory;
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
                    skipped_what.push_str(&format!(" {name} (trustd did not admit it)"));
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
            mem_what.push_str(&format!(" {name}"));
            skipped_what.push_str(&format!(" {name} (cap-bound)"));
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
                && let Some(r) = mem_learn(mem, &mem.f, &cen, today, &host)
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
        if let Some(f) = floor.filter(|f| proved < *f && proved.saturating_add(clock) >= *f)
            && let Some(pace) = starved
        {
            rows.push([
                name.into(),
                format!("NOT-VERIFIED(clock,<{f},{clock})"),
                dash(),
                dash(),
            ]);
            by_clock.push(name.to_string());
            skipped_what.push_str(&format!(" {name} (starved)"));
            notes.push(format!(
                "BUDGET LAPSED: {name}: {proved} proved, floor {f}; in a starved run ({}), {} — undecided; measure it again when the machine is quieter: {}",
                pace.said(),
                credit.said(),
                measure_again(facts, &[name])
            ));
            continue;
        }
        total_proved = total_proved.saturating_add(proved);
        total_all = total_all.saturating_add(all);
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
                if !own_floor {
                    notes.push(format!(
                        "DRIVER CHANGED: {name}: its floor of {f} was recorded under {floor_driver}; this run's prover is {driver_row}, and proved {proved}: fix what it no longer proves, or lower the row onto it with a reason (`targo --unverified ship trust judge --update-ratchet --reason '<why>'`)"
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
            match floor_total.parse::<u64>() {
                Ok(t) if all > t => {
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
                Ok(_) => {}
                Err(_) => {
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
            let mut nf = mem.f.clone();
            let run_c = cen.ceil.clone();
            let lowers = floor.is_some_and(|f| proved < f);
            let reason = update.reason.filter(|_| lowers && !own_floor);
            if lowers && reason.is_none() {
                if !own_floor {
                    notes.push(format!(
                        "NOT LOWERED: {name}: {proved} proved stays below the floor of {} another prover recorded — --update-ratchet lowers no floor; it goes down onto this prover only with its reason: `targo --unverified ship trust judge --update-ratchet --reason '<why>'`",
                        floor.unwrap_or(0)
                    ));
                }
            } else {
                let withhold = run_withhold.or_else(|| match (floor, run_c.as_deref()) {
                    (Some(f), _) if proved >= f || mem.f == "-" => None,
                    (None, _) => None,
                    (Some(_), None) => Some(format!(
                        "this run's log names no verifier ceiling (a seal from trust main has none), and its floor was set at {} GiB; re-base it from a run whose log names its ceiling",
                        mem.f
                    )),
                    (Some(_), Some(rc))
                        if !gib_le(&mem.f, rc)
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
                            mem.f,
                            ram_for(gib(&mem.f).unwrap_or(0.0))
                        ))
                    }
                    (Some(_), Some(_)) => None,
                });
                if let Some(w) = withhold {
                    notes.push(format!("RE-BASE WITHHELD: {name}: {w}"));
                } else {
                    let mut lines: Vec<String> = ratchet_now
                        .as_deref()
                        .unwrap_or("")
                        .lines()
                        .filter(|l| l.split('\t').next() != Some(name))
                        .map(str::to_string)
                        .collect();
                    lines.push(match reason {
                        Some(why) => {
                            format!("{name}\t{proved}\t{all}\t{driver_row}\t{}", one_line(why))
                        }
                        None => format!("{name}\t{proved}\t{all}\t{driver_row}"),
                    });
                    lines.sort();
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
                        Some(rc) if same && mem.f != "-" => gib_min(&mem.f, &rc),
                        Some(rc) => rc,
                        None if same => mem.f.clone(),
                        None => dash(),
                    };
                }
            }
            if let Some(r) = mem_learn(mem, &nf, &cen, today, &host) {
                memory_rows.push((name.into(), r));
            }
        }
    }

    let unverified: Vec<String> = by_trustd.iter().chain(&by_clock).cloned().collect();
    let me = aterm_uds::peer::our_uid();
    let refused_notes: Vec<String> = refused_paths
        .iter()
        .map(|(r, names)| refused_path_note(r, names, me))
        .collect();
    let memory_n = mem_what.split_whitespace().count();
    let gated_n = gated.saturating_sub(capbound_n + unverified.len() + unfinished_n);
    let mut out = String::from("\n");
    let row_line = |r: &[String; 4]| format!("{:<28} {:<26} {:>8} {:>8}\n", r[0], r[1], r[2], r[3]);
    out.push_str(&row_line(&[
        "CRATE".into(),
        "STATUS".into(),
        "PROVED".into(),
        "TOTAL".into(),
    ]));
    for r in &rows {
        out.push_str(&row_line(r));
    }
    out.push('\n');
    out.push_str(&format!("TOTAL: {total_proved}/{total_all} proved\n"));
    let id = if facts.driver_id.is_empty() {
        "no --version answer"
    } else {
        facts.driver_id.as_str()
    };
    out.push_str(&format!("DRIVER: {} ({id})\n", facts.driver));
    match host.ceiling() {
        Some(c) => out.push_str(&format!(
            "HOST: {} GiB RAM, verifier ceiling {c} GiB per library (targo trust: RAM / 8)\n",
            host.ram_gib()
        )),
        None => out.push_str("HOST: memory unreadable, nothing refused up front\n"),
    }
    if gated > 0 {
        match facts.grants.as_str() {
            "1" => out.push_str(&format!("TRUSTD: held by this run's keeper ({sock})\n")),
            "" if !facts.trustd_seen.is_empty() => {
                let what = facts
                    .trustd_seen
                    .split_once('\t')
                    .map_or(facts.trustd_seen.as_str(), |(_, w)| w);
                out.push_str(&format!(
                    "TRUSTD: never held — the keeper holds only this build's trustd, and {what}\n"
                ));
            }
            "" => out.push_str(&format!(
                "TRUSTD: never held — no trustd granted the keeper's request at {sock}\n"
            )),
            n => out.push_str(&format!(
                "TRUSTD: held by this run's keeper, {n} grants — trustd went away under a live grant and came back ({sock})\n"
            )),
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
    out.push_str(&format!(
        "GATED: {gated_n} of {} selected; not verified:{skipped_shown}\n",
        facts.crates.len()
    ));
    if queued > 0 {
        out.push_str(&format!(
            "SCOPE: {queued} vendored fork(s) QUEUED, not gated, nothing claimed:{queued_what}\n"
        ));
    }
    let roster_missing = facts.vanished.len();
    if total_all > 0 && total_proved == total_all {
        if !narrowed && queued == 0 && !fail && unverified.is_empty() && memory_n == 0 {
            out.push_str("VERDICT: 100% MACHINE-PROVED (every workspace library + every vendored fork, advisory lane: every obligation proved)\n");
        } else if narrowed || queued > 0 || roster_missing > 0 || fail || !unverified.is_empty() {
            out.push_str("VERDICT: 100% PROVED OF WHAT RAN — NOT the workspace claim.\n");
            out.push_str("         Everything gated above is clean; the campaign claim is the\n");
            out.push_str("         WHOLE-TREE run, and this run was narrower:\n");
            if narrowed {
                out.push_str(&format!(
                    "     - -p restricted the run to: {}\n",
                    facts.only
                ));
            }
            if queued > 0 {
                out.push_str(&format!(
                    "     - {queued} vendored fork(s) QUEUED (not converted), nothing is claimed about them:{queued_what}\n"
                ));
                out.push_str("       Queued: aterm-release trust_lane.rs QUEUED_FORKS\n");
            }
            if roster_missing > 0 {
                out.push_str(&format!(
                    "     - {roster_missing} librar(ies) a floor or QUEUED_FORKS names are NOT in the resolved graph (FORK-VANISHED): a floor row goes with its library\n"
                ));
            }
            if fail {
                out.push_str("     - at least one crate failed (the NO-FLOOR / TOTAL-ABOVE-ROW / GATE-BROKE / NOT-VERIFIED / OVER-BUDGET /\n");
                out.push_str("       SUMMARY-UNPARSED / AMBIGUOUS-SPEC / FORK-* rows above); one with no usable Summary, or one that\n");
                out.push_str("       did not finish, is in NEITHER total\n");
            }
            if !by_trustd.is_empty() {
                out.push_str(&format!(
                    "     - trustd did not admit the work, so nothing was decided about: {} (in NEITHER total)\n",
                    by_trustd.join(" ")
                ));
            }
            if !by_clock.is_empty() {
                out.push_str(&format!(
                    "     - in a starved run, what the machine can be credited with (its BUDGET LAPSED note says what) could cover the shortfall, so nothing was decided about: {} (in NEITHER total)\n",
                    by_clock.join(" ")
                ));
            }
            out.push_str("         Re-run the whole tree, with every exclusion lifted, before quoting a whole-tree number.\n");
        }
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
        if !by_clock.is_empty() {
            said.push(format!(
                "in a starved run, what the machine can be credited with (each BUDGET LAPSED note says what) could cover the shortfall, so nothing was decided about: {}; measure them again when the machine is quieter: {}",
                by_clock.join(" "),
                measure_again(facts, &by_clock.iter().map(String::as_str).collect::<Vec<_>>())
            ));
        }
        (
            "COULD-NOT-RUN",
            2,
            format!("VERDICT: COULD NOT RUN — {}", said.join("; and ")),
        )
    } else if memory_n > 0 {
        let ram_all = match (mem_ram, mem_ram_unknown) {
            (None, _) if memory_n == 1 => {
                "no RAM is known to verify it (its floor records no ceiling)".to_string()
            }
            (None, _) => "no RAM is known to verify them (no floor of theirs records its ceiling)"
                .to_string(),
            (Some(r), true) => {
                format!("{r} GiB of RAM verifies those whose floor records its ceiling")
            }
            (Some(r), false) if memory_n == 1 => format!("{r} GiB of RAM verifies it"),
            (Some(r), false) => format!("{r} GiB of RAM verifies them all"),
        };
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
        tail.push_str(&format!(" — REFUSED PATH: {}", said.join("; ")));
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
        if r.proved.parse::<u64>().is_err() {
            out.push(format!("{}: floor `{}` is not a count", r.name, r.proved));
        }
        if r.total.parse::<u64>().is_err() {
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
        let Ok(was_n) = was.proved.parse::<u64>() else {
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
        let Ok(is_n) = is.proved.parse::<u64>() else {
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
        let gone = match (was.total.parse::<u64>(), is.total.parse::<u64>()) {
            (Ok(w), Ok(i)) => Some(w.saturating_sub(i)),
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
    let out = git_ok(git, &["ls-tree", "-r", "-z", "--full-tree", rev])?;
    let mut h = Sha256::new();
    let mut entries = 0usize;
    for entry in out.stdout.split(|b| *b == 0).filter(|e| !e.is_empty()) {
        let path = entry
            .iter()
            .position(|b| *b == b'\t')
            .map_or(&entry[..0], |t| &entry[t + 1..]);
        if [RATCHET, MEMORY, COSTS, BACKSTOPS]
            .iter()
            .any(|f| path == f.as_bytes())
        {
            continue;
        }
        h.update(entry);
        h.update([0u8]);
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

/// Parsed `.plan` file written at the start of a verification lane run.
#[derive(Clone, Debug, Default)]
pub struct Plan {
    pub facts: Facts,
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
                "host-mem" => p.facts.host.mem = v.parse().map_err(|_| bad())?,
                "host-mem-os" => p.facts.host.memtotal = v == "MemTotal",
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
                "vanished" => p.facts.vanished.push(v.into()),
                "ambiguous" => {
                    let [n, a, b] = v.split('\t').collect::<Vec<_>>()[..] else {
                        return Err(bad());
                    };
                    p.facts.ambiguous.push((n.into(), a.into(), b.into()));
                }
                "crate" => {
                    let [name, spec, word] = v.split('\t').collect::<Vec<_>>()[..] else {
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
    pub fn admit(&self) -> std::result::Result<(String, Vec<(String, String)>), String> {
        let shown = self.memory.display().to_string();
        let text = fs::read_to_string(&self.memory)
            .map_err(|_| format!("memory record {shown} is missing"))?;
        let record = parse_memory(&text, &shown)?;
        let ratchet = fs::read_to_string(&self.ratchet).ok();
        let obs = self
            .host_obs
            .as_ref()
            .and_then(|p| fs::read_to_string(p).ok())
            .map(|t| parse_host_obs(&t))
            .unwrap_or_default();
        let driver_row = self.facts.driver_row();
        let (mut lines, mut refused) = (String::new(), Vec::new());
        let none = MemRow::default();
        for c in &self.facts.crates {
            let row = record
                .get(&c.name)
                .and_then(|rows| row_for(rows, &self.facts.host))
                .unwrap_or(&none);
            if record.contains_key(&c.name) {
                lines.push_str(&format!("memory {}\t{}\n", one_line(&c.name), row.fields()));
            }
            if !matches!(c.decision, Decision::Gate(_)) || self.try_refused {
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
                    .map_or_else(|| ["-"; 4].join("\t"), Seen::fields);
                lines.push_str(&format!("refused {}\t{seen_fields}\n", one_line(&c.name)));
                refused.push((c.name.clone(), r.line));
            }
        }
        Ok((lines, refused))
    }

    /// Reads each gated crate's log and its `.rc`, `.trustd`, `.budget`, `.pace` and obligation
    /// record ([`crate::trust_oblig::RECORD_SUFFIX`]) beside it into `self.crates`.
    pub fn measure(&mut self) {
        let read = |p: PathBuf| {
            fs::read_to_string(p)
                .map(|s| s.trim().to_string())
                .unwrap_or_default()
        };
        self.facts.grants = read(self.logdir.join(".trustd-grants"));
        self.facts.trustd_seen = read(self.logdir.join(".trustd-seen"));
        for c in &mut self.facts.crates {
            if let Decision::Gate(m) = &mut c.decision {
                let spec = self
                    .specs
                    .get(&c.name)
                    .map_or(c.name.as_str(), String::as_str);
                let log = self.logdir.join(format!("{spec}.log"));
                let rc = fs::read_to_string(self.logdir.join(format!("{spec}.log.rc")))
                    .map_or_else(|_| "?".to_string(), |s| s.trim().to_string());
                *m = match fs::File::open(&log) {
                    Ok(f) => Measured::of_log(BufReader::new(f), &rc),
                    Err(_) => Measured {
                        rc,
                        ..Measured::default()
                    },
                };
                if !m.launch.is_empty() {
                    let probe = read(self.logdir.join(format!("{spec}.log.trustd")));
                    let (held, what) = probe.split_once('\t').unwrap_or((probe.as_str(), ""));
                    (m.held, m.held_what) = (one_line(held), one_line(what));
                }
                let mark = self.logdir.join(format!("{spec}.log.budget"));
                if mark.exists() {
                    let said = read(mark);
                    let said = said.lines().next().unwrap_or("");
                    m.wall_budget = if !said.is_empty() && said.bytes().all(|b| b.is_ascii_digit())
                    {
                        said.to_string()
                    } else {
                        "?".into()
                    };
                }
                m.pace = Pace::parse(&read(self.logdir.join(format!("{spec}.log.pace"))));
                m.backstops = fs::read_to_string(
                    self.logdir
                        .join(format!("{spec}.log{}", crate::trust_oblig::RECORD_SUFFIX)),
                )
                .ok()
                .and_then(|t| crate::trust_oblig::Record::parse(&t).ok())
                .map(|r| Backstop::of_record(&r));
            }
        }
    }
}

fn cargo_sets(name: &str) -> bool {
    matches!(
        name,
        "CARGO"
            | "CARGO_CRATE_NAME"
            | "CARGO_BIN_NAME"
            | "CARGO_PRIMARY_PACKAGE"
            | "CARGO_TARGET_TMPDIR"
            | "CARGO_RUSTC_CURRENT_DIR"
            | "OUT_DIR"
    ) || ["CARGO_PKG_", "CARGO_MANIFEST_", "CARGO_BIN_EXE_"]
        .iter()
        .any(|p| name.starts_with(p))
}

fn env_dep_refusal(
    name: &str,
    value: Option<&str>,
    lane: &LaneEnv,
    emitted: &BTreeSet<String>,
) -> Option<String> {
    if lane.passed.contains(name) {
        return Some(format!(
            "the lane passes {name} from the host, and no key covers its value"
        ));
    }
    let value = value?;
    if cargo_sets(name) || emitted.contains(name) {
        return None;
    }
    match lane.pins.get(name) {
        Some(pin) if pin == value => None,
        Some(_) => Some(format!(
            "{name} is one of the lane's pins, and the compile read another value"
        )),
        None => Some(format!(
            "{name} was set, and neither cargo, a build script nor the lane sets it"
        )),
    }
}

fn dep_info_check(target_root: &Path, lane: &LaneEnv) -> std::result::Result<(), String> {
    let mut stack: Vec<PathBuf> = fs::read_dir(target_root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            let n = e.file_name();
            let n = n.to_string_lossy();
            n.starts_with('j') && n[1..].bytes().all(|b| b.is_ascii_digit()) && n.len() > 1
        })
        .map(|e| e.path())
        .collect();
    let (mut found, mut named) = (0usize, None::<(PathBuf, &str)>);
    let mut reads: Vec<(String, PathBuf, Option<String>)> = Vec::new();
    let mut emitted: BTreeSet<String> = BTreeSet::new();
    while let Some(dir) = stack.pop() {
        for e in fs::read_dir(&dir).into_iter().flatten().flatten() {
            let Ok(ft) = e.file_type() else { continue };
            let p = e.path();
            if ft.is_dir() {
                stack.push(p);
            } else if ft.is_file() && p.extension().is_some_and(|x| x == "d") {
                let Ok(text) = fs::read(&p) else { continue };
                for floors in [RATCHET, MEMORY, BACKSTOPS] {
                    let file = floors.rsplit('/').next().unwrap_or(floors).as_bytes();
                    if text.windows(file.len()).any(|w| w == file) {
                        named = Some((p.clone(), floors));
                    }
                }
                for line in text.split(|b| *b == b'\n') {
                    let Some(read) = line.strip_prefix(b"# env-dep:".as_slice()) else {
                        continue;
                    };
                    let read = String::from_utf8_lossy(read);
                    let read = read.trim_end_matches('\r');
                    let (name, value) = match read.split_once('=') {
                        Some((n, v)) => (n.to_string(), Some(v.to_string())),
                        None => (read.to_string(), None),
                    };
                    reads.push((name, p.clone(), value));
                }
                found += 1;
            } else if ft.is_file()
                && e.file_name() == "output"
                && p.parent()
                    .and_then(Path::parent)
                    .and_then(Path::file_name)
                    .is_some_and(|n| n == "build")
            {
                let Ok(text) = fs::read_to_string(&p) else {
                    continue;
                };
                for line in text.lines() {
                    let Some(set) = line
                        .strip_prefix("cargo::rustc-env=")
                        .or_else(|| line.strip_prefix("cargo:rustc-env="))
                    else {
                        continue;
                    };
                    if let Some((name, _)) = set.split_once('=') {
                        emitted.insert(name.to_string());
                    }
                }
            }
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
    reads.sort();
    let refused: Vec<(String, PathBuf, String)> = reads
        .into_iter()
        .filter_map(|(name, file, value)| {
            env_dep_refusal(&name, value.as_deref(), lane, &emitted).map(|why| (name, file, why))
        })
        .collect();
    if let Some((name, file, why)) = refused.first() {
        let more = refused
            .iter()
            .map(|(n, _, _)| n)
            .collect::<BTreeSet<_>>()
            .len()
            - 1;
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
    if again.host != base.host {
        return Err(format!(
            "the stored record was measured on a host with {} bytes of memory, and this run on one with {}",
            base.host.mem, again.host.mem
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
    if let Some(p) = out.provenance.as_mut() {
        p.remeasured.push(format!(
            "{} {} {}",
            a.date,
            a.head,
            if names.is_empty() {
                "nothing".to_string()
            } else {
                names.join(" ")
            }
        ));
    }
    Ok(out)
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
    if (gated || !plan.remeasure)
        && let Err(why) = dep_info_check(&file.target_root, &file.lane_env)
    {
        return Err(why);
    }
    let tsr = tree_sans_floors(git, &file.commit)
        .map_err(|e| format!("git could not list the tree the key covers ({e})"))?;
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

/// Writes the floor files a judge rewrote (each `Some` text to its path): every one beside
/// itself first, then each moved into place, so a write that fails (a full disk) leaves them all
/// as they were, and a new floor never stands beside the old floor's stops for want of room.
///
/// # Errors
/// Names the file it could not write or move; what it staged is removed.
fn write_floor_files(files: &[(&Path, Option<&str>)]) -> std::result::Result<(), String> {
    let files: Vec<(&Path, &str, PathBuf)> = files
        .iter()
        .filter_map(|&(path, text)| {
            let staged = path.with_extension(format!("tsv.{}", std::process::id()));
            Some((path, text?, staged))
        })
        .collect();
    let done = files
        .iter()
        .try_for_each(|(path, text, staged)| {
            fs::write(staged, text).map_err(|e| {
                format!(
                    "could not write {} (for {}): {e}",
                    staged.display(),
                    path.display()
                )
            })
        })
        .and_then(|()| {
            files.iter().try_for_each(|(path, _, staged)| {
                fs::rename(staged, path).map_err(|e| {
                    format!(
                        "could not move {} onto {}: {e}",
                        staged.display(),
                        path.display()
                    )
                })
            })
        });
    if done.is_err() {
        for (_, _, staged) in &files {
            let _ = fs::remove_file(staged);
        }
    }
    done
}

/// Measures completed logs, judges facts against the ratchet, and files the resulting record.
pub fn conclude(
    git: &dyn GitRunner,
    repo: &Path,
    plan: &mut Plan,
    update: Update<'_>,
) -> Result<Concluded> {
    plan.measure();
    let filed = plan.file.as_ref().map(|file| filing(git, repo, plan, file));
    let mut facts = plan.facts.clone();
    let mut why_not: Option<String> = None;
    if let Some(Ok(prov)) = &filed {
        facts.provenance = Some(prov.clone());
        if plan.remeasure {
            let store = plan
                .file
                .as_ref()
                .map(|f| f.store.clone())
                .unwrap_or_default();
            let stored = store.join(format!("{FACTS_PREFIX}{}", prov.key));
            let base = fs::read_to_string(&stored)
                .ok()
                .and_then(|t| Facts::parse(&t));
            match base.map(|b| remeasured(&b, &facts)) {
                Some(Ok(done)) => facts = done,
                Some(Err(why)) => why_not = Some(why),
                None => {
                    why_not = Some(format!(
                        "--remeasure found no whole record under this run's key ({}) to complete",
                        short(&prov.key)
                    ));
                }
            }
            if why_not.is_some() {
                facts.provenance = None;
            }
        }
    }
    let ratchet_text = fs::read_to_string(&plan.ratchet).ok();
    let backstops_path = backstops_beside(&plan.ratchet);
    let backstops_text = fs::read_to_string(&backstops_path).ok();
    let mut verdict = judge(
        &facts,
        ratchet_text.as_deref(),
        backstops_text.as_deref(),
        &plan.ratchet.display().to_string(),
        &plan.memory.display().to_string(),
        update,
    );
    if plan.remeasure {
        let ran: BTreeSet<&str> = plan.facts.crates.iter().map(|c| c.name.as_str()).collect();
        verdict
            .memory_rows
            .retain(|(n, _)| ran.contains(n.as_str()));
        verdict.host_obs.retain(|(n, _)| ran.contains(n.as_str()));
    }
    write_floor_files(&[
        (&plan.ratchet, verdict.ratchet.as_deref()),
        (&backstops_path, verdict.backstops.as_deref()),
    ])
    .map_err(Error::new)?;
    let mut said: Vec<String> = Vec::new();
    if !verdict.memory_rows.is_empty() {
        let written = fs::read_to_string(&plan.memory)
            .map(|t| memory_rewritten(&t, &verdict.memory_rows))
            .and_then(|t| fs::write(&plan.memory, t));
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
    let none = |why: &str| {
        Some(format!(
            "FACTS: none — {why}; filing requires a whole-tree run on a clean checkout"
        ))
    };
    let facts_line = match (&filed, &plan.file) {
        (None, _) | (_, None) => None,
        (Some(Err(why)), _) => none(why),
        (Some(Ok(_)), _) if why_not.is_some() => none(why_not.as_deref().unwrap_or_default()),
        (Some(Ok(prov)), Some(file)) => match file_record(&file.store, &facts) {
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
        },
    };
    Ok(Concluded {
        verdict,
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
    let old = fs::read_to_string(path).ok();
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
    rows.sort();
    let mut text = format!("{HOST_OBS_HEADER}\n");
    for r in rows {
        text.push_str(&r);
        text.push('\n');
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("tsv.{}", std::process::id()));
    fs::write(&tmp, text)
        .and_then(|()| fs::rename(&tmp, path))
        .inspect_err(|_| {
            let _ = fs::remove_file(&tmp);
        })
}

/// Atomically writes `facts` into `store` under its provenance key.
pub fn file_record(store: &Path, facts: &Facts) -> std::result::Result<PathBuf, String> {
    let key = facts
        .provenance
        .as_ref()
        .map(|p| p.key.clone())
        .filter(|k| !k.is_empty())
        .ok_or("a record with no key cannot be filed")?;
    let path = store.join(format!("{FACTS_PREFIX}{key}"));
    let tmp = store.join(format!(".{FACTS_PREFIX}{key}.{}", std::process::id()));
    fs::create_dir_all(store)
        .and_then(|()| fs::write(&tmp, facts.to_record()))
        .and_then(|()| fs::rename(&tmp, &path))
        .map_err(|e| {
            let _ = fs::remove_file(&tmp);
            format!("could not write {}: {e}", path.display())
        })?;
    Ok(path)
}

/// Extracts the `commit-hash` reported by `<bin>/trustc -vV`, or `"unknown"`.
#[must_use]
pub fn trustc_commit(bin: &Path) -> String {
    std::process::Command::new(bin.join("trustc"))
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
    let listed = git_ok(git, &["ls-tree", commit, "--", path])?.stdout_utf8();
    if listed.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(
        git_ok(git, &["cat-file", "blob", &format!("{commit}:{path}")])?.stdout_utf8(),
    ))
}

/// Checks whether `judged` lowers any ratchet floor relative to when `facts` were measured.
pub fn lowered_since_measured(
    git: &dyn GitRunner,
    facts: &Facts,
    judged: Option<&str>,
) -> std::result::Result<Vec<String>, String> {
    let head = facts
        .provenance
        .as_ref()
        .map(|p| p.head.as_str())
        .filter(|h| !h.is_empty())
        .ok_or("the facts name no commit they were measured on")?;
    let measured = ratchet_at(git, head)
        .map_err(|e| format!("git cannot read {RATCHET} of {} ({e})", short(head)))?;
    let gated: BTreeSet<&str> = facts
        .crates
        .iter()
        .filter(|c| matches!(c.decision, Decision::Gate(_)))
        .map(|c| c.name.as_str())
        .collect();
    let driver = facts.driver_row();
    let driver = (!facts.driver_id.is_empty()).then_some(driver.as_str());
    Ok(floors_lowered(
        measured.as_deref().unwrap_or(""),
        judged.unwrap_or(""),
        &|name| gated.contains(name),
        driver,
    ))
}

fn records_about(receipts: &Path, tsr: &str) -> Vec<(String, Facts)> {
    let mut out: Vec<(String, Facts)> = fs::read_dir(receipts)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.starts_with(FACTS_PREFIX).then_some(())?;
            let facts = Facts::parse(&fs::read_to_string(e.path()).ok()?)?;
            (facts.provenance.as_ref()?.tree_sans_floors == tsr).then_some((name, facts))
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Looks up the stored [`Facts`] and key for `commit` and `prover`, verifying no floor was lowered.
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
    let name = format!("{FACTS_PREFIX}{key}");
    let shown = format!("{FACTS_PREFIX}{}", short(&key));
    let Ok(text) = fs::read_to_string(receipts.join(&name)) else {
        let others = records_about(receipts, &tsr);
        let mut why = format!(
            "no whole-tree Trust facts for its tree (every file but {RATCHET}, {MEMORY}, {COSTS} and {BACKSTOPS}) from the requested prover ({}, identity {}) (`{shown}`)",
            prover.bin.display(),
            short(&id)
        );
        for (n, f) in &others {
            let p = f.provenance.clone().unwrap_or_default();
            why.push_str(&format!(
                "; `{}` holds facts for this tree from prover {} (trustc {}, at {})",
                short_name(n),
                short(&p.prover),
                short(&p.trustc),
                p.prover_root
            ));
        }
        return Ok(Err(why));
    };
    let Some(facts) = Facts::parse(&text) else {
        let head = text.lines().next().unwrap_or("");
        if head != FACTS_MAGIC && head.starts_with("aterm-trust facts ") {
            return Ok(Err(format!(
                "`{shown}` was filed as `{head}`, and this judge reads only `{FACTS_MAGIC}`, so it holds no facts it can judge; measure the tree again: targo --unverified ship trust run --whole-tree"
            )));
        }
        return Ok(Err(format!("`{shown}` is not a complete facts record")));
    };
    let Some(p) = facts.provenance.clone() else {
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

/// Parsed `trust` CLI subcommand (`Conclude`, `AdmitPlan`, `Verdict`, or `Pending`).
#[derive(Debug, PartialEq, Eq)]
pub enum TrustCmd {
    Run(crate::trust_lane::Config),
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
}

/// Parses CLI arguments for the `trust` subcommand.
pub fn parse_cmd<'a>(
    it: &mut impl Iterator<Item = &'a str>,
) -> std::result::Result<TrustCmd, String> {
    let verb = it
        .next()
        .ok_or("trust needs a verb: run, judge or credit")?;
    if verb == "run" {
        return crate::trust_lane::parse(it).map(TrustCmd::Run);
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
    let reason = value("--reason").map(str::to_string);
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

/// Lists crates left undecided (lapsed, OOM, or coordinator-refused) in `rev`'s stored facts.
pub fn pending_for(
    git: &dyn GitRunner,
    prover: &Prover,
    rev: &str,
) -> Result<std::result::Result<Vec<String>, String>> {
    let store = crate::gates::receipt_store(git)?;
    let head = rev_parse(git, &format!("{rev}^{{commit}}"))?;
    let (facts, _) = match facts_for(git, &store, prover, &head)? {
        Ok(found) => found,
        Err(why) => return Ok(Err(why)),
    };
    let ratchet = ratchet_at(git, &head)?;
    let backstops = floors_at(git, &head, BACKSTOPS)?;
    let verdict = judge(
        &facts,
        ratchet.as_deref(),
        backstops.as_deref(),
        RATCHET,
        MEMORY,
        Update::NONE,
    );
    Ok(Ok(verdict.unverified))
}

/// Executes `cmd` against `repo` and returns its process exit code (`0`, `1`, or `2`).
pub fn run_cmd(repo: &Path, cmd: TrustCmd) -> i32 {
    let git = crate::ledger::GitCli::new(repo);
    match cmd {
        TrustCmd::Credit(records) => crate::trust_credit::run_cmd(repo, &records),
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
                facts_for(&git, &store, &prover, &head)
            });
            let (facts, key) = match found {
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
            let text = fs::read_to_string(&path).ok();
            let backstops_path = repo.join(BACKSTOPS);
            let backstops = fs::read_to_string(&backstops_path).ok();
            let verdict = judge(
                &facts,
                text.as_deref(),
                backstops.as_deref(),
                RATCHET,
                MEMORY,
                Update {
                    on: update,
                    reason: reason.as_deref(),
                },
            );
            let lowered = match lowered_since_measured(
                &git,
                &facts,
                verdict.ratchet.as_deref().or(text.as_deref()),
            ) {
                Ok(l) => l,
                Err(why) => {
                    eprintln!("aterm-release trust judge: {why}");
                    return 2;
                }
            };
            print!("{}", verdict.report);
            for l in &lowered {
                println!("LOWERED: {l}");
            }
            let p = facts.provenance.unwrap_or_default();
            println!(
                "FACTS: {FACTS_PREFIX}{} (measured on {}, prover {}), judged against the working {RATCHET}",
                short(&key),
                short(&p.head),
                prover.shown()
            );
            if let Err(e) = write_floor_files(&[
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
                let mem = repo.join(MEMORY);
                let written = fs::read_to_string(&mem)
                    .map(|t| memory_rewritten(&t, &verdict.memory_rows))
                    .and_then(|t| fs::write(&mem, t));
                if let Err(e) = written {
                    eprintln!(
                        "aterm-release trust judge: could not write {}: {e}",
                        mem.display()
                    );
                    return 2;
                }
                println!("MEMORY RECORD: {MEMORY} kept current from the stored facts");
            }
            if !lowered.is_empty() {
                println!(
                    "VERDICT: FAIL — the working floors lower {} floor(s) the ratchet's up-only rule refuses (LOWERED above), against those of {}, which the facts were measured on",
                    lowered.len(),
                    short(&p.head)
                );
                return 1;
            }
            if !verdict.tail.is_empty() {
                println!("{}", verdict.tail);
            }
            verdict.exit
        }
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    /// Creates a mock prover `bin/` directory under `root` for unit tests.
    pub(crate) fn prover_bin(root: &Path, tag: &str, commit: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let bin = root.join(format!("prover-{tag}/bin"));
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join("targo"), format!("#!/bin/sh\n# {tag}\nexit 0\n")).unwrap();
        fs::write(
            bin.join("trustc"),
            format!("#!/bin/sh\necho 'commit-hash: {commit}'\n"),
        )
        .unwrap();
        for tool in ["targo", "trustc"] {
            fs::set_permissions(bin.join(tool), fs::Permissions::from_mode(0o755)).unwrap();
        }
        bin
    }

    /// Builds a [`Prover`] matching [`prover_bin`] without spawning `trustc`.
    pub(crate) fn prover(root: &Path, tag: &str, commit: &str) -> Prover {
        let bin = prover_bin(root, tag, commit);
        Prover {
            identity: prover_identity(&bin).map(|(id, _)| id),
            trustc: commit.to_string(),
            bin,
        }
    }

    /// Files a synthetic whole-tree [`Facts`] record in `repo` for unit tests.
    pub(crate) fn file(
        git: &dyn GitRunner,
        store: &Path,
        prover: &Prover,
        commit: &str,
        driver_id: &str,
        crates: Vec<Crate>,
    ) -> PathBuf {
        let tsr = tree_sans_floors(git, commit).unwrap();
        let id = prover.identity.clone().unwrap();
        let facts = Facts {
            driver: prover.bin.join("targo").display().to_string(),
            driver_id: driver_id.into(),
            trustd: "/tmp/t.sock".into(),
            grants: "1".into(),
            crates,
            provenance: Some(Provenance {
                key: facts_key(&tsr, &id, LANE_WHOLE_TREE),
                tree_sans_floors: tsr,
                prover: id,
                lane: LANE_WHOLE_TREE.into(),
                prover_root: prover.bin.parent().unwrap().display().to_string(),
                trustc: prover.trustc.clone(),
                head: commit.into(),
                ..Provenance::default()
            }),
            ..Facts::default()
        };
        file_record(store, &facts).unwrap()
    }

    /// Creates a gated [`Crate`] with `summary` for unit tests.
    pub(crate) fn gated(name: &str, summary: &str) -> Crate {
        Crate::new(
            name,
            Decision::Gate(Measured {
                rc: "0".into(),
                summary: summary.into(),
                ..Measured::default()
            }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log(text: &str) -> Measured {
        Measured::of_log(text.as_bytes(), "0")
    }

    #[test]
    fn a_summary_line_parses_as_the_shell_read_it() {
        assert_eq!(
            parse_summary("  Summary: 3 proved, 0 failed, 0 assumed, 0 inconclusive (7 total)"),
            (Some(3), Some(7))
        );
        assert_eq!(
            parse_summary("Summary: 1 proved (2 total) Summary: 5 proved (9 total)"),
            (Some(5), Some(9))
        );
        assert_eq!(
            parse_summary("  Summary: three properties proved, nothing outstanding"),
            (None, None)
        );
        assert_eq!(
            parse_summary("Summary: 4 proved, no total"),
            (Some(4), None)
        );
    }

    #[test]
    fn a_log_is_measured_as_the_shell_measured_it() {
        let m = log(concat!(
            "noise timed_out=3 and timed_out=4\n",
            "  [UNKNOWN] [unknown] unsupported MIR `MemoryAdmission`: memory admission failed before solver dispatch: configured trustd admission failed: could not connect to selected endpoint\n",
            "    [unsupported_mir] unsupported MIR `MemoryAdmission`: memory admission failed before solver dispatch: could not connect\n",
            "note: trust: memory census: 397 bodies verified and compacted, 2 compacted slots recomputed, 3 bodies skipped at the memory ceiling; resident 1.2 GiB, peak 3.5 GiB, ceiling 3.0 GiB\n",
            "  [UNKNOWN] [unknown] malformed: type lowering exceeded the per-function verification budget\n",
            "    [unsupported_mir] detail: type lowering exceeded the per-function verification budget\n",
            " ERROR ay_dpll::executor::proof::check::internal internal proof checker rejected UNSAT proof, error=proof validation cancelled by the caller (interrupt, deadline, or memory limit), checked_steps=6, skipped_hole_steps=0, total_steps=6\n",
            " ERROR ay_dpll::executor::proof::check::internal internal proof checker rejected UNSAT proof, error=step t9 has invalid resolution derivation, checked_steps=3, skipped_hole_steps=0, total_steps=9\n",
            "  Summary: 2 proved, 0 failed (3 total)\n",
        ));
        assert_eq!(
            (m.lapsed, m.cancelled),
            (7, 1),
            "seven timed out; one proof check cancelled, an invalid derivation not"
        );
        assert_eq!(m.refused, 1, "the [UNKNOWN] listing, not its detail line");
        assert_eq!(m.refused_why, "trustd unreachable: 1");
        assert_eq!(m.skipped, Some(3));
        assert_eq!(m.ceiling, "3.0 GiB");
        assert_eq!(m.budget_rows, 1, "result rows only, never the detail line");
        assert!(!m.transport);
        assert_eq!(parse_summary(&m.summary), (Some(2), Some(3)));
        assert_eq!(m.peak, "3.5 GiB");
        let m = log("rustc -Ztrust-verify-memory-ceiling-bytes=3221225472 x\n");
        assert_eq!(m.ceiling, "3221225472 B");
        assert_eq!(gib_of(&m.ceiling), Some(3.0));
        let m = log(concat!(
            "note: trust: memory census: 12 artifact slots compacted after their verify pass (0 after the pass had dropped its own handle), 0 compacted slots recomputed by a later pass; resident 1.1 GiB now, highest 2.3 GiB read at a body boundary\n",
            "  Summary: 2 proved, 0 failed (2 total)\n",
        ));
        assert_eq!(
            (m.skipped, m.ceiling.as_str(), m.peak.as_str()),
            (None, "", "")
        );
        let fields = m.to_fields();
        assert_eq!(fields.split('\t').nth(6), Some("-"), "{fields}");
        let back = Measured::from_fields(&fields.split('\t').collect::<Vec<_>>());
        assert_eq!(back.map(|(b, _)| b.skipped), Some(None));
        assert!(log("targo trust: error: verification did not run\n").transport);
    }

    const DONE: &str = "Summary: 5 proved, 3 failed, 8 runtime-checked, 0 assumed, 0 mandated, 0 contract-panic, 5 inconclusive (21 total)\n\
                        Coverage: 9/9 eligible function bodies verified (complete)\n\
                        \x20 Gate: FAIL [advisory lane, exit 1] (5 proved / 21 total)\n";

    const KILLED: &str = "error: could not compile `aterm-census` (lib); 216 warnings emitted\n\nCaused by:\n\
                          \x20 process didn't exit successfully: `/Users//x/toolchains/trust-6606d3bd/bin/trustc --crate-name aterm_census --edition=2024 crates/aterm-census/src/lib.rs --crate-type lib` (signal: 15, SIGTERM: termination signal)\n\
                          === Trust Verification Report ===\n\
                          Level: L2 | Solver timeout: 5000ms | Function budget: 120000ms | Duration: 7865903ms\n\
                          \x20 [UNKNOWN] [transport:missing-crate-summary] compiler emitted function rows without an end-of-compile target inventory (targo-trust)\n\
                          Summary: 207 proved, 440 failed, 252 runtime-checked, 56 assumed, 9 mandated, 0 contract-panic, 1073 inconclusive (2037 total)\n\
                          Coverage: unknown (compiler emitted no coverage_summary transport row; older toolchain?)\n\
                          \x20   [assertion] assertion: transport:missing-crate-summary UNKNOWN (targo-trust: missing crate_summary transport prevents proof that this rustc target completed)\n\
                          \x20 Gate: FAIL [advisory lane, exit 101] (207 proved / 2037 total)\n";

    #[test]
    fn a_check_that_did_not_finish_says_so_in_its_own_record() {
        let ended = |text: &str, rc: &str| Measured::of_log(text.as_bytes(), rc).ended;
        let m = Measured::of_log(KILLED.as_bytes(), "101");
        assert_eq!(
            parse_summary(&m.summary),
            (Some(207), Some(2037)),
            "its Summary still parses"
        );
        assert!(!m.transport);
        assert!(!m.ended.is_empty());
        for rc in ["0", "1", "2", "101", "?"] {
            assert_eq!(ended(DONE, rc), "", "rc {rc}");
        }
        assert_eq!(ended("", "0"), "", "no Summary is GATE-BROKE's, not this");
        for kind in [
            "missing-crate-summary",
            "summary-accounting",
            "crate-summary-accounting",
            "ordering",
            "unsupported-message",
            "malformed",
        ] {
            assert!(
                !ended(
                    &format!(
                        "  [UNKNOWN] [transport:{kind}] the {kind} defect (targo-trust)\n{DONE}"
                    ),
                    "1"
                )
                .is_empty(),
                "{kind}"
            );
        }
        let m = Measured::of_log(
            format!("  [UNKNOWN] [transport:missing-json] missing structured Trust JSON transport (targo-trust)\n{DONE}")
                .as_bytes(),
            "2",
        );
        assert!(
            m.transport && m.ended.is_empty(),
            "missing-json is `transport`: {m:?}"
        );
        assert!(
            !ended(
                &format!(
                    "  process didn't exit successfully: `/x/bin/trustc --crate-name a` (signal: 9, SIGKILL: kill)\n{DONE}"
                ),
                "101"
            )
            .is_empty()
        );
        assert_eq!(
            ended(
                &format!(
                    "  process didn't exit successfully: `/x/bin/trustc --crate-name a` (exit status: 1)\n{DONE}"
                ),
                "101"
            ),
            ""
        );
        for rc in ["143", "137", "158"] {
            assert!(!ended(DONE, rc).is_empty(), "rc {rc}");
        }
        assert_eq!(ended(DONE, "128"), "");
        assert_eq!(ended(DONE, "193"), "");
        let summary = "Summary: 5 proved (21 total)\n";
        assert!(!ended(summary, "0").is_empty());
        assert!(
            !ended(&format!("{summary}Coverage: 9/12 — coverage shortfall: 3 function(s) were never verified (fail-closed: never a passing gate)\n"), "1")
                .is_empty()
        );
        assert!(
            !ended(
                &format!("{summary}Coverage: 8/9 eligible function bodies verified (complete)\n"),
                "0"
            )
            .is_empty(),
            "counts that disagree are no complete coverage"
        );
        assert!(!ended(&format!("{summary}Coverage: unknown (compiler emitted no coverage_summary transport row; older toolchain?)\n"), "0").is_empty());
        assert_eq!(
            ended(
                &format!(
                    "{summary}Coverage: 12/12 eligible function bodies accounted for (10 verified, 2 excluded: test-harness) (complete)\n"
                ),
                "1"
            ),
            ""
        );
        assert!(
            !ended(
                &format!("{summary}Coverage: 11/12 eligible function bodies accounted for (9 verified, 2 excluded: test-harness) (complete)\n"),
                "1"
            )
            .is_empty(),
            "counts that disagree are no complete coverage, in either wording"
        );
    }

    #[test]
    fn a_check_that_did_not_finish_is_never_scored() {
        let check = |ended: &str, budget: &str| {
            Decision::Gate(Measured {
                rc: "101".into(),
                summary: "Summary: 207 proved (2037 total)".into(),
                ended: ended.into(),
                wall_budget: budget.into(),
                ..Measured::default()
            })
        };
        let run = |d: Decision| {
            facts(vec![
                ("alpha", d),
                ("beta", gate("Summary: 3 proved (3 total)")),
            ])
        };
        let low = format!("alpha\t200\t2037\t{DRV}\nbeta\t3\t3\t{DRV}\n");
        let v = judge(
            &run(check("", "")),
            Some(&low),
            None,
            "R",
            "M",
            Update::NONE,
        );
        assert_eq!((v.word, v.exit, v.total.as_str()), ("PASS", 0, "210/2040"));
        let v = judge(
            &run(check("cargo: trustc ended by signal 15 (SIGTERM)", "")),
            Some(&low),
            None,
            "R",
            "M",
            Update::NONE,
        );
        assert_eq!(
            (v.word, v.exit, v.total.as_str()),
            ("FAIL", 1, "3/3"),
            "{}",
            v.report
        );
        assert_eq!(v.failed, ["alpha NOT-VERIFIED(incomplete)"]);
        let v = judge(
            &run(check("", "5400")),
            Some(&low),
            None,
            "R",
            "M",
            Update::NONE,
        );
        assert_eq!((v.word, v.exit, v.total.as_str()), ("FAIL", 1, "3/3"));
        assert_eq!(v.failed, ["alpha OVER-BUDGET(5400s)"]);
        let unfinished = [check("x", ""), check("", "60")];
        for d in &unfinished {
            for (ratchet, update) in [
                (format!("beta\t3\t3\t{DRV}\n"), Update::RAISE),
                (
                    format!("alpha\t100\t2037\t{DRV}\nbeta\t3\t3\t{DRV}\n"),
                    Update::RAISE,
                ),
                (
                    format!("alpha\t274\t2037\tanother prover\nbeta\t3\t3\t{DRV}\n"),
                    Update::RAISE,
                ),
                (
                    format!("alpha\t274\t2037\tanother prover\nbeta\t3\t3\t{DRV}\n"),
                    Update {
                        on: true,
                        reason: Some("the new prover proves less"),
                    },
                ),
            ] {
                let v = judge(&run(d.clone()), Some(&ratchet), None, "R", "M", update);
                assert_eq!(v.exit, 1);
                assert!(
                    v.ratchet
                        .as_deref()
                        .is_none_or(|r| !r.contains("alpha\t207")),
                    "a floor from a check that did not finish: {:?}",
                    v.ratchet
                );
                assert!(v.memory_rows.iter().all(|(n, _)| n != "alpha"));
                assert!(v.host_obs.iter().all(|(n, _)| n != "alpha"));
            }
        }
        let f = run(check("cargo: trustc ended by signal 15 (SIGTERM)", "5400"));
        assert_eq!(Facts::parse(&f.to_record()), Some(f.clone()));
        assert_eq!(
            Facts::parse(&f.to_record().replace(FACTS_MAGIC, "aterm-trust facts 5")),
            None,
            "a version-5 record, which could not tell, is never read"
        );
    }

    #[test]
    fn the_lanes_budget_mark_is_read_into_the_facts() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-trust-facts-budget-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for (spec, rc, mark) in [
            ("alpha@1.0.0", "137", Some("60\n")),
            ("beta@2.0.0", "1", Some("\n")),
            ("gamma@3.0.0", "1", None),
        ] {
            fs::write(dir.join(format!("{spec}.log")), DONE).unwrap();
            fs::write(dir.join(format!("{spec}.log.rc")), rc).unwrap();
            if let Some(mark) = mark {
                fs::write(dir.join(format!("{spec}.log.budget")), mark).unwrap();
            }
        }
        let mut p = Plan::parse(&format!(
            "{PLAN_MAGIC}\nlogdir {}\ncrate alpha\talpha@1.0.0\tgate\ncrate beta\tbeta@2.0.0\tgate\ncrate gamma\tgamma@3.0.0\tgate\n",
            dir.display()
        ))
        .unwrap();
        p.measure();
        let _ = fs::remove_dir_all(&dir);
        let got: Vec<(String, bool)> = p
            .facts
            .crates
            .iter()
            .map(|c| match &c.decision {
                Decision::Gate(m) => (m.wall_budget.clone(), !m.ended.is_empty()),
                other => panic!("{other:?}"),
            })
            .collect();
        let want: Vec<(String, bool)> = vec![
            ("60".to_string(), true),
            ("?".to_string(), false),
            (String::new(), false),
        ];
        assert_eq!(got, want);
    }

    fn gate(summary: &str) -> Decision {
        Decision::Gate(Measured {
            rc: "0".into(),
            summary: summary.into(),
            ..Measured::default()
        })
    }

    fn facts(crates: Vec<(&str, Decision)>) -> Facts {
        Facts {
            driver: "/store/trust/9192/bin/targo".into(),
            driver_id: "targo 1.99.0-dev (aaaaaaaaa 2026-09-17) (targo 0.1.0)".into(),
            trustd: "/tmp/t.sock".into(),
            grants: "1".into(),
            crates: crates.into_iter().map(|(n, d)| Crate::new(n, d)).collect(),
            ..Facts::default()
        }
    }

    const DRV: &str = "targo 1.99.0-dev (aaaaaaaaa 2026-09-17) (targo 0.1.0)";

    #[test]
    fn a_record_round_trips_and_a_cut_short_one_is_none() {
        let mut f = facts(vec![
            ("alpha", gate("  Summary: 3 proved (3 total)")),
            ("beta", Decision::NoLib),
            ("forky", Decision::Queued("queued".into())),
        ]);
        f.vanished.push("ghost".into());
        f.ambiguous
            .push(("dup".into(), "dup@1".into(), "dup@2".into()));
        f.crates.push(Crate::new("dup", Decision::Ambiguous));
        f.provenance = Some(Provenance {
            key: "k".into(),
            ..Provenance::default()
        });
        let text = f.to_record();
        assert_eq!(Facts::parse(&text), Some(f.clone()));
        let cut = text.trim_end().strip_suffix(FACTS_END).unwrap();
        assert_eq!(Facts::parse(cut), None, "no `end`, no record");
        assert_eq!(
            Facts::parse(&text.replace(FACTS_MAGIC, "aterm-trust facts 0")),
            None
        );
        let schema1 = text
            .replace(FACTS_MAGIC, "aterm-trust facts 1")
            .replace("key k\n", "key k\nenv none\n");
        assert_eq!(Facts::parse(&schema1), None);
        assert_eq!(
            Facts::parse(&format!("{text}crate x\tgate\n")),
            None,
            "nothing after `end`"
        );
    }

    use crate::ledger::testing::Git;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    struct World {
        root: PathBuf,
    }

    impl Drop for World {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    const PROVER_COMMIT: &str = "321aaeda75478038420d97830f14724c33c8dc39";

    impl World {
        fn new(label: &str) -> World {
            let root = std::env::temp_dir().join(format!(
                "aterm-trust-facts-{label}-{}-{}",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(root.join("work")).unwrap();
            let w = World { root };
            crate::ledger::testing::init(
                &w.work(),
                &[
                    ("user.name", "Test"),
                    ("user.email", "test@example.invalid"),
                    ("commit.gpgsign", "false"),
                ],
            );
            w
        }
        fn work(&self) -> PathBuf {
            self.root.join("work")
        }
        fn git(&self, args: &[&str]) -> String {
            let out = crate::ledger::testing::git()
                .arg("-C")
                .arg(self.work())
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }
        fn commit(&self, files: &[(&str, &str)]) -> String {
            for (path, body) in files {
                let full = self.work().join(path);
                fs::create_dir_all(full.parent().unwrap()).unwrap();
                fs::write(full, body).unwrap();
            }
            if !files.is_empty() {
                let mut add = vec!["add", "--"];
                add.extend(files.iter().map(|(path, _)| *path));
                self.git(&add);
            }
            self.git(&["commit", "-q", "--allow-empty", "-m", "c"]);
            self.git(&["rev-parse", "HEAD"])
        }
        fn cli(&self) -> Git {
            Git::new(self.work())
        }
        fn prover(&self, name: &str) -> Prover {
            testing::prover(&self.root, name, PROVER_COMMIT)
        }
        fn store(&self) -> PathBuf {
            self.work().join(".git/aterm-verify/receipts")
        }
        fn file(&self, commit: &str, prover: &Prover, proved: u64, total: u64) -> PathBuf {
            let alpha = testing::gated(
                "alpha",
                &format!("Summary: {proved} proved ({total} total)"),
            );
            testing::file(&self.cli(), &self.store(), prover, commit, DRV, vec![alpha])
        }
        fn found(
            &self,
            prover: &Prover,
            rev: &str,
        ) -> std::result::Result<(Facts, String), String> {
            let head = self.git(&["rev-parse", &format!("{rev}^{{commit}}")]);
            facts_for(&self.cli(), &self.store(), prover, &head).unwrap()
        }
        fn judged(&self, prover: &Prover) -> Verdict {
            let (facts, _) = self.found(prover, "HEAD").expect("the facts are found");
            let ratchet = ratchet_at(&self.cli(), "HEAD").unwrap();
            judge(
                &facts,
                ratchet.as_deref(),
                None,
                RATCHET,
                MEMORY,
                Update::NONE,
            )
        }
    }

    fn floor(n: u64) -> String {
        format!("alpha\t{n}\t9\t{DRV}\n")
    }

    #[test]
    fn the_up_only_rule_holds_row_by_row() {
        let d = "targo 1.99.0-dev (aaaaaaaaa 2026-09-17) (targo 0.1.0)";
        let base = format!("alpha\t987\t3806\t{d}\nbeta\t5\t9\t{d}\nold\t3\t3\tolder prover\n");
        let all = |_: &str| true;
        let low = |tree: &str| floors_lowered(&base, tree, &all, Some(d));
        assert!(low(&base).is_empty(), "unchanged");
        assert!(
            low(&base.replace("987\t3806", "990\t3806")).is_empty(),
            "raised"
        );
        assert!(
            low(&base.replace("987\t3806", "987\t3807")).is_empty(),
            "a total raised to the one measured"
        );
        let got = low(&base.replace("987\t3806", "900\t3806"));
        assert_eq!(got.len(), 1, "{got:?}");

        let said = base.replace(
            &format!("987\t3806\t{d}\n"),
            &format!("985\t3800\t{d}\tthe code left\n"),
        );
        assert!(low(&said).is_empty(), "a reason, the total down as far");
        let excuse = said.replace("985\t3800", "900\t3800");
        assert_eq!(
            low(&excuse).len(),
            1,
            "a reason does not excuse a lowering past the total"
        );
        assert!(
            low(&base.replace("older prover", d)).is_empty(),
            "re-labelled onto the measuring prover"
        );
        let rebased = base.replace("3\t3\tolder prover", &format!("1\t3\t{d}"));
        assert_eq!(low(&rebased).len(), 1);
        assert!(
            low(&base.replace(
                "3\t3\tolder prover",
                &format!("1\t3\t{d}\tthe new seal proves two fewer")
            ))
            .is_empty(),
            "a reasoned re-base"
        );
        assert_eq!(
            low(&base.replace("3\t3\tolder prover", "1\t3\tolder prover")).len(),
            1
        );
        let relabel = low(&base.replace(&format!("5\t9\t{d}"), "5\t9\telsewhere"));
        assert_eq!(relabel.len(), 1, "{relabel:?}");
        assert_eq!(
            low(&base.replace(&format!("beta\t5\t9\t{d}\n"), "")).len(),
            1
        );
        assert!(
            floors_lowered(
                &base,
                &base.replace(&format!("beta\t5\t9\t{d}\n"), ""),
                &|n| n != "beta",
                Some(d)
            )
            .is_empty(),
            "a library the facts do not verify"
        );
        let added = format!("{base}gamma\t4\t4\tsomeone\n");
        assert_eq!(low(&added).len(), 1, "a first floor under another name");
        assert!(
            low(&format!("{base}gamma\t4\t4\t{d}\n")).is_empty(),
            "a first floor under the measuring prover"
        );
        assert!(
            floors_lowered(&base, &added, &|n| n != "gamma", Some(d)).is_empty(),
            "a first floor for a library the facts do not verify"
        );
        assert_eq!(low(&base.replace("987\t3806", "9x7\t3806")).len(), 1);
        assert_eq!(
            floors_lowered(&base, &base.replace("older prover", d), &all, None).len(),
            1
        );
    }

    #[test]
    fn another_prover_is_another_key() {
        let w = World::new("prover");
        let (a, b) = (w.prover("a"), w.prover("b"));
        assert_ne!(a.identity, b.identity, "two builds, one trustc commit");
        let real = Prover::at(&a.bin);
        assert_eq!(
            (&real.identity, &real.trustc),
            (&a.identity, &a.trustc),
            "the fixture is what Prover::at answers"
        );
        let head = w.commit(&[("src/lib.rs", "v1"), (RATCHET, &floor(5))]);
        w.file(&head, &a, 5, 9);
        assert!(w.found(&a, "HEAD").is_ok());
        let why = w.found(&b, "HEAD").unwrap_err();
        assert!(why.contains("no whole-tree Trust facts"), "{why}");
        let bare = Prover::at(&w.root.join("nowhere/bin"));
        assert!(w.found(&bare, "HEAD").is_err());
    }

    #[test]
    fn a_record_under_another_key_is_never_read() {
        let w = World::new("forged");
        let a = w.prover("a");
        let first = w.commit(&[("src/lib.rs", "v1"), (RATCHET, &floor(5))]);
        let filed = w.file(&first, &a, 5, 9);
        let second = w.commit(&[("src/lib.rs", "v2")]);
        let tsr = tree_sans_floors(&w.cli(), &second).unwrap();
        let key = facts_key(&tsr, a.identity.as_ref().unwrap(), LANE_WHOLE_TREE);
        let name = w.store().join(format!("{FACTS_PREFIX}{key}"));
        fs::copy(&filed, &name).unwrap();
        assert!(w.found(&a, "HEAD").is_err());
        fs::write(&name, format!("{FACTS_MAGIC}\ncrate alpha\tgate\n")).unwrap();
        assert!(w.found(&a, "HEAD").is_err());
    }

    #[test]
    fn the_fences_still_fire_from_stored_facts() {
        let w = World::new("fences");
        let a = w.prover("a");
        let head = w.commit(&[("src/lib.rs", "v1"), (RATCHET, &floor(5))]);
        let tsr = tree_sans_floors(&w.cli(), &head).unwrap();
        let id = a.identity.clone().unwrap();
        let file_with = |d: Decision| {
            let mut f = facts(vec![("alpha", d)]);
            f.provenance = Some(Provenance {
                key: facts_key(&tsr, &id, LANE_WHOLE_TREE),
                tree_sans_floors: tsr.clone(),
                prover: id.clone(),
                lane: LANE_WHOLE_TREE.into(),
                head: w.git(&["rev-parse", "HEAD"]),
                ..Provenance::default()
            });
            file_record(&w.store(), &f).unwrap();
        };
        file_with(gate("Summary: 5 proved (9 total)"));
        assert_eq!(w.judged(&a).word, "PASS", "(control)");
        let cases: [(&str, Decision, &str); 4] = [
            (
                &format!("alpha\t5\t8\t{DRV}\tone proved obligation left with its code\n"),
                gate("Summary: 5 proved (9 total)"),
                "TOTAL-ABOVE-ROW(>8)",
            ),
            (
                "beta\t1\t1\n",
                gate("Summary: 5 proved (9 total)"),
                "alpha NO-FLOOR",
            ),
            (
                &floor(5),
                Decision::Gate(Measured {
                    rc: "2".into(),
                    transport: true,
                    ..Measured::default()
                }),
                "NOT-VERIFIED(rc=2)",
            ),
            (&floor(5), gate(""), "GATE-BROKE(rc=0)"),
        ];
        for (ratchet, decision, needle) in cases {
            w.commit(&[(RATCHET, ratchet)]);
            file_with(decision);
            let v = w.judged(&a);
            assert!(
                v.word == "FAIL" && v.failed.iter().any(|row| row.contains(needle)),
                "{needle}: {:?}",
                v.failed
            );
        }
        w.commit(&[(RATCHET, &floor(5))]);
        file_with(Decision::Gate(Measured {
            rc: "0".into(),
            summary: "Summary: 4 proved (9 total)".into(),
            refused: 1,
            refused_why: "trustd unreachable: 1".into(),
            ..Measured::default()
        }));
        assert_eq!(w.judged(&a).word, "COULD-NOT-RUN");
    }

    #[test]
    fn conclude_files_only_what_a_release_may_read() {
        let w = World::new("conclude");
        let a = w.prover("a");
        let head = w.commit(&[("src/lib.rs", "v1"), (RATCHET, &floor(3))]);
        let logs = w.root.join("logs");
        let slots = w.root.join("troot");
        fs::create_dir_all(slots.join("j0/debug/deps")).unwrap();
        fs::create_dir_all(&logs).unwrap();
        fs::write(
            logs.join("alpha@1.0.0.log"),
            "  Summary: 3 proved, 0 failed (3 total)\nCoverage: 2/2 eligible function bodies verified (complete)\n",
        )
        .unwrap();
        fs::write(logs.join("alpha@1.0.0.log.rc"), "0\n").unwrap();
        fs::write(logs.join(".trustd-grants"), "1\n").unwrap();
        let dep = slots.join("j0/debug/deps/alpha-1.d");
        let plain = "alpha.rmeta: crates/alpha/src/lib.rs\n";
        fs::write(&dep, plain).unwrap();
        const LANE: &str = "lane-pin SOURCE_DATE_EPOCH=0\nlane-pin ATERM_BUILD_GIT_COMMIT=trust-lane\n\
                            lane-passed PATH\nlane-passed HOME\nlane-passed LANG\nlane-passed CARGO_TARGET_DIR\n";
        let plan = |extra: &str| {
            format!(
                "{PLAN_MAGIC}\ndriver {}/targo\ndriver-id {DRV}\ntrustd /tmp/t.sock\nonly \n\
                 logdir {}\nratchet {}\ncrate alpha\talpha@1.0.0\tgate\n\
                 store {}\ncommit {head}\ntree x\nprover {}\nprover-bin {}\ntarget-root {}\n{LANE}{extra}",
                a.bin.display(),
                logs.display(),
                w.work().join(RATCHET).display(),
                w.store().display(),
                a.identity.as_ref().unwrap(),
                a.bin.display(),
                slots.display(),
            )
        };
        let run = |extra: &str| {
            let mut p = Plan::parse(&plan(extra)).unwrap();
            conclude(&w.cli(), &w.work(), &mut p, Update::NONE).unwrap()
        };
        let facts_line = |extra: &str| run(extra).facts_line.unwrap_or_default();
        assert!(facts_line("file the checkout is not clean\n").starts_with("FACTS: none"));
        assert!(
            run("").facts_line.is_none(),
            "a run that does not file says nothing"
        );
        let mut bare = Plan::parse(&plan("file \n").replace(LANE, "")).unwrap();
        let l = conclude(&w.cli(), &w.work(), &mut bare, Update::NONE)
            .unwrap()
            .facts_line
            .unwrap();
        assert!(l.starts_with("FACTS: none"), "{l}");
        fs::write(
            &dep,
            format!("alpha.rmeta: crates/alpha/src/lib.rs {RATCHET}\n"),
        )
        .unwrap();
        assert!(facts_line("file \n").starts_with("FACTS: none"));
        fs::write(
            &dep,
            format!("alpha.rmeta: crates/alpha/src/lib.rs {MEMORY}\n"),
        )
        .unwrap();
        assert!(facts_line("file \n").starts_with("FACTS: none"));
        let reads = |lines: &str| {
            fs::write(&dep, format!("{plain}\n{lines}")).unwrap();
            facts_line("file \n")
        };
        for lines in [
            "# env-dep:ATERM_RELEASE_BUILD=1\n",
            "# env-dep:HOME\n",
            "# env-dep:LANG=en_US.UTF-8\n# env-dep:HOME=/Users//x\n",
            "# env-dep:SOURCE_DATE_EPOCH=5\n",
            "# env-dep:ATERM_GIT_COMMIT=trust-lane\n",
        ] {
            let l = reads(lines);
            assert!(
                l.starts_with("FACTS: none") && !l.contains("/Users//x"),
                "{lines}: {l}"
            );
        }
        let build = slots.join("j0/debug/build/aterm-gui-0123/output");
        fs::create_dir_all(build.parent().unwrap()).unwrap();
        fs::write(
            &build,
            "cargo:rerun-if-env-changed=ATERM_BUILD_GIT_COMMIT\ncargo:rustc-env=ATERM_GIT_COMMIT=trust-lane\n",
        )
        .unwrap();
        let l = reads(
            "# env-dep:ATERM_RELEASE_BUILD\n# env-dep:CARGO_PKG_VERSION=1.0.0\n# env-dep:OUT_DIR=/x/out\n\
             # env-dep:SOURCE_DATE_EPOCH=0\n# env-dep:ATERM_GIT_COMMIT=trust-lane\n",
        );
        assert!(l.starts_with("FACTS: ") && !l.contains("none"), "{l}");
        fs::remove_file(&build).unwrap();
        fs::write(&dep, plain).unwrap();
        let home = w.root.join("cargo-home");
        fs::create_dir_all(&home).unwrap();
        fs::write(
            home.join("config.toml"),
            "[profile.dev]\ndebug-assertions = false\n",
        )
        .unwrap();
        let l = facts_line(&format!("file \ncargo-home {}\n", home.display()));
        assert!(l.starts_with("FACTS: none"), "{l}");
        let l = run("file \n").facts_line.unwrap();
        assert!(
            l.starts_with("FACTS: ") && !l.contains("none"),
            "the control files: {l}"
        );
        assert!(w.found(&a, "HEAD").is_ok(), "the control is found");
        fs::remove_file(&dep).unwrap();
        let l = facts_line("file \n");
        assert!(
            l.starts_with("FACTS: none") && l.contains("no dep-info"),
            "{l}"
        );
        fs::write(&dep, plain).unwrap();
        let mut unlisted =
            Plan::parse(&plan("file \n").replace(&format!("commit {head}"), "commit 0000"))
                .unwrap();
        let done = conclude(&w.cli(), &w.work(), &mut unlisted, Update::NONE).unwrap();
        let l = done.facts_line.unwrap();
        assert!(
            l.starts_with("FACTS: none") && l.contains("git could not list"),
            "{l}"
        );
        assert_eq!(done.verdict.word, "PASS");
        let mut moved =
            Plan::parse(&plan("file \n").replace(a.identity.as_ref().unwrap(), "0000")).unwrap();
        let l = conclude(&w.cli(), &w.work(), &mut moved, Update::NONE)
            .unwrap()
            .facts_line
            .unwrap();
        assert!(l.starts_with("FACTS: none"), "{l}");
        let done = run("");
        assert_eq!(done.verdict.word, "PASS");
    }

    #[test]
    fn the_trust_verbs_answer_three_ways() {
        let w = World::new("cli");
        let a = w.prover("a");
        let measured = format!("alpha\t6\t10\t{DRV}\n");
        let head = w.commit(&[("src/lib.rs", "v1"), (RATCHET, &measured)]);
        let judge_at = |commit: &str, update| TrustCmd::Judge {
            commit: commit.into(),
            prover: Some(a.bin.clone()),
            update,
            reason: None,
        };
        let judge_cmd = |update| judge_at("HEAD", update);
        assert_eq!(
            run_cmd(&w.work(), judge_cmd(false)),
            1,
            "no facts: not verified"
        );
        assert_eq!(
            run_cmd(&w.work(), judge_at("no-such-rev", false)),
            2,
            "no commit: no answer"
        );
        w.file(&head, &a, 5, 9);
        let working = || fs::read_to_string(w.work().join(RATCHET)).unwrap();
        assert_eq!(run_cmd(&w.work(), judge_cmd(false)), 1, "5 of a floor of 6");
        fs::write(w.work().join(RATCHET), floor(5)).unwrap();
        assert_eq!(run_cmd(&w.work(), judge_cmd(false)), 1);
        assert_eq!(working(), floor(5));
        let said = format!("alpha\t5\t9\t{DRV}\tone proved obligation left with its code\n");
        fs::write(w.work().join(RATCHET), &said).unwrap();
        assert_eq!(run_cmd(&w.work(), judge_cmd(false)), 0);
        fs::write(w.work().join(RATCHET), &measured).unwrap();
        assert_eq!(run_cmd(&w.work(), judge_cmd(true)), 1);
        assert_eq!(working(), measured);
        let w = World::new("cli-first");
        let a = w.prover("a");
        let head = w.commit(&[("src/lib.rs", "v1"), (RATCHET, "")]);
        w.file(&head, &a, 5, 9);
        let judge_cmd = |update| TrustCmd::Judge {
            commit: "HEAD".into(),
            prover: Some(a.bin.clone()),
            update,
            reason: None,
        };
        assert_eq!(run_cmd(&w.work(), judge_cmd(false)), 1, "NO-FLOOR");
        assert_eq!(run_cmd(&w.work(), judge_cmd(true)), 0);
        assert_eq!(
            fs::read_to_string(w.work().join(RATCHET)).unwrap(),
            format!("alpha\t5\t9\t{DRV}\n")
        );
    }

    #[test]
    fn the_key_leaves_out_the_floors_and_nothing_else() {
        let w = World::new("key");
        let one = w.commit(&[
            ("src/lib.rs", "v1"),
            (RATCHET, "alpha\t1\t1\n"),
            (MEMORY, "alpha\t16.0\t1.4\t=\t-\n"),
        ]);
        let two = w.commit(&[(RATCHET, "alpha\t2\t2\n")]);
        let mem = w.commit(&[(MEMORY, "alpha\t16.0\t1.6\t=\t2026-09-30 at 16.0 GiB\n")]);
        let costs = w.commit(&[(COSTS, "alpha\t1\t1\t7\t2026-10-16T00:00:00Z\n")]);
        let stops = w.commit(&[(BACKSTOPS, "alpha\t0123456789abcdef\twall-backstop\tf\n")]);
        let three = w.commit(&[("crates/alpha/src/lib.rs", "pub fn f() {}\n")]);
        let t = |c: &str| tree_sans_floors(&w.cli(), c).unwrap();
        assert_eq!(t(&one), t(&two), "the floors alone move nothing");
        assert_eq!(t(&two), t(&mem), "nor does the memory record");
        assert_eq!(t(&mem), t(&costs), "nor does the cost-history seed");
        assert_eq!(t(&costs), t(&stops), "nor do the floors' backstops");
        assert_ne!(t(&mem), t(&three), "any other file is an input");
        w.git(&["rm", "-q", RATCHET, MEMORY, COSTS, BACKSTOPS]);
        let four = w.commit(&[]);
        assert_eq!(t(&three), t(&four));
        let k = |t: &str, p: &str, l: &str| facts_key(t, p, l);
        assert_ne!(k("t", "p1", LANE_WHOLE_TREE), k("t", "p2", LANE_WHOLE_TREE));
        assert_ne!(k("t", "p", LANE_WHOLE_TREE), k("u", "p", LANE_WHOLE_TREE));
        assert_ne!(k("t", "p", LANE_WHOLE_TREE), k("t", "p", "narrowed"));
    }

    #[test]
    fn the_judge_holds_floors_as_the_lane_did() {
        let f = facts(vec![("alpha", gate("Summary: 3 proved (3 total)"))]);
        let pass = judge(
            &f,
            Some(&format!("alpha\t3\t3\t{DRV}\n")),
            None,
            "R",
            "M",
            Update::NONE,
        );
        assert_eq!((pass.word, pass.exit), ("PASS", 0));
        let fail = judge(
            &f,
            Some(&format!("alpha\t4\t3\t{DRV}\n")),
            None,
            "R",
            "M",
            Update::NONE,
        );
        assert_eq!((fail.word, fail.exit), ("FAIL", 1));
        assert_eq!(fail.failed, ["alpha RATCHET-REGRESSION(<4) 3/3"]);
        let none = judge(&f, Some(""), None, "R", "M", Update::NONE);
        assert_eq!(none.word, "FAIL");
        let rec = judge(&f, None, None, "R", "M", Update::RAISE);
        assert_eq!(rec.word, "PASS");
        assert_eq!(
            rec.ratchet.as_deref(),
            Some(format!("alpha\t3\t3\t{DRV}\n").as_str())
        );
        let bad = judge(&f, Some("alpha\tthree\t3\n"), None, "R", "M", Update::NONE);
        assert_eq!(bad.word, "FAIL");
        let theirs = "targo 1.99.0-dev (bbbbbbbbb 2026-09-17) (targo 0.1.0)";
        let other = format!("alpha\t9\t9\t{theirs}\n");
        let moved = judge(&f, Some(&other), None, "R", "M", Update::NONE);
        assert_eq!((moved.word, moved.exit), ("FAIL", 1));
        assert_eq!(moved.failed, ["alpha RATCHET-REGRESSION(<9) 3/3"]);
        let kept = judge(&f, Some(&other), None, "R", "M", Update::RAISE);
        assert_eq!(kept.ratchet, None);
        let reason = "trust 5ec1 proves fewer; owner-approved";
        let lowered = judge(
            &f,
            Some(&other),
            None,
            "R",
            "M",
            Update {
                on: true,
                reason: Some(reason),
            },
        );
        assert_eq!(
            lowered.ratchet.as_deref(),
            Some(format!("alpha\t3\t3\t{DRV}\t{reason}\n").as_str())
        );
        let met = format!("alpha\t2\t2\t{theirs}\n");
        assert_eq!(
            judge(&f, Some(&met), None, "R", "M", Update::NONE).word,
            "PASS"
        );
        assert_eq!(
            judge(&f, Some(&met), None, "R", "M", Update::RAISE)
                .ratchet
                .as_deref(),
            Some(format!("alpha\t3\t3\t{DRV}\n").as_str())
        );
        for update in [
            Update::RAISE,
            Update {
                on: true,
                reason: Some(reason),
            },
        ] {
            assert_eq!(
                judge(
                    &f,
                    Some(&format!("alpha\t4\t3\t{DRV}\n")),
                    None,
                    "R",
                    "M",
                    update
                )
                .ratchet,
                None
            );
        }
        assert_eq!(
            judge(
                &f,
                Some(&format!("alpha\t3\t4\t{DRV}\n")),
                None,
                "R",
                "M",
                Update::RAISE
            )
            .ratchet
            .as_deref(),
            Some(format!("alpha\t3\t3\t{DRV}\n").as_str())
        );
        let why = judge(
            &f,
            Some(&format!("alpha\t2\t2\t{DRV}\twent with its code\n")),
            None,
            "R",
            "M",
            Update::NONE,
        );
        assert_eq!(why.word, "FAIL");
        assert!(why.report.contains("TOTAL-ABOVE-ROW(>2)"), "{}", why.report);
    }

    #[test]
    fn a_reasoned_rebase_is_held_to_the_memory_guard() {
        let theirs = "targo 1.99.0-dev (bbbbbbbbb 2026-09-17) (targo 0.1.0)";
        let floor = format!("alpha\t3\t3\t{theirs}\n");
        let reason = Update {
            on: true,
            reason: Some("the new seal proves two fewer"),
        };
        let run = |budget_rows: u64, peak: &str| {
            let mut f = facts(vec![(
                "alpha",
                Decision::Gate(Measured {
                    rc: "0".into(),
                    summary: "Summary: 1 proved (1 total)".into(),
                    budget_rows,
                    peak: peak.into(),
                    ceiling: "1.0 GiB".into(),
                    ..Measured::default()
                }),
            )]);
            f.crates[0].memory = MemRow {
                f: "16.0".into(),
                ..MemRow::default()
            };
            f.host = Host {
                mem: 8 * 1_073_741_824,
                memtotal: false,
            };
            f.measured_at = "2026-09-30T00:00:00Z".into();
            judge(&f, Some(&floor), None, "R", "M", reason)
        };
        let ran_out = run(2, "1.3 GiB");
        assert_eq!(ran_out.ratchet, None);
        let roomy = run(0, "0.6 GiB");
        assert_eq!(
            roomy.ratchet.as_deref(),
            Some(format!("alpha\t1\t1\t{DRV}\tthe new seal proves two fewer\n").as_str())
        );
        assert_eq!(
            roomy.memory_rows,
            [(
                "alpha".to_string(),
                MemRow {
                    f: "1.0".into(),
                    ram: "8".into(),
                    at: "1.0".into(),
                    need: "0.6".into(),
                    bound: "=".into(),
                    when: "2026-09-30".into(),
                }
            )]
        );
    }

    #[test]
    fn a_log_that_names_no_ceiling_records_none() {
        let theirs = "targo 1.99.0-dev (bbbbbbbbb 2026-09-17) (targo 0.1.0)";
        let run = |skipped: Option<u64>, floor: &str, update: Update<'_>| {
            let mut f = facts(vec![(
                "alpha",
                Decision::Gate(Measured {
                    rc: "0".into(),
                    summary: "Summary: 3 proved (3 total)".into(),
                    skipped,
                    ..Measured::default()
                }),
            )]);
            f.crates[0].memory = MemRow {
                f: "1.0".into(),
                ..MemRow::default()
            };
            f.host = host(24);
            judge(&f, Some(floor), None, "R", "M", update)
        };
        let met = run(None, &format!("alpha\t2\t2\t{theirs}\n"), Update::RAISE);
        assert_eq!(
            met.ratchet.as_deref(),
            Some(format!("alpha\t3\t3\t{DRV}\n").as_str())
        );
        assert_eq!(met.memory_rows, [("alpha".to_string(), MemRow::default())]);
        let low = run(
            None,
            &format!("alpha\t9\t9\t{theirs}\n"),
            Update {
                on: true,
                reason: Some("the new seal proves fewer"),
            },
        );
        assert_eq!(low.ratchet, None);
        assert!(
            low.report
                .contains("RE-BASE WITHHELD: alpha: this run's log names no verifier ceiling"),
            "{}",
            low.report
        );
        let held = run(Some(2), &format!("alpha\t3\t3\t{DRV}\n"), Update::NONE);
        assert_eq!(
            held.host_obs,
            [(
                "alpha".to_string(),
                Some(["held", "-", "-", "3", "3"].map(String::from))
            )]
        );
    }

    #[test]
    fn a_lapsed_shortfall_is_undecided_never_a_pass() {
        let lapsed = |proved: u64, n: u64| {
            let mut f = facts(vec![(
                "alpha",
                Decision::Gate(Measured {
                    rc: "0".into(),
                    summary: format!("Summary: {proved} proved (9 total)"),
                    lapsed: n,
                    pace: Some(STARVED),
                    ..Measured::default()
                }),
            )]);
            f.provenance = Some(Provenance::default());
            f
        };
        let floor3 = format!("alpha\t3\t9\t{DRV}\n");
        let quiet = |f: Facts| {
            let mut f = f;
            if let Decision::Gate(m) = &mut f.crates[0].decision {
                m.pace = Some(QUIET);
            }
            f
        };
        let v = judge(
            &quiet(lapsed(2, 1)),
            Some(&floor3),
            None,
            "R",
            "M",
            Update::NONE,
        );
        assert_eq!(
            v.failed,
            ["alpha RATCHET-REGRESSION(<3) 2/9"],
            "a lapse in a run that was not starved is not the clock's: {}",
            v.report
        );
        let v = judge(&lapsed(2, 1), Some(&floor3), None, "R", "M", Update::NONE);
        assert_eq!((v.word, v.exit), ("COULD-NOT-RUN", 2));
        assert_eq!(v.unverified, ["alpha"]);
        assert_eq!(v.total, "0/0", "in neither total");
        assert_eq!(
            judge(&lapsed(2, 1), Some(&floor3), None, "R", "M", Update::RAISE).ratchet,
            None
        );
        let v = judge(&lapsed(1, 1), Some(&floor3), None, "R", "M", Update::NONE);
        assert_eq!(v.failed, ["alpha RATCHET-REGRESSION(<3) 1/9"]);
        let v = judge(&lapsed(3, 5), Some(&floor3), None, "R", "M", Update::NONE);
        assert_eq!(v.word, "PASS");
    }

    /// A proof check the lane cancelled at its deadline is the machine's in a starved run, as a
    /// backstop's stop is: every one when the floor's run is unknown, measured or remeasured,
    /// filed or not, and only those beyond the floor's run's count when it was kept. A run that
    /// was not starved decides them.
    #[test]
    fn a_cancelled_proof_check_is_credited_only_in_a_starved_run() {
        let cancelled = " ERROR ay_dpll::executor::proof::check::internal internal proof checker rejected UNSAT proof, error=proof validation cancelled by the caller (interrupt, deadline, or memory limit), checked_steps=1139, skipped_hole_steps=0, total_steps=1139\n";
        let invalid = " ERROR ay_dpll::executor::proof::check::internal internal proof checker rejected UNSAT proof, error=step t5 has invalid resolution derivation, checked_steps=4, skipped_hole_steps=0, total_steps=9\n";
        let run_at = |pace: Pace, proved: u64, n: usize| {
            let text = format!(
                "{}{}    = note: native verifier status: Inconclusive; requested=82, proved=27, failed=0, unknown=34, timed_out=0, unsupported=21\n  Summary: {proved} proved, 26 failed (553 total)\nCoverage: 101/101 eligible function bodies verified (complete)\n",
                cancelled.repeat(n),
                invalid.repeat(7),
            );
            let mut m = log(&text);
            m.rc = "1".into();
            m.pace = Some(pace);
            let mut f = facts(vec![("aterm-codec", Decision::Gate(m))]);
            f.provenance = Some(Provenance::default());
            f
        };
        let run = |proved: u64, n: usize| run_at(STARVED, proved, n);
        let only = |f: &Facts| match &f.crates[0].decision {
            Decision::Gate(m) => (m.lapsed, m.cancelled),
            _ => unreachable!(),
        };
        assert_eq!(
            only(&run(310, 131)),
            (0, 131),
            "each cancellation, and no invalid derivation"
        );
        let floor = format!("aterm-codec\t383\t551\t{DRV}\n");
        let judged = |f: &Facts, backstops: Option<&str>| {
            judge(f, Some(&floor), backstops, "R", "M", Update::NONE)
        };
        let regression = ["aterm-codec RATCHET-REGRESSION(<383) 310/553"];
        let v = judged(&run(310, 131), None);
        assert_eq!(
            row_of(&v, "aterm-codec"),
            "NOT-VERIFIED(clock,<383,131)",
            "{}",
            v.report
        );
        assert_eq!(judged(&run(310, 72), None).failed, regression);
        assert_eq!(judged(&run(310, 0), None).failed, regression);
        let v = judged(&run(383, 91), None);
        assert_ne!(v.word, "COULD-NOT-RUN", "{}", v.report);
        assert!(v.failed.is_empty(), "{:?}", v.failed);
        let v = judged(&run_at(QUIET, 310, 131), None);
        assert_eq!(v.failed, regression, "not starved: {}", v.report);

        let mut again = run(310, 131);
        if let Some(p) = again.provenance.as_mut() {
            p.remeasured
                .push("2026-10-01T00:00:00Z abc aterm-codec".into());
        }
        let mut unfiled = run(310, 131);
        unfiled.provenance = None;
        for f in [&again, &unfiled] {
            assert_eq!(
                judged(f, None).word,
                "COULD-NOT-RUN",
                "a starved run decides nothing its cancellations cover"
            );
        }

        let v = judged(&run(310, 131), Some(&kept_run("aterm-codec", &[], 100)));
        assert_eq!(v.failed, regression, "31 beyond the floor's run's 100");
        let note = v.report.lines().find(|l| l.starts_with("LOST: ")).unwrap();
        assert!(note.contains("31 proof check(s)"), "{note}");
        let v = judged(&run(310, 131), Some(&kept_run("aterm-codec", &[], 58)));
        assert_eq!(row_of(&v, "aterm-codec"), "NOT-VERIFIED(clock,<383,73)");
    }

    /// A starved run: CPU/wall 0.40 at load 30 on 14 cores.
    const STARVED: Pace = Pace {
        cpu_ms: 737_200,
        wall_ms: 1_843_000,
        load: Some(3000),
        cores: 14,
    };

    /// A quiet one: aterm-gpu's 1835 s of CPU in 1843 s at load 2 on 14 cores.
    const QUIET: Pace = Pace {
        cpu_ms: 1_835_000,
        wall_ms: 1_843_000,
        load: Some(200),
        cores: 14,
    };

    /// A real check's log and obligation record, trimmed: the report's Summary, Stop causes,
    /// Coverage and Gate lines with each function's `timed_out=` note, and the rows of its
    /// version 1 record for the functions a backstop stopped (aterm-render's: four of them),
    /// each with the stop its line in the log names (its `[stop:…]` tag, or wall-backstop for a
    /// timeout), since the report the reducer read was removed once its record was written.
    const GPU: (&str, &str, &str) = (
        "aterm-gpu@0.110.0",
        include_str!("../tests/fixtures/trust/aterm-gpu@0.110.0.log"),
        include_str!("../tests/fixtures/trust/aterm-gpu@0.110.0.log.oblig.tsv"),
    );
    const RENDER: (&str, &str, &str) = (
        "aterm-render@0.110.0",
        include_str!("../tests/fixtures/trust/aterm-render@0.110.0.log"),
        include_str!("../tests/fixtures/trust/aterm-render@0.110.0.log.oblig.tsv"),
    );

    /// Measures `check` as the lane leaves it, its log beside its `.rc`, its `.pace` (when
    /// given) and its obligation record (when `record`), through [`Plan::measure`].
    fn measured_check(
        name: &str,
        (spec, log, oblig): (&str, &str, &str),
        pace: Option<Pace>,
        record: bool,
    ) -> Facts {
        let dir = std::env::temp_dir().join(format!(
            "aterm-trust-facts-check-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        let beside = |ext: &str| dir.join(format!("{spec}.log{ext}"));
        fs::write(beside(""), log).unwrap();
        fs::write(beside(".rc"), "1\n").unwrap();
        if let Some(p) = pace {
            fs::write(beside(".pace"), format!("{}\n", p.text())).unwrap();
        }
        if record {
            fs::write(beside(crate::trust_oblig::RECORD_SUFFIX), oblig).unwrap();
        }
        let mut p = Plan::parse(&format!(
            "{PLAN_MAGIC}\nlogdir {}\ncrate {name}\t{spec}\tgate\n",
            dir.display()
        ))
        .unwrap();
        p.measure();
        let _ = fs::remove_dir_all(&dir);
        p.facts.driver = "/store/trust/15698/bin/targo".into();
        p.facts.driver_id = DRV.into();
        p.facts
    }

    fn gated(f: &Facts) -> &Measured {
        match &f.crates[0].decision {
            Decision::Gate(m) => m,
            other => panic!("{other:?}"),
        }
    }

    /// The text of [`BACKSTOPS`] keeping `name`'s floor's run: what a backstop stopped in it,
    /// and how many proof checks the lane cancelled.
    fn kept_run(name: &str, stops: &[Backstop], cancelled: u64) -> String {
        backstops_rewritten(
            None,
            name,
            &FloorRun {
                stops: stops.to_vec(),
                cancelled,
            },
        )
    }

    fn row_of(v: &Verdict, name: &str) -> String {
        v.report
            .lines()
            .find(|l| l.split_whitespace().next() == Some(name))
            .and_then(|l| l.split_whitespace().nth(1))
            .unwrap_or_default()
            .to_string()
    }

    fn note<'a>(v: &'a Verdict, head: &str) -> &'a str {
        v.report
            .lines()
            .find(|l| l.starts_with(head))
            .unwrap_or_else(|| panic!("no {head} note: {}", v.report))
    }

    #[test]
    fn a_real_checks_stops_are_read_from_its_log_and_its_record() {
        let f = measured_check("aterm-gpu", GPU, Some(QUIET), true);
        let m = gated(&f);
        assert_eq!(parse_summary(&m.summary), (Some(812), Some(4685)));
        assert_eq!((m.lapsed, m.rss, m.cancelled), (89, 0, 0));
        assert!(m.ended.is_empty(), "{}", m.ended);
        assert_eq!(m.pace, Some(QUIET));
        let stops = m.backstops.as_deref().unwrap();
        assert_eq!(stops.len(), 89, "the Stop causes line's wall-backstop=89");
        assert!(stops.iter().all(|b| b.stop == "wall-backstop"));
        assert_eq!(Facts::parse(&f.to_record()), Some(f.clone()));
        let f = measured_check("aterm-render", RENDER, None, true);
        let m = gated(&f);
        assert_eq!(parse_summary(&m.summary), (Some(1730), Some(20856)));
        assert_eq!((m.lapsed, m.rss), (747, 715), "its Stop causes line");
        let stops = m.backstops.as_deref().unwrap();
        let rss = stops.iter().filter(|b| b.stop == "rss-backstop").count();
        assert_eq!((rss, stops.len() - rss), (44, 6));
        assert_eq!(m.pace, None);
        let f = measured_check("aterm-gpu", GPU, Some(QUIET), false);
        assert_eq!(gated(&f).backstops, None, "no record, no names");
    }

    /// The acceptance of docs/DESIGN-adaptive-build-2026-10-24.md's TRUST-JUDGE on aterm-gpu's
    /// 812 of 4685: against a floor of 813, with its floor's run having stopped all but one
    /// obligation this run stopped, the lost one is named and the drop is the code's; a starved
    /// run of the same is undecided, and raises nothing.
    #[test]
    fn a_shortfall_is_the_clocks_only_in_a_starved_run_and_only_for_a_new_stop() {
        let floor = format!("aterm-gpu\t813\t4685\t{DRV}\n");
        let rec = crate::trust_oblig::Record::parse(GPU.2).unwrap();
        let alike = |o: &crate::trust_oblig::Obligation| {
            rec.rows.iter().filter(|r| r.key() == o.key()).count()
        };
        let lost = rec
            .rows
            .iter()
            .find(|o| o.backstopped() && o.status == "timeout" && alike(o) == 1)
            .unwrap();
        let all = gated(&measured_check("aterm-gpu", GPU, None, true))
            .backstops
            .clone()
            .unwrap();
        let mut had = all.clone();
        let at = had.iter().position(|b| b.key == lost.key()).unwrap();
        had.remove(at);
        let had = kept_run("aterm-gpu", &had, 0);
        let judged = |pace: Pace, backstops: Option<&str>, update: Update<'_>| {
            let f = measured_check("aterm-gpu", GPU, Some(pace), true);
            judge(&f, Some(&floor), backstops, "R", "M", update)
        };

        let v = judged(QUIET, Some(&had), Update::NONE);
        assert_eq!(v.failed, ["aterm-gpu RATCHET-REGRESSION(<813) 812/4685"]);
        let lost_note = note(&v, "LOST: ");
        assert!(
            lost_note.contains(&lost.said()) && lost_note.contains(&QUIET.said()),
            "{lost_note}"
        );
        assert!(!v.report.contains("BUDGET LAPSED"), "{}", v.report);

        // What the judge filed before: every lapse credited, the machine blamed. With no record
        // of the floor's run, a starved run is still undecided, but no stop is called new.
        let v = judged(STARVED, None, Update::NONE);
        assert_eq!(row_of(&v, "aterm-gpu"), "NOT-VERIFIED(clock,<813,89)");
        note(&v, "BUDGET LAPSED: ");
        assert!(
            ![&v.report, &v.tail]
                .iter()
                .any(|t| t.contains("floor's run did not")),
            "{}{}",
            v.report,
            v.tail
        );
        let v = judged(QUIET, None, Update::NONE);
        assert_eq!(
            v.word, "FAIL",
            "not starved, so not the clock's: {}",
            v.report
        );

        // The negative control: the one obligation the floor's run proved is stopped by a
        // clock in a starved run.
        let v = judged(STARVED, Some(&had), Update::RAISE);
        assert_eq!(row_of(&v, "aterm-gpu"), "NOT-VERIFIED(clock,<813,1)");
        assert_eq!((v.word, v.exit), ("COULD-NOT-RUN", 2));
        assert!(
            note(&v, "BUDGET LAPSED: ").contains(&STARVED.said()),
            "{}",
            v.report
        );
        assert_eq!((v.ratchet, v.backstops), (None, None), "no floor is raised");

        // A stop every run has earns nothing, starved or not.
        let v = judged(STARVED, Some(&kept_run("aterm-gpu", &all, 0)), Update::NONE);
        assert_eq!(v.failed, ["aterm-gpu RATCHET-REGRESSION(<813) 812/4685"]);
        assert!(note(&v, "LOST: ").contains(&STARVED.said()), "{}", v.report);
    }

    /// A starved run sets or raises a floor only when the machine can be credited with nothing
    /// in it: what it proved is then what a run that was not starved proves.
    #[test]
    fn a_starved_run_sets_no_floor_the_machine_can_be_credited_with() {
        let alpha = |stops: usize, cancelled: u64| {
            facts(vec![(
                "alpha",
                Decision::Gate(Measured {
                    rc: "0".into(),
                    summary: "Summary: 3 proved (9 total)".into(),
                    lapsed: stops as u64,
                    cancelled,
                    pace: Some(STARVED),
                    backstops: Some(
                        (0..stops)
                            .map(|i| Backstop {
                                key: format!("{i:016x}"),
                                stop: "wall-backstop".into(),
                                said: format!("f{i}"),
                            })
                            .collect(),
                    ),
                    ..Measured::default()
                }),
            )])
        };
        let recorded = judge(&alpha(0, 0), Some(""), None, "R", "M", Update::RAISE);
        assert_eq!(row_of(&recorded, "alpha"), "FLOOR-RECORDED");
        assert_eq!(
            recorded.ratchet.as_deref(),
            Some(format!("alpha\t3\t9\t{DRV}\n").as_str())
        );
        assert_eq!(
            floor_run(recorded.backstops.as_deref(), "alpha"),
            Some(FloorRun::default()),
            "kept, and it had none"
        );
        for (stops, cancelled) in [(1, 0), (0, 1)] {
            let v = judge(
                &alpha(stops, cancelled),
                Some(""),
                None,
                "R",
                "M",
                Update::RAISE,
            );
            assert_eq!(row_of(&v, "alpha"), "NO-FLOOR", "{}", v.report);
            assert!(note(&v, "RE-BASE WITHHELD: alpha").contains(&STARVED.said()));
            assert!(!v.report.contains("NO FLOOR: "), "{}", v.report);
            assert_eq!((v.ratchet, v.backstops), (None, None));
        }

        let gpu = |pace: Pace| measured_check("aterm-gpu", GPU, Some(pace), true);
        let all = gated(&gpu(QUIET)).backstops.clone().unwrap();
        let met = format!("aterm-gpu\t812\t4685\t{DRV}\n");
        let v = judge(&gpu(STARVED), Some(&met), None, "R", "M", Update::RAISE);
        assert_eq!(
            (v.ratchet, v.backstops),
            (None, None),
            "its floor's run unknown, every stop is the machine's: {}",
            v.report
        );
        let had = kept_run("aterm-gpu", &all, 0);
        let v = judge(
            &gpu(STARVED),
            Some(&met),
            Some(&had),
            "R",
            "M",
            Update::RAISE,
        );
        assert_eq!(
            v.ratchet.as_deref(),
            Some(met.as_str()),
            "every stop recurs, so none is the machine's: {}",
            v.report
        );

        let v = judge(
            &gpu(QUIET),
            Some(""),
            Some(&kept_run("aterm-render", &[], 0)),
            "R",
            "M",
            Update::RAISE,
        );
        assert_eq!(row_of(&v, "aterm-gpu"), "FLOOR-RECORDED");
        assert_eq!(v.ratchet.as_deref(), Some(met.as_str()));
        let kept = v.backstops.unwrap();
        assert_eq!(
            floor_run(Some(&kept), "aterm-gpu").map(|r| (r.stops.len(), r.cancelled)),
            Some((89, 0)),
            "the run's own stops, beside its floor"
        );
        assert!(floor_run(Some(&kept), "aterm-render").is_some());
        assert!(kept.starts_with(BACKSTOPS_HEADER));
        let v = judge(
            &gpu(STARVED),
            Some(&met),
            Some(&kept),
            "R",
            "M",
            Update::NONE,
        );
        assert_eq!(v.word, "PASS", "{}", v.report);
    }

    /// aterm-render's RSS stops are the machine's as its clock stops are; with no record the
    /// counts stand in, rss-backstop's among them, and a floor is not raised over stops nothing
    /// names.
    #[test]
    fn an_rss_stop_is_credited_as_a_clock_stop_is() {
        let f = measured_check("aterm-render", RENDER, Some(STARVED), true);
        let stops = gated(&f).backstops.clone().unwrap();
        let wall: Vec<Backstop> = stops
            .iter()
            .filter(|b| b.stop == "wall-backstop")
            .cloned()
            .collect();
        let judged = |f: &Facts, floor: u64, had: &[Backstop]| {
            judge(
                f,
                Some(&format!("aterm-render\t{floor}\t20856\t{DRV}\n")),
                Some(&kept_run("aterm-render", had, 0)),
                "R",
                "M",
                Update::NONE,
            )
        };
        let v = judged(&f, 1730 + 44, &wall);
        assert_eq!(row_of(&v, "aterm-render"), "NOT-VERIFIED(clock,<1774,44)");
        let v = judged(&f, 1730 + 45, &wall);
        assert_eq!(row_of(&v, "aterm-render"), "RATCHET-REGRESSION(<1775)");
        let v = judged(&f, 1730 + 44, &stops);
        assert_eq!(
            row_of(&v, "aterm-render"),
            "RATCHET-REGRESSION(<1774)",
            "the control: the floor's run had its RSS stops too"
        );

        let bare = measured_check("aterm-render", RENDER, Some(STARVED), false);
        let v = judged(&bare, 1730 + 747 + 715 - 6, &wall);
        assert_eq!(row_of(&v, "aterm-render"), "NOT-VERIFIED(clock,<3186,1456)");
        let v = judged(&bare, 1730 + 747 + 715 - 5, &wall);
        assert_eq!(row_of(&v, "aterm-render"), "RATCHET-REGRESSION(<3187)");
        let quiet = measured_check("aterm-render", RENDER, Some(QUIET), false);
        let v = judge(
            &quiet,
            Some(&format!("aterm-render\t1730\t20856\t{DRV}\n")),
            None,
            "R",
            "M",
            Update::RAISE,
        );
        assert_eq!(v.ratchet, None, "{}", v.report);
        note(&v, "RE-BASE WITHHELD: aterm-render");
    }

    /// A trustd refusal excuses only itself in a run that was not starved: what a backstop
    /// stopped there is the code's, so it cannot help cover a shortfall the refusals do not.
    #[test]
    fn a_refusal_covers_only_itself_in_a_run_that_was_not_starved() {
        let run = |pace: Pace, refused: u64| {
            facts(vec![(
                "aterm-gpu",
                Decision::Gate(Measured {
                    rc: "1".into(),
                    summary: "Summary: 812 proved (4685 total)".into(),
                    refused,
                    refused_why: "trustd unreachable: 1".into(),
                    lapsed: 89,
                    pace: Some(pace),
                    ..Measured::default()
                }),
            )])
        };
        let floor = format!("aterm-gpu\t850\t4685\t{DRV}\n");
        let judged = |f: &Facts| judge(f, Some(&floor), None, "R", "M", Update::NONE);
        let v = judged(&run(QUIET, 1));
        assert_eq!(
            v.failed,
            ["aterm-gpu RATCHET-REGRESSION(<850) 812/4685"],
            "{}",
            v.report
        );
        assert_eq!(
            row_of(&judged(&run(STARVED, 1)), "aterm-gpu"),
            "NOT-VERIFIED(trustd)"
        );
        assert_eq!(
            row_of(&judged(&run(QUIET, 38)), "aterm-gpu"),
            "NOT-VERIFIED(trustd)",
            "the refusals alone cover it"
        );
    }

    #[test]
    fn a_pace_is_starved_below_four_fifths_of_its_wall_or_above_the_cores() {
        let p = |cpu_ms, wall_ms, load| Pace {
            cpu_ms,
            wall_ms,
            load,
            cores: 14,
        };
        assert!(!p(800, 1000, Some(1400)).starved());
        assert!(p(799, 1000, Some(100)).starved());
        assert!(p(1000, 1000, Some(1401)).starved());
        assert!(!p(5000, 1000, None).starved(), "a load not read is no load");
        assert!(!p(0, 0, None).starved());
        for pace in [STARVED, QUIET, p(1, 2, None)] {
            assert_eq!(Pace::parse(&pace.text()), Some(pace));
        }
        assert_eq!(
            p(1, 2, None).said(),
            "CPU/wall 0.50, load unread on 14 cores"
        );
        for bad in ["", "-", "1 2 3", "1 2 x 4", "1 2 3 4 5"] {
            assert_eq!(Pace::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_floors_backstops_keep_each_crates_own_rows() {
        let b = |key: &str, stop: &str| Backstop {
            key: key.into(),
            stop: stop.into(),
            said: format!("f{key}"),
        };
        let run = |stops: &[Backstop], cancelled| FloorRun {
            stops: stops.to_vec(),
            cancelled,
        };
        let text = kept_run("beta", &[b("2", "rss-backstop")], 3);
        let text = backstops_rewritten(Some(&text), "alpha", &run(&[b("1", "wall-backstop")], 0));
        assert_eq!(
            text,
            format!(
                "{BACKSTOPS_HEADER}\nalpha\t-\tcancelled\t0\nalpha\t1\twall-backstop\tf1\nbeta\t-\tcancelled\t3\nbeta\t2\trss-backstop\tf2\n"
            )
        );
        assert_eq!(
            floor_run(Some(&text), "beta"),
            Some(run(&[b("2", "rss-backstop")], 3))
        );
        let text = backstops_rewritten(Some(&text), "beta", &FloorRun::default());
        assert_eq!(
            floor_run(Some(&text), "beta"),
            Some(FloorRun::default()),
            "kept, and it had none"
        );
        assert_eq!(
            floor_run(Some(&text), "alpha").map(|r| r.stops.len()),
            Some(1)
        );
        assert_eq!(floor_run(Some(&text), "gamma"), None, "never kept: unknown");
        assert_eq!(floor_run(Some(&text), "alph"), None);
        assert_eq!(floor_run(None, "alpha"), None);
        let headless = text.replace("alpha\t-\tcancelled\t0\n", "");
        assert_eq!(
            floor_run(Some(&headless), "alpha"),
            None,
            "stops without a head"
        );
        let unread = text.replace("alpha\t-\tcancelled\t0", "alpha\t-\tcancelled\tsome");
        assert_eq!(floor_run(Some(&unread), "alpha"), None);

        let mut f = measured_check("aterm-gpu", GPU, Some(QUIET), true);
        let record = f.to_record();
        let line = record.lines().find(|l| l.starts_with("backstop ")).unwrap();
        assert_eq!(
            Facts::parse(&record.replacen(&format!("{line}\n"), "", 1)),
            None
        );
        assert_eq!(
            Facts::parse(&record.replace("backstop aterm-gpu\t", "backstop aterm-gfx\t")),
            None
        );
        if let Decision::Gate(m) = &mut f.crates[0].decision {
            m.backstops = None;
        }
        assert!(!f.to_record().contains("\nbackstop "));
        assert_eq!(Facts::parse(&f.to_record()), Some(f));
    }

    /// A floor and the stops of the run that set it are written together: a write that fails
    /// leaves both files as they were.
    #[test]
    fn the_floor_files_are_written_together_or_not_at_all() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-trust-floor-files-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        let (floors, stops) = (dir.join("floors.tsv"), dir.join("backstops.tsv"));
        fs::write(&floors, "old floors\n").unwrap();
        fs::write(&stops, "old stops\n").unwrap();
        let blocked = dir.join("blocked.tsv");
        fs::create_dir_all(blocked.with_extension(format!("tsv.{}", std::process::id()))).unwrap();
        let err = write_floor_files(&[
            (&floors, Some("new floors\n")),
            (&blocked, Some("x\n")),
            (&stops, Some("new stops\n")),
        ])
        .unwrap_err();
        assert!(err.contains("blocked.tsv"), "{err}");
        assert_eq!(fs::read_to_string(&floors).unwrap(), "old floors\n");
        assert_eq!(fs::read_to_string(&stops).unwrap(), "old stops\n");
        write_floor_files(&[(&floors, Some("new floors\n")), (&stops, None)]).unwrap();
        assert_eq!(fs::read_to_string(&floors).unwrap(), "new floors\n");
        assert_eq!(fs::read_to_string(&stops).unwrap(), "old stops\n");
        let left: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tsv.") && !n.starts_with("blocked"))
            .collect();
        assert!(left.is_empty(), "nothing staged is left: {left:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_check_targo_would_not_start_is_undecided() {
        let m = log(&format!(
            "error: {TRUSTD_REFUSED}: another trustd owns the socket\n"
        ));
        assert_eq!(m.launch, "owned");
        let m = log(&format!("error: {TRUSTD_REFUSED}\n{TRUSTD_DIRTY}\n"));
        assert_eq!(m.launch, "dirty");
        assert_eq!(
            log(&format!(
                "{TRUSTD_REFUSED}\n  Summary: 1 proved (1 total)\n"
            ))
            .launch,
            "",
            "a check that reached its Summary started"
        );
        let refused = |launch: &str, held: &str| {
            facts(vec![(
                "alpha",
                Decision::Gate(Measured {
                    rc: "1".into(),
                    launch: launch.into(),
                    held: held.into(),
                    held_what: "a trustd of another build serves /tmp/t.sock".into(),
                    ..Measured::default()
                }),
            )])
        };
        let v = judge(
            &refused("owned", "foreign"),
            Some(""),
            None,
            "R",
            "M",
            Update::NONE,
        );
        assert_eq!((v.word, v.exit), ("COULD-NOT-RUN", 2));
        assert_eq!(v.unverified, ["alpha"]);
        let v = judge(
            &refused("dirty", ""),
            Some(""),
            None,
            "R",
            "M",
            Update::NONE,
        );
        assert_eq!((v.word, v.exit), ("COULD-NOT-RUN", 2));
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn the_recovery_command_survives_a_store_path_with_a_space() {
        let r = trustd_remedy("dirty", "/tmp/t.sock", "/a b/bin/trustd");
        assert!(
            r.contains("`'/a b/bin/trustd' --recover-after-crash --confirm-no-solvers --socket /tmp/t.sock`"),
            "{r}"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_recovery_probe_observes_a_matching_owned_ancestor() {
        use std::os::unix::fs::MetadataExt as _;
        use std::time::Instant;

        const CHILD: &str = "ATERM_TEST_PGREP_ANCESTOR";
        if let Some(path) = std::env::var_os(CHILD) {
            let path = PathBuf::from(path);
            let own = std::fs::metadata(std::env::current_exe().unwrap()).unwrap();
            let linked = std::fs::metadata(&path).unwrap();
            assert_eq!((own.dev(), own.ino()), (linked.dev(), linked.ino()));
            let name = path.file_name().unwrap().to_str().unwrap();
            let probe = |options: &str| {
                aterm_uds::spawnfd::command("/usr/bin/pgrep")
                    .args([options, name])
                    .output()
                    .expect("owned process-name probe")
            };
            let matches_self = |out: &std::process::Output| {
                out.status.success()
                    && String::from_utf8_lossy(&out.stdout).lines().any(|line| {
                        line.split_whitespace()
                            .next()
                            .and_then(|pid| pid.parse::<u32>().ok())
                            == Some(std::process::id())
                    })
            };
            assert!(matches_self(&probe("-alx")), "owned ancestor is observable");
            let old = probe("-lx");
            assert_eq!(old.status.code(), Some(1));
            assert!(old.stdout.is_empty());
            let current = probe(trustd_probe_options().expect("macOS probe"));
            println!(
                "OWNED_ANCESTOR_PID={} PROBE={}",
                std::process::id(),
                String::from_utf8_lossy(&current.stdout).trim()
            );
            assert!(
                matches_self(&current),
                "the declared probe must see its matching ancestor"
            );
            return;
        }

        let mut scratch = aterm_uds::deadline::KeptUntilRan::new(
            aterm_tempfile::Builder::new()
                .prefix("trustd-probe-ancestor-")
                .tempdir()
                .unwrap()
                .keep(),
        );
        let named = scratch.path().join(format!("atpa{}", std::process::id()));
        std::fs::hard_link(std::env::current_exe().unwrap(), &named).unwrap();
        let test = "trust_facts::tests::the_recovery_probe_observes_a_matching_owned_ancestor";
        let mut child = aterm_uds::spawnfd::command(&named);
        child
            .args(["--exact", test, "--nocapture"])
            .env(CHILD, &named);
        let out = aterm_uds::deadline::output_until(
            &mut child,
            Instant::now() + aterm_uds::deadline::FIRST_EXEC_HANG,
        )
        .expect("owned ancestor fixture finishes and is reaped");
        if out.status.code().is_some() {
            scratch.ran();
        }
        println!("{}", String::from_utf8_lossy(&out.stdout));
        assert!(out.status.success(), "{out:?}");
        assert!(String::from_utf8_lossy(&out.stdout).contains("OWNED_ANCESTOR_PID="));
    }

    #[test]
    fn a_remeasure_folds_only_its_own_key() {
        let prov = |key: &str| Provenance {
            key: key.into(),
            tree_sans_floors: "t".into(),
            prover: "p".into(),
            lane: LANE_WHOLE_TREE.into(),
            head: "h1".into(),
            date: "d1".into(),
            ..Provenance::default()
        };
        let mut base = facts(vec![
            ("alpha", gate("Summary: 1 proved (3 total)")),
            ("beta", gate("Summary: 2 proved (2 total)")),
        ]);
        base.provenance = Some(prov("k"));
        let mut again = facts(vec![("alpha", gate("Summary: 3 proved (3 total)"))]);
        again.provenance = Some(Provenance {
            head: "h2".into(),
            date: "d2".into(),
            ..prov("k")
        });
        let done = remeasured(&base, &again).unwrap();
        assert_eq!(done.crates[0].decision, gate("Summary: 3 proved (3 total)"));
        assert_eq!(done.crates[1], base.crates[1]);
        let p = done.provenance.unwrap();
        assert_eq!(
            (p.head.as_str(), p.remeasured),
            ("h1", vec!["d2 h2 alpha".to_string()])
        );
        let mut other_key = again.clone();
        other_key.provenance = Some(prov("k2"));
        assert!(
            remeasured(&base, &other_key)
                .unwrap_err()
                .contains("two keys")
        );
        let mut other_host = again.clone();
        other_host.host.mem = 1;
        assert!(remeasured(&base, &other_host).unwrap_err().contains("host"));
        let mut other_driver = again.clone();
        other_driver.driver_id = "targo 2".into();
        assert!(
            remeasured(&base, &other_driver)
                .unwrap_err()
                .contains("driver")
        );
        let mut stranger = again.clone();
        stranger.crates = vec![Crate::new("gamma", gate("Summary: 1 proved (1 total)"))];
        assert!(remeasured(&base, &stranger).unwrap_err().contains("gamma"));
        let text = remeasured(&base, &again).unwrap().to_record();
        assert!(text.contains("\nremeasured d2 h2 alpha\n"), "{text}");
        assert_eq!(
            Facts::parse(&text).unwrap().provenance.unwrap().remeasured,
            ["d2 h2 alpha"]
        );
    }

    fn host(gib: u64) -> Host {
        Host {
            mem: gib * 1_073_741_824,
            memtotal: false,
        }
    }

    fn mem(ram: &str, at: &str, need: &str, bound: &str) -> MemRow {
        MemRow {
            f: "16.0".into(),
            ram: ram.into(),
            at: at.into(),
            need: need.into(),
            bound: bound.into(),
            when: "m".into(),
        }
    }

    #[test]
    fn a_need_printed_at_its_own_hosts_ceiling_is_refused_and_names_what_fits() {
        let cli = [mem("8", "4.0", "2.1", "="), mem("24", "4.0", "3.0", "=")];
        let on = |gib: u64| {
            let h = host(gib);
            let row = row_for(&cli, &h).unwrap().clone();
            (memory_refusal(&h, "aterm-cli", true, &row, None), row)
        };
        let (at24, row) = on(24);
        assert_eq!(row.ram, "24");
        let at24 = at24.expect("a 24 GiB host refuses it up front");
        assert_eq!(at24.token, "~3.0");
        assert_eq!(at24.sure, Some(32));
        let (at32, row) = on(32);
        assert_eq!((at32, row.ram.as_str()), (None, "24"));
        assert!(!fits(&row, &host(32), "4.0"));
        let (at16, row) = on(16);
        assert_eq!(row.ram, "8");
        assert!(at16.is_some());
        let (at8, _) = on(8);
        assert!(at8.is_some());
    }

    #[test]
    fn a_need_from_another_ram_is_read_as_the_bound_it_is() {
        let regex = [mem("24", "3.0", "1.0", "=")];
        let census = [mem("8", "4.0", "1.3", "=")];
        for gib in [8, 16, 24, 32, 128] {
            let h = host(gib);
            let c = h.ceiling().unwrap();
            let row = row_for(&regex, &h).unwrap();
            assert_eq!(
                memory_refusal(&h, "aterm-regex", true, row, None),
                None,
                "{gib}"
            );
            assert_eq!(fits(row, &h, &c), gib <= 24, "regex {gib}");
            let row = row_for(&census, &h).unwrap();
            assert!(!fits(row, &h, &c), "census {gib}");
            assert_eq!(
                memory_refusal(&h, "aterm-census", true, row, None).is_some(),
                gib == 8,
                "census {gib}"
            );
        }
        let verify = mem("24", "3.0", "3.2", "=");
        assert!(fits(&verify, &host(24), "3.0"));
        assert_eq!(memory_refusal(&host(24), "v", true, &verify, None), None);
        let gui = [mem("128", "16.0", "16.4", ">")];
        assert!(memory_refusal(&host(24), "g", true, &gui[0], None).is_some());
        assert_eq!(row_for(&gui, &host(24)).unwrap().ram, "128");
        let both = [mem("8", "4.0", "2.1", "="), mem("24", "4.0", "3.0", "=")];
        assert_eq!(row_for(&both, &Host::default()).unwrap().ram, "24");
        assert_eq!(
            memory_refusal(&Host::default(), "x", true, &both[1], None),
            None
        );
    }

    #[test]
    fn the_memory_record_is_held_to_its_shape() {
        let ok = "# c\nalpha\t16.0\t24\t4.0\t3.0\t=\tm101\nalpha\t16.0\t8\t4.0\t2.1\t=\tA18\nbeta\t16.0\t-\t-\t-\t-\t-\n";
        let rec = parse_memory(ok, "R").unwrap();
        assert_eq!(rec["alpha"].len(), 2);
        if let Err(e) = parse_memory(include_str!("../trust/memory.tsv"), MEMORY) {
            panic!("the committed record does not parse: {e}");
        }
        assert_eq!(
            rec["beta"],
            [MemRow {
                f: "16.0".into(),
                ..MemRow::default()
            }]
        );
        for (text, rule) in [
            (
                "alpha\t16.0\t24\t4.0\t3.0\t=\tx\nalpha\t1.0\t8\t1.0\t0.3\t=\tx\n",
                "two floor ceilings",
            ),
            (
                "alpha\t16.0\t24\t4.0\t3.0\t=\tx\nalpha\t16.0\t24.0\t3.0\t3.1\t>\tx\n",
                "two rows measured on 24.0 GiB",
            ),
            (
                "alpha\t16.0\t-\t-\t-\t-\t-\nalpha\t16.0\t24\t3.0\t1.0\t=\tx\n",
                "a row with no need beside another",
            ),
            (
                "alpha\t16.0\t-\t3.0\t1.0\t=\tx\n",
                "a need names the RAM it was measured on",
            ),
            (
                "alpha\t16.0\t24\t-\t-\t-\tx\n",
                "a need names the RAM it was measured on",
            ),
            (
                "alpha\t16.0\t-\t3.0\t-\t-\tx\n",
                "the run ceiling '3.0' with no need",
            ),
            ("alpha\t16.0\tlots\t3.0\t1.0\t=\tx\n", "the host RAM 'lots'"),
            ("alpha\t16.0\t24\tx\t1.0\t=\tx\n", "the run ceiling 'x'"),
        ] {
            assert!(parse_memory(text, "R").is_err(), "{rule}");
        }
    }

    #[test]
    fn a_run_writes_the_need_it_measured_to_its_own_ram() {
        let census = |peak: &str, ceil: &str, exhausted: bool| Census {
            peak: Some(peak.into()),
            ceil: Some(ceil.into()),
            exhausted,
            ..Census::default()
        };
        let a18 = mem("8", "4.0", "2.1", "=");
        let new = mem_learn(
            &a18,
            "16.0",
            &census("3.0", "3.0", true),
            "2026-10-01",
            &host(24),
        )
        .unwrap();
        assert_eq!(
            (
                new.ram.as_str(),
                new.at.as_str(),
                new.need.as_str(),
                new.bound.as_str(),
                new.when.as_str()
            ),
            ("24", "3.0", "3.0", ">", "2026-10-01")
        );
        let own = mem("24", "4.0", "3.0", "=");
        assert_eq!(
            mem_learn(&own, "16.0", &census("2.9", "3.0", true), "d", &host(24)),
            None
        );
        let more = mem_learn(&own, "16.0", &census("3.3", "3.0", true), "d", &host(24)).unwrap();
        assert_eq!(
            (more.ram.as_str(), more.need.as_str(), more.bound.as_str()),
            ("24", "3.3", ">")
        );
        let unknown = mem_learn(
            &a18,
            "4.0",
            &census("3.0", "3.0", false),
            "d",
            &Host::default(),
        );
        assert_eq!(
            unknown,
            Some(MemRow {
                f: "4.0".into(),
                ..a18.clone()
            })
        );
        let text = "# h\naterm-cli\t16.0\t8\t4.0\t2.1\t=\tA18\nbeta\t16.0\t-\t-\t-\t-\t-\n";
        let mut learned = new.clone();
        learned.f = "4.0".into();
        let beta = mem_learn(
            &MemRow {
                f: "16.0".into(),
                ..MemRow::default()
            },
            "16.0",
            &census("0.6", "16.0", false),
            "d",
            &host(128),
        )
        .unwrap();
        let out = memory_rewritten(
            text,
            &[("aterm-cli".into(), learned), ("beta".into(), beta)],
        );
        assert_eq!(
            out,
            "# h\naterm-cli\t4.0\t8\t4.0\t2.1\t=\tA18\naterm-cli\t4.0\t24\t3.0\t3.0\t>\t2026-10-01\nbeta\t16.0\t128\t16.0\t0.6\t=\td\n"
        );
        assert!(parse_memory(&out, "R").is_ok());
    }

    #[test]
    fn the_facts_carry_each_rows_ram_and_run_ceiling() {
        let mut f = facts(vec![("aterm-cli", Decision::Memory(None))]);
        f.crates[0].memory = mem("24", "4.0", "3.0", "=");
        f.host = host(24);
        let text = f.to_record();
        assert!(
            text.contains("\ncrate aterm-cli\tmemory\t16.0\t24\t4.0\t3.0\t=\tm\t-\t-\t-\t-\n"),
            "{text}"
        );
        assert_eq!(Facts::parse(&text), Some(f.clone()));
        assert_eq!(
            Facts::parse(&text.replace(FACTS_MAGIC, "aterm-trust facts 4")),
            None
        );
        let v = judge(
            &f,
            Some(&format!("aterm-cli\t75\t443\t{DRV}\n")),
            None,
            "R",
            "M",
            Update::NONE,
        );
        assert_eq!(v.word, "NARROWED");
    }

    const REFUSED_LOG: &str = include_str!("../tests/fixtures/trust/refused-path-umask-002.log");

    const REFUSED_DIR: &str = "/home/u/.cache/aterm-judge-fixture.wGWg0z/g";

    const REFUSED_TMP: &str = "error: verified runtime-library directory path component `/tmp` is group- or world-writable (uid=0, mode=0o1777); any local principal could substitute the executed object between authorization and execution\n\
                               targo trust: error: verification did not run: no per-function TRUST_JSON transport row, authenticated coverage summary, or terminal Cargo target inventory was collected\n";

    const REFUSED_TRUSTD: &str = "targo trust: error: could not establish the selected same-sysroot trustd memory authority at /tmp/trustd-runtime-locks-1000/trustd.sock: could not authorize the same-sysroot trustd executable /home/u/tc/bin/trustd: trustd executable ancestor is foreign-owned or cross-user writable: /home/u/tc; refusing to launch a split-authority Cargo fan-out. Confirm the sibling trustd at `/home/u/tc/bin/trustd` is installed/executable and stop any incompatible live daemon.\n";

    fn writable(owner: Option<u32>, mode: u32) -> Refused {
        Refused::Writable {
            owner,
            mode: Some(mode),
            link: None,
        }
    }

    fn refusal(path: &str, why: Refused) -> PathRefusal {
        PathRefusal {
            path: path.into(),
            why,
        }
    }

    #[test]
    fn a_refused_path_is_read_in_targos_own_words() {
        let m = Measured::of_log(REFUSED_LOG.as_bytes(), "2");
        assert!(m.transport && m.summary.is_empty(), "{m:?}");
        assert_eq!(
            m.path_refused,
            [refusal(REFUSED_DIR, writable(Some(1000), 0o775))]
        );
        let (head, _) = REFUSED_LOG
            .split_once("error: verified runtime-library directory")
            .unwrap();
        let foreign = Refused::Foreign {
            owner: 501,
            me: 1000,
        };
        let cases = [
            (REFUSED_TMP.to_string(), refusal("/tmp", writable(Some(0), 0o1777))),
            (
                "error: failed to run custom build command for `x`\n\nCaused by:\n  verified tool launcher path component `/opt/trust` is owned by uid=501, which is neither root nor the invoking identity (uid=1000); an execution object a foreign principal owns can be replaced between authorization and execution\n".into(),
                refusal("/opt/trust", foreign.clone()),
            ),
            (
                "error: verified runtime-library directory path component `/srv/shared` remains writable by Targo's effective identity (uid=1000); ACL-backed write authority is incompatible with verified pathname execution\n".into(),
                refusal("/srv/shared", Refused::Acl { me: 1000 }),
            ),
            (
                "error: the directory `/home/u/.rustup/toolchains` holding protected Trust launch path symlink `/home/u/.rustup/toolchains/trust` is group- or world-writable without the sticky bit (mode=0o775); any local principal could repoint the symlink at another toolchain\n".into(),
                refusal(
                    "/home/u/.rustup/toolchains",
                    Refused::Writable {
                        owner: None,
                        mode: Some(0o775),
                        link: Some("/home/u/.rustup/toolchains/trust".into()),
                    },
                ),
            ),
            (
                "error: the directory `/opt/links` holding protected Trust launch path symlink `/opt/links/trust` is owned by uid=501, which is neither root nor the invoking identity (uid=1000); its owner can repoint the symlink at another toolchain\n".into(),
                refusal("/opt/links", foreign.clone()),
            ),
            (
                "error: protected Trust launch path symlink `/opt/links/trust` is owned by uid=501, which is neither root nor the invoking identity (uid=1000); a redirection a foreign principal owns can be repointed at another toolchain\n".into(),
                refusal("/opt/links/trust", foreign),
            ),
            (
                REFUSED_TRUSTD.into(),
                refusal(
                    "/home/u/tc",
                    Refused::Ancestor {
                        authority: "trustd executable".into(),
                    },
                ),
            ),
            (
                "trustd: trustd runtime ancestor is foreign-owned or cross-user writable: /tmp\n".into(),
                refusal(
                    "/tmp",
                    Refused::Ancestor {
                        authority: "trustd runtime".into(),
                    },
                ),
            ),
            (
                "{\"reason\":\"compiler-message\",\"message\":{\"rendered\":\"error: verified runtime-library candidate path component `/home/u/work/target-trust/j0/debug/deps/libaterm_x-1.rlib` is group- or world-writable (uid=1000, mode=0o664); any local principal could substitute the executed object between authorization and execution\\n\",\"level\":\"error\"}}\n".into(),
                refusal(
                    "/home/u/work/target-trust/j0/debug/deps/libaterm_x-1.rlib",
                    writable(Some(1000), 0o664),
                ),
            ),
            (
                "\x1b[1m\x1b[91merror\x1b[0m\x1b[1m:\x1b[0m verified tool launcher path component `\x1b[1m/opt/trust/bin\x1b[0m` is group- or world-writable (uid=0, mode=0o775); any local principal could substitute the executed object between authorization and execution\n".into(),
                refusal("/opt/trust/bin", writable(Some(0), 0o775)),
            ),
            (
                "error: verified runtime-library directory launch path symlink `/home/u/shared/troot` is owned by uid=501, which is not permitted to own a redirection into the verified toolchain (invoking uid=1000); its owner can repoint it at another sysroot\n".into(),
                refusal(
                    "/home/u/shared/troot",
                    Refused::Foreign {
                        owner: 501,
                        me: 1000,
                    },
                ),
            ),
            (
                format!("targo trust: error: {TRUSTD_REFUSED} at /tmp/trustd-runtime-locks-1000/trustd.sock: socket parent must be euid-owned/non-writable or root-owned/sticky: /tmp/trustd-runtime-locks-1000; refusing to launch a split-authority Cargo fan-out.\n"),
                refusal(
                    "/tmp/trustd-runtime-locks-1000",
                    Refused::Demand {
                        demand: "socket parent must be euid-owned/non-writable or root-owned/sticky".into(),
                    },
                ),
            ),
            (
                "targo trust: error: could not establish the private per-user trustd runtime directory: trustd runtime root must be an euid-owned 0700 directory: /tmp/trustd-runtime-locks-1000\n".into(),
                refusal(
                    "/tmp/trustd-runtime-locks-1000",
                    Refused::Demand {
                        demand: "trustd runtime root must be an euid-owned 0700 directory".into(),
                    },
                ),
            ),
            (
                format!("targo trust: error: {TRUSTD_REFUSED} at /tmp/trustd-runtime-locks-1000/trustd.sock: could not authorize the same-sysroot trustd executable /home/u/tc/bin/trustd: trustd executable must be same-user (or immutable root-owned system), regular, executable, and have no group/other write bits: /home/u/tc/bin/trustd; refusing to launch a split-authority Cargo fan-out.\n"),
                refusal(
                    "/home/u/tc/bin/trustd",
                    Refused::Demand {
                        demand: "trustd executable must be same-user (or immutable root-owned system), regular, executable, and have no group/other write bits".into(),
                    },
                ),
            ),
        ];
        for (text, want) in cases {
            let log = format!("{head}{text}{text}targo trust: error: verification did not run\n");
            let m = Measured::of_log(log.as_bytes(), "2");
            assert_eq!(m.path_refused, [want], "{text}");
        }
        let m = Measured::of_log(REFUSED_TRUSTD.as_bytes(), "2");
        assert_eq!(m.launch, "other");
        for text in [
            head,
            DONE,
            "error: verified tool launcher path component `/opt/trust/bin` is owned by uid=501, but TRUST_REQUIRE_SEALED_LAUNCHER demands a root-owned installation for a sealed-release provenance claim\n",
            "error: verified tool launcher launch path symlink `/home/u/tc` is owned by uid=1000, which is not permitted to own a redirection into the verified toolchain (invoking uid=1000); its owner can repoint it at another sysroot\n",
            "error: cannot inspect the directory `/x` holding protected Trust launch path symlink `/x/t`: No such file or directory (os error 2)\n",
            "error: the directory `/x` holding verified tool launcher launch path symlink `/x/t` is not a plain directory\n",
            "error: failed to inspect verified tool launcher path component `/x`\n",
        ] {
            let m = Measured::of_log(text.as_bytes(), "1");
            assert!(m.path_refused.is_empty(), "{text}: {:?}", m.path_refused);
        }
    }

    #[test]
    fn a_refused_path_is_said_once_with_every_library_it_failed() {
        let me = aterm_uds::peer::our_uid();
        let mine = REFUSED_LOG.replace("uid=1000", &format!("uid={me}"));
        let gated = |text: &str| Decision::Gate(Measured::of_log(text.as_bytes(), "2"));
        let ratchet = format!("eps\t5\t21\t{DRV}\n");
        let v = judge(
            &facts(vec![
                ("alpha", gated(&mine)),
                ("beta", gated(&mine)),
                ("gamma", gated(REFUSED_TMP)),
                ("delta", gated(REFUSED_TRUSTD)),
                ("eps", gate(DONE)),
            ]),
            Some(&ratchet),
            None,
            "R",
            "M",
            Update::NONE,
        );
        let said: Vec<&str> = v
            .report
            .lines()
            .filter(|l| l.starts_with("REFUSED PATH:"))
            .collect();
        assert_eq!(said.len(), 3, "{}", v.report);
        let line = |libs: &str| {
            said.iter()
                .copied()
                .find(|l| l.starts_with(&format!("REFUSED PATH: {libs}: ")))
                .unwrap_or_else(|| panic!("no line for {libs}: {said:#?}"))
        };
        let both = line("alpha beta");
        assert!(
            both.contains(REFUSED_DIR)
                && both.contains("mode 775")
                && both.ends_with(&format!("run: chmod go-w {REFUSED_DIR}")),
            "{both}"
        );
        let tmp = line("gamma");
        assert!(
            tmp.contains("/tmp")
                && tmp.contains("mode 1777")
                && !tmp.contains("chmod")
                && tmp.ends_with(REFUSED_MOVE),
            "{tmp}"
        );
        let trustd = line("delta");
        assert!(
            trustd.contains("chmod go-w /home/u/tc") && trustd.ends_with(REFUSED_MOVE),
            "{trustd}"
        );
        assert_eq!(
            v.failed,
            [
                "alpha NOT-VERIFIED(rc=2)",
                "beta NOT-VERIFIED(rc=2)",
                "gamma NOT-VERIFIED(rc=2)"
            ]
        );
        assert!(
            v.report
                .lines()
                .any(|l| l.starts_with("delta ") && l.contains("NOT-VERIFIED(trustd)")),
            "{}",
            v.report
        );
        assert_eq!(v.unverified, ["delta"]);
        assert!(
            v.tail.starts_with(
                "VERDICT: FAIL — alpha NOT-VERIFIED(rc=2); beta NOT-VERIFIED(rc=2); gamma NOT-VERIFIED(rc=2)"
            ) && v.tail.contains("REFUSED PATH")
                && v.tail.contains(&format!("run: chmod go-w {REFUSED_DIR}")),
            "{}",
            v.tail
        );
        let clean = judge(
            &facts(vec![("eps", gate(DONE))]),
            Some(&ratchet),
            None,
            "R",
            "M",
            Update::NONE,
        );
        assert!(
            !clean.report.contains("REFUSED PATH") && !clean.tail.contains("REFUSED PATH"),
            "{}",
            clean.report
        );
    }

    #[test]
    fn a_refused_paths_remedy_is_chmod_only_for_the_callers_own() {
        let me = 1000;
        let at = |why: Refused| refusal("/home/u/a b", why);
        let chmod = "chmod go-w '/home/u/a b'";
        for (said, who) in [
            (at(writable(Some(me), 0o775)).remedy(me), "the caller's"),
            (
                at(writable(Some(0), 0o775)).remedy(0),
                "root's own, to root",
            ),
        ] {
            assert!(
                said.ends_with(&format!("run: {chmod}")) && !said.contains(REFUSED_MOVE),
                "{who}: {said}"
            );
        }
        for why in [
            writable(Some(0), 0o775),
            writable(Some(0), 0o1777),
            writable(Some(me), 0o1777),
            Refused::Foreign { owner: 501, me },
            Refused::Acl { me },
        ] {
            let said = at(why.clone()).remedy(me);
            assert!(
                !said.contains("chmod") && said.ends_with(REFUSED_MOVE),
                "{why:?}: {said}"
            );
        }
        for why in [
            Refused::Writable {
                owner: None,
                mode: Some(0o775),
                link: Some("/home/u/a b/trust".into()),
            },
            Refused::Ancestor {
                authority: "trustd executable".into(),
            },
            Refused::Demand {
                demand: "trustd executable must be same-user (or immutable root-owned system), regular, executable, and have no group/other write bits".into(),
            },
        ] {
            let said = at(why.clone()).remedy(me);
            assert!(
                said.contains(&format!("run: {chmod}")) && said.ends_with(REFUSED_MOVE),
                "{why:?}: {said}"
            );
        }
        for (demand, chmod) in [
            (
                "trustd runtime root must be an euid-owned 0700 directory",
                "chmod 700 '/home/u/a b'",
            ),
            (
                "socket parent must be euid-owned/non-writable or root-owned/sticky",
                chmod,
            ),
        ] {
            let said = at(Refused::Demand {
                demand: demand.into(),
            })
            .remedy(me);
            assert!(
                said.contains(&format!("run: {chmod}")) && !said.contains(REFUSED_MOVE),
                "{demand}: {said}"
            );
        }
    }

    #[test]
    fn a_run_targo_refused_a_path_in_files_nothing() {
        let w = World::new("refused-path");
        let a = w.prover("a");
        let head = w.commit(&[("src/lib.rs", "v1"), (RATCHET, &floor(3))]);
        let logs = w.root.join("logs");
        let slots = w.root.join("troot");
        fs::create_dir_all(slots.join("j0/debug/deps")).unwrap();
        fs::create_dir_all(&logs).unwrap();
        fs::write(
            slots.join("j0/debug/deps/alpha-1.d"),
            "alpha.rmeta: crates/alpha/src/lib.rs\n",
        )
        .unwrap();
        fs::write(logs.join(".trustd-grants"), "1\n").unwrap();
        let plan = format!(
            "{PLAN_MAGIC}\ndriver {}/targo\ndriver-id {DRV}\ntrustd /tmp/t.sock\nonly \n\
             logdir {}\nratchet {}\ncrate alpha\talpha@1.0.0\tgate\n\
             store {}\ncommit {head}\ntree x\nprover {}\nprover-bin {}\ntarget-root {}\n\
             lane-pin SOURCE_DATE_EPOCH=0\nlane-passed PATH\nlane-passed HOME\nlane-passed CARGO_TARGET_DIR\nfile \n",
            a.bin.display(),
            logs.display(),
            w.work().join(RATCHET).display(),
            w.store().display(),
            a.identity.as_ref().unwrap(),
            a.bin.display(),
            slots.display(),
        );
        let run = |log: &str, rc: &str| {
            fs::write(logs.join("alpha@1.0.0.log"), log).unwrap();
            fs::write(logs.join("alpha@1.0.0.log.rc"), rc).unwrap();
            let mut p = Plan::parse(&plan).unwrap();
            conclude(&w.cli(), &w.work(), &mut p, Update::NONE).unwrap()
        };
        let done = run(REFUSED_LOG, "2\n");
        let l = done.facts_line.unwrap_or_default();
        assert!(
            l.starts_with("FACTS: none") && l.contains(REFUSED_DIR) && l.contains("alpha"),
            "{l}"
        );
        assert!(done.verdict.report.contains("REFUSED PATH: alpha: "));
        assert!(w.found(&a, "HEAD").is_err(), "nothing was filed");
        let done = run(
            "  Summary: 3 proved, 0 failed (3 total)\nCoverage: 2/2 eligible function bodies verified (complete)\n",
            "0\n",
        );
        let l = done.facts_line.unwrap_or_default();
        assert!(l.starts_with("FACTS: ") && !l.contains("none"), "{l}");
        assert!(w.found(&a, "HEAD").is_ok(), "the control is found");
    }
}
