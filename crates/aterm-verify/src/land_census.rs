// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `verify land --census [--since T]` (`docs/DESIGN-judged-main-release-pump-2026-11-11.md`
//! §3.1, JM-01): what the run records of a window say about landing, read from the journal's
//! `land/runs/` ([`crate::judged::run_records`]) and this clone's spool, with the commits main
//! moved to as this clone last fetched it. It prints and decides nothing: no landing, check or
//! exit reads it.
//!
//! - **runs** in the window (each run counted once, by its id, whatever images it ran as, from
//!   the hosts that ran them), and the records that do not read;
//! - **r**, the share of first judgments that went red, of those that judged (pass or fail),
//!   and how many did not run;
//! - the share of runs that **lost a race**: main's tip cut a judgment, doomed one that passed,
//!   or kept moving until the push stopped ([`crate::land::STOPPED`]);
//! - the runs that **ended without landing**, by exit; an interrupted run by its signal, and a
//!   run with no end record (still running, killed outright, or held past its report grace by a
//!   step the interrupt does not cut short);
//! - **minutes to main**, p50 and p95 (nearest rank over seconds, printed to a tenth of a
//!   minute), from a landed run's start to its end record;
//! - **landings** by kind, and how many resumed a judgment an earlier run kept;
//! - **re-judges** (judgments not the run's first) and **rounds** by cause, with their results;
//! - **aborts** by word;
//! - **main tips with no receipt**: of the commits main is known to have moved to in the window
//!   ([`mainline`]), those whose trailers hold no landing receipt, the first few named. The
//!   trailers alone say so, whatever the journal holds;
//! - **receipted landings with no run record**: of the movements that hold a receipt and were
//!   made at or after the start of the earliest run record read, those no landing's end record
//!   names. A landing made before that start has its record, if any, where this census did not
//!   read, so it is not counted.

use crate::exit;
use crate::land_record::Origin;
use crate::land_run::{Entry, Kind, Record};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// The `land` flag that prints the census.
pub const FLAG: &str = "--census";
/// `verify land --census`'s usage.
pub const USAGE: &str = "usage: targo --unverified verify land --census [--since \
                         <unix-seconds|<n>d|<n>h|YYYY-MM-DD>]";
/// The window a census reads where `--since` names none: the run records' retention
/// ([`crate::judged::RUN_RETENTION`]).
pub const WINDOW: u64 = crate::judged::RUN_RETENTION;
/// How many commits a census names on a line; the rest are counted.
const NAMED: usize = 10;

/// A commit as [`census_cli`]'s `git log` lists it for [`mainline`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
    pub commit: String,
    pub parents: Vec<String>,
    /// Its committer time, in Unix seconds.
    pub at: u64,
    /// Whether its trailers hold a landing receipt (`Aterm-Checked` or `Aterm-Rechecked`).
    pub landing: bool,
}

/// The commits `listed` (made from `since` on) shows main moved to, in its order: `tip`, every
/// landing commit (a landing is only ever pushed to main), and each landing's second parent, the
/// main its push saw ([`crate::land`]'s publish makes every landing so). Nothing else is known to
/// have been main: a commit that is not a landing may have been pushed with a branch under it (a
/// merge of a person's branch, pushed as it stands), so no parent of one is followed, and a run
/// of direct pushes between two landings shows as its last. Each is given as listed, with its
/// time and whether it holds a receipt.
#[must_use]
pub fn mainline(tip: &str, listed: &[Listed], since: u64) -> Vec<Listed> {
    let made = |commit: &&Listed| commit.at >= since;
    let mut main: BTreeSet<&str> = BTreeSet::from([tip]);
    for commit in listed.iter().filter(made).filter(|commit| commit.landing) {
        main.insert(&commit.commit);
        if let Some(onto) = commit.parents.get(1) {
            main.insert(onto);
        }
    }
    listed
        .iter()
        .filter(made)
        .filter(|commit| main.contains(commit.commit.as_str()))
        .cloned()
        .collect()
}

/// ` (a, b, and n more)` for the commits a line names, at most [`NAMED`] of them; nothing for
/// none.
fn named(commits: &[&str]) -> String {
    if commits.is_empty() {
        return String::new();
    }
    let mut names = commits[..commits.len().min(NAMED)].join(", ");
    if commits.len() > NAMED {
        names.push_str(&format!(", and {} more", commits.len() - NAMED));
    }
    format!(" ({names})")
}

/// `git log`'s lines as [`census_cli`] asks for them (`%H %P`, `%ct` and the trailers, unit
/// separated, each commit ended by a record separator), as [`Listed`] commits; a commit that
/// does not read is left out.
#[must_use]
pub fn listed(log: &str) -> Vec<Listed> {
    log.split('\u{1e}')
        .filter_map(|record| {
            let mut fields = record.trim_start_matches('\n').split('\u{1f}');
            let ids = fields.next()?;
            let at = fields.next()?.trim().parse().ok()?;
            let trailers = fields.next().unwrap_or("");
            let mut ids = ids.split_whitespace();
            let commit = ids.next()?.to_owned();
            Some(Listed {
                commit,
                parents: ids.map(str::to_owned).collect(),
                at,
                landing: trailers.lines().any(|line| {
                    line.starts_with("Aterm-Checked:") || line.starts_with("Aterm-Rechecked:")
                }),
            })
        })
        .collect()
}

/// One run as its records tell it.
#[derive(Debug, Default)]
struct Told<'a> {
    host: &'a str,
    started: u64,
    records: Vec<&'a Record>,
}

impl Told<'_> {
    fn end(&self) -> Option<&Record> {
        self.records
            .iter()
            .copied()
            .find(|r| r.kind == Kind::End.word())
    }
}

/// `part` of `whole` as `part/whole = p%`, or `0/0` where there is no whole.
fn share(part: usize, whole: usize) -> String {
    if whole == 0 {
        return "0/0".to_owned();
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "counts of runs and judgments, far below 2^52"
    )]
    let percent = part as f64 * 100.0 / whole as f64;
    format!("{part}/{whole} = {percent:.1}%")
}

/// `secs` in minutes, to a tenth of one.
fn minutes(secs: u64) -> String {
    #[expect(
        clippy::cast_precision_loss,
        reason = "a run's seconds, far below 2^52"
    )]
    let minutes = secs as f64 / 60.0;
    format!("{minutes:.1}")
}

/// The nearest-rank `p`th percentile of sorted `values`.
fn percentile(values: &[u64], p: usize) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    let rank = (p * values.len()).div_ceil(100).max(1);
    values.get(rank - 1).copied()
}

/// Counts by word, each with its results: `word n (result m, …)`, words in order.
fn by_cause(steps: &[&Record]) -> String {
    let mut words: BTreeMap<&str, BTreeMap<&str, usize>> = BTreeMap::new();
    for step in steps {
        let word = step.cause.as_ref().map_or("-", |cause| cause.word.as_str());
        *words
            .entry(word)
            .or_default()
            .entry(step.result.as_str())
            .or_default() += 1;
    }
    if words.is_empty() {
        return "none".to_owned();
    }
    words
        .iter()
        .map(|(word, results)| {
            let total: usize = results.values().sum();
            let results: Vec<String> = results.iter().map(|(r, n)| format!("{r} {n}")).collect();
            format!("{word} {total} ({})", results.join(", "))
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// The census of the runs `entries` hold that started at `since` or later, and of `movements`,
/// the commits main moved to in the window ([`mainline`]): the lines [`census_cli`] prints.
/// A movement's receipt is read from its trailers; whether a receipted one has a run record is
/// asked only of those made at or after the earliest run record among `entries`.
/// An entry at a path two origins both hold is read once.
#[must_use]
pub fn census(entries: &[Entry], movements: &[Listed], since: u64) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut unread = 0;
    // The start of the earliest run record read, in the window or before it.
    let mut earliest: Option<u64> = None;
    let mut runs: BTreeMap<&str, Told<'_>> = BTreeMap::new();
    let mut landed_commits = BTreeSet::new();
    for entry in entries {
        if !seen.insert(entry.path.as_str()) {
            continue;
        }
        let Ok(read) = &entry.body else {
            unread += 1;
            continue;
        };
        let record = &read.record;
        if let Some(landing) = &record.landing {
            landed_commits.insert(landing.commit.as_str());
        }
        earliest = Some(earliest.map_or(record.run.started, |at| at.min(record.run.started)));
        if record.run.started < since {
            continue;
        }
        let told = runs.entry(record.run.id.as_str()).or_default();
        told.host = &record.run.host;
        told.started = record.run.started;
        told.records.push(record);
    }
    for told in runs.values_mut() {
        told.records.sort_by_key(|r| r.seq);
    }
    let of = |kind: Kind| -> Vec<&Record> {
        runs.values()
            .flat_map(|told| told.records.iter().copied())
            .filter(|r| r.kind == kind.word())
            .collect()
    };
    let judgments = of(Kind::Judgment);
    let first: Vec<&Record> = judgments
        .iter()
        .copied()
        .filter(|r| r.cause.as_ref().is_none_or(|c| c.word == "first"))
        .collect();
    let rejudges: Vec<&Record> = judgments
        .iter()
        .copied()
        .filter(|r| r.cause.as_ref().is_some_and(|c| c.word != "first"))
        .collect();
    // r is over the first judgments that judged: one that did not run says nothing of red.
    let red = first.iter().filter(|r| r.result == "fail").count();
    let judged = first
        .iter()
        .filter(|r| r.result == "fail" || r.result == "pass")
        .count();
    let not_run = first.len() - judged;
    let aborts = of(Kind::Abort);
    let raced = runs
        .values()
        .filter(|told| {
            told.records.iter().any(|r| r.kind == Kind::Abort.word())
                || told
                    .end()
                    .is_some_and(|end| end.exit == Some(crate::land::STOPPED))
        })
        .count();
    let mut unlanded: BTreeMap<String, usize> = BTreeMap::new();
    let mut seconds = Vec::new();
    let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
    let mut resumed = 0;
    for told in runs.values() {
        let word = match told.end() {
            None => "no end record".to_owned(),
            Some(end) => match (&end.landing, end.signal, end.exit) {
                (Some(landing), _, _) => {
                    seconds.push(end.secs);
                    *kinds.entry(landing.kind.as_str()).or_default() += 1;
                    resumed += usize::from(landing.resumed);
                    continue;
                }
                (None, Some(signal), _) => format!("signal {signal}"),
                (None, None, Some(exit::PASS)) => continue,
                (None, None, Some(code)) => format!("exit {code}"),
                (None, None, None) => "no exit".to_owned(),
            },
        };
        *unlanded.entry(word).or_default() += 1;
    }
    seconds.sort_unstable();
    let hosts: BTreeSet<&str> = runs.values().map(|told| told.host).collect();
    let unreceipted: Vec<&str> = movements
        .iter()
        .filter(|commit| !commit.landing)
        .map(|commit| commit.commit.as_str())
        .collect();
    let receipted: Vec<&str> = movements
        .iter()
        .filter(|commit| commit.landing && earliest.is_some_and(|at| commit.at >= at))
        .map(|commit| commit.commit.as_str())
        .collect();
    let unrecorded: Vec<&str> = receipted
        .iter()
        .copied()
        .filter(|commit| !landed_commits.contains(commit))
        .collect();
    let counted = |counts: &BTreeMap<&str, usize>| {
        if counts.is_empty() {
            return "none".to_owned();
        }
        counts
            .iter()
            .map(|(word, n)| format!("{word} {n}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let aborted: BTreeMap<&str, usize> = aborts.iter().fold(BTreeMap::new(), |mut counts, r| {
        *counts.entry(r.result.as_str()).or_default() += 1;
        counts
    });
    let mut lines = vec![
        format!(
            "verify land census since {} ({since}): {} runs from {} hosts; {unread} records do \
             not read",
            aterm_types::rfc3339::format_rfc3339(since),
            runs.len(),
            hosts.len()
        ),
        format!(
            "  r (first judgments red): {}; {not_run} did not run",
            share(red, judged)
        ),
        format!("  runs that lost a race: {}", share(raced, runs.len())),
        format!(
            "  runs that ended without landing: {} ({})",
            unlanded.values().sum::<usize>(),
            if unlanded.is_empty() {
                "none".to_owned()
            } else {
                unlanded
                    .iter()
                    .map(|(word, n)| format!("{word}: {n}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        ),
        match (percentile(&seconds, 50), percentile(&seconds, 95)) {
            (Some(p50), Some(p95)) => format!(
                "  minutes to main: p50 {}, p95 {} (over {} landed runs)",
                minutes(p50),
                minutes(p95),
                seconds.len()
            ),
            _ => "  minutes to main: no landed run".to_owned(),
        },
        format!(
            "  landings by kind: {} ({resumed} resumed a kept judgment)",
            counted(&kinds)
        ),
        format!("  re-judges by cause: {}", by_cause(&rejudges)),
        format!("  rounds by cause: {}", by_cause(&of(Kind::Round))),
        format!(
            "  composes: {}; aborts: {}",
            of(Kind::Compose).len(),
            counted(&aborted)
        ),
    ];
    lines.push(format!(
        "  main tips with no receipt: {} of {}{}",
        unreceipted.len(),
        movements.len(),
        named(&unreceipted)
    ));
    lines.push(match earliest {
        Some(at) => format!(
            "  receipted landings with no run record: {} of {} since {} (the earliest run record \
             read){}",
            unrecorded.len(),
            receipted.len(),
            aterm_types::rfc3339::format_rfc3339(at),
            named(&unrecorded)
        ),
        None => "  receipted landings with no run record: none counted (no run record was read)"
            .to_owned(),
    });
    lines
}

/// The run records `pending` (a clone's spool) holds, each held to [`Entry::read`]'s checks, and
/// each spool file named as a run record that could not be read at all ([`Entry::unread`]), so
/// the census counts it among the records that do not read.
#[must_use]
pub fn spooled(pending: &crate::land_spool::Pending) -> Vec<Entry> {
    let run = |path: &str| path.starts_with(crate::land_run::DIR);
    let mut entries: Vec<Entry> = pending
        .records
        .iter()
        .filter(|signed| run(&signed.path))
        .map(|signed| Entry::read(&signed.path, &signed.bytes, Origin::Spool))
        .collect();
    for (file, why) in &pending.unreadable {
        if let Some(path) = file
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(crate::land_spool::path_of)
            .filter(|path| run(path))
        {
            entries.push(Entry::unread(&path, Origin::Spool, why.clone()));
        }
    }
    entries
}

/// The window's start `word` names at `now`: Unix seconds, `<n>d` or `<n>h` before now, or a
/// UTC day `YYYY-MM-DD`; `None` for anything else.
#[must_use]
pub fn since_of(word: &str, now: u64) -> Option<u64> {
    if let Some(days) = word.strip_suffix('d') {
        return Some(now.saturating_sub(days.parse::<u64>().ok()?.checked_mul(86_400)?));
    }
    if let Some(hours) = word.strip_suffix('h') {
        return Some(now.saturating_sub(hours.parse::<u64>().ok()?.checked_mul(3_600)?));
    }
    if word.bytes().all(|b| b.is_ascii_digit()) {
        return word.parse().ok();
    }
    aterm_types::rfc3339::parse_utc(&format!("{word}T00:00:00Z"))
        .and_then(|secs| u64::try_from(secs).ok())
}

/// `verify land --census [--since T]` from `cwd`'s checkout: the run records of the journal on
/// [`crate::land::REMOTE`] and this clone's spool from `T` on (the last [`WINDOW`] by default),
/// and main as this clone last fetched it, printed ([`census`]). Exits 0, 2 for words it does
/// not take, 3 where the checkout or the journal was not read (what the spool holds is printed
/// even then).
pub fn census_cli(args: &[String], cwd: &Path) -> i32 {
    let now = aterm_time::unix_secs().unwrap_or(0);
    let since = match args {
        [] => now.saturating_sub(WINDOW),
        [help] if help == "--help" || help == "-h" => {
            println!("{USAGE}");
            return exit::PASS;
        }
        [flag, word] if flag == "--since" => match since_of(word, now) {
            Some(since) => since,
            None => {
                eprintln!("verify land --census: `{word}` names no time\n{USAGE}");
                return exit::USAGE;
            }
        },
        _ => {
            eprintln!("{USAGE}");
            return exit::USAGE;
        }
    };
    let Some(root) = crate::checkout_root(cwd) else {
        eprintln!(
            "verify land --census: COULD NOT RUN — {} is not inside a git checkout",
            cwd.display()
        );
        return exit::COULD_NOT_RUN;
    };
    let until = crate::judged::deadline();
    let journal = crate::judged::head(&root, crate::land::REMOTE, until)
        .and_then(|head| crate::judged::run_records(&root, head.as_deref(), since / 86_400, until));
    let mut entries = journal.as_ref().map_or_else(|_| Vec::new(), Clone::clone);
    match crate::land_spool::Dir::of_clone(&root).and_then(|dir| {
        use crate::land_spool::Spool as _;
        dir.pending()
    }) {
        Ok(pending) => entries.extend(spooled(&pending)),
        Err(why) => eprintln!("verify land --census: this clone's spool was not read: {why}"),
    }
    let main = format!("refs/remotes/{}/{}", crate::land::REMOTE, crate::land::MAIN);
    let movements = crate::judged::git(&root, &["rev-parse", "--verify", "--quiet", &main])
        .and_then(|tip| {
            let log = crate::judged::git(
                &root,
                &[
                    "log",
                    &format!("--max-age={since}"),
                    "--format=%H %P%x1f%ct%x1f%(trailers:only,unfold)%x1e",
                    &tip,
                    "--",
                ],
            )?;
            Ok(mainline(&tip, &listed(&log), since))
        });
    let movements = match movements {
        Ok(movements) => movements,
        Err(why) => {
            eprintln!("verify land --census: {main} was not read ({why}): no movement is counted");
            Vec::new()
        }
    };
    for line in census(&entries, &movements, since) {
        println!("{line}");
    }
    match journal {
        Ok(_) => exit::PASS,
        Err(why) => {
            eprintln!(
                "verify land --census: COULD NOT RUN — the journal was not read ({why}); only \
                 this clone's spool is counted"
            );
            exit::COULD_NOT_RUN
        }
    }
}

#[cfg(test)]
#[path = "land_census_tests.rs"]
mod tests;
