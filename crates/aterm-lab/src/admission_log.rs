// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ADMISSION LOG'S READER (`lab admission [--since DURATION] [--json]`; the landing
//! economy's slice 2, `docs/DESIGN-landing-economy.md`, sub-slice 2a): it reads every rotated file
//! of this account's admission log ([`aterm_capacity::admission_log`]) and prints the slice's
//! exit tests as its plan defines them, each one able to fail:
//! - **E1**, claim against peak at matching width: Σclaim/Σpeak, bytes-weighted, over the stage
//!   claims sized by an own record (`learned`, `elsewhere`) kept at the width the stage ran at: a
//!   pooled run's claim only where the record keyed by its compiler jobs sized it (its row's
//!   `keyed_jobs` is its `width`), an unpooled run's where it had no ceiling (its record is the
//!   host's width's). Each peak is 4 GiB or more and its record kept it (not light). It is gated at
//!   1.3 over at least 30 claims. Beside it: the under-claims per 100, every claim's ratio, the
//!   light share, a basis × stage table, the p90, the test binaries by width class, and G-bin
//!   (mismatched binaries above 1.3 over at least 50).
//! - **E2**, memory-class waits of a minute or more. A wait is avoidable where a look at least a
//!   minute before its end left room for its own true peak's claim once each held claim of
//!   memory counts what its true peak still adds (`room*`). Each avoidable wait is tagged by cause
//!   (`head`, `holders`, `ahead`, `own`, `other`). One avoidable wait fails the gate at any count;
//!   with none it passes over at least 10 waits, and fewer are vacuous.
//! - **E3**, before land judgments take places (2g): land judgments that are heavy by their CPU
//!   after the fact (at least cores × 300 s), and how often they overlapped.
//! - **E4**, the starved share of test binaries' CPU and of compiles'.
//! - **§4 row 2**'s figures: heavy land judgments' mean wall, and completed judgments' CPU per
//!   hour.
//! - **Coverage**: rows lost, the held claims of memory looks were held to whose true peak is
//!   known, and how far back the log, and its run rows, reach whatever the window.
//!
//! A gate's verdict is a figure for the measured day, not this command's status: it exits 0
//! whatever the verdicts are. It writes nothing and claims nothing.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use aterm_capacity::admission_log::{self, Reading, Row};
use aterm_json::{Map, Value};

use crate::{Fail, Res};

const USAGE: &str = "admission [--since DURATION] [--json]";
const GIB: u64 = 1 << 30;
const HOUR_MS: u64 = 3_600_000;
/// The window read when `--since` is not given: a day.
const DEFAULT_SINCE_MS: u64 = 24 * HOUR_MS;
/// E1's population has peaks of at least this size, where `peak_claim`'s margin is at most
/// 1.25× ([`aterm_capacity::peak_claim`]).
const E1_FLOOR: u64 = 4 * GIB;
/// E1's bound, and G-bin's.
const GATE: f64 = 1.3;
/// Fewer claims than this in E1's population leave it vacuous.
const E1_LEAST: u64 = 30;
/// A memory-class wait lasts at least this long, and a look that judges it is at least this
/// long before its end.
const WAIT_LEAST_MS: u64 = 60_000;
/// Fewer memory-class waits than this, with none avoidable, leave E2 vacuous.
const E2_LEAST: usize = 10;
/// Fewer heavy land judgments than this leave E3 vacuous.
const E3_LEAST: usize = 20;
/// A judgment is heavy when its CPU is at least this many milliseconds per core
/// (`H = cores × 300 s`).
const HEAVY_MS_PER_CORE: u64 = 300_000;
/// G-bin reads the mismatched binaries once there are at least this many.
const G_BIN_LEAST: u64 = 50;

/// `lab admission`: read the log in this account's state folder and print what it says.
///
/// # Errors
/// A usage error, an account with no state folder, or a log file that exists and cannot be read.
pub fn cmd(args: &[String]) -> Res {
    let args = parse(args)?;
    let dir = admission_log::default_dir()
        .ok_or_else(|| Fail::die("admission: this account has no state folder"))?;
    let now_ms = aterm_time::unix_millis()
        .ok_or_else(|| Fail::die("admission: the clock is before 1970"))?;
    let since_ms = now_ms.saturating_sub(args.since_ms);
    let reading = admission_log::read(&dir, Some(since_ms))
        .map_err(|e| Fail::die(format!("admission: {}: {e}", dir.display())))?;
    let cores = u64::from(aterm_capacity::cores());
    let report = judge(
        &reading,
        Frame {
            cores,
            since_ms,
            now_ms,
        },
    );
    if args.json {
        let text = aterm_json::to_string(&report.json(&dir))
            .map_err(|e| Fail::die(format!("admission: {e}")))?;
        println!("{text}");
    } else {
        print!("{}", report.text(&dir));
    }
    Ok(0)
}

/// What the verb was asked.
#[derive(Debug, PartialEq, Eq)]
struct Args {
    /// How far back to read.
    since_ms: u64,
    /// Print JSON.
    json: bool,
}

fn parse(args: &[String]) -> Result<Args, Fail> {
    let mut out = Args {
        since_ms: DEFAULT_SINCE_MS,
        json: false,
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--json" => out.json = true,
            "--since" => {
                out.since_ms = it
                    .next()
                    .and_then(|v| duration_ms(v))
                    .ok_or_else(|| Fail::usage(USAGE))?;
            }
            _ => return Err(Fail::usage(USAGE)),
        }
    }
    Ok(out)
}

/// A count followed by its unit (`s`, `m`, `h` or `d`), in milliseconds. `None` for zero, a
/// missing unit, or anything that overflows.
fn duration_ms(text: &str) -> Option<u64> {
    let at = text.find(|c: char| !c.is_ascii_digit())?;
    let (count, unit) = text.split_at(at);
    let per = match unit {
        "s" => 1_000,
        "m" => 60_000,
        "h" => HOUR_MS,
        "d" => 24 * HOUR_MS,
        _ => return None,
    };
    count
        .parse::<u64>()
        .ok()
        .filter(|n| *n > 0)?
        .checked_mul(per)
}

/// What a reading is judged against: the machine's cores (for H), and the window.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Frame {
    /// The machine's cores.
    pub(crate) cores: u64,
    /// The window's start, unix ms.
    pub(crate) since_ms: u64,
    /// Its end: now.
    pub(crate) now_ms: u64,
}

/// Claims and the peaks their work reached, summed over `n` claims.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Sum {
    /// Σ claimed bytes.
    pub(crate) claim: u64,
    /// Σ peak bytes.
    pub(crate) peak: u64,
    /// Claims summed.
    pub(crate) n: u64,
}

impl Sum {
    fn add(&mut self, claim: u64, peak: u64) {
        self.claim = self.claim.saturating_add(claim);
        self.peak = self.peak.saturating_add(peak);
        self.n += 1;
    }

    /// Σclaim/Σpeak: bytes-weighted. `None` with no peak.
    pub(crate) fn ratio(self) -> Option<f64> {
        (self.peak > 0).then(|| self.claim as f64 / self.peak as f64)
    }

    fn json(self) -> Value {
        obj([
            ("claim", self.claim.into()),
            ("peak", self.peak.into()),
            ("n", self.n.into()),
            ("ratio", self.ratio().into()),
        ])
    }

    fn text(self) -> String {
        format!("{} over {}", ratio(self.ratio()), self.n)
    }
}

/// Whether a gate holds over its population.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// It holds.
    Pass,
    /// It fails.
    Fail,
    /// Too few in the population to say.
    #[default]
    Vacuous,
}

impl Verdict {
    const fn word(self) -> &'static str {
        match self {
            Verdict::Pass => "pass",
            Verdict::Fail => "fail",
            Verdict::Vacuous => "vacuous",
        }
    }
}

/// One width class of the test binaries (a `tests` row's `classes`), summed over the window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Binaries {
    /// Σclaim, Σpeak and the count.
    pub(crate) sum: Sum,
    /// Binaries whose claim was more than 1.3× their peak.
    pub(crate) over: u64,
    /// Binaries whose peak was more than their claim.
    pub(crate) under: u64,
    /// What those under-claims missed by, in bytes.
    pub(crate) under_bytes: u64,
}

impl Binaries {
    fn add(&mut self, class: &Value) {
        let field = |k: &str| class.get(k).and_then(Value::as_u64).unwrap_or(0);
        self.sum.claim = self.sum.claim.saturating_add(field("claim"));
        self.sum.peak = self.sum.peak.saturating_add(field("peak"));
        self.sum.n = self.sum.n.saturating_add(field("count"));
        self.over = self.over.saturating_add(field("over"));
        self.under = self.under.saturating_add(field("under"));
        self.under_bytes = self.under_bytes.saturating_add(field("under_bytes"));
    }

    fn json(self) -> Value {
        obj([
            ("sum", self.sum.json()),
            ("over", self.over.into()),
            ("under", self.under.into()),
            ("under_bytes", self.under_bytes.into()),
        ])
    }
}

/// E1: claim against peak at matching width.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct E1 {
    /// The population: own-record claims sized at the width they ran at, peak ≥ 4 GiB, not
    /// light.
    pub(crate) matched: Sum,
    /// The population's claims under their peak.
    pub(crate) under: u64,
    /// Every stage claim with a peak, light ones included.
    pub(crate) all: Sum,
    /// Those the stage's record kept as light.
    pub(crate) light: u64,
    /// Every claim with a peak, by its basis and its stage.
    pub(crate) table: BTreeMap<(String, String), Sum>,
    /// The population's 90th-percentile claim/peak.
    pub(crate) p90: Option<f64>,
    /// The test binaries by width class (`matched`, `mismatched`, `unseen`).
    pub(crate) binaries: BTreeMap<String, Binaries>,
    /// The gate: ≤ 1.3 over at least 30 claims.
    pub(crate) verdict: Verdict,
    /// G-bin: mismatched binaries' ratio is above 1.3 over at least 50 (its pass builds 2i).
    pub(crate) g_bin: Verdict,
}

impl E1 {
    /// The population's under-claims per 100 claims.
    fn under_per_100(&self) -> Option<f64> {
        (self.matched.n > 0).then(|| self.under as f64 * 100.0 / self.matched.n as f64)
    }

    /// The share of claims whose record kept them light.
    fn light_share(&self) -> Option<f64> {
        (self.all.n > 0).then(|| self.light as f64 / self.all.n as f64)
    }

    fn of(rows: &[Row]) -> E1 {
        let mut e1 = E1::default();
        let mut ratios = Vec::new();
        for row in rows.iter().filter(|r| r.kind() == "stage") {
            let (Some(claim), Some(peak)) = (row.u64("claimed"), row.u64("peak")) else {
                continue;
            };
            if peak == 0 {
                continue;
            }
            let basis = row.str("basis").unwrap_or("none");
            let kept = row.str("kept");
            e1.all.add(claim, peak);
            if matches!(kept, Some("light" | "light-beside")) {
                e1.light += 1;
            }
            e1.table
                .entry((basis.to_owned(), row.str("stage").unwrap_or("?").to_owned()))
                .or_default()
                .add(claim, peak);
            let own = matches!(basis, "learned" | "elsewhere");
            // A pooled claim matches where its width's own record sized it; an unpooled one where
            // no ceiling narrowed it below the host's width its record was kept at.
            let width = if row.flag("pooled") {
                row.u64("keyed_jobs").is_some() && row.u64("keyed_jobs") == row.u64("width")
            } else {
                row.u64("ceiling").is_none()
            };
            if own && width && peak >= E1_FLOOR && kept == Some("kept") {
                e1.matched.add(claim, peak);
                if peak > claim {
                    e1.under += 1;
                }
                ratios.push(claim as f64 / peak as f64);
            }
        }
        ratios.sort_by(f64::total_cmp);
        e1.p90 = nearest_rank(&ratios, 90);
        for row in rows.iter().filter(|r| r.kind() == "tests") {
            let Some(classes) = row.get("classes").and_then(Value::as_object) else {
                continue;
            };
            for (name, class) in classes {
                e1.binaries.entry(name.clone()).or_default().add(class);
            }
        }
        e1.verdict = if e1.matched.n < E1_LEAST {
            Verdict::Vacuous
        } else if e1.matched.ratio().is_some_and(|r| r <= GATE) {
            Verdict::Pass
        } else {
            Verdict::Fail
        };
        let mismatched = e1
            .binaries
            .get("mismatched")
            .copied()
            .unwrap_or_default()
            .sum;
        e1.g_bin = if mismatched.n < G_BIN_LEAST {
            Verdict::Vacuous
        } else if mismatched.ratio().is_some_and(|r| r > GATE) {
            Verdict::Pass
        } else {
            Verdict::Fail
        };
        e1
    }

    fn json(&self) -> Value {
        let table: Vec<Value> = self
            .table
            .iter()
            .map(|((basis, stage), sum)| {
                obj([
                    ("basis", basis.as_str().into()),
                    ("stage", stage.as_str().into()),
                    ("sum", sum.json()),
                ])
            })
            .collect();
        let binaries: Map = self
            .binaries
            .iter()
            .map(|(k, b)| (k.clone(), b.json()))
            .collect();
        obj([
            ("matched", self.matched.json()),
            ("under", self.under.into()),
            ("under_per_100", self.under_per_100().into()),
            ("all", self.all.json()),
            ("light", self.light.into()),
            ("light_share", self.light_share().into()),
            ("p90", self.p90.into()),
            ("table", table.into()),
            ("binaries", Value::Object(binaries)),
            ("verdict", self.verdict.word().into()),
            ("g_bin", self.g_bin.word().into()),
        ])
    }
}

/// The value at the `pct`th percentile of the sorted `values`, by nearest rank.
fn nearest_rank(values: &[f64], pct: usize) -> Option<f64> {
    let rank = (values.len() * pct).div_ceil(100);
    values.get(rank.checked_sub(1)?).copied()
}

/// Why an avoidable wait waited.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cause {
    /// A claim waiting ahead that no claim's end could fund (its look row's `unfundable`).
    Head,
    /// What the held claims reserve kept it out, though what their true peaks still add left
    /// room for it.
    Holders,
    /// The held claims' reservations left room for its claim; the claims waiting ahead of it
    /// (none flagged unfundable) did not: it waited its turn in the queue.
    Ahead,
    /// Room for its own true peak's claim, not for the claim it asked.
    Own,
    /// Room for its claim beside every reservation too: it waited for something else as well
    /// (pressure, ALONE).
    Other,
}

impl Cause {
    const fn word(self) -> &'static str {
        match self {
            Cause::Head => "head",
            Cause::Holders => "holders",
            Cause::Ahead => "ahead",
            Cause::Own => "own",
            Cause::Other => "other",
        }
    }
}

/// One memory-class wait of a minute or more, as its `look` rows tell it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Wait {
    /// Its claim's id.
    pub(crate) id: String,
    /// What it is.
    pub(crate) what: String,
    /// Its first memory-class look, unix ms.
    pub(crate) first_ms: u64,
    /// Its admission, or the last look the log holds where it was not admitted in the window.
    pub(crate) end_ms: u64,
    /// Whether it was admitted.
    pub(crate) ended: bool,
    /// Its claim: memory and the margin beside it.
    pub(crate) claim: u64,
    /// What its own true peak claims ([`aterm_capacity::peak_claim`]): from its `stage` row,
    /// else of its claimed memory.
    pub(crate) needs: u64,
    /// The most `room*` a look at least a minute before its end left: available, less what each
    /// held claim's true peak still adds. `None` where no such look read available memory.
    pub(crate) room: Option<u64>,
    /// Why it waited, where it was avoidable: `room*` covered what its own true peak claims.
    pub(crate) cause: Option<Cause>,
}

impl Wait {
    fn json(&self) -> Value {
        obj([
            ("id", self.id.as_str().into()),
            ("what", self.what.as_str().into()),
            ("first_ms", self.first_ms.into()),
            ("end_ms", self.end_ms.into()),
            (
                "waited_ms",
                self.end_ms.saturating_sub(self.first_ms).into(),
            ),
            ("ended", self.ended.into()),
            ("claim", self.claim.into()),
            ("needs", self.needs.into()),
            ("room", self.room.into()),
            ("cause", self.cause.map(Cause::word).into()),
        ])
    }
}

/// E2: no memory-short wait while free memory covers the peak.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct E2 {
    /// The memory-class waits of a minute or more, oldest first.
    pub(crate) waits: Vec<Wait>,
    /// The gate: one avoidable wait fails it at any count; with none it passes over at least 10
    /// waits, and fewer are vacuous.
    pub(crate) verdict: Verdict,
}

impl E2 {
    /// The waits avoidable for `cause`.
    pub(crate) fn avoidable(&self, cause: Cause) -> usize {
        self.waits.iter().filter(|w| w.cause == Some(cause)).count()
    }

    fn of(rows: &[Row], peaks: &BTreeMap<String, u64>) -> E2 {
        let mut looks: BTreeMap<&str, Vec<&Row>> = BTreeMap::new();
        for row in rows.iter().filter(|r| r.kind() == "look") {
            if let Some(id) = row.str("id") {
                looks.entry(id).or_default().push(row);
            }
        }
        let admitted: BTreeMap<&str, u64> = rows
            .iter()
            .filter(|r| r.kind() == "admitted")
            .filter_map(|r| {
                Some((
                    r.str("id")?,
                    r.u64("admitted_ms").unwrap_or_else(|| r.at_ms()),
                ))
            })
            .collect();
        let mut waits = Vec::new();
        for (id, mut rows) in looks {
            rows.sort_by_key(|r| r.at_ms());
            let waiting: Vec<&Row> = rows
                .iter()
                .copied()
                .filter(|r| !r.flag("admitted"))
                .collect();
            let Some(first) = waiting.first() else {
                continue;
            };
            // The claims that looked for it: the one its id names, and each later look's own
            // where another (a caller that entered a fresh waiter at each look).
            let claims: BTreeSet<&str> = std::iter::once(id)
                .chain(rows.iter().filter_map(|r| r.str("claim")))
                .collect();
            let admitting = rows.iter().find(|r| r.flag("admitted")).map(|r| r.at_ms());
            let decided = || claims.iter().find_map(|c| admitted.get(c).copied());
            let (end_ms, ended) = match admitting.or_else(decided) {
                Some(at) => (at, true),
                None => (waiting.last().map_or(first.at_ms(), |r| r.at_ms()), false),
            };
            if end_ms.saturating_sub(first.at_ms()) < WAIT_LEAST_MS {
                continue;
            }
            let mem = first.u64("mem").unwrap_or(0);
            let claim = mem.saturating_add(first.u64("margin").unwrap_or(0));
            let own_peak = claims.iter().find_map(|c| peaks.get(*c).copied());
            let needs = aterm_capacity::peak_claim(own_peak.unwrap_or(mem));
            let mut best: Option<(u64, &Row)> = None;
            for look in waiting
                .iter()
                .filter(|r| r.at_ms().saturating_add(WAIT_LEAST_MS) <= end_ms)
            {
                if let Some(room) = room(look, peaks)
                    && best.is_none_or(|(b, _)| room > b)
                {
                    best = Some((room, look));
                }
            }
            let cause = best
                .filter(|(room, _)| *room >= needs)
                .map(|(room, look)| cause(look, room, claim));
            waits.push(Wait {
                id: id.to_owned(),
                what: first.str("what").unwrap_or("?").to_owned(),
                first_ms: first.at_ms(),
                end_ms,
                ended,
                claim,
                needs,
                room: best.map(|(room, _)| room),
                cause,
            });
        }
        waits.sort_by_key(|w| w.first_ms);
        let verdict = if waits.iter().any(|w| w.cause.is_some()) {
            Verdict::Fail
        } else if waits.len() < E2_LEAST {
            Verdict::Vacuous
        } else {
            Verdict::Pass
        };
        E2 { waits, verdict }
    }

    fn json(&self) -> Value {
        let by_cause: Map = [
            Cause::Head,
            Cause::Holders,
            Cause::Ahead,
            Cause::Own,
            Cause::Other,
        ]
        .into_iter()
        .map(|c| (c.word().to_owned(), (self.avoidable(c) as u64).into()))
        .collect();
        obj([
            (
                "waits",
                self.waits.iter().map(Wait::json).collect::<Vec<_>>().into(),
            ),
            ("avoidable", Value::Object(by_cause)),
            ("verdict", self.verdict.word().into()),
        ])
    }
}

/// The held claims of memory `look` was held to, each as `(id, claimed, reserved)`: its
/// `held_mem`, every one whatever its shortfall names; where a row has none, the held ones among
/// its shortfall's `holders`.
fn held(look: &Row) -> impl Iterator<Item = (&str, u64, u64)> {
    let listed = look.get("held_mem").and_then(Value::as_array);
    let holders = listed
        .is_none()
        .then(|| look.get("holders").and_then(Value::as_array))
        .flatten();
    listed
        .into_iter()
        .flatten()
        .chain(
            holders
                .into_iter()
                .flatten()
                .filter(|h| h.get("held").and_then(Value::as_bool) == Some(true)),
        )
        .filter_map(|h| {
            Some((
                h.get("id")?.as_str()?,
                h.get("claimed").and_then(Value::as_u64).unwrap_or(0),
                h.get("reserved").and_then(Value::as_u64).unwrap_or(0),
            ))
        })
}

/// What the claims waiting ahead of `look`'s waiter that its shortfall names still reserve.
fn ahead_reserve(look: &Row) -> u64 {
    look.get("holders")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|h| h.get("held").and_then(Value::as_bool) == Some(false))
        .filter_map(|h| h.get("reserved").and_then(Value::as_u64))
        .fold(0, u64::saturating_add)
}

/// `room*` at `look`: what it read available, less what each held claim of memory's true peak
/// still adds to what it already uses (`max(0, truepeak − (claimed − reserved))`). A holder's true peak is
/// the peak of its `stage` row in `peaks`; a holder with none (a plain build, the lab, a release,
/// a test binary, a killed run) counts its claim as its true peak, which adds what it reserves.
/// `None` where the look read no available memory.
pub(crate) fn room(look: &Row, peaks: &BTreeMap<String, u64>) -> Option<u64> {
    let available = look.u64("available")?;
    let adds = held(look).fold(0_u64, |sum, (id, claimed, reserved)| {
        let uses = claimed.saturating_sub(reserved);
        let peak = peaks.get(id).copied().unwrap_or(claimed);
        sum.saturating_add(peak.saturating_sub(uses))
    });
    Some(available.saturating_sub(adds))
}

/// Why the avoidable wait judged at `look` (where `room*` was `room`) waited for its `claim`:
/// a head no claim's end can fund; else its own claim, where `room*` held its true peak's claim
/// and not that; else the held claims' reservations, where those alone left too little beside
/// it; else the claims waiting ahead, where every reservation ahead (`Short::Memory`'s
/// `reserved`, held and waiting; where the look was short of no memory itself, what the held
/// claims and the claims named waiting ahead reserve) did; else something else.
fn cause(look: &Row, room: u64, claim: u64) -> Cause {
    let holders = look.get("holders").and_then(Value::as_array);
    let unfundable = holders.into_iter().flatten().any(|h| {
        h.get("held").and_then(Value::as_bool) == Some(false)
            && h.get("unfundable").and_then(Value::as_bool) == Some(true)
    });
    if unfundable {
        return Cause::Head;
    }
    if room < claim {
        return Cause::Own;
    }
    let available = look.u64("available").unwrap_or(0);
    let held_reserve = held(look).fold(0_u64, |sum, (_, _, reserved)| sum.saturating_add(reserved));
    if available.saturating_sub(held_reserve) < claim {
        return Cause::Holders;
    }
    let reserved = look
        .u64("reserved")
        .unwrap_or_else(|| held_reserve.saturating_add(ahead_reserve(look)));
    if available.saturating_sub(reserved) < claim {
        Cause::Ahead
    } else {
        Cause::Other
    }
}

/// One land judgment's run, from its `run` row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Judgment {
    /// Its run id.
    pub(crate) run: String,
    /// Its start, unix ms.
    pub(crate) start_ms: u64,
    /// Its end.
    pub(crate) end_ms: u64,
    /// Its CPU after the fact: the larger of its stages' observed CPU and its test binaries'
    /// measured CPU (the test stage's meter may already count its binaries), a lower bound
    /// either way.
    pub(crate) cpu_ms: u64,
    /// Its outcome word.
    pub(crate) outcome: String,
}

impl Judgment {
    fn of(row: &Row) -> Option<Judgment> {
        (row.kind() == "run" && row.flag("land")).then(|| Judgment {
            run: row.str("run").unwrap_or("?").to_owned(),
            start_ms: row.u64("start_ms").unwrap_or_else(|| row.at_ms()),
            end_ms: row.u64("end_ms").unwrap_or_else(|| row.at_ms()),
            cpu_ms: row
                .u64("stage_cpu_ms")
                .unwrap_or(0)
                .max(row.u64("test_cpu_ms").unwrap_or(0)),
            outcome: row.str("outcome").unwrap_or("?").to_owned(),
        })
    }

    fn wall_ms(&self) -> u64 {
        self.end_ms.saturating_sub(self.start_ms)
    }

    /// Whether it reached a verdict.
    fn completed(&self) -> bool {
        matches!(self.outcome.as_str(), "pass" | "fail")
    }
}

/// E3 before places exist: land judgments heavy by their CPU after the fact, and their overlaps.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct E3 {
    /// H, in CPU ms: cores × 300 s.
    pub(crate) heavy_ms: u64,
    /// Land judgments in the window.
    pub(crate) land: usize,
    /// The heavy ones, oldest first.
    pub(crate) heavy: Vec<Judgment>,
    /// Pairs of heavy judgments whose runs overlapped.
    pub(crate) pairs: u64,
    /// The time two or more heavy judgments ran at once.
    pub(crate) overlap_ms: u64,
    /// The most heavy judgments that ran at once.
    pub(crate) most: usize,
    /// They ran in sequence (no pair overlapped), over at least 20.
    pub(crate) verdict: Verdict,
}

impl E3 {
    fn of(judgments: &[Judgment], cores: u64) -> E3 {
        let heavy_ms = cores.max(1).saturating_mul(HEAVY_MS_PER_CORE);
        let mut heavy: Vec<Judgment> = judgments
            .iter()
            .filter(|j| j.cpu_ms >= heavy_ms)
            .cloned()
            .collect();
        heavy.sort_by_key(|j| (j.start_ms, j.end_ms));
        let mut pairs = 0_u64;
        for (i, a) in heavy.iter().enumerate() {
            for b in &heavy[i + 1..] {
                if a.start_ms < b.end_ms && b.start_ms < a.end_ms {
                    pairs += 1;
                }
            }
        }
        // A sweep over the runs' ends (taken before a start at the same instant).
        let mut edges: Vec<(u64, i64)> = heavy
            .iter()
            .flat_map(|j| [(j.start_ms, 1), (j.end_ms, -1)])
            .collect();
        edges.sort_unstable();
        let (mut running, mut most, mut overlap_ms, mut since) = (0_i64, 0_i64, 0_u64, 0_u64);
        for (at, step) in edges {
            if running >= 2 {
                overlap_ms = overlap_ms.saturating_add(at.saturating_sub(since));
            }
            running += step;
            most = most.max(running);
            since = at;
        }
        let verdict = if heavy.len() < E3_LEAST {
            Verdict::Vacuous
        } else if pairs == 0 {
            Verdict::Pass
        } else {
            Verdict::Fail
        };
        E3 {
            heavy_ms,
            land: judgments.len(),
            heavy,
            pairs,
            overlap_ms,
            most: usize::try_from(most).unwrap_or(0),
            verdict,
        }
    }

    fn json(&self) -> Value {
        let heavy: Vec<Value> = self
            .heavy
            .iter()
            .map(|j| {
                obj([
                    ("run", j.run.as_str().into()),
                    ("start_ms", j.start_ms.into()),
                    ("end_ms", j.end_ms.into()),
                    ("cpu_ms", j.cpu_ms.into()),
                    ("outcome", j.outcome.as_str().into()),
                ])
            })
            .collect();
        obj([
            ("by", "post-hoc cpu".into()),
            ("heavy_ms", self.heavy_ms.into()),
            ("land", (self.land as u64).into()),
            ("heavy", heavy.into()),
            ("pairs", self.pairs.into()),
            ("overlap_ms", self.overlap_ms.into()),
            ("most", (self.most as u64).into()),
            ("verdict", self.verdict.word().into()),
        ])
    }
}

/// Starved time beside the CPU it was starved of.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Starved {
    /// Σ starved ms.
    pub(crate) starved_ms: u64,
    /// Σ CPU ms.
    pub(crate) cpu_ms: u64,
    /// Rows summed.
    pub(crate) n: u64,
}

impl Starved {
    fn add(&mut self, starved_ms: u64, cpu_ms: u64) {
        self.starved_ms = self.starved_ms.saturating_add(starved_ms);
        self.cpu_ms = self.cpu_ms.saturating_add(cpu_ms);
        self.n += 1;
    }

    /// Σ starved / Σ(starved + CPU). `None` with neither.
    pub(crate) fn share(self) -> Option<f64> {
        let all = self.starved_ms.saturating_add(self.cpu_ms);
        (all > 0).then(|| self.starved_ms as f64 / all as f64)
    }

    fn json(self) -> Value {
        obj([
            ("starved_ms", self.starved_ms.into()),
            ("cpu_ms", self.cpu_ms.into()),
            ("n", self.n.into()),
            ("share", self.share().into()),
        ])
    }
}

/// E4: the starved share.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct E4 {
    /// The test binaries', from the `tests` rows.
    pub(crate) tests: Starved,
    /// The compiles', from the `stage` rows whose compile accounts read starved time.
    pub(crate) compiles: Starved,
}

impl E4 {
    fn of(rows: &[Row]) -> E4 {
        let mut e4 = E4::default();
        for row in rows {
            match row.kind() {
                "tests" => e4.tests.add(
                    row.u64("starved_ms").unwrap_or(0),
                    row.u64("cpu_ms").unwrap_or(0),
                ),
                "stage" => {
                    if let Some(starved) = row.u64("starved_ms") {
                        e4.compiles.add(starved, row.u64("cpu_ms").unwrap_or(0));
                    }
                }
                _ => {}
            }
        }
        e4
    }

    /// Both together.
    fn all(self) -> Starved {
        Starved {
            starved_ms: self
                .tests
                .starved_ms
                .saturating_add(self.compiles.starved_ms),
            cpu_ms: self.tests.cpu_ms.saturating_add(self.compiles.cpu_ms),
            n: self.tests.n + self.compiles.n,
        }
    }

    fn json(self) -> Value {
        obj([
            ("tests", self.tests.json()),
            ("compiles", self.compiles.json()),
            ("all", self.all().json()),
        ])
    }
}

/// §4 row 2's figures, as far as the log holds them before 2g.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Row2 {
    /// Heavy land judgments' mean wall, from their run's start (the landing's `since` is not
    /// logged before 2g).
    pub(crate) mean_wall_ms: Option<u64>,
    /// Σ CPU of the land judgments that reached a verdict.
    pub(crate) completed_cpu_ms: u64,
    /// The span the log covers within the window.
    pub(crate) span_ms: u64,
}

impl Row2 {
    fn of(e3: &E3, judgments: &[Judgment], span_ms: u64) -> Row2 {
        let walls: Vec<u64> = e3.heavy.iter().map(Judgment::wall_ms).collect();
        Row2 {
            mean_wall_ms: (!walls.is_empty())
                .then(|| walls.iter().sum::<u64>() / walls.len() as u64),
            completed_cpu_ms: judgments
                .iter()
                .filter(|j| j.completed())
                .fold(0, |s, j| s.saturating_add(j.cpu_ms)),
            span_ms,
        }
    }

    /// Completed judgment CPU-hours per hour of the span.
    pub(crate) fn cpu_per_hour(self) -> Option<f64> {
        (self.span_ms > 0).then(|| self.completed_cpu_ms as f64 / self.span_ms as f64)
    }

    fn json(self) -> Value {
        obj([
            ("mean_wall_ms", self.mean_wall_ms.into()),
            ("wall_from", "run start".into()),
            ("completed_cpu_ms", self.completed_cpu_ms.into()),
            ("span_ms", self.span_ms.into()),
            ("cpu_h_per_h", self.cpu_per_hour().into()),
            ("rejudges_stopped", Value::Null),
        ])
    }
}

/// What the log covers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Coverage {
    /// Rows read in the window.
    pub(crate) rows: usize,
    /// Rows lost, as gap lines say.
    pub(crate) lost: u64,
    /// Rows lost that no gap line says yet.
    pub(crate) lost_pending: u64,
    /// Lines that are no row.
    pub(crate) torn: u64,
    /// Files read.
    pub(crate) files: u32,
    /// Bytes read.
    pub(crate) bytes: u64,
    /// The held claims of memory look rows name.
    pub(crate) holders: u64,
    /// Those with a `stage` row's true peak.
    pub(crate) known: u64,
    /// Their claimed bytes (each at the most a look saw it claim).
    pub(crate) held_bytes: u64,
    /// The claimed bytes of those with no true peak: counted by their claim.
    pub(crate) by_claim_bytes: u64,
    /// The oldest line's time, whatever the window: how far back the log reaches.
    pub(crate) oldest_ms: Option<u64>,
    /// The oldest `run` row's time, whatever the window: how far back the run rows reach.
    pub(crate) oldest_run_ms: Option<u64>,
    /// The oldest `run` row's time in the window.
    pub(crate) window_run_ms: Option<u64>,
}

impl Coverage {
    fn of(reading: &Reading, peaks: &BTreeMap<String, u64>) -> Coverage {
        let mut claimed: BTreeMap<&str, u64> = BTreeMap::new();
        for look in reading.rows.iter().filter(|r| r.kind() == "look") {
            for (id, bytes, _) in held(look) {
                let most = claimed.entry(id).or_default();
                *most = (*most).max(bytes);
            }
        }
        let known: BTreeSet<&str> = claimed
            .keys()
            .copied()
            .filter(|id| peaks.contains_key(*id))
            .collect();
        Coverage {
            rows: reading.rows.len(),
            lost: reading.lost,
            lost_pending: reading.lost_pending,
            torn: reading.torn,
            files: reading.files,
            bytes: reading.bytes,
            holders: claimed.len() as u64,
            known: known.len() as u64,
            held_bytes: claimed.values().fold(0, |s, b| s.saturating_add(*b)),
            by_claim_bytes: claimed
                .iter()
                .filter(|(id, _)| !known.contains(*id))
                .fold(0, |s, (_, b)| s.saturating_add(*b)),
            oldest_ms: reading.oldest_ms,
            oldest_run_ms: reading.oldest_by_kind.get("run").copied(),
            window_run_ms: reading
                .rows
                .iter()
                .filter(|r| r.kind() == "run")
                .map(Row::at_ms)
                .min(),
        }
    }

    /// The share of held look holders whose true peak is known.
    fn known_share(&self) -> Option<f64> {
        (self.holders > 0).then(|| self.known as f64 / self.holders as f64)
    }

    /// The share of held bytes counted by their claim.
    fn by_claim_share(&self) -> Option<f64> {
        (self.held_bytes > 0).then(|| self.by_claim_bytes as f64 / self.held_bytes as f64)
    }

    fn json(&self) -> Value {
        obj([
            ("rows", (self.rows as u64).into()),
            ("lost", self.lost.into()),
            ("lost_pending", self.lost_pending.into()),
            ("torn", self.torn.into()),
            ("files", self.files.into()),
            ("bytes", self.bytes.into()),
            ("holders", self.holders.into()),
            ("known", self.known.into()),
            ("known_share", self.known_share().into()),
            ("held_bytes", self.held_bytes.into()),
            ("by_claim_bytes", self.by_claim_bytes.into()),
            ("by_claim_share", self.by_claim_share().into()),
            ("oldest_ms", self.oldest_ms.into()),
            ("oldest_run_ms", self.oldest_run_ms.into()),
            ("window_run_ms", self.window_run_ms.into()),
        ])
    }
}

/// Every figure the reader prints.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Report {
    /// The machine's cores, which H is read by.
    pub(crate) frame_cores: u64,
    /// E1.
    pub(crate) e1: E1,
    /// E2.
    pub(crate) e2: E2,
    /// E3, by CPU after the fact.
    pub(crate) e3: E3,
    /// E4.
    pub(crate) e4: E4,
    /// §4 row 2.
    pub(crate) row2: Row2,
    /// What the log covers.
    pub(crate) coverage: Coverage,
}

/// Judge `reading` in `frame`.
pub(crate) fn judge(reading: &Reading, frame: Frame) -> Report {
    let rows = &reading.rows;
    let mut peaks: BTreeMap<String, u64> = BTreeMap::new();
    for row in rows.iter().filter(|r| r.kind() == "stage") {
        if let (Some(id), Some(peak)) = (row.str("claim"), row.u64("peak")) {
            let most = peaks.entry(id.to_owned()).or_default();
            *most = (*most).max(peak);
        }
    }
    let judgments: Vec<Judgment> = rows.iter().filter_map(Judgment::of).collect();
    let e3 = E3::of(&judgments, frame.cores);
    let from_ms = reading
        .oldest_ms
        .map_or(frame.since_ms, |o| o.max(frame.since_ms));
    let row2 = Row2::of(&e3, &judgments, frame.now_ms.saturating_sub(from_ms));
    Report {
        frame_cores: frame.cores,
        e1: E1::of(rows),
        e2: E2::of(rows, &peaks),
        e3,
        e4: E4::of(rows),
        row2,
        coverage: Coverage::of(reading, &peaks),
    }
}

impl Report {
    fn json(&self, dir: &Path) -> Value {
        obj([
            ("log", dir.display().to_string().into()),
            ("cores", self.frame_cores.into()),
            ("e1", self.e1.json()),
            ("e2", self.e2.json()),
            ("e3", self.e3.json()),
            ("e4", self.e4.json()),
            ("row2", self.row2.json()),
            ("coverage", self.coverage.json()),
        ])
    }

    fn text(&self, dir: &Path) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        let c = &self.coverage;
        let _ = writeln!(
            out,
            "admission log {}: {} rows in {} files ({} bytes)",
            dir.display(),
            c.rows,
            c.files,
            c.bytes
        );
        let at = |t: Option<u64>| t.map_or("none".to_owned(), |t| t.to_string());
        let _ = writeln!(
            out,
            "coverage: lost {} (+{} not yet said), torn {}; held claims of memory with a known \
             true peak {}/{} ({}), held bytes counted by claim {}; the log reaches back to at_ms \
             {} (its run rows to {}; in the window, {})",
            c.lost,
            c.lost_pending,
            c.torn,
            c.known,
            c.holders,
            share(c.known_share()),
            share(c.by_claim_share()),
            at(c.oldest_ms),
            at(c.oldest_run_ms),
            at(c.window_run_ms),
        );
        let e1 = &self.e1;
        let _ = writeln!(
            out,
            "E1 claim/peak at matching width: {} (≤ {GATE}: {}); under-claims {} per 100 (gate: \
             ≤ B0's)",
            e1.matched.text(),
            e1.verdict.word(),
            e1.under_per_100()
                .map_or("n/a".to_owned(), |u| format!("{u:.1}")),
        );
        let _ = writeln!(
            out,
            "  every claim {}; light {}; p90 {}",
            e1.all.text(),
            share(e1.light_share()),
            ratio(e1.p90),
        );
        for ((basis, stage), sum) in &e1.table {
            let _ = writeln!(out, "  {basis:<16} {stage:<20} {}", sum.text());
        }
        for (class, b) in &e1.binaries {
            let _ = writeln!(
                out,
                "  test binaries {class}: {}; above {GATE} {}, under {} by {}",
                b.sum.text(),
                b.over,
                b.under,
                gib(b.under_bytes),
            );
        }
        let _ = writeln!(
            out,
            "  G-bin (mismatched above {GATE} over ≥ {G_BIN_LEAST}): {}",
            e1.g_bin.word()
        );
        let e2 = &self.e2;
        let _ = writeln!(
            out,
            "E2 memory-class waits of a minute or more: {}; avoidable: head {}, holders {}, ahead \
             {}, own {}, other {} (one avoidable fails; none passes over ≥ {E2_LEAST}: {})",
            e2.waits.len(),
            e2.avoidable(Cause::Head),
            e2.avoidable(Cause::Holders),
            e2.avoidable(Cause::Ahead),
            e2.avoidable(Cause::Own),
            e2.avoidable(Cause::Other),
            e2.verdict.word(),
        );
        for w in &e2.waits {
            let _ = writeln!(
                out,
                "  {} {} waited {} s{}: room* {} for a claim of {} (its true peak claims {}){}",
                w.id,
                w.what,
                w.end_ms.saturating_sub(w.first_ms) / 1_000,
                if w.ended { "" } else { " (not admitted)" },
                w.room.map_or("unread".to_owned(), gib),
                gib(w.claim),
                gib(w.needs),
                w.cause
                    .map_or(String::new(), |c| format!(": avoidable, {}", c.word())),
            );
        }
        let e3 = &self.e3;
        let _ = writeln!(
            out,
            "E3 heavy land judgments (CPU after the fact ≥ {} cores × 300 s): {} of {}; \
             overlapping pairs {}, {} s with two or more, at most {} at once (in sequence over ≥ \
             {E3_LEAST}: {})",
            self.frame_cores,
            e3.heavy.len(),
            e3.land,
            e3.pairs,
            e3.overlap_ms / 1_000,
            e3.most,
            e3.verdict.word(),
        );
        let e4 = &self.e4;
        let _ = writeln!(
            out,
            "E4 starved share: test binaries {} over {} runs, compiles {} over {} stages, all {}",
            share(e4.tests.share()),
            e4.tests.n,
            share(e4.compiles.share()),
            e4.compiles.n,
            share(e4.all().share()),
        );
        let r = &self.row2;
        let _ = writeln!(
            out,
            "§4 row 2: heavy land mean wall {} (from the run's start; a landing's since is \
             logged from 2g); completed judgment CPU {} h per hour over {:.1} h; REJUDGES-stopped \
             not logged before 2g",
            r.mean_wall_ms
                .map_or("n/a".to_owned(), |ms| format!("{} s", ms / 1_000)),
            r.cpu_per_hour()
                .map_or("n/a".to_owned(), |h| format!("{h:.2}")),
            r.span_ms as f64 / HOUR_MS as f64,
        );
        out
    }
}

/// An object of `fields`.
fn obj<const N: usize>(fields: [(&str, Value); N]) -> Value {
    Value::Object(fields.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
}

fn ratio(r: Option<f64>) -> String {
    r.map_or("n/a".to_owned(), |r| format!("{r:.2}"))
}

fn share(s: Option<f64>) -> String {
    s.map_or("n/a".to_owned(), |s| format!("{:.1}%", s * 100.0))
}

fn gib(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / GIB as f64)
}

#[cfg(test)]
#[path = "admission_log_tests.rs"]
mod tests;
