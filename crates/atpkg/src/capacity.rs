// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm pkg capacity`: the machine's capacity now and every claim in this user's ledger
//! (`aterm_capacity`, `docs/DESIGN-adaptive-build-2026-10-24.md` CAP-1). Read-only: it admits
//! nothing and removes no dead holder's file. Every claim's line, held or waiting, says what it
//! reserves against the claims behind it, as their looks reckon it
//! ([`aterm_capacity::reserved`]): its memory (`mem <need>, reserves <bytes>`: its peak less what
//! its holder's tree and the processes credited to it hold in memory, `aterm_capacity::Proc`,
//! only where holder accounting permits that credit; whole and detached claims keep their
//! full peak)
//! and its disk on its own volume (`disk <need> on <mount>, reserves <bytes> against the claims
//! behind it`: its need less what it has written). A waiting claim's disk is marked
//! [`UNSERVABLE`] where its need is more than that volume has free above the floor now: only
//! space freed, or what a scratch claim held there may give back by its end, can make room for
//! it, so it holds back no claim behind it while it is so. After that claim ends the waiter
//! rechecks the actual space, and is refused if its relief leaves it short with no scratch claim
//! held there that could meet it (DISK-ADMIT,
//! [`aterm_capacity::Short::unservable`], [`aterm_capacity::Error::Unmet`]); a held claim is
//! never marked. What a tree holds is the figure its reservation counts, with its footprint whole
//! beside it where the pages the kernel compressed or swapped make that a tenth of a GiB or more
//! ([`aterm_capacity::overrun::whole`]), and a held claim whose holder's tree holds more than it
//! claimed says by how much ([`aterm_capacity::Overrun`]: `14.7 GiB past its 16 GiB claim`). Each
//! volume a claim's disk is on is read, besides the one holding `<path>`, and has a disk line of its own
//! (and, in `--json`, an entry of `volumes`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use aterm_capacity::overrun::whole;
use aterm_capacity::{
    Accounting, Ledger, Live, Machine as _, Overrun, Reading, Record, Reserve, Spared, TreeUse,
    Volume, gib, overruns, span,
};
use aterm_json::{Map, Value};

#[path = "capacity_cpu.rs"]
mod cpu;

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
    let cpu = cpu::Report::read(dir);
    if json {
        println!(
            "{}",
            json_of(&reading, &volumes, dir, &claims, trees.as_ref(), now, &cpu)
        );
    } else {
        for line in lines(&reading, &volumes, dir, &claims, trees.as_ref(), now, &cpu) {
            println!("{line}");
        }
    }
    if claims.is_err() || cpu.failed() {
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
        if let Some(v) = Live.volume(mount).filter(|v| v.mount == mount) {
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

/// How each claim stands as a waiter of memory that cannot start now ([`aterm_capacity::spared`],
/// CAP3): the most a claim behind it may take and go first without delaying its turn
/// ([`Spared::Within`]), or that no claim's end can start it, so it reserves no memory and the
/// claims behind it that fit go first ([`Spared::Stuck`]). `None` for each claim where no memory
/// was read.
fn spares(
    r: &Reading,
    claims: &[Record],
    trees: Option<&HashMap<u32, TreeUse>>,
) -> Vec<Option<Spared>> {
    match r.available.or(r.physical) {
        Some(available) => aterm_capacity::spared(claims, trees, available, r.reachable()),
        None => vec![None; claims.len()],
    }
}

/// The memory `reserve` holds the claims behind it to: none for a waiter no claim's end can
/// start ([`Spared::Stuck`]), whatever it asks.
fn reserved_mem(reserve: Reserve, spare: Option<Spared>) -> u64 {
    match spare {
        Some(Spared::Stuck) => 0,
        Some(Spared::Within(_)) | None => reserve.mem,
    }
}

/// What a claim's memory says of its phases ([`aterm_capacity::phase`], CAP3): what its holder
/// stepped it to, or what its run phase will claim once it runs what its build made.
fn phase_words(c: &Record) -> String {
    match (c.need.stepped, c.need.run_mem) {
        (Some(s), _) if s < c.need.mem => format!(", stepped to {} as its run began", gib(s)),
        (Some(_), Some(run)) => format!(", stepped back up as its run outgrew {}", gib(run)),
        (None, Some(run)) => format!(", {} once it runs what it built", gib(run)),
        _ => String::new(),
    }
}

/// The mark on a waiting claim's disk whose need is more than its volume has free above the
/// floor now ([`Reserve::unservable`]): it reserves no disk while it is so, and is refused if
/// its relief leaves it so with no scratch claim held there that could meet it.
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
    cpu: &cpu::Report,
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
    out.extend(cpu.lines());
    let at = dir.map_or_else(|| "the ledger".to_owned(), |d| d.display().to_string());
    match claims {
        Err(e) => out.push(format!("claims   {at}: unreadable: {e}")),
        Ok(claims) => {
            let held = claims.iter().filter(|c| c.held.is_some()).count();
            out.push(format!(
                "claims   {at}: {held} held, {} waiting",
                claims.len() - held
            ));
            let overruns = overruns(claims, trees);
            let spares = spares(r, claims, trees);
            for ((c, reserve), spare) in claims
                .iter()
                .zip(reserves(r, volumes, claims, trees))
                .zip(spares)
            {
                out.push(claim_line(
                    c,
                    reserve,
                    spare,
                    trees,
                    overrun(c, &overruns),
                    now,
                ));
            }
        }
    }
    out
}

/// What the processes `c` is credited with ([`Record::credit`]) use: `None` without a sweep.
/// Whole and detached accounting never attribute current numeric PIDs to their records.
fn credited(c: &Record, trees: Option<&HashMap<u32, TreeUse>>) -> Option<TreeUse> {
    if c.accounting != Accounting::Holder {
        return None;
    }
    trees.map(|t| {
        c.credit
            .iter()
            .filter_map(|p| t.get(p))
            .fold(TreeUse::default(), |sum, u| TreeUse {
                charged: sum.charged.saturating_add(u.charged),
                footprint: sum.footprint.saturating_add(u.footprint),
                written: sum.written.saturating_add(u.written),
            })
    })
}

/// What `c`'s holder holds past its held claims of memory ([`overruns`]), where `c` is one of
/// them: a waiting claim, one of no memory, or one whose accounting attributes no current
/// process to it has none of its own.
fn overrun<'o>(c: &Record, overruns: &'o HashMap<u32, Overrun>) -> Option<&'o Overrun> {
    overruns
        .get(&c.pid)
        .filter(|_| c.accounting == Accounting::Holder && c.held.is_some() && c.need.mem > 0)
}

/// What a tree holds, as a claim's line says it ([`whole`]).
fn holds(u: TreeUse) -> String {
    whole(u.footprint, u.charged)
}

/// Current accounting and lifetime policy, not a new observation of closure.
/// Holder accounting alone does not distinguish an ordinary claim from a live cohort
/// description, so its lifetime remains the original claim protocol's decision.
fn accounting(c: &Record) -> (&'static str, &'static str) {
    match c.accounting {
        Accounting::Holder => ("holder", "claim-protocol"),
        Accounting::Whole => ("whole", "cohort-closure"),
        Accounting::CohortOwned => ("cohort-owned", "cohort-closure"),
        Accounting::Detached => ("detached", "final-guard-close"),
    }
}

/// Only holder accounting can associate a current PID's tree with this record.
fn tree_use(c: &Record, trees: Option<&HashMap<u32, TreeUse>>) -> Option<TreeUse> {
    if c.accounting != Accounting::Holder {
        return None;
    }
    trees.map(|t| t.get(&c.pid).copied().unwrap_or_default())
}

/// One claim's line, `reserve` what it reserves against the claims behind it, held or waiting:
/// its memory where it needs any, and its disk where it needs any, marked [`UNSERVABLE`] only
/// for a waiter ([`Reserve::unservable`] is a waiter's alone). Only holder accounting uses
/// the current holder's tree ([`aterm_capacity::Procs::trees`]); the other policies retain an
/// original record PID without attributing a current process to it. A holder's tree is the same
/// for every claim that process holds, and so is `overrun`, what that tree holds past its held
/// claims of memory ([`overrun`]). A waiter of memory that cannot start now names what it
/// `spare`s, or that no claim's end can start it and the claims behind it that fit go first,
/// background work aside behind a release's claim ([`spares`]).
fn claim_line(
    c: &Record,
    reserve: Reserve,
    spare: Option<Spared>,
    trees: Option<&HashMap<u32, TreeUse>>,
    overrun: Option<&Overrun>,
    now: u64,
) -> String {
    let (accounting, lifetime) = accounting(c);
    let pid_role = if c.accounting == Accounting::Holder {
        "pid"
    } else {
        "original pid"
    };
    let mut parts = vec![format!(
        "  {:<8} {pid_role} {:<7} for {:<7}",
        if c.held.is_some() { "held" } else { "waiting" },
        c.pid,
        span(Duration::from_millis(now.saturating_sub(c.start_ms)))
    )];
    parts.push(format!("accounting {accounting}, lifetime {lifetime}"));
    if c.need.mem > 0 {
        let using = tree_use(c, trees);
        let vm = credited(c, trees).filter(|_| !c.credit.is_empty());
        let past = overrun.map_or_else(String::new, |o| format!(": {}", o.beyond()));
        parts.push(format!(
            "mem {}{}{}, reserves {}{}",
            gib(c.need.mem),
            phase_words(c),
            match (using, vm) {
                (Some(u), Some(v)) => format!(
                    " (its holder's tree holds {}, the {} processes credited to it {}{past})",
                    holds(u),
                    c.credit.len(),
                    holds(v)
                ),
                (Some(u), None) => format!(" (its holder's tree holds {}{past})", holds(u)),
                (None, _) => String::new(),
            },
            gib(reserved_mem(reserve, spare)),
            match spare {
                Some(Spared::Within(0)) =>
                    ": it cannot start yet, and no claim behind it goes first".to_owned(),
                Some(Spared::Within(s)) => format!(
                    ": it cannot start yet, and a claim behind it of {} or less may go first",
                    gib(s)
                ),
                Some(Spared::Stuck) => format!(
                    ": it cannot start yet and no claim's end can start it (it waits for memory \
                     no claim holds), so each claim behind it that fits goes first{}",
                    if c.need.rank == aterm_capacity::Rank::Release {
                        " (background work aside, which yields to a release's claim)"
                    } else {
                        ""
                    }
                ),
                None => String::new(),
            }
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
                ", {UNSERVABLE}: more than is free above the floor, so it is relieved and refused \
                 if that frees too little, and reserves {} against the claims behind it",
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
/// judged). `accounting` and `lifetime` identify the current reservation policy;
/// `pid_role` distinguishes a holder PID from an original record PID. `tree_held` and
/// `credited_held` are what its holder's tree and the processes credited to it hold as its
/// reservation counts them, `tree_whole` and `credited_whole` their footprints whole, compressed
/// and swapped pages included: each `null` without a process sweep or unless holder accounting
/// permits those observations. `past_claims` is what its holder holds past its held claims of
/// memory ([`Overrun::past`]; `null` for a waiting claim, one of no memory, one of other
/// accounting, or a holder less than a tenth of a GiB past them). `run_mem` is what its run
/// phase claims, `stepped` what its holder stepped it to, and `spares` what a waiter of memory
/// that cannot start now and keeps its place lets a claim behind it take and go first
/// ([`Spared::Within`]), each `null` where it has none (CAP3). `stuck` is whether it is a waiter
/// of memory no claim's end can start ([`Spared::Stuck`]): its `spares` is `null` and its
/// `reserved_mem` `0`, since each claim behind it that fits goes first; `null` where no memory
/// was read.
fn json_of(
    r: &Reading,
    volumes: &[Volume],
    dir: Option<&Path>,
    claims: &Result<Vec<Record>, String>,
    trees: Option<&HashMap<u32, TreeUse>>,
    now: u64,
    cpu: &cpu::Report,
) -> String {
    let mut o = Map::new();
    o.insert("cores".to_owned(), Value::from(r.cores));
    o.insert("build_cpu".to_owned(), cpu.json());
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
            let overruns = overruns(claims, trees);
            let spares = spares(r, claims, trees);
            let memory_read = r.available.or(r.physical).is_some();
            let rows = claims
                .iter()
                .zip(reserves(r, volumes, claims, trees))
                .zip(spares)
                .map(|((c, reserve), spare)| {
                    let used = tree_use(c, trees);
                    let (accounting, lifetime) = accounting(c);
                    let mut m = Map::new();
                    m.insert("pid".to_owned(), Value::from(c.pid));
                    m.insert(
                        "pid_role".to_owned(),
                        Value::from(if c.accounting == Accounting::Holder {
                            "holder"
                        } else {
                            "original-record"
                        }),
                    );
                    m.insert("accounting".to_owned(), Value::from(accounting));
                    m.insert("lifetime".to_owned(), Value::from(lifetime));
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
                    m.insert(
                        "reserved_mem".to_owned(),
                        Value::from(reserved_mem(reserve, spare)),
                    );
                    m.insert("run_mem".to_owned(), Value::from(c.need.run_mem));
                    m.insert("stepped".to_owned(), Value::from(c.need.stepped));
                    m.insert(
                        "spares".to_owned(),
                        Value::from(match spare {
                            Some(Spared::Within(most)) => Some(most),
                            Some(Spared::Stuck) | None => None,
                        }),
                    );
                    m.insert(
                        "stuck".to_owned(),
                        Value::from(memory_read.then_some(spare == Some(Spared::Stuck))),
                    );
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
                    m.insert("tree_held".to_owned(), Value::from(used.map(|u| u.charged)));
                    m.insert(
                        "tree_whole".to_owned(),
                        Value::from(used.map(|u| u.footprint)),
                    );
                    m.insert(
                        "credit".to_owned(),
                        Value::Array(c.credit.iter().map(|p| Value::from(*p)).collect()),
                    );
                    let vm = credited(c, trees);
                    m.insert(
                        "credited_held".to_owned(),
                        Value::from(vm.map(|u| u.charged)),
                    );
                    m.insert(
                        "credited_whole".to_owned(),
                        Value::from(vm.map(|u| u.footprint)),
                    );
                    m.insert(
                        "past_claims".to_owned(),
                        Value::from(overrun(c, &overruns).map(Overrun::past)),
                    );
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
