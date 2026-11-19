// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The command line: `targo --unverified verify [CHECK|SET ...] [OPTIONS]`.
//!
//! Every setting is a flag; the CLI reads no environment knob of its own.

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::time::Duration;

use crate::checks::{CHECKS, Check, DEV, RELEASE, RELEASE_BUNDLE, RELEASE_TREE, Word, lookup};

/// `--judged <commit>`: with [`SHARED`] and `--base`, a round of `verify land`
/// ([`Args::judged`]).
pub const JUDGED: &str = "--judged";

/// `--shared <file>`: the paths a round's cone is read from ([`Args::shared`]).
pub const SHARED: &str = "--shared";

/// `--readers <file>`: with [`JUDGED`] and [`SHARED`], the files that may read the round's
/// shared Markdown ([`Args::readers`]).
pub const READERS: &str = "--readers";

/// `--by-hand`: with [`JUDGED`] and [`SHARED`], the round's merge is not the one the landing
/// makes of the judged commit and the base, Git's merge with its open-issues file merged by
/// entry ([`Args::by_hand`]).
pub const BY_HAND: &str = "--by-hand";

/// `--events-file-only`: with `--events`, the run's events go to that file and its retained
/// copy alone, never to its output ([`Args::events_file_only`]). `verify land` passes it to every
/// judgment, whose console then carries what a person reads.
pub const EVENTS_FILE_ONLY: &str = "--events-file-only";

/// `--queued-since <unix ms>`: when the work this run judges was first queued
/// ([`Args::queued_since`]). `verify land` passes its own start, so each judgment of one landing
/// keeps the landing's place in the judgment turn's queue.
pub const QUEUED_SINCE: &str = "--queued-since";

/// What the command line asked for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Args {
    /// The checks to run, in [`CHECKS`] order, without repeats.
    pub checks: Vec<Check>,
    /// `release` was named: whole workspace, `--app` exactly when `measure` runs, a skip is exit
    /// 3, and its first proven red ends the run except for the complete tree sweep
    /// ([`Self::tree_only`], [`crate::sched::Halt`]). `--tree` and
    /// `--bundle` narrow it to its halves ([`RELEASE_TREE`], [`RELEASE_BUNDLE`]), `--checks` to
    /// some of its checks; each is still a release's run of them.
    pub release: bool,
    /// `release --tree` without `--bundle`: finish every check before returning a red,
    /// unless `--fail-fast` explicitly requests cancellation.
    pub tree_only: bool,
    /// `--fail-fast`: every first proven red ends the run, as it ends other release runs. Without it,
    /// ordinary source/type preflight failures stop the run, while test failures keep the sweep.
    pub fail_fast: bool,
    /// `list`: print the table of checks and run nothing.
    pub list: bool,
    pub help: bool,
    /// `-p`/`--package`/`--scope`, in the order given.
    pub packages: Vec<String>,
    pub all: bool,
    pub base: Option<String>,
    /// [`JUDGED`]: the commit a judgment already stands for. With [`Self::shared`] and `--base`
    /// the run is a round of `verify land`: its checks read the change since `--base` as ever
    /// (so fmt, census and export-scan read the whole of it), and its crates are those of that
    /// cone that the shared paths, each compared with this commit's tree, or the change since
    /// this commit reach (`crate::changed::resolve_round`).
    pub judged: Option<String>,
    /// [`SHARED`]: the file that names a round's shared paths, each ended by a NUL.
    pub shared: Option<PathBuf>,
    /// [`READERS`]: the file that names, each ended by a NUL, the files that may read a shared
    /// path that is Markdown no cone holds: each is a reader of the round's change, so its
    /// member is judged where the merge's cone holds it, with its dependents unless only its
    /// tests, benches and examples compile the file, and main's change is judged as for a
    /// shared path a crate owns (`crate::changed::resolve_round`). `verify land` names them
    /// only for the landing's own merge, so never with [`Self::by_hand`].
    pub readers: Option<PathBuf>,
    /// [`BY_HAND`]: the round's merge is not the landing's own (it was resolved or edited by
    /// hand, or Git made it unlike the landing's merge of the open-issues file), so a shared
    /// path may be a change of the base's that the merge dropped, whose file is the judged
    /// commit's own: the paths are compared with no one tree, and no member is judged alone
    /// for one.
    pub by_hand: bool,
    pub app: Option<PathBuf>,
    /// Expected actual debug-assertion contract of the handed artifact, never a profile override.
    pub app_profile: crate::artifact::Profile,
    /// `--fix`: `fmt` or `census` rewrites what it finds.
    pub fix: bool,
    /// `--cell`: the forge cells `cells` checks, in the order given; empty is
    /// every cell but this machine's own.
    pub cells: Vec<String>,
    pub no_windows: bool,
    /// Only the GUI cursor-pacing burst, with no release-only AOS assertion.
    pub cursor_pacing: bool,
    /// Compile selected tests or measurement benches without executing them or qualifying a release.
    pub compile_only: bool,
    /// `--as-release`: with `test` and `--all`, that check as `release` runs it, the command a
    /// release's red row prints; with
    /// `test --all --compile-only`, what it compiles.
    pub as_release: bool,
    /// `--admit now|wait`: whether the run reclaims room once for the disk it will write, then
    /// starts if it fits or ends at once, each stage waits for room for its memory, and its timed
    /// stages for a quiet machine (the default), or it starts at once, saying what it lacks.
    /// AppKit correctness drivers run under the load found and report it; measurement stages
    /// still require qualified timing admission. Neither lifts the run's own memory budget
    /// ([`crate::sched::memory_holders`]).
    pub admit: aterm_capacity::Admit,
    /// `--test-threads`: the most libtest threads any test binary runs on.
    pub test_threads: Option<NonZeroU32>,
    /// Requested ceiling on the test run's CPU share; admission may grant less.
    pub test_cores: Option<NonZeroU32>,
    /// Planning duration records to read and update; never a verdict source.
    pub test_history: Option<PathBuf>,
    /// Replay every libtest binary's cases in a deterministic shuffled order.
    pub test_seed: Option<u64>,
    /// `--timeout`: `Some(None)` is `off`; `None` keeps the default ceiling.
    pub timeout: Option<Option<Duration>>,
    pub log: Option<PathBuf>,
    /// `--passed-lint-packages <path>`: prior successful per-package lint content proofs.
    pub passed_lint_packages: Option<PathBuf>,
    /// `--passed-appkit-groups <path>`: prior successful AppKit executable-group proofs.
    pub passed_appkit_groups: Option<PathBuf>,
    /// `--carry-context <sha256>`: a release's frozen verifier, seal and toolchain context for
    /// its lint, AppKit and source-reader evidence. It is part of every item content key.
    pub carry_context: Option<String>,
    /// [`QUEUED_SINCE`]: when the work it judges was first queued, unix ms: a heavy run's place
    /// in the judgment turn's queue (`crate::turn`) ages from here, not from its own start.
    pub queued_since: Option<u64>,
    /// `--events <path>`: the file each `check-finished`, `test-binary-finished` and
    /// `lint-package-finished` line is appended to whole, apart from the output its children share
    /// ([`crate::event_log`]): what a release's cutter keeps its records from, and `verify land`
    /// its judgment.
    pub events: Option<PathBuf>,
    /// [`EVENTS_FILE_ONLY`]: no `verify-event:` line reaches the run's output or its error
    /// stream; the events file and the copy kept beside the run's log hold every one. A release's
    /// cutter reads them from captured output (`ship status`), so a release run never names it.
    pub events_file_only: bool,
}

impl Args {
    /// Does any selected check read the selection (the cone, `-p`, `--all`)? `cells` reads `-p`
    /// alone: named with no `-p`, it judges every cell's whole graph.
    #[must_use]
    pub fn narrows(&self) -> bool {
        self.checks.iter().any(|c| {
            c.narrows()
                || (!self.packages.is_empty()
                    && c.spec().reach == crate::checks::Reach::TreeOrPackages)
        })
    }

    /// Does this run hand its `-p` specs to this host's cargo: a check that builds its selection
    /// here ([`Check::builds_selection`]: lint, test, checkers), with `-p` and no `--all`?
    /// Only such a run can reach cargo's resolver with a spec the host's graph lacks
    /// ([`crate::changed::foreign_specs`]); `cells` resolves each `-p` per cell, and
    /// `export-scan` reads files.
    #[must_use]
    pub fn builds_packages_here(&self) -> bool {
        !self.all && !self.packages.is_empty() && self.checks.iter().any(|c| c.builds_selection())
    }
}

/// A usage error, in one line, with where to look next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Usage {
    pub why: String,
    pub hint: &'static str,
}

impl Usage {
    fn new(why: impl Into<String>) -> Self {
        Self {
            why: why.into(),
            hint: "verify --help",
        }
    }

    #[must_use]
    pub fn message(&self) -> String {
        format!("verify: {} ({})", self.why, self.hint)
    }
}

/// Parse the arguments after `verify`.
///
/// # Errors
/// An unknown word or flag, a flag without its value, a value that is not one,
/// options that contradict each other or the checks named, `--judged` without `--shared` or
/// either without `--base`, `--by-hand` or `--readers` without them or together, `--tree`, `--bundle` or `--checks`
/// without `release`, `--events-file-only` without `--events` or with `release`, a `--checks` name that is no check of the release (or of the half named),
/// `--fix` under `release`, or a release run of `measure` without `--app`, or of no `measure`
/// with one.
pub fn parse<I, S>(args: I) -> Result<Args, Usage>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut a = Args::default();
    let mut words: Vec<Word> = Vec::new();
    let (mut tree, mut bundle) = (false, false);
    let mut listed: Vec<String> = Vec::new();
    let mut it = args.into_iter().map(Into::into);
    let usage = |s: String| Err(Usage::new(s));
    while let Some(arg) = it.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") || f == "-p" => {
                (f.to_string(), Some(v.to_string()))
            }
            _ => (arg.clone(), None),
        };
        let mut value = |name: &str| -> Result<String, Usage> {
            match inline.clone().or_else(|| it.next()) {
                Some(v) if v.starts_with('-') => {
                    Err(Usage::new(format!("{name} needs a value, not `{v}`")))
                }
                Some(v) if !v.is_empty() => Ok(v),
                _ => Err(Usage::new(format!("{name} needs a value"))),
            }
        };
        match flag.as_str() {
            "-h" | "--help" => a.help = true,
            "-p" | "--package" | "--scope" => a.packages.push(value(&flag)?),
            "--all" => a.all = true,
            "--base" => a.base = Some(value("--base")?),
            JUDGED => a.judged = Some(value(JUDGED)?),
            SHARED => a.shared = Some(PathBuf::from(value(SHARED)?)),
            READERS => a.readers = Some(PathBuf::from(value(READERS)?)),
            BY_HAND => a.by_hand = true,
            "--app" => a.app = Some(PathBuf::from(value("--app")?)),
            "--app-profile" => {
                a.app_profile = crate::artifact::Profile::parse(&value("--app-profile")?)
                    .ok_or_else(|| Usage::new("--app-profile requires debug or release"))?;
            }
            "--fix" => a.fix = true,
            "--cell" => a.cells.push(value("--cell")?),
            "--no-windows" => a.no_windows = true,
            "--cursor-pacing" => a.cursor_pacing = true,
            "--compile-only" => a.compile_only = true,
            "--as-release" => a.as_release = true,
            "--fail-fast" => a.fail_fast = true,
            "--tree" => tree = true,
            "--bundle" => bundle = true,
            "--checks" => listed.push(value("--checks")?),
            "--admit" => {
                let v = value("--admit")?;
                match aterm_capacity::Admit::parse(&v) {
                    Some(mode) => a.admit = mode,
                    None => return usage(format!("--admit takes now or wait, not `{v}`")),
                }
            }
            "--test-threads" => a.test_threads = Some(count("--test-threads", &value(&flag)?)?),
            "--test-cores" => a.test_cores = Some(count("--test-cores", &value(&flag)?)?),
            "--test-history" => a.test_history = Some(PathBuf::from(value(&flag)?)),
            "--test-seed" => {
                let raw = value(&flag)?;
                a.test_seed = Some(raw.parse().map_err(|_| {
                    Usage::new(format!(
                        "--test-seed takes an unsigned integer, not `{raw}`"
                    ))
                })?);
            }
            "--timeout" => {
                let v = value("--timeout")?;
                match crate::exec::parse_ceiling(&v) {
                    Some(t) => a.timeout = Some(t),
                    None => return usage(format!("--timeout takes seconds or `off`, not `{v}`")),
                }
            }
            "--log" => a.log = Some(PathBuf::from(value("--log")?)),
            "--passed-lint-packages" => {
                a.passed_lint_packages = Some(PathBuf::from(value("--passed-lint-packages")?));
            }
            "--passed-appkit-groups" => {
                a.passed_appkit_groups = Some(PathBuf::from(value("--passed-appkit-groups")?));
            }
            "--carry-context" => a.carry_context = Some(value("--carry-context")?),
            QUEUED_SINCE => {
                let raw = value(QUEUED_SINCE)?;
                a.queued_since = Some(raw.parse().map_err(|_| {
                    Usage::new(format!(
                        "{QUEUED_SINCE} takes unix milliseconds, not `{raw}`"
                    ))
                })?);
            }
            "--events" => a.events = Some(PathBuf::from(value("--events")?)),
            EVENTS_FILE_ONLY => a.events_file_only = true,
            p if p.starts_with("-p") && p.len() > 2 => a.packages.push(p[2..].to_string()),
            f if f.starts_with('-') => return usage(format!("unknown flag `{f}`")),
            w => match lookup(w) {
                Some(word) => words.push(word),
                None => {
                    return Err(Usage {
                        why: format!("unknown check `{w}`"),
                        hint: "verify list names every check",
                    });
                }
            },
        }
    }
    a.list = words.contains(&Word::List);
    if a.list && words.len() > 1 {
        return usage("`list` takes no check or set beside it".to_string());
    }
    a.release = words.contains(&Word::Release);
    // A help parse is Ok whatever `--checks` names, so its checks fall back to the whole release.
    a.tree_only = tree && !bundle;
    let release = release_checks(tree, bundle, &listed);
    let mut wanted: Vec<Check> = Vec::new();
    for w in &words {
        match w {
            Word::Check(c) => wanted.push(*c),
            Word::Dev => wanted.extend(DEV),
            Word::Release => wanted.extend(release.as_deref().unwrap_or(&RELEASE)),
            Word::List => {}
        }
    }
    if words.is_empty() {
        wanted.extend(DEV);
    }
    a.checks = CHECKS
        .iter()
        .map(|s| s.check)
        .filter(|c| wanted.contains(c))
        .collect();
    if a.help || a.list {
        return Ok(a);
    }
    if (tree || bundle || !listed.is_empty()) && !a.release {
        return usage("--tree, --bundle and --checks narrow `release`; name it".to_string());
    }
    release?;
    // The fix rewrites sources in place, so no other check may be reading
    // them: a compile beside it would judge half-written files. A release
    // judges the tree as it is, so never rewrites it.
    if a.fix && (a.release || (a.checks != [Check::Fmt] && a.checks != [Check::Census])) {
        return usage(
            "--fix rewrites sources; run it alone: verify fmt --fix, or verify census --fix"
                .to_string(),
        );
    }
    if !a.cells.is_empty() {
        if !a.checks.contains(&Check::Cells) {
            return usage("--cell narrows `cells`; name cells".to_string());
        }
        let known: Vec<String> = aterm_forge::resolve::default_cells()
            .into_iter()
            .map(|c| c.name)
            .collect();
        if let Some(n) = a.cells.iter().find(|n| !known.contains(n)) {
            return usage(format!("unknown cell `{n}` (cells: {})", known.join(" ")));
        }
    }
    if a.all && !a.packages.is_empty() {
        return usage("-p and --all contradict each other".to_string());
    }
    if a.base.is_some() && (a.all || !a.packages.is_empty()) {
        return usage("--base picks the change cone's base; -p and --all replace the cone".into());
    }
    if a.judged.is_some() != a.shared.is_some() {
        return usage(format!(
            "{JUDGED} and {SHARED} go together: a round names the judged commit and its shared \
             paths"
        ));
    }
    if a.by_hand && a.judged.is_none() {
        return usage(format!(
            "{BY_HAND} says how a round's merge was made; it needs {JUDGED} and {SHARED}"
        ));
    }
    if a.readers.is_some() && a.judged.is_none() {
        return usage(format!(
            "{READERS} names the files that may read a round's shared Markdown; it needs \
             {JUDGED} and {SHARED}"
        ));
    }
    if a.readers.is_some() && a.by_hand {
        return usage(format!(
            "{READERS} and {BY_HAND} contradict each other: a merge made by hand that shares \
             Markdown a file may read is judged in full, never by a round"
        ));
    }
    if a.judged.is_some() && (a.release || a.all || !a.packages.is_empty() || a.base.is_none()) {
        return usage(format!(
            "{JUDGED} and {SHARED} select a round's cone since --base; they need it, and -p, \
             --all and `release` replace the cone"
        ));
    }
    if a.compile_only
        && (a.release
            || a.checks.is_empty()
            || a.checks
                .iter()
                .any(|c| !matches!(c, Check::Test | Check::Checkers | Check::Measure)))
    {
        return usage(
            "--compile-only requires test, checkers or measure; it cannot qualify a release".into(),
        );
    }
    if a.as_release && (!a.all || a.checks.is_empty() || a.checks.iter().any(|c| *c != Check::Test))
    {
        return usage("--as-release goes with `test` and `--all`".into());
    }
    if a.cursor_pacing
        && (a.release || a.compile_only || a.checks != [Check::Measure] || a.app.is_none())
    {
        return usage(
            "--cursor-pacing requires only measure and --app; it cannot qualify a release".into(),
        );
    }
    if a.app_profile == crate::artifact::Profile::Debug
        && (a.release || a.as_release || a.app.is_none() || a.checks != [Check::Measure])
    {
        return usage("--app-profile debug requires only measure and --app; debug validation cannot qualify a release".into());
    }
    if a.release {
        if !a.packages.is_empty() || a.base.is_some() || !a.cells.is_empty() {
            return usage(
                "`release` runs the whole workspace; drop -p, --base and --cell".to_string(),
            );
        }
        a.all = true;
        let measures = a.checks.contains(&Check::Measure);
        if measures && a.app.is_none() {
            return usage(
                "`release` needs the bundle `measure` judges: --app <path to aterm.app>".into(),
            );
        }
        if !measures && a.app.is_some() {
            return usage(
                "this release runs no `measure`, which alone reads --app; drop it".into(),
            );
        }
    }
    if a.events_file_only && (a.events.is_none() || a.release) {
        return usage(format!(
            "{EVENTS_FILE_ONLY} keeps a run's events in its --events file alone; it needs \
             --events, and a release's cutter reads them from its output, so not `release`"
        ));
    }
    if a.passed_lint_packages.is_some() && !(a.release && a.checks.contains(&Check::Lint)) {
        return usage(
            "--passed-lint-packages carries package lint proofs into a release's lint run; run \
             it with `release` and its lint check"
                .into(),
        );
    }
    if a.passed_appkit_groups.is_some() && !(a.release && a.checks.contains(&Check::Appkit)) {
        return usage(
            "--passed-appkit-groups carries exact executable proofs into a release's AppKit run; \
             run it with `release` and its appkit check"
                .into(),
        );
    }
    if let Some(context) = &a.carry_context {
        if !(a.release
            && a.checks
                .iter()
                .any(|check| matches!(check, Check::Test | Check::Lint | Check::Appkit)))
        {
            return usage(
                "--carry-context salts a release's lint, AppKit and source-reader evidence; run \
                 it with `release` and its test, lint or appkit check"
                    .into(),
            );
        }
        if !aterm_codec::hex::is_lower(context, 64) {
            return usage("--carry-context takes exactly 64 lowercase hex digits".into());
        }
    }
    if (a.passed_lint_packages.is_some() || a.passed_appkit_groups.is_some())
        && a.carry_context.is_none()
    {
        return usage("prior content proofs require --carry-context <64 lowercase hex>".into());
    }
    Ok(a)
}

/// The checks `release` runs: its tree half under `--tree`, its bundle half under `--bundle`
/// (both, with both), else all of [`RELEASE`]; of those, only the ones `--checks` lists
/// (comma-separated, repeatable), when it lists any.
fn release_checks(tree: bool, bundle: bool, listed: &[String]) -> Result<Vec<Check>, Usage> {
    let halves: Vec<Check> = if tree || bundle {
        let tree = RELEASE_TREE.iter().filter(|_| tree);
        tree.chain(RELEASE_BUNDLE.iter().filter(|_| bundle))
            .copied()
            .collect()
    } else {
        RELEASE.to_vec()
    };
    if listed.is_empty() {
        return Ok(halves);
    }
    let mut chosen = Vec::new();
    for name in listed.iter().flat_map(|l| l.split(',')) {
        match lookup(name) {
            Some(Word::Check(c)) if halves.contains(&c) => chosen.push(c),
            Some(Word::Check(_)) => {
                let of: Vec<&str> = halves.iter().map(|c| c.name()).collect();
                return Err(Usage::new(format!(
                    "--checks: `{name}` is not one of this release's checks ({})",
                    of.join(" ")
                )));
            }
            _ => {
                return Err(Usage {
                    why: format!("--checks: unknown check `{name}`"),
                    hint: "verify list names every check",
                });
            }
        }
    }
    Ok(chosen)
}

/// A note when `-p` or `--base` was given and no selected check reads a
/// selection, so neither changes what runs.
#[must_use]
pub fn ignored_selection(a: &Args) -> Option<String> {
    if a.narrows() || a.release {
        return None;
    }
    let flags = match (a.packages.is_empty(), a.base.is_none()) {
        (true, true) => return None,
        (false, true) => "-p is",
        (true, false) => "--base is",
        (false, false) => "-p and --base are",
    };
    let names: Vec<&str> = a.checks.iter().map(|c| c.name()).collect();
    let verb = if names.len() == 1 { "reads" } else { "read" };
    Some(format!(
        "{} {verb} the whole tree, so {flags} ignored",
        names.join(" ")
    ))
}

fn count(flag: &str, v: &str) -> Result<NonZeroU32, Usage> {
    v.trim()
        .parse::<NonZeroU32>()
        .map_err(|_| Usage::new(format!("{flag} takes a positive whole number, not `{v}`")))
}

/// `--help`: short and example-led.
#[must_use]
pub fn usage() -> String {
    let names: Vec<&str> = CHECKS.iter().map(|s| s.name).collect();
    let mut checks = String::new();
    let mut line = String::from("  ");
    for n in names {
        if line.len() + n.len() > 78 {
            checks.push_str(line.trim_end());
            checks.push('\n');
            line = String::from("  ");
        }
        line.push_str(n);
        line.push(' ');
    }
    checks.push_str(line.trim_end());
    let reserve = aterm_capacity::span(aterm_capacity::QUIET_PATIENCE.reserve);
    let start = aterm_capacity::span(aterm_capacity::QUIET_PATIENCE.start);
    let half = |checks: &[Check]| {
        checks
            .iter()
            .map(|c| c.name())
            .collect::<Vec<_>>()
            .join(" ")
    };
    let (tree, bundle) = (half(&RELEASE_TREE), half(&RELEASE_BUNDLE));
    format!(
        "targo --unverified verify [CHECK|SET ...] [OPTIONS]

Run aterm's checks in place, alone or in any combination.

  verify                          the dev set on what changed since origin/main
  verify lint test -p aterm-grid  two checks on one crate
  verify test --all               every crate's tests and doctests
  verify fmt --fix                rewrite what fmt finds unformatted
  verify census --fix             move test modules a release build reads out of line
  verify release --app <APP>      everything publication requires
  verify release --tree           its tree half: no bundle, nothing timed
  verify watch                    the follower: judges main tips into refs/aterm/judged
  verify watch --once             judge one tip; return repair context to the caller
  verify land                     merge main, judge the cone since it, push; compose a
                                  moved main, or judge the merge over what the two
                                  share (`verify land --help`)
  verify land --tab               optional program-worker tab: the person's explicit
                                  request, or persistence native command tools
                                  cannot provide; state why before spawning
  verify release --bundle [--app <APP>]
                                  its bundle CLI half: appkit needs no app;
                                  measure needs one after the build and tree
  verify list                     every check: sets, needs, what it runs
  verify perf [LANE...]           the performance lanes (`verify perf --list`)
  verify soak [--ledger PATH]     the crash restore's soak (keeper P4): this
                                  machine's recovery ledger, not the tree
  verify glue <in.wasm> <out-dir>
                                  a wasm module's ES module glue (`verify glue --help`)
  verify vendor-out --into <dir> [--check]
                                  sealed aterm-phase and aterm-agent-names
                                  projections beside <dir>'s independently pinned
                                  vendor/alab-harness; policy dependencies use
                                  that pin (`verify vendor-out --help`)
  verify provision [--linux] [--check] [ROW...]
                                  the tools this checkout pins (fish, bash
                                  5.3, pwsh, binaryen, the browsers, rustup's
                                  stock exceptions; lima),
                                  fetched or built into this user's cache
  verify browser-site DIR         the browser check's first half: both web
                                  modules built and the site laid in DIR
  verify browser-drive SITE OUT [--engine metal|swiftshader|firefox|webkit|safari]...
                       [--no-worker NAME]... [--admit wait|now]
                                  its second half: the sessions run on a laid
                                  site, frames in OUT (no targo: the Linux
                                  lane's guest); webkit and safari, whose pages
                                  keep windows (WebKit's on macOS), only when
                                  named; safari's waits for a quiet machine
                                  (--admit wait, the default; under now it is
                                  could-not-run); --no-worker leaves a
                                  session's worker row out of the run's set

Sets:
  dev      released-shim fmt census export-scan lint test (the default);
           released-shim builds only when bootstrap inputs change; its lint brings cells,
           one cell per other OS that builds a selected crate, and one of
           this OS for a lint with debug assertions off, which shipped code
           calling a debug-only seam fails
  release  every check `ship release` requires; --all implied, --app with measure;
           its tree half (--tree): {tree}
           its bundle half (--bundle): {bundle}

Read existing observations without running checks:
  verify status --events <path> [--json]

Checks:
{checks}

Options:
  -p, --package <crate>   narrow lint/test/cells to a crate (repeatable)
      --all               the whole workspace (default: the change cone)
      --base <ref>        the cone's base (origin/main, else main; no fetch);
                          cannot combine with -p or --all, which replace the cone
      --judged <commit> --shared <file> [--readers <file> | --by-hand]
                          with --base, a round of `verify land`: the crates are
                          those of the cone since --base that the paths the
                          file names (each ended by a NUL), or the change
                          since the judged commit, reach, while each check
                          still reads the change since --base; --readers: the
                          files that may read the Markdown among those paths
                          (each ended by a NUL), whose crates in that cone
                          are judged too;
                          --by-hand: the merge was resolved or edited by hand,
                          so no crate is judged alone for a path the file names
      --app <APP>         the bundle `measure` judges
      --app-profile <P>   expected compiled assertions: release (default) or debug;
                          debug requires ordinary measure, names shipping size as not run,
                          and cannot qualify a release
      --fix               with fmt alone: rewrite the files it finds unformatted;
                          with census alone: move the test modules it finds out of line
      --cell <name>       the forge cell `cells` checks (repeatable; any cell)
      --compile-only      with test/checkers/measure: compile tests or measuring benches without executing them
      --as-release        with test and --all: as release runs it, with
                          ATERM_SPEC_CHECKERS=1, a disposable HOME
                          holding release history, and test owes the golden frame
                          test (--compile-only: what release's test stage compiles)
      --cursor-pacing     with measure --app: run only the GUI pacing burst
      --no-windows        skip each stage that opens a window (a named skip)
      --tree              with release: its complete tree sweep (unless --fail-fast)
      --bundle            with release: only its bundle half (--app only with measure)
      --checks <a,b,..>   with release: only these of its checks (a resume),
                          each as the release runs it
      --fail-fast         end the run at its first failure:
                          what it stopped reads NOT RUN (a check that could
                          not run, its machine's fault, never ends it)
                          ordinary source/type preflight failures stop the run;
                          release --tree otherwise completes its full sweep
      --admit <wait|now>  reclaim room once for the disk the run will write,
                          then start it if it fits or end it at once; wait
                          for each stage's memory before it starts (held
                          until it ends, beside other runs' claims), and
                          for a quiet machine before the first timed stage,
                          held through the last (default wait, saying why
                          once a minute; next in line, it holds back claimed
                          work after {reserve} and starts whatever the load
                          after {start}); now starts AppKit correctness
                          drivers under the load found and reports it,
                          while a measurement without timing qualification
                          and a starved take are refused; the
                          Linux lane's lab is given it too. Under either,
                          the run's stages start only while the memory
                          they claim fits together in what can ever be
                          available here (one that alone does not runs
                          with no other that claims memory)
      --test-threads <n>  RUST_TEST_THREADS ceiling for every test binary; each
                          runs at the width its last runs call for, charged
                          the cores it used, and the binaries running at
                          once stay within held CARGO_BUILD_JOBS, or host
                          cores when no build-slot share is held
      --test-cores <n>    request at most n CPU cores for test execution;
                          admission and the release pool may grant less
      --test-seed <n>     deterministic libtest shuffle at each planned width
      --test-history <path>
                          planning durations to read and update (default: account history);
                          never reuses a verdict
      --timeout <s|off>   hang ceiling on each child's work (default 3h); its
                          waits for a lock or for room are left out
      --log <path>        where the full log goes (default: a new file in
                          <target>/verify/.ubd/logs.noindex/runs.noindex; retention
                          targets 20 while preserving all running logs)
      --passed-lint-packages <path>
                          with release and lint: prior schema-1 per-package lint
                          proofs; only a prior ok with the same complete compiler
                          input key is carried
      --passed-appkit-groups <path>
                          with release and appkit: prior schema-1 executable-group
                          proofs; each current group is built and carried only when
                          its exact executable-byte key still matches
      --carry-context <sha256>
                          a release's frozen context for its lint, appkit and
                          source-reader evidence; required with prior content
                          proofs
      --events <path>     append each check-finished and test-binary-finished
                          line there too, whole; with the run's land plan
                          (run-planned, run-resealed: each check's key)
      --events-file-only  with --events, outside release: keep every event
                          line off the console (the file and the copy beside
                          the log hold them; `verify status --events` reads one)
  -h, --help

Exit: 0 every check that ran passed, 1 a check failed, 2 usage, 3 could not run.
A skip names its fix and fails nothing; under `release` it is exit 3. A checkout
whose HEAD moved to another commit mid-run is exit 3: its checks read both trees.
"
    )
}

#[cfg(test)]
#[path = "cli_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "cli_cursor_pacing_tests.rs"]
mod cursor_pacing_tests;
