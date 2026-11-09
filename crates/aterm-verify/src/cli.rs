// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The command line: `targo --unverified verify [CHECK|SET ...] [OPTIONS]`.
//!
//! Every setting is a flag; the CLI reads no environment knob of its own.

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::time::Duration;

use crate::checks::{CHECKS, Check, DEV, RELEASE, RELEASE_BUNDLE, RELEASE_TREE, Word, lookup};

/// What the command line asked for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Args {
    /// The checks to run, in [`CHECKS`] order, without repeats.
    pub checks: Vec<Check>,
    /// `release` was named: whole workspace, `--app` exactly when `measure` runs, a skip is exit
    /// 3, and its first proven red ends the run ([`crate::sched::Halt`]). `--tree` and
    /// `--bundle` narrow it to its halves ([`RELEASE_TREE`], [`RELEASE_BUNDLE`]), `--checks` to
    /// some of its checks; each is still a release's run of them.
    pub release: bool,
    /// `--fail-fast`: the run's first proven red ends it, as it ends a release; a bare run keeps
    /// its whole sweep.
    pub fail_fast: bool,
    /// Repeatable tree sample: overlapping resources queue behind Ordinary work; Release can end it.
    pub background: bool,
    /// `list`: print the table of checks and run nothing.
    pub list: bool,
    pub help: bool,
    /// `-p`/`--package`/`--scope`, in the order given.
    pub packages: Vec<String>,
    pub all: bool,
    pub base: Option<String>,
    pub app: Option<PathBuf>,
    /// `--fix`: `fmt` rewrites what it finds.
    pub fix: bool,
    /// `--cell`: the forge cells `cells` checks, in the order given; empty is
    /// every cell but this machine's own.
    pub cells: Vec<String>,
    pub no_windows: bool,
    /// Only the GUI cursor-pacing burst, with no release-only AOS assertion.
    pub cursor_pacing: bool,
    /// Compile selected tests or measurement benches without executing them or qualifying a release.
    pub compile_only: bool,
    /// `--as-release`: with `test` or `doc` and `--all`, that check as `release` runs it, pulling
    /// nothing ([`Args::wanted`]), the command a release's red row prints; with
    /// `test --all --compile-only`, what it compiles.
    pub as_release: bool,
    /// `--admit now|wait`: whether the run waits for room for the disk it will write, each stage
    /// for room for its memory, and its timed stages for a quiet machine (the default), or starts
    /// at once, saying what it lacks. Neither lifts the run's own memory budget
    /// ([`crate::sched::memory_holders`]).
    pub admit: aterm_capacity::Admit,
    /// `--test-threads`: the most libtest threads any test binary runs on.
    pub test_threads: Option<NonZeroU32>,
    /// Requested ceiling on the test run's CPU share; admission may grant less.
    pub test_cores: Option<NonZeroU32>,
    /// Replay every libtest binary's cases in a deterministic shuffled order.
    pub test_seed: Option<u64>,
    /// `--timeout`: `Some(None)` is `off`; `None` keeps the default ceiling.
    pub timeout: Option<Option<Duration>>,
    pub log: Option<PathBuf>,
    /// `--passed-binaries <path>`: with `release` running `test`, the test binaries that passed in
    /// this tree before, each `<sha256> <spec>` on its own line; one whose executable still
    /// hashes so is carried, not run again ([`crate::testrun::carry`]).
    pub passed_binaries: Option<PathBuf>,
    /// `--events <path>`: the file each `check-finished` and `test-binary-finished` line is
    /// appended to whole, apart from the output its children share ([`crate::event_log`]): what a
    /// release's cutter keeps its records from, and `verify land` its judgment.
    pub events: Option<PathBuf>,
}

impl Args {
    /// The checks this command runs over a selection `holds` reads: those it names and what they
    /// pull ([`crate::checks::with_pulled`]). `release` pulls nothing, and so neither does
    /// `--as-release`, which is one of its checks as it ran: a pulled check's verdict would answer
    /// for something no release runs.
    #[must_use]
    pub fn wanted(&self, holds: impl Fn(&str) -> bool) -> Vec<Check> {
        crate::checks::with_pulled(&self.checks, self.release || self.as_release, holds)
    }

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
    /// here ([`Check::builds_selection`]: lint, test, doc, checkers), with `-p` and no `--all`?
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
/// options that contradict each other or the checks named, `--tree`, `--bundle` or `--checks`
/// without `release`, a `--checks` name that is no check of the release (or of the half named),
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
            "--app" => a.app = Some(PathBuf::from(value("--app")?)),
            "--fix" => a.fix = true,
            "--cell" => a.cells.push(value("--cell")?),
            "--no-windows" => a.no_windows = true,
            "--cursor-pacing" => a.cursor_pacing = true,
            "--compile-only" => a.compile_only = true,
            "--as-release" => a.as_release = true,
            "--fail-fast" => a.fail_fast = true,
            "--background" => a.background = true,
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
            "--passed-binaries" => {
                a.passed_binaries = Some(PathBuf::from(value("--passed-binaries")?));
            }
            "--events" => a.events = Some(PathBuf::from(value("--events")?)),
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
    if a.fix && (a.release || a.checks != [Check::Fmt]) {
        return usage("--fix rewrites sources; run it alone: verify fmt --fix".to_string());
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
    if a.background && !(a.release && tree && !bundle) {
        return usage("--background requires release --tree".into());
    }
    if a.as_release
        && (!a.all
            || a.checks.is_empty()
            || a.checks
                .iter()
                .any(|c| !matches!(c, Check::Test | Check::Doc)))
    {
        return usage("--as-release goes with `test` or `doc`, and `--all`".into());
    }
    if a.cursor_pacing
        && (a.release || a.compile_only || a.checks != [Check::Measure] || a.app.is_none())
    {
        return usage(
            "--cursor-pacing requires only measure and --app; it cannot qualify a release".into(),
        );
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
    if a.passed_binaries.is_some() && !(a.release && a.checks.contains(&Check::Test)) {
        return usage(
            "--passed-binaries carries test binaries into a release's test run; run it with \
             `release` and its test check"
                .into(),
        );
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
  verify test --all               every crate's tests (+ wasm-smoke)
  verify fmt --fix                rewrite what fmt finds unformatted
  verify release --app <APP>      everything publication requires
  verify release --tree           its tree half: no bundle, nothing timed
  verify watch                    idle-capacity main samples; journaled verdicts, no gate
  verify land                     merge main, judge the cone since it, push; compose a
                                  moved main or re-judge its delta (`verify land --help`)
  verify release --bundle --app <APP>
                                  its bundle half, after the build and the tree
  verify list                     every check: sets, needs, what it runs
  verify perf [LANE...]           the performance lanes (`verify perf --list`)
  verify soak [--ledger PATH]     the crash restore's soak (keeper P4): this
                                  machine's recovery ledger, not the tree
  verify glue <in.wasm> <out-dir>
                                  a wasm module's ES module glue (`verify glue --help`)
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
  dev      fmt census export-scan lint test doc (the default); its lint brings cells,
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
  -p, --package <crate>   narrow lint/test/doc/cells to a crate (repeatable)
      --all               the whole workspace (default: the change cone)
      --base <ref>        the cone's base (origin/main, else main; no fetch);
                          cannot combine with -p or --all, which replace the cone
      --app <APP>         the release bundle `measure` judges
      --fix               with fmt alone: rewrite the files it finds unformatted
      --cell <name>       the forge cell `cells` checks (repeatable; any cell)
      --compile-only      with test/checkers/measure: compile tests or measuring benches without executing them
      --as-release        with test or doc and --all: as release runs them, with
                          ATERM_SPEC_CHECKERS=1, ATERM_RELEASE_RUN=1, a disposable HOME
                          holding release history, and test owes the golden frame
                          test (--compile-only: what release's test stage compiles)
      --cursor-pacing     with measure --app: run only the GUI pacing burst
      --no-windows        skip each stage that opens a window (a named skip)
      --tree              with release: only its tree half
      --bundle            with release: only its bundle half (--app)
      --checks <a,b,..>   with release: only these of its checks (a resume),
                          each as the release runs it
      --fail-fast         end the run at its first failure, as release does:
                          what it stopped reads NOT RUN (a check that could
                          not run, its machine's fault, never ends it)
      --admit <wait|now>  wait for room for the disk the run will write, for
                          each stage's memory before it starts (held until
                          it ends, beside other runs' claims), and for a
                          quiet machine before the first timed stage,
                          held through the last (default wait, saying why
                          once a minute; next in line, it holds back claimed
                          work after {reserve} and starts whatever the load
                          after {start}); now starts at once, saying what
                          it lacks, and a starved take is refused; the
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
      --timeout <s|off>   hang ceiling on each child's work (default 3h); its
                          waits for a lock or for room are left out
      --log <path>        where the full log goes (default: a new file in
                          <target>/verify/logs.noindex/runs.noindex; retention
                          targets 20 while preserving all running logs)
      --passed-binaries <path>
                          with release and its test check: binaries that passed
                          in this tree, `<sha256> <spec>` per line; each whose
                          executable hashes the same is carried, not run again
      --events <path>     append each check-finished and test-binary-finished
                          line there too, whole
  -h, --help

Exit: 0 every check that ran passed, 1 a check failed, 2 usage, 3 could not run.
A skip names its fix and fails nothing; under `release` it is exit 3. A checkout
whose HEAD moved to another commit mid-run is exit 3: its checks read both trees.
"
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn background_is_only_a_repeatable_tree_sample() {
        let parse = |args: &[&str]| super::parse(args.iter().copied());
        assert!(
            parse(&["release", "--tree", "--background"])
                .unwrap()
                .background
        );
        for args in [
            vec!["test", "--background"],
            vec!["release", "--background"],
            vec!["release", "--bundle", "--app", "app", "--background"],
        ] {
            assert!(parse(&args).is_err());
        }
    }
    use super::*;

    fn ok(args: &[&str]) -> Args {
        parse(args.iter().copied()).unwrap_or_else(|e| panic!("{args:?}: {}", e.message()))
    }

    /// The bare command is the dev set on the cone; checks and sets combine
    /// in one order without repeats.
    #[test]
    fn checks_and_sets_combine_in_one_order_and_the_default_is_dev() {
        let a = ok(&[]);
        assert_eq!(a.checks, DEV);
        assert!(!a.all && a.packages.is_empty() && !a.release && a.narrows());
        assert_eq!(ok(&["dev"]).checks, DEV);
        assert_eq!(
            ok(&["test", "lint", "test", "fmt"]).checks,
            [Check::Fmt, Check::Lint, Check::Test]
        );
        let a = ok(&["dev", "forge"]);
        assert_eq!(a.checks.len(), DEV.len() + 1);
        assert!(a.checks.contains(&Check::Forge));
        assert!(!ok(&["fmt", "census"]).narrows());
    }

    /// Only a run whose lint, test, doc or checkers builds its `-p` with this host's cargo asks
    /// whether the host's graph holds each spec. CONTROL: `export-scan` and `cells` read `-p`
    /// too ([`Args::narrows`]), and are not asked.
    #[test]
    fn only_a_check_that_builds_its_selection_here_hands_cargo_its_specs() {
        let p = "windows-sys@0.61.2";
        for (args, here) in [
            (vec!["-p", p], true),
            (vec!["lint", "-p", p], true),
            (vec!["test", "-p", p], true),
            (vec!["doc", "-p", p], true),
            (vec!["checkers", "-p", p], true),
            (vec!["export-scan", "-p", p], false),
            (vec!["cells", "-p", p], false),
            (vec!["fmt", "census", "-p", p], false),
            (vec!["lint"], false),
            (vec!["lint", "--all"], false),
        ] {
            let a = ok(&args);
            assert_eq!(a.builds_packages_here(), here, "{args:?}");
        }
        assert!(ok(&["export-scan", "-p", p]).narrows());
        assert!(ok(&["cells", "-p", p]).narrows());
    }

    #[test]
    fn release_implies_the_whole_workspace_and_needs_its_bundle() {
        let a = ok(&["release", "--app", "/x/aterm.app"]);
        assert!(a.release && a.all);
        assert_eq!(
            a.checks,
            CHECKS
                .iter()
                .map(|s| s.check)
                .filter(|c| RELEASE.contains(c))
                .collect::<Vec<_>>()
        );
        assert!(parse(["release"]).is_err(), "release needs its bundle");
        assert!(parse(["release", "--app", "a", "-p", "x"]).is_err());
        assert!(parse(["release", "cells", "--app", "a", "--cell", "win"]).is_err());
    }

    /// `--passed-binaries` goes only with a release whose run holds `test`, the whole release's
    /// or a resume's; `--events` with any run (a release's cutter and `verify land` keep them).
    /// Negative control: dropping the first rule lets a dev `test --all` take the binaries.
    #[test]
    fn a_release_resume_carries_the_binaries_that_passed_only_into_its_test_run() {
        assert_eq!(
            ok(&["release", "--tree", "--events", "/l/06-verify.events"])
                .events
                .as_deref(),
            Some(std::path::Path::new("/l/06-verify.events"))
        );
        let a = ok(&[
            "release",
            "--checks=test",
            "--passed-binaries",
            "/l/06-verify.passed-binaries",
        ]);
        assert_eq!(a.checks, [Check::Test]);
        assert_eq!(
            a.passed_binaries.as_deref(),
            Some(std::path::Path::new("/l/06-verify.passed-binaries"))
        );
        assert!(
            ok(&["release", "--passed-binaries", "/p", "--app", "/a"])
                .checks
                .contains(&Check::Test)
        );
        for args in [
            &["release", "--checks", "fmt", "--passed-binaries", "/p"][..],
            &[
                "release",
                "--bundle",
                "--passed-binaries",
                "/p",
                "--app",
                "/a",
            ],
            &["test", "--all", "--passed-binaries", "/p"],
        ] {
            assert!(parse(args.iter().copied()).is_err(), "{args:?}");
        }
        assert_eq!(
            ok(&["--base", "abc", "--events", "/e"]).events.as_deref(),
            Some(std::path::Path::new("/e")),
            "a dev run keeps its events (verify land's judgment)"
        );
    }

    /// `release --tree` and `release --bundle --app A` run its halves, both its whole set, and
    /// `--checks` some of its checks; each is a release (whole workspace, its halt), `--app` goes
    /// exactly with `measure`. Controls: each flag without `release`, a listed check outside the
    /// release or its half, an unknown one, and `--app` beside no `measure`, are usage errors.
    #[test]
    fn release_narrows_to_its_halves_and_to_the_checks_listed() {
        let in_order = |set: &[Check]| -> Vec<Check> {
            CHECKS
                .iter()
                .map(|s| s.check)
                .filter(|c| set.contains(c))
                .collect()
        };
        let tree = ok(&["release", "--tree"]);
        assert!(tree.release && tree.all && tree.app.is_none());
        assert_eq!(tree.checks, in_order(&RELEASE_TREE));
        let bundle = ok(&["release", "--bundle", "--app", "a"]);
        assert!(bundle.release && bundle.all);
        assert_eq!(bundle.checks, in_order(&RELEASE_BUNDLE));
        assert_eq!(
            ok(&["release", "--tree", "--bundle", "--app", "a"]).checks,
            ok(&["release", "--app", "a"]).checks
        );
        let resume = ok(&["release", "--checks", "spec,test", "--checks=fmt"]);
        assert!(resume.release && resume.all);
        assert_eq!(resume.checks, [Check::Fmt, Check::Test, Check::Spec]);
        assert_eq!(
            ok(&["release", "--bundle", "--checks", "appkit"]).checks,
            [Check::Appkit],
            "appkit reads no bundle"
        );
        assert_eq!(
            ok(&["release", "--checks", "measure", "--app", "a"]).checks,
            [Check::Measure]
        );
        // Help and list are Ok before any refusal, as they were.
        assert!(ok(&["release", "--tree", "--help"]).help);
        assert_eq!(ok(&["release", "--tree", "--help"]).checks, tree.checks);
        assert!(ok(&["list", "--tree"]).list);
        for args in [
            &["--tree"][..],
            &["lint", "--bundle"],
            &["test", "--checks", "test"],
            &["release", "--checks", "cells"],
            &["release", "--tree", "--checks", "appkit"],
            &["release", "--checks", "tset"],
            &["release", "--checks", "test,"],
            &["release", "--tree", "--app", "a"],
            &["release", "--checks", "lint", "--app", "a"],
            &["release", "--bundle"],
            &["release", "--checks", "fmt", "--fix"],
        ] {
            assert!(parse(args.iter().copied()).is_err(), "{args:?}");
        }
    }

    #[test]
    fn contradictions_and_unknowns_are_usage_errors() {
        for args in [
            &["-p", "x", "--all"][..],
            &["--base", "main", "--all"],
            &["lnit"],
            &["--full"],
            &["-p"],
            &["-p="],
            &["-p=--all"],
            &["-p", "--all"],
            &["--test-threads", "0"],
            &["--test-cores", "0"],
            &["--test-seed", "-1"],
            &["--test-seed", "not-a-seed"],
            &["--test-jobs", "2"],
            &["--timeout", "45m"],
            &["--admit", "later"],
            &["list", "fmt"],
            // --fix rewrites sources, so it runs alone, and only with fmt.
            &["lint", "--fix"],
            &["--fix"],
            &["fmt", "lint", "--fix"],
            &["release", "--app", "a", "--fix"],
            // --cell narrows `cells` to cells forge knows.
            &["lint", "--cell", "win"],
            &["cells", "--cell", "plan9"],
        ] {
            assert!(parse(args.iter().copied()).is_err(), "{args:?}");
        }
        assert!(ok(&["fmt", "--fix"]).fix);
        assert_eq!(
            ok(&["cells", "--cell", "win", "--cell=linux"]).cells,
            ["win", "linux"]
        );
    }

    #[test]
    fn values_parse_into_their_types() {
        let a = ok(&[
            "lint",
            "-p",
            "a",
            "--package",
            "b",
            "--scope=c",
            "-pd",
            "-p=e",
        ]);
        assert_eq!(a.packages, ["a", "b", "c", "d", "e"]);
        let a = ok(&[
            "test",
            "--test-threads=3",
            "--test-cores=10",
            "--test-seed=17",
            "--timeout",
            "off",
            "--log",
            "/tmp/x.log",
            "--no-windows",
            "--base",
            "main",
        ]);
        assert_eq!(a.test_threads.map(NonZeroU32::get), Some(3));
        assert_eq!(a.test_cores.map(NonZeroU32::get), Some(10));
        assert_eq!(a.test_seed, Some(17));
        assert_eq!(a.timeout, Some(None));
        assert_eq!(a.log.as_deref(), Some(std::path::Path::new("/tmp/x.log")));
        assert!(a.no_windows);
        assert!(!a.fail_fast, "a bare run keeps its whole sweep");
        assert!(ok(&["test", "--fail-fast"]).fail_fast);
        assert_eq!(a.base.as_deref(), Some("main"));
        assert_eq!(a.admit, aterm_capacity::Admit::Wait, "wait by default");
        assert_eq!(
            ok(&["measure", "--admit", "now"]).admit,
            aterm_capacity::Admit::Now
        );
        assert_eq!(
            ok(&["measure", "--admit=wait"]).admit,
            aterm_capacity::Admit::Wait
        );
        assert!(parse(["measure", "--admit", "soon"]).is_err());
        assert_eq!(
            ok(&["--timeout", "90"]).timeout,
            Some(Some(Duration::from_secs(90)))
        );
        assert_eq!(ok(&["test"]).admit, atpkg::admit_disk::Admit::Wait);
        assert_eq!(
            ok(&["test", "--admit", "now"]).admit,
            atpkg::admit_disk::Admit::Now
        );
    }
}

#[cfg(test)]
mod cursor_pacing_tests {
    #[test]
    fn compilation_only_cannot_replace_test_execution_or_release_qualification() {
        assert!(
            super::parse(["test", "--all", "--compile-only"])
                .unwrap()
                .compile_only
        );
        for checks in [vec!["checkers"], vec!["test", "checkers"]] {
            let mut args = checks;
            args.extend(["--all", "--compile-only"]);
            assert!(super::parse(args).unwrap().compile_only);
        }
        for args in [
            vec!["release", "--app", "/test.app", "--compile-only"],
            vec!["test", "lint", "--compile-only"],
            vec!["--compile-only"],
        ] {
            assert!(super::parse(args).is_err());
        }
        for args in [
            vec!["test", "--all", "--compile-only", "--as-release"],
            vec!["test", "--all", "--as-release"],
            vec!["doc", "--all", "--as-release"],
        ] {
            assert!(super::parse(args).unwrap().as_release);
        }
        for args in [
            vec!["test", "--as-release"],
            vec!["--all", "--as-release"],
            vec!["lint", "--all", "--as-release"],
            vec!["test", "--compile-only", "--as-release"],
            vec![
                "test",
                "checkers",
                "--all",
                "--compile-only",
                "--as-release",
            ],
            vec!["release", "--app", "/test.app", "--as-release"],
        ] {
            assert!(super::parse(args).is_err());
        }
    }

    #[test]
    fn focused_measurement_cannot_replace_release_qualification() {
        let good = super::parse(["measure", "--app", "/test.app", "--cursor-pacing"]).unwrap();
        assert!(good.cursor_pacing);
        for bad in [
            vec!["release", "--app", "/test.app", "--cursor-pacing"],
            vec!["test", "--app", "/test.app", "--cursor-pacing"],
            vec!["measure", "--cursor-pacing"],
        ] {
            assert!(super::parse(bad).is_err());
        }
    }
}
