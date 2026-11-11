// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-libc-oracle` gate ([`siblings`], [`shape`], [`symbols`], [`controls`]) and authoring
//! verbs ([`regen`], [`harvest`]).
//!
//! STOCK EXCEPTION: authoring cells absent from the selected Trust sysroot use
//! a stock rustup toolchain with those targets' std (`--stock`, by default the
//! checkout's stock pin, `aterm_toolchain::rustup::stock_pin`, whose stds
//! `targo --unverified verify provision rustup` places). A Trust target with
//! actual std/core stays under Trust.

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

/// Default build directory for the oracle workspace (`*.noindex` so Spotlight skips it).
const ORACLE_TARGET: &str = "libc-oracle/target.noindex";

/// Maximum size (256 MiB) accepted when reading source, union, log, or rustdoc JSON files.
pub(crate) const MAX_ORACLE_FILE_BYTES: u64 = 256 << 20;

pub(crate) fn read_regular_file(path: &Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read as _;
    let (file, meta) = aterm_uds::regular_file::open(path)?;
    if meta.len() > MAX_ORACLE_FILE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "file exceeds 256 MiB limit",
        ));
    }
    let cap = usize::try_from(meta.len().min(MAX_ORACLE_FILE_BYTES).saturating_add(1)).unwrap_or(0);
    let mut buf = Vec::with_capacity(cap);
    file.take(MAX_ORACLE_FILE_BYTES + 1).read_to_end(&mut buf)?;
    if buf.len() as u64 > MAX_ORACLE_FILE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "file grew past 256 MiB limit while reading",
        ));
    }
    Ok(buf)
}

pub(crate) fn read_regular_text(path: &Path) -> std::io::Result<String> {
    String::from_utf8(read_regular_file(path)?)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

pub(crate) fn write_fresh(path: &Path, bytes: impl AsRef<[u8]>) -> std::io::Result<()> {
    let _ = std::fs::remove_file(path);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS);
    }
    opts.open(path)?.write_all(bytes.as_ref())
}

const USAGE: &str = "\
aterm-libc-oracle — check crates/aterm-libc against reference libc 0.2.186 on the native ABI cell
(`targo --unverified verify libc` runs this gate)

usage: targo --unverified run -q -p aterm-libc-oracle -- [--root <checkout>] [--target-dir <dir>]
       targo --unverified run -q -p aterm-libc-oracle -- regen [--root <checkout>] [--out <dir>]
           [--cell <key>] [--work <dir>] [--docs <dir>] [--stock <toolchain>] [--no-format]
       targo --unverified run -q -p aterm-libc-oracle -- harvest --cell <key> [--root <checkout>]
           [--work <dir>] [--docs <dir>] [--stock <toolchain>] [--write] <log>

  --root <checkout>   aterm checkout (default: the nearest one at or above cwd)
  --target-dir <dir>  gate build dir (default: <checkout>/libc-oracle/target.noindex)
  -h, --help          print this help

regen: rewrite all four cells by default (or only --cell), crates/aterm-libc/src/{darwin,linux_gnu}_<arch>.rs and
libc-oracle/conformance/src/cell_<key>.rs, from reference libc rustdoc and const-eval probes,
then trustfmt and seal them. Cells with actual std/core in the selected Trust sysroot use
Trust; remaining cells use stock rustup with target std installed (STOCK EXCEPTION).
  --out <dir>         write into this checkout-shaped directory instead of --root
  --cell <key>        regenerate only mac, macx, linux or linuxa (native cell needs no stock)
  --work <dir>        scratch dir (default: <checkout>/libc-oracle/target.noindex/regen)
  --docs <dir>        use prebuilt libc-0.2.186-<triple>.json rustdoc dumps from <dir>
  --stock <toolchain> rustup toolchain for non-host cells (default: the checkout's stock pin)
  --no-format         skip trustfmt on emitted files

harvest: parse a `--message-format short` check log for `--cell <key>` (mac, macx, linux,
linuxa), list missing symbols exported by reference libc on that cell, and with `--write` add
them to libc-oracle/union.json.

Exit: 0 pass, 1 finding (even if the run then stopped), 2 usage, 3 could not run.";

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
            Some("--cell") if harvest || regen => {
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

/// The authoring verbs' options: scratch under the oracle's own target lane, and the checkout's
/// stock pin for the cross cells unless `--stock` names another toolchain.
///
/// # Errors
/// No `--stock`, and the checkout's engine gate disagrees with its stock pin
/// (`aterm_toolchain::rustup::stock_pin`).
fn regen_options(root: PathBuf, args: &Args) -> Result<regen::Options, Stop> {
    let stock = match &args.stock {
        Some(named) => named.clone(),
        None => aterm_toolchain::rustup::stock_pin(&root).map_err(|refused| {
            Stop::CouldNotRun(verdict::CouldNotRun::new(refused.why, refused.remedy))
        })?,
    };
    Ok(regen::Options {
        out: args.out.clone().map_or_else(|| root.clone(), absolute),
        work: args
            .work
            .clone()
            .map_or_else(|| root.join(ORACLE_TARGET).join("regen"), absolute),
        docs: args.docs.clone().map(absolute),
        stock,
        cell: args.cell.clone(),
        format: !args.no_format,
        root,
    })
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
    let options = match regen_options(root, &args) {
        Ok(options) => options,
        Err(stop) => return stopped(stop),
    };
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
    let log = match read_regular_file(&log_path) {
        Ok(bytes) => String::from_utf8(bytes)
            .unwrap_or_else(|e| String::from_utf8_lossy(&e.into_bytes()).into_owned()),
        Err(error) => {
            eprintln!("aterm-libc-oracle: {}: {error}", log_path.display());
            return ExitCode::from(EXIT_USAGE);
        }
    };
    regen_options(root, &args)
        .and_then(|options| report_harvest(&options, cell, &log, args.write))
        .map_or_else(stopped, |()| ExitCode::SUCCESS)
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
            "added {} names to {}; now `targo --unverified run -q -p aterm-libc-oracle -- regen`",
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
        None => root.join(ORACLE_TARGET),
    };
    let options = Options { root, target_dir };
    let report = run::run(&options, &mut Progress);
    print_text(&report);
    ExitCode::from(report.exit_code())
}

#[cfg(test)]
#[path = "main_tests.rs"]
mod tests;
