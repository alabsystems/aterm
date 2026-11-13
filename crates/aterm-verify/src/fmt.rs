// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `verify fmt [--fix]`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::Ctx;
use crate::exec::{self, Cmd};
use crate::ladder::{Finding, Report};

/// The formatter, beside `targo` in the stage2 bin directory, run by path.
pub const TRUSTFMT: &str = "trustfmt";

/// Configuration names in the pinned formatter's order of preference.
pub const CONFIG_FILES: [&str; 4] = [
    ".trustfmt.toml",
    "trustfmt.toml",
    ".rustfmt.toml",
    "rustfmt.toml",
];

/// Tracked sources whose layout is their content, never handed to the formatter.
pub const EXCLUDED: [&str; 6] = [
    "crates/aterm-core/tests/support/replay_corpus_data.rs",
    "crates/aterm-effects/src/animal_glyphs_gen.rs",
    "crates/aterm-effects/src/cat_glyphs_gen.rs",
    "crates/aterm-effects/src/dog_glyphs_gen.rs",
    "crates/aterm-effects/src/pet_glyphs_gen.rs",
    "crates/aterm-gui/src/config_watcher_protocol_island.rs",
];

const DIFF_MARKER: &str = "Diff in ";

const CHUNK: usize = 48;

/// A file the formatter would change, and the edition it was judged at.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Drift {
    pub path: String,
    pub edition: String,
}

/// A directory whose `rustfmt.toml` edition is not its manifest's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditionGap {
    /// The manifest's directory, repo-relative (`""` for the root).
    pub dir: String,
    pub edition: String,
    /// One file the gap reaches.
    pub example: String,
}

/// What the formatter pass decided.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sweep {
    /// Files handed to the formatter.
    pub files: usize,
    pub drifted: Vec<Drift>,
    /// The formatter's own `error` lines for files it could not read.
    pub unread: Vec<String>,
}

/// The `fmt` check's stage.
pub fn run(ctx: &Ctx, r: &mut Report) {
    let Some(bin) = ctx.tools.trustfmt.clone() else {
        r.skip(ctx.tools.missing_trustfmt_label());
        return;
    };
    let files = match rs_files(ctx) {
        Ok(f) if !f.is_empty() => f,
        Ok(_) if ctx.input_paths.is_some() => {
            r.pass("trustfmt: no unchecked Rust files");
            return;
        }
        Ok(_) => {
            r.cannot_run("trustfmt — `git ls-files` named no `.rs` file");
            return;
        }
        Err(why) => {
            r.cannot_run(format!("trustfmt — {why}"));
            return;
        }
    };
    let mut editions = Editions::new(&ctx.root);
    let plans = if ctx.input_paths.is_some() && ctx.carry_context.is_some() {
        input_plans(ctx, &files, &mut editions)
    } else {
        Vec::new()
    };
    let carried: BTreeSet<_> = plans
        .iter()
        .filter(|plan| {
            plan.proof
                .as_ref()
                .is_some_and(|p| crate::input_content::exact(ctx, p))
        })
        .flat_map(|plan| plan.files.iter().cloned())
        .collect();
    let files: Vec<_> = files
        .into_iter()
        .filter(|file| !carried.contains(file))
        .collect();
    let groups = by_edition(&files, &mut editions);
    let found = sweep(ctx, &bin, &groups, false);
    record_sweep(ctx, r, &bin, &found);
    record_editions(r, &edition_gaps(&ctx.root, &files, &mut editions));
    if !carried.is_empty() {
        r.pass(format!(
            "trustfmt: {} files reused by package input key",
            carried.len()
        ));
    }
    if r.outcomes()
        .all(|(outcome, _)| outcome == crate::ladder::Outcome::Ok)
    {
        for proof in plans.into_iter().filter_map(|plan| plan.proof) {
            if proof.reusable(ctx) {
                let reused = crate::input_content::exact(ctx, &proof);
                r.raw(crate::input_content::event(ctx, &proof, reused));
            } else {
                r.cannot_run("formatter package inputs changed during the run");
            }
        }
    }
}

struct InputPlan {
    files: Vec<String>,
    proof: Option<crate::input_content::Proof>,
}

fn input_plans(ctx: &Ctx, files: &[String], editions: &mut Editions) -> Vec<InputPlan> {
    let mut packages: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for file in files {
        let item = format!(
            "{}:edition={}",
            manifest_dir_of(&ctx.root, file),
            editions.manifest(file)
        );
        packages.entry(item).or_default().push(file.clone());
    }
    packages
        .into_iter()
        .map(|(item, files)| {
            let mut paths: BTreeSet<String> = BTreeSet::new();
            for file in &files {
                let mut directory = Path::new(file).parent();
                while let Some(dir) = directory {
                    for name in CONFIG_FILES {
                        paths.insert(dir.join(name).to_string_lossy().into_owned());
                    }
                    directory = dir.parent();
                }
            }
            let inputs: Option<Vec<_>> = paths
                .iter()
                .map(|path| aterm_census::inputs::file(&ctx.root, path))
                .collect();
            let proof = inputs.and_then(|mut inputs| {
                let physical = ctx.root.canonicalize().ok()?;
                if files.iter().any(|file| {
                    ctx.root.join(file).canonicalize().ok().as_ref() != Some(&physical.join(file))
                }) {
                    return None;
                }
                // --skip-children makes each selected source an independent reader. The
                // edition is in the item, and configuration candidates are above; unrelated
                // package assets, manifests and documents cannot affect this invocation.
                for file in &files {
                    inputs.push(aterm_census::inputs::file(&ctx.root, file)?);
                }
                let mut selection = aterm_digest::Sha256::new();
                for file in &files {
                    selection.update((file.len() as u64).to_le_bytes());
                    selection.update(file.as_bytes());
                }
                let item = format!(
                    "{item}:{}",
                    aterm_codec::hex::encode_lower(&selection.finalize())
                );
                crate::input_content::Proof::new(ctx, "fmt", &item, inputs)
            });
            InputPlan { files, proof }
        })
        .collect()
}

fn record_sweep(ctx: &Ctx, r: &mut Report, bin: &Path, found: &Sweep) {
    if !found.unread.is_empty() {
        r.raw(found.unread.join("\n"));
        // What the run's first red kept from being read says so, as the halt's rows do.
        let halted = ctx
            .halted()
            .filter(|words| found.unread.iter().all(|line| line.contains(words)))
            .map_or_else(String::new, |words| format!(" — {words}"));
        r.cannot_run(format!(
            "trustfmt could not read every file: {} error(s) (above){halted}",
            found.unread.len()
        ));
    }
    if found.drifted.is_empty() {
        if found.unread.is_empty() {
            r.pass(format!(
                "trustfmt: {} files formatted{}",
                found.files,
                borrowed_suffix(ctx, TRUSTFMT)
            ));
        }
        return;
    }
    if ctx.fix {
        let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for d in &found.drifted {
            groups
                .entry(d.edition.clone())
                .or_default()
                .push(d.path.clone());
        }
        let groups: Vec<(String, Vec<String>)> = groups.into_iter().collect();
        let fixed = sweep(ctx, bin, &groups, true);
        let still: Vec<&str> = fixed.unread.iter().map(String::as_str).collect();
        r.raw(
            found
                .drifted
                .iter()
                .map(|d| format!("reformatted {} (edition {})", d.path, d.edition))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        if still.is_empty() {
            r.pass(format!(
                "trustfmt --fix: {} of {} files reformatted",
                found.drifted.len(),
                found.files
            ));
        } else {
            r.raw(still.join("\n"));
            r.cannot_run("trustfmt --fix could not write every file (above)");
        }
        return;
    }
    let findings = found
        .drifted
        .iter()
        .map(|d| Finding {
            id: d.path.clone(),
            rerun: None,
            message: vec![format!("unformatted at edition {}", d.edition)],
        })
        .collect();
    r.fail_with_findings(
        format!(
            "trustfmt: {} of {} files unformatted (fix: targo --unverified verify fmt --fix)",
            found.drifted.len(),
            found.files
        ),
        findings,
    );
}

/// ` (the managed Trust build's <file>)` when the run took `name` from the managed Trust build.
pub(crate) fn borrowed_suffix(ctx: &Ctx, name: &str) -> String {
    ctx.tools
        .borrowed(name)
        .map(|file| format!(" (the managed Trust build's {})", file.display()))
        .unwrap_or_default()
}

fn record_editions(r: &mut Report, gaps: &[EditionGap]) {
    if gaps.is_empty() {
        r.pass("rustfmt.toml editions match the manifests");
        return;
    }
    let findings = gaps
        .iter()
        .map(|g| {
            let at = if g.dir.is_empty() {
                "rustfmt.toml".to_string()
            } else {
                format!("{}/rustfmt.toml", g.dir)
            };
            Finding {
                id: at.clone(),
                rerun: None,
                message: vec![format!(
                    "{}: the manifest says edition {} — write `edition = \"{}\"` in {at}",
                    g.example, g.edition, g.edition
                )],
            }
        })
        .collect();
    r.fail_with_findings(
        format!(
            "{} rustfmt.toml edition(s) differ from the manifest",
            gaps.len()
        ),
        findings,
    );
}

fn rs_files(ctx: &Ctx) -> Result<Vec<String>, String> {
    let mut files = rs_files_in(&ctx.root, &ctx.path_env)?;
    if let Some(paths) = &ctx.input_paths
        && !paths.iter().any(|p| p.ends_with("rustfmt.toml"))
    {
        files.retain(|p| paths.contains(p));
    }
    Ok(files)
}

fn rs_files_in(root: &Path, path_env: &std::ffi::OsStr) -> Result<Vec<String>, String> {
    let tracked = ls_files(root, path_env, &["--cached"])?;
    let untracked = ls_files(root, path_env, &["--others", "--exclude-standard"])?;
    let mut files: Vec<String> = tracked
        .into_iter()
        .chain(untracked.into_iter().filter(|p| built_by_cargo(root, p)))
        .filter(|p| !p.starts_with("vendor/") && !EXCLUDED.contains(&p.as_str()))
        .filter(|p| root.join(p).is_file())
        .collect();
    files.sort_unstable();
    files.dedup();
    Ok(files)
}

fn ls_files(
    root: &Path,
    path_env: &std::ffi::OsStr,
    which: &[&str],
) -> Result<Vec<String>, String> {
    let out = crate::changed::git(root, path_env)
        .args(["ls-files", "-z"])
        .args(which)
        .args(["--", "*.rs"])
        .output()
        .map_err(|e| format!("could not run `git ls-files`: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "`git ls-files` failed in {}: {}",
            root.display(),
            String::from_utf8_lossy(&out.stderr).trim_end()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .split('\0')
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .collect())
}

const TARGET_DIRS: [&str; 4] = ["src", "tests", "benches", "examples"];

fn built_by_cargo(root: &Path, rel: &str) -> bool {
    let path = Path::new(rel);
    let Some(dir) = path
        .parent()
        .unwrap_or(Path::new(""))
        .ancestors()
        .find(|d| root.join(d).join("Cargo.toml").is_file())
    else {
        return false;
    };
    let Ok(manifest) = crate::read_regular_text(root.join(dir).join("Cargo.toml")) else {
        return false;
    };
    let Ok(inside) = path.strip_prefix(dir) else {
        return false;
    };
    manifest.lines().any(|l| l.trim() == "[package]")
        && (inside == Path::new("build.rs")
            || inside
                .components()
                .next()
                .is_some_and(|c| TARGET_DIRS.iter().any(|t| c.as_os_str() == *t))
            || declared_dirs(&manifest).iter().any(|d| {
                let parent = inside.parent().unwrap_or(Path::new(""));
                parent == d || (!d.as_os_str().is_empty() && parent.starts_with(d))
            }))
}

fn declared_dirs(manifest: &str) -> Vec<PathBuf> {
    manifest
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            if !["path", "build"].contains(&key.trim()) {
                return None;
            }
            let target = value.trim().strip_prefix('"')?.strip_suffix('"')?;
            let dir = Path::new(target)
                .parent()
                .filter(|_| target.ends_with(".rs"))?;
            dir.components()
                .all(|c| matches!(c, std::path::Component::Normal(_)))
                .then(|| dir.to_path_buf())
        })
        .collect()
}

/// Format `files` (repo-relative) in place, each at its package's edition: what another check's
/// `--fix` runs over the files it wrote. Returns the formatter's lines for those it could not.
pub(crate) fn write(ctx: &Ctx, bin: &Path, files: &[String]) -> Vec<String> {
    let groups = by_edition(files, &mut Editions::new(&ctx.root));
    sweep(ctx, bin, &groups, true).unread
}

fn by_edition(files: &[String], editions: &mut Editions) -> Vec<(String, Vec<String>)> {
    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for f in files {
        groups
            .entry(editions.manifest(f))
            .or_default()
            .push(f.clone());
    }
    groups.into_iter().collect()
}

fn sweep(ctx: &Ctx, bin: &Path, groups: &[(String, Vec<String>)], write: bool) -> Sweep {
    let jobs: Vec<(&str, &[String])> = groups
        .iter()
        .flat_map(|(edition, files)| files.chunks(CHUNK).map(move |c| (edition.as_str(), c)))
        .collect();
    let next = Mutex::new(0usize);
    let found = Mutex::new(Sweep {
        files: groups.iter().map(|(_, f)| f.len()).sum(),
        ..Sweep::default()
    });
    let workers = ctx
        .pool
        .as_deref()
        .map_or_else(|| ctx.parallelism(), aterm_capacity::Jobserver::slots)
        .max(1) as usize;
    let canonical_root = std::fs::canonicalize(&ctx.root).ok();
    std::thread::scope(|scope| {
        for _ in 0..workers.min(jobs.len()) {
            scope.spawn(|| {
                loop {
                    let Some((edition, chunk)) = next.lock().ok().and_then(|mut n| {
                        let job = jobs.get(*n).copied();
                        *n += 1;
                        job
                    }) else {
                        return;
                    };
                    // Each pooled formatter process spends one token beside compiler and test
                    // workers. The coordinator holds none while its workers wait for theirs.
                    let out = crate::compile_width::run(
                        ctx.pool.as_deref(),
                        ctx.pool.as_deref().map(|_| 1),
                        None,
                        &crate::stopping,
                        || exec::run(&trustfmt_cmd(bin, edition, chunk, write), ctx.exec_env()),
                    );
                    let read = match out {
                        Ok(out) => read_run(
                            &out,
                            edition,
                            chunk.len(),
                            &ctx.root,
                            canonical_root.as_deref(),
                        ),
                        Err(error) => Sweep {
                            unread: vec![format!("trustfmt CPU admission: {error}")],
                            ..Sweep::default()
                        },
                    };
                    if let Ok(mut f) = found.lock() {
                        f.drifted.extend(read.drifted);
                        f.unread.extend(read.unread);
                    }
                }
            });
        }
    });
    let mut s = found
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    s.drifted.sort();
    s.drifted.dedup();
    s
}

/// `trustfmt [--check] --edition <E> --unstable-features --skip-children --color never <files>`,
/// from the root. Uncoloured, so each of its `error` lines starts with `error` and is read as one
/// ([`read_run`]), its internal ones included.
#[must_use]
pub fn trustfmt_cmd(bin: &Path, edition: &str, files: &[String], write: bool) -> Cmd {
    let mut cmd = Cmd::new(bin);
    if !write {
        cmd = cmd.arg("--check");
    }
    // `read_run` reads the child's lines by their first word (`error`, `Diff in `). Under
    // verify trustfmt has written them in colour (`\e[1m\e[91merror\e[0m`), which hid every
    // error behind "read none of N file(s)", so the child is told never to colour them.
    cmd.args([
        "--color",
        "never",
        "--edition",
        edition,
        "--unstable-features",
        "--skip-children",
    ])
    .args(files)
}

fn read_run(
    out: &exec::Run,
    edition: &str,
    handed: usize,
    root: &Path,
    canonical_root: Option<&Path>,
) -> Sweep {
    let mut s = Sweep::default();
    if out.ok {
        return s;
    }
    let text = out.output.as_str();
    let lines: Vec<&str> = text.lines().collect();
    // Each error with the `--> <path>:<line>` the formatter puts under it, which names the file.
    let errors: Vec<String> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.starts_with("error"))
        .map(|(i, l)| {
            let at = lines[i + 1..]
                .iter()
                .take(2)
                .find_map(|next| next.trim_start().strip_prefix("--> "));
            match at {
                Some(at) => format!("{} ({})", l.trim_end(), at.trim_end()),
                None => l.trim_end().to_string(),
            }
        })
        .collect();
    if !text.contains(DIFF_MARKER) && errors.is_empty() {
        let why = out
            .environment_failure()
            .unwrap_or_else(|| format!("exit {:?}", out.code));
        s.unread.push(format!(
            "error: {TRUSTFMT} read none of {handed} file(s) at edition {edition} ({why}): {}",
            text.trim_end().lines().last().unwrap_or("no output")
        ));
        return s;
    }
    s.unread = errors;
    s.drifted = diff_paths(text)
        .into_iter()
        .map(|abs| Drift {
            path: relative_to(&abs, root, canonical_root),
            edition: edition.to_string(),
        })
        .collect();
    s
}

/// The paths of `Diff in <path>:<line>:` lines.
#[must_use]
pub fn diff_paths(output: &str) -> BTreeSet<String> {
    output
        .lines()
        .filter_map(|l| l.strip_prefix(DIFF_MARKER))
        .filter_map(|rest| {
            let rest = rest.trim_end();
            let rest = rest.strip_suffix(':').unwrap_or(rest);
            rest.rsplit_once(':').map(|(path, _)| path.to_owned())
        })
        .collect()
}

fn relative_to(abs: &str, root: &Path, canonical_root: Option<&Path>) -> String {
    let p = Path::new(abs);
    std::iter::once(root)
        .chain(canonical_root)
        .find_map(|base| p.strip_prefix(base).ok())
        .map_or_else(|| abs.to_owned(), |rel| rel.to_string_lossy().into_owned())
}

/// Every directory whose nearest `rustfmt.toml` resolves a different edition than its manifest.
#[must_use]
pub fn edition_gaps(root: &Path, files: &[String], editions: &mut Editions) -> Vec<EditionGap> {
    let mut gaps: BTreeMap<(String, String), String> = BTreeMap::new();
    for rel in files {
        let manifest = editions.manifest(rel);
        if editions.rustfmt(rel) != manifest {
            gaps.entry((manifest_dir_of(root, rel), manifest))
                .or_insert_with(|| rel.clone());
        }
    }
    gaps.into_iter()
        .map(|((dir, edition), example)| EditionGap {
            dir,
            edition,
            example,
        })
        .collect()
}

/// The two editions a file has, read once per directory: its manifest's and its `rustfmt.toml`'s.
pub struct Editions {
    root: PathBuf,
    manifest: BTreeMap<PathBuf, String>,
    rustfmt: BTreeMap<PathBuf, String>,
    workspace: String,
}

impl Editions {
    #[must_use]
    pub fn new(root: &Path) -> Self {
        let workspace = crate::read_regular_text(root.join("Cargo.toml"))
            .ok()
            .and_then(|t| toml_edition(&t))
            .unwrap_or_else(|| "2024".to_string());
        Self {
            root: root.to_path_buf(),
            manifest: BTreeMap::new(),
            rustfmt: BTreeMap::new(),
            workspace,
        }
    }

    /// The edition of the nearest `Cargo.toml` above `rel` that declares one.
    pub fn manifest(&mut self, rel: &str) -> String {
        let dir = Path::new(rel)
            .parent()
            .unwrap_or(Path::new(""))
            .to_path_buf();
        if let Some(e) = self.manifest.get(&dir) {
            return e.clone();
        }
        let found = dir
            .ancestors()
            .find_map(|d| {
                let text = crate::read_regular_text(self.root.join(d).join("Cargo.toml")).ok()?;
                if toml_workspace_edition(&text) {
                    Some(self.workspace.clone())
                } else {
                    toml_edition(&text)
                }
            })
            .unwrap_or_else(|| self.workspace.clone());
        self.manifest.insert(dir, found.clone());
        found
    }

    /// The edition rustfmt resolves for `rel`.
    pub fn rustfmt(&mut self, rel: &str) -> String {
        let dir = Path::new(rel)
            .parent()
            .unwrap_or(Path::new(""))
            .to_path_buf();
        if let Some(e) = self.rustfmt.get(&dir) {
            return e.clone();
        }
        let found = dir
            .ancestors()
            .find_map(|d| {
                CONFIG_FILES.iter().find_map(|name| {
                    toml_edition(&crate::read_regular_text(self.root.join(d).join(name)).ok()?)
                })
            })
            .unwrap_or_else(|| "2015".to_string());
        self.rustfmt.insert(dir, found.clone());
        found
    }
}

fn manifest_dir_of(root: &Path, rel: &str) -> String {
    Path::new(rel)
        .parent()
        .unwrap_or(Path::new(""))
        .ancestors()
        .find(|d| root.join(d).join("Cargo.toml").is_file())
        .map(|d| d.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The first `edition = "…"` of a flat TOML file.
#[must_use]
pub fn toml_edition(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .find_map(|l| {
            let rest = l.strip_prefix("edition")?.trim_start();
            let quoted = rest.strip_prefix('=')?.trim_start();
            let quote = quoted.chars().next().filter(|&c| c == '"' || c == '\'')?;
            Some(quoted[1..].split(quote).next()?.to_string())
        })
}

fn toml_workspace_edition(text: &str) -> bool {
    text.lines()
        .map(|l| l.split('#').next().unwrap_or("").trim())
        .filter(|l| !l.is_empty())
        .any(|l| {
            let Some(rest) = l.strip_prefix("edition").map(str::trim_start) else {
                return false;
            };
            if let Some(after_dot) = rest.strip_prefix('.') {
                return after_dot
                    .trim_start()
                    .strip_prefix("workspace")
                    .and_then(|r| r.trim_start().strip_prefix('='))
                    .is_some_and(|v| v.trim() == "true");
            }
            if let Some(after_eq) = rest.strip_prefix('=')
                && let Some(inner) = after_eq.trim_start().strip_prefix('{')
            {
                return inner.split([',', '}']).any(|part| {
                    part.trim()
                        .strip_prefix("workspace")
                        .and_then(|r| r.trim_start().strip_prefix('='))
                        .is_some_and(|v| v.trim() == "true")
                });
            }
            false
        })
}

#[cfg(test)]
#[path = "fmt_tests.rs"]
mod tests;
