// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The release's early judgment of commits without a landing receipt. Receipts describe a
//! delta from a base, not every ancestor of their tree: main imported at the base stays owed.

use crate::{Ctx, EnvSnapshot, Show, checks::Check};
use aterm_types::land_trailer::Stated;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::path::Path;

/// The inexpensive source checks; lint is limited to the unchecked paths' dependency cone.
pub const CHECKS: [Check; 4] = [Check::ExportScan, Check::Fmt, Check::Census, Check::Lint];
const NAMES: [&str; 4] = ["export-scan", "fmt", "census", "lint"];

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
    git_read(root, path, args, false)
}

fn git_read(root: &Path, path: &OsStr, args: &[&str], grep: bool) -> Result<String, String> {
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
    if !out.status.success() && !(grep && out.status.code() == Some(1)) {
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

fn unchecked(
    root: &Path,
    path: &OsStr,
    nodes: &BTreeMap<String, Commit>,
    composed: &BTreeSet<String>,
    empty_lint: &BTreeSet<(String, String)>,
) -> Vec<String> {
    let mut covered = BTreeSet::new();
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
                checked.rows.iter().any(|r| {
                    r.check == *name
                        && (r.word == "ok"
                            || (*name == "lint"
                                && r.word == "skip"
                                && empty_lint
                                    .contains(&(checked.base.clone(), checked.tree.clone()))))
                })
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
            valid |= receipt.checked.last() == Some(checked);
        }
        if valid {
            covered.insert(landing.clone());
        }
    }
    nodes
        .keys()
        .filter(|id| !covered.contains(*id))
        .cloned()
        .collect()
}

/// A skip alone is not a pass: a missing Tippy also produces one. Prove the empty
/// selection from immutable trees, conservatively treating every manifest as a member.
/// Use the cone selector's ownership, replan, tested-file and shared-input rules.
fn lint_has_no_inputs(root: &Path, path: &OsStr, base: &str, tree: &str) -> Result<bool, String> {
    let changed = git(
        root,
        path,
        &[
            "diff",
            "--name-only",
            "--no-renames",
            "-z",
            base,
            tree,
            "--",
        ],
    )?;
    let changed: Vec<_> = changed
        .split('\0')
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .collect();
    let listed = git(root, path, &["ls-tree", "-r", "-z", tree])?;
    let mut manifests = crate::changed::Manifests::new();
    let mut entries = Vec::new();
    for entry in listed.split('\0').filter(|p| !p.is_empty()) {
        let Some((kind, file)) = entry.split_once('\t') else {
            return Ok(false);
        };
        if (kind.starts_with("160000 ")
            && changed
                .iter()
                .any(|p| p == file || p.starts_with(&format!("{file}/"))))
            || (kind.starts_with("120000 ") && file.ends_with(".rs"))
        {
            return Ok(false);
        }
        if let Some(dir) = file.strip_suffix("/Cargo.toml") {
            manifests.insert(dir);
            entries.push(crate::changed::Member {
                name: dir.into(),
                dir: dir.into(),
                has_lib: true,
            });
        }
    }
    if entries.is_empty() {
        return Ok(false);
    }
    // A root package owns root inputs too; the normal workspace ownership walk
    // intentionally stops before the root. Such a workspace needs its real lint.
    let root_manifest = git(root, path, &["show", &format!("{tree}:Cargo.toml")])?;
    let manifest: aterm_toml::Table = match aterm_toml::from_str(&root_manifest) {
        Ok(table) => table,
        Err(_) => return Ok(false),
    };
    if manifest.get("package").is_some() {
        return Ok(false);
    }
    let members = crate::changed::Members::new(entries);
    let (seeds, replan) = crate::changed::seeds(&changed, &manifests, &members);
    if !seeds.is_empty()
        || replan.is_some()
        || changed
            .iter()
            .any(|p| crate::changed::tested_by(p).next().is_some())
    {
        return Ok(false);
    }
    let shared = git_read(
        root,
        path,
        &[
            "grep",
            "--no-textconv",
            "--full-name",
            "-n",
            "-z",
            "-F",
            "-e",
            "../",
            tree,
            "--",
            "*.rs",
        ],
        true,
    )?;
    let changed: Vec<_> = changed.iter().map(|p| root.join(p)).collect();
    for line in shared.lines() {
        let fields: Vec<_> = line.splitn(3, '\0').collect();
        if fields.len() != 3 {
            return Ok(false);
        }
        let Some(source) = fields[0].strip_prefix(&format!("{tree}:")) else {
            return Ok(false);
        };
        let Some(member) = crate::changed::owning_member(source, &manifests, &members) else {
            continue;
        };
        if crate::changed::shares_changed_input(
            &root.join(source),
            fields[2],
            &root.join(member),
            &changed,
        ) {
            return Ok(false);
        }
    }
    Ok(true)
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
    let mut empty_lint = BTreeSet::new();
    for (id, commit) in &nodes {
        let Some(receipt) = &commit.receipt else {
            continue;
        };
        for checked in &receipt.checked {
            let key = (checked.base.clone(), checked.tree.clone());
            if checked
                .rows
                .iter()
                .any(|r| r.check == "lint" && r.word == "skip")
                && !empty_lint.contains(&key)
                && lint_has_no_inputs(root, path, &checked.base, &checked.tree).unwrap_or(false)
            {
                empty_lint.insert(key);
            }
        }
        let (Some(onto), Some(checked), [parent, main]) = (
            &receipt.composed_onto,
            receipt.checked.last(),
            commit.parents.as_slice(),
        ) else {
            continue;
        };
        if onto == main
            && nodes.get(parent).is_some_and(|c| c.tree == checked.tree)
            && crate::land::receipt_composition(root, path, parent, onto, &commit.tree)?
        {
            composed.insert(id.clone());
        }
    }
    let commits = unchecked(root, path, &nodes, &composed, &empty_lint);
    let mut paths = BTreeSet::new();
    for commit in &commits {
        let merge_diff = if nodes[commit].parents.len() > 2 {
            "--diff-merges=separate"
        } else {
            "--diff-merges=remerge"
        };
        let diff = git(
            root,
            path,
            &[
                "show",
                "--format=",
                "--name-only",
                "-z",
                "--no-renames",
                merge_diff,
                commit,
                "--",
            ],
        )?;
        paths.extend(
            diff.split('\0')
                .filter(|p| !p.is_empty())
                .map(str::to_string),
        );
    }
    Ok(Input {
        commits,
        paths: paths.into_iter().collect(),
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
    judge_with_lint(root, base, tip, checks, events, log, LintCarry::default())
}

#[derive(Default)]
struct LintCarry {
    passed: Option<crate::lint_content::Passed>,
    context: Option<String>,
}

fn judge_with_lint(
    root: &Path,
    base: &str,
    tip: &str,
    checks: &[Check],
    events: Option<crate::event_log::EventLog>,
    log: &mut (dyn std::io::Write + Send),
    carry: LintCarry,
) -> Result<String, (bool, String)> {
    let began = std::time::Instant::now();
    let env = EnvSnapshot::capture();
    let input = discover(root, &env.path, base, tip).map_err(|why| (false, why))?;
    if let Some(note) = input.no_work_note() {
        return Ok(format!("{note} ({:.3}s)", began.elapsed().as_secs_f64()));
    }
    writeln!(
        log,
        "release input: {} unreceipted commits, {} paths since {base}",
        input.commits.len(),
        input.paths.len()
    )
    .map_err(|e| (false, e.to_string()))?;
    let tools = crate::preflight(&env, root).map_err(|why| (false, why))?;
    let scratch = aterm_tempfile::tempdir().map_err(|e| (false, e.to_string()))?;
    let selection =
        // The paths are the unreceipted commits', which no one tree precedes: a revert of a
        // receipted change compares equal to `base`.
        crate::changed::resolve_paths(root, &tools, &env.path, base, input.paths.clone(), None);
    let (scope, cone) = crate::changed::cone(base, &selection);
    let (_leases, _) = crate::hold_toolchain(&env, &tools, root);
    let mut ctx = Ctx::new_with_tools(
        root.to_path_buf(),
        scope,
        env,
        scratch.path().to_path_buf(),
        tools,
    )
    .with_quiet(Some(aterm_capacity::Admit::Now))
    .with_halt(crate::halt_of(false, true))
    .with_events(events)
    .with_memory(crate::memory::Book::live());
    ctx.input_paths = Some(input.paths.clone());
    ctx.passed_lint_packages = carry.passed;
    ctx.carry_context = carry.context;
    ctx.head_at_start = Some(tip.into());
    let mut unavailable = Vec::new();
    for check in checks.iter().copied() {
        let name = check.name();
        if (check == Check::Fmt && ctx.tools.trustfmt.is_none())
            || (check == Check::Lint && ctx.tools.tippy.is_none())
        {
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
                cone: Some(&cone),
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
    let mut passed = None;
    let mut context = None;
    for pair in args[2..].chunks(2) {
        if pair.len() != 2 {
            return crate::exit::USAGE;
        }
        match pair[0].as_str() {
            "--carry-context" => context = Some(pair[1].clone()),
            "--passed-lint-packages" => {
                passed = match std::fs::read_to_string(&pair[1])
                    .map_err(|e| e.to_string())
                    .and_then(|text| crate::lint_content::read_passed(&text))
                {
                    Ok(passed) => Some(passed),
                    Err(why) => {
                        eprintln!("cannot read lint package evidence: {why}");
                        return crate::exit::COULD_NOT_RUN;
                    }
                };
            }
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
    let code = match judge_with_lint(
        root,
        &args[1],
        &tip,
        &checks,
        events,
        &mut std::io::stdout(),
        LintCarry { passed, context },
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

    /// Trust an empty lint skip, never a missing compiler's skip over actual source
    /// or a document read by source. All evidence comes from the named tree.
    #[test]
    fn empty_lint_skip_is_proven_from_the_checked_tree() {
        let r = Repo::new();
        std::fs::create_dir_all(r.0.path().join("crates/member/src")).unwrap();
        r.write("Cargo.toml", "[workspace]\nmembers=['crates/member']\n");
        r.write(
            "crates/member/Cargo.toml",
            "[package]\nname='member'\nversion='0.0.0'\n",
        );
        r.write("crates/member/src/lib.rs", "pub fn value() {}\n");
        let base = r.commit("workspace");
        let land = |base: &str| {
            let branch = r.git(&["rev-parse", "HEAD"]);
            let tree = r.git(&["rev-parse", "HEAD^{tree}"]);
            let mut receipt = Stated::of_trailers(&r.receipt(base)).unwrap();
            receipt.checked[0]
                .rows
                .iter_mut()
                .find(|r| r.check == "lint")
                .unwrap()
                .word = "skip".into();
            r.landing(&tree, &branch, base, &receipt)
        };
        r.write("REPORT.md", "documentation\n");
        r.commit("document");
        land(&base);
        assert!(r.input(&base).commits.is_empty());
        r.git(&["checkout", "--detach", &base]);
        r.write("crates/member/src/lib.rs", "pub fn value() -> u8 { 1 }\n");
        let source = r.commit("source without lint");
        land(&base);
        assert!(r.input(&base).commits.contains(&source));
        r.git(&["checkout", "--detach", &base]);
        r.write("REPORT.md", "included\n");
        r.write(
            "crates/member/src/lib.rs",
            "pub const REPORT: &str = include_str!(\"../../../REPORT.md\");\n",
        );
        let reader = r.commit("source reads report");
        r.write("REPORT.md", "unlinted input\n");
        let input = r.commit("change shared input");
        land(&reader);
        assert!(r.input(&reader).commits.contains(&input));
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

    /// The release uses the actual lint stage: raw accept is a red, and removing the call
    /// makes the same target green. No socket is opened or accepted by this fixture.
    #[test]
    fn banned_accept_is_found_by_the_preflight_lint() {
        let r = Repo::new();
        let source = aterm_tempfile::manifest_dir().join("../..");
        for name in ["trust.lock", "rust-toolchain.toml", "clippy.toml"] {
            std::fs::copy(source.join(name), r.0.path().join(name)).unwrap();
        }
        r.write("Cargo.toml", "[package]\nname = \"input-fixture\"\nversion = \"0.0.0\"\nedition = \"2024\"\n[workspace]\n");
        std::fs::create_dir(r.0.path().join("src")).unwrap();
        r.write(
            "src/lib.rs",
            "pub fn call(listener: &std::net::TcpListener) { let _ = listener.accept(); }\n",
        );
        let env = EnvSnapshot::capture();
        let tools = crate::preflight(&env, &source).unwrap();
        let scratch = aterm_tempfile::tempdir().unwrap();
        let ctx = Ctx::new_with_tools(
            r.0.path().to_path_buf(),
            crate::Scope::crate_only("input-fixture"),
            env,
            scratch.path().to_path_buf(),
            tools,
        )
        .with_checks(&[Check::Lint]);
        let stage = crate::plan::plan(&ctx)
            .into_iter()
            .find(|s| s.id == crate::plan::StageId::Tippy)
            .unwrap();
        let start = std::time::Instant::now();
        let red = crate::stages::run_stage(&ctx, &stage);
        assert!(
            red.outcomes().any(
                |(o, _)| o == crate::ladder::Outcome::Fail(crate::ladder::Severity::GateFailed)
            ),
            "{}",
            red.render()
        );
        assert!(red.render().contains("accept"), "{}", red.render());
        r.write("src/lib.rs", "pub fn call() -> u8 { 1 }\n");
        let green = crate::stages::run_stage(&ctx, &stage);
        assert!(
            green
                .outcomes()
                .all(|(o, _)| o == crate::ladder::Outcome::Ok),
            "{}",
            green.render()
        );
        println!(
            "release-input lint fixture: red_and_clean_s={:.3}",
            start.elapsed().as_secs_f64()
        );
    }

    /// Run the production preflight and Tippy scheduler across a moved tip and a product bump.
    /// The dependent must run when its dependency changes; an unrelated package must carry.
    #[test]
    fn preflight_package_receipts_follow_dependency_content_across_a_bump() {
        let r = Repo::new();
        let base = r.git(&["rev-parse", "HEAD"]);
        let source = aterm_tempfile::manifest_dir().join("../..");
        r.write(".gitignore", "/target/\n");
        for name in ["trust.lock", "rust-toolchain.toml"] {
            std::fs::copy(source.join(name), r.0.path().join(name)).unwrap();
        }
        r.write("Cargo.toml", "[workspace]\nmembers=['a','b','c','aterm']\nresolver='2'\n[workspace.package]\nversion='1.0.0'\n");
        for name in ["a", "b", "c", "aterm"] {
            std::fs::create_dir_all(r.0.path().join(name).join("src")).unwrap();
            r.write(&format!("{name}/Cargo.toml"), &format!(
                "[package]\nname='{name}'\n{}\nedition='2024'\n\
                 [package.metadata.aterm.lint-inputs]\nbounded=true\ncontent-only=true\nread-only=true\n\
                 paths=[]\nworkspace-paths=[]\ntools=[]\nvolatile-environment=[]\n{}",
                if name == "aterm" { "version.workspace=true" } else { "version='0.0.0'" },
                if name == "a" { "[dependencies]\nb={path='../b'}\n" } else { "" }
            ));
            r.write(
                &format!("{name}/src/lib.rs"),
                "pub fn value() -> u8 { 1 }\n",
            );
        }
        let env = EnvSnapshot::capture();
        let tools = crate::preflight(&env, &source).unwrap();
        let lockfile = || {
            let mut command = aterm_uds::spawnfd::command(&tools.targo);
            command
                .current_dir(r.0.path())
                .args(["generate-lockfile", "--offline"]);
            aterm_uds::deadline::output_within(&mut command, std::time::Duration::from_secs(60))
                .unwrap()
        };
        let output = lockfile();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let first = r.commit("dry tip");
        let logs = aterm_tempfile::tempdir().unwrap();
        let run = |tip: &str, previous: Option<crate::lint_content::Passed>, name: &str| {
            let events = logs.path().join(format!("{name}.events"));
            let mut output = Vec::new();
            let start = std::time::Instant::now();
            let result = judge_with_lint(
                r.0.path(),
                &base,
                tip,
                &[Check::Lint],
                Some(crate::event_log::EventLog::open(&events).unwrap()),
                &mut output,
                LintCarry {
                    passed: previous,
                    context: Some("ab".repeat(32)),
                },
            );
            assert!(
                result.is_ok(),
                "{result:?}\n{}",
                String::from_utf8_lossy(&output)
            );
            let mut passed = crate::lint_content::Passed::new();
            let mut ran = BTreeSet::new();
            for line in std::fs::read_to_string(events).unwrap().lines() {
                let Some(text) = line.strip_prefix("verify-event: ") else {
                    continue;
                };
                let row: aterm_json::Value = aterm_json::from_str(text).unwrap();
                if row["event"].as_str() != Some("lint-package-finished") {
                    continue;
                }
                let spec = row["spec"].as_str().unwrap().to_string();
                assert_eq!(row["inputs_bounded"].as_bool(), Some(true), "{row}");
                if row["carried"].as_bool() != Some(true) {
                    ran.insert(spec.clone());
                }
                passed.insert(
                    spec.clone(),
                    crate::lint_content::CarryProof {
                        schema: crate::lint_content::SCHEMA,
                        spec,
                        outcome: "ok".into(),
                        content_key: row["content_key"].as_str().map(str::to_owned),
                        inputs_bounded: true,
                        input_reason: None,
                        execution_s: row["execution_s"].as_f64().unwrap(),
                    },
                );
            }
            let wall = start.elapsed().as_secs_f64();
            println!(
                "preflight package pair {name}: wall_s={:.3}, ran={ran:?}",
                wall
            );
            assert_eq!(passed.len(), 4);
            (passed, ran, wall)
        };
        let (prior, ran, _) = run(&first, None, "dry");
        assert_eq!(ran.len(), 4);
        r.write("b/src/lib.rs", "pub fn value() -> u8 { 2 }\n");
        let moved = r.commit("change dependency");
        let (prior, ran, _) = run(&moved, Some(prior), "moved");
        assert_eq!(ran, BTreeSet::from(["a".into(), "b".into()]));
        r.write("Cargo.toml", "[workspace]\nmembers=['a','b','c','aterm']\nresolver='2'\n[workspace.package]\nversion='1.1.0'\n");
        let output = lockfile();
        assert!(output.status.success());
        let bumped = r.commit("bump product version");
        let (_, ran, _) = run(&bumped, Some(prior), "bump");
        assert_eq!(
            ran,
            BTreeSet::from(["aterm".into()]),
            "only the version-inheriting leaf reads the product version"
        );
        // ABBA uses the actual old preflight path (no package context) as its negative
        // control. The recorded walls are measurements, never latency assertions.
        for (take, carry) in [false, true, true, false].into_iter().enumerate() {
            let mut previous = None;
            let mut walls = Vec::new();
            let mut counts = Vec::new();
            for (phase, tip) in [("dry", &first), ("real", &bumped)] {
                r.git(&["checkout", "--detach", tip]);
                if carry {
                    let (passed, ran, wall) = run(tip, previous, &format!("abba-{take}-{phase}"));
                    previous = Some(passed);
                    walls.push(wall);
                    counts.push(ran.len());
                } else {
                    let start = std::time::Instant::now();
                    let mut output = Vec::new();
                    let result = judge(r.0.path(), &base, tip, &[Check::Lint], None, &mut output);
                    assert!(
                        result.is_ok(),
                        "{result:?}\n{}",
                        String::from_utf8_lossy(&output)
                    );
                    walls.push(start.elapsed().as_secs_f64());
                    counts.push(4);
                }
            }
            println!(
                "preflight ABBA {take} carry={carry}: dry_s={:.3} real_s={:.3} pair_s={:.3} packages={counts:?}",
                walls[0],
                walls[1],
                walls.iter().sum::<f64>()
            );
        }
    }
}
