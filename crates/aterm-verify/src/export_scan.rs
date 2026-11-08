// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `export-scan` check: the publication engine's content guards (forbidden content, private
//! references, gitleaks), held offline in the dev set to what the change touched, so a red is
//! found before a cut rather than in one (`docs/DESIGN-fast-build-release-2026-11-05.md`, B7).
//!
//! [`aterm_census::export_scan`] exports each file as the engine does and judges its text;
//! this stage lists the files, runs gitleaks over the exported copies with the engine's
//! command, holds the export's `.gitleaks.toml` to the engine's config policy and adjudicates
//! gitleaks' report as the engine does. A change cone scans the files it changed (the whole
//! export when it changed how every file is exported or what it is held to, [`WIDENS`]); `-p`
//! and `--all` scan the whole export. The engine still runs its own guards on every cut: this
//! finds their reds first, it does not replace them.

use std::ffi::{OsStr, OsString};
use std::path::Path;

use aterm_census::export::{ASTREAM, MANIFEST, Selection, astream_carries};
use aterm_census::export_scan::{self, Engine, Guard, rows};

use crate::exec::{self, Capture, Cmd};
use crate::ladder::Finding;
use crate::ladder::Report;
use crate::{Ctx, Scope};

/// The export's gitleaks config, which gitleaks reads from the exported tree.
pub const CONFIG: &str = ".gitleaks.toml";

/// A repository ignore file gitleaks honours at the root of the tree it scans.
pub const IGNORE: &str = ".gitleaksignore";

/// The path prefixes whose change changes how every exported file is exported or what it is
/// held to, so a cone that touches one scans the whole export: the publish folder (the
/// manifest and the export's rows among it), gitleaks' config and ignore file, and the code of
/// the export's transforms and of this scan.
pub const WIDENS: [&str; 6] = [
    "publish/",
    CONFIG,
    IGNORE,
    "crates/aterm-census/src/export",
    "crates/aterm-verify/src/export_scan",
    "crates/aterm-release/src/transforms",
];

/// Whether a change to `path` widens a cone to the whole export ([`WIDENS`]).
#[must_use]
pub fn widens(path: &str) -> bool {
    WIDENS.iter().any(|prefix| path.starts_with(prefix))
}

/// The engine's registry, relative to its checkout: its repository names, which its org
/// rewrite follows.
pub const REGISTRY: &str = "registry/repos.toml";

/// The one gitleaks rule the engine's adjudication may clear, and only where it says.
const ENTROPY_RULE: &str = "generic-api-key";

/// What gitleaks may take before the stage gives it up, as the engine's run allows.
const GITLEAKS_CEILING_S: u64 = 1800;

/// The engine half the scan reads from the engine's checkout at `dir`: its baseline and its
/// registry's repository names.
///
/// # Errors
/// The baseline or the registry is unreadable, or the registry names no repository.
pub fn engine(dir: &Path) -> Result<Engine, String> {
    let baseline_path = dir.join(rows::BASELINE);
    let baseline = crate::read_regular_text(&baseline_path)
        .map_err(|e| format!("{}: {e}", baseline_path.display()))?;
    let registry_path = dir.join(REGISTRY);
    let registry = crate::read_regular_text(&registry_path)
        .map_err(|e| format!("{}: {e}", registry_path.display()))?;
    let names =
        registry_names(&registry).map_err(|why| format!("{}: {why}", registry_path.display()))?;
    Ok(Engine {
        baseline: (baseline_path.display().to_string(), baseline),
        names,
    })
}

/// The repository names of the engine's registry (`[repos.<name>]`).
///
/// # Errors
/// The text is not TOML, or names no repository.
pub fn registry_names(text: &str) -> Result<Vec<String>, String> {
    let doc: aterm_toml::Value = aterm_toml::from_str(text).map_err(|e| e.to_string())?;
    let names: Vec<String> = doc
        .get("repos")
        .and_then(aterm_toml::Value::as_table)
        .map(|repos| repos.keys().cloned().collect())
        .unwrap_or_default();
    if names.is_empty() {
        return Err("it names no repository under [repos]".into());
    }
    Ok(names)
}

/// What the scan reads: the paths, and whether they are the whole checkout.
#[derive(Debug, PartialEq, Eq)]
pub struct Listed {
    pub paths: Vec<String>,
    /// The checkout's gitlinks among what changed, or all of them when `whole`.
    pub gitlinks: Vec<String>,
    pub whole: bool,
    /// What the paths are, in words.
    pub what: String,
}

/// Run `git` in `dir` and split its NUL-separated output.
fn git_z(dir: &Path, path_env: &OsStr, args: &[&str]) -> Result<Vec<String>, String> {
    let out = crate::changed::git(dir, path_env)
        .args(args)
        .output()
        .map_err(|e| format!("could not run `git {}`: {e}", args.join(" ")))?;
    if !out.status.success() {
        return Err(format!(
            "`git {}` failed in {}: {}",
            args.join(" "),
            dir.display(),
            String::from_utf8_lossy(&out.stderr).trim_end()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .split('\0')
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .collect())
}

/// Every file of the checkout: tracked or untracked and not ignored.
fn every_file(root: &Path, path_env: &OsStr) -> Result<Vec<String>, String> {
    let mut paths = git_z(root, path_env, &["ls-files", "-z", "--cached"])?;
    paths.extend(git_z(
        root,
        path_env,
        &["ls-files", "-z", "--others", "--exclude-standard"],
    )?);
    paths.sort();
    paths.dedup();
    Ok(paths)
}

/// The files of `HEAD` the checkout holds unchanged, with astream's in place of its gitlink
/// ([`with_astream`]): what a cut of `HEAD` writes byte for byte. The engine clears a hit in a
/// base64 copy of a binary only against the binaries the cut writes, so only these may clear
/// one here: an untracked, staged or modified file is none of them, and a gitlink whose
/// checkout differs from `HEAD`'s in any way (`--ignore-submodules=none`) carries none.
///
/// # Errors
/// git cannot list them, or astream's files cannot be read.
pub fn committed(root: &Path, path_env: &OsStr) -> Result<Vec<String>, String> {
    let changed: std::collections::BTreeSet<String> = git_z(
        root,
        path_env,
        &[
            "diff",
            "--name-only",
            "-z",
            "--ignore-submodules=none",
            "HEAD",
        ],
    )?
    .into_iter()
    .collect();
    let paths = git_z(
        root,
        path_env,
        &["ls-tree", "-r", "-z", "--name-only", "--full-tree", "HEAD"],
    )?
    .into_iter()
    .filter(|p| !changed.contains(p))
    .collect();
    with_astream(root, path_env, paths)
}

/// The checkout's gitlinks as its index records them: each one's path and the commit it pins.
fn gitlinks(root: &Path, path_env: &OsStr) -> Result<Vec<(String, String)>, String> {
    Ok(git_z(root, path_env, &["ls-files", "-z", "--stage"])?
        .into_iter()
        .filter_map(|entry| {
            let (meta, path) = entry.split_once('\t')?;
            let mut fields = meta.split(' ');
            let (mode, commit) = (fields.next()?, fields.next()?);
            (mode == "160000").then(|| (path.to_string(), commit.to_string()))
        })
        .collect())
}

/// That astream's checkout holds `pin`, the commit the checkout's gitlink records: the export
/// carries astream's files at the gitlink, and the scan reads them from the checkout.
///
/// # Errors
/// astream is not checked out, or its checkout holds another commit.
fn astream_pinned(root: &Path, path_env: &OsStr, pin: &str) -> Result<(), String> {
    let sub = root.join(ASTREAM);
    let out = crate::changed::git(&sub, path_env)
        .args(["rev-parse", "--show-toplevel", "HEAD"])
        .stderr(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("could not run git in {ASTREAM}: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = text.lines();
    let (top, head) = (lines.next().unwrap_or(""), lines.next().unwrap_or(""));
    let own = out.status.success()
        && match (Path::new(top).canonicalize(), sub.canonicalize()) {
            (Ok(top), Ok(sub)) => top == sub,
            _ => false,
        };
    if !own {
        return Err(format!(
            "{ASTREAM} is not checked out, so the files the export carries from it cannot be \
             read (fix: git submodule update --init {ASTREAM})"
        ));
    }
    if head != pin {
        let short = |c: &str| c[..c.len().min(12)].to_string();
        return Err(format!(
            "{ASTREAM}'s checkout holds {}, but the checkout's gitlink pins {}, which is what the \
             export carries (fix: git submodule update --init {ASTREAM}, or git add {ASTREAM} \
             to pin what is checked out)",
            short(head),
            short(pin)
        ));
    }
    Ok(())
}

/// `paths` with the `vendor/astream` gitlink, where it is one of them, replaced by the files the
/// export carries in its place: astream's root `Cargo.toml` and every file, outside its target
/// directories, of each astream crate the exported manifests reach ([`astream_closure`]).
///
/// # Errors
/// The gitlink is listed but astream is not checked out, or its crates cannot be derived.
pub fn with_astream(
    root: &Path,
    path_env: &OsStr,
    mut paths: Vec<String>,
) -> Result<Vec<String>, String> {
    if !paths.iter().any(|p| p == ASTREAM) {
        return Ok(paths);
    }
    paths.retain(|p| p != ASTREAM);
    let sub = root.join(ASTREAM);
    let not_checked_out = || {
        format!(
            "{ASTREAM} is not checked out, so the files the export carries from it cannot be \
             read (fix: git submodule update --init {ASTREAM})"
        )
    };
    let files = git_z(&sub, path_env, &["ls-files", "-z", "--cached"])
        .ok()
        .filter(|files| !files.is_empty())
        .ok_or_else(not_checked_out)?;
    let Some(crates) = astream_crates(root, path_env)? else {
        return Ok(paths);
    };
    paths.extend(
        files
            .into_iter()
            .filter(|p| {
                astream_carries(p)
                    && (p == "Cargo.toml"
                        || p.strip_prefix("crates/")
                            .and_then(|rest| rest.split_once('/'))
                            .is_some_and(|(krate, _)| crates.iter().any(|c| c == krate)))
            })
            .map(|p| format!("{ASTREAM}/{p}")),
    );
    paths.sort();
    paths.dedup();
    Ok(paths)
}

/// The astream crates the checkout's exported manifests reach, at the commit astream's checkout
/// holds; `None` when the export's manifest does not parse, which the scan reports.
fn astream_crates(root: &Path, path_env: &OsStr) -> Result<Option<Vec<String>>, String> {
    let selection = match crate::read_regular_text(root.join(MANIFEST)) {
        Ok(text) => match Selection::parse(&text) {
            Ok(selection) => selection,
            Err(_) => return Ok(None),
        },
        Err(_) => Selection::everything(),
    };
    let exported: Vec<(String, String)> = every_file(root, path_env)?
        .into_iter()
        .filter(|p| {
            (p == "Cargo.toml" || p.ends_with("/Cargo.toml"))
                && !p.starts_with(&format!("{ASTREAM}/"))
                // The transforms' manifest walk skips these directories.
                && !p.split('/').any(|c| c == "target" || c == ".git")
                && selection.selects(p)
        })
        .filter_map(|p| crate::read_regular_text(root.join(&p)).ok().map(|t| (p, t)))
        .collect();
    let sub = root.join(ASTREAM);
    let at_head = |path: &str| {
        let out = crate::changed::git(&sub, path_env)
            .args(["show", &format!("HEAD:{path}")])
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    };
    astream_closure(&exported, &at_head, "HEAD").map(Some)
}

/// What `scope` scans: the cone's changed files, or the whole checkout when the selection is
/// `-p` or `--all` or the cone changed a path that [`widens`] it, with the gitlinks among them.
///
/// # Errors
/// git cannot list the files, or astream's checkout is not the commit its gitlink pins.
pub fn listed(root: &Path, path_env: &OsStr, scope: &Scope) -> Result<Listed, String> {
    let links = gitlinks(root, path_env)?;
    let has_astream = match links.iter().find(|(path, _)| path == ASTREAM) {
        Some((_, pin)) => {
            astream_pinned(root, path_env, pin)?;
            true
        }
        None => false,
    };
    let (mut paths, whole, what) = match scope {
        Scope::Changed(changed) => {
            let paths = crate::changed::changed_since(root, path_env, &changed.base)?;
            match paths.iter().find(|p| widens(p)) {
                Some(path) => (
                    every_file(root, path_env)?,
                    true,
                    format!("the whole export ({path} changed)"),
                ),
                None => {
                    let what =
                        format!("the {} file(s) changed since {}", paths.len(), changed.base);
                    (paths, false, what)
                }
            }
        }
        _ => (every_file(root, path_env)?, true, "the whole export".into()),
    };
    let gitlinks = links
        .into_iter()
        .map(|(path, _)| path)
        .filter(|path| whole || paths.contains(path))
        .collect();
    // A manifest can change which astream crates the export carries.
    if !whole
        && has_astream
        && !paths.iter().any(|p| p == ASTREAM)
        && paths
            .iter()
            .any(|p| p == "Cargo.toml" || p.ends_with("/Cargo.toml"))
    {
        paths.push(ASTREAM.to_string());
    }
    let mut paths = with_astream(root, path_env, paths)?;
    if !whole && !paths.iter().any(|p| p == CONFIG) {
        paths.push(CONFIG.to_string());
    }
    Ok(Listed {
        paths,
        gitlinks,
        whole,
        what,
    })
}

/// Python's truthiness of a TOML value, as the engine's `if value:` reads it.
fn truthy(value: &aterm_toml::Value) -> bool {
    use aterm_toml::Value;
    match value {
        Value::String(s) => !s.is_empty(),
        Value::Integer(i) => *i != 0,
        Value::Float(f) => *f != 0.0,
        Value::Boolean(b) => *b,
        Value::Array(a) => !a.is_empty(),
        Value::Table(t) => !t.is_empty(),
        Value::Datetime(_) => true,
    }
}

/// Whether any table in `value` sets `regexTarget = "line"`.
fn line_target(value: &aterm_toml::Value) -> bool {
    use aterm_toml::Value;
    match value {
        Value::Table(t) => {
            t.get("regexTarget").and_then(Value::as_str) == Some("line")
                || t.values().any(line_target)
        }
        Value::Array(a) => a.iter().any(line_target),
        _ => false,
    }
}

/// What the engine's `check_gitleaks_config` refuses in the exported config `text`
/// (`None`: the export carries none), with `ignore` when the export carries a root
/// [`IGNORE`]: each refusal, in the engine's order.
#[must_use]
pub fn config_refusals(text: Option<&str>, ignore: bool) -> Vec<String> {
    use aterm_toml::Value;
    let Some(text) = text else {
        return vec![format!(
            "the export carries no {CONFIG} (select it in {MANIFEST})"
        )];
    };
    let mut refused = Vec::new();
    match aterm_toml::from_str::<Value>(text) {
        Err(e) => refused.push(format!("it does not parse ({e})")),
        Ok(cfg) => {
            let extend = cfg.get("extend").and_then(Value::as_table);
            if extend.and_then(|t| t.get("useDefault")) != Some(&Value::Boolean(true)) {
                refused.push(
                    "it must set [extend] useDefault = true so the standard rules stay on".into(),
                );
            }
            let set = |key: &str| extend.and_then(|t| t.get(key)).is_some_and(truthy);
            if set("disabledRules") {
                refused.push("it disables standard rules".into());
            }
            if set("path") || set("url") {
                refused.push("it extends a config the engine cannot validate".into());
            }
            if cfg.get("allowlist").is_some() || cfg.get("allowlists").is_some() {
                refused.push("it defines a repository-wide allowlist".into());
            }
            match cfg.get("rules") {
                None => {}
                Some(Value::Array(rules)) if rules.iter().all(|r| r.as_table().is_some()) => {
                    for rule in rules.iter().filter_map(Value::as_table) {
                        let id = rule.get("id").and_then(Value::as_str);
                        if !id.is_some_and(|id| id.starts_with("repo-extra-")) {
                            refused.push(
                                "a custom rule's id does not start with `repo-extra-`, so it \
                                 may shadow a standard rule"
                                    .into(),
                            );
                        }
                        if rule.get("allowlist").is_some() || rule.get("allowlists").is_some() {
                            refused.push("a custom rule defines an allowlist".into());
                        }
                    }
                }
                Some(_) => refused.push("its custom rules are not a list of tables".into()),
            }
            if line_target(&cfg) {
                refused.push("it sets regexTarget = \"line\"".into());
            }
        }
    }
    if ignore {
        refused.push(format!(
            "the export carries a root {IGNORE}, which gitleaks honours; tag the line \
             gitleaks:allow instead"
        ));
    }
    refused
}

/// The engine's gitleaks command over the exported `tree`, its report at `report`.
#[must_use]
pub fn gitleaks_cmd(gitleaks: &Path, tree: &Path, report: &Path, ignore: &Path) -> Cmd {
    Cmd::new(gitleaks)
        .args([
            OsString::from("detect"),
            "--no-git".into(),
            "--redact=100".into(),
            "--gitleaks-ignore-path".into(),
            ignore.into(),
            "--source".into(),
            tree.into(),
            "-c".into(),
            tree.join(CONFIG).into(),
            "--report-path".into(),
            report.into(),
        ])
        .capture(Capture::Silent)
}

/// One gitleaks finding that stands: its file (relative to the scanned tree), line and rule.
#[derive(Debug, PartialEq, Eq)]
pub struct Leak {
    pub path: String,
    pub line: i64,
    pub rule: String,
}

/// The dependency name a `Cargo.lock` line holds when it is one entry of a `dependencies`
/// list (the engine's `^\s*"([^"]+)",?\s*$`), its first word.
fn lock_entry(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix('"')?;
    let end = rest.find('"')?;
    let after = &rest[end + 1..];
    let after = after.strip_prefix(',').unwrap_or(after);
    (end > 0 && after.trim().is_empty()).then(|| rest[..end].split(' ').next().unwrap_or(""))
}

/// Whether lines `start..=end` (1-based, as gitleaks numbers by `\n` alone) of the lockfile
/// `text` are each one dependency entry naming a package the same lockfile declares.
fn lock_dependency_region(text: &str, start: i64, end: i64) -> bool {
    let names: Vec<String> = aterm_toml::from_str::<aterm_toml::Value>(text)
        .ok()
        .and_then(|doc| doc.get("package").and_then(|p| p.as_array()).cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|p| p.get("name").and_then(aterm_toml::Value::as_str))
        .map(str::to_owned)
        .collect();
    let lines: Vec<&str> = text.split('\n').collect();
    let (Ok(start), Ok(end)) = (usize::try_from(start), usize::try_from(end)) else {
        return false;
    };
    !names.is_empty()
        && 1 <= start
        && start <= end
        && end <= lines.len()
        && lines[start - 1..end]
            .iter()
            .all(|l| lock_entry(l).is_some_and(|n| names.iter().any(|m| m == n)))
}

/// gitleaks' `report` over the exported `tree`, adjudicated as the engine does: a
/// [`ENTROPY_RULE`] finding in a `Cargo.lock` that covers only dependency names that lockfile
/// declares is cleared, and every other finding stands. (The engine's other two clearances,
/// cargo-vendor trees and suite plans, find nothing to clear in aterm's export, which has
/// neither, so a finding there stands here.)
///
/// # Errors
/// The report is not a JSON list, so every finding stands unread.
pub fn adjudicate(report: &str, tree: &Path) -> Result<Vec<Leak>, String> {
    let found: aterm_json::Value = aterm_json::from_str(report).map_err(|e| e.to_string())?;
    let found = found.as_array().ok_or("the report is not a list")?;
    let mut kept = Vec::new();
    for leak in found {
        let field = |k: &str| {
            leak.get(k)
                .and_then(aterm_json::Value::as_str)
                .unwrap_or("")
        };
        let line = |k: &str| leak.get(k).and_then(aterm_json::Value::as_i64).unwrap_or(0);
        let file = Path::new(field("File"));
        let path = file
            .strip_prefix(tree)
            .unwrap_or(file)
            .to_string_lossy()
            .into_owned();
        let rule = field("RuleID").to_string();
        let cleared = rule == ENTROPY_RULE
            && file.starts_with(tree)
            && file.file_name() == Some(OsStr::new("Cargo.lock"))
            && crate::read_regular_text(file).is_ok_and(|text| {
                lock_dependency_region(&text, line("StartLine"), line("EndLine"))
            });
        if !cleared {
            kept.push(Leak {
                path,
                line: line("StartLine"),
                rule,
            });
        }
    }
    Ok(kept)
}

fn finding(guard: Guard, path: &str, line: usize, detail: &str) -> Finding {
    Finding {
        id: if line == 0 {
            path.to_string()
        } else {
            format!("{path}:{line}")
        },
        rerun: None,
        message: vec![format!("{}: {detail}", guard.name())],
    }
}

/// The stage. What it cannot run for want of gitleaks or the engine's checkout is a COULD NOT
/// RUN row beside the verdict of what it could: without the engine, the export's own rows
/// still hold.
pub fn run(ctx: &Ctx, r: &mut Report) {
    run_with(ctx, r, || {
        atpkg::prerequisite::configured(
            &[atpkg::prerequisite::Prerequisite::Gitleaks],
            "verify export-scan",
        )
        .map(Scanner::Signed)
    });
}

enum Scanner {
    Signed(atpkg::prerequisite::HeldSet),
    // Content fixtures exercise the host's scanner without reopening the live
    // package source inside an isolated release-test environment.
    #[cfg(test)]
    Fixture(std::path::PathBuf),
}

impl Scanner {
    fn command(&self, tree: &Path, report: &Path, ignore: &Path) -> Result<Cmd, String> {
        match self {
            Self::Signed(held) => {
                let tool = held
                    .tool(atpkg::prerequisite::Prerequisite::Gitleaks)
                    .ok_or("the operation did not acquire its scanner")?;
                tool.validate()?;
                let mut command = gitleaks_cmd(tool.path(), tree, report, ignore);
                for (name, value) in tool.environment().entries() {
                    command = command.env(name, value);
                }
                Ok(command)
            }
            #[cfg(test)]
            Self::Fixture(path) => Ok(gitleaks_cmd(path, tree, report, ignore)),
        }
    }
}

fn run_with(ctx: &Ctx, r: &mut Report, acquire: impl FnOnce() -> Result<Scanner, String>) {
    let mut not_run = Vec::new();
    let gitleaks = match acquire() {
        Ok(held) => Some(held),
        Err(absent) => {
            if let Some(transport) = atpkg::prerequisite::freshness_transport(&absent) {
                not_run.push(format!(
                    "export-scan — signed gitleaks channel freshness COULD NOT RUN: \
                     {transport}, so the scanner did not run (retry when the channel transport \
                     answers)"
                ));
            } else {
                not_run.push(format!("export-scan — signed gitleaks: {absent}, so it did not run (fix: `aterm pkg install gitleaks`)"));
            }
            None
        }
    };
    let engine = match ctx.env.publication_engine.as_deref() {
        None => Err(
            "neither PUBLICATION_ENGINE nor HOME names the publication engine's checkout"
                .to_string(),
        ),
        Some(dir) => engine(dir)
            .map_err(|why| format!("the publication engine's checkout cannot be read ({why})")),
    };
    if let Err(why) = &engine {
        not_run.push(format!(
            "export-scan — {why}, so neither its baseline nor private references were judged \
             (fix: clone it to ~/publication or set PUBLICATION_ENGINE)"
        ));
    }
    let listed = match listed(&ctx.root, &ctx.path_env, &ctx.scope) {
        Ok(listed) => listed,
        Err(why) => {
            r.cannot_run(format!("export-scan — {why}"));
            return;
        }
    };
    let work = ctx.scratch.join("export-scan.noindex");
    let _ = std::fs::remove_dir_all(&work);
    // Canonical, so the paths gitleaks reports start with it.
    let tree = match std::fs::create_dir_all(work.join("tree"))
        .and_then(|()| work.join("tree").canonicalize())
    {
        Ok(tree) => tree,
        Err(e) => {
            r.cannot_run(format!("export-scan — {}: {e}", work.display()));
            return;
        }
    };
    // A hit may be cleared by a binary the change did not touch, so the binaries are read from
    // every file a cut of HEAD writes; unlisted, none is, and every hit stands.
    let export = || committed(&ctx.root, &ctx.path_env).unwrap_or_default();
    let listing = export_scan::Listing {
        root: &ctx.root,
        paths: &listed.paths,
        gitlinks: &listed.gitlinks,
        whole: listed.whole,
        export: &export,
    };
    let scanned = export_scan::scan(&listing, engine.as_ref().ok(), Some(&tree));
    let mut findings: Vec<Finding> = scanned
        .findings
        .iter()
        .map(|f| finding(f.guard, &f.path, f.line, &f.detail))
        .collect();
    let config = crate::read_regular_text(tree.join(CONFIG)).ok();
    let ignore = tree.join(IGNORE).symlink_metadata().is_ok();
    findings.extend(
        config_refusals(config.as_deref(), ignore)
            .iter()
            .map(|why| finding(Guard::Gitleaks, CONFIG, 0, why)),
    );
    if let Some(gitleaks) = &gitleaks
        && config.is_some()
    {
        match gitleaks_leaks(ctx, gitleaks, &work, &tree) {
            Ok(leaks) => findings.extend(leaks),
            Err(why) => not_run.push(format!("export-scan — {why}")),
        }
    }
    let _ = std::fs::remove_dir_all(&work);
    let held = if engine.is_ok() {
        "the engine's content guards"
    } else {
        "the export's own rows"
    };
    let mut label = format!(
        "export-scan: {} exported file(s) of {} held to {held}",
        scanned.files, listed.what
    );
    if scanned.cleared > 0 {
        label.push_str(&format!(
            ", {} hit(s) cleared as base64 copies of exported binaries",
            scanned.cleared
        ));
    }
    if findings.is_empty() {
        r.pass(label);
    } else {
        r.fail_with_findings(
            format!(
                "{label}: {} finding(s), each a GUARD FAIL in the next cut",
                findings.len()
            ),
            findings,
        );
    }
    for why in not_run {
        r.cannot_run(why);
    }
}

/// gitleaks over the exported `tree`, its report adjudicated: the findings that stand.
fn gitleaks_leaks(
    ctx: &Ctx,
    scanner: &Scanner,
    work: &Path,
    tree: &Path,
) -> Result<Vec<Finding>, String> {
    let report = work.join("gitleaks.json");
    let ignore = work.join("empty-gitleaksignore");
    std::fs::write(&ignore, b"").map_err(|e| format!("{}: {e}", ignore.display()))?;
    let mut env = ctx.exec_env();
    env.child_ceiling = Some(std::time::Duration::from_secs(GITLEAKS_CEILING_S));
    let command = scanner.command(tree, &report, &ignore)?;
    let run = exec::run(&command, env);
    if let Some(e) = &run.spawn_error {
        return Err(format!("gitleaks did not start: {e}"));
    }
    let code = match run.code {
        Some(0) => return Ok(Vec::new()),
        None => return Err("gitleaks was ended before it finished".into()),
        Some(code) => code,
    };
    let Ok(text) = crate::read_regular_text(&report) else {
        return Ok(vec![finding(
            Guard::Gitleaks,
            CONFIG,
            0,
            &format!(
                "gitleaks exited {code} without writing its report: it never scanned (check \
                 that {CONFIG} compiles)"
            ),
        )]);
    };
    Ok(match adjudicate(&text, tree) {
        Ok(kept) => kept
            .iter()
            .map(|leak| {
                finding(
                    Guard::Gitleaks,
                    &leak.path,
                    usize::try_from(leak.line).unwrap_or(0),
                    &format!(
                        "{} (redacted); make the fixture without a secret's shape, or tag the \
                         line gitleaks:allow",
                        leak.rule
                    ),
                )
            })
            .collect(),
        Err(why) => vec![finding(
            Guard::Gitleaks,
            CONFIG,
            0,
            &format!(
                "gitleaks exited {code} and its report is unreadable ({why}): every finding stands"
            ),
        )],
    })
}

/// A manifest's dependency rows that cargo follows: its `[dependencies]` and
/// `[build-dependencies]`, target-specific and `[workspace.dependencies]` ones included,
/// optional ones too (a feature can turn any on), and with `dev` its
/// `[dev-dependencies]`.
fn dependency_rows(manifest: &aterm_toml::Value, dev: bool) -> Vec<(String, Option<String>, bool)> {
    use aterm_toml::Value;
    const KINDS: [&str; 5] = [
        "dependencies",
        "build-dependencies",
        "build_dependencies",
        "dev-dependencies",
        "dev_dependencies",
    ];
    let kinds = if dev { &KINDS[..] } else { &KINDS[..3] };
    let Some(root) = manifest.as_table() else {
        return Vec::new();
    };
    let mut scopes = vec![root];
    scopes.extend(root.get("workspace").and_then(Value::as_table));
    if let Some(targets) = root.get("target").and_then(Value::as_table) {
        scopes.extend(targets.values().filter_map(Value::as_table));
    }
    let mut rows = Vec::new();
    for scope in scopes {
        for &kind in kinds {
            let Some(table) = scope.get(kind).and_then(Value::as_table) else {
                continue;
            };
            for (name, spec) in table.iter() {
                let path = spec.get("path").and_then(Value::as_str).map(str::to_string);
                let workspace = spec.get("workspace").and_then(Value::as_bool) == Some(true);
                rows.push((name.clone(), path, workspace));
            }
        }
    }
    rows
}

/// `base` joined with the relative `path`, `.` and `..` resolved lexically.
fn join_lexically(base: &str, path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in base.split('/').chain(path.split('/')) {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    parts.join("/")
}

/// The sorted closure of the `vendor/astream/crates/*` path dependencies the exported
/// `manifests` (each its path in the export, `/`-separated, and its text) reach in astream at
/// one commit, whose files `at_rev` reads by path and `short` names: the astream crates the
/// export carries. The export's transforms write these; the export scan reads them.
///
/// # Errors
/// Returns an error if a manifest does not parse, no exported manifest reaches astream (the
/// scan is wrong), a crate the export reaches is not at the commit, or an astream crate
/// path-depends outside astream's `crates/`.
pub fn astream_closure(
    manifests: &[(String, String)],
    at_rev: &dyn Fn(&str) -> Option<String>,
    short: &str,
) -> Result<Vec<String>, String> {
    let parse = |text: &str, what: &str| {
        aterm_toml::from_str::<aterm_toml::Value>(text).map_err(|e| format!("{what}: {e}"))
    };
    let mut reached: std::collections::BTreeMap<String, String> = Default::default();
    for (rel_manifest, text) in manifests {
        let rel_dir = rel_manifest.rsplit_once('/').map_or("", |(dir, _)| dir);
        let rows = dependency_rows(&parse(text, rel_manifest)?, true);
        for (_, path, _) in rows {
            let Some(path) = path else { continue };
            let target = join_lexically(rel_dir, &path);
            if let Some(name) = target.strip_prefix("vendor/astream/crates/")
                && !name.is_empty()
                && !name.contains('/')
            {
                reached
                    .entry(name.to_string())
                    .or_insert_with(|| rel_manifest.clone());
            }
        }
    }
    if reached.is_empty() {
        return Err(
            "no exported Cargo.toml path-depends on vendor/astream/crates/<crate>, yet \
                    vendor/astream is a gitlink: the dependency scan is wrong"
                .into(),
        );
    }
    let root =
        at_rev("Cargo.toml").ok_or_else(|| format!("astream {short} has no root Cargo.toml"))?;
    let workspace: std::collections::BTreeMap<String, String> =
        dependency_rows(&parse(&root, "astream's root Cargo.toml")?, false)
            .into_iter()
            .filter_map(|(name, path, _)| Some((name, path?)))
            .collect();
    let mut todo: Vec<String> = reached.keys().cloned().collect();
    let mut done = std::collections::BTreeSet::new();
    while let Some(krate) = todo.pop() {
        if !done.insert(krate.clone()) {
            continue;
        }
        let Some(manifest) = at_rev(&format!("crates/{krate}/Cargo.toml")) else {
            return Err(format!(
                "{} path-depends on vendor/astream/crates/{krate}, which astream {short} does \
                 not have",
                reached[&krate]
            ));
        };
        let own = format!("crates/{krate}");
        for (name, path, in_workspace) in dependency_rows(&parse(&manifest, &own)?, false) {
            let target = match (path, in_workspace) {
                (Some(p), _) => join_lexically(&own, &p),
                (None, true) => match workspace.get(&name) {
                    Some(p) => join_lexically("", p),
                    None => continue,
                },
                (None, false) => continue,
            };
            let Some(dep) = target
                .strip_prefix("crates/")
                .filter(|d| !d.is_empty() && !d.contains('/'))
            else {
                return Err(format!(
                    "astream {own}/Cargo.toml depends on {name} at {target}, which is not an \
                     astream crate the export can carry"
                ));
            };
            reached
                .entry(dep.to_string())
                .or_insert_with(|| format!("astream {own}/Cargo.toml"));
            todo.push(dep.to_string());
        }
    }
    Ok(done.into_iter().collect())
}

#[cfg(test)]
#[path = "export_scan_tests.rs"]
mod tests;
