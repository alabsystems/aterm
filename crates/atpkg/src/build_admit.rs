// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A PLAIN BUILD'S MEMORY CLAIM (`docs/DESIGN-adaptive-build-2026-10-24.md`, BUILD-ADMIT): a
//! `targo` or `cargo` call that compiles claims the memory it will take in this user's capacity
//! ledger before the build starts, and holds it until the build ends.
//!
//! What claims ([`classify`]): `build`, `check`, `test`, `bench`, `doc`, `rustc`, `rustdoc`,
//! `fix`, `package` with its default compile verification, and the lint (`tippy`, `clippy`,
//! `targo-tippy`), and `targo trust` over one of them; a
//! `targo` verb other than the lint only in a lane it runs (`--unverified`, `trust`), since a bare
//! one is refused. Never `run` (which holds its program's whole life), an alias (`verify`, `lab`,
//! `ship`: each claims for the work it starts), `metadata`, `clean`, or a call for help or a
//! version: those start unclaimed, as before.
//!
//! What it claims ([`need`]): the most the same work took, read by [`aterm_capacity::book::chain`]
//! from what earlier builds of it kept here and elsewhere ([`aterm_capacity::book::TARGO_RECORD`])
//! and verify's run of the stage whose work holds its own, its analogue
//! ([`aterm_capacity::book::VERIFY_RECORD`]: a test build's `TestCompile`, a lint's `Tippy`), and
//! a fifth more; else a wider selection's in its checkout, then a wider named selection's in
//! another checkout, its own book's or verify's, unless a narrower record or a unit's peak shows
//! that run warm, at most the cold figure; the cold figure where none is kept. A need at or
//! beyond what can ever be available here asks to run alone, as verify's do, and one the ledger
//! finds can never fit beside others asks again alone.
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
//! claim waits for it ([`end_tree`]). Its tree's memory is metered ([`aterm_capacity::tree_meter`]),
//! never below its root's own resident peak (the accounting of the reap that consumed its exit),
//! and its peak kept when it succeeded, or when it failed above what it claimed ([`Held::end`]).

use std::path::{Path, PathBuf};
use std::time::Instant;

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

#[path = "build_admit_controller.rs"]
mod controller;
pub use controller::{ControllerOutcome, supervise_controller};

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
        "package" => "package",
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
    let mut no_verify = false;
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
            "--no-verify" => no_verify = true,
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
    if verb == "package" && no_verify {
        return None;
    }
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
        ("targo", false, "test") => Some((book::TEST_COMPILE, in_test_compile)),
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

/// What a build kept under `key` claims on a machine of `physical` bytes, as [`book::chain`] reads
/// it ([`book::need`]): the most of what `own` (the plain builds' records) keeps for it, here or
/// in another checkout, and, for a build that has one ([`Build::analogue`]), what `verify` keeps
/// for its analogue over the same selection, as verify reads it or in another checkout (its key
/// has no variant: verify's stage builds what its analogue's targets name and more); else a wider
/// selection's in `own` in its checkout; else the more of a wider named selection's in another
/// checkout, in `own` and of the analogue in `verify`, each refused where a narrower record or a
/// unit's peak shows that run warm, at most the cold figure; and the cold figure where none is
/// kept.
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
    /// This admission requires native ARM/birth/BIND rather than holder transfer.
    #[cfg(unix)]
    cohort: bool,
}

impl Held {
    #[cfg(target_os = "macos")]
    fn into_epoch_parts(self) -> (Claim, Ended) {
        let Held {
            claim, key, own, ..
        } = self;
        let claimed = claim.record().need.mem;
        let run_claimed = claim.record().need.run_mem.unwrap_or(claimed);
        let run = telemetry::Run::of(&claim);
        (
            claim,
            Ended {
                claimed,
                run_claimed,
                key,
                own,
                run,
            },
        )
    }

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

    /// The build ended `end`: let the claim go, then keep its peak and write its row
    /// ([`Ended::keep`]). The bytes kept; `None` when nothing was, or the record could not be
    /// written (said in one line).
    pub fn end(self, end: BuildEnd, peak: u64, say: &mut dyn FnMut(&str)) -> Option<u64> {
        self.let_go().keep(end, peak, say)
    }

    /// Drop local controller custody before reading or recording its final memory sample.
    /// An ordinary claim ends now; a live or uncertain cohort retains its full durable
    /// reservation until actual group drain. What keeps the peak is left ([`Ended::keep`]).
    #[must_use]
    pub fn let_go(self) -> Ended {
        let Held {
            claim, key, own, ..
        } = self;
        let claimed = claim.record().need.mem;
        let run_claimed = claim.record().need.run_mem.unwrap_or(claimed);
        let run = telemetry::Run::of(&claim);
        drop(claim);
        Ended {
            claimed,
            run_claimed,
            key,
            own,
            run,
        }
    }
}

/// A build whose local controller ended ([`Held::let_go`]): its requested memory and
/// sample key/path, and what its claim's record said of it for its telemetry row. This does not
/// certify closure of a retained durable cohort.
#[derive(Debug)]
pub struct Ended {
    claimed: u64,
    /// What its run phase claimed: its need's run phase memory, else its peak.
    run_claimed: u64,
    key: Key,
    own: Option<PathBuf>,
    run: telemetry::Run,
}

impl Ended {
    /// Keep `peak` under its key when something was read and it `succeeded`, or when it failed
    /// with a peak that would claim more than its claim did (a failed build stopped short of the
    /// work its key names, so its peak is the least that work takes: kept, it raises the key's
    /// next claim, and one below would lower it), unless the run was light beside the heavy peak
    /// its key keeps ([`Records::keep_unless_light`]: a fresh build that compiled nothing). A key
    /// whose builds keep failing (an edit-test loop's red runs, a build stopped for swap) so
    /// learns what its compile takes. It `succeeded` as [`BuildEnd::succeeded`] judges `end`.
    /// Then, where its claim was held in this user's own ledger, it writes the build's
    /// `kind=build` or `kind=test` row ([`telemetry`]). The bytes kept; `None` when nothing was,
    /// or the record could not be written (said in one line).
    pub fn keep(self, end: BuildEnd, peak: u64, say: &mut dyn FnMut(&str)) -> Option<u64> {
        self.keep_then(end, peak, say, telemetry::ended)
    }

    /// Keep its RUN PHASE's peak, `run_peak` (the most its tree held at one read once it ran a
    /// program its build made, [`aterm_capacity::phase::Phases::run_peak`]; `None` where it never
    /// did), under its key's run phase key ([`aterm_capacity::phase::run_key`]), by the rule
    /// [`Self::keep`] keeps its whole peak by, against what its run phase claimed: apart from its
    /// compile's, so the next claim of its key steps down to it (CAP3). The bytes kept; `None`
    /// when nothing was.
    pub fn keep_run(
        &self,
        end: &BuildEnd,
        run_peak: Option<u64>,
        say: &mut dyn FnMut(&str),
    ) -> Option<u64> {
        let peak = run_peak?;
        keep_peak(
            self.own.as_deref(),
            aterm_capacity::phase::run_key(&self.key),
            peak,
            self.run_claimed,
            end.succeeded(),
            say,
        )
    }

    /// [`Self::keep`], its row handed to `row` once the book is done.
    fn keep_then(
        self,
        end: BuildEnd,
        peak: u64,
        say: &mut dyn FnMut(&str),
        row: impl FnOnce(&telemetry::Run, &str, &BuildEnd, u64),
    ) -> Option<u64> {
        let Ended {
            claimed,
            key,
            own,
            run,
            ..
        } = self;
        // The row goes after the book, so its lock wait and first host read never hold up the
        // peak a next claim of this key reads.
        let stage = key.stage.clone();
        let kept = keep_peak(own.as_deref(), key, peak, claimed, end.succeeded(), say);
        row(&run, &stage, &end, peak);
        kept
    }
}

/// Keep `peak` under `key` in the book at `own` where something was read and the build
/// `succeeded`, or where it failed with a peak that would claim more than `claimed` did, unless
/// the run was light beside the heavy peak its key keeps ([`Records::keep_unless_light`]): the
/// rule [`Ended::keep`] says. The bytes kept; `None` when nothing was, or the record could not be
/// written (said in one line).
fn keep_peak(
    own: Option<&Path>,
    key: Key,
    peak: u64,
    claimed: u64,
    succeeded: bool,
    say: &mut dyn FnMut(&str),
) -> Option<u64> {
    let raises = aterm_capacity::peak_claim(peak) > claimed;
    let path = own.filter(|_| peak > 0 && (succeeded || raises))?;
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    match Records::keep_unless_light(path, key, peak, at) {
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
    admit_for(build, cwd, seat, probe_lock, say, AdmissionKind::Holder)
}

/// Admit one explicitly managed cohort build for [`supervise`]. This uses a prepared
/// private cohort ledger; it neither redirects the account ledger nor transfers the
/// ordinary shim's terminal/holder responsibilities. Nested coverage and own-claim
/// fallbacks refuse until their managed handoff is qualified. Callers own terminal
/// foreground policy for the dedicated process group.
#[cfg(unix)]
pub fn admit_cohort(
    build: &Build,
    cwd: &Path,
    seat: &Seat<'_>,
    say: &mut dyn FnMut(&str),
) -> Admission {
    admit_for(build, cwd, seat, &|| None, say, AdmissionKind::Cohort)
}

/// Reserve one independent job in an explicit cohort host. This always requests
/// its full budget, even when the host already owns claims. It never borrows an
/// ancestor's coverage or runs unclaimed. One capacity poll either grants the
/// reservation or refuses; the host never waits on its own jobs. Seat overrides
/// refuse: Now means forced admission in ordinary fronts, not a nonblocking
/// capacity check. Queue/retry policy belongs to the host; this does not enable
/// the ordinary managed route.
#[cfg(unix)]
pub fn admit_cohort_job(
    build: &Build,
    cwd: &Path,
    seat: &Seat<'_>,
    say: &mut dyn FnMut(&str),
) -> Admission {
    if seat.mode != Admit::Wait || seat.pid != std::process::id() {
        return refuse(
            say,
            "host job admission requires resource guards and its actual owner process",
        );
    }
    admit_for(build, cwd, seat, &|| None, say, AdmissionKind::HostJob)
}

#[derive(Clone, Copy)]
enum AdmissionKind {
    Holder,
    #[cfg(unix)]
    Cohort,
    #[cfg(unix)]
    HostJob,
}

fn admit_for(
    build: &Build,
    cwd: &Path,
    seat: &Seat<'_>,
    probe_lock: &dyn Fn() -> Option<PathBuf>,
    say: &mut dyn FnMut(&str),
    kind: AdmissionKind,
) -> Admission {
    let cohort = !matches!(kind, AdmissionKind::Holder);
    #[cfg(unix)]
    let independent = matches!(kind, AdmissionKind::HostJob);
    #[cfg(not(unix))]
    let independent = false;
    let Some(ledger) = &seat.ledger else {
        return refuse(say, "no home directory keeps the capacity ledger");
    };
    // The ordinary front transfers holder custody; explicit native cohort admission
    // requires the opposite ledger kind. Neither route silently changes lifetime law.
    match ledger.cohort_aware() {
        Ok(aware) if aware == cohort => {}
        Ok(false) => return refuse(say, "native cohort admission requires a cohort ledger"),
        Ok(true) => {
            return refuse(
                say,
                "the holder-based build front cannot supervise a cohort ledger; native cohort launch is required",
            );
        }
        Err(error) => {
            return refuse(
                say,
                &format!("the capacity ledger could not be validated ({error})"),
            );
        }
    }
    if !independent {
        match aterm_capacity::covering(ledger, seat.machine, seat.pid, seat.marker.as_deref()) {
            Ok(Some(_)) if cohort => {
                return refuse(
                    say,
                    "native cohort admission cannot use a coverage fallback",
                );
            }
            Ok(Some(holder)) => return Admission::Covered(holder),
            Ok(None) => {}
            Err(e) => return refuse(say, &format!("the capacity ledger could not be read ({e})")),
        }
        match ledger.of_pid(seat.pid) {
            Ok(mine) => {
                if let Some(r) = mine.into_iter().find(|r| r.held.is_some()) {
                    if cohort {
                        return refuse(
                            say,
                            "native cohort admission cannot use an own-claim fallback",
                        );
                    }
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
    // What its run phase claims, kept apart from its compile's (CAP3): its holder steps the claim
    // down to it once its tree runs what it built, and a run heavier than its compile is its peak.
    let run = aterm_capacity::phase::run_need(&own, &key, reading.physical);
    let phased = aterm_capacity::phase::phased(need.bytes, run);
    let budget = reading.reachable().unwrap_or(u64::MAX);
    let what = format!("{} {}", key.stage, key.selection());
    let ask = |alone: bool| {
        let mut n = Need::new(what.clone())
            .mem(phased.mem)
            .checkout(key.checkout.clone())
            .covers();
        n.run_mem = phased.run_mem;
        if alone { n.alone() } else { n }
    };
    let beyond = phased.mem >= budget;
    let mut told = false;
    let mut speak = |line: &str| {
        if !std::mem::replace(&mut told, true) {
            say(&format!(
                "{SAYS}: {what} claims {need}{}",
                run_words(run, &phased)
            ));
        }
        say(line);
    };
    let attempt = |need: Need, speak: &mut dyn FnMut(&str)| {
        if !independent && (seat.mode != Admit::Wait || possible_lock.is_none()) {
            // `admit_with` writes this admission's row where it decides it.
            return aterm_capacity::admit_with(
                ledger,
                seat.machine,
                need,
                seat.mode,
                seat.pace,
                speak,
            );
        }
        // Decided here instead: its row is owed from its ask to its decision, written once
        // however many looks met the ledger's lock.
        let asked = aterm_capacity::telemetry::Asked::of(&need);
        let decided = if independent {
            one_look(ledger, seat.machine, &reading, need)
        } else {
            look_in_turn(ledger, seat, &reading, need, speak)
        };
        telemetry::admitted(ledger, asked, &decided);
        decided
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
            #[cfg(unix)]
            cohort,
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

/// What a build's claim line adds for its run phase ([`aterm_capacity::phase`], CAP3): `run`, what
/// its run phase claims where its key or a wider selection keeps one, and `phased`, the need it
/// then asks ([`aterm_capacity::phase::phased`]).
fn run_words(run: Option<aterm_capacity::phase::RunNeed>, phased: &Need) -> String {
    let Some(run) = run else {
        return String::new();
    };
    let kept = if run.same_work {
        "the most its run phase took and a fifth more"
    } else {
        "the most a wider selection's run phase took and a fifth more"
    };
    let bytes = aterm_capacity::size(run.bytes);
    match phased.run_mem {
        Some(_) => format!("; {bytes} once it runs what it built, {kept}"),
        None if run.same_work => {
            format!("; its run phase claims more, {bytes} ({kept}), which it claims from its start")
        }
        None => format!("; no step down: {kept}, {bytes}, is no less than it claims"),
    }
}

/// A HOST JOB'S ADMISSION ([`admit_cohort_job`]): one look decides it, admitted or refused with
/// what it was short of; it never waits, not even for the ledger's lock. One look is one
/// admission, so one row ([`telemetry::admitted`]).
fn one_look(
    ledger: &Ledger,
    machine: &dyn Machine,
    reading: &aterm_capacity::Reading,
    need: Need,
) -> Result<Claim, Error> {
    if let Some(why) = aterm_capacity::never(&need, reading) {
        return Err(Error::Never(why));
    }
    match aterm_capacity::Waiter::enter(ledger, machine, need)
        .map_err(Error::Ledger)?
        .poll(machine)
    {
        aterm_capacity::Polled::Admitted(claim) => Ok(claim),
        aterm_capacity::Polled::Waiting(waiter, short) => {
            drop(waiter);
            Err(Error::Stopped {
                signal: None,
                short: Some(short),
            })
        }
        aterm_capacity::Polled::Gone(error) => Err(Error::Ledger(error)),
        aterm_capacity::Polled::Unqualified(why) => Err(Error::Timing(why)),
    }
}

/// A NESTED BUILD'S ADMISSION UNDER A POSSIBLE BUILD LOCK ([`possible_build_lock`]): one look
/// decides, but only in its turn. The ledger's lock is held only while another admission
/// decides, and a look waits on no build, so a wait for it can close no cycle: a look that met it
/// is tried again every turn gap, saying who holds it by the wait loop's rule, until a look
/// decides or an interrupt ends the wait. However many looks met the lock, it is one admission,
/// so one row ([`telemetry::admitted`]).
fn look_in_turn(
    ledger: &Ledger,
    seat: &Seat<'_>,
    reading: &aterm_capacity::Reading,
    need: Need,
    speak: &mut dyn FnMut(&str),
) -> Result<Claim, Error> {
    if let Some(why) = aterm_capacity::never(&need, reading) {
        return Err(Error::Never(why));
    }
    let watch = aterm_toolchain::interrupt::Watch::enter();
    if let Some(signal) = watch.take() {
        return Err(Error::Stopped {
            signal: Some(signal),
            short: None,
        });
    }
    let mut waiter =
        aterm_capacity::Waiter::enter(ledger, seat.machine, need).map_err(Error::Ledger)?;
    let mut turn: Option<Instant> = None;
    let mut said: Option<Instant> = None;
    let polled = loop {
        let polled = waiter.poll(seat.machine);
        if let Some(signal) = watch.take() {
            drop(polled);
            return Err(Error::Stopped {
                signal: Some(signal),
                short: None,
            });
        }
        match polled {
            aterm_capacity::Polled::Waiting(w, short)
                if matches!(short.shorts[..], [aterm_capacity::Short::Turn { .. }]) =>
            {
                let since = *turn.get_or_insert_with(Instant::now);
                if let Some(line) = w.turn_line(&short, seat.pace, since, said) {
                    speak(&line);
                    said = Some(Instant::now());
                }
                if watch.nap(seat.pace.turn_gap(), &|| false)
                    && let Some(signal) = watch.take()
                {
                    drop(w);
                    return Err(Error::Stopped {
                        signal: Some(signal),
                        short: Some(short),
                    });
                }
                waiter = w;
            }
            decided => break decided,
        }
    };
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
}

/// RUN `command`, a pass-through build, under `held` ([`Admission::Claimed`]; `None`: unclaimed):
/// spawned with every signal that reaches this process for it passed on to it ([`front`]: every
/// catchable one that ends a process with no core, and the terminal keys' where the terminal's
/// key would not reach it itself), handed the claim's cover marker ([`aterm_capacity::COVER_ENV`])
/// so the builds it starts claim nothing more, its tree metered while a claim is held, and its
/// peak kept as [`Held::end`] keeps one: never below its root's own resident peak, which the
/// reap that consumes the root's exit reads from its accounting (`wait4`: the root's and those of
/// the descendants it reaped), as a host job's does, so a child this process reaped before, which
/// is no part of this build, is no floor of it. Its status.
/// Build pass-through commands with [`aterm_uds::spawnfd::TransparentCommand::into_command`]
/// to preserve inherited handles such as compiler jobserver pipes. The native owned-child
/// boundary uses ordinary spawn for ordinary admission. [`admit_cohort`] instead requires
/// native ARM/birth/BIND. A live or unread cohort remains fully reserved after root exit;
/// that incomplete run cannot supply a successful whole-build memory sample. This does
/// not transfer the ordinary shim's terminal policy or enable account-wide cohort admission.
///
/// # Errors
/// Admission's native launch, durable binding or wait failed. A child whose BIND failed
/// remains supervised; the error is returned afterwards with uncertain custody retained.
#[cfg(unix)]
pub fn supervise(
    held: Option<Held>,
    command: &mut aterm_uds::spawnfd::Command,
    say: &mut dyn FnMut(&str),
) -> std::io::Result<std::process::ExitStatus> {
    let cohort = held.as_ref().is_some_and(|held| held.cohort);
    supervise_with(
        held,
        command,
        say,
        || {
            if cohort {
                front::Front::arm_cohort()
            } else {
                front::Front::arm()
            }
        },
        |_, _| {},
    )
}

/// Run explicit cohort admission on the caller's foreground controlling terminal.
/// Native stopped-at-exec launch keeps target code behind durable BIND and terminal
/// handoff; the child's owning wait restores foreground before reaping. Command start
/// policy is restored on return and unwinding. This does not migrate the ordinary shim.
/// # Errors
/// Admission is not cohort-aware, native launch/handoff/resume fails, or wait fails.
#[cfg(target_os = "macos")]
pub fn supervise_terminal(
    held: Held,
    command: &mut aterm_uds::spawnfd::Command,
    terminal: std::fs::File,
    say: &mut dyn FnMut(&str),
) -> std::io::Result<std::process::ExitStatus> {
    supervise_job_using(held, command, Some(terminal), say, None, None).map(JobOutcome::root_status)
}

/// Notifications from the actual supervised root, not proof that its whole cohort
/// is drained or that the CPU is actively executing.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SupervisedEvent {
    /// Actual exit observed while cancellation still retains the original leader pin.
    RootExitObserved,
    /// The kernel reported a stop; the root remains owned and budgeted.
    Stopped(i32),
    /// The kernel accepted this controller's explicit resume request.
    ResumeAccepted { foreground: bool },
    /// Kernel acceptance of one explicit bound-group request, never proof of drain.
    GroupRequestAccepted(aterm_capacity::cohort::launch::CohortSignal),
    /// The actual root was reaped with accounting; cohort settlement is separate.
    RootExited(aterm_uds::spawnfd::Rusage),
    /// Kernel group absence settled this claim; root exit alone did not do so.
    CohortDrained,
    /// Group settlement remains live or unknown; its full budget stays reserved.
    CohortRetained,
}

/// Durable cohort settlement, separate from root exit or signal acceptance.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CohortSettlement {
    /// Kernel absence settled the original group reservation.
    Drained,
    /// Live or unknown work retains the complete reservation.
    Retained,
}

/// Operational job result. Root status alone cannot establish successful work
/// after cancellation or retained custody. This is not a formal proof grade.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub struct JobOutcome {
    root: std::process::ExitStatus,
    cancelled: bool,
    settlement: Option<CohortSettlement>,
}

#[cfg(unix)]
impl JobOutcome {
    /// The actual root status, not a whole-job success verdict.
    #[must_use]
    pub const fn root_status(self) -> std::process::ExitStatus {
        self.root
    }
    /// Explicit cancellation or a termination-class signal forwarded by this supervisor.
    #[must_use]
    pub const fn cancelled(self) -> bool {
        self.cancelled
    }
    /// None for an ordinary holder supervisor without durable cohort custody.
    #[must_use]
    pub const fn cohort_settlement(self) -> Option<CohortSettlement> {
        self.settlement
    }
    /// An uncancelled zero root exit with the original cohort conclusively drained.
    /// Does not establish formal correctness or certificate validation.
    #[must_use]
    pub fn succeeded(self) -> bool {
        !self.cancelled && self.root.success() && self.settlement == Some(CohortSettlement::Drained)
    }
}

/// An explicit owner decision after a stop. No action suspends the resource owner.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobControl {
    /// Keep stopped until an external operator changes it; retain the reservation.
    Wait,
    /// Reacquire the caller's foreground terminal, then continue the admitted group.
    ResumeForeground,
    /// Resume without claiming terminal foreground (terminal I/O may stop it again).
    ResumeBackground,
    /// Request TERM then CONT for the bound group, so stopped members can handle
    /// termination. Each accepted syscall is reported separately; no release is implied.
    TerminateGroup,
    /// Request KILL for the bound group. Acceptance still does not establish drain.
    KillGroup,
}

#[cfg(unix)]
/// A borrowed owner policy; callbacks run on the parent side, never in a signal
/// handler or child pre-exec callback.
pub type JobEvents<'a> = &'a mut dyn FnMut(SupervisedEvent) -> std::io::Result<JobControl>;

#[cfg(unix)]
#[path = "build_admit_live.rs"]
mod live;
#[cfg(unix)]
pub use live::{CancelHandle, CancelRequest, Cancellation};

#[cfg(target_os = "macos")]
#[path = "build_admit_host.rs"]
pub mod host;

#[cfg(target_os = "macos")]
#[path = "build_admit_service.rs"]
pub mod service;

#[cfg(unix)]
struct JobRun<'a> {
    terminal: Option<std::fs::File>,
    events: Option<JobEvents<'a>>,
    live: Option<Cancellation>,
    cancelled: bool,
}

/// Supervise explicit terminal cohort work with owner-directed stop/resume events.
/// Stops yield the terminal and keep the same claim. Only an explicit callback
/// decision resumes work; shell/pipeline policy belongs to the client, never an
/// automatic suspension of a persistent prefix owner. Non-stop events take Wait.
/// # Errors
/// As [`supervise_terminal`], or an event, owner decision or group request fails.
#[cfg(target_os = "macos")]
pub fn supervise_terminal_events(
    held: Held,
    command: &mut aterm_uds::spawnfd::Command,
    terminal: std::fs::File,
    say: &mut dyn FnMut(&str),
    events: JobEvents<'_>,
) -> std::io::Result<std::process::ExitStatus> {
    supervise_job_using(held, command, Some(terminal), say, Some(events), None)
        .map(JobOutcome::root_status)
}

/// Supervise live job-local cancellation. The receiver's explicit grace retains
/// the original leader pin through TERM/CONT and KILL escalation before owning reap.
/// Normal uncancelled jobs keep their ordinary prompt exit behavior. Root exit,
/// accepted requests and actual group drain are distinct events.
/// # Errors
/// Invalid inbox owner, native launch/control/wait failure or rejected owner event.
#[cfg(target_os = "macos")]
pub fn supervise_terminal_controlled(
    held: Held,
    command: &mut aterm_uds::spawnfd::Command,
    terminal: std::fs::File,
    say: &mut dyn FnMut(&str),
    cancellation: Cancellation,
    events: JobEvents<'_>,
) -> std::io::Result<JobOutcome> {
    cancellation.check_owner()?;
    supervise_job_using(
        held,
        command,
        Some(terminal),
        say,
        Some(events),
        Some(cancellation),
    )
}

/// Run an independently owned headless cohort job. This blocks its caller but
/// installs no process signal policy, takes no global forwarding turn and never
/// opens or acquires a controlling terminal. A host can call it concurrently on
/// its own bounded execution threads; the host owns process-wide shutdown policy.
/// Each job retains its actual child, cancellation inbox and complete budget.
/// Foreground resume refuses; background resume and explicit cancellation remain
/// available. This is not yet a single event loop or ordinary shim adoption.
/// # Errors
/// Non-cohort admission, invalid inbox owner, native launch/control/wait failure,
/// foreground resume without a lease, or rejected owner event.
#[cfg(target_os = "macos")]
pub fn supervise_headless_controlled(
    held: Held,
    command: &mut aterm_uds::spawnfd::Command,
    say: &mut dyn FnMut(&str),
    cancellation: Cancellation,
    events: JobEvents<'_>,
) -> std::io::Result<JobOutcome> {
    cancellation.check_owner()?;
    supervise_job_using(held, command, None, say, Some(events), Some(cancellation))
}

#[cfg(target_os = "macos")]
fn supervise_job_using(
    held: Held,
    command: &mut aterm_uds::spawnfd::Command,
    terminal: Option<std::fs::File>,
    say: &mut dyn FnMut(&str),
    events: Option<JobEvents<'_>>,
    live: Option<Cancellation>,
) -> std::io::Result<JobOutcome> {
    struct Restore<'a> {
        command: &'a mut aterm_uds::spawnfd::Command,
        configured: bool,
    }
    impl Drop for Restore<'_> {
        fn drop(&mut self) {
            self.command.start_suspended(self.configured);
        }
    }
    let configured = command.starts_suspended();
    command.start_suspended(true);
    let restore = Restore {
        command,
        configured,
    };
    let forwarding = terminal.is_some();
    supervise_launch(
        Some(held),
        &mut *restore.command,
        say,
        || {
            if forwarding {
                front::Front::arm_cohort()
            } else {
                None
            }
        },
        |_, _| {},
        Some(JobRun {
            terminal,
            events,
            live,
            cancelled: false,
        }),
    )
}

// Parent-side runtime seams exercise unavailable forwarding with actual child ownership and
// metering. The production caller supplies the real factory and no observer.
#[cfg(unix)]
fn supervise_with(
    held: Option<Held>,
    command: &mut aterm_uds::spawnfd::Command,
    say: &mut dyn FnMut(&str),
    arm: impl FnOnce() -> Option<front::Front>,
    before_wait: impl FnOnce(&mut aterm_uds::spawnfd::Child, Option<&TreeMeter>),
) -> std::io::Result<std::process::ExitStatus> {
    supervise_launch(
        held,
        command,
        say,
        arm,
        |owned, meter| before_wait(owned.child_mut(), meter),
        None,
    )
    .map(JobOutcome::root_status)
}

#[cfg(unix)]
fn supervise_launch(
    mut held: Option<Held>,
    command: &mut aterm_uds::spawnfd::Command,
    say: &mut dyn FnMut(&str),
    arm: impl FnOnce() -> Option<front::Front>,
    before_wait: impl FnOnce(&mut SupervisedChild, Option<&TreeMeter>),
    mut terminal: Option<JobRun<'_>>,
) -> std::io::Result<JobOutcome> {
    if terminal.is_some() && held.as_ref().is_none_or(|held| !held.cohort) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "controlled cohort supervision requires explicit native cohort admission",
        ));
    }
    if let Some(held) = &held {
        command.env(
            aterm_capacity::COVER_ENV,
            held.claim().record().cover_marker(),
        );
    }
    let front = arm();
    let (mut owned, mut launch_error) = match launch_held(held.as_mut(), command) {
        Ok(child) => child,
        Err(e) => {
            if let Some(front) = &front {
                front.never();
            }
            return Err(e);
        }
    };
    let child = owned.child_mut();
    if let Some(terminal) = terminal.as_ref() {
        let handed = if launch_error.is_some() {
            Err(std::io::Error::other("unbound child remains stopped"))
        } else {
            let initial = if terminal.events.is_some() {
                acknowledge_initial_stop(child)
            } else {
                Ok(())
            };
            initial
                .and_then(|()| match &terminal.terminal {
                    Some(terminal) => child.take_foreground(terminal.try_clone()?),
                    None => Ok(()),
                })
                .and_then(|()| child.resume())
        };
        if let Err(error) = handed {
            // This exact owned unreaped child has not acknowledged a usable terminal
            // launch. End it and retain the original failure through ordinary cleanup.
            if let Err(kill) = child.kill() {
                say(&format!(
                    "{SAYS}: ending the owned child after controlled launch failure: {kill}"
                ));
            }
            launch_error.get_or_insert(error);
        }
    }
    let meter = held
        .as_ref()
        .filter(|_| !child.is_reaped())
        .map(|_| TreeMeter::start(child.id()));
    if !child.is_reaped() {
        before_wait(&mut owned, meter.as_ref());
    }
    let child = owned.child_mut();
    let (status, passed) = match front {
        Some(front) => {
            if child.is_reaped() {
                front.reaped();
            } else {
                front.aim(child.id(), meter.as_ref().map(TreeMeter::watch));
            }
            let status = wait_supervised(&mut owned, Some(&front), terminal.as_mut(), say);
            let passed = front.passed();
            front.disarm();
            (status, passed)
        }
        None => (
            wait_supervised(&mut owned, None, terminal.as_mut(), say),
            None,
        ),
    };
    if let (Some(meter), Ok(status)) = (&meter, &status) {
        end_tree(meter, passed, *status, say);
    }
    // A root's exit is no group-drain receipt. Drop retains a live/unknown cohort's
    // entire budget; do not record an incomplete sample as a successful whole build.
    let mut settled = true;
    if let Some(held) = held.as_mut().filter(|held| held.cohort)
        && let Err(error) = held.claim.finish_cohort()
    {
        settled = false;
        say(&format!(
            "{SAYS}: cohort reservation retained after root wait ({error})"
        ));
    }
    let settlement = held.as_ref().filter(|held| held.cohort).map(|_| {
        if settled {
            CohortSettlement::Drained
        } else {
            CohortSettlement::Retained
        }
    });
    let mut status = launch_error.map_or(status, Err);
    if let Some(events) = terminal
        .as_mut()
        .and_then(|terminal| terminal.events.as_mut())
    {
        let notified = events(if settled {
            SupervisedEvent::CohortDrained
        } else {
            SupervisedEvent::CohortRetained
        })
        .and_then(|decision| {
            if decision == JobControl::Wait {
                Ok(())
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "settlement events carry no live group authority",
                ))
            }
        });
        if let Err(error) = notified
            && status.is_ok()
        {
            status = Err(error);
        }
    }
    let cancelled = passed.is_some()
        || terminal.as_ref().is_some_and(|terminal| {
            terminal.cancelled || terminal.live.as_ref().is_some_and(Cancellation::requested)
        });
    if let (Some(held), Some(meter)) = (held, meter) {
        let ended = held.let_go();
        // The root's own peak, from the reap that consumed its exit: no other child's.
        let own = owned.child_mut().usage().map_or(0, |usage| usage.maxrss);
        let peak = meter.stop(own);
        ended.keep(
            BuildEnd::judged(status.as_ref().ok().copied(), !cancelled && settled),
            peak,
            say,
        );
    }
    status.map(|root| JobOutcome {
        root,
        cancelled,
        settlement,
    })
}

/// REAP the owned `child` with its resource accounting (`wait4`), which it keeps
/// ([`aterm_uds::spawnfd::Child::usage`]): the root's own resident peak, and those of the
/// descendants it reaped, a supervised build's floor ([`supervise`]). Where this platform's
/// kernel gives no accounting, a plain reap: no floor. A child reaped already (a cohort's
/// capture reaps a root that ended before it, [`aterm_capacity::cohort::Cohort::of_spawned`])
/// gives the status that reap took, and keeps what accounting it collected.
///
/// # Errors
/// The reap's.
#[cfg(unix)]
fn reap_accounted(
    child: &mut aterm_uds::spawnfd::Child,
) -> std::io::Result<std::process::ExitStatus> {
    if child.is_reaped() {
        return child.wait();
    }
    match child.wait_with_rusage() {
        Ok(usage) => Ok(usage.status),
        Err(error) if error.kind() == std::io::ErrorKind::Unsupported => child.wait(),
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn acknowledge_initial_stop(child: &mut aterm_uds::spawnfd::Child) -> std::io::Result<()> {
    match child.wait_job_event()?.acknowledge()? {
        Some(aterm_uds::spawnfd::ChildEvent::Stopped(
            aterm_uds::spawnfd::ChildStop::SuspendedStart,
        )) => Ok(()),
        event => Err(std::io::Error::other(format!(
            "native controlled job did not acknowledge its suspended start: {event:?}",
        ))),
    }
}

#[cfg(unix)]
fn wait_supervised(
    owned: &mut SupervisedChild,
    front: Option<&front::Front>,
    terminal: Option<&mut JobRun<'_>>,
    say: &mut dyn FnMut(&str),
) -> std::io::Result<std::process::ExitStatus> {
    let result = wait_supervised_events(owned, front, terminal);
    if result.is_err() && !owned.child_mut().is_reaped() && request_owned_cleanup(owned, say) {
        let child = owned.child_mut();
        let cleanup = match front {
            Some(front) => front.reap(child),
            None => reap_accounted(child),
        };
        if let Err(error) = cleanup {
            say(&format!(
                "{SAYS}: failed supervision could not reap its owned root ({error})"
            ));
        }
    }
    result
}

#[cfg(unix)]
fn request_owned_cleanup(owned: &mut SupervisedChild, say: &mut dyn FnMut(&str)) -> bool {
    // Control/observer failure must not abandon a live, still-authorized
    // native launch. No fallback may bypass a refused scope/handle check.
    let authorized = match owned {
        SupervisedChild::Bound(bound) => {
            match bound.request_group(aterm_capacity::cohort::launch::CohortSignal::Kill) {
                Ok(()) => true,
                Err(error) if error.raw_os_error() == Some(libc::ESRCH) => true,
                // Keep the original scope/seal checks and try surviving group
                // members first. A kernel permission refusal for an actually
                // terminal root does not prohibit reaping that same owned root.
                // This never counts as accepted signalling or group drain.
                Err(error)
                    if error.raw_os_error() == Some(libc::EPERM)
                        && matches!(bound.child_mut().exited(), Ok(true)) =>
                {
                    true
                }
                Err(error) => {
                    say(&format!(
                        "{SAYS}: failed supervision retains custody; cleanup group request refused ({error})"
                    ));
                    false
                }
            }
        }
        SupervisedChild::Unbound(spawned) => {
            spawned.key().is_current() && spawned.is_original_child()
        }
        SupervisedChild::Ordinary(_) => false,
    };
    if authorized {
        let child = owned.child_mut();
        // Positive-child cleanup also covers a root that left its original
        // group. It never broadcasts to a newly observed group.
        if child.kill().is_ok() || matches!(child.exited(), Ok(true)) {
            return true;
        }
        say(&format!(
            "{SAYS}: failed supervision retains work; owned root could not be ended"
        ));
    }
    false
}

#[cfg(unix)]
fn wait_supervised_events(
    owned: &mut SupervisedChild,
    front: Option<&front::Front>,
    terminal: Option<&mut JobRun<'_>>,
) -> std::io::Result<std::process::ExitStatus> {
    let child = owned.child_mut();
    let Some(terminal) = terminal.filter(|run| run.events.is_some()) else {
        return match front {
            Some(front) => front.reap(child),
            None => reap_accounted(child),
        };
    };
    if child.is_reaped() {
        return child.wait();
    }
    let events = terminal.events.as_mut().expect("event owner");
    loop {
        if let Some(live) = terminal.live.as_mut() {
            live.service(owned, events)?;
            if live.exit_observed() && live.holds_root() {
                live.wait()?;
                continue;
            }
        }
        let child = owned.child_mut();
        let observed = if let Some(live) = terminal.live.as_mut() {
            let Some(observed) = child.try_job_event()? else {
                live.wait()?;
                continue;
            };
            if observed.notice() == aterm_uds::spawnfd::ChildNotice::Exited && live.holds_root() {
                child.restore_foreground()?;
                live.observe_exit(events)?;
                continue;
            }
            observed
        } else {
            child.wait_job_event()?
        };
        let event = match front {
            Some(front) => front.acknowledge_job(observed)?,
            None => observed.acknowledge()?,
        };
        match event {
            Some(aterm_uds::spawnfd::ChildEvent::Stopped(
                aterm_uds::spawnfd::ChildStop::Signal(signal),
            )) => {
                let action = events(SupervisedEvent::Stopped(signal))?;
                // An exited but unreaped original leader still pins its group.
                // Terminate surviving members before any owning root reap.
                match action {
                    JobControl::TerminateGroup => {
                        terminal.cancelled = true;
                        if let Some(live) = terminal.live.as_mut() {
                            live.terminate(owned, events)?;
                            continue;
                        }
                        request_job_group(
                            owned,
                            aterm_capacity::cohort::launch::CohortSignal::Terminate,
                            events,
                        )?;
                        request_job_group(
                            owned,
                            aterm_capacity::cohort::launch::CohortSignal::Continue,
                            events,
                        )?;
                        continue;
                    }
                    JobControl::KillGroup => {
                        terminal.cancelled = true;
                        if let Some(live) = terminal.live.as_mut() {
                            live.kill(owned, events)?;
                            continue;
                        }
                        request_job_group(
                            owned,
                            aterm_capacity::cohort::launch::CohortSignal::Kill,
                            events,
                        )?;
                        continue;
                    }
                    _ => {}
                }
                // The operator may have ended the paused root while the owner
                // decided. Observe and reap that exit instead of using its old pid.
                if child.exited()? {
                    continue;
                }
                let foreground = match action {
                    JobControl::Wait => continue,
                    JobControl::ResumeForeground => {
                        let terminal = terminal.terminal.as_ref().ok_or_else(|| {
                            std::io::Error::new(
                                std::io::ErrorKind::PermissionDenied,
                                "headless job has no foreground terminal lease",
                            )
                        })?;
                        if let Err(error) = child.take_foreground(terminal.try_clone()?) {
                            if child.exited()? {
                                continue;
                            }
                            return Err(error);
                        }
                        true
                    }
                    JobControl::ResumeBackground => false,
                    JobControl::TerminateGroup | JobControl::KillGroup => {
                        unreachable!("handled above")
                    }
                };
                if let Err(error) =
                    owned.request_group(aterm_capacity::cohort::launch::CohortSignal::Continue)
                {
                    if owned.child_mut().exited()? {
                        continue;
                    }
                    return Err(error);
                }
                if events(SupervisedEvent::ResumeAccepted { foreground })? != JobControl::Wait {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "resume decisions require a stopped event",
                    ));
                }
            }
            Some(aterm_uds::spawnfd::ChildEvent::Stopped(
                aterm_uds::spawnfd::ChildStop::SuspendedStart,
            )) => {
                return Err(std::io::Error::other(
                    "unexpected second suspended-start notification",
                ));
            }
            Some(aterm_uds::spawnfd::ChildEvent::Exited(usage)) => {
                notify_job_exit(usage, events)?;
                return Ok(usage.status);
            }
            None => {}
        }
    }
}

#[cfg(unix)]
fn notify_job_exit(
    usage: aterm_uds::spawnfd::Rusage,
    events: &mut JobEvents<'_>,
) -> std::io::Result<()> {
    if events(SupervisedEvent::RootExited(usage))? == JobControl::Wait {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "an exited root cannot be resumed",
        ))
    }
}

#[cfg(unix)]
fn request_job_group(
    owned: &mut SupervisedChild,
    signal: aterm_capacity::cohort::launch::CohortSignal,
    events: &mut JobEvents<'_>,
) -> std::io::Result<()> {
    owned.request_group(signal)?;
    notify_job_group(signal, events)
}

#[cfg(unix)]
fn notify_job_group(
    signal: aterm_capacity::cohort::launch::CohortSignal,
    events: &mut JobEvents<'_>,
) -> std::io::Result<()> {
    if events(SupervisedEvent::GroupRequestAccepted(signal))? != JobControl::Wait {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "group-control decisions require a stopped event",
        ));
    }
    Ok(())
}

/// Keep the actual launch receipt through supervision. An unbound birth stays
/// owned for cleanup but cannot acquire the bound cohort's group-control API.
#[cfg(unix)]
enum SupervisedChild {
    Ordinary(aterm_uds::spawnfd::Child),
    Bound(Box<aterm_capacity::cohort::launch::CohortChild>),
    Unbound(Box<aterm_uds::spawnfd::Spawned>),
}

#[cfg(unix)]
impl SupervisedChild {
    fn group_drained(&self) -> bool {
        matches!(self, Self::Bound(bound) if bound.cohort().lifetime() == aterm_capacity::cohort::Lifetime::Drained)
    }
    fn child_mut(&mut self) -> &mut aterm_uds::spawnfd::Child {
        match self {
            Self::Ordinary(child) => child,
            Self::Bound(bound) => bound.child_mut(),
            Self::Unbound(spawned) => spawned.child_mut(),
        }
    }

    fn request_group(
        &mut self,
        signal: aterm_capacity::cohort::launch::CohortSignal,
    ) -> std::io::Result<()> {
        match self {
            Self::Bound(bound) => bound.request_group(signal),
            Self::Ordinary(_) | Self::Unbound(_) => Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "group control requires an actual bound cohort launch",
            )),
        }
    }
}

/// Preserve the actual child even when BIND fails: supervise it, return the binding
/// error afterwards, and leave uncertain durable custody reserved. Only a genuine
/// no-child receipt can cancel an ARM. Ordinary claims keep their existing spawn.
#[cfg(unix)]
fn launch_held(
    held: Option<&mut Held>,
    command: &mut aterm_uds::spawnfd::Command,
) -> std::io::Result<(SupervisedChild, Option<std::io::Error>)> {
    use aterm_capacity::cohort::launch::LaunchError;
    let Some(held) = held.filter(|held| held.cohort) else {
        return command
            .spawn()
            .map(|child| (SupervisedChild::Ordinary(child), None));
    };
    match held.claim.launch_cohort(command) {
        Ok(child) => Ok((SupervisedChild::Bound(Box::new(child)), None)),
        Err(LaunchError::Refused(error)) => Err(error),
        Err(LaunchError::NotSpawned(proof)) => {
            let error = std::io::Error::new(proof.error().kind(), proof.error().to_string());
            held.claim.cancel_cohort(*proof)?;
            Err(error)
        }
        Err(LaunchError::Unbound { spawned, error }) => {
            Ok((SupervisedChild::Unbound(spawned), Some(error)))
        }
    }
}

/// How long what a build ended by a signal left running is given to end by that signal
/// before [`end_tree`] kills it outright: the grace each left process gets from the signal
/// it was sent. Elapsed time alone never ends the claim: it ends on a read that finds
/// none left ([`aterm_capacity::tree_meter::Left::Read`]), however long what is left
/// takes to drain once killed.
#[cfg(unix)]
pub const TREE_ENDS_WITHIN: std::time::Duration = std::time::Duration::from_mins(1);

/// TEST SEAM: set in a debug build's environment to a count of milliseconds, [`end_tree`]'s
/// escalation grace is that long instead of [`TREE_ENDS_WITHIN`], so a test of a process deaf
/// to the ending signal does not wait a minute for its kill.
#[cfg(all(unix, any(test, debug_assertions)))]
pub const TREE_END_GRACE_ENV: &str = "ATERM_BUILD_TREE_END_GRACE_MS";

/// [`end_tree`]'s escalation grace: [`TREE_ENDS_WITHIN`], or [`TREE_END_GRACE_ENV`]'s
/// milliseconds in a debug build.
#[cfg(unix)]
fn tree_end_grace() -> std::time::Duration {
    #[cfg(any(test, debug_assertions))]
    if let Some(ms) = std::env::var(TREE_END_GRACE_ENV)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
    {
        return std::time::Duration::from_millis(ms);
    }
    TREE_ENDS_WITHIN
}

#[cfg(unix)]
fn signal_observed((pid, start): (u32, u64), signal: i32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    if pid <= 1 || aterm_uds::process::read_process_start(pid).map(|s| s.boot.0) != Some(start) {
        return false;
    }
    // This fresh birth read rejects stale observations; it does not pin a reaped
    // descendant's pid across the kernel call. Durable group custody remains separate.
    // SAFETY: kill takes integer arguments. The caller selected observed work to signal.
    unsafe { libc::kill(pid, signal) == 0 }
}

/// How often [`end_tree`] reads what is left.
#[cfg(unix)]
const TREE_END_READ: std::time::Duration = std::time::Duration::from_millis(100);

/// END WHAT A BUILD ENDED BY A SIGNAL LEFT RUNNING, before its claim ends: a `cargo` killed by a
/// SIGTERM leaves its compilers and build scripts running, reparented away, holding memory its
/// claim no longer would. Where the front passed a signal on (`passed`), or the build died of one,
/// every process its tree leaves ([`TreeMeter::left_observed`]) is sent that signal (a core
/// signal's death sends SIGTERM, so no crash is reported for them), each once by its identity
/// (its pid and its start, so a pid taken anew is sent it as its own process, never killed for
/// its predecessor), and the claim waits until a read finds none left. One still left
/// [`TREE_ENDS_WITHIN`] after it was sent the signal receives a SIGKILL request, said when
/// the kernel accepts it; the claim then waits for the read that finds it gone,
/// with no second bound: elapsed time is no evidence that descendant work stopped. A read
/// that fails ([`aterm_capacity::tree_meter::Left::Unread`]) is no such evidence either, so
/// the wait goes on to the next read. Where no read ever found the tree's group
/// ([`aterm_capacity::tree_meter::Left::Unobserved`]: off macOS, or a root gone before its
/// first read) there is no observed process to end or to wait for: no sampled drain is
/// established. The supervisor separately settles durable cohort custody; an ordinary
/// claim ends unobserved, never as a reading of none left. A build that exited leaves what it
/// started alone (a daemon it meant to outlive it).
#[cfg(unix)]
fn end_tree(
    meter: &TreeMeter,
    passed: Option<i32>,
    status: std::process::ExitStatus,
    say: &mut dyn FnMut(&str),
) {
    use aterm_capacity::tree_meter::Left;
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
    let grace = tree_end_grace();
    // Each left identity the ending signal was sent to, when, and whether SIGKILL followed.
    let mut sent: Vec<((u32, u64), std::time::Instant, bool)> = Vec::new();
    loop {
        let left = match meter.left_observed() {
            Left::Read(left) => left,
            Left::Unread => {
                std::thread::sleep(TREE_END_READ);
                continue;
            }
            Left::Unobserved => {
                say(&format!(
                    "{SAYS}: what the build left running could not be read: \
                     no sampled drain established"
                ));
                return;
            }
        };
        if left.is_empty() {
            return;
        }
        let now = std::time::Instant::now();
        let mut kill_requests = 0usize;
        for &identity in &left {
            match sent.iter_mut().find(|(seen, ..)| *seen == identity) {
                None => {
                    if signal_observed(identity, sig) {
                        sent.push((identity, now, sig == libc::SIGKILL));
                    }
                }
                Some((_, at, done)) if !*done && now.duration_since(*at) >= grace => {
                    if signal_observed(identity, libc::SIGKILL) {
                        *done = true;
                        kill_requests += 1;
                    }
                }
                Some(_) => {}
            }
        }
        if kill_requests > 0 {
            say(&format!(
                "{SAYS}: kernel accepted SIGKILL for {kill_requests} observed process(es) \
                 still running {grace:?} after signal {sig}; its claim waits for them to end"
            ));
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
            let mut command = aterm_uds::spawnfd::transparent(target).into_command();
            command.args(args).current_dir(cwd);
            for (k, v) in env {
                command.env(k, v);
            }
            Some(match supervise(Some(*held), &mut command, &mut say) {
                Ok(status) => end_like(status),
                Err(e) => {
                    eprintln!("atpkg {VERB}: {}: {e}", target.display());
                    std::process::ExitCode::from(127)
                }
            })
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
#[must_use = "return an admitted command's exit status to the caller"]
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
/// __build_admit --capabilities` answers [`HOP_CAPABILITY`] and runs nothing, and
/// `--lock-capabilities` answers [`crate::trust_lock::LOCK_CAPABILITY`]. In the shim's form, a
/// tree whose `trust.lock` names a build runs that build's tool in place of `target`
/// ([`crate::trust_lock::shim_target`]; a lock that does not read, or a build this store does not
/// hold as it names it, is exit 75 with no build started). The shim's form
/// first registers the call's build cache as the shim's own registration call did
/// ([`crate::build_cache::registered::observe_shim_call`]: a refusal is exit 75, no build
/// started); `<placed>` says what the block changed (`-` nothing, `config` its two leading words,
/// plus their directory exported for a call that
/// [`loses_config`](crate::build_cache::loses_config) (tippy, clippy), `env` only its export of
/// `CARGO_BUILD_BUILD_DIR` (the trust lane)), so the registration reads the call as typed.
/// Then a call that claims runs `target` under its claim in this pid ([`front_exec`]); other calls
/// become `target` by exec, their words handed on unchanged. A shim refuses non-UTF-8 words
/// because its cache placement cannot be established; a non-shim front still passes them on.
#[must_use = "return or inspect the admitted build command's exit status"]
pub fn run_hidden(argv: &[std::ffi::OsString]) -> std::process::ExitCode {
    if let [only] = argv
        && only == "--capabilities"
    {
        println!("{HOP_CAPABILITY}");
        return std::process::ExitCode::SUCCESS;
    }
    if let [only] = argv
        && only == crate::trust_lock::LOCK_CAPABILITY_FLAG
    {
        println!("{}", crate::trust_lock::LOCK_CAPABILITY);
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
    // A shim's target is the build it was laid for; in a tree whose trust.lock names another,
    // the hop runs that one, so plain `targo` and verify compile with one compiler.
    let locked = match (shim, std::env::current_dir()) {
        (Some(_), Ok(cwd)) => match crate::trust_lock::shim_target(
            Path::new(target),
            &cwd,
            &mut crate::trust_lock::install_named,
        ) {
            Ok(locked) => locked,
            Err(why) => {
                eprintln!("atpkg: {why}; no build started");
                return std::process::ExitCode::from(75);
            }
        },
        _ => None,
    };
    let target = locked.as_deref().unwrap_or_else(|| Path::new(target));
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
        // Registration above consumes the shim's original leading placement pair. The
        // compiler then gets every config in one parser, past its command boundary.
        let args = if tool == Tool::Targo {
            crate::build_cache::configs_after_subcommand(&args)
        } else {
            args
        };
        let seat = Seat::live();
        let possible_lock = || {
            let me = seat.pid;
            possible_build_lock(
                &|| {
                    let line = seat
                        .machine
                        .lineage(me)
                        .map(|line| line.ancestors(me))
                        .unwrap_or_default();
                    in_session(line, session_of(me), &session_of)
                },
                &|pid| aterm_uds::holders::open_files(pid).unwrap_or_default(),
                &|file| aterm_uds::ownerlock::probe_any(file) == aterm_uds::ownerlock::Owner::Live,
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

/// A POSSIBLE WAIT CYCLE, NOT ADMISSION AUTHORITY: an ancestor (the front's, [`in_session`] of
/// its lineage) has a Cargo-lock-named file open and a kernel probe says a lock is held there.
/// Neither proves it owns that lock. A verified covering/own/credited claim still answers first;
/// otherwise Wait gets one ordinary admission look and a named refusal if it cannot run, rather
/// than force a claim or deadlock. An unlocked file, an unknown probe, and Cargo's environment
/// words imply no exception.
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

/// The ancestors in `line` (its parent first) that share the front's session `own`, as
/// `session_of` reads each: the walk ends at the first one outside it, or whose session is unread
/// (it ended), since no ancestor of a session's leader is in that session. What hosts a session
/// (the terminal app that started its shell, `sshd`, `launchd`) is no build that waits on the
/// front, so it can close no wait cycle ([`possible_build_lock`]); and aterm, which hosts every
/// tab's session, holds a profile's Cargo locks while its disk watch reclaims that profile's
/// `incremental/`, which, read as an outer build's, refused every front in every tab whose
/// admission would wait. Cargo starts its build scripts and tests in its own session, so an outer
/// build stays in the walk. Empty where `own` is unread.
#[cfg(unix)]
#[must_use]
fn in_session(
    line: Vec<u32>,
    own: Option<u32>,
    session_of: &dyn Fn(u32) -> Option<u32>,
) -> Vec<u32> {
    let Some(own) = own else {
        return Vec::new();
    };
    line.into_iter()
        .take_while(|&pid| session_of(pid) == Some(own))
        .collect()
}

/// The session `pid` is in (`getsid`); `None` where the kernel names none (no such process).
#[cfg(unix)]
fn session_of(pid: u32) -> Option<u32> {
    let pid = libc::pid_t::try_from(pid).ok()?;
    // SAFETY: `getsid` reads the process table; any pid is a valid argument.
    u32::try_from(unsafe { libc::getsid(pid) }).ok()
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
            let mut command = aterm_uds::spawnfd::transparent(target).into_command();
            command.args(args);
            match supervise(Some(held), &mut command, &mut say) {
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

/// TEST SEAM ([`aterm_types::dev_seam!`]): a count of milliseconds, [`hand_over`] waits that long
/// for its holder to arm where it is longer than [`HOLDER_ARMS_WITHIN`], so a test whose holder
/// must arm waits out only its hang detector on a loaded machine.
#[cfg(target_os = "macos")]
pub const HOLDER_ARMS_WITHIN_ENV: &str = "ATERM_BUILD_HOLDER_ARMS_WITHIN_MS";

/// [`hand_over`]'s wait for its holder to arm: [`HOLDER_ARMS_WITHIN`], or the longer
/// [`HOLDER_ARMS_WITHIN_ENV`] a debug build was given.
#[cfg(target_os = "macos")]
fn holder_arms_within() -> std::time::Duration {
    aterm_types::dev_seam!(HOLDER_ARMS_WITHIN_ENV)
        .and_then(|ms| ms.to_str()?.parse::<u64>().ok())
        .map_or(HOLDER_ARMS_WITHIN, |ms| {
            HOLDER_ARMS_WITHIN.max(std::time::Duration::from_millis(ms))
        })
}

/// HAND `held`'s CLAIM TO A HOLDER ([`hold`]): this executable, as recorded at its `main`'s first
/// call and unchanged at its path ([`aterm_uds::tether::executable`]), spawned detached with a copy
/// of the claim's descriptor and the arming pipe, and armed: its pid and then its arming byte
/// read within [`holder_arms_within`]. Its child, which forks the holder away, is reaped. A
/// holder that did not arm is let go: the pipe closed under it (so a late arming fails and it
/// leaves, handing the claim off), and once its pid was read it is ended by proof of its
/// identity. The claim is this process's either way until it hands it off. Why it did not arm,
/// where it did not.
#[cfg(target_os = "macos")]
fn hand_over(held: &Held) -> Result<(), String> {
    if held.cohort {
        return Err("native cohort custody cannot transfer to an ordinary holder".into());
    }
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
    let (holder, said) = read_arming(&armed, holder_arms_within());
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
#[must_use = "return or inspect the build's propagated exit status"]
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

#[path = "build_admit_telemetry.rs"]
mod telemetry;
pub use telemetry::BuildEnd;

#[cfg(test)]
#[path = "build_admit_tests.rs"]
mod tests;
