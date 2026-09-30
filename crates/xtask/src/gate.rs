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
//!   — the same function `targo --unverified forge check` runs, so the gate and the hand-run
//!   tool cannot disagree. `tools/forge-budget.tsv` is the authority on the
//!   numbers it ratchets.
//! - `cells [--cell NAME]…`: every forge cell, type-checked by a compiler for its
//!   own triple, with the build-script shims `tools/cross-cell-gate.tsv` lists.
//!   The host cell runs on the pinned toolchain (`driver.rs`), the cross cells on
//!   rustup's `stable`.
//! - `cells-foreign`: `cells` narrowed to the cells no box in this fleet hosts
//!   ([`FLEET_HOST_TRIPLES`]), whose verdict is the same wherever it runs.
//!
//! A verb that could not look says so — NOT RUN, COULD NOT RUN, SKIPPED — and
//! never prints a pass for it: see [`LaneVerdict`] and [`CellOutcome`].

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use crate::driver::{CargoDriver, export_driver_as_cargo, rustc_host_triple};
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

/// The verbs retired on 2026-09-27 that still have a home, each with the package
/// and argument that run it now, so a caller still typing the old name (the dated
/// records under `docs/` keep it) is told where it went.
const RETIRED_VERBS: &[(&str, &str, &str)] = &[
    ("mainloop", "aterm-census", "--mainloop"),
    ("lockorder", "aterm-census", "--locks"),
    ("wasmloop", "aterm-census", "--wasm"),
    ("scope", "aterm-census", "--scope"),
    ("lazyinit", "aterm-census", "--lazy-init"),
    ("perf", "xtask", "perf"),
];

/// What `xtask gate` says when `check` names no verb it dispatches: the usage
/// line for a bare `gate`, the verb list for an unknown one, and the new home
/// of a retired verb.
fn unknown_verb_message(check: Option<&str>) -> String {
    let Some(verb) = check else {
        return format!("usage: xtask gate <{}>", verb_names().join("|"));
    };
    let moved = RETIRED_VERBS
        .iter()
        .find(|(old, ..)| *old == verb)
        .map(|(_, package, arg)| format!(" (now: targo --unverified run -p {package} -- {arg})"))
        .unwrap_or_default();
    format!(
        "xtask gate: no verb `{verb}`{moved} — verbs: {}",
        verb_names().join(", ")
    )
}

/// `rest` is everything after the verb name.
pub(crate) fn run(check: Option<&str>, rest: &[String]) -> ExitCode {
    let Some((_, verb)) = VERBS.iter().find(|(name, _)| Some(*name) == check) else {
        eprintln!("{}", unknown_verb_message(check));
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
// docs); this verb and `targo --unverified forge check` both call `check::check_report`. It
// compiles nothing: it reads `Cargo.lock`, `vendor/`, `vendor/forge.toml`,
// `tools/forge-budget.tsv` and one offline `cargo tree` per cell.

fn gate_forge() -> bool {
    // forge resolves its cells with `$CARGO tree`; hand it the pinned driver
    // when this process was started without one.
    if let Err(why) = export_driver_as_cargo() {
        eprintln!("gate forge: COULD NOT RUN — {why}");
        return false;
    }
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
/// The command itself is not echoed: a clean pass prints nothing, and every
/// other outcome's line names the program.
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

/// Where [`find_rustup`] looks after PATH, for the sentence a gate prints when it
/// finds none. ONE copy, so the skip cannot name a place the search does not visit.
pub(crate) const RUSTUP_LOOKED: &str = "on PATH, in $CARGO_HOME/bin, in ~/.cargo/bin and in \
                                        Homebrew's rustup keg (/opt/homebrew/opt/rustup/bin)";

/// THIS BOX'S `rustup`, by path — PATH first, then where rustup installs itself.
///
/// rustup-init puts `rustup` in `$CARGO_HOME/bin` (`~/.cargo/bin` by default) and
/// leaves PATH to the shell's rc files, and a machine provisioned the product's way
/// (atpkg's store, which is why `rust-toolchain.toml` needs no rustup) has no reason
/// to put that directory on PATH at all. The cross gates asked PATH alone until
/// 2026-09-27. Measured on the owner's Mac that day, rustup in `~/.cargo/bin` and all
/// four foreign stds installed in `stable`: `gate cells-foreign` printed five
/// `SKIPPED(no-std)` rows, each telling the reader to `rustup target add` a std that
/// was already there, and `tools/verify.sh` withheld the merge contract on every run
/// — a skip whose named fix could not clear it.
pub(crate) fn find_rustup() -> Option<PathBuf> {
    find_rustup_in(
        std::env::var_os("PATH").as_deref(),
        std::env::var_os("CARGO_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

/// [`find_rustup`] over the environment it is handed: the first EXECUTABLE
/// candidate, as `command -v` and `execvp` pick — a stray non-executable file named
/// `rustup` early on PATH is passed over, not handed to every rustup call as an
/// EACCES that turns a box fact into a FAILED gate.
fn find_rustup_in(
    path: Option<&std::ffi::OsStr>,
    cargo_home: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
) -> Option<PathBuf> {
    rustup_candidates(path, cargo_home, home)
        .into_iter()
        .find(|p| is_executable_file(p))
}

/// Would `execvp` run `p`: a regular file with an execute bit.
fn is_executable_file(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

/// [`find_rustup`]'s search order, from the environment it is handed. Relative PATH
/// entries are dropped: a rustup found through one would depend on the cwd the gate
/// happened to run in.
fn rustup_candidates(
    path: Option<&std::ffi::OsStr>,
    cargo_home: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = path
        .map(|p| {
            std::env::split_paths(p)
                .filter(|dir| dir.is_absolute())
                .map(|dir| dir.join("rustup"))
                .collect()
        })
        .unwrap_or_default();
    let absolute = |v: Option<&std::ffi::OsStr>| v.map(PathBuf::from).filter(|p| p.is_absolute());
    if let Some(cargo_home) = absolute(cargo_home) {
        out.push(cargo_home.join("bin/rustup"));
    }
    if let Some(home) = absolute(home) {
        out.push(home.join(".cargo/bin/rustup"));
    }
    out.push(PathBuf::from("/opt/homebrew/opt/rustup/bin/rustup"));
    out
}

/// A neutral build cwd for a cross-compile, because cargo discovers config — and
/// rustup its toolchain pin — by walking the cwd upward, and both of this repo's are
/// Trust's: the pin names the Trust toolchain and `.cargo/config.toml` carries
/// Trust-only `-Z` flags. The config's table is `cfg(trust_verify)`-scoped since
/// 2026-08-30, so an upstream compiler no longer sees its flags; when it was a
/// per-triple table, an in-repo `--target` build died at flag-parse on its first HOST
/// unit with an error that named no crate of ours. `tools/wasm-bench/run.sh` uses the
/// same trick. Kept: an upstream lane owes nothing to this repo's Trust settings.
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
// G-CELLS: every forge cell, type-checked by a compiler for its own triple
// ---------------------------------------------------------------------------

/// The build-script overrides `gate cells` may apply. Workspace-relative, and
/// read-only to this gate.
const CELL_GATE_POLICY: &str = "tools/cross-cell-gate.tsv";

/// One `cshim` row: a C-bearing build script this gate REPLACES, for one
/// `cargo check` on ONE triple, with cargo's own build-script override passed on
/// the command line. Honest only where the script emits nothing a compiler reads
/// (no `rustc-cfg`, no `rustc-env`, no generated source): the type-check is then
/// the one a cross C toolchain would give, because `cargo check` never links.
/// The row pins `name@version`, so a bump makes it dead and a dead row fails the
/// gate until somebody reads the new script.
struct CBuildShim {
    package: String,
    version: String,
    /// Never `*`: faithfulness is a claim about what a script emits FOR A TARGET.
    triple: String,
    /// The package's `links` key, which cargo's override table is addressed by.
    links: String,
    // The row's 5th column, its reason, is required by the parser and read by
    // whoever edits the file; the gate's log points at the file instead of
    // reprinting a paragraph per shim per cell on every run.
}

/// [`CELL_GATE_POLICY`], parsed.
struct CellPolicy {
    cshims: Vec<CBuildShim>,
}

impl CellPolicy {
    /// The overrides this policy declares for `triple`, each with its row index
    /// (two rows for one package on two triples are two obligations).
    fn shims_for(&self, triple: &str) -> Vec<(usize, &CBuildShim)> {
        self.cshims
            .iter()
            .enumerate()
            .filter(|(_, c)| c.triple == triple)
            .collect()
    }
}

/// The triples where an override is refused on EVERY box: `zstd-sys` emits
/// `rustc-cfg=feature="std"` for wasm32 and hermit and nothing elsewhere, so a
/// shim there would type-check a different crate. Only box-independent
/// refusals live here — the host's own triple is [`host_shim_refusal`], asked
/// per cell, because the committed file is read on every box.
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

/// The cell whose triple is this box's own declines its overrides: it can RUN
/// the build script, and cargo applies a `[target.<host>.<links>]` override to
/// the native (no `--target`) cell too, so applying one would suppress the real
/// script and call the result coverage. Every other cell keeps its overrides,
/// and the row stays live — the box that needs it is some other box.
fn host_shim_refusal(triple: &str, host: &str) -> Option<String> {
    (triple == host).then(|| {
        format!(
            "`{triple}` is this machine's own triple, where the build script RUNS — a cell that \
             can run a build script needs no override, and applying one there would suppress the \
             real script and call the result coverage"
        )
    })
}

/// Parse [`CELL_GATE_POLICY`]: `cshim <name>@<version> <triple> <links> <why>`,
/// TAB-separated, `#` comments and blank lines ignored. Strict on purpose: a
/// row this cannot read is a COULD-NOT-RUN, never a silently skipped line.
fn parse_cell_policy(text: &str) -> Result<CellPolicy, String> {
    let mut cshims = Vec::new();
    for (n, raw) in text.lines().enumerate() {
        let line = raw.trim_end();
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        if cols[0] != "cshim" {
            return Err(format!(
                "{CELL_GATE_POLICY}:{}: unknown row kind `{}` — the only kind is `cshim`",
                n + 1,
                cols[0]
            ));
        }
        if cols.len() < 5 {
            return Err(format!(
                "{CELL_GATE_POLICY}:{}: a `cshim` row takes 5 TAB-separated columns, found {}",
                n + 1,
                cols.len()
            ));
        }
        let Some((package, version)) = cols[1].split_once('@') else {
            return Err(format!(
                "{CELL_GATE_POLICY}:{}: a `cshim` key is `name@version` (the exact version whose \
                 build script was read), not `{}`",
                n + 1,
                cols[1]
            ));
        };
        if cols[2] == "*" {
            return Err(format!(
                "{CELL_GATE_POLICY}:{}: `cshim` rows name ONE triple — an override is a claim \
                 about what a build script emits FOR A TARGET, and `zstd-sys` emits a different \
                 set on wasm32 than anywhere else.",
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
        });
    }
    Ok(CellPolicy { cshims })
}

/// The package name inside a cargo `package_id`. `registry+…#serde@1.0.228` and
/// `path+…/foo#bar@0.1.0` carry `name@version` after the `#`, but a path package
/// whose directory IS its name abbreviates to `path+…/serde#1.0.228` — no name,
/// just a version — so the name is then the last path segment. Reading the
/// version as the name once scored 42 packages as never-checked.
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

/// The toolchain names in `rustup toolchain list`'s stdout, without the
/// ` (active, default)` marker rustup prints on the same line.
fn parse_toolchain_names(listing: &str) -> Vec<String> {
    listing
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// Which compiler checks a cell: the pinned host driver for the box's own
/// triple, a rustup toolchain carrying the triple's std for every other.
#[derive(Clone, Copy)]
enum CellCompiler<'a> {
    Host(&'a CargoDriver),
    Cross(&'a RustupToolchain),
}

/// The `check` command for ONE cell. Both cell passes build theirs here, so they
/// cannot disagree about it.
///
/// THE HOST CELL goes through the pinned driver with NO `RUSTUP_TOOLCHAIN` and
/// NO `--target`: an explicit target makes cargo withhold `.cargo/config.toml`'s
/// `[target.'cfg(trust_verify)']` rustflags (`-Ztrust-verify=off`) from host
/// units, which then verify strictly and fail. A CROSS CELL is the toolchain's
/// own `cargo` with `--target <triple>`, run from a neutral cwd — a STOCK
/// EXCEPTION, because the Trust sysroot carries only the host std (measured
/// 2026-09-28: `targo --unverified check --target <wasm32|windows-gnu|
/// *-linux-gnu|x86_64-apple-darwin>` stops at error[E0463], no `std`/`core`).
fn cell_check_command(compiler: CellCompiler<'_>, triple: &str) -> Command {
    match compiler {
        CellCompiler::Host(driver) => driver.command("check"),
        CellCompiler::Cross(toolchain) => {
            let mut cmd = toolchain.cargo();
            cmd.arg("check").arg("--target").arg(triple);
            cmd
        }
    }
}

/// A rustup toolchain resolved to its OWN drivers, by path — never a `cargo`
/// off PATH trusted to honour `RUSTUP_TOOLCHAIN`, which only rustup's proxies
/// read (Homebrew's cargo ahead of `~/.cargo/bin` compiled three cells with no
/// cross std, 2026-09-24). `RUSTC` pins the compiler for cargo and for every
/// build script it runs.
#[derive(Clone, Debug, PartialEq, Eq)]
struct RustupToolchain {
    name: String,
    cargo: PathBuf,
    rustc: PathBuf,
}

impl RustupToolchain {
    /// `name`'s `cargo` and `rustc`, as `rustup which --toolchain` answers them —
    /// asked of the `rustup` at [`find_rustup`]'s path, never of PATH's.
    fn resolve(rustup: &Path, name: &str) -> Result<Self, String> {
        let which = |tool: &str| -> Result<PathBuf, String> {
            let out = Command::new(rustup)
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
fn cell_toolchain(triple: &str) -> Result<CellToolchain, String> {
    let Some(rustup) = find_rustup() else {
        return Ok(CellToolchain::NoRustup);
    };
    let carries = |tc: &str| -> bool {
        Command::new(&rustup)
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
            RustupToolchain::resolve(&rustup, &pinned).map(CellToolchain::Found)
        } else {
            Err(format!(
                "$ATERM_CELL_TOOLCHAIN names `{pinned}`, which has no {triple} std. Install it \
                 (`rustup target add {triple} --toolchain {pinned}`) or unset the variable and let \
                 the gate pick."
            ))
        };
    }
    let list = Command::new(&rustup)
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
    match names.into_iter().find(|tc| carries(tc)) {
        Some(tc) => RustupToolchain::resolve(&rustup, &tc).map(CellToolchain::Found),
        None => Ok(CellToolchain::NoStd),
    }
}

/// What [`cell_toolchain`] found for a cross cell. `NoStd` and `NoRustup` are both
/// skips — nothing is compiled — but they are different facts with different fixes,
/// and one `Option` spelled them the same: a box with no rustup was told to
/// `rustup target add` (see [`find_rustup`]).
#[derive(Debug)]
enum CellToolchain {
    /// An installed toolchain carries the triple's std.
    Found(RustupToolchain),
    /// rustup is here, and none of its toolchains carries the triple's std.
    NoStd,
    /// No rustup anywhere [`find_rustup`] looks, so no toolchain could be asked.
    NoRustup,
}

/// Why a cell was SKIPPED — the two ways nothing can be compiled for it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SkipCause {
    NoStd,
    NoRustup,
}

impl SkipCause {
    /// The STATUS column word for a row skipped for this cause.
    const fn status(self) -> &'static str {
        match self {
            Self::NoStd => SKIPPED_NO_STD,
            Self::NoRustup => SKIPPED_NO_RUSTUP,
        }
    }
}

/// The STATUS word a cell wears when no installed toolchain carries its std.
/// ONE definition, spelled at the only place such a row is built
/// ([`CellReport::skipped`]) and read by the tests that hold the row to it.
const SKIPPED_NO_STD: &str = "SKIPPED(no-std)";

/// The STATUS word a cell wears when there is no rustup to ask for a toolchain.
const SKIPPED_NO_RUSTUP: &str = "SKIPPED(no-rustup)";

/// What one cell's run DECIDED. Three-valued, like [`LaneVerdict`]: a cell no
/// compiler started for once carried `ok: true`, and five of them printed
/// `GREEN — all 5 cells type-check` (2026-09-17). A skip is NOT a pass — and not
/// a failure either: an uninstalled std is a fact about the box that no change
/// to the tree can clear, so the run exits 0 and forfeits the matrix claim, as
/// `aterm_verify::verdict` does for a skipped stage.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CellOutcome {
    /// A compiler read this cell's graph FOR THE CELL'S OWN TRIPLE, and every
    /// obligation the policy file records for it held.
    Checked,
    /// No toolchain here can compile this triple — no installed std, or no rustup
    /// to select one with ([`SkipCause`]). NOTHING was compiled: neither a pass nor
    /// a finding, because nothing about the tree was learned.
    Skipped(SkipCause),
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
    /// Build scripts replaced by a [`CBuildShim`] on this cell.
    shimmed: Vec<String>,
    /// IN-REPO packages of this cell's graph that a compiler read, over the
    /// number it could read at all. THE NUMBER THIS GATE IS ABOUT: `linux
    /// 206/253` once read GREEN with every line of aterm's own platform code
    /// unread.
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
        cause: SkipCause,
    ) -> Self {
        Self {
            cell: cell.name.clone(),
            triple: cell.triple.clone(),
            toolchain: "-".to_string(),
            checked: 0,
            graph,
            shimmed: Vec::new(),
            own_checked: 0,
            own_total,
            own_proc_macros,
            tests_checked: 0,
            tests_owed: 0,
            tests_note: Some("nothing was compiled for this cell at all".to_string()),
            secs: 0,
            disk: None,
            status: cause.status().to_string(),
            outcome: CellOutcome::Skipped(cause),
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
    /// The compiler the library pass checked this cell with.
    compiler: CellCompiler<'a>,
    target_dir: &'a Path,
    /// The `--config` build-script overrides this cell's `cshim` rows put in
    /// force, verbatim: a second pass compiling the same graph without them
    /// would die on the same C build scripts the first pass shimmed.
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
        compiler,
        target_dir,
        shim_args,
        members,
        graph_names,
        proc_macros,
    } = *job;
    let is_host = matches!(compiler, CellCompiler::Host(_));
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
    let mut cmd = cell_check_command(compiler, &cell.triple);
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
    // Same column-zero rule as the pass above: a failing build script re-emits
    // its own output INDENTED under `Caused by:`, and `error: could not compile`
    // is cargo's summary of diagnostics already counted through the JSON stream.
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

/// THE TRIPLES SOME BOX IN THIS FLEET RUNS NATIVELY — a claim about machines,
/// not targets. A cell somebody HOSTS gets a different verdict on the box that
/// hosts it (its `cshim` rows are declined there, and a native run counts host
/// artifacts a cross run cannot produce), so only the cells no box hosts can
/// ride every tier of the merge gate as [`gate_cells_foreign`].
/// `no_cell_this_box_runs_natively_is_in_the_always_on_subset` checks this list
/// from whichever machine runs the tests, and
/// `the_libc_oracle_cell_table_agrees_with_the_fleet` holds it to the oracle's
/// own table.
const FLEET_HOST_TRIPLES: &[&str] = &[
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-unknown-linux-gnu",
];

/// The cells [`gate_cells_foreign`] compiles: every forge cell whose triple is
/// in nobody's [`FLEET_HOST_TRIPLES`], derived from forge's own cell list.
fn foreign_cells() -> Vec<aterm_forge::model::Cell> {
    aterm_forge::resolve::default_cells()
        .into_iter()
        .filter(|c| !FLEET_HOST_TRIPLES.contains(&c.triple.as_str()))
        .collect()
}

/// `xtask gate cells-foreign` — the cross-triple compile every run of the merge
/// gate makes (`StageId::ForeignCells` in `crates/aterm-verify`): `gate cells`
/// narrowed to [`foreign_cells`], the cells whose verdict is the same wherever it
/// runs. Windows and wasm are compiled on every box, on every gate run; the
/// natively-hosted cells stay behind the opt-in `cells` (`tools/verify.sh
/// --full`), covered day to day by the native lanes of the boxes that host them.
/// It needs rustup's `stable` with the foreign std targets; without them this
/// exits 0 naming the missing std, and the verify stage is a SKIP. Measured cost
/// (2026-09-27, M5 Max under load, `CARGO_INCREMENTAL=0`): 432 s and 2.6 GiB
/// cold, 8 s warm, 278 s after a core edit.
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

/// `xtask gate cells` — every forge cell ([`aterm_forge::resolve::default_cells`]
/// itself, not a copy), type-checked BY A COMPILER for its own target triple.
///
/// Each cell's ROOT PACKAGE is checked, so the feature set is cargo's own
/// resolution for that root on that triple. The HOST cell runs on the pinned
/// toolchain from the repo root with no `--target`; a CROSS cell runs on a
/// rustup toolchain carrying the triple's std, from a neutral cwd (cargo reads
/// config, and rustup the toolchain pin, from the cwd upward, and both of this
/// repo's are Trust's: [`neutral_build_cwd`]). Nothing is written inside the repo: [`cell_target_dir`]
/// refuses a target dir there, `--locked` keeps `Cargo.lock`, and the policy
/// file is read-only here.
///
/// A cell OWES, and is red without: every in-repo non-proc-macro crate in its
/// graph read by the compiler for its triple (a package count once read GREEN
/// at `linux 206/253` with none of aterm's own crates compiled); every shimmed
/// package type-checked (a mis-keyed `links` is silently ignored by cargo); no
/// cargo error at all (a C build script this box cannot run is shimmed by a
/// `cshim` row or it fails the cell); and, off the host and wasm32, a second
/// pass compiling every target — tests, benches, examples — of every workspace
/// member in its graph ([`run_cell_test_pass`]), because a `#[cfg(test)]`
/// line was otherwise never compiled for a triple the box is not.
///
/// NOT COVERED, and said in the verdict: a shimmed cell never links the bundled
/// C, no cell links or runs anything, and a crate in no cell's graph (the
/// host-only publisher, key and conformance crates) is left to the workspace
/// test stage.
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
                     usage: xtask gate cells [--cell NAME]…   (repeatable; default: every cell)"
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

/// The verb's whole body once [`CELL_GATE_POLICY`] is read.
fn cells_under_policy(root: &Path, policy: &CellPolicy, wanted: &[String]) -> bool {
    let root = root.to_path_buf();

    // THE WORKSPACE MEMBER LIST, read once and shared by every cell: the
    // test-target pass selects `--workspace` minus the members a cell's graph
    // does not carry, and `--exclude` takes package NAMES. Through the pinned
    // driver, which is also exported as `$CARGO` for forge's per-cell `cargo
    // tree` (step 1 below).
    let driver = match export_driver_as_cargo() {
        Ok(d) => d,
        Err(why) => {
            eprintln!("gate cells: COULD NOT RUN — {why}");
            return false;
        }
    };
    let metadata_args = [
        "--no-deps",
        "--locked",
        "--offline",
        "--format-version",
        "1",
    ];
    eprintln!("gate cells: host driver: {}", driver.program.display());
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
                "gate cells: COULD NOT RUN — could not run `{} metadata` ({e}).",
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

    let Some(host) = rustc_host_triple() else {
        eprintln!(
            "gate cells: COULD NOT RUN — {} did not report a host triple (`-vV`), so the native \
             cell cannot be told from the cross ones.",
            driver.trustc.display()
        );
        return false;
    };

    let mut reports: Vec<CellReport> = Vec::new();
    let mut fail = false;
    // Every `cshim` row must name a package really in some graph for its triple,
    // audited across the whole run (by row index: one package on two triples
    // is two obligations).
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
            // PRINTED where the override is really applied — just before cargo
            // runs — and not here. A cell can still SKIP between this point and
            // that one (no installed std for its triple), and "SHIMMED …"
            // followed by "nothing was compiled" reads as though the shim bought
            // something on a box where it bought nothing. The row's reason lives
            // in the policy file, not in every run's log.
            shim_notes.push(format!(
                "gate cells: cell `{}` — `{}@{}`'s build script SHIMMED (`--config \
                 target.{}.{}`; why: {CELL_GATE_POLICY})",
                cell.name, shim.package, shim.version, cell.triple, shim.links
            ));
        }
        if shim_dead {
            continue;
        }

        // 2. THE TOOLCHAIN. A SKIP is a fact about the BOX, decided before
        //    anything compiles; it says nothing was compiled, and it is NOT a
        //    pass ([`CellOutcome`]). The host cell names no rustup toolchain:
        //    the pinned driver is its compiler.
        let toolchain: Option<RustupToolchain> = if is_host {
            None
        } else {
            let skip = match cell_toolchain(&cell.triple) {
                Ok(CellToolchain::Found(tc)) => Ok(tc),
                Ok(CellToolchain::NoStd) => {
                    eprintln!(
                        "gate cells: cell `{}` SKIPPED — no installed toolchain carries a {} std, \
                         so NOTHING WAS COMPILED for it.\n\
                         gate cells:   install one:  rustup target add {} --toolchain stable\n\
                         gate cells:   (`--toolchain` is not optional — this repo's default is the \
                         Trust fork, which refuses `rustup target add`.)",
                        cell.name, cell.triple, cell.triple
                    );
                    Err(SkipCause::NoStd)
                }
                Ok(CellToolchain::NoRustup) => {
                    eprintln!(
                        "gate cells: cell `{}` SKIPPED — no rustup on this box (looked \
                         {RUSTUP_LOOKED}), so no toolchain could be asked for a {} std and \
                         NOTHING WAS COMPILED for it.\n\
                         gate cells:   install rustup (https://rustup.rs), then:  rustup target \
                         add {} --toolchain stable",
                        cell.name, cell.triple, cell.triple
                    );
                    Err(SkipCause::NoRustup)
                }
                Err(e) => {
                    eprintln!("gate cells: FAILED — cell `{}`: {e}", cell.name);
                    fail = true;
                    continue;
                }
            };
            match skip {
                Ok(tc) => Some(tc),
                Err(cause) => {
                    reports.push(CellReport::skipped(
                        cell,
                        graph_names.len(),
                        own.difference(&own_proc_macros).count(),
                        own_proc_macros.len(),
                        cause,
                    ));
                    continue;
                }
            }
        };

        let compiler = match &toolchain {
            Some(tc) => CellCompiler::Cross(tc),
            None => CellCompiler::Host(driver),
        };
        // The TOOLCHAIN column: a cross cell's rustup toolchain, or the host's
        // pinned driver.
        let toolchain_label = toolchain
            .as_ref()
            .map_or_else(|| "host driver (targo)".to_string(), |tc| tc.name.clone());

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

        let mut cmd = cell_check_command(compiler, &cell.triple);
        cmd.current_dir(&cwd)
            .env("CARGO_TARGET_DIR", &target_dir)
            .arg("--locked")
            // Without this, one dead build script hides every type error behind
            // it; with it, the compiler reads everything it still can.
            .arg("--keep-going")
            .arg("--message-format=json")
            .args(["-p", cell.package.as_str()])
            .arg("--manifest-path")
            .arg(root.join("Cargo.toml"));
        // ON THE COMMAND LINE, NOT IN A FILE: the override lives for one
        // process, shows in the command, and is triple-scoped.
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

        // 4. THE VERDICT, read from cargo's own JSON: a run whose units were all
        //    fresh prints no `Checking` line at all while still reporting every
        //    artifact.
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

        // 5. ANY OTHER `error:` cargo printed — a build script this box cannot
        //    run, a rejected flag, a broken manifest — fails the cell with its
        //    own line. COLUMN ZERO IS THE TEST: a failing build script's output
        //    is re-emitted INDENTED under `Caused by:`, and `error: could not
        //    compile` is cargo's summary of diagnostics the JSON already carried.
        for line in stderr.lines() {
            if line.starts_with("error") && !line.starts_with("error: could not compile") {
                cell_ok = false;
                if status == "GREEN" {
                    status = "CARGO-ERROR".to_string();
                }
                eprintln!("gate cells: cell `{}` FAILED — {line}", cell.name);
            }
        }

        // 6. DID THE SHIM ACTUALLY BUY ANYTHING? It fires when the override is
        //    mis-keyed (cargo silently ignores a wrong `links` name) as well as
        //    when the package failed for some other reason.
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

        // 7. THE CELL'S OWN CODE, OWED RATHER THAN REPORTED: every in-repo
        //    package in this cell's graph that is not a proc macro, read by a
        //    compiler for this triple. Machine-independent on purpose.
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

        // 8. THE TEST TARGETS. Everything above reads LIBRARIES; a
        //    `#[cfg(test)]` module is part of its crate's compilation unit, so
        //    one unix-only name costs the crate's whole test binary elsewhere.
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
                compiler,
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

        if cell_ok {
            eprintln!(
                "gate cells: cell `{}` ({}) GREEN on `{}` — {}/{} packages type-checked in {}s, \
                 INCLUDING {}/{} of aterm's own compiled crates{}.",
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

    // DEAD `cshim` ROWS — audited against the graphs this run resolved, so a
    // narrowed run, which has not resolved every cell, says it did not look.
    if narrowed {
        eprintln!(
            "gate cells: NOTE — narrowed to {}; the `cshim` rows were not audited against the \
             cells this run skipped.",
            wanted.join(", ")
        );
    } else {
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
        let tail = if r.shimmed.is_empty() {
            String::new()
        } else {
            format!(" (shimmed: {})", r.shimmed.join(", "))
        };
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
        .filter(|r| matches!(r.outcome, CellOutcome::Skipped(_)))
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
        "\ngate cells: shimmed cells are type-checked, not linked; no cell runs a binary."
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
        // THE CAUSE AND ITS FIX, NAMED TOGETHER, with the triples filled in (the
        // per-cell lines above name the same `--toolchain stable`). With no rustup
        // at all, "no installed std … `rustup target add`" sends the reader to a
        // fix that cannot clear it.
        let install = skipped
            .iter()
            .map(|r| r.triple.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
            .join(" ");
        let add = format!("`rustup target add {install} --toolchain stable`");
        let no_rustup = skipped
            .iter()
            .any(|r| r.outcome == CellOutcome::Skipped(SkipCause::NoRustup));
        let (had, fix) = if no_rustup {
            (
                format!(
                    "could not be compiled — this box has no rustup (looked {RUSTUP_LOOKED}), so \
                     no toolchain could be asked for their std"
                ),
                format!("Install rustup (https://rustup.rs), then {add}"),
            )
        } else {
            (
                "had no installed std".to_string(),
                format!("Install: {add}"),
            )
        };
        return CellsVerdict {
            text: format!(
                "{} — {} of the {} cell(s) this run selected {had}, so NO COMPILER READ THEM: \
                 {names}. {ran} cell(s) were checked: {coverage}. This run does NOT make the \
                 matrix claim about forge's {forge_cells} cells.\n\
                 gate cells: {fix}, then run again. Exits 0; `tools/verify.sh` counts it a SKIP \
                 and withholds `{}`.{scope}",
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
    let how = if reports.iter().any(|r| !r.shimmed.is_empty()) {
        ", with the C build scripts shimmed above"
    } else {
        ", with nothing shimmed"
    };
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

/// The formatter's three passes, by the names the COULD NOT RUN verdict uses.
const FMT_PASS_WORKSPACE: &str = "targo-fmt --all";
const FMT_PASS_SWEEP: &str = "trustfmt sweep";
const FMT_PASS_EDITIONS: &str = "fmt editions";

/// `gate lint` and `gate lint --fmt-only` — the same run: the formatter's three
/// passes over the tree, from THE toolchain ([`trust_toolchain`]). A pass prints
/// a line only when it found drift or could not run; a green run prints the
/// verdict alone.
fn gate_lint(args: &[String]) -> bool {
    if let Some(bad) = args.iter().find(|a| a.as_str() != "--fmt-only") {
        eprintln!(
            "gate lint: unknown argument `{bad}`; nothing was run.\n\
             usage: xtask gate lint [--fmt-only]   (the formatter; tippy is the gate's Tippy stage)"
        );
        return false;
    }
    let root = workspace_root();
    let toolchain = trust_toolchain();
    // A refused directory holds a `targo` that is not the pinned toolchain; its
    // formatter is not the one the tree is held to, so it is not run at all.
    let (verdict, not_run) = if toolchain.refused.is_some() {
        eprintln!(
            "  trustfmt: NOT RUN — {}. Nothing was format-checked.",
            toolchain.missing_targo_label()
        );
        (
            LaneVerdict::NotRun,
            vec![FMT_PASS_WORKSPACE, FMT_PASS_SWEEP, FMT_PASS_EDITIONS],
        )
    } else {
        fmt_lane(&root, &toolchain.stage2_dir)
    };
    let fix = missing_fmt_tools_fix(&toolchain);
    lint_verdict(verdict, &not_run, fix.as_deref())
}

/// The ONE fix for a stage2 that lacks `targo-fmt` or `trustfmt`, printed on
/// the verdict line (each pass's NOT RUN line only names what is missing).
/// `None` when both are there, and for a refused toolchain, whose own line
/// already carries its fix.
///
/// An explicit `$TRUST_STAGE2_BIN` is never fallen back from, so installing
/// the store's build changes nothing there: the fix is to that variable.
fn missing_fmt_tools_fix(toolchain: &aterm_verify::Toolchain) -> Option<String> {
    let dir = &toolchain.stage2_dir;
    let complete = [TRUSTFMT_DRIVER, TRUSTFMT_BIN]
        .iter()
        .all(|tool| dir.join(tool).is_file());
    if toolchain.refused.is_some() || complete {
        return None;
    }
    Some(if toolchain.store_bin.is_none() {
        format!(
            "TRUST_STAGE2_BIN={} has no {TRUSTFMT_DRIVER}/{TRUSTFMT_BIN}: point it at a stage2 \
             that has them, or unset it",
            dir.display()
        )
    } else {
        "`aterm pkg install trust`, then re-run".to_string()
    })
}

/// The three passes, every one run whatever the others found, folded with
/// [`LaneVerdict::worst`], plus the names of the passes that reached no verdict.
fn fmt_lane(root: &Path, tools: &Path) -> (LaneVerdict, Vec<&'static str>) {
    let passes = [
        (FMT_PASS_WORKSPACE, fmt_workspace_pass(root, tools)),
        (FMT_PASS_SWEEP, fmt_sweep(root, tools)),
        (FMT_PASS_EDITIONS, fmt_edition_agreement(root)),
    ];
    let verdict = passes
        .iter()
        .fold(LaneVerdict::Clean, |acc, (_, v)| acc.worst(*v));
    let not_run = passes
        .iter()
        .filter(|(_, v)| *v == LaneVerdict::NotRun)
        .map(|(name, _)| *name)
        .collect();
    (verdict, not_run)
}

/// The verdict line for `verdict`, naming the passes in `not_run` and the
/// `fix` ([`missing_fmt_tools_fix`]) — on a FAILED line too, since a finding
/// outranks a pass that did not run and would otherwise hide it.
///
/// The GREEN claim is scoped to what the passes read: `vendor/` is outside
/// every pass ([`tracked_rs_files`]), and the sweep skips
/// [`FMT_SWEEP_EXCLUSIONS`].
fn lint_verdict_line(verdict: LaneVerdict, not_run: &[&str], fix: Option<&str>) -> String {
    match verdict {
        LaneVerdict::Clean => format!(
            "{LINT_VERDICT_GREEN} — every tracked `.rs` file outside vendor/ is formatted at its \
             crate's edition, bar the {} in FMT_SWEEP_EXCLUSIONS, and rustfmt reads the same \
             editions",
            FMT_SWEEP_EXCLUSIONS.len()
        ),
        LaneVerdict::Finding if not_run.is_empty() => {
            format!("{LINT_VERDICT_FAILED} — formatting drift, named above")
        }
        LaneVerdict::Finding => format!(
            "{LINT_VERDICT_FAILED} — formatting drift, named above; {} did not run.{}",
            not_run.join(", "),
            fix_clause(fix)
        ),
        LaneVerdict::NotRun => format!(
            "{LINT_VERDICT_NO_VERDICT} — {} did not run (above).{}",
            not_run.join(", "),
            fix_clause(fix)
        ),
    }
}

/// ` Fix: <fix>.`, or nothing.
fn fix_clause(fix: Option<&str>) -> String {
    fix.map(|f| format!(" Fix: {f}.")).unwrap_or_default()
}

/// Print the verdict line; `true` only for [`LaneVerdict::Clean`].
fn lint_verdict(verdict: LaneVerdict, not_run: &[&str], fix: Option<&str>) -> bool {
    eprintln!("{}", lint_verdict_line(verdict, not_run, fix));
    verdict == LaneVerdict::Clean
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
            "  trustfmt: NOT RUN — no `{TRUSTFMT_DRIVER}` in {}.",
            tools.display()
        );
        return LaneVerdict::NotRun;
    }
    let (ok, stdout, stderr) =
        run_capturing_both("trustfmt", &driver, &["--all", "--check"], tools, root);
    if ok {
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
            shell_word(&driver.to_string_lossy()),
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
/// against upstream on every file and make `targo --unverified forge attest`'s `[OB-7]`
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
            "  trustfmt sweep: NOT RUN — no `{TRUSTFMT_BIN}` in {}.",
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
    for (edition, group) in &by_edition {
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
    // Each finding with the edition its crate declares, grouped, so the fix is
    // one runnable command per edition rather than a template to fill in.
    let mut findings: std::collections::BTreeMap<String, Vec<&String>> = Default::default();
    for path in drifted
        .iter()
        .filter(|p| !FMT_SWEEP_EXCLUSIONS.contains(&p.as_str()))
    {
        findings
            .entry(crate_edition(root, path))
            .or_default()
            .push(path);
    }
    if findings.is_empty() {
        return LaneVerdict::Clean;
    }
    eprintln!(
        "  trustfmt sweep: FINDING — {} of {} tracked `.rs` file(s) are unformatted:",
        findings.values().map(Vec::len).sum::<usize>(),
        files.len()
    );
    for (edition, paths) in &findings {
        for path in paths {
            eprintln!("      {path} (edition {edition})");
        }
    }
    eprintln!("    Fix, from {}:", root.display());
    for (edition, paths) in &findings {
        eprintln!(
            "      {} --edition {edition} --unstable-features --skip-children {}",
            shell_word(&bin.to_string_lossy()),
            paths
                .iter()
                .map(|p| shell_word(p))
                .collect::<Vec<_>>()
                .join(" ")
        );
    }
    LaneVerdict::Finding
}

/// `word` as one shell word: unchanged when it is plain, single-quoted
/// otherwise, so a printed fix command runs as pasted — the store's `trustfmt`
/// lives under `Application Support`.
fn shell_word(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-+=:@%,".contains(c));
    if plain {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
    }
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
        assert!(lint_verdict(LaneVerdict::Clean, &[], None));
        assert!(!lint_verdict(LaneVerdict::Finding, &[], None));
        assert!(!lint_verdict(LaneVerdict::NotRun, &[FMT_PASS_SWEEP], None));
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
        let (lane, not_run) = fmt_lane(&root, &tools);
        let _ = std::fs::remove_dir_all(&tmp);
        assert_eq!(
            workspace,
            LaneVerdict::NotRun,
            "a missing formatter must not report clean formatting"
        );
        assert_eq!(lane, LaneVerdict::NotRun);
        assert_eq!(
            not_run,
            vec![FMT_PASS_WORKSPACE, FMT_PASS_SWEEP, FMT_PASS_EDITIONS],
            "every pass that reached no verdict is named"
        );
    }

    /// The COULD NOT RUN verdict names the passes that did not run, and only
    /// those. It said "NOTHING was learned about the tree" even when the
    /// editions pass (which needs no toolchain) had run clean beside a missing
    /// `trustfmt`.
    #[test]
    fn the_could_not_run_verdict_names_only_the_passes_that_did_not_run() {
        let line = lint_verdict_line(
            LaneVerdict::NotRun,
            &[FMT_PASS_WORKSPACE, FMT_PASS_SWEEP],
            None,
        );
        assert_eq!(
            line,
            "gate lint: COULD NOT RUN — targo-fmt --all, trustfmt sweep did not run (above)."
        );
        assert!(!line.contains(FMT_PASS_EDITIONS), "{line}");
        assert!(line.starts_with(LINT_VERDICT_NO_VERDICT));
        let green = lint_verdict_line(LaneVerdict::Clean, &[], None);
        assert!(green.starts_with(LINT_VERDICT_GREEN));
        // The claim is scoped to what the passes read: never "every tracked
        // file" while vendor/ and the exclusions go unread.
        assert!(green.contains("outside vendor/"), "{green}");
        assert!(
            green.contains(&format!("bar the {} in", FMT_SWEEP_EXCLUSIONS.len())),
            "{green}"
        );
        assert!(
            lint_verdict_line(LaneVerdict::Finding, &[], None).starts_with(LINT_VERDICT_FAILED)
        );
    }

    /// A stage2 without the formatter gets its fix ONCE, on the verdict line,
    /// and the fix fits where the stage2 came from: an explicit
    /// `$TRUST_STAGE2_BIN` is never fallen back from, so `aterm pkg install
    /// trust` would change nothing there.
    #[test]
    fn the_missing_formatter_fix_is_printed_once_and_fits_the_toolchain_source() {
        let tmp = std::env::temp_dir().join(format!("aterm-gate-lint-fix-{}", std::process::id()));
        let empty = tmp.join("empty-stage2");
        let _ = std::fs::create_dir_all(&empty);
        let toolchain = |store_bin: Option<PathBuf>| aterm_verify::Toolchain {
            stage2_dir: empty.clone(),
            targo: empty.join("targo"),
            trustdoc: empty.join("trustdoc"),
            tippy: None,
            refused: None,
            store_bin,
            demoted: None,
        };
        let explicit = missing_fmt_tools_fix(&toolchain(None));
        let discovered = missing_fmt_tools_fix(&toolchain(Some(tmp.join("store"))));
        let refused = missing_fmt_tools_fix(&aterm_verify::Toolchain {
            refused: Some(tmp.join("wrong")),
            ..toolchain(Some(tmp.join("store")))
        });
        std::fs::write(empty.join(TRUSTFMT_DRIVER), "").unwrap();
        std::fs::write(empty.join(TRUSTFMT_BIN), "").unwrap();
        let complete = missing_fmt_tools_fix(&toolchain(Some(tmp.join("store"))));
        let _ = std::fs::remove_dir_all(&tmp);

        let explicit = explicit.expect("an explicit stage2 without the formatter has a fix");
        assert!(
            explicit.starts_with(&format!("TRUST_STAGE2_BIN={}", empty.display())),
            "{explicit}"
        );
        assert!(explicit.contains("or unset it"), "{explicit}");
        assert!(!explicit.contains("aterm pkg install"), "{explicit}");
        assert_eq!(
            discovered.as_deref(),
            Some("`aterm pkg install trust`, then re-run")
        );
        assert_eq!(
            refused, None,
            "a refused toolchain's own line carries its fix"
        );
        assert_eq!(complete, None);

        let line = lint_verdict_line(
            LaneVerdict::NotRun,
            &[FMT_PASS_WORKSPACE, FMT_PASS_SWEEP],
            discovered.as_deref(),
        );
        assert_eq!(line.matches("aterm pkg install trust").count(), 1, "{line}");
        // A finding elsewhere outranks the pass that did not run; the fix
        // still reaches the verdict line.
        let failed = lint_verdict_line(
            LaneVerdict::Finding,
            &[FMT_PASS_WORKSPACE, FMT_PASS_SWEEP],
            discovered.as_deref(),
        );
        assert!(failed.starts_with(LINT_VERDICT_FAILED), "{failed}");
        assert!(
            failed.contains("trustfmt sweep did not run. Fix: `aterm pkg install trust`"),
            "{failed}"
        );
    }

    /// A printed fix command must run as pasted: the store's `trustfmt` sits
    /// under `~/Library/Application Support`, and an unquoted space split it.
    #[test]
    fn a_printed_fix_command_quotes_what_the_shell_would_split() {
        assert_eq!(shell_word("crates/x/src/y.rs"), "crates/x/src/y.rs");
        assert_eq!(
            shell_word("/Users//me/Library/Application Support/aterm/bin/trustfmt"),
            "'/Users//me/Library/Application Support/aterm/bin/trustfmt'"
        );
        assert_eq!(shell_word("it's"), "'it'\\''s'");
        assert_eq!(shell_word(""), "''");
    }

    /// A bare `xtask gate` prints the usage line and an unknown verb the verb
    /// list — never a Rust `Option` — and a verb retired on 2026-09-27 says
    /// where it went.
    #[test]
    fn an_unknown_gate_verb_names_the_verbs_and_the_new_home_of_a_retired_one() {
        assert_eq!(
            unknown_verb_message(None),
            "usage: xtask gate <lint|forge|cells|cells-foreign>"
        );
        assert_eq!(
            unknown_verb_message(Some("nope")),
            "xtask gate: no verb `nope` — verbs: lint, forge, cells, cells-foreign"
        );
        assert_eq!(
            unknown_verb_message(Some("mainloop")),
            "xtask gate: no verb `mainloop` (now: targo --unverified run -p aterm-census -- \
             --mainloop) — verbs: lint, forge, cells, cells-foreign"
        );
        assert_eq!(
            unknown_verb_message(Some("perf")),
            "xtask gate: no verb `perf` (now: targo --unverified run -p xtask -- perf) — \
             verbs: lint, forge, cells, cells-foreign"
        );
        for (old, package, arg) in RETIRED_VERBS {
            assert!(
                unknown_verb_message(Some(old)).contains(&format!("-p {package} -- {arg})")),
                "{old}"
            );
        }
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
                "tools/grep-guard-fixtures/b9f/must_fire/test_sends_the_door_itself.rs"
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

    /// A row the parser cannot read is a COULD-NOT-RUN, never a skipped line —
    /// including the retired `cdep` and `floor` kinds, which would otherwise be
    /// read as policy nobody enforces.
    #[test]
    fn an_unreadable_policy_row_stops_the_gate_rather_than_being_ignored() {
        assert!(super::parse_cell_policy("cdep\tring\t*\tbundled C\n").is_err());
        assert!(super::parse_cell_policy("floor\tlinux\t206\twhy\n").is_err());
        assert!(super::parse_cell_policy("cshmi\tring@0.17.14\tx\ty\twhy\n").is_err());
        assert!(
            super::parse_cell_policy(
                "# comment\n\ncshim\tring@0.17.14\tx86_64-unknown-linux-gnu\tring_core_0_17_14_\twhy\n"
            )
            .is_ok_and(|p| p.cshims.len() == 1)
        );
        // Four columns is a `cshim` row missing the one column that cannot be
        // derived from anything else: the `links` key.
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
        // Every row names a triple some cell resolves for: a row written against
        // a triple this matrix has never had is dead on every box.
        for shim in &policy.cshims {
            assert!(
                cells.iter().any(|c| c.triple == shim.triple),
                "{} shims `{}` on `{}`, which is no cell's triple",
                super::CELL_GATE_POLICY,
                shim.package,
                shim.triple
            );
        }
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
                super::CellReport::skipped(
                    &cell("win", "x86_64-pc-windows-msvc"),
                    161,
                    74,
                    1,
                    super::SkipCause::NoStd,
                ),
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
        // And the repair is in the sentence, runnable as printed: the skipped
        // triple, never a `<triple>` / `<channel>` placeholder.
        assert!(
            v.text
                .contains("`rustup target add x86_64-pc-windows-msvc --toolchain stable`"),
            "{}",
            v.text
        );
        assert!(
            !v.text.contains('<'),
            "a placeholder in the fix: {}",
            v.text
        );
    }

    /// ONE COMMAND FOR EVERY SKIPPED TRIPLE, each named once: the two wasm cells
    /// share `wasm32-unknown-unknown`, and `rustup target add` takes a list.
    #[test]
    fn the_skip_fix_names_each_skipped_triple_once() {
        let skip = |name: &str, triple: &str| {
            super::CellReport::skipped(&cell(name, triple), 54, 20, 1, super::SkipCause::NoStd)
        };
        let v = super::cells_verdict(
            &[
                checked_cell("mac-arm", "aarch64-apple-darwin"),
                skip("win", "x86_64-pc-windows-msvc"),
                skip("wasm-cpu", "wasm32-unknown-unknown"),
                skip("wasm-gpu", "wasm32-unknown-unknown"),
            ],
            4,
            false,
            false,
        );
        assert!(
            v.text.contains(
                "Install: `rustup target add wasm32-unknown-unknown x86_64-pc-windows-msvc \
                 --toolchain stable`, then run again."
            ),
            "{}",
            v.text
        );
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
        let skipped = super::CellReport::skipped(
            &cell("wasm-cpu", "wasm32-unknown-unknown"),
            54,
            20,
            1,
            super::SkipCause::NoStd,
        );
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
        let skipped = super::CellReport::skipped(
            &cell("win", "x86_64-pc-windows-msvc"),
            161,
            74,
            1,
            super::SkipCause::NoStd,
        );
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
                super::CellReport::skipped(
                    &cell("win", "x86_64-pc-windows-msvc"),
                    161,
                    74,
                    1,
                    super::SkipCause::NoStd,
                ),
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
                            super::SkipCause::NoStd,
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
            super::CellReport::skipped(
                &cell("linux", "x86_64-unknown-linux-gnu"),
                266,
                79,
                2,
                super::SkipCause::NoStd,
            ),
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
        let r = super::CellReport::skipped(
            &cell("win", "x86_64-pc-windows-msvc"),
            161,
            74,
            1,
            super::SkipCause::NoStd,
        );
        assert_eq!(r.status, super::SKIPPED_NO_STD);
        assert_eq!(
            r.outcome,
            super::CellOutcome::Skipped(super::SkipCause::NoStd)
        );
        assert_ne!(r.outcome, super::CellOutcome::Checked);
        // Nothing was compiled, so nothing may be counted as compiled.
        assert_eq!((r.checked, r.own_checked, r.tests_checked), (0, 0, 0));
        assert!(r.tests_note.is_some());
    }

    /// PATH FIRST, then where rustup installs itself, then Homebrew's keg — and a
    /// relative PATH entry is never a candidate (it would resolve against whatever
    /// cwd the gate ran in).
    #[test]
    fn rustup_is_looked_for_on_path_then_where_rustup_installs_itself() {
        use std::ffi::OsStr;
        let got = super::rustup_candidates(
            Some(OsStr::new("/p/one:rel/dir:/p/two")),
            Some(OsStr::new("/ch")),
            Some(OsStr::new("/h")),
        );
        let want: Vec<PathBuf> = [
            "/p/one/rustup",
            "/p/two/rustup",
            "/ch/bin/rustup",
            "/h/.cargo/bin/rustup",
            "/opt/homebrew/opt/rustup/bin/rustup",
        ]
        .iter()
        .map(PathBuf::from)
        .collect();
        assert_eq!(got, want);
        // An unset or relative CARGO_HOME / HOME adds nothing.
        let bare = super::rustup_candidates(None, Some(OsStr::new("ch")), None);
        assert_eq!(
            bare,
            vec![PathBuf::from("/opt/homebrew/opt/rustup/bin/rustup")]
        );
    }

    /// THE BOX THIS WAS MEASURED ON (2026-09-27): rustup in `~/.cargo/bin`, that
    /// directory not on PATH. The search finds it; the PATH-only question the cross
    /// gates asked until that day — the negative control — does not.
    #[test]
    fn a_rustup_off_path_in_cargo_home_is_found() {
        let root = std::env::temp_dir().join(format!("aterm-find-rustup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        let path_dir = root.join("bin");
        std::fs::create_dir_all(home.join(".cargo/bin")).expect("mkdir cargo bin");
        std::fs::create_dir_all(&path_dir).expect("mkdir path dir");
        let rustup = home.join(".cargo/bin/rustup");
        std::fs::write(&rustup, b"#!/bin/sh\n").expect("write rustup");
        // A STRAY `rustup` EARLY ON PATH that cannot be executed (a renamed
        // download nobody chmodded): `execvp` passes over it, so the search must.
        let stray = path_dir.join("rustup");
        std::fs::write(&stray, b"not a program\n").expect("write stray");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&rustup, std::fs::Permissions::from_mode(0o755))
                .expect("chmod rustup");
            std::fs::set_permissions(&stray, std::fs::Permissions::from_mode(0o644))
                .expect("chmod stray");
        }
        let path = path_dir.clone().into_os_string();
        let found = super::find_rustup_in(Some(&path), None, Some(home.as_os_str()));
        // The negative control, the question the cross gates asked until that day:
        // PATH alone, as `command -v` answers it.
        let path_only = std::env::split_paths(&path)
            .map(|d| d.join("rustup"))
            .find(|p| super::is_executable_file(p));
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(found, Some(rustup));
        assert_eq!(
            path_only, None,
            "the negative control: PATH alone misses it"
        );
    }

    /// A box with NO rustup is told so, and is not sent to `rustup target add` as
    /// if a std were all it lacked — and the sentence still opens with the sentinel
    /// `tools/verify.sh` reads, so the stage is still a SKIP, never a pass.
    #[test]
    fn a_cell_skipped_for_no_rustup_names_rustup_not_a_std() {
        let r = super::CellReport::skipped(
            &cell("win", "x86_64-pc-windows-msvc"),
            161,
            74,
            1,
            super::SkipCause::NoRustup,
        );
        assert_eq!(r.status, super::SKIPPED_NO_RUSTUP);
        let v = super::cells_verdict(
            &[checked_cell("mac-arm", "aarch64-apple-darwin"), r],
            2,
            false,
            false,
        );
        assert!(v.ok, "a box fact never blocks: {}", v.text);
        assert!(
            v.text.starts_with(aterm_verify::stages::CELLS_NOT_PROVEN),
            "{}",
            v.text
        );
        assert!(v.text.contains("no rustup"), "{}", v.text);
        assert!(v.text.contains(super::RUSTUP_LOOKED), "{}", v.text);
        assert!(
            v.text.contains(
                "Install rustup (https://rustup.rs), then `rustup target add \
                 x86_64-pc-windows-msvc --toolchain stable`"
            ),
            "{}",
            v.text
        );
        assert!(!v.text.contains("had no installed std"), "{}", v.text);
        assert!(!v.text.contains(super::MATRIX_CLAIM), "{}", v.text);
        // The std-only cause keeps its own words.
        let std_only = super::cells_verdict(
            &[
                checked_cell("mac-arm", "aarch64-apple-darwin"),
                super::CellReport::skipped(
                    &cell("win", "x86_64-pc-windows-msvc"),
                    161,
                    74,
                    1,
                    super::SkipCause::NoStd,
                ),
            ],
            2,
            false,
            false,
        );
        assert!(
            std_only.text.contains("had no installed std"),
            "{}",
            std_only.text
        );
        assert!(!std_only.text.contains("no rustup"), "{}", std_only.text);
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

    // -----------------------------------------------------------------------
    // G-CELLS, THE ALWAYS-ON SUBSET
    // -----------------------------------------------------------------------

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
    /// `cshim` decisions, different artifacts counted — and the merge gate would
    /// be answering a different question depending on who ran it.
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
        let driver = crate::driver::CargoDriver {
            program: PathBuf::from("/s/targo"),
            trustc: PathBuf::from("/s/trustc"),
        };
        let host =
            super::cell_check_command(super::CellCompiler::Host(&driver), "aarch64-apple-darwin");
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
        // The pinned driver, with the lane named.
        assert_eq!(args, ["--unverified", "check"]);
        assert_eq!(host.get_program(), driver.program.as_os_str());

        // A cross cell drives THE TOOLCHAIN'S OWN cargo and rustc, by path — never
        // a `cargo` off PATH trusted to honour RUSTUP_TOOLCHAIN (Homebrew's does not).
        let stable = super::RustupToolchain {
            name: "stable".to_string(),
            cargo: PathBuf::from("/r/toolchains/stable/bin/cargo"),
            rustc: PathBuf::from("/r/toolchains/stable/bin/rustc"),
        };
        let cross = super::cell_check_command(
            super::CellCompiler::Cross(&stable),
            "x86_64-unknown-linux-gnu",
        );
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
