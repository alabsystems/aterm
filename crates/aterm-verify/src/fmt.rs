// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `verify fmt [--fix]`: every `.rs` file git tracks, outside `vendor/`, and
//! every untracked one cargo builds (a new source not yet staged), through
//! the pinned `trustfmt` at the edition its crate declares, in one parallel
//! pass; and every `rustfmt.toml` edition against its manifest.
//!
//! Per file, not `targo-fmt --all`: the files cargo's target discovery never
//! reaches (an `include!`d source, a crate outside `members`) are held too,
//! and `--skip-children` judges each file alone, as `--all` formats an
//! out-of-line module. `--fix` rewrites exactly the files the check found.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::Ctx;
use crate::exec::{self, Cmd};
use crate::ladder::{Finding, Report};

/// The formatter, beside `targo` in the stage2 bin directory, run by path: a
/// `rustfmt` off PATH is stock Rust's, a different style.
pub const TRUSTFMT: &str = "trustfmt";

/// Tracked sources whose layout is their content, never handed to the
/// formatter: generated tables whose generator owns their shape, and a proof
/// island in a notation only a newer seal's formatter parses.
pub const EXCLUDED: [&str; 6] = [
    "crates/aterm-core/tests/support/replay_corpus_data.rs",
    "crates/aterm-effects/src/animal_glyphs_gen.rs",
    "crates/aterm-effects/src/cat_glyphs_gen.rs",
    "crates/aterm-effects/src/dog_glyphs_gen.rs",
    "crates/aterm-effects/src/pet_glyphs_gen.rs",
    "crates/aterm-gui/src/config_watcher_protocol_island.rs",
];

/// How a formatter names an unformatted file: `Diff in <path>:<line>:`. A
/// non-zero exit without one is a formatter that could not read the files.
const DIFF_MARKER: &str = "Diff in ";

/// Files per `trustfmt` child: small enough that the pass spreads over every
/// core, large enough that process start-up stays a fraction of it.
const CHUNK: usize = 48;

/// A file the formatter would change, and the edition it was judged at.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Drift {
    pub path: String,
    pub edition: String,
}

/// A directory whose `rustfmt.toml` edition (what every hand-run formatter
/// reads) is not its manifest's (what this check formats at).
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
    // The toolchain directory's own, else the managed Trust build's
    // (`Toolchain::borrowed`); a skip only when there is none anywhere.
    let Some(bin) = ctx.tools.trustfmt.clone() else {
        r.skip(ctx.tools.missing_trustfmt_label());
        return;
    };
    let files = match rs_files(ctx) {
        Ok(f) if !f.is_empty() => f,
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
    let groups = by_edition(&files, &mut editions);
    let found = sweep(ctx, &bin, &groups, false);
    record_sweep(ctx, r, &bin, &found);
    record_editions(r, &edition_gaps(&ctx.root, &files, &mut editions));
}

/// The sweep's rows: the files it found unformatted, fixed under `--fix`.
fn record_sweep(ctx: &Ctx, r: &mut Report, bin: &Path, found: &Sweep) {
    if !found.unread.is_empty() {
        r.raw(found.unread.join("\n"));
        r.cannot_run(format!(
            "trustfmt could not read {} file(s) (above)",
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

/// ` (the managed Trust build's <file>)` when the run took `name` from the managed
/// Trust build because its toolchain directory has none (`Toolchain::borrowed`), so
/// the row says which formatter or lint decided it; empty otherwise.
pub(crate) fn borrowed_suffix(ctx: &Ctx, name: &str) -> String {
    ctx.tools
        .borrowed(name)
        .map(|file| format!(" (the managed Trust build's {})", file.display()))
        .unwrap_or_default()
}

/// The edition rows: one finding per directory whose `rustfmt.toml` must name
/// its manifest's edition.
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

/// Every `*.rs` path git tracks outside `vendor/` (third-party source
/// mirrored, not authored), and every UNTRACKED one cargo builds
/// ([`built_by_cargo`]) that git does not ignore, present on disk,
/// repo-relative and sorted, bar [`EXCLUDED`]. A new source is held before it
/// is staged: tracked only, `crates/aterm-lab/src/gemini_proof.rs` passed
/// this check unformatted until it was added (2026-10-16). An untracked file
/// cargo does not build (a scratch file, notes) is never handed to the
/// formatter, so `--fix` never rewrites one.
fn rs_files(ctx: &Ctx) -> Result<Vec<String>, String> {
    rs_files_in(&ctx.root, &ctx.path_env)
}

/// [`rs_files`] of the checkout at `root`, git run on `path_env`.
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

/// The `*.rs` paths `git ls-files <which>` names in `root`, repo-relative.
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

/// The directories under a package whose `.rs` files cargo builds: its
/// library and binaries (and every module they declare), and the targets it
/// discovers by itself.
const TARGET_DIRS: [&str; 4] = ["src", "tests", "benches", "examples"];

/// Whether cargo builds `rel` (repo-relative): it sits under a
/// [`TARGET_DIRS`] directory of the package whose `Cargo.toml` is the
/// nearest above it, or is that package's `build.rs`, or sits where that
/// manifest declares a target by path ([`declared_dirs`]: a fuzz crate's
/// `fuzz_targets/`). A file under a manifest that declares no `[package]` (a
/// workspace root), or elsewhere in a package (its notes, a fixture tree), is
/// not.
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
    let Ok(manifest) = std::fs::read_to_string(root.join(dir).join("Cargo.toml")) else {
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

/// The directories, under its package, of the `.rs` targets a manifest
/// declares by path (`path = "fuzz_targets/decode.rs"` under a `[[bin]]`,
/// `build = "…"`): cargo builds what sits there, and the modules beside and
/// under it, though it is no [`TARGET_DIRS`] directory. A path that leaves
/// the package (`path = "../../src/lib.rs"`, an oracle's crate under test)
/// names none; nor does a dependency's (`path = "../x"`, no `.rs`). The
/// package's own directory, where a target sits at its top, counts for the
/// files beside it alone ([`built_by_cargo`]).
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

/// `files` grouped by the edition each is formatted at.
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

/// One formatter pass over `groups`, chunked across the machine's cores:
/// `--check` reports, `write` rewrites.
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
    let workers = ctx.cores().max(1) as usize;
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
                    let out = exec::run(&trustfmt_cmd(bin, edition, chunk, write), ctx.exec_env());
                    let read = read_run(
                        &out,
                        edition,
                        chunk.len(),
                        &ctx.root,
                        canonical_root.as_deref(),
                    );
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

/// `trustfmt [--check] --edition <E> --unstable-features --skip-children
/// <files>`, from the root. `--skip-children` judges each file alone rather
/// than re-formatting every `mod` it declares.
#[must_use]
pub fn trustfmt_cmd(bin: &Path, edition: &str, files: &[String], write: bool) -> Cmd {
    let mut cmd = Cmd::new(bin);
    if !write {
        cmd = cmd.arg("--check");
    }
    cmd.args([
        "--edition",
        edition,
        "--unstable-features",
        "--skip-children",
    ])
    .args(files)
}

/// What one child said: the files it would change, or the lines naming files
/// it could not read. A non-zero exit with no diff is a formatter that read
/// nothing; one with a diff read at least some, and its `error` lines name
/// the rest. Only an `error` at column zero is the formatter's own: every diff
/// line starts with ` `, `+` or `-`, so a source line reading `error: …` is
/// never one.
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
    let errors: Vec<String> = text
        .lines()
        .filter(|l| l.starts_with("error"))
        .map(|l| l.trim_end().to_string())
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

/// The paths of `Diff in <path>:<line>:` lines, split at the last colon so a
/// colon inside a checkout path survives.
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

/// `abs` relative to the root as given or as canonicalised (macOS reports
/// `/private/var` for `/var`); unchanged when neither prefix matches, so a
/// finding is never silently dropped.
fn relative_to(abs: &str, root: &Path, canonical_root: Option<&Path>) -> String {
    let p = Path::new(abs);
    std::iter::once(root)
        .chain(canonical_root)
        .find_map(|base| p.strip_prefix(base).ok())
        .map_or_else(|| abs.to_owned(), |rel| rel.to_string_lossy().into_owned())
}

/// Every directory whose nearest `rustfmt.toml` resolves a different edition
/// than its manifest declares, so a hand-run formatter would reflow it.
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

/// The two editions a file has, read once per directory: its manifest's (the
/// one this check formats at) and its `rustfmt.toml`'s (the one every other
/// formatter spelling reads).
pub struct Editions {
    root: PathBuf,
    manifest: BTreeMap<PathBuf, String>,
    rustfmt: BTreeMap<PathBuf, String>,
    workspace: String,
}

impl Editions {
    #[must_use]
    pub fn new(root: &Path) -> Self {
        // The newest edition rather than an old one when the root names none:
        // a wrong-but-newer edition fails to parse loudly, a wrong-but-older
        // one reflows silently.
        let workspace = std::fs::read_to_string(root.join("Cargo.toml"))
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

    /// The edition of the nearest `Cargo.toml` above `rel` that declares one,
    /// `edition.workspace = true` resolved; the workspace's when none does.
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
                let text = std::fs::read_to_string(self.root.join(d).join("Cargo.toml")).ok()?;
                let line = text
                    .lines()
                    .map(str::trim)
                    .find(|l| l.starts_with("edition"))?;
                if line.contains("workspace") {
                    Some(self.workspace.clone())
                } else {
                    toml_edition(&text)
                }
            })
            .unwrap_or_else(|| self.workspace.clone());
        self.manifest.insert(dir, found.clone());
        found
    }

    /// The edition rustfmt resolves for `rel`: the nearest `rustfmt.toml` or
    /// `.rustfmt.toml` that names one, else rustfmt's own default, 2015.
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
                ["rustfmt.toml", ".rustfmt.toml"].iter().find_map(|name| {
                    toml_edition(&std::fs::read_to_string(self.root.join(d).join(name)).ok()?)
                })
            })
            .unwrap_or_else(|| "2015".to_string());
        self.rustfmt.insert(dir, found.clone());
        found
    }
}

/// The directory of the nearest `Cargo.toml` above `rel`, repo-relative:
/// where a reconciling `rustfmt.toml` goes.
fn manifest_dir_of(root: &Path, rel: &str) -> String {
    Path::new(rel)
        .parent()
        .unwrap_or(Path::new(""))
        .ancestors()
        .find(|d| root.join(d).join("Cargo.toml").is_file())
        .map(|d| d.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The first `edition = "…"` of a flat TOML file. A one-key reader: it skips
/// `edition.workspace = true`, which [`Editions::manifest`] resolves.
#[must_use]
pub fn toml_edition(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .find_map(|l| {
            let rest = l.strip_prefix("edition")?.trim_start();
            let quoted = rest.strip_prefix('=')?.trim();
            Some(quoted.split(['"', '\'']).nth(1)?.to_string())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(files: &[(&str, &str)]) -> aterm_tempfile::TempDir {
        let dir = aterm_tempfile::Builder::new()
            .prefix("atv-fmt-")
            .tempdir()
            .expect("scratch");
        for (rel, text) in files {
            let p = dir.path().join(rel);
            std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
            std::fs::write(p, text).expect("write");
        }
        dir
    }

    /// `git` in the scratch checkout `root`, each call asserted.
    fn git_in(root: &Path, calls: &[&[&str]]) {
        let env = crate::EnvSnapshot::capture();
        for args in calls {
            let ok = crate::changed::git(root, &env.path)
                .args(*args)
                .status()
                .is_ok_and(|s| s.success());
            assert!(ok, "git {args:?} in the scratch checkout");
        }
    }

    /// THE CHECK HOLDS A NEW SOURCE BEFORE IT IS STAGED: an untracked `.rs`
    /// under a package's `src/`, `tests/`, `benches/` or `examples/`, its
    /// `build.rs`, or where its manifest declares a target by path (a fuzz
    /// crate's `fuzz_targets/` and below; a `[lib]` at the package's top and
    /// the files beside it) is listed beside every tracked one (tracked only,
    /// `gemini_proof.rs` passed unformatted until it was added). NEGATIVE
    /// CONTROLS: an untracked file cargo builds nothing from — notes in a
    /// package, a fuzz crate's corpus and its other directories, a file under
    /// a manifest with no `[package]`, outside any manifest, a directory under
    /// a top-level `[lib]`'s, the directory a target path that leaves the
    /// package names, a dependency's path — one git ignores, and one under
    /// `vendor/` are not; and git's tracked listing alone, which the check
    /// read before, names none of the new sources.
    #[test]
    fn an_untracked_source_cargo_builds_is_listed_and_nothing_else_untracked() {
        let t = tree(&[
            ("Cargo.toml", "[workspace]\nmembers = [\"crates/a\"]\n"),
            (
                "crates/a/Cargo.toml",
                "[package]\nname = \"a\"\nedition = \"2024\"\n",
            ),
            ("crates/a/src/lib.rs", "pub fn tracked() {}\n"),
            (".gitignore", "crates/a/src/ignored.rs\n"),
            (
                "crates/a/fuzz/Cargo.toml",
                "[package]\nname = \"a-fuzz\"\n\n[dependencies.x]\npath = \"x\"\n\n\
                 [[bin]]\nname = \"decode\"\npath = \"fuzz_targets/decode.rs\"\n\n\
                 [[bin]]\nname = \"up\"\npath = \"../notes/up.rs\"\n",
            ),
            ("crates/a/fuzz/fuzz_targets/decode.rs", "fn main() {}\n"),
            (
                "crates/flat/Cargo.toml",
                "[package]\nname = \"flat\"\n\n[lib]\npath = \"lib.rs\"\n",
            ),
        ]);
        let root = t.path();
        git_in(root, &[&["init", "-q"], &["add", "."]]);
        for (rel, text) in [
            ("crates/a/src/new.rs", "pub fn new() {}\n"),
            ("crates/a/src/deep/mod.rs", "pub fn deep() {}\n"),
            ("crates/a/tests/t.rs", "#[test]\nfn t() {}\n"),
            ("crates/a/benches/b.rs", "fn main() {}\n"),
            ("crates/a/examples/e.rs", "fn main() {}\n"),
            ("crates/a/build.rs", "fn main() {}\n"),
            ("crates/a/notes/scratch.rs", "fn scratch( ){}\n"),
            ("crates/a/src/ignored.rs", "fn ignored( ){}\n"),
            ("scratch.rs", "fn scratch( ){}\n"),
            ("vendor/v/Cargo.toml", "[package]\nname = \"v\"\n"),
            ("vendor/v/src/lib.rs", "fn v( ){}\n"),
            ("crates/a/fuzz/fuzz_targets/new.rs", "fn main() {}\n"),
            ("crates/a/fuzz/fuzz_targets/util/mod.rs", "pub fn u() {}\n"),
            ("crates/a/fuzz/corpus/seed.rs", "fn seed( ){}\n"),
            ("crates/a/fuzz/scratch.rs", "fn scratch( ){}\n"),
            ("crates/a/fuzz/x/lib.rs", "fn x( ){}\n"),
            ("crates/flat/lib.rs", "pub fn flat() {}\n"),
            ("crates/flat/beside.rs", "pub fn beside() {}\n"),
            ("crates/flat/notes/n.rs", "fn n( ){}\n"),
            ("crates/a/notes/up.rs", "fn up( ){}\n"),
        ] {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
            std::fs::write(p, text).expect("write");
        }
        let env = crate::EnvSnapshot::capture();
        let files = rs_files_in(root, &env.path).expect("listed");
        assert_eq!(
            files,
            [
                "crates/a/benches/b.rs",
                "crates/a/build.rs",
                "crates/a/examples/e.rs",
                "crates/a/fuzz/fuzz_targets/decode.rs",
                "crates/a/fuzz/fuzz_targets/new.rs",
                "crates/a/fuzz/fuzz_targets/util/mod.rs",
                "crates/a/src/deep/mod.rs",
                "crates/a/src/lib.rs",
                "crates/a/src/new.rs",
                "crates/a/tests/t.rs",
                "crates/flat/beside.rs",
                "crates/flat/lib.rs",
            ]
        );
        let tracked = ls_files(root, &env.path, &["--cached"]).expect("listed");
        assert_eq!(
            tracked,
            [
                "crates/a/fuzz/fuzz_targets/decode.rs",
                "crates/a/src/lib.rs"
            ],
            "the control"
        );
    }

    #[test]
    fn a_diff_path_keeps_a_colon_inside_the_checkout_path() {
        let out = "Diff in /a:b/crates/x/src/lib.rs:12:\n stuff\nDiff in /a:b/y.rs:3:\n\
                   Diff in /a:b/y.rs:9:\nerror: something\n";
        let got: Vec<String> = diff_paths(out).into_iter().collect();
        assert_eq!(got, ["/a:b/crates/x/src/lib.rs", "/a:b/y.rs"]);
    }

    #[test]
    fn a_path_is_made_relative_through_either_spelling_of_the_root() {
        let root = Path::new("/var/r");
        let canon = Path::new("/private/var/r");
        assert_eq!(relative_to("/var/r/a.rs", root, Some(canon)), "a.rs");
        assert_eq!(
            relative_to("/private/var/r/a.rs", root, Some(canon)),
            "a.rs"
        );
        assert_eq!(
            relative_to("/elsewhere/a.rs", root, Some(canon)),
            "/elsewhere/a.rs"
        );
    }

    /// Each file is formatted at its own crate's edition, inherited from the
    /// workspace or declared, and the root's when no manifest owns it.
    #[test]
    fn each_file_takes_its_own_crates_edition() {
        let t = tree(&[
            ("Cargo.toml", "[workspace.package]\nedition = \"2024\"\n"),
            ("a/Cargo.toml", "[package]\nedition.workspace = true\n"),
            ("b/Cargo.toml", "[package]\nedition = \"2021\"\n"),
        ]);
        let mut e = Editions::new(t.path());
        assert_eq!(e.manifest("a/src/lib.rs"), "2024");
        assert_eq!(e.manifest("b/src/deep/mod.rs"), "2021");
        assert_eq!(e.manifest("fixtures/x.rs"), "2024");
        let groups = by_edition(
            &["a/src/lib.rs".into(), "b/src/lib.rs".into(), "c.rs".into()],
            &mut e,
        );
        assert_eq!(
            groups,
            [
                ("2021".to_string(), vec!["b/src/lib.rs".to_string()]),
                (
                    "2024".to_string(),
                    vec!["a/src/lib.rs".to_string(), "c.rs".to_string()]
                ),
            ]
        );
    }

    /// A crate whose `rustfmt.toml` names another edition than its manifest
    /// is a gap until the file says the manifest's; a tree with no
    /// `rustfmt.toml` at all reads rustfmt's 2015.
    #[test]
    fn a_rustfmt_edition_that_contradicts_its_manifest_is_a_gap_until_written() {
        let t = tree(&[
            ("Cargo.toml", "[package]\nedition = \"2024\"\n"),
            ("rustfmt.toml", "edition = \"2024\"\n"),
            ("inner/Cargo.toml", "[package]\nedition = \"2021\"\n"),
        ]);
        let files = ["src/lib.rs".to_string(), "inner/src/lib.rs".to_string()];
        let gaps = edition_gaps(t.path(), &files, &mut Editions::new(t.path()));
        assert_eq!(
            gaps,
            [EditionGap {
                dir: "inner".into(),
                edition: "2021".into(),
                example: "inner/src/lib.rs".into(),
            }]
        );
        std::fs::write(t.path().join("inner/rustfmt.toml"), "edition = \"2021\"\n").expect("w");
        assert!(edition_gaps(t.path(), &files, &mut Editions::new(t.path())).is_empty());
        std::fs::remove_file(t.path().join("rustfmt.toml")).expect("rm");
        let gaps = edition_gaps(t.path(), &files, &mut Editions::new(t.path()));
        assert_eq!(gaps.len(), 1, "{gaps:?}");
        assert_eq!(gaps[0].dir, "");
    }

    #[test]
    fn the_command_checks_unless_it_writes() {
        let files = ["a.rs".to_string()];
        let check = trustfmt_cmd(Path::new("/s/trustfmt"), "2021", &files, false).argv();
        assert_eq!(
            check,
            [
                "/s/trustfmt",
                "--check",
                "--edition",
                "2021",
                "--unstable-features",
                "--skip-children",
                "a.rs"
            ]
        );
        let write = trustfmt_cmd(Path::new("/s/trustfmt"), "2021", &files, true).argv();
        assert!(!write.contains(&"--check".to_string()), "{write:?}");
    }

    fn run_of(ok: bool, output: &str) -> exec::Run {
        exec::Run {
            ok,
            output: output.to_string(),
            code: Some(i32::from(!ok)),
            spawn_error: None,
        }
    }

    /// A clean child decides nothing; a diff names its files; a failure with
    /// no diff read nothing; a diff beside `error` lines read only some, and a
    /// source line reading `error` inside the diff is drift, never unread.
    #[test]
    fn a_child_separates_drift_from_files_it_could_not_read() {
        let root = Path::new("/r");
        assert_eq!(
            read_run(&run_of(true, ""), "2024", 3, root, None),
            Sweep::default()
        );
        let drift = read_run(
            &run_of(false, "Diff in /r/a.rs:1:\n-x\n+y\n"),
            "2024",
            3,
            root,
            None,
        );
        assert_eq!(
            drift.drifted,
            [Drift {
                path: "a.rs".into(),
                edition: "2024".into()
            }]
        );
        assert!(drift.unread.is_empty());
        let blind = read_run(&run_of(false, "dyld: missing\n"), "2024", 3, root, None);
        assert!(blind.drifted.is_empty());
        assert_eq!(blind.unread.len(), 1);
        assert!(blind.unread[0].contains("read none of 3"), "{blind:?}");
        let mixed = read_run(
            &run_of(
                false,
                "Diff in /r/a.rs:1:\n     error: String,\n-x\n+y\nerror: expected item, found `≐`\n",
            ),
            "2024",
            3,
            root,
            None,
        );
        assert_eq!(mixed.drifted.len(), 1);
        assert_eq!(mixed.unread, ["error: expected item, found `≐`"]);
    }

    /// The real formatter over a scratch checkout: a file whose diff carries
    /// a context line reading `error: …` is one drift and nothing unread, and
    /// so is a new source not yet staged; `--fix` rewrites both, and leaves an
    /// untracked scratch file cargo builds nothing from as it was; the next
    /// check passes. Skips, saying so, on a machine with no trustfmt.
    #[test]
    fn the_real_formatter_reds_on_drift_and_the_fix_greens_it() {
        use crate::ladder::{Outcome, Severity};
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        // Under `verify` every child inherits the run's directory as TRUST_STAGE2_BIN
        // (`Ctx::new_with_tools`), and an explicit override borrows nothing
        // (`toolchain-borrowed-tools`): a run whose directory lacks a formatter it
        // borrowed from the managed build would hand this test a toolchain with none,
        // and the test would skip on every run. It wants a real formatter, whichever
        // one this machine has, so it looks again without the inherited override.
        let env = crate::EnvSnapshot::capture();
        let mut tools = crate::run_toolchain(&env, &repo);
        if tools.trustfmt.is_none() && env.trust_stage2_bin.is_some() {
            let discovered = crate::EnvSnapshot {
                trust_stage2_bin: None,
                ..env
            };
            tools = crate::run_toolchain(&discovered, &repo);
        }
        if tools.trustfmt.is_none() {
            eprintln!(
                "SKIP the_real_formatter_reds_on_drift_and_the_fix_greens_it: {}, so this \
                 machine cannot show the check either way",
                tools.missing_trustfmt_label()
            );
            return;
        }
        let t = tree(&[
            (
                "Cargo.toml",
                "[package]\nname = \"fx\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
            ),
            ("rustfmt.toml", "edition = \"2021\"\n"),
            (
                "src/lib.rs",
                "pub struct E {\n    error: String,\n}\npub fn wrong(  ) ->usize{let x=1;x}\n",
            ),
        ]);
        let root = t.path().to_path_buf();
        let scratch = t.path().join(".scratch");
        std::fs::create_dir_all(&scratch).expect("scratch");
        git_in(
            &root,
            &[
                &["init", "-q"],
                &["add", "Cargo.toml", "rustfmt.toml", "src"],
            ],
        );
        let untidy = "pub fn new(  ) ->usize{2}\n";
        std::fs::write(root.join("src/new.rs"), untidy).expect("write");
        std::fs::create_dir_all(root.join("notes")).expect("mkdir");
        std::fs::write(root.join("notes/scratch.rs"), untidy).expect("write");
        let check = |fix: bool| {
            let ctx = Ctx::new_with_tools(
                root.clone(),
                crate::Scope::workspace(),
                crate::EnvSnapshot::capture(),
                scratch.clone(),
                tools.clone(),
            )
            .with_fix(fix);
            let mut r = Report::new("fmt");
            run(&ctx, &mut r);
            r
        };
        let red = check(false);
        let rows: Vec<(Outcome, String)> =
            red.outcomes().map(|(o, l)| (o, l.to_string())).collect();
        assert!(
            rows.iter()
                .all(|(o, _)| *o != Outcome::Fail(Severity::CouldNotRun)),
            "{rows:?}"
        );
        let ids: Vec<String> = red.findings().into_iter().map(|f| f.id).collect();
        assert_eq!(ids, ["src/lib.rs", "src/new.rs"], "{rows:?}");
        let fixed = check(true);
        assert!(
            fixed.outcomes().all(|(o, _)| o == Outcome::Ok),
            "{:?}",
            fixed.outcomes().collect::<Vec<_>>()
        );
        assert!(
            std::fs::read_to_string(root.join("src/lib.rs"))
                .expect("read")
                .contains("pub fn wrong() -> usize {")
        );
        assert!(
            std::fs::read_to_string(root.join("src/new.rs"))
                .expect("read")
                .contains("pub fn new() -> usize {")
        );
        assert_eq!(
            std::fs::read_to_string(root.join("notes/scratch.rs")).expect("read"),
            untidy,
            "the scratch file is never rewritten"
        );
        let green = check(false);
        assert!(
            green.outcomes().all(|(o, _)| o == Outcome::Ok),
            "{:?}",
            green.outcomes().collect::<Vec<_>>()
        );
    }

    #[test]
    fn an_edition_line_is_read_and_a_workspace_key_is_not() {
        assert_eq!(
            toml_edition("# edition = \"x\"\nedition = '2021'\n").as_deref(),
            Some("2021")
        );
        assert_eq!(toml_edition("edition.workspace = true\n"), None);
        assert_eq!(toml_edition("[package]\nname = \"a\"\n"), None);
    }
}
