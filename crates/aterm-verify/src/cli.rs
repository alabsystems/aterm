// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The command line: `targo --unverified verify [CHECK|SET ...] [OPTIONS]`.
//!
//! Every setting is a flag; the CLI reads no environment knob of its own.

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::time::Duration;

use crate::checks::{CHECKS, Check, DEV, RELEASE, Word, lookup};

/// What the command line asked for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Args {
    /// The checks to run, in [`CHECKS`] order, without repeats.
    pub checks: Vec<Check>,
    /// `release` was named: whole workspace, `--app` required, a skip is exit 3.
    pub release: bool,
    /// `list`: print the table of checks and run nothing.
    pub list: bool,
    pub help: bool,
    /// `-p`/`--package`/`--scope`, in the order given.
    pub packages: Vec<String>,
    pub all: bool,
    pub base: Option<String>,
    pub app: Option<PathBuf>,
    pub json: bool,
    /// `--fix`: `fmt` rewrites what it finds.
    pub fix: bool,
    /// `--cell`: the forge cells `cells` checks, in the order given; empty is
    /// every cell no fleet machine hosts.
    pub cells: Vec<String>,
    pub no_windows: bool,
    pub test_threads: Option<NonZeroU32>,
    pub test_jobs: Option<NonZeroU32>,
    /// `--timeout`: `Some(None)` is `off`; `None` keeps the default ceiling.
    pub timeout: Option<Option<Duration>>,
    pub log: Option<PathBuf>,
}

impl Args {
    /// Does any selected check read the selection (the cone, `-p`, `--all`)?
    #[must_use]
    pub fn narrows(&self) -> bool {
        self.checks.iter().any(|c| c.narrows())
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
/// or options that contradict each other.
pub fn parse<I, S>(args: I) -> Result<Args, Usage>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut a = Args::default();
    let mut words: Vec<Word> = Vec::new();
    let mut it = args.into_iter().map(Into::into);
    let usage = |s: String| Err(Usage::new(s));
    while let Some(arg) = it.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f.to_string(), Some(v.to_string())),
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
            "--json" => a.json = true,
            "--fix" => a.fix = true,
            "--cell" => a.cells.push(value("--cell")?),
            "--no-windows" => a.no_windows = true,
            "--test-threads" => a.test_threads = Some(count("--test-threads", &value(&flag)?)?),
            "--test-jobs" => a.test_jobs = Some(count("--test-jobs", &value(&flag)?)?),
            "--timeout" => {
                let v = value("--timeout")?;
                match crate::exec::parse_ceiling(&v) {
                    Some(t) => a.timeout = Some(t),
                    None => return usage(format!("--timeout takes seconds or `off`, not `{v}`")),
                }
            }
            "--log" => a.log = Some(PathBuf::from(value("--log")?)),
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
    let mut wanted: Vec<Check> = Vec::new();
    for w in &words {
        match w {
            Word::Check(c) => wanted.push(*c),
            Word::Dev => wanted.extend(DEV),
            Word::Release => wanted.extend(RELEASE),
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
    // The fix rewrites sources in place, so no other check may be reading
    // them: a compile beside it would judge half-written files.
    if a.fix && a.checks != [Check::Fmt] {
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
    if a.release {
        if !a.packages.is_empty() || a.base.is_some() || !a.cells.is_empty() {
            return usage(
                "`release` runs the whole workspace; drop -p, --base and --cell".to_string(),
            );
        }
        a.all = true;
        if a.app.is_none() {
            return usage(
                "`release` needs the bundle `measure` judges: --app <path to aterm.app>".into(),
            );
        }
    }
    Ok(a)
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
    format!(
        "targo --unverified verify [CHECK|SET ...] [OPTIONS]

Run aterm's checks in place, offline, alone or in any combination.

  verify                          the dev set on what changed since origin/main
  verify lint test -p aterm-grid  two checks on one crate
  verify test --all               every crate's tests
  verify fmt --fix                format every file the check finds
  verify release --app <APP>      everything publication requires
  verify list                     every check: sets, needs, what it runs
  verify perf [LANE...]           the performance lanes (`verify perf --list`)
  verify glue <in.wasm> <out> --flavor web|nodejs
                                  a wasm module's JS glue (`--record-toy`: the
                                  wasm-glue check's goldens; `--record-replay`:
                                  wasm-smoke's CPU replay answers)

Sets:
  dev      fmt guards census lint test doc (the default)
  release  every check `ship release` requires; --all implied, a skip is exit 3

Checks:
{checks}

Options:
  -p, --package <crate>   narrow lint/test/doc/trust to a crate (repeatable)
      --all               the whole workspace (default: the change cone)
      --base <ref>        the cone's base (default: local origin/main; no fetch)
      --app <APP>         the release bundle `measure` judges
      --fix               with fmt alone: rewrite the files it finds unformatted
      --cell <name>       cells checks this forge cell (repeatable; any cell)
      --json              one JSON object per check, then a summary (stdout)
      --no-windows        skip the checks that open windows
      --test-threads <n>  RUST_TEST_THREADS for every test binary
      --test-jobs <n>     test binaries at once (default 8, fewer under load)
      --timeout <s|off>   hang ceiling for each child (default 3h)
      --log <path>        where the full log goes (default <target>/verify/logs)
  -h, --help

Exit: 0 every check that ran passed, 1 a check failed, 2 usage, 3 could not run.
A skip prints its reason and fix and fails nothing, except under `release`.
"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(args: &[&str]) -> Args {
        parse(args.iter().copied()).unwrap_or_else(|e| panic!("{args:?}: {}", e.message()))
    }

    fn err(args: &[&str]) -> String {
        parse(args.iter().copied())
            .expect_err("a usage error")
            .message()
    }

    #[test]
    fn the_bare_command_is_the_dev_set_on_the_cone() {
        let a = ok(&[]);
        assert_eq!(a.checks, DEV);
        assert!(!a.all && a.packages.is_empty() && !a.release && a.narrows());
        assert_eq!(ok(&["dev"]).checks, DEV);
    }

    #[test]
    fn checks_and_sets_combine_in_one_order_without_repeats() {
        assert_eq!(
            ok(&["test", "lint", "test", "fmt"]).checks,
            [Check::Fmt, Check::Lint, Check::Test]
        );
        let a = ok(&["dev", "forge"]);
        assert_eq!(a.checks.len(), DEV.len() + 1);
        assert!(a.checks.contains(&Check::Forge));
        assert!(!ok(&["fmt", "guards"]).narrows());
    }

    #[test]
    fn packages_repeat_and_scope_is_a_synonym() {
        let a = ok(&["lint", "-p", "a", "--package", "b", "--scope=c", "-pd"]);
        assert_eq!(a.packages, ["a", "b", "c", "d"]);
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
        assert!(err(&["release"]).contains("--app"));
        assert!(err(&["release", "--app", "a", "-p", "x"]).contains("whole workspace"));
        assert!(err(&["release", "--app", "a", "--cell", "win"]).contains("whole workspace"));
    }

    #[test]
    fn contradictions_and_unknowns_are_usage_errors_naming_the_word() {
        assert!(err(&["-p", "x", "--all"]).contains("contradict"));
        assert!(err(&["--base", "main", "--all"]).contains("--base"));
        assert!(err(&["lnit"]).contains("unknown check `lnit`"));
        assert!(err(&["--full"]).contains("unknown flag `--full`"));
        assert!(err(&["-p"]).contains("-p needs a value"));
        assert!(err(&["-p", "--all"]).contains("-p needs a value, not `--all`"));
        assert_eq!(
            err(&["lnit"]),
            "verify: unknown check `lnit` (verify list names every check)"
        );
        assert_eq!(
            err(&["--fast"]),
            "verify: unknown flag `--fast` (verify --help)"
        );
        assert!(err(&["--test-jobs", "0"]).contains("positive"));
        assert!(err(&["--timeout", "45m"]).contains("--timeout"));
        assert!(err(&["list", "fmt"]).contains("list"));
        for beside in [
            &["lint", "--fix"][..],
            &["--fix"],
            &["fmt", "lint", "--fix"],
        ] {
            assert!(err(beside).contains("run it alone"), "{beside:?}");
        }
        assert!(err(&["release", "--app", "a", "--fix"]).contains("run it alone"));
        assert!(ok(&["fmt", "--fix"]).fix);
        assert_eq!(
            ok(&["cells", "--cell", "win", "--cell=linux"]).cells,
            ["win", "linux"]
        );
        assert!(err(&["lint", "--cell", "win"]).contains("name cells"));
        assert!(err(&["cells", "--cell", "plan9"]).contains("unknown cell `plan9`"));
    }

    #[test]
    fn a_selection_no_check_reads_is_said_to_be_ignored() {
        let note = |args: &[&str]| ignored_selection(&ok(args));
        assert_eq!(
            note(&["fmt", "-p", "aterm-grid"]).as_deref(),
            Some("fmt reads the whole tree, so -p is ignored")
        );
        assert_eq!(
            note(&["fmt", "guards", "--base", "main"]).as_deref(),
            Some("fmt guards read the whole tree, so --base is ignored")
        );
        assert_eq!(note(&["fmt", "lint", "-p", "aterm-grid"]), None);
        assert_eq!(note(&["fmt"]), None);
    }

    #[test]
    fn values_parse_into_their_types() {
        let a = ok(&[
            "test",
            "--test-threads=3",
            "--test-jobs",
            "2",
            "--timeout",
            "off",
            "--log",
            "/tmp/x.log",
            "--json",
            "--no-windows",
            "--base",
            "main",
        ]);
        assert_eq!(a.test_threads.map(NonZeroU32::get), Some(3));
        assert_eq!(a.test_jobs.map(NonZeroU32::get), Some(2));
        assert_eq!(a.timeout, Some(None));
        assert_eq!(a.log.as_deref(), Some(std::path::Path::new("/tmp/x.log")));
        assert!(a.json && a.no_windows);
        assert_eq!(a.base.as_deref(), Some("main"));
        assert_eq!(
            ok(&["--timeout", "90"]).timeout,
            Some(Some(Duration::from_secs(90)))
        );
    }

    #[test]
    fn list_and_help_stand_alone() {
        assert!(ok(&["list"]).list);
        assert!(ok(&["-h"]).help);
        assert!(ok(&["release", "--help"]).help, "help never needs --app");
        let u = usage();
        for s in &CHECKS {
            assert!(u.contains(s.name), "{}", s.name);
        }
    }
}
