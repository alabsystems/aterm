// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm pkg capacity`: the machine's capacity now and every claim in this user's ledger
//! (`aterm_capacity`, `docs/DESIGN-adaptive-build-2026-10-24.md` CAP-1). Read-only: it admits
//! nothing and removes no dead holder's file. Every claim's line, held or waiting, says what it
//! reserves against the claims behind it, as their looks reckon it
//! ([`aterm_capacity::reserved`]): its memory (`mem <need>, reserves <bytes>`: its peak less what
//! its holder's tree and the processes credited to it hold in memory, `aterm_capacity::Proc`)
//! and its disk on its own volume (`disk <need> on <mount>, reserves <bytes> against the claims
//! behind it`: its need less what it has written). A waiting claim's disk is marked
//! [`UNSERVABLE`] where its need is more than that volume has free above the floor now: no
//! claim's end can make room for it, only space freed, so it holds back no claim behind it
//! (DISK-ADMIT, [`aterm_capacity::Short::unservable`]); a held claim is never marked. Each volume
//! a claim's disk is on is read, besides the one holding `<path>`, and has a disk line of its own
//! (and, in `--json`, an entry of `volumes`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use aterm_capacity::{
    Ledger, Live, Machine as _, Reading, Record, Reserve, TreeUse, Volume, gib, span,
};
use aterm_json::{Map, Value};

/// The verb: `aterm pkg capacity [--json] [<path>]`, the volume the one holding `<path>` (the working directory by default).
pub(crate) fn verb(rest: &[String]) -> ExitCode {
    let mut json = false;
    let mut path: Option<PathBuf> = None;
    for arg in rest {
        match arg.as_str() {
            "--json" => json = true,
            flag if flag.starts_with('-') => {
                return crate::cli::usage_error("capacity", Some(&format!("unknown flag {flag}")));
            }
            p if path.is_none() => path = Some(PathBuf::from(p)),
            _ => return crate::cli::usage_error("capacity", Some("one path at most")),
        }
    }
    let path = path
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let reading = Live.read(Some(&path));
    let ledger = Ledger::user();
    let claims = match ledger.as_ref().map(Ledger::list) {
        Some(Ok(claims)) => Ok(claims),
        Some(Err(e)) => Err(e.to_string()),
        None => Err("no home directory to keep it under".to_owned()),
    };
    let trees = match &claims {
        Ok(c) if !c.is_empty() => Live.procs().map(|p| {
            let pids: Vec<u32> = c
                .iter()
                .flat_map(|r| std::iter::once(r.pid).chain(r.credit.iter().copied()))
                .collect();
            p.trees(&pids)
        }),
        _ => None,
    };
    let volumes = match &claims {
        Ok(c) => claim_volumes(c, reading.volume.as_ref()),
        Err(_) => Vec::new(),
    };
    let now = aterm_time::unix_millis().unwrap_or(0);
    let dir = ledger.as_ref().map(Ledger::dir);
    if json {
        println!(
            "{}",
            json_of(&reading, &volumes, dir, &claims, trees.as_ref(), now)
        );
    } else {
        for line in lines(&reading, &volumes, dir, &claims, trees.as_ref(), now) {
            println!("{line}");
        }
    }
    if claims.is_err() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// The volumes the claims' disk is on besides `read` (the one holding `<path>`), each read now,
/// so each claim is reckoned on its own volume as the looks of the claims behind it there
/// reckon it. One that cannot be read, or reads as another mount (unmounted since), is left out:
/// its claims are not judged ([`NOT_READ`]).
fn claim_volumes(claims: &[Record], read: Option<&Volume>) -> Vec<Volume> {
    let mut tried: Vec<&Path> = read.map(|v| v.mount.as_path()).into_iter().collect();
    let mut out = Vec::new();
    for mount in claims
        .iter()
        .filter(|c| c.need.disk > 0)
        .filter_map(|c| c.need.path.as_deref())
    {
        if tried.contains(&mount) {
            continue;
        }
        tried.push(mount);
        if let Some(v) = Live.read(Some(mount)).volume.filter(|v| v.mount == mount) {
            out.push(v);
        }
    }
    out
}

/// What each claim reserves against the claims behind it, each on its own volume: `r`'s or one
/// of `others` ([`claim_volumes`]).
fn reserves(
    r: &Reading,
    others: &[Volume],
    claims: &[Record],
    trees: Option<&HashMap<u32, TreeUse>>,
) -> Vec<Reserve> {
    let volumes: Vec<Volume> = r.volume.iter().chain(others).cloned().collect();
    aterm_capacity::reserved(claims, trees, &volumes)
}

/// The mark on a waiting claim's disk whose need is more than its volume has free above the
/// floor now ([`Reserve::unservable`]): it waits for space to be freed and reserves no disk.
const UNSERVABLE: &str = "unservable";

/// What a claim's disk says where its volume could not be read, so it is not judged.
const NOT_READ: &str = "its volume could not be read";

/// A volume's line: its free space, its floor and what is above it.
fn disk_line(v: &Volume) -> String {
    format!(
        "disk     {} free on {} of {}, floor {} (the window's disk watch's, its room {}): {} above it",
        gib(v.free),
        v.mount.display(),
        gib(v.total),
        gib(v.floor),
        gib(v.room),
        gib(v.above_floor())
    )
}

/// A figure, or `unknown` where this OS does not report it.
fn or_unknown<T>(v: Option<T>, show: impl Fn(T) -> String) -> String {
    v.map_or_else(|| "unknown".to_owned(), show)
}

/// The report a person reads, a line for each of `volumes` (the other volumes claims are on,
/// [`claim_volumes`]) after `r`'s.
fn lines(
    r: &Reading,
    volumes: &[Volume],
    dir: Option<&Path>,
    claims: &Result<Vec<Record>, String>,
    trees: Option<&HashMap<u32, TreeUse>>,
    now: u64,
) -> Vec<String> {
    let mut out = vec![
        format!(
            "cores    {}, {} idle by the 5-minute load",
            r.cores,
            or_unknown(r.idle_cores(), |i| format!("{i:.1}"))
        ),
        format!(
            "load     {} (1 min), {} (5 min)",
            or_unknown(r.load1, |l| format!("{l:.2}")),
            or_unknown(r.load5, |l| format!("{l:.2}"))
        ),
        format!(
            "memory   {} available of {}{}",
            or_unknown(r.available, gib),
            or_unknown(r.physical, gib),
            match (r.wired, r.reachable()) {
                (Some(w), Some(reach)) => format!(
                    ", never more than {} ({} is wired): a claim of that or more is refused unless --admit now starts it anyway",
                    gib(reach),
                    gib(w)
                ),
                _ => String::new(),
            }
        ),
        format!(
            "swap     {} used of {}",
            or_unknown(r.swap_used, gib),
            or_unknown(r.swap_total, gib)
        ),
    ];
    out.push(
        r.volume
            .as_ref()
            .map_or_else(|| "disk     unknown".to_owned(), disk_line),
    );
    out.extend(volumes.iter().map(disk_line));
    let at = dir.map_or_else(|| "the ledger".to_owned(), |d| d.display().to_string());
    match claims {
        Err(e) => out.push(format!("claims   {at}: unreadable: {e}")),
        Ok(claims) => {
            let held = claims.iter().filter(|c| c.held.is_some()).count();
            out.push(format!(
                "claims   {at}: {held} held, {} waiting",
                claims.len() - held
            ));
            for (c, reserve) in claims.iter().zip(reserves(r, volumes, claims, trees)) {
                out.push(claim_line(c, reserve, trees, now));
            }
        }
    }
    out
}

/// What the processes `c` is credited with ([`Record::credit`]) use: `None` without a sweep.
fn credited(c: &Record, trees: Option<&HashMap<u32, TreeUse>>) -> Option<u64> {
    trees.map(|t| {
        c.credit
            .iter()
            .filter_map(|p| t.get(p))
            .map(|u| u.footprint)
            .sum()
    })
}

/// One claim's line, `reserve` what it reserves against the claims behind it, held or waiting:
/// its memory where it needs any, and its disk where it needs any, marked [`UNSERVABLE`] only
/// for a waiter ([`Reserve::unservable`] is a waiter's alone). Its holder's tree
/// ([`aterm_capacity::Procs::trees`]) is the same for every claim that process holds.
fn claim_line(
    c: &Record,
    reserve: Reserve,
    trees: Option<&HashMap<u32, TreeUse>>,
    now: u64,
) -> String {
    let mut parts = vec![format!(
        "  {:<8} pid {:<7} for {:<7}",
        if c.held.is_some() { "held" } else { "waiting" },
        c.pid,
        span(Duration::from_millis(now.saturating_sub(c.start_ms)))
    )];
    if c.need.mem > 0 {
        let using = trees.map(|t| t.get(&c.pid).map_or(0, |u| u.footprint));
        let vm = credited(c, trees).filter(|_| !c.credit.is_empty());
        parts.push(format!(
            "mem {}{}, reserves {}",
            gib(c.need.mem),
            match (using, vm) {
                (Some(u), Some(v)) => format!(
                    " (its holder's tree holds {}, the {} processes credited to it {})",
                    gib(u),
                    c.credit.len(),
                    gib(v)
                ),
                (Some(u), None) => format!(" (its holder's tree holds {})", gib(u)),
                (None, _) => String::new(),
            },
            gib(reserve.mem)
        ));
    }
    if c.need.cores > 0 {
        parts.push(match c.held {
            Some(h) => format!("cores {} of {}", h.cores, c.need.cores),
            None => format!("cores {}", c.need.cores),
        });
    }
    if c.need.disk > 0 {
        let mut disk = format!(
            "disk {}{}",
            gib(c.need.disk),
            c.need
                .path
                .as_deref()
                .map_or_else(String::new, |p| format!(" on {}", p.display()))
        );
        disk.push_str(&match reserve.disk {
            Some(left) if reserve.unservable && c.held.is_none() => format!(
                ", {UNSERVABLE}: more than is free above the floor, so it waits for space to be \
                 freed and reserves {} against the claims behind it",
                gib(left)
            ),
            Some(left) => format!(", reserves {} against the claims behind it", gib(left)),
            None => format!(", reserves unknown: {NOT_READ}"),
        });
        parts.push(disk);
    }
    if let Some(q) = c.need.quiet {
        parts.push(format!("quiet at load {q:.1}"));
    }
    if let Some(left) = c.reserves(now) {
        parts.push(format!(
            "reserves the machine: claimed work that asked after it waits, {} more at most",
            span(left)
        ));
    }
    parts.push(format!("\"{}\"", c.need.what));
    if !c.checkout.is_empty() {
        parts.push(format!("in {}", c.checkout));
    }
    parts.join("  ")
}

/// A volume as the JSON report holds it: `mount`, `total`, `free`, `floor` and `room`, the
/// figures its disk line says ([`disk_line`]).
fn volume_json(v: &Volume) -> Value {
    let mut m = Map::new();
    m.insert(
        "mount".to_owned(),
        Value::from(v.mount.display().to_string()),
    );
    m.insert("total".to_owned(), Value::from(v.total));
    m.insert("free".to_owned(), Value::from(v.free));
    m.insert("floor".to_owned(), Value::from(v.floor));
    m.insert("room".to_owned(), Value::from(v.room));
    Value::Object(m)
}

/// The report as one JSON object: every size in bytes. `volume` is `r`'s (the one holding
/// `<path>`), and `volumes` each of `volumes` (the other volumes claims' disk is on, each read
/// now: [`claim_volumes`]), in the same shape ([`volume_json`]). Each claim's `reserved_mem` and
/// `reserved_disk` are what it reserves against the claims behind it, held or waiting
/// ([`Reserve`]): its memory (`0` where it needs none), and its disk on its own volume (`r`'s or
/// one of `volumes`; `null` where it needs no disk or that volume could not be read); and
/// `unservable` whether it is a waiter whose disk need is more than that volume has free above
/// the floor now (`null` where it needs disk on a volume that could not be read, so it is not
/// judged).
fn json_of(
    r: &Reading,
    volumes: &[Volume],
    dir: Option<&Path>,
    claims: &Result<Vec<Record>, String>,
    trees: Option<&HashMap<u32, TreeUse>>,
    now: u64,
) -> String {
    let mut o = Map::new();
    o.insert("cores".to_owned(), Value::from(r.cores));
    o.insert("load1".to_owned(), Value::from(r.load1));
    o.insert("load5".to_owned(), Value::from(r.load5));
    o.insert("idle_cores".to_owned(), Value::from(r.idle_cores()));
    o.insert("available".to_owned(), Value::from(r.available));
    o.insert("physical".to_owned(), Value::from(r.physical));
    o.insert("wired".to_owned(), Value::from(r.wired));
    o.insert("reachable".to_owned(), Value::from(r.reachable()));
    o.insert("swap_used".to_owned(), Value::from(r.swap_used));
    o.insert("swap_total".to_owned(), Value::from(r.swap_total));
    o.insert(
        "volume".to_owned(),
        r.volume.as_ref().map_or(Value::Null, volume_json),
    );
    o.insert(
        "volumes".to_owned(),
        Value::Array(volumes.iter().map(volume_json).collect()),
    );
    o.insert(
        "ledger".to_owned(),
        Value::from(dir.map(|d| d.display().to_string())),
    );
    match claims {
        Err(e) => {
            o.insert("claims".to_owned(), Value::Null);
            o.insert("unreadable".to_owned(), Value::from(e.as_str()));
        }
        Ok(claims) => {
            let rows = claims
                .iter()
                .zip(reserves(r, volumes, claims, trees))
                .map(|(c, reserve)| {
                    let used = trees.map(|t| t.get(&c.pid).copied().unwrap_or_default());
                    let mut m = Map::new();
                    m.insert("pid".to_owned(), Value::from(c.pid));
                    m.insert(
                        "state".to_owned(),
                        Value::from(if c.held.is_some() { "held" } else { "waiting" }),
                    );
                    m.insert("start_ms".to_owned(), Value::from(c.start_ms));
                    m.insert(
                        "age_ms".to_owned(),
                        Value::from(now.saturating_sub(c.start_ms)),
                    );
                    m.insert("what".to_owned(), Value::from(c.need.what.as_str()));
                    m.insert("checkout".to_owned(), Value::from(c.checkout.as_str()));
                    m.insert("mem".to_owned(), Value::from(c.need.mem));
                    m.insert("cores".to_owned(), Value::from(c.need.cores));
                    m.insert("disk".to_owned(), Value::from(c.need.disk));
                    m.insert("reserved_mem".to_owned(), Value::from(reserve.mem));
                    m.insert("reserved_disk".to_owned(), Value::from(reserve.disk));
                    m.insert(
                        "unservable".to_owned(),
                        if reserve.disk.is_none() && c.need.disk > 0 {
                            Value::Null
                        } else {
                            Value::from(reserve.unservable)
                        },
                    );
                    m.insert("quiet".to_owned(), Value::from(c.need.quiet));
                    m.insert("next_ms".to_owned(), Value::from(c.next_ms));
                    m.insert(
                        "reserves_ms".to_owned(),
                        Value::from(
                            c.reserves(now)
                                .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX)),
                        ),
                    );
                    m.insert(
                        "volume".to_owned(),
                        Value::from(c.need.path.as_ref().map(|p| p.display().to_string())),
                    );
                    m.insert(
                        "granted_cores".to_owned(),
                        Value::from(c.held.map(|h| h.cores)),
                    );
                    m.insert(
                        "tree_held".to_owned(),
                        Value::from(used.map(|u| u.footprint)),
                    );
                    m.insert(
                        "credit".to_owned(),
                        Value::Array(c.credit.iter().map(|p| Value::from(*p)).collect()),
                    );
                    m.insert("credited_held".to_owned(), Value::from(credited(c, trees)));
                    Value::Object(m)
                })
                .collect::<Vec<_>>();
            o.insert("claims".to_owned(), Value::Array(rows));
        }
    }
    aterm_json::to_string(&Value::Object(o)).unwrap_or_else(|_| "{}".to_owned())
}

#[cfg(test)]
#[path = "capacity_tests.rs"]
mod tests;
