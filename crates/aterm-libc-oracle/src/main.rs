// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-libc-oracle` — decides `crates/aterm-libc` against the pinned
//! reference libc on the ABI cell this box is native to.
//!
//! `libc-oracle/` is a separate workspace with no `[patch]` table, so its
//! `libc = "=0.2.186"` reaches the registry crate while `under-test/` names the
//! shim's own sources; inside this workspace the patch would make the oracle
//! compare the shim with itself. One run, all under the pinned Trust toolchain:
//!
//! 1. inputs — `Cargo.lock` current, locked dependencies offline, `union.json`;
//! 2. siblings — each same-OS pair's hand-written bodies identical ([`siblings`]);
//! 3. conformance — the cell's generated assertions compile and its runtime
//!    tests pass (`targo test -p libc-conformance`);
//! 4. shape and symbols — fresh rustdoc of both crates: the shim's public
//!    shape and every extern's linker symbol against the reference's
//!    ([`shape`], [`symbols`]);
//! 5. controls — planted defects the oracle must catch ([`controls`]).
//!
//! A finding outranks a stop: a run the machine cannot finish still exits 1
//! when it found a defect before stopping.
//!
//! Two authoring verbs share its modules, so the generator and the gate read the
//! reference, the cell table and the declaration set through ONE implementation:
//! `regen` ([`regen`]) rewrites the shim's cell modules and the oracle's
//! conformance modules, and `harvest` ([`harvest`]) widens `union.json` from a
//! workspace check's errors.

mod cargo;
mod cells;
mod closure;
mod controls;
mod harvest;
mod regen;
mod run;
mod rustdoc;
mod shape;
mod siblings;
mod symbols;
mod trust;
mod union;
mod verdict;

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::cells::Cell;
use crate::harvest::Harvest;
use crate::regen::Stop;
use crate::run::{Observer, Options};
use crate::union::Union;
use crate::verdict::{EXIT_USAGE, PhaseReport, Report};

const USAGE: &str = "\
aterm-libc-oracle — decide crates/aterm-libc against the pinned reference libc
on the ABI cell this box is native to, and regenerate it

usage: aterm-libc-oracle [--root <checkout>] [--target-dir <dir>]
       aterm-libc-oracle regen [--root <checkout>] [--out <dir>] [--work <dir>]
                               [--docs <dir>] [--stock <toolchain>] [--no-format]
       aterm-libc-oracle harvest --cell <key> [--root <checkout>] [--work <dir>]
                                 [--docs <dir>] [--stock <toolchain>] [--write] <log>

  --root <checkout>   the aterm checkout (default: the one containing the
                      working directory)
  --target-dir <dir>  the oracle workspace's builds (default:
                      <checkout>/libc-oracle/target); one per checkout,
                      never shared with another
  -h, --help          this text

regen: rewrite crates/aterm-libc/src/<cell>.rs and
libc-oracle/conformance/src/cell_<cell>.rs for all four cells from the
reference libc's rustdoc and const-eval probes, then trustfmt them. The cell
this box is native to is compiled under Trust; the others under a stock rustup
toolchain with their std installed (STOCK EXCEPTION: Trust ships std for its
host only).
  --out <dir>         write under this checkout-shaped directory instead
  --work <dir>        scratch (default: <checkout>/libc-oracle/target/regen)
  --docs <dir>        libc-0.2.186-<triple>.json dumps made elsewhere, used
                      for the cells they cover
  --stock <toolchain> the rustup toolchain for the other cells (default: stable)
  --no-format         leave the output as emitted

harvest: read a `--message-format short` workspace check log built for one
cell; print the names it says libc lacks that the reference exports there,
and with --write add them to libc-oracle/union.json (then regen).

exit: 0 pass · 1 finding (even if the run then stopped) · 2 usage ·
      3 could not run, nothing found (the reason names its fix)";

/// What a run does.
#[derive(Debug, Default, PartialEq, Eq)]
enum Verb {
    /// Decide the native cell.
    #[default]
    Gate,
    Regen,
    Harvest,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Args {
    verb: Verb,
    root: Option<PathBuf>,
    target_dir: Option<PathBuf>,
    help: bool,
    out: Option<PathBuf>,
    work: Option<PathBuf>,
    docs: Option<PathBuf>,
    stock: Option<String>,
    no_format: bool,
    cell: Option<String>,
    write: bool,
    log: Option<PathBuf>,
}

fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Args, String> {
    let mut out = Args::default();
    let mut args = args.into_iter().peekable();
    match args.peek().and_then(|a| a.to_str()) {
        Some("regen") => out.verb = Verb::Regen,
        Some("harvest") => out.verb = Verb::Harvest,
        _ => {}
    }
    if out.verb != Verb::Gate {
        args.next();
    }
    while let Some(arg) = args.next() {
        let mut value = |flag: &str| args.next().ok_or_else(|| format!("{flag} needs a value"));
        let (gate, regen, harvest) = (
            out.verb == Verb::Gate,
            out.verb == Verb::Regen,
            out.verb == Verb::Harvest,
        );
        match arg.to_str() {
            Some("--root") => out.root = Some(value("--root")?.into()),
            Some("--target-dir") if gate => out.target_dir = Some(value("--target-dir")?.into()),
            Some("--out") if regen => out.out = Some(value("--out")?.into()),
            Some("--no-format") if regen => out.no_format = true,
            Some("--work") if !gate => out.work = Some(value("--work")?.into()),
            Some("--docs") if !gate => out.docs = Some(value("--docs")?.into()),
            Some("--stock") if !gate => {
                out.stock = Some(value("--stock")?.to_string_lossy().into_owned());
            }
            Some("--cell") if harvest => {
                out.cell = Some(value("--cell")?.to_string_lossy().into_owned());
            }
            Some("--write") if harvest => out.write = true,
            Some("-h" | "--help") => out.help = true,
            Some(log) if harvest && !log.starts_with('-') && out.log.is_none() => {
                out.log = Some(log.into());
            }
            _ => return Err(format!("unknown argument {}", arg.to_string_lossy())),
        }
    }
    if out.verb == Verb::Harvest && !out.help && (out.cell.is_none() || out.log.is_none()) {
        return Err("harvest needs --cell <key> and a log".into());
    }
    Ok(out)
}

/// The nearest ancestor of `start` holding the oracle and the shim.
fn find_root(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|dir| {
            dir.join("libc-oracle/Cargo.toml").is_file()
                && dir.join("crates/aterm-libc/src/lib.rs").is_file()
        })
        .map(Path::to_path_buf)
}

/// Prints each phase as it finishes.
struct Progress;

impl Observer for Progress {
    fn deciding(&mut self, host: &str, cell: &Cell) {
        println!("libc oracle: {host} (cell {}), under Trust", cell.key);
    }

    fn phase(&mut self, report: &PhaseReport) {
        println!("{report}");
        let _ = std::io::stdout().flush();
    }
}

fn print_text(report: &Report) {
    if let Some(host) = &report.host {
        let others: Vec<&str> = cells::CELLS
            .iter()
            .map(|cell| cell.triple)
            .filter(|triple| triple != host)
            .collect();
        if report.cell.is_some() {
            println!(
                "  other cells, decided only on a box native to each: {}",
                others.join(", ")
            );
        }
    }
    if !report.findings.is_empty() {
        println!("findings:");
        for finding in &report.findings {
            println!("  - {finding}");
        }
    }
    if let Some(reason) = &report.could_not_run {
        let stopped = if report.findings.is_empty() {
            "COULD NOT RUN"
        } else {
            "the run stopped early"
        };
        println!("{stopped}: {reason}");
    }
    match report.exit_code() {
        verdict::EXIT_PASS => println!("PASS"),
        verdict::EXIT_FINDING => println!("FAIL ({} findings)", report.findings.len()),
        _ => {}
    }
}

fn absolute(path: PathBuf) -> PathBuf {
    std::path::absolute(&path).unwrap_or(path)
}

/// The authoring verbs' options: scratch under the oracle's own target lane.
fn regen_options(root: PathBuf, args: &Args) -> regen::Options {
    regen::Options {
        out: args.out.clone().map_or_else(|| root.clone(), absolute),
        work: args
            .work
            .clone()
            .map_or_else(|| root.join("libc-oracle/target/regen"), absolute),
        docs: args.docs.clone().map(absolute),
        stock: args.stock.clone().unwrap_or_else(|| "stable".into()),
        format: !args.no_format,
        root,
    }
}

fn stopped(stop: Stop) -> ExitCode {
    match stop {
        Stop::CouldNotRun(could_not_run) => {
            eprintln!("COULD NOT RUN: {could_not_run}");
            ExitCode::from(verdict::EXIT_COULD_NOT_RUN)
        }
        Stop::Failed(reason) => {
            eprintln!("FAIL: {reason}");
            ExitCode::from(verdict::EXIT_FINDING)
        }
    }
}

fn regen_verb(root: PathBuf, args: Args) -> ExitCode {
    let options = regen_options(root, &args);
    match regen::regen(&options, &mut std::io::stdout()) {
        Ok(written) => {
            let formatted = if options.format { " and formatted" } else { "" };
            println!(
                "regen: wrote{formatted} {} modules; a `git diff` now is a real divergence",
                written.len()
            );
            ExitCode::SUCCESS
        }
        Err(stop) => stopped(stop),
    }
}

fn harvest_verb(root: PathBuf, args: Args) -> ExitCode {
    let Some(cell) = cells::CELLS
        .iter()
        .find(|c| Some(c.key) == args.cell.as_deref())
    else {
        let keys: Vec<&str> = cells::CELLS.iter().map(|c| c.key).collect();
        eprintln!("aterm-libc-oracle: --cell is one of {}", keys.join(", "));
        return ExitCode::from(EXIT_USAGE);
    };
    let log_path = args.log.clone().unwrap_or_default();
    let log = match std::fs::read(&log_path) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(error) => {
            eprintln!("aterm-libc-oracle: {}: {error}", log_path.display());
            return ExitCode::from(EXIT_USAGE);
        }
    };
    let options = regen_options(root, &args);
    report_harvest(&options, cell, &log, args.write).map_or_else(stopped, |()| ExitCode::SUCCESS)
}

/// Report what `log` says `cell` lacks; with `write`, add it to the list.
fn report_harvest(
    options: &regen::Options,
    cell: &Cell,
    log: &str,
    write: bool,
) -> Result<(), Stop> {
    let doc = regen::reference_for(options, cell)?;
    let public = doc.public().map_err(|e| Stop::Failed(e.0))?;
    let harvest = Harvest::scan(log, &|name| public.contains_key(name));
    println!("{}", harvest.report(cell.key));
    let mut union = Union::load(&options.root)?;
    let listed: BTreeSet<String> = union.names(cell)?.into_iter().collect();
    let new: Vec<String> = harvest.names().difference(&listed).cloned().collect();
    println!(
        "NOT YET IN {} ({}): {}",
        union::PATH,
        new.len(),
        new.join(" ")
    );
    if write && !new.is_empty() {
        let added = union.widen(cell, new);
        union.store(&options.root)?;
        println!(
            "added {} names to {}; now `aterm-libc-oracle regen`",
            added.len(),
            union::PATH
        );
    }
    Ok(())
}

fn main() -> ExitCode {
    let args = match parse(std::env::args_os().skip(1)) {
        Ok(args) => args,
        Err(error) => {
            eprintln!("aterm-libc-oracle: {error}\n\n{USAGE}");
            return ExitCode::from(EXIT_USAGE);
        }
    };
    if args.help {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    let root = match args.root.clone() {
        Some(root) => absolute(root),
        None => find_root(&cwd).unwrap_or(cwd),
    };
    match args.verb {
        Verb::Gate => {}
        Verb::Regen => return regen_verb(root, args),
        Verb::Harvest => return harvest_verb(root, args),
    }
    let target_dir = match args.target_dir {
        Some(dir) => absolute(dir),
        None => root.join("libc-oracle/target"),
    };
    let options = Options { root, target_dir };
    let report = run::run(&options, &mut Progress);
    print_text(&report);
    ExitCode::from(report.exit_code())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(words: &[&str]) -> Result<Args, String> {
        parse(words.iter().map(OsString::from))
    }

    #[test]
    fn each_verb_takes_only_its_own_flags() {
        assert!(
            args(&["--stock", "stable"]).is_err(),
            "an authoring flag is not the gate's"
        );
        assert_eq!(
            args(&[
                "regen",
                "--out",
                "/o",
                "--docs",
                "d",
                "--stock",
                "1.98.1",
                "--no-format"
            ]),
            Ok(Args {
                verb: Verb::Regen,
                out: Some("/o".into()),
                docs: Some("d".into()),
                stock: Some("1.98.1".into()),
                no_format: true,
                ..Args::default()
            })
        );
        assert!(args(&["regen", "--target-dir", "t"]).is_err());
        assert!(args(&["regen", "log.txt"]).is_err());
        assert_eq!(
            args(&["harvest", "--cell", "linux", "--write", "check.log"]),
            Ok(Args {
                verb: Verb::Harvest,
                cell: Some("linux".into()),
                write: true,
                log: Some("check.log".into()),
                ..Args::default()
            })
        );
        assert!(args(&["harvest", "check.log"]).is_err(), "no cell");
        assert!(args(&["harvest", "--cell", "mac"]).is_err(), "no log");
        assert!(args(&["harvest", "--cell", "mac", "a.log", "b.log"]).is_err());
        assert!(args(&["harvest", "--out", "/o", "--cell", "mac", "a.log"]).is_err());
    }
}
