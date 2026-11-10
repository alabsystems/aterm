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
    let out = aterm_uds::deadline::output_within(&mut command, std::time::Duration::from_secs(60))
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

fn unchecked(nodes: &BTreeMap<String, Commit>, composed: &BTreeSet<String>) -> Vec<String> {
    let mut covered = BTreeSet::new();
    for (landing, commit) in nodes {
        let Some(receipt) = &commit.receipt else {
            continue;
        };
        if commit.parents.len() != 2 {
            continue;
        }
        let reachable = ancestors(nodes, landing);
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
                    (c.tree == checked.tree && reachable.contains(id)).then_some(id.as_str())
                })
            };
            let Some(target) = target else { continue };
            let judged = ancestors(nodes, target);
            if !judged.contains(&checked.base) {
                continue;
            }
            covered.extend(judged.difference(&ancestors(nodes, &checked.base)).cloned());
            valid = true;
        }
        if valid
            && (receipt
                .checked
                .last()
                .is_some_and(|c| c.tree == commit.tree)
                || composed.contains(landing))
        {
            covered.insert(landing.clone());
        }
    }
    nodes
        .keys()
        .filter(|id| !covered.contains(*id))
        .cloned()
        .collect()
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
        let (Some(onto), Some(checked)) = (&receipt.composed_onto, receipt.checked.last()) else {
            continue;
        };
        let reachable = ancestors(&nodes, id);
        if !commit.parents.contains(onto)
            || !reachable.contains(&checked.base)
            || !nodes
                .iter()
                .any(|(id, c)| c.tree == checked.tree && reachable.contains(id))
        {
            continue;
        }
        let imported = git(
            root,
            path,
            &[
                "diff",
                "--name-only",
                "-z",
                &checked.tree,
                &commit.tree,
                "--",
            ],
        )?;
        let own = git(
            root,
            path,
            &[
                "diff",
                "--name-only",
                "-z",
                &checked.base,
                &checked.tree,
                "--",
            ],
        )?;
        let imported: Vec<_> = imported.split('\0').filter(|p| !p.is_empty()).collect();
        if imported
            .iter()
            .any(|p| own.split('\0').any(|own| own == *p))
        {
            continue;
        }
        let mut args = vec!["diff", "--name-only", "-z", onto, &commit.tree, "--"];
        args.extend(imported);
        if git(root, path, &args)?.is_empty() {
            composed.insert(id.clone());
        }
    }
    let commits = unchecked(&nodes, &composed);
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
        crate::changed::resolve_paths(root, &tools, &env.path, base, input.paths.clone());
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
    for pair in args[2..].chunks(2) {
        if pair.len() != 2 {
            return crate::exit::USAGE;
        }
        match pair[0].as_str() {
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
    let code = match judge(
        root,
        &args[1],
        &tip,
        &checks,
        events,
        &mut std::io::stdout(),
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
}
