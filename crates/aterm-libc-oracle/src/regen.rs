// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-libc-oracle regen` — regenerate `crates/aterm-libc`'s four cell modules and the oracle's four conformance modules from measurements of the REFERENCE libc, then format them and stamp each one's seal ([`aterm_census::seal`]).
//!
//! Native cells and cross cells with actual std/core in the selected Trust
//! sysroot are documented and probed under Trust. Remaining cells need a stock
//! rustup toolchain (`--stock`, by default the checkout's stock pin, with each
//! cross triple's std installed, as `verify provision rustup` installs them on
//! the pin): this crate's one STOCK EXCEPTION.
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

/// The generator each module's seal line names ([`aterm_census::seal`]).
pub const GENERATOR: &str = "`aterm-libc-oracle regen`";

/// Where and how to regenerate.
#[derive(Clone, Debug)]
pub struct Options {
    /// Explicit authoring cell; absent retains the complete four-cell regeneration.
    pub cell: Option<String>,
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

/// The compiler for one cell: Trust with matching target libraries, else stock.
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
                let mut command = aterm_uds::spawnfd::command(&trust.targo);
                command
                    .arg("--unverified")
                    .args(words)
                    .current_dir(dir)
                    .env("RUSTC", &trust.trustc)
                    .env("RUSTDOC", &trust.trustdoc);
                if cell.triple != trust.host {
                    command.args(["--target", cell.triple]);
                }
                command
            }
            Self::Stock(stock) => {
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
            Self::Trust(trust) if cell.triple == trust.host => target.join("doc/libc.json"),
            Self::Trust(_) => target.join(cell.triple).join("doc/libc.json"),
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
        let write = |path: PathBuf, text: &str| {
            crate::write_fresh(&path, text).map_err(io("writing", &path))
        };
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
        crate::read_regular_text(&path).map_err(io("reading", &path))
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
        crate::write_fresh(&file, probe.source()).map_err(io("writing", &file))?;
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
/// the stock one only when a selected cell lacks matching Trust target libraries.
struct Toolchains {
    trust: Trust,
    trust_cells: std::collections::BTreeSet<&'static str>,
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
        let cells: Vec<_> = cells.into_iter().collect();
        let trust_cells = cells
            .iter()
            .filter(|cell| cell.triple == trust.host || trust.has_target(cell.triple))
            .map(|cell| cell.triple)
            .collect::<std::collections::BTreeSet<_>>();
        let foreign: Vec<&Cell> = cells
            .into_iter()
            .filter(|c| !trust_cells.contains(c.triple))
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
                let fix = if stock == aterm_toolchain::rustup::STOCK_PIN {
                    "targo --unverified verify provision rustup".to_string()
                } else {
                    format!(
                        "rustup target add --toolchain {stock} {}",
                        missing.join(" ")
                    )
                };
                return Err(Stop::CouldNotRun(CouldNotRun::new(
                    format!(
                        "the stock `{stock}` toolchain has no std for {}",
                        missing.join(", ")
                    ),
                    fix,
                )));
            }
            Some(found)
        };
        Ok(Self {
            trust,
            trust_cells,
            stock,
        })
    }

    fn compiler(&self, cell: &Cell) -> Result<Compiler<'_>, Stop> {
        if self.trust_cells.contains(cell.triple) {
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
        &crate::read_regular_text(path).map_err(io("reading", path))?,
        cell,
    )
}

/// One cell's reference rustdoc: a supplied dump, else documented here.
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

/// Stamp each written module's seal over its final, formatted text.
fn seal(files: &[PathBuf]) -> Result<(), Stop> {
    for path in files {
        let text = crate::read_regular_text(path).map_err(io("reading", path))?;
        let sealed = aterm_census::seal::stamp(&text)
            .map_err(|e| Stop::Failed(format!("{}: {e}", path.display())))?;
        crate::write_fresh(path, sealed).map_err(io("writing", path))?;
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

/// Regenerate the selected cells, write and format their modules, then fail on any coverage gap.
/// # Errors
/// [`Stop::CouldNotRun`] when a prerequisite is missing; [`Stop::Failed`] when the reference, the measurements, the layout engine or the audit refused, the reason named.
pub fn regen(options: &Options, sink: &mut dyn Write) -> Result<Vec<PathBuf>, Stop> {
    let union = Union::load(&options.root)?;
    let cells = selected_cells(options.cell.as_deref())?;
    let toolchains = Toolchains::find(&options.root, &options.stock, cells.iter().copied())?;
    let reference = Reference::scaffold(&options.work)?;
    let mut generated = Vec::new();
    for cell_row in cells {
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
            crate::write_fresh(&path, text).map_err(io("writing", &path))?;
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
    seal(&written)?;
    if !audit.is_empty() {
        return Err(Stop::Failed(format!(
            "COVERAGE AUDIT FAILED: the shim declares something the oracle does not check:\n  {}",
            audit.join("\n  ")
        )));
    }
    Ok(written)
}

fn selected_cells(key: Option<&str>) -> Result<Vec<&'static Cell>, Stop> {
    match key {
        None => Ok(CELLS.iter().collect()),
        Some(key) => CELLS
            .iter()
            .find(|cell| cell.key == key)
            .map(|cell| vec![cell])
            .ok_or_else(|| Stop::Failed(format!("unknown regeneration cell {key}"))),
    }
}

/// One cell's reference rustdoc, for `harvest`: a supplied dump, else documented under that cell's compiler.
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
#[path = "regen_tests.rs"]
mod tests;
