// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The SAME-BOX TREND LEDGER and the machine identity it is keyed by.

use super::LaneRun;
use super::floors::golden_dir;

/// Same-box floor ratio.
pub(crate) const TREND_RATIO: f64 = 0.70;

/// How many most-recent same-box entries per metric form the reference.
pub(crate) const TREND_WINDOW: usize = 10;

/// How far apart one figure of two operating points may be and still be one machine state.
pub(crate) const OPERATING_POINT_BAND: f64 = 1.25;

/// One figure of the machine state a sample was measured in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PointFigure {
    /// The harness report key that measured it (`ref_gpu_p50_us`).
    pub name: &'static str,
    pub value: f64,
}

/// One measured median headed for the ledger.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TrendSample {
    pub lane: &'static str,
    pub metric: String,
    pub value: f64,
    /// LOWER is better (latency metrics, e.g. the resize lane's pump totals and worsts).
    pub inverted: bool,
    /// The figures of the operating point it was measured at.
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
    /// The ledger's box column.
    pub key: String,
    /// Chip + logical cores (`Apple-M5-Max-18c`).
    pub shape: String,
    /// The raw tokens this machine answers to, strongest first.
    pub identities: Vec<String>,
    /// An alias row explicitly claimed one of [`Self::identities`].
    pub claimed: bool,
}

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

fn platform_uuid() -> Option<String> {
    if cfg!(target_os = "macos") {
        let out = crate::exec::check_command(aterm_uds::os_path!("ioreg"))
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
        let out = crate::exec::check_command("reg")
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
        if let Ok(s) = crate::read_regular_text(p) {
            let t = s.trim().to_string();
            if !t.is_empty() {
                return Some(t);
            }
        }
    }
    None
}

fn platform_fingerprint() -> Option<String> {
    let raw = platform_uuid()?;
    let t = raw.trim();
    (!t.is_empty()).then(|| format!("{:016x}", aterm_hash::fnv1a_64(t.as_bytes())))
}

fn hostname() -> String {
    crate::exec::check_command(aterm_uds::os_path!("hostname"))
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown-host".to_string())
}

fn machine_shape() -> String {
    let chip = if cfg!(target_os = "macos") {
        crate::exec::check_command(aterm_uds::os_path!("sysctl"))
            .args(["-n", "machdep.cpu.brand_string"])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
    } else {
        crate::read_regular_text("/proc/cpuinfo")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("model name"))
                    .and_then(|l| l.split_once(':'))
                    .map(|(_, v)| v.trim().to_string())
            })
    };
    let chip = chip.unwrap_or_else(|| std::env::consts::ARCH.to_string());
    format!("{}-{}c", slug(&chip), aterm_capacity::cores())
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

/// The box an alias table gives `identity` (`fp:<digest>` / `host:<name>`).
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
    /// An alias row claims this token (as `host:<token>`/`fp:<token>`) or declares it as a box name
    /// outright.
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
    let aliases = crate::read_regular_text(boxes_path()).unwrap_or_default();
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

/// HEAD's short `sha` as [`tree_label`] labels it.
pub(crate) fn head_sha() -> String {
    let git = |args: &[&str]| {
        crate::exec::check_command(aterm_uds::os_path!("git"))
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

/// The tree a run's rows name: `start`, read before its first lane built anything, made `+dirty`
/// when the tree read as the rows are appended differs (a commit, a checkout or an edit while the
/// lanes built and ran their harnesses).
pub(crate) fn measured_tree(start: &str, end: &str) -> String {
    if start == end || start == "unknown" || start.ends_with("+dirty") {
        start.to_string()
    } else {
        format!("{start}+dirty")
    }
}

const GOLDEN_REL: &str = "crates/aterm-verify/perf/golden/";

/// HEAD's short `sha`, with `+dirty` when `git status --porcelain` names a tracked file outside
/// [`GOLDEN_REL`] that differs from HEAD (or could not be read).
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

const RERECORDED: &str = "rerecorded";

/// One parsed ledger row.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TrendRow<'a> {
    pub token: &'a str,
    pub metric: &'a str,
    pub value: f64,
    /// What measured it ([`toolchain_lane`]).
    pub toolchain: &'a str,
    /// `""` on every row written before the shape column existed.
    pub shape: &'a str,
    /// A run that re-recorded this row's lane wrote it.
    pub rerecorded: bool,
    /// The figures of the operating point it was measured at.
    pub at: Vec<(&'a str, f64)>,
}

fn parse_point(col: &str) -> Option<(&str, f64)> {
    let (name, value) = col.trim().split_once('=')?;
    let value = value
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && *v > 0.0)?;
    (!name.is_empty()).then_some((name, value))
}

/// Whether a row measured at `row` may judge a sample measured at `sample`.
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
        let Some(value) = cols[4].parse::<f64>().ok().filter(|v| v.is_finite()) else {
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

/// The ledger tokens no alias row claims.
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
    /// Metrics whose same-box history was all measured at another operating point.
    pub elsewhere: Vec<Elsewhere>,
    /// `(token, chip)` pairs that resolve to THIS box but were measured on a different CHIP.
    pub conflicts: Vec<(String, String)>,
}

/// Judge `samples` against the same-box history in the ledger text.
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
        let toolchain = toolchain_lane(s.lane);
        let recorded = rows.iter().rposition(|r| r.rerecorded).unwrap_or(0);
        let changed = rows
            .iter()
            .rposition(|r| !r.toolchain.is_empty() && r.toolchain != toolchain)
            .map_or(0, |i| i + 1);
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
TREND_WINDOW entries\n# (crates/aterm-verify/src/perf/trend.rs; the resize lane's *_ms metrics are \
INVERTED — bounded by best-MIN / TREND_RATIO).\n# The BOX column is a name from crates/aterm-verify/perf/golden/perf-boxes.tsv, \
which is also where a\n# pre-W-1 hostname is claimed as the box it was measured on. NEVER edit a \
box name here\n# to merge two machines — add the claim to perf-boxes.tsv, where the chip column \
keeps it\n# honest. A row a re-record (`--record LANE`) wrote carries `rerecorded` in an eighth column:\n# that lane's new level is declared, and each of its metrics' windows starts there. A row \
measured\n# at an operating point carries its figures last (the effects GPU keys: \
`ratio_ref_gpu_p50_us=<us>`,\n# `ratio_ref_blank_gpu_p50_us=<us>`, from the run supplying that ratio's median); \
a sample that names one is judged only \
against rows whose\n# figures are each within OPERATING_POINT_BAND of its own.\n# \
date\tsha\tbox\tlane/metric\tvalue\ttoolchain\tshape\trerecorded\tpoint...\n";

/// Whether this run's samples join the ledger.
pub(crate) fn may_append(lanes_ok: bool, judgment: &TrendJudgment, recorded: &[&str]) -> bool {
    lanes_ok
        && judgment.conflicts.is_empty()
        && judgment.breaches.iter().all(|b| recorded.contains(&b.lane))
}

/// The trend sub-gate. `tree` is [`head_sha`] read before the run's first lane.
pub(crate) fn gate_trend(
    samples: &[TrendSample],
    lanes_ok: bool,
    recorded: &[&str],
    tree: &str,
) -> LaneRun {
    gate_trend_at(
        &trend_path(),
        &boxes_path(),
        &machine_id(),
        samples,
        lanes_ok,
        recorded,
        tree,
    )
}

fn gate_trend_at(
    path: &std::path::Path,
    aliases_path: &std::path::Path,
    me: &MachineId,
    samples: &[TrendSample],
    lanes_ok: bool,
    recorded: &[&str],
    tree: &str,
) -> LaneRun {
    gate_trend_using(
        path,
        aliases_path,
        me,
        samples,
        lanes_ok,
        recorded,
        tree,
        super::publication::publish,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "the publication seam preserves the existing trend gate inputs"
)]
fn gate_trend_using(
    path: &std::path::Path,
    aliases_path: &std::path::Path,
    me: &MachineId,
    samples: &[TrendSample],
    lanes_ok: bool,
    recorded: &[&str],
    tree: &str,
    publish: super::publication::Publisher,
) -> LaneRun {
    let aliases = match read_trend_input(aliases_path) {
        Ok(text) => text,
        Err(error) => {
            return LaneRun::fail(format!(
                "cannot read trend aliases {}: {error}",
                aliases_path.display()
            ));
        }
    };
    let ledger = match read_trend_input(path) {
        Ok(text) => text,
        Err(error) => {
            return LaneRun::fail(format!(
                "cannot read trend ledger {}: {error}",
                path.display()
            ));
        }
    };
    let judgment = judge_trend(&ledger, &aliases, me, samples);
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
            aliases_path.display(),
            me.identities.first().map_or("host:?", String::as_str)
        );
    }

    for (token, same_chip) in orphans(&trend_ledger_rows(&ledger), &aliases) {
        match same_chip {
            Some(claimed) => eprintln!(
                "  trend: NOTE — ledger rows keyed `{token}` are claimed by no row of {} and \
                 were measured on the chip of box `{claimed}`: a renamed machine looks like \
                 this. Claim `{token}` for `{claimed}` there, or note why it is another machine.",
                aliases_path.display()
            ),
            None => eprintln!(
                "  trend: NOTE — ledger rows keyed `{token}` are claimed by no row of {}; \
                 they anchor nothing until some box declares them.",
                aliases_path.display()
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
            aliases_path.display()
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
            aliases_path.display()
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
        append_trend_rows(
            &mut text,
            &trend_rows(
                &utc_date(),
                &measured_tree(tree, &head_sha()),
                me,
                samples,
                recorded,
            ),
        );
        if let Err(e) = publish(path, &text) {
            run.add_failure(format!(
                "could not append trend ledger {}: {e}",
                path.display()
            ));
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

fn read_trend_input(path: &std::path::Path) -> std::io::Result<String> {
    match crate::read_regular_text(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        result => result,
    }
}

fn append_trend_rows(text: &mut String, rows: &str) {
    if !rows.is_empty() && !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(rows);
}

#[cfg(test)]
#[path = "trend_tests.rs"]
mod tests;
