// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-libc-oracle regen` — regenerate `crates/aterm-libc`'s four cell
//! modules and the oracle's four conformance modules from measurements of the
//! REFERENCE libc, then format them.
//!
//! Every value written is a fact about one platform's ABI, and a wrong one does
//! not fail to compile: it passes the wrong flag to a syscall. So nothing is
//! transcribed. Two ground truths, per cell:
//!
//! 1. the reference's rustdoc JSON for that triple ([`decls`]): constant values,
//!    signatures, `link_name`s, field names and types, discriminants;
//! 2. const-eval probes compiled for that triple ([`probe`]): sizes,
//!    alignments, public-field offsets and the bytes of struct-valued
//!    initialisers, measured even for a triple this box cannot execute.
//!
//! [`layout`] reproduces `repr(C)` and is held to every measured offset before
//! it places the padding standing in for libc's private fields; [`shim`] writes
//! the shim's cell modules and [`oracle`] the conformance modules, both from ONE
//! declaration set ([`crate::closure`], the function the gate's shape phase also
//! uses), and the oracle's coverage audit fails the run on any declaration the
//! oracle does not check.
//!
//! The cell this box is native to is documented and probed under Trust; every
//! other cell under a stock rustup toolchain (`--stock`, default `stable`, with
//! each cross triple's std installed) — this crate's one STOCK EXCEPTION, since
//! Trust ships std for its host only. Rustdoc dumps made elsewhere (on a box native
//! to their cell) are used instead when `--docs` holds them, named
//! `libc-0.2.186-<triple>.json`.
//!
//! The written files are formatted with `trustfmt`, so regeneration is
//! idempotent and a `git diff` after it is a real divergence.
//!
//! # Regenerating
//!
//! ```sh
//! targo --unverified run -q -p aterm-libc-oracle -- regen
//! git diff crates/aterm-libc libc-oracle/conformance
//! ```
//!
//! It needs the pinned Trust toolchain, the stock toolchain with the three
//! triples this box is not native to (`rustup target add --toolchain stable
//! x86_64-apple-darwin x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu`,
//! minus the host), and `libc 0.2.186` in the registry cache: every build is
//! `--offline`, so a run without it exits 3 naming the one fetch to make
//! (`targo fetch --manifest-path <work>/reference/Cargo.toml`). The scratch
//! `<work>` is `libc-oracle/target/regen` unless `--work` names another.
//! To document a cell on the box native to it instead, run `targo --unverified
//! rustdoc -q -p libc --manifest-path <work>/reference/Cargo.toml --
//! -Zunstable-options --output-format json` there and pass the resulting
//! `target/doc/libc.json`, renamed, through `--docs`.
//!
//! Every cell is generated before any module is written, so a run that stops
//! leaves the tree as it was.
//!
//! The item list, `libc-oracle/union.json`, only ever grows, and it is widened
//! from the compiler rather than from a source scan: check the workspace for
//! the cell with `--message-format short`, feed the log to `aterm-libc-oracle
//! harvest --cell <key> --write <log>` ([`crate::harvest`]), regenerate, and
//! repeat to a fixed point.

pub mod behaviour;
pub mod decls;
pub mod handwritten;
pub mod layout;
pub mod oracle;
pub mod probe;
pub mod shim;

use std::io::Write;
use std::path::{Path, PathBuf};

use aterm_uds::spawnfd::Command;

use crate::cargo::{AMBIENT, LockedFailure, Outcome, tail};
use crate::cells::{CELLS, Cell};
use crate::rustdoc::Doc;
use crate::trust::{self, Stock, Trust};
use crate::union::Union;
use crate::verdict::CouldNotRun;

/// The reference libc's version, in the rustdoc dumps' names.
pub const REFERENCE_VERSION: &str = "0.2.186";

/// Where and how to regenerate.
#[derive(Clone, Debug)]
pub struct Options {
    /// The checkout: it holds `libc-oracle/union.json`.
    pub root: PathBuf,
    /// Where the eight modules are written, as a checkout (`<out>/crates/…`).
    pub out: PathBuf,
    /// Scratch: the reference crate, its probes and its builds.
    pub work: PathBuf,
    /// Rustdoc dumps made elsewhere, used for the cells they cover.
    pub docs: Option<PathBuf>,
    /// The rustup toolchain for cells this box is not native to.
    pub stock: String,
    /// Run `trustfmt` over what was written.
    pub format: bool,
}

/// Why a regeneration stopped.
#[derive(Debug)]
pub enum Stop {
    /// A prerequisite is missing; nothing about the tree was decided.
    CouldNotRun(CouldNotRun),
    /// The reference, the measurements or the audit refused the result.
    Failed(String),
}

impl From<String> for Stop {
    fn from(message: String) -> Self {
        Self::Failed(message)
    }
}

impl From<CouldNotRun> for Stop {
    fn from(could_not_run: CouldNotRun) -> Self {
        Self::CouldNotRun(could_not_run)
    }
}

fn io(what: &str, path: &Path) -> impl FnOnce(std::io::Error) -> Stop {
    let what = format!("{what} {}", path.display());
    move |e| {
        Stop::CouldNotRun(CouldNotRun::new(
            format!("{what}: {e}"),
            "free disk space and rerun",
        ))
    }
}

/// The compiler for one cell: Trust where this box is native, else stock.
pub enum Compiler<'t> {
    Trust(&'t Trust),
    Stock(&'t Stock),
}

impl Compiler<'_> {
    /// A cargo command over the crate at `dir`, building into `target`, for
    /// `cell`'s triple: `words` before the separator, `after` past it.
    fn cargo(
        &self,
        cell: &Cell,
        dir: &Path,
        target: &Path,
        words: &[&str],
        after: &[&str],
    ) -> Command {
        let mut command = match self {
            Self::Trust(trust) => {
                // From the crate, so the checkout's config applies; no
                // `--target`, which would withhold it from host units.
                let mut command = aterm_uds::spawnfd::command(&trust.targo);
                command
                    .arg("--unverified")
                    .args(words)
                    .current_dir(dir)
                    .env("RUSTC", &trust.trustc)
                    .env("RUSTDOC", &trust.trustdoc);
                command
            }
            Self::Stock(stock) => {
                // From `/`, so no checkout config (its `-Z` tables) reaches a
                // stock compiler; rustdoc's JSON is unstable there.
                let mut command = aterm_uds::spawnfd::command(&stock.cargo);
                command
                    .args(words)
                    .args(["--target", cell.triple, "--manifest-path"])
                    .arg(dir.join("Cargo.toml"))
                    .current_dir("/")
                    .env("RUSTC", &stock.rustc)
                    .env("RUSTDOC", &stock.rustdoc)
                    .env("RUSTC_BOOTSTRAP", "1");
                command
            }
        };
        command.arg("--offline").arg("--target-dir").arg(target);
        if !after.is_empty() {
            command.arg("--").args(after);
        }
        for name in AMBIENT {
            command.env_remove(name);
        }
        command
    }

    /// Where `rustdoc -p libc` leaves its JSON.
    fn doc_json(&self, cell: &Cell, target: &Path) -> PathBuf {
        match self {
            Self::Trust(_) => target.join("doc/libc.json"),
            Self::Stock(_) => target.join(cell.triple).join("doc/libc.json"),
        }
    }

    fn name(&self) -> String {
        match self {
            Self::Trust(_) => "Trust".into(),
            Self::Stock(stock) => format!("stock `{}` (STOCK EXCEPTION)", stock.name),
        }
    }

    /// What to do when this compiler itself fails.
    fn repair(&self) -> String {
        match self {
            Self::Trust(_) => "`aterm pkg doctor`, then `aterm pkg repair`".into(),
            Self::Stock(stock) => format!("`rustup update {}`", stock.name),
        }
    }
}

/// The scratch reference crate: the libc the shim replaces, documented for
/// its declarations and probed for its ABI.
struct Reference {
    dir: PathBuf,
    target: PathBuf,
}

impl Reference {
    fn scaffold(work: &Path) -> Result<Self, Stop> {
        let dir = work.join("reference");
        let src = dir.join("src");
        std::fs::create_dir_all(&src).map_err(io("creating", &src))?;
        let write =
            |path: PathBuf, text: &str| std::fs::write(&path, text).map_err(io("writing", &path));
        write(dir.join("Cargo.toml"), probe::MANIFEST)?;
        write(src.join("lib.rs"), &probe::lib_rs(&CELLS))?;
        for cell in &CELLS {
            write(src.join(format!("{}.rs", probe::module(cell))), "")?;
        }
        Ok(Self {
            dir,
            target: work.join("target"),
        })
    }

    /// Run one offline cargo command for `cell` and read what it reported. A
    /// full disk or a reference not yet downloaded is a stop that says nothing
    /// about the cell.
    fn cargo(
        &self,
        compiler: &Compiler<'_>,
        cell: &Cell,
        words: &[&str],
        after: &[&str],
        what: &str,
    ) -> Result<Outcome, Stop> {
        let mut command = compiler.cargo(cell, &self.dir, &self.target, words, after);
        let out = command.output().map_err(|e| {
            Stop::CouldNotRun(CouldNotRun::new(
                format!("{what} could not start: {e}"),
                compiler.repair(),
            ))
        })?;
        let outcome = Outcome::parse(
            out.status.success(),
            out.status.code(),
            &String::from_utf8_lossy(&out.stdout),
            &String::from_utf8_lossy(&out.stderr),
        );
        if outcome.out_of_space() {
            return Err(Stop::CouldNotRun(CouldNotRun::new(
                format!(
                    "the disk filled during {what}:\n{}",
                    tail(&outcome.stderr, 5)
                ),
                "free disk space and rerun",
            )));
        }
        if outcome.locked_failure() == Some(LockedFailure::NotDownloaded) {
            return Err(Stop::CouldNotRun(CouldNotRun::new(
                format!(
                    "libc {REFERENCE_VERSION} is not in the registry cache ({what} is offline):\n{}",
                    tail(&outcome.stderr, 6)
                ),
                format!(
                    "fetch it once online: targo fetch --manifest-path {}",
                    self.dir.join("Cargo.toml").display()
                ),
            )));
        }
        Ok(outcome)
    }

    /// The reference's rustdoc JSON for `cell`.
    fn document(&self, compiler: &Compiler<'_>, cell: &Cell) -> Result<String, Stop> {
        let words = ["rustdoc", "-q", "-p", "libc"];
        let after = ["-Zunstable-options", "--output-format", "json"];
        let outcome = self.cargo(compiler, cell, &words, &after, "rustdoc")?;
        if !outcome.success {
            return Err(Stop::CouldNotRun(CouldNotRun::new(
                format!(
                    "documenting libc {REFERENCE_VERSION} for {} under {} failed:\n{}",
                    cell.triple,
                    compiler.name(),
                    outcome.detail()
                ),
                compiler.repair(),
            )));
        }
        let path = compiler.doc_json(cell, &self.target);
        std::fs::read_to_string(&path).map_err(io("reading", &path))
    }

    /// Compile `probe` for `cell` and read each fact off the compiler's errors.
    fn measure(
        &self,
        compiler: &Compiler<'_>,
        cell: &Cell,
        probe: &probe::Probe,
    ) -> Result<probe::Measured, Stop> {
        let module = probe::module(cell);
        let file = self.dir.join("src").join(format!("{module}.rs"));
        std::fs::write(&file, probe.source()).map_err(io("writing", &file))?;
        let words = ["check", "--message-format", "json"];
        let outcome = self.cargo(compiler, cell, &words, &[], "the probe")?;
        let reported = probe::reported(&outcome.errors, &format!("{module}.rs"));
        let facts = probe.read(&reported).map_err(|missing| {
            let shown: Vec<String> = missing.iter().take(20).map(ToString::to_string).collect();
            Stop::Failed(format!(
                "{}: {} facts unmeasured under {} ({}):\n{}",
                cell.key,
                missing.len(),
                compiler.name(),
                shown.join(", "),
                probe::unmeasured_because(&outcome)
            ))
        })?;
        let mut measured = probe::Measured::default();
        measured.record(facts)?;
        Ok(measured)
    }
}

/// The dump `--docs` holds for `cell`, if any.
fn supplied_doc(docs: Option<&Path>, cell: &Cell) -> Option<PathBuf> {
    let path = docs?.join(format!("libc-{REFERENCE_VERSION}-{}.json", cell.triple));
    path.is_file().then_some(path)
}

/// The toolchains a run needs: Trust always (the host cell, the formatter),
/// the stock one only when a cell is not native here.
struct Toolchains {
    trust: Trust,
    stock: Option<Stock>,
}

impl Toolchains {
    /// Trust, and the stock toolchain when one of `cells` needs it.
    fn find<'c>(
        root: &Path,
        stock: &str,
        cells: impl IntoIterator<Item = &'c Cell>,
    ) -> Result<Self, Stop> {
        let trust = trust::discover(root)?;
        let foreign: Vec<&Cell> = cells
            .into_iter()
            .filter(|c| c.triple != trust.host)
            .collect();
        let stock = if foreign.is_empty() {
            None
        } else {
            let found = trust::stock(stock, &trust.host)?;
            let missing: Vec<&str> = foreign
                .iter()
                .map(|c| c.triple)
                .filter(|t| !found.has_target(t))
                .collect();
            if !missing.is_empty() {
                return Err(Stop::CouldNotRun(CouldNotRun::new(
                    format!(
                        "the stock `{stock}` toolchain has no std for {}",
                        missing.join(", ")
                    ),
                    format!(
                        "rustup target add --toolchain {stock} {}",
                        missing.join(" ")
                    ),
                )));
            }
            Some(found)
        };
        Ok(Self { trust, stock })
    }

    fn compiler(&self, cell: &Cell) -> Result<Compiler<'_>, Stop> {
        if cell.triple == self.trust.host {
            return Ok(Compiler::Trust(&self.trust));
        }
        self.stock
            .as_ref()
            .map(Compiler::Stock)
            .ok_or_else(|| Stop::Failed(format!("no compiler for {}", cell.triple)))
    }
}

fn parse_doc(text: &str, cell: &Cell) -> Result<Doc, Stop> {
    Doc::parse(text).map_err(|e| Stop::Failed(format!("{}: {e}", cell.triple)))
}

fn read_doc(path: &Path, cell: &Cell) -> Result<Doc, Stop> {
    parse_doc(
        &std::fs::read_to_string(path).map_err(io("reading", path))?,
        cell,
    )
}

/// One cell's reference rustdoc: a supplied dump, else documented here.
///
/// # Errors
/// When the dump cannot be read or made.
fn reference_doc(
    options: &Options,
    toolchains: &Toolchains,
    reference: &Reference,
    cell: &Cell,
) -> Result<Doc, Stop> {
    match supplied_doc(options.docs.as_deref(), cell) {
        Some(path) => read_doc(&path, cell),
        None => parse_doc(
            &reference.document(&toolchains.compiler(cell)?, cell)?,
            cell,
        ),
    }
}

/// What one cell regenerated to.
#[derive(Debug)]
pub struct CellReport {
    pub key: &'static str,
    pub triple: &'static str,
    pub union_items: usize,
    pub closure_only: Vec<String>,
    pub oracle: oracle::Generated,
    pub shim_lines: usize,
}

/// Each cell, measured, emitted and audited.
fn cell(
    options: &Options,
    toolchains: &Toolchains,
    reference: &Reference,
    union: &Union,
    cell: &Cell,
) -> Result<(String, CellReport), Stop> {
    let doc = reference_doc(options, toolchains, reference, cell)?;
    let public = doc.public().map_err(|e| Stop::Failed(e.0))?;
    let names = union.names(cell)?;
    let decls = decls::read(&doc, &public, &names).map_err(|e| format!("{}: {e}", cell.key))?;
    let compiler = toolchains.compiler(cell)?;
    let mut measured = reference.measure(&compiler, cell, &probe::types_and_offsets(&decls))?;
    let bytes =
        probe::constant_bytes(&decls, &measured.size).map_err(|e| format!("{}: {e}", cell.key))?;
    let pass2 = reference.measure(&compiler, cell, &bytes)?;
    let pass3 = reference.measure(&compiler, cell, &probe::field_types(&decls))?;
    measured.bytes = pass2.bytes;
    measured.field_types = pass3.field_types;
    let mismatches =
        layout::validate(&decls, &measured).map_err(|e| format!("{}: {e}", cell.key))?;
    if !mismatches.is_empty() {
        return Err(Stop::Failed(format!(
            "{}: the repr(C) engine disagrees with the measurements:\n  {}",
            cell.key,
            mismatches.join("\n  ")
        )));
    }
    let shim = shim::emit(cell, &decls, &measured).map_err(|e| format!("{}: {e}", cell.key))?;
    let generated =
        oracle::emit(cell, &doc, &public, &decls).map_err(|e| format!("{}: {e}", cell.key))?;
    let report = CellReport {
        key: cell.key,
        triple: cell.triple,
        union_items: names.len(),
        closure_only: decls.closure_only,
        shim_lines: shim.lines().count(),
        oracle: generated,
    };
    Ok((shim, report))
}

/// The shim module and the oracle module `cell` writes, under `out`.
#[must_use]
pub fn outputs(out: &Path, cell: &Cell) -> [PathBuf; 2] {
    [
        out.join("crates/aterm-libc/src").join(cell.source),
        out.join("libc-oracle/conformance/src")
            .join(format!("cell_{}.rs", cell.key)),
    ]
}

fn format(trust: &Trust, files: &[PathBuf]) -> Result<(), Stop> {
    let trustfmt = trust.trustfmt();
    let out = aterm_uds::spawnfd::command(&trustfmt)
        .args(["--edition", "2024"])
        .args(files)
        .output()
        .map_err(|e| {
            Stop::CouldNotRun(CouldNotRun::new(
                format!("{} could not start: {e}", trustfmt.display()),
                "`aterm pkg repair`",
            ))
        })?;
    if !out.status.success() {
        return Err(Stop::Failed(format!(
            "trustfmt refused the generated source:\n{}",
            tail(&String::from_utf8_lossy(&out.stderr), 20)
        )));
    }
    Ok(())
}

fn print_report(report: &CellReport, sink: &mut dyn Write) -> std::io::Result<()> {
    let oracle = &report.oracle;
    writeln!(
        sink,
        "== {} {}: union {} + closure {}; shim {} lines, oracle {} lines, {} runtime tests",
        report.key,
        report.triple,
        report.union_items,
        report.closure_only.len(),
        report.shim_lines,
        oracle.text.lines().count(),
        oracle.runtime_tests,
    )?;
    for (stat, count) in &oracle.stats {
        writeln!(sink, "    {stat:<28} {count}")?;
    }
    writeln!(
        sink,
        "    checked items {}, checked fields/variants {}, audit {}",
        oracle.checked_items,
        oracle.checked_members,
        if oracle.audit.is_empty() {
            "clean"
        } else {
            "FAILED"
        }
    )
}

/// Regenerate every cell, then write and format the modules, then fail on any
/// coverage gap. Every cell is generated before anything is written, so a run
/// that stops on a cell leaves the tree as it was; the modules are written even
/// when the audit fails, so the gap can be read in place.
///
/// # Errors
/// [`Stop::CouldNotRun`] when a prerequisite is missing; [`Stop::Failed`]
/// when the reference, the measurements, the layout engine or the audit
/// refused, the reason named.
pub fn regen(options: &Options, sink: &mut dyn Write) -> Result<Vec<PathBuf>, Stop> {
    let union = Union::load(&options.root)?;
    let toolchains = Toolchains::find(&options.root, &options.stock, &CELLS)?;
    let reference = Reference::scaffold(&options.work)?;
    let mut generated = Vec::new();
    for cell_row in &CELLS {
        let compiler = toolchains.compiler(cell_row)?.name();
        let _ = writeln!(
            sink,
            "regen: {} ({}) under {compiler}",
            cell_row.key, cell_row.triple
        );
        let (shim_text, report) = cell(options, &toolchains, &reference, &union, cell_row)?;
        let _ = print_report(&report, sink);
        generated.push((cell_row, shim_text, report));
    }
    let mut written = Vec::new();
    let mut audit = Vec::new();
    for (cell_row, shim_text, report) in &generated {
        let [shim_path, oracle_path] = outputs(&options.out, cell_row);
        for (path, text) in [(shim_path, shim_text), (oracle_path, &report.oracle.text)] {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).map_err(io("creating", dir))?;
            }
            std::fs::write(&path, text).map_err(io("writing", &path))?;
            written.push(path);
        }
        audit.extend(
            report
                .oracle
                .audit
                .iter()
                .map(|gap| format!("{}: {gap}", report.key)),
        );
    }
    if options.format {
        format(&toolchains.trust, &written)?;
    }
    if !audit.is_empty() {
        return Err(Stop::Failed(format!(
            "COVERAGE AUDIT FAILED: the shim declares something the oracle does not check:\n  {}",
            audit.join("\n  ")
        )));
    }
    Ok(written)
}

/// One cell's reference rustdoc, for `harvest`: a supplied dump, else
/// documented under that cell's compiler.
///
/// # Errors
/// As [`regen`].
pub fn reference_for(options: &Options, cell: &Cell) -> Result<Doc, Stop> {
    if let Some(path) = supplied_doc(options.docs.as_deref(), cell) {
        return read_doc(&path, cell);
    }
    let toolchains = Toolchains::find(&options.root, &options.stock, [cell])?;
    let reference = Reference::scaffold(&options.work)?;
    reference_doc(options, &toolchains, &reference, cell)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::*;

    #[test]
    fn a_supplied_dump_is_found_by_its_triple() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        assert_eq!(supplied_doc(Some(dir.path()), &CELLS[0]), None);
        let name = format!("libc-{REFERENCE_VERSION}-aarch64-apple-darwin.json");
        std::fs::write(dir.path().join(&name), "{}").expect("write");
        assert_eq!(
            supplied_doc(Some(dir.path()), &CELLS[0]),
            Some(dir.path().join(name))
        );
        assert_eq!(supplied_doc(Some(dir.path()), &CELLS[1]), None);
        assert_eq!(supplied_doc(None, &CELLS[0]), None);
    }

    #[test]
    fn each_cell_writes_its_shim_and_its_oracle_module() {
        let [shim, oracle] = outputs(Path::new("/r"), &CELLS[2]);
        assert_eq!(
            shim,
            Path::new("/r/crates/aterm-libc/src/linux_gnu_x86_64.rs")
        );
        assert_eq!(
            oracle,
            Path::new("/r/libc-oracle/conformance/src/cell_linux.rs")
        );
    }

    /// A stock command never runs from the checkout and always names its
    /// triple; a Trust one runs from the crate and names none.
    #[test]
    fn a_cross_cell_compiles_from_root_for_its_triple_and_the_host_from_the_crate() {
        let stock = Stock {
            name: "stable".into(),
            cargo: "/t/bin/cargo".into(),
            rustc: "/t/bin/rustc".into(),
            rustdoc: "/t/bin/rustdoc".into(),
            rustlib: "/t/lib/rustlib".into(),
        };
        let command = Compiler::Stock(&stock).cargo(
            &CELLS[2],
            Path::new("/w/reference"),
            Path::new("/w/target"),
            &["check"],
            &[],
        );
        let args: Vec<&OsStr> = command.get_args().collect();
        assert_eq!(
            args,
            [
                "check",
                "--target",
                "x86_64-unknown-linux-gnu",
                "--manifest-path",
                "/w/reference/Cargo.toml",
                "--offline",
                "--target-dir",
                "/w/target",
            ]
        );
        assert_eq!(command.get_current_dir(), Some(Path::new("/")));
        let trust = Trust {
            targo: "/s/targo".into(),
            trustc: "/s/trustc".into(),
            trustdoc: "/s/trustdoc".into(),
            host: "aarch64-apple-darwin".into(),
        };
        let command = Compiler::Trust(&trust).cargo(
            &CELLS[0],
            Path::new("/w/reference"),
            Path::new("/w/target"),
            &["rustdoc", "-p", "libc"],
            &["--output-format", "json"],
        );
        let args: Vec<&OsStr> = command.get_args().collect();
        assert_eq!(
            args,
            [
                "--unverified",
                "rustdoc",
                "-p",
                "libc",
                "--offline",
                "--target-dir",
                "/w/target",
                "--",
                "--output-format",
                "json",
            ]
        );
        assert_eq!(command.get_current_dir(), Some(Path::new("/w/reference")));
        assert_eq!(trust.trustfmt(), Path::new("/s/trustfmt"));
    }
}
