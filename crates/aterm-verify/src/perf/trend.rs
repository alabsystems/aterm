// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The SAME-BOX TREND LEDGER and the machine identity it is keyed by.

use std::process::Command;

use super::LaneRun;
use super::floors::golden_dir;

/// Same-box floor ratio.
pub(crate) const TREND_RATIO: f64 = 0.70;

/// How many most-recent same-box entries per metric form the reference (their MAX is the floor's base).
pub(crate) const TREND_WINDOW: usize = 10;

/// How far apart one figure of two operating points may be and still be one machine
/// state: the larger at most this many times the smaller. On an M5 Max the effects lane's
/// reference full repaint measured within 0.2% of 535.8 us (blank 367.5 us) across six
/// runs in one GPU state, 175.9 us (blank 117.6 us) and 195.3 us (blank 80.4 us) in two
/// others, and its gated GPU ratios moved by up to 1.6x between states while the code did
/// not change.
pub(crate) const OPERATING_POINT_BAND: f64 = 1.25;

/// One figure of the machine state a sample was measured in, when its number moves with a
/// state the run does not set: a sample that names an operating point (one or more
/// figures) is judged only against rows measured at the same point ([`same_point`]), so a
/// state change is not read as a regression.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PointFigure {
    /// The harness report key that measured it (`ref_gpu_p50_us`); a ledger row carries it
    /// as a `<name>=<value>` column.
    pub name: &'static str,
    pub value: f64,
}

/// One measured median headed for the ledger.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TrendSample {
    pub lane: &'static str,
    pub metric: String,
    pub value: f64,
    /// LOWER is better (worst-latency metrics, e.g. the resize `*_worst_ms` fences): the same-box bound is best(MIN)/TREND_RATIO — a ceiling — instead of best(MAX)*TREND_RATIO — a floor.
    pub inverted: bool,
    /// The figures of the operating point it was measured at; none when its number does not
    /// depend on one.
    pub at: Vec<PointFigure>,
}

pub(crate) fn trend_path() -> std::path::PathBuf {
    golden_dir().join("perf-trend.tsv")
}

/// The committed alias table: which raw machine identities are which box.
pub(crate) fn boxes_path() -> std::path::PathBuf {
    golden_dir().join("perf-boxes.tsv")
}

/// Who this machine is, as the ledger keys it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MachineId {
    /// The ledger's box column: a short name from the alias table, or a deterministic `box-<digest>` when nothing claims this machine.
    pub key: String,
    /// Chip + logical cores (`Apple-M5-Max-18c`).
    pub shape: String,
    /// The raw tokens this machine answers to, strongest first: `fp:<digest>` when a platform UUID could be read, then `host:<hostname>`.
    pub identities: Vec<String>,
    /// An alias row explicitly claimed one of [`Self::identities`].
    pub claimed: bool,
}

/// Alphanumerics kept, runs of anything else collapsed to one `-`, so a chip
/// string is safe in a TSV column and readable in a diff.
fn slug(s: &str) -> String {
    let mut out = String::new();
    let mut pending_sep = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            if pending_sep && !out.is_empty() {
                out.push('-');
            }
            pending_sep = false;
            out.push(c);
        } else {
            pending_sep = true;
        }
    }
    out
}

/// The machine's platform UUID, or `None` where none can be read.
fn platform_uuid() -> Option<String> {
    if cfg!(target_os = "macos") {
        let out = Command::new(aterm_uds::os_path!("ioreg"))
            .args(["-rd1", "-c", "IOPlatformExpertDevice"])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        let key = "\"IOPlatformUUID\"";
        let at = text.find(key)? + key.len();
        let tail = &text[at..];
        let open = tail.find('"')? + 1;
        let rest = &tail[open..];
        let close = rest.find('"')?;
        return Some(rest[..close].to_string());
    }
    if cfg!(target_os = "windows") {
        let out = Command::new("reg")
            .args([
                "query",
                r"HKLM\SOFTWARE\Microsoft\Cryptography",
                "/v",
                "MachineGuid",
            ])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        return text
            .lines()
            .find(|l| l.contains("MachineGuid"))
            .and_then(|l| l.split_whitespace().last())
            .map(str::to_string);
    }
    for p in ["/etc/machine-id", "/var/lib/dbus/machine-id"] {
        if let Ok(s) = std::fs::read_to_string(p) {
            let t = s.trim().to_string();
            if !t.is_empty() {
                return Some(t);
            }
        }
    }
    None
}

/// The stable, non-identifying machine fingerprint — `None` when no platform
/// UUID is readable, in which case the hostname is all we have.
fn platform_fingerprint() -> Option<String> {
    let raw = platform_uuid()?;
    let t = raw.trim();
    (!t.is_empty()).then(|| format!("{:016x}", aterm_hash::fnv1a_64(t.as_bytes())))
}

fn hostname() -> String {
    Command::new(aterm_uds::os_path!("hostname"))
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown-host".to_string())
}

/// Chip + logical cores, e.g. `Apple-M5-Max-18c`. Recorded per row so a wrong
/// alias claim is DETECTABLE; see [`chip_of`] for what is actually compared.
fn machine_shape() -> String {
    let chip = if cfg!(target_os = "macos") {
        Command::new(aterm_uds::os_path!("sysctl"))
            .args(["-n", "machdep.cpu.brand_string"])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
    } else {
        std::fs::read_to_string("/proc/cpuinfo").ok().and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("model name"))
                .and_then(|l| l.split_once(':'))
                .map(|(_, v)| v.trim().to_string())
        })
    };
    let chip = chip.unwrap_or_else(|| std::env::consts::ARCH.to_string());
    let cores = std::thread::available_parallelism().map_or(0, std::num::NonZeroUsize::get);
    format!("{}-{cores}c", slug(&chip))
}

/// The CHIP half of a shape — the shape with a trailing `-<n>c` core count removed.
pub(crate) fn chip_of(shape: &str) -> &str {
    let Some(stripped) = shape.strip_suffix('c') else {
        return shape;
    };
    match stripped.rfind('-') {
        Some(dash)
            if dash + 1 < stripped.len()
                && stripped[dash + 1..].chars().all(|c| c.is_ascii_digit()) =>
        {
            &shape[..dash]
        }
        _ => shape,
    }
}

/// The box an alias table gives `identity` (`fp:<digest>` / `host:<name>`), or `None` when no row claims it.
pub(crate) fn alias_lookup(aliases: &str, identity: &str) -> Option<String> {
    aliases
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .find_map(|l| {
            let mut cols = l.split('\t');
            let key = cols.next()?.trim();
            let ident = cols.next()?.trim();
            (ident == identity && !key.is_empty()).then(|| key.to_string())
        })
}

/// Every box name the alias table declares.
pub(crate) fn alias_boxes(aliases: &str) -> Vec<String> {
    let mut v: Vec<String> = aliases
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .filter_map(|l| {
            let key = l.split('\t').next()?.trim();
            (!key.is_empty()).then(|| key.to_string())
        })
        .collect();
    v.sort();
    v.dedup();
    v
}

/// What a ledger row's box column means, resolved through the alias table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BoxRef {
    /// An alias row claims this token (as `host:<token>`/`fp:<token>`) or declares it as a box name outright.
    Claimed(String),
    /// Nothing in the alias table mentions it.
    Unclaimed(String),
}

impl BoxRef {
    pub(crate) fn key(&self) -> &str {
        match self {
            BoxRef::Claimed(k) | BoxRef::Unclaimed(k) => k,
        }
    }
    pub(crate) const fn is_claimed(&self) -> bool {
        matches!(self, BoxRef::Claimed(_))
    }
}

/// Resolve one ledger row's box column.
pub(crate) fn resolve_ledger_box(aliases: &str, token: &str) -> BoxRef {
    if let Some(key) = alias_lookup(aliases, &format!("host:{token}")) {
        return BoxRef::Claimed(key);
    }
    if let Some(key) = alias_lookup(aliases, &format!("fp:{token}")) {
        return BoxRef::Claimed(key);
    }
    if let Some(digest) = token.strip_prefix("box-")
        && let Some(key) = alias_lookup(aliases, &format!("fp:{digest}"))
    {
        return BoxRef::Claimed(key);
    }
    if alias_boxes(aliases).iter().any(|b| b == token) {
        return BoxRef::Claimed(token.to_string());
    }
    BoxRef::Unclaimed(token.to_string())
}

/// The key an UNCLAIMED machine gets.
pub(crate) fn default_box_key(identities: &[String]) -> String {
    for i in identities {
        if let Some(fp) = i.strip_prefix("fp:") {
            return format!("box-{fp}");
        }
    }
    for i in identities {
        if let Some(h) = i.strip_prefix("host:") {
            return format!("host-{}", slug(h));
        }
    }
    "unknown-box".to_string()
}

/// This machine, resolved against the committed alias table.
pub(crate) fn machine_id() -> MachineId {
    let aliases = std::fs::read_to_string(boxes_path()).unwrap_or_default();
    let mut identities = Vec::new();
    if let Some(fp) = platform_fingerprint() {
        identities.push(format!("fp:{fp}"));
    }
    identities.push(format!("host:{}", hostname()));
    let claimed = identities.iter().find_map(|i| alias_lookup(&aliases, i));
    let key = claimed
        .clone()
        .unwrap_or_else(|| default_box_key(&identities));
    MachineId {
        key,
        shape: machine_shape(),
        identities,
        claimed: claimed.is_some(),
    }
}

/// The tree a run measured, for its rows' sha column: see [`tree_label`].
fn head_sha() -> String {
    let git = |args: &[&str]| {
        aterm_uds::spawnfd::command(aterm_uds::os_path!("git"))
            .args(args)
            .current_dir(super::workspace_root())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
    };
    let Some(sha) = git(&["rev-parse", "--short=12", "HEAD"]) else {
        return "unknown".to_string();
    };
    let status = git(&["status", "--porcelain", "--untracked-files=no"]);
    tree_label(sha.trim(), status.as_deref())
}

/// The perf golden dir, relative to the workspace root: a `--record` rewrites its files
/// before the trend rows are written, so its changes do not make the measured tree dirty.
const GOLDEN_REL: &str = "crates/aterm-verify/perf/golden/";

/// HEAD's short `sha`, with `+dirty` when `git status --porcelain` names a tracked file outside [`GOLDEN_REL`] that differs from HEAD (or could not be read), so a row measured on uncommitted code does not pass as HEAD's.
pub(crate) fn tree_label(sha: &str, porcelain: Option<&str>) -> String {
    let dirty = porcelain.is_none_or(|status| {
        status.lines().filter(|l| !l.trim().is_empty()).any(|line| {
            line.get(3..)
                .unwrap_or(line)
                .split(" -> ")
                .any(|path| !path.trim_matches('"').starts_with(GOLDEN_REL))
        })
    });
    if dirty {
        format!("{sha}+dirty")
    } else {
        sha.to_string()
    }
}

fn now_unix_s() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Today's UTC date, `YYYY-MM-DD`.
pub(crate) fn utc_date() -> String {
    civil_date(now_unix_s() / 86_400)
}

/// Now, as an RFC 3339 UTC timestamp.
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn utc_timestamp() -> String {
    rfc3339(now_unix_s())
}

/// `secs` past the Unix epoch as an RFC 3339 UTC timestamp, `YYYY-MM-DDTHH:MM:SSZ`.
pub(crate) fn rfc3339(secs: u64) -> String {
    let t = secs % 86_400;
    format!(
        "{}T{:02}:{:02}:{:02}Z",
        civil_date(secs / 86_400),
        t / 3_600,
        t % 3_600 / 60,
        t % 60
    )
}

/// Days past the Unix epoch as a UTC date, without a chrono dependency:
/// civil-from-days (Howard Hinnant's algorithm, exact for the Gregorian
/// calendar).
fn civil_date(days: u64) -> String {
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// The eighth column of a row a re-record wrote ([`trend_rows`]).
const RERECORDED: &str = "rerecorded";

/// One parsed ledger row.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TrendRow<'a> {
    pub token: &'a str,
    pub metric: &'a str,
    pub value: f64,
    /// What measured it ([`toolchain_lane`]); `""` on every row written
    /// before the toolchain column existed.
    pub toolchain: &'a str,
    /// `""` on every row written before the shape column existed.
    pub shape: &'a str,
    /// A run that re-recorded this row's lane wrote it: the lane's new level is declared, so this box's reference window for the metric starts here ([`judge_trend`]).
    pub rerecorded: bool,
    /// The figures of the operating point it was measured at (`<name>=<value>` columns after
    /// the shape); none on a row that names none, as every row written before them.
    pub at: Vec<(&'a str, f64)>,
}

/// A `<name>=<value>` ledger column with a finite positive value.
fn parse_point(col: &str) -> Option<(&str, f64)> {
    let (name, value) = col.trim().split_once('=')?;
    let value = value
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && *v > 0.0)?;
    (!name.is_empty()).then_some((name, value))
}

/// Whether a row measured at `row` may judge a sample measured at `sample`: any row when
/// the sample names no point; otherwise only a row that names every figure the sample does,
/// each within [`OPERATING_POINT_BAND`] of the sample's, so a row that names none cannot
/// show it was measured in this state.
pub(crate) fn same_point(sample: &[PointFigure], row: &[(&str, f64)]) -> bool {
    sample.iter().all(|f| {
        row.iter().any(|&(name, v)| {
            name == f.name && v.max(f.value) <= OPERATING_POINT_BAND * v.min(f.value)
        })
    })
}

/// Parse the ledger into rows AND COUNT what it had to throw away.
pub(crate) fn trend_ledger_rows_counted(ledger: &str) -> (Vec<TrendRow<'_>>, Vec<String>) {
    let mut rows = Vec::new();
    let mut dropped = Vec::new();
    for (n, line) in ledger.lines().enumerate() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() < 5 {
            dropped.push(format!(
                "line {}: {} tab-separated column(s), needs at least 5",
                n + 1,
                cols.len()
            ));
            continue;
        }
        let Ok(value) = cols[4].parse::<f64>() else {
            dropped.push(format!(
                "line {}: value column `{}` does not parse as a number",
                n + 1,
                cols[4]
            ));
            continue;
        };
        let tags = cols.get(7..).unwrap_or_default();
        rows.push(TrendRow {
            token: cols[2],
            metric: cols[3],
            value,
            toolchain: cols.get(5).copied().unwrap_or("").trim(),
            shape: cols.get(6).copied().unwrap_or("").trim(),
            rerecorded: tags.iter().any(|c| c.trim() == RERECORDED),
            at: tags.iter().filter_map(|c| parse_point(c)).collect(),
        });
    }
    (rows, dropped)
}

/// Parse the ledger, skipping comments, blanks and unparseable rows.
pub(crate) fn trend_ledger_rows(ledger: &str) -> Vec<TrendRow<'_>> {
    trend_ledger_rows_counted(ledger).0
}

/// One metric outside its same-box bound.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Breach {
    /// The trend lane that measured it (the ledger's `lane/` prefix).
    pub lane: &'static str,
    /// `lane/metric`, as the ledger keys it.
    pub metric: String,
    pub value: f64,
    /// A floor for bigger-is-better metrics, a CEILING for inverted (lower-is-better) ones.
    pub bound: f64,
    /// The best of the reference window the bound derives from.
    pub best: f64,
    pub inverted: bool,
}

/// The ledger tokens no alias row claims, each with the claimed box measured on the same chip, if one was: a machine that renamed itself and started a second, disconnected history has exactly that signature.
pub(crate) fn orphans<'a>(rows: &[TrendRow<'a>], aliases: &str) -> Vec<(&'a str, Option<String>)> {
    let claimed: Vec<(String, &str)> = rows
        .iter()
        .filter_map(|r| match resolve_ledger_box(aliases, r.token) {
            BoxRef::Claimed(key) if !r.shape.is_empty() => Some((key, chip_of(r.shape))),
            _ => None,
        })
        .collect();
    let mut found: Vec<(&'a str, Option<String>)> = Vec::new();
    for r in rows {
        if resolve_ledger_box(aliases, r.token).is_claimed()
            || found.iter().any(|(t, _)| *t == r.token)
        {
            continue;
        }
        let chip = chip_of(r.shape);
        let same = claimed
            .iter()
            .find(|(_, c)| !chip.is_empty() && *c == chip)
            .map(|(k, _)| k.clone());
        found.push((r.token, same));
    }
    found.sort();
    found
}

/// A metric with same-box history none of which was measured at this run's operating point.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Elsewhere {
    pub metric: String,
    pub at: Vec<PointFigure>,
    /// The same-box rows its window could have drawn on.
    pub rows: usize,
}

/// The trend sub-gate's pure verdict over one run's samples.
pub(crate) struct TrendJudgment {
    pub breaches: Vec<Breach>,
    /// Metrics with NO same-box history: first run on this box.
    pub skipped: Vec<String>,
    /// Metrics whose same-box history was all measured at another operating point, or names
    /// none: a named skip, never a pass.
    pub elsewhere: Vec<Elsewhere>,
    /// `(token, chip)` pairs that resolve to THIS box but were measured on a different CHIP.
    pub conflicts: Vec<(String, String)>,
}

/// Judge `samples` against the same-box history in the ledger text, with `aliases` deciding which rows are THIS box.
pub(crate) fn judge_trend(
    ledger: &str,
    aliases: &str,
    me: &MachineId,
    samples: &[TrendSample],
) -> TrendJudgment {
    let (rows, dropped) = trend_ledger_rows_counted(ledger);
    if !dropped.is_empty() {
        eprintln!(
            "  trend: NOTE — {} committed ledger line(s) were DISCARDED as unparseable and \
             anchor nothing; every metric they carried is held to a SMALLER reference \
             window than the file appears to give it.",
            dropped.len()
        );
        for why in &dropped {
            eprintln!("         dropped {why}");
        }
    }
    let my_chip = chip_of(&me.shape);

    let mut conflicts: Vec<(String, String)> = Vec::new();
    let mine: Vec<&TrendRow<'_>> = rows
        .iter()
        .filter(|r| resolve_ledger_box(aliases, r.token).key() == me.key)
        .filter(|r| {
            if r.shape.is_empty() || chip_of(r.shape) == my_chip {
                return true;
            }
            let entry = (r.token.to_string(), chip_of(r.shape).to_string());
            if !conflicts.contains(&entry) {
                conflicts.push(entry);
            }
            false
        })
        .collect();

    let mut breaches = Vec::new();
    let mut skipped = Vec::new();
    let mut elsewhere = Vec::new();
    for s in samples {
        let metric_key = format!("{}/{}", s.lane, s.metric);
        let rows: Vec<&TrendRow<'_>> = mine
            .iter()
            .copied()
            .filter(|r| r.metric == metric_key && r.value > 0.0)
            .collect();
        // A metric's window starts at its lane's last re-record, or after the
        // last row another toolchain or engine measured, whichever is later:
        // neither declares a level this run's engine is held to. A row from
        // before the column says nothing either way.
        let toolchain = toolchain_lane(s.lane);
        let recorded = rows.iter().rposition(|r| r.rerecorded).unwrap_or(0);
        let changed = rows
            .iter()
            .rposition(|r| !r.toolchain.is_empty() && r.toolchain != toolchain)
            .map_or(0, |i| i + 1);
        // Within that span, only the rows measured at this run's operating point.
        let span = &rows[recorded.max(changed)..];
        let mut history: Vec<f64> = span
            .iter()
            .filter(|r| same_point(&s.at, &r.at))
            .map(|r| r.value)
            .collect();
        let recent = history.split_off(history.len().saturating_sub(TREND_WINDOW));
        if recent.is_empty() {
            if s.at.is_empty() || span.is_empty() {
                skipped.push(metric_key);
            } else {
                elsewhere.push(Elsewhere {
                    metric: metric_key,
                    at: s.at.clone(),
                    rows: span.len(),
                });
            }
            continue;
        }
        let (best, bound, breached) = if s.inverted {
            let best = recent.iter().copied().fold(f64::INFINITY, f64::min);
            (best, best / TREND_RATIO, s.value > best / TREND_RATIO)
        } else {
            let best = recent.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            (best, best * TREND_RATIO, s.value < best * TREND_RATIO)
        };
        if breached {
            breaches.push(Breach {
                lane: s.lane,
                metric: metric_key,
                value: s.value,
                bound,
                best,
                inverted: s.inverted,
            });
        }
    }
    TrendJudgment {
        breaches,
        skipped,
        elsewhere,
        conflicts,
    }
}

/// Which toolchain builds a lane's measurement binary, and for the wasm lane
/// the engine that runs it: recorded per ledger row so cross-toolchain drift
/// (a Trust-toolchain bump vs an upstream-stable bump) is never misread as a
/// same-box perf change, and a metric's window starts after the last row
/// another one measured ([`judge_trend`]). The wasm lane's rows before S10
/// say `stable`: node measured them, and the pinned browser is not node.
fn toolchain_lane(lane: &str) -> &'static str {
    if lane == "wasm" {
        "stable+chrome-headless-shell"
    } else {
        "trust"
    }
}

/// Render `samples` as ledger rows (one metric per line, TSV).
pub(crate) fn trend_rows(
    date: &str,
    sha: &str,
    me: &MachineId,
    samples: &[TrendSample],
    recorded: &[&str],
) -> String {
    let mut s = String::new();
    for sample in samples {
        s.push_str(&format!(
            "{date}\t{sha}\t{}\t{}/{}\t{:.3}\t{}\t{}{}{}\n",
            me.key,
            sample.lane,
            sample.metric,
            sample.value,
            toolchain_lane(sample.lane),
            me.shape,
            if recorded.contains(&sample.lane) {
                format!("\t{RERECORDED}")
            } else {
                String::new()
            },
            sample
                .at
                .iter()
                .map(|f| format!("\t{}={:.3}", f.name, f.value))
                .collect::<String>()
        ));
    }
    s
}

const TREND_HEADER: &str = "# aterm same-box perf trend ledger (E0, audit 5.6). Appended by every \
GREEN `verify perf` run;\n# each metric must clear TREND_RATIO x the best of this box's last \
TREND_WINDOW entries\n# (crates/aterm-verify/src/perf/trend.rs; *_worst_ms metrics are INVERTED — bounded \
by best-MIN / TREND_RATIO).\n# The BOX column is a name from crates/aterm-verify/perf/golden/perf-boxes.tsv, \
which is also where a\n# pre-W-1 hostname is claimed as the box it was measured on. NEVER edit a \
box name here\n# to merge two machines — add the claim to perf-boxes.tsv, where the chip column \
keeps it\n# honest. A row a re-record (`--record LANE`) wrote carries `rerecorded` in an eighth column:\n# that lane's new level is declared, and each of its metrics' windows starts there. A row \
measured\n# at an operating point carries its figures last (the effects GPU keys: \
`ref_gpu_p50_us=<us>`,\n# `ref_blank_gpu_p50_us=<us>`); a sample that names one is judged only \
against rows whose\n# figures are each within OPERATING_POINT_BAND of its own.\n# \
date\tsha\tbox\tlane/metric\tvalue\ttoolchain\tshape\trerecorded\tpoint...\n";

/// Whether this run's samples join the ledger, which holds only good states: every floor lane held, no alias claim merges two machines, and any trend breach is on a lane whose baseline this run re-recorded (`recorded` names those trend lanes), because a re-record declares the new level deliberate.
pub(crate) fn may_append(lanes_ok: bool, judgment: &TrendJudgment, recorded: &[&str]) -> bool {
    lanes_ok
        && judgment.conflicts.is_empty()
        && judgment.breaches.iter().all(|b| recorded.contains(&b.lane))
}

/// The trend sub-gate: compare this run's samples against the same-box history, and append them when [`may_append`] allows.
pub(crate) fn gate_trend(samples: &[TrendSample], lanes_ok: bool, recorded: &[&str]) -> LaneRun {
    let path = trend_path();
    let me = machine_id();
    let aliases = std::fs::read_to_string(boxes_path()).unwrap_or_default();
    let ledger = std::fs::read_to_string(&path).unwrap_or_default();
    let judgment = judge_trend(&ledger, &aliases, &me, samples);
    let append = may_append(lanes_ok, &judgment, recorded);
    let TrendJudgment {
        breaches,
        skipped,
        elsewhere,
        conflicts,
    } = judgment;
    let trend_ok = breaches.is_empty() && conflicts.is_empty();
    let bx = &me.key;

    if !me.claimed {
        eprintln!(
            "  trend: NOTE — this machine is not named in {}; its rows are keyed `{bx}`.\n         \
             Add:  {bx}\t{}\t<what this box is>",
            boxes_path().display(),
            me.identities.first().map_or("host:?", String::as_str)
        );
    }

    for (token, same_chip) in orphans(&trend_ledger_rows(&ledger), &aliases) {
        match same_chip {
            Some(claimed) => eprintln!(
                "  trend: NOTE — ledger rows keyed `{token}` are claimed by no row of {} and \
                 were measured on the chip of box `{claimed}`: a renamed machine looks like \
                 this. Claim `{token}` for `{claimed}` there, or note why it is another machine.",
                boxes_path().display()
            ),
            None => eprintln!(
                "  trend: NOTE — ledger rows keyed `{token}` are claimed by no row of {}; \
                 they anchor nothing until some box declares them.",
                boxes_path().display()
            ),
        }
    }

    for (token, chip) in &conflicts {
        eprintln!(
            "  trend: FAILED — ledger rows keyed `{token}` resolve to box `{bx}` but were \
             measured on chip `{chip}`, not `{}`. {} claims two different machines are one \
             box; those rows were DROPPED from the reference window rather than compared \
             against. Fix the claim — do not widen it.",
            chip_of(&me.shape),
            boxes_path().display()
        );
    }

    for metric in &skipped {
        eprintln!(
            "  trend: SKIP — {metric}: no same-box history on {bx} (first run; this \
             run only SEEDS the reference window, it verifies nothing)."
        );
    }
    let mut run = LaneRun::pass();
    for (token, chip) in &conflicts {
        run.add_failure(format!(
            "rows keyed `{token}` resolve to box `{bx}` but were measured on chip `{chip}`"
        ));
    }
    for e in &elsewhere {
        let point: Vec<String> =
            e.at.iter()
                .map(|f| format!("{} {:.1}", f.name, f.value))
                .collect();
        eprintln!(
            "  trend: SKIP — {}: none of its {} same-box row(s) on {bx} was measured at this \
             run's operating point ({}, each within {OPERATING_POINT_BAND}x); a row from \
             another machine state, or one that names none, is not compared, so nothing was \
             judged.",
            e.metric,
            e.rows,
            point.join(", ")
        );
    }
    let judged = samples.len() - skipped.len() - elsewhere.len();
    if trend_ok && judged == 0 {
        eprintln!(
            "  trend: SKIP — no metric had comparable same-box history on {bx}; nothing was \
             judged (seed run, not a pass). If this machine measured before \
             under another name, claim that name for `{bx}` in {}.",
            boxes_path().display()
        );
        run = LaneRun::not_judged("seed run, no comparable same-box history");
    } else if trend_ok {
        eprintln!(
            "  trend: GREEN — {judged} metric(s) within same-box trend bounds of this \
             box's ({bx}) recent best ({} skipped: {} with no history, {} with none at this \
             run's operating point).",
            skipped.len() + elsewhere.len(),
            skipped.len(),
            elsewhere.len()
        );
        if !skipped.is_empty() {
            run.unjudged.push(format!(
                "trend ({} metric(s) with no history)",
                skipped.len()
            ));
        }
        if !elsewhere.is_empty() {
            run.unjudged.push(format!(
                "trend ({} metric(s) with no row at this run's operating point)",
                elsewhere.len()
            ));
        }
    }
    for b in &breaches {
        let Breach {
            metric,
            value,
            bound,
            best,
            ..
        } = b;
        let finding = if b.inverted {
            format!(
                "{metric} {value:.1} > same-box ceiling {bound:.1} (best-of-last-{TREND_WINDOW} \
                 {best:.1} / {TREND_RATIO:.2} on {bx}; lower is better)"
            )
        } else {
            format!(
                "{metric} {value:.1} < same-box floor {bound:.1} (best-of-last-{TREND_WINDOW} \
                 {best:.1} x {TREND_RATIO:.2} on {bx})"
            )
        };
        eprintln!("  trend: FAILED — {finding}");
        run.add_failure(finding);
    }
    if append {
        let mut text = if ledger.is_empty() {
            TREND_HEADER.to_string()
        } else {
            ledger
        };
        text.push_str(&trend_rows(
            &utc_date(),
            &head_sha(),
            &me,
            samples,
            recorded,
        ));
        if let Err(e) = std::fs::write(&path, &text) {
            eprintln!("  trend: could not append ledger {}: {e}", path.display());
        } else {
            eprintln!(
                "  trend: appended {} row(s) -> {}",
                samples.len(),
                path.display()
            );
        }
    } else {
        eprintln!("  trend: not appended — only a healthy run joins the ledger.");
    }
    run
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic box for the pure-judgment tests: a name, and one fixed chip
    /// so a row this helper writes and a row it judges agree.
    fn box_id(key: &str) -> MachineId {
        MachineId {
            key: key.to_string(),
            shape: "Test-Chip-8c".to_string(),
            identities: vec![format!("host:{key}")],
            claimed: true,
        }
    }

    /// [`judge_trend`] with no alias table, where the ledger's box column IS
    /// the box.
    fn judged(ledger: &str, key: &str, samples: &[TrendSample]) -> TrendJudgment {
        judge_trend(ledger, "", &box_id(key), samples)
    }

    /// [`trend_rows`] for a synthetic box.
    fn rows_for(date: &str, sha: &str, key: &str, samples: &[TrendSample]) -> String {
        trend_rows(date, sha, &box_id(key), samples, &[])
    }

    fn sample(lane: &'static str, metric: &str, value: f64) -> TrendSample {
        TrendSample {
            lane,
            metric: metric.to_string(),
            value,
            inverted: false,
            at: Vec::new(),
        }
    }

    /// The effects lane's GPU operating point: the reference full repaint's GPU p50 and the
    /// blank one's, in us.
    fn gpu_point(ref_us: f64, blank_us: f64) -> Vec<PointFigure> {
        vec![
            PointFigure {
                name: "ref_gpu_p50_us",
                value: ref_us,
            },
            PointFigure {
                name: "ref_blank_gpu_p50_us",
                value: blank_us,
            },
        ]
    }

    /// A sample measured at [`gpu_point`]`(ref_us, blank_us)`.
    fn gpu_sample(metric: &str, value: f64, ref_us: f64, blank_us: f64) -> TrendSample {
        TrendSample {
            at: gpu_point(ref_us, blank_us),
            ..sample("effects", metric, value)
        }
    }

    fn inverted_sample(lane: &'static str, metric: &str, value: f64) -> TrendSample {
        TrendSample {
            inverted: true,
            ..sample(lane, metric, value)
        }
    }

    #[test]
    fn only_a_healthy_run_joins_the_ledger_and_a_re_record_excuses_only_its_lane() {
        let breach = |lane: &'static str| Breach {
            lane,
            metric: format!("{lane}/m"),
            value: 1.0,
            bound: 2.0,
            best: 3.0,
            inverted: false,
        };
        let judgment = |breaches: Vec<Breach>, conflicts: Vec<(String, String)>| TrendJudgment {
            breaches,
            skipped: Vec::new(),
            elsewhere: Vec::new(),
            conflicts,
        };
        let clean = judgment(Vec::new(), Vec::new());
        assert!(may_append(true, &clean, &[]));
        assert!(!may_append(false, &clean, &["restore"]));
        let regressed = judgment(vec![breach("throughput")], Vec::new());
        assert!(!may_append(true, &regressed, &["restore"]));
        assert!(!may_append(true, &regressed, &[]));
        assert!(may_append(true, &regressed, &["throughput"]));
        let merged = judgment(Vec::new(), vec![("m15".into(), "Apple M4".into())]);
        assert!(!may_append(true, &merged, &["throughput"]));
    }

    /// A re-record declares its lane's new level: the rows it wrote start each
    /// of that lane's metrics' windows, so the next run is held to the new
    /// level, not to the best of the level it replaced.
    #[test]
    fn a_re_record_starts_its_lanes_window() {
        let old: String = (0..4)
            .map(|_| {
                rows_for(
                    "2026-10-19",
                    "a",
                    "boxA",
                    &[
                        sample("effects", "m", 1000.0),
                        sample("search", "q", 1000.0),
                    ],
                )
            })
            .collect();
        let new = [sample("effects", "m", 500.0), sample("search", "q", 500.0)];
        let me = box_id("boxA");
        let marked = trend_rows("2026-10-20", "b", &me, &new, &["effects"]);
        assert!(
            marked
                .lines()
                .next()
                .is_some_and(|l| l.ends_with("\trerecorded")),
            "{marked}"
        );
        let parsed = trend_ledger_rows(&marked);
        assert_eq!(
            parsed.iter().map(|r| r.rerecorded).collect::<Vec<_>>(),
            [true, false],
            "only the re-recorded lane's rows carry the mark"
        );
        let next = [sample("effects", "m", 450.0), sample("search", "q", 450.0)];
        let after = judged(&format!("{TREND_HEADER}{old}{marked}"), "boxA", &next);
        assert_eq!(
            after
                .breaches
                .iter()
                .map(|b| b.metric.as_str())
                .collect::<Vec<_>>(),
            ["search/q"],
            "450 >= 0.70 x 500 for the re-recorded lane; the other still holds 1000"
        );
        let unmarked = trend_rows("2026-10-20", "b", &me, &new, &[]);
        let control = judged(&format!("{old}{unmarked}"), "boxA", &next);
        assert_eq!(control.breaches.len(), 2, "CONTROL: 450 < 0.70 x 1000");
        assert!(!trend_ledger_rows(&old)[0].rerecorded);
    }

    /// The box whose effects trend stayed red: its one seed row named no GPU state, and its
    /// gated GPU ratios came from another one (episode 2.884 there; 1.80 with the reference
    /// at 535.7 us and the blank at 367.5 us). A sample at an operating point is held only to
    /// rows at that point, and with none it is a named skip, not a pass. NEGATIVE CONTROL:
    /// the same sample naming no point is held to the seed row and breaches.
    #[test]
    fn a_gpu_ratio_is_judged_only_against_rows_at_its_operating_point() {
        let seed =
            "2026-10-22\ta1d868260ffb\tboxA\teffects/episode_gpu_rel\t2.884\ttrust\tTest-Chip-8c\n";
        let fast = rows_for(
            "2026-10-23",
            "b",
            "boxA",
            &[gpu_sample("episode_gpu_rel", 2.842, 195.3, 80.4)],
        );
        let ledger = format!("{TREND_HEADER}{seed}{fast}");
        let now = [gpu_sample("episode_gpu_rel", 1.80, 535.7, 367.5)];
        let j = judged(&ledger, "boxA", &now);
        assert!(j.breaches.is_empty(), "{:?}", j.breaches);
        assert!(j.skipped.is_empty(), "history exists: {:?}", j.skipped);
        assert_eq!(
            j.elsewhere,
            [Elsewhere {
                metric: "effects/episode_gpu_rel".to_string(),
                at: gpu_point(535.7, 367.5),
                rows: 2,
            }]
        );
        let control = judged(
            &ledger,
            "boxA",
            &[sample("effects", "episode_gpu_rel", 1.80)],
        );
        assert_eq!(control.breaches.len(), 1, "CONTROL: 1.80 < 0.70 x 2.884");
        assert!(control.elsewhere.is_empty());
        let fresh = judged("", "boxA", &now);
        assert_eq!(
            fresh.skipped,
            ["effects/episode_gpu_rel"],
            "no history at all"
        );
        assert!(fresh.elsewhere.is_empty());
    }

    /// A true slowdown measured in the same GPU state still fails, against the best of the
    /// rows at that point alone: a row whose figures are each within the band counts, and
    /// one whose reference matches but whose blank repaint does not (175.9/117.6 us against
    /// 195.3/80.4 us, two states this box measured) is another point. NEGATIVE CONTROL: the
    /// state's own level passes.
    #[test]
    fn a_true_slowdown_at_the_same_operating_point_still_fails() {
        let mut ledger = String::new();
        for (value, ref_us, blank_us) in [
            (1.80, 535.8, 367.5),
            (1.79, 536.4, 367.3),
            (2.842, 195.3, 80.4),
            (1.81, 430.0, 300.0),
            (2.9, 535.8, 250.0),
        ] {
            ledger.push_str(&rows_for(
                "2026-10-23",
                "c",
                "boxA",
                &[gpu_sample("episode_gpu_rel", value, ref_us, blank_us)],
            ));
        }
        let slow = judged(
            &ledger,
            "boxA",
            &[gpu_sample("episode_gpu_rel", 0.9, 535.9, 367.4)],
        );
        assert_eq!(slow.breaches.len(), 1, "0.9 < 0.70 x 1.81");
        assert!(
            (slow.breaches[0].best - 1.81).abs() < 1e-9,
            "430/300 us is within 1.25x of 535.9/367.4 us; 195.3/80.4 and 535.8/250 are not: {:?}",
            slow.breaches
        );
        assert!(slow.elsewhere.is_empty());
        let level = judged(
            &ledger,
            "boxA",
            &[gpu_sample("episode_gpu_rel", 1.75, 535.9, 367.4)],
        );
        assert!(level.breaches.is_empty(), "CONTROL: {:?}", level.breaches);

        let here = gpu_point(195.3, 80.4);
        let row = |r: f64, b: f64| [("ref_gpu_p50_us", r), ("ref_blank_gpu_p50_us", b)];
        assert!(same_point(&here, &row(195.3 * OPERATING_POINT_BAND, 80.4)));
        assert!(!same_point(
            &here,
            &row(195.3 * OPERATING_POINT_BAND * 1.01, 80.4)
        ));
        assert!(
            !same_point(&here, &row(175.9, 117.6)),
            "the reference within the band, the blank repaint not"
        );
        assert!(
            !same_point(&here, &[("ref_gpu_p50_us", 195.3)]),
            "a row that lacks a figure cannot show it"
        );
        assert!(
            same_point(
                &here,
                &[
                    ("ref_gpu_p50_us", 195.3),
                    ("ref_blank_gpu_p50_us", 80.4),
                    ("other_us", 1.0)
                ]
            ),
            "a figure the sample does not name constrains nothing"
        );
        assert!(same_point(&[], &[]), "a sample with no point takes any row");
    }

    /// The figures are columns after the shape, after `rerecorded` when both are written, so
    /// a parser that reads only the eighth column for `rerecorded` still reads both rows
    /// right.
    #[test]
    fn a_rows_operating_point_round_trips_beside_its_re_record_mark() {
        let me = box_id("boxA");
        let at = [gpu_sample("episode_gpu_rel", 1.8, 535.667, 367.5)];
        let plain = trend_rows("2026-10-23", "d", &me, &at, &[]);
        let marked = trend_rows("2026-10-23", "d", &me, &at, &["effects"]);
        let figures = "ref_gpu_p50_us=535.667\tref_blank_gpu_p50_us=367.500\n";
        assert!(
            plain.ends_with(&format!("\tTest-Chip-8c\t{figures}")),
            "{plain:?}"
        );
        assert!(
            marked.ends_with(&format!("\tTest-Chip-8c\trerecorded\t{figures}")),
            "{marked:?}"
        );
        for (rows, rerecorded) in [(&plain, false), (&marked, true)] {
            let parsed = trend_ledger_rows(rows);
            assert_eq!(
                parsed[0].at,
                [("ref_gpu_p50_us", 535.667), ("ref_blank_gpu_p50_us", 367.5)]
            );
            assert_eq!(parsed[0].rerecorded, rerecorded);
            assert_eq!(
                rows.split('\t')
                    .nth(7)
                    .is_some_and(|c| c.trim() == RERECORDED),
                rerecorded
            );
        }
        let cpu = rows_for("2026-10-23", "d", "boxA", &[sample("effects", "m", 9.0)]);
        assert!(trend_ledger_rows(&cpu)[0].at.is_empty());
        let bad = "2026-10-23\td\tboxA\teffects/m\t9.0\ttrust\tTest-Chip-8c\tref_gpu_p50_us=nan\n";
        assert!(trend_ledger_rows(bad)[0].at.is_empty());
    }

    #[test]
    fn trend_inverted_metric_uses_min_best_and_a_ceiling() {
        let mut ledger = String::new();
        for v in [20.0, 18.0, 25.0] {
            ledger.push_str(&rows_for(
                "2026-07-22",
                "abc",
                "boxA",
                &[inverted_sample("resize", "resize_tiered_sync_worst_ms", v)],
            ));
        }
        let breaches = judged(
            &ledger,
            "boxA",
            &[inverted_sample(
                "resize",
                "resize_tiered_sync_worst_ms",
                30.0,
            )],
        )
        .breaches;
        assert_eq!(breaches.len(), 1);
        assert!((breaches[0].bound - 18.0 / TREND_RATIO).abs() < 1e-9);
        assert!(breaches[0].inverted, "breach is flagged inverted");
        for ok_v in [24.0, 5.0] {
            let ok = judged(
                &ledger,
                "boxA",
                &[inverted_sample(
                    "resize",
                    "resize_tiered_sync_worst_ms",
                    ok_v,
                )],
            );
            assert!(ok.breaches.is_empty(), "{ok_v} ms must pass");
        }
        let fresh = judged(
            &ledger,
            "boxB",
            &[inverted_sample(
                "resize",
                "resize_tiered_sync_worst_ms",
                30.0,
            )],
        );
        assert!(fresh.breaches.is_empty());
        assert_eq!(fresh.skipped, vec!["resize/resize_tiered_sync_worst_ms"]);
    }

    #[test]
    fn trend_first_run_on_a_box_never_blocks_and_reports_skip() {
        let j = judged("", "boxA", &[sample("throughput", "median_mbps", 100.0)]);
        assert!(j.breaches.is_empty());
        assert_eq!(j.skipped, vec!["throughput/median_mbps"]);
    }

    #[test]
    fn trend_legacy_five_column_rows_still_count_as_history() {
        let legacy = "2026-07-20\tabc\tboxA\tthroughput/median_mbps\t1000.000\n";
        let j = judged(
            legacy,
            "boxA",
            &[sample("throughput", "median_mbps", 400.0)],
        );
        assert_eq!(j.breaches.len(), 1, "legacy history still judges");
        assert!(j.skipped.is_empty());
    }

    #[test]
    fn trend_same_box_regression_trips_and_other_boxes_do_not() {
        let ledger = rows_for(
            "2026-07-22",
            "abc",
            "boxA",
            &[sample("throughput", "median_mbps", 1000.0)],
        );
        let breaches = judged(
            &ledger,
            "boxA",
            &[sample("throughput", "median_mbps", 400.0)],
        )
        .breaches;
        assert_eq!(breaches.len(), 1);
        assert!(
            (breaches[0].bound - 700.0).abs() < 1e-9,
            "floor is best x 0.70"
        );
        let cross = judged(
            &ledger,
            "boxB",
            &[sample("throughput", "median_mbps", 400.0)],
        );
        assert!(cross.breaches.is_empty());
        let ok = judged(
            &ledger,
            "boxA",
            &[sample("throughput", "median_mbps", 850.0)],
        );
        assert!(ok.breaches.is_empty());
    }

    #[test]
    fn trend_floor_is_best_of_window_so_drift_cannot_ratchet_it_down() {
        let mut ledger = String::new();
        for v in [1000.0, 900.0, 800.0] {
            ledger.push_str(&rows_for(
                "2026-07-22",
                "abc",
                "boxA",
                &[sample("scroll", "scrub_median_rps", v)],
            ));
        }
        let breaches = judged(
            &ledger,
            "boxA",
            &[sample("scroll", "scrub_median_rps", 650.0)],
        )
        .breaches;
        assert_eq!(breaches.len(), 1);
        let mut long = String::new();
        long.push_str(&rows_for(
            "2026-07-22",
            "abc",
            "boxA",
            &[sample("scroll", "scrub_median_rps", 1000.0)],
        ));
        for _ in 0..TREND_WINDOW {
            long.push_str(&rows_for(
                "2026-07-22",
                "abc",
                "boxA",
                &[sample("scroll", "scrub_median_rps", 800.0)],
            ));
        }
        let windowed = judged(
            &long,
            "boxA",
            &[sample("scroll", "scrub_median_rps", 650.0)],
        );
        assert!(
            windowed.breaches.is_empty(),
            "650 >= 0.70 x 800 (recent best), old 1000 aged out"
        );
    }

    #[test]
    fn trend_rows_round_trip_through_breach_parser() {
        let rows = rows_for(
            "2026-07-22",
            "abc123",
            "boxA",
            &[sample("search", "rotating_build_klps", 202.6)],
        );
        let ledger = format!("{TREND_HEADER}{rows}");
        let breaches = judged(
            &ledger,
            "boxA",
            &[sample("search", "rotating_build_klps", 100.0)],
        )
        .breaches;
        assert_eq!(breaches.len(), 1, "100 < 0.70 x 202.6");
        let ok = judged(
            &ledger,
            "boxA",
            &[sample("search", "rotating_build_klps", 200.0)],
        );
        assert!(ok.breaches.is_empty());
        assert_eq!(
            rows.trim_end().split('\t').nth(5),
            Some("trust"),
            "row: {rows:?}"
        );
    }

    /// A row names the tree it measured: HEAD's sha alone only when nothing outside the golden
    /// dir (which a record rewrites) differs from HEAD.
    #[test]
    fn a_row_measured_on_uncommitted_code_says_so() {
        assert_eq!(tree_label("abc", Some("")), "abc");
        let golden = " M crates/aterm-verify/perf/golden/perf-baseline-effects.json\n\
                      M  crates/aterm-verify/perf/golden/perf-trend.tsv\n";
        assert_eq!(tree_label("abc", Some(golden)), "abc");
        let code = format!("{golden} M crates/aterm-gpu/src/renderer.rs\n");
        assert_eq!(tree_label("abc", Some(&code)), "abc+dirty");
        let renamed = "R  crates/aterm-verify/perf/golden/a.json -> crates/aterm-gpu/a.json\n";
        assert_eq!(tree_label("abc", Some(renamed)), "abc+dirty");
        assert_eq!(
            tree_label("abc", None),
            "abc+dirty",
            "an unread status vouches nothing"
        );
    }

    #[test]
    fn trend_rows_record_the_wasm_lanes_toolchain_and_engine() {
        let rows = rows_for(
            "2026-07-22",
            "abc",
            "boxA",
            &[sample("wasm", "wasm_cpu_ingest_mixed_mbps", 343.0)],
        );
        assert_eq!(
            rows.trim_end().split('\t').nth(5),
            Some("stable+chrome-headless-shell"),
            "row: {rows:?}"
        );
        assert_eq!(
            trend_ledger_rows(&rows)[0].toolchain,
            "stable+chrome-headless-shell"
        );
    }

    /// Rows another engine measured anchor nothing: the wasm lane's node rows
    /// (`stable`) never hold the browser's numbers, and once the browser has
    /// rows of its own they are the window. A row from before the toolchain
    /// column cuts no window (the trust lanes' oldest rows). NEGATIVE
    /// CONTROL: the same history spelled as the browser's breaches.
    #[test]
    fn another_engines_rows_start_no_window_for_this_one() {
        let row = |toolchain: &str, value: f64| {
            format!("2026-10-01\tabc\tboxA\twasm/w\t{value:.3}\t{toolchain}\tTest-Chip-8c\n")
        };
        let node: String = (0..4).map(|_| row("stable", 1000.0)).collect();
        let now = [sample("wasm", "w", 100.0)];
        let fresh = judged(&node, "boxA", &now);
        assert!(fresh.breaches.is_empty(), "{:?}", fresh.breaches);
        assert_eq!(fresh.skipped, ["wasm/w"], "no history in this engine");
        let browser = row("stable+chrome-headless-shell", 120.0);
        let ledger = format!("{node}{browser}");
        assert!(
            judged(&ledger, "boxA", &now).breaches.is_empty(),
            "100 >= 0.70 x 120"
        );
        let low = [sample("wasm", "w", 50.0)];
        assert_eq!(
            judged(&ledger, "boxA", &low).breaches.len(),
            1,
            "50 < 0.70 x 120"
        );
        let same: String = (0..4)
            .map(|_| row("stable+chrome-headless-shell", 1000.0))
            .collect();
        assert_eq!(
            judged(&same, "boxA", &now).breaches.len(),
            1,
            "CONTROL: 100 < 0.70 x 1000 in the same engine"
        );
        let old = "2026-07-01\tabc\tboxA\tsearch/q\t1000.000\n";
        let trust = format!("{old}2026-07-02\tabc\tboxA\tsearch/q\t900.000\ttrust\n");
        let held = judged(&trust, "boxA", &[sample("search", "q", 650.0)]);
        assert_eq!(
            held.breaches.len(),
            1,
            "650 < 0.70 x 1000: the old row still counts"
        );
    }

    const ALIASES: &str = "# box\tidentity\tnote\n\
         m21\tfp:deadbeefdeadbeef\tthe box\n\
         m21\thost:m21.local\tafter the rename\n\
         m21\thost:m15.local\tbefore the rename — SAME machine\n\
         m7\tfp:0123456789abcdef\ta different box\n";

    /// A rename must not fork a box's history away from itself: both spellings
    /// land on one key, and a machine the table never mentions on its own.
    #[test]
    fn a_rename_no_longer_forks_a_box_from_its_own_history() {
        assert_eq!(resolve_ledger_box(ALIASES, "m15.local").key(), "m21");
        assert_eq!(resolve_ledger_box(ALIASES, "m21.local").key(), "m21");
        assert_eq!(resolve_ledger_box(ALIASES, "m21").key(), "m21");
        assert!(resolve_ledger_box(ALIASES, "m21").is_claimed());
        let mut ledger = String::new();
        ledger.push_str("2026-07-22\tabc\tm15.local\tthroughput/median_mbps\t1000.000\n");
        let me = MachineId {
            key: "m21".to_string(),
            shape: "Apple-M5-Max-18c".to_string(),
            identities: vec!["fp:deadbeefdeadbeef".to_string()],
            claimed: true,
        };
        let j = judge_trend(
            &ledger,
            ALIASES,
            &me,
            &[sample("throughput", "median_mbps", 400.0)],
        );
        assert_eq!(
            j.breaches.len(),
            1,
            "the pre-rename rows must judge the post-rename run"
        );
        assert!(
            j.skipped.is_empty(),
            "history was found, so nothing skipped"
        );
    }

    /// THE OTHER HALF, and the one that matters more: an identity nobody
    /// claimed is NEVER folded into a box that merely looks similar.
    #[test]
    fn an_unclaimed_identity_stands_alone_and_is_never_merged() {
        let r = resolve_ledger_box(ALIASES, "some-other-mac.local");
        assert_eq!(r.key(), "some-other-mac.local");
        assert!(!r.is_claimed());
        let ledger = "2026-07-22\tabc\tsome-other-mac.local\tthroughput/median_mbps\t1000.000\n";
        let me = MachineId {
            key: "m21".to_string(),
            shape: "Apple-M5-Max-18c".to_string(),
            identities: vec!["fp:deadbeefdeadbeef".to_string()],
            claimed: true,
        };
        let j = judge_trend(
            ledger,
            ALIASES,
            &me,
            &[sample("throughput", "median_mbps", 400.0)],
        );
        assert!(j.breaches.is_empty());
        assert_eq!(j.skipped, vec!["throughput/median_mbps"]);
    }

    /// A box that has already written AUTO-KEYED rows keeps them when a human
    /// finally names it — `box-<digest>` resolves through the `fp:` row.
    #[test]
    fn naming_a_box_adopts_the_rows_it_wrote_before_it_had_a_name() {
        assert_eq!(
            resolve_ledger_box(ALIASES, "box-deadbeefdeadbeef").key(),
            "m21"
        );
        assert!(resolve_ledger_box(ALIASES, "box-deadbeefdeadbeef").is_claimed());
        assert!(!resolve_ledger_box(ALIASES, "box-1111111111111111").is_claimed());
    }

    /// THE GUARD ON THE CLAIM.
    #[test]
    fn a_chip_conflict_drops_the_rows_instead_of_comparing_across_machines() {
        let ledger =
            "2026-07-22\tabc\tm21\tthroughput/median_mbps\t9000.000\ttrust\tIntel-Core-i9-8c\n";
        let me = MachineId {
            key: "m21".to_string(),
            shape: "Apple-M5-Max-18c".to_string(),
            identities: vec!["fp:deadbeefdeadbeef".to_string()],
            claimed: true,
        };
        let j = judge_trend(
            ledger,
            ALIASES,
            &me,
            &[sample("throughput", "median_mbps", 400.0)],
        );
        assert_eq!(
            j.conflicts,
            vec![("m21".to_string(), "Intel-Core-i9".to_string())]
        );
        assert!(j.breaches.is_empty());
        assert_eq!(j.skipped, vec!["throughput/median_mbps"]);
        let same =
            "2026-07-22\tabc\tm21\tthroughput/median_mbps\t1000.000\ttrust\tApple-M5-Max-10c\n";
        let ok = judge_trend(
            same,
            ALIASES,
            &me,
            &[sample("throughput", "median_mbps", 400.0)],
        );
        assert!(ok.conflicts.is_empty());
        assert_eq!(ok.breaches.len(), 1, "same chip ⇒ still same-box history");
    }

    #[test]
    fn chip_of_strips_only_a_trailing_core_count() {
        assert_eq!(chip_of("Apple-M5-Max-18c"), "Apple-M5-Max");
        assert_eq!(chip_of("Apple-M5-Max-1c"), "Apple-M5-Max");
        assert_eq!(chip_of("Apple-M5-Max"), "Apple-M5-Max");
        assert_eq!(chip_of("weird-c"), "weird-c");
        assert_eq!(chip_of("weird-xc"), "weird-xc");
        assert_eq!(chip_of(""), "");
        assert_eq!(chip_of("c"), "c");
    }

    #[test]
    fn default_box_key_prefers_the_fingerprint() {
        assert_eq!(
            default_box_key(&["fp:abc".to_string(), "host:x.local".to_string()]),
            "box-abc"
        );
        assert_eq!(
            default_box_key(&["host:m21.local".to_string()]),
            "host-m21-local"
        );
        assert_eq!(default_box_key(&[]), "unknown-box");
    }

    #[test]
    fn a_seven_column_row_round_trips_with_its_shape() {
        let me = MachineId {
            key: "m21".to_string(),
            shape: "Apple-M5-Max-18c".to_string(),
            identities: vec!["fp:deadbeefdeadbeef".to_string()],
            claimed: true,
        };
        let rows = trend_rows(
            "2026-08-23",
            "abc123",
            &me,
            &[sample("throughput", "median_mbps", 500.0)],
            &[],
        );
        let parsed = trend_ledger_rows(&rows);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].token, "m21");
        assert_eq!(parsed[0].shape, "Apple-M5-Max-18c");
        assert_eq!(parsed[0].metric, "throughput/median_mbps");
        let five = "2026-07-20\tabc\tboxA\tthroughput/median_mbps\t1000.000\n";
        assert_eq!(trend_ledger_rows(five)[0].shape, "");
    }

    #[test]
    fn the_digest_is_stable_and_separates_two_uuids() {
        assert_eq!(
            format!(
                "{:016x}",
                aterm_hash::fnv1a_64(b"4F2C6182-55E3-51B1-B505-42FD89F13166")
            ),
            "4d119fc8de0a3b9e"
        );
        assert_ne!(aterm_hash::fnv1a_64(b"a"), aterm_hash::fnv1a_64(b"b"));
    }

    #[test]
    fn an_orphan_on_a_claimed_box_chip_is_named_as_a_likely_rename() {
        let aliases = "m21\thost:m15\tthe M5 Max\n";
        let ledger = "2026-07-01\ta\tm15\tthroughput/median_mbps\t1000.0\ttrust\tApple M5 Max-18\n\
                      2026-07-02\ta\tm15-renamed\tthroughput/median_mbps\t990.0\ttrust\tApple M5 Max-18\n\
                      2026-07-02\ta\tm15-renamed\trestore/restore_median_hz\t9.0\ttrust\tApple M5 Max-18\n\
                      2026-07-03\ta\tnewbox\tthroughput/median_mbps\t700.0\ttrust\tApple M4-10\n\
                      2026-07-04\ta\told\tthroughput/median_mbps\t600.0\n";
        let rows = trend_ledger_rows(ledger);
        assert_eq!(
            orphans(&rows, aliases),
            vec![
                ("m15-renamed", Some("m21".to_string())),
                ("newbox", None),
                ("old", None),
            ]
        );
    }

    #[test]
    fn rfc3339_renders_epoch_seconds_exactly() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(rfc3339(951_782_400), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn utc_date_is_iso_shaped() {
        let d = utc_date();
        assert_eq!(d.len(), 10);
        assert_eq!(d.as_bytes()[4], b'-');
        assert_eq!(d.as_bytes()[7], b'-');
        assert!(d.starts_with("20"), "sane century: {d}");
    }
}
