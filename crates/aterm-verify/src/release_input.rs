// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The release's early judgment of commits without a landing receipt. Receipts describe a
//! delta from a base, not every ancestor of their tree: main imported at the base stays owed.

use crate::{Ctx, EnvSnapshot, Show, checks::Check};
use aterm_types::land_trailer::{Checked, Stated};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::path::Path;

/// Early source readers. Qualification lints the whole release tree before publication;
/// running its compiler passes here too only repeats that work.
pub const CHECKS: [Check; 3] = [Check::ExportScan, Check::Fmt, Check::Census];
const NAMES: [&str; 3] = ["export-scan", "fmt", "census"];

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
    let mut command = aterm_uds::spawnfd::command(aterm_uds::os_path!("git"));
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
        if !receipt
            .checked
            .last()
            .is_some_and(|c| c.tree == commit.tree)
            && !composed.contains(landing)
        {
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
        for checked in &receipt.checked {
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
                    || git(
                        root,
                        path,
                        &["merge-base", "--is-ancestor", &checked.base, target],
                    )
                    .is_err())
            {
                continue;
            }
            covered.extend(judged.difference(&ancestors(nodes, &checked.base)).cloned());
            judgments.push(checked.clone());
            valid |= receipt.checked.last() == Some(checked);
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
    root: &Path,
    path: &OsStr,
    tip: &str,
    mut paths: BTreeSet<String>,
    judgments: &[Checked],
) -> Result<BTreeSet<String>, String> {
    let diff = |a: &str, b: &str| -> Result<BTreeSet<String>, String> {
        Ok(git(
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

/// Read Git's actual landing trailers and union only the unjudged commits' changed paths.
/// Merge commits contribute differences from their automatic merge, including resolutions
/// equal to one parent's content, rather than the already-counted parent diffs.
pub fn discover(root: &Path, path: &OsStr, base: &str, tip: &str) -> Result<Input, String> {
    git(root, path, &["merge-base", "--is-ancestor", base, tip])?;
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
    let mut composed = BTreeSet::new();
    for (id, commit) in &nodes {
        let Some(receipt) = &commit.receipt else {
            continue;
        };
        let (Some(onto), Some(checked), [parent, main]) = (
            &receipt.composed_onto,
            receipt.checked.last(),
            commit.parents.as_slice(),
        ) else {
            continue;
        };
        if onto == main
            && nodes.get(parent).is_some_and(|c| c.tree == checked.tree)
            // Historical receipts can predate the current entry grammar. Failure to
            // establish composition grants no coverage; the ordinary source checks
            // still judge these commits rather than being blocked by old evidence.
            && crate::land::receipt_composition(root, path, parent, onto, &commit.tree)
                .unwrap_or(false)
        {
            composed.insert(id.clone());
        }
    }
    let covered = coverage(root, path, &nodes, &composed);
    let commits: Vec<_> = nodes
        .keys()
        .filter(|id| !covered.commits.contains(*id))
        .cloned()
        .collect();
    let mut paths = BTreeSet::new();
    for commit in &commits {
        let parents = &nodes[commit].parents;
        let parents: Vec<_> = parents.iter().map(String::as_str).collect();
        let changed = crate::git_paths::commit(
            &mut |args| git(root, path, args).map(String::into_bytes),
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
        paths: remaining_paths(root, path, tip, paths, &covered.judgments)?
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
    if checks.iter().any(|check| !CHECKS.contains(check)) {
        return Err((
            false,
            "release-input runs only source readers; lint belongs to qualification".into(),
        ));
    }
    let began = std::time::Instant::now();
    let env = EnvSnapshot::capture();
    let input = discover(root, &env.path, base, tip).map_err(|why| (false, why))?;
    if let Some(note) = input.no_work_note() {
        return Ok(format!("{note} ({:.3}s)", began.elapsed().as_secs_f64()));
    }
    writeln!(
        log,
        "release input discovery: {:.3}s",
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
    if args.len() < 2 || args[0] != "--base" {
        eprintln!(
            "usage: verify release-input --base <last-release-commit> [--checks NAMES] [--events PATH]"
        );
        return crate::exit::USAGE;
    }
    let mut checks = CHECKS.to_vec();
    let mut events = None;
    let mut context = None;
    let mut items = Vec::new();
    for pair in args[2..].chunks(2) {
        if pair.len() != 2 {
            return crate::exit::USAGE;
        }
        match pair[0].as_str() {
            "--passed-input-items" => {
                items = match std::fs::read_to_string(&pair[1])
                    .map_err(|e| e.to_string())
                    .and_then(|text| crate::input_content::read(&text))
                {
                    Ok(items) => items,
                    Err(why) => {
                        eprintln!("cannot read source-check input evidence: {why}");
                        return crate::exit::COULD_NOT_RUN;
                    }
                };
            }
            "--carry-context" => context = Some(pair[1].clone()),
            "--checks" => {
                let names: Vec<_> = pair[1].split(',').collect();
                if names.iter().any(|name| !NAMES.contains(name)) {
                    return crate::exit::USAGE;
                }
                checks.retain(|check| names.contains(&check.name()));
            }
            "--events" => match crate::event_log::EventLog::open(&root.join(&pair[1])) {
                Ok(file) => events = Some(file),
                Err(why) => {
                    eprintln!("cannot open events: {why}");
                    return crate::exit::COULD_NOT_RUN;
                }
            },
            _ => return crate::exit::USAGE,
        }
    }
    #[cfg(unix)]
    crate::exec::group::kill_on_interrupt();
    let tip = match git(
        root,
        &std::env::var_os("PATH").unwrap_or_default(),
        &["rev-parse", "HEAD"],
    ) {
        Ok(tip) => tip.trim().to_string(),
        Err(why) => {
            eprintln!("{why}");
            return crate::exit::COULD_NOT_RUN;
        }
    };
    let code = match judge_with_inputs(
        root,
        &args[1],
        &tip,
        &checks,
        events,
        &mut std::io::stdout(),
        InputCarry { context, items },
    ) {
        Ok(note) => {
            println!("{note}");
            crate::exit::PASS
        }
        Err((red, why)) => {
            eprintln!(
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
mod tests {
    use super::*;
    use aterm_types::land_trailer::{Checked, Row};

    /// Replay immutable release history without entering publication or touching its host.
    #[test]
    #[ignore = "measurement over the caller's repository history"]
    fn replay_release_selection() {
        let root = aterm_tempfile::manifest_dir().join("../..");
        let base = std::env::var("ATERM_REPLAY_BASE").unwrap();
        let tip = std::env::var("ATERM_REPLAY_TIP").unwrap();
        let began = std::time::Instant::now();
        let input = discover(
            &root,
            &std::env::var_os("PATH").unwrap_or_default(),
            &base,
            &tip,
        )
        .unwrap();
        println!(
            "selection: {} commits, {} paths, {:.3}s",
            input.commits.len(),
            input.paths.len(),
            began.elapsed().as_secs_f64()
        );
        for commit in input.commits {
            println!("commit {commit}");
        }
        for path in input.paths {
            println!("path {path}");
        }
    }

    struct Repo(aterm_tempfile::TempDir);
    impl Repo {
        fn new() -> Self {
            let repo = Self(aterm_tempfile::tempdir().unwrap());
            repo.git(&["init", "-q"]);
            repo.git(&["config", "user.name", "Fixture"]);
            repo.git(&["config", "user.email", "fixture@example.invalid"]);
            repo.write("seed", "seed\n");
            repo.commit("base");
            repo
        }
        fn git(&self, args: &[&str]) -> String {
            git(
                self.0.path(),
                &std::env::var_os("PATH").unwrap_or_default(),
                args,
            )
            .unwrap()
            .trim()
            .to_string()
        }
        fn write(&self, name: &str, text: &str) {
            std::fs::write(self.0.path().join(name), text).unwrap();
        }
        fn commit(&self, message: &str) -> String {
            self.git(&["add", "."]);
            self.git(&["commit", "-qm", message]);
            self.git(&["rev-parse", "HEAD"])
        }
        fn input(&self, base: &str) -> Input {
            discover(
                self.0.path(),
                &std::env::var_os("PATH").unwrap_or_default(),
                base,
                "HEAD",
            )
            .unwrap()
        }
        fn receipt(&self, base: &str) -> String {
            Stated {
                checked: vec![Checked {
                    tree: self.git(&["rev-parse", "HEAD^{tree}"]),
                    base: base.into(),
                    secs: 1,
                    rows: NAMES
                        .into_iter()
                        .map(|name| Row {
                            check: name.into(),
                            word: "ok".into(),
                        })
                        .collect(),
                }],
                ..Stated::default()
            }
            .trailers()
        }
        fn landing(&self, tree: &str, branch: &str, main: &str, receipt: &Stated) -> String {
            let id = self.git(&[
                "commit-tree",
                tree,
                "-p",
                branch,
                "-p",
                main,
                "-m",
                &format!("land\n\n{}", receipt.trailers()),
            ]);
            self.git(&["update-ref", "HEAD", &id]);
            id
        }
    }

    #[test]
    fn empty_input_needs_no_workspace_or_check_tools() {
        let repo = Repo::new();
        let head = repo.git(&["rev-parse", "HEAD"]);
        let mut log = Vec::new();
        assert!(judge(repo.0.path(), &head, &head, &CHECKS, None, &mut log).is_ok());
        assert!(log.is_empty(), "no source check was entered");
        assert!(
            Input {
                commits: vec![head],
                paths: vec!["seed".into()]
            }
            .no_work_note()
            .is_none()
        );
    }

    #[test]
    fn early_readers_cannot_dispatch_qualification_lint() {
        let r = Repo::new();
        let base = r.git(&["rev-parse", "HEAD"]);
        let error = judge(
            r.0.path(),
            &base,
            &base,
            &[Check::Lint],
            None,
            &mut Vec::new(),
        )
        .unwrap_err();
        assert!(!error.0);
        assert!(error.1.contains("qualification"));
        assert_eq!(
            run_cli(
                &["--base".into(), base, "--checks".into(), "lint".into()],
                r.0.path()
            ),
            crate::exit::USAGE
        );
    }

    /// The very same tree is owed without its real landing trailer, and covered with it.
    /// An earlier unchecked main commit stays owed: the receipt's base is not its cone.
    #[test]
    fn hand_merge_and_landed_merge_have_different_input() {
        let r = Repo::new();
        let released = r.git(&["rev-parse", "HEAD"]);
        r.write("unreviewed", "unreviewed\n");
        let base = r.commit("unchecked main");
        r.git(&["checkout", "-qb", "topic"]);
        r.write("reviewed", "reviewed\n");
        let topic = r.commit("topic");
        let tree = r.git(&["rev-parse", "HEAD^{tree}"]);
        let receipt = r.receipt(&base);
        let hand = r.git(&[
            "commit-tree",
            &tree,
            "-p",
            &topic,
            "-p",
            &base,
            "-m",
            "hand merge",
        ]);
        r.git(&["update-ref", "HEAD", &hand]);
        let before = r.input(&released);
        assert_eq!(before.paths, ["reviewed", "unreviewed"]);
        let landed = r.git(&[
            "commit-tree",
            &tree,
            "-p",
            &topic,
            "-p",
            &base,
            "-m",
            &format!("land\n\n{receipt}"),
        ]);
        r.git(&["update-ref", "HEAD", &landed]);
        let after = r.input(&released);
        assert_eq!(after.paths, ["unreviewed"]);
        assert!(!after.commits.contains(&topic));
        // A receipt for another tree cannot hide the hand merge's new content.
        let stale = receipt.replace(&tree, &"f".repeat(40));
        let bad = r.git(&[
            "commit-tree",
            &tree,
            "-p",
            &topic,
            "-p",
            &base,
            "-m",
            &format!("stale\n\n{stale}"),
        ]);
        r.git(&["update-ref", "HEAD", &bad]);
        assert_eq!(r.input(&released).paths, before.paths);
    }

    /// A checked edit to an earlier unchecked path covers its resulting entry, including
    /// deletion. Subsequent content or mode changes are still owed.
    #[test]
    fn later_judgment_covers_entries_but_never_later_edits() {
        let r = Repo::new();
        let released = r.git(&["rev-parse", "HEAD"]);
        r.write("reviewed", "unchecked\n");
        r.write("deleted", "unchecked\n");
        r.write("other", "unchecked\n");
        let base = r.commit("unchecked main");
        r.write("reviewed", "checked\n");
        std::fs::remove_file(r.0.path().join("deleted")).unwrap();
        let topic = r.commit("repair and judge");
        let tree = r.git(&["rev-parse", "HEAD^{tree}"]);
        let receipt = Stated::of_trailers(&r.receipt(&base)).unwrap();
        r.landing(&tree, &topic, &base, &receipt);
        assert_eq!(r.input(&released).paths, ["other"]);
        r.write("reviewed", "later unchecked\n");
        r.commit("later edit");
        assert_eq!(r.input(&released).paths, ["other", "reviewed"]);
        r.write("reviewed", "checked\n");
        r.commit("restore checked bytes");
        assert_eq!(r.input(&released).paths, ["other"]);
        r.git(&["update-index", "--chmod=+x", "reviewed"]);
        r.git(&["commit", "-qm", "unchecked mode"]);
        assert_eq!(r.input(&released).paths, ["other", "reviewed"]);
    }

    /// A merge's new resolution is its own input even when both parents were judged.
    #[test]
    fn merge_resolution_cannot_hide_behind_checked_parents() {
        let r = Repo::new();
        let base = r.git(&["rev-parse", "HEAD"]);
        r.write("seed", "topic\n");
        let topic = r.commit("topic");
        r.git(&["checkout", "--detach", &base]);
        r.write("seed", "main\n");
        let main = r.commit("main");
        r.write("seed", "resolution\n");
        r.git(&["add", "."]);
        let tree = r.git(&["write-tree"]);
        let merged = r.git(&[
            "commit-tree",
            &tree,
            "-p",
            &topic,
            "-p",
            &main,
            "-m",
            "resolution",
        ]);
        r.git(&["update-ref", "HEAD", &merged]);
        assert_eq!(r.input(&base).paths, ["seed"]);
    }

    /// An octopus merge's input is every path where it differs from any parent, as
    /// `--diff-merges=separate` reads it, gathered one parent at a time. Here both tips are
    /// already released, so the archive merge alone is owed, with its new file and each path
    /// where it differs from a tip.
    ///
    /// NEGATIVE CONTROL: read the first parent's diff alone and the paths are `manifest` only;
    /// read it as one `--diff-merges=separate` capture and a large enough archive merge
    /// overflows the complete-capture limit, the refusal of 0.193.0's first dry run on main's
    /// 60-parent preservation merge 7514a3dd04d.
    #[test]
    fn an_octopus_merge_owes_every_path_where_it_differs_from_a_parent() {
        let r = Repo::new();
        let base = r.git(&["rev-parse", "HEAD"]);
        r.write("tip-a", "a\n");
        let a = r.commit("tip a");
        r.git(&["checkout", "-q", "--detach", &base]);
        r.write("tip-b", "b\n");
        let b = r.commit("tip b");
        r.git(&["merge", "-q", "--no-edit", &a]);
        let released = r.git(&["rev-parse", "HEAD"]);
        r.write("manifest", "kept\n");
        r.git(&["add", "."]);
        let tree = r.git(&["write-tree"]);
        let merged = r.git(&[
            "commit-tree",
            &tree,
            "-p",
            &released,
            "-p",
            &a,
            "-p",
            &b,
            "-m",
            "archive",
        ]);
        r.git(&["update-ref", "HEAD", &merged]);
        let input = r.input(&released);
        assert_eq!(input.commits, std::slice::from_ref(&merged));
        assert_eq!(input.paths, ["manifest", "tip-a", "tip-b"]);
        let separate = r.git(&[
            "show",
            "--format=",
            "--name-only",
            "--no-renames",
            "--diff-merges=separate",
            &merged,
        ]);
        let separate: BTreeSet<&str> = separate.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(separate, BTreeSet::from(["manifest", "tip-a", "tip-b"]));
    }

    /// Choosing a parent's old content can undo a checked fix. A combined diff omits that
    /// path, but comparison with the automatic merge still finds the unjudged edit.
    #[test]
    fn a_resolution_equal_to_one_parent_is_still_unchecked() {
        let r = Repo::new();
        let base = r.git(&["rev-parse", "HEAD"]);
        let old_tree = r.git(&["rev-parse", "HEAD^{tree}"]);
        r.write("fix", "checked fix\n");
        let topic = r.commit("fix");
        let tree = r.git(&["rev-parse", "HEAD^{tree}"]);
        let receipt = r.receipt(&base);
        let landed = r.git(&[
            "commit-tree",
            &tree,
            "-p",
            &topic,
            "-p",
            &base,
            "-m",
            &format!("land\n\n{receipt}"),
        ]);
        let evil = r.git(&[
            "commit-tree",
            &old_tree,
            "-p",
            &landed,
            "-p",
            &base,
            "-m",
            "drop the fix",
        ]);
        r.git(&["update-ref", "HEAD", &evil]);
        let input = r.input(&base);
        assert_eq!(input.commits, [evil]);
        assert_eq!(input.paths, ["fix"]);
    }

    #[test]
    fn semantic_changelog_result_cannot_borrow_its_parents_judgment() {
        let r = Repo::new();
        let notes = "## [Unreleased]\n\n### Fixed\n\n- Old claim.\n";
        r.write("CHANGELOG.md", notes);
        let base = r.commit("base notes");
        r.write(
            "CHANGELOG.md",
            &notes.replace("Old claim.", "Corrected claim."),
        );
        let branch = r.commit("checked correction");
        let mut receipt = Stated::of_trailers(&r.receipt(&base)).unwrap();
        r.git(&["checkout", "--detach", &base]);
        r.write("CHANGELOG.md", &format!("{notes}\n- Main addition.\n"));
        let main = r.commit("main addition");
        // Combine both entry edits without judging the resulting tree.
        r.write(
            "CHANGELOG.md",
            &format!(
                "{}\n- Main addition.\n",
                notes.replace("Old claim.", "Corrected claim.")
            ),
        );
        r.git(&["add", "."]);
        let tree = r.git(&["write-tree"]);
        receipt.composed_onto = Some(main.clone());
        let landing = r.landing(&tree, &branch, &main, &receipt);
        let input = r.input(&base);
        assert!(input.commits.contains(&landing));
        assert_eq!(input.paths, ["CHANGELOG.md"]);
        // Actual judgment of this tree changes the verdict.
        receipt.checked[0].tree = tree.clone();
        r.landing(&tree, &branch, &main, &receipt);
        assert!(r.input(&base).paths.is_empty());
    }

    #[test]
    fn historical_changelog_ambiguity_owes_checks_instead_of_aborting_discovery() {
        let r = Repo::new();
        let notes = "## [Unreleased]\n\n### Fixed\n\n- First.\n\n### Fixed\n\n- Second.\n";
        r.write("CHANGELOG.md", notes);
        let base = r.commit("old repeated subsections");
        r.write("branch", "checked\n");
        let branch = r.commit("branch");
        let mut receipt = Stated::of_trailers(&r.receipt(&base)).unwrap();
        r.git(&["checkout", "--detach", &base]);
        r.write("CHANGELOG.md", &notes.replace("Second.", "Main's change."));
        let main = r.commit("main");
        r.git(&["checkout", "--detach", &branch]);
        r.git(&["merge", "--no-ff", "-m", "historical merge", &main]);
        let tree = r.git(&["rev-parse", "HEAD^{tree}"]);
        receipt.composed_onto = Some(main.clone());
        let landing = r.landing(&tree, &branch, &main, &receipt);
        // New rules cannot establish this old receipt. It grants no credit, but the
        // ordinary source readers must still be able to judge the candidate.
        let input = r.input(&base);
        assert!(input.commits.contains(&landing));
        assert_eq!(input.paths, ["CHANGELOG.md", "branch"]);
    }

    /// A composed landing carries only the checked branch's delta. Main that arrived while
    /// it ran remains owed, and an extra resolution edit invalidates the composition claim.
    #[test]
    fn composition_excludes_new_main_and_checks_its_actual_tree() {
        let r = Repo::new();
        let base = r.git(&["rev-parse", "HEAD"]);
        r.write("branch", "checked\n");
        let branch = r.commit("branch");
        let mut receipt = Stated::of_trailers(&r.receipt(&base)).unwrap();
        r.git(&["checkout", "--detach", &base]);
        r.write("main", "not checked\n");
        let main = r.commit("main");
        r.git(&["checkout", "--detach", &branch]);
        r.git(&["merge", "--no-ff", "-m", "merge", &main]);
        let tree = r.git(&["rev-parse", "HEAD^{tree}"]);
        receipt.composed_onto = Some(main.clone());
        let landing = r.git(&[
            "commit-tree",
            &tree,
            "-p",
            &branch,
            "-p",
            &main,
            "-m",
            &format!("land\n\n{}", receipt.trailers()),
        ]);
        r.git(&["update-ref", "HEAD", &landing]);
        let input = r.input(&base);
        assert_eq!(input.commits.as_slice(), std::slice::from_ref(&main));
        assert_eq!(input.paths, ["main"]);
        r.write("branch", "unjudged resolution\n");
        r.git(&["add", "."]);
        let bad_tree = r.git(&["write-tree"]);
        let bad = r.git(&[
            "commit-tree",
            &bad_tree,
            "-p",
            &branch,
            "-p",
            &main,
            "-m",
            &format!("bad\n\n{}", receipt.trailers()),
        ]);
        r.git(&["update-ref", "HEAD", &bad]);
        assert!(r.input(&base).paths.contains(&"branch".into()));
    }

    /// A re-judge names the prior checked branch as its base, not the main ancestor
    /// from which the later composition's disjoint files must be computed.
    #[test]
    fn composed_rejudgment_uses_the_parents_common_main() {
        let r = Repo::new();
        let base = r.git(&["rev-parse", "HEAD"]);
        r.write("branch", "checked\n");
        let branch = r.commit("branch");
        let mut receipt = Stated::of_trailers(&r.receipt(&base)).unwrap();
        r.git(&["checkout", "--detach", &base]);
        r.write("main", "first\n");
        let main = r.commit("main");
        r.git(&["checkout", "--detach", &branch]);
        r.git(&["merge", "--no-ff", "-m", "rejudge", &main]);
        let rejudged = r.git(&["rev-parse", "HEAD"]);
        receipt
            .checked
            .extend(Stated::of_trailers(&r.receipt(&branch)).unwrap().checked);
        r.git(&["checkout", "--detach", &main]);
        r.write("main", "second\n");
        let moved = r.commit("moved main");
        r.git(&["checkout", "--detach", &rejudged]);
        r.git(&["merge", "--no-ff", "-m", "compose", &moved]);
        let tree = r.git(&["rev-parse", "HEAD^{tree}"]);
        receipt.composed_onto = Some(moved.clone());
        r.landing(&tree, &rejudged, &moved, &receipt);
        let input = r.input(&base);
        assert_eq!(input.commits.as_slice(), std::slice::from_ref(&moved));
        assert_eq!(input.paths, ["main"]);
        // A stated composition cannot authorize an extra resolution edit.
        r.write("branch", "forged\n");
        r.git(&["add", "."]);
        let bad = r.git(&["write-tree"]);
        r.landing(&bad, &rejudged, &moved, &receipt);
        assert!(r.input(&base).paths.contains(&"branch".into()));
    }

    /// An existing ancestor tree is no more authority than a nonexistent forged tree:
    /// without a valid composition it cannot hide even that ancestor's branch delta.
    #[test]
    fn unrelated_tree_trailer_cannot_cover_a_branch_prefix() {
        let r = Repo::new();
        let base = r.git(&["rev-parse", "HEAD"]);
        r.write("branch", "checked prefix\n");
        let branch = r.commit("branch");
        let receipt = Stated::of_trailers(&r.receipt(&base)).unwrap();
        r.write("extra", "unchecked\n");
        r.commit("extra");
        let tree = r.git(&["rev-parse", "HEAD^{tree}"]);
        let parent = r.git(&["rev-parse", "HEAD"]);
        r.landing(&tree, &parent, &base, &receipt);
        let input = r.input(&base);
        assert!(input.commits.contains(&branch));
        assert_eq!(input.paths, ["branch", "extra"]);
    }

    /// The release boundary truncates the in-memory graph, not the receipt's actual
    /// ancestry. A base before that boundary is valid; an unrelated base is not.
    #[test]
    fn receipt_base_before_the_release_boundary_is_checked_in_git() {
        let r = Repo::new();
        let base = r.git(&["rev-parse", "HEAD"]);
        r.write("released", "released\n");
        let released = r.commit("released");
        r.write("branch", "checked\n");
        let branch = r.commit("branch");
        let tree = r.git(&["rev-parse", "HEAD^{tree}"]);
        let mut receipt = Stated::of_trailers(&r.receipt(&base)).unwrap();
        r.landing(&tree, &branch, &released, &receipt);
        assert!(r.input(&released).commits.is_empty());
        r.git(&["checkout", "--detach", &base]);
        r.write("other", "unrelated\n");
        let unrelated = r.commit("unrelated base");
        r.git(&["checkout", "--detach", &branch]);
        receipt.checked[0].base = unrelated;
        r.landing(&tree, &branch, &released, &receipt);
        assert!(r.input(&released).commits.contains(&branch));
    }

    /// Sixty commits, only two without evidence: history size does not widen the input.
    #[test]
    fn sixty_commit_cone_keeps_only_two_unchecked_changes() {
        let r = Repo::new();
        let base = r.git(&["rev-parse", "HEAD"]);
        let began = std::time::Instant::now();
        for index in 0..29 {
            let parent = r.git(&["rev-parse", "HEAD"]);
            r.write("reviewed", &format!("{index}\n"));
            let topic = r.commit("topic");
            let tree = r.git(&["rev-parse", "HEAD^{tree}"]);
            let message = format!("land\n\n{}", r.receipt(&parent));
            let landing = r.git(&[
                "commit-tree",
                &tree,
                "-p",
                &topic,
                "-p",
                &parent,
                "-m",
                &message,
            ]);
            r.git(&["update-ref", "HEAD", &landing]);
        }
        for name in ["one", "two"] {
            r.write(name, name);
            r.commit(name);
        }
        let start = std::time::Instant::now();
        let input = r.input(&base);
        assert_eq!(input.paths, ["one", "two"]);
        println!(
            "release-input fixture: setup_s={:.3} discovery_s={:.3} commits={} paths={}",
            began.elapsed().as_secs_f64(),
            start.elapsed().as_secs_f64(),
            input.commits.len(),
            input.paths.len()
        );
    }
}
