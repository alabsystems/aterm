// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The xtask gate verbs: the checks the merge gate (`tools/verify.sh`, i.e.
//! `crates/aterm-verify`) shells into this binary for. There is NO CI.
//!
//! Run as `targo --unverified run -p xtask -- gate <verb>`. Every verb here has
//! an automatic caller in `crates/aterm-verify/src/stages.rs`; read that, not
//! this list, for which tier runs which:
//!
//! - `lint` (`--fmt-only` is the documented oracle spelling; the two are the
//!   same run): the formatter, three passes folded into one verdict —
//!   `targo-fmt --all --check` over the workspace; a per-file `trustfmt --check`
//!   sweep over every tracked `.rs` outside `vendor/`, at the edition of the
//!   crate that owns it, which reaches what `--all` cannot (sources pulled in
//!   with `include!`, and every crate outside `members = ["crates/*"]`); and
//!   `fmt editions`, a finding whenever a file's `rustfmt.toml` resolves a
//!   different edition than its manifest declares, since every hand-run
//!   formatter reads the former. Tippy is not here: the gate's Tippy stage
//!   drives it directly.
//! - `forge`: the third-party surface policy, [`aterm_forge::check::check_report`]
//!   — the same function `cargo forge check` runs, so the gate and the hand-run
//!   tool cannot disagree. `tools/forge-budget.tsv` is the authority on the
//!   numbers it ratchets.
//! - `cells [--cell NAME]…`: every forge cell, type-checked by a compiler for its
//!   own triple, under the policy in `tools/cross-cell-gate.tsv`.
//! - `cells-foreign`: `cells` narrowed to the cells no box in this fleet hosts
//!   ([`FLEET_HOST_TRIPLES`]), whose verdict is the same wherever it runs.
//!
//! A verb that could not look says so — NOT RUN, COULD NOT RUN, SKIPPED — and
//! never prints a pass for it: see [`LaneVerdict`] and [`CellOutcome`].

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use crate::driver::{DRIVER_REMEDY, cargo_driver, export_driver_as_cargo, rustc_host_triple};
use crate::workspace_root;

/// A gate verb: its name, and the check it runs on the arguments after it.
type Verb = (&'static str, fn(&[String]) -> bool);

/// Every verb `gate` dispatches, in the order the usage line prints them. ONE
/// table: the dispatch reads it and the usage lines (here and in `main.rs`) are
/// printed from it, so a verb cannot be dispatched without being listed.
const VERBS: &[Verb] = &[
    ("lint", gate_lint),
    ("forge", |_| gate_forge()),
    ("cells", gate_cells),
    ("cells-foreign", |_| gate_cells_foreign()),
];

/// The verb names, for the usage lines.
pub(crate) fn verb_names() -> Vec<&'static str> {
    VERBS.iter().map(|(name, _)| *name).collect()
}

/// `rest` is everything after the verb name.
pub(crate) fn run(check: Option<&str>, rest: &[String]) -> ExitCode {
    let Some((_, verb)) = VERBS.iter().find(|(name, _)| Some(*name) == check) else {
        eprintln!(
            "usage: xtask gate <{}>\n(unknown check {check:?})",
            verb_names().join("|")
        );
        return ExitCode::FAILURE;
    };
    if verb(rest) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

// ---------------------------------------------------------------------------
// G-FORGE: THIRD-PARTY SURFACE POLICY (provenance, patch liveness, the ratchet)
// ---------------------------------------------------------------------------
//
// `crates/aterm-forge` owns the obligations (`[OB-1]`..`[OB-15]`, see its crate
// docs); this verb and `cargo forge check` both call `check::check_report`. It
// compiles nothing: it reads `Cargo.lock`, `vendor/`, `vendor/forge.toml`,
// `tools/forge-budget.tsv` and one offline `cargo tree` per cell.

fn gate_forge() -> bool {
    // forge resolves its cells with `$CARGO tree`; hand it the host driver when
    // this process was started without one (crate::driver documents the case).
    export_driver_as_cargo();
    let (ok, log) = aterm_forge::check::check_report(&workspace_root());
    eprint!("{log}");
    ok
}

// ---------------------------------------------------------------------------
// THE TOOLCHAIN AND THE PROCESS HELPERS
// ---------------------------------------------------------------------------

/// THE toolchain, resolved by `aterm_verify::Toolchain` — the SAME code
/// `tools/verify.sh`'s driver runs, not a second copy of the same rules.
///
/// It answers in the workspace's ONE order — `$TRUST_STAGE2_BIN`, the rustup
/// `trust` toolchain (below the store when it is older than the store's build:
/// `aterm_verify::toolchain::Demoted`), the atpkg store, PATH — always
/// canonicalised because Trust's drivers reject a symlinked toolchain path. Under
/// the merge gate `$TRUST_STAGE2_BIN` is set whenever the gate found a `targo`:
/// aterm-verify hands every child the directory its header names, so a verb run by
/// a stage cannot re-discover another build mid-run. A gate that found none adds
/// nothing (a caller's own export passes through as it came), and this walk then
/// answers for itself.
///
/// AND IT CHECKS THE PIN. A directory holding a file called `targo` is not
/// evidence that it is the fork `rust-toolchain.toml` names; the branded rustc
/// (`trustc`) beside it is. A candidate that fails is REFUSED, not adopted —
/// otherwise this verb would lint with a different frontend under the pinned
/// one's name and print GREEN, which is the accident that put six `-D warnings`
/// violations on main in the sibling `clean` repo.
pub(crate) fn trust_toolchain() -> aterm_verify::Toolchain {
    let root = workspace_root();
    let home = std::env::var_os("HOME").unwrap_or_default();
    let path = std::env::var_os("PATH").unwrap_or_default();
    aterm_verify::Toolchain::discover(
        std::env::var_os("TRUST_STAGE2_BIN")
            .map(PathBuf::from)
            .as_deref(),
        Path::new(&home),
        &path,
        aterm_verify::toolchain::pinned_channel(&root).as_deref(),
    )
}

/// Run a tool capturing BOTH streams, teeing each so the operator sees the
/// report exactly as a hand-run prints it. Returns `(exit-ok, stdout, stderr)`.
///
/// Both streams, drained CONCURRENTLY by `Command::output()`: the formatter's
/// findings land on stdout and its environment faults on stderr, and draining
/// two pipes in sequence deadlocks the moment the one not being read fills its
/// 64 KiB buffer (a real drift report is ~250 KiB of stdout).
fn run_capturing_both(
    desc: &str,
    program: &Path,
    args: &[&str],
    path_prefix: &Path,
    cwd: &Path,
) -> (bool, String, String) {
    eprintln!("  $ {} {}", program.display(), args.join(" "));
    let mut command = Command::new(program);
    command.args(args).current_dir(cwd);
    let existing = std::env::var_os("PATH").unwrap_or_default();
    let mut entries = vec![path_prefix.to_path_buf()];
    entries.extend(std::env::split_paths(&existing));
    match std::env::join_paths(entries) {
        Ok(joined) => {
            command.env("PATH", joined);
        }
        Err(e) => eprintln!("  {desc}: could not extend PATH ({e}); using inherited PATH"),
    }
    match command.output() {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
            let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
            print!("{stdout}");
            eprint!("{stderr}");
            if !out.status.success() {
                eprintln!("  {desc}: exited {:?}", out.status.code());
            }
            (out.status.success(), stdout, stderr)
        }
        Err(e) => {
            eprintln!("  {desc}: could not run ({e})");
            (false, String::new(), e.to_string())
        }
    }
}

/// Is `bin` resolvable on `PATH`?
fn on_path(bin: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {bin}"))
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// A neutral build cwd for a cross-compile, because cargo discovers config by walking
/// the cwd upward and `.cargo/config.toml` carries `-Ztrust-verify=off` for the native
/// triple — a flag upstream stable rejects as an unknown `-Z` on every HOST unit (build
/// script, proc macro) of a `--target` build. `tools/wasm-bench/run.sh` uses the same
/// trick for the same reason. Load-bearing: without it these gates go red with a
/// flag-parse error that names no crate of ours.
fn neutral_build_cwd(gate: &str) -> Option<std::path::PathBuf> {
    let dir = std::env::temp_dir().join(format!("aterm-{gate}-{}", std::process::id()));
    match std::fs::create_dir_all(&dir) {
        Ok(()) => Some(dir),
        Err(e) => {
            eprintln!(
                "gate {gate}: FAILED — could not create the neutral build cwd {}: {e}",
                dir.display()
            );
            None
        }
    }
}

/// Does `listing` — the stdout of `rustup target list --installed` — name exactly
/// `target`? Line-exact ON PURPOSE: a substring test would read
/// `wasm32-unknown-unknown` out of `wasm32-unknown-unknown-nightly`, and read
/// `wasm32-wasip1` as a match for the plain triple, so a box with the WRONG wasm
/// target would sail past the pre-flight and fail the build instead of skipping.
fn toolchain_lists_target(listing: &str, target: &str) -> bool {
    listing.lines().any(|l| l.trim() == target)
}

// ---------------------------------------------------------------------------
// G-CELLS (the five forge cells, TYPE-CHECKED for their own triples)
// ---------------------------------------------------------------------------

/// The policy `gate cells` reads: the excused C dependencies and the per-cell
/// coverage floors. Workspace-relative, and NEVER written by this gate — the
/// floors are hand-edited, because `tools/trust-gate-all.sh`'s `--update-ratchet`
/// is the recorded case of a writer flag letting the one run that DETECTED a
/// regression erase the floor it had just tripped over.
const CELL_GATE_POLICY: &str = "tools/cross-cell-gate.tsv";

/// One `cdep` row: a package whose build script cannot run on this box.
struct ExcusedCDep {
    package: String,
    /// The triple the excuse is scoped to, or `*` for every triple.
    triple: String,
    why: String,
}

/// One `cshim` row: a C-bearing build script this gate REPLACES, for the length
/// of one `cargo check` and on ONE named triple, with a cargo build-script
/// override passed on the command line.
///
/// A `cdep` row buys silence; a `cshim` row buys COVERAGE. Where a `cdep` says
/// "this box cannot run that build script, so forgive the closure it takes with
/// it", a `cshim` says "that build script emits nothing a compiler reads, so
/// cargo may skip it and type-check the Rust anyway". The second is only honest
/// when the claim inside it is TRUE, which is why the row pins the exact
/// `name@version` it was read against: a bump makes the row dead, and a dead row
/// fails the gate rather than silently shimming a build script nobody re-read.
struct CBuildShim {
    package: String,
    /// The exact version the build script was read at. A row for a version the
    /// graph does not carry is DEAD.
    version: String,
    /// The one triple this override applies to. Never `*`: an override is a
    /// claim about what a build script emits FOR A TARGET, and `zstd-sys` is the
    /// standing proof that the answer differs by target.
    triple: String,
    /// The package's `links` key, which is the name cargo's
    /// `[target.<triple>.<links>]` override table is addressed by.
    links: String,
    why: String,
}

/// One `floor` row: the coverage high-water mark for a cell.
struct CoverageFloor {
    cell: String,
    packages: usize,
}

/// [`CELL_GATE_POLICY`], parsed.
struct CellPolicy {
    cdeps: Vec<ExcusedCDep>,
    cshims: Vec<CBuildShim>,
    floors: Vec<CoverageFloor>,
}

impl CellPolicy {
    /// Is `package` excused on `triple`?
    fn excuse(&self, package: &str, triple: &str) -> Option<&ExcusedCDep> {
        self.cdeps
            .iter()
            .find(|c| c.package == package && (c.triple == "*" || c.triple == triple))
    }

    fn floor(&self, cell: &str) -> Option<usize> {
        self.floors
            .iter()
            .find(|f| f.cell == cell)
            .map(|f| f.packages)
    }

    /// The build-script overrides this policy declares for `triple`, each with
    /// its row index so the dead-row audit can tell two rows for one package on
    /// two triples apart.
    fn shims_for(&self, triple: &str) -> Vec<(usize, &CBuildShim)> {
        self.cshims
            .iter()
            .enumerate()
            .filter(|(_, c)| c.triple == triple)
            .collect()
    }
}

/// WHERE A BUILD-SCRIPT OVERRIDE IS NOT ALLOWED TO BE USED ANYWHERE, named
/// rather than assumed.
///
/// An override is faithful exactly when the build script it replaces emits
/// nothing that changes how the compiler reads the crate — no `rustc-cfg`, no
/// `rustc-env`, no generated source. That is a claim about a build script AND a
/// target, and `zstd-sys` is the proof it can differ by target: its `main` emits
/// `cargo:rustc-cfg=feature="std"` when `CARGO_CFG_TARGET_ARCH` is `wasm32` or
/// `CARGO_CFG_TARGET_OS` is `hermit`, and nothing on any other target. Shimming
/// it there would type-check a DIFFERENT crate and call the result coverage.
///
/// So the two triples where the one shipped shim is known to be unfaithful are
/// refused by the parser itself, and a policy row that names one is a hard error
/// rather than a silently-wrong green.
///
/// EVERYTHING REFUSED HERE IS REFUSED ON EVERY BOX, and that is the rule for
/// what may live in this function. The policy file is committed and read on
/// macOS, Linux and Windows alike, so a refusal that depends on which machine
/// is reading cannot be a parse error about the FILE — it would make a portable
/// row unwritable and take the whole verb down with it. The host's own triple
/// is exactly such a refusal, and it lives in `host_shim_refusal`, applied to
/// the one cell it is about.
fn shim_refusal(triple: &str) -> Option<String> {
    if triple.starts_with("wasm32") || triple.contains("hermit") {
        return Some(format!(
            "`{triple}` is a target where a bundled-C build script is known to emit `rustc-cfg` \
             (zstd-sys emits `feature=\"std\"` for wasm32 and hermit). An override there would \
             type-check a different crate than the one that ships"
        ));
    }
    None
}

/// WHERE A BUILD-SCRIPT OVERRIDE IS NOT ALLOWED TO BE APPLIED ON *THIS* BOX.
///
/// A judge proved this one: triple-scoping does NOT exclude the host. The
/// native cell runs with no `--target`, and cargo applies a
/// `[target.<host-triple>.<links>]` override there too — so a committed row
/// naming `aarch64-apple-darwin` made `gate cells --cell mac-arm` print
/// `SHIMMED … 114/114 … 69/69` and exit 0 on an Apple box while zstd-sys's
/// build script never ran.
///
/// The principle: a cell that can RUN a build script has no need of an
/// override. The shim exists only because no cross C toolchain is installed;
/// on the host, one is.
///
/// WHICH TRIPLE IS THE HOST IS A FACT ABOUT THE MACHINE, NOT ABOUT THE FILE,
/// and reading it as a fact about the file is what this function exists to
/// stop. The refusal used to fire inside `parse_cell_policy`, before a single
/// cell had been selected, so the two committed `x86_64-unknown-linux-gnu`
/// rows — correct rows, measured on m21 where Linux is a CROSS cell — made
/// `gate cells --cell win` print `COULD NOT RUN` on every Linux box and never
/// reach the `win` cell at all: the only gate that compiles aterm for Windows,
/// unstartable on a Linux box, so no Windows break authored on one could turn
/// it red. So the answer is per-cell. The row is CARRIED, the cell whose triple
/// is the host declines to apply it and says so, and every other cell gets its
/// overrides. Such a row is not a dead row either — the box that needs it is
/// some other box.
fn host_shim_refusal(triple: &str, host: &str) -> Option<String> {
    (triple == host).then(|| {
        format!(
            "`{triple}` is this machine's own triple, where the build script RUNS — a cell that \
             can run a build script needs no override, and applying one there would suppress the \
             real script and call the result coverage"
        )
    })
}

/// Parse [`CELL_GATE_POLICY`]. Strict on purpose: an unreadable row is a
/// COULD-NOT-RUN, never a silently ignored line, because every row of this file
/// either excuses a failure or sets a floor — both are ways for the gate to
/// pass while proving less, which is exactly what it exists to prevent.
fn parse_cell_policy(text: &str) -> Result<CellPolicy, String> {
    let mut cdeps = Vec::new();
    let mut cshims = Vec::new();
    let mut floors = Vec::new();
    for (n, raw) in text.lines().enumerate() {
        let line = raw.trim_end();
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        // `cshim` carries one more column than the others — the package's
        // `links` key, which is the only name cargo's override table answers to
        // and which cannot be derived from the package name (`zstd-sys` links
        // `zstd`).
        let want = if cols.first() == Some(&"cshim") { 5 } else { 4 };
        if cols.len() < want {
            return Err(format!(
                "{CELL_GATE_POLICY}:{}: a `{}` row takes {want} TAB-separated columns, found {}",
                n + 1,
                cols.first().copied().unwrap_or(""),
                cols.len()
            ));
        }
        match cols[0] {
            "cdep" => cdeps.push(ExcusedCDep {
                package: cols[1].to_string(),
                triple: cols[2].to_string(),
                why: cols[3].to_string(),
            }),
            "cshim" => {
                // `name@version`. The version is not decoration: it is the
                // whole reason a reader can trust the row. It says WHICH build
                // script was read, so a bump retires the claim instead of
                // inheriting it.
                let Some((package, version)) = cols[1].split_once('@') else {
                    return Err(format!(
                        "{CELL_GATE_POLICY}:{}: a `cshim` key is `name@version` (the exact version \
                         whose build script was read), not `{}`",
                        n + 1,
                        cols[1]
                    ));
                };
                if cols[2] == "*" {
                    return Err(format!(
                        "{CELL_GATE_POLICY}:{}: `cshim` rows name ONE triple — an override is a \
                         claim about what a build script emits FOR A TARGET, and `zstd-sys` emits \
                         a different set on wasm32 than anywhere else.",
                        n + 1
                    ));
                }
                if let Some(why) = shim_refusal(cols[2]) {
                    return Err(format!(
                        "{CELL_GATE_POLICY}:{}: refusing the `cshim` row for `{package}` — {why}.",
                        n + 1
                    ));
                }
                cshims.push(CBuildShim {
                    package: package.to_string(),
                    version: version.to_string(),
                    triple: cols[2].to_string(),
                    links: cols[3].to_string(),
                    why: cols[4].to_string(),
                });
            }
            "floor" => {
                let packages = cols[2].parse::<usize>().map_err(|e| {
                    format!(
                        "{CELL_GATE_POLICY}:{}: floor for `{}` is `{}`, not a package count ({e})",
                        n + 1,
                        cols[1],
                        cols[2]
                    )
                })?;
                floors.push(CoverageFloor {
                    cell: cols[1].to_string(),
                    packages,
                });
            }
            other => {
                return Err(format!(
                    "{CELL_GATE_POLICY}:{}: unknown row kind `{other}` — the kinds are `cdep`, \
                     `cshim` and `floor`",
                    n + 1
                ));
            }
        }
    }
    Ok(CellPolicy {
        cdeps,
        cshims,
        floors,
    })
}

/// The package name inside a cargo `package_id`, which comes in three shapes
/// and has exactly one trap.
///
/// `registry+https://…#serde@1.0.228` and `path+file:///…/foo#bar@0.1.0` both
/// carry `name@version` after the `#`. But a PATH package whose directory is
/// already its name abbreviates to `path+file:///…/serde#1.0.228` — no name at
/// all, just a version. Reading the fragment as the name there yields `1.0.228`,
/// and every package that shape covers silently vanishes from the coverage
/// count. THIS IS NOT HYPOTHETICAL: the first measurement taken for this gate
/// reported `indexmap`, `winit`, `libm`, `smol_str` and every `aterm-*` crate as
/// unchecked on Linux — 42 phantom holes — for exactly this reason, and the
/// conclusion drawn from it (that `--keep-going` stops scheduling after a
/// failure) was false. The name is the last path segment when the fragment has
/// no `@`.
fn package_id_name(id: &str) -> &str {
    let Some((url, frag)) = id.rsplit_once('#') else {
        // Cargo's older opaque form: `name version (source)`.
        return id.split_whitespace().next().unwrap_or(id);
    };
    if let Some((name, _version)) = frag.split_once('@') {
        return name;
    }
    url.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(url)
        .trim_end_matches(".git")
}

/// The package named by cargo's ``error: failed to run custom build command for
/// `NAME vX.Y.Z (…)` `` line, or `None` for any other line.
///
/// Anchored at the whole prefix rather than a `contains`, because this string is
/// the ONE failure this gate is allowed to excuse — a looser match would let an
/// arbitrary error carrying that phrase in a diagnostic body buy an excuse.
fn build_script_failure(line: &str) -> Option<&str> {
    let rest = line
        .trim()
        .strip_prefix("error: failed to run custom build command for `")?;
    let inner = rest.strip_suffix('`').unwrap_or(rest);
    // `NAME vX.Y.Z` or `NAME vX.Y.Z (/path)`.
    inner.rsplit_once(" v").map(|(name, _)| name)
}

/// The toolchain names in `rustup toolchain list`'s stdout.
///
/// The active one is printed as `trust (active, default)`, so a caller that
/// takes the line verbatim asks rustup for a toolchain called
/// `trust (active, default)` and gets a "not installed" error it will read as
/// "this box cannot build that cell".
fn parse_toolchain_names(listing: &str) -> Vec<String> {
    listing
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

// This compiler's own host triple — `crate::driver::rustc_host_triple`, imported
// above. Used to tell the HOST cell from the four cross cells — never a name
// comparison, because the cell list is forge's and this gate may not encode a
// second opinion about which row is native. Until 2026-09-18 it was a bare
// `rustc -vV` here, which is `None` on every box provisioned with `aterm pkg
// install trust` and nothing else (no rustup, no `rustc` on PATH — the owner's
// Mac that day): `gate cells` COULD NOT RUN and three tests below panicked. The
// ladder now asks `$RUSTC`, then `$CARGO`'s sibling `rustc`/`trustc`, then the
// store's `trustc`, then bare `rustc` last; the module documents each rung.

/// The `check` command for ONE cell, shaped by whether the cell is this box's
/// own triple. Both cell passes (library and test-target) build theirs here, so
/// they cannot disagree about it.
///
/// THE HOST CELL goes through the host driver (`crate::driver::cargo_driver`:
/// `$CARGO`, the store's targo, a prefix-resolving PATH targo, bare `cargo`)
/// with NO `RUSTUP_TOOLCHAIN` and NO `--target`. Both halves are load-bearing,
/// not tidiness. `RUSTUP_TOOLCHAIN` is a rustup-proxy variable: targo ignores
/// it, and the value the host cell used to export — the literal string `repo
/// pin (rust-toolchain.toml)`, a LABEL — would have sent a rustup-proxied cargo
/// looking for a toolchain of that name. And `--target <native triple>` is the
/// COROLLARY `.cargo/config.toml` spells out: any explicit target makes cargo
/// withhold the `[target.'cfg(trust_verify)']` rustflags (`-Ztrust-verify=off`)
/// from host units — build scripts, proc macros and their dependencies — which
/// then verify STRICTLY and fail the build. Plain `check` applies them to every
/// unit. The library pass had this right from the start; the test-target pass
/// exported both until 2026-09-18 (reachable only through `TestPassJob`, whose
/// one caller exempts the host cell — a latent shape, fixed by construction).
///
/// A CROSS CELL is the other lane entirely: the `cargo` of the rustup toolchain
/// carrying the cell's std ([`RustupToolchain::cargo`]) with `--target <triple>`,
/// from a neutral cwd. That lane is upstream stable BY DESIGN (rust-toolchain.toml's
/// header).
fn cell_check_command(is_host: bool, toolchain: Option<&RustupToolchain>, triple: &str) -> Command {
    if is_host {
        cargo_driver().command("check")
    } else {
        let mut cmd = toolchain.map_or_else(|| Command::new("cargo"), RustupToolchain::cargo);
        cmd.arg("check").arg("--target").arg(triple);
        cmd
    }
}

/// A rustup toolchain resolved to its OWN drivers, by path.
///
/// NOT `cargo` off PATH with `RUSTUP_TOOLCHAIN` exported, which is what every cross
/// lane here did until 2026-09-24: that variable is read only by rustup's proxies,
/// and whatever `cargo` wins PATH first ignores it and compiles with its own
/// `rustc`. Measured on m7 that day, where Homebrew's cargo sits behind aterm's
/// reroute stub ahead of `~/.cargo/bin`: `gate cells` printed
/// `stable-aarch64-apple-darwin` for wasm-cpu, wasm-gpu and mac-x64, and all three
/// died E0463 ("can't find crate for `std`") under Homebrew's 1.95, which carries
/// no cross std — while `stable` carried both. `RUSTC` pins the compiler for cargo
/// and for every build script it runs.
#[derive(Clone, Debug, PartialEq, Eq)]
struct RustupToolchain {
    name: String,
    cargo: PathBuf,
    rustc: PathBuf,
}

impl RustupToolchain {
    /// `name`'s `cargo` and `rustc`, as `rustup which --toolchain` answers them.
    fn resolve(name: &str) -> Result<Self, String> {
        let which = |tool: &str| -> Result<PathBuf, String> {
            let out = Command::new("rustup")
                .args(["which", "--toolchain", name, tool])
                .output()
                .map_err(|e| format!("could not ask rustup for `{name}`'s {tool} ({e})"))?;
            let path = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
            if out.status.success() && path.is_file() {
                Ok(path)
            } else {
                Err(format!(
                    "`rustup which --toolchain {name} {tool}` names no file: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ))
            }
        };
        Ok(Self {
            name: name.to_string(),
            cargo: which("cargo")?,
            rustc: which("rustc")?,
        })
    }

    /// This toolchain's `cargo`, driving this toolchain's `rustc`.
    fn cargo(&self) -> Command {
        let mut cmd = Command::new(&self.cargo);
        cmd.env("RUSTC", &self.rustc)
            .env("RUSTUP_TOOLCHAIN", &self.name);
        cmd
    }
}

/// Pick an installed toolchain that carries `triple`'s std.
///
/// `ATERM_CELL_TOOLCHAIN` overrides, and an override that does NOT carry the
/// triple is an ERROR rather than a skip: a caller who names a toolchain has
/// stated an intent, and silently ignoring it is how a gate ends up reporting on
/// a lane nobody asked for. With no override the installed toolchains are asked
/// in rustup's own listing order and the first match is used and PRINTED, so the
/// answer is reproducible and quotable rather than implicit.
fn cell_toolchain(triple: &str) -> Result<Option<RustupToolchain>, String> {
    if !on_path("rustup") {
        return Ok(None);
    }
    let carries = |tc: &str| -> bool {
        Command::new("rustup")
            .args(["target", "list", "--installed", "--toolchain", tc])
            .output()
            .map(|o| {
                o.status.success()
                    && toolchain_lists_target(&String::from_utf8_lossy(&o.stdout), triple)
            })
            .unwrap_or(false)
    };
    if let Ok(pinned) = std::env::var("ATERM_CELL_TOOLCHAIN") {
        return if carries(&pinned) {
            RustupToolchain::resolve(&pinned).map(Some)
        } else {
            Err(format!(
                "$ATERM_CELL_TOOLCHAIN names `{pinned}`, which has no {triple} std. Install it \
                 (`rustup target add {triple} --toolchain {pinned}`) or unset the variable and let \
                 the gate pick."
            ))
        };
    }
    let list = Command::new("rustup")
        .args(["toolchain", "list"])
        .output()
        .map_err(|e| format!("could not ask rustup which toolchains are installed ({e})"))?;
    if !list.status.success() {
        return Err(format!(
            "`rustup toolchain list` failed: {}",
            String::from_utf8_lossy(&list.stderr).trim_end()
        ));
    }
    // RELEASE CHANNELS FIRST. A nightly compiler accepts syntax and features a
    // release one refuses, so a cell checked on nightly proves slightly less
    // than the same cell checked on stable — and which toolchain rustup happens
    // to list first is not a decision anybody made. Ordering is stable, the
    // choice is printed, and $ATERM_CELL_TOOLCHAIN overrides it.
    let mut names = parse_toolchain_names(&String::from_utf8_lossy(&list.stdout));
    names.sort_by_key(|tc| u8::from(tc.starts_with("nightly")));
    names
        .into_iter()
        .find(|tc| carries(tc))
        .map(|tc| RustupToolchain::resolve(&tc))
        .transpose()
}

/// The STATUS word a cell wears when no installed toolchain carries its std.
/// ONE definition, spelled at the only place such a row is built
/// ([`CellReport::skipped`]) and read by the tests that hold the row to it.
const SKIPPED_NO_STD: &str = "SKIPPED(no-std)";

/// What one cell's run DECIDED. THREE-VALUED, for the reason [`LaneVerdict`] is.
///
/// This field was a `bool` named `ok`, and a cell for which NO COMPILER EVER
/// STARTED carried `ok: true` beside `status: "SKIPPED(no-std)"`. Measured on
/// 2026-09-17, on a box with no cross std installed and no native cell —
/// `xtask gate cells`, five cells, zero compilers, exit 0:
///
/// ```text
/// mac-arm  aarch64-apple-darwin  -  0  121  0/76  1  -  0  SKIPPED(no-std)
/// …
/// gate cells: GREEN — all 5 cells type-check, with nothing excused or shimmed:
/// 0 of 312 in-repo crate-instances read; 312 NOT; test targets compiled for 0
/// of 0 member-instances (5 not owed a test pass: mac-arm, linux, win, …).
/// ```
///
/// "All 5 cells type-check" is the quotable sentence, and it was true of no
/// cell in that run. A skip is NOT a pass.
///
/// It is not a failure either, and that half is a decision this type pins
/// rather than a convenience. An uninstalled std is a fact about the BOX,
/// repairable by one `rustup target add`, identical on every checkout of the
/// tree — so blocking on it would be a red that no change to this repository
/// could clear, and [`LaneVerdict`] records what a permanent red costs
/// (three lint-red commits reached `main` under a gate that could not pass).
/// So: exit 0, and the MATRIX CLAIM is forfeit — the same discipline
/// `aterm_verify::verdict` holds for the run as a whole, where a skipped stage
/// keeps exit 0 and loses `merge contract satisfied`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CellOutcome {
    /// A compiler read this cell's graph FOR THE CELL'S OWN TRIPLE, and every
    /// obligation the policy file records for it held.
    Checked,
    /// No installed toolchain carries this triple's std. NOTHING was compiled:
    /// neither a pass nor a finding, because nothing about the tree was learned.
    Skipped,
    /// A compiler ran and the cell did not hold. Blocks.
    Failed,
}

/// What one cell's check produced.
struct CellReport {
    cell: String,
    triple: String,
    toolchain: String,
    /// Packages of the cell's OWN graph that got a target artifact.
    checked: usize,
    graph: usize,
    excused: Vec<String>,
    /// Build scripts replaced by a [`CBuildShim`] on this cell.
    shimmed: Vec<String>,
    /// IN-REPO packages of this cell's graph that a compiler read, over the
    /// number it could read at all. THE NUMBER THIS GATE IS ABOUT: a cell can
    /// be at its coverage floor with every line of aterm's own platform code
    /// unread, which is exactly the state the Linux and Windows cells shipped
    /// in, and `206/253` does not say so.
    own_checked: usize,
    own_total: usize,
    /// In-repo packages that CANNOT produce an artifact for a cross triple
    /// because they are proc macros, and so are not owed one.
    own_proc_macros: usize,
    /// Workspace members in this cell's graph whose TEST targets a compiler read
    /// for the cell's triple, over the number owed. `0/0` where the cell is
    /// exempt (`tests_note`) — the host cell, and the two wasm32 cells.
    tests_checked: usize,
    tests_owed: usize,
    /// Why the test-target pass did not run here, when it did not.
    tests_note: Option<String>,
    /// The cell's wall seconds: its check and its test-target pass.
    secs: u64,
    /// What the cell's target dir holds once its compile is done
    /// (`aterm_verify::disk::dir_bytes`), or `None` where nothing was
    /// compiled. With `secs`, the cell's COST, printed on every run: the
    /// foreign cells run in every merge-contract run, and their target dirs
    /// are the most that run writes outside the snapshot's own target lanes
    /// (2.6 GiB cold, 2026-09-27), so a reader of the log sees what the gate
    /// cost without measuring it by hand.
    disk: Option<u64>,
    status: String,
    outcome: CellOutcome,
}

impl CellReport {
    /// The row for a cell NOTHING WAS COMPILED FOR, built in one place so the
    /// STATUS word and the [`CellOutcome`] cannot be set apart from each other.
    /// They were separate assignments forty lines apart, and they disagreed:
    /// `SKIPPED(no-std)` beside `ok: true`. A constructor is the repair, not a
    /// comment — the two facts now leave the same expression or neither does.
    fn skipped(
        cell: &aterm_forge::model::Cell,
        graph: usize,
        own_total: usize,
        own_proc_macros: usize,
    ) -> Self {
        Self {
            cell: cell.name.clone(),
            triple: cell.triple.clone(),
            toolchain: "-".to_string(),
            checked: 0,
            graph,
            excused: Vec::new(),
            shimmed: Vec::new(),
            own_checked: 0,
            own_total,
            own_proc_macros,
            tests_checked: 0,
            tests_owed: 0,
            tests_note: Some("nothing was compiled for this cell at all".to_string()),
            secs: 0,
            disk: None,
            status: SKIPPED_NO_STD.to_string(),
            outcome: CellOutcome::Skipped,
        }
    }
}

/// Does `filenames` contain an artifact built FOR `triple`?
///
/// Cross builds put target units under `<target-dir>/<triple>/…` and host units
/// (build scripts, proc macros) under `<target-dir>/debug/…`, so this is what
/// separates "the cell's code type-checked" from "a proc macro compiled for this
/// Mac". `None` means the check ran without `--target`, where the two are the
/// same units and every artifact counts.
fn artifact_is_for(triple: Option<&str>, filenames: &[String]) -> bool {
    match triple {
        None => true,
        Some(t) => {
            let needle = format!("/{t}/");
            filenames.iter().any(|f| f.contains(&needle))
        }
    }
}

/// Is `dir` inside `root`? Both are absolutised first, because `cargo tree`
/// prints the path it resolved and `workspace_root()` returns the path the
/// process was launched with — on macOS one of those routinely says `/tmp` and
/// the other `/private/tmp`, and a raw `starts_with` between them answers "no"
/// for every package in the workspace.
fn path_is_inside(dir: &Path, root: &Path) -> bool {
    match (dir.canonicalize(), root.canonicalize()) {
        (Ok(d), Ok(r)) => d.starts_with(r),
        _ => dir.starts_with(root),
    }
}

/// Does the manifest in `dir` declare a proc-macro crate?
///
/// A proc macro is compiled for the HOST even in a cross build, so it can never
/// produce an artifact for the cell's triple and is not owed one. Read from the
/// manifest rather than from a hard-coded list: the three proc macros in this
/// workspace today are not a fact anybody should have to maintain in two places.
fn manifest_is_proc_macro(dir: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(dir.join("Cargo.toml")) else {
        return false;
    };
    // `proc-macro = true` is only legal under `[lib]`, and a `[lib]` section
    // ends at the next table header.
    let mut in_lib = false;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_lib = t == "[lib]";
            continue;
        }
        if in_lib && t.replace(' ', "").starts_with("proc-macro=true") {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// G-CELLS, SECOND PASS: THE TEST TARGETS
// ---------------------------------------------------------------------------

/// The names of every workspace member, from `cargo metadata --no-deps`.
///
/// Parsed rather than globbed out of the root manifest because `[workspace]
/// members` carries globs and four packages in this workspace are named
/// something other than their directory (`crates/aterm-libc` is `libc`,
/// `crates/aterm-arrayvec` is `arrayvec`, and so on) — and the NAME is what
/// `--exclude` takes.
///
/// `program` is the driver that was actually spawned (the `$ …` line just
/// above names it), so a refusal never blames a `cargo` that did not run —
/// on a Trust-only box the program is the store's `targo` (2026-09-18).
fn parse_member_names(
    program: &str,
    json: &str,
) -> Result<std::collections::BTreeSet<String>, String> {
    let value = aterm_json::from_str::<aterm_json::Value>(json)
        .map_err(|e| format!("`{program} metadata --no-deps` did not print JSON: {e}"))?;
    let packages = value
        .get("packages")
        .and_then(aterm_json::Value::as_array)
        .ok_or_else(|| format!("`{program} metadata --no-deps` printed no `packages` array"))?;
    let names: std::collections::BTreeSet<String> = packages
        .iter()
        .filter_map(|p| p.get("name"))
        .filter_map(aterm_json::Value::as_str)
        .map(str::to_string)
        .collect();
    if names.is_empty() {
        return Err(format!(
            "`{program} metadata --no-deps` listed no workspace members"
        ));
    }
    Ok(names)
}

/// Split the workspace's members into the ones this cell's graph carries and the
/// ones it does not.
///
/// `--workspace --exclude …`, NOT a list of `-p` flags, and that was not a
/// stylistic choice. Cargo re-resolves for a partial `-p` selection, and until
/// 2026-09-25 this workspace could not be resolved for one that held both
/// `aterm-alloc` and `aterm-gpu`: the differential oracle pins `arrayvec =
/// "=0.7.7"` as a dev-dep while `[patch.crates-io]` points every other consumer
/// at the first-party 0.7.8, and the two could not coexist in one partial
/// resolve — measured, `error: failed to select a version for arrayvec … all
/// possible versions conflict with previously selected packages`. The cause is
/// cargo's patch alias (a patch first reached through a crates-io edge also
/// claims crates-io's semver slot, where the oracle already sits), and the fix
/// is `aterm-alloc`'s dev-dependency on the shim, which activates it through its
/// own source first — `crates/aterm-alloc/Cargo.toml` has the whole account.
/// Both forms below now resolve; the `--exclude` form and the patched-member
/// rule stay, because they are right on their own terms.
///
/// AND `--exclude` DOES TOUCH WHAT WAS RESOLVED, which this comment used to
/// deny. Excluding a workspace member that a `[patch.crates-io]` entry POINTS AT
/// takes it out of the roots, so the first edge to reach it is a crates-io one —
/// which reproduced the identical arrayvec conflict the `-p` form was avoided
/// for (the same mechanism; it no longer fires for `arrayvec` since the shim
/// edge above). Measured 2026-09-18 on m17-tower, with the `mac-x64` cell's own
/// exclude list:
///
/// ```text
/// $ cargo +stable check --target x86_64-apple-darwin --all-targets --workspace \
///       --exclude arrayvec …
/// error: failed to select a version for `arrayvec`.
///     ... required by package `wgpu v29.0.3`
///   previously selected package `arrayvec v0.7.7`
///     ... which satisfies dependency `arrayvec_upstream = "=0.7.7"` … of package `aterm-alloc`
/// ```
///
/// The same command with `arrayvec` LEFT IN resolved and compiled. It was the
/// Apple cells that tripped it, because `wgpu` is in no Darwin graph while
/// `aterm-bench` still drags it into the WORKSPACE resolve — so the patched
/// crate is outside the cell's graph, gets excluded, and the patch goes with it.
/// `mac-x64` was the first cell to meet it: `mac-arm` is native on the boxes
/// that run it, and a native cell is exempt from this pass.
///
/// So the third input: a member a patch POINTS AT is never excluded. It is then
/// compiled for the cell's triple as well, which costs one small crate and is
/// owed nothing — `selected` (and therefore the pass's `owed` list) is still
/// exactly the members this cell's graph carries.
fn cell_test_selection(
    members: &std::collections::BTreeSet<String>,
    graph: &std::collections::BTreeSet<String>,
    patched: &std::collections::BTreeSet<String>,
) -> (Vec<String>, Vec<String>) {
    let selected: Vec<String> = members.intersection(graph).cloned().collect();
    let excluded: Vec<String> = members
        .difference(graph)
        .filter(|n| !patched.contains(*n))
        .cloned()
        .collect();
    (selected, excluded)
}

/// The `[patch.crates-io]` keys of the root manifest — the names whose source
/// this workspace replaces, and therefore the members [`cell_test_selection`]
/// may not hand to `--exclude`.
///
/// A hand parser rather than a TOML dependency, for the same reason
/// [`parse_cell_policy`] is one: this reads a section of one committed file and
/// a wrong answer is loud (the pass dies on the resolve conflict above, naming
/// the crate). A manifest with NO `[patch.crates-io]` section yields an empty
/// set — a workspace with no patches cannot have this failure — while an
/// unreadable manifest is an error, because "I could not look" must not read as
/// "there are none".
fn patched_crate_names(root: &Path) -> Result<std::collections::BTreeSet<String>, String> {
    let manifest = root.join("Cargo.toml");
    let text = std::fs::read_to_string(&manifest)
        .map_err(|e| format!("could not read {} ({e})", manifest.display()))?;
    let mut out = std::collections::BTreeSet::new();
    let mut inside = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with('[') {
            inside =
                line.starts_with("[patch.crates-io]") || line.starts_with("[patch.\"crates-io\"]");
            continue;
        }
        if !inside || line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, _)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().trim_matches('"').trim();
        if !key.is_empty() {
            out.insert(key.to_string());
        }
    }
    Ok(out)
}

/// Why this cell is NOT owed a test-target pass — `None` when it is.
///
/// TWO EXEMPTIONS, BOTH NARROW, BOTH SAID OUT LOUD IN THE RUN rather than left
/// as a silent `if`.
///
///   * THE HOST CELL. `cargo test` on this box already compiles every test
///     target for this triple, and `tools/verify.sh` runs it on every change.
///     The hole this pass exists for is a triple that is NOT the box's own,
///     where nothing anywhere compiles a `#[cfg(test)]` line.
///   * THE wasm32 CELLS. `cargo test --target wasm32-unknown-unknown` is not an
///     operation this repo performs — the web renderers run inside Electron —
///     and pulling the test targets in drags every member's DEV-dependencies
///     with them: `criterion` (Rayon: "cannot be used when targeting wasi32"),
///     `getrandom` ("the wasm32-unknown-unknown targets are not supported by
///     default"), `wait-timeout` and `zstd-sys` all fail to build for wasm32 in
///     their own code. Measured 2026-09-16. Owing a pass that upstream crates
///     make impossible would teach the next reader to delete the pass.
fn test_pass_exemption(triple: &str, is_host: bool) -> Option<String> {
    if is_host {
        return Some(format!(
            "it is this box's own triple, so `cargo test` already compiles every test target for \
             {triple}"
        ));
    }
    if triple.starts_with("wasm32") {
        return Some(format!(
            "`cargo test --target {triple}` is not an operation this repo performs, and the test \
             targets drag in dev-dependencies (criterion/Rayon, getrandom, wait-timeout, zstd-sys) \
             that do not build for wasm32 in their own code"
        ));
    }
    None
}

/// What one cell's TEST-TARGET pass produced.
struct TestPass {
    /// Workspace members in this cell's graph that produced a test artifact for
    /// the cell's triple.
    checked: std::collections::BTreeSet<String>,
    /// Workspace members in this cell's graph, minus proc macros: the members
    /// this pass OWES a test artifact for.
    owed: Vec<String>,
    /// Rendered `error` diagnostics, by package.
    errors: Vec<(String, String)>,
    /// Cargo-level errors (a rejected flag, a dead build script, a missing std).
    cargo_errors: Vec<String>,
    secs: u64,
}

/// What [`run_cell_test_pass`] needs: everything the library pass already
/// resolved for this cell, so the two passes cannot disagree about the
/// toolchain, the target dir or the build-script overrides in force.
#[derive(Clone, Copy)]
struct TestPassJob<'a> {
    root: &'a Path,
    cell: &'a aterm_forge::model::Cell,
    /// Is this the box's own triple? Decides the whole command shape — see
    /// [`cell_check_command`].
    is_host: bool,
    /// The rustup toolchain the library pass selected for a CROSS cell;
    /// `None` for the host cell, which has no rustup toolchain to name (the
    /// host driver IS the pin).
    toolchain: Option<&'a RustupToolchain>,
    target_dir: &'a Path,
    /// The `--config` build-script overrides this cell's `cshim` rows put in
    /// force, verbatim: a second pass compiling the same graph without them
    /// would die on the same C build scripts the first pass was excused from.
    shim_args: &'a [String],
    members: &'a std::collections::BTreeSet<String>,
    graph_names: &'a std::collections::BTreeSet<String>,
    proc_macros: &'a std::collections::BTreeSet<String>,
}

/// Type-check EVERY TARGET — lib, bins, tests, benches, examples — of every
/// workspace member in this cell's graph, for the cell's own triple.
///
/// THE HOLE THIS CLOSES, measured rather than argued. The pass above checks
/// `-p <cell root package>` without `--all-targets`, so no compiler any cell
/// ran had ever read a `#[cfg(test)]` line for a triple that is not the box's
/// own. Three defects landed in that gap in four days, each found by hand and
/// each invisible to every gate in the tree: `atpkg`'s unit-test binary did not
/// BUILD off macOS (commit 693f1c331), `aterm-gui`'s did not build for Windows
/// (b2c2bb53d), and `aterm-verify`'s — the merge gate's own crate, which ships
/// inside `aterm-cli` — did not either. A `#[cfg(test)]` module is one
/// compilation unit with the crate, so ONE unix-only name in a fixture costs
/// the whole crate's test binary on every other target, not one test.
fn run_cell_test_pass(job: &TestPassJob<'_>) -> Result<TestPass, String> {
    let TestPassJob {
        root,
        cell,
        is_host,
        toolchain,
        target_dir,
        shim_args,
        members,
        graph_names,
        proc_macros,
    } = *job;
    let patched = patched_crate_names(root)?;
    let (selected, excluded) = cell_test_selection(members, graph_names, &patched);
    // A SELECTION THAT CAME OUT EMPTY IS A FAILURE, NOT A GREEN PASS. If the
    // member list or the graph ever stopped lining up — a rename, a resolver
    // change — `--workspace` minus every member is a cargo run that compiles
    // nothing and exits 0, which is precisely the shape of vacuity this whole
    // verb exists to refuse.
    if selected.is_empty() {
        return Err(format!(
            "no workspace member is in cell `{}`'s graph, so the test-target pass would compile \
             nothing and pass. The member list ({} names) and the graph ({} packages) do not \
             overlap at all.",
            cell.name,
            members.len(),
            graph_names.len()
        ));
    }
    let started = std::time::Instant::now();
    // The same cwd rule as the library pass: the host cell IN-TREE, so
    // `.cargo/config.toml`'s opt-out reaches every unit; a cross cell from a
    // neutral cwd, so that Trust-only table never reaches a stable compiler.
    let neutral = if is_host {
        None
    } else {
        Some(
            neutral_build_cwd("cells-tests")
                .ok_or_else(|| "could not create the neutral build cwd".to_string())?,
        )
    };
    let cwd = neutral.clone().unwrap_or_else(|| root.to_path_buf());
    let mut cmd = cell_check_command(is_host, toolchain, &cell.triple);
    cmd.current_dir(&cwd)
        .env("CARGO_TARGET_DIR", target_dir)
        .arg("--locked")
        .arg("--keep-going")
        .arg("--message-format=json")
        .arg("--all-targets")
        .arg("--workspace")
        .arg("--manifest-path")
        .arg(root.join("Cargo.toml"));
    for name in &excluded {
        cmd.arg("--exclude").arg(name);
    }
    for arg in shim_args {
        cmd.arg("--config").arg(arg);
    }
    let out = cmd.output();
    if let Some(d) = &neutral {
        let _ = std::fs::remove_dir_all(d);
    }
    let out = out.map_err(|e| format!("could not run the cell's cargo driver ({e})"))?;
    let secs = started.elapsed().as_secs();

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    // Host artifacts land in `target/debug/deps`, not under a triple directory.
    let filter = if is_host {
        None
    } else {
        Some(cell.triple.as_str())
    };
    let mut checked: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut errors: Vec<(String, String)> = Vec::new();
    for line in stdout.lines() {
        let Ok(value) = aterm_json::from_str::<aterm_json::Value>(line) else {
            continue;
        };
        let reason = value.get("reason").and_then(aterm_json::Value::as_str);
        let pkg = value
            .get("package_id")
            .and_then(aterm_json::Value::as_str)
            .map(package_id_name)
            .unwrap_or("<unknown>")
            .to_string();
        match reason {
            Some("compiler-artifact") => {
                // `profile.test` is what separates a TEST target's unit from
                // the lib unit the first pass already counted. Without it a
                // crate whose tests failed to compile would still be "checked"
                // here on the strength of its library, and this pass would
                // measure exactly what the pass above it measures.
                let is_test = value
                    .get("profile")
                    .and_then(|p| p.get("test"))
                    .and_then(aterm_json::Value::as_bool)
                    .unwrap_or(false);
                let files: Vec<String> = value
                    .get("filenames")
                    .and_then(aterm_json::Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(aterm_json::Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                if is_test && artifact_is_for(filter, &files) && graph_names.contains(&pkg) {
                    checked.insert(pkg);
                }
            }
            Some("compiler-message") => {
                let msg = value.get("message");
                let level = msg
                    .and_then(|m| m.get("level"))
                    .and_then(aterm_json::Value::as_str);
                if level == Some("error") {
                    let rendered = msg
                        .and_then(|m| m.get("rendered"))
                        .and_then(aterm_json::Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    errors.push((pkg, rendered));
                }
            }
            _ => {}
        }
    }
    // Same column-zero rule as the pass above, and for the same reason: an
    // excused build script re-emits its own output INDENTED under `Caused by:`,
    // and `error: could not compile` is cargo's summary of diagnostics already
    // counted through the JSON stream.
    let cargo_errors: Vec<String> = stderr
        .lines()
        .filter(|l| l.starts_with("error") && !l.starts_with("error: could not compile"))
        .map(str::to_string)
        .collect();

    let owed: Vec<String> = selected
        .into_iter()
        .filter(|n| !proc_macros.contains(n))
        .collect();
    Ok(TestPass {
        checked,
        owed,
        errors,
        cargo_errors,
        secs,
    })
}

// ---------------------------------------------------------------------------
// THE CROSS-TRIPLE COMPILE THAT IS ALWAYS ON
// ---------------------------------------------------------------------------

/// THE TRIPLES SOME BOX IN THIS FLEET RUNS NATIVELY. A claim about MACHINES,
/// not about targets, and the whole reason [`gate_cells_foreign`] can run on
/// every tier of the merge gate on a day when `cells` itself cannot.
///
/// A cell whose triple somebody HOSTS is a cell whose `gate cells` verdict
/// depends on who is asking — measured on 2026-09-17, not reasoned about:
///
///   * its `cshim` rows are DECLINED by the box that hosts the triple
///     (`host_shim_refusal`) and applied by every other box, so a host run and
///     a cross run of the same cell do not reach the same packages;
///   * its `floor` row is whatever the box that recorded it could count, and a
///     HOST run counts artifacts — proc macros, host-only build deps — that a
///     CROSS run cannot produce at all.
///
/// Both halves are live on this tree TODAY, which is why this const exists
/// rather than a comment saying "should be fine". `gate cells` is GREEN on the
/// macOS boxes, where `mac-arm` is native, and RED on m17-tower, where it is the
/// cross cell and no policy row covers `aarch64-apple-darwin`:
/// `mac-arm  aarch64-apple-darwin  …  90  121  58/76  …  BUILD-SCRIPT(ring)` —
/// `ring` and `zstd-sys` die in cc-rs for want of an Apple SDK and take eighteen
/// of aterm's own crates down with them. In the same run the `linux` cell, whose
/// floor of 230 was recorded as a CROSS cell on the Mac, reached 266 as the HOST
/// cell here and printed `gained coverage … raise it by hand` — and raising it
/// would fail the Mac. A gate that cannot pass on a whole platform for ANY
/// input is a verdict that carries no information, which is the disease
/// [`LaneVerdict`] was written to end.
///
/// So the always-on subset is the cells NO box hosts, and this const is the half
/// of that sentence no compiler can derive. It is checked from whichever machine
/// runs the tests, by `no_cell_this_box_runs_natively_is_in_the_always_on_subset`:
/// a box whose own `rustc -vV` host triple is a cell's triple and is missing
/// here FAILS that test rather than quietly making this gate mean something
/// else.
///
/// `x86_64-apple-darwin` joined on 2026-09-19, by that test doing exactly its
/// job. `mac-x64` was not a cell until `fa57c6f97` (`the six triples atpkg
/// publishes are the six a compiler reads`) added it with `linux-arm` and
/// `win-arm`, and the fleet's Intel Mac HOSTS that triple — so from the day the
/// cell arrived, `cells-foreign` was cross-compiling on the other boxes a cell
/// this one builds natively, which is the host-vs-cross divergence the two
/// bullets above describe. Measured on that box (macOS 13.7.8, 4-core x86_64) in
/// the merge gate's own test stage: the test failed naming this const and the
/// triple to add. The subset it leaves is `win`, `wasm-cpu`, `wasm-gpu`,
/// `linux-arm` and `win-arm` — still non-empty, and still carrying the Windows
/// and wasm32 cells that are the gate's reason to exist.
const FLEET_HOST_TRIPLES: &[&str] = &[
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-unknown-linux-gnu",
];

/// The cells [`gate_cells_foreign`] compiles: every forge cell whose triple is
/// in nobody's [`FLEET_HOST_TRIPLES`]. DERIVED from
/// [`aterm_forge::resolve::default_cells`] rather than typed out, so a sixth
/// cell joins the always-on set on the day it joins the matrix — a hand-typed
/// list is exactly how this file's own header came to say "the four" while the
/// dispatch already said five.
fn foreign_cells() -> Vec<aterm_forge::model::Cell> {
    aterm_forge::resolve::default_cells()
        .into_iter()
        .filter(|c| !FLEET_HOST_TRIPLES.contains(&c.triple.as_str()))
        .collect()
}

/// `xtask gate cells-foreign` — the cross-triple compile every tier of the
/// merge gate runs (`StageId::ForeignCells` in `crates/aterm-verify`).
///
/// THE HOLE THIS FILLS. `cells` was the only thing in this tree that compiles
/// aterm for a triple the box is not, and it was OPT-IN: `tools/verify.sh
/// --full` and nothing else, while nothing automatic runs `--full`. The bill
/// came on 2026-09-16, as four commits in one day — `aterm-gui` did not compile
/// for Windows at all (36c0d4c44, 0f4e44d25), `atpkg` neither, and its unit
/// tests did not build off macOS (693f1c331), and `aterm-verify`'s test target
/// could not BUILD for Windows (b436bd4ff) — every one of them found by a human
/// who went looking, none by a gate, while every other gate in the tree called
/// the tree green. 36c0d4c44's own body names the mechanism in one sentence:
/// "From a Unix box the build is clean, which is exactly why it landed".
///
/// WHAT IT RUNS, AND WHY NOT THE WHOLE MATRIX. [`foreign_cells`] — `win`,
/// `wasm-cpu`, `wasm-gpu`, `linux-arm` and `win-arm` on today's matrix (the
/// first three until 2026-09-18): the cells no box in this fleet runs
/// natively, which is exactly the set whose verdict is the SAME wherever it is
/// run. `mac-arm`, `mac-x64` and `linux` stay behind the opt-in `cells` verb
/// because each is native on some box — where the ordinary test and tippy
/// lanes and the shipped release already compile it — and cross on another,
/// and the gate's verdict for them differs between the two. The two
/// mechanisms, and the red one of them is in right now, are in
/// [`FLEET_HOST_TRIPLES`].
///
/// THE COST, MEASURED, because "it cross-checks several triples" was the entire
/// argument for keeping it out. m17-tower (Ryzen 9900X3D, 24 threads, x86_64
/// Linux), load average ~6, the debug `xtask` binary, `$ATERM_CELL_TARGET_DIR`
/// outside the repo, `/usr/bin/time -p`, 2026-09-17, when the matrix had five
/// cells and this subset three:
///
/// ```text
///   RUN                      COLD      WARM (no edit)          AFTER ONE CORE EDIT
///   gate cells (5 cells)    235.7 s    2.10 / 2.15 / 2.25 s    55.7 s
///   gate cells-foreign (3)   90.5 s    1.21 / 1.24 s           16.6 s
/// ```
///
/// RE-MEASURED 2026-09-27 with the subset's FIVE cells, on m7 (M5 Max) at load
/// average 40-65 from other sessions — wall seconds, each run into its own
/// `$ATERM_CELL_TARGET_DIR`, and `CARGO_INCREMENTAL` both ways because
/// `tools/verify.sh`'s children run with it off:
///
/// ```text
///   RUN                              COLD     WARM (no edit)   AFTER ONE CORE EDIT   DISK
///   cells-foreign (5), incremental   394 s    —                75 s                  12.1 GiB
///   cells-foreign (5), CARGO_INC=0   432 s    8 s              278 s                  2.6 GiB
/// ```
///
/// Three quarters of the incremental cache is `incremental/`. The 2026-09-17
/// row and these are not comparable box to box; the load here was 7-10x.
///
/// ("one core edit" is `touch crates/aterm-grid/src/lib.rs`, a crate most of the
/// tree sits above.) The 39 s the subset drops is almost all the HOST cell, and
/// the host cell is the one that buys the least: its triple is the one every
/// other stage of `tools/verify.sh` already compiles, and it pays for it a
/// second time because this verb never shares the repo's `target/`.
///
/// THE OTHER COST, since it is not seconds. The three cells of 2026-09-17 left
/// 5.0 GB of target directories behind (win 3.6 G — it is the one owed the
/// test-target pass — wasm-gpu 824 M, wasm-cpu 620 M); the five of 2026-09-27
/// leave 12.1 GiB with incremental on and 2.6 GiB with it off (the table
/// above). That used to land in the system temp
/// dir, which is a RAM-backed tmpfs on this box, and this paragraph used to tell
/// the reader to point `$ATERM_CELL_TARGET_DIR` at a disk themselves — which is
/// how a `/tmp/aterm-cells` reached 13.6 GB here and took the swap with it.
/// [`cell_target_dir`] now defaults to the disk-backed cache dir, so the cache
/// lands on a disk without anyone being told to arrange it.
///
/// WHAT IT STILL DOES NOT CATCH, said here rather than left for a reader to
/// discover. A break visible only on `aarch64-apple-darwin` or only on
/// `x86_64-unknown-linux-gnu` reaches this gate on NO box, because those two
/// cells are not in it; they are covered by the native lanes of whichever box
/// hosts them, and by `cells` under `tools/verify.sh --full`. What is new is
/// that Windows and wasm are compiled on EVERY box, on every gate run, instead
/// of on whoever remembered to type `--full`.
///
/// AND WHO RUNS IT: `StageId::ForeignCells` in `crates/aterm-verify`'s FAST
/// plan, decided 2026-09-25 under the owner's standing direction on the
/// 2026-09-17 numbers;
/// its real cost is the `CARGO_INC=0` row above (8 s when nothing changed,
/// 278 s after a core edit on a loaded M5 Max, 2.6 GiB cold). A snapshot's run
/// builds into the snapshot's own cells lane beside it
/// (`aterm_verify::disk::cells_lane`), which the verify disk preflight counts
/// and caps, not into the per-checkout cache under `~/.cache/aterm/cells`.
/// It needs rustup's `stable` with the four foreign std targets; without them
/// this verb exits 0 naming the missing std and the verify stage is a SKIP.
fn gate_cells_foreign() -> bool {
    let cells = foreign_cells();
    // FAIL-CLOSED, and this is not a theoretical arm: `gate_cells` reads an
    // EMPTY selection as THE WHOLE MATRIX, so a [`FLEET_HOST_TRIPLES`] grown to
    // cover every cell would turn this gate into the whole-matrix run it was
    // split out of, on every tier, silently.
    if cells.is_empty() {
        eprintln!(
            "gate cells-foreign: FAILED — FLEET_HOST_TRIPLES now names every cell's triple, so \
             this gate has no cell left to compile. It refuses rather than calling `gate cells` \
             with an empty selection: no `--cell` means THE WHOLE MATRIX, and a subset that \
             silently becomes the matrix is the opposite of the decision this gate records."
        );
        return false;
    }
    eprintln!(
        "=== gate cells-foreign ({} cell(s) no box in this fleet runs natively: {}) ===",
        cells.len(),
        cells
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let mut args: Vec<String> = Vec::new();
    for cell in &cells {
        args.push("--cell".to_string());
        args.push(cell.name.clone());
    }
    gate_cells(&args)
}

/// THE MATRIX AUDIT — the half of `gate cells` that needs no compiler, asks its
/// question of forge's WHOLE cell list, and therefore answers the same on a
/// narrowed run: every cell has a `floor` row, every `floor` row still has a
/// cell, and no cell has two.
///
/// It is a function rather than three loops inside a 900-line verb because a
/// check nobody has ever seen fail is a check nobody has verified. Inline, the
/// only way to drive it was to compile five triples; out here
/// `a_shrunken_matrix_or_a_missing_floor_fails_the_cells_audit` plants all three
/// violations and asserts a RED verdict in `targo --unverified test -p xtask`.
///
/// Returns one line per violation — empty means discharged.
fn cell_matrix_audit(
    all_cells: &[aterm_forge::model::Cell],
    floors: &[CoverageFloor],
) -> Vec<String> {
    let mut out = Vec::new();
    // THE DIRECTION A SHRUNKEN MATRIX WALKS THROUGH, and it used to be
    // narrow-sensitive: "which cells have no floor row" was asked inside the
    // per-cell loop, so a run narrowed with `--cell linux` never noticed that
    // `wasm-cpu` had lost its floor — exit 0, nothing said. A judge measured
    // exactly that. Both lists are known in full here regardless of narrowing.
    for cell in all_cells {
        if !floors.iter().any(|f| f.cell == cell.name) {
            out.push(format!(
                "gate cells: FAILED — {CELL_GATE_POLICY} records no `floor` row for cell `{}`, \
                 which `aterm_forge::resolve::default_cells()` carries. A cell with no floor can \
                 shrink to nothing without the gate saying a word, and a narrowed run must not be \
                 able to hide that. Add:\n\
                 gate cells:   floor\t{}\t<packages>\t<why this is the number>",
                cell.name, cell.name
            ));
        }
    }
    for floor in floors {
        if !all_cells.iter().any(|c| c.name == floor.cell) {
            out.push(format!(
                "gate cells: FAILED — {CELL_GATE_POLICY} records a floor of {} for cell `{}`, \
                 which `aterm_forge::resolve::default_cells()` no longer has. Either the cell was \
                 dropped from the matrix — say so by deleting the row, in the same commit — or \
                 the row is a typo. A floor with no cell is a shrunken matrix passing silently.",
                floor.packages, floor.cell
            ));
        }
    }
    // Two rows for the same cell would make `floor()` (a `find`) silently honour
    // the first and ignore the second, so the lower number could be hidden
    // behind the higher one.
    for (i, floor) in floors.iter().enumerate() {
        if floors[..i].iter().any(|f| f.cell == floor.cell) {
            out.push(format!(
                "gate cells: FAILED — {CELL_GATE_POLICY} records more than one `floor` row for \
                 cell `{}`. Only the first is ever read; delete the others.",
                floor.cell
            ));
        }
    }
    out
}

/// `xtask gate cells` — every forge cell, type-checked BY A COMPILER for its own
/// target triple.
///
/// THE HOLE THIS FILLS, in the words of the judge who found it. aterm resolves
/// five cells; this machine's default toolchain compiles one. Reviewing the
/// `once_cell` row on 2026-09-01 that judge wrote: "the linux, win, wasm-cpu and
/// wasm-gpu cells cannot be COMPILED here — no cross std is installed — so ten
/// of the thirteen consumers are held by source reading and by
/// tests/consumers.rs, never by a type checker. BOTH DEFECTS ABOVE LIVED IN
/// EXACTLY THAT GAP." Cargo resolved all five cells offline and `forge`
/// measured all five, and nothing compiled any of them: every claim about the
/// other four was a claim about a graph, not about a program.
///
/// THE PREMISE THAT TURNED OUT TO BE FALSE is the reason this verb can exist.
/// "No cross std is installed" was true of the toolchain `rust-toolchain.toml`
/// pins — the Trust fork's stage2 sysroot carries `aarch64-apple-darwin` and
/// nothing else — and false of the box: `1.95.0` carries std for both Linux
/// triples, both `windows-msvc` triples and `wasm32-unknown-unknown`. So the
/// four cells were never uncheckable; they were unchecked.
///
/// WHAT EACH CELL RUNS. The cell list is [`aterm_forge::resolve::default_cells`]
/// itself — not a copy — so the thing that is measured and the thing that is
/// compiled cannot drift into disagreeing about what a cell is. Each cell's
/// ROOT PACKAGE is checked, which is what makes the FEATURE set exact: it is
/// cargo's own resolution for that root on that triple, not a hand-assembled
/// package list whose members would each arrive with their default features.
///
///   * The HOST cell runs on the repo's own pinned toolchain from the repo root
///     and passes NO `--target`, because `.cargo/config.toml`'s corollary is
///     explicit that pinning `--target` to the native triple makes cargo
///     withhold the `-Ztrust-verify=off` table from host units, which then
///     verify strictly and fail. It is also the only cell that CANNOT ride the
///     upstream toolchain: `crates/trust-gate`'s build script refuses a non-Trust
///     compiler when HOST == TARGET, measured here as a hard stop 16 s into a
///     `+1.95.0` run.
///   * The four CROSS cells run on a toolchain that carries the triple, from a
///     NEUTRAL cwd — cargo discovers config by walking the cwd upward, and
///     `-Ztrust-verify=off` is a flag only Trust understands, so an in-repo cwd
///     kills an upstream cross build at flag-parse on its first unit. Same
///     reason, same trick, as `tools/wasm-bench/run.sh`.
///
/// NOTHING IS WRITTEN INSIDE THE REPO. The cells never build into
/// `<repo>/target`, and the verb refuses to start if `$ATERM_CELL_TARGET_DIR`
/// points inside the workspace. `--locked`
/// keeps `Cargo.lock` untouched, and the policy file is read-only to this gate.
///
/// THE C DEPENDENCIES, AND WHY THEY ARE NO LONGER AN EXCUSE. Two dependencies
/// bundle C and build it with `cc-rs` — `ring` and `zstd-sys` — and this box has
/// no cross C toolchain, so on the Linux and Windows cells their build scripts
/// die before any Rust is read, taking their whole upward closure with them
/// (`zstd -> aterm-scrollback -> aterm-core`, and `ring -> rustls`). Until
/// 2026-09-01 both were EXCUSED by name from [`CELL_GATE_POLICY`], and the cost
/// of that excuse was not visible in the number the verb printed: `linux
/// 206/253` was at its floor, GREEN, and had not read one line of `aterm-gui`,
/// `aterm-core`, `atpkg` or the other fifteen first-party crates above the
/// engine — where 1,164 of the workspace's 1,910 `windows`/`linux`/`unix` `cfg`
/// sites live. A bare `E0308` planted under `#[cfg(target_os = "linux")]` in
/// `crates/aterm-gui/src/control.rs` left this verb GREEN at exit 0, and the
/// same under `#[cfg(windows)]` likewise.
///
/// They are SHIMMED now instead. A `cshim` row hands cargo its own build-script
/// override for that package's `links` key on ONE triple, passed with `--config`
/// on the command line so nothing is written and nothing can leak, and cargo
/// skips the script and type-checks the Rust. That is only honest where the
/// script emits nothing a compiler reads — no `rustc-cfg`, no `rustc-env`, no
/// generated source — which is true of both of these and is written out, with
/// line numbers, in the rows themselves. `shim_refusal` hard-refuses the two
/// triples where it is false (`zstd-sys` emits `rustc-cfg=feature="std"` for
/// wasm32 and hermit). Both plants go RED now, and linux went 206 -> 230, win
/// 123 -> 147, with every remaining unreached package on both a host-only proc
/// macro or a build dep of one.
///
/// A row naming THE READER'S OWN TRIPLE is a third case, and it is answered per
/// cell rather than per file (`host_shim_refusal`): that cell declines the
/// override and compiles with the real build script, every other cell keeps
/// its overrides, and the row is live rather than dead. Refusing it at parse
/// time instead — which this verb did until the rows for a cross Linux cell
/// met a Linux host — took down `--cell win` along with it.
///
/// TWO PASSES PER CELL SINCE 2026-09-16, and the second is the one this gate
/// had been missing. Pass one checks `-p <cell package>` — libraries, the cell's
/// root binary, and everything beneath them. Pass two checks `--workspace
/// --all-targets` minus the members this cell's graph does not carry, so every
/// workspace member's TESTS, benches and examples are read by a compiler for the
/// cell's triple too. Until it existed, no compiler anywhere read a
/// `#[cfg(test)]` line for a triple that was not the box's own, and three crates
/// shipped test binaries that did not BUILD off the author's machine — `atpkg`
/// off macOS, `aterm-gui` and `aterm-verify` for Windows — each found by hand,
/// none by a gate. `test_pass_exemption` names the two cells that are not owed
/// the pass and why, in the run's own output.
///
/// WHAT IT STILL DOES NOT COVER, said out loud in the verdict rather than left
/// to a reader. A shimmed cell TYPE-CHECKS; it never links the bundled C — nor
/// does any other cell, because `cargo check` does not link — and no cell runs a
/// binary, so cross-compiler codegen, C ABI breakage and every runtime behaviour
/// are out of reach. The cells are rooted at `aterm`, `aterm-wasm` and
/// `aterm-gpu-web`, so a crate in no cell's graph (`aterm-release`,
/// `atpkg-keys`, `aterm-conformance`, `aterm-nest`, `aterm-effects-web`, …) is
/// not covered here at all. That is a scope statement, not a hole: those are
/// host-only publisher, key and conformance crates which SHIP to no triple —
/// the release cutter refuses to run anywhere but a macOS arm64 host
/// (`aterm_release::gates`) — and the workspace test stage compiles every one
/// of them on whatever box runs it (`cargo … --workspace`, see
/// `aterm_verify::stages`). What the matrix owes is the six triples aterm's
/// artifacts are PUBLISHED for, and since 2026-09-18 it carries all six.
/// Measured on 2026-09-01, over the 3,245 platform
/// `cfg` attribute sites under `crates/`: 2,143 were reachable by some cell
/// before this change and 2,894 after — of the 351 that remain, 232 are in
/// crates no cell's graph carries and 119 are predicates no cell's triple can
/// satisfy (BSD/Android arms, `not(any(unix, windows))` fallbacks, and the
/// `all(test, not(target_arch = "wasm32"))` guards in the two wasm crates).
fn gate_cells(rest: &[String]) -> bool {
    let mut wanted: Vec<String> = Vec::new();
    let mut args = rest.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--cell" => match args.next() {
                Some(name) => wanted.push(name.clone()),
                None => {
                    eprintln!("gate cells: `--cell` needs a cell name.");
                    return false;
                }
            },
            other => {
                eprintln!(
                    "gate cells: unknown argument `{other}`.\n\
                     usage: xtask gate cells [--cell NAME]…   (repeatable; no --cell means all five)"
                );
                return false;
            }
        }
    }

    let root = workspace_root();
    let policy_path = root.join(CELL_GATE_POLICY);
    let policy = match std::fs::read_to_string(&policy_path).map_err(|e| e.to_string()) {
        Ok(text) => match parse_cell_policy(&text) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("gate cells: COULD NOT RUN — {e}");
                return false;
            }
        },
        Err(e) => {
            eprintln!(
                "gate cells: COULD NOT RUN — cannot read {}: {e}",
                policy_path.display()
            );
            return false;
        }
    };
    cells_under_policy(&root, &policy, &wanted)
}

/// The verb's whole body once [`CELL_GATE_POLICY`] is read — split out (2026-09-24) so
/// a fixture can drive the verb, compile and verdict included, under a policy it
/// states: `a_foreign_cell_under_its_floor_fails_the_cells_verb`.
fn cells_under_policy(root: &Path, policy: &CellPolicy, wanted: &[String]) -> bool {
    let root = root.to_path_buf();

    // THE WORKSPACE MEMBER LIST, read once and shared by every cell: the
    // test-target pass selects `--workspace` minus the members a cell's graph
    // does not carry, and `--exclude` takes package NAMES, not directories.
    // Through the host driver (crate::driver) — `metadata` takes no lane flag
    // from targo, and the driver knows that. Announced once, with its source,
    // so the log says which toolchain read the manifest. Exported as `$CARGO`
    // too, because forge's per-cell `cargo tree` (step 1 below) reads that and
    // nothing else.
    let driver = export_driver_as_cargo();
    let metadata_args = [
        "--no-deps",
        "--locked",
        "--offline",
        "--format-version",
        "1",
    ];
    eprintln!(
        "gate cells: host driver: {} ({})",
        driver.program.display(),
        driver.source
    );
    // A toolchain the discovery REFUSED is skipped by the ladder, never
    // adopted; say so beside the driver that answered instead, so the reader
    // knows why the store rung did not fire (2026-09-18).
    if let Some(why) = &driver.refused {
        eprintln!("gate cells: note — the pinned-toolchain rung was skipped: {why}");
    }
    eprintln!("  $ {}", driver.display("metadata", &metadata_args));
    let members = match driver
        .command("metadata")
        .current_dir(&root)
        .args(metadata_args)
        .output()
    {
        Ok(out) if out.status.success() => {
            match parse_member_names(
                &driver.program.display().to_string(),
                &String::from_utf8_lossy(&out.stdout),
            ) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("gate cells: COULD NOT RUN — {e}");
                    return false;
                }
            }
        }
        Ok(out) => {
            eprintln!(
                "gate cells: COULD NOT RUN — `{} metadata --no-deps` failed:\n{}",
                driver.program.display(),
                String::from_utf8_lossy(&out.stderr)
            );
            return false;
        }
        Err(e) => {
            eprintln!(
                "gate cells: COULD NOT RUN — could not run `{} metadata` ({e}); {DRIVER_REMEDY}.",
                driver.program.display()
            );
            return false;
        }
    };

    let all_cells = aterm_forge::resolve::default_cells();
    let selected = match aterm_forge::resolve::select(&all_cells, wanted) {
        Ok(cells) => cells,
        Err(e) => {
            eprintln!("gate cells: {e}");
            return false;
        }
    };
    let narrowed = !wanted.is_empty();
    // INTENT, IN THE PRESENT TENSE. This banner is printed before a compiler
    // has been asked for anything, and it used to read "N of forge's M cells
    // type-checked for their own triples" — the past tense, at the top of a run
    // that could still skip every one of them, and the only other place in this
    // file that spelled [`MATRIX_CLAIM`].
    eprintln!(
        "=== gate cells (type-checking {} of forge's {} cells, each for its own triple) ===",
        selected.len(),
        all_cells.len()
    );

    let host = rustc_host_triple();
    if host.is_none() {
        eprintln!(
            "gate cells: COULD NOT RUN — no compiler on the host ladder ($RUSTC, $CARGO's sibling \
             rustc/trustc, the store's trustc, bare rustc) reported a host triple, so the native \
             cell cannot be told from the cross ones; {DRIVER_REMEDY}."
        );
        return false;
    }
    let host = host.unwrap_or_default();

    let mut reports: Vec<CellReport> = Vec::new();
    let mut fail = false;
    // Every cdep row must describe a package that is REALLY in the graph of a
    // cell it claims to be about, checked across the whole run. Indexed by the
    // row's position so two rows for the same package on two triples are two
    // separate obligations.
    let mut cdep_row_used: Vec<bool> = vec![false; policy.cdeps.len()];
    let mut cshim_row_used: Vec<bool> = vec![false; policy.cshims.len()];

    for cell in &selected {
        let is_host = cell.triple == host;
        let started = std::time::Instant::now();

        // 1. THE GRAPH, from forge's own resolver, so the denominator of the
        //    coverage fraction is the number forge reports and not a second
        //    opinion about it. Offline and compiler-free.
        let mut resolve_log = String::new();
        let (graph, graph_paths) =
            match aterm_forge::resolve::graph_and_paths(&root, cell, &mut resolve_log) {
                Ok(g) => g,
                Err(e) => {
                    eprintln!(
                        "gate cells: FAILED — cell `{}` did not resolve: {e}",
                        cell.name
                    );
                    fail = true;
                    continue;
                }
            };
        eprint!("{resolve_log}");
        let graph_names: std::collections::BTreeSet<String> =
            graph.nodes.iter().map(|p| p.name.clone()).collect();
        for (i, cdep) in policy.cdeps.iter().enumerate() {
            if (cdep.triple == "*" || cdep.triple == cell.triple)
                && graph_names.contains(&cdep.package)
            {
                cdep_row_used[i] = true;
            }
        }

        // 1b. THE CELL'S OWN CODE. `checked/graph` counts PACKAGES, and a
        //     package count is the number a reader will quote while the thing
        //     they care about — whether a compiler read aterm's platform code
        //     for this triple — is invisible inside it. The Linux cell sat at
        //     206/253 with all eighteen of aterm's own compiled crates
        //     UNREACHED, and its verdict line said GREEN. So the in-repo half of
        //     the graph is counted separately and OWED, not merely reported.
        let own: std::collections::BTreeSet<String> = graph
            .nodes
            .iter()
            .filter(|id| {
                graph_paths
                    .get(*id)
                    .is_some_and(|dir| path_is_inside(dir, &root))
            })
            .map(|id| id.name.clone())
            .collect();
        // A PROC MACRO IS NOT OWED A TARGET ARTIFACT. It compiles for the HOST
        // even in a cross build, so `artifact_is_for` correctly refuses to count
        // it, and demanding one would make every cell permanently red.
        let own_proc_macros: std::collections::BTreeSet<String> = graph
            .nodes
            .iter()
            .filter(|id| {
                graph_paths
                    .get(*id)
                    .is_some_and(|dir| path_is_inside(dir, &root) && manifest_is_proc_macro(dir))
            })
            .map(|id| id.name.clone())
            .collect();

        // 1c. THE BUILD-SCRIPT OVERRIDES for this triple, and their liveness.
        let shims = policy.shims_for(&cell.triple);
        let mut shim_args: Vec<String> = Vec::new();
        let mut shim_notes: Vec<String> = Vec::new();
        let mut shimmed: Vec<String> = Vec::new();
        let mut shim_dead = false;
        for (i, shim) in shims {
            let live = graph
                .nodes
                .iter()
                .any(|id| id.name == shim.package && id.version == shim.version);
            if !live {
                fail = true;
                shim_dead = true;
                let present: Vec<String> = graph
                    .nodes
                    .iter()
                    .filter(|id| id.name == shim.package)
                    .map(|id| id.version.clone())
                    .collect();
                eprintln!(
                    "gate cells: FAILED — {CELL_GATE_POLICY} shims `{}@{}` on {}, and this cell's \
                     graph carries {}. A shim is a claim about ONE build script AT ONE VERSION; \
                     re-read the new one and move the row, or delete it.",
                    shim.package,
                    shim.version,
                    cell.triple,
                    if present.is_empty() {
                        "no such package".to_string()
                    } else {
                        format!("`{}` at {}", shim.package, present.join(", "))
                    }
                );
                continue;
            }
            // LIVE, therefore not a dead row — and that verdict is taken
            // BEFORE the host question below, on purpose. A row naming this
            // box's own triple is a row some OTHER box needs; marking it unused
            // here would make the end-of-run liveness audit say "delete the
            // row", and deleting it would put the cross cell that depends on it
            // back behind an uninstallable C toolchain. Satisfied-by-host, not
            // dead.
            cshim_row_used[i] = true;
            // AND NOW THE HOST QUESTION, asked of this cell rather than of the
            // file. Applying an override on the native cell is the failure a
            // judge caught (`SHIMMED … GREEN` with the real build script never
            // run), so the row is declined here — loudly, in the same place the
            // applied ones are announced — and the cell compiles with the real
            // script, which on this box is a script that RUNS.
            if let Some(why) = host_shim_refusal(&cell.triple, &host) {
                shim_notes.push(format!(
                    "gate cells: cell `{}` — `{}@{}`'s `cshim` row NOT APPLIED: {why}. The row \
                     stays in {CELL_GATE_POLICY} for the boxes where this triple is a CROSS cell; \
                     here the build script runs for real.",
                    cell.name, shim.package, shim.version
                ));
                continue;
            }
            // `[target.<triple>.<links>]` with only a link directive. `cargo
            // check` never links, so nothing here is load-bearing for the
            // type-check — which is the entire reason this is honest.
            shim_args.push(format!(
                "target.{}.{}.rustc-link-lib=[\"{}\"]",
                cell.triple, shim.links, shim.links
            ));
            shimmed.push(shim.package.clone());
            // The justification is PRINTED where the override is really applied
            // — just before cargo runs — and not here. A cell can still SKIP
            // between this point and that one (no installed std for its triple),
            // and "SHIMMED …" followed by "nothing was compiled" reads as though
            // the shim bought something on a box where it bought nothing.
            shim_notes.push(format!(
                "gate cells: cell `{}` — `{}@{}`'s build script SHIMMED (cargo skips it; \
                 `--config target.{}.{}`): {}",
                cell.name, shim.package, shim.version, cell.triple, shim.links, shim.why
            ));
        }
        if shim_dead {
            continue;
        }

        // 2. THE TOOLCHAIN. A SKIP is a fact about the BOX, decided before
        //    anything compiles, it says out loud that nothing was compiled, and
        //    it is NOT a pass — see [`CellOutcome`] for the run this sentence
        //    used to end in `GREEN — all 5 cells type-check`.
        //    THE HOST CELL NAMES NO RUSTUP TOOLCHAIN: the host driver is the
        //    pin (see [`cell_check_command`]). Until 2026-09-18 this arm held
        //    the label `repo pin (rust-toolchain.toml)`, which the test-target
        //    pass then exported as a literal `RUSTUP_TOOLCHAIN`.
        let toolchain: Option<RustupToolchain> = if is_host {
            None
        } else {
            match cell_toolchain(&cell.triple) {
                Ok(Some(tc)) => Some(tc),
                Ok(None) => {
                    eprintln!(
                        "gate cells: cell `{}` SKIPPED — no installed toolchain carries a {} std, \
                         so NOTHING WAS COMPILED for it.\n\
                         gate cells:   install one:  rustup target add {} --toolchain stable\n\
                         gate cells:   (`--toolchain` is not optional — this repo's default is the \
                         Trust fork, which refuses `rustup target add`.)",
                        cell.name, cell.triple, cell.triple
                    );
                    reports.push(CellReport::skipped(
                        cell,
                        graph_names.len(),
                        own.difference(&own_proc_macros).count(),
                        own_proc_macros.len(),
                    ));
                    continue;
                }
                Err(e) => {
                    eprintln!("gate cells: FAILED — cell `{}`: {e}", cell.name);
                    fail = true;
                    continue;
                }
            }
        };

        // The TOOLCHAIN column: a cross cell's rustup toolchain name; for the
        // host cell, the driver that IS the pin here (`host driver (targo)`),
        // instead of the old label a reader could mistake for a rustup name.
        let toolchain_label = toolchain
            .as_ref()
            .map(|tc| tc.name.clone())
            .unwrap_or_else(|| {
                format!(
                    "host driver ({})",
                    driver
                        .program
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| driver.program.display().to_string())
                )
            });

        // 3. WHERE IT RUNS AND WHERE IT WRITES. Never inside the repo.
        let target_dir = match cell_target_dir(&root, &cell.name) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("gate cells: FAILED — cell `{}`: {e}", cell.name);
                fail = true;
                continue;
            }
        };
        let cwd = if is_host {
            root.clone()
        } else {
            match neutral_build_cwd("cells") {
                Some(d) => d,
                None => {
                    fail = true;
                    continue;
                }
            }
        };

        let mut cmd = cell_check_command(is_host, toolchain.as_ref(), &cell.triple);
        cmd.current_dir(&cwd)
            .env("CARGO_TARGET_DIR", &target_dir)
            .arg("--locked")
            // Without this, one dead build script hides every type error behind
            // it; with it, the compiler reads everything it still can and the
            // excuse costs only the closure it really blocks.
            .arg("--keep-going")
            .arg("--message-format=json")
            .args(["-p", cell.package.as_str()])
            .arg("--manifest-path")
            .arg(root.join("Cargo.toml"));
        // ON THE COMMAND LINE, NOT IN A FILE. `--config` takes the same table a
        // `.cargo/config.toml` would carry, so the override lives for exactly
        // one process, is visible in the command a reader runs, and cannot be
        // left behind for a real build to pick up. It is also TRIPLE-SCOPED, so
        // even a leaked copy could not touch a native build.
        for arg in &shim_args {
            cmd.arg("--config").arg(arg);
        }
        for note in &shim_notes {
            eprintln!("{note}");
        }
        let out = cmd.output();
        if !is_host {
            let _ = std::fs::remove_dir_all(&cwd);
        }
        let secs = started.elapsed().as_secs();
        let out = match out {
            Ok(o) => o,
            Err(e) => {
                eprintln!(
                    "gate cells: FAILED — cell `{}`: could not run cargo ({e}).",
                    cell.name
                );
                fail = true;
                continue;
            }
        };

        // 4. THE VERDICT, read from cargo's own JSON rather than from its exit
        //    code alone: with an excused build script the exit code is 101 on a
        //    perfectly clean cell, and a run whose units were all fresh prints
        //    no `Checking` line at all while still reporting every artifact.
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let filter = if is_host {
            None
        } else {
            Some(cell.triple.as_str())
        };
        let mut checked: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let mut compile_errors: Vec<(String, String)> = Vec::new();
        for line in stdout.lines() {
            let Ok(value) = aterm_json::from_str::<aterm_json::Value>(line) else {
                continue;
            };
            let reason = value.get("reason").and_then(aterm_json::Value::as_str);
            let pkg = value
                .get("package_id")
                .and_then(aterm_json::Value::as_str)
                .map(package_id_name)
                .unwrap_or("<unknown>")
                .to_string();
            match reason {
                Some("compiler-artifact") => {
                    let files: Vec<String> = value
                        .get("filenames")
                        .and_then(aterm_json::Value::as_array)
                        .map(|a| {
                            a.iter()
                                .filter_map(aterm_json::Value::as_str)
                                .map(str::to_string)
                                .collect()
                        })
                        .unwrap_or_default();
                    if artifact_is_for(filter, &files) && graph_names.contains(&pkg) {
                        checked.insert(pkg);
                    }
                }
                Some("compiler-message") => {
                    let msg = value.get("message");
                    let level = msg
                        .and_then(|m| m.get("level"))
                        .and_then(aterm_json::Value::as_str);
                    if level == Some("error") {
                        let rendered = msg
                            .and_then(|m| m.get("rendered"))
                            .and_then(aterm_json::Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        compile_errors.push((pkg, rendered));
                    }
                }
                _ => {}
            }
        }

        let mut cell_ok = true;
        // THE FIRST reason a cell went red is the one the summary table names.
        // A type error also drops the coverage count (the crate that failed was
        // not checked, nor was anything above it), so an unguarded assignment
        // let COVERAGE-DROP overwrite TYPE-ERROR and the one-line summary
        // reported the symptom instead of the cause.
        let mut status = String::from("GREEN");

        if !compile_errors.is_empty() {
            cell_ok = false;
            status = format!("TYPE-ERROR({})", compile_errors.len());
            eprintln!(
                "\ngate cells: cell `{}` ({}) FAILED — {} type error(s) the host build cannot see:",
                cell.name,
                cell.triple,
                compile_errors.len()
            );
            for (pkg, rendered) in compile_errors.iter().take(20) {
                eprintln!("gate cells:   in `{pkg}`:");
                eprint!("{rendered}");
            }
        }

        // 5. THE EXCUSES. Every cargo-level error must be a build script this
        //    policy names; anything else fails the cell.
        let mut excused: Vec<String> = Vec::new();
        for line in stderr.lines() {
            let Some(pkg) = build_script_failure(line) else {
                continue;
            };
            match policy.excuse(pkg, &cell.triple) {
                Some(cdep) => {
                    if !excused.contains(&cdep.package) {
                        // SAY WHY, HERE, where the excuse is granted. A verdict
                        // that prints a package name and nothing else makes the
                        // reader go and find the policy file to learn whether
                        // the forgiveness was reasonable — which is exactly the
                        // moment nobody does.
                        eprintln!(
                            "gate cells: cell `{}` — `{}` EXCUSED: {}",
                            cell.name, cdep.package, cdep.why
                        );
                        excused.push(cdep.package.clone());
                    }
                }
                None => {
                    cell_ok = false;
                    if status == "GREEN" {
                        status = format!("BUILD-SCRIPT({pkg})");
                    }
                    eprintln!(
                        "\ngate cells: cell `{}` FAILED — `{pkg}`'s build script did not run, and \
                         no `cdep` row in {CELL_GATE_POLICY} excuses it on {}. Either fix the \
                         build, or add a row saying why this box cannot run it.",
                        cell.name, cell.triple
                    );
                }
            }
        }
        // Any OTHER `error:` cargo printed — a rejected flag, a broken
        // manifest, a missing std — is a failure with its own line, never
        // silence. A run that could not start must not read as a clean cell.
        // COLUMN ZERO IS THE WHOLE TEST. Cargo prints its own errors unindented;
        // a failing build script's captured stdout/stderr is re-emitted INDENTED
        // under `Caused by:`. Trimming first made every `  error occurred in
        // cc-rs: …` line inside an already-excused build-script failure into a
        // second, unexcused cargo error, and both cross cells went red for the
        // failure the policy had just excused. `error: could not compile` is
        // cargo's summary of diagnostics already reported through the JSON
        // stream, so counting it would double-report a type error.
        for line in stderr.lines() {
            if line.starts_with("error")
                && build_script_failure(line).is_none()
                && !line.starts_with("error: could not compile")
            {
                cell_ok = false;
                if status == "GREEN" {
                    status = "CARGO-ERROR".to_string();
                }
                eprintln!("gate cells: cell `{}` FAILED — {line}", cell.name);
            }
        }

        // 6. AN EXCUSE THAT WAS NOT NEEDED HERE. A NOTE, deliberately, and not
        //    a failure: an excuse says "THIS BOX cannot run that build script",
        //    and a box that CAN — one carrying cargo-zigbuild, a Linux sysroot
        //    or an MSVC image — is more capable, not out of policy. Failing the
        //    better machine would teach people to uninstall the cross
        //    toolchain, which is the opposite of what this gate is for. The
        //    teeth for this direction are machine-independent and live
        //    elsewhere: the floor below fails a run that checks FEWER packages,
        //    and the dead-row audit at the end fails a row no cell's graph can
        //    justify at all.
        for cdep in &policy.cdeps {
            if (cdep.triple == "*" || cdep.triple == cell.triple) && checked.contains(&cdep.package)
            {
                eprintln!(
                    "gate cells: NOTE — {CELL_GATE_POLICY} excuses `{}` on {}, and it type-checked \
                     here anyway: this box carries what that build script needs. The cell is WIDER \
                     than the policy assumes — raise the floor, and delete the row once no box \
                     needs it.",
                    cdep.package, cell.triple
                );
            }
        }

        // 6b. DID THE SHIM ACTUALLY BUY ANYTHING? A row that skips a build
        //     script and then does not get the package type-checked is worse
        //     than no row: it has spent the reader's trust on nothing. This
        //     fires when the override is mis-keyed (a wrong `links` name is
        //     silently ignored by cargo) as well as when the package failed for
        //     some other reason.
        for pkg in &shimmed {
            if !checked.contains(pkg) {
                cell_ok = false;
                if status == "GREEN" {
                    status = format!("SHIM-INERT({pkg})");
                }
                eprintln!(
                    "gate cells: cell `{}` FAILED — {CELL_GATE_POLICY} shims `{pkg}`'s build \
                     script on {}, and `{pkg}` STILL did not type-check. Check the row's `links` \
                     key against the package's manifest: cargo ignores an override addressed to a \
                     name no package links.",
                    cell.name, cell.triple
                );
            }
        }

        // 6c. THE CELL'S OWN CODE, OWED RATHER THAN REPORTED. Every in-repo
        //     package in this cell's graph that is not a proc macro must have
        //     been read by a compiler for this triple. This is the obligation
        //     that the package-count floor could not express: `linux 206/253`
        //     was at its floor, GREEN, and had not compiled one line of
        //     `aterm-gui`, where 940 of the workspace's platform `cfg` sites
        //     live. Machine-independent on purpose — unlike an excuse, it does
        //     not get easier on a better-equipped box.
        let own_owed: Vec<&String> = own.difference(&own_proc_macros).collect();
        let own_missing: Vec<&&String> =
            own_owed.iter().filter(|n| !checked.contains(**n)).collect();
        let own_checked = own_owed.len() - own_missing.len();
        if !own_missing.is_empty() {
            cell_ok = false;
            if status == "GREEN" {
                status = format!("OWN-CODE-UNREAD({})", own_missing.len());
            }
            eprintln!(
                "gate cells: cell `{}` FAILED — {} of aterm's own {} compiled crates in this \
                 cell's graph were NOT type-checked for {}: {}. A cell that has not read this \
                 repo's code for its own triple has not verified the thing the matrix claims.",
                cell.name,
                own_missing.len(),
                own_owed.len(),
                cell.triple,
                own_missing
                    .iter()
                    .map(|n| n.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }

        // 6d. THE TEST TARGETS. Everything above this line reads LIBRARIES: the
        //     first pass checks `-p <cell package>` with no `--all-targets`, so
        //     for four days running, three separate crates shipped a test
        //     binary that did not COMPILE for a triple nobody here can see, and
        //     every gate in the tree was green through all of it. A
        //     `#[cfg(test)]` module is part of its crate's compilation unit, so
        //     the unit of loss is the crate's whole test binary.
        let test_note = test_pass_exemption(&cell.triple, is_host);
        let (tests_checked, tests_owed) = match &test_note {
            Some(why) => {
                eprintln!(
                    "gate cells: cell `{}` — test targets NOT owed here: {why}.",
                    cell.name
                );
                (0, 0)
            }
            // Only when the library pass is clean: a crate whose lib did not
            // type-check cannot have its tests type-check either, and piling a
            // second copy of the same diagnostics on the reader hides the one
            // that matters.
            None if !cell_ok => {
                eprintln!(
                    "gate cells: cell `{}` — test targets NOT reached: the library pass above \
                     failed, and a test target cannot compile past its own crate.",
                    cell.name
                );
                (0, 0)
            }
            None => match run_cell_test_pass(&TestPassJob {
                root: &root,
                cell,
                is_host,
                toolchain: toolchain.as_ref(),
                target_dir: &target_dir,
                shim_args: &shim_args,
                members: &members,
                graph_names: &graph_names,
                proc_macros: &own_proc_macros,
            }) {
                Err(e) => {
                    cell_ok = false;
                    if status == "GREEN" {
                        status = "TESTS-COULD-NOT-RUN".to_string();
                    }
                    eprintln!("gate cells: cell `{}` FAILED — {e}", cell.name);
                    (0, 0)
                }
                Ok(pass) => {
                    if !pass.errors.is_empty() {
                        cell_ok = false;
                        if status == "GREEN" {
                            status = format!("TEST-TARGET-ERROR({})", pass.errors.len());
                        }
                        eprintln!(
                            "\ngate cells: cell `{}` ({}) FAILED — {} type error(s) in TEST \
                             TARGETS, which no host build and no other gate compiles for this \
                             triple:",
                            cell.name,
                            cell.triple,
                            pass.errors.len()
                        );
                        for (pkg, rendered) in pass.errors.iter().take(20) {
                            eprintln!("gate cells:   in `{pkg}`:");
                            eprint!("{rendered}");
                        }
                    }
                    for line in &pass.cargo_errors {
                        cell_ok = false;
                        if status == "GREEN" {
                            status = "TESTS-CARGO-ERROR".to_string();
                        }
                        eprintln!(
                            "gate cells: cell `{}` FAILED — test-target pass: {line}",
                            cell.name
                        );
                    }
                    // OWED, exactly like 6c and for the same reason: a pass that
                    // quietly stopped selecting a member would otherwise report
                    // a smaller number and stay green.
                    let missing: Vec<&String> = pass
                        .owed
                        .iter()
                        .filter(|n| !pass.checked.contains(*n))
                        .collect();
                    if !missing.is_empty() {
                        cell_ok = false;
                        if status == "GREEN" {
                            status = format!("TESTS-UNREAD({})", missing.len());
                        }
                        eprintln!(
                            "gate cells: cell `{}` FAILED — {} of the {} workspace members in \
                             this cell's graph produced NO test-target artifact for {}: {}. \
                             Either the target was skipped or it did not build; both leave that \
                             crate's `#[cfg(test)]` code unread for this triple.",
                            cell.name,
                            missing.len(),
                            pass.owed.len(),
                            cell.triple,
                            missing
                                .iter()
                                .map(|n| n.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        );
                    }
                    // THE COVERAGE LINE IS ONLY SAID WHEN IT IS TRUE. A member
                    // counts as reached the moment ONE of its test targets
                    // produced an artifact, which is the right question for
                    // vacuity (did the selection shrink?) and the WRONG one for
                    // health: `aterm-verify` reached it on `profile_pin` and
                    // `process_doc` while `environment_contract` and the lib
                    // test were failing to compile beside them. So a run with
                    // errors above says what it found, never a fraction that
                    // reads as clean.
                    if pass.errors.is_empty() && pass.cargo_errors.is_empty() {
                        eprintln!(
                            "gate cells: cell `{}` — test targets: every target (lib, bins, tests, \
                             benches, examples) of all {} workspace members in its graph \
                             type-checked for {} in {}s.",
                            cell.name,
                            pass.owed.len(),
                            cell.triple,
                            pass.secs
                        );
                    } else {
                        eprintln!(
                            "gate cells: cell `{}` — test targets: {} of {} workspace members \
                             produced an artifact for {} in {}s, but the pass is NOT clean — see \
                             the errors above.",
                            cell.name,
                            pass.owed.len() - missing.len(),
                            pass.owed.len(),
                            cell.triple,
                            pass.secs
                        );
                    }
                    (pass.owed.len() - missing.len(), pass.owed.len())
                }
            },
        };

        // 7. THE FLOOR.
        match policy.floor(&cell.name) {
            None => {
                cell_ok = false;
                if status == "GREEN" {
                    status = "NO-FLOOR".to_string();
                }
                eprintln!(
                    "gate cells: cell `{}` FAILED — {CELL_GATE_POLICY} records no `floor` row for \
                     it, so this run's {} checked package(s) can be compared with nothing. Add:\n\
                     gate cells:   floor\t{}\t{}\t<why this is the number>",
                    cell.name,
                    checked.len(),
                    cell.name,
                    checked.len()
                );
            }
            Some(floor) if checked.len() < floor => {
                cell_ok = false;
                if status == "GREEN" {
                    status = format!("COVERAGE-DROP(<{floor})");
                }
                eprintln!(
                    "gate cells: cell `{}` FAILED — {} of its {} packages type-checked, below the \
                     recorded floor of {floor}. Something stopped being compiled for {}; find out \
                     what before lowering the floor.",
                    cell.name,
                    checked.len(),
                    graph_names.len(),
                    cell.triple
                );
            }
            Some(floor) if checked.len() > floor => {
                eprintln!(
                    "gate cells: cell `{}` gained coverage — {} checked, floor {floor}. Raise it \
                     by hand:  floor\t{}\t{}\t<why>",
                    cell.name,
                    checked.len(),
                    cell.name,
                    checked.len()
                );
            }
            Some(_) => {}
        }

        if cell_ok {
            eprintln!(
                "gate cells: cell `{}` ({}) GREEN on `{}` — {}/{} packages type-checked in {}s, \
                 INCLUDING {}/{} of aterm's own compiled crates{}{}.",
                cell.name,
                cell.triple,
                toolchain_label,
                checked.len(),
                graph_names.len(),
                secs,
                own_checked,
                own_owed.len(),
                if own_proc_macros.is_empty() {
                    String::new()
                } else {
                    format!(
                        " ({} proc macro(s) not owed a {} artifact: {})",
                        own_proc_macros.len(),
                        cell.triple,
                        own_proc_macros
                            .iter()
                            .map(String::as_str)
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                },
                if excused.is_empty() {
                    String::new()
                } else {
                    format!(", {} excused: {}", excused.len(), excused.join(", "))
                }
            );
        } else {
            fail = true;
        }
        reports.push(CellReport {
            cell: cell.name.clone(),
            triple: cell.triple.clone(),
            toolchain: toolchain_label,
            checked: checked.len(),
            graph: graph_names.len(),
            excused,
            shimmed,
            own_checked,
            own_total: own_owed.len(),
            own_proc_macros: own_proc_macros.len(),
            tests_checked,
            tests_owed,
            tests_note: test_note,
            // The whole cell — its check AND its test-target pass, which writes
            // into the same target dir — measured once both are done.
            secs: started.elapsed().as_secs(),
            disk: Some(aterm_verify::disk::dir_bytes(&target_dir)),
            status,
            outcome: if cell_ok {
                CellOutcome::Checked
            } else {
                CellOutcome::Failed
            },
        });
    }

    // 8. DEAD POLICY ROWS.
    //
    //    THE `floor` HALF IS NOT NARROW-SENSITIVE, AND THAT MATTERS. A `cdep` or
    //    `cshim` row is audited against the GRAPHS this run resolved, so a
    //    narrowed run genuinely has not looked. A `floor` row is audited against
    //    forge's CELL LIST, which every run holds in full — so it is checked
    //    always, by [`cell_matrix_audit`], which is also the one half of this
    //    verb a test can drive without compiling five triples. That asymmetry is
    //    the point: deleting `wasm-cpu` from `default_cells()` used to leave
    //    `gate cells` printing "GREEN — all 4 cells type-check" with an orphaned
    //    `floor wasm-cpu 54` row nobody mentioned. A verification floor whose own
    //    matrix can be quietly shrunk is not a floor, and the verdict sentence
    //    was the part that was wrong.
    for line in cell_matrix_audit(&all_cells, &policy.floors) {
        fail = true;
        eprintln!("{line}");
    }
    // The `cdep`/`cshim` half IS narrow-sensitive: a run that skipped a cell has
    // not resolved the graph a row might be justified by.
    if narrowed {
        eprintln!(
            "gate cells: NOTE — narrowed to {}; the `cdep` and `cshim` rows were not audited \
             against the cells this run skipped.",
            wanted.join(", ")
        );
    } else {
        for (i, cdep) in policy.cdeps.iter().enumerate() {
            if !cdep_row_used[i] {
                fail = true;
                eprintln!(
                    "gate cells: FAILED — {CELL_GATE_POLICY} excuses `{}` on {}, and no cell with \
                     that triple has it in its graph. A dead excuse is an excuse nobody can see \
                     expire: delete the row.",
                    cdep.package, cdep.triple
                );
            }
        }
        for (i, shim) in policy.cshims.iter().enumerate() {
            if !cshim_row_used[i] {
                fail = true;
                eprintln!(
                    "gate cells: FAILED — {CELL_GATE_POLICY} shims `{}@{}` on {}, and no cell with \
                     that triple carries it. Delete the row: an override nobody uses is an \
                     override nobody re-reads.",
                    shim.package, shim.version, shim.triple
                );
            }
        }
    }

    eprintln!();
    // OWN-CODE is the column this gate is about: in-repo packages a compiler
    // read for the cell's triple, over the number it owed. PM is the in-repo
    // proc macros, which compile for the HOST on every cell and are therefore
    // outside that obligation rather than silently inside it.
    // TESTS is the second column this gate is now about: workspace members whose
    // TEST targets a compiler read for the cell's triple. `-` means the cell is
    // exempt (the host cell, whose `cargo test` already does it; the wasm32
    // cells, which nothing tests) — never "0 of 0 and green".
    // SECS and DISK are the cell's COST (`CellReport::disk`), summed on the
    // line under the table.
    eprintln!(
        "{:<10} {:<26} {:<34} {:>8} {:>6} {:>9} {:>4} {:>7} {:>5} {:>9}  STATUS",
        "CELL",
        "TRIPLE",
        "TOOLCHAIN",
        "CHECKED",
        "GRAPH",
        "OWN-CODE",
        "PM",
        "TESTS",
        "SECS",
        "DISK"
    );
    for r in &reports {
        let mut tail = String::new();
        if !r.excused.is_empty() {
            tail.push_str(&format!(" (excused: {})", r.excused.join(", ")));
        }
        if !r.shimmed.is_empty() {
            tail.push_str(&format!(" (shimmed: {})", r.shimmed.join(", ")));
        }
        eprintln!(
            "{:<10} {:<26} {:<34} {:>8} {:>6} {:>9} {:>4} {:>7} {:>5} {:>9}  {}{}",
            r.cell,
            r.triple,
            r.toolchain,
            r.checked,
            r.graph,
            format!("{}/{}", r.own_checked, r.own_total),
            r.own_proc_macros,
            if r.tests_note.is_some() {
                "-".to_string()
            } else {
                format!("{}/{}", r.tests_checked, r.tests_owed)
            },
            r.secs,
            r.disk
                .map_or_else(|| "-".to_string(), aterm_verify::disk::gib),
            r.status,
            tail
        );
    }
    eprintln!("{}", cells_cost_line(&reports));
    eprintln!();
    let verdict = cells_verdict(&reports, all_cells.len(), narrowed, fail);
    eprintln!("{}", verdict.text);
    verdict.ok
}

/// THE GATE'S COST, the line under the table: the seconds and the target-dir
/// bytes of the cells this run compiled. `gate cells-foreign` is a stage of
/// every merge-contract run since 2026-09-25, and a review on 2026-09-27
/// measured it by hand because nothing printed what it cost (12 GiB and 242 s
/// cold into a per-checkout cache, before the snapshot's run moved it into a
/// cells lane its disk preflight counts). The bytes are what the target dirs
/// HOLD after the run, not what it added: a warm run's dirs hold a cold run's
/// artifacts too, and holding is what the disk preflight budgets.
fn cells_cost_line(reports: &[CellReport]) -> String {
    let compiled: Vec<&CellReport> = reports.iter().filter(|r| r.disk.is_some()).collect();
    let secs: u64 = compiled.iter().map(|r| r.secs).sum();
    let bytes = compiled
        .iter()
        .filter_map(|r| r.disk)
        .fold(0u64, u64::saturating_add);
    format!(
        "gate cells: cost — {} cell(s) compiled in {secs} s; their target dirs hold {}.",
        compiled.len(),
        aterm_verify::disk::gib(bytes)
    )
}

/// THE MATRIX CLAIM. Nothing else in this file may spell these words, and
/// [`cells_verdict`] is the ONE function that may emit them — the discipline
/// `aterm_verify::verdict` holds around `MERGE_CONTRACT_SENTENCE`, for the same
/// reason: this repo has no CI, and the sentence a reader quotes is the whole
/// distance between "the matrix type-checks" and "I believed it did".
const MATRIX_CLAIM: &str = "cells type-check";

/// The verdict `gate cells` ends on: the words, and the exit bit.
struct CellsVerdict {
    text: String,
    /// Does the process exit 0? A SKIPPED cell does NOT clear this — see
    /// [`CellOutcome::Skipped`] for why an uninstalled std must not block.
    ok: bool,
}

/// Turn the per-cell rows into the sentence a reader quotes.
///
/// A FUNCTION, and tested, because the sentence was wrong and nothing could see
/// it: five `SKIPPED(no-std)` rows, no compiler started, and
/// `GREEN — all 5 cells type-check` underneath them (measured 2026-09-17, see
/// [`CellOutcome`]). The rows are the only input, so the tests can build the
/// state a box without a cross std produces without owning that box.
fn cells_verdict(
    reports: &[CellReport],
    forge_cells: usize,
    narrowed: bool,
    gate_failed: bool,
) -> CellsVerdict {
    // A cell that RAN and did not hold blocks on its own, even if the caller's
    // `fail` flag were ever to miss it: two independent paths to red, because
    // this is the direction where being wrong is unrecoverable.
    if gate_failed || reports.iter().any(|r| r.outcome == CellOutcome::Failed) {
        return CellsVerdict {
            text: "gate cells: FAILED — see the rows above.".to_string(),
            ok: false,
        };
    }
    let skipped: Vec<&CellReport> = reports
        .iter()
        .filter(|r| r.outcome == CellOutcome::Skipped)
        .collect();

    // SAY WHICH GREEN, AND SAY HOW MUCH. The sentence a reader quotes has to be
    // true of the run that printed it. "All five cells type-check" was quotable
    // and wrong for as long as eighteen of aterm's own crates went unread on
    // two of them, so the coverage the sentence is really about — the in-repo
    // half — is IN the sentence now, not in a table above it.
    let own_checked: usize = reports.iter().map(|r| r.own_checked).sum();
    let own_total: usize = reports.iter().map(|r| r.own_total).sum();
    let unread = own_total - own_checked;
    let coverage = if unread == 0 {
        format!(
            "every one of the {own_total} in-repo crate-instances across them read by a compiler \
             for its own triple"
        )
    } else {
        format!("{own_checked} of {own_total} in-repo crate-instances read; {unread} NOT")
    };
    // AND SAY WHAT THE TEST-TARGET PASS REACHED, in the same sentence, because
    // "the cells type-check" was quotable and true while not one `#[cfg(test)]`
    // line had been compiled for a triple that is not this box's.
    let tests_checked: usize = reports.iter().map(|r| r.tests_checked).sum();
    let tests_owed: usize = reports.iter().map(|r| r.tests_owed).sum();
    // EXEMPT MEANS "RAN, AND WAS NOT OWED A TEST PASS" — the host cell, whose
    // `cargo test` already compiled them, and the wasm32 cells, which nothing
    // tests. A SKIPPED cell carries the same `tests_note` field and is not
    // exempt from anything; counting it here printed
    // `(5 not owed a test pass: mac-arm, linux, win, wasm-cpu, wasm-gpu)` about
    // a run that owed all five and ran none.
    let exempt: Vec<&str> = reports
        .iter()
        .filter(|r| r.outcome == CellOutcome::Checked && r.tests_note.is_some())
        .map(|r| r.cell.as_str())
        .collect();
    let coverage = format!(
        "{coverage}; test targets compiled for {tests_checked} of {tests_owed} member-instances{}",
        if exempt.is_empty() {
            String::new()
        } else {
            format!(
                " ({} not owed a test pass: {})",
                exempt.len(),
                exempt.join(", ")
            )
        }
    );
    // WHAT A TYPE CHECK IS NOT, said in the verdict rather than left to a
    // reader of the policy file. A shimmed cell never links the C library the
    // shim stands in for, and no cell here RUNS anything.
    let scope = if reports.iter().any(|r| !r.shimmed.is_empty()) {
        "\ngate cells: SCOPE — a shimmed cell is TYPE-CHECKED and nothing more; it does not LINK \
         the bundled C (`cargo check` never links on any cell) and no cell runs a binary. \
         Cross-compiler-specific codegen and C ABI breakage stay out of reach here."
    } else {
        ""
    };

    if !skipped.is_empty() {
        let names = skipped
            .iter()
            .map(|r| format!("{} ({})", r.cell, r.triple))
            .collect::<Vec<_>>()
            .join(", ");
        let ran = reports.len() - skipped.len();
        return CellsVerdict {
            text: format!(
                "{} — {} of the {} cell(s) this run selected had no installed std, so NO COMPILER \
                 READ THEM: {names}. {ran} cell(s) were checked: {coverage}. This run does NOT \
                 make the matrix claim about forge's {forge_cells} cells.\n\
                 gate cells: exit 0 on purpose — an uninstalled std is a fact about this box, not \
                 a finding about the tree, and a red no change to this repository can clear is how \
                 an operator learns to stop reading a gate. Install it \
                 (`rustup target add <triple> --toolchain <channel>`) and run again to earn the \
                 claim; until then `tools/verify.sh` counts this stage as a SKIP and withholds \
                 `{}`.{scope}",
                aterm_verify::stages::CELLS_NOT_PROVEN,
                skipped.len(),
                reports.len(),
                aterm_verify::MERGE_CONTRACT_SENTENCE,
            ),
            ok: true,
        };
    }
    // NARROWED, or short of forge's list for any other reason: the second test
    // is not redundant. `--cell` is today's only narrowing, and a claim that
    // depends on remembering to add a clause for tomorrow's is the defect one
    // paragraph up, in a different shape.
    if narrowed || reports.len() != forge_cells {
        return CellsVerdict {
            text: format!(
                "gate cells: GREEN OF WHAT RAN — {} of forge's {forge_cells} cells, {coverage}. \
                 NOT the matrix claim.{scope}",
                reports.len()
            ),
            ok: true,
        };
    }
    let mut how = String::new();
    if reports.iter().any(|r| !r.excused.is_empty()) {
        how.push_str(", with the C dependencies excused above");
    }
    if reports.iter().any(|r| !r.shimmed.is_empty()) {
        how.push_str(", with the C build scripts shimmed above");
    }
    if how.is_empty() {
        how.push_str(", with nothing excused or shimmed");
    }
    // EARNED: every cell forge ships, every one of them read by a compiler for
    // its own triple. The only branch that may spell [`MATRIX_CLAIM`].
    CellsVerdict {
        text: format!(
            "gate cells: GREEN — all {} {MATRIX_CLAIM}{how}: {coverage}.{scope}",
            reports.len()
        ),
        ok: true,
    }
}

/// The disk-backed root the cells gate caches into when nothing overrides it.
///
/// Split out of [`cell_target_dir`] so the choice is testable without running a
/// compiler. Order: `$XDG_CACHE_HOME/aterm/cells`, then `$HOME/.cache/aterm/cells`,
/// then — only for a host with neither — the system temp dir. An empty or
/// relative `$XDG_CACHE_HOME` is IGNORED rather than honoured: the spec says the
/// variable is meaningful only as an absolute path, and a relative one would put
/// a multi-gigabyte cache under whatever directory the gate happened to be
/// invoked from, which on this repo is the workspace the gate then refuses.
fn default_cell_cache_root() -> PathBuf {
    cell_cache_root_from(
        std::env::var_os("XDG_CACHE_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
        &std::env::temp_dir(),
    )
}

/// The pure choice behind [`default_cell_cache_root`], taking its three inputs
/// as arguments.
///
/// Separated so the rule can be tested by CALLING it rather than by setting
/// `XDG_CACHE_HOME` on the test process — a test that mutates process env races
/// every other test in the same binary, and this crate runs its tests threaded.
fn cell_cache_root_from(
    xdg_cache_home: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
    temp_dir: &Path,
) -> PathBuf {
    if let Some(xdg) = xdg_cache_home {
        let xdg = Path::new(xdg);
        if xdg.is_absolute() {
            return xdg.join("aterm").join("cells");
        }
    }
    if let Some(home) = home {
        let home = Path::new(home);
        if home.is_absolute() {
            return home.join(".cache").join("aterm").join("cells");
        }
    }
    temp_dir.join("aterm-cells")
}

/// Where a cell's build artifacts go: OUTSIDE the repo, and on a DISK.
///
/// Never `<repo>/target`: a verification gate that writes into the tree it is
/// judging can invalidate the very build a developer is mid-way through, and the
/// brief for this verb makes it a hard rule. `$ATERM_CELL_TARGET_DIR` overrides
/// for a caller who wants the cache somewhere specific, and is REFUSED if it
/// points inside the workspace rather than quietly honoured.
///
/// THE SECOND HAZARD, which this function used to walk straight into while
/// guarding against the first. The default was [`std::env::temp_dir`], and on
/// every mainstream Linux that is `/tmp` — which systemd mounts as a tmpfs,
/// i.e. RAM. These cells leave GIGABYTES behind by design (5.0 GB for the three
/// foreign ones alone, and the doc on [`gate_cells_foreign`] measured it), so
/// the default turned a verification gate into a memory leak: on m17-tower a
/// `/tmp/aterm-cells` reached 13.6 GB and took the box's swap with it, and the
/// only warning was a doc line telling the reader to point the override at a
/// disk themselves. A gate does not get to hand its own hazard to its caller.
/// The default is now the user's CACHE directory, which is disk-backed on every
/// platform we ship and is where a rebuildable multi-gigabyte artifact cache
/// belongs: `$XDG_CACHE_HOME/aterm/cells`, else `$HOME/.cache/aterm/cells`. The
/// temp dir survives only as the last resort for a host with neither, where
/// there is no better answer and the run is a one-shot anyway.
///
/// THE THIRD HAZARD: one cache shared by every checkout. The default used to be
/// the same `<cache>/aterm/cells/<cell>` for every worktree of this repo, and
/// cargo cannot share a target dir between two checkouts of one workspace: it
/// hashes a member's package id RELATIVE TO THE WORKSPACE ROOT, so both write
/// the same artifact names, while the dep-info that decides freshness lists the
/// OTHER checkout's absolute paths — whose mtimes say nothing about this tree's
/// edits. So a cell could read another worktree's stale metadata as fresh.
/// MEASURED 2026-09-25, three times in one day: the dead/gui, dead/engine and
/// dead/services reviews each ran `gate cells` red on source their tree did not
/// have (an `aterm-sixel` `hook()` E0061, an `aterm-gui` `Metadata` E0107), and
/// green with a private `$ATERM_CELL_TARGET_DIR`. A stale artifact can as easily
/// turn a red cell green, which is the direction a gate may never err in. The
/// default is now keyed by the checkout ([`checkout_cache_key`]); an explicit
/// `$ATERM_CELL_TARGET_DIR` is the caller's own choice and is used as given.
///
/// THE FOURTH HAZARD, which the third's fix created: a per-checkout cache
/// outlives its checkout. A worktree is removed, its 20-46 GB of cells stay, and
/// nothing ever names them again — MEASURED 2026-09-25 on m7, nine orphaned
/// checkout caches (356 GB) plus four hand-made override dirs took the disk to
/// 100% and 20 GB free. So each checkout's cache carries a marker naming the
/// checkout it serves, and every default-cache run sweeps the caches whose
/// checkout is gone ([`sweep_orphaned_cell_caches`]).
fn cell_target_dir(root: &Path, cell: &str) -> Result<PathBuf, String> {
    let dir = match std::env::var_os("ATERM_CELL_TARGET_DIR") {
        Some(v) => PathBuf::from(v).join(cell),
        None => {
            let cache = default_cell_cache_root();
            let key = checkout_cache_key(root);
            sweep_orphaned_cell_caches(&cache, &key);
            let checkout = cache.join(&key);
            std::fs::create_dir_all(&checkout).map_err(|e| {
                format!(
                    "could not create the cells cache {}: {e}",
                    checkout.display()
                )
            })?;
            let canon = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
            // Best effort: a marker that cannot be written only means this cache
            // is never swept, which is the old behaviour, not a wrong verdict.
            let _ = std::fs::write(
                checkout.join(CELL_CACHE_MARKER),
                canon.as_os_str().as_encoded_bytes(),
            );
            checkout.join(cell)
        }
    };
    let inside = match (dir.canonicalize(), root.canonicalize()) {
        (Ok(d), Ok(r)) => d.starts_with(&r),
        // Not created yet: compare the paths as written, which is enough to
        // catch the case this refusal exists for.
        _ => dir.starts_with(root),
    };
    if inside {
        return Err(format!(
            "the target dir {} is inside the workspace; this gate never writes into the tree it \
             judges. Point $ATERM_CELL_TARGET_DIR somewhere else.",
            dir.display()
        ));
    }
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("could not create the target dir {}: {e}", dir.display()))?;
    Ok(dir)
}

/// The per-checkout directory under the default cells cache: the checkout's
/// directory name (for a human reading the cache) and an FNV-1a hash of its
/// canonical path (so two checkouts that share a name still differ). Stable
/// across toolchains, unlike `DefaultHasher`, so a checkout keeps its cache.
/// The file in a checkout's cells cache that names the checkout it serves.
const CELL_CACHE_MARKER: &str = ".checkout";

/// The flat `<cache>/<cell>` dirs of the layout before per-checkout keys
/// (2026-09-25): the cells that existed then. A migration list, not the cell
/// table — `tools/cross-cell-gate.tsv` stays the one roster of cells.
const LEGACY_FLAT_CELL_DIRS: [&str; 8] = [
    "mac-arm",
    "mac-x64",
    "linux",
    "linux-arm",
    "win",
    "win-arm",
    "wasm-cpu",
    "wasm-gpu",
];

/// Remove every per-checkout cells cache under `cache` whose checkout no longer
/// exists, and the flat `<cache>/<cell>` dirs of the layout before per-checkout
/// keys. Never the cache named `keep`, and never a dir without a marker (it may
/// be a cache this rule cannot account for). Best effort: a failure to read or
/// remove leaves the cache as it was.
fn sweep_orphaned_cell_caches(cache: &Path, keep: &str) {
    let Ok(entries) = std::fs::read_dir(cache) else {
        return;
    };
    let listed: Vec<(String, Option<PathBuf>)> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| {
            let marker = std::fs::read(e.path().join(CELL_CACHE_MARKER))
                .ok()
                .map(|b| PathBuf::from(String::from_utf8_lossy(&b).into_owned()));
            (e.file_name().to_string_lossy().into_owned(), marker)
        })
        .collect();
    for name in orphaned_cell_caches(&listed, keep, &LEGACY_FLAT_CELL_DIRS, |p| {
        p.join("Cargo.toml").is_file()
    }) {
        let _ = std::fs::remove_dir_all(cache.join(name));
    }
}

/// The pure decision behind [`sweep_orphaned_cell_caches`]: of the cache's
/// entries `(name, marker)`, which to remove. A marked cache goes when its
/// checkout is no longer a workspace (`is_checkout`); a legacy flat dir named
/// for a cell goes; everything else, and `keep`, stays.
fn orphaned_cell_caches<'a>(
    entries: &'a [(String, Option<PathBuf>)],
    keep: &str,
    cells: &[&str],
    is_checkout: impl Fn(&Path) -> bool,
) -> Vec<&'a str> {
    entries
        .iter()
        .filter(|(name, marker)| {
            name != keep
                && match marker {
                    Some(checkout) => !is_checkout(checkout),
                    None => cells.contains(&name.as_str()),
                }
        })
        .map(|(name, _)| name.as_str())
        .collect()
}

fn checkout_cache_key(root: &Path) -> String {
    let canon = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in canon.as_os_str().as_encoded_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    // A worktree kept in a dot-directory would otherwise hide its cache too.
    let name = canon.file_name().map_or_else(
        || "root".to_owned(),
        |n| n.to_string_lossy().trim_start_matches('.').to_owned(),
    );
    format!("{name}-{hash:016x}")
}

// ---------------------------------------------------------------------------
// G-LINT: THE FORMATTER ORACLE
// ---------------------------------------------------------------------------

/// The verdict a formatter pass reached. THREE-valued on purpose.
///
/// "Ran and found nothing", "ran and found something" and "never ran" are three
/// different facts. For a month the third wore the second's clothes —
/// `trustfmt: FAILED (exit Some(1))` was `cargo fmt` failing to find a
/// `cargo-fmt` the Trust stage2 does not ship — so the verb could not pass for
/// any input, stopped being read, and three lint regressions reached `main`
/// under it. A pass that reaches no verdict never renders one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum LaneVerdict {
    /// The pass RAN, over the real tree, and found nothing.
    Clean,
    /// The pass RAN and found drift. Blocks.
    Finding,
    /// The pass did not run: nothing about the tree was learned. Blocks, under
    /// its own headline, because "could not tell" is not "clean".
    NotRun,
}

impl LaneVerdict {
    /// Fold two verdicts, keeping the more alarming: the lane is `Clean` only
    /// when every pass ran and passed, and a finding is never downgraded to
    /// NOT RUN by a later pass that could not run.
    fn worst(self, other: Self) -> Self {
        match (self, other) {
            (Self::Finding, _) | (_, Self::Finding) => Self::Finding,
            (Self::NotRun, _) | (_, Self::NotRun) => Self::NotRun,
            _ => Self::Clean,
        }
    }
}

/// The verdict lines `gate lint` prints. An exit code cannot tell a finding from
/// a pass that never ran, so the words carry what the code cannot.
const LINT_VERDICT_FAILED: &str = "gate lint: FAILED";
const LINT_VERDICT_NO_VERDICT: &str = "gate lint: COULD NOT RUN";
const LINT_VERDICT_GREEN: &str = "gate lint: GREEN";

/// `gate lint` and `gate lint --fmt-only` — the same run: the formatter's three
/// passes over the tree, from THE toolchain ([`trust_toolchain`]).
fn gate_lint(args: &[String]) -> bool {
    if let Some(bad) = args.iter().find(|a| a.as_str() != "--fmt-only") {
        eprintln!(
            "gate lint: unknown argument `{bad}`; nothing was run.\n\
             usage: xtask gate lint [--fmt-only]   (the formatter; tippy is the gate's Tippy stage)"
        );
        return false;
    }
    eprintln!("=== gate lint (targo-fmt --all + the per-file trustfmt sweep + fmt editions) ===");
    let root = workspace_root();
    let toolchain = trust_toolchain();
    // A refused directory holds a `targo` that is not the pinned toolchain; its
    // formatter is not the one the tree is held to, so it is not run at all.
    let verdict = if toolchain.refused.is_some() {
        eprintln!(
            "  trustfmt: NOT RUN — {}. Nothing was format-checked.",
            toolchain.missing_targo_label()
        );
        LaneVerdict::NotRun
    } else {
        fmt_lane(&root, &toolchain.stage2_dir)
    };
    lint_verdict(verdict)
}

/// The three passes, every one run whatever the others found, folded with
/// [`LaneVerdict::worst`].
fn fmt_lane(root: &Path, tools: &Path) -> LaneVerdict {
    fmt_workspace_pass(root, tools)
        .worst(fmt_sweep(root, tools))
        .worst(fmt_edition_agreement(root))
}

/// Print the verdict line for `verdict`; `true` only for [`LaneVerdict::Clean`].
fn lint_verdict(verdict: LaneVerdict) -> bool {
    match verdict {
        LaneVerdict::Clean => {
            eprintln!("{LINT_VERDICT_GREEN}");
            true
        }
        LaneVerdict::Finding => {
            eprintln!("{LINT_VERDICT_FAILED} — formatting drift, named above");
            false
        }
        LaneVerdict::NotRun => {
            eprintln!(
                "{LINT_VERDICT_NO_VERDICT} — a formatter pass never ran, so NOTHING was learned \
                 about the tree. This is not a finding and it is not a clean tree."
            );
            false
        }
    }
}

/// PASS ONE: `targo-fmt --all --check`, cargo's own target discovery over the
/// workspace. The stage2's branded driver, invoked by path and never resolved
/// off PATH: a `cargo-fmt`/`rustfmt` found there is stock Rust's formatter, a
/// different style under the pinned toolchain's name.
fn fmt_workspace_pass(root: &Path, tools: &Path) -> LaneVerdict {
    let driver = tools.join(TRUSTFMT_DRIVER);
    if !driver.is_file() {
        // Checked BEFORE spawning: the answer is a stat().
        eprintln!(
            "  trustfmt: NOT RUN — no `{TRUSTFMT_DRIVER}` in {}. FORMATTING WAS NOT CHECKED. This \
             is a missing toolchain, NOT a clean tree and NOT a finding: `aterm pkg install \
             trust`, or point TRUST_STAGE2_BIN at a built stage2, and re-run.",
            tools.display()
        );
        return LaneVerdict::NotRun;
    }
    let (ok, stdout, stderr) =
        run_capturing_both("trustfmt", &driver, &["--all", "--check"], tools, root);
    if ok {
        eprintln!(
            "  trustfmt: clean — every target `targo-fmt --all` discovers across the workspace \
             is formatted. That is the WORKSPACE, not the tree; the sweep below covers the rest."
        );
        LaneVerdict::Clean
    } else if stdout.contains(FMT_DIFF_MARKER) {
        // A NON-ZERO EXIT IS NOT YET A FINDING: `targo-fmt` exits 1 both for
        // "unformatted" and for "could not look" (see [`FMT_DIFF_MARKER`]).
        // Paths, not files: a source reached through two module paths (a
        // `#[path]` include from a test target) is listed under each.
        let paths = fmt_diff_paths(&stdout).len();
        eprintln!(
            "  trustfmt: FINDING — drift at {paths} path(s) (an upper bound on files). The diff \
             is printed above. Fix the whole tree with `{} --all` from {}.",
            driver.display(),
            root.display()
        );
        LaneVerdict::Finding
    } else {
        eprintln!(
            "  trustfmt: NOT RUN — `{TRUSTFMT_DRIVER}` exited non-zero WITHOUT reporting a single \
             `{FMT_DIFF_MARKER}…` line, so it never read the tree. That is an environment fault, \
             not a formatting finding. Its own words were:\n{}",
            stderr.trim_end()
        );
        LaneVerdict::NotRun
    }
}

/// Trust's branded `cargo fmt` driver, as it is named in the stage2 bin dir.
/// It drives `trustfmt`, which it finds as a sibling on PATH — which is why
/// the pass hands it the toolchain directory as a PATH prefix.
const TRUSTFMT_DRIVER: &str = "targo-fmt";

/// How a formatting FINDING names each file, on STDOUT.
///
/// MEASURED on this tree, both directions. A real drift report — the 254-file
/// one this lane was armed against — writes 2,685 `Diff in <path>:<line>:`
/// lines to STDOUT and exactly ZERO bytes to stderr. An environment fault
/// (`targo-fmt` run with `trustfmt` off PATH; an unresolvable dependency
/// manifest) writes its complaint to STDERR and emits no `Diff in` line at all.
/// Both exit 1. So the presence of this marker, and not the exit code, is what
/// separates "the tree is unformatted" from "the check never read the tree" —
/// the distinction [`LaneVerdict`] exists for.
const FMT_DIFF_MARKER: &str = "Diff in ";

// ---------------------------------------------------------------------------
// THE FMT SWEEP — pass two of the trustfmt lane
// ---------------------------------------------------------------------------

/// The raw `trustfmt` binary, beside [`TRUSTFMT_DRIVER`] in the stage2 bin dir.
///
/// The sweep calls it DIRECTLY rather than through `targo-fmt`, because the
/// thing it needs is the one thing the cargo driver will not do: format a path
/// that is not a target of a workspace member. Same formatter, same
/// `rustfmt.toml`, no cargo in the middle.
const TRUSTFMT_BIN: &str = "trustfmt";

/// Tracked sources the sweep does not hold to the formatter, because their LAYOUT
/// is their content. A drifted file listed here is not a finding.
const FMT_SWEEP_EXCLUSIONS: &[&str] = &[
    // Machine-generated corpus records (const `&[&[u8]]`); the generator owns them.
    "crates/aterm-core/tests/support/replay_corpus_data.rs",
    // Generated const drawlists, `include!`d so rustfmt never reflows them out from
    // under their generator and its `*_matches_assets` drift test.
    "crates/aterm-effects/src/animal_glyphs_gen.rs",
    "crates/aterm-effects/src/cat_glyphs_gen.rs",
    "crates/aterm-effects/src/dog_glyphs_gen.rs",
    "crates/aterm-effects/src/pet_glyphs_gen.rs",
    "crates/aterm-effects/src/robi_glyphs_gen.rs",
    // grep_guard L0 fixtures whose line breaks are the case under test.
    "tools/grep-guard-fixtures/must_fire/alias_then_call_same_line.rs",
    "tools/grep-guard-fixtures/must_fire/split_receiver_chain.rs",
    "tools/grep-guard-fixtures/must_silent/wrapped_chain_offload.rs",
];

/// Every TRACKED `*.rs` path outside `vendor/`, repo-relative, sorted.
///
/// TRACKED, via `git ls-files`, and not a directory walk: a walk cannot tell a
/// source file from a scratch file somebody left in the tree, and a lane that
/// fires on an untracked experiment is a lane people learn to ignore. `git` is
/// already a dependency of this crate's `perf` verb and of `aterm-verify`, so
/// this adds no new one.
///
/// `vendor/` is excluded because it is third-party source aterm mirrors rather
/// than authors; holding a fork to aterm's formatter would produce a diff
/// against upstream on every file and make `cargo forge attest`'s `[OB-7]`
/// fork-vs-upstream diff unreadable.
fn tracked_rs_files(root: &Path) -> Result<Vec<String>, String> {
    let out = Command::new("git")
        .args(["ls-files", "-z", "*.rs"])
        .current_dir(root)
        .output()
        .map_err(|e| format!("could not run `git ls-files` in {}: {e}", root.display()))?;
    if !out.status.success() {
        return Err(format!(
            "`git ls-files` exited {:?} in {}: {}",
            out.status.code(),
            root.display(),
            String::from_utf8_lossy(&out.stderr).trim_end()
        ));
    }
    let mut files: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .split('\0')
        .filter(|p| !p.is_empty() && !p.starts_with("vendor/"))
        .map(str::to_owned)
        .collect();
    files.sort_unstable();
    Ok(files)
}

/// The edition `trustfmt` must be told for `rel`, found from the nearest
/// enclosing `Cargo.toml`.
///
/// THIS IS NOT A DETAIL, it is what makes the sweep's answers true. `cargo fmt`
/// passes each target its own crate's edition; a bare per-file run reads only
/// `rustfmt.toml`, which says `edition = "2024"` for the whole tree, and the
/// tree is not all 2024. MEASURED on 2026-08-31, the same batched sweep run
/// twice over this tree, with the lookup and with everything forced to
/// `rustfmt.toml`'s 2024: WITH it, 35 files drift and stderr is empty; WITHOUT
/// it, 39 files drift and stderr carries 16 `error:` lines. The difference is
/// six PHANTOM findings and two HIDDEN files. The phantoms are `aterm-wasm`'s
/// and `aterm-gpu-web`'s `lib.rs`, `notifications_api.rs` and
/// `scrollback_tiers_api.rs` — both members declare `edition = "2021"`, and
/// 2024 formats them differently. The hidden two are
/// `tools/temporal-extract/refine/src/main.rs` and
/// `tools/temporal-extract/src/infer.rs`, which use `gen` as an identifier:
/// legal in 2021, RESERVED in 2024, so at the wrong edition they do not parse
/// and their real drift never gets reported at all. The wrong edition therefore
/// invents findings AND conceals them, in one step.
///
/// `edition.workspace = true` resolves to the root manifest's
/// `[workspace.package] edition`. A file under no manifest at all (a fixture,
/// a docs repro) gets the root's edition, which is what `rustfmt.toml` would
/// have given it anyway.
fn crate_edition(root: &Path, rel: &str) -> String {
    let mut dir = Path::new(rel).parent();
    while let Some(d) = dir {
        if let Some(e) = manifest_edition(&root.join(d).join("Cargo.toml"), root) {
            return e;
        }
        dir = d.parent();
    }
    workspace_edition(root)
}

/// The `edition` a manifest declares, resolving `edition.workspace = true`.
/// `None` when the manifest is absent or declares no edition (a virtual
/// manifest, or a member inheriting one it never names).
fn manifest_edition(manifest: &Path, root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(manifest).ok()?;
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("edition"))?;
    if line.contains("workspace") {
        return Some(workspace_edition(root));
    }
    let quoted = line.split('"').nth(1)?;
    Some(quoted.to_owned())
}

/// The workspace's own edition, from the root `[workspace.package]`. Falls back
/// to the newest edition this toolchain knows rather than to an old one: a
/// wrong-but-newer edition produces a parse error a reader can act on, while a
/// wrong-but-older one silently reformats to an obsolete style.
fn workspace_edition(root: &Path) -> String {
    std::fs::read_to_string(root.join("Cargo.toml"))
        .ok()
        .and_then(|t| {
            t.lines()
                .map(str::trim)
                .find(|l| l.starts_with("edition") && l.contains('"'))
                .and_then(|l| l.split('"').nth(1).map(str::to_owned))
        })
        .unwrap_or_else(|| "2024".to_string())
}

/// The paths named by `Diff in <path>:<line>:` lines, deduplicated.
///
/// Splitting on the LAST two colons, not the first, because an absolute path on
/// a machine whose checkout lives under a directory with a colon in it would
/// otherwise be truncated to nothing — and the failure would be a silently
/// EMPTY finding list, which reads exactly like a clean tree.
fn fmt_diff_paths(stdout: &str) -> std::collections::BTreeSet<String> {
    stdout
        .lines()
        .filter_map(|l| l.strip_prefix(FMT_DIFF_MARKER))
        .filter_map(|rest| {
            let rest = rest.trim_end();
            let rest = rest.strip_suffix(':').unwrap_or(rest);
            rest.rsplit_once(':').map(|(path, _)| path.to_owned())
        })
        .collect()
}

/// Make an absolute path from a formatter report repo-relative, trying the
/// root as given and then its canonical form. Returns the absolute path
/// unchanged when neither prefix matches — visibly odd, which is the right
/// failure: a path silently rewritten to something that matches no registry row
/// would turn a registered exclusion into a finding, and a path silently
/// DROPPED would turn a finding into a clean sweep.
fn relative_to(abs: &str, root: &Path, canonical_root: Option<&Path>) -> String {
    let p = Path::new(abs);
    for base in std::iter::once(root).chain(canonical_root) {
        if let Ok(rel) = p.strip_prefix(base) {
            return rel.to_string_lossy().into_owned();
        }
    }
    abs.to_owned()
}

/// PASS THREE of the trustfmt lane: EVERY OTHER SPELLING OF THE FORMATTER
/// AGREES WITH THIS ONE.
///
/// Passes one and two are the oracle, and they are right because [`crate_edition`]
/// tells `trustfmt` each file's real edition, read from the nearest
/// `Cargo.toml`. NOTHING ELSE DOES THAT. `rustfmt` resolves a file's edition
/// from the nearest `rustfmt.toml` — never from a manifest — so every other way
/// of reaching the formatter reads a DIFFERENT edition for the same file:
///
///   * `trustfmt <path>` by hand, and every editor's format-on-save, and
///     rust-analyzer;
///   * `targo-fmt -- <paths>`, which MEASURED 2026-09-17 does not even scope to
///     the paths given — it formats the whole workspace and hands the paths to
///     `rustfmt` as extra arguments.
///
/// The consequences are exactly the ones [`crate_edition`] records for the
/// sweep, and they run in BOTH directions: at edition 2024 a 2021 crate's file
/// formats differently (phantom findings, and a hand-run formatter silently
/// rewriting a file the gate then calls unformatted), and a file using `gen` as
/// an identifier does not PARSE, so its real drift is never reported at all.
/// MEASURED on this tree the day this was written: with `rustfmt.toml` naming
/// only the workspace edition, `trustfmt` on an edition-2021 file holding `let
/// gen = 1;` answers `error: expected identifier, found reserved keyword`; with
/// a `rustfmt.toml` in that crate it formats clean.
///
/// So the tree must be shaped so that both resolutions give the same answer,
/// and this is the check that keeps it that way. It compares, for every tracked
/// first-party `.rs` file, the edition its MANIFEST declares against the
/// edition `rustfmt` would resolve for it, and names the one-line file that
/// reconciles them. It needs no toolchain and no build; it is pure `stat` and
/// `read_to_string`.
///
/// `vendor/` is out of scope for the same reason it is out of the sweep's: it
/// is third-party source aterm mirrors rather than authors, and nothing in this
/// repo should be formatting it at all.
fn fmt_edition_agreement(root: &Path) -> LaneVerdict {
    let files = match tracked_rs_files(root) {
        Ok(f) => f,
        Err(e) => {
            eprintln!(
                "  fmt editions: NOT RUN — the file list could not be built, so no file's \
                 edition was compared: {e}"
            );
            return LaneVerdict::NotRun;
        }
    };
    if files.is_empty() {
        eprintln!(
            "  fmt editions: NOT RUN — `git ls-files` named zero `.rs` files under {}. An \
             empty comparison is never a pass.",
            root.display()
        );
        return LaneVerdict::NotRun;
    }
    // (directory needing a file, wanted edition) -> one example file.
    let mut wanted: std::collections::BTreeMap<(String, String), String> = Default::default();
    let mut cache: std::collections::BTreeMap<PathBuf, String> = Default::default();
    for rel in &files {
        let manifest = crate_edition(root, rel);
        let dir = Path::new(rel)
            .parent()
            .unwrap_or(Path::new(""))
            .to_path_buf();
        let resolved = cache
            .entry(dir)
            .or_insert_with(|| rustfmt_edition_for(root, rel))
            .clone();
        if resolved != manifest {
            wanted
                .entry((manifest_dir_of(root, rel), manifest))
                .or_insert_with(|| rel.clone());
        }
    }
    if wanted.is_empty() {
        eprintln!(
            "  fmt editions: clean — every one of {} tracked `.rs` file(s) resolves the same \
             edition through `rustfmt.toml` as its manifest declares.",
            files.len()
        );
        return LaneVerdict::Clean;
    }
    eprintln!(
        "  fmt editions: FINDING — {} crate(s) whose files rustfmt formats at an edition \
         their manifest does not declare. Every hand-run formatter, editor and \
         rust-analyzer therefore disagrees with this gate about them.",
        wanted.len()
    );
    for ((dir, edition), example) in &wanted {
        let at = if dir.is_empty() {
            "rustfmt.toml".to_string()
        } else {
            format!("{dir}/rustfmt.toml")
        };
        eprintln!(
            "      {example}: manifest says edition {edition} — add {at} with `edition = \"{edition}\"`"
        );
    }
    LaneVerdict::Finding
}

/// The directory of the nearest `Cargo.toml` above `rel`, repo-relative — where
/// a `rustfmt.toml` has to go for [`crate_edition`] and `rustfmt` to agree.
fn manifest_dir_of(root: &Path, rel: &str) -> String {
    let mut dir = Path::new(rel).parent();
    while let Some(d) = dir {
        if root.join(d).join("Cargo.toml").is_file() {
            return d.to_string_lossy().into_owned();
        }
        dir = d.parent();
    }
    String::new()
}

/// The edition `rustfmt` itself would use for `rel`: the `edition` of the
/// nearest `rustfmt.toml`/`.rustfmt.toml` at or above the FILE's directory, and
/// `2015` — rustfmt's own default — when there is none.
///
/// Deliberately ignores every manifest: this is the answer the OTHER spellings
/// get, and the whole point of the comparison is that it is computed a
/// different way.
fn rustfmt_edition_for(root: &Path, rel: &str) -> String {
    let mut dir = Path::new(rel).parent();
    loop {
        let d = dir.unwrap_or(Path::new(""));
        for name in ["rustfmt.toml", ".rustfmt.toml"] {
            if let Ok(text) = std::fs::read_to_string(root.join(d).join(name))
                && let Some(e) = toml_edition(&text)
            {
                return e;
            }
        }
        match dir {
            None => return "2015".to_string(),
            Some(d) => dir = d.parent(),
        }
    }
}

/// `edition = "2021"` from a `rustfmt.toml`'s top level. A one-key reader, not
/// a TOML parser: `rustfmt.toml` is flat by construction and this file already
/// reads manifests the same way.
fn toml_edition(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .find_map(|l| {
            let rest = l.strip_prefix("edition")?.trim_start();
            let rest = rest.strip_prefix('=')?.trim();
            Some(rest.trim_matches(['"', '\''].as_slice()).to_string())
        })
}

/// PASS TWO: hold every tracked `.rs` file outside `vendor/` to the same
/// formatter, per file, at the edition of the crate that owns it.
///
/// It sweeps the WHOLE tree rather than the complement of pass one on purpose:
/// computing that complement means modelling cargo's target discovery and
/// rustfmt's `mod` recursion, and a model that drifts drops a file out of BOTH
/// passes silently. Sweeping everything makes the passes cross-check instead.
/// `--skip-children` judges each file alone, at top-level indentation, which is
/// how `--all` formats an out-of-line `mod` file, so the two never disagree
/// (measured 2026-08-31: zero disagreements over 1,752 files). It needs no
/// compiler and costs seconds.
fn fmt_sweep(root: &Path, tools: &Path) -> LaneVerdict {
    let bin = tools.join(TRUSTFMT_BIN);
    if !bin.is_file() {
        eprintln!(
            "  trustfmt sweep: NOT RUN — no `{TRUSTFMT_BIN}` in {}. The files `--all` cannot \
             reach (include!-only sources and the crates outside the workspace) were NOT \
             checked. Build the Trust stage2 and re-run.",
            tools.display()
        );
        return LaneVerdict::NotRun;
    }
    let files = match tracked_rs_files(root) {
        Ok(f) => f,
        Err(e) => {
            eprintln!(
                "  trustfmt sweep: NOT RUN — the file list could not be built, so NOTHING was \
                 swept. This is an environment fault, not a clean tree: {e}"
            );
            return LaneVerdict::NotRun;
        }
    };
    if files.is_empty() {
        eprintln!(
            "  trustfmt sweep: NOT RUN — `git ls-files` named zero `.rs` files under {}. An \
             empty sweep is never a pass.",
            root.display()
        );
        return LaneVerdict::NotRun;
    }
    // GROUPED BY EDITION, then chunked. The grouping is correctness (see
    // [`crate_edition`]); the chunking is only ARG_MAX hygiene — 1,752 paths is
    // ~80 KB against this platform's 1 MiB, so one exec would fit today, and a
    // fixed chunk keeps it fitting on a machine with a longer checkout path or
    // a much larger tree.
    const CHUNK: usize = 400;
    // trustfmt reports ABSOLUTE paths, and on macOS `/var` is a symlink to
    // `/private/var`, so a root under the system temp dir comes back with a
    // prefix that does not textually match the one we passed in. Resolving both
    // forms keeps the finding list repo-relative; falling back to the absolute
    // path is correct-but-ugly rather than wrong, and never silently empty.
    let canonical_root = std::fs::canonicalize(root).ok();
    let mut by_edition: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for rel in &files {
        by_edition
            .entry(crate_edition(root, rel))
            .or_default()
            .push(rel.clone());
    }
    let mut drifted = std::collections::BTreeSet::new();
    // `error:` lines trustfmt wrote about files it could NOT read, in batches where
    // some other file did produce a diff. Non-empty means the sweep is not a sweep.
    let mut unread: Vec<String> = Vec::new();
    let mut editions: Vec<String> = Vec::new();
    for (edition, group) in &by_edition {
        editions.push(format!("{edition}:{}", group.len()));
        for chunk in group.chunks(CHUNK) {
            let mut cmd = Command::new(&bin);
            cmd.current_dir(root)
                .args(["--check", "--edition", edition, "--unstable-features"])
                // Each file judged ALONE. Without this, trustfmt follows `mod`
                // declarations out of every file it is given and re-formats the
                // same trees hundreds of times over.
                .arg("--skip-children")
                .args(chunk.iter().map(String::as_str));
            let out = match cmd.output() {
                Ok(o) => o,
                Err(e) => {
                    eprintln!(
                        "  trustfmt sweep: NOT RUN — could not spawn {} ({e}). Part of the tree \
                         went unchecked, so this is not a clean lint.",
                        bin.display()
                    );
                    return LaneVerdict::NotRun;
                }
            };
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            // The SAME discrimination pass one makes, for the same reason:
            // trustfmt exits 1 both for "these files are unformatted" and for
            // "I could not read them". A `Diff in` line means it read them.
            if !out.status.success() && !stdout.contains(FMT_DIFF_MARKER) {
                eprintln!(
                    "  trustfmt sweep: NOT RUN — `{TRUSTFMT_BIN}` exited {:?} over a batch of {} \
                     file(s) at edition {edition} WITHOUT printing a single `{FMT_DIFF_MARKER}…` \
                     line, so it never read them. Environment fault, not a formatting finding. \
                     Its own words were:\n{}",
                    out.status.code(),
                    chunk.len(),
                    stderr.trim_end()
                );
                return LaneVerdict::NotRun;
            }
            // MIXED batch: one `Diff in` line proved trustfmt read SOMETHING, and the
            // check above then accepted the whole 400-file chunk — discarding every
            // `error:` line it wrote about the OTHERS unread, while the summary below
            // kept counting them as checked. A file that stops parsing at the edition
            // it is handed (this lane's own doc records `gen` under 2024 doing exactly
            // that) left the swept set in silence. Name them, and do not call the
            // sweep clean while any remain.
            if !out.status.success() {
                // `error[E0123]: …` and `error: …` both carry the path after the
                // first `:`; keep the whole line, it is the operator's evidence.
                for line in stderr.lines().map(str::trim) {
                    if line.starts_with("error") {
                        unread.push(line.to_string());
                    }
                }
            }
            for abs in fmt_diff_paths(&stdout) {
                drifted.insert(relative_to(&abs, root, canonical_root.as_deref()));
            }
        }
    }
    if !unread.is_empty() {
        eprintln!(
            "  trustfmt sweep: NOT RUN — `{TRUSTFMT_BIN}` could not read {} file(s) in a batch \
             where others did produce diffs, so part of the tree went unchecked while the \
             count below would have called it checked:",
            unread.len()
        );
        for line in &unread {
            eprintln!("    {line}");
        }
        return LaneVerdict::NotRun;
    }
    let findings: Vec<&String> = drifted
        .iter()
        .filter(|p| !FMT_SWEEP_EXCLUSIONS.contains(&p.as_str()))
        .collect();
    if findings.is_empty() {
        eprintln!(
            "  trustfmt sweep: clean — {} tracked `.rs` file(s) checked per-file at their own \
             crate edition ({}), including every source `targo-fmt --all` cannot reach \
             ({} excluded by FMT_SWEEP_EXCLUSIONS).",
            files.len(),
            editions.join(", "),
            FMT_SWEEP_EXCLUSIONS.len()
        );
        return LaneVerdict::Clean;
    }
    eprintln!(
        "  trustfmt sweep: FINDING — {} of {} tracked `.rs` file(s) are unformatted and are NOT \
         reachable by `targo-fmt --all`, so nothing else in this repository was ever going to \
         report them:",
        findings.len(),
        files.len()
    );
    for path in &findings {
        eprintln!("      {path}");
    }
    eprintln!(
        "    Fix each with `{} --edition <that crate's edition> --unstable-features \
         --skip-children <path>` (drop `--check` to write). FMT_SWEEP_EXCLUSIONS is for a \
         fixture or a generator's output whose layout the formatter would destroy, and \
         nothing else.",
        bin.display()
    );
    LaneVerdict::Finding
}

#[cfg(test)]
mod cell_cache_root_tests {
    use super::*;
    use std::ffi::OsStr;

    /// The rule that keeps a multi-gigabyte artifact cache off a RAM disk.
    ///
    /// `gate cells` and `gate cells-foreign` leave 5.0 GB behind by design. The
    /// default used to be [`std::env::temp_dir`], which is `/tmp` on every
    /// mainstream Linux and a tmpfs — RAM — on systemd boxes; one grew to
    /// 13.6 GB on m17-tower and took the machine's swap with it. The cache dir
    /// is disk-backed everywhere we ship, so that is the default now, and the
    /// temp dir is only for a host that offers neither cache dir.
    #[test]
    fn the_cells_cache_prefers_a_disk_backed_home_over_the_temp_dir() {
        let temp = Path::new("/tmp");

        // XDG_CACHE_HOME wins when it is absolute, and the temp dir is not used.
        let picked = cell_cache_root_from(
            Some(OsStr::new("/var/cache/me")),
            Some(OsStr::new("/home/me")),
            temp,
        );
        assert_eq!(picked, Path::new("/var/cache/me/aterm/cells"));
        assert!(
            !picked.starts_with(temp),
            "a gate that leaves 5.0 GB behind must not default into the temp dir: {}",
            picked.display()
        );

        // No XDG: $HOME/.cache, still off the temp dir.
        let picked = cell_cache_root_from(None, Some(OsStr::new("/home/me")), temp);
        assert_eq!(picked, Path::new("/home/me/.cache/aterm/cells"));
        assert!(!picked.starts_with(temp));

        // A RELATIVE XDG_CACHE_HOME is ignored rather than honoured — honouring
        // it would put the cache under the invoking directory, which for this
        // repo is the workspace `cell_target_dir` then refuses outright.
        let picked = cell_cache_root_from(
            Some(OsStr::new("relative/cache")),
            Some(OsStr::new("/home/me")),
            temp,
        );
        assert_eq!(picked, Path::new("/home/me/.cache/aterm/cells"));
        let picked = cell_cache_root_from(Some(OsStr::new("")), Some(OsStr::new("/home/me")), temp);
        assert_eq!(picked, Path::new("/home/me/.cache/aterm/cells"));

        // Neither available: the temp dir is the last resort, not the default.
        // This is the ONLY arm that may name it.
        assert_eq!(
            cell_cache_root_from(None, None, temp),
            Path::new("/tmp/aterm-cells")
        );
        assert_eq!(
            cell_cache_root_from(None, Some(OsStr::new("relative/home")), temp),
            Path::new("/tmp/aterm-cells")
        );
    }

    /// Two checkouts of the repo never share a default cells cache (cargo reads
    /// one's stale metadata as fresh in the other), and one checkout always
    /// finds its own again.
    #[test]
    fn each_checkout_gets_its_own_default_cells_cache() {
        let a = checkout_cache_key(Path::new("/nonexistent/one/aterm"));
        let b = checkout_cache_key(Path::new("/nonexistent/two/aterm"));
        assert_ne!(a, b, "two worktrees named alike must not share a cache");
        assert!(
            a.starts_with("aterm-") && b.starts_with("aterm-"),
            "{a} {b}"
        );
        assert_eq!(a, checkout_cache_key(Path::new("/nonexistent/one/aterm")));
        let hidden = checkout_cache_key(Path::new("/nonexistent/.aterm-wt"));
        assert!(hidden.starts_with("aterm-wt-"), "{hidden}");
    }

    /// A checkout's cache goes with its checkout: a marked cache whose checkout
    /// is gone is swept, a live one and the running one stay, a pre-key flat
    /// cell dir is migrated away, and an unmarked dir nobody can account for
    /// is left alone.
    #[test]
    fn a_cells_cache_whose_checkout_is_gone_is_swept() {
        let live = PathBuf::from("/wt/live");
        let gone = PathBuf::from("/wt/gone");
        let entries = vec![
            ("live-1".to_owned(), Some(live.clone())),
            ("gone-2".to_owned(), Some(gone.clone())),
            ("me-3".to_owned(), Some(gone.clone())),
            ("linux".to_owned(), None),
            ("someone-elses".to_owned(), None),
        ];
        let swept = orphaned_cell_caches(&entries, "me-3", &LEGACY_FLAT_CELL_DIRS, |p| p == live);
        assert_eq!(swept, vec!["gone-2", "linux"]);
        // Negative control: with every checkout alive nothing marked is swept.
        let none = orphaned_cell_caches(&entries, "me-3", &[], |_| true);
        assert!(none.is_empty(), "{none:?}");
    }

    /// The first hazard still holds: the gate never writes into the tree it judges.
    #[test]
    fn an_override_inside_the_workspace_is_still_refused() {
        let root = Path::new("/home/me/repo");
        assert!(
            cell_cache_root_from(
                Some(OsStr::new("/home/me/repo/target")),
                None,
                Path::new("/tmp")
            )
            .starts_with(root),
            "this is the shape cell_target_dir refuses; if it stops starting with the root the              refusal below is testing nothing"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// THE VERDICT RULE: only a pass that RAN and found nothing passes the verb. A
    /// finding blocks, a pass that never ran blocks under its own headline, and
    /// an argument the verb does not know runs nothing (`--no-fmt`, the flag that
    /// once dropped this lane, now has nothing left to drop).
    #[test]
    fn a_lane_that_never_ran_is_blocked_with_no_verdict_not_a_pass() {
        assert!(lint_verdict(LaneVerdict::Clean));
        assert!(!lint_verdict(LaneVerdict::Finding));
        assert!(!lint_verdict(LaneVerdict::NotRun));
        assert_eq!(
            LaneVerdict::Clean.worst(LaneVerdict::NotRun),
            LaneVerdict::NotRun,
            "a pass that never ran must not be folded away by a clean one"
        );
        assert_eq!(
            LaneVerdict::NotRun.worst(LaneVerdict::Finding),
            LaneVerdict::Finding,
            "a finding must not be downgraded to NOT RUN by a pass that could not run"
        );
        assert!(!gate_lint(&["--no-fmt".to_string()]));
    }

    /// With a stage2 dir holding neither `targo-fmt` nor `trustfmt`, over a root
    /// that is no checkout, every pass answers NOT RUN — the one answer that also
    /// tells the operator the tree was never examined.
    #[test]
    fn an_absent_toolchain_fails_the_fmt_lane_closed() {
        let tmp = std::env::temp_dir().join(format!("aterm-gate-lint-red-{}", std::process::id()));
        let root = tmp.join("root");
        let tools = tmp.join("empty-stage2");
        let _ = std::fs::create_dir_all(&root);
        let _ = std::fs::create_dir_all(&tools);
        let workspace = fmt_workspace_pass(&root, &tools);
        let lane = fmt_lane(&root, &tools);
        let _ = std::fs::remove_dir_all(&tmp);
        assert_eq!(
            workspace,
            LaneVerdict::NotRun,
            "a missing formatter must not report clean formatting"
        );
        assert_eq!(lane, LaneVerdict::NotRun);
    }

    /// PASS THREE, IN BOTH DIRECTIONS: a crate whose `rustfmt.toml` edition
    /// does not match its manifest is a FINDING, and adding the one-line file
    /// clears it.
    ///
    /// This is the shape the real tree was in until 2026-09-17: seven crates at
    /// edition 2021 under a single root `rustfmt.toml` saying 2024, so the
    /// gate's sweep (which reads the MANIFEST) and every other spelling of the
    /// formatter (which reads `rustfmt.toml`) disagreed about 77 files. The
    /// disagreement is invisible to both passes above — each is internally
    /// consistent — which is exactly why it needs a pass of its own.
    ///
    /// Needs no `trustfmt`: it compares two file reads. It does need `git`, for
    /// the same file list the sweep uses.
    #[test]
    fn a_rustfmt_toml_that_contradicts_its_manifest_is_a_finding_until_it_is_written() {
        let root = std::env::temp_dir().join(format!("aterm-fmt-editions-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("inner/src")).expect("create scratch tree");
        std::fs::write(root.join("rustfmt.toml"), "edition = \"2024\"\n").expect("root config");
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"outer\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
        )
        .expect("outer manifest");
        std::fs::create_dir_all(root.join("src")).expect("outer src");
        std::fs::write(root.join("src/lib.rs"), "pub fn a() {}\n").expect("outer lib");
        std::fs::write(
            root.join("inner/Cargo.toml"),
            "[package]\nname = \"inner\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
        )
        .expect("inner manifest");
        std::fs::write(root.join("inner/src/lib.rs"), "pub fn b() {}\n").expect("inner lib");
        let git = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(&root)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        };
        if !(git(&["init", "-q"])
            && git(&["config", "user.email", "fixture@example.invalid"])
            && git(&["config", "user.name", "fixture"])
            && git(&["add", "-A"]))
        {
            let _ = std::fs::remove_dir_all(&root);
            eprintln!(
                "SKIP a_rustfmt_toml_that_contradicts_its_manifest_is_a_finding_until_it_is_written: \
                 `git` could not stage the scratch tree, so there is no file list."
            );
            return;
        }
        assert_eq!(
            fmt_edition_agreement(&root),
            LaneVerdict::Finding,
            "an edition-2021 crate under a 2024 `rustfmt.toml` must be a FINDING: every \
             hand-run formatter reads 2024 for it while the sweep reads 2021"
        );
        // …and the one-line file is the whole repair.
        std::fs::write(root.join("inner/rustfmt.toml"), "edition = \"2021\"\n")
            .expect("inner config");
        let green = fmt_edition_agreement(&root);
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(
            green,
            LaneVerdict::Clean,
            "writing the crate's own `rustfmt.toml` must clear it; if it does not, the \
             FINDING above was about something else"
        );
    }

    /// NO `rustfmt.toml` ANYWHERE is not "the workspace edition" — it is
    /// rustfmt's own default of 2015, and a tree in that state has every file
    /// formatted at an edition no manifest names. The check must say so rather
    /// than pass for want of a file to read.
    #[test]
    fn a_tree_with_no_rustfmt_toml_at_all_is_a_finding_not_a_pass() {
        let root = std::env::temp_dir().join(format!("aterm-fmt-noconf-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).expect("create scratch tree");
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"bare\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
        )
        .expect("manifest");
        std::fs::write(root.join("src/lib.rs"), "pub fn a() {}\n").expect("lib");
        let git = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(&root)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        };
        if !(git(&["init", "-q"])
            && git(&["config", "user.email", "fixture@example.invalid"])
            && git(&["config", "user.name", "fixture"])
            && git(&["add", "-A"]))
        {
            let _ = std::fs::remove_dir_all(&root);
            eprintln!(
                "SKIP a_tree_with_no_rustfmt_toml_at_all_is_a_finding_not_a_pass: `git` could \
                 not stage the scratch tree."
            );
            return;
        }
        let verdict = fmt_edition_agreement(&root);
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(verdict, LaneVerdict::Finding, "2015 is not 2024");
    }

    /// PASS TWO fails closed on a missing formatter on its OWN, over the real
    /// tree — not by inheriting pass one's NOT RUN.
    #[test]
    fn the_fmt_sweep_is_not_run_without_a_trustfmt() {
        let tmp =
            std::env::temp_dir().join(format!("aterm-fmt-sweep-notools-{}", std::process::id()));
        let tools = tmp.join("empty-stage2");
        std::fs::create_dir_all(&tools).expect("create scratch stage2");
        let verdict = fmt_sweep(&workspace_root(), &tools);
        let _ = std::fs::remove_dir_all(&tmp);
        assert_eq!(
            verdict,
            LaneVerdict::NotRun,
            "a missing `trustfmt` must not report a clean sweep — the files `--all` cannot \
             reach would then be claimed checked by a pass that never ran"
        );
    }

    /// THE RED FIXTURE FOR PASS TWO: an unformatted file that pass one cannot
    /// see must turn the sweep RED, and the same tree must be GREEN once it is
    /// formatted — so the pass is shown to move in BOTH directions rather than
    /// merely to be stuck red.
    ///
    /// The scratch tree is deliberately shaped like the real blind spot: the
    /// unformatted source is NOT a module of the crate, it is `include!`d, so a
    /// `targo-fmt --all` over the same tree would report nothing at all.
    ///
    /// SKIPS, loudly, when `trustfmt` or `git` is absent: it prints why, so a
    /// green test run on a bare box cannot be read as evidence.
    #[test]
    fn a_planted_include_only_source_reds_the_fmt_sweep_and_greens_when_fixed() {
        let tools = trust_toolchain().stage2_dir;
        if !tools.join(TRUSTFMT_BIN).is_file() {
            eprintln!(
                "SKIP a_planted_include_only_source_reds_the_fmt_sweep_and_greens_when_fixed: \
                 no `{TRUSTFMT_BIN}` in {} — this box cannot demonstrate the sweep either way.",
                tools.display()
            );
            return;
        }
        let root = std::env::temp_dir().join(format!("aterm-fmt-sweep-red-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).expect("create scratch crate");
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"sweep-fixture\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
        )
        .expect("write manifest");
        // The crate root only `include!`s the offender — the exact shape
        // `targo-fmt --all` walks straight past.
        std::fs::write(root.join("src/lib.rs"), "include!(\"included.rs\");\n")
            .expect("write lib.rs");
        let bad = "pub fn wrong(  ) ->usize{let x=1;x}\n";
        let good = "pub fn wrong() -> usize {\n    let x = 1;\n    x\n}\n";
        std::fs::write(root.join("src/included.rs"), bad).expect("write included.rs");
        let git = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(&root)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        };
        if !(git(&["init", "-q"])
            && git(&["config", "user.email", "fixture@example.invalid"])
            && git(&["config", "user.name", "fixture"])
            && git(&["add", "-A"]))
        {
            let _ = std::fs::remove_dir_all(&root);
            eprintln!(
                "SKIP a_planted_include_only_source_reds_the_fmt_sweep_and_greens_when_fixed: \
                 `git` could not stage the scratch tree, so the sweep has no file list."
            );
            return;
        }
        let red = fmt_sweep(&root, &tools);
        // The NEGATIVE assertion this fixture exists for: the sweep must report
        // a FINDING on a file nothing else in this repository would ever name.
        assert!(
            !matches!(red, LaneVerdict::Clean),
            "planted drift read as clean: {red:?}"
        );
        assert_eq!(red, LaneVerdict::Finding, "planted drift must be a FINDING");
        // BOTH DIRECTIONS. A fixture that is red for an unrelated reason (a
        // broken scratch tree, a missing edition) proves nothing, so the same
        // tree must go green on the repair and on nothing else.
        std::fs::write(root.join("src/included.rs"), good).expect("rewrite included.rs");
        let green = fmt_sweep(&root, &tools);
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(
            green,
            LaneVerdict::Clean,
            "formatting the one offending file must clear the sweep; if it does not, the RED \
             above was about something other than formatting"
        );
    }

    /// The edition comes from the CRATE, never from `rustfmt.toml` alone.
    ///
    /// These four are the real cases, over the real tree: a member that names
    /// its own older edition, a member that inherits the workspace's, a crate
    /// outside the workspace entirely, and a file under no manifest at all.
    #[test]
    fn the_fmt_sweep_reads_each_crate_s_own_edition() {
        let root = workspace_root();
        assert_eq!(
            crate_edition(&root, "crates/aterm-wasm/src/lib.rs"),
            "2021",
            "aterm-wasm declares `edition = \"2021\"`; formatting it as 2024 is what produced \
             six phantom findings in the sweep that motivated this pass"
        );
        assert_eq!(
            crate_edition(&root, "crates/aterm-grid/src/lib.rs"),
            "2024",
            "`edition.workspace = true` must resolve to [workspace.package]"
        );
        assert_eq!(
            crate_edition(&root, "tools/temporal-extract/src/main.rs"),
            "2021",
            "an out-of-workspace crate still has its own manifest, and it wins"
        );
        assert_eq!(
            crate_edition(
                &root,
                "tools/grep-guard-fixtures/must_fire/split_receiver_chain.rs"
            ),
            workspace_edition(&root),
            "a file under no manifest falls back to the workspace edition"
        );
    }

    /// `Diff in <path>:<line>:` parsing, including the path this repo's own
    /// first cut got wrong.
    #[test]
    fn fmt_diff_paths_splits_from_the_right_so_a_colon_in_the_path_survives() {
        let out = "Diff in /a/b/c.rs:12:\n some context\nDiff in /a/b/c.rs:40:\n\
                   Diff in /w:1/x/y.rs:7:\n";
        let paths = fmt_diff_paths(out);
        // One entry for c.rs despite two hunks — the report is per FILE.
        assert_eq!(
            paths.len(),
            2,
            "hunks in one file must collapse to one path"
        );
        assert!(paths.contains("/a/b/c.rs"));
        // Splitting on the FIRST colon would yield "/w" here, and a path that
        // matches no file silently drops out of the finding list — a clean
        // sweep for the wrong reason.
        assert!(paths.contains("/w:1/x/y.rs"), "got {paths:?}");
    }

    /// THE WORKSPACE PASS, driven through all four verdicts — because an armed lane that cannot go red is worse than
    /// an unarmed one, and an armed lane that goes red for the WRONG reason is
    /// how this one lost a month.
    ///
    /// The four cases are the four things the driver can do to this gate:
    ///   1. absent                            -> NOT RUN (the toolchain, not the tree)
    ///   2. exit 1, `Diff in` on STDOUT       -> FINDING (blocks)
    ///   3. exit 1, nothing on stdout         -> NOT RUN (environment fault)
    ///   4. exit 0                            -> CLEAN
    ///
    /// Case 3 is the one worth writing down. `targo-fmt` exits 1 both when the
    /// tree is unformatted and when it could not read the tree at all (an
    /// unresolvable manifest, `trustfmt` off PATH — MEASURED: it complains on
    /// stderr and emits no `Diff in` line). Telling those apart by EXIT CODE is
    /// exactly the mislabel this lane's history is made of, so cases 2 and 3
    /// differ here ONLY in which stream the stub writes to, and the lane is
    /// required to tell them apart anyway.
    ///
    /// It drives [`fmt_workspace_pass`] alone, not [`fmt_lane`]: all four verdicts
    /// are statements about the WORKSPACE pass, and the sweep has its own red
    /// fixture, `a_planted_include_only_source_reds_the_fmt_sweep_and_greens_when_fixed`.
    #[test]
    fn the_armed_fmt_lane_separates_drift_from_a_toolchain_that_never_looked() {
        let tmp = std::env::temp_dir().join(format!("aterm-fmt-lane-{}", std::process::id()));
        let root = tmp.join("root");
        let tools = tmp.join("stage2");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&root).expect("root");
        std::fs::create_dir_all(&tools).expect("stage2");

        // 1. No driver at all: a missing toolchain is never a clean tree.
        assert_eq!(
            fmt_workspace_pass(&root, &tools),
            LaneVerdict::NotRun,
            "a stage2 with no `{TRUSTFMT_DRIVER}` must report NOT RUN — never \
             FAILED, and never CLEAN: nothing was read"
        );

        // 2. Real drift: the marker on STDOUT, which is where targo-fmt puts it.
        write_exec(
            &tools.join(TRUSTFMT_DRIVER),
            "#!/bin/sh\necho 'Diff in /x/y.rs:3:'\nexit 1\n",
        );
        assert_eq!(
            fmt_workspace_pass(&root, &tools),
            LaneVerdict::Finding,
            "a run that named a drifted file IS a finding and must block"
        );

        // 3. Same exit code, same driver — but it never got as far as a file.
        //    Only the STREAM differs, and that has to be enough.
        write_exec(
            &tools.join(TRUSTFMT_DRIVER),
            "#!/bin/sh\necho 'failed to start cargo metadata' >&2\nexit 1\n",
        );
        assert_eq!(
            fmt_workspace_pass(&root, &tools),
            LaneVerdict::NotRun,
            "a non-zero exit with no `Diff in` line is an environment fault, not \
             a formatting finding — rendering it as one is the original bug"
        );

        // 4. Clean, so none of the above is the lane having stopped answering.
        write_exec(&tools.join(TRUSTFMT_DRIVER), "#!/bin/sh\nexit 0\n");
        assert_eq!(fmt_workspace_pass(&root, &tools), LaneVerdict::Clean);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Write an executable shell stub. (The fmt lane's pre-checks only stat, but
    /// the run paths exec, so these must be +x.)
    fn write_exec(path: &Path, body: &str) {
        std::fs::write(path, body).expect("write stub");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
                .expect("chmod stub");
        }
    }

    /// A cell's toolchain pre-flight ([`cell_toolchain`]) must match the
    /// installed-target listing LINE-EXACTLY: a sloppy `contains` reads a
    /// neighbouring triple as the one asked for, and the cell then fails at the
    /// build instead of being reported as SKIPPED(no-std).
    #[test]
    fn a_toolchain_lists_a_target_only_line_exactly() {
        let listing = "aarch64-apple-darwin\nwasm32-unknown-unknown\n";
        assert!(super::toolchain_lists_target(
            listing,
            "wasm32-unknown-unknown"
        ));

        // A neighbouring triple is NOT the triple.
        let wasip1 = "aarch64-apple-darwin\nwasm32-wasip1\n";
        assert!(!super::toolchain_lists_target(
            wasip1,
            "wasm32-unknown-unknown"
        ));

        // Nor is a longer name that merely contains it.
        let suffixed = "wasm32-unknown-unknown-nightly\n";
        assert!(!super::toolchain_lists_target(
            suffixed,
            "wasm32-unknown-unknown"
        ));

        // Nothing installed at all is the skip case, not a match.
        assert!(!super::toolchain_lists_target("", "wasm32-unknown-unknown"));
    }

    // -----------------------------------------------------------------------
    // `gate cells` — the pure halves, each pinned by the mistake it cost.
    // -----------------------------------------------------------------------

    /// THE TRAP THIS GATE'S FIRST MEASUREMENT FELL INTO. A path package whose
    /// directory is already its name abbreviates its id to
    /// `path+file:///…/serde#1.0.228` — the fragment is a VERSION, not a name.
    /// Reading it as the name scored 42 first-party and vendored packages as
    /// never-checked and produced a confident, false conclusion about cargo's
    /// `--keep-going`.
    #[test]
    fn package_id_name_survives_all_three_id_shapes() {
        assert_eq!(
            super::package_id_name(
                "registry+https://github.com/rust-lang/crates.io-index#serde@1.0.228"
            ),
            "serde"
        );
        // Path package whose DIRECTORY differs from the package name — the
        // shape every `[patch.crates-io]` row in this workspace has.
        assert_eq!(
            super::package_id_name("path+file:///repo/crates/aterm-once-cell#once_cell@1.21.4"),
            "once_cell"
        );
        // Path package whose directory IS the name: no `name@` at all.
        assert_eq!(
            super::package_id_name("path+file:///repo/vendor/indexmap#0.2.3"),
            "indexmap"
        );
        // Cargo's older opaque spelling.
        assert_eq!(
            super::package_id_name("winit 0.30.12 (path+file:///repo/vendor/winit)"),
            "winit"
        );
    }

    /// The excuse is anchored at column zero and at the whole prefix, because it
    /// is the ONE failure this gate forgives. An indented `error occurred in
    /// cc-rs:` line is the *body* of a build-script failure cargo has already
    /// reported — counting it as a second, unexcused error turned both cross
    /// cells red for the very failure the policy had just excused.
    #[test]
    fn only_cargos_own_build_script_line_buys_an_excuse() {
        assert_eq!(
            super::build_script_failure(
                "error: failed to run custom build command for `ring v0.17.14`"
            ),
            Some("ring")
        );
        assert_eq!(
            super::build_script_failure(
                "error: failed to run custom build command for `zstd-sys v2.0.16+zstd.1.5.7 (/p)`"
            ),
            Some("zstd-sys")
        );
        // Not cargo's line: a diagnostic that merely mentions it.
        assert_eq!(
            super::build_script_failure(
                "  note: error: failed to run custom build command for `evil v1.0.0`"
            ),
            None
        );
        assert_eq!(
            super::build_script_failure("  error occurred in cc-rs: nope"),
            None
        );
        assert_eq!(
            super::build_script_failure("error[E0277]: the trait bound"),
            None
        );
    }

    /// `rustup toolchain list` marks the active one, and the marker is on the
    /// same line: a caller that takes the line verbatim asks rustup for a
    /// toolchain called `trust (active, default)` and reads the resulting "not
    /// installed" as "this box cannot build that cell".
    #[test]
    fn toolchain_names_drop_rustups_active_marker() {
        let listing =
            "1.95.0-aarch64-apple-darwin\nstable-aarch64-apple-darwin\ntrust (active, default)\n\n";
        assert_eq!(
            super::parse_toolchain_names(listing),
            vec![
                "1.95.0-aarch64-apple-darwin".to_string(),
                "stable-aarch64-apple-darwin".to_string(),
                "trust".to_string(),
            ]
        );
    }

    /// An artifact under `<target-dir>/debug/` is a HOST unit — a build script
    /// or a proc macro compiled for this Mac — and counting it as coverage of a
    /// cross cell is how a gate claims to have type-checked code for a triple
    /// it never touched.
    #[test]
    fn only_target_units_count_toward_a_cross_cells_coverage() {
        let target = vec!["/t/wasm32-unknown-unknown/debug/deps/libfoo.rmeta".to_string()];
        let host = vec!["/t/debug/deps/libserde_derive.dylib".to_string()];
        assert!(super::artifact_is_for(
            Some("wasm32-unknown-unknown"),
            &target
        ));
        assert!(!super::artifact_is_for(
            Some("wasm32-unknown-unknown"),
            &host
        ));
        // The host cell passes no `--target`, so its units are the same units.
        assert!(super::artifact_is_for(None, &host));
        assert!(super::artifact_is_for(None, &[]));
    }

    /// AN EXCUSE IS SCOPED TO A TRIPLE, and the first draft of the policy file
    /// was not: two `*` rows for `ring` and `zstd-sys` matched the mac-arm cell
    /// too, where both packages compile perfectly — so the gate's own audit
    /// caught the native cell being excused for something it does not need, on
    /// the very first full run. (That audit is a NOTE now, not a failure: a box
    /// that CAN run the build script is more capable, not out of policy. The
    /// scoping is still what makes the note mean anything.)
    #[test]
    fn an_excuse_only_applies_to_the_triple_it_names() {
        let policy = super::parse_cell_policy(
            "# comment\n\
             \n\
             cdep\tring\tx86_64-pc-windows-msvc\tno Windows SDK\n\
             cdep\tzstd-sys\t*\tbundled C\n\
             floor\twin\t123\tmeasured\n",
        )
        .expect("this policy parses");
        assert!(policy.excuse("ring", "x86_64-pc-windows-msvc").is_some());
        assert!(policy.excuse("ring", "aarch64-apple-darwin").is_none());
        // `*` is still available for a package no triple can build.
        assert!(policy.excuse("zstd-sys", "aarch64-apple-darwin").is_some());
        assert!(policy.excuse("nothing", "x86_64-pc-windows-msvc").is_none());
        assert_eq!(policy.floor("win"), Some(123));
        assert_eq!(policy.floor("linux"), None);
    }

    /// A row this parser cannot read is a COULD-NOT-RUN, never a skipped line:
    /// every row either forgives a failure or sets a floor, and both are ways
    /// for the gate to pass while proving less.
    #[test]
    fn an_unreadable_policy_row_stops_the_gate_rather_than_being_ignored() {
        assert!(super::parse_cell_policy("cdep\tring\tonly-three-columns\n").is_err());
        assert!(super::parse_cell_policy("floor\tlinux\tlots\twhy\n").is_err());
        assert!(super::parse_cell_policy("cdpe\tring\t*\ttypo in the kind\n").is_err());
        // Four columns of the known kinds, and nothing else, is the file — five
        // for `cshim`, which carries the `links` key as well.
        assert!(super::parse_cell_policy("floor\tlinux\t206\twhy\n").is_ok());
        assert!(
            super::parse_cell_policy(
                "cshim\tring@0.17.14\tx86_64-unknown-linux-gnu\tring_core_0_17_14_\twhy\n"
            )
            .is_ok()
        );
        // Four columns is a `cshim` row missing the one column that cannot be
        // derived from anything else.
        assert!(
            super::parse_cell_policy("cshim\tring@0.17.14\tx86_64-unknown-linux-gnu\twhy\n")
                .is_err()
        );
    }

    /// A SHIM IS A CLAIM ABOUT ONE BUILD SCRIPT AT ONE VERSION ON ONE TRIPLE,
    /// and each of those three is a way the claim can quietly stop being true.
    /// The parser refuses the shapes that would let it.
    #[test]
    fn a_build_script_shim_cannot_be_written_loosely() {
        // No version: the row would be inherited across a bump nobody re-read.
        assert!(
            super::parse_cell_policy(
                "cshim\tring\tx86_64-unknown-linux-gnu\tring_core_0_17_14_\twhy\n"
            )
            .is_err()
        );
        // No `*`: `zstd-sys` emits a `rustc-cfg` on wasm32 and on nothing else,
        // so "every triple" is never a thing anybody has checked.
        assert!(
            super::parse_cell_policy("cshim\tring@0.17.14\t*\tring_core_0_17_14_\twhy\n").is_err()
        );
        // And the two triples where the shipped shim is KNOWN to be unfaithful
        // are refused by the parser, not left to a reviewer.
        assert!(
            super::parse_cell_policy("cshim\tzstd-sys@2.0.16\twasm32-unknown-unknown\tzstd\twhy\n")
                .is_err()
        );
        assert!(super::shim_refusal("wasm32-unknown-unknown").is_some());
        assert!(super::shim_refusal("x86_64-unknown-hermit").is_some());
        // AND THE PARSER REFUSES NOTHING ELSE, on any box. What it rejects is a
        // claim about what a build script EMITS, which no machine gets a vote
        // on; `aarch64-apple-darwin`, `x86_64-unknown-linux-gnu` and
        // `x86_64-pc-windows-msvc` are all writable rows wherever this test
        // runs. The host's own triple is a different question with a different
        // answer — not "unwritable", but "not applied to that one cell" — and
        // it is asked of `host_shim_refusal` in the test below. Asking it here
        // is what shut the Windows lane on every Linux box.
        for cell in aterm_forge::resolve::default_cells() {
            let intrinsic = cell.triple.starts_with("wasm32") || cell.triple.contains("hermit");
            assert_eq!(
                super::shim_refusal(&cell.triple).is_some(),
                intrinsic,
                "the parser's verdict on `{}` must depend on its build scripts and on nothing \
                 else — least of all on which box is reading the file",
                cell.triple
            );
        }
    }

    /// THE HOST'S OWN TRIPLE IS A FACT ABOUT THE BOX, AND IT IS ASKED OF ONE
    /// CELL. A judge got `SHIMMED … 114/114 … GREEN` out of a row naming the
    /// host: the native cell passes no `--target`, so cargo applies a
    /// `[target.<host>.<links>]` override there too and the real build script
    /// never ran. That refusal is intact — as a per-cell decline, taken with
    /// the host triple in hand — and it reaches exactly one cell.
    #[test]
    fn a_shim_for_the_hosts_own_triple_is_declined_by_that_cell_and_by_no_other() {
        let host = super::rustc_host_triple().expect("`rustc -vV` reports a host triple");
        let why = super::host_shim_refusal(&host, &host)
            .expect("a `cshim` row naming this machine's own triple must not be APPLIED");
        assert!(
            why.contains("own triple"),
            "the decline must say WHY the host is different from wasm32/hermit: {why}"
        );
        // And every other cell keeps its overrides. This is the clause the old
        // code did not have: the refusal was global, so one row about the host
        // spoke for cells the host has nothing to do with.
        for cell in aterm_forge::resolve::default_cells() {
            if cell.triple == host {
                continue;
            }
            assert!(
                super::host_shim_refusal(&cell.triple, &host).is_none(),
                "cell `{}` ({}) lost its shim to a fact about the host `{host}`",
                cell.name,
                cell.triple
            );
        }
    }

    /// THE SHIPPED POLICY MUST BE READABLE FROM WHICHEVER MACHINE IS READING
    /// IT, and from 2026-09-01 until this test it was not.
    ///
    /// `tools/cross-cell-gate.tsv` carries two `cshim` rows for
    /// `x86_64-unknown-linux-gnu` — correct rows, measured on m21, where Linux
    /// is a CROSS cell. `parse_cell_policy` refused any row naming the reader's
    /// own triple, and refused it BEFORE `gate cells` had selected a single
    /// cell, so on m17-tower — where Linux is the NATIVE cell —
    /// `cargo run -p xtask -- gate cells --cell win` printed (measured on an
    /// unmodified 2ad5de183, where those rows are lines 89 and 90)
    /// `COULD NOT RUN — tools/cross-cell-gate.tsv:89: refusing the `cshim` row
    /// for `ring`` and never reached the `win` cell at all. The only gate that
    /// compiles aterm for Windows could not be STARTED on a Linux box, so no
    /// Windows break authored on one could ever turn it red — and the first
    /// run after this fix found one: `crate::seam::is_real_dir` is
    /// `#[cfg(unix)]` and `crates/atpkg/src/compat.rs:611` calls it on every
    /// target.
    ///
    /// So this walks the verb's own first two steps, in the verb's own order,
    /// once for every cell's triple pretended to be the host: the file parses,
    /// the narrowed cell is selectable, and it still has its overrides. A
    /// policy row about one machine can never again speak for another.
    #[test]
    fn the_shipped_policy_reads_and_narrows_from_every_cells_own_machine() {
        let root = crate::workspace_root();
        let text = std::fs::read_to_string(root.join(super::CELL_GATE_POLICY))
            .expect("tools/cross-cell-gate.tsv is shipped");
        // Step one of `gate_cells`, and its answer must not depend on who asks.
        let policy = super::parse_cell_policy(&text)
            .expect("the shipped policy parses on THIS machine, whichever machine it is");
        let cells = aterm_forge::resolve::default_cells();
        assert!(
            policy.cshims.iter().any(|c| {
                cells.iter().any(|cell| cell.triple == c.triple)
                    && super::rustc_host_triple().is_some_and(|h| h != c.triple)
            }),
            "this test needs at least one `cshim` row to audit"
        );
        for pretend_host in cells.iter().map(|c| c.triple.as_str()) {
            for cell in &cells {
                if cell.triple == pretend_host {
                    continue;
                }
                // Step two: the narrowing the Windows lane is run with.
                let selected =
                    aterm_forge::resolve::select(&cells, std::slice::from_ref(&cell.name))
                        .unwrap_or_else(|e| panic!("cell `{}` is selectable: {e}", cell.name));
                assert_eq!(selected.len(), 1, "`--cell {}` selects one cell", cell.name);
                // Step 1c: and it is handed every override the file gives it,
                // whichever of its neighbours happens to be native today.
                for (_, shim) in policy.shims_for(&cell.triple) {
                    assert!(
                        super::host_shim_refusal(&cell.triple, pretend_host).is_none(),
                        "`{}` loses `{}`'s override when `{pretend_host}` is the host",
                        cell.name,
                        shim.package
                    );
                }
            }
        }
    }

    /// THE PROC-MACRO CARVE-OUT IS READ FROM THE MANIFEST, not from a list in
    /// this file that would rot the moment a fourth one is written. A proc macro
    /// compiles for the HOST even in a cross build, so it can never produce an
    /// artifact for the cell's triple and is not owed one; every OTHER in-repo
    /// crate is.
    #[test]
    fn the_proc_macro_carve_out_comes_from_the_manifest() {
        let root = crate::workspace_root();
        assert!(super::manifest_is_proc_macro(
            &root.join("crates/aterm-error-derive")
        ));
        assert!(super::manifest_is_proc_macro(
            &root.join("crates/aterm-tracing-attributes")
        ));
        assert!(!super::manifest_is_proc_macro(
            &root.join("crates/aterm-core")
        ));
        assert!(!super::manifest_is_proc_macro(
            &root.join("crates/aterm-gui")
        ));
        // A directory with no manifest is not a proc macro, and is not a panic.
        assert!(!super::manifest_is_proc_macro(&root.join("no/such/dir")));
    }

    fn cell(name: &str, triple: &str) -> aterm_forge::model::Cell {
        aterm_forge::model::Cell {
            name: name.to_string(),
            triple: triple.to_string(),
            package: "aterm".to_string(),
        }
    }

    /// A cell that was really compiled, with the shape the verb builds: every
    /// in-repo crate read, every owed test target read, nothing exempt.
    fn checked_cell(name: &str, triple: &str) -> super::CellReport {
        super::CellReport {
            cell: name.to_string(),
            triple: triple.to_string(),
            toolchain: "stable".to_string(),
            checked: 100,
            graph: 100,
            excused: Vec::new(),
            shimmed: Vec::new(),
            own_checked: 40,
            own_total: 40,
            own_proc_macros: 1,
            tests_checked: 30,
            tests_owed: 30,
            tests_note: None,
            secs: 12,
            disk: Some(3 << 30),
            status: "GREEN".to_string(),
            outcome: super::CellOutcome::Checked,
        }
    }

    /// THE RED THIS FILE WAS WRITTEN FOR. Measured 2026-09-17 with no cross std
    /// installed and no native cell: five `SKIPPED(no-std)` rows, zero compilers
    /// started, and
    ///
    /// ```text
    /// gate cells: GREEN — all 5 cells type-check, with nothing excused or
    /// shimmed: 0 of 312 in-repo crate-instances read; 312 NOT; …
    /// ```
    ///
    /// with exit 0 under it, because the row carried `ok: true`. The verdict
    /// sentence is what a reader quotes, so it is what this holds: a run that
    /// compiled nothing may not spell the matrix claim, and must name what it
    /// did not do.
    #[test]
    fn a_cell_nothing_was_compiled_for_is_not_a_pass() {
        // FORGE'S WHOLE LIST, no narrowing, one cell uncompiled — the exact
        // shape that printed the sentence above, and the only shape in which
        // the matrix claim is even reachable.
        let v = super::cells_verdict(
            &[
                checked_cell("mac-arm", "aarch64-apple-darwin"),
                checked_cell("linux", "x86_64-unknown-linux-gnu"),
                super::CellReport::skipped(&cell("win", "x86_64-pc-windows-msvc"), 161, 74, 1),
                checked_cell("wasm-cpu", "wasm32-unknown-unknown"),
                checked_cell("wasm-gpu", "wasm32-unknown-unknown"),
            ],
            5,
            false,
            false,
        );
        assert!(
            !v.text.contains(super::MATRIX_CLAIM),
            "a run with a skipped cell claimed the matrix: {}",
            v.text
        );
        assert!(v.text.starts_with(aterm_verify::stages::CELLS_NOT_PROVEN));
        // NAMED, not just counted: an unnamed skip is an invisible skip.
        assert!(
            v.text.contains("win (x86_64-pc-windows-msvc)"),
            "{}",
            v.text
        );
        assert!(v.text.contains("NO COMPILER READ THEM"), "{}", v.text);
        // And the repair is in the sentence, not in a wiki.
        assert!(v.text.contains("rustup target add"), "{}", v.text);
    }

    /// THE COST LINE sums what was COMPILED: the seconds and target-dir bytes
    /// of every cell that ran, and nothing for a cell nothing was compiled for
    /// (its `-` DISK is not a zero-byte cell). Before 2026-09-27 no line said
    /// what the gate cost, and a review had to measure it by hand.
    #[test]
    fn the_cost_line_sums_the_cells_that_compiled_and_only_those() {
        let mut win = checked_cell("win", "x86_64-pc-windows-msvc");
        win.secs = 30;
        win.disk = Some(1 << 30);
        let mut arm = checked_cell("linux-arm", "aarch64-unknown-linux-gnu");
        arm.secs = 12;
        arm.disk = Some((1 << 30) + (1 << 29));
        let skipped =
            super::CellReport::skipped(&cell("wasm-cpu", "wasm32-unknown-unknown"), 54, 20, 1);
        assert_eq!(skipped.disk, None);
        assert_eq!(
            super::cells_cost_line(&[win, skipped, arm]),
            "gate cells: cost — 2 cell(s) compiled in 42 s; their target dirs hold 2.5 GiB."
        );
        assert_eq!(
            super::cells_cost_line(&[]),
            "gate cells: cost — 0 cell(s) compiled in 0 s; their target dirs hold 0.0 GiB."
        );
    }

    /// THE OTHER HALF OF THE SAME DECISION, pinned so it cannot be "fixed" into
    /// a blocker. An uninstalled std is a fact about the box that no change to
    /// this repository can clear, and `LaneVerdict` records what a permanent red
    /// costs. So a skip exits 0 — and a cell that RAN and failed does not.
    #[test]
    fn a_skip_does_not_block_a_checkout_and_a_cell_that_ran_and_failed_does() {
        let skipped =
            super::CellReport::skipped(&cell("win", "x86_64-pc-windows-msvc"), 161, 74, 1);
        assert!(super::cells_verdict(&[skipped], 5, false, false).ok);

        let mut failed = checked_cell("linux", "x86_64-unknown-linux-gnu");
        failed.outcome = super::CellOutcome::Failed;
        failed.status = "TYPE-ERROR(3)".to_string();
        // Red on the ROW alone, with the caller's own `fail` flag clear: two
        // independent paths to red, because this is the direction where being
        // wrong is unrecoverable.
        let v = super::cells_verdict(&[failed], 5, false, false);
        assert!(!v.ok);
        assert!(!v.text.contains(super::MATRIX_CLAIM), "{}", v.text);
    }

    /// A SKIPPED CELL IS NOT AN EXEMPT ONE. `tests_note` carries both "the host
    /// cell's `cargo test` already compiled these" and "nothing was compiled for
    /// this cell at all", and the exemption sentence counted the second kind:
    /// the all-skipped run printed `(5 not owed a test pass: mac-arm, linux,
    /// win, wasm-cpu, wasm-gpu)` about a matrix that owed all five and ran none.
    #[test]
    fn a_skipped_cell_is_never_counted_as_exempt_from_the_test_pass() {
        let mut exempt = checked_cell("wasm-cpu", "wasm32-unknown-unknown");
        exempt.tests_note = Some("nothing tests wasm32".to_string());
        exempt.tests_checked = 0;
        exempt.tests_owed = 0;
        let v = super::cells_verdict(
            &[
                exempt,
                super::CellReport::skipped(&cell("win", "x86_64-pc-windows-msvc"), 161, 74, 1),
            ],
            5,
            false,
            false,
        );
        assert!(
            v.text.contains("1 not owed a test pass: wasm-cpu"),
            "{}",
            v.text
        );
        assert!(
            !v.text.contains("win, wasm-cpu") && !v.text.contains("wasm-cpu, win"),
            "a cell nothing was compiled for was called exempt: {}",
            v.text
        );
    }

    /// THE WHOLE (skipped × narrowed × failed) SPACE, because the claim must be
    /// lost by BEING one of those states rather than by someone remembering a
    /// clause — the property `aterm_verify::verdict` holds for the run as a
    /// whole. Exactly one column earns the sentence.
    #[test]
    fn exactly_one_state_earns_the_matrix_claim_and_a_skip_never_renders_like_it() {
        for skipped in [false, true] {
            for narrowed in [false, true] {
                for gate_failed in [false, true] {
                    let mut reports = vec![
                        checked_cell("mac-arm", "aarch64-apple-darwin"),
                        checked_cell("linux", "x86_64-unknown-linux-gnu"),
                    ];
                    if skipped {
                        reports[1] = super::CellReport::skipped(
                            &cell("linux", "x86_64-unknown-linux-gnu"),
                            266,
                            79,
                            2,
                        );
                    }
                    let v = super::cells_verdict(&reports, 2, narrowed, gate_failed);
                    let earned = !skipped && !narrowed && !gate_failed;
                    assert_eq!(
                        v.text.contains(super::MATRIX_CLAIM),
                        earned,
                        "skipped={skipped} narrowed={narrowed} failed={gate_failed}: {}",
                        v.text
                    );
                    assert_eq!(v.ok, !gate_failed, "only a FAILURE blocks");
                }
            }
        }
        // Green-and-complete and green-but-skipped must not render the same —
        // the property `a_skip_is_never_silently_a_pass` holds one layer up.
        let complete = vec![
            checked_cell("mac-arm", "aarch64-apple-darwin"),
            checked_cell("linux", "x86_64-unknown-linux-gnu"),
        ];
        let skipped = vec![
            checked_cell("mac-arm", "aarch64-apple-darwin"),
            super::CellReport::skipped(&cell("linux", "x86_64-unknown-linux-gnu"), 266, 79, 2),
        ];
        assert_ne!(
            super::cells_verdict(&complete, 2, false, false).text,
            super::cells_verdict(&skipped, 2, false, false).text
        );
    }

    /// A run that saw fewer cells than forge ships has not seen the matrix, and
    /// `--cell` is only TODAY's way to do that. The claim asks how many cells
    /// were reported, not which flag was typed.
    #[test]
    fn a_run_short_of_forges_cell_list_never_claims_the_matrix_flagless() {
        let v = super::cells_verdict(
            &[checked_cell("mac-arm", "aarch64-apple-darwin")],
            5,
            false,
            false,
        );
        assert!(!v.text.contains(super::MATRIX_CLAIM), "{}", v.text);
        assert!(v.text.contains("1 of forge's 5 cells"), "{}", v.text);
    }

    /// THE ROW AND ITS VERDICT LEAVE ONE EXPRESSION. The status word and the
    /// outcome were separate assignments forty lines apart and they disagreed
    /// for the life of the verb; a constructor is what makes that unrepresentable,
    /// so the constructor is what is held to it.
    #[test]
    fn a_skipped_row_says_skipped_in_both_the_column_and_the_verdict() {
        let r = super::CellReport::skipped(&cell("win", "x86_64-pc-windows-msvc"), 161, 74, 1);
        assert_eq!(r.status, super::SKIPPED_NO_STD);
        assert_eq!(r.outcome, super::CellOutcome::Skipped);
        assert_ne!(r.outcome, super::CellOutcome::Checked);
        // Nothing was compiled, so nothing may be counted as compiled.
        assert_eq!((r.checked, r.own_checked, r.tests_checked), (0, 0, 0));
        assert!(r.tests_note.is_some());
    }

    /// `cargo tree` prints the path it resolved and `workspace_root()` returns
    /// the path this process was launched with. On macOS one of those routinely
    /// says `/tmp` and the other `/private/tmp`, and a raw `starts_with` between
    /// them answers "no" for every package in the workspace — which would empty
    /// the in-repo census and make the new obligation vacuously satisfied.
    #[test]
    fn the_in_repo_test_survives_a_symlinked_root() {
        let root = crate::workspace_root();
        assert!(super::path_is_inside(&root.join("crates/aterm-gui"), &root));
        assert!(super::path_is_inside(&root, &root));
        assert!(!super::path_is_inside(std::path::Path::new("/"), &root));
    }

    /// THE LIVE FILE, against THE LIVE CELL LIST. A cell with no floor cannot be
    /// compared with anything, and `gate cells` would fail at run time — this
    /// fails in `cargo test -p xtask` instead, the moment forge gains a cell.
    #[test]
    fn every_forge_cell_has_a_floor_in_the_shipped_policy() {
        let root = crate::workspace_root();
        let text = std::fs::read_to_string(root.join(super::CELL_GATE_POLICY))
            .expect("tools/cross-cell-gate.tsv is shipped");
        let policy = super::parse_cell_policy(&text).expect("the shipped policy parses");
        for cell in aterm_forge::resolve::default_cells() {
            assert!(
                policy.floor(&cell.name).is_some(),
                "cell `{}` has no `floor` row in {}",
                cell.name,
                super::CELL_GATE_POLICY
            );
        }
        // Every excuse names a triple some cell actually resolves for, so a row
        // cannot be written against a triple this matrix has never had.
        let triples: Vec<String> = aterm_forge::resolve::default_cells()
            .into_iter()
            .map(|c| c.triple)
            .collect();
        for cdep in &policy.cdeps {
            assert!(
                cdep.triple == "*" || triples.contains(&cdep.triple),
                "{} excuses `{}` on `{}`, which is no cell's triple",
                super::CELL_GATE_POLICY,
                cdep.package,
                cdep.triple
            );
        }
        for shim in &policy.cshims {
            assert!(
                triples.contains(&shim.triple),
                "{} shims `{}` on `{}`, which is no cell's triple",
                super::CELL_GATE_POLICY,
                shim.package,
                shim.triple
            );
        }
    }

    /// THE AUDIT THE OTHER DIRECTION, which is the one a shrunken matrix walks
    /// through. `every_forge_cell_has_a_floor_in_the_shipped_policy` fails when a
    /// cell has no floor; this one fails when a FLOOR HAS NO CELL. Deleting
    /// `wasm-cpu` from `default_cells()` used to leave `gate cells` exiting 0 and
    /// printing "GREEN — all 4 cells type-check" while the orphaned
    /// `floor wasm-cpu 54` row went unmentioned — a verification floor whose own
    /// matrix can be quietly shrunk. `gate cells` fails on this at run time now;
    /// this fails in `cargo test -p xtask` first, and without compiling a cell.
    #[test]
    fn no_floor_row_outlives_the_cell_it_measures() {
        let root = crate::workspace_root();
        let text = std::fs::read_to_string(root.join(super::CELL_GATE_POLICY))
            .expect("tools/cross-cell-gate.tsv is shipped");
        let policy = super::parse_cell_policy(&text).expect("the shipped policy parses");
        let cells = aterm_forge::resolve::default_cells();
        for floor in &policy.floors {
            assert!(
                cells.iter().any(|c| c.name == floor.cell),
                "{} records a floor of {} for cell `{}`, which forge no longer has",
                super::CELL_GATE_POLICY,
                floor.packages,
                floor.cell
            );
        }
        // And exactly one row per cell: `floor()` is a `find`, so a second row
        // would be read by nobody and could hide a lower number behind a higher.
        for (i, floor) in policy.floors.iter().enumerate() {
            assert!(
                !policy.floors[..i].iter().any(|f| f.cell == floor.cell),
                "{} records more than one floor for cell `{}`",
                super::CELL_GATE_POLICY,
                floor.cell
            );
        }
    }

    // -----------------------------------------------------------------------
    // G-CELLS, THE ALWAYS-ON SUBSET
    // -----------------------------------------------------------------------

    /// THE RED FIXTURE FOR `cells-foreign`, AT THE VERB. [`super::cells_under_policy`]
    /// is `gate cells`' whole body — the member list, forge's per-cell graph, the cross
    /// compile for the cell's own triple and the verdict — driven here for ONE foreign
    /// cell, `wasm-cpu` (the cheapest to compile, and one `cells-foreign` compiles on every
    /// box). GREEN under the shipped policy first, so the red is about the plant; then
    /// RED with that cell's floor raised past its graph — a count only the COMPILE can
    /// fall short of, so the verb itself decides. SKIPS, loudly, on a box whose toolchains
    /// carry no std for the cell: nothing can be learned there either way (the fmt-sweep
    /// fixture's posture).
    #[test]
    fn a_foreign_cell_under_its_floor_fails_the_cells_verb() {
        let cell = super::foreign_cells()
            .into_iter()
            .find(|c| c.name == "wasm-cpu")
            .expect("wasm-cpu is foreign on every box in the fleet");
        if !matches!(super::cell_toolchain(&cell.triple), Ok(Some(_))) {
            eprintln!(
                "SKIP a_foreign_cell_under_its_floor_fails_the_cells_verb: no installed \
                 toolchain carries a {} std (`rustup target add {} --toolchain stable`) — this \
                 box cannot show the verb red or green.",
                cell.triple, cell.triple
            );
            return;
        }
        let root = crate::workspace_root();
        let text = std::fs::read_to_string(root.join(super::CELL_GATE_POLICY))
            .expect("tools/cross-cell-gate.tsv is shipped");
        let wanted = [cell.name.clone()];
        let shipped = super::parse_cell_policy(&text).expect("the shipped policy parses");
        assert!(
            super::cells_under_policy(&root, &shipped, &wanted),
            "`{}` must be GREEN under the shipped policy before this fixture plants anything",
            cell.name
        );
        let mut raised = super::parse_cell_policy(&text).expect("the shipped policy parses");
        for floor in &mut raised.floors {
            if floor.cell == cell.name {
                floor.packages = 1_000_000;
            }
        }
        assert!(
            !super::cells_under_policy(&root, &raised, &wanted),
            "a cell whose compile reaches fewer packages than its floor must fail the VERB"
        );
    }

    /// The COMPONENT fixture beside it: `cell_matrix_audit`, the half of the verb that
    /// needs no compiler and that every run — narrowed or not — asks of forge's whole
    /// cell list, so it is shown red on every box, cross std or not.
    ///
    /// GREEN FIRST, over the shipped policy and the shipped matrix, so a run
    /// where the audit could not fail for some unrelated reason cannot be read
    /// as proof. Then RED three ways, one per thing the audit is for.
    #[test]
    fn a_shrunken_matrix_or_a_missing_floor_fails_the_cells_audit() {
        let root = crate::workspace_root();
        let text = std::fs::read_to_string(root.join(super::CELL_GATE_POLICY))
            .expect("tools/cross-cell-gate.tsv is shipped");
        let policy = super::parse_cell_policy(&text).expect("the shipped policy parses");
        let cells = aterm_forge::resolve::default_cells();
        assert!(
            super::cell_matrix_audit(&cells, &policy.floors).is_empty(),
            "the shipped policy must be CLEAN before this fixture plants anything — otherwise \
             the red below proves nothing about the plant"
        );
        let clone = |fs: &[super::CoverageFloor]| -> Vec<super::CoverageFloor> {
            fs.iter()
                .map(|f| super::CoverageFloor {
                    cell: f.cell.clone(),
                    packages: f.packages,
                })
                .collect()
        };

        // ONE: the matrix loses a cell and its floor row is orphaned. This is
        // the exact silence a judge measured — `gate cells` printing
        // "GREEN — all 4 cells type-check" over a five-cell policy file.
        let shrunk: Vec<_> = cells
            .iter()
            .filter(|c| c.name != "wasm-cpu")
            .cloned()
            .collect();
        let red = super::cell_matrix_audit(&shrunk, &policy.floors);
        assert!(
            !red.is_empty() && red.iter().any(|l| l.contains("wasm-cpu")),
            "a floor row whose cell left the matrix must FAIL and name it: {red:?}"
        );

        // TWO: the cell keeps its place and loses its floor, so its coverage
        // could fall to nothing with nothing to compare it against.
        let mut floorless = clone(&policy.floors);
        floorless.retain(|f| f.cell != "win");
        let red = super::cell_matrix_audit(&cells, &floorless);
        assert!(
            !red.is_empty() && red.iter().any(|l| l.contains("win")),
            "a cell with no floor row must FAIL and name it: {red:?}"
        );

        // THREE: two rows for one cell, where `floor()` is a `find` and only the
        // first is ever read — so the lower number hides behind the higher.
        let mut doubled = clone(&policy.floors);
        doubled.push(super::CoverageFloor {
            cell: "win".to_string(),
            packages: 1,
        });
        assert!(
            !super::cell_matrix_audit(&cells, &doubled).is_empty(),
            "a second floor row for one cell must FAIL: only the first is read"
        );
    }

    /// THE DECISION OF 2026-09-17, PINNED: the always-on cross-triple compile is
    /// exactly forge's cells whose triple no box in this fleet runs natively,
    /// and the WHOLE matrix stays opt-in.
    ///
    /// Both halves are load-bearing and both are asserted here, because the
    /// failure this gate exists to stop is a subset that quietly becomes
    /// something else: too small and Windows stops being compiled anywhere (the
    /// four hand-found breaks of 2026-09-16); too large and every tier of the
    /// merge gate inherits a cell whose verdict depends on which box ran it,
    /// which is how a gate stops being read at all.
    #[test]
    fn the_always_on_cell_subset_is_every_cell_no_box_in_this_fleet_hosts() {
        let all = aterm_forge::resolve::default_cells();
        let foreign = super::foreign_cells();
        assert!(
            !foreign.is_empty(),
            "an empty subset is `gate cells` with no `--cell`, which is the WHOLE matrix"
        );
        // DERIVED, not typed: the subset is exactly the complement of the fleet's
        // host triples inside forge's own cell list.
        let expected: Vec<String> = all
            .iter()
            .filter(|c| !super::FLEET_HOST_TRIPLES.contains(&c.triple.as_str()))
            .map(|c| c.name.clone())
            .collect();
        let got: Vec<String> = foreign.iter().map(|c| c.name.clone()).collect();
        assert_eq!(got, expected);
        // AND IT IS NOT VACUOUS: every triple this const names must be a triple
        // the matrix really has, or the complement means nothing. A stale row
        // here shrinks the subset in silence.
        for triple in super::FLEET_HOST_TRIPLES {
            assert!(
                all.iter().any(|c| c.triple == *triple),
                "FLEET_HOST_TRIPLES names `{triple}`, which is no cell's triple: it can only \
                 narrow the always-on subset by accident"
            );
        }
        // THE TRIPLE THAT ROTTED IS IN IT. Windows is the reason this gate
        // exists; wasm is the other thing no box compiles natively.
        assert!(
            foreign.iter().any(|c| c.triple == "x86_64-pc-windows-msvc"),
            "the Windows cell is the whole reason `cells-foreign` runs on every tier"
        );
        assert!(
            foreign.iter().any(|c| c.triple == "wasm32-unknown-unknown"),
            "no box in this fleet runs wasm32 natively either"
        );
    }

    /// THE INVARIANT THAT MAKES THE SUBSET MEAN THE SAME THING ON EVERY BOX,
    /// asked of WHICHEVER MACHINE IS RUNNING THIS TEST rather than of a comment.
    ///
    /// [`FLEET_HOST_TRIPLES`] is a claim about machines, and the only machines
    /// that can check it are the machines. If this box runs a cell's triple
    /// natively and that triple is not in the const, the always-on gate is
    /// compiling a NATIVE cell here and a CROSS cell elsewhere — different
    /// `cshim` decisions, a `floor` that counts host artifacts — and the merge
    /// gate would be answering a different question depending on who ran it. That
    /// is the state the whole matrix is in today, and this test is what keeps the
    /// subset out of it.
    #[test]
    fn no_cell_this_box_runs_natively_is_in_the_always_on_subset() {
        let host = super::rustc_host_triple().expect("`rustc -vV` reports a host triple");
        for cell in super::foreign_cells() {
            assert_ne!(
                cell.triple, host,
                "cell `{}` is NATIVE on this box ({host}), so the always-on subset does not mean \
                 the same thing here as elsewhere. Add `{host}` to FLEET_HOST_TRIPLES — the const \
                 is a list of the triples this fleet's boxes run, and this machine is evidence \
                 it is missing one.",
                cell.name
            );
        }
    }

    /// THE LIBC ORACLE'S CELL TABLE AND THIS CONST ARE ONE CLAIM (2026-09-20).
    /// `libc-oracle/run.sh` decides one cell per box — the cell the box is
    /// native to, the rule [`FLEET_HOST_TRIPLES`] applies to the forge cells —
    /// and it carries its cell table twice: as `cell_table` in the shell driver
    /// and as `CELLS` in `libc-oracle/gen/symgate.py`, which the negative
    /// controls import. Three files naming the fleet's machines, none able to
    /// see the others: this test reads all three, so a host added to any one
    /// of them is red here until the other two agree.
    #[test]
    fn the_libc_oracle_cell_table_agrees_with_the_fleet() {
        let root = crate::workspace_root();
        let sh = std::fs::read_to_string(root.join("libc-oracle/run.sh"))
            .expect("libc-oracle/run.sh is readable");
        let py = std::fs::read_to_string(root.join("libc-oracle/gen/symgate.py"))
            .expect("libc-oracle/gen/symgate.py is readable");
        type Row = (String, String, Option<String>);
        // run.sh: `cell_table='…'`, one `triple cell host` row per line, `none`
        // for a cell no box hosts.
        let sh_block = sh
            .split_once("cell_table='")
            .and_then(|(_, rest)| rest.split_once('\''))
            .map(|(block, _)| block)
            .expect("run.sh carries cell_table='…'");
        let sh_rows: Vec<Row> = sh_block
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                let f: Vec<&str> = l.split_whitespace().collect();
                assert_eq!(
                    f.len(),
                    3,
                    "a run.sh cell_table row is `triple cell host`: {l:?}"
                );
                let host = (f[2] != "none").then(|| f[2].to_string());
                (f[0].to_string(), f[1].to_string(), host)
            })
            .collect();
        // symgate.py: `CELLS = [ ('triple', 'cell' | None, 'host' | None), … ]`,
        // where a None cell is the zero-surface row run.sh spells `zero`.
        let py_block = py
            .split_once("CELLS = [")
            .and_then(|(_, rest)| rest.split_once(']'))
            .map(|(block, _)| block)
            .expect("symgate.py carries CELLS = […]");
        let field = |s: &str| -> Option<String> {
            let s = s.trim();
            (s != "None").then(|| s.trim_matches('\'').to_string())
        };
        let py_rows: Vec<Row> = py_block
            .lines()
            .map(str::trim)
            .filter(|l| l.starts_with('('))
            .map(|l| {
                let inner = l
                    .trim_start_matches('(')
                    .trim_end_matches(',')
                    .trim_end_matches(')');
                let f: Vec<&str> = inner.split(',').collect();
                assert_eq!(
                    f.len(),
                    3,
                    "a symgate.py CELLS row is (triple, cell, host): {l:?}"
                );
                (
                    field(f[0]).expect("a triple"),
                    field(f[1]).unwrap_or_else(|| "zero".to_string()),
                    field(f[2]),
                )
            })
            .collect();
        assert!(sh_rows.len() >= 6, "the cell table lost rows: {sh_rows:?}");
        assert_eq!(
            sh_rows, py_rows,
            "run.sh's cell_table and symgate.py's CELLS must be the same rows in the same order"
        );
        for (triple, _, host) in &sh_rows {
            match host {
                Some(h) => {
                    assert_eq!(
                        h, triple,
                        "a cell is decided where it is native, so `{triple}`'s host can only be itself"
                    );
                    assert!(
                        super::FLEET_HOST_TRIPLES.contains(&triple.as_str()),
                        "the oracle says a fleet box hosts `{triple}`; FLEET_HOST_TRIPLES does not list it"
                    );
                }
                None => assert!(
                    !super::FLEET_HOST_TRIPLES.contains(&triple.as_str()),
                    "FLEET_HOST_TRIPLES lists `{triple}`, so the oracle's table must name it as \
                     that cell's host, not `none`"
                ),
            }
        }
        for triple in super::FLEET_HOST_TRIPLES {
            assert!(
                sh_rows.iter().any(|(t, _, h)| t == triple && h.is_some()),
                "FLEET_HOST_TRIPLES names `{triple}`, which the libc oracle's cell table does not host"
            );
        }
    }

    // -----------------------------------------------------------------------
    // G-CELLS, THE TEST-TARGET PASS
    // -----------------------------------------------------------------------

    /// THE HOST CELL NAMES NO RUSTUP TOOLCHAIN AND NO --target, and a cross
    /// cell names both. The first half is the 2026-09-18 fix: the test-target
    /// pass exported `RUSTUP_TOOLCHAIN="repo pin (rust-toolchain.toml)"` — a
    /// label, as a toolchain name — and pinned `--target` to the native triple,
    /// which `.cargo/config.toml`'s COROLLARY says strips `-Ztrust-verify=off`
    /// from every host unit. Both passes now build the command in ONE place,
    /// and this holds that place to its own doc comment.
    #[test]
    fn the_host_cell_command_names_no_rustup_toolchain_and_no_target() {
        let host = super::cell_check_command(true, None, "aarch64-apple-darwin");
        let args: Vec<String> = host
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(!args.iter().any(|a| a == "--target"), "{args:?}");
        assert!(!args.iter().any(|a| a.starts_with("repo pin")), "{args:?}");
        assert!(
            host.get_envs().all(|(k, _)| k != "RUSTUP_TOOLCHAIN"),
            "the host cell must not export RUSTUP_TOOLCHAIN"
        );
        assert_eq!(args.last().map(String::as_str), Some("check"));
        // The program is the host driver, never a bare `cargo` (the store's
        // targo on a box with no cargo at all); under targo the lane is named.
        let driver = crate::driver::cargo_driver();
        assert_eq!(host.get_program(), driver.program.as_os_str());
        assert_eq!(
            args.iter().any(|a| a == "--unverified"),
            driver.is_targo,
            "{args:?} vs {driver:?}"
        );

        // A cross cell drives THE TOOLCHAIN'S OWN cargo and rustc, by path — never
        // a `cargo` off PATH trusted to honour RUSTUP_TOOLCHAIN (Homebrew's does not).
        let stable = super::RustupToolchain {
            name: "stable".to_string(),
            cargo: PathBuf::from("/r/toolchains/stable/bin/cargo"),
            rustc: PathBuf::from("/r/toolchains/stable/bin/rustc"),
        };
        let cross = super::cell_check_command(false, Some(&stable), "x86_64-unknown-linux-gnu");
        let args: Vec<String> = cross
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, ["check", "--target", "x86_64-unknown-linux-gnu"]);
        assert_eq!(cross.get_program(), stable.cargo.as_os_str());
        let env = |key: &str| {
            cross
                .get_envs()
                .find(|(k, _)| *k == key)
                .and_then(|(_, v)| v.map(std::ffi::OsStr::to_os_string))
        };
        assert_eq!(env("RUSTC"), Some(stable.rustc.clone().into_os_string()));
        assert_eq!(env("RUSTUP_TOOLCHAIN"), Some("stable".into()));
    }

    /// `--exclude` takes NAMES, and four packages in this workspace are named
    /// something other than their directory, so the list has to come from
    /// `cargo metadata` rather than from a glob over `crates/`.
    #[test]
    fn member_names_come_out_of_cargo_metadata() {
        let json = r#"{"packages":[
            {"name":"aterm","id":"path+file:///w/crates/aterm#0.87.0"},
            {"name":"libc","id":"path+file:///w/crates/aterm-libc#libc@0.2.186"},
            {"name":"xtask","id":"path+file:///w/crates/xtask#0.87.0"}
        ],"workspace_root":"/w"}"#;
        let names = super::parse_member_names("/s/targo", json).expect("parses");
        assert!(names.contains("libc"), "the NAME, not the directory");
        assert_eq!(names.len(), 3);
        // A run that produced no members at all must be an error, never an
        // empty selection: `--workspace` minus nothing would then compile the
        // whole tree for a cross triple, and minus everything would compile
        // nothing and exit 0.
        assert!(super::parse_member_names("/s/targo", r#"{"packages":[]}"#).is_err());
        assert!(super::parse_member_names("/s/targo", r#"{"nope":1}"#).is_err());
        // The refusal names the driver that ran, not a `cargo` that did not.
        let err = super::parse_member_names("/s/targo", "not json at all").unwrap_err();
        assert!(
            err.starts_with("`/s/targo metadata --no-deps` did not print JSON"),
            "{err}"
        );
        assert!(!err.contains("`cargo metadata`"), "{err}");
    }

    /// The selection is the INTERSECTION with the cell's graph, and the excludes
    /// are its complement — so a member the cell does not carry is never
    /// compiled for that cell's triple (`aterm-objc` for Windows), and a graph
    /// package that is not a member (a `vendor/` path dependency) is never
    /// handed to `--exclude`, which would be a hard cargo error.
    #[test]
    fn the_test_pass_selects_the_members_this_cell_carries_and_excludes_the_rest() {
        let members: std::collections::BTreeSet<String> =
            ["aterm", "aterm-gui", "aterm-objc", "xtask"]
                .iter()
                .map(|s| (*s).to_string())
                .collect();
        let graph: std::collections::BTreeSet<String> = ["aterm", "aterm-gui", "winit"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        let patched = std::collections::BTreeSet::new();
        let (selected, excluded) = super::cell_test_selection(&members, &graph, &patched);
        assert_eq!(selected, vec!["aterm".to_string(), "aterm-gui".to_string()]);
        assert_eq!(
            excluded,
            vec!["aterm-objc".to_string(), "xtask".to_string()]
        );
        assert!(
            !excluded.contains(&"winit".to_string()),
            "`winit` is in the graph but is not a workspace member: `--exclude winit` is an error"
        );
    }

    /// A MEMBER A `[patch.crates-io]` ENTRY POINTS AT IS NEVER EXCLUDED, even
    /// when the cell's graph does not carry it. Excluding it drops the PATCH
    /// from the resolve and every consumer of the patched name falls back to the
    /// registry — measured on the `mac-x64` cell as `error: failed to select a
    /// version for arrayvec`, which is the whole reason this third input exists.
    /// It stays out of `selected`, so the pass is owed nothing extra for it.
    #[test]
    fn a_patched_member_outside_the_cells_graph_is_still_never_excluded() {
        let members: std::collections::BTreeSet<String> =
            ["aterm", "arrayvec", "profiling", "xtask"]
                .iter()
                .map(|s| (*s).to_string())
                .collect();
        let graph: std::collections::BTreeSet<String> =
            ["aterm"].iter().map(|s| (*s).to_string()).collect();
        let patched: std::collections::BTreeSet<String> = ["arrayvec", "profiling", "winit"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        let (selected, excluded) = super::cell_test_selection(&members, &graph, &patched);
        assert_eq!(selected, vec!["aterm".to_string()]);
        assert_eq!(
            excluded,
            vec!["xtask".to_string()],
            "`arrayvec` and `profiling` are patch targets: excluding either takes its patch out \
             of the workspace resolve"
        );
    }

    /// The parser reads the real manifest's patch table, and the names it must
    /// find are the ones whose absence breaks a cell. An unreadable manifest is
    /// an ERROR, never an empty answer that reads as "this workspace patches
    /// nothing".
    #[test]
    fn the_patch_table_is_read_from_the_root_manifest() {
        let names = super::patched_crate_names(&super::workspace_root())
            .expect("the workspace root has a Cargo.toml");
        for want in ["arrayvec", "libc", "winit", "profiling"] {
            assert!(
                names.contains(want),
                "`{want}` is a [patch.crates-io] key and must be read; got {names:?}"
            );
        }
        assert!(
            !names.contains("aterm"),
            "`aterm` is a workspace member, not a patch key: {names:?}"
        );
        let err = super::patched_crate_names(Path::new("/nope/not/a/workspace"))
            .expect_err("an unreadable manifest is an error");
        assert!(err.contains("could not read"), "{err}");
    }

    /// The two exemptions are NARROW and they are SAID. A cell that is exempt
    /// prints why; every other cell owes the pass, and nothing may quietly
    /// become exempt by being slow or inconvenient.
    #[test]
    fn only_the_host_cell_and_the_wasm_cells_are_exempt_from_the_test_pass() {
        assert!(super::test_pass_exemption("x86_64-pc-windows-msvc", false).is_none());
        assert!(super::test_pass_exemption("aarch64-apple-darwin", false).is_none());
        assert!(super::test_pass_exemption("x86_64-unknown-linux-gnu", false).is_none());
        let host = super::test_pass_exemption("x86_64-unknown-linux-gnu", true)
            .expect("the host cell is exempt");
        assert!(host.contains("cargo test"), "{host}");
        let wasm = super::test_pass_exemption("wasm32-unknown-unknown", false)
            .expect("the wasm cells are exempt");
        assert!(wasm.contains("dev-dependencies"), "{wasm}");
        // THE LIVE CELL LIST: at most two of forge's eight cells may be exempt on
        // any box, and the three shipping triples are never among them unless
        // one of them IS the box. A third exemption appearing here means someone
        // widened the escape hatch.
        let exempt: Vec<String> = aterm_forge::resolve::default_cells()
            .into_iter()
            .filter(|c| super::test_pass_exemption(&c.triple, false).is_some())
            .map(|c| c.name)
            .collect();
        assert_eq!(exempt, vec!["wasm-cpu".to_string(), "wasm-gpu".to_string()]);
    }
}
