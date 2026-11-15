// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Snapshot census; never interprets a missing cause as evidence of a particular sender.
//! Its flaky list compares journal records with git objects and runs nothing.

use crate::judged::{self, Record, Row};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

fn cause(record: &Record) -> String {
    if !record.cause.is_empty() {
        return record.cause.clone();
    }
    let evidence = record
        .rows
        .iter()
        .filter(|row| row.outcome == "could-not-run")
        .map(|row| format!("{}: {}", row.id, row.log))
        .collect::<Vec<_>>()
        .join("\n");
    if evidence.is_empty() {
        "unknown (historical record has no cause)".into()
    } else {
        evidence
    }
}

fn summary(why: &str) -> String {
    if let Some(preemption) = why
        .lines()
        .find(|line| line.starts_with("capacity background "))
    {
        let condition =
            if let Some(sender) = preemption.strip_prefix("capacity background preemption: ") {
                sender
                    .split_once("; ")
                    .map_or(sender, |(_, condition)| condition)
            } else {
                preemption
                    .strip_prefix("capacity background stopped before admission: ")
                    .unwrap_or(preemption)
            };
        if condition.starts_with("ledger observation failed:") {
            return "capacity ledger observation failed".into();
        }
        if condition.starts_with("Release claim pid=") {
            return "Release claim preemption".into();
        }
        return preemption.into();
    }
    if let Some(stopped) = why.lines().find(|line| line.starts_with("watch ")) {
        if let Some((scope, detail)) = stopped.split_once(" stopped: ") {
            let kind = detail
                .split_once(" kind=")
                .and_then(|(_, tail)| tail.split_whitespace().next())
                .unwrap_or("stopped");
            return format!("{scope} {kind}");
        }
        return stopped.into();
    }
    if let Some(signal) = why.lines().rev().find_map(|line| {
        line.split_once("killed by signal ")
            .and_then(|(_, value)| value.split_whitespace().next())
    }) {
        return format!("child signal={signal}");
    }
    why.lines()
        .rev()
        .find(|line| line.contains("COULD NOT RUN"))
        .or_else(|| why.lines().next())
        .unwrap_or("unknown")
        .to_owned()
}

pub(super) fn run_args(root: &Path, args: &[String]) -> Result<(), String> {
    for line in report(root, args)? {
        println!("{line}");
    }
    Ok(())
}

/// What `verify watch --census` prints for `args`, one line each.
fn report(root: &Path, args: &[String]) -> Result<Vec<String>, String> {
    const USAGE: &str = "expected --census REF [--since UNIX_SECONDS] [--flaky]";
    let (head, flags) = args.split_first().ok_or(USAGE)?;
    let (mut since, mut flaky) = (None, false);
    let mut flags = flags.iter();
    while let Some(flag) = flags.next() {
        match flag.as_str() {
            "--since" => {
                since = Some(
                    flags
                        .next()
                        .and_then(|value| value.parse::<u64>().ok())
                        .ok_or("--since requires Unix seconds")?,
                );
            }
            "--flaky" => flaky = true,
            _ => return Err(USAGE.into()),
        }
    }
    // Resolve once so a moving local ref cannot mix snapshots.
    let head = judged::git(
        root,
        &["rev-parse", "--verify", &format!("{head}^{{commit}}")],
    )?;
    let records = match since {
        Some(since) => judged::history(root, &head, since)?,
        None => judged::records(root, Some(&head), judged::deadline())?,
    };
    if flaky {
        let (mut lines, red_cases, listed) = flaky_cases(root, &records);
        lines.push(format!(
            "journal={head} records={} red_cases={red_cases} flaky={listed} since={since:?}",
            records.len()
        ));
        return Ok(lines);
    }
    let mut lines = Vec::new();
    let mut states = BTreeMap::<String, usize>::new();
    let mut causes = BTreeMap::<String, usize>::new();
    for record in &records {
        if since.is_some() {
            lines.push(format!(
                "attempt: {}",
                aterm_json::to_string(record).map_err(|e| e.to_string())?
            ));
        }
        *states.entry(record.state.clone()).or_default() += 1;
        if matches!(record.state.as_str(), "cancelled" | "could-not-run") {
            let why = cause(record);
            // Prefer the tool's refusal over the timing prefix on historical stage rows.
            let summary = summary(&why);
            *causes
                .entry(format!("{}: {}", record.state, summary))
                .or_default() += 1;
            lines.push(format!(
                "tree={} commit={} state={} cause={why:?}",
                record.tree, record.commit, record.state
            ));
        }
    }
    let completed =
        states.get("pass").copied().unwrap_or(0) + states.get("red").copied().unwrap_or(0);
    lines.push(format!(
        "journal={head} records={} completed={completed} states={states:?} since={since:?}",
        records.len()
    ));
    let mut ranked: Vec<_> = causes.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    for (why, count) in ranked {
        lines.push(format!("count={count} cause={why:?}"));
    }
    Ok(lines)
}

/// A red row's item that names one test case, `-p <package> <target> -- <case>`, as
/// `findings::split_test_findings` words it. A row's other items name a stage or a check.
fn is_case(item: &str) -> bool {
    item.starts_with("-p ") && item.contains(" -- ")
}

/// The one-minute load a row's first line records, or `unknown` where it records none.
fn load1(row: &Row) -> String {
    row.log
        .lines()
        .next()
        .and_then(|line| {
            line.split_whitespace()
                .find_map(|word| word.strip_prefix("load1="))
        })
        .and_then(|value| value.strip_prefix("Some(")?.strip_suffix(')'))
        .and_then(|value| value.parse::<f64>().ok())
        .map_or_else(|| "unknown".into(), |load| format!("{load:.2}"))
}

/// A commit's workspace: what a closure in it is read from, and its git pins.
type Workspace = (super::Described, String);

/// The workspace `commit` holds, read from its objects, so it needs neither a checkout of the
/// commit nor a toolchain. Its packages are worded as `targo metadata --no-deps` words a
/// checkout's: each manifest's package under `/`, with the path dependencies its tables and the
/// root's workspace table name. Every manifest of the tree that names a package is one, a
/// workspace member or not, so a closure also follows the path dependencies of a vendored
/// crate, which `--no-deps` leaves out. A manifest below the root that does not parse is no
/// package of the graph.
///
/// # Errors
/// A commit this repository lacks, or one without a root manifest and a lock that parse: its
/// closures would leave out what those name.
fn workspace(root: &Path, commit: &str) -> Result<Workspace, String> {
    let listing = super::git(root, &["ls-tree", "-rz", "--full-tree", commit])?;
    let files: Vec<(&str, &str)> = listing
        .split('\0')
        .filter_map(|entry| entry.split_once('\t'))
        .filter(|(header, path)| {
            header.starts_with("100")
                && (matches!(*path, "Cargo.toml" | "Cargo.lock") || path.ends_with("/Cargo.toml"))
        })
        .filter_map(|(header, path)| Some((path, header.split(' ').nth(2)?)))
        .collect();
    let ids: String = files.iter().map(|(_, id)| format!("{id}\n")).collect();
    let batch = judged::output(
        &mut judged::command(root, &["cat-file", "--batch"]),
        Some(ids.as_bytes()),
    )?;
    let mut parsed: BTreeMap<&str, aterm_toml::Value> = files
        .iter()
        .zip(judged::blobs(batch.as_bytes())?)
        .filter_map(|((path, _), text)| Some((*path, aterm_toml::from_str(text).ok()?)))
        .collect();
    let lock = parsed
        .remove("Cargo.lock")
        .ok_or("it holds no Cargo.lock that parses")?;
    let manifest = parsed
        .get("Cargo.toml")
        .cloned()
        .ok_or("it holds no root manifest that parses")?;
    let named: BTreeMap<String, String> = crate::export_scan::dependency_rows(&manifest, false)
        .into_iter()
        .filter_map(|(name, path, _)| Some((name, path?)))
        .collect();
    let under_root = |dir: &str| format!("/{dir}");
    let packages: Vec<aterm_json::Value> = parsed
        .iter()
        .filter_map(|(path, manifest)| {
            let dir = path.strip_suffix("Cargo.toml")?.trim_end_matches('/');
            let name = manifest.get("package")?.get("name")?.as_str()?;
            let dependencies: Vec<aterm_json::Value> =
                crate::export_scan::dependency_rows(manifest, true)
                    .into_iter()
                    .filter_map(|(name, path, in_workspace)| {
                        let path = match (path, in_workspace) {
                            (Some(path), _) => crate::export_scan::join_lexically(dir, &path),
                            (None, true) => {
                                crate::export_scan::join_lexically("", named.get(&name)?)
                            }
                            // Named by version: the lock says what it resolves to.
                            (None, false) => return None,
                        };
                        Some(aterm_json::json!({"name": name, "path": under_root(&path)}))
                    })
                    .collect();
            Some(aterm_json::json!({
                "name": name,
                "manifest_path": under_root(&format!("{dir}/Cargo.toml")),
                "dependencies": dependencies,
            }))
        })
        .collect();
    let pins: Vec<&str> = listing
        .split('\0')
        .filter(|entry| entry.starts_with("160000 "))
        .collect();
    Ok((
        super::Described {
            metadata: aterm_json::json!({"packages": packages}),
            manifest,
            lock,
        },
        pins.join("\0"),
    ))
}

/// The workspace-wide files every case reads whole: the compiler a tree names, and the flags
/// and configuration every unit of it is built with. They are paths of every closure. Of the
/// rest of [`super::WORKSPACE_INPUTS`] a case reads what its closure is computed from (the
/// root manifest's dependency and patch rows, the lock's packages), so one of those that
/// differs is named beside a closure that is the same at both commits.
const TOOLCHAIN_INPUTS: [&str; 3] = ["trust.lock", "rust-toolchain.toml", ".cargo"];

/// What the test case `item` names reads of `workspace`: the crates it names, every package
/// the lock resolves below them, and the toolchain files.
///
/// # Errors
/// An item whose crate the workspace does not place.
fn closure(workspace: &Workspace, item: &str) -> Result<super::Closure, String> {
    let (described, pins) = workspace;
    let mut closure = super::item_closure(
        Path::new("/"),
        &super::named_crates(item, pins),
        true,
        pins,
        &|| Ok(described),
    )?;
    // No path to compare would agree with every commit.
    if closure.paths.is_empty() {
        return Err(format!("{item:?} names no crate"));
    }
    closure.paths.extend(TOOLCHAIN_INPUTS.map(str::to_owned));
    Ok(closure)
}

/// `paths` in rounds: in each, no path lies below another, and each path is in one.
fn rounds(paths: &BTreeSet<String>) -> Vec<Vec<&str>> {
    let mut rounds = Vec::new();
    let mut rest: Vec<&str> = paths.iter().map(String::as_str).collect();
    while !rest.is_empty() {
        let (below, round) = rest.iter().copied().partition(|path| {
            rest.iter()
                .any(|above| above != path && Path::new(path).starts_with(above))
        });
        rounds.push(round);
        rest = below;
    }
    rounds
}

/// What `commit` holds at each path of `rounds`, by path: the entry's mode, type and object id.
/// A path the commit lacks has no entry. Each round is one listing: a listing leaves out a
/// directory it descends into for a path below it, and two commits would then agree on a
/// directory neither answered for.
fn held(
    root: &Path,
    commit: &str,
    rounds: &[Vec<&str>],
) -> Result<BTreeMap<String, String>, String> {
    let mut held = BTreeMap::new();
    for round in rounds {
        let mut args = vec!["ls-tree", "-z", "--full-tree", commit, "--"];
        args.extend(round);
        held.extend(
            super::git(root, &args)?
                .split('\0')
                .filter_map(|entry| entry.split_once('\t'))
                .map(|(header, path)| (path.to_owned(), header.to_owned())),
        );
    }
    Ok(held)
}

/// One test case of a red row, with its closure at the red commit.
struct Case<'a> {
    item: &'a str,
    record: &'a Record,
    row: &'a Row,
    closure: super::Closure,
}

/// The lines of `--flaky`, with how many test cases the red rows of `records` name and how many
/// of them are listed. A case is listed where a record of another tree passed the same row and
/// the case's closure is the same at both commits: the same paths, held under the same object
/// ids, and the same registry packages. The two rows then read the same crates, built by the
/// same toolchain, and differ in outcome. Of several such records the line names the one whose
/// other workspace-wide files differ least, then the nearest in time. A commit whose objects
/// this repository lacks, or whose manifests and lock do not place a case's crate, is said, and
/// that case is not compared with it.
fn flaky_cases(root: &Path, records: &[Record]) -> (Vec<String>, usize, usize) {
    let mut cases = Vec::new();
    let mut unread = BTreeSet::<(&str, String)>::new();
    let mut red_cases = 0;
    for record in records {
        let items: Vec<(&Row, &str)> = record
            .rows
            .iter()
            .filter(|row| row.outcome == "red")
            .flat_map(|row| row.red_items.iter().map(move |item| (row, item.as_str())))
            .filter(|(_, item)| is_case(item))
            .collect();
        red_cases += items.len();
        if items.is_empty() {
            continue;
        }
        let workspace = match workspace(root, &record.commit) {
            Ok(workspace) => workspace,
            Err(why) => {
                unread.insert((&record.commit, why));
                continue;
            }
        };
        for (row, item) in items {
            match closure(&workspace, item) {
                Ok(closure) => cases.push(Case {
                    item,
                    record,
                    row,
                    closure,
                }),
                Err(why) => {
                    unread.insert((&record.commit, why));
                }
            }
        }
    }
    let mut compared: BTreeSet<String> = cases
        .iter()
        .flat_map(|case| case.closure.paths.iter().cloned())
        .chain(super::WORKSPACE_INPUTS.map(str::to_owned))
        .collect();
    compared.remove("");
    let rounds = rounds(&compared);
    let rows: BTreeSet<&str> = cases.iter().map(|case| case.row.id.as_str()).collect();
    let mut holds = BTreeMap::<&str, BTreeMap<String, String>>::new();
    for record in records {
        let read = cases.iter().any(|case| case.record.tree == record.tree)
            || record
                .rows
                .iter()
                .any(|row| row.outcome == "pass" && rows.contains(row.id.as_str()));
        if !read || holds.contains_key(record.commit.as_str()) {
            continue;
        }
        match held(root, &record.commit, &rounds) {
            Ok(mut entries) => {
                // A package at the root has the whole tree for its directory.
                entries.insert(String::new(), record.tree.clone());
                holds.insert(&record.commit, entries);
            }
            Err(why) => {
                unread.insert((&record.commit, why));
            }
        }
    }
    let mut flaky = Vec::new();
    // The workspace of the green commit read last, alone: it is every manifest of a tree, and a
    // red row's cases mostly share a witness.
    let mut last = BTreeMap::<&str, Result<Workspace, String>>::new();
    for case in &cases {
        let Some(red) = holds.get(case.record.commit.as_str()) else {
            continue;
        };
        let mut greens: Vec<_> = records
            .iter()
            .filter(|green| green.tree != case.record.tree)
            .filter_map(|green| {
                let row = green
                    .rows
                    .iter()
                    .find(|row| row.id == case.row.id && row.outcome == "pass")?;
                let entries = holds.get(green.commit.as_str())?;
                let same = |path: &str| red.get(path) == entries.get(path);
                case.closure.paths.iter().all(|path| same(path)).then(|| {
                    let changed: Vec<&str> = super::WORKSPACE_INPUTS
                        .into_iter()
                        .filter(|path| !same(path))
                        .collect();
                    (
                        changed.len(),
                        green.updated.abs_diff(case.record.updated),
                        green,
                        row,
                        changed,
                    )
                })
            })
            .collect();
        greens.sort_by(|a, b| (a.0, a.1, &a.2.commit).cmp(&(b.0, b.1, &b.2.commit)));
        // The green commit holds the red commit's paths alike. The closure is still another
        // one where the green commit's root manifest or lock gives the case other paths or
        // other registry packages.
        let witness = greens.into_iter().find(|(_, _, green, ..)| {
            let commit = green.commit.as_str();
            if !last.contains_key(commit) {
                last.clear();
            }
            let at_green = last
                .entry(commit)
                .or_insert_with(|| workspace(root, commit))
                .as_ref()
                .map_err(String::clone)
                .and_then(|workspace| closure(workspace, case.item));
            match at_green {
                Ok(at_green) => at_green == case.closure,
                Err(why) => {
                    unread.insert((commit, why));
                    false
                }
            }
        });
        if let Some((_, _, green, row, changed)) = witness {
            let changed = if changed.is_empty() {
                "none".into()
            } else {
                changed.join(",")
            };
            flaky.push(format!(
                "flaky item={:?} row={} red={} red_host={} red_load1={} green={} green_host={} \
                 green_load1={} config_changed={changed}",
                case.item,
                case.row.id,
                case.record.commit,
                case.record.host,
                load1(case.row),
                green.commit,
                green.host,
                load1(row),
            ));
        }
    }
    flaky.sort();
    let listed = flaky.len();
    flaky.extend(unread.into_iter().map(|(commit, why)| {
        format!(
            "unread commit={commit} why={:?}",
            why.lines().next().unwrap_or_default()
        )
    }));
    (flaky, red_cases, listed)
}

#[cfg(test)]
#[path = "watch_census_tests.rs"]
mod tests;
