// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `atpkg-publish adopt trust=<build>`: MOVE A CHECKOUT'S `trust.lock`, judged by aterm's key
//! (docs/DESIGN-fast-build-release-2026-11-05.md A14, A16).
//!
//! The lock is the one place a tree names its compiler, and this verb is the one writer of it.
//! In a dedicated worktree, `<repo>-adopt-trust-<build>` on the branch `adopt/trust-<build>`, it
//!
//! 1. reads the build's facts from this Mac's atpkg store: the tree root recorded for it (or a
//!    fold of its tree, recorded), which the verified head index must sign for the build (a build
//!    the channel withdrew or floored is refused), and the compiler commit and lineage receipt
//!    its `LINEAGE` names, which its `trustc -vV` must answer;
//! 2. writes the sealed lock ([`lock_text`]) and the `.gitattributes` row that marks it
//!    generated, in a worktree whose every other change is committed, its `[stock]` the one the
//!    branch's lock names unless `--stock` names another;
//! 3. resolves the toolchain there as every build will (`atpkg::toolchain`), which must be the
//!    locked build itself, with nothing that takes its compiler's place (`TRUST_STAGE2_BIN`,
//!    `RUSTC`, `CARGO_BUILD_RUSTC`, a `build.rustc`: `atpkg::toolchain::rustc_movers`);
//! 4. runs [`KEY`] from the worktree's `publish/config.sh` on that build, each `targo` step
//!    placed and admitted as the `targo` shim places and admits a build, unless the compiler of
//!    the branch's own head reports the same commit, when no verdict of the key can change;
//! 5. commits the lock only when the key is green (or skipped) and the branch's head and
//!    changes are what it judged, and pushes nothing.
//!
//! A red step leaves the lock written and uncommitted: the fixes the new compiler needs are
//! committed on the same branch, and the next run judges the lock and the fixes together.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use atpkg::toolchain::{Toolchain, rustc_movers};
use atpkg::trust_lock::{self, Found, TrustLock};

use crate::ctx::Ctx;
use crate::error::{Error, Result};

/// The verb's synopsis.
pub const SYNOPSIS: &str = "adopt trust=<N> [--repo DIR] [--base REV] [--stock X.Y.Z]";

/// The generator a lock's seal line names.
pub const GENERATOR: &str = "atpkg-publish adopt";

/// The `publish/config.sh` key whose steps judge a compiler move.
pub const KEY: &str = "TOOLCHAIN_ACCEPT_CMD_DEFAULT";

/// Where [`KEY`] is read, relative to the worktree.
pub const CONFIG: &str = "publish/config.sh";

/// The `.gitattributes` row that marks the lock generated, so `verify census` holds its seal.
pub const ATTRIBUTES_ROW: &str = "trust.lock linguist-generated text eol=lf";

/// `.gitattributes`, at the worktree's root.
const ATTRIBUTES: &str = ".gitattributes";

/// The base a new branch starts from when `--base` names none.
pub const DEFAULT_BASE: &str = "origin/main";

/// The override adopt refuses: it would take the lock's place in every build of the key.
const STAGE2_ENV: &str = "TRUST_STAGE2_BIN";

/// The most of a `LINEAGE` this reads.
const LINEAGE_MAX: u64 = 64 << 10;

/// What one adoption is given: everything it reads from the machine, explicit.
#[derive(Clone)]
pub struct Inputs {
    /// The atpkg prefix whose store holds the build.
    pub prefix: PathBuf,
    /// rustup's home, for the branch head's resolution when its tree has no lock.
    pub rustup_home: PathBuf,
    /// The `PATH` children inherit, behind the locked build's `bin`.
    pub path_env: OsString,
    /// `$CARGO_HOME` (else `~/.cargo`), whose config may name another compiler.
    pub cargo_home: PathBuf,
    /// `RUSTC` and `CARGO_BUILD_RUSTC`, each when set
    /// (`atpkg::toolchain::rustc_movers::RUSTC_VARS`).
    pub rustc_vars: Vec<(&'static str, String)>,
    /// `TRUST_STAGE2_BIN`, when set: refused, since it would take the lock's place in every build.
    pub stage2_env: Option<OsString>,
    /// The tree root the verified head index signs for a Trust build on this Mac
    /// (`atpkg::cli::signed_tree_root`), or why it signs none.
    pub signed_root: SignedRoot,
    /// A directory in the repository; adopt works beside its main checkout.
    pub repo: PathBuf,
    /// The revision a new `adopt/trust-<build>` branch starts from.
    pub base: String,
    /// The Trust store build to lock.
    pub build: u64,
    /// The exact upstream release the lock's `[stock]` names; `None` keeps the `[stock]` of the
    /// lock the branch's head carries.
    pub stock: Option<String>,
}

/// The verified head index's answer for a Trust build: the tree root it signs on this Mac, or
/// why it signs none.
pub type SignedRoot =
    std::sync::Arc<dyn Fn(u64) -> std::result::Result<String, String> + Send + Sync>;

/// What the key did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Key {
    /// Every step exited 0: the steps, as `config.sh` spells them.
    Green(Vec<String>),
    /// Not run, and why.
    Skipped(String),
}

impl Key {
    /// One line for a report and the commit message.
    #[must_use]
    pub fn line(&self) -> String {
        match self {
            Key::Green(steps) => format!(
                "{KEY} green on the locked build ({} steps: {})",
                steps.len(),
                steps
                    .iter()
                    .map(|s| format!("`{s}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Key::Skipped(why) => format!("{KEY} skipped: {why}"),
        }
    }
}

/// A finished adoption.
#[derive(Clone, Debug)]
pub struct Adopted {
    /// The lock written.
    pub lock: TrustLock,
    /// `adopt/trust-<build>`.
    pub branch: String,
    /// The dedicated worktree.
    pub worktree: PathBuf,
    /// The branch's head after the run.
    pub commit: String,
    /// Whether this run made that commit (`false`: the branch already carried this lock).
    pub committed: bool,
    /// What the key did.
    pub key: Key,
    /// The `[stock]` of the lock the branch's head carried, when it carried one that reads.
    pub stock_was: Option<String>,
}

impl Adopted {
    /// The lock's `[stock]`, and whether it moved from the branch's.
    #[must_use]
    pub fn stock_line(&self) -> String {
        stock_line(&self.lock.stock_toolchain, self.stock_was.as_deref())
    }
}

/// `stock` and where it stands against `was`, the `[stock]` it replaces.
fn stock_line(stock: &str, was: Option<&str>) -> String {
    match was {
        Some(was) if was == stock => format!("{stock} (unchanged)"),
        Some(was) => format!("{stock} (moved from {was})"),
        None => format!("{stock} (the branch carried no lock)"),
    }
}

/// `adopt/trust-<build>`.
#[must_use]
pub fn branch(build: u64) -> String {
    format!("adopt/trust-{build}")
}

/// `atpkg-publish adopt trust=<N> [--repo DIR] [--base REV] [--stock X.Y.Z]`: [`adopt`] with the
/// store at `ctx.pkg_prefix`, `RUSTUP_HOME` (else `~/.rustup`), `PATH`, `CARGO_HOME` (else
/// `~/.cargo`), the compiler variables, the cwd's repository, and atpkg's verified head index.
/// Each environment variable is read through `var` (the CLI passes the process's).
///
/// # Errors
/// Usage for a bad line; [`adopt`]'s refusals; could-not-run when the cwd cannot be read.
pub fn run(ctx: &Ctx, args: &[String], var: &dyn Fn(&str) -> Option<OsString>) -> Result<()> {
    let flags = Flags::parse(args)?;
    let repo = match flags.repo {
        Some(repo) => repo,
        None => std::env::current_dir().map_err(|e| Error::io("read the current directory", &e))?,
    };
    let inputs = Inputs {
        prefix: ctx.pkg_prefix.clone(),
        rustup_home: atpkg::toolchain::rustup_home(var("RUSTUP_HOME").as_deref(), &ctx.home),
        path_env: var("PATH").unwrap_or_default(),
        cargo_home: var("CARGO_HOME")
            .map(PathBuf::from)
            .filter(|home| home.is_absolute())
            .unwrap_or_else(|| ctx.home.join(".cargo")),
        rustc_vars: rustc_movers::RUSTC_VARS
            .iter()
            .filter_map(|k| Some((*k, var(k)?.to_string_lossy().into_owned())))
            .collect(),
        stage2_env: var(STAGE2_ENV).filter(|v| !v.is_empty()),
        signed_root: std::sync::Arc::new(|build| {
            atpkg::cli::signed_tree_root(trust_lock::PROGRAM, build)
        }),
        repo,
        base: flags.base,
        build: flags.build,
        stock: flags.stock,
    };
    let adopted = adopt(&inputs)?;
    let lock = &adopted.lock;
    println!("adopted Trust build {} on {}", lock.build, adopted.branch);
    println!("  worktree: {}", adopted.worktree.display());
    if adopted.committed {
        println!("  commit:   {}", adopted.commit);
    } else {
        println!(
            "  commit:   {} (already carries this lock; nothing new to commit)",
            adopted.commit
        );
    }
    println!("  tree root: {}", lock.tree_root);
    println!("  compiler commit: {}", lock.compiler_commit);
    println!("  receipt: {}", lock.receipt);
    println!("  stock: {}", adopted.stock_line());
    println!("  key: {}", adopted.key.line());
    println!("  not pushed");
    Ok(())
}

/// The command line, parsed.
struct Flags {
    build: u64,
    repo: Option<PathBuf>,
    base: String,
    stock: Option<String>,
}

impl Flags {
    fn parse(args: &[String]) -> Result<Flags> {
        let mut build = None;
        let mut repo = None;
        let mut base = None;
        let mut stock = None;
        let mut words = args.iter();
        while let Some(word) = words.next() {
            let (flag, inline) = match word.split_once('=') {
                Some((flag, value)) if flag.starts_with("--") => (flag, Some(value.to_string())),
                _ => (word.as_str(), None),
            };
            let mut value = || {
                inline
                    .clone()
                    .or_else(|| words.next().cloned())
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| Error::usage(format!("{flag} needs a value ({SYNOPSIS})")))
            };
            match flag {
                "--repo" => repo = Some(PathBuf::from(value()?)),
                "--base" => base = Some(value()?),
                "--stock" => stock = Some(value()?),
                _ if flag.starts_with('-') => {
                    return Err(Error::usage(format!(
                        "adopt: unknown flag `{word}` ({SYNOPSIS})"
                    )));
                }
                _ => {
                    if build.is_some() {
                        return Err(Error::usage(format!(
                            "adopt takes one trust=<N>, and `{word}` is a second ({SYNOPSIS})"
                        )));
                    }
                    build = Some(parse_build(word)?);
                }
            }
        }
        let build =
            build.ok_or_else(|| Error::usage(format!("adopt needs trust=<N> ({SYNOPSIS})")))?;
        Ok(Flags {
            build,
            repo,
            base: base.unwrap_or_else(|| DEFAULT_BASE.to_string()),
            stock,
        })
    }
}

/// The build `trust=<N>` names.
fn parse_build(word: &str) -> Result<u64> {
    let Some(number) = word.strip_prefix(&format!("{}=", trust_lock::PROGRAM)) else {
        return Err(Error::usage(format!(
            "adopt takes trust=<N>, the Trust store build to lock: `{word}` names no Trust build"
        )));
    };
    number
        .parse::<u64>()
        .ok()
        .filter(|n| {
            number.bytes().all(|b| b.is_ascii_digit())
                && atpkg::lease::Subject::build(trust_lock::PROGRAM, *n).is_some()
        })
        .ok_or_else(|| Error::usage(format!("`{number}` is not a Trust build number")))
}

fn is_release_number(text: &str) -> bool {
    let mut parts = text.split('.');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(a), Some(b), Some(c), None) => [a, b, c]
            .into_iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())),
        _ => false,
    }
}

fn is_lower_hex(text: &str, width: usize) -> bool {
    text.len() == width
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// ADOPT `inputs.build`: write its sealed `trust.lock` on `adopt/trust-<build>` in the dedicated
/// worktree beside the repository's main checkout, run the key when the compiler changes, and
/// commit the lock when it is green (module docs).
///
/// # Errors
/// Refused: `TRUST_STAGE2_BIN` is set; the store does not hold the build (naming `aterm pkg
/// install trust --build N`); the verified head index does not sign that tree for it (withdrawn,
/// below its floor, another tree, unreachable); its `trustc -vV` is not the compiler its `LINEAGE`
/// names; the worktree path is something else; no `--stock` names a release and the branch's head
/// carries no lock to keep one from; a change besides the lock is uncommitted there; discovery
/// there does not resolve the locked build (a dev-linked checkout overrides it); a `RUSTC`,
/// `CARGO_BUILD_RUSTC` or `build.rustc` names another compiler; a key step fails (the lock stays
/// written and uncommitted); the branch's head or changes moved while the key ran. Usage for a
/// malformed `--stock`. Could-not-run when git, the store or a step cannot be run.
pub fn adopt(inputs: &Inputs) -> Result<Adopted> {
    if let Some(stock) = inputs.stock.as_deref().filter(|s| !is_release_number(s)) {
        return Err(Error::usage(format!(
            "--stock `{stock}` is not an exact release (`1.97.1`)"
        )));
    }
    if let Some(set) = &inputs.stage2_env {
        return Err(Error::refused(format!(
            "{STAGE2_ENV}={} is set: adopt judges the locked build, and the override would take \
             its place in every build of {KEY}; unset {STAGE2_ENV} and adopt again",
            Path::new(set).display()
        )));
    }
    let (facts, _lease) = facts(&inputs.prefix, inputs.build)?;
    let signed = (inputs.signed_root)(inputs.build).map_err(|why| {
        Error::refused(format!(
            "the verified head index signs no Trust build {} for this Mac ({why}): a lock names \
             only a build every Mac can install",
            inputs.build
        ))
    })?;
    if signed != facts.tree_root {
        return Err(Error::refused(format!(
            "this Mac's store holds Trust build {} with tree root {}, and the verified head index \
             signs {} for it: reinstall it (`aterm pkg install trust --build {}`), then adopt again",
            inputs.build, facts.tree_root, signed, inputs.build
        )));
    }
    let repo = main_checkout(&inputs.repo)?;
    let base = git_line(
        &repo,
        [
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{}^{{commit}}", inputs.base),
        ],
    )
    .map_err(|_| {
        Error::refused(format!(
            "--base `{}` names no commit in {}: fetch it, or name another",
            inputs.base,
            repo.display()
        ))
    })?;
    let branch = branch(inputs.build);
    let worktree = worktree(&repo, &branch, &base, inputs.build)?;
    // What the key judges is the branch as it stands, which a reused branch need not share with
    // `--base`: its head decides the compiler the move is from and the `[stock]` that carries over.
    let head = git_line(&worktree, ["rev-parse", "HEAD"])?;
    let head_lock = show(&worktree, &head, trust_lock::LOCK_FILE)?
        .and_then(|text| trust_lock::parse(&text).ok());
    let stock_was = head_lock.as_ref().map(|l| l.stock_toolchain.clone());
    let stock = inputs
        .stock
        .clone()
        .or_else(|| stock_was.clone())
        .ok_or_else(|| {
            Error::refused(format!(
                "{branch}'s head {} carries no {} whose [stock] carries over: name the stock \
                 release with --stock X.Y.Z",
                short(&head),
                trust_lock::LOCK_FILE
            ))
        })?;
    let lock = facts.lock(stock);
    let attributes = attributes_with_row(&worktree)?;
    uncommitted_refusal(&worktree, &branch, inputs.build, attributes.as_deref())?;
    if worktree.join(".gitmodules").is_file() {
        git(&worktree, ["submodule", "update", "--init", "--recursive"])?;
    }
    mover_refusal(&worktree, inputs, &lock)?;
    let text = lock_text(&lock)?;
    crate::lane::report::write_atomic(&worktree.join(trust_lock::LOCK_FILE), &text)?;
    if let Some(attributes) = &attributes {
        crate::lane::report::write_atomic(&worktree.join(ATTRIBUTES), attributes)?;
    }
    eprintln!(
        "adopt: wrote {} for Trust build {} in {}",
        trust_lock::LOCK_FILE,
        lock.build,
        worktree.display()
    );
    let targo = locked_targo(&worktree, inputs, &lock)?;
    let key = match compiler_at(&worktree, &head, inputs) {
        Ok((commit, what)) if commit == lock.compiler_commit => {
            let why = format!(
                "{branch}'s head compiles with {what}, which reports commit {commit}, the locked \
                 build's own: no verdict of the key can change"
            );
            eprintln!("adopt: {KEY} skipped: {why}");
            Key::Skipped(why)
        }
        resolved => {
            match &resolved {
                Ok((commit, what)) => eprintln!(
                    "adopt: the compiler moves from commit {commit} ({what}) to {}: running {KEY}",
                    lock.compiler_commit
                ),
                Err(why) => {
                    eprintln!("adopt: the head's compiler is unknown ({why}): running {KEY}")
                }
            }
            Key::Green(run_key(&worktree, &targo, inputs, &branch)?)
        }
    };
    unmoved(
        &worktree,
        &head,
        &branch,
        inputs.build,
        attributes.as_deref(),
    )?;
    let on = format!(
        "{branch} at {} (made from {} at {})",
        short(&head),
        inputs.base,
        short(&base)
    );
    let stock_said = stock_line(&lock.stock_toolchain, stock_was.as_deref());
    let (commit, committed) = commit(
        &worktree,
        &lock,
        &key,
        attributes.is_some(),
        &on,
        &stock_said,
    )?;
    Ok(Adopted {
        lock,
        branch,
        worktree,
        commit,
        committed,
        key,
        stock_was,
    })
}

fn short(sha: &str) -> &str {
    sha.get(..12).unwrap_or(sha)
}

/// Refused when the key's children would compile with another compiler than `lock`'s: the first
/// of what names one in `worktree` (`atpkg::toolchain::rustc_movers`), resolved on the key's
/// `PATH` (the locked build's `bin` first), must answer the lock's commit.
fn mover_refusal(worktree: &Path, inputs: &Inputs, lock: &TrustLock) -> Result<()> {
    let var = |key: &str| {
        inputs
            .rustc_vars
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.clone())
    };
    let movers = rustc_movers::rustc_movers(Some(worktree), &var, Some(&inputs.cargo_home));
    let path = key_path(
        &trust_lock::build_dir(&inputs.prefix, lock.build).join("bin"),
        inputs,
    )?;
    match rustc_movers::lock_refusal(&movers, &path, &worktree.join(trust_lock::LOCK_FILE), lock) {
        None => Ok(()),
        Some(why) => Err(Error::refused(format!(
            "{why}: adopt judges the locked build itself, then adopt again"
        ))),
    }
}

/// Refused when `worktree`'s branch moved while the key ran, or holds a change besides the lock
/// and adopt's `.gitattributes`: the commit lands only what the key judged.
fn unmoved(
    worktree: &Path,
    head: &str,
    branch: &str,
    build: u64,
    attributes: Option<&str>,
) -> Result<()> {
    let now = git_line(worktree, ["rev-parse", "HEAD"])?;
    if now != head {
        return Err(Error::refused(format!(
            "{branch} moved from {} to {} while {KEY} ran on the first: the lock stays written \
             and uncommitted; run adopt again to judge the branch as it stands",
            short(head),
            short(&now)
        )));
    }
    uncommitted_refusal(worktree, branch, build, attributes)
}

/// The sealed lock for `lock`: [`GENERATOR`]'s seal line, then `trust_lock::render_body`, stamped,
/// and proven to read back as `lock` with its seal holding.
///
/// # Errors
/// Could-not-run when the text does not read back as `lock` or its seal does not hold.
pub fn lock_text(lock: &TrustLock) -> Result<String> {
    let head = aterm_census::seal::line("#", GENERATOR);
    let text = aterm_census::seal::stamp(&format!("{head}\n{}", trust_lock::render_body(lock)))
        .map_err(Error::cannot_run)?;
    match trust_lock::parse(&text) {
        Ok(back) if back == *lock => {}
        Ok(back) => {
            return Err(Error::cannot_run(format!(
                "the lock written for {lock:?} reads back as {back:?}"
            )));
        }
        Err(why) => {
            return Err(Error::cannot_run(format!(
                "the lock written for {lock:?} does not read back: {why}"
            )));
        }
    }
    match aterm_census::seal::check(&text) {
        aterm_census::seal::Seal::Holds { .. } => Ok(text),
        other => Err(Error::cannot_run(format!(
            "the lock's seal does not hold: {other:?}"
        ))),
    }
}

/// What the store says of the build a lock names: everything the lock records but its `[stock]`.
struct Facts {
    build: u64,
    tree_root: String,
    lineage: Lineage,
}

impl Facts {
    /// The lock these facts make with `stock` for its `[stock]`.
    fn lock(self, stock: String) -> TrustLock {
        TrustLock {
            build: self.build,
            tree_root: self.tree_root,
            compiler_commit: self.lineage.compiler_commit,
            receipt: self.lineage.receipt,
            stock_toolchain: stock,
        }
    }
}

/// The lock's facts for store build `build`, and a lease that holds the build while adopt runs.
fn facts(prefix: &Path, build: u64) -> Result<(Facts, atpkg::lease::Lease)> {
    let dir = trust_lock::build_dir(prefix, build);
    let bin = dir.join("bin");
    let tool = |name: &str| bin.join(atpkg::toolchain::tool_file(name));
    if !atpkg::toolchain::is_executable_file(&tool("targo"))
        || !atpkg::toolchain::is_executable_file(&tool("trustc"))
    {
        return Err(Error::refused(format!(
            "this Mac's atpkg store ({}) holds no Trust build {build}: run `aterm pkg install \
             trust --build {build}`, then adopt again",
            prefix.display()
        )));
    }
    let subject = atpkg::lease::Subject::build(trust_lock::PROGRAM, build)
        .ok_or_else(|| Error::usage(format!("`{build}` is not a Trust build number")))?;
    let lease = atpkg::lease::take(
        prefix,
        &subject,
        &format!("atpkg-publish adopt trust={build}"),
        atpkg::lease::DEFAULT_WAIT,
    )
    .map_err(|e| Error::cannot_run(format!("lease Trust build {build}: {e}")))?;
    let tree_root = match trust_lock::recorded_tree_root(prefix, build) {
        Some(root) => root,
        None => trust_lock::fold_and_record(&dir)
            .map_err(|e| Error::io(format!("fold the tree of {}", dir.display()), &e))?
            .ok_or_else(|| {
                Error::cannot_run(format!(
                    "{} was re-staged while it folded: run adopt again",
                    dir.display()
                ))
            })?,
    };
    let lineage = Lineage::read(&dir)?;
    let reported = trust_lock::compiler_commit(&bin).ok_or_else(|| {
        Error::refused(format!(
            "{} answers `-vV` with no commit-hash: no lock can name its compiler",
            tool("trustc").display()
        ))
    })?;
    if reported != lineage.compiler_commit {
        return Err(Error::refused(format!(
            "Trust build {build}'s {} names compiler commit {}, and its bin/trustc reports {}: the \
             build is not the compiler its lineage proves; reinstall it (`aterm pkg install trust \
             --build {build}`)",
            atpkg::lineage::LINEAGE_FILE,
            lineage.compiler_commit,
            reported
        )));
    }
    Ok((
        Facts {
            build,
            tree_root,
            lineage,
        },
        lease,
    ))
}

/// What a build's `LINEAGE` says that the lock records.
struct Lineage {
    compiler_commit: String,
    /// `<lineage_receipt> sha256:<lineage_receipt_sha256>`.
    receipt: String,
}

impl Lineage {
    /// The `LINEAGE` of the build at `dir`: `schema`, `compiler_commit`, `lineage_receipt` and
    /// `lineage_receipt_sha256`, one `key=value` per line, `#` comments.
    fn read(dir: &Path) -> Result<Lineage> {
        use std::io::Read as _;
        let path = dir.join(atpkg::lineage::LINEAGE_FILE);
        let unreadable = |why: String| {
            Error::refused(format!(
                "{}: {why}: nothing names this build's compiler and lineage receipt",
                path.display()
            ))
        };
        let file = std::fs::File::open(&path).map_err(|e| unreadable(e.to_string()))?;
        if !file.metadata().is_ok_and(|m| m.is_file()) {
            return Err(unreadable("not a regular file".to_string()));
        }
        let mut text = String::new();
        file.take(LINEAGE_MAX)
            .read_to_string(&mut text)
            .map_err(|e| unreadable(e.to_string()))?;
        let mut fields = std::collections::BTreeMap::new();
        for line in text.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                return Err(unreadable(format!("`{line}` is not key=value")));
            };
            let key = key.trim();
            if fields.insert(key, value.trim()).is_some() {
                return Err(unreadable(format!("`{key}` is given twice")));
            }
        }
        let field = |key: &str| fields.get(key).copied().unwrap_or("");
        if field("schema") != atpkg::lineage::LINEAGE_SCHEMA {
            return Err(unreadable(format!(
                "schema `{}` is not {}",
                field("schema"),
                atpkg::lineage::LINEAGE_SCHEMA
            )));
        }
        let compiler_commit = field("compiler_commit");
        if !is_lower_hex(compiler_commit, 40) {
            return Err(unreadable(format!(
                "compiler_commit `{compiler_commit}` is not 40 lowercase hex digits"
            )));
        }
        let receipt = field("lineage_receipt");
        if receipt.is_empty() || receipt.contains(char::is_whitespace) {
            return Err(unreadable(format!(
                "lineage_receipt `{receipt}` names no receipt"
            )));
        }
        let digest = field("lineage_receipt_sha256");
        if !is_lower_hex(digest, 64) {
            return Err(unreadable(format!(
                "lineage_receipt_sha256 `{digest}` is not 64 lowercase hex digits"
            )));
        }
        Ok(Lineage {
            compiler_commit: compiler_commit.to_string(),
            receipt: format!("{receipt} sha256:{digest}"),
        })
    }
}

/// A git child in `dir` that never prompts.
fn git_command<I, S>(dir: &Path, args: I) -> (aterm_uds::spawnfd::Command, String)
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<OsString> = args
        .into_iter()
        .map(|a| a.as_ref().to_os_string())
        .collect();
    let shown = format!(
        "git -C {} {}",
        dir.display(),
        args.iter()
            .map(|a| a.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ")
    );
    let mut command = aterm_uds::spawnfd::command(aterm_uds::os_path!("git"));
    command
        .arg("-C")
        .arg(dir)
        .args(&args)
        .env("GIT_TERMINAL_PROMPT", "0");
    (command, shown)
}

/// `git -C <dir> <args…>`'s whole stdout.
fn git<I, S>(dir: &Path, args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let (mut command, shown) = git_command(dir, args);
    let out = command.output().map_err(|e| Error::io("spawn git", &e))?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    Err(Error::refused(format!(
        "`{shown}` failed: {}",
        stderr
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("(no message)")
    )))
}

/// [`git`]'s stdout, trimmed.
fn git_line<I, S>(dir: &Path, args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    git(dir, args).map(|out| out.trim().to_string())
}

/// Whether `git -C <dir> <args…>` exits 0 (`false` for 1).
fn git_holds<I, S>(dir: &Path, args: I) -> Result<bool>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let (mut command, shown) = git_command(dir, args);
    let out = command.output().map_err(|e| Error::io("spawn git", &e))?;
    match out.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(Error::refused(format!(
            "`{shown}` failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ))),
    }
}

fn physical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The main checkout of the repository `dir` is in: adopt's worktree is its sibling, wherever
/// adopt runs from.
fn main_checkout(dir: &Path) -> Result<PathBuf> {
    let list = git(dir, ["worktree", "list", "--porcelain"])?;
    let first = list.split("\n\n").next().unwrap_or_default();
    if first.lines().any(|l| l == "bare") {
        return Err(Error::refused(format!(
            "{} is a bare repository: adopt needs a checkout",
            dir.display()
        )));
    }
    let main = first
        .lines()
        .find_map(|l| l.strip_prefix("worktree "))
        .ok_or_else(|| {
            Error::cannot_run(format!(
                "`git worktree list` in {} names no main checkout",
                dir.display()
            ))
        })?;
    Ok(physical(Path::new(main)))
}

/// `<repo>-adopt-trust-<build>` on `branch`: made from `base` (or from `branch` when that exists
/// already), or the one an earlier run made.
fn worktree(repo: &Path, branch: &str, base: &str, build: u64) -> Result<PathBuf> {
    let name = repo
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| Error::cannot_run(format!("{} has no name", repo.display())))?;
    let dir = repo.with_file_name(format!("{name}-adopt-trust-{build}"));
    if std::fs::symlink_metadata(&dir).is_ok() {
        let common = |at: &Path| {
            git_line(
                at,
                ["rev-parse", "--path-format=absolute", "--git-common-dir"],
            )
            .map(|c| physical(Path::new(&c)))
        };
        let ours = git_line(&dir, ["rev-parse", "--show-toplevel"])
            .is_ok_and(|top| physical(Path::new(&top)) == physical(&dir))
            && common(&dir).ok() == common(repo).ok()
            && git_line(&dir, ["symbolic-ref", "--quiet", "--short", "HEAD"])
                .is_ok_and(|head| head == branch);
        if !ours {
            return Err(Error::refused(format!(
                "{} exists and is not {}'s worktree on {branch}: move it aside, then adopt again",
                dir.display(),
                repo.display()
            )));
        }
        eprintln!("adopt: reusing {} on {branch}", dir.display());
        return Ok(physical(&dir));
    }
    let made = if git_holds(
        repo,
        [
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )? {
        git(
            repo,
            [
                OsStr::new("worktree"),
                OsStr::new("add"),
                OsStr::new("--quiet"),
                dir.as_os_str(),
                OsStr::new(branch),
            ],
        )
    } else {
        git(
            repo,
            [
                OsStr::new("worktree"),
                OsStr::new("add"),
                OsStr::new("--quiet"),
                OsStr::new("-b"),
                OsStr::new(branch),
                dir.as_os_str(),
                OsStr::new(base),
            ],
        )
    };
    made?;
    eprintln!("adopt: made {} on {branch}", dir.display());
    Ok(physical(&dir))
}

/// Whether `.gitattributes` text has a row marking `trust.lock` generated.
fn has_lock_row(text: &str) -> bool {
    text.lines().any(|line| {
        let mut words = line.split_whitespace();
        matches!(words.next(), Some("trust.lock" | "/trust.lock"))
            && words.any(|w| w == "linguist-generated")
    })
}

/// `text` with [`ATTRIBUTES_ROW`] after its last `linguist-generated` row, or at its end.
#[must_use]
pub fn with_lock_row(text: &str) -> String {
    let mut lines: Vec<&str> = text.lines().collect();
    let last_generated = lines.iter().rposition(|line| {
        !line.trim_start().starts_with('#')
            && line
                .split_whitespace()
                .skip(1)
                .any(|w| w == "linguist-generated")
    });
    match last_generated {
        Some(at) => lines.insert(at + 1, ATTRIBUTES_ROW),
        None => {
            if lines.last().is_some_and(|l| !l.trim().is_empty()) {
                lines.push("");
            }
            lines.push(ATTRIBUTES_ROW);
        }
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// The `.gitattributes` the worktree's commit needs: `HEAD`'s with [`ATTRIBUTES_ROW`] added, or
/// `None` when `HEAD`'s already marks the lock.
fn attributes_with_row(worktree: &Path) -> Result<Option<String>> {
    let tracked = !git_line(
        worktree,
        ["ls-tree", "--name-only", "HEAD", "--", ATTRIBUTES],
    )?
    .is_empty();
    let head = if tracked {
        git(worktree, ["show", &format!("HEAD:{ATTRIBUTES}")])?
    } else {
        String::new()
    };
    Ok((!has_lock_row(&head)).then(|| with_lock_row(&head)))
}

/// The paths `git status` lists in `worktree`, untracked files and submodules included.
fn changed_paths(worktree: &Path) -> Result<Vec<String>> {
    let out = git(
        worktree,
        [
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignore-submodules=none",
        ],
    )?;
    let mut paths = Vec::new();
    let mut entries = out.split('\0');
    while let Some(entry) = entries.next() {
        let (Some(code), Some(path)) = (entry.get(..2), entry.get(3..)) else {
            continue;
        };
        if code.starts_with(['R', 'C']) {
            // The rename's or copy's source follows as its own entry.
            entries.next();
        }
        paths.push(path.to_string());
    }
    Ok(paths)
}

/// Refused when `worktree` holds an uncommitted change other than the lock and the
/// `.gitattributes` adopt writes (`attributes`, when it writes one): what the key judges is the
/// branch's commits plus the lock, and only that lands.
fn uncommitted_refusal(
    worktree: &Path,
    branch: &str,
    build: u64,
    attributes: Option<&str>,
) -> Result<()> {
    let ours_attributes = || {
        attributes.is_some_and(|wanted| {
            std::fs::read_to_string(worktree.join(ATTRIBUTES)).is_ok_and(|now| now == wanted)
        })
    };
    let others: Vec<String> = changed_paths(worktree)?
        .into_iter()
        .filter(|path| path != trust_lock::LOCK_FILE)
        .filter(|path| !(path == ATTRIBUTES && ours_attributes()))
        .collect();
    if others.is_empty() {
        return Ok(());
    }
    let shown: Vec<&str> = others.iter().take(8).map(String::as_str).collect();
    Err(Error::refused(format!(
        "{} has uncommitted changes besides {} ({}{}): commit the fixes on {branch} first, then \
         run `atpkg-publish adopt trust={build}` again",
        worktree.display(),
        trust_lock::LOCK_FILE,
        shown.join(", "),
        if others.len() > shown.len() {
            ", …"
        } else {
            ""
        }
    )))
}

/// The `targo` every build in `worktree` resolves now that its lock is written: the locked
/// build's own, or Refused.
fn locked_targo(worktree: &Path, inputs: &Inputs, lock: &TrustLock) -> Result<PathBuf> {
    let found = trust_lock::found_for(worktree, Some(&inputs.prefix))
        .filter(|found| physical(&found.path) == physical(&worktree.join(trust_lock::LOCK_FILE)))
        .ok_or_else(|| {
            Error::cannot_run(format!(
                "{} finds no {} at its root after adopt wrote one",
                worktree.display(),
                trust_lock::LOCK_FILE
            ))
        })?;
    let pinned = atpkg::toolchain::pinned_channel(worktree);
    let toolchain = Toolchain::discover_with_store(
        None,
        &inputs.rustup_home,
        Some(&inputs.prefix),
        &inputs.path_env,
        pinned.as_deref(),
        Some(&found),
    );
    if let Some(why) = &toolchain.lock_override {
        return Err(Error::refused(format!(
            "{why}: adopt judges the locked build itself, and an override takes its place in \
             every build; release it (`aterm pkg unlink {}`) and adopt again",
            trust_lock::PROGRAM
        )));
    }
    if !toolchain.have_targo() {
        return Err(Error::refused(toolchain.missing_targo_label()));
    }
    let want = physical(&trust_lock::build_dir(&inputs.prefix, lock.build).join("bin"));
    if toolchain.stage2_dir != want {
        return Err(Error::refused(format!(
            "discovery in {} resolves {}, not Trust build {}'s {}",
            worktree.display(),
            toolchain.stage2_dir.display(),
            lock.build,
            want.display()
        )));
    }
    Ok(toolchain.targo)
}

/// `<rev>:<path>`'s text, or `None` when `rev` has no such file.
fn show(repo: &Path, rev: &str, path: &str) -> Result<Option<String>> {
    if git_line(repo, ["ls-tree", "--name-only", rev, "--", path])?.is_empty() {
        return Ok(None);
    }
    git(repo, ["show", &format!("{rev}:{path}")]).map(Some)
}

/// The commit the compiler of the tree at `rev` reports, and what resolved it: discovery over
/// that tree's own lock, or with no lock when it has none. `Err` says why it is unknown.
fn compiler_at(
    repo: &Path,
    rev: &str,
    inputs: &Inputs,
) -> std::result::Result<(String, String), String> {
    let lock = show(repo, rev, trust_lock::LOCK_FILE).map_err(|e| e.to_string())?;
    let found = lock.map(|text| Found {
        path: PathBuf::from(format!("{rev}:{}", trust_lock::LOCK_FILE)),
        lock: trust_lock::parse(&text),
    });
    if let Some(Found {
        lock: Ok(locked), ..
    }) = &found
    {
        // A build the store holds unrecorded is folded once, so discovery's read can stand on it.
        let _ = trust_lock::standing(&inputs.prefix, locked);
    }
    let pinned = show(repo, rev, "rust-toolchain.toml")
        .map_err(|e| e.to_string())?
        .and_then(|text| atpkg::toolchain::pinned_channel_in(&text));
    let toolchain = Toolchain::discover_with_store(
        None,
        &inputs.rustup_home,
        Some(&inputs.prefix),
        &inputs.path_env,
        pinned.as_deref(),
        found.as_ref(),
    );
    if !toolchain.have_targo() {
        return Err(toolchain.missing_targo_label());
    }
    let what = match &found {
        Some(Found {
            lock: Ok(locked), ..
        }) => format!("its lock's build {}", locked.build),
        _ => format!("{}, with no lock", toolchain.stage2_dir.display()),
    };
    trust_lock::compiler_commit(&toolchain.stage2_dir)
        .map(|commit| (commit, what))
        .ok_or_else(|| {
            format!(
                "{}'s trustc reports no commit-hash",
                toolchain.stage2_dir.display()
            )
        })
}

/// [`KEY`]'s steps from the worktree's `publish/config.sh`: split on ` && `, each on whitespace.
pub(crate) fn key_steps(worktree: &Path) -> Result<Vec<Vec<String>>> {
    let path = worktree.join(CONFIG);
    let text = std::fs::read_to_string(&path).map_err(|e| {
        Error::refused(format!(
            "{}: {e}: no {KEY} judges this compiler move",
            path.display()
        ))
    })?;
    let prefix = format!("{KEY}=\"");
    let rows: Vec<&str> = text
        .lines()
        .filter_map(|l| l.strip_prefix(&prefix))
        .collect();
    let [row] = rows.as_slice() else {
        return Err(Error::refused(format!(
            "{} holds {} {KEY} lines: it needs exactly one to judge this compiler move",
            path.display(),
            rows.len()
        )));
    };
    let line = row
        .trim_end()
        .strip_suffix('"')
        .filter(|l| !l.contains(['"', '\'']))
        .ok_or_else(|| {
            Error::refused(format!(
                "{}'s {KEY} is not one flat `{KEY}=\"step && step\"` line of unquoted words",
                path.display()
            ))
        })?;
    let steps: Vec<Vec<String>> = line
        .split(" && ")
        .map(|step| step.split_whitespace().map(str::to_string).collect())
        .collect();
    if steps.iter().any(Vec::is_empty) {
        return Err(Error::refused(format!(
            "{}'s {KEY} has an empty step: `{line}`",
            path.display()
        )));
    }
    Ok(steps)
}

/// The key's `PATH`: `bin` (the locked build's) first, then `inputs.path_env`.
fn key_path(bin: &Path, inputs: &Inputs) -> Result<OsString> {
    std::env::join_paths(
        std::iter::once(bin.to_path_buf()).chain(std::env::split_paths(&inputs.path_env)),
    )
    .map_err(|e| Error::cannot_run(format!("PATH with {} first: {e}", bin.display())))
}

/// Run [`KEY`] in `worktree`, in order, stdio inherited: `targo` is the locked build's `targo`,
/// whose `bin` leads `PATH`, its build intermediates placed in the build cache the `targo` shim
/// places a build typed there in (`atpkg::build_cache::with_build_dir_from`: out of Spotlight and
/// fseventsd) and a build it claims run under build admission
/// (`atpkg::build_admit::run_if_claimed_at`), as the shim's hop runs it. Refused at the first step
/// that fails.
fn run_key(worktree: &Path, targo: &Path, inputs: &Inputs, branch: &str) -> Result<Vec<String>> {
    let steps = key_steps(worktree)?;
    let bin = targo.parent().unwrap_or(targo);
    let path = key_path(bin, inputs)?;
    let total = steps.len();
    let again = format!(
        "{} is written in {} and not committed: fix the reds there, commit the fixes on {branch}, \
         and run `atpkg-publish adopt trust={}` again",
        trust_lock::LOCK_FILE,
        worktree.display(),
        inputs.build
    );
    let mut ran = Vec::with_capacity(total);
    for (n, words) in steps.iter().enumerate() {
        let spelled = words.join(" ");
        let (program, args) = words
            .split_first()
            .expect("key_steps refuses an empty step");
        eprintln!("adopt: {KEY} step {}/{total}: {spelled}", n + 1);
        let could_not_start = |e: &dyn std::fmt::Display| {
            Error::cannot_run(format!(
                "{KEY} step {}/{total} `{spelled}` could not start: {e}; {again}",
                n + 1
            ))
        };
        let ok = if program == "targo" {
            run_targo_with_env(
                worktree,
                targo,
                args,
                &[(OsString::from("PATH"), path.clone())],
            )
            .map_err(|e| could_not_start(&e))?
        } else {
            let mut command = aterm_uds::spawnfd::command(program);
            command.args(args).current_dir(worktree).env("PATH", &path);
            command.status().map_err(|e| could_not_start(&e))?.success()
        };
        if !ok {
            return Err(Error::refused(format!(
                "{KEY} step {}/{total} `{spelled}` failed on Trust build {}: {again}",
                n + 1,
                inputs.build
            )));
        }
        ran.push(spelled);
    }
    Ok(ran)
}

/// Run an acceptance step with explicit compiler selection, using the shim's build
/// placement and resource admission. Canary and adopt differ only in their environment.
pub(crate) fn run_targo_with_env(
    worktree: &Path,
    targo: &Path,
    args: &[String],
    env: &[(OsString, OsString)],
) -> std::io::Result<bool> {
    let tool = atpkg::build_cache::Tool::Targo;
    let args = atpkg::build_cache::with_build_dir_from(args, tool, worktree)
        .unwrap_or_else(|| args.to_vec());
    #[cfg(unix)]
    if let Some(code) = atpkg::build_admit::run_if_claimed_at(
        targo,
        &args,
        tool,
        env,
        worktree,
        &atpkg::build_admit::Seat::live(),
    ) {
        return Ok(code == std::process::ExitCode::SUCCESS);
    }
    let mut command = aterm_uds::spawnfd::command(targo);
    command
        .args(&args)
        .current_dir(worktree)
        .envs(env.iter().cloned());
    Ok(command.status()?.success())
}

/// Commit the lock (and `.gitattributes` when adopt wrote it) on the worktree's branch: the head
/// after, and whether this made it (`false`: the branch already carried this lock).
fn commit(
    worktree: &Path,
    lock: &TrustLock,
    key: &Key,
    attributes: bool,
    on: &str,
    stock: &str,
) -> Result<(String, bool)> {
    let mut paths = vec![trust_lock::LOCK_FILE];
    if attributes {
        paths.push(ATTRIBUTES);
    }
    git(
        worktree,
        ["add", "--"].into_iter().chain(paths.iter().copied()),
    )?;
    let staged = !git_holds(
        worktree,
        ["diff", "--cached", "--quiet", "--"]
            .into_iter()
            .chain(paths.iter().copied()),
    )?;
    if staged {
        let message = format!(
            "trust.lock: adopt Trust build {build}\n\n\
             build: {build}\n\
             tree_root: {tree_root}\n\
             compiler_commit: {commit}\n\
             receipt: {receipt}\n\
             stock: {stock}\n\
             key: {key}\n\n\
             Written by `{GENERATOR} trust={build}` on {on}; not pushed.\n",
            build = lock.build,
            tree_root = lock.tree_root,
            commit = lock.compiler_commit,
            receipt = lock.receipt,
            key = key.line(),
        );
        git(
            worktree,
            ["commit", "--quiet", "-m", &message, "--"]
                .into_iter()
                .chain(paths.iter().copied()),
        )?;
    }
    Ok((git_line(worktree, ["rev-parse", "HEAD"])?, staged))
}
