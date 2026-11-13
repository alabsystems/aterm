// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The release's early judgment of commits without a landing receipt. Receipts describe a
//! delta from a base, not every ancestor of their tree: main imported at the base stays owed.

use crate::release_input_facts::Facts;
use crate::{Ctx, EnvSnapshot, Show, checks::Check};
use aterm_types::land_trailer::{Checked, Stated};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::path::Path;

/// Early source readers. Qualification lints the whole release tree before publication;
/// running its compiler passes here too only repeats that work.
pub const CHECKS: [Check; 3] = [Check::ExportScan, Check::Fmt, Check::Census];
const NAMES: [&str; 3] = ["export-scan", "fmt", "census"];

/// The CLI's usage line.
const USAGE_LINE: &str = "usage: verify release-input --base <last-release-commit> [--checks NAMES] \
                     [--events PATH] [--carry-context CONTEXT] [--passed-input-items PATH]";

/// Why `unknown`, check names asked of release-input, are not run here: the readers it runs, and
/// where lint runs instead.
fn not_readers(unknown: &[&str]) -> String {
    let named = unknown
        .iter()
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>()
        .join(", ");
    let verb = if unknown.len() == 1 { "is" } else { "are" };
    let mut why = format!(
        "{named} {verb} not among release-input's source readers ({})",
        NAMES.join(", ")
    );
    if unknown.contains(&Check::Lint.name()) {
        why.push_str(
            "; lint belongs to qualification: `verify release --tree` runs it with the tree's \
             other checks, `verify release --tree --checks lint` alone",
        );
    }
    why
}

/// The readers a `--checks` list names, in [`CHECKS`]' order, or why it names another or none.
fn checks_named(list: &str) -> Result<Vec<Check>, String> {
    if list.is_empty() {
        return Err("the list names no check".into());
    }
    let names: Vec<&str> = list.split(',').collect();
    if names.contains(&"") {
        return Err(format!("`{list}` has an empty name"));
    }
    let unknown: Vec<&str> = names
        .iter()
        .copied()
        .filter(|name| !NAMES.contains(name))
        .collect();
    if !unknown.is_empty() {
        return Err(not_readers(&unknown));
    }
    Ok(CHECKS
        .into_iter()
        .filter(|check| names.contains(&check.name()))
        .collect())
}

#[derive(Debug)]
struct Commit {
    tree: String,
    parents: Vec<String>,
    receipt: Option<Stated>,
}

/// The input still owed a source judgment, measured against the last released commit.
#[derive(Debug)]
pub struct Input {
    pub commits: Vec<String>,
    pub paths: Vec<String>,
}

impl Input {
    /// An empty cone needs neither a toolchain seal nor a source check.
    pub fn no_work_note(&self) -> Option<String> {
        self.paths.is_empty().then(|| {
            format!(
                "release input: {} unreceipted commits, no unchecked paths; 0 checks run",
                self.commits.len(),
            )
        })
    }
}

fn git(root: &Path, path: &OsStr, args: &[&str]) -> Result<String, String> {
    let mut command = crate::git_provider::command();
    command
        .current_dir(root)
        .env("PATH", path)
        .arg("--no-optional-locks")
        .args(args)
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_NO_LAZY_FETCH", "1");
    for key in crate::changed::GIT_REDIRECTS {
        command.env_remove(key);
    }
    let out = aterm_uds::deadline::output_until_complete_stdout_with_limit(
        &mut command,
        std::time::Instant::now() + std::time::Duration::from_secs(60),
        aterm_uds::deadline::MAX_COMPLETE_STDOUT_BYTES,
    )
    .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    if !out.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    String::from_utf8(out.stdout).map_err(|e| e.to_string())
}

/// A Git read whose arguments name objects by ID only (`objects`), so its answer never
/// changes: asked once, then answered from `facts`. Its question is the whole argument list.
fn git_fact(
    facts: &Facts,
    root: &Path,
    path: &OsStr,
    args: &[&str],
    objects: &[&str],
) -> Result<String, String> {
    let mut question: Vec<&[u8]> = vec![b"git"];
    question.extend(args.iter().map(|arg| arg.as_bytes()));
    facts.answer(&question, objects, || git(root, path, args))
}

/// The source text that decides whether a composed receipt's merge holds
/// ([`crate::land::receipt_composition`]): a kept answer is one this text gave.
const COMPOSITION_RULE: &[&str] = &[include_str!("land.rs"), include_str!("land_version.rs")];

/// Every judgment a receipt states that read the source readers' input, in order: each full
/// judgment, and each recheck (`Aterm-Rechecked`) as the judgment of its merge over the cone
/// since the main it merged, so it credits the branch's delta from that main and leaves main's
/// own commits owed. The last is the landing's last judgment: the last recheck where one
/// extends the last full judgment ([`Stated::rechecks_since_judged`]).
fn stated_judgments(receipt: &Stated) -> (Vec<Checked>, Option<Checked>) {
    let rechecked = receipt.rechecked.iter().map(|recheck| Checked {
        tree: recheck.tree.clone(),
        rows: recheck.rows.clone(),
        base: recheck.base.clone(),
        secs: recheck.secs,
    });
    let all: Vec<Checked> = receipt.checked.iter().cloned().chain(rechecked).collect();
    let last = match receipt.rechecks_since_judged().last() {
        Some(recheck) => all
            .iter()
            .rev()
            .find(|judged| judged.tree == recheck.tree && judged.base == recheck.base)
            .cloned(),
        None => receipt.checked.last().cloned(),
    };
    (all, last)
}

fn ancestors(nodes: &BTreeMap<String, Commit>, start: &str) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut pending = vec![start.to_string()];
    while let Some(id) = pending.pop() {
        if seen.insert(id.clone())
            && let Some(commit) = nodes.get(&id)
        {
            pending.extend(commit.parents.iter().cloned());
        }
    }
    seen
}

struct Coverage {
    commits: BTreeSet<String>,
    judgments: Vec<Checked>,
}

fn coverage(
    facts: &Facts,
    root: &Path,
    path: &OsStr,
    nodes: &BTreeMap<String, Commit>,
    composed: &BTreeSet<String>,
) -> Coverage {
    let mut covered = BTreeSet::new();
    let mut judgments = Vec::new();
    for (landing, commit) in nodes {
        let Some(receipt) = &commit.receipt else {
            continue;
        };
        if commit.parents.len() != 2 {
            continue;
        }
        // Bind the last judgment to the actual landing before trusting any part of its
        // receipt. Merely finding a named tree somewhere in the imported main is not proof.
        let (all, last) = stated_judgments(receipt);
        if !last.as_ref().is_some_and(|c| c.tree == commit.tree) && !composed.contains(landing) {
            continue;
        }
        let mut spine = BTreeSet::new();
        let mut next = Some(landing.as_str());
        while let Some(id) = next {
            if !spine.insert(id.to_owned()) {
                break;
            }
            next = nodes
                .get(id)
                .and_then(|c| c.parents.first())
                .map(String::as_str);
        }
        let mut valid = false;
        for checked in &all {
            if !NAMES.iter().all(|name| {
                checked
                    .rows
                    .iter()
                    .any(|r| r.check == *name && r.word == "ok")
            }) {
                continue;
            }
            // A restated merge has the checked tree itself; a composed merge has the
            // checked tree on its first-parent history and names its imported main.
            let target = if checked.tree == commit.tree {
                Some(landing.as_str())
            } else {
                nodes.iter().find_map(|(id, c)| {
                    (c.tree == checked.tree && spine.contains(id)).then_some(id.as_str())
                })
            };
            let Some(target) = target else { continue };
            let judged = ancestors(nodes, target);
            if !judged.contains(&checked.base)
                && (nodes.contains_key(&checked.base)
                    || git_fact(
                        facts,
                        root,
                        path,
                        &["merge-base", "--is-ancestor", &checked.base, target],
                        &[&checked.base, target],
                    )
                    .is_err())
            {
                continue;
            }
            covered.extend(judged.difference(&ancestors(nodes, &checked.base)).cloned());
            judgments.push(checked.clone());
            valid |= last.as_ref() == Some(checked);
        }
        if valid {
            covered.insert(landing.clone());
        }
    }
    Coverage {
        commits: covered,
        judgments,
    }
}

/// A later landing can judge an earlier unchecked file again. Credit only paths in its
/// actual judged delta whose complete Git entry (content, mode and presence) still agrees
/// with the candidate. A path name alone cannot authorize an edit after that judgment.
fn remaining_paths(
    facts: &Facts,
    root: &Path,
    path: &OsStr,
    tip: &str,
    mut paths: BTreeSet<String>,
    judgments: &[Checked],
) -> Result<BTreeSet<String>, String> {
    let diff = |a: &str, b: &str| -> Result<BTreeSet<String>, String> {
        Ok(git_fact(
            facts,
            root,
            path,
            &[
                "diff",
                "--name-only",
                "--no-renames",
                "--no-ext-diff",
                "--no-textconv",
                "--ignore-submodules=none",
                "-z",
                a,
                b,
                "--",
            ],
            &[a, b],
        )?
        .split('\0')
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .collect())
    };
    for checked in judgments {
        if paths.is_empty() {
            break;
        }
        let judged = diff(&checked.base, &checked.tree)?;
        if judged.is_disjoint(&paths) {
            continue;
        }
        let changed = diff(&checked.tree, tip)?;
        paths.retain(|p| !judged.contains(p) || changed.contains(p));
    }
    Ok(paths)
}

/// `rev` as the commit it names now; an object ID is its own name. Everything after this
/// asks about that one commit, whatever a ref does meanwhile.
fn commit_id(root: &Path, path: &OsStr, rev: &str) -> Result<String, String> {
    if aterm_types::land_trailer::object_id(rev) {
        return Ok(rev.to_owned());
    }
    let commit = format!("{rev}^{{commit}}");
    Ok(git(
        root,
        path,
        &["rev-parse", "--verify", "--end-of-options", &commit],
    )?
    .trim()
    .to_owned())
}

/// Read Git's actual landing trailers and union only the unjudged commits' changed paths.
/// Merge commits contribute differences from their automatic merge, including resolutions
/// equal to one parent's content, rather than the already-counted parent diffs.
///
/// What a commit's ID fixes (the paths it changed, a landing's judged delta, whether a
/// composed receipt's merge holds) is asked once per release window and then answered from
/// the facts kept in the clone's release directory ([`Facts::of`]): a second call, a dry run
/// before a real one and the next head of a line ask only about commits no call has read.
///
/// # Errors
/// `base` or `tip` names no commit, `base` is not `tip`'s ancestor, or a Git read failed.
pub fn discover(root: &Path, path: &OsStr, base: &str, tip: &str) -> Result<Input, String> {
    discover_counted(root, path, base, tip).map(|(input, _)| input)
}

/// [`discover`], with how many of its questions about commits it asked and how many it
/// answered from the kept facts.
///
/// # Errors
/// As [`discover`].
pub fn discover_counted(
    root: &Path,
    path: &OsStr,
    base: &str,
    tip: &str,
) -> Result<(Input, (usize, usize)), String> {
    let base = commit_id(root, path, base)?;
    let tip = commit_id(root, path, tip)?;
    let facts = Facts::of(root, &base).read_by(&git(root, path, &["--version"])?);
    let input = discover_with(&facts, COMPOSITION_RULE, root, path, &base, &tip)?;
    Ok((input, facts.counts()))
}

/// [`discover`] over `facts`, with `composition_rule` the text a kept composition answer is
/// bound to.
fn discover_with(
    facts: &Facts,
    composition_rule: &[&str],
    root: &Path,
    path: &OsStr,
    base: &str,
    tip: &str,
) -> Result<Input, String> {
    git_fact(
        facts,
        root,
        path,
        &["merge-base", "--is-ancestor", base, tip],
        &[base, tip],
    )?;
    let range = format!("{base}..{tip}");
    let log = git(
        root,
        path,
        &[
            "log",
            "--format=%H%x1f%T%x1f%P%x1f%(trailers:only,unfold)%x1e",
            &range,
        ],
    )?;
    let mut nodes = BTreeMap::new();
    for record in log.split('\u{1e}').map(str::trim).filter(|s| !s.is_empty()) {
        let fields: Vec<_> = record.splitn(4, '\u{1f}').collect();
        if fields.len() != 4 {
            return Err("malformed Git commit record".into());
        }
        nodes.insert(
            fields[0].to_string(),
            Commit {
                tree: fields[1].to_string(),
                parents: fields[2].split_whitespace().map(str::to_string).collect(),
                receipt: Stated::of_trailers(fields[3]),
            },
        );
    }
    // A receipt whose composition its git evidence does not establish (a conflicted or
    // shared-path merge, an unreadable object) grants no coverage; the ordinary source checks
    // still judge those commits. A read that failed is asked again by the next call.
    let composes = |parent: &str, onto: &str, tree: &str| {
        let mut question: Vec<&[u8]> = vec![b"composition"];
        question.extend(composition_rule.iter().map(|text| text.as_bytes()));
        question.extend([parent.as_bytes(), onto.as_bytes(), tree.as_bytes()]);
        facts
            .answer(&question, &[parent, onto, tree], || {
                crate::land::receipt_composition(root, path, parent, onto, tree)
                    .map(|holds| if holds { "holds" } else { "refused" }.to_owned())
            })
            .is_ok_and(|answer| answer == "holds")
    };
    let mut composed = BTreeSet::new();
    for (id, commit) in &nodes {
        let Some(receipt) = &commit.receipt else {
            continue;
        };
        let (Some(onto), (_, Some(checked)), [parent, main]) = (
            &receipt.composed_onto,
            stated_judgments(receipt),
            commit.parents.as_slice(),
        ) else {
            continue;
        };
        if onto == main
            && nodes.get(parent).is_some_and(|c| c.tree == checked.tree)
            && composes(parent, onto, &commit.tree)
        {
            composed.insert(id.clone());
        }
    }
    let covered = coverage(facts, root, path, &nodes, &composed);
    let commits: Vec<_> = nodes
        .keys()
        .filter(|id| !covered.commits.contains(*id))
        .cloned()
        .collect();
    let mut paths = BTreeSet::new();
    for commit in &commits {
        let parents = &nodes[commit].parents;
        let parents: Vec<_> = parents.iter().map(String::as_str).collect();
        let objects: Vec<&str> = std::iter::once(commit.as_str())
            .chain(parents.iter().copied())
            .collect();
        let changed = crate::git_paths::commit(
            &mut |args| git_fact(facts, root, path, args, &objects).map(String::into_bytes),
            commit,
            &parents,
            crate::git_paths::MergeDiff::Resolutions,
            &[],
        )?;
        for name in changed {
            paths.insert(String::from_utf8(name).map_err(|e| e.to_string())?);
        }
    }
    Ok(Input {
        commits,
        paths: remaining_paths(facts, root, path, tip, paths, &covered.judgments)?
            .into_iter()
            .collect(),
    })
}

/// Run the existing checks on an exact candidate checkout; no publication, receipt or ref write.
/// A missing tool stays an undecided check, distinct from a source finding.
pub fn judge(
    root: &Path,
    base: &str,
    tip: &str,
    checks: &[Check],
    events: Option<crate::event_log::EventLog>,
    log: &mut (dyn std::io::Write + Send),
) -> Result<String, (bool, String)> {
    judge_with_inputs(root, base, tip, checks, events, log, InputCarry::default())
}

#[derive(Default)]
struct InputCarry {
    context: Option<String>,
    items: Vec<crate::input_content::Proof>,
}

fn judge_with_inputs(
    root: &Path,
    base: &str,
    tip: &str,
    checks: &[Check],
    events: Option<crate::event_log::EventLog>,
    log: &mut (dyn std::io::Write + Send),
    carry: InputCarry,
) -> Result<String, (bool, String)> {
    let unknown: Vec<&str> = checks
        .iter()
        .filter(|check| !CHECKS.contains(check))
        .map(|check| check.name())
        .collect();
    if !unknown.is_empty() {
        return Err((false, not_readers(&unknown)));
    }
    let began = std::time::Instant::now();
    let env = EnvSnapshot::capture();
    let (input, (asked, kept)) =
        discover_counted(root, &env.path, base, tip).map_err(|why| (false, why))?;
    if let Some(note) = input.no_work_note() {
        return Ok(format!("{note} ({:.3}s)", began.elapsed().as_secs_f64()));
    }
    writeln!(
        log,
        "release input discovery: {:.3}s ({asked} question(s) asked of Git, {kept} answered \
         from this release window's kept facts)",
        began.elapsed().as_secs_f64()
    )
    .map_err(|e| (false, e.to_string()))?;
    writeln!(
        log,
        "release input: {} unreceipted commits, {} paths since {base}",
        input.commits.len(),
        input.paths.len()
    )
    .map_err(|e| (false, e.to_string()))?;
    let tools = crate::preflight(&env, root).map_err(|why| (false, why))?;
    let scratch = aterm_tempfile::tempdir().map_err(|e| (false, e.to_string()))?;
    let (_leases, _) = crate::hold_toolchain(&env, &tools, root);
    let mut ctx = Ctx::new_with_tools(
        root.to_path_buf(),
        crate::Scope::workspace(),
        env,
        scratch.path().to_path_buf(),
        tools,
    )
    .with_quiet(Some(aterm_capacity::Admit::Now))
    .with_halt(crate::halt_of(&crate::cli::Args {
        fail_fast: true,
        ..crate::cli::Args::default()
    }))
    .with_events(events)
    .with_memory(crate::memory::Book::live());
    ctx.input_paths = Some(input.paths.clone());
    ctx.carry_context = carry.context;
    ctx.input_items = carry.items;
    ctx.head_at_start = Some(tip.into());
    let mut unavailable = Vec::new();
    for check in checks.iter().copied() {
        let name = check.name();
        if check == Check::Fmt && ctx.tools.trustfmt.is_none() {
            unavailable.push(format!("{name}: its Trust driver is unavailable"));
            continue;
        }
        ctx.checks = vec![check];
        let mut output = Vec::new();
        let code = crate::run(
            &ctx,
            &mut output,
            log,
            &Show {
                cone: None,
                log: None,
            },
        )
        .map_err(|e| (false, e.to_string()))?;
        log.write_all(&output).map_err(|e| (false, e.to_string()))?;
        if code == crate::exit::FAILED {
            return Err((
                true,
                format!("{name}: {}", String::from_utf8_lossy(&output)),
            ));
        }
        if code != 0 {
            unavailable.push(format!("{name}: {}", String::from_utf8_lossy(&output)));
        }
    }
    if !unavailable.is_empty() {
        return Err((false, unavailable.join("\n")));
    }
    Ok(format!(
        "release input: {} unreceipted commits, {} paths judged ({:.3}s)",
        input.commits.len(),
        input.paths.len(),
        began.elapsed().as_secs_f64()
    ))
}

/// Standalone entry for measuring the same preflight the release runs at step one.
pub fn run_cli(args: &[String], root: &Path) -> i32 {
    run_cli_to(args, root, &mut std::io::stderr())
}

/// A usage error: why, then the usage line, to `err`; its exit.
fn usage(err: &mut dyn std::io::Write, why: &str) -> i32 {
    let _ = writeln!(err, "verify release-input: {why}\n{USAGE_LINE}");
    crate::exit::USAGE
}

/// What the CLI's arguments ask, read whole before anything is opened or run.
struct Options<'a> {
    base: &'a str,
    checks: Vec<Check>,
    events: Option<&'a str>,
    context: Option<&'a str>,
    items: Option<&'a str>,
}

/// The CLI's arguments as [`Options`], or why they are not: `--base` first, then each option
/// once, with its value.
fn options(args: &[String]) -> Result<Options<'_>, String> {
    let [flag, base, rest @ ..] = args else {
        return Err("--base <last-release-commit> comes first".into());
    };
    if flag != "--base" {
        return Err("--base <last-release-commit> comes first".into());
    }
    let mut asked = Options {
        base,
        checks: CHECKS.to_vec(),
        events: None,
        context: None,
        items: None,
    };
    let mut given: Vec<&str> = Vec::new();
    for pair in rest.chunks(2) {
        let [option, value] = pair else {
            return Err(format!("{} takes a value", pair[0]));
        };
        let option = option.as_str();
        if given.contains(&option) {
            return Err(format!("`{option}` is given twice"));
        }
        given.push(option);
        match option {
            "--passed-input-items" => asked.items = Some(value),
            "--carry-context" => asked.context = Some(value),
            "--checks" => {
                asked.checks = checks_named(value).map_err(|why| format!("--checks: {why}"))?
            }
            "--events" => asked.events = Some(value),
            other => return Err(format!("unknown option `{other}`")),
        }
    }
    Ok(asked)
}

/// [`run_cli`], its own diagnostics written to `err`.
fn run_cli_to(args: &[String], root: &Path, err: &mut dyn std::io::Write) -> i32 {
    let asked = match options(args) {
        Ok(asked) => asked,
        Err(why) => return usage(err, &why),
    };
    let items = match asked.items.map(|path| {
        std::fs::read_to_string(path)
            .map_err(|e| e.to_string())
            .and_then(|text| crate::input_content::read(&text))
    }) {
        None => Vec::new(),
        Some(Ok(items)) => items,
        Some(Err(why)) => {
            let _ = writeln!(err, "cannot read source-check input evidence: {why}");
            return crate::exit::COULD_NOT_RUN;
        }
    };
    let events = match asked
        .events
        .map(|path| crate::event_log::EventLog::open(&root.join(path)))
        .transpose()
    {
        Ok(events) => events,
        Err(why) => {
            let _ = writeln!(err, "cannot open events: {why}");
            return crate::exit::COULD_NOT_RUN;
        }
    };
    let context = asked.context.map(str::to_owned);
    #[cfg(unix)]
    crate::exec::group::kill_on_interrupt();
    let tip = match git(
        root,
        &std::env::var_os("PATH").unwrap_or_default(),
        &["rev-parse", "HEAD"],
    ) {
        Ok(tip) => tip.trim().to_string(),
        Err(why) => {
            let _ = writeln!(err, "{why}");
            return crate::exit::COULD_NOT_RUN;
        }
    };
    let code = match judge_with_inputs(
        root,
        asked.base,
        &tip,
        &asked.checks,
        events,
        &mut std::io::stdout(),
        InputCarry { context, items },
    ) {
        Ok(note) => {
            println!("{note}");
            crate::exit::PASS
        }
        Err((red, why)) => {
            let _ = writeln!(
                err,
                "{} {why}",
                if red { "WOULD REFUSE" } else { "COULD NOT RUN" }
            );
            if red {
                crate::exit::FAILED
            } else {
                crate::exit::COULD_NOT_RUN
            }
        }
    };
    #[cfg(unix)]
    crate::exec::group::end_interrupted();
    code
}

#[cfg(test)]
#[path = "release_input_tests.rs"]
mod tests;
