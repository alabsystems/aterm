// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A PLAIN BUILD'S MEMORY CLAIM (`docs/DESIGN-adaptive-build-2026-10-24.md`, BUILD-ADMIT): a
//! `targo` or `cargo` call that compiles claims the memory it will take in this user's capacity
//! ledger before the build starts, and holds it until the build ends.
//!
//! What claims ([`classify`]): `build`, `check`, `test`, `bench`, `doc`, `rustc`, `rustdoc`,
//! `fix` and the lint (`tippy`, `clippy`, `targo-tippy`), and `targo trust` over one of them; a
//! `targo` verb other than the lint only in a lane it runs (`--unverified`, `trust`), since a bare
//! one is refused. Never `run` (which holds its program's whole life), an alias (`verify`, `lab`,
//! `ship`: each claims for the work it starts), `metadata`, `clean`, or a call for help or a
//! version: those start unclaimed, as before.
//!
//! What it claims ([`need`]): the most the same work took, read by [`aterm_capacity::book::chain`]
//! from what earlier builds of it kept here and elsewhere ([`aterm_capacity::book::TARGO_RECORD`])
//! and verify's run of the stage whose work holds its own, its analogue
//! ([`aterm_capacity::book::VERIFY_RECORD`]: a test build's `TestCompile`, a lint's `Tippy`), and
//! a fifth more; else a wider selection's; the cold figure where none is kept. A need at or beyond what can ever be available here asks to run alone, as verify's do,
//! and one the ledger finds can never fit beside others asks again alone.
//!
//! When it claims nothing ([`admit`]): a held claim of its own process or an ancestor's that
//! covers its tree's builds ([`aterm_capacity::covering`]) already holds its memory (a verify
//! stage, a Trust lane check, the build that runs this one's test); a process that holds a claim
//! of its own that does not cover, which a second claim would wait on, builds unclaimed and says
//! so. An unavailable or unreadable ledger refuses the build. An interrupt ends a wait
//! before the build starts.
//!
//! How the hidden verb runs it ([`front_exec`], BUILD-ADMIT's shim hop): the front, which is the
//! shell's job, claims, hands its claim to a holder that keeps it for the build's life ([`hold`]),
//! and becomes the build in its own pid with nothing else changed (its signal state put back as
//! it was started with it: `build_admit_exec.rs`), so every reader of that pid, its parent, its
//! job and its signals finds the build itself. A build started under a cargo that holds its
//! build lock (a build script's) runs under its verified covering claim; without one, an
//! ambiguous active lock bounds a wait to one admission look ([`possible_build_lock`]). Where
//! no holder arms, it runs as [`supervise`] runs it.
//!
//! How it runs under its front ([`supervise`], and the in-process callers'
//! [`run_if_claimed_at`]): the build is spawned as a pass-through, every descriptor this
//! process holds but its own close-on-exec ones handed on, the claim's among those kept back, so
//! no descendant holds the claim's lock; every catchable signal that ends a process with no core
//! is passed on, and the terminal keys' signals too where the terminal's key would not reach the
//! build by its group ([`front`]); and the build's status is this process's: a death by a
//! signal that dumps no core is this process's death by it, a core signal's `128 + n`
//! ([`end_like`]); what a build a signal ended left running is sent that signal too, and its
//! claim waits for it ([`end_tree`]). Its tree's memory is metered ([`aterm_capacity::tree_meter`])
//! and its peak kept when it succeeded, or when it failed above what it claimed ([`Held::end`]).

use std::path::{Path, PathBuf};

use aterm_capacity::book::{self, BuildNeed, Key, Records};
#[cfg(unix)]
use aterm_capacity::tree_meter::TreeMeter;
use aterm_capacity::{Admit, Claim, Error, Ledger, Machine, Need, Pace, Record};

use crate::build_cache::Tool;

/// The hidden verb that runs a build under its claim, `atpkg __build_admit [--shim <tool>
/// <placed>] <target> [args…]` ([`run_hidden`]): what a `targo` shim's hop execs.
pub const VERB: &str = aterm_uds::build_front::ADMIT_VERB;

/// What `atpkg __build_admit --capabilities` answers, alone on its line: this atpkg's front takes
/// the shim hop's words (`--shim <tool> <placed>`) and requires managed explicit targets.
/// Version-one fronts do not promise that ownership check, so a new shim cannot name them
/// in its hop ([`crate::build_cache::registered::helper_speaks_hop`]).
pub const HOP_CAPABILITY: &str = "aterm-build-admit-hop-v2";

/// The variable that admits a build at once (`now`) instead of waiting (`wait`, the default).
pub const ADMIT_ENV: &str = "ATERM_ADMIT";

/// How each of this module's lines starts.
const SAYS: &str = "build-admit";

/// The variable that points a debug build's [`Seat::live`] at a scratch ledger (its directory),
/// so a test that runs this binary's front never claims in this user's ledger, which
/// [`Ledger::user`] finds by the account's home whatever `$HOME` says, and admits it at once
/// unless [`ADMIT_ENV`] says otherwise, so the test's build never waits on this machine's memory
/// (a critical pressure holds every claim that waits, on an empty ledger too). A release build
/// never reads it.
#[cfg(any(test, debug_assertions))]
pub const LEDGER_ENV: &str = "ATERM_BUILD_ADMIT_LEDGER";

/// How [`Seat::live`] admits where [`ADMIT_ENV`] says nothing it takes: it waits, but at once in
/// a debug build on [`LEDGER_ENV`]'s scratch ledger.
fn unset_admission() -> Admit {
    #[cfg(any(test, debug_assertions))]
    if std::env::var_os(LEDGER_ENV).is_some_and(|dir| !dir.is_empty()) {
        return Admit::Now;
    }
    Admit::Wait
}

/// This user's ledger, or in a debug build the one [`LEDGER_ENV`] names.
fn live_ledger() -> Option<Ledger> {
    #[cfg(any(test, debug_assertions))]
    if let Some(dir) = std::env::var_os(LEDGER_ENV).filter(|dir| !dir.is_empty()) {
        return Some(Ledger::at(PathBuf::from(dir)));
    }
    Ledger::user()
}

/// The ledger a front claims in, as a reader of that front finds it: this user's, or in a debug
/// build the one [`LEDGER_ENV`] names in the front's own environment (`env`, which a reader
/// answers from what the front was started with), as the front's [`Seat::live`] finds it. A
/// release build never reads it.
#[must_use]
pub fn ledger_of_front(env: &dyn Fn(&str) -> Option<String>) -> Option<Ledger> {
    #[cfg(any(test, debug_assertions))]
    if let Some(dir) = env(LEDGER_ENV).filter(|dir| !dir.is_empty()) {
        return Some(Ledger::at(PathBuf::from(dir)));
    }
    #[cfg(not(any(test, debug_assertions)))]
    let _ = env;
    Ledger::user()
}

/// WHAT A FRONT'S LEDGER SAYS OF IT ([`front_claim`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrontClaim {
    /// Its claim waits for memory.
    Waiting {
        /// The memory it claims, bytes ([`Need::mem`]).
        mem: u64,
        /// The pid of each memory claim ahead of it, oldest first, as admission counts them: each
        /// one held, and each one that waits since earlier (none of its own process, whose oldest
        /// waiting claim this is); never a claim of no memory. A pid holding two is named twice.
        ahead: Vec<u32>,
    },
    /// Its claim is admitted: it execs its build, or, where no holder armed, runs it.
    Held,
}

/// What `ledger` says of the front `pid` (`aterm_uds::build_front::front`), read as `aterm pkg
/// capacity` reads it (no admissions' lock, nothing removed): [`front_claim_in`] its live claims.
///
/// # Errors
/// [`Ledger::list`]'s.
pub fn front_claim(ledger: &Ledger, pid: u32) -> std::io::Result<Option<FrontClaim>> {
    Ok(front_claim_in(&ledger.list()?, pid))
}

/// What `records` (a ledger's live claims, [`Ledger::list`]) say of the front `pid`: its oldest
/// claim that waits, with the claims ahead of it, else [`FrontClaim::Held`] where it holds one,
/// else `None` (none laid yet, or one it no longer needs: a front that claims nothing execs at
/// once). A reader of many fronts in one ledger lists it once and asks this of each.
#[must_use]
pub fn front_claim_in(records: &[Record], pid: u32) -> Option<FrontClaim> {
    let Some(mine) = records
        .iter()
        .filter(|r| r.pid == pid && r.held.is_none())
        .min_by_key(|r| r.order())
    else {
        return records
            .iter()
            .any(|r| r.pid == pid)
            .then_some(FrontClaim::Held);
    };
    let mut ahead: Vec<&Record> = records
        .iter()
        .filter(|r| r.order() != mine.order() && r.need.mem > 0)
        .filter(|r| r.held.is_some() || r.order() < mine.order())
        .collect();
    ahead.sort_by_key(|r| r.order());
    Some(FrontClaim::Waiting {
        mem: mine.need.mem,
        ahead: ahead.into_iter().map(|r| r.pid).collect(),
    })
}

/// What a build reads for its selection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Selection {
    /// The packages `-p` named, sorted, each once.
    Named(Vec<String>),
    /// The whole workspace (`--workspace`, `--all`).
    Workspace,
    /// What the manifest it reads selects: its package, or the workspace's default members.
    Here,
}

/// WHAT A CALL BUILDS, read for its claim ([`classify`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Build {
    /// The work: `targo test`, `targo trust check`, `cargo build`.
    pub stage: String,
    /// Verify's stage that does the same work or more of it, whose records are this one's
    /// analogue: its stage's whole work holds this build's (`TestCompile`, verify's
    /// `test --tests --no-run`, for a test build of no profile, target or features whose targets
    /// are among its: the libs, the bins and the integration tests; `Tippy`, verify's lint over
    /// `--all-targets`, for a lint of no profile, target or features); `None` for any other.
    pub analogue: Option<&'static str>,
    /// Its selection.
    pub selection: Selection,
    /// What else changes its work: its profile, target, targets and features, sorted.
    pub variant: Option<String>,
    /// The directory `-C` names.
    pub dir: Option<PathBuf>,
    /// The manifest `--manifest-path` names.
    pub manifest: Option<PathBuf>,
}

/// The flags before `--` that take the next word as their value and change neither the
/// selection nor the variant.
const VALUED: &[&str] = &[
    "--config",
    "-Z",
    "--color",
    "--explain",
    "--target-dir",
    "-j",
    "--jobs",
    "--message-format",
    "--lockfile-path",
    "--artifact-dir",
    "--out-dir",
    "--exclude",
];

/// The flags that name a target kind and take a name.
const NAMED_TARGETS: &[&str] = &["--bin", "--test", "--example", "--bench"];

/// The flags that change the features built and take no value.
const SHAPING: &[&str] = &["--all-features", "--no-default-features"];

/// The flags that name the kinds of target built and take no value.
const TARGET_KINDS: &[&str] = &[
    "--lib",
    "--bins",
    "--tests",
    "--examples",
    "--benches",
    "--all-targets",
    "--doc",
];

/// Whether a target word is among those a verify stage builds ([`in_test_compile`],
/// [`in_lint`]).
type Within = fn(&str) -> bool;

/// Whether verify's test compile (`test --tests --no-run`: every target with `test = true`, the
/// libs, the bins and the integration tests) builds the targets `target` names.
fn in_test_compile(target: &str) -> bool {
    matches!(target, "--lib" | "--bins" | "--tests")
        || target.starts_with("--bin ")
        || target.starts_with("--test ")
}

/// Whether verify's lint (`--all-targets`) lints the targets `target` names: every kind but the
/// doctests, which a lint has none of.
fn in_lint(target: &str) -> bool {
    target != "--doc"
}

/// The verb a word names, spelled whole; `None` for a word that does not compile.
fn compiling(word: &str) -> Option<&'static str> {
    Some(match word {
        "build" | "b" => "build",
        "check" | "c" => "check",
        "test" | "t" => "test",
        "bench" => "bench",
        "doc" | "d" => "doc",
        "rustc" => "rustc",
        "rustdoc" => "rustdoc",
        "fix" => "fix",
        "tippy" | "clippy" => "tippy",
        _ => return None,
    })
}

/// The next value: the one `--flag=value` carries, else the next word.
fn value<'a>(inline: Option<&'a str>, it: &mut dyn Iterator<Item = &'a str>) -> Option<&'a str> {
    inline.or_else(|| it.next())
}

/// WHETHER A CALL CLAIMS, and what it builds: `args` as `tool` reads them (a `cargo` call's
/// first word may name a toolchain, `+stable`). `None` for a call that never claims: one that
/// compiles nothing, asks for help, a version or a list, or only resolves what it would build
/// (`--unit-graph`, `--build-plan`: verify's shaped test compile asks for each call's unit graph
/// before any of its stages claims, and such a call would otherwise claim the whole build's peak
/// and wait in the ledger for it).
#[must_use]
pub fn classify(args: &[String], tool: Tool) -> Option<Build> {
    let mut it = args.iter().map(String::as_str).peekable();
    if tool == Tool::Cargo && it.peek().is_some_and(|a| a.starts_with('+')) {
        it.next();
    }
    let mut verb: Option<&str> = (tool == Tool::Tippy).then_some("tippy");
    let mut trust = false;
    let mut unverified = false;
    let mut named: Vec<String> = Vec::new();
    let mut workspace = false;
    let mut shape: Vec<String> = Vec::new();
    let mut targets: Vec<String> = Vec::new();
    let mut dir = None;
    let mut manifest = None;
    while let Some(a) = it.next() {
        if a == "--" {
            break;
        }
        let (flag, inline) = match a.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v)),
            _ => (a, None),
        };
        match flag {
            "-h" | "--help" | "-V" | "--version" | "--list" | "--unit-graph" | "--build-plan" => {
                return None;
            }
            "--unverified" => unverified = true,
            "-p" | "--package" => named.push(value(inline, &mut it)?.to_owned()),
            "--workspace" | "--all" => workspace = true,
            "-r" | "--release" => shape.push("release".to_owned()),
            "--profile" => shape.push(format!("profile {}", value(inline, &mut it)?)),
            "--target" => shape.push(format!("target {}", value(inline, &mut it)?)),
            "-F" | "--features" => shape.push(format!("features {}", value(inline, &mut it)?)),
            "-C" => dir = Some(PathBuf::from(value(inline, &mut it)?)),
            "--manifest-path" => manifest = Some(PathBuf::from(value(inline, &mut it)?)),
            f if NAMED_TARGETS.contains(&f) => {
                targets.push(format!("{f} {}", value(inline, &mut it)?));
            }
            f if TARGET_KINDS.contains(&f) => targets.push(f.to_owned()),
            f if SHAPING.contains(&f) => shape.push(f.to_owned()),
            f if VALUED.contains(&f) => {
                value(inline, &mut it)?;
            }
            _ => {
                if let Some(p) = a.strip_prefix("-p").filter(|p| !p.is_empty()) {
                    named.push(p.to_owned());
                } else if let Some(f) = a.strip_prefix("-F").filter(|f| !f.is_empty()) {
                    shape.push(format!("features {f}"));
                } else if let Some(d) = a.strip_prefix("-C").filter(|d| !d.is_empty()) {
                    dir = Some(PathBuf::from(d));
                } else if a.starts_with('-') {
                } else if verb.is_none() {
                    verb = Some(a);
                } else if verb == Some("trust") && !trust && tool == Tool::Targo {
                    trust = true;
                    verb = Some(a);
                }
            }
        }
    }
    let verb = compiling(verb?)?;
    let program = match tool {
        Tool::Cargo => "cargo",
        Tool::Targo | Tool::Tippy => "targo",
    };
    let lane = match tool {
        Tool::Cargo | Tool::Tippy => true,
        Tool::Targo => trust || unverified || verb == "tippy",
    };
    if !lane {
        return None;
    }
    let stage = if trust {
        format!("{program} trust {verb}")
    } else {
        format!("{program} {verb}")
    };
    let holds: Option<(&'static str, Within)> = match (program, trust, verb) {
        ("targo", false, "test") => Some(("TestCompile", in_test_compile)),
        ("targo", false, "tippy") => Some(("Tippy", in_lint)),
        _ => None,
    };
    let analogue = holds
        .filter(|(_, within)| shape.is_empty() && targets.iter().all(|t| within(t)))
        .map(|(stage, _)| stage);
    let mut variant = shape;
    variant.append(&mut targets);
    named.sort();
    named.dedup();
    let selection = if !named.is_empty() {
        Selection::Named(named)
    } else if workspace {
        Selection::Workspace
    } else {
        Selection::Here
    };
    variant.sort();
    variant.dedup();
    Some(Build {
        stage,
        analogue,
        selection,
        variant: (!variant.is_empty()).then(|| variant.join(", ")),
        dir,
        manifest,
    })
}

/// The package `toml` declares (its `[package]` table's `name`); `None` for a virtual manifest.
fn package_name(toml: &str) -> Option<String> {
    let mut in_package = false;
    for line in toml.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with('[') {
            let header = trimmed.split('#').next().unwrap_or(trimmed).trim();
            in_package = header == "[package]";
            continue;
        }
        if !in_package {
            continue;
        }
        let Some((k, v)) = trimmed.split_once('=') else {
            continue;
        };
        if k.trim() == "name" {
            let v = v.trim();
            let parsed = if let Some(quote) = v.chars().next().filter(|c| *c == '"' || *c == '\'') {
                let rest = &v[quote.len_utf8()..];
                rest.find(quote).map(|end| &rest[..end])?
            } else {
                v.split('#').next().unwrap_or(v).trim()
            };
            return (!parsed.is_empty()).then(|| parsed.to_owned());
        }
    }
    None
}

/// THE KEY A BUILD'S PEAK IS KEPT UNDER, run in `cwd`: its stage, the checkout it builds in (the
/// nearest directory holding `.git` above the one it reads its manifest from), its selection
/// (the package its nearest manifest declares where `-p` named none and it is no workspace build),
/// and its variant.
#[must_use]
pub fn key(build: &Build, cwd: &Path) -> Key {
    let base = build
        .dir
        .as_ref()
        .map_or_else(|| cwd.to_path_buf(), |d| cwd.join(d));
    let named = build.manifest.as_ref().map(|m| base.join(m));
    let from = named
        .as_deref()
        .and_then(Path::parent)
        .map_or(base.clone(), Path::to_path_buf);
    let checkout = from
        .ancestors()
        .find(|d| d.join(".git").exists())
        .unwrap_or(&from);
    let crates = match &build.selection {
        Selection::Named(names) => Some(names.clone()),
        Selection::Workspace => None,
        Selection::Here => named
            .or_else(|| {
                from.ancestors()
                    .map(|d| d.join("Cargo.toml"))
                    .find(|m| std::fs::symlink_metadata(m).is_ok_and(|meta| meta.is_file()))
            })
            .and_then(|m| crate::metadata_io::read_bounded_regular_utf8(&m, 1 << 20).ok())
            .and_then(|t| package_name(&t))
            .map(|name| vec![name]),
    };
    Key {
        stage: build.stage.clone(),
        checkout: checkout.to_string_lossy().into_owned(),
        crates,
        variant: build.variant.clone(),
    }
}

/// What a build kept under `key` claims on a machine of `physical` bytes: what `own` (the plain
/// builds' records) keeps for it, else, for a build that has one ([`Build::analogue`]), what
/// `verify` keeps for its analogue over the same selection ([`book::need`]), whose key has no
/// variant: verify's stage builds what its analogue's targets name and more.
#[must_use]
pub fn need(
    build: &Build,
    key: &Key,
    own: &Records,
    verify: &Records,
    physical: Option<u64>,
) -> BuildNeed {
    let like = build.analogue.map(|stage| Key {
        stage: stage.to_owned(),
        variant: None,
        ..key.clone()
    });
    book::need(own, key, like.as_ref().map(|l| (verify, l)), physical)
}

/// WHERE A BUILD CLAIMS: the ledger, the machine read, the records, how it is admitted, and who
/// asks. [`Seat::live`] is this user's; a test sets its own.
pub struct Seat<'m> {
    /// The ledger; `None` with no home to keep it under.
    pub ledger: Option<Ledger>,
    /// The machine read.
    pub machine: &'m dyn Machine,
    /// The plain builds' records; `None` with no state directory.
    pub own: Option<PathBuf>,
    /// Verify's records; `None` with no state directory.
    pub verify: Option<PathBuf>,
    /// Wait for the claim, or admit it at once.
    pub mode: Admit,
    /// How often a wait looks and speaks.
    pub pace: Pace,
    /// The process that asks.
    pub pid: u32,
    /// The cover marker it inherited ([`aterm_capacity::COVER_ENV`]).
    pub marker: Option<String>,
}

impl Seat<'static> {
    /// This user's ledger and records on the live machine, admitted as [`ADMIT_ENV`] says (a
    /// value it does not take is waited on, as unset; in a debug build on [`LEDGER_ENV`]'s
    /// scratch ledger, admitted at once), asked by this process.
    #[must_use]
    pub fn live() -> Seat<'static> {
        let state = aterm_types::dirs::account_state_dir();
        Seat {
            ledger: live_ledger(),
            machine: &aterm_capacity::Live,
            own: state.as_ref().map(|d| d.join(book::TARGO_RECORD)),
            verify: state.as_ref().map(|d| d.join(book::VERIFY_RECORD)),
            mode: std::env::var(ADMIT_ENV)
                .ok()
                .and_then(|v| Admit::parse(&v))
                .unwrap_or_else(unset_admission),
            pace: Pace::default(),
            pid: std::process::id(),
            marker: std::env::var(aterm_capacity::COVER_ENV).ok(),
        }
    }
}

/// WHAT A BUILD'S ADMISSION CAME TO ([`admit`]).
#[derive(Debug)]
pub enum Admission {
    /// A held claim of this process or an ancestor's covers its builds.
    Covered(Record),
    /// This process holds a claim of its own that does not cover, which a second claim would
    /// wait on: it builds unclaimed.
    Own(Record),
    /// Claimed: hold it while the build runs.
    Claimed(Box<Held>),
    /// An interrupt ended its wait: the build does not start.
    Stopped(Error),
    /// The ledger or claim is unavailable, or a nested build could wait on work waiting for
    /// its parent's unproven lock. No build started and no claim was forced.
    Refused(String),
}

/// A build's held claim, and where its peak is kept ([`Held::end`]).
#[derive(Debug)]
pub struct Held {
    claim: Claim,
    key: Key,
    own: Option<PathBuf>,
}

impl Held {
    /// The claim.
    #[must_use]
    pub const fn claim(&self) -> &Claim {
        &self.claim
    }

    /// The key its peak is kept under.
    #[must_use]
    pub const fn key(&self) -> &Key {
        &self.key
    }

    /// The build ended: let the claim go, then keep its peak ([`Ended::keep`]). The bytes kept;
    /// `None` when nothing was, or the record could not be written (said in one line).
    pub fn end(self, succeeded: bool, peak: u64, say: &mut dyn FnMut(&str)) -> Option<u64> {
        self.let_go().keep(succeeded, peak, say)
    }

    /// The build ended: LET THE CLAIM GO NOW, before its peak is read whole or kept (a meter's last
    /// read, a write of its book under that book's lock), so no waiter waits on a build that has
    /// ended; what keeps its peak is left ([`Ended::keep`]).
    #[must_use]
    pub fn let_go(self) -> Ended {
        let Held { claim, key, own } = self;
        let claimed = claim.record().need.mem;
        drop(claim);
        Ended { claimed, key, own }
    }
}

/// An ended build whose claim went ([`Held::let_go`]): what it claimed, the key its peak is kept
/// under, and where.
#[derive(Debug)]
pub struct Ended {
    claimed: u64,
    key: Key,
    own: Option<PathBuf>,
}

impl Ended {
    /// Keep `peak` under its key when something was read and it `succeeded`, or when it failed
    /// with a peak that would claim more than its claim did (a failed build stopped short of the
    /// work its key names, so its peak is the least that work takes: kept, it raises the key's
    /// next claim, and one below would lower it), unless the run was light beside the heavy peak
    /// its key keeps ([`Records::keep_unless_light`]: a fresh build that compiled nothing). A key
    /// whose builds keep failing (an edit-test loop's red runs, a build stopped for swap) so
    /// learns what its compile takes. The bytes kept; `None` when nothing was, or the record could
    /// not be written (said in one line).
    pub fn keep(self, succeeded: bool, peak: u64, say: &mut dyn FnMut(&str)) -> Option<u64> {
        let Ended { claimed, key, own } = self;
        let raises = aterm_capacity::peak_claim(peak) > claimed;
        match own.filter(|_| peak > 0 && (succeeded || raises)) {
            Some(path) => {
                let at = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
                match Records::keep_unless_light(&path, key, peak, at) {
                    Ok(kept) => kept.then_some(peak),
                    Err(e) => {
                        say(&format!(
                            "{SAYS}: its peak was not kept in {}: {e}",
                            path.display()
                        ));
                        None
                    }
                }
            }
            None => None,
        }
    }
}

/// Report a refusal without granting unclaimed execution after an admission failure.
fn refuse(say: &mut dyn FnMut(&str), why: &str) -> Admission {
    let line = format!("{SAYS}: {why}; no build started");
    say(&line);
    Admission::Refused(line)
}

/// CLAIM `build`'s memory, run in `cwd`, at `seat`, saying through `say` what it waits for
/// ([`aterm_capacity::admit_with`]), what refuses it and why it claims nothing.
pub fn admit(build: &Build, cwd: &Path, seat: &Seat<'_>, say: &mut dyn FnMut(&str)) -> Admission {
    admit_bounded(build, cwd, seat, &|| None, say)
}

fn admit_bounded(
    build: &Build,
    cwd: &Path,
    seat: &Seat<'_>,
    probe_lock: &dyn Fn() -> Option<PathBuf>,
    say: &mut dyn FnMut(&str),
) -> Admission {
    let Some(ledger) = &seat.ledger else {
        return refuse(say, "no home directory keeps the capacity ledger");
    };
    match aterm_capacity::covering(ledger, seat.machine, seat.pid, seat.marker.as_deref()) {
        Ok(Some(holder)) => return Admission::Covered(holder),
        Ok(None) => {}
        Err(e) => return refuse(say, &format!("the capacity ledger could not be read ({e})")),
    }
    match ledger.of_pid(seat.pid) {
        Ok(mine) => {
            if let Some(r) = mine.into_iter().find(|r| r.held.is_some()) {
                say(&format!(
                    "{SAYS}: this process holds a claim of its own ({:?}), which a second would \
                     wait on; the build runs unclaimed",
                    r.need.what
                ));
                return Admission::Own(r);
            }
        }
        Err(e) => return refuse(say, &format!("the capacity ledger could not be read ({e})")),
    }
    // Hints never precede verified authority, and an explicit mode choice needs no hint.
    let possible_lock = (seat.mode == Admit::Wait).then(probe_lock).flatten();
    let key = key(build, cwd);
    let own = seat.own.as_deref().map(Records::load).unwrap_or_default();
    let verify = seat
        .verify
        .as_deref()
        .map(Records::load)
        .unwrap_or_default();
    let reading = seat.machine.read(None);
    let need = need(build, &key, &own, &verify, reading.physical);
    let budget = reading.reachable().unwrap_or(u64::MAX);
    let what = format!("{} {}", key.stage, key.selection());
    let ask = |alone: bool| {
        let n = Need::new(what.clone())
            .mem(need.bytes)
            .checkout(key.checkout.clone())
            .covers();
        if alone { n.alone() } else { n }
    };
    let beyond = need.bytes >= budget;
    let mut told = false;
    let mut speak = |line: &str| {
        if !std::mem::replace(&mut told, true) {
            say(&format!("{SAYS}: {what} claims {need}"));
        }
        say(line);
    };
    let attempt = |need: Need, speak: &mut dyn FnMut(&str)| {
        if seat.mode != Admit::Wait || possible_lock.is_none() {
            return aterm_capacity::admit_with(
                ledger,
                seat.machine,
                need,
                seat.mode,
                seat.pace,
                speak,
            );
        }
        if let Some(why) = aterm_capacity::never(&need, &reading) {
            return Err(Error::Never(why));
        }
        let watch = aterm_toolchain::interrupt::Watch::enter();
        if let Some(signal) = watch.take() {
            return Err(Error::Stopped {
                signal: Some(signal),
                short: None,
            });
        }
        let polled = aterm_capacity::Waiter::enter(ledger, seat.machine, need)
            .map_err(Error::Ledger)?
            .poll(seat.machine);
        if let Some(signal) = watch.take() {
            drop(polled);
            return Err(Error::Stopped {
                signal: Some(signal),
                short: None,
            });
        }
        match polled {
            aterm_capacity::Polled::Admitted(claim) => Ok(claim),
            aterm_capacity::Polled::Waiting(waiter, short) => {
                drop(waiter);
                Err(Error::Stopped {
                    signal: None,
                    short: Some(short),
                })
            }
            // Its claim file was removed between its entry and its look: no look could admit
            // it, as `admit_waiting` ends such a wait.
            aterm_capacity::Polled::Gone(e) => Err(Error::Ledger(e)),
            aterm_capacity::Polled::Unqualified(why) => Err(Error::Timing(why)),
        }
    };
    let mut admitted = attempt(ask(beyond), &mut speak);
    if !beyond && matches!(admitted, Err(Error::Never(_))) {
        admitted = attempt(ask(true), &mut speak);
    }
    match admitted {
        Ok(claim) => Admission::Claimed(Box::new(Held {
            claim,
            key,
            own: seat.own.clone(),
        })),
        Err(e @ Error::Stopped { signal: None, .. }) if possible_lock.is_some() => {
            let line = format!(
                "{SAYS}: {what}: an ancestor has a held build-lock file {}, but its ownership is \
                 unproven; admission would wait ({e}) and could cycle; enter the outer build \
                 through the targo admission shim; no build started",
                possible_lock
                    .as_deref()
                    .unwrap_or_else(|| Path::new("?"))
                    .display(),
            );
            say(&line);
            Admission::Refused(line)
        }
        Err(e @ Error::Stopped { .. }) => {
            say(&format!(
                "{SAYS}: {what}: its memory claim was {e}; it did not start"
            ));
            Admission::Stopped(e)
        }
        Err(e) => refuse(say, &format!("{what}: its memory claim failed ({e})")),
    }
}

/// The largest resident peak of this process's reaped children, bytes (`getrusage`'s
/// `RUSAGE_CHILDREN`): the floor a front puts under its meter's read, true only of a process
/// whose one child was the build. 0 where it is unread.
#[cfg(all(
    unix,
    target_pointer_width = "64",
    any(target_vendor = "apple", target_os = "linux")
))]
#[must_use]
pub fn children_maxrss() -> u64 {
    /// `struct timeval` as this ABI lays it out.
    #[repr(C)]
    #[derive(Default)]
    struct Timeval {
        sec: i64,
        #[cfg(target_vendor = "apple")]
        usec: i32,
        #[cfg(target_vendor = "apple")]
        pad: i32,
        #[cfg(target_os = "linux")]
        usec: i64,
    }
    /// `struct rusage` as this ABI lays it out: two times, then fourteen longs.
    #[repr(C)]
    #[derive(Default)]
    struct NativeUsage {
        user: Timeval,
        system: Timeval,
        maxrss: i64,
        rest: [i64; 13],
    }
    /// `RUSAGE_CHILDREN` on both.
    const CHILDREN: i32 = -1;
    unsafe extern "C" {
        fn getrusage(who: i32, usage: *mut NativeUsage) -> i32;
    }
    let mut usage = NativeUsage::default();
    // SAFETY: `usage` is one writable `struct rusage` of the native layout, live across the call;
    // `getrusage` keeps no pointer to it.
    if unsafe { getrusage(CHILDREN, &raw mut usage) } != 0 {
        return 0;
    }
    let rss = u64::try_from(usage.maxrss).unwrap_or(0);
    if cfg!(target_vendor = "apple") {
        rss
    } else {
        rss.saturating_mul(1024)
    }
}

/// 0: no reader of the children's accounting on this platform.
#[cfg(all(
    unix,
    not(all(
        target_pointer_width = "64",
        any(target_vendor = "apple", target_os = "linux")
    ))
))]
#[must_use]
pub fn children_maxrss() -> u64 {
    0
}

/// RUN `command`, a pass-through build, under `held` ([`Admission::Claimed`]; `None`: unclaimed):
/// spawned with every signal that reaches this process for it passed on to it ([`front`]: every
/// catchable one that ends a process with no core, and the terminal keys' where the terminal's
/// key would not reach it itself), handed the claim's cover marker ([`aterm_capacity::COVER_ENV`])
/// so the builds it starts claim nothing more, its tree metered while a claim is held, and its
/// peak (never
/// below `floor`'s read once it is reaped) kept as [`Held::end`] keeps one. Its status.
///
/// # Errors
/// The spawn's or the wait's.
#[cfg(unix)]
pub fn supervise(
    held: Option<Held>,
    command: &mut aterm_uds::spawnfd::TransparentCommand,
    floor: &dyn Fn() -> u64,
    say: &mut dyn FnMut(&str),
) -> std::io::Result<std::process::ExitStatus> {
    if let Some(held) = &held {
        command.env(
            aterm_capacity::COVER_ENV,
            held.claim().record().cover_marker(),
        );
    }
    let front = front::Front::arm();
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            if let Some(front) = &front {
                front.never();
            }
            return Err(e);
        }
    };
    let meter = held.as_ref().map(|_| TreeMeter::start(child.id()));
    let status = match front {
        Some(front) => {
            front.aim(child.id(), meter.as_ref().map(TreeMeter::watch));
            let status = front.reap(&mut child);
            let passed = front.passed();
            front.disarm();
            if let (Some(meter), Ok(status)) = (&meter, &status) {
                end_tree(meter, passed, *status, say);
            }
            status
        }
        None => child.wait(),
    };
    if let (Some(held), Some(meter)) = (held, meter) {
        let ended = held.let_go();
        let peak = meter.stop(floor());
        ended.keep(
            status.as_ref().is_ok_and(std::process::ExitStatus::success),
            peak,
            say,
        );
    }
    status
}

/// The longest a claim waits for what a build ended by a signal left running ([`end_tree`]): a
/// one-minute hang detector, past which the claim ends anyway, said.
#[cfg(unix)]
pub const TREE_ENDS_WITHIN: std::time::Duration = std::time::Duration::from_mins(1);

/// How often [`end_tree`] reads what is left.
#[cfg(unix)]
const TREE_END_READ: std::time::Duration = std::time::Duration::from_millis(100);

/// END WHAT A BUILD ENDED BY A SIGNAL LEFT RUNNING, before its claim ends: a `cargo` killed by a
/// SIGTERM leaves its compilers and build scripts running, reparented away, holding memory its
/// claim no longer would. Where the front passed a signal on (`passed`), or the build died of one,
/// every process its tree leaves ([`TreeMeter::left`]) is sent that signal (a core signal's
/// death sends SIGTERM, so no crash is reported for them), each once, and the claim waits until
/// none is left, [`TREE_ENDS_WITHIN`] at most. A build that exited leaves what it started alone
/// (a daemon it meant to outlive it).
#[cfg(unix)]
fn end_tree(
    meter: &TreeMeter,
    passed: Option<i32>,
    status: std::process::ExitStatus,
    say: &mut dyn FnMut(&str),
) {
    use std::os::unix::process::ExitStatusExt as _;
    let Some(sig) = passed.or_else(|| {
        status.signal().map(|n| {
            if ENDS_WITHOUT_CORE.contains(&n) {
                n
            } else {
                libc::SIGTERM
            }
        })
    }) else {
        return;
    };
    let now = std::time::Instant::now();
    let until = now
        .checked_add(TREE_ENDS_WITHIN)
        .unwrap_or_else(|| now + std::time::Duration::from_secs(86_400 * 365));
    let mut sent: Vec<u32> = Vec::new();
    loop {
        let left = meter.left();
        if left.is_empty() {
            return;
        }
        for &pid in &left {
            if sent.contains(&pid) {
                continue;
            }
            sent.push(pid);
            if let Ok(pid) = libc::pid_t::try_from(pid) {
                // SAFETY: `kill` with two integers; `pid` is a process this build started, live
                // at the read just made, by its start.
                unsafe { libc::kill(pid, sig) };
            }
        }
        if std::time::Instant::now() >= until {
            say(&format!(
                "{SAYS}: {} of the build's processes outlived it by {}s: its claim ends anyway",
                left.len(),
                TREE_ENDS_WITHIN.as_secs()
            ));
            return;
        }
        std::thread::sleep(TREE_END_READ);
    }
}

/// Replace this process with `target` run on `args`, unchanged: what a call that claims nothing
/// runs. Returns only when the exec failed: 127, said.
fn exec<A: AsRef<std::ffi::OsStr>>(target: &Path, args: &[A]) -> std::process::ExitCode {
    exec_marked(target, args, None)
}

/// [`exec`], with [`aterm_capacity::COVER_ENV`] set to `marker` where one is given. On Unix the
/// build starts with the signal state this process was started with ([`becoming`]).
fn exec_marked<A: AsRef<std::ffi::OsStr>>(
    target: &Path,
    args: &[A],
    marker: Option<&str>,
) -> std::process::ExitCode {
    #[cfg(unix)]
    let err = becoming::become_build(target, args, marker);
    #[cfg(not(unix))]
    let err = {
        #[expect(
            clippy::disallowed_types,
            reason = "a pass-through by exec: its caller's descriptors are the program's"
        )]
        let mut command = std::process::Command::new(target);
        aterm_uds::spawn::set_flags(&mut command, 0);
        command.args(args);
        if let Some(marker) = marker {
            command.env(aterm_capacity::COVER_ENV, marker);
        }
        crate::platform::exec_or_run(&mut command)
    };
    eprintln!("atpkg {VERB}: failed to exec {}: {err}", target.display());
    std::process::ExitCode::from(127)
}

/// The tool `target` is, by its name: the lint's own binary, else `targo`.
#[cfg(unix)]
fn tool_of(target: &Path) -> Tool {
    match target.file_name().and_then(|n| n.to_str()) {
        Some("targo-tippy") => Tool::Tippy,
        _ => Tool::Targo,
    }
}

/// RUN `target` ON `args` UNDER ITS CLAIM, when the call claims one, as `tool` reads it, run in
/// `cwd` at `seat`, with `env` set over this process's environment for it: its end, as this
/// process's ([`supervise`]; a signal with no core that ended it ends this process), or how an
/// interrupt that ended the wait would have ended it ([`end_like`]). `None` where nothing was
/// claimed (no build that claims, one a held claim covers, one this process's own claim would
/// hold up), and the caller runs `target` as it would have. A refused admission returns an
/// exit status and never delegates an unclaimed launch to the caller.
#[cfg(unix)]
pub fn run_if_claimed_at(
    target: &Path,
    args: &[String],
    tool: Tool,
    env: &[(std::ffi::OsString, std::ffi::OsString)],
    cwd: &Path,
    seat: &Seat<'_>,
) -> Option<std::process::ExitCode> {
    let build = classify(args, tool)?;
    let mut say = |line: &str| eprintln!("{line}");
    match admit(&build, cwd, seat, &mut say) {
        Admission::Claimed(held) => {
            let mut command = aterm_uds::spawnfd::transparent(target);
            command.args(args);
            for (k, v) in env {
                command.env(k, v);
            }
            Some(
                match supervise(Some(*held), &mut command, &children_maxrss, &mut say) {
                    Ok(status) => end_like(status),
                    Err(e) => {
                        eprintln!("atpkg {VERB}: {}: {e}", target.display());
                        std::process::ExitCode::from(127)
                    }
                },
            )
        }
        Admission::Stopped(Error::Stopped {
            signal: Some(sig), ..
        }) => Some(stopped_by(sig)),
        Admission::Stopped(_) => Some(std::process::ExitCode::from(1)),
        Admission::Refused(_) => Some(std::process::ExitCode::from(75)),
        Admission::Covered(_) | Admission::Own(_) => None,
    }
}

/// [`run_if_claimed_at`] in this process's directory, at this user's seat ([`Seat::live`]);
/// `None` too where the directory is unread.
#[cfg(unix)]
#[must_use]
pub fn run_if_claimed(
    target: &Path,
    args: &[String],
    tool: Tool,
    env: &[(std::ffi::OsString, std::ffi::OsString)],
) -> Option<std::process::ExitCode> {
    let cwd = std::env::current_dir().ok()?;
    run_if_claimed_at(target, args, tool, env, &cwd, &Seat::live())
}

/// `atpkg __build_admit [--shim <tool> <placed>] <target> [args…]`: what a `targo` or
/// `targo-tippy` shim's hop runs once its block has placed the build ([`VERB`]); `atpkg
/// __build_admit --capabilities` answers [`HOP_CAPABILITY`] and runs nothing. The shim's form
/// first registers the call's build cache as the shim's own registration call did
/// ([`crate::build_cache::registered::observe_shim_call`]: a refusal is exit 75, no build
/// started); `<placed>` says what the block changed (`-` nothing, `config` its two leading words,
/// plus their directory exported for a call that
/// [`loses_config`](crate::build_cache::loses_config) (tippy, clippy), `env` only its export of
/// `CARGO_BUILD_BUILD_DIR` (the trust lane)), so the registration reads the call as typed.
/// Then a call that claims runs `target` under its claim in this pid ([`front_exec`]); other calls
/// become `target` by exec, their words handed on unchanged. A shim refuses non-UTF-8 words
/// because its cache placement cannot be established; a non-shim front still passes them on.
#[must_use]
pub fn run_hidden(argv: &[std::ffi::OsString]) -> std::process::ExitCode {
    if let [only] = argv
        && only == "--capabilities"
    {
        println!("{HOP_CAPABILITY}");
        return std::process::ExitCode::SUCCESS;
    }
    let (shim, argv) = match argv.split_first() {
        Some((first, rest)) if first == "--shim" => match rest {
            [tool, placed, rest @ ..] => {
                let tool = match tool.to_str() {
                    Some("targo") => Tool::Targo,
                    Some("targo-tippy") => Tool::Tippy,
                    _ => return usage(),
                };
                (Some((tool, placed.as_os_str())), rest)
            }
            _ => return usage(),
        },
        _ => (None, argv),
    };
    let Some((target, rest)) = argv.split_first() else {
        return usage();
    };
    let target = Path::new(target);
    let words = rest
        .iter()
        .map(|a| a.to_str().map(str::to_owned))
        .collect::<Option<Vec<String>>>();
    if let Some((tool, placed)) = shim {
        let registered = words
            .as_deref()
            .ok_or_else(|| std::io::Error::other("compiler-shim arguments are not UTF-8"))
            .and_then(|words| {
                crate::build_cache::registered::observe_shim_call(
                    words,
                    tool,
                    placed.to_str().unwrap_or(""),
                    std::env::current_dir().ok().as_deref(),
                )
            });
        if let Err(error) = registered {
            eprintln!("atpkg: build cache ownership refused: {error}; no build started");
            return std::process::ExitCode::from(75);
        }
    }
    #[cfg(unix)]
    if let Some(args) = words
        && let Ok(cwd) = std::env::current_dir()
    {
        let tool = shim.map_or_else(|| tool_of(target), |(tool, _)| tool);
        let seat = Seat::live();
        let possible_lock = || {
            let me = seat.pid;
            possible_build_lock(
                &|| {
                    seat.machine
                        .lineage(me)
                        .map(|line| line.ancestors(me))
                        .unwrap_or_default()
                },
                &|pid| aterm_uds::holders::open_files(pid).unwrap_or_default(),
                &|file| aterm_uds::ownerlock::probe(file) == aterm_uds::ownerlock::Owner::Live,
            )
        };
        return front_exec_bounded(target, &args, tool, &cwd, &seat, &possible_lock);
    }
    exec(target, rest)
}

/// Candidate lock-file names for a possible-cycle hint, never ownership or admission proof.
/// Cargo was observed opening these while building: its build directory's, its artifact
/// directory's, and the one an older cargo opened for both. Measured
/// (2026-10-29, `lsof` of the parent from a build script and from a test): targo 1.99.0-dev holds
/// all three open while a build script runs and none while a test or a `targo run` program runs;
/// cargo 1.95.0 holds `.cargo-lock` while a build script runs and none while a test runs.
pub const CARGO_LOCKS: &[&str] = &[".cargo-lock", ".cargo-build-lock", ".cargo-artifact-lock"];

/// A POSSIBLE WAIT CYCLE, NOT ADMISSION AUTHORITY: an ancestor has a Cargo-lock-named file
/// open and a kernel probe says a lock is held there. Neither proves it owns that lock. A
/// verified covering/own/credited claim still answers first; otherwise Wait gets one ordinary
/// admission look and a named refusal if it cannot run, rather than force a claim or deadlock.
/// An unlocked file, an unknown probe, and Cargo's environment words imply no exception.
#[must_use]
pub fn possible_build_lock(
    ancestors: &dyn Fn() -> Vec<u32>,
    open: &dyn Fn(u32) -> Vec<PathBuf>,
    held: &dyn Fn(&Path) -> bool,
) -> Option<PathBuf> {
    ancestors().into_iter().find_map(|pid| {
        open(pid).into_iter().find(|file| {
            file.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| CARGO_LOCKS.contains(&n))
                && held(file)
        })
    })
}

/// The verb's usage, said: 2.
fn usage() -> std::process::ExitCode {
    eprintln!("atpkg {VERB}: usage: {VERB} [--shim <tool> <placed>] <target> [args…]");
    std::process::ExitCode::from(2)
}

/// THE SHIM HOP'S FRONT: `target` run on `args` as `tool` reads it, in `cwd`, at `seat`, IN THIS
/// PROCESS'S PID. A call that claims nothing becomes `target` at once ([`exec`]). A claimed one
/// hands its claim to a holder ([`hand_over`]) and becomes `target` with nothing else changed, so
/// every reader of this pid, its parent, its job and its signals finds the build itself; where
/// no holder arms, the build runs under the claim here as [`supervise`] runs it, and only where
/// that cannot start does it run unclaimed, said. An interrupt that ended the wait ends this
/// process as it would have ended the build ([`stopped_by`]).
#[cfg(unix)]
pub fn front_exec(
    target: &Path,
    args: &[String],
    tool: Tool,
    cwd: &Path,
    seat: &Seat<'_>,
) -> std::process::ExitCode {
    front_exec_bounded(target, args, tool, cwd, seat, &|| None)
}

#[cfg(unix)]
fn front_exec_bounded(
    target: &Path,
    args: &[String],
    tool: Tool,
    cwd: &Path,
    seat: &Seat<'_>,
    probe_lock: &dyn Fn() -> Option<PathBuf>,
) -> std::process::ExitCode {
    let Some(build) = classify(args, tool) else {
        return exec(target, args);
    };
    let mut say = |line: &str| eprintln!("{line}");
    let held = match admit_bounded(&build, cwd, seat, probe_lock, &mut say) {
        Admission::Claimed(held) => *held,
        Admission::Stopped(Error::Stopped {
            signal: Some(sig), ..
        }) => return stopped_by(sig),
        Admission::Stopped(_) => return std::process::ExitCode::from(1),
        Admission::Refused(_) => return std::process::ExitCode::from(75),
        Admission::Covered(_) | Admission::Own(_) => {
            return exec(target, args);
        }
    };
    match hand_over(&held) {
        Ok(()) => {
            let marker = std::env::var_os(aterm_capacity::COVER_ENV)
                .map(|_| held.claim.record().cover_marker());
            held.claim.hand_off();
            exec_marked(target, args, marker.as_deref())
        }
        Err(why) => {
            say(&format!(
                "{SAYS}: its claim's holder did not arm ({why}); the build runs under this front"
            ));
            let mut command = aterm_uds::spawnfd::transparent(target);
            command.args(args);
            match supervise(Some(held), &mut command, &children_maxrss, &mut say) {
                Ok(status) => end_like(status),
                Err(e) => {
                    say(&format!(
                        "{SAYS}: running the build under its claim failed ({e}); no unclaimed retry"
                    ));
                    std::process::ExitCode::from(127)
                }
            }
        }
    }
}

/// The longest a front waits for its holder to arm ([`hand_over`]): a production hang bound, past
/// which the build runs under the front instead.
#[cfg(target_os = "macos")]
pub const HOLDER_ARMS_WITHIN: std::time::Duration = std::time::Duration::from_secs(10);

/// HAND `held`'s CLAIM TO A HOLDER ([`hold`]): this executable, as recorded at its `main`'s first
/// call and unchanged at its path ([`aterm_uds::tether::executable`]), spawned detached with a copy
/// of the claim's descriptor and the arming pipe, and armed: its pid and then its arming byte
/// read within [`HOLDER_ARMS_WITHIN`]. Its child, which forks the holder away, is reaped. A
/// holder that did not arm is let go: the pipe closed under it (so a late arming fails and it
/// leaves, handing the claim off), and once its pid was read it is ended by proof of its
/// identity. The claim is this process's either way until it hands it off. Why it did not arm,
/// where it did not.
#[cfg(target_os = "macos")]
fn hand_over(held: &Held) -> Result<(), String> {
    let exe = aterm_uds::tether::executable().ok_or("no unchanged image of this executable")?;
    let me = std::process::id();
    let serial = i32::try_from(me)
        .ok()
        .and_then(aterm_uds::process::process_serial)
        .ok_or("this process's serial is unread")?;
    let words = hold::Words {
        pid: me,
        serial,
        claim: held.claim.path().to_path_buf(),
        key: held.key.clone(),
        own: held.own.clone(),
    };
    let descriptor = held
        .claim
        .descriptor()
        .map_err(|e| format!("its claim's descriptor: {e}"))?;
    let (armed, arming) = std::io::pipe().map_err(|e| format!("a pipe: {e}"))?;
    let mut command = aterm_uds::spawnfd::LaneCommand::new(exe);
    command
        .arg(hold::HOLD_VERB)
        .args(words.render())
        .stdin(aterm_uds::spawnfd::ChildStdio::Null)
        .stdout(aterm_uds::spawnfd::ChildStdio::Null)
        .stderr(aterm_uds::spawnfd::ChildStdio::Null);
    let mut child = aterm_uds::spawnfd::detached_with_two_fds(command, descriptor, arming.into())
        .map_err(|e| format!("its spawn: {e}"))?;
    let (holder, said) = read_arming(&armed, HOLDER_ARMS_WITHIN);
    if said.is_ok() {
        drop(armed);
        let _ = child.wait();
        return Ok(());
    }
    drop(armed);
    let _ = child.kill();
    let _ = child.wait();
    if let Some(token) = holder {
        let _ = aterm_uds::process::end_proven(
            token,
            aterm_uds::process::END_GRACE,
            &mut aterm_uds::process::EndControlLive,
            |_| true,
        );
    }
    said
}

/// Off macOS no holder runs ([`hold`]): the build runs under its front.
#[cfg(all(unix, not(target_os = "macos")))]
fn hand_over(_held: &Held) -> Result<(), String> {
    Err("no holder runs on this platform".to_owned())
}

/// Read a holder's arming from `armed` within `within`: its identity, once its pid was read and
/// it named a live process, and whether it armed (else why not).
#[cfg(target_os = "macos")]
fn read_arming(
    armed: &std::io::PipeReader,
    within: std::time::Duration,
) -> (Option<aterm_uds::process::Identity>, Result<(), String>) {
    use std::os::fd::AsRawFd as _;
    let until = std::time::Instant::now() + within;
    let mut got = [0_u8; 5];
    let mut have = 0;
    let mut holder = None;
    while have < got.len() {
        let left = until.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return (
                holder,
                Err(format!("not armed within {}s", within.as_secs())),
            );
        }
        let mut poll = libc::pollfd {
            fd: armed.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let ms = libc::c_int::try_from(left.as_millis().max(1)).unwrap_or(libc::c_int::MAX);
        // SAFETY: one `pollfd`, live for the call.
        let ready = unsafe { libc::poll(&raw mut poll, 1, ms) };
        if ready < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return (
                holder,
                Err(format!("its pipe: {}", std::io::Error::last_os_error())),
            );
        }
        if ready == 0 {
            continue;
        }
        // SAFETY: reads into the unread tail of `got`, live for the call.
        let n = unsafe {
            libc::read(
                armed.as_raw_fd(),
                got[have..].as_mut_ptr().cast(),
                got.len() - have,
            )
        };
        match n {
            0 => return (holder, Err("it ended unarmed".to_owned())),
            n if n < 0 => {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return (
                    holder,
                    Err(format!("its pipe: {}", std::io::Error::last_os_error())),
                );
            }
            n => have += n.unsigned_abs(),
        }
        if holder.is_none() && have >= 4 {
            let pid = u32::from_le_bytes([got[0], got[1], got[2], got[3]]);
            holder = i32::try_from(pid)
                .ok()
                .and_then(aterm_uds::process::read_identity);
        }
    }
    if got[4] == 1 {
        (holder, Ok(()))
    } else {
        (holder, Err("its arming byte was not 1".to_owned()))
    }
}

/// End as `sig` would have ended the build it kept from starting.
#[cfg(unix)]
fn stopped_by(sig: i32) -> std::process::ExitCode {
    use std::os::unix::process::ExitStatusExt as _;
    end_like(std::process::ExitStatus::from_raw(sig))
}

/// The signals whose default action ends a process with no core dumped: a build that one ended,
/// its front ends by the same ([`raise_like`]).
#[cfg(unix)]
const ENDS_WITHOUT_CORE: [libc::c_int; 10] = [
    libc::SIGHUP,
    libc::SIGINT,
    libc::SIGKILL,
    libc::SIGPIPE,
    libc::SIGALRM,
    libc::SIGTERM,
    libc::SIGUSR1,
    libc::SIGUSR2,
    libc::SIGPROF,
    libc::SIGVTALRM,
];

/// END AS THE BUILD ENDED, where a signal with no core ended it ([`ENDS_WITHOUT_CORE`]): that
/// signal ends this process too, set to its default action and unblocked, a SIGKILL by
/// `aterm_uds::process::kill_self_now`, so a caller that reads how its child died (a shell's
/// `Killed: 9`, Python's `returncode == -9`, a supervisor's OOM test) reads the build's death
/// through the front as it did when the call was the build. A core signal (SIGSEGV, SIGABRT,
/// SIGBUS, SIGQUIT...) is not raised: macOS would record a crash of this executable, the aterm
/// image, for the build's (clippy.toml: no core-dump death), so the front exits `128 + n` for it,
/// as a shell reports one. Returns for an exit, and for those.
#[cfg(unix)]
pub fn raise_like(status: &std::process::ExitStatus) {
    use std::os::unix::process::ExitStatusExt as _;
    let Some(sig) = status.signal().filter(|s| ENDS_WITHOUT_CORE.contains(s)) else {
        return;
    };
    if sig == libc::SIGKILL {
        aterm_uds::process::kill_self_now();
    }
    let mut set = std::mem::MaybeUninit::<libc::sigset_t>::zeroed();
    // SAFETY: `signal` with the default action for a signal that dumps no core; `set` is one
    // writable `sigset_t`, emptied, given `sig` and handed to `pthread_sigmask` live across each
    // call, which keeps no pointer to it; `raise` delivers `sig` to this thread before it returns.
    unsafe {
        libc::signal(sig, libc::SIG_DFL);
        libc::sigemptyset(set.as_mut_ptr());
        libc::sigaddset(set.as_mut_ptr(), sig);
        libc::pthread_sigmask(libc::SIG_UNBLOCK, set.as_ptr(), std::ptr::null_mut());
        #[expect(
            clippy::disallowed_methods,
            reason = "re-raises a signal that dumps no core (ENDS_WITHOUT_CORE): the build's death, as the front's"
        )]
        libc::raise(sig);
    }
}

/// The front's end as its build's `status` ([`raise_like`], else its exit code, `128 + n` for a
/// core signal).
#[cfg(unix)]
#[must_use]
pub fn end_like(status: std::process::ExitStatus) -> std::process::ExitCode {
    raise_like(&status);
    crate::platform::exit_code_like(status)
}

#[cfg(unix)]
#[path = "build_admit_front.rs"]
pub mod front;

#[cfg(unix)]
#[path = "build_admit_exec.rs"]
mod becoming;

#[path = "build_admit_hold.rs"]
pub mod hold;

#[cfg(test)]
#[path = "build_admit_tests.rs"]
mod tests;
