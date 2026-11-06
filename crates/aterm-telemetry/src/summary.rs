// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE READER: `aterm ctl telemetry` — one line a kind (count, failures, p50 and p90 of wall
//! time, CPU and peak memory, and how many rows did not know each), or the rows themselves.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::log::Reading;
use crate::row::{Fig, Outcome, Row, Stop};

/// p50 and p90 of the rows that knew a figure, and how many did not.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Spread {
    /// Rows that knew it.
    pub n: u64,
    /// Rows that did not (unknown or not applicable).
    pub unknown: u64,
    /// The median (nearest rank).
    pub p50: Option<u64>,
    /// The 90th percentile (nearest rank).
    pub p90: Option<u64>,
}

#[derive(Default)]
struct SpreadAcc {
    known: Vec<u64>,
    unknown: u64,
}

impl SpreadAcc {
    fn push(&mut self, fig: Option<u64>) {
        match fig {
            Some(v) => self.known.push(v),
            None => self.unknown += 1,
        }
    }

    fn finish(mut self) -> Spread {
        self.known.sort_unstable();
        Spread {
            n: self.known.len() as u64,
            unknown: self.unknown,
            p50: rank(&self.known, 50),
            p90: rank(&self.known, 90),
        }
    }
}

#[derive(Default)]
struct KindAcc {
    n: u64,
    failed: u64,
    stalled: u64,
    wall: SpreadAcc,
    cpu: SpreadAcc,
    peak: SpreadAcc,
}

/// The nearest-rank `pct`th percentile of sorted `v`.
#[must_use]
pub fn rank(v: &[u64], pct: u64) -> Option<u64> {
    if v.is_empty() {
        return None;
    }
    let n = v.len() as u64;
    let idx = (pct * n).div_ceil(100).max(1) - 1;
    v.get(usize::try_from(idx).ok()?).copied()
}

/// One kind's line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KindLine {
    /// The kind's word.
    pub kind: String,
    /// Rows.
    pub n: u64,
    /// Rows whose outcome was `fail` or `refused`.
    pub failed: u64,
    /// Rows that stopped `stalled` or under `pressure`.
    pub stalled: u64,
    /// Wall time, ms.
    pub wall: Spread,
    /// CPU, ms.
    pub cpu: Spread,
    /// Peak memory, bytes.
    pub peak: Spread,
}

/// One line a kind, in kind order.
#[must_use]
pub fn by_kind(rows: &[Row]) -> Vec<KindLine> {
    let mut kinds: BTreeMap<&str, KindAcc> = BTreeMap::new();
    for row in rows {
        let acc = kinds.entry(row.kind.word()).or_default();
        acc.n += 1;
        if matches!(row.outcome, Outcome::Fail | Outcome::Refused) {
            acc.failed += 1;
        }
        if matches!(row.stop, Stop::Stalled | Stop::Pressure) {
            acc.stalled += 1;
        }
        acc.wall.push(row.wall_ms.known());
        acc.cpu.push(row.cpu_ms.known());
        acc.peak.push(row.peak_bytes.known());
    }
    kinds
        .into_iter()
        .map(|(kind, acc)| KindLine {
            kind: kind.to_owned(),
            n: acc.n,
            failed: acc.failed,
            stalled: acc.stalled,
            wall: acc.wall.finish(),
            cpu: acc.cpu.finish(),
            peak: acc.peak.finish(),
        })
        .collect()
}

/// What `--since` accepts: `<n>s|m|h|d` back from `now_ms`, or a unix ms time.
///
/// # Errors
/// Any other word, said as the reader says it.
pub fn parse_since(word: &str, now_ms: u64) -> Result<u64, String> {
    let bad = || format!("--since takes <n>s, <n>m, <n>h, <n>d or a unix ms time, not `{word}`");
    let (num, unit) = word.split_at(
        word.find(|c: char| !c.is_ascii_digit())
            .unwrap_or(word.len()),
    );
    let n: u64 = num.parse().map_err(|_| bad())?;
    let ms = match unit {
        "" => return Ok(n),
        "s" => n.saturating_mul(1_000),
        "m" => n.saturating_mul(60_000),
        "h" => n.saturating_mul(3_600_000),
        "d" => n.saturating_mul(86_400_000),
        _ => return Err(bad()),
    };
    Ok(now_ms.saturating_sub(ms))
}

fn push_opt(out: &mut String, v: Option<u64>) {
    match v {
        Some(v) => {
            let _ = write!(out, "{v}");
        }
        None => out.push('-'),
    }
}

fn push_mib(out: &mut String, v: Option<u64>) {
    match v {
        Some(b) => {
            let _ = write!(out, "{}", b >> 20);
        }
        None => out.push('-'),
    }
}

/// The reader's head line: what was read and how far back it reaches.
#[must_use]
pub fn head(reading: &Reading, since_ms: Option<u64>, dir: &str) -> String {
    let facts = reading.hosts.last();
    let mut out = String::with_capacity(160 + dir.len());
    let _ = write!(
        out,
        "telemetry schema={} rows={} dropped={} torn={} files={} bytes={} since=",
        crate::row::SCHEMA,
        reading.rows.len(),
        reading.dropped,
        reading.torn,
        reading.files,
        reading.bytes,
    );
    push_opt(&mut out, since_ms);
    out.push_str(" oldest=");
    push_opt(&mut out, reading.oldest_ms);
    let _ = write!(
        out,
        " host={} facts=",
        facts.map_or("-", |h| h.spelled.host.as_str()),
    );
    match facts {
        Some(h) => crate::row::push_hex(&mut out, &h.facts.0),
        None => out.push('-'),
    }
    out.push_str(" dir=");
    if dir.contains(' ') {
        for ch in dir.chars() {
            if ch == ' ' {
                out.push_str("%20");
            } else {
                out.push(ch);
            }
        }
    } else {
        out.push_str(dir);
    }
    out
}

/// The summary: [`head`], then one `kind=` line a kind.
#[must_use]
pub fn render(reading: &Reading, since_ms: Option<u64>, dir: &str) -> String {
    let mut out = head(reading, since_ms, dir);
    out.push('\n');
    for k in by_kind(&reading.rows) {
        let _ = write!(
            out,
            "kind={} n={} failed={} stalled={} wall_p50_ms=",
            k.kind, k.n, k.failed, k.stalled
        );
        push_opt(&mut out, k.wall.p50);
        out.push_str(" wall_p90_ms=");
        push_opt(&mut out, k.wall.p90);
        let _ = write!(out, " wall_unknown={} cpu_p50_ms=", k.wall.unknown);
        push_opt(&mut out, k.cpu.p50);
        out.push_str(" cpu_p90_ms=");
        push_opt(&mut out, k.cpu.p90);
        let _ = write!(out, " cpu_unknown={} peak_p50_mib=", k.cpu.unknown);
        push_mib(&mut out, k.peak.p50);
        out.push_str(" peak_p90_mib=");
        push_mib(&mut out, k.peak.p90);
        let _ = writeln!(out, " peak_unknown={}", k.peak.unknown);
    }
    out
}

/// The rows themselves: [`head`], then the newest `limit` rows of `kind` (every kind for
/// `None`), each as the log spells it less its checksum.
#[must_use]
pub fn render_rows(
    reading: &Reading,
    since_ms: Option<u64>,
    dir: &str,
    kind: Option<&str>,
    limit: usize,
) -> String {
    let mut out = head(reading, since_ms, dir);
    out.push('\n');
    let mut tail: Vec<&Row> = reading
        .rows
        .iter()
        .rev()
        .filter(|r| kind.is_none_or(|k| r.kind.word() == k))
        .take(limit)
        .collect();
    tail.reverse();
    for row in tail {
        row.write_body(&mut out);
        out.push('\n');
    }
    out
}

fn fig_json(f: &Fig<u64>) -> aterm_json::Value {
    match f {
        Fig::Known(v) => aterm_json::json!(v),
        Fig::Na => aterm_json::json!("na"),
        Fig::Unknown(source) => aterm_json::json!({ "unknown": source }),
    }
}

fn spread_json(s: &Spread) -> aterm_json::Value {
    aterm_json::json!({ "n": s.n, "unknown": s.unknown, "p50": s.p50, "p90": s.p90 })
}

/// The summary as one JSON object (`--json`).
#[must_use]
pub fn render_json(reading: &Reading, since_ms: Option<u64>, dir: &str, rows: bool) -> String {
    let kinds: Vec<aterm_json::Value> = by_kind(&reading.rows)
        .iter()
        .map(|k| {
            aterm_json::json!({
                "kind": k.kind,
                "n": k.n,
                "failed": k.failed,
                "stalled": k.stalled,
                "wall_ms": spread_json(&k.wall),
                "cpu_ms": spread_json(&k.cpu),
                "peak_bytes": spread_json(&k.peak),
            })
        })
        .collect();
    let facts = reading.hosts.last().map(|h| {
        aterm_json::json!({
            "digest": h.facts.hex(),
            "host": h.spelled.host,
            "model": h.spelled.model,
            "cpu": h.spelled.cpu,
            "cores": h.spelled.cores,
            "mem_mib": h.spelled.mem_mib,
            "os": h.spelled.os,
            "aterm": h.spelled.aterm,
        })
    });
    let mut doc = aterm_json::json!({
        "schema": crate::row::SCHEMA,
        "dir": dir,
        "since": since_ms,
        "oldest": reading.oldest_ms,
        "rows_read": reading.rows.len(),
        "dropped": reading.dropped,
        "torn": reading.torn,
        "files": reading.files,
        "bytes": reading.bytes,
        "host": facts,
        "kinds": kinds,
    });
    if rows && let aterm_json::Value::Object(map) = &mut doc {
        let list: Vec<aterm_json::Value> = reading
            .rows
            .iter()
            .map(|r| {
                aterm_json::json!({
                    "at": r.at_ms,
                    "kind": r.kind.word(),
                    "key": r.key.map(crate::row::Digest::hex),
                    "dir": r.dir.map(crate::row::Digest::hex),
                    "label": r.label,
                    "sid": r.sid,
                    "host": r.host,
                    "facts": r.facts.hex(),
                    "wall_ms": fig_json(&r.wall_ms),
                    "cpu_ms": fig_json(&r.cpu_ms),
                    "peak_bytes": fig_json(&r.peak_bytes),
                    "wait_ms": fig_json(&r.wait_ms),
                    "cache": match &r.cache {
                        Fig::Known(t) => aterm_json::json!(t),
                        Fig::Na => aterm_json::json!("na"),
                        Fig::Unknown(s) => aterm_json::json!({ "unknown": s }),
                    },
                    "outcome": r.outcome.word(),
                    "stop": r.stop.word(),
                    "code": r.code,
                    "sig": r.sig,
                    "meter": r.meter.word(),
                })
            })
            .collect();
        map.insert("rows".to_owned(), aterm_json::Value::Array(list));
    }
    aterm_json::to_string(&doc).unwrap_or_else(|_| "{}".to_owned())
}
